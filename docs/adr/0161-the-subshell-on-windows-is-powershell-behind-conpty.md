# 0161 — The subshell on Windows is PowerShell behind ConPTY, hooked through its prompt

- Status: proposed
- Date: 2026-09-30
- Decision makers: Oscar González
- Related: ADR 0084 (the subshell), ADR 0157 (Windows builder), ADR 0159
  (the Windows daemon), #142, #363

## Context

ADR 0084 made `app.toggle-panels` a live shell behind the panels, and left
Windows out: the module is `cfg(unix)` and the command declines there. The
TUI already runs on Windows (ADR 0157), so on that platform the key a
Midnight Commander user presses most does nothing.

Everything ADR 0084 decided still holds — the shell gets its own pty, the
cwd is ANNOUNCED by the shell in a private OSC marker authenticated by a
nonce, the `cd` travels through a mailbox file and is never typed, nothing
norte types carries a control byte, the hook is typed in and never written to
a profile. What changes is who reads the input and how a prompt is hooked:

- the pty is ConPTY, which `portable-pty` already drives;
- the default shell is PowerShell (Windows PowerShell 5.1, or `pwsh`),
  whose line editor is PSReadLine, not readline;
- its prompt is a FUNCTION (`prompt`), not `PROMPT_COMMAND`/`precmd`;
- the mailbox cannot be a 0600 file under `$XDG_RUNTIME_DIR`; the per-user
  place is `%LOCALAPPDATA%`, whose ACL already excludes other users.

## Options considered

### A. PowerShell, hooked by wrapping `prompt`

`install` for a new `Shell::PowerShell` defines `__norte_cwd` (read the
mailbox, `Set-Location -LiteralPath`, then write the marker with
`[Console]::Write` and `[char]27`/`[char]7`) and replaces `prompt` with one
that calls it and then the reader's own prompt, saved beforehand. The
mailbox path goes in as `[char]` codes, the counterpart of the octal escapes
on unix, so the typed text stays 7-bit printable. PSReadLine is told to keep
lines starting with a space out of its history, as `HISTCONTROL=ignorespace`
does in bash.

- Good: follows the panel both ways on the shell most Windows users have.
- Bad: a reader whose profile redefines `prompt` AFTER our line (a prompt
  module loaded lazily) drops the hook; the panel then stops following,
  which degrades, never breaks.

### B. `cmd.exe`

`PROMPT` accepts only fixed codes (`$P` is the cwd); it cannot run a
function, so it cannot read the mailbox. It could announce the cwd, not
follow the panel.

### C. Keep declining on Windows

- Bad: the most used key of the orthodox presets stays dead on a platform
  that is now shipped.

## Decision

Option A for PowerShell. Any other shell (`cmd.exe`, a POSIX shell from
MSYS) starts as a LIVE shell without hooks (`which = None`, the path ADR
0084 already has for unknown shells): usable, not followed. The shell is
`$env:NORTE_SHELL` if set, else `pwsh` if found on `PATH`, else Windows
PowerShell. The mailbox lives in `%LOCALAPPDATA%\norte\subshell\`.

## What the ADR 0157 VM measured, and what it changed

- **ConPTY opens with a cursor-position query** (`ESC [ 6 n`) and starts
  nothing until it is answered: PowerShell sat silent. A pseudoconsole just
  created is at `1;1`, so the reader thread answers that ONE query with
  `ESC [ 1 ; 1 R` and removes it from the stream; later position queries
  stay unanswered, as ADR 0084 decided.
- **ConPTY re-renders the console instead of passing its bytes through**,
  and drops an OSC it does not know: the `777` marker never reaches the pty.
  The PowerShell hook therefore ALSO writes where it is to `<mailbox>.pwd`,
  and `Subshell::cwd` reads that file when no marker arrived. The marker
  stays: under `pwsh` on unix the pty is real and it gets through.
- **PSReadLine reads `\n` as Ctrl+J**, a new line inside the same command:
  the typed lines end in `\r`.
- **`app.toggle-panels` had no key in five of the seven presets**, orthodox
  included, on every platform. It is now `Ctrl+O` in orthodox and cua and
  `Ctrl+Z` in vim; krusader and total-commander say in their headers why
  they have none.

## Consequences

- `app.toggle-panels` starts a live PowerShell in the Windows TUI; an
  automated test in the VM checks it announces where it is and follows the
  panel into a non-ASCII directory, and by hand the shell was shown, ran
  commands and changed directory.
- The cwd from the `.pwd` file is UTF-8; a name with a lone surrogate gives
  no cwd rather than a wrong one.
- NOT verified: the full round trip — back to the panels and the panel
  following the shell — in a real Windows console. The VM is driven over
  SSH, whose input reaches ConPTY as VT bytes, and there `Ctrl+O` (`0x0F`,
  Shift In) never arrived as a key; other chords were unreliable too. That
  needs a person at a Windows desktop, which is why this ADR stays
  proposed.
- Found on the way, not fixed here: `ntc` does not switch the console to
  UTF-8 (`SetConsoleOutputCP(65001)`), so in a console left at code page 437
  every box-drawing character renders as three and the screen falls apart.
  The fix needs `unsafe`, which rule 5 does not allow in `norte-tui`.
