//! `norte doctor` (H2, `docs/superpowers/specs/2026-07-23-help-config-system-design.md:128-143`):
//! read-only diagnostics over config layers and keymaps. Every check
//! function here is PURE and INJECTABLE — no process-env read hides inside
//! `check_config`/`check_keymaps` (the CLI wiring in `main.rs` passes
//! `std::env::var_os`), and no blocking filesystem I/O happens inside an
//! async context (`norte_config`/`norte_frontend::config` read with
//! `std::fs` synchronously by design — the caller wraps these functions in
//! `spawn_blocking`, rule 2).
//!
//! Task 1 covers `[config]` (parse + split-brain) and `[keymap]`
//! (structural validity + an HONEST APPROXIMATION of unknown commands, see
//! [`norte_frontend::keymap::preset_commands`]). Task 2 adds `[plugins]`
//! (discovery, digest-stale re-approval, `plugin.wasm` presence) and
//! `[connections]` (parse, endpoint validity, secret-env-var presence —
//! decision 2: side-effect-free v1, keyring/age are NOT probed).

use std::ffi::OsString;
use std::path::Path;

use norte_config::Layers;
use norte_connect::{AuthMethod, ConnectionSpec, ConnectionsFile};
use norte_frontend::keymap::{Effective, KeymapDiagnostic, Screen, presets};
use serde::Serialize;

/// Severity of a [`Finding`]. `Ok`/`Warn` never affect the exit code;
/// `Error` makes `norte doctor` exit non-zero (decision 4) even though the
/// full report is still printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Nothing wrong.
    Ok,
    /// Worth a human's attention, not fatal.
    Warn,
    /// Structural breakage.
    Error,
}

/// One diagnostic line. `code` is a STABLE machine-readable identifier (for
/// `--json` consumers and tests — never the raw display text, which is
/// free-form and may change wording); `detail` is the human-readable
/// specifics (culprit path, offending command name, …).
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    /// `"config"`, `"keymap"`, … (Task 2 adds `"plugins"`/`"connections"`).
    pub section: &'static str,
    /// See [`Severity`].
    pub severity: Severity,
    /// Stable code, e.g. `"config-parse"`, `"keymap-unknown-cmd"`.
    pub code: &'static str,
    /// Human-readable specifics.
    pub detail: String,
}

/// Every `norte.toml`/`keymap.toml`/`connections.toml`/`policy.toml` name
/// checked for split-brain (decision 3).
const SPLIT_BRAIN_FILES: &[&str] = &[
    "norte.toml",
    "keymap.toml",
    "connections.toml",
    "policy.toml",
];

/// Checks the merged `norte.toml` scalars across `layers`: a parse/read
/// failure in ANY layer surfaces as one [`Severity::Error`] finding naming
/// the culprit file (via [`norte_config::ConfigError`]'s own `Display`,
/// which already names the path); success surfaces one [`Severity::Ok`]
/// finding per file
/// that participated. Also runs the split-brain check (decision 3) against
/// `env` — NEVER reads the process environment directly, so tests can
/// inject a closure and the CLI wiring passes `std::env::var_os`.
#[must_use]
pub fn check_config(layers: &Layers, env: &impl Fn(&str) -> Option<OsString>) -> Vec<Finding> {
    let mut findings = Vec::new();
    match norte_config::load(layers) {
        Ok(cfg) => {
            for src in &cfg.sources {
                findings.push(Finding {
                    section: "config",
                    severity: Severity::Ok,
                    code: "config-ok",
                    detail: src.display().to_string(),
                });
            }
        }
        Err(e) => findings.push(Finding {
            section: "config",
            severity: Severity::Error,
            code: "config-parse",
            detail: e.to_string(),
        }),
    }
    if let Some(f) = check_split_brain(env) {
        findings.push(f);
    }
    findings
}

/// `[ui] layout`: the named file has to exist and describe a coherent
/// tree.
///
/// It is `Warn` and not `Error` for the same reason startup does not die:
/// a layout that fails to load falls back to `orthodox`, so norte remains
/// usable — but silently the user would have believed their layout was
/// applied.
#[must_use]
pub fn check_layout(layers: &Layers) -> Vec<Finding> {
    let Ok(cfg) = norte_config::load(layers) else {
        return Vec::new(); // the parse is already reported by check_config
    };
    let Some(name) = cfg.ui_layout.as_deref() else {
        return Vec::new();
    };
    if name == "orthodox" {
        return vec![Finding {
            section: "layout",
            severity: Severity::Ok,
            code: "layout-builtin",
            detail: name.to_owned(),
        }];
    }
    // The layout is the USER's: a system or project layout could carve up
    // the screen of someone who did not write it.
    let Some((dir, _)) = layers
        .dirs
        .iter()
        .find(|(_, l)| matches!(l, norte_config::Layer::User))
    else {
        return Vec::new();
    };
    match norte_frontend::layout::config::load(dir, std::ffi::OsStr::new(name)) {
        Ok(_) => vec![Finding {
            section: "layout",
            severity: Severity::Ok,
            code: "layout-ok",
            detail: name.to_owned(),
        }],
        Err(e) => vec![Finding {
            section: "layout",
            severity: Severity::Warn,
            code: "layout-unusable",
            detail: format!("{name}: {e} (will start with «orthodox»)"),
        }],
    }
}

/// `[ui.columns]` (#108 b4): ids that fail to parse = Warn (skipped while
/// painting — "a configured id that disappears silently is a bug, not a
/// degradation", the columns spec's §Diagnostics). Both `plugin:` and
/// `attr:` cells are now painted by the funnel (#117 and its follow-up):
/// their remaining diagnostics are the caps and the attrs' wire legality.
#[must_use]
pub fn check_columns(layers: &Layers) -> Vec<Finding> {
    let Ok(cfg) = norte_config::load(layers) else {
        return Vec::new(); // the parse is already reported by check_config
    };
    let st = norte_frontend::columns::ColumnsSettings::resolve(&cfg.ui_columns);
    let mut findings = Vec::new();
    for raw in &st.invalid {
        findings.push(Finding {
            section: "config",
            severity: Severity::Warn,
            code: "columns-bad-id",
            detail: format!(
                "[ui.columns] unrecognized id (skipped while painting): {}",
                sanitize_detail(raw)
            ),
        });
    }
    // #117-follow-up: `plugin:` cells are already painted — `columns-no-
    // renderer` is retired; the only diagnostic left for them is the cap
    // (mirroring `columns-attrs-over-cap`).
    for raw in &st.plugins_over_cap {
        findings.push(Finding {
            section: "config",
            severity: Severity::Warn,
            code: "columns-plugins-over-cap",
            detail: format!(
                "[ui.columns] plugin column above the cap of {} per list (neither painted nor requested): {}",
                norte_frontend::columns::PLUGIN_COLUMNS_MAX_REQUEST,
                sanitize_detail(raw)
            ),
        });
    }
    // #117: an `attr:` above the per-list request cap — the funnel
    // neither paints it nor requests it (painted == requested), so doctor
    // is the one that counts it.
    for raw in &st.attrs_over_cap {
        findings.push(Finding {
            section: "config",
            severity: Severity::Warn,
            code: "columns-attrs-over-cap",
            detail: format!(
                "[ui.columns] attr above the cap of {} per list (neither painted nor requested): {}",
                norte_proto::attrs::ATTRS_MAX_REQUEST,
                sanitize_detail(raw)
            ),
        });
    }
    // #117 encoding-audit M1: an `attr:` that parses as a column but whose
    // id is not wire-legal (`is_valid_attr_id`: lowercase with a
    // namespace) — the funnel skips it and the pane does not request it
    // (requesting it from a daemon would be -32602 and would bring down
    // the whole fs.list): doctor names it because the id "looks" fine and
    // nothing else counts it.
    for raw in &st.attrs_not_wire_safe {
        findings.push(Finding {
            section: "config",
            severity: Severity::Warn,
            code: "columns-attr-id-not-wire-safe",
            detail: format!(
                "[ui.columns] attr that parses but is not a wire-legal id (lowercase with a namespace, e.g. posix.mode — the column is skipped): {}",
                sanitize_detail(raw)
            ),
        });
    }
    // #108 7b: a `[[ui.columns.spec]]` with an impossible id or a format
    // that does not match its column (e.g. `iec` on mtime) — the default
    // was applied while painting, never a silent drop.
    for raw in &st.bad_specs {
        findings.push(Finding {
            section: "config",
            severity: Severity::Warn,
            code: "columns-bad-spec",
            detail: format!(
                "[ui.columns.spec] impossible id or a format that does not match its column (the default is applied while painting): {}",
                sanitize_detail(raw)
            ),
        });
    }
    findings
}

/// A column id — or a keymap `run` — comes from a user's TOML but can
/// arrive via a hostile copy-paste: masked + capped, never raw in the
/// output. The 64-char cap is generous for both: the shared catalog's
/// longest command name does not reach 24.
fn sanitize_detail(raw: &str) -> String {
    let masked = norte_frontend::display_name(raw.as_bytes()).0;
    masked.chars().take(64).collect()
}

/// Decision 3: if `NORTE_CONFIG_DIR` is set (non-empty) AND the LEGACY dir
/// (what [`norte_config::user_config_dir_from`] would resolve to WITHOUT
/// that override — `XDG_CONFIG_HOME`/`HOME`/platform default) contains any
/// of [`SPLIT_BRAIN_FILES`], the user likely has two config dirs in play
/// (one intentional, one forgotten) — worth a warning, never an error (the
/// override is honored either way, nothing is actually broken).
fn check_split_brain(env: &impl Fn(&str) -> Option<OsString>) -> Option<Finding> {
    let over = env("NORTE_CONFIG_DIR")?;
    if over.is_empty() {
        return None;
    }
    let without_override = |k: &str| {
        if k == "NORTE_CONFIG_DIR" {
            None
        } else {
            env(k)
        }
    };
    let legacy = norte_config::user_config_dir_from(&without_override)?;
    let present: Vec<&str> = SPLIT_BRAIN_FILES
        .iter()
        .copied()
        .filter(|f| legacy.join(f).is_file())
        .collect();
    if present.is_empty() {
        return None;
    }
    Some(Finding {
        section: "config",
        severity: Severity::Warn,
        code: "config-split-brain",
        detail: format!("{} ({})", legacy.display(), present.join(", ")),
    })
}

/// Screens checked (ADR 0006 — every context the shared keymap engine
/// knows about).
const SCREENS: [(Screen, &str); 3] = [
    (Screen::Browse, "browse"),
    (Screen::Viewer, "viewer"),
    (Screen::Dialog, "dialog"),
];

