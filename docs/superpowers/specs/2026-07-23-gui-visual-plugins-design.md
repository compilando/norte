# GUI plugins — visual effect packs, data-out WASM surfaces, shader research

- Date: 2026-07-23
- Status: approved (scope: full — G0 through G3, G4 as a research spike)
- Related: ADR 0020 (`[effects]` reservation), ADR 0022 (plugin host), ADR 0027
  (GPUI), ADR 0032 (provider WIT), spec §7.1/§17,
  `docs/superpowers/specs/2026-07-23-help-config-system-design.md` (C2/H1/P1)

## Problem

The GUI is a minimal flexbox renderer: solid colors, no shadows, gradients,
animations, or canvas; repaint is event-driven only (`norte-gui/src/main.rs`
`render()` is the single draw entry). Its chrome colors are hardcoded consts
outside the theme. Meanwhile ADR 0020 reserved an opaque `[effects]` theme
section explicitly for GUI-interpreted "gradients, glow, or animation", and
`norte-theme` already parses and stores it untyped (`Theme.effects`,
`has_effects()`) — nothing interprets it. WASM guests have no rendering
capability: previewer returns a plain string the host paints; `columns`/`hook`
exist in the manifest but have no WIT. The user wants plugins "of every kind"
for the GUI, with visual packs (retro-CRT terminal aesthetics) as the
flagship.

## Governing principle

**A visual plugin is data the host interprets, never third-party code that
paints.** Three tiers:

1. **Data packs** — theme + `[effects]` + icon/palette files. No code, no
   sandbox needed; values clamped by the host. This is where retro-CRT lives.
2. **WASM data-out** — guests return structured data (styled spans, badges,
   column values); the host renders it. Covered by the existing ADR 0022
   sandbox unchanged.
3. **Guests that draw** — a canvas/draw API for guests. Rejected for now:
   enormous surface, GPUI has no stable public shader/post-processing
   pipeline. Only revisited if G4 research changes the facts.

This preserves rule 9 (plugins never touch the UI/filesystem directly) and
the ADR 0022 tier separation without opening any new capability.

## GPUI feasibility facts (verified 2026-07-23)

GPUI's shaders are internal per-primitive; there is no public post-processing
pipeline. Public API does offer: `canvas()`, box shadows, gradients, opacity,
`with_animation`. Therefore:

- **Feasible now**: phosphor palettes; scanline overlay (one global
  semi-transparent layer — global, not per-row, for frame budget); vignette
  (layered gradients / canvas); bezel frame (borders, rounded corners, inset
  shadow); approximate glow (brightened colors + bloom-ish overlay; real text
  shadow is not exposed); subtle flicker and cursor blink (`with_animation` +
  an opt-in timer).
- **Not feasible without forking GPUI or adding a custom wgpu layer**: screen
  curvature, true shader bloom, per-pixel distortion. G4 investigates; no
  commitment.

## Phase G0 — prerequisites (partially planned elsewhere)

- Help/config phase C2 lands first: the GUI reads `[ui].theme` via
  `norte-config` (today the theme is hardcoded to the default preset).
- **Chrome through the theme**: the 11 hardcoded consts (`BG`, `PANE_BG`,
  `SEL_BG`, `BORDER_FOCUS`, …) move to `norte_theme::Role` lookups
  (`Background`, `Selection`, `BorderFocus`, `BorderUnfocused`, `StatusBar`,
  …; add missing roles as needed — `Role` is `#[non_exhaustive]`, additive).
  Without this no skin can touch backgrounds or borders. The TUI is
  unaffected (same roles, same fallbacks).
  **Amendment (2026-07-23, discovered in implementation):** 8 of the 11
  historic GUI constants diverge from `default.toml`, whose values are
  TUI-designed style PAIRS (e.g. `status-bar` and `match` are
  dark-text-on-colored-bg idioms). Aligning `default.toml` to the GUI's
  historic hex would repaint the TUI; keeping the GUI pixel-identical would
  need parallel GUI-only roles. Decision — same principle as the shared
  keymap presets: **the theme is canonical**. The GUI consumes roles as
  honest fg+bg pairs (header = `StatusBar` pair, quick-search highlight =
  `Match` pair, selection = `Selection` pair) and its default appearance
  becomes the default theme's appearance, unifying visual identity across
  frontends (the stated goal of ADR 0020). The historic constants remain
  only as fallbacks for channels a theme leaves undeclared.

