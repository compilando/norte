# 0153 — The subshell is moved through a mailbox, not by typing

- Status: accepted and implemented
- Date: 2026-09-24
- Decision makers: Oscar González
- Protocol: unchanged. Bridge: unchanged. One new file per subshell session,
  mode 0600, under the user's runtime directory.
- Related: #363, #142 (the panel follows the subshell), #360, ADR 0084

## Context and problem statement

`Ctrl+O` opens a shell in a pty and the panel follows it: `cd` in the shell and
the panel moves, move the panel and the shell follows. The second direction was
implemented by **typing a command at the shell**: norte wrote
`__norte_cd '\057...'` plus a newline into the pty and the line editor executed
it.

Typing into a line editor is only safe if the buffer is empty, and there is no
way to ask a shell whether it is. norte approximated it with a flag — *a
nonce-authenticated prompt marker arrived and nothing has been written since* —
and that is a different statement. `security-reviewer` found two reachable
states where the flag is up and the buffer is not empty:

**zsh's buffer stack.** `push-line` (Ctrl+Q in the default emacs keymap) pushes
the current line aside, the shell prints a fresh prompt — marker, flag up — and
then pops the line back into the editing buffer. A `print -z` from one of the
reader's own functions reaches the same state with no keystroke at all.

**Type-ahead, in all three shells.** What the reader types while the shell is
busy waits in the pty's input queue. The marker arrives with those bytes still
unconsumed: nobody wrote *since the marker*, and the buffer is not empty.

In both, the `cd` concatenates onto what the reader left and the shell executes
an order nobody gave:

```
rm -rf tmpdir __norte_cd '\057...'
```

That is #142's original bug, reachable again. Both predate the #360 fix, which
only stopped Ctrl+L from lowering the flag.

## Decision

**Stop typing. The destination goes in a file; the prompt hook picks it up.**

1. Each subshell session gets a **mailbox**: a file named
   `subshell-<nonce>.cd`, created mode 0600 under `$XDG_RUNTIME_DIR/norte`
   (already 0700 for the user), falling back to `/tmp/norte-<uid>` — the same
   place and the same reasoning as the daemon socket. The mode is set **at
   creation**, not fixed afterwards: a file that is born 0644 has a window in
   which another user can open it, and what is written here sends a shell
   somewhere.
2. `Subshell::ir_a` writes the destination's raw bytes plus a `_` sentinel into
   that file, through a temp file and a `rename` in the same directory. The
   hook runs at *every* prompt, so a half-written path would be a `cd` to a
   place that is not there; `rename` makes the shell see the whole path or the
   previous one.
3. **The `cd` moves into the prompt hook**, ahead of the marker it already
   prints. One function and one registration instead of two, and the announced
   directory is now where the shell *ended up* rather than where it was.
4. The flag disappears, and with it the Ctrl+L exemption (#360) that existed
   only to avoid lowering it. There is no permission left to protect.
5. Without a mailbox nothing is installed: the hook carries the mailbox's path
   inside it, and a hook pointing at a file that does not exist is plumbing
   typed in the reader's face for nothing. The panel then simply does not drag
   the shell — the honest degradation, better than crashing and better than
   going back to typing.

The mailbox path travels inside the hook as octal `printf` escapes, exactly as
the `cd` destination used to: what norte writes to the pty must not contain a
single control byte, or the line editor interprets it.

## Consequences

**The injection channel is gone.** There is nothing to concatenate onto,
whatever the line editor happens to be holding. That closes both reported
states and every unreported one of the same shape, which is the point: the flag
was an approximation and approximations of "is this buffer empty" will keep
having holes.

**A panel move now applies at the shell's next prompt, not immediately.** This
is the cost, and it is real: after a Ctrl+L, or while the reader is halfway
through typing, the shell stays where it is until it reaches a prompt. It is
also the honest semantics — the hook runs between commands, which is exactly
when a `cd` is safe — and it is what `ir_a` already did every time it refused.
Before, the move was immediate *only when it was already safe*; now it is
deferred always.

**The reader's half-written line survives untouched**, which this subshell
already promised and now actually delivers: norte never writes to their buffer.

**One more file per session.** A few dozen bytes, removed when the subshell is
dropped. If norte dies outright it stays in a directory the system clears at
logout.

## Alternatives considered

**Harden the flag.** Track the line editor's state more closely — count bytes
written versus consumed, parse the echo. Rejected: it is unobservable from
outside the pty, every refinement is another approximation, and the two states
found were found by reading documentation, not by fuzzing. There is no reason
to believe they are the last two.

**Bracketed paste.** Wrapping what norte writes in `ESC[200~`/`ESC[201~` makes
the line editor treat it as text rather than commands — which is the opposite
of what is wanted here: the `cd` has to run.

**Write the `cd` only when the buffer is provably empty**, by asking the shell.
No portable way to ask, and the answer would be stale by the time it arrived.

**A named pipe instead of a file.** The hook would block on open when norte has
nothing to say, which turns every prompt into a hang.
