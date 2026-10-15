//! `CCITTFaxDecode` — a port of pdfium's `FaxDecoder`
//! (core/fxcodec/fax/faxmodule.cpp): Group 3 one- and two-dimensional and
//! Group 4 coding, `EncodedByteAlign`, `EndOfLine`, `BlackIs1`.
//!
//! Ported statement for statement rather than written from T.4/T.6, because
//! what matters here is what pdfium draws, including its behaviour on the
//! ragged edges — a row the data runs out in is left as far as it got, a
//! run longer than the row ends the row, byte alignment switches itself off
//! for good the first time a padding bit is set — and pdfium's own bit
//! convention: the scanline starts as all ones (white), black runs clear
//! bits, and `BlackIs1` inverts the finished row.

/// The filter's `/DecodeParms`, with pdfium's defaults
/// (`fpdf_parser_decode.cpp CreateFaxDecoder`).
#[derive(Debug, Clone, Copy)]
pub struct Params {
    pub k: i32,
    pub end_of_line: bool,
    pub byte_align: bool,
    pub black_is_1: bool,
    /// `/Columns`, default 1728.
    pub columns: usize,
    /// `/Rows`, 0 when absent (the image height then bounds the decode).
    pub rows: usize,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            k: 0,
            end_of_line: false,
            byte_align: false,
            black_is_1: false,
            columns: 1728,
            rows: 0,
        }
    }
}

/// Decode up to `height` rows of `columns` pixels. Each entry is one row of
/// `pitch()` bytes (bit 1 = white unless `black_is_1`), or `None` where
/// pdfium's `GetNextLine` returns nothing — the data ran out, or the row is
/// past `/Rows`; `CPDF_DIB` then hands out a zero row.
pub fn decode(src: &[u8], p: &Params, height: usize) -> Vec<Option<Vec<u8>>> {
    let columns = p.columns;
    let pitch = pitch(columns);
    let limit = if p.rows > 0 {
        p.rows.min(height)
    } else {
        height
    };
    let bitsize = src.len() * 8;
    let mut bitpos = 0usize;
    let mut byte_align = p.byte_align;
    let mut reference = vec![0xffu8; pitch];
    let mut out = Vec::with_capacity(height);
    for row in 0..height {
        if row >= limit {
            out.push(None);
            continue;
        }
        skip_eol(src, &mut bitpos);
        if bitpos >= bitsize {
            out.push(None);
            continue;
        }
        let mut line = vec![0xffu8; pitch];
        if p.k < 0 {
            g4_row(src, &mut bitpos, &mut line, &reference, columns);
            reference.copy_from_slice(&line);
        } else if p.k == 0 {
            line_1d(src, &mut bitpos, &mut line, columns);
        } else {
            if next_bit(src, &mut bitpos) {
                line_1d(src, &mut bitpos, &mut line, columns);
            } else {
                g4_row(src, &mut bitpos, &mut line, &reference, columns);
            }
            reference.copy_from_slice(&line);
        }
        if p.end_of_line {
            skip_eol(src, &mut bitpos);
        }
        if byte_align && bitpos < bitsize {
            let mut bitpos0 = bitpos;
            let bitpos1 = (bitpos + 7) & !7;
            while byte_align && bitpos0 < bitpos1 {
                if src[bitpos0 / 8] & (1 << (7 - bitpos0 % 8)) != 0 {
                    byte_align = false;
                } else {
                    bitpos0 += 1;
                }
            }
            if byte_align {
                bitpos = bitpos1;
            }
        }
        if p.black_is_1 {
            for b in &mut line {
                *b = !*b;
            }
        }
        out.push(Some(line));
    }
    out
}

/// Bytes per decoded row: pdfium's 32-bit-aligned pitch for 1 bpp.
pub fn pitch(columns: usize) -> usize {
    columns.div_ceil(32) * 4
}

