# CLI Verification Notes

Date: 2026-04-09 .. 2026-04-10

## Definitions

- `verified working`: one command was changed in isolation, the affected state was read back from the live device or daemon, then restored and read back again.
- `partially verified`: the write path executed and some related state changed, but the device does not expose authoritative readback for the exact field.
- `not verified`: the command ran, but the intended end state could not be proven from current readback.

## Test setup

- Device under test: miniDSP 2x4 HD (`hw_id=10`, `dsp_version=100`)
- Transport used for most live checks: direct USB HID with explicit `--usb`
- Readback paths:
  - `minidsp status`
  - `minidsp debug dump-word`
  - `minidsp debug dump-float`
  - desktop daemon `GET /devices/0/config` where that endpoint exposes the field cleanly

## Verification method

- Treat the physical 2x4 HD as the golden device when docs or assumptions disagree.
- Use desktop direct-USB first so shared protocol/client fixes can be isolated without ESP32 variables.
- Bracket every write with read-before / write / read-after verification.
- Prefer the public compatibility surface (`GET /devices/0`, `GET /devices/0/config`, `POST /devices/0`, `POST /devices/0/config`) over CLI-only success criteria.
- For per-channel gain on the physical 2x4 HD, use tolerance-based comparison instead of exact float equality; the live device currently quantizes a requested `-1.5 dB` to about `-1.638082 dB`.

## Verified working

- `probe`
- `status`
- `debug id`
- `debug send`
- `debug dump-word`
- `debug dump-float`
- master `gain`
- master `mute`
- master `source`
- master `config`
- input mute
- input gain
- input PEQ set
- input PEQ bypass
- output mute
- output gain
- output invert
- output delay
- output PEQ set
- output PEQ bypass
- output crossover coefficient writes
- output crossover bypass
- output FIR load
- input routing enable / disable
- output FIR coefficient load on the desktop daemon path
- desktop daemon routing enable / disable through `POST /devices/0/config` + `GET /devices/0/config` on the same long-lived client

## Partially verified

- routing gain

This changed and restored, but the live readback is quantized on this device. Example: `-3 dB` read back at about `-3.059 dB`.

- output FIR active tap count across reconnects

The live DSP100 tap-count slot still reads back as `0` on the tested 2x4 HD, but the desktop daemon now persists verified selector shadow by serial number, so a fresh daemon/client restart still reports the last verified `active_taps` correctly from `/config`.

## Manual or audio verification still required

- direct CLI routing enable / disable (`input <n> routing <out> <gain_db> [--off]`) across reconnects
- PEQ bypass
- PEQ clear bypass state
- crossover bypass

Reason: the current DSP100 runtime readback is not authoritative for these paths. Selector-style status reads collapse on the tested device:
- routing status addresses `6..13` read back as non-zero regardless of enabled vs disabled
- FIR/compressor bypass status addresses on the tested 2x4 HD now read back as `1 = bypassed`, `2 = enabled`; fresh-client desktop `/config` decodes those correctly after the 2026-04-09 fix
- a direct `0x02` read experiment did not produce a usable authoritative reply on the physical 2x4 HD; it echoed the request payload unframed and then returned trailing garbage bytes
- PEQ bypass state is currently tracked in client shadow state rather than reliable direct device readback
- desktop daemon routing state is now restored cleanly across process restarts because verified selector shadow is persisted by device serial
- desktop daemon FIR tap count is now restored cleanly across process restarts for the same reason, even though the live DSP100 tap-count slot still reads back as `0`

## Not verified cleanly / likely bugs

- output compressor parameter writes
  - the official Device Console capture confirms the real desktop app uses the same plain `0x13` DSP100 write family we use for compressor edits
  - captured output-0 compressor writes map cleanly to `bypass=22`, `threshold=40`, `ratio=42`, `attack=44`, `release=45`
  - the same capture only shows repeated `0x14 0x0046 0x01 -> 0.0` polls around those writes, i.e. compressor meter reads, not authoritative parameter readback
  - the companion official app session log also repeatedly reports `setItemVal(COMP_3_0_*) has no change in value` around those edits, which matches the live suspicion that this is not an opcode-family mismatch
  - threshold changed in the live readback used during testing, but ratio / attack / release still did not show the expected changes there
