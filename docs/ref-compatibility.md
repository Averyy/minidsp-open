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
- coverage progress and importer work are tracked in `PROGRESS.md` and `docs/context-dump.md`
- when protocol details differ between `minidsp-rs` and MiniDSP's own extracted app assets, the official app assets are the source of truth

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

### File execution semantics

`minidsp -f <file>` is compatibility-sensitive:
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
- `poll=true` enables periodic status polling
- `levels=true` enables frequent level polling
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

Upstream also supports multiple plugin-compatible listeners on one host, mapped by `device_index` or `device_serial`. Desktop compatibility should preserve equivalent multi-device deployment capability.

### TCP framing example

Representative behavior:
- server listens on port `5333`
- bridge forwards USB HID frames for official-app compatibility
- detailed framing/chunking rules should only be frozen here once captured from upstream implementation or a packet trace

## UDP Discovery Contract

### Port

`3999`

### Compatibility requirements

- Broadcast every 1 second to `255.255.255.255:3999`
- Also send the same packet to multicast `224.0.0.250:3999` for Device Console compatibility
- Use the expected magic bytes and packet layout
- Include MAC, IP, hw_id, firmware version, dsp_id, serial, and hostname fields in the expected format
- Packet byte layout is part of compatibility, not just high-level semantics

### Packet structure (verified from minidsp-rs source + real device captures)

Source: minidsp-rs `dev` branch (`minidsp/src/transport/net/discovery/mod.rs`), Wi-DG firmware Alexa client (`d[0] == 0x80 && d[2] == 0x05 && d[3] == 0xA0`), and real device packet captures embedded in the test suite.

#### Magic header (bytes 0..4)

```text
80 00 05 A0
```

These are the bytes written by minidsp-rs `to_bytes()` on the `dev` branch. Real device packets (WI-DG, PWR-ICE) use `80 12 05 A0` -- byte 1 is `0x12` on hardware. The Wi-DG firmware only checks bytes `[0]`, `[2]`, `[3]` (skips byte 1), so both `0x00` and `0x12` are accepted. The MiniDSP Device Console plugin uses the same check via the Wi-DG's Alexa client code.

We write `80 00 05 A0` to match minidsp-rs behavior. The meaning of byte 1 is unknown but not checked by any known consumer.

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
[http_server]
bind_address = "0.0.0.0:5380"
allowed_origins = "*"    # Optional CORS

[[tcp_server]]
bind_address = "0.0.0.0:5333"
device_index = 0         # or device_serial = 912345
advertise = { ip = "192.168.1.100", name = "Example TV" }

[[static_device]]
url = "tcp://192.168.1.2:5333"

ignore_advertisements = false
```

### Compatibility requirements

- Accept existing `minidspd` config files without errors
- Unknown keys should be silently ignored (forward compatibility)
- Default bind address for HTTP: `0.0.0.0:5380`
- Default bind address for TCP: `0.0.0.0:5333`
- `allowed_origins` for CORS support (web-based clients need this)
- `advertise.ip` and `advertise.name` used for UDP discovery packets
- `device_serial` and `device_index` for multi-device TCP server mapping

## Additive Features

These are allowed, but they are not part of the compatibility contract unless later promoted here:

- `GET /devices/{id}/config` full config read-back
- extended `/devices/{id}/config` fields beyond the compatibility request body documented above
- measurement-mode endpoints
- display endpoints
- DRE endpoints
- modification-token endpoints
- `/debug/*`
- `/llms.txt`
- OpenAPI
- ESP32 provisioning/system endpoints
- ezBEQ-native embedded API for the ESP32-served UI

Additive rule:
- additive features must not break or redefine compatibility endpoints
- additive features should prefer new routes over behavior changes on compatibility routes

Current verification note:
- Desktop config read-back is implemented.
- ESP32 `GET /devices/0/config` now exists, but physical-hardware verification is still open because real 2x4 HD config-register `ReadFloats` replies are not yet decoding into valid PEQ/gain state. That route must not be treated as parity-complete until `docs/todo-checker.md` closes Section 6.2.

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
