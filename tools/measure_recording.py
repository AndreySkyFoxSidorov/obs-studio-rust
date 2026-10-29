"""Bounded synthetic 1080p30 Program -> native MP4 throughput measurement.

No device, media-file, audio, GUI, decoder, network or build requests. Runs only
the explicit release CLI binary. An offline benchmark is not live 30fps proof.
"""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import queue
import statistics
import struct
import subprocess
import sys
import threading
import time
import traceback

from measure_headless import peak_working_set

ROOT = Path(__file__).resolve().parents[1]
MAX_REPLY_BYTES = 1024 * 1024
MAX_STDERR_BYTES = 1024 * 1024
MAX_MANIFEST_BYTES = 8 * 1024 * 1024
MAX_OUTPUT_BYTES = 256 * 1024 * 1024
MAX_MOOV_BYTES = 8 * 1024 * 1024
MAX_POLLS_PER_FRAME = 4000
WIDTH, HEIGHT, FPS = 1920, 1080, 30
COLOR = [220, 35, 70, 255]
CC_COLOR = [255, 0, 0, 255]
CC = {
    "kind": "color_correction", "gamma": 0.0, "contrast": 0.0,
    "brightness": 0.0, "saturation": 0.0, "hue_shift": 120.0,
    "opacity": 1.0, "color_multiply": [255, 255, 255], "color_add": [0, 0, 0],
}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def bounded_json(path: Path) -> tuple[bytes, dict]:
    with path.open("rb") as source:
        data = source.read(MAX_MANIFEST_BYTES + 1)
    if len(data) > MAX_MANIFEST_BYTES:
        raise ValueError(f"metadata exceeds {MAX_MANIFEST_BYTES} bytes: {path}")
    value = json.loads(data)
    if not isinstance(value, dict):
        raise ValueError(f"metadata is not an object: {path}")
    return data, value


def provenance(exe: Path, manifest_path: Path, receipt_path: Path) -> tuple[dict, bytes, bytes]:
    manifest_bytes, manifest = bounded_json(manifest_path)
    receipt_bytes, receipt = bounded_json(receipt_path)
    digest = manifest.get("digest")
    if not isinstance(digest, str) or len(digest) != 64 or any(c not in "0123456789abcdef" for c in digest):
        raise ValueError("source manifest requires its exact 64-character lowercase digest")
    if receipt.get("source_manifest_digest") != digest or receipt.get("source_unchanged_during_build") is not True:
        raise ValueError("build receipt does not bind an unchanged build to this source manifest")
    exe_hash, lock_hash = sha256(exe), sha256(ROOT / "Cargo.lock")
    artifacts = receipt.get("artifacts", [])
    if not isinstance(artifacts, list) or len(artifacts) > 10000:
        raise ValueError("invalid build artifact list")
    for path, expected in [(exe, exe_hash), (ROOT / "Cargo.lock", lock_hash)]:
        matches = [row for row in artifacts if isinstance(row, dict)
                   and isinstance(row.get("path"), str)
                   and Path(row["path"]).resolve() == path]
        if len(matches) != 1 or matches[0].get("sha256") != expected:
            raise ValueError(f"exact file/hash is not bound by build receipt: {path}")
    return {
        "executable": str(exe), "exe_sha256": exe_hash,
        "cargo_lock": str(ROOT / "Cargo.lock"), "cargo_lock_sha256": lock_hash,
        "source_manifest": str(manifest_path), "source_manifest_digest": digest,
        "source_manifest_file_sha256": hashlib.sha256(manifest_bytes).hexdigest(),
        "build_receipt": str(receipt_path),
        "build_receipt_file_sha256": hashlib.sha256(receipt_bytes).hexdigest(),
        "measurement_tool_sha256": sha256(Path(__file__).resolve()),
        "scope": "Supplied local build ledger binds executable/lock/source digest; not toolchain attestation or a claim that the current working tree is unchanged.",
    }, manifest_bytes, receipt_bytes


