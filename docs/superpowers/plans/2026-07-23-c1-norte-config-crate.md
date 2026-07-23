# C1 — `norte-config` crate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One config-dir resolver, one layered loader, one strict `norte.toml` parser, shared by TUI/GUI/CLI/daemon via a new `norte-config` crate.

**Architecture:** `norte-config` (no UI deps, no core dep) owns dirs/layers/schema/scalar-merge/watch/persist. `norte-frontend` gains a `config` module that adds the frontend-only passes (keymap layers, openers) on top. `norte-core` re-exports the resolver and consumes typed `[archive]`/`[ai]` sections layered. Spec: `docs/superpowers/specs/2026-07-23-help-config-system-design.md`, phase C1.

**Tech Stack:** Rust workspace crate; serde/toml/toml_edit; notify+tokio behind a `watch` feature; schemars behind a `schema` feature.

**Key facts discovered during planning (read before executing):**

- The three divergent resolvers: `norte-core/src/connect.rs:126-143` (honors `NORTE_CONFIG_DIR`), `norte-tui/src/config.rs:208-241` (does NOT), `norte-gui/src/keymap.rs:63-82` (no Windows branch).
- **Latent bug:** `norte-tui`'s `NorteToml` is `deny_unknown_fields` and has NO `ai` field (`config.rs:24-47`), while ADR 0031 documents `[ai]` in the same `norte.toml`. A user who configures AI breaks TUI startup. C1 fixes this by adding `AiSection` to the canonical strict struct.
- `nav::Mode`, keymap engine, and `OpenersConfig` live in `norte-frontend` (no core dep, no tokio) — so the combined frontend loader goes there, sync-only; the TUI keeps its tiny `load_async` wrapper.
- Schema goldens: `crates/norte-tui/tests/schema.rs` regenerates with `NORTE_UPDATE_SCHEMA=1`; `NorteToml` gaining `[ai]` changes `docs/schema/norte.schema.json`.
- GUI is excluded from the workspace; build it with `cd crates/norte-gui && cargo check`.
- GUI's layer loading also lacks the `has_full_keymap()` rejection the TUI applies (`norte-tui/src/config.rs:479-486`) — reusing the shared loader fixes that drift for free.

**Decisions locked in (go in the ADR, Task 1):**

1. Resolver precedence (everywhere): `NORTE_CONFIG_DIR` → `XDG_CONFIG_HOME/norte` (non-empty) → Windows `%APPDATA%\norte` → `$HOME/.config/norte`.
2. `NORTE_CONFIG_DIR` set ⇒ **hermetic**: `standard_layers()` returns only `(that dir, User)` + `(./.norte, Project)`. No `/etc/norte`. Rationale: the variable exists for tests/headless isolation; leaking system config under an explicit override is a footgun.
3. Canonical strict `NorteToml` gains `[ai]` (fixes the latent bug). `[ai]` honored from System+User, never Project (same carve-out as `[archive]`). Merge: scalars last-present-wins; `denied_prefixes` **union across layers** (a deny never disappears by adding a layer); `providers` merge by name, later layer wins per name.
4. Uniform strictness: daemon/CLI now parse the full strict `NorteToml` — a `[ui]` typo fails daemon startup exactly as it fails the TUI (ADR 0007: invalid configuration is a startup error).
5. `policy.toml` stays single-file user-layer (deliberate; documented, not changed).
6. Crate license `MIT OR Apache-2.0` (shared-lib pattern, like `norte-frontend`).

---

### Task 1: ADR 0035

**Files:**
- Create: `docs/adr/0035-norte-config-crate.md`

