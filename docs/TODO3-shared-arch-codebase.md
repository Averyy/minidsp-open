# Shared Architecture: Second Pass

## Current status

This plan still has open work. `build_peq_shadow`, scalar sanitation, and raw
write parsers remain separate in [the desktop client](../minidsp/src/client.rs),
[desktop shadow module](../minidsp/src/client/shadow.rs), and
[ESP32 device layer](../app-esp32/src/device.rs). Shared protocol/playback and
bridge response encoding already exist; the helper/planner extraction,
persistence policy, and API metadata questions below aren't complete.
Completed shared features are recorded in [CHANGELOG.md](../CHANGELOG.md).

## Goal

Reduce duplicated device and API behavior between ESP32 and desktop/Linux without forcing both targets into the same runtime model.

The project priority order is:

1. ESP32 firmware is the primary product target.
2. Desktop/Linux remains important as the fastest dev, test, and debug harness.
3. Raspberry Pi and similar Linux targets should ride on the desktop/Linux path, not require a separate architecture.

This document is a second-pass cut at what should be shared, what should stay platform-specific, and how to get there incrementally.

## Current Reality

The codebase already has one strong shared layer:

- `protocol/`
  - device profiles
  - DSP tables
  - command encoding/decoding
  - packet framing
  - discovery packet model
  - compatibility tests and extracted official-app coverage

Above that, the code is only partially shared.

Desktop/Linux path today:

- `minidsp/`
- `daemon/`
- `app-desktop/`

ESP32 path today:

- `app-esp32/`
- shared `protocol/`

That means the project currently has:

- shared protocol and device metadata
- duplicated higher-level device behavior
- duplicated HTTP/API behavior
- duplicated TCP bridge / discovery behavior
- different runtime and transport implementations

This is not inherently wrong. It is a reasonable place to be while proving out the ESP32 target. The cost is that feature work above the protocol layer often becomes "implement twice and keep in sync."

## Second-Pass Findings

After reading the code more closely, the recommendation needs to be narrower and more concrete than the first pass.

### 1. There is real, literal duplication in pure helper logic

Some helpers are duplicated almost verbatim between desktop and ESP32:

- `build_peq_shadow`
- `sanitize_shadow_preferred_scalar`
- raw write parsers:
  - `parse_raw_write_biquad`
  - `parse_raw_bypass_filter`
  - `parse_raw_selector_write`
  - `parse_raw_linked_selector_write`
- `default_peq_frequencies`
- most of `device_info`

These are strong candidates for immediate sharing because they are:

- small
- pure
- already behavior-critical
- already duplicated

### 2. Several higher-level workflows are the same in meaning, but not in mechanics

Examples:

- measurement-mode save / restore
- preset verification after `SetConfig`
- PEQ reset-to-defaults
- full config read/write
- raw-command shadow mirroring

The semantics are similar across targets, but desktop often uses:

- async batching
- queue priority management
- token-aware shadow persistence

while ESP32 uses:

- blocking per-command loops
- explicit operation locks
- simpler NVS-backed persistence

That means the shared target here is the workflow planning and state-machine rules, not the full control flow.

### 3. The API layer is more divergent than the first pass implied

Desktop already has typed JSON DTOs in `daemon/src/http/json.rs`.

ESP32 does not simply "re-register the same handlers differently." It also:

- manually reads request bodies
- manually parses `serde_json::Value` for large config writes
- manually builds large config JSON responses
- manually builds OpenAPI JSON
- inserts sleeps while formatting large responses to avoid starving the watchdog / IDLE task

So "share the API contract" is still worthwhile, but it is not the best first refactor target.

### 4. Persistence is not symmetric

Desktop currently has a richer model:

- `DeviceShadowState`
- token-aware preset shadow filtering
- import from official app XML snapshots
- filesystem cache

ESP32 currently has a much narrower persistence model:

- NVS-backed PEQ shadow only

This means "shared persistence" should not be an early goal. It would likely drag ESP32 complexity up or desktop capability down.

### 5. Bridge/discovery behavior is duplicated, but the network shells should stay separate

The duplicated value is mostly in:

- packet content rules
- `SetConfig` / `Reset` bridge behavior
- appliance-vs-device advertisement decisions

The socket loops and server lifecycle are still genuinely platform-specific.

