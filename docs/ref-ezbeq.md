# ezBEQ Integration

ezBEQ is the primary reason people run minidsp-rs. It's a per-movie bass EQ system — community members measure what bass was rolled off in consumer releases and publish correction filters. Users browse a catalogue, tap a movie, and the filters get written to their MiniDSP's PEQ slots. After the movie, the filters get cleared.

minidsp-open targets drop-in compatibility with minidsp-rs in ezBEQ workflows, and on ESP32 we go further: we replace the entire RPi + minidsp-rs + ezBEQ stack with a single $10 board.

This file describes ezBEQ-facing behavior and architecture. It is not the place to track temporary validation state. Current status lives in `PROGRESS.md`, and the compatibility verification checklist lives in `docs/todo-checker.md`.

**Repo:** https://github.com/3ll3d00d/ezbeq
**Docs:** https://ezbeq.readthedocs.io/en/latest/
**Author:** 3ll3d00d (active on AVS Forum)
**Catalogue:** https://github.com/3ll3d00d/beqcatalogue (~4000+ titles, community-maintained)

## What ezBEQ Does

1. **Catalogue browser.** Downloads a ~34.5 MB JSON catalogue of BEQ filter profiles from GitHub. Each entry has: title, author, year, audio codec, and filter definitions (type/freq/gain/Q + pre-computed biquad coefficients at multiple sample rates). Stored in SQLite locally. Auto-refreshes every 5 minutes.

2. **Filter writer.** When user selects a movie, ezBEQ translates the filter profile into biquad coefficient writes for the correct PEQ slots on the connected MiniDSP. It shells out to the `minidsp` CLI binary to do the actual writes.

3. **State tracker.** Since exact MiniDSP config readback is still incomplete on some device/field combinations, ezBEQ tracks which filters are loaded in which slot via JSON files on disk. This state survives restarts.

4. **Multi-device router.** Supports multiple simultaneous MiniDSPs and other DSP platforms (HTP-1, JRiver, CamillaDSP, Q-Sys). Each device has its own slot allocation, routing config, and state.

5. **Live monitor.** WebSocket connection pushes level meter data and state changes to the UI in real-time.

## How ezBEQ Talks to minidsp-rs

ezBEQ does NOT use minidsp-rs as a library. It shells out to the `minidsp` CLI binary via Python's `plumbum` subprocess wrapper.

### Loading filters

Generates a temp file with one command per line, runs `minidsp -f <tempfile>`:

```
input 0 peq 0 set -- 1.0003468 -1.9990215 0.9986773 -1.9990215 0.9990241
input 0 peq 0 bypass off
input 1 peq 0 set -- 1.0003468 -1.9990215 0.9986773 -1.9990215 0.9990241
input 1 peq 0 bypass off
input 0 peq 1 set -- 1.0124892 -1.9710445 0.9594107 -1.9710445 0.9718999
input 0 peq 1 bypass off
...
input 0 peq 7 bypass on
input 1 peq 7 bypass on
...
input 0 peq 9 bypass on
input 1 peq 9 bypass on
```

