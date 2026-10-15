//! Encrypted ("password protected") Office documents — [MS-OFFCRYPTO].
//!
//! Every Office format marks encryption differently: an OOXML package
//! (`.docx`/`.xlsx`/`.pptx`) is no longer a ZIP but a compound file holding
//! `EncryptionInfo` + `EncryptedPackage`; a `.doc` sets `fEncrypted` in its
//! FIB; an `.xls` carries a `FILEPASS` record; a `.ppt` references a
//! `CryptSession10Container` from its current user edit. Each is recognized
//! here or by its backend and reported with the same error (#624), so an
//! encrypted file never converts to an empty document or fails as "bad zip".
//!
//! [`unlock`] decrypts them (#625) — a docling.rs extension, docling reads
//! no encrypted Office file. It runs before dispatch and hands the backend
//! an ordinary file:
//!
//! - **OOXML**, Agile (2.3.4.10, Office 2010+: AES, SHA-1/256/384/512, a
//!   spin count) and Standard encryption (2.3.4.5, Office 2007: AES-ECB,
//!   SHA-1 × 50 000): `EncryptedPackage` decrypts to the ZIP the backend
//!   reads anyway. The `dataIntegrity` HMAC is not checked.
//! - **`.doc` / `.xls` / `.ppt`**, RC4 CryptoAPI (2.3.5, Office 2002+) and
//!   RC4 (2.3.6, Office 97/2000): each encrypted stream is decrypted and
//!   written back over its own sectors ([`CompoundFile::stream_spans`]) with
//!   the encryption marker cleared, so the backend reads the same container
//!   it would have read unencrypted — the procedure msoffcrypto-tool follows.
//!   The `.ppt` `Pictures` stream and the encrypted summary-information
//!   streams are left as they are; XOR obfuscation is reported as
//!   unsupported.
//!
//! The password is the converter's one password (`password`, which opens
//! encrypted PDFs too; `pdf_password` is docling's name for it). Without one, or when it does not
//! open the file, the format's documented default password is tried, as
//! Office does: Excel encrypts a workbook whose only protection is
//! structural with `VelvetSweatshop`, and PowerPoint encrypts a presentation
//! that has only a *modify* password with `/01Hannes Ruescher/01` — such a
//! file opens without prompting in Office, and converts here without a
//! password. Header fields that size work or allocations (spin count, key,
//! salt, block and hash sizes) are checked against the spec's limits before
//! any hashing.

use std::ops::Range;

use aes::cipher::{BlockCipherDecrypt, KeyInit};
use base64::Engine as _;
use sha2::Digest;

use crate::backend::cfb::CompoundFile;
use crate::error::{ConversionError, EncryptionError};
use crate::format::InputFormat;
use crate::source::SourceDocument;

/// Why an encrypted document could not be opened — the decryptor's own
/// account, with the spec field names its checks fail on. It reaches the
/// caller as the public [`EncryptionError`] (#636) on the error's source
/// chain, which `is_encryption_error` and `ConversionError::encryption` read.
#[derive(Debug, PartialEq)]
pub(crate) enum CryptoError {
    /// No password was given and no default password opens it.
    NeedPassword,
    /// The given password (nor a default one) opens it.
    WrongPassword,
    /// An encryption this reader does not implement.
    Unsupported(String),
    /// The encryption header is damaged or out of the spec's bounds.
    Malformed(&'static str),
}

impl CryptoError {
    /// The public value (#636); `EncryptionError`'s text is this module's
    /// wording, so the message is the one #624 fixed.
    fn typed(self) -> EncryptionError {
        match self {
            CryptoError::NeedPassword => EncryptionError::NeedPassword,
            CryptoError::WrongPassword => EncryptionError::WrongPassword,
            CryptoError::Unsupported(what) => EncryptionError::NotDecryptable(what),
            CryptoError::Malformed(what) => EncryptionError::Malformed(what.into()),
        }
    }

    fn into_error(self, fmt: &str) -> ConversionError {
        ConversionError::encrypted(fmt, self.typed())
    }
}

/// The error for an encrypted `fmt` document no password was tried on — a
/// backend's own check (it is reached when [`unlock`] did not run, as for a
/// backend called directly).
pub(crate) fn encrypted(fmt: &str) -> ConversionError {
    CryptoError::NeedPassword.into_error(fmt)
}

/// The error for an encrypted `fmt` document in a scheme this reader does
/// not decrypt (ODF package encryption), so no password would help.
pub(crate) fn unsupported(fmt: &str, scheme: &str) -> ConversionError {
    CryptoError::Unsupported(scheme.into()).into_error(fmt)
}

/// Whether `err` says the document is encrypted (any backend's, #636) — the
/// converter prefers it over the "not a valid <format>" error of a
/// mislabelled file (an encrypted `.ppt` named `.pptx` fails as a ZIP
/// first).
pub(crate) fn is_encryption_error(err: &ConversionError) -> bool {
    err.encryption().is_some()
}

/// Whether `bytes` is an encrypted OOXML package ([MS-OFFCRYPTO] 2.3.4.4 /
/// 2.3.4.5): a compound file with `EncryptionInfo` and `EncryptedPackage`
/// streams in its root storage. Root only (#512): a document embedding an
/// encrypted one is not itself encrypted.
pub(crate) fn is_encrypted_package(bytes: &[u8]) -> bool {
    CompoundFile::detect(bytes)
        && CompoundFile::open(bytes).is_some_and(|cfb| {
            cfb.root_stream("EncryptionInfo").is_some()
                && cfb.root_stream("EncryptedPackage").is_some()
        })
}

/// Excel's default password ([MS-XLS] 2.2.10): a workbook protected only
/// against structural edits is encrypted with it.
const EXCEL_DEFAULT_PASSWORD: &str = "VelvetSweatshop";
/// PowerPoint's default password ([MS-PPT] 2.3.7): a presentation with a
/// modify password but no open password is encrypted with it.
const POWERPOINT_DEFAULT_PASSWORD: &str = "/01Hannes Ruescher/01";

/// Decrypt `source` when it is an encrypted Office document: `Ok(None)` for
/// anything else (a plain file, another format), `Ok(Some(bytes))` with the
/// decrypted file the format's backend reads, or the error saying why it
/// could not be opened. `password` is tried first, then the format's
/// default password.
pub(crate) fn unlock(
    source: &SourceDocument,
    password: Option<&str>,
) -> Result<Option<Vec<u8>>, ConversionError> {
    let bytes = &source.bytes;
    let fmt = source.format.as_str();
    let default: &[&str] = match source.format {
        InputFormat::Xls | InputFormat::Xlsx => &[EXCEL_DEFAULT_PASSWORD],
        InputFormat::Ppt => &[POWERPOINT_DEFAULT_PASSWORD],
        _ => &[],
    };
    let mut tries: Vec<&str> = password.into_iter().collect();
    tries.extend(default.iter().filter(|d| Some(**d) != password));
    let outcome = match source.format {
        InputFormat::Docx | InputFormat::Xlsx | InputFormat::Pptx | InputFormat::Visio
            if is_encrypted_package(bytes) =>
        {
            decrypt_package(bytes, &tries).map(Some)
        }
        InputFormat::Doc | InputFormat::Xls | InputFormat::Ppt if CompoundFile::detect(bytes) => {
            let Some(cfb) = CompoundFile::open(bytes) else {
                return Ok(None); // the backend reports the damage
            };
            match source.format {
                InputFormat::Doc => unlock_doc(bytes, &cfb, &tries),
                InputFormat::Xls => unlock_xls(bytes, &cfb, &tries),
                _ => unlock_ppt(bytes, &cfb, &tries),
            }
        }
        _ => Ok(None),
    };
    outcome
        .map_err(|e| match e {
            CryptoError::NeedPassword if password.is_some() => CryptoError::WrongPassword,
            e => e,
        })
        .map_err(|e| e.into_error(fmt))
}

/// Try each password with `open`; the first that verifies wins.
fn first_key<K>(
    tries: &[&str],
    mut open: impl FnMut(&[u8]) -> Result<Option<K>, CryptoError>,
) -> Result<K, CryptoError> {
    for pw in tries {
        let utf16: Vec<u8> = pw.encode_utf16().flat_map(u16::to_le_bytes).collect();
        if let Some(key) = open(&utf16)? {
            return Ok(key);
        }
    }
    Err(CryptoError::NeedPassword)
}

// --- primitives -----------------------------------------------------------

/// RC4 (ARC4) — the stream cipher of the binary formats' encryption. Ten
/// lines, so no crate.
struct Rc4 {
    s: [u8; 256],
    i: u8,
    j: u8,
}

impl Rc4 {
    fn new(key: &[u8]) -> Self {
        let mut s = [0u8; 256];
        for (i, v) in s.iter_mut().enumerate() {
            *v = i as u8;
        }
        let mut j = 0u8;
        for i in 0..256 {
            j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
            s.swap(i, j as usize);
        }
        Self { s, i: 0, j: 0 }
    }

