#!/bin/bash
# Fails when a QML file of the Store could show remote or file text as markup,
# open a link by itself, build code from data, or load an image from anywhere
# but a local file. Everything from a catalog, a remote, a file or a launch
# argument is untrusted text (CLAUDE.md, docs/DESIGN.md "Trust"), and QML's
# default for Text and Label is AutoText, which turns a decoded "&lt;b&gt;"
# into rich text.
#
#   scripts/check-qml-plaintext.sh [folder-or-file...]   (default: apps/telamon-store/qml)
#
# Rules (per file, comments ignored):
#   1. Every Text, Label or Heading (also QQC2.Label, Kirigami.Heading) sets
#      `textFormat: Text.PlainText` itself; a TextEdit/TextArea sets nothing but
#      PlainText. Text.RichText, StyledText, AutoText, MarkdownText are refused
#      anywhere, and so is Controls' own ToolTip (TelamonToolTip is plain).
#   2. No Qt.openUrlExternally (links open through TelamonPortal, https only),
#      Qt.createQmlObject, Qt.createComponent, Qt.include, eval(), new Function,
#      XMLHttpRequest, WebSocket or fetch(): the QML layer makes no request and
#      runs no code built from data.
#   3. A TelamonPage `title` built from data goes through headerTitle(): the
#      navigation stack shows it in a Label that treats "<img src=...>" as
#      markup (and loads the image).
#   4. An Image or AnimatedImage `source` is empty, a literal that is a
#      `file:`, `qrc:`, `:/` or `image://` address, or a property that ends in
#      `iconSource` (the backend only fills those with `file:` URLs or "").
#      A Loader `source` must be a literal .qml file name.
# Exit status 0 when clean, 1 with one line per finding, 2 on a usage error.
set -euo pipefail

if ! command -v python3 >/dev/null 2>&1; then
    echo "check-qml-plaintext: python3 is needed" >&2
    exit 2
fi

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
if [ "$#" -eq 0 ]; then
    set -- "$root/apps/telamon-store/qml"
fi

exec python3 -I - "$@" <<'PY'
import os
import re
import sys

BANNED_FORMATS = re.compile(r"\b(RichText|StyledText|AutoText|MarkdownText)\b")
BANNED_CALLS = [
    (re.compile(r"\bQt\s*\.\s*openUrlExternally\b"), "Qt.openUrlExternally (open links through TelamonPortal)"),
    (re.compile(r"\bQt\s*\.\s*createQmlObject\b"), "Qt.createQmlObject (code built from a string)"),
    (re.compile(r"\bQt\s*\.\s*createComponent\b"), "Qt.createComponent (loads QML from a URL)"),
    (re.compile(r"\bQt\s*\.\s*include\b"), "Qt.include (loads script from a URL)"),
    (re.compile(r"\beval\s*\("), "eval("),
    (re.compile(r"\bnew\s+Function\b"), "new Function"),
    (re.compile(r"\bXMLHttpRequest\b"), "XMLHttpRequest (the QML layer makes no request)"),
    (re.compile(r"\bWebSocket\b"), "WebSocket (the QML layer makes no request)"),
    (re.compile(r"(?<![\w.])fetch\s*\("), "fetch( (the QML layer makes no request)"),
    (re.compile(r"\bon\s+(textFormat|source)\b"), "a Binding or other value source on textFormat/source (set them in the declaration)"),
    (re.compile(r"\btextFormat\s*="), None),  # handled below: needs `=` but not `==`
]
TEXT_TYPES = {"Text", "Label", "Heading"}
EDIT_TYPES = {"TextEdit", "TextArea"}
LOCAL_LITERAL = re.compile(r'^"(file:|qrc:|:/|image://)[^"]*"$')
ICON_PROP = re.compile(r'^[\w.\[\]]*iconSource(\s*\?\?\s*"")?$')
LOADER_LITERAL = re.compile(r'^"[\w./-]+\.qml"$')
# After these a `/` starts a regular expression, not a division.
REGEX_AFTER = set("(,=:[!&|?{};+-*%<>~^")
REGEX_WORDS = {"return", "typeof", "in", "of", "case", "void", "delete", "throw"}
LOCAL_TITLE_MARK = "check-qml: local-title"


