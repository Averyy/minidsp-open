# ESP32 Logging & Diagnostics Architecture

Research findings and concrete recommendation for minidsp-open ESP32-S3 target.

## 1. ESP-IDF Logging Facilities

### esp_log module

ESP-IDF provides a built-in logging library with two versions (V1 default, V2 for reduced flash/improved flexibility). Key features:

- **Five log levels**: ERROR, WARN, INFO, DEBUG, VERBOSE (maps cleanly to Rust `log` crate's Error/Warn/Info/Debug/Trace)
- **Per-tag filtering**: `esp_log_level_set("tag", level)` controls verbosity per component at runtime
- **Compile-time filtering**: `CONFIG_LOG_MAXIMUM_LEVEL` strips higher-verbosity log calls from the binary entirely, reducing both flash and RAM usage
- **Color output**: Terminal color codes for UART output (configurable via `CONFIG_LOG_COLORS`)
- **Timestamp sources**: RTOS tick counter or system time

### Programmatic log capture via `esp_log_set_vprintf`

ESP-IDF exposes `esp_log_set_vprintf()` to redirect all log output to a custom function. This is the key mechanism for capturing logs into a ring buffer instead of (or in addition to) UART.

**Caveat**: The callback receives a C `va_list` and format string. On Xtensa (ESP32-S3), the `va_list` type is `[u32; 3]` in the bindgen output, and Rust's `core::ffi::VaList` support is still nightly-only with known issues on the Xtensa fork. This makes direct `esp_log_set_vprintf` interception from Rust awkward.

**Caveat**: The callback runs in the caller's context (any task, any priority). Doing blocking I/O (flash writes, network sends) from within the callback **will deadlock** or trigger watchdog resets. The ESP-IDF GitHub issue #12967 documents system hangs when writing to flash from the vprintf callback. The safe pattern is: write to a lock-free ring buffer in the callback, drain the ring buffer from a dedicated task.

### Rust `log` crate integration via esp-idf-svc

`esp-idf-svc` (v0.53) provides `EspIdfLogger` which implements the Rust `log::Log` trait:

- Writes directly to stdout (same output as native ESP-IDF logging, with matching format and colors)
- `EspIdfLogFilter` backend respects ESP-IDF's per-tag log level configuration
- Alternatively, `init_from_env()` uses the `RUST_LOG` compile-time env var
- Level mapping: `log::Level::Error` <-> `ESP_LOG_ERROR`, etc., with `Trace` <-> `ESP_LOG_VERBOSE`

**This means all Rust `log::info!()` / `log::error!()` calls go through ESP-IDF's logging infrastructure and appear on UART.** The integration is clean and well-tested.

### `tracing` crate on ESP32

Not practical. `tracing` is significantly heavier than `log` (subscriber infrastructure, span storage, dynamic dispatch). The `esp-idf-svc` ecosystem standardizes on the `log` crate. For our use case (structured diagnostic messages, not distributed tracing), `log` is the right choice.

## 2. How Other ESP32 Projects Handle Remote Diagnostics

### ESPHome

ESPHome uses **Server-Sent Events (SSE)** at `/events` for real-time log streaming:

- Log messages are emitted as SSE events with type `log`, containing the formatted message, level (as integer), and tag
- The web UI subscribes to this SSE endpoint and renders logs in a console panel
- Logger component has a configurable `tx_buffer_size` (default 512B) and `task_log_buffer_size` (default 768B) -- the latter is a thread-safe ring buffer for cross-task log messages
- Log levels configurable per-tag at both compile time and runtime
- Logs also forwarded over MQTT if configured (the `on_message` automation trigger fires for each log line)
- **Key insight**: ESPHome does NOT use WebSockets for logs. SSE is strictly simpler (unidirectional, no frame overhead, auto-reconnect built into the browser EventSource API)

### Tasmota

Tasmota provides multiple simultaneous log destinations, each with independent log levels:

- `SerialLog` (UART, level 0-4, auto-disables 10 minutes after boot)
- `WebLog` (web console at `/cs?`, level 0-4, ring buffer in RAM shown on page load + live updates)
- `MqttLog` (publish log lines to MQTT topic, level 0-4)
- `SysLog` (UDP syslog to configurable host:port, level 0-4)

Tasmota's `WebLog` uses a simple ring buffer in RAM (typically 4KB). The web console page polls or streams from it. The key architectural point: **every destination is independently leveled**. You can have ERROR-only on syslog, DEBUG on serial, and INFO on web.

### WLED

WLED exposes a `/json/info` endpoint that returns system diagnostics as JSON:

```json
{
  "ver": "0.14.0",
  "freeheap": 134264,
  "uptime": 17985,
  "wifi": { "signal": 82, "channel": 6, "bssid": "..." },
  "ws": 2,
  "arch": "esp32",
  "core": "2_0_14"
}
```

WLED also supports WebSocket connections at `/ws` for real-time state updates, but does NOT stream debug logs over the network. Debug logging is UART-only. The `/json/info` endpoint is the sole remote diagnostic tool.

### Common patterns across all three

| Feature | ESPHome | Tasmota | WLED |
|---------|---------|---------|------|
| UART logging | Yes (default) | Yes (auto-disables after 10min) | Yes |
| Web log streaming | SSE at `/events` | Ring buffer at web console | No |
| WebSocket logs | No | No | No (WS used for state, not logs) |
| MQTT logging | Yes (optional) | Yes (MqttLog) | No |
| Syslog UDP | No | Yes (SysLog) | No |
| System info endpoint | Via SSE state events | `Status 0` command | `/json/info` |
| Independent log levels per output | Yes (per-tag) | Yes (per-destination) | N/A |

**Consensus**: SSE or ring-buffer-over-HTTP for log streaming. WebSockets used for bidirectional state control, NOT for log streaming. Syslog is power-user territory.

## 3. Design Options Analysis for minidsp-open ESP32

### Option A: Ring buffer in RAM + HTTP endpoint

**How it works**: Custom `log` backend writes formatted log lines into a fixed-size circular buffer in RAM. `GET /debug/log` returns the buffer contents. `GET /debug/log?follow=true` could use SSE for live streaming.

**RAM budget**:
- ESP32-S3 has 512KB SRAM total. With WiFi + TLS + picoserve + USB host, expect ~150-200KB free heap at runtime (based on typical ESP32-S3 applications with WiFi active)
- **4KB ring buffer**: Holds ~40-80 log lines (average 50-100 bytes each). Enough to capture the last few minutes of activity at INFO level. Reasonable and safe.
- **8KB ring buffer**: Holds ~80-160 lines. More breathing room for DEBUG level. Still well within budget.
- **16KB ring buffer**: Excessive. That's 3-5% of total SRAM for logs.
- **512B**: Too small. One burst of USB reconnection logs would fill it.
- **Recommendation: 4KB default, compile-time configurable up to 8KB.**

**Format**: Plain text with structured prefix, NOT JSON lines. Reasoning:
- JSON per line costs 40-80 bytes of overhead per message (`{"ts":12345,"lvl":"INFO","tag":"usb","msg":"..."}`)
- Plain text with prefix is ~15 bytes overhead: `I (12345) usb: connected`
- This matches ESP-IDF's native log format, so UART and HTTP logs look identical
- A structured `/debug/info` endpoint (see below) provides JSON diagnostics separately
- JSON lines would roughly halve the effective capacity of the ring buffer

**Implementation**: Lock-free SPSC ring buffer (single producer = log backend, single consumer = HTTP handler that snapshots the buffer). Or a simple `Mutex<heapless::Deque<u8, 4096>>` since log writes are fast and contention is minimal.

### Option B: SSE log streaming at `/debug/events`

**How it works**: Client opens an SSE connection, receives log lines as they happen in real time.

**picoserve support**: picoserve has first-class SSE support via `response::sse::EventStream` and `EventSource` trait. The implementation is proven and works on embassy/ESP32.

**Resource cost**:
- One SSE connection = one TCP socket held open. picoserve uses ~2KB buffer per connection.
- No additional RAM beyond the connection buffer -- events are written directly to the socket as they occur.
- The connection holds a TCP slot. With picoserve typically configured for 2-4 concurrent connections, one SSE client reduces available HTTP connections by one.

**Practical with 150-200KB free RAM?** Yes, easily. The cost is the TCP connection buffer (~2KB), not a log buffer. But it requires at least one listener to be connected to see anything -- no history is available if nobody is listening when the interesting event happens.

**Verdict**: Excellent complement to Option A, not a replacement. SSE for live monitoring, ring buffer for after-the-fact diagnosis.

### Option C: Syslog over UDP

**Standard**: RFC 5424 syslog over UDP port 514. One UDP packet per log message, fire-and-forget.

**Pros**:
- Works with any syslog server (rsyslog, syslog-ng, Home Assistant add-ons, Graylog)
- Zero connection overhead (UDP is stateless)
- Standard protocol that monitoring tools understand natively

**Cons**:
- Requires configuration (syslog server IP/port)
- UDP is unreliable -- messages can be lost, especially on busy WiFi
- Adds ~500 bytes of code for syslog packet formatting
- Most minidsp-open users will not have a syslog server

**Verdict**: Phase 2 feature. Useful for advanced users and HA integrations, but not worth the complexity for initial release. The HTTP log endpoint covers 95% of use cases.

### Option D: USB serial output (UART)

**Always available**: ESP32-S3 has USB-Serial-JTAG on the built-in USB port. Logs appear on the USB-C port used for programming/flashing.

**Limitations**: Requires physical USB access. On a deployed device (plugged into MiniDSP, mounted behind a rack), this is impractical. Also, on ESP32-S3, the USB-Serial-JTAG port is shared with the USB OTG port in some configurations -- need to verify pin assignment doesn't conflict with the USB host mode used for MiniDSP.

**Verdict**: Essential for development, useless for deployed devices. Keep UART logging enabled always (the `esp-idf-svc` logger does this by default). This is the baseline, not the solution.

### Option E: WebSocket streaming at `/debug/ws`

**picoserve support**: Full WebSocket support via `response::ws::WebSocketUpgrade`. Supports text and binary frames.

**Compared to SSE**:
- WebSocket is bidirectional (unnecessary for log streaming)
- Higher per-frame overhead (2-14 byte header + masking)
- More complex client code (no browser auto-reconnect like EventSource)
- Same TCP connection cost as SSE

**Verdict**: No advantage over SSE for log streaming. Use WebSocket only if we need bidirectional communication (e.g., interactive debug shell in the future). SSE is strictly better for one-way log streaming.

## 4. Diagnostic Information Priority

Ranked by usefulness for diagnosing MiniDSP controller issues:

### Must-have (always logged at INFO or above)

1. **USB HID connection lifecycle**: connect, disconnect, reconnect attempts, errors, device identification (hw_id, serial). This is the #1 source of user issues.
2. **WiFi state changes**: connected, disconnected, IP obtained, signal strength. Users need to know if WiFi dropped.
3. **Command errors**: Any non-success response from the MiniDSP device (invalid command, busy, etc.)
4. **Boot/startup sequence**: Firmware version, device found/not found, configuration loaded.
5. **OTA update status**: Download started, progress, success/failure.

### Important (logged at DEBUG)

6. **Command timing**: Round-trip time for USB HID commands. Useful for diagnosing latency issues.
7. **Auto-reconnect details**: Backoff timing, retry count, success after N attempts.
8. **HTTP request summary**: Method, path, status code, duration. Not every request, but errors and slow ones.

### Nice-to-have (logged at TRACE, compile-out in release by default)

9. **Raw HID packet dumps**: The actual 65-byte USB HID reports sent/received. Essential for protocol debugging, huge volume.
10. **WiFi driver internals**: RSSI changes, roaming events, DHCP details.

### System diagnostics (separate JSON endpoint, not log lines)

These are point-in-time metrics, not log events. Serve at `GET /debug/info`:

```json
{
  "firmware": "0.2.0",
  "uptime_secs": 883,
  "heap_free": 4047663,
  "heap_min": 3966155,
  "internal_heap": { "free": 37571, "min": 33499, "largest_block": 19456 },
  "task_stacks": [{ "name": "httpd", "free_bytes": 5208 }, "..."],
  "wifi": {
    "connected": true,
    "ssid": "HomeNetwork",
    "rssi": -52,
    "ip": "192.168.1.100",
    "channel": 6,
    "lastWifiDisconnectReason": 0
  },
  "usb": { "connected": true, "device": "2x4HD", "hw_id": 10, "serial": 900123 },
  "log_level": "info",
  "log_buffer_used": 2847,
  "log_buffer_size": 4096
}
```

`heap_*` covers all heaps, PSRAM included. `internal_heap` is internal RAM
only. That's the number to watch: WiFi, lwIP and DMA need it, and
`CONFIG_SPIRAM_MALLOC_ALWAYSINTERNAL` sends every allocation of 16 KB or less
there first. `task_stacks` gives each FreeRTOS task's stack high-water mark
(bytes never touched).

`POST /debug/heap/reset-min` restarts the internal `min`, so the cost of one
request shows up on its own: reset, make the request, then read
`/debug/info`. At debug log level, the ezBEQ `/ws` handler also logs internal
RAM at each step of a new connection.

Leaks are found with the heap-tracing profile:

```bash
CARGO_TARGET_DIR=target-heaptrace \
ESP_IDF_SDKCONFIG_DEFAULTS="sdkconfig.defaults;sdkconfig.heaptrace.defaults" \
tools/esp-cargo build --release
```

The separate target directory keeps the normal build from a full rebuild, and
it has to sit directly in `app-esp32/`. esp-idf-sys treats the target
directory's parent as the project: it runs `cargo metadata` there to find the
extra components, and resolves the sdkconfig defaults and `partitions.csv`
against it. Anywhere else, the build loses LittleFS, USB host and the
WebSocket server. The profile
adds `POST /debug/heaptrace/start` and `POST /debug/heaptrace/stop`. Stop logs
each internal allocation still held since start, with its call stack, to the
console and `/debug/log`. Decode the addresses with
`xtensa-esp32s3-elf-addr2line -pfiaC -e <elf>`.

For updater-specific diagnostics, the ESP32 also exposes `GET /debug/updater` with:

```json
{
  "baseUrl": "https://averyy.github.io/minidsp-open",
  "installedVersion": "9e063dff704eff2519317e737cde6d346ff2da06+ui2.6.0",
  "hasCatalogueData": true,
  "isUpdating": false
}
```

And `POST /debug/updater/check` returns one of:

- `{"status":"current"}`
- `{"status":"updated","version":"...","files":33}`
- `{"status":"no_storage"}`
- `{"status":"error","error":"..."}`
- HTTP `409` with `{"status":"busy"}` if another update is already running

This endpoint is cheap (no buffering, just read current values) and extremely useful for remote diagnosis. WLED's `/json/info` pattern.

## 5. Production vs Debug Builds

### Compile-time filtering: use `CONFIG_LOG_MAXIMUM_LEVEL`

ESP-IDF's compile-time log level (`CONFIG_LOG_MAXIMUM_LEVEL`) causes the compiler to entirely eliminate log calls above that level. This is the right approach:

- **Release builds**: Set `CONFIG_LOG_MAXIMUM_LEVEL` to `INFO`. All `DEBUG` and `VERBOSE`/`TRACE` log sites are compiled out -- zero flash cost, zero runtime cost, no conditional branches.
- **Debug builds**: Set `CONFIG_LOG_MAXIMUM_LEVEL` to `DEBUG` or `VERBOSE`.

In Rust, the `log` crate has a similar feature: `log/max_level_info` feature flag compiles out `debug!()` and `trace!()` calls. Use this in addition to the ESP-IDF setting.

### Runtime level adjustment

Even in release builds (max compiled level = INFO), allow runtime adjustment within the compiled range:

- `POST /debug/level` with body `{"level": "warn"}` to reduce noise
- `POST /debug/level` with body `{"level": "info"}` to restore default
- Cannot set to DEBUG/TRACE in release builds since those calls are compiled out

### RAM cost of log infrastructure in release builds

- Ring buffer: 4096 bytes (the buffer itself, always allocated)
- Log backend struct: ~100 bytes (mutex, write pointer, level config)
- SSE support: 0 bytes unless a client is connected (then ~2KB for the TCP connection buffer)
- `/debug/info` endpoint: 0 bytes persistent (generates JSON on demand from live values)
- **Total persistent cost: ~4.2KB**. Entirely reasonable.

### Do NOT compile out the log infrastructure in release

Stripping the ring buffer and HTTP endpoints from release builds would save ~4KB RAM but make deployed devices completely opaque. The entire point is diagnosing production issues. Every shipped firmware should include the diagnostic endpoints.

## 6. Concrete Recommendation

### Architecture

```
                    +-------------------+
  Rust log::info!() |                   |
  log::error!()  -->| DiagnosticsLogger |---> UART (always, via EspIdfLogger)
  log::warn!()      | (log::Log impl)   |---> Ring buffer (4KB, circular)
                    +-------------------+
                             |
                    +--------v----------+
                    | Ring Buffer (4KB) |
                    | heapless circular |
                    +-------------------+
                       |            |
            +----------+     +------+--------+
            |                |               |
     GET /debug/log    GET /debug/events   GET /debug/info
     (snapshot dump)   (SSE live stream)   (JSON system info)
```

### Implementation plan

**Phase 1 (MVP -- ship with v0.1)**:

1. **Custom `log::Log` implementation** (`DiagnosticsLogger`) that:
   - Formats each log line as plain text: `E (12345) usb: device disconnected`
   - Writes to a `Mutex<RingBuffer<4096>>` (use `heapless::Deque` or a simple custom circular buffer)
   - Also delegates to `EspIdfLogger` so UART output continues working

2. **`GET /debug/log`**: Returns the ring buffer contents as `text/plain`. Most recent entries at the bottom. Optional `?level=error` query param to filter.

3. **`GET /debug/info`**: Returns JSON system diagnostics (heap, WiFi, USB state, uptime, firmware version). No buffering needed.

4. **`POST /debug/level`**: Change runtime log level within compiled range.

5. **Default log level**: INFO in release builds. DEBUG in dev builds.

**Phase 2 (v0.2+)**:

6. **`GET /debug/events`**: SSE endpoint for live log streaming. Uses picoserve's `EventStream`. Log backend sends to both ring buffer AND any connected SSE clients via an `embassy::channel` or similar broadcast mechanism.

7. **Syslog UDP**: Optional, configured via `/config` API or NVS. `POST /config` with `{"syslog": {"host": "192.168.1.10", "port": 514}}` to enable. RFC 5424 format.

### Why this order

- Phase 1 covers 95% of diagnostic needs with minimal complexity
- The ring buffer + snapshot endpoint requires zero async coordination -- just a mutex-protected buffer
- SSE streaming requires async plumbing (broadcast channel, lifetime management of SSE connections) which is more complex to get right on embassy
- Syslog is a power-user feature that nobody will miss in v0.1

### Key decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Log framework | Rust `log` crate | Standard, lightweight, compiled-out filtering, `esp-idf-svc` integration |
| NOT `tracing` | -- | Too heavy for ESP32, no ecosystem support in esp-rs |
| Log format | Plain text (ESP-IDF format) | Compact, matches UART output, familiar to ESP32 users |
| NOT JSON lines | -- | 2-4x size overhead, halves ring buffer capacity |
| Ring buffer size | 4KB | 40-80 lines at INFO level, ~2-3 minutes of normal operation |
| Live streaming | SSE (not WebSocket) | Unidirectional, simpler, browser auto-reconnect, picoserve native support |
| System info | Separate JSON endpoint | Point-in-time metrics, not log events, cheap to generate |
| Release builds | INFO max compiled level, infrastructure always present | Diagnostic endpoints are the entire point for deployed devices |
| UART | Always on | Zero-cost default via `esp-idf-svc`, essential for development |
| Syslog | Phase 2 | Most users won't have a syslog server |

### Desktop (Linux/macOS/Windows) target

The desktop target is straightforward and does not need this ring buffer architecture:

- Use `tracing` crate with `tracing-subscriber` (standard Rust logging stack)
- Log to stderr by default, configurable via `RUST_LOG` env var
- Optional `--log-file` flag for file output
- The HTTP API can still expose `/debug/log` and `/debug/info` for consistency, reading from a similar ring buffer, but files and stderr are the primary diagnostic path

### Shared abstraction

The `DiagnosticsLogger` trait / ring buffer can be defined in shared code, with platform-specific backends:

```rust
// In shared protocol/diagnostics crate
pub trait DiagnosticsProvider {
    fn log_snapshot(&self) -> &[u8];  // Ring buffer contents
    fn system_info(&self) -> SystemInfo;  // Heap, uptime, connection state
}

// HTTP handlers are shared, they just call the trait methods
async fn handle_debug_log(diag: &impl DiagnosticsProvider) -> impl IntoResponse {
    diag.log_snapshot()
}
```

This keeps the HTTP handler logic identical across platforms while allowing platform-specific log capture mechanisms.