    fn apply(&mut self, data: &mut [u8]) {
        for b in data {
            self.i = self.i.wrapping_add(1);
            self.j = self.j.wrapping_add(self.s[self.i as usize]);
            self.s.swap(self.i as usize, self.j as usize);
            let k = self.s[self.s[self.i as usize].wrapping_add(self.s[self.j as usize]) as usize];
            *b ^= k;
        }
    }
}

/// The hash functions [MS-OFFCRYPTO] names (Agile's `hashAlgorithm`).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Hash {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
    Md5,
}

impl Hash {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "SHA1" | "SHA-1" => Hash::Sha1,
            "SHA256" => Hash::Sha256,
            "SHA384" => Hash::Sha384,
            "SHA512" => Hash::Sha512,
            "MD5" => Hash::Md5,
            _ => return None,
        })
    }

    fn len(self) -> usize {
        match self {
            Hash::Sha1 => 20,
            Hash::Sha256 => 32,
            Hash::Sha384 => 48,
            Hash::Sha512 => 64,
            Hash::Md5 => 16,
        }
    }

    fn of(self, parts: &[&[u8]]) -> Vec<u8> {
        fn run<D: Digest>(parts: &[&[u8]]) -> Vec<u8> {
            let mut d = D::new();
            for p in parts {
                d.update(p);
            }
            d.finalize().to_vec()
        }
        match self {
            Hash::Sha1 => run::<sha1::Sha1>(parts),
            Hash::Sha256 => run::<sha2::Sha256>(parts),
            Hash::Sha384 => run::<sha2::Sha384>(parts),
            Hash::Sha512 => run::<sha2::Sha512>(parts),
            Hash::Md5 => run::<md5::Md5>(parts),
        }
    }

    /// `H(salt + password)`, then `H(iterator + H)` `spin` times — the
    /// password hash of Agile (2.3.4.11) and Standard (2.3.4.7) encryption.
    fn spin(self, salt: &[u8], password: &[u8], spin: u32) -> Vec<u8> {
        let mut h = self.of(&[salt, password]);
        for i in 0..spin {
            h = self.of(&[&i.to_le_bytes(), &h]);
        }
        h
    }
}

/// AES of any key size, decrypting in ECB or CBC mode without padding.
enum Aes {
    A128(aes::Aes128),
    A192(aes::Aes192),
    A256(aes::Aes256),
}

impl Aes {
    fn new(key: &[u8]) -> Option<Self> {
        Some(match key.len() {
            16 => Aes::A128(aes::Aes128::new_from_slice(key).ok()?),
            24 => Aes::A192(aes::Aes192::new_from_slice(key).ok()?),
            32 => Aes::A256(aes::Aes256::new_from_slice(key).ok()?),
            _ => return None,
        })
    }

    fn block(&self, b: &mut [u8]) {
        let block: &mut aes::Block = b.try_into().expect("16-byte AES block");
        match self {
            Aes::A128(c) => c.decrypt_block(block),
            Aes::A192(c) => c.decrypt_block(block),
            Aes::A256(c) => c.decrypt_block(block),
        }
    }

    /// Decrypt whole blocks in place (a trailing partial block is left as
    /// is — no producer writes one).
    fn ecb(&self, data: &mut [u8]) {
        for b in data.chunks_exact_mut(16) {
            self.block(b);
        }
    }

    fn cbc(&self, iv: &[u8], data: &mut [u8]) {
        let mut prev: [u8; 16] = iv[..16].try_into().expect("16-byte IV");
        for b in data.chunks_exact_mut(16) {
            let cipher: [u8; 16] = (&*b).try_into().expect("16-byte block");
            self.block(b);
            for (x, p) in b.iter_mut().zip(prev) {
                *x ^= p;
            }
            prev = cipher;
        }
    }
}

/// `buf` cut or padded with `pad` to `n` bytes (2.3.4.12's key and IV
/// sizing: a hash longer than the key is truncated, a shorter one padded
/// with 0x36).
fn fit(buf: &[u8], n: usize, pad: u8) -> Vec<u8> {
    let mut out = buf[..buf.len().min(n)].to_vec();
    out.resize(n, pad);
    out
}

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

