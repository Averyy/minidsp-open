# USB Audio: Measurement Sweep Playback

## Goal

Add `POST /devices/{id}/play` and `POST /sync` endpoints. The device generates a measurement sweep mathematically, plays it through the miniDSP's USB audio input via the UAC2 driver, and returns the exact playback start timestamp so the caller can align their recording.

**Scope:** Both ESP32 and desktop expose these endpoints. The primary value is ESP32 (cable-free measurement over WiFi), but desktop support is useful for Raspberry Pi setups and client parity. The sweep generator is shared (pure math, no platform deps). The UAC2 transport is ESP32-specific; desktop uses a platform audio backend (tracked separately). These are **additive endpoints** — they extend the existing API without breaking any existing routes (per `docs/ref-compatibility.md` additive rules).

**Use case:** Cable-free subwoofer measurement. A caller (open-sub-optimizer, REW, or plain `curl`) triggers a sweep over WiFi. The ESP32 outputs audio to the miniDSP. The caller captures via USB mic and deconvolves.

## Validation Snapshot

- Real hardware validated on ESP32-S3 + miniDSP 2x4 HD using a **release** firmware build (`cargo build --release`)
- Default `POST /devices/0/play` response arrives in about `0.35s` when already on `Usb`, and about `0.4-0.5s` when `/play` first switches from `Toslink` to `Usb`
- Near-full-ring chunking was the first confirmed root cause: `~8190` byte writes into an `8192` byte TX ring stretched completion to about `13.6s`
- Half-ring chunking (`4092` byte writes) fixed the main pacing bug:
  - `TX_DONE` low-water callbacks dropped from hundreds per run to `0`
  - `last_write_after_start` fell from about `13.56s` to about `12.52s`
- Generator hot-path optimization reduced generation time from about `729ms` to about `483ms` on the default plan
- A second generator refactor that emits contiguous regions directly (instead of branching per-sample across the whole signal) reduced default-plan generation time again, from about `483ms` to about `333ms`
- Source auto-switch is verified live: `Toslink -> Usb -> Toslink`
- Transient bad master-status reads immediately after source changes are handled with short retries
- First overlap attempt (generator thread + blocking writer thread) improved the design direction but failed on real hardware because per-chunk heap allocation was not stable enough (`memory allocation of 8192 bytes failed`)
- Second overlap attempt (fixed two-slot ping-pong to avoid per-chunk allocation) also failed to produce a shippable single-play path; it did not crash, but it stopped making forward progress reliably enough to treat as a regression
- A `20ms` time-based chunk target was also worse than the half-ring baseline:
  - `writes=612`
  - `TX_DONE=611`
  - `last_write_after_start ~= 12.53s`
  - conclusion: larger steady-state chunks reintroduced refill pressure without improving end-to-end timing
- Current best known single-play implementation is still:
  - half-ring chunking
  - optimized generator hot path
  - single playback thread with no extra producer thread
- A startup-path experiment that moved the priming write into the HTTP handler was rejected even though it improved the internal "response ready" point to about `208ms`: on real hardware it immediately reintroduced refill pressure (`TX_DONE=835`) and is therefore not safe
- Current timing characterization on real hardware:
  - `48kHz / 24-bit / stereo`: `tx_need_data=0`, `gen≈333ms`, `write≈12.21s`, `last_write_after_start≈12.56s`
  - `48kHz / 16-bit / stereo`: `tx_need_data=0`, `last_write_after_start≈12.39s`
  - `44.1kHz / 24-bit / stereo`: `tx_need_data=0`, `last_write_after_start≈12.29s`
  - conclusion: once underruns are eliminated, the remaining delta scales with byte rate and enqueue timing; it is not evidence of a UAC2 driver clock/stretch bug

**Performance note:** Use a **release** firmware build for any timing or glitch validation. Debug (`cargo build`) builds are dramatically slower in the sweep-generation hot path and are not representative for `/play`.

## Timing Investigation Checklist

Goal: explain and, if possible, eliminate the remaining gap between nominal signal duration (`12.237s`) and observed playback-thread completion time (`~13.6s`) on real hardware.

- [x] Add precise playback timestamps in the ESP32 `/play` path:
  - stream start call
  - first nonzero `startedAtUs`
  - first HTTP response sent
  - last successful write return
  - stop call / stop return
  - source-restore start / finish
- [x] Add expected-vs-actual byte accounting:
  - expected total bytes
  - actual bytes queued
  - write count
  - first / last write sizes
