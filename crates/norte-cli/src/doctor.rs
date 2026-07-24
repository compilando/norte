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
//! [`norte_frontend::keymap::preset_commands`]). Plugin and connection
//! checks land in Task 2.

use std::ffi::OsString;

use norte_config::Layers;
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
fn check_keymap_screen(
    preset_kf: &norte_frontend::keymap::KeymapFile,
    layer_kfs: &[norte_frontend::keymap::KeymapFile],
    screen: Screen,
    label: &'static str,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut known = norte_frontend::keymap::preset_commands(screen);
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
                findings.push(Finding {
                    section: "keymap",
                    severity: Severity::Warn,
                    code: "keymap-unknown-cmd",
                    detail: format!("{label}: {run}"),
                });
                if !known.contains(&run) {
                    known.push(run);
                }
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
}