## Phase G1 — `[effects]` schema v1 + retro-CRT preset (static)

**ADR required** (defines the schema ADR 0020 deferred). Schema v1, all
static, all values clamped by the interpreter:

```toml
[effects]
# Every key optional; unknown keys WARN and are ignored (lenient per ADR
# 0020 — a newer theme must not break an older GUI).
scanlines = { opacity = 0.12, spacing_px = 3 }        # opacity clamp [0, 0.35]
vignette  = { strength = 0.3 }                        # clamp [0, 0.6]
glow      = { strength = 0.4 }                        # brightened-color approximation
bezel     = { radius_px = 12, inset = true }
palette   = "phosphor-green"                          # or "phosphor-amber", or absent
```

- Interpreter lives in `norte-gui` (a `effects.rs` module): parses
  `Theme.effects` (the stored `toml::Value`) into a typed, clamped
  `EffectsV1` struct; malformed values degrade per-key to "off" with a
  warning, never a startup error (a theme is user data, not structural
  config — same philosophy as hotlist entries).
- Render integration: one overlay layer at the root of `render()` (scanlines
  + vignette), bezel on the root container, palette applied through the
  existing `to_gpui_rgba` seam. Zero cost when `has_effects()` is false —
  the current render path must be byte-identical without effects.
- **Bundled preset `retro-crt`** (green; `retro-crt-amber` variant): a theme
  file with phosphor `[files]`/roles plus the `[effects]` block. Ships next
  to the existing presets; selectable via `[ui].theme` and the theme picker.
  It doubles as the reference "visual plugin": installing a third-party pack
  is copying a `.toml` into the user config dir.
- A11y guard: effects never reduce text contrast below the theme's own
  colors — scanline/vignette opacity clamps are chosen so worst-case overlay
  keeps AA contrast on the default presets; high-contrast themes may declare
  `[effects]` absent and the GUI adds none.

## Phase GP — typography & look-and-feel polish (added 2026-07-24)

User-requested: the GUI "está muy poco trabajado" — it uses GPUI's default
font everywhere, a fixed row height, and improvised paddings. Goal: a
VSCode-grade baseline of visual craft. Scope:

- **Typography**: file listings, viewer, and task strip render in a
  MONOSPACE font (file names and hex dumps are tabular data); chrome
  (headers, modals, banners) in a UI font. New optional `[ui]` settings in
  `norte-config`: `font` (UI family), `mono_font` (listing family),
  `font_size` (base px, clamped). Absent = platform-sensible defaults with
  a documented fallback chain; an uninstalled family falls back silently
  (fontconfig) — `doctor`/banner may warn later, not in scope. Schema
  golden regenerates (additive optional fields).
- **Metrics rhythm**: row height derived from the resolved line height
  instead of a hardcoded const; consistent spacing scale (one small set of
  spacing constants used everywhere: pane padding, header padding, gaps);
  header/status strips get breathing room; viewer line height tuned for
  density.
- **Fine detail**: hover state on rows (subtle bg shift via theme-derived
  color, no new roles — computed from existing ones); pointer cursor on
  clickable rows; focused-pane border treatment kept but aligned to the
  spacing scale; modal gets consistent padding/radius; selected row corner
  rounding consistent with the bezel aesthetic; truncation behavior
  verified with the hostile-names corpus (no regression).
- Non-goals here: animations (G2), icons, ligature config, per-theme font
  overrides (a theme sets colors, not fonts — fonts are user ergonomics,
  config not theme; recorded as a decision).

Testing: chrome/metrics helpers unit-tested; hostile-name rendering pinned;
manual smoke with screenshots on default + retro-crt + nord.

## Phase G2 — motion (opt-in)

- A frame timer exists only while at least one animated effect is active and
  the window is focused; otherwise the GUI stays purely event-driven.
- Schema additions (`v1.1`, still lenient): `flicker = { strength = 0.05 }`
  (clamped tiny), `cursor_blink = true`, `fade_ms = 120` (pane focus/modal
  transitions).
- **`reduce_motion`**: a `[ui] reduce_motion = true` setting (norte-config,
  System+User layers) that force-disables all motion regardless of theme —
  accessibility requirement (spec §17); default follows the platform hint
  when detectable, else false.
