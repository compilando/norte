//! `norte plugin run|install|uninstall|list` (M4-P4).

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use norte_core::backend::Backend;

use crate::PluginCmd;

/// `norte plugin run <id> <command> [arg]`: runs a command of a plugin
/// ALREADY approved+enabled by the human and writes its output to
/// STDOUT. Goes through the `Backend` chosen by the global flags
/// (`--daemon`/`--socket`), like the rest of the operations (rule 7).
pub(crate) async fn plugin_cmd(
    backend: &Backend,
    cmd: PluginCmd,
    socket: Option<PathBuf>,
) -> anyhow::Result<ExitCode> {
    match cmd {
        PluginCmd::Run { id, command, arg } => {
            match backend.plugin_run_command(&id, &command, &arg).await {
                Ok(output) => {
                    println!("{output}");
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => {
                    eprintln!(
                        "{}",
                        norte_i18n::ta("cli-plugin-run-failed", &[("error", &e.to_string())])
                    );
                    Ok(ExitCode::FAILURE)
                }
            }
        }
        // Local copy, WITHOUT a daemon: installing is moving files into
        // the config directory, and making that depend on a live daemon
        // would be asking the user to start the program in order to
        // install something for it.
        PluginCmd::Install { path, force } => {
            let dir = norte_core::connect::config_dir();
            // Copying a directory is blocking I/O (hard rule 2), like
            // `uninstall`'s deletion right below.
            let report = tokio::task::spawn_blocking(move || {
                norte_core::plugins::install(&dir, &path, force)
            })
            .await
            .context("installing")?;
            match report {
                Ok(rep) => {
                    // The name comes from a third party's manifest: it is
                    // painted sanitized, as in the manager. Text via
                    // Fluent (#319).
                    let (name, _) = norte_frontend::display_name(rep.name.as_bytes());
                    let key = if rep.replaced {
                        "cli-plugin-replaced"
                    } else {
                        "cli-plugin-installed"
                    };
                    println!(
                        "{}",
                        norte_i18n::ta(key, &[("id", &rep.id), ("name", &name)])
                    );
                    if rep.replaced {
                        println!("{}", norte_i18n::t("cli-plugin-replaced-consent"));
                    }
                    println!("{}", norte_i18n::t("cli-plugin-unapproved"));
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => {
                    eprintln!("{e}");
                    Ok(ExitCode::FAILURE)
                }
            }
        }
        PluginCmd::Uninstall { id } => plugin_uninstall(backend, socket, &id).await,
        // `plugin.list`, the same catalog the manager paints: id,
        // category, the TWO facts (approved, enabled) and the
        // capabilities approving it would grant. The name comes from a
        // third party: sanitized, as in the manager. Broken ones are
        // counted, not listed — `norte doctor` explains them one by one.
        PluginCmd::List => plugin_list(backend).await,
    }
}

/// `norte plugin uninstall` (ADR 0113). With `--daemon`, THROUGH the
/// daemon (`plugin.uninstall`): deletes in ITS OWN config directory and
/// forgets it in memory. Without `--daemon`, on this process's disk, like
/// `install`.
///
/// Through the daemon only if asked. The default socket comes from the
/// user, not from `NORTE_CONFIG_DIR`, and nothing in `initialize` says
/// which directory the daemon serves: a CLI with a different directory
/// uninstalling through the one it listens on would delete the
/// extension — and withdraw its approval — in THAT daemon's directory.
/// Without `--daemon`, if one is accepting connections, a warning says it
/// will keep listing it until restarted.
async fn plugin_uninstall(
    backend: &Backend,
    socket: Option<PathBuf>,
    id: &str,
) -> anyhow::Result<ExitCode> {
    use norte_core::plugins::UninstallError as U;
    if !norte_core::is_valid_plugin_id(id) {
        return Ok(uninstall_failed(&U::InvalidId));
    }
    match backend {
        Backend::Remote(r) => {
            // "Is it there?" against the DAEMON'S catalog, which is the
            // directory being deleted from. And here, not from its
            // response: it refuses with an untaxonomized `INVALID_PARAMS`,
            // which arrives as an `Internal`.
            let list = backend
                .plugins_list()
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let is_there = list.plugins.iter().any(|p| p.id == id)
                || list
                    .errors
                    .iter()
                    .any(|e| e.dir_bytes.as_deref().unwrap_or(e.dir.as_bytes()) == id.as_bytes());
            if !is_there {
                return Ok(uninstall_failed(&U::NotInstalled(id.to_owned())));
            }
            match r.plugins_uninstall(id).await {
                Ok(res) => Ok(uninstall_done(id, res.was_approved)),
                Err(e) => {
                    eprintln!(
                        "{}",
                        norte_i18n::ta("cli-plugin-uninstall-io", &[("error", &e.to_string())])
                    );
                    Ok(ExitCode::FAILURE)
                }
            }
        }
        Backend::Embedded(_) => {
            let dir = norte_core::connect::config_dir();
            let owned = id.to_owned();
            let report =
                tokio::task::spawn_blocking(move || norte_core::plugins::uninstall(&dir, &owned))
                    .await
                    .context("uninstalling")?;
            // The warning also fires when it was NOT installed: the most
            // likely way to reach `NotInstalled` is exactly ADR 0113's
            // case — a CLI with a different `NORTE_CONFIG_DIR` than the
            // daemon's — and plain "it is not installed" there does not
            // say `--daemon` exists.
            let code = match report {
                Ok(rep) => uninstall_done(&rep.id, rep.was_approved),
                Err(e) => uninstall_failed(&e),
            };
            warn_if_daemon_present(socket).await;
            Ok(code)
        }
    }
}

/// Warns, without starting it, if a daemon ACCEPTS connections on
/// `socket`: it will keep listing what was just deleted behind its back
/// until restarted.
async fn warn_if_daemon_present(socket: Option<PathBuf>) {
    /// How long to wait for a daemon that accepts but does not answer: it
    /// is a warning, and it must not hang a command that previously never
    /// touched the socket.
    const DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);
    let socket = match socket {
        Some(s) => s,
        None => {
            match tokio::task::spawn_blocking(|| norte_core::daemon::default_socket_path(None))
                .await
            {
                Ok(s) => s,
                Err(_) => return,
            }
        }
    };
    let connect = norte_core::backend::remote::RemoteBackend::connect(
        socket,
        None,
        norte_proto::methods::ClientInfo {
            name: "norte-cli".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
    );
    if matches!(tokio::time::timeout(DEADLINE, connect).await, Ok(Ok(_))) {
        eprintln!("{}", norte_i18n::t("cli-plugin-uninstall-daemon-stale"));
    }
}

/// What `plugin uninstall` says when it deleted.
fn uninstall_done(id: &str, was_approved: bool) -> ExitCode {
    println!(
        "{}",
        norte_i18n::ta("cli-plugin-uninstalled", &[("id", id)])
    );
    if was_approved {
        println!("{}", norte_i18n::t("cli-plugin-uninstalled-consent"));
    }
    ExitCode::SUCCESS
}

/// What `plugin uninstall` says when it could not. User-facing text via
/// Fluent (#319); the error's `Display` stays for the logs.
fn uninstall_failed(e: &norte_core::plugins::UninstallError) -> ExitCode {
    use norte_core::plugins::UninstallError as U;
    let msg = match e {
        U::InvalidId => norte_i18n::t("cli-plugin-uninstall-invalid-id"),
        U::NotInstalled(id) => norte_i18n::ta("cli-plugin-uninstall-not-installed", &[("id", id)]),
        U::Io(io) => norte_i18n::ta("cli-plugin-uninstall-io", &[("error", &io.to_string())]),
    };
    eprintln!("{msg}");
    ExitCode::FAILURE
}

/// `norte plugin list`: one row per installed plugin, tabulated.
async fn plugin_list(backend: &Backend) -> anyhow::Result<ExitCode> {
    let list = backend.plugins_list().await?;
    if list.plugins.is_empty() && list.errors.is_empty() {
        println!("{}", norte_i18n::t("cli-plugin-list-empty"));
        return Ok(ExitCode::SUCCESS);
    }
    for p in &list.plugins {
        let (name, _) = norte_frontend::display_name(p.name.as_bytes());
        let approved = norte_i18n::t(if p.approved {
            "cli-plugin-state-approved"
        } else {
            "cli-plugin-state-unapproved"
        });
        let enabled = norte_i18n::t(if p.enabled {
            "cli-plugin-state-enabled"
        } else {
            "cli-plugin-state-disabled"
        });
        let caps = if p.capabilities.is_empty() {
            "-".to_string()
        } else {
            p.capabilities.join(",")
        };
        println!(
            "{}\t{}\t{approved}\t{enabled}\t{caps}\t{name}",
            p.id, p.category
        );
    }
    if !list.errors.is_empty() {
        let n = list.errors.len().to_string();
        eprintln!(
            "{}",
            norte_i18n::ta("cli-plugin-list-broken", &[("count", &n)])
        );
    }
    Ok(ExitCode::SUCCESS)
}
