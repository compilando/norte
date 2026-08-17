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

/// `[ui] layout`: el fichero nombrado tiene que existir y describir un árbol
/// coherente.
///
/// Es `Warn` y no `Error` por la misma razón por la que el arranque no muere:
/// un layout que no carga cae a `orthodox`, así que norte sigue siendo usable
/// — pero en silencio el usuario habría creído que su disposición se aplicó.
#[must_use]
pub fn check_layout(layers: &Layers) -> Vec<Finding> {
    let Ok(cfg) = norte_config::load(layers) else {
        return Vec::new(); // el parse ya lo reporta check_config
    };
    let Some(nombre) = cfg.ui_layout.as_deref() else {
        return Vec::new();
    };
    if nombre == "orthodox" {
        return vec![Finding {
            section: "layout",
            severity: Severity::Ok,
            code: "layout-builtin",
            detail: nombre.to_owned(),
        }];
    }
    // El layout es del USUARIO: un layout de sistema o de proyecto podría
    // repartir la pantalla de alguien que no lo escribió.
    let Some((dir, _)) = layers
        .dirs
        .iter()
        .find(|(_, l)| matches!(l, norte_config::Layer::User))
    else {
        return Vec::new();
    };
    match norte_frontend::layout::config::load(dir, nombre) {
        Ok(_) => vec![Finding {
            section: "layout",
            severity: Severity::Ok,
            code: "layout-ok",
            detail: nombre.to_owned(),
        }],
        Err(e) => vec![Finding {
            section: "layout",
            severity: Severity::Warn,
            code: "layout-unusable",
            detail: format!("{nombre}: {e} (se arrancará con «orthodox»)"),
        }],
    }
}

/// `[ui.columns]` (#108 b4): ids que no parsean = Warn (se saltan al
/// pintar — «un id configurado que desaparece en silencio es un bug, no
/// una degradación», spec de columnas §Diagnostics). Los `plugin:` y los
/// `attr:` se pintan ambos por el funnel (#117 y su follow-up): sus
/// diagnósticos restantes son los caps y la legalidad wire de los attrs.
#[must_use]
pub fn check_columns(layers: &Layers) -> Vec<Finding> {
    let Ok(cfg) = norte_config::load(layers) else {
        return Vec::new(); // el parse ya lo reporta check_config
    };
    let st = norte_frontend::columns::ColumnsSettings::resolve(&cfg.ui_columns);
    let mut findings = Vec::new();
    for raw in &st.invalid {
        findings.push(Finding {
            section: "config",
            severity: Severity::Warn,
            code: "columns-bad-id",
            detail: format!(
                "[ui.columns] id no reconocido (se salta al pintar): {}",
                sanitize_detail(raw)
            ),
        });
    }
    // #117-follow-up: las celdas `plugin:` ya se pintan — `columns-no-
    // renderer` se retira; el único diagnóstico que les queda es el cap
    // (espejo de `columns-attrs-over-cap`).
    for raw in &st.plugins_over_cap {
        findings.push(Finding {
            section: "config",
            severity: Severity::Warn,
            code: "columns-plugins-over-cap",
            detail: format!(
                "[ui.columns] columna de plugin por encima del cap de {} por lista (ni se pinta ni se pide): {}",
                norte_frontend::columns::PLUGIN_COLUMNS_MAX_REQUEST,
                sanitize_detail(raw)
            ),
        });
    }
    // #117: un `attr:` por encima del cap de petición por lista — el funnel
    // no lo pinta ni lo pide (pintado == pedido), así que doctor es quien
    // lo cuenta.
    for raw in &st.attrs_over_cap {
        findings.push(Finding {
            section: "config",
            severity: Severity::Warn,
            code: "columns-attrs-over-cap",
            detail: format!(
                "[ui.columns] attr por encima del cap de {} por lista (ni se pinta ni se pide): {}",
                norte_proto::attrs::ATTRS_MAX_REQUEST,
                sanitize_detail(raw)
            ),
        });
    }
    // #117 encoding-audit M1: un `attr:` que parsea como columna pero cuyo
    // id no es legal en el wire (`is_valid_attr_id`: minúsculas con
    // namespace) — el funnel lo salta y el pane no lo pide (pedido a un
    // daemon sería -32602 y tumbaría el fs.list entero): doctor lo nombra
    // porque el id "parece" bien y nada más lo cuenta.
    for raw in &st.attrs_not_wire_safe {
        findings.push(Finding {
            section: "config",
            severity: Severity::Warn,
            code: "columns-attr-id-not-wire-safe",
            detail: format!(
                "[ui.columns] attr que parsea pero no es un id legal del wire (minúsculas con namespace, p. ej. posix.mode — la columna se salta): {}",
                sanitize_detail(raw)
            ),
        });
    }
    // #108 7b: un `[[ui.columns.spec]]` con id imposible o con un formato
    // que no casa con su columna (p. ej. `iec` en mtime) — se aplicó el
    // default al pintar, jamás un drop mudo.
    for raw in &st.bad_specs {
        findings.push(Finding {
            section: "config",
            severity: Severity::Warn,
            code: "columns-bad-spec",
            detail: format!(
                "[ui.columns.spec] id imposible o formato que no casa con su columna (se aplica el default al pintar): {}",
                sanitize_detail(raw)
            ),
        });
    }
    findings
}

