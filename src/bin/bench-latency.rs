//! Benchmark the shipped search modes against a frozen index and modification-date cache.
use miao_find::index::{self, Category, SortOrder};
use std::{hint::black_box, path::Path, time::Instant};
fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(
        args.len(),
        3,
        "usage: bench-latency INDEX_SNAPSHOT DATE_CACHE"
    );
    let index = index::load_compact_with_dates(Path::new(&args[1]), Path::new(&args[2])).unwrap();
    index::warm_up_parallel_search();
    eprintln!(
        "entries={} threads={} warmup=3 rounds=21 limit=500",
        index.len(),
        rayon::current_num_threads()
    );
    println!("query,scope,sort,median_ms,p95_ms,results");
    for query in [
        "output",
        "pdf",
        "md",
        "SCADA",
        "冷暖站 md",
        "不存在xyz987",
        "Cargo.toml",
        "photo png",
    ] {
        for paths in [false, true] {
            for order in [SortOrder::Latest, SortOrder::Oldest, SortOrder::Name] {
                let run = || {
                    index::search_sorted_with_scope(&index, query, 500, Category::All, order, paths)
                };
                for _ in 0..3 {
                    black_box(run());
                }
                let mut times = Vec::new();
                let mut count = 0;
                for _ in 0..21 {
                    let started = Instant::now();
                    count = black_box(run()).len();
                    times.push(started.elapsed().as_secs_f64() * 1000.0);
                }
                times.sort_by(f64::total_cmp);
                println!(
                    "{query},{},{order:?},{:.3},{:.3},{count}",
                    if paths { "path" } else { "name" },
                    times[10],
                    times[19]
                );
            }
        }
    }
}
