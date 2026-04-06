# minidsp-open

Clean-room MiniDSP controller (not a fork). HTTP API and CLI. Runs on Linux/macOS/Windows AND ESP32-S3 from one Rust codebase. Protocol knowledge from minidsp-rs (credited) + Device Console XML extraction.

## Project State

Desktop Phase 1 complete. 83 tests passing, zero warnings. ESP32 firmware (IDF v5.4) boots, connects to MiniDSP simulator via USB Host HID, runs WiFi SoftAP, serves HTTP API. Blocking std architecture (no embassy). Flashed and tested on hardware — stable, 221KB heap free.

## Build

```bash
# Desktop (all crates)
cargo build
cargo test

# ESP32 (separate toolchain)
cd app-esp32
# Requires: espup, ldproxy, ESP-IDF v5.4 with Python 3.14 venv via uv
cargo build  # uses rust-toolchain.toml (esp) and .cargo/config.toml
```

## ESP32 Flashing

Both ESP32-S3 DevKitC-1 boards have two USB-C ports:
- **Right (UART):** CH340 bridge (VID 0x1A86). Used for flashing AND serial monitor. Shows as `/dev/cu.usbmodem*`.
- **Left (USB OTG):** Connected to other ESP32 for device-to-device USB. NOT used for flashing.

Flash sequence — use `idf.py`, NOT `espflash`:
```bash
# 1. Close serial-mcp session on the target board (exclusive port lock)
# 2. Flash via idf.py (CH340 auto-resets into download mode via DTR/RTS — no boot button needed)
idf.py -p /dev/cu.usbmodem<TARGET> flash
# 3. Reopen serial-mcp after flash completes
```

The port name doesn't change between flashes. Both boards can be flashed independently while their OTG ports remain connected.

## ESP32 Dev Boards

| Serial | Location | Role |
|--------|----------|------|
| firmware-board | 0-1 | miniDSP 2x4 HD simulator (TinyUSB device) |
| simulator-board | 1-1 | minidsp-open firmware target (USB host) |

## Key Files

| File | What |
|------|------|
| `PROJECT.md` | Scope of work — architecture, protocol reference, device tables, goals |
| `PROGRESS.md` | Checklist of all tasks by phase — tick off as we go |
| `docs/ref-devices.md` | Complete device registry — all hw_ids, DSP IDs, sources, USB PIDs, capabilities |
| `docs/ref-compatibility.md` | Canonical external compatibility contract — CLI, HTTP, WebSocket, TCP bridge, UDP discovery |
| `docs/ref-address-tables.md` | Every DSP register address for all 7 SHARC structures (verified against minidsp-rs + Device Console XML) |
| `docs/ref-legacy-devices.md` | Legacy ADAU1445 device analysis — address tables, dialect, config.xml source data |
| `docs/ref-ezbeq.md` | ezBEQ integration plan — protocol, API compat, ESP32 strategy, catalogue pipeline |
| `docs/ref-command-pacing.md` | Command queue architecture — USB HID timing, priority scheme, batch operations |
| `docs/ref-esp32-boot.md` | ESP32 boot sequence, WiFi reconnection, provisioning, data partition state machine |
| `docs/ref-esp32-logging.md` | ESP32 diagnostics — ring buffer logging, /debug endpoints, system info |
| `app/MiniDSP Device Console.app/` | Extracted official app — source of truth for device definitions and addresses |
| `app/app-extracted/` | Decompiled Device Console JS (minified) — protocol details, command formats |
| `app/apk-extracted/` | Decompiled miniDSP Controller Android app — hw_id source maps, flash addresses, legacy devices |
| `docs/shared-understanding.md` | How this project fits with open-sub-optimizer and dirac-sparr |
| `docs/FUTURE-12v-trigger.md` | 12V trigger — GPIO power control, hardware design, API, separate project after v1 |
| `docs/FUTURE-pwr-ice.md` | PWR-ICE125/250 deferred support — protocol details, what we need from users |

## Critical Rules

- **XML is source of truth for addresses.** SHARC: Device Console `DSP_default_parameters.xml`. Legacy ADAU1445: SigmaStudio `config.xml` exports. Both are manufacturer device data, not minidsp-rs code.
- **Drop-in replacement for minidsp-rs.** The canonical compatibility contract is `docs/ref-compatibility.md`. Existing clients (Home Assistant, ezBEQ, mobile apps, CLI scripts) must work without changes. We only add, never remove or break. This includes: accepting `--force-kind`, not rejecting previously-accepted input (biquad coefficients), and passing through unknown TCP bridge commands.
- **No UI on desktop.** Desktop target is API/CLI only. ESP32 serves ezBEQ's unmodified React UI from flash (auto-updated). Ship `llms.txt` at webapp root for AI/LLM discoverability.
- **All devices from day one.** Full PEQ support for 13 devices (7 SHARC + 6 legacy). Basic support (volume/mute/preset) for 3 more (2x4 non-HD, nanoAVR, nanoAVR HDA). See `docs/ref-legacy-devices.md`.
- **Dual target: Linux + ESP32-S3.** Use `cfg(target_os = "espidf")` for platform splits. Protocol crate is shared, transport and HTTP server are platform-specific.
- **Linux: tokio + axum.** ESP32: embassy-executor on ESP-IDF (`arch-std`) + picoserve. Business logic shared, HTTP handler wiring is platform-specific.
- **USB HID on ESP32** has no safe Rust wrappers. Use `esp-idf-sys` unsafe FFI to the C USB host API.
- **No MCP server.** Skipped — zero demand. HTTP API + CLI already usable by LLMs. Ship `llms.txt` instead. Maybe add later as `--mcp` flag if requested.
- **Clean-room implementation.** All code written from scratch. Protocol knowledge referenced from minidsp-rs (credited). Source data from manufacturer (Device Console XML, SigmaStudio config.xml). We do NOT copy minidsp-rs generated code or their codegen pipeline.
- **MIT license.** Credit minidsp-rs in README as protocol reference and inspiration.

## Tech Stack

- **Language:** Rust
- **Not a fork.** Clean-room implementation. Protocol knowledge from minidsp-rs (credited) + Device Console XML extraction
- **Linux:** tokio + axum + hidapi
- **ESP32-S3:** std-on-ESP-IDF. embassy-executor (`arch-std`) + picoserve + esp-idf-svc + esp-idf-sys FFI for USB host
- **JSON:** serde_json (Linux), serde-json-core (ESP32, chosen for heap efficiency)
- **Protocol crate:** Pure Rust, no_std, no platform deps, compiles everywhere

## Protocol Quick Reference

- USB HID: VID `0x2752`, 65-byte reports (report ID 0x00 + 64 bytes, 0xFF padded)
- TCP: port 5333, length-prefixed frames with checksum
- UDP discovery: port 3999, broadcast every 1s
- HTTP API: port 5380 (Linux daemon), configurable on ESP32
- Frame: `[length] [payload] [checksum]` — length is self-inclusive, checksum = sum(all bytes) & 0xFF
- Dialect: old devices use 2-byte addr + fixed-point, new devices use 3-byte addr + float32
- Flash memory at page 0xFF (preset, volume, mute, source, tokens, display, DRE, serial)

## Device Console Reference

Downloaded from:
- macOS: `https://phpstack-793508-3229840.cloudwaysapps.com/deviceconsole/latest-macmx`
- Windows: `https://phpstack-793508-3229840.cloudwaysapps.com/deviceconsole/latest-win`

Address tables extracted from: `Contents/Resources/device_support_package/DSP_Structures/{dsp_id}/DSP_default_parameters.xml`
