#!/usr/bin/env python3
"""Bounded stdlib cmap audit, Python3.11+. No GUI/native-font API/network/Cargo.

python tools/audit_localization_fonts.py --self-test
python tools/audit_localization_fonts.py --windows-candidates

Mapping-only Unicode cmap4/12 nonzero-GID union is not shaping, bidi, native
rendering or editing acceptance. The recorded CSV hash identifies owned bytes,
not a guarantee that the editable source file remains unchanged afterwards.
"""
from __future__ import annotations
import argparse
import bisect
import csv
import datetime
import hashlib
import io
import json
import os
from pathlib import Path
import struct
import sys
import tomllib
import unicodedata
import unittest

ROOT = Path(__file__).resolve().parents[1]
MAX_CSV_BYTES = 32 * 1024 * 1024
MAX_FONT_BYTES = 32 * 1024 * 1024
MAX_FONTS = 16
MAX_TOTAL_FONT_BYTES = 128 * 1024 * 1024
MAX_LOCALES = 128
MAX_ROWS = 32768
MAX_CELL_BYTES = 32768
MAX_UNIQUE = 32768
MAX_LOCALE_UNIQUE = 16384
MAX_MEMBERSHIPS = 262144
MAX_TEXT_VISITS = 64_000_000
MAX_FONT_LOOKUPS = 8_000_000
MAX_FACES = 128
MAX_TABLES = 256
MAX_CMAP_SUBTABLES = 32
MAX_GROUPS = 65536
MAX_REGISTRY_ENTRIES = 128
DEFAULT_FONTS = ("Ubuntu-Light.ttf", "NotoEmoji-Regular.ttf", "emoji-icon-font.ttf")
WINDOWS_FONTS = ("segoeui.ttf", "seguisym.ttf", "tahoma.ttf", "sylfaen.ttf",
                 "Nirmala.ttf", "LeelawUI.ttf", "malgun.ttf", "msgothic.ttc", "msyh.ttc")


class AuditError(ValueError):
    pass


def require(condition, detail):
    if not condition:
        raise AuditError(detail)


def read_owned(path, maximum):
    with path.open("rb") as stream:
        require(os.fstat(stream.fileno()).st_size <= maximum, f"file exceeds {maximum} bytes: {path}")
        data = stream.read(maximum + 1)
    require(len(data) <= maximum, f"file grew beyond {maximum} bytes: {path}")
    return data


def visible_points(text):
    return (ord(ch) for ch in text if not ch.isspace() and unicodedata.category(ch) != "Cf")


def csv_snapshot(path):
    data = read_owned(path, MAX_CSV_BYTES)
    try:
        reader = csv.reader(io.TextIOWrapper(io.BytesIO(data), encoding="utf-8-sig", newline=""), strict=True)
        csv.field_size_limit(MAX_CELL_BYTES)
        headers = next(reader)
        require(2 <= len(headers) <= MAX_LOCALES + 1 and headers[0] == "key", "invalid CSV header")
        require(len(set(headers)) == len(headers) and "en-US" in headers, "duplicate headers/missing en-US")
        codes, en = headers[1:], headers.index("en-US")
        points = {code: set() for code in codes}
        missing = {code: 0 for code in codes}
        union, names, keys = set(), set(), set()
        memberships = visits = 0
        for row in reader:
            require(len(keys) < MAX_ROWS, "CSV row limit exceeded")
            require(len(row) == len(headers), f"column mismatch at row {len(keys) + 2}")
            require(all(len(value.encode("utf-8")) <= MAX_CELL_BYTES for value in row), "cell byte limit exceeded")
            require(bool(row[0]) and row[0] not in keys, f"empty/duplicate key: {row[0]!r}")
            keys.add(row[0])
            for column, code in enumerate(codes, 1):
                value = row[column] or row[en]
                missing[code] += not bool(row[column])
                visits += len(value)
                require(visits <= MAX_TEXT_VISITS, "effective-text work budget exceeded")
                for point in visible_points(value):
                    if point not in points[code]:
                        points[code].add(point)
                        union.add(point)
                        memberships += 1
                        require(len(points[code]) <= MAX_LOCALE_UNIQUE, "per-locale codepoint limit exceeded")
                        require(memberships <= MAX_MEMBERSHIPS and len(union) <= MAX_UNIQUE, "codepoint-set budget exceeded")
                if row[0] == "SkyLive.LanguageName":
                    names.update(visible_points(row[column] or code))
        require(bool(keys), "CSV has no keys")
        require(len(union | names) <= MAX_UNIQUE, "native-name codepoint budget exceeded")
    except (UnicodeError, csv.Error, StopIteration) as cause:
        raise AuditError(f"malformed UTF-8 CSV: {cause}") from cause
    return ({"path": str(path.resolve()), "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest(),
             "keys": len(keys), "locales": len(codes), "missing_cells": missing,
             "effective_text_visits": visits, "owned_snapshot": True,
             "snapshot_may_be_stale_after_generation": True}, points, names)


