// #[global_allocator]
// static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

// originally tried using mimalloc as an allocator alternative but no noticeable performance difference

use std::env;
use std::io;
use std::io::Write;

use serde::{Deserialize, Serialize};

mod tracer;
use tracer::{process_trace, return_lines};

mod json_reader;
use json_reader::read_json;

mod caches;
use caches::{
    CacheMeta, cache::Cache, cache::MainMemory, direct::DirectCache, fullassoc::FullAssocCache,
    n_way::NWayCache,
};

/*
CS4202 - Cache Simulation
*/

// CacheResult struct for collecting results of each cache
#[derive(Serialize, Deserialize)]
struct CacheResult {
    hits: u64,
    misses: u64,
    name: String,
}

// SimulationResults for outputting simulation results as JSON
#[derive(Serialize, Deserialize)]
struct SimulationResults {
    caches: Vec<CacheResult>,
    main_memory_accesses: u64,
}

// Macro to dispatch and create appropriate cache struct for input metadata, then run body on that struct
// It's a bit ugly, but allows us to avoid dynamic dispatch and still have a clean way to build the cache hierarchy
// - that doesn't infinitely recurse, and allows for high performance
macro_rules! dispatch {
    ($meta:expr, $lower:expr, $body:expr) => {
        match $meta.kind {
            0 => {
                let cache = DirectCache::new(&$meta.clone(), $lower);
                $body(cache)
            }
            1 => {
                let cache = NWayCache::<2, _>::new(&$meta.clone(), $lower);
                $body(cache)
            }
            2 => {
                let cache = NWayCache::<4, _>::new(&$meta.clone(), $lower);
                $body(cache)
            }
            3 => {
                let cache = NWayCache::<8, _>::new(&$meta.clone(), $lower);
                $body(cache)
            }
            4 => {
                let cache = FullAssocCache::new(&$meta.clone(), $lower);
                $body(cache)
            }
            _ => panic!("Unsupported cache type"),
        }
    };
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let json_path = &args[1];
    let trace_path = &args[2];

    // Read json file using serde
    let cachemetas: Vec<CacheMeta> = read_json(json_path);

    // Store cache names in order (top to bottom)
    let cache_names: Vec<String> = cachemetas.iter().map(|meta| meta.name.clone()).collect();

    // Build cache hierarchy from bottom up, starting with main memory as the lowest level.
    // For each level of the hierarchy, construct the appropriate cache with dispatch!
    // - and pass in the lower level cache as a reference.
    // Run trace through recursive hierarchy, pipelining trace entries directly into cache structure
    let mem = MainMemory::new();
    let tp = trace_path.as_str();
    let cn = &cache_names;

    match cachemetas.len() {
        0 => run_mma(tp),
        1 => {
            let _ = dispatch!(&cachemetas[0], mem, |mut top| run(&mut top, tp, cn));
        }
        2 => {
            let _ = dispatch!(&cachemetas[1], mem, |l2| {
                dispatch!(&cachemetas[0], l2, |mut top| run(&mut top, tp, cn))
            });
        }
        3 => {
            let _ = dispatch!(&cachemetas[2], mem, |l3| {
                dispatch!(&cachemetas[1], l3, |l2| {
                    dispatch!(&cachemetas[0], l2, |mut top| run(&mut top, tp, cn))
                })
            });
        }
        _ => {
            // In the case of more than 3 cache levels:
            // Levels are built dynamically with Box<dyn>, top 3 are built monomorphically as above.
            let dyn_bottom: Box<dyn Cache> = Box::new(mem);
            let dyn_metas: Vec<CacheMeta> = cachemetas[3..].iter().rev().cloned().collect();
            let dyn_chain = build_dyn_chain(&dyn_metas, dyn_bottom);

            let _ = dispatch!(&cachemetas[2], dyn_chain, |l3| {
                dispatch!(&cachemetas[1], l3, |l2| {
                    dispatch!(&cachemetas[0], l2, |mut top| run(&mut top, tp, cn))
                })
            });
        }
    }
} // main

// Builds chain of dynamic boxed caches for cache structures of arbitrary length.
fn build_dyn_chain(metas: &[CacheMeta], bottom: Box<dyn Cache>) -> Box<dyn Cache> {
    let mut current = bottom;
    for meta in metas {
        current = dispatch_boxed(meta, current);
    }
    current
}

// Dynamic dispatch for building dynamically sized hierarchy.
fn dispatch_boxed(meta: &CacheMeta, lower: Box<dyn Cache>) -> Box<dyn Cache> {
    match meta.kind {
        0 => Box::new(DirectCache::new(meta, lower)),
        1 => Box::new(NWayCache::<2, _>::new(meta, lower)),
        2 => Box::new(NWayCache::<4, _>::new(meta, lower)),
        3 => Box::new(NWayCache::<8, _>::new(meta, lower)),
        4 => Box::new(FullAssocCache::new(meta, lower)),
        _ => panic!("Unsupported cache type"),
    }
}

// Run tracefile entries through cache hierarchy
fn run<C: Cache>(cache: &mut C, trace_path: &str, cache_names: &[String]) {
    // Start processing tracefile & run through cache sim
    run_trace(cache, trace_path);

    // Get total hits and misses for all caches
    let hits = cache.get_hits(Vec::new());
    let misses = cache.get_misses(Vec::new());
    let mma = cache.get_main_memory_accesses();

    // Build results structure
    let cache_results: Vec<CacheResult> = cache_names
        .iter()
        .enumerate()
        .map(|(i, name)| CacheResult {
            hits: hits[i],
            misses: misses[i],
            name: name.clone(),
        })
        .collect();

    // Pass results into final json output struct...
    let results = SimulationResults {
        caches: cache_results,
        main_memory_accesses: mma,
    };

    // ... serialise results and output to stdout
    let json_output = serde_json::to_string_pretty(&results).expect("Failed to serialize results");
    println!("{json_output}");
} // run

// Runs tracefile entries through cache hierarchy, pipelining entries directly into cache structure without intermediate buffering
fn run_trace<C: Cache>(top_cache: &mut C, trace_path: &str) {
    let _ = process_trace(trace_path, |entry| {
        top_cache.access(entry.address, entry.size);
    });
}

// Edge case where no caches are passed in. Return number of accesses as Main Memory accesses.
fn run_mma(path: &str) {
    let results = SimulationResults {
        caches: Vec::new(),
        main_memory_accesses: return_lines(path).unwrap(),
    };

    let json_output = serde_json::to_string_pretty(&results).expect("Failed to serialize results");
    print!("{json_output}");
    io::stdout().flush().unwrap();

    let mut output_file =
        std::fs::File::create("results.json").expect("Failed to create output file");
    std::io::Write::write_all(&mut output_file, json_output.as_bytes())
        .expect("Failed to write results to file");
}