// --- OOXML ----------------------------------------------------------------

/// The spec's upper bound on the spin count (2.3.4.11: "MUST NOT be
/// greater than 10,000,000") — checked before hashing, so a crafted file
/// cannot buy unbounded CPU.
const MAX_SPIN_COUNT: u32 = 10_000_000;
/// Agile's `saltSize`: 1–65536 in the schema; no producer writes more than
/// 16, and the hash input stays small with this cap.
const MAX_SALT: usize = 64;

/// Decrypt an encrypted OOXML package to its ZIP bytes.
fn decrypt_package(bytes: &[u8], tries: &[&str]) -> Result<Vec<u8>, CryptoError> {
    let cfb = CompoundFile::open(bytes).ok_or(CryptoError::Malformed("container"))?;
    let read = |name| {
        cfb.root_stream(name)
            .and_then(|i| cfb.stream_by_index(i))
            .ok_or(CryptoError::Malformed("stream unreadable"))
    };
    let info = read("EncryptionInfo")?;
    let package = read("EncryptedPackage")?;
    let (major, minor) = (
        u16_at(&info, 0).ok_or(CryptoError::Malformed("EncryptionInfo"))?,
        u16_at(&info, 2).ok_or(CryptoError::Malformed("EncryptionInfo"))?,
    );
    let size = package
        .get(..8)
        .map(|b| u64::from_le_bytes(b.try_into().expect("8 bytes")))
        .ok_or(CryptoError::Malformed("EncryptedPackage"))?;
    let data = &package[8..];
    // The declared size cannot exceed the ciphertext (the stream itself is
    // under the per-part budget).
    if size > data.len() as u64 {
        return Err(CryptoError::Malformed("EncryptedPackage size"));
    }
    let mut out = match (major, minor) {
        (4, 4) => {
            let agile = Agile::parse(info.get(8..).unwrap_or_default())?;
            let key = first_key(tries, |pw| agile.secret_key(pw))?;
            agile.decrypt(&key, data)?
        }
        (2..=4, 2) => {
            let standard = Standard::parse(&info)?;
            let key = first_key(tries, |pw| Ok(standard.key(pw)))?;
            let mut out = data.to_vec();
            Aes::new(&key)
                .ok_or(CryptoError::Malformed("key size"))?
                .ecb(&mut out);
            out
        }
        (3 | 4, 3) => return Err(CryptoError::Unsupported("extensible encryption".into())),
        (major, minor) => {
            return Err(CryptoError::Unsupported(format!(
                "EncryptionInfo version {major}.{minor}"
            )))
        }
    };
    out.truncate(size as usize);
    Ok(out)
}

/// An attribute of the Agile XML, or the error naming it.
fn attr<'a>(e: roxmltree::Node<'a, '_>, name: &'static str) -> Result<&'a str, CryptoError> {
    e.attribute(name).ok_or(CryptoError::Malformed(name))
}

fn b64(e: roxmltree::Node, name: &'static str) -> Result<Vec<u8>, CryptoError> {
    base64::engine::general_purpose::STANDARD
        .decode(attr(e, name)?.trim())
        .map_err(|_| CryptoError::Malformed(name))
}

fn num(e: roxmltree::Node, name: &'static str) -> Result<u32, CryptoError> {
    attr(e, name)?
        .trim()
        .parse::<u32>()
        .map_err(|_| CryptoError::Malformed(name))
}

/// Agile encryption's parameters (2.3.4.10): the package's `keyData` and
/// the password key encryptor's `encryptedKey`.
#[derive(Debug)]
struct Agile {
    data_salt: Vec<u8>,
    data_hash: Hash,
    data_block: usize,
    data_key_bits: usize,
    spin: u32,
    key_salt: Vec<u8>,
    key_hash: Hash,
    key_bits: usize,
    verifier_input: Vec<u8>,
    verifier_hash: Vec<u8>,
    encrypted_key: Vec<u8>,
}

/// The block keys of 2.3.4.13 / 2.3.4.14.
const BLOCK_VERIFIER_INPUT: [u8; 8] = [0xFE, 0xA7, 0xD2, 0x76, 0x3B, 0x4B, 0x9E, 0x79];
const BLOCK_VERIFIER_HASH: [u8; 8] = [0xD7, 0xAA, 0x0F, 0x6D, 0x30, 0x61, 0x34, 0x4E];
const BLOCK_KEY_VALUE: [u8; 8] = [0x14, 0x6E, 0x0B, 0xE7, 0xAB, 0xAC, 0xD0, 0xD6];

impl Agile {
    fn parse(xml: &[u8]) -> Result<Self, CryptoError> {
        let bad = CryptoError::Malformed;
        if xml.len() > 1 << 20 {
            return Err(bad("EncryptionInfo too large"));
        }
        let text = std::str::from_utf8(xml).map_err(|_| bad("EncryptionInfo XML"))?;
        let dom = roxmltree::Document::parse(text).map_err(|_| bad("EncryptionInfo XML"))?;
        let key_data = dom
            .descendants()
            .find(|e| e.tag_name().name() == "keyData")
            .ok_or(bad("no keyData"))?;
        // The password key encryptor; a certificate one has its own
        // namespace and attributes.
        let enc_key = dom
            .descendants()
            .find(|e| {
                e.tag_name().name() == "encryptedKey"
                    && e.tag_name().namespace()
                        == Some("http://schemas.microsoft.com/office/2006/keyEncryptor/password")
            })
            .ok_or_else(|| CryptoError::Unsupported("no password key encryptor".into()))?;
        for e in [key_data, enc_key] {
            let cipher = attr(e, "cipherAlgorithm")?;
            if cipher != "AES" {
                return Err(CryptoError::Unsupported(format!("cipher {cipher}")));
            }
            let chaining = attr(e, "cipherChaining")?;
            if chaining != "ChainingModeCBC" {
                return Err(CryptoError::Unsupported(format!("chaining {chaining}")));
            }
            if num(e, "blockSize")? != 16 {
                return Err(bad("blockSize"));
            }
            if !matches!(num(e, "keyBits")?, 128 | 192 | 256) {
                return Err(bad("keyBits"));
            }
            let salt = num(e, "saltSize")? as usize;
            if !(1..=MAX_SALT).contains(&salt) || b64(e, "saltValue")?.len() != salt {
                return Err(bad("saltSize"));
            }
        }
        let hash = |e: roxmltree::Node| {
            let name = attr(e, "hashAlgorithm")?;
            let h = Hash::parse(name)
                .ok_or_else(|| CryptoError::Unsupported(format!("hash {name}")))?;
            if num(e, "hashSize")? as usize != h.len() {
                return Err(bad("hashSize"));
            }
            Ok(h)
        };
        let spin = num(enc_key, "spinCount")?;
        if spin > MAX_SPIN_COUNT {
            return Err(bad("spinCount over 10,000,000"));
        }
        Ok(Self {
            data_salt: b64(key_data, "saltValue")?,
            data_hash: hash(key_data)?,
            data_block: 16,
            data_key_bits: num(key_data, "keyBits")? as usize,
            spin,
            key_salt: b64(enc_key, "saltValue")?,
            key_hash: hash(enc_key)?,
            key_bits: num(enc_key, "keyBits")? as usize,
            verifier_input: b64(enc_key, "encryptedVerifierHashInput")?,
            verifier_hash: b64(enc_key, "encryptedVerifierHashValue")?,
            encrypted_key: b64(enc_key, "encryptedKeyValue")?,
        })
    }

