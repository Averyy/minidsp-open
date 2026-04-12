# Compatibility Verification Master Checklist

This file is the master verification plan for finishing compatibility testing.

It replaces the earlier compatibility and ESP32/MiniDSP investigation TODOs. Any useful content from those temporary docs should be preserved here, in `PROGRESS.md`, or in the permanent `ref-*` docs.

Current planning note:
- the project now uses the physical 2x4 HD as the live regression case for shared behavior
- broader all-device coverage must come from a repeatable importer/enumerator based on `app/app-extracted` and `app/apk-extracted`
- use this checklist for compatibility verification, and use `PROGRESS.md`, `docs/ref-device-coverage.md`, `docs/ref-devices.md`, and `docs/ref-legacy-devices.md` to track importer-driven support expansion
- if imported matrix assumptions conflict with live 2x4 HD behavior, treat the 2x4 HD result as authoritative until the conflict is explained
- keep `/play` audio-quality investigation separate from routing/control verification so audible sweep bugs do not get misdiagnosed as HID/config bugs
- when judging `/play` audio quality, use the current shared REW/MSO-style default (`5-400 Hz` hybrid sweep with `2 s` low-level dither pre-silence and `2 s` post-silence), not the retired `BRAP`-bundle default

## Exit Criteria

We are done when all of the following are true:

1. Real MiniDSP-attached ESP32 command I/O is verified end to end.
2. Desktop compatibility surfaces are verified across the supported transport paths.
3. Known contract gaps are either fixed or explicitly documented as non-goals in permanent docs.
4. The former ESP32/MiniDSP investigation todo is fully closed.
5. The former compatibility audit todo is fully closed and no longer needed.
6. `docs/ref-compatibility.md` matches the verified behavior exactly.
7. `docs/ref-ezbeq.md` matches the verified behavior exactly and does not imply unverified hardware coverage.
8. Official-app device coverage is enumerated against the current support table, so support claims are backed by extracted-app evidence instead of guesswork.

## Rules For This Checklist

- Every checked item needs evidence: command transcript summary, HTTP response summary, hardware used, and date tested.
- If a check fails, do not hand-wave it into a docs update. Open a concrete issue in `PROGRESS.md`.
- If behavior differs between desktop and ESP32, record both separately.
- If behavior differs between “implemented” and “verified on real hardware”, only the verified claim belongs in permanent docs.

## Evidence Template

For each completed section, record:

- Date:
- Hardware:
- Transport:
- Commands / routes exercised:
- Result:
- Notes / follow-up:

---

## Section 1: Real ESP32 + Physical MiniDSP Validation

This section closes the remaining work from the former ESP32/MiniDSP investigation todo.

### 1.1 Firmware + attach

- [x] Flash current ESP32 firmware to the real target board.
- [x] Confirm the board boots cleanly and exposes HTTP routes in STA mode.
- [x] Confirm the MiniDSP is physically attached and detected by USB host.
- [x] Confirm `GET /devices` reports a reconnect-usable network WebSocket URL.

### 1.2 Probe and follow-on reads

- [x] Probe succeeds against the physical MiniDSP from the ESP32 path.
- [x] EEPROM DSP version read succeeds after probe.
- [x] Serial read succeeds after probe.
- [x] Master status read succeeds after probe.
- [x] `GET /devices/0` returns valid compatibility JSON with live device data.

### 1.3 Simple write / ACK path

- [x] `POST /devices/0` volume change succeeds.
- [x] `POST /devices/0` mute change succeeds.
- [x] `POST /devices/0/preset/{preset}` succeeds.
- [x] `POST /devices/0/source/{source}` succeeds for at least one valid source.
- [ ] Returned POST body matches compatibility expectations, including `dirac: null` where appropriate.

### 1.4 Raw transport checks

- [x] Raw WebSocket bridge `/devices/0/ws` accepts a framed command and returns a valid framed response.
  Verified on 2026-04-09 via `minidsp --daemon-url http://127.0.0.1:5380 debug send 250000` and `250100`, which both returned `Ack` and changed preset readback immediately.
