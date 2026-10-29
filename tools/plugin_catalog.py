"""Reproduce the reviewed Rust-provider metadata, without downloading or running plugins."""
import argparse
import json
import unittest
from plugin_references import ROOT, LIST, parse_catalog

SCOPES = {
    "exeldro/obs-freeze-filter": 'CPU SDR Freeze holds the first real upstream frame at the filter position and supports partial edge masks, duration and automatic refresh. Duration includes time waiting for the first frame; failed refresh retains the last complete image. Held, replacing and displayed frames share the existing128MiB budget. Start/end delays, lifecycle actions, logging, Freeze hotkeys, filter-position screenshots and HDR remain unavailable.',
    "obsproject/obs-browser": 'Windows x64 CEF127.145.7 software OSR through a Rust helper and C ABI: URL/local HTML, CSS, viewport/FPS, visibility/reload, alpha frames and HTML popup overlays. Optional actual CEF48kHz stereo PCM feeds source gain, filters, Program conversion and monitoring; control off retains native CEF playback. Matching external CEF runtime is required. OBS JS bridge, browser docks and native OBS browser-plugin ABI are not implemented.',
    "distroav/distroav": 'Windows x64 installed NDI6 Runtime receive: discovered NDI video RGB(A)/BGR(A), FLTp PCM, source properties and shared compositor/DSP/monitor. No NDI send/output, HX codec installation, tally, PTZ, recording metadata or native OBS plugin ABI.',
    'obsproject/obs-studio:obs-ffmpeg': 'Partial equivalent: local MP4/MOV Media Source with Rust H.264 Main/High/CABAC/B and AAC-LC playback, source properties and transport. On Windows an admitted software Media Foundation H.264 alternative supports progressive 8-bit 4:2:0 BT709 limited video, ordered multi-slice access units, PTS/DTS reordering and bounded drain/seek. No FFmpeg binary/library, hardware decoding, arbitrary FFmpeg formats, network media, FFmpeg outputs or encoders. Strict OBS32 local ffmpeg_source import/export uses actual worker-prepared video/audio metadata and preserves loop/restart/close/clear settings plus exact Play/Pause/Restart/Stop source shortcuts. Common Play/Pause chords reversibly map to native toggle bindings and export to both original arrays under the current pressed-event Host policy; configured Windows polling tracks per-action held counts and accepted pair latches; physical timing and OS key delivery remain unverified. Network settings and decoder overrides reject explicitly.',
    'obsproject/obs-studio:image-source': 'Static BMP/TGA/PNG/JPEG/WebP and animated GIF images with unload-when-hidden and linear-alpha properties. Timed slideshows include four stock transitions, custom canvas, visibility behaviors, shared transport, random policies and authored file/folder entries. Native and strict OBS interchange preserve image properties and represented slideshow settings. Bounded workers expand or explicitly refresh folders. Automatic image file watching, eager hidden preload, JXR decoding, empty playlists and complete image-source parity remain unimplemented.',
    'obsproject/obs-studio:win-capture': 'Windows GDI/DXGI/WGC Display Capture, GDI/WGC Window Capture and session-authorized Rust x64 D3D11 Game Capture. DXGI supports color, monochrome and masked-color cursors. Games require the bundled Rust hook DLL and opaque SDR RGBA/BGRA8 Present/Present1 frames. No D3D12, Vulkan, OpenGL, game-cursor/transparency options, HDR or anti-cheat bypass.',
    'obsproject/obs-studio:win-dshow': 'Partial equivalent: Windows Media Foundation camera video with exact raw RGB/YUV and baseline 8-bit MJPEG modes, pure-Rust JPEG decoding, standard driver controls and optional explicitly selected WASAPI input routed through the camera source mixer in Program. No embedded camera audio, automatic USB audio association, H264 camera transport, vendor property pages, DirectSound/WaveOut playback or native DirectShow plugin loading.',
    'obsproject/obs-studio:win-wasapi': 'Partial equivalent: Windows WASAPI endpoint capture and application process-tree loopback on build 20348 or later, with exact PID/creation-time/executable selection and 48 kHz stereo float PCM converted by the mixer. No process exclusion, automatic application rebinding or complete OBS WASAPI parity.',
    'obsproject/obs-studio:mac-avcapture': 'Partial equivalent: macOS 14+ AVFoundation camera video with existing permission and exact native BGRA modes. Camera source properties; no camera audio or complete AVFoundation/OBS parity. Native macOS runtime acceptance is not claimed.',
    "royshil/obs-backgroundremoval": 'Fixed Selfie256 CPU person foreground matte with threshold/softness controls, using an independently executed pure Rust model runtime. Limited single-model port; no GPU provider, recurrent matting, enhancement filters or OBS binary ABI.',
    "obsproject/obs-studio:obs-filters": "SDR OBS video: Color Correction v2, Color Key v2, Chroma Key v2, Luma Key v2, Sharpen v2, Crop/Pad, Image Mask/Blend v2, Apply LUT, Scroll and Scaling/Aspect Ratio. CPU spatial stages; admitted GPU point/LUT stages where available. Audio: Gain, Invert Polarity, Noise Gate, 3-Band Equalizer, RNNoise Noise Suppression, Limiter, Compressor with external sidechain, Upward Compressor and Expander. Original-model RNNoise runs once per source on the CPU, with real delayed history shared by sidechain, monitoring and six tracks at project 44.1/48 kHz. Active RNNoise requires the source-owner pipeline; single-snapshot audio evaluation is explicitly unsupported. Speex, VST hosting, HDR, legacy v1 and remaining standard filters are not implemented. Render Delay uses a shared source-owned CPU SDR frame queue at its exact ordered filter position, with stock0..500ms settings and transparent warmup. Actual rendered ticks advance it once; missed ticks are not fabricated. The unchanged128MiB aggregate can reject large resolution/FPS/delay combinations. Stop, seek, deletion and rebind retire affected history; HDR and isolated stateless delay evaluation are unsupported.",
    "norihiro/obs-audio-pan-filter": "Six-property stereo pan DSP; native plugin ABI and OBS filter interchange remain unsupported.",
    "exeldro/obs-scene-notes-dock": "Plain-text per-scene notes. Rich HTML, external files, auto-scroll, timestamp hotkeys and full dock parity pending.",
    "norihiro/obs-mute-filter": "Not ported: requires source packet discard/readiness and separate MuteByUI resume timing, not just zero samples.",
}
IMPLEMENTED = {"exeldro/obs-freeze-filter", "obsproject/obs-browser", "distroav/distroav", "royshil/obs-backgroundremoval", "norihiro/obs-audio-pan-filter", "exeldro/obs-scene-notes-dock", "skylive/craft-audio-effects", "skylive/craft-video-effects", "obsproject/obs-studio:obs-filters"}
IMPLEMENTED |= {'obsproject/obs-studio:mac-avcapture', 'obsproject/obs-studio:image-source', 'obsproject/obs-studio:obs-ffmpeg', 'obsproject/obs-studio:win-wasapi', 'obsproject/obs-studio:win-capture', 'obsproject/obs-studio:win-dshow'}
NATIVE_SOURCE_MODULES = [{'id': 'obsproject/obs-studio:win-capture', 'name': 'win-capture', 'description': 'Windows display and window capture', 'description_key': 'Plugin.obsproject/obs-studio:win-capture.Description', 'source_url': 'https://github.com/obsproject/obs-studio/tree/master/plugins/win-capture', 'port_scope': 'Windows GDI/DXGI/WGC Display Capture, GDI/WGC Window Capture and session-authorized Rust x64 D3D11 Game Capture. DXGI supports color, monochrome and masked-color cursors. Games require the bundled Rust hook DLL and opaque SDR RGBA/BGRA8 Present/Present1 frames. No D3D12, Vulkan, OpenGL, game-cursor/transparency options, HDR or anti-cheat bypass.', 'origin': 'BuiltIn', 'implemented': True}, {'id': 'obsproject/obs-studio:win-dshow', 'name': 'win-dshow', 'description': 'Windows camera video sources', 'description_key': 'Plugin.obsproject/obs-studio:win-dshow.Description', 'source_url': 'https://github.com/obsproject/obs-studio/tree/master/plugins/win-dshow', 'port_scope': 'Partial equivalent: Windows Media Foundation camera video with exact raw RGB/YUV and baseline 8-bit MJPEG modes, pure-Rust JPEG decoding, standard driver controls and optional explicitly selected WASAPI input routed through the camera source mixer in Program. No embedded camera audio, automatic USB audio association, H264 camera transport, vendor property pages, DirectSound/WaveOut playback or native DirectShow plugin loading.', 'origin': 'BuiltIn', 'implemented': True}, {'id': 'obsproject/obs-studio:win-wasapi', 'name': 'win-wasapi', 'description': 'Windows endpoint audio input and output capture', 'description_key': 'Plugin.obsproject/obs-studio:win-wasapi.Description', 'source_url': 'https://github.com/obsproject/obs-studio/tree/master/plugins/win-wasapi', 'port_scope': 'Partial equivalent: Windows WASAPI endpoint capture and application process-tree loopback on build 20348 or later, with exact PID/creation-time/executable selection and 48 kHz stereo float PCM converted by the mixer. No process exclusion, automatic application rebinding or complete OBS WASAPI parity.', 'origin': 'BuiltIn', 'implemented': True}]
BUNDLED = [
    dict(id="skylive/craft-audio-effects", name="Craft Audio Effects",
         description="Bundled compressor, noise gate, equalizer, SoundCraft Mod Delay and FilmCraft FDN Reverb.",
         description_key="Plugin.skylive/craft-audio-effects.Description.TimeFx1",
         source_url="https://github.com/storytold/filmcraft/tree/bfc23840b603e69c782302c6ed04d4a9f6ec26a8/crates/audio-dsp", origin="Bundled",
         port_scope="Source-owned compressor/gate/EQ plus SoundCraft modulated feedback delay and FilmCraft eight-line FDN reverb; original smoothing and bounded state. No convolution, chorus, complete donor library or composite scene audio-filter claim."),
    dict(id="skylive/craft-video-effects", name="Craft Video Effects",
         description="Bundled SDR color key and color correction kernels adapted from EffectCraft.",
         source_url="https://github.com/storytold/effectcraft/tree/991904888a20b7da72b2a4880b21c7d912154736/crates/effects", origin="Bundled",
         port_scope="Eleven SDR point filters: Linear Color Key, Brightness/Contrast, Spill Suppression, Invert Colors, Grayscale, Posterize, Exposure, Temperature/Tint, Threshold, Sepia and Photo Filter (20 fixed tints and Custom); plus spatial CPU Vignette and Gaussian Blur adapted from EffectCraft. Photo Filter and Sepia share the original density and luminosity kernel. Gaussian has direction and repeat-edge controls. No HDR, ICC, native OBS ABI, Photo Filter/Sepia/Vignette/Gaussian GPU kernels or complete Craft effect suite."),
]


