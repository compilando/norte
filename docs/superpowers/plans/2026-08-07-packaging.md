# Packaging Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** turn a tag into artefacts somebody can download, rename the terminal
binary to `ntc`, and publish the protocol schemas alongside it.

**Architecture:** no CI. `dist build` runs on this machine and `gh release
upload` puts the result on the tag, so the release is x86_64 Linux only while
`dist-workspace.toml` keeps describing the five targets a CI release would
produce. `norte-gui` becomes a workspace member kept out of `default-members`,
so dist sees it and daily builds do not.

**Tech stack:** cargo-dist 0.32, `gh`, cargo-semver-checks, just.

**Spec:** `docs/superpowers/specs/2026-08-07-packaging-design.md`

---

### Task 1: `norte tui` launches the binary the manifest builds

The rename's one defect with user impact: the CLI hands the process to a
frontend BY NAME (`exec_frontend("norte-tui", args)`). That name is a string —
it compiles whether or not a binary answers to it, and fails at `exec` time.
The test goes first and it is the only part of this task that is not mechanical.

**Files:**
- Modify: `crates/norte-cli/src/main.rs:493` (the `Cmd::Tui` arm)
- Modify: `crates/norte-cli/src/main.rs:2113-2117` (the existing sibling test)
- Modify: `crates/norte-tui/Cargo.toml:11-13` (`[[bin]] name`)

- [ ] **Step 1: write the failing test** in `crates/norte-cli/src/main.rs`,
      inside the existing `mod tests`:

```rust
/// El CLI lanza el frontend por NOMBRE DE BINARIO, así que el nombre que
/// pasa tiene que ser el que el manifiesto construye. Un string que no
/// corresponde a ningún binario compila igual de bien y falla en el `exec`,
/// con el usuario delante: `norte tui` deja de funcionar y nada lo dice antes.
///
/// El nombre esperado se lee del `Cargo.toml` del crate hermano, no se
/// escribe aquí: una constante en el test se renombra con el mismo
/// buscar-y-reemplazar que rompería el código, y entonces el test acompaña al
/// defecto en vez de cazarlo.
#[test]
fn tui_lanza_el_binario_que_el_manifiesto_construye() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../norte-tui/Cargo.toml");
    let toml = std::fs::read_to_string(&manifest).expect("el manifiesto del TUI");
    let esperado = toml
        .split("[[bin]]")
        .nth(1)
        .and_then(|s| s.lines().find_map(|l| l.trim().strip_prefix("name = ")))
        .map(|n| n.trim_matches('"').to_owned())
        .expect("el manifiesto declara [[bin]] name");
    assert_eq!(
        TUI_BIN, esperado,
        "el CLI lanza `{TUI_BIN}` y el manifiesto construye `{esperado}`"
    );
}
```

- [ ] **Step 2: run it and watch it fail to compile**

Run: `just t norte-cli`
Expected: FAIL, `cannot find value TUI_BIN in this scope`

- [ ] **Step 3: introduce the constant and use it**, in
      `crates/norte-cli/src/main.rs` above `fn exec_frontend`:

```rust
/// Nombre del binario del frontend de terminal.
///
/// Una constante y no un literal en el `match`: es la MISMA cadena que el
/// `[[bin]]` de `crates/norte-tui/Cargo.toml`, y un test las cruza.
const TUI_BIN: &str = "norte-tui";

/// Nombre del binario del frontend gráfico. Mismo criterio que [`TUI_BIN`].
const GUI_BIN: &str = "norte-gui";
```

and change the two arms at `crates/norte-cli/src/main.rs:493-494`:

```rust
        Cmd::Tui { ref args } => return exec_frontend(TUI_BIN, args),
        Cmd::Gui { ref args } => return exec_frontend(GUI_BIN, args),
```

- [ ] **Step 4: run the test — it must PASS before the rename**

Run: `just t norte-cli`
Expected: PASS. The test now guards a name that is still `norte-tui`; the
rename in Step 5 is what it exists for.

- [ ] **Step 5: rename the binary** in `crates/norte-tui/Cargo.toml`:

```toml
[[bin]]
name = "ntc"
path = "src/main.rs"
```

- [ ] **Step 6: run the test and watch it FAIL** — this is the proof it works

Run: `just t norte-cli`
Expected: FAIL, `el CLI lanza `norte-tui` y el manifiesto construye `ntc``

- [ ] **Step 7: make it pass**: `const TUI_BIN: &str = "ntc";`

Run: `just t norte-cli` → PASS

- [ ] **Step 8: sweep every other mention by grep, never from memory**

