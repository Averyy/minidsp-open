# Current status

History is in [CHANGELOG.md](CHANGELOG.md). The latest release is
[v0.2.0](https://github.com/Averyy/minidsp-open/releases/tag/v0.2.0). Hardware
claims below refer to the 2x4 HD unless another device is named.

## What works

| Area | Implemented and verified baseline |
|------|-----------------------------------|
| Desktop | CLI selectors, env vars, batch files, HTTP/WebSocket/TCP transports, status and supported DSP controls passed live on direct USB and through the daemon. |
| Bridges | Shared 72-byte discovery encoder, desktop per-interface/appliance beacons, default TCP/discovery services, and desktop socket reuse/no-delay/keepalive with client serialization are implemented. Full Wi-DG fingerprint parity remains open. |
| ESP32 | ESP-IDF 6.0.3 boots, joins WiFi, attaches HID/UAC2, serves HTTP/bridges, and recovers after idle unplug/replug and MiniDSP power cycles. Repeated playback and a recorded soak passed with stable RAM and no USB audio errors. The HTTP task, the playback worker and each connected device's state allocate in PSRAM; internal RAM stayed above 32 KB through the full suite. A HID write the MiniDSP stops answering is recovered by a USB bus reset, a snapshot restore resumes after it, and a supervisor restarts the board if a USB host call still never returns. The board runs IPv4 only, and mDNS answers AAAA lookups with NSEC so `.local` names resolve at once. The live validation suite's full destructive profile passes 76 checks on the 2x4 HD. |
| Devices | All 13 exact modern HW/DSP pairs and seven cached legacy config families have source-backed tables and test gates. Only the 2x4 HD has full live control-path verification. |
| Config and recovery | PEQ, routing, gain/mute, delay/invert, crossover, compressor writes, FIR loading, measurement mode, preset copy/reset, reboot, and strict snapshot round trips passed on desktop and ESP32. DSP100 config uses controller shadow where runtime reads aren't authoritative. On the ESP32 with a 2x4 HD, `/config` readback provenance and conditional mute restore (`restore-mutes`) passed: sources move to `shadow` after acknowledged writes, a stale token gets 409 without writing, and snapshot learn keeps factory-default exports strict. |
| ezBEQ | Hosted UI/catalogue, multi-slot load/clear, Levels, and real upstream ezBEQ through the bridge passed on the earlier bundle. The current 2.12.4 bundle loads its 15,582 entries in a browser, streams Levels at four updates a second, and passes the suite's REST, WebSocket and 2.12.4 command-upload checks. In a browser it also passed slot loads and clears, `bq` uploads with their 400 explanations (invalid coefficients, unsupported `filt`/`rs`), Levels after navigating away and back, two clients seeing each other's changes, reconnect after an ESP32 reset, and an offline reload served from the board and its IndexedDB cache. |
| Playback | Desktop and ESP32 sweep-only, marker-only, default marker+sweep, deterministic offsets, and Toslink auto-switch/restore passed on the 2x4 HD. The current fade was listened to and recorded; markers stay on by default. GET playback status is implemented. |
| Rattle finder | CLI, desktop/ESP32 HTTP controls, shared streamed generation, and ESP32 `/status` exist. Labelled tones, start and stop at -40 dB, busy exclusion, volume restoration, 0.5 dB step rejection, rounded log-spaced labels and the 31-row `/status` window passed with a 2x4 HD and UMIK-1. |
| OTA and tooling | Dual-slot OTA, rollback, firmware API/page, release checks, build/release workflows, and local flash/upload wrappers are implemented. `tools/ota-push.sh` installs passed both ways between `ota_0` and `ota_1`, each confirming itself on the network. Both rollback paths passed on hardware: an image that panics before confirming boots the previous slot on the next reset, and one that never reports itself reachable marks itself invalid after 3 minutes and reboots into it. A truncated upload is rejected with 400 and the board keeps its firmware. |
| Apple TV | The desktop spike pairs, reconnects with stored credentials, decodes events, and suggests catalogue matches. Real Infuse/Plex titles and durations work; Companion metadata doesn't identify a safe BEQ audio variant, and Netflix payloads are empty. |

Host tests and ESP32 builds have passed. Newer changes still need the hardware
and browser checks below.

## Open verification and fixes

### Playback and rattle finder

- [ ] Run a long rattle session for RAM stability. A 4096-step session already held internal RAM flat on the ESP32 (32.6 KB free while playing, the same as a 6-step session; 38.5 KB after, against 38.7 KB before).
- [ ] Exercise rattle teardown failure and panic cleanup on the board; they are host-tested only. On the 2x4 HD, `/status` shows "Rattle running" during a session and "Idle" after it, "/play running" during a plain `/play`, and the USB and TOSLINK source names.
- [ ] Test the desktop rattle daemon on real audio hardware.
- [ ] Check the provisioning cases on the board: `/api/wifi/connect` present during provisioning, and firmware upload and source override refused there. Passed from both the board's IP and its `.local` name: foreign-origin POSTs and WebSocket upgrades are refused, the board's own origins and preflights pass, `/api/wifi/connect` is absent on WiFi, and `/firmware/check` cools down. Playwright's bundled Chromium can't resolve `.local` names here, so the by-name checks used curl through the macOS resolver.
- [ ] Check the rest of the optional password on the board: protected provisioning; the ten-second BOOT recovery and its LED; HA xuio URL credentials; a cross-origin preflight and authenticated request from an allowed local origin; another address's failed logins not blocking the owner; WebSockets (including `?poll=true` streams opened before protection) closing after logout, session expiry and password changes, even with all 13 sockets busy; PBKDF2 timing is measured: a login takes about 750 ms at 10,000 iterations with the hashing crates at opt-level 3 (about 1,030 ms at "s"), and Basic is then cached for five minutes; the ESP32-S3's hardware SHA through mbedtls would cut it further. Passed on the board: 401, 403 and 429 replies, cookie, Bearer and cached Basic sessions, WebSocket upgrade rules, TCP 5333 and UDP 3999 stopping and resuming, CLI `--password-file` and `MINIDSP_PASSWORD`, `tools/ota-push.sh --password-file`, and in a browser: `/status`, hosted ezBEQ and `/update` redirecting to the login page with no credential dialog, a wrong password, login with an HttpOnly SameSite=Strict session cookie, logout, and clearing the password from `/status`.
- [ ] Check the interrupted-stream recovery after a panic or watchdog reset during playback (RTC path). The EN-pin path (NVS) recovered sound on the 2x4 HD, verified with the mic. The stuck state is intermittent: 2 of 8 controlled EN resets mid-stream, and none of 2 software restarts in the driver harness.
- [ ] Support changing volume during a running rattle session after concurrent HID/audio is signed off. For now, volume passes use digital scaling under one master volume; HID routes return 409 while streaming.
- [ ] Verify physical ESP32 output duration against the sample plan and the `/sync` latency target; enqueue completion isn't physical output completion.
- [ ] Recheck non-default formats, back-to-back sample-rate changes, malformed requests, and the HID-only-device error on suitable hardware.
- [ ] Check cold-boot `Toslink -> /play -> Toslink` and unplug-mid-play/replug cleanup. Idle reconnect and warm source restore have passed.
- [ ] Broaden desktop playback coverage to other source families/devices, and finish desktop `POST /sync` verification/cleanup; the 2x4 HD clock/offset contract has passed.
- [ ] Finish the targeted hardware checks for close retries, short-decay drain, and hung-control recovery; the six minidsp-open consumer bugs are fixed in code.
- [ ] Keep the two open-sub-optimizer follow-ups: convert playback offsets to the capture rate and budget capture time for playback preparation.

Details: [ESP32 audio](docs/TODO1-esp32-usb-audio.md),
[consumer follow-ups](docs/TODO2-usb-audio-consumer-bugs.md), and the
[playback contract](docs/ref-playback.md).

### Device control and compatibility

- [ ] Find out why the 2x4 HD sometimes stops answering HID over the ESP32 during long write bursts ([bug-usb-hid-write-stall.md](docs/bug-usb-hid-write-stall.md)). The board now recovers by itself: a 20 s grace period, then a USB bus reset, and a snapshot restore resumes from the preset that failed (checked with an injected stall). The next step is long restores over direct USB from a computer, to tell an ESP32-side trigger from a MiniDSP one.
- [ ] Make the other long ESP32 write paths survive a USB stall the way a snapshot restore does: ezBEQ loads, `preset/reset-all`, measurement mode, `/config`, and writes through the TCP bridge. A stall there fails the request, though the board recovers ([bug-usb-hid-write-stall.md](docs/bug-usb-hid-write-stall.md)).
- [ ] Serve other ESP32 requests during a snapshot restore. A restore takes about 65 s on the 2x4 HD and holds the board's single HTTP server task the whole time, so `/status`, hosted ezBEQ and new WebSocket connections wait until it finishes. Handing long handlers to a worker task would fix it.
- [ ] Finish live validation: the ESP32 lane's manual audio, reconnect and Device Console checkpoints, and the desktop direct-USB lane (the MiniDSP on the computer's USB). The ESP32 full destructive profile passes 76 checks with its checkpoints skipped. The desktop daemon lane, driving the 2x4 HD through the ESP32's TCP bridge, passes 51 checks with playback skipped; desktop `restore-mutes` and readback ran there, but desktop playback needs the direct-USB setup.
- [ ] Keep ezBEQ slot titles honest when another tool rewrites a preset. The active slot follows the MiniDSP, but a title stays what ezBEQ last loaded through a snapshot restore, Device Console or minidsp-rs edits, until the next load or a board restart; the board can't tell which catalogue entry, if any, restored filters match.
- [ ] Finish runtime compatibility signoff after the [offline source/host audit](docs/NOTES-compatibility-audit.md): Device Console interactive operations plus concurrent HTTP writes/status/levels, two physical USB devices and live indexing, desktop broadcast/multicast discovery, ESP32 no-device discovery, audible compressor behavior, and the actual ezBEQ process/browser rerun. The audit fixes desktop config keys/bindings/CORS, CLI crossover/compressor/routing/file parsing, and padded TCP replies; in-memory host regressions cover the changed paths. Checklist: [docs/todo-checker.md](docs/todo-checker.md).
- [ ] Run the compatibility stream tests over real sockets/LAN and exercise configured static/network device startup. This audit's sandbox rejects local socket binds, so host HTTP/WebSocket/bridge tests use in-memory duplex streams. New upstream routing/crossover `/config` aliases are desktop-only; ESP32 still needs matching request support before claiming those aliases on both targets.
- [ ] Finish official Device Console interactive testing through the bridge: status, volume, mute, source, presets, routing, PEQ, and compressor view/edit. Initialize-to-default already passed.
- [ ] Audibly retest the compressor on loud material with a threshold that triggers it; encoding/address investigation is closed.
- [ ] Test HT-only bass management and routing polarity on hardware that exposes them, and perform multi-device live verification.
- [ ] Replace the conservative ESP32 attach-settle delay with earlier readiness polling and a bounded fallback.
- [x] Turn the canonical Device Console HID capture into labelled fixtures: 54 sanitized exchanges cover 13 opcodes, pass client/queue replay, and cross-check the opcode table in [the capture notes](docs/NOTES-device-console-hid-logging.md); typed probe-selector/DSP-version gaps are documented.
- [ ] Check desktop meter reads during a preset change: the Device Console capture has one meter reply that took ~2.8 s, longer than the client's 750 ms meter timeout. On the ESP32 a preset change takes about 4 s; HTTP status and meter reads queue behind it, and CLI status reads through the TCP bridge pass now that the bridge waits for it.
- [ ] Prove the exact runtime identity (hw_id, DSP version, USB PID) of HA-DSP, DDRC-22, DDRC-88 and CDSP-8x12/DL with `minidsp probe` on real units. Static research settled Harmony DSP (Device Console normalizes it to PADSP) and how Device Console selects a definition: [identity and Dirac notes](docs/NOTES-device-identity-and-dirac.md).
- [ ] Keep importer/codegen changes source-backed and regenerate coverage artifacts when device evidence changes.
- [ ] Confirm the HT/HTx selectable-source bitmap at `0xFFEA` on live hardware. The typed protocol parser/encoder and tests exist from static research (HT/HTx all-enabled masks: `0x1E`/`0x1F`); no client reads or writes are enabled.
- [ ] Find the live addresses and value ranges of the advanced Dirac controls (`mux_bypass`, `dirac_delay`, `mixer_fir`, `fir-ctrl`) on a Dirac-capable unit; their preset layouts are documented and there is no standalone `iir-ctrl` descriptor.
- [ ] Publish 0.2.1 with the OTA request-buffer fix. ESP32 0.2.0 can't check for or install GitHub releases (see CHANGELOG Unreleased), so a board on 0.2.0 needs that one update uploaded through `/update` or `tools/ota-push.sh`.
- [ ] Prove an ESP32 installs a GitHub release end to end with `POST /firmware/update`. With the fix, the 2x4 HD board's release check follows both GitHub redirects and reads v0.2.0's `firmware.json`; installing needs a release newer than the board's version. At about -97 dBm, 2.5 MB WiFi uploads broke off; at -90 dBm they work.
- [ ] Remove the external-component Cargo tracking workaround once esp-idf-sys tracks component directories itself.
- [ ] Keep PWR-ICE compatibility, later ezBEQ expansion, and custom-PCB VBUS GPIO control deferred. DevKitC boards don't need GPIO-driven VBUS.