- Used slots: write 5 biquad coefficients (b0, b1, b2, a1, a2) + `bypass off`
- Unused slots: just `bypass on` (old coefficients left in place, doesn't matter)
- Same filter written to all channels in the route (e.g., both input 0 and input 1)

### Clearing filters

Same mechanism but with zero used slots — every PEQ slot gets `bypass on`:

```
input 0 peq 0 bypass on
input 1 peq 0 bypass on
input 0 peq 1 bypass on
...
input 0 peq 9 bypass on
input 1 peq 9 bypass on
input 0 mute off
input 0 gain -- 0.00
input 1 mute off
input 1 gain -- 0.00
```

No identity biquads written, no previous state restored. Just bypass everything and reset gain/mute. This is deliberate — `bypass on` is one command per slot vs two for a full clear, and ezBEQ always writes fresh coefficients on next load anyway.

### Reading state

```
minidsp -o jsonline
```

Parses JSON output for: master volume, mute, source, active preset, input/output levels.

### Live monitoring

Connects to minidsp-rs compatibility WebSocket at `ws://{wsIp}/devices/{wsDeviceId}?levels=true&poll=true` for real-time level data and state change notifications. The `wsIp` config value includes the port (default `127.0.0.1:5380`), and `wsDeviceId` specifies the device index. Uses Autobahn Twisted WebSocket client with reconnecting factory.

## ezBEQ's Tech Stack

| Component | Technology |
|-----------|-----------|
| Backend | Python 3.13+, Twisted (async), Flask (REST via flask-restx), Autobahn (WebSocket) |
| Frontend | React 19, Vite 7, Material UI v7, uPlot (meters) — builds to static SPA |
| Database | SQLite with WAL mode, mmap tuning |
| Catalogue | ~34.5 MB JSON from GitHub, parsed incrementally with ijson |
| IPC | shells out to `minidsp` CLI via plumbum |
| Config | YAML (`ezbeq.yml`) |

### ezbeq.yml device descriptor format

This is how ezBEQ knows the PEQ topology of each device:

```yaml
devices:
  minidsp24:
    cmdTimeout: 10
    exe: minidsp
    ignoreRetcode: true
    options: ''
    type: minidsp
    wsDeviceId: 1
    descriptor:
      name: 2x4
      fs: 96000
      routes:
      - name: input
        biquads: 10
        channels: [0, 1]
        slots: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]
      - name: crossover
        biquads: 4
        channels: [0, 1, 2, 3]
        slots: []
        groups: [0, 1]
      - name: output
        biquads: 10
        channels: [0, 1, 2, 3]
        slots: []
```

The `routes` array with `name: input` and `slots: [0..9]` tells ezBEQ to write BEQ filters to input PEQ slots 0-9. The Flex HTx workaround uses `name: crossover` + `name: output` with split slots since it has no input PEQ.

## What minidsp-open Must Support for ezBEQ Compatibility

Canonical source of truth for the public compatibility contract:
- `docs/ref-compatibility.md`

This file focuses on ezBEQ-specific usage patterns and ESP32 strategy. Do not treat it as the canonical definition of CLI/HTTP/WebSocket/TCP/UDP compatibility semantics.

### CLI compatibility (drop-in binary replacement)

ezBEQ shells out to `minidsp`. Our binary must accept the same commands:

| Command | What it does |
|---------|-------------|
| `minidsp` (no args) | Print device status as text |
| `minidsp -o jsonline` | Print device status as JSON |
| `minidsp -f <file>` | Execute batch commands from file |
| `minidsp probe` | List connected devices |
| `minidsp input {ch} peq {n} set -- {b0} {b1} {b2} {a1} {a2}` | Write biquad coefficients |
| `minidsp input {ch} peq {n} bypass {on\|off}` | Enable/disable PEQ filter |
| `minidsp input {ch} mute {on\|off}` | Mute/unmute input channel |
| `minidsp input {ch} gain -- {dB}` | Set input channel gain |
| `minidsp output {ch} peq {n} set -- ...` | Same for output PEQ |
| `minidsp output {ch} peq {n} bypass {on\|off}` | Same for output |
| `minidsp output {ch} mute {on\|off}` | Same for output |
| `minidsp output {ch} gain -- {dB}` | Same for output |
| `minidsp config {n}` | Switch preset/config slot |
| `minidsp gain -- {dB}` | Set master volume |
| `minidsp mute {on\|off}` | Set master mute |
| `minidsp source {name}` | Set input source |
| `minidsp server` | Start HTTP/WebSocket/TCP/UDP daemon |
| `minidsp --force-kind {type}` | Override auto-detected device type |

### HTTP API compatibility (daemon mode)

Canonical endpoint definitions live in `docs/ref-compatibility.md`. Summary for ezBEQ:

When running as `minidsp server`, must serve:

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/devices/{id}` | GET | Device status (JSON) |
| `/devices/{id}` | POST | Set master status |
| `/devices/{id}/config` | POST | Structured config updates |
| `/devices/{id}/volume/{direction}` | POST | Compatibility volume/mute convenience route |
| `/devices/{id}/source/{source}` | POST | Compatibility source route |
| `/devices/{id}/preset/{preset}` | POST | Compatibility preset route |
| `/devices/{id}` | WebSocket | Compatibility WebSocket for live state + level stream (`poll=true`, `levels=true`) |

Plus UDP discovery on port 3999 and TCP bridge on port 5333.

### Auto-detection

minidsp-rs requires `device_type` in config. We auto-detect from `hw_id` in the device's master status response. Every hw_id is mapped in our device registry. The `--force-kind` flag exists only as an escape hatch.

### ezBEQ implementation details for Phase 2

Verified against ezBEQ source (2026-04-03). Not needed for Phase 1 compatibility, but critical for Phase 2 ESP32 ezBEQ:

- **`device_type` config key:** Maps to built-in descriptors: `24HD`, `DDRC24`, `DDRC88`, `HTx`, `4x10`, `10x10`, `SHD`, `8x12CDSP`. Default is `Minidsp24HD`. SHD and DDRC24 share the same PEQ layout internally.
- **Slot switching:** ezBEQ prepends `config {n}` to batch files when the target slot differs from the active one. `slotChangeDelay` config option adds a delay after slot change for Flex devices.
- **`sw_channels`:** Config key that overrides which output channels receive BEQ filters (used for DDRC88/HTx).
- **ezBEQ REST API surface:** Beyond filter load/clear, the frontend calls: `/api/1/devices/{name}/config/{slot}/active`, `/api/1/devices/{name}/mute`, `/api/1/devices/{name}/gain`, `/api/1/devices/{name}/levels`, `/api/2/devices` (all devices with refresh), `/api/3/devices/{name}` (PATCH with MV adjust), `/api/1/search`, `/api/1/catalogue/{id}/details`, `/api/1/authors`, `/api/1/audiotypes`, `/api/1/years`, `/api/1/contenttypes`, `/api/1/version`. All must be implemented on ESP32 for the React UI.
- **ezBEQ WebSocket protocol:** Server sends JSON `{"message": "DeviceState"|"Catalogue"|"CatalogueEntries"|"Levels"|"Error", "data": ...}`. Client sends `"load catalogue"` and `"subscribe levels {device_name}"`. Catalogue entries are sent in chunks.
- **`ignoreRetcode`:** Config flag that suppresses exit code checking from minidsp CLI.

## ESP32 Strategy: Replace the Entire Stack

### The problem today

A typical ezBEQ setup requires:
- Raspberry Pi ($35-75)
- SD card, power supply, case ($20-30)
- Linux knowledge (SSH, systemd, config files)
- Install minidsp-rs (Rust binary)
- Install ezBEQ (Python 3.13 + dependencies)
- Configure `ezbeq.yml` with device descriptor
- USB cable to MiniDSP
- Optional: WI-DG ($75) if you want wireless

Total: $55-180 + significant setup time. Multiple forum users report struggling with the RPi setup and giving up.

### Our ESP32 solution

A single ESP32-S3 dev board ($10) plugged into the MiniDSP's USB port. Flash our firmware once. Connect to WiFi. Open `http://minidsp.local` on your phone. Browse BEQ, tap a movie, done.

No RPi. No Linux. No SSH. No Python. No config files.

### Why can't we just run ezBEQ on ESP32?

ezBEQ is a Python application (Python 3.13+, Twisted, Flask, SQLite). The BEQ catalogue alone is ~34.5 MB — larger than ESP32's entire flash. Python doesn't run on ESP32 at all.

But we don't need to run ezBEQ. We serve ezBEQ's **unmodified pre-built React UI** from flash and implement its backend API natively. A GitHub Action keeps everything synced automatically. We maintain zero ezBEQ code and zero UI code.

### How ezBEQ's data flow actually works

Understanding why this matters: ezBEQ's React frontend **never receives biquad coefficients**. When the backend sends the catalogue to the browser over WebSocket, it deliberately strips out the `filters` and `digest` fields — the browser only gets display metadata (title, year, author, etc.). When the user picks a movie, the browser sends just an entry ID string to the backend, and the backend does the lookup in SQLite to get the biquad coefficients.

This means we can't just "put the catalogue in the browser and have it send coefficients to the ESP32" — the browser never has the coefficients, and the frontend code sends entry IDs, not filter data.

### Our approach: ezBEQ's own UI, auto-updating, zero maintenance

We serve ezBEQ's **unmodified pre-built React UI** from ESP32 flash. We implement ezBEQ's backend API natively in Rust. A GitHub Action automatically syncs the catalogue index and UI build artifacts. The ESP32 self-updates its data partition on every boot — no firmware releases needed for catalogue or UI changes.

**We maintain zero ezBEQ code. Zero UI code.** The ESP32 is a thin shell: it serves ezBEQ's own UI, resolves entry IDs from a pre-built biquad index, and writes to the MiniDSP.

#### The auto-update pipeline (GitHub Action)

A GitHub Action in our repo runs every 15 minutes (~80 lines of Python):

**Catalogue sync (every 15 min):**
1. Fetch `version.txt` from `3ll3d00d/beqcatalogue` — if unchanged, exit immediately (~2 seconds, costs nothing)
2. If changed, download `database.json` (~34.5 MB)
3. Generate two compact artifacts:
   - **`catalogue-meta.bin`** — the same metadata fields ezBEQ's backend sends over WebSocket (`UI_FIELDS`: title, year, author, audioTypes, mvAdjust, etc. — deliberately excludes `filters` and `digest`, matching ezBEQ's own behavior). Pre-formatted in ezBEQ's chunked WebSocket message format so the ESP32 can stream it directly to the browser without parsing. **~1.5-2 MB gzipped.**
   - **`catalogue-biquads.bin`** — compact binary index mapping `entry_id → biquad coefficients` at each sample rate (48kHz, 96kHz). ~4000 entries × ~10 filters × 5 coefficients × 4 bytes ≈ **~1 MB**. The ESP32 uses this to resolve entry IDs into actual DSP writes.