- direct PEQ coefficient verification
  - raw direct reads at the PEQ addresses returned zeros during the test run, so the write could not be proven from that path alone

## minidsp-rs comparison

- Intended compatibility surface: CLI/API behavior should be a drop-in replacement for minidsp-rs where documented.
- Confirmed alignment in this repo:
  - routing uses opcode `0x13` with values `1/2`, matching minidsp-rs semantics
  - modern mute uses `1=muted`, `2=unmuted`, matching minidsp-rs semantics
  - modern polarity/invert now uses the upstream integer `0x13` write shape on DSP100 / 2x4 HD (`0` / `1`) while verifying success through logical before/after invert readback instead of raw-word equality
  - output delay now writes via a sample-count helper that matches the working minidsp-rs/on-wire behavior on the live 2x4 HD
  - preset switching now matches the minidsp-rs pacing model on the tested 2x4 HD path: the immediate transport response is enough to proceed, and the high-level client verifies completion by polling master status until the requested preset is active
- Different from minidsp-rs for a documented hardware/tooling reason:
  - DSP100 selector-style runtime readback is not authoritative on this device, so this repo now falls back to client shadow state where possible instead of treating `0x14` words as exact truth
  - fresh-client FIR/compressor bypass is now recovered directly from live selector words on the tested 2x4 HD path: raw `1` means bypassed and raw `2` means enabled
  - fresh-client routing state and FIR `active_taps` are now recovered across desktop daemon restarts from persisted verified shadow keyed by device serial, because the tested 2x4 HD still does not expose authoritative runtime readback for those fields
  - if the local daemon cache is missing a preset, the desktop daemon can now also import the official Device Console `setting*.xml` snapshot for that preset when its XML timestamp matches the live hardware token, so fresh attach can recover controller-side state from the same saved preset corpus the official app uses
  - PEQ bypass/coefficient readback currently falls back to client shadow state when direct live readback is unavailable or returns unusable zeros on the tested path
- Proven equivalent enough for the current desktop daemon path:
  - routing enable/disable state readback on the same long-lived client instance now reflects the last successful `0x13` selector write
- Proven equivalent enough for the current desktop daemon path across restart:
  - fresh-client routing enable/disable state readback across daemon reconnects now restores from persisted verified shadow
  - fresh-client FIR `active_taps` now restores from persisted verified shadow even when the live tap-count slot reads back `0`
- Not yet proven equivalent by live readback in this test pass:
  - PEQ bypass state
  - PEQ coefficient readback on the tested direct-read path
- Clearly suspect in current implementation, with no good compatibility reason identified yet:
  - compressor parameter writes beyond threshold

## Investigation focus

Priority target where current behavior still appears to drift from minidsp-rs without a clear reason:

