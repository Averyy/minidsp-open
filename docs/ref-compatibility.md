# minidsp-open: Compatibility Contract

Canonical source of truth for all externally visible compatibility behavior.

If this file conflicts with `PROJECT.md`, `PROGRESS.md`, or any other planning doc, this file wins.

## Goal

minidsp-open's compatibility target is drop-in replacement for minidsp-rs on desktop and on the wire.

This file defines the contract the code is expected to satisfy. Verification status and release-gate testing live in:

- `PROGRESS.md` — current project status and open issues
- `docs/todo-checker.md` — master compatibility verification checklist

That means existing clients should continue to work without changes:
- ezBEQ
- Home Assistant integrations built for minidsp-rs
- MiniDSP Device Console via the TCP bridge / UDP discovery path
- CLI scripts written against the current `minidsp` binary

Rule: compatibility behavior is frozen. New features are additive and must use new paths, new flags, or new payload fields without changing compatibility semantics.

Scope note:
- this file defines the external contract once a device family is supported
- it does not imply that every device in MiniDSP's official apps is already fully implemented in this repo
- coverage progress is tracked in `PROGRESS.md`, with importer/device-matrix detail in `docs/ref-device-coverage.md`, `docs/ref-devices.md`, and `docs/ref-legacy-devices.md`
- when protocol details differ between `minidsp-rs` and MiniDSP's own extracted app assets, the official app assets are the source of truth

## Offline audit baseline

