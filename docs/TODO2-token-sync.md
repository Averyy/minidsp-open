# ESP32 Token-Gated Preset Cache

## Goal

Current scope:

Keep these two views of the same MiniDSP aligned as far as the ESP32 can truthfully observe them:

1. the physical MiniDSP device
2. the ESP32 `minidsp-open` bridge and its `/devices/0/config` shadow state

Target behavior:

- ESP32 must not restore stale preset state after some other controller changed the device
- ESP32 should keep working even when one preset is stale
- ESP32 should continue passing commands through to the MiniDSP even if it no longer trusts some cached values
- ESP32 should report unknown or stale shadow-backed values conservatively instead of guessing

Explicit non-goal for the main implementation:

- keeping the official MiniDSP Device Console `setting*.xml` cache in perfect lockstep is useful future work, but it is not required for the firmware-side fix described here

Assumption for the rest of this document:

- the official app is talking to the MiniDSP through `minidsp-open`'s ESP32 bridge path
- if the official app is talking direct USB to the MiniDSP from some other machine, the ESP32 cannot stay in lockstep without an additional host-side agent or a different topology

## Current Facts From This Repo

### 1. Modification tokens already are the shared freshness marker

The MiniDSP stores four 4-byte per-preset modification tokens at `0xFFC8` (`4 presets x 4 bytes`).

References:

- [protocol/src/eeprom.rs](./protocol/src/eeprom.rs:67)
- [docs/ref-command-pacing.md](./docs/ref-command-pacing.md:100)

Important implication:

- the token lives on the MiniDSP, not inside ESP32 and not inside Device Console
- the problem is not inventing a new sync marker
- the problem is making both controllers respect and update the existing one coherently

### 2. Device Console does not have a hidden authoritative readback path

The official app does not appear to reconstruct full DSP100 / 2x4 HD preset state from some secret runtime readback. On first attach without a matching local cache it falls back to initialize/disconnect behavior.

References:

