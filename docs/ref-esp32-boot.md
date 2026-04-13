# ESP32 Boot Behavior & Graceful Degradation

Research findings for minidsp-open ESP32-S3 target. Covers WiFi connection scenarios, reconnection, boot sequence design, data partition state machine, provisioning UX, and flash wear.

## 1. WiFi Connection Scenarios & Failure Modes

### How ESP-IDF WiFi connection works

ESP-IDF does **not** auto-reconnect. The WiFi driver is event-driven. When `esp_wifi_connect()` is called, it runs an internal scan + auth + assoc + 4-way handshake sequence. If any step fails, `WIFI_EVENT_STA_DISCONNECTED` fires with a reason code. The application is responsible for calling `esp_wifi_connect()` again in the event handler. There is no built-in retry loop, no backoff, no max-retry — all of that is the application's job.

### Scenario: WiFi configured but AP is down

- `esp_wifi_connect()` triggers an internal scan for the configured SSID
- Scan dwells 120ms per channel by default, 13 channels = ~1.6 seconds total
- If the AP is not found, `WIFI_EVENT_STA_DISCONNECTED` fires with reason `WIFI_REASON_NO_AP_FOUND` (201)
- This happens **once** per `esp_wifi_connect()` call — there is no internal retry
- **Time to failure: ~1.6 seconds** from calling connect

### Scenario: WiFi connected but no internet (captive portal, DNS failure)

- WiFi layer connects successfully — `WIFI_EVENT_STA_CONNECTED` fires, then DHCP runs
- `IP_EVENT_STA_GOT_IP` fires once DHCP succeeds (even behind a captive portal — you get an IP)
- The WiFi driver does not know about "internet." It only knows about the L2 connection
- **Detection is our responsibility**: after getting IP, attempt to fetch `manifest.json`. If DNS fails or HTTP times out, we know there is no internet. The WiFi connection itself is fine
- The device should serve the HTTP API and stale data normally in this state

### Scenario: WiFi connected, internet works, but GitHub Pages is down

- manifest.json fetch returns HTTP 502/503/504 or times out
- **This is the most common "degraded but fine" state**
- Device has WiFi, has IP, has internet — just can't reach our CDN
- Serve HTTP API + stale ezBEQ data from flash. Log the failure. Retry on the next scheduled check

### Scenario: First boot — no WiFi configured

- NVS has no SSID/password stored
- Must enter provisioning mode (SoftAP captive portal — see section 5)
- No HTTP API, no USB HID activity until provisioning completes
- After provisioning, credentials saved to NVS, device restarts into normal boot

### Scenario: First boot after WiFi configured — no prior data in flash

- WiFi connects successfully, but data partition is empty (no catalogue, no UI files)
- manifest.json fetch succeeds → download all data files
- **This is the only boot where we must block on the download** — there is nothing to serve
- If the download fails, the device can still serve the minidsp-rs compatible HTTP API (no ezBEQ UI, but DSP control works)

### Key disconnect reason codes to handle

| Code | Name | Meaning | Action |
|------|------|---------|--------|
| 2 | AUTH_EXPIRE | Auth timed out | Retry with backoff |
| 15 | HANDSHAKE_TIMEOUT | Wrong password (usually) | Retry limited times, then enter provisioning |
| 200 | BEACON_TIMEOUT | AP went away while connected | Retry immediately, then backoff |
| 201 | NO_AP_FOUND | SSID not found in scan | Retry with backoff |
| 202 | AUTH_FAIL | Auth rejected (wrong password) | Don't retry — enter provisioning or alert user |
| 204 | HANDSHAKE_TIMEOUT | 4-way handshake failed | Could be wrong password or signal issue. Retry limited times |
| 205 | CONNECTION_FAIL | Generic connection failure | Retry with backoff |
| 8 | ASSOC_LEAVE | We called disconnect | Do NOT retry |

**Critical distinction:** reason 202 (AUTH_FAIL) means the password is definitely wrong — do not retry endlessly. Reason 201 (NO_AP_FOUND) means the AP might just be temporarily offline — retry with exponential backoff.

## 2. ESP-IDF WiFi Reconnection Behavior

### Does esp-idf-svc auto-reconnect?

