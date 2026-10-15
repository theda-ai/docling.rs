//! A pre-pass over a content stream for the two constructs lopdf's content
//! lexer (0.44) does not hand over intact:
//!
//! * **Inline images** (`BI … ID … EI`): lopdf drops the whole image when
//!   the colour space is an abbreviation it does not know (`/G`, `/I`), an
//!   `Indexed` array or a resource name, and *every* filtered one
//!   ("filters for inline images are not yet implemented"). pdfTeX rules,
//!   scanner-driver strips and dvips bitmaps are all of those. The pre-pass
//!   cuts each inline image out itself — the header parsed as a dictionary,
//!   the data by its computed length (unfiltered) or the `EI` delimiter
//!   (filtered) — and leaves `/I<n> BIX` in its place, which the interpreter
//!   resolves against [`Prepared::inline`].
//! * **Type 3 glyph operators** `d0` / `d1`: the alphabetic operator lexer
//!   splits them into `d` and a stray `0`/`1` operand that then prefixes the
//!   next operator's operands (a `re` reads the wrong rectangle). They are
//!   rewritten to `dZero` / `dOne`.
//!
//! The pass is one byte scan honouring strings, hex strings, dictionaries
//! and comments; a stream without either construct is returned borrowed.

use std::borrow::Cow;

use lopdf::{Dictionary, Document, Object, Stream};

use super::color::ColorSpace;
use super::objects::{get2, get_bool2, get_int2, name, resource};

pub struct Prepared<'a> {
    pub content: Cow<'a, [u8]>,
    /// The inline images in order of appearance; `/I<n> BIX` names index `n`.
    pub inline: Vec<Stream>,
}

