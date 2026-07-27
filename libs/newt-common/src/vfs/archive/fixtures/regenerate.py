#!/usr/bin/env python3
"""Regenerate archive test fixtures used by TarArchiveVfs / ZipArchiveVfs tests.

Produces deterministic `simple.tar`, `simple.tar.gz`, `simple.tar.zst`,
`encrypted.zip` and `varied.zip`
so the bytes committed to the repo are reproducible. Run from this
directory:

    uv run --no-project python regenerate.py

Tar layout:

    hello.txt            -> b"hello world\\n"
    dir/nested.txt       -> b"nested content\\n"
    dir/big.bin          -> 200_000 bytes of (i % 251) — exercises multi-chunk
                            streaming (>= one VFS_READ_CHUNK_SIZE = 64 KiB).
    links/hard.txt       -> hardlink to hello.txt
    links/soft.txt       -> symlink to ../hello.txt

Zip layout (`encrypted.zip`, password "secret"):

    plain.txt            -> b"unencrypted\\n"   (not encrypted)
    secret.txt           -> b"top secret\\n"   (ZipCrypto-encrypted)

Built via the system `zip` CLI (uses ZipCrypto, which the rust `zip`
crate reads without the `aes-crypto` feature).
"""

import gzip
import io
import os
import shutil
import subprocess
import tarfile
import tempfile
from pathlib import Path

OUT_DIR = Path(__file__).parent
MTIME = 1_700_000_000

HELLO = b"hello world\n"
NESTED = b"nested content\n"
BIG = bytes(i % 251 for i in range(200_000))


def _info(name: str, *, mode: int = 0o644, type_=tarfile.REGTYPE, linkname: str = ""):
    info = tarfile.TarInfo(name=name)
    info.type = type_
    info.mode = mode
    info.mtime = MTIME
    info.uid = 1000
    info.gid = 1000
    info.uname = "user"
    info.gname = "group"
    info.linkname = linkname
    return info


def build() -> bytes:
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=tarfile.USTAR_FORMAT) as tar:
        f = _info("hello.txt"); f.size = len(HELLO)
        tar.addfile(f, io.BytesIO(HELLO))

        tar.addfile(_info("dir", mode=0o755, type_=tarfile.DIRTYPE))

        f = _info("dir/nested.txt"); f.size = len(NESTED)
        tar.addfile(f, io.BytesIO(NESTED))

        f = _info("dir/big.bin"); f.size = len(BIG)
        tar.addfile(f, io.BytesIO(BIG))

        tar.addfile(_info("links", mode=0o755, type_=tarfile.DIRTYPE))

        tar.addfile(_info("links/hard.txt", type_=tarfile.LNKTYPE, linkname="hello.txt"))
        tar.addfile(_info("links/soft.txt", type_=tarfile.SYMTYPE, linkname="../hello.txt"))

    return buf.getvalue()


def build_zstd(data: bytes, out: Path) -> None:
    """Compress via the system `zstd` CLI (stdin/stdout, so no filename
    or mtime metadata lands in the frame header)."""
    zstd_bin = shutil.which("zstd")
    if not zstd_bin:
        raise RuntimeError("`zstd` not found on PATH; cannot regenerate simple.tar.zst")
    result = subprocess.run(
        [zstd_bin, "-q", "-19"],
        input=data,
        stdout=subprocess.PIPE,
        check=True,
    )
    out.write_bytes(result.stdout)


