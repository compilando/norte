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

Pace CI to avoid slowing iteration. **The ladder, cheap to expensive — climb a
rung only when the one below it is green:**

| step | when | warm cost |
| --- | --- | --- |
| `just t <crate>` | the RED→GREEN loop | ~10s |
| `just ci-fast` | before calling a task done | ~34s |
| `just ci` | before a commit or a push | ~143s |

`cov` is 76% of `just ci` (109s of 143s) and can only move if you touched
proto/vfs/core — the sole crates under the 85% gate — so keep it out of the
loop. Filtering tests (`-E 'test(...)'`) buys nothing: compilation dominates,
and running the whole suite is ~12s. Run the full `just ci` once per change,
never on a loop.

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
When space does run out, `just prune` drops the incremental cache and the
coverage target without forcing a rebuild from scratch; `just prune-all` is the
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
