# Firmware Updates (ESP32 OTA)

How the ESP32 firmware gets updated after the first USB flash: over WiFi, from a GitHub release or an uploaded build. The ezBEQ UI/catalogue bundle on the `storage` partition has its own updater; see `docs/ref-esp32-boot.md`.

## Flash layout

`app-esp32/partitions.csv`, 16 MB flash:

| Partition | Offset | Size | Use |
|-----------|--------|------|-----|
| nvs | `0x9000` | `0x6000` | WiFi credentials, settings |
| phy_init | `0xf000` | `0x1000` | RF calibration |
| otadata | `0x10000` | `0x2000` | which app slot boots, and its verify state |
| ota_0 | `0x20000` | `0x300000` | app slot A (3 MB) |
| ota_1 | `0x320000` | `0x300000` | app slot B (3 MB) |
| storage | `0x620000` | `0x9E0000` | LittleFS: ezBEQ UI + catalogue (~9.9 MB) |

A release image is ~2.5 MB and a size-optimised debug image ~2.7 MB (`[profile.dev] opt-level = "s"` in `app-esp32/Cargo.toml`; unoptimised debug builds are 4.7 MB and don't fit a slot).

`nvs` and `phy_init` kept their pre-OTA offsets, so WiFi credentials survive the switch. The storage partition moved, so the first OTA-layout flash rewrites the bundle (`tools/flash-esp32.sh` does that), or the updater downloads it after boot.

Moving a board to this layout takes one USB flash, because OTA can't change the partition table or bootloader. Every flash script erases `otadata`, so the bootloader starts `ota_0`, the slot the script just wrote, even if the board had last OTA-updated into `ota_1`.

## Bootloader compatibility

OTA never updates the bootloader. A board flashed with a bootloader from an ESP-IDF 5.4 build keeps that bootloader through OTA updates, even if the app uses a newer IDF.

Use `tools/flash-esp32.sh --port <serial-port>` for the first move to ESP-IDF 6.x so the bootloader and app come from the same build. Later 6.x builds can go over OTA, provided the partition table and bootloader do not need to change.

## Update paths

Both stream the image into the slot that isn't running. Nothing changes until ESP-IDF's `esp_ota_end()` has validated the whole image (header, segments, appended SHA-256); only then does the boot slot switch. A failed or interrupted update leaves the running firmware untouched.

### Uploaded build

```bash
tools/ota-push.sh --board http://BOARD:5380             # release build, upload, wait for reboot
tools/ota-push.sh --board http://BOARD:5380 --debug     # debug build
curl --data-binary @app.bin "http://BOARD:5380/firmware?sha256=$(shasum -a 256 app.bin | cut -d' ' -f1)"
```

The upload must be the `elf2image` app image (`minidsp-open-esp32-X.Y.Z.bin` from a release), not the ELF and not the `-full` merged image. The board rejects anything that isn't an ESP-IDF app image for an ESP32-S3 (chip id 9), anything bigger than a slot, and anything whose SHA-256 doesn't match the optional `sha256` query parameter (or `X-Firmware-SHA256` header).

Firmware upload and download SHA-256 checks use the pure-Rust `sha2` crate. ESP-IDF still validates the app image through `esp_ota_end()`.

The board also serves a small page at `/update` with status, an upload form, and the GitHub check/install buttons.

### GitHub release

The board checks `https://github.com/Averyy/minidsp-open/releases/latest/download/firmware.json` two minutes after boot and then daily. It only records the result; installing is a deliberate action (`POST /firmware/update` or the `/update` page). Pre-releases are never "latest", so boards don't see them.

`firmware.json` is produced by `tools/release-firmware-manifest.py` in the release workflow:

```json
{
  "version": "0.2.1",
  "chip": "esp32s3",
  "app": {"name": "minidsp-open-esp32-0.2.1.bin", "size": <bytes>, "sha256": "<hex>"},
  "full": {"name": "minidsp-open-esp32-0.2.1-full.bin", "size": <bytes>, "sha256": "<hex>"},
  "releaseNotesUrl": "https://github.com/Averyy/minidsp-open/releases/tag/v0.2.1"
}
```

An install downloads `app` from the same release over HTTPS (ESP-IDF certificate bundle) and checks its size and SHA-256 against the manifest. It only installs a version newer than the running one (semver; a pre-release sorts before its release).

## Rollback

`CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y`. A freshly installed image boots as `pendingVerify`. `ota::start_health_check` confirms it once the board has been up 30 s and is reachable: the HTTP server carrying `/firmware` is listening (`ota::mark_http_ready`), and either STA WiFi is connected or the board has no saved WiFi credentials and is legitimately in SoftAP provisioning mode. If that doesn't happen within 3 minutes, it marks the image invalid and reboots into the previous slot. A crash or watchdog reset before confirmation also lands back on the previous slot.

So a build that breaks WiFi, the HTTP server, or boot rolls itself back, and you can push a fix without touching USB. A build that's reachable but broken in some other way stays installed. Push a fixed build, or install the previous release `.bin` by hand.

## HTTP API

| Method | Path | Notes |
|--------|------|-------|
| `GET` | `/firmware` | `{version, build, runningSlot, slotState, latest?, progress?}` |
| `POST` | `/firmware` | STA mode only; raw app image body; `?sha256=`; `200 {"status":"rebooting"}`, then reboots after ~1.5 s |
| `POST` | `/firmware/check` | `202`; poll `GET /firmware` for `latest`; `409` while a check is pending or during the 30 s cooldown |
| `POST` | `/firmware/update` | `202`; installs the latest release in the background; poll `progress` |
| `GET` | `/update` | HTML page |

`version` is the Cargo package version. `build` is `MINIDSP_BUILD_REV` (`vX.Y.Z` in release builds) or `git describe --always --dirty` for local builds. `slotState` is `valid` or `pendingVerify`. Uploads and installs answer `409` during `/play` playback or while another update runs, and `400` for an unacceptable image.

These endpoints are unauthenticated, like the rest of the board's API: anything on the LAN that can reach port 5380 can install firmware. Treat the board as a trusted-LAN device.

## Catalogue source overrides for development

Release firmware always uses `https://averyy.github.io/minidsp-open` for the UI/catalogue source and ignores any saved `updaterBaseUrl` NVS override. `POST /debug/updater` is available only in STA mode in builds made with `tools/esp-cargo build --features dev-updater-override`. The feature is off by default and is not enabled by CI or release builds. `tools/flash-esp32-with-site.sh` enables it for its temporary local site workflow; when skipping its build/flash steps, the installed firmware must already have the feature. A development override persists across reboot until cleared, but a release build ignores it.

Provisioning mode does not register firmware uploads or the source-override route. WiFi credentials can only be changed through the provisioning form, not through the STA API. No STA page or API caller uses `/api/wifi/connect`; its only caller is that form. If saved credentials fail at startup, the board starts `miniDSP-Open-XXXXXX` automatically. Repeated credential-related disconnects also clear credentials and restart into provisioning. To move to a different network, make the saved network unavailable and restart the board, join its setup AP, and enter the new credentials at `http://192.168.100.1/` (or the captive portal). Other disconnects keep retrying until restart.

Manual and daily firmware checks share one persistent worker, with at most one outstanding check and a 30 s cooldown after completion (including failures).

See [SECURITY.md](../SECURITY.md) for the LAN trust model and unsigned-OTA limits.

## Cutting a release

1. Bump `version` in the root `Cargo.toml` (`[workspace.package]`) and `app-esp32/Cargo.toml`. Keep it above `0.1.12`: ezBEQ reads `minidsp -V` (see `docs/ref-compatibility.md`).
2. Pin `averyy/usb_host_uac2` in `app-esp32/Cargo.toml` to the exact ESP Component Registry version the firmware was tested against (currently `0.1.3`). Build in registry mode with `tools/esp-cargo build --release` and include the updated `app-esp32/components_esp32s3.lock`; both workflows cache against that lockfile.
3. Optionally run the Release workflow by hand (workflow_dispatch) for a dry run that uploads artifacts without publishing.
4. Tag `vX.Y.Z` and push the tag. The workflow checks the tag against both Cargo versions, builds the firmware and desktop binaries, and publishes the release with `firmware.json` and `SHA256SUMS`.
