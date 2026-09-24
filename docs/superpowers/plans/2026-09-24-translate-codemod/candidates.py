"""Phase 2: list every identifier in the Rust sources that contains a Spanish word.

Spanish lexicon = words of es.ftl + help topics/es + a manual list, accent-stripped,
minus English words (cracklib-small + en.ftl + help topics/en).
Output: candidates.tsv  (identifier, occurrences, files, first location)
"""
import collections
import pathlib
import re
import unicodedata

ROOT = pathlib.Path("/home/oscar/work/wot/projects/high/norte")
OUT = pathlib.Path(__file__).parent / "candidates.tsv"


def strip_accents(s):
    return "".join(c for c in unicodedata.normalize("NFD", s) if unicodedata.category(c) != "Mn")


def words_of(text):
    return {strip_accents(w.lower()) for w in re.findall(r"[A-Za-zÁÉÍÓÚÑáéíóúñü]+", text)}


es = set()
es |= words_of((ROOT / "crates/norte-i18n/i18n/es.ftl").read_text())
for p in (ROOT / "crates/norte-help/topics/es").glob("*.md"):
    es |= words_of(p.read_text())
es |= set(
    """senalar soltar aterrizar encolar cola plegar rodeo trozo trozos hueco huecos pestana
    tamano dueno ninguna ninguno propio propia ajeno ajena lote relevo equipo componer vigencia
    causa firma epoca recorte colocada colocacion miniatura buena inservible hermana quieto midiendo
    falso falsa huella tope cota plazo pedible arbol raiz tras todavia aqui alli asi tecleado
    reintento pendiente gobierno aprobacion desinstalar conceder elegir elegida elegido
    """.split()
)

en = set()
for line in pathlib.Path("/usr/share/cracklib/cracklib-small").read_text(errors="ignore").split():
    en.add(line.strip().lower())
en |= words_of((ROOT / "crates/norte-i18n/i18n/en.ftl").read_text())
for p in (ROOT / "crates/norte-help/topics/en").glob("*.md"):
    en |= words_of(p.read_text())
# Short or ambiguous tokens that are English/tech in identifiers.
en |= set("a an as at be by de do go id in is it me no of on or so to up us we fd io rx tx ui ok x y".split())
spanish = {w for w in es - en if len(w) >= 3}
# Words that are Spanish but also common English/tech identifiers: keep out.
spanish -= set("real total final normal general local global error control color plan rename host panel menu".split())

tok = re.compile(r"\b[A-Za-z_][A-Za-z0-9_]*\b")
# strings (incl. raw/byte), char literals, comments
lit = re.compile(r'r#*"(?:.|\n)*?"#*|b?"(?:\\.|[^"\\])*"|b?\'(?:\\.|[^\'\\])\'|//[^\n]*', re.S)


def parts(ident):
    return re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", ident).lower().split("_")


count = collections.Counter()
files = collections.defaultdict(set)
first = {}
for p in list((ROOT / "crates").rglob("*.rs")) + list((ROOT / "plugins").rglob("*.rs")):
    if "target" in p.parts:
        continue
    src = p.read_text(errors="ignore")
    code = lit.sub(lambda m: " " * len(m.group(0)), src)
    for m in tok.finditer(code):
        t = m.group(0)
        if len(t) < 3 or not any(x in spanish for x in parts(t)):
            continue
        count[t] += 1
        rel = str(p.relative_to(ROOT))
        files[t].add(rel.split("/")[1] if rel.startswith("crates/") else rel)
        if t not in first:
            line = src.count("\n", 0, m.start()) + 1
            first[t] = f"{rel}:{line}"

with OUT.open("w") as f:
    for t, n in sorted(count.items(), key=lambda kv: (-kv[1], kv[0])):
        hits = [x for x in parts(t) if x in spanish]
        f.write(f"{t}\t{n}\t{','.join(sorted(files[t]))}\t{first[t]}\t{'/'.join(hits)}\n")
print(len(count), "identifiers,", sum(count.values()), "occurrences ->", OUT)
