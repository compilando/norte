# 0007 - Layered configuration and hot reload

- Status: accepted
- Date: 2026-07-11
- Decision makers: Oscar González
- Related: specification section 13, M1 phase 6, ADR 0006

## Context

norte uses separate TOML files in ordered default, system, user, project, and
CLI layers. The project needs exact merge rules, distinct startup and hot-reload
failure behaviour, and a fallback when native filesystem watchers are
unavailable or exhausted.

## Decision

- Merge scalar fields in ascending precedence: compiled defaults, `/etc/norte/`
  (or `%ProgramData%\norte`), the user config directory, `./.norte/` in the
  current working directory, then CLI flags. A present higher-layer field wins;
  an absent field inherits from below.
- Fold `keymap.toml` layers according to ADR 0006 rather than treating them as
  scalars. Higher-priority prepends come before the preset, followed by appends
  in descending priority. Select the preset through `[keymap].preset` or a CLI
  flag.
- Reject unknown keys with a diagnostic containing the file and field. Invalid
  configuration is a startup error. During hot reload, retain the complete last
  valid configuration and show a status warning instead of disrupting the
  session.
- Debounce events and reload all layers as one unit. If the native watcher
  cannot start, poll mtimes every two seconds and tell the user that monitoring
  has degraded.
- Keep a slow backup poll active even with native watching so newly created
  layer directories and dropped inotify events are detected. Watcher errors also
  trigger a full reload. Dropping `Watch` cancels both mechanisms.
- Keep the implementation in `norte_tui::config` until the daemon needs shared
  configuration.
- Permit this frontend configuration module to read its own files directly;
  using VFS providers would be circular. Async callers must use `load_async` or
  `spawn_blocking`.
- Generate `docs/schema/norte.schema.json` and `keymap.schema.json` with
  `schemars` from the same Serde types used for parsing. Golden tests detect
  drift.

## Consequences

- Precedence is easy to explain and diagnostics can identify a value's source.
- Saving a half-written TOML file does not break a running session.
- Published schemas provide editor validation and completion.
- Project configuration is limited to the current directory; upward discovery
  may be added when norte defines a project root.
- `notify` and `schemars` become dependencies. `schemars` may later move behind
  a feature if binary size warrants it.
- Debouncing adds roughly 300 ms between saving and observing a change.
- Additional files such as `theme.toml` and `openers.toml` arrive with the
  features that consume them.
