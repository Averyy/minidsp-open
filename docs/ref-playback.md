# Playback & Sync Endpoints

Canonical reference for the additive measurement-playback surface:

- `POST /devices/{id}/play`
- `POST /sync`

This file defines the stable endpoint contract and the current verified baseline.
Temporary validation state belongs in:

- `PROGRESS.md`
- `docs/todo-checker.md`

Deep implementation history for the ESP32 bring-up still lives in
`docs/TODO1-esp32-usb-audio.md`.

## Goal

The playback surface lets a caller trigger a deterministic measurement signal
through the MiniDSP USB-audio path and receive enough timing information to
align a recording on the caller side.

These endpoints are additive. They extend the existing HTTP API without changing
the compatibility semantics documented in `docs/ref-compatibility.md`.

## Endpoints

### `POST /devices/{id}/play`

Starts playback of a generated measurement signal on the target device.

The shared request and response DTOs live in the protocol crate. The public
request surface is the canonical marker-aware form:

- `sweepDurationSec`
- `sweepStartHz`
- `sweepEndHz`
- `sweepFadeSec`
- `markerBefore`
- `markerAfter`
- `markerGapMs`
- `markerFrequencyHz`
- `markerCycles`
- `decaySec`
- `sampleRate`
- `bitDepth`

Legacy nested request fields such as `sweep`, `toneBurst`, `tailToneBurst`,
`preambleSec`, and `gapSec` are intentionally rejected.

Representative response fields:

- `startedAtUs`
- `toneBurstOffsetSamples`
- `sweepOffsetSamples`
- `tailToneBurstOffsetSamples`
- `totalSamples`
- `sampleRate`
- `sourceChanged`
- `previousSource`

### `POST /sync`

Returns the server-side microsecond clock used to interpret `play.startedAtUs`.

The important contract is platform-local clock consistency, not matching a
single absolute wall-clock implementation across desktop and ESP32. On a given
host, `/sync.serverTimeUs` and `/play.startedAtUs` share the same clock basis.

## Default Signal Contract

The current shared default is the REW/MSO-style baseline, not the retired
older measurement bundle:

- `5-400 Hz` hybrid sweep
- `5.46 s` sweep duration
- `16-bit`
- `2 s` low-level dither pre-silence
- `2 s` post-silence
- `20 ms` raised-cosine fade in/out
- marker-enabled by default

The sweep follows the REW-style hybrid behavior when the start frequency is
below `10 Hz`: linear from `5 Hz` to `10 Hz`, then logarithmic from `10 Hz`
to the sweep end.

Default request semantics:

- `{}` plays the canonical default bundle with both markers enabled.
- Sweep-only playback is explicit via `{"markerBefore":false,"markerAfter":false}`.
- `markerCycles: 0` disables both marker sections entirely and removes their gaps
  even if `markerBefore` / `markerAfter` remain `true`.

## Source-Switch Behavior

If the active source is not `Usb` and the target device supports USB audio, the
implementation may temporarily switch to `Usb` before playback and restore the
previous source afterward.

Required behavior:

- return whether a source change was required
- report the previous source using PascalCase JSON naming
- attempt restore after playback
- attempt restore on early playback-startup failure as well

## Platform Notes

### ESP32

- Uses the UAC2 host path to stream generated PCM to the MiniDSP.
- Timing and glitch validation should be done with a release firmware build, not
  a debug build.
- The HTTP surface matches the shared request/response contract, but ESP32 keeps
  an explicit allocation ceiling for generated playback buffers.

### Desktop

- Uses the same shared playback DTOs and sweep generator.
- Uses a platform audio backend instead of the ESP32 UAC2 transport.
- `/sync.serverTimeUs` and `startedAtUs` intentionally share the same local
  `SystemTime`-derived basis on desktop.

## Verified Baseline

Shared public-contract verification now covers the real MiniDSP 2x4 HD path:

- sweep-only playback
- marker-only playback
- combined marker + sweep playback
- deterministic sample offsets
- invalid/legacy request rejection with `400`
- `markerCycles: 0` collapse to sweep-only timing
- non-USB-source auto-switch + restore

Desktop live verification has confirmed the canonical contract on the physical
2x4 HD, including `/sync` bracketing and default marker-aware playback.

ESP32 live verification has confirmed the same outcome on the physical 2x4 HD
for the current shared default playback path after the sweep-quality and
source-switch fixes landed.

## Remaining Validation

The remaining playback work is no longer endpoint-design work. It is runtime
signoff and edge-case coverage tracked in `PROGRESS.md`, including:

- non-default playback formats
- cold-boot non-USB playback
- unplug-mid-play / replug recovery
- sample-rate switches between plays

Keep those items in `PROGRESS.md` instead of re-expanding this file into another
running investigation log.