- [ ] TCP bridge on port `5333` accepts a command and returns a valid response.
- [ ] UDP discovery on `3999` advertises correctly while the real MiniDSP is attached.

### 1.5 PEQ and config checks

- [x] A small PEQ write succeeds through desktop `/devices/{id}/config` on real hardware.
- [x] On desktop, the written PEQ is confirmed through the daemon-side PEQ shadow model used by `GET /devices/{id}/config` because live PEQ `0x14` reads still return the all-zero placeholder.
- [x] Output gain/mute/delay/invert writes via desktop `/devices/{id}/config` are verified on hardware.
- [x] On ESP32, `GET /devices/0/config` returns sane typed scalar state plus PEQ shadow/default state on the physical 2x4 HD.
- [x] On ESP32, malformed JSON and empty config POSTs are handled correctly on `/devices/0/config`.
- [x] On ESP32, output gain/mute/delay/invert writes via `/devices/0/config` are verified on hardware.
- [x] On ESP32, output PEQ writes via `/devices/0/config` succeed and are confirmed through the ESP32-side PEQ shadow model used by `GET /devices/0/config`.
- [ ] Input gain/mute writes via `/devices/0/config` are verified on hardware where supported.

### 1.6 ezBEQ-critical ESP32 path

- [ ] An ezBEQ-style batch load succeeds against the real MiniDSP.
- [ ] An ezBEQ-style clear succeeds against the real MiniDSP.
- [ ] The active preset is correct before and after a slot switch during ezBEQ-style writes.
- [ ] Filter application does not land on the wrong preset on devices with slower preset switching.

### 1.7 Stability / reconnect

- [ ] MiniDSP power cycle triggers reconnect and the ESP32 recovers.
- [ ] MiniDSP USB disconnect/reconnect triggers reconnect and the ESP32 recovers.
- [ ] After reconnect, a read command succeeds.
- [ ] After reconnect, a write command succeeds.
- [ ] After reconnect, the raw WS bridge still works.
- [ ] After reconnect, the TCP bridge still works.

### 1.8 WebSocket compatibility path

- [ ] `/devices/0?poll=true` streams state successfully.
- [ ] `/devices/0?levels=true&poll=true` streams levels successfully.
- [ ] Regular `GET /devices/0` still returns HTTP JSON on the same path.
- [ ] Text frames to `/devices/0` receive status JSON as expected.

### 1.9 Closure for old MiniDSP todo

- [ ] All former ESP32/MiniDSP investigation items are verified here.
- [ ] Any remaining failure is narrower than “HID framing issue”.
- [ ] No separate investigation todo is needed anymore.

---

## Section 2: Desktop Compatibility Surface Verification

### 2.1 Direct USB HID

- [x] `minidsp` with no args prints status correctly when no daemon is running.
- [x] `minidsp status` prints status correctly.
- [x] `minidsp -o jsonline` matches the compatibility JSON shape.
- [x] `minidsp -o json` produces pretty JSON.
- [x] `minidsp probe` output format matches expected compatibility line shape.
  Verified on 2026-04-09 directly against the physical 2x4 HD with no daemon running.

### 2.2 File execution semantics

- [ ] `minidsp -f <file>` executes commands in exact file order.
- [ ] No internal reordering occurs in compatibility mode.
- [ ] ezBEQ-style filter file succeeds.
- [ ] ezBEQ-style clear file succeeds.

### 2.3 Command families

- [x] `input {ch} peq {n} set -- ...`
- [x] `input {ch} peq {n} bypass {on|off}`
- [x] `input {ch} mute {on|off}`
- [x] `input {ch} gain -- {dB}`
- [x] `output {ch} peq {n} set -- ...`
- [x] `output {ch} peq {n} bypass {on|off}`
- [x] `output {ch} mute {on|off}`
- [x] `output {ch} gain -- {dB}`
- [x] `config {n}`
  Verified on 2026-04-09 via `minidsp --daemon-url http://127.0.0.1:5380 config 0` / `config 1` with matching before/after `GET /devices/0` preset readback.
