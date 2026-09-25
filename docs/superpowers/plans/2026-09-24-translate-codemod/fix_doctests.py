"""Rename identifiers inside ``` code fences of doc comments (`///`, `//!`), which
the codemod skipped. Only identifiers that no longer exist anywhere in the code
(their definition was renamed) are touched."""
import pathlib
import re
import sys

sys.path.insert(0, str(pathlib.Path(__file__).parent))
ROOT = pathlib.Path("/home/oscar/work/wot/projects/high/norte")
D = pathlib.Path(__file__).parent
active = dict(l.split("\t") for l in (D / "ident_map.tsv").read_text().splitlines() if "\t" in l)

files = [p for p in list((ROOT / "crates").rglob("*.rs")) + list((ROOT / "plugins").rglob("*.rs")) if "target" not in p.parts]
lit = re.compile(r'r#*"(?:.|\n)*?"#*|b?"(?:\\.|[^"\\])*"|b?\'(?:\\.|[^\'\\])\'|//[^\n]*', re.S)
still = set()
for p in files:
    code = lit.sub(" ", p.read_text())
    still |= set(re.findall(r"\b[A-Za-z_]\w*\b", code))
gone = {k: v for k, v in active.items() if k not in still}

IDENT = re.compile(r"\b[A-Za-z_]\w*\b")
n = 0
for p in files:
    lines = p.read_text().split("\n")
    in_fence = False
    changed = False
    for i, l in enumerate(lines):
        m = re.match(r"^(\s*//[/!]\s?)(.*)$", l)
        if not m:
            in_fence = False
            continue
        body = m.group(2)
        if body.lstrip().startswith("```"):
            in_fence = not in_fence
            continue
        if in_fence:
            new = IDENT.sub(lambda im: gone.get(im.group(0), im.group(0)), body)
            if new != body:
                lines[i] = m.group(1) + new
                n += 1
                changed = True
    if changed:
        p.write_text("\n".join(lines))
print("doc-fence lines changed:", n, "(renameable ids:", len(gone), ")")
