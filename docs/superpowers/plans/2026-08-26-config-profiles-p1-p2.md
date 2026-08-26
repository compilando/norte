# Configuration profiles — P1 (the layer) and P2 (the state)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give norte a fourth configuration layer the reader picks by name, and give each pick its own live screen state — with no user interface yet, so the mechanism is complete and tested before any frontend depends on it.

**Architecture:** A profile is a configuration-layer directory under the user's config dir (`profiles/<name>/`), spliced into `Layers.dirs` between `User` and `Project`. It may set presentation only, gated by a positive allow-list rather than by the existing `!= Layer::Project` negation. Its live screen state reuses `SessionBody.layouts`, whose map key becomes the profile name, with slot ids allocated per profile so a flat `slots` map needs no schema surgery.

**Tech Stack:** Rust 2024, `norte-config` (`toml`, `serde`), `norte-frontend` (`serde_json`), nextest, `just`.

**Spec:** `docs/superpowers/specs/2026-08-26-config-profiles-design.md` — read it first. Every task below cites the decision (D1–D10) it implements; when this plan and the spec disagree, the spec wins and the plan is the bug.

## Global Constraints

- **Branch:** `feat/config-profiles`. It already carries the spec commit `9e206b34`. Do not branch again.
- **Protocol:** unchanged at **0.58.0**. No task in this plan may touch `crates/norte-proto`. If one appears to need to, stop and say so — it means a decision was wrong, not that the version should move.
- **Session body schema:** `norte_frontend::session::SCHEMA_VERSION` goes **1 → 2** exactly once, in Task 6. It is `pub const SCHEMA_VERSION: u32` at `crates/norte-frontend/src/session.rs:24`.
- **Bridge:** `BRIDGE_VERSION` goes **40 → 41** exactly once, in Task 1. It is declared in TWO places that must match: `crates/norte-ui-host/src/lib.rs` (re-export; the constant itself lives in that crate) and `crates/norte-gui-tauri/ui/src/types.ts:12`. `just ci-fast` does **not** run `gui-ci`, so a desync ships green — Task 1 has an explicit step for the TypeScript side.
- **Session body cap:** `norte_proto::methods::SESSION_BODY_MAX` is `1024 * 1024`. It is a limit this plan must respect, never change.
- **Rule 1 (bytes):** a profile name is a directory name. It is `OsStr`/`OsString` everywhere except the one place the spec's D4 permits: the UTF-8 key of `SessionBody.layouts`. A `to_str().unwrap()` anywhere in this plan's diff is grounds for rejecting it.
- **Rule 6 (typed errors):** `norte-config` and `norte-frontend` are libraries. `thiserror`, never `anyhow`; no `unwrap()`/`expect()` outside tests without a comment stating the invariant.
- **Docs:** `norte-frontend` and `norte-config` public items need rustdoc. `just t` runs nextest, which does **not** run doctests, and `just c` does **not** check intra-doc links. After any task that adds a documented public item: `cargo test -p <crate> --doc`. After any task that writes a `[`Type`]` link: `cargo doc -p <crate> --no-deps`. Seconds each, and this repository has been bitten by it three times in one session.
- **Gate budget:** `just t <crate>` freely during red→green. **ONE** `just ci-fast` after Task 5, **ONE** after Task 9. `just ci` once at the close of the branch, not before. Never re-run the gate to find out whether a fix worked — reproduce the single failure with `just t <crate>`.
- **Language:** this codebase writes rustdoc and comments in the language of the surrounding module. `norte-config/src/dirs.rs` is English; `norte-config/src/load.rs` and `norte-frontend/src/session.rs` are Spanish. Match the file you are in, do not translate what is already there.

## File Structure

**Created:**

- `crates/norte-config/src/profiles.rs` — profile directory resolution, listing, and the three-way failure rule of D7. One responsibility: turning a profile *name* into a layer (or into a stated reason why not). Kept out of `dirs.rs` because `dirs.rs` answers "where does configuration live" for every norte process, and profiles are a policy on top of that answer.
- `crates/norte-config/tests/profiles.rs` — integration tests over real temp directories, for the parts that are about the filesystem rather than about merging.

**Modified:**

- `crates/norte-config/src/dirs.rs` — `Layer::Profile` variant; `profiles_dir`; `standard_layers_with_profile`; `standard_layers_no_project` excludes the new layer.
- `crates/norte-config/src/schema.rs` — the `[profile]` section (`title`, `start`).
- `crates/norte-config/src/load.rs` — the positive allow-list replacing two `!= Layer::Project` negations; `CommonConfig::profile_warnings`; `CommonConfig::profile_start`.
- `crates/norte-config/src/lib.rs` — re-exports.
- `crates/norte-tui/src/lua/host.rs:33`, `crates/norte-tui/src/lua/api.rs:335` — exhaustive matches on `Layer`.
- `crates/norte-gui-tauri/src/startup.rs:345` — exhaustive match mapping `Layer` into `ConfigLayer`.
- `crates/norte-ui-host/src/settings.rs:32` — `ConfigLayer::Profile` and its Fluent label.
- `crates/norte-gui-tauri/ui/src/types.ts` — `BRIDGE_VERSION` and the `ConfigLayer` union.
- `crates/norte-frontend/src/session.rs` — `SessionBody::active`, `SCHEMA_VERSION` 2, `next_slot_base`, the profile-state cap in `prune`.
- `crates/norte-frontend/src/layout/tree.rs` — `Node::rebase_slot_ids`.
- `crates/norte-frontend/src/keymap/rebind.rs:435` — the widened cut of D10.
- `i18n/en/*.ftl`, `i18n/es/*.ftl` — the `ConfigLayer::Profile` label.

---

## Task 1: `Layer::Profile` exists and nothing else changes

Implements D1's type. This task adds a variant and repairs every place that stops compiling. It deliberately does **not** produce a profile layer anywhere — after it, `Layer::Profile` is a value nothing constructs.

**Files:**
- Modify: `crates/norte-config/src/dirs.rs:15-22`
- Modify: `crates/norte-tui/src/lua/host.rs:33`
- Modify: `crates/norte-tui/src/lua/api.rs:335`
- Modify: `crates/norte-gui-tauri/src/startup.rs:345`
- Modify: `crates/norte-ui-host/src/settings.rs:32-39`
- Modify: `crates/norte-gui-tauri/ui/src/types.ts:12`
- Modify: `i18n/en/*.ftl`, `i18n/es/*.ftl`

**Interfaces:**
- Consumes: nothing.
- Produces: `norte_config::Layer::Profile`, ordered strictly between `Layer::User` and `Layer::Project` (`Layer` derives `PartialOrd, Ord`, and that ordering IS the precedence — putting the variant in the wrong position is a silent bug, not a compile error). `norte_ui_host::settings::ConfigLayer::Profile`.

- [ ] **Step 1: Write the failing test**

In `crates/norte-config/src/dirs.rs`, inside the existing `mod tests`:

```rust
/// El orden del enum ES la precedencia (ADR 0007): sistema → usuario →
/// perfil → proyecto. Un `Profile` colocado en otro sitio compila igual y
/// deja la capa mandando donde no debe, así que se fija aquí.
#[test]
fn el_perfil_va_entre_usuario_y_proyecto() {
    assert!(Layer::System < Layer::User);
    assert!(Layer::User < Layer::Profile);
    assert!(Layer::Profile < Layer::Project);
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `just t norte-config`
Expected: FAIL to compile — `no variant named 'Profile' found for enum 'Layer'`.

- [ ] **Step 3: Add the variant**

In `crates/norte-config/src/dirs.rs`, between `User` and `Project`:

```rust
    /// `<config>/profiles/<name>` — the layer the reader picks by name
    /// (spec 2026-08-26, D1). Above `User` because picking a profile is
    /// meant to override what the user's own `norte.toml` says; below
    /// `Project` so ADR 0026 and #260 are untouched.
    Profile,
