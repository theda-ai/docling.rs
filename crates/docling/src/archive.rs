//! ZIP archives as input (#557): every document inside converts on its own.
//!
//! Batches of documents travel as ZIP files — exports, datasets, mail
//! attachments, uploads. [`Archive`] reads one without unpacking it to disk:
//! [`Archive::open`] lists the entries from the central directory and decides,
//! before decompressing a byte, which ones convert and why the others are
//! skipped (unsupported type, nested archive, unsafe path, encryption, the
//! [`ArchiveLimits`]); [`Archive::read`] then extracts one entry as a
//! [`SourceDocument`], never more than the size its header declared.
//! [`DocumentConverter::convert_archive`] puts the two together as a lazy
//! iterator of per-entry outcomes, so one broken document never fails the
//! rest.
//!
//! An archive is not one document, so it has no [`InputFormat`]:
//! `InputFormat::from_extension("zip")` stays `None` and
//! [`DocumentConverter::convert`] keeps rejecting it — archives go through
//! this module's API (and the CLI's / serve's archive handling built on it).
//! Office packages that are ZIP files underneath (DOCX, XLSX, ODT, EPUB, …)
//! are documents, not archives, and convert as before.
//!
//! [`DocumentConverter::convert_archive`]: crate::DocumentConverter::convert_archive
//! [`DocumentConverter::convert`]: crate::DocumentConverter::convert

use std::io::{Read, Seek};

use crate::error::ConversionError;
use crate::format::InputFormat;
use crate::result::ConversionResult;
use crate::source::SourceDocument;
use crate::DocumentConverter;

/// Bounds on what an archive may make the converter decompress — the guard
/// against ZIP bombs and resource exhaustion on untrusted input. Checked
/// against the central directory before anything is inflated; extraction then
/// stops at the size an entry declared, so a header that understates its
/// entry cannot get past them either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveLimits {
    /// Entries considered at most (directories excluded); the rest are
    /// skipped. Default 10 000.
    pub max_entries: usize,
    /// Largest uncompressed entry converted. Default 256 MiB.
    pub max_entry_size: u64,
    /// Uncompressed bytes the converted entries may add up to. Default 1 GiB.
    pub max_total_size: u64,
    /// Largest uncompressed : compressed ratio accepted for an entry over
    /// 1 MiB (text compresses 5–20×; a bomb, 1000× and more). Default 200.
    pub max_compression_ratio: u64,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            max_entries: 10_000,
            max_entry_size: 256 << 20,
            max_total_size: 1 << 30,
            max_compression_ratio: 200,
        }
    }
}

impl ArchiveLimits {
    /// The defaults with the `DOCLING_RS_ZIP_MAX_ENTRIES`,
    /// `DOCLING_RS_ZIP_MAX_ENTRY_MB`, `DOCLING_RS_ZIP_MAX_TOTAL_MB` and
    /// `DOCLING_RS_ZIP_MAX_RATIO` overrides — what the CLI and serve use.
    pub fn from_env() -> Self {
        let d = Self::default();
        let mb = |name: &str, default: u64| {
            docling_core::env::parse::<u64>(name).map_or(default, |v| v.saturating_mul(1 << 20))
        };
        Self {
            max_entries: docling_core::env::parse("DOCLING_RS_ZIP_MAX_ENTRIES")
                .unwrap_or(d.max_entries),
            max_entry_size: mb("DOCLING_RS_ZIP_MAX_ENTRY_MB", d.max_entry_size),
            max_total_size: mb("DOCLING_RS_ZIP_MAX_TOTAL_MB", d.max_total_size),
            max_compression_ratio: docling_core::env::parse("DOCLING_RS_ZIP_MAX_RATIO")
                .unwrap_or(d.max_compression_ratio),
        }
    }
}

/// Entries this small are never judged by their compression ratio: a page of
/// blanks compresses 1000× legitimately and cannot hurt anyone.
const RATIO_FLOOR: u64 = 1 << 20;

