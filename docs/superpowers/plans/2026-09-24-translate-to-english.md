# Plan: translate the source from Spanish to English

Branch `chore/translate-to-english`. Pilot done: `crates/norte-theme`
(commit `6041bd67`) — read its diff (`git show 6041bd67 -- crates/norte-theme`)
as the reference for tone and depth. Glossary: `TRANSLATION_GLOSSARY.md`.
Wire surface (validated, everything "do not touch"): `TRANSLATION_WIRE_SURFACE.md`.

**Behavior must not change.** This is a language change, not a cleanup.

## Phases

1. **Translate in place** — tasks T01–T25 below, run in parallel by agents on
   DISJOINT file sets. Nothing that crosses a file boundary is renamed here.
2. **Cross-file renames** (controller, serial): `pub`/`pub(crate)` items,
   module and file names, literals asserted in another file. Fed by the
   agents' reports.
3. **UI strings left outside Fluent** (controller): extracted to both `.ftl`.
4. Gate: `just ci-fast`, doctests, `cargo doc`. Then PC4 style pass on the
   `en` catalogue, public rustdoc, README and CHANGELOG.

## Rules for a phase-1 agent

**You own exactly the files of your task. Touch nothing else.** Other agents
are editing the rest of the tree at the same time.

Translate:

- Every comment and rustdoc (`//`, `///`, `//!`, `/* */`), TOML/shell/TS
  comments. Translate faithfully: no rewording, no shortening, no "improving".
  A comment that is already wrong or stale is translated as is and followed by
  `// TODO(translation): review — <why>`.
- Log messages (`tracing::*!`), `panic!`/`expect`/`assert*!` messages,
  internal error texts (`#[error("…")]`, `anyhow!`, `bail!`), `Debug` labels:
  into English, inline.
- Identifiers that are **visible only inside your file**: locals, parameters,
  closure variables, private (no `pub` of any kind) fns/structs/enums/consts,
  test functions, helpers in a `#[cfg(test)] mod`. Before renaming anything that
  is not a local, `rg -w <name> crates plugins` — if it appears in any other
  file, do NOT rename it; report it.
- Use the glossary. A concept missing from it: pick the orthodox-file-manager
  term and add it to your report under *glossary*.

Do NOT touch:

- Anything in `TRANSLATION_WIRE_SURFACE.md` (serde names, `rename`, method
  names, config keys, env vars, CLI flags, Fluent ids, file names on disk).
- `*.ftl`, `crates/norte-help/topics/**`, golden and snapshot files
  (`tests/golden*/**`, `*.snap`, `*.json` fixtures), `docs/**`.
- String literals that are test DATA (file names inside a temp dir are fine to
  translate; a string whose non-ASCII bytes are the point — accents, hostile
  names, encodings, `norte-testkit` corpus — is kept).
- A literal that also appears in a golden/snapshot file or in a file outside
  your task (check with `rg -F`): keep it, report it.
- User-visible strings that do not go through `t!`/Fluent: keep them, report
  them (phase 3 extracts them; the `.ftl` files are shared).
- `pub`, `pub(crate)`, `pub(super)` names, `mod` names, file names: report.

How:

- Edit and Write tools only. `sed -i`, redirections and scripts that write
  files are blocked by a hook.
- Prefer `Edit` on comment blocks. When most of a file's lines change, one
  `Write` of the whole file is cheaper — then re-read nothing, trust it.
- After every whole-file `Write`, check the file's last lines: one agent's
  Write left a stray `</content>` line after the closing brace.
- **Do not dispatch sub-agents.** The session has a 20-agent concurrency limit
  and the controller is already running nine; do the files yourself.
- `rg -w <old_name>` also finds the name in comments — a renamed private fn
  cited in another file's comment is a cross-file reference: report it.
- **No `git` commands that change state — above all never `git stash`.** One
  agent stashed "to check whether a test failed before my change" and reverted
  196 files of eight other agents mid-edit. To compare with the original, read
  it: `git show HEAD:<path>`. No add/commit/checkout/restore/reset either.
  No `cargo fmt` (other agents edit the same crates; the controller formats).
- At the end, once: `cargo check -p <crate> --all-targets` for each crate you
  touched. Fix only what YOUR edits broke. If the build is broken by a file
  that is not yours, say so and stop.
- **Nothing will notify you and nobody is monitoring you.** Never `sleep`,
  never `tail -f`, never wait. When your files are done and checked, report.

Report: write `<scratchpad>/tr/reports/<task>.md` (the controller gives the
path) with these sections, each a list with `file:line`:

1. **pub renames** — `old → proposed` (items you did not rename because they
   cross the file).
