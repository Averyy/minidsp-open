# minidsp-open

Open-source MiniDSP controller. Clean-room implementation (not a fork). All devices, runs on Linux AND ESP32-S3 from one Rust codebase. Replaces both minidsp-rs (desktop) and the $75 WI-DG (WiFi bridge) with a single project. Protocol knowledge from minidsp-rs (credited) + MiniDSP Device Console XML extraction.

## Why Build This (not a fork of minidsp-rs)

We need a dual-target architecture (Linux + ESP32) from day one, and we have complete device data extracted directly from MiniDSP's Device Console XMLs. Clean-room implementation was simpler than retrofitting an existing codebase for a different platform target. Protocol knowledge from minidsp-rs (great project, credited in README).

minidsp-open adds:
- ESP32-S3 target (WiFi MiniDSP controller, no computer needed)
- All missing device definitions (Flex Eight, Flex HT, SHD Power) + all legacy devices
- Config read-back (save/restore full device state)
- Connection resilience (auto-reconnect, exponential backoff)
- ezBEQ on ESP32 (phase 2 — after core is verified)
- 12V trigger / power control via ESP32 GPIO (future — see `docs/FUTURE-12v-trigger.md`)
- `llms.txt` at webapp root for AI/LLM discoverability (MCP server skipped — zero demand, maybe later)
- Fixes for every actionable open issue

## Architecture

The existing minidsp-rs codebase is already split into three crates. ~90% of the code is platform-independent.

```
minidsp-open/
  protocol/              ← SHARED. All device logic. ~13k lines, pure Rust, no platform deps.
                            Device definitions, command encoding, address tables, dialect,
                            biquad format, packet framing. Compiles on ESP32 unchanged.
  minidsp/               ← SHARED client logic + platform-split transport
    transport/trait.rs       ← Transport trait (already exists conceptually)
    transport/hid.rs         ← cfg: hidapi (Linux) / esp-idf-sys FFI (ESP32)
    transport/tcp.rs         ← Shared (tokio, works on both platforms)
  daemon/                ← HTTP API + TCP server + discovery
    http/server_axum.rs      ← Linux: axum handlers (thin wrappers calling minidsp/ business logic)
    http/server_espidf.rs    ← ESP32: picoserve handlers (thin wrappers calling same business logic)
    tcp/mod.rs               ← Shared TCP bridge server (port 5333)
    discovery/mod.rs         ← Shared UDP discovery (port 3999)
  app-esp32/             ← ESP32 binary: WiFi provisioning, NVS config, GPIO trigger, OTA
  app-desktop/           ← Linux/macOS/Win binary: CLI + daemon (existing minidsp-rs entry points)
```

Platform splits use `cfg(target_os = "espidf")`. Proven pattern in esp-rs ecosystem.

### Platform-specific code (~10%):

| Component | Linux | ESP32-S3 |
|-----------|-------|----------|
| USB HID transport | hidapi (libusb) | ESP-IDF `usb_host_hid` via unsafe FFI (~100 lines) |
| Async runtime | tokio (full, multi-thread) | embassy-executor on ESP-IDF (`arch-std`, runs as FreeRTOS task) |
| HTTP server | axum | picoserve (zero heap alloc, axum-like API) |
| JSON | serde_json | serde-json-core (fixed buffer, no alloc) |
| Config storage | filesystem | NVS (ESP32 flash) |
| WiFi / mDNS / NVS | N/A | esp-idf-svc (std, wraps ESP-IDF C APIs) |
| 12V trigger | N/A | GPIO + MOSFET |
| WiFi provisioning | N/A | ESP-IDF captive portal |

**ESP32 is std-on-ESP-IDF, not no_std.** embassy-executor runs as a FreeRTOS task via `arch-std`. esp-idf-svc (std) provides WiFi, mDNS, and NVS. picoserve and serde-json-core are no_std-*compatible* libraries chosen for heap efficiency — not because the target is no_std. The protocol crate is the only true no_std crate (pure Rust, no platform deps).

axum on ESP32 was proven but too heavy (~30-40KB per connection, OOMs at 5 concurrent). picoserve uses zero heap allocation with user-controlled per-connection buffers (~2-4KB each). Device tables are all `&'static` / `const` — they live in flash (.rodata), not heap. ~15-20KB flash across all devices, near-zero RAM impact.

## Hardware (ESP32 target)

### Board: ESP32-S3-DevKitC (~$7)
- Two USB-C ports: one for power, one for OTG host mode
- OTG port → USB cable → MiniDSP
- Power port → USB charger
- WiFi built in

### Power
ESP32 is USB host, MiniDSP is USB device. **Two cables required** (power + data), same as the WI-DG. The MiniDSP USB port does NOT provide VBUS — it's sense-only (XMOS XU216 uses a voltage divider to detect host presence). The ESP32 must provide its own power AND drive VBUS high on the host cable so the MiniDSP enables its USB interface. Single-cable is not possible.

### 12V Trigger + Custom PCB

Separate project, after v1. See `docs/FUTURE-12v-trigger.md`.

## Protocol Reference

### USB HID
```
VID: 0x2752 (MiniDSP) or legacy 0x04D8/0x003F
Report size: 65 bytes (1 byte report ID 0x00 + 64 bytes data)
Write: [0x00] [payload] [0xFF padding to 65 bytes]
Read: 65 bytes, timeout 500ms
```

### Frame encoding (packet.rs)
```
Byte 0:     (payload.len() + 1) as u8    // length, self-inclusive
Bytes 1..N: payload (command bytes)
Byte N+1:   checksum = sum(all preceding bytes) & 0xFF
```

### TCP server (port 5333)
- Server-side framing: read `buf[0] + 1` bytes (length byte counts payload+checksum after itself)
- Client-side framing: `buf[0]` is self-inclusive length, no checksum on responses
- Zero bytes between frames = padding, skip them
- Fixed-packet-size mode: if first chunk >= 64 bytes, split into 64-byte chunks (WI-DG compat)
- TCP bridge: single connection (WI-DG compatibility, second connection rejected). HTTP API: concurrent, all serialized through command queue. Device Console (via TCP) + HTTP clients coexist safely — the command queue arbitrates all paths to the device.

### UDP discovery (port 3999)
Broadcast every 1 second:
```
Offset  Len  Field
0       4    Magic: 0x80 0x00 0x05 0xA0
4       2    Zero padding
6       6    MAC address
12      2    Zero padding
14      4    IPv4 address (network byte order)
18      1    Hardware ID (from device probe cmd 0x31)
19      1    Firmware major
20      1    Firmware minor
21      1    DSP ID (EEPROM 0xFFA1)
22      2    Serial number (big-endian u16, EEPROM 0xFFFE)
24      11   Zero padding
35      1    Hostname length
36+     N    Hostname string
```

### Complete HID command table (extracted from Device Console)