    /// The package key when `password` (UTF-16LE) verifies, else `None`
    /// (2.3.4.13 verifier, 2.3.4.14 key).
    fn secret_key(&self, password: &[u8]) -> Result<Option<Vec<u8>>, CryptoError> {
        let h = self.key_hash.spin(&self.key_salt, password, self.spin);
        let key_len = self.key_bits / 8;
        let aes = |block_key: &[u8]| {
            let key = fit(&self.key_hash.of(&[&h, block_key]), key_len, 0x36);
            Aes::new(&key).ok_or(CryptoError::Malformed("keyBits"))
        };
        let iv = fit(&self.key_salt, 16, 0x36);
        let decrypt = |block_key: &[u8], data: &[u8]| -> Result<Vec<u8>, CryptoError> {
            if data.is_empty() || !data.len().is_multiple_of(16) {
                return Err(CryptoError::Malformed("encrypted verifier"));
            }
            let mut out = data.to_vec();
            aes(block_key)?.cbc(&iv, &mut out);
            Ok(out)
        };
        let input = decrypt(&BLOCK_VERIFIER_INPUT, &self.verifier_input)?;
        let input = &input[..input.len().min(self.key_salt.len())];
        let expected = decrypt(&BLOCK_VERIFIER_HASH, &self.verifier_hash)?;
        let n = self.key_hash.len();
        if expected.len() < n || self.key_hash.of(&[input])[..] != expected[..n] {
            return Ok(None);
        }
        let mut key = decrypt(&BLOCK_KEY_VALUE, &self.encrypted_key)?;
        if key.len() < self.data_key_bits / 8 {
            return Err(CryptoError::Malformed("encryptedKeyValue"));
        }
        key.truncate(self.data_key_bits / 8);
        Ok(Some(key))
    }

    /// Decrypt the package (2.3.4.15): 4096-byte segments, each AES-CBC
    /// with the IV `H(keyData salt + segment index)`.
    fn decrypt(&self, key: &[u8], data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let aes = Aes::new(key).ok_or(CryptoError::Malformed("keyBits"))?;
        let mut out = data.to_vec();
        for (i, segment) in out.chunks_mut(4096).enumerate() {
            let iv = self
                .data_hash
                .of(&[&self.data_salt, &(i as u32).to_le_bytes()]);
            aes.cbc(&fit(&iv, self.data_block, 0x36), segment);
        }
        Ok(out)
    }
}

/// Standard encryption's parameters (2.3.4.5/2.3.4.6): AES with a SHA-1
/// key derivation, ECB over the whole package.
struct Standard {
    salt: Vec<u8>,
    key_bits: usize,
    verifier: [u8; 16],
    verifier_hash: [u8; 32],
}

impl Standard {
    fn parse(info: &[u8]) -> Result<Self, CryptoError> {
        let header = CryptoApiHeader::parse(info)?;
        // fCryptoAPI | fAES; fExternal is extensible encryption.
        if header.flags & 0x10 != 0 {
            return Err(CryptoError::Unsupported("extensible encryption".into()));
        }
        if header.flags & 0x24 != 0x24 || !matches!(header.alg_id, 0x660E..=0x6610) {
            return Err(CryptoError::Unsupported(format!(
                "algorithm {:#06x}",
                header.alg_id
            )));
        }
        if !matches!(header.alg_id_hash, 0 | 0x8004) {
            return Err(CryptoError::Unsupported("hash other than SHA-1".into()));
        }
        let key_bits = header.key_bits as usize;
        if !matches!(key_bits, 128 | 192 | 256) {
            return Err(CryptoError::Malformed("KeySize"));
        }
        let v = header.verifier;
        Ok(Self {
            salt: header.salt,
            key_bits,
            verifier: info
                .get(v..v + 16)
                .and_then(|b| b.try_into().ok())
                .ok_or(CryptoError::Malformed("EncryptionVerifier"))?,
            verifier_hash: info
                .get(v + 20..v + 52)
                .and_then(|b| b.try_into().ok())
                .ok_or(CryptoError::Malformed("EncryptionVerifier"))?,
        })
    }

    /// The AES key when `password` verifies (2.3.4.7 derivation, 2.3.4.8
    /// verifier).
    fn key(&self, password: &[u8]) -> Option<Vec<u8>> {
        let h = Hash::Sha1.spin(&self.salt, password, 50_000);
        let h = Hash::Sha1.of(&[&h, &0u32.to_le_bytes()]);
        let derive = |fill: u8| {
            let mut buf = [fill; 64];
            for (b, x) in buf.iter_mut().zip(&h) {
                *b ^= x;
            }
            Hash::Sha1.of(&[&buf])
        };
        let mut key = derive(0x36);
        key.extend(derive(0x5C));
        key.truncate(self.key_bits / 8);
        let aes = Aes::new(&key)?;
        let mut verifier = self.verifier;
        aes.ecb(&mut verifier);
        let mut hash = self.verifier_hash;
        aes.ecb(&mut hash);
        (Hash::Sha1.of(&[&verifier])[..] == hash[..20]).then_some(key)
    }
}

/// The CryptoAPI `EncryptionInfo` layout shared by Standard (OOXML) and
/// RC4 CryptoAPI (binary) encryption (2.3.2, 2.3.3): version, flags, a
/// header of `HeaderSize` bytes, then the verifier.
struct CryptoApiHeader {
    flags: u32,
    alg_id: u32,
    alg_id_hash: u32,
    key_bits: u32,
    salt: Vec<u8>,
    /// Offset of `EncryptedVerifier` in the info.
    verifier: usize,
}

