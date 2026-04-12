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
- ESP32 `/devices/{id}/config` parity is fixed and verified on the physical 2x4 HD.
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
  - desktop direct-USB `/play` on the physical 2x4 HD now uses `toneBurstOffsetSamples=96000`, `sweepOffsetSamples=96000`, and `totalSamples=454058` while keeping source/routing unchanged before and after playback
  - the older measurement-bundle default (`5-500 Hz` with BRAP bursts) remains retired as historical context only
  - audible confirmation on the real sub is still required before treating the current default as fully validated
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
- [ ] Validate the new shared `/play` default signal on the real sub and keep iterating until it sounds like a normal smooth low-frequency sweep:
  - the previous measurement-bundle default (`5-500 Hz`, `24-bit`, `2 s` preamble, `80 Hz` tone burst, `0.2 s` gap, `2 s` decay, hidden guard-band expansion) was retired after it produced repeated `BRAP` bursts on hardware
  - the current shared default is now a REW/MSO-style `5-400 Hz`, `5.46 s`, `16-bit` hybrid sweep with `2 s` low-level dither pre-silence, `2 s` post-silence, and `20 ms` raised-cosine fade-in/out
  - below `10 Hz`, the current default intentionally uses a linear `5-10 Hz` section before switching to logarithmic `10-400 Hz`
  - a 2026-04-10 qualitative listening check on the physical 2x4 HD after the ESP32 core-1 + pre-gen watchdog fixes described that same default as "actually sounded pretty good", which is a positive partial signal but not a full audible validation
  - the remaining work here is a proper audible comparison against a known-good measurement rig (for example REW direct), not further ESP32 transport investigation
  - if the new default is still wrong after that, the next suspect is PCM/sample-format behavior rather than routing or HID transport
- [ ] Verify remaining `/play` compatibility edges on hardware where possible:
  - malformed request handling
  - sample-rate switch between plays
  - HID-only-device clear error path
- [x] Prove on hardware that the current shared `0x13` route-enable write path restores audible USB playback on DSP100 / 2x4 HD without relying on the official app
  - confirmed live on 2026-04-09: after the shared HID echo-decoder fix, ESP32 route writes made `/play` audible on physical output `3`
- [x] Make DSP100 / 2x4 HD routing-state read-back survive fresh desktop daemon reconnects via persisted verified shadow keyed by device serial
- [x] Make DSP100 / 2x4 HD FIR `active_taps` read-back survive fresh desktop daemon reconnects via persisted verified shadow keyed by device serial
- [x] ~~Continue DSP100 / 2x4 HD authoritative-state reverse engineering so fresh-client `/devices/{id}/config` can eventually prefer real hardware readback over persisted shadow for routing and FIR tap count~~
  - **CLOSED 2026-04-10:** no stronger authoritative readback path has been found in the first-attach experiment, the extracted desktop sources, the captured session logs, or the current Ghidra pass. When the official app has no local cache for a device, it offers only "Initialize to default Settings" or "Disconnect", and the extracted `userPreset` / `userPresetMgr` flow is consistent with the same token-matched `setting*.xml` cache plus explicit modification-token save model we now mirror. Treat this as strong current evidence, not a mathematical proof that no hidden opcode exists anywhere.
- [ ] Decide whether the desktop daemon's UDP discovery should advertise when bridging a local USB device. The 2026-04-10 experiment showed the official Device Console discovers the daemon via UDP broadcast and (until the TCP bridge fix) crashed when using the network path. Even with the TCP bridge fix, there is still a question of whether broadcasting a local-USB-bridge daemon is desirable behavior or should be opt-in (e.g., a `--no-advertise-local` flag).
- [ ] End-to-end verify the official MiniDSP Device Console Electron app against the TCP bridge fix (2026-04-10):
  - Python TCP client wire-level verification on the physical 2x4 HD already confirmed the response sizes are now correct and the `Unexpected DSP command code 1` crash signature can no longer be produced by our bridge
  - Still to do: start the daemon, launch the official Device Console (patched capture build under `app/MiniDSP Device Console.app/` with `MINIDSP_DC_HID_LOG=/tmp/minidsp-bridge-verify.jsonl`), click the network device entry at the Mac's LAN IP, and confirm the app completes the connect without a JS error
  - Then exercise the basic interactive set: master status, volume, mute, source, preset switch, routing matrix view/edit, PEQ view/edit, compressor view/edit
  - If anything still crashes, capture the HID log and compare it against the Python wire-level verification to identify any remaining Device-Console-specific framing quirks
  - This task closes `docs/todo-checker.md` Section 4.2 entries 2 and 3 (the UDP discovery entry is already closed)
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
  - verify desktop `/devices/{id}/play` against the physical 2x4 HD
  - verify source auto-switch to `Usb` and restore on real hardware
  - verify the desktop timing/response behavior is useful for direct comparison with ESP32
- [ ] Desktop `POST /sync` follow-up verification / cleanup
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
