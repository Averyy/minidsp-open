# Live Validation Harness

This file documents the repeatable validation workflow built around
[`tools/run-live-validation.py`](./tools/run-live-validation.py).

The goal is simple: after any material change, run one command for the
machine-checkable compatibility surface instead of trying to remember a long
manual curl / CLI / ezBEQ sequence.

## What It Covers

The harness exercises the automatable subset of `docs/todo-checker.md`:

- static repo checks
- desktop and ESP32 HTTP compatibility routes
- raw WebSocket and TCP bridge round-trips
- CLI compatibility through direct flags, `--url`, `--daemon-url`, and env vars
- `--force-kind` plus deprecated `--force-device` / `--force-dsp` acceptance
- source, preset, mute, gain, config, measurement-mode, `/sync`, and OpenAPI checks
- ESP32-only negative paths
- embedded ezBEQ REST + WebSocket routes
- hosted ezBEQ bundle and updater smoke
- optional spawned upstream ezBEQ process against our CLI / TCP bridge

It intentionally separates the risky paths:

- `--allow-audio` enables `/play` tests and playback-busy checks
- `--allow-destructive` enables preset-copy and overwrite-style mutations
- on desktop targets, `--allow-destructive` now captures a strict snapshot
  baseline first, restores it at the end of the run, and writes an artifact
  bundle under `.scratch/live-validation-runs/<timestamp>/`

## What Stays Manual

No script can honestly replace these checks:

- audible quality approval for sweep-only, marker-only, and combined playback
- unplug / replug and power-cycle timing behavior
- official MiniDSP Device Console interactive verification
- multi-device USB indexing with two or more physical units attached
- product-family-specific features that need hardware we do not currently have on the bench

Treat the harness as the default gate, then use the short manual checklist below
for the remaining human / hardware-only cases.

## Typical Commands

ESP32 safe smoke, no audible or destructive mutations:

```bash
./tools/run-live-validation.py \
  --esp32-url http://192.0.2.251:5380 \
  --continue-on-fail
```

ESP32 full compatibility run, including `/play` and destructive routes:

```bash
./tools/run-live-validation.py \
  --esp32-url http://192.0.2.251:5380 \
  --allow-audio \
  --allow-destructive \
  --continue-on-fail \
  --report .scratch/live-validation-esp32.md
```

Desktop + ESP32 combined run:

```bash
./tools/run-live-validation.py \
  --desktop-url http://127.0.0.1:5380 \
  --esp32-url http://192.0.2.251:5380 \
  --continue-on-fail \
  --report .scratch/live-validation-full.md
```

Desktop destructive run with automatic snapshot baseline / restore artifacts:

```bash
./tools/run-live-validation.py \
  --desktop-url http://127.0.0.1:5380 \
  --allow-destructive \
  --continue-on-fail
```

Full ESP32 run with a real upstream ezBEQ process in front of our bridge:

```bash
./tools/run-live-validation.py \
  --esp32-url http://192.0.2.251:5380 \
  --run-upstream-ezbeq \
  --continue-on-fail
```

If you already know the repo is built and only want live checks:

```bash
./tools/run-live-validation.py \
  --esp32-url http://192.0.2.251:5380 \
  --skip-static \
  --continue-on-fail
```

## Operator Notes

- Keep the MiniDSP attached and idle before starting.
- Keep the official Device Console closed during routine harness runs.
- Use low speaker volume before `--allow-audio`.
- On desktop, `--allow-destructive` now restores the full device from a strict
  snapshot at the end of the run, but it still performs real live mutations in
  between, so keep the unit idle until the run finishes.
- Use `--allow-destructive` only when overwriting inactive presets or ezBEQ slot state is acceptable.
- Prefer `--report` so the result can be attached to a commit, PR, or progress note.
- The desktop destructive artifact bundle currently includes
  `snapshot-before.json`, `snapshot-after.json`, `device-before.json`,
  `device-after.json`, `config-before.json`, `config-after.json`,
  `restore-result.json`, plus top-level `report.md` / `report.json`.

## Manual Checklist

- Confirm the harness passed for the intended target set.
- If playback changed, run the harness again with `--allow-audio`, listen to sweep-only, marker-only, and combined playback, and record explicit human approval.
- If reconnect behavior changed, do live unplug / replug and power-cycle checks on the ESP32 path after the harness completes.
- If TCP bridge or discovery code changed, do one focused official Device Console sanity pass.
- If device-matrix code changed, update `docs/todo-checker.md` and `PROGRESS.md` with what hardware was actually re-verified.
