# Debt wave: the frontend surface (#143, #141)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Paste stops submitting fields it should fill (#143), and the shortcut
editor's unbind stops missing bindings that are right in front of it (#141).

**Architecture:** Two independent tasks in the TUI and the shared frontend
crate. #143 turns on bracketed paste once and routes `Event::Paste` through ONE
router that mirrors the existing per-modal `KeyCode::Char` dispatch — a paste
that reached a field by a different path than a keystroke would be a second
input surface with its own bugs, and the hazard filter is exactly the kind of
thing that gets applied to one and not the other. #141 gives the unbind the
dry-run the bind already has, matching by parsed sequence instead of by bytes.

**Tech stack:** Rust, crossterm bracketed paste, `norte-frontend::keymap::rebind`,
Fluent, no new dependencies.

**Issues:** #143, #141. (#125 turned out to be already fixed and was closed
during planning — `is_terminal_hazard` tests `Default_Ignorable_Code_Point`
through a range table and both fixtures it asked for are in the corpus.)

**Decisions taken during planning, not to be relitigated:**

- **A multi-line paste into a one-line field inserts the FIRST line, discards
  the rest, and says how many it discarded.** It never submits. What must
  become impossible is today's behaviour: the newline reads as Enter, the field
  is submitted, and the tail lands in the dispatcher as keystrokes.
- **A `[global]` binding is an explicit, non-editable row state.** The editor
  says the binding is global and is edited in the file. It does NOT write to
  `[global]` from a row that names one screen — that would change all three.

---

## File structure

| file | responsibility |
| --- | --- |
| `crates/norte-tui/src/main.rs` | Enable bracketed paste; one `Event::Paste` router beside the existing `KeyCode::Char` arms; the unbind's new call. |
| `crates/norte-frontend/src/keymap/rebind.rs` | `unbind_dry_run`, symmetric with `rebind_dry_run` (`:601`). |
| `crates/norte-frontend/src/shortcuts.rs` | The `[global]` row state. |
| `crates/norte-i18n/i18n/{en,es}.ftl` | The discarded-lines message and the global-row wording. |

---

## Task F1: paste fills a field, it does not submit it (#143)

**Files:**
- Modify: `crates/norte-tui/src/main.rs` — terminal setup (bracketed paste on/off, next to the alternate screen in `crates/norte-tui/src/tty.rs`), and the event loop
- Modify: `crates/norte-i18n/i18n/{en,es}.ftl`
- Test: `crates/norte-tui/tests/` (a new file, or the existing modal test file)

The free-text sinks, all of them, from `rg -n 'KeyCode::Char\(c\) if plain' crates/norte-tui/src/main.rs`: quick search, mark pattern, mkdir, command line, AI rename instruction, semantic query, transfer name, the help filter, the shortcuts editor, the settings editor, and the generic dialog. Eleven, not six. Any one of them left out is a field where paste still submits, so the router covers them all or the task is not done.

- [x] **Step 1: Write the failing test**

```rust
/// A pasted newline must never submit. Before bracketed paste, a terminal
/// delivered a paste as ordinary keystrokes, so `mkdir` + a two-line paste
/// created the first line as a directory and fed the second to the dispatcher
/// — which is a paste that runs commands.
#[test]
fn a_multiline_paste_fills_the_field_and_does_not_submit() {
    // Open the mkdir modal, paste "one\ntwo", assert: the field holds "one",
    // the modal is STILL OPEN, and nothing was created.
}

/// The tail is not silently eaten: a user who pasted three lines is told two
/// did not make it, because a field that quietly holds a third of what you
/// pasted is worse than one that refuses.
#[test]
fn the_discarded_lines_are_counted_in_the_message() {
}

/// Paste goes through the SAME hazard filter as a keystroke. A paste that can
/// carry a RLO or an invisible where a keypress cannot is a bypass of
/// `must_mask`, and the whole point of that gate is that a name you approved
/// is the name you saw.
#[test]
fn a_paste_is_sanitised_exactly_like_a_keystroke() {
    // Paste "a\u{202E}b" into mkdir; assert the field holds what the same
    // characters typed one at a time would hold.
}
```

- [x] **Step 2: Run and watch them fail**

Run: `just t norte-tui`

- [x] **Step 3: Turn bracketed paste on**

In `crates/norte-tui/src/tty.rs`, beside the alternate screen: `EnableBracketedPaste` on init, `DisableBracketedPaste` on restore AND in the panic hook. A terminal left in bracketed-paste mode after a crash pastes `\e[200~` markers into the user's shell.

Suspension (`run_suspended`, `main.rs`) must release it the same way it releases the mouse capture: the child did not ask for it. Follow `mouse::release_for_suspend`'s shape exactly — that pattern exists because this class of bug already happened once.

