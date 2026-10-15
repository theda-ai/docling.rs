//! Page metadata straight from the PDF object model (lopdf): page count, the
//! display geometry pdfium reports, `/Rotate`, and the URI link annotations.
//!
//! The first step of retiring pdfium (#478's follow-up): everything the
//! pipeline used to ask pdfium for *besides* rasterizing — `FPDF_GetPageCount`,
//! `FPDF_GetPageWidthF/HeightF`, `FPDFPage_GetRotation`, `FPDFLink_Enumerate`
//! — is answered here, so a checkout with the docling-parse renderer plugin and
//! no `libpdfium` converts a PDF end to end, and pdfium is left with two
//! fallback roles: the text layer of a file the pure-Rust parser cannot read,
//! and the raster when no renderer plugin is present.
//!
//! Every answer matches pdfium's on the corpus (`pdfium_backend::tests` checks
//! geometry, rotation and links against the library when it is installed):
//! the page box is `textparse::page_box` (CropBox ∩ MediaBox with pdfium's
//! fallbacks), the rotation is pdfium's `(rotate / 90) % 4` normalization of
//! the inherited `/Rotate`, and a link is a `/Link` annotation whose `/A`
//! action is a `/URI` one.
//!
//! Pure Rust (lopdf), no feature gate — it compiles for the `pdf-text` (wasm)
//! build too, where it is what `convert_text_layer` could grow into.

use lopdf::{Document, Object, ObjectId};

use crate::pdfium_backend::LinkAnnot;

/// A page's display geometry, the way pdfium's `FPDF_GetPageWidthF/HeightF`
/// and `FPDFPage_GetRotation` report it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageGeom {
    /// Display-frame width in points (`/Rotate` applied: swapped with the
    /// height for 90° and 270°).
    pub width: f32,
    /// Display-frame height in points.
    pub height: f32,
    /// `/Rotate`, normalized to 0 / 90 / 180 / 270.
    pub rotation: u16,
}

impl PageGeom {
    /// The unrotated (content-frame) box size, `(width, height)`.
    pub fn unrotated(&self) -> (f32, f32) {
        if self.rotation == 90 || self.rotation == 270 {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }
}

/// The parsed document with its pages in document order.
pub struct PdfMeta {
    doc: Document,
    /// Page object ids, page 1 first.
    pages: Vec<ObjectId>,
}

impl PdfMeta {
    /// Load `bytes` (with the parser's xref/stream repairs); `None` when lopdf
    /// cannot read the file at all, or when it is encrypted beyond the empty
    /// user password.
    pub fn open(bytes: &[u8]) -> Option<Self> {
        Self::open_with_password(bytes, None).ok().flatten()
    }

    /// [`open`](Self::open) with the document's password. `Ok(None)`: lopdf
    /// cannot read the file (pdfium's turn, when compiled in); `Err`: the file
    /// is encrypted and the password is missing or wrong — the error docling
    /// raises too, instead of a document whose every stream decodes to nothing.
    pub fn open_with_password(
        bytes: &[u8],
        password: Option<&str>,
    ) -> Result<Option<Self>, crate::PdfError> {
        use crate::textparse::OpenError;
        use crate::EncryptionError;
        match crate::textparse::open_document(bytes, password) {
            Ok(doc) => {
                let mut pages: Vec<_> = doc.get_pages().into_iter().collect();
                pages.sort_by_key(|(n, _)| *n);
                Ok(Some(Self {
                    doc,
                    pages: pages.into_iter().map(|(_, pid)| pid).collect(),
                }))
            }
            Err(OpenError::Unreadable) => Ok(None),
            // Typed (#636): a caller tells "ask for a password" from "the
            // file is damaged" without reading the message.
            Err(OpenError::Password) => Err(crate::PdfError::Encrypted(if password.is_some() {
                EncryptionError::WrongPassword
            } else {
                EncryptionError::NeedPassword
            })),
        }
    }

    /// Number of pages (`FPDF_GetPageCount`).
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// The parsed document, for the other pure-Rust readers (`raster`).
    pub(crate) fn doc(&self) -> &Document {
        &self.doc
    }

    /// The page object id of the 0-based `index`.
    pub(crate) fn page_id(&self, index: usize) -> Option<ObjectId> {
        self.pages.get(index).copied()
    }

