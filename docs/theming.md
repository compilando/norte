# Themes

norte uses themes to style its terminal interface. The shared model lives in
`norte-theme` and is also available to the GUI. See ADR 0020 for the design
decision.

## Choose a theme

### Use the theme picker

Press **F9** to open the picker. Moving the cursor previews each bundled theme,
and each of your own in the themes directory (below), immediately. Press **Enter** to apply the selected theme and save it to the
`[ui].theme` key in your user `norte.toml`; comments and formatting in the file
are preserved. Press **Esc** to restore the previous theme without saving.

### Edit the configuration

Set `theme` in the `[ui]` section of `norte.toml`:

```toml
[ui]
theme = "catppuccin-mocha"
```

The value may be either:

- A bundled preset. There are ten:

  | preset | |
  | --- | --- |
  | `default` | neutral dark, tied to no brand |
  | `vscode-dark` | Visual Studio Code's Dark Modern |
  | `vscode-light` | Visual Studio Code's Light Modern |
  | `catppuccin-mocha`, `catppuccin-latte` | dark and light |
  | `gruvbox-dark`, `gruvbox-light` | dark and light |
  | `nord` | dark |
  | `retro-crt`, `retro-crt-amber` | dark, and they declare GPU `[effects]` the terminal ignores |

- The name of one of your own themes: `theme = "mine"` loads
  `~/.config/norte/themes/mine.toml` (`%APPDATA%\norte\themes\` on Windows).
- A path to a custom TOML theme.

A name is looked up in that order and the order is fixed: a bundled preset
always wins, so a stale `themes/nord.toml` cannot change what `nord` means,
and such a file is not offered in the picker either. Only a plain name — letters,
digits, `-`, `_` and `.`, not starting with a dot — is looked up in the themes
directory; anything else is taken as a path.

When the setting is absent, norte uses the neutral `default` preset. If a theme
cannot be loaded because its path does not exist or its TOML is invalid, norte
shows a warning and falls back to `default`. Theme configuration is hot
reloaded, so saving the file applies changes to a running application.

## Write a custom theme

All three sections in a theme file are optional. Missing values inherit a
readable monochrome fallback.

Save it as `~/.config/norte/themes/<name>.toml` and it appears by that name in
the theme picker, the first-run wizard and the settings screen of both
frontends, with a live preview. The list is read with the configuration, so a
new file shows up on the next reload. A file that does not parse is left out of
the list rather than breaking the picker; set it by path to see why it fails.

```toml
name = "my-theme"

# Semantic UI roles
[roles]
regular          = { fg = "#d0d0d0" }
selection        = { fg = "#ffffff", bg = "#3a3a3a" }
border-focus     = { fg = "#5fafd7", bold = true }
border-unfocused = { fg = "#6c6c6c", dim = true }
modal-border     = { fg = "#af87d7" }
status-bar       = { fg = "#1c1c1c", bg = "#5fafd7" }
title            = { fg = "#5fafd7", bold = true }
hostile-badge    = { fg = "#d75f5f", bold = true }
error            = { fg = "#d75f5f" }
warning          = { fg = "#d7af5f" }
info             = { fg = "#5fafd7" }
match            = { fg = "#1c1c1c", bg = "#d7af5f" }

# Chrome roles. The window paints these; the terminal ignores them.
[roles]
hover             = { bg = "#2a2d2e" }
input-background  = { bg = "#313131" }
input-border      = { fg = "#3c3c3c" }
widget-background = { bg = "#202020" }
widget-shadow     = { fg = "#000000" }
badge             = { fg = "#f8f8f8", bg = "#616161" }
scrollbar-slider  = { bg = "#434343" }
separator         = { fg = "#2b2b2b" }
focus-border      = { fg = "#0078d4" }
muted             = { fg = "#9d9d9d" }

# Styles by node type
[files.kind]
dir     = { fg = "#5fafd7", bold = true }
symlink = { fg = "#5fafaf" }
# Other keys include executable, fifo, socket, block-device, and char-device.
# Those five are DORMANT: the protocol's `Entry` carries no mode yet, so no
# frontend can select them. Extension rules still apply to such files.

# Styles by file extension. These take precedence over node types.
[files.ext]
rs  = { fg = "#d7875f" }
zip = { fg = "#d75f5f" }
png = { fg = "#af87d7" }
```

Colours accept `#rrggbb` and shorthand `#rgb` values. Styles support the
boolean attributes `bold`, `dim`, `italic`, `underline`, and `reverse`.

Defining a role replaces its fallback completely.

## Omitting a role

Omitting one of the **eighteen core roles** keeps its monochrome fallback —
the look norte had before it grew themes.

Omitting one of the **ten chrome roles** in the table above is different, and
better: the window derives it from a colour your theme already has, so a theme
that never mentions them still looks coherent. The derivations are

| role | derives from |
| --- | --- |
| `separator`, `input-border`, `scrollbar-slider` | `border-unfocused` |
| `hover` | `pane-focus-background` |
| `input-background`, `widget-background` | `pane-background` |
| `focus-border` | `border-focus` |
| `muted` | `title` |
| `badge` | `selection` |
| `widget-shadow` | a translucent black the stylesheet supplies |

Define one only when the derived value is wrong for your palette. The terminal
ignores all ten, so a theme meant for both frontends loses nothing by setting
them.

## Import a VSCode theme

```sh
norte theme import ~/Downloads/OneDark-Pro.json          # → themes/one-dark-pro.toml
norte theme import dracula.json --name dracula --use     # and set [ui] theme
```

`norte theme import` reads a Visual Studio Code colour theme — the JSON inside
a `.vsix`, comments and trailing commas allowed — and writes
`~/.config/norte/themes/<name>.toml`, which the picker then offers by name.
The name is `--name`, else the theme's own `"name"` made into one
(`One Dark Pro` → `one-dark-pro`), else the file name. `--use` also sets
`[ui] theme`, keeping the comments in `norte.toml`; `--force` replaces a theme
of that name. A name that is a bundled preset is refused, because the preset
would always win and the file would never be read.

What the importer does with the file, so the result is not a surprise:

- **It follows `include`**, relative to the file that names it, up to eight
  deep. A chain that loops back is an error.
- **It paints over `vscode-dark` or `vscode-light`**, chosen by the theme's
  `"type"`. A VSCode theme is not a complete palette — even Dark Modern leaves
  the list selection and the scrollbar to the editor's built-in defaults — so
  whatever the theme does not define comes from that preset rather than from
  the monochrome fallback. `[files.kind]`, `[files.ext]`, `mark` and
  `hostile-badge` always come from it: VSCode has no colour for a file list by
  node type.
- **Translucent colours are flattened** over the theme's editor background,
  because a norte theme has no alpha channel. `widget.shadow` is not imported
  at all; the window's own translucent shadow looks better than a solid one.
- **Only `colors` is read.** `tokenColors` and `semanticTokenColors` are
  syntax colouring, which norte does not do. A colour value that does not
  parse is skipped and named on stderr.

The file it writes is an ordinary theme with a header saying where it came
from; edit it like any other. The mapping, VSCode id → role:

| role | VSCode colour ids (the later one wins when both are set) |
| --- | --- |
| `background`, `pane-focus-background` | `editor.background` |
| `regular` | `editor.foreground`, `foreground` |
| `pane-background` | `sideBar.background` |
| `selection` | `list.activeSelectionBackground` / `…Foreground` |
| `selection-unfocused` | `list.inactiveSelectionBackground` / `…Foreground` |
| `hover` | `list.hoverBackground` |
| `border-focus`, `focus-border` | `focusBorder` |
| `border-unfocused`, `separator` | `panel.border` |
| `modal-border` | `widget.border` |
| `status-bar` | `statusBar.background` / `.foreground` |
| `title` | `sideBarSectionHeader.foreground`, `sideBarTitle.foreground` |
| `button` | `button.background` / `.foreground` |
| `match` | `editor.findMatchBackground` |
| `error` | `editorError.foreground`, `errorForeground` |
| `warning`, `info` | `editorWarning.foreground`, `editorInfo.foreground` |
| `muted` | `descriptionForeground` |
| `badge` | `badge.background` / `.foreground` |
| `input-background`, `input-border` | `input.background`, `input.border` |
| `widget-background` | `editorWidget.background` |
| `scrollbar-slider` | `scrollbarSlider.background` |

A theme carries no fonts. For VSCode's type sizes, set `font_size = 13` under
`[ui]`; `font` and `mono_font` choose the faces.

## What each frontend paints

Not every part of a theme reaches every frontend, and the gaps are deliberate:

- The **ten chrome roles** are the window's. A terminal has no scrollbar
  slider and no pointer hover.
- `[effects]` is the window's too (see below).
- `[files.kind]` and `[files.ext]` reach both, but the window carries only
  `fg`, `bold`, `dim`, `italic` and `underline` from them. It deliberately
  drops `bg` and `reverse`: a row's background is already spoken for by the
  cursor, the hover and the mark, and a fourth claimant would let a theme hide
  where the cursor is.
- An extension is matched against the filename's **raw bytes**, always. If a
  panel is reinterpreting names in a legacy encoding
  (`pane.names-encoding`), the extension you see on screen is not
  necessarily the one `[files.ext]` matches — the bytes are.

## Terminal colour support

norte detects the terminal's colour depth and selects the closest supported
value:

- `COLORTERM=truecolor` or `COLORTERM=24bit`: use the original 24-bit colour.
- A `TERM` value containing `256`: use the closest xterm-256 colour.
- Any other terminal: use the closest 16-colour ANSI value.

This keeps the same theme legible in modern terminals such as Kitty and
WezTerm as well as a basic `xterm`.

## GPU effects

A theme may include an `[effects]` section for gradients, glow, and bezel
framing rendered by the GUI. The terminal interface always ignores this
section — it is safe to include in a theme shared between both frontends.
See ADR 0036 for the design rationale.

```toml
[effects]
scanlines = { opacity = 0.12, spacing_px = 3 }
vignette  = { strength = 0.3 }
glow      = { strength = 0.4 }
bezel     = { radius_px = 12, inset = true }
```

All four keys are optional, and so is the whole section. Every numeric value
is clamped to a safe range; out-of-range values clamp silently instead of
being rejected:

| Key                    | Clamp range   |
| ----------------------- | ------------- |
| `scanlines.opacity`    | `0.0` – `0.35` |
| `scanlines.spacing_px` | `2` – `16`     |
| `vignette.strength`    | `0.0` – `0.6`  |
| `glow.strength`        | `0.0` – `1.0`  |
| `bezel.radius_px`      | `0` – `32`     |

Each subfield is optional on its own: a table like `scanlines = { opacity =
0.2 }` (no `spacing_px`) is valid, and the omitted subfield uses these
defaults:

| Subfield                | Default |
| ------------------------ | ------- |
| `scanlines.opacity`      | `0.1`   |
| `scanlines.spacing_px`   | `3`     |
| `vignette.strength`      | `0.3`   |
| `glow.strength`          | `0.4`   |
| `bezel.radius_px`        | `10`    |
| `bezel.inset`            | `false` |

`scanlines.opacity` is capped at `0.35`, which keeps AA text contrast for the
default preset; on dark themes the tight case is an inverse pair — dark text
on a bright accent color — at the opacity cap. `glow.strength` brightens
foreground colors toward white; it does not currently produce true shader
bloom. `bezel.inset` accepts `true` or `false` only.

Unknown keys inside `[effects]`, and keys with the wrong type, produce a
startup warning and are skipped individually — they never prevent norte from
starting. If every key in `[effects]` is invalid or unrecognized, the GUI
behaves as if the section were absent.
