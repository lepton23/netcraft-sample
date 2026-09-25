use crate::Cache;
use crate::caches::cachemeta::CacheMeta;

// const functions used for branch prediction hints in hot if statement paths
#[inline]
#[cold]
const fn cold() {}

#[inline]
const fn likely(b: bool) -> bool {
    if !b {
        cold()
    }
    b
}

// CacheSet struct used for set associative caches
// const N generic parameter used to specify associativity at compile time for better performance
struct CacheSet<const N: usize> {
    tags: [u64; N], // tag to index in cache lines
    rr_counter: usize,
    access_order: [u64; N],
    access_order_len: u8, // how many valid tags for LRU
    access_frequency: [u64; N],
}

const INVALID_TAG: u64 = u64::MAX;

// const functions allow for compile-time evaluation of cache set operations
impl<const N: usize> CacheSet<N> {
    const fn new() -> Self {
        Self {
            tags: [INVALID_TAG; N],
            rr_counter: 0,
            access_order: [0; N],
            access_order_len: 0,
            access_frequency: [0; N],
        }
    }

    // Remove first tag occurence from access_order then shift others left
    fn order_remove(&mut self, tag: u64) {
        let len = self.access_order_len as usize;
        if let Some(pos) = self.access_order[..len].iter().position(|&t| t == tag) {
            // shift the rest left
            for i in pos..len - 1 {
                self.access_order[i] = self.access_order[i + 1];
            }
            self.access_order_len -= 1;
        }
    }

    // Push tag to end of access_order
    const fn order_push(&mut self, tag: u64) {
        let len = self.access_order_len as usize;
        self.access_order[len] = tag;
        self.access_order_len += 1;
    }

    // Remove and return the oldest tag
    fn order_pop_front(&mut self) -> u64 {
        let tag = self.access_order[0];
        let len = self.access_order_len as usize;
        for i in 0..len - 1 {
            self.access_order[i] = self.access_order[i + 1];
        }
        self.access_order_len -= 1;
        tag
    }

    // Move tag to back as most recently sued
    fn order_move_to_back(&mut self, tag: u64) {
        self.order_remove(tag);
        self.order_push(tag);
    }
} // CacheSet

#[repr(C)]
pub struct NWayCache<const N: usize, L: Cache> {
    // hot path fields

    // Store box of sets rather than box of tags
    // for replacement policies track order & frequency within each set
    sets: Box<[CacheSet<N>]>,

    index_mask: u64,
    line_size: u64,
    asso_num: usize,

    offset_bits: u32,
    index_bits: u32,

    // cold-ish fields
    hits: u64,
    misses: u64,

    // Store a reference to lower level cache
    lower: L,

    rp: u8, // replacement policy - 0 = RR, 1 = LRU, 2 = LFU
}

impl<const N: usize, L: Cache> NWayCache<N, L> {
    // Generic reference to lower passed into constructor for monomorphisation of cache hierarchy at compile time
    pub fn new(meta: &CacheMeta, lower: L) -> Self {
        let asso_num: usize = match meta.kind {
            1 => 2,
            2 => 4,
            3 => 8,
            _ => panic!("Invalid cache kind for NWayCache"),
        };

        let offset_bits = meta.line_size.trailing_zeros();
        // Calculate no. of cache lines & no. of sets
        let num_lines = (meta.size >> offset_bits) as usize;
        let num_sets = num_lines / asso_num;

        // Calculate bits for offset, index, and tag
        let index_bits = (num_sets as u64).trailing_zeros();

        let index_mask = (1u64 << index_bits) - 1;

        // Initialise sets
        let sets = (0..num_sets)
            .map(|_| CacheSet::new())
            .collect::<Vec<_>>()
            .into_boxed_slice();

        // Construct cache
        Self {
            sets,
            index_mask,
            line_size: meta.line_size,
            asso_num,
            offset_bits,
            index_bits,
            hits: 0,
            misses: 0,
            lower,
            rp: meta.rp,
        }
    }

