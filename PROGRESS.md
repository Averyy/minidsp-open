# minidsp-open Progress

This file is a task list and status board.

Permanent technical detail belongs in:
- `PROJECT.md`
- `docs/ref-compatibility.md`
- `docs/ref-device-coverage.md`
- `docs/ref-devices.md`
- `docs/ref-legacy-devices.md`
- `docs/generated/device-support-matrix.json`

## Current Status

- Desktop `/devices/{id}/config` is fixed and verified on the physical 2x4 HD.
- ESP32 `/devices/{id}/config` parity is fixed and verified on the physical 2x4 HD.
- Desktop queue reconnect now re-acquires transports on disconnect and replays idempotent pending PEQ batch writes after reconnect.
- Measurement-mode saved PEQ state now persists in daemon state so restore survives client rebuild/reconnect.
- ESP32 WiFi monitor now records disconnect reason codes, distinguishes credential failures from transient AP loss, and exposes `lastWifiDisconnectReason` in `/debug/info`.
- ESP32 `/sync` and `/devices/{id}/play` are live on real 2x4 HD hardware in release firmware: `/play` responds before playback completes, streams the full sweep, and restores the previous source after auto-switching to USB.
- ESP32 `/play` timing is now characterized on real hardware: the original `~13.6s` completion bug was fixed by half-ring chunking, and the remaining `~12.29-12.56s` enqueue-finish metric scales with byte volume at `TX_DONE=0`, which points to app-side enqueue/reporting semantics rather than a UAC2 driver clock defect.
- ESP32 HID and UAC2 reconnect now recover on the physical 2x4 HD across the three important attach cases:
  - unplug/replug while the ESP32 stays up
  - MiniDSP appears later while the ESP32 is already running
  - ESP32 reboots with the MiniDSP off, then the MiniDSP appears later
- The official MiniDSP app established a known-good audible USB-audio baseline on the physical 2x4 HD:
  - preset `2` in the app UI (zero-based `preset: 1` via our API)
  - source `USB`
  - master volume `0 dB`, master mute off
  - visible routing: `Input 1 -> Output 1` at `0 dB`, `Input 2 -> Output 2` at `0 dB`
  - subwoofer test output should use physical output `2` in that preset
- Opening the official app changed the live device into a working-audio state where direct computer playback became the correct comparison baseline.
  - The strongest current evidence points to preset/routing state, not an ESP32 `/play` transport fault:
    - direct Mac USB playback was also silent before the official-app reset
    - after the reset, Mac playback became audible
    - live user testing confirmed the narrower USB-only rule: a physical output stays silent for USB playback until at least one of `Input 1` or `Input 2` is routed to that output
  - It is still not fully proven whether the app only restored preset/routing state or also touched some additional hidden runtime state.
- The modern config compatibility investigation found and fixed more than one bug:
  - DSP meter values on modern devices were already dB-like; our extra `log10()` conversion was wrong
  - ESP32 `POST /devices/{id}/config` body reads could truncate larger JSON payloads
  - modern routing write support was missing from both the desktop daemon and the ESP32 HTTP config path
  - the desktop CLI `input <n> routing <out> <gain_db>` path was writing the wrong gain encoding on float32 devices
  - shared typed `ReadFloats` parsing for modern raw-word slots was missing the 5-byte typed-response header variant, which could skew status/sample-count reads on SHARC/XMOS devices
- DSP100 routing enable/disable is fully working and audio-verified (2026-04-08):
  - both `0x13` (LoadDspParam) and `0x30` (linked-int) work for mixer status writes
  - switched to `0x13` to match minidsp-rs (the known working open-source implementation)
  - enable = value `0x02`, disable = value `0x01` (matches minidsp-rs `WriteInt::ENABLED/DISABLED`)
  - audio-confirmed: enable, disable, single input, both inputs summing, all work correctly
- Additional direct-USB reverse engineering on the physical 2x4 HD resolved a separate transport bug in our code:
  - real HID write acknowledgements can arrive as report-ID-prefixed unframed echoes beginning with `0x01`, not only as length-framed responses
  - the old decoder treated that leading `0x01` as a one-byte empty frame, which made successful direct USB writes look like `device error []`
  - after fixing `protocol/src/packet.rs`, direct USB writes now ACK cleanly and immediate read-back confirms the write actually landed
- DSP100 routing investigation resolved:
  - mixer status addresses `6..13`: enable=2, disable=1, via opcode `0x13` (matching minidsp-rs)
  - readback via `0x14` is boolean-collapsed (any non-zero → 1) — this caused the two-day debugging loop (`status == 2` always returned false)
  - fixed readback to `status != 0` in both desktop and ESP32 code
  - route-gain cells at `32..39` are writable as float dB values
  - the previous `0x30` approach also works but `0x13` is preferred for minidsp-rs compatibility
- Live device testing also confirmed the modern mute polarity on DSP100:
  - `1 = muted`
  - `2 = unmuted`
  - our current float32 mute read/write logic is inverted and must be corrected in shared desktop + ESP32 code
