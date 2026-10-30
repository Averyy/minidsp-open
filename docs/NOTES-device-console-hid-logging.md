# Device Console HID Logging

Goal: capture the official MiniDSP Device Console HID traffic while changing settings on a real device, so we can compare its exact request/response frames against `minidsp-open`.

## Canonical Capture

The current canonical raw capture is kept in the gitignored local `app/` folder at:

- `app/minidsp-console-app-log.jsonl`

This private working copy is used locally when implementing or validating shared protocol behavior; it is not distributed with the repository. Point-in-time local archive copies belong under:

- `app/minidsp-snapshots/`

The companion high-level app log from an earlier session in this round of RE is:

- `app/minidsp-snapshots/20260409-151113-device-console-session/device-console-main-session.log`

## Shareable capture fixture and replay

The committed, sanitized copy is
[`minidsp/tests/fixtures/device-console-hid.json`](../minidsp/tests/fixtures/device-console-hid.json):
54 labelled exchanges selected from 1,592 requests (13 opcodes), with repeated meter
polls and flash chunks deduplicated. Each exchange records request/reply direction,
64-byte HID report hex, source line ordinals, decoded arguments/expected reply
fields, and the inferred user action. Actions are inferred from the wire fields
and DSP100 address table. The companion app log corroborates command names but
belongs to an earlier session; its timestamps do not identify individual exchanges
in the canonical capture.

Privacy normalization happens before fixture output: host metadata and timestamps
are omitted, serial EEPROM bytes are re-encoded as `30 39` (synthetic serial
912345 minus 900000), and modification tokens use consistent synthetic values.
The full serial is hexadecimal `000debd9`; its low-u16 `ebd9` is **not** the
EEPROM encoding. Request checksums are recalculated. Unused response buffer tails
are replaced with `ff`, including stale bytes after `0xAB` and finish-block
messages. Unframed write acknowledgements retain the echoed command; flash-block
acknowledgements retain seven header/status bytes and normalize the remainder.
These reports are sanitized representatives, not byte-identical private originals.

`cargo test -p minidsp --test device_console_capture` checks:

- Every selected request against the matching command encoder and HID framing,
  including flash reads, token writes, preset values/bypass blocks, scalar/selector/
  delay writes, PEQ/crossover coefficients, bypass, source, volume and preset modes.
- Every selected reply through HID decoding and request-aware response parsing,
  including exact float bits/raw words, flash fields, hardware ID and completion ACKs.
- Queue replay for all 13 opcodes, late `AudioSrcChanged` frames, wrong-address
  stale float/flash replies, and both finish-block ACK shapes.
- Client identity, volume/source verification, PEQ and bypass writes. The client
  reads four-byte master status; the console reads preset/source/volume-mute
  separately. Only that client's status response is reconstructed from the
  captured fields, rather than claimed as an observed four-byte exchange.

Typed coverage limits: hardware probes with selectors `0`/`1` have no typed
request builder (raw passthrough works). `0x40` has an encoder, but its captured
`20 00` response bytes remain `Response::Unknown` via `execute_raw`, with no typed
DSP-version decoder. No new parser/encoder/queue bug was exposed. Interactive
channel gain uses the client's existing RAM-only control byte `0xA0`; console
scalar edits use reload control `0x80`. The fixture checks the encoder's matching
`save=true` form without changing that client policy.

The canonical request opcode counts are `04:3`, `05:14`, `06:1102`, `07:2`,
`13:51`, `14:369`, `19:2`, `25:4`, `30:10`, `31:8`, `34:3`, `40:1`, `42:23`.
There is no separate ReadMemory opcode here: raw word reads share `0x14` with
ReadFloats. Reset/`0xAA`, CopyPreset, FIR capacity/upload, full-address flash,
firmware/bootloader, display/DRE, Dirac, Bluetooth, DFlash and eARC/WiSA are absent
from this capture; the fixture makes no claim about their live behavior.

Pacing evidence: requests are serialized, with observed reply-to-next-request
gaps down to 0 ms (median 1 ms); no universal fixed sleep is supported. Most
round trips are 1–5 ms, while normal preset changes take about 2.8 s and one
meter request also waits about 2.8 s. Queue serialization and its deadline loop
fit this evidence; no timing policy was changed. The replay uses no wall-clock
capture timing and does not prove hardware latency or the client's 750 ms meter
timeout adequate during transitions.

