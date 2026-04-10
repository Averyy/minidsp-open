# minidsp-open

Open-source MiniDSP controller. Clean-room implementation (not a fork). Runs on Linux AND ESP32-S3 from one Rust codebase. The long-term target is support for every device defined by MiniDSP's own desktop and Android app assets, not just the subset currently modeled by minidsp-rs. Replaces both minidsp-rs (desktop) and the $75 WI-DG (WiFi bridge) with a single project. Protocol knowledge from minidsp-rs (credited) + MiniDSP official app extraction.

## Why Build This (not a fork of minidsp-rs)

We need a dual-target architecture (Linux + ESP32) from day one, and we have authoritative device data extracted from MiniDSP's own desktop and Android app assets. Clean-room implementation was simpler than retrofitting an existing codebase for a different platform target. Protocol knowledge from minidsp-rs (great project, credited in README).

minidsp-open adds:
- ESP32-S3 target (WiFi MiniDSP controller, no computer needed)
- All missing device definitions (Flex Eight, Flex HT, SHD Power) + all legacy devices
- Config inspection and restore surfaces, with verified-shadow fallback on DSP100 paths where live runtime readback is not authoritative
- Connection resilience (auto-reconnect, exponential backoff)
- ezBEQ on ESP32 (phase 2 — after core is verified)
- 12V trigger / power control via ESP32 GPIO (future — see `docs/FUTURE-12v-trigger.md`)
- `llms.txt` at webapp root for AI/LLM discoverability (MCP server skipped — zero demand, maybe later)
- Fixes for every actionable open issue


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

Plus features minidsp-rs never had: config inspection/restore APIs, measurement mode, display control, DRE, modification tokens, ESP32 target, ezBEQ integration, `llms.txt` for AI discoverability. Preset import/export, per-source volume offsets deferred to post-v1.

## Current Delivery Strategy

We only have one physical MiniDSP on hand for live validation, so the project cannot rely on physically bench-testing every model.

The 2x4 HD is the worked example for the shared implementation pattern:
- validate protocol behavior on the real device we do have
- compare that behavior against the extracted official desktop/Android app logic
- turn the result into shared protocol rules plus data-driven device metadata

Broader device coverage therefore has to be repeatable and importer-driven:
1. normalize official app assets from `app/app-extracted` and `app/apk-extracted`
2. enumerate every official-app device against the current Rust support table
3. import missing device definitions, source maps, typed config metadata, and capability flags systematically
4. use the physical 2x4 HD as the live regression case for shared protocol behavior

When the extracted official app and `minidsp-rs` disagree, the official app assets win.

First repeatable coverage pass:
- `uv run python tools/extract_device_support_matrix.py`
- `docs/ref-device-coverage.md`
- `docs/generated/device-support-matrix.json`
- `protocol/src/device/generated_official.rs`
- `third_party/legacy-configs/*/config.xml`
- importer-backed protocol tests:
  - `protocol/tests/official_assets.rs`
  - `protocol/tests/official_dsp_tables.rs`
  - `protocol/tests/legacy_assets.rs`
  - `protocol/tests/legacy_profile_metadata.rs`
  - `protocol/tests/legacy_dsp_tables.rs`

This is no longer only a reporting step. The generated coverage artifacts now gate the protocol crate:
- modern official HW/DSP pairs must stay mapped in `probe.rs`
- modern DSP address tables must keep referencing indexes that exist in the extracted official XML
- cached legacy families with local `config.xml` sources must stay represented in the generated matrix
- generated legacy metadata must keep matching the shipped legacy probe names/dialects/core dimensions and FIR capability flags
- promoted legacy runtime tables must keep referencing indexes that exist in the cached `config.xml` corpus
- extractor drift is treated as a bug, not documentation noise

Current importer-backed promotions:
- `miniSHARC 4x8`, `DDRC-88BM`, `C-DSP 8x12 v2`, `10x10 HD`, `4x10 HD`, `miniDSP 2x4`, and `NanoDigi 2x8` now ship real source-backed runtime tables instead of placeholders
- the import work also fixed two shared runtime-model bugs:
  - FIR is no longer treated as input-only in the CLI/runtime path
  - output polarity is now optional in `DspTable`, matching source-backed devices that do not expose polarity on every output