**Core DSP commands (minidsp-rs has these):**
| Byte | Command | Payload |
|------|---------|---------|
| 0x03 | Reset | (none) → response 0xAA when complete |
| 0x04 | WriteFlash | addr_hi:u8, addr_lo:u8, data:bytes |
| 0x05 | ReadFlash | addr_hi:u8, addr_lo:u8, count:u8 |
| 0x13 | WriteDSP | 0x80 (save) or 0xA0 (RAM-only), addr:2-or-3-byte, value:4-byte |
| 0x14 | ReadFloats | addr_hi:u8, addr_lo:u8, count:u8 (max 14) |
| 0x17 | SetMute | value:u8 (0x00=unmute, 0x01=mute) |
| 0x19 | BypassFilter | 0x80/0x00:bypass_bit, addr:2-or-3-byte |
| 0x25 | SetConfig | config:u8, reset:u8 → response 0xAB when complete |
| 0x30 | WriteBiquad | 0x80, addr:2-or-3-byte, 0x0000, 5×f32 coefficients |
| 0x31 | ReadHardwareId | (none) → `[0x31, fw_major, fw_minor, hw_id]` |
| 0x34 | SetSource | source:u8 |
| 0x39 | FirLoadStart | index:u8 → returns max coeff count |
| 0x3A | FirLoadData | index:u8, floats:f32_le[] (max 15) |
| 0x3B | ReloadDspParam | (none) — reload DSP params from flash |
| 0x3F | DiracBypass | value:u8 (0=enabled, 1=bypassed) |
| 0x42 | SetVolume | value:u8 (gain = -0.5 × value) |

**New commands (NOT in minidsp-rs):**
| Byte | Command | Payload | Notes |
|------|---------|---------|-------|
| 0x1A | SetDisplayBrightness | contrast_level % 0x80, current_level | Flex/HT/HTx OLED |
| 0x1B | SetDisplayIdleTime | timeout_secs % 256 | Flex/HT/HTx OLED |
| 0x1E | SetDRE | 0x00, value:u8 (0x01=on, 0x00=off) | Flex HT/HTx only |
| 0x27 | CopyPreset | (none)=async, 0x00=sync | Copy current preset to another slot |
| 0x45 | DiracDelayGain | 0xA1, channel_bitmask | Read Dirac filter delay/gain |
| 0x54 | LogControl | 0x01=on, 0x00=off | Enable device-side logging |

**Response types:**
| Byte | Response |
|------|----------|
| 0x00 | Error |
| 0x01 | OK / ACK |
| 0x02 | Event notification |
| 0xAA | Reset complete |
| 0xAB | Preset change complete |
| 0xFF | Buffer overflow / fatal |

**DSP error codes** (in PicVersion response byte 6):
| Code | Meaning |
|------|---------|
| 0 | No error |
| 0xF0 | DSP start failed |
| 0xF1 | Parameter error |
| 0xF2 | Bypass table error |

**Device error codes** (PicVersion response byte 7):
| Code | Meaning |
|------|---------|
| 0 | No error |
| 1 | Audio codec IC not found |
| 2 | Failed to load preset from flash |

### Flash memory map (page 0xFF)

Cross-verified against: Device Console JS, minidsp-rs source, and miniDSP Controller Android app (apk-extracted).

| Address | Size | Purpose |
|---------|------|---------|
| 0xFF:0xA1 | 1 | DSP ID (dsp_version) |
| 0xFF:0xC8 | 16 | Modification tokens (4 presets × 4 bytes, u32 BE) |
| 0xFF:0xD8 | 1 | Current preset slot |
| 0xFF:0xD9 | 1 | Current audio source index |
| 0xFF:0xDA | 1 | Master volume (encoded: dB = -0.5 × value) |
| 0xFF:0xDB | 1 | Master mute (1=muted, 0=unmuted) |
| 0xFF:0xE0 | 1 | Dirac bypass / Master FIR bypass |
| 0xFF:0xE8 | 5 | Display settings (idle_secs, brightness, pad, pad, contrast_current) |
| 0xFF:0xEA | 32 | Selectable audio sources (1 byte per source, up to 32) |
| 0xFF:0xED | 1 | DRE on/off state |
| 0xFF:0xFE | 2 | Serial number (u16 BE, actual = 900000 + value) |
| 0xFE:0x00 | N | Per-source volume offsets (1 byte per source, signed, half-dB steps) |

### Volume offset per-source

Each audio input can have an independent volume offset. Read: `ReadFlash(0xFE, 0x00, num_sources)`. Write: `WriteFlash(0xFE, 0x00, offset_bytes)`. Each byte = offset_dB × 2 (signed int8, half-dB steps). Enabled when HW XML has `voloffsets="true"`.

### Dialect (old vs new devices)
| | Old (m2x4, m4x10hd, m10x10hd, nanodigi2x8) | New (everything else) |
|--|--|--|
| Address size | 2 bytes | 3 bytes |
| Float encoding | 5.23 fixed-point | IEEE float32 LE |
| Mute values | `0x00800000` / `0x0` | `2` / `1` |
| Invert values | `0xFF800000` / `0x00800000` | `1` / `0` |
| Delay encoding | Int32(samples) | Int16(samples) |

One `if/else`, not per-device logic.

### Device ID table
| hw_id | dsp_version | Device | Sample rate |
|-------|-------------|--------|-------------|
| 10 | 100 | 2x4 HD | 96 kHz |
| 10 | 101 | DDRC-24 | 96 kHz |
| 27 | 100 | Flex | 96 kHz |
| 27 | 101 | Flex DL | 48 kHz |
| 14 | any | SHD | 96 kHz |
| 32 | 113 | Flex HTX | 48 kHz |
| 11 | 97 | C-DSP 8x12 v2 | N/A |
| 6 | 95 | DDRC-88BM | 48 kHz |
| 4 | any | miniSHARC 4x8 | 96 kHz |
| 1 | 51 | 10x10 HD | 96 kHz |
| 1 | other | 4x10 HD | 96 kHz |
| 2 | 54 | nanoDIGI 2x8 | 96 kHz |
| 2 | 22 | 2x4 (non-HD) | 96 kHz |
| **30** | **110/111** | **Flex Eight / Flex Eight DL** | **96 kHz** | 2 in / 8 out. Sources: 1:Toslink, 2:SPDIF, 3:USB, 4:Bluetooth (no analog, idx starts at 1). Address tables complete. |
| **31** | **113/115** | **Flex HT** | **48 kHz** | 8 in / 8 out. Sources: 1:Toslink, 2:SPDIF, 3:USB, 4:HDMI (no analog). Shares DSP with HTx. Address tables complete. |
| **21** | **105** | **SHD Power** | **96 kHz** | 2 in / 4 out. Sources: 0:Toslink, 1:SPDIF, 2:AES, 3:USB, 4:LAN. Same DSP as SHD, different source map. |
| **32** | **115** | **HTx new firmware** | **48 kHz** | Same I/O as HTx but adds input PEQ. Address tables complete (DSP 115). |
| **??** | **??** | **CDSP 8x12DL** (deferred) | **48 kHz** | 8 in / 12 out. Zero data — need probe from device owner. |

### Master status (EEPROM 0xFFD8, 9 bytes)
| Offset | Field |
|--------|-------|
| 0 | preset (u8, 0-3) |
| 1 | source (u8) |
| 2 | volume (u8, encoded: dB = -0.5 × value) |
| 3 | mute (u8, 1=muted) |
| 8 | dirac_bypass (u8, 0=enabled, 1=bypassed) — only if device supports Dirac |

### Config read-back (NEW — not in minidsp-rs)
No high-level read function exists in minidsp-rs, but the primitives are there:
- `ReadFloats` (cmd 0x14) reads up to 14 contiguous floats from any DSP address
- Every PEQ, gain, delay, crossover, compressor address is known from device definitions
- Build a `read_full_config()` that reads all DSP addresses for the current device → returns a full Config struct
- Use for: backup before modification, verification after write, measurement mode save/restore

