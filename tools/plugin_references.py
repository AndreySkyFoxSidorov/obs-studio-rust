#!/usr/bin/env python3
"""Fetch and statically index the explicitly listed OBS references. Never builds code.

Python standard library + Git only. At most two clones run concurrently. Existing
checkouts are never reset, pulled, cleaned, or overwritten. Git submodules and LFS
objects are not downloaded. Every revision and scan limit is recorded.
"""
from __future__ import annotations

import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
from urllib.parse import unquote, urlparse
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parents[1]
LIST = ROOT / "doc/OBS_Studio_Open_Source_Plugins.md"
DEST = ROOT / "doc/plugins"
MAX_SCAN_BYTES = 64 * 1024 * 1024
MAX_FILE_BYTES = 2 * 1024 * 1024
MAX_FILES = 100_000
MAX_EVIDENCE = 4096
SOURCE_EXT = {".c", ".h", ".cc", ".cpp", ".cxx", ".hpp", ".m", ".mm",
              ".rs", ".cs", ".py", ".lua", ".js", ".ts", ".effect", ".glsl", ".hlsl"}
DEPENDENCY_NAMES = {"cmakelists.txt", "cargo.toml", "package.json", "vcpkg.json",
                    "conanfile.py", "conanfile.txt", "requirements.txt", "buildspec.json",
                    "meson.build", "go.mod", ".gitmodules"}
LINK = re.compile(r"\[([^\]]+)\]\((https?://[^\s)]+)\)")
API = re.compile(r"\b((?:obs|gs|bmalloc|bfree|blog)_[A-Za-z0-9_]+)\s*\(")
ID = re.compile(r'(?:\.id\s*=|\bid\s*=|["\']id["\']\s*:)\s*["\']([^"\'\r\n]+)["\']')
REGISTER = re.compile(r"\bobs_register_[A-Za-z0-9_]+\s*\(")
SYMBOL_ID = re.compile(r"\.id\s*=\s*([A-Za-z_]\w*)")
STRING_MACRO = re.compile(r'^\s*#\s*define\s+([A-Za-z_]\w*)\s+"([^"\r\n]+)"')
DEPENDENCY = re.compile(r"\b(?:find_package|FetchContent_Declare|ExternalProject_Add|add_subdirectory|target_link_libraries)\s*\(|(?:FFmpeg|CEF|CUDA|TensorRT|OpenCV|NDI|VST|onnxruntime|GStreamer|Qt[56])", re.I)


def repository_url(url: str):
    """Return only an actual github owner/repository URL; topics are not repos."""
    parsed = urlparse(url)
    parts = [unquote(x) for x in parsed.path.strip("/").split("/")]
    if parsed.hostname != "github.com" or len(parts) < 2 or parts[0].lower() in {"topics", "orgs", "users", "search", "collections"}:
        return None
    owner, repo = parts[:2]
    repo = repo.removesuffix(".git")
    if not all(re.fullmatch(r"[A-Za-z0-9_.-]+", value) and value not in {".", ".."} for value in (owner, repo)):
        raise ValueError(f"Unsafe GitHub repository URL: {url}")
    return {"key": f"{owner.lower()}/{repo.lower()}", "owner": owner, "repo": repo,
            "url": f"https://github.com/{owner}/{repo}", "directory": f"{owner}--{repo}"}


def parse_catalog(text: str):
    entries = []
    section = ""
    counts = Counter()
    for number, line in enumerate(text.splitlines(), 1):
        if line.startswith("## "):
            section = line[3:]
        if not line.startswith("| ") or not (match := LINK.search(line)):
            continue
        cells = line.split("|")
        if cells[1].strip().isdigit():
            category, ordinal = "project", int(cells[1].strip())
        elif section == "Встроенные модули":
            category, ordinal = "builtin", counts["builtin"] + 1
        elif section == "Скрипты, каталоги и разработка":
            category, ordinal = "resource", counts["resource"] + 1
        else:
            continue
        counts[category] += 1
        name, url = match.groups()
        repo = repository_url(url)
        subpath = None
        if repo:
            parts = urlparse(url).path.strip("/").split("/")
            if len(parts) > 4 and parts[2] in {"tree", "blob"}:
                subpath = "/".join(parts[4:])
                if any(p in {".", ".."} for p in subpath.split("/")):
                    raise ValueError(f"Unsafe linked path: {url}")
        entries.append({"id": f"{category}-{ordinal:03d}", "category": category,
                        "number": ordinal, "name": name, "url": url, "section": section,
                        "description": cells[-2].strip(), "source_line": number,
                        "repository": repo["key"] if repo else None, "subpath": subpath})
    if counts != Counter(project=127, builtin=16, resource=11):
        raise ValueError(f"Catalog count changed; review parser/list before fetching: {dict(counts)}")
    if [x["number"] for x in entries if x["category"] == "project"] != list(range(1, 128)):
        raise ValueError("Numbered projects must be exactly 1..127")
    return entries


