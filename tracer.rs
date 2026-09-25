
use std::fs::File;
use memmap2::MmapOptions;

// Checks for x86_64 arch to emable SIMD parsing optimisations.
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

// Struct for trace entries
// u64 for *both* address and size for better alignment, even if size is only 3 decimal digits.
#[derive(Debug, Clone, Copy)]
pub struct TraceEntry {
        pub address: u64,
        pub size: u64,
}

// static hex lookup table prepared at compile time
// only used for non-simd lines if total lines is odd
// Aligned to u64
#[repr(align(64))]
struct HexLut([u8; 256]);

static HEX_LUT: HexLut = HexLut({
        let mut lut = [255u8; 256];
        let mut i = 0;
        while i < 10 {
                lut[(b'0' + i) as usize] = i;
                i += 1;
        }
        i = 0;
        while i < 6 {
                lut[(b'a' + i) as usize] = 10 + i;
                lut[(b'A' + i) as usize] = 10 + i;
                i += 1;
        }
        lut
});



// Parse size from line - if SIMD is not available, or total lines is odd fall back to scalar parsing for last line
#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn parse_decimal_u64(ptr: *const u8) -> u64 {
        // Load 4 bytes at once, avoid 3 separate loads
        let chunk = unsafe { (ptr.cast::<u32>()).read_unaligned() };
        // Extract each digit by masking and shifting
        let d0 = u64::from((chunk & 0xFF).wrapping_sub(0x30));
        let d1 = u64::from(((chunk >> 8) & 0xFF).wrapping_sub(0x30));
        let d2 = u64::from(((chunk >> 16) & 0xFF).wrapping_sub(0x30));
        d0 * 100 + d1 * 10 + d2
}


// read_unaligned since we assume input is well formed
// uses SWAR (SIMD within a register) to parse pairs of lines
#[inline(always)]
unsafe fn parse_decimal_pair(ptr0: *const u8, prt1: *const u8) -> (u64, u64) {
        // load 4 bytes from each line - only 3 used for size - mask them out and retain only the lower nibble
        // since ASCII digits are 0x30-0x39, masking with 0x0f retains the digit value and discards the rest
        let v0 = u64::from(unsafe { (ptr0.cast::<u32>()).read_unaligned() }) & 0x000f_0f0f;
        let v1 = u64::from(unsafe { (prt1.cast::<u32>()).read_unaligned() }) & 0x000f_0f0f;


        // Convert from separate digits into final size with combination of shifts and multiplication
        let s0 = (v0 & 0xff) * 100 + ((v0 >> 8) & 0xff) * 10 + ((v0 >> 16) & 0xff);
        let s1 = (v1 & 0xff) * 100 + ((v1 >> 8) & 0xff) * 10 + ((v1 >> 16) & 0xff);

        (s0, s1)
}


// Parse addresses if no SIMD, or for final lines
#[inline(always)]
unsafe fn parse_hex_u64_no_simd(ptr: *const u8) -> u64 {
        let lut = &HEX_LUT.0;
        // load nibbles first as its better for CPU pipelining
        unsafe {
                let n0 = u64::from(*lut.get_unchecked(*ptr.add(0) as usize));
                let n1 = u64::from(*lut.get_unchecked(*ptr.add(1) as usize));
                let n2 = u64::from(*lut.get_unchecked(*ptr.add(2) as usize));
                let n3 = u64::from(*lut.get_unchecked(*ptr.add(3) as usize));
                let n4 = u64::from(*lut.get_unchecked(*ptr.add(4) as usize));
                let n5 = u64::from(*lut.get_unchecked(*ptr.add(5) as usize));
                let n6 = u64::from(*lut.get_unchecked(*ptr.add(6) as usize));
                let n7 = u64::from(*lut.get_unchecked(*ptr.add(7) as usize));
                let n8 = u64::from(*lut.get_unchecked(*ptr.add(8) as usize));
                let n9 = u64::from(*lut.get_unchecked(*ptr.add(9) as usize));
                let n10 = u64::from(*lut.get_unchecked(*ptr.add(10) as usize));
                let n11 = u64::from(*lut.get_unchecked(*ptr.add(11) as usize));
                let n12 = u64::from(*lut.get_unchecked(*ptr.add(12) as usize));
                let n13 = u64::from(*lut.get_unchecked(*ptr.add(13) as usize));
                let n14 = u64::from(*lut.get_unchecked(*ptr.add(14) as usize));
                let n15 = u64::from(*lut.get_unchecked(*ptr.add(15) as usize));
                
                // pack nibbles and OR together
                (n0 << 60) | (n1 << 56) | (n2 << 52) | (n3 << 48)
                | (n4 << 44) | (n5 << 40) | (n6 << 36) | (n7 << 32)
                | (n8 << 28) | (n9 << 24) | (n10 << 20) | (n11 << 16)
                | (n12 << 12) | (n13 << 8) | (n14 << 4) | n15
        }
}


