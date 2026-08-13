#!/usr/bin/env python3
"""Assemble the latest.json manifest the Tauri updater polls.

Tauri emits one updater payload per platform plus a detached `.sig` next to it.
This walks the collected artifacts, pairs each payload with its signature, and
writes the manifest pointing at the release the assets were uploaded to.

Deliberately strict: a payload with no signature, or a platform that produced
nothing, aborts the release. A manifest that silently omits a platform would
strand exactly the users already running it, and they would never be told —
their app would just keep reporting "up to date" forever.
"""

from __future__ import annotations

import argparse
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

# Filename suffix -> the platform key the updater looks itself up by.
# macOS updates ship as .app.tar.gz; the .dmg is only for first install.
PLATFORMS: list[tuple[str, str]] = [
    ("aarch64.app.tar.gz", "darwin-aarch64"),
    ("x64.app.tar.gz", "darwin-x86_64"),
    ("amd64.AppImage", "linux-x86_64"),
    ("aarch64.AppImage", "linux-arm64"),
    ("x64-setup.exe", "windows-x86_64"),
    ("arm64-setup.exe", "windows-aarch64"),
]

# Platforms a release must contain. The arm64 Linux and Windows entries above
# are recognised if present but are not currently built.
REQUIRED = {"darwin-aarch64", "darwin-x86_64", "linux-x86_64", "windows-x86_64"}


def platform_for(name: str) -> str | None:
    for suffix, key in PLATFORMS:
        if name.endswith(suffix):
            return key
    return None


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--artifacts", required=True, type=Path)
    ap.add_argument("--tag", required=True, help="e.g. desktop-v0.1.0")
    ap.add_argument("--repo", required=True, help="owner/name")
    ap.add_argument("--output", required=True, type=Path)
    args = ap.parse_args()

    version = args.tag.removeprefix("desktop-v")
    if not version or version == args.tag:
        print(f"error: tag {args.tag!r} is not of the form desktop-vX.Y.Z", file=sys.stderr)
        return 1

    base = f"https://github.com/{args.repo}/releases/download/{args.tag}"

    platforms: dict[str, dict[str, str]] = {}
    problems: list[str] = []

    for path in sorted(args.artifacts.iterdir()):
        if not path.is_file() or path.name.endswith(".sig"):
            continue

        key = platform_for(path.name)
        if key is None:
            continue

        sig = path.with_name(path.name + ".sig")
        if not sig.is_file():
            problems.append(f"{path.name}: no {sig.name} beside it")
            continue

        signature = sig.read_text().strip()
        if not signature:
            problems.append(f"{sig.name}: empty")
            continue

        if key in platforms:
            problems.append(f"{key}: matched more than one artifact")
            continue

        platforms[key] = {"signature": signature, "url": f"{base}/{path.name}"}

    missing = REQUIRED - platforms.keys()
    if missing:
        problems.append("no updater artifact for: " + ", ".join(sorted(missing)))

    if problems:
        print("error: refusing to publish an incomplete update manifest", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        print(
            "\nUsually this means TAURI_SIGNING_PRIVATE_KEY was not set, so the\n"
            "build produced no signatures.",
            file=sys.stderr,
        )
        return 1

    manifest = {
        "version": version,
        "notes": f"See https://github.com/{args.repo}/releases/tag/{args.tag}",
        "pub_date": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "platforms": dict(sorted(platforms.items())),
    }

    args.output.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"wrote {args.output} for {version} ({len(platforms)} platforms)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