class Protocol:
    """One request in flight; bounded pipe staging, main-thread deadlines."""
    def __init__(self, command: list[str], deadline: float, reply_timeout: float):
        self.deadline, self.reply_timeout = deadline, reply_timeout
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE, cwd=ROOT)
        self.replies: queue.Queue[bytes | None] = queue.Queue(maxsize=2)
        self.failure: queue.Queue[str] = queue.Queue(maxsize=1)
        self.stderr = bytearray()
        self.peak: int | None = None
        self.requests = 0
        self._reader = threading.Thread(target=self._read_replies, daemon=True)
        self._errors = threading.Thread(target=self._read_errors, daemon=True)
        self._reader.start()
        self._errors.start()

    def _fail(self, text: str) -> None:
        try:
            self.failure.put_nowait(text)
        except queue.Full:
            pass

    def _read_replies(self) -> None:
        assert self.process.stdout is not None
        try:
            while True:
                line = self.process.stdout.readline(MAX_REPLY_BYTES + 1)
                if len(line) > MAX_REPLY_BYTES:
                    self._fail("CLI reply exceeds bounded 1 MiB staging")
                    return
                if line and not line.endswith(b"\n"):
                    self._fail("CLI ended with an incomplete JSON reply")
                    return
                try:
                    self.replies.put_nowait(line if line else None)
                except queue.Full:
                    self._fail("unsolicited CLI output exceeded the one-request reply window")
                    return
                if not line:
                    return
        except (OSError, ValueError) as error:
            self._fail(f"CLI stdout reader: {error}")

    def _read_errors(self) -> None:
        assert self.process.stderr is not None
        try:
            while True:
                block = self.process.stderr.read(4096)
                if not block:
                    return
                if len(self.stderr) + len(block) > MAX_STDERR_BYTES:
                    self._fail("CLI stderr exceeds bounded 1 MiB staging; measurement rejected")
                    return
                self.stderr.extend(block)
        except (OSError, ValueError) as error:
            self._fail(f"CLI stderr reader: {error}")

    def _check(self) -> None:
        try:
            error = self.failure.get_nowait()
        except queue.Empty:
            return
        raise RuntimeError(error)

    def sample_memory(self) -> None:
        observed = peak_working_set(self.process.pid)
        if observed is not None:
            self.peak = max(self.peak or 0, observed)

    def request(self, value: dict) -> tuple[dict, float]:
        self._check()
        until = min(self.deadline, time.perf_counter() + self.reply_timeout)
        if until <= time.perf_counter():
            raise TimeoutError("measurement deadline reached before CLI request")
        payload = (json.dumps(value, separators=(",", ":")) + "\n").encode("utf-8")
        if len(payload) > 2048:
            raise ValueError("measurement request exceeds 2 KiB (use a shorter output path)")
        # Requests are tiny, one-at-a-time and written only after the preceding
        # reply. The CLI input pipe never accumulates a workload queue.
        assert self.process.stdin is not None
        started = time.perf_counter()
        self.process.stdin.write(payload)
        self.process.stdin.flush()
        self.requests += 1
        while True:
            self._check()
            remaining = until - time.perf_counter()
            if remaining <= 0:
                raise TimeoutError(f"CLI reply deadline expired for {value['request']}")
            try:
                line = self.replies.get(timeout=min(remaining, 0.05))
                break
            except queue.Empty:
                self.sample_memory()
        if line is None:
            raise RuntimeError(f"CLI ended before reply for {value['request']}")
        reply = json.loads(line)
        if not isinstance(reply, dict) or reply.get("ok") is not True or not isinstance(reply.get("result"), dict):
            raise RuntimeError(f"CLI rejected request {value}: {reply}")
        elapsed = (time.perf_counter() - started) * 1000.0
        self.sample_memory()
        return reply, elapsed

    def finish(self) -> int:
        if self.process.stdin is not None:
            self.process.stdin.close()
        remaining = max(0.001, min(15.0, self.deadline - time.perf_counter()))
        code = self.process.wait(timeout=remaining)
        self._reader.join(timeout=1)
        self._errors.join(timeout=1)
        self._check()
        if self._reader.is_alive() or self._errors.is_alive():
            raise TimeoutError("CLI pipe readers did not settle after process exit")
        return code

    def abort(self) -> None:
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait(timeout=5)
        self._reader.join(timeout=1)
        self._errors.join(timeout=1)
        for pipe in (self.process.stdin, self.process.stdout, self.process.stderr):
            if pipe is not None:
                try:
                    pipe.close()
                except (OSError, ValueError):
                    pass


