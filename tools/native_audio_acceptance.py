#!/usr/bin/env python3
"""Explicitly gated Windows OUTPUT/SYSTEM loopback acceptance via existing CLI.

No microphone/input capture, playback, network or PCM persistence. A temporary
owned native JSON contains authoring metadata only and is removed after exit.
Evidence keeps device identities/counters/errors, never PCM or channel meters.
"""
from __future__ import annotations
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
MAX_REPLY_BYTES = 2 * 1024 * 1024
MAX_STDERR_BYTES = 128 * 1024
MAX_REQUESTS = 64
PROCESS_SECONDS = 12.0
NATIVE_SECONDS = 1.5


class AcceptanceError(RuntimeError):
    pass


class Cli:
    def __init__(self, executable):
        self.deadline = time.monotonic() + PROCESS_SECONDS
        self.responses = queue.Queue(maxsize=4)
        self.stderr = bytearray()
        self.reader_problem = None
        self.requests = 0
        self.protocol_synced = True
        self.process = subprocess.Popen(
            [str(executable), "--stdio"], cwd=ROOT, stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=8192,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        self.threads = [threading.Thread(target=self.read_stdout, daemon=True),
                        threading.Thread(target=self.read_stderr, daemon=True)]
        for thread in self.threads:
            thread.start()

    def read_stdout(self):
        try:
            while True:
                line = self.process.stdout.readline(MAX_REPLY_BYTES + 1)
                if len(line) > MAX_REPLY_BYTES:
                    raise AcceptanceError("CLI reply exceeds 2 MiB")
                self.responses.put_nowait(line or None)
                if not line:
                    return
        except Exception as cause:
            self.reader_problem = str(cause)

    def read_stderr(self):
        try:
            while True:
                data = self.process.stderr.read(4096)
                if not data:
                    return
                if len(self.stderr) + len(data) > MAX_STDERR_BYTES:
                    raise AcceptanceError("CLI stderr exceeds 128 KiB")
                self.stderr.extend(data)
        except Exception as cause:
            self.reader_problem = str(cause)

    def call(self, request, timeout=2.0):
        self.requests += 1
        if self.requests > MAX_REQUESTS:
            raise AcceptanceError("CLI request-count bound exceeded")
        if self.reader_problem:
            raise AcceptanceError(self.reader_problem)
        timeout = min(timeout, self.deadline - time.monotonic())
        if timeout <= 0:
            raise AcceptanceError("CLI process deadline exceeded")
        payload = (json.dumps(request, ensure_ascii=False) + "\n").encode("utf-8")
        if len(payload) > 65536:
            raise AcceptanceError("CLI request staging exceeds 64 KiB")
        self.process.stdin.write(payload)
        self.process.stdin.flush()
        try:
            line = self.responses.get(timeout=timeout)
        except queue.Empty as cause:
            self.protocol_synced = False
            raise AcceptanceError(f"CLI request deadline: {request['request']}") from cause
        if line is None:
            self.protocol_synced = False
            raise AcceptanceError(f"CLI exited before reply: {request['request']}")
        response = json.loads(line)
        if response.get("ok") is not True:
            raise AcceptanceError(json.dumps(response.get("error"), ensure_ascii=False))
        return response["result"]

    def finish(self):
        if self.process.stdin and not self.process.stdin.closed:
            self.process.stdin.close()
        try:
            code = self.process.wait(timeout=max(0.01, min(2.0, self.deadline - time.monotonic())))
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=2)
            raise AcceptanceError("CLI shutdown deadline; process forcibly terminated")
        for thread in self.threads:
            thread.join(timeout=0.2)
        return code

    def abort(self):
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait(timeout=2)
        for thread in self.threads:
            thread.join(timeout=0.2)


def block_metadata(output):
    block = output["block"]
    samples = block.pop("samples", None)
    if not isinstance(samples, list):
        raise AcceptanceError("CLI block lacks expected transient PCM array")
    count = len(samples)
    samples.clear()
    if block["frames"] != 1024 or count > 8192 or count % 1024:
        raise AcceptanceError("unexpected Program block shape")
    # Explicit whitelist: samples, peaks/RMS and arbitrary future fields never
    # reach persisted evidence. Counts and identities remain exact.
    metadata = {name: block[name] for name in (
        "instance", "epoch", "session", "data_generation", "routing_serial",
        "settings", "start_frame", "frames", "clipped_samples", "gaps")}
    metadata["transient_sample_count"] = count
    return {"key": output["key"], "block": metadata, "stats": output["stats"]}


