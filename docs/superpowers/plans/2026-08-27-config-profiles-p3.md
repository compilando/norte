# Configuration profiles — P3 (the TUI can pick one)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make profiles reachable by a human: a picker, a `--profile` flag, a hot switch that says what it could not do, and the two wiring gaps P1 left behind.

**Architecture:** The pure picker is a sibling of `layout_picker.rs` in `norte-frontend`; the TUI paints it through the six-site template the layout picker already uses. D7's three-way rule moves up to cover the *whole* layer instead of `norte.toml` alone, so nothing duplicates it. The sticky profile arrives with the UI session, i.e. after the daemon connects, so startup applies it through the same switch path as any other switch.

**Tech Stack:** Rust 2024, `norte-config`, `norte-frontend`, `norte-tui`, ratatui, nextest, `just`.

**Spec:** `docs/superpowers/specs/2026-08-26-config-profiles-design.md`, decisions **D7**, **D8**, **D9**, **D10**. **ADR:** `docs/adr/0079-a-profile-declares-it-does-not-execute.md`. Read both; this plan argues from them.

**Predecessor:** `docs/superpowers/plans/2026-08-26-config-profiles-p1-p2.md`, merged as `3425fe89`. P1 built the layer, P2 the state. Nothing constructs a `Layer::Profile` in production yet — this phase is what makes one exist.

## Global Constraints

- **Branch:** `feat/config-profiles-p3`, already created off `main` at `3425fe89`.
- **Protocol 0.58.0 and bridge 40 do not move.** No task here touches `crates/norte-proto` or `crates/norte-ui-host`. The window is P4.
- **Session body schema stays at 2.** Both numbers (`norte_frontend::session::SCHEMA_VERSION`, `norte_core::ui_session::disk::SCHEMA_VERSION`) went to 2 in P2 and nothing in this phase adds a field.
- **Rule 1 (bytes):** a profile name is an `OsStr` from `--profile` to the directory. `Cli::os_text` exists for exactly this — `--layout` uses it, and `Cli::text` is what #246 was. The single UTF-8 crossing is `SessionBody.active`, per D4.
- **Rule 2 (no blocking I/O on the event loop):** listing `profiles/` and reading each one's `norte.toml` happens in `spawn_blocking`, off the runtime, and arrives at `App` already done. `dispatch.rs:181` is the worked example and #244 is why.
- **Rule 7:** the picker's rows, cursor and warnings live in `norte-frontend`. The TUI paints; it does not decide.
- **Closes #305**, both halves: Task 1 is its first, Task 6 its second.
- **Gate budget:** `just t <crate>` freely. **ONE** `just ci-fast` after Task 3, **ONE** after Task 6. `just ci` once at the close. `just t` does **not** compile `norte-gui-tauri` — if a task touches anything it consumes, run `cargo check -p norte-gui-tauri --lib`.
- **Docs:** after a task that adds a documented public item, `cargo test -p <crate> --doc`; after one that writes an intra-doc link, `cargo doc -p <crate> --no-deps`. Neither `just t` nor `just c` sees these, and this branch has already been bitten once.
- **Language:** match the module you are in. `norte-config/src/profiles.rs` is English, `norte-tui/src/dispatch.rs` and `app/pickers.rs` are Spanish.

## The startup ordering problem, and its answer

`SessionBody.active` names the sticky profile, and it arrives from the daemon — *after* the configuration that says how to reach the daemon has been loaded. The profile cannot be part of the first load.

**The answer is that startup applies the sticky profile the same way any switch does**, once the session lands. Almost everything falls out for free: the screen is built from `apply_session` anyway, and theme, keymap, columns, favourites and arrangements are all re-appliable.

The one genuine casualty is **`ui.lang`**, which initialises Fluent once per process (`norte-tui/src/main.rs`, `norte_i18n::force`). A profile that sets a different language announces that it needs a restart rather than pretending. This is a deliberate, stated cost: a per-profile language is a fringe want, and the alternative — a second, local pointer file holding the active profile name so it can be read before connecting — duplicates state that the session already owns and needs its own format and migration.

**Task 4 measures the rest of the list rather than assuming it**, and pins it with a test that fails when a new `[ui]` scalar appears unclassified.

## File Structure

**Created:**

- `crates/norte-frontend/src/profile_picker.rs` — rows, cursor, per-row facts. Pure. Sibling of `layout_picker.rs` and deliberately shaped like it.
- `crates/norte-tui/tests/profile_switch.rs` — the switch sequence end to end over a real `App`.