```

- [ ] **Step 4: Repair the three exhaustive matches**

`crates/norte-tui/src/lua/host.rs:33` — the layer's name for `init.lua`:

```rust
        Layer::Profile => "profile",
```

`crates/norte-tui/src/lua/api.rs:335` — the human-facing origin of a script:

```rust
            Layer::Profile => "init.lua (perfil)",
```

`crates/norte-gui-tauri/src/startup.rs:345`:

```rust
                    norte_config::Layer::Profile => ConfigLayer::Profile,
```

And in `crates/norte-ui-host/src/settings.rs`, add the variant to `ConfigLayer` between `User` and `Project`, plus its arm in `label_id` returning a new Fluent key (follow the naming of the three keys already there). Add that key to **both** `i18n/en` and `i18n/es` — the settings surfaces have a coverage test that fails on a key present in one locale only.

- [ ] **Step 5: Bump the bridge on both sides**

`crates/norte-ui-host`: `BRIDGE_VERSION` 40 → 41.
`crates/norte-gui-tauri/ui/src/types.ts:12`: `export const BRIDGE_VERSION = 41;`, and add `"profile"` to the `ConfigLayer` union in the same file (match the serde renaming the Rust enum uses — check it, do not assume lowercase).

- [ ] **Step 6: Run the tests**

Run: `just t norte-config` — Expected: PASS.
Run: `just t norte-ui-host` — Expected: PASS.
Run: `cd crates/norte-gui-tauri/ui && npm test` — Expected: PASS. This is the step `ci-fast` will not do for you.

- [ ] **Step 7: Commit**

```bash
git add -A
git diff --cached --stat
git commit -m "feat(config): Layer::Profile, entre usuario y proyecto"
```

---

## Task 2: A profile name resolves to a directory, and to a layer

Implements D1's resolution and D4's byte discipline.

**Files:**
- Create: `crates/norte-config/src/profiles.rs`
- Modify: `crates/norte-config/src/dirs.rs`
- Modify: `crates/norte-config/src/lib.rs`
- Test: inline `mod tests` in `profiles.rs`, plus `crates/norte-config/tests/profiles.rs`

**Interfaces:**
- Consumes: `Layer::Profile` (Task 1); `norte_config::dirs::user_config_dir_on`, `standard_layers_on`.
- Produces:
  - `pub fn profiles_dir_from(get: &impl Fn(&str) -> Option<OsString>) -> Option<PathBuf>` — the user config dir joined with `profiles`.
  - `pub fn profile_dir_from(get: &impl Fn(&str) -> Option<OsString>, name: &OsStr) -> Option<PathBuf>`.
  - `pub fn standard_layers_with_profile(name: Option<&OsStr>) -> Layers` and its injectable seam `standard_layers_with_profile_on(windows: bool, get: &impl Fn(&str) -> Option<OsString>, name: Option<&OsStr>) -> Layers`.
  - `pub fn standard_layers_no_project_on(windows: bool, get: &impl Fn(&str) -> Option<OsString>, name: Option<&OsStr>) -> Layers` — the core-consumer variant, which drops **both** `Project` and `Profile`.
  - `pub fn list_profiles(dir: &Path) -> std::io::Result<Vec<OsString>>` — directory names only, sorted by bytes, non-directories skipped.

`crates/norte-config/src/lib.rs` declares `pub mod profiles;` next to the existing `pub mod dirs;`, and re-exports `list_profiles`, `standard_layers_with_profile`, `ProfileSource` and `load_with_profile` in the `pub use` block that already re-exports from `dirs` and `load` — the integration test in this task calls `norte_config::list_profiles`, so the re-export is part of the task, not an afterthought.

- [ ] **Step 1: Write the failing tests**

In the new `crates/norte-config/src/profiles.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k: &str| {
            owned
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn el_dir_de_perfiles_cuelga_del_dir_de_usuario() {
        let e = env(&[("NORTE_CONFIG_DIR", "/custom")]);
        assert_eq!(
            profiles_dir_from(&e),
            Some(PathBuf::from("/custom").join("profiles"))
        );
    }

    /// Bajo `NORTE_CONFIG_DIR` el resolutor es hermético (solo esa capa y
    /// `./.norte`), y el perfil tiene que quedarse DENTRO de esa hermeticidad
    /// o los tests dejarían de aislar lo que dicen aislar.
    #[test]
    fn con_norte_config_dir_el_perfil_sigue_dentro() {
        let e = env(&[("NORTE_CONFIG_DIR", "/custom")]);
        let l = standard_layers_with_profile_on(false, &e, Some(OsStr::new("work")));
        assert_eq!(
            l.dirs,
            vec![
                (PathBuf::from("/custom"), Layer::User),
                (PathBuf::from("/custom/profiles/work"), Layer::Profile),
                (PathBuf::from(".norte"), Layer::Project),
            ]
        );
    }

    #[test]
    fn sin_perfil_las_capas_son_las_de_siempre() {
        let e = env(&[("NORTE_CONFIG_DIR", "/custom")]);
        let con = standard_layers_with_profile_on(false, &e, None);
        let sin = crate::dirs::standard_layers_on(false, &e);
        assert_eq!(con.dirs, sin.dirs);
    }

    /// El perfil va DESPUÉS de usuario y ANTES de proyecto, con las tres
    /// capas presentes (el caso hermético de arriba no tiene `System`).
    #[test]
    fn el_perfil_se_intercala_entre_usuario_y_proyecto() {
        let e = env(&[("HOME", "/home/u")]);
        let l = standard_layers_with_profile_on(false, &e, Some(OsStr::new("photos")));
        let kinds: Vec<Layer> = l.dirs.iter().map(|(_, k)| *k).collect();
        assert_eq!(
            kinds,
            vec![Layer::System, Layer::User, Layer::Profile, Layer::Project]
        );
    }

    /// Un consumidor de valores del core (`[archive]`, `[ai]`) no ve la capa
    /// de perfil: no puede fijar nada de lo suyo (D2) y no tiene por qué
    /// saber qué perfil eligió un frontend.
    #[test]
    fn no_project_tampoco_trae_perfil() {
        let e = env(&[("HOME", "/home/u")]);
        let l = standard_layers_no_project_on(false, &e, Some(OsStr::new("work")));
        let kinds: Vec<Layer> = l.dirs.iter().map(|(_, k)| *k).collect();
        assert_eq!(kinds, vec![Layer::System, Layer::User]);
    }
}
```

- [ ] **Step 2: Write the failing filesystem test**

In the new `crates/norte-config/tests/profiles.rs`:

```rust
use std::ffi::OsString;

/// Los nombres son BYTES: un directorio con bytes no-UTF-8 se LISTA (existe,
/// el lector lo creó) en vez de desaparecer del selector. #245/#246 fueron
/// este bug dos veces con los nombres de disposición.
#[test]
#[cfg(unix)]
fn un_nombre_no_utf8_se_lista_tal_cual() {
    use std::os::unix::ffi::OsStringExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let hostil = OsString::from_vec(vec![b'w', 0xFF, b'k']);
    std::fs::create_dir(dir.path().join(&hostil)).expect("mkdir");
    std::fs::create_dir(dir.path().join("work")).expect("mkdir");
    std::fs::write(dir.path().join("no-soy-un-dir"), b"x").expect("write");

    let mut got = norte_config::list_profiles(dir.path()).expect("list");
    got.sort();
    let mut want = vec![hostil, OsString::from("work")];
    want.sort();
    assert_eq!(got, want, "los ficheros no son perfiles; los bytes se respetan");
}