def build_encrypted_zip(out: Path) -> None:
    """Build encrypted.zip via the system `zip` CLI.

    Two entries: `plain.txt` (cleartext) and `secret.txt` (ZipCrypto
    encrypted with password "secret"). `zip -P` applies the password to
    files added on that invocation, which is why we add them in two
    steps.
    """
    zip_bin = shutil.which("zip")
    if not zip_bin:
        raise RuntimeError("`zip` not found on PATH; cannot regenerate encrypted.zip")

    if out.exists():
        out.unlink()

    with tempfile.TemporaryDirectory() as td:
        tdp = Path(td)
        (tdp / "plain.txt").write_bytes(b"unencrypted\n")
        (tdp / "secret.txt").write_bytes(b"top secret\n")

        # Add cleartext entry.
        subprocess.run(
            [zip_bin, "-X", "-q", "-j", str(out), str(tdp / "plain.txt")],
            check=True,
            env={**os.environ, "TZ": "UTC"},
        )
        # Add encrypted entry.
        subprocess.run(
            [zip_bin, "-X", "-q", "-j", "-P", "secret", str(out), str(tdp / "secret.txt")],
            check=True,
            env={**os.environ, "TZ": "UTC"},
        )


def build_varied_zip(out: Path) -> None:
    """Build varied.zip with Python's zipfile: the structural gamut the ZIP
    VFS must handle — stored/deflated/bzip2/lzma members, an explicit and an
    implicit directory, a symlink (unix mode in external attrs, target as
    content), a unicode name, and unix permission bits throughout.

    Layout:

        hello.txt            stored,   b"hello world\\n", mode 0644
        dir/                 explicit directory entry, mode 0755
        dir/nested.txt       deflated, b"nested content\\n"
        dir/big.bin          deflated, 200_000 patterned bytes
        implicit/deep.txt    bzip2,    b"deep\\n" (no dir/ entry for implicit/)
        packed.lzma          lzma,     b"lzma packed\\n"
        links/soft.txt       symlink -> ../hello.txt
        π — unicode.txt      deflated, b"unicode name\\n" (UTF-8 flag)
    """
    import stat
    import zipfile

    dt = (2023, 11, 14, 22, 13, 20)  # matches MTIME

    def info(name: str, mode: int, is_dir: bool = False) -> zipfile.ZipInfo:
        zi = zipfile.ZipInfo(name, date_time=dt)
        zi.create_system = 3  # Unix
        type_bits = stat.S_IFDIR if is_dir else stat.S_IFREG
        zi.external_attr = ((type_bits | mode) << 16) | (0x10 if is_dir else 0)
        return zi

    with zipfile.ZipFile(out, "w") as zf:
        zf.writestr(info("hello.txt", 0o644), HELLO, zipfile.ZIP_STORED)
        zf.writestr(info("dir/", 0o755, is_dir=True), b"")
        zf.writestr(info("dir/nested.txt", 0o644), NESTED, zipfile.ZIP_DEFLATED)
        zf.writestr(info("dir/big.bin", 0o644), BIG, zipfile.ZIP_DEFLATED)
        zf.writestr(info("implicit/deep.txt", 0o600), b"deep\n", zipfile.ZIP_BZIP2)
        zf.writestr(info("packed.lzma", 0o644), b"lzma packed\n", zipfile.ZIP_LZMA)
        link = zipfile.ZipInfo("links/soft.txt", date_time=dt)
        link.create_system = 3
        link.external_attr = (stat.S_IFLNK | 0o777) << 16
        zf.writestr(link, b"../hello.txt", zipfile.ZIP_STORED)
        zf.writestr(info("π — unicode.txt", 0o644), b"unicode name\n", zipfile.ZIP_DEFLATED)


def main() -> None:
    data = build()
    (OUT_DIR / "simple.tar").write_bytes(data)

    # Deterministic gzip: empty filename header, mtime=0, max compression.
    with open(OUT_DIR / "simple.tar.gz", "wb") as f:
        gz = gzip.GzipFile(filename="", mode="wb", fileobj=f, mtime=0, compresslevel=9)
        try:
            gz.write(data)
        finally:
            gz.close()

    build_zstd(data, OUT_DIR / "simple.tar.zst")

    build_encrypted_zip(OUT_DIR / "encrypted.zip")
    build_varied_zip(OUT_DIR / "varied.zip")


if __name__ == "__main__":
    main()
