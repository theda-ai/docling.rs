//! PDF functions (ISO 32000-1, 7.10): sampled (type 0), exponential (2),
//! stitching (3) and PostScript calculator (4) — what shadings, Separation /
//! DeviceN tint transforms and `/Decode`-less colour ramps evaluate.

use lopdf::{Dictionary, Document, Object};

use super::objects::{deref, num, nums};

#[derive(Debug, Clone)]
pub enum Function {
    Sampled {
        domain: Vec<f64>,
        range: Vec<f64>,
        size: Vec<usize>,
        bps: u32,
        encode: Vec<f64>,
        decode: Vec<f64>,
        samples: Vec<u8>,
        n_out: usize,
    },
    Exponential {
        domain: Vec<f64>,
        c0: Vec<f64>,
        c1: Vec<f64>,
        n: f64,
    },
    Stitching {
        domain: Vec<f64>,
        functions: Vec<Function>,
        bounds: Vec<f64>,
        encode: Vec<f64>,
    },
    PostScript {
        domain: Vec<f64>,
        range: Vec<f64>,
        program: Vec<PsOp>,
    },
    /// Several 1-out functions side by side (a shading's `/Function` array).
    Array(Vec<Function>),
    /// The identity, for a `/Function` that could not be read: outputs are
    /// the inputs, which keeps a broken shading visible rather than absent.
    Identity,
}

/// One token of a type 4 program.
#[derive(Debug, Clone)]
pub enum PsOp {
    Num(f64),
    Op(&'static str),
    /// `{ … }`: the index of the block in the flattened program, exclusive end.
    Block(usize, usize),
}

impl Function {
    /// Parse a `/Function` entry: a dictionary, a stream, or an array of them.
    pub fn parse(doc: &Document, obj: &Object) -> Option<Function> {
        Self::parse_depth(doc, obj, 0)
    }

    fn parse_depth(doc: &Document, obj: &Object, depth: usize) -> Option<Function> {
        if depth > 8 {
            return None;
        }
        let obj = deref(doc, obj);
        if let Object::Array(a) = obj {
            let fns: Vec<Function> = a
                .iter()
                .filter_map(|o| Self::parse_depth(doc, o, depth + 1))
                .collect();
            if fns.is_empty() {
                return None;
            }
            return Some(Function::Array(fns));
        }
        let (dict, stream) = match obj {
            Object::Dictionary(d) => (d, None),
            Object::Stream(s) => (&s.dict, Some(s)),
            _ => return None,
        };
        let get = |k: &[u8]| dict.get(k).ok().map(|o| deref(doc, o));
        let getnums = |k: &[u8]| get(k).and_then(|o| nums(doc, o));
        let ftype = get(b"FunctionType").and_then(|o| o.as_i64().ok())?;
        let domain = getnums(b"Domain").unwrap_or_else(|| vec![0.0, 1.0]);
        match ftype {
            0 => {
                let s = stream?;
                let samples = s.decompressed_content().ok()?;
                let size: Vec<usize> = getnums(b"Size")?
                    .iter()
                    .map(|v| (*v as i64).max(1) as usize)
                    .collect();
                let bps = get(b"BitsPerSample").and_then(|o| o.as_i64().ok())? as u32;
                if !matches!(bps, 1 | 2 | 4 | 8 | 12 | 16 | 24 | 32) {
                    return None;
                }
                let range = getnums(b"Range")?;
                let n_out = range.len() / 2;
                let m = size.len();
                let mut encode = getnums(b"Encode").unwrap_or_default();
                if encode.len() < 2 * m {
                    encode = size
                        .iter()
                        .flat_map(|&s| [0.0, (s as f64 - 1.0).max(0.0)])
                        .collect();
                }
                let mut decode = getnums(b"Decode").unwrap_or_default();
                if decode.len() < 2 * n_out {
                    decode = range.clone();
                }
                Some(Function::Sampled {
                    domain,
                    range,
                    size,
                    bps,
                    encode,
                    decode,
                    samples,
                    n_out,
                })
            }
            2 => {
                let c0 = getnums(b"C0").unwrap_or_else(|| vec![0.0]);
                let c1 = getnums(b"C1").unwrap_or_else(|| vec![1.0]);
                let n = get(b"N").and_then(num).unwrap_or(1.0);
                Some(Function::Exponential { domain, c0, c1, n })
            }
            3 => {
                let functions: Vec<Function> = get(b"Functions")
                    .and_then(|o| o.as_array().ok())?
                    .iter()
                    .map(|o| Self::parse_depth(doc, o, depth + 1).unwrap_or(Function::Identity))
                    .collect();
                let bounds = getnums(b"Bounds").unwrap_or_default();
                let encode = getnums(b"Encode").unwrap_or_default();
                Some(Function::Stitching {
                    domain,
                    functions,
                    bounds,
                    encode,
                })
            }
            4 => {
                let s = stream?;
                let src = s.decompressed_content().ok()?;
                let program = parse_postscript(&src)?;
                let range = getnums(b"Range").unwrap_or_default();
                Some(Function::PostScript {
                    domain,
                    range,
                    program,
                })
            }
            _ => None,
        }
    }