- [x] `gain -- {dB}`
- [x] `mute {on|off}`
- [x] `source {name}`
  Verified on 2026-04-09 via `--daemon-url` against the physical 2x4 HD with matching `GET /devices/0` readback after each change.

### 2.4 Extended compatibility commands

- [x] crossover subcommand
  Verified on 2026-04-09 on the physical 2x4 HD:
  CLI `output 2 crossover set 0 ...` changed all four group-0 biquads on the live device, `GET /devices/0/config` read back the new coefficients immediately, and the original group was restored through `/config`.
- [ ] compressor subcommand
  Updated 2026-04-09 official-app capture finding: Device Console uses plain `0x13` writes on the physical 2x4 HD — output-0 addresses `bypass=22`, `threshold=40`, `ratio=42`, `attack=44`, `release=45`. The interleaved `0x14 0x0046 0x01` polls are compressor-meter reads, not authoritative parameter readback.
  Updated 2026-04-10 protocol RE finding (CLOSED as test-methodology issue): multi-angle RE (Device Console JS source, APK, Wi-DG, all captures) confirmed compressor ratio/attack/release write as raw float32-LE via `0x13` ctrl=`0x80` — same encoding this repo already uses. "No change in value" session log messages = Device Console in-memory cache comparison, not hardware readback. Most likely prior test failure: default threshold `-30 dBFS` is too quiet to trigger audible compression on typical test signals. Protocol gap is closed. **Remaining item**: audible retest on loud material with threshold set to trigger; if audible compression is confirmed, check this item.
  Updated 2026-04-09 daemon finding: `POST /devices/0/config` changed output-0 compressor state on the physical 2x4 HD and a fresh daemon restart still returned the same values via token-gated scalar shadow restore.
  Updated 2026-04-10 recovery-model finding: the daemon can now import the matching official Device Console `setting*.xml` snapshot for missing preset shadow when its `<timestamp>` matches the hardware token.
- [x] FIR subcommand
  Verified on 2026-04-09 on the physical 2x4 HD:
  desktop `POST /devices/0/config` with output FIR coefficients returned `200`, raw coeff readback at `4183..` matched the requested six-tap pattern, and same-client `GET /devices/0/config` reported `active_taps: 6` with `bypass: false`.
  Updated 2026-04-09 desktop finding: fresh-daemon `/config` now restores FIR `active_taps` across reconnects from persisted verified selector shadow keyed by serial, while authoritative live DSP100 tap-count readback still returns `0`. Fresh-client FIR/compressor bypass still decodes correctly as `1 = bypassed`, `2 = enabled`.
  Updated 2026-04-10 tap-count model (RE confirmed): Device Console never reads hardware address `0x0054` for tap count. Configured taps come from local XML `_Taps`. Available tap capacity queried via opcode `0x39` (`GetNumFirTaps`) during FIR writes only (not startup). Hardware low-address tap-count slot returns garbage on DSP100 — this is a confirmed hardware limitation, not a protocol gap.
- [x] routing subcommand
  Verified on 2026-04-09 via `minidsp --daemon-url ... input 0 routing 2 0 --off` / restore with matching `GET /devices/0/config` before/after readback on the physical 2x4 HD.
  Updated 2026-04-09 desktop finding: fresh-daemon `/config` now restores routing state across reconnects from persisted verified selector shadow keyed by serial. Authoritative live DSP100 routing readback is still unavailable; a full readable-DSP-space diff found no routing mirror outside the collapsed `6..13` words, and EEPROM modification tokens at `0xFFC8` stayed unchanged across raw runtime routing writes.
  Updated 2026-04-09 controller-model finding: official Device Console and the desktop daemon both explicitly save a fresh active-preset token after preset-content updates, so unchanged `0xFFC8` during raw routing writes does not contradict the later token-gated shadow restore model.
  Updated 2026-04-09 official-app capture finding: the remaining `0x06` / `0x07` traffic is the preset-save flash-block pipeline, not a hidden routing/compressor runtime-readback channel.
  Updated 2026-04-10 recovery-model finding: the daemon can now also recover missing preset routing/FIR/compressor shadow from the official Device Console `setting*.xml` cache when the XML timestamp matches the device token for that slot; this is stronger than stale cache but still controller-side reconstruction.