def boxes(data: bytes, start: int = 0, end: int | None = None):
    end = len(data) if end is None else end
    position, count = start, 0
    while position < end:
        count += 1
        if count > 10000 or end - position < 8:
            raise ValueError("invalid or excessive MP4 metadata box list")
        size, kind = struct.unpack_from(">I4s", data, position)
        header = 8
        if size == 1:
            if end - position < 16:
                raise ValueError("truncated MP4 extended box")
            size = struct.unpack_from(">Q", data, position + 8)[0]
            header = 16
        elif size == 0:
            size = end - position
        if size < header or size > end - position:
            raise ValueError("MP4 metadata box escapes its parent")
        yield kind, position + header, position + size
        position += size


def duration_header(data: bytes, start: int, end: int) -> dict:
    if end - start < 4:
        raise ValueError("truncated MP4 duration header")
    version = data[start]
    if version == 0 and end - start >= 20:
        scale, duration = struct.unpack_from(">II", data, start + 12)
    elif version == 1 and end - start >= 32:
        scale, duration = struct.unpack_from(">IQ", data, start + 20)
    else:
        raise ValueError("unsupported or truncated MP4 duration version")
    if scale == 0:
        raise ValueError("MP4 timescale is zero")
    return {"timescale": scale, "duration_ticks": duration, "duration_seconds": duration / scale}


def parse_moov(data: bytes) -> dict:
    tracks = []
    movie = None
    for kind, start, end in boxes(data):
        if kind == b"mvhd":
            movie = duration_header(data, start, end)
        if kind != b"trak":
            continue
        for child, child_start, child_end in boxes(data, start, end):
            if child != b"mdia":
                continue
            track = {}
            for field, field_start, field_end in boxes(data, child_start, child_end):
                if field == b"hdlr":
                    if field_end - field_start < 12:
                        raise ValueError("truncated MP4 handler")
                    track["handler"] = data[field_start + 8:field_start + 12].decode("ascii")
                elif field == b"mdhd":
                    track.update(duration_header(data, field_start, field_end))
                elif field == b"minf":
                    for table, table_start, table_end in boxes(data, field_start, field_end):
                        if table != b"stbl":
                            continue
                        for leaf, leaf_start, leaf_end in boxes(data, table_start, table_end):
                            if leaf == b"stsz":
                                if leaf_end - leaf_start < 12:
                                    raise ValueError("truncated MP4 sample size table")
                                sample_size, sample_count = struct.unpack_from(">II", data, leaf_start + 4)
                                expected = 12 if sample_size else 12 + 4 * sample_count
                                if sample_count > 10000 or expected != leaf_end - leaf_start:
                                    raise ValueError("invalid MP4 sample size entry count")
                                track["sample_count"] = sample_count
                            elif leaf == b"stts":
                                if leaf_end - leaf_start < 8:
                                    raise ValueError("truncated MP4 time-to-sample table")
                                count = struct.unpack_from(">I", data, leaf_start + 4)[0]
                                if count > 10000 or 8 + count * 8 != leaf_end - leaf_start:
                                    raise ValueError("invalid MP4 time-to-sample entry count")
                                entries = [struct.unpack_from(">II", data, leaf_start + 8 + i * 8) for i in range(count)]
                                track["timed_sample_count"] = sum(n for n, _ in entries)
                                track["sample_table_duration_ticks"] = sum(n * delta for n, delta in entries)
            tracks.append(track)
    videos = [track for track in tracks if track.get("handler") == "vide"]
    if len(videos) != 1 or any(track.get("handler") == "soun" for track in tracks):
        raise ValueError("expected exactly one video track and no audio tracks")
    video = videos[0]
    for field in ("timescale", "duration_seconds", "sample_count", "timed_sample_count", "sample_table_duration_ticks"):
        if field not in video:
            raise ValueError(f"flat MP4 lacks required {field}")
    video["sample_table_duration_seconds"] = video["sample_table_duration_ticks"] / video["timescale"]
    return {"movie": movie, "video": video, "audio_track_count": 0,
            "scope": "Bounded flat MP4 header/sample-table metadata; not bitstream decoding or independent RGB verification."}