    /// Display geometry of the 0-based `index`; `None` past the last page.
    pub fn geometry(&self, index: usize) -> Option<PageGeom> {
        let pid = *self.pages.get(index)?;
        let pb = crate::textparse::page_box(&self.doc, pid);
        let rotation = self.rotation(pid);
        let (width, height) = if rotation == 90 || rotation == 270 {
            (pb.h, pb.w)
        } else {
            (pb.w, pb.h)
        };
        Some(PageGeom {
            width,
            height,
            rotation,
        })
    }

    /// The URI link annotations of the 0-based `index`, as top-left-origin
    /// rects in the page's *unrotated* content frame — the frame every text
    /// coordinate lives in — counted from the display box's corner like the
    /// glyphs are, restricted to the web/mail/tel schemes the Markdown export
    /// renders (`extract_links`'s rule). Empty past the last page.
    pub fn links(&self, index: usize) -> Vec<LinkAnnot> {
        let Some(&pid) = self.pages.get(index) else {
            return Vec::new();
        };
        let pb = crate::textparse::page_box(&self.doc, pid);
        let top = pb.top();
        let mut out = Vec::new();
        let Some(annots) = self
            .page_dict(pid)
            .and_then(|d| d.get(b"Annots").ok())
            .and_then(|o| self.deref(o))
            .and_then(|o| o.as_array().ok())
        else {
            return out;
        };
        for annot in annots {
            let Some(dict) = self.deref(annot).and_then(|o| o.as_dict().ok()) else {
                continue;
            };
            if !name_is(dict.get(b"Subtype").ok(), b"Link") {
                continue;
            }
            let Some(action) = dict
                .get(b"A")
                .ok()
                .and_then(|o| self.deref(o))
                .and_then(|o| o.as_dict().ok())
            else {
                continue;
            };
            if !name_is(action.get(b"S").ok(), b"URI") {
                continue;
            }
            let Some(uri) = action
                .get(b"URI")
                .ok()
                .and_then(|o| self.deref(o))
                .and_then(|o| o.as_str().ok())
                .map(|b| String::from_utf8_lossy(b).into_owned())
            else {
                continue;
            };
            if !["http://", "https://", "mailto:", "tel:"]
                .iter()
                .any(|s| uri.starts_with(s))
            {
                continue;
            }
            let Some(rect) = dict
                .get(b"Rect")
                .ok()
                .and_then(|o| self.deref(o))
                .and_then(|o| o.as_array().ok())
            else {
                continue;
            };
            let v: Vec<f32> = rect
                .iter()
                .filter_map(|o| self.deref(o).and_then(number).map(|x| x as f32))
                .collect();
            if v.len() != 4 || v.iter().any(|x| !x.is_finite()) {
                continue;
            }
            let (x0, y0, x1, y1) = (
                v[0].min(v[2]),
                v[1].min(v[3]),
                v[0].max(v[2]),
                v[1].max(v[3]),
            );
            out.push(LinkAnnot {
                l: x0 - pb.l,
                t: top - y1,
                r: x1 - pb.l,
                b: top - y0,
                uri,
            });
        }
        out
    }

    /// pdfium's `CPDF_Page::GetPageRotation`: the inherited `/Rotate`
    /// integer, `(rotate / 90) % 4` with a negative result wrapped, in degrees.
    fn rotation(&self, pid: ObjectId) -> u16 {
        let mut id = pid;
        for _ in 0..32 {
            let Some(dict) = self.page_dict(id) else {
                return 0;
            };
            if let Some(obj) = dict.get(b"Rotate").ok().and_then(|o| self.deref(o)) {
                let rotate = number(obj).unwrap_or(0.0) as i64;
                let mut quarter = (rotate / 90) % 4;
                if quarter < 0 {
                    quarter += 4;
                }
                return (quarter * 90) as u16;
            }
            match dict.get(b"Parent").ok().and_then(|o| o.as_reference().ok()) {
                Some(parent) => id = parent,
                None => return 0,
            }
        }
        0
    }

    fn page_dict(&self, id: ObjectId) -> Option<&lopdf::Dictionary> {
        self.doc.get_object(id).ok()?.as_dict().ok()
    }

