//! The shared libjpeg-exact decoder ([`docling_core::jpeg`]) against
//! Pillow's libjpeg-turbo on `tests/data/jpeg/` (the fixtures and the `image`
//! crate that reads the reference PNGs live here, not in docling-core).

use docling_core::jpeg::{decode, info};

fn fixtures() -> Vec<(String, Vec<u8>, image::DynamicImage)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/jpeg");
    let mut out = Vec::new();
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "jpg"))
        .collect();
    names.sort();
    for jpg in names {
        let png = jpg.with_extension("png");
        let reference = image::open(&png).unwrap_or_else(|e| panic!("{}: {e}", png.display()));
        out.push((
            jpg.file_stem().unwrap().to_string_lossy().into_owned(),
            std::fs::read(&jpg).unwrap(),
            reference,
        ));
    }
    assert!(out.len() >= 8, "fixtures missing");
    out
}

/// Every fixture decodes to the bytes Pillow's libjpeg-turbo produced
/// (islow IDCT, fancy upsampling, fixed-point colour conversion): gray
/// and RGB, 4:4:4 / 4:2:2 / 4:2:0, baseline and progressive, restart
/// intervals, odd sizes.
#[test]
fn matches_libjpeg_on_the_fixtures() {
    let mut failures = Vec::new();
    for (name, jpg, reference) in fixtures() {
        let img = decode(&jpg, true, 1).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let want: Vec<u8> = if img.channels == 1 {
            reference.to_luma8().into_raw()
        } else {
            reference.to_rgb8().into_raw()
        };
        assert_eq!(
            (img.width, img.height),
            (reference.width() as usize, reference.height() as usize),
            "{name}: size"
        );
        if img.data != want {
            let diff = img.data.iter().zip(&want).filter(|(a, b)| a != b).count();
            let max = img
                .data
                .iter()
                .zip(&want)
                .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
                .max()
                .unwrap_or(0);
            failures.push(format!(
                "{name}: {diff} of {} bytes differ, max |Δ| {max}",
                want.len()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn info_reads_the_frame_header() {
    let (_, jpg, reference) = fixtures()
        .into_iter()
        .find(|(n, _, _)| n == "rgb_420")
        .unwrap();
    let i = info(&jpg).unwrap();
    assert_eq!(
        (i.width, i.height, i.components),
        (reference.width() as usize, reference.height() as usize, 3)
    );
}

/// Reduced-scale decoding: the output is `ceil(w/N)` × `ceil(h/N)`, and
/// every reduced pixel is close to the mean of the full-size pixels it
/// stands for — over the *whole* image, on every fixture (grayscale,
/// progressive and restart-interval streams decode their scans one
/// component at a time, whose block grid is the coded size, not the
/// scaled one: a reduced grayscale scan once came back three-quarters
/// black).
#[test]
fn reduced_scales_have_the_right_size_and_content() {
    for (name, jpg, _) in fixtures() {
        let full = decode(&jpg, true, 1).unwrap();
        for denom in [2usize, 4, 8] {
            let img = decode(&jpg, true, denom as u32).unwrap();
            assert_eq!(img.width, full.width.div_ceil(denom), "{name} 1/{denom}");
            assert_eq!(img.height, full.height.div_ceil(denom), "{name} 1/{denom}");
            assert_eq!(img.channels, full.channels, "{name} 1/{denom}");
            let ch = img.channels;
            let mut worst = 0i32;
            for oy in 0..img.height {
                for ox in 0..img.width {
                    for c in 0..ch {
                        let (mut sum, mut n) = (0u32, 0u32);
                        for y in oy * denom..((oy + 1) * denom).min(full.height) {
                            for x in ox * denom..((ox + 1) * denom).min(full.width) {
                                sum += u32::from(full.data[(y * full.width + x) * ch + c]);
                                n += 1;
                            }
                        }
                        let mean = (sum / n.max(1)) as i32;
                        let got = i32::from(img.data[(oy * img.width + ox) * ch + c]);
                        worst = worst.max((got - mean).abs());
                    }
                }
            }
            // A reduced IDCT is a low-pass of the block, not a box mean:
            // sharp edges may sit a few dozen levels off the mean, but a
            // missing block (black) is 100+ off on any real image.
            assert!(
                worst < 96,
                "{name} 1/{denom}: worst |Δ| {worst} vs the block mean"
            );
        }
    }
}