def run(executable):
    report = {
        "schema": 1, "generated_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "status": "failed", "scope": "Windows output/system WASAPI loopback only",
        "exe": str(executable.resolve()), "exe_sha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
        "microphone_requested": False, "playback_requested": False, "network_requested": False,
        "pcm_persisted": False, "channel_meters_persisted": False, "blocks": [],
        "native_stop_acknowledgment_verified": False,
        "limitations": [
            "One endpoint and bounded window; not all-device or signal-fidelity acceptance.",
            "CLI Stop explicitly reports native_stops_acknowledged=false.",
            "CLI Close is asynchronous; successful process exit alone does not prove WASAPI owner join.",
            "Microphone/input capture, speaker monitoring and sound playback remain unverified.",
        ],
    }
    client = None
    started = False
    stopped = False
    try:
        with tempfile.TemporaryDirectory(prefix="skylive-output-loopback-") as temporary:
            client = Cli(executable)
            devices = client.call({"request": "list_audio_devices", "flow": "output"})
            report["enumeration"] = devices
            endpoints = [device for device in devices["devices"]
                         if device["flow"] == "output" and device["target"]["kind"] == "windows_endpoint"]
            if not endpoints:
                report["status"] = "unavailable"
                report["reason"] = "No output endpoint enumerated; no capture attempted."
            else:
                selected = next((device for device in endpoints if device["is_default"]), endpoints[0])
                report["selected_device"] = selected
                state = client.call({"request": "snapshot"})
                scene = state["preview"]
                binding = {"flow": "output", "target": selected["target"],
                           "use_device_timing": True, "exclude_own_audio": False}
                report["binding"] = binding
                report["add_source"] = client.call({"request": "command", "command": {
                    "op": "add_audio_capture_source", "scene": scene,
                    "name": "Native output loopback acceptance", "binding": binding}})
                plan = client.call({"request": "audio_plan"})
                report["program_plan"] = plan
                if len(plan["plan"]["sources"]) != 1 or plan["plan"]["sources"][0]["binding"] != binding:
                    raise AcceptanceError("Program route does not contain exactly the requested output binding")
                saved = client.call({"request": "save", "path": str(Path(temporary) / "session.json")})
                if saved.get("accepted") is not True:
                    raise AcceptanceError("temporary metadata save was not accepted")
                report["temporary_metadata_snapshot_saved"] = True
                began = time.monotonic()
                native_deadline = began + NATIVE_SECONDS
                report["start"] = client.call({"request": "start_native_audio_program", "time_ns": 0}, timeout=0.75)
                started = True
                # First two blocks, then allow idle loopback time to deliver a
                # real packet. Read at most four Program blocks in total.
                while len(report["blocks"]) < 2 and time.monotonic() < native_deadline - 0.4:
                    result = client.call({"request": "read_native_audio_program", "max_blocks": min(2, 2 - len(report["blocks"]))},
                                         timeout=min(0.35, native_deadline - time.monotonic()))
                    report["last_status"] = result.get("status")
                    for output in result["blocks"]:
                        report["blocks"].append(block_metadata(output))
                    if len(report["blocks"]) < 2:
                        time.sleep(0.04)
                capture = {}
                while time.monotonic() < native_deadline - 0.4:
                    capture = client.call({"request": "capture_audio"}, timeout=min(0.3, native_deadline - time.monotonic()))
                    if capture.get("packets") or any(problem["code"] != "audio_capture_waiting" for problem in capture.get("problems", [])):
                        break
                    time.sleep(0.04)
                for packet in capture.get("packets", []):
                    packet.pop("meters", None)
                report["capture_metadata"] = capture
                if len(report["blocks"]) < 4:
                    result = client.call({"request": "read_native_audio_program", "max_blocks": 4 - len(report["blocks"])},
                                         timeout=max(0.05, native_deadline - time.monotonic()))
                    report["last_status"] = result.get("status")
                    for output in result["blocks"]:
                        report["blocks"].append(block_metadata(output))
                report["stop"] = client.call({"request": "stop_native_audio_program"},
                                              timeout=max(0.05, native_deadline - time.monotonic()))
                stopped = True
                report["native_control_elapsed_seconds"] = time.monotonic() - began
                after_stop = client.call({"request": "read_native_audio_program", "max_blocks": 1})
                after_stop["blocks"] = [block_metadata(output) for output in after_stop["blocks"]]
                report["after_stop"] = after_stop
                if after_stop["blocks"]:
                    raise AcceptanceError("stopped transport published stale PCM")
                status = report.get("last_status") or {}
                problems = [problem for problem in capture.get("problems", []) if problem["code"] != "audio_capture_waiting"]
                report["native_packets_observed"] = status.get("input_packets", 0)
                if report["native_control_elapsed_seconds"] > 2.0:
                    raise AcceptanceError("native-control acceptance exceeded2s")
                expected_backpressure = bool(
                    status.get("problem", {}).get("code") == "audio_capture_backpressure"
                    and status.get("dropped_blocks", 0) > 0 and not status.get("input_errors", 0)
                ) if status.get("problem") else False
                report["bounded_output_backpressure_observed"] = expected_backpressure
                if status.get("input_errors", 0) or (status.get("problem") and not expected_backpressure) or problems:
                    raise AcceptanceError("native input/processing problems: " + json.dumps(
                        {"status_problem": status.get("problem"), "input_errors": status.get("input_errors"), "capture_problems": problems},
                        ensure_ascii=False))
                if len(report["blocks"]) < 2:
                    raise AcceptanceError("fewer than2 Program output blocks observed")
                if not report["native_packets_observed"]:
                    report["status"] = "inconclusive_native_packet_acquisition"
                    report["reason"] = "Program gap output ran, but no real loopback input packets arrived. Idle output and acquisition defects cannot be distinguished without a signal stimulus."
                else:
                    report["status"] = "passed_bounded_capture"
            report["close"] = client.call({"request": "close"})
            report["exit_code"] = client.finish()
            report["process_shutdown_verified"] = report["exit_code"] == 0
            if report["exit_code"]:
                raise AcceptanceError(f"CLI exit {report['exit_code']}")
        report["temporary_metadata_removed"] = True
    except Exception as cause:
        report["status"] = "failed"
        report["error"] = str(cause)
        if client is not None and client.protocol_synced and client.process.poll() is None and started and not stopped:
            try:
                report["failure_cleanup_stop"] = client.call({"request": "stop_native_audio_program"}, timeout=0.25)
            except Exception as cleanup:
                report["failure_cleanup_error"] = str(cleanup)
        if client is not None:
            if client.protocol_synced and client.process.poll() is None and stopped:
                try:
                    report["failure_cleanup_close"] = client.call({"request": "close"}, timeout=0.5)
                    report["exit_code"] = client.finish()
                    report["process_shutdown_verified"] = report["exit_code"] == 0
                except Exception as cleanup:
                    report["failure_cleanup_error"] = str(cleanup)
            if client.process.poll() is None:
                client.abort()
                report["process_shutdown_verified"] = False
            report["exit_code"] = client.process.returncode
    finally:
        if client is not None:
            report["stderr"] = bytes(client.stderr).decode("utf-8", errors="strict")
            report["reader_problem"] = client.reader_problem
            report["request_count"] = client.requests
    report["checks"] = observed_checks(report)
    return report


