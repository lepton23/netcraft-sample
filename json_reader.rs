
use std::fs;
use serde_json::{Value};

use crate::caches::cachemeta::CacheMeta;

// generate cache(s) metadata from json
pub fn read_json(path: &str) -> Vec<CacheMeta> {

        // read in json
        let json_data = fs::read_to_string(path).expect("Unable to read json file");
        let json: Value = 
                serde_json::from_str(&json_data).expect("\nJSON formatted incorrectly.\n\n");

        let json_caches = json["caches"].as_array().expect("json formatting issue");

        let mut cachemetas: Vec::<CacheMeta> = Vec::with_capacity(json_caches.len());

        for cache in json_caches {

                let name = cache["name"].as_str().expect("Expected str for Name");
                let size = cache["size"].as_u64().expect("Expected u64 for size");
                let line_size = cache["line_size"].as_u64().expect("Expected u64 for line size");
                let kind = cache["kind"].as_str().expect("Expected str for kind");

                let mut rp = "rr";
                if !cache["replacement_policy"].is_null() {
                        rp = cache["replacement_policy"].as_str().expect("Expected str for replacement policy");
                }

                let size_u64: u64 = size;
                let line_size_u64: u64 = line_size;

                let kind_u8: u8 = match kind {
                        "direct"        => 0,
                        "2way"          => 1,
                        "4way"          => 2,
                        "8way"          => 3,
                        "full"          => 4,
                        _               => panic!("Unkown cache kind: {kind}"),
                };

                let rp_u8: u8 = match rp {
                        "lru"   => 1,
                        "lfu"   => 2,
                        _       => 0,
                };

                cachemetas.push(
                        CacheMeta {
                                name: name.to_string(),
                                size: size_u64, 
                                line_size: line_size_u64, 
                                kind: kind_u8, 
                                rp: rp_u8
                        }
                );
        }

        cachemetas
}