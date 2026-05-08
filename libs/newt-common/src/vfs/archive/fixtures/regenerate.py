#!/usr/bin/env python3
"""Regenerate tar test fixtures used by TarArchiveVfs tests.

Produces deterministic `simple.tar` and `simple.tar.gz` so the bytes
committed to the repo are reproducible. Run from this directory:

    uv run --no-project python regenerate.py

Layout produced:

    hello.txt            -> b"hello world\\n"
    dir/nested.txt       -> b"nested content\\n"
    dir/big.bin          -> 200_000 bytes of (i % 251) — exercises multi-chunk
                            streaming (>= one VFS_READ_CHUNK_SIZE = 64 KiB).
    links/hard.txt       -> hardlink to hello.txt
    links/soft.txt       -> symlink to ../hello.txt
"""

import gzip
import io
import tarfile
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


if __name__ == "__main__":
    main()