/// Un directorio de perfiles que no existe no es un error: es «no tienes
/// perfiles todavía».
#[test]
fn sin_directorio_de_perfiles_la_lista_esta_vacia() {
    let dir = tempfile::tempdir().expect("tempdir");
    let got = norte_config::list_profiles(&dir.path().join("no-existe")).expect("list");
    assert!(got.is_empty());
}
```

- [ ] **Step 3: Run them and watch them fail**

Run: `just t norte-config`
Expected: FAIL to compile — `profiles` module and its functions do not exist.

- [ ] **Step 4: Implement**

Write `crates/norte-config/src/profiles.rs`. The shape, with the bodies left to you:

```rust
//! Profiles (spec 2026-08-26): turning a profile NAME into a configuration
//! layer, or into a stated reason why not.
//!
//! Kept out of `dirs.rs` because that module answers "where does
//! configuration live" for every norte process; this one is a policy on top
//! of that answer, and only the frontends ask it.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::dirs::{Layer, Layers, user_config_dir_on};

/// The directory holding every profile: `<user config dir>/profiles`.
#[must_use]
pub fn profiles_dir_from(get: &impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> { /* … */ }

/// One profile's directory. The name is joined as BYTES (rule 1).
#[must_use]
pub fn profile_dir_from(
    get: &impl Fn(&str) -> Option<OsString>,
    name: &OsStr,
) -> Option<PathBuf> { /* … */ }

/// Test seam. Not general API — call [`standard_layers_with_profile`].
#[doc(hidden)]
#[must_use]
pub fn standard_layers_with_profile_on(
    windows: bool,
    get: &impl Fn(&str) -> Option<OsString>,
    name: Option<&OsStr>,
) -> Layers { /* … */ }

/// The standard layers with `name`'s profile spliced in after `User`.
/// `None` gives exactly [`crate::dirs::standard_layers`].
#[must_use]
pub fn standard_layers_with_profile(name: Option<&OsStr>) -> Layers { /* … */ }

/// Test seam for the core-consumer variant.
#[doc(hidden)]
#[must_use]
pub fn standard_layers_no_project_on(
    windows: bool,
    get: &impl Fn(&str) -> Option<OsString>,
    name: Option<&OsStr>,
) -> Layers { /* … */ }

/// Every profile that exists, by directory name, sorted by bytes.
/// A missing directory is an empty list, not an error.
///
/// # Errors
/// Any I/O error other than "not found" while reading the directory.
pub fn list_profiles(dir: &Path) -> std::io::Result<Vec<OsString>> { /* … */ }
```

Two implementation notes that are decisions, not style. **The splice is positional but not by index**: find the `User` entry in the vector `standard_layers_on` produced and insert after it; a profile with no `User` layer at all (no `HOME`, no `NORTE_CONFIG_DIR`) has nowhere to live, so `standard_layers_with_profile` returns the plain layers unchanged. And **`list_profiles` skips anything that is not a directory**, so a stray `profiles/README` is not a profile.

- [ ] **Step 5: Run the tests**

Run: `just t norte-config`
Expected: PASS, all six.

- [ ] **Step 6: Check the docs the test loop cannot**

Run: `cargo test -p norte-config --doc` — Expected: PASS.
Run: `cargo doc -p norte-config --no-deps` — Expected: no warnings. This module writes `[`crate::dirs::standard_layers`]`, which is exactly the intra-doc link `just c` and `just t` do not check.

- [ ] **Step 7: Commit**

```bash
git add -A
git diff --cached --stat
git commit -m "feat(config): un nombre de perfil resuelve a un directorio y a una capa"
```

---

## Task 3: The profile layer may set presentation and nothing else

Implements D2 — the finding this design turned up, and the one place where getting it wrong is a security bug rather than a bug.

**Files:**
- Modify: `crates/norte-config/src/load.rs:1932` and `:1985` (the two `!= Layer::Project` negations) and `:1315` (`CommonConfig`)
- Test: inline `mod tests` in `load.rs`

**Interfaces:**
- Consumes: `Layer::Profile` (Task 1).
- Produces: `CommonConfig::profile_warnings: Vec<String>` — sibling of the existing `project_warnings`, never merged into it. Two provenances that read identically in a message are two provenances a reader cannot act on differently.

- [ ] **Step 1: Write the failing test**

In `crates/norte-config/src/load.rs`, inside `mod tests`. Follow the existing helpers there for writing a layer to a temp dir; the assertions are what matters:

```rust
/// D2: el recorte de proyecto está escrito como `!= Layer::Project`, así que
/// una cuarta variante hereda EN SILENCIO todo lo del usuario. Este test es
/// el que impide que eso vuelva: un perfil no redirige el transporte, no
/// enciende la IA, no elige dónde se escriben los logs y no sube los límites
/// anti-bomba.
#[test]
fn un_perfil_no_puede_tocar_daemon_ai_log_ni_archive() {
    let usuario = tempfile::tempdir().expect("tempdir");
    let perfil = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        perfil.path().join("norte.toml"),
        r#"
[daemon]
socket = "/tmp/ajeno.sock"
[ai]
enabled = true
[log]
dir = "/tmp/logs-ajenos"
[archive]
max_entries = 999999999
"#,
    )
    .expect("write");

    let layers = Layers {
        dirs: vec![
            (usuario.path().to_path_buf(), Layer::User),
            (perfil.path().to_path_buf(), Layer::Profile),
        ],
    };
    let cfg = load(&layers).expect("carga");

    assert_eq!(cfg.daemon_socket, None, "el transporte no se redirige");
    assert!(!cfg.ai.enabled, "la IA no se enciende sola");
    assert_eq!(cfg.log_dir, None, "los logs no se mudan");
    assert_eq!(
        cfg.archive_max_entries, None,
        "los límites anti-bomba no suben"
    );
    assert_eq!(
        cfg.profile_warnings.len(),
        4,
        "y las cuatro se DICEN: callarlas convierte el selector en un \
         escalador de permisos"
    );
    for seccion in ["daemon", "ai", "log", "archive"] {
        assert!(
            cfg.profile_warnings.iter().any(|w| w.contains(seccion)),
            "falta el aviso de [{seccion}]"
        );
    }
}

/// Y lo que SÍ puede: presentación entera, más el preset de keymap y los
/// favoritos, que la capa de proyecto no puede y ésta sí — un perfil es del
/// usuario, un repositorio ajeno no.
#[test]
fn un_perfil_pisa_presentacion_keymap_y_favoritos() {
    let usuario = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        usuario.path().join("norte.toml"),
        "[ui]\ntheme = \"nord\"\nlayout = \"orthodox\"\n[keymap]\npreset = \"orthodox\"\n",
    )
    .expect("write");
    let perfil = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        perfil.path().join("norte.toml"),
        r#"
[ui]
theme = "solarized"
layout = "explorer"
[keymap]
preset = "far"
[[hotlist]]
name = "src"
path = "/home/u/src"
"#,
    )
    .expect("write");

    let layers = Layers {
        dirs: vec![
            (usuario.path().to_path_buf(), Layer::User),
            (perfil.path().to_path_buf(), Layer::Profile),
        ],
    };
    let cfg = load(&layers).expect("carga");

    assert_eq!(cfg.ui_theme.as_deref(), Some("solarized"));
    assert_eq!(cfg.ui_layout.as_deref(), Some("explorer"));
    assert_eq!(cfg.preset, "far");
    assert_eq!(cfg.hotlist.len(), 1);
    assert!(cfg.profile_warnings.is_empty());
}