4. Publish to GitHub Pages + update `manifest.json`

**ezBEQ UI sync (on ezBEQ release):**
1. Same Action also checks the latest `3ll3d00d/ezbeq` release tag via GitHub API
2. If new, downloads the pre-built React static files from their release
3. Repackages as **`ui.tar.gz`** (~1 MB gzipped)
4. Publishes to GitHub Pages + updates `manifest.json`

**Version manifest** (published at `https://our-org.github.io/minidsp-open/manifest.json`):
```json
{"catalogue": "2026-03-28-abcdef", "ui": "4.2.0"}
```

The Action touches zero ezBEQ application code. It just downloads public build artifacts and catalogue data and repackages them.

**Cost:** GitHub free tier = 2,000 Actions minutes/month. The 15-minute version check = ~96 min/month. Full rebuilds only run when something changes (catalogue: a few times/week, UI: a few times/year). Well within free tier.

**Future optimization:** If we collaborate with 3ll3d00d, he could add a `repository_dispatch` webhook to trigger our Action instantly on catalogue update — zero polling, zero delay.

#### ESP32 self-update (no firmware releases needed)

The ESP32 flash has two partitions:

| Partition | Contents | How it updates |
|-----------|----------|----------------|
| **Firmware** | Rust binary (API server, USB HID, WiFi) | OTA — only when we ship new features/fixes |
| **Data** | ezBEQ UI + catalogue index | **Automatically on every boot** from GitHub Pages |

