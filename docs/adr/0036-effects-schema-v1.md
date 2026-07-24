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

## Amendment (2026-07-24, phase G2 — schema v1.1)

Phase G2 (`docs/superpowers/plans/2026-07-24-g2-motion.md`) begins landing
the motion extension this ADR pre-authorized above. Its Task 1 extends
`norte-gui`'s `EffectsV1` (`crates/norte-gui/src/effects.rs`) with three
more keys, under the exact per-key-lenient contract of decision 2 — same
clamp-never-error numeric treatment, same per-key wrong-type degrade-and-warn,
same "unknown key" leniency for anything still newer:

| Table     | Key            | Type  | Range        | Default | Status                              |
| --------- | -------------- | ----- | ------------ | ------- | ------------------------------------ |
| `flicker` | `strength`     | f32   | `[0.0, 0.15]`| `0.05`  | schema landed (T1); rendered in T2   |
| —         | `cursor_blink` | bool  | —            | —       | schema landed (T1); rendered in T2   |
| —         | `fade_ms`      | int   | `[0, 400]`   | —       | schema landed (T1); **parsed only**  |

`flicker` mirrors `vignette`'s shape (a sub-table with one clamped `f32`
subfield); `cursor_blink` and `fade_ms` are bare scalars directly under
`[effects]`, not sub-tables — there is no "table present, subfield absent"
default-substitution case for them (see the module's rustdoc), so absent
means [`None`], not a default value.

`flicker.strength`'s range is deliberately tiny — `[0.0, 0.15]`, less than
half of `scanlines.opacity`'s ceiling — for the same accessibility reason
class as decision 2's AA-contrast guard: unbounded flicker risks a
photosensitive trigger, not just an ugly overlay. GPUI's native
`with_animation` + `cx.reduce_motion()` (verified at rev f14fea9,
`animation.rs:161-168`) mean every animated effect this schema can produce
is force-static under `reduce_motion`, both through GPUI's own gate and
through a second `cx.reduce_motion()` check directly in `norte-gui`'s frame
loop as a belt-and-suspenders (decision 3 of the G2 motion plan).

**`fade_ms` is schema-complete but not yet rendered.** G2's plan (decision 1
of `docs/superpowers/plans/2026-07-24-g2-motion.md`) deferred wiring
modal/pane fade transitions: touching every overlay's show/hide path for a
marginal visual payoff was judged out of scope for the milestone that ships
flicker and cursor blink. The field parses and clamps correctly today — a
theme author can set `fade_ms = 200` and the value round-trips through
`EffectsV1` — but no GPUI element currently reads it to animate an opacity
ramp. This is a recorded, deliberate schema/implementation gap, not an
oversight; closing it is a follow-up, not a re-opening of this ADR.

**`reduce_motion` accessibility override.** T1 also lands the configuration
side: `norte-config`'s `[ui]` section (`crates/norte-config/src/schema.rs`)
gains `reduce_motion: Option<bool>`, merged last-wins across every
configuration layer including Project — the same presentation-only class as
`ui_theme`/`ui_lang`, not the security-sensitive fail-closed carve-out
`[archive]`/`[ai]`/hotlist get (`CommonConfig::ui_reduce_motion` in
`crates/norte-config/src/load.rs`). Task 2 wires the GUI side: `norte-gui`
will read it once at startup and call GPUI's `App::set_reduce_motion`
(`app.rs:1016` at the verified rev) — GPUI then kills all `with_animation`
motion for every element for free, with no per-effect plumbing required, and
Task 2's direct `request_animation_frame` path adds a second
`cx.reduce_motion()` check as a belt-and-suspenders on top of GPUI's own
gate. GPUI itself exposes no platform "prefers reduced motion" query at this
revision, so an absent config value takes the "else false" branch (motion
allowed) rather than an OS-level default — documented here since it is a
capability gap in the upstream library, not a norte design choice.