**No.** The `esp-idf-svc` Rust wrapper (`EspWifi` / `BlockingWifi`) does not implement auto-reconnect. Looking at the source (`wifi.rs`), when `StaDisconnected` fires, the driver simply updates its internal status to `WifiStaStatus::Started` — it does not call `connect()` again.

The application must:
1. Subscribe to `WifiEvent::StaDisconnected` via the system event loop
2. Inspect the disconnect reason
3. Decide whether to call `wifi.connect()` again
4. Implement its own backoff/retry logic

### Reconnection strategy for minidsp-open

```
                    ┌─────────────────────────┐
                    │   WIFI_DISCONNECTED      │
                    │   (with reason code)     │
                    └──────────┬──────────────┘
                               │
                    ┌──────────▼──────────────┐
                    │  Is reason retryable?    │
                    │  (not AUTH_FAIL,         │
                    │   not ASSOC_LEAVE)       │
                    └──────────┬──────────────┘
                          yes  │  no
                    ┌──────────▼──┐  ┌────────▼───────┐
                    │ retry_count │  │ Enter error     │
                    │ < MAX (20)? │  │ state / re-     │
                    │             │  │ provision       │
                    └──────┬──┘  │  └─────────────────┘
                      yes  │     │ no
                    ┌──────▼──┐  ┌────▼────────────────┐
                    │ Wait    │  │ Wait 5 min, then    │
                    │ backoff │  │ restart retry count  │
                    │ delay   │  │ (AP might come back) │
                    └──────┬──┘  └─────────────────────┘
                           │
                    ┌──────▼──────────────────┐
                    │ esp_wifi_connect()       │
                    │ retry_count++            │
                    │ delay = min(delay*2, 60s)│
                    └─────────────────────────┘
```

**Recommended parameters:**
- Base delay: 1 second
- Max delay: 60 seconds
- Max retries before long pause: 20
- Multiplier: 2x (exponential)
- On successful connection: reset retry_count and delay to base
- On `IP_EVENT_STA_GOT_IP`: mark "connected", notify all waiting tasks

### What happens to TCP/WebSocket connections on WiFi drop

From the ESP-IDF docs: "Upon receiving WIFI_EVENT_STA_DISCONNECTED, the default behavior of the event task is shutting down the station's LwIP netif, notifying the LwIP task to clear the UDP/TCP connections which cause the wrong status to all sockets."

**All sockets are invalidated immediately.** This means:
- Active WebSocket connections to browsers die instantly
- TCP bridge connections (port 5333) from external minidsp-rs clients die
- The HTTP server's listening sockets become invalid

**Our action on disconnect:**
1. Close all active WebSocket connections gracefully (send close frame if possible)
2. Close all TCP bridge connections
3. Stop the HTTP server listener
4. After reconnect + got-IP: restart HTTP server, re-bind all listening sockets
5. Clients must reconnect — this is expected behavior (same as minidsp-rs when host WiFi drops)

## 3. Boot Sequence Design

### Recommended boot sequence

```
POWER ON
    │
    ▼
[1] Initialize early runtime state (logger, RGB LED controller, NVS, storage)
    │
    ▼
[2] Check NVS for WiFi credentials
    │
    ├── No credentials → [PROVISIONING MODE]
    │   │                  SoftAP + captive portal
    │   │                  User enters SSID/password
    │   │                  Save to NVS, restart
    │   │
    ▼
[3] Start WiFi STA, connect (with retry/backoff)
    │
    ├── Timeout after 30s → Start HTTP API anyway (if data exists)
    │                        Continue retrying in background
    ▼
[4] Got IP — start HTTP API + mDNS immediately
    │         (serve whatever is in flash, even if stale)
    │
    ▼
[5] Fetch manifest.json (3-second timeout)
    │
    ├── Timeout/error → Log warning, serve stale data, done
    │
    ├── No data in flash (first boot) AND fetch failed
    │   → Serve minidsp-rs API only (no ezBEQ UI), retry manifest every 5 min
    │
    ▼
[6] Compare versions with flash
    │
    ├── Up to date → Done, serving current data
    │
    ▼
[7] Download changed files (non-blocking for API)
    │   catalogue-meta.bin, catalogue-biquads.bin, ui.tar.gz
    │   15-second timeout per file
    │
    ├── Any download fails → Keep serving old data, retry next boot
    │
    ▼
[8] Write to data partition, update stored versions
    │
    ▼
[READY] Serving current data

Background policy in the current implementation:

- Initial update check runs about 5 seconds after STA WiFi becomes stable
- After that, the updater sleeps for 24 hours and checks again
- Failed/offline checks are non-fatal and simply defer to the next 24-hour interval
- Manual `POST /debug/updater/check` uses the same updater path
- Only one update run is allowed at a time; overlapping manual/scheduled checks return `busy`
```

