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

- [x] `PaneState::mark_range(from: usize, to: usize)` — marks every entry
      between two visible indices inclusive, in either order, respecting the
      live quick-search filter exactly as `mark_all` does (`markable_indices`).
      Returns how many marks it changed, like the glob marker.
- [x] `PaneState::set_mark(index, bool)` — the primitive ctrl+click needs;
      `toggle_mark` already exists but works on the cursor only.
- [x] `mouse.rs`: a `DragKind { MarkSweep, Transfer }` and a `Drag` state
      machine — `press(pane, index, mods)`, `motion(pane, index)`,
      `release(pane, index, mods)` — returning an `Effect` the frontend
      applies (`MoveCursor`, `SetMarks`, `Transfer { from_pane, to_pane, move_files }`).
      Pure: no I/O, no rendering, no frontend types.
- [x] A drag that starts on an UNMARKED entry marks the sweep; a drag that
      starts on a MARKED entry is a transfer of the current marks. Pin that
      distinction with tests — it is the rule that lets one gesture do both.
- [x] Tests: range marking in both directions, under a filter, on an empty
      listing; sweep vs transfer; a release outside any row cancels.

Commit: `feat(frontend): shared marking and drag semantics for the mouse`.

---

### Task 2: TUI mouse capture and click handling

**Files:** `crates/norte-tui/src/main.rs`, `crates/norte-tui/src/ui.rs`,
`crates/norte-config` (the `[ui] mouse` key), `crates/norte-help/topics/*`.

- [x] `[ui] mouse` (default true) in the config schema, with the settings-overlay
      entry so it is discoverable, and hot reload like every other `[ui]` key.
- [x] Enable/disable crossterm mouse capture at startup and on hot reload.
      Capture must be released on exit and on the external-opener suspend path
      (`run_opener`), or the launched program inherits a terminal in mouse mode.
- [x] Hit-testing: the draw already knows each pane's rect; store the last
      painted rects (the `#124` precedent, where the real viewport height goes
      back into the model after each frame) and map a click position to
      `(pane, index)`.
- [x] Left click: focus that pane and move the cursor. Double click: `nav.enter`.
      Wheel: scroll the listing under the pointer, not the focused one.
      Ctrl+click, shift+click and drag: the Task 1 semantics.
- [x] Right click: open the context menu of Task 4 if it has landed; otherwise
      nothing (do not invent a second menu).
- [x] Tests: hit-testing maths against a known layout, including the header and
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
      doing nothing. Task 5 replaced the message with the drop, and the key
      with `gui-drag-copy`/`gui-drag-move`.

Commit: `feat(gui): ctrl and shift click, and drag, mark entries`.

---

### Task 4: Right-click context menu (GUI)

**Files:** `crates/norte-gui/src/main.rs`, i18n catalogs.

- [x] A menu at the pointer with the operations that already exist as commands:
      open (`nav.enter`), view, copy, move, rename, delete, and "copy path".
      Every entry dispatches the SAME command the keyboard does — no second
      code path. Two of them had no command to dispatch and both became real
      commands rather than menu-only actions: `pane.rename` (rename in place,
      `shift+f6` — the name and the chord were already in the shared presets;
      only the GUI implementation was missing) and `pane.copy-path` (`alt+y`).
      The AI rename keeps its own entry, separately labelled, because it is a
      different operation (it acts on the whole folder).
- [x] The menu acts on the marks when the clicked row is marked, and on the
      clicked row alone when it is not. That is the rule every file manager
      uses and the one users expect. Clicking an UNMARKED row drops that
      pane's marks (desktop behaviour): `marked_paths` prefers marks, so
      leaving them would make the menu say "1" and the copy take eleven. The
      discarded selection cannot be recovered — a deliberate trade, stated in
      `context_target`'s own header, taken because the failure it prevents is
      silent and this one is visible the instant the menu opens.
- [x] Entries that cannot run right now (read-only backend, remote without the
      capability) are shown disabled with the reason, reusing the `Availability`
      vocabulary `norte-help` already defines rather than inventing a second one.
      `norte-help` became a dependency of `norte-gui` (in-workspace, no new
      external crate) and gained one variant, `Reason::WrongTarget`, for the
      dimming that is about the SELECTION rather than the backend.
- [x] Localized labels through the existing Fluent keys; no hard-coded strings.

Commit: `feat(gui): right-click context menu`.

---

### Task 5: Drag and drop between panes (GUI)

**Files:** `crates/norte-gui/src/main.rs`, i18n catalogs.

