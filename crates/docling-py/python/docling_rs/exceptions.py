"""docling-parity alias of ``docling.exceptions``::

    # was: from docling.exceptions import ConversionError
    from docling_rs.exceptions import ConversionError

Beyond docling (#636): an encrypted document raises a *subclass* of
``ConversionError`` that names the case, so a caller can prompt for a password
instead of matching the message::

    try:
        converter.convert("report.pdf")
    except PasswordRequiredError:   # no password was given
        ...
    except WrongPasswordError:      # the given one does not open it
        ...
    except EncryptionError:         # encrypted in a scheme docling.rs cannot read
        ...

``except ConversionError`` still catches all of them.
"""

from ._native import (
    ConversionError,
    EncryptionError,
    PasswordRequiredError,
    WrongPasswordError,
)

__all__ = [
    "ConversionError",
    "EncryptionError",
    "PasswordRequiredError",
    "WrongPasswordError",
]