/// Checks every `keymap.toml` layer across `layers` against the merged
/// preset (`[keymap] preset` from `norte.toml`, defaulting like
/// [`norte_config`] does): structural errors (bad TOML, `AmbiguousPrefix`,
/// `BadChord`, `EscInSequence`, a layer using the preset-only `keymap`
/// list, …) are [`Severity::Error`]; a LAYER binding to a `run` name absent
/// from every bundled preset's vocabulary for that screen (decision 1 —
/// see [`preset_commands`](norte_frontend::keymap::preset_commands)'s
/// rustdoc for the honesty caveat) is downgraded to [`Severity::Warn`].
#[must_use]
pub fn check_keymaps(layers: &Layers) -> Vec<Finding> {
    let mut findings = Vec::new();
    let preset_name = match norte_config::load(layers) {
        // `check_config` already reports the parse failure; nothing more
        // to check here without a resolved preset name.
        Err(_) => return findings,
        Ok(cfg) => cfg.preset,
    };
    let Some(preset_src) = presets::source(&preset_name) else {
        findings.push(Finding {
            section: "keymap",
            severity: Severity::Error,
            code: "keymap-unknown-preset",
            detail: preset_name,
        });
        return findings;
    };
    // Bundled presets are compile-time embedded and pinned by
    // `presets_catalog_tests` in norte-frontend: a parse failure here would
    // be a build-breaking regression there, not a user config error.
    let Ok(preset_kf) = norte_frontend::keymap::parse_keymap(preset_src) else {
        findings.push(Finding {
            section: "keymap",
            severity: Severity::Error,
            code: "keymap-bundled-preset-broken",
            detail: preset_name,
        });
        return findings;
    };
    let mut sources = Vec::new();
    let mut layer_kfs = Vec::new();
    for (dir, kind) in &layers.dirs {
        match norte_frontend::config::load_keymap_layer(dir, *kind, &mut sources) {
            Ok(Some(kf)) => layer_kfs.push(kf),
            Ok(None) => {}
            Err(e) => {
                findings.push(Finding {
                    section: "keymap",
                    severity: Severity::Error,
                    code: "keymap-parse",
                    detail: e.to_string(),
                });
                return findings;
            }
        }
    }
    for (screen, label) in SCREENS {
        findings.extend(check_keymap_screen(&preset_kf, &layer_kfs, screen, label));
    }
    findings
}

/// Builds the effective keymap for one `screen` in a SINGLE pass
/// ([`Effective::build_diagnostics`], issue #102) and turns each
/// [`KeymapDiagnostic`] into a [`Finding`]:
///
/// - [`KeymapDiagnostic::UnknownCommand`] → [`Severity::Warn`]
///   (`keymap-unknown-cmd`): a layer binding to a `run` name **the shared
///   catalogue** has never heard of (decision 1 — a typo or a newer version's
///   command, not fatal). Since K1 (ADR 0043) this is no longer "no bundled
///   preset recognizes it for this screen": a name the catalogue knows but
///   this screen does not implement is a declared unavailability and produces
///   NO finding, which is why the false positives went away.
/// - [`KeymapDiagnostic::Structural`] → [`Severity::Error`]
///   (`keymap-structural`): bad chord, empty/`esc` sequence, wrong layer key,
///   ambiguous prefix, a `lua:` name that fails the charset, or — since K2a
///   (ADR 0044) — a digit key bound while the preset enables counts, or a
///   reserved key (`Tab`) taken from `pane.switch` on the Browse screen.
///
/// No diagnostics → one [`Severity::Ok`] `keymap-ok`. The one-pass builder
/// reports EVERY finding at once, so this needs no retry loop, no anti-DoS
/// cap, and cannot get stuck on the non-convergent `lua:`-charset case the
/// old retry-with-known-name approach had to special-case.
fn check_keymap_screen(
    preset_kf: &norte_frontend::keymap::KeymapFile,
    layer_kfs: &[norte_frontend::keymap::KeymapFile],
    screen: Screen,
    label: &'static str,
) -> Vec<Finding> {
    let known = norte_frontend::keymap::preset_commands(screen);
    let known_refs: Vec<&str> = known.iter().map(String::as_str).collect();
    let diags = Effective::build_diagnostics(preset_kf, layer_kfs, &known_refs, screen);
    if diags.is_empty() {
        return vec![Finding {
            section: "keymap",
            severity: Severity::Ok,
            code: "keymap-ok",
            detail: label.to_owned(),
        }];
    }
    diags
        .into_iter()
        .map(|d| match d {
            // `run` is UNTRUSTED config text, verbatim from a user's — or a
            // cloned repository's — `keymap.toml`, and this line goes to a
            // terminal. Unmasked, `"app.\u{202E}tiuq"` PAINTS as `app.quit`,
            // so the warning names a command the user cannot tell from the
            // real one, and `"app.quit\u{1B}]0;x\u{7}"` sets the title. The
            // same rule the column-id and plugin arms already follow.
            KeymapDiagnostic::UnknownCommand { run } => Finding {
                section: "keymap",
                severity: Severity::Warn,
                code: "keymap-unknown-cmd",
                detail: format!("{label}: {}", sanitize_detail(&run)),
            },
            // `KeymapError`'s `Display` interpolates the offending text with
            // `{:?}`, and `escape_debug` catches Cf — but NOT U+3164 HANGUL
            // FILLER or U+2800 BRAILLE BLANK, which norte classifies as
            // hazards (#125). Do not rely on the accident; mask. No cap:
            // unlike a command name, a structural diagnostic is norte's own
            // prose and needs its length to stay actionable.
            KeymapDiagnostic::Structural { message } => Finding {
                section: "keymap",
                severity: Severity::Error,
                code: "keymap-structural",
                detail: format!(
                    "{label}: {}",
                    norte_encoding::mask_terminal_hazards(&message)
                ),
            },
        })
        .collect()
}

/// Checks `config_dir/plugins/*` (manifest discovery via
/// [`norte_core::plugins::PluginRegistry::discover`]) + the persisted
/// `plugins-state.toml` + `plugin.wasm` presence.
///
/// `discover` failing (state file corrupt — `io::ErrorKind::InvalidData`, see
/// its rustdoc) is a single [`Severity::Error`] finding: the STATE, not any
/// one plugin, is unreadable, so nothing more can be checked. On success:
/// each [`norte_proto::methods::PluginLoadError`] (broken manifest) is an
/// [`Severity::Error`] (its `dir` is ALREADY the pre-redacted basename —
/// `PluginRegistry::list`'s own rustdoc: never the absolute path, home dir
/// disclosure); each [`norte_proto::methods::PluginInfo`] is an
/// [`Severity::Ok`] naming id + effective approved/enabled.
///
/// Digest-stale detection (issue #69's mechanism, surfaced here): `list()`
/// only exposes the EFFECTIVE `approved` (digest-checked against the current
/// manifest — `PluginRegistry::list`'s own doc). Comparing it against the
/// RAW persisted flag from `PluginRegistry::state_snapshot` (the state file
/// as last written by a human) is what tells "never approved" apart from
/// "approved, but the manifest changed since" — a former `true` that reads
/// back as an effective `false` can ONLY mean the digest stopped matching, so
/// that pair becomes a [`Severity::Warn`] naming the re-approval requirement
/// (`detail` carries only the plugin id — a MACHINE value; the narrative
/// sentence lives in the text renderer's `cli-doctor-detail-plugin-digest-stale`
/// Fluent key, keyed by `code`, so `--json` stays locale-free, review
/// MINOR-3).
///
/// `plugin.wasm` presence is a direct `Path::is_file` — read-only, no
/// `spawn_blocking` needed by ITSELF (the caller already runs the whole
/// check inside one, rule 2) — against `PluginRegistry::wasm_path` (review
/// MINOR-4: was a raw `config_dir.join(...)` duplicate of that layout here
/// before). No symlink-safety canonicalization here, unlike the registry's
/// own `verified_wasm` (issue #69) — this is a presence check for a
/// diagnostic, not a load path, so that defense does not apply.
///
/// P2 Task 2 adds `[config]` VALUES visibility (decision 5: host-side only —
/// `doctor` runs embedded, so it CAN show them; the wire-facing extension
/// manager cannot until the protocol bump G3 already requires). Each
/// resolved key of `PluginRegistry::settings_of` becomes one
/// [`Severity::Ok`] `plugin-config` finding, `detail` = `{id}: {key}=
/// {value}` — the value is the plugin's OWN default or the user's OWN
/// override, but still MASKED+CAPPED via [`masked_and_capped`] (belt, same
/// policy as the TUI's `must_mask`/`display_name`: a value is untrusted text
/// regardless of who wrote it). A `config.toml` that fails validation
/// against the schema does NOT reach `plugin-config` at all — the whole
/// plugin is already excluded and surfaced by the `plugin-manifest-broken`
/// loop above instead (fail-closed at catalog level,
/// `norte_plugin_host::Catalog::load_dir`'s own contract) — but that
/// loop's `reason` is untrusted TOO (P2 Task 4a security review: a
/// `ConfigValueError::UnknownKey`'s message can carry a key straight out of
/// a hostile `config.toml`, which — unlike a manifest-declared key — has no
/// charset guarantee even after the source-side fix in `norte-plugin-host`;
/// defense in depth applies [`masked_and_capped`] here too, not just to
/// `plugin-config`).
#[must_use]
pub fn check_plugins(config_dir: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    let registry = match norte_core::plugins::PluginRegistry::discover(config_dir) {
        Ok(r) => r,
        Err(e) => {
            findings.push(Finding {
                section: "plugins",
                severity: Severity::Error,
                code: "plugins-state-unreadable",
                detail: e.to_string(),
            });
            return findings;
        }
    };
    let snapshot = registry.state_snapshot();
    let list = registry.list();
    for err in registry.load_errors() {
        // Only the directory's basename, as `plugin.list` does: the absolute
        // path would name the user's home.
        let dir = err
            .dir
            .file_name()
            .map_or_else(|| "?".to_owned(), |b| b.to_string_lossy().into_owned());
        // A binary built against another WIT is not a broken manifest: the
        // author has to rebuild, not edit (ADR 0094). Its own code, so the
        // reader gets the two versions and the one verb.
        if let norte_core::plugins::ManifestError::WitMismatch {
            package,
            built_against,
            served,
        } = &err.error
        {
            findings.push(Finding {
                section: "plugins",
                severity: Severity::Warn,
                code: "plugin-wit-mismatch",
                // `package` and `served` are the host's constants; the version
                // the binary names is third-party bytes, and the reader for
                // this list narrows it to a version shape — masked and capped
                // here anyway, at the same boundary as every other detail.
                detail: format!(
                    "{dir}: {package}@{} (served: @{served})",
                    masked_and_capped(built_against)
                ),
            });
            continue;
        }
        findings.push(Finding {
            section: "plugins",
            severity: Severity::Error,
            code: "plugin-manifest-broken",
            // security review P2 Task 4a: the reason can carry untrusted
            // text (a `config.toml` values error names the offending KEY,
            // which is user TOML and — unlike a manifest-declared key — has
            // no charset guarantee; a manifest parse error can likewise
            // quote hostile bytes). Masked + capped at the SAME boundary as
            // `plugin-config` below, not just relying on the source-side
            // fix in `ConfigValueError` (defense in depth: this loop also
            // covers `ManifestError` variants that pre-date that fix).
            detail: format!("{dir}: {}", masked_and_capped(&err.error.to_string())),
        });
    }
    for p in &list.plugins {
        findings.push(Finding {
            section: "plugins",
            severity: Severity::Ok,
            code: "plugin-ok",
            detail: format!("{} (approved={}, enabled={})", p.id, p.approved, p.enabled),
        });
        let was_approved_raw = snapshot.get(&p.id).is_some_and(|st| st.approved);
        if was_approved_raw && !p.approved {
            findings.push(Finding {
                section: "plugins",
                severity: Severity::Warn,
                code: "plugin-digest-stale",
                detail: p.id.clone(),
            });
        }
        if !registry.wasm_path(&p.id).is_file() {
            findings.push(Finding {
                section: "plugins",
                severity: Severity::Warn,
                code: "plugin-no-binary",
                detail: p.id.clone(),
            });
        }
        // `announces_help`, NOT the wire's `p.has_help`: the two answer
        // different questions on purpose. The wire flag is STRICT (it applies
        // the escape guard, so a `help.md` symlinked out of the plugin's own
        // directory reports `false` and stops being a path-existence oracle);
        // this is a LOCAL diagnostic, and gating it on the strict flag would
        // make `norte doctor` blind to exactly the case it exists to report.
        if registry.announces_help(&p.id) {
            findings.extend(check_plugin_help(&registry, &p.id));
        }
        if let Some(settings) = registry.settings_of(&p.id) {
            for (key, value) in settings {
                findings.push(Finding {
                    section: "plugins",
                    severity: Severity::Ok,
                    code: "plugin-config",
                    detail: format!("{}: {key}={}", p.id, masked_and_capped(value)),
                });
            }
        }
    }
    findings
}

