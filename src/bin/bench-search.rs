#[allow(dead_code)]
#[path = "../../bench/baseline_index.rs"]
mod baseline;
#[allow(dead_code)]
#[path = "../index.rs"]
mod optimized;
use std::{hint::black_box, path::Path, time::Instant};
fn main() {
    let path = std::env::args().nth(1).expect("index snapshot path");
    let old = baseline::load_compact(Path::new(&path)).unwrap();
    let new = optimized::load_compact(Path::new(&path)).unwrap();
    optimized::warm_up_parallel_search();
    println!(
        "entries={}\nquery,baseline_ms,optimized_ms,speedup,results",
        new.len()
    );
    for q in [
        "pdf",
        "md",
        "扫码",
        "SCADA",
        "冷暖站 md",
        "tmp photo",
        "不存在xyz987",
        "Cargo.toml",
        "FreeVideo",
        "供热",
        "rust src",
        "photo png",
    ] {
        let a = baseline::search(&old, q, usize::MAX);
        let b = optimized::search(&new, q, usize::MAX);
        assert_eq!(a, b, "complete result mismatch for {q}");
        for category in optimized::Category::ALL {
            let expected: Vec<_> = b
                .iter()
                .copied()
                .filter(|&id| category == optimized::Category::All || new.category(id) == category)
                .collect();
            assert_eq!(
                optimized::search_filtered(&new, q, usize::MAX, category),
                expected,
                "category mismatch for {q} {category:?}"
            );
            let limited = optimized::search_filtered(&new, q, 500, category);
            assert_eq!(limited.len(), expected.len().min(500));
            assert!(limited.iter().all(|id| expected.contains(id)));
        }
        let expected = b.len().min(500);
        for _ in 0..3 {
            black_box(baseline::search(&old, q, 500));
            black_box(optimized::search(&new, q, 500));
        }
        let mut aa = Vec::new();
        let mut bb = Vec::new();
        for _ in 0..15 {
            let t = Instant::now();
            let a = black_box(baseline::search(&old, q, 500));
            aa.push(t.elapsed().as_secs_f64() * 1000.);
            let t = Instant::now();
            let b = black_box(optimized::search(&new, q, 500));
            bb.push(t.elapsed().as_secs_f64() * 1000.);
            assert_eq!(a.len(), expected);
            assert_eq!(b.len(), expected);
        }
        aa.sort_by(f64::total_cmp);
        bb.sort_by(f64::total_cmp);
        println!(
            "{q},{:.3},{:.3},{:.2},{}",
            aa[7],
            bb[7],
            aa[7] / bb[7],
            expected
        );
    }
}
