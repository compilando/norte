# 0020 - Shared semantic themes and terminal colour fallback

- Status: accepted
- Date: 2026-07-15
- Decision makers: Oscar González
- Related: specification sections 15 and 16.2; ADRs 0007 and 0010

## Context

The original TUI was monochrome and used a few hard-coded ratatui modifiers.
Themes must be reusable by the GPUI frontend, cover semantic UI roles and file
types, degrade cleanly on limited terminals, and reserve room for GUI-only
effects without coupling the model to either renderer.

## Decision

### Shared presentation crate

Create the permissively licensed `norte-theme` crate with no dependency on
ratatui or a rendering backend. Each frontend converts the shared types to its
native colour and style types. Themes are presentation data and do not enter
the core or protocol.

### Semantic model

- Canonical `Color` values are 24-bit RGB authored as `#rrggbb`.
- `ColorDepth` is `Truecolor`, `Ansi256`, or `Ansi16`.
  `Color::resolve` projects RGB to the closest xterm-256 or base ANSI colour
  without changing the source theme.
- `Role` describes meaning, such as selection, focused border, status bar,
  error, warning, or hostile-name badge. A theme maps roles to `Style`, which
  contains foreground/background colours and text attributes. Widgets request
  roles rather than hard-coded colours.
- File styles may match node kind or raw-byte extension. Extension rules take
  precedence over kind, followed by the `regular` role.

### Presets and configuration

Bundle curated default, Catppuccin, Gruvbox, and Nord TOML presets. The
`[ui].theme` setting accepts a preset name or a custom file. Missing roles
inherit from the default theme. Configuration hot reload also reloads and
resolves the theme.

### GPU effects

Reserve a lenient, opaque `[effects]` section. The TUI ignores it at no cost;
the GUI may interpret gradients, glow, or animation. This lets one theme file
serve both frontends without implementing GPU effects in this milestone.

### Roadmap

After M2, order the milestones as themes, plugins, agent integration, then GUI.
This changes sequence, not scope or exit criteria.

## Consequences

The TUI gains consistent colour and M5 can reuse the exact same model. Terminal
fallbacks cover true-colour through 16-colour environments, while presets work
without external files. Costs include one more workspace crate, approximate
16-colour matching, heuristic `COLORTERM`/`TERM` detection, and deferred GPU
effects. Syntax highlighting remains a previewer/plugin responsibility.
