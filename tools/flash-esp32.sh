#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_DIR="$ROOT_DIR/app-esp32"
PARTITIONS_CSV="$APP_DIR/partitions.csv"
DEFAULT_PORT="/dev/ttyUSB0"
DEFAULT_BAUD="460800"
LITTLEFS_PY_VERSION="0.15.0"
LITTLEFS_BLOCK_SIZE=4096
DEFAULT_NAME_MAX=64
MIN_STORAGE_HEADROOM_BYTES=$((1024 * 1024))

PORT="$DEFAULT_PORT"
BAUD="$DEFAULT_BAUD"
BOARD_URL="${MINIDSP_BOARD_URL:-}"
SITE_DIR=""
SKIP_BUILD=0
SKIP_SITE_BUILD=0
SKIP_APP_FLASH=0
SKIP_STORAGE_FLASH=0

usage() {
  cat <<'EOF'
Usage: tools/flash-esp32.sh [options]

Builds the current ESP32 firmware in release mode, builds the latest ezBEQ site bundle,
creates a LittleFS image for the `storage` partition, flashes both app and
storage images, then optionally verifies the board's /version.txt after boot.

Options:
  --board-url URL       Optional board base URL to verify after flash, e.g. http://192.0.2.251:5380
  --port DEV            Serial port for esptool flashing
  --baud N              Serial baud for esptool flashing (default: 460800)
  --site-dir DIR        Existing site bundle directory (skips local site build)
  --skip-build          Skip `esp cargo build` for the firmware app image
  --skip-site-build     Skip `tools/build-catalogue.py` (requires --site-dir)
  --skip-app-flash      Do not flash bootloader/partition-table/app
  --skip-storage-flash  Do not flash the LittleFS storage image
  -h, --help            Show this help
EOF
}

require_cmd() {
  local cmd="$1"
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "missing required command: $cmd" >&2
    exit 1
  fi
}

trim() {
  local value="$1"
  value="${value#"${value%%[![:space:]]*}"}"
  value="${value%"${value##*[![:space:]]}"}"
  printf '%s' "$value"
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --board-url)
        BOARD_URL="$2"
        shift 2
        ;;
      --port)
        PORT="$2"
        shift 2
        ;;
      --baud)
        BAUD="$2"
        shift 2
        ;;
      --site-dir)
        SITE_DIR="$2"
        shift 2
        ;;
      --skip-build)
        SKIP_BUILD=1
        shift
        ;;
      --skip-site-build)
        SKIP_SITE_BUILD=1
        shift
        ;;
      --skip-app-flash)
        SKIP_APP_FLASH=1
        shift
        ;;
      --skip-storage-flash)
        SKIP_STORAGE_FLASH=1
        shift
        ;;
      -h|--help)
        usage
        exit 0
        ;;
      *)
        echo "unknown argument: $1" >&2
        usage >&2
        exit 1
        ;;
    esac
  done

  BOARD_URL="${BOARD_URL%/}"

  if [[ "$SKIP_SITE_BUILD" -eq 1 && -z "$SITE_DIR" ]]; then
    echo "--skip-site-build requires --site-dir" >&2
    exit 1
  fi
}

partition_csv_value() {
  local partition_name="$1"
  local field_index="$2"
  awk -F, -v name="$partition_name" -v field="$field_index" '
    function trim_field(s) {
      gsub(/^[ \t]+|[ \t]+$/, "", s)
      return s
    }
    $0 !~ /^[[:space:]]*#/ && trim_field($1) == name {
      print trim_field($field)
      exit
    }
  ' "$PARTITIONS_CSV"
}

find_sdkconfig() {
  find "$APP_DIR/target/xtensa-esp32s3-espidf" -path '*/out/sdkconfig' -type f 2>/dev/null | sort | tail -n1
}

sdkconfig_value() {
  local key="$1"
  local default_value="$2"
  local sdkconfig
  sdkconfig="$(find_sdkconfig || true)"
  if [[ -n "$sdkconfig" ]]; then
    local value
    value="$(awk -F= -v key="$key" '$1 == key {print $2; exit}' "$sdkconfig")"
    if [[ -n "$value" ]]; then
      printf '%s' "$value"
      return 0
    fi
  fi
  printf '%s' "$default_value"
}