```sh
grep -rn "norte-tui" --include="*.md" --include="*.toml" --include="justfile" \
  --include="*.ftl" . | grep -v "^./target" | grep -v Cargo.lock
```

Each hit is one of two things and they must not be confused:
- the CRATE `norte-tui` (`-p norte-tui`, `--path crates/norte-tui`,
  `norte-tui = { workspace = true }`) — **leave it**, the crate keeps its name;
- the BINARY (`command -v norte-tui`, `cargo uninstall norte-tui`, the
  installer URLs in `README.md`, prose telling a reader what to type) —
  **rename it**.

Known binary hits:
- `justfile:241`: `@echo "instalados: $(command -v norte-tui) y $(command -v norte)"`
- `justfile:244`: `cargo uninstall norte-tui` — this one takes the CRATE name,
  so it stays as it is. Add a comment saying so, because it now reads as
  inconsistent with the line above it.
- `README.md:22,28`: the installer URLs are derived by dist from the binary
  name and become `ntc-installer.sh` / `ntc-installer.ps1`.
- `README.md:35`: `cargo install --path crates/norte-tui` — crate path, stays.
- `README.md:49`: prose about `norte tui` handing over to `norte-tui` — rename.

- [ ] **Step 9: full gate for the crates that moved**

Run: `just t norte-cli && just t norte-tui && just c norte-cli`
Expected: green.

- [ ] **Step 10: commit**

```bash
git add crates/norte-cli/src/main.rs crates/norte-tui/Cargo.toml justfile README.md
git commit -m "feat(tui)!: the terminal binary is now ntc"
```

---

### Task 2: `norte-gui` becomes a workspace member, outside `default-members`

**Files:**
- Modify: `Cargo.toml:5-27` (`members`), `Cargo.toml:32` (`exclude`)
- Modify: `deny.toml` (only if the audit demands it — see Step 4)
- Delete: `crates/norte-gui/Cargo.lock`

- [ ] **Step 1: move it from `exclude` to `members`** in `Cargo.toml`:

```toml
members = [
    # …the existing entries, unchanged…
    "crates/norte-ai",
    # M5: miembro del workspace para que `dist` lo vea como un binario más y
    # para que haya UN lockfile. FUERA de `default-members` (abajo): las deps
    # de GPU no deben entrar en un `cargo build`/`cargo test` sin `-p`, que es
    # lo que hacía valiosa la exclusión anterior. Su gate propio sigue siendo
    # `just gui-ci`.
    "crates/norte-gui",
]
# Lo que se construye SIN `-p`: todo menos la GUI. `just ci` cuesta lo mismo
# que antes de que la GUI entrase al workspace.
default-members = [
    "crates/norte-proto",
    "crates/norte-vfs",
    "crates/norte-vfs-local",
    "crates/norte-vfs-sftp",
    "crates/norte-vfs-object",
    "crates/norte-vfs-archive",
    "crates/norte-connect",
    "crates/norte-config",
    "crates/norte-testkit",
    "crates/norte-core",
    "crates/norte-index",
    "crates/norte-cli",
    "crates/norte-tui",
    "crates/norte-frontend",
    "crates/norte-help",
    "crates/norte-encoding",
    "crates/norte-theme",
    "crates/norte-i18n",
    "crates/norte-plugin-host",
    "crates/norte-mcp",
    "crates/norte-ai",
]
```

and drop it from `exclude`, keeping the WASM guests:

```toml
exclude = ["crates/norte-plugin-host/examples-wasm"]
```

- [ ] **Step 2: delete the GUI's own lockfile**

```bash
git rm crates/norte-gui/Cargo.lock
```

A member cannot have its own lockfile — the workspace root now owns the
resolution for both.

- [ ] **Step 3: resolve and see what moved**

Run: `cargo metadata --format-version 1 > /dev/null && git diff --stat Cargo.lock`
Expected: the root lockfile grows by the GPUI tree. **If any pre-existing
dependency changes version**, stop and read the diff: unification pulling a
core crate to a different version is a real risk of this task and must be
understood before continuing, not after a test fails.

- [ ] **Step 4: run the licence and advisory audit — the part that can block**

Run: `cargo deny check 2>&1 | tail -30`

Three possible outcomes, and only the first two are acceptable:
- **clean** → nothing to do;
- **findings that are genuinely fine** → add them to `deny.toml` with a comment
  saying WHICH crate, WHY it is acceptable, and a date to revisit. Example
  shape, to be filled with the real crate and reason:

