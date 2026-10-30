#!/usr/bin/env -S uv run --script
"""Offline safety/workflow tests: uv run --offline --no-project python tools/test-live-validation.py."""

import contextlib
import copy
import importlib.util
import io
import json
import os
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("live_validation", Path(__file__).with_name("run-live-validation.py"))
live = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = live
spec.loader.exec_module(live)


def snapshot():
    return {
        "formatVersion": 1, "capturedAt": "before", "tool": {"name": "fake"},
        "device": {"serial": 12345, "hwId": 10, "dspVersion": 100, "presetCount": 2},
        "transport": {"kind": "fake", "target": "offline"},
        "master": {"activePreset": 0, "source": "Usb", "volume": -18.0, "mute": False},
        "presets": [{"slot": slot, "token": f"token-{slot}", "complete": True,
                     "completeness": {"strict": True}, "shadow": {"scalar": {"1": slot}}, "firBlocks": []}
                    for slot in range(2)],
        "completeness": {"strict": True},
    }


class FakeDevice:
    """In-memory HTTP layer; never starts a server or opens a socket."""

    def __init__(self):
        self.state = snapshot()
        self.calls = []
        self.identity_serial = 12345
        self.reject_restore = False
        self.ignore_test_volume = False
        self.snapshot_status = 200
        self.playing = False
        self.hold_snapshot = False
        self.broken_guard = False
        self.serialized = False
        self.admit_snapshot_probe = False
        self.server_lock = threading.Lock()
        self.queue_lock = threading.Lock()
        self.queued_requests = 0
        self.busy = threading.Event()
        self.release = threading.Event()
        self.mutations = 0
        self.settings = {"maxVolumeDb": None}
        self.fail_settings_post = False
        self.password_protected = False
        self.playback_last = None
        self.playback_terminal_state = "completed"
        self.playback_errors = {}
        self.playback_polls = 1
        self.playback_polls_remaining = 0
        self.rattle_last = None
        self.broken_rattle_ceiling = False
        self.firmware = {"version": "0.1.0", "build": "fake", "runningSlot": "ota_0", "slotState": "valid"}
        self.config_post_status = 200

    def response(self, status, data):
        return live.HttpResponse(status, {"Content-Type": "application/json"}, json.dumps(data).encode())

    def request(self, method, url, *, body=None, **kwargs):
        path = live.urllib.parse.urlparse(url).path
        payload = json.loads(body) if body else None
        self.calls.append((method, path, payload))
        if self.password_protected:
            return self.response(401, {"error": "password required"})
        if self.serialized and self.busy.is_set():
            with self.queue_lock:
                self.queued_requests += 1
                if self.queued_requests == 3:
                    self.release.set()
        with self.server_lock if self.serialized else contextlib.nullcontext():
            return self.handle_request(method, path, payload)

    def clamp_volume(self, value):
        ceiling = self.settings["maxVolumeDb"]
        return value if ceiling is None else min(value, ceiling)

    def handle_request(self, method, path, payload):
        if path == "/devices":
            return self.response(200, [{"url": "/devices/0", "version": {"serial": self.identity_serial, "hw_id": 10, "dsp_version": 100}}])
        if path.endswith("/snapshot"):
            if self.hold_snapshot:
                self.busy.set()
                if not self.release.wait(2):
                    raise AssertionError("test did not release fake snapshot operation")
                self.busy.clear()
            if method == "POST":
                if self.reject_restore:
                    return self.response(500, {"error": "fake restore failed"})
                self.state = copy.deepcopy(payload)
                self.state["master"]["volume"] = self.clamp_volume(self.state["master"]["volume"])
            return self.response(self.snapshot_status, self.state)
        if self.busy.is_set():
            if method == "POST":
                self.mutations += 1
                if self.mutations == 2 or self.broken_guard:
                    self.release.set()
                return self.response(200 if self.broken_guard else 409, {"error": "device busy"})
            return self.response(200 if self.admit_snapshot_probe else 409, {"error": "device busy"})
        if path == "/settings":
            if method == "POST":
                self.settings = dict(payload)
                self.state["master"]["volume"] = self.clamp_volume(self.state["master"]["volume"])
                if self.fail_settings_post:
                    self.fail_settings_post = False
                    self.settings = {"maxVolumeDb": None}
                    return self.response(500, {"error": "fake settings persistence failed"})
            return self.response(200, self.settings)
        if path == "/firmware":
            return self.response(200, self.firmware)
        if path.endswith("/rattle/stop"):
            self.playing = False
            return self.response(200, {"stopped": True})
        if path.endswith("/rattle"):
            if method == "GET":
                return self.response(200, {"playing": self.playing, "last": self.rattle_last})
            ceiling = self.settings["maxVolumeDb"]
            if not self.broken_rattle_ceiling and ceiling is not None and any(v > ceiling for v in payload["volumesDb"]):
                return self.response(400, {"error": f"volumesDb exceeds maximum master volume of {ceiling} dB"})
            self.playing = True
            self.rattle_last = {"state": "playing"}
            return self.response(200, self.rattle_last)
        if path.endswith("/play"):
            if method == "GET":
                playing = self.playing or self.playback_polls_remaining > 0
                if self.playback_polls_remaining:
                    self.playback_polls_remaining -= 1
                elif self.playback_last is not None:
                    self.playback_last.update({"state": self.playback_terminal_state, **self.playback_errors})
                return self.response(200, {"playing": playing, "last": self.playback_last})
            if payload and "tailToneBurst" in payload:
                return self.response(400, {"error": "legacy field"})
            if self.state["master"]["volume"] != -35:
                raise AssertionError("fake audio attempted without verified -35 dB")
            result = {"startedAtUs": 10, "sweepOffsetSamples": 20, "toneBurstOffsetSamples": 0,
                      "totalSamples": 100, "sampleRate": 100}
            self.playback_last = {**result, "state": "playing"}
            self.playback_polls_remaining = self.playback_polls
            return self.response(200, result)
        if path == "/sync":
            return self.response(200, {"serverTimeUs": 1 if payload["clientTimeUs"] == 1 else 20})
        if path == "/devices/0/config":
            return self.response(self.config_post_status if method == "POST" else 200,
                                 {"inputs": [], "outputs": [{"index": 0, "gain": 0.0}]})
        if path == "/devices/0":
            if method == "POST" and not (self.ignore_test_volume and payload.get("volume") == -35):
                self.state["master"].update(payload)
                self.state["master"]["volume"] = self.clamp_volume(self.state["master"]["volume"])
            master = dict(self.state["master"])
            master["preset"] = master.pop("activePreset")
            return self.response(200, {"master": master})
        if path.endswith("reset") or path.endswith("reset-all"):
            self.state["presets"][0]["shadow"] = {"reset": True}
            return self.response(200, {"status": "reset"})
        if path.endswith("/reboot"):
            return self.response(200, {"status": "rebooted"})
        if path == "/api/2/devices":
            return self.response(200, {"master": {"slots": [{"id": "1", "last": "Empty"}]}})
        if path == "/api/3/devices/master" and method == "PATCH":
            self.state["master"]["volume"] = self.clamp_volume(payload["masterVolume"])
            return self.response(200, {})
        raise AssertionError(f"unexpected fake request: {method} {path}")


class LiveValidationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.device = FakeDevice()
        self.real_http_request = live.http_request
        self.stack = contextlib.ExitStack()
        self.addCleanup(self.stack.close)
        self.stack.enter_context(patch.dict(os.environ, {"CI": "false"}))
        self.stack.enter_context(patch.object(live.urllib.request, "urlopen", side_effect=AssertionError("real HTTP forbidden")))
        self.stack.enter_context(patch.object(live.socket, "socket", side_effect=AssertionError("real sockets forbidden")))
        self.stack.enter_context(patch.object(live, "run_command", side_effect=AssertionError("CLI/USB forbidden")))
        self.stack.enter_context(patch.object(live, "http_request", side_effect=self.device.request))
        self.stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
        self.stack.enter_context(contextlib.redirect_stderr(io.StringIO()))

    def harness(self, *extra, kind="desktop"):
        args = live.parse_args([f"--{kind}-url", "http://fake.invalid", "--skip-static", "--skip-cli",
                                "--artifact-root", str(self.root / "runs"), *extra])
        harness = live.Harness(args)
        harness.run_dir = self.root
        return harness, live.build_targets(args)[0]

    def only_steps(self, harness, *names):
        run_step = harness.run_step
        return patch.object(harness, "run_step", side_effect=lambda section, name, fn, **kwargs:
                            run_step(section, name, fn, **kwargs) if name in names else True)

    def test_full_profile_and_plan_do_not_touch_any_target_or_artifacts(self):
        harness, _ = self.harness("--destructive-profile", "full", "--plan")
        self.assertTrue(harness.args.allow_audio and harness.args.allow_destructive and harness.args.restore_on_exit)
        with patch.object(sys, "argv", ["harness", "--desktop-url", "http://fake.invalid", "--destructive-profile", "full", "--plan"]), \
                patch.object(live, "prepare_run_artifact_dir", side_effect=AssertionError("plan wrote artifacts")), \
                patch.object(live, "run_static_checks", side_effect=AssertionError("plan built code")), \
                patch.object(live, "http_request", side_effect=AssertionError("plan used HTTP")):
            self.assertEqual(live.main(), 0)
        plan = "\n".join(live.plan_lines(harness.args))
        for phase in ("409", "reboot", "reset-all", "-35 dB", "combined", "direct-USB", "hard stop"):
            self.assertIn(phase, plan)

    def test_help_exits_without_io(self):
        with self.assertRaises(SystemExit) as exc:
            live.parse_args(["--help"])
        self.assertEqual(exc.exception.code, 0)
        self.assertEqual(self.device.calls, [])

    def test_dry_run_without_target_lists_both_lanes(self):
        args = live.parse_args(["--destructive-profile", "full", "--dry-run", "--manual-answers", "/nonexistent/answers.json"])
        plan = "\n".join(live.plan_lines(args))
        self.assertIn("desktop:", plan)
        self.assertIn("esp32:", plan)
        self.assertEqual(self.device.calls, [])

    def test_unbounded_or_nonpositive_timeouts_are_rejected(self):
        for value in ("nan", "inf", "0", "-1"):
            with self.subTest(value=value), self.assertRaises(SystemExit):
                live.parse_args(["--desktop-url", "http://fake.invalid", f"--reconnect-timeout={value}"])

    def test_baseline_failure_is_a_hard_stop_even_with_continue(self):
        for invalid in ("http-error", "partial", "missing-preset"):
            with self.subTest(invalid=invalid):
                self.device = FakeDevice()
                if invalid == "http-error":
                    self.device.snapshot_status = 500
                elif invalid == "partial":
                    self.device.state["completeness"]["strict"] = False
                else:
                    self.device.state["presets"].pop()
                harness, target = self.harness("--allow-destructive", "--continue-on-fail")
                with patch.object(live, "http_request", side_effect=self.device.request), \
                        patch.object(live, "run_common_http_suite") as mutations:
                    with self.assertRaises(live.SafetyStop):
                        live.run_target_suite(harness, target, self.root)
                mutations.assert_not_called()
                self.assertFalse(any(m == "POST" for m, _, _ in self.device.calls))

    def test_restore_failure_stops_before_mutation_and_records_failure(self):
        harness, target = self.harness("--allow-destructive", "--continue-on-fail")
        self.device.reject_restore = True
        with patch.object(live, "run_common_http_suite") as mutations:
            with self.assertRaises(live.SafetyStop):
                live.run_target_suite(harness, target, self.root)
        mutations.assert_not_called()
        self.assertFalse(json.loads((self.root / "desktop/restore-result.json").read_text())["restored"])

    def test_identity_mismatch_prevents_restore_post(self):
        artifacts = live.create_snapshot_artifacts(self.root, self.harness()[1])
        live.json_dump(artifacts.snapshot_before, snapshot())
        self.device.identity_serial = 999
        with self.assertRaises(live.SafetyStop):
            live.restore_snapshot(self.harness()[1], artifacts.snapshot_before, artifacts.restore_result)
        self.assertFalse(any(m == "POST" for m, _, _ in self.device.calls))

    def test_post_restore_mismatch_marks_restore_artifact_failed(self):
        harness, target = self.harness("--allow-destructive", "--continue-on-fail")
        artifacts = live.start_destructive_snapshot_guard(harness, target, self.root)
        live.restore_snapshot(target, artifacts.snapshot_before, artifacts.restore_result)
        self.device.state["presets"][0]["token"] = "wrong-token"
        with self.assertRaisesRegex(live.StepFailed, "did not match baseline"):
            live.verify_snapshot_restore(target, artifacts)
        result = json.loads(artifacts.restore_result.read_text())
        self.assertFalse(result["restored"])
        self.assertFalse(result["equalWithoutMetadata"])

    def test_active_playback_and_existing_snapshot_copy_stop_before_writes(self):
        path = self.root / "saved.json"
        path.write_text("existing backup")
        harness, target = self.harness("--snapshot-path", str(path), "--continue-on-fail")
        with self.assertRaises(live.SafetyStop):
            live.start_destructive_snapshot_guard(harness, target, self.root)
        self.assertEqual(path.read_text(), "existing backup")
        self.device.playing = True
        harness, target = self.harness("--allow-destructive", "--continue-on-fail")
        with self.assertRaises(live.SafetyStop):
            live.start_destructive_snapshot_guard(harness, target, self.root)
        self.assertFalse(any(m == "POST" for m, _, _ in self.device.calls))

    def test_manual_answers_are_structured_and_fail_marks_run_failed(self):
        answers = self.root / "answers.json"
        live.json_dump(answers, {"desktop.audio-sweep": {"status": "pass", "note": "clean"},
                                "desktop.device-console": {"status": "fail", "note": "disconnect prompt"}})
        harness, target = self.harness("--non-interactive", "--manual-answers", str(answers), "--continue-on-fail")
        self.assertEqual(harness.checkpoint(target, "audio-sweep", "Sweep approved?"), "pass")
        self.assertEqual(harness.checkpoint(target, "device-console", "Console approved?"), "fail")
        self.assertEqual(harness.checkpoint(target, "reconnect", "Recovery approved?"), "skip")
        records = json.loads((self.root / "manual-checkpoints.json").read_text())
        self.assertEqual(records[0]["source"], "file")
        self.assertEqual(records[0]["note"], "clean")
        self.assertEqual(harness.failures, 1)
        self.assertEqual(harness.summary_payload()["manualCheckpoints"], records)

    def test_interactive_checkpoint_and_ci_skip(self):
        harness, target = self.harness()
        with patch.object(sys.stdin, "isatty", return_value=True), patch("builtins.input", side_effect=["pass", "heard clean audio"]):
            self.assertEqual(harness.checkpoint(target, "audio-sweep", "Approved?"), "pass")
        ci, target = self.harness("--ci", "--manual-answers", str(self.root / "nonexistent.json"))
        with patch("builtins.input", side_effect=AssertionError("CI prompted")):
            self.assertEqual(ci.checkpoint(target, "device-console", "Approved?"), "skip")
        self.assertEqual(ci.manual_checkpoints[0]["source"], "ci")

    def test_invalid_manual_answer_is_recorded_as_failed(self):
        harness, target = self.harness("--continue-on-fail")
        harness.manual_answers = {"desktop.audio-sweep": {"status": [], "note": "bad schema"}}
        self.assertEqual(harness.checkpoint(target, "audio-sweep", "Approved?"), "fail")
        self.assertEqual(harness.failures, 1)

    def test_audio_confirmation_required_and_volume_restored_after_failure(self):
        harness, target = self.harness("--allow-audio", "--non-interactive")
        with self.assertRaises(live.SafetyStop):
            live.run_playback_suite(harness, target)
        self.assertEqual(self.device.calls, [])
        harness.args.yes_i_know = True
        with self.assertRaisesRegex(RuntimeError, "fake audio failed"):
            with live.safe_test_volume(harness, target):
                self.assertEqual(self.device.state["master"]["volume"], -35)
                raise RuntimeError("fake audio failed")
        self.assertEqual(self.device.state["master"]["volume"], -18)

    def test_interactive_confirmation_requires_exact_yes(self):
        harness, target = self.harness()
        with patch.object(sys.stdin, "isatty", return_value=True), patch("builtins.input", return_value="yes"):
            with self.assertRaises(live.SafetyStop):
                harness.confirm(target, "reboot")
        with patch.object(sys.stdin, "isatty", return_value=True), patch("builtins.input", return_value="YES"):
            harness.confirm(target, "reboot")
        self.assertEqual(self.device.calls, [])

    def test_failed_test_volume_verification_prevents_audio(self):
        harness, target = self.harness("--allow-audio", "--yes-i-know")
        self.device.ignore_test_volume = True
        with self.assertRaises(live.StepFailed):
            live.run_playback_suite(harness, target)
        self.assertFalse(any(m == "POST" and p.endswith("/play") for m, p, _ in self.device.calls))
        self.assertEqual(self.device.state["master"]["volume"], -18)

    def test_ceiling_below_test_level_stops_before_volume_writes_or_audio(self):
        for kind in ("desktop", "esp32"):
            with self.subTest(kind=kind):
                self.device.calls.clear()
                self.device.settings = {"maxVolumeDb": -40.0}
                self.device.state["master"]["volume"] = -45.0
                harness, target = self.harness("--allow-audio", "--yes-i-know", kind=kind)
                with self.assertRaisesRegex(live.SafetyStop, r"ceiling is -40.0 dB.*raise or clear"):
                    live.run_playback_suite(harness, target)
                self.assertFalse(any(m == "POST" for m, _, _ in self.device.calls))
                self.assertEqual(self.device.state["master"]["volume"], -45)

    def test_audio_three_passes_and_previous_volume_restore(self):
        harness, target = self.harness("--manual-audio", "--yes-i-know", "--non-interactive")
        self.device.settings = {"maxVolumeDb": -35.0}
        self.device.state["master"]["volume"] = -40.0
        with patch.object(live.time, "sleep"):
            live.run_playback_suite(harness, target)
        self.assertEqual([r["checkpoint"] for r in harness.manual_checkpoints], ["audio-sweep", "audio-marker", "audio-combined"])
        self.assertEqual(self.device.state["master"]["volume"], -40)
        self.assertEqual(harness.failures, 0)
        self.assertEqual(self.device.playback_last["state"], "completed")
        self.assertGreaterEqual(sum(m == "GET" and p.endswith("/play") for m, p, _ in self.device.calls), 6)
        self.assertEqual(sum(m == "GET" and p == "/settings" for m, p, _ in self.device.calls), 1)

    def test_failed_playback_terminal_state_and_restore_errors_fail_audio_steps(self):
        for terminal, errors in (("failed", {"error": "stream failed"}),
                                 ("completed", {"error": "stream failed"}),
                                 ("completed", {"sourceRestoreError": "source failed"}),
                                 ("completed", {"volumeRestoreError": "volume failed"})):
            with self.subTest(terminal=terminal, errors=errors):
                self.device.playback_terminal_state = terminal
                self.device.playback_errors = errors
                harness, target = self.harness("--manual-audio", "--yes-i-know", "--non-interactive", "--continue-on-fail")
                with patch.object(live.time, "sleep"):
                    live.run_playback_suite(harness, target)
                failures = [r for r in harness.results if r.status == "FAIL"]
                self.assertEqual(len(failures), 3)
                self.assertTrue(all(next(iter(errors.values())) in r.detail for r in failures))
                self.assertEqual(harness.manual_checkpoints, [])
                self.assertEqual(self.device.state["master"]["volume"], -18)

    def test_playback_completion_requires_matching_successful_last_run(self):
        _, target = self.harness()
        for last in (None, {"startedAtUs": 10, "state": "playing"}, {"startedAtUs": 9, "state": "completed"}):
            with self.subTest(last=last):
                self.device.playback_last = last
                self.device.playback_terminal_state = "playing" if last and last["state"] == "playing" else "completed"
                with self.assertRaises(live.StepFailed):
                    live.wait_for_playback_completion(target, {"startedAtUs": 10}, timeout=1)

    def test_audio_identity_swap_and_stuck_playback_prevent_volume_restore(self):
        harness, target = self.harness("--allow-audio", "--yes-i-know")
        with self.assertRaisesRegex(live.SafetyStop, "identity mismatch"):
            with live.safe_test_volume(harness, target):
                self.device.identity_serial = 999
        self.assertEqual(self.device.state["master"]["volume"], -35)
        self.device.identity_serial = 12345
        self.device.calls.clear()
        with patch.object(live.time, "monotonic", side_effect=[0, 2]):
            with self.assertRaises(live.SafetyStop):
                live.wait_for_idle_playback(target, 1)
        self.assertFalse(any(m == "POST" for m, _, _ in self.device.calls))

    def test_volume_steps_respect_ceiling_and_reuse_settings_in_http_and_ezbeq(self):
        for kind in ("desktop", "esp32"):
            for ceiling, expected in ((None, -17.5), (-18.0, -18.5), (-17.5, -18.5), (-17.0, -17.5)):
                with self.subTest(kind=kind, ceiling=ceiling):
                    self.device.calls.clear()
                    self.device.settings = {"maxVolumeDb": ceiling}
                    harness, target = self.harness(kind=kind)
                    with self.only_steps(harness, "POST /devices/{id} volume change + restore",
                                         "PATCH /api/3/devices/master masterVolume"), \
                            patch.object(live, "find_catalogue_entry", return_value={"id": "fake"}):
                        live.run_common_http_suite(harness, target)
                        if kind == "esp32":
                            live.run_ezbeq_suite(harness, target)
                    volume_posts = [payload["volume"] for m, p, payload in self.device.calls if m == "POST" and p == "/devices/0"]
                    self.assertEqual(volume_posts, [expected, -18.0])
                    if kind == "esp32":
                        self.assertEqual([payload["masterVolume"] for m, _, payload in self.device.calls if m == "PATCH"],
                                         [expected, -18.0])
                    self.assertEqual(self.device.state["master"]["volume"], -18)
                    self.assertEqual(sum(m == "GET" and p == "/settings" for m, p, _ in self.device.calls), 1)

    def test_fake_ceiling_clamps_master_ezbeq_and_snapshot_volume_writes(self):
        _, target = self.harness()
        self.device.settings = {"maxVolumeDb": -40.0}
        for method, path, payload in (("POST", "/devices/0", {"volume": -35.0}),
                                       ("PATCH", "/api/3/devices/master", {"masterVolume": -35.0}),
                                       ("POST", "/devices/0/snapshot", snapshot())):
            with self.subTest(path=path):
                live.http_json(method, target.path(path), payload)
                self.assertEqual(self.device.state["master"]["volume"], -40)

    def test_settings_shape_requires_a_number_or_null_ceiling(self):
        for invalid in ({}, {"maxVolumeDb": "-18"}, {"maxVolumeDb": True}, {"maxVolumeDb": float("inf")}, [], None):
            with self.subTest(invalid=invalid):
                self.device.settings = invalid
                _, target = self.harness()
                with self.assertRaises(live.StepFailed):
                    live.read_settings(target)

    def test_settings_round_trip_restores_null_and_numeric_values_and_is_gated(self):
        for kind in ("desktop", "esp32"):
            for ceiling in (None, -18.0):
                with self.subTest(kind=kind, ceiling=ceiling):
                    self.device.calls.clear()
                    original = {"maxVolumeDb": ceiling}
                    self.device.settings = dict(original)
                    harness, target = self.harness("--allow-destructive", kind=kind)
                    live.run_settings_suite(harness, target)
                    self.assertEqual(self.device.settings, original)
                    self.assertEqual([v for m, p, v in self.device.calls if m == "POST" and p == "/settings"],
                                     [original, original])
                    self.assertEqual(harness.failures, 0)
        self.device.calls.clear()
        harness, target = self.harness()
        live.run_settings_suite(harness, target)
        self.assertFalse(any(m == "POST" and p == "/settings" for m, p, _ in self.device.calls))
        self.assertTrue(any(r.status == "SKIP" and "POST /settings" in r.name for r in harness.results))

    def test_settings_are_restored_even_when_post_applies_then_fails(self):
        self.device.settings = {"maxVolumeDb": -18.0}
        self.device.fail_settings_post = True
        harness, target = self.harness("--allow-destructive", "--continue-on-fail")
        live.run_settings_suite(harness, target)
        self.assertEqual(harness.failures, 1)
        self.assertEqual(self.device.settings, {"maxVolumeDb": -18.0})
        self.assertEqual(live.read_settings(target), self.device.settings)
        self.assertEqual(sum(m == "POST" and p == "/settings" for m, p, _ in self.device.calls), 2)

    def test_settings_restore_failure_is_a_hard_stop(self):
        harness, target = self.harness("--allow-destructive", "--continue-on-fail")
        settings_posts = 0

        def fail_restore(method, url, **kwargs):
            nonlocal settings_posts
            if method == "POST" and url.endswith("/settings"):
                settings_posts += 1
                if settings_posts == 2:
                    return self.device.response(500, {"error": "fake restore failed"})
            return self.device.request(method, url, **kwargs)

        with patch.object(live, "http_request", side_effect=fail_restore):
            with self.assertRaisesRegex(live.SafetyStop, "original settings could not be restored"):
                live.run_settings_suite(harness, target)

    def test_rattle_ceiling_rejection_runs_only_where_a_failure_stays_quiet(self):
        audio = ("--allow-audio", "--yes-i-know")
        cases = [
            (None, audio, False),
            (-40.0, (), False),  # no audio approval
            (-18.0, audio, False),  # a failed rejection would play above -35 dB
            (-35.0, audio, False),
            (-40.0, audio, True),
        ]
        for ceiling, extra, runs in cases:
            with self.subTest(ceiling=ceiling, extra=extra):
                self.device.calls.clear()
                self.device.settings = {"maxVolumeDb": ceiling}
                harness, target = self.harness(*extra)
                live.run_settings_suite(harness, target)
                posts = [(p, v) for m, p, v in self.device.calls if m == "POST" and p.endswith("/rattle")]
                if runs:
                    self.assertEqual(len(posts), 1)
                    self.assertEqual(posts[0][1]["volumesDb"], [ceiling + 0.5])
                else:
                    self.assertEqual(posts, [])
                    self.assertTrue(any(r.status == "SKIP" and "rattle" in r.name for r in harness.results))
                self.assertFalse(self.device.playing)
                self.assertIsNone(self.device.rattle_last)
                self.assertEqual(harness.failures, 0)

    def test_rattle_ceiling_rejection_stops_if_playback_starts(self):
        self.device.settings = {"maxVolumeDb": -40.0}
        self.device.broken_rattle_ceiling = True
        harness, target = self.harness("--continue-on-fail", "--allow-audio", "--yes-i-know")
        with self.assertRaisesRegex(live.SafetyStop, "changed playback state"):
            live.run_settings_suite(harness, target)
        self.assertIn(("POST", "/devices/0/rattle/stop", {}), self.device.calls)
        self.assertFalse(self.device.playing)

    def test_firmware_shape_on_esp32(self):
        harness, target = self.harness(kind="esp32")
        with self.only_steps(harness, "GET /firmware"):
            live.run_esp32_specific_suite(harness, target)
        self.assertEqual(harness.failures, 0)
        self.assertEqual(self.device.calls, [("GET", "/firmware", None)])
        for field in ("version", "build", "runningSlot", "slotState"):
            for invalid in (None, 1, ""):
                with self.subTest(field=field, invalid=invalid):
                    original = self.device.firmware[field]
                    self.device.firmware[field] = invalid
                    harness, target = self.harness("--continue-on-fail", kind="esp32")
                    with self.only_steps(harness, "GET /firmware"):
                        live.run_esp32_specific_suite(harness, target)
                    self.assertEqual(harness.failures, 1)
                    self.device.firmware[field] = original

    def test_firmware_check_runs_only_in_esp32_target_suite(self):
        for kind in ("desktop", "esp32"):
            with self.subTest(kind=kind), contextlib.ExitStack() as stack:
                self.device.calls.clear()
                harness, target = self.harness(kind=kind)
                for suite in ("run_common_http_suite", "run_transport_suite", "run_cli_suite", "run_playback_suite", "run_ezbeq_suite"):
                    stack.enter_context(patch.object(live, suite))
                stack.enter_context(self.only_steps(harness, "GET /firmware"))
                live.run_target_suite(harness, target, None)
                self.assertEqual(self.device.calls, [("GET", "/firmware", None)] if kind == "esp32" else [])

    def test_password_protection_is_a_clear_hard_stop_for_http_and_preflight(self):
        self.device.password_protected = True
        harness, target = self.harness("--allow-destructive", "--continue-on-fail")

        def unauthorized(req, **kwargs):
            response = self.device.request(req.method, req.full_url, body=req.data)
            raise live.urllib.error.HTTPError(req.full_url, response.status, "Unauthorized", response.headers, io.BytesIO(response.body))

        with patch.object(live, "http_request", side_effect=self.real_http_request), \
                patch.object(live.urllib.request, "urlopen", side_effect=unauthorized):
            for call in (lambda: live.http_request("GET", target.path("/firmware")),
                         lambda: live.http_json("POST", target.path("/settings"), {"maxVolumeDb": None}),
                         lambda: live.read_devices(target),
                         lambda: live.wait_for_http(target.path("/devices"), timeout=1),
                         lambda: live.wait_for_upstream_ezbeq_catalogue(target.base_url, timeout=1),
                         lambda: live.start_destructive_snapshot_guard(harness, target, self.root)):
                with self.subTest(call=call), self.assertRaisesRegex(
                        live.SafetyStop, r"password-protected.*doesn't send credentials.*TCP bridge is off.*clear the password"):
                    harness.run_step("auth", "protected target", call)

    def test_optional_ezbeq_slot_count_read_does_not_hide_a_password_stop(self):
        harness, target = self.harness("--continue-on-fail")
        with self.only_steps(harness, "POST /devices/{id}/preset/{preset}"), \
                patch.object(live, "read_ezbeq_devices", side_effect=live.SafetyStop("target is password-protected")):
            with self.assertRaisesRegex(live.SafetyStop, "password-protected"):
                live.run_common_http_suite(harness, target)
        self.assertFalse(any(m == "POST" for m, _, _ in self.device.calls))

    def test_manual_rejection_aborts_without_continue(self):
        harness, target = self.harness()
        harness.manual_answers = {"desktop.audio-sweep": {"status": "fail", "note": "stop"}}
        with self.assertRaises(SystemExit):
            harness.checkpoint(target, "audio-sweep", "Approved?")
        self.assertEqual(json.loads((self.root / "manual-checkpoints.json").read_text())[0]["status"], "fail")

    def test_esp32_uses_http_guard_without_cli_and_ci_skips_physical_prompt(self):
        harness, target = self.harness("--allow-destructive", "--manual-reconnect", "--ci", kind="esp32")
        artifacts = live.start_destructive_snapshot_guard(harness, target, self.root)
        live.finish_destructive_snapshot_guard(harness, target, artifacts)
        with patch("builtins.input", side_effect=AssertionError("CI prompted")):
            live.run_recovery_suite(harness, target, artifacts)
        self.assertTrue((self.root / "esp32/ezbeq-before.json").exists())
        self.assertTrue((self.root / "esp32/ezbeq-after.json").exists())
        self.assertEqual(harness.manual_checkpoints[0]["status"], "skip")

    def test_suite_failure_restores_baseline(self):
        harness, target = self.harness("--allow-destructive")

        def fail_after_write(*_):
            self.device.state["master"]["volume"] = -50
            self.device.state["presets"][0]["shadow"] = {"changed": True}
            raise SystemExit(1)

        with patch.object(live, "run_common_http_suite", side_effect=fail_after_write):
            with self.assertRaises(SystemExit):
                live.run_target_suite(harness, target, self.root)
        self.assertEqual(self.device.state, snapshot())
        self.assertTrue(json.loads((self.root / "desktop/restore-result.json").read_text())["equalWithoutMetadata"])

    def test_snapshot_busy_assertions_export_and_import_and_missing_guard(self):
        harness, target = self.harness("--allow-destructive")
        artifacts = live.start_destructive_snapshot_guard(harness, target, self.root)
        for method in ("GET", "POST"):
            self.device.release.clear()
            self.device.mutations = 0
            self.device.hold_snapshot = True
            self.assertIn("409", live.assert_snapshot_busy(harness, target, artifacts, method))
            self.assertEqual(self.device.mutations, 2)
        self.device.release.clear()
        self.device.mutations = 0
        self.device.broken_guard = True
        with self.assertRaisesRegex(live.StepFailed, "expected 409, got 200"):
            live.assert_snapshot_busy(harness, target, artifacts, "POST")

    def test_unobserved_busy_window_is_not_a_pass(self):
        harness, target = self.harness("--allow-destructive")
        artifacts = live.start_destructive_snapshot_guard(harness, target, self.root)
        with self.assertRaisesRegex(live.StepFailed, "busy window not observed"):
            live.assert_snapshot_busy(harness, target, artifacts, "GET")

    def test_snapshot_serialization_assertions_export_and_import_record_completion_order(self):
        harness, target = self.harness("--allow-destructive", kind="esp32")
        artifacts = live.start_destructive_snapshot_guard(harness, target, self.root)
        self.device.serialized = True
        self.device.hold_snapshot = True
        for method in ("GET", "POST"):
            with self.subTest(method=method):
                self.device.release.clear()
                self.device.queued_requests = 0
                self.assertIn("after snapshot response", live.assert_snapshot_busy(harness, target, artifacts, method))
                evidence = json.loads((artifacts.root / f"snapshot-busy-{method.lower()}.json").read_text())
                self.assertEqual(evidence["concurrencyModel"], "serialized")
                self.assertFalse(evidence["busyObserved"])
                self.assertFalse(evidence["timedOut"])
                self.assertEqual(evidence["snapshot"], snapshot())
                self.assertEqual(self.device.queued_requests, 3)
                self.assertEqual(len(evidence["mutations"]), 2)
                for request in [evidence["probe"], *evidence["mutations"]]:
                    self.assertEqual(request["status"], 200)
                    self.assertLess(request["startedAt"], evidence["snapshotCompletedAt"])
                    self.assertGreater(request["completedAt"], evidence["snapshotCompletedAt"])

    def test_snapshot_serialization_rejects_early_completion_errors_and_changed_snapshot(self):
        for failure in ("early", "error", "changed", "incomplete"):
            with self.subTest(failure=failure):
                self.device.hold_snapshot = False
                self.device.state = snapshot()
                harness, target = self.harness("--allow-destructive", kind="esp32")
                artifacts = live.start_destructive_snapshot_guard(harness, target, self.root)
                self.device.release.clear()
                self.device.queued_requests = self.device.mutations = 0
                self.device.hold_snapshot = True
                self.device.serialized = failure != "early"
                self.device.broken_guard = self.device.admit_snapshot_probe = failure == "early"
                self.device.config_post_status = 500 if failure == "error" else 200
                if failure == "changed":
                    self.device.state["master"]["volume"] = -19
                elif failure == "incomplete":
                    self.device.state["completeness"]["strict"] = False
                expected = {"early": "completed before snapshot response", "error": "serialized request failed",
                            "changed": "did not match baseline", "incomplete": "strict snapshot incomplete"}[failure]
                with self.assertRaisesRegex(live.StepFailed, expected):
                    live.assert_snapshot_busy(harness, target, artifacts, "GET")
                self.assertTrue((artifacts.root / "snapshot-busy-get.json").exists())

    def test_reconnect_identity_and_timeout(self):
        harness, target = self.harness()
        with patch.object(live.time, "sleep"):
            self.assertIn("two consecutive", live.wait_for_reconnect(target, snapshot(), 1))
        self.device.identity_serial = 0
        with self.assertRaisesRegex(live.SafetyStop, "identity mismatch"):
            live.wait_for_reconnect(target, snapshot(), 1)
        with patch.object(live, "verify_target_identity", side_effect=live.StepFailed("offline")), \
                patch.object(live.time, "monotonic", side_effect=[0, 0, 0, 0.1, 2]), patch.object(live.time, "sleep"):
            with self.assertRaisesRegex(live.SafetyStop, "timed out"):
                live.wait_for_reconnect(target, snapshot(), 1)

    def test_full_new_workflow_fake_device_and_failure_cleanup(self):
        harness, target = self.harness("--destructive-profile", "full", "--yes-i-know", "--non-interactive")
        real_busy = live.assert_snapshot_busy

        def busy(*args):
            self.device.hold_snapshot = True
            self.device.mutations = 0
            self.device.release.clear()
            try:
                return real_busy(*args)
            finally:
                self.device.hold_snapshot = False

        with contextlib.ExitStack() as stack:
            for suite in ("run_common_http_suite", "run_transport_suite", "run_cli_suite"):
                stack.enter_context(patch.object(live, suite))
            stack.enter_context(patch.object(live, "assert_snapshot_busy", side_effect=busy))
            stack.enter_context(patch.object(live, "wait_for_playback_completion"))
            stack.enter_context(patch.object(live.time, "sleep"))
            live.run_target_suite(harness, target, self.root)
        self.assertEqual(harness.failures, 0)
        self.assertEqual(self.device.state, snapshot())
        paths = [p for m, p, _ in self.device.calls if m == "POST"]
        for route in ("preset/reset", "preset/1/reset", "preset/reset-all", "reboot"):
            self.assertIn(f"/devices/0/{route}", paths)

    def test_failure_reports_are_written_and_later_targets_do_not_run(self):
        args = ["harness", "--desktop-url", "http://fake.invalid", "--esp32-url", "http://another.invalid",
                "--allow-destructive", "--continue-on-fail", "--skip-static", "--skip-cli", "--artifact-root", str(self.root / "runs")]
        self.device.reject_restore = True
        with patch.object(sys, "argv", args):
            self.assertEqual(live.main(), 1)
        reports = list((self.root / "runs").glob("*/report.json"))
        self.assertEqual(len(reports), 1)
        report = json.loads(reports[0].read_text())
        self.assertGreater(report["summary"]["failed"], 0)
        self.assertFalse(any(r["section"].startswith("esp32") for r in report["results"]))


if __name__ == "__main__":
    unittest.main()
