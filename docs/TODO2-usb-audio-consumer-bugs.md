# USB audio consumer follow-ups

All six minidsp-open fixes from the esp-uac2-host v0.1.3 audit are implemented
and recorded under 0.2.0 in [CHANGELOG.md](../CHANGELOG.md). The targeted
hardware checks and two open-sub-optimizer tasks below remain open. The
open-sub-optimizer fixes belong in that repo.

## Remaining minidsp-open hardware checks

- [ ] Exercise a failed audio close and confirm the cleanup worker retains the handle and retries until the interface is released.
- [ ] Use a short `decaySec` to confirm queued audio drains before stop and the tail marker isn't cut short.
- [ ] Force a hung control endpoint and confirm root-port recycle works while the open gate is held, including failure paths that must allow later reopening.
- [ ] Check non-default devices/formats for packed subslots, missing alternate settings, and stepped sample-rate ranges.

Code evidence: `Uac2Device::close_with_retry`, `validate_stream`, and
`sample_rate_in_range` in [usb_audio.rs](../app-esp32/src/usb_audio.rs);
`wait_for_queued_audio_to_play`, `AudioRecoveryStep`, and missing-start-time
errors in [http_server.rs](../app-esp32/src/http_server.rs). Playback status
and late failures use [the shared playback contract](ref-playback.md).

## Open-sub-optimizer work

These changes belong in open-sub-optimizer, not this repo.

- [ ] Convert playback offsets from `playResponse.sampleRate` to the capture sample rate, or reject a mismatch. `estimatePredictedLocalSampleIndex()` in `src/minidsp-open-alignment.ts` currently adds playback-sample offsets to a capture-rate index. A different mic rate can move the prediction outside the marker/sweep search windows.
- [ ] Budget capture time for `/play` preparation, or start capture after the response. `src/minidsp-open-audio-backend.ts` starts capture before the request and allows only two seconds beyond the signal; generation, source switching, or recovery can take longer and cut off the recording.

## Timing constraint

`startedAtUs` marks the driver's first URB submission, not analog output.
Initial ESP32 URBs carry silence; PCM sample zero follows about 9 ms later,
plus the MiniDSP's output latency. Callers must leave room for those delays
when searching for markers. See [ref-playback.md](ref-playback.md).