def git(args, cwd=None, timeout=300):
    environment = dict(os.environ, GIT_TERMINAL_PROMPT="0", GIT_LFS_SKIP_SMUDGE="1",
                       GCM_INTERACTIVE="Never")
    config = ["-c", "credential.interactive=false", "-c", "core.hooksPath=NUL",
              "-c", "filter.lfs.smudge=", "-c", "filter.lfs.required=false"]
    if cwd is not None:
        config += ["-c", f"safe.directory={Path(cwd).resolve().as_posix()}"]
    result = subprocess.run(["git", *config, *args], cwd=cwd, env=environment,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout,
                            encoding="utf-8", errors="replace")
    if result.returncode:
        raise RuntimeError(f"git {' '.join(args)}\n{result.stdout}{result.stderr}")
    return result.stdout, result.stderr


def origin_matches(actual, requested, local_source):
    remote = repository_url(actual)
    if remote and remote["key"] == requested["key"]:
        return True
    return requested["key"] == "obsproject/obs-studio" and actual.rstrip("/") == local_source.resolve().as_uri().rstrip("/")


def fetch_repository(repo, destination, local_source, verify_only=False, previous=None):
    path = destination / repo["directory"]
    record = {**repo, "path": path.relative_to(ROOT).as_posix() if path.is_relative_to(ROOT) else str(path),
              "status": "existing" if path.exists() else "missing", "attempted": False}
    # Preserve evidence from the original download during subsequent offline audits.
    if previous:
        for key in ("provenance", "git_messages", "first_observed_commit", "first_observed_at"):
            if key in previous:
                record[key] = previous[key]
    log = destination / "logs" / (repo["directory"] + ".txt")
    try:
        if path.exists():
            if path.is_symlink() or not (path / ".git").exists():
                raise ValueError("Existing destination is not an ordinary Git checkout; preserved without changes")
            origin = git(["remote", "get-url", "origin"], cwd=path)[0].strip()
            record["origin"] = origin
            if not origin_matches(origin, repo, local_source):
                raise ValueError(f"Existing origin differs from requested repository: {origin}")
            dirty = git(["status", "--porcelain", "--untracked-files=normal"], cwd=path)[0]
            record["working_tree_clean"] = not dirty
            if dirty:
                record["dirty_status"] = dirty
                raise ValueError("Existing checkout has changes; no reset/pull/overwrite was attempted")
        elif verify_only:
            record["detail"] = "Not present; --verify-only does not access the network"
            return record
        else:
            record["attempted"] = True
            source = repo["url"] + ".git"
            local_provenance = None
            if repo["key"] == "obsproject/obs-studio" and (local_source / ".git").exists():
                origin = git(["remote", "get-url", "origin"], cwd=local_source)[0].strip()
                if repository_url(origin) and repository_url(origin)["key"] == repo["key"]:
                    dirty = git(["status", "--porcelain", "--untracked-files=no"], cwd=local_source)[0]
                    if not dirty:
                        source = local_source.resolve().as_uri()
                        local_provenance = {"source": str(local_source.resolve()), "origin": origin,
                                            "commit": git(["rev-parse", "HEAD"], cwd=local_source)[0].strip(),
                                            "tracked_working_tree_clean": True,
                                            "method": "git clone --depth 1 file:// (committed source only)"}
            # A dedicated new temporary directory avoids ever overwriting an existing destination.
            # A failed clone is retained for diagnosis; a rerun can use a fresh temporary path.
            staging = Path(tempfile.mkdtemp(prefix=".clone-", dir=destination))
            record["staging_path"] = str(staging)
            stdout, stderr = git(["clone", "--quiet", "--depth", "1", "--no-recurse-submodules", source, str(staging)])
            log.write_text(stdout + stderr, encoding="utf-8")
            if path.exists():
                raise ValueError("Destination appeared during clone; staging preserved, destination untouched")
            staging.rename(path)
            record.pop("staging_path", None)
            record["status"] = "downloaded"
            if local_provenance:
                record["provenance"] = local_provenance
            if stderr:
                record["git_messages"] = stderr
        record["origin"] = git(["remote", "get-url", "origin"], cwd=path)[0].strip()
        record["commit"] = git(["rev-parse", "HEAD"], cwd=path)[0].strip()
        if record.get("provenance") and record["provenance"]["commit"] != record["commit"]:
            raise ValueError("Local-source provenance HEAD differs from checkout HEAD; preserved for manual review")
        record["shallow"] = git(["rev-parse", "--is-shallow-repository"], cwd=path)[0].strip() == "true"
        record["branch"] = git(["rev-parse", "--abbrev-ref", "HEAD"], cwd=path)[0].strip()
        record.setdefault("first_observed_commit", record["commit"])
        record.setdefault("first_observed_at", datetime.now(timezone.utc).isoformat())
        record["working_tree_clean"] = not git(["status", "--porcelain", "--untracked-files=normal"], cwd=path)[0]
        record["audit"] = audit_repository(path)
    except (OSError, subprocess.SubprocessError, RuntimeError, ValueError) as error:
        record["status"] = "unavailable"
        record["detail"] = str(error)
        log.write_text(str(error), encoding="utf-8")
    return record


