# minidsp-open

Open-source MiniDSP controller. Clean-room implementation (not a fork). All devices, runs on Linux AND ESP32-S3 from one Rust codebase. Replaces both minidsp-rs (desktop) and the $75 WI-DG (WiFi bridge) with a single project. Protocol knowledge from minidsp-rs (credited) + MiniDSP Device Console XML extraction.

## Why Build This (not a fork of minidsp-rs)

We need a dual-target architecture (Linux + ESP32) from day one, and we have complete device data extracted directly from MiniDSP's Device Console XMLs. Clean-room implementation was simpler than retrofitting an existing codebase for a different platform target. Protocol knowledge from minidsp-rs (great project, credited in README).

minidsp-open adds:
- ESP32-S3 target (WiFi MiniDSP controller, no computer needed)
- All missing device definitions (Flex Eight, Flex HT, SHD Power, CDSP 8x12DL)
- Config read-back (save/restore full device state)
- Connection resilience (auto-reconnect, exponential backoff)
- 12V trigger / power control via ESP32 GPIO
- MCP server for AI assistant integration
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
    http/handlers.rs         ← SHARED: pure functions, device-agnostic
    http/server_axum.rs      ← Linux: axum wiring
    http/server_espidf.rs    ← ESP32: embedded-svc or picoserve wiring
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
| Async runtime | tokio (full, multi-thread) | embassy (no_std, interrupt-driven, lighter than tokio) |
| HTTP server | axum | picoserve (no_std, zero heap alloc, axum-like API) |
| JSON | serde_json | serde-json-core (no_std, no alloc, fixed buffer) |
| Config storage | filesystem | NVS (ESP32 flash) |
| 12V trigger | N/A | GPIO + MOSFET |
| WiFi provisioning | N/A | ESP-IDF captive portal |

axum on ESP32 was proven but too heavy (~30-40KB per connection, OOMs at 5 concurrent). picoserve + embassy uses zero heap allocation with user-controlled per-connection buffers (~2-4KB each). Device tables are all `&'static` / `const` — they live in flash (.rodata), not heap. ~15-20KB flash across all devices, near-zero RAM impact.

## Hardware (ESP32 target)

### Board: ESP32-S3-DevKitC (~$7)
- Two USB-C ports: one for power, one for OTG host mode
- OTG port → USB cable → MiniDSP
- Power port → USB charger
- WiFi built in

### Power
ESP32 is USB host, MiniDSP is USB device. **Two cables required** (power + data), same as the WI-DG. The MiniDSP USB port does NOT provide VBUS — it's sense-only (XMOS XU216 uses a voltage divider to detect host presence). The ESP32 must provide its own power AND drive VBUS high on the host cable so the MiniDSP enables its USB interface. Single-cable is not possible.

### 12V Trigger (optional hardware add-on)
- ESP32 GPIO → IRLZ34N MOSFET → switches MiniDSP DC power
- API: `POST /trigger/on`, `POST /trigger/off`, `GET /trigger/status`
- ESP32 stays powered independently — always reachable on WiFi even when MiniDSP is off
- Optional trigger INPUT: detect receiver power state → auto-power MiniDSP
- Hardware cost: ~$0.50 (MOSFET + flyback diode)
- Solves minidsp-rs #663 permanently

