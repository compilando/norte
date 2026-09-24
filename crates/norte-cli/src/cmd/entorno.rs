//! `norte doctor`, `norte paths` and `norte shell-init`: read-only
//! diagnostics over config/keymaps and environment utilities.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;

use crate::{doctor, paths};

/// Text-mode line for one [`doctor::Finding`] (review MINOR-3): `Finding`
/// itself carries only STABLE, MACHINE detail (ids, paths, var names — see
/// `doctor.rs`'s own module doc) so `--json` stays locale-free. A few codes
/// had a full narrative sentence baked into `detail` as English prose before
/// this review (`plugin-digest-stale`, `connections-none`); that sentence now
/// lives HERE, keyed by `code`, and is looked up ONLY for text-mode
/// rendering — every other code still prints its raw (already-machine)
/// `detail` unchanged.
fn doctor_finding_line(f: &doctor::Finding) -> String {
    match f.code {
        "connections-parse" => norte_i18n::t("cli-doctor-detail-connections-parse"),
        "connections-none" => norte_i18n::t("cli-doctor-detail-connections-none"),
        // #320: `detail` stays the MACHINE value (`conn: VAR`); the
        // sentences that distinguish the variable's three states live
        // here, like the ones above. All three, not just the new one: two
        // adjacent warnings from the same section read in different
        // registers — one narrated and one raw — compare worse than if
        // neither did.
        "conn-secret-env-empty" => norte_i18n::ta(
            "cli-doctor-detail-conn-secret-env-empty",
            &[("detail", &f.detail)],
        ),
        "conn-secret-env-not-utf8" => norte_i18n::ta(
            "cli-doctor-detail-conn-secret-env-not-utf8",
            &[("detail", &f.detail)],
        ),
        "conn-secret-env-absent" => norte_i18n::ta(
            "cli-doctor-detail-conn-secret-env-absent",
            &[("detail", &f.detail)],
        ),
        "plugin-digest-stale" => norte_i18n::ta(
            "cli-doctor-detail-plugin-digest-stale",
            &[("id", &f.detail)],
        ),
        "plugin-wit-mismatch" => norte_i18n::ta(
            "cli-doctor-detail-plugin-wit-mismatch",
            &[("detail", &f.detail)],
        ),
        // H3e `plugin-help-*`: same policy — `detail` stays the machine value
        // (the plugin id; for `foreign-command`, `{id}: {command}`, already
        // masked and capped at the `doctor` boundary) and the sentence lives
        // here.
        "plugin-help-truncated" => norte_i18n::ta(
            "cli-doctor-detail-plugin-help-truncated",
            &[("id", &f.detail)],
        ),
        "plugin-help-lossy" => {
            norte_i18n::ta("cli-doctor-detail-plugin-help-lossy", &[("id", &f.detail)])
        }
        "plugin-help-empty" => {
            norte_i18n::ta("cli-doctor-detail-plugin-help-empty", &[("id", &f.detail)])
        }
        "plugin-help-bad-header" => norte_i18n::ta(
            "cli-doctor-detail-plugin-help-bad-header",
            &[("id", &f.detail)],
        ),
        "plugin-help-foreign-command" => norte_i18n::ta(
            "cli-doctor-detail-plugin-help-foreign-command",
            &[("detail", &f.detail)],
        ),
        "plugin-help-shadows-topic" => norte_i18n::ta(
            "cli-doctor-detail-plugin-help-shadows-topic",
            &[("id", &f.detail)],
        ),
        _ => f.detail.clone(),
    }
}

/// `norte shell-init <SHELL>` (S3, shell-integration): prints the wrapper's
/// source for the caller's rc file to `eval` (bash/zsh) or `source` (fish).
/// Pure lookup — [`norte_frontend::shell::Shell`] owns the actual text — so
/// this is early-returned in `run()` like `doctor_cmd`/`help::run`, ahead of
/// any engine/daemon wiring.
pub(crate) fn shell_init_cmd(shell: &str) -> ExitCode {
    if let Some(sh) = norte_frontend::shell::Shell::parse(shell) {
        // No trailing newline of our own: the wrapper's own text already
        // ends in one, and `eval "$(norte shell-init bash)"` runs command
        // substitution either way (it strips trailing newlines itself).
        print!("{}", sh.wrapper());
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "{}",
            norte_i18n::ta("cli-shell-init-unknown", &[("shell", shell)])
        );
        ExitCode::FAILURE
    }
}

