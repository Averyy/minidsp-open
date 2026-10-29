#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_DIR="$ROOT_DIR/app-esp32"
DEFAULT_HTTP_PORT=8766

PORT="${MINIDSP_ESP32_PORT:-}"
BOARD_URL="${MINIDSP_BOARD_URL:-}"
SITE_DIR=""
LOCAL_PORT="$DEFAULT_HTTP_PORT"
SKIP_BUILD=0
SKIP_FLASH=0
SKIP_SITE_BUILD=0
KEEP_OVERRIDE=0

usage() {
  cat <<'EOF'
Usage: tools/flash-esp32-with-site.sh --board-url http://BOARD:5380 [options]

Builds the latest hosted ezBEQ bundle, serves it locally, points the board
updater at that temporary server, triggers a refresh, then restores the normal
updater URL. This is the updater-path fallback and explicit updater test flow;
the default local flashing workflow is `tools/flash-esp32.sh`, which flashes a
LittleFS storage image directly.

Options:
  --board-url URL      Board base URL, e.g. http://192.0.2.251:5380
  --port DEV           Serial port for esptool flashing (or MINIDSP_ESP32_PORT)
  --site-dir DIR       Existing site bundle directory (skips local site build)
  --http-port N        Local temporary HTTP port for side-load server
  --skip-build         Skip `tools/esp-cargo build`
  --skip-flash         Skip esptool flashing
  --skip-site-build    Skip `tools/build-catalogue.py` (requires --site-dir)
  --keep-override      Leave the board updater override pointed at the local site
  -h, --help           Show this help
EOF
}

partition_csv_value() {
  awk -F, -v name="$1" -v field="$2" '
    function trim_field(s) { gsub(/^[ \t]+|[ \t]+$/, "", s); return s }
    $0 !~ /^[[:space:]]*#/ && trim_field($1) == name { print trim_field($field); exit }
  ' "$APP_DIR/partitions.csv"
}

require_cmd() {
  local cmd="$1"
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "missing required command: $cmd" >&2
    exit 1
  fi
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
      --site-dir)
        SITE_DIR="$2"
        shift 2
        ;;
      --http-port)
        LOCAL_PORT="$2"
        shift 2
        ;;
      --skip-build)
        SKIP_BUILD=1
        shift
        ;;
      --skip-flash)
        SKIP_FLASH=1
        shift
        ;;
      --skip-site-build)
        SKIP_SITE_BUILD=1
        shift
        ;;
      --keep-override)
        KEEP_OVERRIDE=1
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

  if [[ -z "$BOARD_URL" ]]; then
    echo "--board-url is required" >&2
    usage >&2
    exit 1
  fi

  if [[ -z "$PORT" ]]; then
    echo "--port or MINIDSP_ESP32_PORT is required" >&2
    exit 1
  fi

  BOARD_URL="${BOARD_URL%/}"

  if [[ "$SKIP_SITE_BUILD" -eq 1 && -z "$SITE_DIR" ]]; then
    echo "--skip-site-build requires --site-dir" >&2
    exit 1
  fi
}

detect_local_ip() {
  local iface
  iface="$(route -n get default 2>/dev/null | awk '/interface:/{print $2; exit}')"
  if [[ -n "$iface" ]]; then
    ipconfig getifaddr "$iface" 2>/dev/null && return 0
  fi
  if command -v ip >/dev/null 2>&1; then
    ip route get 1.1.1.1 2>/dev/null | awk '/src/{for (i=1; i<=NF; ++i) if ($i=="src") {print $(i+1); exit}}' && return 0
  fi
  return 1
}

