#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_DIR="$ROOT_DIR/app/MiniDSP Device Console.app"
RESOURCES_DIR="$APP_DIR/Contents/Resources"
ASAR_PATH="$RESOURCES_DIR/app.asar"
EXTRACTED_DIR="$ROOT_DIR/app/app-extracted"
PLIST_PATH="$APP_DIR/Contents/Info.plist"
ASAR_BACKUP_PATH="$RESOURCES_DIR/app.asar.original"
PLIST_BACKUP_PATH="$APP_DIR/Contents/Info.plist.original"

if [[ ! -d "$APP_DIR" ]]; then
    echo "missing app bundle: $APP_DIR" >&2
    exit 1
fi

if [[ ! -d "$EXTRACTED_DIR" ]]; then
    echo "missing extracted app tree: $EXTRACTED_DIR" >&2
    exit 1
fi

if ! command -v npx >/dev/null 2>&1; then
    echo "npx is required to repack app.asar" >&2
    exit 1
fi

if [[ ! -f "$ASAR_BACKUP_PATH" ]]; then
    cp "$ASAR_PATH" "$ASAR_BACKUP_PATH"
fi

if [[ ! -f "$PLIST_BACKUP_PATH" ]]; then
    cp "$PLIST_PATH" "$PLIST_BACKUP_PATH"
fi

rm -f "$ASAR_PATH"
npx --yes asar pack "$EXTRACTED_DIR" "$ASAR_PATH"

ASAR_HASH="$(shasum -a 256 "$ASAR_PATH" | awk '{print $1}')"
if /usr/libexec/PlistBuddy -c "Print :ElectronAsarIntegrity:Resources/app.asar:hash" "$PLIST_PATH" >/dev/null 2>&1; then
    /usr/libexec/PlistBuddy -c "Set :ElectronAsarIntegrity:Resources/app.asar:hash $ASAR_HASH" "$PLIST_PATH"
fi

if command -v codesign >/dev/null 2>&1; then
    if ! codesign --force --deep --sign - "$APP_DIR" >/dev/null 2>&1; then
        echo "warning: ad-hoc codesign failed; the app may need to be launched from Terminal" >&2
    fi
fi

cat <<EOF
Repacked MiniDSP Device Console with HID logging support.

Default raw HID log path:
  /tmp/minidsp-device-console-hid.jsonl

Launch it normally:
  open -na '$APP_DIR'

Optional override:
  MINIDSP_DC_HID_LOG=/path/to/custom.jsonl open -na '$APP_DIR'

Each log line is JSON with:
  dir=open|tx|tx-result|rx|rx-error|tx-error|close
  device_args=[ constructor args, usually the HID path ]
  len=<byte count when available>
  hex="<space-separated hex bytes>"

Original files were backed up to:
  $ASAR_BACKUP_PATH
  $PLIST_BACKUP_PATH
EOF
