//! `convert_text_layer` (the `pdf-text` / wasm32 path) must produce the same
//! extraction as the full pipeline's `no_ocr` flag: both run the pure-Rust
//! text parser through the orphan-region assembly, differing only in the
//! entry point. Runs under the default (ml) feature so
//! both entries exist to compare. (The scanned-table e2e below shares this
//! binary rather than its own: every docling-pdf test target statically links
//! onnxruntime, so one target carries all the pipeline e2es.)

#![cfg(feature = "ml")]

#[test]
fn text_layer_matches_no_ocr() {
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/data/pdf/sources/code_and_formula.pdf"
    ))
    .expect("corpus pdf");

    let text_layer = docling_pdf::convert_text_layer(&bytes, "code_and_formula.pdf")
        .expect("text-layer conversion");
    let no_ocr = docling_pdf::convert_with_options(
        &bytes,
        None,
        "code_and_formula.pdf",
        true,  // no_table_former (moot: no_ocr skips it anyway)
        true,  // no_ocr — the path convert_text_layer mirrors
        false, // force_full_page_ocr (ignored under no_ocr)
        false, // no_text_panels (moot: no_ocr never demotes)
        docling_pdf::EnrichmentOptions::default(),
        None,
        None,
    )
    .expect("no_ocr conversion");

    let a = text_layer.export_to_markdown();
    assert!(!a.trim().is_empty(), "text layer should extract");
    assert_eq!(a, no_ocr.export_to_markdown());
}

/// #173: a scanned (image-only) page with an uncaptioned bar chart above a
/// bordered data table. The chart must stay a picture (line-height uniformity
/// gate) and the table must extract with its OCR'd cell text — region-scoped
/// OCR skips table interiors, so the words come from the dedicated
/// `ocr_table_words` pass; without it the cell matcher saw no words and the
/// table dissolved. Needs the layout/OCR/TableFormer models, so it skips on a
/// model-free CI checkout.
#[test]
fn scanned_page_extracts_table_and_keeps_chart() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    // The page renders through the pure-Rust raster; pdfium is not needed.
    for needed in [
        ".models/layout_heron_int8.onnx",
        ".models/ocr_rec_en.onnx",
        ".models/tableformer/encoder.onnx",
    ] {
        if !root.join(needed).exists() {
            eprintln!("skipping scanned-table e2e: {needed} not found");
            return;
        }
    }
    // Model resolution is CWD-relative; tests run from the crate dir.
    std::env::set_current_dir(&root).expect("chdir to repo root");
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/data/scanned/sources/scanned_chart_table.pdf"
    ))
    .expect("scanned fixture");
    let doc =
        docling_pdf::convert(&bytes, None, "scanned_chart_table.pdf").expect("scanned conversion");
    let md = doc.export_to_markdown();
    assert!(
        md.contains("<!-- image -->"),
        "uncaptioned chart must keep its picture crop:\n{md}"
    );
    // The 8x4 bordered table: header + 7 body rows, cells carrying OCR text.
    let rows: Vec<&str> = md.lines().filter(|l| l.starts_with('|')).collect();
    assert!(
        rows.len() >= 9,
        "expected a header + 7-row table, got {} pipe rows:\n{md}",
        rows.len()
    );
    for cell in ["Aquifer", "Guarani", "Nubian", "380", "540"] {
        assert!(md.contains(cell), "table cell {cell:?} missing:\n{md}");
    }
}

#[test]
fn scanned_pdf_yields_empty_document() {
    // An image-only page — the no-text-layer contract is an empty doc, not
    // an error (callers decide whether to fall back to OCR).
    let scan = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/data/scanned/sources/ocr_test_raster.pdf"),
    )
    .unwrap();
    let doc = docling_pdf::convert_text_layer(&scan, "scan.pdf").expect("no error");
    assert!(doc.nodes.is_empty());
    // (Bytes no reader opens are an error, not an empty document:
    // `lib.rs`'s `text_layer_path_reports_an_unreadable_file`.)
}

/// #211: HEIC is detected by content, and without the `heif` feature the
/// error names the fix instead of a generic decode failure. Runs before any
/// model loads, so it needs no assets. (With the feature on, the same bytes
/// fail in libheif instead — a 12-byte file is not a real container.)
#[test]
fn heif_without_feature_reports_clearly() {
    let stub = b"\x00\x00\x00\x18ftypheic\0\0\0\0";
    let err = docling_pdf::convert_image(stub, "photo.heic").unwrap_err();
    let msg = err.to_string();
    #[cfg(not(feature = "heif"))]
    assert!(msg.contains("--features heif"), "unexpected error: {msg}");
    #[cfg(feature = "heif")]
    assert!(msg.contains("heif"), "unexpected error: {msg}");
}

/// #517: ONNX Runtime 1.26–1.28's x86 NCHWc rewrite collapsed the picture
/// classifier onto one distribution for every input (`table` 0.091 first).
/// Three unambiguous figures from the corpus must each win their own class
/// clearly (ONNX Runtime 1.29 gives them 0.985–0.999; the bar is 0.9).
#[test]
fn picture_classifier_tells_pictures_apart() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let model = root.join(".models/picture_classifier.onnx");
    if !model.exists() {
        eprintln!(
            "skipping picture-classifier check: {} not found",
            model.display()
        );
        return;
    }
    std::env::set_var("DOCLING_PICTURE_CLASSIFIER_ONNX", &model);
    let mut classifier =
        docling_pdf::enrich::PictureClassifier::load_with(1).expect("classifier loads");
    for (file, class) in [
        (
            "latex/sources/2310.06825/images/230927_bars.png",
            "bar_chart",
        ),
        (
            "latex/sources/1706.03762/Figures/ModalNet-21.png",
            "flow_chart",
        ),
        ("scanned/sources/qr_bill_example.jpg", "qr_code"),
    ] {
        let img = image::open(root.join("tests/data").join(file))
            .expect("fixture image")
            .to_rgb8();
        let preds = classifier.classify(&img).expect("classify");
        let top = &preds[0];
        assert!(
            top.class_name == class && top.confidence > 0.9,
            "{file}: expected {class}, got {} {:.3}",
            top.class_name,
            top.confidence
        );
    }
}
