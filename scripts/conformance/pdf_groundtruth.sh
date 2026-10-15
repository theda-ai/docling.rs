#!/usr/bin/env bash
#
# Per-PDF byte-conformance of the Rust pipeline vs the committed docling
# groundtruth (tests/data/pdf/groundtruth/*.md). Unlike conformance.sh this needs
# no docling install — it diffs against the checked-in reference. Use it to track
# how many groundtruth PDFs are byte-for-byte exact (see docs/PDF_CONFORMANCE.md).
#
# Usage: scripts/conformance/pdf_groundtruth.sh

set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/../.."

# Pin the snapshot-baseline pixel path: the scalar image-crate resize (the
# committed snapshots were generated with it; the SIMD default differs by
# ±1/255 per pixel, enough to flip borderline table cells).
export DOCLING_RS_SLOW_RESIZE="${DOCLING_RS_SLOW_RESIZE:-1}"
export DOCLING_LAYOUT_ONNX="${DOCLING_LAYOUT_ONNX:-$(pwd)/.models/layout_heron.onnx}"
export DOCLING_OCR_REC_ONNX="${DOCLING_OCR_REC_ONNX:-$(pwd)/.models/ocr_rec.onnx}"
export DOCLING_OCR_DICT="${DOCLING_OCR_DICT:-$(pwd)/.models/ppocr_keys_v1.txt}"
# The text detector (#429) is part of the OCR baseline: without it bitmap
# pages lose every line the layout model gave no region, and the image /
# figure snapshots change.
export DOCLING_OCR_DET_ONNX="${DOCLING_OCR_DET_ONNX:-$(pwd)/.models/ocr_det.onnx}"
# Optional: falls back to geometric table reconstruction if missing.
export DOCLING_TABLEFORMER_ENCODER="${DOCLING_TABLEFORMER_ENCODER:-$(pwd)/.models/tableformer/encoder.onnx}"
export DOCLING_TABLEFORMER_DECODER="${DOCLING_TABLEFORMER_DECODER:-$(pwd)/.models/tableformer/decoder.onnx}"
export DOCLING_TABLEFORMER_BBOX="${DOCLING_TABLEFORMER_BBOX:-$(pwd)/.models/tableformer/bbox.onnx}"
# The model inputs are rendered by docling-parse's renderer (#478), like the
# docling that wrote the groundtruth (its default backend since 2.123). The
# baselines are that renderer's, so its absence is an error here, not the
# quiet Rust-renderer fallback the `auto` default gives a plain checkout; run
# with DOCLING_RS_RENDERER=rust (or pdfium) to score that path against the
# same files.
export DOCLING_RS_RENDERER="${DOCLING_RS_RENDERER:-docling-parse}"
export DOCLING_PARSE_RENDER_LIB="${DOCLING_PARSE_RENDER_LIB:-$(pwd)/.docling-parse/lib}"
if [ "$DOCLING_RS_RENDERER" = docling-parse ]; then
  for f in "$DOCLING_PARSE_RENDER_LIB"/libdparse_render.*; do
    [ -e "$f" ] || { echo "MISSING: $DOCLING_PARSE_RENDER_LIB/libdparse_render.so  (run scripts/install/build_docling_parse_render.sh, or DOCLING_RS_RENDERER=rust)"; exit 1; }
  done
fi

cargo build --release --quiet -p docling-cli
BIN=./target/release/docling-rs
# Upstream generates tests/data/pdf/groundtruth with `do_ocr=False`
# (tests/test_e2e_conversion.py: layout + TableFormer with cell matching, no
# OCR) and serializes it with `export_to_markdown(compact_tables=True)`
# (tests/verify_utils.py), so the pipeline runs the same way — layout and
# tables, never OCR, unpadded `| - |` tables.
BIN_ARGS=(--skip-ocr --compact-tables)

# Collapse whitespace runs to a single space (and trim) so a spacing-only diff —
# e.g. docling's spurious double space in amt's `up to  1 / 4`, where our
# single-spaced rendering is the more faithful one — counts as a normalized match.
norm() { sed -E 's/[[:space:]]+/ /g; s/^ +//; s/ +$//'; }

exact=0
nmatch=0
total=0
printf "%-34s %14s\n" "PDF" "DIFF-LINES"
printf "%-34s %14s\n" "---" "----------"
for gt in tests/data/pdf/groundtruth/*.md; do
  stem="$(basename "$gt" .md)"
  src="tests/data/pdf/sources/$stem.pdf"
  [[ -f "$src" ]] || continue
  total=$((total + 1))
  out="$("$BIN" "${BIN_ARGS[@]}" "$src" 2>/dev/null || echo '<ERROR>')"
  # Strict comparison, trailing-newline-insensitive; one changed line counts as 2.
  d="$(diff <(printf '%s' "$out") <(printf '%s' "$(cat "$gt")") | grep -cE '^[<>]' || true)"
  # Whitespace-normalized comparison.
  dn="$(diff <(printf '%s' "$out" | norm) <(printf '%s' "$(cat "$gt")" | norm) | grep -cE '^[<>]' || true)"
  if [[ "$d" -eq 0 ]]; then
    exact=$((exact + 1))
    nmatch=$((nmatch + 1))
    mark="EXACT"
  elif [[ "$dn" -eq 0 ]]; then
    nmatch=$((nmatch + 1))
    mark="$d (ws-ok)"
  else
    mark="$d"
  fi
  printf "%-34s %14s\n" "$stem" "$mark"
done
echo
echo "Fully conformant (strict):     $exact / $total"
echo "Whitespace-normalized matches: $nmatch / $total"
