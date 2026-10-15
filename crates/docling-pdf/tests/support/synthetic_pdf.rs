//! A synthetic dense-table PDF: `pages` US-letter pages, each a heading and a
//! `rows` × `cols` grid of short numeric cells in Helvetica — the shape (a few
//! hundred small text items per page, thousands of pages) that the text-layer
//! memory regressions scale with. Uncompressed and deterministic, so tests and
//! the `text_layer_memory` example need no fixture file.

pub fn dense_table_pdf(pages: usize, rows: usize, cols: usize) -> Vec<u8> {
    // Objects: 1 catalog, 2 page tree, 3 font, then (page, content) pairs.
    let page_id = |p: usize| 4 + 2 * p;
    let kids: Vec<String> = (0..pages).map(|p| format!("{} 0 R", page_id(p))).collect();
    let mut objs: Vec<Vec<u8>> = vec![
        b"<</Type/Catalog/Pages 2 0 R>>".to_vec(),
        format!("<</Type/Pages/Kids[{}]/Count {pages}>>", kids.join(" ")).into_bytes(),
        b"<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".to_vec(),
    ];
    for p in 0..pages {
        let mut content = format!("BT /F1 14 Tf 40 750 Td (Table {}) Tj ET\n", p + 1);
        for r in 0..rows {
            for c in 0..cols {
                let v = (p * 7919 + r * 104_729 + c * 1_299_709) % 100_000;
                let (x, y) = (40 + c * 52, 720 - r * 12);
                content.push_str(&format!(
                    "BT /F1 8 Tf {x} {y} Td ({}.{:02}) Tj ET\n",
                    v / 100,
                    v % 100
                ));
            }
        }
        objs.push(
            format!(
                "<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Contents {} 0 R\
                 /Resources<</Font<</F1 3 0 R>>>>>>",
                page_id(p) + 1
            )
            .into_bytes(),
        );
        let mut stream = format!("<</Length {}>>stream\n", content.len()).into_bytes();
        stream.extend_from_slice(content.as_bytes());
        stream.extend_from_slice(b"endstream");
        objs.push(stream);
    }
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::with_capacity(objs.len());
    for (i, body) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"endobj\n");
    }
    let xref_at = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for off in &offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer<</Size {}/Root 1 0 R>>\nstartxref\n{xref_at}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}