- Modern-device readback issue resolved (2026-04-08):
  - DSP100/2x4 HD mixer status readback via `0x14` is boolean-collapsed: any non-zero value reads as `1`, so `2` (enabled) and `1` (disabled) are indistinguishable
  - fixed `client.rs` readback to use `status != 0` instead of `status == 2` (the old check always returned false since readback never returns 2)
  - for authoritative routing state, the API should eventually read from saved preset flash data rather than runtime DSP parameter readback
- All 13 exact modern Device Console `HW`/`DSP` pairs are implemented and test-gated.
- All 7 cached legacy `config.xml` families are implemented as source-backed runtime tables and test-gated.
- The repeatable official-app importer/report pipeline is in place:
  - `tools/extract_device_support_matrix.py`
  - `docs/ref-device-coverage.md`
  - `docs/generated/device-support-matrix.json`
  - `protocol/src/device/generated_official.rs`

## Active Work

- [ ] Capture or otherwise verify the physical output duration of ESP32 `/play` on the real 2x4 HD now that software timing and repeated-play stability are characterized.
- [ ] Verify remaining `/play` compatibility edges on hardware where possible:
  - malformed request handling
  - sample-rate switch between plays
  - HID-only-device clear error path
- [ ] Prove on hardware that the new shared linked-int `0x30` route-enable write path restores audible USB playback on DSP100 / 2x4 HD without relying on the official app
- [ ] Finish DSP100 / 2x4 HD routing-state reverse engineering so `/devices/{id}/config` read-back matches the official app for mute/routing state, not just audible behavior
- [ ] Resolve exact runtime identity for the remaining official USB-only labels if stronger official evidence is found:
  - `HA-DSP`
  - `DDRC-22`
  - `DDRC-88`
  - `CDSP-8x12/DL`
  - `Harmony DSP`
- [ ] Continue importer/codegen work only where it improves source-backed device identity or typed metadata, not as hand-maintained guesswork.
- [ ] Keep the generated coverage artifacts current whenever device-matrix evidence changes.

## Verified Foundation

- [x] Shared protocol/config fixes validated on the physical 2x4 HD.
- [x] Desktop and ESP32 config write/read behavior aligned for the implemented 2x4 HD path.
- [x] Modern official matrix coverage gated by extracted app assets.
- [x] Modern DSP address tables gated by extracted XML indexes.
- [x] Legacy cached `config.xml` families gated by generated metadata and source-backed runtime tables.
- [x] Shared USB acceptance policy reduced to the official extracted VID/PID set.

## Known Boundaries

- [x] The extracted desktop fallback-model picker is not a second identity catalog.
- [x] `app/MiniDSP Device Console.app/Contents/MacOS/MiniDSP Device Console` is only an Electron bootstrap.
- [x] `app/apk-extracted/classes.dex` is Cordova shell/transport glue, not a richer device-definition source.
- [x] Public website/manual evidence may stabilize product-family and software-family naming, but exact runtime identity still requires extracted app/native evidence.

## Remaining Support Boundary

- [ ] `HA-DSP`
  - Public product family known.
  - Public software family known: `HA-DSP-CF`.
  - Exact runtime identity still unresolved.
- [ ] `DDRC-22`
  - Public product family known.
  - Public software family known: `DDRC-22 Utility`.
  - Source-backed small Dirac runtime bucket known.
  - Exact runtime identity still unresolved.
- [ ] `DDRC-88`
  - Public product family known: `DDRC-88A / DDRC-88D`.
  - Public software family known: `DDRC-88BM`.
  - Source-backed legacy 8-channel Dirac bucket known.
  - Exact runtime identity still unresolved.
- [ ] `CDSP-8x12/DL`
  - Public product family known.
  - Public software family known: `C-DSP 8x12 DL`.
  - Source-backed custom-install 8x12 Dirac bucket known.
  - Exact runtime identity still unresolved.
- [ ] `Harmony DSP`
  - Public product family known: `Harmony DSP 8x12`.
  - Public software family known: `C-DSP 8x12 DL`.
  - Source-backed HT-family 8x12 Dirac bucket known.
  - Exact runtime identity still unresolved.

## Deferred

- [ ] Desktop `POST /play` and `POST /sync` — same API as ESP32, platform audio backend (see `docs/TODO-desktop-audio-sweep.md`)
- [ ] PWR-ICE compatibility verification
- [ ] Multi-device live verification
- [ ] CI for desktop targets
- [ ] CI for ESP32 cross-build
- [ ] ezBEQ expansion after core device-matrix work is stable
- [ ] ESP32 VBUS GPIO drive — not needed on DevKitC boards (both boards powered via UART); only needed for custom PCB where ESP32-S3 must supply 5V to the MiniDSP over the OTG port (GPIO19 high before `usb_host_install`)

## Standard Validation

- `uv run python tools/extract_device_support_matrix.py`
- `cargo test -p minidsp-protocol`
- `cargo test`
