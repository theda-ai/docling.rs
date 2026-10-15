// -*- C++ -*-
//
// A C ABI over docling-parse's Blend2D page renderer, for docling.rs (#478).
//
// docling 2.123+ renders every page image its model stages consume — the
// scale-1.0 layout input, the scale-2.0 TableFormer input, the OCR and
// enrichment crops — with docling-parse's own renderer
// (`src/render/blend2d_renderer.h`: FreeType glyph outlines filled by
// Blend2D), not with pdfium. docling.rs renders them with pdfium, exactly like
// docling's `PyPdfiumDocumentBackend`, and the two rasters differ at every
// glyph edge, which moves the layout model's borderline labels. This shim
// exposes docling-parse's renderer behind a handful of `extern "C"` functions
// so `crates/docling-pdf/src/dparse_render.rs` can `dlopen` it at runtime (the
// way pdfium is loaded) and feed the models the image docling feeds them.
//
// It reproduces `docling_threaded_renderer::worker_loop` — the code path
// behind `ThreadedDoclingParseDocumentBackend.get_page_image` — step for step:
// one warmed `blend2d_font_resolver` and one `blend2d_embedded_font_cache` per
// process, a `freetype_font_cache` + `glyph_bbox_cache` per document (the
// threaded renderer keeps one per worker thread), a thread-safe page decoder
// decoded once with `extract_font_programs` / `extract_bitmap_pixels` forced
// on and `bitmap_target_pixels_per_unit` set to the *first* requested scale
// (docling decodes at its `render_scale`, 1.0, and re-renders the same page
// decoder at 2.0 for TableFormer — `PageParseResult._render_image_at_scale`),
// then `renderer<BLEND2D>` over the page's instructions with
// `render_config.scale` set and the canvas dimensions left at -1.
//
// Built by `scripts/install/build_docling_parse_render.sh` inside docling-parse's
// own CMake tree (its `DEPENDENCIES` + `LIB_LINK`), never by cargo: the C++
// side is a measurement/parity instrument, not a build dependency.

#include "parse.h"
#include "render.h"

#include <cstdlib>
#include <cstring>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <unordered_map>

#ifndef DPR_DOCLING_PARSE_VERSION
#define DPR_DOCLING_PARSE_VERSION "unknown"
#endif

namespace
{
  // The ABI version: bump when a signature below changes. The Rust side
  // refuses a library whose `dpr_abi_version()` it does not know.
  // 2: dpr_render takes the bitmap decode hint separately from the scale.
  constexpr int DPR_ABI_VERSION = 2;

  std::mutex g_mutex; // serializes every call: one renderer state per process
  bool g_initialised = false;
  std::shared_ptr<pdflib::blend2d_font_resolver> g_font_resolver;
  std::shared_ptr<pdflib::blend2d_embedded_font_cache> g_embedded_font_cache;

  void set_err(char* err, int err_len, const std::string& msg)
  {
    if(err == nullptr or err_len <= 0)
      {
        return;
      }
    const std::size_t n = std::min<std::size_t>(msg.size(), static_cast<std::size_t>(err_len - 1));
    std::memcpy(err, msg.data(), n);
    err[n] = '\0';
  }

  bool ensure_initialised(const char* resources_dir, char* err, int err_len)
  {
    if(g_initialised)
      {
        return true;
      }
    // docling's `ThreadedPdfParserConfig.loglevel` defaults to "fatal".
    loguru::g_stderr_verbosity = loguru::Verbosity_FATAL;
    if(resources_dir != nullptr and *resources_dir != '\0')
      {
        if(not resource_utils::set_resources_dir(std::filesystem::path(resources_dir)))
          {
            set_err(err, err_len, std::string("docling-parse resources dir not found: ") + resources_dir);
            return false;
          }
      }
    const std::string dir = resource_utils::get_resources_dir(true).string();
    if(not std::filesystem::exists(dir))
      {
        set_err(err, err_len, "docling-parse resources dir not found: " + dir
                + " (pass it to dpr_init / DOCLING_PARSE_RESOURCES)");
        return false;
      }
    nlohmann::json data = nlohmann::json::object({});
    data[pdflib::pdf_resource<pdflib::PAGE_FONT>::RESOURCE_DIR_KEY] = dir;
    std::unordered_map<std::string, double> timings = {};
    pdflib::pdf_resource<pdflib::PAGE_FONT>::initialise(data, timings);

    g_font_resolver = std::make_shared<pdflib::blend2d_font_resolver>();
    g_font_resolver->warm();
    g_embedded_font_cache = std::make_shared<pdflib::blend2d_embedded_font_cache>();
    g_initialised = true;
    return true;
  }
}

