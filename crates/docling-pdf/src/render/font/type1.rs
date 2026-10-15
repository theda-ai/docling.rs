//! Type 1 font programs (`/FontFile`): the clear-text header's `/FontMatrix`
//! and built-in `/Encoding`, the eexec-encrypted private portion's `/Subrs`
//! and `/CharStrings`, and a Type 1 charstring interpreter (Adobe's *Type 1
//! Font Format*, chapters 6–8) that turns a glyph into a path in font units:
//! `hsbw`/`sbw`, the move/line/curve operators, `closepath`, `callsubr`,
//! `div`, `seac` accent composition, and flex / hint replacement through
//! `callothersubr`/`pop`/`setcurrentpoint`. Hints are parsed and dropped.

use std::collections::HashMap;

use super::encodings;
use super::GlyphPath;

pub struct Type1Font {
    /// Glyph name → decrypted charstring.
    charstrings: HashMap<String, Vec<u8>>,
    subrs: Vec<Vec<u8>>,
    /// The program's own `/Encoding`: code → glyph name.
    pub encoding: HashMap<u8, String>,
    pub font_matrix: [f64; 6],
}

const EEXEC_R: u16 = 55665;
const CHARSTRING_R: u16 = 4330;

fn decrypt(data: &[u8], mut r: u16, skip: usize) -> Vec<u8> {
    const C1: u16 = 52845;
    const C2: u16 = 22719;
    let mut out = Vec::with_capacity(data.len());
    for &c in data {
        let p = c ^ (r >> 8) as u8;
        r = (u16::from(c))
            .wrapping_add(r)
            .wrapping_mul(C1)
            .wrapping_add(C2);
        out.push(p);
    }
    if out.len() > skip {
        out.drain(..skip);
    } else {
        out.clear();
    }
    out
}

fn is_hex(b: u8) -> bool {
    b.is_ascii_hexdigit()
}

