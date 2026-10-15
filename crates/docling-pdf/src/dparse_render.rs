//! Opt-in page-image plugin: docling-parse's Blend2D renderer, loaded at
//! runtime (#478).
//!
//! docling 2.123+ renders every page image its model stages consume — the
//! scale-1.0 layout input, the scale-2.0 TableFormer input, the OCR and
//! enrichment crops — with docling-parse's own renderer (FreeType glyph
//! outlines filled by Blend2D), while this pipeline renders them with its own
//! pure-Rust renderer (`crate::render`, tiny-skia) — close to, not identical
//! with, that canvas (mean |Δ| ≈ 1/255 over the corpus), and heron's
//! borderline labels follow the pixels. This module lets the pipeline consume
//! docling's raster itself: a small C shim over `renderer<BLEND2D>`
//! (`crates/docling-pdf/ffi/docling-parse-render/dparse_render.cpp`, built by
//! `scripts/install/build_docling_parse_render.sh`) is `dlopen`ed, and
//! [`Doc::render`] hands back the RGBA canvas docling's `get_page_image(scale)`
//! would. Since phase 5 of "Retiring pdfium" it is a development oracle: the
//! conformance scripts ask for it by name, the default pipeline never loads
//! it.
//!
//! Selection is an environment knob, never a build feature: nothing links the
//! C++ side, CI and wasm are untouched, and a missing library degrades to
//! the Rust renderer.
//!
//! * `DOCLING_RS_RENDERER` — `auto` (default: the pure-Rust renderer,
//!   `crate::render`; the shim is never opened), `docling-parse` (the shim —
//!   what the conformance scripts run, because the baselines in
//!   `tests/snapshots` and `docs/PDF_CONFORMANCE.md` are its renders; a
//!   missing library warns once and falls back to the Rust renderer), `rust`
//!   (the default, spelled out) or `pdfium` (the library's render, docling's
//!   pypdfium2 chain — only in a build with docling-pdf's `pdfium` feature,
//!   otherwise a one-time warning and the Rust renderer).
//! * `DOCLING_PARSE_RENDER_LIB` — the shim library (a file, or the directory
//!   holding `libdparse_render.so`/`.dylib`); default `.docling-parse/lib`
//!   resolved like `.models` ([`crate::resolve_asset`]).
//! * `DOCLING_PARSE_RESOURCES` — docling-parse's `pdf_resources` directory
//!   (fallback fonts, encodings, cmaps); default `<lib dir>/../pdf_resources`,
//!   where the build script installs it.
//!
//! The renderer is docling-parse's, so its output is compared against the
//! Python package's `PageParseResult.get_image(scale)` byte for byte
//! (`scripts/conformance/dparse_render_check.py`); everything downstream —
//! the Pillow-exact 640 stretch, the TableFormer crop chain — is unchanged.

