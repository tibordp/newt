#!/usr/bin/env python3
"""Stage package artifacts and build the website's latest.json manifest."""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import quote


SUPPORTED_FORMATS = ("pkg.tar.zst", "appimage", "dmg", "msi", "deb", "rpm")


def package_format(path: Path) -> str | None:
    name = path.name.lower()
    for suffix in SUPPORTED_FORMATS:
        if name.endswith(f".{suffix}"):
            return suffix
    return None


def artifact_metadata(name: str) -> dict[str, str]:
    if name.startswith("package-macos-"):
        return {
            "platform": "macos",
            "architecture": name.removeprefix("package-macos-"),
        }

    if name.startswith("package-windows-"):
        return {
            "platform": "windows",
            "architecture": name.removeprefix("package-windows-"),
        }

    if name.startswith("package-appimage-"):
        return {
            "platform": "linux",
            "architecture": name.removeprefix("package-appimage-").replace("arm64", "aarch64"),
        }

    if name.startswith("package-"):
        distro = name.removeprefix("package-")
        return {
            "platform": "linux",
            "architecture": "aarch64" if distro.endswith("-arm64") else "x86_64",
            "distro": distro,
        }

    raise ValueError(f"Unrecognized package artifact directory: {name}")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifacts-dir", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--artifact-prefix", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--run-attempt", required=True, type=int)
    parser.add_argument("--artifact-set", choices=("lean", "full"), required=True)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    artifact_dirs = sorted(path for path in args.artifacts_dir.iterdir() if path.is_dir())
    if not artifact_dirs:
        raise SystemExit(f"No artifact directories found in {args.artifacts_dir}")

    staged_root = args.output_dir / "artifacts"
    staged_root.mkdir(parents=True, exist_ok=True)
    manifest_artifacts: list[dict[str, str | int]] = []

    for artifact_dir in artifact_dirs:
        metadata = artifact_metadata(artifact_dir.name)
        packages = sorted(
            path for path in artifact_dir.rglob("*") if path.is_file() and package_format(path)
        )
        if not packages:
            raise SystemExit(f"No supported package found in {artifact_dir}")

        destination_dir = staged_root / artifact_dir.name
        destination_dir.mkdir(parents=True, exist_ok=True)

        for package in packages:
            format_name = package_format(package)
            assert format_name is not None
            checksum = sha256(package)
            destination = destination_dir / package.name
            shutil.copy2(package, destination)
            destination.with_name(f"{destination.name}.sha256").write_text(
                f"{checksum}  {package.name}\n", encoding="utf-8"
            )

            encoded_dir = quote(artifact_dir.name, safe="")
            encoded_name = quote(package.name, safe="")
            url = f"/downloads/artifacts/{args.artifact_prefix}/{encoded_dir}/{encoded_name}"
            manifest_artifacts.append(
                {
                    **metadata,
                    "format": format_name,
                    "filename": package.name,
                    "url": url,
                    "checksum_url": f"{url}.sha256",
                    "sha256": checksum,
                    "size": package.stat().st_size,
                }
            )

    manifest = {
        "schema_version": 1,
        "commit": args.commit,
        "version": args.version,
        "built_at": datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z"),
        "run_id": args.run_id,
        "run_attempt": args.run_attempt,
        "artifact_set": args.artifact_set,
        "artifacts": sorted(
            manifest_artifacts,
            key=lambda item: (
                str(item["platform"]),
                str(item.get("distro", "")),
                str(item["architecture"]),
                str(item["format"]),
            ),
        ),
    }
    manifest_path = args.output_dir / "latest.json"
    manifest_path.write_text(f"{json.dumps(manifest, indent=2)}\n", encoding="utf-8")
    print(f"Staged {len(manifest_artifacts)} packages and wrote {manifest_path}")


if __name__ == "__main__":
    main()
