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
        self.busy = threading.Event()
        self.release = threading.Event()
        self.mutations = 0

    def response(self, status, data):
        return live.HttpResponse(status, {"Content-Type": "application/json"}, json.dumps(data).encode())

    def request(self, method, url, *, body=None, **kwargs):
        path = live.urllib.parse.urlparse(url).path
        payload = json.loads(body) if body else None
        self.calls.append((method, path, payload))
        if path == "/devices":
            return self.response(200, [{"version": {"serial": self.identity_serial, "hw_id": 10, "dsp_version": 100}}])
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
            return self.response(self.snapshot_status, self.state)
        if self.busy.is_set():
            if method == "POST":
                self.mutations += 1
                if self.mutations == 2 or self.broken_guard:
                    self.release.set()
                return self.response(200 if self.broken_guard else 409, {"error": "device busy"})
            return self.response(409, {"error": "device busy"})
        if path.endswith("/play"):
            if method == "GET":
                return self.response(200, {"playing": self.playing})
            if payload and "tailToneBurst" in payload:
                return self.response(400, {"error": "legacy field"})
            if self.state["master"]["volume"] != -35:
                raise AssertionError("fake audio attempted without verified -35 dB")
            return self.response(200, {"startedAtUs": 10, "sweepOffsetSamples": 20, "toneBurstOffsetSamples": 0,
                                       "totalSamples": 100, "sampleRate": 100})
        if path == "/sync":
            return self.response(200, {"serverTimeUs": 1 if payload["clientTimeUs"] == 1 else 20})
        if path == "/devices/0/config":
            return self.response(200, {"inputs": [], "outputs": [{"index": 0, "gain": 0.0}]})
        if path == "/devices/0":
            if method == "POST" and not (self.ignore_test_volume and payload.get("volume") == -35):
                self.state["master"].update(payload)
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
        raise AssertionError(f"unexpected fake request: {method} {path}")


class LiveValidationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.device = FakeDevice()
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

    def test_audio_three_passes_and_previous_volume_restore(self):
        harness, target = self.harness("--manual-audio", "--yes-i-know", "--non-interactive")
        with patch.object(live, "wait_for_playback_completion"):
            live.run_playback_suite(harness, target)
        self.assertEqual([r["checkpoint"] for r in harness.manual_checkpoints], ["audio-sweep", "audio-marker", "audio-combined"])
        self.assertEqual(self.device.state["master"]["volume"], -18)
        self.assertEqual(harness.failures, 0)

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