/// One file entry of an archive as listed by [`Archive::open`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEntryInfo {
    /// Position in the archive's central directory (for [`Archive::read`]).
    pub index: usize,
    /// The entry's path inside the archive, `/`-separated. For a skipped
    /// entry with an unsafe name, the raw name as stored.
    pub path: String,
    /// The format the entry converts as (from its extension); `None` when
    /// it is skipped.
    pub format: Option<InputFormat>,
    /// Why the entry is not converted, when it is not.
    pub skipped: Option<String>,
    /// Uncompressed size as the archive declares it.
    pub size: u64,
}

/// A ZIP archive opened for conversion. See the [module docs](self).
pub struct Archive<R> {
    zip: zip::ZipArchive<R>,
    entries: Vec<ArchiveEntryInfo>,
}

impl<R: Read + Seek> Archive<R> {
    /// Read the archive's directory and classify every file entry. Fails only
    /// when `reader` is not a readable ZIP archive.
    pub fn open(reader: R, limits: &ArchiveLimits) -> Result<Self, ConversionError> {
        let mut zip = zip::ZipArchive::new(reader)
            .map_err(|e| ConversionError::Parse(format!("zip: {e}")))?;
        let mut entries = Vec::new();
        let mut total: u64 = 0;
        for index in 0..zip.len() {
            // Raw access reads the local header only — nothing is inflated.
            let Ok(file) = zip.by_index_raw(index) else {
                entries.push(ArchiveEntryInfo {
                    index,
                    path: format!("#{index}"),
                    format: None,
                    skipped: Some("unreadable entry header".into()),
                    size: 0,
                });
                continue;
            };
            if file.is_dir() {
                continue;
            }
            let size = file.size();
            let compressed = file.compressed_size();
            let safe = file
                .enclosed_name()
                .map(|p| p.to_string_lossy().replace('\\', "/"));
            let path = safe.clone().unwrap_or_else(|| file.name().to_string());
            let skip = |why: &str| Some(why.to_string());
            let mut format = None;
            let skipped = if entries.len() >= limits.max_entries {
                skip("over the archive's entry limit")
            } else if safe.is_none() {
                // An absolute path or one climbing out with `..`: never
                // honoured, never written anywhere.
                skip("unsafe path")
            } else if file.encrypted() {
                skip("encrypted")
            } else if is_macos_metadata(&path) {
                skip("macOS metadata")
            } else if is_archive(&path) {
                skip("nested archive")
            } else if let Some(f) = extension(&path).and_then(InputFormat::from_extension) {
                if size > limits.max_entry_size {
                    skip("larger than the per-entry size limit")
                } else if size > RATIO_FLOOR
                    && size / compressed.max(1) > limits.max_compression_ratio
                {
                    skip("compression ratio over the limit")
                } else if total.saturating_add(size) > limits.max_total_size {
                    skip("over the archive's total size limit")
                } else {
                    total += size;
                    format = Some(f);
                    None
                }
            } else {
                skip("unsupported file type")
            };
            entries.push(ArchiveEntryInfo {
                index,
                path,
                format,
                skipped,
                size,
            });
        }
        Ok(Self { zip, entries })
    }

    /// The archive's file entries in directory order, converted or skipped.
    pub fn entries(&self) -> &[ArchiveEntryInfo] {
        &self.entries
    }

    /// Extract the entry at directory position `index` as a source document
    /// named after its file stem. Errors for an entry [`open`](Self::open)
    /// skipped, one that fails to inflate, or one that turns out larger than
    /// its header declared.
    pub fn read(&mut self, index: usize) -> Result<SourceDocument, ConversionError> {
        let info = self
            .entries
            .iter()
            .find(|e| e.index == index)
            .ok_or_else(|| ConversionError::Parse(format!("zip: no entry #{index}")))?;
        let format = match (&info.format, &info.skipped) {
            (Some(f), None) => *f,
            (_, reason) => {
                return Err(ConversionError::Parse(format!(
                    "zip: {} is skipped ({})",
                    info.path,
                    reason.as_deref().unwrap_or("not convertible")
                )))
            }
        };
        let (path, declared) = (info.path.clone(), info.size);
        let file = self
            .zip
            .by_index(index)
            .map_err(|e| ConversionError::Parse(format!("zip: {path}: {e}")))?;
        // Never more than the header promised (which the limits vetted).
        let mut bytes = Vec::with_capacity(declared.min(64 << 20) as usize);
        file.take(declared.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| ConversionError::Parse(format!("zip: {path}: {e}")))?;
        if bytes.len() as u64 > declared {
            return Err(ConversionError::Parse(format!(
                "zip: {path}: larger than its header declares"
            )));
        }
        let name = path
            .rsplit('/')
            .next()
            .map(|f| f.rsplit_once('.').map_or(f, |(stem, _)| stem))
            .filter(|s| !s.is_empty())
            .unwrap_or("document")
            .to_string();
        Ok(SourceDocument::from_bytes(name, format, bytes))
    }
}

