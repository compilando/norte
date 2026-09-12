# Themes

norte uses themes to style its terminal interface. The shared model lives in
`norte-theme` and is also available to the GUI. See ADR 0020 for the design
decision.

## Choose a theme

### Use the theme picker

Press **F9** to open the picker. Moving the cursor previews each bundled theme
immediately. Press **Enter** to apply the selected theme and save it to the
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

- A path to a custom TOML theme.

When the setting is absent, norte uses the neutral `default` preset. If a theme
cannot be loaded because its path does not exist or its TOML is invalid, norte
shows a warning and falls back to `default`. Theme configuration is hot
reloaded, so saving the file applies changes to a running application.

## Write a custom theme

All three sections in a theme file are optional. Missing values inherit a
readable monochrome fallback.

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

# Styles by node type
[files.kind]
dir     = { fg = "#5fafd7", bold = true }
symlink = { fg = "#5fafaf" }
# Other keys include executable, fifo, socket, block-device, and char-device.

# Styles by file extension. These take precedence over node types.
[files.ext]
rs  = { fg = "#d7875f" }
zip = { fg = "#d75f5f" }
png = { fg = "#af87d7" }
```

Colours accept `#rrggbb` and shorthand `#rgb` values. Styles support the
boolean attributes `bold`, `dim`, `italic`, `underline`, and `reverse`.

Defining a role replaces its fallback completely. Omitting it keeps the
default monochrome style for that role.

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
