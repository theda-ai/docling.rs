//! Render one page with the pure-Rust renderer and, when installed, the
//! docling-parse shim, and write both plus a difference image:
//!
//!     cargo run -q -p docling-pdf --example render_compare -- file.pdf PAGE SCALE OUT_STEM
//!
//! Writes `OUT_STEM.rust.png`, `OUT_STEM.shim.png`, `OUT_STEM.diff.png` and
//! prints the mean absolute difference.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!("usage: render_compare file.pdf PAGE SCALE OUT_STEM");
        std::process::exit(2);
    }
    let bytes = std::fs::read(&args[1]).expect("read pdf");
    let page: usize = args[2].parse().expect("page");
    let scale: f64 = args[3].parse().expect("scale");
    let stem = &args[4];
    if std::env::var_os("DOCLING_RS_RENDER_DUMP").is_some() {
        dump(&bytes, page);
    }
    if let Ok(ids) = std::env::var("DOCLING_RS_RENDER_OBJ") {
        if let Ok(doc) = lopdf::Document::load_mem(&bytes) {
            for id in ids.split(',').filter_map(|s| s.trim().parse::<u32>().ok()) {
                match doc.get_object((id, 0)) {
                    Ok(lopdf::Object::Stream(s)) => {
                        let mut d = s.dict.clone();
                        d.remove(b"Length");
                        eprintln!("obj {id}: stream {d:?}");
                        if let Ok(c) = s.decompressed_content() {
                            let text = String::from_utf8_lossy(&c[..c.len().min(600)]);
                            eprintln!(
                                "   content ({} bytes): {}",
                                c.len(),
                                text.replace('\n', " ")
                            );
                        }
                    }
                    Ok(o) => eprintln!("obj {id}: {o:?}"),
                    Err(e) => eprintln!("obj {id}: {e}"),
                }
            }
        }
    }
    let meta = docling_pdf::pdf_meta::PdfMeta::open(&bytes).expect("lopdf open");
    let t = std::time::Instant::now();
    let rust = docling_pdf::render::render_page(&meta, page - 1, scale).expect("rust render");
    eprintln!(
        "rust: {}x{} in {:?}",
        rust.width(),
        rust.height(),
        t.elapsed()
    );
    rust.save(format!("{stem}.rust.png")).unwrap();
    if let Some(dp) = docling_pdf::dparse_render::Doc::open_if_enabled(&bytes, None) {
        let t = std::time::Instant::now();
        let shim = dp.render(page - 1, scale, 1.0).expect("shim render");
        eprintln!(
            "shim: {}x{} in {:?}",
            shim.width(),
            shim.height(),
            t.elapsed()
        );
        shim.save(format!("{stem}.shim.png")).unwrap();
        if shim.dimensions() == rust.dimensions() {
            let mut diff = image::RgbImage::new(rust.width(), rust.height());
            let mut sum = 0u64;
            for (d, (a, b)) in diff.pixels_mut().zip(rust.pixels().zip(shim.pixels())) {
                let mut m = 0u8;
                for c in 0..3 {
                    let v = (i32::from(a[c]) - i32::from(b[c])).unsigned_abs() as u8;
                    sum += u64::from(v);
                    m = m.max(v);
                }
                *d = image::Rgb([255 - m, 255 - m, 255]);
            }
            diff.save(format!("{stem}.diff.png")).unwrap();
            println!(
                "mean |Δ| = {:.3}",
                sum as f64 / (rust.as_raw().len() as f64)
            );
        } else {
            println!(
                "size mismatch: rust {:?} shim {:?}",
                rust.dimensions(),
                shim.dimensions()
            );
        }
    }
    docling_pdf::timing::report();
}

