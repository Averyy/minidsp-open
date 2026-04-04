# minidsp-open

Clean-room MiniDSP controller (not a fork). HTTP API and CLI. Runs on Linux/macOS/Windows AND ESP32-S3 from one Rust codebase. Protocol knowledge from minidsp-rs (credited) + Device Console XML extraction.

## Project State

Planning/research complete. Not yet coding. All protocol details, device definitions, and address tables extracted and verified.

**Phase 1:** Desktop CLI/API + ESP32 core (WiFi, USB HID, same API).
**Phase 2:** ezBEQ on ESP32 (catalogue pipeline, UI serving, entry ID lookup). Built after phase 1 is verified working.
**Separate projects (after v1):** 12V trigger, MQTT, SSE log streaming, syslog.

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

## Goal: Fix Everything Wrong With minidsp-rs

minidsp-rs is an upstream protocol reference (one maintainer, 10 stale dependabot PRs, 9 unresolved device requests). We fix all of it:

| minidsp-rs issue | Problem | Our fix |
|-----------------|---------|---------|
| #765 | Device unreachable after standby/wake | Auto-reconnect with health checks, exponential backoff |
| #511 | Daemon doesn't find device after boot | Retry USB enumeration, don't fail on first attempt |
| #144 | Stale device after USB unplug | Proper hotplug lifecycle, clean up and notify clients |
| #662 | Can't run multiple devices on one host | Per-device connection isolation, no global lock |
| #699 | Simultaneous connections corrupt state | Command queue with arbitration, return "busy" |
| #570 | Level streaming blocks preset changes | Interleave commands properly |
| #557 | Delay hard limit too low for some devices | Per-device limits from device definition |
| #663 | No standby/power control | 12V trigger via ESP32 GPIO (FUTURE — see `docs/FUTURE-12v-trigger.md`) |
| #566 | Flex Eight not supported | Full definition with addresses from Device Console XML |
| #698 | Flex HT not supported | Full definition with addresses from Device Console XML |
| #657 | SHD Power not supported | Add hw_id 21 + 5-source mapping (trivial) |
| #764 | CDSP 8x12DL not supported | Pending device owner probe |
| #701 | HTx new firmware breaks existing | DSP 115 definition with input PEQ addresses |

Also: MiniDSP's WI-DG WiFi module is $75 for a MediaTek SoC running OpenWrt that just bridges TCP↔USB HID. Our ESP32-S3 target does the same thing (and more — full HTTP API, plus ezBEQ UI in phase 2) on a $10 dev board.

Plus features minidsp-rs never had: config read-back, measurement mode, display control, DRE, modification tokens, ESP32 target, ezBEQ integration, `llms.txt` for AI discoverability. Preset import/export, per-source volume offsets deferred to post-v1.

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
