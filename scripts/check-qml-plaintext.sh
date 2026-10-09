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
]
TEXT_TYPES = {"Text", "Label", "Heading"}
EDIT_TYPES = {"TextEdit", "TextArea"}
LOCAL_LITERAL = re.compile(r'^"(file:|qrc:|:/|image://)[^"]*"$')
ICON_PROP = re.compile(r'^[\w.\[\]]*iconSource(\s*\?\?\s*"")?$')
LOADER_LITERAL = re.compile(r'^"[\w./-]+\.qml"$')


def strip_comments(src):
    """Blanks // and /* */ comments (keeping line numbers) outside strings."""
    out = []
    i, n = 0, len(src)
    while i < n:
        c = src[i]
        if c in "\"'`":
            q = c
            j = i + 1
            while j < n and src[j] != q:
                j += 2 if src[j] == "\\" else 1
            out.append(src[i:j + 1])
            i = j + 1
        elif src.startswith("//", i):
            j = src.find("\n", i)
            j = n if j < 0 else j
            out.append(" " * (j - i))
            i = j
        elif src.startswith("/*", i):
            j = src.find("*/", i + 2)
            j = n if j < 0 else j + 2
            out.append(re.sub(r"[^\n]", " ", src[i:j]))
            i = j
        else:
            out.append(c)
            i += 1
    return "".join(out)


def mask_strings(src):
    """The source with the inside of string literals replaced by spaces."""
    out = []
    i, n = 0, len(src)
    while i < n:
        c = src[i]
        if c in "\"'`":
            q = c
            j = i + 1
            while j < n and src[j] != q:
                j += 2 if src[j] == "\\" else 1
            out.append(q + re.sub(r"[^\n]", " ", src[i + 1:j]) + q)
            i = j + 1
        else:
            out.append(c)
            i += 1
    return "".join(out)


def objects(masked):
    """Yields (type, start, own_text_start, end, line) for each `Type {` block."""
    header = re.compile(r"([A-Za-z_][\w]*(?:\s*\.\s*[A-Za-z_]\w*)*)\s*\{")
    stack = []
    result = []
    i = 0
    n = len(masked)
    while i < n:
        c = masked[i]
        if c == "{":
            m = None
            for cand in header.finditer(masked, max(0, i - 120), i + 1):
                if cand.end() - 1 == i:
                    m = cand
            kind = None
            if m:
                name = re.sub(r"\s+", "", m.group(1))
                last = name.split(".")[-1]
                before = masked[:m.start()].rstrip()
                # `key: Type {` and bare `Type {` are objects; `on: {` is code.
                if last[:1].isupper() and not before.endswith(("=", ")", "=>")):
                    kind = name
            stack.append([kind, i])
        elif c == "}":
            if stack:
                kind, start = stack.pop()
                if kind:
                    result.append((kind, start, i))
        i += 1
    return result


def own_text(masked, start, end):
    """The body of an object without the braces of anything nested in it."""
    body = list(masked[start + 1:end])
    # Blank every nested {...} group (objects, functions, literals).
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


def prop(own, name):
    m = re.search(r"(?:^|[;\n])[ \t]*" + re.escape(name) + r"[ \t]*:[ \t]*([^;\n]+)", own)
    return m.group(1).strip() if m else None


def check(path):
    findings = []
    raw = open(path, encoding="utf-8").read()
    code = strip_comments(raw)
    masked = mask_strings(code)

    for m in BANNED_FORMATS.finditer(masked):
        findings.append((line_of(code, m.start()), "text format %s (use Text.PlainText)" % m.group(1)))
    for rx, why in BANNED_CALLS:
        for m in rx.finditer(masked):
            findings.append((line_of(code, m.start()), why))

    objs = objects(masked)
    for kind, s, e in objs:
        last = kind.split(".")[-1]
        own_m = own_text(masked, s, e)
        own_c = own_text(code, s, e)
        line = line_of(code, s)
        if last in TEXT_TYPES:
            v = prop(own_m, "textFormat")
            if v is None:
                findings.append((line, "%s without textFormat: Text.PlainText (the default is AutoText)" % kind))
            elif v != "Text.PlainText":
                findings.append((line, "%s with textFormat: %s" % (kind, v)))
        elif last == "ToolTip" and "." in kind:
            findings.append((line, "%s decides for itself whether its text is markup: use TelamonToolTip" % kind))
        elif last in EDIT_TYPES:
            v = prop(own_m, "textFormat")
            if v is not None and v not in ("Text.PlainText", "TextEdit.PlainText"):
                findings.append((line, "%s with textFormat: %s" % (kind, v)))
        elif last == "TelamonPage":
            v = prop(own_c, "title")
            if v is not None and re.search(r"\b(info|modelData|detail|plan|preview)\b|\.name\b", v) and "headerTitle(" not in v:
                findings.append((line, "TelamonPage title %s comes from data: the navigation stack header shows it as AutoText, so pass it through headerTitle()" % v))
        elif last in ("Image", "AnimatedImage"):
            v = prop(own_c, "source")
            if v is None or v == '""':
                continue
            if not (LOCAL_LITERAL.match(v) or ICON_PROP.match(v)):
                findings.append((line, "%s source %s is not a file:/qrc:/image:// literal or an iconSource" % (kind, v)))
        elif last == "Loader":
            v = prop(own_c, "source")
            if v is not None and not LOADER_LITERAL.match(v):
                findings.append((line, "Loader source %s is not a literal .qml file" % v))
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