- the extractor now also infers old-dialect topology from `PEQ_*` and `EQ_ch*` naming families, which is how `miniDSP 2x4` graduated from fake `0/0` generated metadata to a source-backed `2 in / 4 out` runtime table
- desktop HID enumeration and ESP32 USB-host attach now share an official-asset-backed USB acceptance helper instead of duplicated local VID/PID checks

Current remaining importer scope:
- USB-only official product hints still need exact probe/runtime mapping from app assets:
  - `HA-DSP`
  - `DDRC-22`
  - `DDRC-88`
  - `CDSP-8x12/DL`
  - `Harmony DSP`
- for those labels, USB discovery is no longer the blocker; the missing piece is exact runtime identity from official app assets
- the current best classification is now explicit:
- `HA-DSP` = public product family known, exact runtime identity unresolved
- `DDRC-22` = public family known + small Dirac runtime bucket known, exact runtime identity unresolved
- `DDRC-88` = public `DDRC-88A / DDRC-88D` family known + legacy 8-channel Dirac bucket known, exact runtime identity unresolved
- `CDSP-8x12/DL` = public `C-DSP 8x12 DL` family known + 8x12 custom-install bucket known, exact runtime identity unresolved
- `Harmony DSP` = public `Harmony DSP 8x12` family known + HT-family 8x12 bucket known, exact runtime identity unresolved
- the older Android quick controller only branches on source-list `hw_id` values `[1, 2, 4, 5, 10, 11, 14, 17, 18, 27, 30, 31, 32]`, with `7/8` used only for a separate HDMI toggle; none of the unresolved USB-only labels appear there as standalone runtime `hw_id` families, which is useful negative evidence for alias reuse rather than missing hidden rows in the old APK logic
- the generated coverage report now also captures the package boundary directly:
  - exact HW directories present in the Device Console package
  - exact DSP directories present in the Device Console package
  - desktop product-image rows keyed by `product-HW-DSP`
  - desktop userdocs rows keyed by `Contents/Resources/userdocs/product-path.json`
  - sparse desktop manual-topic rows keyed by `Contents/Resources/userdocs/file-index.json`
  - packaged Dirac-helper alias strings
- the generated USB acceptance helper now follows the exact official VID/PID list instead of accepting every `0x2752:*` device
- the extracted desktop app does expose a blank-device fallback-model catalog path (`get-dspid-fallback-models`), but decoded bundle logic shows it is just `deviceSupportPkg.deviceIdList` grouped by `dspId`, not a separate alias-identity catalog
- package inventory, product-image rows, sparse userdocs rows, and fallback-model logic now all agree that the exact modern runtime matrix is still just the 13 enumerated HW/DSP pairs
- `Harmony DSP` is no longer only a top-level USB comment: the packaged Dirac helper contains `Harmony DSP 8x12`, `Failed to get dsp id, assume it is 113`, and `dsp id is 115`, which makes it a source-backed 8x12 family candidate even though the exact `hw_id` is still unresolved
- `CDSP-8x12/DL` is also slightly stronger than before because the same Dirac helper contains `PADSP-8x12-DL`, reinforcing the 8x12 custom-install family connection without exposing an exact `hw_id`
- for the unresolved families, the generated metadata now also carries supporting public product/manual URLs so public product naming and sibling-family organization stay explicit without being confused for exact runtime identity
- `NanoDigi 2x8` now follows the wider `4 / 10` internal topology inferred from its cached `config.xml`; keep treating the extracted app assets as source of truth when product names and internal channel maps disagree


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
    http/server_espidf.rs    ← ESP32: EspHttpServer handlers (thin wrappers calling same business logic)
    tcp/mod.rs               ← Shared TCP bridge server (port 5333)
    discovery/mod.rs         ← Shared UDP discovery (port 3999)
  app-esp32/             ← ESP32 binary: WiFi provisioning, NVS config, GPIO trigger, OTA
  app-desktop/           ← Linux/macOS/Win binary: CLI + daemon (existing minidsp-rs entry points)
