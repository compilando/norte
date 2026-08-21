# 0070 - A mutation names its own operands, and the surface that approves it labels out of band

- Status: accepted
- Date: 2026-08-21
- Decision makers: Oscar González
- Related: ADR 0058 (a screen is a tree the core keeps and does not read, D7),
  ADR 0061 (a configuration name that becomes a filename is bytes), ADR 0066
  (renderers use a Rust UI host, D6 and D11), ADR 0068 (a row is named by key
  and generation), ADR 0069 (how image bytes reach the webview), the phase 5.1
  entry in
  `docs/superpowers/plans/2026-08-19-multi-frontend-tauri-transition.md`.

## Context and problem statement

Phase 5 turns the graphical window from something that looks into something
that writes. Task 5.1 is the everyday half of it: copy, move, create and
delete, through the same daemon, journal and policy path the TUI already uses.

Three of those four already existed. `pane.mkdir` and `pane.delete` were built
in phase 2 and enqueue a real Task. What copy and move add is not a new kind of
effect — it is a **second operand**. Deleting names one thing; copying names a
source *and* a destination, and the destination is not in the pane the user is
looking at.

That is where the questions are, and they are not questions about copying:

**Who names the destination?** The renderer lives in a webview and is
untrusted by design (ADR 0066 D11). ADR 0069 already settled that no path
crosses from it — the image-bytes command has no parameter at all for exactly
this reason. A destination is a path. So either the renderer names it, which
reopens what 0069 closed, or the host derives it, which means the host needs a
rule for *which pane is the destination* that is right with three panes and not
only with two.

**What does the confirmation actually say?** A confirmation is the last
surface where a person can say no. It has to carry a destination, a list of
sources, and the fact that both come from whoever wrote in those directories.
The existing delete dialog carries a flat `Vec<String>` body, and that shape
cannot express any of it: the destination would be a line among lines, and a
list too long to fit would be silently short.

**What does the screen do when the operation ends?** A copy that succeeds
leaves every pane showing that directory wrong. Refreshing them is obvious;
what is not obvious is that refreshing is itself a mutation of the reader's
state — it moves their cursor and drops their selection — and that it races
with whatever navigation they were doing while the copy ran.

The corpus had already written down the answer to the second question, in a
fixture nobody had needed yet. `arrow_join_spoof` is the bytes
`a → mem_b.txt`, and its `why` reads: *"contiene el joiner visual '→' (U+2192,
legítimo, no enmascarable) — un nombre simula DOS rutas en cualquier UI que
concatene con separador in-band; etiquetar fuera de banda (posición/estilo),
jamás por joiner"*.

## Decision drivers

- Hard rule 1: filenames are bytes. A name that goes to a screen and comes back
  is a different name (ADR 0061).
- Hard rule 4: every mutation goes through the journal with an undo path.
- ADR 0066 D11: the webview is unprivileged. Its authority is what the host
  grants it, not what it asks for.
- ADR 0058 D7: with several panes, a destination the engine breaks a tie for is
  silent data loss.
- "What is masked is said": the project's uniform criterion, and this is the
  one screen where a masked name is *approved*.

## Considered options

### Naming the operands

**A. The renderer sends the paths.** The webview knows which rows are marked
and which pane is on the right; it could send `{from: [...], to: "..."}`.
Simple, and every web application does it.

Rejected. It reopens ADR 0069's decision one task after taking it, and the
consequence is worse here: a compromised renderer that can name a destination
can name `~/.ssh` and a source it never displayed. It also breaks rule 1 —
what the renderer holds is `display_name`, already masked and clamped, so the
bytes would have to be re-derived and could not be.

**B. The host derives both from slot state.** The renderer sends
`pane.copy` and nothing else. Sources come from the focused slot's marks (or
its cursor); the destination is the directory of the slot holding
`RoleId::Target`.

Chosen. It costs a rule for what `Target` means with more than two panes, which
is option set two.

### Which pane is the destination

**C. The lowest-numbered other visible pane.** What the host actually did.
Cheap, and indistinguishable from correct while every factory layout has
exactly two browsers.

Rejected once a mutation reads it. With three panes it guesses, and — worse —
it recorded its guess through `Roles::set`, which marks the target
*explicit*, i.e. "a person chose this". So a destination someone had designated
by hand was overwritten on every focus change, by a value that then claimed
human provenance.

**D. The shared rule from ADR 0058 D7.** `norte_frontend::layout::roles::Roles::reconcile`
already implements it: an explicit target survives while it stays a candidate;
with exactly one candidate the engine may assign it, unmarked; with several and
none chosen the role is **cleared**.

Chosen. It is decision D14 of the plan applied literally — no second
implementation of a presentation rule. With the role cleared, the transfer
refuses and says *designate a destination first*, which is a different sentence
from *there is no other panel*, because they are different situations.

### How the confirmation is laid out

**E. Everything in the body.** Destination as line zero, prefixed with `→`;
truncation as a final line; masked names as plain strings. What the delete
dialog does today, extended.

Rejected on all three counts, and the corpus says why. `→` is U+2192: it is
legitimate in a filename, it is not a terminal hazard, so `display_name` does
not mask it and does not flag it. A directory named `docs → /casa/BORRAR`
produces the line `→ ⟨mem⟩/casa/docs → /casa/BORRAR`, and a reader who parses
"arrow, then path" reads the last pair. With F6 the originals are then gone
from where they were and are not where the user believes. The same argument
retires the truncation line: a filename can impersonate it.

