#!/usr/bin/env bash
# Build the ESP32 firmware and install it on a board over WiFi (OTA).
#
#   tools/ota-push.sh --board http://BOARD:5380            # release build + push
#   tools/ota-push.sh --board http://BOARD:5380 --debug    # debug build
#   tools/ota-push.sh --board http://BOARD:5380 --bin app.bin --skip-build
#
# The board writes the image to its inactive slot, verifies it, and reboots.
# A new image that never gets back on the network rolls back on its own.
# The partition table, bootloader, and ezBEQ bundle are not touched; the
# first switch to the OTA partition layout needs tools/flash-esp32.sh over USB.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_DIR="$ROOT_DIR/app-esp32"
BOARD_URL=""
PROFILE="release"
BIN=""
SKIP_BUILD=0
REBOOT_TIMEOUT_SEC=120

usage() {
  sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --board) BOARD_URL="${2%/}"; shift 2 ;;
    --debug) PROFILE="debug"; shift ;;
    --bin) BIN="$2"; shift 2 ;;
    --skip-build) SKIP_BUILD=1; shift ;;
    -h|--help) usage 0 ;;
    *) echo "unknown argument: $1" >&2; usage 1 ;;
  esac
done

[[ -n "$BOARD_URL" ]] || { echo "--board is required" >&2; usage 1; }
command -v curl >/dev/null || { echo "curl is required" >&2; exit 1; }

before="$(curl -fsS --max-time 5 "$BOARD_URL/firmware")" || {
  echo "board at $BOARD_URL is not answering GET /firmware (is it on OTA firmware yet?)" >&2
  exit 1
}
read -r before_build before_slot < <(printf '%s' "$before" | uv run python -c \
  'import json,sys; s=json.load(sys.stdin); print(s["build"], s["runningSlot"])')
echo "==> Board is running build $before_build from $before_slot"

if [[ -z "$BIN" ]]; then
  if [[ "$SKIP_BUILD" -eq 0 ]]; then
    echo "==> Building ESP32 firmware ($PROFILE)"
    if [[ "$PROFILE" == "release" ]]; then
      "$ROOT_DIR/tools/esp-cargo" build --release
    else
      "$ROOT_DIR/tools/esp-cargo" build
    fi
  fi
  BIN="$(mktemp /tmp/minidsp-ota.XXXXXX.bin)"
  trap 'rm -f "$BIN"' EXIT
  "$ROOT_DIR/tools/esptool" --chip esp32s3 elf2image \
    "$APP_DIR/target/xtensa-esp32s3-espidf/$PROFILE/minidsp-esp32" -o "$BIN" >/dev/null
fi

sha256="$(shasum -a 256 "$BIN" | awk '{print $1}')"
size="$(stat -f%z "$BIN" 2>/dev/null || stat -c%s "$BIN")"
echo "==> Uploading $size bytes (sha256 $sha256)"
curl -fsS --max-time 300 -H 'Content-Type: application/octet-stream' \
  --data-binary "@$BIN" "$BOARD_URL/firmware?sha256=$sha256" || {
  echo "upload failed; the board kept its current firmware" >&2
  exit 1
}
echo

echo "==> Waiting for the board to come back"
deadline=$((SECONDS + REBOOT_TIMEOUT_SEC))
sleep 5
while (( SECONDS < deadline )); do
  if status="$(curl -fsS --max-time 3 "$BOARD_URL/firmware" 2>/dev/null)"; then
    read -r build slot state < <(printf '%s' "$status" | uv run python -c \
      'import json,sys; s=json.load(sys.stdin); print(s["build"], s["runningSlot"], s["slotState"])')
    # Every successful OTA boots the other slot. The build string alone
    # can't tell two uncommitted local builds apart (both "<sha>-dirty").
    if [[ "$slot" != "$before_slot" ]]; then
      echo "==> Board is running build $build from $slot ($state)"
      [[ "$state" == "pendingVerify" ]] && echo "    it confirms itself after ~30 s on the network"
      exit 0
    fi
  fi
  sleep 2
done
echo "board did not come back on the other slot within ${REBOOT_TIMEOUT_SEC}s" >&2
echo "(an image that fails to reach the network rolls back to $before_build)" >&2
exit 1