def audit_repository(path):
    raw = git(["ls-files", "-z"], cwd=path)[0]
    names = raw.split("\0")[:-1]
    if len(names) > MAX_FILES:
        raise ValueError(f"Tracked-file count {len(names)} exceeds auditor limit {MAX_FILES}")
    extensions = Counter()
    licenses, dependencies, registrations, identifiers, api_sites, submodules = [], [], [], [], {}, []
    dependency_manifests, symbolic_ids, string_macros, lfs_pointers, declarations = [], [], [], [], []
    scanned_bytes = scanned_files = 0
    skipped = []
    # Gitlinks do not appear as files and are deliberately not initialized.
    tree = git(["ls-tree", "-r", "HEAD"], cwd=path)[0]
    for line in tree.splitlines():
        if line.startswith("160000 commit "):
            commit, name = line[len("160000 commit "):].split("\t", 1)
            submodules.append({"path": name, "commit": commit,
                               "initialized": (path / name / ".git").exists()})
    modules = []
    for name in names:
        relative = Path(name)
        file = path / relative
        base = relative.name.lower()
        ext = relative.suffix.lower()
        extensions[ext or "<none>"] += 1
        if re.match(r"^(?:licen[cs]e|copying|copyright|notice)(?:[._-]|$)", base):
            licenses.append(name)
        if base == ".gitmodules" and file.is_file() and not file.is_symlink():
            raw_modules = file.read_bytes()
            modules.append({"path": name, "text": raw_modules.decode("utf-8", errors="replace"),
                            "sha256": hashlib.sha256(raw_modules).hexdigest()})
        # LFS pointers can describe binary assets, not only source extensions.
        if file.is_file() and not file.is_symlink() and file.stat().st_size <= 1024:
            pointer = file.read_bytes()
            if pointer.startswith(b"version https://git-lfs.github.com/spec/v1\n"):
                lfs_pointers.append({"path": name, "pointer": pointer.decode("utf-8", errors="replace"),
                                     "downloaded": False})
        dependency_file = base in DEPENDENCY_NAMES or ext in {".cmake", ".csproj", ".vcxproj"}
        if dependency_file:
            dependency_manifests.append(name)
        if ext not in SOURCE_EXT and not dependency_file:
            continue
        if not file.is_file() or file.is_symlink():
            skipped.append({"path": name, "reason": "not ordinary checked-out file (gitlink/symlink)"})
            continue
        size = file.stat().st_size
        if size > MAX_FILE_BYTES or scanned_bytes + size > MAX_SCAN_BYTES:
            skipped.append({"path": name, "reason": "scan byte budget", "bytes": size})
            continue
        contents = file.read_bytes()
        scanned_bytes += len(contents)
        scanned_files += 1
        text = contents.decode("utf-8", errors="replace")
        for line_no, line in enumerate(text.splitlines(), 1):
            if REGISTER.search(line) and len(registrations) < MAX_EVIDENCE:
                registrations.append({"path": name, "line": line_no, "text": line.strip()[:1500]})
            if "OBS_DECLARE_MODULE" in line and len(declarations) < MAX_EVIDENCE:
                declarations.append({"path": name, "line": line_no, "text": line.strip()[:1500]})
            if (macro := STRING_MACRO.match(line)) and len(string_macros) < MAX_EVIDENCE:
                string_macros.append({"symbol": macro[1], "value": macro[2], "path": name, "line": line_no})
            for match in SYMBOL_ID.finditer(line):
                if len(symbolic_ids) < MAX_EVIDENCE:
                    symbolic_ids.append({"symbol": match[1], "path": name, "line": line_no})
            for match in ID.finditer(line):
                if len(identifiers) < MAX_EVIDENCE:
                    identifiers.append({"id": match[1], "path": name, "line": line_no,
                                        "classification": "literal id assignment candidate; not semantic validation"})
            for match in API.finditer(line):
                function = match[1]
                if function not in api_sites and len(api_sites) >= MAX_EVIDENCE:
                    continue
                value = api_sites.setdefault(function, {"count": 0, "sites": []})
                value["count"] += 1
                if len(value["sites"]) < 4:
                    value["sites"].append({"path": name, "line": line_no})
            if dependency_file and DEPENDENCY.search(line) and len(dependencies) < MAX_EVIDENCE:
                dependencies.append({"path": name, "line": line_no, "text": line.strip()[:2000]})
    for identifier in symbolic_ids:
        identifier["candidate_definitions"] = [m for m in string_macros if m["symbol"] == identifier["symbol"]][:16]
    evidence_limits = {"registration_calls": len(registrations) == MAX_EVIDENCE,
                       "literal_ids": len(identifiers) == MAX_EVIDENCE,
                       "symbolic_ids": len(symbolic_ids) == MAX_EVIDENCE,
                       "string_macros": len(string_macros) == MAX_EVIDENCE,
                       "dependency_cues": len(dependencies) == MAX_EVIDENCE,
                       "api_names": len(api_sites) == MAX_EVIDENCE}
    return {"tracked_files": len(names), "tracked_extensions": dict(sorted(extensions.items())),
            "source_extensions": {key: value for key, value in sorted(extensions.items()) if key in SOURCE_EXT},
            "license_paths": sorted(licenses), "gitmodules": modules, "gitlinks": submodules,
            "registration_calls": registrations, "literal_id_candidates": identifiers,
            "symbolic_id_candidates": symbolic_ids, "module_declarations": declarations,
            "dependency_manifests": sorted(dependency_manifests), "lfs_pointers": lfs_pointers,
            "api_usage": dict(sorted(api_sites.items())), "dependency_cues": dependencies,
            "scanned_files": scanned_files, "scanned_bytes": scanned_bytes, "skipped_files": skipped,
            "limits": {"files": MAX_FILES, "file_bytes": MAX_FILE_BYTES, "total_bytes": MAX_SCAN_BYTES,
                       "evidence_per_kind": MAX_EVIDENCE},
            "evidence_limit_reached": evidence_limits,
            "method": "Static lexical evidence only; no compilation, execution, dependency/submodule download, ABI or license compatibility assertion"}