/// `plugin-help` findings for ONE plugin that announces a `help.md` (H3e).
///
/// None of this is a hard error — help is cosmetic and never stops an approved
/// plugin from loading:
///
/// * `plugin-help-truncated`: the file is past the untrusted cap and is served
///   cut short.
/// * `plugin-help-lossy`: it carries bytes that decode under no reading the
///   parser is willing to make.
/// * `plugin-help-empty`: it ANNOUNCES a page and serves nothing.
///   [`norte_core::plugins::PluginRegistry::announces_help`] answers the lax
///   question — is there a `help.md` at all — while the CONTENT is read
///   through the escape guard, so this state is real and otherwise invisible.
///   The WIRE's `PluginInfo.has_help` is the strict flag and would hide it:
///   gating this check on that one would blind the diagnostic to the very case
///   it is here to name. Three causes,
///   all the author's: the file is empty, it is unreadable (permissions, a
///   directory), or it is a symlink pointing OUT of the plugin's own directory
///   — which the host refuses to serve. That last one is the guard doing its
///   job, and from the reader's side it is indistinguishable from an author who
///   wrote nothing, which is exactly why it needs a finding.
/// * `plugin-help-bad-header`: the file opens a `+++` fence and the header it
///   starts does not parse. `FrontMatter`'s `id` and `title` are REQUIRED, so
///   forgetting one — the commonest way to write a `help.md` that silently does
///   nothing — makes the WHOLE header vanish: `parse_untrusted` degrades a bad
///   header to "there is no header" and reads its text as body prose. Nothing
///   else reports it, because `foreign_commands` answers `[]` for a header that
///   failed to parse exactly as it does for a clean one, and that asymmetry is
///   the entire argument for this finding
///   ([`norte_help::has_broken_front_matter`] is the question; the grammar is
///   NOT re-derived here). A file with no fence at all is not an error — a
///   plugin may choose not to declare a header.
/// * `plugin-help-foreign-command`: the header declares commands that are not
///   the plugin's own. They are dropped from the model in silence, so this is
///   where the author finds out. Only the HEADER is examined: a foreign
///   `{{cmd:…}}` in the BODY degrades to literal text inside the span cutter,
///   with no counter to thread out, and adding one would touch every span
///   signature for the sake of a diagnostic
///   ([`norte_help::foreign_commands`]'s own contract).
/// * `plugin-help-shadows-topic`: the plugin's id is also a built-in help page
///   id. The frontend discards the node (the corpus wins, fail-closed), so
///   without this finding the plugin's page would vanish without saying why.
///   It cannot fire TODAY — a `plugin.id` must be reverse-DNS (two dotted
///   segments minimum) and every corpus id is a single segment, so the two id
///   spaces do not overlap. That is a property of two crates that neither
///   promises to the other, and it costs one lookup to keep the guard; the
///   test `la_colision_de_id_con_el_corpus_es_hoy_estructuralmente_imposible`
///   pins both invariants so relaxing either one is loud.
///
/// Third-party text reaching a `detail` goes through [`masked_and_capped`],
/// exactly as in `plugin-config`. The plugin ID does not: it is already the
/// catalogue's own validated key, and every other plugin finding in this module
/// prints it raw.
///
fn check_plugin_help(registry: &norte_core::plugins::PluginRegistry, id: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let Some(help) = registry.help_of(id) else {
        return out;
    };
    if help.truncated {
        out.push(Finding {
            section: "plugins",
            severity: Severity::Warn,
            code: "plugin-help-truncated",
            detail: id.to_owned(),
        });
    }
    if help.lossy {
        out.push(Finding {
            section: "plugins",
            severity: Severity::Warn,
            code: "plugin-help-lossy",
            detail: id.to_owned(),
        });
    }
    if help.markdown.is_empty() {
        out.push(Finding {
            section: "plugins",
            severity: Severity::Warn,
            code: "plugin-help-empty",
            detail: id.to_owned(),
        });
    }
    if norte_help::has_broken_front_matter(&help.markdown) {
        out.push(Finding {
            section: "plugins",
            severity: Severity::Warn,
            code: "plugin-help-bad-header",
            detail: id.to_owned(),
        });
    }
    for foreign in norte_help::foreign_commands(&help.markdown, id) {
        out.push(Finding {
            section: "plugins",
            severity: Severity::Warn,
            code: "plugin-help-foreign-command",
            detail: format!("{id}: {}", masked_and_capped(&foreign)),
        });
    }
    // The collision is judged against the ENGLISH corpus: both locales carry
    // the same ids (the parity test pins that), so asking both would report
    // the same defect twice.
    if norte_help::topic(norte_help::Lang::En, id).is_some() {
        out.push(Finding {
            section: "plugins",
            severity: Severity::Warn,
            code: "plugin-help-shadows-topic",
            detail: id.to_owned(),
        });
    }
    out
}

/// Cap (CHARS, not bytes — same criterion as the manifest's own
/// `description`/`[config]` topes) on untrusted text shown in a plugin
/// finding: a `plugin-config` VALUE (P2 Task 2 — a plugin's own default, or a
/// user's own override, could still be pathologically long: the manifest
/// caps a `[config]` DEFAULT at 280 chars, decision 1, but `config.toml`
/// VALUES had no such cap before the P2 Task 4a security-review fix, and
/// even with it a legitimate 280-char value is still too wide for one line),
/// or a `plugin-manifest-broken` REASON (P2 Task 4a — a `ConfigValueError`
/// naming an untrusted key, or any other `ManifestError`'s `Display`).
const PLUGIN_FINDING_TEXT_MAX_CHARS: usize = 80;

/// Masks terminal hazards ([`norte_encoding::mask_terminal_hazards`]) and
/// caps to [`PLUGIN_FINDING_TEXT_MAX_CHARS`] CHARS (an ellipsis marks a cut)
/// for display in a `plugin-config`/`plugin-manifest-broken` finding — ANY
/// text that ultimately traces back to a plugin manifest or a user's
/// `config.toml`, neither of which this process trusts.
fn masked_and_capped(value: &str) -> String {
    let masked = norte_encoding::mask_terminal_hazards(value);
    if masked.chars().count() <= PLUGIN_FINDING_TEXT_MAX_CHARS {
        return masked;
    }
    let mut truncated: String = masked.chars().take(PLUGIN_FINDING_TEXT_MAX_CHARS).collect();
    truncated.push('…');
    truncated
}

/// The local log's state (roadmap item 9): where it is, how much space it
/// takes, and whether it can be written to.
///
/// **It is the row that turns "there are logs" into "someone other than
/// you can report a failure".** `doctor` is where a user looks when
/// something goes wrong, and until now there was no way for them to know
/// the file exists, or where.
///
/// None of its states is [`Severity::Error`], on purpose: a machine with
/// no state directory, or one that cannot be written to, WORKS — it just
/// leaves no trace. An `Error` would make `norte doctor` exit non-zero
/// (decision 4) for something that breaks nothing, and that trains people
/// to ignore its exit code.
///
/// `dir` is whatever [`norte_core::logging::log_dir`] resolves; `None` =
/// no state directory on this machine.
#[must_use]
pub fn check_logs(dir: Option<&Path>) -> Vec<Finding> {
    let Some(dir) = dir else {
        return vec![Finding {
            section: "logs",
            severity: Severity::Warn,
            code: "logs-no-state-dir",
            detail: String::new(),
        }];
    };
    // Writability is checked by TRYING IT, not by reading permissions:
    // permissions do not account for ACLs, a read-only mount, or SELinux.
    // It is created and deleted, which is exactly what the appender will
    // do.
    //
    // **`create_new`, never `fs::write`.** `write` is `O_TRUNC` and DOES
    // follow symlinks: with a link planted at the probe's name — fixed
    // and predictable, so there is no race to win — a `norte doctor`
    // would truncate to zero whatever it pointed at, and the
    // `remove_file` afterward would delete the LINK and not the target,
    // so the file would end up empty and the evidence would vanish.
    // `O_EXCL` refuses to follow a link and refuses to overwrite
    // something that already exists, which is exactly what is needed
    // here.
    let probe = dir.join(format!(".norte-doctor-probe.{}", std::process::id()));
    let writable = std::fs::create_dir_all(dir).is_ok()
        && std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
            .is_ok();
    // A failed delete is not reported: it has just been proven that the
    // directory can be written to, so the only possible leftover is one
    // per pid, and the next `create_new` would detect it as unwritable
    // instead of overwriting it — which is the safe side of the error.
    let _ = std::fs::remove_file(&probe);
    if !writable {
        return vec![Finding {
            section: "logs",
            severity: Severity::Warn,
            code: "logs-unwritable",
            detail: dir.display().to_string(),
        }];
    }
    let bytes: u64 = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum(),
        Err(_) => 0,
    };
    vec![Finding {
        section: "logs",
        severity: Severity::Ok,
        code: "logs-ok",
        detail: format!("{} ({bytes} bytes)", dir.display()),
    }]
}