def observed_checks(report):
    closed = report.get("close") or report.get("failure_cleanup_close") or {}
    return {
        "output_enumeration": bool(report.get("enumeration", {}).get("devices")),
        "program_start_acknowledged": report.get("start", {}).get("status", {}).get("running") is True,
        "program_blocks_drained": len(report.get("blocks", [])),
        "program_gap_frames_observed": sum(gap["missing_frames"] for output in report.get("blocks", []) for gap in output["block"]["gaps"]),
        "transport_stop_acknowledged": report.get("stop", {}).get("stop_requested") is True and report.get("stop", {}).get("status", {}).get("running") is False,
        "post_stop_blocks": len(report.get("after_stop", {}).get("blocks", [])),
        "close_acknowledged": closed.get("closed") is True,
        "process_exit_zero": report.get("exit_code") == 0 and report.get("process_shutdown_verified") is True,
        "native_packet_acquisition": "observed" if report.get("native_packets_observed", 0) else "inconclusive",
        "native_wasapi_stop_receipt": "not_exposed_by_existing_cli",
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-output-loopback", action="store_true")
    parser.add_argument("--exe", type=Path, default=ROOT / "target/release/skylive-cli.exe")
    parser.add_argument("--json", type=Path, default=ROOT / "evidence/windows-output-loopback-acceptance.json")
    parser.add_argument("--log", type=Path, default=ROOT / "evidence/windows-output-loopback-acceptance.log")
    args = parser.parse_args()
    if not args.run_output_loopback:
        parser.error("native execution disabled; explicit --run-output-loopback is required")
    if sys.platform != "win32":
        parser.error("this acceptance covers Windows output/system loopback only")
    if not args.exe.is_file() or args.exe.stat().st_size > 128 * 1024 * 1024:
        parser.error("CLI executable missing or exceeds128MiB")
    report = run(args.exe)
    # Defense in depth: recursively reject any retained PCM/meter field.
    def no_pcm(value):
        if isinstance(value, dict):
            if any(name in value for name in ("samples", "program_meters", "source_meters", "meters")):
                raise AcceptanceError("unexpected PCM/meter field in evidence")
            for child in value.values(): no_pcm(child)
        elif isinstance(value, list):
            for child in value: no_pcm(child)
    no_pcm(report)
    encoded = json.dumps(report, ensure_ascii=False, indent=2) + "\n"
    if len(encoded.encode("utf-8")) > 1024 * 1024:
        raise AcceptanceError("metadata evidence exceeds1MiB")
    log = ("OUTPUT LOOPBACK ONLY; NO MICROPHONE/PLAYBACK/PCM FILES\n"
           + json.dumps({key: report.get(key) for key in (
               "status", "exe_sha256", "selected_device", "native_packets_observed",
               "native_control_elapsed_seconds", "process_shutdown_verified",
               "native_stop_acknowledgment_verified", "exit_code", "error", "reason")}, ensure_ascii=False, indent=2)
           + "\nSee JSON for exact device metadata/counters and evidence limits.\n")
    for path, text in [(args.json, encoded), (args.log, log)]:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8", newline="\n")
    print(log, end="")
    return 0 if report["status"] == "passed_bounded_capture" else 2


if __name__ == "__main__":
    raise SystemExit(main())