def external_status(url):
    try:
        with urlopen(Request(url, method="HEAD", headers={"User-Agent": "SkyLive-source-reference-auditor/1"}), timeout=20) as response:
            return {"status": "existing", "reference_only": True, "http_status": response.status,
                    "resolved_url": response.url, "detail": "External catalog/topic link; not a Git repository, not recursively crawled"}
    except Exception as error:
        return {"status": "unavailable", "reference_only": True, "detail": str(error)}


def atomic_write(path, text):
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(text, encoding="utf-8")
    temporary.replace(path)


def write_outputs(destination, catalog, repositories, external, source_hash):
    rows = []
    for entry in catalog:
        row = dict(entry)
        if entry["repository"]:
            repo = repositories[entry["repository"]]
            row.update(status=repo["status"], path=repo["path"], commit=repo.get("commit"))
            if row["status"] in {"downloaded", "existing"} and entry["subpath"]:
                linked = ROOT / repo["path"] / entry["subpath"]
                row["path"] += "/" + entry["subpath"]
                row["status"] = "builtin-path" if linked.exists() else "missing"
                if not linked.exists():
                    row["detail"] = "Linked path absent at recorded commit; submodules were not initialized"
            if repo.get("detail"):
                row["detail"] = repo["detail"]
        else:
            row.update(external.get(entry["url"], {"status": "missing", "reference_only": True,
                                                    "detail": "External URL was not probed in verification mode"}))
        rows.append(row)
    manifest = {"schema": 1, "generated_at": datetime.now(timezone.utc).isoformat(),
                "source_list": LIST.relative_to(ROOT).as_posix(), "source_list_sha256": source_hash,
                "script_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                "policy": {"concurrent_clones": 2, "depth": 1, "submodules": False, "lfs": False,
                           "execute_references": False, "update_existing": False, "source_reference_only": True},
                "counts": {"entries": len(rows), "repositories": len(repositories),
                           "entry_status": dict(Counter(x["status"] for x in rows)),
                           "repository_status": dict(Counter(x["status"] for x in repositories.values()))},
                "entries": rows, "repositories": [repositories[key] for key in sorted(repositories)]}
    atomic_write(destination / "manifest.json", json.dumps(manifest, ensure_ascii=False, indent=2) + "\n")
    lines = ["# OBS plugin source references", "", "Generated by `python tools/plugin_references.py`. These are reference sources; no downloaded code was executed.",
             "", f"Catalog: **127 projects + 16 built-in modules + 11 resources**; distinct repositories: **{len(repositories)}**.",
             "", "Existing checkouts are preserved. No submodules, LFS payloads, release binaries, dependencies, or build scripts are executed/downloaded. Exact gitlinks and .gitmodules text are in manifest.json. External catalogs/topics are checked as links only.",
             "", "Lexical IDs/API/dependency evidence is an audit aid, not an assertion that every match registers a plugin. License paths retain upstream texts; redistribution compatibility needs separate review. A missing license path is explicitly unresolved.",
             "", "HTTP probe failures describe this verification attempt, not a claim that an external resource has permanently disappeared. TLS certificate validation is retained.",
             "", "Re-run fetch/audit: `python tools/plugin_references.py`; offline verification: `python tools/plugin_references.py --verify-only`; self-tests: `python tools/plugin_references.py --self-test`.",
             "", "| Entry | Project / resource | Status | Source path or URL | Commit |", "| --- | --- | --- | --- | --- |"]
    for row in rows:
        target = row.get("path", row["url"])
        lines.append(f"| {row['id']} | {row['name']} | {row['status']} | `{target}` | {row.get('commit') or '—'} |")
    lines += ["", "## Unavailable / missing", ""]
    for row in rows:
        if row["status"] in {"unavailable", "missing"}:
            lines.append(f"- {row['id']} {row['name']}: {row.get('detail', 'See manifest repository record').replace(chr(10), ' ')}")
    lines += ["", "## Unresolved license evidence", ""]
    for repo in sorted(repositories.values(), key=lambda r: r["key"]):
        if "audit" in repo and not repo["audit"]["license_paths"]:
            lines.append(f"- `{repo['key']}`: no LICENSE/COPYING/COPYRIGHT/NOTICE filename detected. Read source headers and obtain permission before reuse; public access alone is not a license.")
    lines += ["", "## Provenance and bounded audit", "", "Full origins, HEADs, shallow flags, upstream warnings, local OBS provenance, license paths, source extension counts, lexical source locations, scan omissions, and submodule requirements are recorded in [manifest.json](manifest.json). Clone diagnostics are retained in `logs/`. Failed staging directories are preserved for diagnosis and never reused or overwritten.", ""]
    missing_gitlinks = sum(not link["initialized"] for repo in repositories.values() for link in repo.get("audit", {}).get("gitlinks", []))
    missing_lfs = sum(len(repo.get("audit", {}).get("lfs_pointers", [])) for repo in repositories.values())
    lines += [f"Reference completeness: **{missing_gitlinks} uninitialized gitlinks**, **{missing_lfs} retained LFS pointer files**. These deliberately omitted payloads are not represented as downloaded source/dependencies. Consult each repository's exact requirements before implementation or a separately authorized build.", ""]
    atomic_write(destination / "README.md", "\n".join(lines))
    return manifest


