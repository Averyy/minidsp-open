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
- [x] Master status parsing (EEPROM 0xFFD8) — byte order verified against simulator
- [x] Flash memory map helpers (preset, volume, mute, source, tokens, display, DRE, serial, volume offsets)

## Phase 1b: Device Definitions
- [x] SHARC DSP 100 (2x4 HD / Flex) — address table, channel layout, PEQ/crossover/delay/compressor/FIR addresses
- [x] SHARC DSP 101 (DDRC-24 / Flex DL) — + Dirac block
- [x] SHARC DSP 105 (SHD / SHD Studio / SHD Power) — + Dirac block
- [x] SHARC DSP 110 (Flex Eight) — 2 in / 8 out, FIR on input, no Dirac
- [x] SHARC DSP 111 (Flex Eight DL) — + Dirac block
- [x] SHARC DSP 113 (Flex HT/HTx old FW) — 8 in / 8 out, bass management, Dirac capable
- [x] SHARC DSP 115 (Flex HT/HTx new FW) — + input PEQ (mute/gain/polarity marked None — not in XML)
- [ ] Legacy config.xml parser (SigmaStudio format) — BLOCKED: need to write XML parser for address extraction. Legacy devices work at DspKnown tier (volume/mute/source/preset).
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
- [x] Transport trait (send_frame/recv_frame — transport handles wire encoding)
- [x] USB HID transport (hidapi, desktop, spawn_blocking for sync HID API)
- [x] TCP transport (tokio, frame reassembly with persistent read buffer)
- [x] Connection lifecycle: connect, health check, disconnect
- [x] Auto-reconnect with exponential backoff (100ms → 10s) — reconnect module with tests
- [x] USB hotplug detection + cleanup — polling-based watch_hotplug() with events
- [x] Multi-device support (per-device connection isolation) — daemon state HashMap, per-device queue
- [x] Legacy USB VID 0x04D8 scanning

## Phase 1d: Command Queue
- [x] Priority queue (LOW/NORMAL/HIGH/CRITICAL)
- [x] Batch execution (contiguous, no interleaving with level reads)
- [x] Level polling pause during batch operations
- [x] Post-preset-switch gating (DeviceBusy state, poll until confirmed)
- [x] Per-device queues (no global lock) — each MiniDspClient has its own CommandQueue
- [x] Timeout handling (10s default)
- [x] "Busy" response for simultaneous connections — queue depth limit (64), returns 503

## Phase 1e: Config Read-Back & Measurement Mode
- [x] `read_full_config()` — ReadFloats across all DSP addresses (GET /devices/:id/config)
- [x] Config struct (JSON-serializable: DeviceConfig, InputChannelConfig, OutputChannelConfig, BiquadCoeffs, CompressorConfig)
- [x] Measurement mode: save state → bypass all PEQ → restore (enter_measurement_mode / restore_measurement_mode)
- [x] Modification tokens (read/write EEPROM 0xFFC8) — GET /devices/:id/tokens

## Phase 1f: Partial Write Recovery
- [x] Bypass-first write ordering for batch PEQ writes (write_peq_batch: bypass all → write coeffs → set final bypass)
- [x] Pending batch saved in connection state (saved_peq Mutex in MiniDspClient)
- [ ] Idempotent retry on reconnect — DEFERRED: requires persisting pending state across reconnection. Log to PROGRESS.
- [x] Optional post-write verification via ReadFloats (read_full_config can verify after write)

## Phase 1g: Network Services (Desktop)
- [x] UDP discovery broadcast (port 3999, 1s interval, broadcast + multicast)
- [x] mDNS service advertisement (`_minidsp._tcp`, `mdns-sd` crate)
- [x] mDNS hostname generation from serial number (minidsp-{serial}.local)
- [x] TCP bridge server (port 5333, length-prefixed frames, persistent read buffer)
- [x] TCP bridge: decode and queue through command queue (raw passthrough via execute_raw)
- [x] TCP bridge: pass through unrecognized commands as raw frames
- [x] TCP bridge: WI-DG compat (64-byte chunking, auto-detection on first read)
- [ ] TCP bridge: PWR-ICE compat (70-byte fixed frames) — DEFERRED: need device owner to test. PWR-ICE uses LAN-only protocol.
- [x] Unix socket `/tmp/minidsp.sock` (permissions 0o766, same HTTP API as TCP listener)

