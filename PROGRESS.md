# minidsp-open Progress

This file is a task list and status board.

Permanent technical detail belongs in:
- `PROJECT.md`
- `docs/ref-compatibility.md`
- `docs/ref-device-coverage.md`
- `docs/ref-devices.md`
- `docs/ref-legacy-devices.md`
- `docs/generated/device-support-matrix.json`

## Current Status

- Apple TV Companion phase-0 desktop spike:
  - **The isolated spike crate now exists and host-native discovery is working** — `atv-companion-spike` is a separate Rust workspace tool with repo-local scratch persistence under `.scratch/atv-companion/`, and `cargo +esp run -p atv-companion-spike -- scan --timeout-secs 3` now discovers the local Companion services and persists them to `devices.json`.
  - **Discovery now merges multi-address Companion advertisements into one stable device record** — the spike keys devices by the Apple TV's stable `rpMRtID` when available, merges repeated mDNS resolves into one record, and prefers IPv4 for `connect_host` so a single Apple TV advertising IPv4 plus multiple IPv6 addresses no longer produces a bad link-local pairing target. On the latest live scan, `Example TV._companion-link._tcp` resolves as device `11111111-2222-4333-8444-555555555555` with reachable target `192.0.2.12:49153`.
  - **Pairing flow is partially implemented and multiple protocol drifts are now corrected** — the CLI now starts Companion pairing before asking for the four-digit PIN, matching the Apple TV flow that shows the PIN only after pair-start, and auth frames now carry `_x` transaction IDs like pyatv instead of relying on bare pairing payloads. The next live step is to retry pair-setup / pair-verify against the `Example TV` Apple TV with the improved auth framing, persist credentials to `.scratch/atv-companion/credentials.json`, and then validate reconnect plus now-playing dumps through `watch`.
  - **The next pairing retry now uses pyatv-style HAP SRP proof math instead of the crate defaults** — the spike's pair-setup path now computes the SRP password hash and `M1` / `M2` proof flow to match pyatv / `srptools` more closely, after live runs showed the Apple TV displaying a PIN but rejecting submitted codes with HAP `Authentication (0x02)` errors. The next live result should tell us whether the rejection was caused by the earlier proof mismatch or by a later pair-setup payload problem.
  - **Live pair + reconnect is now proven against the real `Example TV` Apple TV** — after switching the pair-setup proof logic to the HomeKit-compatible pyatv-style SRP flow, the spike successfully paired with device `11111111-2222-4333-8444-555555555555`, persisted credentials to `.scratch/atv-companion/credentials.json`, and immediately reconnected in a fresh `watch --once` process using the stored credentials. The credentials file is currently created with restrictive `0600` permissions.
  - **Reconnect state is keyed by the stable Companion device ID, not by a single IP string** — the spike stores credentials under the Apple TV's `rpMRtID` / device ID and refreshes discovery before reconnect, so DHCP churn, hostname changes, and address-family changes do not require re-pairing as long as the same Apple TV is rediscovered. The current `Example TV` advertisement is exposing one IPv4 plus multiple IPv6 addresses rather than two distinct IPv4 addresses, and the same stored credential record has now been re-used successfully against an alternate IPv6 address for the same device ID.
  - **IPv6 reconnect support is now fixed for explicit host overrides** — the client connect path no longer formats raw targets as `host:port` strings, which previously broke manual IPv6 reconnect attempts. The spike now has a `watch --host <address>` override, and `watch 11111111-2222-4333-8444-555555555555 --host 2001:db8::12 --once` succeeded using the already-paired credential material instead of re-pairing.
  - **Now-playing event capture is now proven on real Infuse playback** — the spike now subscribes to `NowPlayingInfo`, preserves async Companion events instead of discarding them while waiting for request responses, and decodes `NSKeyedArchiver` / binary-plist payloads embedded in `NowPlayingInfoKey` and `UpNextInfosKey`. On the live `Example TV` Infuse sample, repeated `watch --once` runs now capture a stable `NowPlayingInfo` event about `0.84–0.85 s` after `FetchCurrentNowPlayingInfoEvent`, with normalized fields including title `Example Film A`, duration `12817.56 s`, playback-state code `1`, and a stable player/session identifier.
  - **The current Infuse sample is now pinned as a comparison artifact** — the latest good `Example TV` / Infuse now-playing event has been saved to `.scratch/atv-companion/debug/infuse-sample.json` so later Netflix / Plex runs can be compared against a fixed baseline instead of whichever row happens to be last in `metadata.jsonl`.
  - **Plex now has a pinned baseline sample, but metadata quality is still limited** — live Plex playback of `Example Film B` has now produced repeated stable `NowPlayingInfo` events with duration `6843.75 s`, playback-state code `1`, playback rate `1.0`, and a stable player/session identifier `aaaaaaaa-3333-4333-8333-333333333333`. The pinned sample is `.scratch/atv-companion/debug/plex-sample.json`, but the payload still omits `bundleID`, `programID`, and meaningful progress just like the current Infuse sample.
  - **Amazon Prime Video now provides a stronger third-party baseline sample** — live Prime playback of `Example Film C` has now produced four stable `NowPlayingInfo` events with bundle ID `com.amazon.aiv.AIVApp`, canonical ID `example-content-1`, Amazon content ID `example-content-2`, duration `6255.876 s`, and non-zero progress once playback settled. The pinned sample is `.scratch/atv-companion/debug/amazon-prime-sample.json`, and settled event latency is still roughly `0.84–0.85 s` after the explicit fetch trigger.
  - **Apple TV app movie playback is currently the best metadata case** — live Apple TV playback of `Example Film F` has now produced repeated stable `NowPlayingInfo` events with bundle ID `com.apple.TVWatchList`, canonical ID `example-content-3`, program ID `1234567890`, iTunes Store ID `1234567891`, duration `9699.928001 s`, and non-zero progress. The pinned sample is `.scratch/atv-companion/debug/apple-tv-movie-sample.json`, and the good event form now arrives in roughly `0.90 s` once the app settles.
  - **Netflix is currently a negative metadata case with the present handshake / event set** — during active Netflix movie playback, the spike captured at least twelve consecutive `NowPlayingInfo` events over more than thirty seconds, but every one decoded to an empty `TVRCNowPlayingInfo` object with `metadata = null`. The pinned negative sample is `.scratch/atv-companion/debug/netflix-sample.json`, and `FetchUpNextInfoEvent` stayed stale on the previous Prime title instead of tracking the live Netflix session.
  - **The watcher now follows pyatv's Companion attention-state bootstrap more closely, but Netflix is still empty** — the spike now subscribes to `TVSystemStatus` in addition to `SystemStatus`, and a live `FetchAttentionState` request succeeds with `state = 3` while Netflix playback is active. That proves the device accepts the attention-state request path, but the subsequent Netflix `NowPlayingInfo` events are still empty shells, so the current blocker is not just "missing attention-state bootstrap".
  - **The last obvious pyatv bootstrap drift is also now closed, with no Netflix improvement** — the spike now issues `_touchStart` and does so before `_sessionStart`, matching pyatv's Companion connect order more closely (`_systemInfo -> _touchStart -> _sessionStart -> _tiStart`). Live follow-up runs still produced only empty `NowPlayingInfo` shells and stale `UpNext` values when testing the problematic app case, so the remaining Netflix gap does not appear to be caused by that handshake-order mismatch either.
  - **The empty Netflix payloads now look genuinely sparse, not just badly decoded** — the repeated empty `NowPlayingInfoKey` blobs captured during the failing Netflix case are only about `538` bytes each, while the working Prime, Plex, and Apple TV samples carry much larger embedded bplists on the order of `73 KB`, `103 KB`, or `252 KB`. That makes the current evidence much stronger that the useful metadata is simply absent from the Companion payloads we are receiving in that case, rather than merely hidden inside the same blob.
  - **Metadata quality is materially better but still app-dependent** — Infuse and Plex currently report stable titles and durations but still leave `bundleID`, `programID`, and progress effectively missing; Prime exposes a real app identity plus stronger content identifiers but still omits `programID` for the current movie sample; Apple TV app playback currently provides the strongest direct now-playing payload; and Netflix still collapses to an empty `TVRCNowPlayingInfo` payload in the existing watcher flow. `FetchUpNextInfoEvent` can decode rich Apple TV+ watchlist-style entries when available, but it is not currently a dependable source for third-party playback.
  - **Infuse and Plex still do not expose a trustworthy app identity through the current Companion path** — both pinned samples still show `bundleID = null`, `programID = null`, `iTunesStoreIdentifier = 0`, and the generic canonical token `com.apple.tvremote.canonical-identifier.unknown`; the only durable-looking extra field is a UUID-like `identifier` / `additional_identifier`, which currently looks like a playback/session token rather than an app allowlist key. Those fields are therefore not useful as stable routing keys; the right read is movie detection from title plus duration, with final BEQ variant choice kept manual.
  - **Repeated Infuse and Plex title swaps now make that identifier conclusion much stronger** — on Infuse, changing playback from `Example Film A` to `Example Film D` changed `additional_identifier` from `aaaaaaaa-1111-4111-8111-111111111111` to `aaaaaaaa-2222-4222-8222-222222222222`; on Plex, changing playback from `Example Film B` to `Example Film E` changed it from `aaaaaaaa-3333-4333-8333-333333333333` to `aaaaaaaa-4444-4444-8444-444444444444`. In both apps, `playerIdentifier` stayed `MediaRemote-DefaultPlayer`, `bundleID` stayed null, and the canonical ID stayed generic, which is strong evidence that these UUID-like values are per-playback tokens rather than stable app identifiers. The comparison captures are pinned at `.scratch/atv-companion/debug/infuse-comparison-sample.json` and `.scratch/atv-companion/debug/plex-comparison-sample.json`.
  - **Phase-0 watcher proof is now good enough to move into movie-level matching work** — the approved Companion-only policy is: require an exact normalized title match, allow duration drift within `±15 min`, and use the result only to suggest likely movie matches. Infuse and Plex are acceptable on title-plus-duration alone for this pass; Netflix remains out of scope because the current Companion payloads are still empty. The matcher writes suggestion records to `.scratch/atv-companion/matches.jsonl` and does not auto-apply anything.
  - **The first matcher replay now works against the captured Apple TV samples, and it exposed the real product boundary** — the spike can now load the upstream ezBEQ `database.json`, normalize titles, apply the approved `exact title + ±15 min` rule, and write suggestion records to `.scratch/atv-companion/matches.jsonl`. On the current live sample set, it found plausible title matches for `Example Film C`, `Example Film D`, and `Example Film B`, but all three stayed ambiguous because the BEQ catalogue contains multiple same-title variants with similar runtimes. It found no match for `Example Film F`, `Example Film A`, or `Example Film E` in the current catalogue snapshot.
  - **Companion-only matching is now explicitly ruled out for auto-apply, even on a single BEQ hit** — the current watcher cannot reliably tell us which app or audio mix is actually playing, and AVSForum guidance is clear that BEQ should match the specific source audio format or it becomes unsupported and potentially unsafe. So even if the matcher narrows a movie down to one catalogue row, that row is still only a best-guess suggestion, not proof that the active mix matches. The correct next step is movie-level suggestion plus manual BEQ variant selection, not automatic filter application.
  - **The next planned ATV work is now backend/session lock-in plus an early ESP32 feasibility gate, not an immediate UI build** — the desktop watcher is considered stable enough to move forward, but the updated product rule is that ATV should be scrapped early if the watcher cannot be made to work acceptably on ESP32. So the immediate plan is: lock the backend session model, prove a minimal ESP32 reconnect/watch path, and only then spend time on the narrow custom UI documented in `docs/TODO2-ATV-Companion.md` while keeping stock ezBEQ available as `/classic`.
  - **The ESP32 storage budget has now been rechecked against the current image and site artifacts, and it does not look like the blocker** — the live partition layout is `4 MB` app plus `11.94 MB` LittleFS data, the current release firmware image is about `2.1 MB`, and a fresh `tools/build-catalogue.py` run produced about `4.3 MB` total of embedded ezBEQ data (`catalogue.bin` plus UI files). That shifts the real ESP32 risk toward pairing/reconnect stability, mDNS/discovery, crypto/session memory pressure, and long-running watcher reliability rather than raw flash capacity.
  - **The spike reconnect path is now less brittle for manual-host workflows** — `pair --host` now persists credentials under the Apple TV's stable paired identifier instead of a synthetic `host:port` key, `watch --host` can resolve from stored credentials without requiring a fresh Bonjour hit first, and the selected host is written back into the scratch device cache as a reconnect hint. That keeps the desktop-only spike isolated while closing the main stable-ID / alternate-address regression found in review.
  - **Protocol and matcher hardening from the ATV review is now landed and build-verified** — OPACK integer encoding no longer corrupts positive values above signed byte/word ranges, encrypted frame reads now refuse unreasonably large payload lengths before allocating, matcher replay now tolerates malformed JSONL rows instead of aborting on the first bad line, and title matching now includes a narrow movie-suffix fallback that closes the known real `F1 & The Movie` miss against the current ezBEQ catalogue snapshot. Local verification: `cargo +esp test -p atv-companion-spike` (`9` tests passing) plus a direct matcher replay against `.scratch/atv-companion/catalogue/database.json` now returns the expected ambiguous `F1` candidate set for that title shape.