    /// Evaluate at `inputs`, clamped to the domain; the output count follows
    /// the function (`Range`, `C0`, the array length …).
    pub fn eval(&self, inputs: &[f64]) -> Vec<f64> {
        match self {
            Function::Identity => inputs.to_vec(),
            Function::Array(fns) => fns
                .iter()
                .flat_map(|f| f.eval(inputs).into_iter().take(1))
                .collect(),
            Function::Exponential { domain, c0, c1, n } => {
                let x = clamp_domain(inputs.first().copied().unwrap_or(0.0), domain, 0);
                let t = if *n == 1.0 {
                    x
                } else {
                    x.abs().powf(*n) * x.signum()
                };
                c0.iter()
                    .zip(c1.iter().chain(std::iter::repeat(&1.0)))
                    .map(|(a, b)| a + t * (b - a))
                    .collect()
            }
            Function::Stitching {
                domain,
                functions,
                bounds,
                encode,
            } => {
                let x = clamp_domain(inputs.first().copied().unwrap_or(0.0), domain, 0);
                let k = functions.len();
                if k == 0 {
                    return vec![0.0];
                }
                let mut i = 0;
                while i < bounds.len() && i + 1 < k && x >= bounds[i] {
                    i += 1;
                }
                let lo = if i == 0 { domain[0] } else { bounds[i - 1] };
                let hi = if i >= bounds.len() {
                    domain.get(1).copied().unwrap_or(1.0)
                } else {
                    bounds[i]
                };
                let e0 = encode.get(2 * i).copied().unwrap_or(0.0);
                let e1 = encode.get(2 * i + 1).copied().unwrap_or(1.0);
                let t = if hi > lo {
                    e0 + (x - lo) / (hi - lo) * (e1 - e0)
                } else {
                    e0
                };
                functions[i].eval(&[t])
            }
            Function::Sampled {
                domain,
                range,
                size,
                bps,
                encode,
                decode,
                samples,
                n_out,
            } => eval_sampled(
                inputs, domain, range, size, *bps, encode, decode, samples, *n_out,
            ),
            Function::PostScript {
                domain,
                range,
                program,
            } => {
                let mut stack: Vec<f64> = inputs
                    .iter()
                    .enumerate()
                    .map(|(i, &v)| clamp_domain(v, domain, i))
                    .collect();
                exec_postscript(program, 0, program.len(), &mut stack, 0);
                let n_out = range.len() / 2;
                if n_out > 0 {
                    let start = stack.len().saturating_sub(n_out);
                    let mut out: Vec<f64> = stack[start..].to_vec();
                    while out.len() < n_out {
                        out.insert(0, 0.0);
                    }
                    for (i, v) in out.iter_mut().enumerate() {
                        *v = v.clamp(
                            range[2 * i].min(range[2 * i + 1]),
                            range[2 * i].max(range[2 * i + 1]),
                        );
                    }
                    out
                } else {
                    stack
                }
            }
        }
    }
}

fn clamp_domain(x: f64, domain: &[f64], i: usize) -> f64 {
    match (domain.get(2 * i), domain.get(2 * i + 1)) {
        (Some(&lo), Some(&hi)) if hi >= lo => x.clamp(lo, hi),
        _ => x,
    }
}

#[allow(clippy::too_many_arguments)]
fn eval_sampled(
    inputs: &[f64],
    domain: &[f64],
    range: &[f64],
    size: &[usize],
    bps: u32,
    encode: &[f64],
    decode: &[f64],
    samples: &[u8],
    n_out: usize,
) -> Vec<f64> {
    let m = size.len();
    if m == 0 || n_out == 0 {
        return vec![0.0; n_out.max(1)];
    }
    let max = ((1u64 << bps) - 1) as f64;
    let sample_at = |idx: usize, j: usize| -> f64 {
        let bit = (idx * n_out + j) as u64 * u64::from(bps);
        let byte = (bit / 8) as usize;
        let v: u64 = match bps {
            8 => u64::from(*samples.get(byte).unwrap_or(&0)),
            16 => {
                let b = |k: usize| u64::from(*samples.get(byte + k).unwrap_or(&0));
                (b(0) << 8) | b(1)
            }
            24 => {
                let b = |k: usize| u64::from(*samples.get(byte + k).unwrap_or(&0));
                (b(0) << 16) | (b(1) << 8) | b(2)
            }
            32 => {
                let b = |k: usize| u64::from(*samples.get(byte + k).unwrap_or(&0));
                (b(0) << 24) | (b(1) << 16) | (b(2) << 8) | b(3)
            }
            _ => {
                // 1/2/4/12-bit: read bit by bit, MSB first.
                let mut v = 0u64;
                for k in 0..bps as u64 {
                    let p = bit + k;
                    let byte = *samples.get((p / 8) as usize).unwrap_or(&0);
                    v = (v << 1) | u64::from((byte >> (7 - p % 8)) & 1);
                }
                v
            }
        };
        v as f64 / max
    };
    // Multilinear interpolation over the first input dimension only for m == 1
    // (the common case); nearest sample otherwise — mesh shadings and tint
    // transforms are smooth enough that the difference is invisible.
    let mut idx0 = 0usize;
    let mut stride = 1usize;
    let mut frac0 = 0.0;
    let mut stride0 = 1usize;
    for i in 0..m {
        let x = clamp_domain(inputs.get(i).copied().unwrap_or(0.0), domain, i);
        let (d0, d1) = (domain[2 * i], domain[2 * i + 1]);
        let (e0, e1) = (encode[2 * i], encode[2 * i + 1]);
        let e = if d1 > d0 {
            e0 + (x - d0) * (e1 - e0) / (d1 - d0)
        } else {
            e0
        };
        let e = e.clamp(0.0, (size[i] as f64 - 1.0).max(0.0));
        let fl = e.floor() as usize;
        if i == 0 {
            frac0 = e - fl as f64;
            stride0 = stride;
        }
        idx0 += fl.min(size[i] - 1) * stride;
        stride *= size[i];
    }
    let mut out = Vec::with_capacity(n_out);
    for j in 0..n_out {
        let s0 = sample_at(idx0, j);
        let s = if frac0 > 0.0 && (idx0 / stride0) % size[0] + 1 < size[0] {
            let s1 = sample_at(idx0 + stride0, j);
            s0 + (s1 - s0) * frac0
        } else {
            s0
        };
        let (dmin, dmax) = (decode[2 * j], decode[2 * j + 1]);
        let mut v = dmin + s * (dmax - dmin);
        if let (Some(&r0), Some(&r1)) = (range.get(2 * j), range.get(2 * j + 1)) {
            v = v.clamp(r0.min(r1), r0.max(r1));
        }
        out.push(v);
    }
    out
}

/// Tokenize a type 4 program into a flat list where `{ … }` blocks are
/// recorded as `(start, end)` spans over the same list.
fn parse_postscript(src: &[u8]) -> Option<Vec<PsOp>> {
    let text = String::from_utf8_lossy(src);
    let mut toks: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        match ch {
            '{' | '}' => {
                if !cur.is_empty() {
                    toks.push(std::mem::take(&mut cur));
                }
                toks.push(ch.to_string());
            }
            c if c.is_whitespace() => {
                if !cur.is_empty() {
                    toks.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        toks.push(cur);
    }
    // Flatten: a block becomes PsOp::Block(start, end) placed where it appears,
    // with its body appended after the whole enclosing sequence. Simpler: emit
    // recursively into one vector where a block's body immediately follows a
    // Block marker whose `end` skips it.
    let mut out = Vec::new();
    let mut pos = 0usize;
    // The outermost `{ }` wraps the whole program.
    if toks.first().map(String::as_str) == Some("{") {
        pos = 1;
    }
    parse_ps_seq(&toks, &mut pos, &mut out, 0)?;
    Some(out)
}

fn parse_ps_seq(toks: &[String], pos: &mut usize, out: &mut Vec<PsOp>, depth: usize) -> Option<()> {
    if depth > 64 {
        return None;
    }
    while *pos < toks.len() {
        let t = toks[*pos].as_str();
        *pos += 1;
        match t {
            "}" => return Some(()),
            "{" => {
                let marker = out.len();
                out.push(PsOp::Block(0, 0));
                parse_ps_seq(toks, pos, out, depth + 1)?;
                let end = out.len();
                out[marker] = PsOp::Block(marker + 1, end);
            }
            _ => {
                if let Ok(v) = t.parse::<f64>() {
                    out.push(PsOp::Num(v));
                } else {
                    out.push(PsOp::Op(ps_operator(t)?));
                }
            }
        }
    }
    Some(())
}

fn ps_operator(t: &str) -> Option<&'static str> {
    const OPS: &[&str] = &[
        "abs", "add", "atan", "ceiling", "cos", "cvi", "cvr", "div", "exp", "floor", "idiv", "ln",
        "log", "mod", "mul", "neg", "round", "sin", "sqrt", "sub", "truncate", "and", "bitshift",
        "eq", "false", "ge", "gt", "le", "lt", "ne", "not", "or", "true", "xor", "if", "ifelse",
        "copy", "dup", "exch", "index", "pop", "roll",
    ];
    OPS.iter().copied().find(|o| *o == t)
}

/// Execute `program[start..end]` on `stack`.
fn exec_postscript(program: &[PsOp], start: usize, end: usize, stack: &mut Vec<f64>, depth: usize) {
    if depth > 64 {
        return;
    }
    let mut i = start;
    // Pending procedure operands for `if` / `ifelse`.
    let mut blocks: Vec<(usize, usize)> = Vec::new();
    let pop = |s: &mut Vec<f64>| s.pop().unwrap_or(0.0);
    while i < end {
        if stack.len() > 1000 {
            return;
        }
        match &program[i] {
            PsOp::Num(v) => stack.push(*v),
            PsOp::Block(s, e) => {
                blocks.push((*s, *e));
                i = *e;
                continue;
            }
            PsOp::Op(op) => match *op {
                "if" => {
                    let cond = pop(stack) != 0.0;
                    if let Some((s, e)) = blocks.pop() {
                        if cond {
                            exec_postscript(program, s, e, stack, depth + 1);
                        }
                    }
                    blocks.clear();
                }
                "ifelse" => {
                    let cond = pop(stack) != 0.0;
                    let b2 = blocks.pop();
                    let b1 = blocks.pop();
                    if let (Some((s1, e1)), Some((s2, e2))) = (b1, b2) {
                        if cond {
                            exec_postscript(program, s1, e1, stack, depth + 1);
                        } else {
                            exec_postscript(program, s2, e2, stack, depth + 1);
                        }
                    }
                    blocks.clear();
                }
                "abs" => {
                    let a = pop(stack);
                    stack.push(a.abs());
                }
                "add" => {
                    let b = pop(stack);
                    let a = pop(stack);
                    stack.push(a + b);
                }
                "sub" => {
                    let b = pop(stack);
                    let a = pop(stack);
                    stack.push(a - b);
                }
                "mul" => {
                    let b = pop(stack);
                    let a = pop(stack);
                    stack.push(a * b);
                }
                "div" => {
                    let b = pop(stack);
                    let a = pop(stack);
                    stack.push(if b != 0.0 { a / b } else { 0.0 });
                }
                "idiv" => {
                    let b = pop(stack) as i64;
                    let a = pop(stack) as i64;
                    stack.push(if b != 0 { (a / b) as f64 } else { 0.0 });
                }
                "mod" => {
                    let b = pop(stack) as i64;
                    let a = pop(stack) as i64;
                    stack.push(if b != 0 { (a % b) as f64 } else { 0.0 });
                }
                "neg" => {
                    let a = pop(stack);
                    stack.push(-a);
                }
                "atan" => {
                    let den = pop(stack);
                    let numr = pop(stack);
                    let mut deg = numr.atan2(den).to_degrees();
                    if deg < 0.0 {
                        deg += 360.0;
                    }
                    stack.push(deg);
                }
                "ceiling" => {
                    let a = pop(stack);
                    stack.push(a.ceil());
                }
                "floor" => {
                    let a = pop(stack);
                    stack.push(a.floor());
                }
                "round" => {
                    let a = pop(stack);
                    stack.push(a.round());
                }
                "truncate" => {
                    let a = pop(stack);
                    stack.push(a.trunc());
                }
                "cos" => {
                    let a = pop(stack);
                    stack.push(a.to_radians().cos());
                }
                "sin" => {
                    let a = pop(stack);
                    stack.push(a.to_radians().sin());
                }
                "sqrt" => {
                    let a = pop(stack);
                    stack.push(a.max(0.0).sqrt());
                }
                "exp" => {
                    let b = pop(stack);
                    let a = pop(stack);
                    stack.push(a.powf(b));
                }
                "ln" => {
                    let a = pop(stack);
                    stack.push(if a > 0.0 { a.ln() } else { 0.0 });
                }
                "log" => {
                    let a = pop(stack);
                    stack.push(if a > 0.0 { a.log10() } else { 0.0 });
                }
                "cvi" => {
                    let a = pop(stack);
                    stack.push(a.trunc());
                }
                "cvr" => {}
                "dup" => {
                    let a = stack.last().copied().unwrap_or(0.0);
                    stack.push(a);
                }
                "pop" => {
                    stack.pop();
                }
                "exch" => {
                    let b = pop(stack);
                    let a = pop(stack);
                    stack.push(b);
                    stack.push(a);
                }
                "copy" => {
                    let n = pop(stack).max(0.0) as usize;
                    let len = stack.len();
                    if n <= len {
                        for k in 0..n {
                            stack.push(stack[len - n + k]);
                        }
                    }
                }
                "index" => {
                    let n = pop(stack).max(0.0) as usize;
                    let len = stack.len();
                    let v = if n < len { stack[len - 1 - n] } else { 0.0 };
                    stack.push(v);
                }
                "roll" => {
                    let j = pop(stack) as i64;
                    let n = pop(stack).max(0.0) as usize;
                    let len = stack.len();
                    if n > 0 && n <= len {
                        let s = &mut stack[len - n..];
                        let j = j.rem_euclid(n as i64) as usize;
                        s.rotate_right(j);
                    }
                }
                "eq" | "ne" | "gt" | "ge" | "lt" | "le" => {
                    let b = pop(stack);
                    let a = pop(stack);
                    let r = match *op {
                        "eq" => a == b,
                        "ne" => a != b,
                        "gt" => a > b,
                        "ge" => a >= b,
                        "lt" => a < b,
                        _ => a <= b,
                    };
                    stack.push(if r { 1.0 } else { 0.0 });
                }
                "and" | "or" | "xor" => {
                    let b = pop(stack) as i64;
                    let a = pop(stack) as i64;
                    let r = match *op {
                        "and" => a & b,
                        "or" => a | b,
                        _ => a ^ b,
                    };
                    stack.push(r as f64);
                }
                "not" => {
                    let a = pop(stack);
                    // Boolean not on 0/1, bitwise on other integers.
                    stack.push(if a == 0.0 {
                        1.0
                    } else if a == 1.0 {
                        0.0
                    } else {
                        !(a as i64) as f64
                    });
                }
                "bitshift" => {
                    let s = pop(stack) as i64;
                    let a = pop(stack) as i64;
                    stack.push(if s >= 0 {
                        a.checked_shl(s.min(63) as u32).unwrap_or(0) as f64
                    } else {
                        (a >> (-s).min(63)) as f64
                    });
                }
                "true" => stack.push(1.0),
                "false" => stack.push(0.0),
                _ => {}
            },
        }
        i += 1;
    }
}

/// Convenience: a function's outputs for one input, as an `n`-vector padded /
/// truncated to `n`.
pub fn eval_n(f: &Function, t: f64, n: usize) -> Vec<f64> {
    let mut v = f.eval(&[t]);
    v.resize(n, 0.0);
    v
}

/// `/Function` on a dictionary, if any.
pub fn function_of(doc: &Document, dict: &Dictionary, key: &[u8]) -> Option<Function> {
    dict.get(key).ok().and_then(|o| Function::parse(doc, o))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ps(src: &str, inputs: &[f64], range: &[f64]) -> Vec<f64> {
        let program = parse_postscript(src.as_bytes()).unwrap();
        let f = Function::PostScript {
            domain: vec![0.0, 1.0, 0.0, 1.0],
            range: range.to_vec(),
            program,
        };
        f.eval(inputs)
    }

    #[test]
    fn postscript_calculator() {
        assert_eq!(ps("{ add 2 div }", &[0.2, 0.6], &[0.0, 1.0]), vec![0.4]);
        assert_eq!(
            ps(
                "{ dup 0.5 gt { pop 1 } { pop 0 } ifelse }",
                &[0.7],
                &[0.0, 1.0]
            ),
            vec![1.0]
        );
        assert_eq!(
            ps(
                "{ dup 0.5 gt { pop 1 } { pop 0 } ifelse }",
                &[0.2],
                &[0.0, 1.0]
            ),
            vec![0.0]
        );
        assert_eq!(ps("{ 1 exch sub }", &[0.25], &[0.0, 1.0]), vec![0.75]);
        assert_eq!(
            ps(
                "{ 3 1 roll }",
                &[0.1, 0.2, 0.3],
                &[0.0, 1.0, 0.0, 1.0, 0.0, 1.0]
            ),
            vec![0.3, 0.1, 0.2]
        );
    }

    #[test]
    fn exponential_and_stitching() {
        let f = Function::Exponential {
            domain: vec![0.0, 1.0],
            c0: vec![0.0, 0.0, 1.0],
            c1: vec![1.0, 0.0, 0.0],
            n: 1.0,
        };
        assert_eq!(f.eval(&[0.5]), vec![0.5, 0.0, 0.5]);
        let st = Function::Stitching {
            domain: vec![0.0, 1.0],
            functions: vec![
                Function::Exponential {
                    domain: vec![0.0, 1.0],
                    c0: vec![0.0],
                    c1: vec![1.0],
                    n: 1.0,
                },
                Function::Exponential {
                    domain: vec![0.0, 1.0],
                    c0: vec![1.0],
                    c1: vec![0.0],
                    n: 1.0,
                },
            ],
            bounds: vec![0.5],
            encode: vec![0.0, 1.0, 0.0, 1.0],
        };
        assert!((st.eval(&[0.25])[0] - 0.5).abs() < 1e-9);
        assert!((st.eval(&[0.75])[0] - 0.5).abs() < 1e-9);
        assert!((st.eval(&[0.5])[0] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn sampled_interpolates() {
        let f = Function::Sampled {
            domain: vec![0.0, 1.0],
            range: vec![0.0, 1.0],
            size: vec![3],
            bps: 8,
            encode: vec![0.0, 2.0],
            decode: vec![0.0, 1.0],
            samples: vec![0, 255, 0],
            n_out: 1,
        };
        assert!((f.eval(&[0.25])[0] - 0.5).abs() < 0.01);
        assert!((f.eval(&[0.5])[0] - 1.0).abs() < 0.01);
    }
}
