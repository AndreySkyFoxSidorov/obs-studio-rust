"""Resumable public-UI translation with explicit machine-unreviewed provenance.

Only generated public English UI text is eligible; edited English, nonempty
cells and app-invariant IDs/tests are excluded. No source bodies, logs, device
values or private authored data are transmitted. No credentials are required.
"""
import argparse
from collections import Counter, defaultdict
from concurrent.futures import ThreadPoolExecutor, wait, FIRST_COMPLETED
from datetime import datetime, timezone
import hashlib
import html
import json
from pathlib import Path
import re
import sys
import threading
import time
import tempfile
import unittest
from unittest import mock
from urllib.error import HTTPError
from urllib.parse import urlencode
from urllib.request import Request, urlopen

import localization_catalog as catalog
from package_resources import stage_bytes

ROOT = catalog.ROOT
CSV = catalog.CSV
CACHE = ROOT / "localization/translation-provenance.json"
REPORT = ROOT / "localization/translation-status.json"
SERVICE_GATE = ROOT / "localization/translation-service-gate.json"
ENDPOINT = "https://translate.googleapis.com/translate_a/single"
DOCS = "https://docs.cloud.google.com/translate/docs/languages"
MAX_CACHE = 128 * 1024 * 1024
STOP = threading.Event()
REQUEST_LOCK = threading.Lock()
REQUEST_COUNT = 0
REQUEST_LIMIT = 256
REQUEST_INTERVAL = 2.0
NEXT_REQUEST_AT = 0.0
RATE_REJECTIONS = []
UNCHANGED_REJECTIONS = []
# Exact language/script policy. Unsupported variants are never neighboring
# language aliases. sr-CS in the supplied source is Latin; sr-SP is Cyrillic.
UNSUPPORTED = {"an-ES": "Aragonese exact target unavailable", "kaa": "Karakalpak exact target unavailable",
               "kab-KAB": "Kabyle exact target unavailable", "val-ES": "Valencian exact target unavailable",
               "nn-NO": "Nynorsk exact target unavailable; Bokmal is not a substitute",
               "sr-CS": "Latin Serbian target silently returns Cyrillic in live probe; rejected"}
TARGETS = {"kmr-TR": "ku", "nb-NO": "nb", "sr-SP": "sr", "pt-BR": "pt-BR",
           "pt-PT": "pt-PT", "zh-CN": "zh-CN", "zh-TW": "zh-TW", "fil-PH": "fil", "tl-PH": "tl"}
# Protect Rust, Qt/printf, HTML and URLs from transformation. Their exact token
# multiset must be unchanged after restoration; no positional repair is guessed.
TOKEN = re.compile(r'https?://[^\s<>]+|</?[A-Za-z][^>]*>|\{\{|\}\}|\{[^{}\r\n]*\}|%(?:L?\d+|L?n)|%[A-Za-z][A-Za-z0-9_]*|%(?:\d+\$)?[-+#0 ]*(?:\d+|\*)?(?:\.\d+)?[diuoxXfFeEgGaAcsp%](?![A-Za-z])')
MARKER = re.compile(r"⟦([0-9]{4})⟧")


def sha(text):
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def tokens(text):
    return Counter(TOKEN.findall(text))


def protect(text, index):
    replacements = {}
    def replace(match):
        sentinel = f"⟦{90000000 + index * 1000 + len(replacements):08d}⟧"
        if sentinel in text:
            raise ValueError("Source collides with placeholder sentinel")
        replacements[sentinel] = match[0]
        return sentinel
    return TOKEN.sub(replace, text), replacements


