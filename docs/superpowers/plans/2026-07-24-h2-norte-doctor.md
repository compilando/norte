# H2 — `norte doctor` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `norte doctor` — read-only diagnostics over config layers, keymaps, plugins, and connections; human report + `--json`; exit non-zero on errors, zero with warnings.

**Architecture:** New `crates/norte-cli/src/doctor.rs` module + `Cmd::Doctor { json: bool }` variant, early-returned before engine construction (like `audit_cmd`, main.rs:360-362). Pure check functions returning a shared `Finding { section, severity: Ok|Warn|Error, code, detail }` list; one renderer for human (Fluent `cli-doctor-*`) and one serde path for `--json` (the `ls --json` pattern, main.rs:1449-1453). Consumes C1 loaders — no re-implementation.

**Spec:** H2 in `docs/superpowers/specs/2026-07-23-help-config-system-design.md:128-143`.

**Verified facts:** CLI is fully Fluent (`t`/`ta`, `cli-*` keys in both ftl); exit convention = `anyhow::Result<ExitCode>`, audit_verify returns FAILURE-with-full-report; norte-cli must ADD deps `norte-config`, `norte-frontend`, `norte-connect` (PluginRegistry + config_dir already reachable via norte-core). `KeymapFile` fields private — validation happens by CALLING `build_for` per screen and collecting `KeymapError` (AmbiguousPrefix/UnknownCommand/etc. all surface there). `PluginInfo.approved` is already digest-effective; `plugin.wasm` presence NOT in `list()` — doctor stats the file directly (read-only diagnostic). `SecretResolver::resolve` fetches plaintext and may prompt on non-Linux keychains — NOT side-effect-free.

**Decisions locked:**
1. Keymap command vocabulary WITHOUT a norte-tui dep: build each screen (Browse/Viewer/Dialog) with vocabulary = the union of `run` names harvested from the SHARED PRESET's own bindings (via a successful permissive build's `bindings()` — see T2 mechanics) — layer bindings to names outside that union are WARN ("not bound by any bundled preset; may be frontend-specific"), not Error. Structural failures (parse, bare `keymap` in layer, BadChord, AmbiguousPrefix, EscInSequence) are Error. Honest approximation, documented in rustdoc + report footer.
2. Secrets: side-effect-free v1 = env-var presence ONLY. `norte-connect` exposes `pub fn env_key(conn: &str) -> String` (currently private, secret.rs:132-144 — tiny promotion + rustdoc + test). Doctor reports per connection: auth method, and for secret-bearing auth (Password/AccessKey): env var name + present/absent; keyring/age explicitly reported as "not probed (side-effect-free)". `--probe` (network) deferred — noted in report footer and spec.
3. Split-brain: if `NORTE_CONFIG_DIR` non-empty AND the legacy dir (computed via `user_config_dir_from` with a closure that omits NORTE_CONFIG_DIR) contains any of norte.toml/keymap.toml/connections.toml/policy.toml → WARN listing the legacy path.
4. Severity → exit: any Error → `ExitCode::FAILURE` (still printing the full report); else SUCCESS. Human lines: `[OK]/[AVISO]/[ERROR]`-style markers via Fluent keys; paths via `.display()` — config paths are the user's own (not hostile); plugin dirs come pre-redacted (basename) from PluginLoadError.

---

### Task 1: skeleton + config & keymap checks

**Files:** `crates/norte-cli/Cargo.toml` (+norte-config, +norte-frontend), `crates/norte-cli/src/doctor.rs` (new), `crates/norte-cli/src/main.rs` (Cmd::Doctor + early dispatch + `mod doctor;`), `crates/norte-i18n/i18n/{en,es}.ftl` (cli-doctor-* keys, file-end)