// Use SIMD to parse pairs of lines in one go!
// AVX2 is an x86 extension that allows processing of 32 bytes in parallel in 256 bit wide registers using SIMD instructions.
// Should be enabled on all the Intel CPUs in the labs.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[inline]
unsafe fn parse_line_pair_simd(ptr0: *const u8, ptr1: *const u8) -> (u64, u64) {
        // Load 16 hex chars into 128 bit registers for each line
        let lo = unsafe { _mm_loadu_si128(ptr0.cast::<__m128i>()) };
        let hi = unsafe { _mm_loadu_si128(ptr1.cast::<__m128i>()) };
        let input = _mm256_set_m128i(hi, lo); // combine into 256 bit register

        // Mask out high nibble of each byte
        let mask_0f = _mm256_set1_epi8(0x0f); // this is an 8 bit needle which we can AND over our 256 bit input
        let low_nibbles = _mm256_and_si256(input, mask_0f); // as so - grabbing the low nibble of each byte

        // ASCII digits: 0-9: 0x30 -> 0x39
        // ASCII letters: a-f: 0x61 -> 0x66, A-F: 0x41 -> 0x46
        // So if a byte > 0x39, it's a letter and needs a +9 offset to convert to a number        
        let mask_39 = _mm256_set1_epi8(0x39u8.cast_signed()); // 8 bit needle with value of 0x39 we can run over 256 bit register
        let is_letter = _mm256_cmpgt_epi8(input, mask_39); // cmpgt returns 0xFF for bytes larger than 0x39

        // form a register with a value 9 in each byte position where there is a letter
        let nine_offset = _mm256_set1_epi8(9); // 8 bit needle with value of 9
        let letter_offset = _mm256_and_si256(is_letter, nine_offset); // AND it witht he is_letter mask to get 9 in letter positions

        // add offset to low nibbles to convert letters to numbers - digits are unaffected since their offset is 0
        let nibbles = _mm256_add_epi8(low_nibbles, letter_offset);

        // Bytes are now in this 2 lane format in the 256 bit register - example: 
        // 0x00 0x00 0x00 0x00 0x00 0x00 0x00 0x00 0x09 0x0A 0x0B 0x0C 0x0F 0x02 0x0D 0x02
        // 0x00 0x00 0x00 0x00 0x00 0x00 0x00 0x00 0x09 0x0A 0x0B 0x0C 0x0F 0x02 0x0D 0x02        
        // We need to pack nibbles together - need to reorder them with some shifting and masking
        let evens_shifted = _mm256_slli_epi16::<4>(nibbles); // nibbles in even positions are shifted to high nibble of each byte
        let evens_mask = _mm256_set1_epi16(0x00f0u16.cast_signed());
        let evens = _mm256_and_si256(evens_shifted, evens_mask); // mask out all other bits to get nibbles in even positions

        let odds_mask = _mm256_set1_epi16(0x0f00u16.cast_signed()); 
        let odds = _mm256_and_si256(nibbles, odds_mask); // mask out every even byte to get nibbles in odd positions
        let odds_shifted = _mm256_srli_epi16::<8>(odds); // shift these nibbles 8 bits right

        // OR the even and odd nibbles together to get the nibbles into the same bytes
        // now required bytes are in even bytes of each 128 bit lane
        let combined = _mm256_or_si256(evens, odds_shifted);

        // Use pshufb to unshuffle bytes
        // this zeros out the bytes we don't need and picks the useful bytes and packs them to the low end of each 128 bit lane
        let shuffle = _mm256_set_epi8(
                -1, -1, -1, -1, -1, -1, -1, -1,
                0, 2, 4, 6, 8, 10, 12, 14,
                -1, -1, -1, -1, -1, -1, -1, -1,
                0, 2, 4, 6, 8, 10, 12, 14 
        );

        let packed = _mm256_shuffle_epi8(combined, shuffle);

        // extract each 128-bit lane and pull out low 64 bits
        let lane0 = _mm256_castsi256_si128(packed);
        let lane1 = _mm256_extracti128_si256::<1>(packed);

        // cast each lane to u64
        let result0 = _mm_cvtsi128_si64(lane0).cast_unsigned(); 
        let result1 = _mm_cvtsi128_si64(lane1).cast_unsigned();
        
        // return both results
        (result0, result1)

} // parse_line_pair_simd


