#!/usr/bin/env python3
"""Package ESP32 firmware, or write firmware.json for existing named binaries.

Build/package: uv run --with esptool==5.6.0 tools/release-firmware-manifest.py \
    --version 0.2.0 --build-dir app-esp32/target/xtensa-esp32s3-espidf/release
Manifest only: uv run tools/release-firmware-manifest.py --version 0.2.0
"""

import argparse
import csv
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile


def partition_number(value):
    value = value.strip().lower()
    multiplier = {"k": 1024, "m": 1024 * 1024}.get(value[-1:], 1)
    return int(value[:-1] if multiplier != 1 else value, 0) * multiplier


def package(build_dir, partitions, app, full):
    rows = {}
    with partitions.open() as source:
        for row in csv.reader(line for line in source if not line.lstrip().startswith("#")):
            if row and row[0].strip():
                rows[row[0].strip()] = [field.strip() for field in row]
    app_offset, app_size = map(partition_number, rows["ota_0"][3:5])
    ota_offset, ota_size = map(partition_number, rows["otadata"][3:5])
    esptool = [sys.executable, "-m", "esptool", "--chip", "esp32s3"]
    subprocess.run(esptool + ["elf2image", str(build_dir / "minidsp-esp32"),
                             "-o", str(app)], check=True)
    if app.stat().st_size > app_size:
        raise ValueError("App image exceeds the ota_0 partition")
    # esp-idf-sys may leave the IDF output in its build-script OUT_DIR.
    candidates = sorted(build_dir.rglob("ota_data_initial.bin"))
    with tempfile.TemporaryDirectory() as temporary:
        ota_data = Path(temporary) / "otadata.bin"
        data = candidates[0].read_bytes() if candidates else b""
        if len(data) > ota_size:
            raise ValueError("Initial OTA data exceeds the otadata partition")
        # Erased OTA metadata selects ota_0 when there is no factory partition.
        ota_data.write_bytes(data.ljust(ota_size, b"\xff"))
        subprocess.run(esptool + [
            "merge-bin", "--flash-size", "16MB", "-o", str(full),
            "0x0", str(build_dir / "bootloader.bin"),
            "0x8000", str(build_dir / "partition-table.bin"),
            hex(ota_offset), str(ota_data), hex(app_offset), str(app),
        ], check=True)


def asset(path):
    data = path.read_bytes()
    return {"name": path.name, "size": len(data), "sha256": hashlib.sha256(data).hexdigest()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True, help="Release version without the v prefix")
    parser.add_argument("--output-dir", type=Path, default=Path("dist"))
    parser.add_argument("--build-dir", type=Path, help="Also convert the ELF and merge flash images")
    parser.add_argument("--partitions", type=Path, default=Path("app-esp32/partitions.csv"))
    args = parser.parse_args()
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", args.version):
        parser.error("--version must be a semantic version without a v prefix")
    args.output_dir.mkdir(parents=True, exist_ok=True)
    stem = f"minidsp-open-esp32-{args.version}"
    app = args.output_dir / f"{stem}.bin"
    full = args.output_dir / f"{stem}-full.bin"
    if args.build_dir:
        package(args.build_dir, args.partitions, app, full)
    manifest = {
        "version": args.version,
        "chip": "esp32s3",
        "app": asset(app),
        "full": asset(full),
        "releaseNotesUrl": f"https://github.com/Averyy/minidsp-open/releases/tag/v{args.version}",
    }
    (args.output_dir / "firmware.json").write_text(json.dumps(manifest, separators=(",", ":")) + "\n")


if __name__ == "__main__":
    main()