- 2026-04-13 Device Console reset-flow reverse engineering:
  - **The official reset-to-defaults flow is now code-verified** — extracted Device Console sources confirm that `resetActive()` writes a full default preset image plus bypass block, `resetActiveOnlyOnDevice()` can target a specific preset slot directly, and the UI's global initialize path `resetAllPresets(...)` resets preset 1, calls `copyPresetSync()` to propagate it across the remaining slots, updates modification tokens / preset files, and normally schedules a reboot at the end. This confirms that "Initialize All Presets to Default" is app-driven controller logic, not a hidden device-side factory-reset opcode.
  - **The first non-Dirac reset implementation is now in the codebase and build-verified** — `protocol/build.rs` now compiles the checked-in `app/Audio Firmware/*/DSP_default_parameters.xml` corpus into static default-preset images for DSP `100`, `110`, and `115`; desktop and ESP32 reset flows now write the preset-values block, write the preset-bypass block, verify program status, write the modification token, reload the same preset slot, and expose active-preset / preset-N / initialize-all / reboot routes. Local verification passed with `cargo +esp test -p minidsp-protocol --lib`, `cargo +esp test -p minidsp --lib`, `cargo +esp test -p minidsp-daemon --lib`, and `cd app-esp32 && esp cargo build`. Dirac families remain intentionally excluded until the extra official-app reset payload is modeled exactly.
  - **The first live 2x4 HD reset attempt exposed two concrete compatibility bugs, and the follow-up reflash now live-proves the fix on hardware** — the ESP32 serial log originally showed `USB IN wait failed for [07, 02]: USB timeout`, which traced to our shared matcher discarding valid event-style `FinishFlashBlock` completions (`0x02 <block_id>`). Separately, the real Device Console log showed `Disconnected while waiting for reply to GetWriteFlashBlockDspCmd 0x06`, which traced to the ESP32 raw TCP bridge still using the generic `5 s` USB timeout for flash/reset command families that already need the `35 s` slow-command budget on desktop. The follow-up fix now accepts `FinishFlashBlock` events as valid completions, preserves them as `0x02 <block_id>` on the TCP bridge, and applies the slow timeout budget to raw bridge `CopyPreset`, `Reset`, `WriteFlashBlock`, and `FinishFlashBlock` traffic. After reflashing the attached ESP32-S3 board `firmware-board`, `POST /devices/0/preset/reset-all` completed successfully on the physical 2x4 HD `912345`, returned `{"status":"all_presets_reset","rebooted":true}`, rebooted the unit, and the bridge redetected the device cleanly with the expected `0xFEEF8FCF` recovery tokens in all four preset slots.
  - **The ESP32 reboot-recovery path is now live-proven again on the attached board after the April 13 reclaim follow-up** — the firmware now does fast disconnect cleanup, preserves fresh attach state across teardown races, and waits a full `20 s` after `NEW_DEV` before the first HID open. On the live `firmware-board` + physical 2x4 HD `912345` retest, both `POST /devices/0/reboot` and `POST /devices/0/preset/reset-all` re-enumerated cleanly, claimed HID on the first delayed attempt after reboot, restored the PEQ shadow, reopened UAC2, logged `HTTP reboot wait: device recovered after 27.096s` / `27.297284s`, and returned `200` (`{"status":"rebooted"}` / `{"status":"all_presets_reset","rebooted":true}`). A separate boot regression is also contained on the same image: the WiFi monitor now starts before the updater, so the updater can still fail task creation as a logged non-fatal `Not enough space` condition without taking the board or HTTP API down.
  - **The real Device Console reset path is now live-verified on the official macOS app** — on 2026-04-13, the stock MiniDSP Device Console completed `Initialize to Default Settings` against the ESP32 TCP bridge with no error dialog. The app log shows both `GetFinishFlashBlockDspCmd` sends completing cleanly, then `Reloading Preset 1...`, `ChangePresetSlotDspCmd sent`, `reloadCurrentPreset: preset-change-completed`, `Reload Preset Completed`, and `Initialize-to-Default completed.` The prior failure signatures (`Disconnected while waiting for reply to GetFinishFlashBlockDspCmd 0x07`, `ChangePresetSlotDspCmd Error: sendDspCmd() returns false.`, `Cannot Reload with new preset settings`) did not recur in the live retest, and UART stayed clean aside from the expected TCP client connect/disconnect while the board remained in `MiniDspReady`.

- 2026-04-14 ESP32 disconnect-cleanup regression retest on the attached `firmware-board` board + physical 2x4 HD `912345`:
  - **The unplug/replug regression no longer resets the board** — after flashing the disconnect-cleanup fix, a fresh manual unplug/replug left the ESP32 running, logged the expected `USB device disconnected` and `UAC2 device disconnected` events, fell back to `MiniDspUnavailable`, rescanned, re-enumerated the MiniDSP at a new USB address, reprobed protocol state, reopened UAC2, and returned to `MiniDspReady` with no panic/reset in between.
  - **Scope of that retest** — this pass verified idle unplug/replug recovery after the new cleanup changes. It did not include an active `/play` run in the serial log, so explicit unplug-during-`/play` signoff is still pending.

- 2026-04-12 ESP32 live reconnect bench on the attached `firmware-board` board + physical 2x4 HD `912345`:
  - **USB disconnect/reconnect recovery is now live-proven end to end** — after a manual unplug/replug, UART showed the expected disconnect cleanup, device re-enumeration, the then-current ~`9.9 s` firmware wait, protocol probe, PEQ shadow restore, and `MiniDspReady` LED transition. The board then passed sequential post-reconnect surface checks: `GET /devices/0`, reversible `POST /devices/0/preset/1` then `/preset/0` with matching readback, `./target/debug/minidsp --daemon-url ws://192.0.2.251:5380/devices/0/ws status`, and `./target/debug/minidsp --tcp 192.0.2.251:5333 status`. UART stayed clean during the WS/TCP follow-up with no new transport warnings. This older 2026-04-12 result is now superseded by the explicit `20 s` attach-gate recovery path documented in the 2026-04-13 update above.
  - **MiniDSP power-cycle recovery and ESP32 cold-start attach are also live-proven** — with the MiniDSP power removed, the board immediately fell back to the amber `MiniDspUnavailable` state; after MiniDSP power returned it reattached cleanly, restored PEQ shadow, and passed the same read/write/raw-WS/TCP checks. A separate controlled ESP32 reboot with the MiniDSP left powered on also came back cleanly: WiFi reconnected, HID/UAC2 attached, the then-current ~`9.9 s` wait completed, protocol probe succeeded on the first attempt, `MiniDspReady` asserted, and follow-up HTTP/WS/TCP plus reversible preset `1 -> 0` writes all succeeded. The later 2026-04-13 reclaim follow-up replaced that attach timing with the explicit `20 s` gate while preserving clean recovery.
- 2026-04-12 ESP32 preset-switch stability and desktop-compatible ezBEQ shell-out follow-up on the attached `firmware-board` board + physical 2x4 HD `912345`:
  - **Desktop-compatible `minidsp -f` preset batching is now live-proven on the ESP32 TCP bridge** — `./target/debug/minidsp --tcp 192.0.2.251:5333 -f <tempfile>` successfully loaded one coefficient set into preset `1`, a different set into preset `2`, left preset `0` untouched, and the matching clear file restored `bypass: true`, `mute: false`, and `gain: 0.0` on presets `1` and `2` while preserving the stored coefficients, matching ezBEQ's real load/clear semantics.
  - **The repeated preset-switch USB-host crash is fixed on the live board** — the earlier stress pass exposed an ESP-IDF host assert (`assert failed: hcd_urb_dequeue ...`) after we intentionally timed out `SetConfig` too early on the ESP32 side. `app-esp32/src/device.rs::set_config_with_verify` now waits up to `5 s` for the normal preset-complete response instead of forcing the USB timeout-recovery path on normal switches, and `minidsp/src/client.rs` now gives raw bridge `SetConfig` responses enough budget (`12 s`) to wait for that verified reply. After reflashing, the same multi-preset load/clear script completed cleanly, a 10-switch TCP-bridge `config` stress loop completed cleanly, UART showed no `USB IN wait failed for [25, ...]` warnings, and the board stayed up with HTTP still responsive.
- 2026-04-12 ESP32 live verification follow-up on the attached `firmware-board` board + physical 2x4 HD `912345`:
  - **Embedded ezBEQ preset targeting is now live-proven across multiple slots** — the attached board successfully loaded `10 Cloverfield Lane` into slot `2`, `10 Minutes Gone` into slot `3`, and `1917` into slot `4` via both `PUT /api/1/devices/master/filter/{slot}` and `PATCH /api/3/devices/master`. `GET /devices/0/config` showed the expected distinct PEQ coefficients on presets `1`, `2`, and `3`, and switching away/back restored each slot's own filter set instead of bleeding between presets.
  - **ezBEQ clears are also live-proven** — `DELETE /api/1/devices/master/filter/3`, PATCH clear with `{"slots":[{"id":"4","entry":""}]}`, and the final cleanup clear on slot `2` all returned the targeted presets to bypassed identity PEQs while leaving other slots alone. `/api/2/devices` ended the pass back at four `Empty` slots with slot `1` active.
  - **Input-channel `/config` parity is no longer a live gap** — `POST /devices/0/config {"inputs":[{"index":0,"gain":0.0,"mute":true}]}` changed only input `0` on the real 2x4 HD, `GET /devices/0/config` read the new gain/mute state back correctly, and restoring the original input-0 value returned the same pre-test readback.
  - **Compat WebSocket text-frame behavior is now live-proven** — a browser-side `WebSocket('ws://192.0.2.251:5380/devices/0')` text frame returned immediate status JSON with `master`, `input_levels`, and `output_levels`, matching the documented compatibility behavior instead of only supporting poll/levels query modes.
  - **HTTP catalogue-proxy abort did not reproduce a persistent server wedge when tested sequentially** — a controlled early-abort `GET /database.json | head -c 1024` still logged the expected client-side short write, but immediate follow-up `GET /api/1/version` and `GET /devices/0` succeeded. The earlier broken-HTTP window now looks consistent with the combination of parallel probe pressure plus a real USB disconnect/re-enumeration during that pass, not with a simple single-client catalogue abort.