- [docs/NOTES-cli-verification.md](./docs/NOTES-cli-verification.md:272)
- [docs/ref-widg-reimplementation.md](./docs/ref-widg-reimplementation.md:356)
- Official 2x4 HD Device Console manual: `It is not possible to load a preset from the 2x4 HD back to the Device Console.` (<https://docs.minidsp.com/product-manuals/2x4-hd/device-console.html>)
- Official Device Console docs: the template device mirrors the real device when you change presets and when you disconnect, and editing from a different computer triggers the same changed-settings dialog (<https://docs.minidsp.com/getting-started/device-console.html>)

This is the key ceiling:

- tokens can tell us whether a cached preset is still valid
- tokens cannot reconstruct unseen preset contents by themselves

### 3. Device Console's local cache is tied to the device token

The desktop daemon already imports the official app's `setting*.xml` files when their `<timestamp>` matches the live device token for that preset. The parser also confirms the file contains PEQ/filter and selector/scalar state, not only a token.

References:

- [daemon/src/state.rs](./daemon/src/state.rs:218)
- [daemon/src/state.rs](./daemon/src/state.rs:258)
- [daemon/src/state.rs](./daemon/src/state.rs:413)
- [docs/ref-device-coverage.md](./docs/ref-device-coverage.md:15)
- [app/Audio Firmware/RE-NOTES.md](./app/Audio%20Firmware/RE-NOTES.md:269)

Important implication:

- updating only the device token is not enough
- updating only the XML `<timestamp>` is not enough
- the token and the cached preset payload have to move together

### 4. Raw DSP writes do not auto-advance the token on the tested path

The repo notes are explicit that raw DSP writes on the physical 2x4 HD do not automatically change `0xFFC8`. Device Console explicitly writes a new active-preset token as part of its controller-side save/update flow.

References:

- [docs/NOTES-device-console-hid-logging.md](./docs/NOTES-device-console-hid-logging.md:131)
- [docs/ref-command-pacing.md](./docs/ref-command-pacing.md:103)
- [app/Audio Firmware/RE-NOTES.md](./app/Audio%20Firmware/RE-NOTES.md:280)

Important implication:

- a bridge that only forwards DSP writes is not enough for long-term cache coherence
- if our own API mutates preset content, we must also update the token if we want other cached controllers to detect drift correctly

### 5. Desktop already has the stronger token-gated model

The desktop daemon:

- refreshes modification tokens
- writes a fresh active-preset token after preset-content mutations
- persists per-device shadow state together with tokens
- filters cached preset shadow by live tokens on reconnect

References:

- [minidsp/src/client/shadow.rs](./minidsp/src/client/shadow.rs:417)
- [minidsp/src/client/shadow.rs](./minidsp/src/client/shadow.rs:457)
- [daemon/src/state.rs](./daemon/src/state.rs:87)
- [daemon/src/state.rs](./daemon/src/state.rs:120)
- [daemon/src/bridge.rs](./daemon/src/bridge.rs:66)

This is the best current model in the repo.

### 6. ESP32 currently has only part of that model

The ESP32 build does these things today:

- reads master status live, which updates the active preset
- mirrors successful raw bridge writes into in-memory PEQ / selector / scalar shadow
- reads PEQ and some weak-readback selector/scalar fields back from that shadow
- persists PEQ shadow to NVS at `/config` commit boundaries
- exposes a token read helper / endpoint

References:

- [app-esp32/src/device.rs](./app-esp32/src/device.rs:415)
- [app-esp32/src/device.rs](./app-esp32/src/device.rs:722)
- [app-esp32/src/device.rs](./app-esp32/src/device.rs:756)
- [app-esp32/src/device.rs](./app-esp32/src/device.rs:864)
- [app-esp32/src/device.rs](./app-esp32/src/device.rs:1814)
- [app-esp32/src/device.rs](./app-esp32/src/device.rs:1821)
- [app-esp32/src/device.rs](./app-esp32/src/device.rs:1321)
- [app-esp32/src/http_server.rs](./app-esp32/src/http_server.rs:872)

What ESP32 does not do today:

- persist per-preset selector/scalar/PEQ shadow with per-preset tokens
- filter restored shadow by live token before replaying it
- write a fresh token after its own preset-content mutations
- import or export official `setting*.xml` cache

## Problem Statement

There are really two separate sync problems:

### Problem A: stop ESP32 from restoring stale state

If Device Console changes the MiniDSP and ESP32 later reconnects or reboots, ESP32 must not replay stale cached shadow back onto the device.

This problem can be solved on ESP32 alone.

### Problem B: stop Device Console from demanding initialize/revert after ESP32-side changes

If ESP32 changes preset content and Device Console later reconnects, the app should see a cache state that matches the live token and live preset contents.

This cannot be solved on ESP32 alone because the official app's cache lives on the host filesystem, not on the ESP32.

For the current project scope, Problem A is the target. Problem B is optional future work.

## Behavioral Model

### Per-preset, not global

Modification tokens are per preset.

That means stale-state detection is also per preset:

- if preset 2's token changed, preset 2 is stale
- presets 0, 1, and 3 may still be fully trusted

The ESP32 should not drop the whole device into an "out of sync" state just because one preset no longer matches.

### What happens when a preset is stale

If a cached preset shadow does not match the live token for that preset, ESP32 should:

- refuse to restore that cached preset shadow to hardware
- stop treating its old shadow-backed values for that preset as trustworthy
- continue forwarding commands to the MiniDSP normally
- continue using live hardware readback where the hardware provides it
- surface shadow-backed fields for that preset as unknown or stale rather than replaying or inventing values

This is the key difference from the official app:

- Device Console wants a full authoritative local model for its UI, so a cache miss can force initialize or disconnect behavior
- ESP32 does not need a full authoritative UI cache to stay useful as a bridge
- therefore ESP32 can degrade read confidence without forcing a reset

## What Piggybacking on Device Console Writes Can and Cannot Do

### What piggybacking can do

Because Device Console writes pass through the ESP32 bridge, ESP32 can observe:

- raw parameter writes
- preset switches
- explicit token writes from Device Console

That means ESP32 can:

- update its own in-memory shadow
- persist per-preset shadow when Device Console has effectively committed a new state
- keep its own future restore logic coherent without needing reset/init

### What piggybacking cannot do by itself

ESP32 cannot directly update Device Console's local `setting*.xml` files on the user's computer.

Therefore:

- piggybacking alone can fix Problem A
- piggybacking alone cannot fully fix Problem B

## Recommended Architecture

## Phase 1: ESP32 token-gated preset shadow

This phase is the minimum required fix.

### Design

Replace the ESP32's PEQ-only NVS model with a per-preset shadow cache keyed by device serial and guarded by live modification tokens.

Persist, per preset:

- PEQ shadow
- selector shadow
- scalar shadow
- last-known 4-byte token

At runtime:

1. Read live master status to know the active preset.
2. Capture raw bridge writes into the active preset's in-memory shadow.
3. On preset switch, snapshot the previous preset shadow before changing active preset.
4. On successful preset-content mutation from our own structured API, bump the active token the same way desktop does, then persist the updated preset shadow plus token.
5. On successful raw Device Console token write observed through the bridge, treat that as a commit point and persist the current preset shadow with the new token.
6. On boot or reconnect, read live device tokens first.
7. Restore only preset shadows whose stored token exactly matches the live token for that slot.
8. Never replay a mismatched shadow to hardware.

### Why this works

- It stops stale restore.
- It preserves the state ESP32 actually observed.
- It does not claim stronger live hardware readback than we really have.
- It matches the freshness model already used by desktop.
- It degrades on a per-preset basis instead of treating the entire device as unusable.

### Required code areas

- `app-esp32/src/device.rs`
  - extend current shadow model from PEQ-only NVS to full `PresetShadowState`
  - add token refresh / save helpers analogous to desktop
  - classify active-preset commit boundaries
- `app-esp32/src/http_server.rs`
  - bump token after preset-content `/devices/0/config` mutations
- `app-esp32/src/main.rs`
  - load token-gated preset cache before publishing the device
- TCP / WS raw bridge path
  - persist after observed Device Console commit points

### Important rule

Do not restore shadow blindly at boot. Read live tokens first, then decide whether any cached preset is still valid.

## Phase 2: host-side official-cache sync helper

This phase is out of scope for the main firmware fix.

It is only required if the end-state is truly "desktop app and ESP32 can always stay in sync so neither side needs reset/init."

### Design

Add a small host-side helper that keeps the official app's `setting*.xml` cache aligned with the device token and the state observed by `minidsp-open`.

The helper can be separate from the ESP32 firmware. It can run on the same machine as Device Console.

Inputs:

- current device serial / product / DSP version
- per-preset shadow exported by `minidsp-open`
- live per-preset tokens

Outputs:

- rewritten or generated `setting*.xml` files under the official Device Console settings root

Relevant host cache path logic already exists in desktop:

- [daemon/src/state.rs](./daemon/src/state.rs:313)

### Why a host helper is necessary

ESP32 cannot write `~/Documents/miniDSP/MiniDSP Device Console/...` on the host computer.

Therefore full bidirectional no-reset behavior requires one of:

- a host helper process
- an extended desktop daemon path acting as the bridge instead of ESP32 direct

### Safety rule

The helper must update both:

- the XML payload values
- the XML `<timestamp>`

Blindly forcing only the timestamp to match the device token would suppress mismatch detection while leaving stale preset contents in the official cache. That would be worse than the current reset/init dialog because it hides drift instead of surfacing it.

### Good trigger points for the helper

- after ESP32-originated preset-content mutations
- after a raw Device Console session observed through the bridge reaches a token-write commit point
- on explicit user request to "export current bridge state into official cache"

### Suggested implementation shape

Expose a structured export from ESP32 such as:

- `GET /devices/0/tokens`
- `GET /devices/0/config`
- new debug/state export endpoint returning per-preset shadow plus tokens
- optional mutation/event stream so a host helper can react to committed token changes instead of polling

Then a host helper:

1. finds the correct Device Console cache directory by serial/product/dspversion
2. reads existing `setting*.xml`
3. merges or rewrites only the fields we can authoritatively populate
4. writes atomically via temp file + rename

## Phase 3: bridge commit journaling

This phase strengthens both Phase 1 and Phase 2.

### Design

Add an explicit commit journal for raw bridge traffic:

- active preset before mutation
- sequence of parameter writes
- observed token write
- time of commit

The token write becomes the natural boundary between:

- tentative in-memory mutations
- committed preset state safe to persist or export

### Why this matters

It avoids persisting half-applied states if:

- the app disconnects mid-edit
- a preset switch happens before the token save
- the bridge resets in the middle of a write burst

### Commit rule

Treat "raw parameter writes seen" as tentative. Treat "token write for the active preset observed" as committed.

For our own structured API, the commit boundary is successful mutation plus our own token bump.

## Effort Estimate

These estimates assume the goal is a production-quality implementation in this repo, including basic validation on real hardware and preserving current behavior for existing `/config` clients.

### Phase 1: ESP32 token-gated preset shadow

Effort level: medium

Expected implementation time:

- about 1 to 2 focused days for a first solid implementation
- about 2 to 3 days if we include cleanup, migration handling, and stronger validation

Why this is not huge:

- the ESP32 already has in-memory `preset_shadows` and active-preset tracking
- raw bridge writes already mirror into PEQ / selector / scalar shadow
- there is already a token read helper and endpoint
- the desktop implementation already shows the exact model we want to copy

What is still new work:

- replace the PEQ-only NVS blob with a serializable per-preset shadow blob
- persist and restore per-preset tokens alongside the shadow
- filter restored preset data by live tokens before applying it
- add an ESP32-side helper to bump the active-preset token after structured preset mutations
- detect raw token-write commit points from bridged Device Console traffic and persist at that boundary

Main risks:

- getting the NVS format right without regressing current PEQ restore
- correctly distinguishing tentative raw edits from committed token writes
- keeping flash writes bounded so we do not create unnecessary wear

Confidence:

- high confidence this is worth doing
- high confidence it materially reduces stale-state surprises

### Phase 2: host-side official-cache sync helper

Effort level: medium-large

Expected implementation time:

- about 2 to 4 focused days for a practical first version
- longer if we want broad product coverage and safer XML merge behavior

Why this is more expensive:

- the hard part is not writing files, it is writing the right files safely
- the helper must locate the correct Device Console cache folder for a device
- it must write both payload and `<timestamp>` coherently
- partial or incorrect XML coverage can poison the official cache more subtly than today's reset/init dialog

What drives scope:

- exposing an authoritative export from `minidsp-open`
- mapping that export onto `setting*.xml`
- atomic rewrite strategy
- validation against the real Device Console attach flow

Main risks:

- product-specific XML differences or missing fields
- mismatched assumptions about which cache directory Device Console will actually use
- writing a cache state that suppresses mismatch detection while still being incomplete

Confidence:

- medium confidence on effort
- medium confidence on correctness until exercised against a real host setup

### Phase 3: bridge commit journaling

Effort level: small-medium

Expected implementation time:

- about 0.5 to 1.5 days if kept internal
- more if exposed as a formal event stream or debug API

Why this is relatively contained:

- the raw bridge path already centralizes successful writes
- Phase 1 already needs a notion of tentative versus committed mutations
- journaling can start as an internal struct without changing the wire protocol

Main risks:

- avoiding duplicated commit logic across HTTP, TCP, and WS mutation paths
- keeping the model simple enough that it helps instead of becoming another cache layer

Confidence:

- high confidence this is a good support layer
- lower confidence that it should be built before Phase 1 unless a host helper is imminent

## Recommended Investment Order

If the goal is "make ESP32 safe and reliable when Device Console was used recently," the recommended order is:

1. Phase 1 first
2. optional lightweight commit journal pieces only where Phase 1 needs them
3. stop there unless "keep official Device Console cache in sync too" is a hard requirement

If the goal is the stronger promise "desktop app and ESP32 can swap back and forth without either side ever asking for reset/init," then the realistic order is:

1. Phase 1
2. Phase 3
3. Phase 2

## Bottom Line

The firmware-side fix is not a huge science project. It is a moderate extension of code that already exists in both the ESP32 path and the desktop path.

The core result is not "perfect sync with every controller." The core result is:

- the ESP32 keeps a local per-preset cache
- it knows when a preset has gone stale because the token no longer matches
- it refuses to trust or restore stale cached state
- it can still send commands and use live readback where available

The host-side cache-sync helper is the real line where this turns from "tight firmware improvement" into "cross-controller ecosystem work." That part is feasible, but it is a meaningfully larger and riskier project than the ESP32-side token-gated shadow fix.

## Ideas Considered

## Idea: firmware-only token gating

### Pros

- solves stale restore on ESP32
- no host software required
- small conceptual jump from existing code

### Cons

- does not update Device Console's local cache
- does not give full bidirectional no-reset behavior with the official app

### Verdict

Required main implementation for the current scope.

## Idea: update only the device token

### Pros

- simple

### Cons

- does not update Device Console's cached preset payload
- can cause the app to trust stale cache contents

### Verdict

Reject.

## Idea: update only the XML `<timestamp>`

### Pros

- simple host-side hack

### Cons

- same correctness problem as token-only syncing
- hides drift instead of fixing it

### Verdict

Reject.

## Idea: make ESP32 emulate stronger readback than the hardware exposes

### Pros

- would theoretically reduce dependence on cache

### Cons

- conflicts with the bridge contract
- repo evidence says there is no hidden authoritative path to mirror
- risks inventing fake state instead of preserving real state

### Verdict

Reject.

## Idea: desktop daemon as the only bridge for Device Console

### Pros

- desktop already has the stronger token-gated shadow model
- host filesystem access is naturally available

### Cons

- does not satisfy the direct-ESP32 deployment goal
- changes topology rather than fixing ESP32

### Verdict

Useful fallback architecture, not the primary ESP32 answer.

## Idea: explicit mutation/event stream

### Pros

- clean subscription point for a future host companion
- makes committed token changes first-class instead of implicit
- useful even if we keep the raw bridge protocol unchanged

### Cons

- does not help stock Device Console by itself
- still needs Phase 1 state modeling plus a host consumer for full value

### Verdict

Good enabling layer, not the standalone fix.

## Implementation Checklist

- [ ] Port desktop-style token refresh / save helpers into `app-esp32`
- [ ] Replace PEQ-only NVS persistence with per-preset `PresetShadowState + token`
- [ ] Restore shadow only after live-token match
- [ ] Snapshot previous preset on preset switch
- [ ] Detect raw bridge token writes and persist committed preset shadow on that boundary
- [ ] Classify ESP32 `/devices/0/config` preset-content mutations and bump active token after success
- [ ] Add a structured export endpoint for per-preset shadow plus tokens
- [ ] Build a host-side helper that can update official `setting*.xml` files atomically
- [ ] Validate that Device Console reconnects cleanly after:
  - Device Console-originated edits through ESP32
  - ESP32-originated edits through `/devices/0/config`
  - ESP32 reboot with unchanged tokens
  - ESP32 reboot after Device Console changed the device

## Open Questions

- Which exact Device Console cache directory selection rule is safest when multiple directories partially match serial/product?
- Should the host helper rewrite whole `setting*.xml` files or patch only recognized items/filters?
- Do we want to persist tentative raw-bridge writes before the token write arrives, or only commit-token-complete states?
- Is there any official-app behavior that requires writing "token to file" at a slightly different time than "token to device," or can we treat them as one atomic exported state?

## Recommended Order

1. Implement Phase 1 on ESP32.
2. Add bridge commit journaling.
3. Expose structured preset-shadow export.
4. Build the optional host helper for full bidirectional no-reset behavior.

That sequence gives immediate value:

- Step 1 prevents ESP32 from restoring stale state.
- Step 2 makes the captured state robust.
- Step 3 decouples firmware from host-cache policy.
- Step 4 is the final piece required for Device Console itself to stay happy after ESP32-side changes.
