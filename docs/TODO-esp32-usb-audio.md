# ESP32 USB Audio Output to miniDSP

## Goal

Add a `POST /devices/{id}/play` HTTP endpoint to minidsp-open that generates a measurement sweep and plays it through the miniDSP's USB audio input. This enables cable-free sub measurement: the caller sends a command over the network, the ESP32 generates the sweep mathematically, outputs audio to the miniDSP, and returns the exact playback start timestamp so the caller can align their recording.

This endpoint is useful for:
- **open-sub-optimizer** — automated measurement (calls play, captures via UMIK, deconvolves, optimizes)
- **REW users** — anyone with minidsp-open can trigger a sweep wirelessly while REW captures. No need for open-sub-optimizer at all.
- **Any HTTP client** — `curl -X POST .../play` works. The endpoint is a general-purpose "play a sweep through the miniDSP" feature.

## The Sweep Signal

### Mathematical Generation (No File Transfer)

The sweep is a Farina exponential sine sweep — a mathematical function. **Generate it sample-by-sample on the ESP32 in real-time.** No file transfer, no storage, no compression. Perfect quality.

```
x(t) = sin(2π × f1 × T / ln(f2/f1) × (exp(t × ln(f2/f1) / T) - 1))
```

Parameters (open-sub-optimizer defaults from `src/sweep-gen.ts`):
- `f1 = 5 Hz` (display start; actual sweep starts at f1/2 = 2.5 Hz for guard band)
- `f2 = 500 Hz` (display stop; actual sweep goes to min(2×f2, fs/2) = 1000 Hz)
- `duration = 8 seconds`
- `fs = 48000 Hz` (sample rate)
- Tukey fade-in: 0.02s

At 240 MHz with hardware FPU, `sin(exp(x))` takes ~50-100 cycles per sample. At 48 kHz stereo = 96K samples/sec × 100 cycles = ~10M cycles/sec = **4% CPU**. Trivially feasible.

### Signal Structure

```
[2s preamble/dither] [40ms tone burst @ 80Hz] [200ms gap] [8s sweep @ 5-500Hz] [2s decay]
Total: ~12.24 seconds
```

- **Preamble**: Low-level dither for DAC stabilization and miniDSP ASRC lock
- **Tone burst**: 3 cycles at 80 Hz (timing marker — see Timing section)
- **Gap**: Short silence between burst and sweep
- **Sweep**: The actual measurement signal (Farina ESS)
- **Decay**: Captures room reverb tail

### API

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

**Response** (returned when playback actually begins):
```json
{
  "startedAtUs": 1719482837291456,
  "toneBurstOffsetSamples": 96000,
  "sweepOffsetSamples": 105600,
  "totalSamples": 588288,
  "sampleRate": 48000
}
```

The response includes:
- `startedAtUs` — ESP32 hardware timer value (microsecond precision) when the first audio sample was output. Callers use this + clock sync to know when playback started in their own timeframe.
- `toneBurstOffsetSamples` — sample index where the tone burst begins in the generated signal. Callers use this for timing alignment (see Timing section).
- `sweepOffsetSamples` — sample index where the sweep begins. Callers deconvolve starting here.
- `totalSamples` — total signal length.

Signal goes to both stereo channels identically. The miniDSP's input routing matrix routes input 1 to all outputs. Individual output muting (via the existing HID mute commands) isolates each sub.

### Clock Sync Endpoint

For callers that need timing alignment (see Timing section), minidsp-open exposes a clock sync endpoint:

```
POST /sync
Content-Type: application/json

{ "clientTimeUs": 1719482837000000 }

Response:
{ "clientTimeUs": 1719482837000000, "serverTimeUs": 1719482837291456 }
```

The caller sends multiple sync requests (e.g., 50-100), measures round-trip times, takes the minimum RTT, estimates one-way latency as min_RTT/2, and builds a clock offset model. Research shows ~200μs median accuracy over WiFi with this approach (Lectrobox ESP32 NTP study).

## Timing Synchronization

### The Problem

When the ESP32 plays audio through the miniDSP and a laptop captures via USB mic, the clocks are different. The caller needs to know when the sweep started relative to their recording, or inter-sub phase alignment will be wrong.

### The Solution: Three-Layer Alignment (No Extra Hardware)

No timing speaker. No chirp. No extra hardware. The sub itself provides the timing reference.

**Layer 1: Coarse clock sync (~200μs)**

Before measurement, the caller hits `POST /sync` 50-100 times and builds a clock offset model. This gets the two clocks within ~200μs of each other. The caller now knows roughly when to expect audio in their recording.

**Layer 2: ESP32 start confirmation (eliminates network command jitter)**