def parse_translation(text, items, replacements, target=None):
    matches = list(MARKER.finditer(text))
    if [match[1] for match in matches] != [f"{i:04d}" for i in range(len(items))]:
        raise ValueError("Translation markers missing/duplicated/reordered; batch rejected")
    result = []
    for i, (english, keys) in enumerate(items):
        value = text[matches[i].end():matches[i + 1].start() if i + 1 < len(matches) else len(text)].strip()
        for sentinel, original in replacements[i].items():
            if value.count(sentinel) != 1:
                raise ValueError("Translation placeholder sentinel missing/duplicated; batch rejected")
            value = value.replace(sentinel, original)
        if re.search(r"⟦9[0-9]{7}⟧", value) or tokens(value) != tokens(english):
            raise ValueError(f"Restored placeholder multiset differs at item {i}; source={english!r}; translated={value!r}; batch rejected")
        if not value or len(value.encode("utf-8")) > 32768:
            raise ValueError(f"Translation empty/oversized at item {i}; source={english!r}")
        # Unchanged English is an unresolved gap, never a machine-coverage pass.
        if value.strip().casefold() == english.strip().casefold():
            UNCHANGED_REJECTIONS.append(dict(target=target, english_sha256=sha(english), keys=keys,
                                             reason="Possible natural cognate/invariant or untranslated service output; left empty pending review"))
            continue
        result.append((english, keys, value))
    return result


def request_translation(target, items):
    global REQUEST_COUNT, NEXT_REQUEST_AT
    protected, replacements = [], []
    for index, (english, keys) in enumerate(items):
        value, mapping = protect(english, index)
        protected.append(f"⟦{index:04d}⟧ {value}")
        replacements.append(mapping)
    query = "\n".join(protected)
    if len(query) > 4000:
        raise ValueError("Batch exceeds 4000 protected characters")
    url = ENDPOINT + "?" + urlencode(dict(client="gtx", sl="en", tl=target, dt="t", q=query))
    for attempt in range(3):
        if STOP.is_set():
            raise RuntimeError("Service rate/security rejection stopped this run")
        try:
            with REQUEST_LOCK:
                if REQUEST_COUNT >= REQUEST_LIMIT:
                    STOP.set()
                    raise RuntimeError("Bounded request budget reached; checkpoint and stop")
                delay = max(0.0, NEXT_REQUEST_AT - time.monotonic())
                NEXT_REQUEST_AT = max(time.monotonic(), NEXT_REQUEST_AT) + REQUEST_INTERVAL
                REQUEST_COUNT += 1
            if delay:
                time.sleep(delay)
            if STOP.is_set():
                raise RuntimeError("Stopped before the next service request")
            request = Request(url, headers={"User-Agent": "SkyLive-public-UI-localization/1"})
            with urlopen(request, timeout=30) as response:
                body = response.read(2 * 1024 * 1024 + 1)
            if len(body) > 2 * 1024 * 1024:
                raise ValueError("Service response byte limit")
            payload = json.loads(body)
            translated = html.unescape("".join(segment[0] for segment in payload[0] if segment and isinstance(segment[0], str)))
            return parse_translation(translated, items, replacements, target=target)
        except HTTPError as error:
            if error.code in {403, 429}:
                RATE_REJECTIONS.append(dict(status=error.code, retry_after=error.headers.get("Retry-After"), at=datetime.now(timezone.utc).isoformat()))
                STOP.set()  # No CAPTCHA, client rotation, cookies or security bypass.
                raise RuntimeError(f"Service HTTP {error.code}; stopped without bypass") from error
            if attempt == 2:
                raise
        except (OSError, TimeoutError):
            if attempt == 2:
                raise
        time.sleep(1 + attempt)
    raise RuntimeError("Service retries exhausted")