- [x] debug subcommand
  Verified on 2026-04-09 via `debug id`, `debug send`, `debug dump-word`, and `debug dump-float` on the physical 2x4 HD.
- [ ] `--force-kind`
- [ ] deprecated `--force-device`
- [ ] deprecated `--force-dsp`

### 2.5 `--url` compatibility paths

- [x] `--url tcp://host:port`
- [x] `--url ws://host:5380/devices/{id}/ws`
- [x] `--url http://host:5380`
  Verified on 2026-04-09 with `status` and source switching against the local desktop daemon on the physical 2x4 HD.
- [x] `--url usb://path`
  Verified on 2026-04-09 directly against the physical 2x4 HD with the emitted macOS HID path form `usb://DevSrvsID%3A1234567890?vid=2752&pid=0011`.
- [ ] `--url usb:%2Fpath?...` probe-style URL

### 2.6 Auto-discovery / default behavior

- [x] On Unix, default CLI path uses `/tmp/minidsp.sock` first when daemon is available.
- [x] On Unix, default CLI falls back to direct USB when the socket is absent or unresponsive.
- [x] `MINIDSP_URL`, `MINIDSPD_URL`, `MINIDSP_SOCK`, `MINIDSP_TCP`, `MINIDSP_USB`, `MINIDSP_INDEX`, and `MINIDSP_LOG` behave as documented.
  Verified on 2026-04-09 on the physical 2x4 HD:
  `MINIDSP_URL=tcp://127.0.0.1:5333`, `MINIDSPD_URL=http://127.0.0.1:5380`, `MINIDSP_SOCK=/tmp/minidsp.sock`, `MINIDSP_TCP=127.0.0.1:5333`, `MINIDSP_USB=2752:0011`, `MINIDSP_USB=2752`, `MINIDSP_INDEX=0/1`, and `MINIDSP_LOG=/tmp/minidsp-client.log`.

### 2.7 `--all-local-devices`

- [ ] Verified with 2+ local USB devices attached.
- [ ] Confirms fan-out across local HID devices.
- [ ] Confirms current limitation to local USB only is understood and documented.

---

## Section 3: Desktop Daemon HTTP / WS / Unix Socket Verification

### 3.1 Device list and URL reporting

- [x] `GET /devices` returns the real transport URL.
- [x] Device list ordering is stable and sorted as expected.
- [x] Product name, hw_id, dsp_version, and serial are correct.
  Verified on 2026-04-09 on the physical 2x4 HD: `GET /devices` returned a single stable entry with probe-style `usb:` transport URL, product name `2x4HD`, and matching version tuple (`hw_id=10`, `dsp_version=100`, serial `912345`).

### 3.2 Compatibility routes

- [x] `GET /devices/{id}`
  Re-verified on 2026-04-10: `GET /devices/0` remained fast and returned HTTP `200` with the correct master state and `-140.0` input/output levels after the desktop daemon's best-effort meter fallback for misbehaving DSP100 meter reads.
- [x] `POST /devices/{id}`
  Verified on 2026-04-09 on the physical 2x4 HD with the compatibility bare `MasterStatus` request body (`{"volume":-1.0}`, `{"mute":true}`, `{"source":"Usb"}`, `{"preset":1}`), not a wrapper object.
- [x] `POST /devices/{id}/config`
  Verified on 2026-04-09 on the physical 2x4 HD across both input rows, outputs `2` and `3`, and all eight routing edges with before/after readback through `GET /devices/0/config`.
