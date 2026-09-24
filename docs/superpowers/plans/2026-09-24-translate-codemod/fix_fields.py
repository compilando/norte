"""Fix struct-literal / pattern field names the compiler reports (E0026, E0559, E0560),
at the exact line:col it gives. `spanish: x` -> `english: x`; shorthand
`spanish` -> `english: spanish`. The English name comes from ident_map.tsv."""
import pathlib
import re

ROOT = pathlib.Path("/home/oscar/work/wot/projects/high/norte")
D = pathlib.Path(__file__).parent
active = dict(l.split("\t") for l in (D / "ident_map.tsv").read_text().splitlines() if "\t" in l)

fixes = {}
for l in (D / "errs.txt").read_text().splitlines():
    m = re.match(r"(.+?):(\d+):(\d+): error\[(E0026|E0559|E0560)\].*?named `(\w+)`", l)
    if m:
        f, line, col, _, name = m.groups()
        fixes.setdefault(f, []).append((int(line), int(col), name))
    m = re.match(r"(.+?):(\d+):(\d+): error\[E0026\].*?fields named (.*?):", l)
    if m:
        f, line, col, names = m.groups()
        for name in re.findall(r"`(\w+)`", names):
            fixes.setdefault(f, []).append((int(line), 0, name))

done = 0
# Bare calls to a function whose definition was renamed: `x(` -> `english(`.
calls = {}
for l in (D / "errs.txt").read_text().splitlines():
    m = re.match(r"(.+?):\d+:\d+: error\[E0425\]: cannot find function `(\w+)`", l)
    if m and m.group(2) in active:
        calls.setdefault(m.group(1), set()).add(m.group(2))
for f, names in calls.items():
    p = ROOT / f
    s = p.read_text()
    for n in names:
        s, k = re.subn(r"(?<![\w.:])%s(\s*\()" % re.escape(n), active[n] + r"\1", s)
        done += k
    p.write_text(s)
fixes = {f: [x for x in v if not (x[1] == 0 and False)] for f, v in fixes.items()}
seen = set()
for f, items in fixes.items():
    p = ROOT / f
    lines = p.read_text().split("\n")
    for line, col, name in sorted(set(items), reverse=True):
        if (f, line, name) in seen:
            continue
        seen.add((f, line, name))
        t = active.get(name)
        if not t:
            print("no map for", name, f, line)
            continue
        i = line - 1
        # search this line and the next few (multi-field patterns span lines)
        for j in range(i, min(i + 8, len(lines))):
            s = lines[j]
            m = re.search(r"(?<![\w.])%s\b(\s*)(:(?!:))?" % re.escape(name), s)
            if not m:
                continue
            if m.group(2):
                new = s[:m.start()] + t + s[m.start() + len(name):]
            else:
                new = s[:m.start()] + f"{t}: {name}" + s[m.start() + len(name):]
            lines[j] = new
            done += 1
            break
        else:
            print("not found", name, f, line)
    p.write_text("\n".join(lines))
print("fixed", done)