def translation_job(target, items, depth=0):
    try:
        return [(*value, "protected_whole_batch") for value in request_translation(target, items)], []
    except ValueError as error:
        # A bad batch is discarded completely. Smaller independent requests may
        # succeed; a rejected singleton remains an explicit gap, never guessed.
        if len(items) > 1 and depth < 8:
            middle = len(items) // 2
            left, left_errors = translation_job(target, items[:middle], depth + 1)
            right, right_errors = translation_job(target, items[middle:], depth + 1)
            return left + right, left_errors + right_errors
        if len(items) == 1 and TOKEN.search(items[0][0]):
            english, keys = items[0]
            pieces = TOKEN.split(english)
            protected_tokens = TOKEN.findall(english)
            fragments = [(piece, [str(index)]) for index, piece in enumerate(pieces) if piece.strip() and re.search(r"[^\W\d_]", piece)]
            try:
                translated = request_translation(target, fragments) if fragments else []
                by_piece = {int(indices[0]): value for _, indices, value in translated}
                if len(by_piece) != len(fragments):
                    raise ValueError("Fragment unchanged/untranslated; field remains empty")
                assembled = ""
                for index, piece in enumerate(pieces):
                    if index in by_piece:
                        leading = piece[:len(piece) - len(piece.lstrip())]
                        trailing = piece[len(piece.rstrip()):]
                        assembled += leading + by_piece[index].strip() + trailing
                    else:
                        assembled += piece
                    if index < len(protected_tokens):
                        assembled += protected_tokens[index]
                if tokens(assembled) != tokens(english):
                    raise ValueError("Fragment restoration changed placeholder contract")
                if assembled.strip().casefold() == english.strip().casefold():
                    raise ValueError("Fragment fallback produced unchanged English; field remains unresolved")
                return [(english, keys, assembled, "token_fragment_fallback_unreviewed")], []
            except ValueError as fragment_error:
                error = fragment_error
        return [], [dict(english_hashes=[sha(text) for text, _ in items], detail=str(error))]


def atomic_bytes(path, data):
    root = path.parent.resolve(strict=True)
    temporary = stage_bytes(root, path, data)
    try:
        temporary.replace(path)
    finally:
        temporary.unlink(missing_ok=True)


def public_snapshot():
    # Official reuse must be drawn from original INI values, never from our
    # assistant-reviewed supplements injected into a fresh generated table.
    languages, generated, provenance, modules = catalog.build_catalog(include_reviewed_source=False)
    public = {key: values["en-US"] for key, values in generated.items() if values.get("en-US", "").strip()}
    return generated, public, provenance


def record(key, locale, english, value, method, source_key=None, target=None, strategy=None):
    return dict(key=key, language=locale, english_sha256=sha(english), value=value,
                value_sha256=sha(value), method=method, source_key=source_key,
                source_locale="en-US", source_contract="Unchanged generated public UI English; private/edited English excluded",
                service=ENDPOINT if method == "machine_unreviewed" else None,
                target=target, reviewed=False, strategy=strategy,
                at=datetime.now(timezone.utc).isoformat())


def eligible(rows, public):
    invariants = catalog.app_invariants()
    return {key: values for key, values in rows.items()
            if key in public and values.get("en-US") == public[key]
            and not (key.startswith("SkyLive.") and values.get("en-US") in invariants)
            and key != "SkyLive.LanguageName"}


def reuse_official(languages, rows, generated, public):
    aliases = defaultdict(lambda: defaultdict(dict))
    for key, values in generated.items():
        if key.startswith(("SkyLive.", "Plugin.")):
            continue
        english = values.get("en-US", "").replace("\r\n", "\n")
        if not english:
            continue
        for locale in languages:
            value = values.get(locale, "")
            if value and tokens(value) == tokens(english):
                aliases[english][locale].setdefault(value, key)
    found = []
    for key, values in eligible(rows, public).items():
        english = values["en-US"]
        for locale in languages:
            if values.get(locale) or locale == "en-US":
                continue
            candidates = aliases[english.replace("\r\n", "\n")].get(locale, {})
            if len(candidates) == 1:
                value, source = next(iter(candidates.items()))
                found.append(record(key, locale, english, value, "official_unambiguous_reuse", source_key=source))
            elif locale == "en-GB":
                found.append(record(key, locale, english, english, "same_language_English_variant"))
    return found