/// PFB segments (`0x80 0x01/0x02 len32`) concatenated into a plain stream.
fn unwrap_pfb(data: &[u8]) -> Vec<u8> {
    if data.len() < 6 || data[0] != 0x80 {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(data.len());
    let mut pos = 0;
    while pos + 6 <= data.len() && data[pos] == 0x80 {
        let kind = data[pos + 1];
        if kind == 3 {
            break;
        }
        let len = u32::from_le_bytes([data[pos + 2], data[pos + 3], data[pos + 4], data[pos + 5]])
            as usize;
        pos += 6;
        let end = (pos + len).min(data.len());
        out.extend_from_slice(&data[pos..end]);
        pos = end;
    }
    out
}

impl Type1Font {
    pub fn parse(raw: &[u8]) -> Option<Type1Font> {
        let data = unwrap_pfb(raw);
        let eexec = find(&data, b"eexec")?;
        let clear = &data[..eexec];
        let mut pos = eexec + 5;
        while pos < data.len() && matches!(data[pos], b'\r' | b'\n' | b' ' | b'\t') {
            pos += 1;
        }
        let enc_part = &data[pos..];
        // Hex or binary: four hex digits in a row mean the PFA hex form.
        let bin: Vec<u8> = if enc_part.len() >= 4 && enc_part[..4].iter().all(|&b| is_hex(b)) {
            let mut out = Vec::with_capacity(enc_part.len() / 2);
            let mut hi: Option<u8> = None;
            for &b in enc_part {
                let v = match b {
                    b'0'..=b'9' => b - b'0',
                    b'a'..=b'f' => b - b'a' + 10,
                    b'A'..=b'F' => b - b'A' + 10,
                    _ => continue,
                };
                match hi.take() {
                    None => hi = Some(v),
                    Some(h) => out.push((h << 4) | v),
                }
            }
            out
        } else {
            enc_part.to_vec()
        };
        let private = decrypt(&bin, EEXEC_R, 4);
        let len_iv = find(&private, b"/lenIV")
            .and_then(|p| parse_int_after(&private, p + 6))
            .unwrap_or(4)
            .max(0) as usize;

        let font_matrix = find(clear, b"/FontMatrix")
            .and_then(|p| parse_array6(clear, p + 11))
            .unwrap_or([0.001, 0.0, 0.0, 0.001, 0.0, 0.0]);

        let mut encoding = HashMap::new();
        if let Some(p) = find(clear, b"/Encoding") {
            let rest = &clear[p + 9..];
            let head = &rest[..rest.len().min(40)];
            if find(head, b"StandardEncoding").is_some() {
                for &(c, n) in encodings::STANDARD {
                    encoding.insert(c, n.to_string());
                }
            } else {
                // `dup <code> /<name> put` entries up to `readonly def` / ` def`.
                let end = find(rest, b" def").map(|e| e + 4).unwrap_or(rest.len());
                let section = &rest[..end];
                let mut i = 0;
                while let Some(d) = find(&section[i..], b"dup ") {
                    let at = i + d + 4;
                    let toks: Vec<&[u8]> = section[at..]
                        .split(|b| b.is_ascii_whitespace())
                        .filter(|t| !t.is_empty())
                        .take(3)
                        .collect();
                    if toks.len() >= 2 {
                        if let (Ok(code), Some(nm)) = (
                            std::str::from_utf8(toks[0])
                                .ok()
                                .and_then(|s| s.parse::<u32>().ok())
                                .ok_or(()),
                            toks[1].strip_prefix(b"/"),
                        ) {
                            if code <= 255 {
                                encoding
                                    .insert(code as u8, String::from_utf8_lossy(nm).into_owned());
                            }
                        }
                    }
                    i = at;
                }
            }
        }

        let subrs = parse_subrs(&private, len_iv);
        let charstrings = parse_charstrings(&private, len_iv)?;
        Some(Type1Font {
            charstrings,
            subrs,
            encoding,
            font_matrix,
        })
    }

    pub fn has_glyph(&self, name: &str) -> bool {
        self.charstrings.contains_key(name)
    }

    pub fn glyph_names(&self) -> impl Iterator<Item = &String> {
        self.charstrings.keys()
    }

    /// The glyph's outline and advance width, in font units (`FontMatrix`
    /// maps them to text space).
    pub fn glyph(&self, name: &str) -> Option<GlyphPath> {
        let cs = self.charstrings.get(name)?;
        let mut st = Interp {
            font: self,
            path: tiny_skia::PathBuilder::new(),
            stack: Vec::new(),
            ps_stack: Vec::new(),
            x: 0.0,
            y: 0.0,
            width: 0.0,
            sbx: 0.0,
            sby: 0.0,
            flex: None,
            open: false,
            depth: 0,
        };
        st.run(cs);
        st.close();
        let width = st.width;
        Some(GlyphPath {
            path: st.path.finish(),
            advance: width,
        })
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn parse_int_after(data: &[u8], pos: usize) -> Option<i64> {
    let s = &data[pos..data.len().min(pos + 32)];
    let tok = s
        .split(|b| b.is_ascii_whitespace())
        .find(|t| !t.is_empty())?;
    std::str::from_utf8(tok).ok()?.parse().ok()
}

fn parse_array6(data: &[u8], pos: usize) -> Option<[f64; 6]> {
    let open = pos + find(&data[pos..], b"[")?;
    let close = open + find(&data[open..], b"]")?;
    let vals: Vec<f64> = std::str::from_utf8(&data[open + 1..close])
        .ok()?
        .split_whitespace()
        .filter_map(|t| t.parse().ok())
        .collect();
    if vals.len() != 6 {
        return None;
    }
    Some([vals[0], vals[1], vals[2], vals[3], vals[4], vals[5]])
}

/// `/Subrs N array` followed by `dup <i> <n> RD <n bytes> NP` entries.
fn parse_subrs(private: &[u8], len_iv: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let Some(p) = find(private, b"/Subrs") else {
        return out;
    };
    let count = parse_int_after(private, p + 6).unwrap_or(0).clamp(0, 65536) as usize;
    out.resize(count, Vec::new());
    let mut pos = p + 6;
    for _ in 0..count {
        let Some(d) = find(&private[pos..], b"dup ") else {
            break;
        };
        let at = pos + d + 4;
        let Some((idx, rest)) = read_int(private, at) else {
            break;
        };
        let Some((n, rest)) = read_int(private, rest) else {
            break;
        };
        // The RD token (`RD` or `-|`), then one space, then the bytes.
        let Some(tok_end) = skip_token(private, rest) else {
            break;
        };
        let start = tok_end + 1;
        let end = start + n.max(0) as usize;
        if end > private.len() {
            break;
        }
        if idx >= 0 && (idx as usize) < count {
            out[idx as usize] = decrypt(&private[start..end], CHARSTRING_R, len_iv);
        }
        pos = end;
    }
    out
}

/// `/CharStrings N dict dup begin` followed by `/<name> <n> RD <bytes> ND`.
fn parse_charstrings(private: &[u8], len_iv: usize) -> Option<HashMap<String, Vec<u8>>> {
    let p = find(private, b"/CharStrings")?;
    let mut map = HashMap::new();
    let mut pos = p
        + 12
        + find(&private[p + 12..], b"begin")
            .map(|b| b + 5)
            .unwrap_or(0);
    // Each iteration consumes one `/name <len> RD <bytes> ND` entry.
    while let Some(slash) = private[pos..].iter().position(|&b| b == b'/') {
        let name_start = pos + slash + 1;
        let name_end = name_start
            + private[name_start..]
                .iter()
                .position(|b| b.is_ascii_whitespace() || *b == b'{' || *b == b'(')
                .unwrap_or(0);
        if name_end == name_start {
            pos = name_start;
            continue;
        }
        let name = String::from_utf8_lossy(&private[name_start..name_end]).into_owned();
        let Some((n, rest)) = read_int(private, name_end) else {
            // `end` reached, or a non-charstring entry.
            if find(&private[pos..pos + slash], b"end").is_some() {
                break;
            }
            pos = name_end;
            if !map.is_empty()
                && find(
                    &private[name_end..(name_end + 8).min(private.len())],
                    b"end",
                )
                .is_some()
            {
                break;
            }
            continue;
        };
        let Some(tok_end) = skip_token(private, rest) else {
            break;
        };
        let start = tok_end + 1;
        let end = start + n.max(0) as usize;
        if end > private.len() {
            break;
        }
        map.insert(name, decrypt(&private[start..end], CHARSTRING_R, len_iv));
        pos = end;
        if map.len() > 70_000 {
            break;
        }
    }
    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

/// An integer token at `pos` (skipping whitespace): `(value, position after)`.
fn read_int(data: &[u8], mut pos: usize) -> Option<(i64, usize)> {
    while pos < data.len() && data[pos].is_ascii_whitespace() {
        pos += 1;
    }
    let start = pos;
    while pos < data.len() && (data[pos].is_ascii_digit() || (pos == start && data[pos] == b'-')) {
        pos += 1;
    }
    if pos == start {
        return None;
    }
    let v = std::str::from_utf8(&data[start..pos]).ok()?.parse().ok()?;
    Some((v, pos))
}

/// Skip whitespace and one token; the position of its last byte + 1.
fn skip_token(data: &[u8], mut pos: usize) -> Option<usize> {
    while pos < data.len() && data[pos].is_ascii_whitespace() {
        pos += 1;
    }
    let start = pos;
    while pos < data.len() && !data[pos].is_ascii_whitespace() {
        pos += 1;
    }
    if pos == start {
        return None;
    }
    Some(pos)
}

struct Interp<'a> {
    font: &'a Type1Font,
    path: tiny_skia::PathBuilder,
    stack: Vec<f64>,
    ps_stack: Vec<f64>,
    x: f64,
    y: f64,
    width: f64,
    sbx: f64,
    sby: f64,
    /// Flex in progress: the collected points (the reference point excluded).
    flex: Option<Vec<(f64, f64)>>,
    open: bool,
    depth: usize,
}

impl Interp<'_> {
    fn close(&mut self) {
        if self.open {
            self.path.close();
            self.open = false;
        }
    }

    fn move_to(&mut self, x: f64, y: f64) {
        self.close();
        self.path.move_to(x as f32, y as f32);
        self.open = true;
    }

    fn line_to(&mut self, x: f64, y: f64) {
        if !self.open {
            self.path.move_to(self.x as f32, self.y as f32);
            self.open = true;
        }
        self.path.line_to(x as f32, y as f32);
    }

    fn curve_to(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, x3: f64, y3: f64) {
        if !self.open {
            self.path.move_to(self.x as f32, self.y as f32);
            self.open = true;
        }
        self.path.cubic_to(
            x1 as f32, y1 as f32, x2 as f32, y2 as f32, x3 as f32, y3 as f32,
        );
    }

    /// Returns `true` when `endchar` was reached.
    fn run(&mut self, cs: &[u8]) -> bool {
        if self.depth > 30 {
            return true;
        }
        let mut i = 0;
        while i < cs.len() {
            let v = cs[i];
            i += 1;
            match v {
                32..=246 => self.stack.push(f64::from(v) - 139.0),
                247..=250 => {
                    let w = f64::from(*cs.get(i).unwrap_or(&0));
                    i += 1;
                    self.stack.push((f64::from(v) - 247.0) * 256.0 + w + 108.0);
                }
                251..=254 => {
                    let w = f64::from(*cs.get(i).unwrap_or(&0));
                    i += 1;
                    self.stack.push(-(f64::from(v) - 251.0) * 256.0 - w - 108.0);
                }
                255 => {
                    if i + 4 > cs.len() {
                        return true;
                    }
                    let n = i32::from_be_bytes([cs[i], cs[i + 1], cs[i + 2], cs[i + 3]]);
                    i += 4;
                    self.stack.push(f64::from(n));
                }
                13 => {
                    // hsbw: sbx wx
                    if self.stack.len() >= 2 {
                        self.sbx = self.stack[0];
                        self.width = self.stack[1];
                        self.x = self.sbx;
                        self.y = 0.0;
                    }
                    self.stack.clear();
                }
                9 => {
                    self.close();
                    self.stack.clear();
                }
                1 | 3 => self.stack.clear(),
                21 => {
                    let (dx, dy) = self.take2();
                    self.x += dx;
                    self.y += dy;
                    self.after_move();
                }
                22 => {
                    let dx = self.take1();
                    self.x += dx;
                    self.after_move();
                }
                4 => {
                    let dy = self.take1();
                    self.y += dy;
                    self.after_move();
                }
                5 => {
                    let (dx, dy) = self.take2();
                    self.x += dx;
                    self.y += dy;
                    let (x, y) = (self.x, self.y);
                    self.line_to(x, y);
                }
                6 => {
                    let dx = self.take1();
                    self.x += dx;
                    let (x, y) = (self.x, self.y);
                    self.line_to(x, y);
                }
                7 => {
                    let dy = self.take1();
                    self.y += dy;
                    let (x, y) = (self.x, self.y);
                    self.line_to(x, y);
                }
                8 => {
                    if self.stack.len() >= 6 {
                        let s = self.stack.clone();
                        self.rrcurveto(s[0], s[1], s[2], s[3], s[4], s[5]);
                    }
                    self.stack.clear();
                }
                30 => {
                    // vhcurveto: dy1 dx2 dy2 dx3
                    if self.stack.len() >= 4 {
                        let s = self.stack.clone();
                        self.rrcurveto(0.0, s[0], s[1], s[2], s[3], 0.0);
                    }
                    self.stack.clear();
                }
                31 => {
                    // hvcurveto: dx1 dx2 dy2 dy3
                    if self.stack.len() >= 4 {
                        let s = self.stack.clone();
                        self.rrcurveto(s[0], 0.0, s[1], s[2], 0.0, s[3]);
                    }
                    self.stack.clear();
                }
                10 => {
                    let Some(n) = self.stack.pop() else { continue };
                    let n = n as i64;
                    if n >= 0 {
                        if let Some(sub) = self.font.subrs.get(n as usize) {
                            // The standard flex/hint subrs 0–3 are emulated
                            // through callothersubr; running them is what
                            // real fonts expect anyway.
                            self.depth += 1;
                            let sub = sub.clone();
                            let done = self.run(&sub);
                            self.depth -= 1;
                            if done {
                                return true;
                            }
                        }
                    }
                }
                11 => return false,
                14 => {
                    self.close();
                    return true;
                }
                12 => {
                    let v2 = *cs.get(i).unwrap_or(&0);
                    i += 1;
                    match v2 {
                        0 => self.stack.clear(),     // dotsection
                        1 | 2 => self.stack.clear(), // vstem3 hstem3
                        6 => {
                            // seac: asb adx ady bchar achar
                            if self.stack.len() >= 5 {
                                let s = self.stack.clone();
                                self.stack.clear();
                                self.seac(s[0], s[1], s[2], s[3] as i64, s[4] as i64);
                            }
                            return true;
                        }
                        7 => {
                            // sbw: sbx sby wx wy
                            if self.stack.len() >= 4 {
                                self.sbx = self.stack[0];
                                self.sby = self.stack[1];
                                self.width = self.stack[2];
                                self.x = self.sbx;
                                self.y = self.sby;
                            }
                            self.stack.clear();
                        }
                        12 => {
                            let b = self.stack.pop().unwrap_or(1.0);
                            let a = self.stack.pop().unwrap_or(0.0);
                            self.stack.push(if b != 0.0 { a / b } else { 0.0 });
                        }
                        16 => self.callothersubr(),
                        17 => {
                            let v = self.ps_stack.pop().unwrap_or(0.0);
                            self.stack.push(v);
                        }
                        33 => {
                            // setcurrentpoint
                            if self.stack.len() >= 2 {
                                self.x = self.stack[0];
                                self.y = self.stack[1];
                            }
                            self.stack.clear();
                        }
                        _ => self.stack.clear(),
                    }
                }
                _ => self.stack.clear(),
            }
        }
        false
    }

    fn take1(&mut self) -> f64 {
        let v = self.stack.first().copied().unwrap_or(0.0);
        self.stack.clear();
        v
    }

    fn take2(&mut self) -> (f64, f64) {
        let a = self.stack.first().copied().unwrap_or(0.0);
        let b = self.stack.get(1).copied().unwrap_or(0.0);
        self.stack.clear();
        (a, b)
    }

    fn after_move(&mut self) {
        if let Some(pts) = &mut self.flex {
            pts.push((self.x, self.y));
        } else {
            let (x, y) = (self.x, self.y);
            self.move_to(x, y);
        }
    }

    fn rrcurveto(&mut self, dx1: f64, dy1: f64, dx2: f64, dy2: f64, dx3: f64, dy3: f64) {
        let x1 = self.x + dx1;
        let y1 = self.y + dy1;
        let x2 = x1 + dx2;
        let y2 = y1 + dy2;
        self.x = x2 + dx3;
        self.y = y2 + dy3;
        let (x, y) = (self.x, self.y);
        self.curve_to(x1, y1, x2, y2, x, y);
    }

    fn callothersubr(&mut self) {
        let Some(othersubr) = self.stack.pop() else {
            return;
        };
        let Some(n) = self.stack.pop() else { return };
        let n = (n.max(0.0) as usize).min(self.stack.len());
        let args: Vec<f64> = self.stack.split_off(self.stack.len() - n);
        match othersubr as i64 {
            1 => {
                // Flex start: the next rmovetos collect points.
                self.flex = Some(Vec::new());
            }
            0 => {
                // Flex end: 17 args in PostScript terms; here the collected
                // points carry the geometry, the args' last two are the end
                // point handed back through two `pop`s.
                if let Some(pts) = self.flex.take() {
                    // The first collected point is the reference point.
                    if pts.len() >= 7 {
                        let p = &pts[1..7];
                        let (sx, sy) = (self.x, self.y);
                        // Curves from the point before the flex; the current
                        // point moved with the rmovetos, so rebuild from p.
                        let _ = (sx, sy);
                        self.curve_to(p[0].0, p[0].1, p[1].0, p[1].1, p[2].0, p[2].1);
                        self.curve_to(p[3].0, p[3].1, p[4].0, p[4].1, p[5].0, p[5].1);
                        self.x = p[5].0;
                        self.y = p[5].1;
                    } else if let Some(last) = pts.last() {
                        self.line_to(last.0, last.1);
                    }
                }
                let end_y = self.y;
                let end_x = self.x;
                // `pop pop setcurrentpoint` → x then y.
                self.ps_stack = vec![end_y, end_x];
            }
            2 => {}
            3 => {
                // Hint replacement: `subr# 1 3 callothersubr pop callsubr`.
                self.ps_stack = vec![3.0];
                let _ = args;
            }
            _ => {
                // Unknown: hand the arguments back to the following `pop`s.
                let mut a = args;
                a.reverse();
                self.ps_stack = a;
            }
        }
    }

    fn seac(&mut self, asb: f64, adx: f64, ady: f64, bchar: i64, achar: i64) {
        let name_of = |c: i64| -> Option<&'static str> {
            if !(0..=255).contains(&c) {
                return None;
            }
            encodings::lookup(encodings::STANDARD, c as u8)
        };
        let (Some(bname), Some(aname)) = (name_of(bchar), name_of(achar)) else {
            return;
        };
        let sbx = self.sbx;
        let width = self.width;
        if let Some(base) = self.font.glyph(bname).and_then(|g| g.path) {
            self.path.push_path(&base);
        }
        if let Some(acc) = self.font.glyph_raw(aname) {
            let dx = sbx - acc.sbx + adx - asb;
            let dy = ady;
            if let Some(p) = acc
                .path
                .transform(tiny_skia::Transform::from_translate(dx as f32, dy as f32))
            {
                self.path.push_path(&p);
            }
        }
        self.width = width;
        self.open = false;
    }
}

struct RawGlyph {
    path: tiny_skia::Path,
    sbx: f64,
}

impl Type1Font {
    /// A glyph with its side bearing, for accent placement in `seac`.
    fn glyph_raw(&self, name: &str) -> Option<RawGlyph> {
        let cs = self.charstrings.get(name)?;
        let mut st = Interp {
            font: self,
            path: tiny_skia::PathBuilder::new(),
            stack: Vec::new(),
            ps_stack: Vec::new(),
            x: 0.0,
            y: 0.0,
            width: 0.0,
            sbx: 0.0,
            sby: 0.0,
            flex: None,
            open: false,
            depth: 1,
        };
        st.run(cs);
        st.close();
        let sbx = st.sbx;
        Some(RawGlyph {
            path: st.path.finish()?,
            sbx,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encrypt with the charstring/eexec cipher (the inverse of `decrypt`).
    fn encrypt(data: &[u8], mut r: u16, lead: usize) -> Vec<u8> {
        const C1: u16 = 52845;
        const C2: u16 = 22719;
        let mut out = Vec::new();
        let plain: Vec<u8> = std::iter::repeat_n(0u8, lead)
            .chain(data.iter().copied())
            .collect();
        for &p in &plain {
            let c = p ^ (r >> 8) as u8;
            r = (u16::from(c))
                .wrapping_add(r)
                .wrapping_mul(C1)
                .wrapping_add(C2);
            out.push(c);
        }
        out
    }

    fn num(v: i32) -> Vec<u8> {
        if (-107..=107).contains(&v) {
            vec![(v + 139) as u8]
        } else {
            let mut out = vec![255];
            out.extend_from_slice(&v.to_be_bytes());
            out
        }
    }

    /// A square glyph: `50 600 hsbw 0 0 rmoveto 500 hlineto 500 vlineto -500 hlineto closepath endchar`.
    #[test]
    fn parses_a_synthetic_program() {
        let mut cs = Vec::new();
        cs.extend(num(50));
        cs.extend(num(600));
        cs.push(13);
        cs.extend(num(0));
        cs.extend(num(0));
        cs.push(21);
        cs.extend(num(500));
        cs.push(6);
        cs.extend(num(500));
        cs.push(7);
        cs.extend(num(-500));
        cs.push(6);
        cs.push(9);
        cs.push(14);
        let enc_cs = encrypt(&cs, CHARSTRING_R, 4);
        let mut private = Vec::new();
        private.extend_from_slice(
            b"dup /Private 8 dict dup begin /lenIV 4 def /Subrs 1 array\ndup 0 1 RD ",
        );
        private.extend(encrypt(&[11], CHARSTRING_R, 4));
        private.extend_from_slice(b" NP\n/CharStrings 2 dict dup begin\n/square ");
        private.extend_from_slice(format!("{} RD ", enc_cs.len()).as_bytes());
        private.extend(&enc_cs);
        private.extend_from_slice(b" ND\n/.notdef 1 RD ");
        private.extend(encrypt(&[14], CHARSTRING_R, 4));
        private.extend_from_slice(b" ND\nend\n");
        let mut data = Vec::new();
        data.extend_from_slice(
            b"%!PS-AdobeFont-1.0: Test\n/FontMatrix [0.001 0 0 0.001 0 0] readonly def\n/Encoding 256 array\n0 1 255 {1 index exch /.notdef put} for\ndup 65 /square put\nreadonly def\ncurrentdict end\ncurrentfile eexec\n",
        );
        data.extend(encrypt(&private, EEXEC_R, 4));
        let font = Type1Font::parse(&data).expect("parses");
        assert_eq!(font.encoding.get(&65).map(String::as_str), Some("square"));
        assert_eq!(font.font_matrix[0], 0.001);
        let g = font.glyph("square").expect("glyph");
        assert_eq!(g.advance, 600.0);
        let b = g.path.as_ref().expect("path").bounds();
        assert_eq!(
            (b.left(), b.top(), b.right(), b.bottom()),
            (50.0, 0.0, 550.0, 500.0)
        );
    }
}