### Wi-DG parity

The [parity plan](docs/plan-widg-parity.md) and
[wire reference](docs/ref-widg-reimplementation.md) retain the detailed gates.
Current discovery uses a shared 72-byte layout at one-second intervals. Desktop
already has per-interface/appliance beacons, socket options, multiple clients,
and default bridge/discovery services. The remaining gates are:

- [ ] P0: compare beacon bytes/cadence with a physical Wi-DG; reconcile the branded SoftAP SSID/hostname and AP addressing with the planned DHCP pool/lease and AP+STA behavior; finish ESP32 TCP socket options, multi-client fan-out, and serialization. Verify desktop defaults against the zero-config discovery gate.
- [ ] P1: reconcile ESP32/desktop mDNS hostname, HTTP service and TXT path with the fingerprint plan; add the planned HTTP-to-HTTPS redirect, certificate and Server header; reconcile STA DHCP hostname; use no-device appliance identity on ESP32 (desktop already does).
- [ ] P3: default WPA2 provisioning with an opt-in open AP; Wi-DG-style management BasicAuth beyond the optional ESP32 password; certificate lifetime/subject; authenticated-only or absent log serving.
- [ ] Decide whether desktop discovery should advertise a local USB bridge by default or offer an opt-out.

## Next steps and plans

