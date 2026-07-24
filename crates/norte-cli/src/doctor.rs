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
use norte_connect::{AuthMethod, ConnectionsFile};
use norte_frontend::keymap::{Effective, KeymapError, Screen, presets};
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

/// Anti-DoS cap on the unknown-command retry loop below (a hostile/broken
/// layer with hundreds of distinct made-up `run` names must not spin
/// forever) — matches the order of magnitude of a keymap's binding count
/// (`Effective`'s own doc: "a linear scan over ≤ hundreds of bindings").
const MAX_UNKNOWN_COMMAND_RETRIES: usize = 256;

/// Builds the effective keymap for one `screen`, RETRYING past
/// [`KeymapError::UnknownCommand`] from a layer binding (decision 1): each
/// occurrence becomes a [`Severity::Warn`] finding naming the command, and
/// is added to the known-commands set for the next attempt — so a config
/// with several distinct typos gets ALL of them reported, not just the
/// first. Any other [`KeymapError`] is a [`Severity::Error`] finding that
/// stops the screen's check (retrying would not converge).
///
/// One case masquerades as an ordinary unknown command but can NEVER
/// converge by adding it to `known`: a `lua:<name>` binding whose `<name>`
/// fails the Lua identifier charset. `Effective::build_for` validates that
/// charset UNCONDITIONALLY for any `lua:` run string, never consulting
/// `known_commands` (`norte-frontend`'s `keymap.rs`, the `lua:` branch right
/// before the `known_commands.contains` check) — so retrying with the exact
/// same broken name added to `known` reproduces the identical error forever,
/// burning the whole retry budget on one binding and ending in a misleading
/// `keymap-too-many-unknown-commands`. [`norte_frontend::keymap::valid_lua_name`]
/// is the SAME charset check the engine uses (single source), so it is
/// checked directly here to short-circuit that case in one iteration instead
/// of two-hundred-fifty-six; `known.contains(&run)` is kept as a
/// defense-in-depth guard for any OTHER non-convergent case this reasoning
/// missed (issue #102 tracks replacing this whole retry loop with a
/// one-pass diagnostic that can't have this class of bug at all).
fn check_keymap_screen(
    preset_kf: &norte_frontend::keymap::KeymapFile,
    layer_kfs: &[norte_frontend::keymap::KeymapFile],
    screen: Screen,
    label: &'static str,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut known = norte_frontend::keymap::preset_commands(screen);
    // TODO(#102): replace this retry loop with a one-pass
    // `Effective::build_diagnostics`-style API that reports every unknown
    // command in a single walk — would also remove the need for the
    // non-convergence guard below entirely.
    for _ in 0..MAX_UNKNOWN_COMMAND_RETRIES {
        let known_refs: Vec<&str> = known.iter().map(String::as_str).collect();
        match Effective::build_for(preset_kf, layer_kfs, &known_refs, screen) {
            Ok(_) => {
                findings.push(Finding {
                    section: "keymap",
                    severity: Severity::Ok,
                    code: "keymap-ok",
                    detail: label.to_owned(),
                });
                return findings;
            }
            Err(KeymapError::UnknownCommand { run }) => {
                let lua_charset_error = run
                    .strip_prefix("lua:")
                    .is_some_and(|name| !norte_frontend::keymap::valid_lua_name(name));
                if lua_charset_error || known.contains(&run) {
                    findings.push(Finding {
                        section: "keymap",
                        severity: Severity::Error,
                        code: "keymap-structural",
                        detail: format!("{label}: {run}"),
                    });
                    return findings;
                }
                findings.push(Finding {
                    section: "keymap",
                    severity: Severity::Warn,
                    code: "keymap-unknown-cmd",
                    detail: format!("{label}: {run}"),
                });
                known.push(run);
            }
            Err(e) => {
                findings.push(Finding {
                    section: "keymap",
                    severity: Severity::Error,
                    code: "keymap-structural",
                    detail: format!("{label}: {e}"),
                });
                return findings;
            }
        }
    }
    findings.push(Finding {
        section: "keymap",
        severity: Severity::Error,
        code: "keymap-too-many-unknown-commands",
        detail: label.to_owned(),
    });
    findings
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
/// manifest — [`PluginRegistry::list`]'s own doc). Comparing it against the
/// RAW persisted flag from [`PluginRegistry::state_snapshot`] (the state file
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
/// check inside one, rule 2) — against [`PluginRegistry::wasm_path`] (review
/// MINOR-4: was a raw `config_dir.join(...)` duplicate of that layout here
/// before). No symlink-safety canonicalization here, unlike the registry's
/// own `verified_wasm` (issue #69) — this is a presence check for a
/// diagnostic, not a load path, so that defense does not apply.
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
    for err in &list.errors {
        findings.push(Finding {
            section: "plugins",
            severity: Severity::Error,
            code: "plugin-manifest-broken",
            detail: format!("{}: {}", err.dir, err.reason),
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
    }
    findings
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
/// `password_inline_en_url_rechazado_sin_eco`/`scheme_invalido_con_password_inline_no_eco`
/// tests pin that); `Agent`/`Key` auth need no secret and are
/// [`Severity::Ok`]; `Password`/`AccessKey` (secret-bearing, decision 2) are
/// checked for [`norte_connect::env_key`]'s var via `env` — present is
/// [`Severity::Ok`], absent is [`Severity::Warn`] NAMING THE VAR (never a
/// value). The "keyring/age not probed" note is NOT a finding — it is a
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
        for (name, spec) in &file.connections {
            match spec.endpoint() {
                Ok(_) => findings.push(Finding {
                    section: "connections",
                    severity: Severity::Ok,
                    code: "connection-ok",
                    detail: name.clone(),
                }),
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
                AuthMethod::Agent | AuthMethod::Key => {}
                AuthMethod::Password | AuthMethod::AccessKey => {
                    let var = norte_connect::env_key(name);
                    if env(&var).is_some() {
                        findings.push(Finding {
                            section: "connections",
                            severity: Severity::Ok,
                            code: "conn-secret-env-present",
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

#[cfg(test)]
mod tests {
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

    /// TDD: valid layers → every finding is `Ok`; a broken `norte.toml` in
    /// one layer → an `Error` finding carrying the culprit path.
    #[test]
    fn config_ok_y_toml_roto() {
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

        let roto = tempfile::tempdir().unwrap();
        std::fs::write(roto.path().join("norte.toml"), "esto no es toml [[[").unwrap();
        let layers = Layers {
            dirs: vec![(roto.path().to_path_buf(), Layer::User)],
        };
        let findings = check_config(&layers, &env(&[]));
        let err = findings
            .iter()
            .find(|f| f.severity == Severity::Error)
            .unwrap_or_else(|| panic!("expected an Error finding: {findings:?}"));
        assert_eq!(err.code, "config-parse");
        assert!(
            err.detail
                .contains(&roto.path().join("norte.toml").display().to_string()),
            "detail must name the culprit path: {}",
            err.detail
        );
    }

    /// TDD (decision 3): `NORTE_CONFIG_DIR` set + a legacy dir (resolved
    /// via `XDG_CONFIG_HOME` once the override is omitted) that ALSO has a
    /// `norte.toml` → a `Warn` split-brain finding.
    #[test]
    fn split_brain_avisa() {
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
    fn keymap_prefijo_ambiguo_es_error() {
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
    fn keymap_comando_desconocido_es_aviso() {
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

    /// TDD (review IMPORTANT-1): a `lua:<name>` binding whose name fails the
    /// charset (`valid_lua_name`) can NEVER be fixed by adding it to the
    /// known-commands set — the retry loop must detect that directly and
    /// stop in ONE iteration with a single `Error`, not spin through the
    /// whole retry budget into a spurious `keymap-too-many-unknown-commands`.
    #[test]
    fn keymap_lua_charset_invalido_no_reintenta_hasta_agotar() {
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
    fn plugins_manifest_valido_sin_wasm_es_aviso() {
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

    /// TDD: a broken manifest surfaces via `PluginLoadError` as an `Error`
    /// finding, WITHOUT stopping the valid plugin from also being reported
    /// (mirrors `PluginRegistry::list`'s own best-effort contract).
    #[test]
    fn plugins_manifest_roto_es_error() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "org.norte.demo", DEMO_MANIFEST);
        write_plugin(dir.path(), "roto", "esto no es toml [ valido =");

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
        assert!(err.detail.contains("roto"), "{}", err.detail);
    }

    /// TDD: `plugins-state.toml` says `approved = true` with a `digest` that
    /// does not match the current manifest's — `list()`'s effective
    /// `approved` reads back `false` (issue #69's own mechanism) → doctor
    /// must surface that gap as `plugin-digest-stale`, not silently agree
    /// with the (now stale) raw flag.
    #[test]
    fn plugins_digest_obsoleto_es_aviso() {
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

    /// TDD (decision 2): a `Password`-auth connection with the secret env
    /// var absent → `Warn` naming the var; present (via an injected env
    /// closure) → `Ok`. Never the secret VALUE, only presence.
    #[test]
    fn conexiones_secreto_env_ausente_y_presente() {
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

    /// TDD: absent `connections.toml` → a single `Ok`-empty finding, never an
    /// error (connections are optional). Its `detail` is a MACHINE (empty)
    /// value, per review MINOR-3 — the narrative sentence is the text
    /// renderer's job.
    #[test]
    fn conexiones_ausentes_es_ok_vacio() {
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

    /// TDD: broken `connections.toml` → a single `Error` finding, `detail`
    /// empty (review MINOR-3: machine-only; the sentence is the text
    /// renderer's `cli-doctor-detail-connections-parse`).
    #[test]
    fn conexiones_toml_roto_es_error() {
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
    fn conexiones_toml_con_secreto_roto_no_filtra_el_valor() {
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
}