#[inline]
fn next_bit(src: &[u8], bitpos: &mut usize) -> bool {
    let pos = *bitpos;
    *bitpos += 1;
    src.get(pos / 8)
        .is_some_and(|b| b & (1 << (7 - pos % 8)) != 0)
}

/// `FindBit`: the first position in `[start, max)` whose bit equals `bit`,
/// or `max`.
fn find_bit(buf: &[u8], max: usize, start: i32, bit: bool) -> usize {
    let start = start.max(0) as usize;
    if start >= max {
        return max;
    }
    let xor: u8 = if bit { 0x00 } else { 0xff };
    let mut pos = start;
    // Finish the partial first byte bit by bit, then skip whole bytes that
    // hold no match, then the tail bit by bit.
    while pos < max && !pos.is_multiple_of(8) {
        if ((buf[pos / 8] ^ xor) >> (7 - pos % 8)) & 1 == 1 {
            return pos;
        }
        pos += 1;
    }
    while pos + 8 <= max {
        let b = buf[pos / 8] ^ xor;
        if b != 0 {
            return pos + b.leading_zeros() as usize;
        }
        pos += 8;
    }
    while pos < max {
        if ((buf[pos / 8] ^ xor) >> (7 - pos % 8)) & 1 == 1 {
            return pos;
        }
        pos += 1;
    }
    max
}

/// `FaxG4FindB1B2`: the first changing element on the reference line to the
/// right of `a0` with the opposite colour of `a0color`, and the next one.
fn find_b1_b2(reference: &[u8], columns: usize, a0: i32, a0color: bool) -> (usize, usize) {
    let mut first_bit = a0 < 0 || (reference[a0 as usize / 8] & (1 << (7 - a0 as usize % 8))) != 0;
    let mut b1 = find_bit(reference, columns, a0 + 1, !first_bit);
    if b1 >= columns {
        return (columns, columns);
    }
    if first_bit != a0color {
        b1 = find_bit(reference, columns, b1 as i32 + 1, first_bit);
        first_bit = !first_bit;
    }
    if b1 >= columns {
        return (columns, columns);
    }
    let b2 = find_bit(reference, columns, b1 as i32 + 1, first_bit);
    (b1, b2)
}

/// `FaxFillBits`: clear (blacken) `[startpos, endpos)` — by subtraction, as
/// pdfium does, so an overlapping fill in a damaged stream wraps the same way.
fn fill_bits(dest: &mut [u8], columns: usize, startpos: i32, endpos: i32) {
    let startpos = startpos.max(0) as usize;
    let endpos = endpos.clamp(0, columns as i32) as usize;
    if startpos >= endpos {
        return;
    }
    let first_byte = startpos / 8;
    let last_byte = (endpos - 1) / 8;
    if first_byte == last_byte {
        for i in startpos % 8..=(endpos - 1) % 8 {
            dest[first_byte] = dest[first_byte].wrapping_sub(1 << (7 - i));
        }
        return;
    }
    for i in startpos % 8..8 {
        dest[first_byte] = dest[first_byte].wrapping_sub(1 << (7 - i));
    }
    for i in 0..=(endpos - 1) % 8 {
        dest[last_byte] = dest[last_byte].wrapping_sub(1 << (7 - i));
    }
    if last_byte > first_byte + 1 {
        dest[first_byte + 1..last_byte].fill(0);
    }
}

/// `FaxGetRun`: one run length from the modified-Huffman tables below, read
/// bit by bit; `None` at an invalid code or the end of the data.
fn get_run(ins: &[u8], src: &[u8], bitpos: &mut usize) -> Option<u16> {
    let bitsize = src.len() * 8;
    let mut code: u32 = 0;
    let mut off = 0usize;
    loop {
        let n = *ins.get(off)?;
        off += 1;
        if n == 0xff {
            return None;
        }
        if *bitpos >= bitsize {
            return None;
        }
        code <<= 1;
        if src[*bitpos / 8] & (1 << (7 - *bitpos % 8)) != 0 {
            code += 1;
        }
        *bitpos += 1;
        let next = off + usize::from(n) * 3;
        while off < next {
            if u32::from(ins[off]) == code {
                return Some(u16::from(ins[off + 1]) + u16::from(ins[off + 2]) * 256);
            }
            off += 3;
        }
    }
}

