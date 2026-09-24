"""Phase 2, residual pass: the Spanish locals the shadowing guard held back.

For each file and each residual identifier, pick the first English alternative
(alternatives.tsv) that is not already a bare identifier in any function where
the Spanish one appears, and rename only the bare (local-variable) occurrences.
Members (`.x`, `x::`), definitions and struct fields keep their name; a
shorthand field `T { x }` becomes `T { x: alt }`; `{x}` in format strings follows.
"""
import pathlib
import re

ROOT = pathlib.Path("/home/oscar/work/wot/projects/high/norte")
D = pathlib.Path(__file__).parent
alts = {}
for l in (D / "alternatives.tsv").read_text().splitlines():
    if "\t" in l:
        k, v = l.split("\t")
        alts[k] = v.split()

TOKEN = re.compile(
    r"""(?P<lc>//[^\n]*)|(?P<bc>/\*)|(?P<raw>b?r(?P<h>\#*)")|(?P<str>b?"(?:\\.|[^"\\])*")"""
    r"""|(?P<chr>b?'(?:\\.|[^'\\\n])')|(?P<life>'[A-Za-z_]\w*)|(?P<id>[A-Za-z_]\w*)""",
    re.S,
)
DEF = {"fn", "struct", "enum", "type", "trait", "const", "static", "mod", "union", "macro_rules"}
FMT = re.compile(r"\{([A-Za-z_]\w*)((?::[^{}]*)?)\}")


def block_end(s, start):
    depth, i = 0, start
    while i < len(s):
        if s.startswith("/*", i):
            depth += 1
            i += 2
        elif s.startswith("*/", i):
            depth -= 1
            i += 2
            if depth == 0:
                return i
        else:
            i += 1
    return len(s)


def blank(s):
    out, pos = [], 0
    keep = lambda seg: re.sub(r"[^\n]", " ", seg)
    while True:
        m = TOKEN.search(s, pos)
        if not m:
            out.append(s[pos:])
            return "".join(out)
        out.append(s[pos:m.start()])
        if m.group("bc"):
            end = block_end(s, m.start())
            out.append(keep(s[m.start():end]))
            pos = end
        elif m.group("raw"):
            end = s.find('"' + m.group("h"), m.end())
            end = len(s) if end < 0 else end + 1 + len(m.group("h"))
            out.append(keep(s[m.start():end]))
            pos = end
        elif m.group("lc") or m.group("str") or m.group("chr"):
            out.append(keep(m.group(0)))
            pos = m.end()
        else:
            out.append(m.group(0))
            pos = m.end()


def fn_spans(b):
    spans = []
    for m in re.finditer(r"\bfn\s+[A-Za-z_]\w*", b):
        i, depth = m.end(), 0
        while i < len(b):
            c = b[i]
            if c in "(<[":
                depth += 1
            elif c in ")>]":
                depth -= 1
            elif c == ";" and depth <= 0:
                break
            elif c == "{" and depth <= 0:
                j, d = i, 0
                while j < len(b):
                    d += {"{": 1, "}": -1}.get(b[j], 0)
                    j += 1
                    if d == 0:
                        break
                spans.append((i, j))
                break
            i += 1
    return spans


def innermost(spans, pos):
    best = None
    for a, z in spans:
        if a <= pos < z and (best is None or z - a < best[1] - best[0]):
            best = (a, z)
    return best


STRUCT_HEAD = re.compile(r"\b(struct|union)\s+\w+[^{};]*$")
NOT_FIELD_HEAD = re.compile(r"\b(impl|trait|enum|mod|fn|match|if|else|loop|while|for|unsafe|async|move)\b[^{};]*$")
LITERAL_HEAD = re.compile(r"(\b[A-Z]\w*(<[^{};]*>)?|\bSelf)\s*$")