1. compressor parameter writes
   - current CLI path writes threshold / ratio / attack / release as plain DSP floats
   - upstream minidsp-rs and the extracted official-app compressor path both still use plain `0x13` scalar writes here, so the remaining drift is not explained by a switch back to `0x30`
   - the captured official app session on the physical 2x4 HD explicitly wrote output-0 `threshold=40`, `ratio=42`, `attack=44`, `release=45`, and toggled bypass/status at `22`
   - live readback only showed threshold changing in the test pass
   - a full direct-USB scan of the exposed `0..34959` DSP/config space on 2026-04-09 tightened this further:
     - distinctive raw writes to `COMP_5_0_ratio`, `COMP_5_0_atime`, and `COMP_5_0_rtime` did not appear anywhere in the exposed address space
     - sending `ReloadDspParam` immediately after those writes still left the live compressor block unchanged
     - the same pass still showed `COMP_5_0_threshold` landing correctly, so this is not a generic `0x13` failure
   - direct 2026-04-09 replay with upstream `minidsp-rs` on the same physical 2x4 HD also only moved `threshold`; immediate typed-word readback still left `ratio / attack / release = 0`
  - forcing compressor bypass `off` first did not change that result on the tested hardware
  - desktop daemon `/config` now persists output compressor `bypass`, `threshold`, `ratio`, `attack`, `release`, and `gain` through a fresh daemon restart on the physical 2x4 HD via token-gated scalar shadow restore, so the app/daemon no longer depend on same-client CLI state for compressor visibility
  - the desktop daemon now also mirrors the official app's explicit token-save behavior after preset-content `/config` mutations, so those persisted compressor values are tagged with a fresh active-preset modification token instead of relying on stale EEPROM token state
  - the official app's nearby `0x14 0x0046 0x01` polls decode as compressor meter reads, not a parameter-readback path, so the remaining issue is now narrowed to effect/readback/state recovery rather than opcode selection
  - likely causes to check:
    - the physical 2x4 HD firmware ignores or virtualizes some of the exposed XML-defined slots even though threshold / gain still work
    - authoritative compressor state is still not exposed through the simple `0x14` path, and the decoded official `0x06` / `0x07` preset-save flow does not fill that gap

4. low-address typed reads
   - the physical 2x4 HD returns short `ReadFloats` replies in the selector/status range that are valid for typed-word callers but nonsensical as float payloads
   - before the 2026-04-09 queue fix, the shared raw path dropped those frames as `Unknown`, which made live commands like `debug dump-word 22 2` and `debug dump-word 24 2` time out
   - after the fix, those reads return the real raw words immediately:
     - `22..23` read back `1, 1`
     - `24..25` read back `2, 1`
   - this is why the current desktop verification loop can now bracket selector-style writes with real before/after readback instead of timing out
   - follow-up live daemon checks on 2026-04-10 narrowed the remaining boundary further:
     - low-address reads are still not universally authoritative on a fresh daemon/client; e.g. `debug dump-word 43 1` returned `1`, while FIR tap-count `debug dump-word 84 1` returned the bogus word `0x4A039901`
     - the desktop daemon now treats those slots as non-authoritative for `/devices/0/config`, so compressor `knee` and FIR `active_taps` fall back to shadow or conservative `0` instead of surfacing garbage values

5. CLI env / transport parity
   - live desktop verification on 2026-04-09 confirmed:
     - `minidsp` with no args works directly on the physical 2x4 HD when no daemon is running
     - `minidsp status`, `-o json`, `-o jsonline`, and `probe` all work on direct USB
     - `--url usb://...`, `--url usb:...`, `--url tcp://127.0.0.1:5333`, `--daemon-sock /tmp/minidsp.sock`, and default Unix-socket fallback all work on the physical 2x4 HD
     - `MINIDSP_URL`, `MINIDSPD_URL`, `MINIDSP_SOCK`, `MINIDSP_TCP`, `MINIDSP_USB`, `MINIDSP_INDEX`, and `MINIDSP_LOG` now behave as documented
   - specific 2026-04-09 fixes:
     - `MINIDSP_LOG` now means a transport log-file path again, matching upstream `minidsp-rs`
     - TCP and WS transports now emit frame-level logs to that file, not just HID
     - `--usb` / `MINIDSP_USB` now accept upstream-style `vid[:pid]` selectors like `2752:0011`