use std::ffi::{c_char, c_double, c_int, c_uchar, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use image::RgbImage;
use libloading::{Library, Symbol};

/// The C ABI the shim exports; bump together with `DPR_ABI_VERSION` there.
/// 2: `dpr_render` takes the bitmap decode hint separately from the scale.
const ABI_VERSION: c_int = 2;

type AbiVersionFn = unsafe extern "C" fn() -> c_int;
type VersionFn = unsafe extern "C" fn() -> *const c_char;
type InitFn = unsafe extern "C" fn(*const c_char, *mut c_char, c_int) -> c_int;
type OpenFn =
    unsafe extern "C" fn(*const c_uchar, usize, *const c_char, *mut c_char, c_int) -> *mut c_void;
type PageCountFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type RenderFn = unsafe extern "C" fn(
    *mut c_void,
    c_int,
    c_double,
    c_double,
    *mut *mut c_uchar,
    *mut c_int,
    *mut c_int,
    *mut c_char,
    c_int,
) -> c_int;
type ReleasePageFn = unsafe extern "C" fn(*mut c_void, c_int);
type FreeFn = unsafe extern "C" fn(*mut c_uchar);
type CloseFn = unsafe extern "C" fn(*mut c_void);

/// The loaded shim: the library plus its resolved entry points. One per
/// process, behind [`plugin`].
pub struct Plugin {
    // Declared first so the symbols below are dropped before the library.
    open: Symbol<'static, OpenFn>,
    page_count: Symbol<'static, PageCountFn>,
    render: Symbol<'static, RenderFn>,
    release_page: Symbol<'static, ReleasePageFn>,
    free: Symbol<'static, FreeFn>,
    close: Symbol<'static, CloseFn>,
    /// docling-parse's version the shim was built against (`DPR_VERSION`).
    pub docling_parse_version: String,
    /// Where the library was loaded from.
    pub path: PathBuf,
    _lib: &'static Library,
}

const ERR_LEN: usize = 1024;

fn err_string(buf: &[c_char]) -> String {
    // The shim always NUL-terminates within the buffer.
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// What `DOCLING_RS_RENDERER` asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Choice {
    /// The pure-Rust renderer ([`crate::render`]) — the default. The shim is
    /// a development oracle since phase 5 of "Retiring pdfium": it is loaded
    /// only when asked for by name.
    Auto,
    /// docling-parse's renderer (the shim), warning when it is unavailable —
    /// what the conformance scripts run, the renderer the baselines are
    /// pinned to.
    DoclingParse,
    /// The pure-Rust renderer, spelled out.
    Rust,
    /// pdfium (the renderer of docling's pypdfium2 backend); only offered by
    /// a build with the `pdfium` feature.
    Pdfium,
}

/// The renderer `DOCLING_RS_RENDERER` selects; unset, empty or `auto` is
/// [`Choice::Auto`], an unknown value warns once and counts as `auto`.
pub fn choice() -> Choice {
    match docling_core::env::nonempty("DOCLING_RS_RENDERER") {
        None => Choice::Auto,
        Some(v) => match v.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => Choice::Auto,
            "docling-parse" | "docling_parse" | "dparse" => Choice::DoclingParse,
            "rust" => Choice::Rust,
            #[cfg(feature = "pdfium")]
            "pdfium" => Choice::Pdfium,
            #[cfg(not(feature = "pdfium"))]
            "pdfium" => {
                static WARNED: std::sync::Once = std::sync::Once::new();
                WARNED.call_once(|| {
                    eprintln!(
                        "docling-pdf: DOCLING_RS_RENDERER=pdfium but pdfium support is not compiled in \
                         (docling-pdf feature `pdfium`); rendering with the Rust renderer"
                    );
                });
                Choice::Auto
            }
            other => {
                eprintln!(
                    "docling-pdf: unknown DOCLING_RS_RENDERER={other:?} (auto | docling-parse | rust | pdfium); using auto"
                );
                Choice::Auto
            }
        },
    }
}

/// Is the docling-parse renderer explicitly requested?
pub fn requested() -> bool {
    choice() == Choice::DoclingParse
}

fn platform_lib_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "libdparse_render.dylib"
    } else if cfg!(target_os = "windows") {
        "dparse_render.dll"
    } else {
        "libdparse_render.so"
    }
}