/// `FaxG4GetRow`: one two-dimensionally coded row (T.6 table 1).
fn g4_row(src: &[u8], bitpos: &mut usize, dest: &mut [u8], reference: &[u8], columns: usize) {
    let mut a0: i32 = -1;
    let mut a0color = true;
    let bitsize = src.len() * 8;
    let cols = columns as i32;
    loop {
        if *bitpos >= bitsize {
            return;
        }
        let mut v_delta: i32 = 0;
        if !next_bit(src, bitpos) {
            if *bitpos >= bitsize {
                return;
            }
            let bit1 = next_bit(src, bitpos);
            if *bitpos >= bitsize {
                return;
            }
            let bit2 = next_bit(src, bitpos);
            if bit1 {
                // Vertical, VR(1) / VL(1).
                v_delta = if bit2 { 1 } else { -1 };
            } else if bit2 {
                // Horizontal.
                let mut run_len1: i32 = 0;
                loop {
                    let run = get_run(if a0color { WHITE_RUNS } else { BLACK_RUNS }, src, bitpos);
                    let Some(run) = run else { return };
                    run_len1 += i32::from(run);
                    if run_len1 > cols {
                        return;
                    }
                    if run < 64 {
                        break;
                    }
                }
                if a0 < 0 {
                    run_len1 += 1;
                }
                let a1 = a0 + run_len1;
                if !a0color {
                    fill_bits(dest, columns, a0, a1);
                }
                let mut run_len2: i32 = 0;
                loop {
                    let run = get_run(if a0color { BLACK_RUNS } else { WHITE_RUNS }, src, bitpos);
                    let Some(run) = run else { return };
                    run_len2 += i32::from(run);
                    if run_len2 > cols {
                        return;
                    }
                    if run < 64 {
                        break;
                    }
                }
                let a2 = a1 + run_len2;
                if a0color {
                    fill_bits(dest, columns, a1, a2);
                }
                a0 = a2;
                if a0 < cols {
                    continue;
                }
                return;
            } else {
                if *bitpos >= bitsize {
                    return;
                }
                if next_bit(src, bitpos) {
                    // Pass.
                    let (_, b2) = find_b1_b2(reference, columns, a0, a0color);
                    if !a0color {
                        fill_bits(dest, columns, a0, b2 as i32);
                    }
                    if b2 >= columns {
                        return;
                    }
                    a0 = b2 as i32;
                    continue;
                }
                if *bitpos >= bitsize {
                    return;
                }
                let next_bit1 = next_bit(src, bitpos);
                if *bitpos >= bitsize {
                    return;
                }
                let next_bit2 = next_bit(src, bitpos);
                if next_bit1 {
                    // Vertical, VR(2) / VL(2).
                    v_delta = if next_bit2 { 2 } else { -2 };
                } else if next_bit2 {
                    if *bitpos >= bitsize {
                        return;
                    }
                    // Vertical, VR(3) / VL(3).
                    v_delta = if next_bit(src, bitpos) { 3 } else { -3 };
                } else {
                    if *bitpos >= bitsize {
                        return;
                    }
                    // Extension.
                    if next_bit(src, bitpos) {
                        *bitpos += 3;
                        continue;
                    }
                    *bitpos += 5;
                    return;
                }
            }
        }
        // Vertical, V(0) falls through with v_delta 0.
        let (b1, _) = find_b1_b2(reference, columns, a0, a0color);
        let a1 = b1 as i32 + v_delta;
        if !a0color {
            fill_bits(dest, columns, a0, a1);
        }
        if a1 >= cols {
            return;
        }
        // The position of picture elements must increase monotonically.
        if a0 >= a1 {
            return;
        }
        a0 = a1;
        a0color = !a0color;
    }
}