/// Checks `config_dir/connections.toml` (decision 2: side-effect-free v1 —
/// secret PRESENCE only, never the value; keyring/age are NOT probed here,
/// that would touch the OS keychain or prompt).
///
/// [`ConnectionsFile::load`] already treats an absent file as empty (not an
/// error — connections are optional); that empty case surfaces as one
/// [`Severity::Ok`] `connections-none` finding. A broken file (bad TOML, or
/// a rejected inline secret field — `deny_unknown_fields`, ADR 0015 B) is a
/// single [`Severity::Error`] `connections-parse`. Per connection:
/// [`ConnectionSpec::endpoint`][ep] parsing is [`Severity::Error`] on
/// failure (its `Display` never echoes a password — `spec.rs`'s own
/// `inline_password_in_url_rejected_without_echo`/`an_invalid_scheme_with_an_inline_password_is_not_echoed`
/// tests pin that); `Agent`/`Key` auth need no secret and are
/// [`Severity::Ok`]; `Password`/`AccessKey` (secret-bearing, decision 2) are
/// checked for [`norte_connect::env_key`]'s var via `env`, which has FOUR
/// outcomes, all naming the var and never a value: usable is
/// [`Severity::Ok`]; absent is [`Severity::Warn`], because the keyring or
/// `secrets.age` may still supply it; and set-but-empty or set-but-not-UTF-8
/// are [`Severity::Error`] — after #320 neither can resolve and no later step
/// can rescue them, which is the same class as `connection-endpoint-invalid`:
/// knowably unable to connect, decided without probing. Grading those `Warn`
/// would let a `norte doctor` preflight exit 0 on a connection that cannot
/// work. The "keyring/age not probed" note is NOT a finding — it is a
/// constant caveat about THIS FUNCTION, not a fact about the config it read,
/// so it is printed once by the text renderer instead (review MINOR-3);
/// `--json` consumers don't need it repeated per run.
///
/// `connections-parse`'s `detail` is DELIBERATELY not `e.to_string()`:
/// [`norte_connect::ConnectError::Config`] wraps `toml`'s own parse-error
/// `Display`, which echoes the offending line — an unterminated
/// `password = "hunter2` would put that fragment straight into stdout and
/// `--json` (rule 10, review MINOR-2). `connections-none`/`connections-parse`
/// details are therefore left as MACHINE values (empty — there is no
/// identifying value to report for either), and their full sentence lives
/// in the text renderer's `cli-doctor-detail-*` Fluent keys, keyed by `code`
/// (review MINOR-3, same policy as `plugin-digest-stale` above).
///
/// [ep]: norte_connect::ConnectionSpec::endpoint
#[must_use]
pub fn check_connections(
    config_dir: &Path,
    env: &impl Fn(&str) -> Option<OsString>,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    let Ok(file) = ConnectionsFile::load(config_dir) else {
        findings.push(Finding {
            section: "connections",
            severity: Severity::Error,
            code: "connections-parse",
            detail: String::new(),
        });
        return findings;
    };
    if file.connections.is_empty() {
        findings.push(Finding {
            section: "connections",
            severity: Severity::Ok,
            code: "connections-none",
            detail: String::new(),
        });
    } else {
        // The connections parser accepts any scheme (a provider plugin
        // serves whatever it declares, ADR 0093), so `sfpt://` no longer
        // dies while parsing: connecting answers with a bare
        // `Unsupported`. Here the file and the catalog sit side by side,
        // which is the only place that can say "nobody serves that
        // scheme".
        let plugin_schemes = norte_core::plugins::installed_provider_schemes(config_dir);
        for (name, spec) in &file.connections {
            match spec.endpoint() {
                Ok(ep) => {
                    let served = norte_core::plugins::CORE_SCHEMES.contains(&ep.scheme.as_str())
                        || plugin_schemes.contains(&ep.scheme);
                    findings.push(Finding {
                        section: "connections",
                        severity: if served { Severity::Ok } else { Severity::Warn },
                        code: if served {
                            "connection-ok"
                        } else {
                            "connection-scheme-unserved"
                        },
                        detail: name.clone(),
                    });
                    findings.extend(rsa_finding(name, spec, &ep.scheme));
                }
                Err(e) => {
                    findings.push(Finding {
                        section: "connections",
                        severity: Severity::Error,
                        code: "connection-endpoint-invalid",
                        detail: format!("{name}: {e}"),
                    });
                    continue;
                }
            }
            match spec.auth {
                // #325: `secret = "prompt"` only does something with
                // `password` and `access-key`. With `agent` there is no
                // secret to ask for, and with `key` the secret is the
                // key's PASSPHRASE, where empty and absent are the same
                // thing — asking there would pop a dialog every time
                // someone uses an unencrypted key. That the key does
                // nothing is defensible; that NOBODY says so is the same
                // kind of silent lie #320 came to remove.
                AuthMethod::Agent | AuthMethod::Key => {
                    if spec.secret == norte_connect::SecretSource::Prompt {
                        findings.push(Finding {
                            section: "connections",
                            severity: Severity::Warn,
                            code: "conn-secret-prompt-inert",
                            detail: name.clone(),
                        });
                    }
                }
                AuthMethod::Password | AuthMethod::AccessKey => {
                    let var = norte_connect::env_key(name);
                    // Set-but-unusable is its OWN diagnosis (#320), never
                    // `present`: saying `Ok` about a variable that cannot
                    // authenticate is the lie that cost a debugging session,
                    // told by the one tool meant to catch it. The value is
                    // inspected inside this expression and dropped there — an
                    // `OsString` cannot be zeroized, so it must not outlive the
                    // question being asked of it.
                    let state = env(&var).map(|v| {
                        if v.is_empty() {
                            (Severity::Error, "conn-secret-env-empty")
                        } else if v.to_str().is_none() {
                            // `resolve` reads with `var_os` + `into_string`, so
                            // bytes that do not decode are a hard failure there.
                            // Reading with a different policy here is how this
                            // check certified a variable the resolver ignored.
                            (Severity::Error, "conn-secret-env-not-utf8")
                        } else {
                            (Severity::Ok, "conn-secret-env-present")
                        }
                    });
                    if let Some((severity, code)) = state {
                        findings.push(Finding {
                            section: "connections",
                            severity,
                            code,
                            detail: format!("{name}: {var}"),
                        });
                    } else if spec.secret == norte_connect::SecretSource::Prompt {
                        // #325: the entry says `prompt`, so the absence is
                        // EXPECTED — norte will ask for it. Warning here
                        // would be the same kind of lie #320 came to
                        // remove, only the other way around: a Warn about
                        // the one configuration with nothing broken.
                        findings.push(Finding {
                            section: "connections",
                            severity: Severity::Ok,
                            code: "conn-secret-prompt",
                            detail: format!("{name}: {var}"),
                        });
                    } else {
                        findings.push(Finding {
                            section: "connections",
                            severity: Severity::Warn,
                            code: "conn-secret-env-absent",
                            detail: format!("{name}: {var}"),
                        });
                    }
                }
            }
        }
    }
    findings
}

/// ADR 0150: `allow_rsa` is an accepted risk, not a preference, so it is
/// recalled while it stays set. It only acts on sftp with `auth = "key"`;
/// elsewhere it does nothing, and that is also said (like #325).
fn rsa_finding(name: &str, spec: &ConnectionSpec, scheme: &str) -> Option<Finding> {
    if !spec.allow_rsa {
        return None;
    }
    let acts = scheme == "sftp" && spec.auth == AuthMethod::Key;
    Some(Finding {
        section: "connections",
        severity: Severity::Warn,
        code: if acts {
            "conn-rsa-allowed"
        } else {
            "conn-rsa-allowed-inert"
        },
        detail: name.to_owned(),
    })
}

#[cfg(test)]
mod tests {

    /// #108 b4: a broken id = a named Warn (never a silent drop).
    /// #117-follow-up: `plugin:` cells are ALREADY painted (like `attr:`
    /// since #117) — `columns-no-renderer` is RETIRED and fires for
    /// nobody; only the cap diagnostic is left for them.
    #[test]
    fn broken_columns_ids_are_reported_and_no_renderer_is_retired() {
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::write(
            dir.path().join("norte.toml"),
            "[ui.columns]\ndefault = [\"name\", \"sise\", \"attr:posix.mode\", \"plugin:demo/x\"]\n",
        )
        .expect("write");
        let layers = norte_config::Layers {
            dirs: vec![(dir.path().to_path_buf(), norte_config::Layer::User)],
        };
        let f = super::check_columns(&layers);
        assert!(
            f.iter()
                .any(|x| x.code == "columns-bad-id" && x.detail.contains("sise")),
            "{f:?}"
        );
        assert!(
            !f.iter().any(|x| x.code == "columns-no-renderer"),
            "columns-no-renderer retired — plugin: is painted: {f:?}"
        );
    }

    /// #117-follow-up: a list's 9th plugin column exceeds the request cap
    /// — neither painted nor requested, and doctor names it
    /// (`columns-plugins-over-cap`, mirroring the attrs cap).
    #[test]
    fn columns_plugins_over_the_cap_are_reported() {
        let dir = tempfile::tempdir().expect("tmp");
        let cols: Vec<String> = (0..9).map(|i| format!("\"plugin:p/c{i}\"")).collect();
        std::fs::write(
            dir.path().join("norte.toml"),
            format!("[ui.columns]\ndefault = [\"name\", {}]\n", cols.join(", ")),
        )
        .expect("write");
        let layers = norte_config::Layers {
            dirs: vec![(dir.path().to_path_buf(), norte_config::Layer::User)],
        };
        let f = super::check_columns(&layers);
        assert!(
            f.iter().any(|x| {
                x.code == "columns-plugins-over-cap" && x.detail.contains("plugin:p/c8")
            }),
            "{f:?}"
        );
    }

    /// #117: a list's 17th attr exceeds the request cap — neither
    /// painted nor requested, and doctor names it
    /// (`columns-attrs-over-cap`).
    #[test]
    fn columns_attrs_over_the_cap_are_reported() {
        let dir = tempfile::tempdir().expect("tmp");
        let attrs: Vec<String> = (0..17).map(|i| format!("\"attr:mem.a{i:02}\"")).collect();
        std::fs::write(
            dir.path().join("norte.toml"),
            format!("[ui.columns]\ndefault = [\"name\", {}]\n", attrs.join(", ")),
        )
        .expect("write");
        let layers = norte_config::Layers {
            dirs: vec![(dir.path().to_path_buf(), norte_config::Layer::User)],
        };
        let f = super::check_columns(&layers);
        assert!(
            f.iter()
                .any(|x| x.code == "columns-attrs-over-cap" && x.detail.contains("mem.a16")),
            "{f:?}"
        );
    }

    /// #117 encoding-audit M1: an `attr:` that parses but is not a
    /// wire-legal id (a case typo) — the column is skipped and doctor
    /// names it (`columns-attr-id-not-wire-safe`); without this it would
    /// be invisible (the id parses fine and nothing else counts it).
    #[test]
    fn columns_attr_id_not_wire_safe_is_reported() {
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::write(
            dir.path().join("norte.toml"),
            "[ui.columns]\ndefault = [\"name\", \"attr:Posix.Mode\", \"attr:posix.mode\"]\n",
        )
        .expect("write");
        let layers = norte_config::Layers {
            dirs: vec![(dir.path().to_path_buf(), norte_config::Layer::User)],
        };
        let f = super::check_columns(&layers);
        assert!(
            f.iter()
                .any(|x| x.code == "columns-attr-id-not-wire-safe"
                    && x.detail.contains("Posix.Mode")),
            "{f:?}"
        );
        // The well-formed one triggers nothing.
        assert!(
            !f.iter().any(|x| x.code == "columns-attr-id-not-wire-safe"
                && x.detail.contains("attr:posix.mode")),
            "{f:?}"
        );
    }

    /// #108 7b: a spec whose format does not match its column (`iec` on a
    /// timestamp) = Warn `columns-bad-spec` naming the id — the renderer
    /// applies the default silently, so doctor is the one that counts it.
    #[test]
    fn a_mismatched_columns_spec_is_reported() {
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::write(
            dir.path().join("norte.toml"),
            "[[ui.columns.spec]]\nid = \"mtime\"\nformat = \"iec\"\n",
        )
        .expect("write");
        let layers = norte_config::Layers {
            dirs: vec![(dir.path().to_path_buf(), norte_config::Layer::User)],
        };
        let f = super::check_columns(&layers);
        assert!(
            f.iter()
                .any(|x| x.code == "columns-bad-spec" && x.detail.contains("mtime")),
            "{f:?}"
        );
    }
    use std::ffi::OsString;

