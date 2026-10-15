//! Peak heap and time of the text-layer conversion and its JSON export on a
//! synthetic dense-table PDF (or a PDF of your own) — the numbers behind the
//! `text_layer_memory` regression tests, at a scale those tests can't afford.
//!
//! ```bash
//! cargo run --release -p docling-pdf --no-default-features \
//!     --example text_layer_memory -- [PAGES ROWS COLS | FILE.pdf] [FIRST LAST]
//! ```
//! Defaults: 2000 pages of 40 × 12 cells. `FIRST LAST` converts that 1-based
//! page window instead of the whole document.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

#[path = "../tests/support/synthetic_pdf.rs"]
mod synthetic_pdf;

struct Counting;
static CUR: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let c = CUR.fetch_add(l.size(), Relaxed) + l.size();
        PEAK.fetch_max(c, Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        CUR.fetch_sub(l.size(), Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if n > l.size() {
            let c = CUR.fetch_add(n - l.size(), Relaxed) + n - l.size();
            PEAK.fetch_max(c, Relaxed);
        } else {
            CUR.fetch_sub(l.size() - n, Relaxed);
        }
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn mb(b: usize) -> f64 {
    b as f64 / (1 << 20) as f64
}

/// Run `f` and print its time, its peak heap and what it left allocated.
fn stage<T>(name: &str, f: impl FnOnce() -> T) -> T {
    let start = CUR.load(Relaxed);
    PEAK.store(start, Relaxed);
    let t = Instant::now();
    let out = f();
    eprintln!(
        "{name:>8}: {:7.2} s  peak +{:9.1} MB  retained +{:9.1} MB",
        t.elapsed().as_secs_f64(),
        mb(PEAK.load(Relaxed) - start),
        mb(CUR.load(Relaxed).saturating_sub(start)),
    );
    out
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let num = |i: usize, d: usize| args.get(i).and_then(|a| a.parse().ok()).unwrap_or(d);
    let (bytes, rest) = match args.first() {
        Some(a) if a.parse::<usize>().is_err() => (std::fs::read(a).expect("read the pdf"), 1),
        _ => {
            let (p, r, c) = (num(0, 2000), num(1, 40), num(2, 12));
            eprintln!("synthetic: {p} pages × {r} rows × {c} cols");
            (synthetic_pdf::dense_table_pdf(p, r, c), 3)
        }
    };
    let window = match (args.get(rest), args.get(rest + 1)) {
        (Some(a), Some(b)) => Some((a.parse().unwrap(), b.parse().unwrap())),
        _ => None,
    };
    eprintln!("{:.1} MB of PDF, window {window:?}", mb(bytes.len()));
    let doc = stage("convert", || {
        docling_pdf::convert_text_layer_pages(&bytes, "input.pdf", window).expect("converts")
    });
    let json = stage("json", || doc.export_to_json_value());
    let texts = json["texts"].as_array().map_or(0, Vec::len);
    eprintln!("{} nodes, {texts} text items", doc.nodes.len());
}
