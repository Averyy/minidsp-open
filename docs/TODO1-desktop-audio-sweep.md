# Desktop USB Audio: Measurement Sweep Playback

## Goal

Add `POST /devices/{id}/play` and `POST /sync` endpoints to the desktop daemon, matching the ESP32 API surface. Desktop uses a platform audio backend (e.g., CPAL) instead of UAC2 FFI, but shares the same sweep generator and response contract.

## Scope

- Reuse `sweep_gen.rs` (pure math, already desktop-testable)
- Same `/play` request/response schema as ESP32 (see `docs/TODO-esp32-usb-audio.md`)
- Same shared REW/MSO-style default profile as ESP32 (`5-400 Hz` hybrid sweep, `2 s` low-level dither pre-silence, `2 s` post-silence, `16-bit`)
- Same `/sync` endpoint (trivial — just reads system clock)
- Same source auto-switch + restore behavior via HID
- Platform audio output via CPAL or similar crate — route audio to the miniDSP's USB audio input
- Primarily useful for Raspberry Pi and other headless Linux setups running the desktop daemon

## Current Status

The desktop code paths are now in place:

- `POST /devices/{id}/play` exists and uses the shared playback contract
- `POST /sync` exists
- the default `/play` profile matches the shared REW/MSO-style baseline
- the public `/play` request shape is now the canonical marker-aware form: `sweepDurationSec`, `sweepStartHz`, `sweepEndHz`, `sweepFadeSec`, `markerBefore`, `markerAfter`, `markerGapMs`, `markerFrequencyHz`, `markerCycles`, `decaySec`, `sampleRate`, `bitDepth`
- `{}` now plays the canonical default bundle with both markers enabled; sweep-only playback is explicit via `{"markerBefore":false,"markerAfter":false}`
- `markerCycles: 0` disables both marker sections entirely and removes their gaps, even if `markerBefore` / `markerAfter` stay `true`
- legacy nested request fields (`sweep`, `toneBurst`, `tailToneBurst`, `preambleSec`, `gapSec`) are rejected, and `/openapi.json` advertises the supported request options
- desktop `/sync.serverTimeUs` and desktop `startedAtUs` now come from the same `SystemTime`-derived helper so their clock basis is intentionally aligned
- live desktop verification now covers the full current contract on the physical 2x4 HD: repeated deterministic offsets, `/sync` bracketing, `markerCycles: 0` sweep-only collapse, legacy/invalid-request `400`s, marker diagnostic mode, and source auto-switch + restore from `Toslink`

This file records implementation status. Fresh real-hardware evidence still lives in `PROGRESS.md`, and the remaining items below are runtime signoff, not missing feature work.

Remaining work is signoff, not feature construction:

- optionally revisit desktop timing semantics later if a monotonic clock is desired; current contract is stable wall-clock-derived microseconds, not monotonic time

## Not Yet Specified

- Audio backend selection (CPAL, PulseAudio, ALSA direct, etc.)
- Timestamp precision on desktop (current implementation uses `SystemTime`-derived Unix-epoch microseconds; no hardware timer equivalent to `esp_timer_get_time`)
- Whether to support non-miniDSP audio output targets
- Detailed task breakdown

## Dependencies

- ESP32 implementation lands first — it defines the API contract and validates the sweep generator
- `sweep_gen.rs` must remain platform-independent (no ESP-IDF deps)
- `has_usb_audio` in `DeviceProfile` is shared
