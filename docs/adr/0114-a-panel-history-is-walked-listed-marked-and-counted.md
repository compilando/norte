# 0114 — A panel's history is walked, listed, marked and counted

- Status: accepted
- Date: 2026-09-15
- Decision makers: Oscar González
- Related: ADR 0043/0045 (keymap catalogue and imported presets), ADR 0059
  (UI session), ADR 0066 D14 (shared frontend logic), ADR 0077 (TUI/window
  parity), spec `2026-09-15-historia-y-wow-design.md` (phase 1), plan
  `2026-09-15-fase1-historia-de-navegacion.md`

## Context and problem statement

Oscar asked for Krusader's Alt+Left ("goes back through the navigation
history") and for the history feature analysed as a whole.

norte already had the core: one shared `norte_frontend::nav::History` per
slot (an MRU of 30 plus a browser-style back/forward trail), `nav.back` /
`nav.forward` and a `pane.history` list. What it did not have:

1. **Krusader had no back/forward key at all.** Its preset header said Alt+←/→
   were "per-panel bookmark menus". That came from the docs.kde.org key table,
   which is stale: in Krusader's source (`listpanelactions.cpp`) the left/right
   bookmark actions have no shortcut, and Alt+←/→ are
   `KStandardAction::Back/Forward`. Ctrl+Alt+←/→ open the left/right panel's
   history, Ctrl+J jumps back to a marked point, Ctrl+Z opens "Popular URLs"
   (`kractions.cpp`).
2. Far, Norton and Total Commander said nothing about the history features
   they do not bind, which the "every preset or a written reason" rule forbids.
3. The list could only be walked: no "where am I", no removing an entry, no
   opening one in the other panel.
4. The TUI and the window built the list rows each on their own.

What the reference managers do (official sources): Krusader as above,
persisted per panel; Total Commander Alt+←/→, Alt+↓ (a list "thinned" of
directories passed through) and Alt+Shift+↓ (unthinned); Far has no
back/forward (its Alt+←/→ scroll long names) and a folder history at Alt+F12
whose list keys are Del (clear), Shift+Del (remove), Ins (lock),
Ctrl+Shift+Enter (open in the passive panel); mc Alt+Y / Alt+U and
Alt+Shift+H; ranger and yazi H / L; desktop convention Alt+←/→ and the mouse's
side buttons. No primary source attests a Norton Commander history.

## Decision

**D1 — One module decides the lists.** `norte_frontend::history` holds the
rows both frontends paint (`history_rows`, `popular_rows`, a `HistoryMark` of
current / visited / forward), the start cursor (the row after "here"), the
Fluent key of each mark, and `record_visit`: the ONE decision of what counts as
a step (`prev != dir && trail == Record`). The TUI's `record_step` and the
window's `navegar_hueco` both call it. A `Replay` or a `Seed` counts neither in
the trail nor as a visit.

**D2 — The list says where you are.** The first row is the current directory,
marked "here"; rows still reachable with `nav.forward` are marked "forward";
the cursor starts on the second row. The TUI appends the mark to the path (the
middle ellipsis keeps the tail); the window puts it in the row's detail.

**D3 — The list is editable.** New dialog verbs `dialog.confirm-other` (open
in the OTHER panel, focus stays) and `dialog.clear` (clear that panel's
history, or the popular list); `dialog.remove` also works on history and
popular rows. Keys in the three native presets, from Far's list where a
terminal can deliver them: `delete` remove, `shift+delete` clear, `alt+enter`
confirm-other (Far's Ctrl+Shift+Enter is indistinguishable from Enter in a
terminal). No confirmation on clear: it is navigation memory, not files, and a
notice says it happened.

**D4 — Jump point.** `nav.set-jump-point` / `nav.jump-back`, one point per
slot, persisted in the session as `SlotState.jump`. Jumping is an ordinary
navigation, so `nav.back` undoes it; removing the directory from the history
clears the point.

**D5 — Popular directories.** `pane.popular` lists ONE session-wide list
(`SessionBody.popular`, capped at 50) ranked by visits, like Krusader's — the
question is "where do I usually go", which does not depend on the side it is
asked from. Full list evicts the least visited, oldest on a tie; `last` is a
counter of the list, not a clock, so ties are deterministic.

**D6 — History of one side.** `pane.history-left/-right` freeze the side when
opened and navigate that panel even without focus, the same shape as
`pane.select-drive-left/-right`.

**D7 — Configurable size.** `[ui] history_size`, 5..=64, default 30. The
ceiling is the per-slot history the session already keeps (`HISTORY_CAP`); a
larger number would be lost on restart. Lowering it drops what is farthest
from the reader. `norte-config` cannot depend on `norte-frontend`, so it
validates with its own numbers, pinned equal by a test.

**D8 — Keys, every preset.**

| preset | back / forward | list | also |
| --- | --- | --- | --- |
| orthodox | alt+left/right, alt+y | alt+down, alt+H | — |
| vim | alt+left/right, H / L | alt+down | — |
| cua | alt+left/right | alt+down | — |
| total-commander | alt+left/right | alt+down | — |
| krusader | alt+left/right | ctrl+h | ctrl+alt+left/right sided lists, ctrl+j jump back, ctrl+z popular |
| far | — | alt+f12 | — |
| norton | — | — | — |

Every omission is written in its preset header: mc's Alt+U stays `pane.pull`
in orthodox; TC's Alt+Shift+↓ would be the same list; Krusader's Ctrl+Shift+J
is an undeliverable ctrl+UPPERCASE; Far has no back/forward; Norton has no
source. What has no key is in the Go menu and the palette.

## Considered and rejected

- **Pinned / locked entries (Far's Ins).** An entry that survives clearing and
  capping is a bookmark, and norte has bookmarks.
- **TC's thinned list.** Needs dwell time per directory; without a real case
  it would be a second list with the same content.
- **A global history of all panels.** Krusader and mc are per panel; the
  popular list already is the global view.
- **Copy the path from the list (Far's Ctrl+Enter).** No reliable system
  clipboard from a terminal, and the chord is more useful as "open in the
  other panel".
- **Ctrl+Enter for confirm-other.** Not deliverable in a legacy terminal.

## Consequences

- The session body gains two additive fields (no schema bump). The popular
  list is the first thing dropped after orphan slots when the body does not
  fit.
- Spanish `dialog-cmd-remove` reads "quitar": on a history, bookmark or
  extension list the verb removes a row, and "borrar" read as deleting the
  directory.
- The help corpus gains the `history` topic, linked from the index and from
  `panes`.
- A help snapshot test that looked for titles anywhere in the frame passed by
  accident (the index body printed the longest title whole); it now measures
  the sidebar column only, at a width where the 35 % ceiling fits the title.
- Still to do in this phase: filtering the list with `/`, adding a history row
  to bookmarks, and the window's mouse side buttons as back/forward.
