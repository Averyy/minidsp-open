# Changelog

Notable changes follow [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

There are no git tags or tagged releases. The workspace version, `0.2.0`, is
unreleased. Unreleased lists the current working-tree changes; the dated
milestones below describe earlier development, not published releases.
Current verification limits and open work live in [PROGRESS.md](PROGRESS.md).

## Unreleased

### Added

- Rattle finder with labelled tones, volume passes, CLI and HTTP controls, and an ESP32 status page; basic playback verified on a 2x4 HD with a UMIK-1.
- Playback completion and failure status through `GET /devices/{id}/play` on desktop and ESP32.
- ESP32 firmware OTA with two app slots, rollback, a `/firmware` API, an `/update` page, daily release checks, and a local WiFi upload tool.
- Desktop test and ESP32 build workflows, plus release packaging for desktop binaries and firmware.
- ESP32 internal-heap and task-stack diagnostics, minimum-heap reset, and an optional heap-tracing build profile.
- Public README, contributor setup, third-party notices, and bundled license texts.

### Changed

- The workspace builds clippy-clean.
- ESP32 builds use ESP-IDF 6.0.3 and esp-uac2-host v0.1.3; the first move from IDF 5.4 requires a USB bootloader flash.
- Repository build and flashing wrappers select the pinned ESP-IDF and esptool versions and track external UAC2 component changes.
- CI and release builds pin esp-uac2-host v0.1.3, espup v0.16.0 and Xtensa Rust 1.93.0.0 (1.97.0.0 crashes LLVM on this firmware), and host tests no longer read local-only device captures.
- ESP32 partitions now provide two 3 MB firmware slots and about 9.9 MB of bundle storage; existing boards need a USB flash to change layouts.
- Hosted ezBEQ is pinned to 2.12.4 with checksum verification, slot title/author metadata, custom biquad uploads, filter-author and new-title queries, and active-slot/output controls.
- Catalogue bundles include device-ready 96 kHz float32 coefficients with an authored-filter fallback and a sample-rate guard.
- ezBEQ state serialization and reusable service workers reduce ESP32 internal-RAM pressure during HTTP, WebSocket, and playback use.
- Rattle plans and generators use memory proportional to tones plus volume passes, and ESP32 streams tones through the shared playback worker.
- The default sweep fades in over at least one start-frequency cycle; timing markers remain enabled by default.
- The advertised CLI version is 0.2.0 so ezBEQ enables Flex HTx input PEQs.
- Protocol crate documentation states that supported builds require `std`.
- Project history lives here; PROGRESS.md holds current status and open work.

### Fixed

- USB audio cleanup retains failed-close handles and retries until the driver releases them.
- ESP32 playback drains queued audio before stopping, preserving short-decay sweep endings and tail markers; desktop playback also drains device latency.
- Missing USB start timestamps fail playback instead of returning a fabricated `startedAtUs`.
- Timed-out USB audio control requests trigger root-port recovery, with opening held during recovery and restored on failure.
- USB audio validation checks packed subslots, scans past missing alternate settings, and honours stepped sample-rate ranges.
- Playback reports transfer/stream failures and handles allocation, panic, open/stop races, and failed source switching without leaving the device busy.
- Desktop audio refuses ambiguous output mappings before changing source/volume, and rattle requests reject unrepresentable volume steps and duplicate rounded tones.
- Rattle sessions fail on teardown errors and skip volume restoration while the stream may still be running, stop and restore after a worker panic, and `minidsp rattle` stops any session it interrupted with Ctrl-C or a failed start.
- Sweep generation avoids float-step drift, silent narrow bands, rate-dependent truncation errors, and sample-count overflow on ESP32.
- Streaming and random-access sweep generation use the same fade envelope, removing the low-frequency onset burst verified on a 2x4 HD.
- ESP-IDF 6.x HTTP-load crashes and repeated-use RAM loss are addressed with WiFi/stack allocation changes, reusable threads, and direct ezBEQ serialization; repeated sweeps and a recorded soak passed on a 2x4 HD.
- Modern meter replies are parsed by length before treating a first data byte as a count echo.
- Protocol decoding handles empty frames and framed HID replies whose length equals an opcode.
- TCP bridge parsing preserves frame boundaries across reads, bounds buffered input, and handles padding without executing embedded fragments.
- Client queue cancellation, ambiguous timeouts, serial-checked reconnects, idempotent replay, FIR restart, and token/shadow freshness are handled consistently.
- Snapshot restore preserves zero-valued gain/delay/routing readback and skips unchanged preset payloads and tokens.
- Config indices are checked before writes, batch files honour `--force-kind`, and discovery uses the configured name with one-second broadcasts and multicast.
- ESP32 HID cleanup releases its claimed interface, retires timed-out transfers before reuse, and handles allocation failures without leaking resources.
- ESP32 snapshot export captures the active preset, and restore rejects incomplete payloads.
- Hosted ezBEQ handles UTF-8 and complete request bodies, resets gain/mute on clear, and supports concurrent state/Levels clients with text ping/pong.
- Bundle updates stage and resume downloads while reclaiming stale files.
- ESP-IDF 6.x WebSocket handshakes register the callback required by the updated HTTP server.
- The ESP32 no longer replays its boot log at startup, which printed every boot line twice on serial and pushed real entries out of `/debug/log`.

### Removed

- The superseded desktop sweep planning redirect; the stable contract stays in [ref-playback.md](docs/ref-playback.md).

### Security

- Checked-in documentation and fixtures use synthetic device identifiers; flashing tools accept an explicit port instead of a maintainer-specific device path.

## Snapshot and recovery milestone (untagged) - 2026-04-16

### Added

- Strict full-device snapshot export/import for the CLI and desktop/ESP32 APIs, including tokens, preset shadow, and FIR data; round trips verified on a 2x4 HD.
- Snapshot learning and explicit refusal when controller state isn't complete enough for a strict backup.
- XML-backed active-preset, selected-preset, and all-preset reset flows plus reboot controls on desktop and ESP32; verified on a 2x4 HD.
- Embedded default-preset images for all seven modern SHARC XML families, generated from protocol-owned vendor assets without a private app checkout.
- A repeatable live-validation harness for HTTP, WebSocket, TCP, CLI, playback, embedded ezBEQ, and a real upstream ezBEQ process.
- Snapshot-backed destructive validation with automatic restore, baseline equality checks, and durable reports/artifacts.
- Desktop Apple TV Companion discovery, pairing, credential persistence, reconnect, and decoded now-playing events; verified with real Apple TV playback.
- A Companion movie-suggestion matcher using normalized title and duration, replayable samples, and candidate BEQ variants that require manual selection.

### Changed

- USB flashing seeds the local ezBEQ UI and compressed catalogue into LittleFS so the board can serve them before a network update.
- The hosted browser catalogue, filter metadata, and firmware-owned shim load from the board rather than relying on a warm browser cache.
- Reset assets are generated as binary blobs to keep builds manageable across DSP families.

### Fixed

- Official Device Console preset-save/reset replies and slow-command timeouts match its parser; the macOS app completed network initialization through the ESP32 bridge on a 2x4 HD.
- Desktop and ESP32 reset/reboot recovery tolerate expected disconnects and restore the saved baseline after destructive checks on a 2x4 HD.
- ESP32 preset-specific reset routes use exact registrations supported by ESP-IDF's wildcard matcher.
- ESP32 playback recovery releases audio handles before reopen/restart/root-port recovery, avoiding stuck-open interfaces after start failures.
- TCP bridge contention with compatibility polling no longer breaks upstream ezBEQ load/clear operations.
- Embedded ezBEQ updates the active-preset shadow so multi-slot load/clear and restore target the intended preset.
- Hosted catalogue streaming yields to the ESP32 scheduler, and updater startup/version checks preserve installed data when the remote site is unavailable.
- Updater versions reflect the stored bundle, avoid duplicate UI suffixes, and prevent stale remote bundles from replacing newer local flashes.
- Hosted Levels and compatibility WebSocket polling keep valid sender ownership, ignore duplicate subscriptions, and use real timestamps.
- Apple TV discovery merges addresses by stable device ID, credentials have restrictive permissions, and encrypted frames are bounded before allocation.

## Playback and device-control milestone (untagged) - 2026-04-14

### Added

- Shared measurement-sweep generation, desktop CPAL playback, ESP32 UAC2 playback, and `/sync` clock endpoints.
- Canonical flat `/play` requests with deterministic sample offsets, optional markers, source auto-switch, and source restore; desktop and ESP32 playback verified on a 2x4 HD.
- Persistent desktop routing, scalar, selector, and FIR shadow with token-matched imports from official Device Console preset XML.
- Shared 72-byte Wi-DG beacon encoding, per-interface desktop discovery/appliance beacons, default desktop bridge/discovery, and TCP socket options with serialized multi-client handling.
- ESP32 preset-scoped in-memory shadow with capture on preset switch, extended `/config` parity, bass-management routes, default PEQ frequencies, and OpenAPI output.
- Production RGB LED states for boot, provisioning, network/device readiness, updates, and fatal errors.

### Changed

- Sweep defaults use a hybrid low-frequency/logarithmic sweep with pre/post silence and timing markers; `markerCycles=0` removes both markers and their gaps.
- ESP32 sweeps are generated into PSRAM before playback, with bounded writes and scheduler yields to keep USB audio and the watchdog healthy.
- HID operations return a busy error during ESP32 playback; source and master volume changes happen outside the streaming window.
- DSP100 config readback uses verified controller shadow for fields whose runtime reads aren't authoritative.

### Fixed

- Modern routing gain/enable, mute polarity, output invert, delay encoding, FIR capacity/end acknowledgements, and preset/source verification work on a 2x4 HD.
- HID decoding accepts real write echoes while rejecting stale read replies and unrelated source-change notifications.
- Raw TCP/WebSocket writes update config shadow, and desktop preset mutations save fresh modification tokens before persisting state.
- ESP32 source switching and reconnect cleanup recover without USB-host assertions across idle unplug/replug, delayed attach, and device power cycles.
- USB-audio task placement, playback pacing, and pre-generation yields remove periodic blips and watchdog stalls on the verified 2x4 HD path.
- Config body reads, JSON error responses, preset-copy response shape, PEQ-reset slot metadata, and source/volume route parsing match desktop behavior.
- Fatal LED state stays latched until reboot.

### Removed

- Legacy nested playback request fields and the earlier bursty measurement default; invalid fields now return explicit errors.

## Core controller milestone (untagged) - 2026-04-07

### Added

- Shared Rust protocol, modern float32 and legacy fixed-point dialects, device profiles, DSP address tables, command encoding/decoding, and USB HID/TCP/WebSocket transports.
- minidsp-rs-compatible desktop CLI, HTTP/WebSocket API, TCP bridge, discovery, measurement mode, and config controls.
- ESP32-S3 USB host, WiFi provisioning with a captive portal, HTTP/bridge services, mDNS, and debug logs.
- Embedded ezBEQ API, LittleFS UI/catalogue storage, a bundle builder, and background catalogue updates.
- Source-backed definitions and test gates for all 13 exact modern HW/DSP pairs and seven cached legacy config families.
- Device-support importer and generated coverage reports tied to official Device Console assets.

### Fixed

- Early ESP32 USB hotplug, WiFi provisioning, level-meter, mDNS, and WebSocket compatibility failures.
- Desktop/ESP32 config reads avoid fabricated DSP100 values where hardware readback isn't authoritative.
