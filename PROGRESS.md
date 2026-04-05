# minidsp-open Progress

## Phase 0: Project Setup
- [x] Cargo workspace (protocol, minidsp, daemon, app-desktop, app-esp32)
- [ ] CI: GitHub Actions for Linux (x86_64, armhf, arm64), macOS (x86_64, aarch64), Windows
- [ ] CI: ESP32 cross-compile
- [x] License, README, CLAUDE.md finalized
- [x] `docs/ref-compatibility.md` finalized as canonical external contract

## Phase 1a: Protocol Crate (pure Rust, no platform deps)
- [x] Frame encoding: length-prefixed, checksum, 0xFF padding
- [x] HID packet wrapping (65-byte reports, report ID 0x00)
- [x] Command builders: WriteDSP, ReadFloats, WriteBiquad, SetVolume, SetMute, SetConfig, SetSource, ReadHardwareId, WriteFlash, ReadFlash
- [x] New commands: SetDisplayBrightness, SetDisplayIdleTime, SetDRE, CopyPreset, DiracBypass, DiracDelayGain, FirLoadStart, FirLoadData, ReloadDspParam, LogControl
- [x] Response parsing (OK, error codes, preset complete, reset complete)
- [x] Dialect layer: old (2-byte addr, 5.23 fixed-point) vs new (3-byte addr, float32)
- [x] Biquad stability validation (warn by default)
- [x] Master status parsing (EEPROM 0xFFD8)
- [x] Flash memory map helpers (preset, volume, mute, source, tokens, display, DRE, serial, volume offsets)

## Phase 1b: Device Definitions
- [x] SHARC DSP 100 (2x4 HD / Flex) — address table, channel layout, PEQ/crossover/delay/compressor/FIR addresses
- [x] SHARC DSP 101 (DDRC-24 / Flex DL) — + Dirac block
- [x] SHARC DSP 105 (SHD / SHD Studio / SHD Power) — + Dirac block
- [x] SHARC DSP 110 (Flex Eight) — 2 in / 8 out, FIR on input, no Dirac
- [x] SHARC DSP 111 (Flex Eight DL) — + Dirac block
- [x] SHARC DSP 113 (Flex HT/HTx old FW) — 8 in / 8 out, bass management, Dirac capable
- [x] SHARC DSP 115 (Flex HT/HTx new FW) — + input PEQ
- [ ] Legacy config.xml parser (SigmaStudio format)
- [x] Legacy 4x10 HD (old dialect) — probe entry, DspKnown tier
- [x] Legacy 10x10 HD (old dialect) — probe entry, DspKnown tier
- [x] Legacy NanoDigi 2x8 (old dialect) — probe entry, DspKnown tier
- [x] Legacy miniSHARC 4x8 — probe entry, DspKnown tier
- [x] Legacy DDRC-88BM — probe entry, DspKnown tier
- [x] Legacy C-DSP 8x12 v2 — probe entry, DspKnown tier
- [x] Basic support stubs: 2x4 non-HD, nanoAVR, nanoAVR HDA (volume/mute/preset only)
- [x] Two-stage device identification (dsp_version → address table, hw_id → profile)
- [x] Capability tiers (Universal / DspKnown / Full)
- [x] Per-device delay limits, sample rates, source mappings

## Phase 1c: Transport & Connection
- [x] Transport trait
- [x] USB HID transport (hidapi, desktop)
- [x] TCP transport (tokio, shared)
- [x] Connection lifecycle: connect, health check, disconnect
- [ ] Auto-reconnect with exponential backoff (100ms → 10s)
- [ ] USB hotplug detection + cleanup
- [ ] Multi-device support (per-device connection isolation)
- [x] Legacy USB VID 0x04D8 scanning

## Phase 1d: Command Queue
- [x] Priority queue (LOW/NORMAL/HIGH/CRITICAL)
- [x] Batch execution (contiguous, no interleaving with level reads)
- [x] Level polling pause during batch operations
- [x] Post-preset-switch gating (DeviceBusy state, poll until confirmed)
- [ ] Per-device queues (no global lock)
- [x] Timeout handling (10s default)
- [ ] "Busy" response for simultaneous connections

