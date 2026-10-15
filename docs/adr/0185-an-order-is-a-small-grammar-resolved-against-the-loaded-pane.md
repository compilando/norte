# 0185 - An order is a small grammar in a pure crate, resolved against the loaded pane

- Status: accepted
- Date: 2026-10-14
- Decision makers: Oscar González
- Related: ADR 0050 (the agent plans and does not apply), ADR 0077 (the same
  command means the same thing in both frontends), ADR 0126 (the catalogue
  declares what a command does), ADR 0181 (the command bar runs the shell),
  spec `docs/superpowers/specs/2026-10-14-command-box-ai-design.md`, plan
  `docs/superpowers/plans/2026-10-14-orders-box.md`.

## Context and problem statement

The omnibox finds commands (`>`), places and help (`?`), but it cannot
compose: "these files, that action, there" took a mark-by-pattern dialog, a
key and a destination prompt. The reader asked for norte to feel like a
tool for people who live in a terminal, and for an AI that writes what to do
from plain language. Both need the same thing first: a line that says
what to do, that a person can read, edit and run, and that a program can
validate.

Three questions had to be answered: what the line is, where it is
resolved, and through which door it mutates.

## Considered options

1. **Shell commands.** The command bar already runs them (ADR 0181).
   Familiar, but a shell line cannot be previewed (what would `cp *.pdf x`
   touch?), cannot be validated before it runs, does not go through the
   journal, and an AI that writes shell is an AI that can write anything.
2. **One wire method per operation, called by the AI as tools.** Precise,
   but nothing a person reads or edits: the plan is a JSON call list, and
   the box gains no grammar of its own.
3. **A small grammar, resolved in the core (`orders.plan` / `orders.apply`
   with a `plan_hash`).** One implementation for every client, but a new
   mutation handler next to `fs.copy` with its per-item gates and its task
   registration invariant, a protocol bump, and a socket round trip on
   every keystroke for the live preview.
4. **A small grammar, resolved against the pane the frontend already has
   loaded, executed through the doors the keys use.** No wire change; the
   preview is local and instant; every guard F5/F6/F7/F8 already has
   (same-directory refusal, collision dialog, space check, trash probe,
   journal, undo) comes for free.

## Decision outcome

Option 4.

- `norte-orders` is a pure crate — parser, printer, a reversible `\xHH`
  escape — that depends on no other crate of ours, so the core (to validate
  what an AI writes), both frontends and the CLI link it. Only bytes typed
  plainly can be syntax: a folder called `other` or a name starting with
  `!` is written escaped and stays a name. The converse is refused
  (`InvisibleChar`): a character drawn as nothing (Unicode `Cf`, and the
  invisible letters and marks the printer escapes) typed plainly so that a
  word would read as a verb, key, `!`, `>other` or `>here` without it; any
  such character in a negated `sel` term, where a value that matches
  nothing inverts into "everything"; and a destination or argument made of
  nothing else. In a name or a positive pattern, or escaped, or quoted, the
  character is a real file's and stays.
- `norte-frontend::orders` evaluates a `sel` against the loaded listing with
  `mark_glob`'s own fold and glob engine (`NameMatcher`), so the two never
  disagree on a name, and turns the order into steps. Both frontends run the
  same steps; the last stage goes through the door its key uses. Only the
  last stage may act, because each action opens its own confirmation.
- A `sel` that matched nothing runs nothing: an empty mark set falls back to
  the cursor row, and "nothing matched" must never become "act on whatever
  the cursor is on".
- A term whose answer is unknown (no size, no mtime, a directory's inode
  size without `type:dir`) is false negated or not. Because local listings
  are lazy (#52), an order stats the rows its `sel` needs before it selects;
  the live preview, which cannot stat on every key, says how many it could
  not judge.
- `log` opens the timeline panel; `undo N` cuts the reader's own page at
  the (N+1)-th operation and opens the timeline's confirmation with the
  ceiling frozen. `redo` is refused until the journal has one.
- `norte run` is the same grammar from a script, without a pane. A script
  confirms nothing, so it behaves like a pane with hidden files off — `sel`
  leaves dotfiles out unless `--hidden` — and only `undo` asks for `--yes`;
  `--dry-run` says what would run.
- A read-only window refuses an order whose acting stage writes, whatever
  door it would take: an order must not be the way around the filtered
  keymap, palette and menus.
- A pattern that names raw bytes (`\xff`) folds losslessly, so it never
  selects the file whose name only looks the same once both are painted as
  `�`. A pattern typed as text keeps the lossy fold, where `�` matches what
  the pane paints as `�`.

### Positive consequences

- Zero protocol change; the gate, the journal and the policy see the same
  calls a key makes.
- The AI (spec §4) gets a language a person can read before applying, and
  the core can validate it by parsing. ADR 0050 holds as it is.

### Negative consequences

- `sel` is not recursive: it sees the listing on screen, not the tree.
- There is no stale-tree hash: the preview and the run read the same
  listing, and a file that changed underneath is what F5 would also meet.
- Executing lives twice (TUI and window) over one shared step list; the
  parity test pins that both mark the same rows.
