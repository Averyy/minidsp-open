# Offline compatibility audit

Date: 2026-10-29. Hardware: none. Transport: source inspection and in-memory
HTTP/WebSocket/TCP byte streams with two original fake MiniDSP transports.
No upstream implementation code was copied into the project.

Shell GitHub access could not resolve github.com; the available fetch integration
retrieved pinned source into `/tmp`, outside the repository. Local socket binds
are denied by this sandbox; no LAN or physical Device Console claim follows.

## Source baseline and findings

- [minidsp-rs dev 8e987f9](https://github.com/mrene/minidsp-rs/tree/8e987f9d16c580482eda1b52692b3238c18d7ab8).
- [ezBEQ bd981bf](https://github.com/3ll3d00d/ezbeq/tree/bd981bf778f76e5ffc7b0ecbfff94c82c4320321).

| Surface | Upstream evidence | Result |
|---|---|---|
| CLI status/probe | [CLI output and serial ordering](https://github.com/mrene/minidsp-rs/blob/8e987f9d16c580482eda1b52692b3238c18d7ab8/minidsp/src/bin/minidsp/main.rs#L462-L559); [ezBEQ JSON/probe reader](https://github.com/3ll3d00d/ezbeq/blob/bd981bf778f76e5ffc7b0ecbfff94c82c4320321/ezbeq/minidsp.py#L678-L710) | json/jsonline share the compatible status shape; local discovery now sorts by serial; probe can use the local daemon. Version remains above ezBEQ's 0.1.12 gate. |
| CLI selectors | [DeviceKind](https://github.com/mrene/minidsp-rs/blob/8e987f9d16c580482eda1b52692b3238c18d7ab8/protocol/src/device/probe.rs#L19-L49); [USB path selector](https://github.com/mrene/minidsp-rs/blob/8e987f9d16c580482eda1b52692b3238c18d7ab8/minidsp/src/transport/hid/discover.rs#L26-L37); [TCP default](https://github.com/mrene/minidsp-rs/blob/8e987f9d16c580482eda1b52692b3238c18d7ab8/minidsp/src/bin/minidsp/main.rs#L127-L133) | Added m2x4/generic kinds, path= selector and missing default TCP port behavior; encoded USB paths have host tests. Deprecated flags are accepted no-ops, absent from current dev. |
| CLI writes/files | [crossover/compressor grammar](https://github.com/mrene/minidsp-rs/blob/8e987f9d16c580482eda1b52692b3238c18d7ab8/minidsp/src/bin/minidsp/main.rs#L353-L384); [clear semantics](https://github.com/mrene/minidsp-rs/blob/8e987f9d16c580482eda1b52692b3238c18d7ab8/minidsp/src/bin/minidsp/handlers.rs#L240-L243); [ezBEQ crossover generator](https://github.com/3ll3d00d/ezbeq/blob/bd981bf778f76e5ffc7b0ecbfff94c82c4320321/ezbeq/minidsp.py#L1185-L1198); [runner options](https://github.com/3ll3d00d/ezbeq/blob/bd981bf778f76e5ffc7b0ecbfff94c82c4320321/ezbeq/config.py#L194-L224) | Added upstream crossover, routing and compressor forms. PEQ clear now enables identity. Files/stdin use the direct command parser with shell quoting and preserve order; existing command spellings remain accepted. |
| HTTP config | [routing entries](https://github.com/mrene/minidsp-rs/blob/8e987f9d16c580482eda1b52692b3238c18d7ab8/minidsp/src/model.rs#L208-L250); [crossover coeff arrays](https://github.com/mrene/minidsp-rs/blob/8e987f9d16c580482eda1b52692b3238c18d7ab8/minidsp/src/model.rs#L316-L350); [biquad defaults](https://github.com/mrene/minidsp-rs/blob/8e987f9d16c580482eda1b52692b3238c18d7ab8/minidsp/src/biquad.rs#L6-L43) | Desktop now accepts upstream index/mute/gain routing patches and flat indexed crossover coeff arrays without dropping existing forms. Omitted routing controls remain unchanged; coefficients have identity defaults. ESP32 aliases remain open. |
| HTTP status/WS | [HTTP handlers and query options](https://github.com/mrene/minidsp-rs/blob/8e987f9d16c580482eda1b52692b3238c18d7ab8/daemon/src/http/mod.rs#L131-L250); [ezBEQ WebSocket consumer](https://github.com/3ll3d00d/ezbeq/blob/bd981bf778f76e5ffc7b0ecbfff94c82c4320321/ezbeq/minidsp.py#L1068-L1090) | Host test verifies GET omission/POST null for dirac, complete state, levels and CORS. Query option presence now matches upstream. Existing full-snapshot cadence remains documented; new empty error replies become stable JSON. Existing error envelope/status and OpenAPI metadata intentionally differ from dev. |
| TCP | [asymmetric framing](https://github.com/mrene/minidsp-rs/blob/8e987f9d16c580482eda1b52692b3238c18d7ab8/minidsp/src/transport/net/codec.rs#L10-L12); [multiple clients](https://github.com/mrene/minidsp-rs/blob/8e987f9d16c580482eda1b52692b3238c18d7ab8/daemon/src/tcp/mod.rs#L143-L177) | Padded desktop replies now omit checksum. Concurrent normal/padded streams and HTTP reads/writes pass through the same fake device queue. Single-client checklist expectation corrected; real accepts/Device Console remain open. |
| UDP | [packet layout](https://github.com/mrene/minidsp-rs/blob/8e987f9d16c580482eda1b52692b3238c18d7ab8/minidsp/src/transport/net/discovery/mod.rs#L51-L74); [advertisement identity](https://github.com/mrene/minidsp-rs/blob/8e987f9d16c580482eda1b52692b3238c18d7ab8/daemon/src/tcp/mod.rs#L98-L127) | Corrected contract to describe actual 72-byte Wi-DG encoder, rather than falsely claiming upstream byte equality. Configured listeners use selected device identity and synthetic IP-derived MAC. LAN receipt remains open. |
| TOML | [config keys and defaults](https://github.com/mrene/minidsp-rs/blob/8e987f9d16c580482eda1b52692b3238c18d7ab8/daemon/src/config.rs#L5-L84) | Arrays, selectors, nested advertisements, CORS origin, static devices and ignore_advertisements are consumed rather than ignored. Existing singleton TCP/top-level advertise forms remain accepted. Multiple fake-device selection is tested; static/network startup over sockets remains open. |

Deliberate extensions and differences, including `/play`, `/rattle`, ESP32
`/status`, discovery fingerprints, polling cadence, legacy no-op flags, error
responses and metadata routes, are stated in [the contract](ref-compatibility.md).

## Initially unchecked inventory

`docs/ref-compatibility.md` had no literal unchecked checkboxes. Its final freeze
list remains a release target, with JSON/probe examples and TCP/UDP framing now
linked to the source baseline. Below are **every one of the 55** unchecked
`docs/todo-checker.md` items at audit start, in their original order.

| # | Section / original item | Disposition |
|---|---|---|
| 1 | 1.9 Closure for old MiniDSP todo: All former ESP32/MiniDSP investigation items are verified here. | Open: finish the remaining physical/runtime gates before retiring this investigation. |
| 2 | 1.9 Closure for old MiniDSP todo: Any remaining failure is narrower than “HID framing issue”. | Open: live recovery, arbitration and audio checks remain individually scoped in PROGRESS.md. |
| 3 | 1.9 Closure for old MiniDSP todo: No separate investigation todo is needed anymore. | Open: keep the master checklist until the live gates close. |
| 4 | 2.4 Extended compatibility commands: compressor subcommand | Open: upstream flag syntax is now parser-tested; audible retest needs loud material and a threshold that triggers compression. |
| 5 | 2.4 Extended compatibility commands: `--force-kind` | Settled offline: all upstream DeviceKind spellings resolve; added missing m2x4 and generic; parser/resolver regression. |
| 6 | 2.4 Extended compatibility commands: deprecated `--force-device` | Settled offline: accepted no-op retained; absent from audited dev CLI. Use --force-kind for an override. |
| 7 | 2.4 Extended compatibility commands: deprecated `--force-dsp` | Settled offline: accepted no-op retained; absent from audited dev CLI. Use --force-kind for an override. |
| 8 | 2.5 `--url` compatibility paths: `--url usb:%2Fpath?...` probe-style URL | Settled offline: percent decoding/query stripping and encoded platform paths tested; transport opens the decoded HID path. |
| 9 | 2.7 `--all-local-devices`: Verified with 2+ local USB devices attached. | Open: attach at least two physical USB processors. |
| 10 | 2.7 `--all-local-devices`: Confirms fan-out across local HID devices. | Open: exercise reversible writes on both attached USB processors and confirm fan-out. |
| 11 | 2.7 `--all-local-devices`: Confirms current limitation to local USB only is understood and documented. | Settled offline: CLI rejects explicit remote transports; permanent contract documents USB-only fan-out. |
| 12 | 4.1 Basic bridge behavior: TCP bridge accepts one compatibility client. | Open: source accepts clients and in-memory handler works; real socket acceptance could not run in this sandbox. |
| 13 | 4.1 Basic bridge behavior: Second TCP client is rejected as expected. | Settled offline: obsolete rejection expectation corrected. Upstream permits multiple clients; desktop admits up to eight; two stream handlers pass concurrently. |
| 14 | 4.1 Basic bridge behavior: Frame handling works in normal length-prefixed mode. | Settled offline, desktop: in-memory bridge test covers fragmented request frames and checksum-free replies. |
| 15 | 4.1 Basic bridge behavior: WI-DG-style fixed-size packet mode works. | Settled offline, desktop: padded replies fixed to omit checksum; 64-byte stream regression pins padding and payload. |
| 16 | 4.2 Device Console compatibility: Device Console can perform normal interactive operations through the bridge. | Open: running stock Device Console plus MiniDSP required for status/volume/mute/source/preset/routing/PEQ/compressor operations. |
| 17 | 4.3 Coexistence / arbitration: Device Console over TCP plus HTTP status reads coexist correctly. | Open: run Device Console TCP session concurrently with HTTP status reads on a physical processor. |
| 18 | 4.3 Coexistence / arbitration: Device Console over TCP plus HTTP writes coexist correctly. | Open: run Device Console TCP session concurrently with reversible HTTP writes on a physical processor. |
| 19 | 4.3 Coexistence / arbitration: Device Console over TCP plus levels polling coexist correctly. | Open: run Device Console TCP session concurrently with state/levels WebSocket polling on a physical processor. |
| 20 | 4.3 Coexistence / arbitration: No response cross-wiring or obvious queue corruption occurs. | Settled offline for fake devices: concurrent normal/padded bridge streams and real HTTP handlers share the queue without response mixing. Physical Device Console gates remain above. |
| 21 | 5.1 Desktop daemon: Broadcast advertisement works. | Open: packet construction/cadence is covered by host tests; receive real desktop broadcasts on the LAN. |
| 22 | 5.1 Desktop daemon: Multicast advertisement works. | Open: targets include multicast by source/host test; receive real multicast on the LAN. |
| 23 | 5.1 Desktop daemon: `probe --net` discovers the daemon/device. | Open: run probe --net while the daemon advertises on a real interface; socket bind is blocked here. |
| 24 | 5.2 ESP32 without MiniDSP attached: Fallback bridge identity is advertised. | Open: boot an ESP32 without MiniDSP, capture its appliance beacon, and decode its identity. |
| 25 | 5.2 ESP32 without MiniDSP attached: `probe --net` discovers the live board in no-device mode. | Open: run probe --net against that live no-device ESP32. |
| 26 | Section 7: Negative / Error-Path Verification: No-device responses return stable JSON errors. | Settled offline, desktop: GET/POST missing devices return JSON 404; in-memory HTTP test verifies both. ESP32 no-device operation remains section 5.2. |
| 27 | Section 8: Multi-Device / Matrix Verification: 2+ local USB devices tested with desktop daemon. | Open: connect two physical USB devices to the daemon. |
| 28 | Section 8: Multi-Device / Matrix Verification: Device indexing is stable across CLI and daemon. | Open: direct CLI/initial USB daemon sorting is corrected to serial order; confirm stable live mappings across CLI/daemon and topology changes. |
| 29 | Section 8: Multi-Device / Matrix Verification: Equivalent multi-device deployment assumptions from `ref-compatibility.md` are exercised at least once. | Open: config arrays and two fake device selectors are tested; exercise two actual listeners/devices on distinct addresses. |
| 30 | Section 8: Multi-Device / Matrix Verification: Any limitation found is recorded in `PROGRESS.md`. | Settled: current runtime, socket and ESP32-alias limitations are recorded in PROGRESS.md. |
| 31 | 9.1 Desktop binary-swap flow: Actual ezBEQ process reads `minidsp -o jsonline` successfully end to end. | Open: run the actual ezBEQ process with this binary and a real device or protocol simulator; source consumers are inspected. |
| 32 | 9.1 Desktop binary-swap flow: Actual ezBEQ process connects to `/devices/{id}?levels=true&poll=true` successfully end to end. | Open: run actual ezBEQ with the state/levels WebSocket; in-memory HTTP/WS client tests cover the wire shapes. |
| 33 | 10.1 Former ESP32/MiniDSP Investigation Todo: Every open claim is either verified or superseded. | Open: physical runtime closure is still required. |
| 34 | 10.1 Former ESP32/MiniDSP Investigation Todo: Any remaining useful historical context is copied into a permanent reference doc if needed. | Open: complete the investigation closure review after the live gates. |
| 35 | 10.1 Former ESP32/MiniDSP Investigation Todo: No separate temporary todo file is needed. | Open: retain this checklist while runtime gates remain. |
| 36 | 10.2 Former Compatibility Audit Todo: Every item under “Remaining Gaps” is either verified or moved to `PROGRESS.md` as an active issue. | Settled: all initial unchecked items have explicit dispositions here and runtime follow-ups in PROGRESS.md. |
| 37 | 10.2 Former Compatibility Audit Todo: Every item under “What We Can Honestly Claim Right Now” is updated to match verified reality. | Settled: reference docs distinguish the source/host baseline from live hardware and application verification. |
| 38 | 10.2 Former Compatibility Audit Todo: No separate temporary todo file is needed once no validation gap remains. | Open: validation gaps remain, so the checklist cannot be retired. |
| 39 | 10.3 `docs/ref-compatibility.md`: Contract matches verified desktop behavior. | Settled for audited desktop surfaces: reference contract now describes supported aliases, framing, source-backed differences, and host-test limits. |
| 40 | 10.3 `docs/ref-compatibility.md`: Contract matches verified ESP32 behavior where parity is claimed. | Open: new desktop HTTP routing/crossover aliases are not implemented on ESP32; verify parity after matching request support. |
| 41 | 10.3 `docs/ref-compatibility.md`: No aspirational or unverified wording remains. | Open: full live-contract freeze remains gated; target requirements are distinguished from verified results. |
| 42 | 10.3 `docs/ref-compatibility.md`: Any intentional limitation is called out explicitly. | Settled: UDP fingerprints, HTTP error/metadata differences, polling cadence, deprecated no-ops and transport limits are explicit. |
| 43 | 10.4 `docs/ref-ezbeq.md`: Desktop binary-swap compatibility claims match verified reality. | Settled docs review: binary-swap target is distinguished from the still-pending actual ezBEQ process rerun. |
| 44 | 10.4 `docs/ref-ezbeq.md`: ESP32 embedded-ezBEQ claims match verified reality. | Open: current hosted 2.12.4 bundle needs browser Levels/upload/reconnect/offline-reload verification. |
| 45 | 10.4 `docs/ref-ezbeq.md`: The doc does not imply broader real-hardware validation than was actually performed. | Settled docs review: only recorded 2x4 HD physical results are claimed; broader families are unverified. |
| 46 | 10.4 `docs/ref-ezbeq.md`: Any remaining caveat is phrased as a current fact, not hidden in TODO files. | Settled docs review: current runtime/browser/alias caveats are stated in the reference docs. |
| 47 | 10.5 `PROGRESS.md`: Current status distinguishes implementation from hardware verification. | Settled: PROGRESS.md separates implementation/host evidence from physical verification. |
| 48 | 10.5 `PROGRESS.md`: Any failed verification is added as a concrete open item. | Settled: blocked socket validation and unported ESP32 aliases have concrete follow-ups in PROGRESS.md. |
| 49 | 10.5 `PROGRESS.md`: Open verification, deferred work, and next steps reflect the post-test reality. | Settled: remaining Device Console, ezBEQ, LAN, multi-device and compressor gates are listed explicitly. |
| 50 | 10.5 `PROGRESS.md`: User-visible changes are in [CHANGELOG.md](../CHANGELOG.md) under Unreleased; `PROGRESS.md` has no dated history. | Open for lead: CHANGELOG.md is intentionally untouched under the delegated task instruction. |
| 51 | Final Go / No-Go: Desktop compatibility claim is justified. | Open: finish real socket/LAN, multi-device and actual-client reruns before broad desktop signoff. |
| 52 | Final Go / No-Go: ESP32 compatibility claim is justified. | Open: finish ESP32 runtime and current UI checks, including alias parity before extending that claim. |
| 53 | Final Go / No-Go: Device Console coexistence claim is justified. | Open: requires running Device Console arbitration tests on physical hardware. |
| 54 | Final Go / No-Go: ezBEQ compatibility claim is justified. | Open: requires actual ezBEQ process and latest hosted bundle reruns. |
| 55 | Final Go / No-Go: The old TODO docs can be retired. | Open: retain the checklist until the remaining release gates close. |

## Contract freeze items without checkboxes

| Original final-freeze item in ref-compatibility.md | Offline disposition |
|---|---|
| jsonline sample | Existing canonical compact StatusSummary example retained; host HTTP/WS tests assert master/level fields and omission rules. |
| probe line | Existing indexed product/serial/URL example retained; upstream line and ezBEQ regex inspected; live transport URL rerun remains open. |
| HTTP bodies for PEQ/mute/gain/source/preset | Existing examples retained; routing/crossover aliases and defaults added; fake HTTP test pins GET versus POST null behavior. |
| state WebSocket shapes | Immediate complete state and periodic levels verified in memory; snapshot-versus-delta/cadence difference documented. |
| raw WebSocket bridge | Distinct framed binary endpoint retained; upstream forwards binary transport frames. Existing ignored-text behavior is more permissive than upstream's close-on-nonbinary. Fresh live replay remains open. |
| TCP frame transcript | Source-derived request and checksum-free reply examples frozen; normal/padded fragmentation and concurrency exercised in memory. |
| UDP bytes | Source-derived upstream samples retained as comparison; actual shared 72-byte Wi-DG magic/tag/filler and hostname limit corrected explicitly. Fresh LAN capture remains open. |

## Host validation

- `cargo test --workspace` — passed, including original fake-device HTTP/WS and concurrent normal/padded bridge regressions.
- `cargo clippy --workspace --all-targets -- -D warnings` — passed with zero warnings.
- `cargo fmt --all -- --check` — passed.

No ESP32 sources or shared protocol behavior were changed by this audit; no
ESP32 build was required for these desktop-only fixes. CHANGELOG.md remains the lead's task.
