# Compatibility Verification Master Checklist

This file is the master verification plan for finishing compatibility testing.

It replaces the earlier compatibility and ESP32/MiniDSP investigation TODOs. Any useful content from those temporary docs should be preserved here, in `PROGRESS.md`, or in the permanent `ref-*` docs.

Current planning note:
- the project now uses the physical 2x4 HD as the live regression case for shared behavior
- broader all-device coverage must come from a repeatable importer/enumerator based on `app/app-extracted` and `app/apk-extracted`
- use this checklist for compatibility verification, and use `PROGRESS.md`, `docs/ref-device-coverage.md`, `docs/ref-devices.md`, and `docs/ref-legacy-devices.md` to track importer-driven support expansion
- the default executable regression harness now lives at `tools/run-live-validation.py`, with operator notes in `docs/ref-live-validation.md`
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
  Re-verified on 2026-04-11 on the physical 2x4 HD after routing `DeviceState::set_preset` through `set_config_with_verify(preset, PRESET_CHANGE_NORMAL)`: `/devices/0/preset/{0..3}` cycled cleanly with no dropped-completion-frame surface.
- [x] `POST /devices/0/source/{source}` succeeds for every source exposed by the profile.
  Re-verified on 2026-04-11 on the physical 2x4 HD after adding the stale-frame skip loop in `usb_hid::command_with_timeout`: `/devices/0/source/{analog|toslink|usb}` cycled cleanly with no `FlashData([255, 169, N])` surfaces; `/source/spdif` correctly returns `400 source not available on this device`.
- [x] Returned POST body matches compatibility expectations, including `dirac: null` where appropriate.
  Verified on 2026-04-11 on the physical 2x4 HD: `POST /devices/0` responses include `dirac: null`, PascalCase `source`, numeric `volume`, and boolean `mute`/`preset` fields.

### 1.4 Raw transport checks

- [x] Raw WebSocket bridge `/devices/0/ws` accepts a framed command and returns a valid framed response.
  Verified on 2026-04-09 via `minidsp --daemon-url http://127.0.0.1:5380 debug send 250000` and `250100`, which both returned `Ack` and changed preset readback immediately.
  Re-verified on 2026-04-11 on the physical 2x4 HD: sending a `GetMasterStatus` frame `02 31 33` returned a 13-byte framed binary response `0c 31 01 55 0a 00 00 00 00 00 00 00 9d`.
- [x] TCP bridge on port `5333` accepts a command and returns a valid response.
  Verified on 2026-04-11 on the physical 2x4 HD via raw Python socket (`02 31 33` → `0c 31 01 55 0a 00 00 00 00 00 00 00`) and via the desktop `minidsp --tcp 192.0.2.251:5333` CLI round-trip (`probe`, `status`, `source usb`, `source toslink`, `gain -21`, `input 0 peq 0 set -- 1.0 0.0 0.0 0.0 0.0`, `output 0 gain -3.5`).
- [x] UDP discovery on `3999` advertises correctly while the real MiniDSP is attached.
  Verified on 2026-04-11 on the physical 2x4 HD: the 72-byte Wi-DG beacon reported `mac=00:00:5e:00:53:01 ip=192.0.2.251 hw_id=10 fw=1.85 dsp=100 serial_field=0x3039 (12345 via serial_to_u16) hostname=minidsp-000debd9`.

### 1.5 PEQ and config checks

- [x] A small PEQ write succeeds through desktop `/devices/{id}/config` on real hardware.
- [x] On desktop, the written PEQ is confirmed through the daemon-side PEQ shadow model used by `GET /devices/{id}/config` because live PEQ `0x14` reads still return the all-zero placeholder.
- [x] Output gain/mute/delay/invert writes via desktop `/devices/{id}/config` are verified on hardware.
- [x] On ESP32, `GET /devices/0/config` returns sane typed scalar state plus PEQ shadow/default state on the physical 2x4 HD.
- [x] On ESP32, malformed JSON and empty config POSTs are handled correctly on `/devices/0/config`.
- [x] On ESP32, output gain/mute/delay/invert writes via `/devices/0/config` are verified on hardware.
- [x] On ESP32, output PEQ writes via `/devices/0/config` succeed and are confirmed through the ESP32-side PEQ shadow model used by `GET /devices/0/config`.
- [x] Input gain/mute writes via `/devices/0/config` are verified on hardware where supported.
  Verified on 2026-04-12 on the attached ESP32 + physical 2x4 HD: `POST /devices/0/config {"inputs":[{"index":0,"gain":0.0,"mute":true}]}` changed only input `0`, `GET /devices/0/config` read back `gain: 0.0` / `mute: true` for input `0`, input `1` stayed unchanged, and restoring the original input-0 value returned the same pre-test readback.