/// Y el proyecto sigue mandando sobre el perfil (D1): esto es lo que hace
/// que ADR 0026 y #260 no cambien de significado.
#[test]
fn proyecto_sigue_pisando_al_perfil_en_presentacion() {
    let perfil = tempfile::tempdir().expect("tempdir");
    std::fs::write(perfil.path().join("norte.toml"), "[ui]\ntheme = \"solarized\"\n")
        .expect("write");
    let proyecto = tempfile::tempdir().expect("tempdir");
    std::fs::write(proyecto.path().join("norte.toml"), "[ui]\ntheme = \"nord\"\n")
        .expect("write");

    let layers = Layers {
        dirs: vec![
            (perfil.path().to_path_buf(), Layer::Profile),
            (proyecto.path().to_path_buf(), Layer::Project),
        ],
    };
    let cfg = load(&layers).expect("carga");
    assert_eq!(cfg.ui_theme.as_deref(), Some("nord"));
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-config`
Expected: FAIL — `no field 'profile_warnings' on type 'CommonConfig'`. Once that field exists, the first test fails on its assertions, because `!= Layer::Project` is true for `Layer::Profile` and every forbidden section goes straight through.

- [ ] **Step 3: Add the field**

In `CommonConfig` (`load.rs:1315`), after `project_warnings`:

```rust
    /// Claves que una capa de PERFIL declaró y no puede fijar (D2), con su
    /// motivo ya dicho.
    ///
    /// Va aparte de [`Self::project_warnings`] a propósito: son dos
    /// procedencias distintas, y un lector no puede reaccionar igual a «tu
    /// perfil pide algo que un perfil no decide» que a «este repositorio
    /// trae una config que no se aplica». Vacío = el perfil solo pedía lo
    /// suyo.
    pub profile_warnings: Vec<String>,
```

- [ ] **Step 4: Replace the negations with a predicate**

In `load()`, add near the top of the layer loop a named predicate rather than editing the two `if`s in place — the comment blocks around them are load-bearing and must survive:

```rust
    /// Qué capas pueden fijar lo que NO es presentación.
    ///
    /// Escrito en positivo a propósito. La versión anterior era
    /// `*kind != Layer::Project`, y con ella añadir una variante de [`Layer`]
    /// concedía en SILENCIO el transporte, la IA, los logs y los límites
    /// anti-bomba a la capa nueva. Un `match` exhaustivo obliga a decidirlo
    /// cuando la variante se añade, que es cuando alguien lo está pensando.
    const fn manda_fuera_de_presentacion(kind: Layer) -> bool {
        match kind {
            Layer::System | Layer::User => true,
            Layer::Profile | Layer::Project => false,
        }
    }
```

Use it at both sites, and at the `keymap.preset` site (`load.rs:1932`) use a **separate** predicate, because a profile *may* set the preset and a project may not:

```rust
    /// `keymap.preset`: el perfil SÍ (es del usuario), el proyecto NO (#260).
    const fn manda_el_keymap(kind: Layer) -> bool {
        match kind {
            Layer::System | Layer::User | Layer::Profile => true,
            Layer::Project => false,
        }
    }
```

Then, for a `Layer::Profile` layer only, push one warning per forbidden section the parsed file actually declared. Check presence on the parsed struct (`parsed.daemon`, `parsed.log`, `parsed.ai`, `parsed.archive`) — a section that is absent produces no warning, and `[[hotlist]]` is not on this list because a profile may set it.

- [ ] **Step 5: Run the tests**

Run: `just t norte-config`
Expected: PASS, all three.

- [ ] **Step 6: Commit**

```bash
git add -A
git diff --cached --stat
git commit -m "feat(config): la capa de perfil fija presentación, y lo demás se dice en voz alta"
```

---

## Task 4: `[profile]` — a title, and where the panels start

Implements D3.

**Files:**
- Modify: `crates/norte-config/src/schema.rs`
- Modify: `crates/norte-config/src/load.rs`
- Test: inline `mod tests` in `load.rs`

**Interfaces:**
- Consumes: `Layer::Profile`, `manda_fuera_de_presentacion` (Task 3).
- Produces:
  - `schema::ProfileSection { title: Option<String>, start: BTreeMap<String, String> }`, reached as `NorteToml::profile`.
  - `CommonConfig::profile_title: Option<String>`.
  - `CommonConfig::profile_start: BTreeMap<u32, PathBuf>` — parsed slot ids, invalid keys warned about and dropped.

- [ ] **Step 1: Write the failing test**

```rust
/// D3: `[profile.start]` es lo que hace útil un perfil recién creado. Las
/// claves son ids de hueco TAL Y COMO los escribe la disposición del perfil.
#[test]
fn profile_start_se_lee_con_sus_ids() {
    let perfil = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        perfil.path().join("norte.toml"),
        "[profile]\ntitle = \"Trabajo\"\n\n[profile.start]\n1 = \"/home/u/src\"\n2 = \"/tmp\"\n",
    )
    .expect("write");
    let layers = Layers {
        dirs: vec![(perfil.path().to_path_buf(), Layer::Profile)],
    };
    let cfg = load(&layers).expect("carga");
    assert_eq!(cfg.profile_title.as_deref(), Some("Trabajo"));
    assert_eq!(
        cfg.profile_start.get(&1).map(std::path::PathBuf::as_path),
        Some(std::path::Path::new("/home/u/src"))
    );
    assert_eq!(cfg.profile_start.len(), 2);
}

/// Una clave que no es un id de hueco no rompe el arranque: se tira y se
/// dice. El fichero es del usuario, pero un dedazo en un id no vale una
/// negativa a arrancar.
#[test]
fn una_clave_de_start_que_no_es_un_id_se_avisa_y_se_tira() {
    let perfil = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        perfil.path().join("norte.toml"),
        "[profile.start]\nizquierda = \"/tmp\"\n1 = \"/home/u\"\n",
    )
    .expect("write");
    let layers = Layers {
        dirs: vec![(perfil.path().to_path_buf(), Layer::Profile)],
    };
    let cfg = load(&layers).expect("carga");
    assert_eq!(cfg.profile_start.len(), 1, "el bueno sobrevive");
    assert!(
        cfg.profile_warnings.iter().any(|w| w.contains("izquierda")),
        "y el malo se dice por su nombre"
    );
}

/// `[profile]` en una capa que NO es de perfil no significa nada, y decirlo
/// evita que alguien lo escriba en su norte.toml y espere que pase algo.
#[test]
fn profile_fuera_de_un_perfil_se_ignora_con_aviso() {
    let usuario = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        usuario.path().join("norte.toml"),
        "[profile]\ntitle = \"no soy un perfil\"\n",
    )
    .expect("write");
    let layers = Layers {
        dirs: vec![(usuario.path().to_path_buf(), Layer::User)],
    };
    let cfg = load(&layers).expect("carga");
    assert_eq!(cfg.profile_title, None);
    assert_eq!(cfg.profile_warnings.len(), 1);
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-config`
Expected: FAIL — `[profile]` is an unknown field under `deny_unknown_fields`, so the load errors before any assertion runs.

- [ ] **Step 3: Add the section to the schema**

In `crates/norte-config/src/schema.rs`, following the shape of the sections already there (`#[serde(default, deny_unknown_fields)]`, rustdoc on every field):

