#!/usr/bin/env python3
"""Deterministic, source-only OBS migration inventory. Never starts OBS or a GUI."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import unittest
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "doc" / "obs-studio"
DESTINATION = ROOT / "doc" / "migration"
SOURCE_EXTENSIONS = {".c", ".cpp", ".h", ".hpp", ".m", ".mm"}
EDITABLE_WIDGETS = {
    "QCheckBox", "QRadioButton", "QComboBox", "QSpinBox", "QDoubleSpinBox",
    "QLineEdit", "QPlainTextEdit", "QTextEdit", "QSlider", "QKeySequenceEdit",
    "QPushButton", "QToolButton", "EditableComboBox", "FocusList",
}


def text(path: Path) -> str:
    return path.read_text(encoding="utf-8-sig")


def reference(path: Path, line: int = 1) -> dict:
    return {"file": path.relative_to(ROOT).as_posix(), "line": line}


def named_lines(source: str) -> dict[str, int]:
    return {match.group(1): source.count("\n", 0, match.start()) + 1
            for match in re.finditer(r'<(?:widget|action)\b[^>]*\bname="([^"]+)"', source)}


def properties(element: ET.Element) -> dict[str, str]:
    return {prop.attrib["name"]: "".join(prop.itertext()).strip()
            for prop in element.findall("property")}


def locale() -> dict[str, str]:
    values = {}
    for line in text(SOURCE / "frontend/data/locale/en-US.ini").splitlines():
        match = re.fullmatch(r'([^#;=]+)="(.*)"', line)
        if match:
            values[match.group(1)] = match.group(2).replace(r'\"', '"').replace(r"\n", "\n")
    return values


def ui_inventory(labels: dict[str, str]) -> dict:
    main = SOURCE / "frontend/forms/OBSBasic.ui"
    source = text(main)
    lines = named_lines(source)
    root = ET.fromstring(source)
    actions = {}
    for action in root.iter("action"):
        props = properties(action)
        key = props.get("text", "")
        actions[action.attrib["name"]] = {
            "id": action.attrib["name"], "label_key": key,
            "label": labels.get(key, key), "shortcut": props.get("shortcut"),
            "checkable": props.get("checkable") == "true",
            "enabled_in_form": props.get("enabled", "true") == "true",
            "reference": reference(main, lines[action.attrib["name"]]),
        }
    menus = {}
    for widget in root.iter("widget"):
        if widget.attrib.get("class") == "QMenu":
            key = properties(widget).get("title", "")
            menus[widget.attrib["name"]] = {
                "id": widget.attrib["name"], "label_key": key,
                "label": labels.get(key, key),
                "items": [{"kind": "separator" if item.attrib["name"] == "separator"
                           else "menu" if item.attrib["name"] in {
                               node.attrib["name"] for node in root.iter("widget")
                               if node.attrib.get("class") == "QMenu"} else "action",
                           "id": item.attrib["name"]}
                          for item in widget.findall("addaction")],
                "reference": reference(main, lines[widget.attrib["name"]]),
            }
    bar = next(widget for widget in root.iter("widget")
               if widget.attrib.get("class") == "QMenuBar")
    order = [item.attrib["name"] for item in bar.findall("addaction")]
    docks = []
    for widget in root.iter("widget"):
        if widget.attrib.get("class") in {"OBSDock", "QDockWidget"}:
            key = properties(widget).get("windowTitle", "")
            docks.append({"id": widget.attrib["name"], "label_key": key,
                          "label": labels.get(key, key),
                          "reference": reference(main, lines[widget.attrib["name"]])})
    settings_file = SOURCE / "frontend/forms/OBSBasicSettings.ui"
    settings_text = text(settings_file)
    settings_lines = named_lines(settings_text)
    settings = ET.fromstring(settings_text)
    navigation = next(w for w in settings.iter("widget") if w.attrib.get("name") == "listWidget")
    keys = [properties(item).get("text", "") for item in navigation.findall("item")]
    pages = next(w for w in settings.iter("widget") if w.attrib.get("name") == "settingsPages")
    settings_pages = []
    for key, page in zip(keys, pages.findall("widget"), strict=True):
        controls = []
        for widget in page.iter("widget"):
            if widget.attrib.get("class") in EDITABLE_WIDGETS:
                props = properties(widget)
                label_key = props.get("text", props.get("toolTip", ""))
                controls.append({"id": widget.attrib["name"], "class": widget.attrib["class"],
                                 "label_key": label_key, "label": labels.get(label_key, label_key),
                                 "properties": props,
                                 "options": [properties(item) for item in widget.findall("item")],
                                 "reference": reference(settings_file, settings_lines[widget.attrib["name"]])})
        settings_pages.append({"id": page.attrib["name"], "label_key": key,
                               "label": labels.get(key, key), "controls": controls,
                               "reference": reference(settings_file, settings_lines[page.attrib["name"]])})
    forms = []
    for path in sorted((SOURCE / "frontend/forms").rglob("*.ui")):
        form = ET.fromstring(text(path))
        forms.append({"id": form.findtext("class"), "reference": reference(path),
                      "widget_count": sum(1 for _ in form.iter("widget"))})
    return {"menu_order": order, "menus": list(menus.values()),
            "actions": list(actions.values()), "docks": docks,
            "settings_pages": settings_pages, "forms": forms}


def without_comments(source: str) -> str:
    # Preserve strings and byte offsets so source references remain exact.
    return re.sub(r'"(?:\\.|[^"\\])*"|\'(?:\\.|[^\'\\])*\'|//[^\n]*|/\*.*?\*/',
                  lambda m: re.sub(r"[^\n]", " ", m.group())
                  if m.group().startswith(("//", "/*")) else m.group(), source, flags=re.S)


def initializer(source: str, start: int) -> str:
    depth = 0
    quoted = False
    escape = False
    for index in range(start, len(source)):
        char = source[index]
        if escape:
            escape = False
        elif char == "\\" and quoted:
            escape = True
        elif char == '"':
            quoted = not quoted
        elif not quoted:
            if char == "{":
                depth += 1
            elif char == "}":
                depth -= 1
                if depth == 0:
                    return source[start:index + 1]
    raise ValueError("Unbalanced registration initializer")


def registrations(path: Path) -> list[dict]:
    source = without_comments(text(path))
    declarations = list(re.finditer(
        r'\b(?:struct\s+)?obs_(source|output|encoder|service)_info\s+(\w+)\s*=\s*\{', source))
    records = []
    for match in declarations:
        body = initializer(source, match.end() - 1)
        id_match = re.search(r'\.id\s*=\s*([^,\n}]+)', body)
        assignments = list(re.finditer(r'\b' + re.escape(match.group(2)) + r'\.id\s*=\s*([^;\n]+)', source))
        discovered = [(id_match.group(1).strip(), match.start())] if id_match else []
        discovered += [(item.group(1).strip(), item.start()) for item in assignments]
        if not discovered:
            discovered = [(None, match.start())]
        for expression, offset in discovered:
            literal = re.fullmatch(r'"([^"\\]+)"', expression or "")
            records.append({"kind": match.group(1), "symbol": match.group(2),
                            "id": literal.group(1) if literal else None,
                            "id_expression": expression,
                            "type_expression": (type_match.group(1).strip() if (type_match := re.search(
                                r'\.type\s*=\s*([^,\n}]+)', body)) else None),
                            "reference": reference(path, source.count("\n", 0, offset) + 1)})
    return records


def source_inventory() -> dict:
    plugins = []
    for directory in sorted((SOURCE / "plugins").iterdir()):
        if not directory.is_dir():
            continue
        files = sorted(path for path in directory.rglob("*") if path.suffix in SOURCE_EXTENSIONS)
        plugins.append({"id": directory.name, "source_files": len(files),
                        "checkout_state": "present" if files else "missing-or-unpopulated",
                        "reference": reference(directory),
                        "registrations": [item for path in files for item in registrations(path)]})
    dynamic = []
    for path in sorted((SOURCE / "frontend").rglob("*")):
        if path.suffix not in SOURCE_EXTENSIONS:
            continue
        for index, line in enumerate(text(path).splitlines(), 1):
            if re.search(r'\b(addAction|addMenu|insertAction|insertMenu)\s*\(', line):
                dynamic.append({"expression": line.strip(), "reference": reference(path, index)})
    builtins = [item for path in sorted((SOURCE / "libobs").glob("*.c"))
                for item in registrations(path) if item["id"] is not None]
    return {"plugins": plugins, "builtin_registrations": builtins,
            "dynamic_menu_sites": dynamic}


def build() -> dict:
    result = subprocess.run(["git", "-c", f"safe.directory={SOURCE.as_posix()}",
                             "-C", str(SOURCE), "rev-parse", "HEAD"],
                            check=True, capture_output=True, text=True)
    inventory = {
        "schema_version": 1,
        "reference": {"repository": "https://github.com/obsproject/obs-studio",
                      "commit": result.stdout.strip(), "license": "GPL-2.0-or-later",
                      "license_reference": reference(SOURCE / "README.rst", 22),
                      "copying_reference": reference(SOURCE / "COPYING"),
                      "main_form_sha256": hashlib.sha256((SOURCE / "frontend/forms/OBSBasic.ui").read_bytes()).hexdigest()},
        "coverage": {
            "method": "Static Qt Designer XML and C/C++/Objective-C registration declaration scan",
            "rust_parity": "unverified: inventory entries do not imply implementation",
            "limitations": [
                "Menu ordering is the Designer baseline; runtime platform, plugin and device actions require separate checks.",
                "Dynamic menu sites are review leads; this script does not evaluate C++ or platform conditionals.",
                "Settings controls describe the Designer file, not all dynamically generated property dialogs or backend options.",
                "Plugin registrations are declarations or assignments; they do not establish registration, availability or runtime support.",
                "Macro/generated registrations, third-party plugins and unpopulated Git submodules require further discovery.",
                "No GUI, capture device, network, encoder or operating-system acceptance is performed.",
            ]},
    }
    inventory.update(ui_inventory(locale()))
    inventory.update(source_inventory())
    validate(inventory)
    inventory["counts"] = {
        key: len(inventory[key]) for key in ["menus", "actions", "docks", "forms", "settings_pages", "plugins", "builtin_registrations", "dynamic_menu_sites"]}
    inventory["counts"]["settings_controls"] = sum(len(p["controls"]) for p in inventory["settings_pages"])
    inventory["counts"]["registration_declarations"] = sum(len(p["registrations"]) for p in inventory["plugins"])
    return inventory


def validate(inventory: dict) -> None:
    menu_ids = {menu["id"] for menu in inventory["menus"]}
    action_ids = {action["id"] for action in inventory["actions"]}
    if len(menu_ids) != len(inventory["menus"]) or len(action_ids) != len(inventory["actions"]):
        raise ValueError("Duplicate menu/action identity")
    if any(name not in menu_ids for name in inventory["menu_order"]):
        raise ValueError("Unresolved menu bar reference")
    for menu in inventory["menus"]:
        for item in menu["items"]:
            if item["kind"] != "separator" and item["id"] not in menu_ids | action_ids:
                raise ValueError(f"Unresolved menu item: {item}")
    if not inventory["settings_pages"] or not inventory["plugins"]:
        raise ValueError("Empty settings/plugins inventory")


def markdown(inventory: dict) -> str:
    def clean(value: str) -> str:
        return value.replace("|", "\\|").replace("\n", " ").replace("&", "")

    lines = ["# OBS Studio migration parity ledger", "",
             f"Reference: `{inventory['reference']['commit']}`; `GPL-2.0-or-later` (OBS README.rst:22).", "",
             "Generated by `python tools/obs_inventory.py`. Verify without writing using `python tools/obs_inventory.py --check`.", "",
             "This is a source discovery baseline. Every feature below remains **unverified** until linked to Rust implementation and reproducible acceptance evidence. An existing label, declaration or menu item is not functional parity.", "",
             "## Discovery counts", "", "| Area | Count |", "|---|---:|"]
    lines += [f"| {key.replace('_', ' ')} | {count} |" for key, count in inventory["counts"].items()]
    lines += ["", "## Ordered main menu baseline", "",
              "The order and separators come directly from `frontend/forms/OBSBasic.ui`. Runtime additions are separately enumerated in the JSON inventory.", "",
              "| Menu / submenu | Action identity | English label | Shortcut | Rust parity |", "|---|---|---|---|---|"]
    menus = {menu["id"]: menu for menu in inventory["menus"]}
    actions = {action["id"]: action for action in inventory["actions"]}

    def walk(menu_id: str, parents: list[str]) -> None:
        menu = menus[menu_id]
        path = parents + [clean(menu["label"])]
        if not menu["items"]:
            lines.append(f"| {' / '.join(path)} | `{menu_id}` | Runtime populated menu | | unverified |")
        for item in menu["items"]:
            if item["kind"] == "menu":
                walk(item["id"], path)
            elif item["kind"] == "separator":
                lines.append(f"| {' / '.join(path)} | | separator | | unverified |")
            else:
                action = actions[item["id"]]
                lines.append(f"| {' / '.join(path)} | `{action['id']}` | {clean(action['label'])} | {action['shortcut'] or ''} | unverified |")

    for name in inventory["menu_order"]:
        walk(name, [])
    lines += ["", "## Settings", "", "| Page | Identity | Designer controls | Rust parity |", "|---|---|---:|---|"]
    lines += [f"| {clean(p['label'])} | `{p['id']}` | {len(p['controls'])} | unverified |" for p in inventory["settings_pages"]]
    lines += ["", "## Designer docks", "", "| Dock | Identity | Rust parity |", "|---|---|---|"]
    lines += [f"| {clean(d['label'])} | `{d['id']}` | unverified |" for d in inventory["docks"]]
    lines += ["", "Controls dock, browser docks, projectors, source properties, device menus and plugin dialogs also need runtime/source tracing beyond Designer docks.", "",
              "## Plugins and registered types", "", "| Plugin | Source files | Registration declarations | Checkout | Rust parity |", "|---|---:|---:|---|---|"]
    lines += [f"| `{p['id']}` | {p['source_files']} | {len(p['registrations'])} | {p['checkout_state']} | unverified |" for p in inventory["plugins"]]
    lines += ["", "Built-in libobs registrations: " + ", ".join(f"`{item['id'] or item['id_expression']}`" for item in inventory["builtin_registrations"]) + ". All remain unverified in Rust.", "",
              "`obs-browser` and `obs-websocket` are Git submodules. An unpopulated submodule is missing reference material, not an absent OBS feature.", "",
              "## Acceptance groups still required", "",
              "- Scene/source lifecycle, nesting, transforms, groups, filters, preview/program, transitions, projectors and multiview.",
              "- Audio mixing, monitoring, synchronization, channel routing, resampling, device loss and reconnect.",
              "- Screen/window/game/camera/audio capture on Windows and macOS, permissions, HDR and GPU/device recovery.",
              "- Real codec encode/decode, mux/remux, recording recovery, replay buffer, RTMP/SRT/RIST/WHIP outputs and service authentication.",
              "- Profiles, scene-collection compatibility, migrations, settings, hotkeys, docks, localization and accessibility.",
              "- Browser sources, native/plugin compatibility policy, VST, scripting, WebSocket automation and vendor hardware integrations.",
              "- Packaging, updates, signing, Windows and macOS actual-host acceptance; Linux is secondary.", "",
              "Pure Rust implementation must not silently substitute OBS/FFmpeg C or C++ libraries for migrated engine code. OS/framework boundary exceptions need explicit architecture decisions and accurate capability reports.", "",
              "## Discovery limitations", ""]
    lines += [f"- {limitation}" for limitation in inventory["coverage"]["limitations"]]
    return "\n".join(lines) + "\n"


class ScannerTests(unittest.TestCase):
    def test_initializer_handles_nested_braces_and_quoted_braces(self):
        self.assertEqual(initializer('{ .id = "a}b", .nested = { 1, 2 } }; other', 0),
                         '{ .id = "a}b", .nested = { 1, 2 } }')

    def test_comments_preserve_lines_and_string_literals(self):
        source = '// .id = "bad"\n{ .id = "https://example", /* }\n */ .x = 2 }'
        filtered = without_comments(source)
        self.assertEqual(len(source), len(filtered))
        self.assertEqual(source.count("\n"), filtered.count("\n"))
        self.assertNotIn('"bad"', filtered)
        self.assertIn('"https://example"', filtered)

    def test_unbalanced_initializer_is_rejected(self):
        with self.assertRaises(ValueError):
            initializer("{ .x = { 1 }", 0)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="fail if generated inventory differs from saved files")
    parser.add_argument("--self-test", action="store_true", help="run scanner edge-case tests")
    args = parser.parse_args()
    if args.self_test:
        result = unittest.TextTestRunner().run(unittest.defaultTestLoader.loadTestsFromTestCase(ScannerTests))
        return 0 if result.wasSuccessful() else 1
    inventory = build()
    outputs = {"obs-inventory.json": json.dumps(inventory, indent=2, ensure_ascii=False) + "\n",
               "parity.md": markdown(inventory)}
    if args.check:
        stale = [name for name, content in outputs.items()
                 if not (DESTINATION / name).is_file() or text(DESTINATION / name) != content]
        if stale:
            print("Inventory out of date: " + ", ".join(stale), file=sys.stderr)
            return 1
        print("Inventory matches reference source: " + json.dumps(inventory["counts"]))
    else:
        DESTINATION.mkdir(parents=True, exist_ok=True)
        for name, content in outputs.items():
            (DESTINATION / name).write_text(content, encoding="utf-8", newline="\n")
        print("Wrote source inventory: " + json.dumps(inventory["counts"]))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