### Config write constraints
- **Cannot write to non-active preset.** All DSP writes go to currently active state. Must switch preset first (causes brief audio interruption).
- **Atomic deployment strategy:** Switch to target preset → write all parameters → verify via read-back. No way to stage silently.
- **Compressor/FIR bypass:** per-output, single command each
- **Dirac bypass:** global, single command (cmd 0x3F)

## HTTP API

Match minidsp-rs endpoints + additions:

| Method | Path | Description |
|--------|------|-------------|
| GET | `/devices` | List all detected devices |
| GET | `/devices/:id` | Master status (preset, source, volume, mute, dirac) |
| GET | `/devices/:id?levels` | WebSocket: live input/output dB levels (250ms poll) |
| POST | `/devices/:id` | Set master status (preset, source, volume, mute) |
| POST | `/devices/:id/config` | Full config write (PEQ, crossover, gain, delay, polarity, compressor, FIR) |
| **GET** | **`/devices/:id/config`** | **Full config read-back (NEW)** |
| POST | `/devices/:id/volume/up` | +0.5 dB |
| POST | `/devices/:id/volume/down` | -0.5 dB |
| POST | `/devices/:id/volume/toggle_mute` | Toggle mute |
| POST | `/devices/:id/source/:source` | Set input source |
| POST | `/devices/:id/preset/:preset` | Set config preset |
| **POST** | **`/devices/:id/measurement-mode`** | **Enter measurement mode: save state, set flat/bypass (NEW)** |
| **POST** | **`/devices/:id/measurement-mode/restore`** | **Exit measurement mode: restore saved state (NEW)** |
| POST | `/trigger/on` | TODO: 12V trigger on — ESP32 only |
| POST | `/trigger/off` | TODO: 12V trigger off — ESP32 only |
| GET | `/trigger/status` | TODO: 12V trigger state — ESP32 only |
| **GET** | **`/devices/:id/display`** | **Read display settings (brightness, idle timeout) (NEW)** |
| **POST** | **`/devices/:id/display`** | **Set display brightness/idle (NEW)** |
| **GET** | **`/devices/:id/dre`** | **Read DRE status (NEW)** |
| **POST** | **`/devices/:id/dre`** | **Set DRE on/off (NEW)** |
| **POST** | **`/devices/:id/preset/copy`** | **Copy current preset to another slot (NEW)** |
| **GET** | **`/devices/:id/tokens`** | **Read modification tokens for all presets (NEW)** |
| **GET** | **`/debug/log`** | **Ring buffer log snapshot — ESP32 (NEW)** |
| **GET** | **`/debug/info`** | **JSON system diagnostics: heap, WiFi, USB state, uptime (NEW)** |
| **POST** | **`/debug/level`** | **Change runtime log level (NEW)** |
| **GET** | **`/debug/events`** | **SSE live log stream — phase 2 (NEW)** |
| **GET** | **`/api/system/status`** | **Data partition state, last sync time — ESP32 (NEW)** |
| **POST** | **`/api/system/reprovision`** | **Reset WiFi, enter SoftAP provisioning — ESP32 (NEW)** |
| GET | `/openapi.json` | OpenAPI schema |
| GET | `/llms.txt` | LLM-readable API description |
| ANY | `/devices/:id/ws` | WebSocket: raw binary frame bridge |

## Fixes Over minidsp-rs

| minidsp-rs issue | Problem | Our fix |
|-----------------|---------|---------|
| #765 | Device unreachable after standby/wake | Auto-reconnect with health checks, re-probe on USB reconnect |
| #511 | Daemon doesn't find device after boot | Retry USB enumeration with exponential backoff (100ms→10s) |
| #144 | Stale device after USB unplug | Proper USB hotplug lifecycle, clean up and notify clients |
| #662 | Can't run multiple devices | Per-device connection isolation, no global lock |
| #699 | Simultaneous connections corrupt state | Command queue with arbitration, return "busy" not silent corruption |
| #570 | Level streaming blocks preset changes | Interleave commands — pause level reads for state-changing writes |
| #557 | Delay hard limit too low | Per-device limits from device definition, not global constant |
| #663 | No standby/power control | 12V trigger via ESP32 GPIO (deferred — separate project after v1) |
| #566, #698, #657, #764 | Missing device definitions | All devices included at launch |

## Device Behavior Notes

- **No standby control via USB/TCP.** Flex standby is IR-only. USB drops off bus in standby. 2x4 HD has no standby. 12V trigger GPIO is our workaround.
- **Settings persist across power cycles.** All presets, PEQ, gain, delay, volume, source saved to flash.
- **Level metering available.** Real-time input/output dB via ReadFloats at meter addresses.
- **Sample rate is hardcoded per device** — 96kHz for most, 48kHz for some. Affects delay conversion.

## ESP32-S3 Constraints

- 512KB SRAM (~150-200KB free heap after WiFi/FreeRTOS), optional 8MB PSRAM
- WiFi stack: ~60-100KB heap
- picoserve: minimal overhead, ~2-4KB per connection (user-controlled buffers)
- Realistic capacity: 4-8 concurrent connections with headroom for application state
- USB Host HID: no safe Rust wrappers. Use `esp-idf-sys` unsafe FFI to C API.
- Device tables: ~15-20KB in flash (.rodata), near-zero heap (all `&'static const`)
- JSON: serde-json-core (fixed buffer, no alloc) — streams directly to socket via picoserve
- WiFi provisioning: SoftAP + captive portal, SSID/password saved to NVS
- WiFi reconnection: app-level exponential backoff (ESP-IDF does NOT auto-reconnect)
- Data partition: LittleFS (wear leveling + power-loss safety), ~5 MB for UI + catalogue
- Diagnostics: 4KB ring buffer + `/debug/log` + `/debug/info` endpoints (~4.2KB persistent RAM)
- Command queue: `heapless::Deque` (16 capacity), `embassy_sync` primitives
- mDNS: `EspMdns` from `esp-idf-svc` (singleton, own FreeRTOS task)

## Phase Overview

See `PROGRESS.md` for the full task checklist. High-level phasing:

**Phase 1 — Desktop + ESP32 core (ship first, verify it works):**
Protocol crate, all device definitions (7 SHARC + 6 legacy), command queue, config read-back, measurement mode, connection resilience, USB/TCP transports, HTTP API (axum + picoserve), CLI, ESP32 USB host HID, WiFi provisioning, diagnostics, testing.

**Phase 2 — ezBEQ on ESP32 (after core is verified):**
Catalogue index pipeline, data partition + auto-update, ezBEQ backend API, serve React UI from flash.

**Separate projects (after v1):**
12V trigger, MQTT discovery + HA entities, SSE live log streaming, syslog UDP.

## v1 Features From Device Console Extraction

Beyond what minidsp-rs supports, we add from day one:

| Feature | Commands |
|---------|----------|
| **Modification tokens** | ReadFlash(0xFF, 0xC8+idx×4, 4) / WriteFlash same |
| **Display brightness** | 0x1A (contrast, current) |
| **Display idle timeout** | 0x1B (seconds) |
| **DRE control** | 0x1E (on/off), ReadFlash(0xFF, 0xED, 1) |
| **Preset copy** | 0x27 (async/sync) |
| **Error code reporting** | Parse PicVersion response bytes 6-7 |

