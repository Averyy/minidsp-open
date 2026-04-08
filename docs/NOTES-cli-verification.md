# CLI Verification Notes

Date: 2026-04-08

## Definitions

- `verified working`: one command was changed in isolation, the affected state was read back from the live device or daemon, then restored and read back again.
- `partially verified`: the write path executed and some related state changed, but the device does not expose authoritative readback for the exact field.
- `not verified`: the command ran, but the intended end state could not be proven from current readback.

## Test setup

- Device under test: miniDSP 2x4 HD (`hw_id=10`, `dsp_version=100`)
- Transport used for most live checks: direct USB HID with explicit `--usb`
- Readback paths:
  - `minidsp status`
  - `minidsp debug dump-word`
  - `minidsp debug dump-float`
  - desktop daemon `GET /devices/0/config` where that endpoint exposes the field cleanly

## Verified working

- `probe`
- `status`
- `debug id`
- `debug send`
- `debug dump-word`
- `debug dump-float`
- master `gain`
- master `mute`
- master `source`
- master `config`
- input mute
- output mute
- output invert
- output crossover coefficient writes
- output FIR unsupported-path behavior (`output channel does not have FIR filter`)

## Partially verified

- input gain
- output gain
- routing gain

These changed and restored, but the live readback is quantized on this device. Example: `-3 dB` read back at about `-3.059 dB`.

## Manual or audio verification still required

- routing enable / disable (`--off`)
- PEQ bypass
- PEQ clear bypass state
- crossover bypass

Reason: the current DSP100 runtime readback is not authoritative for these paths. Mixer status readback is boolean-collapsed for addresses `6..13`, so enabled and disabled both appear as non-zero. PEQ bypass state is currently tracked in client shadow state rather than reliable direct device readback.

## Not verified cleanly / likely bugs

- output delay
  - Setting `0.5 ms` read back as about `79.833336 ms` via daemon `/config`
- output compressor parameter writes
  - threshold changed in readback
  - ratio / attack / release did not show the expected changes in the live readback used during testing
- direct PEQ coefficient verification
  - raw direct reads at the PEQ addresses returned zeros during the test run, so the write could not be proven from that path alone

## minidsp-rs comparison

- Intended compatibility surface: CLI/API behavior should be a drop-in replacement for minidsp-rs where documented.
- Confirmed alignment in this repo:
  - routing uses opcode `0x13` with values `1/2`, matching minidsp-rs semantics
  - modern mute uses `1=muted`, `2=unmuted`, matching minidsp-rs semantics
- Different from minidsp-rs for a documented hardware/tooling reason:
  - DSP100 routing runtime readback is boolean-collapsed on this device, so this repo uses `status != 0` for readback instead of expecting an exact `2` for enabled
  - PEQ bypass/coefficient readback currently falls back to client shadow state when direct live readback is unavailable or returns unusable zeros on the tested path
- Not yet proven equivalent by live readback in this test pass:
  - routing enable/disable state readback
  - PEQ bypass state
  - PEQ coefficient readback on the tested direct-read path
- Clearly suspect in current implementation, with no good compatibility reason identified yet:
  - output delay
  - compressor parameter writes beyond threshold

## Investigation focus

Priority targets where current behavior appears to drift from minidsp-rs without a clear reason:

1. output delay
   - current desktop CLI path converts `ms -> samples`, then writes raw `u32` samples on float32 devices
   - live readback on the 2x4 HD was wrong by a large factor (`0.5 ms` came back as about `79.833336 ms`)
   - likely causes to check:
     - wrong unit or scaling for DSP100 delay registers
     - wrong address selection from the device table
     - wrong write opcode or save/runtime behavior for this field

2. compressor parameter writes
   - current CLI path writes threshold / ratio / attack / release as plain DSP floats
   - live readback only showed threshold changing in the test pass
   - likely causes to check:
     - ratio / attack / release use different encoding than threshold on DSP100
     - one or more compressor addresses in the extracted table are wrong for this device
     - direct readback path for these fields is not reading the authoritative runtime/config values

Items that are different but currently have a defensible reason:

- routing readback on DSP100
  - write semantics are aligned to minidsp-rs
  - readback is limited by hardware behavior on this device, not by a deliberate repo-only semantic change

- PEQ bypass / coefficient readback
  - current shadow-state fallback is weaker than authoritative hardware readback
  - this should still be improved, but it is currently a tooling/readback limitation rather than a proven command-write mismatch

## Current recommendation

Before doing another broad CLI validation pass:

1. Fix output delay write/read behavior.
2. Fix compressor parameter write behavior.
3. Improve authoritative readback coverage for PEQ and routing state, or treat those paths as audio-verified only.
