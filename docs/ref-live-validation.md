# Live Validation Harness

[`tools/run-live-validation.py`](../tools/run-live-validation.py) is the shared
entry point for desktop-daemon and ESP32 compatibility and destructive checks.
Run Python through `uv`.

## Offline verification and planning

These commands do not contact a daemon, LAN target, USB device, or audio output:

```bash
uv run --offline --no-project python tools/test-live-validation.py
uv run --offline --no-project python tools/run-live-validation.py --help
uv run --offline --no-project python tools/run-live-validation.py \
  --desktop-url http://example.invalid --esp32-url http://esp32.invalid \
  --destructive-profile full --manual-reconnect --manual-device-console --plan
```

`--plan` (alias `--dry-run`) exits before target access, CLI execution, builds,
manual-answer reads, prompts, or artifact writes. It prints selected phases,
confirmations and hard stops. With no URL flags, it shows both lanes using
reserved `.invalid` example hosts. WebSocket dependencies are loaded only by live WS
checks; the offline commands need only Python's standard library.

The unit tests use an in-memory fake HTTP/device layer and prohibit real HTTP,
sockets and CLI execution. They cover manual answers, CI skips, confirmation,
strict-capture and restore failures, identity mismatches, audio volume safety,
maximum-volume ceilings, playback terminal failures, settings rollback, password
protection, firmware status, reconnect timeouts, both snapshot concurrency models,
failure artifacts, and the new full workflow. They do not establish hardware
correctness.

## Live lanes

Only run the following commands on an intentionally prepared bench. Keep the
unit idle, stop external control clients, and close Device Console except during
its explicit checkpoint. Confirm there is no pending measurement-mode restore;
the compatibility API does not expose a portable read-only check for that state.

The non-audio compatibility lane covers HTTP controls/config/measurement-mode,
WS/TCP bridges, CLI transports/flags/environment variables, embedded ezBEQ,
hosted bundle/updater smoke, and optional upstream ezBEQ. It still performs
control mutations and local restores; it is not a read-only smoke test.

Both lanes validate `GET /settings` and cache `maxVolumeDb` for volume and audio
checks. Master-volume and ezBEQ volume steps go down by 0.5 dB when the current
volume is at or within 0.5 dB of the ceiling. A starting volume above the
ceiling (set with the knob or IR remote) can't be restored through the API, so
those steps skip and audio validation refuses to start. With `--allow-destructive`, a
same-value settings POST is verified by GET and the original setting is restored
in `finally`, including on failure. Settings are re-read for that round trip.
With a ceiling at or below -35.5 dB and `--allow-audio`, an above-ceiling
rattle request must return `400` and leave playback idle with the previous
rattle run unchanged. A broken rejection would play that request, so the check
needs the audio confirmation and a ceiling that keeps any tone at or below the
-35 dB test level. If playback starts anyway, the suite sends rattle stop and
hard-stops. It skips otherwise, and never changes the ceiling to create coverage.

```bash
uv run --script tools/run-live-validation.py \
  --desktop-url http://127.0.0.1:5380 --restore-on-exit --continue-on-fail
```

For ESP32, replace `--desktop-url` with `--esp32-url http://BOARD:5380`.
Both flags can be supplied for two independently owned bench devices. Do not
point both lanes at the same physical MiniDSP or let a daemon and direct-USB CLI
own its HID interface concurrently.

The suite sends no credentials. Any HTTP `401` is a hard stop explaining that the
target is password-protected and the TCP bridge is off while protected. Clear
the ESP32 password before validation; open LAN control is the default.

### Desktop daemon lane

Required coverage is HTTP, raw and compatibility WebSockets, TCP 5333, CLI
`--daemon-url`, CLI `--url http://`, `--url ws://`, `--url tcp://`, and transport
environment variables. Snapshot guards use the shared HTTP routes and remain
active with `--skip-cli`. `--desktop-cli-transport` changes CLI compatibility
checks; it does not redirect the HTTP snapshot guard to a different device.

The daemon can reach the MiniDSP through an ESP32's TCP bridge (a
`[[static_device]] url = "tcp://BOARD:5333"` config, with the daemon bound to
`127.0.0.1`). The suite then skips the playback suite after the audio
confirmation, because desktop playback needs the MiniDSP's USB audio interface
on the computer; every other check runs. Don't run the ESP32 lane against the
same board at the same time.

### Desktop direct-USB lane (separate, required hardware gate)

The daemon-backed harness does not prove direct USB. Stop the daemon and close
Device Console, select one physical MiniDSP explicitly with `--device-index`,
and use the same built CLI for a strict direct-USB round-trip:

```bash
# Hardware only: daemon stopped, exactly the intended MiniDSP selected.
target/debug/minidsp --device-index 0 snapshot export /tmp/usb-before.json
target/debug/minidsp --device-index 0 snapshot import /tmp/usb-before.json
target/debug/minidsp --device-index 0 snapshot export /tmp/usb-after.json
```

