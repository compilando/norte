# 0077 - The same command means the same thing in both frontends

- Status: accepted
- Date: 2026-08-26
- Decision makers: Oscar González
- Related: ADR 0076 (creating a file is a mutation like any other), ADR 0066
  (D10 all effects through the daemon), ADR 0065 (the retired GPUI frontend),
  issues #290, #140, #133.

## Context and problem statement

Two commands did different things depending on which frontend ran them, and
both divergences were invisible to the test suite because each frontend tested
its own half and both halves were green.

**`pane.disconnect`.** The window walks the panel's trail backwards and lands on
the most recent place that is not on the machine it just released; only if the
whole trail is that machine does it fall back to home. The TUI went home,
always. Same key, same name, two destinations. The decision existed twice in the
source — `Controller::donde_volver_tras_desconectar` and an inline block in
`gestures::disconnect` — which is how they drifted.

**`pane.edit-new`.** ADR 0076 added `fs.create` so the window could create a
file through the daemon and open the editor on it: policy gate, journal
`Created`, undo. That ADR also recorded, as a fact and without arguing with it,
that "the TUI never needed one" — it launched `$EDITOR` with an empty buffer and
let the editor create the file at save time.

That is a hard rule 4 violation wearing a terminal. The file appears on disk,
attributed to nobody, with no journal entry and no undo. Worse, it appears
*even when the policy would have refused it*: there is nothing to refuse,
because norte never asked. And the same command in the other frontend does the
governed thing, so "did this file go through the journal?" has no answer until
you know which window the human was in front of.

## Considered options

### 1. Leave the TUI as it was and document the divergence

Cheapest, and defensible on one point: a shell or an editor that the human
launches is the human acting with their own permissions, which is exactly the
reasoning that keeps `app.terminal` and `pane.command-line` out of the journal
(design §D). Whatever the human's shell writes, norte does not claim.

It breaks down on the operand. `app.terminal` hands over a *directory* and makes
no promise about what happens next. `pane.edit-new` names a *file that does not
exist yet* and the entire point of the gesture is bringing it into existence.
That is a mutation norte requested, and the process it delegated to is an
implementation detail of how the bytes get typed.

### 2. Make the window match the TUI

Impossible and undesirable: a window has no terminal to hand a process to, and
ADR 0066 D10 forbids the host process from touching the filesystem. This option
only exists to be named.

### 3. Make the TUI match the window, and hoist the shared decision

The TUI asks for the name, calls `fs.create`, and opens the editor on the
created path when the task completes. The disconnect destination becomes one
function in `norte-frontend` that both frontends call.

Cost: `pane.edit-new` in the TUI grows a dialog it did not have, which is a
change in feel for anyone used to mc's Shift+F4.

## Decision

**Option 3, for both commands.**

### The TUI creates through the daemon, then opens the editor

The name is asked *before* anything is created, and the refusal on a remote
panel is said before the name is typed — same order as the window, and for the
reason ADR 0076 gives: what opens afterwards is a program on this machine, and
it cannot be handed an `sftp://`.

The editor opens on the **task's successful outcome**, never at submit time.
This is the part that carries the whole decision: an editor launched over a file
that is not there shows an empty buffer and creates it on save — which is
indistinguishable from success, and is precisely the behaviour being removed. A
creation that fails — policy, journal unavailable, a name the provider refuses,
a taken name — opens nothing.

The intention is remembered with the **task id**, and only that task's outcome
redeems it. Any other task finishing in between — a copy, a delete, another
creation — leaves it alone. Without the id, the editor opens on the wrong file
the first time two tasks overlap.

`norte-frontend::shell::login_shell_editor` — the editor with no file — is
**removed** rather than left unused. Its only purpose was creating a file behind
norte's back, and a function that exists solely to do that is an invitation to
do it again.

### Where a disconnected panel goes is one decision, in one place

`norte_frontend::nav::regreso_tras_desconectar` takes the released path and the
panel's trail and returns the most recent entry that is not on that machine —
scheme *and* authority, so another server on the same scheme is a legitimate
destination. `None` means nothing in the trail qualifies, and the caller falls
back to `shell::home_vpath`.

That fallback also stops going through `to_str()`. A `$HOME` that is not UTF-8
is a perfectly valid home (rule 1), and lossy-decoding it sent the reader to `/`
without saying why.

## Consequences

- `pane.edit-new` in the TUI now asks for a name. It is a visible change for a
  Shift+F4 habit built on mc, and the help topic says why in both languages: the
  file is created by norte, so it is norte that has to name it.
- Every file created by that command is now in the journal with an undo,
  whichever frontend created it, and the policy gate applies to both.
- A `pane.edit-new` in the TUI now costs a round trip to the daemon before the
  editor opens. On a local panel this is a task that creates an empty file; it
  is not measurable against the editor's own startup.
- Two decisions that were written twice are now written once, so the next
  divergence has to be introduced on purpose rather than by editing one copy.
- No protocol change: `fs.create` and `TaskKind::Create` already exist since
  0.57.0 (ADR 0076). This is the second caller.
- The trail-based destination inherits a property worth stating: the trail is
  per-panel session state, so a panel restored from a session file disconnects
  to whatever its restored trail says, not to where the previous run's window
  would have gone. Both frontends now agree on that too, because it is the same
  code.
