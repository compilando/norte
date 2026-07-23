# C2+G0 — GUI config parity & chrome-through-theme Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The GUI consumes real configuration (`[ui].theme`, `[ui].lang`, `[keymap] preset`) through the shared loaders, keymap presets live once, and every hardcoded GUI chrome color routes through `norte-theme` roles — unblocking phase G1 (visual effect packs).

**Architecture:** Two shared pieces move into `norte-frontend` (theme-spec resolution from `norte-tui/src/theme.rs`; the three keymap-preset TOMLs) plus one engine addition (lenient-preset build that skips bindings to commands a frontend lacks). `norte-theme` gains three additive roles. The GUI (out-of-workspace) then loads `FrontendConfig` at boot and paints chrome from the theme.

**Tech Stack:** Existing crates only: norte-frontend, norte-theme, norte-tui, norte-gui (GPUI, excluded from workspace — build with `cd crates/norte-gui`).

**Specs:** `docs/superpowers/specs/2026-07-23-help-config-system-design.md` (phase C2) and `docs/superpowers/specs/2026-07-23-gui-visual-plugins-design.md` (phase G0).

**Key facts (verified during planning):**

- `norte-tui/src/theme.rs:144-164` — `resolve(spec, depth)`: preset-name-or-path → `TuiTheme`. The name/path resolution + `ResolveError` (113-136) are frontend-agnostic; only `TuiTheme`/depth/ratatui conversion is TUI-specific. Its own doc (lines 5-6) anticipates this split.
- `norte-frontend/src/keymap.rs:467-479` — `Effective::build_for` is STRICT about preset bindings (`UnknownCommand`). That is exactly why the GUI ships a trimmed private `orthodox.toml`: the full preset binds commands (`app.help`, `app.extensions`, …) the GUI doesn't implement. Sharing presets requires a lenient-preset mode (layers stay strict).
- Preset files: `crates/norte-tui/src/keymap_presets/{orthodox,vim,cua}.toml`, `crates/norte-gui/src/keymap_presets/orthodox.toml` (trimmed duplicate — dies).
- GUI chrome consts: `crates/norte-gui/src/main.rs:96-108` — BG 0x121212, FG 0xffffff, PANE_BG 0x1e1e1e, PANE_BG_FOCUS 0x252526, HEADER_BG 0x2d2d2d, BORDER_FOCUS 0x3b82f6, BORDER_UNFOCUS 0x3a3a3a, SEL_BG 0x264f78, ERR_FG 0xf87171, QUICK_FG 0xfbbf24, MARK_BG 0x3d3315. Theme hardcoded to default at `main.rs:208`; color seams: `theme_map::to_gpui_rgba`, `entry_color` (`main.rs:1986-1991`).
- `norte-theme` roles (`role.rs:15-44`, `#[non_exhaustive]`): Background, Regular, Selection, BorderFocus, BorderUnfocused, ModalBorder, StatusBar, Title, HostileBadge, Error, Warning, Info, Match. Missing for GUI chrome: pane backgrounds and mark background.
- GUI lang today: `NORTE_LANG`/`LC_ALL` env chain only (`norte-gui/src/main.rs:2119`); TUI chain is `NORTE_LANG` > `[ui].lang` > env (`norte-tui/src/main.rs:224-225`).
- Out of scope (features the GUI does not have): hotlist UI, quick-search UI, hot reload (spec C2 marks it optional — not done here), daemon-mode selection.

**Role mapping decision (locked):** existing roles cover most chrome — BG→`Background`, FG→`Regular`, SEL_BG→`Selection`, BORDER_FOCUS→`BorderFocus`, BORDER_UNFOCUS→`BorderUnfocused`, HEADER_BG→`StatusBar`, ERR_FG→`Error`, QUICK_FG→`Match`. Three additive roles close the gap: `PaneBackground` (PANE_BG), `PaneFocusBackground` (PANE_BG_FOCUS), `Mark` (MARK_BG). Code fallbacks reproduce the current GUI constants so the default look is pixel-identical.

---

### Task 1: Shared theme-spec resolution in norte-frontend

**Files:**
- Create: `crates/norte-frontend/src/theme.rs`
- Modify: `crates/norte-frontend/src/lib.rs`, `crates/norte-frontend/Cargo.toml`, `crates/norte-tui/src/theme.rs`

