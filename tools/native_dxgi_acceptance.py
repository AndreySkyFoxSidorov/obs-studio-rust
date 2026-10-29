#!/usr/bin/env python3
"""Opt-in real Windows DXGI desktop acquisition via existing CLI metadata only.

Never saves pixels, screenshots, audio or recordings, and never enumerates audio
or cameras. The only temporary native JSON contains authored display metadata.
Run this manually only when desktop capture has been explicitly authorized.
"""
from __future__ import annotations
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import tempfile
import time
from native_audio_acceptance import Cli, AcceptanceError, ROOT


def frame_metadata(result):
    """Persist an explicit metadata allowlist, never future raw frame fields."""
    capture = result["capture"]
    return {"requested": result.get("requested"), "capture": {
        "epoch": capture["epoch"], "generation": capture["generation"],
        "active_sources": capture["active_sources"], "problems": capture["problems"],
        "frames": [{key: frame[key] for key in ("source", "binding", "display", "sequence", "requested_at_ns", "width", "height")}
                   for frame in capture["frames"]]}}


def run(executable: Path, method="dxgi"):
    report = {"schema": 1, "status": "failed", "generated_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
              "scope": "real Windows display acquisition; metadata only; no pixel persistence", "requested_method": method,
              "exe": str(executable.resolve()), "exe_sha256": hashlib.sha256(executable.read_bytes()).hexdigest(), "observations": []}
    client = None
    saved_path = None
    source_item = None
    scene = None
    media_ns = 0
    try:
        if os.name != "nt":
            raise AcceptanceError("This opt-in acceptance is Windows-only")
        with tempfile.TemporaryDirectory(prefix="skylive-dxgi-acceptance-") as temporary:
            saved_path = str(Path(temporary) / "metadata.skylive.json")
            client = Cli(executable)
            report["capabilities"] = client.call({"request": "capabilities"})
            report["enumeration"] = client.call({"request": "list_displays"})
            displays = report["enumeration"].get("displays", [])
            windows = [display for display in displays if display.get("target", {}).get("platform") == "windows"]
            if not windows:
                raise AcceptanceError("No available Windows display target")
            selected = next((display for display in windows if display.get("primary")), windows[0])
            report["selected_display"] = selected
            snapshot = client.call({"request": "snapshot"})
            scene = snapshot["preview"]
            report["add_source"] = client.call({"request": "command", "command": {
                "op": "add_display_capture_source", "scene": scene, "name": "DXGI acceptance display",
                "binding": {"target": selected["target"], "capture_cursor": False,
                            "width": selected["width"], "height": selected["height"], "method": method}}})
            snapshot = client.call({"request": "snapshot"})
            # Source/item identities are returned by the existing public read model.
            model = snapshot.get("collection", snapshot)
            source_item = next(scene_row["items"][-1]["id"] for scene_row in model["scenes"] if scene_row["id"] == scene)
            deadline = time.monotonic() + 2.0
            latest = None
            for attempt in range(20):
                media_ns = attempt * 40_000_000
                try:
                    captured = frame_metadata(client.call({"request": "capture_frame", "time_ns": media_ns}))
                    report["observations"].append(captured)
                    frames = captured["capture"]["frames"]
                    if frames:
                        latest = frames[0]
                        backend = latest.get("display", {}).get("backend")
                        if method == "dxgi" and backend != "dxgi":
                            raise AcceptanceError("Explicit DXGI did not report actual DXGI acquisition")
                        if method == "auto" and (backend not in {"gdi", "dxgi"} or (backend == "gdi" and not latest["display"].get("fallback_reason"))):
                            raise AcceptanceError("Auto must identify its actual backend and explain a GDI fallback")
                        if latest["width"] != selected["width"] or latest["height"] != selected["height"]:
                            raise AcceptanceError("Actual frame dimensions disagree with selected display")
                        if len(report["observations"]) >= 4:
                            break
                except AcceptanceError as error:
                    detail = str(error)
                    report["observations"].append({"error": detail})
                    if "capture_frame_unavailable" not in detail:
                        raise
                if time.monotonic() >= deadline:
                    break
                time.sleep(0.04)
            if latest is None:
                report["status"] = "inconclusive_no_native_frame"
                report["reason"] = "The bounded interval produced no admitted desktop image. No acquisition success is claimed."
            else:
                report["status"] = "passed_bounded_" + method + "_capture"
                report["actual_frame"] = latest
            client.call({"request": "command", "command": {"op": "remove_item", "scene": scene, "item": source_item}})
            media_ns += 80_000_000
            report["retirement"] = frame_metadata(client.call({"request": "capture_frame", "time_ns": media_ns}))
            retired = report["retirement"]["capture"]
            if retired["active_sources"] != 0 or retired["frames"]:
                raise AcceptanceError("Removed display binding was not retired before close")
            report["save_metadata"] = client.call({"request": "save", "path": saved_path})
            report["close"] = client.call({"request": "close"})
            report["exit_code"] = client.finish()
            if report["exit_code"] != 0:
                raise AcceptanceError("CLI did not exit successfully")
        report["temporary_metadata_removed"] = True
    except Exception as error:
        report["status"] = "failed"
        report["error"] = str(error)
        if client is not None:
            client.abort()
            report["exit_code"] = client.process.returncode
    finally:
        if client is not None:
            report["stderr"] = bytes(client.stderr).decode("utf-8", errors="strict")
            report["reader_problem"] = client.reader_problem
            report["request_count"] = client.requests
        retired = report.get("retirement", {}).get("capture", {})
        report["native_release_receipt"] = ("worker retention completion observed; individual COM Release calls are not exposed by CLI"
                                             if retired.get("active_sources") == 0 and retired.get("frames") == [] else "worker retention completion not observed")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-desktop-capture", action="store_true", help="Explicit opt-in: actually acquire this Windows desktop")
    parser.add_argument("--exe", type=Path, default=ROOT / "target/debug/skylive-cli.exe")
    parser.add_argument("--method", choices=["dxgi", "auto"], default="dxgi")
    parser.add_argument("--evidence", type=Path)
    args = parser.parse_args()
    if not args.run_desktop_capture:
        parser.error("No native operation performed. Pass --run-desktop-capture only with explicit authorization.")
    report = run(args.exe, args.method)
    if args.evidence is None:
        args.evidence = ROOT / ("evidence/windows-" + args.method + "-acceptance.json")
    args.evidence.parent.mkdir(parents=True, exist_ok=True)
    args.evidence.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"status": report["status"], "evidence": str(args.evidence), "error": report.get("error")}, ensure_ascii=False))
    raise SystemExit(0 if report["status"] == "passed_bounded_" + args.method + "_capture" else 1)


if __name__ == "__main__":
    main()
