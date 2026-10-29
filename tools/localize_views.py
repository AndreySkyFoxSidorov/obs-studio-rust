"""Wrap literal desktop chrome in the shared CSV adapter; never rewrite authored names."""
from pathlib import Path
import json
import re

ROOT = Path(__file__).resolve().parents[1]
METHODS = {"label": 0, "heading": 0, "button": 0, "small": 0, "weak": 0,
           "strong": 0, "menu_button": 0, "checkbox": 1, "selectable_label": 1,
           "selectable_value": 2, "colored_label": 1, "on_hover_text": 0,
           "on_disabled_hover_text": 0, "alt_text": 0, "radio_value":2, "collapsing":0}
CALLS = re.compile(r"\.((?:" + "|".join(METHODS) + r"))\s*\(|egui::(?:Button|RichText|Label)::new\s*\(")
LITERAL = re.compile(r'"(?:\\.|[^"\\])*"\Z', re.S)


def arguments(source, opening):
    start = opening + 1
    stack = [")"]
    spans = []
    index = start
    while index < len(source):
        char = source[index]
        if char == '"':
            index += 1
            while index < len(source):
                if source[index] == "\\":
                    index += 2
                elif source[index] == '"':
                    index += 1
                    break
                else:
                    index += 1
            continue
        if source.startswith("//", index):
            index = source.find("\n", index)
            if index < 0:
                return []
            continue
        if char in "([{":
            stack.append({"(": ")", "[": "]", "{": "}"}[char])
        elif char in ")]}":
            if not stack or char != stack.pop():
                return []
            if not stack:
                spans.append((start, index))
                return spans
        elif char == "," and len(stack) == 1:
            spans.append((start, index))
            start = index + 1
        index += 1
    return []


def formatted(expression):
    if not expression.startswith("format!(") or not expression.endswith(")"):
        return None
    spans=arguments(expression,7)
    if not spans:return None
    literal=expression[spans[0][0]:spans[0][1]].strip()
    if not LITERAL.fullmatch(literal):return None
    try:template=json.loads(literal)
    except ValueError:return None
    values=[expression[a:b].strip() for a,b in spans[1:] if expression[a:b].strip()]
    if any(re.match(r"^[A-Za-z_]\w*\s*=",value) for value in values):return None
    implicit=0;prepared=[]
    for field in re.finditer(r"(?<!\{)\{([A-Za-z_]\w*|[0-9]+)?(?::([^{}]*))?\}(?!\})",template):
        name,spec=field.groups()
        if spec and any(ch in spec for ch in "*$"):return None
        if name is None:
            if implicit>=len(values):return None
            value=values[implicit];implicit+=1
        elif name.isdigit():
            if int(name)>=len(values):return None
            value=values[int(name)]
        else:value=name
        mini="{"+(":"+spec if spec else "")+"}"
        token=json.dumps(field.group(),ensure_ascii=False)
        prepared.append("("+token+",format!("+json.dumps(mini,ensure_ascii=False)+","+value+"))")
    if not prepared:return None
    return "crate::i18n::tf(ui,"+literal+",&["+",".join(prepared)+"])"


def main():
    result = {}
    for path in (ROOT / "apps/desktop/src").rglob("*.rs"):
        if path.name in {"i18n.rs", "theme.rs", "main.rs", "io.rs"}:
            continue
        source = path.read_text(encoding="utf-8")
        tests = re.search(r"(?m)^#\[cfg\(test\)\]\s*\nmod\s+", source)
        limit = tests.start() if tests else len(source)
        edits = {}
        for match in CALLS.finditer(source, 0, limit):
            spans = arguments(source, match.end() - 1)
            position = METHODS[match.group(1)] if match.group(1) else 0
            if position >= len(spans):
                continue
            begin, end = spans[position]
            text = source[begin:end]
            stripped = text.strip()
            if LITERAL.fullmatch(stripped):
                edits[(begin, end)] = text.replace(stripped, "crate::i18n::tr(ui," + stripped + ")", 1)
            elif (replacement:=formatted(stripped)) is not None:
                edits[(begin,end)]=text.replace(stripped,replacement,1)
        for (begin, end), text in sorted(edits.items(), reverse=True):
            source = source[:begin] + text + source[end:]
        if edits:
            path.write_text(source, encoding="utf-8")
        result[path.relative_to(ROOT).as_posix()] = len(edits)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
