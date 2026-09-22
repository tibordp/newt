#!/usr/bin/env python3
"""Regenerate the 7z fixtures used by the `newt_archive::sevenz` tests.

    uv run --with py7zr --with pyzstd python regenerate.py

Every archive comes from 7-Zip's `7zz` (26.03 when last regenerated) except
`zstd.7z`, which 7-Zip cannot write and py7zr can. Contents are
deterministic: `pattern(i, n)`, `noise(seed, n)` and `code_like(seed, n)` are
mirrored in `tests.rs`, which checks every byte read back. mtime is fixed;
7-Zip stores it at full precision.

    basic.7z            LZMA2, solid. hello.txt, dir/nested.txt, dir/big.bin
                        (pattern, 300_000), empty.txt (empty file), emptydir/,
                        links/soft.txt -> ../hello.txt (-snl), "π — unicode.txt".
    solid.7z            LZMA2, one solid folder: 24 noise/pattern files, ~1.5 MiB.
    blocks.7z           Same set with -ms=200k: several solid folders.
    nonsolid.7z         Small set with -ms=off: one folder per file.
    bigdict.7z          Small set with -md=64m.
    times.7z            Small set with -mtc=on -mta=on (ctime and atime stored).
    lzma.7z copy.7z bzip2.7z deflate.7z zstd.7z
                        Single-coder variants of the small set.
    x86.7z arm.7z armt.7z ppc.7z sparc.7z ia64.7z arm64.7z riscv.7z
                        [BCJ filter, LZMA2] over code-like bytes (-mf=…).
    delta.7z            [Delta:4, LZMA2].
    bcj2.7z             [BCJ2 over LZMA2 + two LZMA side streams] over CODE.
    bcj2_tails.7z       BCJ2, solid: code ending in a converted call, a bare
                        E8, an E8 with three bytes after it, and 0F 85.
    bcj2_encrypted.7z   BCJ2 with a password: one AES coder per packed stream.
    ppmd.7z deflate64.7z
                        Coders outside scope: list, refuse to read.
    encrypted.7z        AES-256 on the data, cleartext header. Password "secret".
    encrypted_header.7z AES-256 on the header too (-mhe=on). Password "secret".
"""

import os
import subprocess
import tempfile
from pathlib import Path

OUT = Path(__file__).parent
MTIME = 1_700_000_000


def pattern(i: int, n: int) -> bytes:
    return bytes((j * 7 + i) % 251 for j in range(n))


def noise(seed: int, n: int) -> bytes:
    s = seed
    out = bytearray()
    while len(out) < n:
        s = (s * 6364136223846793005 + 1442695040888963407) & ((1 << 64) - 1)
        out.append((s >> 33) & 0xFF)
    return bytes(out)


def code_like(seed: int, n: int) -> bytes:
    s = seed
    out = bytearray()
    while len(out) < n:
        s = (s * 6364136223846793005 + 1442695040888963407) & ((1 << 64) - 1)
        r = (s >> 33) & 0xFFFFFFFF
        k = r % 6
        if k == 0:
            out += b"\xE8" + ((r >> 8) % 4096).to_bytes(4, "little")
        elif k == 1:
            out += (0xEB000000 | (r & 0x00FFFFFF)).to_bytes(4, "big")
        elif k == 2:
            out += b"the quick brown fox "
        else:
            out += (r & 0xFFFFFFFF).to_bytes(4, "little")
    return bytes(out[:n])


BASIC = {
    "hello.txt": b"hello world\n",
    "dir/nested.txt": b"nested content\n",
    "dir/big.bin": pattern(3, 300_000),
    "empty.txt": b"",
    "π — unicode.txt": "unicode ☃\n".encode(),
}

SMALL = dict(BASIC)
SMALL["dir/big.bin"] = pattern(3, 100_000)

