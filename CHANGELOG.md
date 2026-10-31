# Changelog

Notable changes follow [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

Releases are published on the [GitHub Releases page](https://github.com/Averyy/minidsp-open/releases),
and each release's notes there are its section below. Unreleased lists changes
since the latest release. The untagged milestones below 0.2.0 describe earlier
development, not published releases. Current verification limits and open work
live in [PROGRESS.md](PROGRESS.md).

## Unreleased

### Fixed

- The ESP32 couldn't check for or install GitHub releases. GitHub redirects release downloads to a signed `release-assets.githubusercontent.com` URL of about 900 bytes, and the HTTP client's 512-byte request buffer rejected it ("Out of buffer", `ESP_FAIL`). The request buffer is 2 KB now. Boards on 0.2.0 need one uploaded update (`/update` page or `tools/ota-push.sh`) to get the fix.
- The x86_64 Linux release build ran on GitHub's `ubuntu-latest`, now Ubuntu 26.04, so the binary needed glibc 2.43 and wouldn't start on Ubuntu 24.04, Debian 12 or 13, or Raspberry Pi OS. It builds on Ubuntu 24.04 again and needs glibc 2.34, like the ARM64 build, and the release workflow fails if a Linux binary needs anything newer.

## [0.2.0] - 2026-10-31

First public release.

### Added

- `llms.txt`, a compact API reference for LLM clients: CLI, HTTP and WebSocket routes, config file, ESP32 extras and common mistakes. The daemon and the ESP32 serve it at `GET /llms.txt`, replacing a two-line stub.
- `GET /devices/{id}/config` includes a `readback` object on desktop and ESP32 that records, for each field, whether the value was read from the device, comes from this controller's acknowledged writes, is a fallback default, or couldn't be read. A failed mute or routing read no longer looks like a real `false`. Existing value fields are unchanged.
- `POST /devices/{id}/config/restore-mutes` restores output mutes only while the `readback.snapshot_token` from an earlier `GET /config` still matches. A preset change, reset, reboot, reconnect, copy or snapshot restore in between makes it answer 409 without writing.
- Rattle finder with labelled tones, volume passes, CLI and HTTP controls, and an ESP32 status page; basic playback verified on a 2x4 HD with a UMIK-1.
- Playback completion and failure status through `GET /devices/{id}/play` on desktop and ESP32.
- ESP32 firmware OTA with two app slots, rollback, a `/firmware` API, an `/update` page, daily release checks, and a local WiFi upload tool.
- Desktop test and ESP32 build workflows, plus release packaging for desktop binaries and firmware.
- ESP32 internal-heap and task-stack diagnostics, minimum-heap reset, and an optional heap-tracing build profile.
- Public README, contributor setup, third-party notices, and bundled license texts.
- Device Console HID capture fixtures (54 sanitized exchanges, 13 opcodes) with parser and client/queue replay tests.
- Research notes on Device Console device identity, the `0xFFEA` source bitmap, and Dirac preset layouts.
- A typed parser and encoder for the HT/HTx selectable-source bitmap at `0xFFEA`, from the research above; not yet read or written by the client.
- Destructive validation workflow: a full profile with hard stops, recorded manual checkpoints, verified -35 dB audio steps, reboot/reconnect identity checks, concurrent 409 assertions (serialization checks on the single-task ESP32), ceiling-aware volume steps, terminal playback-status checks, `/settings` and `/firmware` coverage, a clear stop on password-protected boards, and a no-target `--plan` mode, with fake-device tests.
- SECURITY.md describing the LAN trust model and how to report vulnerabilities.
- Optional maximum master volume (off by default) on the ESP32 and the desktop daemon: `GET/POST /settings`, the `/status` page, or the `max_volume_db` config key. It clamps every volume write minidsp-open makes, including the TCP bridge and ezBEQ, and rejects rattle passes above it. The daemon also lowers a louder device when it registers or reconnects, and the CLI accepts a bridge's clamped read-back. The miniDSP's own knob and IR remote bypass it.
- Optional ESP32 password (off by default), set on `/status` or during first WiFi setup. With it on, the board UI, API, OTA uploads and WebSockets need a login page session, Basic `admin` credentials or a Bearer token; the TCP bridge and UDP discovery stop until it's cleared. The CLI and `tools/ota-push.sh` read it from `--password-file` or `MINIDSP_PASSWORD`. Holding BOOT for ten seconds clears the password and WiFi.

### Changed

- The ESP32 logs its heap line when a heap minimum moves and otherwise once an hour. Every 5 s, it filled the 4 KB log buffer that `GET /debug/log` serves within about two minutes and pushed out every other line, warnings included.
- The live validation suite skips desktop playback when the MiniDSP is reached over the network, since desktop playback needs its USB audio interface on the local computer. It confirms audible playback before contacting the target.
- A snapshot restore on the ESP32 that loses the MiniDSP to a stalled write now waits for the bus reset to bring it back and resumes from the preset that failed, instead of failing. `POST /debug/usb/stall` injects a stall to test it. The per-write `write_biquad`/`bypass_filter` log lines moved to debug level, which also takes about 2 s off a restore.
- When a HID write to the MiniDSP gets no completion within 5 s, the ESP32 logs the USB controller's registers (`USB DWC …` lines, also at `GET /debug/usb`) and waits up to 20 s more for the MiniDSP. If it still doesn't answer, the ESP32 power-cycles the USB port instead of halting the stuck channel, whose halt never finishes; the MiniDSP re-enumerates in about 20 s and the board stays on the network. The restore or command in flight still fails. `POST /debug/usb/reset-bus` runs the same recovery on demand.
- Desktop and ESP32 share one copy of the PEQ shadow builder, raw-write shadow parsers, scalar sanitation, default PEQ frequencies and device-info assembly in the protocol crate; equivalence tests showed the two copies already behaved identically.
- The workspace builds clippy-clean.
- ESP32 builds use ESP-IDF 6.0.3 and esp-uac2-host v0.2.0; the first move from IDF 5.4 requires a USB bootloader flash. On a 2x4 HD with v0.2.0, 16- and 24-bit sweeps and rattle sessions were audible on a UMIK-1, with a click-free stop.
- Repository build and flashing wrappers select the pinned ESP-IDF and esptool versions and track external UAC2 component changes.
- The UAC2 host driver comes from the ESP Component Registry (`averyy/usb_host_uac2` 0.1.3) instead of a sibling checkout; `ESP_UAC2_HOST_PATH` builds against a local driver working copy.
- CI and release builds pin espup v0.16.0 and Xtensa Rust 1.93.0.0 (1.97.0.0 crashes LLVM on this firmware), use current Node 24 actions, and host tests no longer read local-only device captures. CI also checks formatting and runs clippy with warnings denied.
- ESP32 partitions now provide two 3 MB firmware slots and about 9.9 MB of bundle storage; existing boards need a USB flash to change layouts.
- Hosted ezBEQ is pinned to 2.12.4 with checksum verification, slot title/author metadata, custom biquad uploads, filter-author and new-title queries, and active-slot/output controls.
- Catalogue bundles include device-ready 96 kHz float32 coefficients with an authored-filter fallback and a sample-rate guard.
- ezBEQ state serialization and reusable service workers reduce ESP32 internal-RAM pressure during HTTP, WebSocket, and playback use.
- Rattle plans and generators use memory proportional to tones plus volume passes, and ESP32 streams tones through the shared playback worker.
- The default sweep fades in over at least one start-frequency cycle; timing markers remain enabled by default.
- The ESP32 firmware image is about 550 KB smaller (2.46 MB in its 3 MB slot). Release Rust is size-optimised, apart from the playback generators; ESP-IDF's C code keeps full optimisation. The standard library drops backtrace support, which the board can't use, and picks its smaller algorithms. The TLS bundle keeps the common root CAs, which cover GitHub Pages and release downloads.
- The ESP32 accepts request header blocks up to 1024 bytes (was 512), leaving room for a session cookie or Basic header on browser requests.
- The advertised CLI version is 0.2.0 so ezBEQ enables Flex HTx input PEQs.
- Protocol crate documentation states that supported builds require `std`.
- Project history lives here; PROGRESS.md holds current status and open work.

### Fixed

- `tools/ota-push.sh` reports an upload the board answered with an HTTP error as failed straight away. It had treated that like a lost reply, and on a weak WiFi link, where its one status read also timed out, waited two minutes for a reboot that never came.
- If the ESP32 reset during USB playback (crash, watchdog, restart, reset button or flashing), the 2x4 HD could keep accepting audio but stay silent until it was power-cycled. It's intermittent: 2 of 8 controlled mid-stream resets did it. The firmware now records active streams, in RTC memory and NVS, and reboots the MiniDSP on the next boot before opening audio. A cable unplug never caused this.
- During a preset change (about 4 s on a 2x4 HD) the ESP32 TCP bridge answered other commands with an error byte after 500 ms, failing a Device Console or CLI session. It now waits up to 8 s, as a Wi-DG would deliver the reply late; 30 of 30 CLI status reads passed across three preset changes.
- A USB host call that never returned (a channel halt after a HID write stalled) hung the ESP32 until it was reset. A supervisor now restarts the board when device-loss cleanup or any USB endpoint halt runs past 30 s, or the main loop stops for five minutes.
- ESP32 HTTP handlers could exhaust internal RAM. A snapshot restore left 16 bytes, which stalled USB and hung the board until it was reset, and measurement mode starved the WiFi PHY into an abort. The HTTP server task now allocates in PSRAM, and the live validation suite (61 checks) leaves internal RAM where it started. The playback worker does too: its per-write statistics used to take internal RAM's low-water mark from about 40 KB to 15 KB during the first sweep of each boot.
- Opening the ESP32 by its `minidsp-<serial>.local` name stalled for 5 s on macOS: the resolver asks for an IPv6 (AAAA) record too and waits for an answer the board never sent. The board now answers that question with an NSEC record saying it has only an IPv4 address (RFC 6762 §6.1), so name lookups take a few milliseconds, and it runs IPv4 only. espressif/mdns doesn't send NSEC, so `app-esp32/components/mdns` carries a patched copy of mdns 1.14.1; the update from 1.4.3 also brings later fixes for fuzzer-found packet-parsing bugs, including unbounded recursion in name decompression.
- The ESP32 login and password-change forms had no username field, so browsers couldn't save or fill the password reliably. Both now carry a hidden `admin` username, the user Basic credentials already use. `tools/ota-push.sh` now says a protected board needs its password instead of reporting that it isn't answering.
- `tools/ota-push.sh` reported "the board kept its current firmware" whenever the upload's reply failed, including when the reply was lost after a complete upload and the board was already rebooting into the new image. It now checks the board first and only reports a failed upload when the board is still up on the same slot and not rebooting.
- A desktop reboot or reset of a MiniDSP behind an ESP32's TCP bridge reported failure. The client waited 12 s for the device to answer again, and behind an ESP32 it answers about 27 s after the command. Network transports now wait up to 45 s; local USB keeps 12 s.
- The desktop daemon advertised itself over mDNS even when its HTTP server was bound to loopback, where no other host can reach it, and a daemon bound to one address advertised every interface's address. It now advertises only addresses it listens on, and only over IPv4: the IPv6 multicast attempts failed on VPN tunnel interfaces and logged errors. The startup banner says when mDNS is off.
- Hosted ezBEQ showed the slot it had last activated as active, even after the preset changed through the HTTP API, the TCP bridge, a snapshot restore or the MiniDSP's remote. The ESP32 now reports the MiniDSP's current preset as the active slot.
- Every MiniDSP reconnect briefly took another 12 to 16 KB of the ESP32's internal RAM: the PEQ shadow loaded from NVS through a fixed 8 KiB buffer plus a decoded copy, and a reboot over HTTP kept the old device state alive while the new one loaded. The device state now lives in PSRAM, which leaves about 6 KB more internal RAM free while a MiniDSP is connected. The NVS read is sized to the blob, and the reboot wait keeps only the old device's identity. A MiniDSP reboot now dips internal RAM by about 1.5 KB.
- While audio played, the ESP32 `/status` page kept requesting device state that the board refuses with 409, logging a failed request every four seconds. It now shows the last known values without asking.
- The ESP32 OpenAPI document described the rattle `bitDepth` as a number; requests take the string `"16"` or `"24"`, as `/play` and the desktop daemon document.
- An open hosted ezBEQ page crash-looped the ESP32 every few seconds. Its Levels worker re-read the catalogue version from flash on a PSRAM stack, which the flash driver forbids. The worker now compares a cached copy that WebSocket connects and bundle updates refresh.
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
- The release and catalogue-sync workflows referenced a `setup-uv` version that doesn't exist; the catalogue sync also waits for GitHub Pages to be enabled instead of failing every four hours.
- The catalogue sync rebuilt and republished the GitHub Pages site every four hours even when nothing had changed, so boards and browsers saw a new site version each time. It compared the upstream catalogue revision with the site's full version string, and the builder counted its own output directory as a source change and stamped each CI build with the clock time. It now skips when the upstream catalogue and the repo commit match the deployed manifest.
- The ESP32 no longer replays its boot log at startup, which printed every boot line twice on serial and pushed real entries out of `/debug/log`.
- CLI, config and HTTP compatibility with minidsp-rs from an offline audit: crossover, compressor, routing and selector forms, command files, TOML arrays and device bindings, HTTP routing/crossover shapes and padded TCP replies.
- Firmware update checks run on one persistent worker with a cooldown instead of a new thread per request, and `/debug/info` no longer reads task names from freed task memory.

### Removed

- The superseded desktop sweep planning redirect; the stable contract stays in [ref-playback.md](docs/ref-playback.md).

### Security

- Browser requests that change state, and WebSocket upgrades, need a local or configured Origin on both the board and the daemon; native clients without Origin are unaffected. DNS-rebound pages are refused. Unlike minidsp-rs, an unset `cors_origin` is no longer permissive.
- WiFi credentials can only be changed in provisioning mode, firmware uploads and catalogue source overrides are off in provisioning, and source overrides need a `dev-updater-override` build.
- Workflow actions are pinned to commit SHAs and the espup download is checksum-verified.
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

[0.2.0]: https://github.com/Averyy/minidsp-open/releases/tag/v0.2.0
