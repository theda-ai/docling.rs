"""Tests for ``docling_rs.pandoc`` — the Rust Pandoc AST serializer (#515)
exposed to Python. Declarative path only, no ML models."""

import json

import pytest

docling_rs = pytest.importorskip("docling_rs")

from docling_rs.pandoc import (  # noqa: E402
    PANDOC_API_VERSION,
    export_to_pandoc,
    save_as_pandoc,
)

MD = b"# Guide\n\nInstall the **tools**.\n\n- clone\n- build\n\n| a | b |\n|---|---|\n| 1 | 2 |\n"


def _document():
    return docling_rs.DocumentConverter().convert_bytes("guide.md", MD).document


def test_export_is_a_pandoc_document():
    ast = json.loads(export_to_pandoc(_document()))
    assert ast["pandoc-api-version"] == list(PANDOC_API_VERSION)
    kinds = [b["t"] for b in ast["blocks"]]
    assert kinds == ["Header", "Para", "BulletList", "Table"]


def test_dict_and_string_inputs_match_the_document():
    doc = _document()
    expected = export_to_pandoc(doc)
    assert export_to_pandoc(doc.export_to_dict()) == expected
    assert export_to_pandoc(json.dumps(doc.export_to_dict())) == expected


def test_unsupported_api_version_raises():
    assert export_to_pandoc(_document(), api_version="1.23")
    with pytest.raises(ValueError, match="unsupported Pandoc API version '1.22'"):
        export_to_pandoc(_document(), api_version="1.22")


def test_save_writes_the_file(tmp_path):
    out = tmp_path / "guide.pandoc.json"
    save_as_pandoc(_document(), out)
    assert json.loads(out.read_text())["blocks"][0]["t"] == "Header"
    with pytest.raises(ValueError, match="save_as_pandoc"):
        export_to_pandoc(_document(), image_mode="referenced")
