"""Opt-in native camera probe. Persist metadata only; never invoke a renderer.

Device discovery is the default. --query-modes may open a camera driver briefly.
--capture opens a small actual uncompressed mode for three samples.
--full-hd explicitly requests an actual 1920x1080 mode closest to30fps.
No device calls occur when importing this module or running --self-test.
"""
import argparse
import hashlib
from fractions import Fraction
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading
import time
import unittest

MAX_RESPONSE = 1_048_576
MAX_STDERR = 65_536


class ProbeError(RuntimeError):
    pass


def pick_fields(value, fields):
    return {key: value[key] for key in fields if key in value}


def mode_metadata(mode):
    return pick_fields(mode, ("width", "height", "fps_num", "fps_den", "native_subtype",
                              "mode", "unsupported_reason"))


def frame_metadata(frame):
    # Deliberately excludes image, pixels, binding, buffers, or future payloads.
    return pick_fields(frame, ("source", "sequence", "stream_generation", "received_media_ns",
                               "presentation_ns", "width", "height", "dropped_frames"))


def supported_mode(modes):
    def supported(mode):
        fmt = mode.get("format")
        return fmt in ("rgb32", "bgra8", "rgb24") if isinstance(fmt, str) else isinstance(fmt, dict) and len(fmt) == 1 and next(iter(fmt)) in ("yuy2", "nv12")
    eligible = [row["mode"] for row in modes if isinstance(row.get("mode"), dict)
                and supported(row["mode"])]
    return min(eligible, key=lambda m: (m["width"] * m["height"], m["width"],
                                      m["fps_num"] / m["fps_den"])) if eligible else None


def full_hd_mode(modes):
    """An explicitly requested exact reported Full HD mode; never upscale VGA."""
    eligible = [row["mode"] for row in modes if isinstance(row.get("mode"), dict)
                and (row["mode"]["width"], row["mode"]["height"]) == (1920, 1080)]
    # Match the application's near-30fps recommendation, using exact rational rates.
    return min(eligible, key=lambda mode: (
        abs(Fraction(mode["fps_num"], mode["fps_den"]) - 30),
        -Fraction(mode["fps_num"], mode["fps_den"]))) if eligible else None


class Cli:
    """One persistent CLI, bounded protocol lines and wall deadlines."""
    def __init__(self, argv, timeout, total_timeout):
        self.timeout = timeout
        self.deadline = time.monotonic() + total_timeout
        self.child = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                      stderr=subprocess.PIPE, bufsize=0)
        self.lines = queue.Queue(maxsize=2)
        self.stderr = bytearray()
        self.threads = [threading.Thread(target=self._stdout, daemon=True),
                        threading.Thread(target=self._stderr, daemon=True)]
        for thread in self.threads:
            thread.start()

    def _stdout(self):
        try:
            while True:
                line = self.child.stdout.readline(MAX_RESPONSE + 1)
                if len(line) > MAX_RESPONSE:
                    item = ProbeError("CLI response exceeds 1 MiB metadata limit")
                elif not line:
                    item = ProbeError("CLI ended before the next response")
                else:
                    item = line
                try:
                    self.lines.put_nowait(item)
                except queue.Full:
                    return
                if not isinstance(item, bytes):
                    return
        except (OSError, ValueError) as error:
            try:
                self.lines.put_nowait(ProbeError(str(error)))
            except queue.Full:
                pass

    def _stderr(self):
        try:
            while chunk := self.child.stderr.read(4096):
                self.stderr.extend(chunk)
                if len(self.stderr) > MAX_STDERR:
                    del self.stderr[:-MAX_STDERR]
        except (OSError, ValueError):
            pass

    def send(self, request):
        remaining = min(self.timeout, self.deadline - time.monotonic())
        if remaining <= 0:
            raise ProbeError("probe total deadline exceeded")
        wire = (json.dumps(request, ensure_ascii=False) + "\n").encode("utf-8")
        if len(wire) > 32_768:
            raise ProbeError("probe request exceeds metadata limit")
        try:
            self.child.stdin.write(wire)
            self.child.stdin.flush()
            line = self.lines.get(timeout=remaining)
        except queue.Empty as error:
            raise ProbeError("CLI response deadline exceeded; native shutdown is unacknowledged") from error
        except (BrokenPipeError, OSError) as error:
            raise ProbeError("CLI input closed") from error
        if isinstance(line, Exception):
            raise line
        response = json.loads(line)
        if not response.get("ok"):
            detail = response.get("error", response)
            raise ProbeError(json.dumps(detail, ensure_ascii=False)[:8192])
        return response["result"]

    def finish(self):
        """Bound process cleanup; kill is NOT proof of an SDK stop acknowledgment."""
        try:
            self.child.stdin.close()
        except OSError:
            pass
        forced = False
        try:
            self.child.wait(timeout=2)
        except subprocess.TimeoutExpired:
            forced = True
            self.child.kill()
            self.child.wait(timeout=2)
        for thread in self.threads:
            thread.join(timeout=1)
        self.child.stdout.close()
        self.child.stderr.close()
        return {"exit_code": self.child.returncode, "forced_process_termination": forced,
                "stderr_tail": self.stderr.decode("utf-8", errors="replace")}