- [x] `GET /devices/{id}/config`
  Re-verified on 2026-04-10: output-0 compressor `knee` now falls back to `0.0` and output-0 FIR `active_taps` now fall back to `0` instead of surfacing bogus low-address DSP100 words when no verified shadow is present after a fresh daemon restart. The same fresh-daemon run also recovered sane routing/compressor/FIR state on presets `2` and `3`, confirming the token-matched official XML import path for missing preset shadow.
- [x] `POST /devices/{id}/volume/{direction}`
- [x] `POST /devices/{id}/source/{source}`
- [x] `POST /devices/{id}/preset/{preset}`
  Re-verified on 2026-04-10 on the physical 2x4 HD: verified preset-switch surfaces now intentionally wait for master-status confirmation instead of returning on the transport ack. `POST /devices/0/preset/2` completed in `5.02 s`, `minidsp --daemon-url http://127.0.0.1:5382 config 3` completed in `5.37 s`, `minidsp --url tcp://127.0.0.1:5333 config 1` completed in `5.38 s`, and compatibility `POST /devices/0 {"preset":0}` completed in `5.32 s`.
- [x] `POST /devices/{id}/preset/copy`
  Re-verified on 2026-04-10 on the physical 2x4 HD: `POST /devices/0/preset/copy` returned `200` in `23.02 s`, matching the official Device Console `0x27` timing (`2026-04-08 15:14:04.417` send -> `2026-04-08 15:14:27.284` OK), and the route correctly returned an empty success body (`Content-Length: 0`). Re-verified again on 2026-04-11 on the physical 2x4 HD after wiring the new `copy_preset_verified` path: `POST /devices/0/preset/copy` returned `200 OK`, and a direct `dump-flash 0xFFA3 1` reads back `0x00` immediately afterwards (well below the `0x0D` Device Console failure threshold), confirming the verifyProgram round-trip mirrors `userPreset.js verifyProgram()` and that the route only bumps tokens on a clean save.

### 3.3 Response-shape checks

- [x] `GET /devices/{id}` omits absent `dirac`.
- [x] `POST /devices/{id}` includes `dirac: null` when absent.
- [x] JSON source names are PascalCase.
- [x] URL path source names remain lowercase.

### 3.4 WebSocket compatibility endpoint

- [x] `/devices/{id}?poll=true`
- [x] `/devices/{id}?levels=true&poll=true`
- [x] initial status is sent immediately on connect
- [x] level polling and state polling both work without breaking write operations
  Re-verified on 2026-04-10 on the physical 2x4 HD: manual WebSocket handshakes showed `/devices/0?poll=true` returning an immediate status frame and `/devices/0?levels=true&poll=true` returning an immediate state/levels frame with `2` input levels and `4` output levels. Sequential live reruns on the same daemon/build still showed `POST /devices/0/source/Usb` / `/source/Analog` completing in about `5-8 ms`; preset-switch surfaces now wait for the full hardware apply window (`~5 s`) rather than the early transport ack.

### 3.5 Raw frame bridge

- [x] `/devices/{id}/ws` accepts framed binary commands
- [x] binary response framing is correct
- [x] text behavior does not break compatibility clients
  Re-verified on 2026-04-10 on the physical 2x4 HD: `/devices/0/ws` ignored an incoming text frame and still returned the correct framed binary `ReadHardwareId` response.
- [x] slow raw state commands survive the daemon bridge timeout budget
  Re-verified on 2026-04-10 on the physical 2x4 HD: `minidsp --daemon-url http://127.0.0.1:5382 debug send 03` returned framed `aa` in about `6.7 s`. The live daemon log showed `HID OUT payload: [03]`, then timeout fallback into master-status polling, then an undecodable `ff f1 ...` packet before normal `0xFFD8` reads recovered, so the bridge no longer depends on the device returning a decodable raw `0xAA`.

### 3.6 Unix socket

- [x] CLI preflight via `--daemon-sock` works.
- [x] direct daemon discovery over Unix socket works.
- [x] WebSocket upgrade over Unix socket works.
  Re-verified on 2026-04-10 via `minidsp --daemon-sock /tmp/minidsp.sock status` against the local desktop daemon on the physical 2x4 HD.