```rust
/// `[profile]` — solo tiene sentido en `profiles/<nombre>/norte.toml`
/// (spec 2026-08-26, D3). En cualquier otra capa se ignora con un aviso.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProfileSection {
    /// Nombre para enseñar. La IDENTIDAD del perfil es su directorio, no
    /// esto: dos perfiles pueden compartir título y seguir siendo dos.
    pub title: Option<String>,
    /// Dónde abre cada hueco cuando este perfil todavía no tiene estado
    /// guardado. Claves: ids de hueco de la disposición del perfil, en
    /// texto (TOML no tiene claves numéricas).
    pub start: std::collections::BTreeMap<String, String>,
}
```

and add `pub profile: ProfileSection` to `NorteToml`.

- [ ] **Step 4: Merge it in `load()`**

Only for `Layer::Profile`, and last-wins like everything else. Parse each `start` key with `str::parse::<u32>()`; a key that does not parse becomes a `profile_warnings` entry naming the key and is dropped. For any other layer, a non-default `[profile]` produces one warning and is ignored.

Values are stored as `PathBuf` and **not** expanded here: `~` expansion and relative-path resolution belong to the frontend that has a cwd, and doing it in the loader would bake this process's `$HOME` into a value the daemon might read.

- [ ] **Step 5: Run the tests**

Run: `just t norte-config`
Expected: PASS, all three.

- [ ] **Step 6: Regenerate the JSON schema if this crate feeds one**

Run: `just t norte-cli` — Expected: PASS. If a golden fails because `[profile]` is new, regenerate it with `NORTE_UPDATE_GOLDEN=1` and include the regenerated file in this commit, not a later one.

- [ ] **Step 7: Commit**

```bash
git add -A
git diff --cached --stat
git commit -m "feat(config): [profile], con su título y dónde abre cada hueco"
```

---

## Task 5: A broken profile has three answers

Implements D7 — the decision that came from reading `parse_layer`'s rustdoc rather than from taste.

**Files:**
- Modify: `crates/norte-config/src/profiles.rs`
- Test: inline `mod tests` in `profiles.rs`

**Interfaces:**
- Consumes: `standard_layers_with_profile_on` (Task 2), `load` (Task 3).
- Produces:

```rust
/// Quién pidió este perfil. Decide qué pasa si no carga (D7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileSource {
    /// Lo nombró el humano en esta invocación (`--profile`).
    Explicit,
    /// Lo traía la sesión de la vez anterior.
    Sticky,
    /// Se está cambiando a él con el programa en marcha.
    Switch,
}

/// La carga cuando hay un perfil de por medio, con lo que se degradó.
#[derive(Debug)]
pub struct ProfileLoad {
    /// La configuración resultante.
    pub config: crate::load::CommonConfig,
    /// El perfil que quedó activo. `None` = se arrancó SIN capa de perfil.
    pub active: Option<OsString>,
    /// Por qué no se pudo usar el que se pidió. `None` = se usó.
    pub degraded: Option<String>,
}

/// # Errors
/// [`crate::load::ConfigError`] cuando el perfil lo pidió el humano por su nombre
/// ([`ProfileSource::Explicit`]) y no carga, o cuando falla una capa que no
/// es la del perfil.
pub fn load_with_profile(
    layers_for: &impl Fn(Option<&OsStr>) -> Layers,
    name: Option<&OsStr>,
    source: ProfileSource,
) -> Result<ProfileLoad, crate::load::ConfigError>;
```

`layers_for` is a closure rather than a `Layers` value because the degrade path must rebuild the layers **without** the profile, and rebuilding is the only honest way to do that — filtering a vector would leave a `Layers` that no resolver ever produced.

- [ ] **Step 1: Write the failing tests**

```rust
    /// `--profile` roto ABORTA: el lector pidió ese perfil por su nombre, y
    /// arrancar como otra cosa sería contestar otra pregunta.
    #[test]
    fn explicito_y_roto_es_fatal() {
        let (dirs, _guards) = arbol_con_perfil_roto("work");
        let err = load_with_profile(&dirs, Some(OsStr::new("work")), ProfileSource::Explicit)
            .expect_err("tiene que abortar");
        assert!(
            format!("{err}").contains("norte.toml"),
            "y decir qué fichero: {err}"
        );
    }

    /// El PEGAJOSO roto arranca sin capa de perfil y lo dice. Abortar dejaría
    /// al lector fuera del programa, sin manera de elegir otro.
    #[test]
    fn pegajoso_y_roto_arranca_sin_perfil_y_lo_dice() {
        let (dirs, _guards) = arbol_con_perfil_roto("work");
        let r = load_with_profile(&dirs, Some(OsStr::new("work")), ProfileSource::Sticky)
            .expect("arranca igual");
        assert_eq!(r.active, None, "sin capa de perfil");
        assert!(r.degraded.is_some(), "y no en silencio");
    }

    /// Cambiar EN CALIENTE a uno roto se RECHAZA: el llamante se queda con la
    /// configuración que ya tenía. Un perfil a medio aplicar no es un estado
    /// que este diseño admita.
    #[test]
    fn cambiar_a_uno_roto_se_rechaza() {
        let (dirs, _guards) = arbol_con_perfil_roto("work");
        let err = load_with_profile(&dirs, Some(OsStr::new("work")), ProfileSource::Switch)
            .expect_err("el cambio se rechaza");
        assert!(format!("{err}").contains("norte.toml"));
    }

    /// Un perfil que NO EXISTE se trata igual que uno que no parsea: es la
    /// misma pregunta («¿puedo usar el que pediste?») con la misma respuesta
    /// por procedencia.
    #[test]
    fn un_perfil_que_no_existe_sigue_la_misma_regla() {
        let (dirs, _guards) = arbol_sin_perfiles();
        assert!(
            load_with_profile(&dirs, Some(OsStr::new("fantasma")), ProfileSource::Explicit)
                .is_err()
        );
        let r = load_with_profile(&dirs, Some(OsStr::new("fantasma")), ProfileSource::Sticky)
            .expect("arranca");
        assert_eq!(r.active, None);
        assert!(r.degraded.is_some());
    }

    /// Y el camino feliz sigue siendo el camino feliz.
    #[test]
    fn un_perfil_sano_queda_activo_y_sin_degradar() {
        let (dirs, _guards) = arbol_con_perfil_sano("work");
        let r = load_with_profile(&dirs, Some(OsStr::new("work")), ProfileSource::Sticky)
            .expect("carga");
        assert_eq!(r.active.as_deref(), Some(OsStr::new("work")));
        assert!(r.degraded.is_none());
    }
```

Write the three helpers (`arbol_con_perfil_roto`, `arbol_sin_perfiles`, `arbol_con_perfil_sano`) in the same `mod tests`. Each builds temp directories, writes `norte.toml` files (the broken one gets `[ui]\nthem = "nord"\n` — an unknown key, fatal under `deny_unknown_fields`, which is exactly the realistic typo), and returns a closure plus the `TempDir` guards. **Return the guards**: a dropped `TempDir` deletes the tree, and a test that loses them tests the missing-directory path by accident.

- [ ] **Step 2: Run and watch them fail**

Run: `just t norte-config`
Expected: FAIL to compile — `load_with_profile` does not exist.

- [ ] **Step 3: Implement**

The whole body is one `match` on the outcome and one on the source. `Explicit` and `Switch` propagate; `Sticky` retries with `layers_for(None)` and fills `degraded`. A "does not exist" check happens before loading, so its message says the profile is missing rather than reporting that a file could not be read.

Distinguish the profile layer's failure from any other layer's: if the user's own `norte.toml` is broken, that is fatal for every source, and degrading would hide it. The straightforward way is to attempt `layers_for(None)` first when the profile load fails and let *its* error propagate if it also fails.

