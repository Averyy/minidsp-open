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
- All 13 exact modern Device Console `HW`/`DSP` pairs are implemented and test-gated.
- All 7 cached legacy `config.xml` families are implemented as source-backed runtime tables and test-gated.
- The repeatable official-app importer/report pipeline is in place:
  - `tools/extract_device_support_matrix.py`
  - `docs/ref-device-coverage.md`
  - `docs/generated/device-support-matrix.json`
  - `protocol/src/device/generated_official.rs`

## Active Work

- [ ] Verify reconnect retry, pending-PEQ replay, and WiFi disconnect-reason handling on a real ESP32-S3 with real MiniDSP attach/detach cycles.
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
