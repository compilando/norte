# 0106 — The chrome is derived from the keymap and the catalogue, not drawn

- Status: accepted
- Date: 2026-09-10
- Decision makers: Oscar González
- Related: ADR 0077 (a presentation decision is taken once, in the shared
  layer), ADR 0102 (two focus rings), ADR 0103 (a modal line declares its
  role), ADR 0104 (a button goes through the key's path), spec
  `docs/superpowers/specs/2026-09-10-usabilidad-y-apariencia-design.md`

## Context

Piloting `ntc` in tmux for an hour with a fresh configuration surfaced ten
things a reader of any orthodox file manager notices before anything else:
no function-key bar, a cursor that vanished on the default theme, a palette
that truncated the human label down to `sw…ane`, a panel bar of six bare
letters, no counts or free space anywhere, dialogs closed by a line of text,
a status notice that never went away, dates only as `11h ago`, F9 opening
the theme picker where every manager in the family opens the menu, and a
first start that asked nothing and configured nothing.

Each could have been drawn by hand: a table of ten labels for the key bar, a
list of six names for the panel bar, a `[ Ok ]` painted into each modal.
This repository has paid for hand-drawn chrome three times already — a menu
that said one thing while the preset did another (#250), a panel bar that
shipped under the wrong wire name with the gate green, and a bound command
nobody's terminal could deliver — and the lesson is the same each time: a
second copy of a fact diverges in silence.

## Decision

1. **The chrome is derived, never drawn.** The key bar is computed from the
   effective keymap of the screen that owns the keyboard (`keybar::cells_in`);
   rebinding F5 relabels it and a screen that binds nothing to F7 shows an
   empty cell. The panel bar's names come from the kind registry and the same
   Fluent keys that named its letters. The pane footer is counted from the
   listing and the volume table. Dialog buttons are the generated key line,
   recognised as the last body line that parses as `[key] verb`. Nothing new
   is written twice.

2. **A click is the key.** A key-bar cell, a dialog button and the notices
   badge do not dispatch a command of their own: the terminal synthesizes the
   key and hands it to `on_key`, the only path with the three resolvers; the
   window sends the key (`key_bar_activate`) or the panel-bar index and the
   host runs it through `tecla` or `pulsar_barra_de_paneles`. There is no
   second dispatch that can diverge from the first (ADR 0104, generalised).

3. **Everything with a choice is a `[ui]` key and a settings row.** Six keys
   (`key_bar`, `panel_bar_style`, `pane_footer`, `date_format`,
   `notice_seconds`, `dialog_buttons`) land as one `UiChrome` struct on
   `CommonConfig`, validated at load, hot-reloaded, listed on the settings
   screen of both frontends, and documented in one help topic. The cursor is
   theme data (`selection`, `selection-unfocused`), not a key: the theme
   file already exists for exactly that.

4. **The two frontends share the model, and each keeps only the painting.**
   `footer`, `keybar`, `wizard`, `panelbar::button_cell`, the palette's
   recents and the smart date format live in `norte-frontend`; the TUI paints
   cells and the window paints DOM. Where the TUI measures cells for the
   mouse, the same computation serves the painter, so a zone is exactly what
   was painted.

5. **Ticks, not clocks, for expiry.** A notice's lifetime is counted in the
   one-second session tick in both frontends, so a test drives it without
   sleeping and the host test waits for the snapshot, never for wall time.

6. **The first start writes configuration, not state.** The wizard writes
   `[keymap] preset` and `[ui] theme` through the same `persist_set` as the
   settings screen, and the existence of the file is the mark that it ran.
   Esc writes the current theme for that reason. `NORTE_NO_WIZARD` keeps it
   closed for pilots and tests; `ntc --setup` reruns it.

7. **F9 opens the menu** in every preset whose source attests it (mc, FAR,
   Norton Commander, Total Commander) and in the three designed presets;
   Krusader keeps F9 as the terminal, as its source attests, and says so in
   its header. The theme moves to `alt+9`, where the imported presets already
   had it.

## Consequences

- Bridge 63, one bump for the whole wave: `PaletteRowView.recent`,
  `PanelBarView.names`, `BrowserSlotView.footer` and its header patch,
  `ViewSnapshot.key_bar` with the `key_bar` change and `key_bar_activate`,
  `StatusView.notices_unread`, `ViewSnapshot.wizard` with `wizard_open` and
  `wizard_activate_row`. No wire change to the daemon.
- The mtime column grows from 10 to 12 cells for the smart format; 21 TUI
  snapshots moved by that column and nothing else.
- `Iso` stays UTC RFC 3339: metadata, modals and sync pin it as
  locale-free. `Smart` is the one that is local, through `jiff`, which was
  already in the dependency graph.
- The window's dialogs keep their `DialogChoice` buttons and structural
  fields; the TUI's `LineKind` does not cross the bridge. That half of item
  six was scoped out: twelve initializers for a role nothing would paint
  differently.
- The spec's "modifier-sensitive key bar" is out: a terminal does not report
  a held modifier, and a bar that lied about Shift would be worse than none.

## Alternatives considered

- **A hand-written table per bar.** Rejected for the reason above: three
  prior divergences in this repository, each caught by a human.
- **Tab focus over dialog buttons.** In the dialog screen Tab is
  `dialog.pane`, and a button focus competing with the text field is more
  error than help. Buttons are clickable; keys stay keys.
- **`Instant`-based notice expiry.** Rejected: 26 wall-clock sleeps already
  lose their race under load; a tick counter is deterministic and the host
  test waits on a snapshot instead.
- **Passing `first_run` through `UiHostOptions`.** Thirty-three test
  initializers would have changed; a catalogue flag and a `wizard_open`
  action from the renderer cost one field and no test churn.
