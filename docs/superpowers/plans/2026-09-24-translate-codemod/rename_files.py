"""Rename the Spanish-named test/bench files (git mv) and the `mod` lines,
Cargo [[bench]], justfile and golden-path strings that name them."""
import pathlib
import re
import subprocess

ROOT = pathlib.Path("/home/oscar/work/wot/projects/high/norte")
M = {
    "acciones_del_puente": "bridge_actions",
    "backend_paridad": "backend_parity",
    "catalogo_completo": "full_catalogue",
    "catalogo_del_host": "host_catalogue",
    "catalogo_rpc": "rpc_catalogue",
    "catalogo_wire": "wire_catalogue",
    "celdas_locales": "local_cells",
    "config_cobertura": "config_coverage",
    "copy_remoto": "copy_remote",
    "empaquetado": "packaging",
    "engine_destino_que_desaparece": "engine_vanishing_destination",
    "engine_m2_fase1": "engine_m2_phase1",
    "engine_sync_destino_que_desaparece": "engine_sync_vanishing_destination",
    "engine_undo_de_copia_fallida": "engine_undo_of_failed_copy",
    "gestos": "gestures",
    "ir_a": "go_to",
    "modo_publicado": "published_mode",
    "paridad": "parity_matrix",
    "presupuestos": "budgets",
    "rename_solo_caja": "rename_case_only",
    "revisiones": "reviews",
    "sondas": "probes",
    "spans_en_spawn": "spans_in_spawn",
    "variables_de_tema": "theme_variables",
    "viewer_imagen": "viewer_image",
    "visor": "viewer",
}
files = subprocess.run(["git", "ls-files", "crates", "plugins"], cwd=ROOT, capture_output=True, text=True).stdout.split()
for f in files:
    p = ROOT / f
    if p.suffix != ".rs" or p.stem not in M:
        continue
    dst = p.with_name(M[p.stem] + ".rs")
    if dst.exists():
        print("COLLIDE", f, "->", dst.name)
        continue
    subprocess.run(["git", "mv", str(p), str(dst)], cwd=ROOT, check=True)
    print("MOVE", f, "->", dst.name)
    # a `mod stem;` in the sibling main.rs / mod.rs / lib.rs
    for host in ("main.rs", "mod.rs", "lib.rs"):
        h = p.parent / host
        if h.exists():
            s = h.read_text()
            s2 = re.sub(r"\bmod %s;" % re.escape(p.stem), f"mod {M[p.stem]};", s)
            s2 = re.sub(r"\b%s::" % re.escape(p.stem), f"{M[p.stem]}::", s2)
            if s2 != s:
                h.write_text(s2)
                print("  mod updated in", h.relative_to(ROOT))

# [[bench]] name and the justfile recipe
for f, a, z in [
    ("crates/norte-tui/Cargo.toml", 'name = "presupuestos"', 'name = "budgets"'),
    ("justfile", "--bench presupuestos", "--bench budgets"),
    ("crates/norte-proto/tests/catalog.rs", '"tests/golden/catalogo.tsv"', '"tests/golden/catalogue.tsv"'),
]:
    p = ROOT / f
    s = p.read_text()
    if a in s:
        p.write_text(s.replace(a, z))
        print("edit", f, a, "->", z)
g = ROOT / "crates/norte-proto/tests/golden/catalogo.tsv"
if g.exists():
    subprocess.run(["git", "mv", str(g), str(g.with_name("catalogue.tsv"))], cwd=ROOT, check=True)
    print("MOVE golden catalogo.tsv -> catalogue.tsv")
