//! pdfium's `CStretchEngine` (core/fxge/dib/cstretchengine.cpp), ported so an
//! embedded image can be scaled to the very bytes pdfium draws.
//!
//! pdfium resamples every axis-aligned image through this engine: a weight
//! table per axis — area coverage when shrinking, two-tap linear
//! interpolation when enlarging, nearest when smoothing is off — in 16.16
//! fixed point, a horizontal pass into an 8-bit intermediate buffer over the
//! source rows the clip needs, then a vertical pass over that buffer. Every
//! rounding of the original is kept: the weights are `round(w · 65536)` with
//! the running rounding error carried into the next tap and the last tap
//! taking whatever is left of 65536 (unsigned, wrapping), each pass truncates
//! its 32-bit accumulator with `>> 16`, and the source and destination clips
//! are computed from a *float* scale. Negative destination sizes flip the
//! axis, as they do in pdfium (`CFX_AggImageRenderer` negates `dest_width`
//! for `a < 0` and `dest_height` for `d > 0`).
//!
//! The port stays in the integer/float types of the original — `u32`
//! wrapping arithmetic, `f64` weights from an `f32` scale — because the
//! result is compared byte for byte with pdfium's bitmap
//! (`raster::tests::matches_pdfium_on_the_scanned_fixtures`).

/// `FXDIB_ResampleOptions` — the two flags that reach the weight table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// `bInterpolateBilinear`: two-tap interpolation when enlarging (the
    /// image's `/Interpolate`, or pdfium's own heuristic below).
    pub bilinear: bool,
    /// `bNoSmoothing`: nearest neighbour (`FPDF_RENDER_NO_SMOOTHIMAGE`).
    pub no_smoothing: bool,
}

/// pdfium's `FX_RECT`: integer, `left/top` inclusive, `right/bottom`
/// exclusive, y down.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }

    pub fn is_empty(&self) -> bool {
        self.left >= self.right || self.top >= self.bottom
    }

    pub fn normalize(&mut self) {
        if self.left > self.right {
            std::mem::swap(&mut self.left, &mut self.right);
        }
        if self.top > self.bottom {
            std::mem::swap(&mut self.top, &mut self.bottom);
        }
    }

    /// `FX_RECT::Intersect`: both normalized; an empty result is all zeros.
    pub fn intersect(&mut self, other: &Rect) {
        let mut o = *other;
        o.normalize();
        self.normalize();
        self.left = self.left.max(o.left);
        self.top = self.top.max(o.top);
        self.right = self.right.min(o.right);
        self.bottom = self.bottom.min(o.bottom);
        if self.left > self.right || self.top > self.bottom {
            *self = Rect::default();
        }
    }

    pub fn offset(&mut self, dx: i32, dy: i32) {
        self.left += dx;
        self.right += dx;
        self.top += dy;
        self.bottom += dy;
    }

    /// `FX_RECT::SwappedClipBox`: the clip of a 90°-rotated placement, in the
    /// coordinates of the un-rotated stretch (`width`/`height` are the
    /// rotated image's device size).
    pub fn swapped_clip_box(&self, width: i32, height: i32, flip_x: bool, flip_y: bool) -> Rect {
        let mut r = Rect::default();
        if flip_y {
            r.left = height - self.top;
            r.right = height - self.bottom;
        } else {
            r.left = self.top;
            r.right = self.bottom;
        }
        if flip_x {
            r.top = width - self.left;
            r.bottom = width - self.right;
        } else {
            r.top = self.left;
            r.bottom = self.right;
        }
        r.normalize();
        r
    }
}