User-visible LED behavior during that sequence now follows `docs/ref-rgb-led.md`:

- boot startup: solid amber
- SoftAP provisioning: blue breathe
- STA reconnect / connect-in-progress: blue flash
- WiFi up but no usable MiniDSP path yet: amber breathe between `AMBER_MIN` and `AMBER`
- MiniDSP path ready: dim solid green
- real catalogue download/write activity: green breathe
- fatal startup failure: solid red, latched until reboot

### What should be blocking vs non-blocking

| Step | Blocking? | Rationale |
|------|-----------|-----------|
| NVS + hardware init | Blocking | Fast (~100ms), required for everything |
| WiFi connect | Semi-blocking | Block up to 30s. If no connection, start API anyway with stale data |
| USB HID init | Non-blocking | Start immediately. Device might not be plugged in yet |
| manifest.json fetch | Non-blocking | 3s timeout. API already serving. Done in background |
| Data download | Non-blocking | API serves stale data while downloading. Update atomically when done |
| HTTP API start | As early as possible | Start the moment we have an IP (or even before, on AP interface during provisioning) |

### Timeout recommendations

| Operation | Timeout | Rationale |
|-----------|---------|-----------|
| WiFi connect attempt | ~1.6s (scan) + 5s (auth/assoc) | Built into ESP-IDF. We control the retry |
| WiFi "give up blocking" | 30 seconds | If WiFi isn't up in 30s, start serving locally and keep retrying |
| manifest.json fetch | 3 seconds | Tiny file (~100 bytes). If GitHub Pages is this slow, skip the update |
| catalogue/data download | Non-blocking | Current implementation downloads published files listed in `manifest.json` while API stays up |
| DNS resolution | 5 seconds | System default, but worth enforcing explicitly |

### How to handle first-boot-with-no-data

First boot with WiFi but empty data partition:

1. Start HTTP API immediately on port 5380 — it serves the minidsp-rs compatible endpoints (device status, PEQ writes, WebSocket levels). These work regardless of ezBEQ data
2. On `GET /` or any static file request: return `503 Service Unavailable` with a JSON body `{"status": "initializing", "message": "Downloading ezBEQ catalogue and UI. Please wait..."}`
3. Attempt manifest + full data download
4. On success: serve normally
5. On failure: keep minidsp-rs API functional. Retry data download every 5 minutes. The device is still fully usable for direct MiniDSP control — it just lacks the ezBEQ browsing UI

**The MiniDSP is always controllable.** USB HID works regardless of WiFi or data state. The minidsp-rs compatible API works the moment WiFi is up. The ezBEQ browsing UI is the only thing that needs the data partition.

## 4. Data Partition State Machine

### States

```
┌──────────────────────────────────────────────────────┐
│                                                      │
│   EMPTY ──────────► DOWNLOADING ──────► CURRENT      │
│     │                    │                  │         │
│     │                    │ (failure)        │         │
│     │                    ▼                  │         │
│     │               EMPTY (still)           │         │
│     │                                       │         │
│     │                              (boot, check fails)│
│     │                                       ▼         │
│     │                                    STALE        │
│     │                                       │         │
│     │                              (boot, check OK,   │
│     │                               versions match)   │
│     │                                       ▼         │
│     │                                    CURRENT      │
│     │                                                 │
│     └──── (boot, fetch succeeds) ────► DOWNLOADING ──►│
│                                                      │
└──────────────────────────────────────────────────────┘
```

### State: EMPTY (first boot, no prior sync)

- **Detection:** Version file in NVS is absent or reads as zeroed
- **minidsp-rs HTTP API:** Fully functional (device control, PEQ, WebSocket levels)
- **ezBEQ API:** `/api/2/devices` returns devices but no catalogue. `Catalogue` WebSocket message sends empty list
- **Static UI files:** Return 503 with JSON status message
- **CLI:** Fully functional
- **TCP bridge (5333):** Fully functional
- **UDP discovery (3999):** Fully functional