- [x] **Step 4: One router**

`Event::Paste(String)` arrives at the event loop. Write ONE function that takes the app and the pasted text and dispatches to the active sink, mirroring the `KeyCode::Char(c) if plain` arms one for one. First line only: split at the first `\n`, and strip a trailing `\r` (a Windows clipboard sends CRLF). If anything was discarded, count the remaining lines and set the status message.

Every character of the first line goes through the same filter a keystroke does (`main.rs:5251` is the predicate). Do not reimplement it.

The status message, both locales:

```ftl
# en.ftl
msg-paste-truncated = pasted the first line; { $lines } more discarded
# es.ftl
msg-paste-truncated = pegada la primera línea; { $lines } descartadas
```

- [x] **Step 5: Run the tests**

Run: `just t norte-tui`, `just c`

- [x] **Step 6: Reviewers**

`encoding-auditor`: is there any path by which a pasted character reaches a field or the dispatcher without the hazard filter a keystroke gets? `security-reviewer` is not needed here unless the auditor finds a bypass.

encoding-auditor ran: no BLOCKER. Two MEDIUM findings, both applied — (1) the line splitter only recognized `\n`, so a bare `\r`/NEL/LS/PS paste wasn't split and under-counted its discard (`first_pasted_line` now treats CRLF/`\r`/`\n`/NEL/LS/PS as boundaries, CRLF folded first so it counts as one); (2) a comment in `shortcuts_key` claimed norte never enables bracketed paste, which this task makes false — updated to describe `route_paste`'s rejection as the primary defense and `hostile_key` as the fallback for terminals that don't honor `\e[?2004h`.

- [x] **Step 7: Commit**

```bash
git commit -am "fix(tui): a paste fills the field instead of submitting it (#143)"
```

---

## Task F2: the unbind sees what the bind sees (#141)

**Files:**
- Modify: `crates/norte-frontend/src/keymap/rebind.rs` (add `unbind_dry_run` next to `rebind_dry_run:601`)
- Modify: `crates/norte-tui/src/main.rs:5367` (`unbind_shortcut`)
- Modify: `crates/norte-frontend/src/shortcuts.rs` (the `[global]` row state)
- Modify: `crates/norte-i18n/i18n/{en,es}.ftl`

Read #141 in full (`gh issue view 141`). The three cases are: `[global]` is unreachable, a twin spelling (`mod+p` vs `ctrl+p`) is missed by the byte-exact writer, and another layer may still bind the key after a successful removal.

- [x] **Step 1: Write the failing tests**

```rust
    /// The bind path already repairs a twin spelling — `rebind_dry_run` hands
    /// the writer the spelling that is IN the file. The unbind had no
    /// equivalent, so a hand-written `mod+p` survived an unbind of `ctrl+p`
    /// and the key kept firing.
    #[test]
    fn an_unbind_finds_a_twin_spelling_and_returns_the_files_own() {
    }

    /// Removing the user's entry is not the same as the key going quiet: a
    /// project layer can still bind it. The outcome is worded from the REBUILT
    /// map, so the editor says what the key does now instead of what the file
    /// no longer says.
    #[test]
    fn an_unbind_shadowed_by_a_project_layer_says_the_key_still_runs() {
    }

    /// Nothing to remove is not an error and not a lie: the file did not have
    /// it, and the answer says so.
    #[test]
    fn an_unbind_of_a_key_the_file_does_not_bind_removes_nothing() {
    }
```

- [x] **Step 2: Run and watch them fail**

Run: `just t norte-frontend`

- [x] **Step 3: Write `unbind_dry_run`**

Symmetric with `rebind_dry_run` and reusing its parts:

- Match the target layer's list by PARSED sequence (`parses_to`, already there), not by bytes, and return the file's own spelling so `persist_keymap_unbind` lands on the entry it would otherwise have walked past.
- Rebuild the effective map without that entry and word the outcome from `after.lookup(seq)`: the key now runs another command, or it does nothing, or a layer this editor does not write still binds it.
- Return a type in the shape of `RebindWrite` — the writer needs `section`, `list` and `chords`; the caller needs the outcome. Do not return a pre-rendered sentence: the crate has no locale (that is why `RebindError` carries data, not prose).

Landed as `UnbindWrite { section, chords, command, outcome }` and `UnbindOutcome { NotBound, Cleared, Runs { command, avail } }` — no `list`: `persist_keymap_unbind` searches both `prepend_keymap` and `append_keymap` itself and takes no list argument, so there was nothing for that field to carry. The match to find the entry is by `parses_to` (a twin spelling is found); the REMOVAL from the cloned target is BYTE-exact on `(chords, command)` — mirroring `norte_config`'s own `binding_is` — not a second `parses_to` pass. That distinction was the rust-reviewer BLOCKER in Step 6: see there.