**Modified:**

- `crates/norte-config/src/profiles.rs` — `load_with_profile` generifies over the loader (Task 1).
- `crates/norte-frontend/src/config.rs` — `load_with_profile` for `FrontendConfig`; `FrontendConfig::keymap_layer_dirs` (Task 6).
- `crates/norte-frontend/src/keymap/rebind.rs` — `RebindSplit` exposes which layer it targeted (Task 6).
- `crates/norte-frontend/src/keymap/catalogue.rs` — four command ids.
- `crates/norte-frontend/src/menu.rs` — the menu entry.
- `crates/norte-frontend/src/lib.rs` — the module.
- `crates/norte-tui/src/keymap.rs` — id → variant, around `:143`.
- `crates/norte-tui/src/dispatch.rs` — the arms, around `:181`.
- `crates/norte-tui/src/app.rs` — the picker field, around `:399`.
- `crates/norte-tui/src/app/pickers.rs` — open/close, around `:35`.
- `crates/norte-tui/src/app/profile.rs` — **new module** for the switch sequence.
- `crates/norte-tui/src/ui/pickers.rs` — the painter, around `:232`.
- `crates/norte-tui/src/main.rs` — `--profile`, around `:41-53`.
- `crates/norte-tui/src/shortcuts_editor.rs` — the write target, `:345` and `:443`.
- `crates/norte-i18n/i18n/{en,es}.ftl` — every new string.

---

## Task 1: D7 covers the whole layer, not just `norte.toml`

Closes the first half of #305. Do this first: every later task depends on there being one place where "what happens to a broken profile" is decided.

**Files:**
- Modify: `crates/norte-config/src/profiles.rs`
- Modify: `crates/norte-frontend/src/config.rs`
- Test: inline `mod tests` in both