The source/host audit targets minidsp-rs `dev` at
[`8e987f9`](https://github.com/mrene/minidsp-rs/tree/8e987f9d16c580482eda1b52692b3238c18d7ab8)
and ezBEQ at
[`bd981bf`](https://github.com/3ll3d00d/ezbeq/tree/bd981bf778f76e5ffc7b0ecbfff94c82c4320321).
The [audit inventory](NOTES-compatibility-audit.md) records all 55 initially
unchecked checklist items, source citations, fixes, and remaining runtime gates.
Source and in-memory host verification do not certify Device Console operation,
LAN discovery reception, or additional physical device families.

## Surface Areas

Compatibility applies to six public surfaces:

1. CLI
2. HTTP API
3. WebSocket API
4. TCP bridge
5. UDP discovery
6. Unix socket (daemon IPC)

Internal architecture is not part of this contract. Queueing, locking, transport ownership, runtime choice, and handler reuse can change as long as the public behavior below remains compatible.

## Compatibility Rules

- Keep existing compatibility paths stable.
- Do not overload a compatibility endpoint with a new meaning.
- Accept `--force-kind` and other previously supported inputs even if better internal models exist.
- Do not reject biquad coefficients by default if minidsp-rs previously accepted them.
- Unknown TCP bridge commands must pass through unchanged when valid on the wire.
- Additive features must live on additive endpoints or flags.
- **Source name casing:** JSON serialization (serde) uses PascalCase variant names (`Toslink`, `Spdif`, `Usb`, `Aesebu`, `Hdmi`). CLI and URL path parameters use lowercase (`toslink`, `spdif`, `usb`) via strum `Display`/`FromStr`. These are two different serialization paths — do not normalize one to match the other.

## CLI Contract

Desktop `minidsp` must remain a drop-in replacement for minidsp-rs.

Upstream desktop behavior is hybrid:
- if the local daemon/service is running, the CLI uses it automatically
- otherwise the CLI can still operate directly

Desktop compatibility should preserve that behavior when feasible.

### Required commands and modes

| Command | Compatibility requirement |
|---------|---------------------------|
| `minidsp` | Print device status as text |
| `minidsp status` | Print device status as text |
| `minidsp -o jsonline` | Print device status in JSON form suitable for ezBEQ/state parsing |
| `minidsp -f <file>` | Execute commands sequentially in file order |
| `minidsp probe` | Enumerate connected devices |
| `minidsp server` | Compatibility-relevant command; upstream marks it deprecated in favor of `minidspd` packaging/service flow |
| `minidsp --force-kind <type>` | Backward-compatible override |
| `minidsp -V` | Print `minidsp <semver>`; the last token must parse as semver **greater than `0.1.12`** |

`-V` is load-bearing: ezBEQ ≥ 2.7.0 runs `minidsp -V`, parses the last token
with `semver`, and only targets Flex HTx **input** PEQs when it is above
`0.1.12` (the last official minidsp-rs release). A lower version silently
moves HTx BEQ filters to the outputs. The workspace version therefore stays
above `0.1.12`, and `app-desktop` has a test enforcing it.

### Required command families

The compatibility CLI must continue to support the existing command shapes used by scripts and ezBEQ, including:

- `input {ch} peq {n} set -- {b0} {b1} {b2} {a1} {a2}`
- `input {ch} peq {n} bypass {on|off}`
- `input {ch} mute {on|off}`
- `input {ch} gain -- {dB}`
- `output {ch} peq {n} set -- ...`
- `output {ch} peq {n} bypass {on|off}`
- `output {ch} mute {on|off}`
- `output {ch} gain -- {dB}`
- `config {n}`
- `gain -- {dB}`
- `mute {on|off}`
- `source {name}`

Additional accepted inputs:
- `--force-kind` accepts all upstream `DeviceKind` spellings, including `m2x4` and `generic`, plus the existing minidsp-open aliases.
- `--force-device` and `--force-dsp` remain accepted no-ops. They are absent from the audited upstream `dev` CLI; use `--force-kind` for a profile override.
- `--usb path=...` and percent-encoded probe URLs (`usb:%2Fpath?vid=...&pid=...`) select the decoded HID path; `--tcp host` defaults to port `5333`.
- `--all-local-devices` fans out only across local USB devices. Explicit remote transports are rejected. Two physical USB devices still need live verification.
- Local devices are indexed in serial order after successful probing, consistently for direct CLI discovery and initial daemon USB discovery. Runtime attachment of static/network devices appends new daemon indexes; do not assume indexes persist across topology changes. Use `device_serial` for TCP bindings that must identify one unit.
- Upstream `output N crossover GROUP INDEX set -- b0 b1 b2 a1 a2`, `bypass on|off`, and `clear` are accepted, including `all` targets. Existing whole-group `crossover set GROUP ...` remains accepted.
- Upstream compressor flags `-b/--bypass`, `-t/--threshold`, `-k/--ratio`, `-a/--attack`, `-r/--release` are accepted alongside the existing `compressor set` and `compressor bypass` commands.
- Upstream routing `input N routing M gain -- dB` and `enable on|off` are accepted alongside the existing numeric gain/`--off` form.
- PEQ `clear` writes the identity filter and **removes bypass**, matching upstream. ezBEQ's clear operation instead uses explicit `bypass on` and is unchanged.

### File execution semantics

`minidsp -f <file>` is compatibility-sensitive:
- `-f -` reads stdin; shell-style quoting is accepted
- file commands use the same parser and handlers as direct commands
- commands execute in the order given
- no internal reordering for compatibility mode
- this preserves ezBEQ batch behavior exactly

If minidsp-open adds higher-level batch APIs with safer ordering, those are additive and do not change `-f` execution semantics.

### CLI examples

#### `minidsp -o jsonline`

Compatibility requirement:
- emit the compact JSON serialization of upstream `StatusSummary`
- remain parseable by existing consumers such as ezBEQ

Representative upstream example:
```json
{"master":{"preset":0,"source":"Toslink","volume":-8.0,"mute":false},"input_levels":[-131.36298,-131.36298],"output_levels":[-140.21962,-139.85617,-120.0,-120.0]}
```

Notes:
- ezBEQ reads the first JSON line and expects `master.preset`, `master.mute`, `master.volume`, `input_levels`, and `output_levels`
- field names and exact nesting must match upstream `StatusSummary`

#### `minidsp probe`

Compatibility requirement:
- preserve the upstream text line format because existing tooling parses it

Representative upstream lines (URL varies by transport):
```text
0: Found 2x4HD with serial 912345 at usb:%2Fdev%2Fhidraw3?vid=2752&pid=0011 [hw_id: 10, dsp_version: 100]
0: Found 2x4HD with serial 912345 at ws://localhost:5380/devices/0/ws [hw_id: 10, dsp_version: 100]
```

The `at` URL reflects the transport that discovered the device (USB HID path, TCP address, or daemon WebSocket). Do not hardcode a single format.

#### `minidsp -f filters.txt`

Input file:
```text
input 0 peq 0 set -- 1.0003468 -1.9990215 0.9986773 -1.9990215 0.9990241
input 0 peq 0 bypass off
input 1 peq 0 set -- 1.0003468 -1.9990215 0.9986773 -1.9990215 0.9990241
input 1 peq 0 bypass off
```

Compatibility requirement:
- execute sequentially in file order
- do not reorder for internal batching in compatibility mode

## HTTP API Contract

This section defines the minidsp-rs-compatible daemon API surface.

### Required compatibility routes

| Path | Method | Returns | Compatibility requirement |
|------|--------|---------|---------------------------|
| `/devices` | `GET` | `Vec<Device>` JSON | List detected devices |
| `/devices/{id}` | `GET` | `StatusSummary` JSON | Device status (nulls omitted) |
| `/devices/{id}` | `POST` | `MasterStatus` JSON | Change master status (nulls included) |
| `/devices/{id}/config` | `POST` | Empty body (200) | Change any parameters, including master status |
| `/devices/{id}/volume/{direction}` | `POST` | `MasterStatus` JSON | Convenience route for `up`, `down`, `toggle_mute` |
| `/devices/{id}/source/{source}` | `POST` | `MasterStatus` JSON | Convenience route |
| `/devices/{id}/preset/{preset}` | `POST` | `MasterStatus` JSON | Convenience route |
| `/devices/{id}` | `WebSocket upgrade` | — | Compatibility WebSocket endpoint, with query params |
| `/devices/{id}/ws` | `WebSocket upgrade` | — | Raw frame bridge compatibility endpoint |

Compatibility note:
- `/devices/{id}` with WebSocket upgrade and query params is part of compatibility behavior.
- Upstream source shows `/devices/{id}/ws` as the raw frame bridge, not the level/state WebSocket.
- Do not reuse the compatibility WebSocket surface for a raw frame bridge or a new protocol.

### HTTP examples

#### `GET /devices`

Representative upstream response:
```json
[
  {
    "url": "usb:0001%3A0005%3A04?vid=2752&pid=0011",
    "version": {
      "hw_id": 10,
      "fw_major": 1,
      "fw_minor": 13,
      "dsp_version": 100,
      "serial": 912345
    },
    "product_name": "2x4HD"
  }
]
```

#### `GET /devices/0`

Representative response shape (from `StatusSummary`):
```json
{
  "master": {
    "preset": 1,
    "source": "Toslink",
    "volume": -8.0,
    "mute": false,
    "dirac": false
  },
  "input_levels": [
    -131.38994,
    -131.38994
  ],
  "output_levels": [
    -105.435165,
    -139.5934,
    -105.435165,
    -139.59183
  ]
}
```

Compatibility requirement:
- response must remain usable by existing minidsp-rs consumers
- additive fields are allowed only if they do not break tolerant parsers
- `GET` returns the model-level `StatusSummary` — null `master` fields are **omitted** (`skip_serializing_if`)
- `dirac` is present only on devices that support Dirac Live; omitted otherwise

#### `POST /devices/0`

Representative upstream request body:
```json
{
  "preset": 0,
  "source": "Toslink",
  "volume": -12.0,
  "mute": false
}
```

Compatibility requirement:
- `POST /devices/{id}` accepts a bare `MasterStatus` body
- live 2026-04-09 verification on the physical 2x4 HD confirmed this exact shape remains the tested compatible request body; wrapper bodies like `{"master":{...}}` are not part of the compatibility contract
- do not replace this compatibility shape with incompatible wrapper objects
- the convenience routes below are also part of compatibility and should continue to work
- all POST endpoints (`/devices/{id}`, `/volume/:direction`, `/source/:source`, `/preset/:preset`) return the **protocol-level** `MasterStatus` — null fields are **included** (e.g., `"dirac": null`), unlike the GET response which omits them

#### `POST /devices/0/volume/up`

Compatibility requirement:
- accept `up`, `down`, and `toggle_mute` in the route path and return updated status

#### `POST /devices/0/source/toslink`

Compatibility requirement:
- accept the source value in the route path and return updated status
- URL path uses **lowercase** source names (`toslink`, not `Toslink`) — parsed via strum `FromStr`, not serde

#### `POST /devices/0/preset/0`

Compatibility requirement:
- accept the preset value in the route path and return updated status

#### `POST /devices/0/config`

Representative request body shape:
```json
{
  "master_status": {
    "preset": 1
  },
  "inputs": [
    {
      "index": 0,
      "peq": [
        {
          "index": 0,
          "coeff": { "b0": 1, "b1": 0, "b2": 0, "a1": 0, "a2": 0 },
          "bypass": false
        },
        { "index": 1, "bypass": true }
      ]
    }
  ]
}
```

Implementation note:
- this structured config body is part of compatibility
- the convenience master-status routes above are also part of compatibility
- additive minidsp-open routes can still exist, but must not replace either compatibility style

Additional `/config` request compatibility:
- Routing accepts upstream `{"index":2,"mute":true}` and `{"index":2,"gain":-3.5}` entries. Omitted fields leave the other routing control unchanged. Existing `output` / `enabled` / `gain_db` fields remain accepted.
- Crossover accepts upstream `{"index":1,"coeff":[{"index":2,"b0":1,"b1":0,"b2":0,"a1":0,"a2":0}]}` groups. Existing nested `filters` / `coeff` objects remain accepted.
- Coefficient objects use upstream identity defaults for omitted numeric fields; malformed numeric values still fail deserialization.
- These request aliases are desktop changes. ESP32 verification recorded before this audit covers its existing request shapes; the new aliases have not been ported or hardware-verified there.

Error responses retain minidsp-open's existing `{"error":"message"}` envelope and HTTP status distinctions (`400`, `404`, `409`, `503` as applicable). Empty error responses from desktop status-only handlers now also have this JSON envelope. This deliberately differs from upstream `dev`'s HTTP `500` and `{"message":...,"error":{"type":...}}` errors. Upstream `/api` Redoc and `*.schema` metadata routes are not provided; minidsp-open exposes `/openapi.json` instead.

## WebSocket Contract

### Canonical compatibility endpoint

`/devices/{id}` with WebSocket upgrade and compatibility query params

Common compatibility query params:
- `poll=true`
- `levels=true`

Representative client usage from ezBEQ/ecosystem docs:
- `ws://{ip}/devices/{device_id}?levels=true&poll=true`

### Meaning

This endpoint is the minidsp-rs-compatible WebSocket used by existing clients for:
- live state updates
- level streaming

This endpoint is not the place for a new raw frame tunnel.

### Raw frame bridge

Upstream-compatible raw frame bridge endpoint:
- `/devices/{id}/ws`

This is distinct from the compatibility state/levels WebSocket on `/devices/{id}`.

### WebSocket examples

Compatibility requirement:
- the compatibility device WebSocket must remain the live state + level-streaming endpoint used by existing clients

Representative upstream-compatible message shapes:
```json
{"master":{"preset":0,"source":"Toslink","volume":-19.5,"mute":false},"input_levels":[-44.400307,-42.08899],"output_levels":[-54.93488,-56.41222,-120.0,-120.0]}
{"master":{"volume":-19.0,"mute":false}}
{"master":{},"input_levels":[-131.44533,-131.44533],"output_levels":[-140.41307,-140.02238,-120.0,-120.0]}
```

Implementation note:
- As upstream does, the presence of `poll` or `levels` enables that option, including `poll=false` or another value.
- Desktop sends complete snapshots rather than upstream's mixture of full state and deltas. Its polling intervals remain `500 ms` with levels, `1 s` with poll only, and `5 s` without either option. Upstream uses `250 ms` levels, `2 s` changed-state polling, and device notifications. Existing ezBEQ consumers accept complete snapshots; polling cadence and unsolicited hardware notifications are not asserted identical.
- do not repurpose this endpoint for raw binary tunneling

### Raw WebSocket bridge example

Compatibility requirement:
- `/devices/{id}/ws` remains the raw frame bridge used for frame-level transport bridging
- this endpoint is distinct from the status/levels WebSocket on `/devices/{id}`

## TCP Bridge Contract

### Port

`5333`

### Compatibility requirements

- Speak the existing MiniDSP TCP framing used by minidsp-rs / WI-DG / Device Console workflows
- Allow unknown valid command frames to pass through
- Keep the bridge as a frame-level HID forwarding transport, even if internally all commands are serialized through a queue
- Support the official-app advertisement model with configured `ip` and `name`

### Connection model

The transport may be internally arbitrated and serialized, but wire-level compatibility must remain intact for a normal compatibility client.

Upstream accepts multiple clients; rejecting a second client is not an upstream requirement. Desktop accepts up to eight clients per listener, serialized through the device queue. ESP32 currently services one bridge connection at a time; broader multi-client parity remains open.

Desktop supports multiple `[[tcp_server]]` listeners mapped by `device_index` or `device_serial`; serial takes precedence. Each listener's discovery advertisement uses the same device binding. With neither selector, prefer the first local USB device. Host tests cover two mock devices and concurrent bridge/HTTP traffic; actual socket acceptance and physical multi-device operation remain release gates.

### TCP response framing requirement

Write-ack responses must not include trailing HID buffer bytes.

When real hardware acknowledges a write command, the USB HID report can have the form `[0x01][echoed command bytes][padding/junk to fill the 64-byte HID report]`. The bridge must trim that report to just the echoed command bytes before network framing. Forwarding the full 64-byte buffer as a response payload is a protocol error.

The official MiniDSP Device Console enforces this strictly: it crashes with `Error: Unexpected DSP command code 1` if any trailing garbage byte in the framed response happens to contain `0x01` where it expects a new command code.

Canonical verification (2026-04-10, Python TCP client against the fixed daemon on the physical 2x4 HD):

| Command | Response size before fix | Response size after fix |
|---------|--------------------------|------------------------|
| `WriteDSP` | 64 bytes (length byte `0x40`) | 9 bytes (length byte `0x09`) |
| `SetVolume` | 64 bytes | 3 bytes |
| `SetMute` | 64 bytes | 3 bytes |
| `ReadHardwareId` | 12 bytes | 12 bytes (unchanged) |
| `ReadFloats` | 12 bytes | 12 bytes (unchanged) |

Read responses are not affected — only write-ack responses that arrive as unframed `0x01`-prefix HID echoes need trimming. The trim rule: if the request opcode is a write command and the oversized response first byte matches the request first byte, truncate the response to `cmd.payload.len()`. See `minidsp/src/queue.rs::execute_queued_command` for the implementation.

### TCP framing example

Representative behavior:
- server listens on port `5333`
- bridge forwards USB HID frames for official-app compatibility
- request `02 31 33` is a length-prefixed command plus checksum
- ordinary TCP replies contain a self-inclusive length and payload **without checksum**; e.g. a fake hardware-ID reply is `05 31 01 0d 0a`
- a zero-padded 64-byte request selects padded replies on desktop; the reply is the same checksum-free response followed by zeros to 64 bytes
- source: upstream `minidsp/src/transport/net/codec.rs:10-12,126-172`; in-memory stream regressions also cover fragmentation and concurrent clients

## UDP Discovery Contract

### Port

`3999`

### Compatibility requirements

- Broadcast every 1 second to `255.255.255.255:3999`
- Also send the same packet to multicast `224.0.0.250:3999` for Device Console compatibility
- Use the expected magic bytes and packet layout
- Include MAC, IP, hw_id, firmware version, dsp_id, serial, and hostname fields in the expected format
- Packet byte layout is part of compatibility, not just high-level semantics

### Current emitted packet versus upstream

Desktop and ESP32 currently emit the shared Wi-DG-style **72-byte** encoder:
magic `80 12 05 A0`, tag bytes `14 D5` at offsets `12..14`, `FF` filler,
and at most 36 hostname bytes. Identity fields and offsets match the upstream
layout below. No-device appliance identity is an extension; upstream advertises
only when a selected device is available. Default desktop discovery sends on
non-loopback interfaces, including multicast `224.0.0.250`, at one-second
intervals. Explicit `advertise.ip` uses upstream's synthetic MAC
`FF:FF:<four IPv4 octets>` to distinguish configured listeners. Default automatic
beacons retain the host MAC. Physical Wi-DG fingerprint parity and LAN receipt
remain separate verification gates.

### Upstream packet structure (comparison baseline)

Source: minidsp-rs `dev` branch (`minidsp/src/transport/net/discovery/mod.rs`), Wi-DG firmware Alexa client (`d[0] == 0x80 && d[2] == 0x05 && d[3] == 0xA0`), and real device packet captures embedded in the test suite.

#### Magic header (bytes 0..4)

```text
80 00 05 A0
```

These are the bytes written by minidsp-rs `to_bytes()` on the `dev` branch. Real device packets (WI-DG, PWR-ICE) use `80 12 05 A0` -- byte 1 is `0x12` on hardware. The Wi-DG firmware only checks bytes `[0]`, `[2]`, `[3]` (skips byte 1), so both `0x00` and `0x12` are accepted. The MiniDSP Device Console plugin uses the same check via the Wi-DG's Alexa client code.

Upstream writes `80 00 05 A0`; minidsp-open currently writes `80 12 05 A0` through its shared Wi-DG encoder. Both forms parse in minidsp-open. Byte-for-byte equality with upstream is not claimed.

#### Field offsets

| Offset | Length | Field | Notes |
|--------|--------|-------|-------|
| `0..4` | 4 | Magic header | `80 00 05 A0` |
| `4..6` | 2 | (unused) | Zero-filled |
| `6..12` | 6 | MAC address | 6 bytes, big-endian |
| `12..14` | 2 | (unused) | Zero-filled |
| `14..18` | 4 | IPv4 address | Network byte order |
| `18` | 1 | `hw_id` | Device hardware ID |
| `19` | 1 | `fw_major` | Firmware major version |
| `20` | 1 | `fw_minor` | Firmware minor version |
| `21` | 1 | `dsp_id` | DSP version / type ID |
| `22..24` | 2 | Serial number | Big-endian u16: `((serial - 900000) & 0xFFFF)`, written as `(sn >> 8), (sn & 0xFF)` |
| `24..35` | 11 | (unused) | Zero-filled; byte 30 is non-zero in some real captures but meaning unknown |
| `35` | 1 | Hostname length | `u8`, max 255 |
| `36..36+len` | var | Hostname | UTF-8 string, not null-terminated |

Minimum valid packet length: 36 bytes (empty hostname). Real packets may have trailing bytes after the hostname (seen in captures: crypto/signature data from the WI-DG up to ~68 bytes total). The parser ignores trailing data.

#### Serial number encoding

minidsp-rs `dev` branch encodes the serial for advertisement as: `((device_info.serial - 900000) & 0xFFFF) as u16`. This maps the full 32-bit device serial into the 16-bit UDP field. On parse, the serial is read as a raw `u16` without the reverse transform.

#### Advertised packet construction (daemon)

On the `dev` branch, the daemon dynamically constructs the packet from actual device info:
- `hw_id`, `dsp_id`, `fw_major`, `fw_minor` come from the connected device's `DeviceInfo`
- `ip_address` comes from the config `advertise.ip`
- `hostname` comes from the config `advertise.name`
- MAC is synthetic: `[0xFF, 0xFF, ip[0], ip[1], ip[2], ip[3]]` -- used by Device Console to distinguish devices on its list page

On the older `master`/`next` branches, the packet used hardcoded dummy values (hwid=0 or 10, typ=0, sn=65535, fake MAC) because it did not read device info. The `dev` branch fixed this.

#### Branch history

| Branch | Header bytes | Field names | Device info in packet |
|--------|-------------|-------------|----------------------|
| `master` | All zeros (bug) | `typ` (not `dsp_id`) | No -- hardcoded dummy values |
| `next` | All zeros (bug) | `typ` (not `dsp_id`) | No -- hardcoded dummy values |
| `dev` | `80 00 05 A0` (fixed) | `dsp_id`, `fw_major`, `fw_minor` | Yes -- from connected device |

The fix was committed as: `feat(net/discovery): change broadcast to start with 0x80 0x00 0x05 0xA0. closes #576` followed by `fix(discovery): add device type and firmware information to discovery packet`.

#### Real device capture (from minidsp-rs test suite)

```text
80 12 05 A0 00 00 AA BB CC DD EE FF 00 00 C0 A8
79 8D DE 03 00 33 AB 64 00 00 00 00 00 00 AC 00
00 00 00 08 50 52 4F 44 55 43 54 00
```

Parsed: MAC=`AA:BB:CC:DD:EE:FF`, IP=`192.168.121.141`, hw_id=`0xDE`(222), fw_major=3, fw_minor=0, dsp_id=`0x33`(51), sn=`0xAB64`(43876), hostname=`PRODUCT\0` (8 bytes including trailing null in this capture).

#### PWR-ICE device capture (from minidsp-rs test suite)

```text
80 12 05 A0 00 00 00 50 C2 D4 37 26 00 00 C0 A8
B2 78 00 03 00 33 00 B6 00 00 00 00 00 00 5B 00
00 00 00 10 50 57 52 2D 49 43 45 32 20 20 20 20
20 20 20 00
```

Parsed: MAC=`00:50:C2:D4:37:26`, IP=`192.168.178.120`, hw_id=0, fw_major=3, fw_minor=0, dsp_id=`0x33`(51), sn=182, hostname=`PWR-ICE2       \0` (16 bytes, space-padded with trailing null).

This surface is required for Device Console and ecosystem discovery compatibility.

## Unix Socket Contract

### Path

`/tmp/minidsp.sock`

### Compatibility requirements

- The daemon creates this Unix socket alongside the HTTP TCP listener on Unix platforms
- Permissions: `0o766` (world-readable/writable, allowing other users on the same host to connect)
- If the socket file already exists at startup, delete and recreate it
- The socket serves the same HTTP API as the TCP listener (same routes, same handlers)

### CLI discovery order

The upstream CLI tries transports in this order:
1. `--daemon-sock` (explicit Unix socket path)
2. `--tcp` (explicit TCP address)
3. `--url` (direct device URL)
4. `--daemon-url` (HTTP/WebSocket to remote daemon)
5. `--usb` (explicit HID device)
6. **Default fallback:** Try `/tmp/minidsp.sock` first, then default USB HID enumeration

This means: if the daemon is running, the CLI automatically uses it via the Unix socket without any flags. This is critical for drop-in compatibility — users expect `minidsp` (no args) to "just work" whether the daemon is running or not.

## Config File Contract

### Format

TOML. The daemon must accept the same config format as minidsp-rs `minidspd`.

### Key fields (from minidsp-rs `config.example.toml`)

```toml
ignore_advertisements = false
# minidsp-open extension; omit for no ceiling on a fresh installation:
# max_volume_db = -20.5

[http_server]
bind_address = "0.0.0.0:5380"
cors_origin = "https://ui.example.com" # Optional extra browser origin; "*" opts out

[[tcp_server]]
bind_address = "0.0.0.0:5333"
device_index = 0         # or device_serial = 912345
advertise = { ip = "192.168.1.100", name = "Example TV" }

[[static_device]]
url = "tcp://192.168.1.2:5333"

```

### Maximum master volume (minidsp-open extension)

The top-level optional `max_volume_db` key accepts -127.0 through 0.0 in exact
0.5 dB steps. Unset on a fresh installation means no ceiling. An explicit config
value takes precedence at startup over runtime settings saved by `POST /settings`.
The daemon persists runtime settings beside its shadow cache, at
`~/.config/minidsp-open/settings.json` (temporary directory fallback when HOME is
unavailable). Remove the config key to use the persisted value, including a
persisted disabled ceiling. Invalid config or persisted settings fail startup.
ESP32 settings persist in the NVS `settings` namespace across reboot/reconnect.

`GET /settings` returns `{"maxVolumeDb":null}` when disabled, or a numeric ceiling.
`POST /settings` accepts the same required field, sets or clears the ceiling, and
returns the saved settings. Invalid values return `400`; browser mutations use
the existing Origin policy. Lowering the ceiling immediately lowers any louder
connected device. If persistence or hardware lowering fails, POST returns `500`
and retains the new runtime policy; GET reports that active policy.

Absolute writes above the ceiling clamp; relative writes clamp their result.
Status responses report the volume actually set. This covers master status,
config/snapshot writes, raw WebSocket and TCP bridge volume commands, and ESP32
ezBEQ master volume. Rattle requests with any `volumesDb` above it return `400`
before audio/HID actions. `/play` uses the current volume and never raises it.

CLI requests through `--daemon-url`, the daemon socket (including automatic
socket discovery), or a minidsp-open TCP bridge are bounded. Direct USB CLI use
is local terminal control and does not read/apply daemon settings. The MiniDSP's
own front-panel knob and IR remote also bypass minidsp-open and cannot be stopped
by this ceiling. LAN clients can raise/clear the setting; it is not authentication.

### Browser origins (intentional security difference)

Both HTTP servers allow native clients that omit `Origin`, including ezBEQ's Python server, Home Assistant, the minidsp CLI, scripts, and driver tools. `Host: nas:5380` works without a hostname allowlist. The board's hosted ezBEQ UI, `/status`, and `/update` remain same-origin.

For POST/PUT/PATCH/DELETE and WebSocket upgrades, a browser `Origin` must be a local UI: `localhost`, a single-label host (`http://nas:5380`), a `*.local`, `.lan`, `.home`, `.home.arpa`, `.internal` or `.localdomain` name, or a loopback/private/link-local IP literal (RFC 1918, `fc00::/7`, `fe80::/10`). An IP-literal origin may also address that same IP literal, so a global IPv6 address works too. Anything else must match the daemon's configured `cors_origin`; other origins, including `null`, get JSON `403`. An explicit `cors_origin = "*"` opts out. GET responses and preflights echo only allowed origins; native GETs remain available.

A page served from a public DNS name is refused even when its Host matches, because that's exactly what DNS rebinding produces: an attacker's name resolving to the board. If you reach the board through your own public-suffix name, use its IP or `.local` name in the browser, or set `cors_origin` (desktop) to that origin. Requests without `Origin` are never checked, so hostname-addressed native clients are unaffected.

### Compatibility requirements

- Accept existing `minidspd` config files without errors
- Unknown keys should be silently ignored (forward compatibility)
- Default bind address for HTTP: `0.0.0.0:5380`
- Default bind address for TCP: `0.0.0.0:5333`
- `cors_origin` (also accepted as `allowed_origins`) adds an allowed browser origin; explicit `"*"` allows all origins. Unset uses the safe browser-origin policy above, an intentional difference from upstream's permissive `*`.
- `advertise.ip` and `advertise.name` used for UDP discovery packets
- `device_serial` and `device_index` for multi-device TCP server mapping
- `advertise.bind_address` selects the UDP sender bind address
- `[[static_device]] url` adds an explicit USB/TCP/WebSocket device at daemon startup
- `ignore_advertisements` disables remote discovery consumption, not our own outgoing beacons
- Unknown keys are ignored. The earlier minidsp-open `[tcp_server]` singleton and top-level `[advertise]` remain accepted extensions.
- Defaults are additive: minidsp-open starts TCP/discovery automatically and applies the browser-origin policy above unless `cors_origin = "*"` is configured. Upstream's default TCP-server list is empty; its CORS middleware is opt-in.

## Additive Features

These are allowed, but they are not part of the compatibility contract unless later promoted here:

- `GET /settings` and `POST /settings` — optional maximum master volume, disabled by default; see the config extension above.
- minidsp-open rattle-finder extensions (desktop daemon and ESP32 HTTP API):
  `minidsp --daemon-url http://host:5380 rattle` accepts optional `--tones`,
  `--start-hz`/`--end-hz`/`--count`, `--hold`, `--gap`, `--cycles`, and
  `--volume` flags; `MINIDSPD_URL`, `--device-index`, and `-o json`/`jsonline`
  are supported. Ctrl-C requests a fade-out and waits for restoration.
  `POST /devices/{id}/rattle` starts, `GET /devices/{id}/rattle` reports status,
  and `POST /devices/{id}/rattle/stop` stops. See [ref-playback.md](ref-playback.md#rattle-finder)
  for the shared contract. Existing CLI and HTTP behavior stays compatible.
- `GET /devices/{id}/config` full config read-back
- extended `/devices/{id}/config` fields beyond the compatibility request body documented above (including the optional `polarity_inverted` per routing entry on families whose `RoutingMatrix.polarity_base` is populated — DSP_110 Flex Eight, DSP_113 / DSP_115 Flex HT/HTx, legacy DDRC-88BM)
- controller-side snapshot backup / restore routes:
  - `GET /devices/{id}/snapshot` — export a strict `DeviceSnapshot` when every preset is complete, otherwise return `409` with the incomplete snapshot body
  - `POST /devices/{id}/snapshot` — restore a previously exported snapshot, verify it immediately with a follow-up export, and return the verified snapshot body
  - `POST /devices/{id}/snapshot/learn` — sweep presets, seed the controller's trusted snapshot cache, and return the learned snapshot (`200` when strict, `409` when incomplete)
- controller-side snapshot CLI:
  - `minidsp snapshot export <file>`
  - `minidsp snapshot import <file>`
  - `snapshot export --allow-partial` writes an explicitly incomplete artifact instead of refusing it
- `GET /status` — ESP32 status/rattle UI; no upstream counterpart and no desktop page
- measurement-mode endpoints
- measurement playback endpoints:
  - `POST /sync` echoes `clientTimeUs` and returns `serverTimeUs` from the same platform clock basis used by `/play.startedAtUs`
  - desktop currently uses `SystemTime`-derived Unix-epoch microseconds for both values; ESP32 uses the `esp_timer_get_time()` domain, with `/play.startedAtUs` preferring the UAC2 hardware start timestamp and falling back into that same timer domain
  - `POST /devices/{id}/play` uses the canonical request fields `sweepDurationSec`, `sweepStartHz`, `sweepEndHz`, `sweepFadeSec`, `markerBefore`, `markerAfter`, `markerGapMs`, `markerFrequencyHz`, `markerCycles`, `decaySec`, `sampleRate`, and `bitDepth`
  - `{}` is the canonical default measurement bundle: fixed `2 s` low-level preamble, one pre-marker, `50 ms` gap, the sweep, `50 ms` gap, one post-marker, and `2 s` decay
  - omitted `markerBefore`/`markerAfter` default to `true`, omitted `markerCycles` defaults to `1`, and omitted `markerGapMs` defaults to `50`
  - `markerCycles = 0` disables both marker sections entirely and removes their gaps, even if `markerBefore` / `markerAfter` remain `true`
  - sweep-only playback is explicit via `{"markerBefore":false,"markerAfter":false}`
  - legacy nested request fields such as `sweep`, `toneBurst`, `tailToneBurst`, `preambleSec`, and `gapSec` are rejected
- display endpoints
- DRE endpoints
- modification-token endpoints
- `POST /devices/{id}/preset/copy` (CopyPreset + verifyProgram round-trip — copies the active preset to the remaining preset slots and bails before bumping tokens if `0xFFA3` indicates a corrupt save)
- bass management endpoints for the HT/HTx 113 + 115 firmware variants (returns `501 Not Implemented` on every other family):
  - `GET /devices/{id}/bm` — full BM snapshot (8 gain channels + 9 × 8 mixer cells, LFE virtual row at index 8)
  - `POST /devices/{id}/bm/channel/{ch}` — partial patch `{gain_db?, muted?}`
  - `POST /devices/{id}/bm/mixer/{row}/{col}` — partial patch `{enabled?, gain_db?, polarity_inverted?}`
- `/debug/*`
- `/llms.txt`
- OpenAPI
- ESP32 provisioning/system endpoints
- ezBEQ-native embedded API for the ESP32-served UI

Additive rule:
- additive features must not break or redefine compatibility endpoints
- additive features should prefer new routes over behavior changes on compatibility routes

Current verification note:
- TCP bridge write-ack framing is fixed and wire-verified on the physical 2x4 HD (2026-04-10) via Python TCP client against the live daemon: `WriteDSP` shrank from 64 bytes to 9 bytes, `SetVolume` from 64 to 3 bytes, `SetMute` from 64 to 3 bytes, and read responses are unchanged at 12 bytes each. The follow-up live official-app check on 2026-04-13 also proved the stock macOS Device Console can now complete the network-device `Initialize to Default Settings` flow over the ESP32 bridge without the previous parser/reload failure. Remaining official-app work is the broader interactive surface: status reads, volume/mute/preset/source writes, routing edits, and PEQ edits. Tracked in [PROGRESS.md](../PROGRESS.md#device-control-and-compatibility) and `docs/todo-checker.md` Section 4.2.
- Desktop `GET /devices/{id}` is live-verified on the physical 2x4 HD after the 2026-04-10 meter fallback fix: malformed DSP100 meter replies now degrade to `-140 dB` silence instead of stalling compatibility status.
- Desktop config read-back is implemented, but on DSP100 / 2x4 HD some fields still rely on verified shadow state rather than authoritative live hardware readback. Fresh-daemon `/devices/{id}/config` now clamps bogus low-address DSP100 scalar/tap reads to verified shadow or conservative zero instead of surfacing garbage values.
- Desktop daemon preset-content writes now also mirror the official app's explicit modification-token save model on the physical 2x4 HD, so `/devices/{id}/config` shadow restore is keyed by a real active-preset freshness marker rather than by raw runtime writes that never touched `0xFFC8`.
- Desktop daemon can also import missing preset shadow from the official Device Console `setting*.xml` cache when the XML timestamp matches the live device token for that slot and the XML `dspversion` matches the connected DSP family. That is part of the controller-side recovery model for DSP100, not a claim of stronger live hardware readback.
- Controller-side snapshot backup / restore is now live-verified on the physical 2x4 HD `912345` on both desktop paths:
  - raw USB CLI export -> destructive mutate -> import -> export matches baseline once metadata fields are ignored
  - daemon-backed destructive harness run `20260415T234640Z` completed with `44` passed, `3` skipped, `0` failed and a matching post-restore strict snapshot
- Desktop WebSocket compatibility surfaces are live-verified on the physical 2x4 HD: `/devices/{id}?poll=true` sends an immediate status frame, `/devices/{id}?levels=true&poll=true` keeps source writes fast and still allows preset writes to complete, and `/devices/{id}/ws` ignores text frames without breaking framed binary commands.
- ESP32 `/devices/0/config` is now verified on the physical 2x4 HD for typed scalar read/write behavior and PEQ write/readback through the same PEQ shadow/default model used on desktop when live PEQ `0x14` reads return the all-zero placeholder.

## Canonical Ownership

Use this file as the canonical contract for:
- CLI compatibility language in `CLAUDE.md`
- compatibility tasks in `PROGRESS.md`
- public API wording in `PROJECT.md`
- minidsp-rs / ezBEQ compatibility notes in `docs/ref-ezbeq.md`

Other reference docs may discuss rationale or implementation, but should point back here for the public contract.

## Test Matrix

Before release, verify:

| Surface | Test |
|---------|------|
| CLI | ezBEQ shell-out commands run unchanged |
| CLI | `-o jsonline` remains parseable by existing consumers |
| CLI | `-f` preserves file order |
| CLI | `--force-kind` accepted |
| HTTP | `GET /devices`, `GET /devices/{id}`, `POST /devices/{id}`, `POST /devices/{id}/config` match compatibility behavior |
| HTTP | `POST /devices/{id}/volume/{direction}`, `POST /devices/{id}/source/{source}`, `POST /devices/{id}/preset/{preset}` work as upstream compatibility routes |
| WebSocket | `/devices/{id}` with `poll=true` and `levels=true` works for existing level/state consumers |
| WebSocket | `/devices/{id}/ws` works as the raw frame bridge |
| TCP | Device Console coexistence through the bridge |
| UDP | discovery packet recognized by existing clients |

`PROGRESS.md` and `docs/todo-checker.md` should track these as explicit release gates.

## Final Freeze Checklist

Before phase 1 implementation is called complete, freeze these exact items here:
- `-o jsonline` sample output captured from current compatible behavior
- `probe` output line captured from current compatible behavior
- compatibility HTTP request/response bodies for PEQ, mute, gain, source, preset switch
- compatibility WebSocket message shapes on `/devices/{id}`
- raw bridge behavior on `/devices/{id}/ws`
- representative TCP frame transcript or packet capture
- representative UDP discovery packet bytes

## Optional ESP32 password

This is additive and opt-in. Without a password the existing LAN control contract stays unchanged. Enable it on `/status` → Device settings → Password or in the optional first-provisioning field. All HTTP/WS control then requires a session cookie, Basic `admin:<password>`, or Bearer session token. WiFi fallback retains the gate. TCP 5333 and legacy UDP 3999 advertising stop while protected, and resume when cleared; mDNS remains public.

| Client | Protected firmware support |
|---|---|
| Board-hosted ezBEQ UI, `/status`, `/update` | Browser login; same-origin session cookie covers HTTP and WS |
| External ezBEQ | Updated minidsp-open CLI through `exe` / `options`, below; omit direct WS settings |
| HA xuio/minidsp-rs-hass | Basic via `http://admin:ENCODED_PASSWORD@BOARD:5380`; source-inspected, unverified live for HTTP/WS; URL may appear in debug logs |
| HA jprouty/hass-minidsp-rs | Cannot authenticate without modification |
| HA BadLiveware/minidsp-rs-homeassistant | Cannot authenticate; rejects URL credentials |
| HA RESTful Command | Basic username/password or Bearer Authorization header |
| Stock minidsp-rs CLI | Cannot authenticate; URL credentials do not add Authorization |
| Device Console / official mobile app | Cannot authenticate; TCP bridge disabled |

For external ezBEQ, point `exe` at the updated minidsp-open binary and use:

```yaml
exe: /usr/local/bin/minidsp
options: '--daemon-url http://BOARD:5380 --password-file /config/minidsp-password'
```

Omit `wsDeviceId` and `wsIp`: ezBEQ's direct WebSocket connection has no credential configuration. Its `options` splits on spaces, so use a password-file path without spaces. CLI `--password-file` overrides `MINIDSP_PASSWORD`; the UTF-8 file has one optional trailing LF/CRLF removed. Environment values are used exactly. Both accept 1–128 UTF-8 bytes. There is no plaintext password argument, because ezBEQ logs command lines. Protect the password file and service environment. `tools/ota-push.sh` accepts the same `--password-file` / `MINIDSP_PASSWORD` credentials for every curl call.

`POST /login {"password":"..."}` returns a session token and `Set-Cookie: minidsp_session=...; Path=/; HttpOnly; SameSite=Strict`. Use `Authorization: Bearer <token>` for native HTTP/WS clients. `POST /logout` revokes that session. Sessions expire after 30 minutes idle or 12 hours total, and on reboot. `POST /settings/password` accepts `{"current":"...","new":"..."}` or `{"current":"...","clear":true}`; the current password is required when protected, and every session/WS connection is revoked after a change. `GET /settings/password` reports `passwordSet`. Invalid credentials yield 401 JSON. Native clients also get a `WWW-Authenticate: Basic` challenge; browser requests (with `Origin`, `Referer` or `Sec-Fetch-Mode`) don't, so the browser shows no credential dialog, and HTML GET navigation redirects to `/login`. A wrong `current` password yields 403, and backoff yields 429 with `Retry-After`. Basic WS connections stay open until the password changes. Plain HTTP exposes credentials and tokens to LAN interception; see [SECURITY.md](../SECURITY.md).