- [ ] **Step 4: Run the tests**

Run: `just t norte-config`
Expected: PASS, all five.

- [ ] **Step 5: Docs**

Run: `cargo test -p norte-config --doc` — Expected: PASS.
Run: `cargo doc -p norte-config --no-deps` — Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add -A
git diff --cached --stat
git commit -m "feat(config): un perfil roto contesta según quién lo pidió"
```

- [ ] **Step 7: Spend the first gate run**

Run: `just ci-fast`
Expected: PASS. This is **one** of the two `ci-fast` runs this plan budgets. If it is red, reproduce the single failure with `just t <crate>` and fix it there — do not re-run `ci-fast` to check.

- [ ] **Step 8: Review before moving on**

Dispatch `rust-reviewer` over `HEAD~5..HEAD` and `security-reviewer` over the same range. Tell them what the change is *for* and ask the two questions worth asking: whether the positive predicate in Task 3 can be bypassed by any section reachable from a profile layer, and whether the degrade path in Task 5 can ever swallow a *user*-layer error. Apply BLOCKER and MAJOR findings in one pass, with one `just t norte-config` at the end. Say which MINORs you skipped and why.

---

## Task 6: The session remembers which profile you were in

Implements D5's field and the schema bump.

**Files:**
- Modify: `crates/norte-frontend/src/session.rs:24` (`SCHEMA_VERSION`), `:107` (`SessionBody`)
- Test: inline `mod tests` in `session.rs`

**Interfaces:**
- Consumes: nothing from P1 — `norte-frontend` does not depend on a profile *layer*, only on its name.
- Produces: `SessionBody::active: String` (empty = no profile), `SCHEMA_VERSION == 2`.

- [ ] **Step 1: Write the failing test**

```rust
    /// Un cuerpo v1 no trae `active`, y eso significa exactamente «sin
    /// perfil». Leerlo tiene que seguir funcionando: quitarle la sesión a
    /// quien actualiza el binario es justo lo que ADR 0059 promete que no
    /// pasa.
    #[test]
    fn un_cuerpo_v1_se_lee_como_sin_perfil() {
        let v1 = serde_json::json!({ "version": 1, "layouts": {}, "slots": {} });
        let b = SessionBody::from_value(1, &v1).expect("un v1 se sigue leyendo");
        assert_eq!(b.active, "", "sin perfil, que es la verdad");
    }

    #[test]
    fn el_perfil_activo_sobrevive_al_viaje() {
        let mut b = SessionBody::default();
        b.active = "work".to_owned();
        b.layouts
            .insert("work".into(), Node::slot(SlotId(1), KindId::browser()));
        let vuelta = SessionBody::from_value(SCHEMA_VERSION, &b.to_value()).expect("ida y vuelta");
        assert_eq!(vuelta.active, "work");
    }

    /// Y un cuerpo del FUTURO se sigue rehusando entero: subir a 2 no puede
    /// abrir la puerta a un 3.
    #[test]
    fn un_cuerpo_v3_se_sigue_rehusando() {
        let v3 = serde_json::json!({ "layouts": {}, "slots": {}, "active": "x" });
        assert!(matches!(
            SessionBody::from_value(SCHEMA_VERSION + 1, &v3),
            Err(SessionError::FromTheFuture { .. })
        ));
    }
```

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-frontend`
Expected: FAIL — `no field 'active' on type 'SessionBody'`.

- [ ] **Step 3: Implement**

`SCHEMA_VERSION` 1 → 2, and in `SessionBody`:

```rust
    /// El perfil activo. Vacío = ninguno.
    ///
    /// Es ESTADO, no configuración: lo que estabas haciendo, no lo que
    /// decidiste. Por eso vive aquí y no en el `norte.toml` del lector, que
    /// sigue siendo un fichero que escribió él.
    ///
    /// Es `String` y no `OsString` porque es la CLAVE de [`Self::layouts`],
    /// que es un objeto JSON y por tanto UTF-8 por construcción. Un perfil
    /// cuyo directorio no sea UTF-8 vale para configuración y no puede
    /// llevar estado (spec 2026-08-26, D4).
    #[serde(default)]
    pub active: String,
```

`#[serde(default)]` is what makes the v1 body read as "no profile" without a migration.

- [ ] **Step 4: Run the tests**

Run: `just t norte-frontend`
Expected: PASS. Expect several existing tests in `session.rs` and in `norte-tui` to fail construction because `SessionBody` gains a field — fix them by adding `active: String::new()` or by using `..Default::default()`, whichever the surrounding test already does.

- [ ] **Step 5: Commit**

```bash
git add -A
git diff --cached --stat
git commit -m "feat(frontend): la sesión recuerda en qué perfil estabas (cuerpo v2)"
```

---

## Task 7: Two profiles never share a slot

Implements D5's allocation rule. This is the task that makes the whole design work without a schema migration, and the one where a wrong answer means two workspaces overwriting each other's directories.

**Files:**
- Modify: `crates/norte-frontend/src/layout/tree.rs`
- Modify: `crates/norte-frontend/src/session.rs`
- Test: inline `mod tests` in both

**Interfaces:**
- Consumes: `SessionBody::active` (Task 6).
- Produces:
  - `Node::rebase_slot_ids(&self, base: u32) -> (Node, BTreeMap<SlotId, SlotId>)` — a copy whose slot ids start at `base`, in the deterministic order `Node::slot_ids` already returns, plus the old→new map.
  - `SessionBody::next_slot_base(&self) -> u32` — one past the highest slot id mentioned by any arrangement or held in `slots`.

The remap map is not a convenience. `[profile.start]` (Task 4) is keyed by the ids **as the profile's layout file writes them**, so applying it after a rebase requires the map; returning only the tree would make those keys unusable.

- [ ] **Step 1: Write the failing tests**

In `tree.rs`:

```rust
    /// Las disposiciones de fábrica usan 1..=8, todas. Sin rebase, dos
    /// perfiles comparten hueco 1 y se pisan el directorio y el historial —
    /// que es exactamente el bug que los perfiles existen para arreglar.
    #[test]
    fn rebase_reasigna_desde_la_base_y_devuelve_el_mapa() {
        let arbol = Node::split(
            Dir::H,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let (nuevo, mapa) = arbol.rebase_slot_ids(100);
        assert_eq!(nuevo.slot_ids(), vec![SlotId(100), SlotId(101)]);
        assert_eq!(mapa.get(&SlotId(1)), Some(&SlotId(100)));
        assert_eq!(mapa.get(&SlotId(2)), Some(&SlotId(101)));
    }

    /// Rebasar no puede cambiar la FORMA: mismo árbol, mismos kinds, mismos
    /// tamaños. Solo los números.
    #[test]
    fn rebase_conserva_la_forma() {
        let arbol = Node::split(
            Dir::V,
            vec![
                Node::slot(SlotId(3), KindId::new("places")),
                Node::split(
                    Dir::H,
                    vec![
                        Node::slot(SlotId(1), KindId::browser()),
                        Node::slot(SlotId(2), KindId::new("viewer")),
                    ],
                ),
            ],
        );
        let (nuevo, _) = arbol.rebase_slot_ids(50);
        assert_eq!(nuevo.slot_ids().len(), arbol.slot_ids().len());
        assert!(crate::layout::validate(&nuevo).is_ok());
    }

    /// Un árbol rebasado sigue siendo válido aunque el original repitiera un
    /// id: `validate` lo rechazaría, así que rebase no puede FABRICAR un
    /// duplicado a partir de uno sano.
    #[test]
    fn rebase_no_fabrica_duplicados() {
        let arbol = Node::split(
            Dir::H,
            vec![
                Node::slot(SlotId(7), KindId::browser()),
                Node::slot(SlotId(1), KindId::browser()),
            ],
        );
        let (nuevo, _) = arbol.rebase_slot_ids(10);
        assert!(nuevo.duplicate_slot_ids().is_empty());
    }
```

