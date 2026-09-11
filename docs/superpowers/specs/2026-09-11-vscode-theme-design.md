# VSCode theme: presets, chrome roles, and a theme importer

- Date: 2026-09-11
- Status: approved (user request 2026-09-11: "quiero uno tipo vscode, pero
  especialmente en gui debería parecerse mucho, en todo — colores,
  tipografías, etc."; scope confirmed as all six phases)
- Related: ADR 0020 (shared semantic themes), ADR 0037 (plugin data-out v2,
  closed `role` vocabulary), ADR 0105 (icon column / WIT package bumps),
  ADR 0107 (window visual polish), `docs/theming.md`

## Problem

norte bundles eight theme presets. None of them is a VSCode theme, and the
window would not look like VSCode even if one existed: the theme model has no
vocabulary for the surfaces VSCode's appearance is actually built from.

Three concrete findings from the review:

1. **No VSCode preset.** `default`, `catppuccin-mocha`, `catppuccin-latte`,
   `gruvbox-dark`, `gruvbox-light`, `nord`, `retro-crt`, `retro-crt-amber`.
2. **The role vocabulary is too small for the window's chrome.** VSCode's look
   is surfaces at different elevations, not borders: list hover, input fields,
   widget/quick-pick backgrounds, badges, a distinctive overlay scrollbar,
   focus rings. `Role` has 18 variants and none of them names any of those.
   The scrollbar in particular is not styled at all today — the window shows
   WebKitGTK's, which is the single loudest visual tell.
3. **Three CSS variables are read but never fed.** `--warn-fg`
   (`style.css:2610`) is a misspelling of the `warning-fg` that Rust projects,
   so log-panel warnings silently ignore the theme and always fall back to
   `#fc6`. `--dim-fg` (`:1128`) and `--chip-bg` (`:2621,2629,2641`) have no
   producer at all.

Two further findings emerged while validating that the presets could be
transcribed rather than invented, and both change the design:

4. **A VSCode theme JSON is not a complete palette.** `dark_modern.json`
   carries `include: ./dark_plus.json`, which includes `dark_vs.json`; and
   even the three of them together define no `list.activeSelectionBackground`,
   `list.hoverBackground`, `scrollbarSlider.*`, `badge.*` or
   `descriptionForeground`. Those come from VSCode's **built-in colour
   registry** (`src/vs/platform/theme/common/colors/`: `baseColors.ts`,
   `listColors.ts`, `inputColors.ts`, `quickpickColors.ts`, `miscColors.ts`),
   which a theme only partially overrides.
5. **Therefore F1 and F5 are the same work seen twice.** An importer fed a
   marketplace theme that sets twenty keys would emit a norte theme with
   twenty colours and everything else on the monochrome fallback — which
   reads as broken, and the user would blame the importer. The importer needs
   a base layer per `"type": "dark" | "light"`, and that base layer is
   precisely what F1 produces. F1 must therefore transcribe the registry
   defaults too, not just the two theme files.

## Architecture

The colour chain does not change:

```
preset/JSON → Theme (Role→Style) → roles_de_tema() → applyTheme() → var(--x) → style.css
   F1,F5          F2                   F2               —             F2         F3
```

F1 and F5 produce `Theme`s. F2 widens the vocabulary and the projection. F3
spends the new variables. F4 runs on a separate track (WIT package + the
`file-icons` plugin).

One structural finding shapes F5: **`preset_names()` has seven callers**
(`norte-ui-host` wizard/settings/profiles, `norte-tui`
wizard/app::pickers/screens::settings). "Which themes exist" is written seven
times. The moment user themes exist, seven places diverge — the lesson already
recorded as *a shared function is not enough*. F5 introduces one
`norte_frontend::theme::available_themes()` and replaces all seven.

## F0 — the orphaned CSS variables

Independent bug fix, lands first and alone.

| variable | today | fix |
| --- | --- | --- |
| `--warn-fg` | read at `style.css:2610`; Rust projects `warning-fg` | rename in CSS to `warning-fg` |
| `--dim-fg` | read at `:1128`, no producer | fed by F2 (`muted`) |
| `--chip-bg` | read ×3, no producer | fed by F2 (`badge`) |

F0 fixes only `warn-fg`. The other two stay orphaned until F2 and that is
deliberate: inventing a producer for them now would guess at a role name that
F2 is about to choose properly.

**The test is the point of F0.** A test that collects every `var(--…)` in
`style.css`, subtracts the geometry variables (an explicit allow-list:
`cell-w`, `cell-h`, `menubar-h`, `panelbar-h`, `keybar-h`, `depth`,
`busy-delay`, `menu-left`, `menu-open`, `mono`, `ui-font`, `ui-font-size`,
`font-mono`, `font-ui`, `dialog-backdrop`), and asserts the remainder is
exactly the key set that `roles_de_tema()` produces for a theme that defines
every role. It fails on the next orphan in either direction — a variable
nobody feeds, or a colour nobody spends. A nested `var(--a, var(--b))`
(F2's derivation) counts both names, which is correct: both must be fed by
someone.

It needs one more list to go green at F0: `dim-fg` and `chip-bg` are known
orphans until F2 feeds them, so they enter as a second, explicitly named
allow-list — and **F2 deletes that list**, which is what makes F2 unable to
forget them. Two lists, not one, because they mean different things: geometry
is permanently not a colour, whereas these two are a colour that is missing.

The check belongs in `norte-ui-host`, whose `pickers.rs` owns the key set, and
reads `style.css` through a path relative to `CARGO_MANIFEST_DIR`.

## F1 — `vscode-dark` and `vscode-light`

Two presets, transcribed from Microsoft's sources, never invented:

| preset | theme file | base |
| --- | --- | --- |
| `vscode-dark` | `extensions/theme-defaults/themes/dark_modern.json` | `dark_plus.json` → `dark_vs.json` → registry defaults (dark) |
| `vscode-light` | `extensions/theme-defaults/themes/light_modern.json` | `light_plus.json` → `light_vs.json` → registry defaults (light) |

Dark Modern and Light Modern are VSCode's defaults since 1.75, so they are
what a reader actually sees today.

Resolution order for any colour id: the theme's own `colors`, then each
`include` in turn, then the registry default for that id at that base. Each
preset's header records which ids came from the registry rather than the
theme file, because a reader comparing our TOML against `dark_modern.json`
will otherwise find colours that are not there.

The colour ids that matter to us, and the role each lands in, are tabulated in
F2 below. That table is the single mapping: F1 applies it by hand to two
themes, F5 applies it mechanically to any theme.

Known risk: `las_senales_semanticas_llegan_a_wcag_aa_en_todo_preset` requires
4.5:1 against the theme background for `error`, `warning` and
`hostile-badge`. VSCode's warning yellow is weak. If a transcribed value fails
the gate, **raise the colour and record the divergence in the preset header** —
the test does not move. That policy is the same one the imported keymap
presets already use for chords their source does not attest.

Eight places name a preset and all eight change: `presets/*.toml`,
`presets.rs`, `tests/presets.rs`, `norte-config/src/schema.rs`,
`docs/schema/norte.schema.json`, `docs/theming.md`, `CHANGELOG.md`, and the
TUI theme-picker snapshot. `docs/theming.md` currently lists six of the eight
existing presets (the two `retro-crt` are missing); that is fixed in the same
pass.

## F2 — ten chrome roles and a requestable filter

Ten new `Role` variants, each with a `fallback()` so a theme written before
this change still resolves, and each projected by `roles_de_tema()`:

| role | VSCode id | consumer |
| --- | --- | --- |
| `hover` | `list.hoverBackground` | row under the pointer |
| `input-background` | `input.background` | text fields |
| `input-border` | `input.border` | text fields |
| `widget-background` | `editorWidget.background`, `quickInput.background` | palette, dropdowns, menus |
| `widget-shadow` | `widget.shadow` | the hard-coded `rgb(0 0 0 / 35%)` at `style.css:1234` |
| `badge` | `badge.background` / `badge.foreground` | counters, and the log panel's chips |
| `scrollbar-slider` | `scrollbarSlider.background` | the overlay scrollbar (F3) |
| `separator` | `panel.border` | chrome rules between surfaces |
| `focus-border` | `focusBorder` | focus ring of a control |
| `muted` | `descriptionForeground` | breadcrumbs, sizes, secondary cells |

Two decisions inside:

- **`badge` serves two consumers.** VSCode's counter and the log panel's chip
  (`--chip-bg`). That is the shape `Mark` already has — one role, two readings
  — and its rustdoc says so, rather than splitting off a near-duplicate
  `chip`.
- **`focus-border` is not `border-focus`.** `border-focus` is the border of
  the *pane* that has focus; `focus-border` is the ring of a *control*. The
  names are dangerously close, so each one's rustdoc names the other.

**The requestable filter.** `Role` is the closed vocabulary a plugin may name
in a span or a decoration (ADR 0037: `role: option<string>` in WIT, validated
host-side by `Role::from_kebab`). Adding a variant therefore makes it
plugin-requestable with no WIT bump — and a plugin painting a badge with the
scrollbar slider's colour is nonsense. So:

- `Role::ALL` keeps meaning *every* role (preset completeness still checks
  all of them).
- `Role::REQUESTABLE` is new: the semantic subset a plugin may name.
- ADR 0037's validation point consults `REQUESTABLE`. A non-requestable name
  degrades to `None` — the same degradation an unknown name already has, so
  the failure mode is one the code and the tests already describe.

This narrows what a plugin may ask for. Verified against our twelve bundled
plugins: they request only `info` and `title`, so nothing of ours changes. A
third-party plugin naming a chrome role loses a colour and keeps working.

The TUI needs no change — its bridge resolves roles generically. Roles it
cannot paint say so in their own rustdoc, the way `PaneBackground` already
does.

### What ten new roles do to the eight existing presets

`cada_preset_parsea_y_es_completo` asserts that **every** preset gives
**every** `Role::ALL` a colour. Ten new roles therefore demand eighty new
values across the eight existing presets, or the test goes red. That cost is
real and it is the reason this subsection exists rather than being discovered
by an agent halfway through.

It is not worth paying, because the monochrome `fallback()` is the wrong
default for a chrome role. A `hover` with no colour is not a conservative
hover — it is an invisible one, and `Style::new()` cannot know that nord's
hover should be bluish and gruvbox's brown.

So chrome roles **derive in the stylesheet** from the colours a theme already
has:

| chrome var | derives from | when the theme is silent |
| --- | --- | --- |
| `--separator` | `--border` | today's rule, unchanged |
| `--hover` | `--panel-focus-bg` | the surface a focused pane already uses |
| `--scrollbar-slider` | `--border` | visible, quiet |
| `--input-background`, `--widget-background` | `--panel-bg` | the pane surface |
| `--input-border` | `--border` | today's rule |
| `--focus-border` | `--border-focus` | the accent the theme already picked |
| `--muted` | `--title-fg` | what `--dim-fg` already falls back to |
| `--badge` | `--selection-bg` | the accent, which is what a badge is |
| `--widget-shadow` | `rgb(0 0 0 / 35%)` | today's hard-coded value |

Written as `var(--hover, var(--panel-focus-bg))`, one level, no JavaScript.

Consequently the completeness test splits: it keeps asserting totality over
the eighteen roles that exist today — renamed `Role::CORE` — and exempts the
chrome roles, with a comment saying they derive and naming this spec. The two
VSCode presets define all twenty-eight regardless, because for them the
chrome colours are the whole point.

The eight existing presets gain **nothing** and change appearance **nowhere**.
That is the test of whether the derivation table above is right: if adopting
it would visibly alter gruvbox, the default chosen is wrong.

## F3 — chrome, universal and theme-derived

No per-theme branches in the CSS. Four changes:

1. **Scrollbar.** Overlay, no arrows, slider from `scrollbar-slider`, appearing
   on hover. Replaces WebKitGTK's, which is the loudest tell that this is not
   VSCode.
2. **Row hover** from `hover`. Today only the mark checkbox reacts to the
   pointer.
3. **Elevation instead of borders.** Chrome rules move from `border-unfocused`
   to `separator`. A theme that wants a visible rule sets one; the VSCode
   presets set it near the background and get elevation for free. **No
   existing theme changes appearance**, by the derivation table in F2:
   `var(--separator, var(--border))` keeps painting today's rule for every
   theme that does not name `separator`.
4. **Typography.** A `Theme` carries no fonts and will not start to — that
   would fuse two independent choices. The VSCode type setup is reached
   through `[ui] font` / `mono_font` / `font_size`, which already exist and
   already win: 13px chrome, 14px cells. The first-run wizard and
   `docs/theming.md` say what to set; the presets' headers point at it.

## F5 — importer and user themes

Three pieces.

**The converter** lives in `norte-theme`, which already depends on
`serde_json`. It accepts VSCode theme JSON — JSONC, so comments and trailing
commas — resolves `include` relative to the file, and layers the result over
the base palette named by `"type"`: `vscode-dark` or `vscode-light` from F1.
That layering is what makes an imported theme complete rather than
twenty-coloured.

`tokenColors` is **ignored and said out loud**. norte does not colour syntax;
an importer that silently dropped half its input would be the third kind of
green test that proves nothing.

**A third branch in the resolver.** Today `[ui] theme` is an embedded preset
name or a path. A name that is neither now resolves against
`<config>/themes/<name>.toml` before being treated as a path. An imported
theme is then first-class: it appears in the F9 picker, in the wizard, and in
profiles, and `[ui] theme = "vscode-dracula"` works like any preset name.

Order matters and is fixed: **embedded preset → user theme directory → path**.
An embedded preset cannot be shadowed by a file, so a stale
`~/.config/norte/themes/nord.toml` cannot change what `nord` means.

**`available_themes()`** in `norte-frontend`, one function, replacing the
seven `preset_names()` callers. It returns embedded presets and user themes,
marked by origin so a surface can group them.

**Surface**: `ntc theme import <file.json> [--name X] [--use]`. It writes the
TOML and does not touch `[ui] theme` unless `--use` is given.

## F4 — icons

The heaviest phase, and separable.

- **File icons**: a Seti-style set (MIT) as a new style in the `file-icons`
  plugin, beside `nerd` and `ascii`.
- **Chrome icons**: Codicons (CC-BY-4.0). `deny.toml` does not allow
  CC-BY-4.0, so this needs a **scoped exception** with an attribution file
  beside the Nerd Fonts subset that already ships. CC-BY is permissive with an
  attribution condition, not copyleft; the exception is the same shape as the
  ones already recorded for `notify` and `webpki-roots`.
- If the decorator needs new data, this is a **WIT package bump** and carries
  ADR 0105's eleven-site checklist.

## Testing

- **F0**: the orphan-variable test described above.
- **F1**: preset completeness and the WCAG floor are already automatic — the
  two new presets simply enter the existing loops.
- **F2**: kebab round-trip over the ten new roles (the existing loop covers
  them via `Role::ALL`); a new test that a non-requestable role name degrades
  to `None` at ADR 0037's validation point; and that `REQUESTABLE` is a
  subset of `ALL`.
- **F3**: the orphan test from F0 now guards the new variables in both
  directions.
- **F5**: import→parse round-trip over a real theme fixture in the
  `norte-testkit` corpus; that an imported theme with twenty keys resolves
  every role; that `available_themes()` returns the same set in both
  frontends (a parity test, per ADR 0077).
- **F4**: per ADR 0105.

## Decision record

One new ADR, amending ADR 0020: the chrome roles, `Role::REQUESTABLE` (which
narrows the plugin vocabulary and so amends ADR 0037 too), and the third
branch of the theme resolver.

## Non-goals

- **Syntax colouring.** norte does not have it; `tokenColors` would tempt it.
- **VSCode's layout** — activity bar, editor tabs. norte is an orthodox
  two-pane manager. Copying that layout would be copying the wrong program;
  only the finish is in scope.
- **New GPU effects.** The `[effects]` block stays as ADR 0036 left it.
- **Fonts inside a `Theme`.** Kept in `[ui]`, where they already are.

## Phasing and gate budget

Six tranches, in order: F0, F1, F2, F3, F5, F4. F5 depends on F1 (the base
palette) and F2 (the roles it maps onto); F4 depends on nothing here.

Branches: `fix/theme-css-vars-huerfanas` (F0), `feat/vscode-theme` (F1–F3),
`feat/theme-import` (F5), `feat/theme-icons` (F4).

Gate: `just t <crate>` inside the RED→GREEN loop; **one `just ci-fast` every
~3 tranches** (two in total); **one `just ci`** before the merge.