- [ ] **Step 1: Write the failing test** in the new `crates/norte-frontend/src/theme.rs` (bottom):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_es_el_preset_default() {
        let t = resolve_theme(None).expect("default");
        assert_eq!(t.name.as_deref(), Theme::preset_default().name.as_deref());
    }

    #[test]
    fn nombre_de_preset_resuelve() {
        let t = resolve_theme(Some("nord")).expect("preset embebido");
        assert_eq!(t.name.as_deref(), Some("nord"));
    }

    #[test]
    fn ruta_a_fichero_resuelve_y_rota_es_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("mio.toml");
        std::fs::write(&p, "name = \"mio\"\n").unwrap();
        let t = resolve_theme(Some(p.to_str().unwrap())).expect("fichero");
        assert_eq!(t.name.as_deref(), Some("mio"));
        let missing = dir.path().join("no-existe.toml");
        assert!(matches!(
            resolve_theme(Some(missing.to_str().unwrap())),
            Err(ResolveError::Io { .. })
        ));
    }
}
```

(Test uses `to_str().unwrap()` on a tempdir path we constructed from ASCII — fine in tests.) Add `norte-theme.workspace = true` to `crates/norte-frontend/Cargo.toml` `[dependencies]`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p norte-frontend theme`
Expected: FAIL to compile — `resolve_theme` undefined.

- [ ] **Step 3: Implement by moving.** MOVE from `crates/norte-tui/src/theme.rs` into the new module: `ResolveError` (lines 113-136, rustdoc intact) and the name/path resolution logic of `resolve` (144-164) reshaped to return `Theme`:

```rust
//! Frontend-shared theme resolution (ADR 0020): a `[ui].theme` spec is a
//! bundled preset NAME or a PATH to a theme TOML. Each frontend then bridges
//! the resolved [`Theme`] to its renderer (ratatui in the TUI, GPUI in the
//! GUI).

use std::path::Path;

use norte_theme::Theme;

/// Resolves the `[ui].theme` spec: an embedded preset name or a path to a
/// `.toml` theme file. `None` = the default preset.
///
/// # Errors
/// [`ResolveError`] if the path cannot be read or the TOML does not
/// validate; the caller decides to degrade to the default and warn.
pub fn resolve_theme(spec: Option<&str>) -> Result<Theme, ResolveError> {
    let Some(spec) = spec else {
        return Ok(Theme::preset_default());
    };
    let parse = |e: norte_theme::ThemeError| ResolveError::Parse {
        spec: spec.to_owned(),
        detail: e.to_string(),
    };
    if let Some(theme) = Theme::preset(spec).map_err(parse)? {
        return Ok(theme);
    }
    let path = Path::new(spec);
    let raw = std::fs::read_to_string(path).map_err(|e| ResolveError::Io {
        spec: spec.to_owned(),
        source: e,
    })?;
    Theme::from_toml(&raw).map_err(parse)
}
```

