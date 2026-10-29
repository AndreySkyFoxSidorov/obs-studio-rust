"""Extract pinned OBS32 source data into immutable Rust address tables.

Authoring tool only. No Core parser, native API, network, codec or application
execution. Never read the mutable checkout HEAD. No connectivity claim.
"""
from pathlib import Path
import hashlib
import json
import re

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "tools/data/obs32-services.json"
COMMIT = "ba2f32bdf791005443988a4955e963663e16b1ed"
EXPECTED_SHA256 = "b476c9f41c4f01e7e45996a793ae8b605a2bbdbfa78bfbecf1d2d35af3ea59c0"

def exclusion(service, url):
    if service.get("protocol", "RTMP") not in ("RTMP", "RTMPS"):
        return "unsupported_protocol"
    if service.get("supported video codecs") and "h264" not in service["supported video codecs"]:
        return "unsupported_video_codec"
    if service.get("supported audio codecs") and "aac" not in service["supported audio codecs"]:
        return "unsupported_audio_codec"
    if not url.startswith(("rtmp://", "rtmps://")):
        return "dynamic_api_or_non_rtmp_url"
    if any(value in url for value in ("{", "}", "#", "\\")):
        return "template_fragment_or_backslash"
    if len(url.encode("utf-8")) > 4096 or any(ord(c) < 33 or 127 <= ord(c) <= 159 or c.isspace() for c in url):
        return "unsupported_url_bytes"
    authority, slash, application = url.split("://", 1)[1].partition("/")
    if not slash or not application.split("?", 1)[0]:
        return "application_path_required_by_current_endpoint"
    # Every fixed address in this pinned dataset uses an ASCII DNS authority.
    # Do not invent parsing/mappings for unsupported source-data authorities.
    host, colon, port = authority.partition(":")
    if not host or len(host) > 253 or re.fullmatch(r"[A-Za-z0-9_.-]+", host) is None:
        return "unsupported_authority"
    if colon and (not port.isascii() or not port.isdigit() or len(port) > 5 or not 1 <= int(port) <= 65535):
        return "unsupported_port"
    if re.search(r"%(?![A-Fa-f0-9]{2})", application):
        return "invalid_percent_escape"
    return None

def generate():
    raw = SOURCE.read_bytes()
    if hashlib.sha256(raw).hexdigest() != EXPECTED_SHA256:
        raise ValueError("pinned OBS source bytes changed; update provenance explicitly")
    data = json.loads(raw)
    included = []
    excluded = []
    for service in data["services"]:
        servers = []
        for server in service.get("servers", []):
            reason = exclusion(service, server["url"])
            if reason:
                excluded.append(dict(service=service["name"], server=server["name"], url=server["url"], reason=reason))
            else:
                servers.append(server)
        if servers:
            included.append((service, servers))
    # Finite authoring bounds for this pinned file; never truncate addresses.
    if len(included) > 128 or sum(len(servers) for _, servers in included) > 1024 or any(len(servers) > 256 for _, servers in included):
        raise ValueError("pinned service/server table exceeds its authoring bounds")
    literal = lambda value: json.dumps(value, ensure_ascii=False)
    lines = ["//! Generated from OBS32.2.2 " + COMMIT + "; GPL-2.0-or-later.\n",
             "//! tools/stream_service_presets.py; exact URLs, no live service lookup.\n",
             "use super::{ServicePreset,ServerPreset};\n",
             'pub const DATA_SHA256:&str="' + hashlib.sha256(raw).hexdigest() + '";\n',
             "pub(super) static SERVICES:&[ServicePreset]=&[\n"]
    for service, servers in included:
        aliases = service.get("alt_names", [])
        search = " ".join([service["name"], *aliases]).lower()
        lines.append("ServicePreset {name:" + literal(service["name"]) + ",common:" + str(service.get("common", False)).lower() + ",aliases:&[" + ",".join(literal(a) for a in aliases) + "],search:" + literal(search) + ",servers:&[\n")
        for server in servers:
            search = (server["name"] + " " + server["url"]).lower()
            lines.append("ServerPreset {name:" + literal(server["name"]) + ",url:" + literal(server["url"]) + ",search:" + literal(search) + "},\n")
        lines.append("]},\n")
    lines.append("];\n")
    target = ROOT / "crates/core/src/stream_service_presets/catalog.rs"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("".join(lines), encoding="utf-8", newline="\n")
    (ROOT / "tools/data/obs32-services-excluded.json").write_text(json.dumps(dict(commit=COMMIT, source_services=len(data["services"]), included_services=len(included), included_servers=sum(len(s) for _, s in included), excluded=excluded), ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

if __name__ == "__main__":
    generate()