6. FIR load sequencing
   - output FIR definitions for DSP100 / 2x4 HD were missing initially; desktop output FIR was therefore incorrectly treated as unsupported until the shared table was fixed
   - the physical 2x4 HD returns `FirLoadStart` size bytes as big-endian; `[0x39, 0x08, 0x00]` means `0x0800 = 2048`, not `8`
   - the physical 2x4 HD returns the `FirLoadEnd (0x3B)` `Ack` seconds after the packet is sent
   - before the 2026-04-09 queue fix, the shared queue treated each 500 ms receive poll timeout as a hard command failure, so FIR load end timed out even though the device later ACKed it
   - a direct 2026-04-09 replay with upstream `minidsp-rs` on the same physical 2x4 HD reproduced the delayed bare `[0x01]` `FirLoadEnd` ack and still timed out there
   - after the queue + parser fixes in this repo, desktop `POST /devices/0/config` FIR writes return `200`, raw coefficient readback shows the requested taps landing on the real device, and `/config` reports `active_taps` correctly both before and after a fresh daemon restart
   - remaining drift:
     - authoritative live tap-count readback still falls back to `0` on the physical 2x4 HD
     - the desktop daemon now masks that hardware limitation with persisted verified shadow so `/config` stays useful across reconnects

2. preset switching
   - the physical 2x4 HD returns an immediate raw `Ack` for `SetConfig`
   - the old shared queue logic waited for a second preset-only completion frame, which caused nested `--daemon-url` timeouts even when the preset had already changed
   - the shared queue now returns the first valid preset response immediately, but `MiniDspClient::set_config()` and the daemon bridges only report success after master-status readback confirms that the requested preset is active
   - live desktop verification on 2026-04-10 on the physical 2x4 HD:
     - `POST /devices/0/preset/2` completed in about `5.02 s`
     - `minidsp --daemon-url http://127.0.0.1:5382 config 3` completed in about `5.37 s`
     - `minidsp --url tcp://127.0.0.1:5333 config 1` completed in about `5.38 s`
     - compatibility `POST /devices/0 {"preset":0}` completed in about `5.32 s`
     - a fresh-daemon `/config` read on presets `2` and `3` still returned sane routing/compressor/FIR state, proving the token-matched official XML import path is seeding the missing preset shadow correctly

4. slow flash/state commands
   - the physical 2x4 HD does not treat every state-changing command like a fast `Ack` path
   - official Device Console evidence:
     - `CopyPreset (0x27 0x00)` was sent at `2026-04-08 15:14:04.417` and only got `Received OK` at `2026-04-08 15:14:27.284`
   - live desktop verification on 2026-04-10 after the reset-bridge follow-up fix:
     - `POST /devices/0/preset/copy` returned `200` in `23.02 s` on the physical 2x4 HD, matching the official app timing instead of failing at the old `10 s` queue timeout
     - the route returns an empty success body (`Content-Length: 0`); earlier harness failures here were test bugs caused by assuming JSON
     - `minidsp --daemon-url http://127.0.0.1:5382 debug send 03` returned framed `aa` in about `6.7 s`
     - the same live daemon log showed `HID OUT payload: [03]`, then `reset response timed out; verifying via master-status recovery`, then an undecodable `ff f1 ...` packet during the reset window, and finally normal `0xFFD8` reads recovering
     - the daemon still answered `GET /devices/0` after that reset, so the slow reset path is now usable through the daemon bridge even when the device never yields a decodable raw `0xAA`

5. websocket compatibility surfaces
   - live desktop verification on 2026-04-10 with the physical 2x4 HD:
     - `/devices/0?poll=true` sent an immediate status frame on connect
     - `/devices/0?levels=true&poll=true` sent an immediate state/levels frame with `2` input levels and `4` output levels
     - clean reruns on the same daemon/build still showed `POST /devices/0/source/Usb` / `/source/Analog` completing in about `5-8 ms`, so source switching remains fast while preset switching intentionally waits for the hardware apply window
     - the official raw capture still explains that timing: `0x25` acked immediately at `2026-04-09T19:30:52.875Z`, but the first follow-up `0x14` status reply only arrived at `2026-04-09T19:30:56.185Z`
   - raw bridge verification on the same daemon:
     - `/devices/0/ws` ignored an incoming text frame and still returned the correct framed binary response to `ReadHardwareId`

