# Settings UI (VSCode-style) & per-directory cursor memory

- Date: 2026-07-24
- Status: approved (user request 2026-07-24: cursor memory; visible, searchable,
  modular settings — "igual que el de vscode, sencillo, con buscador, de
  generales y plugins", incl. a confirm-on-quit setting)
- Related: help-config spec (2026-07-23), GUI plugins spec (2026-07-23),
  ADR 0035/0036, P2 plugin `[config]`

## Problem

1. Navigation resets the pane cursor to 0 on every `cd` (`PaneState::set_listing`,
   norte-frontend pane.rs:145); no per-directory memory, no
   cursor-on-child-you-left when navigating to the parent. Both frontends.
2. Everything built for help/config so far is TUI/CLI-only or file-only:
   there is NO in-app settings surface in either frontend (only the theme
   picker and hotlist persist anything). The modular/autodiscovery machinery
   (schemas with descriptions, plugin `[config]`, validation) has no visible
   face — the user rightly "doesn't see it".

## Phase S1 — per-directory cursor memory (shared)

In `norte_frontend::PaneState` (both frontends inherit for free):

- `cursor_memory: LruMap<VPath, usize>` (cap 64, simple Vec/ordered impl —
  no new dependency). On leaving a directory (`begin_loading`/`begin_listing`
  call sites record `current dir → cursor` first), on `set_listing(dir, …)`
  restore by lookup, clamped to the listing length; absent → 0 as today.
- Parent-nav polish: `nav.parent` passes the child LEFT (the dir you came
  from) as a hint; after listing, the cursor lands on that entry (byte-exact
  path match; falls back to memory/0). Applies to both frontends' parent
  dispatch (TUI main.rs nav.parent arm; GUI run_command nav.parent).
- Memory is per-pane, session-only (no persistence — matches the History
  precedent), survives refresh/pagination via the existing re-anchor paths.
- Tests: unit on PaneState (save/restore/clamp/LRU eviction/hostile paths
  byte-exact), parent-nav hint in both frontends' tests where dispatch is
  testable.

## Phase S2 — settings model + generic persist (shared)

- `norte_frontend::settings`: a CURATED, Fluent-localized registry
  `SettingsCatalog` of general settings — NOT parsed from the JSON schema at
  runtime (schema descriptions are English rustdoc; the UI must localize).
  Each entry: stable id (`ui.theme`, `ui.font_size`, `keymap.preset`,
  `ui.quick_search`, `ui.reduce_motion`, `ui.lang`, `ui.confirm_quit`, …),
  kind (bool / enum(values) / string / int(range) / theme-name /
  preset-name), current value (read from `FrontendConfig`/CommonConfig),
  default, Fluent description key (`setting-<id>` in BOTH locales), and an
  `applies_live: bool` flag (GUI shows "restart required" when false).
  Coverage test: every entry's Fluent keys exist in both locales AND its
  read/write accessors compile against the real config types (the registry
  cannot drift from `NorteToml`).
- Plugins section: entries generated from each approved plugin's manifest
  `ConfigKeySpec` map (id, type, default, description — plugin-supplied,
  MASKED) + current values (`settings_of`). Embedded mode only for values
  (the P2 wire deferral stands; remote shows the mode note).
- Generic writer in `norte-config`: `persist_set(dir, section: &str,
  key: &str, value: toml_edit::Value)` — comment-preserving, mirrors
  `persist_ui_theme_to`; `persist_ui_theme` becomes a thin wrapper. Plugin
  values: `persist_plugin_setting(config_dir, plugin_id, key, value)` writing
  `plugins/<id>/config.toml` (validated against the schema BEFORE writing —
  never persist an invalid value).
- New setting wired end-to-end as the exemplar: `[ui] confirm_quit =
  "auto" | "always" | "never"` (default `auto` = today's pending-work-only
  behavior). GUI `quit_or_confirm` gains the always/never branches; the TUI
  gains a ConfirmQuit modal path on `app.quit` (dialog-context, `dialog.confirm`
  allowlist — reuses H1 machinery) honoring the same setting.

## Phase S3 — TUI settings overlay

- Command `app.settings` (all presets; palette lists it; suggest `f10` if
  free — verify) opening a palette-style overlay: search input (nav::fold
  filter over id+localized name+description), filtered list `name  value`
  with the description of the selected entry in a footer line.
- Editing: Enter/space cycles bool/enum; string/int opens the inline input
  (nav-popup name-input pattern) with validation before persist (invalid →
  status-bar error, value untouched). Writes via S2; the existing hot-reload
  applies changes live (`applies_live` is true for everything the TUI
  hot-reloads).
- Sections rendered as headers (General / Plugins). Hostile plugin
  descriptions masked (P1 discipline). Snapshot + unit tests; hostile corpus
  pin on the plugin rows.

## Phase S4 — GUI settings view

- Full-view swap (the viewer pattern: `Option<SettingsView>` child of root),
  opened by `app.settings` (GUI COMMANDS + subset build picks the shared
  preset binding). Search field on top, grouped list below, mouse + keyboard
  (up/down/enter/esc; hover from GP).
- Editing like S3; on write, GUI re-resolves what it can live (theme,
  effects, fonts recompute at next render where cheap) and marks the rest
  "restart required" (`applies_live=false`: e.g. daemon mode).
- This is the user-visible "VSCode settings": simple, searchable, general +
  plugins, with descriptions.

## Order & gates

S1 → S2 → S3 → S4, each with the session's review cycle (rust-reviewer;
encoding-auditor on S3/S4 rendering; no proto changes — plugin values stay
host-side per P2; if a wire need appears, STOP → guardian → re-plan). Then
resume G3 → G4.

## Out of scope

- Persisting cursor memory across sessions.
- Editing keybindings from the settings UI (keymap.toml stays file-edited;
  a future phase).
- Remote-mode plugin setting VALUES (P2 deferral stands until G3's bump).
