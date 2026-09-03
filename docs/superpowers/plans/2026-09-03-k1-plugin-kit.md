# K1 — the plugin kit: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a third party can write, build, install and diagnose a norte plugin
from the documentation alone, and a plugin built against a WIT package the
host no longer serves is listed as broken with the reason instead of dying in
wasmtime.

**Architecture:** the host learns which `norte:*` packages a component imports
by reading its import section with `wasmparser` (no compile), compares them
with the one version of each package it serves, and routes a mismatch to the
catalogue's `errors` like any other unloadable plugin. The kit is a template
guest built by the gate, an author guide, and two `just` recipes.

**Tech Stack:** wasmparser 0.251 (already in the tree under wasmtime),
wit-bindgen 0.46 guests on `wasm32-wasip2`, just.

**Spec:** `docs/superpowers/specs/2026-09-03-plugin-kit-and-demo-plugins-design.md`, section K1.

## Global Constraints

- Public documentation in English (`docs/README.md` conventions).
- Guests are not workspace members; each has `[workspace]`, own lockfile,
  `wit` symlink to `crates/norte-plugin-host/wit`.
- `just t <crate>` in the loop; one `just ci-fast` for the plan; `just ci`
  at the close. Reviewers never compile.
- New dependency (`wasmparser`) justified in the commit: same version as the
  one wasmtime already pulls, zero new code in the tree.

## Gate budget

`just t norte-plugin-host` / `just t norte-core` / `just t norte-cli` during
RED→GREEN. ONE `just ci-fast` after Task 5. ONE `just ci` at the end.

---

### Task 1: the host knows what it serves, and can read what a guest imports

**Files:**
- Modify: `crates/norte-plugin-host/Cargo.toml` (add `wasmparser.workspace = true`), `Cargo.toml` (workspace dep `wasmparser = "0.251"`)
- Create: `crates/norte-plugin-host/src/wit_imports.rs`
- Modify: `crates/norte-plugin-host/src/lib.rs` (module + re-exports)
- Test: `crates/norte-plugin-host/tests/wit_packages.rs`

**Produces:**
```rust
/// The one version of each `norte:*` package this host serves.
pub const SERVED_WIT: &[(&str, &str)] = &[
    ("norte:host", "0.1.0"),
    ("norte:plugin", "0.8.0"),
    ("norte:provider", "0.1.0"),
    ("norte:location", "0.1.0"),
];
/// `(package, version)` pairs among a component's imports, `norte:*` only,
/// sorted and deduplicated. A core module (not a component) or a component
/// with no norte imports yields an empty vector.
pub fn wit_imports(bytes: &[u8]) -> Vec<(String, String)>;
/// The first import whose package the host serves at a different version.
pub fn wit_mismatch(imports: &[(String, String)]) -> Option<WitMismatch>;
pub struct WitMismatch { pub package: String, pub built_against: String, pub served: String }
```

- [ ] **Step 1: failing tests** in `wit_packages.rs`:
  - `served_wit_matches_the_package_files`: parse every `package norte:<n>@<v>;`
    line under `crates/norte-plugin-host/wit/**/*.wit` and assert the set equals
    `SERVED_WIT`.
  - `wit_imports_of_a_real_guest_name_the_served_versions`: build
    `previewer-demo` with the existing `build_guest` helper of `tests/runtime.rs`
    (move it to `tests/support/mod.rs` if not shared yet), read the bytes, assert
    `wit_imports` contains `("norte:plugin","0.8.0")` and `("norte:host","0.1.0")`
    and `wit_mismatch` is `None`.
  - `a_guest_built_against_another_version_is_a_mismatch`: take those bytes,
    replace every `b"@0.8.0"` with `b"@0.1.0"` (same length, so sections stay
    valid), assert `wit_mismatch` is `Some` with `built_against == "0.1.0"` and
    `served == "0.8.0"` for `norte:plugin`.
  - `bytes_that_are_not_a_component_have_no_imports`: `wit_imports(b"\0asm\x01\0\0\0")`
    is empty and `wit_imports(b"garbage")` is empty (never panics, never errors).
