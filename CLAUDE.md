# Claude Code project instructions

norte is an orthodox file manager with a headless Rust core, a JSON-RPC daemon,
TUI/GUI/CLI clients, and MCP-based agent access. Read the
[project specification](docs/spec/norte-spec.md) for the full design and
[ARCHITECTURE.md](ARCHITECTURE.md) for a repository map.

## Commands

```sh
just t norte-vfs                        # Test one crate (nextest; never `cargo test`)
just c                                  # Clippy the gate's crates, warnings denied
cargo fmt --all
just ci-fast                            # Gate minus coverage (lint test docs)
just ci                                 # Run the complete local CI suite
just disk                               # Where the build cache went
just prune                              # Reclaim it without a full rebuild
```

### The gate budget

**The gate is billed per PLAN, not per task.** Measured on the batch-rename
session (10 tasks, 17 agents): the gate ran 50 times and consumed 7.3 of the
12 hours — 60% of the whole session. The implementing agents spent 77%, 87%
and 98% of their lifetime waiting on it. The fix is fewer runs, not faster
ones: `target/` takes an exclusive cargo lock, so a second compile in the same
tree does not overlap, it queues.

This budget works — the session after it went from 50 gate runs to 3. It is
**not** the largest cost, though; that is the token budget below. Keep both.

| when | run | budget |
| --- | --- | --- |
| the RED→GREEN loop | `just t <crate>` (+ `just c` if you touched lint surface) | unlimited |
| every ~3 tasks of a plan | `just ci-fast` | ONE run |
| closing the branch, before the merge | `just ci` | ONE run |

**Costs, measured under real load** — the numbers that used to be here (10s /
34s / 143s) were 3–7× optimistic, and budgeting against them is what produced
the 50 runs:

| step | observed |
| --- | --- |
| `just t <crate>` | ~78s |
| `just ci-fast` | ~4min |
| `just ci` | 4–10min |

**Never use the gate as a debugger.** A red gate tells you WHICH test failed;
re-running it to see whether your fix worked costs 4–10 minutes for a test that
takes three seconds. Reproduce the single failure (`just t <crate>` — filtering
with `-E 'test(...)'` buys nothing, compilation dominates and the whole crate
suite is seconds), fix it there, and spend the gate run once, afterwards.

**Batch the fix rounds.** Reviewer findings from three tasks are applied in ONE
pass with ONE gate run at the end, not one gate run per finding.

**Reviewer agents never compile.** They read the diff and reason. A reviewer
that runs `just ci` has doubled the cost of a review that was, at ~0 seconds of
gate time, the cheapest part of the session.

`cov` is the bulk of `just ci` and can only move if you touched proto/vfs/core
— the sole crates under the 85% gate — so it stays out of the loop entirely.

### The token budget

**Generated tokens are the wall clock.** Measured on the keymap session (K1 +
K2a, 11 dispatches, 9.7 hours): about **one million output tokens at ~48
tokens/second**, which alone accounts for roughly five and a half hours. The
gate was 78 minutes of it. Everything else — reading, editing, git — was two
minutes.

| where the session went | |
| --- | --- |
| token generation (controller + subagents) | ~1M tokens, ~5.5h equivalent |
| gate (`ci`, `ci-fast`, `t`, `c`, `gui-ci`) | 78 min |
| agents idling on `sleep` / `tail -f /dev/null` | 19 min |
| waiting for a human to answer a question | 4.9h, of which 4.6h was two questions |

So the lever is **write less**, and it is mostly the controller's to pull.

**Dispatch prompts point at the plan; they do not contain it.** Pasting a
task's full text into a subagent prompt costs 3–6k tokens of *controller
output* — 80 seconds of generation each, eleven times a session. The subagent
reading `docs/superpowers/plans/<plan>.md` costs it ~2k tokens of *input*,
which is instant. A dispatch prompt is the plan path, the task number, and the
one thing the subagent cannot derive from the file: **what the previous task
discovered.** That last part is the only reason the prompt exists; everything
else is duplication. (This contradicts `superpowers:subagent-driven-development`,
which says to inline the text. Its reasoning is file-reading overhead, and that
is not what costs here.)

**Use the cheapest model that can do the task.** Pass `model:` on the Agent
call. A pure file move, a mechanical pattern adaptation, a TOML transcribed
from a source document — none of these need the largest model, and using it
anyway is the single easiest thing to stop doing.

**Never let an agent idle.** `sleep` in the foreground is blocked; agents route
around it with `timeout N tail -f /dev/null`, which is the same waste wearing a
hat. Forbid both in dispatch prompts. If something must be waited on, it is a
background job the harness will report, not a wall-clock guess.

**Plans carry code where determinism pays for it, and not elsewhere.** A plan
with every test body written out costs thousands of controller tokens and is
the work done twice — but on this repository it has twice caught a design error
*before* an agent ran (a count over `cursor.top` cannot mean "go to line 12"; a
sacred key must also not open a sequence). Spend the tokens on the tests and
the tricky signatures; leave mechanical bodies to the agent.

