#!/usr/bin/env bash
# Build docling-parse's Blend2D page renderer as a small shared library
# docling.rs can load at runtime (#478) — the opt-in "render the model inputs
# the way docling 2.123+ does" plugin.
#
# What it does:
#   1. clones docling-project/docling-parse at $DOCLING_PARSE_TAG (default: the
#      version this shim was written against) into $BUILD_DIR,
#   2. builds its C++ tree the way the project builds itself (CMake fetches and
#      compiles qpdf, FreeType, Blend2D + asmjit, libjpeg, OpenJPEG, lcms2 and
#      pdfium's JBIG2 decoder from source — expect 20–60 minutes on 4 cores),
#      plus one extra target: crates/docling-pdf/ffi/docling-parse-render/
#      dparse_render.cpp, a C ABI over `renderer<BLEND2D>`,
#   3. installs into $OUT_DIR (default `.docling-parse/` at the repo root, next
#      to `.models/`):
#        .docling-parse/lib/libdparse_render.so     (.dylib on macOS)
#        .docling-parse/pdf_resources/              (fallback fonts, encodings, cmaps)
#
# Then run with
#   DOCLING_RS_RENDERER=docling-parse docling-rs file.pdf
# (or set DOCLING_PARSE_RENDER_LIB / DOCLING_PARSE_RESOURCES explicitly — see
# crates/docling-pdf/src/dparse_render.rs). Nothing in the normal build or CI
# depends on this: the pipeline renders in pure Rust by default and loads the
# library only when asked for by name — it is the development oracle the PDF
# conformance scripts run against (the baselines are its renders).
#
# Requirements: git, cmake ≥ 3.20, a C++20 compiler, make, python3 (pybind11
# is installed into a throwaway venv because docling-parse's CMakeLists
# requires it, even though the Python module itself is not built here).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
DOCLING_PARSE_TAG="${DOCLING_PARSE_TAG:-v7.22.1}"
BUILD_DIR="${BUILD_DIR:-$REPO_ROOT/target/docling-parse-src}"
OUT_DIR="${OUT_DIR:-$REPO_ROOT/.docling-parse}"
JOBS="${JOBS:-$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)}"
SHIM="$REPO_ROOT/crates/docling-pdf/ffi/docling-parse-render/dparse_render.cpp"

case "$(uname -s)" in
  Darwin) LIB_NAME=libdparse_render.dylib ;;
  *) LIB_NAME=libdparse_render.so ;;
esac

if [ ! -d "$BUILD_DIR/.git" ]; then
  echo "cloning docling-parse $DOCLING_PARSE_TAG into $BUILD_DIR"
  git clone --depth 1 --branch "$DOCLING_PARSE_TAG" https://github.com/docling-project/docling-parse "$BUILD_DIR"
else
  echo "reusing docling-parse checkout in $BUILD_DIR ($(git -C "$BUILD_DIR" describe --tags --always))"
fi

# docling-parse's CMakeLists.txt requires pybind11 (for its Python module,
# which we do not build). A throwaway venv provides the CMake config.
if [ ! -x "$BUILD_DIR/.venv/bin/python" ]; then
  python3 -m venv "$BUILD_DIR/.venv"
  "$BUILD_DIR/.venv/bin/pip" install --quiet pybind11
fi
PYBIND11_DIR="$("$BUILD_DIR/.venv/bin/python" -m pybind11 --cmakedir)"

# Add the shim target to docling-parse's own CMake tree, so it compiles and
# links exactly like `render.exe`: same include dirs, same dependency libs.
MARKER="# --- docling.rs: dparse_render shim ---"
if ! grep -q "$MARKER" "$BUILD_DIR/CMakeLists.txt"; then
  cat >> "$BUILD_DIR/CMakeLists.txt" <<'EOF'

# --- docling.rs: dparse_render shim ---
# Appended by docling.rs/scripts/install/build_docling_parse_render.sh.
add_library(dparse_render SHARED "${DPR_SHIM_SOURCE}")
set_property(TARGET dparse_render PROPERTY CXX_STANDARD 20)
set_target_properties(dparse_render PROPERTIES POSITION_INDEPENDENT_CODE ON)
add_dependencies(dparse_render ${DEPENDENCIES})
target_include_directories(dparse_render PRIVATE ${TOPLEVEL_PREFIX_PATH}/src)
target_compile_definitions(dparse_render PRIVATE DPR_DOCLING_PARSE_VERSION="${DPR_VERSION}")
target_link_libraries(dparse_render ${DEPENDENCIES} ${LIB_LINK})
EOF
fi

cmake -S "$BUILD_DIR" -B "$BUILD_DIR/build" \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
  -Dpybind11_DIR="$PYBIND11_DIR" \
  -DPython_EXECUTABLE="$BUILD_DIR/.venv/bin/python" \
  -DPYTHON_EXECUTABLE="$BUILD_DIR/.venv/bin/python" \
  -DDPR_SHIM_SOURCE="$SHIM" \
  -DDPR_VERSION="$DOCLING_PARSE_TAG"
cmake --build "$BUILD_DIR/build" --target dparse_render -j "$JOBS"

mkdir -p "$OUT_DIR/lib"
LIB="$(find "$BUILD_DIR/build" -maxdepth 2 -name "$LIB_NAME" | head -n1)"
if [ -z "$LIB" ]; then
  echo "build finished but $LIB_NAME was not produced" >&2
  exit 1
fi
cp "$LIB" "$OUT_DIR/lib/$LIB_NAME"
rm -rf "$OUT_DIR/pdf_resources"
cp -R "$BUILD_DIR/docling_parse/pdf_resources" "$OUT_DIR/pdf_resources"
echo "installed $OUT_DIR/lib/$LIB_NAME and $OUT_DIR/pdf_resources (docling-parse $DOCLING_PARSE_TAG)"
echo "run: DOCLING_RS_RENDERER=docling-parse docling-rs <file.pdf>"
