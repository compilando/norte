# Mouse support — TUI and GUI

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task.

**Goal:** Make the mouse a first-class input in both frontends: click to focus
and move the cursor, wheel to scroll, ctrl/shift-click and drag to mark, a
right-click context menu, and drag-and-drop between panes.

**Architecture:** The semantics live once, in `norte-frontend`, as pure
functions over `PaneState` plus a small drag state machine; each frontend maps
its own event type onto them and renders the feedback. That is what keeps the
two frontends from drifting and what makes the behaviour testable without a
terminal or a GPU.

**Decisions taken (do not re-open):**
- **Drag between panes copies. Shift+drag moves.** The prudent default: the
  destination gains a copy and the source is untouched if anything goes wrong.
  Moving is an explicit modifier.
- **The TUI captures the mouse by default**, with `[ui] mouse = false` to turn
  it off. Capture disables the terminal's own text selection; Shift+drag still
  selects in most terminals, and that must be said in the help topic, not just
  in a comment.
- Marking modifiers match the desktop: **ctrl+click toggles one**, **shift+click
  marks the range** from the cursor, **drag marks what it sweeps**.

**Tech stack:** `crossterm` mouse events (already a dependency) in the TUI,
GPUI mouse events in `norte-gui`, shared logic in `norte-frontend`.

---

### Task 1: Shared marking and drag semantics (`norte-frontend`)

**Files:** `crates/norte-frontend/src/pane.rs`, new
`crates/norte-frontend/src/mouse.rs`.

- [ ] `PaneState::mark_range(from: usize, to: usize)` — marks every entry
      between two visible indices inclusive, in either order, respecting the
      live quick-search filter exactly as `mark_all` does (`markable_indices`).
      Returns how many marks it changed, like the glob marker.
- [ ] `PaneState::set_mark(index, bool)` — the primitive ctrl+click needs;
      `toggle_mark` already exists but works on the cursor only.
- [ ] `mouse.rs`: a `DragKind { MarkSweep, Transfer }` and a `Drag` state
      machine — `press(pane, index, mods)`, `motion(pane, index)`,
      `release(pane, index, mods)` — returning an `Effect` the frontend
      applies (`MoveCursor`, `SetMarks`, `Transfer { from_pane, to_pane, move_files }`).
      Pure: no I/O, no rendering, no frontend types.
- [ ] A drag that starts on an UNMARKED entry marks the sweep; a drag that
      starts on a MARKED entry is a transfer of the current marks. Pin that
      distinction with tests — it is the rule that lets one gesture do both.
- [ ] Tests: range marking in both directions, under a filter, on an empty
      listing; sweep vs transfer; a release outside any row cancels.

Commit: `feat(frontend): shared marking and drag semantics for the mouse`.

---

### Task 2: TUI mouse capture and click handling

**Files:** `crates/norte-tui/src/main.rs`, `crates/norte-tui/src/ui.rs`,
`crates/norte-config` (the `[ui] mouse` key), `crates/norte-help/topics/*`.

- [ ] `[ui] mouse` (default true) in the config schema, with the settings-overlay
      entry so it is discoverable, and hot reload like every other `[ui]` key.
- [ ] Enable/disable crossterm mouse capture at startup and on hot reload.
      Capture must be released on exit and on the external-opener suspend path
      (`run_opener`), or the launched program inherits a terminal in mouse mode.
- [ ] Hit-testing: the draw already knows each pane's rect; store the last
      painted rects (the `#124` precedent, where the real viewport height goes
      back into the model after each frame) and map a click position to
      `(pane, index)`.
- [ ] Left click: focus that pane and move the cursor. Double click: `nav.enter`.
      Wheel: scroll the listing under the pointer, not the focused one.
      Ctrl+click, shift+click and drag: the Task 1 semantics.
- [ ] Right click: open the context menu of Task 4 if it has landed; otherwise
      nothing (do not invent a second menu).
- [ ] Tests: hit-testing maths against a known layout, including the header and
      footer rows and a click outside any row; capture is released on suspend.

Commit: `feat(tui): mouse capture, click, wheel and drag marking`.

---

### Task 3: GUI marking with the mouse

**Files:** `crates/norte-gui/src/main.rs`.

- [x] Ctrl+click toggles a mark, shift+click marks the range, drag sweeps —
      all through Task 1, so the two frontends cannot drift.
