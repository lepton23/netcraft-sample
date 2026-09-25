
use crate::caches::cachemeta::CacheMeta;
use crate::Cache;

// FxHashMap is much more performant than the standard library HashMap
// - since we don't need the security guarantees of a cryptographic hasher
use rustc_hash::FxHashMap;
use rustc_hash::FxBuildHasher;

// const functions used for branch prediction hints in hot if statement paths
#[inline]
#[cold]
const fn cold() {}

#[inline]
const fn likely(b: bool) -> bool {
    if !b { cold() }
    b
}

// sentinel value to avoid using Option for tags
const INVALID_TAG: u64 = u64::MAX;

// Min-heap indexed by cache line used for LFU
// Entries keyed with frequency & insertion order for tiebreaking
// Could have used a BTree but this is simpler and more performant
// Originally used a hashmap only, but that had O(n) complexity for eviction
// This has O(log n) complexity for insertions and updates, but O(1) for eviction since the target is always kept at the top of the heap
struct IndexedMinHeap {
        // boxes used for cache locality - requires less storage than a Vec since it doesn't store capacity
        heap: Box<[usize]>,
        heap_pos: Box<[usize]>,
        keys: Box<[(u64, u64)]>,
        len: usize,
}

impl IndexedMinHeap {
        fn new(capacity: usize) -> Self {
                Self {
                        heap: vec![0; capacity].into_boxed_slice(),
                        heap_pos: vec![usize::MAX; capacity].into_boxed_slice(),
                        keys: vec![(0, 0); capacity].into_boxed_slice(),
                        len: 0,
                }
        }

        // Compare keys
        #[inline(always)]
        fn less(&self, i: usize, j: usize) -> bool {
                self.keys[self.heap[i]] < self.keys[self.heap[j]]
        }

        // Swap keys
        #[inline(always)]
        fn swap(&mut self, i: usize, j: usize) {
                self.heap.swap(i, j);
                self.heap_pos[self.heap[i]] = i;
                self.heap_pos[self.heap[j]] = j;
        }

        // Sift heap up and down for insertions & updates
        fn sift_up(&mut self, mut pos: usize) {
                while pos > 0 {
                        let parent = (pos - 1) / 2;
                        if self.less(pos, parent) {
                                self.swap(pos, parent);
                                pos = parent;
                        } else {
                                break;
                        }
                }
        }

        fn sift_down(&mut self, mut pos: usize) {
                let len = self.len;
                loop {
                        let left = 2 * pos + 1;
                        let right = 2 * pos + 2;
                        let mut smallest = pos;

                        if left < len && self.less(left, smallest) {
                                smallest = left;
                        }
                        if right < len && self.less(right, smallest) {
                                smallest = right;
                        }
                        if smallest == pos {
                                break;                                  
                        } else {
                                self.swap(pos, smallest);
                                pos = smallest; 
                        }
                }
        }

        // Insert new heap key with key (frequency, cache index)
        fn insert(&mut self, line_index: usize, key: (u64, u64)) {
                let pos = self.len;
                self.heap[pos] = line_index;
                self.heap_pos[line_index] = pos;
                self.keys[line_index] = key;
                self.len += 1;
                self.sift_up(pos);
        }

        // O(1) access to min key for eviction
        #[inline(always)]
        fn peek_min(&self) -> usize {
                self.heap[0]
        }

        // update key for cache line and re heapify
        fn update(&mut self, line_index: usize, key: (u64, u64)) {
                let old = self.keys[line_index];
                self.keys[line_index] = key;
                let pos = self.heap_pos[line_index];
                if key < old {
                        self.sift_up(pos);
                } else {
                        self.sift_down(pos);
                }
        }
} // IndexedMinHeap

#[repr(C)]
pub struct FullAssocCache<L: Cache> {
        // hot path fields
        lfu_heap: IndexedMinHeap,
        // Store hashmap of tags to their index in the cache
        tags: Box<[u64]>,
        tags_map: FxHashMap<u64, usize>, 
        lru_timestamps: Box<[u64]>, // for LRU

        // For replacement policies track order & frequency
        global_clock: u64, // for LRU
        rr_counter: usize, // for RR

        num_lines: usize,
        line_size: u64,
        occupied: usize,
        offset_bits: u32,

        // cold-ish fields

        hits: u64,
        misses: u64,

        // Store a reference to lower level cache if exists
        lower: L,
        rp: u8, // replacement policy - 0 = RR, 1 = LRU, 2 = LFU
}


impl<L: Cache> FullAssocCache<L> {
        // Generic reference to lower passed into constructor for monomorphisation of cache hierarchy at compile time
        pub fn new(meta: &CacheMeta, lower: L) -> Self {
                // Calculate no of cache lines
                let num_lines = (meta.size / meta.line_size) as usize;

                // Calculate bits for offset, index, and tag
                let offset_bits = meta.line_size.trailing_zeros();

                // Initialise cache
                Self {
                        lfu_heap: IndexedMinHeap::new(num_lines),
                        tags: vec![INVALID_TAG; num_lines].into_boxed_slice(),
                        tags_map: FxHashMap::with_capacity_and_hasher(num_lines, FxBuildHasher),
                        lru_timestamps: vec![0; num_lines].into_boxed_slice(),
                        global_clock: 0,
                        rr_counter: 0,
                        num_lines,
                        line_size: meta.line_size,
                        occupied: 0,
                        offset_bits,
                        hits: 0,
                        misses: 0,
                        lower,
                        rp: meta.rp,
                }
        }

