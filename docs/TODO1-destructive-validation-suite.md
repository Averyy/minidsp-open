# Full Destructive Validation Suite Plan

## Status

The snapshot-backed desktop first pass is implemented and live-verified on a
2x4 HD: strict baseline capture, automatic restore, equality checks, durable
artifacts, compatibility transports, and playback checks. Completed work is in
[CHANGELOG.md](../CHANGELOG.md); the implementation is
[tools/run-live-validation.py](../tools/run-live-validation.py).

### Remaining scope

The orchestration below is implemented and covered by offline fake-device unit
tests. The new full sequence has **not** been run against hardware. Prior live
snapshot results above do not constitute sign-off for the new workflow.

- [x] Structured manual checkpoint recording for audio / reconnect / Device
  Console, with prompt/file answers, explicit CI skips and durable JSON.
- [x] Explicit `--destructive-profile full`, proposed snapshot/artifact/manual
  controls, confirmation gates and unconditional safety hard stops.
- [x] Integrated reset / reboot / bounded reconnect, identity checks and strict
  post-recovery snapshots, with baseline replay/equality after recovery.
- [x] Explicit separate direct-USB desktop lane requirements alongside the
  daemon-backed lane in [ref-live-validation.md](ref-live-validation.md).
- [x] Desktop concurrent master/config mutations must return `409` during
  observed snapshot export and restore busy windows. ESP32 queued requests must
  complete after the snapshot response. Unobserved overlap fails as unproven.
- [x] Shared HTTP snapshot guard for desktop and ESP32; strict baseline replay
  before suites and rollback/reporting on failures, even with `--skip-cli`.
- [x] Offline unit tests, `--help` and a full-profile `--plan` / `--dry-run` that
  performs no target access, builds, prompts or artifact writes.
- [ ] Run staged snapshot-only, limited destructive and full-profile validation
  on desktop-daemon and ESP32 2x4 HD hardware; investigate real busy-window
  timing and retain artifacts.
- [ ] Re-run the separate direct-USB round-trip with the daemon stopped.
- [ ] Record real acoustic approvals, physical recovery timing and Device
  Console observations using the new checkpoints.
- [ ] Broader ESP32 and multi-profile rollout, FIR/Dirac truthfulness checks and
  multiple-device USB indexing on actual hardware.

This plan is no longer blocked on shared snapshot import/export.
That prerequisite is already complete and documented in:

