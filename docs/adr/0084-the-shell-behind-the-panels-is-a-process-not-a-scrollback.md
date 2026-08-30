# 0084 - The shell behind the panels is a process, not a scrollback

- Status: accepted
- Date: 2026-08-30
- Decision makers: Oscar González
- Related: ADR 0082 (what norte hands to a program it does not own),
  issue #142, help topic `shell`.

## Context and problem statement

`app.toggle-panels` released the terminal and showed whatever the host
terminal's scrollback already held, until the next keypress. The help topic
said so in a section called "What it is not", and pointed at #142.

Midnight Commander does the other thing, and the difference is not cosmetic:
its subshell is a process that STAYS ALIVE behind the panels. Press the key and
you are in the shell you were in last time, with its history, its exported
variables and the half-typed line still there. `cd` in it and the panel
follows; move the panel and the shell follows. That is a different feature
wearing the same key.

Four things had to be decided before any of it could be written, and only the
first is plumbing.

## Decision

### The shell gets its own pty, and the pty lives in the TUI

A shell that must stay alive between two keypresses cannot be a child that
inherits norte's terminal: norte needs that terminal back while the shell is
still running. So the shell gets a pty of its own (`portable-pty`, justified
under rule 8 in `crates/norte-tui/Cargo.toml`), norte reads its output into a
mailbox on a dedicated thread, and "showing" the shell means copying that
mailbox to the controlling terminal and translating keys back into it.

The pty half lives in `norte-tui` because the frontend that OWNS the terminal
is the only one that can hand it over. The GUI has no terminal to hand over and
is not getting this command: it resolves to `Availability::NotHere` there, as
it already did.

The reading thread is a THREAD and not a task: `portable-pty` gives a blocking
reader, and a blocking read in the executor is rule 2. For the same reason
`attach_subshell` is a plain `fn` called through `tokio::task::block_in_place`
— it sits inside its loop for as long as the reader stays in the shell, which
can be minutes, and an `async fn` that never yields would be rule 2 with a
different signature.

### Everything norte types is 7-bit printable, because a pty is read by a line editor

This is the rule the rest of the design hangs off, and it was learned the
expensive way. **What you write to a pty is not read by a shell parser: it is
read by readline / ZLE / fish's reader**, and control bytes are that reader's
COMMANDS, not text. `0x15` is `unix-line-discard`, `0x01` is
`beginning-of-line`, `0x7f` deletes backwards, `0x1b` is the meta prefix.

Two consequences, both of which were live defects in the first draft:

- **Quoting does not protect a control byte.** `cd -- '<path>'` with the quotes
  correctly doubled is airtight against the parser and defenceless against the
  editor, because those bytes never enter the buffer the quotes protect. A
  directory called `<0x15>id #` — legal on any Unix, creatable by an ordinary
  `tar` extraction — turned a quoted `cd` into an executed `id`. So the path
  travels as OCTAL ESCAPES to a `printf` inside a helper function norte
  installs (`__norte_cd`), and the bytes on the wire are digits, quotes and
  spaces.
- **The prompt hook cannot carry raw `ESC` and `BEL` either.** It did, and
  readline ate the `ESC ]` as a meta prefix: what actually got installed
  printed `777;norte-cwd;<path>` with no OSC framing at all. The marker was
  never recognised, the panel never followed the shell — the headline of this
  ADR — and the reader saw that text on every prompt. The hook now writes them
  as `printf` escapes (`\033`, `\a`).

A test pins the rule directly: nothing norte types may contain a byte outside
`0x20..0x7f` except the newlines that send each line.

### norte is the terminal on the other end of that pty, and has to answer as one

A modern shell does not assume what it is talking to: it ASKS and WAITS. fish 4
sends Device Attributes (`ESC [ c`) and a kitty-keyboard query (`ESC [ ? u`)
before painting its first prompt and blocks until something answers. Nothing
did, so under norte fish simply hung — no prompt, no error, no clue. The reader
thread now answers those two, poorly and honestly: a VT100 with options, and
"no, I do not speak kitty". Cursor-position and background-colour queries are
left alone; fish reaches its prompt without them, and answering those means
inventing a number the program will use to place things.