wait_for_http() {
  local url="$1"
  local attempts="${2:-60}"
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

post_json() {
  local url="$1"
  local body="$2"
  curl -fsS -X POST "$url" -H 'Content-Type: application/json' -d "$body"
}

cleanup() {
  local exit_code=$?
  if [[ -n "${HTTP_PID:-}" ]]; then
    kill "$HTTP_PID" >/dev/null 2>&1 || true
    wait "$HTTP_PID" 2>/dev/null || true
  fi
  if [[ "${KEEP_OVERRIDE:-0}" -eq 0 && "${OVERRIDE_APPLIED:-0}" -eq 1 ]]; then
    curl -fsS -X POST "$BOARD_URL/debug/updater" \
      -H 'Content-Type: application/json' \
      -d '{"baseUrl":""}' >/dev/null 2>&1 || true
  fi
  if [[ -n "${TEMP_SITE_DIR:-}" ]]; then
    rm -rf "$TEMP_SITE_DIR"
  fi
  exit "$exit_code"
}

main() {
  parse_args "$@"
  require_cmd curl
  require_cmd uv
  if [[ "$SKIP_BUILD" -eq 0 || "$SKIP_FLASH" -eq 0 ]]; then
    require_cmd uvx
  fi

  local local_ip
  local site_url
  local update_response
  local final_status
  local expected_version
  local board_version

  trap cleanup EXIT

  local_ip="$(detect_local_ip)"
  if [[ -z "$local_ip" ]]; then
    echo "failed to detect local IP for temporary site server" >&2
    exit 1
  fi
  site_url="http://$local_ip:$LOCAL_PORT"

  if [[ "$SKIP_BUILD" -eq 0 ]]; then
    echo "==> Building ESP32 firmware"
    "$ROOT_DIR/tools/esp-cargo" build
  fi

  if [[ "$SKIP_FLASH" -eq 0 ]]; then
    echo "==> Flashing ESP32 firmware on $PORT"
    "$ROOT_DIR/tools/esptool" --chip esp32s3 elf2image \
      "$APP_DIR/target/xtensa-esp32s3-espidf/debug/minidsp-esp32" \
      -o /tmp/app.bin
    # Blank otadata makes the bootloader start ota_0, the slot written here.
    "$ROOT_DIR/tools/esptool" --chip esp32s3 -p "$PORT" -b 460800 erase-region \
      "$(partition_csv_value otadata 4)" "$(partition_csv_value otadata 5)"
    "$ROOT_DIR/tools/esptool" --chip esp32s3 -p "$PORT" -b 460800 write-flash \
      --flash-size 16MB \
      0x0 "$APP_DIR/target/xtensa-esp32s3-espidf/debug/bootloader.bin" \
      0x8000 "$APP_DIR/target/xtensa-esp32s3-espidf/debug/partition-table.bin" \
      "$(partition_csv_value ota_0 4)" /tmp/app.bin
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

  echo "==> Starting temporary site server at $site_url"
  (cd "$ROOT_DIR" && uv run python -m http.server "$LOCAL_PORT" --bind 0.0.0.0 --directory "$SITE_DIR" >/tmp/minidsp-site-server.log 2>&1) &
  HTTP_PID=$!
  wait_for_http "$site_url/manifest.json" 30 1

  echo "==> Waiting for board HTTP at $BOARD_URL"
  wait_for_http "$BOARD_URL/debug/info" 90 1

  echo "==> Pointing board updater at temporary site"
  post_json "$BOARD_URL/debug/updater" "{\"baseUrl\":\"$site_url\"}" >/dev/null
  OVERRIDE_APPLIED=1

  echo "==> Triggering board-side update"
  update_response="$(post_json "$BOARD_URL/debug/updater/check" '{}')"
  echo "$update_response"

  final_status="$(printf '%s' "$update_response" | uv run python -c 'import json,sys; print(json.load(sys.stdin).get("status",""))')"
  if [[ "$final_status" != "updated" && "$final_status" != "current" ]]; then
    echo "unexpected updater result: $final_status" >&2
    exit 1
  fi

  board_version="$(curl -fsS "$BOARD_URL/version.txt" | tr -d '\r\n')"
  if [[ "$board_version" != "$expected_version" ]]; then
    echo "board version mismatch: expected $expected_version, got $board_version" >&2
    exit 1
  fi
  echo "==> Board is serving version $board_version"

  if [[ "$KEEP_OVERRIDE" -eq 0 ]]; then
    echo "==> Restoring normal updater URL"
    post_json "$BOARD_URL/debug/updater" '{"baseUrl":""}' >/dev/null
    OVERRIDE_APPLIED=0
    final_status="$(curl -fsS "$BOARD_URL/debug/updater")"
    echo "$final_status"
  fi
}

main "$@"