    use norte_config::{Layer, Layers};

    use super::*;

    fn env<'a>(v: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |k| {
            v.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, x)| OsString::from(x))
        }
    }

    /// P2 Task 4a security review: `masked_and_capped` (the boundary policy
    /// shared by `plugin-config` values AND `plugin-manifest-broken`
    /// reasons) tested DIRECTLY, independent of whichever upstream source
    /// happens to already be clean — a defense-in-depth boundary must hold
    /// on its own, not just because nothing hostile reaches it today.
    #[test]
    fn masked_and_capped_masks_hazards_and_trims_long_ones() {
        let hostile = format!("safe{}rest", '\u{1b}');
        let out = masked_and_capped(&hostile);
        assert!(!out.contains('\u{1b}'), "{out}");
        assert!(out.contains('\u{fffd}'), "{out}");

        let long = "a".repeat(200);
        let out2 = masked_and_capped(&long);
        assert!(
            out2.chars().count() < 200,
            "a long value must be capped: {out2}"
        );
    }

    /// TDD: valid layers → every finding is `Ok`; a broken `norte.toml` in
    /// one layer → an `Error` finding carrying the culprit path.
    #[test]
    fn config_ok_and_broken_toml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("norte.toml"), "[ui]\ntheme = \"nord\"\n").unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let findings = check_config(&layers, &env(&[]));
        assert!(!findings.is_empty(), "at least the config-ok finding");
        assert!(
            findings.iter().all(|f| f.severity == Severity::Ok),
            "{findings:?}"
        );

        let broken = tempfile::tempdir().unwrap();
        std::fs::write(broken.path().join("norte.toml"), "this is not toml [[[").unwrap();
        let layers = Layers {
            dirs: vec![(broken.path().to_path_buf(), Layer::User)],
        };
        let findings = check_config(&layers, &env(&[]));
        let err = findings
            .iter()
            .find(|f| f.severity == Severity::Error)
            .unwrap_or_else(|| panic!("expected an Error finding: {findings:?}"));
        assert_eq!(err.code, "config-parse");
        assert!(
            err.detail
                .contains(&broken.path().join("norte.toml").display().to_string()),
            "detail must name the culprit path: {}",
            err.detail
        );
    }

    /// TDD (decision 3): `NORTE_CONFIG_DIR` set + a legacy dir (resolved
    /// via `XDG_CONFIG_HOME` once the override is omitted) that ALSO has a
    /// `norte.toml` → a `Warn` split-brain finding.
    #[test]
    fn split_brain_warns() {
        let legacy_xdg = tempfile::tempdir().unwrap();
        let legacy_norte_dir = legacy_xdg.path().join("norte");
        std::fs::create_dir_all(&legacy_norte_dir).unwrap();
        std::fs::write(legacy_norte_dir.join("norte.toml"), "").unwrap();

        let over = tempfile::tempdir().unwrap();
        let vars = [
            ("NORTE_CONFIG_DIR", over.path().to_str().unwrap()),
            ("XDG_CONFIG_HOME", legacy_xdg.path().to_str().unwrap()),
        ];
        let e = env(&vars);
        let layers = Layers {
            dirs: vec![(over.path().to_path_buf(), Layer::User)],
        };
        let findings = check_config(&layers, &e);
        let warn = findings
            .iter()
            .find(|f| f.code == "config-split-brain")
            .unwrap_or_else(|| panic!("expected a split-brain finding: {findings:?}"));
        assert_eq!(warn.severity, Severity::Warn);
        assert!(
            warn.detail
                .contains(&legacy_norte_dir.display().to_string()),
            "{}",
            warn.detail
        );

        // Without a legacy norte.toml present, no split-brain finding.
        let over2 = tempfile::tempdir().unwrap();
        let clean_xdg = tempfile::tempdir().unwrap();
        let vars2 = [
            ("NORTE_CONFIG_DIR", over2.path().to_str().unwrap()),
            ("XDG_CONFIG_HOME", clean_xdg.path().to_str().unwrap()),
        ];
        let e2 = env(&vars2);
        let layers2 = Layers {
            dirs: vec![(over2.path().to_path_buf(), Layer::User)],
        };
        let findings2 = check_config(&layers2, &e2);
        assert!(
            !findings2.iter().any(|f| f.code == "config-split-brain"),
            "{findings2:?}"
        );
    }

    /// TDD: a layer prepending a LONGER sequence over an existing SHORTER
    /// one (`home` from orthodox's `[pane]` vs. a layer's `home g`) is an
    /// `AmbiguousPrefix` — structural, `Error`, naming both sequences.
    #[test]
    fn keymap_ambiguous_prefix_is_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("keymap.toml"),
            "[pane]\nprepend_keymap = [{ on = [\"home\", \"g\"], run = \"cursor.top\" }]\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let findings = check_keymaps(&layers);
        let err = findings
            .iter()
            .find(|f| f.code == "keymap-structural")
            .unwrap_or_else(|| panic!("expected a structural finding: {findings:?}"));
        assert_eq!(err.severity, Severity::Error);
        // The chord `Debug` repr capitalizes the key name (`Home`).
        assert!(err.detail.contains("Home"), "{}", err.detail);
    }

    /// TDD (decision 1): a layer binding to a `run` name no bundled preset
    /// recognizes for that screen is a `Warn`, not an `Error`.
    #[test]
    fn keymap_unknown_command_is_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("keymap.toml"),
            "[pane]\nappend_keymap = [{ on = [\"z\"], run = \"invented.command\" }]\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let findings = check_keymaps(&layers);
        assert!(
            !findings.iter().any(|f| f.severity == Severity::Error),
            "{findings:?}"
        );
        let warn = findings
            .iter()
            .find(|f| f.code == "keymap-unknown-cmd")
            .unwrap_or_else(|| panic!("expected an unknown-cmd finding: {findings:?}"));
        assert_eq!(warn.severity, Severity::Warn);
        assert!(warn.detail.contains("invented.command"), "{}", warn.detail);
    }

    /// encoding-auditor MAJOR: `run` is untrusted config text — a cloned
    /// repository's `./.norte/keymap.toml` is loaded without trust — and this
    /// finding goes to a terminal. Unmasked, `"app.\u{202E}tiuq"` PAINTS as
    /// `app.quit`, so the warning names a command the user cannot tell from
    /// the real one; `"app.quit\u{1B}]0;x\u{7}"` sets the terminal title.
    /// `--json` is no refuge: `serde_json` escapes C0 but not U+202E.
    #[test]
    fn a_hostile_keymap_run_comes_out_masked_never_raw() {
        for h in norte_testkit::corpus::hostile_runs() {
            let dir = tempfile::tempdir().unwrap();
            // TOML basic string: escape the hazards the way an attacker would
            // ship them, so the fixture reaches `run` as the bytes it names.
            let escaped: String = h
                .run
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '.' {
                        c.to_string()
                    } else {
                        format!("\\u{:04X}", c as u32)
                    }
                })
                .collect();
            std::fs::write(
                dir.path().join("keymap.toml"),
                format!("[pane]\nappend_keymap = [{{ on = [\"z\"], run = \"{escaped}\" }}]\n"),
            )
            .unwrap();
            let layers = Layers {
                dirs: vec![(dir.path().to_path_buf(), Layer::User)],
            };
            for f in check_keymaps(&layers) {
                assert!(
                    !f.detail.chars().any(norte_encoding::is_terminal_hazard),
                    "{}: {} came out raw — {}",
                    h.id,
                    f.detail.escape_debug(),
                    h.why
                );
            }
        }
    }

    /// A `lua:<name>` binding whose name fails the charset (`valid_lua_name`)
    /// can NEVER be fixed by adding it to the known-commands set. The one-pass
    /// [`Effective::build_diagnostics`] (#102) classifies it directly as a
    /// single structural `Error` — the old retry-with-known-name loop had to
    /// special-case it to avoid burning its whole budget into a spurious
    /// `keymap-too-many-unknown-commands`; that code and its escalation are
    /// gone, and this test guards that they stay gone.
    #[test]
    fn keymap_invalid_lua_charset_is_a_single_structural_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("keymap.toml"),
            "[pane]\nappend_keymap = [{ on = [\"z\"], run = \"lua:bad name!\" }]\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let findings = check_keymaps(&layers);
        // Exactly one Error finding for this screen, no cap escalation.
        let errors: Vec<_> = findings
            .iter()
            .filter(|f| f.section == "keymap" && f.severity == Severity::Error)
            .collect();
        assert_eq!(errors.len(), 1, "{findings:?}");
        assert_eq!(errors[0].code, "keymap-structural");
        assert!(
            errors[0].detail.contains("lua:bad name!"),
            "{}",
            errors[0].detail
        );
        assert!(
            !findings
                .iter()
                .any(|f| f.code == "keymap-too-many-unknown-commands"),
            "must terminate fast, not exhaust the retry cap: {findings:?}"
        );
    }

    /// Minimal valid manifest shape (copied from `norte-core`'s own
    /// `DEMO_MANIFEST` test const, `crates/norte-core/src/plugins.rs`).
    const DEMO_MANIFEST: &str = r#"
