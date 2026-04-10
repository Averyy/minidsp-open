# Device Console HID Logging

Goal: capture the official MiniDSP Device Console HID traffic while changing settings on a real device, so we can compare its exact request/response frames against `minidsp-open`.

## Canonical Capture

The current canonical raw capture is saved in-repo at:

- `app/minidsp-console-app-log.jsonl`

This is the working copy to diff against when implementing or validating shared protocol behavior. Point-in-time archive copies belong under:

- `app/minidsp-snapshots/`

The most useful companion high-level app log from this round of RE is:

- `app/minidsp-snapshots/20260409-151113-device-console-session/device-console-main-session.log`

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

5. Save the run into the canonical in-repo log when it contains useful coverage:

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
  - `0x13 0x0028 <- threshold`
  - `0x13 0x002a <- ratio`
  - `0x13 0x002c <- attack`
  - `0x13 0x002d <- release`
  - those addresses match the repo's DSP100 table, so the remaining compressor gap is not a wrong opcode/address-family choice
- The interleaved `0x14 0x0046 0x01` polls in that same session are compressor-meter reads (`DSP_100.meters.compressor[0]`), not authoritative parameter readback.
- The remaining "unknown" command families from that session are now identified:
  - `0x40` is the DSP-version query (`GetDspVersionCmd`)
  - `0x06` is `WriteFlashBlock` and `0x07` is `FinishFlashBlock`
  - on the physical 2x4 HD they appear in the preset-save pipeline as:
    - `0x06 0x01` / `0x06 0x81` chunks for the `presetVals` block
    - `0x07 0x01` to finish that block
    - `0x06 0x02` / `0x06 0x82` chunks for the `presetBypass` block
    - `0x07 0x02` to finish that block
    - then `0x05 ff a3 01`, `0x04 ff c8 ....`, and `0x25 slot 0x01`
- That means `0x06` / `0x07` are flash-persistence traffic, not the missing authoritative runtime readback path for DSP100 compressor or routing state.
- The companion `device-console-main-session.log` tightens the same conclusion:
  - it logs `device token is the same as XML token` before update/save decisions
  - after audio-proc updates it logs `MMM: Saving modification-token to device` and then `MMM: Saving modification-token to file`
  - so modification tokens are not passive runtime-state mirrors on this path; the controller explicitly rewrites them as part of its local-state/save model
- The desktop daemon now mirrors that controller behavior for `/config` and measurement-mode preset-content writes by saving a fresh active-preset token before persisting shadow state.
