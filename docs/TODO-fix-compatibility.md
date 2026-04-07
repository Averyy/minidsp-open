# Compatibility Audit: minidsp-rs / ezBEQ

Audit date: 2026-04-07

This file records the current compatibility state after re-checking the live code against:

- our compatibility contract in `docs/ref-compatibility.md`
- our ezBEQ notes in `docs/ref-ezbeq.md`
- upstream `minidsp-rs` CLI source on GitHub `dev`

It replaces the older line-by-line gap list, which had become stale after multiple fixes landed.

## Summary

We are close on the core command and daemon surfaces, but we are **not yet at a truthful 100% drop-in claim** for `minidsp-rs`.

The main remaining blocker is now **real MiniDSP-attached validation on ESP32**.

The no-MiniDSP ESP32 appliance path is working on hardware:

- Wi-Fi provisioning / STA mode
- HTTP API
- UI serving
- updater debug endpoints
- UDP discovery / `probe --net`

What is still not proven end to end is the real USB host control path with an actual MiniDSP attached:

- probe and follow-on EEPROM reads
- serial read
- ACK/write commands
- ezBEQ filter load / clear

There are still a few desktop/daemon validation gaps too, but the highest-priority unresolved risk is no longer generic discovery behavior; it is the real MiniDSP-attached ESP32 path.

For ezBEQ specifically, the critical paths are in much better shape:

- `minidsp -f <file>` sequential command execution
- `minidsp -o jsonline`
- HTTP status/config routes
- compatibility WebSocket on `/devices/{id}?levels=true&poll=true`

Those are largely in place.

## Resolved In This Pass

### Desktop `/devices` now returns the real transport URL

Previously the desktop daemon fabricated `ws://localhost:5380/devices/{id}/ws`.

Now:

- `MiniDspClient` stores the transport URL from the actual transport
- `GET /devices` returns that real URL
- the `/devices` array is emitted in sorted device-id order instead of `HashMap` iteration order

Files:

- `minidsp/src/client.rs`
- `daemon/src/http/routes.rs`

### CLI `-o json` now works

Previously:

- `-o jsonline` worked
- `-o json` silently fell back to text

Now:

- `jsonline` emits compact JSON
- `json` emits pretty JSON

Files:

- `app-desktop/src/cli.rs`
- `app-desktop/src/output.rs`

### `--url` now accepts real probe-style USB URLs

Previously:

- `--url usb:%2Fdev%2Fhidraw3?vid=2752&pid=0011` did not work even though that is the kind of URL probe output produces

Now:

- the path is query-stripped
- URL-decoded HID paths work
- `ws://` and `http://` URLs are accepted for the WS bridge path too

Files:

- `app-desktop/src/main.rs`
- `minidsp/src/transport/ws.rs`

### ESP32 UDP discovery now sends both broadcast and multicast

Desktop already did this. ESP32 now matches that behavior better.

File:

- `app-esp32/src/main.rs`

### ESP32 no-device UDP discovery now actually advertises

Previously:

- the ESP32 UDP broadcaster sent nothing unless a real MiniDSP was attached
- `probe --net` therefore found nothing in the no-MiniDSP appliance state even though the board was up on Wi-Fi

Now:

- the ESP32 advertises a fallback bridge identity when no MiniDSP is attached
- desktop `probe --net` receives and decodes those packets correctly
- real UDP advertisement capture was performed against the live board

Files:

- `app-esp32/src/main.rs`
- `app-desktop/src/main.rs`

### ESP32 HTTP handler-limit failure is fixed

Previously:

- STA mode with ezBEQ enabled registered more URI handlers than the configured limit
- the HTTP server aborted with `ESP_ERR_HTTPD_HANDLERS_FULL`

Now:

- `max_uri_handlers` was raised with headroom for the current STA+ezBEQ route set
- the no-MiniDSP HTTP appliance path is working again on hardware

File:

- `app-esp32/src/http_server.rs`

## Verified Compatible Areas

### ezBEQ-critical CLI behavior