def scan(src):
    """(code, masked, problems): `code` is the source with comments blanked,
    `masked` also has the inside of strings and regular expression literals
    blanked; both keep every offset and line break. A regular expression
    literal with a quote or `//` in it is a problem of its own: other tools
    and readers lose their way in it."""
    code, masked, problems = [], [], []
    i, n = 0, len(src)
    prev, word = "", ""

    def put(text, blank=False):
        shown = re.sub(r"[^\n]", " ", text) if blank else text
        code.append(shown)
        masked.append(shown)

    while i < n:
        c = src[i]
        if c in "\"'`":
            j = i + 1
            while j < n and src[j] != c and not (c != "`" and src[j] == "\n"):
                j += 2 if src[j] == "\\" else 1
            j = min(j + 1, n) if j < n and src[j] == c else min(j, n)
            code.append(src[i:j])
            masked.append(src[i] + re.sub(r"[^\n]", " ", src[i + 1:j - 1]) + src[j - 1] if j - i > 2 else src[i:j])
            i = j
            prev, word = c, ""
        elif src.startswith("//", i):
            j = src.find("\n", i)
            j = n if j < 0 else j
            put(src[i:j], True)
            i = j
        elif src.startswith("/*", i):
            j = src.find("*/", i + 2)
            j = n if j < 0 else j + 2
            put(src[i:j], True)
            i = j
        elif c == "/" and (prev == "" or prev in REGEX_AFTER or word in REGEX_WORDS):
            j, in_class = i + 1, False
            while j < n and src[j] != "\n":
                if src[j] == "\\":
                    j += 2
                    continue
                if src[j] == "[":
                    in_class = True
                elif src[j] == "]":
                    in_class = False
                elif src[j] == "/" and not in_class:
                    break
                j += 1
            if j < n and src[j] == "/":
                body = src[i + 1:j]
                if re.search(r"[\"'`]|//|/\*", body):
                    problems.append((src.count("\n", 0, i) + 1,
                                     "a regular expression literal with a quote or // in it: write it without (a string, a function), so the file can be checked"))
                code.append(src[i:j + 1])
                masked.append("/" + re.sub(r"[^\n]", " ", body) + "/")
                i = j + 1
                prev, word = "/", ""
            else:
                put(c)
                i += 1
                prev, word = c, ""
        else:
            put(c)
            if not c.isspace():
                if c.isalnum() or c == "_":
                    word = (word if prev.isalnum() or prev == "_" else "") + c
                else:
                    word = ""
                prev = c
            i += 1
    return "".join(code), "".join(masked), problems


def objects(masked):
    """(type, start, end) of each `Type {` block, `start` at the brace."""
    header = re.compile(r"([A-Za-z_][\w]*(?:\s*\.\s*[A-Za-z_]\w*)*)\s*\{")
    stack, result = [], []
    for i, c in enumerate(masked):
        if c == "{":
            kind = None
            for cand in header.finditer(masked, max(0, i - 120), i + 1):
                if cand.end() - 1 == i:
                    name = re.sub(r"\s+", "", cand.group(1))
                    before = masked[:cand.start()].rstrip()
                    if name.split(".")[-1][:1].isupper() and not before.endswith(("=", ")", "=>")):
                        kind = name
            stack.append([kind, i])
        elif c == "}" and stack:
            kind, start = stack.pop()
            if kind:
                result.append((kind, start, i))
    return result


def own_text(text, start, end):
    """The body of an object without the braces of anything nested in it."""
    body = list(text[start + 1:end])
    depth = 0
    for idx, ch in enumerate(body):
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
        elif depth > 0 and ch != "\n":
            body[idx] = " "
    return "".join(body)


def line_of(src, pos):
    return src.count("\n", 0, pos) + 1


def value_after(code, pos):
    """The expression that starts at `pos` (just after a colon or equals
    sign), which may start on a later line and run over several: up to a `;`,
    or a line end where the next line does not go on with it."""
    n = len(code)
    while pos < n and code[pos].isspace():
        pos += 1
    out, depth, i = [], 0, pos
    while i < n:
        c = code[i]
        if c in "\"'`":
            j = i + 1
            while j < n and code[j] != c:
                j += 2 if code[j] == "\\" else 1
            out.append(code[i:j + 1])
            i = j + 1
            continue
        if c in "([{":
            depth += 1
        elif c in ")]}":
            if depth == 0:
                break
            depth -= 1
        elif c == ";" and depth == 0:
            break
        elif c == "\n" and depth == 0:
            k = i
            while k < n and code[k].isspace():
                k += 1
            so_far = "".join(out).rstrip()
            nxt = code[k:k + 2]
            goes_on = so_far == "" or so_far[-1] in "?:+-*/&|=,.<>!" or nxt[:1] in "?:.+-*/&|" or nxt in ("??",)
            if k >= n or not goes_on:
                break
        out.append(c)
        i += 1
    return re.sub(r"\s+", " ", "".join(out)).strip()


