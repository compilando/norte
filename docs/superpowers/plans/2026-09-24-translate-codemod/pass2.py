"""Phase 2, pass 2: for the renames the shadow guard skipped in a file, rename
only the occurrences that cannot shadow a local:

- member access and paths: `.x`, `::x`, `x::`, macros `x!`;
- definitions: `fn x`, `struct x`, `enum x`, `type x`, `trait x`, `const x`,
  `static x`, `mod x`, `macro_rules! x`;
- CamelCase / SCREAMING identifiers (types, variants, consts — never locals);
- `x:` field positions, only when the innermost enclosing `{` opens a struct
  body or a `Type { … }` literal/pattern.

Bare snake_case tokens stay as they are; shorthand fields are logged.
"""
import pathlib
import re

ROOT = pathlib.Path("/home/oscar/work/wot/projects/high/norte")
D = pathlib.Path(__file__).parent

log = (D / "codemod.log").read_text().splitlines()
moves = {}
for l in log:
    if l.startswith("MOVE\t"):
        _, a, b = l.split("\t")
        moves[a] = b


def moved(rel):
    if rel in moves:
        return moves[rel]
    for a, b in sorted(moves.items(), key=lambda kv: -len(kv[0])):
        if rel.startswith(a + "/"):
            return b + rel[len(a):]
    return rel


todo = {}
for l in log:
    if l.startswith(("SHADOW\t", "TWO-TO-ONE\t")):
        _, f, k, t = l.split("\t")
        todo.setdefault(moved(f), {})[k] = t

TOKEN = re.compile(
    r"""(?P<lc>//[^\n]*)|(?P<bc>/\*)|(?P<raw>b?r(?P<h>\#*)")|(?P<str>b?"(?:\\.|[^"\\])*")"""
    r"""|(?P<chr>b?'(?:\\.|[^'\\\n])')|(?P<life>'[A-Za-z_]\w*)|(?P<id>[A-Za-z_]\w*)""",
    re.S,
)
DEF = {"fn", "struct", "enum", "type", "trait", "const", "static", "mod", "union"}


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


STRUCT_HEAD = re.compile(r"\b(struct|union)\s+\w+[^{};]*$")
LITERAL_HEAD = re.compile(r"(\b[A-Z]\w*(<[^{};]*>)?|\bSelf)\s*$")


NOT_FIELD_HEAD = re.compile(r"\b(impl|trait|enum|mod|fn|match|if|else|loop|while|for|unsafe|async|move)\b[^{};]*$")


def open_bracket(b, pos):
    """Innermost unclosed `{`, `(` or `[` before `pos`: (index, char)."""
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


total, shorthand = 0, []
for rel, m in todo.items():
    p = ROOT / rel
    if not p.exists():
        print("MISSING", rel)
        continue
    s = p.read_text()
    b = blank(s)
    out, pos, prev_tok, n = [], 0, "", 0
    while True:
        mt = TOKEN.search(s, pos)
        if not mt:
            out.append(s[pos:])
            break
        out.append(s[pos:mt.start()])
        if mt.group("bc"):
            end = block_end(s, mt.start())
            out.append(s[mt.start():end])
            pos = end
            continue
        if mt.group("raw"):
            end = s.find('"' + mt.group("h"), mt.end())
            end = len(s) if end < 0 else end + 1 + len(mt.group("h"))
            out.append(s[mt.start():end])
            pos = end
            continue
        tok = mt.group(0)
        if mt.group("id") and tok in m:
            before = b[:mt.start()].rstrip()
            after = b[mt.end():].lstrip()
            member = (before.endswith(".") and not before.endswith("..")) or before.endswith("::") or after.startswith(("::", "!"))
            definition = prev_tok in DEF or before.endswith("macro_rules!")
            typelike = tok[:1].isupper()
            field = after.startswith(":") and not after.startswith("::") and field_context(b, mt.start())
            if member or definition or typelike or field:
                out.append(m[tok])
                n += 1
            else:
                if (after.startswith((",", "}")) or after.startswith("..")) and field_context(b, mt.start()):
                    shorthand.append(f"{rel}:{s.count(chr(10), 0, mt.start()) + 1}\t{tok}")
                out.append(tok)
        else:
            out.append(tok)
        if mt.group("id"):
            prev_tok = tok
        pos = mt.end()
    new = "".join(out)
    if new != s:
        p.write_text(new)
        total += n
# String-literal paths that name renamed modules (the codemod never edits literals).
FIXUPS = [
    ("crates/norte-ui-host/tests/controller/main.rs", '"../backend_falso/mod.rs"', '"../backend_fake/mod.rs"'),
    ("crates/norte-frontend/src/session.rs", 'with = "columnas"', 'with = "columns"'),
    ("crates/norte-frontend/src/session.rs", '"populares::deserialize"', '"popular::deserialize"'),
]
for rel, a, z in FIXUPS:
    p = ROOT / rel
    s = p.read_text()
    if a in s:
        p.write_text(s.replace(a, z))
        print("fixup", rel, a, "->", z)

(D / "pass2_shorthand.txt").write_text("\n".join(shorthand) + "\n")
print("pass2 replacements:", total, "files:", len(todo), "shorthand fields left:", len(shorthand))
