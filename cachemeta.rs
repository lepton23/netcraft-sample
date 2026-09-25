
// Cache metadata struct
// use same data type for each field of struct for cache alignment
#[derive(Debug, Clone)]
pub struct CacheMeta {
        pub name:          String,
        pub size:           u64,
        pub line_size:      u64,
        pub kind:           u8, // 0 = direct, 1 = 2way, 2 = 4way, 3 = 8way, 4 = fullassoc
        pub rp:             u8, // 0 = rr, 1 = lru, 2 = lfu
}