def inspect_mp4(path: Path) -> dict:
    length = path.stat().st_size
    if not 16 < length <= MAX_OUTPUT_BYTES:
        raise ValueError("final MP4 lies outside 16-byte..256-MiB measurement bounds")
    with path.open("rb") as source:
        position, count, moov = 0, 0, None
        while position < length:
            count += 1
            if count > 10000 or length - position < 8:
                raise ValueError("invalid MP4 top-level boxes")
            source.seek(position)
            size, kind = struct.unpack(">I4s", source.read(8))
            header = 8
            if size == 1:
                if length - position < 16:
                    raise ValueError("truncated MP4 extended header")
                size = struct.unpack(">Q", source.read(8))[0]
                header = 16
            elif size == 0:
                size = length - position
            if size < header or size > length - position:
                raise ValueError("MP4 top-level box escapes the file")
            if kind == b"moov":
                if moov is not None or size - header > MAX_MOOV_BYTES:
                    raise ValueError("multiple or oversized MP4 movie metadata")
                moov = source.read(size - header)
                if len(moov) != size - header:
                    raise ValueError("MP4 changed while reading movie metadata")
            position += size
    if moov is None:
        raise ValueError("MP4 has no final movie metadata (fragmented MP4 is outside this measurement)")
    return parse_moov(moov)


def summary(values: list[float]) -> dict:
    ordered = sorted(values)
    return {"count": len(values), "min_ms": ordered[0], "median_ms": statistics.median(ordered),
            "p95_nearest_rank_ms": ordered[max(0, math.ceil(len(ordered) * .95) - 1)],
            "max_ms": ordered[-1], "sum_ms": sum(values)}


def frame_time_ns(index: int) -> int:
    # Owner assigns floor(time * FPS / 1e9); do not repeat the preceding slot.
    return (index * 1_000_000_000 + FPS - 1) // FPS


def moving_rect(index: int) -> dict:
    return {"x": 2 * ((7 * index) % 600), "y": 2 * ((5 * index) % 300),
            "width": 640, "height": 360, "opacity": 1.0,
            "rotation_degrees": 0.0, "flip_x": False, "flip_y": False,
            "crop": {"left": 0, "top": 0, "right": 0, "bottom": 0}}


def cache_pattern(status: dict, completed: int, dynamic: bool) -> dict:
    names = ("prepared_video_cache_hits", "prepared_video_cache_misses")
    present = [name in status for name in names]
    expected = {"hits": 0 if dynamic else completed - 1,
                "misses": completed if dynamic else 1}
    if not any(present):
        return {"availability": "unavailable", "scope": "This frozen CLI predates the counters; absent is not zero.",
                "expected_if_supported": expected, "validated": False}
    if not all(present):
        raise ValueError("incomplete prepared-frame cache counters")
    actual = {"hits": status[names[0]], "misses": status[names[1]]}
    if any(not isinstance(value, int) or isinstance(value, bool) or value < 0 for value in actual.values()) or actual != expected:
        raise ValueError(f"prepared-frame cache path differs: actual={actual}, expected={expected}")
    return {"availability": "available", "actual": actual, "expected": expected, "validated": True}