- Perf budget: animated overlays must not regress the ADR 0027 measured
  criteria; scanlines/vignette are static layers (no per-frame recompute),
  flicker animates one opacity value.

## Phase G3 — WASM data-out surfaces (WIT v2)

Protocol/WIT changes, protocol-guardian mandatory. All guest output remains
data; the host paints with untrusted-text masking everywhere.

- **Styled previews**: new WIT `render-styled(input) -> result<styled-text,
  string>` where `styled-text = list<span>`, `span = { text: string, role:
  option<string>, fg: option<color> }` — roles resolve through the theme;
  raw colors clamp. The plain `render` stays for old guests (per-category
  world debt P2b is a good moment). The GUI viewer paints spans with real
  colors; the TUI maps them to its cell styles. Syntax-highlighting
  previewers finally render in color.
- **Row decorators**: new WIT `decorate(entries) -> list<decoration>`,
  `decoration = { badge: option<string>, role: option<string> }` — e.g. git
  status badges. Batched per listing (one call per visible page, not per
  row); epoch budget applies; a dead plugin never blocks the listing (same
  fallback contract as the previewer).
- **Columns**: give the already-declared `columns` contribution its WIT
  (`column-values(id, entries) -> list<string>`), rendered as extra pane
  columns in the GUI (and later TUI).
- **GUI command palette + extension manager**: the GUI gains the palette
  (shares H1's design: COMMANDS join + plugin `CommandContrib.title`/P1
  `description`) and a minimal extension manager view (approve/enable —
  parity with the TUI's F12, same human-only governance via `plugin.*`).
- Wire impact: rides the help/config P1 proto bump where possible; new
  `plugin.preview_styled` (or a capability flag on `plugin.preview`) —
  decide in the phase ADR with protocol-guardian.

## Phase G4 — research spike: real shaders (no commitment)

Time-boxed investigation, outcome is a written report + decision, not code:

1. Can a custom wgpu render pass wrap GPUI's output at our pinned rev
   (render-to-texture + post shader) without forking GPUI?
2. Cost of maintaining a GPUI fork with a post-processing hook (rebase burden
   against Zed's pace vs. the pinned-SHA policy of ADR 0027).
3. Alternative: ship curvature/bloom as "not worth it" and close tier 3
   permanently.

Exit: an ADR recording the decision. Only if (1) or (2) is cheap does a G5
appear.

## Governance & security (cross-cutting)

- Data packs are not code: no approval flow, but hard clamps on every
  numeric value, no paths/URLs in `[effects]`, and theme names/strings are
  untrusted text (masked in pickers).
- Icon packs (if added later to tier 1) treat SVG as hostile input: size
  caps, no external references; decode failures degrade to the default icon.
  Not in G1 scope.
- WIT v2 outputs are data with the same 4 MiB return cap; span/decoration
  counts are bounded per call; all strings masked before painting.
- Nothing in this program grants guests exec, network, or new FS access.

## Testing (cross-cutting)

- G1: EffectsV1 parse/clamp matrix (hostile values, unknown keys warn-only);
  render smoke with effects on/off asserting the no-effects tree is
  unchanged; contrast assertions for the clamps; `retro-crt` preset parses
  and `has_effects()`.
- G2: timer lifecycle (starts with animation, stops on blur/none),
  reduce-motion force-off, budget bench.
- G3: WIT round-trip with hostile spans (bidi, kilometric, non-UTF8-adjacent
  handling per encoding corpus), dead-plugin fallback, batching bounds;
  goldens for any proto change.
- G4: report only.

## Order and dependencies

G0 (C2 + chrome-to-roles) → G1 (ADR + interpreter + retro-crt) → G2 (motion)
→ G3 (WIT v2 + palette/manager; shares work with help/config H1/P1) → G4
(spike, can run parallel to G3). Each phase is its own plan; G1 and G3 carry
ADRs; G3 carries protocol-guardian + security review; encoding-auditor on
everything that paints guest strings.

## Out of scope

- Tier 3 (guests drawing) — unless G4 flips the facts.
- Icon packs (tier-1 follow-up after G1).
- TUI rendering of `[effects]` (ignores them by design, ADR 0020).
- Lua-driven visuals.