### 3.7 Daemon URL

- [x] CLI preflight via `--daemon-url` works.
- [x] direct WS connect through daemon URL works.
  Re-verified on 2026-04-10 by live `config 3`, raw `debug send 31`, and raw `debug send 03` against the physical 2x4 HD.
- [x] device index validation works.
  Verified on 2026-04-09 via `MINIDSPD_URL=http://127.0.0.1:5380 MINIDSP_INDEX=1 minidsp status`, which correctly returned `device index 1 out of range (found 1 device)`.

---

## Section 4: TCP Bridge / Device Console Coexistence

### 4.1 Basic bridge behavior

- [ ] TCP bridge accepts one compatibility client.
- [ ] Second TCP client is rejected as expected.
- [ ] Frame handling works in normal length-prefixed mode.
- [ ] WI-DG-style fixed-size packet mode works.

### 4.2 Device Console compatibility

- [x] Device Console discovers the daemon/device through UDP discovery.
  - Verified 2026-04-10 on macOS: the official MiniDSP Device Console (patched capture build in `app/MiniDSP Device Console.app/`) displayed our `minidsp server` daemon as a network device at the Mac's LAN IP with matching serial `12345`. Our UDP broadcast packet format is close enough to the real hardware's format that the app treats it as a legitimate network-attached 2x4 HD. See `app/minidsp-snapshots/20260410-firstattach-experiment/device-console-firstattach.jsonl`.
- [ ] Device Console connects through the TCP bridge.
  - **Wire-format fix landed 2026-04-10** (`minidsp/src/queue.rs::execute_queued_command`, `app-esp32/src/device.rs::raw_command`, helper `protocol/src/commands.rs::is_write_command`). Python TCP client verification on the physical 2x4 HD confirmed write-ack response sizes are now correct (`WriteDSP` 64→9, `SetVolume` 64→3, `SetMute` 64→3) and the specific `Unexpected DSP command code 1` crash signature can no longer be produced by our TCP bridge. Tests added in `minidsp/src/queue.rs`: `write_dsp_response_trimmed_to_request_length`, `read_floats_response_not_trimmed`.
  - **Still unchecked:** the actual official Device Console Electron app has not yet been exercised against the fixed daemon. Launch the daemon, open the app, click the network device entry, confirm the connection completes without a JS error.
- [ ] Device Console can perform normal interactive operations through the bridge.
  - **Required once the basic connect succeeds:** read master status, change volume, toggle mute, switch source, switch preset, view routing matrix, view/edit PEQ bands, view/edit compressor params. Record each via the HID capture log (`MINIDSP_DC_HID_LOG=/tmp/minidsp-bridge-verify.jsonl`) and confirm the app does not crash or show state corruption.

### 4.3 Coexistence / arbitration

- [ ] Device Console over TCP plus HTTP status reads coexist correctly.
- [ ] Device Console over TCP plus HTTP writes coexist correctly.
- [ ] Device Console over TCP plus levels polling coexist correctly.
- [ ] No response cross-wiring or obvious queue corruption occurs.

---

## Section 5: UDP Discovery Verification

### 5.1 Desktop daemon

- [ ] Broadcast advertisement works.
- [ ] Multicast advertisement works.
- [ ] `probe --net` discovers the daemon/device.

### 5.2 ESP32 without MiniDSP attached

- [ ] Fallback bridge identity is advertised.
- [ ] `probe --net` discovers the live board in no-device mode.

### 5.3 ESP32 with MiniDSP attached

- [ ] Real device identity is advertised correctly.
- [ ] Reported hw_id, dsp_version, serial, IP, and hostname are correct.

---

## Section 6: `/config` Contract Verification

This section is mandatory before deleting the compatibility todo.

### 6.1 Desktop `/config`

- [x] Desktop `GET /devices/{id}/config` returns read-back data that matches verified desktop device/model state on the real 2x4 HD.
- [x] `POST /devices/{id}/config` applies master status fields correctly.
  Verified on 2026-04-09 on the physical 2x4 HD: `/config` changed and restored `volume`, `mute`, `source`, and `preset`, each with matching before/after `GET /devices/0` readback.