impl CryptoApiHeader {
    fn parse(info: &[u8]) -> Result<Self, CryptoError> {
        let bad = CryptoError::Malformed;
        let header_size = u32_at(info, 8).ok_or(bad("EncryptionHeader"))? as usize;
        if !(32..=4096).contains(&header_size) {
            return Err(bad("EncryptionHeader size"));
        }
        let h = 12;
        let v = h + header_size;
        let salt_size = u32_at(info, v).ok_or(bad("EncryptionVerifier"))? as usize;
        if salt_size != 16 {
            return Err(bad("SaltSize"));
        }
        Ok(Self {
            flags: u32_at(info, h).ok_or(bad("EncryptionHeader"))?,
            alg_id: u32_at(info, h + 8).ok_or(bad("EncryptionHeader"))?,
            alg_id_hash: u32_at(info, h + 12).ok_or(bad("EncryptionHeader"))?,
            key_bits: u32_at(info, h + 16).ok_or(bad("EncryptionHeader"))?,
            salt: info
                .get(v + 4..v + 20)
                .ok_or(bad("EncryptionVerifier"))?
                .to_vec(),
            verifier: v + 20,
        })
    }
}

// --- binary formats (RC4) -------------------------------------------------

/// The RC4 key schedule of a binary document, by block number.
enum Rc4Key {
    /// RC4 CryptoAPI (2.3.5.2): `H0 = SHA-1(salt + password)`, then
    /// `SHA-1(H0 + block)` cut to the key size — a 40-bit key is padded
    /// with zeros to 128 bits.
    CryptoApi { h0: Vec<u8>, key_bytes: usize },
    /// Office 97/2000 RC4 (2.3.6.2): MD5 throughout, 128-bit block keys
    /// from the first five bytes of the salted, sixteen-fold password hash.
    Rc4 { h1: [u8; 5] },
}

impl Rc4Key {
    fn block(&self, n: u32) -> Rc4 {
        match self {
            Rc4Key::CryptoApi { h0, key_bytes } => {
                let h = Hash::Sha1.of(&[h0, &n.to_le_bytes()]);
                if *key_bytes == 5 {
                    let mut key = [0u8; 16];
                    key[..5].copy_from_slice(&h[..5]);
                    Rc4::new(&key)
                } else {
                    Rc4::new(&h[..*key_bytes])
                }
            }
            Rc4Key::Rc4 { h1 } => Rc4::new(&Hash::Md5.of(&[h1, &n.to_le_bytes()])),
        }
    }

    /// Decrypt `data` in `block`-byte blocks, block `i` with key `i` — the
    /// `.doc` (512) and `.xls` (1024) streams' scheme. A stream position
    /// keeps its block number whatever is decrypted around it.
    fn blocks(&self, data: &mut [u8], block: usize) {
        for (i, chunk) in data.chunks_mut(block).enumerate() {
            self.block(i as u32).apply(chunk);
        }
    }

    /// The key for the first of `tries` that verifies against a binary
    /// `EncryptionInfo` (RC4 1.1 or RC4 CryptoAPI x.2).
    fn open(info: &[u8], tries: &[&str]) -> Result<Self, CryptoError> {
        let bad = CryptoError::Malformed;
        match (u16_at(info, 0), u16_at(info, 2)) {
            (Some(1), Some(1)) => {
                let salt = info.get(4..20).ok_or(bad("EncryptionHeader"))?;
                let verifier = info.get(20..36).ok_or(bad("EncryptionHeader"))?;
                let hash = info.get(36..52).ok_or(bad("EncryptionHeader"))?;
                first_key(tries, |pw| {
                    let h0 = Hash::Md5.of(&[pw]);
                    let mut buf = Vec::with_capacity(16 * 21);
                    for _ in 0..16 {
                        buf.extend_from_slice(&h0[..5]);
                        buf.extend_from_slice(salt);
                    }
                    let h1: [u8; 5] = Hash::Md5.of(&[&buf])[..5].try_into().expect("5 bytes");
                    let key = Rc4Key::Rc4 { h1 };
                    Ok(key.verifies(verifier, hash, Hash::Md5).then_some(key))
                })
            }
            (Some(2..=4), Some(2)) => {
                let header = CryptoApiHeader::parse(info)?;
                if header.flags & 0x20 != 0 || !matches!(header.alg_id, 0 | 0x6801) {
                    return Err(CryptoError::Unsupported(format!(
                        "algorithm {:#06x}",
                        header.alg_id
                    )));
                }
                let key_bits = match header.key_bits {
                    0 => 40,
                    bits if (40..=128).contains(&bits) && bits.is_multiple_of(8) => bits,
                    _ => return Err(bad("KeySize")),
                };
                let v = header.verifier;
                let verifier = info.get(v..v + 16).ok_or(bad("EncryptionVerifier"))?;
                let hash_size = u32_at(info, v + 16).ok_or(bad("EncryptionVerifier"))?;
                if hash_size != 20 {
                    return Err(bad("VerifierHashSize"));
                }
                let hash = info.get(v + 20..v + 40).ok_or(bad("EncryptionVerifier"))?;
                first_key(tries, |pw| {
                    let key = Rc4Key::CryptoApi {
                        h0: Hash::Sha1.of(&[&header.salt, pw]),
                        key_bytes: key_bits as usize / 8,
                    };
                    Ok(key.verifies(verifier, hash, Hash::Sha1).then_some(key))
                })
            }
            (Some(major), Some(minor)) => Err(CryptoError::Unsupported(format!(
                "EncryptionInfo version {major}.{minor}"
            ))),
            _ => Err(bad("EncryptionInfo")),
        }
    }

    /// Block 0 decrypts the verifier and then, continuing the same stream,
    /// its hash (2.3.5.6 / 2.3.6.4).
    fn verifies(&self, verifier: &[u8], hash: &[u8], h: Hash) -> bool {
        let mut buf = [verifier, hash].concat();
        self.block(0).apply(&mut buf);
        let (v, rest) = buf.split_at(16);
        h.of(&[v])[..] == rest[..h.len()]
    }
}

