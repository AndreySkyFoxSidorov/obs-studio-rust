"""Explicit synthetic native recording through the real JSON CLI protocol."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ("exe", "output", "document", "report"):
        parser.add_argument("--" + key, type=Path, required=True)
    parser.add_argument("--width",type=int,default=64)
    parser.add_argument("--height",type=int,default=48)
    parser.add_argument("--frames",type=int,default=6)
    args = parser.parse_args()
    assert 16<=args.width<=1920 and 16<=args.height<=1080 and 1<=args.frames<=8
    for path in (args.output, args.document, args.report):
        assert not path.exists(), f"acceptance never overwrites {path}"
    requests = [
        {"request": "command", "command": {"op": "set_video", "width": args.width, "height": args.height, "fps_num": 10, "fps_den": 1}},
        {"request": "command", "command": {"op": "add_color_source", "scene": 1, "name": "Synthetic recording color", "rgba": [220, 35, 70, 255]}},
        {"request": "save", "path": str(args.document.resolve())},
        {"request": "start_recording", "path": str(args.output.resolve()), "time_ns": 0, "audio": False},
        *[{"request": "record_frame", "time_ns": index * 100_000_000} for index in range(args.frames)],
        {"request": "stop_recording"}, {"request": "close"}]
    child = subprocess.run([str(args.exe.resolve(strict=True)), "--stdio"], input="".join(json.dumps(request) + "\n" for request in requests),
                           text=True, encoding="utf-8", stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=45)
    replies = [json.loads(line) for line in child.stdout.splitlines()]
    passed = child.returncode == 0 and len(replies) == len(requests) and all(reply["ok"] for reply in replies)
    if passed:
        finished = replies[-2]["result"]["event"]["Finished"]
        passed = finished["video_frames"] == args.frames and finished["audio_frames"] == 0 and args.output.is_file()
    report = {"schema": 1, "passed": passed, "executable": str(args.exe.resolve()), "exe_sha256": hashlib.sha256(args.exe.read_bytes()).hexdigest(),
              "requests": requests, "replies": replies, "exit_code": child.returncode, "stderr": child.stderr,
              "scope": "synthetic video-only native MP4 throughCLI; independent decoder check is separate"}
    with args.report.open("x", encoding="utf-8") as output:
        json.dump(report, output, indent=2, ensure_ascii=False)
    assert passed, json.dumps(report, ensure_ascii=False)
    print(json.dumps({"passed": True, "output": str(args.output), "report": str(args.report)}))


if __name__ == "__main__":
    main()