On every boot:
1. Fetch `manifest.json` from our GitHub Pages (~100 bytes, instant)
2. Compare `catalogue` and `ui` versions with what's in flash
3. If catalogue changed → download `catalogue-biquads.bin` + `catalogue-meta.bin` (~3 MB, 2-3 sec on WiFi)
4. If UI changed → download `ui.tar.gz` (~1 MB, ~1 sec)
5. Write to data partition. Ready to serve.

**The ESP32 keeps itself current without any firmware updates from us.** New BEQ titles appear within 15 minutes of community submission. New ezBEQ UI versions appear within hours of release. The user does nothing. We do nothing.

#### How it all fits together

```
┌──────────────────────────────────────────────────────────┐
│  Phone/Tablet/Laptop Browser                             │
│                                                          │
│  ┌─────────────────────────────────────────────────────┐ │
│  │  ezBEQ React UI (unmodified, served from ESP32)     │ │
│  │                                                     │ │
│  │  1. Connects WebSocket → receives catalogue         │ │
│  │     metadata in chunks (streamed from ESP32 flash)  │ │
│  │  2. User browses/searches movies                    │ │
│  │  3. User taps "load" → sends entry ID to ESP32      │ │
│  │  4. Receives state updates via WebSocket            │ │
│  └──────────────────────┬──────────────────────────────┘ │
│                         │                                │
│   PUT /api/1/devices/{name}/filter/{slot}                │
│   body: {"entryId": "abc123_0"}                          │
└─────────────────────────┼────────────────────────────────┘
                          │ WiFi
                          ▼
┌────────────────────────────────────┐  USB HID  ┌──────────┐
│  ESP32-S3 ($10)                    │──────────▶│ MiniDSP  │
│                                    │           └──────────┘
│  Firmware partition (OTA only):    │
│  ├─ ezBEQ backend API             │
│  ├─ minidsp-rs compat API         │
│  ├─ USB HID transport             │
│  ├─ WebSocket (levels/state)      │
│  ├─ WiFi + mDNS                   │
│  └─ Auto-update manager           │
│                                    │
│  Data partition (auto-updated):    │
│  ├─ ezBEQ UI static files   ~1 MB │ ← from their releases
│  ├─ catalogue-meta.bin      ~2 MB │ ← from beqcatalogue
│  ├─ catalogue-biquads.bin   ~1 MB │ ← from beqcatalogue
│  └─ device state (NVS)            │
└────────────────────────────────────┘
        │               ▲
        │  on boot:     │
        │  check        │  versions changed?
        ▼               │
┌─────────────────────────────────────┐
│  Our GitHub Pages                   │
│  (auto-published by GitHub Action)  │
│                                     │
│  manifest.json                      │
│  catalogue-meta.bin                 │
│  catalogue-biquads.bin              │
│  ui.tar.gz                          │
└─────────────────────────────────────┘
        ▲
        │  every 15 min: version changed?
┌───────┴─────────────────────────────┐
│  Upstream repos (not ours)          │
│                                     │
│  3ll3d00d/beqcatalogue              │
│  └─ database.json, version.txt     │
│                                     │
│  3ll3d00d/ezbeq                     │
│  └─ release artifacts (React SPA)   │
└─────────────────────────────────────┘
```

