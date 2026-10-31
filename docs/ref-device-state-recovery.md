# Device State Recovery

This document describes the stable first-pass recovery surfaces in
`minidsp-open`.

It covers two related but distinct capabilities:

- snapshot import/export for controller-side backup and restore
- reset-to-defaults for rebuilding presets from the bundled official defaults

## Snapshot Import / Export

The snapshot feature is a controller-side backup/restore mechanism. It is not a
claim that MiniDSP hardware can natively export complete authoritative preset
files.

### Scope

Current first-pass scope:

- desktop CLI `snapshot export` / `snapshot import`
- desktop daemon HTTP snapshot export / import / learn
- ESP32 HTTP snapshot export / import / learn
- strict per-preset completeness / provenance reporting
- exact-token restore where supported
- FIR coefficient capture / replay for the verified 2x4 HD path

Current proven paths:

- raw desktop USB CLI round-trip
- desktop daemon-backed destructive round-trip
- ESP32 API-only round-trip backed by host-side verification through the TCP bridge

### Artifact Contract

The shared artifact is `DeviceSnapshot` in:

- [minidsp/src/snapshot.rs](../minidsp/src/snapshot.rs#L13)
- [app-esp32/src/snapshot.rs](../app-esp32/src/snapshot.rs#L14)

The current schema version is `formatVersion = 1`.

Top-level fields:

- `formatVersion`
- `capturedAt`
- `tool`
- `device`
- `transport`
- `master`
- `presets`
- `completeness`
- `notes`

Each preset snapshot records:

- `slot`
- `token`
- `complete`
- `provenance`
- `completeness`
- `shadow`
- `firBlocks`

Strict export must only succeed when the captured snapshot is complete for the
supported surface of that device/profile.

### Truth Rules

The snapshot system uses the strongest truthful source available for each
preset:

- trusted controller shadow state
- token-matched official Device Console `setting*.xml` cache, when available
- bundled default preset image for recovery-token presets
- live readback only where the repo can currently treat it as authoritative

Important behavior:

- strict export refuses incomplete presets
- restore replays the saved preset/master state through our own control paths
- restore preserves the exact preset token when the snapshot carries one
- recovery-token presets may be exported from the bundled default preset image
- trust in a stored preset survives preset switches, reloads, resets, reboots
  and reconnects, which leave stored presets unchanged; a failed or
  unacknowledged write, a preset copy, a raw flash write outside the
  modification tokens, or a restore that fails part way revokes it
- learn keeps a capture it can't trust (mute and routing reads on the 2x4 HD
  and Flex) as an untrusted copy, so it no longer replaces a trusted one and the
  default-image path still applies
- on the ESP32, a restore that loses the MiniDSP to a stalled HID write resumes
  once from the preset that failed, after the USB bus reset brings it back (see
  [the stall bug](bug-usb-hid-write-stall.md))

### Surfaces

Desktop CLI:

- `minidsp snapshot export <file>`
- `minidsp snapshot import <file>`

Desktop daemon HTTP:

- `GET /devices/{id}/snapshot`
- `POST /devices/{id}/snapshot`
- `POST /devices/{id}/snapshot/learn`

ESP32 HTTP:

- `GET /devices/0/snapshot`
- `POST /devices/0/snapshot`
- `POST /devices/0/snapshot/learn`

### Validation

The destructive validation workflow is documented in
[docs/ref-live-validation.md](ref-live-validation.md#L1).

Current live sign-off:

- attached MiniDSP 2x4 HD `912345`
- raw desktop USB CLI round-trip
- desktop daemon destructive restore
- ESP32 API-only round-trip
- final direct-daemon confirmation after the reset-defaults work, including
  strict export, `snapshot learn`, destructive restore, and exact baseline
  restoration

### Current Limits

These are real future follow-up items, not first-pass blockers:

- broader device-family rollout beyond the verified 2x4 HD path
- Dirac-family truthfulness review and wider live verification
- richer failure-path coverage and any future forced-import policy
- optional UX additions like `snapshot inspect` or official-file export compatibility
- any future controller-visible settings we decide belong in the snapshot schema

## Reset To Defaults

The goal of reset-to-defaults is to provide first-class preset reset/recovery
flows through our own stack instead of depending on the official MiniDSP Device
Console for normal recovery work.

### Source Of Truth

The checked-in modern SHARC reset corpus lives under:

- [protocol/assets/audio_firmware](../protocol/assets/audio_firmware/README.md#L1)

Build-time behavior:

- `protocol/build.rs` reads
  `protocol/assets/audio_firmware/{dsp}/DSP_default_parameters.xml`
- it generates compact preset-value and bypass blobs into `OUT_DIR`
- desktop and ESP32 binaries embed those generated blobs

Runtime behavior:

- the desktop binary does not load XML from disk
- the ESP32 firmware does not load XML from disk
- both targets consume the same embedded `DefaultPresetImage` assets from
  `minidsp-protocol`

### Supported Modern Families

The shared reset-image generator currently covers every checked-in modern SHARC
family in the protocol-owned XML corpus:

- DSP `100`
- DSP `101`
- DSP `105`
- DSP `110`
- DSP `111`
- DSP `113`
- DSP `115`

The implementation includes the XML-defined Dirac default block for the
Dirac-capable families.

### Implemented Operations

Current user-visible operations:

- reset the active preset to defaults
- reset a specific preset slot to defaults
- initialize all presets to defaults
- manual reboot

Read-only diagnostics used by the recovery flow:

- modification tokens at `0xFFC8`
- verify-program status at `0xFFA3`

Current HTTP surfaces:

- `POST /devices/{id}/preset/reset`
- `POST /devices/{id}/preset/{preset}/reset`
- `POST /devices/{id}/preset/reset-all`
- `POST /devices/{id}/reboot`
- `GET /devices/{id}/tokens`
- `GET /devices/{id}/verify-program`

ESP32 uses the equivalent `/devices/0/...` routes.

### Device-Side Flow

The shared recovery flow matches the verified device-side sequence:

1. switch to the target preset without reset when needed
2. write preset-values flash block
3. write preset-bypass flash block
4. verify the post-write status byte at `0xFFA3`
5. write/update the preset modification token
6. reload the same preset slot

The initialize-all recovery flow:

1. switches to preset `1`
2. rebuilds preset `1` from defaults
3. writes the recovery token for preset `1`
4. reloads preset `1`
5. copies preset `1` to the remaining slots
6. overwrites all preset tokens with `0xFEEF8FCF`
7. reboots the device

### Validation

Current live sign-off is on the attached MiniDSP 2x4 HD `912345`.

Verified paths:

- ESP32 bridge path on the flashed board
- direct desktop daemon path on the same physical 2x4 HD
- stock macOS Device Console reset/save flow through the ESP32 TCP bridge

The desktop direct-daemon path has been re-proven for:

- `POST /devices/0/reboot`
- `POST /devices/0/preset/reset-all`
- `POST /devices/0/preset/reset`
- `POST /devices/0/preset/1/reset`

with a final destructive pass restoring the exact pre-test baseline snapshot.

### Current Limits

These are real future follow-up items, not blockers for the completed
first-pass scope:

- live bench verification still exists only for the attached DSP `100` / 2x4 HD path
- host-side official Device Console preset-file maintenance parity is still not implemented
- legacy families from `third_party/legacy-configs/` use a separate source corpus and are out of scope for this modern XML-based reset path
