# 0036 - GUI effects schema v1 for theme `[effects]`

- Status: accepted
- Date: 2026-07-23
- Decision makers: Oscar González
- Related: ADR 0020 (shared semantic themes and terminal colour fallback);
  design spec `docs/superpowers/specs/2026-07-23-gui-visual-plugins-design.md`
  (phase G1)

## Context

ADR 0020 reserved a theme `[effects]` section as opaque and lenient, pending
the GUI milestone: "the GUI may interpret gradients, glow, or animation" for
GPU-backed rendering that the terminal frontend cannot use. `norte-theme`
already parses and stores the section untyped — `Theme.effects: Option<toml::
Value>` and `Theme::has_effects()` — with no interpreter on either side; the
TUI ignores it by design, pinned by an existing test in
`crates/norte-theme/tests/model.rs`.

Phase G0 of the GUI-plugins spec landed GUI chrome through theme roles: the
11 previously hardcoded GUI color consts now resolve through
`norte_theme::Role`, using the same pair-honest `ChromeColors` idiom as the
TUI (status bar, selection, and match rendered as fg+bg pairs, not isolated
hexes). That work makes the GUI a real theme consumer for the first time and
is the prerequisite this ADR builds on.

The GUI-plugins spec (phase G1) locks the governing principle for all visual
plugins: **a visual plugin is data the host interprets, never third-party
code that paints.** The bundled retro-CRT presets are the flagship instance —
a `[effects]` block shipped inside an ordinary theme `.toml`, no code, no
sandbox. That block needs a typed, clamped schema before any GUI code can
interpret it safely. This ADR defines schema v1.

## Decision

### 1. Schema v1 keys

All keys are optional; the section itself is optional. All values in v1 are
**static** — no animation, no frame timer. Motion is deferred to phase G2
under an extended schema version.

| Table       | Key          | Type  | Meaning                                   |
| ----------- | ------------ | ----- | ------------------------------------------ |
| `scanlines` | `opacity`    | f32   | Global overlay opacity                     |
| `scanlines` | `spacing_px` | int   | Vertical spacing between scanlines, pixels |
| `vignette`  | `strength`   | f32   | Edge-darkening overlay strength            |
| `glow`      | `strength`   | f32   | Foreground-brightening amount              |
| `bezel`     | `radius_px`  | int   | Root container corner radius, pixels       |
| `bezel`     | `inset`      | bool  | Whether the bezel renders an inset shadow  |

```toml
[effects]
scanlines = { opacity = 0.12, spacing_px = 3 }
vignette  = { strength = 0.3 }
glow      = { strength = 0.4 }
bezel     = { radius_px = 12, inset = true }
```

### 2. Clamps and per-key leniency

| Key                    | Range          |
| ----------------------- | -------------- |
| `scanlines.opacity`    | `[0.0, 0.35]`  |
| `scanlines.spacing_px` | `[2, 16]`      |
| `vignette.strength`    | `[0.0, 0.6]`   |
| `glow.strength`        | `[0.0, 1.0]`   |
| `bezel.radius_px`      | `[0, 32]`      |

Every subfield above is independently optional. When a known key's table
(`[effects.scanlines]`, etc.) is present but a given subfield is absent, the
subfield falls back to its documented default below, which is then clamped
like any explicit value:

| Subfield                | Default |
| ------------------------ | ------- |
| `scanlines.opacity`      | `0.1`   |
| `scanlines.spacing_px`   | `3`     |
| `vignette.strength`      | `0.3`   |
| `glow.strength`          | `0.4`   |
| `bezel.radius_px`        | `10`    |
| `bezel.inset`            | `false` |

The `scanlines.opacity` upper bound is an accessibility guard, not an
aesthetic choice: at the worst case (maximum opacity, default-preset text
colors) the overlay must still keep AA text contrast. The other ranges bound
GPU cost and keep the effect recognizably subtle rather than a new visual
language.

Out-of-range numeric values clamp to the nearest bound; they do not warn or
get skipped, since a slider-adjacent typo (`opacity = 2.0`) is ordinary user
data, not malformed data. Wrong-typed values (a string where a float is
expected) and unknown keys inside `[effects]` warn and are skipped **per
key** — never a startup error. This extends ADR 0020's lenient contract: a
theme is user data, and a newer theme (or a hand-edited one) must not break
an older GUI build. If `[effects]` is present but every key degrades this
way, the GUI treats the theme as if `[effects]` were absent — it paints no
overlay, no bezel, no glow.

### 3. No `palette` key

The GUI-plugins spec's draft schema included a `palette = "phosphor-green"`
key. This ADR deliberately omits it. A phosphor palette is exactly what the
theme's own `[roles]` and `[files]` sections already express — foreground,
background, and per-extension colors — and the bundled `retro-crt` /
`retro-crt-amber` presets carry their green/amber identity directly through
those sections, not through a side channel. A dedicated `palette` key inside
`[effects]` would duplicate a mechanism the theme format already has, for no
capability gain. It is deferred until a concrete use case needs a named
palette independent of `[roles]`/`[files]` — none exists today.

### 4. Glow v1 is foreground brightening, not shader bloom

`glow.strength` is interpreted as:

```
lerp(fg, white, strength * 0.25)
```

applied to chrome and entry foreground colors. This is the "approximate
glow" the GUI-plugins spec's GPUI feasibility survey found achievable today
(brightened colors, no text-shadow primitive exposed by GPUI). Real
shader-based bloom is explicitly out of scope for v1 and is phase G4's
research question (custom wgpu post-processing layer, or forking GPUI); this
ADR makes no commitment either way.

### 5. Overlay input transparency and TUI exclusion

The scanlines+vignette overlay is a purely visual layer painted at the root
of `render()`. It **must never intercept input** — clicks, drags, and hover
state must pass through to the underlying UI exactly as if the overlay did
not exist. This is a hard requirement on the GUI implementation, not a
schema constraint, but it is recorded here because it is a correctness
property of every effect this schema can produce.

`[effects]` is GUI-only. The TUI ignores the section entirely, unchanged
from ADR 0020 and pinned by the existing `norte-theme` test that asserts
`has_effects()` is true while the TUI's own rendering path never consults
the value.

## Consequences

- `Theme.effects` stays untyped (`Option<toml::Value>`) in `norte-theme`.
  The typed, clamped interpretation (`EffectsV1` and its parser) lives in
  `norte-gui`, not in the shared theme crate — `norte-theme` stays
  renderer-agnostic per ADR 0020, and a TUI build never links effects-parsing
  code it cannot use.
- Because unknown and wrong-typed keys degrade per-key rather than failing
  the whole section, a theme written for a future schema version (v1.1's
  motion keys, phase G2) still renders its v1 keys correctly in an older GUI
  build; only the keys the older build does not understand are dropped, with
  a warning.
- The bundled `retro-crt` and `retro-crt-amber` presets become the reference
  "visual plugin": installing a third-party visual pack is copying a `.toml`
  file into the user config directory, exercising exactly this schema.
- `docs/theming.md` documents the schema, clamps, and per-key leniency as
  user-facing reference material; this ADR is the design record.
- Phase G2 will extend the schema with motion keys (`flicker`,
  `cursor_blink`, `fade_ms`) under the same lenient, per-key-degrading
  contract, plus a `reduce_motion` accessibility override — not part of this
  decision.
