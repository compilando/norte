# Comparison and Synchronisation: the Surfaces (spec 3)

**Roadmap item 1, spec 3.** Specs 1 and 2 built the engine; this one gives it
the three surfaces it does not have. Issues #162 (CLI, MCP) and #161/#158
(GUI).

**Status:** design accepted 2026-08-13. Decisions taken up front, in the
"Decisions" section: the agent can **plan and not apply**, and the scope
includes the GUI's compare pane (#158), which spec 3 originally left aside.

---

## 1. Where this starts

`fs.compare`, `sync.plan`, `sync.apply` and `sync.report` exist, are versioned
(proto 0.40.0), journalled, undoable and gated. Exactly one client can reach
them: the TUI.

That is not just an inconvenience. The roadmap's argument for the whole item
was "a real answer to *did the copy work*", and the answer is only real if a
script can ask. It also leaves the plan-as-a-wire-type — the design decision
specs 1 and 2 kept paying for — without the two consumers it was paid for.

**What is already UI-independent, and therefore not built again here:**

| crate | what it holds |
| --- | --- |
| `norte-compare` | the comparison engine, streaming over two providers |
| `norte-sync` | the transducer: rows + capabilities + options → steps |
| `norte-core::sync` | the task, spool, executor, journal batch |
| `norte-frontend::compare` (1221 lines) | `ComparePane`, `cells_for`, `CATEGORIES`, glyphs, all labels |
| `norte-frontend::sync` (2414 lines) | `SyncState`, `SyncPlan`, `StepUndo`, `UndoOutlook`, `PlanIntegrity`, `render_step`, `summary_lines`, `confirmation` |
| `i18n/` | ~45 Fluent ids for sync, in both locales |

So each surface is presentation plus one lifecycle. **No new wire method, no
new proto version, no schema change.** If a phase below finds itself editing
`norte-proto`, that is the signal to stop and re-open this document.

---

## 2. Decisions

### 2.1 The agent plans; it does not apply

An MCP agent gets `compare` and `sync_plan`. It does not get `sync_apply`.

Three facts make this the cheap and the correct answer at once:

- `EMBEDDED_CONN_ID` runs as `Actor::User` and therefore passes **no policy
  gate**. Its own rustdoc says these methods must not be wired to the Lua
  sandbox or the plugin host without an actor of their own. An apply tool
  reached through the embedded backend inherits exactly that.
- A `plan_hash` is **not a secret**: an unkeyed deterministic digest anybody who
  can read both trees can compute. The only thing binding a plan to its
  requester is `conn_id`.
- Applying is the one operation in this subsystem whose blast radius is a whole
  subtree, and #166's warning notwithstanding, the daemon's gate for it has
  never been exercised by a non-`User` actor.

Giving the agent apply means giving the embedded bridge an actor of its own,
routing `Ask` approvals for a method that has no approval path today, and
deciding what a `Mirror` even means under a scope. That is a spec, not a tool
definition. It is **not** in this one.

### 2.2 The agent's plan_hash is not redeemable by the human

This falls out of the retention model and has to be said, because the obvious
mental model is wrong.

A plan is retained **per connection**, in an in-memory issuance registry, for
`SYNC_PLAN_TTL_MS`. The agent's `sync.plan` runs on the agent's connection. A
human who then applies from the TUI is a different connection, so **the agent's
hash means nothing to them** — `sync.apply` will refuse it.

Therefore the agent's output is a **report, not a token**. It says what would
change and why; the human re-plans in their own client (getting their own hash)
and applies there. The tool description must say this in so many words, or every
agent that reads it will try to hand the hash over and get a confusing refusal.

This is a feature. It means the agent cannot arrange for a subtree rewrite even
by social engineering: the only apply that can happen is one a human's own
client planned.

### 2.3 The CLI plans and applies on ONE connection

Same fact, other consequence. `norte sync` cannot be two invocations. One
process: plan, print, ask, apply — with the connection held open across the
question. `--dry-run` is the plan without the apply, and it is the flag the
"did the copy work" use case actually wants.

### 2.4 The GUI is in scope, and that pulls #158 in

#161 (no sync surface) depends on #158 (no compare pane) — there is nothing to
put a sync on. Both are in, as phase C.

