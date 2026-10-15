//! Peak heap of the text-layer conversion on a long, dense PDF.
//!
//! The text layer used to be parsed for the whole file and every page's cells
//! held until assembly finished — with ~30× dead capacity per page left over
//! from the per-glyph buffer — so a 6.6k-page table-heavy PDF peaked at 6.8 GB
//! whatever page window was asked for. Pages are now parsed, assembled and
//! dropped one at a time, and a window only parses outside itself until the
//! vestigial-layer verdict is settled.
//!
//! Measured with a counting allocator, per thread so the harness's other test
//! threads don't leak into the numbers (the conversion is single-threaded).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

#[path = "support/synthetic_pdf.rs"]
mod synthetic_pdf;

struct PerThread;

thread_local! {
    static CUR: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

fn track(delta: isize) {
    let _ = CUR.try_with(|c| {
        let v = c.get() + delta;
        c.set(v);
        let _ = PEAK.try_with(|p| p.set(p.get().max(v)));
    });
}

unsafe impl GlobalAlloc for PerThread {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        track(l.size() as isize);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        track(-(l.size() as isize));
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        track(n as isize - l.size() as isize);
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static ALLOC: PerThread = PerThread;

/// Run `f`, returning its result and the peak heap it added on this thread.
fn peak_of<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let start = CUR.with(Cell::get);
    PEAK.with(|p| p.set(start));
    let out = f();
    (out, (PEAK.with(Cell::get) - start).max(0) as usize)
}

const PAGES: usize = 40;

#[test]
fn a_page_window_does_not_pay_for_the_whole_document() {
    let bytes = synthetic_pdf::dense_table_pdf(PAGES, 20, 10);

    let (full, full_peak) =
        peak_of(|| docling_pdf::convert_text_layer_pages(&bytes, "t.pdf", None).unwrap());
    let (one, one_peak) = peak_of(|| {
        docling_pdf::convert_text_layer_pages(&bytes, "t.pdf", Some((PAGES / 2, PAGES / 2)))
            .unwrap()
    });
    eprintln!("whole document: peak {full_peak} B; one-page window: peak {one_peak} B");
    assert!(full.nodes.len() > PAGES * 200, "{} nodes", full.nodes.len());
    assert!(!one.nodes.is_empty() && one.nodes.len() * 10 < full.nodes.len());
    // A one-page window costs the document load plus one page; it used to
    // cost every page's cells (≈ the whole conversion's peak).
    assert!(
        one_peak * 4 < full_peak,
        "one-page window peaked at {one_peak} B, the whole document at {full_peak} B"
    );
}

#[test]
fn the_whole_document_peak_is_its_output_plus_one_page() {
    let bytes = synthetic_pdf::dense_table_pdf(PAGES, 20, 10);
    let start = CUR.with(Cell::get);
    let (doc, peak) =
        peak_of(|| docling_pdf::convert_text_layer_pages(&bytes, "t.pdf", None).unwrap());
    // What the returned document itself holds.
    let output = (CUR.with(Cell::get) - start).max(1) as usize;
    eprintln!("peak {peak} B, output {output} B");
    assert!(doc.nodes.len() > PAGES * 200);
    // Peak = the output + the loaded file + one page's parse state. Holding
    // every page's cells — each with its per-glyph buffer — put it at ~4×.
    assert!(
        peak < 2 * output,
        "peaked at {peak} B for a {output} B document"
    );
}

#[test]
fn page_windows_keep_the_vestigial_and_range_verdicts() {
    let bytes = synthetic_pdf::dense_table_pdf(3, 2, 2);
    // A window past the end of a real text layer is an error …
    assert!(docling_pdf::convert_text_layer_pages(&bytes, "t.pdf", Some((9, 9))).is_err());
    // … and a window inside it converts only its own page.
    let doc = docling_pdf::convert_text_layer_pages(&bytes, "t.pdf", Some((2, 2))).unwrap();
    let md = doc.export_to_markdown();
    assert!(md.contains("Table 2") && !md.contains("Table 1") && !md.contains("Table 3"));
    // A vestigial layer (three one-line pages, 21 characters) reads as the
    // empty document whatever the window — pages outside it still count
    // toward the verdict — and a window past its end is not an error, as
    // before (callers treat the empty document as "needs OCR").
    let vestigial = synthetic_pdf::dense_table_pdf(3, 0, 0);
    for window in [None, Some((2, 2)), Some((9, 9))] {
        let doc = docling_pdf::convert_text_layer_pages(&vestigial, "t.pdf", window).unwrap();
        assert!(doc.nodes.is_empty(), "{window:?}");
    }
}