- 2026-04-12 ESP32 live polish pass on the attached `firmware-board` board + physical 2x4 HD `912345`:
  - **Inline sine playback is reliable again on the real board** — `app-esp32/src/http_server.rs` now gives the `POST /devices/0/play?test=sine` diagnostic the same safer write budget as the normal `/play` path and retries transient stream-write stalls before failing. After reflashing the attached board, the previously-broken diagnostic completed `500` writes in `4.81-4.93 s` instead of aborting after ~`0.57 s`.
  - **Desktop WS clients now close the raw bridge cleanly** — `minidsp/src/queue.rs`, `minidsp/src/client.rs`, and `app-desktop/src/main.rs` now explicitly close transports after CLI commands, so `cargo +esp run -p minidsp-app -- --daemon-url ws://192.0.2.251:5380/devices/0/ws status` no longer leaves the ESP32 printing the false-positive `httpd_ws_get_frame_type: Failed to read header byte (socket FD invalid)` warning on disconnect.
  - **A few remaining ESP32 error-swallow points were tightened** — the sine-test spawn-failure path now logs source-restore errors instead of dropping them, updater `version.txt` persistence now warns on failure, and the embedded ezBEQ `/ws` handler now uses the same logged send/recv helpers as the compat/raw WS paths instead of silently ignoring send failures.
  - **Latest live verification on the reflashed board** — valid `POST /sync` returned matching `clientTimeUs/serverTimeUs`, CLI `status` via `/devices/0/ws` still returned the expected live status, `POST /devices/0/play?test=sine` completed cleanly on UART, and the ezBEQ browser UI loaded again with no Playwright console warnings while `/ws` established its detached sender normally.
- 2026-04-12 ESP32 live retest follow-up on the attached `firmware-board` board + physical 2x4 HD `912345`:
  - **Out-of-range compat requests stay inside the JSON contract** — `app-esp32/src/http_server.rs` now registers a late `/devices/*` mutation fallback, so out-of-range requests such as `POST /devices/1/peq/reset` and `POST /devices/99 {"mute":true}` return JSON `404 {"error":"not found"}` instead of leaking the ESP-IDF HTML `405` responder.
  - **Compat-side PEQ reset now synchronizes embedded ezBEQ state** — `POST /devices/0/peq/reset` still applies the shared-client identity biquad reset (`bypass: false`, identity coefficients), but `app-esp32/src/http_server.rs` now also clears the active ezBEQ slot metadata, persists the refreshed PEQ shadow to NVS, and pushes a fresh `/ws` `DeviceState` update so the browser no longer shows a stale loaded filter after a compat reset.
  - **`preset/copy` response shape matches desktop again** — ESP32 `POST /devices/0/preset/copy` now returns an empty `200` body (`Content-Length: 0`) instead of an ESP32-only `{"status":"ok"}` JSON payload, restoring wire-level parity with the desktop daemon and the existing compatibility notes.
  - **Browser/UI path is now live-verified end to end** — the board serves the ezBEQ UI at `/`, proxies same-origin `/database.json`, loads 14,750 catalogue rows in the browser, successfully uploads and clears `10 Cloverfield Lane`, and keeps the browser slot cards aligned with the actual MiniDSP state after UI mutations, compat `peq/reset`, and measurement-mode enter/restore.
  - **Desktop-side discovery/CLI retested against the live board** — `cargo +esp run -p minidsp-app -- probe --net` found `minidsp-000debd9 (192.0.2.251) [hw_id: 10, dsp_version: 100, serial: 912345]`; `cargo +esp run -p minidsp-app -- --daemon-url ws://192.0.2.251:5380/devices/0/ws status` returned the expected live status; `cargo +esp run -p minidsp-app -- --all-local-devices --daemon-url ws://192.0.2.251:5380/devices/0/ws status` failed with the documented local-only error.
  - **Remaining live gaps** — no-device-mode behavior still needs a physical unplug/replug bench, and HT-only bass-management / routing-polarity paths still need hardware that actually exposes those blocks.
- 2026-04-12 ESP32 HTTP parity pass (live 2x4 HD retest complete; HT-only bench items still pending):
  - **Missing route parity is closed in firmware** — `app-esp32/src/http_server.rs` now serves `GET /openapi.json`, `GET /devices/0/bm`, `POST /devices/0/bm/channel/{ch}`, `POST /devices/0/bm/mixer/{row}/{col}`, `POST /devices/0/peq/reset`, and `GET /devices/0/peq/default-frequencies`, matching the desktop daemon's route surface instead of falling through to the generic 404/UI path.
  - **`/config` parity widened to the desktop shape** — `GET /devices/0/config` now emits desktop-compatible `master_status`/inputs/outputs JSON including routing polarity, output invert, crossover groups, FIR `active_taps`, and compressor fields. `POST /devices/0/config` now accepts routing polarity writes plus crossover/FIR/compressor updates, and the request-body cap increased from `8192` bytes to `128 KiB` so FIR coefficient uploads fit.
  - **Wildcard source/volume routing now matches desktop behavior** — ESP32 switched `/devices/0/source/*` and `/devices/0/volume/*` to wildcard parsing. That closes the old exact-route hole where PascalCase source names and device-specific sources such as `bluetooth`, `rca`, `xlr`, or `lan` would miss the router, and it makes unknown source names / unknown volume directions return explicit `400` errors instead of default HTTPD fallthrough.
  - **DSP100 shadow parity follow-up** — `app-esp32/src/device.rs` now mirrors the desktop client's preset-scoped DSP100 shadow rules for non-authoritative scalar/selector fields. The ESP32 device layer now caches gain/delay/compressor/routing scalars, selector-backed mute/bypass/routing state, and successful TCP-bridge raw writes, so `/devices/0/config` reflects both HTTP mutations and desktop `minidsp --tcp` bridge traffic instead of drifting after write/readback.
  - **Verification status** — `esp cargo build` succeeds after the parity pass, and unit tests cover the source/volume/BM path parsing helpers plus the new raw-selector/shadow sanitization helpers. Live re-verification on the attached `firmware-board` ESP32 + physical 2x4 HD serial `912345` covered `GET /openapi.json`, `GET /devices/0/bm` (`501` expected on 2x4 HD), `GET /devices/0/peq/default-frequencies`, `GET /devices/0/tokens`, `POST /devices/0/preset/copy`, `POST /devices/0/measurement-mode`, `POST /devices/0/measurement-mode/restore`, source/volume error semantics, exact `/devices/0/config` mutation/restore for input gain plus output gain/mute/delay/invert/crossover/FIR/compressor, and desktop `minidsp --tcp` output gain/mute writes surfacing immediately in ESP32 `/config`. The follow-up live pass also covered browser catalogue/load/clear, compat `peq/reset` slot sync, `probe --net`, and CLI `status` via `/devices/0/ws`; remaining live bench work is now limited to no-device mode plus HT-only bass-management and routing-polarity paths on hardware that actually exposes those blocks.
- 2026-04-12 ESP32 RGB LED production integration pass (live spot-checked on dev board `led-test-board`):
  - **Production LED controller landed** — `app-esp32/src/led.rs` now owns the board LED through a dedicated controller thread in normal task context. The production path preserves the tuned `GPIO48` / `GRB` / amber-floor behavior from the demo driver instead of replacing it with a GPIO blink fallback.
  - **Real firmware state plumbing** — `app-esp32/src/main.rs` now drives boot / provisioning / WiFi-connected / USB-present state changes, `app-esp32/src/wifi.rs` drives reconnect and provisioning-fallback transitions, and `app-esp32/src/updater.rs` only asserts `BackgroundUpdate` during real download/write work. `BackgroundUpdate` only replaces `MiniDspReady`, matching `docs/ref-rgb-led.md`.
  - **Fatal state latching fix** — code review during the integration pass found that a later WiFi reconnect could have overwritten `Fatal` red with a non-fatal mode. Fixed by latching `Fatal` in the LED controller so later mode updates cannot clear it until reboot.
  - **Demo path remains available** — `app-esp32/src/bin/led-demo.rs` still points at the amber-only breathe loop for tuning, but the demo-only loops now live in `app-esp32/src/led_demo.rs` so `app-esp32/src/led.rs` stays focused on the production controller and rendering logic. The raw color verification loop and full state demo remain available for future board bring-up.
  - **Verification status** — live visual checks after flashing the integrated firmware confirmed the production blue and amber states on the connected board. Build verification: `esp cargo build` succeeds for both `minidsp-esp32` and `led-demo`.