```toml
[[licenses.exceptions]]
# GPUI (M5) arrastra <crate> bajo <licencia>. <Por qué es aceptable aquí.>
# Revisar: 2026-11-07.
name = "<crate>"
allow = ["<SPDX>"]
```

- **findings that are not fine** (an unmaintained crate with a known advisory in
  a path we ship) → STOP and report. The GUI staying excluded is a better
  outcome than a silent `skip`, and this plan does not get to decide that
  alone.

- [ ] **Step 5: prove the default build did not get more expensive**

```sh
cargo build --timings 2>/dev/null >/dev/null; \
  cargo metadata --no-deps --format-version 1 \
  | python3 -c "import json,sys; print([p['name'] for p in json.load(sys.stdin)['packages']])"
```

Run: `cargo tree -p norte-gui --depth 0 && cargo build --dry-run 2>&1 | head -3`
Expected: `cargo build` with no `-p` does NOT mention `gpui`. If it does,
`default-members` is wrong.

- [ ] **Step 6: both gates**

Run: `just ci-fast && just gui-ci`
Expected: green. `just ci-fast` must not have got noticeably slower; if it did,
`default-members` is not doing its job.

- [ ] **Step 7: commit**

```bash
git add Cargo.toml Cargo.lock deny.toml
git rm --cached crates/norte-gui/Cargo.lock 2>/dev/null || true
git commit -m "build: norte-gui joins the workspace, outside default-members"
```

---

### Task 3: `just dist` builds the artefact, `just dist-publish` uploads it

**Files:**
- Modify: `justfile` (new recipes at the end of the installation section,
  after `uninstall` at line 245)
- Create: `scripts/dist-smoke.sh`

- [ ] **Step 1: install the tool**

```sh
cargo install cargo-dist --locked --version 0.32.0
```

Expected: `dist --version` prints `dist 0.32.0`. The version is pinned to the
one `dist-workspace.toml` declares; a mismatch makes dist rewrite the config.

- [ ] **Step 2: write the smoke script** at `scripts/dist-smoke.sh`:

```sh
#!/usr/bin/env bash
# Desempaqueta el artefacto recién construido y arranca lo que lleva dentro.
#
# Una release que no arranca es el único fallo de empaquetado que el usuario
# descubre ANTES que nosotros. Se comprueba sobre el archivo, no sobre
# `target/release/`: lo que se publica es el archivo, y el binario de dentro
# puede llevar otro perfil, otro strip u otro nombre.
set -euo pipefail

archivo=$(find target/distrib -name '*.tar.xz' -print -quit)
[ -n "$archivo" ] || { echo "no hay artefacto: corre \`just dist\` antes"; exit 1; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
tar -xf "$archivo" -C "$tmp"

fallos=0
for bin in ntc norte; do
    ruta=$(find "$tmp" -name "$bin" -type f -print -quit)
    if [ -z "$ruta" ]; then
        echo "FALTA en el artefacto: $bin"; fallos=$((fallos + 1)); continue
    fi
    if ! "$ruta" --version >/dev/null 2>&1; then
        echo "NO ARRANCA desde el artefacto: $bin"; fallos=$((fallos + 1)); continue
    fi
    echo "ok: $bin $("$ruta" --version)"
done
exit "$fallos"
```

- [ ] **Step 3: make it executable and add the recipes** to `justfile`, after
      the `uninstall` recipe:

```make
# ---------- distribución ----------

# Construye los artefactos de release para ESTA máquina (x86_64 Linux).
#
# `dist-workspace.toml` declara CINCO targets: son los que produciría una
# release desde CI. Aquí solo sale el del host — macOS y Windows necesitan esas
# máquinas, y cross-compilar aws-lc-rs es lo que el ADR 0021 dio por frágil.
# Las notas de la release dicen qué plataformas lleva; la config no se recorta
# para disimularlo.
dist:
    dist build --artifacts=local
    @echo "artefactos en target/distrib/:"
    @ls -1 target/distrib/

# Arranca los binarios DESDE el artefacto construido (no desde target/release).
dist-smoke:
    ./scripts/dist-smoke.sh

# Sube a la release del tag lo construido aquí. El tag ya tiene que existir y
# estar empujado: esto publica, no etiqueta.
dist-publish tag:
    ./scripts/dist-smoke.sh
    gh release upload {{tag}} \
        target/distrib/*.tar.xz \
        target/distrib/*.sh \
        target/distrib/*-checksums.txt \
        docs/schema/proto.schema.json \
        docs/schema/norte.schema.json \
        docs/schema/keymap.schema.json \
        --clobber
    @echo "subido a {{tag}}. Comprueba: gh release view {{tag}}"
```