```

Platform splits use `cfg(target_os = "espidf")`. Proven pattern in esp-rs ecosystem.

### Platform-specific code (~10%):

| Component | Linux | ESP32-S3 |
|-----------|-------|----------|
| USB HID transport | hidapi (libusb) | ESP-IDF USB Host raw API via unsafe FFI (~300 lines) |
| Runtime | tokio (full, multi-thread) | Blocking std on FreeRTOS (no async) |
| HTTP server | axum | EspHttpServer (ESP-IDF httpd, FreeRTOS task pool) |
| WebSocket | axum (native) | EspHttpServer ws_handler + raw FFI for GET/WS dual handling |
| JSON | serde_json | serde_json (same crate — ESP32 has enough heap) |
| Config storage | filesystem | NVS (ESP32 flash) |
| WiFi / mDNS / NVS | N/A | esp-idf-svc (std, wraps ESP-IDF C APIs) |
| 12V trigger | N/A | GPIO + MOSFET (future) |
| WiFi provisioning | N/A | Captive portal (DNS hijack + DHCP Option 114) |

**ESP32 is std-on-ESP-IDF, not no_std.** Blocking architecture — each HTTP request runs in an EspHttpServer FreeRTOS task. esp-idf-svc (std) provides WiFi, mDNS, NVS, and HTTP server. The protocol crate is the only true no_std crate (pure Rust, no platform deps). Device tables are all `&'static` / `const` — they live in flash (.rodata), not heap.

