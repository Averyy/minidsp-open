# minidsp-open

Original MiniDSP controller implementation (not a fork). HTTP API and CLI. Runs on Linux/macOS/Windows AND ESP32-S3 from one Rust codebase. Protocol knowledge from minidsp-rs (credited) + Device Console XML extraction.

Record user-visible changes in `CHANGELOG.md` under Unreleased (Keep a Changelog). Keep `PROGRESS.md` as current status and open items only, never a dated log.

## Build

```bash
# Desktop
cargo build && cargo test

# ESP32 — ESP-IDF v6.0.3; repo wrapper handles espup environment setup
tools/esp-cargo build
```

Do not build with an activated ESP-IDF environment (`. export.sh`, or the personal `esp` wrapper with `--idf`): its environment variables override the pinned version. `tools/esp-cargo` clears those variables so esp-idf-sys installs the pinned IDF and matching build tools. esp-idf-sys 0.38 doesn't watch `component_dirs`, so the `ESP_IDF_GLOB_UAC2_*` entries in `app-esp32/.cargo/config.toml` make Cargo rebuild after a `~/code/esp-uac2-host/components/usb_host_uac2` edit; keep them in step with the component's build inputs. Plain `esp cargo …` works too: the wrapper sees the pin and activates no IDF.

**Use `/esp-idf` for general ESP-IDF knowledge** — environment setup, flashing, serial monitoring, common gotchas, DTR/RTS, Python issues.

### ESP32 Flashing (project-specific)

Use `tools/flash-esp32.sh` as the default local flashing workflow. It builds the current ezBEQ bundle locally, turns it into a LittleFS image for the `storage` partition, flashes that image alongside the firmware, and can optionally verify the board's `/version.txt` after boot via `--board-url http://BOARD:5380`.

This is the correct dev workflow even while the published ezBEQ site is stale because the board comes up with a real stored bundle immediately after flash instead of depending on an immediate post-boot download.

For the first move from ESP-IDF 5.4 to 6.x, use `tools/flash-esp32.sh` to update the bootloader; OTA never updates it. Once a board has the 6.x bootloader and runs OTA-capable firmware, push new builds over WiFi instead: `tools/ota-push.sh --board http://BOARD:5380` builds, uploads to `POST /firmware`, and waits for the reboot. The board verifies the image, and rolls back by itself if the new firmware never gets back on the network. USB flashing is only needed for partition-table/bootloader changes or a bricked board. Firmware updates are documented in `docs/ref-firmware-updates.md`.

If you need the low-level primitive directly, flash with `tools/esptool` (not espflash — it panics on our CH340 boards). The build produces an ELF that must be converted first. Offsets come from `app-esp32/partitions.csv` (app in `ota_0` at `0x20000`); erasing `otadata` makes the bootloader start `ota_0`:

```bash
# 1. Close serial-mcp session
# 2. Convert ELF → bin, erase otadata, then flash all three parts
tools/esptool --chip esp32s3 elf2image app-esp32/target/xtensa-esp32s3-espidf/debug/minidsp-esp32 -o /tmp/app.bin
tools/esptool --chip esp32s3 -p "$PORT" -b 460800 erase-region 0x10000 0x2000
tools/esptool --chip esp32s3 -p "$PORT" -b 460800 write-flash \
  --flash-size 16MB \
  0x0 app-esp32/target/xtensa-esp32s3-espidf/debug/bootloader.bin \
  0x8000 app-esp32/target/xtensa-esp32s3-espidf/debug/partition-table.bin \
  0x20000 /tmp/app.bin
# 3. Reopen serial-mcp
```

Bootloader/partition-table binaries are in `app-esp32/target/xtensa-esp32s3-espidf/debug/`. If the port is stuck (Resource busy), `lsof` and `kill` the holding process — you have permission.

`tools/flash-esp32-with-site.sh` remains available as the explicit updater-path fallback for testing `/debug/updater` against a temporary local site.

Board serials, USB ports, and LAN addresses for the maintainer's hardware live in the gitignored `CLAUDE.local.md`.

## Key Files

| File | What |
|------|------|
| `PROJECT.md` | Architecture, protocol reference, device tables, goals |
| `CHANGELOG.md` | User-visible changes and development milestones |
| `PROGRESS.md` | Current status, verification limits, and open work |
| `docs/ref-devices.md` | Device registry — hw_ids, DSP IDs, USB PIDs, capabilities |
| `docs/ref-compatibility.md` | External compatibility contract — CLI, HTTP, WebSocket, TCP, UDP |
| `docs/ref-address-tables.md` | DSP register addresses for all 7 SHARC structures |
| `docs/ref-esp32-boot.md` | ESP32 boot sequence, WiFi provisioning (captive portal requirements), data partition |
| `docs/ref-esp32-logging.md` | Ring buffer logging, /debug endpoints |
| `app/MiniDSP Device Console.app/` | Official app — source of truth for device definitions and addresses |
| `~/code/esp-uac2-host/simulators/minidsp-2x4hd/` | 2x4 HD simulator firmware (separate repo) |

## Critical Rules

- **XML is source of truth for addresses.** SHARC: Device Console `DSP_default_parameters.xml`. Legacy: SigmaStudio `config.xml`.
- **Drop-in replacement for minidsp-rs.** Compatibility contract: `docs/ref-compatibility.md`. Never break existing clients.
- **No UI on desktop.** API/CLI only. ESP32 serves ezBEQ React UI from flash.
- **All devices from day one.** 13 devices full PEQ, 3 more basic support.
- **Dual target.** `cfg(target_os = "espidf")` for platform splits. Protocol crate is shared.
- **Desktop: tokio + axum.** ESP32: blocking std on ESP-IDF, EspHttpServer, esp-idf-sys FFI for USB host.
- **Original implementation.** Protocol knowledge from minidsp-rs (Apache-2.0, credited in `NOTICE`). Vendor XML files are included for interoperability under the provenance notices; generated tables and reset payloads derive from vendor data.
- **MIT license for original code.** Vendor data is excluded; see `NOTICE` and the asset READMEs.

## Protocol Quick Reference

- USB HID: VID `0x2752`, 65-byte reports (report ID 0x00 + 64 bytes, 0xFF padded)
- TCP: port 5333, length-prefixed frames with checksum
- UDP discovery: port 3999, broadcast every 1s
- HTTP API: port 5380 (desktop), port 80 SoftAP / 5380 STA (ESP32)
- Frame: `[length] [payload] [checksum]` — length is self-inclusive, checksum = sum(all bytes) & 0xFF
- Dialect: old devices use 2-byte addr + fixed-point, new devices use 3-byte addr + float32
- Flash memory at page 0xFF (preset, volume, mute, source, tokens, display, DRE, serial)

## Device Console Reference

Downloaded from:
- macOS: `https://phpstack-793508-3229840.cloudwaysapps.com/deviceconsole/latest-macmx`
- Windows: `https://phpstack-793508-3229840.cloudwaysapps.com/deviceconsole/latest-win`

Address tables extracted from: `Contents/Resources/device_support_package/DSP_Structures/{dsp_id}/DSP_default_parameters.xml`
