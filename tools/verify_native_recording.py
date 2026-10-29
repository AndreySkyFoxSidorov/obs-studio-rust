"""Explicit independent oracle for the owned64x48/10fps/0.6s synthetic recording.

FFmpeg is an acceptance oracle only, never a production runtime dependency.
No devices, playback, network or user content. Requires NumPy for PCM analysis.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import numpy as np


def run(argv):
    result = subprocess.run(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, timeout=30, check=False)
    if result.returncode:
        raise RuntimeError(result.stderr.decode("utf-8", errors="replace")[:8192])
    return result.stdout


def verify(path, ffmpeg, ffprobe, audio, width=64, height=48, frame_count=6):
    assert 256 < path.stat().st_size < 64 * 1024 * 1024
    probe = json.loads(run([str(ffprobe), "-v", "error", "-show_streams", "-show_format", "-of", "json", str(path)]))
    streams = probe["streams"]
    videos = [stream for stream in streams if stream["codec_type"] == "video"]
    audios = [stream for stream in streams if stream["codec_type"] == "audio"]
    assert len(videos) == 1 and len(audios) == (audio == "chirp")
    video = videos[0]
    assert video["codec_name"] == "h264" and (video["width"], video["height"]) == (width, height)
    assert int(video["nb_frames"]) == frame_count and video["avg_frame_rate"] == "10/1"
    assert float(video["start_time"]) == 0 and abs(float(video["duration"]) - frame_count/10) < .0001
    assert [video.get(name) for name in ("color_space", "color_transfer", "color_primaries")] == ["bt709"] * 3
    assert video["color_range"] == "tv"
    frames = json.loads(run([str(ffprobe), "-v", "error", "-select_streams", "v:0", "-show_frames", "-show_entries", "frame=best_effort_timestamp_time", "-of", "json", str(path)]))["frames"]
    pts = [float(frame["best_effort_timestamp_time"]) for frame in frames]
    assert len(pts) == frame_count and all(abs(time - index / 10) < .0001 for index, time in enumerate(pts))
    raw = run([str(ffmpeg), "-v", "error", "-i", str(path), "-map", "0:v:0", "-frames:v", str(frame_count+1), "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
    assert len(raw) == frame_count * width * height * 3
    pixels = np.frombuffer(raw, np.uint8).reshape(frame_count, height, width, 3)
    color_error = float(np.max(np.abs(pixels.astype(np.float64) - [220, 35, 70])))
    assert color_error <= 6, color_error
    report = {"schema": 1, "passed": True, "input": str(path), "input_sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
              "ffmpeg_sha256": hashlib.sha256(ffmpeg.read_bytes()).hexdigest(), "ffprobe_sha256": hashlib.sha256(ffprobe.read_bytes()).hexdigest(),
              "video": {"codec": "h264", "width":width,"height":height,"frames": frame_count, "pts_seconds": pts, "rgb_mean": pixels.mean(axis=(0, 1, 2)).tolist(), "maximum_rgb_error": color_error, "color": "BT.709 limited"},
              "audio": None, "limits": [f"synthetic{frame_count/10}s only", "hardware selection not established", "no camera/microphone/screen acquisition", "no playback device"]}
    if audio == "chirp":
        assert frame_count==6,"chirp oracle duration is0.6s"
        sound = audios[0]
        assert sound["codec_name"] == "aac" and sound["profile"] == "LC"
        assert int(sound["sample_rate"]) == 48000 and sound["channels"] == 2
        assert float(sound["start_time"]) == 0
        assert .6 <= float(sound["duration"]) <= .6 + 1024 / 48000 + .0001
        pcm = np.frombuffer(run([str(ffmpeg), "-v", "error", "-i", str(path), "-map", "0:a:0", "-t", "0.65", "-f", "f32le", "-"]), dtype="<f4").reshape(-1, 2)
        assert 28800 <= len(pcm) <= 29824 and np.isfinite(pcm).all()
        time = np.arange(28800) / 48000
        expected = np.sin(2 * np.pi * (400 * time + 1000 * time * time)) * .2
        # Native AAC startup/flush windows are measured separately; do not
        # misrepresent a lossy encoder as sample-exact at the boundaries.
        actual = pcm[2048:27776, 0]
        reference = expected[2048:27776]
        rmse = float(np.sqrt(np.mean((actual - reference) ** 2)))
        correlation = float(np.corrcoef(actual, reference)[0, 1])
        lags = {lag: float(np.mean((pcm[2048 + lag:27776 + lag, 0] - reference) ** 2)) for lag in range(-32, 33)}
        best_lag = min(lags, key=lags.get)
        assert rmse < .002 and correlation > .999 and best_lag == 0
        assert float(np.max(np.abs(pcm))) < .25
        report["audio"] = {"codec": "aac_lc", "rate": 48000, "channels": 2, "decoded_frames": len(pcm),
                           "duration_seconds": float(sound["duration"]), "middle_rmse": rmse, "middle_correlation": correlation, "best_lag_samples": best_lag,
                           "first1024_rms": float(np.sqrt(np.mean(pcm[:1024] ** 2))),
                           "startup_rmse": float(np.sqrt(np.mean((pcm[:1024, 0] - expected[:1024]) ** 2))),
                           "padding_frames": len(pcm) - 28800}
        report["limits"].append("native AAC startup loss and up to1024 frames of flush padding remain visible; not gapless-audio proof")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--ffmpeg", type=Path, required=True)
    parser.add_argument("--ffprobe", type=Path, required=True)
    parser.add_argument("--audio", choices=["none", "chirp"], default="chirp")
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--width",type=int,default=64)
    parser.add_argument("--height",type=int,default=48)
    parser.add_argument("--frames",type=int,default=6)
    args = parser.parse_args()
    assert 16<=args.width<=1920 and 16<=args.height<=1080 and 1<=args.frames<=8
    assert not args.report.exists(), "acceptance reports are never silently replaced"
    report = verify(args.input.resolve(strict=True), args.ffmpeg.resolve(strict=True), args.ffprobe.resolve(strict=True), args.audio,args.width,args.height,args.frames)
    with args.report.open("x", encoding="utf-8") as output:
        json.dump(report, output, indent=2, ensure_ascii=False)
    print(json.dumps({"passed": True, "report": str(args.report), "video_frames": report["video"]["frames"], "audio": report["audio"]}, ensure_ascii=False))


if __name__ == "__main__":
    main()