One piece of #161 is independent of all of it and ships first, in phase A,
because it is currently a lie rather than a gap: `pane.sync-dirs` is `Live` in
the keymap catalogue, so **every** frontend's reference sheet claims the command
exists, and the GUI's shows it as available. It must show `NotHere` until phase
C lands. Same commit fixes `norte-gui`'s hardcoded `journalled: true` → the
`Backend` fact.

---

## 3. Phase A — the command line

**Ships on its own.** After this phase a script can answer "did the copy work",
which is the roadmap's stated payoff, with nothing else built.

### 3.1 `norte compare <a> <b>`

Read-only, no plan, no journal. Runs `fs.compare` as a task and prints the rows.

```
norte compare <a> <b> [--json] [--criteria size,mtime,hash] [--max-depth N]
                      [--mtime-tolerance-ms N] [--only differing,left,right]
```

Human output is one row per finding, using `norte_frontend::compare::cells_for`
so the column vocabulary is the one the TUI already shows — not a second
rendering that drifts. `--json` emits the `CompareRow`s as they come.

**Exit code carries the answer**, `diff`-style, because that is what makes it
usable from a script without parsing:

| code | meaning |
| --- | --- |
| 0 | the trees agree under the criteria asked for |
| 1 | they differ |
| 2 | the comparison could not complete (I/O, cancelled, incomplete) |

The distinction between 1 and 2 is load-bearing: an incomplete comparison that
reported 0 would be the exact failure this command exists to prevent.

### 3.2 `norte sync <source> <dest>`

```
norte sync <source> <dest> --mode update|mirror [--dry-run] [--yes] [--json]
                           [--criteria …] [--mtime-tolerance-ms N]
                           [--on-unknown skip|copy]
```

The shape is `norte ai rename`'s, which is already the plan-then-confirm command
in this CLI and should be read before writing this one
(`crates/norte-cli/src/main.rs`, `ai_cmd`). Specifically inherited:

- the plan is **printed in full** before the question, rendered through
  `norte_frontend::sync::render_step` and `summary_lines`;
- **hostile names are masked and the masking is marked** (`norte_frontend::display_name`,
  prefix `!`). A destination path can carry RLO and spoof the confirmation
  prompt; the names here come off a filesystem the user may not control;
- the journal is resolved **before the question**, not at the first mutation, and
  "this will not be undoable" is part of what is being asked — including under
  `--yes`, where nobody is watching the screen and the log is all there is;
- `--yes` skips the question, never the printing.

`--dry-run` prints the plan and exits. Exit codes: 0 nothing to do, 1 there are
steps (dry-run) or all steps applied (real run), 2 the plan or the apply failed.
Failures come from `sync.report`'s `failures`, printed one per line.

`--mode mirror` deletes. The confirmation line must say how many deletions and
whether they go to a trash, which `norte_frontend::sync::confirmation` already
computes from `dest_trash`.

### 3.3 The catalogue fix (#161, the independent half)

`pane.sync-dirs` → `NotHere` in the GUI's reference sheet; `norte-gui`'s
`journalled: true` → `Backend::is_journalled()`. Two small edits, one commit,
no dependency on anything else in this spec.

### 3.4 What phase A must not do

No `norte sync --daemon` special-casing beyond what `make_backend` already
gives. No new config keys. No progress bar — the task's progress goes to the
same place every other CLI task's does.

---

## 4. Phase B — the agent

**Ships on its own**, needs phase A only for the ADR's wording about what a
human does with the result.

Two tools added to the eight in `tool_defs()` (`crates/norte-mcp/src/bridge.rs`),
dispatched in `call_tool` next to the existing arms:

**`compare`** — `{a, b, criteria?, max_depth?, mtime_tolerance_ms?}` → the rows,
capped like `list_dir` is capped, with the same `next_cursor` shape if the
result is long. Read-only; it is `fs.compare`, which already sits behind the
read gate.

**`sync_plan`** — `{source, dest, mode, criteria?, on_unknown?}` → the steps and
the counts, plus `dest_trash` and the plan's confidence vocabulary. **The
description states that the hash is not usable by anyone else and that applying
is a human action in their own client** (§2.2).

The ADR records §2.1 — why there is no `sync_apply` tool, what it would take to
add one, and the three facts that make "just wire it" wrong. Use `/adr`.

### 4.1 What phase B must not do

No actor change, no policy work, no approval routing. If the implementation
finds itself touching `EMBEDDED_CONN_ID` or `Actor`, it has left the phase.

