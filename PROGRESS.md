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
- [x] `esp-idf-sys` FFI to USB Host raw API (not HID managed component) — descriptor walk, interface claim, interrupt transfers
- [x] Device enumeration (VID 0x2752) — auto-detect on USB connect event
- [x] 64-byte interrupt transfer read/write (no report ID on wire for report ID 0)
- [x] Separate XferSync per transfer direction (OUT/IN) — prevents spurious callback races
- [x] Blocking `command()` API with Mutex serialization — thread-safe for HTTP handlers
- [x] Hotplug detection (reconnect after unplug) — disconnect detection via callback, reconnect in main loop with backoff
- [ ] VBUS drive (GPIO high for MiniDSP USB enable) — not needed on DevKitC (both boards powered via UART)

## Phase 1k: ESP32 — HTTP API (EspHttpServer)
- [x] EspHttpServer (ESP-IDF httpd) — replaced picoserve (compatibility issues)
- [x] `GET /devices` — list connected devices
- [x] `GET /devices/0` — device status (volume, mute, source, preset, dirac)
- [x] `POST /devices/0` — set master status (returns error on USB failure, not 200)
- [x] `POST /devices/0/volume/{up,down,toggle_mute}` — compatibility routes
- [x] `POST /devices/0/source/{name}` — per-source routes from device profile
- [x] `POST /devices/0/preset/{0-3}` — preset switch routes
- [x] `GET/POST /devices/0/display` — brightness and idle time
- [x] `GET/POST /devices/0/dre` — DRE enable/disable
- [x] `GET /devices/0/tokens` — modification tokens
- [x] `POST /devices/0/preset/copy` — copy preset to flash
- [x] `GET /llms.txt` — AI discoverability
- [x] CORS headers (Access-Control-Allow-Origin: *) + OPTIONS wildcard preflight (204)
- [x] WebSocket raw bridge — /devices/0/ws with push-based status streaming (1s poll) + request-response
- [x] WebSocket compat — /devices/0 with GET/WS dual handling (FFI upgrade detection), ?levels=true (250ms) / ?poll=true (1s)
- [x] Level meter reading — CMD_READ_FLOATS (0x14) via DSP address table, contiguous batching, float→dB conversion

## Phase 1l: ESP32 — WiFi & Provisioning
- [x] SoftAP mode (SSID `minidsp-setup`, open)
- [x] WiFi credential storage (NVS) — read/write ssid+password
- [x] WiFi STA connection with exponential backoff (1s base, 2x, 60s max, 5 retries)
- [x] Captive portal HTML page with SSID/password form — glass-card design (open-sub-optimizer style)
- [x] `POST /api/wifi/connect` — save credentials + restart
- [x] SoftAP fallback on no credentials or failed STA connect
- [x] Captive portal auto-open on iOS/Android — DNS hijack + DHCP Option 114 + `esp_netif_set_dns_info` + wildcard HTTP redirect
- [x] DNS server: only answers Type A queries (non-A get ANCOUNT=0), 8KB stack
- [x] HTTP `/*` catch-all with `uri_match_wildcard: true`, absolute URL redirect, non-empty body (iOS requirement)
- [x] DHCP: `esp_netif_set_dns_info` to advertise AP IP as DNS server + Option 114 (RFC 8910 captive portal URI)
- [x] `CONFIG_LWIP_MAX_SOCKETS=16`, `max_open_sockets=13` for captive portal probe burst
- [x] mDNS via `EspMdns` — _minidsp._tcp:5380 with TXT records (hw_id, serial, product). Uses `espressif/mdns` from IDF Component Registry (moved out of core IDF in v5.4).
- [x] Auto-reconnect after WiFi disconnect — background monitor thread, exponential backoff (1s–60s)
- [ ] Reason code handling (AUTH_FAIL vs BEACON_TIMEOUT) — DEFERRED: monitor reconnects generically for now

