# GP — GUI typography & look-and-feel polish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Monospace listings/viewer (VSCode-grade), configurable fonts, metrics derived from the font, and a pass of fine visual detail (hover, cursor, spacing rhythm, separators).

**Architecture:** `norte-config` gains three optional `[ui]` fields (`font`, `mono_font`, `font_size`). The GUI resolves a `FontSet` once at startup (`Font` structs with fallback lists ending in GPUI's bundled `.ZedMono`/`.SystemUIFont`, so resolution can never fail), derives `row_h` from `font_size`, and applies mono to panes/viewer/task strip, UI font to chrome. A `Spacing` const module replaces scattered magic px. Hover/cursor land on rows.

**Spec:** GP phase in `docs/superpowers/specs/2026-07-23-gui-visual-plugins-design.md`.

**Verified API facts (pinned GPUI rev f14fea9):** `.font_family(impl Into<SharedString>)` (styled.rs:708); `.font(Font)` applies family+fallbacks+weight (styled.rs:720); `Font { family, features, fallbacks: Option<FontFallbacks>, weight, style }`, `FontFallbacks::from_fonts(Vec<String>)`; bundled families `.ZedMono`/`.SystemUIFont`; unknown families fall through a global stack (never a hard error on normal systems); `.text_size(px)`, `.line_height(px)` (styled.rs:538/740); `.hover(|s| ...)` on div (div.rs:776); `cursor_pointer()` (macro-generated on Styled); `.truncate()` exists and is in use. Current GUI: zero font calls, `ROW_H: f32 = 22.0` (main.rs:88), paddings catalogued in the planning transcript; viewer hex rows are space-padded fixed-width strings (norte-frontend viewer.rs:402-421) that align correctly ONLY under mono — this plan fixes their rendering for free.

**Decisions locked:**
1. Fonts are USER CONFIG, not theme data (`[ui]`, not `[effects]`/roles) — ergonomics, not skin. Recorded in the spec.
2. Defaults: `font` → `.SystemUIFont`, `mono_font` → `.ZedMono`, `font_size` → 14.0. User-set families get `fallbacks = [<bundled default>]` so a typo degrades to the default family, never to chaos.
3. `font_size` out of range [8.0, 32.0] is a LOAD ERROR with the culprit file (ADR 0007 fail-loud; config is strict, unlike theme `[effects]` which clamps — different contracts, both documented).
4. `row_h = (font_size * 1.5).round().max(18.0)`; viewer uses the same. Replaces the `ROW_H` const (which becomes the doc'd formula's default: 14*1.5=21 ≈ old 22 — a ≤1px visual shift, accepted).
5. Hover color computed, no new roles: `hover_bg = lerp(pane_bg, sel_bg, 0.35)` (per-channel), applied via `.hover()` only on rows; `cursor_pointer` on rows.
6. Spacing scale: `mod sp { XS=2.0, S=4.0, M=8.0, L=12.0 }` replacing the scattered magic px in render fns (mapping: row px 4→S, py 1→XS/2 stays 1.0 via XS where it fits — keep visual parity where a value already matches the scale; bump ONLY: pane header py 2→S/2? NO: header py 2.0→4.0 (S) for breathing room, modal px 12 (L) py 8 (M) stays, task strip py 2→XS). Header gains a 1px bottom border in `chrome.border_unfocus`.

---

### Task 1: `[ui]` font fields in norte-config

**Files:** Modify `crates/norte-config/src/schema.rs` (UiSection), `crates/norte-config/src/load.rs` (CommonConfig + validation), `docs/schema/norte.schema.json` (regen).

- [ ] **Step 1 (failing tests, load.rs test mod):**

```rust
    #[test]
    fn ui_fonts_se_cargan_y_validan() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "[ui]\nfont = \"Inter\"\nmono_font = \"JetBrains Mono\"\nfont_size = 15.5\n",
        )
        .unwrap();
        let layers = Layers { dirs: vec![(dir.path().to_path_buf(), Layer::User)] };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.ui_font.as_deref(), Some("Inter"));
        assert_eq!(cfg.ui_mono_font.as_deref(), Some("JetBrains Mono"));
        assert!((cfg.ui_font_size.unwrap() - 15.5).abs() < f32::EPSILON);
    }

    /// ADR 0007: config inválida es error de arranque CON fichero culpable —
    /// un font_size fuera de [8, 32] no se clampa en silencio (contrato
    /// distinto al de [effects], que es data de tema y clampa).
    #[test]
    fn ui_font_size_fuera_de_rango_es_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("norte.toml"), "[ui]\nfont_size = 4.0\n").unwrap();
        let layers = Layers { dirs: vec![(dir.path().to_path_buf(), Layer::User)] };
        assert!(matches!(load(&layers), Err(ConfigError::Toml { .. })));
    }
```

- [ ] **Step 2:** `UiSection` gains (with `#[serde(default)]`, full rustdoc citing the GP spec and the [8,32] range): `pub font: Option<String>`, `pub mono_font: Option<String>`, `pub font_size: Option<f32>`. `CommonConfig` gains `ui_font`, `ui_mono_font`, `ui_font_size` (last-wins scalars, honored from ALL layers incl. Project — presentation-only like theme/lang, consistent with the existing `[ui]` scalars' comment in load.rs). Validation in `load`: `font_size` outside [8.0, 32.0] → `ConfigError::Toml` with the culprit path and a message that does NOT quote the raw value (#73 style: "\[ui\] font_size fuera de rango \[8, 32\]").
- [ ] **Step 3:** `cargo nextest run -p norte-config` green; regen golden: `NORTE_UPDATE_SCHEMA=1 cargo nextest run -p norte-tui --features schema schema` then plain run PASSES; `git diff docs/schema` shows only the three ui additions. clippy/fmt.
- [ ] **Step 4:** Commit `feat(config): [ui] font/mono_font/font_size (GP)` (+ golden in same commit).

### Task 2: FontSet + metrics in the GUI

**Files:** Modify `crates/norte-gui/src/main.rs` (git add -p discipline: TaskKind::Index hunk NEVER staged).

- [ ] **Step 1:** New struct near ChromeColors:

```rust
/// Resolved typography for the session (GP): UI font for chrome, mono for
/// listings/viewer/task strip. Built once at startup from `[ui]` config;
/// user-set families carry a fallback to the bundled default so a typo'd
/// family degrades to the default look, never to an arbitrary system font.
struct FontSet {
    ui: gpui::Font,
    mono: gpui::Font,
    size: gpui::Pixels,
    row_h: gpui::Pixels,
}

impl FontSet {
    fn resolve(font: Option<&str>, mono_font: Option<&str>, font_size: Option<f32>) -> Self {
        let size = font_size.unwrap_or(14.0);
        let row_h = (size * 1.5).round().max(18.0);
        Self {
            ui: family_with_fallback(font, ".SystemUIFont"),
            mono: family_with_fallback(mono_font, ".ZedMono"),
            size: gpui::px(size),
            row_h: gpui::px(row_h),
        }
    }
}

fn family_with_fallback(family: Option<&str>, bundled: &'static str) -> gpui::Font {
    match family {
        None => gpui::font(bundled),
        Some(f) => {
            let mut font = gpui::font(f);
            font.fallbacks =
                Some(gpui::FontFallbacks::from_fonts(vec![bundled.to_owned()]));
            font
        }
    }
}
```

(Adapt to exact constructor paths: `gpui::font(name)` free fn exists at text_system.rs:1077; `FontFallbacks::from_fonts(Vec<String>)`.) `NorteGui` stores `fonts: FontSet`, built in `new` from `cfg.common.ui_font/...` (fallback defaults on config-Err path).
- [ ] **Step 2:** Apply: root div `.font(self.fonts.ui.clone()).text_size(self.fonts.size)`; pane list container + `render_row` + task strip + viewer body/status `.font(self.fonts.mono.clone())` (check whether `.font()` on a parent cascades via text style — it should, TextStyleRefinement cascades; then mono only needs setting on the pane-list container, viewer body, task strip container — verify by smoke). Replace every `px(ROW_H)` with `self.fonts.row_h` (thread through render fns like chrome was); `ROW_H` const deleted; viewer `viewport_size().height / ROW_H` uses the same value. `.line_height(self.fonts.row_h)` on the mono containers so text centers in rows.
- [ ] **Step 3:** Tests: `fontset_defaults` (None everywhere → ui family ".SystemUIFont", mono ".ZedMono", size 14, row_h 21); `fontset_familia_usuario_con_fallback` (Some("Nope Mono") → family "Nope Mono", fallbacks == [".ZedMono"]); `row_h_minimo` (font_size 8 → row_h 18).
- [ ] **Step 4:** `cargo test` + clippy + fmt; commit `feat(gui): FontSet — mono listings/viewer, configurable families/size (GP)`.

### Task 3: fine detail — hover, cursor, spacing, separators

**Files:** Modify `crates/norte-gui/src/main.rs`.

- [ ] **Step 1:** `mod sp` consts (XS 2.0, S 4.0, M 8.0, L 12.0) + sweep the catalogued magic px onto the scale per Decision 6 (keep parity where equal; the two deliberate bumps: pane header `py` 2→4 with the new 1px bottom border `chrome.border_unfocus`; task-strip row px 2→4).
- [ ] **Step 2:** Rows (`render_row`, task-strip rows): `.hover(|s| s.bg(hover_bg))` where `hover_bg = lerp_rgba(chrome.pane_bg_focus_or_pane_bg, chrome.sel_bg, 0.35)` precomputed in `ChromeColors` (new field `hover_bg`, derived — no new theme role; glow does not apply, it's a bg) + `.cursor_pointer()`. Hover must NOT override selected/marked bg confusingly: GPUI hover style layers on top — acceptable; verify visually.
- [ ] **Step 3:** Modal: radius consistent with bezel aesthetic (`.rounded(px(6.0))` on panel), padding already L/M. Banner lines get `.px(px(sp::S))`. Selected row `.rounded(px(3.0))` subtle.
- [ ] **Step 4:** Hostile-names pin still green (row rendering path unchanged semantically — run the existing corpus tests); `cargo test` + clippy + fmt.
- [ ] **Step 5:** Manual smoke REQUIRED with screenshots: default + retro-crt + nord; check mono alignment in F3 hex view (space-padded columns now line up), hover visible, cursor changes, text legible at 14px; then one run with `[ui] font_size = 18` + custom `mono_font = "JetBrains Mono"` (if installed; else report fallback behavior observed).
- [ ] **Step 6:** Commit `feat(gui): look-and-feel — hover, cursor, spacing scale, separators (GP)`.

### Task 4: gate + reviewers

- [ ] `just ci` (workspace side: norte-config + golden).
- [ ] rust-reviewer on the GP range (FontSet resolution, cascade correctness, row_h threading — any leftover ROW_H; spacing sweep completeness) + encoding-auditor quick pass (font family strings are user config rendered nowhere except… verify they never hit a banner unmasked; hostile corpus rows under mono unchanged).
- [ ] Apply findings; memory update.

## Self-review notes
- Spec GP coverage: mono listings/viewer/task strip ✓ (T2), `[ui]` fields ✓ (T1), fallback chain documented ✓ (T2 family_with_fallback), row_h from font ✓, spacing scale ✓ (T3), hover/cursor ✓, modal/banner polish ✓, hostile-names pin ✓, screenshots ✓ (T3.5). Non-goals respected (no ligature config, no per-theme fonts).
- Type consistency: FontSet fields used in T2 Step 2 match Step 1; `hover_bg` defined before use; sp consts named where swept.
- Known risk: `.font()` cascade behavior — T2 verifies by smoke; if it doesn't cascade, set on each text-bearing child (mechanical fallback, noted).