Original plan used picoserve (zero-alloc) and embassy-executor (async). Switched to EspHttpServer + blocking std after picoserve had compatibility issues with ESP-IDF httpd. serde_json works fine on ESP32 — 167KB heap free at steady state with 13 open sockets.

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
Write: [0x00] [request_frame] [0xFF padding to 65 bytes]
Read: 64 bytes data from interrupt IN; logical response is [len][payload], bytes after frame boundary may be arbitrary tail data
```

### Request frame encoding (packet.rs)
```
Byte 0:     (payload.len() + 1) as u8    // length, self-inclusive
Bytes 1..N: payload (command bytes)
Byte N+1:   checksum = sum(all preceding bytes) & 0xFF
```

### Response frame encoding
- HID / direct USB responses: `[len][payload]` with no checksum
- TCP server responses: `[len][payload]` with no checksum
- HID interrupt IN packets may contain arbitrary non-protocol bytes after the logical frame boundary inside the fixed 64-byte packet; ignore them

### TCP server (port 5333)
- Server-side framing: read `buf[0] + 1` bytes total (`buf[0]` is self-inclusive length covering length byte + payload; the extra +1 is the checksum appended after)
- Client-side framing: `buf[0]` is self-inclusive length, no checksum on responses
- Zero bytes between frames = padding, skip them
- Fixed-packet-size mode: if first chunk >= 64 bytes, split into 64-byte chunks (WI-DG compat)
- TCP bridge: single connection (WI-DG compatibility, second connection rejected). HTTP API: concurrent, all serialized through command queue. Device Console (via TCP) + HTTP clients coexist safely — the command queue arbitrates all paths to the device.

### UDP discovery (port 3999)
Broadcast every 1 second. Note: Device Console listens on multicast group `224.0.0.250:3999`, while minidsp-rs broadcasts to `255.255.255.255:3999`. We should send both (broadcast + multicast) for maximum compatibility.
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

### Complete HID command table (extracted from official app assets)

**Core DSP commands (minidsp-rs has these):**
| Byte | Command | Payload |
|------|---------|---------|
| 0x03 | Reset | (none) → response 0xAA when complete |
| 0x04 | WriteFlash | addr_hi:u8, addr_lo:u8, data:bytes |
| 0x05 | ReadFlash | addr_hi:u8, addr_lo:u8, count:u8 |
| 0x13 | WriteDSP | 0x80 (save) or 0xA0 (RAM-only), addr_hi:u8, addr_lo:u8, value:4-byte |
| 0x14 | ReadFloats | addr_hi:u8, addr_lo:u8, count:u8 (max 14) |
| 0x17 | SetMute | value:u8 (0x00=unmute, 0x01=mute) |
| 0x19 | BypassFilter | 0x80/0x00:bypass_bit, addr_hi:u8, addr_lo:u8, 0x00, 0x00 |
| 0x25 | SetConfig | config:u8, reset:u8 → response 0xAB when complete |
| 0x30 | WriteBiquad | 0x80, addr_hi:u8, addr_lo:u8, 0x00, 0x00, 5×f32 coefficients |
| 0x31 | ReadHardwareId | (none) → `[0x31, fw_major, fw_minor, hw_id]` |
| 0x34 | SetSource | source:u8 |
| 0x39 | FirLoadStart | index:u8 → returns max coeff count |
| 0x3A | FirLoadData | index:u8, floats:f32_le[] (max 15) |
| 0x3B | ReloadDspParam | (none) — reload DSP params from flash |
| 0x3F | DiracBypass | value:u8 (0=enabled, 1=bypassed) |
| 0x42 | SetVolume | value:u8 (gain = -0.5 × value) |
| 0x49 | SetHdmiSource | source:u8 | nanoAVR (hw_id 7/8) HDMI input switching — separate from SetSource (0x34) |

**New commands (NOT in minidsp-rs):**
| Byte | Command | Payload | Notes |
|------|---------|---------|-------|
| 0x1A | SetDisplayBrightness | contrast_level % 0x80, current_level | Flex/HT/HTx OLED |
| 0x1B | SetDisplayIdleTime | timeout_secs % 256 | Flex/HT/HTx OLED |
| 0x1E | SetDRE | 0x00, value:u8 (0x01=on, 0x00=off) | Flex Eight and Flex HT only (NOT Flex HTx — no `HAS_DRE_OPTION` in Device Console XML) |
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
| 0xFF:0xE8 | 2 | Display settings (byte 0: idle_secs, byte 1: brightness) — APK reads only 0xE8. Device Console may read more via SetDisplayBrightness (0x1A) which uses contrast_level and current_level as separate command params, not flash reads. |
| 0xFF:0xEA | varies | Selectable audio sources (1 byte per source, device-dependent count) |
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

### Device registry

Canonical source:
- `docs/ref-devices.md` — full hw_id / dsp_version registry, source maps, USB VID/PID, capabilities
- `docs/ref-device-coverage.md` — generated official-app coverage snapshot versus the current Rust support table
- `docs/ref-address-tables.md` — canonical SHARC address tables
- `docs/ref-legacy-devices.md` — canonical legacy inventory and probe mapping

Do not maintain device tables in this file. `PROJECT.md` is the architecture/scope doc; the `docs/ref-*` files are the source of truth for device identification and address data.

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

Verification note:
- Desktop config inspection is implemented, and live 2026-04-10 daemon checks on the physical DSP100 / 2x4 HD restored fast `GET /devices/{id}` / `GET /devices/{id}/config` behavior. Some fields still come from verified shadow/token-backed restore rather than universal authoritative live hardware readback, and low-address DSP100 scalar/tap reads now clamp to shadow or conservative zero when the device returns bogus raw words instead of real state. When local preset shadow is missing, the daemon can also seed it from token-matched official Device Console `setting*.xml` snapshots, which is a stronger controller-side source than stale cache but still not the same thing as authoritative live DSP readback.
- ESP32 physical-hardware config read-back is only partially verified on the live 2x4 HD so far: typed scalar reads/writes and PEQ write/readback work through the shared PEQ shadow/default model, but the broader DSP100 authoritative-state gaps remain open.

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
| GET | `/devices/:id` + WS upgrade | Compatibility WebSocket for live state + levels (`poll=true`, `levels=true`) |
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
| ANY | `/devices/:id/ws` | Raw frame bridge compatibility endpoint |

Canonical compatibility reference:
- `docs/ref-compatibility.md` — canonical CLI / HTTP / WebSocket / TCP / UDP compatibility contract
- `docs/ref-ezbeq.md` — ezBEQ-specific integration details and phase 2 strategy
- `docs/ref-command-pacing.md` — queueing/arbitration behavior for HTTP/TCP/WebSocket paths

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

- 512KB SRAM (~167KB free heap at steady state after WiFi/FreeRTOS/HTTP), optional 8MB PSRAM
- WiFi stack: ~60-100KB heap
- EspHttpServer: ESP-IDF httpd, 16KB stack per handler, 13 max open sockets, 35 URI handlers
- Realistic capacity: 13 concurrent connections, heap min watermark ~139KB under load
- USB Host HID: no safe Rust wrappers. `esp-idf-sys` unsafe FFI to C USB Host raw API (~300 lines).
- Device tables: ~15-20KB in flash (.rodata), near-zero heap (all `&'static const`)
- JSON: serde_json (standard crate — ESP32 has enough heap, ~167KB free)
- WiFi provisioning: SoftAP + captive portal (DNS hijack + DHCP Option 114), SSID/password saved to NVS
- WiFi reconnection: app-level exponential backoff (ESP-IDF does NOT auto-reconnect)
- Data partition: LittleFS (wear leveling + power-loss safety), ~5 MB for UI + catalogue
- Diagnostics: 4KB ring buffer + `/debug/log` + `/debug/info` endpoints (~4.2KB persistent RAM)
- USB bus: Mutex-serialized blocking `command()` API, shared across HTTP handler threads
- mDNS: `EspMdns` from `esp-idf-svc` (singleton, own FreeRTOS task). Component from IDF Component Registry (`espressif/mdns`).