def open_bracket(b, pos):
    depth = {"{": 0, "(": 0, "[": 0}
    close = {"}": "{", ")": "(", "]": "["}
    i = pos - 1
    while i >= 0:
        c = b[i]
        if c in close:
            depth[close[c]] += 1
        elif c in depth:
            if depth[c] == 0:
                return i, c
            depth[c] -= 1
        i -= 1
    return -1, ""


def field_context(b, pos):
    ob, kind = open_bracket(b, pos)
    if ob < 0 or kind != "{":
        return False
    head = b[max(0, ob - 300):ob]
    if STRUCT_HEAD.search(head):
        return True
    if NOT_FIELD_HEAD.search(head):
        return False
    return bool(LITERAL_HEAD.search(head.rstrip()))


def classify(b, start, end, prev_tok):
    before = b[:start].rstrip()
    after = b[end:].lstrip()
    if before.endswith((".", "::")) or after.startswith(("::", "!")) or prev_tok in DEF:
        return "keep"
    if after.startswith(":") and not after.startswith("::") and field_context(b, start):
        return "keep"  # a field name
    if (after.startswith((",", "}")) or after.startswith("..")) and before.endswith(("{", ",")) and field_context(b, start):
        return "shorthand"
    return "bare"


files = [p for p in list((ROOT / "crates").rglob("*.rs")) + list((ROOT / "plugins").rglob("*.rs")) if "target" not in p.parts]
report, total = [], 0
for p in files:
    s = p.read_text()
    b = blank(s)
    ids = set(re.findall(r"\b[A-Za-z_]\w*\b", b))
    here = [k for k in alts if k in ids]
    if not here:
        continue
    spans = fn_spans(b)
    # bare identifiers per function span
    bare_in = {}
    prev = ""
    for m in re.finditer(r"\b[A-Za-z_]\w*\b", b):
        t = m.group(0)
        kind = classify(b, m.start(), m.end(), prev)
        if kind != "keep":
            bare_in.setdefault(innermost(spans, m.start()), set()).add(t)
        prev = t
    chosen = {}
    for k in here:
        kspans = {innermost(spans, m.start()) for m in re.finditer(r"\b%s\b" % re.escape(k), b)}
        for t in alts[k]:
            if t in chosen.values():
                continue
            if any(t in bare_in.get(sp, set()) for sp in kspans):
                continue
            chosen[k] = t
            break
        else:
            report.append(f"NO-ALT\t{p.relative_to(ROOT)}\t{k}")
    if not chosen:
        continue
    out, pos, prev, n = [], 0, "", 0
    while True:
        m = TOKEN.search(s, pos)
        if not m:
            out.append(s[pos:])
            break
        out.append(s[pos:m.start()])
        if m.group("bc"):
            end = block_end(s, m.start())
            out.append(s[m.start():end])
            pos = end
            continue
        if m.group("raw"):
            end = s.find('"' + m.group("h"), m.end())
            end = len(s) if end < 0 else end + 1 + len(m.group("h"))
            out.append(s[m.start():end])
            pos = end
            continue
        tok = m.group(0)
        if m.group("str"):
            new = FMT.sub(lambda fm: "{" + chosen.get(fm.group(1), fm.group(1)) + fm.group(2) + "}", tok)
            out.append(new)
            n += new != tok
        elif m.group("id") and tok in chosen:
            kind = classify(b, m.start(), m.end(), prev)
            if kind == "bare":
                out.append(chosen[tok])
                n += 1
            elif kind == "shorthand":
                out.append(f"{tok}: {chosen[tok]}")
                n += 1
            else:
                out.append(tok)
        else:
            out.append(tok)
        if m.group("id"):
            prev = tok
        pos = m.end()
    new_s = "".join(out)
    if new_s != s:
        p.write_text(new_s)
        total += n
        report.append(f"DONE\t{p.relative_to(ROOT)}\t" + ",".join(f"{k}->{v}" for k, v in chosen.items()))
(D / "residual.log").write_text("\n".join(report) + "\n")
print("residual replacements:", total, "| no-alt:", sum(1 for r in report if r.startswith("NO-ALT")))