- [x] Add write-latency distribution stats:
  - min / max / avg / p50 / p95 / p99
- [x] Count `UAC2_HOST_DEVICE_EVENT_TX_DONE` notifications to detect underrun / refill pressure
- [x] Measure whether the extra time is:
  - before first queued sample
  - during steady-state writes
  - after the last write while stopping / draining
- [x] Run release-build hardware measurements with current chunk sizing
- [x] Run release-build hardware measurements with smaller chunk sizes to see whether near-full-ring writes are causing avoidable backpressure
- [x] Compare 24-bit vs 16-bit and 48kHz vs 44.1kHz timing
- [x] Measure source-switch overhead separately from playback time
- [ ] Capture or otherwise verify the physical output duration once software timestamps are clear
- [x] Decide whether the remaining gap is:
  - real playback stretch
  - enqueue pacing only
  - stop/drain overhead
  - acceptable transport latency vs bug
- [ ] Capture or otherwise verify the physical output duration once software timestamps are clear
- [ ] Update this doc and `PROGRESS.md` with the final root cause and validated fix

**Current conclusion:** the original `~13.6s` issue was a real app-side pacing bug caused by near-full-ring writes. That is fixed. The remaining `~12.29-12.56s` enqueue-finish metric depends on byte volume (`48/24` > `48/16` > `44.1/24`), stays at `TX_DONE=0`, and has negligible stop time, which is consistent with app-side enqueue timing against the driver ring buffer and `startedAtUs` semantics, not with a driver-side sample-clock defect.

## Definition of Done

The feature is complete when:
- [ ] `curl -X POST http://<esp32>:5380/devices/0/play` plays a ~12-second measurement sweep through the miniDSP's analog outputs
- [ ] The response arrives within ~200ms (before playback finishes) and contains a real `startedAtUs` hardware timestamp plus correct sample offsets
- [ ] `curl -X POST http://<esp32>:5380/sync -d '{"clientTimeUs":0}'` returns an ESP32 timestamp (works with or without a miniDSP connected)
- [ ] HID control (volume, mute, config, etc.) continues working normally during and after audio playback
- [ ] Audio playback at 48kHz/24-bit/stereo has no audible glitches over the full sweep duration
- [ ] USB disconnect mid-playback doesn't crash — cleans up gracefully
- [ ] The sweep generator has desktop unit tests (`cargo test` in the workspace, no ESP32 needed)
- [ ] The firmware builds and boots cleanly with zero new warnings
- [ ] Back-to-back plays work: sweep finishes, immediately `POST /play` again — full stop→cleanup→re-init cycle succeeds
- [ ] Play then HID: after a sweep completes, `GET /devices/0/config` and other HID commands still work correctly
- [ ] Heap survives playback: free heap doesn't drop dangerously low with WiFi + HTTP + HID + UAC2 all active (log free heap before/during/after)
- [ ] HID-only device: `POST /play` on a device without USB audio (e.g., 2x4 non-HD) returns a clear error, not a crash or hang
- [ ] Malformed requests: empty body uses defaults, garbage JSON returns 400, missing Content-Type is handled gracefully
- [ ] Sample rate switch between plays: play at 48kHz, then play at 44.1kHz — clock reconfiguration works

## Task Ordering

```
Tasks 4 (sweep_gen) and 6 (/sync) have NO dependencies — start here.
Tasks 1 → 2 → 3 are sequential (build → FFI → boot integration).
Task 5 (/play endpoint) depends on all others.
```

## What Already Exists

| What | Where | Status |
|------|-------|--------|
| UAC2 host driver (C, ESP-IDF component) | `~/code/esp-uac2-host/components/uac2_host/` | **Complete v0.1.0** — 12 tests pass on real miniDSP 2x4 HD, zero errors, 766s stability run |
| UAC2 public API | `~/code/esp-uac2-host/components/uac2_host/include/usb/uac2_host.h` | Full API with install/uninstall, open/close, start/stop/write, clock, volume |
| UAC2 descriptor structs | `~/code/esp-uac2-host/components/uac2_host/include/usb/uac2_desc.h` | Clock source/selector, terminals, feature unit, AS, format type |
| ESP32 firmware (Rust on ESP-IDF) | `app-esp32/` | Boots, WiFi, USB HID, HTTP API, mDNS, TCP bridge, UDP discovery — all working |
| USB HID transport | `app-esp32/src/usb_hid.rs` | Raw `esp_idf_sys` FFI, calls `usb_host_install()`, claims HID interface |
| HTTP server | `app-esp32/src/http_server.rs` | `EspHttpServer` with all device routes, debug endpoints, ezBEQ |
| Device state | `app-esp32/src/device.rs` | `DeviceState` wrapping `Arc<UsbHidBus>` + protocol layer |
| miniDSP 2x4 HD simulator | `~/code/esp-uac2-host/simulators/minidsp-2x4hd/` | Full composite: UAC2 audio + HID, fault injection, 373-byte descriptor match |
| UAC2 driver reference doc | `~/code/esp-uac2-host/llms.txt` | Complete API reference, internal design, hardware constraints |