- 2026-04-11 late-night ESP32 hardening pass (live re-verified on the sim-board-turned-firmware-host firmware-board + real 2x4 HD serial 912345):
  - **usb_hid stale-frame skip** — `bus.command_with_timeout` now mirrors the desktop queue's skip-stale loop and drops decoded frames whose opcode doesn't match the request via `commands::matches_response`. Root cause: the 2x4 HD emits an `AudioSrcChanged` flash notification `[0x05, 0xFF, 0xA9, new_source]` alongside the real `0x34` ack after `set_source`, and also emits stale `0xFFA9` flash frames that had been leaking into the NEXT command's response parsing. `POST /devices/0/source/{analog|toslink|usb}` was intermittently surfacing `protocol: set source: unexpected response FlashData([255, 169, 1])` while the hardware had already changed state. After the skip loop landed, a full `analog → toslink → usb → analog → toslink → usb` sweep via HTTP completed cleanly on the real 2x4 HD with each response body reporting the correct resulting source. Matches the behavior exercised by `minidsp::queue::tests::set_source_skips_stale_flash_response_and_accepts_ack` on the desktop side.
  - **HTTP preset now uses the verified path** — `DeviceState::set_preset` routes through `set_config_with_verify(preset, PRESET_CHANGE_NORMAL)` instead of the raw `set_config(preset, 0)` write, so `/devices/0/preset/{n}`, `POST /devices/0 {"preset": n}`, and the CLI's TCP-bridge preset path all inherit the same dropped-`RESP_PRESET_COMPLETE` fallback that the TCP bridge already used. This also corrects the preset change flag from `PRESET_CHANGE_WITHOUT_RESET` (`0x00`) to `PRESET_CHANGE_NORMAL` (`0x02`), matching Device Console's own `userPreset` flow. Live cycled presets `1 → 2 → 3 → 0` via HTTP on the real 2x4 HD with no timeouts or drops.
  - **Discovery serial truncation** — ESP32 `discovery_packet_for_state` was publishing the raw `dev.serial() as u16` (`912345 & 0xFFFF = 0xebd9`) in the Wi-DG beacon's `sn_high`/`sn_low` slot while the hostname and desktop daemon both use `serial_to_u16(912345) = (912345 - 900000) & 0xFFFF = 12345 = 0x3039`. Fixed to call `minidsp_protocol::discovery::serial_to_u16(dev.serial())`. Live UDP capture now shows `serial_field=0x3039 (12345)` alongside `hostname=minidsp-000debd9`, matching the official Wi-DG encoding and the desktop daemon's advertised value.
  - **`/debug/dsp` float/word auto-fallback** — when the caller doesn't specify `format=`, the handler now tries float decoding first and transparently falls back to the lenient typed-word parser if the strict float parser rejects the response as `Unknown`. This lets the endpoint inspect any DSP address (EEPROM pages, unused slots, etc.) without needing the caller to know the dialect or the parser's float-plausibility heuristic. `?format=floats|words` is still honored strictly. Live verified on the real 2x4 HD at `0xFFA1`, `0xFFD8`, `0x20 count=4`.
  - **Updater user-triggered refresh cooldown** — `trigger_background_refresh` now throttles user-triggered retries to at most once per 5 minutes via a `LAST_TRIGGER_SECS` atomic seeded from `esp_timer_get_time`. Prevents missing UI assets (e.g. the GitHub Pages `manifest.json` 404 while the Pages bundle isn't published yet) from burning a TLS handshake on every `GET /` hit. Cooldown seed is updated only after the refresh thread spawn succeeds, so spawn failures don't accidentally block the next attempt. The scheduled 24h periodic check bypasses the cooldown.
  - **Updater NVS-version clear is now fatal** — if `write_nvs_version(nvs, "")` fails before the LittleFS data wipe, the updater returns `UpdateResult::Error("clear NVS version: ...")` and leaves the old state intact instead of proceeding into `delete_data_files()`. Previous behavior risked a recovery hole where a failed clear plus a later download failure would leave the old version string pointing at an empty data partition and the next boot would believe it was current.
  - **Catalogue parser panic hardening** — `app-esp32/src/ezbeq.rs::parse_biquad_record` and `read_entry_biquads_by_digest` no longer rely on `.try_into().unwrap()` for fixed-size slice reads; they now validate record length up front and build `[u8; 4]` arrays element-wise so a truncated or malformed `catalogue.bin` surfaces as a recoverable update error instead of panicking the firmware.
  - **HTTP handler input validation + body reads** — `/devices/0` (POST), `/devices/0/display`, `/devices/0/dre`, `/debug/level`, `/debug/updater`, `/api/wifi/connect` now use the shared `read_request_body` helper with explicit `max_len`, return `413` on oversize bodies and `400` on invalid JSON, bound `preset`/`brightness`/`idle_time` to the u8 range with explicit `400` responses, and propagate `set_display_*` / `set_dre` errors as `500` instead of silently dropping them into `let _ =`. Live verified: `POST /devices/0` with bad JSON → `400`, oversize body → `413`.
  - **Desktop + shared-crate panic paths closed** — `daemon/src/http/routes.rs::play_device` now returns a 500 with a serialize error instead of `serde_json::to_value(response).expect("play response serializes")`, and `minidsp/src/transport/hid.rs::HidTransport::open` now maps `CString::new(path)` failures into `TransportError::Hid` instead of `.unwrap()` on a NUL-containing HID path.
  - **Comprehensive retest outcome** — all previously-passing surfaces still pass on the sim-board-as-host + real 2x4 HD 912345: `GET/POST /devices/0`, `/devices/0/config` round-trip, `/devices/0/tokens`, `/devices/0/preset/{0..3}`, `/devices/0/source/{analog|toslink|usb}`, `/devices/0/volume/{up|down|toggle_mute}`, `/devices/0/display` + `/devices/0/dre` (correct 404 `not supported` on 2x4 HD), `/devices/0/preset/copy`, `/devices/0/measurement-mode` + `/measurement-mode/restore`, `POST /devices/0/play` (full ~9.5 s measurement sweep at 48 kHz, source auto-switched to USB and restored), `POST /sync`, ezBEQ surfaces (`/api/1/version`, `PATCH /api/3/devices/master`, `PUT /api/1/devices/master/config/{1..4}/active`, `PUT /api/1/devices/master/commands`, `DELETE /api/1/devices/master/filter/{n}`), WebSocket compat stream at `/devices/0?poll=true&levels=true`, WebSocket raw bridge at `/devices/0/ws` with a framed `0x31` request → framed response, ezBEQ `/ws` with `Catalogue` message on connect, TCP bridge port 5333 (both via direct socket and via the desktop `minidsp --tcp` CLI round-trip for probe/status/source/preset/input PEQ/output gain), UDP discovery beacon, mDNS `_minidsp._tcp` advertisement (`minidsp-912345-http`), and all `/debug/*` endpoints (`info`, `log`, `level`, `updater`, `updater/check`, `dsp`).
  - **Pre-existing non-regressions still present**: occasional `output meter read failed: protocol: read_floats: Unknown(...)` on the 2x4 HD is handled gracefully (clamps to `-140 dB`, empty levels array) and is the same DSP100 low-address readback quirk documented above; `PUT /api/1/devices/master/config/0/active` returns `405` because ezBEQ slots are 1-indexed.
  - **Second-pass code review via codex-dobby** (task `9d429e6a...`) was the driver for the usb_hid skip-stale and NVS-clear changes above.
- 2026-04-11 control-surface additions for the HT/HTx launch story plus a corrected EEPROM constant (live verified on the physical 2x4 HD):
  - **0xFFA9 mislabel fixed**: `protocol/src/eeprom.rs` previously declared `SELECTABLE_AUDIO_SOURCES_ADDR = (0xFF, 0xA9)` with a 32-byte size. Live dump on the 2x4 HD plus the deobfuscated Device Console `dspMessage.js` (`AudioSrcChangedMsg`) confirmed `0xFFA9` is a 1-byte unsolicited notification address — not a source list. Constant renamed to `AUDIO_SRC_CHANGED_NOTIFICATION_ADDR`. The real selectable-source descriptor lives at `0xFFEA` (`AudioSrcSelectMsg`, max 32 source IDs, 1 byte each) and now has its own `AUDIO_SRC_SELECT_ADDR` / `AUDIO_SRC_SELECT_MAX_SOURCES` constants. Both addresses return uninitialized flash on the 2x4 HD because that family has fixed sources; the descriptor only matters on HT-class devices and the parser is intentionally deferred until an HT is on the bench.
  - **verifyProgram post-save check** added to mirror Device Console's `userPreset.js verifyProgram()` flow: `protocol/src/eeprom.rs` exposes `is_verify_program_ok(byte)` (status `<= 0x0D`); `minidsp/src/client.rs` adds `read_verify_program_status()` and `copy_preset_verified()`; the desktop `POST /devices/{id}/preset/copy` route now bails with a typed error before bumping tokens if the device reports a corrupt save; `app-esp32/src/device.rs::copy_preset` does the same on the firmware side. Live verified: `POST /devices/0/preset/copy` returns `200 OK` and a direct `dump-flash 0xFFA3 1` reads back `0x00` (well under the failure threshold).
  - **Bass management client API** wired for the HT 113/115 firmware variants. `minidsp/src/client.rs` now exposes `supports_bass_management`, `read_bass_management_state`, `read/set_bm_channel`, `set_bm_channel_gain`, `set_bm_channel_mute`, `read/set_bm_mixer_cell` against the existing `BassManagement` / `BmMatrixMixer` address tables (incl. the LFE virtual row at index 8). HTTP routes: `GET /devices/{id}/bm`, `POST /devices/{id}/bm/channel/{ch}`, `POST /devices/{id}/bm/mixer/{row}/{col}` — all return `501 Not Implemented` on non-HT families. CLI: new `minidsp bm status | channel <ch> [--gain DB] [--mute on|off] | mixer <row> <col> [--enable on|off] [--gain DB] [--invert on|off]`. Smoke tested on the 2x4 HD: refusal path returns `device does not support bass management (HT 113/115 firmware only)`. New protocol tests pin the 9×8 base addresses (status @ 56, gain @ 128, polarity @ 200) and confirm only DSP_113 / DSP_115 expose the block. Underlying `BassManagement` / `BmMatrixMixer` address tables were already validated against `app/Audio Firmware/113/DSP_default_parameters.xml` lines 159..271 in the previous RE session.
  - **Per-cell routing polarity exposure** completes the routing contract gap on families whose `RoutingMatrix.polarity_base` is populated (DSP_110 Flex Eight, DSP_113 / DSP_115 Flex HT/HTx, legacy DDRC-88BM). `RoutingConfig` and `RoutingConfigJson` gained an `Option<bool> polarity_inverted` field; `MiniDspClient::set_routing_polarity` returns `Unsupported` on families without `polarity_base`; the bulk `/devices/{id}/config` write path applies polarity when set; `minidsp input <ch> routing <out> <gain> --invert on|off` exposes it on the CLI. Live verified: `--invert on` against the 2x4 HD (DSP_100, no polarity_base) correctly errors with `unsupported operation for this device`; plain routing without `--invert` still succeeds.
  - Full test suite stays green after all four changes (`cargo test -p minidsp-protocol -p minidsp -p minidsp-daemon`: ~250 tests, 0 failed). New tests: `is_verify_program_ok_boundary`, `bm_matrix_addresses_are_contiguous_and_lfe_row_marked`, `only_ht_families_expose_bass_management`.
  - Open follow-ups (deliberately deferred): typed parser for the `0xFFEA` selectable-source descriptor (needs an HT-class device on the bench to confirm byte layout); advanced Dirac control surface (mux_bypass / dirac_delay / mixer_fir / fir-ctrl / iir-ctrl) — still needs an RE session for the address table.
- TCP bridge write-response framing is fixed and verified on the physical 2x4 HD (2026-04-10):
  - the official MiniDSP Device Console discovered the daemon via UDP broadcast and crashed immediately on every write command with `Error: Unexpected DSP command code 1`
  - root cause: `protocol/src/packet.rs::decode_hid_response` was returning all 63 trailing bytes of the HID buffer for the `0x01`-prefix unframed-echo case instead of trimming to the actual echo length
  - fix: `minidsp/src/queue.rs::execute_queued_command` now trims the response to `cmd.payload.len()` when the request opcode is a write command and the oversized response first byte matches the request first byte; same trim applied in `app-esp32/src/device.rs::raw_command`
  - Python TCP client verification on the physical 2x4 HD: `WriteDSP` response shrank from 64 bytes to 9 bytes, `SetVolume` from 64 to 3 bytes, `SetMute` from 64 to 3 bytes; `ReadHardwareId` and `ReadFloats` responses unchanged at 12 bytes each
- 2026-04-10 first-attach experiment with the official Device Console against the physical 2x4 HD (see `app/minidsp-snapshots/20260410-firstattach-experiment/`) confirmed:
  - no stronger authoritative readback path has been found so far for DSP100 routing / compressor / FIR state; the official app when starting with no local cache offers only "Initialize to default Settings" or "Disconnect" — the same limitation as our shadow fallback model
  - "Initialize to default Settings" is app-driven: the app reads defaults from its bundled XML and writes them via the `0x06` flash-block pipeline (2204 `0x06` writes in the captured session); there is no device-side reset opcode
  - extracted desktop sources and captured session logs are consistent with the same controller-side model: token-matched `setting*.xml` state keyed by modification token (`timestamp`) plus explicit modification-token save-to-device and save-to-file after preset-content writes
  - preset names are client-side XML only (`<customname>` in the app's local `setting*.xml`); zero device writes occur on a rename
  - compressor bypass polarity confirmed: `2 = enabled`, `3 = bypassed/off` (distinct from routing polarity which uses `1/2`); our code at `minidsp/src/client.rs:1109-1110` already has the correct polarity
  - compressor param table for all four outputs matches our `DSP100_OUTPUTS` table in `protocol/src/device/dsp.rs`, including knee=`0x80002b`; the 2026-04-10 edit capture directly exercised threshold=`0x800028`, ratio=`0x80002a`, attack=`0x80002c`, release=`0x80002d`, status enable=`0x800016`, and the official XML/cache entries cover knee
  - our UDP discovery packets correctly advertise the daemon to the official Device Console; it treats the daemon as a legitimate network-attached MiniDSP
  - experiment artifacts archived at `app/minidsp-snapshots/20260410-firstattach-experiment/` (HID capture `device-console-firstattach.jsonl`, Device Console cache states before and after)
- 2026-04-10 deep RE session (codex-dobby multi-angle): full Device Console JS source + APK + Wi-DG + all captures analyzed:
  - **Compressor encoding confirmed end-to-end**: ratio, attack, release are raw float32-LE values via opcode `0x13` ctrl=`0x80`; no special conversion formula. Example live writes from capture: ratio `9.9` → `66 66 1e 41`, attack `30.0 ms` → `00 00 f0 41`, release `220.0 ms` → `00 00 5c 43`. These are direct UI values, not scaled integers.
  - **"no change in value" explained**: Device Console's `userPreset.setItemVal()` compares against its in-memory `singleValues` cache; if cached value matches, hardware write is skipped. Not a hardware limitation. The prior test was likely trying to write default values back to a device already at defaults.
  - **Compressor investigation CLOSED as test-methodology issue**: the remaining likely cause is that default threshold `-30 dBFS` is too quiet to trigger audible compression on the test signal. Audible retest on loud material is the next step, not further protocol RE.
  - **FIR tap count model confirmed**: Device Console does NOT read hardware address `0x0054`. Configured taps come from local XML `_Taps`. Available capacity queried via opcode `0x39` (`GetNumFirTaps`): wire format `[len=0x03, cmd=0x39, channel, cksum]`; response tap count at bytes [2..3] uint16BE.
  - **0x39 = GetNumFirTaps confirmed**: called during FIR write operations (not startup). Identical to what NOTES-cli-verification called "FirLoadStart" — now correctly identified.
  - **0xAB = PresetChangeCompleted confirmed**: 2-byte async notification `[0x02, 0xAB]` sent after `0x25` with non-zero flag. Trailing HID bytes are stale buffer, not payload. `0x25` with flag=0 gets synchronous `result-ok` instead.
  - **Routing defaults confirmed from "Initialize to defaults" 0x06 capture**: diagonal pattern only — `in0→out0` (addr 6=2) and `in1→out1` (addr 11=2) active; cross-routes (addr 7,8,9,10,12,13=1) inactive. All gain slots = float32 0.0.
  - **Wi-DG confirmed as transparent 64-byte pass-through**: no DSP protocol logic, no extra framing. TCP 5333 = raw 64-byte HID pipe. UDP beacon = 72 bytes every 5 s, field-by-field breakdown in `docs/ref-widg-reimplementation.md`.
  - **Preset-bypass block format confirmed**: `0x06 0x02` flash block contains 3-byte records: `[type+flags_byte, addrHB, addrLB]`; type `0x01`=PEQ, `0x04`=BPF/crossover/custom, `0x80` bit set when bypass value is nonzero.
  - **Full opcode table compiled**: see `docs/NOTES-device-console-hid-logging.md` for the complete table including `0x03`, `0x11`, `0x17`, `0x24`, `0x39`, `0x3C`, `0x3D`, `0x3E`, `0x45`, `0x46`, `0x49`, `0x50`, `0x52`, `0xAA`, `0xAB`.
- Desktop `/devices/{id}/config` is fixed and verified on the physical 2x4 HD.
- Desktop `GET /devices/{id}` was re-verified on 2026-04-10 on the physical 2x4 HD:
  - malformed DSP100 meter reads are now treated as best-effort and clamp to `-140 dB` silence instead of stalling the compatibility status surface
  - live daemon `GET /devices/0` returned `200` with the expected master state and stable `-140.0` input/output levels on the attached 2x4 HD
- ESP32 `/devices/{id}/config` parity is verified on the physical 2x4 HD through the currently-implemented 2x4 HD surface: PEQ/gain/mute/routing/delay/polarity base parity landed earlier, and the 2026-04-12 firmware pass re-verified the extended crossover/FIR/compressor shape and write paths on the attached board. The only remaining hardware-specific `/config` bench item is routing-polarity on a family whose `RoutingMatrix.polarity_base` is actually populated.
- Fresh-daemon desktop `/devices/{id}/config` was tightened again on 2026-04-10:
  - live HTTP verification on the physical 2x4 HD confirmed output-0 gain/delay/compressor ratio/attack/release plus input-0 gain and routing edits through `/devices/0/config`
  - the scalar-shadow path still survives a full daemon restart for those non-authoritative DSP100 fields on preset `0`
  - fresh low-address DSP100 reads can still return bogus raw words like `0x4A039901` at compressor/FIR slots, so `/config` now clamps those fields to verified shadow or conservative `0` instead of surfacing garbage values
  - when local shadow is missing for a preset, the daemon can now import the official Device Console `setting*.xml` snapshot for that preset if its XML timestamp matches the live hardware modification token and its `dspversion` matches the connected DSP family, which lets fresh daemon attaches recover sane routing/FIR/compressor state from the official app's own saved preset data instead of guessing
- Desktop `/devices/{id}/play` is now implemented with the shared `PlaybackPlan` / `PlaybackGenerator` path used by ESP32.
- Desktop `/play` now uses the shared REW/MSO-style measurement sweep defaults (`5-400 Hz` hybrid sweep, `2 s` low-level dither pre-silence, `2 s` post-silence, `16-bit`) instead of the retired subwoofer-only baseline.
- Desktop playback backend is smoke-tested on local speakers: the default sweep plays audibly through the CPAL path and reports a real `startedAtUs` timestamp.
- Desktop queue reconnect now re-acquires transports on disconnect and replays idempotent pending PEQ batch writes after reconnect.
- Measurement-mode saved PEQ state now persists in daemon state so restore survives client rebuild/reconnect.
- ESP32 WiFi monitor now records disconnect reason codes, distinguishes credential failures from transient AP loss, and exposes `lastWifiDisconnectReason` in `/debug/info`.
- ESP32 firmware updated 2026-04-10 to match desktop fixes: HID retry-on-undecodable, `has_unreliable_selector_readback` PEQ readback, TCP bridge shadow mirroring, slow command timeouts (35s for preset copy/switch), PSRAM enabled for large allocations.
- ESP32 all non-playback features verified on real 2x4 HD hardware: status, volume/mute/source/preset, config read/write (PEQ/gains/routing/delay/polarity/mute), preset copy, TCP bridge, UDP discovery, WebSocket, ezBEQ API, debug endpoints, sync.
- ESP32 `/play` now pre-generates entire sweep into PSRAM before starting USB audio stream (ESP32 float math too slow for real-time 48 kHz generation). All bytes delivered correctly (`tx_need_data=0` with 48 KB ring buffer), feedback endpoint reads `48.0009 Hz`, and the periodic audio blip regression that was tracked here earlier is now fixed on the physical 2x4 HD (2026-04-10):
  - `UAC2_TASK_CORE` moved from core `0` to core `1` in `app-esp32/src/usb_audio.rs`, isolating the UAC2 driver task and its TX transfer callbacks from WiFi, mDNS, UDP discovery, and other core-`0` activity
  - `run_playback_thread` now generates the sweep into PSRAM in 10 ms slices and calls `std::thread::sleep(10 ms)` every 50 chunks so the IDLE task can reset the task watchdog during pre-generation; the previous `yield_now()` only yielded to equal/higher-priority tasks and starved IDLE, and sub-tick sleeps busy-wait instead of crossing a FreeRTOS tick boundary
  - SRAM copy scratch buffers for the streaming loop and the ring-buffer pre-fill are now sized for 24-bit stereo 10 ms chunks (`2880 B`) instead of the old 16-bit-only `1920 B`
  - WiFi power save disable/restore around playback is now wrapped in a `WifiPsGuard` RAII struct, so every error path in both the normal playback thread and the inline sine diagnostic restores `WIFI_PS_MIN_MODEM` on drop
  - source switching before/after playback is now shared between the normal sweep path and the `?test=sine` diagnostic via the `switch_to_usb_source` helper, so both paths pick up the same error handling and the same source restore behavior on spawn failure
  - a new `POST /devices/0/play?test=sine` diagnostic endpoint bypasses `PlaybackGenerator` and PSRAM entirely, generating a `50 Hz` inline sine into a stack buffer and writing it in a tight loop that matches the standalone `esp-uac2-host` test harness; this was the tool that isolated the blip source to the system environment rather than the data pipeline
  - live hardware test on 2026-04-10 on the physical 2x4 HD: `tx_need_data=0`, `write_p50 ~ 8.75 ms`, `write_p99 ~ 11.7 ms`, zero task watchdog triggers during pre-gen, and the audible sweep was reported as "actually sounded pretty good" on the real sub (first-pass qualitative check only; full audible validation on a known-good measurement rig is still pending)
- Shared `/play` defaults were then updated again to match REW/MSO-style measurement conventions:
  - default request is now a `5-400 Hz`, `5.46 s`, `16-bit` sweep with `2 s` low-level dither pre-silence, `2 s` post-silence, and `20 ms` raised-cosine fade-in/out
  - when the sweep starts below `10 Hz`, the generator now follows REW's hybrid behavior: linear chirp from `5 Hz` to `10 Hz`, then logarithmic from `10 Hz` to `400 Hz`
  - shared tone bursts now use a short deterministic `5 ms` raised-cosine fade-out at the end instead of a hard gate, so the marker ends cleanly without changing its onset/offset accounting
  - the shared `/play` contract is now a single canonical public request shape: `sweepDurationSec`, `sweepStartHz`, `sweepEndHz`, `sweepFadeSec`, `markerBefore`, `markerAfter`, `markerGapMs`, `markerFrequencyHz`, `markerCycles`, `decaySec`, `sampleRate`, and `bitDepth`
  - `{}` now means the canonical default measurement bundle: `2 s` low-level preamble, one `80 Hz` pre-marker, `50 ms` gap, the sweep, another `50 ms` gap, one `80 Hz` post-marker, and `2 s` decay. Sweep-only playback is explicit via `{"markerBefore":false,"markerAfter":false}`.
  - `markerCycles = 0` now has the intuitive meaning callers expect: no marker sections are emitted and no marker gaps are reserved, even if `markerBefore` / `markerAfter` stay `true`
  - legacy nested request fields (`sweep`, `toneBurst`, `tailToneBurst`, `preambleSec`, `gapSec`) were removed before release and are now rejected instead of being silently supported
  - desktop daemon follow-up on 2026-04-14 caught one false negative that was just a stale local binary. After rebuilding with `cargo +stable build -p minidsp-app`, live HTTP verification on the physical 2x4 HD returned the expected canonical default response (`toneBurstOffsetSamples=96000`, `sweepOffsetSamples=99000`, `tailToneBurstOffsetSamples=363458`, `totalSamples=460058`, `sampleRate=48000`, `sourceChanged=false`) and `/sync.serverTimeUs` still bracketed `/play.startedAtUs` on the same clock basis
  - the April 14 desktop acoustic pass then split and verified each component on the real sub path: the sweep-only request (`{"markerBefore":false,"markerAfter":false}`) was confirmed clean after fixing the sweep-end taper, the marker-only diagnostic (`?test=marker`) was confirmed clean on its own, and the combined default marker+sweep playback was confirmed audibly good across repeated runs
  - the finalized OSO-facing desktop validation pass on 2026-04-14 then exercised the whole contract against the live daemon + physical 2x4 HD: `/openapi.json` advertises the flat schema with `additionalProperties=false`; bad requests (`tailToneBurst`, unknown fields, `markerGapMs > 2000`, too-short nonzero markers) all return `400`; repeated identical short marker-enabled requests keep deterministic offsets (`toneBurstOffsetSamples=96000`, `sweepOffsetSamples=99000`, `tailToneBurstOffsetSamples=125380`, `totalSamples=135580`) while `/play.startedAtUs` stays bracketed by adjacent `/sync.serverTimeUs` samples on the same clock basis; `markerCycles=0` now collapses to the same sweep-only timing as explicit `{"markerBefore":false,"markerAfter":false}`; `?test=marker` returns the expected zero-based marker-only offsets; and a forced `Toslink -> /play -> Toslink` run proved source auto-switch to `Usb` plus restore on completion (`sourceChanged=true`, `previousSource=\"Toslink\"`)
  - the older measurement-bundle default (`5-500 Hz` with BRAP bursts) remains retired as historical context only
- ESP32 HID and UAC2 reconnect now recover on the physical 2x4 HD across the three important attach cases:
  - unplug/replug while the ESP32 stays up
  - MiniDSP appears later while the ESP32 is already running
  - ESP32 reboots with the MiniDSP off, then the MiniDSP appears later
- The official MiniDSP app established a known-good audible USB-audio baseline on the physical 2x4 HD:
  - preset `2` in the app UI (zero-based `preset: 1` via our API)
  - source `USB`
  - master volume `0 dB`, master mute off
  - visible routing: `Input 1 -> Output 1` at `0 dB`, `Input 2 -> Output 2` at `0 dB`
  - subwoofer test output should use physical output `2` in that preset
- Opening the official app changed the live device into a working-audio state where direct computer playback became the correct comparison baseline.
  - The strongest current evidence points to preset/routing state, not an ESP32 `/play` transport fault:
    - direct Mac USB playback was also silent before the official-app reset
    - after the reset, Mac playback became audible
    - live user testing confirmed the narrower USB-only rule: a physical output stays silent for USB playback until at least one of `Input 1` or `Input 2` is routed to that output
  - It is still not fully proven whether the app only restored preset/routing state or also touched some additional hidden runtime state.
- The modern config compatibility investigation found and fixed more than one bug:
  - DSP meter values on modern devices were already dB-like; our extra `log10()` conversion was wrong
  - ESP32 `POST /devices/{id}/config` body reads could truncate larger JSON payloads
  - modern routing write support was missing from both the desktop daemon and the ESP32 HTTP config path
  - the desktop CLI `input <n> routing <out> <gain_db>` path was writing the wrong gain encoding on float32 devices
  - shared typed `ReadFloats` parsing for modern raw-word slots was missing the 5-byte typed-response header variant, which could skew status/sample-count reads on SHARC/XMOS devices
- Additional direct-USB reverse engineering on the physical 2x4 HD resolved a separate transport bug in our code:
  - real HID write acknowledgements can arrive as report-ID-prefixed unframed echoes beginning with `0x01`, not only as length-framed responses
  - the old decoder treated that leading `0x01` as a one-byte empty frame, which made successful direct USB writes look like `device error []`
  - after fixing `protocol/src/packet.rs`, direct USB writes now ACK cleanly and immediate read-back confirms the write actually landed
- A follow-up fix in the shared HID decoder resolved the remaining modern route-write failure on the real 2x4 HD:
  - we were still accepting `0x01`-prefixed unframed echoes for read commands, not just write/ack commands
  - that let a stale `ReadFlash` echo get misclassified as the next command response, which is why ESP32 route writes sometimes failed as `unexpected response FlashData([255, 169, 2])`
  - restricting that special-case decoder path to write/ack-like commands fixed the poisoned-response path for both desktop HID and ESP32 USB host
- DSP100 routing write compatibility is now proven on the real 2x4 HD:
  - earlier direct USB checks established the scalar semantics we care about: mixer status addresses `6..13` use enable=`2`, disable=`1`, and route-gain cells `32..39` are writable as float dB values
  - modern routing readback is still boolean-collapsed via `0x14` (any non-zero reads back as `1`), so `status == 2` was never a valid runtime check
  - desktop hardware testing found `0x13` works for route-enable writes and `0x30` does not on the 2x4 HD, so we should not switch opcodes based on the old `0x30` note alone
  - after the shared `0x01` echo-decoder fix, ESP32 `POST /devices/0/config` successfully forced `Input 1 -> Output 3 = enabled`, `Input 2 -> Output 3 = disabled`, and `/play` became audible on physical output `3`
  - current runtime routing readback is still not authoritative enough to treat as solved state introspection, but the live route-enable write path itself is now working
- Live device testing also confirmed and fixed the modern mute polarity on DSP100:
  - `1 = muted`
  - `2 = unmuted`
  - shared desktop + ESP32 code now uses that polarity
- Live direct-USB verification on 2026-04-09 fixed the DSP100 / 2x4 HD delay write path:
  - the shared client now writes delay slots with the device's sample-count integer encoding instead of the old raw big-endian `u32` path
  - on the physical 2x4 HD, writing `0.5 ms` now lands as `48` samples and reads back as `0.5 ms`
- Live direct-USB verification on 2026-04-09 also fixed desktop preset-switch semantics on the physical 2x4 HD:
  - the real device returns an immediate raw `Ack` for `SetConfig`; the old shared queue behavior that waited for a second preset-only completion frame was wrong for this path
  - the shared queue now returns the first valid `SetConfig` response immediately, but high-level control surfaces intentionally wait for master-status confirmation before reporting success
  - re-verification on 2026-04-10 on the same physical 2x4 HD showed the real apply window is about `5.0-5.4 s` across every tested surface:
    - `POST /devices/0/preset/2` in `5.02 s`
    - `minidsp --daemon-url http://127.0.0.1:5382 config 3` in `5.37 s`
    - `minidsp --url tcp://127.0.0.1:5333 config 1` in `5.38 s`
    - compatibility `POST /devices/0 {"preset":0}` in `5.32 s`
- Live direct-USB verification on 2026-04-09 also fixed long-running desktop flash/state commands on the physical 2x4 HD:
  - the shared queue now gives `CopyPreset (0x27)` and `Reset (0x03)` a longer default deadline instead of the generic `10 s` command timeout
  - `POST /devices/0/preset/copy` returned `200` in `23.02 s` on 2026-04-10 with an empty body (`Content-Length: 0`), matching the official Device Console session that took about `22.87 s` from `0x27` send to `Received OK`
  - raw reset through the daemon path now returns framed `aa` in about `6.7 s`; on 2026-04-10 the live daemon log showed `HID OUT payload: [03]`, a timeout fallback into master-status polling, an undecodable `ff f1 ...` packet during the reset window, and then normal `0xFFD8` recovery instead of a decodable device-side `0xAA`
  - the preset-copy route now persists refreshed daemon shadow/token state after the successful flash save instead of leaving the cache stale
- Live direct-USB verification on 2026-04-09 also fixed desktop source-switch semantics on the physical 2x4 HD:
  - the device emits a stale `ReadFlash`-shaped `0xFFA9` notification after source changes, and also reuses the short-form `ReadFloats` meter reply shape
  - the shared queue now skips stale mismatched responses instead of treating them as the current command reply, and request-specific `ReadFlash` / `ReadFloats` parsing now rejects wrong-address payloads without rejecting the real short-form meter replies
  - high-level `set_source()` now verifies completion by polling master-status readback until the requested source is active
  - live desktop checks on the physical 2x4 HD now show both `POST /devices/0` with `{"source":"Analog"}` / `{"source":"Usb"}` and the dedicated `POST /devices/0/source/{source}` route completing cleanly with matching follow-up `GET /devices/0` responses
- Modern-device readback issue resolved (2026-04-08):
  - DSP100/2x4 HD selector-style status readback via `0x14` is not authoritative: routing enable and compressor/FIR bypass words can collapse to `1` on live hardware even when the last written value was different
  - the shared desktop client now keeps selector shadow state for successful writes so long-lived `/config` readback can reflect what we actually drove on DSP100
  - live desktop daemon verification on 2026-04-09 proved that same-client `/devices/0/config` route readback now tracks what we drove: `Input 1 -> Output 3` can be flipped without changing `Input 2 -> Output 3`, and `Input 1 -> Output 2 = false` reads back as `false` while `Input 2 -> Output 2` remains `true`
  - for truly authoritative routing state, the API should eventually read from saved preset flash data or another source stronger than runtime DSP parameter readback
- The shared client now validates write responses more strictly:
  - PEQ batch phases, single biquad writes, and bypass writes now check for real `Ack` / completion responses instead of silently accepting parsed device errors
- The current live CLI verification findings are consolidated in `docs/NOTES-cli-verification.md` and should drive the next debugging pass.
- The physical 2x4 HD is now the project’s golden device for shared control-path debugging.
  - imported official-app coverage still defines the intended support matrix
  - live 2x4 HD behavior overrides inferred multi-device assumptions when the two disagree
  - `minidsp-rs` and official-app behavior are reference points, but only live 2x4 HD reads, writes, and audible results decide whether a shared fix is actually correct
- The strongest remaining likely compatibility drifts from known-working minidsp-rs behavior are:
  - compressor scalar writes beyond threshold / gain on DSP100 / 2x4 HD
  - direct desktop verification on 2026-04-09 tightened this further:
    - upstream minidsp-rs and the extracted official-app compressor path both still point to plain `0x13` scalar writes, so this drift is not explained by a `0x30` vs `0x13` command mismatch
    - raw distinctive writes to `COMP_5_0_ratio`, `COMP_5_0_atime`, and `COMP_5_0_rtime` on the physical 2x4 HD did not appear anywhere in the exposed `0..34959` DSP/config space, even after `ReloadDspParam`
    - the same pass proved `COMP_5_0_threshold` still writes correctly, so this is not a general `0x13` write failure
    - desktop compressor scalar writes now use read-before / read-after verification so a no-op field fails loudly instead of being treated as success
    - a direct 2026-04-09 control check with upstream `minidsp-rs` on the same physical 2x4 HD also only moved `threshold`; immediate typed-word readback still left `ratio / knee / attack / release = 0`
    - forcing compressor bypass `off` first did not change that result on the tested hardware
    - that means the remaining gap is no longer specific to this repo's desktop control path
- Desktop raw typed-read compatibility was tightened on 2026-04-09:
  - the physical 2x4 HD returns short `ReadFloats` replies in the low selector/status address range that the float parser classifies as `Unknown`, even though the raw payload is valid for typed-word callers
  - the shared queue now lets raw/typed callers receive those same-opcode frames instead of timing out, which fixed live reads like `debug dump-word 22 2` and `debug dump-word 24 2` on the physical 2x4 HD
- Desktop DSP100 polarity handling was corrected again on 2026-04-09:
  - the false failure was in verification, not transport: the physical 2x4 HD polarity slot should be checked as a logical boolean instead of exact raw-word equality
  - the shared desktop client now uses the upstream `minidsp-rs` modern integer `0x13` polarity write path (`0` / `1`) again and verifies success through logical before/after invert readback
  - live desktop `/devices/0/config` toggles for outputs `2` and `3` now return `200`, read back correctly, and log the upstream-style `HID OUT payload: [13, 80, 00, 52|53, ...]`
- Live desktop verification on 2026-04-09 also tightened the expected gain-readback model on the physical 2x4 HD:
  - per-channel gain reads are quantized on this device; a requested `-1.5 dB` currently reads back around `-1.638082 dB`
  - shared gain verification and the repeatable acceptance checks now use tolerance-based comparison instead of exact-float equality for those slots
- A broader desktop HTTP verification pass on 2026-04-09 now live-proves the physical 2x4 HD for:
  - `POST /devices/0` with the compatibility bare `MasterStatus` body (`{"volume":...}`, `{"mute":...}`), not a wrapper object
  - source and preset switching through both the bare route and dedicated convenience routes
  - input `0` and `1` gain/mute
  - output `2` and `3` gain/mute/invert/delay
  - all eight routing edges (`2 inputs x 4 outputs`) toggled and restored through `/devices/0/config`
- Live desktop WebSocket verification on 2026-04-09 now also proves the physical 2x4 HD for:
  - `/devices/0?poll=true` sending an immediate status frame on connect
  - `/devices/0?levels=true&poll=true` continuing to stream state/levels while source writes stay fast and preset writes still return `200`
  - clean live reruns on 2026-04-09 showed `POST /devices/0/source/Usb` / `/source/Analog` still completing in about `11 ms` / `6 ms` with the polling socket attached
  - the official raw Device Console capture shows the same preset-change shape on this hardware: `0x25` acknowledged immediately at `2026-04-09T19:30:52.875Z`, but the first follow-up `0x14` status reply did not arrive until `2026-04-09T19:30:56.185Z`
  - the websocket meter cadence now matches the official app's `500 ms` polling rhythm and no longer injects an extra immediate post-connect tick
  - `/devices/0/ws` ignoring text frames and still returning the correct framed binary reply to a `ReadHardwareId` request
  - manual stdlib WebSocket checks on 2026-04-10 re-verified `/devices/0?poll=true`, `/devices/0?levels=true&poll=true`, and `/devices/0/ws` on the live daemon:
    - `/devices/0?poll=true` delivered an immediate status frame with the current preset/source
    - `/devices/0?levels=true&poll=true` delivered an immediate state/levels frame with `2` input levels and `4` output levels
    - `/devices/0/ws` ignored a text frame and still returned the correct framed `ReadHardwareId` reply
- Live direct-USB verification on 2026-04-09 also fixed the desktop DSP100 / 2x4 HD FIR load path:
  - the shared DSP100 output profile now includes FIR definitions for all four outputs, so output FIR is no longer treated as unsupported on the 2x4 HD
  - the real device returns `FirLoadStart` size bytes as big-endian; on the tested 2x4 HD, `[0x39, 0x08, 0x00]` means `0x0800 = 2048` coefficients, not `8`
  - the real device returns the `FirLoadEnd (0x3B)` `Ack` seconds later, not within the queue's 500 ms polling slice
  - the shared queue now keeps polling until the overall command deadline instead of treating each 500 ms receive timeout as a hard command failure
  - desktop direct-USB replay with upstream `minidsp-rs` on the same 2x4 HD reproduced the delayed bare `[0x01]` `FirLoadEnd` ack; upstream times out there, while this repo now accepts that delayed ack on the shared queue path
  - with those fixes, desktop `POST /devices/0/config` FIR writes now return `200`, raw coefficient readback confirms the new taps landed, and same-client `GET /devices/0/config` reports `active_taps` correctly from shadow fallback
  - a follow-up fresh-client fix corrected FIR/compressor bypass decoding on DSP100: live direct-USB verification on the physical 2x4 HD showed raw selector `1` = bypassed and `2` = enabled for these slots, so fresh-client `/devices/{id}/config` now decodes output FIR/compressor bypass correctly after daemon restart
  - a follow-up persisted-shadow fix now restores fresh-daemon `active_taps` readback on desktop by serial number even though the live DSP100 tap-count slot still reads back as `0`
- Desktop raw bridge shadowing was tightened on 2026-04-09:
  - successful raw WS/TCP bridge writes now mirror their selector/PEQ side effects into the same daemon shadow state used by `/devices/{id}/config`
  - that keeps desktop `/config` readback coherent after raw bridge activity instead of only after HTTP/CLI writes
  - the daemon now persists that selector/PEQ shadow by device serial under `~/.config/minidsp-open/shadow-cache.json`, so fresh desktop daemon restarts restore verified routing and FIR tap-count state for the attached 2x4 HD
- Official-app raw HID capture is now in place on 2026-04-09:
  - canonical saved log: `app/minidsp-console-app-log.jsonl`
  - companion high-level session log: `app/minidsp-snapshots/20260409-151113-device-console-session/device-console-main-session.log`
  - this is a full raw `open / tx / tx-result / rx / close` capture from the patched local Device Console talking to the physical `2x4 HD`
  - the capture confirms official-app use of `0x31`, `0x05`, `0x14`, `0x42`, `0x34`, `0x25`, `0x13`, `0x19`, and `0x30` on this hardware
  - the same capture plus `app/app-extracted/build/src/dspMessage.js` now identify `0x06` / `0x07` as the preset-save flash-block pipeline and `0x40` as the DSP-version query
  - that rules `0x06` / `0x07` out as the missing authoritative DSP100 compressor/runtime readback path on the tested 2x4 HD
  - the companion session log also shows Device Console comparing the active preset's device token against its XML token and then explicitly writing a fresh modification token after audio-proc updates
- Desktop DSP100 selector-write verification was tightened again on 2026-04-09:
  - modern output mute and FIR/compressor bypass writes now use raw before/after selector-word checks on the physical 2x4 HD instead of trusting the write ACK alone
  - those desktop HTTP/shared-client paths now fail loudly if the selector word does not land on the device
- A stronger direct-USB routing-state pass on 2026-04-09 ruled out two likely false leads for fresh-client DSP100 routing recovery:
  - a full readable `0..34959` DSP/config diff around a single route toggle found no authoritative routing mirror outside the collapsed `6..13` status words; the only unrelated changes were output meter values
  - EEPROM modification tokens at `0xFFC8` stayed unchanged across raw live routing and bypass writes on the tested 2x4 HD path, so the firmware does not auto-advance them for those runtime writes
- Desktop CLI compatibility was tightened again on 2026-04-09:
  - `--log` / `MINIDSP_LOG` now match `minidsp-rs` semantics as a log-file path instead of a tracing-filter string
  - TCP and WebSocket transports now emit the same frame-level send/receive logs as HID, so the transport log file is useful across all desktop paths
  - `--usb` / `MINIDSP_USB` now accept upstream-style `vid[:pid]` selectors like `2752:0011`, not just raw HID paths
- A broader desktop CLI acceptance pass on 2026-04-09 now live-verifies the real 2x4 HD for:
  - device listing and status summary endpoints
  - input/output mute and gain
  - input/output PEQ set + bypass
  - routing CLI off/on
  - crossover bypass
  - FIR load via CLI with matching `/config` readback
- Desktop daemon `/config` compressor persistence was live-verified again on 2026-04-09:
  - `POST /devices/0/config` changed output-0 compressor `bypass`, `threshold`, `ratio`, `attack`, `release`, and `gain` on the physical 2x4 HD
  - same-daemon `GET /devices/0/config` reflected those values immediately
  - a fresh daemon restart still returned the same compressor values for that preset, proving the token-gated scalar shadow restore now covers the non-authoritative DSP100 compressor fields
- Desktop daemon preset-mutation token handling was aligned to the official app on 2026-04-09:
  - `/devices/0/config` and measurement-mode preset-content writes now save a fresh active-preset modification token before persisting shadow state
  - live daemon logging on the physical 2x4 HD showed `WriteFlash` traffic to `0xffcc` for active preset `1`, matching the official `0xffc8 + preset*4` token-save rule
  - a no-op compressor `/config` write advanced the active preset token while the visible compressor state stayed stable, which is the intended controller-side freshness marker for later shadow restore
- Fresh-daemon preset shadow recovery was extended again on 2026-04-10:
  - the official Device Console `setting*.xml` `<timestamp>` field matches the per-preset hardware modification token on the physical 2x4 HD
  - the desktop daemon now uses that match to import missing preset shadow from `~/Documents/miniDSP/MiniDSP Device Console/.../setting*.xml` when the official app has a current saved preset and live DSP100 readback is still non-authoritative
  - this does not make low-address `0x14` authoritative; it gives the daemon a stronger controller-side source of truth than stale local cache when the official XML and device token agree
- All 13 exact modern Device Console `HW`/`DSP` pairs are implemented and test-gated.
- All 7 cached legacy `config.xml` families are implemented as source-backed runtime tables and test-gated.
- The repeatable official-app importer/report pipeline is in place:
  - `tools/extract_device_support_matrix.py`
  - `docs/ref-device-coverage.md`
  - `docs/generated/device-support-matrix.json`
  - `protocol/src/device/generated_official.rs`

## Active Work

### Wi-DG parity (compat / discovery / fingerprint)

Parallel workstream to the DSP control-surface items below. Makes
minidsp-open a drop-in replacement for MiniDSP's Wi-DG networking
appliance so the stock MiniDSP Device Console auto-discovers us on any
LAN with zero config (UDP 3999 beacon + TCP 5333 bridge), and so the
desktop daemon becomes an N-device rack-mounted Wi-DG replacement. Does
not fix any DSP control-surface gap — see items below — but should become
a feature-coverage safety net once the remaining Device Console bridge
verification closes, because Device Console ought to be able to drive the
same DSP feature set through our bridge that it can drive through a real
Wi-DG.

Plan + spec:
- `docs/plan-widg-parity.md` — phased implementation plan
- `docs/ref-widg-reimplementation.md` — master behavioral spec, every
  byte of the external Wi-DG contract

**P0 — Device Console auto-discovery (~7 h):**

- [ ] P0-1 — UDP beacon byte-level rewrite to the 72-byte Wi-DG layout and
      5-second interval (`protocol/src/discovery.rs`, `daemon/src/discovery/mod.rs`,
      `app-esp32/src/main.rs`)
- [ ] P0-2 — ESP32 SoftAP SSID template `miniDSP-Wi-DG_XXYYZZ` (last 3 MAC
      bytes uppercase hex) (`app-esp32/src/wifi.rs`)
- [ ] P0-3 — ESP32 hostname `miniDSP-Wi-DG`, LAN IP `192.168.100.1/24`,
      DHCP pool `.100–.249`, 12 h lease, dual-mode AP+STA
      (`app-esp32/src/main.rs`, `app-esp32/src/wifi.rs`)
- [ ] P0-4 — TCP 5333 socket options (`SO_REUSEADDR`, `TCP_NODELAY`,
      `SO_KEEPALIVE`) + multi-client fan-out + write-serialization mutex
      (`daemon/src/tcp/mod.rs`, `app-esp32/src/main.rs`)
- [ ] P0-5 — Desktop daemon defaults UDP beacon + TCP 5333 to ON so zero
      config is needed for Device Console discovery (`daemon/src/lib.rs`,
      `config.example.toml`)

**P1 — Byte-for-byte fingerprint match (~5 h, after P0):**

- [ ] P1-1 — mDNS hostname `miniDSP-Wi-DG.local`, service type
      `_http._tcp:80` with TXT `path=/` (`app-esp32/src/main.rs:start_mdns`;
      desktop via `mdns-sd` crate if not already present)
- [ ] P1-2 — HTTP→HTTPS redirect on port 80, HTTPS on 443 with ECDSA-P256
      self-signed cert (`CN=miniDSP-Wi-DG`), `Server: miniDSP-Wi-DG` header
      (`app-esp32/src/http_server.rs`)
- [ ] P1-3 — Desktop mDNS advertisement (same as P1-1)
- [ ] P1-4 — ESP32 STA DHCP client advertises hostname `miniDSP-Wi-DG` via
      DHCP option 12 (`app-esp32/src/wifi.rs`)
- [ ] P1-5 — Appliance-mode beacon fill `0xFF` for identity bytes when no
      DSP attached (replaces current `hw_id=0, dsp_id=0` fallback in
      `app-esp32/src/main.rs:579-589`)

**P3 — Security improvements (~2 h, independent of P0/P1):**

- [ ] P3-1 — SoftAP default WPA2-PSK with a printed-on-label password;
      leave open-AP as an opt-in flag for strict Wi-DG parity during
      provisioning
- [ ] P3-2 — Optional HTTP BasicAuth gate on the management UI (behind a
      flag; off by default)
- [ ] P3-3 — TLS cert ECDSA-P256, 3650-day validity, clean subject (folds
      into P1-2)
- [ ] P3-4 — Remove `/www/log-minidsp.txt` equivalent from the
      unauthenticated space (authenticated only, or don't serve)

Live verification gates (against a physical Wi-DG on the same LAN): see
`app/widg-re/live-verification-plan.md` — TEST-01 (beacon byte capture),
TEST-02 (TCP framing), TEST-06 (USB HID opcode capture) after P0; TEST-03
and TEST-04 after P1.

---

### DSP control-surface (our own HTTP 5380 / CLI / ezBEQ API)

- [ ] Treat `docs/NOTES-cli-verification.md` as the top-priority investigation checklist until the remaining CLI compatibility drifts are explained or fixed.
- [x] ~~Investigate DSP100 / 2x4 HD compressor parameter drift from minidsp-rs~~ **CLOSED 2026-04-10 (protocol gap)**: Multi-angle RE session (Device Console JS source, APK, Wi-DG, all captures) confirmed:
  - compressor ratio, attack, release write as raw float32-LE via opcode `0x13` ctrl=`0x80` — same as this repo already does
  - "no change in value" session log messages = Device Console in-memory cache comparison, not hardware readback; the prior test was likely writing default values back to a device already at defaults
  - Device Console never reads back compressor params after writing; only polls compressor meter at address `0x0046`
  - **Most likely prior test failure cause**: default threshold `-30 dBFS` is too quiet to trigger audible compression on typical test signals
  - **Remaining item (methodology, not protocol)**: audible retest on loud material with compressor enabled and threshold set to trigger. Open as audio-verification item below.
- [ ] Capture or otherwise verify the physical output duration of ESP32 `/play` on the real 2x4 HD now that software timing and repeated-play stability are characterized.
- [x] Validate the new shared `/play` default signal on the real sub and keep iterating until it sounds like a normal smooth low-frequency sweep:
  - the previous measurement-bundle default (`5-500 Hz`, `24-bit`, `2 s` preamble, `80 Hz` tone burst, `0.2 s` gap, `2 s` decay, hidden guard-band expansion) was retired after it produced repeated `BRAP` bursts on hardware
  - the current shared default is now a REW/MSO-style `5-400 Hz`, `5.46 s`, `16-bit` hybrid sweep with `2 s` low-level dither pre-silence, `2 s` post-silence, and `20 ms` raised-cosine fade-in/out
  - below `10 Hz`, the current default intentionally uses a linear `5-10 Hz` section before switching to logarithmic `10-400 Hz`
  - a 2026-04-10 qualitative listening check on the physical 2x4 HD after the ESP32 core-1 + pre-gen watchdog fixes described that same default as "actually sounded pretty good", which is a positive partial signal but not a full audible validation
  - the ESP32 sweep-only collapse is now audibly signed off on the physical 2x4 HD via `POST /devices/0/play {"markerCycles":0}`; the current driver/app combination that cleared the last random pops includes the committed TX ringbuffer wrap fix in `esp-uac2-host`, a higher-priority UAC2 background task, and shorter `~2 ms` playback writes
  - follow-up audible checks on the same ESP32 path then confirmed marker-only playback via `POST /devices/0/play?test=marker`, the combined default marker+sweep bundle via `POST /devices/0/play {}`, and the longer-gap variant via `POST /devices/0/play {"markerGapMs":1000}`; the user signed those off as sounding clean/perfect on the attached 2x4 HD
- [x] Validate non-USB-source `/play` source auto-switch + restore on real hardware:
  - desktop path is now complete on the physical 2x4 HD: forcing `Toslink`, starting `/play`, and waiting for completion restored `Toslink`, with the response reporting `sourceChanged=true` and `previousSource="Toslink"`
  - direct desktop source-switch recheck on 2026-04-14 also stayed healthy on the same physical 2x4 HD: both the direct USB CLI path and the desktop HTTP daemon cleanly switched `Usb <-> Toslink` and `Usb <-> Analog`, and the device remained probeable afterward once the owning desktop process released the HID handle
  - the remaining wedge was ESP32-specific rather than a shared `0x34` source-opcode problem: the attached 2x4 HD stayed healthy on the direct desktop CLI / daemon path and on the official app, while the ESP32 path could still wedge it during `Toslink -> Usb`
  - the final ESP32 fix was to stop treating the official `AudioSrcChanged` flash notification (`ReadFlash 0xFFA9 <new_source>`) as stale noise and to stop the ESP32-only `set_source` timeout/reopen verification loop from hammering master-status reads after a dropped ack
  - live re-verification on 2026-04-14 is now clean on the attached 2x4 HD: repeated HTTP `Usb -> Toslink -> Usb` cycles stayed healthy, `GET /devices/0` remained probeable after every leg, and UART no longer showed HID reopen churn or device-loss after `set_source(Usb)`
  - live ESP32 `/play` re-verification on the same hardware is also complete: forcing `Toslink`, starting the canonical default `/devices/0/play {}`, and waiting for completion restored `Toslink`, with the response reporting `sourceChanged=true` and `previousSource="Toslink"`; the same source-restore path also stayed correct for both `?test=marker` and `?test=tail-marker`
- [ ] Verify remaining `/play` compatibility edges on hardware where possible:
  - malformed request handling
  - sample-rate switch between plays
  - HID-only-device clear error path
  - live 2026-04-14 busy-guard verification is now complete on the attached ESP32 + 2x4 HD: during an active default `/play`, plain `GET /devices/0` and `GET /api/2/devices` both returned `409 {"error":"device busy during playback"}`, the raw TCP bridge on port `5333` returned `0x00` busy/error for a framed `0x31` request, and the device remained healthy after playback finished
  - post-push bridge parity follow-up on 2026-04-14 is also clean on the same hardware: raw TCP `CMD_SET_SOURCE` (`0x34`) now reuses the same source-recovery path as HTTP, repeated live `Toslink <-> Usb` cycles over port `5333` returned clean `0x01` acks and left the device healthy, and the raw WebSocket bridge at `/devices/0/ws` now does the same on framed binary requests
  - embedded ezBEQ `/ws` playback gating was rechecked live after that follow-up: connecting and subscribing during an active sweep now yields only the normal `Catalogue` envelope during the busy window instead of injecting HTTP-style `{"error":...}` payloads into the ezBEQ stream
  - the same follow-up also tightened the playback pre-generation yield cadence on the ESP32 after an 8 s sweep reproduced a CPU0 `task_wdt` warning; reflashing and rerunning the same 8 s `Toslink -> /play -> Toslink` hardware check completed cleanly with no fresh `task_wdt` output in the UART log while source restore still succeeded
- [x] Prove on hardware that the current shared `0x13` route-enable write path restores audible USB playback on DSP100 / 2x4 HD without relying on the official app
  - confirmed live on 2026-04-09: after the shared HID echo-decoder fix, ESP32 route writes made `/play` audible on physical output `3`
- [x] Make DSP100 / 2x4 HD routing-state read-back survive fresh desktop daemon reconnects via persisted verified shadow keyed by device serial
- [x] Make DSP100 / 2x4 HD FIR `active_taps` read-back survive fresh desktop daemon reconnects via persisted verified shadow keyed by device serial
- [x] ~~Continue DSP100 / 2x4 HD authoritative-state reverse engineering so fresh-client `/devices/{id}/config` can eventually prefer real hardware readback over persisted shadow for routing and FIR tap count~~
  - **CLOSED 2026-04-10:** no stronger authoritative readback path has been found in the first-attach experiment, the extracted desktop sources, the captured session logs, or the current Ghidra pass. When the official app has no local cache for a device, it offers only "Initialize to default Settings" or "Disconnect", and the extracted `userPreset` / `userPresetMgr` flow is consistent with the same token-matched `setting*.xml` cache plus explicit modification-token save model we now mirror. Treat this as strong current evidence, not a mathematical proof that no hidden opcode exists anywhere.
- [ ] Decide whether the desktop daemon's UDP discovery should advertise when bridging a local USB device. The 2026-04-10 experiment showed the official Device Console discovers the daemon via UDP broadcast and (until the TCP bridge fix) crashed when using the network path. Even with the TCP bridge fix, there is still a question of whether broadcasting a local-USB-bridge daemon is desirable behavior or should be opt-in (e.g., a `--no-advertise-local` flag).
- [ ] End-to-end verify the official MiniDSP Device Console Electron app against the TCP bridge fix (2026-04-10):
  - RE update 2026-04-13: Ghidra confirms `app/MiniDSP Device Console.app/Contents/MacOS/MiniDSP Device Console` is only the Electron launcher (`entry` just dispatches to `_ElectronMain`), so the extracted `app/app-extracted/build/src/*.js` remains the source of truth for bridge compatibility.
  - Extracted-UI + direct parser probes on 2026-04-13 confirmed the exact network reply contract the official app enforces for the preset-save path:
    - `WriteFlashBlock (0x06)` success must be compact `OK` (`[0x01]`), not a command-shaped `[len][0x06 ...]` echo
    - `FinishFlashBlock (0x07)` success must be `[0x02, block_id]`, not `[len][0x07 ...]`
    - `ChangePreset (0x25)` with reset mode `0x00` expects `OK`; reset modes `0x01` and `0x02` expect preset-complete (`[0x02, 0xAB]`)
  - The shared TCP reply encoder now reflects those RE-confirmed rules in `protocol/src/packet.rs::encode_device_console_tcp_response`, and both the daemon TCP bridge and ESP32 TCP bridge now use it. `cargo +esp test -p minidsp-protocol --lib`, `cargo +esp test -p minidsp-daemon --lib`, and `cd app-esp32 && esp cargo build` all passed on 2026-04-13.
  - Live verification update 2026-04-13: after reflashing the attached ESP32-S3 board `firmware-board`, the official macOS Device Console completed the network-device `Initialize to Default Settings` flow end to end with no JS error and no error dialog. The log showed `GetFinishFlashBlockDspCmd sent` twice, immediate listener cleanup on both completions, `Reloading Preset 1...`, `ChangePresetSlotDspCmd sent`, `reloadCurrentPreset: preset-change-completed`, `Reload Preset Completed`, and `Initialize-to-Default completed.`
  - Remaining scope is the broader interactive surface, not the reset/save failure: master status, volume, mute, source, preset switch, routing matrix view/edit, PEQ view/edit, compressor view/edit
  - If anything still crashes, capture the HID log and compare it against the Python wire-level verification to identify any remaining Device-Console-specific framing quirks
  - This clears the official-app reset/save blocker tracked in `docs/todo-checker.md` Section 4.2; the broader interactive-surface pass remains open.
- Reference capture for 2026-04-10 Device Console first-attach findings: `app/minidsp-snapshots/20260410-firstattach-experiment/` (HID capture `device-console-firstattach.jsonl`, Device Console cache states before and after).
- [ ] Audible retest of compressor on loud material: enable compressor (status=2), set threshold high enough to trigger (e.g. -20 dBFS), verify ratio/attack/release have audible effect. This is the last remaining compressor item — the protocol is fully confirmed correct.
- [ ] Implement the first pass of learnings from `app/minidsp-console-app-log.jsonl`
  - turn the canonical raw capture into labeled fixtures for the command families we already support (`0x04`, `0x05`, `0x06`, `0x07`, `0x13`, `0x19`, `0x25`, `0x30`, `0x34`, `0x40`, `0x42`)
  - diff those request/response pairs against the shared client and queue behavior and close any remaining opcode or response-shape drift
  - keep using the decoded official preset-save pipeline plus the companion session log's explicit token-save behavior as evidence that the remaining DSP100 compressor/runtime-state gap is not hidden behind `0x06` / `0x07`
  - the full opcode table is now compiled in `docs/NOTES-device-console-hid-logging.md`; cross-check any new captures against that table
- [ ] Resolve exact runtime identity for the remaining official USB-only labels if stronger official evidence is found:
  - `HA-DSP`
  - `DDRC-22`
  - `DDRC-88`
  - `CDSP-8x12/DL`
  - `Harmony DSP`
- [ ] Continue importer/codegen work only where it improves source-backed device identity or typed metadata, not as hand-maintained guesswork.
- [ ] Keep the generated coverage artifacts current whenever device-matrix evidence changes.

## Verified Foundation

- [x] Shared protocol/config fixes validated on the physical 2x4 HD.
- [x] Desktop and ESP32 config write/read behavior aligned for the implemented 2x4 HD path.
- [x] Modern official matrix coverage gated by extracted app assets.
- [x] Modern DSP address tables gated by extracted XML indexes.
- [x] Legacy cached `config.xml` families gated by generated metadata and source-backed runtime tables.
- [x] Shared USB acceptance policy reduced to the official extracted VID/PID set.

## Known Boundaries

- [x] The extracted desktop fallback-model picker is not a second identity catalog.
- [x] `app/MiniDSP Device Console.app/Contents/MacOS/MiniDSP Device Console` is only an Electron bootstrap.
- [x] `app/apk-extracted/classes.dex` is Cordova shell/transport glue, not a richer device-definition source.
- [x] Public website/manual evidence may stabilize product-family and software-family naming, but exact runtime identity still requires extracted app/native evidence.

## Remaining Support Boundary

- [ ] `HA-DSP`
  - Public product family known.
  - Public software family known: `HA-DSP-CF`.
  - Exact runtime identity still unresolved.
- [ ] `DDRC-22`
  - Public product family known.
  - Public software family known: `DDRC-22 Utility`.
  - Source-backed small Dirac runtime bucket known.
  - Exact runtime identity still unresolved.
- [ ] `DDRC-88`
  - Public product family known: `DDRC-88A / DDRC-88D`.
  - Public software family known: `DDRC-88BM`.
  - Source-backed legacy 8-channel Dirac bucket known.
  - Exact runtime identity still unresolved.
- [ ] `CDSP-8x12/DL`
  - Public product family known.
  - Public software family known: `C-DSP 8x12 DL`.
  - Source-backed custom-install 8x12 Dirac bucket known.
  - Exact runtime identity still unresolved.
- [ ] `Harmony DSP`
  - Public product family known: `Harmony DSP 8x12`.
  - Public software family known: `C-DSP 8x12 DL`.
  - Source-backed HT-family 8x12 Dirac bucket known.
  - Exact runtime identity still unresolved.

## Deferred

- [ ] Desktop `/play` hardware validation on the real MiniDSP USB-audio path:
  - live desktop daemon re-check on 2026-04-14 confirmed the canonical default `/devices/0/play {}` response on the physical 2x4 HD after rebuilding the stale local binary: `toneBurstOffsetSamples=96000`, `sweepOffsetSamples=99000`, `tailToneBurstOffsetSamples=363458`, `totalSamples=460058`
  - the user also audibly confirmed sweep-only, marker-only, and combined default marker+sweep playback on the real sub path
  - the later contract pass also confirmed deterministic repeated offsets, legacy/invalid-request `400`s, `markerCycles=0` sweep-only collapse, and non-USB-source auto-switch + restore from `Toslink`
  - the remaining desktop work here is optional broader coverage on other source families / devices, not uncertainty in the current 2x4 HD contract
- [ ] Desktop `POST /sync` follow-up verification / cleanup
  - live re-check on 2026-04-14 confirmed `/sync.serverTimeUs` increases across calls and brackets desktop `/play.startedAtUs` on the same wall-clock-derived microsecond basis
- [ ] PWR-ICE compatibility verification
- [ ] Multi-device live verification
- [ ] CI for desktop targets
- [ ] CI for ESP32 cross-build
- [ ] ezBEQ expansion after core device-matrix work is stable
- [ ] ESP32 VBUS GPIO drive — not needed on DevKitC boards (both boards powered via UART); only needed for custom PCB where ESP32-S3 must supply 5V to the MiniDSP over the OTG port (GPIO19 high before `usb_host_install`)

## Standard Validation

- `uv run python tools/extract_device_support_matrix.py`
- `cargo test -p minidsp-protocol`
- `cargo test`