1. Finish the current 2x4 HD hardware/browser checks above before extending support claims.
2. Complete the [destructive validation suite](docs/TODO1-destructive-validation-suite.md) hardware gates: staged desktop/ESP32 runs and real audio/recovery/Device Console approvals, the separate direct-USB round-trip, wider ESP32/profile coverage, richer product-specific mutations, FIR/Dirac truthfulness, and multi-device USB indexing.
3. Finish [ESP32 token sync](docs/TODO2-token-sync.md): token-gated per-preset persistence, mutation/token commit handling, and the optional atomic Device Console XML helper.
4. Lock in [ATV backend/session rules](docs/TODO2-ATV-Companion.md), then prove ESP32 reconnect/watch feasibility before custom UI and auto-reset work. Keep manual BEQ apply and the planned `/classic` fallback.
5. Continue Phases 2–5 of the [shared architecture plan](docs/TODO3-shared-arch-codebase.md); Phase 1 pure helpers are shared in `protocol/`. Workflow planners and later phases remain open; keep platform runtimes separate.
6. Keep [Plex/Trakt enrichment](docs/TODO3-Plex-Trakt-Int.md), [12 V trigger control](docs/TODO4-12v-trigger.md), and [PWR-ICE support](docs/TODO4-pwr-ice-compatibility.md) scoped to their remaining research, implementation, and hardware gates.

## Validation

- `uv run python tools/extract_device_support_matrix.py`
- `cargo test -p minidsp-protocol`
- `cargo test`
- Hardware commands and manual checks: [live validation](docs/ref-live-validation.md).
