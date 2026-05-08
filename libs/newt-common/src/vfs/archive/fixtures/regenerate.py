#!/usr/bin/env python3
"""Regenerate archive test fixtures used by TarArchiveVfs / ZipArchiveVfs tests.

Produces deterministic `simple.tar`, `simple.tar.gz` and `encrypted.zip`
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

    build_encrypted_zip(OUT_DIR / "encrypted.zip")


if __name__ == "__main__":
    main()
