# Terminal panel: several instances — design

- Date: 2026-10-09
- Status: approved in conversation, pending spec review
- Model: VS Code's integrated terminal (instances, tab list, profiles,
  icon/colour). Split groups are out of scope.

## Goal

The terminal panel (#362) holds ONE shell per frontend today
(`State.terminal: Option<Shell>` in `norte-ui-host`, `App.terminal` in
`norte-tui`). The reader wants several, managed the way VS Code manages them:
open another, switch, close one, name it, mark it, and choose which shell it
runs. In both frontends (parity, ADR 0077), with the rules written once.

Success: with the panel open, the reader can run a build in one shell and a
`vim` in another, switch between them by key or mouse in the TUI and in the
window, see which one has new output, and close either without touching the
other.

## Decisions taken

| question | answer |
| --- | --- |
| scope | instances + list, shell profiles, icon/colour. NO split. |
| shape | ONE `terminal` slot with N shells inside (approach A) |
| TUI list | tabs in the panel's top border; no column taken from the shell |
| window list | VS Code's right-hand list, shown only with ≥2 instances |
| shell exits with 0 | the instance is removed |
| shell exits ≠0 | it stays, last screen + "exited with code N", until closed |
| last instance closed/exited 0 | panel stays, saying "no shell", as today |
| title | manual name > program's OSC 0/2 title > profile name |
| persistence | none: a restored session opens one shell, as today |
| wire | `norte-proto` untouched; bridge version bumps |

### Why one slot and not one slot per instance

ADR 0170 shares the set of OPEN PANELS between the TUI and the window. One
slot per shell would make three shells opened in the window appear as three
shell-less terminal slots in the TUI. And `slot_of_kind(KIND)` — used by the
tick, the resize and the key routing — assumes one. VS Code's model is also
this one: the panel is one, the list is inside it.

### Why the OSC title and not the foreground process

VS Code titles a terminal `${process}`. Getting the foreground process name
needs `tcgetpgrp` plus `/proc/<pid>/comm` on Linux and something else on
macOS and Windows — three mechanisms, and a `std::fs` read outside
`norte-vfs-local`. OSC 0/2 is what every terminal emulator already shows,
fish emits it by default and bash on most distributions does via
`PROMPT_COMMAND`. When nothing emits it, the profile name is honest.

## Components

### 1. `norte-frontend::terminals` — the model (new, pure)

```rust
pub struct Terminals<S> { instances: Vec<Instance<S>>, active: Option<InstanceId>, next_id: u32 }

pub struct Instance<S> {
    pub id: InstanceId,          // monotonic, never reused in a session
    pub profile: String,         // profile name it was started from
    pub name: Option<String>,    // set by `terminal.rename`; wins over title
    pub title: Option<String>,   // last OSC 0/2, already sanitised
    pub icon: Option<IconName>,
    pub color: Option<AnsiColor>, // 1..=6, resolved by the theme
    pub exited: Option<i32>,     // Some = the shell is gone, with its code
    pub unseen: bool,            // output arrived while not active
    pub shell: Option<S>,        // None once exited
}
```

Generic over `S` so the rules are tested with no pty. Operations: `push`,
`close(id)`, `select(id)`, `next`, `prev`, `rename`, `decorate`,
`on_exit(id, code)`, `on_output(id)`, `display_title(id)`.

Rules the model owns, each with a unit test:

- `on_exit(id, 0)` removes the instance; `on_exit(id, n≠0)` keeps it with
  `exited = Some(n)` and drops the shell.
- Closing or removing the active instance activates its RIGHT neighbour,
  else its left one, else none (VS Code's order).
- `next`/`prev` wrap.
- `on_output` sets `unseen` only on a non-active instance; `select` clears it.
- `display_title`: `name` > `title` > `profile`.
- A `name` that is empty after trimming clears the name instead of setting an
  empty title.

### 2. Profiles — `terminal.toml` (new, in `norte-frontend`)

Named **shell profiles** in code and docs, never bare "profiles": norte
already has configuration profiles (the fourth config layer) and the two must
not be confused.

```toml
default = "fish"

[[shell]]
name = "fish"
program = "/usr/bin/fish"
args = []
icon = "terminal"
color = 4
```

- Read from the system, user and configuration-profile layers. **The project
  layer (`./.norte`) is excluded**, same rule and same reason as
  `openers.toml`: a hostile repository must not choose programs norte runs.
- `program` must be absolute (#302, ADR 0082); a relative one is a load error
  naming the file.
- Higher layer wins on equal `name`. Unknown keys are an error
  (`deny_unknown_fields`).
- Without the file there is one implicit profile, `login_shell()`, named
  after the program's file name.
- A `default` naming no profile is a load error, not a silent fallback.
- `icon` is one name from a fixed set (the same set `IconName` enumerates);
  `color` is 1..=6.

### 3. `norte-term` changes

- **Exit code**: `Shell::exit_code(&mut self) -> Option<i32>` from
  `child.try_wait()`. `dead()` stays. A shell killed by a signal reports a
  non-zero code (it should stay visible: something killed it).
- **OSC 0/2 title**: `Screen` stops discarding OSC 0 and OSC 2 and keeps the
  last one, exposed as `Screen::take_title() -> Option<String>`. Sanitised in
  `norte-term`: C0/C1 and DEL removed, lossy UTF-8 (it is display, rule 1),
  capped at 128 chars. The norte-cwd OSC 777 handling is untouched.
- Pumping all instances: no change needed in `norte-term`; the frontends call
  `pump()` on every shell each tick.

### 4. `norte-ui-host` (window)

- `State.terminal: Option<Shell>` → `State.terminals: Terminals<Shell>`.
  `terminal_epoch` stays one per panel, not per instance.
- Tick: pump EVERY instance (otherwise the 256 KiB tail cap corrupts an
  inactive shell's screen); resize every instance to the slot; collect exit
  codes and titles; republish only if the active one changed or the list
  changed (title, `unseen`, exit).
- `open_terminal_panel` keeps its door semantics. `start_si_missing` starts
  the default profile when the list is EMPTY (restored session, or the last
  one exited).
- Keys: `PASS_THROUGH` gains the `terminal.*` commands below.
- Bridge: `TerminalSlotView` gains
  `instances: Vec<TerminalInstanceView { id, title, icon, color, exited, unseen }>`
  and `active: Option<u32>`; `rows`/`cursor` are the ACTIVE instance's. An
  exited-≠0 active instance sends its last grid plus `exited`. `no_shell`
  means "list empty". New bridge actions for the mouse: select, close, new
  (with optional profile), rename, decorate. `BRIDGE_VERSION` bumps.

### 5. Window UI (`ui/src/render/terminal.ts`)

- Panel title bar actions: `+` (new, default profile), `▾` (profile menu),
  trash (close active).
- Right-hand list, ONLY with ≥2 instances: icon, colour, title, `●` when
  `unseen`, "exited N" when exited. Click selects; context menu: rename,
  colour, icon, close.
- Colour as `var(--term-N)`, same as the grid, so the theme decides.
- An exited instance shows its last screen dimmed with a line
  "exited with code N" (Fluent).

### 6. TUI

- `App.terminal` → `App.terminals: Terminals<TermPanel>`; same tick rules.
- Top border, only with ≥2: `┌ 1 fish │ 2 ● vim │ 3 bash ──┐`. The number is
  the 1-based position. Active one in the focused-row style. When they do not
  fit, the strip is cut around the active one with `…` at the cut side(s).
- Icon glyph only if the icon column is enabled (ADR 0140); colour as the
  ANSI index through the theme.
- Clicks on a tab select it (mouse geometry recorded at paint, like the other
  border buttons).
- Rename and decorate use the existing input/picker modals.

### 7. Commands and keys

New catalogue commands, all in both dispatchers:
`terminal.new`, `terminal.new-profile` (picker of shell profiles),
`terminal.close`, `terminal.next`, `terminal.prev`, `terminal.rename`,
`terminal.decorate`.

- Available only with the terminal panel open (`Availability::NotHere`
  otherwise); `terminal.new` on a remote pane says `host-not-local`, same as
  `layout.terminal`.
- `terminal.close` on a live shell asks nothing (VS Code does not either);
  the shell is the reader's, killing it is the reader's gesture.
- Every command goes in `PASS_THROUGH`, so its chord is taken from the shell.
  Hence the chord rule for this feature: **a lone chord a shell or a
  full-screen program does not use daily**. Not `ctrl+<letter>`, not
  `alt+<letter>` (readline/emacs meta), not `shift+<char>` (dead key).
  Candidates: `ctrl+alt+<key>` checked against desktop capture (memory: a
  free chord is not a deliverable chord), F-keys with modifiers, or
  `ctrl+pageup/pagedown` for next/prev (VS Code's own). Chosen per preset in
  the plan, checked against each imported manager's documentation; where a
  preset does not bind one, its header says why.
- Catalogue entry, `help-cmd-*` in both locales, help topic `terminal`,
  `norte-cli` golden.

## Not doing

- Split groups.
- Persisting instances across sessions or restarts.
- Moving a terminal to the editor area / its own window.
- Remote shells (`sftp://` keeps refusing).
- Per-instance cwd following (ADR 0084's subshell follows the panel; these
  shells do not, as today).
- Bell / activity notifications beyond `unseen`.

## Security notes

- Profiles run programs: project layer excluded, absolute `program` only.
  `security-reviewer` on the profile loader.
- The OSC title is FOREIGN text (a `cat` of a file can set it): sanitised in
  `norte-term`, painted with `textContent` in the window and as a plain span
  in the TUI. It is never obeyed, only shown.
- Nothing reaches the journal (ADR 0084): a shell is the reader acting. One
  `tracing::info!` per started instance, naming the profile, not the command.

## Testing

- `norte-frontend::terminals`: the rules listed in §1.
- `terminal.toml`: project layer ignored, relative `program` rejected,
  unknown `default` rejected, layer precedence, implicit profile.
- `norte-term`: OSC 0/2 captured, control bytes stripped, cap; exit code 0,
  non-zero, by signal.
- `norte-ui-host`: two instances — inactive one pumped and marked `unseen`;
  exit 0 removes, exit 3 stays; bridge golden for `TerminalSlotView`; one
  test per `PASS_THROUGH` command that the chord does NOT reach the shell.
- `norte-tui`: render snapshot of the tab strip (fits, cut around active,
  single instance shows no strip); click selects.
- Presets: the existing catalogue/preset guards; `norte-cli` golden.

## Deliverables beyond code

- ADR: "the terminal panel holds several shells" (one slot vs. one slot per
  instance; OSC title vs. foreground process; shell profiles' layers).
- CHANGELOG entry; help topic; memory.
