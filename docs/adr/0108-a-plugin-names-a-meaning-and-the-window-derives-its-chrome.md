# 0108 — A plugin names a meaning, and the window derives its chrome

- Status: accepted
- Date: 2026-09-13
- Decision makers: Oscar González
- Amends: ADR 0020 (shared semantic themes), ADR 0037 (plugin data-out v2)
- Related: ADR 0066 (frontend boundary), ADR 0077 (TUI/window parity),
  spec `docs/superpowers/specs/2026-09-11-vscode-theme-design.md`

## Context and problem statement

norte shipped eight theme presets and none of them was a VSCode theme. Adding
one exposed three problems that the theme model could not express, and one
contract that adding it would quietly break.

**The role vocabulary was too small for a graphical window.** A modern
editor's appearance is built from surfaces at different elevations — list
hover, input fields, widget and menu backgrounds, badges, an overlay
scrollbar, control focus rings — and `Role` had eighteen variants naming none
of them. The scrollbar in particular was not styled at all, so the window
showed WebKitGTK's: system chrome inside a window that paints everything else
itself.

**Adding roles silently widens what a plugin may ask for.** ADR 0037 makes
`role` a string on the WIT boundary, validated host-side against
`norte_theme::Role`. Every new variant therefore became requestable by a
guest with no WIT bump and no review — including names like
`scrollbar-slider`, which mean nothing attached to a filename.

**Half of a theme file was invisible in the window.** The terminal has painted
`[files.kind]` and `[files.ext]` since ADR 0020; `norte-ui-host` never called
`file_style` at all. Every entry came out the same colour, which does not read
as a plain theme — it reads as a broken one.

**A VSCode theme JSON is not a complete palette.** `dark_modern.json` includes
`dark_plus.json`, which includes `dark_vs.json`, and none of the three defines
`list.activeSelectionBackground`, `list.hoverBackground`, `scrollbarSlider.*`,
`badge.*` or `descriptionForeground`. Those are defaults registered in the
editor's own colour registry. A transcription that copied only the JSON would
ship a theme with no cursor and no scrollbar.

## Decision 1 — chrome roles, derived rather than defaulted

`Role` gains ten variants: `hover`, `input-background`, `input-border`,
`widget-background`, `widget-shadow`, `badge`, `scrollbar-slider`,
`separator`, `focus-border`, `muted`.

### Options

**(a) Require every preset to define them.** `Role::ALL` stays one set and
preset completeness keeps asserting totality. Simple rule, and every theme is
explicit about its chrome. Costs eighty invented values across the eight
existing presets, written by someone who did not design those palettes.

**(b) Give them a colour in `Role::fallback`.** No preset changes. But the
fallback is a single literal shared by every theme, and the sensible default
for `hover` is "the focused pane's background of THIS theme" — a value that
cannot be written as a constant.

**(c) Derive them in the stylesheet, from a colour the theme already has.**
`var(--hover, var(--panel-focus-bg))`, one level, at the point of use.

### Decision

Option (c). `Role::CORE` is the eighteen a preset must colour and what
completeness asserts; `Role::ALL` stays the whole vocabulary. The chrome roles
keep a colourless `fallback`, and the window's stylesheet derives each from a
colour the theme already carries.

The reason is that the correct default is a *function of the theme*, not a
constant, and the only place where both colours are in scope at once is the
stylesheet. The test of whether the derivation table is right is that adopting
it changes no existing preset's appearance — which is exactly what was
verified.

## Decision 2 — `Role::REQUESTABLE`: a plugin names a meaning

The set a plugin may name in a `SpanWire` or a `DecorationWire` is
`Role::REQUESTABLE`: `regular`, `title`, `hostile-badge`, `error`, `warning`,
`info`, `match`, `badge`, `muted`. `Role::from_kebab_requestable` is the
single entry point, and both of ADR 0037's validation sites call it.

**This narrows ADR 0037**, which said "`Role`'s closed set". The criterion is
one line: *a plugin describes CONTENT*, so it may name what a piece of content
MEANS, and may not name what the window uses to say what STATE it is in. That
excludes the ten chrome roles, and also `selection`, `selection-unfocused`,
`status-bar`, `mark`, `background`, the pane backgrounds, the pane borders and
`button`. A badge painted with the scrollbar slider's colour means nothing; a
badge painted with the cursor's colour would lie about where the cursor is.

A non-requestable name degrades to `None`, which is the degradation ADR 0037
already specified for an unknown name — so a guest built against a newer or
forked norte still cannot break an older one's render. No WIT bump: the type
was and remains `option<string>`.

