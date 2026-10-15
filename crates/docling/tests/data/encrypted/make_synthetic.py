"""Synthesize the encrypted fixtures Office no longer writes (#625).

Office 16 writes Agile OOXML and RC4 CryptoAPI (128-bit) binaries — the
`min_encrypted.*` fixtures. The older schemes are built here from those
files' plaintext and checked by msoffcrypto-tool before they are written:

- `std_encrypted.docx`: ECMA-376 Standard encryption (Office 2007: AES-128,
  SHA-1 x 50 000, AES-ECB), msoffcrypto's own container writer.
- `rc4_encrypted.doc`: Office 97/2000 RC4 (MD5, EncryptionInfo 1.1).
- `rc4_40bit_encrypted.doc`: RC4 CryptoAPI with a 40-bit key.
- `velvet_encrypted.xlsx`: Agile, encrypted by msoffcrypto with Excel's
  default password `VelvetSweatshop` (what Excel writes for a workbook
  protected only against structural edits).

Password `1234`. The binaries come out byte-identical on every run; the
OOXML containers differ only in msoffcrypto's directory timestamps (and the
Agile salts). Usage (msoffcrypto-tool 5.4, cryptography, olefile):

    python make_synthetic.py <min_encrypted.docx> <min_encrypted.doc> \
        <min_encrypted.xlsx> <out dir>
"""

import hashlib
import io
import os
import shutil
import struct
import sys

import msoffcrypto
import olefile
from cryptography.hazmat.decrepit.ciphers.algorithms import ARC4
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
from msoffcrypto.method.container.ecma376_encrypted import ECMA376Encrypted

PASSWORD = "1234"
SALT = bytes(range(16))  # fixed, so the fixtures are reproducible
VERIFIER = bytes(range(100, 116))


def plain(path):
    f = msoffcrypto.OfficeFile(open(path, "rb"))
    if f.format == "ooxml":
        f.load_key(password=PASSWORD, verify_password=True)
    else:
        f.load_key(password=PASSWORD)
    out = io.BytesIO()
    f.decrypt(out)
    return out.getvalue()


def aes_ecb(key, data, encrypt=True):
    c = Cipher(algorithms.AES(key), modes.ECB())
    op = c.encryptor() if encrypt else c.decryptor()
    return op.update(data) + op.finalize()


def standard_docx(zip_bytes):
    pw = PASSWORD.encode("utf-16le")
    h = hashlib.sha1(SALT + pw).digest()
    for i in range(50000):
        h = hashlib.sha1(struct.pack("<I", i) + h).digest()
    h = hashlib.sha1(h + struct.pack("<I", 0)).digest()
    x1 = hashlib.sha1(bytes(a ^ b for a, b in zip(h.ljust(64, b"\0"), b"\x36" * 64))).digest()
    key = x1[:16]
    csp = "Microsoft Enhanced RSA and AES Cryptographic Provider\0".encode("utf-16le")
    header = struct.pack("<IIIIIIII", 0x24, 0, 0x660E, 0x8004, 128, 0x18, 0, 0) + csp
    verifier_hash = hashlib.sha1(VERIFIER).digest().ljust(32, b"\0")
    info = (
        struct.pack("<HHI", 3, 2, 0x24)
        + struct.pack("<I", len(header))
        + header
        + struct.pack("<I", 16)
        + SALT
        + aes_ecb(key, VERIFIER)
        + struct.pack("<I", 20)
        + aes_ecb(key, verifier_hash)
    )
    padded = zip_bytes + b"\0" * (-len(zip_bytes) % 16)
    package = struct.pack("<Q", len(zip_bytes)) + aes_ecb(key, padded)
    out = io.BytesIO()
    ECMA376Encrypted(package, info).write_to(out)
    return out.getvalue()


def rc4(key, data):
    return Cipher(ARC4(key), mode=None).encryptor().update(data)


def blocks(keyfn, data, size=512):
    return b"".join(rc4(keyfn(i), data[o : o + size]) for i, o in enumerate(range(0, len(data), size)))


def rc4_md5_key(block):
    h0 = hashlib.md5(PASSWORD.encode("utf-16le")).digest()
    h1 = hashlib.md5((h0[:5] + SALT) * 16).digest()
    return hashlib.md5(h1[:5] + struct.pack("<I", block)).digest()


