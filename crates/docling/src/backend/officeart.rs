//! Shared OfficeArt (Escher, [MS-ODRAW]) record machinery.
//!
//! The legacy binary Office formats embed drawings — and the images inside
//! them — as OfficeArt record trees: the same 8-byte record header the PPT
//! backend walks (version/instance, type, length; version `0xF` marks a
//! container). This module hosts the record iterator plus the BLIP (picture)
//! decoding used by both the DOC backend (inline `PICF` pictures in the Data
//! stream, floating shapes via the drawing in the Table stream) and the PPT
//! backend (slide drawings).

use docling_core::PictureImage;

/// A record header: version, instance, and type.
pub(crate) struct RecordHeader {
    pub(crate) version: u8,
    pub(crate) instance: u16,
    pub(crate) rec_type: u16,
}

/// Iterator over the records of one container body (or a whole stream).
pub(crate) struct Records<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Records<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
}

impl<'a> Iterator for Records<'a> {
    type Item = (RecordHeader, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let d = self.data.get(self.pos..)?;
        if d.len() < 8 {
            return None;
        }
        let ver_inst = u16::from_le_bytes([d[0], d[1]]);
        let rec_type = u16::from_le_bytes([d[2], d[3]]);
        let len = u32::from_le_bytes([d[4], d[5], d[6], d[7]]) as usize;
        let body = d.get(8..8 + len)?;
        self.pos += 8 + len;
        Some((
            RecordHeader {
                version: (ver_inst & 0x0F) as u8,
                instance: ver_inst >> 4,
                rec_type,
            },
            body,
        ))
    }
}

/// OfficeArt BLIP record types ([MS-ODRAW] OfficeArtBlip*).
pub(crate) const RT_BLIP_EMF: u16 = 0xF01A;
pub(crate) const RT_BLIP_WMF: u16 = 0xF01B;
pub(crate) const RT_BLIP_PICT: u16 = 0xF01C;
pub(crate) const RT_BLIP_JPEG: u16 = 0xF01D;
pub(crate) const RT_BLIP_PNG: u16 = 0xF01E;
pub(crate) const RT_BLIP_DIB: u16 = 0xF01F;
pub(crate) const RT_BLIP_TIFF: u16 = 0xF029;
/// Second JPEG record type (CMYK variants use 0xF02A).
pub(crate) const RT_BLIP_JPEG2: u16 = 0xF02A;

pub(crate) fn is_blip(rec_type: u16) -> bool {
    matches!(
        rec_type,
        RT_BLIP_EMF
            | RT_BLIP_WMF
            | RT_BLIP_PICT
            | RT_BLIP_JPEG
            | RT_BLIP_PNG
            | RT_BLIP_DIB
            | RT_BLIP_TIFF
            | RT_BLIP_JPEG2
    )
}

/// Decode a BLIP record body into a [`PictureImage`].
///
/// Raster BLIPs carry one or two 16-byte UIDs (per the record instance),
/// then a tag byte, then the raw image bytes. Rather than hard-coding every
/// instance value, the (small) set of plausible payload offsets is tried and
/// validated by actually decoding the header — a wrong offset simply fails
/// image sniffing. EMF / WMF BLIPs are (usually deflated) metafiles,
/// rendered to PNG through [`super::metafile`] (#536); a Mac PICT yields
/// `None` and the caller keeps the picture as a placeholder.
pub(crate) fn decode_blip(header: &RecordHeader, body: &[u8]) -> Option<PictureImage> {
    let mime = match header.rec_type {
        RT_BLIP_JPEG | RT_BLIP_JPEG2 => "image/jpeg",
        RT_BLIP_PNG => "image/png",
        RT_BLIP_TIFF => "image/tiff",
        RT_BLIP_DIB => "image/bmp",
        RT_BLIP_EMF | RT_BLIP_WMF => return decode_metafile_blip(header, body),
        _ => return None, // PICT: placeholder
    };
    for uid_len in [16usize, 32] {
        let Some(rest) = body.get(uid_len + 1..) else {
            continue;
        };
        if header.rec_type == RT_BLIP_DIB {
            // DIB payload is a BITMAPINFOHEADER + bits with no file header;
            // prepend one so the `image` crate can read it as BMP.
            if let Some(img) = decode_dib(rest) {
                return Some(img);
            }
            // Some writers omit the tag byte for DIBs.
            if let Some(img) = body.get(uid_len..).and_then(decode_dib) {
                return Some(img);
            }
        } else if let Some(img) = crate::backend::images::build_picture(mime, rest.to_vec()) {
            return Some(img);
        }
    }
    None
}

