# 0065 - A frontend is retired before its replacement exists

- Status: accepted
- Date: 2026-08-20
- Decision makers: Oscar González
- Related: ADR 0027 (GPUI feasibility), the multi-frontend plan
  `docs/superpowers/plans/2026-08-19-multi-frontend-tauri-transition.md`
  (decision D1, task 8.3), `docs/architecture-review-2026-08-20.md` (H1).

## Context and problem statement

`norte-gui` was the GPUI graphical frontend: 32.058 lines, 374 inline tests, a
`main.rs` of 15.866 lines, and the lowest test ratio in the repository (0,79
against a workspace average of 0,96). ADR 0027 accepted it as a feasibility
spike; it grew into a second frontend.

It has been paid for twice ever since:

1. **As a second implementation of presentation rules.** `norte-frontend`
   exists precisely so that pane state, sorting, hostile-name display, keymaps,
   compare and sync presentation live once. The GPUI frontend nevertheless
   carried its own copies — `sync_view.rs` alone is 3.465 lines against the
   4.566 of `norte-frontend/src/sync.rs` — and the repository is full of
   comments explaining which rule had drifted in which direction.
2. **As a hole in the gate.** GPUI turns on `serde_json/preserve_order`, and
   cargo unifies features per invocation, so putting the GUI in the same
   `cargo` as the core changed the JSON the project publishes. The workaround
   was to exclude it: `core_pkgs = "--workspace --exclude norte-gui"`, a
   `check-gui` recipe, a separate `gui-ci` gate, a separate `deny.toml`
   allowing nine licences the workspace policy forbids, and `precise-builds`
   in the packaging config. The frontend that ran least was the one with the
   most infrastructure around it.

The multi-frontend plan already decided that the replacement is not GPUI, and
that its boundary (`norte-client` + `norte-ui-host`) is toolkit-independent.
That plan's decision D1 says the migration is additive: keep GPUI alive
through construction and retire it in task 8.3, after parity.

The question this ADR answers is whether that is still the right order.

## Options considered

### Option A — Keep GPUI until the new frontend reaches parity (plan D1)

- **Advantage:** users never lose the graphical frontend, and every phase can
  be measured against a working reference.
- **Advantage:** the parity ledger has something to compare against
  behaviourally, not only against the TUI.
- **Drawback:** every change to `norte-frontend`, to the protocol or to the
  shared presentation rules is paid twice for months, and the second copy is
  the one nobody runs.
- **Drawback:** the exclusion infrastructure stays, and with it a gate that
  does not cover a member of the workspace.
- **Drawback:** the new frontend is built while the old one still defines what
  "the GUI" means, which is how a rewrite inherits the shape it was meant to
  leave behind.

### Option B — Retire GPUI first, then build

- **Advantage:** one presentation implementation from day one. Everything the
  new frontend needs is in `norte-frontend` or it is missing, and missing is
  visible.
- **Advantage:** the gate simplifies to the whole workspace: no `--exclude`,
  no second `deny.toml`, no `gui-ci`, no `check-gui`.
- **Advantage:** the parity target becomes the TUI, which is the complete
  frontend and the one covered by 900 tests — a better reference than a GUI
  that never had feature parity either.
- **Drawback:** there is no graphical frontend at all until the new one lands.
- **Drawback:** the GPUI behaviour is only recoverable from git history.

## Decision

**Option B.** `norte-gui` is removed from the repository, and the plan's
decision D1 is amended: the migration is additive in its BOUNDARIES
(`norte-client` and `norte-ui-host` are new crates that nothing existing has to
adopt at once), not in its frontends.

Two things make the cost acceptable and were checked before deciding:

- The graphical frontend was already **experimental and source-only** — never
  packaged, never in a release, and documented as such in the README. Nobody
  is running an installed copy that this breaks.
- The behaviour is not lost, it is **in git**. Anything the new frontend wants
  from it is one `git show` away, and what it wants is design, not code: the
  code is GPUI-shaped and the replacement is not.

## Consequences

### Positive

- One implementation of every presentation rule, enforced by there being only
  one frontend able to hold it.
- `just ci` covers the whole workspace. `core_pkgs`, `check-gui`, `gui-ci`,
  the GUI `deny.toml` and the workspace `default-members` split all disappear.
- The licence policy applies to everything the repository builds again: the
  nine exceptions the GPUI tree needed were the only reason for a second
  policy file.
- 32.058 lines and 14 GB of a private `target/` leave the tree.

### Negative

- **No graphical frontend until the new one lands.** The TUI and the CLI are
  the frontends, and the README says so.
- **`norte gui` is gone from the CLI.** The subcommand returns when there is a
  binary to hand the process to; keeping a command that can only fail would be
  worse.
- The keymap presets lose their graphical half-check
  (`todos_los_presets_construyen_las_tres_pantallas_de_la_gui`): until the new
  frontend brings its own command list, no preset is validated against a
  graphical surface. This is recorded in the two doc comments that used to
  point at it.