        // Handle hits for different replacement policies        
        #[inline(always)]
        fn on_hit(&mut self, idx: usize) {
                match self.rp {
                        0 => {},
                        1 => {
                                // LRU update access order
                                self.global_clock += 1;
                                unsafe { *self.lru_timestamps.get_unchecked_mut(idx) = self.global_clock };
                        },
                        2 => {
                                // LFU update access frequency
                                let (freq, _) = self.lfu_heap.keys[idx];
                                self.lfu_heap.update(idx, (freq + 1, idx as u64));
                        },
                        _ => unreachable!(),
                }
        }

        // Find victim line accordint to replacement policy
        #[inline(always)]
        fn find_victim(&mut self) -> usize {
                // if cache not full, use next empty slot
                if self.occupied < self.num_lines {
                        let idx = self.occupied;
                        self.occupied += 1;
                        return idx;
                }

                match self.rp {
                        0 => {
                                // RR
                                let idx = self.rr_counter % self.num_lines;
                                self.rr_counter += 1;
                                idx
                        },
                        1 => {
                                // LRU: find min timestamp
                                let mut min_ts = u64::MAX;
                                let mut min_idx = 0;
                                for i in 0..self.num_lines {
                                        let ts = unsafe { *self.lru_timestamps.get_unchecked(i) };
                                        if ts < min_ts {
                                                min_ts = ts;
                                                min_idx = i;
                                        }
                                }
                                min_idx
                        },
                        2 => {
                                // LFU: find min freqeuncy, tiebreak w/ index
                                self.lfu_heap.peek_min()
                        },
                        _ => unreachable!(),
                }
        } // find_victim

        #[inline(always)]
        fn replace_line(&mut self, idx: usize, tag: u64) {
                // on eviction, remove old tag from map
                let old_tag = self.tags[idx];
                if old_tag != INVALID_TAG {
                        self.tags_map.remove(&old_tag);
                }
                self.tags_map.insert(tag, idx);
                
                unsafe { *self.tags.get_unchecked_mut(idx) = tag };

                match self.rp {
                        0 => {},
                        1 => {
                                // LRU update access order
                                self.global_clock += 1;
                                unsafe { *self.lru_timestamps.get_unchecked_mut(idx) = self.global_clock; }
                        },
                        2 => {
                                // LFU update access frequency
                                let new_key = (1u64, idx as u64);
                                if old_tag == INVALID_TAG {
                                        self.lfu_heap.insert(idx, new_key);
                                } else {
                                        self.lfu_heap.update(idx, new_key);
                                }
                        },
                        _ => unreachable!(),
                }
        } 
} // FullAssocCache
        
impl<L: Cache> Cache for FullAssocCache<L> {
        // "Perform access"
        fn access(&mut self, address: u64, size: u64) {
                // Calculate which cache lines are accessed (& check for accesses that span across lines)
                // end_line has -1 since if we access the last byte of a line, it should still count as accessing that line
                let start_line = address >> self.offset_bits;
                let end_line = (address + size - 1) >> self.offset_bits; 

                // hot path for accesses that only touch 1 line
                if likely(start_line == end_line) {
                        self.access_line(start_line);
                } else {
                        // Access each cache line that the address + size spans
                        for line_index in start_line..=end_line {
                                self.access_line(line_index);
                        }
                }

        }

        #[inline(always)]
        fn access_line(&mut self, line_index: u64) {
                let tag = line_index;
                
                // get cache line index from tags map - O(1)
                let found = self.tags_map.get(&tag).copied();

                if let Some(idx) = found {
                        self.hits += 1;
                        self.on_hit(idx); // handle hit
                } else {
                        // MISS
                        self.misses += 1;
                        let victim_idx = self.find_victim();
                        self.replace_line(victim_idx, tag);

                        // access lower level cache on miss - fetch entire cache line from start address of line
                        self.lower.access(line_index << self.offset_bits, self.line_size);
                }
        }

        // Recursively get metadata on hits and misses from this cache and all lower caches
        // Format into vec
        fn get_hits(&self, mut hits_vec: Vec<u64>) -> Vec<u64> {

                hits_vec.push(self.hits);
                hits_vec = self.lower.get_hits(hits_vec);

                hits_vec
        }

        fn get_misses(&self, mut misses_vec: Vec<u64>) -> Vec<u64> {

                misses_vec.push(self.misses);
                misses_vec = self.lower.get_misses(misses_vec);
                
                misses_vec
        }

        // Get main mem accesses from lowest cache
        fn get_main_memory_accesses(&self) -> u64 {
                self.lower.get_main_memory_accesses()
        }
} // impl Cache for FullAssocCache