```sh
chmod +x scripts/dist-smoke.sh
```

- [ ] **Step 4: build and smoke it**

Run: `just dist && just dist-smoke`
Expected: the archive lists `ntc` and `norte`, and both print a version. If
`ntc` is missing, Task 1's rename did not reach the manifest dist reads.

- [ ] **Step 5: commit**

```bash
git add justfile scripts/dist-smoke.sh
git commit -m "build(release): just dist builds the artefact and just dist-publish uploads it"
```

---

### Task 4: `release-check` runs semver-checks, and the schemas ship

**Files:**
- Modify: `.claude/commands/release-check.md`
- Modify: `justfile` (a `semver` recipe next to `dist`)

- [ ] **Step 1: install the tool**

```sh
cargo install cargo-semver-checks --locked
```

- [ ] **Step 2: find out what it says today**, before wiring it into anything:

```sh
cargo semver-checks --workspace --baseline-rev v0.3.0-alpha.2 2>&1 | tail -30
```

Expected: findings, or a clean run. Either is information. A crate that cannot
be checked (no baseline on crates.io, git-only deps) reports so — record which,
because that is the honest scope of the check rather than a failure.

- [ ] **Step 3: add the recipe** to `justfile`, under the distribution section:

```make
# Rupturas de API pública contra el último tag (issue #13).
#
# Los frontends (`norte-cli`/`norte-tui`/`norte-gui`) son BINARIOS: no tienen
# API pública que romper y `--workspace` los recorre igual. Lo que importa aquí
# son las librerías publicables — proto, vfs, testkit —, que es lo que un
# tercero consume.
semver baseline="v0.3.0-alpha.2":
    cargo semver-checks --workspace --baseline-rev {{baseline}}
```

- [ ] **Step 4: update the release checklist** at
      `.claude/commands/release-check.md`, replacing step 1:

```markdown
1. Run `just semver` (cargo-semver-checks against the latest tag). It is
   installed; an absent tool is now a BLOCKER, not a note.
```

and adding a step after the schema comparison:

```markdown
7. Confirm the artefacts: `just dist && just dist-smoke`. A release that does
   not start is the one packaging failure the user finds before we do.
```

- [ ] **Step 5: run the whole checklist end to end**

Run: `just semver && just ci-fast && just dist && just dist-smoke`
Expected: green, and `target/distrib/` holds the archive, the installer and the
checksums.

- [ ] **Step 6: commit**

```bash
git add justfile .claude/commands/release-check.md
git commit -m "build(release): semver-checks and artefact smoke join the release checklist"
```

---

### Task 5: close-out

- [ ] **Step 1: CHANGELOG**, under `## [Unreleased]` → `### Changed`, in the
      voice of the existing entries: the terminal binary is `ntc`; `norte tui`
      still launches it and the crate is unchanged; the GUI is a workspace
      member kept out of the default build; a release is now built here and
      uploaded by hand, so the alpha carries x86_64 Linux only and says so.

- [ ] **Step 2: the README's install section** must match what the release
      actually contains: the installer URL is `ntc-installer.sh`, and the
      binary table says `ntc` / `norte` / `norte-gui (experimental)`.

- [ ] **Step 3: full gate**

Run: `just ci` (or its five steps separately if the harness kills it), plus
`just gui-ci`.

- [ ] **Step 4: commit**

```bash
git add CHANGELOG.md README.md
git commit -m "docs(release): the binary is ntc and the alpha ships Linux only"
```

---

## Self-review

**Spec coverage.** §1 artefacts built locally → Task 3. §2 the `ntc` rename →
Task 1, including the CLI-launch defect the spec singles out. §3 GUI into the
workspace outside `default-members`, with the `cargo deny` outcome recorded
either way → Task 2, whose Step 4 refuses to hide a finding. §4 semver-checks
and the schemas → Task 4 (schemas ride in `dist-publish`, Task 3 Step 3).
Out-of-scope items appear nowhere, which is what out of scope means.

**Placeholders.** The one `<crate>`/`<SPDX>` shape in Task 2 Step 4 is a
template for a finding that does not exist yet and cannot be pre-written; the
step says what must fill it and what to do if the finding is not acceptable.
Everything else is literal.

**Type consistency.** `TUI_BIN`/`GUI_BIN` are introduced in Task 1 Step 3 and
used in the same step. `scripts/dist-smoke.sh` is created in Task 3 Step 2 and
called from two recipes and one checklist step under that exact path.