fn is_white(c: u8) -> bool {
    matches!(c, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

fn is_delim(c: u8) -> bool {
    matches!(
        c,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn is_regular(c: u8) -> bool {
    !is_white(c) && !is_delim(c)
}

/// Skip one lexical item starting at `i` (a string, hex string, comment,
/// delimiter or regular token); returns the index after it, and the token's
/// bytes when it was a regular token.
fn skip_item(content: &[u8], i: usize) -> (usize, Option<&[u8]>) {
    let c = content[i];
    match c {
        b'%' => {
            let mut j = i;
            while j < content.len() && content[j] != b'\n' && content[j] != b'\r' {
                j += 1;
            }
            (j, None)
        }
        b'(' => {
            let mut depth = 0usize;
            let mut j = i;
            while j < content.len() {
                match content[j] {
                    b'\\' => j += 1,
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            return (j + 1, None);
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            (content.len(), None)
        }
        b'<' => {
            if content.get(i + 1) == Some(&b'<') {
                (i + 2, None)
            } else {
                let mut j = i + 1;
                while j < content.len() && content[j] != b'>' {
                    j += 1;
                }
                ((j + 1).min(content.len()), None)
            }
        }
        b'/' => {
            let mut j = i + 1;
            while j < content.len() && is_regular(content[j]) {
                j += 1;
            }
            (j, None)
        }
        c if is_delim(c) => (i + 1, None),
        _ => {
            let mut j = i;
            while j < content.len() && is_regular(content[j]) {
                j += 1;
            }
            (j, Some(&content[i..j]))
        }
    }
}

/// Does the content at `i` (after the data) read as whitespace, `EI`, then
/// a delimiter, whitespace or the end?
fn ei_at(content: &[u8], mut i: usize) -> Option<usize> {
    while i < content.len() && is_white(content[i]) {
        i += 1;
    }
    if content.get(i..i + 2) != Some(b"EI") {
        return None;
    }
    let end = i + 2;
    if end == content.len() || is_white(content[end]) || is_delim(content[end]) {
        Some(end)
    } else {
        None
    }
}

/// The first `EI` after `start` that stands alone between whitespace (or a
/// delimiter) and is followed by plausible content-stream text, as pdfium's
/// and lopdf's own heuristics have it.
fn find_ei(content: &[u8], start: usize) -> Option<(usize, usize)> {
    let mut i = start;
    while i + 1 < content.len() {
        if content[i] == b'E'
            && content[i + 1] == b'I'
            && (i == start || is_white(content[i - 1]))
            && (i + 2 == content.len() || is_white(content[i + 2]) || is_delim(content[i + 2]))
        {
            let tail = &content[i + 2..(i + 34).min(content.len())];
            if tail
                .iter()
                .all(|&c| is_white(c) || (0x20..0x7f).contains(&c))
            {
                let mut data_end = i;
                while data_end > start && is_white(content[data_end - 1]) {
                    data_end -= 1;
                }
                return Some((data_end, i + 2));
            }
        }
        i += 1;
    }
    None
}

/// The header of an inline image as a dictionary: lopdf's operand parser
/// on `<< … >> X`.
fn parse_header(header: &[u8]) -> Option<Dictionary> {
    let mut src = Vec::with_capacity(header.len() + 8);
    src.extend_from_slice(b"<<");
    src.extend_from_slice(header);
    src.extend_from_slice(b">> X");
    let ops = lopdf::content::Content::decode(&src).ok()?;
    let op = ops.operations.into_iter().next()?;
    match op.operands.into_iter().next()? {
        Object::Dictionary(d) => Some(d),
        _ => None,
    }
}

/// Components per sample of an inline image's colour space (1 for masks and
/// Indexed), `None` when it cannot be told without decoding.
fn components(doc: &Document, d: &Dictionary, res: Option<&Dictionary>) -> Option<usize> {
    if get_bool2(doc, d, b"ImageMask", b"IM") == Some(true) {
        return Some(1);
    }
    let cs = get2(doc, d, b"ColorSpace", b"CS")?;
    let cs = match cs {
        Object::Name(n) => match n.as_slice() {
            b"G" | b"DeviceGray" | b"CalGray" | b"I" | b"Indexed" => return Some(1),
            b"RGB" | b"DeviceRGB" | b"CalRGB" => return Some(3),
            b"CMYK" | b"DeviceCMYK" => return Some(4),
            other => resource(doc, res, b"ColorSpace", other)?,
        },
        other => other,
    };
    if let Object::Array(a) = cs {
        if let Some(Object::Name(n)) = a.first() {
            if n == b"I" || n == b"Indexed" {
                return Some(1);
            }
        }
    }
    ColorSpace::parse(doc, cs, res).map(|c| c.components())
}

/// Rewrite `content` as the module docs describe.
pub fn prepare<'a>(content: &'a [u8], doc: &Document, res: Option<&Dictionary>) -> Prepared<'a> {
    let mut out: Option<Vec<u8>> = None;
    let mut inline = Vec::new();
    let mut copied = 0usize; // content[..copied] is already in `out`
    let mut i = 0usize;
    while i < content.len() {
        if is_white(content[i]) {
            i += 1;
            continue;
        }
        let (next, tok) = skip_item(content, i);
        let Some(tok) = tok else {
            i = next;
            continue;
        };
        match tok {
            b"d0" | b"d1" => {
                let o = out.get_or_insert_with(|| Vec::with_capacity(content.len() + 64));
                o.extend_from_slice(&content[copied..i]);
                o.extend_from_slice(if tok == b"d0" { b"dZero" } else { b"dOne" });
                copied = next;
                i = next;
            }
            b"BI" => {
                // The header runs to the `ID` token; the data starts one
                // byte after it.
                let header_start = next;
                let mut j = next;
                let mut id_end = None;
                while j < content.len() {
                    if is_white(content[j]) {
                        j += 1;
                        continue;
                    }
                    let (n2, t2) = skip_item(content, j);
                    if t2 == Some(b"ID".as_slice()) {
                        id_end = Some((j, n2));
                        break;
                    }
                    j = n2;
                }
                let Some((id_start, id_end)) = id_end else {
                    i = next;
                    continue;
                };
                let header = &content[header_start..id_start];
                let dict = parse_header(header);
                let data_start = (id_end + 1).min(content.len());
                // Unfiltered data has a computed length; otherwise (or when
                // the computed end is not followed by `EI`) the delimiter
                // search decides.
                let mut span = None;
                if let Some(d) = &dict {
                    let filtered = get2(doc, d, b"Filter", b"F").is_some();
                    if let Some(len) = get_int2(doc, d, b"Length", b"L") {
                        let end = data_start.saturating_add(len.max(0) as usize);
                        if end <= content.len() {
                            if let Some(ei) = ei_at(content, end) {
                                span = Some((end, ei));
                            }
                        }
                    }
                    if span.is_none() && !filtered {
                        let w = get_int2(doc, d, b"Width", b"W").unwrap_or(0).max(0) as usize;
                        let h = get_int2(doc, d, b"Height", b"H").unwrap_or(0).max(0) as usize;
                        let bpc = if get_bool2(doc, d, b"ImageMask", b"IM") == Some(true) {
                            1
                        } else {
                            get_int2(doc, d, b"BitsPerComponent", b"BPC")
                                .unwrap_or(8)
                                .max(1) as usize
                        };
                        if let Some(nc) = components(doc, d, res) {
                            let len = (w * nc * bpc).div_ceil(8) * h;
                            let end = data_start.saturating_add(len);
                            if end <= content.len() {
                                if let Some(ei) = ei_at(content, end) {
                                    span = Some((end, ei));
                                }
                            }
                        }
                    }
                }
                if span.is_none() {
                    span = find_ei(content, data_start);
                }
                let Some((data_end, ei_end)) = span else {
                    // No terminator: the rest of the stream is image data.
                    i = content.len();
                    continue;
                };
                let o = out.get_or_insert_with(|| Vec::with_capacity(content.len() + 64));
                o.extend_from_slice(&content[copied..i]);
                if let Some(d) = dict {
                    o.extend_from_slice(format!(" /I{} BIX ", inline.len()).as_bytes());
                    inline.push(Stream::new(d, content[data_start..data_end].to_vec()));
                } else {
                    o.push(b' ');
                }
                copied = ei_end;
                i = ei_end;
            }
            _ => i = next,
        }
    }
    let content = match out {
        Some(mut o) => {
            o.extend_from_slice(&content[copied..]);
            Cow::Owned(o)
        }
        None => Cow::Borrowed(content),
    };
    Prepared { content, inline }
}

/// The inline image `/I<n> BIX` names.
pub fn inline_index(operand: &Object) -> Option<usize> {
    let n = name(operand)?;
    n.strip_prefix(b"I")
        .and_then(|s| std::str::from_utf8(s).ok())
        .and_then(|s| s.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> Document {
        Document::with_version("1.5")
    }

    #[test]
    fn passes_plain_content_through_borrowed() {
        let c = b"q 1 0 0 1 0 0 cm (BI ID EI d1) Tj Q";
        let p = prepare(c, &doc(), None);
        assert!(matches!(p.content, Cow::Borrowed(_)));
        assert!(p.inline.is_empty());
    }

    #[test]
    fn rewrites_type3_operators() {
        let p = prepare(b"1000 0 0 0 750 750 d1 0 0 750 750 re f", &doc(), None);
        assert_eq!(
            &*p.content,
            &b"1000 0 0 0 750 750 dOne 0 0 750 750 re f"[..]
        );
        let p = prepare(b"1000 0 d0\n", &doc(), None);
        assert_eq!(&*p.content, &b"1000 0 dZero\n"[..]);
        // `d0`/`d1` inside a string or as part of a longer token are left.
        let p = prepare(b"(d1) Tj /d1 gs xd1 cm", &doc(), None);
        assert!(matches!(p.content, Cow::Borrowed(_)));
    }

    #[test]
    fn cuts_inline_images_by_length() {
        // 2 × 1 gray, 8 bpc: two data bytes — one of them `E`, the other `I`.
        let mut c = b"q BI /W 2 /H 1 /CS /G /BPC 8 ID ".to_vec();
        c.extend_from_slice(b"EI");
        c.extend_from_slice(b" EI Q");
        let p = prepare(&c, &doc(), None);
        assert_eq!(&*p.content, &b"q  /I0 BIX  Q"[..]);
        assert_eq!(p.inline.len(), 1);
        assert_eq!(p.inline[0].content, b"EI");
        assert_eq!(p.inline[0].dict.get(b"W").unwrap().as_i64().unwrap(), 2);
        let ops = lopdf::content::Content::decode(&p.content).unwrap();
        let names: Vec<_> = ops.operations.iter().map(|o| o.operator.as_str()).collect();
        assert_eq!(names, ["q", "BIX", "Q"]);
        assert_eq!(inline_index(&ops.operations[1].operands[0]), Some(0));
    }

    #[test]
    fn cuts_filtered_inline_images_at_the_delimiter() {
        let c = b"BI /W 4 /H 4 /CS /RGB /BPC 8 /F /AHx ID\n00ff00 ff0000 >\nEI\n0 g 0 0 1 1 re f";
        let p = prepare(c, &doc(), None);
        assert_eq!(p.inline.len(), 1);
        assert_eq!(p.inline[0].content, b"00ff00 ff0000 >");
        let ops = lopdf::content::Content::decode(&p.content).unwrap();
        let names: Vec<_> = ops.operations.iter().map(|o| o.operator.as_str()).collect();
        assert_eq!(names, ["BIX", "g", "re", "f"]);
    }

    #[test]
    fn indexed_and_mask_headers_count_one_component() {
        let d = doc();
        let mut c = b"BI /W 8 /H 1 /IM true /D [1 0] ID ".to_vec();
        c.push(0xAA);
        c.extend_from_slice(b" EI");
        let p = prepare(&c, &d, None);
        assert_eq!(p.inline.len(), 1);
        assert_eq!(p.inline[0].content, [0xAA]);
        let mut c = b"BI /W 2 /H 1 /BPC 8 /CS [/I /RGB 1 <000000ffffff>] ID ".to_vec();
        c.extend_from_slice(&[0, 1]);
        c.extend_from_slice(b" EI ");
        let p = prepare(&c, &d, None);
        assert_eq!(p.inline.len(), 1);
        assert_eq!(p.inline[0].content, [0, 1]);
    }
}
