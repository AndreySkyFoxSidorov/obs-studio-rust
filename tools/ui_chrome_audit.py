"""Inventory actionable Rust UI sites and their tooltip/localization coverage.

Source inspection only: no application/device execution and no dependencies.
The scanner balances Rust call delimiters outside comments/string literals;
dynamic or generic captions are reported explicitly, never counted as precise
semantic coverage. --apply only adds missing hover chains to views/*.rs.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]
METHODS = re.compile(r"\.\s*(button|small_button|checkbox|radio|selectable_label|selectable_value|menu_button|text_edit_singleline|text_edit_multiline|color_edit_button_srgba_unmultiplied|color_edit_button_srgb|add_enabled|add|show_ui)\s*\(")
BUILDERS = re.compile(r"\b(Button|Checkbox|RadioButton|TextEdit|Slider|DragValue)\s*::")
GENERICS = {"Item", "Menu", "Text field", "Numeric value", "Slider", "Color"}


def mask(source: str) -> str:
    result = list(source)
    i = 0
    while i < len(source):
        end = i
        if source.startswith("//", i):
            end = source.find("\n", i)
            if end < 0:
                end = len(source)
        elif source.startswith("/*", i):
            end, depth = i + 2, 1
            while end < len(source) and depth:
                if source.startswith("/*", end):
                    depth += 1
                    end += 2
                elif source.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
        else:
            raw = re.match(r'r(#{0,32})"', source[i:i + 34]) if source[i] == "r" else None
            if raw:
                close = '"' + raw.group(1)
                found = source.find(close, i + raw.end())
                end = len(source) if found < 0 else found + len(close)
            elif source[i] == '"':
                end = i + 1
                while end < len(source):
                    if source[end] == "\\":
                        end += 2
                    elif source[end] == '"':
                        end += 1
                        break
                    else:
                        end += 1
            elif source[i] == "'":
                char = re.match(r"'(?:\\.|[^'\\\n])'", source[i:])
                if char:
                    end = i + char.end()
        if end > i:
            for position in range(i, end):
                if result[position] != "\n":
                    result[position] = " "
            i = end
        else:
            i += 1
    return "".join(result)


def close_call(code: str, opening: int, left="(", right=")") -> int:
    depth = 0
    for i in range(opening, len(code)):
        if code[i] == left:
            depth += 1
        elif code[i] == right:
            depth -= 1
            if depth == 0:
                return i + 1
    raise ValueError(f"unbalanced delimiter at {opening}")


def literals(source: str) -> list[str]:
    values = []
    for match in re.finditer(r'"(?:\\.|[^"\\])*"', source):
        try:
            values.append(json.loads(match.group()))
        except json.JSONDecodeError:
            values.append(match.group()[1:-1])
    return values


def arguments(source: str) -> list[str]:
    code = mask(source)
    values, start, depth = [], 0, 0
    for i, char in enumerate(code):
        if char in "([{":
            depth += 1
        elif char in ")]}":
            depth -= 1
        elif char == "," and depth == 0:
            values.append(source[start:i].strip())
            start = i + 1
    values.append(source[start:].strip())
    return [value for value in values if value]


def infer_caption(method: str, body: str, preceding: str) -> tuple[str, str]:
    args = arguments(body)
    index = 2 if method == "selectable_value" else 1 if method in {"checkbox", "radio", "selectable_label", "add_enabled"} else 0
    if method != "show_ui" and len(args) > index:
        body = args[index]
    # Empty presentation text is not a semantic caption. In particular, an
    # empty helper argument such as selected_name(..., "", ...) must not be
    # mistaken for the caption returned by that helper.
    if body.strip() == '""':
        return "", "empty"
    strings = [value for value in literals(body) if value.strip()]
    if method in {"text_edit_singleline", "text_edit_multiline"} or "TextEdit::" in body:
        identity = re.search(r'\.id_salt\(\s*("(?:\\.|[^"\\])*")', body)
        if identity:
            return json.loads(identity.group(1)), "field_id"
        return (strings[0], "literal") if strings else ("Text field", "generic")
    if "Slider::" in body:
        return ("T-bar", "range") if "1023" in body else ("Volume", "range") if "20.0" in body else ("Slider", "generic")
    if "DragValue::" in body:
        fields = {"pan_left": "Pan (Left)", "pan_right": "Pan (Right)", "gain_left_db": "Gain (Left)", "gain_right_db": "Gain (Right)", "rotation_degrees": "Rotation", "opacity": "Opacity", "duration_ms": "Duration (ms)", "switch_point": "Switch Point"}
        for field, label in fields.items():
            if field in body:
                return label, "field"
        labels = re.findall(r'ui\.label\((?:crate::i18n::tr\(ui,\s*)?("(?:\\.|[^"\\])*")', preceding[-300:])
        if labels:
            return json.loads(labels[-1]), "preceding_label"
        return "Numeric value", "generic"
    if method.startswith("color_edit"):
        return "Color", "generic"
    if method == "show_ui":
        return "Menu", "generic"
    if strings:
        if "{" in strings[0] and re.search(r"(?:format!|i18n::tf)\s*\(", body):
            return strings[0], "formatted_dynamic"
        return strings[0], "literal"
    for variable, caption in [("scene", "Scene"), ("source", "Source"), ("profile", "Profile"), ("preset", "Transition preset")]:
        if f"{variable}.name()" in body:
            return caption, "authored_name"
    return ("Menu", "generic") if method == "menu_button" else ("Item", "generic")


def inventory_source(source: str, file: str, known: set[str]) -> list[dict]:
    code = mask(source)
    tests = []
    for test in re.finditer(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]\s*mod\s+\w+\s*\{", code):
        opening = code.index("{", test.start())
        tests.append((opening, close_call(code, opening, "{", "}")))
    sites = []
    for call in METHODS.finditer(code):
        if any(start <= call.start() < end for start, end in tests):
            continue
        method = call.group(1)
        opening = code.index("(", call.start())
        end = close_call(code, opening)
        body = source[opening + 1:end - 1]
        if method in {"add", "add_enabled"} and not BUILDERS.search(mask(body)):
            continue
        if method == "show_ui" and "ComboBox::" not in source[max(0, call.start() - 260):call.start()]:
            continue
        chain_end = end
        while True:
            chain = re.match(r"\s*\.\s*(\w+)\s*(\()?", code[chain_end:])
            if not chain:
                break
            if chain.group(2):
                chain_end = close_call(code, chain_end + chain.start(2))
            else:
                chain_end += chain.end()
        chain_text = source[end:chain_end]
        passive = bool(re.search(r"crate::hints::passive\(\s*ui\s*$", source[max(0, call.start()-100):call.start()]))
        caption, caption_source = infer_caption(method, body, source[:call.start()])
        direct = method not in {"menu_button", "show_ui"}
        sites.append({"file": file, "line": source.count("\n", 0, call.start()) + 1,
            "method": method, "caption": caption, "caption_source": caption_source,
            "caption_expression": body.strip(), "has_hover": passive or ".on_hover_text" in chain_text or ".on_hover_ui" in chain_text,
            "has_disabled_hover": passive or ".on_disabled_hover_text" in chain_text or ".on_disabled_hover_ui" in chain_text,
            "hint_known": caption in known, "specific_hint": caption in known and caption not in GENERICS,
            "offset": end, "direct_response": direct, "chain_text": chain_text})
    return sites


def inventory(path: Path, known: set[str]) -> list[dict]:
    return inventory_source(path.read_text(encoding="utf-8-sig"), path.relative_to(ROOT).as_posix(), known)


def self_test() -> None:
    source = r'''
    // ui.button("Fake comment");
    let literal = r###"ui.button("Fake raw");"###;
    /* nested /* ui.button("Fake block"); */ comment */
    ui.button("Add").on_hover_text(hints::text(ui, "Add"));
    ui.menu_button(scene.name(), |ui| { ui.button("Remove").on_hover_text("Remove this item."); }).response.on_hover_text("Scene actions.");
    ui.selectable_label(false, crate::i18n::tf(ui,"{} ({})",&[("{}",locale.name())])).on_hover_text("Language.");
    crate::hints::passive(ui.add(egui::Slider::new(&mut gain, 0.0..=20.0)),ui,"Volume");
    #[cfg(test)] mod tests { fn hidden() { ui.button("Not production"); } }
    ui.button("Unknown");
    '''
    sites = inventory_source(source, "fixture.rs", {"Add", "Remove", "Scene", "Volume"})
    assert len(sites) == 6, sites
    assert [site["caption"] for site in sites[:3]] == ["Add", "Scene", "Remove"], sites
    assert sites[1]["caption_source"] == "authored_name", sites
    assert sites[3]["caption_source"] == "formatted_dynamic", sites
    assert sites[4]["has_hover"] and sites[4]["has_disabled_hover"], sites
    assert all(site["has_hover"] for site in sites[:-1]), sites
    assert not sites[-1]["has_hover"] and not sites[-1]["hint_known"], sites
    empty = inventory_source('ui.button("").on_hover_text("Color."); ui.selectable_label(false, selected_name(ui, "", picker)).on_hover_text("Device.");', "fixture.rs", set())
    assert [(site["caption"], site["caption_source"]) for site in empty] == [("", "empty"), ("Item", "generic")], empty
    assert arguments('a, f("x,y", [1,2]), |ui| { ui.label("x"); }') == ['a', 'f("x,y", [1,2])', '|ui| { ui.label("x"); }']


def known_hints() -> set[str]:
    path = ROOT / "apps/desktop/src/hints.rs"
    if not path.exists():
        return set()
    return set(re.findall(r'\(\s*"([^"\\]+)"\s*,', path.read_text(encoding="utf-8")))


def main() -> None:
    sys.stdout.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--apply", action="store_true", help="Add missing tooltip chains in views only; preserve custom hints")
    parser.add_argument("--summary", action="store_true")
    parser.add_argument("--check", action="store_true", help="Exit nonzero when tooltip coverage or known static captions are missing")
    parser.add_argument("--self-test", action="store_true", help="Run in-memory scanner fixtures without changing source files")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        print(json.dumps({"self_test": "passed"}))
        return
    known = known_hints()
    paths = sorted((ROOT / "apps/desktop/src").rglob("*.rs"))
    if args.apply:
        for path in paths:
            if path.parent.name != "views":
                continue
            source = path.read_text(encoding="utf-8-sig")
            edits = []
            for site in inventory(path, known):
                if site["has_hover"]:
                    continue
                if not site["direct_response"] and any(member in site["chain_text"] for member in [".inner", ".response"]):
                    continue
                caption = json.dumps(site["caption"], ensure_ascii=False)
                expression = f"crate::hints::text(ui,{caption})"
                suffix = "" if site["direct_response"] or ".response" in site["chain_text"] else ".response"
                suffix += f".on_hover_text({expression})"
                if not site["has_disabled_hover"]:
                    suffix += f".on_disabled_hover_text({expression})"
                edits.append((site["offset"], 0, suffix))
            for offset, length, replacement in sorted(edits, reverse=True):
                source = source[:offset] + replacement + source[offset + length:]
            path.write_text(source, encoding="utf-8", newline="")
    sites = [site for path in paths for site in inventory(path, known)]
    result = {"sites": len(sites), "missing_hover": sum(not s["has_hover"] for s in sites),
        "dynamic_or_generic": sum(s["caption_source"] in {"authored_name", "generic", "formatted_dynamic"} for s in sites),
        "empty_caption_controls": sum(s["caption_source"] == "empty" for s in sites),
        "unknown_static_captions": sorted({s["caption"] for s in sites if not s["hint_known"] and s["caption_source"] not in {"authored_name", "generic", "formatted_dynamic", "empty"}}),
        "scope": "Actionable source sites, not proof of native accessibility or exact semantic coverage."}
    if not args.summary:
        result["widgets"] = [{key: value for key, value in site.items() if key not in {"offset", "chain_text"}} for site in sites]
    print(json.dumps(result, ensure_ascii=False, indent=2))
    if args.check and (result["missing_hover"] or result["unknown_static_captions"]):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