The `POST /play` response includes `startedAtUs` — the exact ESP32 hardware timer value when audio output began. The caller converts this to their own clock using the sync model. This tells them the sweep's position in the recording within ~200μs (the clock sync accuracy).

**Layer 3: Tone burst + deconvolution (sub-sample precision)**

The generated signal includes a **tone burst** (3 cycles at 80 Hz, ~40ms) before the sweep. This burst goes through the same sub being measured. The caller:

1. Knows roughly where the burst should appear (from layers 1+2, ±200μs = ±10 samples at 48kHz)
2. Cross-correlates the recording against the known burst template in that narrow window
3. Finds the exact sample where the burst arrives — direct sound is always the first arrival
4. The sweep starts at a known sample offset after the burst (same stream, sample-locked)
5. Deconvolves the sweep portion → transfer function H(f) with correct phase

**Why the tone burst works through the sub:**
- 80 Hz is well within the sub's passband
- 3 cycles = 37.5ms, giving a focused cross-correlation peak
- Room reflections arrive AFTER the direct sound — and we know where to look (±200μs window from clock sync), so we pick the first peak
- The burst and sweep travel the same acoustic path (same sub) — playback delay cancels completely

**Why inter-sub phase is correct:**
- Sub 1: align to sub 1's tone burst arrival → deconvolve → H₁(f) referenced to sub 1's acoustic arrival
- Sub 2: align to sub 2's tone burst arrival → deconvolve → H₂(f) referenced to sub 2's acoustic arrival
- Each measurement is self-referenced — no common timing speaker needed
- The optimizer computes relative delays between subs, which are correct because each H(f) is anchored to its own sub's direct sound

**Additional validation:** The sweep itself has a known, exact frequency-time relationship: `f(t) = f1 × exp(t × ln(f2/f1) / T)`. The deconvolution is a matched filter that exploits this structure. Room modes affect amplitude but not the frequency trajectory. The deconvolution peak provides sub-sample alignment as a final confirmation that the tone burst timing was correct.

**Accuracy summary:**

| Layer | What it does | Accuracy |
|-------|-------------|----------|
| Clock sync (`POST /sync`) | Tells caller roughly when to expect audio | ~200μs |
| Start confirmation (`startedAtUs`) | Eliminates network command jitter | ~200μs (clock-sync limited) |
| Tone burst cross-correlation | Finds exact onset from the sub | ~20-40μs (1-2 samples) |
| Deconvolution (matched filter) | Full sweep alignment + validation | Sub-sample (~2μs) |

At 200 Hz: 200μs = 14° phase error (from clock sync alone). With tone burst refinement: <1° phase error. With deconvolution: negligible.

### For REW Users

REW users calling this endpoint don't need to implement the clock sync or tone burst detection. REW has its own "acoustic timing reference" feature. They can either:
- Use REW's built-in timing detection on the tone burst (REW recognizes timing markers)
- Or just use REW's standard measurement mode — REW handles timing internally when it controls the capture

The endpoint plays the sweep, REW captures, REW aligns. The `startedAtUs` and sample offsets in the response are available for callers that want them, but not required.

## USB Audio Implementation

### Status: READY

**The UAC2 driver is built and working.** `~/code/esp-uac2-host` provides a complete USB Audio Class 2.0 host driver for ESP32-S3, tested end-to-end with a miniDSP 2x4 HD simulator. The driver handles:
- UAC2 descriptor parsing (clock source/selector, terminals, feature units)
- Control requests (SET_CUR, GET_CUR, GET_RANGE) for sample rate, volume, mute
- SET_INTERFACE for isochronous endpoint activation/deactivation
- Isochronous OUT streaming with feedback-based adaptive packet sizing
- Ring buffer for PCM data flow
- All verified with 487,000+ audio frames delivered, zero errors

**UAC1 vs UAC2 answer:** The miniDSP presents UAC2 descriptors (`bInterfaceProtocol = 0x20`) at Full Speed. Espressif's UAC1 driver cannot be used. The custom UAC2 driver is required and is complete.

### What We're Working With

The miniDSP accepts audio via USB — that's a core product feature. The ESP32-S3 is already connected via USB for HID control. The miniDSP is a composite USB device with both HID and Audio interfaces on the same connection.

**Bandwidth:** 48 kHz × 2ch × 24-bit = 288 KB/s = 2.3 Mbps. USB Full Speed supports 12 Mbps. Each 1ms isochronous frame needs 288 bytes vs the 1023-byte maximum. Plenty of room.

### Implementation Plan

**Step 1: Audio streaming via `esp-uac2-host`**