def prop(own_m, own_c, name):
    """The value bound to `name:` in an object's own text, or None."""
    m = re.search(r"(?:^|[;\n{])[ \t]*" + re.escape(name) + r"[ \t]*:", own_m)
    return value_after(own_c, m.end()) if m else None


def source_ok(v, loader=False):
    return (
        v in ("", '""', "''")
        or bool(LOCAL_LITERAL.match(v))
        or bool(ICON_PROP.match(v))
        or (loader and bool(LOADER_LITERAL.match(v)))
    )


def top_level(expr, ops):
    """Index of the first top-level occurrence of one of `ops`, or -1."""
    depth, i = 0, 0
    while i < len(expr):
        c = expr[i]
        if c in "\"'`":
            j = i + 1
            while j < len(expr) and expr[j] != c:
                j += 2 if expr[j] == "\\" else 1
            i = j + 1
            continue
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        elif depth == 0:
            for op in ops:
                if expr.startswith(op, i):
                    return i
        i += 1
    return -1


def call_args(expr, name):
    """The text inside `name( ... )` when `expr` starts with it, and the rest."""
    if not expr.startswith(name + "("):
        return None
    depth, i = 0, len(name)
    while i < len(expr):
        c = expr[i]
        if c in "\"'`":
            j = i + 1
            while j < len(expr) and expr[j] != c:
                j += 2 if expr[j] == "\\" else 1
            i = j
        elif c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
            if depth == 0:
                return expr[len(name) + 1:i], expr[i + 1:]
        i += 1
    return None


def title_ok(expr):
    """A page title that is safe to hand to the navigation header: a string
    literal, qsTr() of literals (with .arg() of such things), headerTitle(...)
    or a choice between those. The condition of a choice is not looked at."""
    e = expr.strip()
    while e.startswith("(") and call_args("x" + e, "x") and call_args("x" + e, "x")[1].strip() == "":
        e = call_args("x" + e, "x")[0].strip()
    q = top_level(e, ["?"]) if "?" in e else -1
    while q != -1 and (e[q:q + 2] in ("??", "?.")):
        # `??` and `?.` are not the conditional operator.
        rest = e[q + 2:]
        k = top_level(rest, ["?"])
        q = -1 if k == -1 else q + 2 + k
    if q != -1:
        colon = top_level(e[q + 1:], [":"])
        if colon == -1:
            return False
        colon += q + 1
        return title_ok(e[q + 1:colon]) and title_ok(e[colon + 1:])
    if re.fullmatch(r'"(?:[^"\\]|\\.)*"|\'(?:[^\'\\]|\\.)*\'', e):
        return True
    hit = call_args(e, "headerTitle")
    if hit and hit[1].strip() == "":
        return True
    hit = call_args(e, "qsTr")
    if hit:
        args, rest = hit
        if not re.fullmatch(r'\s*(?:"(?:[^"\\]|\\.)*"|\'(?:[^\'\\]|\\.)*\')\s*(?:,\s*(?:"(?:[^"\\]|\\.)*"|\'(?:[^\'\\]|\\.)*\')\s*)*', args):
            return False
        rest = rest.strip()
        while rest:
            m = call_args(rest, ".arg")
            if not m:
                return False
            inner, rest = m[0].strip(), m[1].strip()
            if not (re.fullmatch(r"-?\d+(?:\.\d+)?", inner) or title_ok(inner)):
                return False
        return True
    return False