// For edge case of no caches - return number of lines in trace
pub fn return_lines(path: &str) -> Result<u64, Box<dyn std::error::Error>> {
        let file = File::open(path)?;
        let mmap = unsafe { MmapOptions::new().map(&file)? };
        Ok((mmap.len() / 40) as u64)
}


// Main function to process tracefile - takes a closure which is called for each trace entry
// This allows for flexible processing of the tracefile without needing to load it all into memory at once. 
// Uses memory mapping and SIMD optimisations for fast processing of large tracefiles.
pub fn process_trace<F>(path: &str, mut process: F) -> Result<(), Box<dyn std::error::Error>>
where
        F: FnMut(TraceEntry),
{
        let file = File::open(path)?;

        // Mmap::populate prefaults in all pages to avoid page faults during processing
        let mmap = unsafe { MmapOptions::new().populate().map(&file)? };

        // advise kernel of access pattern for better readahead
        // no need for MADV_WILLNEED since populate already faults pages in
        // MADV_SEQUENTIAL to tell kernel we will use a linear access pattern
        // - allows for more aggressive readahead and for deallocating pages after we have accessed them
        unsafe {
                libc::madvise(
                        mmap.as_ptr() as *mut libc::c_void,
                        mmap.len(),
                        libc::MADV_SEQUENTIAL | libc::MADV_HUGEPAGE,
                );
        }

        let line_width = 40; // 40 bytes per line in trace file (39 chars + newline)
        let total_lines = mmap.len() / line_width;
                                        
        // process quads of lines with dual-lane AVX2
        let quads = total_lines / 4;
        for i in 0..quads {
                let offset = (i * 4) * line_width;

                // prefetch several quads ahead to hide memory latency
                let prefetch_offset = offset + 64 * line_width; // 8 quads ahead
                if prefetch_offset < mmap.len() {
                        unsafe { _mm_prefetch::<_MM_HINT_T0>(mmap.as_ptr().add(prefetch_offset) as *const i8); }
                }

                unsafe {
                        // Not sure why - but parsing and processing 2 pairs sequentially seems to be fasted than parsing and processing a quad sequentially  
                        let ptr0 = mmap.as_ptr().add(offset);
                        let ptr1 = mmap.as_ptr().add(offset + line_width);
                        let (addr0, addr1) = parse_line_pair_simd(ptr0.add(17), ptr1.add(17));
                        let (size0, size1) = parse_decimal_pair(ptr0.add(36), ptr1.add(36));
                        // pass entries into closure for processing through the cache hierarchy
                        process(TraceEntry { address: addr0, size: size0 });
                        process(TraceEntry { address: addr1, size: size1 });

                        // so do lines 2 by 2, maybe allows for better pipelining
                        let ptr2 = mmap.as_ptr().add(offset + 2 * line_width);
                        let ptr3 = mmap.as_ptr().add(offset + 3 * line_width);
                        let (addr2, addr3) = parse_line_pair_simd(ptr2.add(17), ptr3.add(17));
                        let (size2, size3) = parse_decimal_pair(ptr2.add(36), ptr3.add(36));
                        process(TraceEntry { address: addr2, size: size2 });
                        process(TraceEntry { address: addr3, size: size3 });
  
                }

        }
        
        // process last line if batch_len is odd
        for j in (quads * 4)..total_lines {
                let line_offset = j * line_width;
                unsafe {
                        let ptr = mmap.as_ptr().add(line_offset);
                        process(TraceEntry {
                                // using non-simd parsing in these cases
                                address: parse_hex_u64_no_simd(ptr.add(17)),
                                size: parse_decimal_u64(ptr.add(36)),
                        });
                }
        }

        Ok(())
} // process_trace