Verified against the twelve bundled plugins: they request only `info` and
`title`.

## Decision 3 — an entry's colour is resolved by the host and travels in its row

`RowView` carries `name_color`, `name_bold`, `name_dim`, `name_italic` and
`name_underline` (bridge 66).

### Options

**(a) Send a rule name and let the renderer resolve it**, as `badge_role`
does. Impossible: roles are a closed vocabulary but extensions are an open one
— a theme colours whatever it likes — so there is no set of classes the
renderer could know in advance.

**(b) Project per-kind colours as CSS variables.** Covers `[files.kind]` and
not `[files.ext]`, for the same reason.

**(c) Resolve in the host, per row.** Costs one small string per visible row,
on a path that already allocates several.

### Decision

Option (c), matched against the **raw name bytes** (rule 1) and never the
painted name: masking is not injective, so an extension matched on the display
string can be some other file's extension.

Four of `Style`'s six attributes cross. `bg` and `reverse` stay behind
deliberately: a row's background is already claimed by the cursor, the hover
and the mark, and a fourth claimant would let a theme hide where the cursor
is. That omission is written in the bridge register and in `docs/theming.md`
rather than left to be discovered.

Under the cursor the selection's foreground wins and the entry colour is not
applied — replicating ratatui's `highlight_style`, which patches over an
item's own style whenever the theme gives `selection` a foreground, as all ten
presets do.

## Decision 4 — the desktop's colour scheme crosses the bridge

`set_color_scheme { dark }` is a new action (bridge 67), sent on startup and
on every `prefers-color-scheme` change. `HostTheme` keeps the variant
`Theme`s, not only their projected variables, and resolves an entry's colour
against the variant the renderer is painting.

This is a consequence of Decision 3 and not an independent feature. Variant
themes (V6) previously reached the renderer as CSS variables only, and the
renderer swapped them by itself — which was correct while all colour lived in
variables. Once an entry's colour is baked into its row and resolved by the
host, the host must know the scheme, or it paints the chrome from one variant
and the names from the other: with `theme_dark = "vscode-dark"` and
`theme_light = "vscode-light"`, a light desktop got `dir` at `#4daafc` on
white, 2.6:1.

The rule "that side's variant if present, else `theme`" is written on both
sides — `themeFor` in the renderer, `HostTheme::para_esquema` in the host —
because each needs something the other cannot supply: the renderer needs the
variables synchronously (going through the host would flash the wrong
palette), the host needs the whole `Theme` for `[files.ext]`. The duplication
is pinned by a test over all three cases and a cross-reference in each.

## Decision 5 — the presets are transcriptions, and divergences are written down

`vscode-dark` and `vscode-light` transcribe Dark Modern and Light Modern,
their `include` chains, **and the editor's built-in colour registry** for the
ids no file in the chain defines. Each preset's header records which colours
came from the registry and which are derived.

Where a transcribed value cannot be used, the value is changed and the
original is named in the header — never the reverse. Three did not survive:
on white, VSCode's `errorForeground` gives 3.35:1 and its `editorWarning`
3.12:1, both under the 4.5:1 that
`semantic_signals_reach_wcag_aa_in_every_preset` requires of the
signals a reader must read when something has gone wrong. The test does not
move. This is the same discipline the imported keymap presets already use for
chords their source does not attest.

## Consequences

### Positive

- The window is themeable where it was not: scrollbar, hover, widgets,
  inputs, focus rings, and the entry colours that were half of every theme
  file.
- The eight existing presets gain nothing and change nowhere.
- A plugin's vocabulary now means something: every requestable role is a
  statement about content.
- Two guard tests keep the stylesheet and the host's projection honest in both
  directions — a variable nobody feeds, and a colour nobody spends.

### Negative

- A theme author has twenty-eight roles to learn instead of eighteen, and the
  ten new ones behave differently on omission. `docs/theming.md` carries the
  derivation table for exactly this reason.
- A third-party plugin naming a non-requestable role loses a colour it used to
  get. It keeps working; nothing of ours was affected.
- The derivation lives in CSS, so a future non-webview renderer must
  reimplement that table rather than inherit it.
- The variant rule is written twice. A test pins it, but it is duplication,
  and the honest reason is a startup flash we were not willing to pay.
- `[files.kind]`'s `executable`, `fifo`, `socket`, `block-device` and
  `char-device` keys remain dormant in every preset: the protocol's `Entry`
  carries no mode, so no frontend can select them.