/// `FaxSkipEOL`: skip an EOL code (eleven zeros and a one) when one sits at
/// the current position; a one within the first eleven bits is data and
/// rewinds.
fn skip_eol(src: &[u8], bitpos: &mut usize) {
    let bitsize = src.len() * 8;
    let start = *bitpos;
    while *bitpos < bitsize {
        if !next_bit(src, bitpos) {
            continue;
        }
        if *bitpos - start <= 11 {
            *bitpos = start;
        }
        return;
    }
}

/// `FaxGet1DLine`: one modified-Huffman row.
fn line_1d(src: &[u8], bitpos: &mut usize, dest: &mut [u8], columns: usize) {
    let bitsize = src.len() * 8;
    let mut color = true;
    let mut startpos: i32 = 0;
    let cols = columns as i32;
    loop {
        if *bitpos >= bitsize {
            return;
        }
        let mut run_len: i32 = 0;
        loop {
            let run = get_run(if color { WHITE_RUNS } else { BLACK_RUNS }, src, bitpos);
            let Some(run) = run else {
                // Invalid code: skip to the next set bit and give up the row.
                while *bitpos < bitsize {
                    if next_bit(src, bitpos) {
                        return;
                    }
                }
                return;
            };
            run_len += i32::from(run);
            if run_len > cols {
                return;
            }
            if run < 64 {
                break;
            }
        }
        if !color {
            fill_bits(dest, columns, startpos, startpos + run_len);
        }
        startpos += run_len;
        if startpos >= cols {
            break;
        }
        color = !color;
    }
}

// pdfium's run-length decoding tables (`kFaxBlackRunIns` / `kFaxWhiteRunIns`):
// for each code length in turn, a count of entries followed by
// (code, run low byte, run high byte) triples; 0xff ends the table.
static BLACK_RUNS: &[u8] = &[
    0, 2, 0x02, 3, 0, 0x03, 2, 0, 2, 0x02, 1, 0, 0x03, 4, 0, 2, 0x02, 6, 0, 0x03, 5, 0, 1, 0x03, 7,
    0, 2, 0x04, 9, 0, 0x05, 8, 0, 3, 0x04, 10, 0, 0x05, 11, 0, 0x07, 12, 0, 2, 0x04, 13, 0, 0x07,
    14, 0, 1, 0x18, 15, 0, 5, 0x08, 18, 0, 0x0f, 64, 0, 0x17, 16, 0, 0x18, 17, 0, 0x37, 0, 0, 10,
    0x08, 0x00, 0x07, 0x0c, 0x40, 0x07, 0x0d, 0x80, 0x07, 0x17, 24, 0, 0x18, 25, 0, 0x28, 23, 0,
    0x37, 22, 0, 0x67, 19, 0, 0x68, 20, 0, 0x6c, 21, 0, 54, 0x12, 192, 7, 0x13, 0, 8, 0x14, 64, 8,
    0x15, 128, 8, 0x16, 192, 8, 0x17, 0, 9, 0x1c, 64, 9, 0x1d, 128, 9, 0x1e, 192, 9, 0x1f, 0, 10,
    0x24, 52, 0, 0x27, 55, 0, 0x28, 56, 0, 0x2b, 59, 0, 0x2c, 60, 0, 0x33, 64, 1, 0x34, 128, 1,
    0x35, 192, 1, 0x37, 53, 0, 0x38, 54, 0, 0x52, 50, 0, 0x53, 51, 0, 0x54, 44, 0, 0x55, 45, 0,
    0x56, 46, 0, 0x57, 47, 0, 0x58, 57, 0, 0x59, 58, 0, 0x5a, 61, 0, 0x5b, 0, 1, 0x64, 48, 0, 0x65,
    49, 0, 0x66, 62, 0, 0x67, 63, 0, 0x68, 30, 0, 0x69, 31, 0, 0x6a, 32, 0, 0x6b, 33, 0, 0x6c, 40,
    0, 0x6d, 41, 0, 0xc8, 128, 0, 0xc9, 192, 0, 0xca, 26, 0, 0xcb, 27, 0, 0xcc, 28, 0, 0xcd, 29, 0,
    0xd2, 34, 0, 0xd3, 35, 0, 0xd4, 36, 0, 0xd5, 37, 0, 0xd6, 38, 0, 0xd7, 39, 0, 0xda, 42, 0,
    0xdb, 43, 0, 20, 0x4a, 128, 2, 0x4b, 192, 2, 0x4c, 0, 3, 0x4d, 64, 3, 0x52, 0, 5, 0x53, 64, 5,
    0x54, 128, 5, 0x55, 192, 5, 0x5a, 0, 6, 0x5b, 64, 6, 0x64, 128, 6, 0x65, 192, 6, 0x6c, 0, 2,
    0x6d, 64, 2, 0x72, 128, 3, 0x73, 192, 3, 0x74, 0, 4, 0x75, 64, 4, 0x76, 128, 4, 0x77, 192, 4,
    0xff,
];

