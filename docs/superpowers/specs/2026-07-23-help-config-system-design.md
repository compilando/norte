# Help & configuration system — unification and plugin extension

- Date: 2026-07-23
- Status: approved (scope: "A completo", all six phases)
- Related: ADR 0006 (keymap), ADR 0007 (layered config), ADR 0010 (config vs
  plugin boundary), ADR 0020 (themes), ADR 0022 (plugin host), spec §8/§13

## Problem

Configuration handling is fragmented and help/discoverability is uneven:

1. **Three divergent config-dir resolvers**: `norte_core::connect::config_dir`
   (honors `NORTE_CONFIG_DIR`), `norte_tui::config::user_config_dir` (does
   not), `norte_gui::keymap::env_user_dir` (no Windows branch). With
   `NORTE_CONFIG_DIR` set, the TUI splits its own configuration across two
   roots.
2. **Three parsers of `norte.toml`**: the TUI's strict `NorteToml`
   (`deny_unknown_fields`) versus the tolerant `[archive]` and `[ai]` scrapers
   in `norte-core`. A typo is a hard error in one frontend and silence in
   another.
3. **Asymmetric layering**: the TUI merges system/user/project layers; the
   daemon and CLI read only the user layer, so `[archive]` limits in
   `/etc/norte/norte.toml` bind the TUI but not the daemon.
4. **GUI drift**: hand-rolled fork of layer discovery (no Windows support,
   `bool is_project` instead of the `Layer` enum), hardcoded theme, no
   `norte.toml` at all, its own copy of the orthodox preset.