Open the miniDSP's audio interface alongside the existing HID interface. Use the UAC2 driver API:
```c
#include "uac2_host.h"

// Open the UAC2 device after USB enumeration
uac2_host_device_handle_t audio_dev;
uac2_host_device_open(client, usb_dev, event_cb, event_cb_arg, &audio_dev);

// Start streaming: 48kHz, 24-bit, stereo
uac2_stream_config_t cfg = {
    .sample_rate = 48000,
    .bit_resolution = 24,
    .channels = 2,
};
uac2_host_stream_start(audio_dev, UAC2_STREAM_TX, &cfg);

// Write PCM data (blocks until ringbuf has space, or returns ESP_ERR_TIMEOUT)
uac2_host_stream_write(audio_dev, pcm_data, num_bytes, timeout_ms);

// Get hardware timestamp of first audio frame output (microseconds)
int64_t start_us = uac2_host_stream_get_start_time(audio_dev);

// Stop and clean up
uac2_host_stream_stop(audio_dev, UAC2_STREAM_TX);
uac2_host_device_close(audio_dev);
```

**Step 2: Sweep generation**

Mathematical real-time generation (see The Sweep Signal section above). No file transfer.

**Step 3: HTTP endpoint**

Wire `POST /play` handler → sweep generator → USB audio output:
```
POST /play request
  → parse parameters
  → open USB audio interface (alongside existing HID)
  → set sample rate, bit depth, channel count
  → note hardware timer value → startedAtUs
  → generate samples in real-time:
      preamble → dither
      tone burst → 3 cycles of sin(2π × 80 × t)
      gap → silence
      sweep → Farina ESS
      decay → silence
  → pack into PCM frames, submit isochronous OUT transfers
  → respond with startedAtUs + sample offsets
```

### Supported Devices

Only XMOS-based (modern) miniDSP devices have USB audio.

| Device | USB Audio | Supported |
|--------|-----------|-----------|
| 2x4 HD, DDRC-24 | Yes (XMOS XU216, UAC2) | Yes |
| Flex, Flex DL | Yes (XMOS, UAC2) | Yes |
| Flex Eight, Flex HT/HTx | Yes (XMOS, UAC2) | Yes |
| SHD, SHD Studio | Yes (XMOS, UAC2) | Yes |
| 2x4 (non-HD), 4x10 HD, 10x10 HD | No (HID only) | No |
| OpenDRC series, miniSHARC | No (HID only) | No |

## What the Caller Needs to Do

### open-sub-optimizer (automated measurement)

The measurement flow in open-sub-optimizer for the minidsp-open network path:

1. **Sync clocks**: `POST /sync` × 50-100 times → build clock offset model
2. **For each sub at each position**:
   a. Mute all except target sub: `POST /devices/0/config/output/{ch}/mute`
   b. Start mic capture (laptop UMIK, ~0.5s before expected playback)
   c. Play sweep: `POST /devices/0/play` → get `startedAtUs`, offsets
   d. Wait for capture to complete
   e. Find tone burst in recording (cross-correlate in ±200μs window)
   f. Deconvolve sweep portion → H(f)
3. **Unmute all**: restore muting state

open-sub-optimizer needs:
- A `MinidspOpenClient` class that wraps the clock sync + play + mute APIs
- Tone burst cross-correlation (similar to existing `findChirpArrival()` but with a tone burst template)
- Modified orchestrator flow for the network measurement path

### REW users (manual measurement)

1. Set up REW for measurement capture (UMIK input, acoustic timing reference enabled)
2. Trigger sweep: `curl -X POST http://minidsp.local:5380/devices/0/play -H 'Content-Type: application/json' -d '{"sweep":{"f1":5,"f2":500,"duration":8}}'`
3. REW captures and processes using its own timing alignment
4. Export measurements, import into MSO or open-sub-optimizer

No special software needed beyond minidsp-open and REW.

## Files to Create/Modify

| File | Purpose |
|------|---------|
| `src/usb_audio.rs` (or `.c`) | USB audio host — enumerate audio interface, isochronous OUT |
| `src/sweep_gen.rs` | Mathematical sweep + tone burst generation |
| `src/http/play.rs` | `POST /devices/{id}/play` handler |
| `src/http/sync.rs` | `POST /sync` clock sync handler |
| `src/http/mod.rs` | Register new routes |

### Dependencies

- ESP-IDF USB Host stack (already used for HID)
- **`esp-uac2-host`** (`~/code/esp-uac2-host`) — custom UAC2 host driver (ESP-IDF component, C). Add as a local component or git submodule. MIT licensed.
- No additional crates for sweep generation (`libm::sinf`, `libm::expf`)