def probe(args):
    exe = args.exe.resolve(strict=True)
    if exe.stem != "skylive-cli" or "release" not in exe.parts:
        raise ProbeError("use the built release skylive-cli, never the desktop executable")
    report = {"schema": 1, "scope": "native camera metadata and optional short video stream",
              "exe": str(exe), "exe_sha256": hashlib.sha256(exe.read_bytes()).hexdigest(),
              "device_enumeration_requested": True, "native_modes_requested": args.query_modes,
              "native_video_requested": args.capture, "exact_full_hd_requested": args.full_hd, "audio_requested": False,
              "pixel_persistence": False, "visual_pixel_correctness_verified": False,
              "native_stops_acknowledged": False, "status": "inconclusive"}
    cli = Cli([str(exe), "--stdio"], args.timeout, args.total_timeout)
    added = False
    stopped = False
    try:
        found = cli.send({"request": "list_camera_devices"})
        devices = found.get("devices", [])
        if not isinstance(devices, list) or len(devices) > 64:
            raise ProbeError("device catalog exceeds the expected bounded profile")
        report["backend"] = pick_fields(found.get("backend", {}),
                                         ("available", "backend", "video", "audio", "note"))
        report["devices"] = [pick_fields(d, ("target", "name")) for d in devices]
        if not devices:
            report["reason"] = "No native camera device was enumerated"
        elif args.query_modes:
            if args.device_index >= len(devices):
                raise ProbeError("device index is outside the actual enumeration")
            target = devices[args.device_index]["target"]
            modes = cli.send({"request": "list_camera_modes", "target": target}).get("modes", [])
            if not isinstance(modes, list) or len(modes) > 1024:
                raise ProbeError("mode catalog exceeds the expected bounded profile")
            report["selected_device_index"] = args.device_index
            report["modes"] = [mode_metadata(m) for m in modes]
            mode = full_hd_mode(modes) if args.full_hd else supported_mode(modes)
            report["supported_native_uncompressed_modes"] = sum(isinstance(m.get("mode"), dict) for m in modes)
            if args.capture and mode is not None:
                report["selected_mode"] = mode
                cli.send({"request": "command", "command": {"op": "add_video_capture_source",
                         "scene": 1, "name": "Native camera acceptance", "binding": {
                             "target": target, "mode": mode, "deactivate_when_hidden": False}}})
                added = True
                samples = []
                interval = max(.15, 1.2 * mode["fps_den"] / mode["fps_num"])
                for index in range(3):
                    if index:
                        time.sleep(interval)
                    captured = cli.send({"request": "capture_frame", "time_ns": int(index * interval * 1_000_000_000)})
                    camera = captured.get("camera_capture", {})
                    frames = camera.get("frames", [])
                    if len(frames) != 1 or camera.get("problems"):
                        raise ProbeError("capture did not publish exactly one healthy camera frame")
                    frame = frame_metadata(frames[0])
                    if (frame.get("width"), frame.get("height")) != (mode["width"], mode["height"]):
                        raise ProbeError("native frame dimensions differ from the exact selected mode")
                    int(frame["presentation_ns"])
                    samples.append(frame)
                report["frames"] = samples
                if len({row["sequence"] for row in samples}) < 2:
                    raise ProbeError("camera frame sequence did not progress during the short stream")
                if len({row["stream_generation"] for row in samples}) != 1:
                    raise ProbeError("native stream was reopened during the short persistent capture")
                if len({int(row["presentation_ns"]) for row in samples}) < 2:
                    raise ProbeError("native frame presentation timestamps did not progress")
                report["native_video_metadata_verified"] = True
                report["status"] = "passed_supported_native_uncompressed_metadata"
            elif args.capture:
                report["reason"] = ("No actual supported 1920x1080 mode was reported" if args.full_hd else "No supported native uncompressed RGB/YUY2/NV12 mode; unsupported native color metadata or compressed formats require additional support")
            else:
                report["status"] = "passed_device_and_mode_enumeration"
        else:
            report["status"] = "passed_device_enumeration_only"
        if added:
            ack = cli.send({"request": "stop_camera_capture"})
            stopped = bool(ack.get("native_stops_acknowledged"))
            if not stopped:
                raise ProbeError("native camera stop was not acknowledged")
            cli.send({"request": "command", "command": {"op": "remove_item", "scene": 1, "item": 3}})
            with tempfile.TemporaryDirectory(prefix="skylive-camera-metadata-") as folder:
                clean = Path(folder) / "empty-studio.json"
                cli.send({"request": "save", "path": str(clean)})
                # Native storage must contain no frames or source binding after removal.
                saved = json.loads(clean.read_text(encoding="utf-8"))
                report["clean_metadata_save"] = not saved["collection"]["sources"]
                if not report["clean_metadata_save"]:
                    raise ProbeError("camera source remained in the clean metadata save")
        cli.send({"request": "close"})
        report["cli_close_acknowledged"] = True
    except Exception as error:
        report["status"] = "failed"
        report["error"] = str(error)[:8192]
        # A fatal CLI error may already have disconnected the protocol. Never
        # claim native quiescence from process termination or invent a stop ack.
        if added and not stopped and cli.child.poll() is None:
            try:
                stopped = bool(cli.send({"request": "stop_camera_capture"}).get("native_stops_acknowledged"))
            except Exception as cleanup_error:
                report["stop_error"] = str(cleanup_error)[:8192]
    finally:
        report["native_stops_acknowledged"] = stopped
        report["process"] = cli.finish()
        if report["process"]["forced_process_termination"]:
            report["status"] = "failed"
    return report