## Future Additions

**After v1 (separate projects):**
- **12V trigger** — ESP32 GPIO + MOSFET for power control. API endpoints, optional trigger input for receiver power state detection. Solves minidsp-rs #663. Needs hardware validation first.
- **MQTT discovery + state** — publish HA auto-discovery configs to `homeassistant/{type}/minidsp_{serial}/{entity}/config`. Ongoing state/control via MQTT topics. Complementary to mDNS.
- **SSE live log streaming** — `GET /debug/events` on ESP32 via picoserve `EventStream`.
- **Syslog UDP** — optional remote logging for ESP32, configured via API. RFC 5424 format.

**Someday / maybe:**
- **Preset import/export** — Device Console XML format. Read/write preset files, parse filter/FIR/label data.
- **Per-source volume offsets** — ReadFlash/WriteFlash(0xFE, 0x00, N).
- **Template/offline device** — In-memory device simulation for preset editing without hardware.
- **MCP server** — `/mcp` JSON-RPC/SSE endpoint. Design internal API as clean function calls so MCP calls same functions as HTTP handlers. Zero demand from target audience.
- **OTA firmware updates** — ESP32 supports OTA over WiFi natively. Add `/update` endpoint. Signed firmware images.
- **Custom PCB** — USB stick form factor with integrated MOSFET for trigger. BOM ~$5-8.
- **WiSA speaker management** — full scan/associate/assign/volume protocol (0xF0 subcommands). Low priority, niche audience.

## Bill of Materials (ESP32 prototype)

| Part | Cost |
|------|------|
| ESP32-S3-DevKitC | ~$7 |
| USB-C to USB-A female adapter | ~$3 |
| USB-A cable to MiniDSP | ~$3 |
| IRLZ34N MOSFET + diode (12V trigger) | ~$0.50 |
| **Total** | **~$14** |

## AVS Forum Coverage Audit

Comprehensive review of all minidsp-rs complaints from the ezBEQ thread (7,200+ posts), Flex HTx thread (400+ posts), and general MiniDSP thread (5,700+ posts). Every unique issue mapped to our plan:

