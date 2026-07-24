# P1 — plugin descriptions & commands on the wire Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Plugins stop being mute: optional manifest `description` (cosmetic, OUTSIDE the approval digest), `PluginInfo.description` + `PluginInfo.commands` on the wire (ONE protocol bump), surfaced in the TUI extension manager and as palette rows.

**Spec:** P1 in `docs/superpowers/specs/2026-07-23-help-config-system-design.md` (§Phase P1). Protocol change → **protocol-guardian review MANDATORY**; plugin-supplied text is UNTRUSTED everywhere it renders → encoding-auditor.

**Known facts:** proto currently 0.25.0 (methods.rs:128 area — re-verify); `PluginInfo` at norte-proto methods.rs:946-966 (id/name/publisher/version/category/capabilities/approved/enabled); goldens in the methods.json wire-freeze corpus (protocol-guardian's requirement — grep tests for methods.json); manifest `PluginSection` (norte-plugin-host manifest.rs:161-169, `deny_unknown_fields`) has NO description; approval digest = sha256 over category+contributions+capabilities ONLY (manifest.rs:296-307 — name/publisher/version cosmetic precedent); `CommandContrib { id, title }` exists (manifest.rs:61-68); `PluginRegistry::list()` builds PluginInfo (norte-core plugins.rs:189-208); TUI extension manager renders via `plugin_line` (ui.rs, name/version/badges masked by display_name); palette rows built by `norte-tui/src/palette.rs::build_rows` (trusted consts today; chord column already masked via mask_terminal_hazards after H1).

**Decisions locked:**
1. One bump: 0.25.0 → 0.26.0. `PluginInfo` gains `#[serde(skip_serializing_if = "Option::is_none", default)] description: Option<String>` and `#[serde(default)] commands: Vec<PluginCommandInfo>` (`PluginCommandInfo { id: String, title: String }`) — both tolerant for N-1 peers. Goldens: update existing plugin.list golden + add one WITH description+commands. Version-window doc (N-1 shift) per house convention.
2. Manifest: `description: Option<String>` in `[plugin]` (cap 280 chars at PARSE — longer = ManifestError, fail-loud like id validation; rustdoc says cosmetic). EXCLUDED from `approval_digest` — editing description must NOT reset approval (test pins digest equality across description edits).
3. Palette plugin rows: label `plugin:{id}:{command-id}`, display title + description, ALL masked (display_name/mask_terminal_hazards — same belt as chords). Enter dispatches via the existing backend `plugin_run_command(id, command, "")` path (async like other dispatches). Rows come from `backend.plugins_list()` fetched at PALETTE OPEN (not keymap-build: plugin state changes at runtime via F12) — open becomes async like "app.extensions" arm; only APPROVED+ENABLED plugins contribute rows.
4. Extension manager: description as a second line under the plugin row (masked + middle_ellipsis).
5. GUI: no palette/manager yet — out of scope, noted (G3 picks it up).

---

### Task 1: proto bump + goldens (protocol-guardian)

**Files:** `crates/norte-proto/src/methods.rs`, goldens corpus (locate via `rg -l "plugin.list" crates/norte-proto/tests`), version consts/docs.

- [ ] TDD: golden round-trip test for PluginInfo WITH description+commands FIRST (fails: fields missing). Then: `PluginCommandInfo` (rustdoc + doctest per proto crate rules), the two fields (serde attrs per decision 1), version bump 0.25.0→0.26.0 + history doc line, N-1 window note, goldens updated/added (old golden WITHOUT the fields must still deserialize — tolerance test).
- [ ] `cargo nextest run -p norte-proto` + clippy + fmt (real exits).
- [ ] Commit `feat(proto): 0.26.0 — PluginInfo description + commands (P1)`.
- [ ] **Dispatch protocol-guardian on the diff; apply findings before proceeding.**

### Task 2: manifest + registry

**Files:** `crates/norte-plugin-host/src/manifest.rs`, `crates/norte-core/src/plugins.rs`.

- [ ] TDD: manifest parses `description` (cap 280: 281 chars → error; absent → None); digest UNCHANGED by description edit (pin vs a manifest without it); registry `list()` populates `PluginInfo.description` + `commands` from `Contributions.command` (only for valid plugins; order = manifest order).
- [ ] Implement; verify norte-plugin-host + norte-core suites + daemon plugin.list handler compiles (it serializes PluginInfo — tolerant additions, no handler change expected; confirm).
- [ ] Commit `feat(plugin-host,core): manifest description (out of digest) + wire population (P1)`.

### Task 3: TUI surfacing + gate

**Files:** `crates/norte-tui/src/{app.rs,ui.rs,palette.rs,main.rs}`, ftl (palette section header key), CHANGELOG.

- [ ] Extension manager: description second line (masked, middle_ellipsis, dim style) — snapshot with a hostile description (bidi) from the corpus.
- [ ] Palette: `"app.palette"` arm becomes async-open like extensions (fetch plugins_list; approved+enabled only; rows appended after built-in commands under the same list — row label `plugin:{id}:{cmd}` + masked title/description; chord column "—"). Enter on a plugin row → `backend.plugin_run_command(id, cmd, "")` with the status-bar message result (mirror how CLI/TUI handle run_command output — check existing plumbing; if the TUI lacks a run_command backend call, add it to the Backend trait mirroring plugins_list). Unit tests + snapshot with one hostile plugin row.
- [ ] Encoding-auditor on the new surfaces (description in manager+palette, plugin titles) + apply findings.
- [ ] `just ci` (real exit), CHANGELOG entry, memory update.
- [ ] Commit `feat(tui): plugin descriptions in extension manager + plugin rows in palette (P1)`.

## Self-review notes
- Spec P1 coverage: manifest description outside digest ✓, one proto bump with both fields ✓, manager+palette surfacing ✓, H1's palette-plugin slot filled ✓, GUI deferred to G3 (spec'd). Untrusted-text discipline on every new render. Guardian gate is a hard step inside T1, not an afterthought.