## UAC2 Driver: `esp-uac2-host`

**Status: COMPLETE.** The driver is at `~/code/esp-uac2-host` and provides:
- UAC2 descriptor parsing (clock source/selector/multiplier entities, terminals, feature units)
- UAC2 control requests (CUR/RANGE for sample rate, clock validity, volume, mute)
- SET_INTERFACE for proper endpoint activation/deactivation
- Isochronous OUT with feedback-based adaptive packet sizing (10.14 and 16.16 format)
- Isochronous IN (capture, limited by ESP32-S3 FIFO — not needed for sweeps)
- Ring buffer with configurable pre-fill and threshold-based events
- Hardware timer start timestamp (`uac2_host_stream_get_start_time`)
- Transfer error limiting (auto-stops after 10 consecutive errors)
- All 5 automated tests pass (48kHz, volume/mute, stop/restart, 44.1kHz, long-running stability)
- Reviewed by 6-agent code review, all findings fixed

### Using as ESP-IDF Component

Add to your project's `CMakeLists.txt`:
```cmake
set(EXTRA_COMPONENT_DIRS "${CMAKE_CURRENT_SOURCE_DIR}/../esp-uac2-host/components/uac2_host")
```

Or add as a git submodule:
```bash
git submodule add ../esp-uac2-host components/uac2_host
```

### Kconfig Required

In `sdkconfig.defaults`:
```
CONFIG_USB_HOST_CONTROL_TRANSFER_MAX_SIZE=512  # miniDSP config descriptor is 373 bytes
CONFIG_USB_HOST_HW_BUFFER_BIAS_PERIODICOUT=y   # Bias DWC_OTG FIFO for isochronous OUT
```

## miniDSP 2x4 HD Simulator

A full TinyUSB-based simulator is available at `~/code/esp-uac2-host/simulators/minidsp-2x4hd/` for development and testing without real hardware. It runs on a second ESP32-S3 dev board.

### What It Simulates

- **UAC2 audio**: Playback IF1 (alt 1 = 24-bit, alt 2 = 16-bit), feedback endpoint (10.14 format)
- **HID control**: All 20 commands (ReadHardwareId, ReadFlash, ReadFloats, WriteDSP, WriteBiquad, SetVolume, SetMute, SetConfig, SetSource, BypassFilter, DiracBypass, etc.)
- **EEPROM state**: Preset, source, volume, mute, serial, mod tokens, DSP ID
- **DSP parameter state**: WriteDSP/WriteBiquad store values, ReadFloats reads them back
- **Fault injection**: Config switch delay, empty HID response, HID NAK, command timeout
- **Exact USB identity**: VID=0x2752, PID=0x0011, bcdDevice=0x06F2 (matches real miniDSP)

### What It Does NOT Simulate

- ASRC lock behavior (the 2-second preamble requirement — real miniDSP needs this)
- Capture direction (ESP32-S3 FIFO limitation, not needed for sweeps)
- DFU interface
- Real analog/TOSLINK input switching
- Actual audio processing (samples are received and counted, not processed)

### Hardware Setup

Two ESP32-S3-DevKitC-1 boards connected via their USB OTG ports (left-side USB-C):
- **Board 1 (simulator)**: Runs `simulators/minidsp-2x4hd/` firmware. UART serial on right-side USB-C.
- **Board 2 (host)**: Runs your application (or the test firmware). UART serial on right-side USB-C.
- **VBUS**: Simulator board is self-powered, but VBUS must be present on the OTG cable for enumeration. Use a powered USB hub or provide 5V externally.

### Building and Flashing

```bash
# Activate ESP-IDF
export PATH="/opt/homebrew/bin:$PATH" && . ~/esp/esp-idf/export.sh

# Build and flash the simulator
cd ~/code/esp-uac2-host/simulators/minidsp-2x4hd
idf.py build
idf.py -p /dev/cu.usbmodem<SIMULATOR_SERIAL> flash

# Build and flash the host
cd ~/code/esp-uac2-host  # or your project
idf.py build
idf.py -p /dev/cu.usbmodem<HOST_SERIAL> flash
```

### Serial Monitoring via serial-mcp

Claude Code has direct serial access to both boards via `serial-mcp` tools. Use these instead of copy-pasting terminal output:

