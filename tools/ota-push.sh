#!/usr/bin/env bash
# Build the ESP32 firmware and install it on a board over WiFi (OTA).
#
#   tools/ota-push.sh --board http://BOARD:5380            # release build + push
#   tools/ota-push.sh --board http://BOARD:5380 --password-file /path/to/password
#   MINIDSP_PASSWORD=... tools/ota-push.sh --board http://BOARD:5380
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
PASSWORD_FILE=""
AUTH_CONFIG=""
TEMP_BIN=""
CURL_AUTH=()
# bash 3.2 (macOS) treats an empty "${CURL_AUTH[@]}" as unbound under set -u.
board_curl() {
  if ((${#CURL_AUTH[@]})); then curl "${CURL_AUTH[@]}" "$@"; else curl "$@"; fi
}
cleanup() {
  [[ -z "$AUTH_CONFIG" ]] || rm -f "$AUTH_CONFIG"
  [[ -z "$TEMP_BIN" ]] || rm -f "$TEMP_BIN"
}
trap cleanup EXIT
PROFILE="release"
BIN=""
SKIP_BUILD=0
REBOOT_TIMEOUT_SEC=120

usage() {
  sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --board) BOARD_URL="${2%/}"; shift 2 ;;
    --password-file) PASSWORD_FILE="$2"; shift 2 ;;
    --debug) PROFILE="debug"; shift ;;
    --bin) BIN="$2"; shift 2 ;;
    --skip-build) SKIP_BUILD=1; shift ;;
    -h|--help) usage 0 ;;
    *) echo "unknown argument: $1" >&2; usage 1 ;;
  esac
done

[[ -n "$BOARD_URL" ]] || { echo "--board is required" >&2; usage 1; }
command -v curl >/dev/null || { echo "curl is required" >&2; exit 1; }

if [[ -n "$PASSWORD_FILE" || -n "${MINIDSP_PASSWORD+x}" ]]; then
  AUTH_CONFIG="$(mktemp /tmp/minidsp-ota-auth.XXXXXX)"
  chmod 600 "$AUTH_CONFIG"
  uv run python - "$PASSWORD_FILE" "$AUTH_CONFIG" <<'PYAUTH'
import base64, os, pathlib, sys
try:
    if sys.argv[1]:
        password = pathlib.Path(sys.argv[1]).read_bytes().decode('utf-8')
        if password.endswith('\n'):
            password = password[:-1]
            if password.endswith('\r'): password = password[:-1]
    else:
        password = os.environ['MINIDSP_PASSWORD']
    if not 1 <= len(password.encode('utf-8')) <= 128:
        raise ValueError('invalid password length')
    header = base64.b64encode(('admin:' + password).encode()).decode()
    pathlib.Path(sys.argv[2]).write_text('header = "Authorization: Basic ' + header + '"\n')
except (OSError, UnicodeError, ValueError, KeyError):
    sys.exit('could not load password (expected 1–128 UTF-8 bytes)')
PYAUTH
  CURL_AUTH=(--config "$AUTH_CONFIG")
fi

before="$(board_curl -fsS --max-time 5 "$BOARD_URL/firmware")" || {
  code="$(board_curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$BOARD_URL/firmware" || true)"
  if [[ "$code" == "401" ]]; then
    echo "board at $BOARD_URL needs its password: pass --password-file FILE or set MINIDSP_PASSWORD" >&2
  else
    echo "board at $BOARD_URL is not answering GET /firmware (is it on OTA firmware yet?)" >&2
  fi
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
  TEMP_BIN="$BIN"
  "$ROOT_DIR/tools/esptool" --chip esp32s3 elf2image \
    "$APP_DIR/target/xtensa-esp32s3-espidf/$PROFILE/minidsp-esp32" -o "$BIN" >/dev/null
fi

sha256="$(shasum -a 256 "$BIN" | awk '{print $1}')"
size="$(stat -f%z "$BIN" 2>/dev/null || stat -c%s "$BIN")"
echo "==> Uploading $size bytes (sha256 $sha256)"
upload_rc=0
board_curl -fsS --max-time 300 -H 'Content-Type: application/octet-stream' \
  --data-binary "@$BIN" "$BOARD_URL/firmware?sha256=$sha256" || upload_rc=$?
if (( upload_rc == 22 )); then
  # curl exit 22 is an HTTP error the board sent, and it only sends one
  # without installing (a broken upload gets 500).
  echo "upload failed; the board kept its current firmware" >&2
  exit 1
elif (( upload_rc != 0 )); then
  # The reply can be lost after a complete upload: the board reboots 1.5 s
  # after answering. Only a board still up on the same slot, and not
  # rebooting, kept its firmware. A weak link can drop one status read, so
  # try a few.
  sleep 5
  for _ in 1 2 3; do
    if status="$(board_curl -fsS --max-time 5 "$BOARD_URL/firmware" 2>/dev/null)"; then
      read -r now_slot now_state < <(printf '%s' "$status" | uv run python -c \
        'import json,sys; s=json.load(sys.stdin); print(s["runningSlot"], (s.get("progress") or {}).get("state") or "idle")')
      if [[ "$now_slot" == "$before_slot" && "$now_state" != "rebooting" ]]; then
        echo "upload failed; the board kept its current firmware" >&2
        exit 1
      fi
      break
    fi
    sleep 2
  done
  echo "upload reply lost, but the board is installing or rebooting"
fi
echo

echo "==> Waiting for the board to come back"
deadline=$((SECONDS + REBOOT_TIMEOUT_SEC))
sleep 5
while (( SECONDS < deadline )); do
  if status="$(board_curl -fsS --max-time 3 "$BOARD_URL/firmware" 2>/dev/null)"; then
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