| # | Issue | Frequency | Our fix | Status |
|---|-------|-----------|---------|--------|
| 1 | Device unreachable after standby/wake (#765) | COMMON | Auto-reconnect, health checks, exponential backoff | Planned |
| 2 | Boot race — daemon starts before USB ready (#511) | VERY COMMON | Retry USB enumeration, don't fail on first attempt | Planned |
| 3 | Concurrent access crashes minidsp-rs (#699) | COMMON | Command queue with arbitration, return "busy" | Planned |
| 4 | ARM .deb broken on Bookworm (glibc mismatch) | COMMON | We ship native static binaries for all platforms | Solved |
| 5 | Multi-device setup extremely difficult (#662) | COMMON | Per-device connection isolation, no global lock | Planned |
| 6 | HTx firmware update breaks minidsp-rs (#701) | MODERATE | DSP 115 definition, auto-detect hw_id, `--force-kind` escape hatch | Planned |
| 7 | Cannot read config back from device | VERY COMMON | Full config read-back via cmd 0x14 (`ReadFloats`) | Planned |
| 8 | HID error on newer platforms (RPi5, newer kernels) | MODERATE | Test hidapi on RPi5 + newer kernels during dev | **NEW — needs testing** |
| 9 | Version upgrade breaks config / bind defaults | COMMON | Design config format from scratch, sane defaults (bind 0.0.0.0) | Planned |
| 10 | Windows config file location undocumented | MODERATE | Unified config across platforms, XDG/well-known paths | Planned |
| 11 | Device Console discovery broken via minidsp-rs | COMMON | Proper UDP advertise implementation matching Device Console expectations | Planned |
| 12 | Intermittent communication timeouts | MODERATE | Connection health checks, retry with backoff | Planned |
| 13 | 10x10 filter routing overwrites user output EQ | MODERATE | BEQ loads to input PEQ first, overflows to XO biquads, never output PEQ | **NEW — add to design** |
| 14 | WI-DG is flaky and overpriced ($75) | UNIVERSAL | ESP32-S3 target ($10) replaces WI-DG | Planned |
| 15 | Levels display broken on non-2x4HD devices | MODERATE | All device types tested with correct level meter addresses from day one | Planned |
| 16 | Service autostart unreliable across platforms | MODERATE | Ship systemd unit in .deb; launchd and Windows service deferred | **NEW — add to deliverables** |
| 17 | Server/CLI mode conflict (can't run both) | COMMON | Single daemon architecture, CLI commands talk to daemon over socket | Planned |
| 18 | Flex Eight not supported (#566) | MODERATE | Full definition from Device Console XML | Planned |
| 19 | Flex HT/HTx incomplete support (#698) | COMMON | Full definition including DSP 115 (new FW with input PEQ) | Planned |
| 20 | Stale WebSocket state requires browser refresh | MODERATE | Robust WebSocket lifecycle, heartbeat, reconnect | **NEW — add to API design** |
| 21 | Device Console + minidsp-rs coexistence pain | COMMON | Proper TCP bridge, reliable UDP advertise | Planned |
| 22 | Preset change briefly mutes audio | LOW | Firmware behavior, not fixable in software | Won't fix |
| 23 | Python dependency hell installing ezBEQ on RPi | VERY COMMON | ESP32 eliminates the RPi entirely; desktop is a single static binary | Solved |
| 24 | No per-channel output control via API | MODERATE | Full per-channel API (gain, delay, mute, PEQ) from day one | Planned |
| 25 | No confirmation that BEQ filter was applied | MODERATE | Config read-back verifies actual device state after write | Planned |
| 26 | minidsp-rs development appears stalled | BACKGROUND | That's why we exist | Solved |

**4 new items added from this audit:** #8 (HID on newer platforms), #13 (10x10 filter routing), #16 (service management files), #20 (WebSocket lifecycle).

## Open Questions

1. ~~VBUS power from MiniDSP?~~ **ANSWERED: No.** USB port is sense-only. Two cables required. Not negotiable.
2. ~~axum vs picoserve on ESP32~~ **ANSWERED: picoserve + embassy.** axum works but OOMs at 5 connections (~30-40KB each). picoserve uses zero heap with user-controlled buffers. Linux keeps axum, ESP32 uses picoserve. Handler logic shared.
3. ~~Missing device config.xml files~~ **RESOLVED.** Device Console app extracted (see below). All DSP structure layouts, default parameters, source mappings, and firmware files obtained. SHD Power trivial (same DSP 105, add hw_id 21 + 5-source map). Flex Eight (DSP 110/111), Flex HT/HTx new FW (DSP 115) all have full structure layouts. Address tables to be computed from layouts or dumped via `debug dump-float`. Only CDSP 8x12DL still needs a device owner.
4. ~~OTA update security~~ — ESP32 OTA should require authentication. Signed firmware images.
5. ~~Multiple clients~~ **RESOLVED.** Single executor task with priority queue. See Command Queue Architecture section.
6. ~~embassy + esp-idf interop~~ **RESOLVED.** ESP32 target is std-on-ESP-IDF. `embassy-executor` with `arch-std` runs as a FreeRTOS task alongside `esp-idf-svc`. picoserve and serde-json-core are no_std-compatible but run in a std environment.
7. ~~hidapi on RPi5 + newer kernels~~ **RESOLVED.** No blockers. See Resolved Technical Questions.
8. ~~ezBEQ entry ID format~~ **RESOLVED.** Dual lookup: ephemeral `{git_hash}_{index}` + stable SHA-256 digest. v3 API tries ID first, falls back to digest.
9. ~~Legacy device address tables~~ **RESOLVED.** SigmaStudio config.xml exports available for all devices. See `docs/ref-legacy-devices.md`.
10. ~~Command pacing / multi-client~~ **RESOLVED.** See Command Queue Architecture section.
11. ~~ESP32 boot without internet~~ **RESOLVED.** See ESP32 Boot Sequence section.
12. ~~WiFi provisioning~~ **RESOLVED.** SoftAP + captive portal. See ESP32 Boot Sequence section.
13. ~~mDNS service type~~ **RESOLVED.** `_minidsp._tcp` on port 5380. See Network Discovery section.
14. ~~Biquad validation~~ **RESOLVED.** Warn by default, opt-in reject. See Biquad Stability Validation section.
15. ~~ESP32 logging~~ **RESOLVED.** Ring buffer + HTTP endpoint. See ESP32 Diagnostics section.

## MiniDSP Device Console (extracted)

Download URLs (undocumented, found via app auto-updater):
- macOS (Apple Silicon): `https://phpstack-793508-3229840.cloudwaysapps.com/deviceconsole/latest-macmx`
- Windows: `https://phpstack-793508-3229840.cloudwaysapps.com/deviceconsole/latest-win`

Full device registry with all extracted data: **see `docs/ref-devices.md`**

Extracted app bundle is at `app/MiniDSP Device Console.app/`.

### Device Support Package
Location: `Contents/Resources/device_support_package/`

```
info.xml                    ← Master device registry (hw_id → DSP structures, USB VID/PID)
HW/{hw_id}/info.xml         ← Per-device: source mappings, firmware versions, XMOS info
DSP_Structures/{dsp_id}/
  DSP_structure_layout.xml  ← Signal chain topology (modules, connections, channel count)
  DSP_default_parameters.xml ← Parameter ranges, default values, labels
  info.xml                  ← DSP firmware info, conversion paths
```

### Confirmed device data from extracted XMLs:

| hw_id | dsp_id | Name | Freq | In/Out | Sources (idx → name) |
|-------|--------|------|------|--------|---------------------|
| 10 | 100 | 2x4 HD | 96kHz | 2/4 | 0:Analog, 1:Toslink, 2:USB |
| 10 | 101 | DDRC-24 | — | 2/4 | (same) |
| 14 | 105 | SHD | — | 2/4 | 0:Toslink, 1:SPDIF, 2:AES, 3:RCA, 4:XLR, 5:USB, 6:LAN |
| 17 | 105 | SHD Studio | — | 2/4 | (same as SHD) |
| **21** | **105** | **SHD Power** | — | 2/4 | **0:Toslink, 1:SPDIF, 2:AES-EBU, 3:USB, 4:LAN** (5 sources, not 7) |
| 27 | 100 | Flex | 96kHz | 2/4 | 0:Analog, 1:Toslink, 2:USB, 3:Bluetooth |
| 27 | 101 | Flex DL | — | 2/4 | (same) |
| **30** | **110** | **Flex Eight** | **96kHz** | **2/8** | **1:Toslink, 2:SPDIF, 3:USB, 4:Bluetooth** (no analog, idx starts at 1) |
| **30** | **111** | **Flex Eight DL** | — | 2/8 | (same) |
| **31** | **113** | **Flex HT** (old FW) | 48kHz | 8/8 | **1:Toslink, 2:SPDIF, 3:USB, 4:HDMI** (no analog) |
| **31** | **115** | **Flex HT** (new FW) | 48kHz | 8/8 | (same, adds input PEQ) |
| **32** | **113** | **Flex HTx** (old FW) | 48kHz | 8/8 | 0:Analog, 1:Toslink, 2:SPDIF, 3:USB, 4:HDMI |
| **32** | **115** | **Flex HTx** (new FW) | 48kHz | 8/8 | (same, adds input PEQ) |

Key findings:
- DSP 115 (new firmware) adds `GROUP_INPUT` with PEQ10 to Flex HT/HTx — this is what breaks minidsp-rs issue #701
- DSP 110 (Flex Eight) has FIR support (max 4096 taps, 2048 per channel) + input PEQ + 8 outputs with full compressor/crossover
- Flex HT (hw_id 31) and Flex HTx (hw_id 32) share DSP structures (113 and 115) but differ in source mapping (HT has no analog)
- SHD/SHD Studio/SHD Power all share DSP 105 but have different source mappings

### Address Table Generation
The Device Console XMLs define signal chain topology (DSP_structure_layout.xml) and parameter ranges (DSP_default_parameters.xml), but NOT explicit numeric DSP register addresses. The addresses are computed at runtime by the Device Console from the structure layout + firmware binary.

To get addresses for new devices, two approaches:
1. **Dump from hardware**: Connect device, use minidsp-rs `debug dump-float 0 255` to read all DSP float registers. Map known values to symbols.
2. **Reverse the address computation**: The Device Console JS (in app.asar, minified) contains the logic that maps structure layouts to addresses. Could be deobfuscated.
3. **Compare with existing**: DSP 110 (Flex Eight) likely shares address patterns with DSP 100 (2x4 HD / Flex) since both are 96kHz SHARC. DSP 115 shares with 113 (HTx) with added input PEQ block.

## Additional minidsp-rs Issues to Address

From comprehensive GitHub issue review (34 open, 10 dependabot PRs, last release 2024-10-27):

| Issue | What | Priority | Effort |
|-------|------|----------|--------|
| #762 | TCP server doesn't start on Windows 11 | Medium | Easy |
| #703 | "No devices found" when actually permission denied | Medium | Trivial |
| #666 | Legacy USB VID 0x04D8 not scanned (miniSHARC, nanoDIGI, old 2x4) | High | Easy |
| #625 | Negative PEQ coefficients parsed as CLI flags | Low | Trivial |
| #571 | No CORS headers for REST API | Medium | Trivial |
| #569 | nanoAVR HDA (hw_id 8) partially broken | Low | 1 day |
| #245 | PWR-ICE125/250 — deferred, see `docs/FUTURE-pwr-ice.md` | — | Transport (70-byte frames) included; full support needs user packet captures |
| #229 | MiniDSP 2x4 non-HD (legacy dialect, VID 0x04D8) | Medium | 2 days |

Also: ship OpenAPI docs at `/docs` endpoint (current minidsp-rs docs page is empty). Ship `llms.txt` at webapp root for AI/LLM discoverability of the API.

## Release Artifacts

Must ship pre-built binaries for every platform minidsp-rs users are on:

| Artifact | Target | Notes |
|----------|--------|-------|
| `.deb` (armhf) | RPi 32-bit Bullseye/Bookworm | Critical — most ezBEQ setups are RPi |
| `.deb` (arm64) | RPi 64-bit Bookworm | Growing share of RPi users |
| `.deb` (amd64) | x86_64 Linux | NAS, server, desktop |
| `.tar.gz` (aarch64-apple-darwin) | macOS Apple Silicon | |
| `.tar.gz` (x86_64-apple-darwin) | macOS Intel | |
| `.exe` / `.msi` (x86_64-windows) | Windows | |
| `.bin` (ESP32-S3) | ESP32 firmware | Flash via esptool or web flasher |

Each `.deb` includes:
- Static binary (no runtime deps)
- udev rules at `/etc/udev/rules.d/70-minidsp.rules`
- systemd unit at `/lib/systemd/system/minidsp.service` (with `After=dev-bus-usb.target` — no sleep hack)
- Default config at `/etc/minidsp/config.toml`

GitHub Actions CI builds all artifacts on every tagged release.

## Home Assistant Integration

Existing landscape is fragmented:
- `trevorwarwick/minidspshd` (8 stars, SHD-only via `_Volumio._tcp` zeroconf → Volumio HTTP API port 3000)
- `xuio/minidsp-rs-hass` (2 stars, all devices via WebSocket at `ws://host:5380`, no auto-discovery)
- None use UDP 3999 discovery or mDNS for DSP control

Our advantages:
- **Drop-in minidsp-rs API** = existing `minidsp-rs-hass` WebSocket integration works immediately, zero changes
- **mDNS hostname** = reachable by name (`minidsp-plum.local`) instead of IP. `_minidsp._tcp` service advertised for future HA zeroconf integration (no HA component exists yet — not auto-discovery today)
- **Config read-back** = full device state queryable (volume, preset, source, per-channel gain/delay/PEQ)
- **MQTT discovery** (future) = native HA entities without custom component
- **WebSocket streaming** = real-time level meters + state changes (compat with existing xuio integration)

## Config Backup & Restore

Full device state is ~2.1 KB per preset (~528 floats × 4 bytes). All 4 presets = ~8.4 KB. Trivially small.

Enabled by config read-back (cmd 0x14, `ReadFloats`):
- `GET /devices/{id}/config` — full state as JSON
- `POST /devices/{id}/config` — restore from JSON
- `GET /devices/{id}/config/export` — Device Console XML format (for interop)
- `POST /devices/{id}/config/import` — import from Device Console XML

Use cases:
- **Backup before BEQ load** — save state, load BEQ, restore after movie
- **Preset cloning** — read preset 1, write to preset 2
- **Migration** — read from old device, write to new device
- **Version control** — save configs as JSON files in git
- **Measurement mode** — save state, flatten all EQ, measure, restore

Modification tokens (EEPROM 0xFFC8, 4 × u32 per preset) detect if another client changed the device since last read. Use for cache invalidation and multi-client coordination.

## Resolved Technical Questions

### picoserve WebSocket: YES
picoserve v0.18.0 fully supports WebSocket behind the `ws` feature flag. Provides `WebSocketUpgrade` extractor, `SocketRx`/`SocketTx` for async frame I/O, ping/pong, protocol negotiation. Suitable for both ezBEQ API compat and live level streaming.

### Embassy + esp-idf interop: WORKS with caveats
`embassy-executor` with `arch-std` feature works alongside `esp-idf-svc`. Key: remove `critical-section` from embassy features to avoid conflicts. Known timer driver bugs (esp-idf-svc#619, #630) caused by FreeRTOS priority inversion — fixed by using `embassy-executor` (not `async-executor`) or latest `edge-executor`.

### hidapi on RPi5: NO BLOCKERS
No Pi 5-specific hidapi issues with standard HID devices. Kernel 6.12.x had a hid-core regression (fixed in 6.12.75). Main issue is permissions — ship udev rules:
```
KERNEL=="hidraw*", ATTRS{idVendor}=="2752", TAG+="uaccess", GROUP="plugdev", MODE="0660"
SUBSYSTEMS=="usb", ATTRS{idVendor}=="2752", TAG+="uaccess", GROUP="plugdev", MODE="0660"
```

### ESP32-S3 USB Host HID: No safe wrapper, FFI required
No safe Rust wrapper exists. Use `esp-idf-sys` FFI to the ESP-IDF HID Host class driver (`usb_host_hid` component). ~200-300 lines of unsafe glue. Well-tested C API underneath.

### ezBEQ entry ID format: Dual lookup needed
- Ephemeral: `{git_hash}_{index}` — changes every catalogue refresh
- Stable: SHA-256 digest of `json.dumps({title, filters, mv, season, episode})`
- v3 API tries ID first, falls back to digest
- Our biquad index must support both

### ezBEQ license: MIT (both ezbeq and beqcatalogue)
Full permission to redistribute UI build artifacts and reprocess catalogue data, including commercially. Must include MIT copyright notice.

### ezBEQ UI build: No separate release artifact
UI static files are bundled inside the Python wheel on PyPI (in `ezbeq/ui/`). Our GitHub Action must either extract from the wheel or build from source (`cd ui && yarn install && yarn build`).

## License

MIT. 100% our code, not a fork. Credit minidsp-rs in README as protocol reference and inspiration.

## Legacy Device Support

**All legacy devices included at launch.** Full details in `docs/ref-legacy-devices.md`.

### Address table sourcing (clean-room)

- **SHARC devices (hw_id 10-32):** Address tables extracted from Device Console XML (`DSP_default_parameters.xml`). Already complete in `docs/ref-address-tables.md`.
- **Legacy ADAU1445 devices (hw_id 1-8):** Address tables extracted from SigmaStudio config.xml exports — these are MiniDSP device data files, not minidsp-rs code. We write our own extraction code and device definitions.
- We do NOT copy minidsp-rs's generated Rust files or their codegen pipeline. Same principle as SHARC devices: use the manufacturer's source data, write all code ourselves.

### Device inventory at launch

**Full support (13 devices):**

| Category | Devices |
|----------|---------|
| SHARC / New dialect | 2x4 HD, DDRC-24, Flex, Flex DL, SHD, SHD Studio, SHD Power, Flex Eight, Flex Eight DL, Flex HT, Flex HTx (both DSP 113 + 115) |
| Legacy / Old dialect | 4x10 HD, 10x10 HD, NanoDigi 2x8 |
| Legacy / New dialect (SHARC despite age) | miniSHARC 4x8, DDRC-88BM, C-DSP 8x12 v2 |

**Basic support only (3 devices):** miniDSP 2x4 non-HD, nanoAVR, nanoAVR HDA — volume/mute/preset/source only, no PEQ. No config.xml exists for nanoAVR/HDA.

**Deferred:** PWR-ICE (see `docs/FUTURE-pwr-ice.md`), CDSP 8x12DL (need device owner probe).

### Effort: ~1.5 days

| Task | Time |
|------|------|
| Config.xml parser (shared, simple XML) | 2 hours |
| Dialect layer (5.23 fixed-point codec + 2-byte addr) | 2-3 hours |
| 6 legacy device definitions (extract from config.xml) | 3-6 hours |
| Wire up 2x4 non-HD from existing symbols | 1-2 hours |
| nanoAVR/HDA basic support stubs | 30 min |
| Probe entries for all hw_ids | 30 min |

## Command Queue Architecture

Single executor task owns the transport. All commands from all clients go through a priority queue. Full details in `docs/ref-command-pacing.md`.

### USB HID timing (measured)

- Round-trip: ~3ms per simple command
- Practical sustained throughput: ~150-250 commands/second
- Preset switch: 4-5 seconds device-side processing after ACK
- Protocol is strictly synchronous: one command in flight, one response. No command tagging.

### Priority levels

| Priority | Category | Examples | On busy |
|----------|----------|----------|---------|
| 0 (LOW) | Telemetry | Level meters, compressor meters | Skip this cycle (`try_enqueue`) |
| 1 (NORMAL) | Status reads | get_master_status, TCP bridge commands | Queue and wait (10s timeout) |
| 2 (HIGH) | Config writes | PEQ, gain, delay, volume, source | Queue as batch, response after all complete |
| 3 (CRITICAL) | State transitions | Preset switch, FIR load, measurement mode | Must complete, blocks all else |

### Key behaviors

- **Batch operations** (ezBEQ's 40-command PEQ load) execute contiguously — not interleaved with level reads
- **Level polling pauses** during batch operations, resumes after completion
- **Post-preset-switch gating:** Queue enters DeviceBusy state, polls device status every 500ms until preset confirms changed, before allowing further commands
- **TCP bridge decoded and queued:** Device Console commands go through the same queue as everything else. Unrecognized command bytes pass through as raw frames at NORMAL priority — never reject a valid frame. This preserves compatibility with any Device Console version.
- **Per-device queues:** Each physical device gets its own queue and executor task. No global lock.
- **ESP32 adaptation:** `heapless::Deque` (16 capacity), `embassy_sync` primitives, 500ms level interval

### Why not minidsp-rs's approach

minidsp-rs uses a Mutex on the transport write + Hub broadcast. This causes:
- Level polling holds SharedService Mutex, starving all other commands (#570)
- TCP bridge does raw frame forwarding with zero serialization (#699)
- No "busy" response to callers — commands silently queue or fail
- No pause mechanism for level polling during batch operations
- Preset switch doesn't gate subsequent commands

## Partial Write Recovery

WriteBiquad (cmd 0x30) always saves to flash immediately (0x80 flag hardcoded). No RAM-only mode for biquads. No transaction mechanism. Each PEQ write is instantly active on the audio path AND persisted.

### Strategy: Bypass-first write ordering + idempotent retry

**During batch PEQ writes (ezBEQ filter load, API batch, etc.):**

1. Bypass all target PEQ bands first (`bypass on` for each)
2. Write all biquad coefficients
3. Un-bypass the written bands (`bypass off` for each)

If interrupted after step 1 but before step 3: bands are silent (bypassed) rather than in a mixed old/new state. Sonically safe.

**On reconnect after interrupted batch:**
- Pending batch is saved in connection state
- On auto-reconnect, re-send the entire batch from the beginning
- Biquad writes are idempotent — rewriting an already-written band is harmless

**Optional verification after batch completion:**
- Read back written registers via `ReadFloats` (cmd 0x14)
- Compare against intended values
- If mismatch, log warning and retry once

**What we do NOT implement:**
- No read-before-write rollback (adds latency, can't rollback if disconnected anyway)
- No RAM-only pseudo-commit (protocol doesn't support it for biquads)
- No journaling (overkill — device survives power loss fine)

**CLI compatibility note:** When running `minidsp -f <file>`, commands execute exactly as given in the file (preserving ezBEQ's exact ordering). The bypass-first ordering is only used for our batch API endpoints, not for sequential CLI commands. This preserves drop-in compatibility.

## Device Identification: Two-Stage Matching

minidsp-rs uses a single `match (hw_id, dsp_version)` that falls to Generic on unknown combinations, losing all PEQ/crossover/metering capability. We split into two stages.

### Stage 1: Match dsp_version → DSP address table

```
dsp_version 100 → DSP_100 (2x4 HD / Flex)
dsp_version 101 → DSP_101 (DDRC-24 / Flex DL)
dsp_version 105 → DSP_105 (SHD / SHD Studio / SHD Power)
dsp_version 110 → DSP_110 (Flex Eight)
dsp_version 111 → DSP_111 (Flex Eight DL)
dsp_version 113 → DSP_113 (Flex HT/HTx old FW)
dsp_version 115 → DSP_115 (Flex HT/HTx new FW)
(legacy dsp_versions for old-dialect devices)
unknown       → None (Tier 1 only)
```

### Stage 2: Match hw_id → device profile (source names, features)

```
(hw_id 10, dsp 100) → 2x4 HD profile
(hw_id 10, dsp 101) → DDRC-24 profile
(hw_id 30, dsp 110) → Flex Eight profile
...
unknown             → None (generic source names)
```

### Capability tiers

| Tier | Requirements | What works |
|------|-------------|-----------|
| Universal | Always | Volume, mute, preset, source by index, hardware info, flash read/write |
| DspKnown | Known dsp_version | + Full PEQ/crossover/delay/compressor/FIR/metering, config read-back |
| Full | Known hw_id | + Source names, display control, DRE, Dirac, per-source volume offsets |

A new hw_id that reuses an existing DSP structure (e.g., hypothetical Flex Eight-N with DSP 110) automatically gets Tier 2 — full PEQ/crossover/delay/metering — even though we don't know its source names. minidsp-rs would drop it to Generic and lose everything.

### CLI flags (backward compatible)

- `--force-kind {type}` — backward compat alias, maps to both DSP structure and device profile (same behavior as minidsp-rs)
- `--force-device {name}` — override device profile only (new)
- `--force-dsp {id}` — override DSP structure only, keep auto-detected hw_id for source mapping (new, safer)

### Unknown device communication

CLI output for unknown device:
```
Connected to unknown device (hw_id=35, dsp_version=110, serial=901500)
DSP structure recognized: Flex Eight (2 in / 8 out)
Source names unavailable — use 'minidsp source 1' (numeric index)
Full DSP control available (PEQ, crossover, delay, FIR, metering)
Report this device: https://github.com/.../issues/new?template=new-device
```

API response includes capability tier and degraded_reason. No telemetry — clear error message with GitHub issue template link.

## Biquad Stability Validation

Unstable biquad coefficients (poles outside unit circle) cause DSP oscillation → loud noise → potential speaker damage. Neither the ADAU1445 nor ADSP-21489 validate coefficients on hardware. CamillaDSP validates and rejects; minidsp-rs does not.

### The check (trivial)

```rust
fn is_stable(a1: f32, a2: f32) -> bool {
    a1.is_finite() && a2.is_finite()
        && a2.abs() < 1.0
        && a1.abs() < 1.0 + a2
}
```

Two comparisons per biquad. Nanoseconds on any platform.

### Behavior (warn by default, opt-in reject)

**Drop-in compatibility rule: we only add, never break.** minidsp-rs accepts any coefficients. Rejecting by default would break existing scripts. Instead:

| Path | Default behavior | Opt-in strict mode |
|------|-----------------|-------------------|
| CLI `peq set` | Warn to stderr, write anyway | `--strict` rejects with error |
| HTTP API | Warn in response JSON, write anyway | `?strict=true` returns 400 |
| ezBEQ batch | Warn in log, write anyway | Strict mode per-request |
| Config import (preset restore) | Warn in log, write anyway | N/A (came from device) |
| TCP proxy (Device Console) | No validation | N/A (transparent proxy) |

Warning message in plain English: explains the risk, suggests checking sample rate mismatch (most common cause of bad coefficients from REW), shows the offending coefficients.

## Network Discovery

Run all three mechanisms simultaneously. No conflicts.

### UDP broadcast (port 3999) — required for Device Console

Exact packet format from minidsp-rs protocol. Magic bytes `0x80 0x00 0x05 0xA0`, MAC, IP, hw_id, dsp_version, serial, hostname. Broadcast every 1 second. Required for Device Console and MiniDSP mobile app compatibility.

### mDNS — hostname resolution + service advertisement

Primarily useful for `.local` hostname resolution so users can reach ESP32 by name instead of IP. Service advertisement (`_minidsp._tcp`) is cheap infrastructure for a future HA integration but does NOT provide HA auto-discovery today — no HA component exists that listens for it. The existing `minidsp-rs-hass` integration works with our API out of the box via manually configured WebSocket.

**Service type:** `_minidsp._tcp` on port 5380

**Hostname:** `minidsp-{word}.local` — human-friendly, deterministic from serial number

Default hostname is derived by hashing the device serial number (EEPROM 0xFFFE) into a ~50-word list of short (3-5 letter), unambiguous, easy-to-type words. Examples:
```
minidsp-plum.local
minidsp-oak.local
minidsp-reef.local
```

User-settable via NVS — the SoftAP provisioning page includes a "Device name" field. Users with multiple devices can set meaningful names:
```
minidsp-sub.local       (subwoofer DSP)
minidsp-main.local      (main speakers)
```

Or clear the suffix for single-device setups: just `minidsp.local`.

Word list: ~50 words, ~250 bytes in flash. Short, distinct, no homophones, no profanity, no confusable spellings. For the near-zero chance of collision on the same network, mDNS conflict resolution (RFC 6762 Section 9) auto-appends `-2`.

**Instance name:** `MiniDSP 2x4 HD` (friendly name, visible in network browsers)

**TXT records:**
```
txtvers=1  hwid=10  dsp=100  model=2x4HD  serial=AB64
fw=1.85  api=5380  tcp=5333  ezbeq=1
```

**Secondary service:** `_http._tcp` on port 5380 with `path=/` and `vendor=minidsp` for generic service browsers.

### Implementation

- **ESP32:** `EspMdns` from `esp-idf-svc` (~10 lines, singleton, runs own FreeRTOS task)
- **Linux:** `mdns-sd` crate (pure Rust, tokio-compatible, no avahi dependency)
- Both also run UDP 3999 broadcast for Device Console compat

### Home Assistant (future)

No HA custom component at launch. The existing `minidsp-rs-hass` WebSocket integration works with our drop-in API immediately.

Future HA integration could use `_minidsp._tcp` zeroconf for auto-discovery. MQTT discovery (phase 2) would provide native HA entities without a custom component. Neither is needed for launch — the existing ecosystem works as-is.

## ESP32 Boot Sequence

Full details in `docs/ref-esp32-boot.md`.

### Boot flow

```
POWER ON
  → Init NVS + hardware + USB HID host (blocking, fast)
  → Check NVS for WiFi creds
    → No creds: SoftAP provisioning (see below)
  → Connect WiFi (30s window, exponential backoff: 1s base, 2x, 60s cap)
    → Timeout: start HTTP API anyway if data exists, keep retrying
  → Got IP: start HTTP API + mDNS immediately (serve whatever is in flash)
  → Fetch manifest.json in background (3s timeout)
    → Up to date: done
    → Changed: download files (15s timeout each, non-blocking for API)
    → Failed: log warning, serve stale data
  → READY
```

**Key principle: the MiniDSP is always controllable.** USB HID works regardless of WiFi. minidsp-rs API works the moment WiFi is up. Only ezBEQ catalogue browsing degrades.

### Data partition states

| State | minidsp-rs API | ezBEQ UI | ezBEQ catalogue |
|-------|---------------|----------|-----------------|
| EMPTY (first boot) | Fully functional | 503 "Downloading..." | Empty list |
| STALE (update check failed) | Fully functional | Works normally | Old data |
| CURRENT (matches manifest) | Fully functional | Works normally | Current data |

Expose via `GET /api/system/status` → `{"data_state": "stale", "last_sync": "...", "last_check_error": "timeout"}`.

### WiFi provisioning: SoftAP + captive portal

Matching WLED/Tasmota/ESPHome pattern. No app install required.

1. Device starts SoftAP: SSID `minidsp-setup` (open network)
2. DNS redirect triggers captive portal on phone/laptop
3. Web page: scan available networks, pick one, enter password, optionally set device name (for mDNS hostname)
4. Save to NVS, switch to STA mode, connect
5. Success: restart into normal mode. Failure: re-enter SoftAP with error message.

Re-provisioning: after N consecutive AUTH_FAIL (code 202) disconnects, automatically re-enter provisioning. Or `POST /api/system/reprovision`.

Consider `esp-wifi-provisioning` crate (v0.1.0, March 2026) as starting point.

### WiFi reconnection

ESP-IDF does NOT auto-reconnect. Our application handles it:

- Exponential backoff: 1s base, 2x multiplier, 60s cap, 20 retries before 5-minute pause
- Distinguish reason codes: AUTH_FAIL (202) = wrong password → limited retries, then re-provision. BEACON_TIMEOUT (200) = AP went away → retry aggressively. NO_AP_FOUND (201) → retry with backoff.
- On disconnect: close all WebSocket/TCP connections, stop HTTP listener
- On reconnect + got-IP: restart HTTP server, re-bind listeners. Clients must reconnect.

### Flash wear

Data partition: LittleFS (wear leveling + power-loss safety + directory support).

- NOR flash: 100,000 erase cycles per sector minimum
- 5 MB partition = 1,280 sectors with wear leveling
- Catalogue updates ~2x/week → 62 erases/sector/year → **1,600+ years flash life**
- Version check before write: boots with no update cause **zero flash erases**

## ESP32 Diagnostics

Full details in `docs/ref-esp32-logging.md`.

### Phase 1 (ship with v0.1)

**Custom `log::Log` implementation** that writes to both:
- UART (via EspIdfLogger delegation — always on)
- 4KB ring buffer in RAM (~40-80 log lines at INFO level)

**Endpoints:**
- `GET /debug/log` — snapshot of ring buffer as plain text (ESP-IDF format: `I (12345) usb: connected`)
- `GET /debug/info` — JSON system diagnostics: `{firmware, uptime_secs, heap_free, heap_min_free, wifi: {connected, ssid, rssi, ip}, usb: {connected, device, hw_id, serial, reconnects, last_cmd_ms}, log_level, log_buffer_used}`
- `POST /debug/level` — change runtime log level within compiled range

### Phase 2

- `GET /debug/events` — SSE live log streaming (picoserve has native `EventStream` support)
- Syslog UDP (optional, configured via API)

### Key decisions

- `log` crate (not `tracing` — too heavy for ESP32)
- Plain text format (not JSON lines — 2-4x smaller, matches UART output)
- Compile-time `max_level_info` in release builds (strips DEBUG/TRACE calls)
- **Keep diagnostic infrastructure in release builds** — the whole point is diagnosing deployed devices
- Total persistent RAM cost: ~4.2KB

### Desktop target

- `tracing` + `tracing-subscriber`, log to stderr, `RUST_LOG` env var
- Same `/debug/log` and `/debug/info` endpoints for consistency
- Optional `--log-file` flag