SOLID = {}
for i in range(24):
    if i % 3 == 0:
        SOLID[f"solid/noise{i:02}.bin"] = noise(i + 1, 40_000 + i * 1_000)
    else:
        SOLID[f"solid/pat{i:02}.bin"] = pattern(i, 50_000 + i * 2_000)

CODE = {"prog.bin": code_like(9, 60_000), "readme.txt": b"code-like payload\n"}

TAILS = {
    "call.bin": code_like(9, 60_000)[:-5] + b"\xE8\x10\x00\x00\x00",
    "e8.bin": code_like(9, 60_000) + b"\xE8",
    "e8_3.bin": code_like(9, 60_000) + b"\xE8\x01\x02",
    "jcc.bin": code_like(9, 60_000) + b"\x0F\x85",
}


def populate(root: Path, files: dict, symlink=False, emptydir=False):
    for name, data in files.items():
        p = root / name
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data)
        os.utime(p, (MTIME, MTIME))
    if emptydir:
        (root / "emptydir").mkdir()
    if symlink:
        (root / "links").mkdir()
        os.symlink("../hello.txt", root / "links" / "soft.txt")
        os.utime(root / "links" / "soft.txt", (MTIME, MTIME), follow_symlinks=False)
    # Directories last, so populating them did not bump their mtime.
    for d in [p for p in root.rglob("*") if p.is_dir() and not p.is_symlink()] + [root]:
        os.utime(d, (MTIME, MTIME))


def sevenz(name: str, files: dict, *switches: str, **kw):
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp) / "src"
        root.mkdir()
        populate(root, files, **kw)
        out = OUT / name
        if out.exists():
            out.unlink()
        subprocess.run(
            ["7zz", "a", "-y", "-bso0", "-bsp0", "-snl", *switches, str(out), "."],
            cwd=root,
            check=True,
        )
        print(f"{name}: {out.stat().st_size} bytes")


def py7zr_zstd(name: str, files: dict):
    import py7zr

    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp) / "src"
        root.mkdir()
        populate(root, files)
        out = OUT / name
        if out.exists():
            out.unlink()
        with py7zr.SevenZipFile(out, "w", filters=[{"id": py7zr.FILTER_ZSTD}]) as z:
            for p in sorted(root.rglob("*"), key=lambda p: str(p.relative_to(root))):
                z.write(p, str(p.relative_to(root)))
        print(f"{name}: {out.stat().st_size} bytes (py7zr)")


sevenz("basic.7z", BASIC, symlink=True, emptydir=True)
sevenz("solid.7z", SOLID)
sevenz("blocks.7z", SOLID, "-ms=200k")
sevenz("nonsolid.7z", SMALL, "-ms=off")
sevenz("bigdict.7z", SMALL, "-md=64m")
sevenz("times.7z", SMALL, "-mtc=on", "-mta=on")
sevenz("lzma.7z", SMALL, "-m0=LZMA")
sevenz("copy.7z", SMALL, "-m0=Copy")
sevenz("bzip2.7z", SMALL, "-m0=BZip2")
sevenz("deflate.7z", SMALL, "-m0=Deflate")
py7zr_zstd("zstd.7z", SMALL)
for arch in ["BCJ", "ARM", "ARMT", "PPC", "SPARC", "IA64", "ARM64", "RISCV"]:
    name = {"BCJ": "x86"}.get(arch, arch.lower())
    sevenz(f"{name}.7z", CODE, f"-mf={arch}")
sevenz("delta.7z", SMALL, "-mf=Delta:4")
sevenz("ppmd.7z", SMALL, "-m0=PPMd")
sevenz("bcj2.7z", CODE, "-mf=BCJ2")
sevenz("bcj2_tails.7z", TAILS, "-mf=BCJ2")
sevenz("bcj2_encrypted.7z", CODE, "-mf=BCJ2", "-psecret")
sevenz("deflate64.7z", SMALL, "-m0=Deflate64")
sevenz("encrypted.7z", SMALL, "-psecret")
sevenz("encrypted_header.7z", SMALL, "-psecret", "-mhe=on")