### 1.6 ezBEQ-critical ESP32 path

- [x] An ezBEQ-style batch load succeeds against the real MiniDSP.
  Verified on 2026-04-12 on the attached ESP32 + physical 2x4 HD via the embedded ezBEQ API: `PUT /api/1/devices/master/filter/2` with `10 Cloverfield Lane` and `PUT /api/1/devices/master/filter/3` with `10 Minutes Gone` both returned `200`, updated `/api/2/devices` slot metadata, and `GET /devices/0/config` on the active preset showed the expected non-bypass input PEQ coefficients on both input channels.
- [x] An ezBEQ-style clear succeeds against the real MiniDSP.
  Verified on 2026-04-12 on the attached ESP32 + physical 2x4 HD: `DELETE /api/1/devices/master/filter/3` and `PATCH /api/3/devices/master {"slots":[{"id":"4","entry":""}]}` both returned `200`, set the targeted slot state back to `Empty`, and `GET /devices/0/config` on the active preset showed the cleared input PEQ bands back at bypassed identity coefficients.
- [x] The active preset is correct before and after a slot switch during ezBEQ-style writes.
  Verified on 2026-04-12 on the attached ESP32 + physical 2x4 HD: filter load into slot `2` set `preset: 1`, activating slot `1` returned `preset: 0`, re-activating slot `2` returned `preset: 1`, loading slot `3` returned `preset: 2`, and patch-loading slot `4` returned `preset: 3`, with `/api/2/devices` and `/devices/0/config` staying aligned on each transition.
- [x] Filter application does not land on the wrong preset on devices with slower preset switching.
  Verified on 2026-04-12 on the attached ESP32 + physical 2x4 HD: slot `2` kept the `10 Cloverfield Lane` coefficients while slot `3` held a different `10 Minutes Gone` filter set, switching between them restored the expected per-slot PEQ state each time, and clearing slot `3` did not alter slot `2`.

### 1.7 Stability / reconnect

- [x] MiniDSP power cycle triggers reconnect and the ESP32 recovers.
- [x] MiniDSP USB disconnect/reconnect triggers reconnect and the ESP32 recovers.
- [x] After reconnect, a read command succeeds.
- [x] After reconnect, a write command succeeds.
- [x] After reconnect, the raw WS bridge still works.
- [x] After reconnect, the TCP bridge still works.
  Verified on 2026-04-12 on the attached ESP32 + physical 2x4 HD `912345` after a live unplug/replug: UART showed the disconnect cleanup, re-enumeration, firmware warmup delay, protocol probe, PEQ shadow restore, and `MiniDspReady` transition; `GET /devices/0` succeeded immediately afterward; `POST /devices/0/preset/1` and `POST /devices/0/preset/0` both completed with matching readback; `./target/debug/minidsp --daemon-url ws://192.0.2.251:5380/devices/0/ws status` succeeded; and `./target/debug/minidsp --tcp 192.0.2.251:5333 status` succeeded. No new UART transport warnings appeared during the post-reconnect WS/TCP checks.
  Re-verified on 2026-04-12 after a live MiniDSP power cycle: the LED fell back to the `MiniDspUnavailable` amber-breathe state when power was removed, and once power returned the ESP32 completed protocol bring-up, restored PEQ shadow from NVS, transitioned back to `MiniDspReady`, and passed the same post-recovery read/write/raw-WS/TCP checks.
  Verified on 2026-04-12 after a manual ESP32 reboot with the MiniDSP left powered on: UART showed WiFi reconnect, HID attach, UAC2 reopen, the then-current ~`9.9 s` firmware wait, protocol probe success on the first attempt, `MiniDspReady`, mDNS/HTTP/TCP startup, and `minidsp-open ready`; that older result is now superseded by the 2026-04-13 reclaim follow-up, which uses the explicit `20 s` attach gate and still recovers cleanly. Follow-up `GET /devices/0`, raw WS `status`, TCP `status`, and reversible preset `1 -> 0` writes all succeeded.

