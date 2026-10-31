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

Connects to minidsp-rs compatibility WebSocket at `ws://{wsIp}/devices/{wsDeviceId}?levels=true&poll=true` for real-time level data and state change notifications. The `wsIp` config value includes the port (default `127.0.0.1:5380`), and `wsDeviceId` specifies the zero-based device index. For a single-device daemon, that means `wsDeviceId: 0`. Uses Autobahn Twisted WebSocket client with reconnecting factory.

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
    wsDeviceId: 0
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

ezBEQ requires `device_type` in its device config. We auto-detect from `hw_id` in the device's master status response. Every hw_id is mapped in our device registry. The `--force-kind` flag exists only as an escape hatch.

### ezBEQ implementation details for Phase 2

Verified against ezBEQ source (2026-04-03). Not needed for Phase 1 compatibility, but critical for Phase 2 ESP32 ezBEQ:

- **`device_type` config key:** Maps to built-in descriptors: `24HD`, `DDRC24`, `DDRC88`, `HTx`, `4x10`, `10x10`, `SHD`, `8x12CDSP`. Default is `Minidsp24HD`. SHD and DDRC24 share the same PEQ layout internally.
- **Slot switching:** ezBEQ prepends `config {n}` to batch files when the target slot differs from the active one. `slotChangeDelay` config option adds a delay after slot change for Flex devices.
- **`sw_channels`:** Config key that overrides which output channels receive BEQ filters (used for DDRC88/HTx).
- **ezBEQ REST API surface:** Upstream Python exposes `/api/1/devices/{name}/config/{slot}/active`, `/api/1/devices/{name}/mute`, `/api/1/devices/{name}/gain`, `/api/1/devices/{name}/levels`, `/api/2/devices` (all devices with refresh), `/api/3/devices/{name}` (PATCH with MV adjust), `/api/1/search`, `/api/1/catalogue/{id}/details`, `/api/1/authors`, `/api/1/audiotypes`, `/api/1/years`, `/api/1/contenttypes`, and `/api/1/version`. The hosted React levels page actually uses the WebSocket `subscribe levels {device_name}` flow rather than the REST `levels` helper, but keeping `/api/1/devices/{name}/levels` on ESP32 is still useful for broader API parity and non-browser consumers.
- **ezBEQ WebSocket protocol:** Server sends JSON `{"message": "DeviceState"|"Catalogue"|"CatalogueEntries"|"Levels"|"Error", "data": ...}`. Client sends `"load catalogue"` and `"subscribe levels {device_name}"`. `Levels` messages must include `data.name`, `data.levels`, and a real monotonic `data.ts` in seconds; the hosted React levels plot uses that timestamp as the x-axis. Catalogue entries are sent in chunks.
- **`ignoreRetcode`:** Config flag that suppresses exit code checking from minidsp CLI.

## ESP32 hosted ezBEQ contract

We consume ezBEQ, which is 3ll3d00d's product. We serve the upstream **2.12.4**
React build and implement its backend contract in firmware. We do not fork the
React components or change its generated JavaScript/CSS. The only change to the
upstream HTML is a script tag loading the ESP32 catalogue transport adapter.
Catalogue content and published device profiles remain upstream data, refreshed
automatically; adopting a new UI version is a deliberate compatibility change.

### Bundle and storage

Build with `mkdir -p DIR && uv run tools/build-catalogue.py --output-dir DIR`.
Boards download the published copy from GitHub Pages
(`https://averyy.github.io/minidsp-open`), which `.github/workflows/catalogue-sync.yml`
rebuilds every four hours when the BEQ catalogue changes. Scheduled runs only
start once Pages is enabled for the repository and the `CATALOGUE_PAGES_ENABLED`
repository variable is `true`; a manual run always goes.
The builder fails if manifest files plus the manifest exceed **8,000,000 bytes**.
This leaves filesystem/update headroom in the `0x9E0000` storage partition.
The updater downloads only `manifest.json`'s `files` entries:

| File | Purpose |
|------|---------|
| `catalogue.bin` | BEQ v3, 96 kHz coefficients and digest-prefix binary index |
| `database.json.gz` | Compressed slim browser listing, served as `/database.json` with gzip encoding |
| `entry-metadata.jsonl` | `[id, title, author]` per entry; catalogue.bin records point at each row |
| `recent-changes.jsonl` | What's New rows from the last 180 days, newest first |
| `authors.json`, `filterauthors.json`, `years.json`, `audiotypes.json`, `contenttypes.json`, `languages.json` | Startup facets; years are descending integers |
| `version.txt` | Installed site version, including catalogue, UI and payload identity |
| `catalogue-built-at.txt` | Snapshot timestamp for feed defaults before the board has a wall clock |
| `ui/*` | Upstream UI, WOFF2 fonts and shared shim; unused gzip/Brotli/WOFF siblings are omitted |
| `licenses/*` | ezBEQ's LICENSE extracted from the pinned wheel, beqcatalogue's published LICENSE and attribution |