class SelfTests(unittest.TestCase):
    def test_repository_normalization_and_topics(self):
        self.assertEqual(repository_url("https://github.com/OBSProject/obs-studio/tree/master/plugins/obs-filters")["key"], "obsproject/obs-studio")
        self.assertIsNone(repository_url("https://github.com/topics/obs-plugin"))
        self.assertIsNone(repository_url("https://obsproject.com/forum/plugins/"))
        self.assertRaises(ValueError, repository_url, "https://github.com/x/%2e%2e")

    def test_exact_catalog_and_shared_builtin_repository(self):
        rows = parse_catalog(LIST.read_text(encoding="utf-8"))
        self.assertEqual(len(rows), 154)
        builtins = [x for x in rows if x["category"] == "builtin"]
        self.assertEqual(sum(x["repository"] == "obsproject/obs-studio" for x in builtins), 13)
        self.assertRaises(ValueError, parse_catalog, LIST.read_text(encoding="utf-8").replace("| 127 |", "| 128 |"))

    def test_audit_is_lexical_and_does_not_execute(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary)
            git(["init", "--quiet", str(path)])
            (path / "LICENSE").write_text("test license", encoding="utf-8")
            (path / "CMakeLists.txt").write_text("find_package(Imaginary REQUIRED)\n", encoding="utf-8")
            (path / "plugin.c").write_text('struct obs_source_info p = {.id = "test_source"};\nobs_register_source(&p);\n', encoding="utf-8")
            (path / "danger.py").write_text('raise RuntimeError("MUST NOT RUN")', encoding="utf-8")
            git(["add", "."], cwd=path)
            git(["-c", "user.name=Reference test", "-c", "user.email=reference@example.invalid", "commit", "--quiet", "-m", "fixture"], cwd=path)
            result = audit_repository(path)
            self.assertEqual(result["license_paths"], ["LICENSE"])
            self.assertEqual(result["literal_id_candidates"][0]["id"], "test_source")
            self.assertEqual(result["registration_calls"][0]["line"], 2)
            self.assertEqual(result["dependency_cues"][0]["text"], "find_package(Imaginary REQUIRED)")

    def test_dirty_checkout_preserved(self):
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary)
            (destination / "logs").mkdir()
            repo = repository_url("https://github.com/owner/repo")
            path = destination / repo["directory"]
            git(["init", "--quiet", str(path)])
            git(["remote", "add", "origin", repo["url"] + ".git"], cwd=path)
            (path / "user.txt").write_text("do not overwrite", encoding="utf-8")
            result = fetch_repository(repo, destination, ROOT / "doc/obs-studio", verify_only=True)
            self.assertEqual(result["status"], "unavailable")
            self.assertEqual((path / "user.txt").read_text(), "do not overwrite")
            self.assertIn("changes", result["detail"])

    def test_gitlink_requirements_and_symbolic_ids(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary)
            git(["init", "--quiet", str(path)])
            (path / ".gitmodules").write_text('[submodule "sdk"]\n\tpath = sdk\n\turl = https://github.com/example/sdk.git\n', encoding="utf-8")
            (path / "plugin.c").write_text('#define SOURCE_ID "real_id"\nstruct obs_source_info p={.id=SOURCE_ID};\nOBS_DECLARE_MODULE()\n', encoding="utf-8")
            git(["add", "."], cwd=path)
            git(["-c", "user.name=Reference test", "-c", "user.email=reference@example.invalid", "commit", "--quiet", "-m", "fixture"], cwd=path)
            commit = git(["rev-parse", "HEAD"], cwd=path)[0].strip()
            git(["update-index", "--add", "--cacheinfo", f"160000,{commit},sdk"], cwd=path)
            git(["-c", "user.name=Reference test", "-c", "user.email=reference@example.invalid", "commit", "--quiet", "-m", "gitlink fixture"], cwd=path)
            audit = audit_repository(path)
            self.assertEqual(audit["gitlinks"], [{"path": "sdk", "commit": commit, "initialized": False}])
            self.assertIn("https://github.com/example/sdk.git", audit["gitmodules"][0]["text"])
            self.assertEqual(audit["symbolic_id_candidates"][0]["candidate_definitions"][0]["value"], "real_id")
            self.assertEqual(audit["module_declarations"][0]["line"], 3)

    def test_wrong_origin_preserved_and_verify_does_not_clone(self):
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary)
            (destination / "logs").mkdir()
            repo = repository_url("https://github.com/owner/repo")
            missing = fetch_repository(repo, destination, ROOT / "doc/obs-studio", verify_only=True)
            self.assertEqual(missing["status"], "missing")
            self.assertFalse((destination / repo["directory"]).exists())
            path = destination / repo["directory"]
            git(["init", "--quiet", str(path)])
            git(["remote", "add", "origin", "https://github.com/different/repo.git"], cwd=path)
            result = fetch_repository(repo, destination, ROOT / "doc/obs-studio", verify_only=True)
            self.assertEqual(result["status"], "unavailable")
            self.assertIn("origin differs", result["detail"])
            self.assertEqual(git(["remote", "get-url", "origin"], cwd=path)[0].strip(), "https://github.com/different/repo.git")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify-only", action="store_true", help="No network; audit existing snapshots only")
    parser.add_argument("--self-test", action="store_true", help="Run isolated local-Git/parser tests, no network")
    args = parser.parse_args()
    if args.self_test:
        result = unittest.main(argv=[sys.argv[0]], exit=False)
        if not result.result.wasSuccessful():
            raise SystemExit(1)
        return
    source = LIST.read_bytes()
    catalog = parse_catalog(source.decode("utf-8"))
    repos = {}
    for entry in catalog:
        repo = repository_url(entry["url"])
        if repo:
            repos.setdefault(repo["key"], repo)
    DEST.mkdir(exist_ok=True)
    (DEST / "logs").mkdir(exist_ok=True)
    previous = {}
    external = {}
    manifest_path = DEST / "manifest.json"
    if manifest_path.exists():
        old = json.loads(manifest_path.read_text(encoding="utf-8"))
        previous = {repo["key"]: repo for repo in old.get("repositories", [])}
        external = {entry["url"]: {key: value for key, value in entry.items() if key in {"status", "reference_only", "http_status", "resolved_url", "detail"}} for entry in old.get("entries", []) if entry.get("reference_only")}
    results = {}
    # Deterministic scheduling order; priority references support the current implementation slice.
    priority = {"norihiro/obs-audio-pan-filter": 0, "norihiro/obs-mute-filter": 1, "exeldro/obs-scene-notes-dock": 2, "obsproject/obs-studio": 3}
    ordered = sorted(repos.values(), key=lambda r: (priority.get(r["key"], 4), r["key"]))
    with ThreadPoolExecutor(max_workers=2) as executor:
        futures = {executor.submit(fetch_repository, repo, DEST, ROOT / "doc/obs-studio", args.verify_only, previous.get(repo["key"])): repo for repo in ordered}
        for future in as_completed(futures):
            result = future.result()
            results[result["key"]] = result
            print(f"[{len(results)}/{len(repos)}] {result['status']}: {result['key']}" + (f" — {result['detail'].splitlines()[0]}" if result.get("detail") else ""), flush=True)
            write_outputs(DEST, catalog, results | {key: {**r, "status": "missing", "path": f"doc/plugins/{r['directory']}", "detail": "Pending this run"} for key, r in repos.items() if key not in results}, external, hashlib.sha256(source).hexdigest())
    if not args.verify_only:
        for url in sorted({entry["url"] for entry in catalog if entry["repository"] is None}):
            external[url] = external_status(url)
            print(f"[link] {external[url]['status']}: {url}", flush=True)
    manifest = write_outputs(DEST, catalog, results, external, hashlib.sha256(source).hexdigest())
    print(json.dumps(manifest["counts"], ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
