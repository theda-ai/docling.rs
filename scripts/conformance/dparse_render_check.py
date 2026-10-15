#!/usr/bin/env python3
"""Check the docling-parse renderer shim against the Python package, byte for byte.

The shim (`crates/docling-pdf/ffi/docling-parse-render/dparse_render.cpp`,
built by `scripts/install/build_docling_parse_render.sh`) claims to hand
docling.rs the very canvas docling 2.123+'s `ThreadedDoclingParseDocumentBackend`
hands its models. This script loads the shared library through ctypes, renders
every page of the given PDFs at the scales the pipeline asks for (1.0 for the
layout input, 2.0 for the TableFormer/OCR bitmap) and compares each canvas with
`docling_parse`'s own `PageParseResult.get_image(scale)` — the same
docling-parse version must be installed in the Python environment
(`pip install docling-parse==<tag>`).

    python3 scripts/conformance/dparse_render_check.py tests/data/pdf/sources/*.pdf
    # options: --lib .docling-parse/lib/libdparse_render.so
    #          --resources .docling-parse/pdf_resources
    #          --scales 1.0,2.0

Exit status 0 when every page at every scale is byte-identical.
"""

from __future__ import annotations

import argparse
import ctypes
import os
import sys
from pathlib import Path

import numpy as np
from PIL import Image

from docling_parse.pdf_parser import (
    DoclingThreadedPdfParser,
    RenderConfig,
    ThreadedPdfParserConfig,
)


def load_shim(lib_path: Path, resources: Path | None) -> ctypes.CDLL:
    lib = ctypes.CDLL(str(lib_path))
    lib.dpr_abi_version.restype = ctypes.c_int
    lib.dpr_docling_parse_version.restype = ctypes.c_char_p
    lib.dpr_init.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_int]
    lib.dpr_init.restype = ctypes.c_int
    lib.dpr_open.argtypes = [
        ctypes.c_char_p,
        ctypes.c_size_t,
        ctypes.c_char_p,
        ctypes.c_char_p,
        ctypes.c_int,
    ]
    lib.dpr_open.restype = ctypes.c_void_p
    lib.dpr_page_count.argtypes = [ctypes.c_void_p]
    lib.dpr_page_count.restype = ctypes.c_int
    lib.dpr_render.argtypes = [
        ctypes.c_void_p,
        ctypes.c_int,
        ctypes.c_double,
        ctypes.c_double,
        ctypes.POINTER(ctypes.POINTER(ctypes.c_ubyte)),
        ctypes.POINTER(ctypes.c_int),
        ctypes.POINTER(ctypes.c_int),
        ctypes.c_char_p,
        ctypes.c_int,
    ]
    lib.dpr_render.restype = ctypes.c_int
    lib.dpr_release_page.argtypes = [ctypes.c_void_p, ctypes.c_int]
    lib.dpr_free.argtypes = [ctypes.POINTER(ctypes.c_ubyte)]
    lib.dpr_close.argtypes = [ctypes.c_void_p]
    err = ctypes.create_string_buffer(1024)
    rc = lib.dpr_init(str(resources).encode() if resources else None, err, 1024)
    if rc != 0:
        sys.exit(f"dpr_init failed: {err.value.decode(errors='replace')}")
    return lib


def shim_render(lib: ctypes.CDLL, doc: int, page: int, scale: float, bitmap_hint: float) -> np.ndarray:
    """docling decodes the page at its render_scale (the first scale) and re-renders
    it at the others, so the hint is the first scale for every render — the same
    decoder state `PageParseResult.get_image(scale)` draws from."""
    buf = ctypes.POINTER(ctypes.c_ubyte)()
    w = ctypes.c_int()
    h = ctypes.c_int()
    err = ctypes.create_string_buffer(1024)
    rc = lib.dpr_render(
        doc, page, scale, bitmap_hint, ctypes.byref(buf), ctypes.byref(w), ctypes.byref(h), err, 1024
    )
    if rc != 0:
        raise RuntimeError(err.value.decode(errors="replace"))
    n = w.value * h.value * 4
    arr = np.ctypeslib.as_array(buf, shape=(n,)).reshape(h.value, w.value, 4).copy()
    lib.dpr_free(buf)
    return arr


def python_renders(path: Path, scales: list[float]) -> dict[tuple[int, float], np.ndarray]:
    """docling's path: decode with render_scale = scales[0], re-render for the rest."""
    rc = RenderConfig()
    rc.scale = scales[0]
    parser = DoclingThreadedPdfParser(parser_config=ThreadedPdfParserConfig(threads=1, render_config=rc))
    parser.load(str(path))
    out: dict[tuple[int, float], np.ndarray] = {}
    for res in parser.iterate_results():
        page = res.page_number - 1  # 0-based
        for s in scales:
            img = res.get_image(scale=s)
            out[(page, s)] = np.asarray(img.convert("RGBA"))
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("pdfs", nargs="+", type=Path)
    ap.add_argument("--lib", type=Path, default=Path(".docling-parse/lib/libdparse_render.so"))
    ap.add_argument("--resources", type=Path, default=None)
    ap.add_argument("--scales", default="1.0,2.0")
    ap.add_argument("--dump", type=Path, default=None, help="write mismatching pages as PNG pairs here")
    args = ap.parse_args()
    scales = [float(s) for s in args.scales.split(",")]
    resources = args.resources
    if resources is None:
        sib = args.lib.resolve().parent.parent / "pdf_resources"
        resources = sib if sib.is_dir() else None
    lib = load_shim(args.lib, resources)
    print(
        f"shim: ABI {lib.dpr_abi_version()}, docling-parse {lib.dpr_docling_parse_version().decode()}",
        file=sys.stderr,
    )

    pages = 0
    mismatches = 0
    for pdf in args.pdfs:
        data = pdf.read_bytes()
        err = ctypes.create_string_buffer(1024)
        doc = lib.dpr_open(data, len(data), None, err, 1024)
        if not doc:
            print(f"{pdf.name}: shim could not open ({err.value.decode(errors='replace')})")
            mismatches += 1
            continue
        try:
            ref = python_renders(pdf, scales)
            n = lib.dpr_page_count(doc)
            for p in range(n):
                for s in scales:
                    got = shim_render(lib, doc, p, s, scales[0])
                    want = ref.get((p, s))
                    pages += 1
                    if want is None:
                        print(f"{pdf.name} p{p + 1} @{s}: no Python render")
                        mismatches += 1
                        continue
                    if got.shape != want.shape or not np.array_equal(got, want):
                        d = np.abs(got.astype(np.int16) - want.astype(np.int16)) if got.shape == want.shape else None
                        detail = (
                            f"shape {got.shape} vs {want.shape}"
                            if d is None
                            else f"{(d.max(axis=2) > 0).mean() * 100:.2f}% px differ, max {d.max()}"
                        )
                        print(f"{pdf.name} p{p + 1} @{s}: MISMATCH ({detail})")
                        mismatches += 1
                        if args.dump:
                            args.dump.mkdir(parents=True, exist_ok=True)
                            Image.fromarray(got).save(args.dump / f"{pdf.stem}.p{p + 1}.{s}.shim.png")
                            Image.fromarray(want).save(args.dump / f"{pdf.stem}.p{p + 1}.{s}.python.png")
                lib.dpr_release_page(doc, p)
        finally:
            lib.dpr_close(doc)
    print(f"{pages - mismatches}/{pages} page renders byte-identical to docling-parse")
    return 0 if mismatches == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