## Main Decision

The right target is not "one codepath for everything."

The right target is:

- shared protocol
- shared device/business logic
- shared API contract and JSON shapes
- thin platform adapters for ESP32 and desktop/Linux

The wrong target is:

- one giant abstraction that pretends `tokio + hidapi + axum` and `blocking ESP-IDF + USB host FFI + EspHttpServer` are the same environment

That would make the code harder to reason about and slower to change.

One more constraint from the second pass:

- do not push service-level shared logic into `protocol/`

`protocol/` is already the clean low-level truth for wire format, device metadata, and protocol compatibility. Service/workflow logic belongs in a separate shared layer once the first extractions justify one.

## Why Share More Logic

Shared logic is worth it when the behavior should stay identical across targets.

Examples:

- measurement-mode entry/restore semantics
- preset-switch verification and timeout recovery
- raw-bridge command policy for `SetConfig` / `Reset`
- shadow-state handling for unreliable live readback
- modification-token persistence rules
- config/status assembly
- bass-management read/write semantics
- route / gain / mute / invert write rules
- default-PEQ reset and default-frequency logic

If those stay duplicated, every fix must be done twice and parity drift becomes a constant tax.

For new devices, shared higher-level logic helps because:

- the device table lands once in shared metadata
- support checks and capability gates land once
- platform targets automatically inherit the same feature semantics

For new features, shared logic helps because:

- the core behavior is implemented once
- each target only needs a transport/server binding layer

## Why Not Share Everything

Some parts should remain separate because the platform constraints are genuinely different.

Examples:

- USB transport implementation
- HTTP server framework
- WebSocket implementation details
- tasking model and concurrency primitives
- Wi-Fi provisioning
- NVS / flash storage
- OTA / updater
- ESP32 diagnostics and ring-buffer logging
- CPAL / Linux audio backend work
- process/service management on Linux

Trying to force those into a common abstraction too early will make the architecture worse, not better.

## Share This

### 1. Pure helper duplication that is already nearly identical

Best immediate candidates:

- `build_peq_shadow`
- raw-write shadow parsers
- scalar shadow sanitation helpers
- `default_peq_frequencies`
- common device-info assembly helpers

These are the safest first moves because they do not force runtime or storage decisions.

### 2. Workflow planners and state-machine rules

Primary candidates:

- measurement-mode save / bypass / restore sequencing
- preset-switch verification behavior
- reset verification behavior
- token update policy
- raw command side-effect classification
- bass-management patch application
- PEQ reset/default planning
- config/status assembly planning

Current duplicated homes:

- `minidsp/src/client.rs`
- `app-esp32/src/device.rs`

Goal:

- move target-independent planning into shared helper modules
- leave transport invocation, batching, locking, retry style, and persistence hooks platform-local

### 3. Bridge/discovery behavior

Primary candidates:

- raw bridge command handling policy
- `SetConfig` / `Reset` special handling
- discovery packet content rules
- appliance-vs-device advertisement rules

Current duplicated homes:

- desktop: `daemon/src/bridge.rs`, `daemon/src/tcp/mod.rs`, `daemon/src/discovery/mod.rs`
- ESP32: `app-esp32/src/main.rs`, plus device-local raw-command handling

Goal:

- share the behavior and packet decisions
- keep socket/event-loop code platform-local

### 4. API DTOs and validators, but only after the helpers above land

Primary candidates:

- typed request/response structs
- common validation helpers
- compatibility error mapping
- route/OpenAPI parity metadata

Current duplicated homes:

- desktop: `daemon/src/http/json.rs`, `daemon/src/http/routes.rs`
- ESP32: `app-esp32/src/http_server.rs`

Goal:

- share models where it reduces drift
- do not force ESP32 to give up manual request/response handling where watchdog/body-size constraints require it

### 5. Shared pure utilities

Primary candidates:

- sweep generation / playback planning
- DSP config transforms
- route-table helpers
- default-value generators
- capability / feature gating helpers

These are high-value because they are easy to test on desktop and easy to reuse on ESP32.

## Do Not Share This

### 1. USB transport implementations

Keep separate:

- `minidsp/src/transport/hid.rs`
- `app-esp32/src/usb_hid.rs`

Reason:

- desktop uses `hidapi`
- ESP32 uses raw ESP-IDF USB host APIs and different hotplug/failure behavior