static WHITE_RUNS: &[u8] = &[
    0, 0, 0, 6, 0x07, 2, 0, 0x08, 3, 0, 0x0B, 4, 0, 0x0C, 5, 0, 0x0E, 6, 0, 0x0F, 7, 0, 6, 0x07,
    10, 0, 0x08, 11, 0, 0x12, 128, 0, 0x13, 8, 0, 0x14, 9, 0, 0x1b, 64, 0, 9, 0x03, 13, 0, 0x07, 1,
    0, 0x08, 12, 0, 0x17, 192, 0, 0x18, 128, 6, 0x2a, 16, 0, 0x2B, 17, 0, 0x34, 14, 0, 0x35, 15, 0,
    12, 0x03, 22, 0, 0x04, 23, 0, 0x08, 20, 0, 0x0c, 19, 0, 0x13, 26, 0, 0x17, 21, 0, 0x18, 28, 0,
    0x24, 27, 0, 0x27, 18, 0, 0x28, 24, 0, 0x2B, 25, 0, 0x37, 0, 1, 42, 0x02, 29, 0, 0x03, 30, 0,
    0x04, 45, 0, 0x05, 46, 0, 0x0a, 47, 0, 0x0b, 48, 0, 0x12, 33, 0, 0x13, 34, 0, 0x14, 35, 0,
    0x15, 36, 0, 0x16, 37, 0, 0x17, 38, 0, 0x1a, 31, 0, 0x1b, 32, 0, 0x24, 53, 0, 0x25, 54, 0,
    0x28, 39, 0, 0x29, 40, 0, 0x2a, 41, 0, 0x2b, 42, 0, 0x2c, 43, 0, 0x2d, 44, 0, 0x32, 61, 0,
    0x33, 62, 0, 0x34, 63, 0, 0x35, 0, 0, 0x36, 64, 1, 0x37, 128, 1, 0x4a, 59, 0, 0x4b, 60, 0,
    0x52, 49, 0, 0x53, 50, 0, 0x54, 51, 0, 0x55, 52, 0, 0x58, 55, 0, 0x59, 56, 0, 0x5a, 57, 0,
    0x5b, 58, 0, 0x64, 192, 1, 0x65, 0, 2, 0x67, 128, 2, 0x68, 64, 2, 16, 0x98, 192, 5, 0x99, 0, 6,
    0x9a, 64, 6, 0x9b, 192, 6, 0xcc, 192, 2, 0xcd, 0, 3, 0xd2, 64, 3, 0xd3, 128, 3, 0xd4, 192, 3,
    0xd5, 0, 4, 0xd6, 64, 4, 0xd7, 128, 4, 0xd8, 192, 4, 0xd9, 0, 5, 0xda, 64, 5, 0xdb, 128, 5, 0,
    3, 0x08, 0, 7, 0x0c, 64, 7, 0x0d, 128, 7, 10, 0x12, 192, 7, 0x13, 0, 8, 0x14, 64, 8, 0x15, 128,
    8, 0x16, 192, 8, 0x17, 0, 9, 0x1c, 64, 9, 0x1d, 128, 9, 0x1e, 192, 9, 0x1f, 0, 10, 0xff,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data/fax")
            .join(name);
        std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    }

    /// The reference bilevel image as rows of packed bits in the decoder's
    /// convention (1 = white), i.e. the *inverse* of the PNG's set pixels:
    /// libtiff encoded Pillow's `1`-mode image with its set bits as the
    /// "black" runs (TIFF PhotometricInterpretation 1, min-is-black), which
    /// pdfium — and this port — hand back as cleared bits.
    fn reference() -> (usize, usize, Vec<Vec<u8>>) {
        let img = image::open(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/fax/shapes.png"),
        )
        .unwrap()
        .to_luma8();
        let (w, h) = (img.width() as usize, img.height() as usize);
        let rows = (0..h)
            .map(|y| {
                let mut row = vec![0u8; pitch(w)];
                for x in 0..w {
                    if img.get_pixel(x as u32, y as u32)[0] < 128 {
                        row[x / 8] |= 1 << (7 - x % 8);
                    }
                }
                row
            })
            .collect();
        (w, h, rows)
    }

    /// The first `w` bits of a row (the padding past the last column is
    /// whatever the decoder's 0xff fill left there).
    fn row_bits(row: &[u8], w: usize) -> Vec<u8> {
        let mut v = row[..w.div_ceil(8)].to_vec();
        if !w.is_multiple_of(8) {
            *v.last_mut().unwrap() &= 0xffu8 << (8 - w % 8);
        }
        v
    }

    fn check(name: &str, k: i32) {
        let (w, h, want) = reference();
        let p = Params {
            k,
            columns: w,
            rows: h,
            ..Params::default()
        };
        let rows = decode(&fixture(name), &p, h);
        assert_eq!(rows.len(), h);
        let bad: Vec<usize> = (0..h)
            .filter(|&y| rows[y].as_deref().map(|r| row_bits(r, w)) != Some(row_bits(&want[y], w)))
            .collect();
        assert!(bad.is_empty(), "{name}: rows differ: {bad:?}");
    }

    /// Pillow's libtiff encodings of the same bilevel image decode back to
    /// it: Group 4, Group 3 one-dimensional, Group 3 two-dimensional.
    #[test]
    fn decodes_group4_and_group3_fixtures() {
        check("shapes.g4", -1);
        check("shapes.g3", 0);
        check("shapes.g3_2d", 4);
    }

    /// `BlackIs1` inverts the finished rows; rows past `/Rows` or past the
    /// data are `None`.
    #[test]
    fn black_is_1_and_missing_rows() {
        let (w, h, want) = reference();
        let p = Params {
            k: -1,
            columns: w,
            rows: 10,
            black_is_1: true,
            ..Params::default()
        };
        let rows = decode(&fixture("shapes.g4"), &p, h);
        let inverted: Vec<u8> = rows[0].as_deref().unwrap().iter().map(|b| !b).collect();
        assert_eq!(row_bits(&inverted, w), row_bits(&want[0], w));
        assert!(rows[10].is_none() && rows[h - 1].is_none());
        let truncated = decode(
            &fixture("shapes.g4")[..40],
            &Params {
                k: -1,
                columns: w,
                ..Params::default()
            },
            h,
        );
        assert!(truncated[0].is_some() && truncated[h - 1].is_none());
    }
}