3. source switching
   - the physical 2x4 HD emits a stale `ReadFlash`-shaped `0xFFA9` payload after source changes
   - the old queue/parser path could misclassify that stale payload as the reply to the next `ReadFlash 0xFFD8` master-status request, which poisoned follow-up `GET /devices/0` and later source commands
   - the queue now skips mismatched stale responses, and `set_source()` now verifies completion by polling master-status readback
   - live desktop verification on 2026-04-09:
     - `POST /devices/0` must use the compatibility bare `MasterStatus` body shape such as `{"source":"Analog"}` or `{"volume":-1.0}`; wrapper bodies like `{"master":{...}}` are not the compatible request shape
     - `POST /devices/0` with `{"source":"Analog"}` returned `200` and the next `GET /devices/0` showed `source:"Analog"`
     - `POST /devices/0` with `{"source":"Usb"}` returned `200` and the next `GET /devices/0` showed `source:"Usb"`
     - the dedicated `POST /devices/0/source/analog` and `/source/usb` routes also returned `200` with matching follow-up `GET /devices/0` readback

Items that are different but currently have a defensible reason:

- routing readback on DSP100
  - write semantics are aligned to minidsp-rs
  - same-client daemon readback is now good enough for before/after verification
  - a full readable `0..34959` DSP/config diff around a single route toggle found no stronger routing mirror outside the collapsed `6..13` words
  - EEPROM modification tokens at `0xFFC8` also stayed unchanged across raw live routing and bypass writes on the tested path
  - the official app session log shows why: Device Console explicitly rewrites the active preset token after preset-content updates instead of relying on raw runtime DSP writes to advance it
  - fresh-client readback is therefore still limited by hardware behavior on this device, not by a deliberate repo-only semantic change

## Concrete verified route example

- Date: 2026-04-09
- Path: desktop daemon on direct USB HID, physical 2x4 HD
- Sequence:
  - `GET /devices/0/config` showed `Input 1 -> Output 2 = true` and `Input 2 -> Output 2 = true`
  - `POST /devices/0/config` with `{"inputs":[{"index":0,"routing":[{"output":2,"enabled":false,"gain_db":0.0}]}]}`
  - follow-up `GET /devices/0/config` showed `Input 1 -> Output 2 = false` while `Input 2 -> Output 2` remained `true`
- Meaning:
  - the shared `0x13` route write is working on the real 2x4 HD
  - the desktop daemon's same-client selector shadow is now good enough to verify before/after route state changes

- PEQ bypass / coefficient readback
  - current shadow-state fallback is weaker than authoritative hardware readback
  - this should still be improved, but it is currently a tooling/readback limitation rather than a proven command-write mismatch

## Current recommendation

Before doing another broad CLI validation pass:

1. Fix compressor parameter write behavior, or prove the remaining discrepancy is readback-only.
2. Improve authoritative readback coverage for selector-style DSP100 state, or treat those paths as write-shadow/audio-verified only.
   - fresh-client FIR/compressor bypass is now fixed
   - fresh-client routing state and FIR `active_taps` now restore cleanly on the desktop daemon via persisted verified shadow
3. Keep `/play` debugging separate from config/control debugging.

## Pause State

- `0x06` / `0x07` are now decoded as the official preset-save flash-block pipeline, not a hidden authoritative compressor/routing readback path.
- `0x40` is the DSP-version query, not an unknown state opcode.
- Device Console writes compressor edits through the same `0x13` DSP100 slots this repo already uses and still polls only the compressor meter path nearby.
- Device Console also compares XML/device modification tokens and explicitly writes a fresh active-preset token after preset-content updates; the desktop daemon now mirrors that behavior for `/config` and measurement-mode mutations.
- Remaining DSP100 gaps are still authoritative runtime readback for routing/FIR/compressor-style state and compressor parameter effect/readback parity beyond threshold/gain.
