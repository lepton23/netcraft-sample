
use crate::caches::cachemeta::CacheMeta;
use crate::Cache;


// const functions used for branch prediction hints in hot if statement paths
#[inline]
#[cold]
const fn cold() {}

#[inline]
const fn likely(b: bool) -> bool {
    if !b { cold() }
    b
}

// repr C used to order fields for better cache locality
#[repr(C)]
pub struct DirectCache<L: Cache> {
        // hot path fields 

        // Cache storage
        // No need for valid bits or dirty bits since we are only simulating reads
        // Store tags as a box of u64s, where each u64 represents the tag of a cache line
        // box is used since it stores pointer + length, vs the pointer + length + capacity of a vec
        tags: Box<[u64]>,        
        index_mask: u64,
        line_size: u64,
        offset_bits: u32,
        index_bits: u32,

        // cold-ish fields
        hits: u64,
        misses: u64,

        // Store a reference to lower level cache if exists
        lower: L,
}

// sentinel value to avoid using Option for tags
const INVALID_TAG: u64 = u64::MAX;


// Direct Cache implementation
impl<L: Cache> DirectCache<L> {
        // Generic reference to lower passed into constructor for monomorphisation of cache hierarchy at compile time
        pub fn new(meta: &CacheMeta, lower: L) -> Self {
                // Calculate bits for offset, index, and tag
                let offset_bits = meta.line_size.trailing_zeros();
                 // Calculate no of cache lines
                let num_lines = meta.size >> offset_bits;
                let line_size = meta.line_size;

                let index_bits = num_lines.trailing_zeros();

                // Mask used to extract index from line address
                let index_mask = (1u64 << index_bits) - 1;

                // Initialise cache with empty lines
                Self {
                        tags: vec![INVALID_TAG; num_lines as usize].into_boxed_slice(),
                        index_mask,
                        line_size, 
                        offset_bits,
                        index_bits,
                        hits: 0,
                        misses: 0,
                        lower,
                }
        }
}

        
impl<L: Cache> Cache for DirectCache<L> {
        // "Perform access"
        #[inline(always)]
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
                let index = (line_index & self.index_mask) as usize;
                let tag = line_index >> self.index_bits;

                // check for hit
                let stored_tag = unsafe { *self.tags.get_unchecked(index) };
                // fast route for hits
                if likely(stored_tag == tag) {
                        self.hits += 1;
                } else {
                        // we're just simulating reads, so can just overwrite lines on miss without worrying about evicting dirty lines
                        unsafe { *self.tags.get_unchecked_mut(index) = tag };
                        self.misses += 1;

                        // If we have a lower level cache, access it as well
                        // if not - "Access main memory"
                        // fetch entire cache line from start address of line
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
} // impl Cache for DirectCache