**Interfaces:**
- Consumes: `ProfileSource`, `ProfileError`, `ProfileLoad` (P1).
- Produces:
  - `pub struct Loaded<T> { pub config: T, pub active: Option<OsString>, pub degraded: Option<String> }` in `norte-config`, replacing `ProfileLoad` (which becomes `Loaded<CommonConfig>`, kept as a type alias so P1's tests read unchanged).
  - `pub fn load_with<T>(layers_for: &impl Fn(Option<&OsStr>) -> Layers, name: Option<&OsStr>, source: ProfileSource, load: &impl Fn(&Layers) -> Result<T, ConfigError>) -> Result<Loaded<T>, ProfileError>` — the D7 rule, generic over what a layer means.
  - `load_with_profile` keeps its signature and becomes a one-line call into `load_with` with `crate::load::load`.
  - `norte_frontend::config::load_with_profile(layers_for, name, source) -> Result<norte_config::Loaded<FrontendConfig>, ProfileError>` — the same rule over `keymap.toml` and `openers.toml` too.

`norte_frontend::config::load` already returns `Result<FrontendConfig, ConfigError>`, the same error type, so the generic needs no error plumbing.

- [ ] **Step 1: Write the failing test**

In `crates/norte-frontend/src/config.rs`, inside `mod tests`:

```rust
/// D7 es una regla sobre la CAPA, no sobre `norte.toml`. Un `keymap.toml`
/// roto en el perfil PEGAJOSO tiene que degradar igual: `load_keymap_layer`
/// es fatal para toda capa que no sea de proyecto, así que sin esto un perfil
/// con una errata en un atajo deja al lector fuera del programa — y sin
/// manera de elegir otro, que es justo lo que D7 existe para impedir.
#[test]
fn un_keymap_roto_en_el_perfil_pegajoso_degrada() {
    let usuario = tempfile::tempdir().expect("tempdir");
    let dir = usuario.path().join("profiles").join("work");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("norte.toml"), "[ui]\ntheme = \"nord\"\n").expect("write");
    // `keymap` (la lista entera) es clave SOLO de preset: en una capa es un
    // error que nombra el fichero culpable.
    std::fs::write(
        dir.join("keymap.toml"),
        "[pane]\nkeymap = [{ on = [\"f5\"], run = \"pane.copy\" }]\n",
    )
    .expect("write");
    let raiz = usuario.path().to_path_buf();
    let layers_for = move |n: Option<&std::ffi::OsStr>| Layers {
        dirs: match n {
            None => vec![(raiz.clone(), Layer::User)],
            Some(n) => vec![
                (raiz.clone(), Layer::User),
                (raiz.join("profiles").join(n), Layer::Profile),
            ],
        },
    };

    let r = load_with_profile(
        &layers_for,
        Some(std::ffi::OsStr::new("work")),
        norte_config::ProfileSource::Sticky,
    )
    .expect("arranca igual");
    assert_eq!(r.active, None, "sin capa de perfil");
    assert!(r.degraded.is_some(), "y no en silencio");

    // Y con `--profile`, fatal: el lector nombró ese perfil.
    assert!(
        load_with_profile(
            &layers_for,
            Some(std::ffi::OsStr::new("work")),
            norte_config::ProfileSource::Explicit,
        )
        .is_err()
    );
}

/// El camino feliz trae el keymap DEL PERFIL, y su kind viaja para que
/// `split_at` pueda cortar por él.
#[test]
fn un_perfil_sano_aporta_su_capa_de_keymap() {
    let usuario = tempfile::tempdir().expect("tempdir");
    let dir = usuario.path().join("profiles").join("work");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(
        dir.join("keymap.toml"),
        "[pane]\nprepend_keymap = [{ on = [\"f5\"], run = \"pane.move\" }]\n",
    )
    .expect("write");
    let raiz = usuario.path().to_path_buf();
    let layers_for = move |n: Option<&std::ffi::OsStr>| Layers {
        dirs: match n {
            None => vec![(raiz.clone(), Layer::User)],
            Some(n) => vec![
                (raiz.clone(), Layer::User),
                (raiz.join("profiles").join(n), Layer::Profile),
            ],
        },
    };
    let r = load_with_profile(
        &layers_for,
        Some(std::ffi::OsStr::new("work")),
        norte_config::ProfileSource::Explicit,
    )
    .expect("carga");
    assert_eq!(r.active.as_deref(), Some(std::ffi::OsStr::new("work")));
    assert_eq!(r.config.keymap_layer_kinds, vec![Layer::Profile]);
}
```

- [ ] **Step 2: Run and watch it fail** — `just t norte-frontend`. Expected: `load_with_profile` not found in this module.

- [ ] **Step 3: Generify in `norte-config`**

Rename `ProfileLoad` to `Loaded<T>` with `pub type ProfileLoad = Loaded<crate::load::CommonConfig>;` beside it, extract the body of `load_with_profile` into `load_with` taking the loader as a parameter, and leave `load_with_profile` as the `crate::load::load` instantiation. The three-way `match source`, the name validation, the listing check and the load-without-the-profile-first rule all move as one; **do not** re-derive them.

- [ ] **Step 4: Add the frontend's**

```rust
/// [`load`] con un perfil de por medio, con la regla de tres respuestas de
/// D7 aplicada a la CAPA ENTERA y no solo a su `norte.toml`.
///
/// Ésta es la que llama un frontend. La de `norte-config` decide sobre
/// `norte.toml`; un perfil también trae `keymap.toml` y `openers.toml`, y los
/// dos son fatales para cualquier capa que no sea de proyecto, así que
/// pasando por la otra un perfil con una errata en un atajo se declaraba sano
/// y reventaba después (#305).
///
/// # Errors
/// [`norte_config::ProfileError`] según la procedencia, igual que la de abajo.
pub fn load_with_profile(
    layers_for: &impl Fn(&OsStr) -> Layers,
    name: Option<&OsStr>,
    source: norte_config::ProfileSource,
) -> Result<norte_config::Loaded<FrontendConfig>, norte_config::ProfileError>
```

(Match the real closure signature to `norte-config`'s — `Fn(Option<&OsStr>) -> Layers`. The sketch above is deliberately not copy-paste.)

- [ ] **Step 5: Run** — `just t norte-config && just t norte-frontend`. Both green, P1's five D7 tests included and unchanged.

- [ ] **Step 6: Docs** — `cargo test -p norte-config --doc`, `cargo doc -p norte-frontend --no-deps`.

- [ ] **Step 7: Commit** — `git add -A && git diff --cached --stat && git commit`, message saying that D7 is a rule about a layer.

---

## Task 2: The picker, pure

**Files:**
- Create: `crates/norte-frontend/src/profile_picker.rs`
- Modify: `crates/norte-frontend/src/lib.rs`

**Interfaces:**
- Consumes: `norte_config::list_profiles`, `valid_profile_name`.
- Produces:

```rust
pub struct UserProfile { pub name: OsString, pub title: Option<String>, pub problem: Option<String> }
pub struct Row {
    pub name: OsString,
    pub title: Option<String>,
    pub active: bool,
    pub shares_layout_name: bool,
    pub shares_keymap_name: bool,
    pub carries_state: bool,
    pub problem: Option<String>,
}
pub struct ProfilePicker { /* rows, cursor */ }
impl ProfilePicker {
    pub fn open(profiles: Vec<UserProfile>, active: Option<&OsStr>) -> Self;
    pub fn rows(&self) -> &[Row];
    pub fn cursor(&self) -> usize;
    pub fn up(&mut self); pub fn down(&mut self);
    pub fn chosen(&self) -> Option<&OsStr>;
}
```

Read `layout_picker.rs` before writing this and follow it: same `open`-takes-what-disk-said shape, same cursor clamping, same "warn about a name collision rather than silently doing something else". Two rows are new and both come from the spec:

- `carries_state` is `false` when the name is not valid UTF-8 (D4: such a profile works for configuration and cannot be sticky, and the picker says so in advance rather than letting the reader discover it at the next start).
- `problem` is `Some` when the profile's `norte.toml` did not parse. **The row stays**, exactly as the layout picker keeps an unparseable layout: hiding a directory the reader created is worse than showing it as broken.

- [ ] **Step 1: Write the failing tests**

```rust
/// La fila del perfil ACTIVO se marca. Sin eso, el selector es una lista de
/// nombres en la que no se sabe dónde estás.
#[test]
fn el_activo_se_marca() { /* … */ }

/// D4: un nombre que no es UTF-8 vale para configuración y NO puede llevar
/// estado, ni siquiera pegajoso. La fila lo dice ANTES de elegirlo, no
/// después de perderlo.
#[test]
#[cfg(unix)]
fn un_nombre_no_utf8_se_lista_y_avisa_de_que_no_guarda_estado() {
    use std::os::unix::ffi::OsStringExt;
    let hostil = OsString::from_vec(vec![b'w', 0xFF, b'k']);
    let p = ProfilePicker::open(
        vec![UserProfile { name: hostil.clone(), title: None, problem: None }],
        None,
    );
    let fila = &p.rows()[0];
    assert_eq!(fila.name, hostil, "los bytes intactos");
    assert!(!fila.carries_state);
}

/// Un perfil que no parsea SE LISTA, con su motivo. Esconder un directorio
/// que el lector creó es peor que enseñarlo roto.
#[test]
fn un_perfil_roto_se_lista_con_su_motivo() { /* … */ }

/// Compartir nombre con una disposición o con un preset de keymap se AVISA:
/// son tres ajustes distintos, y la coincidencia es una trampa si no se dice
/// — el selector de disposiciones ya lo hace por lo mismo.
#[test]
fn una_coincidencia_de_nombre_se_avisa() { /* … */ }

/// El cursor se acota a las filas que hay, y una lista vacía no panica.
#[test]
fn el_cursor_se_acota() { /* … */ }
```

Fill the elided bodies from `layout_picker.rs`'s equivalents; they are the same shape with different fields.

- [ ] **Step 2–4:** run (fail), implement, run (pass): `just t norte-frontend`.
- [ ] **Step 5: Docs** — `cargo test -p norte-frontend --doc`.
- [ ] **Step 6: Commit.**

---

## Task 3: The commands, the flag, and the startup path

**Files:**
- Modify: `crates/norte-frontend/src/keymap/catalogue.rs`, `menu.rs`
- Modify: `crates/norte-tui/src/keymap.rs:143`, `dispatch.rs:181`, `app.rs:399`, `app/pickers.rs:35`, `main.rs:41-53`
- Modify: `crates/norte-i18n/i18n/{en,es}.ftl`

**Interfaces:**
- Consumes: Task 1's `load_with_profile`, Task 2's `ProfilePicker`.
- Produces: command ids `profile.pick`, `profile.next`, `profile.prev`, `profile.save-as`; `--profile <name>`; `App::profile_picker`.

Four decisions, all already made, restated so nobody re-litigates them mid-task:

- The commands ship **unbound in every preset**. #228 is the lesson about presets leaving core commands unreachable, but binding four new keys across seven presets without being asked is the opposite mistake. They are reachable from the palette and the menu.
- `--profile` uses `Cli::os_text`, never `Cli::text` (#246).
- `profile.save-as` is declared here and **implemented in P5**; declare it `Planned` in the catalogue, not `live`, so the "every command has a dispatch arm" compile check (#112) and the availability surface both tell the truth.
- The listing happens in `spawn_blocking` and arrives at `App` already read, exactly as `Command::LayoutPick` does at `dispatch.rs:181`. Copy that arm's structure including its no-config-dir fallback — reading `./profiles/` out of the current directory is the #244 m3 bug.

- [ ] **Step 1: Write the failing test**

In `crates/norte-tui/tests/keymap.rs` (or wherever the catalogue's coverage test lives — find it first):

```rust
/// Los cuatro comandos de perfil existen en el catálogo, y ninguno viene
/// atado en ningún preset: elegir por el lector qué tecla es un perfil es lo
/// contrario del hueco que #228 arregló.
#[test]
fn los_comandos_de_perfil_existen_y_no_los_ata_ningun_preset() { /* … */ }
```

- [ ] **Step 2: Run and watch it fail.**
- [ ] **Step 3: Add the ids, the variants, the arms, the field, the flag.**
- [ ] **Step 4: Wire the startup path.**

In `main.rs`, the first load stays profile-less — the daemon socket comes from it. After the session arrives and before the screen is built, if `SessionBody.active` is non-empty, run the switch of Task 4 with `ProfileSource::Sticky`. With `--profile <name>` present, it is `ProfileSource::Explicit` and the flag **wins over** the sticky value: the reader named one this run.

- [ ] **Step 5: Run** — `just t norte-tui`, `just t norte-frontend`.
- [ ] **Step 6: Fluent** — every new string in both locales; the coverage test fails on a key present in one only.
- [ ] **Step 7: Commit.**
- [ ] **Step 8: Gate** — `just ci-fast`, one run. If red, reproduce the single failure with `just t <crate>` and fix it there.

---

## Task 4: The switch, and what it could not do

The heart of the phase (D8). **Files:** create `crates/norte-tui/src/app/profile.rs` and `crates/norte-tui/tests/profile_switch.rs`.

The sequence, in this order, and the order is the design:

1. Flush the outgoing profile's state into `layouts[old]` and its slots.
2. Rebuild the layers with the new profile directory and reload through Task 1's `load_with_profile`.
3. Apply theme, keymap, columns, favourites, openers.
4. Mount `layouts[new]` if the session has one; else the arrangement its `[ui] layout` names; else the factory default. A newly adopted arrangement is rebased with `Node::rebase_slot_ids(body.next_slot_base()?)` and inserted into `layouts` **before** any further base is asked for (see that method's rustdoc).
5. Seed each slot from its saved `SlotState`, or from `[profile.start]` translated through the rebase map, or from the fallback directory.
6. Emit one line naming what could not be applied without a restart.

- [ ] **Step 1: Write the failing tests**

```rust
/// Step 1 va PRIMERO: cambiar de perfil y volver devuelve cada panel donde
/// estaba. Si el volcado fuese después de recargar, el estado que se guarda
/// es el del perfil nuevo escrito bajo el nombre del viejo.
#[test]
fn ir_y_volver_devuelve_cada_panel_donde_estaba() { /* … */ }

/// Un perfil sin estado guardado abre por `[profile.start]`, y las claves de
/// esa tabla son los ids que escribe SU fichero de disposición — así que hay
/// que traducirlas por el mapa del rebase o abren el hueco equivocado.
#[test]
fn un_perfil_nuevo_abre_por_profile_start() { /* … */ }

/// Un cambio que falla en el paso 3 deja al lector en el perfil que tenía,
/// con el estado que tenía. Un perfil a medio aplicar no es un estado que
/// este diseño admita (D7, `ProfileSource::Switch`).
#[test]
fn un_cambio_que_falla_no_deja_nada_a_medias() { /* … */ }

/// Y lo que no se puede aplicar en caliente se DICE. Un cambio que se calla
/// lo que no hizo es un cambio que miente.
#[test]
fn lo_que_no_se_aplica_en_caliente_se_anuncia() { /* … */ }
```

- [ ] **Step 2: Run and watch them fail.**

- [ ] **Step 3: Measure the hot-reload list.** Do not assume it. For each `[ui]` scalar in `CommonConfig`, determine by reading where it is consumed whether re-applying it takes effect in a running TUI. `ui_lang` is known: `norte_i18n::force` runs once at startup. Produce a table in `app/profile.rs`'s module doc.

- [ ] **Step 4: Pin the list with a test that cannot rot**

```rust
/// Cada escalar de `[ui]` está clasificado como recargable o no. Este test
/// se pone rojo cuando alguien añade uno y no lo clasifica, que es la única
/// manera de que la línea del paso 6 siga siendo verdad dentro de un año.
#[test]
fn todo_escalar_de_ui_esta_clasificado() { /* … */ }
```

Implement it against an explicit list of field names checked for exhaustiveness by destructuring `CommonConfig` with no `..` — a new field then fails to compile, which is stronger than a failing assertion.

- [ ] **Step 5: Implement, run, commit.**

---

## Task 5: The TUI surface

**Files:** `crates/norte-tui/src/ui/pickers.rs:232` (painter), `app/pickers.rs` (open/close), `menu.rs` entry.

Follow the layout picker's painter. What the rows must show, because each one prevents a specific confusion: the active profile marked; the title when there is one and the directory name always (the name is the identity, D3); a broken profile's reason; a non-UTF-8 name's "cannot carry state"; and a name collision with a layout or a keymap preset.

- [ ] **Step 1:** Write a rendering test in the style of the existing picker tests (find them under `crates/norte-tui/tests/` first — `layout_picker.rs` is the model).
- [ ] **Step 2–4:** run, implement, run.
- [ ] **Step 5:** Consider driving it once under the tmux harness — the memory note is that piloting the TUI in tmux surfaces composition bugs a green suite does not see.
- [ ] **Step 6:** Commit.

---

## Task 6: A rebind lands where the split says

Closes the second half of #305. D10 moved the write *target* in P2 and the writer never learned.

**Files:** `crates/norte-frontend/src/config.rs` (`keymap_layer_dirs`), `crates/norte-frontend/src/keymap/rebind.rs` (`RebindSplit`), `crates/norte-tui/src/shortcuts_editor.rs:345` and `:443`.

**Interfaces:**
- Produces: `FrontendConfig::keymap_layer_dirs: Vec<PathBuf>`, in lockstep with `keymap_layers` and `keymap_layer_kinds` — the three are one table, and the rustdoc on `keymap_layer_kinds` already explains why a positional guess is silent and wrong. Plus `RebindSplit::target_index(&self) -> Option<usize>`, `None` meaning "the write creates a file that does not exist yet".

- [ ] **Step 1: Write the failing test**

```rust
/// D10 movió el DESTINO del rebind al `keymap.toml` del perfil y el escritor
/// siguió resolviendo `user_config_dir()`. Resultado: la puerta planifica
/// sobre el fichero del perfil y la escritura cae en el del usuario, donde el
/// perfil la tapa — «visiblemente guardado, y sin hacer nada», que es la
/// frase de D10. Y es PEOR que antes: antes un orden inesperado se rechazaba.
#[test]
fn con_perfil_activo_el_atajo_se_escribe_en_el_fichero_del_perfil() { /* … */ }

/// Sin perfil, sigue yendo al del usuario. Esta tarea no puede cambiar lo que
/// ya funcionaba.
#[test]
fn sin_perfil_el_atajo_sigue_yendo_al_del_usuario() { /* … */ }
```

- [ ] **Step 2–4:** run, implement, run.
- [ ] **Step 5:** Close #305 by hand — `Cierra #305` in a commit does **not** close it; GitHub only understands closes/fixes/resolves in English.
- [ ] **Step 6:** Commit.
- [ ] **Step 7: Gate** — `just ci-fast`, one run.

---

## Closing the branch

- `just ci` once, by recipe (`lint`, `test`, `docs`, `cov`) and in the foreground: it does not survive a background job here.
- `cargo check -p norte-gui-tauri --lib` — `just t` excludes that crate and P1 already shipped one arm it never compiled.
- Amend the spec if any decision moved, as P1 did. A spec that disagrees with the code is worse than no spec.
- Changelog entry, and update ADR 0079's consequences if #305's answers turned out different from what it predicted.

## What P3 leaves for later

**P4** is the window: the picker's DTO in `norte-ui-host`, the renderer, the golden corpus entry, and the bridge bump. **P5** is `profile.save-as` (the `toml_edit` writer `settings.rs` already has), `norte doctor` validation, and the help topics — which means regenerating the `norte-cli` golden with `NORTE_UPDATE_GOLDEN=1`.
