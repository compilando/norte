# P2 — declarative per-plugin configuration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Plugins declare typed settings in the manifest (`[config]`, INSIDE the approval digest); users set values in `config_dir/plugins/<id>/config.toml`; the host validates fail-closed and delivers via a new optional WIT host interface; the extension manager displays them.

**Spec:** P2 in `docs/superpowers/specs/2026-07-23-help-config-system-design.md`. Security-reviewer on digest+delivery; encoding-auditor on value rendering; protocol-guardian NOT needed (no norte-proto change — the wire `PluginInfo` is untouched in P2; display uses host-side data. If that changes, STOP and re-plan).

**Facts:** digest canonical form at manifest.rs:296-307 (category+contributions+capabilities, domain-prefixed `norte-plugin-manifest:v1`); WIT package `norte:plugin@0.4.0` (crates/norte-plugin-host/wit/norte-plugin.wit; worlds `norte-plugin` imports host-log, exports previewer+command; `norte-provider` for providers); runtime instantiation in runtime.rs (empty WasiCtx, host functions linked); component-model fact: a host offering EXTRA interfaces old guests don't import is link-compatible — additive. Registry discover/catalog error surfacing: `Catalog.errors: Vec<LoadError>` (fail-closed per-dir), `PluginLoadError { dir(basename), reason }` crosses the wire.

