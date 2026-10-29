"""Measure a named headless editing workload; never starts the desktop program."""
import argparse
import ctypes
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import statistics
import struct
import subprocess
import tempfile
import time
import traceback
import zlib


def peak_working_set(pid):
    if os.name != "nt":
        return None
    from ctypes import wintypes

    class Counters(ctypes.Structure):
        _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD)] + [
            (name, ctypes.c_size_t) for name in (
                "PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage",
                "QuotaPagedPoolUsage", "QuotaPeakNonPagedPoolUsage", "QuotaNonPagedPoolUsage",
                "PagefileUsage", "PeakPagefileUsage")]
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    psapi = ctypes.WinDLL("psapi", use_last_error=True)
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    psapi.GetProcessMemoryInfo.argtypes = [wintypes.HANDLE, ctypes.POINTER(Counters), wintypes.DWORD]
    process_handle = kernel.OpenProcess(0x410, False, pid)
    if not process_handle:
        return None
    try:
        info = Counters()
        info.cb = ctypes.sizeof(info)
        if psapi.GetProcessMemoryInfo(process_handle, ctypes.byref(info), info.cb):
            return info.PeakWorkingSetSize
        return None
    finally:
        kernel.CloseHandle(process_handle)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--max-total-ms", type=float)
    args = parser.parse_args()
    exe = args.exe.resolve(strict=True)
    if "release" not in exe.parts or exe.stem != "skylive-cli":
        parser.error("pass the built release skylive-cli; desktop programs are never accepted")
    report = {"schema": 1, "scope": "headless CLI process only; no device/GPU/realtime/GUI claim",
              "environment": {"platform": platform.platform(), "machine": platform.machine()},
              "exe_sha256": hashlib.sha256(exe.read_bytes()).hexdigest(),
              "lock_sha256": hashlib.sha256(Path("Cargo.lock").read_bytes()).hexdigest(),
              "workload": {"color_layers": 64, "rename_undo_pairs": 100, "renders": 3,"affine_renders":3,
                           "linked_image":{"format":"bmp24","native_size":[2,2],"shared_instances":16,"renders":3},
                           "animated_gif":{"frames":2,"frame_delay_ns":100_000_000,"timestamps_ns":[0,100_000_000,100_000_001,150_000_000,200_000_001],"finite_cycles":1},
                           "automatic_fade":{"duration_ms":300,"timestamps_ns":[0,150_000_000,300_000_000],"program_layout_capture":True,"gif_from_color_at_midpoint":"green","target":"opaque blue","swap_after_video_end":True},
                           "manual":{"position":512,"hold_time_ns":10_300_000_000,"finish_time_ns":20_300_000_000,"samples":3,"hold_has_no_tick":True},
                           "quick_black":{"duration_ms":300,"samples":4,"linear_green_blue_half":188,"stored_preview_return":True,"logical_visual_alias_reopen_resets":True},
                           "fade_to_color":{"rgb":[255,255,255],"switch_point":50,"samples":12,"encoded_null_half":128,"compound_delete_undo":True},
                           "directional":{"variants":12,"samples":36,"shared_manual_samples":2,"unused_presets_preserved":True,"virtual_time":True},
                           "source_alpha":{"samples":5,"native_rgba":[255,255,255,128],"color_over_gray":205,"color_over_black":188,"image_over_black":128,"image_over_gray":154,"gray_image_over_gray":111},
                           "render_size": [320, 180], "save_reopen": True, "display_capture": {"native_capture_requested": False, "native_enumeration_requested": False, "native_workload_measured": False}}}
    report["workload"]["audio"] = {"sources":8,"sample_rate":48000,"channels":2,"frames":480,"blocks":32,
                                     "samples_per_source":960,"shared_global_source":True,
                                     "native_capture_requested":False,"audio_output_requested":False,
                                     "synthetic_pcm_only":True,"realtime_measured":False}
    report["workload"]["timestamped_audio_program"] = {
        "sources":8,"shared_native_binding":True,"input_sample_rate":16000,
        "input_channels":1,"input_packets":16,"input_frames_per_packet":1000,
        "output_sample_rate":44100,"output_channels":1,"output_frames":44100,
        "max_output_block_frames":1024,"output_blocks":44,"input_dc":0.125,"source_gain":0.5,
        "mute_first_source_after_ns":500_000_000,"expected_before":0.5,
        "expected_after":0.4375,"oracle_ignored_edge_frames":1024,
        "synthetic_pcm_only":True,"native_capture_requested":False,
        "native_enumeration_requested":False,"audio_output_requested":False,
        "continuous_native_transport_measured":False,"realtime_measured":False}
    samples = {"edit_roundtrip_ms": [], "render_export_roundtrip_ms": [],"affine_render_export_roundtrip_ms":[],"image_import_roundtrip_ms":[],"image_render_export_roundtrip_ms":[],"gif_render_export_roundtrip_ms":[],"transition_render_export_roundtrip_ms":[],"manual_render_export_roundtrip_ms":[],"quick_black_render_export_roundtrip_ms":[],"source_alpha_render_export_roundtrip_ms":[],"directional_render_export_roundtrip_ms":[],"directional_manual_render_export_roundtrip_ms":[],"color_transition_render_export_roundtrip_ms":[],"audio_mix_roundtrip_ms":[]}
    peak = None
    samples["profile_roundtrip_ms"] = []
    samples["plugin_roundtrip_ms"] = []
    for metric in ("audio_program_reset_roundtrip_ms","audio_program_ingest_roundtrip_ms",
                   "audio_program_finish_roundtrip_ms","audio_program_route_roundtrip_ms",
                   "audio_program_render_roundtrip_ms"):
        samples[metric] = []
    started = time.perf_counter()
    child = subprocess.Popen([str(exe), "--stdio"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, text=True, encoding="utf-8")
    def send(request, metric=None):
        nonlocal peak
        begin = time.perf_counter()
        child.stdin.write(json.dumps(request) + "\n")
        child.stdin.flush()
        line = child.stdout.readline()
        if not line:
            raise RuntimeError("CLI ended before response: " + child.stderr.read())
        result = json.loads(line)
        if not result.get("ok"):
            raise RuntimeError(str(result))
        if metric:
            samples[metric].append((time.perf_counter() - begin) * 1000)
        observed = peak_working_set(child.pid)
        if observed is not None:
            peak = max(peak or 0, observed)
        return result["result"]
    try:
        with tempfile.TemporaryDirectory(prefix="skylive-measure-") as folder:
            temporary = Path(folder)
            for index in range(64):
                send({"request": "command", "command": {"op": "add_color_source", "scene": 1,
                      "name": f"Color {index}", "rgba": [index, 40, 90, 255]}})
            state = send({"request": "snapshot"})
            last = state["collection"]["sources"][-1]["id"]
            for _ in range(100):
                send({"request": "command", "command": {"op": "rename_source", "source": last,
                      "name": "Temporary"}}, "edit_roundtrip_ms")
                send({"request": "command", "command": {"op": "undo"}}, "edit_roundtrip_ms")
            assert send({"request": "snapshot"})["collection"]["sources"][-1]["name"] == "Color 63"
            for index in range(3):
                path = temporary / f"frame-{index}.ppm"
                send({"request": "render", "path": str(path), "width": 320, "height": 180}, "render_export_roundtrip_ms")
                pixels = path.read_bytes().split(b"\n", 3)[3]
                assert pixels == bytes([63, 40, 90]) * (320 * 180), "composition differs from opaque top-layer oracle"
            item = state["collection"]["scenes"][0]["items"][-1]["id"]
            send({"request":"command","command":{"op":"set_transform","scene":1,"item":item,
                "transform":{"x":960,"y":540,"width":960,"height":540,"opacity":1,
                "rotation_degrees":90,"flip_x":True,"flip_y":False,
                "crop":{"left":480,"top":270,"right":480,"bottom":270}}}})
            # Independent integer-pixel oracle: signed cropped axes rotate into
            # canvas bounds x=[690,960), y=[60,540), then preview scales by 1/6.
            background = bytes([62,40,90])
            foreground = bytes([63,40,90])
            expected = b"".join(background*115+foreground*45+background*160 if 10<=y<90
                                else background*320 for y in range(180))
            for index in range(3):
                path=temporary/f"affine-{index}.ppm"
                send({"request":"render","path":str(path),"width":320,"height":180},"affine_render_export_roundtrip_ms")
                assert path.read_bytes().split(b"\n",3)[3]==expected,"affine crop/rotation/flip differs from pixel oracle"
            image_path=temporary/"pattern.bmp"
            header=b"BM"+struct.pack("<IHHI",70,0,0,54)+struct.pack("<IiiHHIIIIII",40,2,2,1,24,0,16,0,0,0,0)
            # Bottom-up BGR rows: logical top red/green, bottom blue/white.
            image_path.write_bytes(header+bytes([255,0,0,255,255,255,0,0,0,0,255,0,255,0,0,0]))
            added=send({"request":"import_image","scene":1,"name":"Pattern","path":str(image_path)},"image_import_roundtrip_ms")
            image_source=send({"request":"snapshot"})["collection"]["sources"][-1]["id"]
            for index in range(16):
                item=added["created"] if index==0 else send({"request":"command","command":{"op":"add_existing_source","scene":1,"source":image_source}})["created"]
                send({"request":"command","command":{"op":"transform_item","scene":1,"item":item,"action":"stretch_to_canvas"}})
            expected=b"".join(bytes([255,0,0])*160+bytes([0,255,0])*160 if y<90 else bytes([0,0,255])*160+bytes([255,255,255])*160 for y in range(180))
            for index in range(3):
                path=temporary/f"image-{index}.ppm"
                send({"request":"render","path":str(path),"width":320,"height":180},"image_render_export_roundtrip_ms")
                assert path.read_bytes().split(b"\n",3)[3]==expected,"linked image/shared-reference composition differs from pixel oracle"
            gif_path=temporary/"once.gif"
            gif=bytearray(b"GIF89a"+bytes([1,0,1,0,0x80,0,0,255,0,0,0,255,0]))
            for data in [0x44,0x4c]:
                gif.extend(bytes([0x21,0xf9,4,0,10,0,0,0,0x2c,0,0,0,0,1,0,1,0,0,2,2,data,1,0]))
            gif.append(0x3b)
            gif_path.write_bytes(gif)
            added=send({"request":"import_image","scene":1,"name":"Animated GIF","path":str(gif_path)})
            send({"request":"command","command":{"op":"transform_item","scene":1,"item":added["created"],"action":"stretch_to_canvas"}})
            for index,time_ns in enumerate(report["workload"]["animated_gif"]["timestamps_ns"]):
                path=temporary/f"gif-{index}.ppm"
                send({"request":"render","path":str(path),"width":320,"height":180,"time_ns":time_ns},"gif_render_export_roundtrip_ms")
                expected=bytes([255,0,0] if index<2 else [0,255,0])*(320*180)
                assert path.read_bytes().split(b"\n",3)[3]==expected,"GIF explicit-time pixels differ from independent frame oracle"
            target=send({"request":"command","command":{"op":"add_scene","name":"Transition target"}})["created"]
            send({"request":"command","command":{"op":"add_color_source","scene":target,"name":"Transition blue","rgba":[0,0,255,255]}})
            send({"request":"studio_mode","enabled":True})
            send({"request":"select_preview","scene":target})
            send({"request":"transition"})
            for index,(time_ns,color) in enumerate(zip(report["workload"]["automatic_fade"]["timestamps_ns"],[[255,0,0],[0,128,128],[0,0,255]])):
                path=temporary/f"transition-{index}.ppm"
                send({"request":"render_program","path":str(path),"width":320,"height":180,"time_ns":time_ns},"transition_render_export_roundtrip_ms")
                assert path.read_bytes().split(b"\n",3)[3]==bytes(color)*(320*180),"captured Program/GIF/Fade pixels differ from independent time/color oracle"
            state=send({"request":"snapshot"})
            assert state["preview"]==1 and state["program"]==target and not state["transition"]["active"],"Studio must swap Preview only after video end"
            send({"request":"command","command":{"op":"configure_scene_transition","scene":1,"kind":"cut","duration_ms":1250}})
            send({"request":"tbar_position","value":512})
            path=temporary/"manual-hold.ppm"
            send({"request":"render_program","path":str(path),"width":320,"height":180,"time_ns":10_300_000_000},"manual_render_export_roundtrip_ms")
            assert path.read_bytes().split(b"\n",3)[3]==bytes([0,128,128])*(320*180),"manual hold must blend actual green GIF and blue Program equally"
            state=send({"request":"tbar_release"})
            assert state["transition"]["active"] and not state["transition"]["needs_tick"],"middle release holds without periodic static work"
            send({"request":"tbar_position","value":0})
            state=send({"request":"tbar_release"})
            assert not state["transition"]["active"] and state["program"]==target and state["preview"]==1,"low release must restore old Program without swap"
            path=temporary/"manual-cancel.ppm"
            send({"request":"render_program","path":str(path),"width":320,"height":180},"manual_render_export_roundtrip_ms")
            assert path.read_bytes().split(b"\n",3)[3]==bytes([0,0,255])*(320*180)
            send({"request":"tbar_position","value":1023})
            send({"request":"tbar_release"})
            path=temporary/"manual-finish.ppm"
            send({"request":"render_program","path":str(path),"width":320,"height":180,"time_ns":20_300_000_000},"manual_render_export_roundtrip_ms")
            assert path.read_bytes().split(b"\n",3)[3]==bytes([0,255,0])*(320*180)
            state=send({"request":"snapshot"})
            assert state["program"]==1 and state["preview"]==target and not state["transition"]["active"]
            send({"request":"quick_transition","id":3})
            for name,time_ns,color in [("black-mid",20_450_000_000,[0,188,0]),("black-end",20_600_000_000,[0,0,0])]:
                path=temporary/f"{name}.ppm"
                send({"request":"render_program","path":str(path),"width":320,"height":180,"time_ns":time_ns},"quick_black_render_export_roundtrip_ms")
                assert path.read_bytes().split(b"\n",3)[3]==bytes(color)*(320*180),"null endpoint must use linear-sRGB source gain"
            state=send({"request":"snapshot"})
            assert state["preview"]==target and state["program"]==target,"black transition must not swap Preview"
            send({"request":"select_preview","scene":1})
            send({"request":"quick_transition","id":3})
            for name,time_ns,color in [("return-mid",20_750_000_000,[0,0,188]),("return-end",20_900_000_000,[0,0,255])]:
                path=temporary/f"{name}.ppm"
                send({"request":"render_program","path":str(path),"width":320,"height":180,"time_ns":time_ns},"quick_black_render_export_roundtrip_ms")
                assert path.read_bytes().split(b"\n",3)[3]==bytes(color)*(320*180),"quick return must restore captured blue Preview, bypassing the new current-scene Cut override"
            state=send({"request":"snapshot"})
            assert state["program"]==1 and state["preview"]==1 and "scene_alias" in state["transition"]["composition"]
            alpha_scene=send({"request":"command","command":{"op":"add_scene","name":"Source alpha oracles"}})["created"]
            send({"request":"command","command":{"op":"add_color_source","scene":alpha_scene,"name":"Alpha background","rgba":[128,128,128,255]}})
            background_source=send({"request":"snapshot"})["collection"]["sources"][-1]["id"]
            foreground=send({"request":"command","command":{"op":"add_color_source","scene":alpha_scene,"name":"Alpha Color","rgba":[255,255,255,128]}})["created"]
            def alpha_frame(name,expected):
                path=temporary/f"{name}.ppm"
                send({"request":"render","scene":alpha_scene,"path":str(path),"width":320,"height":180},"source_alpha_render_export_roundtrip_ms")
                assert path.read_bytes().split(b"\n",3)[3]==bytes([expected]*3)*(320*180),"source-specific SDR blend differs from literal byte oracle"
            alpha_frame("color-on-gray",205)
            send({"request":"command","command":{"op":"set_color","source":background_source,"rgba":[0,0,0,255]}})
            alpha_frame("color-on-black",188)
            send({"request":"command","command":{"op":"remove_item","scene":alpha_scene,"item":foreground}})
            def write_alpha_png(path,rgba):
                def chunk(kind,data):
                    return struct.pack(">I",len(data))+kind+data+struct.pack(">I",zlib.crc32(kind+data))
                path.write_bytes(b"\x89PNG\r\n\x1a\n"+chunk(b"IHDR",struct.pack(">IIBBBBB",1,1,8,6,0,0,0))+chunk(b"IDAT",zlib.compress(b"\0"+bytes(rgba)))+chunk(b"IEND",b""))
            alpha_path=temporary/"white-alpha.png"
            write_alpha_png(alpha_path,[255,255,255,128])
            added=send({"request":"import_image","scene":alpha_scene,"name":"Alpha Image","path":str(alpha_path)})
            alpha_source=send({"request":"snapshot"})["collection"]["sources"][-1]["id"]
            send({"request":"command","command":{"op":"transform_item","scene":alpha_scene,"item":added["created"],"action":"stretch_to_canvas"}})
            alpha_frame("image-on-black",128)
            send({"request":"command","command":{"op":"set_color","source":background_source,"rgba":[128,128,128,255]}})
            alpha_frame("image-on-gray",154)
            gray_path=temporary/"gray-alpha.png"
            write_alpha_png(gray_path,[128,128,128,128])
            send({"request":"reload_image","source":alpha_source,"path":str(gray_path)})
            alpha_frame("gray-image-on-gray",111)
            preset=send({"request":"command","command":{"op":"add_transition_preset","name":"Shared movement","kind":{"swipe":{"direction":"left","swipe_in":False}}}})["created_transition_preset"]
            send({"request":"command","command":{"op":"add_transition_preset","name":"Unused movement","kind":{"slide":{"direction":"down"}}}})
            send({"request":"command","command":{"op":"select_transition","selection":{"reference":{"preset":preset},"duration_ms":400}}})
            send({"request":"command","command":{"op":"update_quick_transition_selection","id":2,"selection":{"reference":{"preset":preset},"duration_ms":500},"fade_to_black":False}})
            send({"request":"command","command":{"op":"configure_scene_transition_selection","scene":1,"reference":{"preset":preset},"duration_ms":800}})
            def movement_frame(name,time_ns,direction,denominator,part,metric):
                output=temporary/f"{name}.ppm"
                send({"request":"render_program","path":str(output),"width":320,"height":180,"time_ns":time_ns},metric)
                expected=bytearray()
                for y in range(180):
                    for x in range(320):
                        coord,extent=(x,320) if direction in ("left","right") else (y,180)
                        # Exact pixel-center mask; source shader uses inclusive
                        # [0,1]. Solid endpoint colors make bilinear irrelevant.
                        a=((2*coord+1)*denominator<=2*extent*(denominator-part)) if direction in ("left","up") else ((2*coord+1)*denominator>=2*extent*part)
                        expected.extend([0,255,0] if a else [0,0,255])
                assert output.read_bytes().split(b"\n",3)[3]==expected,"directional pixel-center/cubic-easing oracle differs"
            now=21_000_000_000
            variants=[]
            for direction in ("left","right","up","down"):
                variants.extend((direction,{"swipe":{"direction":direction,"swipe_in":mode}}) for mode in (False,True))
                variants.append((direction,{"slide":{"direction":direction}}))
            for index,(direction,kind) in enumerate(variants):
                send({"request":"command","command":{"op":"set_transition_preset_kind","id":preset,"kind":kind}})
                send({"request":"select_preview","scene":1,"time_ns":now})
                send({"request":"cut"})
                send({"request":"select_preview","scene":target})
                send({"request":"transition"})
                movement_frame(f"movement-quarter-{index}",now+100_000_000,direction,16,1,"directional_render_export_roundtrip_ms")
                movement_frame(f"movement-half-{index}",now+200_000_000,direction,2,1,"directional_render_export_roundtrip_ms")
                movement_frame(f"movement-end-{index}",now+400_000_000,direction,1,1,"directional_render_export_roundtrip_ms")
                now+=500_000_000
            send({"request":"select_preview","scene":1,"time_ns":now})
            send({"request":"cut"})
            send({"request":"select_preview","scene":target})
            send({"request":"tbar_position","value":512})
            now+=10_000_000_000
            movement_frame("movement-held-down",now,"down",2,1,"directional_manual_render_export_roundtrip_ms")
            send({"request":"tbar_release"})
            held=send({"request":"snapshot"})["transition"]
            assert held["manual"]["current"]==0.5 and not held["needs_tick"]
            send({"request":"command","command":{"op":"configure_transition_preset","id":preset,"name":"Shared renamed movement","kind":{"swipe":{"direction":"left","swipe_in":True}}}})
            movement_frame("movement-held-edited",now,"left",2,1,"directional_manual_render_export_roundtrip_ms")
            edited=send({"request":"snapshot"})["transition"]
            assert held["epoch"]==edited["epoch"] and held["composition"]!=edited["composition"]
            send({"request":"tbar_position","value":0})
            send({"request":"tbar_release"})
            send({"request":"select_preview","scene":1})
            color_preset=send({"request":"command","command":{"op":"add_transition_preset","name":"White color bridge","kind":{"fade_to_color":{"rgb":[255,255,255],"switch_point":50}}}})["created_transition_preset"]
            send({"request":"command","command":{"op":"select_transition","selection":{"reference":{"preset":color_preset},"duration_ms":1000}}})
            for quick_id in (2,3):
                send({"request":"command","command":{"op":"update_quick_transition_selection","id":quick_id,"selection":{"reference":{"preset":color_preset},"duration_ms":1000},"fade_to_black":quick_id==3}})
            send({"request":"command","command":{"op":"configure_scene_transition_selection","scene":1,"reference":{"preset":color_preset},"duration_ms":1000}})
            send({"request":"select_preview","scene":target})
            send({"request":"transition"})
            def color_transition_frame(name,time_ns,color):
                output=temporary/f"{name}.ppm"
                send({"request":"render_program","path":str(output),"width":320,"height":180,"time_ns":time_ns},"color_transition_render_export_roundtrip_ms")
                assert output.read_bytes().split(b"\n",3)[3]==bytes(color)*(320*180),"Fade to Color encoded smoothstep/null oracle differs"
            for offset,color in [(250_000_000,[128,255,128]),(500_000_000,[255,255,255]),(750_000_000,[128,128,255]),(1_000_000_000,[0,0,255])]:
                color_transition_frame(f"color-ordinary-{offset}",now+offset,color)
            now+=1_000_000_000
            send({"request":"quick_transition","id":3})
            for offset,color in [(250_000_000,[128,128,255]),(500_000_000,[255,255,255]),(750_000_000,[128,128,128]),(1_000_000_000,[0,0,0])]:
                color_transition_frame(f"color-black-{offset}",now+offset,color)
            now+=1_000_000_000
            send({"request":"quick_transition","id":3})
            for offset,color in [(250_000_000,[128,128,128]),(500_000_000,[255,255,255]),(750_000_000,[128,255,128]),(1_000_000_000,[0,255,0])]:
                color_transition_frame(f"color-return-{offset}",now+offset,color)
            before_delete=send({"request":"snapshot"})["collection"]
            send({"request":"command","command":{"op":"remove_transition_preset_and_references","id":color_preset}})
            deleted=send({"request":"snapshot"})["collection"]
            assert len(deleted["transition_presets"])==2 and len(deleted["quick_transitions"])==1
            assert deleted["scenes"][0]["transition_override"] is None and deleted["scenes"][0]["inactive_transition_duration_ms"]==1000
            assert deleted["transition"]["reference"]=={"preset":2},"compound removal must select last remaining preset"
            send({"request":"command","command":{"op":"undo"}})
            restored=send({"request":"snapshot"})["collection"]
            assert {key:value for key,value in before_delete.items() if key!="revision"}=={key:value for key,value in restored.items() if key!="revision"},"one Undo must restore all canonical preset references"
            audio_ids=[]
            for index in range(8):
                send({"request":"command","command":{"op":"add_audio_capture_source","scene":1,"name":f"Synthetic audio {index}",
                     "binding":{"flow":"input","target":{"kind":"default"},"use_device_timing":False,"exclude_own_audio":False}}})
                audio_id=send({"request":"snapshot"})["collection"]["sources"][-1]["id"]
                audio_ids.append(audio_id)
                send({"request":"command","command":{"op":"set_audio","source":audio_id,"gain":0.5,"muted":False}})
            send({"request":"command","command":{"op":"set_global_audio","slot":"mic1","source":audio_ids[0]}})
            # Studio intentionally preserves its captured item layout. Switch
            # to live scene 1 so this workload evaluates the newly added items.
            send({"request":"studio_mode","enabled":False})
            send({"request":"select_preview","scene":1})
            audio_plan=send({"request":"audio_plan"})["plan"]
            assert {source["source"] for source in audio_plan["sources"]}==set(audio_ids),"new live audio items must join Program after exiting the captured Studio layout"
            audio_inputs=[{"kind":"samples","source":source,"sample_rate":48000,"channels":2,"samples":[0.125]*960} for source in audio_ids]
            for _ in range(32):
                result=send({"request":"mix_program_audio","sample_rate":48000,"channels":2,"frames":480,"inputs":audio_inputs},"audio_mix_roundtrip_ms")
                assert result["native_capture"] is False and result["audio_output"] is False
                assert result["mixed"]["samples"]==[0.5]*960,"eight independent faders plus shared global ref must mix once per source ID"
            catalog=send({"request":"plugins"})
            assert len(catalog["providers"])==145 and not catalog["native_binary_loading"]
            third_party=[item for item in catalog["providers"] if item["descriptor"]["origin"]=="third_party"]
            bundled=[item for item in catalog["providers"] if item["descriptor"]["origin"]=="bundled"]
            assert third_party and all(not item["enabled"] for item in third_party),"third-party defaults must be off"
            assert len(bundled)==2 and all(item["enabled"] for item in bundled),"available bundled Craft modules default on"
            pan_id="norihiro/obs-audio-pan-filter"
            send({"request":"command","command":{"op":"set_plugin_enabled","plugin":pan_id,"enabled":True}})
            pan={"kind":"pan","pan_left":100.0,"pan_right":-100.0,"gain_left_db":0.0,"gain_right_db":0.0,"invert_left":False,"invert_right":False}
            send({"request":"command","command":{"op":"set_source_audio_filters","source":audio_ids[0],"filters":[pan]}})
            pan_input=[{"kind":"samples","source":source,"sample_rate":48000,"channels":2,"samples":[0.25,0.75]*480} if source==audio_ids[0] else {"kind":"silence","source":source} for source in audio_ids]
            for _ in range(8):
                mixed=send({"request":"mix_program_audio","sample_rate":48000,"channels":2,"frames":480,"inputs":pan_input},"plugin_roundtrip_ms")["mixed"]["samples"]
                assert all(abs(mixed[i]-expected)<1e-6 for i,expected in enumerate([0.375,0.125]*480)),"Pan swap precedes source gain0.5 and shared reference mixes once"
            send({"request":"command","command":{"op":"set_plugin_enabled","plugin":pan_id,"enabled":False}})
            mixed=send({"request":"mix_program_audio","sample_rate":48000,"channels":2,"frames":480,"inputs":pan_input},"plugin_roundtrip_ms")["mixed"]["samples"]
            assert mixed==[0.125,0.375]*480,"disabled Pan must bypass, preserving configured chain"
            send({"request":"command","command":{"op":"set_plugin_enabled","plugin":"exeldro/obs-scene-notes-dock","enabled":True}})
            send({"request":"command","command":{"op":"set_scene_notes","scene":1,"text":"Measured cue\nNative notes"}})
            send({"request":"command","command":{"op":"set_plugin_enabled","plugin":"exeldro/obs-scene-notes-dock","enabled":False}})
            report["workload"]["plugins"]={"catalog":145,"third_party_default_off":True,"bundled_default_on":[item["descriptor"]["id"] for item in bundled],"pan_blocks":8,"pan_frames_per_block":480,"source_gain":0.5,"expected_swap":[0.375,0.125],"disabled_bypass":[0.125,0.375],"native_notes_and_disabled_filter_reopen":True,"legacy_binary_loader":False}
            send({"request":"command","command":{"op":"set_audio_settings",
                  "settings":{"sample_rate":44100,"speakers":"mono"}}})
            reset=send({"request":"audio_program_reset"},"audio_program_reset_roundtrip_ms")
            identity=reset["identity"]
            start_frame=(reset["time_ns"]*44100+999_999_999)//1_000_000_000
            base_ns=start_frame*1_000_000_000//44100
            audio_binding={"flow":"input","target":{"kind":"default"},"use_device_timing":False,"exclude_own_audio":False}
            produced=0
            for packet in range(16):
                packet_ns=base_ns+(packet*1000*1_000_000_000)//16000
                result=send({"request":"audio_program_packet","identity":identity,
                    "binding":audio_binding,"stream_generation":1,"sequence":packet+1,
                    "first_media_ns":str(packet_ns),"received_media_ns":str(packet_ns),
                    "sample_rate":16000,"channels":1,"channel_mask":4,
                    "samples":[0.125]*1000},"audio_program_ingest_roundtrip_ms")
                assert result["native_capture"] is False and result["audio_output"] is False
                assert result["report"]["accepted_frames"]==1000
                produced+=result["report"]["output_frames"]
            finished=send({"request":"audio_program_finish","identity":identity,
                "binding":audio_binding,"stream_generation":1,"sequence":16},"audio_program_finish_roundtrip_ms")
            produced+=finished["report"]["output_frames"]
            assert produced==44100,"finite timestamped PCM conversion must produce exactly one project second"
            mute_ns=base_ns+500_000_000
            send({"request":"advance","time_ns":mute_ns})
            send({"request":"command","command":{"op":"set_audio","source":audio_ids[0],"gain":0.5,"muted":True}})
            routed=send({"request":"audio_program_route"},"audio_program_route_roundtrip_ms")
            assert routed["changed"] and routed["identity"]==identity
            mute_frame=(mute_ns*44100+999_999_999)//1_000_000_000
            cursor=start_frame
            while cursor<start_frame+44100:
                frames=min(1024,start_frame+44100-cursor)
                result=send({"request":"render_audio_program","start_frame":cursor,"frames":frames},"audio_program_render_roundtrip_ms")
                assert result["native_capture"] is False and result["audio_output"] is False
                block=result["block"]
                assert block["start_frame"]==cursor and block["frames"]==frames
                assert len(block["samples"])==frames and not block["gaps"],"supplied PCM must cover the entire timestamped output grid"
                assert block["clipped_samples"]==[0]
                for index,value in enumerate(block["samples"]):
                    assert math.isfinite(value),"timestamped Program PCM must stay finite"
                    absolute=cursor+index
                    if 1024<=absolute-start_frame<44100-1024:
                        expected=0.5 if absolute<mute_frame else 0.4375
                        assert abs(value-expected)<0.0001,"resampled shared-input faders/mid-block mute differ from independent DC oracle"
                cursor+=frames
            program_status=send({"request":"audio_program_status"})["status"]
            assert program_status["input_groups"]==1 and program_status["continuous_native_transport"] is False
            report["workload"]["timestamped_audio_program"].update(
                start_frame=start_frame,base_ns=base_ns,mute_event_frame=mute_frame,
                produced_output_frames=produced,accounted_bytes=program_status["accounted_bytes"])
            # Restore the authored fader before the existing native-save and
            # video reopen assertions; runtime PCM never enters native storage.
            send({"request":"command","command":{"op":"undo"}})
            restored_sources=send({"request":"snapshot"})["collection"]["sources"]
            assert next(source for source in restored_sources if source["id"]==audio_ids[0])["muted"] is False
            # Exercise real profile commands without touching devices or changing
            # the authored graph; the final reopen must retain the whole catalog.
            profile_before=send({"request":"snapshot"})
            original_profile=profile_before["profiles"]["selected"]
            profile_command=lambda command: send({"request":"command","command":{"op":"profile","command":command}},"profile_roundtrip_ms")
            profile_command({"op":"duplicate","name":"Measured duplicate"})
            duplicate=send({"request":"snapshot"})
            assert duplicate["collection"]["audio"]==profile_before["collection"]["audio"]
            profile_command({"op":"new","name":"Measured default"})
            fresh=send({"request":"snapshot"})
            assert fresh["collection"]["audio"]["sample_rate"]==48000
            assert fresh["collection"]["audio"]["speakers"]=="stereo"
            assert fresh["preferences"]==profile_before["preferences"]
            assert fresh["collection"]["sources"]==profile_before["collection"]["sources"]
            profile_command({"op":"select","id":original_profile})
            profile_command({"op":"rename","id":duplicate["profiles"]["selected"],"name":"Measured renamed"})
            profile_command({"op":"remove","id":fresh["profiles"]["selected"]})
            after_profiles=send({"request":"snapshot"})
            assert after_profiles["collection"]["audio"]==profile_before["collection"]["audio"]
            assert len(after_profiles["profiles"]["profiles"])==2
            assert after_profiles["collection"]["scenes"]==profile_before["collection"]["scenes"]
            report["workload"]["profiles"]={"operations":["duplicate","new","select","rename","remove","save","reopen"],"profiles_retained":2,"global_preferences_preserved":True,"scene_graph_preserved":True}
            path = temporary / "show.json"
            send({"request": "save", "path": str(path)})
            before_all = send({"request": "snapshot"})
            send({"request": "load", "path": str(path)})
            after_all=send({"request": "snapshot"})
            for scope in ("collection","profiles","preferences"):
                assert after_all[scope]==before_all[scope],f"native reopen must preserve {scope}"
            path=temporary/"gif-reopen.ppm"
            send({"request":"render","path":str(path),"width":320,"height":180})
            assert path.read_bytes().split(b"\n",3)[3]==bytes([255,0,0])*(320*180),"native reopen/default render must restart GIF at zero"
            path=temporary/"program-reopen.ppm"
            send({"request":"render_program","path":str(path),"width":320,"height":180})
            assert path.read_bytes().split(b"\n",3)[3]==bytes([255,0,0])*(320*180),"native reopen retains logical Program but resets its runtime return alias and GIF clock"
            send({"request": "close"})
            child.stdin.close()
            if child.wait(timeout=20) != 0:
                raise RuntimeError(child.stderr.read())
        total = (time.perf_counter() - started) * 1000
        report.update(status="passed", total_ms=total, observed_peak_working_set_bytes=peak,
                      memory_scope="Windows kernel peak working set, single CLI process; excludes GUI/GPU and other processes")
        report["timings"] = {key: {"count": len(values), "median_ms": statistics.median(values),
                                  "p95_ms": sorted(values)[max(0, math.ceil(len(values) * .95) - 1)], "max_ms": max(values)}
                             for key, values in samples.items()}
        report["budget"] = {"max_total_ms": args.max_total_ms,
                            "passed": None if args.max_total_ms is None else total <= args.max_total_ms}
        if report["budget"]["passed"] is False:
            report["status"] = "failed_budget"
    except Exception as error:
        report.update(status="failed", error=f"{type(error).__name__}: {error}", failure_traceback=traceback.format_exc())
    finally:
        if child.poll() is None:
            child.kill()
            child.wait()
        child.stdout.close()
        child.stderr.close()
        if not child.stdin.closed:
            child.stdin.close()
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"status": report["status"], "report": str(args.out)}))
    return 0 if report["status"] == "passed" else 2 if report["status"] == "failed_budget" else 1


if __name__ == "__main__":
    raise SystemExit(main())
