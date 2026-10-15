//! `JPXDecode` images (ISO 32000-1, 7.4.9): JPEG 2000 codestreams and JP2
//! files, decoded with `hayro-jpeg2000` (#598).
//!
//! A photograph stored as JPEG 2000 used to draw as a mid-gray block, and
//! the layout model then saw a blank rectangle where docling(-parse) shows it
//! the picture: on the reporter's NASA scans the picture box lost its
//! confidence (0.64 instead of 0.99), ran over the caption line below, and
//! two stacked photos separated by a caption fused into one — the caption
//! then nested inside the picture and vanished from the Markdown. Decoded,
//! the same pages give the same regions as the docling-parse renderer.
//!
//! What the filter dictionary decides and what the codestream decides
//! (7.4.9): the colour space comes from the codestream (gray, RGB — sYCC
//! already converted —, CMYK, or by component count for an ICC profile) when
//! the image dictionary has no `/ColorSpace`; a dictionary `/ColorSpace`
//! wins, and an `Indexed` one means the samples are palette indices, so the
//! decoder is told not to resolve the codestream's own palette.
//! `/SMaskInData` 1 or 2 takes the codestream's alpha channel as the soft
//! mask (2 = premultiplied, which the renderer treats like 1: the blit
//! premultiplies again, a shade darker on the edges of such an image);
//! without it the alpha channel is dropped. `/BitsPerComponent` is the
//! codestream's; the decoder normalizes every depth to 8 bits.
//!
//! A reduced decode (`target`, the renderer's `codec_reduction_shift` hint)
//! lets the decoder stop at a lower resolution level when the image is drawn
//! far smaller than it is; the returned size is whatever it decoded at.

use hayro_jpeg2000::{ColorSpace as JpxColorSpace, DecodeSettings, DecoderContext, Image};

/// A decoded JPX image: 8-bit interleaved colour samples, the alpha channel
/// (when the codestream has one) split off.
pub struct Decoded {
    pub width: usize,
    pub height: usize,
    /// Colour components per pixel (the alpha channel excluded).
    pub ncomp: usize,
    /// `width * height * ncomp` bytes.
    pub data: Vec<u8>,
    /// The codestream's opacity channel, `width * height` bytes.
    pub alpha: Option<Vec<u8>>,
    /// What the codestream says its colour components are.
    pub color: Color,
}

/// The codestream's colour interpretation, as far as the image dictionary
/// needs it when it names no `/ColorSpace` of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Gray,
    Rgb,
    Cmyk,
    /// An ICC profile or an unknown space: by component count.
    Other,
}