#### Flash budget

| Component | Size | Updates |
|-----------|------|---------|
| Firmware (Rust + esp-idf) | ~2-3 MB | OTA only — our releases |
| ezBEQ UI static files (gzip) | ~1 MB | Auto — on ezBEQ release |
| Catalogue metadata | ~1.5-2 MB | Auto — within 15 min of catalogue change |
| Biquad index | ~1 MB | Auto — within 15 min of catalogue change |
| Device state (NVS) | ~4 KB | On device write |
| **Total** | **~5.5-7 MB** | ESP32-S3 has 8-16 MB flash |

#### What the ESP32 implements

Two API surfaces that share the same device control core:

**minidsp-rs compatible API** (for desktop ezBEQ, Home Assistant, etc.):
- HTTP endpoints on port 5380
- WebSocket on `/devices/{id}` with compatibility query params
- TCP bridge on port 5333
- UDP discovery on port 3999

**ezBEQ compatible API** (for the embedded React UI):
- WebSocket: `Catalogue` message on connect, `"load catalogue"` handler, `CatalogueEntries` chunks streamed from flash, `DeviceState` pushes
- REST: `PUT/DELETE /api/1/devices/{name}/filter/{slot}` — resolve entry ID from biquad index in flash, write to MiniDSP
- REST: `PATCH /api/2/devices/{name}`, `PATCH /api/3/devices/{name}` — with MV adjust support
- REST: `GET /api/2/devices` — device state
- Static file serving: `/*` → ezBEQ React SPA from data partition

