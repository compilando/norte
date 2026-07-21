# Contributing to norte

Thank you for helping improve norte. This guide covers the repository workflow;
crate-specific constraints are documented in [CLAUDE.md](CLAUDE.md), and the
system boundaries are summarized in [ARCHITECTURE.md](ARCHITECTURE.md).

## Set up the workspace

Install the pinned Rust toolchain and development tools with:

```sh
make setup
```

Before opening a pull request, run:

```sh
just ci
```

For a faster test-only cycle, use `just test`. The full CI command checks
formatting, Clippy warnings, licenses, advisories, tests, and documentation.

## Make a focused change

- Keep each pull request focused on one problem. Prefer small changes that are
  easy to review; roughly 400 net changed lines is a useful target, not a hard
  limit.
- Use Conventional Commits, such as `fix(vfs): preserve non-UTF-8 names`.
- Add a failing regression test before fixing a bug.
- Update public rustdoc, user documentation, schemas, and the changelog when a
  change affects them.
- Link every new `TODO` to an issue.

Do not mix generated files, dependency updates, refactors, and behaviour changes
unless they are inseparable.

## Respect the architecture

The most important constraints are:

- Preserve filename bytes. Never assume a path is UTF-8.
- Keep blocking filesystem work outside async executor threads.
- Represent long operations as cancellable tasks.
- Journal every mutation and provide undo, or explicitly classify it as
  irreversible.
- Keep business logic out of frontends.
- Route agent and plugin filesystem access through the core and policy engine.
- Keep secrets out of configuration, diagnostics, fixtures, and logs.

Protocol changes require updated golden fixtures, a protocol-version review,
and additional maintainer review. Security, licensing, and structural dependency
decisions require an ADR.

## Tests and documentation

Choose tests that match the boundary you changed:

- Unit tests for local decisions and parsing.
- Property tests for byte paths, normalization, and other round-trip contracts.
- Provider contract tests for VFS implementations.
- Integration tests at operating-system, transport, or provider boundaries.
- Clean-cancellation tests for every new task.
- Journal-based undo tests for every new mutation.

Write public documentation in English. Keep examples executable where practical
and use repository-relative links in Markdown. JSON Schemas are generated from
Rust types; regenerate them with:

```sh
NORTE_UPDATE_SCHEMA=1 cargo nextest run -p norte-tui --features schema schemas
```

## Licenses

Protocol, VFS, testkit, presentation-library, and future plugin-SDK crates use
`MIT OR Apache-2.0`. The core and official frontends use `AGPL-3.0-only`.
Check the target crate before adding or moving code.

## Reporting security issues

Do not open a public issue for a suspected vulnerability. Follow
[SECURITY.md](SECURITY.md) instead.
