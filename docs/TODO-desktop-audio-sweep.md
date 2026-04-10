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

## Not Yet Specified

- Audio backend selection (CPAL, PulseAudio, ALSA direct, etc.)
- Timestamp precision on desktop (no hardware timer equivalent to `esp_timer_get_time`)
- Whether to support non-miniDSP audio output targets
- Detailed task breakdown

## Dependencies

- ESP32 implementation lands first — it defines the API contract and validates the sweep generator
- `sweep_gen.rs` must remain platform-independent (no ESP-IDF deps)
- `has_usb_audio` in `DeviceProfile` is shared