### 1.8 WebSocket compatibility path

- [x] `/devices/0?poll=true` streams state successfully.
- [x] `/devices/0?levels=true&poll=true` streams levels successfully.
  Verified on 2026-04-11 on the physical 2x4 HD: three consecutive messages received with `master`, `input_levels[2]`, and `output_levels[4]` fields populated.
- [x] Regular `GET /devices/0` still returns HTTP JSON on the same path.
- [x] Text frames to `/devices/0` receive status JSON as expected.
  Verified on 2026-04-12 on the attached ESP32 + physical 2x4 HD: a browser-side `WebSocket('ws://192.0.2.251:5380/devices/0')` text frame (`"status please"`) returned immediate JSON status with `master`, `input_levels`, and `output_levels`.

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

- [x] `minidsp -f <file>` executes commands in exact file order.
  Verified on 2026-04-12 on the attached ESP32 + physical 2x4 HD via `./target/debug/minidsp --tcp 192.0.2.251:5333 -f <tempfile>`: a batch file that loaded one PEQ coefficient set into preset `1` and a different coefficient set into preset `2` completed cleanly, and follow-up `GET /devices/0/config` readback showed the first set only on preset `1`, the second set only on preset `2`, and preset `0` unchanged.
- [x] No internal reordering occurs in compatibility mode.
  Verified on 2026-04-12 on the attached ESP32 + physical 2x4 HD: the same multi-preset batch file preserved file order across `config 1`, preset-1 writes, `config 2`, and preset-2 writes with no cross-preset bleed or late application onto the wrong slot.
- [x] ezBEQ-style filter file succeeds.
  Verified on 2026-04-12 on the attached ESP32 + physical 2x4 HD: an ezBEQ-style load file containing `config {n}` plus `input {0,1} peq 0 set -- ...` and `bypass off` lines for presets `1` and `2` succeeded over the ESP32 TCP bridge and left the active preset state/readback aligned with the file contents.
- [x] ezBEQ-style clear file succeeds.
  Verified on 2026-04-12 on the attached ESP32 + physical 2x4 HD: the matching clear file (`bypass on`, `mute off`, `gain -- 0.00` for inputs `0`/`1` on presets `1` and `2`) completed cleanly and `GET /devices/0/config` read back `bypass: true`, `mute: false`, and `gain: 0.0` on both presets while preserving the previously written coefficients, matching ezBEQ clear semantics.

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
  Re-verified on 2026-04-10 on the physical 2x4 HD: `POST /devices/0/preset/copy` returned `200` in `23.02 s`, matching the official Device Console `0x27` timing (`2026-04-08 15:14:04.417` send -> `2026-04-08 15:14:27.284` OK), and the route correctly returned an empty success body (`Content-Length: 0`). Current best evidence is that this is the standard MiniDSP flow that copies the active preset to the remaining preset slots, not an arbitrary indexed `src -> dst` API. Re-verified again on 2026-04-11 on the physical 2x4 HD after wiring the new `copy_preset_verified` path: `POST /devices/0/preset/copy` returned `200 OK`, and a direct `dump-flash 0xFFA3 1` reads back `0x00` immediately afterwards (well below the `0x0D` Device Console failure threshold), confirming the verifyProgram round-trip mirrors `userPreset.js verifyProgram()` and that the route only bumps tokens on a clean save. Re-verified again on 2026-04-12 on the attached ESP32 + physical 2x4 HD after removing an ESP32-only response-shape regression: the route still completed in `23.14 s` and once again returned an empty `200` body (`Content-Length: 0`) instead of JSON.

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
  Re-verified on 2026-04-10 by live `config 3`, raw `debug send 31`, and raw `debug send 03` against the physical 2x4 HD. Re-verified again on 2026-04-12 against the attached ESP32 + physical 2x4 HD with `cargo +esp run -p minidsp-app -- --daemon-url ws://192.0.2.251:5380/devices/0/ws status`; the command still returned the expected live status, and after teaching the shared WS transport / CLI shutdown path to send a close frame, the ESP32 no longer logged the false-positive `socket FD invalid` warning on disconnect.
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
- [x] Device Console connects through the TCP bridge.
  - **Wire-format fix landed 2026-04-10** (`minidsp/src/queue.rs::execute_queued_command`, `app-esp32/src/device.rs::raw_command`, helper `protocol/src/commands.rs::is_write_command`). Python TCP client verification on the physical 2x4 HD confirmed write-ack response sizes are now correct (`WriteDSP` 64→9, `SetVolume` 64→3, `SetMute` 64→3) and the specific `Unexpected DSP command code 1` crash signature can no longer be produced by our TCP bridge. Tests added in `minidsp/src/queue.rs`: `write_dsp_response_trimmed_to_request_length`, `read_floats_response_not_trimmed`.
  - **RE update 2026-04-13:** the app's actual TCP parser lives in extracted Electron JS, not the tiny native launcher. The confirmed network contract for the preset-save path is:
    - `0x06 WriteFlashBlock` success = `[0x01]`
    - `0x07 FinishFlashBlock` success = `[0x02, block_id]`
    - `0x25 ChangePreset` reset mode `0x00` = `[0x01]`, reset modes `0x01`/`0x02` = `[0x02, 0xAB]`
  - **Bridge follow-up landed 2026-04-13:** shared helper `protocol/src/packet.rs::encode_device_console_tcp_response` now enforces those exact reply shapes; both the daemon TCP bridge and the ESP32 TCP bridge use it; `cargo +esp test -p minidsp-protocol --lib`, `cargo +esp test -p minidsp-daemon --lib`, and `cd app-esp32 && esp cargo build` passed after the change.
  - **Live-verified 2026-04-13:** after reflashing the attached ESP32-S3 board `firmware-board`, the stock macOS Device Console connected to the network device and completed `Initialize to Default Settings` end to end with no JS error and no error dialog. The app log showed both `GetFinishFlashBlockDspCmd` sends completing, then `Reloading Preset 1...`, `ChangePresetSlotDspCmd sent`, `reloadCurrentPreset: preset-change-completed`, `Reload Preset Completed`, and `Initialize-to-Default completed.`
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

