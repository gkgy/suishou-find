use miao_find::index::{self, Category, SortOrder};
use std::{
    hint::black_box,
    path::{Path, PathBuf},
    time::Instant,
};
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let index = index::load_compact_with_dates(Path::new(&args[1]), Path::new(&args[2])).unwrap();
    index::warm_up_parallel_search();
    let home = directories::BaseDirs::new().unwrap().home_dir().to_owned();
    let roots: Vec<PathBuf> = [
        "Desktop",
        "Documents",
        "Downloads",
        "Pictures",
        "Movies",
        "Music",
        "AI",
    ]
    .into_iter()
    .map(|d| home.join(d))
    .collect();
    let started = Instant::now();
    let mask = index.personal_mask(&roots);
    eprintln!(
        "entries={} threads={} scope_cache_ms={:.3} warmup=3 rounds=21",
        index.len(),
        rayon::current_num_threads(),
        started.elapsed().as_secs_f64() * 1000.0
    );
    println!("query,mode,median_ms,p95_ms,results");
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
        let old = index::search_sorted_with_scope(
            &index,
            query,
            500,
            Category::All,
            SortOrder::Relevance,
            false,
        );
        let new = index::search_with_filters(
            &index,
            query,
            500,
            Category::All,
            SortOrder::Relevance,
            false,
            None,
            None,
        );
        assert_eq!(old, new);
        // Alternate order each round to reduce thermal/load bias.
        let mut measurements = [Vec::new(), Vec::new()];
        for round in 0..24 {
            for mode in if round % 2 == 0 { [0, 1] } else { [1, 0] } {
                let t = Instant::now();
                let result = if mode == 0 {
                    index::search_sorted_with_scope(
                        &index,
                        query,
                        500,
                        Category::All,
                        SortOrder::Relevance,
                        false,
                    )
                } else {
                    index::search_with_filters(
                        &index,
                        query,
                        500,
                        Category::All,
                        SortOrder::Relevance,
                        false,
                        None,
                        None,
                    )
                };
                black_box(result);
                if round >= 3 {
                    measurements[mode].push(t.elapsed().as_secs_f64() * 1000.0);
                }
            }
        }
        for (i, times) in measurements.iter_mut().enumerate() {
            times.sort_by(f64::total_cmp);
            println!(
                "{query},{},{:.3},{:.3},{}",
                if i == 0 { "baseline" } else { "new-default" },
                times[10],
                times[19],
                new.len()
            );
        }
    }
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
        - 7 * 86400 * 1000;
    for query in ["", "/output", "/output baby", "/article-share"] {
        let mut times = Vec::new();
        let mut count = 0;
        for round in 0..24 {
            let t = Instant::now();
            let result = index::search_with_options(
                &index,
                query,
                500,
                Category::Image,
                SortOrder::Latest,
                false,
                Some(&mask),
                Some(since),
                true,
            );
            count = black_box(result).len();
            if round >= 3 {
                times.push(t.elapsed().as_secs_f64() * 1000.0);
            }
        }
        times.sort_by(f64::total_cmp);
        println!(
            "{query},personal-images-7days,{:.3},{:.3},{count}",
            times[10], times[19]
        );
    }
    let until = since + 7 * 86_400_000;
    let mut times = Vec::new();
    let mut count = 0;
    for round in 0..24 {
        let t = Instant::now();
        let hits = index::search_with_date_range(
            &index,
            "/output",
            500,
            Category::Image,
            SortOrder::Latest,
            false,
            Some(&mask),
            Some(since),
            Some(until),
            true,
        );
        assert!(
            hits.iter()
                .all(|&id| index.modified(id).is_some_and(|d| d >= since && d < until))
        );
        count = black_box(hits).len();
        if round >= 3 {
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
    }
    times.sort_by(f64::total_cmp);
    println!(
        "/output,personal-images-custom-range,{:.3},{:.3},{count}",
        times[10], times[19]
    );
}
