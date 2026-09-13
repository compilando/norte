# VSCode theme Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `vscode-dark` and `vscode-light` presets and the chrome
vocabulary that makes the window actually look like VSCode, plus an importer
for any VSCode theme JSON.

**Architecture:** The colour chain is unchanged — a preset or an imported JSON
becomes a `Theme` (`Role`→`Style`), `roles_de_tema()` projects it to CSS
variable names, `applyTheme()` writes them on the root element, and
`style.css` spends them. This plan widens the vocabulary (ten chrome roles),
feeds the three variables nobody feeds today, spends them on the chrome
(scrollbar, hover, elevation), and adds a third resolution branch so an
imported theme is a first-class name.

**Tech Stack:** Rust (`norte-theme`, `norte-frontend`, `norte-ui-host`,
`norte-cli`, `norte-gui-tauri`), TOML themes, TypeScript + CSS webview,
nextest, insta snapshots.

**Spec:** `docs/superpowers/specs/2026-09-11-vscode-theme-design.md`

## Estado: F0–F3 cerrados (2026-09-13), ADR 0108

Tareas 1–8 hechas y fusionadas a `main` en la rama `feat/vscode-theme`. Lo
que la ejecución cambió respecto a lo planeado, para quien retome esto:

- **F2 fue antes que F1**, para transcribir cada preset una vez y no dos.
- **Task 4 partió `roles_de_tema` en dos preguntas** (`nombres_de_tema`, el
  acuerdo con la hoja; `roles_de_tema`, lo que este tema dice). El plan las
  confundía, y el guardián de huérfanas habría leído el silencio de un tema
  como «nadie alimenta esta variable».
- **Dos huecos salieron mirando la ventana, no de la suite**: `[files.kind]`
  y `[files.ext]` nunca se pintaron en la ventana (puente 66), y el esquema
  del escritorio no llegaba al host (puente 67). Los dos están en ADR 0108.
- **Dos BLOCKER de revisión**, ambos causados por el diseño de F2: el cromo
  del tema anterior se quedaba puesto al cambiar de tema, y elegir un preset
  no repintaba las filas.
- **`just gui-test` es nuevo**: `just t norte-gui-tauri` no corría nada.

Quedan las tareas 9–12 (importador) y el plan aparte de F4 (iconos).

## Global Constraints

- **Branch:** `feat/vscode-theme` for Tasks 1–8, `feat/theme-import` for
  Tasks 9–11, `docs/vscode-theme-adr` for Task 12. Never work on `main`.
- **Gate budget:** `just t <crate>` inside the RED→GREEN loop, unlimited.
  **One `just ci-fast` after Task 4 and one after Task 8.** **One `just ci`
  after Task 12**, before the merge. Never re-run the gate to check a fix —
  reproduce the single failure with `just t <crate>` (~78s) and spend the gate
  once, afterwards.
- **Never `cargo` directly** for anything that compiles the workspace; go
  through `just`, which pins one feature set. `cargo nextest run -p X` builds
  a second ~30 GB universe.
- **`just t` does not run doctests and `just c` does not check intra-doc
  links.** Any task that touches a documented public item also runs
  `cargo test -p <crate> --doc`; any task that writes a `[`Type`]` doc link
  also runs `cargo doc -p <crate> --no-deps`. Seconds each.
- **Rule 6 (CLAUDE.md):** no `unwrap()`/`expect()` outside tests unless a
  comment states the invariant. Libraries use `thiserror`, binaries `anyhow`.
- **Rule 1:** filenames are bytes. `files.ext` matching already works on
  `&[u8]`; do not introduce a `to_str().unwrap()` anywhere near it.
- **Public items in `norte-theme` need rustdoc + a doctest** — the crate sets
  `#![warn(missing_docs)]`.
- **Comments and rustdoc in this repo are written in Spanish** in
  `norte-theme`, `norte-ui-host` and `norte-frontend`; `docs/` and ADRs are in
  English. Match the file you are editing.
- **Never write files with Bash** (`echo >`, heredocs). Use Write/Edit.
- **Commit messages:** Conventional Commits, and end with
  `Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>`.
  A `<…>` inside `git commit -m` is read as a shell redirection by this
  repo's hook — write the message to a file and use `git commit -F`.
- **Read `git diff --cached --stat` before every commit.**
- Nothing will notify you. Do not `sleep`, do not `timeout N tail -f
  /dev/null`, do not wait for a monitor. There isn't one.

## Task order note

F2 (the roles) lands **before** F1 (the presets), reversing the spec's
numbering. The spec's F1 would otherwise write the two presets twice: once
with eighteen roles and again with twenty-eight after F2 widened the
vocabulary. Doing the vocabulary first means each preset is transcribed once.

## File structure

| file | responsibility | tasks |
| --- | --- | --- |
| `crates/norte-theme/src/role.rs` | the `Role` enum, `CORE`/`REQUESTABLE`/`ALL`, `fallback`, kebab names | 2, 3 |
| `crates/norte-theme/src/vscode.rs` *(new)* | pure parser for VSCode theme JSON → colour map; no I/O | 9 |
| `crates/norte-theme/presets/vscode-{dark,light}.toml` *(new)* | the two transcribed presets | 5 |
| `crates/norte-theme/src/presets.rs` | registers them | 5 |
| `crates/norte-frontend/src/decoration.rs`, `src/viewer.rs` | the two ADR 0037 validation points | 3 |
| `crates/norte-frontend/src/theme.rs` | resolver third branch, `available_themes()` | 10 |
| `crates/norte-ui-host/src/pickers.rs` | `roles_de_tema()` — the CSS key set | 4 |
| `crates/norte-gui-tauri/ui/src/style.css` | geometry + the derivation table + the chrome | 1, 4, 6, 7 |
| `crates/norte-gui-tauri/tests/variables_de_tema.rs` *(new)* | the orphan-variable guard | 1, 4 |
| `crates/norte-cli/src/main.rs`, `src/theme.rs` *(new)* | `norte theme import` | 11 |

---

### Task 1: The orphan-variable guard, and the one orphan it catches today

**Files:**
- Create: `crates/norte-gui-tauri/tests/variables_de_tema.rs`
- Modify: `crates/norte-gui-tauri/ui/src/style.css:2610`

**Interfaces:**
- Consumes: `norte_ui_host::pickers::roles_de_tema(&norte_theme::Theme) -> Vec<(String, String)>` (already public).
- Produces: nothing other tasks call; Tasks 4 and 6 extend the two allow-lists inside this test.

**Why this test lives in `norte-gui-tauri` and not `norte-ui-host`:** ADR 0066
forbids `norte-ui-host` from knowing a painting toolkit, and `style.css` is the
Tauri renderer's. The renderer checking its own stylesheet against what the
host projects is the right direction of knowledge, and `norte-gui-tauri`
already depends on `norte-ui-host`.

- [ ] **Step 1: Write the failing test**

Create `crates/norte-gui-tauri/tests/variables_de_tema.rs`:

```rust
//! Cada `var(--x)` de la hoja de estilos la alimenta alguien.
//!
//! Una variable que nadie escribe no se ve rota: cae a su valor de respaldo y
//! se queda ahí para siempre. `--warn-fg` era eso — una errata de
//! `warning-fg` — y los avisos del panel de registro llevaban meses
//! ignorando el tema.

use std::collections::BTreeSet;

/// Variables de GEOMETRÍA y de fuente: nunca salen del tema.
const NO_SON_COLOR: &[&str] = &[
    "cell-w",
    "cell-h",
    "menubar-h",
    "panelbar-h",
    "keybar-h",
    "depth",
    "busy-delay",
    "menu-left",
    "menu-open",
    "mono",
    "ui-font",
    "ui-font-size",
    "font-mono",
    "font-ui",
    "dialog-backdrop",
];

/// Huérfanas CONOCIDAS, con fecha de caducidad: las alimenta la tarea 4
/// (roles `muted` y `badge`). Esta lista se VACÍA allí, y vaciarla es lo que
/// impide que se olviden.
const HUERFANAS_CONOCIDAS: &[&str] = &["dim-fg", "chip-bg"];

fn variables_de_la_hoja() -> BTreeSet<String> {
    let css = include_str!("../ui/src/style.css");
    let mut out = BTreeSet::new();
    // `var(` y el nombre pueden ir en LÍNEAS distintas (la hoja parte las
    // pilas de fuentes), así que se busca sobre el texto entero, no por línea.
    let mut resto = css;
    while let Some(i) = resto.find("var(") {
        resto = &resto[i + 4..];
        let t = resto.trim_start();
        if let Some(nombre) = t.strip_prefix("--") {
            let fin = nombre
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '-')
                .unwrap_or(nombre.len());
            if fin > 0 {
                out.insert(nombre[..fin].to_owned());
            }
        }
    }
    out
}

#[test]
fn cada_variable_de_color_la_alimenta_el_tema() {
    // Un tema que define TODOS los roles: es el conjunto de claves POSIBLE.
    let tema = norte_theme::Theme::preset_default();
    let proyectadas: BTreeSet<String> = norte_ui_host::pickers::roles_de_tema(&tema)
        .into_iter()
        .map(|(k, _)| k)
        .collect();

    let usadas = variables_de_la_hoja();
    let huerfanas: Vec<&String> = usadas
        .iter()
        .filter(|v| !proyectadas.contains(*v))
        .filter(|v| !NO_SON_COLOR.contains(&v.as_str()))
        .filter(|v| !HUERFANAS_CONOCIDAS.contains(&v.as_str()))
        .collect();
    assert!(
        huerfanas.is_empty(),
        "variables de color que nadie alimenta: {huerfanas:?}"
    );
}

#[test]
fn cada_color_proyectado_lo_gasta_la_hoja() {
    let tema = norte_theme::Theme::preset_default();
    let usadas = variables_de_la_hoja();
    let sin_gastar: Vec<String> = norte_ui_host::pickers::roles_de_tema(&tema)
        .into_iter()
        .map(|(k, _)| k)
        .filter(|k| !usadas.contains(k))
        .collect();
    assert!(
        sin_gastar.is_empty(),
        "colores que el host proyecta y la hoja no pinta: {sin_gastar:?}"
    );
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `just t norte-gui-tauri`
Expected: `cada_variable_de_color_la_alimenta_el_tema` FAILS with
`variables de color que nadie alimenta: ["warn-fg"]`.

If `cada_color_proyectado_lo_gasta_la_hoja` also fails, that is a second real
finding — a role Rust projects that the stylesheet never paints. Record which
names, fix them in this task if the fix is a one-line CSS spend, and if it is
not, add them to a third allow-list with a comment naming the reason. Do not
delete the assertion.

- [ ] **Step 3: Fix the orphan**

In `crates/norte-gui-tauri/ui/src/style.css:2610`, change
`color: var(--warn-fg, #fc6);` to `color: var(--warning-fg, #fc6);`.

- [ ] **Step 4: Run the tests**

Run: `just t norte-gui-tauri`
Expected: both tests PASS.

- [ ] **Step 5: Commit**

Write the message to a file and commit with `-F` (the hook rejects `<>` in
`-m`):

```
fix(gui): a log-panel warning obeys the theme

`--warn-fg` was a misspelling of the `warning-fg` the host projects, so
every warning in the log panel fell back to a hard-coded #fc6 and ignored
the theme. The test is the fix: it collects every var(--x) in the sheet
and asserts the set matches what roles_de_tema() produces, in both
directions, so the next orphan cannot land quietly.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
```

```bash
git add crates/norte-gui-tauri/tests/variables_de_tema.rs crates/norte-gui-tauri/ui/src/style.css
git diff --cached --stat
git commit -F <message-file>
```

---

### Task 2: Ten chrome roles, and splitting `ALL` into `CORE`

**Files:**
- Modify: `crates/norte-theme/src/role.rs`
- Modify: `crates/norte-theme/tests/presets.rs:18-56`

**Interfaces:**
- Produces:
  - `Role::Hover`, `Role::InputBackground`, `Role::InputBorder`,
    `Role::WidgetBackground`, `Role::WidgetShadow`, `Role::Badge`,
    `Role::ScrollbarSlider`, `Role::Separator`, `Role::FocusBorder`,
    `Role::Muted`
  - `Role::CORE: &'static [Role]` — the eighteen that exist today
  - `Role::ALL: &'static [Role]` — all twenty-eight (unchanged meaning)
  - kebab names: `hover`, `input-background`, `input-border`,
    `widget-background`, `widget-shadow`, `badge`, `scrollbar-slider`,
    `separator`, `focus-border`, `muted`
- Task 3 adds `Role::REQUESTABLE`. Task 4 projects these to CSS names. Task 5
  gives the two new presets values for all twenty-eight.

- [ ] **Step 1: Write the failing tests**

Append to the `mod tests` block in `crates/norte-theme/src/role.rs`:

```rust
/// `CORE` es un SUBCONJUNTO de `ALL`, y `ALL` no pierde a nadie.
///
/// Los dos conjuntos existen porque miden cosas distintas: `ALL` es el
/// vocabulario, `CORE` es lo que un preset está OBLIGADO a colorear. Sin
/// esta comprobación, un rol nuevo puede caer fuera de los dos y no
/// existir para nadie.
#[test]
fn core_es_subconjunto_de_all_y_all_los_tiene_a_todos() {
    for &r in Role::CORE {
        assert!(Role::ALL.contains(&r), "{r:?} está en CORE y no en ALL");
    }
    assert_eq!(Role::CORE.len(), 18, "CORE son los dieciocho de siempre");
    assert_eq!(Role::ALL.len(), 28, "ALL son esos más los diez de cromo");
}

/// Los diez roles de cromo NO están en CORE: se DERIVAN en la hoja de
/// estilos de colores que el tema ya tiene (spec 2026-09-11, F2), y por eso
/// un preset no tiene que definirlos.
#[test]
fn los_roles_de_cromo_quedan_fuera_de_core() {
    for r in [
        Role::Hover,
        Role::InputBackground,
        Role::InputBorder,
        Role::WidgetBackground,
        Role::WidgetShadow,
        Role::Badge,
        Role::ScrollbarSlider,
        Role::Separator,
        Role::FocusBorder,
        Role::Muted,
    ] {
        assert!(!Role::CORE.contains(&r), "{r:?} no debería exigirse a cada preset");
    }
}
```

The two tests that already exist (`as_kebab_es_el_nombre_de_serde` and
`from_kebab_todos_los_roles_hacen_roundtrip`) iterate `Role::ALL` and so cover
the ten new names for free. Do not duplicate them.

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-theme`
Expected: compile error — `Role::Hover` and `Role::CORE` do not exist.

- [ ] **Step 3: Add the variants**

In `crates/norte-theme/src/role.rs`, add the ten variants to the enum with
Spanish rustdoc on each. Three of them need their doc to say something
specific, because the name alone misleads:

- `Badge`: serves TWO consumers — VSCode's counter and the log panel's chip
  (`--chip-bg`). Same shape as `Mark`, whose rustdoc already explains a
  two-reading role. Style it with `bg` and `fg`.
- `FocusBorder`: the ring of a CONTROL. Name `BorderFocus` in the doc and say
  it is the border of the focused PANE, a different thing.
- `BorderFocus` (existing): add the reciprocal sentence naming `FocusBorder`.

Each of the ten also gets a `fallback()` arm. They all return `Style::new()` —
no colour — because the derivation that gives them a sensible default lives in
the stylesheet (Task 4), not here: a literal in `fallback()` cannot follow
eight different palettes. Put that reason in the comment on the match arm,
beside the existing `PaneBackground` one that says the same thing.

Add each variant to `as_kebab` (the match is exhaustive and will not compile
until you do) and to `ALL`. Then add `CORE` with the eighteen existing
variants.

- [ ] **Step 4: Split the preset completeness test**

In `crates/norte-theme/tests/presets.rs`, change the loop at line ~32 from
`for &role in Role::ALL` to `for &role in Role::CORE`, and put a comment above
it:

```rust
// CORE, no ALL: los diez roles de CROMO se derivan en la hoja de estilos
// de la ventana de colores que el tema ya tiene (spec 2026-09-11, F2), así
// que exigírselos a cada preset serían ochenta valores inventados. Los dos
// presets `vscode-*` sí los definen, porque para ellos son el asunto.
```

- [ ] **Step 5: Run the tests**

Run: `just t norte-theme` — expected PASS.
Run: `cargo test -p norte-theme --doc` — expected PASS (the rustdoc you wrote
on `Role::from_kebab` and `as_kebab` has doctests that still name real roles).
Run: `cargo doc -p norte-theme --no-deps` — expected no warnings (you wrote
`[`FocusBorder`]`-style links).

- [ ] **Step 6: Commit**

```
feat(theme): ten chrome roles, and a preset only owes the core eighteen

VSCode's look is surfaces at different elevations rather than borders, and
Role had no name for any of them: list hover, inputs, widget backgrounds,
badges, the overlay scrollbar, focus rings.

Preset completeness now iterates Role::CORE. Asserting it over all
twenty-eight would demand eighty invented values across the eight existing
presets, and a monochrome fallback is the wrong default for a chrome role:
an invisible hover is not a conservative hover. They derive in the window's
stylesheet from colours the theme already has instead.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
```

---

### Task 3: A plugin may not ask for the scrollbar's colour

**Files:**
- Modify: `crates/norte-theme/src/role.rs`
- Modify: `crates/norte-frontend/src/decoration.rs:78`
- Modify: `crates/norte-frontend/src/viewer.rs:367`

**Interfaces:**
- Consumes: `Role::CORE`, the ten variants from Task 2.
- Produces: `Role::REQUESTABLE: &'static [Role]` and
  `Role::from_kebab_requestable(s: &str) -> Option<Role>`.

**Background:** ADR 0037 makes `role` a string on the WIT boundary, validated
host-side by `Role::from_kebab`. Adding a variant therefore made it
plugin-requestable with no WIT bump — and a plugin painting a badge with the
scrollbar slider's colour is nonsense. Our twelve bundled plugins request only
`info` and `title`, so narrowing breaks nothing of ours; a third-party plugin
naming a chrome role loses a colour and keeps working, which is the
degradation ADR 0037 already specifies for an unknown name.

- [ ] **Step 1: Write the failing tests**

In `crates/norte-theme/src/role.rs`'s `mod tests`:

```rust
/// Lo PEDIBLE por un plugin es un subconjunto de lo que existe, y deja
/// fuera el cromo (ADR 0037 + spec 2026-09-11, F2): una insignia pintada
/// con el color del deslizador de la barra de desplazamiento no significa
/// nada.
#[test]
fn lo_pedible_deja_fuera_el_cromo() {
    for &r in Role::REQUESTABLE {
        assert!(Role::ALL.contains(&r), "{r:?} pedible y no existe");
    }
    for r in [Role::ScrollbarSlider, Role::WidgetShadow, Role::InputBorder] {
        assert!(!Role::REQUESTABLE.contains(&r), "{r:?} no debería ser pedible");
    }
    // Las señales que un plugin SÍ necesita.
    for r in [Role::Error, Role::Warning, Role::Info, Role::Title, Role::Match] {
        assert!(Role::REQUESTABLE.contains(&r), "{r:?} tiene que ser pedible");
    }
}

/// El punto de entrada de un nombre que viene de un plugin: un rol de
/// cromo degrada a `None`, igual que un nombre desconocido. No es un
/// error — un guest más nuevo no puede romper el render de uno más viejo.
#[test]
fn from_kebab_requestable_degrada_el_cromo_a_none() {
    assert_eq!(Role::from_kebab_requestable("warning"), Some(Role::Warning));
    assert_eq!(Role::from_kebab_requestable("scrollbar-slider"), None);
    assert_eq!(Role::from_kebab_requestable("no-existe"), None);
    // Y sigue siendo un rol de verdad para quien pregunte sin filtro.
    assert_eq!(Role::from_kebab("scrollbar-slider"), Some(Role::ScrollbarSlider));
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-theme`
Expected: compile error — `REQUESTABLE` and `from_kebab_requestable` do not
exist.

- [ ] **Step 3: Implement**

Add to `impl Role`:

```rust
/// Los roles que un PLUGIN puede nombrar en un span o una decoración
/// (ADR 0037). Es `CORE` menos los que no significan nada en una insignia
/// y más nada: el cromo (`hover`, `scrollbar-slider`, `widget-*`,
/// `input-*`, `focus-border`, `separator`) lo pinta la ventana, no el
/// contenido.
pub const REQUESTABLE: &'static [Role] = &[ /* … */ ];
```

Choose its members as: every `CORE` role except `Background`,
`PaneBackground`, `PaneFocusBackground`, `BorderFocus`, `BorderUnfocused`,
`ModalBorder` and `Button` — those are the window's own surfaces — plus
`Badge` and `Muted` from the new ten, which are exactly the two a decoration
has a legitimate use for. Write that reasoning into the rustdoc.

Then:

```rust
/// [`Self::from_kebab`] acotado a [`Self::REQUESTABLE`]: el punto de
/// entrada de un nombre que viene de un plugin.
#[must_use]
pub fn from_kebab_requestable(s: &str) -> Option<Role> {
    Self::from_kebab(s).filter(|r| Self::REQUESTABLE.contains(r))
}
```

Give it a doctest (the crate warns on missing docs and this is public).

- [ ] **Step 4: Switch the two validation points**

`crates/norte-frontend/src/decoration.rs:78` and
`crates/norte-frontend/src/viewer.rs:367`: replace
`norte_theme::Role::from_kebab` with
`norte_theme::Role::from_kebab_requestable`. Update the rustdoc above each
(both already describe the closed-vocabulary rule) to say the vocabulary is
now the requestable subset, and name the spec.

- [ ] **Step 5: Run the tests**

Run: `just t norte-theme` then `just t norte-frontend` — expected PASS.
Run: `cargo test -p norte-theme --doc` — expected PASS.

If a `norte-frontend` test fails because it asserted a now-unrequestable role,
read it before changing it: if it was asserting the general vocabulary it
should call `from_kebab`; if it was asserting plugin behaviour its expectation
is now `None` and that is the point of this task.

- [ ] **Step 6: Commit**

```
feat(theme,frontend): a plugin names a meaning, not a piece of chrome

Role is the closed vocabulary a plugin may name (ADR 0037), so the ten
chrome roles had just become requestable with no WIT bump. A badge painted
with the scrollbar slider's colour is nonsense; Role::REQUESTABLE names the
subset a decoration can mean, and the two validation points consult it. A
chrome name degrades to None, the same degradation an unknown name already
had. Our twelve bundled plugins request only info and title.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
```

---

### Task 4: Project the ten, derive them in the stylesheet, empty the orphan list

**Files:**
- Modify: `crates/norte-ui-host/src/pickers.rs:44-84` (`roles_de_tema`)
- Modify: `crates/norte-ui-host/src/pickers.rs:560-575` (the test that lists expected keys)
- Modify: `crates/norte-gui-tauri/ui/src/style.css` (`:root` block, lines 31-49)
- Modify: `crates/norte-gui-tauri/tests/variables_de_tema.rs` (empty `HUERFANAS_CONOCIDAS`)

**Interfaces:**
- Consumes: the ten variants (Task 2).
- Produces: the CSS names `hover`, `input-bg`, `input-border`,
  `widget-bg`, `widget-shadow`, `badge-bg`, `badge-fg`, `scrollbar-slider`,
  `separator`, `focus-border`, `muted`. Task 6 and Task 7 spend them.

**Note on two existing names:** `--dim-fg` and `--chip-bg` are already read by
the stylesheet (`:1128` and `:2621,2629,2641`). Do **not** invent new names
for those two uses — point them at `muted` and `badge-bg` respectively by
editing those four CSS sites, and delete `HUERFANAS_CONOCIDAS`. That is what
retires the allow-list rather than growing it.

- [ ] **Step 1: Write the failing test**

Extend the existing expected-keys test in `crates/norte-ui-host/src/pickers.rs`
(around line 560) with the eleven new names. Read the test first — it asserts
a specific ordered or contained set; follow its existing shape rather than
replacing it.

Also, in `crates/norte-gui-tauri/tests/variables_de_tema.rs`, change:

```rust
const HUERFANAS_CONOCIDAS: &[&str] = &[];
```

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-ui-host` — expected FAIL, the new keys are not projected.
Run: `just t norte-gui-tauri` — expected FAIL with
`variables de color que nadie alimenta: ["chip-bg", "dim-fg"]`.

- [ ] **Step 3: Project the ten**

In `roles_de_tema`, after the existing `poner` calls, add:

```rust
// El cromo de la ventana (spec 2026-09-11, F2). Los diez se DERIVAN en la
// hoja de estilos cuando el tema calla —`var(--hover, var(--panel-focus-bg))`—
// así que aquí solo se proyecta lo que el tema dice de verdad, y un tema
// viejo no cambia de aspecto.
poner("hover", Role::Hover, true);
poner("input-bg", Role::InputBackground, true);
poner("input-border", Role::InputBorder, false);
poner("widget-bg", Role::WidgetBackground, true);
poner("widget-shadow", Role::WidgetShadow, false);
poner("badge-bg", Role::Badge, true);
poner("badge-fg", Role::Badge, false);
poner("scrollbar-slider", Role::ScrollbarSlider, true);
poner("separator", Role::Separator, false);
poner("focus-border", Role::FocusBorder, false);
poner("muted", Role::Muted, false);
```

- [ ] **Step 4: Write the derivation table into the stylesheet**

The ten are spent as `var(--name, var(--fallback))`, one level. The table,
verbatim from the spec:

| var | derives from |
| --- | --- |
| `--separator` | `--border` |
| `--hover` | `--panel-focus-bg` |
| `--scrollbar-slider` | `--border` |
| `--input-bg`, `--widget-bg` | `--panel-bg` |
| `--input-border` | `--border` |
| `--focus-border` | `--border-focus` |
| `--muted` | `--title-fg` |
| `--badge-bg` | `--selection-bg` |
| `--widget-shadow` | `rgb(0 0 0 / 35%)` |

In this task, apply it only to the two sites that already exist: change
`var(--dim-fg, var(--title-fg))` at `:1128` to
`var(--muted, var(--title-fg))`, and the three `var(--chip-bg, …)` at
`:2621,2629,2641` to `var(--badge-bg, var(--selection-bg))`. Keep the
derivation table itself as a comment in the `:root` block so the next reader
finds it in one place. Tasks 6 and 7 add the remaining sites as they spend
them.

- [ ] **Step 5: Run the tests**

Run: `just t norte-ui-host` then `just t norte-gui-tauri` — expected PASS.

Note: `cada_color_proyectado_lo_gasta_la_hoja` will now fail for the names
nothing spends yet (`hover`, `scrollbar-slider`, `separator`, `input-*`,
`widget-*`, `focus-border`, `badge-fg`). Add them to a THIRD allow-list in
that test named `PENDIENTES_DE_GASTAR`, with a comment naming Tasks 6 and 7,
and empty it in Task 7. A list with an owner and a deadline is a plan; a list
without one is a leak.

- [ ] **Step 6: Commit**

```
feat(ui-host,gui): project the chrome roles and derive them when a theme is silent

The ten new roles reach the window as CSS variables, and the stylesheet
spends them as var(--hover, var(--panel-focus-bg)) so a theme that never
heard of them keeps painting exactly what it paints today. --dim-fg and
--chip-bg, orphaned since they were written, become --muted and --badge-bg:
the known-orphan allow-list is now empty.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
```

- [ ] **Step 7: GATE — run `just ci-fast` ONCE**

Run: `just ci-fast` (~4 min, foreground, never piped through `tail`).
This is one of the plan's two budgeted `ci-fast` runs. If it is red, read
which test failed, reproduce it with `just t <that crate>`, fix it there, and
do **not** re-run `ci-fast` to check — the next budgeted run is after Task 8.

---

### Task 5: Transcribe `vscode-dark` and `vscode-light`

**Files:**
- Create: `crates/norte-theme/presets/vscode-dark.toml`
- Create: `crates/norte-theme/presets/vscode-light.toml`
- Modify: `crates/norte-theme/src/presets.rs:12-33`
- Modify: `crates/norte-theme/tests/presets.rs:18-24`
- Modify: `crates/norte-config/src/schema.rs:180-183`
- Modify: `docs/schema/norte.schema.json`
- Modify: `docs/theming.md:25-28`
- Modify: `CHANGELOG.md`
- Modify: `crates/norte-tui/tests/snapshots/snapshots_ui__snapshot_theme_picker_80x24.snap`

**Interfaces:**
- Consumes: all twenty-eight roles (Tasks 2–4).
- Produces: preset names `vscode-dark`, `vscode-light`. Task 10 uses them as
  the importer's base layer.

**These are TRANSCRIPTIONS.** Fetch the sources; do not invent a hex. The
repo's rule for imported keymap presets applies here verbatim: where a source
does not attest a value, say so in the file's header rather than guessing
quietly.

- [ ] **Step 1: Fetch the sources**

Four files, via WebFetch on `raw.githubusercontent.com/microsoft/vscode/main`:

- `extensions/theme-defaults/themes/dark_modern.json`
- `extensions/theme-defaults/themes/dark_plus.json`
- `extensions/theme-defaults/themes/dark_vs.json`

and the same three with `light_`. Resolution order for a colour id: the
theme's own `colors`, then each `include` in turn.

A verified fact you can rely on: `dark_modern.json` carries
`include: ./dark_plus.json` and defines `focusBorder: #0078D4`,
`badge.background: #616161`, `badge.foreground: #F8F8F8`,
`editor.background: #1F1F1F`, `foreground: #CCCCCC`,
`input.background: #313131`, `input.border: #3C3C3C`,
`sideBar.background: #181818`, `panel.border: #2B2B2B`,
`statusBar.background: #181818`, `statusBar.foreground: #CCCCCC`,
`descriptionForeground: #9D9D9D`, `errorForeground: #F85149`,
`editorWidget.background: #202020`, `quickInput.background: #222222`,
`widget.border: #313131`, `editor.findMatchBackground: #9E6A03`,
`menu.background: #1F1F1F`, `menu.selectionBackground: #0078d4`.

- [ ] **Step 2: Fetch the registry defaults for what the chain does not define**

Verified: none of the three dark files defines
`list.activeSelectionBackground`, `list.inactiveSelectionBackground`,
`list.hoverBackground`, `scrollbarSlider.background` or `widget.shadow`. Those
are VSCode's built-in defaults, registered in
`src/vs/platform/theme/common/colors/` — a directory whose contents are
confirmed: `baseColors.ts`, `chartsColors.ts`, `editorColors.ts`,
`inputColors.ts`, `listColors.ts`, `menuColors.ts`, `minimapColors.ts`,
`miscColors.ts`, `quickpickColors.ts`, `searchColors.ts`.

Fetch `listColors.ts` (for the `list.*` ids) and `miscColors.ts` (for
`scrollbarSlider.*` and `widget.shadow`), and read the `dark:` / `light:` arm
of each `registerColor` call. Record in each preset's header which ids came
from the registry rather than from the theme JSON — a reader diffing our TOML
against `dark_modern.json` will otherwise find colours that are not there.

If a registry default is given as a reference to another colour id rather than
a literal, resolve it one more hop and note the hop in the header.

- [ ] **Step 3: Write the two preset files**

Map by the table in the spec's F2 section. The eighteen core roles first, then
the ten chrome ones — these two presets define all twenty-eight, because for
them the chrome is the whole point.

`background` ← `editor.background`; `regular` ← `foreground`;
`pane-background` ← `sideBar.background`; `pane-focus-background` ←
`editor.background`; `selection` ← `list.activeSelectionBackground` /
`...Foreground`; `selection-unfocused` ← `list.inactiveSelectionBackground`;
`hover` ← `list.hoverBackground`; `status-bar` ← `statusBar.background` /
`.foreground`; `border-focus` and `focus-border` ← `focusBorder`;
`border-unfocused` and `separator` ← `panel.border`; `modal-border` ←
`widget.border`; `match` ← `editor.findMatchBackground`; `error` ←
`errorForeground`; `muted` ← `descriptionForeground`; `badge` ←
`badge.background` / `.foreground`; `input-background` ← `input.background`;
`input-border` ← `input.border`; `widget-background` ←
`editorWidget.background`; `widget-shadow` ← `widget.shadow`;
`scrollbar-slider` ← `scrollbarSlider.background`; `button` ←
`button.background` / `.foreground`; `title` ← `sideBarTitle.foreground`.

For `warning`, `info`, `hostile-badge`, `mark`, and the `[files.kind]` /
`[files.ext]` tables, VSCode has no equivalent id — it does not colour a file
list by node type. Derive them from the theme's own palette (VSCode's editor
token colours and `editorGutter.*` are the honest source: `#2EA043` added,
`#F85149` deleted, `#0078D4` modified) and **write in the header that they are
derived, not attested**. That header block is not optional; it is the same
divergences/omissions block the imported keymap presets carry.

The header also carries the typography pointer (spec F3, item 4): a `Theme`
holds no fonts and will not start to, so the VSCode type setup is reached
through `[ui] font_size = 13` and `[ui] font` / `mono_font`. Say it in both
preset headers, because the reader who picks `vscode-dark` and finds the text
a size too large will look in the theme file first.

- [ ] **Step 4: Register them**

In `crates/norte-theme/src/presets.rs`, add both to the `PRESETS` array with
`include_str!`, in a new commented group (`// VSCode (spec 2026-09-11).`)
beside the existing `// Claros.` and `// Retro (G1)` groups.

In `crates/norte-theme/tests/presets.rs`, add
`assert!(names.contains(&"vscode-dark"));` and the light one beside the four
existing assertions.

- [ ] **Step 5: Run the tests and expect WCAG trouble**

Run: `just t norte-theme`

`las_senales_semanticas_llegan_a_wcag_aa_en_todo_preset` requires 4.5:1
against the preset's own background for `error`, `warning` and
`hostile-badge`. VSCode's warning yellow is weak and may fail, and
`vscode-light`'s `errorForeground` may too.

**If it fails: raise the colour until it passes and record the divergence in
the preset header, naming the original value.** Do not touch the test, do not
add an exemption. These are the colours a reader must be able to read when
something has gone wrong.

- [ ] **Step 6: Update the eight naming sites**

- `crates/norte-config/src/schema.rs:180-183`: the doc comment listing preset
  names. It lists six today and there are eight; make it ten and correct.
- `docs/schema/norte.schema.json`: regenerate rather than hand-edit if a
  recipe produces it; otherwise mirror the schema.rs change exactly.
- `docs/theming.md:25-28`: same list, same omission (the two `retro-crt` are
  missing today). Fix both.
- `CHANGELOG.md`: an entry under the unreleased heading.
- The TUI theme-picker snapshot: run the TUI test suite and accept the
  snapshot change with `cargo insta accept` (or the repo's usual mechanism —
  check how `crates/norte-tui/tests/theme_picker.rs` is driven before
  assuming).

- [ ] **Step 7: Run the tests**

Run: `just t norte-theme`, `just t norte-config`, `just t norte-tui` —
expected PASS.
Run: `cargo test -p norte-theme --doc` — the `effect_names` doctest names
`nord`; confirm it still holds.

- [ ] **Step 8: Commit**

```
feat(theme): bundle vscode-dark and vscode-light

Transcribed from dark_modern.json / light_modern.json and the include chain
behind each, plus VSCode's built-in colour registry for the ids no theme
file in the chain defines -- list.*, scrollbarSlider.*, widget.shadow. Each
preset's header records which colours came from the registry and which are
derived, because VSCode has no id for a file list coloured by node type and
guessing quietly is how a preset stops being a transcription.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
```

---

### Task 6: The overlay scrollbar

**Files:**
- Modify: `crates/norte-gui-tauri/ui/src/style.css` (`.scroller` at ~:477, plus a new block)
- Modify: `crates/norte-gui-tauri/tests/variables_de_tema.rs` (shrink `PENDIENTES_DE_GASTAR`)

**Interfaces:**
- Consumes: `--scrollbar-slider` (Task 4).

The window shows WebKitGTK's scrollbar today — nothing styles it — and it is
the single loudest tell that this is not VSCode.

- [ ] **Step 1: Style it**

WebKitGTK honours `::-webkit-scrollbar`. Add one block that covers every
scrolling surface — grep `overflow-y: auto` to confirm the list (`.scroller`,
the viewer, the log panel, the settings pane) before settling on the selector:

```css
/* La barra de desplazamiento (spec 2026-09-11, F3). Superpuesta y sin
   flechas, como la del editor que este tema imita: el deslizador se pinta
   con `scrollbar-slider`, que cae a `--border` en cualquier tema que no lo
   nombre. El recuadro es de 14 px y el deslizador de 8: los 3 px de cada
   lado son un borde TRANSPARENTE, no un `width`, porque un `width` menor
   dejaría el resto del canal sin zona de arrastre. */
.scroller::-webkit-scrollbar,
.viewer-body::-webkit-scrollbar,
.log-body::-webkit-scrollbar,
.settings-body::-webkit-scrollbar {
  width: 14px;
  height: 14px;
  background: transparent;
}

.scroller::-webkit-scrollbar-thumb,
.viewer-body::-webkit-scrollbar-thumb,
.log-body::-webkit-scrollbar-thumb,
.settings-body::-webkit-scrollbar-thumb {
  background: var(--scrollbar-slider, var(--border));
  /* Cuadrado a propósito: el del editor no tiene radio. */
  border-radius: 0;
  border: 3px solid transparent;
  background-clip: padding-box;
  opacity: 0.6;
  transition: opacity 80ms ease-out;
}

.scroller::-webkit-scrollbar-thumb:hover,
.viewer-body::-webkit-scrollbar-thumb:hover,
.log-body::-webkit-scrollbar-thumb:hover,
.settings-body::-webkit-scrollbar-thumb:hover {
  opacity: 1;
}

/* Sin flechas: ocupan sitio y no existen en lo que esto imita. */
.scroller::-webkit-scrollbar-button {
  display: none;
}
```

Verify the four class names against the stylesheet before writing them — they
are this plan's guess at the scrolling surfaces, not a read fact, and a
selector that matches nothing fails silently.

Then extend the existing `[data-reduce-motion="true"]` block at the end of the
file with the thumb's `transition: none`. Extend it; do not start a second
one.

- [ ] **Step 2: Shrink the pending list**

Remove `scrollbar-slider` from `PENDIENTES_DE_GASTAR`.

- [ ] **Step 3: Run the tests**

Run: `just t norte-gui-tauri` — expected PASS.

- [ ] **Step 4: See it**

The suite cannot tell you whether a scrollbar looks right. Build and look:
`just link-gui`, then run `ntc-gui`. This repo's own history is explicit that
four visual bugs in the window were found only by painting real files, never
by a green suite.

- [ ] **Step 5: Commit**

```
feat(gui): an overlay scrollbar the theme owns

Nothing styled the scrollbar, so the window showed WebKitGTK's -- the
loudest tell that this is not the program it is dressed as. The slider takes
scrollbar-slider, which derives from --border for any theme that does not
name it.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
```

---

### Task 7: Row hover, elevation, and the rest of the chrome

**Files:**
- Modify: `crates/norte-gui-tauri/ui/src/style.css`
- Modify: `crates/norte-gui-tauri/tests/variables_de_tema.rs` (empty `PENDIENTES_DE_GASTAR`)

**Interfaces:**
- Consumes: `--hover`, `--separator`, `--input-bg`, `--input-border`,
  `--widget-bg`, `--widget-shadow`, `--focus-border`, `--badge-fg`.

- [ ] **Step 1: Row hover**

`.row:hover` gets `background: var(--hover, var(--panel-focus-bg))`, and must
lose to both `[aria-selected="true"]` and `[data-marked="true"]` — check the
cascade order, because `.row` rules are written in a specific sequence at
`:490-525` and a hover that beats the cursor would hide where the keys go.

- [ ] **Step 2: Elevation**

Every chrome rule currently painted with `var(--border)` that separates two
SURFACES — `.keybar` border-top, `.panelbar` border-bottom, the menubar, the
sidebar edge — moves to `var(--separator, var(--border))`. A pane's own focus
border does **not**: that is `border-focus` and it means something else.

- [ ] **Step 3: Inputs, widgets, focus rings**

- Text inputs in dialogs, the palette and settings: `background:
  var(--input-bg, var(--panel-bg))`, `border: 1px solid var(--input-border,
  var(--border))`.
- `.dialog`, `.palette`, `.menu-items`, `#whichkey`: `background:
  var(--widget-bg, var(--panel-bg))`, and the hard-coded
  `box-shadow: 0 4px 16px rgb(0 0 0 / 35%)` at `:1234` becomes
  `0 4px 16px var(--widget-shadow, rgb(0 0 0 / 35%))`.
- A focused control gets `outline: 1px solid var(--focus-border,
  var(--border-focus))`. The sheet sets `outline: none` at `:478` and `:508`;
  read why before overriding — one of them is the scroller, which is focused
  as a whole and should stay ringless.
- `--badge-fg` is spent on the chip text beside the `--badge-bg` already
  applied in Task 4.

- [ ] **Step 4: Empty the pending list**

`PENDIENTES_DE_GASTAR` becomes `&[]`. Both directions of the guard are now
live and no allow-list remains except geometry.

- [ ] **Step 5: Run the tests**

Run: `just t norte-gui-tauri` — expected PASS, including
`cada_color_proyectado_lo_gasta_la_hoja` with no exemptions.

- [ ] **Step 6: See it, in both schemes and three themes**

`just link-gui`, then `ntc-gui` with `[ui] theme = "vscode-dark"`, again with
`"vscode-light"`, and again with `"gruvbox-dark"`. The third is the acceptance
test for the derivation table: **gruvbox must look exactly as it looked
before this plan started.** If it does not, a derivation default is wrong —
fix the table, not the preset.

- [ ] **Step 7: Commit**

```
feat(gui): hover, elevation and widget surfaces from the theme

Rows react to the pointer, chrome rules separate surfaces through
--separator instead of the pane border, inputs and widgets take their own
backgrounds, and the widget shadow stops being a hard-coded black. Every
default derives from a colour the theme already had, so the eight existing
presets look exactly as they did.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
```

- [ ] **Step 8: GATE — run `just ci-fast` ONCE**

The plan's second and last budgeted `ci-fast`. Same rule as Task 4: a red
result is diagnosed with `just t <crate>`, not by re-running the gate.

---

### Task 8: Open the PR for `feat/vscode-theme`

- [ ] **Step 1: Dispatch the reviewers yourself, before the PR**

Per CLAUDE.md, the agent doing the work dispatches its own reviewers and
reports with findings already applied. By surface: `rust-reviewer` (the
`norte-theme` and `norte-ui-host` diff) and `encoding-auditor` is **not**
needed here — nothing touches paths, filenames or archives. No
`protocol-guardian`: the wire is untouched.

Give the reviewer the commit range, what the change is for, and the specific
question you are unsure about. The useful one here: *does `REQUESTABLE`
narrowing break any plugin contract we have promised in a doc or an ADR, and
is the derivation table's `--badge-bg` → `--selection-bg` default right for a
light theme?*

Apply BLOCKER and MAJOR findings in ONE pass with ONE `just t` per crate
touched. Say which MINORs you skipped and why.

- [ ] **Step 2: Open the PR**

Body ends with:

```
🤖 Generated with [Claude Code](https://claude.com/claude-code)
```

---

### Task 9: A pure parser for VSCode theme JSON

**Files:**
- Create: `crates/norte-theme/src/vscode.rs`
- Modify: `crates/norte-theme/src/lib.rs` (add `pub mod vscode;`)

**Interfaces:**
- Produces:
  - `pub struct VsCodeTheme { pub name: Option<String>, pub base: VsBase, pub include: Option<String>, pub colors: HashMap<String, Color> }`
  - `pub enum VsBase { Dark, Light }`
  - `pub fn parse(src: &str) -> Result<VsCodeTheme, VsCodeError>`
  - **`Color` has no `from_hex`.** The constructor from a string is
    `Color::parse(&str) -> Result<Color, ColorParseError>`
    (`color.rs:68`), and it accepts `#rgb` and `#rrggbb` only — so the
    eight-digit `#rrggbbaa` that VSCode allows must have its alpha stripped
    in **this** module before `Color::parse` ever sees it.
  - `pub fn to_theme(colors: &HashMap<String, Color>, base: &Theme) -> Theme`
  - `pub const MAPPING: &[(&str, Role, bool)]` — VSCode colour id, role,
    `true` if it fills `bg`
- Task 11 (the CLI) walks the `include` chain and does all file I/O.

**Boundary:** this module does **no I/O**. `norte-theme` is a pure model crate
with no dependency of ours, and rule 2 keeps blocking reads out of it. The
chain walk lives in the binary.

- [ ] **Step 1: Write the failing tests**

Create the test module inside `crates/norte-theme/src/vscode.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// JSONC: un tema del marketplace trae comentarios y comas colgantes.
    /// `serde_json` no los acepta, así que hay que limpiarlos antes.
    #[test]
    fn parsea_jsonc_con_comentarios_y_coma_colgante() {
        let src = r#"{
            // el nombre
            "name": "Mío",
            "type": "dark",
            "colors": {
                "editor.background": "#1F1F1F", /* bloque */
                "foreground": "#CCCCCC",
            },
        }"#;
        let t = parse(src).expect("parsea");
        assert_eq!(t.name.as_deref(), Some("Mío"));
        assert!(matches!(t.base, VsBase::Dark));
        assert_eq!(t.colors.len(), 2);
    }

    /// Un `#RRGGBBAA` de ocho dígitos es legal en VSCode. norte no tiene
    /// alfa: se descarta el canal y se documenta, en vez de fallar.
    #[test]
    fn un_color_con_alfa_pierde_el_alfa_y_no_falla() {
        let src = r#"{"type":"dark","colors":{"widget.shadow":"#00000066"}}"#;
        let t = parse(src).expect("parsea");
        assert_eq!(t.colors["widget.shadow"].to_hex(), "#000000");
    }

    /// `include` se DEVUELVE sin resolver: este módulo no toca el disco.
    #[test]
    fn el_include_se_devuelve_crudo() {
        let src = r#"{"include":"./dark_plus.json","type":"dark","colors":{}}"#;
        assert_eq!(parse(src).unwrap().include.as_deref(), Some("./dark_plus.json"));
    }

    /// Un tema que fija VEINTE claves produce un tema COMPLETO: lo que no
    /// dice lo pone la base (spec 2026-09-11, F5). Sin esto, importar del
    /// marketplace daría veinte colores y el resto en monocromo, que se lee
    /// como un importador roto.
    #[test]
    fn lo_que_el_tema_no_dice_lo_pone_la_base() {
        let base = Theme::preset("vscode-dark").unwrap().expect("preset");
        let mut colors = HashMap::new();
        // `Color::parse`, NO `from_hex` — esa no existe (color.rs:68).
        colors.insert("editor.background".to_owned(), Color::parse("#101010").unwrap());
        let t = to_theme(&colors, &base);
        assert_eq!(t.style(Role::Background).bg.unwrap().to_hex(), "#101010");
        // No lo dijo: lo pone la base, no el fallback monocromo.
        assert_eq!(
            t.style(Role::Hover).bg,
            base.style(Role::Hover).bg,
            "un rol que el tema calla lo hereda de la base"
        );
        assert!(t.style(Role::Regular).fg.is_some());
    }

    /// `tokenColors` se IGNORA, y el tipo lo dice: norte no colorea
    /// sintaxis. Un importador que se tragase la mitad de su entrada en
    /// silencio sería un test verde que no prueba nada.
    #[test]
    fn token_colors_se_ignora() {
        let src = r#"{"type":"dark","colors":{},"tokenColors":[{"scope":"comment"}]}"#;
        assert!(parse(src).is_ok());
    }
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-theme`
Expected: the module does not exist.

- [ ] **Step 3: Implement**

- JSONC stripping: write it by hand, ~30 lines, a small state machine over
  chars tracking "inside a string", "inside `//`", "inside `/* */`", and a
  trailing-comma pass. **Do not add a dependency for this** — CLAUDE.md rule 8
  makes a new dep a thing you justify in the PR, and `json5`/`jsonc-parser`
  would be a whole crate for thirty lines. Note in the rustdoc that a `//`
  inside a string literal must not be treated as a comment, and test it.
- `VsBase` from `"type"`, defaulting to `Dark` when absent (VSCode's own
  default), documented.
- `MAPPING` is the table from the spec's F2 section plus the core-role
  mappings listed in Task 5 Step 3. One table, used by both `to_theme` here
  and, by eye, by the two presets.
- `to_theme` clones `base`, then for each `(id, role, is_bg)` in `MAPPING`
  present in `colors`, overwrites that side of the role's `Style`. It uses
  `Style::overlay` so an id that fills only `bg` does not erase a `fg` the
  base had.
- `VsCodeError` with `thiserror`: `Json`, and `BadColor { id: String }`.
  Rule 6 — no `unwrap()` outside tests.

Every public item gets Spanish rustdoc and a doctest.

- [ ] **Step 4: Run the tests**

Run: `just t norte-theme` and `cargo test -p norte-theme --doc` — expected
PASS.

- [ ] **Step 5: Commit**

```
feat(theme): parse a VSCode theme JSON, over a base palette

A VSCode theme is not a complete palette: dark_modern.json includes
dark_plus.json includes dark_vs.json, and none of the three defines list.*,
scrollbarSlider.* or badge.* -- those are the editor's built-in registry
defaults. So an imported theme layers over vscode-dark or vscode-light and
comes out complete rather than twenty-coloured. JSONC by hand rather than a
dependency; tokenColors is ignored and the rustdoc says so.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
```

---

### Task 10: A theme name can be a file you own

**Files:**
- Modify: `crates/norte-frontend/src/theme.rs`
- Modify: 7 call sites of `preset_names()`: `norte-ui-host/src/controller/wizard.rs:16`, `.../settings.rs:143`, `.../profiles.rs:350`, `norte-tui/src/wizard.rs:42`, `norte-tui/src/app/pickers.rs:11`, `norte-tui/src/screens/settings.rs:125`

**Interfaces:**
- Produces:
  - `pub struct ThemeEntry { pub name: String, pub origin: ThemeOrigin }`
  - `pub enum ThemeOrigin { Preset, User }` — derives
    `Debug, Clone, Copy, PartialEq, Eq`, because the tests below compare it
    with `==`
  - `pub fn available_themes(config_dir: &Path) -> Vec<ThemeEntry>`
  - `resolve_theme` gains the middle branch.

**The resolution order is fixed and load-bearing: embedded preset → user theme
directory → path.** An embedded preset cannot be shadowed by a file, so a
stale `~/.config/norte/themes/nord.toml` cannot change what `nord` means.

- [ ] **Step 1: Write the failing tests**

In `crates/norte-frontend/src/theme.rs`'s test module:

```rust
/// Un nombre que no es preset se busca en `<config>/themes/<nombre>.toml`
/// ANTES de tratarse como ruta (spec 2026-09-11, F5).
#[test]
fn un_nombre_de_usuario_resuelve_contra_el_directorio_de_temas() {
    let dir = tempfile::tempdir().unwrap();
    let temas = dir.path().join("themes");
    std::fs::create_dir_all(&temas).unwrap();
    std::fs::write(temas.join("mio.toml"), "name = \"mio\"\n[roles]\nregular = { fg = \"#abcdef\" }\n").unwrap();
    let t = resolve_theme_in(Some("mio"), dir.path()).expect("resuelve");
    assert_eq!(t.name.as_deref(), Some("mio"));
}

/// Y un preset EMBEBIDO no se puede tapar con un fichero: `nord` es `nord`
/// aunque haya un `themes/nord.toml` viejo al lado.
#[test]
fn un_fichero_no_puede_tapar_un_preset() {
    let dir = tempfile::tempdir().unwrap();
    let temas = dir.path().join("themes");
    std::fs::create_dir_all(&temas).unwrap();
    std::fs::write(temas.join("nord.toml"), "name = \"impostor\"\n").unwrap();
    let t = resolve_theme_in(Some("nord"), dir.path()).expect("resuelve");
    assert_eq!(t.name.as_deref(), Some("nord"), "gana el preset embebido");
}

/// La lista que ven las SEIS superficies es una sola, y marca el origen.
#[test]
fn available_themes_lista_presets_y_temas_de_usuario() {
    let dir = tempfile::tempdir().unwrap();
    let temas = dir.path().join("themes");
    std::fs::create_dir_all(&temas).unwrap();
    std::fs::write(temas.join("mio.toml"), "name = \"mio\"\n").unwrap();
    let v = available_themes(dir.path());
    assert!(v.iter().any(|e| e.name == "nord" && e.origin == ThemeOrigin::Preset));
    assert!(v.iter().any(|e| e.name == "mio" && e.origin == ThemeOrigin::User));
    assert!(v.iter().any(|e| e.name == "vscode-dark"));
    // Un fichero que no parsea no revienta la lista: no aparece.
    std::fs::write(temas.join("roto.toml"), "esto no es toml {{{").unwrap();
    assert!(!available_themes(dir.path()).iter().any(|e| e.name == "roto"));
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-frontend` — expected: the three functions do not exist.

- [ ] **Step 3: Implement**

- `resolve_theme_in(spec, config_dir)` carries the real logic; keep
  `resolve_theme(spec)` as a thin wrapper that calls it with
  `norte_config::dirs::config_dir()`, so every existing caller keeps compiling
  and the tests get an injectable directory. Say in the rustdoc that the
  injectable one exists for tests and for profiles, which resolve against a
  different directory.
- `available_themes` lists `preset_names()` then reads `*.toml` in
  `<config>/themes/`, skipping anything that does not parse, and skipping any
  file whose stem collides with a preset name — the collision is already
  unreachable by the resolution order, and listing it twice would offer the
  reader a choice that does not exist.
- `is_preset` is unchanged: a user theme is not a preset and does touch the
  disk, so the `spawn_blocking` split it drives stays correct.

- [ ] **Step 4: Replace the seven callers**

Each of the six surfaces listed under **Files** builds its own `Vec<String>`
from `preset_names()` today. Point each at `available_themes()`. Read each
call site before editing: two of them (`settings.rs` in both frontends) also
build a separate `preset_names` list for validation, and that one may need to
stay a preset-only list — check what it validates before collapsing it.

- [ ] **Step 5: Write the parity test**

Per ADR 0077, a decision duplicated between frontends diverges silently. Add a
test asserting the TUI's theme list and the window's theme list come from the
same call and hold the same names for the same config dir. Put it wherever the
repo's existing frontend-parity tests live — grep for the ones written for the
paridad plan before inventing a location.

- [ ] **Step 6: Run the tests**

Run: `just t norte-frontend`, `just t norte-tui`, `just t norte-ui-host` —
expected PASS.

- [ ] **Step 7: Commit**

```
feat(frontend): a theme name can be a file you own

[ui] theme resolved a bundled preset or a path, so an imported theme was
invisible to the picker, the wizard and profiles. A name now resolves
against <config>/themes/<name>.toml in between -- and never before a
bundled preset, so a stale themes/nord.toml cannot change what nord means.

available_themes() replaces seven separate preset_names() calls. "Which
themes exist" was written seven times, which is how a list diverges.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
```

---

### Task 11: `norte theme import`

**Files:**
- Create: `crates/norte-cli/src/theme.rs`
- Modify: `crates/norte-cli/src/main.rs` (the `Command` enum, ~line 227)
- Test: `crates/norte-cli/tests/` (follow the existing test layout)
- Modify: the `norte-cli` help golden

**Interfaces:**
- Consumes: `norte_theme::vscode::{parse, to_theme}` (Task 9),
  `norte_frontend::theme::available_themes` (Task 10).

- [ ] **Step 1: Write the failing test**

A fixture theme JSON goes in the canonical `norte-testkit` corpus (the repo
has a `/fixture` skill for this — use it rather than dropping a file in
`tests/`). Then:

```rust
/// Importar produce un TOML que PARSEA como tema y resuelve todos los
/// roles: el viaje entero, no solo el parser.
#[test]
fn importar_produce_un_tema_completo_y_resoluble() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join(".config/norte");
    std::fs::create_dir_all(&config).unwrap();

    let salida = std::process::Command::new(env!("CARGO_BIN_EXE_norte"))
        .args(["theme", "import", FIXTURE_DRACULA, "--name", "dracula"])
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .output()
        .expect("arranca el binario");
    assert!(
        salida.status.success(),
        "import falló: {}",
        String::from_utf8_lossy(&salida.stderr)
    );

    let escrito = std::fs::read_to_string(config.join("themes/dracula.toml")).unwrap();
    let t = norte_theme::Theme::from_toml(&escrito).expect("el TOML escrito parsea");
    for &role in norte_theme::Role::CORE {
        let s = t.style(role);
        assert!(s.fg.is_some() || s.bg.is_some(), "{role:?} sin color tras importar");
    }
}

/// Un nombre que ya es preset embebido se RECHAZA: el resolutor pone los
/// presets primero, así que el fichero nunca se leería y escribirlo sería
/// una operación que no hace nada sin decirlo.
#[test]
fn importar_con_el_nombre_de_un_preset_se_rechaza() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".config/norte")).unwrap();
    let salida = std::process::Command::new(env!("CARGO_BIN_EXE_norte"))
        .args(["theme", "import", FIXTURE_DRACULA, "--name", "nord"])
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .output()
        .expect("arranca el binario");
    assert!(!salida.status.success(), "debería rechazar un nombre de preset");
}
```

`FIXTURE_DRACULA` is the path to the corpus fixture from Step 1. Confirm the
binary name in `crates/norte-cli/Cargo.toml`'s `[[bin]]` before using
`CARGO_BIN_EXE_norte` — if the target is named differently, the env var
follows the target name, not the crate name.

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-cli`

- [ ] **Step 3: Implement**

```rust
/// Temas: importar uno de VSCode a `<config>/themes/`
Theme {
    #[command(subcommand)]
    cmd: ThemeCmd,
},
```

`ThemeCmd::Import { file: PathBuf, name: Option<String>, r#use: bool }`.

The command: read the file, `parse`, then walk `include` relative to the
file's directory, merging each parent UNDER the child (a child's colour wins),
with a depth cap of 8 and a cycle check on canonicalised paths — a theme that
includes itself must be an error, not a hang. Then `to_theme` over the base
named by `VsBase`. Write the result as TOML to
`<config>/themes/<name>.toml`, where `name` is `--name`, else the JSON's
`"name"` slugified, else the file stem. **Refuse to overwrite an existing file
without `--force`**, and refuse a name that collides with a bundled preset —
the resolution order would make that file unreachable, so writing it would be
a silent no-op.

Touch `[ui] theme` only with `--use`, via `norte_config::persist_set` (which
preserves comments), never by rewriting the file.

This is a binary: `anyhow` is correct here (rule 6).

- [ ] **Step 4: Regenerate the CLI help golden**

Run the golden update the repo uses: `NORTE_UPDATE_GOLDEN=1 just t norte-cli`.
Check `git diff` on the golden shows only the new subcommand.

- [ ] **Step 5: Run the tests**

Run: `just t norte-cli` — expected PASS.

- [ ] **Step 6: Try it for real**

Download one real marketplace theme JSON and import it. Open `ntc-gui` with
it. A test that the TOML parses does not tell you whether the result is
legible.

- [ ] **Step 7: Commit**

```
feat(cli): norte theme import

Reads a VSCode theme JSON, walks its include chain (depth-capped, cycle
checked), layers it over vscode-dark or vscode-light by its declared type,
and writes <config>/themes/<name>.toml. It refuses a name that collides
with a bundled preset, because the resolver puts presets first and the file
would never be read.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
```

---

### Task 12: The ADR, the docs, and the close

**Files:**
- Create: `docs/adr/00NN-vscode-theme-and-chrome-roles.md` (use the `/adr` skill — it picks the number)
- Modify: `docs/theming.md`
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Write the ADR**

It amends ADR 0020 and ADR 0037. Record, with the reasoning and not just the
outcome:

1. The ten chrome roles, and why they derive in the stylesheet rather than in
   `Role::fallback` (a literal cannot follow eight palettes; and eighty
   invented values was the alternative).
2. `Role::CORE` vs `Role::ALL` — what a preset owes.
3. `Role::REQUESTABLE` — this **narrows** ADR 0037's vocabulary, which is the
   part a future reader most needs to find written down.
4. The resolver's third branch and its fixed order.
5. That a VSCode theme JSON is not a complete palette, which is why the
   presets carry registry defaults and the importer needs a base.

- [ ] **Step 2: Document the themes**

`docs/theming.md`: the two new presets; the chrome roles in the custom-theme
section with the derivation table; `norte theme import`; the
`<config>/themes/` directory and the resolution order; and the VSCode
typography note — `[ui] font_size = 13` for chrome-like sizing, and that a
`Theme` deliberately carries no fonts.

- [ ] **Step 3: Update the memory**

Per the standing instruction, this work closes with a memory entry, not only
code. Write one memory file for the non-obvious findings — that a VSCode theme
JSON is not a complete palette, and that adding a `Role` silently widens what
a plugin may request — and add its one-line pointer to `MEMORY.md`.

- [ ] **Step 4: GATE — `just ci` ONCE**

Run the recipes one at a time in the foreground (`lint`, `test`, `docs`,
`cov`), never piped through `tail`: a killed pipe leaves nothing behind.
`just ci` does not fit in a background job here — it is killed at ~5 minutes.

Note that `cov` can only move if you touched proto/vfs/core. This plan touches
none of the three, so a coverage failure means something unexpected happened —
read it rather than re-running.

- [ ] **Step 5: Commit and open the PRs**

Three branches, three PRs, each under 400 net changed lines where practical.
Stacked PRs do not retarget themselves: merge bottom-up and verify
`origin/main` afterwards, not the PR list.

---

## Deferred to its own plan

**F4 (icons)** — a Seti-style file icon set (MIT) in the `file-icons` plugin
and Codicons (CC-BY-4.0) for the chrome. It is a separate subsystem: a WIT
package bump with ADR 0105's eleven-site checklist, and a scoped `deny.toml`
exception with an attribution file, since CC-BY-4.0 is not in the allow list.
Nothing in this plan blocks on it, and it blocks on nothing here.
