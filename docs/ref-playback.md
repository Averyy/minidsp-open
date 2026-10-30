# Playback & Sync Endpoints

Canonical reference for the additive measurement-playback surface:

- `POST /devices/{id}/play`
- `GET /devices/{id}/play`
- `POST /sync`

This file defines the stable endpoint contract and the current verified baseline.
Temporary validation state belongs in:

- `PROGRESS.md`
- `docs/todo-checker.md`

Implementation history is in [CHANGELOG.md](../CHANGELOG.md). Remaining ESP32
audio checks are in [TODO1-esp32-usb-audio.md](TODO1-esp32-usb-audio.md).

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

Limits: every duration (`sweepDurationSec`, `sweepFadeSec`, `decaySec`, and
the marker length `markerCycles / markerFrequencyHz`) must be finite and at most
`600 s`, and `sampleRate` at most `384000`. These keep sample offsets
representable on the 32-bit ESP32; anything outside them is a `400`.

Representative response fields:

- `startedAtUs`
- `toneBurstOffsetSamples`
- `sweepOffsetSamples`
- `tailToneBurstOffsetSamples`
- `totalSamples`
- `sampleRate`
- `sourceChanged`
- `previousSource`

`200` means the stream started, not that the signal finished playing. The
first written sample goes out about 9 ms after `startedAtUs` on ESP32 (the
driver's initial URBs carry silence), plus the MiniDSP's own output latency.
If the stream cannot report a real start time, `/play` fails with `500`
rather than substituting the current clock.

### `GET /devices/{id}/play`

Playback status, so a caller can learn whether the rest of the signal played
after `/play` returned `200`:

```json
{
  "playing": false,
  "last": {
    "startedAtUs": 123456789,
    "totalSamples": 456000,
    "sampleRate": 48000,
    "state": "completed",
    "error": "...",
    "sourceRestoreError": "..."
  }
}
```

- `playing` is true from `POST /play` until the run is fully finished,
  including any source restore. A new `/play` gets `409` while it is true.
- `last` is the most recent run that returned `200`, identified by its
  `startedAtUs`. A run that fails before starting answers `/play` with an
  error and leaves `last` unchanged.
- `state` is `playing`, `completed`, or `failed`. `failed` carries `error`
  (a write failure, unplug, or driver stream error cut the signal short).
- `sourceRestoreError` is set when the source switched for playback could not
  be restored, including when the restore was skipped after an interrupted run.

Poll until `playing` is false and `last.startedAtUs` matches the run's
`/play` response.

### `POST /sync`

Returns the server-side microsecond clock used to interpret `play.startedAtUs`.
The body is `{"clientTimeUs": <client clock in µs>}`; the response echoes it
as `clientTimeUs` alongside `serverTimeUs`. A client maps the two clocks from
the lowest-round-trip sample of a few calls.

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
- raised-cosine fade-in over at least one cycle of the start frequency
  (`200 ms` at the `5 Hz` default), and a `20 ms` fade-out
- marker-enabled by default

`sweepFadeSec` (default `20 ms`) sets the fade-out and the shortest fade-in;
the fade-in is stretched to one cycle of `sweepStartHz` when that is longer,
and `0` disables both. A 20 ms fade is a tenth of a 5 Hz cycle: that onset
put a burst into the 20-120 Hz band at -22 dB relative to a full-scale 50 Hz
tone, which a subwoofer plays as a thump before the sweep becomes audible.
One full cycle brings it to -58 dB. Sample offsets are unchanged, since the
fade is inside the sweep.

The sweep follows the REW-style hybrid behavior when the start frequency is
below `10 Hz`: linear from `5 Hz` to `10 Hz`, then logarithmic from `10 Hz`
to the sweep end.

Default request semantics:

- `{}` plays the canonical default bundle with both markers enabled.
- Sweep-only playback is explicit via `{"markerBefore":false,"markerAfter":false}`.
- `markerCycles: 0` disables both marker sections entirely and removes their gaps
  even if `markerBefore` / `markerAfter` remain `true`.

## Rattle finder

The shared `protocol::rattle` DTOs and generator schedule steady tones on both
ESP32 and desktop. Desktop playback (including `/play`) requires one local USB
controller and one matching miniDSP audio output. CPAL names cannot safely map
multiple miniDSPs or a network control transport; these requests fail with `503`
before any source or volume change. `MINIDSP_PLAY_DEVICE` only narrows this match.

- `POST /devices/{id}/rattle` accepts a `RattleRequest` body and returns `200`
  with `RattleResponse` once streaming starts: `startedAtUs`, `schedule`,
  optional `previousVolumeDb`, `sourceChanged`, and optional `previousSource`.
  Invalid requests get `400`; an active `/play` or rattle run gets `409`
  (both share the playback-busy flag); no device or no USB audio gets `503`.
- `GET /devices/{id}/rattle` returns `RattleStatus`: `playing`, optional
  `current`, and optional `last`. `playing` stays true until streaming and
  volume/source restoration finish. `last` is the most recent run that
  returned `200`; a failure before startup leaves it unchanged.
- `POST /devices/{id}/rattle/stop` fades the current tone out and ends the
  session (silence ends immediately). Returns `200` with `RattleStatus`, or
  `409` if nothing is playing. Poll GET until `playing` is false and
  `last.startedAtUs` matches the start response.

Request defaults and limits:

- `frequenciesHz` defaults to `[20, 25, 31.5, 40, 50, 63, 80, 100, 125, 160, 200]`.
  Alternatively, supply all of `startHz`, `endHz`, `count` for logarithmic
  spacing (`count >= 1`, `endHz >= startHz`; one tone uses `startHz`). The two
  forms cannot be combined. Generated frequencies are rounded to 0.1 Hz;
  requests whose generated tones collide after rounding are rejected. Either form is capped at `64` tones. Unknown fields
  are rejected.
- `toneSec` defaults to `4`, `gapSec` to `1`, `cycles` to `1`, and `volumesDb`
  to `[-35]`. `sampleRate` and `bitDepth` have the `/play` defaults and limits.
- Frequencies must be finite, `5–2000 Hz`, and below Nyquist. Durations must be
  finite and at most `600 s`; tones must be at least `0.5 s` and span at least
  two samples, gaps nonnegative, and cycles at least one. Volumes must be a
  nonempty list of finite values within `-127..=0 dB`, in multiples of `0.5 dB`
  (other values are rejected with `400`). The schedule is capped
  at `4096` steps (`tones × volumes × cycles`) before allocation. The entire
  session, including the `250 ms` silent lead-in and final gap, is capped at one hour
  before and after rounding to samples.

Responses carry a compact `RattleSchedule`, never a per-step list:
`sampleRate`, `leadInSamples`, `toneSamples`, `gapSamples`, `totalSamples`,
`frequenciesHz` in list order, `volumesDb` quietest first, `cycles`, and
`masterVolumeDb`. Steps run cycle first, then volume, then frequency. Clients
derive the step count as `frequenciesHz.length × volumesDb.length × cycles`
and step `k`'s start as `leadInSamples + k × (toneSamples + gapSamples)`.
Offsets count stereo frames, not bytes; tone and gap ends are exclusive.

`current` contains zero-based `step` and `cycle`, counts `steps` and `cycles`,
`label` (e.g. `31.5 Hz @ -40 dB`), `frequencyHz`, `volumeDb`, `phase`
(`leadIn`, `tone`, or `gap`), `remainingSec` in that phase, `elapsedSec`, and
`totalSec`. Lead-in labels the upcoming first tone; a gap labels its preceding
tone. Remaining time includes the current sample. `current` is omitted once
audio ends and is only filled while this run is playing.

`last` contains `startedAtUs`, `schedule`, and `state` (`playing`, `completed`,
`stopped`, or `failed`). Optional `error` explains a stream failure;
`volumeRestoreError` and `sourceRestoreError` report restore failures without
changing the stream's outcome. Absent optional fields are omitted from JSON.

The platform sets the master to `masterVolumeDb` (the loudest requested volume)
before the first tone and restores `previousVolumeDb` after completion, stop,
or failure, once audio is confirmed stopped or its interface closed/device gone.
If teardown cannot be confirmed, the session volume is left in place and
`volumeRestoreError` is `skipped: stream may still be running`. Teardown failure
also fails the run. A failed restore goes in `volumeRestoreError`. Source switch and
restore follow `/play`, including PascalCase `previousSource` and a restore
attempt on startup failure. HID endpoints answer `409` while a session streams,
as during `/play`.

Quieter steps scale PCM by `10^((volumeDb - masterVolumeDb) / 20)`, so no HID
traffic is needed while streaming. Both channels carry the same sine at the
sweep's digital peak. Tones start at phase zero and fade in/out over the greater
of two cycles or `100 ms`, clamped to half their length. Gaps are exact silence;
a stop fades from the last emitted envelope gain.

`RattlePlan::schedule()` exposes the compact schedule; `RattleSchedule::step(k)`
derives any step. `current_at(sample)` on either type derives the status and
returns none at/past the end. Platforms use
`current_at(elapsed_samples(started_at_us, now_us, sample_rate, output_latency_us))`
to label clock-mapped output. `RattlePlan::step_at(sample)` still reports no step
during lead-in. `RattleGenerator` streams complete 16- or 24-bit little-endian
frames and reports its cursor, completion, and whether stopped.

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
- If the board resets while a stream runs and VBUS stays up (crash, watchdog,
  restart, reset button or flashing), the 2x4 HD keeps accepting USB audio but
  outputs nothing until it reboots. USB looks healthy throughout, so this can't
  be detected from the host. A cable unplug doesn't cause it, because the
  MiniDSP sees VBUS drop. The firmware sets a flag while streaming, in RTC
  memory (which survives panics, watchdogs and software resets) and in NVS
  (which the main task writes, and which also survives EN-pin resets). A boot
  that finds the flag sends the MiniDSP its reboot command before opening
  audio. `POST /devices/0/reboot` clears the state by hand. Reproduced and
  verified with a UMIK-1 on 2026-10-30: an EN reset while idle stays audible,
  one mid-stream goes silent, and the boot recovery restores sound.

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