### State: STALE (has data, update check failed)

- **Detection:** Version file in NVS has values, but last manifest check failed or returned different versions
- **All APIs:** Fully functional with existing data
- **ezBEQ UI:** Works normally — user sees catalogue as of last successful sync
- **Behavior:** Indistinguishable from CURRENT to the end user. Only logging reveals staleness
- **A status endpoint should expose this:** `GET /api/system/status` → `{"data_state": "stale", "last_sync": "2026-03-28T...", "last_check": "2026-03-30T...", "last_check_error": "timeout"}`

### State: CURRENT (has data, matches manifest)

- **Detection:** Version file in NVS matches latest manifest.json
- **All APIs:** Fully functional with current data
- **This is the normal operating state**

### API surface availability matrix

| API | EMPTY | STALE | CURRENT |
|-----|-------|-------|---------|
| `GET /devices/{id}` | Yes | Yes | Yes |
| `POST /devices/{id}` | Yes | Yes | Yes |
| `POST /devices/{id}/config` | Yes | Yes | Yes |
| `POST /devices/{id}/volume/{direction}` | Yes | Yes | Yes |
| `POST /devices/{id}/source/{source}` | Yes | Yes | Yes |
| `POST /devices/{id}/preset/{preset}` | Yes | Yes | Yes |
| WebSocket `/devices/{id}` (`poll=true`, `levels=true`) | Yes | Yes | Yes |
| TCP bridge (5333) | Yes | Yes | Yes |
| UDP discovery (3999) | Yes | Yes | Yes |
| ezBEQ catalogue WebSocket | Empty list | Old data | Current data |
| `PUT /api/1/devices/{name}/filter/{slot}` | 404 (no catalogue) | Yes (old IDs) | Yes |
| Static UI files (`/*`) | 503 | Yes | Yes |
| `GET /api/system/status` | Yes | Yes | Yes |

## 5. WiFi Provisioning UX

### What popular ESP32 projects do

| Project | Method | UX |
|---------|--------|-----|
| **WLED** | SoftAP (`WLED-AP`, password `wled1234`) + web UI at 192.168.4.1 | Connect to AP, open browser, enter WiFi credentials. Simple, no app needed |
| **Tasmota** | SoftAP (`tasmota-XXXX`, open) + web UI at 192.168.4.1 | Same pattern as WLED. Scan for networks, pick one, enter password |
| **ESPHome** | Fallback SoftAP + captive portal when configured WiFi fails. Also supports "Improv WiFi" (serial/BLE) | Same SoftAP pattern, plus BLE for initial setup via ESPHome app |
| **ESP RainMaker** | BLE provisioning via phone app | Requires dedicated app download. More complex |

**The clear winner for headless consumer devices is SoftAP + captive portal.** Every major project uses it. No app install required. Works on any phone/laptop.

### Recommended approach: SoftAP + captive portal

**First boot (no credentials in NVS):**

1. Device starts SoftAP: SSID `minidsp-setup` (or `minidsp-XXXX` with last 4 of MAC), open network (no password — reduces friction for one-time setup)
2. DNS server redirects all queries to ESP32 IP (192.168.4.1) — triggers captive portal on most phones
3. Serve a simple provisioning web page:
   - Scan and list available networks (call `esp_wifi_scan_start`)
   - User taps their network, enters password
   - Submit button
4. Device saves credentials to NVS
5. Device switches to STA mode, attempts connection
6. If connection succeeds: provisioning complete, restart into normal mode
7. If connection fails (wrong password): re-enter SoftAP mode, show error message on the portal page

**Re-provisioning trigger:**
- Long-press a button (if hardware has one)
- Or: after N consecutive failed connection attempts with AUTH_FAIL (code 202), automatically re-enter provisioning mode
- Or: API endpoint `POST /api/system/reprovision` (accessible on the SoftAP if the device falls back)
- Clear credentials: erase WiFi NVS namespace, restart

### The `esp-wifi-provisioning` crate

A new Rust crate (`esp-wifi-provisioning` v0.1.0, published March 2026) does exactly this:

```rust
use esp_wifi_provisioning::Provisioner;

let wifi = Provisioner::new(wifi, nvs)
    .ap_ssid("minidsp-setup")
    .retry_config(
        RetryConfig::default()
            .max_attempts(3)
            .connect_timeout(Duration::from_secs(15)),
    )
    .provision()?;
```

- On boot: tries stored NVS credentials first. If they work, returns immediately
- If no credentials or they fail: starts SoftAP with captive portal
- User connects, enters credentials via web UI
- Saves to NVS, connects, returns the connected WiFi driver
- Supports `clear_credentials()` for factory reset

**Recommendation:** Evaluate this crate. If it's solid, use it directly. If not, it's only ~1.5K SLOC — we can vendor or reimplement the pattern. The core logic is straightforward: SoftAP + DNS redirect + HTTP form + NVS save.

### Why NOT BLE provisioning

- BLE adds ~100KB to firmware size
- Requires a companion app (or generic BLE scanner with knowledge of GATT characteristics)
- Our target users are home theater enthusiasts, not developers. They expect "connect to WiFi, open browser" — not "download an app, pair via Bluetooth"
- BLE and WiFi sharing the radio on ESP32-S3 can cause contention
- Every successful open-source project in this space (WLED, Tasmota, ESPHome) uses SoftAP

### Why NOT SmartConfig

- Requires Espressif's EspTouch app
- Fails on many routers (AP isolation, multicast filtering)
- Credentials broadcast over the air (obfuscated, not encrypted)
- Unreliable in practice — many user complaints in forums

## 6. Flash Wear Analysis

### ESP32-S3 flash specifications

- Flash type: SPI NOR (typically Winbond W25Q128 or GigaDevice GD25Q128 on S3 modules)
- Guaranteed erase cycles: **100,000 per sector** (minimum, per datasheet)
- Typical endurance: often exceeds 100,000 — some chips rated at 200,000+
- Sector size: 4,096 bytes (4 KB)
- ESP-IDF wear leveling distributes erases across all sectors in a partition

### Data partition wear math

**Assumptions for minidsp-open:**
- Current data partition size: `0xBF0000` bytes = ~11.94 MB
- 11.94 MB / 4 KB = 3,056 sectors
- Current measured payload build (2026-04-13): `catalogue.bin` ~3.0 MB, UI files ~1.35 MB, total site payload ~4.3 MB
- With ESP-IDF wear leveling: erases are distributed across all 3,056 sectors

**Update scenarios:**

| Event | Frequency | Data written | Sectors erased |
|-------|-----------|-------------|----------------|
| Boot (no update needed) | Daily | 0 | 0 |
| Catalogue update | ~2x/week | ~3.1 MB | ~772 sectors |
| UI update | ~4x/year | ~1.35 MB | ~330 sectors |
| Full site refresh | Uncommon | ~4.3 MB | ~1,101 sectors |
| NVS writes (device state) | Rare | 4 KB | 1 sector |

**Worst case: catalogue updates twice per week**

- Sectors erased per update: ~768 (3 MB / 4 KB)
- Updates per year: ~104
- Total sector erases per year: 104 × 768 = 79,872 sector erases
- With wear leveling across 1,280 sectors: 79,872 / 1,280 = **62 erases per sector per year**
- At 100,000 cycles per sector: **100,000 / 62 = 1,612 years**

**Even without wear leveling** (worst case, always writing to same sectors):
- 100,000 / (104 updates × 1) = **961 years** per sector if each update only hits each sector once

**With daily reboots that trigger version checks (no update):**
- manifest.json is fetched to RAM, compared to NVS version string
- If versions match: **zero flash writes**, zero erases
- Only NVS read (no wear)

### How to avoid unnecessary erases

1. **Version check before write:** Always fetch manifest.json first. Compare version strings in RAM. Only download and write if versions differ. This is already the planned design
2. **Atomic update:** Download new files to a staging area (or download to RAM for small files), then erase and write the data partition in one pass. Don't erase-then-download (risk of power loss leaving empty partition)
3. **Checksum verification:** After download, verify CRC32/SHA256 of downloaded data against manifest before writing to flash. Never write corrupt data
4. **NVS for version tracking:** Store `{"catalogue": "2026-03-28-abcdef", "ui": "4.2.0"}` in NVS (separate from the data partition). NVS has its own wear leveling. Reading NVS has zero wear cost

### ESP-IDF wear leveling support

