"""#636: an encrypted document raises a typed subclass of ``ConversionError``.

Fixtures: the #624/#625 Office files (password ``1234``) and the
password-protected PDF of ``tests/data/pdf_password`` (text-layer path, so no
models are needed).
"""

from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[3]
DOCX = REPO / "crates/docling/tests/data/encrypted/min_encrypted.docx"
PDF = REPO / "tests/data/pdf_password/sources/2206.01062_pg3.pdf"

docling_rs = pytest.importorskip("docling_rs")


def test_the_exceptions_are_conversion_errors():
    from docling_rs.exceptions import (
        ConversionError,
        EncryptionError,
        PasswordRequiredError,
        WrongPasswordError,
    )

    assert issubclass(EncryptionError, ConversionError)
    assert issubclass(PasswordRequiredError, EncryptionError)
    assert issubclass(WrongPasswordError, EncryptionError)
    assert docling_rs.PasswordRequiredError is PasswordRequiredError


@pytest.mark.parametrize("path", [DOCX, PDF], ids=["docx", "pdf"])
def test_no_password_raises_password_required(path):
    from docling_rs import DocumentConverter, PasswordRequiredError

    with pytest.raises(PasswordRequiredError) as e:
        DocumentConverter(text_layer_only=True).convert(path)
    assert "encrypted" in str(e.value)


@pytest.mark.parametrize("path", [DOCX, PDF], ids=["docx", "pdf"])
def test_a_wrong_password_raises_wrong_password(path):
    from docling_rs import DocumentConverter, WrongPasswordError

    with pytest.raises(WrongPasswordError):
        DocumentConverter(text_layer_only=True, password="nope").convert(path)


@pytest.mark.parametrize("path", [DOCX, PDF], ids=["docx", "pdf"])
def test_the_password_opens_it(path):
    from docling_rs import DocumentConverter

    result = DocumentConverter(text_layer_only=True, password="1234").convert(path)
    assert result.document.export_to_markdown().strip()


def test_plain_failures_stay_conversion_errors(tmp_path):
    from docling_rs import ConversionError, DocumentConverter, EncryptionError

    junk = tmp_path / "junk.docx"
    junk.write_bytes(b"PK\x03\x04 not a zip")
    with pytest.raises(ConversionError) as e:
        DocumentConverter().convert(junk)
    assert not isinstance(e.value, EncryptionError)