### Custom PCB (future)
ESP32-S3-WROOM module, USB-A male plug, LDO, MOSFET for trigger. USB stick form factor. BOM ~$5-8.

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
- Single client at a time for WI-DG compatibility; support multiple clients as improvement

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
| **30** | **110/111** | **Flex Eight** (missing) | **96 kHz?** | 2 in (digital only) / 8 out. Sources: Toslink(0)/SPDIF(1)/USB(2)/BT(3). No analog. USB PID 0x0012 (non-DL) / 0x004A (DL). Need config.xml from Device Console. |
| **31** | **113** | **Flex HT** (missing) | **48 kHz** | 2 in (digital only) / 8 out. Sources: Toslink/SPDIF/USB/HDMI (no analog, shifts IDs vs HTx). Shares DSP structure with HTx. USB PID 0x004B. |
| **21** | **105** | **SHD Power** (missing) | **96 kHz** | 2 in / 4 out. Sources: Toslink(0)/SPDIF(1)/AES(2)/USB(3)/LAN(4). Same DSP as SHD, just add hw_id + 5-source map. USB PID 0x0045. **Ready to add.** |
| **32** | **115** | **HTx new firmware** (breaks existing) | **48 kHz** | Same I/O as HTx but adds input PEQ, shifts DSP addresses. Need config.xml from new Device Console. |
| **??** | **??** | **CDSP 8x12DL** (missing) | **48 kHz** | 8 in / 12 out. Zero data — need probe from device owner. USB PID 0x0047. |

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
| **POST** | **`/trigger/on`** | **12V trigger on — ESP32 only (NEW)** |
| **POST** | **`/trigger/off`** | **12V trigger off — ESP32 only (NEW)** |
| **GET** | **`/trigger/status`** | **12V trigger state — ESP32 only (NEW)** |
| **GET** | **`/devices/:id/display`** | **Read display settings (brightness, idle timeout) (NEW)** |
| **POST** | **`/devices/:id/display`** | **Set display brightness/idle (NEW)** |
| **GET** | **`/devices/:id/dre`** | **Read DRE status (NEW)** |
| **POST** | **`/devices/:id/dre`** | **Set DRE on/off (NEW)** |
| **POST** | **`/devices/:id/preset/copy`** | **Copy current preset to another slot (NEW)** |
| **GET** | **`/devices/:id/tokens`** | **Read modification tokens for all presets (NEW)** |
| **GET** | **`/devices/:id/volume-offsets`** | **Read per-source volume offsets (NEW)** |
| **POST** | **`/devices/:id/volume-offsets`** | **Set per-source volume offsets (NEW)** |
| **POST** | **`/devices/:id/preset/import`** | **Import Device Console preset XML file (NEW)** |
| **GET** | **`/devices/:id/preset/export`** | **Export current preset as Device Console XML (NEW)** |
| GET | `/openapi.json` | OpenAPI schema |
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
| #663 | No standby/power control | 12V trigger via ESP32 GPIO |
| #566, #698, #657, #764 | Missing device definitions | All devices included at launch |

## Device Behavior Notes

- **No standby control via USB/TCP.** Flex standby is IR-only. USB drops off bus in standby. 2x4 HD has no standby. 12V trigger GPIO is our workaround.
- **Settings persist across power cycles.** All presets, PEQ, gain, delay, volume, source saved to flash.
- **Level metering available.** Real-time input/output dB via ReadFloats at meter addresses.
- **Sample rate is hardcoded per device** — 96kHz for most, 48kHz for some. Affects delay conversion.

## ESP32-S3 Constraints

- 512KB SRAM (~300KB available after WiFi/FreeRTOS), optional 8MB PSRAM
- WiFi stack: ~60-100KB heap
- embassy + picoserve: minimal overhead, ~2-4KB per connection (user-controlled buffers)
- Realistic capacity: 4-8 concurrent connections with headroom for application state
- USB Host HID: no safe Rust wrappers. Use `esp-idf-sys` unsafe FFI to C API.
- Device tables: ~15-20KB in flash (.rodata), near-zero heap (all `&'static const`)
- JSON: serde-json-core (no_std, no alloc) — streams directly to socket via picoserve
- WiFi provisioning: ESP-IDF built-in captive portal, SSID/password saved to NVS

## Effort Estimate

| Component | Time |
|-----------|------|
| Fork + repo setup, CI for both targets | 1 day |
| ESP32 USB Host HID transport (esp-idf-sys FFI) | 3 days |
| Config read-back (ReadFloats at device addresses) | 2 days |
| Measurement mode (save/restore state, flat/bypass) | 2 days |
| Missing device definitions (Flex Eight, Flex HT, SHD Power, CDSP 8x12DL) | 2 days |
| Connection resilience (reconnect, backoff, hotplug lifecycle) | 3 days |
| Multi-client / command arbitration | 2 days |
| Per-device delay limits | 1 day |
| ESP32 WiFi provisioning + NVS config | 1 day |
| ESP32 12V trigger GPIO | 1 day |
| UDP discovery (shared) | 1 day |
| TCP bridge server (shared, port 5333) | 1 day |
| HTTP API server (ESP32 wiring) | 2 days |
| Measurement mode + config read-back endpoints | 1 day |
| Testing: minidsp-rs CLI, Device Console, mobile apps | 2 days |
| Testing: ESP32 target end-to-end | 2 days |
| **Total** | **~4 weeks** |