## Why patch `node-hid`

The macOS Device Console bundle is an Electron app. Its MiniDSP USB traffic ultimately goes through `node-hid`, not the obfuscated `build/src/usbListener.js` business logic.

The generic interception point is:

- `app/app-extracted/node_modules/node-hid/nodehid.js`

Patching that wrapper is the cleanest option because it logs every HID open, write, read, and close without reverse-engineering each higher-level command path first.

## What the patch does

The patched `nodehid.js` logs JSON lines by default to:

- `/tmp/minidsp-device-console-hid.jsonl`

You can still override the destination with `MINIDSP_DC_HID_LOG` if needed.

- `open`
- `tx`
- `tx-result`
- `rx`
- `rx-error`
- `tx-error`
- `close`

Each frame includes:

- timestamp
- process id
- constructor args for the HID device, usually the device path
- byte length
- full space-separated hex payload

Logging is strictly best-effort and must never alter HID behavior.

## Repack workflow

1. Repack the app bundle with the patched extracted tree:

```bash
tools/repack_device_console_with_hid_logging.sh
```

2. Launch the app:

```bash
open -na 'app/MiniDSP Device Console.app'
```

3. Make the change in Device Console.

4. Inspect the captured traffic:

```bash
tail -f /tmp/minidsp-device-console-hid.jsonl
```

5. Save the run into the canonical gitignored local log when it contains useful coverage:

```bash
cp /tmp/minidsp-device-console-hid.jsonl app/minidsp-console-app-log.jsonl
```

6. Restore the original app bundle when done:

```bash
tools/restore_device_console_app.sh
```

## Notes

- The repack script updates `ElectronAsarIntegrity` in `Info.plist` to match the new `app.asar`.
- The script also attempts an ad-hoc `codesign --deep` so the modified app is launchable locally.
- This workflow is intended for the repo-local copy of Device Console under `app/`, not a system-installed copy elsewhere.

## Immediate Learnings

- The raw capture is sufficient for RE because it includes the actual HID request/response pairs, not just app-level debug logs.
- The official app definitely uses these command families on the physical `2x4 HD`:
  - `0x31` hardware/probe reads
  - `0x05` flash reads
  - `0x14` DSP float/meter reads
  - `0x42` master volume writes
  - `0x34` source writes
  - `0x25` preset/config writes
  - `0x13` scalar DSP writes for selector/compressor-style slots
  - `0x19` bypass writes
  - `0x30` biquad/filter writes
- The captured output-0 compressor edit sequence is now identified:
  - `0x13 0x0016 <- 2/3` for the compressor/FIR-style bypass selector slot
  - `0x13 0x0028 <- threshold (float32-LE, dBFS value e.g. -35.0)`
  - `0x13 0x002a <- ratio (float32-LE, direct ratio value e.g. 9.9 for 9.9:1)`
  - `0x13 0x002c <- attack (float32-LE, direct milliseconds e.g. 30.0)`
  - `0x13 0x002d <- release (float32-LE, direct milliseconds e.g. 220.0)`
  - all writes use ctrl byte `0x80` (reloadFlag=true) in the 3-byte address prefix
  - those addresses match the repo's DSP100 table, so the remaining compressor gap is not a wrong opcode/address-family choice
  - knee (0x002b) is present in the device XML with default float32 `20.0` but Device Console never writes it interactively in any captured session — it may be a hardware-only parameter
  - Device Console **never** reads back compressor param addresses via `0x14` after writing them
- The interleaved `0x14 0x0046 0x01` polls in that same session are compressor-meter reads (`DSP_100.meters.compressor[0]`), not authoritative parameter readback.
- The remaining "unknown" command families from that session are now identified:
  - `0x40` is the DSP-version query (`GetDspVersionCmd`)
  - `0x06` is `WriteFlashBlock` and `0x07` is `FinishFlashBlock`
  - on the physical 2x4 HD they appear in the preset-save pipeline as:
    - `0x06 0x01` / `0x06 0x81` chunks for the `presetVals` block (~34,967-byte image)
    - `0x07 0x01` to finish that block (response frame: `rx 02 01`; subsequent bytes are stale)
    - `0x06 0x02` / `0x06 0x82` chunks for the `presetBypass` block (3-byte records: type+flags byte + 16-bit BE address; type 0x01=PEQ, 0x04=BPF/crossover, 0x80 bit set when bypass≠0)
    - `0x07 0x02` to finish that block (response frame: `rx 02 02`; subsequent bytes are stale)
    - then `0x05 ff a3 01`, `0x04 ff c8 ....`, and `0x25 slot 0x01`
  - the `presetVals` block written by "Initialize to default Settings" contains **default** values — it does NOT reflect live-edited values at the time of capture; live `0x13` edits and flash saves are independent operations
