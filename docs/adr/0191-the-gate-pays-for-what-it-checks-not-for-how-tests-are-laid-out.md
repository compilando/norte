# 0191 - The gate pays for what it checks, not for how tests are laid out

- Status: accepted
- Date: 2026-11-02
- Decision makers: Oscar González (asked for a faster gate and chose each
  option below), with the measurements by the implementer
- Related: ADR 0087 (the pre-push gate), the gate and disk budgets in
  `CLAUDE.md`.

## Context and problem statement

Every push to `main` pays `just ci-fast`, and it had grown to ~9 minutes.
Measured on 2026-11-02 after touching `norte-core`, step by step and in
series:

| step | wall |
| --- | --- |
| `cargo doc` | 150–235 s — norte-tui's lib 94 s, the `norte` binary 69 s, norte-mcp 46 s, `ntc` 22 s |
| doctests | 144 s (44 s with nothing changed: never cached) |
| test build | ~105–119 s, 957 s of CPU; norte-core (74 binaries) and norte-tui (62) were 790 s of it |
| test run | 83–93 s; 1252 s of summed duration over 16 threads, already near ideal |
| clippy | 52 s |
| `deny-guests` | 21 s fixed, 14 manifests one after another |

Running the tests is not the problem: nextest already packs them near the
ideal. The cost is in what gets compiled and documented, and in tests that
sleep through fixed deadlines.

## Considered options

1. **Document only the libraries in `ci-fast`; the frontends at release.**
   - Good: removes the two longest rustdoc runs and the step that died of
     OOM under load.
   - Bad: a broken intra-doc link in norte-tui, norte-cli or norte-mcp is
     found by `just ci` at release-prep, not on the push.
2. **Run docs in a second target directory, in parallel with the tests.**
   - Good: keeps every check on every push.
   - Bad: another ~30 GB universe, and two heavy builds at once is exactly
     the memory pressure that already kills `docs`.
3. **One integration-test binary per heavy crate.**
   - Good: 135 binaries that each link the whole core and wasmtime become
     two (plus five kept apart); less CPU, less disk, faster `just t`.
   - Bad: a new file in `tests/` needs its line in `tests/it.rs`, or it
     never compiles.
4. **Shorten the fixed waits.**
   - Bad: a silence window that is too short is the intermittent red this
     repository treats as a bug. Rejected in favour of ending the wait on the
     condition, or running independent waits at once.

## Decision outcome

Options 1, 3 and the condition-based variant of 4:

- `ci-fast` runs `docs-fast`: `docs` minus norte-tui, norte-cli and
  norte-mcp. `just ci` keeps `docs` whole.
- norte-core and norte-tui declare `autotests = false` and one
  `[[test]] it` whose `tests/it.rs` includes every file with
  `#[path] mod`. The files stay where they were, so relative `include_*!`
  paths and nested modules resolve as before. norte-tui keeps as their own
  binaries the four insta suites — their `.snap` files are named after the
  module path — and `schema`, which needs a feature. A test in each `it.rs`
  fails when a file in `tests/` is in neither place.
- `drain_sync` stops when it has the terminal state and the close, with a
  2 s grace when only the terminal came; the `probes` tests observe every
  panel at once, each on its own host, instead of spending the 3 s deadline
  once per panel in turn.
- `deny-guests` fetches the advisory database once and checks the fourteen
  manifests in parallel, offline.

nextest still runs each test in its own process, so merging binaries
changes no test's isolation.

### Consequences

- Good: `ci-fast` after a core change went from ~9 min to 287 s; norte-core
  links 2 test binaries instead of 74 and norte-tui 8 instead of 62; the
  sync tests went from 10.2 s to under 0.2 s and the probes from 22 s to
  3.2 s.
- Good: fewer rustdoc runs on the push means fewer OOM kills of `docs`.
- Bad: frontend doc links are checked only at release.
- Bad: `tests/it.rs` is one more place to edit when adding a test file — the
  guard test says so instead of letting the file go silently uncompiled.
- Not covered: doctests (44 s even with no change) and the payload tests
  over 100 000 entries, whose cost is the work they measure.