## v1 Features From Device Console Extraction

Beyond what minidsp-rs supports, we add from day one:

| Feature | Commands | Effort |
|---------|----------|--------|
| **Preset import/export** (Device Console XML format) | Read/write preset files, parse filter/FIR/label data | 2 days |
| **Modification tokens** | ReadFlash(0xFF, 0xC8+idx×4, 4) / WriteFlash same | Trivial |
| **Display brightness** | 0x1A (contrast, current) | Trivial |
| **Display idle timeout** | 0x1B (seconds) | Trivial |
| **DRE control** | 0x1E (on/off), ReadFlash(0xFF, 0xED, 1) | Trivial |
| **Preset copy** | 0x27 (async/sync) | Trivial |
| **Per-source volume offsets** | ReadFlash/WriteFlash(0xFE, 0x00, N) | 1 day |
| **Error code reporting** | Parse PicVersion response bytes 6-7 | Trivial |
| **Template/offline device** | In-memory device simulation for preset editing | 2 days |

## Future Additions

- **MCP server** — `/mcp` JSON-RPC/SSE endpoint. Exposes tools: `set_gain`, `set_delay`, `write_peq`, `get_status`, `set_preset`, `read_levels`, `trigger_on`, `trigger_off`, `set_display`, `set_dre`. Design internal API as clean function calls so MCP calls same functions as HTTP handlers.
- **OTA firmware updates** — ESP32 supports OTA over WiFi natively. Add `/update` endpoint.
- **Custom PCB** — USB stick form factor with integrated MOSFET for trigger.
- **Home Assistant integration** — the HTTP API is already HA-friendly. Could add MQTT discovery.
- **WiSA speaker management** — full scan/associate/assign/volume protocol (0xF0 subcommands). Low priority, niche audience.

## Bill of Materials (ESP32 prototype)

| Part | Cost |
|------|------|
| ESP32-S3-DevKitC | ~$7 |
| USB-C to USB-A female adapter | ~$3 |
| USB-A cable to MiniDSP | ~$3 |
| IRLZ34N MOSFET + diode (12V trigger) | ~$0.50 |
| **Total** | **~$14** |

## Open Questions

1. ~~VBUS power from MiniDSP?~~ **ANSWERED: No.** USB port is sense-only. Two cables required. Not negotiable.
2. ~~axum vs picoserve on ESP32~~ **ANSWERED: picoserve + embassy.** axum works but OOMs at 5 connections (~30-40KB each). picoserve uses zero heap with user-controlled buffers. Linux keeps axum, ESP32 uses picoserve. Handler logic shared.
3. ~~Missing device config.xml files~~ **RESOLVED.** Device Console app extracted (see below). All DSP structure layouts, default parameters, source mappings, and firmware files obtained. SHD Power trivial (same DSP 105, add hw_id 21 + 5-source map). Flex Eight (DSP 110/111), Flex HT/HTx new FW (DSP 115) all have full structure layouts. Address tables to be computed from layouts or dumped via `debug dump-float`. Only CDSP 8x12DL still needs a device owner.
4. **OTA update security** — ESP32 OTA should require authentication. Signed firmware images.
5. **Multiple clients** — start with single client (WI-DG compat), expand to multi-client with command queue.
6. **embassy + esp-idf interop** — picoserve needs embassy, but USB Host HID needs esp-idf-sys FFI. Can run embassy executor alongside esp-idf-hal on separate pthreads (proven pattern by ivmarkov). Need to verify this works cleanly for our use case.

## MiniDSP Device Console (extracted)

Download URLs (undocumented, found via app auto-updater):
- macOS (Apple Silicon): `https://phpstack-793508-3229840.cloudwaysapps.com/deviceconsole/latest-macmx`
- Windows: `https://phpstack-793508-3229840.cloudwaysapps.com/deviceconsole/latest-win`

Full device registry with all extracted data: **see `devices.md`**

Extracted app bundle is at `~/code/temp/minidsp-open/MiniDSP Device Console.app/`.

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

## License

MIT. 100% our code, not a fork. Credit minidsp-rs in README as protocol reference and inspiration.