`database.json` (the full published database) and `overviews.json.gz` (digest →
overview) are **Pages-only assets**. They are not in the manifest and must never
be downloaded to the ESP32. Each manifest file is SHA-256 checked by the updater.
All bundled licenses are served at `/licenses/ezbeq-LICENSE.txt`,
`/licenses/beqcatalogue-LICENSE.txt` and `/licenses/ATTRIBUTION.txt`.

Updates plan changed files by their 4096-byte allocation sizes, with shrinking
files first and `ui/index.html` last. The updater keeps each old file until its
replacement verifies when space permits, otherwise deletes each changed file
just before downloading its replacement. Both plans reserve a 64 KiB margin.
The durable incomplete journal prevents partial updates from appearing current;
version commit and obsolete-file reclamation follow successful downloads.
Obsolete UI assets remain until commit so the previous page can still load them.

BEQ v3 has a 12-byte header: `BEQ\x03`, little-endian u32 entry count and sample
rate. A sorted index contains 8-byte digest prefixes and u32 absolute record
offsets. Each variable record contains a u8 filter count (high bit means
optimised), f32 MV adjustment, count × five f32 coefficients, then a u32 byte
offset into `entry-metadata.jsonl`. That row carries the full digest, formatted
title and author, so firmware resolves slot metadata without browser request
rewriting. Duplicate full digests keep the first published entry, as upstream does.
Distinct digests sharing a prefix fail the build. Legacy
BEQ v2 indexes remain readable during upgrade, but have no title/author index;
legacy loads use the digest as their title until the bundle updates.

For `float32-96k`, the builder consumes `beqcatalogue/docs/devices/index.json`
and the indicated profile file, verifying its SHA-256, schema, rate, precision
and loading model. Published decimal strings are converted to float32 in
`b0,b1,b2,a1,a2` order. Feedback is already additive: **do not negate it again**.
An entry uses published coefficients only when its full digest matches and its
optimised count equals the authored count. Missing/malformed entries, or an
unavailable/invalid profile, use authored coefficients. Only the selected set
is stored. A bundle build reports optimised/fallback counts and per-file sizes.

The firmware checks the binary rate against the detected DSP rate **before
changing presets or writing coefficients**. A 48 kHz device cannot load this
96 kHz catalogue. Published float32 optimisation also requires a float32 DSP.
Additional sample rates and device-specific BEQ output routing require a future
bundle/backend change; they are not silently approximated.

### Thin browser adapter

`app-esp32/static/ezbeq-shim.js` is the sole source. The builder copies it into
`ui/ezbeq-shim.js`; firmware serves that installed copy at `/ezbeq-shim.js`, with
its embedded copy as the fallback when the bundle is absent.

The stock 2.12.4 `StateService` keeps entries in React memory, tracks catalogue
version/count, and reloads on version changes or incomplete reconnects. It does
**not** persist the catalogue across page loads. The shim intercepts
`load catalogue`, loads the same-origin compressed listing, and delivers stock
`CatalogueEntries` messages. IndexedDB caches that listing by the installed site
version announced in the upstream Catalogue message, avoiding a duplicate HTTP
version check and repeated transfers of roughly 10 MB of decoded metadata
through the board's single httpd task. Upstream retains responsibility for
reconnects, heartbeat probes, UI state, filtering and selection. Firmware sends
API failure explanations through the stock state socket Error message because
the upstream HTTP service reports only the response status. PUT and PATCH
entry uploads use the stock requests; firmware resolves title and author by
digest from the installed catalogue and metadata feed. The adapter does not
wrap mutation fetches, format titles or recompute snapshot freshness.

After sending the listing, the adapter lazily fetches `overviews.json.gz` from
`/debug/updater`'s configured `updaterBaseUrl`, defaulting to
`https://averyy.github.io/minidsp-open`. It handles both raw gzip on Pages and
hosts that apply Content-Encoding, caches summaries in IndexedDB by site
version, and publishes another normal entry update when ready. Offline,
unsupported decompression or CORS failures silently leave the listing usable.
It never downloads the full upstream database for overview hydration.

### HTTP and WebSocket behaviour

One USB device is exposed as `master`, with four one-based slots. REST mutations
return the plain upstream device object; `/api/2/devices` wraps it by name and
`DeviceState` wraps it in a WS message. State includes `connected`, slot
`optimise`, input/output gains/mutes, and loaded `coefficients`/`profile`. A
slot's `active` flag is the MiniDSP's current preset, read with the master status,
so a preset change from anywhere else (HTTP API, TCP bridge, snapshot restore,
the remote) shows; slot titles are what ezBEQ last loaded.
`optimisation` is deliberately **null**: 2.12.4's `Optimisation.jsx` then hides the
sticky toggle and profile search/comparison while still showing a loaded slot's
Optimised chip. The bundle always selects published optimisation when available;
it does not store both sets or offer a nonfunctional switch. An explicit PATCH
request disabling optimisation returns 400. Authored fallback is `standard`.

