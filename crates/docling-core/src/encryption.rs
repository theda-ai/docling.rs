//! Why an encrypted document could not be opened — the one typed signal every
//! backend raises for a password-protected input (#636).
//!
//! A PDF, an Office document, an iWork package and a WordPerfect file each
//! detect encryption in their own way and word the failure for their own
//! users, but a caller has one question: *is this a password problem, and
//! would asking for one help?* Before this type, the answer meant matching
//! on message text ("document is encrypted", "the PDF is encrypted …",
//! "password-protected"), which no release promised to keep. The value lives
//! here, in the crate every backend already depends on, so `docling-pdf`'s
//! error and the converter's error can both carry it; the converter's
//! `ConversionError::encryption()` finds it anywhere on the `source()` chain.
//!
//! The [`Display`](std::fmt::Display) text is the Office backends' wording
//! (`document is encrypted (a password is required to open it)`, …), which a
//! backend prefixes with its format; backends with an established wording of
//! their own (the PDF reader, iWork, WordPerfect) keep it and attach the
//! typed value as the cause, so no error message changed with its arrival.

use std::fmt;

/// Why an encrypted document could not be opened.
///
/// Only [`NeedPassword`](Self::NeedPassword) and
/// [`WrongPassword`](Self::WrongPassword) are solved by a password
/// ([`needs_password`](Self::needs_password)); the other two say the file
/// cannot be read however it is asked. Non-exhaustive: a reader may learn to
/// tell another case apart.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EncryptionError {
    /// No password was given and none the reader tries by itself (a
    /// format's documented default) opens the document.
    NeedPassword,
    /// A password was given and neither it nor a default one opens it.
    WrongPassword,
    /// The document is encrypted in a way this reader does not decrypt
    /// (ODF package encryption, XOR obfuscation, an iWork package, a
    /// WordPerfect file, an unknown cipher), so no password would help. The
    /// payload names the scheme.
    NotDecryptable(String),
    /// The document is encrypted, but its encryption header is damaged or
    /// outside the specification's bounds. The payload names the field.
    Malformed(String),
}

impl EncryptionError {
    /// Whether a (different) password would open the document — the two
    /// cases worth prompting a user for.
    pub fn needs_password(&self) -> bool {
        matches!(self, Self::NeedPassword | Self::WrongPassword)
    }

    /// A stable machine-readable name for the case — what an HTTP error
    /// body or a log line carries: `password_required`, `wrong_password`,
    /// `not_decryptable`, `malformed_encryption`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NeedPassword => "password_required",
            Self::WrongPassword => "wrong_password",
            Self::NotDecryptable(_) => "not_decryptable",
            Self::Malformed(_) => "malformed_encryption",
        }
    }
}

impl fmt::Display for EncryptionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NeedPassword => {
                f.write_str("document is encrypted (a password is required to open it)")
            }
            Self::WrongPassword => f.write_str("document is encrypted and the password is wrong"),
            Self::NotDecryptable(what) => {
                write!(
                    f,
                    "document is encrypted with an unsupported scheme ({what})"
                )
            }
            Self::Malformed(what) => write!(
                f,
                "document is encrypted, but its encryption header is damaged ({what})"
            ),
        }
    }
}

impl std::error::Error for EncryptionError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_password_cases_need_a_password() {
        assert!(EncryptionError::NeedPassword.needs_password());
        assert!(EncryptionError::WrongPassword.needs_password());
        assert!(!EncryptionError::NotDecryptable("ODF".into()).needs_password());
        assert!(!EncryptionError::Malformed("keyBits".into()).needs_password());
    }

    #[test]
    fn codes_are_distinct_and_stable() {
        let all = [
            EncryptionError::NeedPassword,
            EncryptionError::WrongPassword,
            EncryptionError::NotDecryptable("x".into()),
            EncryptionError::Malformed("y".into()),
        ];
        let codes: std::collections::BTreeSet<_> = all.iter().map(|e| e.code()).collect();
        assert_eq!(codes.len(), all.len());
        assert_eq!(EncryptionError::NeedPassword.code(), "password_required");
        assert_eq!(EncryptionError::WrongPassword.code(), "wrong_password");
    }

    /// The wording #624 fixed, which every Office backend prefixes with its
    /// format name.
    #[test]
    fn display_is_the_office_wording() {
        assert_eq!(
            EncryptionError::NeedPassword.to_string(),
            "document is encrypted (a password is required to open it)"
        );
        assert_eq!(
            EncryptionError::WrongPassword.to_string(),
            "document is encrypted and the password is wrong"
        );
        assert_eq!(
            EncryptionError::NotDecryptable("XOR obfuscation".into()).to_string(),
            "document is encrypted with an unsupported scheme (XOR obfuscation)"
        );
        assert_eq!(
            EncryptionError::Malformed("keyBits".into()).to_string(),
            "document is encrypted, but its encryption header is damaged (keyBits)"
        );
    }
}