/// A metafile BLIP ([MS-ODRAW] 2.2.24 / 2.2.25): one or two 16-byte UIDs
/// (per the low instance bit), then a 34-byte `OfficeArtMetafileHeader` —
/// `cbSize` (uncompressed size), `rcBounds`, `ptSize` (the picture size in
/// EMUs), `cbSave`, `compression` (0 = deflate, 0xFE = none), `filter` —
/// then the metafile. A WMF BLIP drops the placeable header, so `ptSize` is
/// what sizes it.
fn decode_metafile_blip(header: &RecordHeader, body: &[u8]) -> Option<PictureImage> {
    use std::io::Read as _;
    let uid_len = 16 * (1 + usize::from(header.instance & 1));
    let mh = body.get(uid_len..uid_len + 34)?;
    let le32 = |at: usize| u32::from_le_bytes([mh[at], mh[at + 1], mh[at + 2], mh[at + 3]]);
    let size = le32(0) as usize;
    let (cx, cy) = (le32(20) as i32, le32(24) as i32);
    let saved = body.get(uid_len + 34..)?;
    let saved = &saved[..saved.len().min(le32(28) as usize)];
    let data = match mh[32] {
        0 => {
            // `cbSize` bounds the inflate: a crafted header cannot make it
            // allocate more than the record claims.
            let mut out = Vec::with_capacity(size.min(64 << 20));
            flate2::read::ZlibDecoder::new(saved)
                .take(size.min(256 << 20) as u64)
                .read_to_end(&mut out)
                .ok()?;
            out
        }
        _ => saved.to_vec(),
    };
    // 9525 EMU per pixel at 96 dpi.
    let hint = (cx > 0 && cy > 0).then(|| (f64::from(cx) / 9525.0, f64::from(cy) / 9525.0));
    super::metafile::render(&data, hint)
}

/// Wrap headerless DIB bytes in a BMP file header and decode.
fn decode_dib(dib: &[u8]) -> Option<PictureImage> {
    if dib.len() < 40 || u32::from_le_bytes([dib[0], dib[1], dib[2], dib[3]]) < 40 {
        return None;
    }
    let mut bmp = Vec::with_capacity(dib.len() + 14);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&(14 + dib.len() as u32).to_le_bytes());
    bmp.extend_from_slice(&[0; 4]);
    // Pixel-data offset: file header + info header + palette; leaving the
    // palette out of the computation still decodes for the common 24/32-bit
    // case, and `image` re-derives it from the info header anyway.
    bmp.extend_from_slice(&(14u32 + 40).to_le_bytes());
    bmp.extend_from_slice(dib);
    crate::backend::images::build_picture("image/bmp", bmp)
}

/// The OfficeArtFBSE record type (BLIP store entry). Not a container by
/// version, but it embeds a BLIP record after its 36 fixed bytes.
pub(crate) const RT_FBSE: u16 = 0xF007;

