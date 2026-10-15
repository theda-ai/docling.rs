//! Colour spaces (ISO 32000-1, 8.6) → device RGB in `[0, 1]`.
//!
//! CMYK follows docling-parse's `color::cmyk_to_rgb` (`parse/utils/color/
//! device_cmyk.h`): a Yule–Nielsen-corrected Neugebauer model fitted to a
//! coated-stock press, not the naive `1 − min(1, c + k)`. It makes `0 0 0 1 k`
//! text a warm near-black (35, 31, 32) rather than pure black, which is what
//! the reference renders show; the layout model's inputs are compared against
//! those renders, so the same numbers are used here.

use std::collections::HashMap;

use lopdf::{Dictionary, Document, Object};

use super::function::Function;
use super::objects::{as_dict, as_stream, deref, get, get_int, name, nums, resource};

#[derive(Debug, Clone)]
pub enum ColorSpace {
    DeviceGray,
    DeviceRGB,
    DeviceCMYK,
    /// CIE L*a*b* with its white point and `/Range`.
    Lab {
        wp: [f64; 3],
        range: [f64; 4],
    },
    Indexed {
        base: Box<ColorSpace>,
        hival: usize,
        lookup: Vec<u8>,
    },
    /// Separation / DeviceN: `n` tints through a function into `alt`.
    Separation {
        n: usize,
        alt: Box<ColorSpace>,
        tint: Option<Function>,
        /// `/None`: paints nothing.
        is_none: bool,
        /// `/All`: paints every colorant — black at full tint.
        is_all: bool,
    },
    /// A pattern colour space, with the base space of an uncoloured (type 2)
    /// tiling pattern's colour operands.
    Pattern(Option<Box<ColorSpace>>),
}

impl ColorSpace {
    pub fn components(&self) -> usize {
        match self {
            ColorSpace::DeviceGray => 1,
            ColorSpace::DeviceRGB => 3,
            ColorSpace::DeviceCMYK => 4,
            ColorSpace::Lab { .. } => 3,
            ColorSpace::Indexed { .. } => 1,
            ColorSpace::Separation { n, .. } => *n,
            ColorSpace::Pattern(_) => 1,
        }
    }

    /// The initial colour when the space is selected (`cs`): black, or all
    /// colorants at 1.0 for Separation/DeviceN.
    pub fn initial(&self) -> Vec<f64> {
        match self {
            ColorSpace::DeviceCMYK => vec![0.0, 0.0, 0.0, 1.0],
            ColorSpace::Separation { n, .. } => vec![1.0; *n],
            ColorSpace::Lab { .. } => vec![0.0, 0.0, 0.0],
            ColorSpace::Indexed { .. } => vec![0.0],
            other => vec![0.0; other.components()],
        }
    }

    /// The default `/Decode` range of image samples in this space for `bpc`
    /// bits: `[0 1]` per component, `[0 2ᵇᵖᶜ−1]` for Indexed, the `/Range`
    /// for Lab.
    pub fn default_decode(&self, bpc: u32) -> Vec<f64> {
        match self {
            ColorSpace::Indexed { .. } => vec![0.0, ((1u64 << bpc) - 1) as f64],
            ColorSpace::Lab { range, .. } => {
                vec![0.0, 100.0, range[0], range[1], range[2], range[3]]
            }
            other => (0..other.components()).flat_map(|_| [0.0, 1.0]).collect(),
        }
    }

    /// Resolve a colour space object; `res` is the resources dictionary named
    /// spaces are looked up in.
    pub fn parse(doc: &Document, obj: &Object, res: Option<&Dictionary>) -> Option<ColorSpace> {
        Self::parse_depth(doc, obj, res, 0)
    }