2. **cross-file literals** — Spanish literals you kept because another file
   or a golden asserts them.
3. **UI strings outside Fluent** — user-visible Spanish you left.
4. **Spanish file/module names** in your task.
5. **glossary** — new terms.
6. **TODO(translation)** — how many you added.
7. **cargo check** — the result line.

Your final message is one paragraph: counts, anything that went wrong.

## Status (2026-09-24, interrupted by the monthly spend limit)

- **Done:** T02, T03, T04, T05, T06, T07 (without its two big tests → T07b),
  T08, T10. Reports in the session scratchpad `tr/reports/`.
- **Interrupted mid-file** — resume by re-dispatching the SAME task; the agent
  scans its files and translates what is still Spanish: T01, T07b
  (`proto/tests/{types,golden_types}.rs`), T09, T11, T12, T13, T14, T15, T16, T17.
- **Not started:** T18–T25.
- Build broken only by half-done edits in `core/tests/columns_git_e2e.rs`,
  `core/tests/columns_size_bar_e2e.rs` (call `run_column_values_for_test`,
  owned by T01's `plugins.rs`) and `tui/src/gestures.rs` (T13).
- `stash@{0}` holds a snapshot from the stash incident; `ops.rs` in it has
  T01's earlier progress. Drop it once T01 is done.
- For phase 2: T10 found a probable pre-existing bug in
  `ui-host/src/controller/approvals.rs::abrir_aprobacion` (a repaint patch for
  the agents panel is built and never returned) — file an issue, do not fix
  in this branch.

## Tasks

Paths are prefixes; a task owns every text file under them. `core` =
`crates/norte-core`, and so on.

| task | owns | ~lines |
| --- | --- | --- |
| T01 | `core/src/{engine,ops,plugins}.rs` | 3800 |
| T02 | `core/src/{journal,embedded,undo,search,connect,pack,policy,backend,ai,hooks}.rs` | 3700 |
| T03 | every other file directly in `core/src/`; `core/src/{backend,policy,rename,ui_session,volumes,sync}/`; `core/{benches,Cargo.toml,clippy.toml}` | 3800 |
| T04 | `core/src/daemon/`, `core/tests/daemon/`, `core/tests/origen_a_peticion/` | 3100 |
| T05 | files directly in `core/tests/` | 2700 |
| T06 | `proto/src/methods.rs` | 4400 |
| T07 | the rest of `crates/norte-proto` (not `tests/golden/`); `crates/norte-sync` | 3900 |
| T08 | files directly in `ui-host/src/` | 4600 |
| T09 | `ui-host/src/controller/{mod,tasks,selectors,sync,logpanel,extensions,dialogs,ai,transfer,listing,help,fileops}.rs` | 4500 |
| T10 | the rest of `ui-host/src/controller/` | 3300 |
| T11 | `ui-host/tests/`, `ui-host/Cargo.toml` | 4600 |
| T12 | `tui/src/{app,mouse,event_loop,gestures,dispatch,logview,keys,subshell,session_push}.rs` | 4000 |
| T13 | every other file directly in `tui/src/` | 4300 |
| T14 | `tui/src/app/`, `tui/src/jobs/` | 3900 |
| T15 | `tui/src/ui/`, `tui/src/screens/` | 3800 |
| T16 | `tui/tests/`, `tui/src/lua/`, `tui/{benches,Cargo.toml}` | 3500 |
| T17 | files directly in `frontend/src/` | 4100 |
| T18 | `frontend/src/{chrome,keymap,layout,navigation,ops}/` | 3900 |
| T19 | `frontend/src/{overlays,pane,sync,view}/`, `frontend/presets/`, `frontend/{tests,build.rs,Cargo.toml}` | 3900 |
| T20 | `crates/norte-gui-tauri` (Rust and `ui/src` TypeScript/CSS; not `ui/dist`, not `node_modules`) | 4000 |
| T21 | `crates/norte-plugin-host` (incl. the `.wit` comments), `crates/norte-cli` | 4200 |
| T22 | `crates/norte-vfs-local`, `crates/norte-vfs-archive`, `crates/norte-vfs` | 3900 |
| T23 | `crates/norte-config`, `crates/norte-client`, `crates/norte-compare` | 3800 |
| T24 | `crates/norte-{connect,testkit,mcp,term,vfs-object,vfs-rar,ai,encoding}` | 3900 |
| T25 | `crates/norte-{vfs-sftp,index,help,i18n}` (not `topics/`, not `*.ftl`), `plugins/`, `scripts/`, `justfile`, `Makefile` | 1300 |
