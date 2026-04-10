#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_DIR="$ROOT_DIR/app/MiniDSP Device Console.app"
RESOURCES_DIR="$APP_DIR/Contents/Resources"
ASAR_PATH="$RESOURCES_DIR/app.asar"
PLIST_PATH="$APP_DIR/Contents/Info.plist"
ASAR_BACKUP_PATH="$RESOURCES_DIR/app.asar.original"
PLIST_BACKUP_PATH="$APP_DIR/Contents/Info.plist.original"

if [[ ! -f "$ASAR_BACKUP_PATH" ]]; then
    echo "missing backup: $ASAR_BACKUP_PATH" >&2
    exit 1
fi

if [[ ! -f "$PLIST_BACKUP_PATH" ]]; then
    echo "missing backup: $PLIST_BACKUP_PATH" >&2
    exit 1
fi

cp "$ASAR_BACKUP_PATH" "$ASAR_PATH"
cp "$PLIST_BACKUP_PATH" "$PLIST_PATH"

if command -v codesign >/dev/null 2>&1; then
    if ! codesign --force --deep --sign - "$APP_DIR" >/dev/null 2>&1; then
        echo "warning: ad-hoc codesign failed during restore" >&2
    fi
fi

echo "Restored original MiniDSP Device Console bundle from backups."