```
# List ports — look for VID 0x1A86 (CH340 UART bridges)
list_serial_ports

# Open both boards
serial_open port=/dev/cu.usbmodem<SIM_SERIAL> baud_rate=115200
serial_open port=/dev/cu.usbmodem<HOST_SERIAL> baud_rate=115200

# Read simulator output
serial_read session_id=/dev/cu.usbmodem<SIM_SERIAL>

# Wait for specific output
serial_wait_for pattern="TEST.*PASS|abort" session_id=/dev/cu.usbmodem<HOST_SERIAL> timeout=30

# Simulator fault injection (send keypress)
serial_command data="1" session_id=/dev/cu.usbmodem<SIM_SERIAL>  # toggle config switch delay
serial_command data="s" session_id=/dev/cu.usbmodem<SIM_SERIAL>  # show simulator state
serial_command data="h" session_id=/dev/cu.usbmodem<SIM_SERIAL>  # fault injection help
```

### Simulator Fault Injection Keys

| Key | Effect |
|-----|--------|
| `1` | Toggle config switch delay (4.5s audio NAK after SetConfig) |
| `2` | Toggle empty HID response |
| `3` | NAK next 3 HID writes |
| `4` | Toggle command timeout (withhold HID response) |
| `s` | Show simulator state (preset, volume, frame count, faults) |
| `h` | Show help |

## Clock Sync: Implementation Notes

Use the **Snapcast protocol pattern**: custom NTP-like ping-pong over UDP, median filtering over 50+ samples. Snapcast ESP32 clients (multiple implementations exist, proven in production multi-room audio) achieve ~200μs accuracy on commodity WiFi hardware.

**Lectrobox NTP implementation** (`github.com/jelson/rulos`, path `src/lib/chip/esp32/periph/ntp`) achieved 216μs median with:
- WiFi promiscuous mode for low-jitter receive timestamps
- Least-squares linear regression over observations
- 200μs asymmetry correction constant (send path is slower than receive on ESP32)
- Top-quartile RTT outlier rejection

For our use case, a simpler version suffices: 50-100 UDP round-trips before measurement, min-RTT filtering, simple offset calculation. We only need accurate sync for the next ~15 seconds, not continuous long-term tracking.

**Clock drift is negligible for our sweep duration.** Gamper (Microsoft Research, 2017) proved drift is ~0.02 samples/second between two devices. For a 15s sweep: 0.3 samples total. Irrelevant.

## Resolved Questions

- **Does the miniDSP fall back to UAC1 at Full Speed?** NO. It presents UAC2 descriptors (`bInterfaceProtocol = 0x20`) at Full Speed. The custom UAC2 driver (`esp-uac2-host`) is required and is complete.
- **Can we open HID and Audio interfaces simultaneously?** YES, confirmed with the simulator. ESP-IDF supports claiming multiple interfaces on a composite device. The host driver opens the audio interface while HID is on a separate interface.
- **ASRC lock time:** miniDSP ASRC needs ~2 seconds to lock. The 2-second preamble in the sweep signal covers this. The simulator does not replicate ASRC behavior — verify with real hardware.

## Remaining Questions (Need Real Hardware)

- **Feedback format confirmation:** Expect 3 bytes / 10.14 from XMOS at Full Speed (based on XMOS source code). Simulator uses TinyUSB auto-conversion. Verify with real miniDSP.
- **Full 373-byte config descriptor:** We have 257 bytes captured + 116 bytes reconstructed. Verify complete descriptor with real device.

## References

### Code (for implementation)
- **USE THIS** — UAC2 host driver (complete): `~/code/esp-uac2-host` (MIT, ESP-IDF component)
- **Simulator** — miniDSP 2x4 HD simulator: `~/code/esp-uac2-host/simulators/minidsp-2x4hd/`
- **Reference** — Espressif UAC1 host driver (architecture reference): `github.com/espressif/esp-usb` (`host/class/uac/usb_host_uac`)
- **UAC2 protocol reference** — USBX (MIT): `github.com/eclipse-threadx/usbx` (`ux_class_audio20.h`)
- **Clock sync reference** — Lectrobox ESP32 NTP: `github.com/jelson/rulos` (`src/lib/chip/esp32/periph/ntp`)
- **Proves ~200μs WiFi sync works** — ESP32 Snapcast client: `github.com/carlosderseher/snapclient`

### Documentation
- XMOS USB Audio Design Guide: `xmos.com/file/sw_usb_audio-design-guide`
- Espressif UAC2 request: `github.com/espressif/esp-usb/issues/173`
- Gamper (2017): clock drift negligible for sweeps <30s
- miniDSP device registry: `~/code/minidsp-open/docs/ref-devices.md`

### open-sub-optimizer (caller side)
- Sweep generation: `~/code/open-sub-optimizer/src/sweep-gen.ts`
- Cross-correlation: `~/code/open-sub-optimizer/src/cross-correlate.ts`
- Measurement paths: `~/code/open-sub-optimizer/docs/TODO-measurement-paths.md`