- [docs/ref-device-state-recovery.md](ref-device-state-recovery.md#L1)

## Goal

Extend the existing live validation harness into a full destructive regression
suite that can:

1. snapshot the device before testing
2. run the full `minidsp-open` + ezBEQ mutation surface
3. restore the original state automatically
4. produce a machine-readable and human-readable report

This suite should become the default gate for any material change that touches:

- DSP writes
- preset handling
- `/play`
- source switching
- reboot / reconnect
- ezBEQ integration
- TCP / WS bridge compatibility

## What Exists Today

The repo already has a good starting point:

- [`tools/run-live-validation.py`](../tools/run-live-validation.py#L1)
  covers the machine-checkable compatibility surface
- [`docs/ref-live-validation.md`](ref-live-validation.md#L1)
  documents the safe vs destructive split
- `--allow-audio` and `--allow-destructive` already gate the riskiest paths

That means this plan should extend the existing harness, not replace it.

Also note the current ceiling:

- the harness now proves full-device return-to-baseline for the verified
  desktop path through shared snapshot export/import
- the new checkpoint/full-profile/recovery orchestration is implemented but has
  only fake-device coverage; the broader hardware matrix remains open

## Why The Current Harness Is Not Enough

The current harness is no longer just a conservative smoke test, but it is not
yet the final shape described below:

- the explicit full profile is implemented, but needs staged live sign-off
- recorded approvals still require real human acoustic/physical/app observations
- direct USB remains a separate required hardware gate with exclusive ownership
- additional product-family mutation surfaces and live matrix coverage still
  need hardware

Those are the remaining gaps this plan should close.

## Dependency

This plan depends on the shared recovery reference surface in:

- [docs/ref-device-state-recovery.md](ref-device-state-recovery.md#L1)

The destructive suite should not invent its own backup format in Python.
It should call the shared snapshot export/import feature and treat that as the
single source of truth.

Important current state:

- the existing shared `apply_shadow_state()` helper is still not a hardware
  restore path
- that is no longer a blocker here, because the shared replay-capable snapshot
  import/export path now exists and is the recovery primitive this suite should
  use

## Design Principles

### 1. Extend, do not fork, `run-live-validation.py`

Keep one executable regression entry point.

Add destructive-mode phases and artifact handling to the existing harness
instead of introducing a second unrelated script.

### 2. Snapshot first or do not run

Any full destructive run must:

1. capture a strict pre-test snapshot
2. refuse to continue if that snapshot is incomplete

No "best effort" destructive mode should be the default.

Corollary:

- if the target profile is not supported for truthful strict snapshot/restore,
  the full destructive profile must refuse to run

### 3. Capability-gated tests

The suite should derive what to run from the target's actual profile and
verified compatibility surface.

Examples:

- do not run Dirac-specific checks on non-Dirac hardware
- do not run HT-only bass-management checks on 2x4 HD
- do not claim FIR restore coverage on a profile where strict snapshot support
  is not yet implemented
- do not run the full destructive profile on any FIR-capable device until FIR
  state is truthfully captured and replayable

### 4. Artifact-first execution

Every run should write a dated artifact directory with:

- pre-test snapshot
- post-test snapshot
- restore result
- markdown summary
- machine-readable JSON summary
- relevant HTTP / CLI / serial logs if requested

### 5. Manual gates remain explicit

Some checks cannot honestly be automated yet:

- audible sweep quality approval
- unplug / replug timing
- official Device Console interactive verification

Those should stay as explicit operator checkpoints, not be mislabeled as
"automated."

## Proposed Execution Model

## Phase 0: preflight

The harness should verify:

- target reachable
- device attached and idle
- no active playback
- no pending measurement-mode restore
- snapshot export supported for this target/profile
- strict restore support available for every destructive feature the selected
  profile intends to touch

If any of those fail, abort before making mutations.

## Phase 1: capture baseline

Collect:

- strict device snapshot via shared import/export feature
- baseline `GET /devices/{id}`
- baseline `GET /devices/{id}/config`
- baseline `/api/2/devices` for ESP32 targets
- baseline ezBEQ slot metadata
- optional boot / serial / debug log snapshot

Suggested artifact layout:

```text
.scratch/live-validation-runs/20260415T183000Z/
  snapshot-before.json
  device-before.json
  config-before.json
  ezbeq-before.json
  report.md
  report.json
```

## Phase 2: machine-checkable mutation suites

### 2a. Core compatibility

Keep the current safe checks:

- HTTP route existence and schema expectations
- CLI compatibility
- direct-USB desktop CLI snapshot round-trip for desktop targets, not only the
  daemon-backed path
- TCP bridge round-trips
- raw WebSocket bridge round-trips
- env-var and flag compatibility

### 2b. Structured control mutations

Run the current restore-capable checks and extend them:

- volume mutate + verify + restore
- mute mutate + verify + restore
- preset switch + verify + restore
- source switch matrix + verify + restore
- `/config` gain/mute/delay/invert/crossover/compressor/FIR/routing mutations
- `measurement-mode` enter / restore
- preset copy
- reset-to-defaults routes where supported

Important sequencing rule:

- now that full snapshot restore exists for the verified desktop path, these
  mutations should no longer depend on scattered hand-written per-feature
  restore logic
- targeted local restore is still useful inside a single step, but the
  authoritative end-of-run rollback should come from the shared snapshot import

### 2c. Playback contract

Retain `/play` in the same harness, but split it into three explicit passes:

1. sweep-only
2. marker-only
3. combined marker + sweep

The machine-checkable portion should verify:

- HTTP contract
- offsets / timing fields
- busy-guard responses during playback
- source auto-switch and restore
- oversize request rejection
- legacy field rejection
- snapshot import/export refuses or fails loudly when another command tries to
  mutate the device concurrently; no silent race is acceptable during the
  destructive restore window

The manual portion should still separately record human approval for:

- sweep-only audio quality
- marker-only audio quality
- combined audio quality

### 2d. ezBEQ contract

Keep and expand the current ezBEQ checks:

- embedded ezBEQ REST surface
- embedded ezBEQ WebSocket updates
- upstream ezBEQ process against our bridge
- load / clear / activate / overwrite semantics
- levels and catalogue metadata routes

### 2e. Reboot / reconnect

Automate what the current bench can support safely:

- target reboot route
- reconnect wait and post-recovery sanity checks
- restore of shadow-backed state after reboot

Physical unplug / replug should remain an operator-gated stage for now.

Also keep scope honest:

- reboot/reconnect automation should only run after the suite has captured a
  valid baseline snapshot
- if reconnect leaves the device in a state where snapshot restore can no
  longer proceed safely, the run must fail loudly and preserve artifacts

## Phase 3: operator checkpoints

The suite should support explicit manual checkpoints with artifact logging.

Implemented through `--manual-audio`, `--manual-reconnect`,
`--manual-device-console`, `--manual-answers` and `--non-interactive`.
Answers are written immediately to `manual-checkpoints.json` and included in
the reports. `--ci` / a truthy `CI` environment explicitly skips human checks.

### Audio approval

For `/play`, the harness should:

- run sweep-only, marker-only, and combined playback in a known order
- print a concise operator prompt
- record pass/fail notes in the report

Suggested prompt text:

- "Sweep-only approved?"
- "Marker-only approved?"
- "Combined marker + sweep approved?"

These answers should be written into the report, not left in terminal scrollback.

### Physical recovery approval

Optional operator stage:

- unplug / replug MiniDSP
- power-cycle MiniDSP
- confirm reconnect timing and post-recovery sanity checks

### Official app sanity pass

Optional operator stage when bridge/discovery code changed:

- confirm Device Console can discover/connect
- confirm basic control still works
- confirm no initialize/disconnect prompt regression

## Phase 4: restore

After all destructive phases:

1. import the pre-test snapshot
2. verify master state restored
3. verify per-preset fingerprints restored
4. verify per-preset tokens restored
5. verify ezBEQ-visible state matches baseline again
6. verify Dirac state restored on devices that support it

The suite should fail the overall run if restore verification fails, even if
all mutation tests passed.

## Phase 5: final diff and report

Capture:

- post-restore snapshot
- diff vs pre-test snapshot
- summary of passed / skipped / failed phases
- explicit list of manual checkpoints and their operator answers

The final report should answer two different questions clearly:

1. did the feature surface behave correctly during testing?
2. did the device return to its exact starting state?

## Proposed CLI / Harness Additions

Keep the current entry point:

```bash
uv run --script tools/run-live-validation.py ...
```

Add new flags instead of a second tool:

```bash
--snapshot-path
--restore-on-exit
--require-strict-snapshot
--destructive-profile full
--artifacts-dir .scratch/live-validation-runs
--manual-audio
--manual-reconnect
--manual-device-console
--manual-answers answers.json
--non-interactive
--ci
--yes-i-know
--reconnect-timeout 60
--plan
```

Reasonable behavior:

- `--destructive-profile full` implies `--allow-destructive`, `--allow-audio`,
  `--manual-audio`, `--require-strict-snapshot` and `--restore-on-exit`
- `--manual-audio` implies `--allow-audio`
- `--restore-on-exit` should run on both success and failure
- `--destructive-profile full` should error out immediately when strict snapshot
  support is not available for the chosen target

These controls are implemented; see [ref-live-validation.md](ref-live-validation.md)
for current semantics and artifact/answer schemas. `--snapshot-path` is an
additional output copy of a newly captured baseline, never an arbitrary input
restore image. Safety stops override `--continue-on-fail`. Audio/reset/reboot
require interactive `YES` or explicit `--yes-i-know`; answer files do not grant
that authorization. Every playback request sets and verifies -35 dB first and
announces before playback, with previous-volume restore after idle playback.

## Test Matrix

## Minimum full-run matrix

### Desktop daemon target

- HTTP
- raw WS
- TCP 5333
- CLI over `--daemon-url`
- CLI over `--url http://`
- CLI over `--url ws://`
- CLI over `--url tcp://`

### ESP32 target

- HTTP compatibility routes
- raw WS bridge
- TCP 5333 bridge
- embedded ezBEQ routes
- hosted bundle smoke
- reboot / reconnect checks
- `/play` busy guard and source-restore checks

### Upstream ezBEQ interoperability

- optional spawned upstream ezBEQ process
- load / clear through our CLI / bridge path

## Explicit Non-Goals For Automation

Do not label these as automated until they really are:

- acoustic judgment of pop-free playback
- official Device Console deep interactive testing
- multi-device USB indexing with two or more physical MiniDSPs attached
- product-family coverage for hardware not present on the bench

Those should remain checklist items recorded by the harness, not hidden
outside it.

Also avoid official-app naming collisions:

- do not call the harness artifact a "program snapshot"
- use names like `snapshot-before.json`, `snapshot-after.json`, and
  `restore-report.json`

## Safety Model

### Hard stops

Abort before destructive execution if:

- strict snapshot export fails
- target profile is unsupported for strict restore
- playback is already active
- reconnect / probe state is unstable
- the selected destructive phases would touch state the current snapshot model
  cannot truthfully restore

### Best-effort cleanup

On failure during the run:

- stop any active playback
- attempt snapshot restore
- attempt source restore
- write failure artifacts even if cleanup partially fails

### Trust model

The destructive suite should trust:

- shared snapshot export/import for device-state backup and restore
- current HTTP/CLI reads for live assertions

It should not trust:

- ad-hoc field-by-field "restore" logic on profiles where shared snapshot
  restore already exists
- current in-memory shadow alone as proof that hardware has been restored

## Validation Of The Validation Tool

The harness itself needs staged proof:

### Stage 1

- snapshot and restore without intermediate mutations
- verify idempotence and matching before/after fingerprints

### Stage 2

- run a limited destructive subset, then restore
- verify exact return to baseline

### Stage 3

- run the full suite against the live 2x4 HD / ESP32 bench
- verify the device remains usable afterward without manual repair

## Acceptance Criteria

- one command can run the machine-checkable destructive surface on the live
  bench and restore the original state automatically
- the suite emits durable artifacts for every run
- the suite fails if restore fails, even when the tested feature paths pass
- audio, reconnect, and Device Console checks are recorded as explicit manual
  gates instead of being omitted or hand-waved
- `docs/ref-live-validation.md` can be simplified to point to this stronger
  workflow once it is implemented
- the suite refuses to run a "full" destructive profile on hardware where the
  snapshot model is still incomplete

## Next Honest Steps

1. Run the documented offline test command:
   `uv run --offline --no-project python tools/test-live-validation.py`.
2. Review the full step list using `--destructive-profile full --plan` without a
   connected target.
3. On an intentionally prepared bench, run Stage 1, Stage 2 and Stage 3 on the
   existing 2x4 HD desktop/ESP32 paths, including busy-window timing and failure
   cleanup. No live target was accessed for this implementation.
4. Complete the separate direct-USB lane and record real manual checkpoints.
5. Roll out to additional device/DSP families only with their own strict
   snapshot/replay and FIR/Dirac/recovery evidence.

## Remaining Implementation Order

The remaining work is hardware validation and any failures it reveals, broader
product-specific control mutations/ezBEQ coverage, and multi-profile rollout.
The new full profile is available for staged validation, not yet a live-verified
replacement for the established bench gates. Targeted per-step restores remain;
shared snapshot import is the authoritative final rollback.