### The shell reports where it is; norte does not guess

The panel can only follow the shell if it knows where the shell went, and there
is no way to ask a running shell that from outside. So norte asks it to SAY:
the shell prints a private OSC marker (`ESC ] 777 ; norte-cwd ; <path> BEL`) in
its prompt, and the reader thread strips those markers out of the stream before
anything is painted.

OSC 7 is the standard way to announce a cwd and would have been the elegant
choice. It is emitted by whoever feels like it — an unconfigured bash does not —
so depending on it would give a panel that follows the shell on some machines
and not others.

**The hook is TYPED IN, never installed in a file.** norte writes the
`PROMPT_COMMAND` / `precmd_functions` / `--on-event fish_prompt` line into the
shell's own input at startup, as if the reader had typed it. Editing a user's
`~/.bashrc` would be a permanent change to their machine in exchange for a
feature that ends when the shell does, and it would survive a crash that
removed norte from the picture entirely. The bash hook APPENDS to any
`PROMPT_COMMAND` already there: the reader's prompt is theirs — and it checks
whether that variable is an ARRAY first, because since bash 5.1 it can be, and
`PROMPT_COMMAND=(__vte_prompt_command)` is what GNOME Terminal ships. A string
assignment there overwrites element 0 and drops the rest without a word.

"Typed in" is not free of traces: the lines land in the reader's history file
and echo on screen. Each is sent with a leading space and the burst begins by
asking the shell to ignore space-prefixed lines (fish already does), so only
that first line is recorded; a `Ctrl+L` afterwards clears the wall of
plumbing off the screen.

The path travels as BYTES the whole way (rule 1), which is why the marker is
scanned out of a `&[u8]` and the `cd` is built out of `&[u8]`. A `printf '%s'`
and not an interpolated `$PWD`, because a directory called `%d` is a directory.

**The marker carries a per-session nonce.** The pty's output stream is partly
controlled by people who should not control the panel: a file named
`…\e]777;norte-cwd;/etc\a…` and an `ls` are enough to make the marker appear
without any prompt printing it — and the marker is not painted, it is OBEYED.
Unauthenticated, that moved the reader's panel to a directory of an attacker's
choosing, right before the next copy or delete aims at it. The nonce is
generated when the shell starts and typed into the hook, so the shell knows it
and a file cannot guess it. It sits in the reader's own scrollback, which is
exactly where it does not matter: whoever can read that terminal has already
won.

**BEL is escaped inside the payload**, because BEL is both a legal byte in a
filename and the terminator norte chose. Without escaping, `/tmp/a<BEL>b` was
announced as `/tmp/a`, and if that existed the panel followed the shell to a
directory the shell was not in — silently. The hook doubles `DLE` and writes
BEL as `DLE G`, using each shell's own builtin substitution, so it costs no
fork per prompt.

**The announcement must be absolute.** A relative one cannot come from `$PWD`,
and following it would resolve it against norte's own process directory.

**An unterminated marker is capped.** A stream carrying the prefix and never a
BEL — a `cat` of a binary — used to swallow everything the shell printed from
that point on: nothing painted, memory growing at pty speed, a shell that
looked hung. Past 8 KiB it was not a marker, so it is painted as the text it
is.

### The key that takes the panels back is the key that gave them away

It comes from the KEYMAP (`detach_chord`), not from a constant. The presets do
not agree on it — `norton` and `far` both say `Ctrl+O`, another preset may move
it — and a hard-coded `Ctrl+O` would leave the reader of a remapped preset
inside a shell with the panels alive behind an unresponsive screen.

