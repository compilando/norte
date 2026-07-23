# 0035 - Shared norte-config crate and unified configuration resolution

- Status: accepted
- Date: 2026-07-23
- Decision makers: Oscar González
- Related: design spec `docs/superpowers/specs/2026-07-23-help-config-system-design.md`;
  ADR 0006 (keymap resolution), ADR 0007 (layered configuration and hot
  reload), ADR 0010 (core/plugin/config boundaries), ADR 0031 (AI subsystem,
  `[ai]` in `norte.toml`).

## Context

Configuration resolution has forked. Three divergent resolvers compute the
user config directory today:

- `norte-core/src/connect.rs:126-143` honors `NORTE_CONFIG_DIR`.
- `norte-tui/src/config.rs:208-241` does **not** — with the env var set, the
  TUI splits its own configuration across two roots (core-owned files read from
  the override, TUI-owned files from the XDG path).
- `norte-gui/src/keymap.rs:63-82` has no Windows branch at all.

`norte.toml` itself has three parsers: the TUI's strict
`deny_unknown_fields` struct versus tolerant single-table scrapers for
`[archive]` and `[ai]` in norte-core. Layering is asymmetric too: the TUI
merges system/user/project layers per ADR 0007, while the daemon and CLI read
only the user layer.

ADR 0007 deferred extraction with the clause "keep the implementation in
norte_tui until the daemon needs shared configuration". That clause has now
fired: the GUI, the CLI, and the daemon each grew a fork instead of sharing
one.

## Decision

Add a **`norte-config` crate** owning path resolution, the canonical strict
`NorteToml` model, and layered merging, used by every frontend and the core.

### Resolver precedence (everywhere)

The user config directory resolves, in order:

1. `NORTE_CONFIG_DIR` (if set),
2. `XDG_CONFIG_HOME/norte` (if `XDG_CONFIG_HOME` is set and non-empty),
3. Windows: `%APPDATA%\norte`,
4. `$HOME/.config/norte`.

Every consumer — core, daemon, CLI, TUI, GUI — uses this single resolver.

### `NORTE_CONFIG_DIR` is hermetic

When `NORTE_CONFIG_DIR` is set, `standard_layers()` returns **only**
`(that directory, User)` plus `(./.norte, Project)`. No `/etc/norte` (or
`%ProgramData%\norte`) layer is consulted. The variable exists for tests and
headless isolation; leaking system configuration under an explicit override is
a footgun.

### Canonical strict `NorteToml`, including `[ai]`

The strict model gains an `[ai]` table. This fixes a latent bug: the TUI's
`deny_unknown_fields` struct at `crates/norte-tui/src/config.rs:24-47` has no
`ai` field while ADR 0031 documents `[ai]` in the same `norte.toml` — a user
configuring AI breaks TUI startup today.

`[ai]` is honored from the System and User layers only, never Project — the
same carve-out ADR 0010 established for `[archive]`.

Merge semantics across layers:

- Scalars: last-present-wins (ADR 0007 precedence).
- `denied_prefixes`: **union** across layers — a deny never disappears by
  adding a layer.
- `providers`: merge by name; for a given name the later layer wins.

### Uniform strictness

The daemon and CLI now parse the full strict `NorteToml`. A `[ui]` typo fails
daemon startup exactly as it fails the TUI. This is ADR 0007's rule applied
uniformly: invalid configuration is a startup error, not a silent skip.

### `policy.toml` stays single-file

`policy.toml` remains a single user-layer file. This asymmetry is deliberate
and documented, not changed by this ADR: the policy file is a security
boundary, and layered merging of allow/deny rules invites surprises.

### Licensing

The crate is `MIT OR Apache-2.0`, following the shared-library pattern of
`norte-frontend` (ADR 0003 crate-license policy).

## Consequences

- `norte-core` gains a dependency on `norte-config`; the three ad-hoc
  resolvers and the tolerant scrapers are deleted.
- **Behavior change**: `NORTE_CONFIG_DIR` becomes hermetic. Anyone relying on
  `/etc/norte` merging underneath the override loses that merge; they must
  copy the system layer into the override directory.
- **Behavior change**: daemon startup becomes strict about the whole
  `norte.toml`. Previously-ignored typos in sections the daemon did not read
  now fail startup — surfacing errors ADR 0007 always intended to surface.
- `docs/schema/norte.schema.json` regenerates and gains the `ai` table.
- One resolver means the TUI no longer splits its configuration across two
  roots when `NORTE_CONFIG_DIR` is set, and the GUI gains the Windows branch
  for free.