    // extract set index from address
    #[inline(always)]
    const fn get_set_index(&self, address: u64) -> usize {
        ((address >> self.offset_bits) & self.index_mask) as usize
    }

    // extract tag from address
    #[inline(always)]
    const fn get_tag(&self, address: u64) -> u64 {
        address >> (self.offset_bits + self.index_bits)
    }
} // impl NWayCache

impl<const N: usize, L: Cache> Cache for NWayCache<N, L> {
    // "Perform access"
    fn access(&mut self, address: u64, size: u64) {
        // Calculate which cache lines are accessed (& check for accesses that span across lines)
        // end_line has -1 since if we access the last byte of a line, it should still count as accessing that line
        let start_line = address >> self.offset_bits;
        let end_line = (address + size - 1) >> self.offset_bits;

        // fast route for accesses that only touch 1 line
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
        // Calculate which cache lines are accessed (& check for accesses that span across lines)
        // end_line has -1 since if we access the last byte of a line, it should still count as accessing that line
        let line_address = line_index << self.offset_bits;
        let set_index = self.get_set_index(line_address);
        let tag = self.get_tag(line_address);

        let set = &mut self.sets[set_index];

        // Find tag in set
        let hit_index = set.tags.iter().position(|&t| t == tag);

        // Check for hit
        if let Some(index) = hit_index {
            // HIT
            self.hits += 1;

            if self.rp == 1 {
                // Update access order for LRU
                set.order_move_to_back(tag);
            }
            if self.rp == 2 {
                // Update access frequency for LFU
                set.access_frequency[index] += 1;
            }
        } else {
            // MISS
            self.misses += 1;

            // Find empty index
            let empty_index = set.tags[..self.asso_num]
                .iter()
                .position(|&t| t == INVALID_TAG);

            // SET NOT FULL
            if let Some(free_index) = empty_index {
                // Set not full, insert new tag into set
                // Find first free index in set
                set.tags[free_index] = tag;

                if self.rp == 1 {
                    // Add new tag to end of access order for LRU
                    set.order_push(tag);
                } else if self.rp == 2 {
                    // Initialise frequency for new tag
                    set.access_frequency[free_index] = 1;
                }

            // SET FULL
            } else {
                // Set full, need to evict line
                // Select victim tag & index based on replacement policy
                let victim_index = match self.rp {
                    0 => {
                        let idx = set.rr_counter % self.asso_num;
                        set.rr_counter = set.rr_counter.wrapping_add(1);
                        idx
                    }
                    1 => {
                        // least recently used tag at start of vec
                        let vtag = set.order_pop_front();
                        set.tags[..self.asso_num]
                            .iter()
                            .position(|&t| t == vtag)
                            .expect("Victim not found for LRU")
                    }
                    2 => {
                        // least frequently used tag tiebroken by lowest index
                        // tiebreak ensures deterministic behaviour
                        set.access_frequency[..self.asso_num]
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| set.tags[*i] != INVALID_TAG)
                            .min_by_key(|&(ref i, &freq)| (freq, *i))
                            .map(|(i, _)| i)
                            .expect("No LFU victim found")
                    }
                    _ => panic!("Invalid replacement policy"),
                };

                let victim_tag = set.tags[victim_index];

                // Remove victim from all tracking structs
                if self.rp == 1 {
                    set.order_remove(victim_tag);
                }

                // Insert new tag
                set.tags[victim_index] = tag;
                if self.rp == 1 {
                    set.order_push(tag);
                } else if self.rp == 2 {
                    set.access_frequency[victim_index] = 1;
                }
            } // if set not full / full

            // On miss, access lower-level cache if exists, otherwise "access main memory"
            self.lower
                .access(line_index << self.offset_bits, self.line_size);
        } // if hit or miss
    } // access_line

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
} // impl Cache for NWayCache