**Only a single chord serves.** A two-chord sequence cannot be recognised
without putting the whole resolver inside the pty loop, and more importantly it
must not be: the first chord of the sequence would have to be stolen from the
shell, which is exactly where the reader is typing. Bound to a sequence, the
command says so (`msg-subshell-no-key`) and hands over nothing — handing over a
terminal with no way back is the worse failure.

### The `cd` is typed only when the shell is provably idle

Norte types into the same line the reader types into, so it must not type while
there is anything in it. The rule is: a prompt marker has arrived and NOBODY
has written to the shell since — the reader's keys go through the same write
path, so a half-typed line lowers the flag and only the next prompt's marker
raises it.

The obvious formulation — "the marker was the last thing in the chunk" — is
wrong, and cost a debugging round: the hook runs BEFORE the shell paints its
prompt (`PROMPT_COMMAND` then `PS1`), so something always follows the marker.

Without this the `cd` was appended to whatever was there. A reader who typed
`rm -rf tmpdir`, thought better of it, and pressed the toggle key got
`rm -rf tmpdircd -- '/somewhere'` executed on the way back — a recursive delete
from a key that is supposed to toggle a view. And if what was in front was
`vim`, the bytes went into the buffer. The cost of the rule is that the panel
does not push its directory into a busy shell, which is the right answer
anyway.

### It starts late and dies with norte

Lazy: never press the key and no shell is ever forked, no pty opened, no thread
spawned. A shell the reader ended with `exit` is REPLACED on the next press
rather than resurrected — writing to a dead child's pty fails, and without this
the key would quietly stop working for the rest of the session.

Killed in `Drop`, not in the `app.quit` arm. The run loop also leaves through
`RunError` — the terminal or the event stream breaking — and a shutdown that
only covered the clean exit would orphan a shell precisely when the terminal
went wrong.

### It refuses on a remote pane, and that is a change

The old `app.toggle-panels` worked on an SFTP pane because it launched nothing.
This one launches a local shell, so it declines there with the same message
`app.terminal` uses, for the same reason: a shell opened "there" would silently
be somewhere else.

### Nothing of this reaches the journal

Same rule as ADR 0082 and the rest of #135: a shell the reader opens is the
reader acting with their own permissions. There is no actor to attribute and no
reversal to record, and inventing entries for it would put irreversible rows in
a journal whose contract is that its rows can be undone. The audit trail is one
`tracing::info!` line naming no command — the command line is the reader's.

## Consequences

- `app.toggle-panels` no longer works on a remote pane. The help topic says so.
- While the shell has the terminal, norte is not running: no watch, no task
  tick, no answer to an agent's approval request. Unchanged from the
  suspension, but the exposure is longer now that the shell is meant to be left
  open.
- Output the shell produces while nobody is looking is buffered, capped at
  256 KiB, keeping the TAIL. A `find /` left running does not grow without
  bound; what it printed an hour ago is gone.
- Unix only, and enforced: the module is `#[cfg(unix)]` and Windows takes a
  refusal path with its own message. Without the gate the crate did not compile
  there at all, and this ADR claimed a refusal that did not exist.
- The reader's keys are translated, not proxied, so what the shell gets is what
  `tecla_a_bytes` knows how to say. Function keys and the non-letter control
  chords are in; the mouse is not. A paste IS forwarded — the argument that "a
  shell does not ask for one" stops holding the moment the shell has a `vim` in
  front of it.
- The exit chord is taken from the shell, never the other way round: under the
  `norton`/`far` presets `Ctrl+O` never reaches the subshell, where nano reads
  it as "write out".
- Comparing "where the shell ended up" against "where the panel was" NORMALISES
  while keeping the real bytes. `$PWD` is the string the shell was given, not a
  re-read of the disk, so on macOS a reader who types an NFC path gets a `$PWD`
  that never byte-matches the NFD `VPath` norte read from `readdir` — every
  toggle would relist, and the panel would end up holding a `VPath` that its
  own history, hotlist and marks no longer recognise.
