# 0184 - The window paints colored file-type icons of its own

- Status: accepted
- Date: 2026-10-13
- Decision makers: Oscar González
- Related: ADR 0105 (the icon column), ADR 0108 (entry colors cross the
  bridge), ADR 0066/0067 (the renderer decides nothing).

## Context and problem statement

Side by side with Krusader, norte's window looked flat. Every file was the
same grey stroke page at 55% opacity, and only a folder looked different.
Krusader paints the desktop's colored icons: a red PDF, a blue document, a
picture for an image.

Colored per-type icons only existed through the `file-icons` plugin. That
plugin ships unapproved and off. It answers with a glyph (emoji or a Nerd
Font codepoint) painted in the name's color, so even when on it is one
color per row.

## Considered options

1. **Own multicolor SVGs, on by default.**
2. A color per icon from the plugin and the theme. That is a WIT and wire
   change, and the glyphs stay single-colored.
3. The desktop's freedesktop icon theme, as Krusader does. Linux only,
   nothing for the terminal, and the most expensive.

## Decision

Option 1.

- Fourteen drawings of our own in `norte-frontend/assets/file-icons/`:
  `folder`, a generic `file`, and twelve types (text, pdf, doc, sheet,
  slides, image, audio, video, archive, code, script, config).
  - Each one is a page with a colored band and a white mark, on the same
    24 grid as the other icons.
  - The colors are fixed, as a desktop icon theme's are, and chosen to read
    on light and dark themes.
  - They are our own, so there is no license to audit, and the terminal can
    rasterize the same files one day.
- **The host decides the type** (`norte_frontend::file_icon::icon_kind`)
  from the entry kind and the RAW name bytes. It sends the id in
  `RowView.icon_kind` (bridge 114), and the renderer only draws it.
- The renderer reads the files with the same two-pattern reader as the
  other icons. It keeps only paint attributes, so the webview parses no
  markup (D11).
- **A plugin icon still wins.** With `file-icons` on, the row shows its
  glyph exactly as before.

## Consequences

- The window looks like a desktop file manager out of the box, with no
  plugin to approve.
- The icon no longer takes the name's color: the type's colors belong to
  the drawing. The name keeps its theme color, so the column of names reads
  the same as before.
- **The terminal is unchanged.** It has no images on listing rows, and the
  `file-icons` plugin (emoji, Nerd or Seti) remains its way to get icons.
  Rasterizing these files there through the kitty/sixel path of ADR 0168 is
  possible later.
- This table and the plugin's `icons.rs` overlap. The plugin is a WASM
  guest and cannot link `norte-frontend`, so the overlap stays, and
  `file_icon.rs` says it is close to the plugin's on purpose.