wait_for_http() {
  local url="$1"
  local attempts="${2:-90}"
  local delay="${3:-1}"
  local i
  for ((i=1; i<=attempts; ++i)); do
    if curl -fsS "$url" >/dev/null 2>&1; then
      return 0
    fi
    sleep "$delay"
  done
  echo "timed out waiting for $url" >&2
  return 1
}

cleanup() {
  local exit_code=$?
  if [[ -n "${TEMP_SITE_DIR:-}" ]]; then
    rm -rf "$TEMP_SITE_DIR"
  fi
  if [[ -n "${TEMP_STORAGE_DIR:-}" ]]; then
    rm -rf "$TEMP_STORAGE_DIR"
  fi
  if [[ -n "${TEMP_STORAGE_IMAGE:-}" ]]; then
    rm -f "$TEMP_STORAGE_IMAGE"
  fi
  if [[ -n "${LITTLEFS_LOG:-}" ]]; then
    rm -f "$LITTLEFS_LOG"
  fi
  exit "$exit_code"
}

main() {
  parse_args "$@"

  require_cmd uv
  if [[ "$SKIP_APP_FLASH" -eq 0 || "$SKIP_STORAGE_FLASH" -eq 0 || "$SKIP_BUILD" -eq 0 ]]; then
    require_cmd esp
  fi
  if [[ -n "$BOARD_URL" ]]; then
    require_cmd curl
  fi

  local storage_offset_raw
  local storage_size_raw
  local storage_offset
  local storage_size
  local name_max
  local expected_version
  local board_version
  local storage_source_dir
  local storage_payload_bytes
  local storage_image_size
  local littlefs_log

  trap cleanup EXIT

  storage_offset_raw="$(partition_csv_value storage 4)"
  storage_size_raw="$(partition_csv_value storage 5)"
  if [[ -z "$storage_offset_raw" || -z "$storage_size_raw" ]]; then
    echo "failed to read storage partition offset/size from $PARTITIONS_CSV" >&2
    exit 1
  fi
  storage_offset="$storage_offset_raw"
  storage_size=$((storage_size_raw))

  if [[ "$SKIP_BUILD" -eq 0 && "$SKIP_APP_FLASH" -eq 0 ]]; then
    echo "==> Building ESP32 firmware (release)"
    (cd "$APP_DIR" && esp cargo build --release)
  fi

  if [[ -z "$SITE_DIR" ]]; then
    TEMP_SITE_DIR="$(mktemp -d /tmp/minidsp-site.XXXXXX)"
    SITE_DIR="$TEMP_SITE_DIR"
  fi

  if [[ "$SKIP_SITE_BUILD" -eq 0 ]]; then
    echo "==> Building latest ezBEQ site bundle"
    (cd "$ROOT_DIR" && uv run tools/build-catalogue.py --output-dir "$SITE_DIR")
  fi

  if [[ ! -f "$SITE_DIR/version.txt" ]]; then
    echo "site bundle is missing version.txt: $SITE_DIR" >&2
    exit 1
  fi
  expected_version="$(tr -d '\r\n' < "$SITE_DIR/version.txt")"
  if [[ -z "$expected_version" ]]; then
    echo "site bundle version.txt is empty: $SITE_DIR/version.txt" >&2
    exit 1
  fi

  TEMP_STORAGE_DIR="$(mktemp -d /tmp/minidsp-storage-dir.XXXXXX)"
  echo "==> Staging storage-partition files"
  uv run python -c '
import json, pathlib, shutil, sys

site_dir = pathlib.Path(sys.argv[1])
storage_dir = pathlib.Path(sys.argv[2])
manifest = json.loads((site_dir / "manifest.json").read_text())

for relative_path in manifest["files"]:
    src = site_dir / relative_path
    dst = storage_dir / relative_path
    dst.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(src, dst)
' "$SITE_DIR" "$TEMP_STORAGE_DIR"
  storage_source_dir="$TEMP_STORAGE_DIR"

  storage_payload_bytes="$(
    find "$storage_source_dir" -type f -exec stat -f%z {} + | awk '{sum += $1} END {print sum + 0}'
  )"
  storage_image_size=$(((storage_payload_bytes + MIN_STORAGE_HEADROOM_BYTES + LITTLEFS_BLOCK_SIZE - 1) / LITTLEFS_BLOCK_SIZE * LITTLEFS_BLOCK_SIZE))
  if (( storage_image_size > storage_size )); then
    storage_image_size="$storage_size"
  fi

  TEMP_STORAGE_IMAGE="$(mktemp /tmp/minidsp-storage.XXXXXX.bin)"
  littlefs_log="$(mktemp /tmp/minidsp-storage-log.XXXXXX.txt)"
  LITTLEFS_LOG="$littlefs_log"
  name_max="$(sdkconfig_value CONFIG_LITTLEFS_OBJ_NAME_LEN "$DEFAULT_NAME_MAX")"

  while :; do
    echo "==> Building LittleFS storage image (${storage_image_size} bytes used in $storage_size_raw partition at offset $storage_offset)"
    if uvx --from "littlefs-python==$LITTLEFS_PY_VERSION" littlefs-python create \
      "$storage_source_dir" \
      "$TEMP_STORAGE_IMAGE" \
      -v \
      --fs-size="$storage_image_size" \
      --name-max="$name_max" \
      --block-size="$LITTLEFS_BLOCK_SIZE" >"$littlefs_log" 2>&1; then
      cat "$littlefs_log"
      break
    fi

    cat "$littlefs_log" >&2
    if grep -q 'LFS_ERR_NOSPC' "$littlefs_log" && (( storage_image_size < storage_size )); then
      storage_image_size=$((storage_image_size + MIN_STORAGE_HEADROOM_BYTES))
      if (( storage_image_size > storage_size )); then
        storage_image_size="$storage_size"
      fi
      rm -f "$TEMP_STORAGE_IMAGE"
      TEMP_STORAGE_IMAGE="$(mktemp /tmp/minidsp-storage.XXXXXX.bin)"
      continue
    fi
    exit 1
  done
  rm -f "$littlefs_log"

  if [[ "$SKIP_APP_FLASH" -eq 0 || "$SKIP_STORAGE_FLASH" -eq 0 ]]; then
    echo "==> Flashing ESP32 on $PORT"
    if [[ "$SKIP_APP_FLASH" -eq 0 ]]; then
      esp esptool.py --chip esp32s3 elf2image \
        "$APP_DIR/target/xtensa-esp32s3-espidf/release/minidsp-esp32" \
        -o /tmp/app.bin
      esp esptool.py --chip esp32s3 -p "$PORT" -b "$BAUD" write_flash --flash_size 16MB \
        0x0 "$APP_DIR/target/xtensa-esp32s3-espidf/release/bootloader.bin" \
        0x8000 "$APP_DIR/target/xtensa-esp32s3-espidf/release/partition-table.bin" \
        0x10000 /tmp/app.bin
    fi

    if [[ "$SKIP_STORAGE_FLASH" -eq 0 ]]; then
      echo "==> Erasing storage partition region $storage_offset ($storage_size_raw)"
      esp esptool.py --chip esp32s3 -p "$PORT" -b "$BAUD" erase_region "$storage_offset" "$storage_size_raw"
      echo "==> Flashing LittleFS storage image at $storage_offset"
      esp esptool.py --chip esp32s3 -p "$PORT" -b "$BAUD" write_flash --flash_size 16MB \
        "$storage_offset" "$TEMP_STORAGE_IMAGE"
    fi
  fi

  if [[ -n "$BOARD_URL" && "$SKIP_STORAGE_FLASH" -eq 0 ]]; then
    echo "==> Waiting for board HTTP at $BOARD_URL"
    wait_for_http "$BOARD_URL/version.txt" 120 1
    board_version="$(curl -fsS "$BOARD_URL/version.txt" | tr -d '\r\n')"
    if [[ "$board_version" != "$expected_version" ]]; then
      echo "board version mismatch: expected $expected_version, got $board_version" >&2
      exit 1
    fi
    echo "==> Board is serving flashed version $board_version"
  fi
}

main "$@"
