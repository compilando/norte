# 0126 — The catalogue declares what a command does

- Status: accepted
- Date: 2026-09-19
- Decision makers: Oscar González
- Related: ADR 0006 (named commands), ADR 0066 (`norte-ui-host`), ADR 0125
  (menu roles — its decision 3 is superseded here), the architecture review of
  2026-09-18

## Context and problem statement

A command has one name (ADR 0006) and one row in the shared catalogue
(`norte-frontend/src/keymap/catalogue.rs`). What the command *does to the
reader's world* had no row. Three tables answered pieces of that question on
their own:

| table | question it answered |
| --- | --- |
| `norte-ui-host::commands::MUTAN` | may a read-only window run it? |
| `norte-frontend::menu::role` | is it painted as destructive, or as AI? |
| `norte-frontend::availability::verdict` | is it vetoed on a read-only backend? |

The first two are the same fact seen twice, and neither is where a new command
is declared. Adding a command that writes meant remembering `MUTAN` by hand; a
forgotten entry is a key that a window which promised "only look" executes.
That failure is fail-OPEN and silent, which is the worst combination.

The architecture review asked for the Command pattern "one file per action".
That part is rejected below; the useful kernel of it is that **the facts about
a command should live with the command**.

## Options

1. **One type per command behind `Box<dyn Action>`**, each in its own file.
   - Good: textbook Command pattern; a new command is one new file.
   - Bad: loses the exhaustive `match` that the compiler checks in one place
     (the TUI's `dispatch.rs` header argues this in writing); every action
     still needs `&mut App` or the controller, so the "independence" is
     nominal; `dyn` over async methods is awkward in Rust; ~170 files of
     ceremony for arms that are one to three lines each.
2. **A field on `CommandDef`: the command's effect**, declared explicitly for
   every entry, with the per-frontend tables derived from it.
   - Good: one place to decide, next to the name; no default, so a new
     command cannot be classified by omission; the effect `match` stays
     exhaustive where it is.
   - Bad: 173 rows gain a column; the catalogue, which was pure vocabulary,
     now carries one semantic fact.
3. **Keep the tables, add a cross-check test.**
   - Good: smallest diff.
   - Bad: a test can say two lists disagree; it cannot say which one is right,
     and it still needs a third list to compare against.

## Decision

Option 2. `CommandDef` gains `effect: Effect`, and **every row names it** —
there is no constructor with a default.

```rust
pub enum Effect {
    Inert,        // only norte's own state: view, marks, layout, settings
    ReadsContent, // reads file contents wholesale and reports on them
    Launches,     // hands control to a program norte does not govern
    Writes,       // creates or changes the reader's files, journalled
    Destroys,     // deletes the reader's files
    SendsOut,     // sends data out of the process (an AI provider)
}
```

A command has **one** effect: the one the reader must be warned about first.
`pane.ai-rename` renames, but the listing has left the machine before
anything is renamed, so it is `SendsOut`.

What is derived from it:

- A read-only window (`Efectos::SoloLectura`) runs only `Inert` commands.
  `MUTAN` is gone.
- `menu::role` is `Destructive` for `Destroys` and `Ai` for `SendsOut`.
  This supersedes ADR 0125's decision 3, which kept the role as a
  presentation-only list; it was the second copy of the same fact.

What is **not** derived from it: `availability::verdict`. Which backend a
command writes to (source, destination, both) and in which order the vetoes
are reported depends on runtime facts and is decided per command there. The
effect says *that* a command writes; the verdict says *where*.

Dispatch is unchanged: the TUI's flat `match` and the host's `efecto_de` stay
the single exhaustive tables that turn a name into behaviour.

## Consequences

- Good: declaring a command forces the question "what does it do to the
  reader's files?" at the moment it is written, in the one file every command
  passes through.
- Good: a test pins the exact set of non-inert commands, so a reclassification
  is a visible diff, not a drift.
- Good: a read-only window can no longer be fail-open by omission. The
  listing's commands are filtered by effect. The viewer's and the dialogs'
  lists are served whole, so a test requires every entry in them to be
  `Inert`: a `viewer.edit` that launched an editor would fail it instead of
  slipping through.
- Bad: `Inert` covers writing norte's *own* files (`profile.save-as`, the
  settings). That is deliberate — the effect describes the reader's data, and
  a window that may save a profile is still one that only looks at files — but
  it has to be read in the enum's rustdoc to be understood.
- Bad: `pane.sync-dirs` deletes at the destination and is classed `Writes`,
  not `Destroys`: its deletions go to the journal as a plan the reader
  approved, and painting the whole command red would say the opposite of what
  the plan review says.