## Phase 1e: Config Read-Back & Measurement Mode
- [ ] `read_full_config()` — ReadFloats across all DSP addresses for current device
- [ ] Config struct (JSON-serializable full device state)
- [ ] Measurement mode: save state → flatten PEQ/bypass → restore
- [ ] Modification tokens (read/write EEPROM 0xFFC8)

## Phase 1f: Partial Write Recovery
- [ ] Bypass-first write ordering for batch PEQ writes
- [ ] Pending batch saved in connection state
- [ ] Idempotent retry on reconnect
- [ ] Optional post-write verification via ReadFloats

## Phase 1g: Network Services (Desktop)
- [x] UDP discovery broadcast (port 3999, 1s interval)
- [ ] mDNS service advertisement (`_minidsp._tcp`, `mdns-sd` crate)
- [ ] mDNS hostname generation from serial number
- [x] TCP bridge server (port 5333, length-prefixed frames)
- [x] TCP bridge: decode and queue through command queue
- [x] TCP bridge: pass through unrecognized commands as raw frames
- [ ] TCP bridge: WI-DG compat (64-byte chunking, non-zero padding handling)
- [ ] TCP bridge: PWR-ICE compat (70-byte fixed frames)
- [ ] Unix socket `/tmp/minidsp.sock` (permissions 0o766, same HTTP API as TCP listener)

## Phase 1h: HTTP API (Desktop — axum)
- [x] `GET /devices` — list detected devices
- [x] `GET /devices/:id` — master status
- [x] `POST /devices/:id` — set master status
- [x] `POST /devices/:id/config` — full config write
- [ ] `GET /devices/:id/config` — full config read-back
- [x] `POST /devices/:id/volume/:direction` — compatibility route for `up`, `down`, `toggle_mute`
- [x] `POST /devices/:id/source/:source` — compatibility route
- [x] `POST /devices/:id/preset/:preset` — compatibility route
- [ ] `POST /devices/:id/measurement-mode` + `/restore`
- [ ] `GET/POST /devices/:id/display`
- [ ] `GET/POST /devices/:id/dre`
- [ ] `POST /devices/:id/preset/copy`
- [ ] `GET /devices/:id/tokens`
- [ ] `GET /openapi.json`
- [x] `GET /llms.txt`
- [ ] Compatibility state/levels WebSocket on `/devices/:id` with `poll=true` / `levels=true` per `docs/ref-compatibility.md`
- [ ] Raw frame bridge on `/devices/:id/ws`
- [ ] CORS headers

