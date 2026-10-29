# ESP32 USB audio validation

Playback is implemented. The completed bring-up and fixes are in
[CHANGELOG.md](../CHANGELOG.md); [PROGRESS.md](../PROGRESS.md) holds current
status. The stable request, timing, marker, and rattle contracts are in
[ref-playback.md](ref-playback.md).

## Open checks

- [ ] Measure physical analog output duration against the shared sample plan. Software enqueue completion includes ring-buffer backpressure and isn't an output-clock measurement.
- [ ] Confirm current default and marker-enabled startup latency against the earlier measured window: about 0.35 s already on USB and 0.4–0.5 s after switching from Toslink. Check the real start timestamp and offsets together.
- [ ] Measure `POST /sync` latency against the original under-5-ms target.
- [ ] Sign off concurrent HID control while audio streams. Post-play HID recovery already passed; current firmware deliberately returns 409 during playback. This also blocks live master-volume changes in the rattle finder.
- [ ] Interrupt active playback with USB disconnect, confirm bounded cleanup without a crash, then replug and check control/audio recovery.
- [ ] Exercise non-default formats and back-to-back 48 kHz/44.1 kHz plays, checking reopen, clock configuration, packet pacing, and marker timing.
- [ ] Run cold-boot `Toslink -> /play -> Toslink`; warm source switching and restoration already passed.
- [ ] Finish the official-app comparison for modern routing/mute after reset. DSP100 runtime selector reads aren't authoritative; compare controller state and audible behavior rather than treating collapsed words as truth.

Default playback, audible sweep quality, marker-only/combined playback, build
and boot, repeated plays, post-play HID, heap stability, and idle hotplug were
verified on the 2x4 HD. Generator configuration, deterministic counts, error
responses, and the UAC2 FFI/build/boot integration are implemented. Repeat the
malformed-request and HID-only-device checks on hardware where possible; their
code paths exist, but broader device claims still need a bench.

The six consumer fixes have their own remaining checks in
[TODO2-usb-audio-consumer-bugs.md](TODO2-usb-audio-consumer-bugs.md).

## Design constraints for the remaining work

- HID and UAC2 share one ESP-IDF USB host installation but own separate clients/interfaces. The UAC2 driver owns its client and audio discovery callbacks.
- Check profile `has_usb_audio` before runtime availability: unsupported devices return 400; a supported profile without a connected audio interface returns 503.
- ESP32 sweeps are pre-generated in PSRAM because real-time sweep math is too slow. Stream via an internal-SRAM buffer and yield across FreeRTOS ticks during generation. Rattle tones use the same worker but generate incrementally.
- Keep audio transfer work off the WiFi-heavy core and preserve internal SRAM for USB, WiFi, and HTTP. Use the current heap/stack diagnostics when testing concurrency.
- The ESP32-S3 full-speed USB/FIFO configuration is for playback plus feedback, not simultaneous playback and capture.
- Preserve the two-second low-level preamble for MiniDSP ASRC lock, a real driver start timestamp, deterministic sample offsets, and source restoration on success and failure.
- Timing markers stay on by default. They are audible signals; `markerCycles=0` removes markers and their gaps. Use the current shared fade envelope rather than the retired measurement-bundle design.
- WiFi power-save state and the playback busy flag must recover on every exit path. Don't bypass the HID busy guard to test concurrent control before it's signed off.

## Evidence and verification

- Shared plan/generator: [protocol/src/playback.rs](../protocol/src/playback.rs).
- FFI, lifecycle, drain/format rules: [app-esp32/src/usb_audio.rs](../app-esp32/src/usb_audio.rs).
- Playback worker, recovery and routes: [app-esp32/src/http_server.rs](../app-esp32/src/http_server.rs).
- Build/boot integration: [Cargo.toml](../app-esp32/Cargo.toml), [build.rs](../app-esp32/build.rs), and [main.rs](../app-esp32/src/main.rs).
- Driver API and simulator: the external esp-uac2-host checkout described in [CONTRIBUTING.md](../CONTRIBUTING.md).
- Host verification: `cargo test -p minidsp-protocol` and `cargo test`.
- Firmware build: `tools/esp-cargo build --release`.
- Live checks: [ref-live-validation.md](ref-live-validation.md), with audio explicitly enabled.