- That means `0x06` / `0x07` are flash-persistence traffic, not the missing authoritative runtime readback path for DSP100 compressor or routing state.
- The companion `device-console-main-session.log` tightens the same conclusion:
  - it logs `device token is the same as XML token` before update/save decisions
  - after audio-proc updates it logs `MMM: Saving modification-token to device` and then `MMM: Saving modification-token to file`
  - so modification tokens are not passive runtime-state mirrors on this path; the controller explicitly rewrites them as part of its local-state/save model
- The desktop daemon now mirrors that controller behavior for `/config` and measurement-mode preset-content writes by saving a fresh active-preset token before persisting shadow state.

## FIR Tap Count Model (2026-04-10 RE)

Device Console does NOT read FIR tap count from hardware address `0x0054` (or any other
parameter-space address). The hardware low-address tap-count slot reads back garbage on DSP100.

Actual model used by Device Console:
- **Configured taps**: read from local XML `_Taps` values in the saved preset/settings file
- **Available tap capacity**: queried via opcode `0x39` (`GetNumFirTaps`) — a dedicated command
  sent before FIR coefficient upload (during `deviceFlash.writeFirTapsToFlash`)
  - Wire format: `[len=0x03, cmd=0x39, channel_byte, checksum]`
  - Response: 4-byte DSP message; tap count = `response.readUInt16BE(0x02)`
  - Example: `[0x04, 0x39, 0x08, 0x00]` → available taps = `0x0800 = 2048`
  - NOT called during startup/attach; only when FIR data is about to be written

Note: the `[0x39, 0x08, 0x00]` bytes described in `docs/NOTES-cli-verification.md` section 6
under "FirLoadStart" are the response bytes for this `GetNumFirTaps (0x39)` command. The old
label "FirLoadStart" was the pre-identification name for what is now confirmed as `0x39`.

## Preset Change Notification (0xAB)

After a `0x25` preset-change command with a non-zero flag byte, the device sends an async
notification:

- Opcode `0xAB` = `PresetChangeCompleted`
- Logical payload: just 2 bytes `[0x02, 0xAB]` (the `size=0x02` tells the parser this is the full message)
- Trailing bytes in the 64-byte HID report buffer are stale/irrelevant and are not decoded
- `streamDevice.changePresetSlot(preset, flag)`: flag=0 → wait for sync `result-ok`; flag≠0 → wait for async `0xAB`
- Wire format of `0x25`: `[len=0x04, cmd=0x25, preset_byte, flag_byte, checksum]`

Also: opcode `0xAA` = `ResetCompleted` (async notification after device reset via `0x03`)

## Full Opcode Table (2026-04-10 RE — all confirmed or identified)

Commands confirmed on the physical 2x4 HD across live captures are marked with ★.
The canonical fixture covers only the 13 request opcodes listed above; in particular,
CopyPreset and FIR capacity are identified from other evidence, not this capture.