## Phase 1i: CLI
- [ ] Drop-in minidsp-rs command compatibility
- [ ] `--force-kind`, `--force-device`, `--force-dsp` flags
- [ ] Daemon mode (`minidsp server`)
- [ ] CLI auto-discovery order: Unix socket → USB (per `docs/ref-compatibility.md`)
- [ ] All per-channel commands: gain, delay, mute, peq, crossover, fir, compressor
- [ ] Global commands: volume, source, preset, dirac, dre, display
- [ ] Negative coefficient parsing fix (minidsp-rs #625)
- [ ] Accept minidsp-rs config TOML format (per `docs/ref-compatibility.md`)

## Phase 1j: ESP32 — USB Host HID
- [ ] `esp-idf-sys` FFI to `usb_host_hid` C API (~200-300 lines unsafe)
- [ ] Device enumeration (VID 0x2752)
- [ ] 65-byte HID report read/write
- [ ] Hotplug detection
- [ ] VBUS drive (GPIO high for MiniDSP USB enable)

## Phase 1k: ESP32 — HTTP API (picoserve)
- [ ] picoserve route wiring (same handler logic as axum)
- [ ] serde-json-core responses (fixed buffer sizing)
- [ ] WebSocket support (picoserve `ws` feature)
- [ ] Connection limit management (~4-8 concurrent)

## Phase 1l: ESP32 — WiFi & Provisioning
- [ ] SoftAP + captive portal (SSID `minidsp-setup`)
- [ ] WiFi credential storage (NVS)
- [ ] WiFi reconnection with backoff (1s base, 2x, 60s cap)
- [ ] Reason code handling (AUTH_FAIL → re-provision, BEACON_TIMEOUT → aggressive retry)
- [ ] mDNS via `EspMdns` from `esp-idf-svc`
- [ ] `POST /api/system/reprovision`

## Phase 1m: ESP32 — Diagnostics
- [ ] Custom `log::Log` implementation (UART + 4KB ring buffer)
- [ ] `GET /debug/log` — ring buffer snapshot
- [ ] `GET /debug/info` — JSON system diagnostics (heap, WiFi, USB, uptime)
- [ ] `POST /debug/level` — runtime log level change
- [ ] `GET /api/system/status` — data partition state

## Phase 1n: Release & Testing
- [ ] Test: minidsp-rs CLI compatibility (all commands produce same behavior)
- [ ] Test: `minidsp -o jsonline` output remains parseable by existing consumers
- [ ] Test: `minidsp probe` output preserves upstream line format
- [ ] Test: `minidsp -f <file>` preserves file order exactly
- [ ] Test: `--force-kind` compatibility behavior
- [ ] Test: convenience compatibility routes (`/volume/:direction`, `/source/:source`, `/preset/:preset`)
- [ ] Test: compatibility state/levels WebSocket on `/devices/:id`
- [ ] Test: raw frame bridge on `/devices/:id/ws`
- [ ] Test: Device Console coexistence (TCP bridge + UDP discovery)
- [ ] Test: ezBEQ client compatibility (PEQ batch load)
- [ ] Test: multi-device
- [ ] Test: reconnect after USB unplug/replug
- [ ] Test: ESP32 end-to-end (WiFi → USB HID → MiniDSP)
- [ ] Device-family matrix: 2x4 HD / Flex / DDRC-24 / Flex DL
- [ ] Device-family matrix: SHD / SHD Studio / SHD Power
- [ ] Device-family matrix: Flex Eight / Flex Eight DL
- [ ] Device-family matrix: Flex HT / Flex HTx on DSP 113
- [ ] Device-family matrix: Flex HT / Flex HTx on DSP 115
- [ ] Device-family matrix: old-dialect legacy devices (4x10 HD, 10x10 HD, NanoDigi 2x8)
- [ ] Device-family matrix: legacy new-dialect devices (miniSHARC 4x8, DDRC-88BM, C-DSP 8x12 v2)
- [ ] For each device-family matrix row: probe, status read, preset switch, source switch, gain/mute, PEQ write, compatibility WS, reconnect
- [ ] Release artifacts: .deb (armhf, arm64, amd64), macOS (x86_64, aarch64), Windows .exe/.msi
- [ ] .deb includes: udev rules, systemd unit, default config
- [ ] ESP32 .bin firmware
- [ ] README with migration guide from minidsp-rs

---

## Phase 2: ezBEQ on ESP32

- [ ] GitHub Action: catalogue index pipeline (~80 lines Python)
- [ ] Data partition: LittleFS setup (~5 MB)
- [ ] Boot sequence: manifest check → download UI + catalogue → serve
- [ ] Auto-update from GitHub Pages (background, non-blocking)
- [ ] ezBEQ backend API: entry ID lookup (ephemeral + stable hash)
- [ ] ezBEQ backend API: biquad write to MiniDSP
- [ ] ezBEQ backend API: WebSocket catalogue browsing
- [ ] Serve ezBEQ React UI static files from flash
- [ ] Test: ezBEQ end-to-end on ESP32

---

## Separate Projects (after v1)

- [ ] 12V trigger: GPIO + MOSFET, `POST /trigger/on|off`, `GET /trigger/status`
- [ ] MQTT discovery + HA entities
- [ ] SSE live log streaming (`GET /debug/events`)
- [ ] Syslog UDP (RFC 5424)

---

## Stretch / Someday

- [ ] Preset import/export (Device Console XML format)
- [ ] Per-source volume offsets
- [ ] Template/offline device (in-memory simulation)
- [ ] MCP server (`--mcp` flag)
- [ ] OTA firmware updates (ESP32)
- [ ] Custom PCB (USB stick form factor)
- [ ] WiSA speaker management