- [ ] **Step 2:** `just t norte-plugin-host` — RED (unresolved names).
- [ ] **Step 3:** implement `wit_imports` with `wasmparser::Parser::new(0).parse_all(bytes)`,
  collecting `Payload::ComponentImportSection` entries' `import.name.0`, keeping
  names of the form `norte:<pkg>/<iface>@<ver>`; a parse error stops the walk
  and returns what was collected.
- [ ] **Step 4:** GREEN. `cargo test -p norte-plugin-host --doc` (doctests on the two pub fns).
- [ ] **Step 5:** commit `feat(plugin-host): the host reads which WIT a guest was built against`.

### Task 2: a mismatched guest is listed as broken, with the reason

**Files:**
- Modify: `crates/norte-plugin-host/src/catalog.rs` (`PluginEntry.wit`, `load_dir`), `src/manifest.rs` (`ManifestError::WitMismatch`)
- Modify: `crates/norte-core/src/plugins.rs` (`PluginRegistry::load_errors()` accessor)
- Modify: `crates/norte-cli/src/doctor.rs` (`plugin-wit-mismatch` finding), Fluent `cli-doctor-detail-plugin-wit-mismatch` en/es
- Test: `crates/norte-plugin-host/tests/model.rs`, `crates/norte-cli/src/doctor.rs` tests

**Produces:** `PluginEntry { …, pub wit: Vec<(String, String)> }`;
`ManifestError::WitMismatch { package, built_against, served }` (Display:
"built against `norte:plugin@0.1.0`, this norte serves `@0.8.0`: rebuild the plugin");
`PluginRegistry::load_errors(&self) -> &[norte_plugin_host::LoadError]`.

- [ ] **Step 1: failing tests.**
  - `model.rs`: `un_guest_compilado_contra_otro_wit_se_lista_roto`: write a plugin dir
    with a valid manifest and a `plugin.wasm` whose bytes are the rewritten
    demo guest from Task 1 (build it again via the shared helper; SKIP without
    the target). `Catalog::load_dir` puts it in `errors` with `WitMismatch`, and
    `plugins` is empty. And with the intact bytes it is in `plugins` with
    `wit` naming `norte:plugin@0.8.0`.
  - `doctor.rs`: `un_plugin_de_otro_wit_es_un_hallazgo_propio`: same setup through
    `check_plugins`; finding `plugin-wit-mismatch`, `Severity::Warn`, detail =
    dir basename (masked). The generic `plugin-manifest-broken` is NOT emitted
    for it.
- [ ] **Step 2:** RED.
- [ ] **Step 3:** in `load_dir`, read the wasm bytes ONCE (replace `wasm_digest(&dir)`
  with a `read_wasm(&dir) -> Option<Vec<u8>>` and derive digest + imports from
  it); on `wit_mismatch(..)` push `LoadError { dir, error: ManifestError::WitMismatch{..} }`
  instead of the entry. Doctor: iterate `registry.load_errors()`, match the
  variant, emit the specific code; everything else keeps `plugin-manifest-broken`.
- [ ] **Step 4:** GREEN: `just t norte-plugin-host`, `just t norte-core`, `just t norte-cli`.
- [ ] **Step 5:** commit `feat(plugin-host,cli): a plugin built against another WIT is listed as broken, with the reason`.

### Task 3: `just plugin-git-status` and `just plugins`

**Files:** `justfile` (next to `plugin-syntect`), `plugins/README.md` (create).

- [ ] **Step 1:** add `plugin-git-status` (build `plugins/git-status` for
  `wasm32-wasip2`, stage under `target/plugin-stage/git-status/`, copy
  `plugin.toml`, `help.md` if present, `plugin.wasm`, run `norte plugin install "$stage" "$@"`).
- [ ] **Step 2:** add `plugins` that runs `plugin-syntect` and `plugin-git-status`
  in sequence, passing `"$@"` through (so `just plugins --force` replaces).
- [ ] **Step 3:** `plugins/README.md`: what lives here (official plugins,
  outside the workspace), one line per plugin, `just plugins`.
- [ ] **Step 4:** run `just plugin-git-status` on this machine; `norte plugin list`
  shows `org.norte.git-status` unapproved.
- [ ] **Step 5:** commit `build(plugins): install git-status and all official plugins with just`.

### Task 4: the template guest, built by the gate