## Architecture Overview

The miniDSP is a **composite USB device** with HID (control) and Audio (playback) interfaces on the same USB connection. The ESP32-S3 USB Host Library supports multiple clients — the existing HID client and the new UAC2 driver client coexist.

```
                     ┌──────────────────────────────────┐
                     │          miniDSP 2x4 HD          │
                     │  (composite USB device)          │
                     │  ┌─────────┐  ┌──────────────┐  │
                     │  │  HID    │  │  UAC2 Audio   │  │
                     │  │ (IF 4)  │  │  (IF 1, alt1) │  │
                     │  └────▲────┘  └──────▲───────┘  │
                     └───────┼──────────────┼──────────┘
                             │              │
                     ┌───────┼──────────────┼──────────┐
                     │   ESP-IDF USB Host Library       │
                     │       │              │           │
                     │  ┌────┴────┐  ┌──────┴───────┐  │
                     │  │HID clnt │  │ UAC2 driver  │  │
                     │  │(Rust    │  │ (C component │  │
                     │  │ FFI)    │  │  via FFI)    │  │
                     │  └────▲────┘  └──────▲───────┘  │
                     └───────┼──────────────┼──────────┘
                             │              │
                 ┌───────────┴──┐   ┌───────┴──────────┐
                 │ usb_hid.rs   │   │ usb_audio.rs     │
                 │ (existing)   │   │ (NEW)            │
                 └───────▲──────┘   └───────▲──────────┘
                         │                  │
                 ┌───────┴──────────────────┴──────────┐
                 │          main.rs boot sequence       │
                 │  1. usb_host_install()               │
                 │  2. UsbHidBus::init() (HID client)   │
                 │  3. uac2_host_install() (audio clnt) │
                 └───────────────────▲─────────────────┘
                                     │
                 ┌───────────────────┴─────────────────┐
                 │        HTTP server handlers          │
                 │  POST /devices/{id}/play             │
                 │  POST /sync                          │
                 └─────────────────────────────────────┘
```

## Implementation Tasks

### Task 1: Add esp-uac2-host as a C component to the build

The UAC2 driver is a C ESP-IDF component. The Rust ESP32 project needs to compile and link it.

**What to do:**
- Add the component path so ESP-IDF discovers it during build
- Add required sdkconfig options for isochronous audio
- Ensure `esp-idf-sys` bindgen picks up the UAC2 headers

**Build integration options** (pick the simplest that works):
- `extra_components` in `Cargo.toml` `[package.metadata.esp-idf-sys]` section
- Or symlink/copy the component into `app-esp32/components/uac2_host/`
- Or set `EXTRA_COMPONENT_DIRS` in the build

**Required sdkconfig additions** (`app-esp32/sdkconfig.defaults`):
```
CONFIG_USB_HOST_HW_BUFFER_BIAS_PERIODIC_OUT=y  # Bias DWC_OTG FIFO for isochronous OUT (required for audio playback)
```
Note: `CONFIG_USB_HOST_CONTROL_TRANSFER_MAX_SIZE=512` is already set.

**Acceptance criteria:**
- [ ] `cargo build` (for ESP32 target) compiles with the UAC2 component included and zero new warnings
- [ ] The UAC2 header types (`uac2_host_driver_config_t`, `uac2_host_device_handle_t`, etc.) are accessible from Rust via `esp_idf_sys` bindings or a local `bindings.rs`

### Task 2: Rust FFI bindings for the UAC2 driver

Create a safe Rust wrapper around the C UAC2 driver API. The driver is event-driven (callbacks) but the ESP32 firmware is blocking/synchronous.