/// The shim library path: `DOCLING_PARSE_RENDER_LIB` (file or directory),
/// else `.docling-parse/lib/<platform name>` resolved like the other assets.
fn lib_path() -> PathBuf {
    let candidate = docling_core::env::nonempty("DOCLING_PARSE_RENDER_LIB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(crate::resolve_asset(".docling-parse/lib")));
    if candidate.is_dir() {
        candidate.join(platform_lib_name())
    } else {
        candidate
    }
}

/// docling-parse's `pdf_resources`: `DOCLING_PARSE_RESOURCES`, else the
/// `pdf_resources` sibling of the library's `lib/` directory.
fn resources_dir(lib: &Path) -> Option<PathBuf> {
    if let Some(dir) = docling_core::env::nonempty("DOCLING_PARSE_RESOURCES") {
        return Some(PathBuf::from(dir));
    }
    let sibling = lib.parent()?.parent()?.join("pdf_resources");
    sibling.is_dir().then_some(sibling)
}

fn load() -> Result<Plugin, String> {
    let path = lib_path();
    // SAFETY: loading a library runs its initializers; the shim's are the C++
    // runtime's and docling-parse's static state, which the shim guards.
    let lib = unsafe { Library::new(&path) }
        .map_err(|e| format!("cannot load {}: {e}", path.display()))?;
    let lib: &'static Library = Box::leak(Box::new(lib));
    // SAFETY: every symbol is declared with the shim's exact C signature.
    unsafe {
        let abi: Symbol<AbiVersionFn> = lib
            .get(b"dpr_abi_version\0")
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let got = abi();
        if got != ABI_VERSION {
            return Err(format!(
                "{}: shim ABI {got}, this build expects {ABI_VERSION} — rebuild it with \
                 scripts/install/build_docling_parse_render.sh",
                path.display()
            ));
        }
        let version: Symbol<VersionFn> = lib
            .get(b"dpr_docling_parse_version\0")
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let docling_parse_version = CStr::from_ptr(version()).to_string_lossy().into_owned();
        let init: Symbol<InitFn> = lib
            .get(b"dpr_init\0")
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let resources = resources_dir(&path)
            .map(|p| CString::new(p.to_string_lossy().into_owned()).unwrap_or_default());
        let mut err = [0 as c_char; ERR_LEN];
        let rc = init(
            resources
                .as_ref()
                .map(|c| c.as_ptr())
                .unwrap_or(std::ptr::null()),
            err.as_mut_ptr(),
            ERR_LEN as c_int,
        );
        if rc != 0 {
            return Err(format!("dpr_init: {}", err_string(&err)));
        }
        Ok(Plugin {
            open: lib.get(b"dpr_open\0").map_err(|e| e.to_string())?,
            page_count: lib.get(b"dpr_page_count\0").map_err(|e| e.to_string())?,
            render: lib.get(b"dpr_render\0").map_err(|e| e.to_string())?,
            release_page: lib.get(b"dpr_release_page\0").map_err(|e| e.to_string())?,
            free: lib.get(b"dpr_free\0").map_err(|e| e.to_string())?,
            close: lib.get(b"dpr_close\0").map_err(|e| e.to_string())?,
            docling_parse_version,
            path,
            _lib: lib,
        })
    }
}

/// The plugin when `DOCLING_RS_RENDERER=docling-parse` asks for it and the
/// shim loads; `None` otherwise — under `auto` the shim is never opened
/// (the Rust renderer is the default), and an unavailable library under
/// `docling-parse` warns once and falls back to the Rust renderer
/// (degradation over failure).
pub fn plugin() -> Option<&'static Plugin> {
    static PLUGIN: OnceLock<Option<Plugin>> = OnceLock::new();
    PLUGIN
        .get_or_init(|| {
            let choice = choice();
            if choice != Choice::DoclingParse {
                return None;
            }
            match load() {
                Ok(p) => {
                    docling_core::debug_log!(
                        "docling-pdf: rendering page images with docling-parse {} ({})",
                        p.docling_parse_version,
                        p.path.display()
                    );
                    Some(p)
                }
                Err(e) if choice == Choice::DoclingParse => {
                    eprintln!(
                        "docling-pdf: DOCLING_RS_RENDERER=docling-parse but the renderer plugin \
                         is unavailable ({e}); rendering with the Rust renderer"
                    );
                    None
                }
                Err(e) => {
                    // Unreachable while only `docling-parse` opens the shim;
                    // kept for the day `auto` asks again.
                    docling_core::debug_log!(
                        "docling-pdf: docling-parse renderer plugin not loaded ({e}); rendering with the Rust renderer"
                    );
                    None
                }
            }
        })
        .as_ref()
}

/// Which renderer produces the model inputs in this process:
/// `"docling-parse"`, `"rust"` or `"pdfium"` — for diagnostics
/// (`--version`-style banners, serve health).
pub fn active_name() -> &'static str {
    if plugin().is_some() {
        "docling-parse"
    } else if choice() == Choice::Pdfium {
        "pdfium"
    } else {
        "rust"
    }
}

/// A PDF opened by docling-parse; renders its pages on request.
pub struct Doc {
    plugin: &'static Plugin,
    handle: *mut c_void,
}

// SAFETY: the shim serializes every call behind one mutex and the handle is
// only ever used through it; the pipeline renders on one thread anyway.
unsafe impl Send for Doc {}

