"""Phase 2 codemod: rename Spanish identifiers to English across the Rust sources.

Authorized by the user for branch chore/translate-to-english (2026-09-24).

  python3 codemod.py --dry-run   # report only
  python3 codemod.py             # write files

Never touches: string/char literal contents (except `{ident}` format args),
comments outside `backticks`/[links], serde/clap-derived fields and variants,
#[tauri::command] fns, `mod` names, test fns whose name is an insta snapshot.
Per-file shadowing guard: if the English target already exists as an
identifier in the file, that rename is skipped in that file and logged.
"""
import collections
import pathlib
import re
import sys

ROOT = pathlib.Path("/home/oscar/work/wot/projects/high/norte")
D = pathlib.Path(__file__).parent
DRY = "--dry-run" in sys.argv

KEYWORDS = set(
    """as break const continue crate else enum extern false fn for if impl in let loop match mod
    move mut pub ref return self Self static struct super trait true type unsafe use where while
    async await dyn abstract become box do final macro override priv typeof unsized virtual yield try gen""".split()
)

# ---------------------------------------------------------------- the map
words = dict(l.split("\t") for l in (D / "words_map.tsv").read_text().splitlines() if "\t" in l)
if (D / "words_map2.tsv").exists():
    words.update(dict(l.split("\t") for l in (D / "words_map2.tsv").read_text().splitlines() if "\t" in l))
ident_map = {}
for name in ("phrases_A.tsv", "phrases_B.tsv", "phrases_C.tsv", "phrases_D.tsv"):
    p = D / name
    if p.exists():
        for l in p.read_text().splitlines():
            if "\t" in l:
                a, b = l.split("\t", 1)
                if a != b.strip():
                    ident_map[a] = b.strip()