- [x] Real device identity is advertised correctly.
- [x] Reported hw_id, dsp_version, serial, IP, and hostname are correct.
  Verified on 2026-04-11 on the physical 2x4 HD (serial `912345`, hw_id `10`, dsp_version `100`): the 72-byte Wi-DG beacon decoded to `hw_id=10 fw=1.85 dsp=100 serial_field=0x3039 (12345 = serial_to_u16(912345)) hostname=minidsp-000debd9`. Discovery serial now matches the desktop daemon's `discovery::serial_to_u16` after the `dev.serial() as u16` truncation bug was fixed in `app-esp32/src/main.rs::discovery_packet_for_state`. Re-verified on 2026-04-12 with `cargo +esp run -p minidsp-app -- probe --net`, which found `minidsp-000debd9 (192.0.2.251) [hw_id: 10, dsp_version: 100, serial: 912345]`.

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
  Re-verified on 2026-04-11 on the physical 2x4 HD: `GET /devices/0/config` returned a 5744-byte JSON body covering the full input/output PEQ tree.
- [x] `POST /devices/0/config` applies master status fields correctly.
  Re-verified on 2026-04-14 on the physical 2x4 HD: bare compatibility master-status bodies such as `{"volume":-22.5}` belong on `POST /devices/0`, while `POST /devices/0/config` applies the same fields through the structured `{"master_status":{...}}` shape. Both paths updated live master state correctly on the board.
- [x] `POST /devices/0/config` applies input PEQ/gain/mute correctly.
  Covered by the real-device ESP32 retest recorded in `PROGRESS.md`: config read/write for PEQ/gains/routing/delay/polarity/mute was live-verified on the physical 2x4 HD.
- [x] `POST /devices/0/config` applies output PEQ/gain/mute/delay/invert correctly.
  Covered by the same real-device ESP32 retest on the physical 2x4 HD.
- [x] If routing/crossover/compressor/FIR fields are part of the compatibility shape, they are implemented and tested.
  Re-verified on 2026-04-12 on the attached ESP32 + physical 2x4 HD: ESP32 `/devices/0/config` now round-trips the 2x4 HD-exposed crossover/FIR/compressor fields plus input/output gain/mute/delay/invert with matching readback after mutation and restore, and raw desktop `minidsp --tcp` bridge writes now surface immediately in `/devices/0/config` through the same shadow-backed state model the desktop client uses. Routing-polarity remains hardware-dependent and still needs a live bench on a device whose `RoutingMatrix.polarity_base` is populated.

---