impl Doc {
    /// Open `bytes` with the plugin when it is active; `None` when the pipeline
    /// should render with pdfium. An open failure warns and returns `None` too,
    /// so a document docling-parse rejects still converts.
    pub fn open_if_enabled(bytes: &[u8], password: Option<&str>) -> Option<Doc> {
        let plugin = plugin()?;
        match Doc::open(plugin, bytes, password) {
            Ok(doc) => Some(doc),
            Err(e) => {
                eprintln!("docling-pdf: docling-parse could not open the document ({e}); rendering with the Rust renderer");
                None
            }
        }
    }

    fn open(plugin: &'static Plugin, bytes: &[u8], password: Option<&str>) -> Result<Doc, String> {
        let password = password
            .map(|p| CString::new(p).map_err(|e| e.to_string()))
            .transpose()?;
        let mut err = [0 as c_char; ERR_LEN];
        // SAFETY: the byte slice outlives the call (the shim copies it), the
        // error buffer is NUL-terminated by the shim.
        let handle = unsafe {
            (plugin.open)(
                bytes.as_ptr(),
                bytes.len(),
                password
                    .as_ref()
                    .map(|c| c.as_ptr())
                    .unwrap_or(std::ptr::null()),
                err.as_mut_ptr(),
                ERR_LEN as c_int,
            )
        };
        if handle.is_null() {
            return Err(err_string(&err));
        }
        Ok(Doc { plugin, handle })
    }

    /// Number of pages docling-parse sees.
    pub fn page_count(&self) -> usize {
        // SAFETY: a live handle from `dpr_open`.
        unsafe { (self.plugin.page_count)(self.handle) }.max(0) as usize
    }

    /// Render the 0-based `page` at `scale` pixels per point: docling-parse's
    /// `get_image(scale)` canvas — `ceil(w·scale)` × `ceil(h·scale)`, display
    /// orientation — with its opaque white background, so the alpha plane is
    /// dropped and the RGB triples are returned as they are.
    ///
    /// `bitmap_hint` is docling-parse's `bitmap_target_pixels_per_unit`, the
    /// resolution the JPEG/JPX decoders may reduce an oversampled embedded
    /// image to (docling passes its `render_scale`, 1.0; `0.0` decodes at
    /// full resolution). A page is decoded once per distinct hint.
    pub fn render(&self, page: usize, scale: f64, bitmap_hint: f64) -> Result<RgbImage, String> {
        let mut rgba: *mut c_uchar = std::ptr::null_mut();
        let (mut w, mut h) = (0 as c_int, 0 as c_int);
        let mut err = [0 as c_char; ERR_LEN];
        // SAFETY: a live handle; the out-pointers are valid for the call and
        // the returned buffer is `w * h * 4` bytes owned by us until `dpr_free`.
        let rc = unsafe {
            (self.plugin.render)(
                self.handle,
                page as c_int,
                scale,
                bitmap_hint,
                &mut rgba,
                &mut w,
                &mut h,
                err.as_mut_ptr(),
                ERR_LEN as c_int,
            )
        };
        if rc != 0 || rgba.is_null() || w <= 0 || h <= 0 {
            return Err(err_string(&err));
        }
        let (w, h) = (w as u32, h as u32);
        let n = (w as usize) * (h as usize);
        // SAFETY: the shim wrote exactly n * 4 bytes.
        let src = unsafe { std::slice::from_raw_parts(rgba, n * 4) };
        let mut rgb = Vec::with_capacity(n * 3);
        for px in src.chunks_exact(4) {
            rgb.extend_from_slice(&px[..3]);
        }
        // SAFETY: the buffer came from `dpr_render`.
        unsafe { (self.plugin.free)(rgba) };
        RgbImage::from_raw(w, h, rgb)
            .ok_or_else(|| "docling-parse canvas size mismatch".to_string())
    }

    /// Drop the decoded state of `page` once every scale of it was rendered.
    pub fn release_page(&self, page: usize) {
        // SAFETY: a live handle.
        unsafe { (self.plugin.release_page)(self.handle, page as c_int) }
    }
}

impl Drop for Doc {
    fn drop(&mut self) {
        // SAFETY: closes the handle exactly once.
        unsafe { (self.plugin.close)(self.handle) }
    }
}
