# Compatibility Verification Master Checklist

This file is the master verification plan for finishing compatibility testing.

It replaces the earlier compatibility and ESP32/MiniDSP investigation TODOs. Any useful content from those temporary docs should be preserved here, in `PROGRESS.md`, or in the permanent `ref-*` docs.

## Exit Criteria

We are done when all of the following are true:

1. Real MiniDSP-attached ESP32 command I/O is verified end to end.
2. Desktop compatibility surfaces are verified across the supported transport paths.
3. Known contract gaps are either fixed or explicitly documented as non-goals in permanent docs.
4. The former ESP32/MiniDSP investigation todo is fully closed.
5. The former compatibility audit todo is fully closed and no longer needed.
6. `docs/ref-compatibility.md` matches the verified behavior exactly.
7. `docs/ref-ezbeq.md` matches the verified behavior exactly and does not imply unverified hardware coverage.

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

- [ ] Raw WebSocket bridge `/devices/0/ws` accepts a framed command and returns a valid framed response.
- [ ] TCP bridge on port `5333` accepts a command and returns a valid response.
- [ ] UDP discovery on `3999` advertises correctly while the real MiniDSP is attached.

### 1.5 PEQ and config checks

- [ ] A small PEQ write succeeds through `/devices/0/config`.
- [ ] The written PEQ can be read back or otherwise confirmed from the device.
- [ ] Output gain/mute/delay/invert writes via `/devices/0/config` are verified on hardware.
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

- [ ] `minidsp` with no args prints status correctly when no daemon is running.
- [ ] `minidsp status` prints status correctly.
- [ ] `minidsp -o jsonline` matches the compatibility JSON shape.
- [ ] `minidsp -o json` produces pretty JSON.
- [ ] `minidsp probe` output format matches expected compatibility line shape.

### 2.2 File execution semantics

- [ ] `minidsp -f <file>` executes commands in exact file order.
- [ ] No internal reordering occurs in compatibility mode.
- [ ] ezBEQ-style filter file succeeds.
- [ ] ezBEQ-style clear file succeeds.

### 2.3 Command families

- [ ] `input {ch} peq {n} set -- ...`
- [ ] `input {ch} peq {n} bypass {on|off}`
- [ ] `input {ch} mute {on|off}`
- [ ] `input {ch} gain -- {dB}`
- [ ] `output {ch} peq {n} set -- ...`
- [ ] `output {ch} peq {n} bypass {on|off}`
- [ ] `output {ch} mute {on|off}`
- [ ] `output {ch} gain -- {dB}`
- [ ] `config {n}`
- [ ] `gain -- {dB}`
- [ ] `mute {on|off}`
- [ ] `source {name}`

### 2.4 Extended compatibility commands

- [ ] crossover subcommand
- [ ] compressor subcommand
- [ ] FIR subcommand
- [ ] routing subcommand
- [ ] debug subcommand
- [ ] `--force-kind`
- [ ] deprecated `--force-device`
- [ ] deprecated `--force-dsp`

### 2.5 `--url` compatibility paths

- [ ] `--url tcp://host:port`
- [ ] `--url ws://host:5380/devices/{id}/ws`
- [ ] `--url http://host:5380`
- [ ] `--url usb://path`
- [ ] `--url usb:%2Fpath?...` probe-style URL

### 2.6 Auto-discovery / default behavior

- [ ] On Unix, default CLI path uses `/tmp/minidsp.sock` first when daemon is available.
- [ ] On Unix, default CLI falls back to direct USB when the socket is absent or unresponsive.
- [ ] `MINIDSP_URL`, `MINIDSPD_URL`, `MINIDSP_SOCK`, `MINIDSP_TCP`, `MINIDSP_USB`, `MINIDSP_INDEX`, and `MINIDSP_LOG` behave as documented.

### 2.7 `--all-local-devices`

- [ ] Verified with 2+ local USB devices attached.
- [ ] Confirms fan-out across local HID devices.
- [ ] Confirms current limitation to local USB only is understood and documented.

---

## Section 3: Desktop Daemon HTTP / WS / Unix Socket Verification

### 3.1 Device list and URL reporting

- [ ] `GET /devices` returns the real transport URL.
- [ ] Device list ordering is stable and sorted as expected.
- [ ] Product name, hw_id, dsp_version, and serial are correct.

### 3.2 Compatibility routes

- [ ] `GET /devices/{id}`
- [ ] `POST /devices/{id}`
- [ ] `POST /devices/{id}/config`
- [ ] `GET /devices/{id}/config`
- [ ] `POST /devices/{id}/volume/{direction}`
- [ ] `POST /devices/{id}/source/{source}`
- [ ] `POST /devices/{id}/preset/{preset}`

### 3.3 Response-shape checks

- [ ] `GET /devices/{id}` omits absent `dirac`.
- [ ] `POST /devices/{id}` includes `dirac: null` when absent.
- [ ] JSON source names are PascalCase.
- [ ] URL path source names remain lowercase.

### 3.4 WebSocket compatibility endpoint

- [ ] `/devices/{id}?poll=true`
- [ ] `/devices/{id}?levels=true&poll=true`
- [ ] initial status is sent immediately on connect
- [ ] level polling and state polling both work without breaking write operations

### 3.5 Raw frame bridge

- [ ] `/devices/{id}/ws` accepts framed binary commands
- [ ] binary response framing is correct
- [ ] text behavior does not break compatibility clients

### 3.6 Unix socket

- [ ] CLI preflight via `--daemon-sock` works.
- [ ] direct daemon discovery over Unix socket works.
- [ ] WebSocket upgrade over Unix socket works.

### 3.7 Daemon URL

- [ ] CLI preflight via `--daemon-url` works.
- [ ] direct WS connect through daemon URL works.
- [ ] device index validation works.

---

## Section 4: TCP Bridge / Device Console Coexistence

### 4.1 Basic bridge behavior

- [ ] TCP bridge accepts one compatibility client.
- [ ] Second TCP client is rejected as expected.
- [ ] Frame handling works in normal length-prefixed mode.
- [ ] WI-DG-style fixed-size packet mode works.

### 4.2 Device Console compatibility

- [ ] Device Console discovers the daemon/device through UDP discovery.
- [ ] Device Console connects through the TCP bridge.
- [ ] Device Console can perform normal interactive operations through the bridge.

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

- [ ] `GET /devices/{id}/config` returns read-back data that matches device state.
- [ ] `POST /devices/{id}/config` applies master status fields correctly.
- [ ] `POST /devices/{id}/config` applies input PEQ/gain/mute correctly.
- [ ] `POST /devices/{id}/config` applies output PEQ/gain/mute/delay/invert correctly.

### 6.2 ESP32 `/config`

- [x] `GET /devices/0/config` exists and is tested, or the gap is explicitly fixed before claiming parity.
- [ ] `POST /devices/0/config` applies master status fields correctly.
- [ ] `POST /devices/0/config` applies input PEQ/gain/mute correctly.
- [ ] `POST /devices/0/config` applies output PEQ/gain/mute/delay/invert correctly.
- [ ] If routing/crossover/compressor/FIR fields are part of the compatibility shape, they are implemented and tested.
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