class Work:
    def __init__(self, maximum=MAX_FONT_LOOKUPS):
        self.remaining = maximum

    def charge(self, count):
        require(count <= self.remaining, "glyph lookup work budget exceeded")
        self.remaining -= count


class Font:
    """Bounds-checked SFNT/TTC and cmap4/12 parser, explicit collection face.

    All table ranges, relative glyph-array offsets and maxp GIDs are checked.
    Supported Unicode subtables are unioned; no shaping or rasterizer is used.
    """
    def __init__(self, data, face=0):
        require(len(data) <= MAX_FONT_BYTES and 0 <= face < MAX_FACES, "font byte/face limit exceeded")
        self.data, self.face_count, self.records = data, 1, 0
        offset = 0
        if data[:4] == b"ttcf":
            require(self.u32(4) in (0x10000, 0x20000), "unsupported TTC version")
            self.face_count = self.u32(8)
            require(1 <= self.face_count <= MAX_FACES and face < self.face_count, "TTC count/index limit")
            self.range(12, 4 * self.face_count)
            offset = self.u32(12 + face * 4)
            require(offset >= 12 + 4 * self.face_count, "TTC face overlaps header")
        else:
            require(face == 0, "noncollection font only has face zero")
        require(self.range(offset, 4) in (b"\0\1\0\0", b"OTTO", b"true", b"typ1"), "invalid SFNT signature")
        count = self.u16(offset + 4)
        require(1 <= count <= MAX_TABLES, "SFNT table-count limit")
        self.range(offset + 12, count * 16)
        tables = {}
        for index in range(count):
            record = offset + 12 + index * 16
            tag, start, length = self.range(record, 4), self.u32(record + 8), self.u32(record + 12)
            self.bounds(start, length)
            require(tag not in tables, "duplicate SFNT table")
            tables[tag] = (start, length)
        require(b"maxp" in tables and tables[b"maxp"][1] >= 6, "missing/truncated maxp")
        self.glyph_count = self.u16(tables[b"maxp"][0] + 4)
        require(self.glyph_count > 0 and b"cmap" in tables, "missing glyphs/cmap")
        start, length = tables[b"cmap"]
        require(length >= 4 and self.u16(start) == 0, "invalid cmap header")
        count = self.u16(start + 2)
        require(count <= 128 and 4 + count * 8 <= length, "cmap encoding-record bounds")
        seen, self.subtables = set(), []
        for index in range(count):
            record = start + 4 + index * 8
            platform, encoding = self.u16(record), self.u16(record + 2)
            if platform != 0 and not (platform == 3 and encoding in (1, 10)):
                continue
            relative = self.u32(record + 4)
            require(4 + count * 8 <= relative and relative + 2 <= length, "cmap subtable offset outside table/data area")
            absolute = start + relative
            if absolute in seen:
                continue
            seen.add(absolute)
            require(len(seen) <= MAX_CMAP_SUBTABLES, "cmap subtable-count limit")
            form = self.u16(absolute)
            if form == 4:
                self.subtables.append(self.format4(absolute, start + length))
            elif form == 12:
                self.subtables.append(self.format12(absolute, start + length))
        require(bool(self.subtables), "no supported Unicode cmap4/12")

    def range(self, offset, length):
        self.bounds(offset, length)
        return self.data[offset:offset + length]

    def bounds(self, offset, length):
        require(offset >= 0 and length >= 0 and offset + length <= len(self.data), "font range outside owned bytes")

    def charge_records(self, count):
        self.records += count
        require(self.records <= MAX_GROUPS, "aggregate parsed-cmap record budget exceeded")

    def u16(self, offset):
        require(0 <= offset and offset + 2 <= len(self.data), "truncated u16")
        return struct.unpack_from(">H", self.data, offset)[0]

    def u32(self, offset):
        require(0 <= offset and offset + 4 <= len(self.data), "truncated u32")
        return struct.unpack_from(">I", self.data, offset)[0]

    def format4(self, start, cmap_end):
        length = self.u16(start + 2)
        require(length >= 24 and start + length <= cmap_end, "format4 length outside cmap")
        twice = self.u16(start + 6)
        require(twice > 0 and twice % 2 == 0, "invalid format4 segment count")
        count = twice // 2
        require(16 + count * 8 <= length, "truncated format4 arrays")
        self.charge_records(count)
        ends = [self.u16(start + 14 + i * 2) for i in range(count)]
        starts = [self.u16(start + 16 + count * 2 + i * 2) for i in range(count)]
        deltas = [self.u16(start + 16 + count * 4 + i * 2) for i in range(count)]
        ranges, previous = [], -1
        for i, (first, last) in enumerate(zip(starts, ends)):
            require(previous < first <= last, "unordered/overlapping format4 segments")
            previous = last
            position = start + 16 + count * 6 + i * 2
            relative = self.u16(position)
            require(relative % 2 == 0, "unaligned format4 glyph range")
            if relative:
                require(position + relative >= start + 16 + count * 8, "format4 glyph range overlaps arrays")
                require(position + relative + (last - first + 1) * 2 <= start + length, "format4 glyph range exceeds subtable")
            ranges.append((position, relative))
        require(ends[-1] == 65535, "format4 sentinel absent")
        return (4, starts, ends, deltas, ranges)

    def format12(self, start, cmap_end):
        require(start + 16 <= cmap_end, "truncated format12 header")
        length, count = self.u32(start + 4), self.u32(start + 12)
        require(length >= 16 and start + length <= cmap_end, "format12 length outside cmap")
        require(count <= MAX_GROUPS and 16 + count * 12 <= length, "format12 group count/bounds")
        self.charge_records(count)
        groups, previous = [], -1
        for i in range(count):
            p = start + 16 + i * 12
            first, last, glyph = self.u32(p), self.u32(p + 4), self.u32(p + 8)
            require(previous < first <= last <= 0x10FFFF, "unordered/invalid format12 Unicode range")
            require(glyph + last - first < self.glyph_count, "format12 GID exceeds maxp")
            groups.append((first, last, glyph))
            previous = last
        return (12, [group[0] for group in groups], groups)

    def coverage(self, needed, work):
        require(len(needed) <= MAX_UNIQUE, "glyph query-set limit")
        found = set()
        for table in self.subtables:
            pending = needed - found
            work.charge(len(pending))
            if table[0] == 12:
                _, starts, groups = table
                for point in pending:
                    index = bisect.bisect_right(starts, point) - 1
                    if index >= 0:
                        first, last, glyph = groups[index]
                        if point <= last and glyph + point - first:
                            found.add(point)
            else:
                _, starts, ends, deltas, ranges = table
                for point in pending:
                    if point > 65535:
                        continue
                    index = bisect.bisect_left(ends, point)
                    if index == len(ends) or point < starts[index]:
                        continue
                    position, relative = ranges[index]
                    if relative:
                        glyph = self.u16(position + relative + (point - starts[index]) * 2)
                        if glyph:
                            glyph = (glyph + deltas[index]) & 65535
                    else:
                        glyph = (point + deltas[index]) & 65535
                    require(glyph < self.glyph_count, "format4 GID exceeds maxp")
                    if glyph:
                        found.add(point)
        return found


