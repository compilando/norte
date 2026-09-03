# 0095 — A renamer plugin proposes, and the core renames

- Status: accepted
- Date: 2026-09-04
- Decision makers: Oscar González
- Related: ADR 0022 (manifest and sandbox), ADR 0041 decision 4 (one WIT
  package per concern), ADR 0057 (the location token), ADR 0077 (a decision
  taken once, in `norte-frontend`), ADR 0094 (one served version per WIT
  package), #310 (batch rename), spec §7.1 C3 ("renamer plugins, needs B1")

## Context

Batch rename (#310) gave norte one pipeline for renaming many entries at
once: a plan of `{ from, to }` pairs, a review screen in each frontend, a
`plan_hash` the core checks before executing, `fs.rename_batch` through the
journal, and undo. Two producers feed it today: the template engine and the
AI plan (`ai.rename_plan`). Both frontends already paint the review and
harvest the result the same way.

The plugin spec asked for a third producer: a plugin that renames "by EXIF
date", "by ID3 title", or by anything a third party can compute. The
question was what the plugin is allowed to do, and where its plan goes.

## Decision

1. **A renamer plugin proposes; it never renames.** Its only export is
   `plan(id, location, names) -> result<list<proposal>, string>`, where a
   proposal is `{ current, proposed }`. The plugin sees names, not paths,
   and gets the files only through the location token of ADR 0057 when its
   manifest declares `location = "read"`. Executing the plan is the batch
   rename pipeline's job, with every check it already makes: legal segment,
   no collision, journal entry, undo path. A plugin cannot skip the review
   any more than the AI can.

2. **The plan is the AI plan.** `plugin.rename_plan` answers the existing
   `AiRenamePlanResult`; the core drops identity pairs and proposals for
   names it did not ask about, and hands the rest to the same result type.
   Both frontends route it into the harvest they have for `ai.rename_plan`
   — the TUI's in-flight AI run, the window's background plan message —
   and paint nothing new. One review screen, three producers; a bug fixed in
   the review is fixed for all of them (ADR 0077).

3. **Its own WIT package, `norte:renamer@0.1.0`.** Adding an interface to
   `norte:plugin` would have bumped it and, under ADR 0094, invalidated
   every installed previewer for a kind none of them uses. The world
   `norte-renamer` imports `host-log`, `host-config` and
   `norte:location/location@0.2.0`, and exports `renamer`.

4. **A renamer is listed where commands are.** `PluginInfo.commands`
   carries it with `PluginCommandInfo.kind = "renamer"`, a field omitted
   when it is the default `command`. The palette shows it under `[rename]`
   instead of `[plugin]`, keyed `renamer:{plugin}:{id}`, and each frontend
   routes that key to the plan request. A 0.66 client sees a command it
   cannot run, which is the honest degradation: the plugin exists, the
   client is old.

5. **The daemon gate is the read gate on `dir`.** A plan mutates nothing;
   it reads under the directory of the names, so any actor allowed to read
   there may ask for one. Executing it goes through `fs.rename_batch` and
   the policy as always. `climb` past the directory is allowed only for a
   human, as for columns.

6. **Caps and errors.** `names` has the same cap as `ai.rename_plan`; a
   plan has at most 10 000 proposals and the byte budget every plugin
   return has; beyond either the run fails, it does not truncate. On the
   wire: `NotFound` when the plugin or renamer is not consented,
   `Unsupported` when the guest refuses, `Io` when it does not run. The
   guest's refusal sentence does NOT cross: error reasons are a closed
   vocabulary and the sentence is third-party text, so it goes to the
   daemon log, which both frontends show. That is a known gap, not a
   contract: #332 asks for an additive reason channel on a dedicated
   result, never on `Error`.

7. **A renamer is not runnable.** `plugin.run_command` on any plugin whose
   world does not export `command` (decorator, columns, provider, renamer)
   answers `INVALID_PARAMS` before instantiating anything. It is what makes
   the 0.66 story in decision 4 true: the old client shows an error, and
   the daemon does not pay a wasm instantiation per click to find out.

## Consequences

- A third party can ship a way of renaming without touching the core, and
  the human keeps the last word on every name.
- The demo, `org.norte.date-prefix`, is the pattern: a pure function with
  host tests, `stat` under the token, a refusal instead of a guess when
  there is no location, and idempotence (already-dated names are left
  alone).
- The plan shape being the AI's means a renamer inherits the AI's limits:
  names are UTF-8 strings on the wire, so non-UTF-8 entries are filtered
  out before the call, as the template path already does.
- A renamer that needs file content beyond `read-prefix` (a full hash, a
  decoded image) is not served by this package's imports; that is a later
  bump of `norte:location`, not of `norte:renamer`.