- [x] `POST /devices/{id}/config` applies input PEQ/gain/mute correctly.
  Verified on 2026-04-09 on the physical 2x4 HD: input `0` gain, mute, and PEQ band `0` each changed and restored with matching `GET /devices/0/config` readback.
- [x] Desktop `POST /devices/{id}/config` applies routing changes correctly on the real 2x4 HD, with same-client `GET /devices/{id}/config` before/after verification.
- [x] Desktop `POST /devices/{id}/config` applies output PEQ/gain/mute/delay/invert correctly on the real 2x4 HD.
  Verified on 2026-04-09 on the physical 2x4 HD: output `2` gain, mute, delay, invert, and PEQ band `0` each changed and restored with matching `GET /devices/0/config` readback.
- [x] Desktop `POST /devices/{id}/config` applies output FIR coefficients correctly on the real 2x4 HD for the same long-lived client instance.

### 6.2 ESP32 `/config`

- [x] `GET /devices/0/config` exists and is tested, or the gap is explicitly fixed before claiming parity.
- [ ] `POST /devices/0/config` applies master status fields correctly.
- [ ] `POST /devices/0/config` applies input PEQ/gain/mute correctly.
- [ ] `POST /devices/0/config` applies output PEQ/gain/mute/delay/invert correctly.
- [ ] If routing/crossover/compressor/FIR fields are part of the compatibility shape, they are implemented and tested.
  Current desktop state (2026-04-10):
  routing, crossover coefficient writes, and FIR write paths are live-verified on the long-lived daemon client;
  compressor write encoding confirmed correct by RE — opcode `0x13`, raw float32-LE, same as this repo uses; remaining item is audible retest on loud material (test-methodology, not protocol);
  fresh-daemon routing and FIR `active_taps` readback restore from persisted verified shadow keyed by serial;
  authoritative live DSP100 readback for routing and FIR tap count is a confirmed hardware limitation (Device Console has the same limitation).
- [ ] If they are not part of the compatibility shape, `ref-compatibility.md` says so clearly.

---

## Section 7: Negative / Error-Path Verification

- [ ] No-device responses return stable JSON errors.
- [ ] Unknown source names return `400`.
- [ ] Out-of-range device index errors are correct.
- [ ] Invalid JSON returns `400`.
- [ ] Invalid biquad values return `400`.
- [ ] Non-local `--all-local-devices` usage fails with the documented limitation.
- [ ] Unsupported Dirac operations on non-Dirac devices do not break compatibility behavior.

---

## Section 8: Multi-Device / Matrix Verification

- [ ] 2+ local USB devices tested with desktop daemon.
- [ ] Device indexing is stable across CLI and daemon.
- [ ] Equivalent multi-device deployment assumptions from `ref-compatibility.md` are exercised at least once.
- [ ] Any limitation found is recorded in `PROGRESS.md`.

---

## Section 9: ezBEQ-Specific Verification

### 9.1 Desktop binary-swap flow

- [ ] ezBEQ shells out to `minidsp -f` successfully.
- [ ] ezBEQ reads `minidsp -o jsonline` successfully.
- [ ] ezBEQ connects to `/devices/{id}?levels=true&poll=true` successfully.
- [ ] ezBEQ slot switch behavior is correct when preset changes are involved.

### 9.2 ESP32 embedded-ezBEQ flow

- [ ] `/api/1/version`
- [ ] `/api/2/devices`
- [ ] `/api/1/devices/{name}/config/{slot}/active`
- [ ] `/api/1/devices/{name}/mute`
- [ ] `/api/1/devices/{name}/gain`
- [ ] `/api/1/devices/{name}/levels`
- [ ] `/ws` ezBEQ WebSocket
- [ ] UI can load catalogue metadata
- [ ] UI can load a filter
- [ ] UI can clear a filter
- [ ] UI/device state stays in sync with actual MiniDSP state