- [x] **Step 4: The `[global]` row**

`Screen::section()` deliberately never answers `"global"`. So the editor marks such a row as global and refuses to edit it, saying where it lives. The row state belongs in `shortcuts.rs` beside the other row states; the wording is Fluent:

```ftl
# en.ftl
shortcuts-row-global = bound in [global]; edit keymap.toml to change it
# es.ftl
shortcuts-row-global = atado en [global]; edítalo en keymap.toml
```

How the editor KNOWS a row is global is the part to get right: the effective map merges `[global]` into every screen, so the row's provenance has to come from the layer the binding was found in, not from the screen the row is displayed under. If that provenance is not available today, adding it is part of this task — an editor that guesses is the bug, not the fix.

That provenance did not exist: `merged_bindings`/`merge_ctx` tagged a binding's `Origin` (preset vs. layer) but not which SECTION it came from. Added a `Section { Specific, Global }` alongside `Origin`, stamped onto `Effective`'s internal `Binding` (`global: bool`) at dedup time — specific wins the merge first, so a sequence bound in both `[pane]` and `[global]` correctly reports `false`. Exposed as `Effective::is_global(seq)`, threaded into `SheetRow`/`ShortcutRow::global` and `ShortcutRow::is_editable()`. `ShortcutsState::begin_capture` itself refuses a non-editable row (not just the TUI's call site), so the GUI's existing blind `begin_capture()` call inherits the same refusal for free.

- [x] **Step 5: Wire the TUI**

`unbind_shortcut` (`main.rs:5367`) calls the dry run, hands the writer the returned spelling, and words the result from the outcome the dry run reported. Its rustdoc currently points at #141 as known residue — that paragraph goes.

- [x] **Step 6: Run and review**

Run: `just t norte-frontend`, `just t norte-tui`, `just c`

`rust-reviewer` on the diff, asked exactly the question above. Found a **BLOCKER**: the first cut of `unbind_dry_run` simulated the removal with `retain(|b| !parses_to(&b.on, seq))` — the same PARSED-sequence match used to *find* the entry, reused to *remove* it. That deletes every entry sharing the parsed sequence, not only the byte-exact one the real writer removes, so a legal, loadable shape (two entries under one parsed chord, different spellings and commands — the loader shadows this, it does not reject it) made the door predict `Cleared` while the byte-exact writer left the shadowed twin bound and firing: the exact "editor says one thing, file does another" defect #141 was filed over, reintroduced through the new door. Fixed by matching the removal on `(chords, command)` byte-exactly (mirroring `norte_config`'s own `binding_is`), with a regression test (`an_unbind_leaves_a_differently_spelled_twin_of_the_removed_entry_standing`) covering both the simulation and the real writer. Also flagged **MAJOR**: `write.outcome`'s wording is computed from the layers as loaded before the write, same as `rebind_dry_run` already documents for the bind ("the writer takes the file lock, this does not") — a hand edit or a second norte in that window can make the shown text stale even though `KeymapWrite::changed` (the boolean gate) stays honest. Accepted as the same pre-existing, documented tradeoff rather than re-verifying post-write (which would need a second file read + rebuild for a window this narrow); documented explicitly in `unbind_dry_run`'s rustdoc and at the TUI call site instead of silently inherited. No other BLOCKER/MAJOR; two MINORs noted and skipped (GUI's own unbind path is untouched, out of scope per this task's file list; `is_global`'s per-row linear scan is O(n²) over binding count, negligible at real keymap sizes).

- [x] **Step 7: Commit**

```bash
git commit -am "fix(tui,frontend): an unbind matches the sequence, not the spelling (#141)"
```

---

## Closing

- [ ] `just ci` — ONE run, at the end.
- [ ] Close #143 and #141 with what was built and what was left.

## Notes for whoever executes this

- **The gate is billed per plan.** `just t <crate>` freely; `just ci` once, mine
  to spend. `just … | tail` returns tail's exit code — read the summary lines.
  `just t` does not run doctests.
- **Bracketed paste is terminal state, like raw mode and the mouse.** Every
  place that hands the terminal to somebody else — suspension, the panic hook,
  restore — has to put it back. There are exactly three, and `mouse.rs` already
  names all three.
- **The GUI is out of scope for #143.** Bracketed paste is a terminal protocol;
  GPUI gets paste from the platform. If the GUI has the same submit-on-newline
  bug, file it rather than fixing it here.