/// The original file with `streams` (root stream name → plaintext of the
/// same length) written over their own sectors.
fn rewrite(
    bytes: &[u8],
    cfb: &CompoundFile,
    streams: &[(&str, Vec<u8>)],
) -> Result<Vec<u8>, CryptoError> {
    let mut out = bytes.to_vec();
    for (name, plain) in streams {
        let spans: Vec<Range<usize>> = cfb
            .root_stream(name)
            .and_then(|i| cfb.stream_spans(i))
            .ok_or(CryptoError::Malformed("stream unreadable"))?;
        let mut pos = 0;
        for span in spans {
            let n = span.len();
            out[span].copy_from_slice(&plain[pos..pos + n]);
            pos += n;
        }
        debug_assert_eq!(pos, plain.len());
    }
    Ok(out)
}

fn root_bytes(cfb: &CompoundFile, name: &str) -> Option<Vec<u8>> {
    cfb.stream_by_index(cfb.root_stream(name)?)
}

/// `len` bytes of a root stream from offset `at` (fewer at its end), read
/// through its sector spans — enough to see an encryption marker without
/// reading a plain file's whole stream on every conversion.
fn root_slice(
    bytes: &[u8],
    cfb: &CompoundFile,
    name: &str,
    at: usize,
    len: usize,
) -> Option<Vec<u8>> {
    let end = at.saturating_add(len);
    let mut out = Vec::with_capacity(len.min(1 << 16));
    let mut pos = 0;
    for span in cfb.stream_spans(cfb.root_stream(name)?)? {
        let n = span.len();
        if pos + n > at {
            let from = at.saturating_sub(pos);
            let to = n.min(end - pos);
            out.extend_from_slice(&bytes[span.start + from..span.start + to]);
        }
        pos += n;
        if pos >= end {
            break;
        }
    }
    Some(out)
}

fn root_head(bytes: &[u8], cfb: &CompoundFile, name: &str, n: usize) -> Option<Vec<u8>> {
    root_slice(bytes, cfb, name, 0, n)
}

/// `.doc` ([MS-DOC] 2.2.6.2): with `fEncrypted` set, the table stream opens
/// with the `EncryptionInfo` (`lKey` bytes); `WordDocument` past the 68-byte
/// FibBase, the whole table stream and the `Data` stream are RC4-encrypted
/// in 512-byte blocks numbered from each stream's start.
fn unlock_doc(
    bytes: &[u8],
    cfb: &CompoundFile,
    tries: &[&str],
) -> Result<Option<Vec<u8>>, CryptoError> {
    let Some(head) = root_head(bytes, cfb, "WordDocument", 0x44) else {
        return Ok(None);
    };
    let flags = u16_at(&head, 0x0A).unwrap_or(0);
    if head.len() < 0x44 || flags & 0x0100 == 0 {
        return Ok(None);
    }
    let mut word = root_bytes(cfb, "WordDocument").ok_or(CryptoError::Malformed("WordDocument"))?;
    // Word 6.0/95 (nFib 101–105) encrypts with its own XOR scheme and has
    // no table stream to hold an EncryptionInfo.
    if flags & 0x8000 != 0 || u16_at(&word, 0) != Some(0xA5EC) {
        return Err(CryptoError::Unsupported("XOR obfuscation".into()));
    }
    let table_name = if flags & 0x0200 != 0 {
        "1Table"
    } else {
        "0Table"
    };
    let mut table = root_bytes(cfb, table_name).ok_or(CryptoError::Malformed("no table stream"))?;
    let key_len = u32_at(&word, 0x0E).unwrap_or(0) as usize;
    let info = table.get(..key_len).ok_or(CryptoError::Malformed("lKey"))?;
    let key = Rc4Key::open(info, tries)?;
    let fib_base: [u8; 0x44] = word[..0x44].try_into().expect("FibBase");
    key.blocks(&mut word, 512);
    word[..0x44].copy_from_slice(&fib_base);
    // fEncrypted and fObfuscation off, lKey 0 — the FIB of the plain file.
    word[0x0A..0x0C].copy_from_slice(&(flags & !0x8100).to_le_bytes());
    word[0x0E..0x12].fill(0);
    key.blocks(&mut table, 512);
    let mut streams = vec![("WordDocument", word), (table_name, table)];
    if let Some(mut data) = root_bytes(cfb, "Data") {
        key.blocks(&mut data, 512);
        streams.push(("Data", data));
    }
    rewrite(bytes, cfb, &streams).map(Some)
}

/// `.xls` ([MS-XLS] 2.2.10): every record after `FILEPASS` is RC4-encrypted
/// in 1024-byte blocks numbered by stream offset — but record headers stay
/// in the clear, as do `BOF`, `FILEPASS`, `UsrExcl`, `FileLock`,
/// `InterfaceHdr`, `RRDInfo`, `RRDHead` and `BoundSheet8`'s `lbPlyPos`.
/// `FILEPASS` becomes a zero-filled record of type 0, as msoffcrypto-tool
/// writes it, so calamine reads the workbook as unencrypted.
fn unlock_xls(
    bytes: &[u8],
    cfb: &CompoundFile,
    tries: &[&str],
) -> Result<Option<Vec<u8>>, CryptoError> {
    let name = if cfb.root_stream("Workbook").is_some() {
        "Workbook"
    } else {
        "Book"
    };
    // FILEPASS sits in the globals substream right after its BOF; look
    // through the first few records only — of the stream's head, so a plain
    // workbook's stream is not read twice.
    let Some(book) = root_head(bytes, cfb, name, 8192) else {
        return Ok(None);
    };
    let mut pos = 0;
    let mut filepass = None;
    for _ in 0..16 {
        let (Some(kind), Some(len)) = (u16_at(&book, pos), u16_at(&book, pos + 2)) else {
            break;
        };
        if kind == 0x002F {
            filepass = Some((pos, len as usize));
            break;
        }
        pos += 4 + len as usize;
    }
    let Some((fp, fp_len)) = filepass else {
        return Ok(None);
    };
    // BOF.vers: only BIFF8 (Excel 97+) carries an EncryptionInfo; Excel
    // 5.0/95's FILEPASS is an XOR key and verifier.
    if u16_at(&book, 4) != Some(0x0600) {
        return Err(CryptoError::Unsupported(
            "Excel 5.0/95 XOR obfuscation".into(),
        ));
    }
    let body = book
        .get(fp + 4..fp + 4 + fp_len)
        .ok_or(CryptoError::Malformed("FILEPASS"))?;
    match u16_at(body, 0) {
        Some(1) => {}
        Some(0) => return Err(CryptoError::Unsupported("XOR obfuscation".into())),
        _ => return Err(CryptoError::Malformed("FILEPASS")),
    }
    let key = Rc4Key::open(&body[2..], tries)?;
    let book = root_bytes(cfb, name).ok_or(CryptoError::Malformed("Workbook"))?;
    let mut plain = book.clone();
    key.blocks(&mut plain, 1024);
    let mut out = book.clone();
    let mut pos = 0;
    while let (Some(kind), Some(len)) = (u16_at(&book, pos), u16_at(&book, pos + 2)) {
        let body = pos + 4..(pos + 4 + len as usize).min(book.len());
        match kind {
            // Type 0, same length, zero body: every later offset holds.
            0x002F => {
                out[pos..pos + 2].fill(0);
                out[body.clone()].fill(0);
            }
            0x0809 | 0x0194 | 0x0195 | 0x00E1 | 0x0196 | 0x0138 => {}
            0x0085 => {
                let from = (body.start + 4).min(body.end);
                out[from..body.end].copy_from_slice(&plain[from..body.end]);
            }
            _ => out[body.clone()].copy_from_slice(&plain[body.clone()]),
        }
        pos = body.end;
        if body.end - body.start < len as usize {
            break;
        }
    }
    rewrite(bytes, cfb, &[(name, out)]).map(Some)
}