def commit(records, cache, public):
    languages, current = catalog.read_csv(CSV)
    applied = 0
    for item in records:
        if item["method"].startswith("rejected_"):
            continue
        key, locale = item["key"], item["language"]
        if key not in current or locale not in languages or current[key].get(locale):
            continue
        english = current[key].get("en-US", "")
        if key not in public or public[key] != english or sha(english) != item["english_sha256"]:
            continue
        if sha(item["value"]) != item["value_sha256"] or tokens(item["value"]) != tokens(english):
            continue
        current[key][locale] = item["value"]
        cache[key + "\t" + locale] = item
        applied += 1
    data = catalog.serialize_csv(languages, current)
    if len(data) > 32 * 1024 * 1024:
        raise ValueError("Translated CSV exceeds 32 MiB")
    metadata = json.dumps(dict(schema=1, language_support_reference=DOCS, cells=cache), ensure_ascii=False, separators=(",", ":")).encode()
    if len(metadata) > MAX_CACHE:
        raise ValueError("Provenance budget exceeds 128 MiB")
    # Cache precedes CSV: an interrupted publication can replay verified values.
    atomic_bytes(CACHE, metadata)
    if data != CSV.read_bytes():
        atomic_bytes(CSV, data)
    return applied


def repair_owned_machine_invariants(cache, public):
    """Repair only our hash-matching malformed output and approved invariants.

    Every other nonempty/source/user cell remains authoritative. A previous
    value hash in the durable correction supports replay after CSV failure.
    """
    languages, current = catalog.read_csv(CSV)
    approved = set(catalog.reviewed_source_seeds()["language_independent_english"])
    repairs = []
    for identifier, item in list(cache.items()):
        key, locale = item["key"], item["language"]
        row = current.get(key)
        if not row or locale not in row:
            continue
        english = row.get("en-US", "")
        value = row[locale]
        if english not in approved or public.get(key) != english or sha(english) != item["english_sha256"]:
            continue
        owned_bad = item["method"] == "machine_unreviewed" and value == item["value"] and sha(value) == item["value_sha256"] and tokens(value) != tokens(english)
        replay = item.get("repaired_from_machine_sha256") == sha(value) and item["method"] == "invariant_not_translated"
        if not owned_bad and not replay:
            continue
        corrected = record(key, locale, english, english, "invariant_not_translated", source_key="localization/required-source-seeds.json", strategy="Repair own malformed machine filename/format codes using explicit language-independent whitelist")
        corrected["repaired_from_machine_sha256"] = sha(value)
        cache[identifier] = corrected
        row[locale] = english
        repairs.append(dict(key=key, language=locale, previous_value=value, previous_sha256=sha(value), corrected_value=english, corrected_sha256=sha(english), method=corrected["method"]))
    if repairs:
        encoded = json.dumps(dict(schema=1, language_support_reference=DOCS, cells=cache), ensure_ascii=False, separators=(",", ":")).encode()
        data = catalog.serialize_csv(languages, current)
        if len(encoded) > MAX_CACHE or len(data) > 32 * 1024 * 1024:
            raise ValueError("Repair publication exceeds resource budgets")
        atomic_bytes(CACHE, encoded)
        atomic_bytes(CSV, data)
        atomic_bytes(ROOT / "localization/placeholder-repairs.json", (json.dumps(repairs, ensure_ascii=False, indent=2) + "\n").encode())
    return repairs


def reject_owned_unchanged_fragments(cache, public):
    languages, current = catalog.read_csv(CSV)
    approved = set(catalog.reviewed_source_seeds()["language_independent_english"])
    rejected = []
    for identifier, item in list(cache.items()):
        row = current.get(item["key"])
        if not row or item["language"] not in row:
            continue
        english, value = row.get("en-US", ""), row[item["language"]]
        if item["method"] not in {"machine_unreviewed", "rejected_unchanged_machine_output"} or item.get("strategy") != "token_fragment_fallback_unreviewed":
            continue
        if english in approved or public.get(item["key"]) != english or sha(english) != item["english_sha256"]:
            continue
        if value != item["value"] or sha(value) != item["value_sha256"] or value != english or TOKEN.fullmatch(english) is None:
            continue
        cache[identifier] = dict(item, method="rejected_unchanged_machine_output", rejection="Pure protected-token fallback produced no linguistic translation; left empty")
        row[item["language"]] = ""
        rejected.append(dict(key=item["key"], language=item["language"], previous_value=value, previous_sha256=sha(value), reason=cache[identifier]["rejection"]))
    if rejected:
        metadata = json.dumps(dict(schema=1, language_support_reference=DOCS, cells=cache), ensure_ascii=False, separators=(",", ":")).encode()
        atomic_bytes(CACHE, metadata)
        atomic_bytes(CSV, catalog.serialize_csv(languages, current))
        atomic_bytes(ROOT / "localization/unchanged-output-qc.json", (json.dumps(rejected, ensure_ascii=False, indent=2) + "\n").encode())
    return rejected