**Decisions locked:**
1. Schema: `[config.<key>] = { type = "string"|"bool"|"int"|"enum", default = <typed>, description?, min?/max? (int only), values? (enum only, ≤16 entries) }`. Caps: ≤32 keys/plugin; key charset `[a-z0-9-]{1,32}`; description ≤280; enum values/string default ≤280 chars. Defaults MUST validate against their own type/range at parse (fail-loud ManifestError).
2. Digest: `[config]` schema IS included (behavior-affecting). Canonical form: append a `config:` section ONLY when present — a manifest WITHOUT `[config]` digests byte-identical to pre-P2 (pin against a fixed known digest or via old-manifest equality test; existing approvals never reset).
3. Values file: `config_dir/plugins/<id>/config.toml`, FLAT `key = value` (TOML types must match schema type; int range checked; enum membership checked). Validation at discover: any violation → the plugin becomes a `PluginLoadError`-style catalog error (fail-closed, visible in manager + doctor) and is NOT loaded. Absent file = all defaults.
4. Delivery: new WIT interface `host-config` (`get: func(key: string) -> option<string>`; `all: func() -> list<tuple<string, string>>`) in package bump `norte:plugin@0.5.0`; values canonically string-encoded (bool: "true"/"false"; int: decimal; string/enum: raw). Host links it for EVERY plugin (validated map, defaults filled); old guests simply don't import it. Provider world gets the same import.
5. Display: extension manager shows, under the description line, one dim line per key: `key = value` (values are the USER's own but masked anyway — belt), from schema+values resolved at list() time → needs host-side accessor, NOT wire: `PluginRegistry::settings_of(id) -> Vec<(String,String)>` consumed by the TUI via... TUI talks Backend/wire! Extension manager data comes from plugin.list (wire). Settings display therefore DAEMON-mode-incapable without a wire change — DECISION: display only in EMBEDDED mode? NO — honest scope cut: P2 ships host-side plumbing + doctor surfacing (doctor runs host-side and CAN show settings); the extension-manager display is DEFERRED to the wire bump that G3 already requires (note in spec + CHANGELOG). Doctor gains `plugin-config` findings (per-plugin: key=value lines as Ok/info; violations already surface as catalog errors).

---

### Task 1: manifest `[config]` schema + digest

**Files:** `crates/norte-plugin-host/src/manifest.rs`, tests/model.rs.

- [ ] TDD: parse happy path (all 4 types); each cap violation (33 keys, bad charset, 281-desc, default-out-of-range, enum default not in values, 17 enum values) → typed ManifestError; **digest pins**: manifest WITHOUT config → digest EQUAL to pre-P2 value (compute against a literal known-good digest captured from current code — capture it in the test BEFORE implementing); manifest WITH config → digest differs; two manifests differing only in a default value → digests differ (defaults are behavior).
- [ ] Implement: `ConfigKeySpec` enum-per-type or struct+validated kind (mirror Capabilities' style), `Manifest.config: BTreeMap<String, ConfigKeySpec>` (BTreeMap = deterministic digest order), canonical digest extension per decision 2.
- [ ] Verify + commit `feat(plugin-host): [config] manifest schema inside the approval digest (P2)`.

### Task 2: values load + validation + doctor

**Files:** `crates/norte-plugin-host/src/catalog.rs` (or new config_values.rs), `crates/norte-core/src/plugins.rs`, `crates/norte-cli/src/doctor.rs`.

- [ ] TDD: absent config.toml → defaults map; valid values override; unknown key / wrong TOML type / out-of-range int / non-member enum → catalog error naming the KEY (never the value — #73), plugin excluded (fail-closed); values canonical string encoding per decision 4.
- [ ] Implement `resolve_settings(manifest, dir) -> Result<BTreeMap<String,String>, ConfigValueError>` in norte-plugin-host; wire into Catalog::load_dir (validation failure → LoadError entry); registry exposes `settings_of(&self, id) -> Option<&BTreeMap<String,String>>`.
- [ ] Doctor: check_plugins gains per-plugin Ok/info findings `plugin-config` (key=value, masked value, capped) when settings exist; violations arrive via existing error path (verify with a fixture).
- [ ] Verify + commit `feat(plugin-host,core,cli): plugin config values — validated, fail-closed, doctor-visible (P2)`.

### Task 3: WIT `host-config` + runtime + guest e2e

**Files:** `crates/norte-plugin-host/wit/norte-plugin.wit` (0.4.0→0.5.0), runtime.rs, examples-wasm guest, e2e test.

- [ ] WIT: `interface host-config { get: func(key: string) -> option<string>; all: func() -> list<tuple<string, string>>; }`; both worlds import it. Package bump comment documents additive-link compatibility for old artifacts.
- [ ] runtime.rs: store the validated settings map in the per-instance state; link the two functions (read-only lookups; NO fs, NO env — comment the sandbox invariant holds). Old-guest compatibility test: existing e2e guests (compiled against 0.4.0) still instantiate+run (the suite's existing SKIP-without-target pattern).
- [ ] Guest demo: extend one example (command-demo) to read a setting via host-config and echo it; e2e `plugins_config_e2e.rs` (SKIP sin wasm32-wasip2): manifest with [config] + config.toml value → run_command output reflects the value; default when file absent.
- [ ] Verify (incl. building the wasm examples if the target exists — follow the existing e2e build convention) + commit `feat(plugin-host): host-config WIT 0.5.0 — validated settings delivered to guests (P2)`.

### Task 4: gate

- [ ] security-reviewer on the P2 range (digest canonicalization — collision/format-injection between sections; config.toml as attack surface: hostile values reaching guests are FINE [guest's own config] but must never influence HOST paths; fail-closed exclusion honesty; WIT functions' read-only guarantee). encoding-auditor quick pass (doctor value rendering masked+capped; key charset enforced). Apply findings.
- [ ] `just ci` real exit; CHANGELOG (note the manager-display deferral to G3); spec edit: P2 section gains the deferral note; memory update.
- [ ] Commit closure.

## Self-review notes
- Spec P2 coverage: schema in digest ✓, values file validated fail-closed ✓, WIT delivery à la provider-config ✓ (pull-model host-config, additive), manager display — DEFERRED to G3's wire bump with spec note (decision 5 records why: settings live host-side; the TUI manager is wire-fed; no proto bump in P2 by design). Doctor picks up the visibility slack host-side.
- No norte-proto change anywhere; if any task discovers one is needed → STOP, guardian, re-plan.