**Key C functions to wrap** (from `uac2_host.h`):
```c
esp_err_t uac2_host_install(const uac2_host_driver_config_t *config);
esp_err_t uac2_host_uninstall(void);
esp_err_t uac2_host_device_open(const uac2_host_device_config_t *config, uac2_host_device_handle_t *handle);
esp_err_t uac2_host_device_close(uac2_host_device_handle_t dev);
esp_err_t uac2_host_device_start(uac2_host_device_handle_t dev, const uac2_host_stream_config_t *config);
esp_err_t uac2_host_device_stop(uac2_host_device_handle_t dev);
esp_err_t uac2_host_device_write(uac2_host_device_handle_t dev, const uint8_t *data, uint32_t size, uint32_t timeout_ms);
int64_t   uac2_host_device_get_start_time(uac2_host_device_handle_t dev);
```

**Design notes:**
- The driver's event callbacks fire from the USB Host client task — they must not block
- The driver callback reports `UAC2_HOST_DRIVER_EVENT_TX_CONNECTED` with `addr` + `iface_num` — stash these so a later `open` call can use them
- Use `Condvar` or `AtomicBool` to signal from C callback to Rust (same pattern as `usb_hid.rs` `XferSync`)
- The driver handle is an opaque pointer — wrap in a struct that implements `Send` (transfers only happen from one thread at a time)

**Acceptance criteria:**
- [ ] A Rust module (`app-esp32/src/usb_audio.rs`) that wraps the UAC2 C API
- [ ] `install()` / `uninstall()` manage the driver lifecycle
- [ ] Can detect when a UAC2 playback interface connects (callback → stored state)
- [ ] Can open, start streaming, write PCM data, get start timestamp, stop, and close
- [ ] Error handling converts `esp_err_t` to Rust `Result`

### Task 3: Integrate UAC2 driver into boot sequence

The UAC2 driver needs to be installed after `usb_host_install()` (which happens inside `UsbHidBus::init()` in `usb_hid.rs`).

**Current boot sequence** (`app-esp32/src/main.rs`):
1. Logger → NVS → Storage → WiFi
2. `UsbHidBus::init()` — calls `usb_host_install()`, registers HID client, starts event threads
3. `bus.wait_for_device()` — waits for MiniDSP
4. `DeviceState::probe()` — reads HID identity
5. HTTP server starts

**Modified sequence — add after step 2:**
- Create `SharedAudioState` (see below) and pass it to the UAC2 install callback
- Call `uac2_host_install()` (via the Rust wrapper) — registers a second USB Host client
- The UAC2 driver will automatically discover the audio interface when the miniDSP connects
- Pass `SharedAudioState` to `http_server::start()` alongside the existing `SharedDeviceState`

**Shared audio/playback state design:**

The audio state is **independent** of the HID `SharedDeviceState`. They have separate lifecycles:

```rust
/// Audio subsystem state — created at boot, lives for the process lifetime.
/// UAC2 callbacks update `device`; HTTP handlers check `device` and `playing`.
pub struct AudioState {
    /// UAC2 device handle — set by connect callback, cleared by disconnect callback.
    /// None means no audio-capable device is currently connected.
    device: Mutex<Option<Uac2Device>>,
    /// Playback lock — true while a /play request is actively streaming.
    /// Checked atomically by /play to reject concurrent requests (409).
    playing: AtomicBool,
    /// Signals UAC2 device arrival (for callers that want to wait).
    connected: Condvar,
}
pub type SharedAudioState = Arc<AudioState>;
```

**Lifecycle rules:**
- `AudioState` is created once at boot, before `uac2_host_install()`
- The UAC2 **connect callback** sets `device` to `Some(handle)` and notifies `connected`
- The UAC2 **disconnect callback** sets `device` to `None`. If `playing` is true, the in-flight playback thread will see write failures and clean up on its own (it clears `playing` and drops its handle clone).
- The **main loop does NOT manage audio state** — it only manages HID `SharedDeviceState` as today. Audio lifecycle is entirely callback-driven.
- On reconnect: connect callback replaces the handle. Next `/play` just works.
- HID disconnect does not invalidate audio state, and vice versa. They are independent clients on the same composite device.

**Important:** `usb_host_install()` must only be called once. It's currently called inside `UsbHidBus::init()`. Do NOT call it again. `uac2_host_install()` is a separate function that registers a new client on the already-installed USB Host Library. **Do NOT modify `usb_hid.rs`** — it already provides the shared infrastructure (global `usb_host_install()` + `usb_host_lib_handle_events()` pump) that the UAC2 driver needs. The UAC2 driver registers its own client alongside HID. Add the UAC2 integration in a new `usb_audio.rs`, not inside `usb_hid.rs`.

