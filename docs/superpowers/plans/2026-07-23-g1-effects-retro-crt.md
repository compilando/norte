# G1 — `[effects]` schema v1 + retro-CRT presets Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The GUI interprets a typed, clamped `[effects]` v1 (scanlines, vignette, glow, bezel — all static) and ships bundled `retro-crt` / `retro-crt-amber` presets: the "retrotermcool" look, as data, no plugin code.

**Architecture:** ADR 0036 defines the schema (closing ADR 0020's deferral). `norte-theme` gains two new complete presets carrying `[effects]`. `norte-gui` gains an `effects.rs` interpreter (parse `Theme.effects`' opaque `toml::Value` → clamped `EffectsV1`, per-key warn-and-skip degradation) and a render overlay (one `canvas()` layer for scanlines+vignette, bezel on the root, glow as a `ChromeColors` fg post-process). Zero cost and byte-identical render tree when a theme has no effects.

**Tech Stack:** Existing crates. GPUI `canvas()` element (availability at the pinned rev must be verified in T4 — fallback documented there).

**Spec:** `docs/superpowers/specs/2026-07-23-gui-visual-plugins-design.md` phase G1. Prereq G0 landed (chrome via `ChromeColors`, pair-honest; theme canonical).

**Design decisions locked here (recorded in the ADR):**

1. Schema v1 keys: `scanlines { opacity, spacing_px }`, `vignette { strength }`, `glow { strength }`, `bezel { radius_px, inset }`. **No `palette` key**: the spec's draft listed one, but a phosphor palette is exactly what the theme's own `[roles]`/`[files]` express — the retro presets carry green/amber roles directly. A dedicated palette key would be a second way to say the same thing; deferred until a real use case appears (deviation from the spec draft, documented).
2. Clamps (a11y, spec G1): `scanlines.opacity` [0.0, 0.35], `scanlines.spacing_px` [2, 16], `vignette.strength` [0.0, 0.6], `glow.strength` [0.0, 1.0], `bezel.radius_px` [0, 32]. Out-of-range values CLAMP (not error); wrong-typed or unknown keys inside `[effects]` WARN (log) and skip that key — a theme is user data, never a startup error (ADR 0020: lenient).
3. Glow v1 = fg brightening: each `ChromeColors` fg channel (and `entry_color` output) lerps toward white by `strength * 0.25` max. Static, cheap, no per-frame cost. Real bloom is G4's question.
4. The overlay never intercepts input: scanlines+vignette paint in one non-interactive canvas layered last; T4 must verify clicks/scroll still reach panes (GPUI hit-testing) — if the canvas captures input at the pinned rev, fall back to painting the overlay via `div` gradient layers or mark the element non-interactive per GPUI's mechanism; BLOCKED report if neither works.
5. Effects apply to the GUI only (TUI ignores `[effects]`, pinned by existing norte-theme test).

---

### Task 1: ADR 0036 — effects schema v1

**Files:** Create `docs/adr/0036-effects-schema-v1.md`