def catalog(entries):
    rows = []
    for entry in entries:
        if entry["category"] == "resource":
            continue
        identity = entry["repository"]
        if entry.get("subpath"):
            identity += ":" + entry["subpath"].split("/")[-1].lower()
        rows.append(dict(id=identity, name=entry["name"], description=entry["description"],
                         source_url=entry["url"], origin="ThirdParty" if entry["category"] == "project" else "BuiltIn",
                         implemented=identity in IMPLEMENTED,
                         port_scope=SCOPES.get(identity, "No Rust provider implemented.")))
    known={row["id"] for row in rows}
    rows.extend(dict(row) for row in NATIVE_SOURCE_MODULES if row["id"] not in known)
    rows.extend(dict(row, implemented=row["id"] in IMPLEMENTED) for row in BUNDLED)
    for row in rows:
        row.setdefault("description_key", f"Plugin.{row['id']}.Description")
        if row["id"]=="obsproject/obs-studio:obs-filters":row["description_key"]+=".Dynamics1"
        if row["id"]=="royshil/obs-backgroundremoval":row["description_key"]+=".Selfie1"
    if len(rows) > 256 or len({row["id"] for row in rows}) != len(rows):
        raise ValueError("Catalog limit or duplicate identity")
    return rows


def rust_string(value):
    # Encode each character once: literal backslash-u text stays literal data.
    escapes = {"\\": "\\\\", '"': '\\"', "\n": "\\n", "\r": "\\r", "\t": "\\t"}
    return '"' + "".join(escapes.get(char, f"\\u{{{ord(char):04x}}}" if ord(char) < 32 else char) for char in value) + '"'