These can conform to the same conceptual contract without being literally merged.

### 2. HTTP server shells

Keep separate:

- `daemon/src/http/routes.rs` route registration and axum integration
- `app-esp32/src/http_server.rs` route registration and `EspHttpServer` integration

Reason:

- framework and execution model differ too much

What should be shared is the handler logic behind the routes, not the route-registration layer itself.

### 3. Persistence backends and most persistence policy, at least initially

Keep separate:

- desktop token-aware shadow cache behavior
- official-app XML import path
- ESP32 Wi-Fi provisioning
- ESP32 NVS persistence
- ESP32 updater / OTA
- ESP32 diagnostics
- Linux filesystem/service concerns
- platform audio backend integration

Reason:

- these are not currently symmetric
- the desktop side is already richer than the ESP32 side
- premature unification here is likely to regress a working path or overcomplicate the primary target

## Proposed Layer Split

This does not have to mean new crates immediately, but the conceptual split should look like this.

### Layer 1: Protocol and device metadata

Already shared:

- `protocol/`

### Layer 2: Shared device service logic

New or extracted shared layer:

- target-neutral workflows built on top of a small device-I/O interface
- no `axum`, no `EspHttpServer`, no `hidapi`, no `esp-idf-sys`
- minimal assumptions about async vs blocking

Possible shape:

- shared pure modules first
- if those prove useful, a small new crate for service-level helpers
- only a very small adapter surface for "read/write command" and "persist shadow state" if I/O is unavoidable
- shared functions for:
  - state/workflow planning
  - measurement mode
  - preset/reset verification
  - BM patch application
  - shadow bookkeeping
  - discovery/bridge decisions

### Layer 3: Platform adapters

Desktop/Linux adapter:

- `minidsp/`
- `daemon/`
- `app-desktop/`

ESP32 adapter:

- `app-esp32/`

These adapters should:

- expose transport primitives
- own locks / concurrency choices
- own persistence backends
- own HTTP registration and socket servers
- call shared layer functions for actual device behavior

## Async vs Blocking

This is the main design constraint.

Desktop is async-heavy:

- `tokio`
- async transport trait
- axum
- async sockets

ESP32 is mostly blocking:

- `EspHttpServer`
- blocking USB host operations
- thread-per-task style

Because of that, the shared layer should not be "desktop async code imported into ESP32."

Preferred direction:

- shared pure helpers first
- shared state machines / workflow functions second
- async/blocking wrappers per platform around the shared logic

If a shared helper needs I/O, prefer one of these patterns:

- a small sync-style callback interface used from both targets
- duplicated thin wrappers that gather I/O and call a shared pure workflow

Avoid:

- pulling `tokio` into the ESP32-side business logic
- forcing desktop to adopt blocking-style architecture just for parity

## Recommended Refactor Order

### Phase 1: Extract the obvious pure duplicates

Low-risk extractions:

- `build_peq_shadow`
- raw-write shadow parsers
- scalar sanitation helper
- default-PEQ frequency helper
- small device-info helper pieces

This gives immediate parity wins with almost no architectural risk.

Important constraint:

- do this first as extracted modules/helpers in the existing codebase
- do not start by creating a new shared crate

### Phase 2: Share workflow planners, not transport control flow

Extract shared helpers for:

- measurement-mode save/restore ordering
- preset verification decisions
- token bumping classification
- PEQ reset/default-frequency logic
- BM patch-to-write planning
- possibly config read/write planning where the output is a list of device actions, not a server response

This is the highest-value tier because it removes "implement twice" behavior without forcing runtime unification.

### Phase 3: Unify bridge/discovery behavior

Extract:

- shared discovery packet builder
- shared raw-bridge command policy helpers

Keep:

- socket listeners
- accept loops
- per-platform connection management

### Phase 4: Re-evaluate shared API DTOs and validators

Only after the planner/helper extractions land.

Reason:

- the ESP32 HTTP layer has real watchdog and body-handling constraints
- typed DTO sharing is good, but it is less valuable than sharing the device behavior underneath

### Phase 5: Re-evaluate whether a new shared crate is justified

At that point, decide whether:

- the extracted helpers should stay as a small shared crate
- or more workflow logic should move into it
- or `minidsp/` should become a thinner desktop adapter over that shared layer