    /// Follow one level of reference (a `/Rect` or `/A` is often indirect).
    fn deref<'a>(&'a self, obj: &'a Object) -> Option<&'a Object> {
        match obj {
            Object::Reference(id) => self.doc.get_object(*id).ok(),
            other => Some(other),
        }
    }
}

fn name_is(obj: Option<&Object>, name: &[u8]) -> bool {
    matches!(obj, Some(Object::Name(n)) if n == name)
}

fn number(obj: &Object) -> Option<f64> {
    match obj {
        Object::Integer(i) => Some(*i as f64),
        Object::Real(r) => Some(f64::from(*r)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(rel: &str) -> Vec<u8> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        std::fs::read(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
    }

    /// A password-protected fixture: no password is the error docling raises,
    /// the right one opens the object model and the text layer.
    #[test]
    fn password_protected_files_need_their_password() {
        let bytes = fixture("tests/data/pdf_password/sources/2206.01062_pg3.pdf");
        assert!(PdfMeta::open(&bytes).is_none());
        let err = PdfMeta::open_with_password(&bytes, None)
            .err()
            .expect("no password");
        assert_eq!(
            err.to_string(),
            "pdf: the PDF is encrypted: a password is required"
        );
        assert!(
            matches!(
                err,
                crate::PdfError::Encrypted(crate::EncryptionError::NeedPassword)
            ),
            "{err:?}"
        );
        let err = PdfMeta::open_with_password(&bytes, Some("nope"))
            .err()
            .expect("wrong password");
        assert_eq!(
            err.to_string(),
            "pdf: the PDF is encrypted and the password is wrong"
        );
        assert!(
            matches!(
                err,
                crate::PdfError::Encrypted(crate::EncryptionError::WrongPassword)
            ),
            "{err:?}"
        );
        // The typed value is on the source chain for callers that only see
        // `dyn Error` (#636).
        let chained = std::error::Error::source(&err)
            .and_then(|s| s.downcast_ref::<crate::EncryptionError>())
            .expect("source");
        assert_eq!(*chained, crate::EncryptionError::WrongPassword);
        let meta = PdfMeta::open_with_password(&bytes, Some("1234"))
            .unwrap()
            .expect("readable");
        assert_eq!(meta.page_count(), 1);
        let mut parser =
            crate::textparse::PageTextParser::open_with_password(&bytes, Some("1234")).unwrap();
        assert!(!parser.cells(0).prose.is_empty());
    }

    #[test]
    fn page_count_and_geometry_of_a_plain_paper() {
        let meta = PdfMeta::open(&fixture("tests/data/pdf/sources/2206.01062.pdf")).unwrap();
        assert_eq!(meta.page_count(), 9);
        let g = meta.geometry(0).unwrap();
        assert_eq!((g.width, g.height, g.rotation), (612.0, 792.0, 0));
        assert!(meta.geometry(9).is_none());
    }

    /// The `/Rotate` fixtures: display size swaps for 90/270 and the content
    /// frame stays the unrotated box.
    #[test]
    fn rotated_pages_report_the_display_frame() {
        for (rel, rot) in [
            ("tests/data/pdf/sources/base14_fonts_rot90.pdf", 90u16),
            ("tests/data/pdf/sources/base14_fonts_rot180.pdf", 180),
            ("tests/data/pdf/sources/base14_fonts_rot270.pdf", 270),
        ] {
            let meta = PdfMeta::open(&fixture(rel)).unwrap();
            let g = meta.geometry(0).unwrap();
            assert_eq!(g.rotation, rot, "{rel}");
            let plain = PdfMeta::open(&fixture("tests/data/pdf/sources/base14_fonts.pdf"))
                .unwrap()
                .geometry(0)
                .unwrap();
            assert_eq!(g.unrotated(), (plain.width, plain.height), "{rel}");
            if rot == 90 || rot == 270 {
                assert_eq!((g.width, g.height), (plain.height, plain.width), "{rel}");
            }
        }
    }

    /// arXiv papers carry hyperref `/Link` annotations: 2206's first page has
    /// three `/URI` actions (the DOI and the DocLayNet data links) and `/GoTo`
    /// ones, which are not hyperlinks for the Markdown and are left out like
    /// pdfium's `extract_links` leaves them out; 2305-pg9's links are all
    /// `/GoTo`.
    #[test]
    fn uri_links_are_read_from_the_annotations() {
        let meta = PdfMeta::open(&fixture("tests/data/pdf/sources/2206.01062.pdf")).unwrap();
        let links = meta.links(0);
        assert_eq!(links.len(), 3, "{links:?}");
        for l in &links {
            assert!(l.uri.starts_with("https://"), "{}", l.uri);
            assert!(l.l < l.r && l.t < l.b, "{l:?}");
            assert!(l.b <= meta.geometry(0).unwrap().height + 1.0, "{l:?}");
        }
        let pg9 = PdfMeta::open(&fixture("tests/data/pdf/sources/2305.03393v1-pg9.pdf")).unwrap();
        assert!(pg9.links(0).is_empty());
        assert!(meta.links(99).is_empty());
    }
}