[plugin]
id = "org.norte.demo"
name = "Demo"
publisher = "norte"
version = "0.1.0"
category = "command"
[capabilities]
fs-read = "scoped"
"#;

    fn write_plugin(config_dir: &std::path::Path, id: &str, src: &str) {
        let dir = config_dir.join("plugins").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plugin.toml"), src).unwrap();
    }

    /// TDD: a discovered plugin with a valid manifest but no `plugin.wasm`
    /// on disk → `Ok` for the plugin itself, `Warn` `plugin-no-binary`.
    #[test]
    fn plugins_valid_manifest_without_wasm_is_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "org.norte.demo", DEMO_MANIFEST);

        let findings = check_plugins(dir.path());
        assert!(
            !findings.iter().any(|f| f.severity == Severity::Error),
            "{findings:?}"
        );
        let ok = findings
            .iter()
            .find(|f| f.code == "plugin-ok")
            .unwrap_or_else(|| panic!("expected a plugin-ok finding: {findings:?}"));
        assert!(ok.detail.contains("org.norte.demo"), "{}", ok.detail);
        let warn = findings
            .iter()
            .find(|f| f.code == "plugin-no-binary")
            .unwrap_or_else(|| panic!("expected a no-binary finding: {findings:?}"));
        assert_eq!(warn.severity, Severity::Warn);
        assert!(warn.detail.contains("org.norte.demo"), "{}", warn.detail);
    }

    /// Builds the `previewer-demo` guest of `norte-plugin-host` (SKIP without
    /// the `wasm32-wasip2` target) and returns its bytes.
    fn demo_guest_bytes() -> Option<Vec<u8>> {
        use std::process::Command;
        let installed = Command::new("rustup")
            .args(["target", "list", "--installed"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .is_some_and(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .any(|l| l == "wasm32-wasip2")
            });
        if !installed {
            eprintln!("SKIP: target wasm32-wasip2 not installed");
            return None;
        }
        let guest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../norte-plugin-host/examples-wasm/previewer-demo");
        // `CARGO_TARGET_TMPDIR` only exists for integration tests; this is a
        // unit test of the binary, so the guest goes under the workspace's
        // `target/` (where `just prune` can see it), never the system temp.
        let target_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp/wasm-guests");
        let status = Command::new(env!("CARGO"))
            .current_dir(&guest_dir)
            .args([
                "build",
                "--release",
                "--target",
                "wasm32-wasip2",
                "--target-dir",
            ])
            .arg(&target_dir)
            .status()
            .expect("cargo build of the guest");
        assert!(status.success(), "previewer-demo did not compile");
        let wasm = target_dir.join("wasm32-wasip2/release/previewer_demo.wasm");
        Some(std::fs::read(wasm).expect("reads the guest"))
    }

    /// A plugin whose binary was built against another WIT is its own
    /// finding — a warning that names the package and both versions — and
    /// NOT the generic broken-manifest error: the author has to rebuild, not
    /// edit (ADR 0094). Made by rewriting `@0.10.0` to `@0.70.0` in the bytes
    /// of the real demo guest (same length, sections stay valid).
    #[test]
    fn a_plugin_from_another_wit_is_its_own_finding() {
        let Some(bytes) = demo_guest_bytes() else {
            return;
        };
        let old: Vec<u8> = {
            let mut out = bytes.clone();
            let (from, to) = (b"@0.10.0", b"@0.70.0");
            let mut i = 0;
            while i + from.len() <= out.len() {
                if &out[i..i + from.len()] == from {
                    out[i..i + from.len()].copy_from_slice(to);
                    i += from.len();
                } else {
                    i += 1;
                }
            }
            out
        };
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "org.norte.demo", DEMO_MANIFEST);
        std::fs::write(dir.path().join("plugins/org.norte.demo/plugin.wasm"), old).unwrap();

        let findings = check_plugins(dir.path());
        let f = findings
            .iter()
            .find(|f| f.code == "plugin-wit-mismatch")
            .unwrap_or_else(|| panic!("expected plugin-wit-mismatch: {findings:?}"));
        assert_eq!(f.severity, Severity::Warn);
        assert!(f.detail.contains("org.norte.demo"), "{}", f.detail);
        assert!(f.detail.contains("norte:plugin@0.70.0"), "{}", f.detail);
        assert!(f.detail.contains("@0.10.0"), "{}", f.detail);
        assert!(
            !findings.iter().any(|f| f.code == "plugin-manifest-broken"),
            "not a broken manifest: {findings:?}"
        );
        assert!(
            !findings.iter().any(|f| f.code == "plugin-ok"),
            "does not load: {findings:?}"
        );
    }

    /// TDD: a broken manifest surfaces via `PluginLoadError` as an `Error`
    /// finding, WITHOUT stopping the valid plugin from also being reported
    /// (mirrors `PluginRegistry::list`'s own best-effort contract).
    #[test]
    fn plugins_broken_manifest_is_error() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "org.norte.demo", DEMO_MANIFEST);
        write_plugin(dir.path(), "broken", "this is not valid toml [ =");

        let findings = check_plugins(dir.path());
        assert!(
            findings.iter().any(|f| f.code == "plugin-ok"),
            "the valid plugin must still be reported: {findings:?}"
        );
        let err = findings
            .iter()
            .find(|f| f.code == "plugin-manifest-broken")
            .unwrap_or_else(|| panic!("expected a manifest-broken finding: {findings:?}"));
        assert_eq!(err.severity, Severity::Error);
        assert!(err.detail.contains("broken"), "{}", err.detail);
    }

    /// TDD: `plugins-state.toml` says `approved = true` with a `digest` that
    /// does not match the current manifest's — `list()`'s effective
    /// `approved` reads back `false` (issue #69's own mechanism) → doctor
    /// must surface that gap as `plugin-digest-stale`, not silently agree
    /// with the (now stale) raw flag.
    #[test]
    fn plugins_stale_digest_is_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "org.norte.demo", DEMO_MANIFEST);
        std::fs::write(
            dir.path().join("plugins-state.toml"),
            "[plugins]\n\"org.norte.demo\" = { approved = true, digest = \"stale-digest\" }\n",
        )
        .unwrap();

        let findings = check_plugins(dir.path());
        let warn = findings
            .iter()
            .find(|f| f.code == "plugin-digest-stale")
            .unwrap_or_else(|| panic!("expected a digest-stale finding: {findings:?}"));
        assert_eq!(warn.severity, Severity::Warn);
        assert!(warn.detail.contains("org.norte.demo"), "{}", warn.detail);
        // The plugin-ok finding must reflect the EFFECTIVE (not raw) state.
        let ok = findings
            .iter()
            .find(|f| f.code == "plugin-ok")
            .unwrap_or_else(|| panic!("expected a plugin-ok finding: {findings:?}"));
        assert!(ok.detail.contains("approved=false"), "{}", ok.detail);
    }

    /// Manifest declaring `[config]` (P2 Task 2), for the `plugin-config`
    /// finding tests below.
    const CONFIG_MANIFEST: &str = r#"