In `session.rs`:

```rust
    /// La base sale de TODO lo que hay: las disposiciones de cada perfil y
    /// los huecos guardados, huérfanos incluidos. Mirar solo el perfil activo
    /// reasignaría encima del estado de otro.
    #[test]
    fn la_base_deja_atras_todo_lo_que_ya_existe() {
        let mut b = SessionBody::default();
        b.layouts
            .insert("work".into(), Node::slot(SlotId(4), KindId::browser()));
        b.slots.insert(
            9,
            SlotState {
                path: VPath::parse("file:///tmp").expect("vpath"),
                cursor: 0,
                back: Vec::new(),
                forward: Vec::new(),
                sort: SortSpec::default(),
                columns: Vec::new(),
                show_hidden: false,
                touched_ms: 0,
            },
        );
        assert_eq!(b.next_slot_base(), 10);
    }

    #[test]
    fn una_sesion_vacia_empieza_en_uno() {
        assert_eq!(SessionBody::default().next_slot_base(), 1);
    }

    /// Dos perfiles adoptados sobre la MISMA disposición de fábrica acaban
    /// con conjuntos de huecos disjuntos. Éste es el test que fija el diseño.
    #[test]
    fn dos_perfiles_sobre_la_misma_disposicion_no_comparten_hueco() {
        let fabrica = Node::split(
            Dir::H,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let mut b = SessionBody::default();

        let (t1, _) = fabrica.rebase_slot_ids(b.next_slot_base());
        b.layouts.insert("work".into(), t1);
        let (t2, _) = fabrica.rebase_slot_ids(b.next_slot_base());
        b.layouts.insert("photos".into(), t2);

        let a: BTreeSet<SlotId> = b.layouts["work"].slot_ids().into_iter().collect();
        let c: BTreeSet<SlotId> = b.layouts["photos"].slot_ids().into_iter().collect();
        assert!(a.is_disjoint(&c), "work {a:?} y photos {c:?} se pisan");
    }
```

- [ ] **Step 2: Run and watch them fail**

Run: `just t norte-frontend`
Expected: FAIL to compile — neither method exists.

- [ ] **Step 3: Implement**

`rebase_slot_ids` walks the tree once building the map in `slot_ids()` order, then walks again substituting. `next_slot_base` is `max(all ids) + 1`, or `1` when there are none. Use `u32::saturating_add` — a session whose ids reached `u32::MAX` should stop allocating rather than wrap onto a live slot.

- [ ] **Step 4: Run the tests**

Run: `just t norte-frontend`
Expected: PASS, all five.

- [ ] **Step 5: Docs**

Run: `cargo test -p norte-frontend --doc` — Expected: PASS.
Run: `cargo doc -p norte-frontend --no-deps` — Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add -A
git diff --cached --stat
git commit -m "feat(frontend): los huecos se reparten por perfil, así que dos perfiles no se pisan"
```

---

## Task 8: The megabyte, and what goes first

Implements D6.

**Files:**
- Modify: `crates/norte-frontend/src/session.rs` (`prune`, and a new constant)
- Test: inline `mod tests` in `session.rs`

**Interfaces:**
- Consumes: `SessionBody::active` (Task 6), per-profile ids (Task 7).
- Produces: `pub const PROFILE_STATE_CAP: usize = 4;` and a `prune` that enforces it before the existing orphan sweep.

"Least recently activated" is derived, not stored: it is the profile whose slots have the smallest maximum `touched_ms`. This adds no field and no clock — the same reasoning `SlotStore`'s orphan ordering already uses.

- [ ] **Step 1: Write the failing tests**

```rust
    /// Pasado el tope, el estado del perfil que hace más que nadie activa se
    /// va ENTERO. Su directorio de configuración no se toca: el perfil sigue
    /// existiendo y arranca de su `[profile.start]`.
    #[test]
    fn pasado_el_tope_se_va_el_perfil_mas_viejo() {
        let mut b = cuerpo_con_perfiles(&[("a", 10), ("b", 20), ("c", 30), ("d", 40), ("e", 50)]);
        b.active = "e".to_owned();
        b.prune(100);
        assert!(!b.layouts.contains_key("a"), "el más viejo se va");
        assert_eq!(b.layouts.len(), PROFILE_STATE_CAP);
    }

    /// El ACTIVO no lo barre nada, en ningún paso, ni siendo el más viejo.
    #[test]
    fn el_activo_no_se_barre_aunque_sea_el_mas_viejo() {
        let mut b = cuerpo_con_perfiles(&[("a", 10), ("b", 20), ("c", 30), ("d", 40), ("e", 50)]);
        b.active = "a".to_owned();
        b.prune(100);
        assert!(b.layouts.contains_key("a"), "el activo se queda");
        assert_eq!(b.layouts.len(), PROFILE_STATE_CAP);
    }

    /// Y tirar un perfil se lleva SUS huecos, no los de otro.
    #[test]
    fn tirar_un_perfil_se_lleva_solo_sus_huecos() {
        let mut b = cuerpo_con_perfiles(&[("a", 10), ("b", 20), ("c", 30), ("d", 40), ("e", 50)]);
        b.active = "e".to_owned();
        let de_b: Vec<u32> = b.layouts["b"].slot_ids().iter().map(|s| s.0).collect();
        b.prune(100);
        for id in de_b {
            assert!(b.slots.contains_key(&id), "el hueco {id} de «b» sigue ahí");
        }
    }

    /// Un cuerpo realista con el tope lleno cabe en SESSION_BODY_MAX. Si este
    /// test se pone rojo, la cura es BAJAR `PROFILE_STATE_CAP`, no subir el
    /// tope del protocolo: el core rehúsa un `put` que se pase y deja la
    /// sesión como estaba, así que pasarse es perder lo que estabas haciendo.
    #[test]
    fn un_cuerpo_realista_con_el_tope_lleno_cabe_en_el_sobre() {
        let mut b = cuerpo_realista_al_tope();
        b.prune(0);
        let bytes = serde_json::to_vec(&b.to_value()).expect("serializa");
        assert!(
            bytes.len() <= norte_proto::methods::SESSION_BODY_MAX,
            "{} bytes contra un tope de {}",
            bytes.len(),
            norte_proto::methods::SESSION_BODY_MAX
        );
    }
```

Write `cuerpo_con_perfiles(&[(nombre, touched_ms)])` and `cuerpo_realista_al_tope()` in the same `mod tests`. The realistic body is `PROFILE_STATE_CAP` profiles, each with an eight-slot arrangement, each slot carrying `HISTORY_CAP` entries in both directions, with paths of about 80 bytes (`file:///home/u/src/norte/crates/norte-frontend/src/<n>` — a real path from this repository, not a padded string, because a padded string measures the padding).

- [ ] **Step 2: Run and watch them fail**

Run: `just t norte-frontend`
Expected: FAIL to compile — `PROFILE_STATE_CAP` does not exist.

- [ ] **Step 3: Implement**

Add the constant with a rustdoc paragraph saying where the number came from (the test above), and put the profile sweep in `prune` **before** the orphan age sweep, so the orphan pass sees a smaller map.

- [ ] **Step 4: Run the tests**