/// What became of one archive entry.
#[derive(Debug)]
pub enum ArchiveOutcome {
    /// Converted (possibly partially — see the result's status).
    Converted(Box<ConversionResult>),
    /// Not attempted, and why: unsupported type, nested archive, unsafe path,
    /// encryption, or a limit.
    Skipped(String),
    /// Attempted and failed; the other entries are unaffected.
    Failed(ConversionError),
}

/// One entry's path inside the archive and its outcome.
#[derive(Debug)]
pub struct ArchiveItem {
    pub path: String,
    pub outcome: ArchiveOutcome,
}

/// The lazy per-entry conversion [`DocumentConverter::convert_archive`]
/// returns: each `next()` extracts and converts one entry, so memory holds a
/// single document at a time.
///
/// [`DocumentConverter::convert_archive`]: crate::DocumentConverter::convert_archive
pub struct ArchiveConversion<'c, R> {
    converter: &'c DocumentConverter,
    archive: Archive<R>,
    next: usize,
}

impl<R: Read + Seek> Iterator for ArchiveConversion<'_, R> {
    type Item = ArchiveItem;

    fn next(&mut self) -> Option<ArchiveItem> {
        let info = self.archive.entries.get(self.next)?.clone();
        self.next += 1;
        let outcome = match info.skipped {
            Some(reason) => ArchiveOutcome::Skipped(reason),
            None => match self.archive.read(info.index) {
                Ok(source) => match self.converter.convert(source) {
                    Ok(result) => ArchiveOutcome::Converted(Box::new(result)),
                    Err(e) => ArchiveOutcome::Failed(e),
                },
                Err(e) => ArchiveOutcome::Failed(e),
            },
        };
        Some(ArchiveItem {
            path: info.path,
            outcome,
        })
    }
}

impl DocumentConverter {
    /// Convert every document inside a ZIP archive (#557), one at a time: a
    /// lazy iterator of each entry's [`ArchiveOutcome`] in directory order.
    /// Entries convert with this converter's settings; the archive limits come
    /// from [`DocumentConverter::archive_limits`]. Errors only when `reader`
    /// is not a ZIP archive.
    pub fn convert_archive<R: Read + Seek>(
        &self,
        reader: R,
    ) -> Result<ArchiveConversion<'_, R>, ConversionError> {
        Ok(ArchiveConversion {
            converter: self,
            archive: Archive::open(reader, &self.archive_limits_ref())?,
            next: 0,
        })
    }
}

/// Whether an entry is a ZIP/tar/7z/rar archive by its extension — never
/// descended into (one level is the contract; a nested bomb stays shut).
pub fn is_archive(path: &str) -> bool {
    matches!(
        extension(path).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst")
    )
}

/// `__MACOSX/…` resource forks and `._name` AppleDouble files: Finder's
/// metadata, never documents.
fn is_macos_metadata(path: &str) -> bool {
    path.starts_with("__MACOSX/") || path.rsplit('/').next().is_some_and(|f| f.starts_with("._"))
}