/// Decode `data` (a JP2 file or a raw J2K codestream). `indexed`: the PDF
/// colour space is `Indexed`, so a palette in the codestream must stay
/// unresolved (the samples are the indices). `target`: a `(width, height)`
/// the decoder may stop at (a lower resolution level), `None` for full size.
pub fn decode(data: &[u8], indexed: bool, target: Option<(u32, u32)>) -> Result<Decoded, String> {
    let settings = DecodeSettings {
        resolve_palette_indices: !indexed,
        target_resolution: target,
        ..DecodeSettings::default()
    };
    let image = Image::new(data, &settings).map_err(|e| format!("jpx: {e:?}"))?;
    let color = match image.color_space() {
        JpxColorSpace::Gray => Color::Gray,
        JpxColorSpace::RGB => Color::Rgb,
        JpxColorSpace::CMYK => Color::Cmyk,
        _ => Color::Other,
    };
    let mut ctx = DecoderContext::default();
    let decoded = image.decode(&mut ctx).map_err(|e| format!("jpx: {e:?}"))?;
    // The image's size is the decoded one: with a target resolution the
    // header's extent is already divided by the skipped levels.
    let (width, height) = (image.width() as usize, image.height() as usize);
    let channels = decoded.components().len();
    if width == 0 || height == 0 || channels == 0 {
        return Err("jpx: empty image".into());
    }
    let interleaved = decoded.data_u8();
    if interleaved.len() != width * height * channels {
        return Err(format!(
            "jpx: {} samples for {width}x{height}x{channels}",
            interleaved.len()
        ));
    }
    // The alpha channel, when declared, is the last one.
    let has_alpha = image.has_alpha() && channels >= 2;
    let ncomp = if has_alpha { channels - 1 } else { channels };
    let (data, alpha) = if has_alpha {
        let mut data = Vec::with_capacity(width * height * ncomp);
        let mut alpha = Vec::with_capacity(width * height);
        for px in interleaved.chunks_exact(channels) {
            data.extend_from_slice(&px[..ncomp]);
            alpha.push(px[ncomp]);
        }
        (data, Some(alpha))
    } else {
        (interleaved, None)
    };
    Ok(Decoded {
        width,
        height,
        ncomp,
        data,
        alpha,
        color,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/data/jpx")
                .join(name),
        )
        .expect("jpx fixture")
    }

    /// The fixtures are OpenJPEG (Pillow) encodes of known gradients,
    /// lossless (5/3 wavelet), so the samples come back exactly.
    #[test]
    fn gray_jp2_and_raw_codestream_decode_exactly() {
        for name in ["gray_12x9.jp2", "gray_12x9.j2k"] {
            let d = decode(&fixture(name), false, None).unwrap();
            assert_eq!((d.width, d.height, d.ncomp), (12, 9, 1), "{name}");
            assert_eq!(d.color, Color::Gray, "{name}");
            assert!(d.alpha.is_none());
            for y in 0..9 {
                for x in 0..12 {
                    assert_eq!(
                        d.data[y * 12 + x],
                        ((x * 21 + y * 3) % 256) as u8,
                        "{name} ({x},{y})"
                    );
                }
            }
        }
    }

    #[test]
    fn rgb_jp2_decodes_exactly() {
        let d = decode(&fixture("rgb_8x6.jp2"), false, None).unwrap();
        assert_eq!((d.width, d.height, d.ncomp), (8, 6, 3));
        assert_eq!(d.color, Color::Rgb);
        for y in 0..6 {
            for x in 0..8 {
                let px = &d.data[(y * 8 + x) * 3..][..3];
                assert_eq!(
                    px,
                    [
                        ((x * 32) % 256) as u8,
                        ((y * 40) % 256) as u8,
                        (((x + y) * 17) % 256) as u8
                    ],
                    "({x},{y})"
                );
            }
        }
    }

    /// A JP2 with an opacity channel: colour and alpha come apart, alpha
    /// row-major at the image size.
    #[test]
    fn alpha_channel_is_split_off() {
        let d = decode(&fixture("rgba_8x6.jp2"), false, None).unwrap();
        assert_eq!((d.width, d.height, d.ncomp), (8, 6, 3));
        let alpha = d.alpha.expect("alpha channel");
        assert_eq!(alpha.len(), 48);
        for y in 0..6 {
            for x in 0..8 {
                assert_eq!(alpha[y * 8 + x], ((x * 36) % 256) as u8, "({x},{y})");
                assert_eq!(d.data[(y * 8 + x) * 3 + 2], 128, "({x},{y})");
            }
        }
    }

    /// A target resolution lets the decoder stop a level early; the samples
    /// match the size it reports.
    #[test]
    fn reduced_decode_reports_its_own_size() {
        let full = decode(&fixture("gray_12x9.jp2"), false, None).unwrap();
        let small = decode(&fixture("gray_12x9.jp2"), false, Some((6, 4))).unwrap();
        assert_eq!(small.data.len(), small.width * small.height * small.ncomp);
        assert!(small.width <= full.width && small.height <= full.height);
        assert!(
            small.width >= 3 && small.height >= 2,
            "{}x{}",
            small.width,
            small.height
        );
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(decode(b"not a jpx", false, None).is_err());
        assert!(decode(&[], false, None).is_err());
        let mut truncated = fixture("gray_12x9.jp2");
        truncated.truncate(60);
        let _ = decode(&truncated, false, None); // either way, no panic
    }
}
