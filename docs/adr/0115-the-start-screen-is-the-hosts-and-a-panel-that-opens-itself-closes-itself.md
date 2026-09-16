# 0115 — The start screen is the host's, and a panel that opens itself closes itself

- Status: accepted
- Date: 2026-09-16
- Decision makers: Oscar González
- Related: ADR 0058 (a screen is a tree), ADR 0066 (the UI host), ADR 0077
  (parity), ADR 0106 (the chrome is derived, not drawn), spec
  `2026-09-15-historia-y-wow-design.md` (phase 2), plan
  `2026-09-16-fase2-splash-y-cromo.md`

## Context and problem statement

Phase 2 of the WOW programme adds three things a reader meets in the first
minute: a start screen, a processes panel that does not sit there saying
"nothing running", and three pieces of chrome that were wrong in the
screenshots Oscar sent (the `/` in front of every directory, a pane footer
painted in the dimmed colour of an unfocused border, and `1Ayuda` with no
space).

Three questions had no obvious answer:

1. **Where does the start screen live?** The window paints nothing until
   `main.ts` runs and the first snapshot arrives — `index.html` has no loading
   markup at all. A splash drawn by the webview alone would appear earlier, but
   it could not say where you were last, which is the only thing that makes a
   start screen worth the keystroke.
2. **How does a panel open itself?** Both frontends only have a TOGGLE
   (`toggle_processes`, `alternar_hueco`). Reusing it for "a task started" would
   close the panel when the second task starts, and reopen the one the reader
   just closed.
3. **Where does speed come from?** `TaskProgress` carries `bytes_done` and
   totals, and no rate and no timestamp.

## Decision

**D1 — The start screen is host state, in both frontends.** It is built where
the session, the popular directories and the bookmarks already live, and it
crosses to the window as one more overlay view (`ViewSnapshot.splash`), the way
the wizard does. The webview gains no splash of its own: two start screens
would drift, and the one that knows something is this one.

**D2 — `[ui] splash = brief | off | home`.** `brief` (default) is a cover any
key takes away, and the run loop drops it once the first listing is up and
`SPLASH_BRIEF_MS` (1.2 s) has passed on the PAINTING clock. It carries no rows:
a cover that removes itself is no place to offer a choice. `home` stays until a
key and numbers its rows across sections (1–9); `off` paints nothing.
`--no-splash` and `NORTE_NO_SPLASH` turn it off for one run, and the first-run
wizard wins over it — of two things that would cover the first frame, the one
that asks a question goes first.

**D3 — The sections come from a registry.** `SplashSource` is a trait, and a
source with nothing to say takes no space. Today: popular directories and
bookmarks. A plugin-contributed section (phase 3's `panel` kind) plugs in the
same way, without touching whoever paints.

**D4 — Auto-open is two halves, never the toggle.** `open_processes(with_keys)`
/ `close_processes` in the TUI, and the same split in the window. `[ui]
processes_panel = auto` opens the panel when the board grows its first row and
closes it when the last row is gone — and only if the AUTO opened it: a panel
a person opened stays open. Opening this way does NOT take the keyboard: the
reader is in their listing, and a copy starting is no reason to take the arrow
keys away from them. The command keeps taking it, because someone pressing it
is about to act on a task.

**D5 — Speed and ETA are computed by whoever is looking.** `norte_frontend::
tasks::Rate` keeps an exponential average over successive snapshots with the
painting clock, drops samples it cannot measure (no elapsed time; a counter
that went backwards after a resume) and answers `None` rather than zero. It is
per task, not per board: two copies run at different speeds and an average of
the two describes neither. This is not a wire change, and deliberately so —
a rate is a property of the observation, not of the operation.

**D6 — `[ui] dir_indicator = auto | slash | none`.** `auto` drops the `/` in
front of a directory when the icon column is open, because the icon already
says what the row is and the slash costs a cell of the name. The `@` of a
symlink stays: no icon says that.

**D7 — Chrome that was lying.** The pane footer is painted with ITS pane's
border style (an active pane's footer was unreadable in the dimmed colour), and
a key-bar cell puts a space between the number and the label when the cell can
afford it (`2 Copiar`, not `2Copiar`) — the mouse zones come from
`keybar::layout` and do not move.

## Considered and rejected

- **A client-side splash in `index.html`.** Earlier on screen, but it cannot
  name where you were, and it would be a second start screen to keep in step.
- **Auto-open reusing the toggle.** One line shorter and wrong twice: it closes
  on the second task and reopens what the reader closed.
- **A `rate` field on the wire.** The daemon would have to timestamp and smooth
  for every client; the two frontends would still disagree about the window.
  Computing it where it is painted keeps `TaskProgress` about the operation.
- **Dropping the `/` outright.** It is the orthodox convention and it still
  earns its cell where there are no icons.

## Consequences

- One bridge bump for the window: the splash view, per-task rate and ETA (as
  strings the host already formatted) and a per-row progress percentage.
- The TUI paints rate and ETA in the processes panel rows, and the row of a
  listing shows the progress of the task working on it.
- `SPLASH_BRIEF_MS` is a deadline, not a key resolution: ADR 0006 forbids the
  latter, and no key here waits for the clock — any key removes the cover
  first.