    fn parse_depth(
        doc: &Document,
        obj: &Object,
        res: Option<&Dictionary>,
        depth: usize,
    ) -> Option<ColorSpace> {
        if depth > 6 {
            return None;
        }
        match deref(doc, obj) {
            Object::Name(n) => match n.as_slice() {
                b"DeviceGray" | b"G" | b"CalGray" => Some(ColorSpace::DeviceGray),
                b"DeviceRGB" | b"RGB" | b"CalRGB" => Some(ColorSpace::DeviceRGB),
                b"DeviceCMYK" | b"CMYK" => Some(ColorSpace::DeviceCMYK),
                b"Pattern" => Some(ColorSpace::Pattern(None)),
                b"Indexed" | b"I" => None,
                other => {
                    let o = resource(doc, res, b"ColorSpace", other)?;
                    // A named resource is looked up once, without the resources
                    // again (a self-referencing `/CS0 /CS0` would loop).
                    Self::parse_depth(doc, o, None, depth + 1)
                }
            },
            Object::Array(a) => {
                let fam = a.first().map(|o| deref(doc, o)).and_then(name)?;
                match fam {
                    b"DeviceGray" | b"G" | b"CalGray" => Some(ColorSpace::DeviceGray),
                    b"DeviceRGB" | b"RGB" | b"CalRGB" => Some(ColorSpace::DeviceRGB),
                    b"DeviceCMYK" | b"CMYK" => Some(ColorSpace::DeviceCMYK),
                    b"Lab" => {
                        let d = a.get(1).and_then(|o| as_dict(doc, o));
                        let wp = d
                            .and_then(|d| get(doc, d, b"WhitePoint"))
                            .and_then(|o| nums(doc, o))
                            .filter(|v| v.len() == 3)
                            .map(|v| [v[0], v[1], v[2]])
                            .unwrap_or([0.9505, 1.0, 1.089]);
                        let range = d
                            .and_then(|d| get(doc, d, b"Range"))
                            .and_then(|o| nums(doc, o))
                            .filter(|v| v.len() == 4)
                            .map(|v| [v[0], v[1], v[2], v[3]])
                            .unwrap_or([-100.0, 100.0, -100.0, 100.0]);
                        Some(ColorSpace::Lab { wp, range })
                    }
                    b"ICCBased" => {
                        let s = a.get(1).and_then(|o| as_stream(doc, o));
                        let n = s.and_then(|s| get_int(doc, &s.dict, b"N"));
                        match n {
                            Some(1) => Some(ColorSpace::DeviceGray),
                            Some(4) => Some(ColorSpace::DeviceCMYK),
                            Some(3) => Some(ColorSpace::DeviceRGB),
                            _ => s
                                .and_then(|s| s.dict.get(b"Alternate").ok())
                                .and_then(|o| Self::parse_depth(doc, o, res, depth + 1))
                                .or(Some(ColorSpace::DeviceRGB)),
                        }
                    }
                    b"Indexed" | b"I" => {
                        if a.len() < 4 {
                            return None;
                        }
                        let base = Self::parse_depth(doc, &a[1], res, depth + 1)?;
                        let hival = deref(doc, &a[2]).as_i64().unwrap_or(0).clamp(0, 255) as usize;
                        let lookup = match deref(doc, &a[3]) {
                            Object::String(s, _) => s.clone(),
                            Object::Stream(s) => s.decompressed_content().ok()?,
                            _ => return None,
                        };
                        Some(ColorSpace::Indexed {
                            base: Box::new(base),
                            hival,
                            lookup,
                        })
                    }
                    b"Separation" | b"DeviceN" => {
                        let (n, names): (usize, Vec<Vec<u8>>) = match deref(doc, a.get(1)?) {
                            Object::Name(nm) => (1, vec![nm.clone()]),
                            Object::Array(arr) => (
                                arr.len().max(1),
                                arr.iter()
                                    .filter_map(|o| name(deref(doc, o)).map(|n| n.to_vec()))
                                    .collect(),
                            ),
                            _ => (1, Vec::new()),
                        };
                        let alt = a
                            .get(2)
                            .and_then(|o| Self::parse_depth(doc, o, res, depth + 1))
                            .unwrap_or(ColorSpace::DeviceGray);
                        let tint = a.get(3).and_then(|o| Function::parse(doc, o));
                        let is_none =
                            fam == b"Separation" && names.first().is_some_and(|n| n == b"None");
                        let is_all =
                            fam == b"Separation" && names.first().is_some_and(|n| n == b"All");
                        Some(ColorSpace::Separation {
                            n,
                            alt: Box::new(alt),
                            tint,
                            is_none,
                            is_all,
                        })
                    }
                    b"Pattern" => {
                        let base = a
                            .get(1)
                            .and_then(|o| Self::parse_depth(doc, o, res, depth + 1))
                            .map(Box::new);
                        Some(ColorSpace::Pattern(base))
                    }
                    b"DeviceRGBA" => Some(ColorSpace::DeviceRGB),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Component values → RGB in `[0, 1]`. `None` for `/Separation /None`
    /// (nothing is painted).
    pub fn to_rgb(&self, v: &[f64]) -> Option<[f64; 3]> {
        let c = |i: usize| v.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        match self {
            ColorSpace::DeviceGray => {
                let g = c(0);
                Some([g, g, g])
            }
            ColorSpace::DeviceRGB => Some([c(0), c(1), c(2)]),
            ColorSpace::DeviceCMYK => Some(cmyk_to_rgb(c(0), c(1), c(2), c(3))),
            ColorSpace::Lab { wp, range } => {
                let l = v.first().copied().unwrap_or(0.0).clamp(0.0, 100.0);
                let a = v.get(1).copied().unwrap_or(0.0).clamp(range[0], range[1]);
                let b = v.get(2).copied().unwrap_or(0.0).clamp(range[2], range[3]);
                Some(lab_to_rgb(l, a, b, wp))
            }
            ColorSpace::Indexed {
                base,
                hival,
                lookup,
            } => {
                let idx = v.first().copied().unwrap_or(0.0).round().max(0.0) as usize;
                let idx = idx.min(*hival);
                let n = base.components();
                let comps: Vec<f64> = (0..n)
                    .map(|i| f64::from(*lookup.get(idx * n + i).unwrap_or(&0)) / 255.0)
                    .collect();
                // Lab lookups are byte-scaled over the range, not 0..1.
                let comps = match &**base {
                    ColorSpace::Lab { range, .. } => vec![
                        comps[0] * 100.0,
                        range[0] + comps.get(1).copied().unwrap_or(0.0) * (range[1] - range[0]),
                        range[2] + comps.get(2).copied().unwrap_or(0.0) * (range[3] - range[2]),
                    ],
                    _ => comps,
                };
                base.to_rgb(&comps)
            }
            ColorSpace::Separation {
                n,
                alt,
                tint,
                is_none,
                is_all,
            } => {
                if *is_none {
                    return None;
                }
                if *is_all {
                    let g = 1.0 - c(0);
                    return Some([g, g, g]);
                }
                match tint {
                    Some(f) => {
                        let inputs: Vec<f64> = (0..*n).map(c).collect();
                        let out = f.eval(&inputs);
                        alt.to_rgb(&out)
                    }
                    // No usable tint transform: treat the first colorant as
                    // an ink coverage of gray.
                    None => {
                        let g = 1.0 - c(0);
                        Some([g, g, g])
                    }
                }
            }
            ColorSpace::Pattern(_) => Some([0.0, 0.0, 0.0]),
        }
    }
}

/// CIE L*a*b* (D50-ish white point `wp`) → sRGB.
fn lab_to_rgb(l: f64, a: f64, b: f64, wp: &[f64; 3]) -> [f64; 3] {
    let m = (l + 16.0) / 116.0;
    let ll = m + a / 500.0;
    let n = m - b / 200.0;
    let g = |x: f64| {
        if x >= 6.0 / 29.0 {
            x * x * x
        } else {
            108.0 / 841.0 * (x - 4.0 / 29.0)
        }
    };
    let x = wp[0] * g(ll);
    let y = wp[1] * g(m);
    let z = wp[2] * g(n);
    // XYZ → linear sRGB (D65 matrix; the white-point mismatch is small).
    let r = 3.2406 * x - 1.5372 * y - 0.4986 * z;
    let gg = -0.9689 * x + 1.8758 * y + 0.0415 * z;
    let bb = 0.0557 * x - 0.2040 * y + 1.0570 * z;
    [linear_to_srgb(r), linear_to_srgb(gg), linear_to_srgb(bb)]
}

fn srgb_to_linear(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(v: f64) -> f64 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.0031308 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

struct CmykModel {
    encoded_positive: [[f64; 3]; 8],
    negative: [[f64; 3]; 8],
    yule_nielsen: [f64; 3],
    ink_tone: [f64; 3],
    black: [[f64; 3]; 9],
}

fn cmyk_model() -> &'static CmykModel {
    static MODEL: std::sync::OnceLock<CmykModel> = std::sync::OnceLock::new();
    MODEL.get_or_init(|| {
        let yule_nielsen = [1.553623, 1.348579, 1.536033];
        let corner: [[f64; 3]; 8] = [
            [1.000000, 1.000000, 1.000000],  // paper
            [-0.179803, 0.419937, 0.868289], // C
            [0.869084, -0.001406, 0.268871], // M
            [0.039807, 0.023510, 0.277328],  // C + M
            [1.052223, 0.853703, 0.007227],  // Y
            [-0.074325, 0.403406, 0.078689], // C + Y
            [0.879638, 0.012292, 0.011953],  // M + Y
            [0.043065, 0.031199, 0.032550],  // C + M + Y
        ];
        let mut encoded_positive = [[0.0; 3]; 8];
        let mut negative = [[0.0; 3]; 8];
        for i in 0..8 {
            for j in 0..3 {
                let v = corner[i][j];
                encoded_positive[i][j] = v.max(0.0).powf(1.0 / yule_nielsen[j]);
                negative[i][j] = v.min(0.0);
            }
        }
        let ramp: [[f64; 3]; 9] = [
            [255.0, 255.0, 255.0],
            [225.0, 226.0, 228.0],
            [199.0, 200.0, 202.0],
            [173.0, 174.0, 178.0],
            [147.0, 149.0, 152.0],
            [123.0, 125.0, 128.0],
            [99.0, 99.0, 102.0],
            [69.0, 70.0, 71.0],
            [35.0, 31.0, 32.0],
        ];
        let mut black = [[0.0; 3]; 9];
        for i in 0..9 {
            for j in 0..3 {
                black[i][j] = srgb_to_linear(ramp[i][j] / 255.0);
            }
        }
        CmykModel {
            encoded_positive,
            negative,
            yule_nielsen,
            ink_tone: [1.176647, 1.212366, 1.145622],
            black,
        }
    })
}

/// docling-parse's `color::cmyk_to_rgb`, inputs and outputs in `[0, 1]`.
pub fn cmyk_to_rgb(c: f64, m: f64, y: f64, k: f64) -> [f64; 3] {
    let model = cmyk_model();
    let clamp = |v: f64| v.clamp(0.0, 1.0);
    let area = [
        clamp(c).powf(1.0 / model.ink_tone[0]),
        clamp(m).powf(1.0 / model.ink_tone[1]),
        clamp(y).powf(1.0 / model.ink_tone[2]),
    ];
    let mut weight = [0.0; 8];
    for (i, w) in weight.iter_mut().enumerate() {
        *w = (if i & 1 != 0 { area[0] } else { 1.0 - area[0] })
            * (if i & 2 != 0 { area[1] } else { 1.0 - area[1] })
            * (if i & 4 != 0 { area[2] } else { 1.0 - area[2] });
    }
    let kx = clamp(k) * 8.0;
    let k0 = (kx as usize).min(7);
    let kf = kx - k0 as f64;
    let mut out = [0.0; 3];
    for (j, o) in out.iter_mut().enumerate() {
        let mut mixed = 0.0;
        let mut offset = 0.0;
        for (i, wt) in weight.iter().enumerate() {
            mixed += wt * model.encoded_positive[i][j];
            offset += wt * model.negative[i][j];
        }
        let linear = mixed.max(0.0).powf(model.yule_nielsen[j]) + offset;
        let transmittance = model.black[k0][j] * (1.0 - kf) + model.black[k0 + 1][j] * kf;
        *o = linear_to_srgb(linear * transmittance);
    }
    out
}

/// A per-document cache of CMYK → RGB bytes (the conversion is nine `powf`
/// calls; photographic CMYK images call it per pixel).
#[derive(Default)]
pub struct CmykCache {
    map: HashMap<u32, [u8; 3]>,
}

impl CmykCache {
    pub fn rgb8(&mut self, c: u8, m: u8, y: u8, k: u8) -> [u8; 3] {
        let key = u32::from_be_bytes([c, m, y, k]);
        if let Some(v) = self.map.get(&key) {
            return *v;
        }
        let rgb = cmyk_to_rgb(
            f64::from(c) / 255.0,
            f64::from(m) / 255.0,
            f64::from(y) / 255.0,
            f64::from(k) / 255.0,
        );
        let out = [to_u8(rgb[0]), to_u8(rgb[1]), to_u8(rgb[2])];
        if self.map.len() < 1 << 16 {
            self.map.insert(key, out);
        }
        out
    }
}

/// `lround(255 · v)` clamped.
pub fn to_u8(v: f64) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmyk_black_is_docling_parses_warm_black() {
        let k = cmyk_to_rgb(0.0, 0.0, 0.0, 1.0);
        assert_eq!([to_u8(k[0]), to_u8(k[1]), to_u8(k[2])], [35, 31, 32]);
        let w = cmyk_to_rgb(0.0, 0.0, 0.0, 0.0);
        assert_eq!([to_u8(w[0]), to_u8(w[1]), to_u8(w[2])], [255, 255, 255]);
        let c = cmyk_to_rgb(1.0, 0.0, 0.0, 0.0);
        assert!(c[2] > c[0], "cyan is blue-ish: {c:?}");
    }

    #[test]
    fn lab_white_and_black() {
        let w = lab_to_rgb(100.0, 0.0, 0.0, &[0.9505, 1.0, 1.089]);
        assert!(w.iter().all(|v| *v > 0.97), "{w:?}");
        let k = lab_to_rgb(0.0, 0.0, 0.0, &[0.9505, 1.0, 1.089]);
        assert!(k.iter().all(|v| *v < 0.02), "{k:?}");
    }
}