- [ ] TDD: doctor.rs unit tests FIRST against pure fns with tempdir layers:
  - `config_ok_y_toml_roto`: valid layers → all Ok findings; a broken norte.toml in one layer → Error finding carrying the culprit path.
  - `split_brain_avisa`: env closure injection — NORTE_CONFIG_DIR set + legacy dir with a norte.toml → Warn. (Checks take `&Layers` + an env getter closure, NEVER read process env directly except in the CLI wiring — injectable like norte-config's `_from` seams.)
  - `keymap_prefijo_ambiguo_es_error`: layer with an AmbiguousPrefix conflict → Error naming both sequences.
  - `keymap_comando_desconocido_es_aviso`: layer binding `run = "invented.command"` → Warn (decision 1).
- [ ] Implement: `Finding`/`Severity` (serde Serialize for --json), `check_config(layers, env) -> Vec<Finding>` (norte_config::load + per-file findings; split-brain), `check_keymaps(layers) -> Vec<Finding>` (per-screen: parse layers via `norte_frontend::config::load_keymap_layer`, harvest preset vocabulary — mechanics: `Effective::build_for_subset(preset, &[], &ALL_PRESET_RUNS?...)` — simplest harvesting: build the preset ALONE per screen with `build_for_subset` and an empty known set? subset SKIPS unknown → empty effective. Instead: parse each preset via `parse_keymap` and build with a two-pass trick: first `build_for_subset(preset, &[], &[], screen)` yields empty; NO. Correct mechanism: `Effective::build_for(preset, &[], KNOWN, screen)` needs KNOWN up front. Use the presets module: the vocabulary IS harvestable by building each preset per screen with `build_for_subset` against a wildcard? Not available. FINAL mechanics — add a tiny pub helper to norte-frontend keymap: `pub fn preset_commands(screen: Screen) -> Vec<String>` that parses the three bundled presets and returns the union of run names for screen∪global via a lenient internal walk (the crate CAN see RawSection privately — implement it inside keymap.rs with a unit test; ~15 lines). Doctor calls it, then `build_for(preset, layers, &vocab_union ∪ layer_runs?, screen)` — no: build with vocab = preset_commands(screen); UnknownCommand errors from LAYERS get downgraded to Warn findings by matching the KeymapError variant. Structural errors stay Error.)
- [ ] Cmd::Doctor { #[arg(long)] json: bool } + handler `doctor_cmd(json) -> anyhow::Result<ExitCode>` early-returned in run() — runs check_config+check_keymaps (T2 adds more), renders. Fluent keys: `cli-doctor-title`, `cli-doctor-section-{config,keymap,plugins,connections}`, `cli-doctor-ok/warn/error` markers, per-code detail keys as needed (keep codes few: config-parse, config-split-brain, keymap-structural, keymap-unknown-cmd, …).
- [ ] Verify: `cargo nextest run -p norte-cli -p norte-frontend`, clippy (REAL exit codes), fmt, i18n parity test. Commit `feat(cli): norte doctor — config & keymap checks (H2)`.

### Task 2: plugin & connection checks + `--json` + gate

**Files:** `crates/norte-cli/src/doctor.rs`, `crates/norte-connect/src/secret.rs` (env_key promotion), `crates/norte-cli/src/main.rs`, ftl files

- [ ] TDD: `plugins_digest_y_wasm` (tempdir config_dir with a plugin dir: manifest ok + missing plugin.wasm → Warn "no binary"; catalog error → Error; approved-but-digest-stale surfaces as approved=false → Warn "re-approval required" — build fixtures via the manifest TOML shapes from norte-core plugins.rs test consts); `conexiones_y_secretos` (connections.toml with a Password-auth conn, env var absent → Warn naming NORTE_SECRET_X; present via injected env closure → Ok; broken connections.toml → Error).
- [ ] `norte-connect`: promote `env_key` to `pub` with rustdoc + doctest (naming convention documented; unit test exists? add one pinning `mi-server.1 → NORTE_SECRET_MI_SERVER_1`). Doctor takes the env getter closure for testability.
- [ ] `check_plugins(config_dir) -> Vec<Finding>`: `PluginRegistry::discover` (or `empty` on state corruption → Error finding), `list()` → per-plugin findings (enabled/approved states; approved==false + state file had approved=true → Warn digest-stale is NOT distinguishable via public API — check state_snapshot() vs list(): if snapshot says approved but list() effective says not → digest-stale Warn; report the mechanics you use); `plugin.wasm` existence via `config_dir/plugins/<id>/plugin.wasm` metadata → Warn if absent.
- [ ] `check_connections(config_dir, env) -> Vec<Finding>`: `ConnectionsFile::load` (absent = Ok-empty; broken = Error), per conn: `endpoint()` parse (Error on invalid), auth classification, env-secret presence (decision 2), `logical_trash`/tls notes as Ok-info lines. Keyring/age "not probed" footer.
- [ ] `--json`: `serde_json::to_writer_pretty` of `{ findings: [...], summary: { errors, warnings } }` (stable field names — this is a tool-consumable surface; add a doc note it's NOT the wire protocol, no proto guardian needed, but keep names stable).
- [ ] Human render: sections in fixed order, findings with markers, summary line, exit per decision 4. Manual smoke: run `cargo run -p norte-cli -- doctor` against the real user config (read-only!) and against a scratch NORTE_CONFIG_DIR with seeded breakage — paste output in report.
- [ ] Gate: `just ci` (real exits), rust-reviewer on the H2 range (pure-fn injectability, no side effects — ESPECIALLY no keyring/network touch, exit-code honesty, Fluent coverage) — apply findings. Update CHANGELOG (doctor entry). Memory update.
- [ ] Commit `feat(cli,connect): doctor plugin/connection checks + --json (H2)`.

## Self-review notes
- Spec H2 coverage: config layers+parse+split-brain ✓, keymap conflicts/unknown-commands ✓ (vocabulary approximation documented), plugins discovery/digest/wasm ✓, connections parse+secret-presence ✓ (probe deferred, documented), human+--json ✓, exit codes ✓ (audit_verify pattern). All checks injectable (env closures + explicit dirs) — no process-env reads inside check fns.
- The one new pub API outside the CLI: `norte_frontend::keymap::preset_commands(screen)` + `norte_connect::env_key` — both tiny, tested, rustdoc'd.