def cryptoapi40_key(block):
    h0 = hashlib.sha1(SALT + PASSWORD.encode("utf-16le")).digest()
    return hashlib.sha1(h0 + struct.pack("<I", block)).digest()[:5] + b"\0" * 11


def encrypted_doc(plain_doc, scheme):
    ole = olefile.OleFileIO(io.BytesIO(plain_doc))
    word = bytearray(ole.openstream("WordDocument").read())
    table_name = "1Table" if struct.unpack_from("<H", word, 0x0A)[0] & 0x0200 else "0Table"
    table = ole.openstream(table_name).read()
    data = ole.openstream("Data").read() if ole.exists("Data") else None
    if scheme == "rc4":
        keyfn = rc4_md5_key
        v = rc4(keyfn(0), VERIFIER + hashlib.md5(VERIFIER).digest())
        info = struct.pack("<HH", 1, 1) + SALT + v
    else:
        keyfn = cryptoapi40_key
        csp = "Microsoft Base Cryptographic Provider v1.0\0".encode("utf-16le")
        header = struct.pack("<IIIIIIII", 0x04, 0, 0x6801, 0x8004, 0, 0x01, 0, 0) + csp
        v = rc4(keyfn(0), VERIFIER + hashlib.sha1(VERIFIER).digest())
        info = (
            struct.pack("<HHI", 2, 2, 0x04)
            + struct.pack("<I", len(header))
            + header
            + struct.pack("<I", 16)
            + SALT
            + v[:16]
            + struct.pack("<I", 20)
            + v[16:]
        )
    # FibBase: fEncrypted on, lKey = the EncryptionInfo's length, which the
    # table stream opens with (its first bytes are unused plaintext).
    flags = struct.unpack_from("<H", word, 0x0A)[0] | 0x0100
    struct.pack_into("<H", word, 0x0A, flags)
    struct.pack_into("<I", word, 0x0E, len(info))
    enc_word = bytes(word[:0x44]) + blocks(keyfn, bytes(word))[0x44:]
    enc_table = info + blocks(keyfn, table)[len(info) :]
    buf = io.BytesIO(plain_doc)
    out = olefile.OleFileIO(buf, write_mode=True)
    out.write_stream("WordDocument", enc_word)
    out.write_stream(table_name, enc_table)
    if data is not None:
        out.write_stream("Data", blocks(keyfn, data))
    out.close()
    return buf.getvalue()


def check(data, ext, want):
    path = os.path.join(sys.argv[4], "check." + ext)
    open(path, "wb").write(data)
    got = plain(path)
    os.remove(path)
    if ext == "docx":
        assert got == want, "msoffcrypto does not decrypt it back"
    else:
        a, b = olefile.OleFileIO(io.BytesIO(got)), olefile.OleFileIO(io.BytesIO(want))
        assert a.openstream("WordDocument").read()[0x44:] == b.openstream("WordDocument").read()[0x44:]


def main():
    docx, doc, xlsx, out = sys.argv[1:5]
    zip_bytes = plain(docx)
    std = standard_docx(zip_bytes)
    check(std, "docx", zip_bytes)
    open(os.path.join(out, "std_encrypted.docx"), "wb").write(std)
    plain_doc = plain(doc)
    for scheme, name in [("rc4", "rc4_encrypted.doc"), ("cryptoapi40", "rc4_40bit_encrypted.doc")]:
        data = encrypted_doc(plain_doc, scheme)
        check(data, "doc", plain_doc)
        open(os.path.join(out, name), "wb").write(data)
    from msoffcrypto.format.ooxml import OOXMLFile

    velvet = io.BytesIO()
    OOXMLFile(io.BytesIO(plain(xlsx))).encrypt("VelvetSweatshop", velvet)
    f = msoffcrypto.OfficeFile(io.BytesIO(velvet.getvalue()))
    f.load_key(password="VelvetSweatshop", verify_password=True)
    open(os.path.join(out, "velvet_encrypted.xlsx"), "wb").write(velvet.getvalue())
    print("ok")


if __name__ == "__main__":
    main()