def build_jobs(languages, rows, public, batch_chars, selected=None):
    jobs = []
    exclusions = Counter()
    candidates = eligible(rows, public)
    for locale in languages:
        if locale in {"en-US", "en-GB"} or (selected and locale not in selected):
            continue
        if locale in UNSUPPORTED:
            exclusions[locale] = sum(not row.get(locale) for row in candidates.values())
            continue
        target = TARGETS.get(locale, locale.split("-")[0])
        grouped = defaultdict(list)
        for key, values in sorted(candidates.items(), key=lambda item: (0 if item[0].startswith("SkyLive.") else 1 if item[0].startswith("Plugin.") else 2, item[0])):
            if not values.get(locale):
                grouped[values["en-US"]].append(key)
        batch, size = [], 0
        for english, keys in grouped.items():
            protected, _ = protect(english, len(batch))
            cost = len(protected) + 9
            if cost > batch_chars:
                exclusions["oversized_public_string"] += len(keys)
                continue
            if batch and size + cost > batch_chars:
                jobs.append((locale, target, batch))
                batch, size = [], 0
            batch.append((english, keys))
            size += cost
        if batch:
            jobs.append((locale, target, batch))
    counts = Counter()
    interleaved = []
    for job in jobs:
        index = counts[job[0]]
        counts[job[0]] += 1
        interleaved.append((index, languages.index(job[0]), job))
    return [job for _, _, job in sorted(interleaved)], dict(exclusions)


def status(generated, errors=None):
    languages, rows = catalog.read_csv(CSV)
    data = catalog.coverage(languages, rows, generated)
    invariant = catalog.app_invariants()
    groups = {"frontend": lambda key, value: not key.startswith(("Module.", "SkyLive.", "Plugin.")),
              "modules": lambda key, value: key.startswith("Module."),
              "plugin_metadata": lambda key, value: key.startswith("Plugin."),
              "app_user_facing": lambda key, value: key.startswith("SkyLive.") and value.get("en-US") not in invariant,
              "app_invariant": lambda key, value: key.startswith("SkyLive.") and value.get("en-US") in invariant}
    data["all_locale_missing_by_namespace"] = {name: {language: sum(not value.get(language) for key, value in rows.items() if predicate(key, value)) for language in languages} for name, predicate in groups.items()}
    data.update(unsupported_exact_targets=UNSUPPORTED, language_support_reference=DOCS,
                machine_review="Machine-produced cells are unreviewed; nonempty data is not native linguistic acceptance", errors=errors or [],
                requests_this_run=REQUEST_COUNT, request_limit=REQUEST_LIMIT, global_request_interval_seconds=REQUEST_INTERVAL,
                rate_or_security_rejections=RATE_REJECTIONS, csv_sha256=hashlib.sha256(CSV.read_bytes()).hexdigest(),
                unchanged_service_outputs=UNCHANGED_REJECTIONS,
                updated_at=datetime.now(timezone.utc).isoformat())
    if SERVICE_GATE.exists():
        data["network_service_gate"] = json.loads(SERVICE_GATE.read_text(encoding="utf-8"))
    atomic_bytes(REPORT, (json.dumps(data, ensure_ascii=False, indent=2) + "\n").encode())
    return data