struct dpr_doc
{
  pdflib::pdf_timings timings;
  std::unique_ptr<pdflib::pdf_decoder<pdflib::DOCUMENT>> decoder;
  // Per document, like the threaded renderer keeps them per worker: FT_Face
  // objects mutate while building an outline, so they are never shared.
  std::shared_ptr<pdflib::freetype_font_cache> freetype_cache;
  std::shared_ptr<pdflib::glyph_bbox_cache> glyph_bbox_cache;
  // Page decoders decoded so far, keyed by (0-based page index, bitmap decode
  // hint). docling decodes a page once, at its `render_scale` (1.0), and
  // renders that decoder at every later scale (2.0 for TableFormer, 3.0 for
  // OCR): `bitmap_target_pixels_per_unit` lets the JPEG/JPX decoders reduce
  // an oversampled scan to the hint's resolution, so docling's TableFormer and
  // OCR see a 1×-decoded raster upscaled. The caller chooses the hint per
  // render: docling's for the layout image, full resolution (0) for the
  // bitmap the OCR and TableFormer crops come from.
  std::unordered_map<std::string, std::shared_ptr<pdflib::pdf_decoder<pdflib::PAGE>>> pages;

  dpr_doc():
    timings(),
    decoder(std::make_unique<pdflib::pdf_decoder<pdflib::DOCUMENT>>(timings)),
    freetype_cache(std::make_shared<pdflib::freetype_font_cache>()),
    glyph_bbox_cache(std::make_shared<pdflib::glyph_bbox_cache>(pdflib::render_config().glyph_bbox_cache_capacity))
  {}
};