def discover_defaults(directory):
    lock = tomllib.loads(read_owned(ROOT / "Cargo.lock", 2 * 1024 * 1024).decode("utf-8"))
    versions = {p["version"] for p in lock["package"] if p["name"] == "epaint_default_fonts"}
    require(len(versions) == 1, "Cargo.lock must pin one epaint_default_fonts version")
    version = versions.pop()
    require(version == "0.31.1", "default proportional family list must be reviewed for this epaint version")
    if directory is None:
        source = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")) / "registry/src"
        registries = []
        for index, path in enumerate(source.iterdir()):
            require(index < MAX_REGISTRY_ENTRIES, "registry entry discovery limit")
            if path.is_dir():
                registries.append(path)
                require(len(registries) <= 32, "registry directory discovery limit")
        registries.sort()
        directory = next((p / f"epaint_default_fonts-{version}/fonts" for p in registries
                          if (p / f"epaint_default_fonts-{version}/fonts").is_dir()), None)
        require(directory is not None, "pinned fonts absent; specify --default-font-dir")
    require(all((directory / name).is_file() for name in DEFAULT_FONTS), "default font directory incomplete")
    return directory, version


def audit(args):
    source, locales, names = csv_snapshot(args.csv)
    needed = set().union(*locales.values(), names)
    directory, version = discover_defaults(args.default_font_dir)
    specs = [(directory / name, 0, "default_proportional") for name in DEFAULT_FONTS]
    specs += [(path, 0, "candidate") for path in args.font]
    specs += [(Path(path), int(face), "candidate") for path, face in args.font_face]
    absent = []
    if args.windows_candidates:
        windows = Path(os.environ.get("WINDIR", "C:/Windows")) / "Fonts"
        for name in WINDOWS_FONTS:
            path = windows / name
            if path.is_file():
                specs.append((path, 0, "candidate"))
            else:
                absent.append(str(path))
    require(len(specs) <= MAX_FONTS, "font-count limit")
    default, candidate, fonts, total, work = set(), set(), [], 0, Work()
    for path, face, role in specs:
        remaining = MAX_TOTAL_FONT_BYTES - total
        require(remaining > 0, "total encoded-font work limit")
        data = read_owned(path, min(MAX_FONT_BYTES, remaining))
        total += len(data)
        require(total <= MAX_TOTAL_FONT_BYTES, "total encoded-font work limit")
        font = Font(data, face)
        covered = font.coverage(needed, work)
        (default if role == "default_proportional" else candidate).update(covered)
        fonts.append({"path": str(path.resolve()), "face": face, "face_count": font.face_count, "role": role,
                      "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest(), "glyph_count": font.glyph_count,
                      "required_codepoints_mapped": len(covered), "formats": sorted({t[0] for t in font.subtables})})
    union = default | candidate
    return {"schema": 1, "generated_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
            "scope": "Unicode cmap4/12 nonzero-GID union only; not shaping/raster/native acceptance",
            "csv": source, "epaint_default_fonts_version": version,
            "policy": {"exclude_whitespace": True, "exclude_category_Cf": True, "include_combining_marks": True,
                       "ttc_face_explicit": True, "require_gid_below_maxp": True},
            "limits": {n: v for n, v in globals().items() if n.startswith("MAX_") and isinstance(v, int)},
            "fonts": fonts, "missing_optional_candidate_files": absent,
            "summary": {"required_unique": len(needed), "default_missing_unique": len(needed - default),
                        "with_candidates_missing_unique": len(needed - union),
                        "native_names_default_missing": len(names - default),
                        "native_names_with_candidates_missing": len(names - union),
                        "total_font_bytes_processed": total, "glyph_lookups": MAX_FONT_LOOKUPS - work.remaining},
            "locales": [{"code": c, "required_unique": len(p), "default_missing": len(p - default),
                         "with_candidates_missing": len(p - union),
                         "first_default_missing": [f"U+{x:04X}" for x in sorted(p - default)[:32]]}
                        for c, p in locales.items()]}