**Acceptance criteria:**
- [ ] UAC2 driver installs successfully during boot (visible in serial log)
- [ ] When a miniDSP connects, both HID and UAC2 interfaces are discovered (HID by existing code, audio by UAC2 driver callback)
- [ ] `SharedAudioState` is created at boot and passed to both the UAC2 callback and the HTTP server
- [ ] UAC2 connect/disconnect callbacks correctly update `AudioState.device` without touching HID state
- [ ] If no miniDSP is connected, boot continues normally (audio just stays unavailable)
- [ ] USB reconnect: if the miniDSP disconnects and reconnects, the UAC2 driver rediscovers the audio interface and updates `AudioState`
- [ ] During playback, a USB disconnect causes the playback thread to clean up (clear `playing`, drop handle) without crashing

### Task 4: Sweep generator

Implement mathematical real-time audio generation. **No file transfer, no WAV files, no storage. Generate sample-by-sample.**

**Signal structure:**
```
[preamble: 2s low-level dither] [tone burst: 3 cycles @ 80Hz] [gap: 200ms silence] [sweep: 8s Farina ESS] [decay: 2s silence]
Total: ~12.24 seconds at 48kHz = ~588,288 samples
```

**Farina exponential sine sweep formula:**
```
x(t) = sin(2pi * f1 * T / ln(f2/f1) * (exp(t * ln(f2/f1) / T) - 1))
```

**Default parameters** (overridable via API):
- `f1 = 5 Hz` (display start; actual sweep starts at f1/2 = 2.5 Hz for guard band)
- `f2 = 500 Hz` (display stop; actual sweep goes to min(2*f2, fs/2) = 1000 Hz)
- `duration = 8 seconds`
- `fs = 48000 Hz`
- Tukey fade-in: 0.02s

**Implementation approach:**
- Implement as a state machine / iterator that produces samples on demand
- Each call returns a chunk of PCM samples (e.g., 288 bytes = one USB frame at 48kHz/24-bit/stereo)
- Stereo: both channels identical
- Use `f32` std math (`sin`, `exp`, `ln`) — ESP32-S3 has hardware FPU. No `libm` crate needed.
- At 240 MHz with hardware FPU, `sin(exp(x))` is ~50-100 cycles per sample = ~4% CPU at 48kHz stereo. Trivially feasible.
- **This module must have zero ESP-IDF dependencies** (`no use esp_idf_*`) — it's pure math + PCM encoding. This is what makes it desktop-testable via `cargo test`.

**Zero-crossing truncation** (must match `open-sub-optimizer/src/sweep-gen.ts`):
- Generate the raw sweep for `round(duration * fs)` samples
- Walk backward from the end to find the last sign change (where `raw[n] * raw[n+1] <= 0`)
- Truncate the sweep at that zero crossing (`nSamples = lastZero + 1`)
- Apply the Tukey fade-in to the truncated sweep (same order as the TypeScript reference)
- `totalSamples` in the `/play` response reflects the truncated sweep length (plus preamble/tone burst/gap/decay)
- This ensures deconvolution window alignment matches the existing `open-sub-optimizer` client