/// Un id de columna —o un `run` de keymap— viene de un TOML del usuario pero
/// puede llegar por copy-paste hostil: enmascarado + tope, jamás crudo en la
/// salida. El tope de 64 chars es holgado para ambos: el nombre de comando
/// más largo del catálogo compartido no llega a 24.
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
    for err in &list.errors {
        findings.push(Finding {
            section: "plugins",
            severity: Severity::Error,
            code: "plugin-manifest-broken",
            // security review P2 Task 4a: `err.reason` can carry untrusted
            // text (a `config.toml` values error names the offending KEY,
            // which is user TOML and — unlike a manifest-declared key — has
            // no charset guarantee; a manifest parse error can likewise
            // quote hostile bytes). Masked + capped at the SAME boundary as
            // `plugin-config` below, not just relying on the source-side
            // fix in `ConfigValueError` (defense in depth: this loop also
            // covers `ManifestError` variants that pre-date that fix).
            detail: format!("{}: {}", err.dir, masked_and_capped(&err.reason)),
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

/// El estado del log local (roadmap ítem 9): dónde está, cuánto ocupa, y si se
/// puede escribir en él.
///
/// **Es la fila que convierte «hay logs» en «alguien que no seas tú puede
/// reportar un fallo».** `doctor` es donde un usuario mira cuando algo va mal, y
/// hasta ahora no había forma de que supiera que el fichero existe ni dónde.
///
/// Ninguno de sus estados es [`Severity::Error`], a propósito: una máquina sin
/// directorio de estado, o con uno que no se deja escribir, FUNCIONA — solo no
/// deja rastro. Un `Error` haría que `norte doctor` saliera distinto de cero
/// (decisión 4) por algo que no rompe nada, y eso entrena a ignorar su código
/// de salida.
///
/// `dir` es lo que resuelva [`norte_core::logging::log_dir`]; `None` = no hay
/// directorio de estado en esta máquina.
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
    // Escribible se comprueba INTENTÁNDOLO, no leyendo permisos: los permisos
    // no cuentan ACLs, ni un montaje de solo lectura, ni SELinux. Se crea y se
    // borra, que es exactamente lo que hará el appender.
    //
    // **`create_new`, jamás `fs::write`.** `write` es `O_TRUNC` y SIGUE
    // symlinks: con un enlace plantado en el nombre de la sonda —fijo y
    // predecible, así que no hay carrera que ganar— un `norte doctor` truncaba
    // a cero lo que apuntara, y el `remove_file` de después borraba el ENLACE y
    // no el destino, así que el fichero se quedaba vacío y la prueba
    // desaparecía. `O_EXCL` se niega a seguir un enlace y se niega a pisar algo
    // que ya exista, que es exactamente lo que hace falta aquí.
    let sonda = dir.join(format!(".norte-doctor-probe.{}", std::process::id()));
    let escribible = std::fs::create_dir_all(dir).is_ok()
        && std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&sonda)
            .is_ok();
    // Un borrado que falle no se reporta: acaba de demostrarse que el
    // directorio se deja escribir, así que el único resto posible es uno por
    // pid, y el `create_new` de la próxima vez lo detectaría como no-escribible
    // en vez de pisarlo — que es el lado seguro del error.
    let _ = std::fs::remove_file(&sonda);
    if !escribible {
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

    /// #108 b4: un id roto = Warn nombrado (jamás drop silencioso).
    /// #117-follow-up: los `plugin:` YA se pintan (como los `attr:` desde
    /// #117) — `columns-no-renderer` está RETIRADO y no dispara para nadie;
    /// solo les queda el diagnóstico del cap.
    #[test]
    fn columns_ids_rotos_se_reportan_y_no_renderer_esta_retirado() {
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
            "columns-no-renderer retirado — plugin: se pinta: {f:?}"
        );
    }

    /// #117-follow-up: la columna de plugin 9.ª de una lista supera el cap
    /// de petición — ni se pinta ni se pide, y doctor la nombra
    /// (`columns-plugins-over-cap`, espejo del cap de attrs).
    #[test]
    fn columns_plugins_sobre_el_cap_se_reportan() {
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

    /// #117: el attr 17.º de una lista supera el cap de petición — ni se
    /// pinta ni se pide, y doctor lo nombra (`columns-attrs-over-cap`).
    #[test]
    fn columns_attrs_sobre_el_cap_se_reportan() {
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

    /// #117 encoding-audit M1: un `attr:` que parsea pero no es un id
    /// legal del wire (typo de caja) — la columna se salta y doctor lo
    /// nombra (`columns-attr-id-not-wire-safe`); sin esto sería invisible
    /// (el id parsea bien y nada más lo cuenta).
    #[test]
    fn columns_attr_id_no_wire_safe_se_reporta() {
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
        // El bien formado no dispara nada.
        assert!(
            !f.iter().any(|x| x.code == "columns-attr-id-not-wire-safe"
                && x.detail.contains("attr:posix.mode")),
            "{f:?}"
        );
    }

    /// #108 7b: un spec cuyo formato no casa con su columna (`iec` en un
    /// timestamp) = Warn `columns-bad-spec` nombrando el id — el render
    /// aplica el default en silencio, así que doctor es quien lo cuenta.
    #[test]
    fn columns_spec_que_no_casa_se_reporta() {
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
    fn masked_and_capped_enmascara_hazards_y_recorta_largos() {
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

    /// encoding-auditor MAJOR: `run` is untrusted config text — a cloned
    /// repository's `./.norte/keymap.toml` is loaded without trust — and this
    /// finding goes to a terminal. Unmasked, `"app.\u{202E}tiuq"` PAINTS as
    /// `app.quit`, so the warning names a command the user cannot tell from
    /// the real one; `"app.quit\u{1B}]0;x\u{7}"` sets the terminal title.
    /// `--json` is no refuge: `serde_json` escapes C0 but not U+202E.
    #[test]
    fn keymap_run_hostil_sale_enmascarado_jamas_crudo() {
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
                    "{}: {} salió crudo — {}",
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
    fn keymap_lua_charset_invalido_es_un_solo_error_estructural() {
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
    fn plugins_config_sin_fichero_muestra_los_defaults() {
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
    fn plugins_config_con_override_muestra_el_valor_efectivo() {
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
    fn plugins_sin_config_no_tiene_findings_plugin_config() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "org.norte.demo", DEMO_MANIFEST);

        let findings = check_plugins(dir.path());
        assert!(!findings.iter().any(|f| f.code == "plugin-config"));
    }

    /// TDD (P2 Task 2, fail-closed): a `config.toml` that fails validation
    /// against the manifest's `[config]` schema excludes the WHOLE plugin —
    /// it surfaces via the EXISTING `plugin-manifest-broken` catalog-error
    /// path (mirrors `plugins_manifest_roto_es_error`), not as a
    /// `plugin-config`/`plugin-ok` finding. The error names the KEY, never
    /// the value (#73).
    #[test]
    fn plugins_config_toml_invalido_excluye_el_plugin_y_reporta_error() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "org.norte.cfg", CONFIG_MANIFEST);
        write_config_values(dir.path(), "org.norte.cfg", "no-declarada = \"x\"\n");

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
        assert!(err.detail.contains("no-declarada"), "{}", err.detail);
    }

    /// P2 Task 4a security review: an UNKNOWN key from a hostile
    /// `config.toml` (ESC/bidi override) must never reach the
    /// `plugin-manifest-broken` finding raw — unlike a manifest-declared
    /// key, a `config.toml` key has no charset guarantee, and the message
    /// crosses both `norte doctor`'s stdout and the wire
    /// (`PluginLoadError.reason`).
    #[test]
    fn plugins_manifest_broken_con_clave_hostil_se_enmascara() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "org.norte.cfg", CONFIG_MANIFEST);
        // Clave TOML entrecomillada con ESC + un override RLO (bidi) —
        // ninguno de los dos es válido en el charset [a-z0-9-]{1,32} que
        // exige toda clave DECLARADA, así que este es TOML puro de usuario.
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
    fn plugins_config_value_con_hazard_de_terminal_se_enmascara() {
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
    fn plugins_config_value_largo_se_recorta() {
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
    fn un_help_md_recortado_sale_como_hallazgo() {
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
    fn un_help_md_con_bytes_que_no_decodifican_sale_como_hallazgo() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "acme.ftp", &manifest_for("acme.ftp"));
        write_help(dir.path(), "acme.ftp", b"\xef\xbb\xbf# T\xc3\x28tulo\n");

        let f = check_plugins(dir.path())
            .into_iter()
            .find(|f| f.code == "plugin-help-lossy")
            .expect("se reporta la pérdida");
        assert_eq!(f.severity, Severity::Warn);
        assert!(f.detail.contains("acme.ftp"), "detail: {}", f.detail);
    }

    /// TDD (H3e): the header declares a command the plugin does not own. The
    /// parser drops that row in silence, so this finding is the only place
    /// the author learns of it.
    #[test]
    fn un_help_md_con_comandos_ajenos_sale_como_hallazgo() {
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
    fn un_help_md_con_la_cabecera_rota_sale_como_hallazgo() {
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
            "una cabecera que no parsea no tiene lista de comandos que revisar: {findings:?}"
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
    fn la_colision_de_id_con_el_corpus_es_hoy_estructuralmente_imposible() {
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
    fn un_help_md_que_escapa_del_directorio_del_plugin_sale_como_vacio() {
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
    fn un_plugin_sin_help_md_no_genera_ruido() {
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
    fn un_help_md_limpio_no_genera_ruido() {
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
            "un help.md correcto no es un hallazgo: {findings:?}"
        );
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

    /// El log es lo que hace posible un reporte de bug de alguien que no somos
    /// nosotros, así que lo primero que tiene que decir `doctor` es DÓNDE está.
    #[test]
    fn doctor_nombra_el_fichero_de_log() {
        let dir = tempfile::tempdir().expect("tmp");
        let logs = dir.path().join("logs");
        std::fs::create_dir_all(&logs).expect("logs");
        std::fs::write(logs.join("norte.log.2026-08-16"), b"una linea\n").expect("log");

        let hallazgos = check_logs(Some(&logs));
        let f = una(&hallazgos, "logs");
        assert_eq!(f.severity, Severity::Ok);
        assert_eq!(f.code, "logs-ok");
        assert!(
            f.detail.contains(&logs.display().to_string()),
            "la fila lleva la RUTA: {}",
            f.detail
        );
    }

    /// Un directorio de estado que no existe es una DEGRADACIÓN, no una avería:
    /// la máquina funciona, simplemente no deja rastro. `Error` haría que
    /// `norte doctor` saliera distinto de cero por algo que no rompe nada
    /// (decisión 4).
    #[test]
    fn sin_directorio_de_estado_es_aviso_y_no_error() {
        let hallazgos = check_logs(None);
        let f = una(&hallazgos, "logs");
        assert_eq!(f.severity, Severity::Warn);
        assert_eq!(f.code, "logs-no-state-dir");
    }

    /// Y un directorio que no se deja escribir tampoco es una avería: se avisa
    /// y el programa arranca igual, que es lo que hace `logging::init_to_file`.
    #[cfg(unix)]
    #[test]
    fn un_directorio_no_escribible_avisa() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tmp");
        let logs = dir.path().join("logs");
        std::fs::create_dir_all(&logs).expect("logs");
        std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o500)).expect("chmod");

        let hallazgos = check_logs(Some(&logs));
        let f = una(&hallazgos, "logs");
        // Restaurar antes de cualquier assert: si falla, el TempDir tiene que
        // poder borrarse igual.
        std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        assert_eq!(f.severity, Severity::Warn);
        assert_eq!(f.code, "logs-unwritable");
    }

    /// La única fila de `section` en `findings`.
    fn una<'a>(findings: &'a [Finding], section: &str) -> &'a Finding {
        let filas: Vec<&Finding> = findings.iter().filter(|f| f.section == section).collect();
        assert_eq!(filas.len(), 1, "una fila de {section}: {findings:?}");
        filas[0]
    }
}