Then in `crates/norte-tui/src/theme.rs`: delete the moved `ResolveError` and rewrite `resolve` as a thin wrapper (keep name and signature so `main.rs` call sites don't change):

```rust
pub use norte_frontend::theme::{ResolveError, resolve_theme};

/// Resuelve la especificación `[ui].theme` al [`TuiTheme`] (tema compartido
/// + profundidad del terminal). La resolución nombre/ruta vive en
/// `norte_frontend::theme` (compartida con la GUI).
///
/// # Errors
/// Los de [`resolve_theme`].
pub fn resolve(spec: Option<&str>, depth: ColorDepth) -> Result<TuiTheme, ResolveError> {
    Ok(TuiTheme::new(resolve_theme(spec)?, depth))
}
```

`crates/norte-frontend/src/lib.rs` gains `pub mod theme;`. The rule-2 note: `resolve_theme` does sync `std::fs` — copy the "SYNC (startup): spawn_blocking in async contexts" sentence into its rustdoc; check how the TUI calls `resolve` (main.rs `apply_theme` path) and confirm it already runs it via spawn_blocking or at sync startup — report if not, do not change the call pattern.

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p norte-frontend -p norte-tui && cargo clippy -p norte-frontend -p norte-tui --all-targets -- -D warnings && cargo fmt --all`
Expected: PASS (frontend theme tests + full TUI suite green — TUI theme tests keep passing through the wrapper).

- [ ] **Step 5: Commit**

```bash
git add crates/norte-frontend crates/norte-tui Cargo.lock
git commit -m "refactor(frontend,tui): shared theme-spec resolution (C2)"
```

### Task 2: Shared keymap presets + lenient-preset build

**Files:**
- Create: `crates/norte-frontend/presets/keymap/{orthodox,vim,cua}.toml` (moved)
- Modify: `crates/norte-frontend/src/keymap.rs`, `crates/norte-tui/src/keymap.rs`, delete `crates/norte-tui/src/keymap_presets/`

- [ ] **Step 1: Move the preset files.** `git mv crates/norte-tui/src/keymap_presets crates/norte-frontend/presets/keymap` (keep the three TOMLs byte-identical). In `crates/norte-frontend/src/keymap.rs` add at the end:

```rust
/// Bundled keymap presets (ADR 0006), shared by every frontend. Each
/// frontend validates against ITS OWN command set — via [`Effective::build_for`]
/// (strict) or [`Effective::build_for_subset`] (preset bindings to commands
/// the frontend lacks are skipped).
pub mod presets {
    /// The default orthodox preset.
    pub const ORTHODOX: &str = include_str!("../presets/keymap/orthodox.toml");
    /// Vim-style preset.
    pub const VIM: &str = include_str!("../presets/keymap/vim.toml");
    /// CUA preset.
    pub const CUA: &str = include_str!("../presets/keymap/cua.toml");

    /// Preset source by name; `None` if unknown (caller falls back +
    /// reports, same contract the TUI had).
    #[must_use]
    pub fn source(name: &str) -> Option<&'static str> {
        match name {
            "orthodox" => Some(ORTHODOX),
            "vim" => Some(VIM),
            "cua" => Some(CUA),
            _ => None,
        }
    }
}
```

(Adjust the `include_str!` relative path to the crate layout — it is relative to `src/keymap.rs`.)

- [ ] **Step 2: Failing tests for the lenient build** (in `norte-frontend/src/keymap.rs` tests):

```rust
    /// `build_for_subset`: a PRESET binding to a command this frontend does
    /// not implement is skipped (the GUI implements a subset of the TUI's
    /// commands); a LAYER binding to an unknown command is still an error
    /// (a user typo must never die silently — ADR 0006).
    #[test]
    fn build_for_subset_filtra_preset_pero_capa_sigue_estricta() {
        let preset = parse_keymap(
            "[pane]\nkeymap = [\n { on = [\"q\"], run = \"app.quit\" },\n { on = [\"f1\"], run = \"app.help\" },\n]\n",
        )
        .unwrap();
        let known = ["app.quit"];
        let eff = Effective::build_for_subset(&preset, &[], &known, Screen::Browse)
            .expect("preset con extras construye");
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('q'))),
            Resolution::Run("app.quit".into())
        );
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::F(1))),
            Resolution::NoMatch,
            "el binding filtrado no existe"
        );
        let layer = parse_keymap(
            "[pane]\nprepend_keymap = [{ on = [\"z\"], run = \"app.help\" }]\n",
        )
        .unwrap();
        assert!(
            Effective::build_for_subset(&preset, &[layer], &known, Screen::Browse).is_err(),
            "capa con comando desconocido: error, no filtrado"
        );
    }
```

(Check the real `Resolution` variant name for a non-matching chord — `rg "NoMatch\|enum Resolution" -A 5 crates/norte-frontend/src/keymap.rs` — and adapt; if a single unmatched chord yields something else (`Pending`/`Unbound`), assert that.) Run: `cargo nextest run -p norte-frontend build_for_subset` — expect compile failure.

- [ ] **Step 3: Implement `build_for_subset`.** In `Effective`: refactor `build_for` minimally so both entry points share the body with a `preset_mode` flag:

```rust
    /// Like [`Effective::build_for`], but PRESET bindings whose command is
    /// not in `known_commands` are skipped instead of failing. For
    /// frontends that implement a subset of the shared presets' commands
    /// (the GUI). User/project LAYERS remain strict.
    ///
    /// # Errors
    /// Same as [`Effective::build_for`], except preset `UnknownCommand`.
    pub fn build_for_subset(
        preset: &KeymapFile,
        layers: &[KeymapFile],
        known_commands: &[&str],
        screen: Screen,
    ) -> Result<Self, KeymapError> { ... }
