//! `.dclx` packaging: the DocLang OPC archive (`doclang.pack` counterpart).
//!
//! Layout: `[Content_Types].xml`, `_rels/.rels` (both static bytes, matching
//! the Python `doclang` package verbatim), one `assets/image_NNNNNN_<sha256>.png`
//! part per picture the markup references (docling's `save_as_doclang_archive`
//! stores every picture asset as PNG), and `document.xml` — the
//! [`DoclingDocument::export_to_doclang`] markup plus a single trailing
//! newline. Entries are deflate-compressed and written in the reference's
//! lexicographic order. Page images (`pages/`) are not emitted: the backends
//! keep none by default, like docling.

use std::io::Write;

use docling_core::DoclingDocument;
use zip::write::SimpleFileOptions;

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="png" ContentType="image/png"/>
  <Default Extension="jpg" ContentType="image/jpeg"/>
  <Default Extension="jpeg" ContentType="image/jpeg"/>
  <Default Extension="webp" ContentType="image/webp"/>
  <Override PartName="/document.xml" ContentType="application/vnd.doclang.document+xml"/>
</Types>
"#;

const RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1"
    Type="http://doclang.ai/ns/package/2026/relationships/document"
    Target="document.xml"/>
</Relationships>
"#;

/// Serialize `doc` into `.dclx` bytes.
pub fn to_dclx_bytes(doc: &DoclingDocument) -> Vec<u8> {
    let (xml, assets) = doc.export_to_doclang_with_assets();
    let xml = format!("{xml}\n");
    // PNG and JPEG pictures arrive as PNG; anything else is converted here.
    // An image no decoder reads is left out (its `<src>` still names it, as a
    // docling archive would name a picture PIL failed to save).
    let mut assets: Vec<(String, Vec<u8>)> = assets
        .into_iter()
        .filter_map(|(path, bytes)| {
            if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                return Some((path, bytes));
            }
            let img = image::load_from_memory(&bytes).ok()?;
            let mut png = std::io::Cursor::new(Vec::new());
            img.write_to(&mut png, image::ImageFormat::Png).ok()?;
            Some((path, png.into_inner()))
        })
        .collect();
    // The zero-padded index already orders them; sort anyway, the reference
    // writes its staged tree lexicographically.
    assets.sort_by(|a, b| a.0.cmp(&b.0));
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let opts =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        // Reference order: lexicographic over the staged tree.
        zip.start_file("[Content_Types].xml", opts)
            .expect("zip start");
        zip.write_all(CONTENT_TYPES.as_bytes()).expect("zip write");
        zip.start_file("_rels/.rels", opts).expect("zip start");
        zip.write_all(RELS.as_bytes()).expect("zip write");
        for (path, bytes) in &assets {
            zip.start_file(path.as_str(), opts).expect("zip start");
            zip.write_all(bytes).expect("zip write");
        }
        zip.start_file("document.xml", opts).expect("zip start");
        zip.write_all(xml.as_bytes()).expect("zip write");
        zip.finish().expect("zip finish");
    }
    buf.into_inner()
}

/// Write `doc` as a `.dclx` archive at `path`.
pub fn save_as_dclx(doc: &DoclingDocument, path: &std::path::Path) -> std::io::Result<()> {
    std::fs::write(path, to_dclx_bytes(doc))
}

/// Deflate arbitrary `(name, bytes)` entries into one zip archive — the
/// generic sibling of [`to_dclx_bytes`], for callers that batch rendered
/// outputs rather than OPC parts (docling-serve's `zip` output target, #303).
pub fn zip_bytes<'a>(entries: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let opts =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            zip.start_file(name, opts).expect("zip start");
            zip.write_all(bytes).expect("zip write");
        }
        zip.finish().expect("zip finish");
    }
    buf.into_inner()
}
