"""Identifier parts (Rust code, not literals/comments) that are not English words:
candidates for a last Spanish sweep. Output unknown_parts.tsv: part, occurrences, sample ids."""
import collections
import pathlib
import re

ROOT = pathlib.Path("/home/oscar/work/wot/projects/high/norte")
D = pathlib.Path(__file__).parent
en = {w.strip().lower() for w in pathlib.Path("/usr/share/cracklib/cracklib-small").read_text(errors="ignore").split()}
for p in (ROOT / "crates/norte-help/topics/en").glob("*.md"):
    en |= {w.lower() for w in re.findall(r"[A-Za-z]+", p.read_text())}
en |= {w.lower() for w in re.findall(r"[A-Za-z]+", (ROOT / "crates/norte-i18n/i18n/en.ftl").read_text())}
lit = re.compile(r'b?"(?:\\.|[^"\\])*"|b?\'(?:\\.|[^\'\\])\'|//[^\n]*')
parts = collections.Counter()
sample = collections.defaultdict(set)
for p in list((ROOT / "crates").rglob("*.rs")) + list((ROOT / "plugins").rglob("*.rs")):
    if "target" in p.parts:
        continue
    code = lit.sub(" ", p.read_text(errors="ignore"))
    for t in re.findall(r"\b[A-Za-z_][A-Za-z0-9_]*\b", code):
        for x in re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", t).lower().split("_"):
            x = re.sub(r"\d+$", "", x)
            if len(x) >= 4 and x.isalpha() and x not in en and x[:-1] not in en and x[:-2] not in en:
                parts[x] += 1
                if len(sample[x]) < 3:
                    sample[x].add(t)
with (D / "unknown_parts.tsv").open("w") as f:
    for x, n in parts.most_common():
        f.write(f"{x}\t{n}\t{' '.join(sorted(sample[x]))}\n")
print(len(parts), "unknown parts")