def rust(rows):
    result = "// Generated catalog metadata from supplied list; not executable downloaded code.\nuse super::{PluginDescriptor, PluginOrigin};\npub(super) const CATALOG: &[PluginDescriptor] = &[\n"
    for row in rows:
        fields = [f"{key}:" + rust_string(row[key]) for key in ("id", "name", "description", "description_key", "source_url")]
        fields += ["origin:PluginOrigin::" + row["origin"], "implemented:" + str(row["implemented"]).lower(), "port_scope:" + rust_string(row["port_scope"])]
        result += "    PluginDescriptor { " + ", ".join(fields) + " },\n"
    return result + "];\n"


class Tests(unittest.TestCase):
    def test_description_version_changes_only_the_revised_bundled_scope(self):
        rows = catalog(parse_catalog(LIST.read_text(encoding="utf-8")))
        for row in rows:
            suffix = ".TimeFx1" if row["id"] == "skylive/craft-audio-effects" else ".Dynamics1" if row["id"]=="obsproject/obs-studio:obs-filters" else ".Selfie1" if row["id"]=="royshil/obs-backgroundremoval" else ""
            self.assertEqual(row["description_key"], f"Plugin.{row['id']}.Description{suffix}")
    def test_actual_catalog_counts_and_manual_implementation_allowlist(self):
        rows = catalog(parse_catalog(LIST.read_text(encoding="utf-8")))
        self.assertEqual(len(rows), 145)
        self.assertEqual(sum(row["origin"] != "Bundled" for row in rows), 143)
        self.assertEqual({row["id"] for row in rows if row["origin"] == "Bundled"},
                         {"skylive/craft-audio-effects", "skylive/craft-video-effects"})
        self.assertEqual({r["id"] for r in rows if r["implemented"]}, IMPLEMENTED)
        self.assertFalse(next(r for r in rows if r["id"] == "norihiro/obs-mute-filter")["implemented"])

    def test_source_text_is_quoted_as_data(self):
        self.assertEqual(rust_string('"\n\x00'), '"\\\"\\n\\u{0000}"')

    def test_literal_unicode_escape_text_stays_literal(self):
        self.assertEqual(rust_string("\\u0000"), '"\\\\u0000"')

    def test_duplicate_ids_are_not_silently_aliased(self):
        entry = dict(category="project", repository="owner/repo", name="Name", description="", url="https://github.com/owner/repo", subpath=None)
        with self.assertRaises(ValueError):
            catalog([entry, entry])
        with self.assertRaises(ValueError):
            catalog([dict(entry, repository="skylive/craft-audio-effects")])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        result = unittest.main(argv=[__file__], exit=False)
        raise SystemExit(0 if result.result.wasSuccessful() else 1)
    rows = catalog(parse_catalog(LIST.read_text(encoding="utf-8")))
    outputs = {ROOT / "doc/migration/plugin-catalog.json": json.dumps(dict(schema=1, reference_count=sum(row["origin"] != "Bundled" for row in rows), bundled_count=sum(row["origin"] == "Bundled" for row in rows), catalog=rows), ensure_ascii=False, indent=2) + "\n",
               ROOT / "crates/core/src/plugin_catalog.rs": rust(rows)}
    for path, content in outputs.items():
        if args.check:
            if path.read_text(encoding="utf-8") != content:
                raise SystemExit(f"Catalog differs: {path}")
        else:
            path.write_text(content, encoding="utf-8")
    print(f"Reviewed catalog matches {len(rows)} providers: 143 supplied references + 3 native source modules + 2 bundled; only manually implemented scopes are activatable")


if __name__ == "__main__":
    main()
