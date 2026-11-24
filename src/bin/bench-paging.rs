use miao_find::index::{self, Category, SearchSession, SortOrder};
use std::{hint::black_box, path::Path, time::Instant};
fn stats(mut values: Vec<f64>) -> (f64, f64) {
    values.sort_by(f64::total_cmp);
    (values[values.len() / 2], values[values.len() * 95 / 100])
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let index = index::load_compact_with_dates(Path::new(&args[1]), Path::new(&args[2])).unwrap();
    index::warm_up_parallel_search();
    eprintln!("entries={} warmup=3 rounds=21", index.len());
    println!(
        "query,order,total,old_first_median,new_first_median,new_first_p95,page2_median,jump_median"
    );
    for (query, order) in [
        ("a", SortOrder::Relevance),
        (".", SortOrder::Relevance),
        ("output", SortOrder::Relevance),
        ("pdf", SortOrder::Relevance),
        ("md", SortOrder::Relevance),
        ("SCADA", SortOrder::Relevance),
        ("冷暖站 md", SortOrder::Relevance),
        ("不存在xyz987", SortOrder::Relevance),
        ("/ComfyUI", SortOrder::Latest),
        ("md", SortOrder::Name),
        ("md", SortOrder::Latest),
    ] {
        let make = || {
            SearchSession::new(
                &index,
                query,
                Category::All,
                order,
                false,
                None,
                None,
                None,
                false,
            )
        };
        let mut timings = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        let mut total = 0;
        for round in 0..24 {
            let mut old = Vec::new();
            let mut first = Vec::new();
            let mut session = None;
            for mode in if round % 2 == 0 { [0, 1] } else { [1, 0] } {
                let t = Instant::now();
                if mode == 0 {
                    old = index::search_with_date_range(
                        &index,
                        query,
                        500,
                        Category::All,
                        order,
                        false,
                        None,
                        None,
                        None,
                        false,
                    );
                } else {
                    let mut s = make();
                    total = s.total();
                    first = s.page(0, 500);
                    session = Some(s);
                }
                if round >= 3 {
                    timings[mode].push(t.elapsed().as_secs_f64() * 1000.0);
                }
            }
            assert_eq!(old, first, "first page mismatch {query}");
            let mut s = session.unwrap();
            for (slot, page) in [(2, 1), (3, (total.div_ceil(500) / 2).max(1))] {
                let t = Instant::now();
                black_box(s.page(page, 500));
                if round >= 3 {
                    timings[slot].push(t.elapsed().as_secs_f64() * 1000.0);
                }
            }
        }
        let stats: Vec<_> = timings.into_iter().map(stats).collect();
        println!(
            "{query},{order:?},{total},{:.3},{:.3},{:.3},{:.3},{:.3}",
            stats[0].0, stats[1].0, stats[1].1, stats[2].0, stats[3].0
        );
    }
}
