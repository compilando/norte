# 0096 — What is operated on and what is pointed at are two questions

- Status: accepted
- Date: 2026-09-05
- Decision makers: Oscar González
- Related: ADR 0058 (layout of slots, roles and follows), ADR 0077 (a
  decision taken once, in `norte-frontend`), #291 (the docked viewer),
  #246 (a layout name is a filename)

## Context

The `..` row (`[ui] parent_entry`, on by default) is synthetic: it is not in
the listing the provider returned, its `Entry` carries the *parent's* path,
and it has neither size nor mtime. What makes it safe is a single sentence
of code in `norte_frontend::PaneState::selected`: over that row, "what is
selected" is `None`. Eighty-odd call sites ask that question to copy, delete,
rename or descend, and none of them has to remember the row exists.

Two panels then asked the same question for a different purpose. The details
sheet and the docked viewer *follow* the cursor and describe what is under
it (ADR 0058's `follows`). They asked `selected()`, got `None`, and printed
"nothing under the cursor" / "nothing selected".

The cursor is born on `..`. So in the window those two panels were empty at
every start and after every `cd` — a working panel that looks broken. The
terminal appeared to be fine only because of a second bug: restoring a
session replaced the pane and lost the row, so `[ui] parent_entry = true`
quietly became `false` and the cursor never landed on `..` at all.

Nothing caught either one. The shared host fixtures set
`ui_parent_entry = Some(false)` — deliberately, because most tests reason
about listing indices — so ~20k lines of controller tests and the whole
parity harness only ever ran in the one state a reader never starts in.

## Decision

1. **One index, three questions.** `PaneState::indice_senalado()` answers
   *which row the screen is pointing at* — the quick-search filter's
   selection when a filter is running, the real cursor otherwise — and the
   three public questions are derived from it: `selected()` ("what would an
   operation act on", still `None` over `..`), `cursor_entry()` ("what is
   pointed at", `..` included, documented as painting-only), and
   `cursor_is_parent_row()` ("is that row the parent row").

   Deriving all three from one index is not tidiness; two separate
   derivations were two bugs. The guard in `selected()` used to test
   `self.cursor`, but in `Mode::Filter` the real cursor does not move and the
   filter picks the row — and a filter's empty query is born selecting index
   0. So **opening the quick search was enough to make `selected()` return
   the parent directory**, which `marked_paths()` falls back to when nothing
   is marked, which F8 turns into a delete target. That hole predates this
   change and had no test. Separately, asking `is_parent_row(cursor())`
   alongside `cursor_entry()` let the sheet label a filtered file `..` and
   drop its hostile badge.

   The alternative — teaching `selected()` to return the row and asking each
   of the eighty call sites to re-check `is_parent_row` — is the design the
   row was built to avoid.

2. **A follower panel describes the row, including `..`.** On the parent row
   the sheet is `Name: ..`, `Kind: folder`, `Leads to: <parent path>` — not
   the parent's basename, which would claim the cursor is on the parent
   itself. The viewer says `directory`, because that is what `..` leads to,
   and still reads nothing.

3. **The sheet's field list lives in `norte-frontend`, once.** It was written
   twice, and the copies had already diverged: the window marked a hostile
   attribute value and the terminal did not, while the equivalent *column*
   marked it in both. `norte_frontend::metadata::sheet` decides the rows;
   the frontends paint them. This is ADR 0077 applied to a panel that had
   escaped it.

   The same reasoning moved a second rule: which tree a layout *name* means
   (`layouts/<name>.toml` first, factory preset second) is now
   `norte_frontend::layout::config::or_preset`, called by both frontends.
   It had lived only in the TUI, so `norte-gui --layout mine` could not open
   a user layout that the same window listed in its picker. Resolution is
   shared; the *policy* around it is not, and deliberately: the terminal
   warns and carries on with an unknown `--layout` because it already has a
   screen up, while the window refuses to start, because it does not.

4. **A listing born outside the pane constructor is adopted through one
   door.** In the TUI that is `App::adoptar_pane`, and the three session
   paths use it — `apply_session`, `restore_slots`, and `pin_start_dir`,
   which was a third instance of the same shape found in review. A pane that
   is replaced wholesale must be given this session's configuration back, and
   "sort and hidden but not the `..` row" is the form the bug took; the door
   stamps what comes from *configuration*, while the caller restores what
   comes from the *session*.

   `pin_start_dir` mattered more than it looks: `set_listing` calls
   `poner_padre`, which is a no-op from the `Apagada` state, so a pane that
   started without the row never got it back — and the case that triggers it
   is `ntc ~/dir` over a session whose listing failed, which is exactly when
   a reader wants to go up.

5. **Parity scenarios run with the row off *and* on.** The default
   configuration is not an edge case, and a harness that never exercises it
   is measuring a product nobody runs. The index-sensitive unit tests keep
   the row off, which is why the two configurations are a loop around the
   comparison rather than a change to the shared fixture.

## Consequences

- A new follower panel has to choose which question it is asking. That is
  the point: the choice is now visible in the name of the accessor instead
  of implicit in "everyone calls `selected()`".
- `cursor_entry()` is a way to obtain the parent's `VPath` without going
  through `parent_target()`. It is documented as painting-only, and the
  operand funnel is unchanged, but it is one more thing a reviewer must look
  at when a diff calls it.
- The parity harness costs twice the scenarios. It found a real modelling
  gap on the first run (`Enter` over `..` navigates, which the harness's
  primitives side did not model), which is the return on that cost.

## Not decided here

`Enter` over `..` goes up in both frontends but does not leave the cursor on
the directory just left, while the dedicated "go up" command does
(`set_pending_focus`). The two surfaces agree with each other, so it is not a
parity defect; it is a shared gap, and it needs its own change and its own
tests.
