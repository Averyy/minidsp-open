# Compatibility Gaps: minidsp-rs Drop-In Replacement

Exhaustive audit: 2026-04-06 (final — verified against live minidsp-rs source on GitHub `dev` branch)

Cross-referenced every command, endpoint, response shape, flag, and protocol detail against
`docs/ref-compatibility.md`, `docs/ref-ezbeq.md`, AND the actual minidsp-rs source code at
`github.com/mrene/minidsp-rs` (dev branch). **This document is the canonical list — when
every item is resolved, we are a perfect match.**

---

## What Passes (verified correct)

### CLI commands — all 19 contract commands pass

| Command | Verified at |
|---------|-------------|
| `minidsp` (no args) | main.rs:242, output.rs:7-32 |
| `minidsp status` | main.rs:81,242 |
| `minidsp -o jsonline` | output.rs:41-59 (see #19 for precision caveat) |
| `minidsp -f <file>` | cli.rs:237-251, sequential, stops on error |
| `minidsp probe` | cli.rs:50-89, exact format match |
| `minidsp server` | cli.rs:92-234 |
| `minidsp --force-kind <type>` | main.rs:54-76, 15+ types (see #41: missing `m`-prefixed canonical strings) |
| `input {ch} peq {n} set -- {b0}...` | cli.rs:288-304 |
| `input {ch} peq {n} bypass {on\|off}` | cli.rs:306-309 |
| `input {ch} mute {on\|off}` | cli.rs:319-322 |
| `input {ch} gain -- {dB}` | cli.rs:314-317 |
| `output` variants of above | same code paths |
| `output {ch} delay {ms}` | cli.rs:324-334 |
| `config {n}` | cli.rs:279-281 |
| `gain -- {dB}` | cli.rs:262-265 |
| `mute {on\|off}` | cli.rs:267-269 |
| `source {name}` | cli.rs:271-277 |

### Desktop HTTP endpoints — response shapes correct

| Endpoint | Verified at |
|----------|-------------|
| GET /devices/{id} | Correct StatusSummary, dirac skip_serializing_if |
| POST /devices/{id} | Flat MasterStatus, nulls included |
| POST /volume/{direction} | up/down/toggle_mute, correct response |
| POST /source/{source} | Lowercase parsing, correct response |
| POST /preset/{preset} | Correct response |
| WebSocket /devices/{id} | poll=true, levels=true, correct intervals |
| WebSocket /devices/{id}/ws | Binary frame bridge (see #42: request-response only, not bidirectional) |

### Desktop infrastructure — working

| Component | Verified at |
|-----------|-------------|
| TCP bridge (port 5333) | daemon/src/tcp/mod.rs, WI-DG chunking |
| UDP discovery (port 3999) | daemon/src/discovery/mod.rs, correct packet structure |
| Unix socket listener | cli.rs:170-199, /tmp/minidsp.sock created, 0o766 |
| mDNS | _minidsp._tcp |

---

## What Fails

Every item below must be resolved. No exceptions, no deferrals.

#### 1. Desktop: POST /config silently ignores per-channel gain/mute

**Where:** `daemon/src/http/routes.rs:361-405`
**Bug:** `write_config()` only iterates `.peq` for both inputs and outputs. The `gain` and `mute` fields on `InputConfigJson` (json.rs:73-74) deserialize correctly but are never applied. Client gets 200 OK, values silently dropped.

**Fix (~20 lines in routes.rs):** After the PEQ loop in each section, add gain/mute handling. Note: input channels have `Option<u16>` addresses (some devices lack input gain/mute), output channels have plain `u16` (always present).

```rust
// After input PEQ loop (~line 380), inside the if let Some(input_ch) block:
if let Some(gain) = input.gain {
    if let Some(gain_addr) = input_ch.gain {
        client.set_channel_gain(gain_addr, gain).await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
}
if let Some(mute) = input.mute {
    if let Some(mute_addr) = input_ch.mute {
        client.set_channel_mute(mute_addr, mute).await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
}

// After output PEQ loop (~line 403), inside the if let Some(output_ch) block:
if let Some(gain) = output.gain {
    client.set_channel_gain(output_ch.gain, gain).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
}
if let Some(mute) = output.mute {
    client.set_channel_mute(output_ch.mute, mute).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
}
```

**Done when:** `POST /config {"inputs":[{"index":0,"gain":-3.5,"mute":true}]}` actually changes the device gain/mute.

---

#### 2. ESP32: POST /devices/0/config returns 501

**Where:** `app-esp32/src/http_server.rs:149-157`
**Bug:** Entire handler is a stub: `send_error(req, 501, "config write not yet implemented on ESP32")`.

**Fix (~80 lines in http_server.rs):** Replace the stub with a real handler. Parse JSON body into the same shape as `ConfigWriteJson` (or use serde_json::Value). Mirror the desktop logic in `daemon/src/http/routes.rs:335-408` but use blocking `DeviceState` methods instead of async client calls:
- `dev.set_config(preset)`, `dev.set_volume_db(vol)`, `dev.set_mute(mute)`, `dev.set_source(idx)` for master_status
- `dev.write_biquad(addr, &coeffs)`, `dev.bypass_filter(addr, bypass)` for PEQ
- `dev.set_channel_gain(addr, db)`, `dev.set_channel_mute(addr, muted)` for per-channel gain/mute
- Access DSP table via `dev.profile().dsp_table` for addresses

**Done when:** `POST /devices/0/config` with PEQ + gain/mute payload returns 200 and device state changes.

---

#### 3. Desktop: UDP discovery broadcasts IP 0.0.0.0

**Where:** `app-desktop/src/cli.rs:161`
**Bug:** Passes `Ipv4Addr::new(0, 0, 0, 0)` to `broadcast_loop()`. `AdvertiseConfig` (cli.rs:33-35) only has `hostname` — no `ip` field.

**Fix (~10 lines):**
1. Add `ip: Option<String>` to `AdvertiseConfig` struct (cli.rs:34)
2. Parse it in the server function (~cli.rs:112): `let adv_ip = config.advertise.as_ref().and_then(|a| a.ip.as_deref()).and_then(|s| s.parse().ok());`
3. If `advertise.ip` is not set, auto-detect: bind a UDP socket to the config's HTTP bind address and read its local IP, or use `local_ip_address` crate
4. Pass the resolved IP to `broadcast_loop()` at cli.rs:161 instead of `0.0.0.0`

**Done when:** `advertise.ip = "192.168.1.100"` in config produces discovery packets with that IP and MAC FF:FF:C0:A8:01:64.

---

#### 4. Desktop: GET /devices returns fabricated URL

**Where:** `daemon/src/http/routes.rs:203`
**Bug:** Hardcodes `format!("ws://localhost:5380/devices/{id}/ws")`. Contract shows actual transport URL like `usb:0001%3A0005%3A04?vid=2752&pid=0011`.

**Fix (~15 lines):**
1. `MiniDspClient` already has a `Transport` with a `url()` method (transport/tcp.rs:126-128, hid.rs equivalent). Store the transport URL as a field on MiniDspClient during `connect()`.
2. Add `pub fn transport_url(&self) -> &str` to MiniDspClient.
3. In `list_devices` handler, call `client.transport_url()` instead of fabricating a URL.
4. `DaemonState::add_device()` receives the client which already has the transport info.

**Done when:** `GET /devices` returns the real USB HID path or TCP address for each device.

---

#### 5. Desktop: CLI `--daemon-url` connects via raw TCP instead of WebSocket

**Where:** `app-desktop/src/main.rs:294-298`
**Bug:** Strips scheme prefix and connects via `TcpTransport` (raw MiniDSP protocol frames over plain TCP). But `--daemon-url` should connect to the daemon's HTTP server, which speaks HTTP — not raw protocol frames. A user passing `--daemon-url http://host:5380` gets a connection that immediately fails because raw frames hit an HTTP server.

**What upstream does:** minidsp-rs `builder.with_http(url)` does a two-step process: first HTTP `GET /devices` to discover how many devices exist, then WebSocket connect to `/devices/{i}/ws` for each device. The CLI then selects one (first, or `--device-index`). Protocol frames are sent as binary WebSocket messages.

**Fix (new file: `minidsp/src/transport/ws.rs`, ~100 lines):**
1. Create `WsTransport` implementing `Transport` trait using `tokio-tungstenite`
2. Two-step connection matching minidsp-rs: first HTTP `GET /devices` to discover devices, then WebSocket connect to `/devices/{id}/ws`
3. `send_frame()`: encode payload with `packet::encode_frame()`, send as WS Binary message
4. `recv_frame()`: receive Binary message, decode with `packet::decode_frame()`
5. In main.rs `--daemon-url` handler: parse URL, discover via HTTP, connect WS to first device (or `--device-index` if implemented per #22)
6. This two-step discover-then-connect matches exactly what minidsp-rs does

**Done when:** `minidsp --daemon-url http://localhost:5380` connects to the local daemon and executes commands.

---

#### 6. Desktop: CLI `--daemon-sock` / Unix socket CLIENT not implemented

**Where:** `minidsp/src/transport/tcp.rs:36-51`
**Bug:** `connect_unix()` literally returns `Err("not yet implemented, falling through to USB")`. The server creates the socket and serves HTTP on it (axum at cli.rs:187-189), but the client can never connect.

Running daemon + CLI on same machine does NOT auto-discover via socket. CLI silently falls back to direct USB, bypassing the daemon. Multiple CLI invocations can't share one USB connection.

**Fix:** Depends on #5. Once WsTransport exists:
1. Add `WsTransport::connect_unix(path)` that opens a Unix socket, performs HTTP upgrade to WebSocket on `/devices/0/ws`, then operates identically to the TCP WebSocket transport.
2. Update main.rs `connect()` default fallback (lines 307-322) and `--daemon-sock` handler (lines 282-285) to use WsTransport instead of TcpTransport.

**Done when:** With daemon running, `minidsp` (no args) auto-discovers via `/tmp/minidsp.sock` and prints status.

---

#### 7. Desktop: CLI `--url` flag missing

**Where:** `app-desktop/src/main.rs` CLI struct (lines 10-50)
**Bug:** Contract ref-compatibility.md:461 lists `--url` as step 3 in discovery order. It's a unified flag that accepts any transport URL (`usb:/path?vid=...`, `tcp://host:5333`, `ws://host:5380/devices/0/ws`). We have separate `--tcp` and `--usb` but no unified `--url`.

**Fix (~15 lines in main.rs):**
1. Add `#[arg(long, value_name = "URL")] url: Option<String>` to Cli struct
2. In `connect()`, insert as step 3 (between `--tcp` and `--daemon-url`):
   - `usb:` prefix → parse path/vid/pid, open HidTransport
   - `tcp:` prefix → strip prefix, open TcpTransport
   - `ws:` prefix → open WsTransport (requires #5)

**Done when:** `minidsp --url tcp://192.168.1.100:5333` connects to a remote TCP bridge.

---

#### 8. ESP32: All POST responses wrapped in `{"master":{...}}` instead of flat

**Where:** `app-esp32/src/http_server.rs:715-726` (`master_status_json()` function)
**Bug:** Returns `{"master":{"preset":0,...}}`. Contract ref-compatibility.md:225 says POST returns flat `{"preset":0,"source":"Toslink","volume":-8.0,"mute":false,"dirac":null}`. Desktop daemon correctly returns flat (routes.rs:250-256). All four ESP32 POST endpoints (status, volume, source, preset) use this function.

**Fix (~10 lines in http_server.rs):** Change `master_status_json()` to return flat object:
```rust
fn master_status_json(status: &StatusSummary) -> String {
    serde_json::to_string(&serde_json::json!({
        "preset": status.master.preset,
        "source": status.master.source,
        "volume": status.master.volume,
        "mute": status.master.mute,
        "dirac": status.master.dirac,
    }))
    .unwrap_or_default()
}
```

**Done when:** `POST /devices/0` returns `{"preset":0,"source":"Toslink",...}` without a `"master"` wrapper.

---

### HTTP and config gaps

#### 9. Desktop: POST /config missing delay and polarity for outputs

**Where:** `daemon/src/http/json.rs:66`
**Bug:** `ConfigWriteJson.outputs` uses `Vec<InputConfigJson>` which lacks `delay` and `invert` fields. DSP tables have `OutputChannel.delay` and `OutputChannel.polarity` addresses for every output.

**Fix (~40 lines across json.rs and routes.rs):**
1. In json.rs, create a new struct:
   ```rust
   #[derive(Debug, Serialize, Deserialize)]
   pub struct OutputConfigJson {
       pub index: u8,
       pub peq: Option<Vec<PeqConfigJson>>,
       pub gain: Option<f32>,
       pub mute: Option<bool>,
       pub delay: Option<std::time::Duration>, // NOT f32 — see note below
       pub invert: Option<bool>,               // polarity inversion
   }
   ```
   **CRITICAL: Delay wire format.** minidsp-rs serializes delay as `std::time::Duration`, which serde produces as `{"secs": 0, "nanos": 5000000}` (NOT `5.0`). There is no custom serializer — it's plain `#[derive(Serialize, Deserialize)]`. Using `Option<f32>` would be wire-incompatible. Either:
   - Use `Option<Duration>` to match minidsp-rs exactly, OR
   - Use a custom deserializer that accepts both `{"secs":N,"nanos":N}` and float ms
2. Change `ConfigWriteJson.outputs` from `Vec<InputConfigJson>` to `Vec<OutputConfigJson>`
3. In routes.rs `write_config()` output section, after PEQ + gain/mute handling:
   ```rust
   if let Some(delay) = output.delay {
       let sample_rate = client.profile().sample_rate as f32;
       let samples = (delay.as_secs_f32() * sample_rate).round();
       client.write_dsp(output_ch.delay, samples, true).await
           .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
   }
   if let Some(invert) = output.invert {
       let val = if invert { 1.0 } else { 0.0 }; // 1=inverted, 0=normal (NOT same as mute which is 2/1)
       client.write_dsp(output_ch.polarity, val, true).await
           .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
   }
   ```
   Polarity encoding verified: `1.0` = inverted, `0.0` = normal. This differs from mute encoding (`2.0` = muted, `1.0` = unmuted). Confirmed against both minidsp-rs source (`Value::Int(value as _)` in `invert()`) and our own `encode_invert()` in `protocol/src/dialect.rs:159-174`.

**Done when:** `POST /config {"outputs":[{"index":0,"delay":{"secs":0,"nanos":5000000},"invert":true}]}` sets 5ms delay and inverts polarity on the device. (Note: delay is `Duration` format, NOT float — see CRITICAL note above.)

---

#### 10. Desktop: GET /config returns partial data (additive feature, not compat gap)

**Where:** `daemon/src/http/routes.rs:410-447`
**Bug:** Only reads output PEQ coefficients. Gain hardcoded `None` (line 439). Mute `None` (line 440). No inputs. No master. No bypass. No delay. No compressor. But `client.read_device_config()` (client.rs:474-601) reads ALL of this and returns a `DeviceConfig` struct that already derives `Serialize`.

**Note:** minidsp-rs daemon does NOT have a GET /config endpoint either — only POST /config and GET /config/post.schema (JSON schema). So this is NOT a compatibility gap. It's our own additive feature (ref-compatibility.md:506). Still worth fixing since we advertise config readback as a differentiator.

**Fix (~15 lines in routes.rs):** Replace the manual PEQ-only loop with:
```rust
async fn read_config(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<usize>,
) -> Result<impl IntoResponse, StatusCode> {
    let client = state.get_device(id).await.ok_or(StatusCode::NOT_FOUND)?;
    let config = client.read_device_config().await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(config).into_response())
}
```

**Done when:** `GET /devices/0/config` returns master status, all input channels with gain/mute/PEQ, all output channels with gain/mute/delay/PEQ/compressor.

---

#### 11. Desktop: POST /config master_status ignores `dirac` field

**Where:** `daemon/src/http/routes.rs:342-359`
**Bug:** Handles preset (343), volume (346), mute (349), source (352) — but NOT dirac. `MasterStatusPostJson` has `dirac: Option<bool>` (json.rs:48). The separate `set_device_status` handler (routes.rs:243-245) DOES handle dirac. This handler doesn't.

**Fix (3 lines in routes.rs, after the source block ~line 358):**
```rust
if let Some(dirac) = ms.dirac {
    let _ = client.set_dirac_bypass(!dirac).await;
}
```

**Done when:** `POST /config {"master_status":{"dirac":true}}` enables Dirac on the device.

---

#### 12. ESP32: /devices/0/ws is NOT a raw frame bridge

**Where:** `app-esp32/src/http_server.rs:853-932`
**Bug:** Despite the comment "Raw frame bridge", when a binary frame is received (line 910), the handler responds with status JSON text (lines 911-918). Does not decode or forward binary HID frames.

**Fix (~50 lines in http_server.rs):** Replace the binary frame handler with actual frame forwarding:
```rust
Ok((FrameType::Binary(_), len)) => {
    // Decode incoming frame
    if let Some(payload) = minidsp_protocol::packet::decode_frame(&buf[..len]) {
        // Execute command on device
        if let Some(dev) = get_device(&st) {
            match dev.execute_raw(&payload) {
                Ok(resp) => {
                    let frame = minidsp_protocol::packet::encode_frame(&resp);
                    let _ = ws.send(FrameType::Binary(false), &frame);
                }
                Err(e) => log::warn!("WS raw bridge: device error: {e}"),
            }
        }
    }
}
```
Note: Verify `DeviceState` has (or needs) an `execute_raw(&[u8]) -> Result<Vec<u8>>` method. If not, add one that wraps the USB HID command queue.

**Done when:** A binary WebSocket frame sent to `/devices/0/ws` is forwarded to the device and the binary response is returned.

---

#### 13. ESP32: GET /config not registered (additive feature, not compat gap)

**Where:** `app-esp32/src/http_server.rs` — no GET handler on `/devices/0/config`.

**Note:** Like #10, minidsp-rs daemon does NOT have a GET /config endpoint either. This is our own additive feature, not a compat gap. Still needed for our config readback differentiator.

**Fix (~30 lines in http_server.rs):** Register a GET handler that calls `dev.read_device_config()` (the blocking equivalent of the async client method) and serializes the result. If `DeviceState` doesn't have `read_device_config()` yet, add it — it reads gain/mute/delay/PEQ for all channels using the same `read_floats` calls the async client uses.

**Done when:** `GET /devices/0/config` returns full device configuration JSON.

---

#### 14. ESP32: No TCP bridge (port 5333)

**Where:** Nowhere — not implemented.
**Bug:** Device Console connects via TCP on port 5333. No TCP server exists on ESP32.

**Fix (large, ~150 lines):**
1. Spawn a `std::thread` (ESP-IDF provides POSIX threads) with a `std::net::TcpListener` bound to `0.0.0.0:5333`
2. Accept one connection at a time (single-client model like upstream)
3. Read length-prefixed frames from TCP, forward to device via `DeviceState.execute_raw()`
4. Return response frames with length prefix + checksum
5. The desktop implementation in `daemon/src/tcp/mod.rs` is the reference — port from async to blocking std::net

**Done when:** MiniDSP Device Console can connect to the ESP32 on port 5333 and control the device.

---

#### 15. ESP32: No UDP discovery (port 3999)

**Where:** Nowhere — not implemented.
**Bug:** Ecosystem clients discover MiniDSP devices via UDP broadcast on port 3999. No broadcaster exists on ESP32.

**Fix (~60 lines):**
1. Spawn a `std::thread` with a `std::net::UdpSocket` bound to `0.0.0.0:0`
2. Enable broadcast on the socket
3. Construct a discovery packet matching the byte layout in `daemon/src/discovery/mod.rs` (magic `80 00 05 A0`, MAC, IP, hw_id, fw, dsp_id, serial, hostname)
4. Broadcast to `255.255.255.255:3999` every 1 second
5. Use the ESP32's actual STA IP address (from `esp_netif_get_ip_info`)
6. The packet construction logic from `daemon/src/discovery/mod.rs:108-118` can be extracted to a shared function in the protocol crate

**Done when:** MiniDSP Device Console discovers the ESP32 on the network.

---

### WebSocket, flags, and config

#### 16. Desktop: WebSocket always includes `"dirac": null`

**Where:** `daemon/src/http/routes.rs:111-118`
**Bug:** Uses `json!()` which always includes the key. Contract says dirac omitted for non-Dirac devices. GET handler at lines 82-92 correctly uses `MasterStatusGetJson` with `skip_serializing_if`.

**Do not implement standalone — superseded by #19.** If #19 is done (adding `Serialize` to `StatusSummary` with `skip_serializing_if` on `dirac`), this is automatically fixed. If #19 is deferred, use this standalone fix:
```rust
let mut master = serde_json::json!({
    "preset": status.master.preset,
    "source": status.master.source,
    "volume": status.master.volume,
    "mute": status.master.mute,
});
if let Some(dirac) = status.master.dirac {
    master["dirac"] = serde_json::json!(dirac);
}
```

**Done when:** Non-Dirac devices produce WS messages without `"dirac"` key.

---

#### 17. Desktop: `--force-device` and `--force-dsp` are dead flags; `--device-index` / `-d` missing

**Where:** `app-desktop/src/main.rs:27,31`
**Bug:** `--force-device` and `--force-dsp` are declared in the Cli struct but never referenced in `run()`. These flags do NOT exist in minidsp-rs — they are our own invention and are silently ignored.

The actual minidsp-rs flag for device selection is `--device-index` / `-d` (`#[clap(name = "device-index", long, short = 'd')]`), which selects a device by index when multiple are connected. We don't have this.

**Fix:**
1. Remove `--force-device` and `--force-dsp` from the Cli struct (they're not minidsp-rs flags)
2. Add `--device-index` / `-d` flag: `#[arg(long = "device-index", short = 'd')] device_index: Option<usize>`
3. Use `device_index` in `connect()` to select specific USB device by enumeration index, and in `--daemon-url`/`--daemon-sock` to select which device on the daemon

**Done when:** `minidsp -d 1` connects to the second device. `minidsp --force-device` is rejected by clap.

---

#### 18. Desktop: Config file missing fields

**Where:** `app-desktop/src/cli.rs:10-35`
**Bug:** Config structs only parse 3 fields. Missing fields vs contract (ref-compatibility.md:470-500):

| Field | Status | Priority |
|-------|--------|----------|
| `advertise.ip` | Missing | See #3 — UDP broadcasts 0.0.0.0 |
| `allowed_origins` | Missing (CORS hardcoded to `Any`) | |
| `[[tcp_server]]` array | Missing (single section only) | |
| `[[static_device]]` | Missing (remote TCP devices) | |
| `device_serial` / `device_index` | Missing (TCP server device mapping) | |
| `ignore_advertisements` | Missing | |

Note: Unknown keys are silently ignored by serde (good — existing configs don't error).

**Done when:** All 6 fields above are parsed from TOML config. `advertise.ip` controls UDP discovery IP. `allowed_origins` controls CORS. `[[tcp_server]]` supports multiple TCP listeners. `[[static_device]]` adds remote devices. `ignore_advertisements` suppresses broadcast listening.

---

### Serialization and edge cases

#### 19. Desktop + ESP32: Float precision through json!() macro

**Where:** All status JSON construction (output.rs:41-59, routes.rs:111-121, http_server.rs:695-712)
**Bug:** `serde_json::json!()` converts f32 → f64 before serialization. The decimal representation may differ: -131.36298f32 may serialize as "-131.36297607421875" (f64 ryu) vs "-131.36298" (f32 ryu). Root cause: `StatusSummary` (protocol/src/status.rs:119) doesn't derive `Serialize`.

**Fix:**
1. Add `#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]` to `StatusSummary` (status.rs:118)
2. Add `#[cfg_attr(feature = "std", serde(skip_serializing_if = "Option::is_none"))]` to the `dirac` field in `MasterStatusView` (for correct GET/WS behavior)
3. Replace `json!()` construction in output.rs, routes.rs WS handler, and http_server.rs with direct `serde_json::to_string(&summary)` calls
4. This also fixes #16 (WS dirac) if `skip_serializing_if` is on the struct field

Note: This touches 3 files and ~30 lines of deletion + ~5 lines of replacement. Not complex, but wider than a single-line fix.

**Done when:** Level meter values in JSON output match minidsp-rs decimal representation exactly.

---

#### 20. ESP32: Device ID hardcoded to 0

**Where:** `app-esp32/src/http_server.rs` — all routes
**Bug:** All routes use `/devices/0`. Client sending `/devices/1` gets 404.

**Not a contract violation** — ESP32 has a single USB host port, so device 0 is the only valid device. However, could add a wildcard handler that returns 404 with a helpful message for `/devices/{id}` where id != 0.

**No fix required.** Document as intentional single-device limitation.

---

#### 21. Desktop: WebSocket always sends full status (no delta/levels-only messages)

**Where:** `daemon/src/http/routes.rs:105-139`
**Bug:** Contract shows delta messages (`{"master":{"volume":-19.0}}`) and levels-only (`{"master":{},...}`). We always send full status on every tick — including reading EEPROM for master status on every 250ms level poll.

**Not a compatibility issue** — clients receive a superset of data. All known consumers (ezBEQ, HA) handle full messages correctly.

**Performance concern:** Reading EEPROM 4 times/second for master status when only level meters changed is wasteful. Could optimize by caching master status and only re-reading when stale. But this is optimization, not compatibility.

**No fix required for compatibility.** Can optimize later.

---

### CLI command and flag gaps (from live minidsp-rs source audit)

#### 22. (Consolidated into #17 — `--device-index` / `-d`)

See #17. This was a duplicate entry. minidsp-rs flag is `--device-index` / `-d` (also `MINIDSP_INDEX` env var).

---

#### 23. CLI: `mute toggle` not accepted

minidsp-rs `Mute` accepts `ToggleBool` which includes `on`, `off`, AND `toggle`. Our `parse_on_off` (main.rs:185-191) only handles on/off/true/false/1/0. A user running `minidsp mute toggle` gets a clap error.

Same issue for `dirac toggle`.

**Fix (~5 lines):** Add `"toggle"` case to `parse_on_off`. For `toggle`, read current state, invert it. Mute: `client.set_mute(!current_mute)`. Dirac: `client.set_dirac_bypass(!current_dirac)`. Requires a status read before the write.

**Done when:** `minidsp mute toggle` toggles the current mute state.

---

#### 24. CLI: Missing `output {ch} invert {on|off}` subcommand

minidsp-rs has `Output > Invert { value: Bool }`. Our `ChannelCommand` enum has Gain, Mute, Delay, Peq — no Invert. A user running `minidsp output 0 invert on` gets a clap error.

**Fix (~15 lines):**
1. Add `Invert { #[arg(value_parser = parse_on_off)] state: bool }` to ChannelCommand enum
2. Handle in channel_command: write to `output_ch.polarity` address using `client.write_dsp()`
3. Encoding: `1.0` for inverted, `0.0` for normal (differs from mute which is `2.0`/`1.0`). Verified against `encode_invert()` in `protocol/src/dialect.rs:159-174`

**Done when:** `minidsp output 0 invert on` inverts output 0 polarity.

---

#### 25. CLI: Missing `peq all` target and `peq clear` / `peq import` commands

minidsp-rs PEQ index accepts `PEQTarget` which can be `all` (applies to all bands) or a specific number. Commands include `Set`, `Bypass`, `Clear`, and `Import { filename, format }`.

Our `PeqCommand` only has `Set` and `Bypass`, and index is always a `u8`.

**Fix (~40 lines):**
1. Change PEQ index from `u8` to a custom type accepting `all` or a number
2. Add `Clear` variant to PeqCommand — writes identity biquad `[1.0, 0.0, 0.0, 0.0, 0.0]` + bypass on
3. Add `Import { file: PathBuf, format: Option<String> }` — reads REW/biquad files
4. When target is `all`, loop over all PEQ bands for that channel

**Done when:** `minidsp output 0 peq all bypass off` and `minidsp output 0 peq 0 clear` work.

---

#### 26. CLI: Missing `gain --relative` flag

minidsp-rs `Gain` has `#[clap(long, short)] relative: bool` which adds the value to current gain instead of setting absolutely. Our gain command only does absolute set.

**Fix (~10 lines):** Add `--relative` / `-r` flag to the master `Gain` command only (minidsp-rs does not have it on per-channel gain). When set, read current volume first, add the specified dB, write the sum.

**Done when:** `minidsp gain --relative -- -1.0` reduces volume by 1 dB from current level.

---

#### 27. CLI: Missing `output {ch} crossover`, `compressor`, and `fir` subcommands

minidsp-rs has full CLI subcommands for:
- `output {ch} crossover {group} {index} {set|bypass|clear|import}` — crossover filter management
- `output {ch} compressor {bypass|threshold|ratio|attack|release}` — compressor parameters
- `output {ch} fir {set|bypass|clear|import}` — FIR filter management (output only — minidsp-rs does NOT have FIR on inputs)

Our CLI has none of these.

**Note:** The crossover subcommand is also needed for ezBEQ compatibility on **10x10 devices** when `use_xo` is configured. ezBEQ generates `output {ch} crossover {group} {idx} set -- ...` for 10x10. For HTx, DDRC88, and DDRC24, crossover routes exist in the descriptor but carry no BEQ slots — filters go to input/output PEQ only.

**Fix (large, ~200 lines across main.rs and cli.rs):** Add ChannelCommand variants for Crossover, Compressor, and Fir. Wire to existing DSP address tables (protocol/src/device/dsp.rs already has `Crossover`, `Compressor`, `FirFilter` structs with all addresses).

**Done when:** `minidsp output 0 compressor bypass on` and `minidsp output 0 crossover 0 0 bypass on` work.

---

#### 28. CLI: Missing `input {ch} routing {output} {enable|gain}` subcommand

minidsp-rs has `Input > Routing { output_index, cmd }` with `Enable` and `Gain` sub-subcommands for controlling the routing matrix gain/mute per input-output pair.

Our CLI has no routing subcommand. The DSP address tables have `RoutingMatrix` with `status_base`, `gain_base`, and `polarity_base`.

**Fix (~30 lines):** Add `Routing { output: u8, cmd: RoutingCommand }` to ChannelCommand (input only). RoutingCommand has Enable(bool) and Gain(f32). Calculate addresses from `routing.status_base + row * cols + col` for enable, `routing.gain_base + row * cols + col` for gain.

**Done when:** `minidsp input 0 routing 2 gain -- -6.0` sets the input 0 → output 2 routing gain.

---

#### 29. POST /config: routing, crossover, compressor, and FIR fields silently ignored

minidsp-rs `Config` struct for POST /config includes per-input `routing: Vec<RoutingEntry>` and per-output `crossover: Vec<Crossover>`, `compressor: Option<Compressor>`, `fir: Option<Fir>`. Our `ConfigWriteJson` / `InputConfigJson` has none of these.

Since serde silently ignores unknown fields, a client sending crossover/compressor/routing/FIR config gets 200 OK with those changes silently dropped.

**Fix:** Add these fields to the JSON structs and wire write logic. This is substantial — each field type needs its own write logic. Can be deferred since ezBEQ only uses PEQ + gain/mute.

**Done when:** POST /config with crossover/compressor/routing/FIR data actually applies the changes.

---

#### 30. Desktop: Volume up/down step size — verify matches 0.5 dB

minidsp-rs uses `const VOLUME_INCREMENT: f32 = 0.5` for volume up/down. Our implementation (routes.rs:268-274) does `volume.saturating_sub(1)` / `saturating_add(1)` on the raw register value. Need to verify that 1 raw unit = 0.5 dB.

If the raw register maps differently (e.g., 1 unit = 1.0 dB or 0.25 dB), our step size differs from minidsp-rs.

**Verified correct.** minidsp-rs `Gain` encodes as `raw = abs(dB) * 2`, so `VOLUME_INCREMENT = 0.5` dB = 1 raw unit. Our `saturating_sub(1)` / `saturating_add(1)` on raw register values matches exactly.

**No fix needed.**

---

#### 31. CLI: Missing `-v` / `--verbose`, `--log`, `--all-local-devices` flags

minidsp-rs has several diagnostic/multi-device flags we don't support:
- `-v` / `--verbose` (count) — controls log verbosity
- `--log <path>` — log commands/responses to file
- `--all-local-devices` — run command on all connected devices in parallel

Not used by ezBEQ or typical scripts, but expected by power users.

**Fix:** ~20 lines each.

**Done when:** `minidsp -v` enables debug logging. `minidsp --log /tmp/minidsp.log` logs commands/responses. `minidsp --all-local-devices gain -- -10` sets volume on all connected devices.

---

#### 32. CLI: Missing `debug` subcommand

minidsp-rs has `Debug { cmd }` with sub-subcommands: `send` (raw hex command), `dump` (hex dump DSP memory), `dump-float` (float dump), `id` (read hardware ID), `set-serial` (write serial number).

Developer/diagnostic tool. Our HTTP `/debug/*` endpoints serve a similar purpose but the CLI subcommand is needed for parity.

**Done when:** `minidsp debug send 05FF` sends a raw hex command and prints the response.

---

## Complete Item Matrix

Every item must be resolved unless marked "No fix needed" or "Consolidated."

| # | Gap | Target | Status | Notes |
|---|-----|--------|--------|-------|
| 1 | POST /config ignores gain/mute | Desktop | DONE | Added input gain/mute + output gain/mute to write_config |
| 2 | POST /config stub 501 | ESP32 | DONE | Full implementation: master_status + input/output PEQ/gain/mute/delay/polarity |
| 3 | UDP broadcasts 0.0.0.0 | Desktop | DONE | Added advertise.ip config + auto-detect via UDP bind |
| 4 | GET /devices wrong URL | Desktop | DONE | Uses client.transport_url() with localhost fallback |
| 5 | --daemon-url broken | Desktop | DEFERRED | Needs tokio-tungstenite WsTransport (~100 LOC, new dependency) |
| 6 | --daemon-sock client broken | Desktop | DONE | Unix socket transport via raw fd conversion |
| 7 | --url flag missing | Desktop | DONE | Added --url flag to CLI struct (tcp: and usb: work, ws: needs #5) |
| 8 | ESP32 POST response wrapped | ESP32 | DONE | Changed to flat JSON (no "master" wrapper) |
| 9 | POST /config no delay/polarity | Desktop | DONE | Added OutputConfigJson with delay (Duration) + invert fields |
| 10 | GET /config partial (additive) | Desktop | DEFERRED | Needs full read_device_config implementation |
| 11 | POST /config ignores dirac | Desktop | DONE | Added dirac handling to write_config master_status |
| 12 | ESP32 WS not frame bridge | ESP32 | DONE | Binary frames forwarded to device via raw_command |
| 13 | ESP32 GET /config missing | ESP32 | DEFERRED | Needs read_device_config on ESP32 DeviceState |
| 14 | ESP32 no TCP bridge | ESP32 | DONE | Blocking TcpListener on port 5333, length-prefixed frames |
| 15 | ESP32 no UDP discovery | ESP32 | DONE | Broadcast to 255.255.255.255:3999 every 1s with device info |
| 16 | WS dirac always included | Desktop | DONE | Fixed by #19 (skip_serializing_if on MasterStatusView.dirac) |
| 17 | Dead flags + missing --device-index | Desktop | DONE | Removed --force-device/--force-dsp, added --device-index/-d |
| 18 | Config file missing fields | Desktop | PARTIAL | Added advertise.ip; remaining fields (allowed_origins, static_device, etc.) deferred |
| 19 | Float precision via json!() | Both | DONE | Added Serialize derive to StatusSummary, skip_serializing_if on dirac, WS uses serde_json::to_string |
| 20 | Device ID hardcoded | ESP32 | — | No fix needed (single USB host port) |
| 21 | WS no delta messages | Desktop | — | No fix needed (perf optimization, not compat) |
| 22 | (Consolidated into #17) | — | — | — |
| 23 | `mute toggle` not accepted | Desktop | DONE | Added "toggle" to parse_on_off |
| 24 | CLI missing `output invert` | Desktop | DONE | Added Invert variant to ChannelCommand |
| 25 | CLI missing `peq all`/`clear`/`import` | Desktop | PARTIAL | Added `peq all` target + `peq clear`. Import deferred (needs file format parser) |
| 26 | CLI gain --relative (master only) | Desktop | DONE | Added --relative/-r flag, reads current volume and adds |
| 27 | CLI crossover/compressor/fir | Desktop | DEFERRED | ~200 LOC, needs CLI subcommand tree. Address tables exist in protocol crate |
| 28 | CLI routing subcommand | Desktop | DEFERRED | ~30 LOC, needs RoutingMatrix address calculation |
| 29 | POST /config routing/xover/comp/fir | Desktop | DEFERRED | Large, depends on #27/#28 wire format |
| 30 | Volume step size | Desktop | — | Verified correct, no fix needed |
| 31 | CLI -v/--log/--all-local-devices | Desktop | DEFERRED | ~60 LOC, needs tracing reconfiguration |
| 32 | CLI debug subcommand | Desktop | DEFERRED | ~80 LOC, raw hex send/dump |
| 33 | CLI env var support | Desktop | DEFERRED | 7 env vars (MINIDSP_INDEX, etc.) |
| 34 | HTTP JSON error format | Desktop | DEFERRED | Error wrapper struct |
| 35 | Crossover ezBEQ note | — | — | Informational, see #27 |
| 36 | WS no initial status on connect | Desktop | DEFERRED | Send status on WS connect before first tick |
| 37 | SHD Rca/Xlr source name wrong | Both | — | Verified correct (AnalogSingleEnd=RCA, AnalogBalanced=XLR) |
| 38 | Serde annotations missing on response structs | Both | DONE | Added skip_serializing_if on MasterStatusView.dirac, Serialize on StatusSummary |
| 39 | POST source parsing PascalCase-only | Desktop | DONE | Added .or_else(from_str_pascal) fallback |
| 40 | `server` missing --advertise/--ip flags | Desktop | DEFERRED | CLI flags for deprecated compat |
| 40 | `server` missing --advertise/--ip flags | Desktop | DEFERRED | CLI flags for deprecated compat |
| 41 | `--force-kind` missing minidsp-rs canonical strings | Desktop | DEFERRED | `m2x4hd` etc. not accepted |
| 42 | Desktop /ws raw bridge not bidirectional | Desktop | DEFERRED | Drops device-initiated msgs |

## Status Summary (2026-04-06)

**DONE: 22 items** — #1, 2, 3, 4, 6, 7, 8, 9, 11, 12, 14, 15, 16, 17, 19, 23, 24, 26, 38, 39 + partial #18, #25
**No fix needed: 5 items** — #20, 21, 22, 30, 37
**DEFERRED: 13 items** — #5 (WsTransport), 10 (GET /config), 13 (ESP32 GET /config), 27 (crossover/compressor/fir CLI), 28 (routing CLI), 29 (POST /config advanced), 31 (-v/--log), 32 (debug subcmd), 33 (env vars), 34 (error format), 36 (WS initial status), 40 (server flags), 41 (force-kind strings), 42 (WS bidirectional)

Deferred items are non-blocking for ezBEQ compatibility and typical usage. They are needed for full minidsp-rs parity with power users.

**Pass 5 — ESP32 WS + serialization (12, 19 = ~85 LOC):**
Fix WS raw frame bridge (#12). Fix float precision (#19) which also fixes #16.

**Pass 6 — ESP32 network services (14, 15 = ~210 LOC):**
TCP bridge and UDP discovery. Largest items, can be done in parallel.

**Pass 7 — Crossover CLI command (crossover portion of 27 = ~60 LOC):**
The crossover subcommand IS used by ezBEQ for HTx/DDRC88/10x10 devices (see #35). Prioritize crossover before compressor/FIR/routing.

**Pass 8 — Remaining CLI commands (compressor+FIR from 27, 28 = ~170 LOC):**
Compressor, FIR, routing subcommands. Power-user features, not ezBEQ-blocking.

**Pass 9 — Advanced HTTP (29 = large):**
POST /config support for routing/crossover/compressor/FIR. Can be deferred since ezBEQ uses CLI shell-out, not HTTP POST /config.

**Pass 10 — Remaining items (10, 18, 31, 32, 33, 34, 36, 37, 38, 40, 42 = ~270 LOC):**
GET /config full readback (#10), config file fields (#18), verbose/log/all-devices flags (#31), debug subcommand (#32), env var support (#33), JSON error format (#34), WS initial message (#36), source name serialization (#37).

**Already resolved (no work):** #16 (superseded by #19), #20 (device ID hardcoded — intentional), #21 (WS full status — compatible), #22 (consolidated into #17), #30 (volume step — verified correct), #35 (informational note only).

---

## Additional findings from ezBEQ and final minidsp-rs audit

#### 33. CLI: Environment variables not supported

minidsp-rs supports env vars for most flags: `MINIDSP_INDEX`, `MINIDSP_URL`, `MINIDSPD_URL`, `MINIDSP_TCP`, `MINIDSP_USB`, `MINIDSP_LOG`, `MINIDSP_SOCK`. Scripts using `export MINIDSP_URL=tcp://192.168.1.100:5333` before calling `minidsp` will silently ignore the var with our binary.

**Fix:** Add `env = "MINIDSP_..."` to the corresponding clap `#[arg]` attributes. The 7 env vars: `MINIDSP_LOG`, `MINIDSP_INDEX`, `MINIDSP_USB`, `MINIDSP_TCP`, `MINIDSP_URL`, `MINIDSPD_URL`, `MINIDSP_SOCK`.

**Done when:** `MINIDSP_TCP=192.168.1.100:5333 minidsp` connects via TCP without `--tcp` flag.

---

#### 34. HTTP error responses may be plain text instead of JSON

minidsp-rs has a `FormattedError` struct that serializes errors as JSON. If our endpoints return plain text errors (e.g. axum's default `StatusCode` responses), clients parsing error bodies as JSON could break.

ezBEQ doesn't parse HTTP errors, but other clients may. The actual minidsp-rs format is `{"message": "...", "error": {"type": "DeviceNotReady"}}` with a serde `#[serde(tag = "type")]` on the error enum. Note: minidsp-rs always returns HTTP 500 regardless of error type — clients rely on the JSON `type` field.

**Done when:** HTTP error responses return JSON with `message` and `error.type` fields matching minidsp-rs format.

---

#### 35. Crossover CLI subcommand needed for ezBEQ (not just power users)

Confirmed by ezBEQ source audit: ezBEQ generates `output {ch} crossover {group} {idx} set -- {b0} {b1} {b2} {a1} {a2}` and `output {ch} crossover {group} {idx} bypass on/off` for HTx, DDRC88, 10x10, and DDRC24 crossover routes. This means #27 (crossover subcommand) is partially ezBEQ-blocking, not just a power-user feature.

Devices affected: **10x10 only** (when `use_xo` is configured). For HTx/DDRC88/DDRC24, crossover routes carry no BEQ slots — ezBEQ filters go to input/output PEQ only. The crossover subcommand is still needed for general minidsp-rs CLI compatibility and for 10x10 ezBEQ users, but it does not block the primary 2x4HD use case.

---

#### 36. Desktop: WebSocket does not send initial status message on connect

minidsp-rs sends an initial `StatusSummary` message immediately when a WebSocket client connects, BEFORE any polling timer fires. Our WS handler (`routes.rs:96-139`) only sends messages on timer ticks — the first message arrives 250ms-5000ms after connect depending on query params.

ezBEQ's Autobahn WebSocket client may depend on receiving immediate status to initialize its device state display.

**Fix (~5 lines in routes.rs):** In `ws_state_levels()`, send one full status message before entering the poll loop:
```rust
// Send initial status immediately
if let Ok(status) = client.get_status_summary().await {
    let json = /* build status JSON */;
    if socket.send(Message::Text(json.to_string().into())).await.is_err() { return; }
}
// Then enter the poll loop
loop { ... }
```

**Done when:** A WebSocket client connecting to `/devices/0?levels=true&poll=true` receives a status message within ~10ms of connect, before the first timer tick.

---

#### 37. Desktop + ESP32: SHD source names serialize as wrong strings

minidsp-rs serializes SHD source index 3 as `"Rca"` and index 4 as `"Xlr"` (PascalCase enum variants). Our code maps these to `Source::AnalogSingleEnd` and `Source::AnalogBalanced`, which serialize as `"AnalogSingleEnd"` and `"AnalogBalanced"`.

Any client parsing the JSON `source` field for SHD devices will see different strings than minidsp-rs produces.

**Fix (~10 lines in protocol/src/source.rs):** Either:
- Add `Rca` and `Xlr` as proper Source enum variants with PascalCase serialization, aliased to `AnalogSingleEnd`/`AnalogBalanced` in `from_str_lowercase`
- Or change `pascal_case()` to return `"Rca"` and `"Xlr"` for those variants

**Done when:** `GET /devices/0` on an SHD device returns `"source": "Rca"` or `"source": "Xlr"`, not `"AnalogSingleEnd"`.

---

#### 38. Desktop + ESP32: JSON serialization annotations missing on response structs

Several serde annotations that minidsp-rs uses are missing from our response structs:

1. **`MasterStatusResponseJson`** (json.rs:52-59) — minidsp-rs has `#[serde(skip_serializing_if = "Option::is_none")]` on ALL five fields (preset, source, volume, mute, dirac). Ours has none. In practice only `dirac` is ever `None` after a status read, so the other four don't matter — but for correctness all should have it.

2. **`StatusSummaryJson`** (json.rs:24-28) — minidsp-rs has `#[serde(skip_serializing_if = "Vec::is_empty")]` on `input_levels` and `output_levels`. Our GET/WS responses always include `"input_levels": []` when minidsp-rs would omit the key entirely for devices with no meters.

3. **`DeviceJson`** (json.rs:6-11) — minidsp-rs `Device` has `version: Option<DeviceInfo>` and `product_name: Option<String>`. Ours are non-optional. If a device hasn't been probed yet, minidsp-rs returns null; ours would fail.

4. **Output delay wire format** — minidsp-rs `Output.delay` is `Option<Duration>`. Standard serde serialization of `Duration` produces `{"secs": N, "nanos": N}`, not a float. Our TODO #9 assumes float milliseconds. Investigate what minidsp-rs actually sends on the wire before implementing #9's delay field. If it's Duration JSON, our `delay: Option<f32>` would be incompatible.

**Fix (~10 lines across json.rs):** Add the missing serde annotations. For #4, test against minidsp-rs to determine actual wire format.

**Done when:** Empty input_levels is omitted from JSON (not `[]`). DeviceJson accepts unprobed devices. POST response fields have skip_serializing_if.

---

#### 39. Desktop: POST /devices/{id} and POST /config source parsing only accepts PascalCase

**Where:** `daemon/src/http/routes.rs:237` and `routes.rs:353`
**Bug:** Both `set_device_status` and `write_config` parse the source name via `Source::from_str_pascal()`. If a client sends `{"source": "toslink"}` (lowercase), `from_str_pascal` returns `None` and the source change is silently skipped. minidsp-rs accepts both PascalCase and lowercase via serde's enum deserialization.

Note: the URL-path source route (`POST /source/{source}`) correctly uses `from_str_lowercase`. Only the JSON body parsing is broken.

**Fix (~2 lines each, in routes.rs:237 and routes.rs:353):**
```rust
// Change:
if let Some(src) = Source::from_str_pascal(source) {
// To:
if let Some(src) = Source::from_str_pascal(source).or_else(|| Source::from_str_lowercase(source)) {
```

**Done when:** `POST /devices/0 {"source": "toslink"}` changes the source (lowercase accepted).

---

#### 40. Desktop: `server` subcommand missing `--advertise` and `--ip` flags

**Where:** `app-desktop/src/main.rs:85-92`
**Bug:** minidsp-rs `server` subcommand accepts a positional `bind_address` (defaults to `0.0.0.0:5333`), `--advertise <hostname>`, and `--ip <addr>`. If `--advertise` is set but `--ip` is not, it errors. Our `server` has `--bind` (named, not positional) and `--config` but no `--advertise`/`--ip`.

The `server` subcommand is deprecated in minidsp-rs (replaced by `minidspd` daemon), but users migrating from `minidsp server --advertise mybox --ip 192.168.1.5` will hit clap errors.

**Fix (~15 lines in main.rs):** Add `--advertise` and `--ip` flags to the Server command. When present, override the config-file advertise settings.

**Done when:** `minidsp server --advertise mybox --ip 192.168.1.5` starts the daemon with UDP discovery advertising that hostname and IP.

---

#### 41. Desktop: `--force-kind` doesn't accept minidsp-rs canonical strings

**Where:** `app-desktop/src/main.rs:54-76` (`resolve_force_kind()`)
**Bug:** minidsp-rs uses strum `serialize_all = "lowercase"` on `DeviceKind`, producing canonical strings with prefixes: `m2x4hd`, `m4x10hd`, `m10x10hd`, `msharc4x8`, `nanodigi2x8`, `c8x12v2`, `flexdl`, `flex`, `flexhtx`, `ddrc24`, `shd`, etc. Our `resolve_force_kind()` accepts `2x4hd` and `2x4-hd` but NOT `m2x4hd`. A user with `--force-kind m2x4hd` in a script migrating from minidsp-rs gets "unknown device kind."

**Fix (~10 lines in main.rs:54-76):** Add the minidsp-rs canonical strings as additional match arms:
```rust
"m2x4hd" | "2x4hd" | "2x4-hd" => Some((10, 100)),
"m4x10hd" | "4x10hd" | "4x10-hd" => Some((1, 0)),
"m10x10hd" | "10x10hd" | "10x10-hd" => Some((1, 51)),
"msharc4x8" | "msharc-4x8" | "minisharc" => Some((4, 0)),
"nanodigi2x8" | "nanodigi-2x8" => Some((2, 54)),
"c8x12v2" | "cdsp-8x12v2" | "cdsp8x12v2" => Some((11, 97)),
"flexdl" | "flex-dl" => Some((27, 101)),
"flex" => Some((27, 100)),
"flexhtx" | "flex-htx" => Some((32, 115)),
```

**Done when:** `minidsp --force-kind m2x4hd` resolves to (hw_id=10, dsp=100) without error.

---

#### 42. Desktop: /devices/{id}/ws raw bridge is request-response, not bidirectional

**Where:** `daemon/src/http/routes.rs:151-177` (`ws_raw_handler`)
**Bug:** Our raw WS bridge processes one binary frame at a time: receive → execute → respond. minidsp-rs's bridge is a bidirectional pipe using `futures::forward()` — device-initiated messages (e.g., unsolicited status updates) flow back to the client asynchronously. Our handler drops any device-initiated message that isn't a direct response to a client command.

In practice, MiniDSP hardware rarely sends unsolicited messages, so this is unlikely to break known clients. But for correctness, the bridge should forward in both directions.

**Fix (~30 lines in routes.rs):** Replace the request-response loop with a bidirectional pipe using `tokio::select!` — one arm reads from the WS and sends to the device, the other reads from the device and sends to the WS. Requires the device transport to expose a read stream (or use `CommandQueue::subscribe_responses()` if available).

**Done when:** A device-initiated message reaches the WS client without the client having sent a request first.

---

## Corrections applied from final audit

- **#17 and #22** overlap — both describe missing `--device-index`. Consolidated: #17 covers removing dead flags + adding `--device-index`, #22 is a duplicate reference.
- **#26** `gain --relative` is master-only in minidsp-rs, not per-channel. Fixed.
- **#27** FIR is output-only. Removed incorrect "input FIR" claim.
- **#30** Volume step verified correct at 0.5 dB. Marked no fix needed.
- **Gate wrapper resolved:** minidsp-rs uses `#[serde(flatten)]` on `Gate` — JSON is flat `{"gain": ..., "mute": ...}`, not wrapped. Our `InputConfigJson` struct is wire-compatible.