/// Depth-first search of a record tree for the first BLIP, decoded.
/// Returns `Some(picture)` on a decodable image, `None` otherwise.
pub(crate) fn first_blip(data: &[u8], depth: usize) -> Option<PictureImage> {
    if depth > 16 {
        return None;
    }
    for (h, body) in Records::new(data) {
        if is_blip(h.rec_type) {
            if let Some(img) = decode_blip(&h, body) {
                return Some(img);
            }
        } else if h.rec_type == RT_FBSE {
            if let Some(img) = body.get(36..).and_then(|tail| first_blip(tail, depth + 1)) {
                return Some(img);
            }
        } else if h.version == 0xF {
            if let Some(img) = first_blip(body, depth + 1) {
                return Some(img);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_iterator_walks_headers() {
        // One record: ver 0xF (container), instance 1, type 0xF004, len 0.
        let data = [0x1Fu8, 0x00, 0x04, 0xF0, 0x00, 0x00, 0x00, 0x00];
        let mut it = Records::new(&data);
        let (h, body) = it.next().expect("one record");
        assert_eq!(h.version, 0xF);
        assert_eq!(h.instance, 1);
        assert_eq!(h.rec_type, 0xF004);
        assert!(body.is_empty());
        assert!(it.next().is_none());
    }

    #[test]
    fn truncated_record_is_none_not_panic() {
        assert!(Records::new(&[0x00, 0x00, 0x1E]).next().is_none());
        // Header claims more body than exists.
        let data = [0x00u8, 0x00, 0x1E, 0xF0, 0xFF, 0x00, 0x00, 0x00];
        assert!(Records::new(&data).next().is_none());
    }

    #[test]
    fn decode_blip_finds_png_behind_single_uid() {
        // 1×1 PNG preceded by a 16-byte UID and a tag byte.
        const RED_PNG: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x90, 0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x08,
            0xd7, 0x63, 0xf8, 0xcf, 0xc0, 0x00, 0x00, 0x00, 0x03, 0x00, 0x01, 0x6e, 0x2c, 0xdc,
            0x33, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let mut body = vec![0u8; 16]; // rgbUid1
        body.push(0xFF); // tag
        body.extend_from_slice(RED_PNG);
        let h = RecordHeader {
            version: 0,
            instance: 0x6E0,
            rec_type: RT_BLIP_PNG,
        };
        let img = decode_blip(&h, &body).expect("decodes");
        assert_eq!((img.width, img.height), (1, 1));
    }

    /// A Word 97 WMF BLIP (no placeable header, sized by `ptSize`) behind
    /// two UIDs, deflated and stored.
    #[cfg(feature = "pdf")]
    #[test]
    fn decode_blip_renders_metafiles() {
        use std::io::Write as _;
        let wmf = crate::backend::metafile::tests::wmf(
            None,
            &crate::backend::metafile::tests::wmf_body(),
        );
        let deflated = {
            let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            z.write_all(&wmf).unwrap();
            z.finish().unwrap()
        };
        for (compression, saved) in [(0u8, &deflated), (0xFE, &wmf)] {
            let mut body = vec![0u8; 32]; // rgbUid1 + rgbUid2 (odd instance)
            body.extend_from_slice(&(wmf.len() as u32).to_le_bytes());
            body.extend_from_slice(&[0; 16]); // rcBounds
            body.extend_from_slice(&(120 * 9525u32).to_le_bytes());
            body.extend_from_slice(&(60 * 9525u32).to_le_bytes());
            body.extend_from_slice(&(saved.len() as u32).to_le_bytes());
            body.extend_from_slice(&[compression, 0xFE]);
            body.extend_from_slice(saved);
            let h = RecordHeader {
                version: 0,
                instance: 0x217,
                rec_type: RT_BLIP_WMF,
            };
            let img = decode_blip(&h, &body).expect("renders");
            assert_eq!(
                (img.mimetype.as_str(), img.width, img.height),
                ("image/png", 120, 60)
            );
        }
        // A PICT stays a placeholder.
        let h = RecordHeader {
            version: 0,
            instance: 0x542,
            rec_type: RT_BLIP_PICT,
        };
        assert!(decode_blip(&h, &[0; 64]).is_none());
    }
}