[plugin]
id = "org.norte.cfg"
name = "Cfg"
publisher = "norte"
version = "0.1.0"
category = "command"
[config.greeting]
type = "string"
default = "hola"
[config.retries]
type = "int"
default = 3
min = 0
max = 10
"#;

    fn write_config_values(dir: &std::path::Path, id: &str, toml: &str) {
        std::fs::write(dir.join("plugins").join(id).join("config.toml"), toml).unwrap();
    }

    /// TDD (P2 Task 2, decision 5): a plugin declaring `[config]` with no
    /// `config.toml` on disk → one `Severity::Ok` `plugin-config` finding
    /// PER KEY, showing the DEFAULT (`key=value`, id-prefixed).
    #[test]
    fn plugins_config_without_a_file_shows_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "org.norte.cfg", CONFIG_MANIFEST);

        let findings = check_plugins(dir.path());
        let config_findings: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.code == "plugin-config")
            .collect();
        assert_eq!(config_findings.len(), 2, "{findings:?}");
        assert!(config_findings.iter().all(|f| f.severity == Severity::Ok));
        assert!(
            config_findings
                .iter()
                .any(|f| f.detail == "org.norte.cfg: greeting=hola"),
            "{config_findings:?}"
        );
        assert!(
            config_findings
                .iter()
                .any(|f| f.detail == "org.norte.cfg: retries=3"),
            "{config_findings:?}"
        );
    }

    /// A valid override in `config.toml` is reflected in the finding's
    /// value, not the schema default.
    #[test]
    fn plugins_config_with_override_shows_the_effective_value() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "org.norte.cfg", CONFIG_MANIFEST);
        write_config_values(dir.path(), "org.norte.cfg", "retries = 9\n");

        let findings = check_plugins(dir.path());
        assert!(
            findings
                .iter()
                .any(|f| f.code == "plugin-config" && f.detail == "org.norte.cfg: retries=9"),
            "{findings:?}"
        );
    }

    /// A plugin with NO `[config]` schema gets no `plugin-config` findings
    /// at all (empty settings map, decision 5 — nothing to show).
    #[test]
    fn plugins_without_config_have_no_plugin_config_findings() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "org.norte.demo", DEMO_MANIFEST);

        let findings = check_plugins(dir.path());
        assert!(!findings.iter().any(|f| f.code == "plugin-config"));
    }

    /// TDD (P2 Task 2, fail-closed): a `config.toml` that fails validation
    /// against the manifest's `[config]` schema excludes the WHOLE plugin —
    /// it surfaces via the EXISTING `plugin-manifest-broken` catalog-error
    /// path (mirrors `plugins_broken_manifest_is_error`), not as a
    /// `plugin-config`/`plugin-ok` finding. The error names the KEY, never
    /// the value (#73).
    #[test]
    fn plugins_invalid_config_toml_excludes_the_plugin_and_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "org.norte.cfg", CONFIG_MANIFEST);
        write_config_values(dir.path(), "org.norte.cfg", "not-declared = \"x\"\n");

        let findings = check_plugins(dir.path());
        assert!(
            !findings.iter().any(|f| f.code == "plugin-ok"),
            "the invalid plugin must not load: {findings:?}"
        );
        assert!(
            !findings.iter().any(|f| f.code == "plugin-config"),
            "no config values should be shown for an excluded plugin: {findings:?}"
        );
        let err = findings
            .iter()
            .find(|f| f.code == "plugin-manifest-broken")
            .unwrap_or_else(|| panic!("expected a manifest-broken finding: {findings:?}"));
        assert_eq!(err.severity, Severity::Error);
        assert!(err.detail.contains("not-declared"), "{}", err.detail);
    }

    /// P2 Task 4a security review: an UNKNOWN key from a hostile
    /// `config.toml` (ESC/bidi override) must never reach the
    /// `plugin-manifest-broken` finding raw — unlike a manifest-declared
    /// key, a `config.toml` key has no charset guarantee, and the message
    /// crosses both `norte doctor`'s stdout and the wire
    /// (`PluginLoadError.reason`).
    #[test]
    fn plugins_manifest_broken_with_hostile_key_is_masked() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "org.norte.cfg", CONFIG_MANIFEST);
        // Quoted TOML key with an ESC + an RLO (bidi) override — neither
        // is valid in the [a-z0-9-]{1,32} charset every DECLARED key
        // requires, so this is pure user TOML.
        write_config_values(
            dir.path(),
            "org.norte.cfg",
            "\"mal\\u001b\\u202eicious\" = \"x\"\n",
        );

        let findings = check_plugins(dir.path());
        let err = findings
            .iter()
            .find(|f| f.code == "plugin-manifest-broken")
            .unwrap_or_else(|| panic!("expected a manifest-broken finding: {findings:?}"));
        assert!(
            !err.detail.contains('\u{1b}') && !err.detail.contains('\u{202e}'),
            "raw ESC/RLO must never reach the finding: {}",
            err.detail
        );
    }

    /// Encoding audit (H2-style, same policy as elsewhere in this file):
    /// a config value carrying a terminal hazard (ESC) must never reach the
    /// finding's `detail` raw — it is masked to U+FFFD.
    #[test]
    fn plugins_config_value_with_terminal_hazard_is_masked() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "org.norte.cfg", CONFIG_MANIFEST);
        write_config_values(
            dir.path(),
            "org.norte.cfg",
            "greeting = \"ok \\u001bmalicious\"\n",
        );

        let findings = check_plugins(dir.path());
        let f = findings
            .iter()
            .find(|f| f.code == "plugin-config" && f.detail.starts_with("org.norte.cfg: greeting"))
            .unwrap_or_else(|| panic!("expected a greeting plugin-config finding: {findings:?}"));
        assert!(
            !f.detail.contains('\u{1b}'),
            "raw ESC must never reach the finding: {}",
            f.detail
        );
        assert!(f.detail.contains('\u{fffd}'), "{}", f.detail);
    }

    /// A config value longer than the display cap is truncated.
    #[test]
    fn plugins_config_long_value_is_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "org.norte.cfg", CONFIG_MANIFEST);
        let long = "a".repeat(200);
        write_config_values(
            dir.path(),
            "org.norte.cfg",
            &format!("greeting = \"{long}\"\n"),
        );

        let findings = check_plugins(dir.path());
        let f = findings
            .iter()
            .find(|f| f.code == "plugin-config" && f.detail.starts_with("org.norte.cfg: greeting"))
            .unwrap_or_else(|| panic!("expected a greeting plugin-config finding: {findings:?}"));
        assert!(
            f.detail.chars().count() < 200,
            "the long value must be capped: {}",
            f.detail
        );
    }

    /// A minimal valid manifest for an arbitrary `id` — the `plugin-help`
    /// tests below need the id to be the thing under test (`acme.ftp` for the
    /// detail, `copying` for the corpus collision).
    fn manifest_for(id: &str) -> String {
        format!(
            "[plugin]\nid = \"{id}\"\nname = \"X\"\npublisher = \"norte\"\n\
             version = \"0.1.0\"\ncategory = \"command\"\n[capabilities]\nfs-read = \"scoped\"\n"
        )
    }

    /// Lays down `help.md` (raw BYTES: some fixtures are deliberately not
    /// valid UTF-8) inside an already-written plugin directory.
    fn write_help(config_dir: &std::path::Path, dir: &str, bytes: &[u8]) {
        std::fs::write(config_dir.join("plugins").join(dir).join("help.md"), bytes).unwrap();
    }

    /// TDD (H3e): a `help.md` past the untrusted cap is served cut short, and
    /// the author only finds out here.
    #[test]
    fn a_truncated_help_md_comes_out_as_a_finding() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "acme.ftp", &manifest_for("acme.ftp"));
        // Over `Limits::untrusted().max_bytes` (64 KiB), and valid UTF-8 so
        // the ONLY flag this fixture raises is `truncated`.
        write_help(dir.path(), "acme.ftp", &vec![b'a'; 70 * 1024]);

        let f = check_plugins(dir.path())
            .into_iter()
            .find(|f| f.code == "plugin-help-truncated")
            .expect("se reporta el recorte");
        assert_eq!(f.severity, Severity::Warn);
        assert!(f.detail.contains("acme.ftp"), "detail: {}", f.detail);
    }

    /// TDD (H3e): bytes that decode under no reading the parser is willing to
    /// make. The fixture needs a UTF-8 BOM — `norte_encoding::detect` recovers
    /// bare high bytes as a legacy encoding, so only a BOM makes the encoding
    /// a CERTAINTY and the following invalid sequence a genuine loss.
    #[test]
    fn a_help_md_with_non_decoding_bytes_comes_out_as_a_finding() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "acme.ftp", &manifest_for("acme.ftp"));
        write_help(dir.path(), "acme.ftp", b"\xef\xbb\xbf# T\xc3\x28tulo\n");

        let f = check_plugins(dir.path())
            .into_iter()
            .find(|f| f.code == "plugin-help-lossy")
            .expect("the loss is reported");
        assert_eq!(f.severity, Severity::Warn);
        assert!(f.detail.contains("acme.ftp"), "detail: {}", f.detail);
    }

    /// TDD (H3e): the header declares a command the plugin does not own. The
    /// parser drops that row in silence, so this finding is the only place
    /// the author learns of it.
    #[test]
    fn a_help_md_with_foreign_commands_comes_out_as_a_finding() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "acme.ftp", &manifest_for("acme.ftp"));
        write_help(
            dir.path(),
            "acme.ftp",
            b"+++\nid = \"acme.ftp\"\ntitle = \"FTP\"\ncommands = [\"fs.copy\"]\n+++\nbody\n",
        );

        let f = check_plugins(dir.path())
            .into_iter()
            .find(|f| f.code == "plugin-help-foreign-command")
            .expect("se reporta el comando ajeno");
        assert_eq!(f.severity, Severity::Warn);
        assert!(f.detail.contains("fs.copy"), "detail: {}", f.detail);
    }

    /// TDD (H3e, addition B): the mistake that actually happens — a header
    /// declaring `title` and `commands` but NOT the required `id`. The whole
    /// header is then read as prose, and the asymmetry pinned here is the
    /// argument for the finding: `foreign_commands` cannot see the `fs.copy`
    /// it declares (there is no parsed header to read a list from), so a
    /// broken header must NOT show up as a foreign-command defect, and
    /// without `plugin-help-bad-header` it would not show up at all.
    #[test]
    fn a_help_md_with_a_broken_header_comes_out_as_a_finding() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "acme.ftp", &manifest_for("acme.ftp"));
        write_help(
            dir.path(),
            "acme.ftp",
            b"+++\ntitle = \"FTP\"\ncommands = [\"fs.copy\"]\n+++\nbody\n",
        );

        let findings = check_plugins(dir.path());
        let f = findings
            .iter()
            .find(|f| f.code == "plugin-help-bad-header")
            .unwrap_or_else(|| panic!("expected a bad-header finding: {findings:?}"));
        assert_eq!(f.severity, Severity::Warn);
        assert!(f.detail.contains("acme.ftp"), "detail: {}", f.detail);
        assert!(
            !findings
                .iter()
                .any(|f| f.code == "plugin-help-foreign-command"),
            "a header that fails to parse has no command list to check: {findings:?}"
        );
    }

    /// H3e: `plugin-help-shadows-topic` guards the case where a plugin's id
    /// is also a corpus page id — the frontend discards that node (the corpus
    /// wins, fail-closed) and the page would vanish without saying why.
    ///
    /// It CANNOT fire today, and this test is what pins the two invariants
    /// that make it unreachable, so that relaxing either one fails here
    /// instead of silently re-opening the hole:
    ///
    /// 1. a `plugin.id` must be reverse-DNS — at least two `[A-Za-z0-9-]`
    ///    segments separated by a dot (`norte_plugin_host`'s own
    ///    `is_valid_plugin_id`); a manifest claiming a bare `copying` does not
    ///    even load, it surfaces as `plugin-manifest-broken`;
    /// 2. every corpus id is a single segment, with no dot in it.
    ///
    /// The finding stays wired regardless: it costs one lookup, and "the id
    /// spaces cannot overlap" is a property of two crates that neither of them
    /// promises to the other.
    #[test]
    fn an_id_collision_with_the_corpus_is_structurally_impossible_today() {
        for id in norte_help::topic_ids(norte_help::Lang::En) {
            assert!(
                !id.as_str().contains('.'),
                "a corpus id with a dot COULD be claimed by a plugin: {id:?} — \
                 write the real fixture test for plugin-help-shadows-topic"
            );
        }

        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "copying", &manifest_for("copying"));
        write_help(dir.path(), "copying", b"# Copying\n\nprose\n");

        let findings = check_plugins(dir.path());
        assert!(
            findings
                .iter()
                .any(|f| f.code == "plugin-manifest-broken" && f.severity == Severity::Error),
            "a bare (non reverse-DNS) id must not load at all: {findings:?}"
        );
        assert!(
            !findings.iter().any(|f| f.code.starts_with("plugin-help")),
            "an excluded plugin has no help findings: {findings:?}"
        );
    }

    /// TDD (H3e, addition A): `announces_help` is one `is_file` at discovery
    /// while the CONTENT is read later through the escape guard, so a `help.md`
    /// symlinked OUT of the plugin's own directory announces a page and serves
    /// nothing. From the reader's side that is indistinguishable from an author
    /// who wrote nothing, which is exactly why it needs a finding. The wire's
    /// `PluginInfo.has_help` is the STRICT flag and reports `false` here — this
    /// finding is the only thing that surfaces the case at all.
    #[cfg(unix)]
    #[test]
    fn a_help_md_that_escapes_the_plugin_directory_comes_out_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "acme.ftp", &manifest_for("acme.ftp"));
        let outside = dir.path().join("secreto.md");
        std::fs::write(&outside, "# no es suyo\n").unwrap();
        std::os::unix::fs::symlink(
            &outside,
            dir.path().join("plugins").join("acme.ftp").join("help.md"),
        )
        .unwrap();

        let findings = check_plugins(dir.path());
        let f = findings
            .iter()
            .find(|f| f.code == "plugin-help-empty")
            .unwrap_or_else(|| panic!("expected a plugin-help-empty finding: {findings:?}"));
        assert_eq!(f.severity, Severity::Warn);
        assert!(f.detail.contains("acme.ftp"), "detail: {}", f.detail);
    }

    /// Not documenting yourself is not a defect: a plugin without `help.md`
    /// must produce no `plugin-help` noise at all.
    #[test]
    fn a_plugin_without_help_md_generates_no_noise() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "org.norte.demo", DEMO_MANIFEST);

        assert!(
            !check_plugins(dir.path())
                .iter()
                .any(|f| f.code.starts_with("plugin-help")),
            "no documentarse no es un defecto"
        );
    }

    /// …and neither is documenting yourself WELL: a clean `help.md` declaring
    /// only its own commands is silent too.
    #[test]
    fn a_clean_help_md_generates_no_noise() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "acme.ftp", &manifest_for("acme.ftp"));
        write_help(
            dir.path(),
            "acme.ftp",
            b"+++\nid = \"acme.ftp\"\ntitle = \"FTP\"\n\
              commands = [\"plugin:acme.ftp:connect\"]\n+++\nprosa\n",
        );

        let findings = check_plugins(dir.path());
        assert!(
            !findings.iter().any(|f| f.code.starts_with("plugin-help")),
            "a correct help.md is not a finding: {findings:?}"
        );
    }

    /// TDD (decision 2): a `Password`-auth connection with the secret env
    /// var absent → `Warn` naming the var; present (via an injected env
    /// closure) → `Ok`. Never the secret VALUE, only presence.
    #[test]
    fn connections_secret_env_absent_and_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("connections.toml"),
            "[connections.backup]\nurl = \"ftp://backup@ftp.example.com\"\nauth = \"password\"\n",
        )
        .unwrap();

        let findings = check_connections(dir.path(), &env(&[]));
        assert!(
            !findings.iter().any(|f| f.severity == Severity::Error),
            "{findings:?}"
        );
        let warn = findings
            .iter()
            .find(|f| f.code == "conn-secret-env-absent")
            .unwrap_or_else(|| panic!("expected an env-absent finding: {findings:?}"));
        assert_eq!(warn.severity, Severity::Warn);
        assert!(
            warn.detail.contains("NORTE_SECRET_BACKUP"),
            "{}",
            warn.detail
        );

        let vars = [("NORTE_SECRET_BACKUP", "irrelevant-marker")];
        let findings2 = check_connections(dir.path(), &env(&vars));
        assert!(
            !findings2.iter().any(|f| f.code == "conn-secret-env-absent"),
            "{findings2:?}"
        );
        let ok = findings2
            .iter()
            .find(|f| f.code == "conn-secret-env-present")
            .unwrap_or_else(|| panic!("expected an env-present finding: {findings2:?}"));
        assert_eq!(ok.severity, Severity::Ok);
    }

    /// #325: with `secret = "prompt"` the absence is EXPECTED — norte is
    /// going to ask for it — so it is `Ok` with its own code and not the
    /// `Warn` above. Warning here would be the same lie #320 came to
    /// remove, the other way around: a warning about the one
    /// configuration with nothing broken.
    #[test]
    fn a_connection_with_prompt_does_not_warn_about_the_absent_variable() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("connections.toml"),
            "[connections.backup]\nurl = \"ftp://backup@ftp.example.com\"\n\
             auth = \"password\"\nsecret = \"prompt\"\n",
        )
        .unwrap();

        let findings = check_connections(dir.path(), &env(&[]));
        assert!(
            !findings.iter().any(|f| f.code == "conn-secret-env-absent"),
            "with prompt there is no warning for the absence: {findings:?}"
        );
        let ok = findings
            .iter()
            .find(|f| f.code == "conn-secret-prompt")
            .unwrap_or_else(|| panic!("expected a prompt finding: {findings:?}"));
        assert_eq!(ok.severity, Severity::Ok);
        assert!(ok.detail.contains("NORTE_SECRET_BACKUP"), "{}", ok.detail);

        // And an EMPTY variable is still Error even with prompt: #320
        // comes first — a variable set to empty is a configuration
        // failure, not a way to ask for the dialog.
        let empty = check_connections(dir.path(), &env(&[("NORTE_SECRET_BACKUP", "")]));
        let err = empty
            .iter()
            .find(|f| f.code == "conn-secret-env-empty")
            .unwrap_or_else(|| panic!("expected an env-empty finding: {empty:?}"));
        assert_eq!(err.severity, Severity::Error);
    }

    /// TDD (#320): the var set but EMPTY is neither present nor absent — it is
    /// the failure that cost a real debugging session, because `is_some()`
    /// reported it as `Ok` while the connection silently degraded to the
    /// ambient credential chain. `Error`, not `Warn`: after #320 the resolver
    /// rejects it and no later step can rescue it, so a `norte doctor`
    /// preflight must not exit 0 on it.
    #[test]
    fn connections_empty_secret_env_does_not_count_as_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("connections.toml"),
            "[connections.backup]\nurl = \"ftp://backup@ftp.example.com\"\nauth = \"password\"\n",
        )
        .unwrap();

        let findings = check_connections(dir.path(), &env(&[("NORTE_SECRET_BACKUP", "")]));
        assert!(
            !findings.iter().any(|f| f.code == "conn-secret-env-present"),
            "an empty value is not a present secret: {findings:?}"
        );
        let f = findings
            .iter()
            .find(|f| f.code == "conn-secret-env-empty")
            .unwrap_or_else(|| panic!("expected an env-empty finding: {findings:?}"));
        assert_eq!(f.severity, Severity::Error);
        assert!(f.detail.contains("NORTE_SECRET_BACKUP"), "{}", f.detail);
    }

    /// TDD (#320, rust review MAJOR-4): the doctor reads the env with `var_os`
    /// and the resolver with `var_os` + `into_string`. Before this, the doctor
    /// used the presence of the raw `OsString` alone and reported a non-UTF-8
    /// password as `Ok` while the resolver skipped it entirely and went on to
    /// the keyring — set-but-unusable, certified fine. Two readers, one byte
    /// policy.
    #[cfg(unix)]
    #[test]
    fn connections_non_utf8_secret_env_does_not_count_as_present() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("connections.toml"),
            "[connections.backup]\nurl = \"ftp://backup@ftp.example.com\"\nauth = \"password\"\n",
        )
        .unwrap();

        // 0xFF is never valid UTF-8, in any position.
        let raw = OsString::from(std::ffi::OsStr::from_bytes(b"clave\xffrota"));
        let environment = |k: &str| (k == "NORTE_SECRET_BACKUP").then(|| raw.clone());
        let findings = check_connections(dir.path(), &environment);
        assert!(
            !findings.iter().any(|f| f.code == "conn-secret-env-present"),
            "bytes the resolver cannot read are not a present secret: {findings:?}"
        );
        let f = findings
            .iter()
            .find(|f| f.code == "conn-secret-env-not-utf8")
            .unwrap_or_else(|| panic!("expected an env-not-utf8 finding: {findings:?}"));
        assert_eq!(f.severity, Severity::Error);
        assert!(f.detail.contains("NORTE_SECRET_BACKUP"), "{}", f.detail);
    }

    /// TDD: absent `connections.toml` → a single `Ok`-empty finding, never an
    /// error (connections are optional). Its `detail` is a MACHINE (empty)
    /// value, per review MINOR-3 — the narrative sentence is the text
    /// renderer's job.
    #[test]
    fn absent_connections_is_an_empty_ok() {
        let dir = tempfile::tempdir().unwrap();
        let findings = check_connections(dir.path(), &env(&[]));
        assert!(
            findings.iter().all(|f| f.severity != Severity::Error),
            "{findings:?}"
        );
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].code, "connections-none");
        assert!(findings[0].detail.is_empty(), "{}", findings[0].detail);
    }

    /// A scheme nobody serves — neither the core nor an installed
    /// provider plugin — is a warning naming the connection. It used to
    /// be a parse error; with the parser open to plugin schemes, this is
    /// the only place that can catch a typed `sfpt://`.
    #[test]
    fn a_scheme_nobody_serves_is_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("connections.toml"),
            "[connections.typo]\nurl = \"sfpt://host\"\n[connections.ok]\nurl = \"sftp://host\"\n",
        )
        .unwrap();
        let findings = check_connections(dir.path(), &env(&[]));
        let typo = findings
            .iter()
            .find(|f| f.detail == "typo")
            .expect("the connection with the typo has a finding");
        assert_eq!(typo.code, "connection-scheme-unserved");
        assert_eq!(typo.severity, Severity::Warn);
        let ok = findings
            .iter()
            .find(|f| f.detail == "ok")
            .expect("the good one");
        assert_eq!(ok.code, "connection-ok");

        // Once a provider that declares `sfpt` is installed, it stops being a typo.
        let plugin = dir.path().join("plugins/org.demo.sfpt");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(
            plugin.join("plugin.toml"),
            "[plugin]\nid = \"org.demo.sfpt\"\nname = \"S\"\npublisher = \"d\"\nversion = \"0.1.0\"\ncategory = \"provider\"\n[[contributions.provider]]\nscheme = \"sfpt\"\n",
        )
        .unwrap();
        let findings = check_connections(dir.path(), &env(&[]));
        let typo = findings.iter().find(|f| f.detail == "typo").unwrap();
        assert_eq!(typo.code, "connection-ok");
    }

    /// ADR 0150: `allow_rsa` is an ACCEPTED risk, and `doctor` recalls it
    /// while it stays set (`Warn`, not `Ok`). Where it can do nothing —
    /// auth other than `key`, or a scheme other than sftp — it is said
    /// separately, like `conn-secret-prompt-inert`: a key that does
    /// nothing is not kept quiet.
    #[test]
    fn allow_rsa_warns_and_says_where_it_does_not_apply() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("connections.toml"),
            "[connections.rsa]\nurl = \"sftp://u@h\"\nauth = \"key\"\n\
             key = \"/k\"\nallow_rsa = true\n\
             [connections.pass]\nurl = \"sftp://u@h\"\nauth = \"password\"\nallow_rsa = true\n\
             [connections.s3]\nurl = \"s3://bucket\"\nregion = \"eu-west-1\"\nallow_rsa = true\n\
             [connections.limpia]\nurl = \"sftp://u@h\"\nauth = \"key\"\nkey = \"/k\"\n",
        )
        .unwrap();
        let findings = check_connections(dir.path(), &env(&[]));
        let de = |code: &str| -> Vec<&str> {
            findings
                .iter()
                .filter(|f| f.code == code)
                .inspect(|f| assert_eq!(f.severity, Severity::Warn, "{f:?}"))
                .map(|f| f.detail.as_str())
                .collect()
        };
        assert_eq!(de("conn-rsa-allowed"), ["rsa"], "{findings:?}");
        assert_eq!(de("conn-rsa-allowed-inert"), ["pass", "s3"], "{findings:?}");
    }

    /// TDD: broken `connections.toml` → a single `Error` finding, `detail`
    /// empty (review MINOR-3: machine-only; the sentence is the text
    /// renderer's `cli-doctor-detail-connections-parse`).
    #[test]
    fn connections_broken_toml_is_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("connections.toml"), "esto no es toml [[[").unwrap();
        let findings = check_connections(dir.path(), &env(&[]));
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].code, "connections-parse");
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].detail.is_empty(), "{}", findings[0].detail);
    }

    /// TDD (review MINOR-2, secret hygiene): a syntax error inside a
    /// secret-shaped line (`password = "hunter2` — unterminated string) must
    /// NOT leak through `connections-parse`'s `detail`.
    /// `ConnectError::Config`'s `Display` (`toml`'s own parser) echoes the
    /// offending line/snippet, which could be a real secret a user pasted
    /// straight into `connections.toml` by mistake (rule 10) — this pins
    /// that `check_connections` never propagates it, in `--json` or text.
    #[test]
    fn connections_toml_with_a_broken_secret_does_not_leak_the_value() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("connections.toml"),
            "[connections.x]\nurl = \"sftp://h\"\npassword = \"hunter2\n",
        )
        .unwrap();
        let findings = check_connections(dir.path(), &env(&[]));
        assert_eq!(findings.len(), 1, "{findings:?}");
        let f = &findings[0];
        assert_eq!(f.code, "connections-parse");
        assert_eq!(f.severity, Severity::Error);
        assert!(!f.detail.contains("hunter2"), "{}", f.detail);
        assert!(!f.detail.contains('"'), "{}", f.detail);
    }

    /// The log is what makes a bug report from someone who is not us
    /// possible, so the first thing `doctor` has to say is WHERE it is.
    #[test]
    fn doctor_names_the_log_file() {
        let dir = tempfile::tempdir().expect("tmp");
        let logs = dir.path().join("logs");
        std::fs::create_dir_all(&logs).expect("logs");
        std::fs::write(logs.join("norte.log.2026-08-16"), b"a line\n").expect("log");

        let findings = check_logs(Some(&logs));
        let f = only(&findings, "logs");
        assert_eq!(f.severity, Severity::Ok);
        assert_eq!(f.code, "logs-ok");
        assert!(
            f.detail.contains(&logs.display().to_string()),
            "the row carries the PATH: {}",
            f.detail
        );
    }

    /// A state directory that does not exist is a DEGRADATION, not a
    /// breakage: the machine works, it just leaves no trace. `Error`
    /// would make `norte doctor` exit non-zero for something that breaks
    /// nothing (decision 4).
    #[test]
    fn without_a_state_dir_is_a_warning_not_an_error() {
        let findings = check_logs(None);
        let f = only(&findings, "logs");
        assert_eq!(f.severity, Severity::Warn);
        assert_eq!(f.code, "logs-no-state-dir");
    }

    /// And a directory that cannot be written to is not a breakage
    /// either: it warns and the program starts anyway, which is what
    /// `logging::init_to_file` does.
    #[cfg(unix)]
    #[test]
    fn an_unwritable_directory_warns() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tmp");
        let logs = dir.path().join("logs");
        std::fs::create_dir_all(&logs).expect("logs");
        std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o500)).expect("chmod");

        let findings = check_logs(Some(&logs));
        let f = only(&findings, "logs");
        // Restore before any assert: if it fails, the TempDir still has
        // to be deletable.
        std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        assert_eq!(f.severity, Severity::Warn);
        assert_eq!(f.code, "logs-unwritable");
    }

    /// The single `section` row in `findings`.
    fn only<'a>(findings: &'a [Finding], section: &str) -> &'a Finding {
        let rows: Vec<&Finding> = findings.iter().filter(|f| f.section == section).collect();
        assert_eq!(rows.len(), 1, "one row for {section}: {findings:?}");
        rows[0]
    }
}
