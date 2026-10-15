# Orders in the box, an AI that writes them, and a live activity strip — design

- Date: 2026-10-14
- Status: approved in conversation, pending spec review
- Frontends: both. Grammar in a new pure crate; resolution in the core;
  the box model in `norte-frontend` (the omnibox, spec 2026-10-09); the TUI
  and the window paint it.

## Goal

norte should feel like a tool for people who live in a terminal: typed
orders first, the mouse optional. Today the omnibox finds commands (`>`),
places (empty) and help (`?`), but it cannot *compose*: there is no way to
say "these files, that action, there" in one line, and no way to ask for it
in plain language.

Success, as a reader would see it in the first minute:

1. `:sel ext:pdf age<30d | cp >other` in the box lights up the 14 rows it
   would copy while it is being typed, says `14 files · 38 MB · 0
   conflicts`, and Enter copies them, journalled.
2. In the AI panel, "copy this month's PDFs to the other pane" produces that
   same line, editable, with the same preview and an **Apply** button. "Only
   the ones over 1 MB" refines it. "What takes the most space here?" gets an
   answer.
3. `undo` reverts it; `log` shows it in a `git log --graph`-style list.
4. While the copy runs, the status bar carries a braille sparkline of its
   throughput.
5. `ntc run ':sel ext:pdf age<30d | cp >other'` does the same from a script.

## Out of scope (later specs, in this order)