class SelfTests(unittest.TestCase):
    def test_metadata_allowlist_excludes_pixels(self):
        self.assertEqual(frame_metadata({"sequence": 1, "pixels": [255], "image": "private", "binding": {}}), {"sequence": 1})

    def test_smallest_actual_supported_mode_only(self):
        rows = [{"mode": None}, {"mode": {"width": 640, "height": 480, "fps_num": 30, "fps_den": 1, "format": "rgb32"}},
                {"mode": {"width": 320, "height": 240, "fps_num": 30, "fps_den": 1, "format": "bgra8"}}]
        self.assertEqual(supported_mode(rows)["width"], 320)
        self.assertIsNone(supported_mode([{"native_subtype": "NV12", "mode": None}]))

    def test_explicit_fullhd_requires_a_real_mode_and_exact_rational_rate(self):
        def mode(width, height, num, den=1):
            return {"mode": {"width":width, "height":height, "fps_num":num, "fps_den":den, "format":"rgb32"}}
        rows=[mode(640,480,120),mode(1920,1080,5),mode(1920,1080,30000,1001),mode(1920,1080,30),mode(3840,2160,30)]
        self.assertEqual(full_hd_mode(rows),rows[3]["mode"])
        self.assertIsNone(full_hd_mode([rows[0],rows[4],{"mode":None}]))
        self.assertEqual(full_hd_mode([rows[1]])["fps_num"],5)

    def test_child_response_timeout_is_bounded_without_native_sdk(self):
        child = Cli([sys.executable, "-u", "-c", "import sys,time;sys.stdin.readline();time.sleep(10)"], .05, .1)
        with self.assertRaises(ProbeError):
            child.send({"request": "fake"})
        child.child.kill()
        self.assertFalse(child.finish()["forced_process_termination"])

    def test_oversized_protocol_response_rejected(self):
        code = "import sys;sys.stdin.readline();sys.stdout.write('x'*1048578+'\\n');sys.stdout.flush()"
        child = Cli([sys.executable, "-u", "-c", code], 2, 3)
        try:
            with self.assertRaisesRegex(ProbeError, "1 MiB"):
                child.send({"request": "fake"})
        finally:
            child.finish()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", type=Path)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--query-modes", action="store_true")
    parser.add_argument("--capture", action="store_true")
    parser.add_argument("--full-hd", action="store_true", help="Select an actual reported 1920x1080 mode closest to 30 fps")
    parser.add_argument("--device-index", type=int, default=0)
    parser.add_argument("--timeout", type=float, default=15)
    parser.add_argument("--total-timeout", type=float, default=90)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        unittest.main(argv=[sys.argv[0]])
        return
    if args.exe is None or args.out is None:
        parser.error("--exe and --out are required")
    if args.full_hd and not args.capture:
        parser.error("--full-hd requires explicit --capture")
    if args.capture and not args.query_modes:
        parser.error("--capture also requires explicit --query-modes")
    if args.device_index < 0 or not 0 < args.timeout <= 30 or not args.timeout <= args.total_timeout <= 180:
        parser.error("device index must be nonnegative; timeout 0..30s; total timeout timeout..180s")
    report = probe(args)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    # Unique same-directory temporary, atomic final metadata publication.
    descriptor, temporary = tempfile.mkstemp(prefix=args.out.name + ".", suffix=".tmp", dir=args.out.parent)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as output:
            json.dump(report, output, ensure_ascii=False, indent=2)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, args.out)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
    print(json.dumps({"status": report["status"], "report": str(args.out.resolve())}, ensure_ascii=False))
    return 1 if report["status"] == "failed" else 0


if __name__ == "__main__":
    raise SystemExit(main())