5. **Help exists only as the TUI F1 screen.** That screen is the model to
   generalize: auto-generated as the join of the effective keymap and the
   Fluent catalog over the single `COMMANDS` source of truth, with a test that
   breaks the build when a command lacks an EN+ES description. But modal key
   hints are hardcoded strings (issue #24), the command palette promised in
   spec §8 does not exist, and `norte doctor` (spec §13) does not exist.
6. **Plugins are mute and unconfigurable**: no `description` field in the
   manifest, nothing beyond name/badges on the wire or in the extension
   manager, no per-plugin configuration mechanism (empty WASI env by design).

ADR 0007 kept the layered loader in `norte_tui` "until the daemon needs shared
configuration". That condition is now met: GUI, CLI, and daemon each need it
and each grew a fork.

## Design principle

One pattern, applied everywhere: **a single declared source of truth, a join
computed at runtime, and a test that forces coverage.** The F1 help screen
already does this for TUI commands. This design extends the same shape to
configuration loading, modal hints, the palette, `doctor`, and plugin
metadata/config. No hand-maintained parallel lists anywhere.

## Phase C1 — `norte-config` crate (foundation)

New workspace crate `crates/norte-config` (lib, `thiserror`, no dependency on
`norte-core` or any frontend; `norte-core` and all frontends depend on it).
Requires an ADR (structural change, fulfils the ADR 0007 relocation clause).

Contents, moved or unified — not rewritten:

- **`config_dir()`** — the one resolver. Precedence: `NORTE_CONFIG_DIR` →
  `XDG_CONFIG_HOME/norte` (non-empty) → Windows `%APPDATA%\norte` →
  `$HOME/.config/norte`. Replaces resolvers in `connect.rs:126`,
  `tui/config.rs:233`, `gui/keymap.rs:63`.
- **`Layer` enum + `standard_layers()`** — moved from `norte_tui::config`.
  System = `/etc/norte` (`%ProgramData%\norte` on Windows), User =
  `config_dir()`, Project = `./.norte`. The user layer now honors
  `NORTE_CONFIG_DIR` (fixes the split-root bug).
- **One `NorteToml`** — the strict TUI struct becomes the canonical parser,
  including `[archive]` and `[ai]` sections. `norte-core`'s
  `archive_config.rs` and `ai.rs` raw scrapers are deleted; core consumes the
  typed sections from `norte-config`. Unknown fields are a hard error
  everywhere, uniformly (ADR 0007: invalid configuration is a startup error).
- **Layered load for core consumers** — `load_archive_limits` and
  `AiConfig::load` now merge layers like the TUI does. Existing per-layer
  security carve-outs are preserved exactly (project-layer `[archive]` is
  dropped; hotlist/openers project rules unchanged). `policy.toml` stays
  deliberately single-file user-layer (fail-closed, security-sensitive); the
  spec note documents this asymmetry as intentional.
- **`watch()`** — the notify/polling watcher moves too, so C2 can reuse it.
- Hostile-content-safe `toml_diag` (issue #73 guard) moves with the loader.

Migration: TUI, CLI, GUI, daemon all switch to `norte-config`. Pure
relocation plus the two behavior fixes above (env var in layered path; core
honors layers). Golden diagnostics tests move with the code.

Testing: existing `config.rs` tests relocate; new tests pin (a) all four
consumers resolve the same directory under `NORTE_CONFIG_DIR`, (b) a `[ui]`
typo fails daemon startup the same way it fails the TUI, (c) system-layer
`[archive]` binds the daemon.

## Phase C2 — GUI parity

The GUI consumes `LoadedConfig` from `norte-config`: `[ui].theme` (drops the
hardcoded default theme), `[keymap] preset` (vim/cua become available),
hotlist, quick_search. The hand-rolled `layer_dirs`/`env_user_dir` fork is
deleted. The duplicated orthodox preset is removed — presets live once, in a
shared location (`norte-frontend` alongside the keymap engine), embedded by
both frontends. Hot reload in the GUI is optional here; not doing it is
acceptable, doing it is cheap since `watch()` is shared.

## Phase H1 — dialog keymap context + command palette

**Dialog context (closes issue #24).** Add the `dialog` context to the keymap
(ADR 0006 anticipated this). Modal keys (confirm/collision/approval/
trust-host, theme picker, extension manager, hotlist) move from hardcoded
`match` arms to bindings in the presets under `context = "dialog"` (plus
narrower contexts if needed, resolved most-specific-first per ADR 0006).
Modal footer hints are then **generated** from the effective keymap the same
way F1 is — the `modal-*-keys` Fluent strings become per-command labels, and
the hardcoded key legends are deleted. The F1 coverage test extends to the new
contexts. Rebinding can no longer desync a hint.

Non-goal: approval-modal semantics stay as they are (Enter must not approve;
n/Esc deny). The binding data moves; the safety rules stay in code.

**Command palette (spec §8).** New command `app.palette` (bound in all three
presets, e.g. `ctrl+p` / `:` in vim). An overlay listing every `COMMANDS`
entry: name, effective binding (or "unbound"), Fluent description — the same
join F1 uses, plus substring filter and Enter-to-run. Plugin commands appear
as `plugin:<id>:<command>` rows using the manifest `title` (and, after P1,
`description`), running via the existing `plugin.run_command` backend path.
Palette rows for plugins render with the untrusted-text masking already used
by the extension manager. The palette is presentation only (rule 7): it reads
the same sources, executes the same dispatch.

Wire note: the palette needs plugin command contributions client-side.
`PluginInfo` does not carry them today; that field rides the single P1
protocol bump (see below) — H1 ships human commands first if it lands before
P1.

## Phase H2 — `norte doctor`

New CLI subcommand. Read-only diagnostics, no daemon required:

- Config: resolve layers, parse every file, report file+field errors; warn
  when `NORTE_CONFIG_DIR` is set and any legacy path still contains files.
- Keymap: build effective maps per context, report prefix conflicts and
  bindings to unknown commands.
- Plugins: run discovery, report catalog errors, digest-mismatch (approval
  reset) states, missing `plugin.wasm`.
- Connections: validate `connections.toml`, report unresolvable secrets
  (without touching them), optional `--probe` for reachability.

Output: human-readable table plus `--json`. Exit code non-zero on errors,
zero with warnings. This reuses loaders from C1 — doctor is a consumer, not a
second implementation.

## Phase P1 — plugin descriptions on the wire

Manifest: optional `description: Option<String>` in `[plugin]` (length-capped,
e.g. 256 chars, masked as untrusted text wherever rendered). Like
`name`/`publisher`/`version` it is cosmetic: **excluded from the approval
digest** — editing a description must not reset approval.

Protocol (one bump, protocol-guardian review, goldens updated):

- `PluginInfo.description: Option<String>` (skip-if-none).
- `PluginInfo.commands: Vec<PluginCommandInfo { id, title }>` — the palette's
  feed (H1 dependency).

Surfacing: extension manager shows the description line under the plugin row;
palette shows title + description for plugin commands.

## Phase P2 — declarative per-plugin configuration

The only phase with new WIT surface. Design:

**Manifest `[config]` schema.** Keys declared declaratively:

```toml
[config]
max-depth = { type = "int", default = 3, min = 0, max = 32, description = "…" }
style     = { type = "enum", values = ["plain", "fancy"], default = "plain" }
verbose   = { type = "bool", default = false }
```

Types: `string`, `bool`, `int` (with optional min/max), `enum`. Nothing else
(no nesting, no lists) — YAGNI. `deny_unknown_fields` on the schema structs.
The `[config]` schema **is included in the approval digest**: a plugin that
grows settings changes behavior and re-consents.

**User values.** `config_dir/plugins/<id>/config.toml`, flat key = value.
Host-side validation against the schema at load: unknown key, type mismatch,
or range violation → plugin load error in the catalog (fail-closed, visible in
the extension manager and `doctor`), never a silent default.

**Delivery to the guest.** Following the `provider-config` precedent (ADR
0033): validated settings are passed at instantiation as a
`list<tuple<string, string>>` (canonical string encoding per type) through a
new optional WIT interface method. Guests that don't import it are unaffected.
No WASI env, no host filesystem access — the empty-sandbox invariant (ADR
0022) holds.

**Autodiscovery for free.** The extension manager renders each enabled
plugin's settings from the schema (key, type, current value, default,
description) — generated UI, no per-plugin code. Editing values in-app is a
possible follow-up, not in scope; P2 ships read/validate/deliver + display.

## Error handling (cross-cutting)

Unchanged philosophy, now uniform: startup errors are loud with file+field;
hot reload keeps last-valid with a status warning; all plugin-supplied text
(description, titles, config descriptions) is untrusted and masked; catalog
and config errors are surfaced, never swallowed.

## Testing (cross-cutting)

- C1: resolver-equivalence tests across all consumers; layered-parity tests
  (system `[archive]` binds daemon); relocated goldens.
- H1: coverage test extended to `dialog` contexts (every bound command has
  EN+ES text); palette snapshot with hostile plugin titles.
- H2: doctor fixture tree with one broken file per category; `--json` golden.
- P1: proto goldens for the bump; digest test proving description edits do not
  reset approval.
- P2: schema validation matrix (type/range/unknown-key), digest test proving
  `[config]` edits DO reset approval, guest round-trip through the WIT method
  with hostile values, catalog error surfacing.

## Order and dependencies

C1 first (everything else consumes it). Then H1, H2, P1 in any order (H1's
plugin rows wait for P1's wire field). P2 last. Each phase is one PR-sized
unit with its own reviewers per house rules (protocol-guardian on P1/P2 wire
changes, security-reviewer on P2, encoding-auditor on palette/hints
rendering).

## Out of scope

- Editing plugin settings interactively in the extension manager (follow-up).
- CLI `--help` localization / man pages (separate decision; clap Spanish
  doc-comments stay).
- which-key overlay (ADR 0006 leaves it conditional).
- Lua-facing config APIs.
- GUI hot reload (optional in C2, not required).