```

Implementation guidance: locate where `build_for` validates each preset binding's `run` against `known_commands` (the `UnknownCommand` construction); in subset mode, `continue` over that binding instead of erroring — ONLY for bindings originating from the preset's `keymap` lists, not from any layer. Keep `build_for` delegating to the shared body with strict mode so its behavior is bit-identical (the full TUI keymap test suite is the guard).

- [ ] **Step 4: Point the TUI at the shared presets.** In `crates/norte-tui/src/keymap.rs`, find where the TUI embeds its presets (`rg -n "include_str!" crates/norte-tui/src`) and replace the three `include_str!` with `norte_frontend::keymap::presets::{ORTHODOX, VIM, CUA}` (or `presets::source(name)` if the TUI resolves by name — mirror its current shape). Delete nothing else. The old `crates/norte-tui/src/keymap_presets/` dir is already gone via `git mv`.

- [ ] **Step 5: Verify**

Run: `cargo nextest run -p norte-frontend -p norte-tui && cargo clippy -p norte-frontend -p norte-tui --all-targets -- -D warnings && cargo fmt --all`
Expected: PASS — including every existing TUI keymap/help/snapshot test (presets byte-identical, strict path untouched).

- [ ] **Step 6: Commit**

```bash
git add crates/norte-frontend crates/norte-tui Cargo.lock
git commit -m "refactor(frontend,tui): shared keymap presets + build_for_subset (C2)"
```

### Task 3: GUI consumes config — preset, theme, lang

**Files:**
- Modify: `crates/norte-gui/src/keymap.rs`, `crates/norte-gui/src/main.rs`, `crates/norte-gui/Cargo.toml` (only if a dep is missing — norte-config and norte-frontend are already there)

CAUTION: `crates/norte-gui/src/main.rs` may carry pre-existing uncommitted changes; edit surgically, never revert lines you didn't write. Build with `cd crates/norte-gui && cargo check`.

- [ ] **Step 1: GUI keymap uses shared presets + subset build + `[keymap] preset`.** In `crates/norte-gui/src/keymap.rs`:
  - Delete `crates/norte-gui/src/keymap_presets/orthodox.toml` and the `orthodox()` `include_str!` fn.
  - Replace with resolution by name: `fn preset(name: &str) -> KeymapFile` parsing `norte_frontend::keymap::presets::source(name)` and falling back to `ORTHODOX` when unknown (with the same panic-on-invalid-embedded-TOML contract, since the sources are compile-time constants — keep the test pinning each of the three presets builds for the GUI).
  - `build_effectives_layers` switches `Effective::build_for` → `Effective::build_for_subset` (both Browse and Viewer builds) and takes the preset name as a parameter, threaded from the loaded config (next step). Existing signatures `build_effectives()`/`build_effectives_from(...)` gain a `preset: &str` parameter (update the tests; default `"orthodox"`).
  - New test: `preset_vim_construye_para_la_gui` — `build_effectives` with `"vim"` succeeds (subset mode skips the vim bindings to commands the GUI lacks) and still resolves a shared binding (e.g. F3 → `pane.view`, mirroring the existing orthodox test at `keymap.rs` tests).

- [ ] **Step 2: GUI boots from `FrontendConfig`.** In `crates/norte-gui/src/main.rs` startup (around the current `theme = Theme::preset_default()` at ~line 208 and the keymap setup at ~212-224):

```rust
    // Configuración real (C2): capas compartidas — escalares + keymap.
    // Síncrono y bloqueante A PROPÓSITO: esto corre en el arranque, antes
    // de crear la ventana; la GUI no tiene runtime async propio aquí.
    let loaded = norte_frontend::config::load(&norte_config::standard_layers());
    let (cfg, cfg_error) = match loaded {
        Ok(c) => (Some(c), None),
        Err(e) => (None, Some(e.to_string())),
    };
    let preset_name = cfg
        .as_ref()
        .map_or("orthodox", |c| c.common.preset.as_str());
    let theme = match cfg.as_ref().and_then(|c| c.common.ui_theme.as_deref()) {
        spec => norte_frontend::theme::resolve_theme(spec).unwrap_or_else(|_| {
            // Tema roto: degradar al default y avisar por el banner (misma
            // filosofía que el keymap: la GUI arranca siempre).
            norte_theme::Theme::preset_default()
        }),
    };