## Phase 1m: ESP32 — Diagnostics
- [x] System info collection (heap free, heap min, uptime)
- [x] Periodic diagnostics logging (30s interval)
- [x] Custom `log::Log` ring buffer — 4KB circular buffer, writes to both UART and ring buffer
- [x] `GET /debug/info` endpoint — JSON system diagnostics (heap, WiFi, USB, uptime, log stats)
- [x] `GET /debug/log` endpoint — ring buffer snapshot as text/plain
- [x] `POST /debug/level` endpoint — runtime log level change (error/warn/info/debug/trace)
- [x] Runtime log level change — via HTTP endpoint and esp_log_level_set FFI

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
- [x] Test: ESP32 boots, WiFi SoftAP starts, USB host detects simulator, runs stable (167KB heap free, no crashes)
- [x] Test: full code review (31 issues — 3 critical, 9 high, 11 medium, 8 low) — all addressed
- [x] Test: boot order verified — captive portal (DNS+HTTP) starts before USB init (which blocks 30s)
- [x] Test: no boot loop — previous 4KB WiFi monitor stack caused stack overflow → watchdog reset. Fixed to 8KB.
- [x] Test: WiFi provisioning end-to-end (phone → SoftAP → captive portal auto-open → save creds → restart → STA) — PASSED 2026-04-06, connected to WPA3-SAE network at 192.0.2.246
- [x] Test: ESP32 full network test (2026-04-06) — 15 bugs found and fixed across 3 sessions. All 31 HTTP routes + 2 WS endpoints verified.
- [x] Test: USB hotplug — device power cycle (auto-reconnect ~4s), firmware reboot (device found in 6.7s), firmware-first/device-later (503 → background scan → auto-detect)
- [x] Test: mDNS discovery — hostname resolves, _minidsp._tcp browsable, TXT records correct
- [x] Test: WebSocket compat — levels=true (~250ms), poll=true (~1s), GET/WS interleave, raw bridge
- [x] Test: heap stability — 167KB free after 20 GET + 3 WS sessions, no leak, min watermark 139KB
- [ ] Device-family matrix tests — NEEDS REAL HARDWARE (each device type)
- [ ] Release artifacts: .deb (armhf, arm64, amd64), macOS (x86_64, aarch64), Windows .exe/.msi
- [ ] .deb includes: udev rules, systemd unit, default config
- [ ] ESP32 .bin firmware
- [ ] README with migration guide from minidsp-rs

## Known Issues / Deferred
- Legacy device config.xml address table extraction not implemented — devices work at DspKnown tier (basic support)
- DSP 115 input PEQ addresses confirmed but input mute/gain/polarity not in Device Console XML (marked Option::None)
- Idempotent retry on reconnect deferred — requires persisting pending state
- PWR-ICE TCP compat deferred — need device owner
- ESP32 simulator uses 2-byte address parsing for CMD_READ_FLOATS — returns 0 floats. Real hardware uses 3-byte (NEW dialect) correctly. Update simulator at `~/code/esp-uac2-host/simulators/minidsp-2x4hd/`.

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

## Phase 3: ESP32 USB Audio Output (Measurement Sweeps)

See [`docs/TODO-esp32-usb-audio.md`](docs/TODO-esp32-usb-audio.md) for full design (API, timing, clock sync, UAC2 driver details).

- [ ] Integrate `esp-uac2-host` as ESP-IDF component (submodule or local path)
- [ ] Open miniDSP UAC2 audio interface alongside existing HID interface
- [ ] Sweep generator: real-time Farina ESS (sin/exp, 48kHz stereo 24-bit)
- [ ] Signal structure: preamble → tone burst (80Hz, 3 cycles) → gap → sweep → decay
- [ ] `POST /devices/{id}/play` endpoint — generate + stream sweep, return `startedAtUs` + sample offsets
- [ ] `POST /sync` endpoint — clock sync (UDP ping-pong, min-RTT filtering)
- [ ] Hardware timestamp of first audio frame output (`uac2_host_stream_get_start_time`)
- [ ] Verify with real miniDSP: feedback format (10.14 vs 16.16), ASRC lock timing, full config descriptor
- [ ] Test: open-sub-optimizer network measurement path (sync → mute → play → capture → deconvolve)
- [ ] Test: REW manual sweep trigger via curl

---

## Stretch / Someday

- [ ] Preset import/export (Device Console XML format)
- [ ] Per-source volume offsets
- [ ] Template/offline device (in-memory simulation)
- [ ] MCP server (`--mcp` flag)
- [ ] OTA firmware updates (ESP32)
- [ ] Custom PCB (USB stick form factor)
- [ ] WiSA speaker management
