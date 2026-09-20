# 0130 — The window edits settings with its own controls

- Status: accepted
- Date: 2026-09-20
- Decision makers: Oscar González
- Protocol: unchanged. Bridge: **83** — `SettingRowView` grows `control`,
  `choices`, `min`, `max` and `default`; the action `settings_set` puts a
  value instead of cycling to it.
- Related: ADR 0129 (the sections this builds on), ADR 0077 (parity between
  the terminal and the window), the S4 design of 2026-07-24 (which made the
  window mirror the terminal's editor)

## Context and problem statement

The settings screen shipped in the window as a copy of the terminal's: a
list of rows painting `true`, `false`, `nord`, `14` as text, where Enter
cycled a value or opened a one-field dialog. That is the right design for a
terminal — a terminal has no switches.

Opening it in the window made the gap obvious. Its reader's words: *"si
estamos en GUI, ¿no tenemos controles más ricos que sólo true/false?"*, and,
looking at a screenshot beside VSCode's settings editor, *"no da una imagen
muy profesional"*.

Three separate faults, and only the first is about widgets:

1. **No controls.** A boolean was the word `true`. Changing it meant knowing
   that Enter toggles. A ten-item list was cycled one Enter at a time.
2. **A table.** Name, value and actions in columns. The control ended up far
   from the thing it controls, the description had nowhere to live (so it
   was hidden behind the cursor), and — because each row was its own grid —
   the columns did not even line up.
3. **Repeated warnings.** `requiere reinicio` painted as a pill on every row
   that has it: six at once on the first screen. A label repeated six times
   stops being read.

## Decision

**The window gets real controls, and the shared model tells it which.**

### The catalog says what a setting IS; the renderer only paints it

`SettingKind` already knew — `Bool`, `Enum`, `Int{min,max}`, `Text`, `Args`,
`ThemeName`, `PresetName` — and none of it crossed the bridge. It does now,
as `control` plus `choices`/`min`/`max`, with the **live** lists (installed
themes, keymap presets) already resolved by the host.

The renderer does not decide what a setting admits and does not validate.
A frontend that validated would be a second rule, and two rules drift.

### `settings_set` puts a value; it does not cycle to it

`settings_activate` cycles, which is what Enter means in a terminal. With a
ten-theme dropdown, picking the seventh would be seven round trips and six
writes to `norte.toml`. The new action carries the catalog **id**, not the
row: a control takes as long as the reader takes to let go of it, and the
search behind it may have changed which rows exist by then.

Validation stays in `SettingsState::set_value`, the same machine the
terminal's keyboard drives.

### One setting is a block, not a table row

Name, description, control — stacked, the way the settings editors people
already know how to use are built. The description is **always** visible:
it is what says what a setting does, and hiding it behind the cursor forces
you to walk the list to read it.

"Not the factory value" is a 3px bar down the left of the block, not a dot
lost in the text: in a long list it is what you scan for.

### Information that only matters when you touch a row goes in its prose

`requiere reinicio` moves into the end of the description, in italics. Same
fact, said once per row that has it, where the reader already is.

### An empty field shows the default VALUE, not a sentence about it

The first attempt used a placeholder reading "lo que norte trae". A
placeholder occupies the space where the datum goes, so it has to BE the
datum: `auto`, `14`. Where the factory value is genuinely empty — the fonts,
which norte does not set at all — the box stays empty rather than claiming a
configuration that does not exist.

## Consequences

- The terminal is untouched. It keeps cycling with Enter, which is correct
  there, and both frontends keep sharing the catalog, the filter, the
  sections and the validation — which is where the parity that matters
  lives (ADR 0077). Parity is a shared MODEL, not the same widgets.
- `default_value` becomes public in `norte-frontend`, and with it the fact
  that "modified" is measured against the factory value rather than against
  "is the key in your file" (ADR 0129) now shows up in the UI as a
  placeholder, not just as a dot.
- Text and command-line settings are edited in place and save on blur or
  Enter, never per keystroke: a write per keystroke is a `norte.toml`
  rewrite and a full config reload per keystroke. Escape gives up the edit.
- The one-field dialog stays for the keyboard path and for anything the
  window cannot edit inline.
- The window's stylesheet stops using `--info-fg` for prose. Blue reads as
  "this is clickable"; a description is not.