fn extension(path: &str) -> Option<&str> {
    let file = path.rsplit('/').next()?;
    file.rsplit_once('.')
        .map(|(_, ext)| ext)
        .filter(|e| !e.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        let mut w = zip::ZipWriter::new(&mut out);
        let opts = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            if name.ends_with('/') {
                w.add_directory(name.trim_end_matches('/'), opts).unwrap();
            } else {
                w.start_file(*name, opts).unwrap();
                w.write_all(data).unwrap();
            }
        }
        w.finish().unwrap();
        out.into_inner()
    }

    #[test]
    fn entries_are_classified_before_inflating() {
        let data = zip_of(&[
            ("docs/", b""),
            ("docs/a.md", b"# A\n\nalpha"),
            ("docs/b.html", b"<html><body><p>beta</p></body></html>"),
            ("tool.exe", b"MZ"),
            ("inner.zip", b"PK\x03\x04"),
            ("__MACOSX/docs/._a.md", b"junk"),
            ("noext", b"x"),
        ]);
        let archive = Archive::open(Cursor::new(data), &ArchiveLimits::default()).unwrap();
        let got: Vec<(&str, Option<&str>)> = archive
            .entries()
            .iter()
            .map(|e| (e.path.as_str(), e.skipped.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                ("docs/a.md", None),
                ("docs/b.html", None),
                ("tool.exe", Some("unsupported file type")),
                ("inner.zip", Some("nested archive")),
                ("__MACOSX/docs/._a.md", Some("macOS metadata")),
                ("noext", Some("unsupported file type")),
            ]
        );
    }

    #[test]
    fn convert_archive_reports_every_entry() {
        let data = zip_of(&[
            ("a.md", b"# A\n\nalpha"),
            ("broken.docx", b"not a zip at all"),
            ("c.exe", b"MZ"),
        ]);
        let conv = DocumentConverter::new();
        let items: Vec<ArchiveItem> = conv.convert_archive(Cursor::new(data)).unwrap().collect();
        assert_eq!(items.len(), 3);
        match &items[0].outcome {
            ArchiveOutcome::Converted(r) => {
                assert_eq!(r.input_name, "a");
                assert!(r.document.export_to_markdown().contains("alpha"));
            }
            other => panic!("a.md: {other:?}"),
        }
        assert!(
            matches!(items[1].outcome, ArchiveOutcome::Failed(_)),
            "{:?}",
            items[1]
        );
        assert!(matches!(items[2].outcome, ArchiveOutcome::Skipped(_)));
    }

    #[test]
    fn limits_stop_entries_before_decompression() {
        let big = vec![b'a'; 3 << 20]; // 3 MiB of one byte: ratio ≫ 200
        let data = zip_of(&[("a.md", b"alpha"), ("bomb.md", &big), ("c.md", b"gamma")]);
        let limits = ArchiveLimits::default();
        let archive = Archive::open(Cursor::new(data.clone()), &limits).unwrap();
        assert_eq!(
            archive.entries()[1].skipped.as_deref(),
            Some("compression ratio over the limit")
        );
        let tight = ArchiveLimits {
            max_entries: 2,
            max_entry_size: 4,
            ..limits
        };
        let archive = Archive::open(Cursor::new(data), &tight).unwrap();
        let skipped: Vec<_> = archive
            .entries()
            .iter()
            .map(|e| e.skipped.as_deref())
            .collect();
        assert_eq!(
            skipped,
            [
                Some("larger than the per-entry size limit"),
                Some("larger than the per-entry size limit"),
                Some("over the archive's entry limit"),
            ]
        );
    }

    #[test]
    fn unsafe_paths_are_never_extracted() {
        // `..` climbing out is refused; a leading `/` is read as relative
        // (the zip crate's `enclosed_name`), so it stays inside the target.
        let data = zip_of(&[("../escape.md", b"x"), ("/abs.md", b"y"), ("ok.md", b"z")]);
        let mut archive = Archive::open(Cursor::new(data), &ArchiveLimits::default()).unwrap();
        let listed: Vec<(&str, Option<&str>)> = archive
            .entries()
            .iter()
            .map(|e| (e.path.as_str(), e.skipped.as_deref()))
            .collect();
        assert_eq!(
            listed,
            [
                ("../escape.md", Some("unsafe path")),
                ("abs.md", None),
                ("ok.md", None)
            ]
        );
        assert!(archive.read(0).is_err());
        assert_eq!(archive.read(1).unwrap().bytes, b"y");
    }

    #[test]
    fn not_a_zip_is_an_error() {
        assert!(Archive::open(Cursor::new(b"plain".to_vec()), &ArchiveLimits::default()).is_err());
        // An archive has no InputFormat: `convert` keeps rejecting `.zip`.
        assert_eq!(InputFormat::from_extension("zip"), None);
    }
}