---

## 5. Phase C — the graphical frontend

**Depends on nothing in A or B**, and is the longest. It closes #158 and the
rest of #161.

`norte-gui` is excluded from the workspace and has its own gate: **`just gui-ci`**,
not `just ci`.

### 5.1 The compare pane (#158)

The TUI's diff pane is a virtual pane: rows come from a task, `ComparePane` holds
the state, `cells_for` renders each row, `CATEGORIES` drives the filter, and
`verdict_glyph`/`confidence_glyph` carry the verdict without relying on colour
alone (spec §17). All of that is in `norte-frontend` and is reused verbatim.

What GPUI needs: a pane that renders those cells, the category filter, the
running/done/incomplete/cancelled/failed status line, and the two commands that
start and cancel the comparison. The status vocabulary is
`CompareState` in `crates/norte-tui/src/app.rs` — if it is not already
UI-independent, moving it to `norte-frontend` is part of this phase, not a
refactor to be skipped.

**Colour is not the signal.** A verdict must be legible in monochrome; the TUI
already does this and the GUI must not regress it into a colour-coded list.

### 5.2 The synchronisation surface (#161)

Over the compare pane: the plan rendered through `norte_frontend::sync`, the
approval dialog (`confirmation`, `summary_lines`, `UndoOutlook`,
`PlanIntegrity`), and the two mode keys. The Fluent ids exist in both locales.

`pane.sync-dirs` flips from `NotHere` back to whatever the fact says once this
lands — which, since the GUI is remote-only, is available.

### 5.3 What phase C must not do

No new comparison logic, no second rendering of a row, no GUI-only Fluent ids
for strings that already exist. If a label is missing, it is missing in
`norte-frontend` and gets added there, where the TUI gets it too.

---

## 6. Cross-cutting

**Filenames are bytes.** Every path in every surface goes through `VPath` or
`display_name`. The CLI's `--json` emits the wire encoding, not a lossy string.
An `unwrap` on `to_str` is grounds for rejecting the change (rule 1).

**No business logic in the frontends** (rule 7). If the CLI needs to decide
something the TUI also decides, it belongs in `norte-frontend`.

**i18n**: every user-facing string through Fluent, both locales, in the same
commit (`t!`/`norte_i18n::t`). The CLI's `--json` output stays locale-free —
`doctor --json` already sets that precedent and its module doc explains why.

**Testing.** Each phase ships integration tests at its boundary: the CLI's
against the real binary with a tempdir state directory (see
`crates/norte-cli/tests/smoke.rs`, which now isolates `NORTE_CONFIG_DIR` — do
not regress that), the MCP's against the bridge, the GUI's under `gui-ci`.

---

## 7. Non-goals

- **Two-way synchronisation.** §17's other half. Spec 2 deliberately did not
  attempt it and neither does this.
- **`sync_apply` as an MCP tool** (§2.1).
- **Transferring a retained plan between connections** (§2.2). Changing that is
  a wire and policy question, not a surface one.
- **Fixing #164, #178 or #179.** They are open, they touch this code, and they
  are not this.

---

## 8. Keeping it short

The stated ask for this spec was that the build be quicker than the last ones.
Where the time actually goes is written down in `CLAUDE.md` — generated tokens,
then the gate — so the levers are:

- **Three phases, three branches, three gate runs.** Not one twelve-task plan.
  Phase A is useful merged on its own, and merging it is what stops phase B from
  inheriting its review debt.
- **Cheap model by default.** Phases A and B are mechanical against precedents
  that exist in-tree (`ai_cmd` for the CLI, the eight existing tool arms for
  MCP). They do not need the largest model. Phase C's pane does.
- **One reviewer per task, not two**, except where the surface earns it:
  `security-reviewer` on phase B (agent surface), `encoding-auditor` where names
  are printed. Reviewers never compile.
- **The plan carries the tricky signatures and the tests, not the mechanical
  bodies** — that is what paid for itself on the last two plans and what cost
  nothing when it was skipped.
- **A phase that grows a fourth surface, a proto change or an actor is over.**
  Stop and re-open this document instead of absorbing it.

---

## 9. Issues

| issue | phase |
| --- | --- |
| #162 CLI half | A |
| #161 catalogue lie + `journalled` literal | A |
| #162 MCP half | B |
| #158 GUI compare pane | C |
| #161 GUI sync surface | C |
