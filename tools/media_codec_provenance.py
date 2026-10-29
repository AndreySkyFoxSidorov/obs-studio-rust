"""Verify original reference hashes and refresh adapted hashes; never executes references."""
from __future__ import annotations
import argparse
import hashlib
import json
import os
from pathlib import Path
import tempfile

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "doc/migration/media-codec-provenance.json"

def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()

def verify_or_refresh(refresh: bool) -> int:
    data = json.loads(MANIFEST.read_text(encoding="utf-8"))
    changed = []
    for row in data["records"]:
        source = Path(row["source"])
        if not source.is_absolute():
            source = ROOT / source
        destination = (ROOT / row["destination"]).resolve()
        if not destination.is_relative_to((ROOT / "crates").resolve()):
            raise ValueError(f"destination outside codec workspace: {destination}")
        if digest(source) != row["source_sha256"]:
            raise ValueError(f"original reference hash changed: {source}")
        actual = digest(destination)
        if row["adapted_sha256"] != actual:
            changed.append(row["destination"])
            if refresh:
                row["adapted_sha256"] = actual
    if refresh and changed:
        payload = (json.dumps(data, indent=2, ensure_ascii=False) + "\n").encode("utf-8")
        handle, name = tempfile.mkstemp(prefix=MANIFEST.name + ".", suffix=".tmp", dir=MANIFEST.parent)
        try:
            with os.fdopen(handle, "wb") as stream:
                stream.write(payload)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(name, MANIFEST)
        finally:
            Path(name).unlink(missing_ok=True)
    print(json.dumps({"records": len(data["records"]), "source_hashes_valid": True,
                      "adapted_hashes_valid": refresh or not changed,
                      "refreshed": changed if refresh else [],
                      "stale": [] if refresh else changed}, ensure_ascii=False))
    return 0 if refresh or not changed else 1

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--refresh-adapted", action="store_true")
    args = parser.parse_args()
    raise SystemExit(verify_or_refresh(args.refresh_adapted))