**Files:**
- Create: `plugins/template/{Cargo.toml, plugin.toml, help.md, README.md, src/lib.rs, .gitignore, wit -> ../../crates/norte-plugin-host/wit}`
- Create: `crates/norte-core/tests/plugin_template_e2e.rs`

The guest exports the `norte-plugin` world: `render` returns
`"<mimetype>, <n> bytes"` plus the first line of the content, `render-styled`
wraps that in one span per line, `run("hello", arg)` returns `"hello, <arg>"`
and any other id an error. Manifest: id `org.example.template`, category
`previewer`, `previewer = [{ mimetypes = ["text/plain"] }]`, one command
`hello`, `fs-read = "scoped"`, `[config.greeting]` string default `"hello"`
used by `run`. Every field commented for the reader. README: "copy this
directory, rename the id, replace `src/lib.rs`, keep the symlink".

- [ ] **Step 1: failing E2E** (pattern: `columns_git_e2e.rs` builds a guest under
  `plugins/`): build `plugins/template`, stage, `install`, approve+enable through
  `PluginRegistry`, `run_command("org.example.template","hello","norte")` returns
  `"hello, norte"`; `resolve_previewer("text/plain")` finds it; `plugin.set_config`
  path: write `config.toml` with `greeting = "hola"` and rediscover → `"hola, norte"`.
- [ ] **Step 2:** RED (guest missing). **Step 3:** write the guest. **Step 4:** GREEN.
- [ ] **Step 5:** commit `feat(plugins): a template guest, built and installed by the gate`.

### Task 5: ADR 0094, the guide, and the links

**Files:**
- Create: `docs/adr/0094-a-plugin-says-which-wit-it-was-built-against.md`, `docs/plugins.md`
- Modify: `docs/adr/README.md`, `docs/README.md`, `README.md` (Plugins section links the guide), `CHANGELOG.md`

ADR 0094 records: the fact (version in every import name; any bump breaks a
compiled guest), the mechanism (imports read from the binary, mismatch listed
not loaded, state untouched, recompiled binary needs re-approval by #241), the
policy (one served version per package, no window; `norte:host` moves rarely;
minor bump on any change; the CHANGELOG names the bump and the release note
line), and the alternatives (a manifest field: self-declared and stale; a
compatibility window: every old world linked forever; do nothing: the
wasmtime error).

`docs/plugins.md` sections, in order: What a plugin is · Kinds and what each
contributes · The manifest, field by field · Capabilities and what approving
shows · `[config]` · Directory layout · Building (toolchain, `wit` symlink,
`generate_all`, profile) · Installing and consenting · `plugin list`, `doctor`,
the manager · Help pages · WIT compatibility (ADR 0094) · Walk-through: from
an empty directory to a running command (the exact commands, verified in Task 6).

- [ ] **Step 1:** write both; link from `docs/README.md` ("Plugin author guide"),
  `README.md` Plugins section, ADR index; CHANGELOG entries for Tasks 1–4.
- [ ] **Step 2:** `just docs` (intra-doc links) — the guide is Markdown, but ADR links to code items are not; check paths by hand.
- [ ] **Step 3:** commit `docs(plugins): the author guide and ADR 0094`.

### Task 6: the walk-through, done for real, then the gate

- [ ] **Step 1:** in a temp directory OUTSIDE the repo, follow `docs/plugins.md`
  literally: copy the template, rename the id to `org.example.walk`, build,
  `norte plugin install`, `norte plugin list`, approve in the TUI manager (F12),
  `norte plugin run org.example.walk hello world`. Fix the guide where a step
  did not work as written. Record the command block in the guide.
- [ ] **Step 2:** `just ci-fast` (ONE run). Fix, and reproduce any failure with `just t <crate>`.
- [ ] **Step 3:** dispatch `rust-reviewer` (Tasks 1–2 diff, questions: the byte
  rewrite test's validity; whether `wit_imports` can be fooled by a component
  that imports the served version and instantiates an inner one) and
  `security-reviewer` (does listing `wit` on `PluginEntry` leak anything to
  `plugin.list`? It does not cross the wire — confirm; is `wasmparser` on
  untrusted bytes bounded? cap the read at `MAX_ARTIFACT_BYTES`).
- [ ] **Step 4:** apply findings; `just ci` (ONE run, recipes one at a time if it does not fit in the foreground).
- [ ] **Step 5:** memory note, push.