**Note on decay duration:** The default `decaySec` is 2.0 seconds (vs. `open-sub-optimizer`'s 1.5s default for trailing silence). 2s is more conservative for room decay capture. The difference is intentional but the parameter is overridable via the API, so clients can request 1.5s if they prefer parity.

**Preamble**: Low-level white noise or dither (-60 dBFS). Purpose: stabilize miniDSP ASRC lock (~2 seconds).

**Tone burst**: `sin(2pi * 80 * t)` for 3 cycles (37.5ms). Purpose: timing marker for cross-correlation alignment.

**PCM encoding:**
- 24-bit: pack each `f32` sample as 3 bytes little-endian (multiply by 2^23 - 1, clamp, take lower 3 bytes)
- 16-bit: pack as 2 bytes little-endian (multiply by 2^15 - 1)
- Stereo interleaved: L, R, L, R, ...

**Acceptance criteria:**
- [ ] A module (`app-esp32/src/sweep_gen.rs`) that generates the complete signal
- [ ] Configurable parameters: f1, f2, duration, fade, tone burst frequency/cycles, preamble/gap/decay durations, sample rate, bit depth
- [ ] Produces PCM bytes on demand (iterator/callback pattern, not allocating the entire signal)
- [ ] 24-bit stereo PCM at 48kHz by default
- [ ] Total sample count is deterministic and known before generation starts
- [ ] Unit-testable: can run on desktop (`cargo test`) by testing the math/PCM encoding without USB

### Task 5: POST /devices/{id}/play endpoint

The HTTP handler that ties everything together.

**Request:**
```
POST /devices/{deviceIndex}/play
Content-Type: application/json

{
  "sweep": {
    "f1": 5,
    "f2": 500,
    "duration": 8,
    "fadeDuration": 0.02
  },
  "toneBurst": {
    "frequency": 80,
    "cycles": 3
  },
  "preambleSec": 2,
  "gapSec": 0.2,
  "decaySec": 2,
  "sampleRate": 48000,
  "bitDepth": 24
}
```

All fields are optional — use defaults above when omitted.

**Response** (returned after playback starts, not after it finishes):
```json
{
  "startedAtUs": 1719482837291456,
  "toneBurstOffsetSamples": 96000,
  "sweepOffsetSamples": 105600,
  "totalSamples": 588288,
  "sampleRate": 48000,
  "sourceChanged": true,
  "previousSource": "toslink"
}
```

- `startedAtUs` — from `uac2_host_device_get_start_time()`, the ESP32 `esp_timer_get_time()` value when the first isochronous URB was submitted. Microsecond precision.
- `toneBurstOffsetSamples` — sample index where tone burst begins (= preamble_samples)
- `sweepOffsetSamples` — sample index where sweep begins (= preamble + tone_burst + gap samples)
- `totalSamples` — total signal length in samples (reflects zero-crossing-truncated sweep length)
- `sourceChanged` — `true` if `/play` switched the device source to USB, `false` if it was already on USB
- `previousSource` — the source that was active before `/play` switched (only present when `sourceChanged` is `true`). Source will be restored after playback completes.

**Playback flow:**
1. Parse request parameters (or use defaults)
2. Check that UAC2 audio interface is connected (`AudioState.device.is_some()`) → 503 if not
3. Check that no other playback is in progress (`AudioState.playing`) → 409 if busy
4. Set `AudioState.playing = true`
5. **Source auto-switch:** Read current source via HID status → save as `prev_source`. If current source is not `Usb`, send `set_source(usb_index)` via HID. If source is already `Usb`, skip.
6. Start the UAC2 stream (`device_start` with requested sample rate / bit depth)
7. Get `startedAtUs` from `device_get_start_time()`
8. Compute sample offsets
9. **Return the HTTP response immediately** (caller now knows when playback started)
10. Continue generating + writing samples in a background thread until complete
11. Stop the stream after the last sample
12. **Source restore:** Send `set_source(prev_source)` to restore the original source. Best-effort — if the device disconnected mid-play, skip restore (device is already gone).
13. Clear `AudioState.playing = false`

**Source switching semantics:**
- `/play` always auto-switches to `Usb` source before playback and restores the previous source after completion
- If source switch fails (HID error), fail the `/play` request before starting audio — return 500
- If device disconnects mid-play: playback thread sees write failures, cleans up, does NOT attempt source restore (device is gone)
- The response includes `sourceChanged` and `previousSource` so the caller knows what happened

**Sample rate note:** The `/play` `sampleRate` parameter defaults to **48000 Hz** regardless of `DeviceProfile.sample_rate`. `DeviceProfile.sample_rate` describes the DSP's internal processing rate (e.g., 96kHz for SHD family). USB playback rate is negotiated separately with the UAC2 device. The implementation must NOT use `profile.sample_rate` as the playback default.

**Important design decisions:**
- The HTTP response returns as soon as streaming starts (after `device_start` + first write), NOT after all 12 seconds of audio finish. The caller needs the timing info early to align their capture.
- Use `AudioState.playing` (AtomicBool) to prevent concurrent playback requests.
- Playback runs in a dedicated thread. The HTTP handler spawns it, gets the start timestamp back via a channel or shared state, then responds.
- If the USB device disconnects mid-playback, the write calls will fail — clean up gracefully (clear `playing`, skip source restore).

**Acceptance criteria:**
- [ ] `POST /devices/{id}/play` with default parameters plays a ~12s sweep through the miniDSP's audio interface
- [ ] Response is returned within ~200ms of the request (not 12 seconds later)
- [ ] `startedAtUs` value is a real ESP32 hardware timer reading, not a fake timestamp
- [ ] `toneBurstOffsetSamples` and `sweepOffsetSamples` are correct (match the actual signal structure)
- [ ] Concurrent requests return 409 Conflict
- [ ] Request on a device where `has_usb_audio` is `false` returns 400 ("device does not support USB audio playback")
- [ ] Request with `has_usb_audio: true` but no UAC2 connection returns 503 Service Unavailable
- [ ] Source auto-switch: if device is not on USB source, `/play` switches to USB before playback and restores the previous source after completion
- [ ] Source auto-switch: `sourceChanged` and `previousSource` are correct in the response
- [ ] Source auto-switch: if device is already on USB, no switch occurs and `sourceChanged` is `false`
- [ ] Audio plays at 48kHz (default), not at `DeviceProfile.sample_rate`
- [ ] Playback completes and cleans up (stops stream, restores source, clears `playing` flag)
- [ ] Custom parameters (e.g., different f1/f2, different duration) are honored

### Task 6: POST /sync endpoint

Simple clock sync for callers that need timing alignment.

**Request:**
```
POST /sync
Content-Type: application/json

{ "clientTimeUs": 1719482837000000 }
```

**Response:**
```json
{
  "clientTimeUs": 1719482837000000,
  "serverTimeUs": 1719482837291456
}
```

`serverTimeUs` is `esp_timer_get_time()` — same clock as `startedAtUs` from the play endpoint.

**Acceptance criteria:**
- [ ] `POST /sync` returns within <5ms (it's just a clock read)
- [ ] `serverTimeUs` is a real `esp_timer_get_time()` value
- [ ] `clientTimeUs` is echoed back unchanged
- [ ] Works without any USB device connected (it's just a clock endpoint)

## USB Audio + HID Coexistence

Both drivers share the ESP-IDF USB Host Library but operate independently:

- `usb_host_install()` — called once (currently in `UsbHidBus::init()`)
- HID client — registered by `UsbHidBus::init()`, claims HID interface
- UAC2 client — registered by `uac2_host_install()`, claims audio interface
- Both clients receive `NEW_DEV` events and filter for their own interfaces
- Both can be active simultaneously on the same composite device (confirmed working on real miniDSP 2x4 HD with the UAC2 driver test suite)

**The UAC2 driver manages its own USB Host client internally.** You do NOT need to manually register a client or handle USB events for audio — just call `uac2_host_install()` and it handles discovery, enumeration, and streaming.

## Supported Devices

USB audio capability is tracked in the shared device model via `DeviceProfile.has_usb_audio: bool` (`protocol/src/device/mod.rs`). This flag is `true` for all XMOS-based (modern) devices that have a UAC2 audio interface: 2x4 HD, DDRC-24, Flex, Flex DL, Flex Eight, Flex Eight DL, Flex HT, Flex HTx, SHD, SHD Studio, SHD Power. It is `false` for HID-only devices (2x4, 4x10 HD, 10x10 HD, OpenDRC series, miniSHARC).

**Capability gating — two layers:**
1. **Profile flag** (`has_usb_audio`): gates whether `/play` is even attempted. If `false`, return 400 with a clear error ("device does not support USB audio playback"). This check uses the already-probed `DeviceProfile` and doesn't require a UAC2 connection.
2. **Runtime handle** (`AudioState.device`): gates whether audio is currently available. A device may have `has_usb_audio = true` but the UAC2 interface might not have connected yet (or disconnected). If `device` is `None`, return 503.

The `POST /play` endpoint should check both: profile capability first, then runtime availability.

## Hardware Constraints

- **ESP32-S3 is Full Speed USB (12 Mbps)**. 48kHz/24-bit/stereo = 288 bytes per 1ms frame = 2.3 Mbps = 25% bus utilization. Fine.
- **DWC_OTG FIFO is 1024 bytes total.** With `PERIODIC_OUT` bias: PTX=600 (iso OUT), RX=128 (iso IN feedback), NPTX=64. Audio OUT + feedback IN fits. **Cannot do simultaneous TX+RX** (capture packets are ~294 bytes, exceed 128-byte RX FIFO). Playback-only is fine.
- **ASRC lock:** The miniDSP's ASRC needs ~2 seconds to lock on a new USB audio source. The 2-second preamble covers this.
- **Memory:** UAC2 driver uses ~40-50 KB internal SRAM (ring buffers + URBs). ESP32-S3 has ~200-280 KB free after WiFi. Fits.
- **Feedback:** The miniDSP sends 4-byte 16.16 feedback at 48kHz = 0x00300000 (48.0000 samples/frame). The UAC2 driver handles feedback-based adaptive packet sizing internally.

## Signal Design Rationale

**Why a tone burst before the sweep:**
- Enables sub-sample timing alignment via cross-correlation
- 80 Hz is within the sub's passband
- 3 cycles (37.5ms) gives a focused correlation peak
- Room reflections arrive after direct sound — caller picks the first peak

**Why NOT a chirp/click timing marker:**
- Subs roll off above ~200 Hz — a broadband marker gets filtered
- 80 Hz tone burst passes through cleanly

**Why 2-second preamble:**
- miniDSP ASRC lock time (~2s for USB audio source)
- Without this, the first seconds of audio are garbled

## Testing

### With the simulator (no real hardware needed)
Two ESP32-S3 boards connected via USB OTG. Board 1 runs the minidsp-2x4hd simulator (`~/code/esp-uac2-host/simulators/minidsp-2x4hd/`). Board 2 runs the minidsp-open firmware.

The simulator accepts UAC2 audio and counts frames — it confirms the host is sending valid audio at the correct rate. It also provides HID for device identity. This is the primary development environment.

### With real miniDSP 2x4 HD
Connect the ESP32 directly to the miniDSP via USB OTG. The miniDSP outputs audio on its analog outputs — audible verification.

### Desktop unit tests
The sweep generator (`sweep_gen.rs`) should be testable via `cargo test` (no ESP32 needed) — pure math.

## Existing Code Patterns to Follow

- **USB FFI pattern**: `app-esp32/src/usb_hid.rs` — raw `esp_idf_sys` FFI with C callbacks, `Condvar`-based sync, `AtomicBool` for connection state. Follow this style for the UAC2 FFI.
- **HTTP handler pattern**: `app-esp32/src/http_server.rs` — `EspHttpServer::fn_handler()` closures, `SharedDeviceState` for shared state, `get_device()` helper. Register new routes in the `start()` function.
- **Error responses**: existing handlers use `req.into_response(status, ...)` with JSON error bodies.
- **Thread spawning**: `std::thread::Builder::new().name(...).stack_size(...).spawn(...)` — see TCP bridge and UDP discovery in `main.rs`.
- **Module structure**: each module in `app-esp32/src/` is declared in `main.rs` with `mod module_name;`.

## File Summary

| File | Action | Purpose |
|------|--------|---------|
| `app-esp32/src/usb_audio.rs` | **Create** | UAC2 driver FFI wrapper, `AudioState`/`SharedAudioState` types, lifecycle management |
| `app-esp32/src/sweep_gen.rs` | **Create** | Sweep/tone burst/preamble generator (pure math, iterator-based, zero-crossing truncation) |
| `app-esp32/src/main.rs` | **Modify** | Add `mod usb_audio; mod sweep_gen;`, create `SharedAudioState`, install UAC2 driver after HID init, pass audio state to HTTP server |
| `app-esp32/src/http_server.rs` | **Modify** | Accept `SharedAudioState` param, add `POST /devices/{id}/play` and `POST /sync` routes, source auto-switch logic |
| `app-esp32/Cargo.toml` | **Modify** | Add esp-uac2-host component reference (if needed for build integration) |
| `app-esp32/build.rs` | **Modify** | Add component path or bindgen config for UAC2 headers (if needed) |
| `app-esp32/sdkconfig.defaults` | **Modify** | Add `CONFIG_USB_HOST_HW_BUFFER_BIAS_PERIODICOUT=y` |
| `protocol/src/device/mod.rs` | **Modify** | Add `has_usb_audio: bool` to `DeviceProfile` |
| `protocol/src/device/probe.rs` | **Modify** | Set `has_usb_audio: true` for XMOS devices (2x4 HD, DDRC-24, Flex/DL, Flex Eight/DL, Flex HT/HTx, SHD/Studio/Power), `false` for all others |
| `docs/ref-compatibility.md` | **Modify** | Document `POST /devices/{id}/play` and `POST /sync` as additive endpoints |

## Key References

- **UAC2 driver API**: `~/code/esp-uac2-host/components/uac2_host/include/usb/uac2_host.h` (the header you'll be calling)
- **UAC2 driver LLM reference**: `~/code/esp-uac2-host/llms.txt` (complete API docs, design notes, hardware constraints)
- **UAC2 driver implementation**: `~/code/esp-uac2-host/components/uac2_host/uac2_host.c` (~2400 lines)
- **UAC2 test app** (C usage example): `~/code/esp-uac2-host/main/main.c` — shows install, callback, open, start, write, stop
- **Existing HID FFI** (Rust pattern to follow): `app-esp32/src/usb_hid.rs`
- **Open-sub-optimizer sweep reference**: `~/code/open-sub-optimizer/src/sweep-gen.ts` (TypeScript implementation of the same Farina ESS)