**Do not block on a question you can answer yourself.** Nine of eleven
questions in that session were answered within four minutes and cost 18 minutes
total; two cost 4.6 hours because nobody was there. Every avoidable question is
a potential three-hour stall, so batch what must be asked, and otherwise state
the assumption and keep going.

**An intermittently red test is a bug, not noise.** Do not re-run it until it
goes green: that demonstrates nothing and hides the cause. There are ~26
wall-clock `sleep()`s in the suite, and under load any of them can lose its
race. Diagnose it — the repo's rule is test-first on bugs.

**Go through `just`, not through bare `cargo`, for anything that compiles the
workspace.** Cargo keys its artifacts on the feature set, so `cargo nextest run
-p norte-tui` and `just test` build two complete, separate universes of that
crate and everything under it — and cargo never garbage-collects, so both stay
on disk forever. One workspace universe is ~30 GB; this is how a target
directory reaches 300 GB. The `just` recipes all share one feature set
(`features` in the justfile) so targeted runs reuse what the gate compiled.
When space does run out, `just prune` reclaims without forcing a rebuild from
scratch (see the disk budget below for what it takes); `just prune-all` is the
hammer. `just ci` refuses to start below 40 GB free, because running out of
disk mid-build corrupts artifacts and surfaces as linker errors that look like
code bugs.

**Concurrent sessions get a worktree each: `scripts/wt.sh <name>`.** Never two
sessions on one tree. They clobber each other's edits without a word — the
serious risk — and they serialize on cargo's exclusive `target/` lock, each
recompiling what the other just built. A worktree isolates both. It is not
free: its `target/` is its own, so it is another ~30 GB and one cold build
(~250s). Use it when sessions genuinely overlap, and `git worktree remove` when
they stop — a forgotten worktree is 30 GB of nothing (`just disk` lists them).

### The disk budget

**The tree reached 288 GB in six days, and 184 GiB of it was dead test
binaries.** Measured 2026-08-12. The breakdown is the whole lesson:

| where the 288 GB was | |
| --- | --- |
| 2075 test executables in `debug/deps` (1262 of them >1 day stale) | 184 GiB |
| incremental cache | 54 GB |
| `target/semver-checks` (from `release-check`) | 17 GB |
| `llvm-cov-target` | 8.3 GB |

Two causes, and neither is what you would guess:

**Cargo never deletes an artifact.** Every code change relinks ~30 test
binaries; the previous 30 stay on disk forever. Nothing in the tree was older
than 7 days, so that 288 GB was six days of accumulation — roughly 40 GB/day of
pure garbage.

**Every test binary carried its own copy of the dependencies' debuginfo.** In a
measured `ntc`, `.debug_*` was 114 MB of 266 MB, duplicated 2075 times.
`debug = "line-tables-only"` did not help: `.debug_line` (42 MB) and
`.debug_str` (37 MB) ARE the bulk.

What is in place now, and what each part buys:

| lever | effect |
| --- | --- |
| `[profile.dev.package."*"] debug = 0` | deps carry no debuginfo at all (they are ~90% of the linked code; nobody debugs inside wasmtime) |
| `[profile.dev] split-debuginfo = "unpacked"` | our debuginfo goes to `.dwo` files SHARED by every binary — 4672 of them total 0.24 GiB — instead of being copied into each |
| `[build] incremental = false` in `.cargo/config.toml` | buys nothing for full-suite runs, cost 54 GB |
| `just prune [days]` | sweeps stale test exes and `semver-checks` on top of the coverage target |

Result: `target/` 288 GB → 30 GB, test exes 90 MB → 35 MB average, `ntc`
266 MB → 171 MB. Backtraces still resolve to `file:line` in our crates —
verified with `addr2line`, which is the check to repeat if anyone touches
`profile.dev`.

**The sweep only deletes executables, never `.rlib`/`.rmeta`.** That is
deliberate: a wrongly-swept executable costs a relink (seconds under lld), a
wrongly-swept `.rlib` costs a full compile.

