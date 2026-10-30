#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "websockets>=15,<16",
# ]
# ///
"""Run repeatable live validation against minidsp-open targets.

This script is the executable companion to docs/todo-checker.md. It covers the
automatable subset of the compatibility and ezBEQ matrix so routine regressions
can be exercised from one command instead of ad-hoc manual curls.

The harness supports:
- static repo checks (cargo tests, CLI build, ESP32 build, catalogue build)
- live desktop-daemon checks over HTTP / WS / TCP / CLI
- live ESP32 checks over HTTP / WS / TCP / CLI
- embedded ezBEQ REST + WS checks
- optional real upstream ezBEQ process validation through our CLI/TCP bridge

It intentionally separates audio/destructive flows behind explicit flags:
- --allow-audio enables /play and playback-busy checks
- --allow-destructive enables preset-copy / overwrite / other stateful flows
- --destructive-profile full adds strict rollback, audio, reset/reboot and snapshot busy assertions
- --plan prints the selected workflow without target access, builds or artifacts
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import dataclasses
import gzip
import json
import math
import os
import shlex
import socket
import subprocess
import sys
import tempfile
import threading
import time
import traceback
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Iterable

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_CLI_BIN = ROOT / "target" / "debug" / "minidsp"
DEFAULT_DEVICE_ID = 0
DEFAULT_EZBEQ_SLOT = 2
DEFAULT_TCP_PORT = 5333
DEFAULT_TIMEOUT = 15.0
CATALOGUE_TIMEOUT = 45.0
CATALOGUE_CACHE: dict[str, list[dict[str, Any]]] = {}


class StepFailed(RuntimeError):
    """Raised when a validation step fails."""


class SafetyStop(StepFailed):
    """A failure that must stop the run even with --continue-on-fail."""


@dataclasses.dataclass(slots=True)
class StepResult:
    section: str
    name: str
    status: str
    duration_sec: float
    detail: str


@dataclasses.dataclass(slots=True)
class HttpResponse:
    status: int
    headers: dict[str, str]
    body: bytes

    @property
    def text(self) -> str:
        charset = "utf-8"
        content_type = self.headers.get("Content-Type", "")
        if "charset=" in content_type:
            charset = content_type.split("charset=", 1)[1].split(";", 1)[0].strip()
        return self.body.decode(charset, errors="replace")

    def json(self) -> Any:
        return json.loads(self.text)


@dataclasses.dataclass(slots=True)
class SnapshotArtifacts:
    root: Path
    snapshot_before: Path
    snapshot_after: Path
    device_before: Path
    config_before: Path
    device_after: Path
    config_after: Path
    restore_result: Path


@dataclasses.dataclass(slots=True)
class Target:
    label: str
    kind: str
    base_url: str
    device_id: int
    tcp_port: int
    cli_bin: Path | None
    cli_transport: str | None = None

    @property
    def parsed(self) -> urllib.parse.ParseResult:
        return urllib.parse.urlparse(self.base_url)

    @property
    def host(self) -> str:
        host = self.parsed.hostname
        if not host:
            raise StepFailed(f"{self.label}: unable to determine host from {self.base_url}")
        return host

    @property
    def port(self) -> int:
        if self.parsed.port is not None:
            return self.parsed.port
        if self.parsed.scheme == "https":
            return 443
        return 80

    @property
    def ws_base(self) -> str:
        scheme = "wss" if self.parsed.scheme == "https" else "ws"
        return urllib.parse.urlunparse(
            (
                scheme,
                self.parsed.netloc,
                "",
                "",
                "",
                "",
            )
        ).rstrip("/")

    def path(self, relative: str) -> str:
        if not relative.startswith("/"):
            relative = "/" + relative
        return self.base_url.rstrip("/") + relative

    def raw_ws_url(self) -> str:
        return f"{self.ws_base}/devices/{self.device_id}/ws"

    def compat_ws_url(self, query: str = "") -> str:
        suffix = f"/devices/{self.device_id}"
        if query:
            suffix += "?" + query
        return self.ws_base + suffix

    def ezbeq_ws_url(self) -> str:
        return self.ws_base + "/ws"

    def cli_args(self) -> list[str]:
        if self.cli_transport:
            return shlex.split(self.cli_transport)
        return ["--tcp", f"{self.host}:{self.tcp_port}"]


class Harness:
    def __init__(self, args: argparse.Namespace):
        self.args = args
        self.results: list[StepResult] = []
        self.failures = 0
        self.manual_checkpoints: list[dict[str, Any]] = []
        self.run_dir: Path | None = None
        self.baselines: dict[str, dict[str, Any]] = {}
        self.manual_answers = {}
        if args.manual_answers and not args.ci:
            self.manual_answers = json.loads(args.manual_answers.read_text())
            if not isinstance(self.manual_answers, dict):
                raise StepFailed("manual answers must be a JSON object keyed by target.checkpoint")

    def run_step(
        self,
        section: str,
        name: str,
        fn: Callable[[], str | None],
        *,
        abort_on_fail: bool = True,
        hard_stop: bool = False,
    ) -> bool:
        start = time.perf_counter()
        try:
            detail = fn() or "ok"
            status = "PASS"
            ok = True
        except StepFailed as exc:
            detail = str(exc)
            status = "FAIL"
            ok = False
            hard_stop = hard_stop or isinstance(exc, SafetyStop)
        except Exception as exc:  # pragma: no cover - defensive path
            detail = "".join(traceback.format_exception_only(type(exc), exc)).strip()
            status = "FAIL"
            ok = False
        duration = time.perf_counter() - start
        self.results.append(
            StepResult(section=section, name=name, status=status, duration_sec=duration, detail=detail)
        )
        if not ok:
            self.failures += 1
            if hard_stop:
                raise SafetyStop(detail)
            if abort_on_fail and not self.args.continue_on_fail:
                raise SystemExit(1)
        return ok

    def confirm(self, target: Target, action: str) -> None:
        if self.args.yes_i_know:
            return
        if self.args.non_interactive or self.args.ci or not sys.stdin.isatty():
            raise SafetyStop(f"{target.label}: {action} requires interactive confirmation or --yes-i-know")
        try:
            answer = input(f"{target.label}: confirm {action}; type YES to continue: ")
        except EOFError as exc:
            raise SafetyStop("confirmation input ended") from exc
        if answer.strip() != "YES":
            raise SafetyStop(f"{target.label}: {action} was not confirmed")

    def checkpoint(self, target: Target, name: str, prompt: str) -> str:
        key = f"{target.label}.{name}"
        source = "interactive"
        if self.args.ci:
            answer = {"status": "skip", "note": "CI run: operator checkpoint skipped"}
            source = "ci"
        elif key in self.manual_answers:
            answer = self.manual_answers[key]
            source = "file"
        elif self.args.non_interactive or not sys.stdin.isatty():
            answer = {"status": "skip", "note": "no recorded answer in non-interactive run"}
            source = "non-interactive"
        else:
            try:
                status = input(f"{target.label}: {prompt} [pass/fail/skip]: ").strip().lower()
                note = input("Note: ").strip()
                answer = {"status": status, "note": note}
            except EOFError:
                answer = {"status": "skip", "note": "operator input ended"}
        if (not isinstance(answer, dict) or not isinstance(answer.get("status"), str)
                or answer.get("status") not in {"pass", "fail", "skip"}
                or not isinstance(answer.get("note"), str)):
            answer = {"status": "fail", "note": f"invalid answer for {key}; expected status and string note"}
        record = {"target": target.label, "checkpoint": name, "prompt": prompt,
                  "status": answer["status"], "note": answer["note"],
                  "source": source, "recordedAt": datetime.now(timezone.utc).isoformat()}
        self.manual_checkpoints.append(record)
        assert self.run_dir is not None
        json_dump(self.run_dir / "manual-checkpoints.json", self.manual_checkpoints)
        section = f"{target.label}-manual"
        if answer["status"] == "skip":
            self.skip(section, name, answer["note"])
        else:
            self.run_step(section, name, lambda: assert_manual_answer(answer))
        return answer["status"]

    def skip(self, section: str, name: str, detail: str) -> None:
        self.results.append(
            StepResult(section=section, name=name, status="SKIP", duration_sec=0.0, detail=detail)
        )

    def summary_lines(self) -> list[str]:
        lines = []
        current_section = None
        for result in self.results:
            if result.section != current_section:
                current_section = result.section
                lines.append(f"[{current_section}]")
            lines.append(
                f"  {result.status:4} {result.name} "
                f"({result.duration_sec:.2f}s) - {result.detail}"
            )
        total = len(self.results)
        passed = sum(1 for r in self.results if r.status == "PASS")
        skipped = sum(1 for r in self.results if r.status == "SKIP")
        failed = sum(1 for r in self.results if r.status == "FAIL")
        lines.append(
            f"Summary: {passed} passed, {skipped} skipped, {failed} failed, {total} total"
        )
        return lines

    def write_report(self, path: Path) -> None:
        lines = ["# Live Validation Report", "", *self.summary_lines(), ""]
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("\n".join(lines))

    def summary_payload(self) -> dict[str, Any]:
        total = len(self.results)
        passed = sum(1 for r in self.results if r.status == "PASS")
        skipped = sum(1 for r in self.results if r.status == "SKIP")
        failed = sum(1 for r in self.results if r.status == "FAIL")
        return {
            "manualCheckpoints": self.manual_checkpoints,
            "results": [dataclasses.asdict(result) for result in self.results],
            "summary": {
                "passed": passed,
                "skipped": skipped,
                "failed": failed,
                "total": total,
            },
        }

    def write_json_report(self, path: Path) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(self.summary_payload(), indent=2) + "\n")


def assert_manual_answer(answer: dict[str, Any]) -> str:
    assert_true(answer["status"] == "pass", answer["note"] or "operator rejected checkpoint")
    return answer["note"] or "operator approved"


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--desktop-url", help="Desktop daemon base URL, e.g. http://127.0.0.1:5380")
    parser.add_argument("--esp32-url", help="ESP32 base URL, e.g. http://192.0.2.251:5380")
    parser.add_argument(
        "--device-id",
        type=int,
        default=DEFAULT_DEVICE_ID,
        help=f"Device index to exercise (default: {DEFAULT_DEVICE_ID})",
    )
    parser.add_argument(
        "--tcp-port",
        type=int,
        default=DEFAULT_TCP_PORT,
        help=f"TCP bridge port (default: {DEFAULT_TCP_PORT})",
    )
    parser.add_argument(
        "--cli-bin",
        type=Path,
        default=DEFAULT_CLI_BIN,
        help=f"Path to the minidsp CLI binary (default: {DEFAULT_CLI_BIN})",
    )
    parser.add_argument(
        "--desktop-cli-transport",
        help="Explicit CLI transport args for desktop checks, e.g. '--tcp 127.0.0.1:5333'",
    )
    parser.add_argument(
        "--esp32-cli-transport",
        help="Explicit CLI transport args for ESP32 checks, e.g. '--tcp 192.0.2.251:5333'",
    )
    parser.add_argument(
        "--ezbeq-title",
        default="10 Cloverfield Lane",
        help="Catalogue title to load during ezBEQ tests (default: %(default)s)",
    )
    parser.add_argument(
        "--ezbeq-slot",
        type=int,
        default=DEFAULT_EZBEQ_SLOT,
        help=f"1-indexed ezBEQ slot to use for load/clear tests (default: {DEFAULT_EZBEQ_SLOT})",
    )
    parser.add_argument(
        "--allow-audio",
        action="store_true",
        help="Enable /play and playback-busy checks (will emit audible audio).",
    )
    parser.add_argument(
        "--allow-destructive",
        action="store_true",
        help="Enable preset-copy / overwrite and similar stateful flows.",
    )
    parser.add_argument(
        "--skip-static",
        action="store_true",
        help="Skip static repo checks (cargo tests/builds and catalogue artifact build).",
    )
    parser.add_argument(
        "--skip-cli",
        action="store_true",
        help="Skip CLI compatibility checks even if --cli-bin exists.",
    )
    parser.add_argument(
        "--run-upstream-ezbeq",
        action="store_true",
        help="Spawn a real upstream ezBEQ instance against the selected target and verify it end to end.",
    )
    parser.add_argument(
        "--upstream-ezbeq-port",
        type=int,
        default=18080,
        help="Port to use for the optional spawned upstream ezBEQ instance (default: %(default)s).",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=DEFAULT_TIMEOUT,
        help=f"Default per-step timeout in seconds (default: {DEFAULT_TIMEOUT})",
    )
    parser.add_argument(
        "--report",
        type=Path,
        help="Optional path to write a markdown report.",
    )
    parser.add_argument(
        "--artifact-root", "--artifacts-dir",
        type=Path,
        default=ROOT / ".scratch" / "live-validation-runs",
        help="Root directory for destructive-run artifacts (default: %(default)s).",
    )
    parser.add_argument(
        "--continue-on-fail",
        action="store_true",
        help="Continue running later checks even after a failure.",
    )
    parser.add_argument("--destructive-profile", choices=["full"], help="Enable strict snapshot/restore, destructive mutations, audio, reset, reboot and busy assertions.")
    parser.add_argument("--snapshot-path", type=Path, help="Also save the captured baseline here (one target only; never imports an existing file).")
    parser.add_argument("--restore-on-exit", action="store_true", help="Capture a strict baseline and restore on success or failure.")
    parser.add_argument("--require-strict-snapshot", action="store_true", help="Require a strict snapshot and verified restore before any suites.")
    parser.add_argument("--manual-audio", action="store_true", help="Enable audio and record sweep, marker and combined approval.")
    parser.add_argument("--manual-reconnect", action="store_true", help="Record physical unplug/replug or power-cycle approval and verify recovery.")
    parser.add_argument("--manual-device-console", action="store_true", help="Record official Device Console discovery/control approval.")
    parser.add_argument("--manual-answers", type=Path, help="JSON answers keyed by target.checkpoint, each with status (pass/fail/skip) and note.")
    parser.add_argument("--non-interactive", action="store_true", help="Read manual answers from file; skip unanswered checkpoints and never prompt.")
    parser.add_argument("--ci", action="store_true", default=os.environ.get("CI", "").lower() not in {"", "0", "false"}, help="Skip all manual checkpoints, including file answers (also set by CI environment).")
    parser.add_argument("--yes-i-know", action="store_true", help="Explicitly confirm audio, reset and reboot steps without an interactive prompt.")
    parser.add_argument("--reconnect-timeout", type=float, default=60.0, help="Maximum seconds to wait for stable reconnect (default: %(default)s).")
    parser.add_argument("--plan", "--dry-run", dest="plan", action="store_true", help="Print selected phases and safety gates without I/O, builds or artifacts.")
    args = parser.parse_args(argv)
    if args.plan and not args.desktop_url and not args.esp32_url:
        args.desktop_url = "http://desktop.invalid"
        args.esp32_url = "http://esp32.invalid"
    if not args.desktop_url and not args.esp32_url:
        parser.error("provide at least one of --desktop-url or --esp32-url")
    if not all(math.isfinite(value) and value > 0 for value in (args.timeout, args.reconnect_timeout)):
        parser.error("timeouts must be positive and finite")
    if args.device_id < 0:
        parser.error("--device-id must be nonnegative")
    if args.snapshot_path and args.desktop_url and args.esp32_url:
        parser.error("--snapshot-path requires exactly one target")
    if args.destructive_profile == "full":
        args.allow_destructive = args.allow_audio = True
        args.manual_audio = True
        args.require_strict_snapshot = args.restore_on_exit = True
    if args.manual_audio:
        args.allow_audio = True
    if (args.allow_destructive or args.require_strict_snapshot or args.manual_reconnect
            or args.manual_device_console or args.snapshot_path):
        args.restore_on_exit = True
    return args


def run_command(
    argv: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    timeout: float | None = None,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    proc = subprocess.run(
        argv,
        cwd=cwd,
        env=env,
        text=True,
        capture_output=True,
        timeout=timeout,
        check=False,
    )
    if check and proc.returncode != 0:
        output = (proc.stdout + proc.stderr).strip()
        raise StepFailed(
            f"command failed ({proc.returncode}): {' '.join(shlex.quote(x) for x in argv)}\n{output}"
        )
    return proc


def run_command_until_timeout(
    argv: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    timeout: float = 7.0,
) -> subprocess.CompletedProcess[str]:
    proc = subprocess.Popen(
        argv,
        cwd=cwd,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        stdout, stderr = proc.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        proc.kill()
        stdout, stderr = proc.communicate()
    return subprocess.CompletedProcess(argv, proc.returncode or 0, stdout, stderr)


def assert_true(condition: bool, message: str) -> None:
    if not condition:
        raise StepFailed(message)


def approx_equal(actual: float, expected: float, tolerance: float = 0.05) -> None:
    if abs(actual - expected) > tolerance:
        raise StepFailed(f"expected {expected} +/- {tolerance}, got {actual}")


def decode_body(body: bytes, headers: dict[str, str]) -> bytes:
    if headers.get("Content-Encoding", "").lower() == "gzip":
        return gzip.decompress(body)
    return body


def http_request(
    method: str,
    url: str,
    *,
    body: bytes | None = None,
    headers: dict[str, str] | None = None,
    timeout: float = DEFAULT_TIMEOUT,
) -> HttpResponse:
    req = urllib.request.Request(url=url, data=body, method=method)
    for key, value in (headers or {}).items():
        req.add_header(key, value)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            raw = resp.read()
            resp_headers = {k: v for k, v in resp.headers.items()}
            return HttpResponse(
                status=resp.getcode(),
                headers=resp_headers,
                body=decode_body(raw, resp_headers),
            )
    except urllib.error.HTTPError as exc:
        raw = exc.read()
        resp_headers = {k: v for k, v in exc.headers.items()}
        return HttpResponse(
            status=exc.code,
            headers=resp_headers,
            body=decode_body(raw, resp_headers),
        )
    except urllib.error.URLError as exc:
        raise StepFailed(f"HTTP {method} {url} failed: {exc}") from exc


def http_json(
    method: str,
    url: str,
    payload: Any | None = None,
    *,
    timeout: float = DEFAULT_TIMEOUT,
) -> tuple[HttpResponse, Any]:
    body = None
    headers = {"Accept": "application/json"}
    if payload is not None:
        body = json.dumps(payload).encode("utf-8")
        headers["Content-Type"] = "application/json"
    resp = http_request(method, url, body=body, headers=headers, timeout=timeout)
    data = None
    if resp.body:
        with contextlib.suppress(json.JSONDecodeError):
            data = resp.json()
    return resp, data


async def ws_receive_one(url: str, *, send: str | bytes | None = None, timeout: float = DEFAULT_TIMEOUT) -> Any:
    import websockets

    async with websockets.connect(url, open_timeout=timeout, ping_interval=None, close_timeout=timeout) as ws:
        if send is not None:
            await ws.send(send)
        return await asyncio.wait_for(ws.recv(), timeout=timeout)


async def ws_receive_many(
    url: str,
    *,
    sends: Iterable[str | bytes] = (),
    timeout: float = DEFAULT_TIMEOUT,
    count: int = 2,
) -> list[Any]:
    import websockets

    async with websockets.connect(url, open_timeout=timeout, ping_interval=None, close_timeout=timeout) as ws:
        for item in sends:
            await ws.send(item)
        messages = []
        for _ in range(count):
            messages.append(await asyncio.wait_for(ws.recv(), timeout=timeout))
        return messages


def tcp_roundtrip(host: str, port: int, payload: bytes, timeout: float) -> bytes:
    with socket.create_connection((host, port), timeout=timeout) as sock:
        sock.settimeout(timeout)
        sock.sendall(payload)
        return sock.recv(4096)


def lower_source_name(name: str) -> str:
    return name.strip().lower()


def read_devices(target: Target) -> list[dict[str, Any]]:
    resp, data = http_json("GET", target.path("/devices"))
    assert_true(resp.status == 200 and isinstance(data, list), f"{target.label}: GET /devices did not return a JSON list")
    return data


def read_status(target: Target) -> dict[str, Any]:
    resp, data = http_json("GET", target.path(f"/devices/{target.device_id}"))
    assert_true(resp.status == 200 and isinstance(data, dict), f"{target.label}: GET /devices/{target.device_id} failed")
    return data


def read_config(target: Target) -> dict[str, Any]:
    resp, data = http_json("GET", target.path(f"/devices/{target.device_id}/config"))
    assert_true(resp.status == 200 and isinstance(data, dict), f"{target.label}: GET /devices/{target.device_id}/config failed")
    return data


def read_ezbeq_devices(target: Target) -> dict[str, Any]:
    resp, data = http_json("GET", target.path("/api/2/devices"))
    assert_true(resp.status == 200 and isinstance(data, dict), f"{target.label}: GET /api/2/devices failed")
    return data


def ezbeq_slots_from_document(data: dict[str, Any], *, context: str) -> list[dict[str, Any]]:
    master = data.get("master")
    if isinstance(master, dict) and isinstance(master.get("slots"), list):
        return master["slots"]
    slots = data.get("slots")
    if isinstance(slots, list):
        return slots
    raise StepFailed(f"{context}: missing slots array")


def pick_alternate_source(current: str) -> str | None:
    current_lower = lower_source_name(current)
    for candidate in [
        "usb",
        "toslink",
        "analog",
        "spdif",
        "aesebu",
        "bluetooth",
        "rca",
        "xlr",
        "lan",
    ]:
        if candidate != current_lower:
            return candidate
    return None


def choose_gain_delta(value: float) -> float:
    candidate = value + 0.5
    if candidate > 12.0:
        candidate = value - 0.5
    if abs(candidate - value) < 0.01:
        candidate = value + 1.0
    return round(candidate, 2)


def extract_input_peq_fingerprint(config: dict[str, Any]) -> list[Any]:
    inputs = config.get("inputs", [])
    fingerprint = []
    for row in inputs:
        for band in row.get("peq", []):
            coeff = band.get("coeff", {})
            fingerprint.append(
                (
                    row.get("index"),
                    band.get("index"),
                    bool(band.get("bypass")),
                    round(float(coeff.get("b0", 0.0)), 6),
                    round(float(coeff.get("b1", 0.0)), 6),
                    round(float(coeff.get("b2", 0.0)), 6),
                    round(float(coeff.get("a1", 0.0)), 6),
                    round(float(coeff.get("a2", 0.0)), 6),
                )
            )
    return fingerprint


def find_catalogue_entry(target: Target, title: str) -> dict[str, Any]:
    data = read_catalogue_entries(target)
    wanted = title.casefold()
    for entry in data:
        formatted = str(entry.get("formattedTitle") or entry.get("title") or "")
        if formatted.casefold() == wanted or str(entry.get("title") or "").casefold() == wanted:
            return entry
    for entry in data:
        formatted = str(entry.get("formattedTitle") or entry.get("title") or "")
        if wanted in formatted.casefold():
            return entry
    raise StepFailed(f"{target.label}: could not find ezBEQ entry matching {title!r}")


def read_catalogue_entries(target: Target) -> list[dict[str, Any]]:
    cached = CATALOGUE_CACHE.get(target.base_url)
    if cached is not None:
        return cached
    resp = http_request(
        "GET",
        target.path("/database.json"),
        headers={"Accept": "application/json"},
        timeout=CATALOGUE_TIMEOUT,
    )
    assert_true(resp.status == 200, f"{target.label}: GET /database.json returned {resp.status}")
    data = json.loads(resp.text)
    assert_true(isinstance(data, list), f"{target.label}: /database.json did not return a JSON list")
    CATALOGUE_CACHE[target.base_url] = data
    return data


def ensure_cli_available(cli_bin: Path) -> None:
    assert_true(cli_bin.exists(), f"CLI binary not found: {cli_bin}")
    assert_true(os.access(cli_bin, os.X_OK), f"CLI binary is not executable: {cli_bin}")


def wait_for_http(url: str, timeout: float) -> None:
    deadline = time.time() + timeout
    last_status = None
    while time.time() < deadline:
        try:
            resp = http_request("GET", url, timeout=2.0)
            if resp.status == 200:
                return
            last_status = resp.status
        except StepFailed:
            pass
        time.sleep(0.5)
    raise StepFailed(f"timed out waiting for {url} (last status: {last_status})")


def wait_for_upstream_ezbeq_catalogue(base_url: str, timeout: float) -> None:
    deadline = time.time() + timeout
    last_status = None
    last_detail = "catalogue not ready"
    while time.time() < deadline:
        try:
            resp = http_request("GET", base_url.rstrip("/") + "/api/1/authors", timeout=2.0)
            last_status = resp.status
            if resp.status == 200:
                data = json.loads(resp.text)
                if isinstance(data, list) and data:
                    return
                last_detail = "authors list empty"
            else:
                last_detail = f"authors returned HTTP {resp.status}"
        except (StepFailed, json.JSONDecodeError) as exc:
            last_detail = str(exc)
        time.sleep(0.5)
    raise StepFailed(
        f"timed out waiting for upstream ezBEQ catalogue readiness at {base_url} "
        f"(last status: {last_status}, detail: {last_detail})"
    )


def make_target(label: str, kind: str, base_url: str, args: argparse.Namespace, cli_transport: str | None) -> Target:
    return Target(
        label=label,
        kind=kind,
        base_url=base_url.rstrip("/"),
        device_id=args.device_id,
        tcp_port=args.tcp_port,
        cli_bin=None if args.skip_cli else args.cli_bin,
        cli_transport=cli_transport,
    )


def run_static_checks(harness: Harness, args: argparse.Namespace) -> None:
    section = "static"
    harness.run_step(
        section,
        "cargo +stable test -p minidsp-protocol",
        lambda: command_detail(["cargo", "+stable", "test", "-p", "minidsp-protocol"]),
    )
    harness.run_step(
        section,
        "cargo +stable test",
        lambda: command_detail(["cargo", "+stable", "test"]),
    )
    if args.cli_bin:
        harness.run_step(
            section,
            "cargo +stable build -p minidsp-app",
            lambda: command_detail(["cargo", "+stable", "build", "-p", "minidsp-app"]),
        )
    if args.esp32_url:
        harness.run_step(
            section,
            "tools/esp-cargo build",
            lambda: command_detail([str(ROOT / "tools/esp-cargo"), "build"]),
        )
        harness.run_step(section, "build catalogue bundle", lambda: build_catalogue_artifacts())


def command_detail(argv: list[str], *, cwd: Path | None = None) -> str:
    proc = run_command(argv, cwd=cwd, timeout=None)
    tail = (proc.stdout + proc.stderr).strip().splitlines()
    snippet = tail[-1] if tail else "ok"
    return snippet[:400]


def build_catalogue_artifacts() -> str:
    with tempfile.TemporaryDirectory(prefix="minidsp-live-validation-") as tmp:
        run_command(["uv", "run", "tools/build-catalogue.py", "--output-dir", tmp], cwd=ROOT, timeout=None)
        required = [
            "manifest.json",
            "version.txt",
            "catalogue.bin",
            "database.json.gz",
            "authors.json",
            "years.json",
            "audiotypes.json",
            "contenttypes.json",
            "languages.json",
            "ui/index.html",
            "ui/ezbeq-shim.js",
        ]
        for rel in required:
            assert_true((Path(tmp) / rel).exists(), f"missing catalogue artifact {rel}")
        return "bundle + metadata artifacts built"


def timestamp_slug() -> str:
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def prepare_run_artifact_dir(root: Path) -> Path:
    path = root / timestamp_slug()
    path.mkdir(parents=True, exist_ok=False)
    return path


def json_dump(path: Path, payload: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")


def create_snapshot_artifacts(run_dir: Path, target: Target) -> SnapshotArtifacts:
    root = run_dir / target.label
    root.mkdir(parents=True, exist_ok=True)
    return SnapshotArtifacts(
        root=root,
        snapshot_before=root / "snapshot-before.json",
        snapshot_after=root / "snapshot-after.json",
        device_before=root / "device-before.json",
        config_before=root / "config-before.json",
        device_after=root / "device-after.json",
        config_after=root / "config-after.json",
        restore_result=root / "restore-result.json",
    )


def normalize_snapshot(snapshot: dict[str, Any]) -> dict[str, Any]:
    normalized = json.loads(json.dumps(snapshot))
    normalized.pop("capturedAt", None)
    normalized.pop("tool", None)
    normalized.pop("notes", None)
    return normalized


def compare_snapshot_documents(expected: dict[str, Any], actual: dict[str, Any]) -> list[str]:
    mismatches: list[str] = []
    keys = sorted(set(expected) | set(actual))
    for key in keys:
        if expected.get(key) != actual.get(key):
            mismatches.append(key)
    return mismatches


def snapshot_identity(snapshot: dict[str, Any]) -> tuple[int, int, int]:
    device = snapshot.get("device", {})
    values = tuple(device.get(key) for key in ("serial", "hwId", "dspVersion"))
    if not all(type(value) is int for value in values) or values[0] <= 0:
        raise SafetyStop("snapshot has no usable device identity")
    return values


def verify_target_identity(target: Target, snapshot: dict[str, Any], *, timeout: float = DEFAULT_TIMEOUT) -> None:
    # /devices is ordered by device index; fail closed if the requested slot is absent.
    resp, devices = http_json("GET", target.path("/devices"), timeout=timeout)
    assert_true(resp.status == 200 and isinstance(devices, list), "cannot read device identity")
    assert_true(0 <= target.device_id < len(devices), "target device index is absent")
    version = devices[target.device_id].get("version", {})
    actual = tuple(version.get(key) for key in ("serial", "hw_id", "dsp_version"))
    if actual != snapshot_identity(snapshot):
        raise SafetyStop(f"{target.label}: device identity mismatch; refusing all further writes")


def validate_strict_snapshot(snapshot: Any) -> None:
    assert_true(isinstance(snapshot, dict) and snapshot.get("formatVersion") == 1, "unsupported snapshot format")
    assert_true(snapshot.get("completeness", {}).get("strict") is True, "strict snapshot incomplete or unsupported")
    snapshot_identity(snapshot)
    presets = snapshot.get("presets", [])
    count = snapshot["device"].get("presetCount", 0)
    assert_true(type(count) is int and count > 0 and len(presets) == count, "snapshot missing preset payloads")
    assert_true({p.get("slot") for p in presets} == set(range(count)), "snapshot preset slots incomplete")
    assert_true(all(p.get("complete") is True and p.get("completeness", {}).get("strict") is True
                    and isinstance(p.get("shadow"), dict) for p in presets), "snapshot contains unrestorable presets")


def export_snapshot(target: Target, path: Path) -> dict[str, Any]:
    resp, snapshot = http_json("GET", target.path(f"/devices/{target.device_id}/snapshot"), timeout=300.0)
    json_dump(path, snapshot)
    assert_true(resp.status == 200, f"strict snapshot export returned {resp.status}")
    validate_strict_snapshot(snapshot)
    return snapshot


def assert_snapshot_equal(expected: dict[str, Any], actual: dict[str, Any]) -> None:
    validate_strict_snapshot(actual)
    if snapshot_identity(expected) != snapshot_identity(actual):
        raise SafetyStop("snapshot device identity mismatch")
    mismatches = compare_snapshot_documents(normalize_snapshot(expected), normalize_snapshot(actual))
    assert_true(not mismatches, f"snapshot did not match baseline (top-level mismatches: {', '.join(mismatches[:8])})")


def start_destructive_snapshot_guard(
    harness: Harness,
    target: Target,
    run_dir: Path | None,
) -> SnapshotArtifacts | None:
    if not harness.args.restore_on_exit:
        return None
    section = f"{target.label}-snapshot"
    assert run_dir is not None
    artifacts = create_snapshot_artifacts(run_dir, target)

    def export_baseline_step() -> str:
        resp, playback = http_json("GET", target.path(f"/devices/{target.device_id}/play"))
        assert_true(resp.status == 200 and isinstance(playback, dict) and playback.get("playing") is False,
                    "cannot establish idle playback state")
        snapshot = export_snapshot(target, artifacts.snapshot_before)
        verify_target_identity(target, snapshot)
        if harness.args.snapshot_path:
            assert_true(not harness.args.snapshot_path.exists(), "--snapshot-path already exists; refusing to overwrite")
            json_dump(harness.args.snapshot_path, snapshot)
        return str(artifacts.snapshot_before)

    harness.run_step(section, "strict snapshot baseline export", export_baseline_step, hard_stop=True)

    def capture_baseline_http_step() -> str:
        json_dump(artifacts.device_before, read_status(target))
        json_dump(artifacts.config_before, read_config(target))
        if target.kind == "esp32":
            json_dump(artifacts.root / "ezbeq-before.json", read_ezbeq_devices(target))
        return str(artifacts.root)

    harness.run_step(section, "capture baseline HTTP state", capture_baseline_http_step, hard_stop=True)
    return artifacts


def finish_destructive_snapshot_guard(
    harness: Harness,
    target: Target,
    artifacts: SnapshotArtifacts | None,
) -> None:
    if artifacts is None:
        return
    section = f"{target.label}-snapshot"

    harness.run_step(
        section,
        "snapshot restore",
        lambda: restore_snapshot(target, artifacts.snapshot_before, artifacts.restore_result),
        abort_on_fail=False,
        hard_stop=True,
    )

    harness.run_step(
        section,
        "post-restore snapshot verify",
        lambda: verify_snapshot_restore(target, artifacts),
        abort_on_fail=False,
        hard_stop=True,
    )
    harness.run_step(
        section,
        "capture post-restore HTTP state",
        lambda: capture_post_restore_http_state(target, artifacts),
        abort_on_fail=False,
        hard_stop=True,
    )


def restore_snapshot(target: Target, snapshot_path: Path, restore_result_path: Path) -> str:
    try:
        snapshot = json.loads(snapshot_path.read_text())
        validate_strict_snapshot(snapshot)
        verify_target_identity(target, snapshot)
        resp, verified = http_json("POST", target.path(f"/devices/{target.device_id}/snapshot"), snapshot, timeout=300.0)
        assert_true(resp.status == 200, f"snapshot restore returned {resp.status}")
        assert_snapshot_equal(snapshot, verified)
    except Exception as exc:
        json_dump(restore_result_path, {"restored": False, "snapshot": str(snapshot_path), "detail": str(exc)})
        raise
    json_dump(restore_result_path, {"restored": True, "snapshot": str(snapshot_path), "equalWithoutMetadata": True})
    return "restored snapshot and verified baseline equality"


def verify_snapshot_restore(target: Target, artifacts: SnapshotArtifacts) -> str:
    restore_data = json.loads(artifacts.restore_result.read_text())
    try:
        before = json.loads(artifacts.snapshot_before.read_text())
        verify_target_identity(target, before)
        after = export_snapshot(target, artifacts.snapshot_after)
        assert_snapshot_equal(before, after)
    except Exception as exc:
        restore_data.update({"restored": False, "equalWithoutMetadata": False, "detail": str(exc)})
        json_dump(artifacts.restore_result, restore_data)
        raise
    restore_data["equalWithoutMetadata"] = True
    restore_data["snapshotAfter"] = str(artifacts.snapshot_after)
    json_dump(artifacts.restore_result, restore_data)
    return "snapshot-after matched baseline"


def capture_post_restore_http_state(target: Target, artifacts: SnapshotArtifacts) -> str:
    json_dump(artifacts.device_after, read_status(target))
    json_dump(artifacts.config_after, read_config(target))
    if target.kind == "esp32":
        ezbeq = read_ezbeq_devices(target)
        json_dump(artifacts.root / "ezbeq-after.json", ezbeq)
        # Levels are transient; slot metadata is the state the operator sees.
        before = json.loads((artifacts.root / "ezbeq-before.json").read_text())
        assert_true(ezbeq_slots_from_document(before, context="baseline") == ezbeq_slots_from_document(ezbeq, context="restored"),
                    "ezBEQ slot metadata did not return to baseline")
    return str(artifacts.root)


def wait_for_playback_completion(play_result: dict[str, Any], *, padding_sec: float = 0.5) -> None:
    total_samples = play_result.get("totalSamples")
    sample_rate = play_result.get("sampleRate")
    if not isinstance(total_samples, int) or not isinstance(sample_rate, int) or sample_rate <= 0:
        return
    time.sleep(total_samples / sample_rate + padding_sec)


def run_common_http_suite(harness: Harness, target: Target) -> None:
    section = f"{target.label}-http"

    def devices_step() -> str:
        devices = read_devices(target)
        assert_true(len(devices) >= 1, f"{target.label}: no devices returned")
        first = devices[0]
        assert_true("url" in first and "version" in first, f"{target.label}: device list missing url/version")
        return f"{len(devices)} device(s), first url={first['url']}"

    harness.run_step(section, "GET /devices", devices_step)

    def status_step() -> str:
        status = read_status(target)
        master = status.get("master", {})
        assert_true("volume" in master and "source" in master and "preset" in master, f"{target.label}: master status missing fields")
        return f"preset={master['preset']} source={master['source']} volume={master['volume']}"

    harness.run_step(section, "GET /devices/{id}", status_step)

    def config_step() -> str:
        config = read_config(target)
        assert_true("master_status" in config and "inputs" in config and "outputs" in config, f"{target.label}: /config missing top-level fields")
        return f"inputs={len(config['inputs'])} outputs={len(config['outputs'])}"

    harness.run_step(section, "GET /devices/{id}/config", config_step)

    def openapi_step() -> str:
        resp = http_request("GET", target.path("/openapi.json"))
        assert_true(resp.status == 200, f"{target.label}: GET /openapi.json returned {resp.status}")
        spec = json.loads(resp.text)
        assert_true("/devices/{id}/play" in spec.get("paths", {}), f"{target.label}: /openapi.json missing /devices/{{id}}/play")
        return f"{len(spec.get('paths', {}))} paths"

    harness.run_step(section, "GET /openapi.json", openapi_step)

    baseline_status = read_status(target)
    baseline_master = baseline_status["master"]

    def volume_step() -> str:
        original = float(baseline_master["volume"])
        desired = choose_gain_delta(original)
        resp, data = http_json("POST", target.path(f"/devices/{target.device_id}"), {"volume": desired})
        assert_true(resp.status == 200, f"{target.label}: volume POST returned {resp.status}")
        current = read_status(target)["master"]["volume"]
        approx_equal(float(current), desired, tolerance=0.1)
        http_json("POST", target.path(f"/devices/{target.device_id}"), {"volume": original})
        restored = read_status(target)["master"]["volume"]
        approx_equal(float(restored), original, tolerance=0.1)
        return f"{original} -> {desired} -> {original}"

    harness.run_step(section, "POST /devices/{id} volume change + restore", volume_step)

    def mute_step() -> str:
        original = bool(baseline_master["mute"])
        desired = not original
        resp, _ = http_json("POST", target.path(f"/devices/{target.device_id}"), {"mute": desired})
        assert_true(resp.status == 200, f"{target.label}: mute POST returned {resp.status}")
        current = bool(read_status(target)["master"]["mute"])
        assert_true(current == desired, f"{target.label}: mute did not change to {desired}")
        http_json("POST", target.path(f"/devices/{target.device_id}"), {"mute": original})
        restored = bool(read_status(target)["master"]["mute"])
        assert_true(restored == original, f"{target.label}: mute did not restore")
        return f"{original} -> {desired} -> {original}"

    harness.run_step(section, "POST /devices/{id} mute change + restore", mute_step)

    def toggle_mute_step() -> str:
        before = bool(read_status(target)["master"]["mute"])
        resp, _ = http_json("POST", target.path(f"/devices/{target.device_id}/volume/toggle_mute"))
        assert_true(resp.status == 200, f"{target.label}: toggle_mute returned {resp.status}")
        mid = bool(read_status(target)["master"]["mute"])
        assert_true(mid != before, f"{target.label}: toggle_mute did not change mute")
        resp, _ = http_json("POST", target.path(f"/devices/{target.device_id}/volume/toggle_mute"))
        assert_true(resp.status == 200, f"{target.label}: second toggle_mute returned {resp.status}")
        after = bool(read_status(target)["master"]["mute"])
        assert_true(after == before, f"{target.label}: second toggle_mute did not restore mute")
        return f"{before} -> {mid} -> {after}"

    harness.run_step(section, "POST /devices/{id}/volume/toggle_mute", toggle_mute_step)

    def preset_step() -> str:
        original = int(read_status(target)["master"]["preset"])
        slot_count = 4
        with contextlib.suppress(Exception):
            slot_count = len(read_ezbeq_devices(target)["master"]["slots"]) or slot_count
        if slot_count <= 1:
            raise StepFailed(f"{target.label}: only {slot_count} preset available")
        desired = (original + 1) % slot_count
        resp, _ = http_json("POST", target.path(f"/devices/{target.device_id}/preset/{desired}"))
        assert_true(resp.status == 200, f"{target.label}: preset POST returned {resp.status}")
        current = int(read_status(target)["master"]["preset"])
        assert_true(current == desired, f"{target.label}: preset did not change to {desired}")
        http_json("POST", target.path(f"/devices/{target.device_id}/preset/{original}"))
        restored = int(read_status(target)["master"]["preset"])
        assert_true(restored == original, f"{target.label}: preset did not restore")
        return f"{original} -> {desired} -> {original}"

    harness.run_step(section, "POST /devices/{id}/preset/{preset}", preset_step)

    def source_step() -> str:
        original = str(read_status(target)["master"]["source"])
        desired = pick_alternate_source(original)
        assert_true(desired is not None, f"{target.label}: no alternate source candidate")
        last_error = None
        success_source = None
        for candidate in [desired, "usb", "toslink", "analog", "spdif"]:
            if lower_source_name(candidate) == lower_source_name(original):
                continue
            resp, _ = http_json("POST", target.path(f"/devices/{target.device_id}/source/{candidate}"))
            if resp.status == 200:
                current = str(read_status(target)["master"]["source"])
                success_source = current
                break
            last_error = f"{candidate} -> {resp.status} {resp.text.strip()}"
        if success_source is None:
            raise StepFailed(f"{target.label}: could not switch to an alternate source ({last_error})")
        restore_name = lower_source_name(original)
        http_json("POST", target.path(f"/devices/{target.device_id}/source/{restore_name}"))
        restored = str(read_status(target)["master"]["source"])
        assert_true(restored == original, f"{target.label}: source did not restore to {original}")
        return f"{original} -> {success_source} -> {original}"

    harness.run_step(section, "POST /devices/{id}/source/{source}", source_step)

    def config_write_step() -> str:
        baseline = read_config(target)
        input0 = baseline["inputs"][0]
        output0 = baseline["outputs"][0]
        input_gain = float(input0["gain"])
        output_gain = float(output0["gain"])
        desired_input_gain = choose_gain_delta(input_gain)
        desired_output_gain = choose_gain_delta(output_gain)
        payload = {
            "inputs": [{"index": int(input0["index"]), "gain": desired_input_gain}],
            "outputs": [{"index": int(output0["index"]), "gain": desired_output_gain}],
        }
        resp, _ = http_json("POST", target.path(f"/devices/{target.device_id}/config"), payload)
        assert_true(resp.status == 200, f"{target.label}: config POST returned {resp.status}")
        changed = read_config(target)
        approx_equal(float(changed["inputs"][0]["gain"]), desired_input_gain, tolerance=0.1)
        approx_equal(float(changed["outputs"][0]["gain"]), desired_output_gain, tolerance=0.1)
        restore = {
            "inputs": [{"index": int(input0["index"]), "gain": input_gain}],
            "outputs": [{"index": int(output0["index"]), "gain": output_gain}],
        }
        http_json("POST", target.path(f"/devices/{target.device_id}/config"), restore)
        restored = read_config(target)
        approx_equal(float(restored["inputs"][0]["gain"]), input_gain, tolerance=0.1)
        approx_equal(float(restored["outputs"][0]["gain"]), output_gain, tolerance=0.1)
        return f"input0 {input_gain}->{desired_input_gain}, output0 {output_gain}->{desired_output_gain}"

    harness.run_step(section, "POST /devices/{id}/config gain change + restore", config_write_step)

    def measurement_mode_step() -> str:
        before = read_config(target)
        before_fp = extract_input_peq_fingerprint(before)
        resp, data = http_json("POST", target.path(f"/devices/{target.device_id}/measurement-mode"))
        assert_true(resp.status == 200 and data.get("status") == "measurement_mode_active", f"{target.label}: measurement-mode enter failed")
        entered = read_config(target)
        entered_fp = extract_input_peq_fingerprint(entered)
        assert_true(any(item[2] for item in entered_fp), f"{target.label}: measurement mode did not bypass any PEQ band")
        resp, data = http_json("POST", target.path(f"/devices/{target.device_id}/measurement-mode/restore"))
        assert_true(resp.status == 200 and data.get("status") == "measurement_mode_restored", f"{target.label}: measurement-mode restore failed")
        after = read_config(target)
        after_fp = extract_input_peq_fingerprint(after)
        assert_true(after_fp == before_fp, f"{target.label}: measurement mode restore did not restore the original PEQ fingerprint")
        return "enter + restore succeeded"

    harness.run_step(section, "measurement-mode enter/restore", measurement_mode_step)

    def sync_step() -> str:
        resp, data = http_json("POST", target.path("/sync"), {"clientTimeUs": 123456789})
        assert_true(resp.status == 200 and isinstance(data, dict), f"{target.label}: /sync returned {resp.status}")
        assert_true(data.get("clientTimeUs") == 123456789, f"{target.label}: /sync did not echo clientTimeUs")
        assert_true(isinstance(data.get("serverTimeUs"), int), f"{target.label}: /sync missing integer serverTimeUs")
        return f"serverTimeUs={data['serverTimeUs']}"

    harness.run_step(section, "POST /sync", sync_step)

    def bm_route_step() -> str:
        resp = http_request("GET", target.path(f"/devices/{target.device_id}/bm"))
        assert_true(resp.status in {200, 501}, f"{target.label}: GET /devices/{target.device_id}/bm returned {resp.status}")
        if resp.status == 501:
            return "501 not supported"
        data = json.loads(resp.text)
        assert_true("channels" in data and "mixer" in data, f"{target.label}: BM status missing channels/mixer")
        return f"channels={len(data['channels'])}"

    harness.run_step(section, "GET /devices/{id}/bm", bm_route_step)

    if harness.args.allow_destructive:
        def preset_copy_step() -> str:
            resp = http_request(
                "POST",
                target.path(f"/devices/{target.device_id}/preset/copy"),
                timeout=max(35.0, harness.args.timeout),
            )
            assert_true(resp.status == 200, f"{target.label}: preset copy returned {resp.status}")
            read_status(target)
            return "200 OK"

        harness.run_step(section, "POST /devices/{id}/preset/copy", preset_copy_step)
    else:
        harness.skip(section, "POST /devices/{id}/preset/copy", "--allow-destructive not set")

    if target.kind == "esp32":
        def busy_negative_step() -> str:
            resp = http_request(
                "POST",
                target.path(f"/devices/{target.device_id}"),
                body=b"{bad",
                headers={"Content-Type": "application/json"},
            )
            assert_true(resp.status == 400, f"{target.label}: invalid JSON expected 400, got {resp.status}")
            resp = http_request("POST", target.path(f"/devices/999"), body=json.dumps({"mute": True}).encode("utf-8"), headers={"Content-Type": "application/json"})
            assert_true(resp.status == 404, f"{target.label}: out-of-range device expected 404, got {resp.status}")
            resp = http_request("POST", target.path(f"/devices/{target.device_id}/source/not-a-source"))
            assert_true(resp.status == 400, f"{target.label}: unknown source expected 400, got {resp.status}")
            resp = http_request(
                "POST",
                target.path(f"/devices/{target.device_id}/config"),
                body=json.dumps(
                    {
                        "inputs": [
                            {
                                "index": 0,
                                "peq": [
                                    {
                                        "index": 0,
                                        "coeff": {
                                            "b0": "oops",
                                            "b1": 0,
                                            "b2": 0,
                                            "a1": 0,
                                            "a2": 0,
                                        },
                                    }
                                ],
                            }
                        ]
                    }
                ).encode("utf-8"),
                headers={"Content-Type": "application/json"},
            )
            assert_true(resp.status == 400, f"{target.label}: invalid biquad expected 400, got {resp.status}")
            resp = http_request(
                "POST",
                target.path(f"/devices/{target.device_id}"),
                body=json.dumps({"padding": "x" * 3000}).encode("utf-8"),
                headers={"Content-Type": "application/json"},
            )
            assert_true(resp.status == 413, f"{target.label}: oversized body expected 413, got {resp.status}")
            return "400/404/413 negative paths"

        harness.run_step(section, "negative HTTP paths", busy_negative_step)
    else:
        harness.skip(section, "negative HTTP paths", "ESP32-focused negative checks skipped for desktop target")


def wait_for_idle_playback(target: Target, timeout: float) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        resp, state = http_json("GET", target.path(f"/devices/{target.device_id}/play"),
                                timeout=min(5.0, max(0.01, deadline - time.monotonic())))
        if resp.status == 200 and isinstance(state, dict) and state.get("playing") is False:
            return
        time.sleep(0.1)
    raise SafetyStop("playback did not become idle; refusing to restore listening volume")


@contextlib.contextmanager
def safe_test_volume(harness: Harness, target: Target) -> Iterable[None]:
    harness.confirm(target, "audible playback at -35 dB")
    if target.label in harness.baselines:
        verify_target_identity(target, harness.baselines[target.label])
    devices = read_devices(target)
    assert_true(0 <= target.device_id < len(devices), "audio target device index absent")
    version = devices[target.device_id]["version"]
    identity = {"device": {"serial": version["serial"], "hwId": version["hw_id"], "dspVersion": version["dsp_version"]}}
    harness.baselines.setdefault(target.label, identity)
    wait_for_idle_playback(target, harness.args.reconnect_timeout)
    original = float(read_status(target)["master"]["volume"])
    try:
        resp, _ = http_json("POST", target.path(f"/devices/{target.device_id}"), {"volume": -35.0})
        assert_true(resp.status == 200, f"setting -35 dB test volume returned {resp.status}")
        approx_equal(float(read_status(target)["master"]["volume"]), -35.0, tolerance=0.01)
        yield
    finally:
        wait_for_idle_playback(target, harness.args.reconnect_timeout)
        verify_target_identity(target, identity)
        resp, _ = http_json("POST", target.path(f"/devices/{target.device_id}"), {"volume": original})
        if resp.status != 200:
            raise SafetyStop(f"listening-volume restore returned {resp.status}")
        try:
            approx_equal(float(read_status(target)["master"]["volume"]), original, tolerance=0.01)
        except StepFailed as exc:
            raise SafetyStop(f"listening-volume restore failed: {exc}") from exc


def audio_request(harness: Harness, target: Target, path: str, payload: Any) -> tuple[HttpResponse, Any]:
    # Recheck for every pass, including after an operator checkpoint.
    verify_target_identity(target, harness.baselines[target.label])
    wait_for_idle_playback(target, harness.args.reconnect_timeout)
    resp, _ = http_json("POST", target.path(f"/devices/{target.device_id}"), {"volume": -35.0})
    if resp.status != 200:
        raise SafetyStop(f"setting -35 dB before playback returned {resp.status}")
    try:
        approx_equal(float(read_status(target)["master"]["volume"]), -35.0, tolerance=0.01)
    except StepFailed as exc:
        raise SafetyStop(f"test volume verification failed: {exc}") from exc
    print(f"{target.label}: test volume verified at -35 dB; audio will play now.", flush=True)
    return http_json("POST", target.path(path), payload, timeout=max(10.0, harness.args.timeout))


def run_playback_suite(harness: Harness, target: Target) -> None:
    if not harness.args.allow_audio:
        harness.skip(f"{target.label}-playback", "playback suite", "--allow-audio not set")
        return
    with safe_test_volume(harness, target):
        run_audio_steps(harness, target)


def run_audio_steps(harness: Harness, target: Target) -> None:
    section = f"{target.label}-playback"

    def sweep_only_step() -> str:
        _, before = http_json("POST", target.path("/sync"), {"clientTimeUs": 1})
        resp, data = audio_request(
            harness, target, f"/devices/{target.device_id}/play",
            {
                "markerBefore": False,
                "markerAfter": False,
                "sweepDurationSec": 1.0,
                "decaySec": 0.25,
            },
        )
        _, after = http_json("POST", target.path("/sync"), {"clientTimeUs": 2})
        assert_true(resp.status == 200 and isinstance(data, dict), f"{target.label}: /play sweep-only returned {resp.status}")
        for key in ["startedAtUs", "sweepOffsetSamples", "totalSamples", "sampleRate"]:
            assert_true(key in data, f"{target.label}: /play response missing {key}")
        assert_true(before["serverTimeUs"] <= data["startedAtUs"] <= after["serverTimeUs"], f"{target.label}: /play startedAtUs not bracketed by /sync")
        wait_for_playback_completion(data)
        return f"startedAtUs={data['startedAtUs']} totalSamples={data['totalSamples']}"

    if harness.run_step(section, "sweep-only /play", sweep_only_step) and harness.args.manual_audio:
        harness.checkpoint(target, "audio-sweep", "Sweep-only approved?")

    def marker_test_step() -> str:
        resp, data = audio_request(
            harness, target, f"/devices/{target.device_id}/play?test=marker",
            {},
        )
        assert_true(resp.status == 200 and isinstance(data, dict), f"{target.label}: marker test returned {resp.status}")
        assert_true(int(data.get("toneBurstOffsetSamples", -1)) == 0, f"{target.label}: marker test expected toneBurstOffsetSamples=0")
        wait_for_playback_completion(data)
        return f"sampleRate={data['sampleRate']} totalSamples={data['totalSamples']}"

    if harness.run_step(section, "?test=marker", marker_test_step) and harness.args.manual_audio:
        harness.checkpoint(target, "audio-marker", "Marker-only approved?")

    def combined_step() -> str:
        resp, data = audio_request(harness, target, f"/devices/{target.device_id}/play",
                                  {"markerBefore": True, "markerAfter": True, "sweepDurationSec": 1.0, "decaySec": 0.25})
        assert_true(resp.status == 200 and isinstance(data, dict), f"combined playback returned {resp.status}")
        assert_true(int(data.get("sweepOffsetSamples", -1)) > 0, "combined playback missing marker offset")
        wait_for_playback_completion(data)
        return "combined marker + sweep completed"

    if harness.run_step(section, "combined marker + sweep /play", combined_step) and harness.args.manual_audio:
        harness.checkpoint(target, "audio-combined", "Combined marker + sweep approved?")

    def legacy_field_negative_step() -> str:
        resp, _ = audio_request(harness, target, f"/devices/{target.device_id}/play", {"tailToneBurst": {}})
        assert_true(resp.status == 400, f"{target.label}: legacy playback field expected 400, got {resp.status}")
        return "legacy fields rejected"

    harness.run_step(section, "legacy playback field rejection", legacy_field_negative_step)

    if target.kind == "esp32":
        def playback_busy_step() -> str:
            play_resp, play_data = audio_request(
                harness, target, f"/devices/{target.device_id}/play",
                {
                    "markerBefore": False,
                    "markerAfter": False,
                    "sweepDurationSec": 4.0,
                    "decaySec": 1.0,
                },
            )
            assert_true(play_resp.status == 200, f"busy-test playback returned {play_resp.status}")
            deadline = time.time() + 2.0
            status_code = None
            devices_code = None
            saw_busy = False
            while time.time() < deadline:
                status = http_request("GET", target.path(f"/devices/{target.device_id}"))
                devices = http_request("GET", target.path("/api/2/devices"))
                status_code = status.status
                devices_code = devices.status
                if status_code == 409 and devices_code == 409:
                    saw_busy = True
                    break
                time.sleep(0.1)
            assert_true(
                saw_busy,
                f"{target.label}: playback busy guard expected 409/409, got {status_code}/{devices_code}",
            )
            if isinstance(play_data, dict):
                wait_for_playback_completion(play_data)
            return "busy guard returned 409 during active playback"

        harness.run_step(section, "playback busy guard", playback_busy_step)
    else:
        harness.skip(section, "playback busy guard", "busy guard check is ESP32-focused")


def run_transport_suite(harness: Harness, target: Target) -> None:
    section = f"{target.label}-transport"

    def compat_poll_step() -> str:
        msg = asyncio.run(ws_receive_one(target.compat_ws_url("poll=true"), timeout=harness.args.timeout))
        assert_true(isinstance(msg, str), f"{target.label}: compat poll WS did not return text")
        data = json.loads(msg)
        assert_true("master" in data, f"{target.label}: compat poll WS missing master")
        return f"source={data['master']['source']}"

    harness.run_step(section, "WS /devices/{id}?poll=true", compat_poll_step)

    def compat_levels_step() -> str:
        msg = asyncio.run(
            ws_receive_one(target.compat_ws_url("levels=true&poll=true"), timeout=harness.args.timeout)
        )
        assert_true(isinstance(msg, str), f"{target.label}: compat levels WS did not return text")
        data = json.loads(msg)
        assert_true("input_levels" in data and "output_levels" in data, f"{target.label}: compat levels WS missing level arrays")
        return f"inputs={len(data['input_levels'])} outputs={len(data['output_levels'])}"

    harness.run_step(section, "WS /devices/{id}?levels=true&poll=true", compat_levels_step)

    def compat_text_step() -> str:
        msg = asyncio.run(ws_receive_one(target.compat_ws_url(), send="status please", timeout=harness.args.timeout))
        assert_true(isinstance(msg, str), f"{target.label}: compat text WS did not return text")
        data = json.loads(msg)
        assert_true("master" in data, f"{target.label}: compat text WS missing master")
        return "text frame returned status JSON"

    harness.run_step(section, "WS /devices/{id} text frame", compat_text_step)

    def raw_ws_step() -> str:
        resp = asyncio.run(ws_receive_one(target.raw_ws_url(), send=bytes.fromhex("023133"), timeout=harness.args.timeout))
        assert_true(isinstance(resp, (bytes, bytearray)), f"{target.label}: raw WS did not return binary")
        assert_true(len(resp) >= 2 and resp[1] == 0x31, f"{target.label}: raw WS response does not look like master status")
        return resp.hex()

    harness.run_step(section, "WS /devices/{id}/ws binary roundtrip", raw_ws_step)

    def tcp_step() -> str:
        resp = tcp_roundtrip(target.host, target.tcp_port, bytes.fromhex("023133"), timeout=harness.args.timeout)
        assert_true(len(resp) >= 2 and resp[1] == 0x31, f"{target.label}: TCP response does not look like master status")
        return resp.hex()

    harness.run_step(section, "TCP bridge roundtrip", tcp_step)


def run_cli_suite(harness: Harness, target: Target) -> None:
    section = f"{target.label}-cli"
    if harness.args.skip_cli:
        harness.skip(section, "CLI suite", "--skip-cli set")
        return
    if target.cli_bin is None:
        harness.skip(section, "CLI suite", "no CLI binary configured")
        return

    ensure_cli_available(target.cli_bin)
    cli = [str(target.cli_bin), *target.cli_args()]

    def cli_env(**overrides: str) -> dict[str, str]:
        env = dict(os.environ)
        for key in [
            "MINIDSP_URL",
            "MINIDSPD_URL",
            "MINIDSP_SOCK",
            "MINIDSP_TCP",
            "MINIDSP_USB",
            "MINIDSP_INDEX",
        ]:
            env.pop(key, None)
        env.update(overrides)
        return env

    def run_status_command(argv: list[str], *, env: dict[str, str] | None = None) -> str:
        proc = run_command([*argv, "status"], cwd=ROOT, env=env, timeout=harness.args.timeout)
        text = proc.stdout.strip()
        assert_true(bool(text), f"{target.label}: CLI status was empty for {' '.join(argv)}")
        return text.splitlines()[0][:200]

    def status_jsonline_step() -> str:
        proc = run_command([*cli, "-o", "jsonline"], cwd=ROOT, timeout=harness.args.timeout)
        data = json.loads(proc.stdout.strip())
        assert_true("master" in data, f"{target.label}: CLI jsonline missing master")
        return f"source={data['master']['source']}"

    harness.run_step(section, "minidsp -o jsonline", status_jsonline_step)

    def status_text_step() -> str:
        return run_status_command(cli)

    harness.run_step(section, "minidsp status", status_text_step)

    def probe_net_step() -> str:
        devices = read_devices(target)
        first = devices[0]
        markers = [target.host]
        version = first.get("version", {})
        serial = version.get("serial")
        if serial is not None:
            markers.append(str(serial))
        product_name = first.get("product_name")
        if product_name:
            markers.append(str(product_name))
        proc = run_command_until_timeout(
            [str(target.cli_bin), "probe", "--net"],
            cwd=ROOT,
            timeout=12.0,
        )
        output = (proc.stdout + proc.stderr).strip()
        assert_true(any(marker in output for marker in markers), f"{target.label}: probe --net output did not mention any expected marker {markers}\n{output}")
        return output.splitlines()[-1][:200]

    harness.run_step(section, "minidsp probe --net", probe_net_step)

    def url_tcp_step() -> str:
        return run_status_command([str(target.cli_bin), "--url", f"tcp://{target.host}:{target.tcp_port}"])

    harness.run_step(section, "minidsp --url tcp://...", url_tcp_step)

    def url_http_step() -> str:
        return run_status_command([str(target.cli_bin), "--url", target.base_url])

    harness.run_step(section, "minidsp --url http://...", url_http_step)

    def url_ws_step() -> str:
        return run_status_command([str(target.cli_bin), "--url", target.raw_ws_url()])

    harness.run_step(section, "minidsp --url ws://.../devices/{id}/ws", url_ws_step)

    def daemon_url_step() -> str:
        return run_status_command([str(target.cli_bin), "--daemon-url", target.base_url])

    harness.run_step(section, "minidsp --daemon-url ...", daemon_url_step)

    def env_tcp_step() -> str:
        return run_status_command([str(target.cli_bin)], env=cli_env(MINIDSP_TCP=f"{target.host}:{target.tcp_port}"))

    harness.run_step(section, "MINIDSP_TCP env transport", env_tcp_step)

    def env_url_step() -> str:
        return run_status_command([str(target.cli_bin)], env=cli_env(MINIDSP_URL=target.base_url))

    harness.run_step(section, "MINIDSP_URL env transport", env_url_step)

    def env_daemon_url_step() -> str:
        return run_status_command([str(target.cli_bin)], env=cli_env(MINIDSPD_URL=target.base_url))

    harness.run_step(section, "MINIDSPD_URL env transport", env_daemon_url_step)

    def force_kind_step() -> str:
        return run_status_command([str(target.cli_bin), "--force-kind", "2x4hd", *target.cli_args()])

    harness.run_step(section, "minidsp --force-kind 2x4hd", force_kind_step)

    def deprecated_flags_step() -> str:
        return run_status_command(
            [
                str(target.cli_bin),
                "--force-device",
                "compat",
                "--force-dsp",
                "compat",
                *target.cli_args(),
            ]
        )

    harness.run_step(section, "deprecated --force-device/--force-dsp accepted", deprecated_flags_step)

    def all_local_devices_negative_step() -> str:
        proc = run_command(
            [
                str(target.cli_bin),
                "--all-local-devices",
                "--tcp",
                f"{target.host}:{target.tcp_port}",
                "status",
            ],
            cwd=ROOT,
            timeout=harness.args.timeout,
            check=False,
        )
        output = (proc.stdout + proc.stderr).strip()
        assert_true(proc.returncode != 0, f"{target.label}: --all-local-devices with network transport unexpectedly succeeded")
        assert_true(
            "--all-local-devices only supports local USB discovery" in output,
            f"{target.label}: --all-local-devices returned unexpected error\n{output}",
        )
        return output.splitlines()[-1][:200]

    harness.run_step(section, "non-local --all-local-devices rejection", all_local_devices_negative_step)

    def file_mode_step() -> str:
        baseline = read_status(target)
        original = float(baseline["master"]["volume"])
        desired = choose_gain_delta(original)
        with tempfile.NamedTemporaryFile("w", prefix="minidsp-file-", suffix=".txt", delete=False) as tmp:
            tmp.write(f"gain -- {desired}\n")
            tmp.write(f"gain -- {original}\n")
            path = tmp.name
        try:
            proc = run_command([*cli, "-f", path], cwd=ROOT, timeout=harness.args.timeout)
            restored = float(read_status(target)["master"]["volume"])
            approx_equal(restored, original, tolerance=0.1)
            return proc.stdout.strip()[:200] or f"{original} -> {desired} -> {original}"
        finally:
            with contextlib.suppress(FileNotFoundError):
                os.unlink(path)

    harness.run_step(section, "minidsp -f <file>", file_mode_step)


def run_ezbeq_suite(harness: Harness, target: Target) -> None:
    section = f"{target.label}-ezbeq"

    def version_step() -> str:
        resp, data = http_json("GET", target.path("/api/1/version"))
        assert_true(resp.status == 200 and isinstance(data, dict), f"{target.label}: /api/1/version failed")
        assert_true("version" in data, f"{target.label}: /api/1/version missing version")
        return data["version"]

    harness.run_step(section, "GET /api/1/version", version_step)

    def devices_step() -> str:
        data = read_ezbeq_devices(target)
        master = data.get("master", {})
        assert_true("slots" in master and isinstance(master["slots"], list), f"{target.label}: /api/2/devices missing slots")
        return f"slots={len(master['slots'])}"

    harness.run_step(section, "GET /api/2/devices", devices_step)

    for route in ["authors", "years", "audiotypes", "contenttypes", "languages"]:
        def metadata_step(route_name: str = route) -> str:
            resp, data = http_json("GET", target.path(f"/api/1/{route_name}"))
            assert_true(resp.status == 200 and isinstance(data, list), f"{target.label}: /api/1/{route_name} failed")
            assert_true(len(data) >= 1, f"{target.label}: /api/1/{route_name} returned an empty list")
            return f"{len(data)} value(s)"

        harness.run_step(section, f"GET /api/1/{route}", metadata_step)

    def levels_step() -> str:
        resp, data = http_json("GET", target.path("/api/1/devices/master/levels"))
        assert_true(resp.status == 200 and isinstance(data, dict), f"{target.label}: /api/1/devices/master/levels failed")
        assert_true("levels" in data and "ts" in data, f"{target.label}: levels response missing fields")
        return f"levels={sorted(data['levels'])[:2]}..."

    harness.run_step(section, "GET /api/1/devices/master/levels", levels_step)

    def search_stub_step() -> str:
        resp, data = http_json("GET", target.path("/api/1/search?title=Cloverfield"))
        assert_true(resp.status == 200 and isinstance(data, list), f"{target.label}: /api/1/search failed")
        return f"{len(data)} result(s)"

    harness.run_step(section, "GET /api/1/search", search_stub_step)

    entry = find_catalogue_entry(target, harness.args.ezbeq_title)
    entry_id = str(entry.get("digest") or entry.get("id"))
    entry_title = str(entry.get("formattedTitle") or entry.get("title") or entry_id)
    entry_author = str(entry.get("author") or "")
    slot = harness.args.ezbeq_slot
    initial_state = read_ezbeq_devices(target)
    initial_active_slot = next(
        (item for item in initial_state["master"]["slots"] if item.get("active")),
        None,
    )
    initial_active_slot_id = str(initial_active_slot["id"]) if initial_active_slot else "1"

    def load_clear_step() -> str:
        baseline_config = read_config(target)
        baseline_fp = extract_input_peq_fingerprint(baseline_config)
        payload = {"entryId": entry_id, "title": entry_title, "author": entry_author}
        resp, data = http_json("PUT", target.path(f"/api/1/devices/master/filter/{slot}"), payload, timeout=max(10.0, harness.args.timeout))
        assert_true(resp.status == 200 and isinstance(data, dict), f"{target.label}: filter load returned {resp.status}")
        slots = ezbeq_slots_from_document(data, context=f"{target.label}: filter load response")
        slot_state = next((item for item in slots if str(item["id"]) == str(slot)), None)
        assert_true(slot_state is not None, f"{target.label}: loaded slot {slot} missing from response")
        assert_true(slot_state["last"] != "Empty", f"{target.label}: slot {slot} still empty after load")
        changed_fp = extract_input_peq_fingerprint(read_config(target))
        assert_true(changed_fp != baseline_fp, f"{target.label}: /config fingerprint did not change after BEQ load")
        resp, data = http_json("DELETE", target.path(f"/api/1/devices/master/filter/{slot}"), timeout=max(10.0, harness.args.timeout))
        assert_true(resp.status == 200 and isinstance(data, dict), f"{target.label}: filter clear returned {resp.status}")
        slot_state = next(
            (item for item in ezbeq_slots_from_document(data, context=f"{target.label}: filter clear response") if str(item["id"]) == str(slot)),
            None,
        )
        assert_true(slot_state and slot_state["last"] == "Empty", f"{target.label}: slot {slot} did not clear")
        http_json("PUT", target.path(f"/api/1/devices/master/config/{initial_active_slot_id}/active"))
        restored_fp = extract_input_peq_fingerprint(read_config(target))
        assert_true(restored_fp == baseline_fp, f"{target.label}: /config fingerprint did not restore after BEQ clear")
        return f"slot {slot}: {entry_title}"

    harness.run_step(section, "PUT/DELETE /api/1/devices/master/filter/{slot}", load_clear_step)

    def activate_step() -> str:
        resp, data = http_json("PUT", target.path(f"/api/1/devices/master/config/{slot}/active"))
        assert_true(resp.status == 200 and isinstance(data, dict), f"{target.label}: activate slot returned {resp.status}")
        active = next(
            (item for item in ezbeq_slots_from_document(data, context=f"{target.label}: activate response") if item["active"]),
            None,
        )
        assert_true(active is not None and str(active["id"]) == str(slot), f"{target.label}: slot {slot} is not active")
        http_json("PUT", target.path(f"/api/1/devices/master/config/{initial_active_slot_id}/active"))
        return f"active slot {slot}"

    harness.run_step(section, "PUT /api/1/devices/master/config/{slot}/active", activate_step)

    def patch_volume_step() -> str:
        original = float(read_status(target)["master"]["volume"])
        desired = choose_gain_delta(original)
        resp, data = http_json("PATCH", target.path("/api/3/devices/master"), {"masterVolume": desired})
        assert_true(resp.status == 200 and isinstance(data, dict), f"{target.label}: PATCH /api/3/devices/master returned {resp.status}")
        approx_equal(float(read_status(target)["master"]["volume"]), desired, tolerance=0.1)
        http_json("PATCH", target.path("/api/3/devices/master"), {"masterVolume": original})
        approx_equal(float(read_status(target)["master"]["volume"]), original, tolerance=0.1)
        return f"{original} -> {desired} -> {original}"

    harness.run_step(section, "PATCH /api/3/devices/master masterVolume", patch_volume_step)

    if harness.args.allow_destructive:
        def overwrite_step() -> str:
            resp, data = http_json("PUT", target.path("/api/1/devices/master/commands"), {"overwrite": True}, timeout=max(30.0, harness.args.timeout))
            assert_true(resp.status == 200 and isinstance(data, dict), f"{target.label}: overwrite command returned {resp.status}")
            return data.get("status", "ok")

        harness.run_step(section, "PUT /api/1/devices/master/commands overwrite", overwrite_step)
    else:
        harness.skip(section, "PUT /api/1/devices/master/commands overwrite", "--allow-destructive not set")

    def ezbeq_ws_step() -> str:
        messages = asyncio.run(
            ws_receive_many(
                target.ezbeq_ws_url(),
                sends=["load catalogue", "subscribe levels master"],
                timeout=harness.args.timeout,
                count=3,
            )
        )
        parsed = []
        for item in messages:
            assert_true(isinstance(item, str), f"{target.label}: ezBEQ /ws returned non-text frame")
            parsed.append(json.loads(item))
        kinds = {msg.get("message") for msg in parsed}
        assert_true("Catalogue" in kinds, f"{target.label}: ezBEQ /ws did not emit Catalogue")
        assert_true("Levels" in kinds or "DeviceState" in kinds, f"{target.label}: ezBEQ /ws did not emit Levels/DeviceState")
        return ",".join(sorted(k for k in kinds if k))

    harness.run_step(section, "WS /ws catalogue + levels", ezbeq_ws_step)

    def hosted_ui_step() -> str:
        root = http_request("GET", target.path("/"))
        assert_true(root.status == 200, f"{target.label}: GET / returned {root.status}")
        assert_true("ezbeq" in root.text.lower(), f"{target.label}: GET / did not look like the hosted UI")
        version = http_request("GET", target.path("/version.txt"))
        assert_true(version.status == 200 and version.text.strip(), f"{target.label}: GET /version.txt failed")
        entries = read_catalogue_entries(target)
        assert_true(len(entries) >= 1, f"{target.label}: cached /database.json entries list was empty")
        return f"{version.text.strip()} entries={len(entries)}"

    harness.run_step(section, "hosted UI static bundle", hosted_ui_step)


@dataclasses.dataclass(slots=True)
class SpawnedEzbeq:
    process: subprocess.Popen[str]
    base_url: str
    temp_dir: Path


def write_upstream_ezbeq_config(temp_dir: Path, cli_bin: Path, target: Target, port: int) -> None:
    config = f"""port: {port}