- [ ] **Step 1:** Write the ADR (MADR format, mirror 0035's structure; English). Title: "GUI effects schema v1 for theme `[effects]`". Status accepted, date 2026-07-23. Context: ADR 0020 reserved `[effects]` opaque pending M5; G0 landed chrome-through-theme; spec G1 approved. Decision: the five decisions above (schema table with keys/types/clamps/degradation semantics; no-palette rationale; glow-as-brightening; overlay non-interactivity requirement; GUI-only). Consequences: `Theme.effects` stays `Option<toml::Value>` in norte-theme (the TYPED interpretation lives in the GUI — the theme crate stays renderer-agnostic per ADR 0020); newer keys degrade gracefully in older GUIs (warn+skip); the bundled retro presets become the reference "visual plugin"; `docs/theming.md` gains a user-facing `[effects]` section.
- [ ] **Step 2:** Add ADR row to `docs/adr/README.md` index. Update `docs/theming.md`'s "GPU effects" section (currently says "reserved"): document the v1 keys, clamps, and that unknown keys are ignored with a warning.
- [ ] **Step 3:** Commit: `docs(adr): 0036 GUI effects schema v1 + theming.md user docs`

### Task 2: retro-CRT presets in norte-theme

**Files:** Create `crates/norte-theme/presets/retro-crt.toml`, `crates/norte-theme/presets/retro-crt-amber.toml`; modify `crates/norte-theme/src/presets.rs`

- [ ] **Step 1 (failing test):** In `crates/norte-theme/tests/presets.rs` add:

```rust
/// G1: los presets retro declaran [effects] (la GUI los interpreta; la TUI
/// los ignora) y pasan la misma completitud que el resto.
#[test]
fn presets_retro_traen_effects() {
    for name in ["retro-crt", "retro-crt-amber"] {
        let t = norte_theme::Theme::preset(name)
            .expect("parsea")
            .expect("preset registrado");
        assert!(t.has_effects(), "{name} sin [effects]");
    }
}
```

Run `cargo nextest run -p norte-theme presets_retro` — FAIL (unknown preset).

- [ ] **Step 2:** Write the two preset files. Requirements:
  - ALL 16 roles declared (the completeness test `cada_preset_parsea_y_es_completo` enforces it) + `[files.kind]`/`[files.ext]` blocks (mirror default.toml's coverage).
  - `retro-crt`: green phosphor (P1-style). Palette guide: bg `#0a0f0a`, pane `#0d140d`, pane-focus `#122012`, regular fg `#33ff66`-family toned to `#2ee65c`, dim green `#1a8f3c` for unfocused/borders, selection bg `#134f26` fg `#aaffcc`, status-bar pair fg `#0a0f0a` bg `#2ee65c`, error `#ff5f5f`, warning `#d7af5f`, match pair fg `#0a0f0a` bg `#7bff9e`, mark bg `#123a12`, files.kind dir bold bright green, symlink cyan-green `#33d9a6`, executable `#7bff9e`. Tune for contrast: regular fg on bg must clear 7:1 (measure while writing: relative-luminance formula, quick mental check is fine — the review will measure).
  - `retro-crt-amber`: same structure, amber (P3): bg `#100c04`, fg family `#ffb000`/`#e69a00`, dims `#8f6a1a`, selection bg `#4f3a13`, etc. — coherent monochrome-amber ramp.
  - Both end with:

```toml
[effects]
scanlines = { opacity = 0.12, spacing_px = 3 }
vignette  = { strength = 0.35 }
glow      = { strength = 0.5 }
bezel     = { radius_px = 10, inset = true }
```

- [ ] **Step 3:** Register both in `crates/norte-theme/src/presets.rs` `PRESETS` (after the light themes, comment `// Retro (G1): [effects] interpretados por la GUI.`).
- [ ] **Step 4:** `cargo nextest run -p norte-theme -p norte-tui` — ALL green (completeness + new test; TUI picker will now list the retro presets and render their ROLES fine, ignoring effects). `cargo fmt --all`, clippy norte-theme.
- [ ] **Step 5:** Commit: `feat(theme): retro-crt + retro-crt-amber presets with [effects] v1 (G1)`

### Task 3: `EffectsV1` interpreter in norte-gui

**Files:** Create `crates/norte-gui/src/effects.rs`; modify `crates/norte-gui/src/main.rs` (mod decl only in this task)

- [ ] **Step 1 (failing tests first):** effects.rs test mod:

```rust
    #[test]
    fn tema_sin_effects_es_none() {
        let t = norte_theme::Theme::preset_default();
        assert!(EffectsV1::from_theme(&t).is_none());
    }

    #[test]
    fn retro_crt_parsea_con_valores_del_preset() {
        let t = norte_theme::Theme::preset("retro-crt").unwrap().unwrap();
        let e = EffectsV1::from_theme(&t).expect("retro trae effects");
        let s = e.scanlines.expect("scanlines");
        assert!((s.opacity - 0.12).abs() < f32::EPSILON);
        assert_eq!(s.spacing_px, 3);
        assert!(e.vignette.is_some() && e.glow.is_some() && e.bezel.is_some());
    }

    /// Clamps: fuera de rango SATURA, jamás error.
    #[test]
    fn valores_fuera_de_rango_se_clampan() {
        let t = norte_theme::Theme::from_toml(
            "[effects]\nscanlines = { opacity = 9.0, spacing_px = 1 }\nvignette = { strength = -3.0 }\n",
        )
        .unwrap();
        let e = EffectsV1::from_theme(&t).unwrap();
        let s = e.scanlines.unwrap();
        assert!((s.opacity - 0.35).abs() < f32::EPSILON, "opacity clampa a 0.35");
        assert_eq!(s.spacing_px, 2, "spacing clampa a 2");
        assert!(e.vignette.unwrap().strength.abs() < f32::EPSILON, "strength clampa a 0");
    }

    /// Clave desconocida o tipo malo: WARN + skip de ESA clave, el resto vive.
    #[test]
    fn clave_rota_degrada_por_clave_no_todo() {
        let t = norte_theme::Theme::from_toml(
            "[effects]\nscanlines = \"muchas\"\nvignette = { strength = 0.2 }\nfuturo = { x = 1 }\n",
        )
        .unwrap();
        let e = EffectsV1::from_theme(&t).expect("vignette sobrevive");
        assert!(e.scanlines.is_none(), "scanlines mal tipado: skip");
        assert!(e.vignette.is_some(), "vignette válido: vive");
    }
```

Run in norte-gui (`cd crates/norte-gui && cargo test effects`) — compile FAIL.

- [ ] **Step 2:** Implement:

```rust
//! Typed interpretation of the theme's opaque `[effects]` (ADR 0036, v1).
//! Lenient by contract (ADR 0020): a broken or future key WARNS and is
//! skipped — a theme is user data, never a startup error. All values clamp.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scanlines {
    /// Overlay line opacity, clamped [0.0, 0.35] (a11y: text must stay AA).
    pub opacity: f32,
    /// Distance between lines in px, clamped [2, 16].
    pub spacing_px: u8,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vignette {
    /// Edge-darkening strength, clamped [0.0, 0.6].
    pub strength: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glow {
    /// Fg brightening toward white, clamped [0.0, 1.0]; applied as
    /// `lerp(fg, white, strength * 0.25)` (ADR 0036 decision 3).
    pub strength: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bezel {
    /// Root corner radius in px, clamped [0, 32].
    pub radius_px: u8,
    /// Draw an inset shadow frame.
    pub inset: bool,
}

/// The interpreted v1 effects; every section optional.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EffectsV1 {
    pub scanlines: Option<Scanlines>,
    pub vignette: Option<Vignette>,
    pub glow: Option<Glow>,
    pub bezel: Option<Bezel>,
}

impl EffectsV1 {
    /// `None` when the theme declares no `[effects]` — the render path must
    /// then be byte-identical to pre-G1.
    pub fn from_theme(theme: &norte_theme::Theme) -> Option<Self> { ... }
}
```

`from_theme`: read `theme.effects` (`Option<toml::Value>`); for each known key, try to decode its sub-table (manual `Value` field reads — do NOT `Deserialize` the whole thing: per-key degradation is the contract); wrong type/shape → `tracing::warn!(key, "…skipped")` + `None` for that key; numeric values via `as_float()`/`as_integer()` then clamp. Unknown keys: single warn listing them. Return `Some(EffectsV1{..})` even if all keys ended `None` BUT the section existed? NO — decide: if `[effects]` exists but every key degraded, return `Some(default)` (empty effects, still "effects mode" but paints nothing) — simplest and honest; document. Add module to main.rs (`mod effects;`).

- [ ] **Step 3:** Tests green, clippy clean, fmt. Commit: `feat(gui): EffectsV1 interpreter — clamped, per-key lenient (G1)`

### Task 4: render integration — overlay, bezel, glow

**Files:** Modify `crates/norte-gui/src/main.rs` (NEVER stage the TaskKind::Index hunk — git add -p discipline)

- [ ] **Step 1:** Resolve once: `NorteGui` gains `effects: Option<effects::EffectsV1>` set where the theme is resolved (startup; also wherever the theme can change at runtime — grep for theme reassignment). Glow: in `ChromeColors::resolve` or immediately after, if glow present, apply the lerp to every fg field (bg fields untouched) + to `entry_color`'s result (thread `Option<Glow>` or pre-brighten via a helper `fn glowed(c: Rgba, g: Option<Glow>) -> Rgba`). Keep per-row cost zero: entry colors resolve per row already — the lerp is 3 mults, acceptable; do NOT re-resolve the theme per row.
- [ ] **Step 2:** Overlay. First VERIFY `gpui::canvas` exists at the pinned rev (`rg -n "pub fn canvas" ~/.cargo/git/checkouts/*/*/crates/gpui/src 2>/dev/null || cargo doc` — or simply try compiling a minimal canvas element). Then: after the modal overlay in `render()` (topmost), when `self.effects` has scanlines or vignette, push an absolute `inset_0` canvas that paints: horizontal lines every `spacing_px` at `rgba(0,0,0,opacity)`; vignette as N concentric alpha steps or 4 edge gradients (approximation fine — document). CRITICAL: the overlay must not capture input — verify per ADR 0036 decision 4: after wiring, run the app and confirm click-to-select and scroll still work (manual smoke REQUIRED, report evidence). If canvas is unavailable or captures input irreparably: implement scanlines/vignette as non-interactive `div` layers with gradients; if that also blocks input, STOP → BLOCKED with details.
- [ ] **Step 3:** Bezel: when present, root container gains `.rounded(px(radius))`-equivalent (check GPUI corner-radius API on div) and, if `inset`, an inset shadow or a 2px darker border — smallest honest approximation, document what GPUI offers.
- [ ] **Step 4:** Zero-cost pin: test `sin_effects_no_hay_overlay` — with default theme, `NorteGui`'s effects field is None (unit-level; the render-tree identity is enforced by construction: every effects branch is behind `if let Some`). Plus `retro_crt_activa_effects` (constructing the startup path with retro-crt theme yields Some with scanlines).
- [ ] **Step 5:** Manual smoke with `[ui] theme = "retro-crt"`: screenshot-level verification — scanlines visible, text legible, clicks work, no fps collapse (report subjective smoothness; GPUI debug overlay if available). Also `retro-crt-amber`.
- [ ] **Step 6:** `cargo test` + clippy + fmt (norte-gui). Commit: `feat(gui): render [effects] v1 — scanline/vignette overlay, bezel, glow (G1)`

### Task 5: gate + reviewers

- [ ] `just ci` (workspace: norte-theme changes ride it).
- [ ] rust-reviewer on the G1 range (ADR fidelity: clamps match ADR; overlay input-transparency; per-key degradation honest; preset completeness) + a11y-focused check: measured contrast of retro presets' regular/selection/status pairs (AA minimum under scanline worst-case: fg on bg darkened by opacity).
- [ ] Apply findings; final commit; update memory.

## Self-review notes

- Spec G1 coverage: ADR ✓ (T1), interpreter+clamps+lenient ✓ (T3), overlay/bezel root integration ✓ (T4), bundled retro-crt (+amber variant) ✓ (T2), a11y clamps ✓ (T3 clamps + T5 measured check), zero-cost-without-effects ✓ (T4). Palette key consciously dropped (T1 records it). Theme-picker exposure comes free via preset registration (T2).
- Uncertainty flagged, not hidden: canvas availability + input transparency at the pinned GPUI rev is T4's first verification with explicit fallbacks and a BLOCKED escape hatch.
- Type consistency: `EffectsV1` fields referenced in T4 match T3's definition; `from_theme` signature consistent across tasks.