---

## Section 10: Doc Closure Review

This section must be completed after the technical tests.

### 10.1 Former ESP32/MiniDSP Investigation Todo

- [ ] Every open claim is either verified or superseded.
- [ ] Any remaining useful historical context is copied into a permanent reference doc if needed.
- [ ] No separate temporary todo file is needed.

### 10.2 Former Compatibility Audit Todo

- [ ] Every item under “Remaining Gaps” is either verified or moved to `PROGRESS.md` as an active issue.
- [ ] Every item under “What We Can Honestly Claim Right Now” is updated to match verified reality.
- [ ] No separate temporary todo file is needed once no validation gap remains.

### 10.3 `docs/ref-compatibility.md`

- [ ] Contract matches verified desktop behavior.
- [ ] Contract matches verified ESP32 behavior where parity is claimed.
- [ ] No aspirational or unverified wording remains.
- [ ] Any intentional limitation is called out explicitly.

---

## Current Evidence

### Section 1 baseline

- Date: 2026-04-07
- Hardware: ESP32-S3 DevKitC + physical MiniDSP 2x4 HD (hw_id 10, dsp_version 100, serial 912345)
- Transport: ESP32 USB host HID over WiFi STA at `192.0.2.246:5380`
- Commands / routes exercised:
  `cargo run -p minidsp-app -- probe --net`
  `GET /devices`
  `GET /devices/0`
  `GET /debug/info`
  `GET /devices/0/tokens`
  `POST /devices/0`
  `POST /devices/0/volume/toggle_mute`
  `POST /devices/0/source/toslink`
  `POST /devices/0/preset/1`
- Result:
  Physical attach, probe, EEPROM reads, master status, tokens, and basic write routes all work on real hardware after fixing real-device `ReadFlash`/`ReadFloats` response-shape handling.
- Notes / follow-up:
  `GET /devices/0/config` now exists on ESP32 but is still failing compatibility verification on real hardware. It first returned garbage PEQ/gain values, and after widening the parser search it times out instead of returning verified config data. Keep this open under Section 6.2 and `PROGRESS.md`.

### Section 1 config-read retest

- Date: 2026-04-07
- Hardware: ESP32-S3 DevKitC + physical MiniDSP 2x4 HD (hw_id 10, dsp_version 100, serial 912345)
- Transport: ESP32 USB host HID over WiFi STA at `192.0.2.246:5380`
- Commands / routes exercised:
  `GET /devices/0`
  `GET /devices/0/config`
  serial debug logging of raw USB HID replies during both paths
- Result:
  `GET /devices/0` still returns live status and level data. `GET /devices/0/config` now returns empty PEQ arrays and `null` scalar fields instead of fabricated garbage values.
- Notes / follow-up:
  Desktop direct-USB investigation resolved the model on the same physical 2x4 HD: scalar config slots are typed 32-bit values, modern-device write commands use the official app’s 2-byte address shape on `0x13` / `0x19` / `0x30`, and live PEQ `0x14` reads still return the all-zero placeholder so the daemon now uses a PEQ shadow/default model. ESP32 still needs that corrected behavior ported and verified.

### 10.4 `docs/ref-ezbeq.md`

- [ ] Desktop binary-swap compatibility claims match verified reality.
- [ ] ESP32 embedded-ezBEQ claims match verified reality.
- [ ] The doc does not imply broader real-hardware validation than was actually performed.
- [ ] Any remaining caveat is phrased as a current fact, not hidden in TODO files.

### 10.5 `PROGRESS.md`

- [ ] Checked items remain checked only if verified.
- [ ] Any failed verification is added as a concrete open item.
- [ ] Known issues / deferred section reflects the post-test reality.

---

## Final Go / No-Go

- [ ] Desktop compatibility claim is justified.
- [ ] ESP32 compatibility claim is justified.
- [ ] Device Console coexistence claim is justified.
- [ ] ezBEQ compatibility claim is justified.
- [ ] The old TODO docs can be retired.
