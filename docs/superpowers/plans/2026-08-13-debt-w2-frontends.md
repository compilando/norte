# Debt wave W2 — frontends

**Tier T1.** Local bugs in TUI, GUI and CLI. Clear repro, one crate each,
test-first.

**Rules:** the same as [W1](2026-08-13-debt-w1-mechanical.md) — issue as spec,
one agent per crate cluster, no `ci`/`ci-fast` inside an agent, one commit per
issue, test-first, mid-tier model, private git index.

**Reviewers: each agent dispatches its own `rust-reviewer` on its own diff
before its last commit.** No controller pass at the close: this is a
frontend-only wave and nothing here mutates. (This wave ran one anyway, under
the older rule, and what it established was that the rule should change.)

**Branch:** `debt/w2-frontends`

| issue | crate(s) | what |
| --- | --- | --- |
| #183 | norte-tui, norte-gui | a comparison that fails before publishing its snapshot is painted `Done` by both frontends |
| #157 | norte-tui | the diff pane shows the size of the pairs and not of the orphans |
| #184 | norte-gui | the help overlay dispatches command ids the GUI's own `COMMANDS` table never declared |
| #147 | norte-gui | the unbind and the paste fixes stopped at the TUI |
| #180 | norte-cli | `Ctrl+C` during `norte sync`'s planning phase orphans a `.part` spool file forever |
| #187 | norte-cli | a cancelled `norte sync` prints "destination clean" and never reaches its report |
| #159 | norte-tui | under tmux no MODIFIED function key arrives: `Shift+F2`, `Shift+F6`, `Alt+F7` dead while `F5` works |
| #191 | norte-gui | `on_apply_started` reassigns `task_id`, orphaning the plan task's cancel handle |
| #194 | norte-gui | step element ids come from a daemon-supplied `SyncStep::id` with no uniqueness check |
| #197 | norte-gui | the anchor qualifier and the hostile prefix are appended in band in the aural surface |
| #144 | norte-tui, norte-gui | an opener runs without the pane's directory as cwd — **decided**, see below |

**#159 was an investigation and it stays OPEN — deliberately.** Reproduced
against the exact configuration on record (tmux 3.7b, `default-terminal
"alacritty"`, `extended-keys off`) with a fresh binary: `Shift+F2`, `Shift+F6`
and `Alt+F7` all dispatched correctly via `tmux send-keys`, each verified
against the DIFFERENT binding of its unmodified twin — `Shift+F6`'s "move to"
dialog targeted the file's own parent rather than the other pane's directory,
which proves `pane.rename` fired and not `pane.move`.

**Not reproducible is not confirmed upstream.** `send-keys` injects into tmux's
input queue; it cannot simulate a physical keypress travelling
alacritty → tmux, which is suspect #1 on the issue. Relabelling it as upstream
on this evidence would have closed a live bug with a green test that never
touched the failing path. The findings are a comment on the issue.

**#183 and #147 both span two frontends.** Fix the shared decision in
`norte-frontend` and let both call it. The C1/C2 reviews caught exactly this
twice: a fix applied to the GUI copy while the TUI kept the defect.

## Two issues left this wave after reading their bodies

Same correction W1 needed, for the same reason: titles do not carry cost.

**#173 is a scheduler change, and its own issue says so** — "it is not a
two-line fix". `TaskRef` is deliberately not `Clone`, because a cancellation
handle with two owners is a handle two places think they own. Closing it means
either making the task board a subscriber (a `TaskId` plus a channel) or adding
a cheap cloneable observer and handing the board THAT. Either way it touches the
scheduler, so it goes to **W4**, which already owns the rule-3 work — and it
matters there: a sync that is actively rewriting a subtree is invisible unless
the pane that launched it stays open.

**#144 was a deferred DECISION, and it now has an answer.** The openers path
passed `None` where the shell commands pass the pane's directory, left that way
on purpose because an opener that writes a relative path would start writing it
somewhere else.

**Decided 2026-08-14 (Oscar): pass the pane's directory.** The reasoning is that
an editor opened on a file in the pane should save and navigate where the reader
is looking — the behaviour anyone arriving from mc or Far expects — and the
relative-path risk is the smaller of the two surprises. So `run_suspended`'s
`cwd` gets the pane's directory on the openers path too, matching the three
shell commands.

Do it LAST in this wave: it touches `norte-tui` and `norte-gui` at once, which
is exactly the pair the two agents own while they run. One commit, and say in
its message that the behaviour change was decided rather than discovered.

**#180 and #187 are the same story** — what a `Ctrl+C` during `norte sync`
leaves behind — and want one design: the spool file and the report are two
halves of the same abandoned run. Filed an hour apart by two different passes
over the same code, which is why they read as separate issues.

**#188 is NOT in this wave and is not debt.** It is the remainder of #161 —
`Mirror` and the diff-pane sync gesture — and it needs its own
`security-reviewer` pass, because wiring `Mirror` is what makes #186 reachable
from the GUI. Roadmap, not debt wave.

## What the branch review found, and what was done with it

One BLOCKER, three MAJORs, nine MINORs. The BLOCKER is why this wave's
controller pass earned itself: #180 left `Ctrl+C` inert from the end of
planning onwards, including the `[y/N]` prompt of a destructive sync, because
tokio's SIGINT registration is process-wide and permanent while the abort only
kills the listener. No per-task review could have seen it — it lives in the gap
BETWEEN two tasks' work.

Applied: the BLOCKER, all three MAJORs, and MINOR-4.

Filed rather than fixed, with the reviewer's reasoning: #198 (the #157 probe
leaks across comparisons), #199 (#157's GUI half — the mirror image of the
warning this very plan gives), #200 (paste does not work anywhere in the GUI,
which is the honest residue of #147's security question), #201 (the #187 signal
test can pass without exercising the fix once the fixture outgrows a pipe
buffer), #202 (`on_plan_ended`'s guard is racy in principle and its rustdoc
overclaims).

Skipped with a reason: MINOR-5 is historical (#194 landed as two commits and
one of them rendered a raw id; the second commit says so). MINOR-6 asks for a
test pinning #197's fail-safe asymmetry — worth having, and it belongs with
#200's paste work, since both are about what reaches a text field.

**Close:** `just gui-ci` and `just ci-fast`, then `just ci` once.