class Tests(unittest.TestCase):
    def test_exact_placeholder_restore_and_multisets(self):
        original = "Source {name}: {} / {:.3} / %1 / %L2 / %n <b>https://example.org</b>"
        protected, mapping = protect(original, 0)
        text = "⟦0000⟧ Quelle " + protected.removeprefix("Source ")
        value = parse_translation(text, [(original, ["K"])], [mapping])[0][2]
        self.assertEqual(tokens(value), tokens(original))
        self.assertIn("Quelle", value)
    def test_bad_alignment_and_missing_placeholder_reject_whole_batch(self):
        original = "Value {}"
        protected, mapping = protect(original, 0)
        with self.assertRaises(ValueError):
            parse_translation("⟦0001⟧ " + protected, [(original, ["K"])], [mapping])
        with self.assertRaises(ValueError):
            parse_translation("⟦0000⟧ Wert", [(original, ["K"])], [mapping])
    def test_unchanged_english_is_not_machine_coverage(self):
        self.assertEqual(parse_translation("⟦0000⟧ Save", [("Save", ["K"])], [{}]), [])
    def test_no_private_english_or_invariant_admission(self):
        rows = {"A": {"en-US": "private edit"}, "B": {"en-US": "Public"}, "SkyLive.X": {"en-US": "preview"}}
        self.assertEqual(set(eligible(rows, {"A": "Original", "B": "Public", "SkyLive.X": "preview"})), {"B"})
    def test_official_reuse_requires_unambiguous_same_language_and_placeholders(self):
        generated = {"A": {"en-US": "Save", "de-DE": "Speichern"}, "B": {"en-US": "Save", "de-DE": "Sichern"}, "C": {"en-US": "Value {}", "de-DE": "Wert"}}
        rows = {"SkyLive.X": {"en-US": "Save"}, "SkyLive.Y": {"en-US": "Value {}"}}
        found = reuse_official(["en-US", "de-DE"], rows, generated, {key: value["en-US"] for key, value in rows.items()})
        self.assertEqual(found, [])

    def test_commit_preserves_edits_and_replays_cache_after_csv_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            csv_path, cache_path = Path(directory) / "translations.csv", Path(directory) / "provenance.json"
            rows = {"A": {"en-US": "Save", "de-DE": "Edited"}, "B": {"en-US": "Open", "de-DE": ""}}
            original = catalog.serialize_csv(["en-US", "de-DE"], rows)
            csv_path.write_bytes(original)
            records = [record("A", "de-DE", "Save", "Speichern", "machine_unreviewed"), record("B", "de-DE", "Open", "Öffnen", "machine_unreviewed")]
            real_atomic = atomic_bytes
            def fail_csv(path, data):
                if path == csv_path:
                    raise OSError("fixture CSV publication failure")
                return real_atomic(path, data)
            with mock.patch(__name__ + ".CSV", csv_path), mock.patch(__name__ + ".CACHE", cache_path):
                with mock.patch(__name__ + ".atomic_bytes", side_effect=fail_csv):
                    with self.assertRaises(OSError):
                        commit(records, {}, {"A": "Save", "B": "Open"})
                self.assertEqual(csv_path.read_bytes(), original)
                cache = json.loads(cache_path.read_text(encoding="utf-8"))["cells"]
                self.assertNotIn("A\tde-DE", cache)
                self.assertEqual(commit(list(cache.values()), cache, {"A": "Save", "B": "Open"}), 1)
                _, final = catalog.read_csv(csv_path)
                self.assertEqual(final["A"]["de-DE"], "Edited")
                self.assertEqual(final["B"]["de-DE"], "Öffnen")
                self.assertEqual(sorted(p.name for p in Path(directory).iterdir()), ["provenance.json", "translations.csv"])

    def test_request_budget_stops_before_network(self):
        STOP.clear()
        try:
            with mock.patch(__name__ + ".REQUEST_LIMIT", 0), mock.patch(__name__ + ".REQUEST_COUNT", 0), mock.patch(__name__ + ".urlopen") as network:
                with self.assertRaises(RuntimeError):
                    request_translation("de", [("Save", ["A"])])
                network.assert_not_called()
                self.assertTrue(STOP.is_set())
        finally:
            STOP.clear()

    def test_repair_only_own_hash_matching_approved_invariant(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "localization").mkdir()
            csv_path, cache_path = root / "localization/translations.csv", root / "localization/provenance.json"
            english, malformed = "%CCYY-%MM-%DD %hh-%mm-%ss", "%CCAA-%MM-%DD %hh-%mm-%ss"
            rows = {"Owned": {"en-US": english, "es-ES": malformed}, "Edited": {"en-US": english, "es-ES": "User edit"}}
            csv_path.write_bytes(catalog.serialize_csv(["en-US", "es-ES"], rows))
            cache = {key + "\tes-ES": record(key, "es-ES", english, malformed, "machine_unreviewed") for key in rows}
            with mock.patch(__name__ + ".CSV", csv_path), mock.patch(__name__ + ".CACHE", cache_path), mock.patch(__name__ + ".ROOT", root), mock.patch.object(catalog, "reviewed_source_seeds", return_value={"language_independent_english": [english]}):
                repairs = repair_owned_machine_invariants(cache, {key: english for key in rows})
                self.assertEqual([(item["key"], item["language"]) for item in repairs], [("Owned", "es-ES")])
                _, result = catalog.read_csv(csv_path)
                self.assertEqual(result["Owned"]["es-ES"], english)
                self.assertEqual(result["Edited"]["es-ES"], "User edit")
                self.assertEqual(repair_owned_machine_invariants(cache, {key: english for key in rows}), [])

    def test_pure_token_fallback_cannot_be_a_fake_translation(self):
        with mock.patch(__name__ + ".request_translation", side_effect=ValueError("fixture bad batch")):
            values, errors = translation_job("ba", [("<cleared>", ["K"])])
        self.assertEqual(values, [])
        self.assertEqual(len(errors), 1)
        self.assertIn("unchanged English", errors[0]["detail"])


