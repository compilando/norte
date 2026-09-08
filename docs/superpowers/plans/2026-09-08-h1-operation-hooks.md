# H1 — operation hooks: implementation plan

Approved 2026-09-08 (after-only, `notify` as the single effect). Supersedes
`docs/superpowers/specs/2026-09-04-operation-hooks-design.md`; the decision
goes to ADR 0100. Direct work on `main`, one commit per task, ONE `just
ci-fast` at the end and ONE `just ci` before the push.

## What is built

- **Source of events: the journal's commit path.** `Journal::record_entry`
  hands every committed row to a `HookSender` (bounded, `try_send`, drops
  counted). Every writer — daemon `SqliteJournal`, embedded `LazyJournal`,
  batch wrappers, undo compensations — ends there, so no frontend and no
  handler has a say in whether a hook fires (ADR 0077 parity).
- **Vocabulary = journal ops.** `on = "after-created" | "after-removed" |
  "after-trashed" | "after-renamed" | "after-mode-changed"`. Closed; an
  unknown value is a manifest error. No `after-batch`: events carry
  `batch`, and the guest receives a **list** of events per call, so a
  rename batch reaches it as a group and it can say "renamed 3 files".
- **WIT `norte:hook@0.1.0`**, world `norte-hook`: imports `host-log`,
  `host-config`, `norte:location/location@0.2.0`; exports
  `hook::on-events(list<event>) -> result<list<effect>, string>`. `event` =
  `{ seq, ts-ms, op, actor, path: list<u8>, path-to: option<list<u8>>,
  batch: option<u64>, location: option<location-ref> }`. `effect` =
  `notify(string)`. A hook cannot mutate.
- **Dispatcher** (`norte-core::hooks`): one task per engine; drains up to 256
  events, discovers the registry once per drain (so approvals are always
  fresh), one instance per hook plugin per drain, mints one location session
  per distinct parent directory when `location = "read"`. Off the critical
  path. Three consecutive failures (instantiate, trap, over-budget) disable
  that plugin's hooks for the process and say so through the same notice.
- **`plugin.notice`** (proto 0.69.0): Direct notification, humans only,
  `{ plugin_id, text }`, text masked and capped (`guest_reason`).
  `PluginInfo.hooks: Vec<String>` lists the events (additive, omitted when
  empty) so the extension manager shows what a hook listens to.
- **Both modes.** Daemon: broadcast to human connections. Embedded:
  `Backend::take_plugin_notices()` installs the dispatcher on the engine and
  returns the channel, like `take_degraded`.
- **Demo `plugins/rename-log`** (`org.norte.rename-log`): `after-renamed`,
  notifies "renamed N file(s)". Pure function with host tests.

## Tasks

1. `plugin-host`: WIT package + bindings + `HookInstance` + manifest
   vocabulary (`HookNotImplemented` → `HookUnknownEvent`). Tests: model.rs,
   wit_packages.rs, `cargo test -p norte-plugin-host --doc`.
2. `core`: journal tap (`Journal::set_hook_sender`, event on commit, unit
   test), `LazyJournal` forwards, `Engine::enable_hooks`, `hooks.rs`
   dispatcher + fuse (unit tests), `PluginRegistry::resolve_hooks`.
3. `proto` + daemon + SDK + backend: `PLUGIN_NOTICE`, `PluginNotice`,
   `PluginInfo.hooks`, 0.69.0, goldens, schema, window; daemon wiring;
   `RemoteBackend::take_plugin_notices`; `Backend::take_plugin_notices`.
4. Frontends: TUI (`app.message`), ui-host (`status.message` +
   `UiNotice::Message`, `backend_falso`), extension manager lists events.
5. Demo plugin + `just plugin-rename-log` + `plugins` + README + e2e
   (`hooks_rename_log_e2e.rs`: in-process and over the wire).
6. ADR 0100, `docs/plugins.md`, CHANGELOG, delete the proposal spec.
7. Reviews (`protocol-guardian`, `security-reviewer`, `rust-reviewer`) on
   the range, ONE fix commit, `just ci-fast`, `just ci`.