/// The source bitmap, as pdfium's `CPDF_DIB` hands it to the engine.
#[derive(Clone, Copy, Debug)]
pub enum Source<'a> {
    /// 1 bit per pixel, most significant bit first, `stride` bytes per row —
    /// pdfium's `k1bppRgb`, expanded per row to 0/255 (`Expand1BppRow`) and
    /// stretched as 8-bit; a palette, when the image has one, is applied to
    /// the result by the caller (`CFX_ImageStretcher` builds the 256-entry
    /// gradient for the destination bitmap).
    Bilevel { data: &'a [u8], stride: usize },
    /// 8-bit gray, no palette (`k8bppRgb`).
    Gray8 { data: &'a [u8], stride: usize },
    /// 8-bit, three channels (`kBgr`; channel order is irrelevant to the
    /// arithmetic, so the caller may pass RGB).
    Rgb8 { data: &'a [u8], stride: usize },
}

impl Source<'_> {
    fn channels(&self) -> usize {
        match self {
            Source::Bilevel { .. } | Source::Gray8 { .. } => 1,
            Source::Rgb8 { .. } => 3,
        }
    }
}

/// The stretched pixels of the destination clip: `height` rows of `width`
/// pixels, `channels` bytes each.
#[derive(Debug, Clone)]
pub struct Stretched {
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    pub data: Vec<u8>,
}

const FIXED_BITS: u32 = 16;
const FIXED_ONE: u32 = 1 << FIXED_BITS;

/// `FixedFromDouble`: `FXSYS_round(d * 65536)` as an unsigned 32-bit value.
fn fixed_from_double(d: f64) -> u32 {
    (d * f64::from(FIXED_ONE)).round() as i64 as u32
}

/// `PixelFromFixed`: truncate the accumulator.
fn pixel_from_fixed(fixed: u32) -> u8 {
    (fixed >> FIXED_BITS) as u8
}

/// One destination pixel's taps: the inclusive source range and a weight per
/// position (`weights[j - src_start]`).
struct PixelWeight {
    src_start: i32,
    src_end: i32,
    weights: Vec<u32>,
}

impl PixelWeight {
    fn taps(&self) -> &[u32] {
        if self.src_end < self.src_start {
            &[]
        } else {
            &self.weights[..(self.src_end - self.src_start + 1) as usize]
        }
    }
}

/// `CStretchEngine::WeightTable::CalculateWeights`.
fn calculate_weights(
    dest_len: i32,
    dest_min: i32,
    dest_max: i32,
    src_len: i32,
    src_min: i32,
    src_max: i32,
    options: Options,
) -> Option<Vec<PixelWeight>> {
    let bilinear = options.bilinear;
    if dest_len == 0 {
        return Some(Vec::new());
    }
    if dest_min > dest_max {
        return None;
    }
    let scale = f64::from(src_len) / f64::from(dest_len);
    let base = if dest_len < 0 {
        f64::from(src_len)
    } else {
        0.0
    };
    let weight_count = (scale.abs().ceil() as usize) + 1;
    let mut table = Vec::with_capacity((dest_max - dest_min) as usize);
    if options.no_smoothing || scale.abs() < 1.0 {
        for dest_pixel in dest_min..dest_max {
            let src_pos = f64::from(dest_pixel) * scale + scale / 2.0 + base;
            let mut weights = vec![0u32; weight_count.max(2)];
            let (src_start, src_end);
            if bilinear {
                let mut start = (src_pos - 0.5).floor() as i32;
                let mut end = (src_pos + 0.5).floor() as i32;
                start = start.max(src_min);
                end = end.min(src_max - 1);
                src_start = start;
                src_end = end;
                if start >= end {
                    weights[0] = FIXED_ONE;
                } else {
                    let second = fixed_from_double(src_pos - f64::from(start) - 0.5);
                    weights[0] = FIXED_ONE.wrapping_sub(second);
                    weights[1] = second;
                }
            } else {
                let pixel_pos = src_pos.floor() as i32;
                src_start = pixel_pos.max(src_min);
                src_end = pixel_pos.min(src_max - 1);
                weights[0] = FIXED_ONE;
            }
            table.push(PixelWeight {
                src_start,
                src_end,
                weights,
            });
        }
        return Some(table);
    }

    for dest_pixel in dest_min..dest_max {
        let src_start = f64::from(dest_pixel) * scale + base;
        let src_end = src_start + scale;
        let mut start_i = src_start.min(src_end).floor() as i32;
        let mut end_i = src_start.max(src_end).floor() as i32;
        start_i = start_i.max(src_min);
        end_i = end_i.min(src_max - 1);
        if start_i > end_i {
            start_i = start_i.clamp(0, (src_max - 1).max(0));
            table.push(PixelWeight {
                src_start: start_i,
                src_end: start_i,
                weights: vec![0; weight_count.max(1)],
            });
            continue;
        }
        let mut weights = vec![0u32; ((end_i - start_i) as usize + 1).max(weight_count)];
        let mut remaining = FIXED_ONE;
        let mut rounding_error = 0.0f64;
        for j in start_i..end_i {
            let mut dest_start = (f64::from(j) - base) / scale;
            let mut dest_end = (f64::from(j + 1) - base) / scale;
            if dest_start > dest_end {
                std::mem::swap(&mut dest_start, &mut dest_end);
            }
            let area_start = dest_start.max(f64::from(dest_pixel));
            let area_end = dest_end.min(f64::from(dest_pixel + 1));
            let weight = (area_end - area_start).max(0.0);
            let fixed_weight = fixed_from_double(weight + rounding_error);
            weights[(j - start_i) as usize] = fixed_weight;
            remaining = remaining.wrapping_sub(fixed_weight);
            rounding_error = weight - f64::from(fixed_weight) / f64::from(FIXED_ONE);
        }
        let mut src_end = end_i;
        // Unsigned underflow is defined behaviour in the original and lands
        // in the `RemoveLastWeightAndAdjust` branch.
        if remaining != 0 && remaining <= FIXED_ONE {
            weights[(end_i - start_i) as usize] = remaining;
        } else {
            if src_end <= start_i {
                return None;
            }
            src_end -= 1;
            let idx = (src_end - start_i) as usize;
            weights[idx] = weights[idx].wrapping_add(remaining);
        }
        table.push(PixelWeight {
            src_start: start_i,
            src_end,
            weights,
        });
    }
    Some(table)
}

/// `CStretchEngine::UseInterpolateBilinear`.
fn use_interpolate_bilinear(
    options: Options,
    dest_width: i32,
    dest_height: i32,
    src_width: i32,
    src_height: i32,
) -> bool {
    !options.bilinear
        && !options.no_smoothing
        && dest_width != 0
        && i64::from(dest_height.abs() / 8)
            < i64::from(src_width) * i64::from(src_height) / i64::from(dest_width.abs())
}

/// Stretch `src` (`src_width` × `src_height`) to `dest_width` × `dest_height`
/// (negative = flipped on that axis) and return the pixels inside `clip`
/// (destination coordinates, before any flip) — `CStretchEngine` end to end,
/// horizontal pass then vertical pass. `None` where pdfium would produce no
/// bitmap (empty dimensions, or a table it refuses).
pub fn stretch(
    src: &Source<'_>,
    src_width: i32,
    src_height: i32,
    dest_width: i32,
    dest_height: i32,
    clip: Rect,
    options: Options,
) -> Option<Stretched> {
    if dest_width == 0 || dest_height == 0 || clip.is_empty() || src_width <= 0 || src_height <= 0 {
        return None;
    }
    let channels = src.channels();
    let resample = if options.no_smoothing {
        Options {
            bilinear: false,
            no_smoothing: true,
        }
    } else if use_interpolate_bilinear(options, dest_width, dest_height, src_width, src_height) {
        Options {
            bilinear: true,
            no_smoothing: false,
        }
    } else {
        options
    };
    // The source clip is derived from a *float* scale (`static_cast<float>`
    // of the source size divided by the int destination size).
    let scale_x = f64::from(src_width as f32 / dest_width as f32);
    let scale_y = f64::from(src_height as f32 / dest_height as f32);
    let base_x = if dest_width > 0 {
        0.0
    } else {
        f64::from(dest_width)
    };
    let base_y = if dest_height > 0 {
        0.0
    } else {
        f64::from(dest_height)
    };
    let mut src_left = scale_x * (f64::from(clip.left) + base_x);
    let mut src_right = scale_x * (f64::from(clip.right) + base_x);
    let mut src_top = scale_y * (f64::from(clip.top) + base_y);
    let mut src_bottom = scale_y * (f64::from(clip.bottom) + base_y);
    if src_left > src_right {
        std::mem::swap(&mut src_left, &mut src_right);
    }
    if src_top > src_bottom {
        std::mem::swap(&mut src_top, &mut src_bottom);
    }
    let mut src_clip = Rect::new(
        src_left.floor() as i32,
        src_top.floor() as i32,
        src_right.ceil() as i32,
        src_bottom.ceil() as i32,
    );
    src_clip.intersect(&Rect::new(0, 0, src_width, src_height));
    if src_clip.is_empty() {
        return None;
    }

    // Horizontal pass: every source row of the clip → `dest_cols` pixels.
    let dest_cols = clip.width() as usize;
    let dest_rows = clip.height() as usize;
    let horz = calculate_weights(
        dest_width,
        clip.left,
        clip.right,
        src_width,
        src_clip.left,
        src_clip.right,
        resample,
    )?;
    let inter_pitch = dest_cols * channels;
    let src_rows = src_clip.height() as usize;
    let mut inter = vec![0u8; inter_pitch * src_rows];
    let mut expanded = Vec::new();
    for (r, row) in (src_clip.top..src_clip.bottom).zip(inter.chunks_exact_mut(inter_pitch)) {
        let r = r as usize;
        let src_row: &[u8] = match src {
            Source::Bilevel { data, stride } => {
                let bytes = data.get(r * stride..(r * stride + stride))?;
                expanded.clear();
                expanded.reserve(stride * 8);
                for &b in bytes {
                    for bit in 0..8 {
                        expanded.push(if (b >> (7 - bit)) & 1 == 1 { 255 } else { 0 });
                    }
                }
                &expanded
            }
            Source::Gray8 { data, stride } => {
                data.get(r * stride..r * stride + src_width as usize)?
            }
            Source::Rgb8 { data, stride } => {
                data.get(r * stride..r * stride + 3 * src_width as usize)?
            }
        };
        for (pw, dest) in horz.iter().zip(row.chunks_exact_mut(channels)) {
            let taps = pw.taps();
            let start = pw.src_start as usize;
            for (c, d) in dest.iter_mut().enumerate() {
                let mut acc: u32 = 0;
                for (k, &w) in taps.iter().enumerate() {
                    let v = *src_row.get((start + k) * channels + c)?;
                    acc = acc.wrapping_add(w.wrapping_mul(u32::from(v)));
                }
                *d = pixel_from_fixed(acc);
            }
        }
    }

    // Vertical pass over the intermediate buffer.
    let vert = calculate_weights(
        dest_height,
        clip.top,
        clip.bottom,
        src_height,
        src_clip.top,
        src_clip.bottom,
        resample,
    )?;
    let mut out = vec![0u8; inter_pitch * dest_rows];
    let mut accum = vec![0u32; inter_pitch];
    for (pw, dest) in vert.iter().zip(out.chunks_exact_mut(inter_pitch)) {
        let taps = pw.taps();
        accum.fill(0);
        for (k, &w) in taps.iter().enumerate() {
            let src_row_index = (pw.src_start + k as i32 - src_clip.top) as usize;
            let row = inter.get(src_row_index * inter_pitch..(src_row_index + 1) * inter_pitch)?;
            for (a, &v) in accum.iter_mut().zip(row) {
                *a = a.wrapping_add(w.wrapping_mul(u32::from(v)));
            }
        }
        for (d, &a) in dest.iter_mut().zip(&accum) {
            *d = pixel_from_fixed(a);
        }
    }
    Some(Stretched {
        width: dest_cols,
        height: dest_rows,
        channels,
        data: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shrinking a 4-pixel row to 2 pixels: each destination pixel covers
    /// exactly two source pixels, so the area weights are 0.5 + 0.5 and the
    /// result is the truncated mean.
    #[test]
    fn shrink_by_two_averages_pairs() {
        let data = [10u8, 20, 30, 41];
        let out = stretch(
            &Source::Gray8 {
                data: &data,
                stride: 4,
            },
            4,
            1,
            2,
            1,
            Rect::new(0, 0, 2, 1),
            Options::default(),
        )
        .unwrap();
        assert_eq!(out.data, vec![15, 35]);
    }

    /// A negative destination width flips the axis: the same averages, in
    /// reverse order.
    #[test]
    fn negative_width_flips() {
        let data = [10u8, 20, 30, 41];
        let out = stretch(
            &Source::Gray8 {
                data: &data,
                stride: 4,
            },
            4,
            1,
            -2,
            1,
            Rect::new(0, 0, 2, 1),
            Options::default(),
        )
        .unwrap();
        assert_eq!(out.data, vec![35, 15]);
    }

    /// Enlarging 2 → 4 pixels: with `/Interpolate` (or pdfium's heuristic,
    /// which a 2-pixel source is too small to trigger) the two-tap filter
    /// interpolates between neighbours and clamps at the edges; without it
    /// pdfium picks the nearest source pixel.
    #[test]
    fn enlarge_interpolates() {
        let data = [0u8, 100];
        let src = Source::Gray8 {
            data: &data,
            stride: 2,
        };
        let bilinear = Options {
            bilinear: true,
            no_smoothing: false,
        };
        let out = stretch(&src, 2, 1, 4, 1, Rect::new(0, 0, 4, 1), bilinear).unwrap();
        // src_pos = 0.25, 0.75, 1.25, 1.75 → taps (0), (0,1 @ .25), (0,1 @ .75), (1)
        assert_eq!(out.data, vec![0, 25, 75, 100]);
        let out = stretch(&src, 2, 1, 4, 1, Rect::new(0, 0, 4, 1), Options::default()).unwrap();
        assert_eq!(out.data, vec![0, 0, 100, 100]);
    }

    #[test]
    fn bilevel_rows_expand_to_0_and_255() {
        let data = [0b1010_0000u8];
        let out = stretch(
            &Source::Bilevel {
                data: &data,
                stride: 1,
            },
            4,
            1,
            4,
            1,
            Rect::new(0, 0, 4, 1),
            Options::default(),
        )
        .unwrap();
        assert_eq!(out.data, vec![255, 0, 255, 0]);
    }

    #[test]
    fn swapped_clip_box_matches_pdfium() {
        let r = Rect::new(1, 2, 5, 9);
        let s = r.swapped_clip_box(10, 20, false, false);
        assert_eq!(s, Rect::new(2, 1, 9, 5));
        let s = r.swapped_clip_box(10, 20, true, true);
        assert_eq!(s, Rect::new(11, 5, 18, 9));
    }
}