Run: `just t norte-frontend`
Expected: PASS, all four. If the size test fails, lower `PROFILE_STATE_CAP` by one and re-run until it passes; record the final number and the measured byte count in the constant's rustdoc.

- [ ] **Step 5: File what this measurement turned up**

The existing `ORPHAN_CAP` is 128 slots, and 128 slots each holding 128 history entries of 80 bytes is already about 1.3 MB — over `SESSION_BODY_MAX` before this feature existed. Confirm it with a quick calculation against the numbers your test printed, and if it holds, open an issue titled `[frontend] ORPHAN_CAP can exceed SESSION_BODY_MAX on its own` describing the arithmetic. Do not fix it here: it is not this branch's bug, and folding it in would make this PR two changes.

- [ ] **Step 6: Commit**

```bash
git add -A
git diff --cached --stat
git commit -m "feat(frontend): el estado por perfil tiene tope, y el activo no se barre"
```

---

## Task 9: A rebind made under a profile is written into that profile

Implements D10.

**Files:**
- Modify: `crates/norte-frontend/src/keymap/rebind.rs:435` (`RebindSources::split_at`)
- Test: inline `mod tests` in `rebind.rs`

**Interfaces:**
- Consumes: `Layer::Profile` (Task 1).
- Produces: no signature change. `split_at` keeps taking `kinds: &[Layer]`; only the accepted order widens, from `System`\* `User`? to `System`\* `User`? `Profile`?.

- [ ] **Step 1: Write the failing tests**

```rust
    /// Con un perfil activo, el atajo se escribe EN EL PERFIL. Escribirlo en
    /// la capa de usuario lo dejaría tapado por el keymap del perfil, y el
    /// lector vería una rebind que no hace nada.
    #[test]
    fn con_perfil_activo_el_destino_es_el_perfil() {
        let preset = parse_keymap("[pane]\nkeymap = [{ on = [\"f5\"], run = \"pane.copy\" }]\n")
            .expect("preset");
        let usuario =
            parse_keymap_layer("[pane]\nprepend_keymap = [{ on = [\"f6\"], run = \"pane.move\" }]\n")
                .expect("usuario");
        let perfil = KeymapFile::default();
        let layers = [usuario, perfil];
        let kinds = [Layer::User, Layer::Profile];
        let known = ["pane.copy", "pane.move"];
        let split = RebindSources::split_at(&preset, &kinds, &layers, &known, Screen::Browse);
        assert!(
            split.above.is_empty(),
            "nada por encima del destino: el perfil ES el destino"
        );
    }

    /// Sin perfil, el destino sigue siendo el usuario. Esta tarea no puede
    /// cambiar lo que ya funcionaba.
    #[test]
    fn sin_perfil_el_destino_sigue_siendo_el_usuario() {
        let preset = parse_keymap("[pane]\nkeymap = [{ on = [\"f5\"], run = \"pane.copy\" }]\n")
            .expect("preset");
        let sistema =
            parse_keymap_layer("[pane]\nprepend_keymap = [{ on = [\"f5\"], run = \"pane.copy\" }]\n")
                .expect("sistema");
        let layers = [sistema];
        let kinds = [Layer::System];
        let known = ["pane.copy", "pane.move"];
        let split = RebindSources::split_at(&preset, &kinds, &layers, &known, Screen::Browse);
        let w = rebind_dry_run(&split.sources(), &[parse_chord("f5").expect("chord")], "pane.move")
            .expect("escribe");
        assert_eq!((w.section, &w.chords[..]), ("pane", &["f5".to_owned()][..]));
    }

    /// Y cualquier OTRO orden sigue cayendo en `above`, que es el lado
    /// fail-closed: la puerta rehúsa lo que no sabe modelar, nunca aprueba lo
    /// que no aprobó.
    #[test]
    fn un_orden_inesperado_sigue_siendo_fail_closed() {
        let preset = parse_keymap("[pane]\nkeymap = [{ on = [\"f5\"], run = \"pane.copy\" }]\n")
            .expect("preset");
        let layers = [KeymapFile::default(), KeymapFile::default()];
        let kinds = [Layer::Project, Layer::User];
        let known = ["pane.copy"];
        let split = RebindSources::split_at(&preset, &kinds, &layers, &known, Screen::Browse);
        assert_eq!(split.above.len(), 2, "todo por encima; no se escribe nada");
    }
```

- [ ] **Step 2: Run and watch the first one fail**

Run: `just t norte-frontend`
Expected: the first test FAILS — the current cut stops after `User`, so the `Profile` layer lands in `above` and `above.is_empty()` is false. The other two should already pass; if the third does not, stop and say so, because it means the fail-closed property was already weaker than its rustdoc claims.

- [ ] **Step 3: Implement**

After the existing `System` run and the optional `User` step, accept one optional `Profile`. The target is the **last** of the two that is present; `above_from` moves past it. Update the rustdoc paragraph that describes the accepted order — it is a contract, and leaving it describing the old cut makes the next reader trust the wrong thing.

- [ ] **Step 4: Run the tests**

Run: `just t norte-frontend`
Expected: PASS, all three, plus the existing rebind suite unchanged.

- [ ] **Step 5: Docs**

Run: `cargo test -p norte-frontend --doc` — Expected: PASS. This file's rustdoc carries a runnable example with `let kinds = [Layer::System];`; if your edit changed the described contract, the example changes with it.

- [ ] **Step 6: Commit**

```bash
git add -A
git diff --cached --stat
git commit -m "feat(frontend): un atajo cambiado con un perfil activo se escribe en el perfil"
```

- [ ] **Step 7: Spend the second gate run**

Run: `just ci-fast`
Expected: PASS.

- [ ] **Step 8: Review**

Dispatch `rust-reviewer` over `HEAD~4..HEAD`. The questions worth asking: whether `rebase_slot_ids` can produce a collision with a slot held only in `slots` and mentioned by no arrangement, and whether the profile sweep in `prune` can drop a slot that a *different* profile's arrangement mentions. Apply BLOCKER and MAJOR in one pass.

---

## What P1 and P2 deliberately leave undone

After Task 9 the mechanism is complete and nothing uses it. There is no way to pick a profile, no `--profile` flag, and `standard_layers_with_profile` is called by no binary. That is the intended end state: the layer and the state are testable without a frontend, and a frontend built on top of an untested layer is two unknowns at once.

The remaining phases, at task granularity, to be planned once P1 has fixed the real signatures:

**P3 — the TUI.** The pure `ProfilePicker` in `norte-frontend` (a sibling of `layout_picker.rs`, same row/preview/warning shape); `profile.pick`, `profile.next`, `profile.prev`, `profile.save-as` in the shared catalogue, unbound in every preset; `--profile <name>` through `norte-frontend/src/cli.rs`; the switch sequence of D8 with the flush-first ordering; and the measurement that produces the list of settings a hot switch cannot apply, plus the test that fails when a new `[ui]` scalar is added without being classified. Reading `profiles/` happens off the event loop (#244).

**P4 — the window.** The picker's DTO in `norte-ui-host`, the renderer, a second bridge bump, and the golden corpus entry (#257 is the precedent for a corpus checked against a hand-written list and missing variants). Remember that `just ci-fast` does not run `gui-ci`.

**P5 — creating one, and checking it.** "Save the current workspace as a profile" over the `toml_edit` writer `settings.rs` already has; `norte doctor` validation of every profile directory; help topics — which means regenerating the `norte-cli` golden with `NORTE_UPDATE_GOLDEN=1` — and Fluent keys in both locales.

**ADR 0079** is written at the close of the branch, not now: it records the structure of configuration and its precedence, and the honest version of that is the one that survived the implementation.