## Phase Overview

See `PROGRESS.md` for the full task checklist. High-level phasing:

**Phase 1 — Desktop + ESP32 core (ship first, verify it works):**
Protocol crate, all device definitions (7 SHARC + 6 legacy), command queue, config read-back, measurement mode, connection resilience, USB/TCP transports, HTTP API (axum + EspHttpServer), CLI, ESP32 USB host HID, WiFi provisioning, mDNS, WebSocket compat, level meters, diagnostics, testing.

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
- **SSE live log streaming** — `GET /debug/events` on ESP32 via EspHttpServer chunked response.
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
| 7 | Cannot read config back from device | VERY COMMON | Full config inspection via cmd 0x14 (`ReadFloats`) where the device exposes usable values, plus verified shadow/default fallback for the known DSP100 gaps | Partially implemented and live-verified on desktop; ESP32 still only has the typed-scalar/PEQ path closed on the physical 2x4 HD |
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

## Research Status

Planning/research is complete enough to begin implementation. Canonical reference docs:
- `docs/ref-devices.md` — device registry and extracted app data
- `docs/ref-device-coverage.md` — generated official-app coverage and DSP typing snapshot
- `docs/ref-compatibility.md` — canonical external compatibility contract
- `docs/ref-address-tables.md` — SHARC address tables
- `docs/ref-legacy-devices.md` — legacy address sourcing and probe mapping
- `docs/ref-command-pacing.md` — queueing and arbitration model
- `docs/ref-esp32-boot.md` — boot/provisioning/reconnect behavior
- `docs/ref-esp32-logging.md` — diagnostics/logging design
- `docs/ref-ezbeq.md` — ezBEQ compatibility and phase 2 strategy

Extracted assets referenced by those docs live in:
- `app/MiniDSP Device Console.app/`
- `app/app-extracted/`
- `app/apk-extracted/`

Coverage snapshot regeneration:
- `uv run python tools/extract_device_support_matrix.py`

Do not restate detailed extracted tables or reverse-engineering status in this file. Update the `docs/ref-*` files instead.

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

Also: minidsp-rs serves ReDoc at `/api` and OpenAPI schema at `/openapi.json`. We serve the same plus `llms.txt` at webapp root for AI/LLM discoverability.

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

Compatibility contract for all public daemon/CLI/network surfaces lives in `docs/ref-compatibility.md`. Do not redefine endpoint semantics in this file.

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