**To run the dev build: `just link`, not `just install`.** It symlinks `ntc` and
`norte` from `~/.local/bin` (which precedes cargo's bin on PATH) to this tree's
`target/debug`, so the binary is whatever the last build produced — cost zero,
and never stale while you run tests. `cargo install --path` compiles in a target
directory of its OWN: a full cold build and another universe of disk every time
you want to try a change. `just link release` when you need to measure the
<50 ms cold start. Keep `cargo install` for installing for real, and note that a
stale `cargo install` shadows nothing but confuses everything — the binary was
renamed `norte-tui`→`ntc` once already and the installed copy kept the old name.

## Workspace map

- `crates/norte-proto`: protocol types. Any change affects the wire format and
  requires updated golden tests, a protocol version bump, and an extra review.
- `crates/norte-vfs`: the `Provider` trait and shared types such as `VPath`,
  `Entry`, and `Capabilities`.
- `crates/norte-vfs-{local,sftp,object,archive}`: independent providers. A
  provider must not know about other providers.
- `crates/norte-core`: task scheduler, daemon, policy engine, journal, and
  sessions.
- `crates/norte-{index,ai,mcp,plugin-host}`: core subsystems.
- `crates/norte-{tui,gui,cli}`: frontends. Business logic belongs in the core or
  a UI-independent shared crate.
- `crates/norte-testkit`: `MemProvider`, hostile fixtures, and proptest
  strategies.
- `docs/adr/`: architecture decision records. `docs/spec/`: specification.

## Hard rules

1. **Treat filenames as bytes.** Never assume paths are UTF-8. Use `VPath` or
   `OsString`; use `String` only for display, with an explicit lossy conversion.
   A `path.to_str().unwrap()` is grounds for rejecting a change.
2. **Do not perform blocking I/O in an async context.** Local filesystem work
   goes through `spawn_blocking` or `norte-vfs-local`. Do not call `std::fs`
   directly outside that crate.
3. **Represent every long-running operation as a task.** Its inner loop must
   check a `CancellationToken`, and each new task needs a clean-cancellation
   test.
4. **Send every mutation through the journal.** New mutations require a journal
   entry and an undo path, or an explicit `Irreversible` classification with a
   reason.
5. **Forbid `unsafe` by default.** Only `norte-vfs-local` may use it, with a
   `// SAFETY:` comment and a test.
6. **Use typed errors.** Libraries use `thiserror`; only binaries use `anyhow`.
   Do not use `unwrap()` or `expect()` outside tests unless a comment states the
   invariant that makes it safe.
7. **Keep business logic out of frontends.** Put it in the core and expose it
   through the protocol, or place presentation-only logic in a shared frontend
   crate.
8. **Justify new dependencies in the PR.** Cover the benefit, size, maintenance
   status, and alternatives considered.
9. **Do not let agents or plugins access the filesystem directly.** All access
   goes through the core and policy engine. Do not add temporary bypasses.
10. **Never store secrets in configuration or logs.** Store them in the system
    keyring and keep only references in configuration.

## Conventions

- Use Conventional Commits, for example `feat(vfs): ...` or `fix(tui): ...`.
  Keep pull requests below 400 net changed lines where practical, and give each
  PR one purpose.
- For a bug fix, add a failing test first. Encoding and path regressions also
  need a fixture in the canonical `norte-testkit` corpus.
- Add rustdoc and a doctest to every public item in protocol, VFS, and SDK
  crates. These crates enable `#![warn(missing_docs)]`.
- Route user-facing strings through Fluent resources in `i18n/`; do not
  hard-code them. Example: `t!("pane.copy.confirm")`.
- Instrument effectful core functions with `#[instrument]`. Include useful
  fields such as `task_id` and a redacted VPath.
- Record decisions that affect the protocol, licensing, security, or structural
  dependencies in a new ADR. Use the `/adr` project command.

## Review workflow

The reviewer agents (`rust-reviewer`, `protocol-guardian`, `security-reviewer`,
`encoding-auditor`, `test-engineer`) earn their keep — they have caught data
loss, wrong-file renames and overclaimed tamper evidence. What costs time is
*when* they run, not *that* they run.

**The agent doing the work dispatches its own reviewers before committing**, and
reports once with the findings already applied. One agent lifecycle instead of
three, and the reviewer's context is the code that is still warm. Pick by
surface, not by habit:

| surface touched | reviewer |
| --- | --- |
| `norte-proto`, JSON-RPC handlers | `protocol-guardian` (mandatory) |
| journal, policy, daemon auth, secrets, plugin-host, MCP | `security-reviewer` |
| paths, filenames, archives, viewers, search | `encoding-auditor` |
| any substantial Rust diff | `rust-reviewer` |

A second, external review pass is for work whose failure is expensive and
silent — the journal, the wire, the policy gate. Frontends, tests and
end-to-end wiring do not need one.

Give a reviewer the commit range, what the change is *for*, and the specific
questions you are unsure about. A reviewer told only "review this diff" returns
a checklist; one told "I chose X over Y here, and I am unsure whether Z can
race" returns the bug.

Apply BLOCKER and MAJOR findings. Say which MINORs you skipped and why —
silently dropping them is how a review becomes theatre.

## Definition of done

A pull request includes the code, unit tests, integration tests at OS or
provider boundaries, rustdoc, and any necessary changelog entry. `just ci`
passes locally, and every `TODO` links to an issue.

## Domain pitfalls

- macOS commonly normalizes filenames to NFD. Compare in NFC while preserving
  the original bytes.
- Windows paths longer than 260 characters require the `\\?\` prefix. Reserved
  names such as `CON`, `NUL`, and `AUX`, plus trailing dots or spaces, can be
  altered unless that prefix is used.
- ZIP filename encoding depends on bit 11: UTF-8 when set, otherwise often
  CP437 or a local encoding. Do not decode names blindly.
- Evaluate case-insensitive collisions against the destination filesystem, not
  the source.
- inotify watch counts are limited. Fall back to polling with a warning instead
  of failing.
- Cancelling a copy must leave a clean destination or a `.norte-partial` file,
  never an unmarked partial file.