def split_parts(t):
    return [x for x in re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", t).split("_")]


def style(t):
    core = t.strip("_")
    if core.upper() == core and (len(core) > 1 and any(c.isalpha() for c in core)):
        return "SCREAMING"
    if core[:1].isupper() and "_" not in core:
        return "Camel"
    return "snake"


def assemble(parts, st, lead, trail):
    parts = [p for p in parts if p]
    if st == "SCREAMING":
        s = "_".join(p.upper() for p in parts)
    elif st == "Camel":
        s = "".join(q[:1].upper() + q[1:] for p in parts for q in p.split("_"))
    else:
        s = "_".join(p.lower() for p in parts)
    return lead + s + trail


unmapped = collections.Counter()
for line in (D / "candidates.tsv").read_text().splitlines():
    ident = line.split("\t")[0]
    if ident in ident_map:
        continue
    lead = re.match(r"^_*", ident).group(0)
    trail = re.search(r"_*$", ident).group(0)
    parts = split_parts(ident.strip("_"))
    out, changed = [], False
    for p in parts:
        w = words.get(p.lower())
        if w:
            out.append(w)
            changed = True
        else:
            out.append(p.lower() if style(ident) != "Camel" else p)
            if p.lower() not in words and p.lower() == p.lower():
                pass
    if not changed:
        unmapped[ident] += 1
        continue
    new = assemble(out, style(ident), lead, trail)
    if new in KEYWORDS:
        new += "_"
    if new != ident:
        ident_map[ident] = new

if (D / "overrides.tsv").exists():
    for l in (D / "overrides.tsv").read_text().splitlines():
        if "\t" in l:
            a, b = l.split("\t")
            ident_map[a] = b
ONLY = next((a.split("=", 1)[1] for a in sys.argv if a.startswith("--only=")), None)

# ---------------------------------------------------------------- exclusions
exclude = set()
reason = {}
for k in (D / "stop.txt").read_text().split():
    exclude.add(k)
    reason[k] = "stop list: English/tech meaning (serde::ser, rustls Der, MS-DOS time)"
files = [p for p in list((ROOT / "crates").rglob("*.rs")) + list((ROOT / "plugins").rglob("*.rs")) if "target" not in p.parts]
sources = {p: p.read_text() for p in files}

derive_re = re.compile(r"#\[derive\(([^)]*)\)\]")
item_re = re.compile(r"\b(struct|enum)\s+(\w+)[^{;]*\{")
WIRE = re.compile(r"Serialize|Deserialize|JsonSchema|Parser|Args|Subcommand|ValueEnum")
for p, s in sources.items():
    for m in derive_re.finditer(s):
        if not WIRE.search(m.group(1)):
            continue
        im = item_re.search(s, m.end())
        if not im or im.start() - m.end() > 400:
            continue
        i, depth = im.end(), 1
        while depth and i < len(s):
            depth += {"{": 1, "}": -1}.get(s[i], 0)
            i += 1
        body = re.sub(r"//[^\n]*", "", s[im.end():i])
        for n in re.findall(r"^\s*(?:pub(?:\([^)]*\))?\s+)?([A-Za-z_]\w*)\s*[:,({]", body, re.M):
            if n in ident_map:
                exclude.add(n)
                reason[n] = f"serde/clap field or variant in {p.relative_to(ROOT)}"
    for m in re.finditer(r"#\[tauri::command[^\]]*\]\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(\w+)", s):
        if m.group(1) in ident_map:
            exclude.add(m.group(1))
            reason[m.group(1)] = "tauri command"
snap_moves = []
for snap in ROOT.glob("crates/**/snapshots/*.snap"):
    base = snap.stem.split("__")[-1].split("@")[0]
    for cand in (base, re.sub(r"-\d+$", "", base)):
        if cand in ident_map and cand not in exclude:
            snap_moves.append((snap, snap.with_name(snap.name.replace(cand, ident_map[cand], 1))))
            break

active = {k: v for k, v in ident_map.items() if k not in exclude}

# ---------------------------------------------------------------- lexer
TOKEN = re.compile(
    r"""(?P<lc>//[^\n]*)"""
    r"""|(?P<bc>/\*)"""
    r"""|(?P<raw>b?r(?P<h>\#*)")"""
    r"""|(?P<str>b?"(?:\\.|[^"\\])*")"""
    r"""|(?P<chr>b?'(?:\\.|[^'\\\n])')"""
    r"""|(?P<life>'[A-Za-z_]\w*)"""
    r"""|(?P<id>[A-Za-z_]\w*)""",
    re.S,
)
IDENT = re.compile(r"[A-Za-z_]\w*")
FMT = re.compile(r"\{([A-Za-z_]\w*)((?::[^{}]*)?)\}")


def code_idents(s):
    out = set()
    pos = 0
    while True:
        m = TOKEN.search(s, pos)
        if not m:
            return out
        if m.group("bc"):
            end = block_end(s, m.start())
            pos = end
            continue
        if m.group("raw"):
            end = s.find('"' + m.group("h"), m.end())
            pos = len(s) if end < 0 else end + 1 + len(m.group("h"))
            continue
        if m.group("id"):
            out.add(m.group("id"))
        pos = m.end()


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


def rename_in_comment(text, m):
    # only inside `backticks` and [link] spans
    def sub_span(sm):
        return IDENT.sub(lambda im: m.get(im.group(0), im.group(0)), sm.group(0))

    return re.sub(r"`[^`\n]*`|\[[^\]\n]*\]", sub_span, text)


def blank(s):
    """Same-length copy of `s` with comments and literals blanked (newlines kept)."""
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
        i = m.end()
        depth = 0
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


def scope_conflict(b, spans, k, t):
    kspans = {innermost(spans, m.start()) for m in re.finditer(r"\b%s\b" % re.escape(k), b)}
    kspans.discard(None)
    if not kspans:
        return False
    for m in re.finditer(r"\b%s\b" % re.escape(t), b):
        before = b[:m.start()].rstrip()
        after = b[m.end():].lstrip()
        # `x.t`, `T::t`, `t::x`, `t!`: members, paths and macros do not live in
        # the local value namespace, so they cannot shadow or be shadowed.
        if before.endswith((".", "::")) or after.startswith(("::", "!")):
            continue
        if innermost(spans, m.start()) in kspans:
            return True
    return False


log = []
# A module rename whose English file/dir already exists would half-apply: keep it Spanish.
for p in files:
    for name, parent in ((p.stem, p.parent), (p.parent.name, p.parent.parent)):
        if name in active:
            t = active[name]
            if (parent / (t + ".rs")).exists() or (parent / t).is_dir():
                exclude.add(name)
                reason[name] = f"module target exists next to {p.relative_to(ROOT)}"
                active.pop(name, None)
changed_files = 0
changed_tokens = 0
for p, s in sources.items():
    if ONLY and not str(p.relative_to(ROOT)).startswith(ONLY):
        continue
    present = code_idents(s)
    local = {}
    targets = collections.Counter()
    for k in present & active.keys():
        targets[active[k]] += 1
    b = spans = None
    for k in present & active.keys():
        t = active[k]
        if t in present:
            if b is None:
                b = blank(s)
                spans = fn_spans(b)
            if scope_conflict(b, spans, k, t):
                log.append(f"SHADOW\t{p.relative_to(ROOT)}\t{k}\t{t}")
                continue
        if targets[t] > 1:
            log.append(f"TWO-TO-ONE\t{p.relative_to(ROOT)}\t{k}\t{t}")
            continue
        local[k] = t
    # comment/doc references to renamed items defined elsewhere: always follow the global map
    cmap = dict(active)
    cmap.update(local)
    if not local and not any(k in s for k in ()):
        pass
    out, pos, n = [], 0, 0
    while True:
        m = TOKEN.search(s, pos)
        if not m:
            out.append(s[pos:])
            break
        out.append(s[pos:m.start()])
        if m.group("lc"):
            new = rename_in_comment(m.group(0), {k: v for k, v in cmap.items() if k not in exclude})
            n += new != m.group(0)
            out.append(new)
            pos = m.end()
        elif m.group("bc"):
            end = block_end(s, m.start())
            seg = s[m.start():end]
            new = rename_in_comment(seg, {k: v for k, v in cmap.items() if k not in exclude})
            n += new != seg
            out.append(new)
            pos = end
        elif m.group("raw"):
            end = s.find('"' + m.group("h"), m.end())
            end = len(s) if end < 0 else end + 1 + len(m.group("h"))
            out.append(s[m.start():end])
            pos = end
        elif m.group("str"):
            lit = m.group(0)
            new = FMT.sub(lambda fm: "{" + local.get(fm.group(1), fm.group(1)) + fm.group(2) + "}", lit)
            n += new != lit
            out.append(new)
            pos = m.end()
        elif m.group("id"):
            t = m.group("id")
            if t in local:
                out.append(local[t])
                n += 1
            else:
                out.append(t)
            pos = m.end()
        else:
            out.append(m.group(0))
            pos = m.end()
    new_s = "".join(out)
    if new_s != s:
        changed_files += 1
        changed_tokens += n
        if not DRY:
            p.write_text(new_s)

# ---------------------------------------------------------------- file / module renames
import subprocess

moves = []
for p in sorted(files, key=lambda x: -len(x.parts)):
    if p.stem in ("main", "lib", "mod", "build"):
        continue
    if p.stem in active:
        moves.append((p, p.with_name(active[p.stem] + ".rs")))
for d in sorted({q.parent for q in files}, key=lambda x: -len(x.parts)):
    if d.name in active and ((d / "mod.rs").exists() or (d / "main.rs").exists() or (d.parent / (d.name + ".rs")).exists() or any(d.glob("*.rs"))):
        moves.append((d, d.with_name(active[d.name])))
moves = snap_moves + moves
if ONLY:
    moves = [(a, z) for a, z in moves if str(a.relative_to(ROOT)).startswith(ONLY)]
for src, dst in moves:
    log.append(f"MOVE\t{src.relative_to(ROOT)}\t{dst.relative_to(ROOT)}")
    if not DRY:
        subprocess.run(["git", "mv", str(src), str(dst)], cwd=ROOT, check=True)

(D / "codemod.log").write_text(
    "\n".join(log)
    + "\n\n# excluded\n"
    + "\n".join(f"EXCLUDED\t{k}\t{active.get(k, ident_map.get(k))}\t{reason[k]}" for k in sorted(exclude))
    + "\n\n# unmapped\n"
    + "\n".join(f"UNMAPPED\t{k}" for k in sorted(unmapped))
    + "\n"
)
(D / "ident_map.tsv").write_text("\n".join(f"{k}\t{v}" for k, v in sorted(active.items())) + "\n")
print(f"map {len(ident_map)} active {len(active)} excluded {len(exclude)} unmapped {len(unmapped)}")
print(f"files {changed_files} replacements {changed_tokens} guard-skips {len(log)}  dry={DRY}")