- [x] Dragging from a pane onto the other pane transfers: **copy by default,
      move with shift held at release**. The decision is read at RELEASE, not at
      press, so the user can change their mind mid-drag — and the feedback must
      say which one will happen. `Drag::pending(mods)` answers "what would a
      release do RIGHT NOW" with the same rules as `release`, so the label
      cannot promise one thing and the drop do another; the GUI re-reads it on
      `on_modifiers_changed`, because shift goes down without the pointer
      moving a pixel.
- [x] The drop routes through the same task submission the keyboard copy/move
      uses: same confirmation, same collision dialog, same journal entry, same
      undo. A drop must not become a second, quieter mutation path. One
      function, `transfer_modal`, is now the single source of what a copy or a
      move submits; `pane.copy`/`pane.move` and the drop both call it, and the
      GUI test confirms both modals and compares the `PendingOp`s.
- [x] Visual feedback: the source rows, the drop target pane, and a label
      saying copy or move. Dropping on the source pane itself is a no-op.
- [x] Tests: the pure part (which files, which direction, copy vs move) lives in
      Task 1's state machine and is tested there; the GUI test asserts the
      submitted command matches what the keyboard path would submit.

**Known consequence of the Task 1 fork — DECIDED: promote.** A press on an
UNMARKED row arms a mark sweep, so dragging a single unmarked file to the other
pane would transfer NOTHING — it would sweep one row and mark it, which is the
most common drag in any file manager. Of the three options, the sweep is now
PROMOTED to a transfer of the pressed row the moment the pointer crosses into
the other pane. The objection to it — that the gesture then means two things
depending on where it ends — is answered by the feedback: `Drag::pending`
tells the frontend what a release would do, and the GUI renders it (how many
items, to which pane, copy or move) before the button comes up. Promotion
changes what the gesture DOES, not what is selected: the pressed row is never
marked, and the rows the sweep marked on the way out are given back
(`Effect::RevertSweep`), so a cancelled drag leaves the selection exactly as it
was. A sweep armed with shift is NOT promotable — shift means "extend the
range", and reading a range that ends past the pane boundary as a drop would
turn a marking gesture into a MOVE of the whole selection.

The TUI drives the same machine, so it promotes too — and since the TUI drop
landed it behaves identically: same rules, same `Drag::pending` feedback (in the
status bar), same submission. Task 6's help topic states the promotion rule for
both frontends.

---

### Task 5b: Drag and drop between panes (TUI)

**Files:** `crates/norte-tui/src/{mouse,app,ui,main}.rs`, i18n catalogs.

- [x] The same rules as the GUI: copy by default, move with shift held AT
      RELEASE, a press on an unmarked row promoted to a one-row transfer when
      the pointer crosses, a drop on the source pane a no-op, marks restored
      exactly on cancel.
- [x] It routes through the SAME submission the keyboard `pane.copy`/`pane.move`
      use. `App::open_transfer(kind, from, to, promoted)` is now the single
      source of what a transfer submits — the key and the drop both call it, and
      it owns the "one item ⇒ editable name (#105), several ⇒ list confirm"
      decision that used to live in `dispatch`. A promoted drop never consumes
      the pane's marks: the row it carries was never marked.
- [x] Feedback before the drop: the status bar renders `Drag::pending` (the same
      source the release reads) as "Drop to COPY/MOVE n item(s) → dir". Shared
      keys with the GUI, renamed `gui-drag-*` → `drag-*`; the TUI-only
      `msg-mouse-transfer-unavailable` is gone from both catalogs. The
      modifiers come from the last mouse event, because a terminal reports the
      keyboard only alongside a mouse report while the button is down.
- [x] Tests: the drop opens the same modal the key opens for the same
      selection, shift at release decides, a drop at home submits nothing, a
      cancelled drag restores the marks, and the hint matches the drop.

Commit: `feat(tui): drag and drop between panes`.

Commit: `feat(gui): drag and drop between panes`.

---

### Task 6: Close

- [x] The `mouse` help topic (both locales) now covers the buttons, the marking
      modifiers, drag-copies/shift-drag-moves, the promotion rule and what it
      gives back, the right-click menu and its target rule (including that
      right-clicking an unmarked row discards the marks), and — for the TUI —
      that capture takes over the terminal's own selection and how to get it
      back (`[ui] mouse = false`, or Shift+drag in most terminals).
      `pane.rename` left the documentation gate's allowlist, whose ceiling
      came down with it.
- [x] Changelog entry.
- [x] `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo nextest run --workspace`, `just ci`, plus fmt/clippy/nextest
      inside `crates/norte-gui` (it is outside the workspace).

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