/// `.ppt` ([MS-PPT] 2.3.7): each persist object of the `PowerPoint
/// Document` stream — record header and body — is one RC4 stream keyed by
/// its persist id; the `UserEditAtom`s, `PersistDirectoryAtom`s and the
/// `CryptSession10Container` holding the `EncryptionInfo` are in the clear.
/// Every edit's objects are decrypted (the backend walks the stream
/// linearly, superseded objects included), and the current edit's
/// `encryptSessionPersistIdRef` is zeroed so the file reads as plain.
fn unlock_ppt(
    bytes: &[u8],
    cfb: &CompoundFile,
    tries: &[&str],
) -> Result<Option<Vec<u8>>, CryptoError> {
    use crate::backend::ppt::UserEdits;
    let Some(user) = root_bytes(cfb, "Current User") else {
        return Ok(None);
    };
    // The current edit's UserEditAtom alone says whether the file is
    // encrypted (0x20 bytes long, a session reference): read just it before
    // the whole stream.
    let Some(at) = u32_at(&user, 16) else {
        return Ok(None);
    };
    let edit = root_slice(bytes, cfb, "PowerPoint Document", at as usize, 40).unwrap_or_default();
    if u16_at(&edit, 2) != Some(0x0FF5)
        || u32_at(&edit, 4) != Some(0x20)
        || u32_at(&edit, 36) == Some(0)
    {
        return Ok(None);
    }
    let Some(mut doc) = root_bytes(cfb, "PowerPoint Document") else {
        return Ok(None);
    };
    let Some(edits) = UserEdits::read(&user, &doc) else {
        return Ok(None);
    };
    if !edits.encrypted() {
        return Ok(None);
    }
    let session = edits
        .crypt_session(&doc)
        .ok_or(CryptoError::Malformed("CryptSession10Container"))?;
    let key = Rc4Key::open(session, tries)?;
    let session_offset = edits.session_offset();
    for &(id, offset) in &edits.objects {
        let off = offset as usize;
        if Some(off) == session_offset || off + 8 > doc.len() {
            continue;
        }
        let mut rc4 = key.block(id);
        let mut header: [u8; 8] = doc[off..off + 8].try_into().expect("8 bytes");
        rc4.apply(&mut header);
        let len = u32::from_le_bytes(header[4..8].try_into().expect("4 bytes")) as usize;
        let end = (off + 8).saturating_add(len);
        if end > doc.len() {
            // A wrong key cannot get here (the verifier passed); a damaged
            // object is left encrypted for the backend to skip.
            continue;
        }
        doc[off..off + 8].copy_from_slice(&header);
        rc4.apply(&mut doc[off + 8..end]);
    }
    if let Some(at) = edits.encrypt_ref_offset() {
        doc[at..at + 4].fill(0);
    }
    rewrite(bytes, cfb, &[("PowerPoint Document", doc)]).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha256_hex(data: &[u8]) -> String {
        Hash::Sha256
            .of(&[data])
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// The decrypted bytes are msoffcrypto-tool's (5.4): the same ZIP for
    /// an OOXML package (Agile, Office 16), the same `WordDocument`, table
    /// and `Data` streams for a `.doc` and the same `Workbook` stream for an
    /// `.xls` (RC4 CryptoAPI) — SHA-256 of msoffcrypto's output, password
    /// `1234` (`C`/`min_writepw`: PowerPoint's default password), on #624's
    /// Office-written fixtures. The `.ppt` digests are this reader's own:
    /// its `PowerPoint Document` stream equals msoffcrypto's in every
    /// decrypted persist object and differs only where the two clear the
    /// encryption — msoffcrypto zero-fills the `CryptSession10Container`
    /// and shortens the `UserEditAtom`/`PersistDirectoryAtom` lengths, this
    /// reader zeroes the session reference and keeps both.
    #[test]
    fn decryption_matches_msoffcrypto() {
        const CASES: &[(&str, &str, &str)] = &[
            (
                "min_encrypted.docx",
                "",
                "1452cfea6a74fc4542084bd77efa9e99fca51e743139d178860ea375901976c7",
            ),
            (
                "min_encrypted.xlsx",
                "",
                "31a175dcba94ed8ac40eddc67cca524d243d8856b575482393a07eeaea8bb3b7",
            ),
            (
                "min_encrypted.pptx",
                "",
                "4ceb0c81dd8a86af89526e9bd5f6b4f18314b5eb50f489516a9143944016df21",
            ),
            (
                "min_encrypted.doc",
                "WordDocument",
                "35d18dc4d86189712430dabaca3747b6008f5fb6fb11aaa269b0c13814db9e8e",
            ),
            (
                "min_encrypted.doc",
                "1Table",
                "c9251018603e6104a661aed033ab6a87b2f39498b1adb3c5d995fb508f96e4b6",
            ),
            (
                "min_encrypted.doc",
                "Data",
                "00dda59c88a1c268457d1bd4b5a56115e8d793a39b622735e7c33b7b75bf0c48",
            ),
            (
                "min_encrypted.xls",
                "Workbook",
                "f22f957d1e27880de78ded80ad4c36a1fda5801f8e2a598f7176d6e32c4347fc",
            ),
            (
                "min_encrypted.ppt",
                "PowerPoint Document",
                "f638ae098188614ba4b9124bc340112802332ec0be4d975bbf5b0d23a0681efd",
            ),
            (
                "B_openpw.ppt",
                "PowerPoint Document",
                "f07c0bd71545c054e3ba8f2470cc179d20f78368719e8585af1ffa612f5a4a64",
            ),
            (
                "C_writepw.ppt",
                "PowerPoint Document",
                "7ed75e53bb3691e6dece35d493c1e190355c416632d98c21f23621f7f87ad2f2",
            ),
            (
                "D_both.ppt",
                "PowerPoint Document",
                "3d41a6ae61ba617086778cd3e279a81b6a90b9d1e0105687c3c76e71ea6d7d79",
            ),
            (
                "H_A_addpw_save.ppt",
                "PowerPoint Document",
                "fb830bc3ba3595dd503f7284cbf50fe785b15f75c6aabb93ea592e222a2a9038",
            ),
            (
                "min_writepw.ppt",
                "PowerPoint Document",
                "183a072b3c73c0115dfd86f90f027f200c13fcecf8b19bffe006f7b40a2a3fa0",
            ),
        ];
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/encrypted/");
        for &(name, stream, want) in CASES {
            let ext = name.rsplit('.').next().unwrap();
            let format = InputFormat::from_extension(ext).unwrap();
            let bytes = std::fs::read(format!("{dir}{name}")).unwrap();
            let source = SourceDocument::from_bytes(name, format, bytes);
            let plain = unlock(&source, Some("1234")).unwrap().expect("encrypted");
            let got = if stream.is_empty() {
                sha256_hex(&plain)
            } else {
                let data = CompoundFile::open(&plain).unwrap().stream(stream).unwrap();
                sha256_hex(&data)
            };
            assert_eq!(got, want, "{name} {stream}");
        }
    }

    /// A slice read through the sector spans is the slice of the stream —
    /// across sector boundaries, at the end, past the end.
    #[test]
    fn root_slice_matches_the_stream() {
        let data = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/data/ppt/sources/powerpoint_sample.ppt"
        ))
        .unwrap();
        let cfb = CompoundFile::open(&data).unwrap();
        let doc = cfb.stream("PowerPoint Document").unwrap();
        let user = cfb.stream("Current User").unwrap();
        for (at, len) in [
            (0, 40),
            (500, 30),
            (511, 2),
            (1000, 5000),
            (doc.len() - 3, 40),
        ] {
            let want = &doc[at..(at + len).min(doc.len())];
            let got = root_slice(&data, &cfb, "PowerPoint Document", at, len).unwrap();
            assert_eq!(got, want, "{at}+{len}");
        }
        let past = root_slice(&data, &cfb, "PowerPoint Document", doc.len() + 10, 4).unwrap();
        assert!(past.is_empty());
        assert_eq!(
            root_head(&data, &cfb, "Current User", 1 << 20).unwrap(),
            user
        );
    }

    #[test]
    fn rc4_matches_the_reference_vectors() {
        // The classic ARC4 test vectors (Wikipedia / RFC 6229 style).
        for (key, plain, cipher) in [
            (&b"Key"[..], &b"Plaintext"[..], "bbf316e8d940af0ad3"),
            (b"Wiki", b"pedia", "1021bf0420"),
            (b"Secret", b"Attack at dawn", "45a01f645fc35b383552544b9bf5"),
        ] {
            let mut data = plain.to_vec();
            Rc4::new(key).apply(&mut data);
            let hex: String = data.iter().map(|b| format!("{b:02x}")).collect();
            assert_eq!(hex, cipher);
        }
    }

    fn agile_xml(
        spin: &str,
        key_bits: &str,
        salt_size: &str,
        hash: &str,
        hash_size: &str,
    ) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<encryption xmlns="http://schemas.microsoft.com/office/2006/encryption" xmlns:p="http://schemas.microsoft.com/office/2006/keyEncryptor/password">