accessLogging: false
debugLogging: false
devices:
  master:
    type: minidsp
    exe: {json.dumps(str(cli_bin))}
    cmdTimeout: 10
    ignoreRetcode: false
    options: {json.dumps(f"--tcp {target.host}:{target.tcp_port}")}
    wsDeviceId: {target.device_id}
    wsIp: {json.dumps(f"{target.host}:{target.port}")}
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
"""
    (temp_dir / "ezbeq.yml").write_text(config)


def spawn_upstream_ezbeq(cli_bin: Path, target: Target, port: int) -> SpawnedEzbeq:
    temp_dir = Path(tempfile.mkdtemp(prefix="minidsp-upstream-ezbeq-"))
    write_upstream_ezbeq_config(temp_dir, cli_bin, target, port)
    env = dict(os.environ)
    env["EZBEQ_CONFIG_HOME"] = str(temp_dir)
    proc = subprocess.Popen(
        ["uvx", "--from", "ezbeq", "ezbeq"],
        cwd=ROOT,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    base_url = f"http://127.0.0.1:{port}"
    try:
        wait_for_http(base_url + "/api/1/version", timeout=60.0)
        wait_for_upstream_ezbeq_catalogue(base_url, timeout=60.0)
    except Exception:
        with contextlib.suppress(Exception):
            proc.terminate()
        output = ""
        with contextlib.suppress(Exception):
            output = proc.stdout.read() if proc.stdout else ""
        raise StepFailed(f"failed to start upstream ezBEQ at {base_url}\n{output}")
    return SpawnedEzbeq(process=proc, base_url=base_url, temp_dir=temp_dir)


def cleanup_spawned_ezbeq(spawned: SpawnedEzbeq) -> None:
    with contextlib.suppress(Exception):
        spawned.process.terminate()
        spawned.process.wait(timeout=10)
    with contextlib.suppress(Exception):
        if spawned.process.poll() is None:
            spawned.process.kill()
    with contextlib.suppress(Exception):
        for path in sorted(spawned.temp_dir.rglob("*"), reverse=True):
            if path.is_file():
                path.unlink()
            elif path.is_dir():
                path.rmdir()
        spawned.temp_dir.rmdir()


def run_upstream_ezbeq_suite(harness: Harness, target: Target) -> None:
    section = f"{target.label}-upstream-ezbeq"
    if target.cli_bin is None:
        harness.skip(section, "upstream ezBEQ suite", "CLI binary unavailable")
        return

    ensure_cli_available(target.cli_bin)
    spawned = spawn_upstream_ezbeq(target.cli_bin, target, harness.args.upstream_ezbeq_port)
    try:
        upstream_target = Target(
            label=f"{target.label}-upstream-ezbeq",
            kind="upstream-ezbeq",
            base_url=spawned.base_url,
            device_id=target.device_id,
            tcp_port=target.tcp_port,
            cli_bin=None,
        )

        def version_step() -> str:
            resp, data = http_json("GET", upstream_target.path("/api/1/version"))
            assert_true(resp.status == 200 and isinstance(data, dict), "upstream ezBEQ /api/1/version failed")
            return data["version"]

        harness.run_step(section, "spawn + GET /api/1/version", version_step)

        def devices_step() -> str:
            resp, data = http_json("GET", upstream_target.path("/api/2/devices"))
            assert_true(resp.status == 200 and isinstance(data, dict), "upstream ezBEQ /api/2/devices failed")
            assert_true("master" in data, "upstream ezBEQ device list missing master")
            return f"slots={len(data['master']['slots'])}"

        harness.run_step(section, "GET /api/2/devices", devices_step)

        def levels_step() -> str:
            resp, data = http_json("GET", upstream_target.path("/api/1/devices/master/levels"))
            assert_true(resp.status == 200 and isinstance(data, dict), "upstream ezBEQ /levels failed")
            return f"levels={len(data.get('levels', {}))}"

        harness.run_step(section, "GET /api/1/devices/master/levels", levels_step)

        def load_clear_step() -> str:
            resp, data = http_json(
                "GET",
                upstream_target.path("/api/1/search?title=" + urllib.parse.quote(harness.args.ezbeq_title)),
            )
            assert_true(resp.status == 200 and isinstance(data, list), "upstream ezBEQ /search failed")
            match = None
            wanted = harness.args.ezbeq_title.casefold()
            for entry in data:
                title = str(entry.get("formattedTitle") or entry.get("title") or "")
                if title.casefold() == wanted:
                    match = entry
                    break
            if match is None and data:
                match = data[0]
            assert_true(match is not None, f"upstream ezBEQ search returned no match for {harness.args.ezbeq_title!r}")
            entry_id = str(match.get("id") or match.get("digest"))
            resp, data = http_json(
                "PUT",
                upstream_target.path(f"/api/1/devices/master/filter/{harness.args.ezbeq_slot}"),
                {"entryId": entry_id},
                timeout=max(20.0, harness.args.timeout),
            )
            assert_true(resp.status == 200 and isinstance(data, dict), f"upstream ezBEQ load returned {resp.status}")
            slot_state = next(
                (
                    item
                    for item in ezbeq_slots_from_document(data, context="upstream ezBEQ load response")
                    if str(item["id"]) == str(harness.args.ezbeq_slot)
                ),
                None,
            )
            assert_true(slot_state and slot_state["last"] != "Empty", "upstream ezBEQ slot did not load")
            resp, data = http_json(
                "DELETE",
                upstream_target.path(f"/api/1/devices/master/filter/{harness.args.ezbeq_slot}"),
                timeout=max(20.0, harness.args.timeout),
            )
            assert_true(resp.status == 200 and isinstance(data, dict), f"upstream ezBEQ clear returned {resp.status}")
            slot_state = next(
                (
                    item
                    for item in ezbeq_slots_from_document(data, context="upstream ezBEQ clear response")
                    if str(item["id"]) == str(harness.args.ezbeq_slot)
                ),
                None,
            )
            assert_true(slot_state and slot_state["last"] == "Empty", "upstream ezBEQ slot did not clear")
            return f"slot {harness.args.ezbeq_slot} load + clear"

        harness.run_step(section, "PUT/DELETE filter through real upstream ezBEQ", load_clear_step)
    finally:
        cleanup_spawned_ezbeq(spawned)


def build_targets(args: argparse.Namespace) -> list[Target]:
    targets = []
    if args.desktop_url:
        targets.append(
            make_target("desktop", "desktop", args.desktop_url, args, args.desktop_cli_transport)
        )
    if args.esp32_url:
        targets.append(
            make_target("esp32", "esp32", args.esp32_url, args, args.esp32_cli_transport)
        )
    return targets


def assert_snapshot_busy(harness: Harness, target: Target, artifacts: SnapshotArtifacts, method: str) -> str:
    baseline = json.loads(artifacts.snapshot_before.read_text())
    verify_target_identity(target, baseline)
    current = read_status(target)["master"]
    config = read_config(target)
    result: dict[str, Any] = {}
    mutations_observed: list[dict[str, Any]] = []
    busy_observed = False
    started = threading.Event()

    def snapshot_worker() -> None:
        started.set()
        try:
            result["response"], result["snapshot"] = http_json(
                method, target.path(f"/devices/{target.device_id}/snapshot"),
                baseline if method == "POST" else None, timeout=300.0)
        except Exception as exc:
            result["error"] = exc

    worker = threading.Thread(target=snapshot_worker, daemon=True)
    worker.start()
    started.wait(harness.args.timeout)
    try:
        deadline = time.monotonic() + harness.args.timeout
        while worker.is_alive() and time.monotonic() < deadline:
            probe = http_request("GET", target.path(f"/devices/{target.device_id}"), timeout=harness.args.timeout)
            if probe.status == 409:
                busy_observed = True
                break
            time.sleep(0.01)
        else:
            raise StepFailed("snapshot busy window not observed; concurrency coverage is unproven")
        # Same-value writes minimize damage if a broken guard admits a mutation.
        mutations = [
            (f"/devices/{target.device_id}", {"volume": current["volume"]}),
            (f"/devices/{target.device_id}/config", {"outputs": [{"index": config["outputs"][0]["index"],
                                                                      "gain": config["outputs"][0]["gain"]}]}),
        ]
        for path, payload in mutations:
            resp, _ = http_json("POST", target.path(path), payload, timeout=harness.args.timeout)
            mutations_observed.append({"path": path, "status": resp.status})
            assert_true(resp.status == 409, f"concurrent mutation {path} expected 409, got {resp.status}")
    finally:
        worker.join(timeout=305.0)
        json_dump(artifacts.root / f"snapshot-busy-{method.lower()}.json",
                  {"status": result["response"].status if "response" in result else None,
                   "snapshot": result.get("snapshot"), "error": str(result.get("error", "")),
                   "busyObserved": busy_observed, "mutations": mutations_observed, "timedOut": worker.is_alive()})
        if worker.is_alive():
            raise SafetyStop("snapshot request exceeded its timeout; operation state is unknown")
    if "error" in result:
        raise StepFailed(f"snapshot request failed: {result['error']}")
    assert_true(result["response"].status == 200, f"snapshot {method} returned {result['response'].status}")
    assert_snapshot_equal(baseline, result["snapshot"])
    return "master and config mutations both returned 409 inside observed snapshot busy window"


def wait_for_reconnect(target: Target, baseline: dict[str, Any], timeout: float) -> str:
    deadline = time.monotonic() + timeout
    stable_reads = 0
    last_error = "no successful status read"
    while time.monotonic() < deadline:
        try:
            remaining = max(0.01, deadline - time.monotonic())
            verify_target_identity(target, baseline, timeout=min(5.0, remaining))
            resp, status = http_json("GET", target.path(f"/devices/{target.device_id}"),
                                     timeout=min(5.0, max(0.01, deadline - time.monotonic())))
            assert_true(resp.status == 200 and isinstance(status, dict) and isinstance(status.get("master"), dict),
                        f"reconnect status returned {resp.status}")
            stable_reads += 1
            if stable_reads >= 2 and time.monotonic() <= deadline:
                return "same device identity and two consecutive live status reads"
        except SafetyStop:
            raise
        except (StepFailed, OSError) as exc:
            last_error = str(exc)
            stable_reads = 0
        time.sleep(min(0.25, max(0.0, deadline - time.monotonic())))
    raise SafetyStop(f"{target.label}: reconnect timed out after {timeout}s ({last_error})")


def run_recovery_suite(harness: Harness, target: Target, artifacts: SnapshotArtifacts) -> None:
    section = f"{target.label}-recovery"
    baseline = json.loads(artifacts.snapshot_before.read_text())

    def restore_baseline() -> str:
        restore_snapshot(target, artifacts.snapshot_before, artifacts.restore_result)
        return verify_snapshot_restore(target, artifacts)

    # Start recovery coverage from the exact saved state, not earlier mutation state.
    harness.run_step(section, "restore baseline before recovery", restore_baseline, hard_stop=True)
    if harness.args.destructive_profile == "full":
        count = baseline["device"]["presetCount"]
        slot = (baseline["master"]["activePreset"] + 1) % count
        routes = ["preset/reset", f"preset/{slot}/reset", "preset/reset-all", "reboot"]
        for route in routes:
            def recovery_step(route: str = route) -> str:
                verify_target_identity(target, baseline)
                harness.confirm(target, f"POST /devices/{target.device_id}/{route}")
                resp, _ = http_json("POST", target.path(f"/devices/{target.device_id}/{route}"), timeout=300.0)
                assert_true(resp.status == 200, f"{route} returned {resp.status}")
                wait_for_reconnect(target, baseline, harness.args.reconnect_timeout)
                if route == "reboot":
                    recovered = export_snapshot(target, artifacts.root / "snapshot-after-reboot.json")
                    assert_snapshot_equal(baseline, recovered)
                else:
                    export_snapshot(target, artifacts.root / f"snapshot-after-{route.replace('/', '-')}.json")
                restore_baseline()
                return "reconnected with same identity; strict snapshot checked and baseline restored"
            harness.run_step(section, route, recovery_step, hard_stop=True)
    if harness.args.manual_reconnect:
        if not harness.args.ci and (sys.stdin.isatty() and not harness.args.non_interactive or
                                   f"{target.label}.reconnect" in harness.manual_answers):
            harness.confirm(target, "physical unplug/replug or power cycle")
        answer = harness.checkpoint(target, "reconnect", "Unplug/replug or power-cycle the MiniDSP, then confirm reconnect timing and sanity (include elapsed time in note).")
        if answer == "pass":
            def physical_recovery() -> str:
                wait_for_reconnect(target, baseline, harness.args.reconnect_timeout)
                snapshot = export_snapshot(target, artifacts.root / "snapshot-after-manual-reconnect.json")
                assert_snapshot_equal(baseline, snapshot)
                return "physical recovery identity and baseline snapshot match"
            harness.run_step(section, "physical reconnect verification", physical_recovery, hard_stop=True)


def run_target_suite(harness: Harness, target: Target, run_dir: Path | None) -> None:
    guard = None
    deferred_exit: SystemExit | None = None
    try:
        guard = start_destructive_snapshot_guard(harness, target, run_dir)
        if guard:
            harness.baselines[target.label] = json.loads(guard.snapshot_before.read_text())
            # Prove the baseline can be restored before enabling destructive groups.
            finish_destructive_snapshot_guard(harness, target, guard)
            if harness.args.destructive_profile == "full":
                for method in ("GET", "POST"):
                    harness.run_step(f"{target.label}-snapshot", f"{method} snapshot concurrent mutations",
                                     lambda method=method: assert_snapshot_busy(harness, target, guard, method), hard_stop=True)
        for suite in (run_common_http_suite, run_transport_suite, run_cli_suite, run_playback_suite):
            if guard:
                verify_target_identity(target, json.loads(guard.snapshot_before.read_text()))
            suite(harness, target)
        if target.kind == "esp32":
            run_ezbeq_suite(harness, target)
            run_esp32_specific_suite(harness, target)
        else:
            harness.skip(f"{target.label}-ezbeq", "embedded ezBEQ suite", "embedded ezBEQ routes are ESP32-only")
        if harness.args.run_upstream_ezbeq:
            run_upstream_ezbeq_suite(harness, target)
        if guard and (harness.args.destructive_profile == "full" or harness.args.manual_reconnect):
            run_recovery_suite(harness, target, guard)
        if harness.args.manual_device_console:
            harness.checkpoint(target, "device-console", "Device Console discovers/connects, basic controls work, and no initialize/disconnect prompt regression? Close it before continuing.")
    except SystemExit as exc:
        deferred_exit = exc
    finally:
        if guard:
            harness.run_step(f"{target.label}-snapshot", "idle before final restore",
                             lambda: wait_for_idle_playback(target, harness.args.reconnect_timeout), hard_stop=True)
        finish_destructive_snapshot_guard(harness, target, guard)
        if deferred_exit is not None:
            raise deferred_exit


def plan_lines(args: argparse.Namespace) -> list[str]:
    lines = ["Plan only: no target I/O, CLI, builds, prompts or artifact writes."]
    if not args.skip_static:
        lines.append("static: repo checks / CLI and ESP32 builds / catalogue build")
    for target in build_targets(args):
        lines.append(f"{target.label}: {target.base_url} device index {target.device_id}")
        if args.restore_on_exit:
            lines += ["  preflight: idle playback / strict complete snapshot / device identity / baseline HTTP state",
                      "  hard stop: failed capture, unsupported strict snapshot, identity mismatch or failed restore/equality",
                      "  preflight: import baseline, re-export and compare before mutations"]
        if args.destructive_profile == "full":
            lines.append("  snapshot GET and POST: concurrent master/config mutations must return 409")
        lines += ["  HTTP control/config/measurement-mode compatibility (with local restores)",
                  "  raw WebSocket / TCP / CLI flags, URLs and environment compatibility"]
        if args.allow_destructive:
            lines.append("  destructive: preset copy / ESP32 ezBEQ overwrite")
        if args.allow_audio:
            lines += ["  confirm audio (interactive YES or --yes-i-know)",
                      "  audio: set and verify -35 dB / announce / sweep-only / marker-only / combined / legacy rejection / ESP32 busy",
                      "  audio: wait until idle, restore and verify previous listening volume"]
        if target.kind == "esp32":
            lines.append("  embedded ezBEQ REST/WS / hosted bundle / updater compatibility")
        if args.run_upstream_ezbeq:
            lines.append("  optional spawned upstream ezBEQ interoperability")
        if args.destructive_profile == "full":
            lines += ["  restore baseline before recovery",
                      "  confirm each reset/reboot (interactive YES or --yes-i-know)",
                      "  reset active preset / reset alternate preset / reset-all (reboots) / reboot",
                      "  after each: bounded stable reconnect / identity / strict snapshot / baseline restore and equality"]
        for enabled, name in [(args.manual_audio, "audio-sweep / audio-marker / audio-combined"),
                              (args.manual_reconnect, "reconnect (physical recovery plus identity/snapshot check)"),
                              (args.manual_device_console, "device-console")]:
            if enabled:
                lines.append(f"  manual JSON checkpoints: {name} (CI skips; otherwise prompt or answer file)")
        if args.restore_on_exit:
            lines.append("  finally: idle / identity / snapshot import / strict snapshot equality / HTTP and ezBEQ state / durable reports")
        if target.kind == "desktop":
            lines.append("  separate required hardware lane: direct-USB CLI snapshot round-trip with daemon stopped")
    return lines


def main() -> int:
    args = parse_args()
    if args.plan:
        print("\n".join(plan_lines(args)))
        return 0
    harness = Harness(args)
    run_dir = prepare_run_artifact_dir(args.artifact_root) if (args.restore_on_exit or args.allow_audio or
              args.manual_device_console or args.manual_reconnect) else None
    harness.run_dir = run_dir

    try:
        if not args.skip_static:
            run_static_checks(harness, args)
        else:
            harness.skip("static", "static repo checks", "--skip-static set")
        for target in build_targets(args):
            run_target_suite(harness, target, run_dir)
    except (Exception, SystemExit, KeyboardInterrupt) as exc:
        if not harness.results or harness.results[-1].status != "FAIL":
            harness.results.append(StepResult("safety", "run aborted", "FAIL", 0.0, str(exc) or type(exc).__name__))
            harness.failures += 1
        print(f"Run stopped: {exc}", file=sys.stderr)
        return 1
    finally:
        print("\n".join(harness.summary_lines()))
        if args.report:
            harness.write_report(args.report)
        if run_dir is not None:
            harness.write_report(run_dir / "report.md")
            harness.write_json_report(run_dir / "report.json")
            print(f"Artifact reports written to {run_dir}")
    return 1 if harness.failures else 0


def run_esp32_specific_suite(harness: Harness, target: Target) -> None:
    section = f"{target.label}-board"

    def debug_info_step() -> str:
        resp, data = http_json("GET", target.path("/debug/info"))
        assert_true(resp.status == 200 and isinstance(data, dict), f"{target.label}: /debug/info failed")
        assert_true("wifi" in data and "usb" in data, f"{target.label}: /debug/info missing wifi/usb")
        return f"ip={data['wifi'].get('ip')} usb_connected={data['usb'].get('connected')}"

    harness.run_step(section, "GET /debug/info", debug_info_step)

    def updater_step() -> str:
        resp, data = http_json("GET", target.path("/debug/updater"))
        assert_true(resp.status == 200 and isinstance(data, dict), f"{target.label}: /debug/updater failed")
        assert_true(data.get("hasCatalogueData") is True, f"{target.label}: updater says hasCatalogueData=false")
        resp, data = http_json("POST", target.path("/debug/updater/check"))
        assert_true(resp.status == 200 and isinstance(data, dict), f"{target.label}: /debug/updater/check failed")
        assert_true(data.get("status") in {"current", "updated"}, f"{target.label}: unexpected updater status {data}")
        version = http_request("GET", target.path("/version.txt"))
        assert_true(version.status == 200 and version.text.strip(), f"{target.label}: /version.txt failed")
        return f"installed={version.text.strip()}"

    harness.run_step(section, "GET/POST /debug/updater", updater_step)

    def bundle_step() -> str:
        version = http_request("GET", target.path("/version.txt"))
        assert_true(version.status == 200 and version.text.strip(), f"{target.label}: /version.txt failed")
        root = http_request("GET", target.path("/"))
        assert_true(root.status == 200 and "ezbeq" in root.text.lower(), f"{target.label}: hosted UI did not load")
        entries = read_catalogue_entries(target)
        assert_true(len(entries) >= 1, f"{target.label}: cached /database.json entries list was empty")
        return f"{version.text.strip()} entries={len(entries)}"

    harness.run_step(section, "hosted bundle endpoints", bundle_step)


if __name__ == "__main__":
    sys.exit(main())