def main():
    global REQUEST_LIMIT, REQUEST_INTERVAL
    sys.stdout.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true")
    parser.add_argument("--reuse-only", action="store_true", help="Offline invariant/official/cache replay only; never send a network request")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--workers", type=int, default=1)
    parser.add_argument("--batch-chars", type=int, default=4000)
    parser.add_argument("--max-batches", type=int)
    parser.add_argument("--language", action="append")
    parser.add_argument("--max-requests", type=int, default=256)
    parser.add_argument("--request-interval", type=float, default=2.0)
    args = parser.parse_args()
    if args.self_test:
        result = unittest.main(argv=[sys.argv[0]], exit=False)
        raise SystemExit(0 if result.result.wasSuccessful() else 1)
    if not 1 <= args.workers <= 4 or not 100 <= args.batch_chars <= 4000:
        parser.error("workers must be 1..4 and batch-chars 100..4000")
    if not 1 <= args.max_requests <= 4096 or args.request_interval < 2.0:
        parser.error("max-requests must be 1..4096; global request interval must be at least2 seconds")
    REQUEST_LIMIT, REQUEST_INTERVAL = args.max_requests, args.request_interval
    generated, public, provenance = public_snapshot()
    languages, rows = catalog.read_csv(CSV)
    invariant = catalog.app_invariants()
    invariant_records = []
    for key, values in rows.items():
        english = values.get("en-US", "")
        if key.startswith("SkyLive.") and english in invariant:
            public.setdefault(key, english)  # Reviewed invariant dictionary; never sent to service.
            invariant_records.extend(record(key, locale, english, english, "invariant_not_translated", strategy=invariant[english]) for locale in languages if not values.get(locale))
    source_seeds = catalog.reviewed_source_seeds()
    for key, values in rows.items():
        english = values.get("en-US", "")
        if public.get(key) != english:
            continue
        for locale, value in source_seeds["values"].get(english, {}).items():
            if not values.get(locale):
                invariant_records.append(record(key, locale, english, value, "assistant_reviewed_required_source", source_key="localization/required-source-seeds.json", strategy="Explicit reviewed technical term/cognate/translation; not native-linguist reviewed"))
        if english in source_seeds["language_independent_english"]:
            invariant_records.extend(record(key, locale, english, english, "invariant_not_translated", source_key="localization/required-source-seeds.json", strategy="Explicit source codec/brand/format-template whitelist; not a linguistic translation") for locale in languages if not values.get(locale))
    official = reuse_official(languages, rows, generated, public)
    cache = {}
    if CACHE.exists():
        if CACHE.stat().st_size > MAX_CACHE:
            raise ValueError("Provenance cache exceeds 128 MiB")
        cache = json.loads(CACHE.read_text(encoding="utf-8"))["cells"]
    print(json.dumps(dict(total_missing=sum(not row.get(lang) for row in rows.values() for lang in languages), official_reuse_cells=len(official), invariants_excluded=len(catalog.app_invariants()))), flush=True)
    if not args.run and not args.reuse_only:
        jobs, excluded = build_jobs(languages, rows, public, args.batch_chars, args.language)
        print(json.dumps(dict(batches=len(jobs), exclusions=excluded)), flush=True)
        return
    repairs = repair_owned_machine_invariants(cache, public)
    if repairs:
        print(json.dumps(dict(repaired_owned_machine_cells=len(repairs))), flush=True)
    rejected = reject_owned_unchanged_fragments(cache, public)
    if rejected:
        print(json.dumps(dict(rejected_owned_unchanged_cells=len(rejected))), flush=True)
    initial = commit(list(cache.values()) + invariant_records + official, cache, public)
    print(f"Reused/replayed {initial} cells", flush=True)
    if args.reuse_only:
        summary = status(generated)
        print(json.dumps(dict(offline=True, filled=initial, remaining=sum(summary["missing_by_locale"].values()), network_requests=REQUEST_COUNT)), flush=True)
        return
    languages, rows = catalog.read_csv(CSV)
    jobs, excluded = build_jobs(languages, rows, public, args.batch_chars, args.language)
    if args.max_batches is not None:
        jobs = jobs[:args.max_batches]
    if len(jobs) > 8192:
        raise ValueError("Batch queue exceeds bounded 8192-job profile")
    print(json.dumps(dict(batches=len(jobs), exclusions=excluded)), flush=True)
    errors, pending, completed, applied = [], [], 0, initial
    last_checkpoint = time.monotonic()
    iterator = iter(jobs)
    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        running = {}
        def submit():
            try:
                job = next(iterator)
            except StopIteration:
                return
            running[pool.submit(translation_job, job[1], job[2])] = job
        for _ in range(args.workers * 2):
            submit()
        while running:
            done, _ = wait(running, timeout=10, return_when=FIRST_COMPLETED)
            for future in done:
                locale, target, items = running.pop(future)
                try:
                    values, qc_errors = future.result()
                    errors.extend(dict(language=locale, target=target, **error) for error in qc_errors)
                    for english, keys, value, strategy in values:
                        pending.extend(record(key, locale, english, value, "machine_unreviewed", target=target, strategy=strategy) for key in keys)
                except Exception as error:
                    errors.append(dict(language=locale, target=target, english_hashes=[sha(text) for text, _ in items], detail=str(error)))
                completed += 1
                if not STOP.is_set():
                    submit()
            if time.monotonic() - last_checkpoint >= 20 or not running:
                applied += commit(pending, cache, public)
                pending.clear()
                summary = status(generated, errors)
                print(json.dumps(dict(completed=completed, batches=len(jobs), filled=applied, remaining=sum(summary["missing_by_locale"].values()), failed_batches=len(errors), stopped=STOP.is_set())), flush=True)
                last_checkpoint = time.monotonic()
    print(json.dumps(dict(finished=True, completed=completed, filled=applied, failed_batches=len(errors), stopped=STOP.is_set())), flush=True)


if __name__ == "__main__":
    main()