Do not decide this upfront.

## Raspberry Pi Impact

Raspberry Pi support does not require this refactor.

Pi should ride on the existing desktop/Linux stack:

- `minidsp/`
- `daemon/`
- `app-desktop/`

The refactor still helps Pi indirectly because:

- shared device behavior reduces parity drift
- Linux remains the fastest place to test shared behavior
- new platform bring-up stays focused on packaging and runtime validation, not feature reimplementation

But Pi support itself should not be blocked on shared-architecture work.

## ESP32 Priority Guardrails

Because ESP32 is the primary product target, any shared-architecture work must obey these rules:

- never block ESP32 feature delivery on an ideal refactor
- when in doubt, land the ESP32 behavior first and extract afterward
- do not replace proven ESP32-specific code with a "generic" layer unless the generic layer is simpler and already validated
- preserve desktop/Linux as the fast feedback loop for protocol and behavior testing

The desktop path is not just a secondary target. It is also the best developer harness for:

- quick iteration
- unit/integration testing
- log-heavy debugging
- reproducing protocol issues without flash cycles
- future Raspberry Pi deployment

## What Success Looks Like

The project is in a better architectural state when:

- a new device profile mostly lands in shared metadata and just works on both targets
- a new cross-target feature mostly lands in one shared behavior layer
- HTTP parity bugs are fixed once, not twice
- desktop remains the fastest place to validate shared behavior
- ESP32-specific work stays local and does not fight a desktop-first abstraction

## First Concrete Candidates

Good first extractions:

1. Shared raw-write shadow parsers and related helper functions.
2. Shared `build_peq_shadow`.
3. Shared measurement restore planner.
4. Shared preset/reset verification decision helpers.
5. Shared discovery-packet decision helper.

Bad first extractions:

1. A single transport abstraction for both `hidapi` and ESP-IDF USB host.
2. A single HTTP server implementation for both axum and `EspHttpServer`.
3. A single persistence model for desktop cache + ESP32 NVS.
4. A single global async runtime model.

## Questions Answered Now

- Do we need a new shared crate before starting?
  - No. Start with a few extracted pure helpers and planners first, then re-evaluate.
- Should early sharing target the HTTP/API layer?
  - No. The ESP32 HTTP layer has enough real platform-specific behavior that this is not the best first move.
- Should early sharing target persistence?
  - No. Desktop and ESP32 persistence are too asymmetric right now.
- Should service/workflow sharing go into `protocol/`?
  - No. `protocol/` should stay the low-level metadata/wire-format layer.

## Remaining Open Questions

- After the first few extractions, should the shared device-service layer live inside `minidsp/` or move to a new small crate?
- If a shared helper needs I/O, is the right interface sync-style callbacks or pure planners with thin wrappers per platform?
- How much of the desktop shadow/token model should eventually move toward ESP32 behavior, if any?
- Do we want shared route metadata that can generate parity checks for both OpenAPI and route registration?

## Concerns

- Workflow drift is the main risk, not transport drift. Measurement mode, preset/reset verification, raw bridge side effects, and shadow behavior are where the two targets can silently diverge.
- The ESP32 HTTP layer is easy to underestimate. Manual body parsing, manual JSON building, and watchdog-friendly formatting mean an over-eager API unification could slow down the primary target.
- Persistence asymmetry is real. Desktop already has a richer token-aware shadow cache model than ESP32. Treating that as accidental duplication would be a mistake.
- Desktop/Linux must remain the fast dev and test harness. Any refactor that makes that path slower or harder to debug is probably a net loss.

## Recommendation

Yes, move toward a more shared architecture.

But do it selectively:

- share pure helpers first
- share workflow planning next
- share bridge/discovery behavior after that
- share API DTOs only where it actually reduces drift
- do not over-share runtime/framework/platform code

That gives the project the actual upside:

- easier feature work
- easier new-device rollout
- less parity drift
- better use of desktop/Linux as the dev harness

without paying the downside:

- brittle over-abstraction
- slower ESP32 progress
- more confusing platform code

The biggest correction from the second pass is this:

- the best early refactor is not "shared API models"
- it is "shared pure device/workflow helpers that are already duplicated"

That keeps the work aligned with the real codebase instead of the idealized architecture.