extern "C"
{
  int dpr_abi_version(void)
  {
    return DPR_ABI_VERSION;
  }

  const char* dpr_docling_parse_version(void)
  {
    return DPR_DOCLING_PARSE_VERSION;
  }

  // Initialise the font resources (docling-parse's `pdf_resources` directory:
  // the bundled fallback faces, encodings, cmaps). `resources_dir` may be NULL
  // to use the path compiled in. Idempotent. Returns 0 on success.
  int dpr_init(const char* resources_dir, char* err, int err_len)
  {
    std::lock_guard<std::mutex> lock(g_mutex);
    try
      {
        return ensure_initialised(resources_dir, err, err_len) ? 0 : 1;
      }
    catch(const std::exception& exc)
      {
        set_err(err, err_len, std::string("dpr_init: ") + exc.what());
        return 1;
      }
  }

  // Open a PDF from memory (`password` may be NULL). Returns NULL on failure.
  dpr_doc* dpr_open(const unsigned char* bytes, size_t len, const char* password, char* err, int err_len)
  {
    std::lock_guard<std::mutex> lock(g_mutex);
    try
      {
        if(not ensure_initialised(nullptr, err, err_len))
          {
            return nullptr;
          }
        auto doc = std::make_unique<dpr_doc>();
        auto buffer = std::make_shared<std::string>(reinterpret_cast<const char*>(bytes), len);
        std::optional<std::string> pw = std::nullopt;
        if(password != nullptr and *password != '\0')
          {
            pw = std::string(password);
          }
        if(not doc->decoder->process_document_from_bytesio(buffer, pw, "docling.rs buffer", false))
          {
            set_err(err, err_len, "docling-parse could not open the document");
            return nullptr;
          }
        return doc.release();
      }
    catch(const std::exception& exc)
      {
        set_err(err, err_len, std::string("dpr_open: ") + exc.what());
        return nullptr;
      }
  }

  int dpr_page_count(dpr_doc* doc)
  {
    std::lock_guard<std::mutex> lock(g_mutex);
    return doc == nullptr ? 0 : doc->decoder->get_number_of_pages();
  }

  // Render page `page_index` (0-based) at `scale` pixels per point into an
  // RGBA8 buffer, row-major, top to bottom, in display orientation (`/Rotate`
  // applied) — `renderer<BLEND2D>::get_canvas`. The canvas is
  // `ceil(width * scale)` × `ceil(height * scale)` of the crop box, as
  // docling-parse sizes it. `bitmap_hint` is the decoder's
  // `bitmap_target_pixels_per_unit`: docling passes its `render_scale`
  // (1.0), 0 decodes every embedded image at full resolution. A page is
  // decoded once per distinct hint and re-rendered at any scale after that.
  // The buffer is malloc'd; free it with `dpr_free`. Returns 0 on success.
  int dpr_render(dpr_doc* doc, int page_index, double scale, double bitmap_hint,
                 unsigned char** rgba, int* width, int* height,
                 char* err, int err_len)
  {
    std::lock_guard<std::mutex> lock(g_mutex);
    if(doc == nullptr or rgba == nullptr or width == nullptr or height == nullptr)
      {
        set_err(err, err_len, "dpr_render: null argument");
        return 1;
      }
    *rgba = nullptr;
    *width = 0;
    *height = 0;
    if(scale <= 0.0 or bitmap_hint < 0.0)
      {
        set_err(err, err_len, "dpr_render: scale must be > 0 and bitmap_hint >= 0");
        return 1;
      }
    try
      {
        const std::string key = std::to_string(page_index) + "@" + std::to_string(bitmap_hint);
        auto found = doc->pages.find(key);
        if(found == doc->pages.end())
          {
            // `_compile_decode_config` defaults (docling's `ContentConfig`
            // computes char/word/line cells) + what the threaded renderer
            // forces on; `do_thread_safe` stays on like docling's. The cell
            // passes are kept although the raster walks the page's
            // instructions: the decode builds the text instructions alongside
            // the cells, and the pixels are checked against the Python
            // package byte for byte with the Python configuration.
            pdflib::decode_config config;
            config.extract_font_programs = true;
            config.extract_bitmap_pixels = true;
            config.bitmap_target_pixels_per_unit = bitmap_hint;
            auto page = doc->decoder->make_thread_safe_page_decoder(page_index, config.keep_qpdf_warnings);
            if(not page)
              {
                set_err(err, err_len, "dpr_render: page " + std::to_string(page_index) + " could not be decoded");
                return 1;
              }
            page->decode_page(config);
            found = doc->pages.emplace(key, page).first;
          }
        auto& page = found->second;

        pdflib::render_config render_cfg;
        render_cfg.scale = static_cast<float>(scale);
        render_cfg.canvas_width = -1;
        render_cfg.canvas_height = -1;

        pdflib::renderer<pdflib::BLEND2D> rnd(render_cfg,
                                              g_font_resolver,
                                              g_embedded_font_cache,
                                              doc->freetype_cache,
                                              doc->glyph_bbox_cache);
        page->get_instructions().iterate_over_instructions(rnd);

        auto canvas = rnd.get_canvas();
        const auto& shape = rnd.get_shape(); // {height, width, channels}
        if(not canvas or canvas->empty() or shape[0] <= 0 or shape[1] <= 0 or shape[2] != 4)
          {
            set_err(err, err_len, "dpr_render: empty canvas for page " + std::to_string(page_index));
            return 1;
          }
        const std::size_t expected = static_cast<std::size_t>(shape[0]) * static_cast<std::size_t>(shape[1]) * 4;
        if(canvas->size() != expected)
          {
            set_err(err, err_len, "dpr_render: canvas size " + std::to_string(canvas->size())
                    + " does not match shape " + std::to_string(shape[1]) + "x" + std::to_string(shape[0]));
            return 1;
          }
        unsigned char* out = static_cast<unsigned char*>(std::malloc(expected));
        if(out == nullptr)
          {
            set_err(err, err_len, "dpr_render: out of memory");
            return 1;
          }
        std::memcpy(out, canvas->data(), expected);
        *rgba = out;
        *width = shape[1];
        *height = shape[0];
        return 0;
      }
    catch(const std::exception& exc)
      {
        set_err(err, err_len, std::string("dpr_render: ") + exc.what());
        return 1;
      }
  }

  // Drop the cached page decoders of `page_index` (call once every scale of a
  // page has been rendered; a 2,000-page document would otherwise keep every
  // decoded page alive until `dpr_close`).
  void dpr_release_page(dpr_doc* doc, int page_index)
  {
    std::lock_guard<std::mutex> lock(g_mutex);
    if(doc != nullptr)
      {
        const std::string prefix = std::to_string(page_index) + "@";
        for(auto it = doc->pages.begin(); it != doc->pages.end();)
          {
            if(it->first.compare(0, prefix.size(), prefix) == 0)
              {
                it = doc->pages.erase(it);
              }
            else
              {
                ++it;
              }
          }
      }
  }

  void dpr_free(unsigned char* p)
  {
    std::free(p);
  }

  void dpr_close(dpr_doc* doc)
  {
    std::lock_guard<std::mutex> lock(g_mutex);
    delete doc;
  }
}