**F. Out-of-band labelling.** The destination gets its own field and its own
element. The truncation notice gets its own field, translated in Rust. The body
becomes lines, each carrying whether what is painted differs from the bytes.

Chosen. This is bridge version **23**, and it is one shape change for four
facts.

### What a refresh is allowed to disturb

**G. Re-list and let the pane restore itself.** `set_listing` already restores
a cursor from its per-directory memory.

Rejected. That memory is an **index**, and a refresh is precisely the case
where the index no longer names the same file: the operation removed or added
an entry. A pane whose cursor walks onto a different file, with no keystroke
from the reader and no visible cause, is a wrong-file destructive operation
waiting for the next F8. `set_listing` also clears marks, which is right for a
`cd` and a punishment for someone who did not move.

**H. Refresh by identity, and never over an in-flight request.** The cursor is
pinned with `set_pending_focus` (byte-exact, self-consuming); the marks are
re-applied by path and what the operation removed is simply not re-marked; and
a slot with a request already in flight is skipped entirely.

Chosen. The last part is the one that is not obvious: a refresh reserves a
fresh token, so the in-flight navigation's response would arrive with a stale
one and be discarded — the pane would sit in the directory the reader had just
left, having recorded the trail entry, in silence. Losing a refresh is a
slightly old screen; losing a navigation is the application moving on its own.

## Decision

For a graphical frontend over `norte-ui-host`:

1. **The renderer never names an operand.** It sends a semantic command; the
   host derives sources from the focused slot's marks (or cursor) and the
   destination from the slot holding `RoleId::Target`. The final path is
   composed in Rust as `destination.join(source.file_name())` — the source's
   last segment verbatim, bytes, never through a display function. A root
   (`file_name() == None`) rejects the **whole** batch rather than being
   skipped: transferring "almost everything you asked for" in silence is what a
   mutation must not do.

2. **The destination role follows the shared rule**, `Roles::reconcile`, and is
   never assigned by a rule local to the host — including at startup, where
   assigning it by hand was what marked the engine's own guess as a human
   choice. With several candidates and none designated, the transfer refuses.

3. **A pending mutation captures its operands, resolved, at the moment the
   question is asked** — the destination directory, the source paths, *and the
   source slot*. `UiAction::FocusSlot` is not gated by an open dialog (only
   keys are), so between asking and answering the focus can move. The
   destination was already captured for this reason; the source slot was not,
   and its marks were being consumed from whichever pane happened to be active
   at confirm time.

4. **A decision surface labels out of band.** The destination, the truncation
   notice and each line's hostility are separate fields, never a separator, a
   position or a sentence inside the body. This is the corpus rule
   (`arrow_join_spoof`, `cause_join_spoof`) applied to the graphical dialog.

5. **The collision policy is `Fail`**, the wire's safe default. Overwrite and
   rename are the reader's decisions and this window has nowhere yet to take
   them; choosing on their behalf is the kind of silence that deletes files.

6. **A terminal mutation refreshes the directories it changed**, identified
   when the task is enqueued and not deduced from progress. A slot counts as
   affected by where it is *heading* if it has a request in flight, and by what
   it shows if it does not. A hidden slot is marked loading rather than
   re-listed. Cursor and marks survive by identity. While another live task
   touches the same directory, nothing is re-listed.

## Consequences

### Positive

- The property ADR 0069 established for image bytes now covers every mutation:
  `UiAction` contains no `VPath` and no path-bearing `String`, so a compromised
  renderer cannot name a file it was never shown. Path traversal from a hostile
  name is impossible by construction — `Segment::new` rejects empty, `/`, NUL,
  `.` and `..`, and `join` only pushes validated segments.
- The window and the TUI agree about what a destination is, because they read
  the same function.
- Four masking gaps closed at once, including one that predated this work: the
  approval dialog for an agent's operation cited `modal-approval-truncated`, a
  Fluent key defined in neither locale, so a truncated batch painted the raw
  identifier.
- Refreshing became something the reader does not notice, which also fixed the
  column picker's re-list — it shared the defect and nobody had looked.

### Negative

- Bridge **22 → 23**, and old renderers break on purpose (ADR 0068). The four
  flags of issue #266 did not travel with it, so the next shape change will
  spend another version on them.
- With three or more browsers and no designated destination, F5 now refuses
  where it previously copied somewhere. That is the intended reading of ADR
  0058 D7, and it is a behaviour change for anyone with a custom layout.
- The batch is enqueued serially, so a very large transfer takes longer to
  appear complete in the task board than it did to fire off. That is the point
  — the previous shape opened one RPC per marked entry — but the board still
  retains one row per entry, which issue #271 tracks.
- Two byte-exact comparisons remain, and both are recorded rather than fixed:
  the same-directory refusal (`host-same-directory`) and the core's
  `is_descendant`. Neither loses data today, because `norte_core::ops::same_node`
  folds under the destination's real capabilities; issues #268 and #269 carry
  the rest.
- The window still does not write. `EFECTOS` stays `SoloLectura` until task
  5.4, and there is now a test that says so — the whole barrier had been one
  constant nothing looked at.