Verified in current code:

- `minidsp -f <file>` executes sequentially in file order
- `minidsp -o jsonline` emits `StatusSummary` JSON
- `input/output peq set`
- `input/output peq bypass`
- `input/output mute`
- `input/output gain`
- `config`
- `gain`
- `mute`
- `source`

Relevant files:

- `app-desktop/src/main.rs`
- `app-desktop/src/cli.rs`
- `app-desktop/src/output.rs`

### Desktop daemon compatibility routes

Verified in current code:

- `GET /devices`
- `GET /devices/{id}`
- `POST /devices/{id}`
- `POST /devices/{id}/config`
- `POST /devices/{id}/volume/{direction}`
- `POST /devices/{id}/source/{source}`
- `POST /devices/{id}/preset/{preset}`
- WebSocket compatibility endpoint on `/devices/{id}`
- raw WebSocket bridge on `/devices/{id}/ws`
- TCP bridge on port `5333`
- UDP discovery on port `3999`

Relevant files:

- `daemon/src/http/routes.rs`
- `daemon/src/http/json.rs`
- `daemon/src/tcp/mod.rs`
- `daemon/src/discovery/mod.rs`

### ESP32 network replacement surface

Verified in current code and on live hardware:

- HTTP compatibility routes for `/devices/0`, `/volume/*`, `/source/*`, `/preset/*`, `/config`
- compatibility WebSocket on `/devices/0`
- raw WebSocket bridge on `/devices/0/ws`
- TCP bridge on `5333`
- UDP discovery on `3999`
- no-MiniDSP appliance responses for `/`, `/devices`, `/api/1/version`, `/api/2/devices`, `/debug/info`, `/debug/updater`, `/debug/updater/check`

Relevant files:

- `app-esp32/src/http_server.rs`
- `app-esp32/src/main.rs`

## Remaining Gaps

The four compatibility blockers that prompted this pass are now fixed in code:

- `probe` is transport-aware and supports `probe --net`
- `--all-local-devices` now executes against all local HID devices
- `--daemon-url` / `--daemon-sock` now preflight through `/devices` before WS connection
- ESP32 `GET /devices` now reports a reconnect-usable network WS URL

What remains is validation depth, not a known contract mismatch:

- real multi-device local USB testing for `--all-local-devices`
- live daemon preflight testing over both TCP and Unix socket
- ESP32 runtime verification that the reported WS URL is correct in both STA and SoftAP modes
- real MiniDSP-attached validation for the ESP32 bridge, write path, and ezBEQ filter application

## What We Can Honestly Claim Right Now

### True today

- ezBEQ-style CLI batch writes are compatible
- ezBEQ-style status JSON is compatible
- the desktop daemon routes ezBEQ relies on are present
- the compatibility WebSocket endpoint exists and uses the expected path/query shape
- the raw WS bridge and TCP bridge exist
- `probe` is no longer hardwired to local USB
- `--all-local-devices` is no longer a dead flag
- daemon WS connection paths now verify device existence through `/devices`
- ESP32 `/devices` reports a network WS endpoint instead of a synthetic USB location
- ESP32 no-MiniDSP mode serves the UI/API/debug surface on hardware again after fixing the handler-limit regression
- `probe --net` now discovers the live ESP32 appliance in the no-device state

### Not true yet

We still should not claim “perfect” drop-in parity until the new behavior is exercised against:

- real multi-device hardware
- live daemon HTTP + Unix socket setups
- real MiniDSP-attached ESP32 traffic

## Recommended Next Work

In priority order:

1. Hardware-test `--all-local-devices` with 2+ local USB devices.
2. Exercise `probe` against daemon TCP, daemon Unix socket, raw TCP bridge, direct WS URL, and `probe --net`.
3. Verify ESP32 `/devices` URL correctness in STA and SoftAP modes on real hardware with and without a real MiniDSP attached.
4. Validate real MiniDSP command flow on ESP32: probe, EEPROM reads, serial read, ACK commands, and ezBEQ filter load/clear.
