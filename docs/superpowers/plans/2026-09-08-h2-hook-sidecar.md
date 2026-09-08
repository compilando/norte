# H2 — a hook writes a sidecar (ADR 0101): implementation plan

Follows `2026-09-08-h1-operation-hooks.md`. Direct work on `main`, one
commit per task, ONE `just ci-fast`, ONE `just ci`.

Assumptions taken where ADR 0101 left questions open (a line each to flip):
`effect-denied` reaches the human once per plugin per process, then the log;
a policy `ask` for a plugin actor is a `deny` — a hook runs unattended.

## Tasks

1. `plugin-host`: `norte:hook@0.2.0` (`write-sidecar { seq, name, content,
   if-exists }`), `FsWriteCap` (`fs-write = { sidecar = [names] }`;
   `"scoped"` rejected, ADR 0088; hooks only; names validated), badges
   `fs-write:<name>`, `MAX_SIDECAR_BYTES` / `MAX_SIDECAR_EFFECTS`.
2. `core`: `Engine::write_file_as(path, bytes, on_exists, actor)` →
   `ops::write_task` (create with content; replace = trash then create;
   refuse = `Conflict`); plugin `Ask` → deny; dispatcher applies sidecar
   effects through the engine as `Actor::Plugin` with a transient scope.
3. `proto` 0.70.0: `PluginNotice.kind` gains `effect-denied`; goldens,
   schema, window; i18n.
4. `plugins/rename-log` writes `.norte-renames.log` (`replace`, reading the
   previous content under the location token); e2e: the file appears after
   a move through the daemon; a `deny` rule stops it and says so.
5. ADR 0101 → accepted with the answers; guide; CHANGELOG.
6. Reviews (`security-reviewer`, `rust-reviewer`, `protocol-guardian`), ONE
   fix commit, gate.