def benchmark(args) -> tuple[dict, int]:
    exe = args.exe.resolve(strict=True)
    if exe.stem != "skylive-cli" or "release" not in exe.parts:
        raise ValueError("pass the built release skylive-cli; desktop binaries are not accepted")
    metadata, manifest, receipt = provenance(exe, args.source_manifest.resolve(strict=True), args.build_receipt.resolve(strict=True))
    folder = args.out.resolve()
    folder.mkdir(exist_ok=False)
    document, output = folder / "synthetic.skylive.json", folder / "program.mp4"
    final_document = folder / "synthetic-final.skylive.json" if args.dynamic_rect else document
    for name, data in [("source-manifest.json", manifest), ("build-receipt.json", receipt)]:
        with (folder / name).open("xb") as file:
            file.write(data)
    report = {"schema": 1, "status": "failed", "started_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
              "scope": "Unpaced one-inflight synthetic CLI Program composition -> native video-only MP4. No OS input, media file, audio, GUI, network, decoder, or live capture acceptance.",
              "environment": {"platform": platform.platform(), "machine": platform.machine(), "python": platform.python_version()},
              "provenance": metadata,
              "workload": {"width": WIDTH, "height": HEIGHT, "fps_num": FPS, "fps_den": 1,
                           "frames": args.frames, "layers": args.layers, "opaque_full_frame_rgba": CC_COLOR if args.cc else COLOR,
                           "top_source_color_correction": CC if args.cc else None,
                           "source_pattern": "All layers are full-frame opaque identical solid colors; top-layer correction is static and may be cached after first evaluation.",
                           "independent_rgb_oracle": {"expected_full_frame_rgb": [0, 0, 255] if args.cc else COLOR[:3],
                                                      "cc_basis": "OBS ColorCorrection hue +120 maps saturated source red to blue in the supported profile; native lossy decoder tolerance must be specified by separate verification.",
                                                      "executed": False},
                           "audio": False, "hardware_preference": True, "hardware_selection_established": False,
                           "no_queue_hiding": "Every record_frame waits for exact cumulative video_frames before admitting the next."},
              "limits": {"frames": [1, 90], "layers": [1, 8], "reply_bytes": MAX_REPLY_BYTES,
                         "stderr_bytes": MAX_STDERR_BYTES, "output_bytes": MAX_OUTPUT_BYTES,
                         "total_timeout_seconds": args.timeout, "reply_timeout_seconds": args.reply_timeout,
                         "poll_interval_seconds": .005, "max_status_polls_per_frame": MAX_POLLS_PER_FRAME},
              "receipts": [], "frames": []}
    if args.dynamic_rect:
        report["workload"].update(
            mode="moving_rectangle", opaque_full_frame_rgba=None,
            source_pattern="Opaque green background layers plus one red (or CC-blue) 640x360 rectangle; every frame commits a distinct even-pixel transform from an initially offscreen position.",
            intentional_authored_edits=args.frames,
            independent_rgb_oracle={"background_rgb": [0, 255, 0], "rectangle_rgb": [0, 0, 255] if args.cc else [255, 0, 0],
                                    "positions": [moving_rect(index) for index in range(args.frames)],
                                    "sample_inset_pixels": 8,
                                    "scope": "Separate decoder must verify each frame's rectangle interior/background away from chroma/codec edges, with declared lossy tolerance.", "executed": False})
    else:
        report["workload"]["mode"] = "static_solid"
    child = None
    started = time.perf_counter()
    try:
        child = Protocol([str(exe), "--stdio"], started + args.timeout, args.reply_timeout)

        def request(value, receipt_name=None):
            reply, elapsed = child.request(value)
            if receipt_name:
                report["receipts"].append({"name": receipt_name, "request": value, "reply": reply, "roundtrip_ms": elapsed})
            return reply["result"], elapsed

        initial, _ = request({"request": "snapshot"}, "initial")
        scene = initial["collection"]["scenes"][0]["id"]
        profile = initial["profiles"]["selected"]
        request({"request": "command", "command": {"op": "set_video", "width": WIDTH, "height": HEIGHT, "fps_num": FPS, "fps_den": 1}}, "video_profile")
        request({"request": "command", "command": {"op": "profile", "command": {"op": "set_recording", "id": profile,
                 "settings": {"directory": str(folder), "video_bitrate_kbps": 6000, "audio_bitrate_kbps": 160, "audio_enabled": False, "prefer_hardware": True}}}}, "recording_profile")
        for index in range(args.layers):
            color = ([255, 0, 0, 255] if index == args.layers - 1 else [0, 255, 0, 255]) if args.dynamic_rect else (CC_COLOR if args.cc else COLOR)
            request({"request": "command", "command": {"op": "add_color_source", "scene": scene, "name": f"Synthetic layer {index + 1}", "rgba": color}}, f"layer_{index + 1}")
        state, _ = request({"request": "snapshot"})
        top_item = state["collection"]["scenes"][0]["items"][-1]["id"]
        if args.dynamic_rect:
            initial_rect = moving_rect(0)
            initial_rect["x"] = -640
            request({"request": "command", "command": {"op": "set_transform", "scene": scene,
                     "item": top_item, "transform": initial_rect}}, "initial_offscreen_rectangle")
        if args.cc:
            top_source = state["collection"]["scenes"][0]["items"][-1]["source"]
            request({"request": "command", "command": {"op": "set_source_video_filters", "source": top_source,
                     "filters": [{"enabled": True, "effect": CC}]}}, "real_color_correction")
        request({"request": "save", "path": str(document)}, "save_clean_synthetic_document")
        saved, _ = request({"request": "snapshot"}, "saved_snapshot")
        authored_revision = saved["collection"]["revision"]
        expected_revision = authored_revision
        start, start_ms = request({"request": "start_recording", "path": str(output), "time_ns": 0, "audio": False}, "start")
        if start.get("event") != "Started" or not start["status"]["active"]:
            raise RuntimeError(f"missing actual recording Start acknowledgement: {start}")
        pipeline_started = time.perf_counter()
        for index in range(args.frames):
            begin = time.perf_counter()
            edit, edit_ms = None, 0.0
            if args.dynamic_rect:
                edit, edit_ms = request({"request": "command", "expected_revision": expected_revision,
                                       "command": {"op": "set_transform", "scene": scene, "item": top_item,
                                                   "transform": moving_rect(index)}})
                if edit.get("changed") is not True or edit.get("revision") != expected_revision + 1:
                    raise RuntimeError(f"dynamic frame {index} did not publish one real visual edit: {edit}")
                expected_revision = edit["revision"]
            stamp = frame_time_ns(index)
            admission, admission_ms = request({"request": "record_frame", "time_ns": stamp})
            admitted_at = time.perf_counter()
            frame = {"index": index, "media_time_ns": stamp, "admission_roundtrip_ms": admission_ms,
                     "admission_reply": admission, "status_poll_roundtrips_ms": []}
            if args.dynamic_rect:
                frame.update(edit_reply=edit, edit_roundtrip_ms=edit_ms, expected_transform=moving_rect(index))
            report["frames"].append(frame)
            if admission.get("accepted") is not True:
                raise RuntimeError(f"frame {index} was not admitted: {admission}")
            status = admission["status"]
            polls = 0
            while int(status["video_frames"]) < index + 1:
                if polls >= MAX_POLLS_PER_FRAME:
                    raise TimeoutError(f"frame {index} exceeded bounded status polling")
                if status.get("problem") or not status.get("active"):
                    raise RuntimeError(f"frame {index} failed before completion: {status}")
                time.sleep(min(.005, max(0.0, child.deadline - time.perf_counter())))
                response, poll_ms = request({"request": "recording_status"})
                frame["status_poll_roundtrips_ms"].append(poll_ms)
                status, polls = response["status"], polls + 1
            if int(status["video_frames"]) != index + 1 or int(status["audio_frames"]) != 0:
                raise RuntimeError(f"unexpected cumulative frame count: {status}")
            if status.get("problem") or int(status.get("skipped_video_frames", 0)) or int(status.get("degraded_video_frames", 0)):
                raise RuntimeError(f"frame {index} completed with failed, skipped or degraded video: {status}")
            frame.update(status_polls=polls, completion_status=status,
                         prepared_cache_pattern=cache_pattern(status, index + 1, args.dynamic_rect),
                         observed_service_after_admission_ms=(time.perf_counter() - admitted_at) * 1000,
                         end_to_end_frame_wall_ms=(time.perf_counter() - begin) * 1000)
        pipeline_ms = (time.perf_counter() - pipeline_started) * 1000
        stopped, stop_ms = request({"request": "stop_recording"}, "stop_and_finalize")
        finished = stopped.get("event", {}).get("Finished", {})
        if finished.get("video_frames") != args.frames or finished.get("audio_frames") != 0 or stopped["status"]["active"]:
            raise RuntimeError(f"finalize receipt does not match workload: {stopped}")
        final, _ = request({"request": "snapshot"}, "final_snapshot")
        if final["collection"]["revision"] != expected_revision:
            raise RuntimeError("recording changed authored state beyond the explicitly requested edits")
        if args.dynamic_rect:
            if final_document.exists():
                raise RuntimeError("final synthetic document already exists; never overwrite the initial save")
            request({"request": "save", "path": str(final_document)}, "save_clean_final_synthetic_document")
        request({"request": "close"}, "close_clean_document")
        report["exit_code"] = child.finish()
        if report["exit_code"] != 0:
            raise RuntimeError(f"CLI exited {report['exit_code']}")
        actual = inspect_mp4(output)
        video = actual["video"]
        expected_duration = args.frames / FPS
        tolerance = max(.0001, 2 / video["timescale"])
        if video["sample_count"] != args.frames or video["timed_sample_count"] != args.frames:
            raise RuntimeError(f"final native MP4 sample count differs: {actual}")
        if any(abs(video[field] - expected_duration) > tolerance for field in ("duration_seconds", "sample_table_duration_seconds")):
            raise RuntimeError(f"final native MP4 duration differs from authored 30fps timeline: {actual}")
        durations = [frame["end_to_end_frame_wall_ms"] for frame in report["frames"]]
        report.update(status="passed", total_wall_ms=(time.perf_counter() - started) * 1000,
                      recording={"start_roundtrip_ms": start_ms, "stop_finalize_roundtrip_ms": stop_ms,
                                 "pipeline_wall_ms": pipeline_ms, "path": str(output), "bytes": output.stat().st_size,
                                 "sha256": sha256(output), "finalized_video_frames": finished["video_frames"],
                                 "finalized_audio_frames": finished["audio_frames"], "observed_mp4_metadata": actual,
                                 "prepared_cache_pattern": cache_pattern(stopped["status"], args.frames, args.dynamic_rect),
                                 "duration_comparison_tolerance_seconds": tolerance},
                      timing={"admission_roundtrip": summary([frame["admission_roundtrip_ms"] for frame in report["frames"]]),
                              "observed_service_after_admission": summary([frame["observed_service_after_admission_ms"] for frame in report["frames"]]),
                              "end_to_end_frame": summary(durations), "first_frame_ms": durations[0],
                              "remaining_frames": summary(durations[1:]) if len(durations) > 1 else None,
                              "authored_edit_roundtrip": summary([frame["edit_roundtrip_ms"] for frame in report["frames"]]) if args.dynamic_rect else None,
                              "unpaced_throughput_fps": args.frames * 1000 / pipeline_ms,
                              "media_duration_seconds": expected_duration,
                              "30fps_per_frame_budget_ms": 1000 / FPS,
                              "frames_within_30fps_budget": sum(value <= 1000 / FPS for value in durations),
                              "all_frames_within_30fps_budget_this_run": all(value <= 1000 / FPS for value in durations),
                              "pipeline_within_media_duration_this_run": pipeline_ms <= expected_duration * 1000,
                              "end_to_end_includes_explicit_authored_edit": args.dynamic_rect,
                              "limits": "Wall timings include CLI IPC/status observation (5ms polling); not isolated CPU/kernel timings. No claim about device input, GUI, audio, hardware selection, paced transport, sustained live 30fps or other machines."},
                      document={"path": str(document), "sha256": sha256(document), "saved_revision": authored_revision,
                                "final_path": str(final_document), "final_sha256": sha256(final_document), "final_saved_revision": expected_revision},
                      independent_decoder_or_rgb_verification="not run")
    except Exception as error:
        report.update(error=f"{type(error).__name__}: {error}", traceback=traceback.format_exc(),
                      total_wall_ms=(time.perf_counter() - started) * 1000)
    finally:
        if child is not None:
            child.abort()
            report["exit_code"] = child.process.returncode
            report["observed_peak_working_set_bytes"] = child.peak
            report["memory_scope"] = "Windows kernel peak working set for the single CLI process, sampled while waiting; None on unsupported hosts. Excludes GUI/GPU/other processes."
            report["protocol_request_count"] = child.requests
            try:
                report["stderr"] = bytes(child.stderr).decode("utf-8")
            except UnicodeDecodeError:
                report["stderr_encoding"] = "non-UTF8; exact bytes are in cli-stderr.log and stderr_hex"
                report["stderr_hex"] = bytes(child.stderr).hex()
            with (folder / "cli-stderr.log").open("xb") as file:
                file.write(child.stderr)
        with (folder / "report.json").open("x", encoding="utf-8") as file:
            json.dump(report, file, indent=2, ensure_ascii=False, allow_nan=False)
            file.write("\n")
    return report, 0 if report["status"] == "passed" else 1


