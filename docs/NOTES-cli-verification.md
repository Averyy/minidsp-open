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

## Not verified cleanly / likely methodology issue

- output compressor parameter writes (beyond threshold/gain)
  - **Protocol encoding is confirmed correct** (2026-04-10 RE session, multiple capture angles):
    - All compressor params use opcode `0x13`, ctrl=`0x80` (reloadFlag=true), 3-byte address prefix
    - ratio writes as raw float32-LE ratio value (e.g., `9.9` → `66 66 1e 41`)
    - attack writes as raw float32-LE milliseconds (e.g., `30.0` → `00 00 f0 41`)
    - release writes as raw float32-LE milliseconds (e.g., `220.0` → `00 00 5c 43`)
    - addresses match our `DSP100_OUTPUTS` table exactly
    - Device Console never reads back these addresses via `0x14` after writing; only polls compressor meter at `0x0046`
  - **"no change in value" explained**: Device Console's `userPreset.setItemVal()` compares against its in-memory `singleValues` cache, not live hardware. If the cached value matches what's being set, the hardware write is skipped. This is a DC-internal optimization, not a hardware limitation.
  - **Most likely remaining cause**: the default compressor threshold is `-30 dBFS`, which is too quiet to trigger audible compression on typical test signals. The compressor will only have a measurable effect if the signal exceeds the threshold.
  - **Next step**: audible retest on loud material with compressor enabled (status=2) and threshold set to trigger. This is a test methodology issue, not a protocol investigation item.
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
   - **Protocol is confirmed correct** (2026-04-10 RE — see `docs/NOTES-device-console-hid-logging.md`)
   - All compressor params use opcode `0x13`, ctrl=`0x80`, raw float32-LE values:
     - ratio: direct ratio value (e.g., 9.9 for 9.9:1)
     - attack/release: direct milliseconds (e.g., 30.0 ms)
     - threshold: direct dBFS (e.g., -35.0)
   - Device Console never reads back these addresses after writing; only polls the compressor meter at `0x0046`
   - The "no change in value" session log messages are Device Console's in-memory cache comparison, not hardware readback
   - **Most likely cause of prior test failure**: default threshold `-30 dBFS` is too quiet to trigger compression on the test signal. Compressor effect is only audible when the signal exceeds the threshold.
   - **Status**: protocol gap is closed. Remaining work is an audible retest on loud material — this is a test methodology item, not a protocol investigation item.
   - desktop daemon `/config` now persists output compressor `bypass`, `threshold`, `ratio`, `attack`, `release`, and `gain` through a fresh daemon restart on the physical 2x4 HD via token-gated scalar shadow restore
   - the desktop daemon now also mirrors the official app's explicit token-save behavior after preset-content `/config` mutations

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
   - the physical 2x4 HD returns `GetNumFirTaps (0x39)` response bytes as big-endian; `[0x04, 0x39, 0x08, 0x00]` means `readUInt16BE(2) = 0x0800 = 2048` available taps, not `8`
     - opcode `0x39` is `GetNumFirTaps` — confirmed from `dspMessage.js` RE (2026-04-10)
     - wire format: `[len=0x03, cmd=0x39, channel_byte, checksum]`; response: 4-byte DSP message, tap count at bytes [2..3] uint16BE
     - called before FIR coefficient upload (in `deviceFlash.writeFirTapsToFlash`), NOT during startup
   - the physical 2x4 HD returns the `FirLoadEnd (0x3B)` `Ack` seconds after the packet is sent
   - before the 2026-04-09 queue fix, the shared queue treated each 500 ms receive poll timeout as a hard command failure, so FIR load end timed out even though the device later ACKed it
   - a direct 2026-04-09 replay with upstream `minidsp-rs` on the same physical 2x4 HD reproduced the delayed bare `[0x01]` `FirLoadEnd` ack and still timed out there
   - after the queue + parser fixes in this repo, desktop `POST /devices/0/config` FIR writes return `200`, raw coefficient readback shows the requested taps landing on the real device, and `/config` reports `active_taps` correctly both before and after a fresh daemon restart
   - FIR tap count model (confirmed from Device Console RE, 2026-04-10):
     - Device Console does NOT read hardware address `0x0054` for tap count (never appears in any capture)
     - configured taps come from local XML `_Taps` values in the saved preset/settings file
     - available tap capacity comes from opcode `0x39` (GetNumFirTaps), queried during FIR writes
     - the hardware low-address tap-count slot at index 84 reads back garbage on DSP100 — this is a known hardware limitation
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

1. ~~Fix compressor parameter write behavior, or prove the remaining discrepancy is readback-only.~~ **CLOSED 2026-04-10**: Protocol encoding is confirmed correct by multi-angle RE. Remaining work is audible retest on loud material (threshold must be exceeded).
2. Improve authoritative readback coverage for selector-style DSP100 state, or treat those paths as write-shadow/audio-verified only.
   - fresh-client FIR/compressor bypass is now fixed
   - fresh-client routing state and FIR `active_taps` now restore cleanly on the desktop daemon via persisted verified shadow
3. Keep `/play` debugging separate from config/control debugging.

## 2026-04-10 additions

### TCP bridge write-ack framing fix