```

Adapt to the real surrounding code: the existing keymap-error banner mechanism (`main.rs:212-224` fallback + banner) is the reporting channel — reuse it: a `cfg_error` or theme resolve error appends to that banner text rather than adding a new UI element. Thread `preset_name` into `build_effectives`. IMPORTANT: `build_effectives` currently also loads keymap layers itself via `standard_layers` — after this step the layers come from the SAME `FrontendConfig.keymap_layers`? NO — keep it simple and consistent with the current structure: `build_effectives(preset_name)` keeps doing its own layer pass (it already goes through `load_keymap_layer`); the `FrontendConfig` here is used for `common.*` scalars only. Loading keymap layers twice at startup is redundant but correct and cheap; note it as a follow-up in the report (unifying would restructure T9's code — out of scope).
  - Lang: where the GUI negotiates language (`main.rs:2119` env chain), insert `[ui].lang` between `NORTE_LANG` and the env fallback, mirroring the TUI (`NORTE_LANG` > `cfg.common.ui_lang` > `LC_ALL`/env). Keep the function testable if it already is; adapt its tests.

- [ ] **Step 3: Verify**

Run: `cd crates/norte-gui && cargo check && cargo test`
Expected: all GUI tests pass (keymap tests adapted for the preset param, new vim test green). Manual smoke optional: `NORTE_CONFIG_DIR=$(mktemp -d)` with a `norte.toml` setting `[ui] theme = "nord"` + `[keymap] preset = "vim"` — report if you ran it.

- [ ] **Step 4: Commit**

```bash
git add crates/norte-gui/Cargo.toml crates/norte-gui/src/keymap.rs crates/norte-gui/src/main.rs crates/norte-gui/Cargo.lock
git commit -m "feat(gui): consume [ui].theme/[ui].lang/[keymap] preset via shared config (C2)"
```

(If main.rs pre-existing dirt is entangled in the same hunks you edited, report it BEFORE committing and stage only your hunks with `git add -p` if separable; if inseparable, stop and report BLOCKED.)

### Task 4: Three new theme roles (G0 groundwork)

**Files:**
- Modify: `crates/norte-theme/src/role.rs`, `crates/norte-theme/presets/default.toml`

- [ ] **Step 1: Failing test** (in norte-theme's existing test layout — `tests/model.rs` or role.rs tests, mirror where `Role::ALL` is pinned):

```rust
    /// C2/G0: los tres roles de chrome de la GUI existen, están en ALL y
    /// tienen fallback monocromo utilizable.
    #[test]
    fn roles_de_chrome_gui_presentes() {
        for r in [Role::PaneBackground, Role::PaneFocusBackground, Role::Mark] {
            assert!(Role::ALL.contains(&r));
            let _ = r.fallback();
        }
    }
```

Run `cargo nextest run -p norte-theme` — compile failure expected.

- [ ] **Step 2: Add the roles.** In `role.rs` (enum is `#[non_exhaustive]`, kebab-case serde):
  - `PaneBackground` — doc: "Pane interior background (GUI chrome; TUI may adopt it later)." Fallback: bg-only style, no color forced (monochrome fallback = `Style::default()` shape consistent with existing entries — read two existing `fallback()` arms first and mirror the pattern exactly).
  - `PaneFocusBackground` — "Focused pane interior background."
  - `Mark` — "Marked-entry background (selection marks, distinct from the cursor's `Selection`)."
  - Extend `Role::ALL`.
  - In `presets/default.toml` add the three roles with the CURRENT GUI constants so the default look is preserved: `pane-background` bg `#1e1e1e`, `pane-focus-background` bg `#252526`, `mark` bg `#3d3315`. Check the preset TOML's exact role-table syntax first (read `default.toml`) and mirror it. Other presets: untouched (missing roles inherit the default theme per ADR 0020 — verify that inheritance claim in `Theme::style` and report if missing roles actually fall back to `Role::fallback()` instead; either way the GUI keeps working).

- [ ] **Step 3: Verify + commit**