Use fresh artifact paths. Compare device identity, master state, per-preset
payloads/tokens and FIR blocks; exclude capture/tool/notes metadata and
per-preset provenance, which records how the target knows a preset (an ESP32
re-derives it from the factory token after the MiniDSP reboots). Save both
snapshots with the run evidence, then restart the daemon for its lane. Never
substitute `--daemon-url` or a network transport for this direct-USB gate.
Multiple attached USB devices and indexing require a separate physical test.

### ESP32 lane

The same snapshot-backed workflow uses ESP32 HTTP snapshot export/import,
compatibility HTTP, WS, TCP 5333, embedded ezBEQ REST/WS, hosted bundle/updater,
playback and recovery routes. Read-only `GET /firmware` must include string
`version`, `build`, `runningSlot` and `slotState` fields, with a recognized slot
state. Firmware checks run only for ESP32 targets. Strict export and a verified
baseline replay must succeed before destructive mutations. Optional
`--run-upstream-ezbeq` adds the spawned upstream process checks; the full profile
does not install or spawn it implicitly.

## Full destructive profile

```bash
uv run --script tools/run-live-validation.py \
  --desktop-url http://127.0.0.1:5380 \
  --destructive-profile full \
  --manual-reconnect --manual-device-console \
  --artifacts-dir .scratch/live-validation-runs
```

`--destructive-profile full` implies `--allow-destructive`, `--allow-audio`,
`--manual-audio`, `--require-strict-snapshot` and `--restore-on-exit`.
Physical reconnect and Device Console remain optional explicit checkpoints.

The sequence is:

1. Verify idle playback, capture a strict complete snapshot and baseline HTTP
   state, and match snapshot identity against the selected device.
2. Import the baseline and re-export/compare it before any mutation suites.
   A complete export alone is insufficient proof that restore works.
3. Exercise snapshot GET/export and POST/restore concurrently with same-value
   master and config writes. On desktop, both mutations must return `409` inside
   an observed busy window. ESP32's single HTTP task queues requests behind the
   synchronous snapshot handler: launch the status probe and both mutations
   0.5 s after the snapshot starts, require successful responses, and require
   each to finish no earlier than the snapshot response headers arrived (with
   0.25 s for client scheduling), which body size and decoding can't skew.
   Each request must start
   before that response to establish overlap. An ESP32 export of trusted
   presets is answered from the board's cache in about 0.3 s, before the
   requests start; the GET case then skips with that timing, and the POST
   restore (about 66 s on a 2x4 HD) still has to prove serialization. Both lanes
   require a strictly complete snapshot equal to baseline. Unobserved overlap or
   boundary races fail as unproven; retain artifacts and investigate on hardware.
4. Run existing HTTP, transport, CLI and destructive preset-copy checks.
5. Confirm audio, save the previous listening volume, and set/read back **-35 dB**
   before every `/play` request. Announce before playing sweep-only, marker-only,
   combined marker/sweep, and ESP32 playback-busy checks. Record three separate
   audio approvals. A ceiling below -35 dB stops audio preflight with instructions
   to raise or clear it; the suite never raises the ceiling or changes the test
   level. After every started run, poll `GET /devices/{id}/play` until idle and
   require matching `last.startedAtUs`, `last.state == "completed"` and no `error`,
   `sourceRestoreError` or `volumeRestoreError`. Elapsed time alone does not prove
   completion. Wait until playback is idle before restoring and verifying the
   previous volume, including on failure. A stuck playback run stops cleanup
   before raising the listening volume.
6. Run ESP32 ezBEQ/hosted-bundle checks and optional upstream ezBEQ.
7. Restore the baseline, then confirm each active-preset reset, alternate-preset
   reset, reset-all (which reboots), and explicit MiniDSP reboot. After each,
   require two consecutive live status reads of the same identity within
   `--reconnect-timeout`, capture a strict snapshot, and restore/compare baseline.
   Explicit reboot additionally requires its recovered snapshot to match the
   baseline before restore. The reboot route reboots the MiniDSP, not the ESP32.
8. Record optional physical reconnect and Device Console checkpoints. A passed
   physical reconnect answer triggers bounded identity/status and baseline
   snapshot checks. Include observed reconnect duration in its operator note.
9. In `finally`, verify idle playback and identity, import baseline, re-export,
   compare snapshots and capture HTTP state. ESP32 ezBEQ slot metadata must also
   match. Emit reports on success and failure.

