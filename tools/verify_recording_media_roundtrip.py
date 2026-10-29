"""Feed an owned synthetic native recording back into the actual Rust player.

No FFmpeg, devices, playback sink or network. This complements the independent
decoder oracle by checking the application's encoder→file-source integration.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for key in ("exe","input","document","pixels","report"):
        parser.add_argument("--"+key,type=Path,required=True)
    args=parser.parse_args()
    for path in (args.document,args.pixels,args.report):
        assert not path.exists(),f"acceptance never overwrites {path}"
    child=subprocess.Popen([str(args.exe.resolve(strict=True)),"--stdio"],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding="utf-8")
    exchanges=[]
    def request(value):
        child.stdin.write(json.dumps(value)+"\n");child.stdin.flush()
        line=child.stdout.readline()
        assert line,"CLI closed before its reply"
        reply=json.loads(line);exchanges.append({"request":value,"reply":reply})
        assert reply["ok"],reply
        return reply["result"]
    passed=False
    try:
        prepared=request({"request":"prepare_media","path":str(args.input.resolve(strict=True))})
        media=prepared["media"];media["looping"]=True
        request({"request":"command","command":{"op":"set_video","width":64,"height":48,"fps_num":10,"fps_den":1}})
        request({"request":"command","command":{"op":"add_media_source","scene":1,"name":"Owned recorded Program","media":media}})
        request({"request":"save","path":str(args.document.resolve())})
        request({"request":"render_program","path":str(args.pixels.resolve()),"width":64,"height":48,"time_ns":0})
        request({"request":"stop_media_playback"})
        request({"request":"close"})
        child.stdin.close();child.wait(timeout=15)
        assert child.returncode==0
        pixels=args.pixels.read_bytes();header=b"P6\n64 48\n255\n";assert pixels.startswith(header)
        rgb=pixels[len(header):];assert len(rgb)==64*48*3
        maximum=max(abs(value-[220,35,70][index%3]) for index,value in enumerate(rgb))
        assert maximum<=6,maximum
        passed=True
    finally:
        if child.poll() is None:child.kill();child.wait(timeout=5)
        stderr=child.stderr.read()
        report={"schema":1,"passed":passed,"input":str(args.input.resolve()),"input_sha256":hashlib.sha256(args.input.read_bytes()).hexdigest(),"exe_sha256":hashlib.sha256(args.exe.read_bytes()).hexdigest(),"exchanges":exchanges,"stderr":stderr,"scope":"own synthetic encoder output decoded by real Rust Media Source to Program pixels; no external decoder/device"}
        if passed:report["maximum_rgb_error"]=maximum
        with args.report.open("x",encoding="utf-8") as output:json.dump(report,output,indent=2,ensure_ascii=False)
    print(json.dumps({"passed":True,"report":str(args.report),"maximum_rgb_error":maximum}))


if __name__=="__main__":main()