## Section 7: Negative / Error-Path Verification

- [ ] No-device responses return stable JSON errors.
- [x] Unknown source names return `400`.
  2026-04-12: ESP32 switched to wildcard `/devices/0/source/*` parsing, so unknown source names now return explicit `400` instead of falling through the router; PascalCase and device-specific valid source names are also accepted. Existing live verification on 2026-04-11 already covered the distinct `400 source not available on this device` path for `spdif` on a 2x4 HD.
- [x] Out-of-range device index errors are correct.
  Verified on 2026-04-12 on the attached ESP32 + physical 2x4 HD: `POST /devices/1/peq/reset`, `GET /devices/1/config`, and `POST /devices/99 {"mute":true}` all return JSON `404 {"error":"not found"}` after the ESP32 `/devices/*` mutation fallback was wired in, instead of leaking the ESP-IDF HTML `405` responder.
- [x] Invalid JSON returns `400`.
  Verified on 2026-04-11 on the physical 2x4 HD for ESP32 `POST /devices/0` with body `{bad`.
- [x] Invalid biquad values return `400`.
  Verified on 2026-04-12 on the attached ESP32 + physical 2x4 HD: `POST /devices/0/config` with `{"inputs":[{"index":0,"peq":[{"index":0,"coeff":{"b0":"oops","b1":0,"b2":0,"a1":0,"a2":0}}]}]}` returns `400 {"error":"biquad coefficients must include numeric b0/b1/b2/a1/a2 fields"}`, and `GET /devices/0/config` before/after confirmed no PEQ state change.
- [x] Non-local `--all-local-devices` usage fails with the documented limitation.
  Verified on 2026-04-12: `cargo +esp run -p minidsp-app -- --all-local-devices --daemon-url ws://192.0.2.251:5380/devices/0/ws status` exits with `Error: --all-local-devices only supports local USB discovery in this implementation`.
- [x] Unsupported Dirac operations on non-Dirac devices do not break compatibility behavior.
  Verified on 2026-04-12 on the attached ESP32 + physical 2x4 HD: `POST /devices/0 {"dirac":true}` returned `200` with `{"dirac":null,"mute":false,"preset":0,"source":"Usb","volume":-20.0}`.
- [x] Oversized request bodies return `413`.
  Verified on 2026-04-11 on the physical 2x4 HD for ESP32 `POST /devices/0` with a 2KB JSON body exceeding the 1KB handler limit after migrating the handler to `read_request_body`.

---

## Section 8: Multi-Device / Matrix Verification

- [ ] 2+ local USB devices tested with desktop daemon.
- [ ] Device indexing is stable across CLI and daemon.
- [ ] Equivalent multi-device deployment assumptions from `ref-compatibility.md` are exercised at least once.
- [ ] Any limitation found is recorded in `PROGRESS.md`.

---

## Section 9: ezBEQ-Specific Verification

### 9.1 Desktop binary-swap flow

- [x] ezBEQ-style shell-out to `minidsp -f` succeeds over the live ESP32 TCP bridge.
  Verified on 2026-04-12 on the attached ESP32 + physical 2x4 HD: `./target/debug/minidsp --tcp 192.0.2.251:5333 -f <tempfile>` succeeded for both a multi-preset load file and its matching clear file, with `/devices/0/config` confirming the expected per-preset PEQ/bypass/gain/mute state afterwards.
- [x] `minidsp -o jsonline` over the live ESP32 TCP bridge returns parseable compatibility JSON.
  Verified on 2026-04-12 on the attached ESP32 + physical 2x4 HD: `cargo +esp run -q -p minidsp-app -- --tcp 192.0.2.251:5333 -o jsonline` returned compact JSON with the expected `master`, `input_levels`, and `output_levels` fields.
- [ ] Actual ezBEQ process reads `minidsp -o jsonline` successfully end to end.
- [ ] Actual ezBEQ process connects to `/devices/{id}?levels=true&poll=true` successfully end to end.
- [x] ezBEQ-style slot switch behavior is correct when preset changes are involved.
  Verified on 2026-04-12 on the attached ESP32 + physical 2x4 HD: the shell-out load file switched to preset `1`, wrote the first PEQ set, switched to preset `2`, wrote the second PEQ set, and subsequent preset changes/readback showed the correct filter set on each preset with no wrong-preset application.

### 9.2 ESP32 embedded-ezBEQ flow