## Phase 1h: HTTP API (Desktop — axum)
- [x] `GET /devices` — list detected devices
- [x] `GET /devices/:id` — master status (JSON) OR WebSocket upgrade for state/levels streaming
- [x] `POST /devices/:id` — set master status
- [x] `POST /devices/:id/config` — full config write (inputs + outputs PEQ, master status)
- [x] `GET /devices/:id/config` — full config read-back (output PEQ coefficients)
- [x] `POST /devices/:id/volume/:direction` — compatibility route for `up`, `down`, `toggle_mute`
- [x] `POST /devices/:id/source/:source` — compatibility route (lowercase source names)
- [x] `POST /devices/:id/preset/:preset` — compatibility route
- [x] `POST /devices/:id/measurement-mode` + `/restore` — save PEQ state, bypass all, restore
- [x] `GET/POST /devices/:id/display` — brightness and idle time
- [x] `GET/POST /devices/:id/dre` — DRE enable/disable
- [x] `POST /devices/:id/preset/copy` — copy preset to flash
- [x] `GET /devices/:id/tokens` — modification tokens
- [x] `GET /openapi.json` — OpenAPI 3.0 spec
- [x] `GET /llms.txt` — AI discoverability
- [x] Compatibility state/levels WebSocket on `/devices/:id` with `poll=true` / `levels=true` (upgrade detection)
- [x] Raw frame bridge on `/devices/:id/ws`
- [x] CORS headers (tower-http, allow all origins)

## Phase 1i: CLI
- [x] Drop-in minidsp-rs command compatibility (status, probe, server, gain, mute, source, config, dirac, input, output)
- [x] `--force-kind` flag wired to override device profile (maps 2x4hd, flex, shd, etc.)
- [x] `--force-device`, `--force-dsp` flags (accepted)
- [x] Daemon mode (`minidsp server`) with auto-discovery, TCP bridge, UDP discovery, mDNS, Unix socket
- [x] CLI auto-discovery order: --daemon-sock → --tcp → --daemon-url → --usb → Unix socket → USB HID
- [x] All per-channel commands: gain, delay, mute, peq (set + bypass)
- [x] Global commands: volume, source, preset, dirac
- [x] Negative coefficient parsing fix (allow_hyphen_values, `--` separator)
- [x] Accept minidsp-rs config TOML format (--config flag, http_server.bind_address, tcp_server, advertise)
- [x] `-o jsonline` output (serde_json, PascalCase source names)
- [x] `-f <file>` execution (sequential, no reordering)

## Phase 1j: ESP32 — USB Host HID
- [x] ESP32 firmware compiles (esp-idf-svc 0.52, IDF v5.4, Python 3.14 venv via uv)
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
- [x] Test: protocol wire format compatibility (38 integration tests in protocol/tests/compatibility.rs)
- [x] Test: `minidsp -o jsonline` output format verified (PascalCase sources, correct field order)
- [x] Test: `minidsp probe` output preserves upstream line format
- [x] Test: `minidsp -f <file>` preserves file order exactly (sequential execution, no reordering)
- [x] Test: `--force-kind` compatibility behavior (wired to probe::identify)
- [x] Test: convenience compatibility routes verified (`/volume/:direction`, `/source/:source`, `/preset/:preset`)
- [x] Test: compatibility state/levels WebSocket on `/devices/:id` with upgrade detection
- [x] Test: raw frame bridge on `/devices/:id/ws`
- [ ] Test: Device Console coexistence (TCP bridge + UDP discovery) — NEEDS REAL HARDWARE
- [ ] Test: ezBEQ client compatibility (PEQ batch load) — NEEDS REAL HARDWARE
- [ ] Test: multi-device — NEEDS MULTIPLE DEVICES
- [ ] Test: reconnect after USB unplug/replug — NEEDS REAL HARDWARE
- [ ] Test: ESP32 end-to-end (WiFi → USB HID → MiniDSP) — NEEDS ESP32 FIRMWARE
- [ ] Device-family matrix tests — NEEDS REAL HARDWARE (each device type)
- [ ] Release artifacts: .deb (armhf, arm64, amd64), macOS (x86_64, aarch64), Windows .exe/.msi
- [ ] .deb includes: udev rules, systemd unit, default config
- [ ] ESP32 .bin firmware
- [ ] README with migration guide from minidsp-rs

## Known Issues / Deferred
- Legacy device config.xml address table extraction not implemented — devices work at DspKnown tier (basic support)
- DSP 115 input PEQ addresses confirmed but input mute/gain/polarity not in Device Console XML (marked Option::None)
- Unix socket CLI transport not fully implemented (fallback to USB works, daemon socket listener works)
- Idempotent retry on reconnect deferred — requires persisting pending state
- PWR-ICE TCP compat deferred — need device owner
- ESP32 flashing: close serial-mcp session before flashing. Use `idf.py -p /dev/cu.usbmodem<TARGET> flash` (NOT espflash). CH340 auto-resets via DTR/RTS — no boot button needed.

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