/// `norte paths`: print where every file norte reads or writes lives.
///
/// Resolution is NOT re-derived here — layers, config dir, state dir, the
/// effective log dir and the socket all come from the same functions the rest
/// of the binary uses, so this command cannot drift away from the code it
/// explains. Read-only: it stats, it never creates, so asking where the config
/// dir is does not bring it into existence.
///
/// Always exits `SUCCESS`: a missing path is an ANSWER (half of these files
/// are optional), not a failure. Judging config is `doctor`'s job.
pub(crate) async fn paths_cmd(json: bool, socket: Option<PathBuf>) -> anyhow::Result<ExitCode> {
    let layers = norte_config::standard_layers();
    let config_dir = norte_config::config_dir();
    let socket = socket.unwrap_or_else(|| norte_core::daemon::default_socket_path(None));
    // The same effective `[log] dir` the frontends and `doctor` resolve:
    // pointing at the default while the real log is somewhere else is
    // exactly the mistake this command exists to prevent.
    let layers_log = layers.clone();
    // `collect` stats each path (synchronous I/O, hard rule 2).
    let entries = tokio::task::spawn_blocking(move || {
        let dir_log = norte_config::load(&layers_log)
            .ok()
            .and_then(|c| c.log.dir)
            .filter(|d| d.is_absolute());
        let log_dir = norte_core::logging::log_dir(dir_log.as_deref());
        paths::collect(
            &layers_log,
            &config_dir,
            norte_config::dirs::state_dir().as_deref(),
            log_dir.as_deref(),
            &socket,
        )
    })
    .await
    .map_err(|e| anyhow::anyhow!("paths: {e}"))?;

    if json {
        #[derive(serde::Serialize)]
        struct Row<'a> {
            id: &'a str,
            /// Lossy on purpose, and the only lossy thing here: a path that is
            /// not UTF-8 still has to be printable. Same policy as `doctor`.
            path: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            layer: Option<&'a str>,
            exists: bool,
        }
        let rows: Vec<Row<'_>> = entries
            .iter()
            .map(|e| Row {
                id: e.id,
                path: e.path.display().to_string(),
                layer: e.layer.map(paths::layer_name),
                exists: e.exists,
            })
            .collect();
        serde_json::to_writer_pretty(std::io::stdout().lock(), &rows)
            .context(norte_i18n::t("cli-serialize-failed"))?;
        println!();
    } else {
        let width = entries.iter().map(|e| e.id.len()).max().unwrap_or(0);
        for e in &entries {
            let label = match e.layer {
                Some(l) => format!("{}:{}", e.id, paths::layer_name(l)),
                None => e.id.to_string(),
            };
            let mark = if e.exists {
                String::new()
            } else {
                format!("  {}", norte_i18n::t("cli-paths-missing"))
            };
            println!(
                "{label:<width$}  {}{mark}",
                e.path.display(),
                width = width + 8
            );
        }
        println!("{}", norte_i18n::t("cli-paths-footer"));
    }
    Ok(ExitCode::SUCCESS)
}