Intended once config read-back is fully verified across the relevant transport paths:
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

Modification tokens (EEPROM `0xFFC8`, 4 × `u32` per preset) are useful freshness markers only when the controlling client explicitly updates them after preset-content writes. On the tested DSP100 / 2x4 HD, raw runtime routing/bypass/compressor DSP writes do not auto-advance these EEPROM tokens. The official Device Console session log shows the desktop app comparing device/XML tokens and then writing a fresh active-preset token after audio-proc updates, and the desktop daemon now mirrors that behavior for `/config` and measurement-mode preset mutations. The same model also makes the official app's saved `setting*.xml` files useful for fresh daemon recovery: if the XML `<timestamp>` matches the device token for that slot, the daemon can import that preset snapshot as current controller-side state. Use tokens for cache invalidation and multi-client coordination only with that explicit-write model in mind.

## Resolved Technical Questions

### picoserve WebSocket: YES (but not used)
picoserve v0.18.0 supports WebSocket, but we switched to EspHttpServer (ESP-IDF httpd) due to picoserve compatibility issues. EspHttpServer's `ws_handler` provides WebSocket with detached sender for push-based streaming. For the compat endpoint on `/devices/0` (dual GET + WS on same path), we use raw FFI to detect upgrade requests and send HTTP responses from within a ws_handler callback.

### Embassy + esp-idf interop: WORKS but not used
Research confirmed `embassy-executor` works with `esp-idf-svc`, but we went with blocking std instead. EspHttpServer runs handlers in a FreeRTOS task pool — simpler than async, and the USB HID bus is inherently blocking (Mutex-serialized `command()` API). 167KB heap free at steady state proves blocking approach has plenty of headroom.

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

Canonical source:
- `docs/ref-legacy-devices.md`

That file owns:
- launch inventory
- clean-room source-data notes
- probe mapping
- deferred legacy devices
- rough effort estimates

Do not duplicate the legacy inventory in `PROJECT.md`.

| Task | Time |
|------|------|
| Config.xml parser (shared, simple XML) | 2 hours |
| Dialect layer (5.23 fixed-point codec + 2-byte addr) | 2-3 hours |
| 6 legacy device definitions (extract from config.xml) | 3-6 hours |
| Wire up 2x4 non-HD from existing symbols | 1-2 hours |
| nanoAVR/HDA basic support stubs | 30 min |
| Probe entries for all hw_ids | 30 min |

## Command Queue Architecture

Canonical source:
- `docs/ref-command-pacing.md`

This file owns:
- measured USB timing
- queue priorities
- arbitration across HTTP/TCP/WebSocket/CLI paths
- batch semantics
- level polling behavior
- rationale vs minidsp-rs

Keep only the high-level requirement here: single executor per physical device, with all command paths serialized through the queue.

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
| DspKnown | Known dsp_version | + Full PEQ/crossover/delay/compressor/FIR/metering, config inspection |
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

No HA custom component at launch. The intended compatibility surface is the existing `minidsp-rs-hass` WebSocket integration; current implementation and runtime-validation status are tracked in `PROGRESS.md` and `docs/todo-checker.md`.

Future HA integration could use `_minidsp._tcp` zeroconf for auto-discovery. MQTT discovery (phase 2) would provide native HA entities without a custom component. Neither is needed for launch — the existing ecosystem works as-is.

## ESP32 Boot Sequence

Canonical source:
- `docs/ref-esp32-boot.md`

This file owns:
- boot flow
- provisioning UX
- WiFi reconnect/backoff policy
- data partition state machine
- graceful degradation rules
- flash-wear analysis

Keep only the project-level principle here: the MiniDSP remains controllable even when catalogue/UI data is stale or unavailable.

## ESP32 Diagnostics

Canonical source:
- `docs/ref-esp32-logging.md`

This file owns:
- logging architecture
- ring buffer sizing
- `/debug/*` endpoints
- SSE/syslog phasing
- desktop/ESP32 logging split

Keep only the project-level requirement here: ship diagnostic infrastructure in release builds.