Run: `cargo nextest run -p norte-theme -p norte-tui && cargo clippy -p norte-theme --all-targets -- -D warnings`
Expected: PASS (TUI unaffected — it doesn't use the new roles).

```bash
git add crates/norte-theme
git commit -m "feat(theme): PaneBackground/PaneFocusBackground/Mark roles (G0)"
```

### Task 5: GUI chrome through the theme

**Files:**
- Modify: `crates/norte-gui/src/main.rs`, possibly `crates/norte-gui/src/theme_map.rs`

- [ ] **Step 1: Add a chrome-resolution helper** next to `entry_color` (main.rs ~1986):

```rust
/// Chrome color from the theme with the pre-C2 constant as fallback: the
/// default preset reproduces the historic look exactly; a themed GUI gets
/// its skin. `fg`=false takes the role's background color.
fn chrome(theme: &norte_theme::Theme, role: Role, fg: bool, fallback: u32) -> gpui::Rgba {
    let style = theme.style(role);
    let c = if fg { style.fg } else { style.bg };
    c.map_or(gpui::rgb(fallback), theme_map::to_gpui_rgba)
}
```

(Adapt: check `theme.style(role)` return type — owned `Style` vs ref — and `to_gpui_rgba`'s exact signature/import path; the GUI already imports both.)

- [ ] **Step 2: Replace the consts.** Sweep every use of the 11 consts (`rg -n "PANE_BG|HEADER_BG|BORDER_FOCUS|BORDER_UNFOCUS|SEL_BG|MARK_BG|ERR_FG|QUICK_FG|\bBG\b|\bFG\b" crates/norte-gui/src/main.rs`) and route through `chrome(...)` with this mapping: BG→(Background,bg), FG→(Regular,fg), PANE_BG→(PaneBackground,bg), PANE_BG_FOCUS→(PaneFocusBackground,bg), HEADER_BG→(StatusBar,bg), BORDER_FOCUS→(BorderFocus,fg), BORDER_UNFOCUS→(BorderUnfocused,fg), SEL_BG→(Selection,bg), MARK_BG→(Mark,bg), ERR_FG→(Error,fg), QUICK_FG→(Match,fg). Keep the const declarations (they are now the documented fallbacks passed to `chrome`) — move them next to `chrome` with a comment "pre-C2 look, used as fallback when the theme doesn't style the role". Compute the chrome colors ONCE per `render()` (a small `struct ChromeColors` built at the top of `render` and threaded to the helpers), not per row — `render_row` runs per visible entry.

- [ ] **Step 3: Pin the look.** New test (GUI tests, wherever render-adjacent tests live; if none exist for colors, add a unit test on `chrome`):

```rust
    /// Default theme must reproduce the historic chrome exactly (the C2
    /// migration is invisible without a custom theme).
    #[test]
    fn tema_default_reproduce_el_chrome_historico() {
        let t = norte_theme::Theme::preset_default();
        assert_eq!(chrome(&t, Role::PaneBackground, false, PANE_BG), gpui::rgb(0x1e1e1e));
        assert_eq!(chrome(&t, Role::Selection, false, SEL_BG), gpui::rgb(SEL_BG));
        assert_eq!(chrome(&t, Role::Mark, false, MARK_BG), gpui::rgb(0x3d3315));
    }
```

(Adapt assertions to whatever the default preset actually declares: if default.toml styles Selection's bg differently from SEL_BG 0x264f78, the HISTORIC look wins — add the missing declarations to default.toml in Task 4 style rather than weakening this test. Verify `gpui::Rgba` implements `PartialEq`; otherwise compare components.)

- [ ] **Step 4: Verify + commit**

Run: `cd crates/norte-gui && cargo check && cargo test && cargo clippy --all-targets -- -D warnings 2>&1 | tail -5`
Expected: PASS. Then:

```bash
git add crates/norte-gui/src
git commit -m "feat(gui): chrome colors through theme roles (G0)"
```

### Task 6: Workspace gate + reviewers

- [ ] **Step 1:** `just ci` — EXIT=0 (workspace: frontend/tui/theme changes ride the normal gate; GUI is out of workspace, already verified per-task).
- [ ] **Step 2:** Dispatch `rust-reviewer` on the full C2+G0 diff range and `encoding-auditor` focused on: preset name handling (untrusted `[keymap] preset` string from config → `presets::source` fallback), theme-spec path handling (`resolve_theme` takes a user string as a path — no traversal concern since it's the user's own config, but banner rendering of the error must mask the spec, mirroring #73). Apply findings, re-run gates.
- [ ] **Step 3:** Final commit if fixes landed.

---

## Self-review notes

- C2 spec coverage: theme ✓ (T1/T3), preset incl. vim/cua ✓ (T2/T3), fork already deleted in C1-T9, preset dedup ✓ (T2), hot reload explicitly skipped (spec: optional), hotlist/quick_search consumption N/A (no GUI feature — documented in scope). G0 coverage: chrome-through-roles ✓ (T4/T5); C2-lands-first ordering preserved (T1-T3 before T4-T5 is not strictly required but tasks are ordered so the config plumbing exists when chrome lands).
- Lenient-subset decision documented in T2 (frontend-internal, no ADR).
- Type consistency: `chrome` uses `Role` variants defined in T4; `build_for_subset` referenced in T2 tests before implementation (TDD); `resolve_theme` signature consistent across T1/T3.
- Known redundancy accepted: GUI loads keymap layers twice at startup (FrontendConfig + build_effectives) — noted as follow-up, not fixed here.
