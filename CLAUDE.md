# Claude Code project instructions

norte is an orthodox file manager with a headless Rust core, a JSON-RPC daemon,
TUI/GUI/CLI clients, and MCP-based agent access. Read the
[project specification](docs/spec/norte-spec.md) for the full design and
[ARCHITECTURE.md](ARCHITECTURE.md) for a repository map.

## Commands

```sh
cargo build --workspace                  # Build the full workspace
cargo nextest run --workspace           # Run tests; do not use `cargo test`
cargo nextest run -p norte-vfs          # Test one crate
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo llvm-cov nextest --workspace      # Local coverage; CI threshold is 85% for core/VFS/proto
cargo deny check                        # Licenses and security advisories
just ci                                 # Run the complete local CI suite
```

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