Supported routes include:

- `GET /api/2/devices`, startup facet endpoints, `/api/1/version`, and device levels.
- `PUT/DELETE /api/1/devices/master/filter/{slot}` and
  `PUT /api/1/devices/master/config/{slot}/active`.
- `PATCH /api/3/devices/master` for master volume/mute, entry loads/clears, input
  gains/mutes, output gains/mutes and slot activation. Controls are applied before
  entry loads/clears, then the presence of `active` activates the slot (including
  `active: false`, matching upstream). Advertised controls write through
  `DeviceState`; fields/types/channels/ranges are validated before mutations.
- `PUT /api/1/devices/master/commands` for miniDSP `bq` coefficient text, selected
  one-based inputs/outputs and slot, and overwrite bypass of unused bands.
  Empty command text with overwrite enabled bypasses all selected bands.
  Successful uploads return device state and mark input loads CUSTOM. The shared
  `protocol/src/biquad.rs` provides validation, not parametric design or a command
  interpreter: `filt` and `rs` return a clear 400 rather than claiming success.
- `GET /api/1/whats-new?since=<Unix-seconds>&limit=<integer>` streams the precomputed
  feed, sorted by `max(created_at,updated_at)` descending. Defaults are two weeks
  and 50 entries; nonpositive limits are unbounded, like upstream. Fresh/Updated/
  Stale use upstream's inclusive two-week cutoff and created-before-updated
  priority, with Unknown for missing timestamps. One builder function computes
  freshness for both listing and feed at the catalogue snapshot's build time.
  Before the ESP32 has a synchronised wall clock, the default two-week window
  uses `catalogue-built-at.txt`; explicit `since` queries retain their meaning.

The `/ws` handler tracks up to four independent sessions and queues closure of
the oldest beyond the bound, within httpd's 13-socket limit. State sessions get
DeviceState and Catalogue broadcasts. `subscribe levels master` marks only
that session for 4 Hz Levels; closing Levels leaves the state socket working.
Text `ping` receives JSON `{"message":"Pong"}`, and protocol ping/pong remains
supported. A single PSRAM worker handles levels, catalogue version changes and
coalesced complete state broadcasts. Detached sends never run on the single
httpd task or create a thread per subscriber/update. Close and failed-send paths
close and remove only the affected session. Recipient snapshots and pending
state consumption share the registry lock with new-client registration.

Filter-parameter comparison is intentionally unavailable:
`/api/1/catalogue/filters` returns a JSON 404 and the stock comparison view shows
“Unable to load filters”. Composite devices and multiple catalogue sources are
not implemented by this single-device adapter.

### Deliberately advancing the UI pin

`UI_VERSION` and `UI_WHEEL_SHA256` in `tools/build-catalogue.py` pin the PyPI
`ezbeq-2.12.4-py3-none-any.whl` to
`72873c702dbdcf0b55d00b63595d3580fc750cafc8e27c9f7d89efc5e5c66873`.
The downloaded wheel bytes must match; the builder never follows latest PyPI.

For a bump, inspect the proposed upstream tag's React services/components and
backend contract, update the version and wheel digest from that version's PyPI
release metadata, implement any firmware contract changes, then rebuild. Verify
startup facets/feed, browse/load/clear, metadata on PUT/PATCH, gains/mutes,
custom uploads/errors, Levels round-trip navigation, multiple clients, heartbeat,
reconnect and offline reload in a real browser before publishing the bundle.
Keep catalogue data automatic; do not add a separate UI fork to accommodate an
unreviewed release.

Validation commands:

```bash
uv run tools/test-build-catalogue.py
cargo +stable test
tools/esp-cargo build
mkdir -p /tmp/ezbeq-site
uv run tools/build-catalogue.py --output-dir /tmp/ezbeq-site
```

Local firmware/storage seeding remains `tools/flash-esp32.sh --board-url
http://BOARD:5380`. It builds the bundle and flashes only manifest-listed files
into LittleFS. The runtime updater subsequently checks the configured remote
site. `tools/flash-esp32-with-site.sh` remains the explicit updater-path test.

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
- ESP32 uses the same read-back and shadow model for config, snapshot backup/restore and ezBEQ writes; broader device coverage still needs hardware validation.
- This removes the previous ESP32 config-route parity blocker on the physical 2x4 HD. Broader device-matrix coverage still depends on the importer/codegen work tracked in `PROGRESS.md`, `docs/ref-device-coverage.md`, and the permanent device reference docs.


## Current compatibility verification limits

The [offline source/host audit](NOTES-compatibility-audit.md) pins the upstream
CLI and WebSocket consumers and verifies their JSON shapes with fake devices.
The actual ezBEQ process has not been rerun against the changes from that audit.
The current hosted 2.12.4 bundle still needs its browser Levels/upload/reconnect/
offline-reload check. Recorded physical control-path results cover the 2x4 HD;
they do not certify other devices or the new desktop-only `/config` aliases.
