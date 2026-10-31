#!/usr/bin/env python3
"""Print a release's GitHub notes: its CHANGELOG.md section plus the downloads.

usage: python3 tools/release-notes.py VERSION [--allow-unreleased]
"""

import argparse
from pathlib import Path
import re
import sys


def section(changelog, heading):
    match = re.search(rf"^## {heading}[^\n]*\n(.*?)(?=^## |\Z)", changelog, re.S | re.M)
    return match.group(1).strip() if match else None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", help="Release version without the v prefix")
    parser.add_argument("--changelog", type=Path, default=Path("CHANGELOG.md"))
    parser.add_argument("--allow-unreleased", action="store_true",
                        help="Use the Unreleased section when the version has none (dry runs)")
    args = parser.parse_args()
    changelog = args.changelog.read_text()
    notes = section(changelog, re.escape(f"[{args.version}]"))
    if notes is None and args.allow_unreleased:
        notes = section(changelog, r"\[?Unreleased\]?")
    if not notes and args.allow_unreleased:
        notes = "No changes recorded yet."
    if not notes:
        sys.exit(f'{args.changelog} has no "## [{args.version}]" section with entries')
    v = args.version
    # Relative links in CHANGELOG.md would resolve against the release page.
    notes = re.sub(r"\]\((?!https?://|#|mailto:)([^)\s]+)\)",
                   rf"](https://github.com/Averyy/minidsp-open/blob/v{v}/\1)", notes)
    print(notes)
    print(f"""
## Downloads

- `minidsp-{v}-<target>.tar.gz` or `.zip`: the `minidsp` CLI and daemon for Linux (x86_64, ARM64), macOS (Intel, Apple silicon) and Windows (x86_64).
- `minidsp-open-esp32-{v}-full.bin`: the first USB flash of an ESP32-S3 board. See [First install](https://github.com/Averyy/minidsp-open#first-install).
- `minidsp-open-esp32-{v}.bin`: the app image for WiFi updates. Boards install it from their `/update` page.
- `firmware.json`: what boards read to find this release.
- `SHA256SUMS`: checksums for every file above.""")


if __name__ == "__main__":
    main()
