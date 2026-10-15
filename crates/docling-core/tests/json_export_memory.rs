//! Peak heap of `export_to_json_value` on a long document.
//!
//! The export's `json!` constructors deep-copy every interpolated `Value`
//! (`to_value`): the top-level object copied the finished `texts` array whole,
//! so the export briefly held its result twice — 22 GB at peak for a 10 GB
//! value on a 3M-item PDF. The hot constructors now move their parts in.
//!
//! Measured with a counting allocator, per thread so the harness's other test
//! threads don't leak into the numbers.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use docling_core::{DoclingDocument, Node};

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

/// `pages` pages of `per_page` short located paragraphs — the flat-node shape
/// the PDF text layer produces.
fn long_document(pages: usize, per_page: usize) -> DoclingDocument {
    let mut doc = DoclingDocument::new("long");
    for p in 1..=pages {
        doc.nodes.push(Node::PageInfo {
            page_no: p,
            width: 612.0,
            height: 792.0,
        });
        for i in 0..per_page {
            let (x, y) = ((i % 10) as f32 * 50.0, (i / 10) as f32 * 12.0);
            doc.nodes.push(Node::Prov {
                page_no: p,
                bbox: [x, y, x + 40.0, y + 9.0],
                charspan: [0, 6],
                seq: None,
                inner: Box::new(Node::Paragraph {
                    text: format!("{:06}", p * per_page + i),
                }),
            });
        }
    }
    doc
}

#[test]
fn export_does_not_hold_its_result_twice() {
    let doc = long_document(50, 200);
    let start = CUR.with(Cell::get);
    PEAK.with(|p| p.set(start));
    let json = doc.export_to_json_value();
    let peak = (PEAK.with(Cell::get) - start) as usize;
    let output = (CUR.with(Cell::get) - start) as usize;
    eprintln!("export peak {peak} B for a {output} B value");
    assert_eq!(json["texts"].as_array().map(Vec::len), Some(50 * 200));
    assert!(
        peak * 4 < output * 5,
        "export peaked at {peak} B for a {output} B value"
    );
}

#[test]
fn write_json_writes_the_value_without_building_it() {
    let doc = long_document(50, 200);
    let start = CUR.with(Cell::get);
    let value = doc.export_to_json_value();
    let value_size = (CUR.with(Cell::get) - start) as usize;
    let expected = serde_json::to_vec(&value).unwrap();
    let pretty = serde_json::to_vec_pretty(&value).unwrap();
    drop(value);

    let mut out = Vec::with_capacity(expected.len());
    let start = CUR.with(Cell::get);
    PEAK.with(|p| p.set(start));
    doc.write_json(&mut out).unwrap();
    let peak = (PEAK.with(Cell::get) - start) as usize;
    eprintln!("write_json peak {peak} B; the Value is {value_size} B");
    assert!(
        out == expected,
        "write_json differs from serializing the Value"
    );
    // The typed items cost a fraction of their `Value`s.
    assert!(
        peak * 4 < value_size,
        "write_json peaked at {peak} B; the Value it avoids is {value_size} B"
    );

    let mut out = Vec::new();
    doc.write_json_pretty(&mut out).unwrap();
    assert!(out == pretty);
    assert_eq!(doc.export_to_json().into_bytes(), pretty);
}

/// Items the compact path doesn't cover — an ASR segment's `source` (#614),
/// a heading, an unlocated paragraph — and a box past the page edge (the
/// clamp) come out of `write_json` exactly as from the `Value`.
#[test]
fn write_json_matches_the_value_off_the_compact_path() {
    use docling_core::tree::TreeTrack;
    let mut doc = DoclingDocument::new("mixed");
    doc.nodes.push(Node::PageInfo {
        page_no: 1,
        width: 612.0,
        height: 792.0,
    });
    doc.nodes.push(Node::Track {
        track: TreeTrack {
            start_time: 0.0,
            end_time: 1.5,
            identifier: Some("c1".into()),
            voice: Some("Ann".into()),
        },
        cue: "hello".into(),
        inner: Box::new(Node::Prov {
            page_no: 1,
            bbox: [10.0, 20.0, 700.0, 30.0],
            charspan: [0, 5],
            seq: None,
            inner: Box::new(Node::Paragraph {
                text: "[time: 0.0-1.5] hello".into(),
            }),
        }),
    });
    doc.nodes.push(Node::Located {
        location: [10, 10, 200, 20],
        inner: Box::new(Node::Paragraph {
            text: "a \\_b &amp; c".into(),
        }),
    });
    doc.nodes.push(Node::Heading {
        level: 2,
        text: "Heading".into(),
    });
    doc.nodes.push(Node::Paragraph {
        text: "unlocated".into(),
    });

    let value = doc.export_to_json_value();
    let mut out = Vec::new();
    doc.write_json(&mut out).unwrap();
    assert!(out == serde_json::to_vec(&value).unwrap());

    let first = &value["texts"][0];
    let keys: Vec<&str> = first
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "self_ref",
            "parent",
            "children",
            "content_layer",
            "label",
            "prov",
            "source",
            "orig",
            "text"
        ]
    );
    assert_eq!(first["prov"][0]["bbox"]["r"], 612.0);
}