- [x] The existing single click (focus + cursor) and double click (`cd`) stay
      exactly as they are. One deliberate narrowing, for parity with the TUI:
      the double click only fires WITHOUT modifiers, because ctrl+double-click
      is now "mark, then unmark" and must not also walk into a directory.
- [x] Marked rows already have a visual treatment; verify it survives a sweep
      and that the row under the pointer during a drag updates live.
- [x] A gesture expires when the listing moves under the pointer or an overlay
      appears (`expire_stale_mouse_gesture`, the GUI's twin of the TUI's
      `mouse::after_frame`) — in the GUI a relist lands ASYNC, mid-drag.
- [x] A drag that starts on a marked row is a transfer: it says so
      (`gui-mouse-transfer-unavailable`, both locales) instead of marking or
      doing nothing. Task 5 replaces the message with the drop.

Commit: `feat(gui): ctrl and shift click, and drag, mark entries`.

---

### Task 4: Right-click context menu (GUI)

**Files:** `crates/norte-gui/src/main.rs`, i18n catalogs.

- [ ] A menu at the pointer with the operations that already exist as commands:
      open, view, copy, move, rename, delete, and "copy path". Every entry
      dispatches the SAME command the keyboard does — no second code path.
- [ ] The menu acts on the marks when the clicked row is marked, and on the
      clicked row alone when it is not. That is the rule every file manager
      uses and the one users expect.
- [ ] Entries that cannot run right now (read-only backend, remote without the
      capability) are shown disabled with the reason, reusing the `Availability`
      vocabulary `norte-help` already defines rather than inventing a second one.
- [ ] Localized labels through the existing Fluent keys; no hard-coded strings.

Commit: `feat(gui): right-click context menu`.

---

### Task 5: Drag and drop between panes (GUI)

**Files:** `crates/norte-gui/src/main.rs`, i18n catalogs.

- [ ] Dragging from a pane onto the other pane transfers: **copy by default,
      move with shift held at release**. The decision is read at RELEASE, not at
      press, so the user can change their mind mid-drag — and the feedback must
      say which one will happen.
- [ ] The drop routes through the same task submission the keyboard copy/move
      uses: same confirmation, same collision dialog, same journal entry, same
      undo. A drop must not become a second, quieter mutation path.
- [ ] Visual feedback: the source rows, the drop target pane, and a label
      saying copy or move. Dropping on the source pane itself is a no-op.
- [ ] Tests: the pure part (which files, which direction, copy vs move) lives in
      Task 1's state machine and is tested there; the GUI test asserts the
      submitted command matches what the keyboard path would submit.

**Known consequence of the Task 1 fork, decide here:** a press on an UNMARKED
row arms a mark sweep, so dragging a single unmarked file to the other pane
transfers NOTHING — it sweeps one row and marks it. That is the most common
drag in any file manager. It follows from the mark-state fork as specified (a
transfer carries the marks, and an unmarked row has none), so it is not a
defect in `norte-frontend::mouse`; it is a gap this task must close. The
options, none of them free: promote the sweep to a transfer when the pointer
crosses into the other pane (the drag then means two things depending on where
it ends, and the feedback must say so before the drop); or mark the pressed row
implicitly at the start of a cross-pane drag (a gesture that silently changes
the selection); or leave it and require a mark first (honest, and what an
orthodox file manager already teaches, but it will read as broken to anyone
arriving from a desktop file manager). Whichever wins, say so in the help topic
of Task 6.

Commit: `feat(gui): drag and drop between panes`.

---

### Task 6: Close

- [ ] Document the mouse in the help corpus (both locales): what the buttons do,
      the marking modifiers, and — for the TUI — that capture takes over the
      terminal's own selection and how to get it back (`[ui] mouse = false`, or
      Shift+drag in most terminals).
- [ ] Changelog entry.
- [ ] `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo nextest run --workspace`, `just ci`, plus `cargo check` and
      `cargo nextest run` inside `crates/norte-gui` (it is outside the
      workspace).

Commit: `docs(mouse): help topic and changelog`.

---

## Notes for the executor

- `norte-gui` is NOT a workspace member: build and test it from its own
  directory. It compiles in this environment; a change that does not compile
  there is not done.
- Rule 7 holds: no business logic in a frontend. If a mouse gesture needs a
  decision, the decision belongs in `norte-frontend`.
- A drop is a mutation: it goes through the journal and undo like any other,
  and through the policy gate. There is no "it was just a drag" exemption.
