# Debt wave W2 — frontends

**Tier T1.** Local bugs in TUI, GUI and CLI. Clear repro, one crate each,
test-first.

**Rules:** the same six as
[W1](2026-08-13-debt-w1-mechanical.md) — issue as spec, one agent per crate
cluster, no `ci`/`ci-fast` inside an agent, one commit per issue, test-first,
mid-tier model. **Reviewers: one `rust-reviewer` over the whole branch diff at
the close, not one per task.** No security reviewer: nothing here mutates.

**Branch:** `debt/w2-frontends`

| issue | crate(s) | what |
| --- | --- | --- |
| #183 | norte-tui, norte-gui | a comparison that fails before publishing its snapshot is painted `Done` by both frontends |
| #173 | norte-tui, norte-core | an applying sync is invisible to the task board, because `TaskRef` is not `Clone` |
| #157 | norte-tui | the diff pane shows the size of the pairs and not of the orphans |
| #184 | norte-gui | the help overlay dispatches command ids the GUI's own `COMMANDS` table never declared |
| #147 | norte-gui | the unbind and the paste fixes stopped at the TUI |
| #180 | norte-cli | `Ctrl+C` during `norte sync`'s planning phase orphans a `.part` spool file forever |
| #187 | norte-cli | a cancelled `norte sync` prints "destination clean" and never reaches its report |
| #144 | norte-tui, norte-gui | an opener runs without the pane's directory as cwd |
| #159 | norte-tui | under tmux no MODIFIED function key arrives: `Shift+F2`, `Shift+F6`, `Alt+F7` dead while `F5` works |
| #191 | norte-gui | `on_apply_started` reassigns `task_id`, orphaning the plan task's cancel handle |
| #194 | norte-gui | step element ids come from a daemon-supplied `SyncStep::id` with no uniqueness check |
| #197 | norte-gui | the anchor qualifier and the hostile prefix are appended in band in the aural surface |

**#159 is an investigation, not a fix.** Cause unidentified; suspects are on the
issue. Timebox it and reproduce with the tmux harness
(`[Harness tmux para la TUI]`) rather than reasoning from the parser. If it
turns out to be upstream (tmux `extended-keys`, crossterm), relabel and close —
do not sink the wave into it.

**#183 and #147 both span two frontends.** Fix the shared decision in
`norte-frontend` and let both call it. The C1/C2 reviews caught exactly this
twice: a fix applied to the GUI copy while the TUI kept the defect.

**#180 and #187 are the same story** — what a `Ctrl+C` during `norte sync`
leaves behind — and want one design: the spool file and the report are two
halves of the same abandoned run. Filed an hour apart by two different passes
over the same code, which is why they read as separate issues.

**#188 is NOT in this wave and is not debt.** It is the remainder of #161 —
`Mirror` and the diff-pane sync gesture — and it needs its own
`security-reviewer` pass, because wiring `Mirror` is what makes #186 reachable
from the GUI. Roadmap, not debt wave.

**Close:** `just gui-ci` and `just ci-fast`, then `just ci` once.