ESP-IDF provides a `wear_levelling` component that sits between the application and raw flash:

- **Performance mode** (default): Sector data buffered in RAM during erase. Fastest, but 4KB data loss risk on power failure during erase
- **Safety mode**: Sector data saved to another flash area before erase. Survives power loss but slower
- Wear leveling works with FAT filesystem or raw partition access
- NVS has its own built-in wear leveling (separate from the WL component)

**Recommendation for data partition:** Use **LittleFS** on the data partition rather than raw flash or FAT:
- Built-in wear leveling (no need for separate WL component)
- Power-loss resilient (journaling)
- Directory support (organize UI files, catalogue files, versions)
- Low RAM usage
- Available as ESP-IDF component: `joltwallet/littlefs`

LittleFS is the best fit because:
- FAT has no power-loss protection (bad for embedded)
- SPIFFS is unmaintained, no directories, slows down when full
- Raw flash + WL component works but requires manual file management
- LittleFS gives us a real filesystem with wear leveling and crash safety

### NVS wear considerations

NVS has its own page-based wear leveling:
- NVS partition: typically 24 KB (6 pages of 4 KB)
- NVS writes for minidsp-open: WiFi credentials (once), version strings (on catalogue update ~2x/week), device state (on preset/volume change)
- Even at 1,000 NVS writes/day: with 6-page wear leveling, each page sees ~167 writes/day = 60,000/year
- At 100,000 cycle limit: **~1.6 years** — but we won't do anywhere near 1,000 writes/day
- Realistic: ~10 NVS writes/day (version check + occasional device state) = **~16,000 years**

## Summary of Recommendations

### Boot sequence
1. Init hardware + NVS (blocking, fast)
2. Check NVS for WiFi creds → if missing, enter SoftAP provisioning
3. Connect WiFi with exponential backoff (1s base, 60s max, 20 retries)
4. Start HTTP API the moment we get IP (serve stale/empty data)
5. Check manifest.json in background (3s timeout)
6. Repeat the background check every 24 hours while the device remains powered
6. Download updates if needed (15s timeout per file, non-blocking for API)

### WiFi provisioning (implemented and tested 2026-04-06)
- SoftAP + captive portal, matching WLED/Tasmota pattern
- Open AP named `minidsp-setup`, IP 192.168.71.1
- Glass-card styled provisioning page (open-sub-optimizer design)
- Save to NVS, restart into STA mode
- **Captive portal auto-open requires all of:**
  1. DNS hijack on port 53 (only answer Type A queries, ANCOUNT=0 for others)
  2. `esp_netif_set_dns_info` to advertise AP IP as DNS server in DHCP offers (note: `esp_netif_dhcps_option` with `ESP_NETIF_DOMAIN_NAME_SERVER` returns error 20481 — broken API, use `set_dns_info` instead)
  3. DHCP Option 114 (RFC 8910) via `esp_netif_dhcps_option` with `ESP_NETIF_CAPTIVEPORTAL_URI`
  4. HTTP `/*` wildcard catch-all with `uri_match_wildcard: true` in server config
  5. Redirect must use **absolute URL** (`http://192.168.71.1/`) with **non-empty body** (iOS rejects empty redirect bodies)
  6. `max_open_sockets: 13` and `CONFIG_LWIP_MAX_SOCKETS=16` (phones open many connections during portal detection)
  7. DNS thread needs 8KB stack (6KB overflows with logging)

### Reconnection
- App-level exponential backoff (ESP-IDF does not auto-reconnect)
- Distinguish retryable vs fatal disconnect reasons
- AUTH_FAIL (202) = wrong password → limited retries, then re-provision
- BEACON_TIMEOUT (200) = AP went away → retry aggressively
- NO_AP_FOUND (201) = AP not broadcasting → retry with backoff

### Data partition
- Use LittleFS for power-loss safety + wear leveling + directory support
- Three states: EMPTY, STALE, CURRENT
- Always serve minidsp-rs API regardless of data state
- Version check before write (zero unnecessary erases)
- Flash lifetime: >1,600 years under worst-case update frequency

### Key design principle
**The MiniDSP is always controllable.** WiFi down? USB HID still works locally. No internet? Stale catalogue data still works. Empty data partition? minidsp-rs API still works for direct DSP control. Every degradation mode preserves core functionality.