def log_text(report):
    lines = ["MAPPING-ONLY FONT AUDIT; not native rendering/shaping acceptance",
             f"CSV snapshot SHA256 {report['csv']['sha256']}",
             f"{report['csv']['bytes']} bytes; {report['csv']['keys']} keys; {report['csv']['locales']} locales",
             f"Pinned epaint_default_fonts {report['epaint_default_fonts_version']}",
             json.dumps(report["summary"], sort_keys=True), "locale default_missing with_candidates_missing"]
    lines += [f"{r['code']} {r['default_missing']} {r['with_candidates_missing']}" for r in report["locales"]]
    lines.append("Owned CSV snapshot may be stale after generation; compare its hash before citing current coverage.")
    return "\n".join(lines) + "\n"


def sfnt_fixture(subtable, glyphs=16, base=0):
    cmap = struct.pack(">HHHHI", 0, 1, 3, 10 if subtable[:2] == b"\0\x0c" else 1, 12) + subtable
    maxp = struct.pack(">IH", 0x10000, glyphs)
    start = base + 44
    return (struct.pack(">IHHHH", 0x10000, 2, 0, 0, 0)
            + struct.pack(">4sIII", b"cmap", 0, start, len(cmap))
            + struct.pack(">4sIII", b"maxp", 0, start + len(cmap), len(maxp)) + cmap + maxp)


