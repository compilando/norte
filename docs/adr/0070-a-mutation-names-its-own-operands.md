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

### Extended by task 5.2

Renaming reached the same three rules from a different direction, and they are
worth recording next to the ones above because the second one is not obvious:

- **A single rename** (`shift+F6`) seeds its field with what the row PAINTS,
  and an untouched field reconstructs the ORIGINAL BYTES — which makes the
  destination equal the source, so nothing is renamed. That is the protection
  rather than a gap: the seed is a screen projection, for a name that is not
  UTF-8 it is not reversible, and it must never become the operand. A touched
  field still containing U+FFFD is refused, and a name whose projection does
  not fit on screen cannot be edited here at all (the clamp appends `…`, a
  legal filename character that nothing masks and nothing flags). "Untouched"
  is recognised by comparing against the seed rather than by a flag, because
  the renderer sends the whole text on every event, not a delta. The honest
  consequence: a name that is not valid UTF-8, or that is longer than the
  display budget, cannot be renamed from this window.
- **A plan proposed by a model** is validated whole before it is shown, and the
  core's verdict is a second trip — so the review opens in a "checking" state
  and fills itself in. Approving sends the `plan_hash` the core returned: what
  executes is exactly what was displayed.
- **A pending request carries its own directory.** A plan asked for `series/`
  and landing while the reader is in `descargas/` would open promising to
  rename what is on screen and rename something else. The epoch that discards a
  stale answer is not enough on its own; the operand has to travel with the
  request, which is the same rule as point 3 above one layer down.

### Extended by task 5.3

The board, the approvals and the two reports pushed the same rules one step
further, and three of them are worth writing down:

- **A surface that opens BY ITSELF does not get answered by the next
  keystroke.** Task 5.2 learned this for the AI plan review; it is now a
  property of the dialog itself (`reconocido`), and the approval dialog and the
  batch/undo reports carry it. A dialog opened by a gesture is born
  acknowledged, because there the next key IS an answer. `Escape` is exempt in
  both cases: getting rid of something you did not ask for has to work first
  time.
- **An approval has a deadline, so the surface has one too.** The TTL is shown,
  and when it runs out the dialog closes itself with a notice rather than
  sitting there inviting an approval the daemon will no longer accept — which
  would leave a human believing they authorized what was in fact denied by
  silence. No `policy.decide` is sent on expiry: the daemon already resolved
  it, and answering a closed id only produces an error that means nothing to
  the reader. The same approval arriving twice does not open a second dialog,
  because the SDK resyncs `policy.pending` on every reconnect and two dialogs
  would be two answers to a question that admits one.
- **A terminal state is not an outcome.** Two task classes carry a report —
  a rename batch and an undo — and in both the report is the ONLY account of
  what stayed half done. It is asked for even when the task says `Completed`
  (the task's state talks about the operation, the report talks about the
  disk), and for FOREIGN tasks too: a half-renamed directory is the same
  directory whoever started the work. When the report cannot be fetched, "the
  outcome is unverified" is said as its own fact — never folded into "it went
  fine". What the report contributes that nothing else can is the name the
  file carries NOW, which is the only actionable thing in it, so it travels as
  a masked path line and never inside a sentence where another path could
  impersonate it.

Two presentation rules moved to `norte-frontend` on the way (D14): the
plaintext-session banner, and the progress percentage — which had already
diverged, since only the TUI's copy fell back to entry counts, so a delete
crossed the bridge with no progress to paint at all.

### Closed by task 5.4: the window writes

`EFECTOS` is `Completo`. What the audit added to the rules above is one
sentence, and it applies to any indicator, not just these:

**An indicator that cannot turn itself off lies.** The journal notice says
"this session does not record", which is a statement about everything that
follows; a daemon that refused once may recover, and nothing announces it. An
accepted mutation is the proof, so it clears the notice. The same shape
appears twice more in this phase: the daemon-going-away banner clears when the
daemon returns, and an approval that expires closes its own dialog instead of
waiting for someone to answer a question that no longer exists.

Its mirror image is **"I said it" is not "it arrived"**. `policy.decide` is
sent and forgotten; approving when the daemon has died left the window
believing it had authorized what stayed denied by silence. Approving now says
when it does not land. Denying does not need it: if that message is the one
lost, the outcome is still the one that was asked for.

### What the reviews added to the rule

Three findings from the 5.3/5.4 reviews generalise past this ADR's subject,
and all three are about the same thing — an invariant that holds on one path
and not on its twin:

- **A rule that only covers the keyboard does not cover the surface.** The
  acknowledge guard for self-opening dialogs lived in the key path while the
  pointer is the primary input of a window. Guards belong where the two inputs
  meet, not on the one you were thinking about when you wrote it.
- **A masked string cannot be detected by masking it again.** The daemon
  redacts paths before sending them, so the host's "did masking change
  anything?" test answered no for exactly the bytes that had been replaced.
  U+FFFD is itself the signal; where it comes from a wire that already
  redacted, its presence is the flag.
- **An index is only as good as the list it indexes.** The board is capped
  before it crosses the bridge; the cursor counted over the uncapped map. Any
  cap plus any index needs one definition of "the visible ones", and the
  invariant belongs in a function rather than in three call sites.

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