- [x] `/api/1/version`
  Verified on 2026-04-11 on the physical 2x4 HD: `{"version":"0.1.0"}`.
- [x] `/api/2/devices`
  Verified on 2026-04-11 on the physical 2x4 HD: full `master` + `slots` JSON document with 4 slot objects.
- [x] `/api/1/devices/{name}/config/{slot}/active`
  Verified on 2026-04-11 on the physical 2x4 HD for `PUT /api/1/devices/master/config/{1..4}/active`; `PUT .../config/0/active` correctly returns `405` because ezBEQ slots are 1-indexed.
- [x] `/api/1/devices/{name}` mutations (gain via `PATCH /api/3/devices/master {"masterVolume":...}`, filter delete via `DELETE /api/1/devices/master/filter/{n}`, commands via `PUT /api/1/devices/master/commands`).
  Verified on 2026-04-11 on the physical 2x4 HD.
- [x] `/ws` ezBEQ WebSocket
  Verified on 2026-04-11 on the physical 2x4 HD: initial `Catalogue` message emitted on connect. Re-verified on 2026-04-12 on the attached ESP32 + physical 2x4 HD after switching the handler over to logged send/recv helpers: the browser UI loaded successfully, `/ws` established its detached sender normally, and Playwright saw no browser-console warnings during page load.
- [x] UI can load catalogue metadata
  Verified on 2026-04-12 on the attached ESP32 + physical 2x4 HD: the live board served the ezBEQ React UI at `/`, the browser loaded 14,750 catalogue rows, and same-origin `GET /database.json` succeeded through the ESP32 proxy instead of depending on a published local `manifest.json`.
- [x] UI can load a filter
  Verified on 2026-04-12 on the attached ESP32 + physical 2x4 HD with `10 Cloverfield Lane`: browser `PATCH /api/3/devices/master` returned `200`, slot `1` updated to `10 Cloverfield Lane (aron7awol)`, and `GET /devices/0/config` showed the expected non-bypass input PEQ coefficients.
- [x] UI can clear a filter
  Verified on 2026-04-12 on the attached ESP32 + physical 2x4 HD: browser `DELETE /api/1/devices/master/filter/1` returned `200`, slot `1` returned to `Empty`, and `GET /devices/0/config` showed identity PEQ coefficients with `bypass: true` for the cleared input band.
- [x] UI/device state stays in sync with actual MiniDSP state
  Verified on 2026-04-12 on the attached ESP32 + physical 2x4 HD: browser slot state updated immediately after UI upload/clear, `POST /devices/0/measurement-mode` / `/measurement-mode/restore` preserved the loaded filter state while toggling PEQ bypass in `/devices/0/config`, and compat `POST /devices/0/peq/reset` now clears the active ezBEQ slot plus the browser `DeviceState` view while `/devices/0/config` reflects the reset identity PEQ state.

---

## Section 9.3: ESP32 ↔ desktop daemon route parity gaps

Resolved in code on 2026-04-12. These are no longer missing routes on the
ESP32 firmware; remaining bench verification for HT/BM or advanced `/config`
surfaces is tracked in Section 6.2 / Section 7 rather than here.

- [x] `GET /openapi.json` — ESP32 now serves an OpenAPI JSON document instead
      of falling through to the UI path.
- [x] `GET /devices/{id}/bm` — ESP32 now registers the route and returns
      `501 not supported` on non-HT devices.
- [x] `POST /devices/{id}/bm/channel/{ch}` — implemented on ESP32 with JSON
      patch parsing and `501 not supported` on non-HT devices.
- [x] `POST /devices/{id}/bm/mixer/{row}/{col}` — implemented on ESP32 with
      JSON patch parsing and `501 not supported` on non-HT devices.
- [x] `POST /devices/{id}/peq/reset` — implemented on ESP32.
- [x] `GET /devices/{id}/peq/default-frequencies` — implemented on ESP32.

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
  This early baseline note is now superseded by the later 2026-04-11 and 2026-04-12 ESP32 `/config` verification in Sections 6.2 and 9.2. Keep this block only as historical context for the bring-up sequence, not as the current `/config` status.

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
  This retest note is also historical only now. The shared model discovered here did later land on ESP32, and the live compatibility status for `/devices/0/config` is the verified state recorded in Section 6.2 rather than this earlier pre-fix snapshot.

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