def format4_fixture(array=False):
    ends, starts = [66 if array else 65, 65535], [65, 65535]
    delta = [1 if array else (3 - 65) & 65535, 1]
    body = (struct.pack(">HHH", *ends, 0) + struct.pack(">HH", *starts)
            + struct.pack(">HHHH", *delta, 4 if array else 0, 0)
            + (struct.pack(">HH", 0, 2) if array else b""))
    return struct.pack(">HHHHHHH", 4, 14 + len(body), 0, 4, 0, 0, 0) + body


class ParserTests(unittest.TestCase):
    def test_format4_delta_wrap_zero_and_relative_array(self):
        self.assertEqual(Font(sfnt_fixture(format4_fixture())).coverage({64, 65, 66, 65535}, Work()), {65})
        self.assertEqual(Font(sfnt_fixture(format4_fixture(True))).coverage({65, 66}, Work()), {66})

    def test_format12_supplementary_zero_gid_and_range_boundaries(self):
        sub = struct.pack(">HHIII", 12, 0, 40, 0, 2) + struct.pack(">IIIIII", 65, 65, 0, 0x1F600, 0x1F601, 3)
        self.assertEqual(Font(sfnt_fixture(sub)).coverage({65, 0x1F5FF, 0x1F600, 0x1F601, 0x1F602}, Work()), {0x1F600, 0x1F601})

    def test_ttc_second_face_uses_absolute_offsets(self):
        first = sfnt_fixture(format4_fixture(), base=20)
        second_offset = 20 + len(first)
        second = sfnt_fixture(format4_fixture(True), base=second_offset)
        data = struct.pack(">4sIIII", b"ttcf", 0x10000, 2, 20, second_offset) + first + second
        self.assertEqual(Font(data, 0).coverage({65, 66}, Work()), {65})
        self.assertEqual(Font(data, 1).coverage({65, 66}, Work()), {66})
        with self.assertRaises(AuditError):
            Font(data, 2)
        for position in (8, 12):
            malformed = bytearray(data)
            struct.pack_into(">I", malformed, position, 0xFFFFFFFF)
            with self.assertRaises(AuditError):
                Font(bytes(malformed))

    def test_malformed_offsets_lengths_counts_and_gids_reject(self):
        good = sfnt_fixture(format4_fixture(True))
        mutations = []
        for position, value in [(20, 0xFFFFFFF0), (28, 0)]:
            damaged = bytearray(good)
            struct.pack_into(">I", damaged, position, value)
            mutations.append(bytes(damaged))
        damaged = bytearray(good)
        struct.pack_into(">H", damaged, 44 + 12 + 28, 0xFFFE)
        mutations.append(bytes(damaged))
        for data in [b"", b"ttcf", good[:40], *mutations]:
            with self.assertRaises(AuditError):
                Font(data)
        with self.assertRaises(AuditError):
            Font(sfnt_fixture(struct.pack(">HHIII", 12, 0, 16, 0, 0xFFFFFFFF)))
        with self.assertRaises(AuditError):
            Font(sfnt_fixture(struct.pack(">HHIII", 12, 0, 28, 0, 1) + struct.pack(">III", 65, 65, 16)))

    def test_work_limit(self):
        with self.assertRaises(AuditError):
            Font(sfnt_fixture(format4_fixture())).coverage({65}, Work(0))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--csv", type=Path, default=ROOT / "localization/translations.csv")
    parser.add_argument("--default-font-dir", type=Path)
    parser.add_argument("--font", action="append", type=Path, default=[])
    parser.add_argument("--font-face", action="append", nargs=2, metavar=("PATH", "INDEX"), default=[])
    parser.add_argument("--windows-candidates", action="store_true")
    parser.add_argument("--json", type=Path, default=ROOT / "evidence/localization-font-coverage.json")
    parser.add_argument("--log", type=Path, default=ROOT / "evidence/localization-font-coverage.log")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(ParserTests))
        return 0 if result.wasSuccessful() else 1
    try:
        report = audit(args)
        encoded = json.dumps(report, ensure_ascii=False, indent=2) + "\n"
        require(len(encoded.encode("utf-8")) <= 4 * 1024 * 1024, "evidence byte limit")
        for path, value in [(args.json, encoded), (args.log, log_text(report))]:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(value, encoding="utf-8", newline="\n")
        print(log_text(report), end="")
        return 0
    except (AuditError, OSError, ValueError, struct.error) as cause:
        print(f"font_audit_failed: {cause}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