- **Wire-format bug fixed.** The official Device Console crashed with `Error: Unexpected DSP command code 1` on every write command when connecting to the daemon via TCP bridge (screen capture in `app/minidsp-snapshots/20260410-firstattach-experiment/`). Root cause: `protocol/src/packet.rs::decode_hid_response` for the `0x01`-prefix unframed-echo case returned `data[1..].to_vec()` — all 63 trailing HID buffer bytes — instead of trimming to the actual echo length. The oversized payload contained stale HID garbage, and the Device Console's parser hit a literal `0x01` byte inside the trailing junk.
- Fix at `minidsp/src/queue.rs::execute_queued_command`: when the request opcode is a write command and the oversized response first byte matches the request opcode, truncate to `cmd.payload.len()`. Same trim in `app-esp32/src/device.rs::raw_command`. Shared `is_write_command` helper added in `protocol/src/commands.rs`.
- Python TCP client verification on the physical 2x4 HD: `WriteDSP` 64→9 bytes, `SetVolume` 64→3 bytes, `SetMute` 64→3 bytes. Read responses unchanged at 12 bytes each. All response length bytes are now self-consistent and all first-payload bytes are valid DSP opcodes.
- Two new unit tests added to `minidsp/src/queue.rs`: `write_dsp_response_trimmed_to_request_length` and `read_floats_response_not_trimmed`.
- **Official-app reset/save path now live-verified (2026-04-13).** The stock macOS Device Console completed `Initialize to Default Settings` against the ESP32 TCP bridge with no error dialog. The log shows both `GetFinishFlashBlockDspCmd` completions, then `Reloading Preset 1...`, `ChangePresetSlotDspCmd sent`, `reloadCurrentPreset: preset-change-completed`, `Reload Preset Completed`, and `Initialize-to-Default completed.` The earlier `GetFinishFlashBlock` disconnect and `ChangePresetSlotDspCmd` failure signatures did not recur. Broader interactive coverage is still tracked in `PROGRESS.md` and `docs/todo-checker.md` Section 4.2.

### DSP100 authoritative-state investigation — CLOSED

- **Item closed.** The 2026-04-10 first-attach experiment with the official Device Console (reference: `app/minidsp-snapshots/20260410-firstattach-experiment/`) definitively answered the open question. When the app has no local cache for a device it offers only "Initialize to default Settings" or "Disconnect". There is no hidden readback opcode path. The official app has the same limitation as this repo: token-matched XML cache, fall back to defaults if no cache.
- DSP state recovery items from the investigation checklist (routing, FIR tap count) can be treated as solved at the current ceiling: persisted verified shadow keyed by device serial.

### Compressor parameter drift — new data point

- **Item stays open** but the address/polarity question is now settled. Direct capture of the Device Console editing Output 1 compressor on a clean-default preset (2026-04-10 session) confirmed our addresses match exactly: threshold=`0x800028`, ratio=`0x80002a`, attack=`0x80002c`, release=`0x80002d`, status=`0x800016` with `2=enabled`/`3=bypassed`. These match `DSP100_OUTPUTS` in `protocol/src/device/dsp.rs` exactly.
- The bypass polarity for compressor status (`2=on`, `3=off`) is already correct in `minidsp/src/client.rs:1109-1110` with a matching comment.
- Likely remaining cause: default threshold `-30 dB` is too quiet to trigger audible compression on the signal used in prior tests. Next step is an audible retest on loud material; if that still fails, reopen protocol/timing investigation rather than assuming the control path is finished.

## Pause State

- `0x06` / `0x07` are now decoded as the official preset-save flash-block pipeline, not a hidden authoritative compressor/routing readback path.
- `0x40` is the DSP-version query, not an unknown state opcode.
- Device Console writes compressor edits through the same `0x13` DSP100 slots this repo already uses and still polls only the compressor meter path nearby.
- Device Console also compares XML/device modification tokens and explicitly writes a fresh active-preset token after preset-content updates; the desktop daemon now mirrors that behavior for `/config` and measurement-mode mutations.
- **DSP100 authoritative-state reverse engineering task is closed (2026-04-10).** In current captures, extracted desktop sources, and current RE, the official app shows the same workaround rule we do: token-matched XML cache or initialize defaults when no cache exists. No stronger readback path has been found so far.
- **TCP bridge write-ack framing wire-format bug is fixed and the official reset/save path is now live-verified (2026-04-13).** Python TCP client confirmed response sizes and first-byte opcodes, and the stock macOS Device Console now completes the `Initialize to Default Settings` flow over the ESP32 TCP bridge without the previous parser/reload failure. Broader interactive coverage remains open.
- Remaining DSP100 gap: compressor parameter effect/readback parity beyond threshold/gain. Addresses, polarity, and encoding are now all confirmed correct by multi-angle RE (2026-04-10); the remaining issue is test methodology (signal too quiet to trigger the compressor), not protocol mismatch. Next step: audible retest on loud material with compressor enabled and threshold set to trigger.
- **New opcode 0x39 = GetNumFirTaps** confirmed from Device Console source RE (2026-04-10). Queried during FIR coefficient upload (not startup). Wire format in NOTES-device-console-hid-logging.md.
- **0xAB = PresetChangeCompleted** async notification after `0x25` with non-zero flag. Payload-less (2-byte logical frame). Handled in our queue already.
- **Routing defaults confirmed**: diagonal pattern (in0→out0=active, in1→out1=active, cross-routes=inactive). Selector word 2=active, 1=inactive.