`ntc --demo` (the guided sandbox, next spec) · macros (`q a` / `@a`) ·
rename-in-`$EDITOR` (vidir style) · read-only git column and `git:` filter ·
`ntc select` from stdin and `ntc watch --json` · `at 10:42` (a directory as
it was) · disk IOPS in the activity strip (platform-specific
`/proc/diskstats`; throughput of norte's own tasks only here).

## 1. The box: two new prefixes on the omnibox

The omnibox (`navigation/goto.rs`) already dispatches on a prefix:
`>` commands, `?` help, empty = go anywhere. This adds two:

| prefix | mode | what Enter does |
| --- | --- | --- |
| `:` | **order** — the grammar below | `orders.apply` on the previewed plan |
| `@` | **ask** — hands the text to the AI panel | opens the AI panel with it as the first turn |

`?` stays help: it is already taken, and help is the older promise. `@` reads
as "address the assistant".

In order mode the line is parsed **locally** on every keystroke
(`norte-orders`, no socket): tokens are coloured, the verb and query keys
autocomplete, a parse error is shown under the line at its byte offset. The
*preview* (which rows, how many bytes, which conflicts) is an `orders.plan`
call, debounced (~120 ms) and cancelled by the next keystroke; the rows it
names are lit in the pane.

Keys: the palette chord already in each preset keeps opening the box with
`>`. `:` opens it with `:` typed **where `:` is free in browse** — certainly
`vim`; every preset where it is not gets the reason in its header's
omissions block (CLAUDE.md, "a key change is not done until every preset is
done"). The command `app.orders` (opens the box with `:`) is bound in all
seven presets on some chord, so nobody lacks the mode.

## 2. `norte-orders`: the grammar (new pure crate)

Parser, AST, printer and the query predicate. **No I/O, no tokio.** Depends
on `norte-proto` (for `VPath` on the wire side) and nothing else of ours, so
the core (to validate AI output), `norte-frontend` (to colour and
autocomplete) and `norte-cli` (`ntc run`) all link it. Boundary test like
`norte-client`'s.

```
order    := stage ( '|' stage )*
stage    := verb arg*
verb     := core-verb | catalogue-name        (catalogue names take no args)
core-verb:= sel | cp | mv | trash | mkdir | cd | log | undo | redo
query    := term ( ' ' term )*                 (implicit AND)
term     := '!'? ( ext:LIST | name:GLOB | size(OP)N[KMG] | age(OP)N[mhdw]
                 | type:(file|dir|link) | GLOB )
dest     := '>' ( other | here | PATH )
```

- A stage reads the selection the previous one produced: `sel` *sets* it,
  `cp`/`mv`/`trash` *consume* it. A first stage that is not `sel` consumes
  the pane's current marks (or the cursor row, as a key would).
- `undo [N]`, `redo [N]`, `log [N]` stand alone; piping into them is a parse
  error, not a silent no-op.
- `pane.swap` etc.: any catalogue command by its stable name, no arguments,
  dispatched exactly as its key would (ADR 0077). Unknown names are a parse
  error with the nearest catalogue match suggested.
- **Paths are bytes.** `PATH` and `GLOB` are parsed from the line into
  `VPath`/byte globs; a byte that is not valid UTF-8 is written `\xHH`
  (and a literal backslash `\\`, a space or `|` inside a name quoted or
  escaped), and the printer emits exactly that back. No such reversible
  escape exists in the tree today — the display paths are lossy — so this
  crate owns it. `parse(print(ast)) == ast` is a proptest over the
  testkit's hostile-name strategy, and the canonical corpus gets the
  escaped forms as fixtures.
- `age` is measured against the mtime; `size` against the entry's size;
  directories match `size` only with an explicit `type:dir`.

The AST is the contract. The AI emits *text*, which goes through the same
parser; anything that does not parse is never shown as an applicable order.

## 3. The wire: `orders.plan` and `orders.apply`

> **Superseded by ADR 0185 (2026-10-14).** Orders are resolved against the
> pane the frontend already has loaded and run through the doors the keys
> use; there is no new wire. The CLI is `norte run`, not `ntc run`. `redo`
> is out until the journal has one. The text below is the option that was
> not taken.

Two methods, a protocol minor bump, `protocol-guardian` mandatory.

- `orders.plan { text, context }` → `OrdersPlan { stages, effects, totals,
  conflicts, plan_hash }`. `context` carries the focused and other pane
  locations and the current marks; the core never reads frontend state. A
  **read**: it lists and stats, nothing else. Long resolutions are a task
  with a `CancellationToken` (a `sel` over a large tree), cancelled when the
  next keystroke supersedes it.
- `orders.apply { plan_hash }` → task id. Re-resolves, refuses with a
  typed error if the result no longer matches `plan_hash` (the tree changed
  under the preview), then runs the stages through the operations that
  already exist (`fs.copy`, `fs.move`, `fs.trash`, `fs.create`, journal
  undo/redo). No new mutation path: journal entries and undo come from
  those operations. A plan is bound to its `conn_id`, as in ADR 0049/0050.
- `log` resolves from the journal timeline (ADR 0121) and needs no new
  storage.

`ntc run '<order>'` is a thin client: `orders.plan`, print the plan, then
`orders.apply` (or `--dry-run` stops after printing; `--json` per ADR 0088).

## 4. The AI panel

A new panel kind (like the terminal panel), in both frontends, opened by
`@…` in the box, by a command `ai.chat` bound in all seven presets, and from
the menu.

Each assistant turn is exactly one of:

- **an order**: text in the grammar, validated by `norte-orders` in the
  core, shown with its `orders.plan` preview and an **Apply** button; or
- **an answer**: prose, built from read-only tool calls.

**The human always applies.** Apply calls `orders.apply` as the user; the
model has no apply tool. ADR 0050 holds unchanged.

New wire: `ai.chat { session, message, context }` streaming turns as
notifications, `ai.chat_cancel`. The provider is the existing `AiProvider`
(Ollama, Anthropic, OpenAI-compatible), asked for structured output by JSON
schema so a turn is `{kind: "order", text}` or `{kind: "answer", text}`.

**What the model may read** — tools served by the core, all through the
policy engine under a new `Actor::Assistant`:

| tool | gives | never |
| --- | --- | --- |
| `list` | names, kind, size, mtime of a directory | file contents |
| `stat` | one entry's metadata | |
| `sizes` | the disk-map totals of a subtree | |
| `index_query` | `index.query` hits | semantic snippets of contents |
| `log` | the last N journal operations | |

Every path a tool touches must be under the root of an open pane; anything
else is refused by the policy, not by the prompt. With a remote provider,
only names and sizes leave the machine — the panel says so in its header.
Tool output is data to the model, never instructions: names are hostile
input (the corpus has names that read like prompts, and the tests use them).

**First run, nothing configured:** if Ollama answers on localhost, use it
with the configured or smallest installed model. Otherwise the panel shows
two lines: how to point at Ollama, or how to store an API key (keyring,
never config — hard rule 10). The `:` box works without any AI at all.

## 5. The activity strip

The right end of the status bar, shown **only while tasks run**: a braille
sparkline of aggregate throughput over the last ~30 s, the rate, and the
count of active tasks (`⣀⣤⣶⣿ 84 MB/s · 2`). Click or `app.processes`
opens the processes panel, whose rows gain a per-task sparkline.

Data: the progress notifications tasks already publish; the frontend keeps a
fixed ring per task and samples on its existing tick (no new timer, no wire
change). The window draws the same series as SVG. Glyphs degrade to ASCII
where the terminal's charset lacks braille (the existing capability probe).

## 6. `log`, `undo N`, `redo`

Core verbs of the grammar over the journal: `log [N]` renders the last N
operations in the box as a graph-like list (`● copy 14 → /mnt/b  10:42`),
Enter on a row opens it in the timeline panel; `undo N` undoes the last N in
order, stopping at the first that refuses, and says which.

## Execution: one spec, three plans

| plan | contents | branch |
| --- | --- | --- |
| (a) | `norte-orders`, `orders.plan`/`apply`, the `:` mode, `log`/`undo`/`redo`, `ntc run` | `feat/orders-box` |
| (b) | the AI panel, `ai.chat`, the assistant actor and its tools | `feat/ai-panel` |
| (c) | the activity strip and per-task sparklines | `feat/activity-strip` |

(b) depends on (a); (c) is independent and can go in parallel on a
disjoint surface.

## Testing

- `norte-orders`: unit tests per production, parse-error offsets, and the
  round-trip proptest over hostile names; canonical-corpus fixtures for the
  escaped forms.
- Daemon integration: `orders.plan` against `MemProvider` trees,
  `plan_hash` mismatch after a change, clean cancellation of a long `sel`,
  `orders.apply` producing undoable journal entries.
- AI: the `fake` provider scripts turns — an order, an answer, an invalid
  order (never offered as applicable), a tool call outside the pane roots
  (refused by policy), a prompt-injecting filename.
- `ntc run`: CLI golden for plan output and `--json`.
- Parity: the same order from the TUI and from the window produces the same
  plan (ADR 0077), and the keymap guard sees `app.orders` and `ai.chat` in
  all seven presets.
- Proto: goldens, schema, the six-place checklist for any new enum.

## Reviewers

`protocol-guardian` (the wire), `security-reviewer` (the assistant actor,
its tools, the policy and the provider key), `encoding-auditor` (the
grammar's paths and globs), `rust-reviewer` (all of it).

## ADRs to write

- The order grammar is a pure crate and the AI speaks it, not shell (why
  not shell commands, why not tool-call-per-operation).
- The assistant reads metadata through its own actor and never applies.
- `@` asks; `?` stays help.
