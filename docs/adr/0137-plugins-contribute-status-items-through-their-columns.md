# 0137 — Plugins contribute status items through their columns

- Status: accepted
- Date: 2026-09-21
- Decision makers: Oscar González
- Protocol: unchanged. Bridge: unchanged. WIT: unchanged. Config: new
  `[ui] status_plugins = ["plugin:<plugin>/<column>", …]`, at most four.
- Related: ADR 0132 (the status bar is made of items), ADR 0037 and #117
  (plugin columns), ADR 0110 (the terminal's Lua status hook, frozen)

## Context and problem statement

VS Code's status bar is where extensions speak: the branch you are on,
the linter's count. ADR 0132 made norte's status bar a list of items but a
closed one — six ids, all of norte's own. The terminal has had a Lua hook
for its status line, frozen since ADR 0110 and absent from the window.

A new plugin kind for this would be a new WIT package, a protocol method,
both frontends and the bridge — the cost the panel kind paid in a whole
session. But the question a status item answers for a file manager is
nearly always *something about the entry under the cursor*, and a kind
that computes something per entry already exists: `columns`.

## Decision

**A plugin status item is a plugin column, shown for the entry under the
cursor.** `[ui] status_plugins` lists `plugin:<plugin>/<column>` ids —
the same shape a plugin column has in `[columns]` — and each becomes an
item whose text is that column's value for the entry under the cursor.
The git plugin's branch column becomes a branch indicator without a line
of plugin code.

**Same fetch, same gate.** `columns::plugin_requests` is the list a
listing asks plugins for: the columns painted plus the ones the status bar
shows, without repeats, in one list for both frontends. It goes through
the existing fetch and `validated_plugin_requests`, so a plugin that is
not approved and enabled, or does not declare that column, is never
called and its item never appears. Values arrive through
`PaneState::plugin_cell`, re-masked, and are cut to 32 cells: a
third party's value cannot eat the bar.

**Last in line, and inert.** Plugin items sit at the left of the right
half — where VS Code puts the branch — with the lowest priority, so they
are the first to give way when the bar is short. They have no command: a
columns plugin was approved to paint, not to drive the manager, so a
click on one does nothing.

`StatusItemView.id` becomes a `String` so an item can carry its plugin
id; the six built-in ids are unchanged. The key is honoured from every
layer, like `status_items`: it only chooses what to show, and nothing runs
that the user did not approve.

## Consequences

- Any existing columns plugin can put something in the status bar, in the
  terminal and the window alike, with no new plugin API.
- A status item cannot say something that is not per entry (a count for
  the whole directory, a remote service's state). If that is ever needed,
  it is a kind of its own, with its own WIT package.
- Each configured item that is not also a painted column costs one more
  `plugin.column_values` call per listing, at most four: a listing asks for
  twelve columns at most, eight painted and four for the bar. Both panes
  ask, though only the focused one's value is shown.
- A change of `status_plugins` on a live reload shows from the next
  listing, not at once: the columns a pane asks for are re-requested when
  its directory is listed again.
- The truncation to 32 cells drops trailing zero-width characters before
  the ellipsis, so a cut joiner or combining mark does not attach to it.