| Opcode | Name | Direction | Notes |
|--------|------|-----------|-------|
| `0x03` | Reset | TX | Zero payload; `0xAA` async response |
| `0x04` | WriteFlash | TX | `[addrHB, addrLB, ...data]`; used for token writes ★ |
| `0x05` | ReadFlash | TX/RX | `[addrHB, addrLB, len]`; returns flash data ★ |
| `0x06` | WriteFlashBlock | TX | `[block_id (first) or block_id\|0x80 (continuation), ≤32 data bytes]`; preset-save blocks 1/2; RX unframed OK/status ★ |
| `0x07` | FinishFlashBlock | TX | `[block_id]`; canonical RX `[02 01]` for block 1, `[02 02]` for block 2 (remaining HID bytes stale) ★ |
| `0x11` | LoadDspProgram | TX | Raw DSP program chunk (DSP firmware upload only) |
| `0x13` | LoadDspParam | TX | `[ctrl, addrHB, addrLB, ...4n bytes]`; ctrl=`0x80` reloads; scalar floats and selector/delay integers are LE ★ |
| `0x14` | ReadDspParam | TX/RX | TX `[addrHB, addrLB, wordCount]`; canonical RX `[14, addrHB, addrLB, ...4n LE bytes]`, no count echo ★ |
| `0x17` | SetMute (legacy) | TX | Old APK only: `[value]` quick-control |
| `0x19` | BypassDspFilter | TX | `[enable? 0x80:0x00, addrHB, addrLB, ...]` ★ |
| `0x1A` | DreStatus | TX/RX | DRE feature; device-specific |
| `0x1B` | OledIdleTime | TX | `[secs]`; display idle timeout |
| `0x1E` | SetDreStatus | TX | DRE enable/disable |
| `0x21` | BluetoothAtCmd | TX/RX | Raw AT command bytes (e.g. `ATI0\r`); Bluetooth devices only |
| `0x24` | ReloadE2promHdr | TX | Zero payload |
| `0x25` | ChangePresetSlot | TX | `[preset, flag]`; flag=0 sync, flag≠0 async (0xAB response) ★ |
| `0x27` | CopyPreset | TX | Sync form `0x27 0x00`; current best evidence is "copy the active preset to the remaining preset slots"; ~23 s on physical 2x4 HD ★ |
| `0x30` | SetDspFilterBiquads | TX | `[0x80, addrHB, addrLB, auxHB, auxLB, 5×float32-LE]` ★ |
| `0x31` | GetHardwareId | TX/RX | No payload or selector `00`/`01`; canonical RX `[31, 01, 55, 0a, ...]` (firmware 1.85, hardware 10) ★ |
| `0x34` | SetDigitalSource | TX | `[source_id]` ★ |
| `0x39` | GetNumFirTaps | TX/RX | `[channel]`; response tap count at bytes [2..3] uint16BE |
| `0x3C` | WriteFlashFullAddr | TX | `[addrB2, addrB1, addrB0, ...≤32 bytes]`; 24-bit address |
| `0x3D` | ReadFlashFullAddr | TX/RX | `[addrB2, addrB1, addrB0, len≤0x3A]`; 24-bit address |
| `0x3E` | EnterBootloader | TX | Zero payload; disconnects after send |
| `0x3F` | BypassFIR (legacy) | TX | Old APK: master FIR bypass `[value]` |
| `0x40` | GetDspVersion | TX/RX | Canonical TX `[40]`, RX `[40, 20, 00]`; distinct from DSP ID 100 at flash `0xFFA1` ★ |
| `0x42` | SetMasterVolume | TX | `[volume_byte]` ★ |
| `0x45` | DiracDelayGain | TX/RX | Subcommand `0xA1`; reads delay+gain tuples per channel |
| `0x46` | ReadDFlashId | TX/RX | OTP read; `[selector]` → 32 bytes |
| `0x49` | WriteDFlashId | TX | OTP write; `[selector, ...32 bytes]` |
| `0x50` | EraseFlash | TX | Zero payload; standalone flash erase |
| `0x52` | ComFwUpgrade | TX | XMOS firmware chunks `[phase, 32 bytes]` |
| `0x05` / `0xFFA9` | AudioSrcChanged | RX | Unsolicited `[05, ff, a9, source]` after synchronous source ACK; not a selectable-source list ★ |
| `0xAA` | ResetCompleted | RX | Async notification: `[0x02, 0xAA]` |
| `0xAB` | PresetChangeCompleted | RX | Async notification: `[0x02, 0xAB]` |

## DSP100 Routing Defaults

Confirmed from "Initialize to default Settings" flash-save capture (2026-04-10):
- Diagonal pattern only: `Input 0 → Output 0` and `Input 1 → Output 1` are active; cross-routes are inactive
- Selector word encoding: `2 = active`, `1 = inactive`
- Default values at addresses 6..13: `6=2, 7=1, 8=1, 9=1, 10=1, 11=2, 12=1, 13=1`
- Routing gain at addresses 32..39: all `0.00000000` (float32 `0x00000000`, i.e. 0 dB)