/// `norte doctor` (H2): read-only diagnostics over config layers, keymaps,
/// plugins and connections. Never touches the daemon/engine — early-returned
/// in `run()` like `audit_cmd`.
pub(crate) async fn doctor_cmd(json: bool) -> anyhow::Result<ExitCode> {
    let layers = norte_config::standard_layers();
    // Plugins/connections live under the SINGLE resolved config dir (ADR
    // 0035's `norte_config::config_dir` — the same one `norte_core::connect`
    // re-exports and every other command in this binary already uses for
    // `connections.toml`/`journal.db`/`policy.toml`), NOT `layers.dirs.last()`
    // — that is the PROJECT layer (`.norte`, lowest precedence but last in
    // the ascending-precedence `Layers::dirs` list), a different directory.
    let config_dir = norte_config::config_dir();
    // `doctor::check_*` do synchronous fs I/O (norte-config's own design —
    // see its crate doc); never call them directly on the async executor
    // (rule 2).
    let layers_log = layers.clone();
    let findings = tokio::task::spawn_blocking(move || {
        let env = |k: &str| std::env::var_os(k);
        let mut findings = doctor::check_config(&layers, &env);
        findings.extend(doctor::check_columns(&layers));
        findings.extend(doctor::check_layout(&layers));
        findings.extend(doctor::check_keymaps(&layers));
        findings.extend(doctor::check_plugins(&config_dir));
        findings.extend(doctor::check_connections(&config_dir, &env));
        // Roadmap item 9: where the log is. Without this row the file
        // exists and nobody knows to ask for it when needed — and it has
        // to resolve the SAME `[log] dir` the frontends resolve: pointing
        // at the default while the real log is somewhere else is worse
        // than saying nothing.
        let dir_log = norte_config::load(&layers_log)
            .ok()
            .and_then(|c| c.log.dir)
            .filter(|d| d.is_absolute());
        findings.extend(doctor::check_logs(
            norte_core::logging::log_dir(dir_log.as_deref()).as_deref(),
        ));
        findings
    })
    .await
    .map_err(|e| anyhow::anyhow!("doctor: {e}"))?;

    if json {
        #[derive(serde::Serialize)]
        struct Summary {
            errors: usize,
            warnings: usize,
        }
        #[derive(serde::Serialize)]
        struct Report<'a> {
            findings: &'a [doctor::Finding],
            summary: Summary,
        }
        let errors = findings
            .iter()
            .filter(|f| f.severity == doctor::Severity::Error)
            .count();
        let warnings = findings
            .iter()
            .filter(|f| f.severity == doctor::Severity::Warn)
            .count();
        serde_json::to_writer_pretty(
            std::io::stdout().lock(),
            &Report {
                findings: &findings,
                summary: Summary { errors, warnings },
            },
        )
        .context(norte_i18n::t("cli-serialize-failed"))?;
        println!();
    } else {
        println!("{}", norte_i18n::t("cli-doctor-title"));
        let mut last_section = "";
        for f in &findings {
            if f.section != last_section {
                let key = match f.section {
                    "config" => "cli-doctor-section-config",
                    "keymap" => "cli-doctor-section-keymap",
                    "plugins" => "cli-doctor-section-plugins",
                    "connections" => "cli-doctor-section-connections",
                    // Should not happen (`Finding::section` is a closed
                    // catalog in this module): printed raw instead of
                    // panicking — a diagnostic must never bring down the
                    // process.
                    other => other,
                };
                println!("{}", norte_i18n::t(key));
                last_section = f.section;
            }
            let marker = match f.severity {
                doctor::Severity::Ok => norte_i18n::t("cli-doctor-ok"),
                doctor::Severity::Warn => norte_i18n::t("cli-doctor-warn"),
                doctor::Severity::Error => norte_i18n::t("cli-doctor-error"),
            };
            println!("  [{marker}] {}: {}", f.code, doctor_finding_line(f));
        }
        println!("{}", norte_i18n::t("cli-doctor-footer-keymap-approx"));
        println!(
            "{}",
            norte_i18n::t("cli-doctor-footer-connections-not-probed")
        );
        // S3 (shell-integration): unconditional, like the two footers above
        // — whether the wrapper is actually `eval`ed in the caller's rc file
        // cannot be observed from this process, so the honest answer is the
        // instruction, not a Finding with a severity that would claim more
        // than can be known.
        println!("{}", norte_i18n::t("cli-doctor-footer-shell-init"));
    }
    let has_error = findings
        .iter()
        .any(|f| f.severity == doctor::Severity::Error);
    Ok(if has_error {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Appends a line (+`\n`) to `path`, creating it `0600` if it does not
/// exist.
pub(crate) async fn append_line_0600(path: &std::path::Path, line: &str) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut opts = tokio::fs::OpenOptions::new();
    opts.append(true).create(true);
    #[cfg(unix)]
    opts.mode(0o600);
    let mut f = opts.open(path).await.context("journal-anchors.jsonl")?;
    f.write_all(line.as_bytes()).await?;
    f.write_all(b"\n").await?;
    f.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// H3e: every `plugin-help-*` code must have a renderer arm AND a Fluent
    /// message in BOTH locales. A missing key falls back to the key ITSELF
    /// (`norte_i18n::ta_in`'s contract), and a missing arm falls back to the
    /// raw machine `detail` — both are silent in text mode, so they are pinned
    /// here instead.
    #[test]
    fn plugin_help_findings_render_in_both_languages() {
        for code in [
            "plugin-help-truncated",
            "plugin-help-lossy",
            "plugin-help-empty",
            "plugin-help-bad-header",
            "plugin-help-foreign-command",
            "plugin-help-shadows-topic",
        ] {
            let key = format!("cli-doctor-detail-{code}");
            for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
                let rendered = norte_i18n::ta_in(
                    lang,
                    &key,
                    &[("id", "acme.ftp"), ("detail", "acme.ftp: fs.copy")],
                );
                assert_ne!(rendered, key, "{key} missing in {lang:?}");
                assert!(
                    rendered.contains("acme.ftp"),
                    "{key} drops the id: {rendered}"
                );
            }
            // …and the arm exists: the line is not the raw detail.
            let f = doctor::Finding {
                section: "plugins",
                severity: doctor::Severity::Warn,
                code,
                detail: "acme.ftp".to_owned(),
            };
            assert_ne!(
                doctor_finding_line(&f),
                f.detail,
                "{code} has no renderer arm"
            );
        }
    }
}