### What we maintain vs what we don't

| Component | Who maintains it | Our code |
|-----------|-----------------|----------|
| BEQ catalogue content | 3ll3d00d + community | Zero |
| ezBEQ Python backend | 3ll3d00d | Zero |
| ezBEQ React UI | 3ll3d00d | Zero — we serve their unmodified build |
| Catalogue index pipeline | Us | ~80 lines Python (GitHub Action) |
| minidsp-rs + ezBEQ API | Us | Core project — both APIs share device control |
| Device control (USB HID) | Us | Core project |

**Zero ezBEQ code. Zero UI code. Everything auto-updates.** We consume public build artifacts and catalogue data. The ~80-line GitHub Action is the only thing that touches the catalogue. The ESP32 self-updates. The only time we ship firmware is when our own Rust code changes.

### Desktop users: intended drop-in replacement

On desktop (Linux/macOS/Windows), the target user experience is still binary swap compatibility: users replace `minidsp-rs` with our `minidsp` binary and keep the same ezBEQ configuration. The core CLI, daemon, WebSocket, and batch-command surfaces are implemented for that workflow. Current verification status is tracked in `PROGRESS.md` and `docs/todo-checker.md`.

No catalogue index, no web UI, no GitHub Actions involved. ezBEQ runs on the same machine and shells out to our `minidsp` binary exactly as it does to minidsp-rs today.

## Config Read-Back: Improved, Not Universal

The ezBEQ docs warn:

> "Bear in mind that, so far, settings cannot be read back from the MiniDSP 2x4HD. This means that no applications are able to show your currently loaded configs."

The MiniDSP protocol has command `0x14` (`ReadFloats`) that reads DSP registers, and minidsp-open uses that mechanism where the hardware exposes usable values. On the physical DSP100 / 2x4 HD path, that is not universal: PEQ state still requires the shadow/default model because live PEQ `0x14` reads return the all-zero placeholder even though writes succeed, routing and FIR tap count still need verified shadow fallback across reconnects, and the official Device Console capture confirms compressor writes without proving an authoritative compressor-parameter readback path. Even MiniDSP's own Device Console does not appear to rely on direct parameter readback here; it uses local state plus explicit modification-token writes and additional flash/token command families (`0x06`, `0x07`, `0x40`) that are now identified and do not provide the missing runtime readback.

Long-term target behavior:
- Read every biquad coefficient, gain, delay, mute state, compressor param, mixer routing directly from hardware when the device exposes authoritative state
- Expose via `GET /devices/{id}/config` returning full state as JSON
- Enable true status display, measurement mode (save/flatten/measure/restore), preset export, and write verification

Verified reality today:
- Desktop implements the config surface, but some DSP100 fields still come from verified shadow state instead of authoritative live hardware readback.
- Desktop daemon preset-content writes now also mirror the official app's explicit active-preset token save, so that verified shadow state has the same freshness marker the official app relies on when direct runtime readback is missing.
- Desktop daemon can now also import missing preset shadow from the official Device Console `setting*.xml` files when the XML timestamp matches the live preset token, so a fresh attach can recover sane controller-side state even for presets the daemon did not write itself.
- Desktop status reads also stay compatible on the physical 2x4 HD when DSP100 meter polling misbehaves: the daemon now treats those meter reads as best-effort and clamps failures to `-140 dB` silence instead of breaking `minidsp -o jsonline` / `GET /devices/{id}` consumers.
- ESP32 physical-hardware validation has only closed status/meter/basic-control paths so far.
- This removes the previous ESP32 config-route parity blocker on the physical 2x4 HD. Broader device-matrix coverage still depends on the importer/codegen work tracked in `PROGRESS.md`, `docs/ref-device-coverage.md`, and the permanent device reference docs.
