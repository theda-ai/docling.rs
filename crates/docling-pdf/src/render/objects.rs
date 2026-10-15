//! Small lopdf helpers shared by the renderer's modules.

use lopdf::{Dictionary, Document, Object};

/// Follow references (one level is enough: lopdf never nests them).
pub fn deref<'a>(doc: &'a Document, obj: &'a Object) -> &'a Object {
    match obj {
        Object::Reference(id) => doc.get_object(*id).unwrap_or(obj),
        o => o,
    }
}

pub fn as_dict<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a Dictionary> {
    match deref(doc, obj) {
        Object::Dictionary(d) => Some(d),
        Object::Stream(s) => Some(&s.dict),
        _ => None,
    }
}

pub fn as_stream<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a lopdf::Stream> {
    match deref(doc, obj) {
        Object::Stream(s) => Some(s),
        _ => None,
    }
}

pub fn num(o: &Object) -> Option<f64> {
    match o {
        Object::Integer(i) => Some(*i as f64),
        Object::Real(r) => Some(f64::from(*r)),
        _ => None,
    }
}

/// An array of numbers (references resolved), `None` if not an array.
pub fn nums(doc: &Document, o: &Object) -> Option<Vec<f64>> {
    match deref(doc, o) {
        Object::Array(a) => Some(a.iter().filter_map(|x| num(deref(doc, x))).collect()),
        _ => None,
    }
}

pub fn name(o: &Object) -> Option<&[u8]> {
    match o {
        Object::Name(n) => Some(n),
        _ => None,
    }
}

/// `dict[key]` dereferenced.
pub fn get<'a>(doc: &'a Document, dict: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
    dict.get(key).ok().map(|o| deref(doc, o))
}

/// `dict[key]` or `dict[alt]` (inline-image abbreviations), dereferenced.
pub fn get2<'a>(
    doc: &'a Document,
    dict: &'a Dictionary,
    key: &[u8],
    alt: &[u8],
) -> Option<&'a Object> {
    get(doc, dict, key).or_else(|| get(doc, dict, alt))
}

/// `get_int` over `key` or its inline-image abbreviation `alt`.
pub fn get_int2(doc: &Document, d: &Dictionary, key: &[u8], alt: &[u8]) -> Option<i64> {
    get_int(doc, d, key).or_else(|| get_int(doc, d, alt))
}

/// `get_bool` over `key` or its inline-image abbreviation `alt`.
pub fn get_bool2(doc: &Document, d: &Dictionary, key: &[u8], alt: &[u8]) -> Option<bool> {
    get_bool(doc, d, key).or_else(|| get_bool(doc, d, alt))
}

pub fn get_num(doc: &Document, dict: &Dictionary, key: &[u8]) -> Option<f64> {
    get(doc, dict, key).and_then(num)
}

pub fn get_int(doc: &Document, dict: &Dictionary, key: &[u8]) -> Option<i64> {
    get(doc, dict, key).and_then(|o| match o {
        Object::Integer(i) => Some(*i),
        Object::Real(r) => Some(*r as i64),
        _ => None,
    })
}

pub fn get_bool(doc: &Document, dict: &Dictionary, key: &[u8]) -> Option<bool> {
    get(doc, dict, key).and_then(|o| match o {
        Object::Boolean(b) => Some(*b),
        Object::Integer(i) => Some(*i != 0),
        _ => None,
    })
}

pub fn get_name<'a>(doc: &'a Document, dict: &'a Dictionary, key: &[u8]) -> Option<&'a [u8]> {
    get(doc, dict, key).and_then(name)
}

pub fn get_dict<'a>(doc: &'a Document, dict: &'a Dictionary, key: &[u8]) -> Option<&'a Dictionary> {
    dict.get(key).ok().and_then(|o| as_dict(doc, o))
}

/// A resource `kind`/`name` from a resources dictionary.
pub fn resource<'a>(
    doc: &'a Document,
    res: Option<&'a Dictionary>,
    kind: &[u8],
    name: &[u8],
) -> Option<&'a Object> {
    res?.get(kind)
        .ok()
        .and_then(|o| as_dict(doc, o))
        .and_then(|d| d.get(name).ok())
}

/// The decoded bytes of a stream, `None` on a filter this build cannot undo.
pub fn stream_data(s: &lopdf::Stream) -> Option<Vec<u8>> {
    s.decompressed_content().ok()
}