def check(path):
    findings = []
    raw = open(path, encoding="utf-8").read()
    code, masked, problems = scan(raw)
    findings += problems
    lines = raw.split("\n")

    for m in BANNED_FORMATS.finditer(masked):
        findings.append((line_of(code, m.start()), "text format %s (use Text.PlainText)" % m.group(1)))
    for rx, why in BANNED_CALLS:
        for m in rx.finditer(masked):
            if why is None:
                # `textFormat = x` but not `textFormat == x`
                if masked[m.end():m.end() + 1] == "=":
                    continue
                findings.append((line_of(code, m.start()), "textFormat assigned in code (set it in the declaration, to Text.PlainText)"))
            else:
                findings.append((line_of(code, m.start()), why))
    # `img.source = x` in a handler, but not `var source = x` or `source == x`.
    for m in re.finditer(r"(?<![\w$])(?:(?:var|let|const)\s+)?(\w+\s*\.\s*)?source\s*=(?!=)", masked):
        if m.group(0).lstrip().startswith(("var", "let", "const")):
            continue
        v = value_after(code, m.end())
        if not source_ok(v, loader=True):
            findings.append((line_of(code, m.start()), "source assigned in code from %s: only a file:/qrc:/image:// literal or an iconSource is allowed" % (v or "nothing")))

    for kind, s, e in objects(masked):
        last = kind.split(".")[-1]
        own_m = own_text(masked, s, e)
        own_c = own_text(code, s, e)
        line = line_of(code, s)
        if last in TEXT_TYPES:
            v = prop(own_m, own_c, "textFormat")
            if v is None:
                findings.append((line, "%s without textFormat: Text.PlainText (the default is AutoText)" % kind))
            elif v != "Text.PlainText":
                findings.append((line, "%s with textFormat: %s" % (kind, v)))
        elif last == "ToolTip" and "." in kind:
            findings.append((line, "%s decides for itself whether its text is markup: use TelamonToolTip" % kind))
        elif last in EDIT_TYPES:
            v = prop(own_m, own_c, "textFormat")
            if v is not None and v not in ("Text.PlainText", "TextEdit.PlainText"):
                findings.append((line, "%s with textFormat: %s" % (kind, v)))
        elif last in ("Image", "AnimatedImage", "Loader", "PropertyChanges", "Binding"):
            if last == "Binding":
                p = prop(own_m, own_c, "property")
                if p is not None and re.search(r"textFormat|source", p):
                    findings.append((line, "Binding on %s: set textFormat and source in the declaration" % p))
                continue
            if last == "PropertyChanges":
                tf = prop(own_m, own_c, "textFormat")
                if tf is not None and tf != "Text.PlainText":
                    findings.append((line, "PropertyChanges sets textFormat: %s" % tf))
            v = prop(own_m, own_c, "source")
            if v is not None and not source_ok(v, loader=(last in ("Loader", "PropertyChanges"))):
                findings.append((line, "%s source %s is not a file:/qrc:/image:// literal or an iconSource%s" % (
                    kind, v, " (a Loader needs a literal .qml file)" if last == "Loader" else "")))
        if last == "TelamonPage" or last.endswith("Page"):
            m = re.search(r"(?:^|[;\n{])[ \t]*title[ \t]*:", own_m)
            if m:
                v = value_after(own_c, m.end())
                tline = line_of(code, s + 1 + m.start() + (1 if own_m[m.start()] in ";\n{" else 0))
                prev_line = lines[tline - 2] if tline >= 2 else ""
                if not title_ok(v) and LOCAL_TITLE_MARK not in prev_line:
                    findings.append((tline, "%s title %s is not a string literal, qsTr() of literals or headerTitle(): the navigation header shows it as AutoText (a local-only title: put '// %s' on the line above)" % (kind, v, LOCAL_TITLE_MARK)))
    return sorted(set(findings))


files = []
for arg in sys.argv[1:]:
    if os.path.isdir(arg):
        for dirpath, _dirs, names in os.walk(arg):
            files += [os.path.join(dirpath, n) for n in sorted(names) if n.endswith(".qml")]
    elif os.path.isfile(arg):
        files.append(arg)
    else:
        print("check-qml-plaintext: no such file or folder: %s" % arg, file=sys.stderr)
        sys.exit(2)
if not files:
    print("check-qml-plaintext: no .qml files found", file=sys.stderr)
    sys.exit(2)

bad = 0
for f in sorted(files):
    for line, why in check(f):
        print("%s:%d: %s" % (f, line, why))
        bad += 1
if bad:
    print("check-qml-plaintext: %d finding(s) in %d file(s) checked" % (bad, len(files)), file=sys.stderr)
    sys.exit(1)
print("check-qml-plaintext: %d file(s) clean" % len(files))
PY