- [ ] **Step 1: Write the ADR** using the `/adr` project skill if available; otherwise copy the MADR format of `docs/adr/0034-index-fts5.md`. Title: "Shared norte-config crate and unified configuration resolution". Content: the six decisions above, plus context (three divergent resolvers with file refs; the `[ai]`-breaks-TUI bug; ADR 0007's relocation clause now triggered by GUI/CLI/daemon all needing the loader) and consequences (core gains a dep on `norte-config`; `NORTE_CONFIG_DIR` becomes hermetic — behavior change for anyone relying on `/etc/norte` merging under the override; daemon startup becomes strict about the whole file).

- [ ] **Step 2: Commit**

```bash
git add docs/adr/0035-norte-config-crate.md
git commit -m "docs(adr): 0035 shared norte-config crate + unified resolution"
```

### Task 2: Scaffold `crates/norte-config`

**Files:**
- Modify: `Cargo.toml` (workspace members + workspace deps)
- Create: `crates/norte-config/Cargo.toml`, `crates/norte-config/src/lib.rs`

- [ ] **Step 1: Add workspace member and dep.** In root `Cargo.toml`: add `"crates/norte-config"` to `members` (after `norte-connect`), and to `[workspace.dependencies]` (alphabetical, next to `norte-frontend`):

```toml
norte-config = { path = "crates/norte-config", version = "0.3.0-alpha.1" }
```

- [ ] **Step 2: Crate manifest.** `crates/norte-config/Cargo.toml`:

```toml
[package]
name = "norte-config"
description = "Layered configuration for norte (ADR 0007/0035): shared dir resolution, strict norte.toml schema, scalar merge, watch"
license = "MIT OR Apache-2.0"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
repository.workspace = true
authors.workspace = true

[dependencies]
norte-proto.workspace = true
serde = { workspace = true, features = ["derive"] }
thiserror.workspace = true
toml.workspace = true
toml_edit.workspace = true
schemars = { workspace = true, features = ["derive"], optional = true }
notify = { workspace = true, optional = true }
tokio = { workspace = true, features = ["rt", "time", "sync"], optional = true }
tokio-util = { workspace = true, optional = true }

[dev-dependencies]
tempfile.workspace = true
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "test-util"] }

[features]
# JSON Schema generation for docs/schema goldens (ADR 0007).
schema = ["dep:schemars"]
# Live config watching (notify + polling fallback). Frontends enable it;
# core/CLI do not need it.
watch = ["dep:notify", "dep:tokio", "dep:tokio-util"]

[lints]
workspace = true
```

- [ ] **Step 3: Minimal lib.rs** (module docs + module decls added as tasks land):

```rust
//! Layered configuration (ADR 0007, ADR 0035): the single config-dir
//! resolver, the strict `norte.toml` schema, scalar merge across layers,
//! persistence helpers, and (feature `watch`) live reload plumbing.
//!
//! This crate deliberately reads configuration with `std::fs`; using
//! providers would be circular because configuration selects how a frontend
//! starts. It depends on `norte-proto` only (for `VPath`) — never on the
//! core or a frontend.

pub mod dirs;

pub use dirs::{Layer, Layers, config_dir, standard_layers, user_config_dir};
```

- [ ] **Step 4: Verify it builds**

Run: `cargo check -p norte-config` (create an empty `src/dirs.rs` with `//! Config directory resolution.` first so it compiles)
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/norte-config
git commit -m "feat(config): scaffold norte-config crate (ADR 0035)"
```

### Task 3: `dirs` module — the one resolver

**Files:**
- Modify: `crates/norte-config/src/dirs.rs`

Env-reading wrappers stay thin; logic lives in injectable `_from` functions so tests never mutate the environment (the workspace forbids `unsafe`, and `env::set_var` is unsafe in edition 2024).

- [ ] **Step 1: Write failing tests** at the bottom of `src/dirs.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn env(v: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + '_ {
        move |k| v.iter().find(|(n, _)| *n == k).map(|(_, x)| OsString::from(x))
    }

    #[test]
    fn norte_config_dir_wins_over_everything() {
        let d = user_config_dir_from(&env(&[
            ("NORTE_CONFIG_DIR", "/custom"),
            ("XDG_CONFIG_HOME", "/xdg"),
            ("HOME", "/home/u"),
        ]));
        assert_eq!(d, Some(PathBuf::from("/custom")));
    }

    #[test]
    fn xdg_empty_falls_through_to_home() {
        let d = user_config_dir_from(&env(&[("XDG_CONFIG_HOME", ""), ("HOME", "/home/u")]));
        assert_eq!(d, Some(PathBuf::from("/home/u/.config/norte")));
    }

    #[test]
    fn xdg_beats_home() {
        let d = user_config_dir_from(&env(&[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/home/u")]));
        assert_eq!(d, Some(PathBuf::from("/xdg/norte")));
    }

    #[test]
    fn sin_entorno_es_none() {
        assert_eq!(user_config_dir_from(&env(&[])), None);
    }

    /// ADR 0035 decision 2: an explicit override is HERMETIC — no system
    /// layer, only (override, User) + (./.norte, Project).
    #[test]
    fn standard_layers_con_override_es_hermetico() {
        let l = standard_layers_from(&env(&[("NORTE_CONFIG_DIR", "/custom"), ("HOME", "/home/u")]));
        assert_eq!(
            l.dirs,
            vec![
                (PathBuf::from("/custom"), Layer::User),
                (PathBuf::from(".norte"), Layer::Project),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn standard_layers_sin_override_incluye_sistema() {
        let l = standard_layers_from(&env(&[("HOME", "/home/u")]));
        assert_eq!(
            l.dirs,
            vec![
                (PathBuf::from("/etc/norte"), Layer::System),
                (PathBuf::from("/home/u/.config/norte"), Layer::User),
                (PathBuf::from(".norte"), Layer::Project),
            ]
        );
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p norte-config`
Expected: FAIL to compile — `user_config_dir_from` etc. not defined.

- [ ] **Step 3: Implement.** Move `Layer` and `Layers` verbatim from `crates/norte-tui/src/config.rs:178-202` (keep their rustdoc, including the debt-#75 note). Then:

```rust
use std::ffi::OsString;
use std::path::PathBuf;

/// The user config dir, resolved from an injectable environment (tests pass
/// a closure; production wrappers pass [`std::env::var_os`]). Precedence
/// (ADR 0035): `NORTE_CONFIG_DIR` → `XDG_CONFIG_HOME/norte` (non-empty) →
/// `%APPDATA%\norte` (Windows) → `$HOME/.config/norte`.
pub fn user_config_dir_from(get: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    if let Some(d) = get("NORTE_CONFIG_DIR") {
        return Some(PathBuf::from(d));
    }
    if let Some(d) = get("XDG_CONFIG_HOME")
        && !d.is_empty()
    {
        return Some(PathBuf::from(d).join("norte"));
    }
    if cfg!(windows)
        && let Some(d) = get("APPDATA")
    {
        return Some(PathBuf::from(d).join("norte"));
    }
    get("HOME").map(|h| PathBuf::from(h).join(".config").join("norte"))
}

/// The user config dir from the process environment; `None` when the
/// environment defines nothing (CI without HOME): the caller warns.
#[must_use]
pub fn user_config_dir() -> Option<PathBuf> {
    user_config_dir_from(&|k| std::env::var_os(k))
}

/// Infallible variant for core paths (`connections.toml`, `journal.db`, …):
/// falls back to `./.config/norte` like the historic
/// `norte_core::connect::config_dir` did.
#[must_use]
pub fn config_dir() -> PathBuf {
    user_config_dir().unwrap_or_else(|| PathBuf::from(".").join(".config").join("norte"))
}

/// Standard layers (ADR 0007/0035) from an injectable environment.
/// `NORTE_CONFIG_DIR` set ⇒ hermetic: only that dir (User) + `./.norte`.
pub fn standard_layers_from(get: &dyn Fn(&str) -> Option<OsString>) -> Layers {
    let mut dirs = Vec::new();
    if let Some(over) = get("NORTE_CONFIG_DIR") {
        dirs.push((PathBuf::from(over), Layer::User));
        dirs.push((PathBuf::from(".norte"), Layer::Project));
        return Layers { dirs };
    }
    if cfg!(windows) {
        if let Some(pd) = get("ProgramData") {
            dirs.push((PathBuf::from(pd).join("norte"), Layer::System));
        }
    } else {
        dirs.push((PathBuf::from("/etc/norte"), Layer::System));
    }
    if let Some(user) = user_config_dir_from(get) {
        dirs.push((user, Layer::User));
    }
    dirs.push((PathBuf::from(".norte"), Layer::Project));
    Layers { dirs }
}

/// Standard layers from the process environment.
#[must_use]
pub fn standard_layers() -> Layers {
    standard_layers_from(&|k| std::env::var_os(k))
}
```

Note vs the old TUI code: the Windows User layer now comes from `user_config_dir_from` (so `XDG_CONFIG_HOME` works on Windows too, matching the core resolver), and the historic `home_dir()` call is replaced by the `HOME` lookup — same result on unix, injectable in tests.

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p norte-config`
Expected: PASS (all 6)

- [ ] **Step 5: Commit**

```bash
git add crates/norte-config/src/dirs.rs crates/norte-config/src/lib.rs
git commit -m "feat(config): unified config-dir resolver + hermetic NORTE_CONFIG_DIR"
```

### Task 4: `schema` module — one strict `NorteToml`, now with `[ai]`

**Files:**
- Create: `crates/norte-config/src/schema.rs`
- Modify: `crates/norte-config/src/lib.rs`

- [ ] **Step 1: Write the failing test** (in `schema.rs`'s test mod):

```rust
/// The latent bug this task fixes: the strict struct must accept `[ai]`
/// (ADR 0031 documents it in norte.toml; the old TUI struct rejected it).
#[test]
fn norte_toml_estricto_acepta_seccion_ai() {
    let doc = r#"
[ui]
theme = "nord"

[ai]
enabled = true
local_only = true
denied_prefixes = ["file:///secret"]
rename_provider = "local"

[ai.providers.local]
kind = "ollama"
model = "llama3"
"#;
    let parsed: NorteToml = toml::from_str(doc).expect("[ai] es sección canónica");
    assert!(parsed.ai.enabled);
    assert_eq!(parsed.ai.providers.len(), 1);
}

/// Strictness is uniform: a typo anywhere is a hard error.
#[test]
fn campo_desconocido_sigue_siendo_error() {
    assert!(toml::from_str::<NorteToml>("[ui]\ntheem = \"nord\"\n").is_err());
}
```

- [ ] **Step 2: Verify failure**

Run: `cargo nextest run -p norte-config schema`
Expected: FAIL to compile — `NorteToml` not defined here yet.

- [ ] **Step 3: Move + extend.** Move verbatim from `crates/norte-tui/src/config.rs` into `schema.rs`: `NorteToml` (lines 23-47), `HotlistEntry` (49-63), `ArchiveSection` (65-81), `DaemonSection`/`DaemonMode` (83-108), `UiSection` (110-130), `KeymapSection` (132-140), `ConfigError` (142-161), `toml_diag` (163-176, make it `pub(crate)`), `read_optional` (646-657, make it `pub(crate)`), `DEFAULT_PRESET` (20-21), and the `toml_diag_tests` mod (811-836). Then add the `[ai]` section to `NorteToml`:

```rust
    /// AI subsystem settings (`[ai]`, ADR 0031/0035). Honored from
    /// System+User layers only — never Project (fail-closed, same carve-out
    /// as `[archive]`).
    #[serde(default)]
    pub ai: AiSection,
```

and define (strict, unlike the old tolerant scraper in `norte-core/src/ai.rs:60-81`):

```rust
/// The `[ai]` section of `norte.toml` (ADR 0031). All off by default.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AiSection {
    /// AI enabled. `false` (default) = the gate rejects every operation.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Local-only mode: reject remote providers (spec §9).
    #[serde(default)]
    pub local_only: Option<bool>,
    /// Prefixes whose content/names never leave the process. Wire strings;
    /// validated to `VPath` during merge ([`crate::load`]).
    #[serde(default)]
    pub denied_prefixes: Vec<String>,
    /// Provider name used for AI rename.
    #[serde(default)]
    pub rename_provider: Option<String>,
    /// Declared providers (`[ai.providers.<name>]`).
    #[serde(default)]
    pub providers: std::collections::BTreeMap<String, AiProviderEntry>,
}

/// One `[ai.providers.<name>]` entry.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AiProviderEntry {
    /// `anthropic` | `ollama` | `openai-compat`.
    pub kind: String,
    /// Model id as the provider expects it.
    pub model: String,
    /// Base URL (required for `openai-compat`).
    #[serde(default)]
    pub base_url: Option<String>,
}
```

(`enabled`/`local_only` become `Option<bool>` so layer merge can distinguish "absent" from "false" — ADR 0007 last-present-wins.)

Update `lib.rs`: `pub mod schema;` + re-export `schema::{AiProviderEntry, AiSection, ArchiveSection, ConfigError, DaemonMode, DaemonSection, DEFAULT_PRESET, HotlistEntry, KeymapSection, NorteToml, UiSection}`.

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p norte-config`
Expected: PASS (schema tests + relocated toml_diag tests + dirs tests)

- [ ] **Step 5: Commit**

```bash
git add crates/norte-config/src
git commit -m "feat(config): canonical strict NorteToml — adds [ai] (fixes latent TUI reject)"
```

### Task 5: `load` module — scalar merge + persist

**Files:**
- Create: `crates/norte-config/src/load.rs`
- Modify: `crates/norte-config/src/lib.rs`

- [ ] **Step 1: Move the merge core.** From `crates/norte-tui/src/config.rs` move into `load.rs`: `HotlistItem` + `ERR_INVALID_PATH` + `merge_hotlist_entry` (379-420), `persist_ui_theme`/`persist_ui_theme_to` (243-287), `persist_hotlist_add` (289-336), `persist_hotlist_remove` (338-377), and the `load` function body (517-630) REWRITTEN as `load` returning `CommonConfig` — same logic minus the keymap/openers passes (those move to `norte-frontend` in Task 6) and with quick_search validated into a local enum plus the new `[ai]` merge:

```rust
/// Quick-search behaviour of `/` (`[ui] quick_search`). The frontend maps
/// this onto its own navigation mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QuickSearch {
    /// Narrow the listing (default).
    #[default]
    Filter,
    /// Move the cursor without changing the listing.
    Jump,
}

/// `[ai]` already merged across layers and validated (ADR 0035 decision 3:
/// scalars last-present-wins; `denied_prefixes` union; providers merge by
/// name, later layer wins).
#[derive(Debug, Clone, Default)]
pub struct AiSettings {
    /// AI enabled (default false).
    pub enabled: bool,
    /// Local-only mode (default false).
    pub local_only: bool,
    /// Validated denied prefixes (union of all non-project layers).
    pub denied_prefixes: Vec<norte_proto::VPath>,
    /// Provider selected for rename.
    pub rename_provider: Option<String>,
    /// Providers by name (BTreeMap keeps deterministic order).
    pub providers: std::collections::BTreeMap<String, crate::schema::AiProviderEntry>,
}

/// The merged `norte.toml` scalars — everything that is NOT a frontend-only
/// pass (keymap layers, openers). Core consumers read `archive_*`/`ai`;
/// frontends wrap this in their own loaded-config type.
#[derive(Debug, Clone)]
pub struct CommonConfig {
    /// Effective keymap preset (last-wins; compiled default).
    pub preset: String,
    /// `[ui] lang` (last-wins; None = environment).
    pub ui_lang: Option<String>,
    /// `[ui] theme` (last-wins; None = default preset).
    pub ui_theme: Option<String>,
    /// `[ui] quick_search`, validated (invalid value = load error).
    pub quick_search: QuickSearch,
    /// `[daemon] mode` (last-wins; None = embedded). Startup only.
    pub daemon_mode: Option<crate::schema::DaemonMode>,
    /// `[daemon] socket` (last-wins; None = OS default).
    pub daemon_socket: Option<std::path::PathBuf>,
    /// Hotlist merged from every layer except Project.
    pub hotlist: Vec<HotlistItem>,
    /// `[archive]` limits (last-wins per field; never from Project).
    pub archive_max_entries: Option<u64>,
    /// `[archive] max_decompressed_bytes`.
    pub archive_max_decompressed_bytes: Option<u64>,
    /// `[archive] max_nesting` (#56).
    pub archive_max_nesting: Option<usize>,
    /// `[ai]` merged (never from Project).
    pub ai: AiSettings,
    /// Files that participated (watcher + diagnostics).
    pub sources: Vec<std::path::PathBuf>,
}
```

`load(layers: &Layers) -> Result<CommonConfig, ConfigError>` keeps the existing per-layer walk of `norte.toml` (`read_optional` → strict parse with `toml_diag` → scalar last-wins; hotlist and `[archive]` skip `Layer::Project` exactly as today, comments preserved). New `[ai]` merge inside the same loop, also gated `if *kind != Layer::Project`:

```rust
            if *kind != Layer::Project {
                let a = parsed.ai;
                if let Some(v) = a.enabled {
                    ai.enabled = v;
                }
                if let Some(v) = a.local_only {
                    ai.local_only = v;
                }
                if let Some(v) = a.rename_provider {
                    ai.rename_provider = Some(v);
                }
                for p in &a.denied_prefixes {
                    let vp = norte_proto::VPath::parse(p).map_err(|_| ConfigError::Toml {
                        path: norte.clone(),
                        // Same #73 caution as quick_search: never quote the
                        // raw (possibly hostile) value in the diagnostic.
                        message: "[ai] denied_prefixes: entry does not parse as a VPath".to_owned(),
                    })?;
                    if !ai.denied_prefixes.contains(&vp) {
                        ai.denied_prefixes.push(vp);
                    }
                }
                ai.providers.extend(a.providers);
            }
```

The quick_search arm returns `QuickSearch::Filter`/`Jump` instead of `nav::Mode` (keep the exact error message and #73 comment from `config.rs:551-567`).

- [ ] **Step 2: Move the tests.** Relocate the whole `hotlist_tests` mod (`config.rs:841-1189`) EXCEPT the two openers tests (`openers_de_proyecto_se_ignoran_usuario_se_honra`, `openers_usuario_gana_sobre_sistema` — those move in Task 6). Mechanical adaptations: `load(...)` now returns `CommonConfig`; `crate::nav::Mode::Jump` assertions become `QuickSearch::Jump`/`QuickSearch::Filter`. Add two new tests:

```rust
    /// ADR 0035: [ai] merges across layers — scalars last-wins, denied
    /// prefixes UNION (a system deny survives a user layer), providers
    /// merge by name.
    #[test]
    fn ai_merge_escalares_ultimo_gana_y_denied_union() {
        let sistema = tempfile::tempdir().unwrap();
        std::fs::write(
            sistema.path().join("norte.toml"),
            "[ai]\nenabled = true\nlocal_only = true\ndenied_prefixes = [\"file:///etc\"]\n",
        )
        .unwrap();
        let usuario = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[ai]\nlocal_only = false\ndenied_prefixes = [\"file:///home/u/secret\"]\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (sistema.path().to_path_buf(), Layer::System),
                (usuario.path().to_path_buf(), Layer::User),
            ],
        };
        let cfg = load(&layers).expect("carga");
        assert!(cfg.ai.enabled, "absent in user layer: inherits system");
        assert!(!cfg.ai.local_only, "present in user layer: user wins");
        assert_eq!(cfg.ai.denied_prefixes.len(), 2, "denies UNION, never shrink");
    }

    /// [ai] from the project layer is ignored fail-closed — a hostile repo
    /// must not enable AI nor redirect providers.
    #[test]
    fn ai_de_proyecto_se_ignora() {
        let proyecto = tempfile::tempdir().unwrap();
        std::fs::write(proyecto.path().join("norte.toml"), "[ai]\nenabled = true\n").unwrap();
        let layers = Layers {
            dirs: vec![(proyecto.path().to_path_buf(), Layer::Project)],
        };
        let cfg = load(&layers).expect("carga");
        assert!(!cfg.ai.enabled);
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p norte-config`
Expected: PASS

- [ ] **Step 4: Update lib.rs re-exports** — `pub mod load;` + `pub use load::{load, AiSettings, CommonConfig, HotlistItem, QuickSearch, persist_hotlist_add, persist_hotlist_remove, persist_ui_theme, persist_ui_theme_to};`

- [ ] **Step 5: Commit**

```bash
git add crates/norte-config/src
git commit -m "feat(config): CommonConfig scalar merge incl layered [ai] + persist helpers"
```

### Task 6: watch module (feature `watch`) + `norte-frontend::config`

**Files:**
- Create: `crates/norte-config/src/watch.rs`, `crates/norte-frontend/src/config.rs`
- Modify: `crates/norte-config/src/lib.rs`, `crates/norte-frontend/src/lib.rs`, `crates/norte-frontend/Cargo.toml`

- [ ] **Step 1: Move the watcher.** Move verbatim from `crates/norte-tui/src/config.rs` into `crates/norte-config/src/watch.rs`: `WatchMode` (659-666), `Watch` + `Drop` (668-682), `watch` (684-741), `watch_polling` (743-762), `spawn_poll` (764-791), `snapshot` (793-809). One fix while moving (pre-existing gap): `snapshot` polls only `norte.toml`/`keymap.toml` — add `"openers.toml"` to the array so the polling fallback catches opener edits too (the native watcher already watches the whole dir). In `lib.rs`:

```rust
#[cfg(feature = "watch")]
pub mod watch;
#[cfg(feature = "watch")]
pub use watch::{Watch, WatchMode, watch, watch_polling};
```

Run: `cargo check -p norte-config --features watch`
Expected: PASS

- [ ] **Step 2: Add the frontend combined loader.** `crates/norte-frontend/Cargo.toml` gains `norte-config.workspace = true` (no features). New `crates/norte-frontend/src/config.rs`; move `load_keymap_layer` (`norte-tui/src/config.rs:456-489`) and `load_openers` (491-515) here (their bodies unchanged; they call `norte_config::schema` helpers — make `read_optional` reachable by re-exporting it `pub` from `norte-config`'s schema module with `#[doc(hidden)]`), plus:

```rust
//! Frontend configuration: the shared scalar merge (`norte-config`) plus the
//! frontend-only passes — `keymap.toml` layers and `openers.toml` (#28).

use norte_config::{CommonConfig, ConfigError, Layer, Layers, QuickSearch};

use crate::keymap::{KeymapFile, parse_keymap};
use crate::nav;
use crate::openers::OpenersConfig;

/// Everything a frontend needs, flat (same shape the TUI historically used).
#[derive(Debug, Clone)]
pub struct FrontendConfig {
    /// The merged scalars (preset, ui, daemon, hotlist, archive, ai, sources).
    pub common: CommonConfig,
    /// `keymap.toml` layers present, ascending precedence.
    pub keymap_layers: Vec<KeymapFile>,
    /// Quick-search mode mapped onto the navigation enum.
    pub quick_search_mode: nav::Mode,
    /// Merged declarative openers (#28): System/User only, fail-closed.
    pub openers: OpenersConfig,
}

/// Load and merge every layer (ADR 0007): common scalars + keymap + openers.
///
/// # Errors
/// [`ConfigError`] with the culprit file; an absent layer is not an error.
pub fn load(layers: &Layers) -> Result<FrontendConfig, ConfigError> {
    let mut common = norte_config::load(layers)?;
    let mut keymap_layers = Vec::new();
    let mut openers = OpenersConfig::empty();
    for (dir, kind) in &layers.dirs {
        if let Some(parsed) = load_keymap_layer(dir, *kind, &mut common.sources)? {
            keymap_layers.push(parsed);
        }
        if let Some(parsed) = load_openers(dir, *kind, &mut common.sources)? {
            openers.extend_front(parsed);
        }
    }
    let quick_search_mode = match common.quick_search {
        QuickSearch::Filter => nav::Mode::Filter,
        QuickSearch::Jump => nav::Mode::Jump,
    };
    Ok(FrontendConfig {
        common,
        keymap_layers,
        quick_search_mode,
        openers,
    })
}
```

`crates/norte-frontend/src/lib.rs` gains `pub mod config;`. Move the two openers tests from the TUI (`openers_de_proyecto_se_ignoran_usuario_se_honra`, `openers_usuario_gana_sobre_sistema`, `norte-tui/src/config.rs:1039-1104`) into this module's test mod, calling `config::load` and asserting on `cfg.openers`.

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p norte-frontend`
Expected: PASS (2 relocated openers tests + existing suite)

- [ ] **Step 4: Commit**

```bash
git add crates/norte-config crates/norte-frontend
git commit -m "feat(config,frontend): watch feature + FrontendConfig combined loader"
```

### Task 7: TUI migration

**Files:**
- Modify: `crates/norte-tui/src/config.rs` (becomes a shim), `crates/norte-tui/src/main.rs`, `crates/norte-tui/Cargo.toml`, `crates/norte-tui/src/lua.rs` (Layer re-export source)

- [ ] **Step 1: Rewrite `crates/norte-tui/src/config.rs` as a shim** (delete everything moved in Tasks 3-6; the file becomes):

```rust
//! Layered configuration — relocated to `norte-config` + `norte-frontend`
//! (ADR 0035; the ADR 0007 "until another frontend needs it" clause fired).
//! This module re-exports the old names so call sites and the schema golden
//! keep compiling, and keeps the TUI-only async wrapper.

pub use norte_config::{
    ConfigError, DEFAULT_PRESET, DaemonMode, HotlistItem, Layer, Layers, NorteToml, Watch,
    WatchMode, config_dir, persist_hotlist_add, persist_hotlist_remove, persist_ui_theme,
    persist_ui_theme_to, standard_layers, user_config_dir, watch, watch_polling,
};
pub use norte_frontend::config::FrontendConfig as LoadedConfig;

/// Load in async context (hot reload): runs in `spawn_blocking` — the
/// runtime never blocks on the FS (rule 2).
///
/// # Errors
/// Those of [`norte_frontend::config::load`].
pub async fn load_async(layers: Layers) -> Result<LoadedConfig, ConfigError> {
    match tokio::task::spawn_blocking(move || norte_frontend::config::load(&layers)).await {
        Ok(res) => res,
        // A panic inside load() is OUR bug: never bury it as a ConfigError
        // with a fake path (rule 6) — let it blow up visibly.
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}
```

- [ ] **Step 2: Adapt call sites in `main.rs`.** `LoadedConfig` is now `FrontendConfig` with a nested `common` — the flat fields that moved are: `preset`, `ui_lang`, `ui_theme`, `daemon_mode`, `daemon_socket`, `hotlist`, `archive_*`, `sources`. Grep-and-fix: `cfg.preset` → `cfg.common.preset`, `cfg.ui_lang` → `cfg.common.ui_lang`, `cfg.ui_theme` → `cfg.common.ui_theme`, `cfg.daemon_mode` → `cfg.common.daemon_mode`, `cfg.daemon_socket` → `cfg.common.daemon_socket`, `cfg.hotlist` → `cfg.common.hotlist`, `cfg.archive_max_entries` → `cfg.common.archive_max_entries` (idem the other two archive fields), `cfg.sources` → `cfg.common.sources`. `cfg.keymap_layers`, `cfg.quick_search_mode`, `cfg.openers` stay flat. Also fix the same field accesses anywhere else they appear (`rg "cfg\.|\.hotlist|archive_max" crates/norte-tui/src` to sweep; snapshot/UI tests included).

- [ ] **Step 3: Prune `crates/norte-tui/Cargo.toml`.** Add `norte-config = { workspace = true, features = ["watch"] }`. Remove `notify` and `toml_edit` (now only used by norte-config); attempt removing `toml` and `serde` — keep whichever `cargo check -p norte-tui` still demands (keymap/lua/theme may use them). The `schema` feature forwards: `schema = ["dep:schemars", "norte-config/schema", "norte-frontend/schema"]`.

- [ ] **Step 4: Fix the `Layer` re-export for Lua.** `crates/norte-tui/src/lua.rs` re-exports `Layer` from `crate::config` — the shim still provides it; verify with the build.

- [ ] **Step 5: Build + full TUI suite**

Run: `cargo nextest run -p norte-tui && cargo check -p norte-tui --features schema`
Expected: PASS (config unit tests now live in norte-config/norte-frontend; TUI keeps integration/snapshot tests)

- [ ] **Step 6: Commit**

```bash
git add crates/norte-tui
git commit -m "refactor(tui): config module becomes shim over norte-config/norte-frontend"
```

### Task 8: Core migration (resolver + layered `[archive]`/`[ai]`)

**Files:**
- Modify: `crates/norte-core/src/connect.rs:121-143`, `crates/norte-core/src/archive_config.rs`, `crates/norte-core/src/ai.rs`, `crates/norte-core/Cargo.toml`

- [ ] **Step 1: Write the failing parity tests** in `archive_config.rs`'s test mod (they define the new injectable API):

```rust
    /// C1 exit criterion: a system-layer `[archive]` binds the daemon path
    /// exactly like it binds the TUI (spec gap 3).
    #[test]
    fn capa_sistema_tambien_aplica() {
        let sistema = tempfile::tempdir().unwrap();
        std::fs::write(sistema.path().join("norte.toml"), "[archive]\nmax_entries = 7\n").unwrap();
        let layers = norte_config::Layers {
            dirs: vec![(sistema.path().to_path_buf(), norte_config::Layer::System)],
        };
        let l = load_archive_limits_from(&layers).expect("carga").expect("overrides");
        assert_eq!(l.max_entries, 7);
    }

    /// Uniform strictness (ADR 0035 decision 4): a `[ui]` typo now fails
    /// the daemon load too — no more silent divergence from the TUI.
    #[test]
    fn typo_en_otra_seccion_es_error_tambien_para_el_daemon() {
        let user = tempfile::tempdir().unwrap();
        std::fs::write(user.path().join("norte.toml"), "[ui]\ntheem = \"nord\"\n").unwrap();
        let layers = norte_config::Layers {
            dirs: vec![(user.path().to_path_buf(), norte_config::Layer::User)],
        };
        assert!(load_archive_limits_from(&layers).is_err());
    }
```

Run: `cargo nextest run -p norte-core archive_config`
Expected: FAIL to compile — `load_archive_limits_from` not defined.

- [ ] **Step 2: Rewrite `archive_config.rs`.** Delete the private `NorteToml`/`ArchiveSection`/`parse` (lines 9-62) and the old single-file tests (`sin_seccion_ni_campos_es_none` etc. — superseded by norte-config's own coverage); keep the module and its saturation logic:

```rust
//! `[archive]` limits for processes without the TUI loader (#95), now via
//! the shared layered loader (ADR 0035): System+User layers apply, the
//! Project layer never does (a foreign repo must not raise security limits).

use norte_vfs_archive::Limits;

/// Merged `[archive]` overrides from the given layers. `None` = no
/// overrides anywhere (use compiled defaults).
///
/// # Errors
/// Any layer that exists but does not parse strictly — fail-loud: an
/// operator who LOWERED limits for agents must not stay at 64 GiB over a
/// silent typo.
pub fn load_archive_limits_from(layers: &norte_config::Layers) -> std::io::Result<Option<Limits>> {
    let cfg = norte_config::load(layers)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    if cfg.archive_max_entries.is_none()
        && cfg.archive_max_decompressed_bytes.is_none()
        && cfg.archive_max_nesting.is_none()
    {
        return Ok(None);
    }
    let mut limits = Limits::default();
    if let Some(n) = cfg.archive_max_entries {
        // Saturate UPWARD (only possible on 32-bit with a value > u32::MAX):
        // never shrink a limit by wrap, and not silently.
        limits.max_entries = usize::try_from(n).unwrap_or_else(|_| {
            tracing::warn!(n, "[archive] max_entries saturates to usize::MAX on this platform");
            usize::MAX
        });
    }
    if let Some(b) = cfg.archive_max_decompressed_bytes {
        limits.max_decompressed_bytes = b;
    }
    if let Some(n) = cfg.archive_max_nesting {
        limits.max_nesting = n;
    }
    Ok(Some(limits))
}

/// Layered load from the standard layers. SYNC (startup): wrap in
/// `spawn_blocking` from async contexts.
///
/// # Errors
/// Those of [`load_archive_limits_from`].
pub fn load_archive_limits() -> std::io::Result<Option<Limits>> {
    load_archive_limits_from(&norte_config::standard_layers())
}
```

`norte-core/Cargo.toml` gains `norte-config.workspace = true`; add `tempfile` to dev-deps if not already there.

- [ ] **Step 3: Delegate the resolver.** In `connect.rs`, replace the body-bearing `config_dir` (lines 121-143) with a re-export that keeps every call site (`norte_core::connect::config_dir()`) compiling:

```rust
// The user config dir — relocated to norte-config (ADR 0035); re-exported
// so the historic `norte_core::connect::config_dir()` path keeps working.
pub use norte_config::config_dir;
```

- [ ] **Step 4: Layer the AI config.** In `ai.rs`: delete the tolerant raw structs `NorteTomlAi`/`AiSection`/`RawProvider` (lines 60-81) and `AiConfig::parse` (83-114); replace `AiConfig::load` (128-141) with:

```rust
    /// Build from the already-merged `[ai]` settings (norte-config).
    fn from_settings(s: norte_config::AiSettings) -> Self {
        Self {
            enabled: s.enabled,
            local_only: s.local_only,
            denied_prefixes: s.denied_prefixes,
            rename_provider: s.rename_provider,
            providers: s
                .providers
                .into_iter()
                .map(|(name, r)| AiProviderConfig {
                    name,
                    kind: r.kind,
                    model: r.model,
                    base_url: r.base_url,
                })
                .collect(),
        }
    }

    /// Layered load (ADR 0035): System+User layers, Project ignored
    /// fail-closed. SYNC (startup): `spawn_blocking` in async contexts.
    ///
    /// # Errors
    /// [`AiConfigError`] if any layer's TOML is invalid.
    pub fn load() -> Result<Self, AiConfigError> {
        Self::load_from(&norte_config::standard_layers())
    }

    /// Like [`AiConfig::load`] with explicit layers (test injection).
    ///
    /// # Errors
    /// [`AiConfigError`] if any layer's TOML is invalid.
    pub fn load_from(layers: &norte_config::Layers) -> Result<Self, AiConfigError> {
        let cfg = norte_config::load(layers).map_err(|e| {
            AiConfigError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
        })?;
        Ok(Self::from_settings(cfg.ai))
    }
```

Fix any `AiConfig::parse` callers found by the build (tests in `ai.rs` and possibly `norte-cli`): rewrite them against `load_from` with a tempdir layer (same pattern as the archive tests in Step 1). The `AiConfigError::Toml` variant becomes unused if nothing constructs it — remove the variant only if the build agrees; otherwise leave it (it is `#[non_exhaustive]`).

- [ ] **Step 5: Run core suite**

Run: `cargo nextest run -p norte-core`
Expected: PASS, including the two new parity tests.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-core Cargo.lock
git commit -m "refactor(core): config_dir + [archive]/[ai] via norte-config, layered (ADR 0035)"
```

### Task 9: GUI migration

**Files:**
- Modify: `crates/norte-gui/Cargo.toml`, `crates/norte-gui/src/keymap.rs:55-123`

GUI is outside the workspace: path deps, separate lockfile, build with `cd crates/norte-gui`.

- [ ] **Step 1: Add deps.** In `crates/norte-gui/Cargo.toml`, alongside the existing `norte-frontend` path dep, add:

```toml
norte-config = { path = "../norte-config", version = "0.3.0-alpha.1" }
```

- [ ] **Step 2: Replace the hand-rolled fork.** In `keymap.rs`: delete `env_user_dir` (63-68) and `layer_dirs` (70-82). Rewrite `build_effectives`/`build_effectives_from` on the shared pieces — layer discovery from `norte-config`, per-layer keymap loading from `norte-frontend` (this also gains the `has_full_keymap` rejection the GUI was missing):

```rust
use norte_config::{Layer, Layers};

/// Build the two `Effective`s (Browse and Viewer) from preset + layers.
///
/// # Errors
/// The first `KeymapError` from any layer.
pub fn build_effectives() -> Result<(Effective, Effective), KeymapError> {
    build_effectives_layers(&norte_config::standard_layers())
}

/// Like [`build_effectives`] with an EXPLICIT user config dir (test
/// injection, kept for the existing tests): builds a minimal Layers of
/// (user, User) + (./.norte, Project).
///
/// # Errors
/// The first `KeymapError` from any layer.
pub fn build_effectives_from(
    user_dir: Option<PathBuf>,
) -> Result<(Effective, Effective), KeymapError> {
    let mut dirs = Vec::new();
    if let Some(u) = user_dir {
        dirs.push((u, Layer::User));
    }
    dirs.push((PathBuf::from(".norte"), Layer::Project));
    build_effectives_layers(&Layers { dirs })
}

fn build_effectives_layers(layers: &Layers) -> Result<(Effective, Effective), KeymapError> {
    let preset = orthodox();
    let mut kfs: Vec<KeymapFile> = Vec::new();
    let mut sources = Vec::new();
    for (dir, kind) in &layers.dirs {
        match norte_frontend::config::load_keymap_layer(dir, *kind, &mut sources) {
            Ok(Some(kf)) => kfs.push(kf),
            Ok(None) => {}
            // The engine error type is what our callers handle; a config
            // error here is always a bad keymap.toml.
            Err(e) => return Err(KeymapError::Parse(e.to_string())),
        }
    }
    let cmds = all_commands();
    let browse = Effective::build_for(&preset, &kfs, &cmds, Screen::Browse)?;
    let viewer = Effective::build_for(&preset, &kfs, &cmds, Screen::Viewer)?;
    Ok((browse, viewer))
}
```

Check the actual variant name for a parse error on `norte_frontend::keymap::KeymapError` (`rg "pub enum KeymapError" -A 15 crates/norte-frontend/src/keymap.rs`) and use it instead of `KeymapError::Parse(String)` if it differs — do not add a new variant; map to the closest existing one. `load_keymap_layer` must be `pub` in `norte-frontend::config` for this (make it so in Task 6 if it was left private).

- [ ] **Step 3: Build + GUI tests**

Run: `cd crates/norte-gui && cargo check && cargo nextest run keymap 2>/dev/null || cargo test keymap`
Expected: PASS — including `build_effective_carga_la_capa_de_usuario_via_norte_config_dir` (its env-based flow now goes through the unified resolver) and a NEW expectation: a user layer with a bare `keymap` list now errors (was silently accepted). If any existing GUI test asserted the old lenient behavior, update it to expect the error, citing ADR 0006.

- [ ] **Step 4: Commit**

```bash
git add crates/norte-gui
git commit -m "refactor(gui): keymap layers via norte-config/norte-frontend (drops drifted fork)"
```

### Task 10: Schema goldens, full CI, reviewers

**Files:**
- Modify: `docs/schema/norte.schema.json` (regenerated)

- [ ] **Step 1: Regenerate the golden** (NorteToml gained `[ai]`):

Run: `NORTE_UPDATE_SCHEMA=1 cargo nextest run -p norte-tui --features schema schema && cargo nextest run -p norte-tui --features schema schema`
Expected: first run writes, second run PASS. `git diff docs/schema/norte.schema.json` must show only the added `ai` definitions.

- [ ] **Step 2: Full local CI**

Run: `just ci`
Expected: EXIT=0. Fix anything it surfaces before proceeding (clippy pedantic is part of the gate; see the recurring-traps memory: formatter hook, nextest, fixtures).

- [ ] **Step 3: Reviewers.** Dispatch `rust-reviewer` on the whole diff and `security-reviewer` focused on: the hermetic `NORTE_CONFIG_DIR` decision, the Project-layer carve-outs surviving the move (`[archive]`, `[ai]`, hotlist, openers), and the denied_prefixes union semantics. Apply findings; re-run `just ci`.

- [ ] **Step 4: Final commit**

```bash
git add -A
git commit -m "feat(config): C1 — shared norte-config crate, unified resolution (ADR 0035)"
```

---

## Self-review notes

- Spec C1 coverage: resolver único (T3), capas+Layer movidos (T3), parser único con core consumiendo secciones (T4/T8), `watch` movido (T6), migración TUI/CLI/GUI/daemon (T7 shim cubre TUI; CLI y daemon llegan por el re-export de `connect::config_dir` + `load_archive_limits`/`AiConfig::load` de T8; GUI T9), fixes de comportamiento (env var en capas T3; core honra capas T8), goldens (T10), ADR (T1). Carve-outs preservados: hotlist/archive en T5 (código movido con sus guardas), openers en T6, ai nuevo con guard Project en T5.
- The `[ai]`-breaks-TUI latent bug is fixed structurally (canonical struct includes `ai`); the schema golden update in T10 is the visible artifact.
- Type consistency checked: `CommonConfig` fields referenced in T7 Step 2 and T8 Step 2 match T5's definition; `FrontendConfig` fields match T6; `load_archive_limits_from`/`AiConfig::load_from` defined where first referenced.
