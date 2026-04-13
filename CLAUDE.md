# minidsp-open

Clean-room MiniDSP controller (not a fork). HTTP API and CLI. Runs on Linux/macOS/Windows AND ESP32-S3 from one Rust codebase. Protocol knowledge from minidsp-rs (credited) + Device Console XML extraction.

When progress or verification status changes, update `PROGRESS.md`. Do not turn this file into a dated progress log.

## Build

```bash
# Desktop
cargo build && cargo test

# ESP32 — `esp` wrapper handles all environment setup
cd app-esp32
esp cargo build
```

**Use `/esp-idf` for general ESP-IDF knowledge** — environment setup, flashing, serial monitoring, common gotchas, DTR/RTS, Python issues.

### ESP32 Flashing (project-specific)

Flash with `esptool.py` (not espflash — it panics on our CH340 boards). The build produces an ELF that must be converted first:

```bash
# 1. Close serial-mcp session
# 2. Convert ELF → bin, then flash all three parts
esp esptool.py --chip esp32s3 elf2image target/xtensa-esp32s3-espidf/debug/minidsp-esp32 -o /tmp/app.bin
esp esptool.py --chip esp32s3 -p /dev/ttyUSB1 -b 460800 write_flash \
  --flash_size 16MB 0x0 bootloader.bin 0x8000 partition-table.bin 0x10000 /tmp/app.bin
# 3. Reopen serial-mcp
```

Bootloader/partition-table binaries are in `target/xtensa-esp32s3-espidf/debug/`. If the port is stuck (Resource busy), `lsof` and `kill` the holding process — you have permission.

## ESP32 Dev Boards

| Serial | Port | Role |
|--------|------|------|
| firmware-board | `/dev/ttyUSB0` | miniDSP 2x4 HD simulator (TinyUSB device) |
| simulator-board | `/dev/ttyUSB1` | minidsp-open firmware target (USB host) |

## Key Files

| File | What |
|------|------|
| `PROJECT.md` | Architecture, protocol reference, device tables, goals |
| `PROGRESS.md` | Task checklist by phase |
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
- **Clean-room.** No copied code. Protocol knowledge from minidsp-rs (credited) + manufacturer XML.
- **MIT license.**

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