/// `DOCLING_RS_RENDER_DUMP=1`: print the page's operator histogram and its
/// first operators, to see what a blank render skipped.
#[allow(dead_code)]
fn dump(bytes: &[u8], page: usize) {
    let Ok(doc) = lopdf::Document::load_mem(bytes) else {
        return;
    };
    let mut pages: Vec<_> = doc.get_pages().into_iter().collect();
    pages.sort_by_key(|(n, _)| *n);
    let Some((_, pid)) = pages.get(page - 1) else {
        return;
    };
    let content = doc.get_page_content(*pid);
    let Ok(ops) = lopdf::content::Content::decode(&content) else {
        eprintln!("content undecodable");
        return;
    };
    let mut hist = std::collections::BTreeMap::<String, usize>::new();
    for op in &ops.operations {
        *hist.entry(op.operator.clone()).or_default() += 1;
    }
    eprintln!("ops: {hist:?}");
    for op in ops.operations.iter().take(80) {
        eprintln!("  {} {:?}", op.operator, op.operands);
    }
    // `DOCLING_RS_RENDER_OPS=Do`: every occurrence of that operator with the
    // six operators before it.
    if let Ok(want) = std::env::var("DOCLING_RS_RENDER_OPS") {
        for (i, op) in ops.operations.iter().enumerate() {
            if op.operator == want {
                let start = i.saturating_sub(6);
                let ctx: Vec<String> = ops.operations[start..=i]
                    .iter()
                    .map(|o| format!("{} {:?}", o.operator, o.operands))
                    .collect();
                eprintln!("… {}", ctx.join(" | "));
            }
        }
    }
    if let Ok((res, ids)) = doc.get_page_resources(*pid) {
        let res = res.or_else(|| ids.first().and_then(|id| doc.get_dictionary(*id).ok()));
        if let Some(res) = res {
            let mut r = res.clone();
            let fonts = r.remove(b"Font");
            let xobjs = r.remove(b"XObject");
            eprintln!("resources: {r:?}");
            if let Some(xo) = xobjs {
                let deref = |o: &lopdf::Object| -> lopdf::Object {
                    match o {
                        lopdf::Object::Reference(id) => {
                            doc.get_object(*id).cloned().unwrap_or(lopdf::Object::Null)
                        }
                        o => o.clone(),
                    }
                };
                if let lopdf::Object::Dictionary(xd) = deref(&xo) {
                    for (name, x) in xd.iter() {
                        if let lopdf::Object::Stream(s) = deref(x) {
                            let mut d = s.dict.clone();
                            d.remove(b"Length");
                            eprintln!(
                                "xobject /{} ({:?}): {:?}",
                                String::from_utf8_lossy(name),
                                x,
                                d
                            );
                            // JPEG markers: SOF component count, APP14 Adobe transform.
                            let raw = &s.content;
                            let mut i = 2;
                            while i + 4 <= raw.len() && raw[i] == 0xFF {
                                let m = raw[i + 1];
                                let len = usize::from(raw[i + 2]) << 8 | usize::from(raw[i + 3]);
                                if m == 0xEE && raw.len() > i + 15 {
                                    eprintln!(
                                        "   APP14 {:?} transform {}",
                                        std::str::from_utf8(&raw[i + 4..i + 9]).unwrap_or("?"),
                                        raw[i + 15]
                                    );
                                }
                                if (0xC0..=0xCF).contains(&m) && m != 0xC4 && m != 0xC8 && m != 0xCC
                                {
                                    eprintln!("   SOF{} components {}", m - 0xC0, raw[i + 9]);
                                    break;
                                }
                                if m == 0xDA {
                                    break;
                                }
                                i += 2 + len;
                            }
                            for key in ["SMask", "Mask", "ColorSpace"] {
                                if let Ok(v) = d.get(key.as_bytes()) {
                                    if let lopdf::Object::Stream(ms) = deref(v) {
                                        let mut md = ms.dict.clone();
                                        md.remove(b"Length");
                                        eprintln!("   {key}: {md:?}");
                                    } else if let lopdf::Object::Array(a) = deref(v) {
                                        eprintln!("   {key}: {a:?}");
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if let Some(fonts) = fonts {
                let deref = |o: &lopdf::Object| -> lopdf::Object {
                    match o {
                        lopdf::Object::Reference(id) => {
                            doc.get_object(*id).cloned().unwrap_or(lopdf::Object::Null)
                        }
                        o => o.clone(),
                    }
                };
                if let lopdf::Object::Dictionary(fd) = deref(&fonts) {
                    for (name, f) in fd.iter() {
                        let f = deref(f);
                        eprintln!("font /{}: {:?}", String::from_utf8_lossy(name), f);
                        if let lopdf::Object::Dictionary(d) = &f {
                            for key in ["FontDescriptor", "DescendantFonts", "Encoding"] {
                                if let Ok(v) = d.get(key.as_bytes()) {
                                    let v = deref(v);
                                    let v = match v {
                                        lopdf::Object::Array(a) => {
                                            a.first().map(deref).unwrap_or(lopdf::Object::Null)
                                        }
                                        v => v,
                                    };
                                    let short = format!("{v:?}");
                                    eprintln!("   {key}: {}", &short[..short.len().min(600)]);
                                    if let lopdf::Object::Dictionary(dd) = &v {
                                        for ff in ["FontFile", "FontFile2", "FontFile3"] {
                                            if let Ok(s) = dd.get(ff.as_bytes()) {
                                                if let lopdf::Object::Stream(st) = deref(s) {
                                                    let data = st
                                                        .decompressed_content()
                                                        .unwrap_or_default();
                                                    eprintln!(
                                                        "   {ff}: {:?} {} bytes, head {:?}",
                                                        st.dict.get(b"Subtype").ok(),
                                                        data.len(),
                                                        &data[..data.len().min(16)]
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