Hard stops override `--continue-on-fail`: failed/incomplete baseline capture,
unsupported strict snapshot, failed restore/equality, identity mismatch, unsafe
volume verification/restoration, failed settings restoration, HTTP `401`, failed
recovery, or unproven snapshot concurrency assertions. Identity mismatch prevents
snapshot import into the replacement device. Failed restore details are durable
in `restore-result.json`. Cleanup is attempted after ordinary failures and
interrupts; it cannot guarantee recovery
after process termination, power loss, or unavailable hardware.

Before audio, reset or reboot, the operator must type `YES`. For a deliberately
unattended run, `--yes-i-know` explicitly authorizes those actions. Neither manual
answer files nor `--continue-on-fail` provide this authorization. Full mode is
not a claim that every family supports these operations: strict snapshot
validation and baseline replay are eligibility gates, and hardware rollout is
still pending below.

## Flags and artifacts

- `--allow-destructive`: same-value settings round trip / preset-copy / ezBEQ
  overwrite, with strict snapshot capture and automatic rollback on either
  target. Missing CLI does not bypass the guard.
- `--allow-audio`: playback with confirmation, verified -35 dB and previous-volume
  restoration. This flag alone does not request a whole-device snapshot guard.
- `--restore-on-exit`: strict baseline and preflight replay, plus final rollback
  on success/failure, without enabling audio/reset/reboot.
- `--require-strict-snapshot`: also enables `--restore-on-exit`.
- `--snapshot-path PATH`: save an additional copy of the newly captured strict
  baseline; requires one target, enables rollback, and refuses an existing path.
  This is an output path, not an externally supplied restore image.
- `--artifacts-dir DIR`: alias for existing `--artifact-root`; each run creates a
  timestamped subdirectory with per-target artifacts and top-level reports.
- `--reconnect-timeout SECONDS`: stable reconnect / idle wait limit (default 60).
- `--manual-audio`: enables audio and three approval checkpoints.
- `--manual-reconnect`, `--manual-device-console`: explicit human checks; both
  enable snapshot rollback. Close Device Console before answering its prompt.
- `--non-interactive`: no prompts; missing manual answers become explicit skips.
- `--manual-answers FILE`: JSON answers instead of terminal input, also usable
  with `--non-interactive`. Answers attest to operator observations, not to an
  automated acoustic/physical/official-app test.
- `--ci`: skip all human checkpoints, including supplied answers. Also enabled
  by a truthy `CI` environment variable. It does not authorize audio or recovery;
  a live full CI run still requires `--yes-i-know` and deliberately attached hardware.

Example manual-answer file (keys use `desktop` or `esp32`):

```json
{
  "desktop.audio-sweep": {"status": "pass", "note": "Clean sweep"},
  "desktop.audio-marker": {"status": "fail", "note": "Audible click"},
  "desktop.audio-combined": {"status": "skip", "note": "Speaker disconnected"},
  "desktop.reconnect": {"status": "pass", "note": "Unplug/replug: 4 seconds; controls work"},
  "desktop.device-console": {"status": "pass", "note": "Discovery and basic controls work; app closed"}
}
```

Every answer requires `status` (`pass`, `fail`, `skip`) and a string `note`.
Invalid answers fail the checkpoint; a failed human answer fails the overall run.
Answers are written immediately to `manual-checkpoints.json` with target,
checkpoint, prompt, source and timestamp, and included in `report.json` and the
Markdown summary. Skips never count as human approval.

The bundle also includes per-target `snapshot-before.json`, `snapshot-after.json`,
`device-before.json`, `device-after.json`, `config-before.json`, `config-after.json`,
`restore-result.json`, snapshot concurrency/recovery evidence, and ESP32
`ezbeq-before.json` / `ezbeq-after.json`. Top-level `report.md` / `report.json`
record each phase. `--report PATH` additionally writes a Markdown report there.
`snapshot-busy-get.json` / `snapshot-busy-post.json` retain the concurrency model,
snapshot status/body, probe and mutation statuses, and timeout evidence. For
ESP32, they include monotonic request start/completion timestamps and the
snapshot's header-arrival and completion times for the serialization assertion.

## Hardware validation still required

The new orchestration has offline fake-device coverage only. Prior 2x4 HD
snapshot/restore bench results remain documented in
[device-state recovery](ref-device-state-recovery.md); they do not validate this
new full sequence. Before promoting it as a live gate:

- Run no-mutation snapshot/replay, limited destructive, then full profile on the
  2x4 HD desktop-daemon and ESP32 lanes; retain failure and busy-window evidence.
- Perform the separate direct-USB round-trip with exclusive ownership.
- Listen to all three passes, measure physical reconnect timing and complete the
  Device Console checkpoint; a CI skip cannot satisfy these gates.
- Repeat on additional device/DSP families, including FIR and Dirac completeness
  and reset/reboot behavior. Do not mark a profile verified from fake tests.
- Verify multiple USB-device indexing and richer product-specific mutation
  surfaces on actual hardware. These are outside this orchestration change.