<keyData saltSize="16" blockSize="16" keyBits="256" hashSize="64" cipherAlgorithm="AES" cipherChaining="ChainingModeCBC" hashAlgorithm="SHA512" saltValue="AAAAAAAAAAAAAAAAAAAAAA=="/>
<keyEncryptors><keyEncryptor uri="http://schemas.microsoft.com/office/2006/keyEncryptor/password">
<p:encryptedKey spinCount="{spin}" saltSize="{salt_size}" blockSize="16" keyBits="{key_bits}" hashSize="{hash_size}" cipherAlgorithm="AES" cipherChaining="ChainingModeCBC" hashAlgorithm="{hash}" saltValue="AAAAAAAAAAAAAAAAAAAAAA==" encryptedVerifierHashInput="AAAAAAAAAAAAAAAAAAAAAA==" encryptedVerifierHashValue="AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==" encryptedKeyValue="AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="/>
</keyEncryptor></keyEncryptors></encryption>"#
        )
    }

    /// Hostile Agile headers are refused while parsing — before any of the
    /// spin count's hashing or a size-driven allocation.
    #[test]
    fn hostile_agile_headers_are_refused_before_hashing() {
        let ok = Agile::parse(agile_xml("100000", "256", "16", "SHA512", "64").as_bytes()).unwrap();
        assert_eq!(
            (ok.spin, ok.key_bits, ok.key_hash),
            (100_000, 256, Hash::Sha512)
        );
        for (xml, want) in [
            (
                agile_xml("4294967295", "256", "16", "SHA512", "64"),
                "spinCount",
            ),
            (
                agile_xml("10000001", "256", "16", "SHA512", "64"),
                "spinCount",
            ),
            (agile_xml("1", "1024", "16", "SHA512", "64"), "keyBits"),
            (agile_xml("1", "256", "4000000", "SHA512", "64"), "saltSize"),
            (agile_xml("1", "256", "16", "SHA512", "9999"), "hashSize"),
        ] {
            let started = std::time::Instant::now();
            let err = Agile::parse(xml.as_bytes()).unwrap_err();
            assert!(format!("{err:?}").contains(want), "{want}: {err:?}");
            assert!(
                started.elapsed().as_millis() < 100,
                "{want}: parsing hashed"
            );
        }
        let err =
            Agile::parse(agile_xml("1", "256", "16", "WHIRLPOOL", "64").as_bytes()).unwrap_err();
        assert_eq!(err, CryptoError::Unsupported("hash WHIRLPOOL".into()));
        let des = agile_xml("1", "256", "16", "SHA512", "64").replacen(
            "cipherAlgorithm=\"AES\"",
            "cipherAlgorithm=\"3DES\"",
            1,
        );
        assert_eq!(
            Agile::parse(des.as_bytes()).unwrap_err(),
            CryptoError::Unsupported("cipher 3DES".into())
        );
    }
}