def self_test() -> None:
    def box(kind, payload):
        return struct.pack(">I4s", len(payload) + 8, kind) + payload
    timing = b"\0" * 12 + struct.pack(">II", 30000, 3000)
    handler = b"\0" * 8 + b"vide" + b"\0" * 4
    table = box(b"stsz", b"\0" * 4 + struct.pack(">II", 1, 3)) + box(b"stts", b"\0" * 4 + struct.pack(">III", 1, 3, 1000))
    moov = box(b"mvhd", timing) + box(b"trak", box(b"mdia", box(b"mdhd", timing) + box(b"hdlr", handler) + box(b"minf", box(b"stbl", table))))
    actual = parse_moov(moov)
    assert actual["video"]["sample_count"] == 3 and actual["video"]["duration_seconds"] == .1
    assert actual["video"]["sample_table_duration_seconds"] == .1
    for index in range(90):
        assert frame_time_ns(index) * FPS // 1_000_000_000 == index
        rect = moving_rect(index)
        assert rect["x"] % 2 == rect["y"] % 2 == 0
        assert 0 <= rect["x"] <= WIDTH - rect["width"] and 0 <= rect["y"] <= HEIGHT - rect["height"]
    assert len({(moving_rect(index)["x"], moving_rect(index)["y"]) for index in range(90)}) == 90
    assert cache_pattern({}, 30, False)["availability"] == "unavailable"
    assert cache_pattern({"prepared_video_cache_hits": 29, "prepared_video_cache_misses": 1}, 30, False)["validated"]
    assert cache_pattern({"prepared_video_cache_hits": 0, "prepared_video_cache_misses": 30}, 30, True)["validated"]
    try:
        cache_pattern({"prepared_video_cache_hits": 29, "prepared_video_cache_misses": 1}, 30, True)
    except ValueError:
        pass
    else:
        raise AssertionError("a dynamic run cannot silently use static-frame cache hits")
    for invalid in [b"\0", struct.pack(">I4s", 1000, b"trak"), moov.replace(b"vide", b"soun", 1)]:
        try:
            parse_moov(invalid)
        except (ValueError, struct.error):
            pass
        else:
            raise AssertionError("invalid metadata must be rejected")
    # A silent/blocked pipe must not trap the caller in readline. This child is
    # plain Python, not SkyLive, an encoder, a GUI or an OS input consumer.
    child = Protocol([sys.executable, "-u", "-c", "import sys,time;sys.stdin.readline();time.sleep(10)"], time.perf_counter() + 2, .05)
    started = time.perf_counter()
    try:
        try:
            child.request({"request": "test"})
        except TimeoutError:
            assert time.perf_counter() - started < 1
        else:
            raise AssertionError("silent pipe must hit its bounded reply deadline")
    finally:
        child.abort()
    child = Protocol([sys.executable, "-u", "-c", "import sys;sys.stdin.readline();print('{\"ok\":true,\"result\":{\"value\":7}}',flush=True)"], time.perf_counter() + 2, 1)
    try:
        reply, elapsed = child.request({"request": "test"})
        assert reply["result"]["value"] == 7 and elapsed >= 0
        assert child.finish() == 0
    finally:
        child.abort()
    print(json.dumps({"self_test": "passed", "scope": "bounded MP4 metadata/parser pipe deadlines, exact rational frame slots, moving-rectangle bounds and cache-pattern assertions; no recording/device/GUI activation"}))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    for name in ("exe", "out", "source-manifest", "build-receipt"):
        parser.add_argument("--" + name, type=Path)
    parser.add_argument("--frames", type=int, default=30)
    parser.add_argument("--layers", type=int, default=1)
    parser.add_argument("--cc", action="store_true", help="Apply real OBS ColorCorrection (120 degree hue shift) to the top source")
    parser.add_argument("--dynamic-rect", action="store_true", help="Move a 640x360 rectangle every frame; requires at least two layers and cache misses for every encoded frame")
    parser.add_argument("--timeout", type=float, default=180.0)
    parser.add_argument("--reply-timeout", type=float, default=30.0)
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return 0
    if any(getattr(args, name) is None for name in ("exe", "out", "source_manifest", "build_receipt")):
        parser.error("--exe, --out, --source-manifest and --build-receipt are required")
    if not 1 <= args.frames <= 90 or not 1 <= args.layers <= 8:
        parser.error("frames must be 1..90 and layers 1..8")
    if args.dynamic_rect and args.layers < 2:
        parser.error("--dynamic-rect requires --layers 2..8")
    if not math.isfinite(args.timeout) or not 10 <= args.timeout <= 600 or not math.isfinite(args.reply_timeout) or not 1 <= args.reply_timeout <= 60:
        parser.error("total timeout must be 10..600s and reply timeout 1..60s")
    if args.out.exists() or args.out.is_symlink() or not args.out.parent.is_dir():
        parser.error("--out must be a new directory in an existing parent; no output is overwritten")
    report, result = benchmark(args)
    print(json.dumps({"status": report["status"], "report": str(args.out.resolve() / "report.json"),
                      "error": report.get("error")}, ensure_ascii=False))
    return result


if __name__ == "__main__":
    raise SystemExit(main())
