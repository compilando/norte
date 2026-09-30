# 0162 — `norte-winpipe` also holds the console's code page, and norte speaks UTF-8 to it

- Status: accepted
- Date: 2026-09-30
- Decision makers: Oscar González
- Related: ADR 0159 (`norte-winpipe`, hard rule 5's second exception), ADR
  0161 (the Windows subshell)

## Context

`ntc` writes UTF-8. A Windows console reads what a program writes in its
output code page, which in Windows Terminal and conhost is the OEM one — 437
or 850 — unless the system locale opts into UTF-8. Seen by a person at the
ADR 0157 VM's desktop: every box-drawing character came out as three
(`Ôöé`), the widths stopped matching and the whole screen fell apart. With
`chcp 65001` typed first, it rendered correctly. `norte` has the same
problem with any non-ASCII file name it prints.

The fix is `SetConsoleOutputCP(CP_UTF8)` (and `SetConsoleCP` for input),
restored on exit so the console the reader goes back to is the one they
had. Both are FFI calls, and hard rule 5 allows `unsafe` only in
`norte-vfs-local` and `norte-winpipe`.

## Options considered

### A. A `console` module in `norte-winpipe`

- Good: no new crate and no new exception to rule 5; the crate already has
  the `unsafe` discipline (deny + allow per item + `SAFETY`) and a Windows
  test job in the VM.
- Bad: the name says "pipe". Its description and this ADR widen it to "the
  Windows boundaries norte needs `unsafe` for".

### B. A new crate for it

- Good: a name that says what it holds.
- Bad: a third exception to rule 5 for two function calls, and a third
  place to audit.

### C. Run `chcp 65001` as a child process at start, and again to restore

- Good: no `unsafe` at all.
- Bad: parsing `chcp`'s output to learn the old page depends on the
  display language, a spawn at every start, and a restore that a crash
  skips just the same.

## Decision

Option A. `norte_winpipe::console_utf8()` switches input and output to
UTF-8 if there is a console and returns a guard that restores the previous
pages on drop; `ntc` and `norte` hold it for the whole run. Off Windows it
does not exist and nothing is called.

## Consequences

- `ntc` renders correctly in a console left at its OEM code page; the
  reader's console gets its page back on a normal exit.
- An abnormal exit (a crash, a kill) leaves the console at 65001, which is
  harmless to a modern console and what `chcp 65001` would have left.
- `norte-winpipe` is described as the Windows `unsafe` boundary, not only
  the pipe's; rule 5 in `CLAUDE.md` names what it now holds.
