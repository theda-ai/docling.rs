//! Affine geometry for the renderer: PDF's `[a b c d e f]` matrices in `f64`,
//! applied as `x' = a·x + c·y + e`, `y' = b·x + d·y + f` (ISO 32000-1, 8.3.4).

use tiny_skia::Transform;

/// A PDF transformation matrix.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mat {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Mat {
    pub const IDENTITY: Mat = Mat {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Mat {
        Mat { a, b, c, d, e, f }
    }

    pub fn scale(sx: f64, sy: f64) -> Mat {
        Mat::new(sx, 0.0, 0.0, sy, 0.0, 0.0)
    }

    pub fn translate(tx: f64, ty: f64) -> Mat {
        Mat::new(1.0, 0.0, 0.0, 1.0, tx, ty)
    }

    /// From a six-element array, `None` unless every entry is finite.
    pub fn from_slice(v: &[f64]) -> Option<Mat> {
        if v.len() != 6 || v.iter().any(|x| !x.is_finite()) {
            return None;
        }
        Some(Mat::new(v[0], v[1], v[2], v[3], v[4], v[5]))
    }

    /// `self × r`: apply `self` first, then `r` (PDF's `cm` prepends the new
    /// matrix to the CTM: `new_ctm = m × ctm`).
    pub fn then(self, r: Mat) -> Mat {
        Mat {
            a: self.a * r.a + self.b * r.c,
            b: self.a * r.b + self.b * r.d,
            c: self.c * r.a + self.d * r.c,
            d: self.c * r.b + self.d * r.d,
            e: self.e * r.a + self.f * r.c + r.e,
            f: self.e * r.b + self.f * r.d + r.f,
        }
    }

    pub fn apply(self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// The linear part applied to a vector (no translation).
    pub fn apply_vec(self, x: f64, y: f64) -> (f64, f64) {
        (self.a * x + self.c * y, self.b * x + self.d * y)
    }

    pub fn det(self) -> f64 {
        self.a * self.d - self.b * self.c
    }

    /// `sqrt(|det|)`: the geometric mean scale — what docling-parse scales a
    /// line width by (`pdf_state<SHAPE>::trafo_scale`).
    pub fn mean_scale(self) -> f64 {
        self.det().abs().sqrt()
    }

    /// The larger singular value: the most a unit length can stretch.
    pub fn max_scale(self) -> f64 {
        let sx = self.a.hypot(self.b);
        let sy = self.c.hypot(self.d);
        sx.max(sy)
    }

    pub fn invert(self) -> Option<Mat> {
        let det = self.det();
        if det.abs() < 1e-12 || !det.is_finite() {
            return None;
        }
        let ia = self.d / det;
        let ib = -self.b / det;
        let ic = -self.c / det;
        let id = self.a / det;
        Some(Mat {
            a: ia,
            b: ib,
            c: ic,
            d: id,
            e: -(self.e * ia + self.f * ic),
            f: -(self.e * ib + self.f * id),
        })
    }

    pub fn is_finite(self) -> bool {
        [self.a, self.b, self.c, self.d, self.e, self.f]
            .iter()
            .all(|v| v.is_finite())
    }

    /// The same transform for tiny-skia (`f32`; `from_row(sx, ky, kx, sy, tx, ty)`
    /// takes the matrix in the same `a b c d e f` order).
    pub fn to_ts(self) -> Transform {
        Transform::from_row(
            self.a as f32,
            self.b as f32,
            self.c as f32,
            self.d as f32,
            self.e as f32,
            self.f as f32,
        )
    }

    /// The axis-aligned bounding box of the unit square under this matrix,
    /// `(x_min, y_min, x_max, y_max)`.
    pub fn unit_bbox(self) -> (f64, f64, f64, f64) {
        let pts = [
            self.apply(0.0, 0.0),
            self.apply(1.0, 0.0),
            self.apply(0.0, 1.0),
            self.apply(1.0, 1.0),
        ];
        let mut b = (pts[0].0, pts[0].1, pts[0].0, pts[0].1);
        for &(x, y) in &pts[1..] {
            b.0 = b.0.min(x);
            b.1 = b.1.min(y);
            b.2 = b.2.max(x);
            b.3 = b.3.max(y);
        }
        b
    }
}

/// An axis-aligned rectangle in device space, `x0 <= x1`, `y0 <= y1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Box2 {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Box2 {
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Box2 {
        Box2 {
            x0: x0.min(x1),
            y0: y0.min(y1),
            x1: x0.max(x1),
            y1: y0.max(y1),
        }
    }

    pub fn intersect(self, o: Box2) -> Box2 {
        Box2 {
            x0: self.x0.max(o.x0),
            y0: self.y0.max(o.y0),
            x1: self.x1.min(o.x1),
            y1: self.y1.min(o.y1),
        }
    }

    pub fn is_empty(self) -> bool {
        !(self.x1 > self.x0 && self.y1 > self.y0)
    }

    pub fn width(self) -> f64 {
        (self.x1 - self.x0).max(0.0)
    }

    pub fn height(self) -> f64 {
        (self.y1 - self.y0).max(0.0)
    }

    /// The four corners of `[x0 y0 x1 y1]` under `m`, as a bounding box.
    pub fn transformed(self, m: Mat) -> Box2 {
        let pts = [
            m.apply(self.x0, self.y0),
            m.apply(self.x1, self.y0),
            m.apply(self.x0, self.y1),
            m.apply(self.x1, self.y1),
        ];
        let mut b = Box2::new(pts[0].0, pts[0].1, pts[0].0, pts[0].1);
        for &(x, y) in &pts[1..] {
            b.x0 = b.x0.min(x);
            b.y0 = b.y0.min(y);
            b.x1 = b.x1.max(x);
            b.y1 = b.y1.max(y);
        }
        b
    }
}
