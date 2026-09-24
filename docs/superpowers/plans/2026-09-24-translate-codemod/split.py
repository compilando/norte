"""Split candidates into phrase-like identifiers (translate whole) and word-level ones."""
import collections
import pathlib
import re

D = pathlib.Path(__file__).parent
rows = [l.split("\t") for l in (D / "candidates.tsv").read_text().splitlines()]


def nparts(t):
    return len(re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", t).strip("_").split("_"))


FUNC = set("de del la el los las en con sin por al lo se un y a o su sus le les me mi tu".split())
done = set()
for name in ("phrases_A_input.txt", "phrases_B_input.txt"):
    pass
if (D / "phrases.tsv").exists() and "--extra" in __import__("sys").argv:
    done = {l.split("\t")[0] for l in (D / "phrases.tsv").read_text().splitlines()}


def lowparts(t):
    return re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", t).lower().strip("_").split("_")


phrases, words = [], collections.Counter()
for ident, n, crates, loc, hits in rows:
    sp = hits.split("/")
    if (nparts(ident) >= 4 and len(sp) >= 2) or (FUNC & set(lowparts(ident)) and nparts(ident) >= 2):
        phrases.append((ident, n, loc))
    else:
        for w in sp:
            words[w] += int(n)

if done:
    extra = [(i, n, l) for i, n, l in phrases if i not in done]
    with (D / "phrases_extra.tsv").open("w") as f:
        for ident, n, loc in extra:
            f.write(f"{ident}\t{loc}\n")
    print(len(extra), "extra phrase identifiers -> phrases_extra.tsv")
    raise SystemExit
with (D / "phrases.tsv").open("w") as f:
    for ident, n, loc in phrases:
        f.write(f"{ident}\t{loc}\n")
with (D / "words.txt").open("w") as f:
    for w, n in words.most_common():
        f.write(f"{w}\n")
print(len(phrases), "phrase identifiers;", len(words), "words for the word map")
print("phrase occurrences:", sum(int(n) for _, n, _ in phrases))
