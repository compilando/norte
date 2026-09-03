# C3 — `renamer` plugin category

**Goal:** a plugin PROPOSES rename pairs for the marked entries; the core
checks and executes them through the batch-rename pipeline that the AI plan
and the template already use (review, `plan_hash`, journal, undo). Both
frontends receive the plan through the code they already have for the AI
plan: nothing new to paint.

**Spec:** `docs/superpowers/specs/2026-08-07-plugins-and-delivery-plan.md`
(C3, "needs B1"). B1 is done (#310).

## Decisions

- **Its own WIT package, `norte:renamer@0.1.0`** (`wit/deps/renamer/`),
  like `norte:location`: adding an interface to `norte:plugin` would bump
  it and invalidate every installed previewer for a kind they do not use
  (ADR 0094). World `norte-renamer` imports `host-log`, `host-config` and
  `norte:location/location@0.2.0` (a renamer that reads headers — EXIF,
  ID3, mtime — needs the token), exports `renamer`.
- `renamer.plan(id: string, location: option<location-ref>, names:
  list<string>) -> result<list<proposal>, string>` where `proposal` is
  `{ current, proposed }` (`from` is a WIT keyword). Names are UTF-8
  strings because a plan pair travels as text on the wire
  (`AiRenameEntry`); the host filters non-UTF-8 names out before calling,
  as the template path does. The guest returns only the pairs it wants
  changed; the host drops identity pairs and proposals for names it did
  not ask about, caps the count at 10 000, and the batch-rename pipeline
  validates every `to` as a legal segment when the plan is executed.
- **Manifest**: `category = "renamer"` (digest tag 6, appended),
  `[[contributions.renamer]] id, title`. A plugin may offer several
  renamers ("by EXIF date", "by ID3 title").
- **Wire** (proto 0.67.0): `plugin.rename_plan` with
  `PluginRenamePlanParams { plugin_id, renamer_id, dir, names }` and the
  EXISTING `AiRenamePlanResult` as result — same shape, different
  producer, which is the whole point. A renamer is listed in
  `PluginInfo.commands` with `PluginCommandInfo.kind = "renamer"`
  (additive, omitted when `command`), so the one list the palette already
  reads carries both and a 0.66 client shows a renamer as a command it
  cannot run rather than not at all.
- **Core**: `resolve_renamer(plugin_id, renamer_id)`, `run_rename_plan`
  with the same location minting as columns (read gate on `dir`, `climb`
  only for a human), `Backend::plugin_rename_plan` embedded + remote,
  daemon handler open to any actor that passes the read gate (a plan
  mutates nothing; executing it goes through `fs.rename_batch` and the
  policy as always).
- **Frontends**: palette rows `renamer:{plugin}:{id}` next to the plugin
  command rows; on Enter each frontend calls `plugin_rename_plan` with
  the marked names (or the cursor's) and hands the result to its AI-plan
  harvest. The TUI's `AiRenameRun`, the window's `Fondo::PlanIa`.
- **Demo**: `plugins/date-prefix` — `YYYY-MM-DD_name` from the file's
  mtime, read through `location.stat`. Small, useful, exercises the
  token. Refuses (with a sentence) when it gets no location instead of
  guessing dates; leaves already-dated names alone so a second run is a
  no-op.

## Done (2026-09-04)

All of the above, end to end: WIT package, host bindings and instance,
manifest/catalogue/digest, proto 0.67.0 with goldens and schema, core
resolver and runner, embedded and remote backends, daemon handler, palette
rows in `norte-frontend`, TUI and window routing into their AI-plan review,
the demo plugin and its core e2e. ADR 0095 records the decision.

## Not in this batch

- Hooks (A2 of the plan: declarable, deliberately not runnable).
- A renamer that needs the file CONTENT beyond `read-prefix`.
