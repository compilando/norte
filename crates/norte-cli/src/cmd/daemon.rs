//! `norte daemon run|stop`, `norte mcp serve`, `norte policy grant` and
//! `norte undo` (ADR 0011, M3-4).

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context;
use norte_core::Engine;
use norte_core::backend::Backend;

use crate::{DaemonCmd, McpCmd, PolicyCmd};

/// Resolves the daemon's socket and an auto-start `spawn_cmd` (this same
/// binary knows how to be a daemon). FS probes off the runtime (hard rule
/// 2).
#[cfg(unix)]
async fn socket_and_spawn(
    socket: Option<PathBuf>,
) -> anyhow::Result<(PathBuf, Vec<std::ffi::OsString>)> {
    let (socket, exe) = tokio::task::spawn_blocking(move || {
        let socket = socket.unwrap_or_else(|| norte_core::daemon::default_socket_path(None));
        (socket, std::env::current_exe())
    })
    .await
    .context("resolución del socket/exe")?;
    let exe = exe.context("current_exe")?;
    let spawn_cmd = norte_core::daemon::daemon_run_argv(exe, &socket);
    Ok((socket, spawn_cmd))
}

/// `norte mcp serve`: serves MCP over stdio, starting the daemon if
/// needed. The bridge connects as an agent session; tracing goes to
/// stderr (the global `logging::init` already sets it), stdout is
/// EXCLUSIVE to the MCP transport.
#[cfg(unix)]
pub(crate) async fn mcp_cmd(cmd: McpCmd, socket: Option<PathBuf>) -> anyhow::Result<ExitCode> {
    let McpCmd::Serve { session } = cmd;
    let (socket, spawn_cmd) = socket_and_spawn(socket).await?;
    // Idempotent auto-start: connect_or_spawn starts the daemon if the
    // socket does not answer, then the bridge reconnects.
    if let Err(e) = norte_core::daemon::Client::connect_or_spawn(&socket, move || {
        let mut cmd = std::process::Command::new(&spawn_cmd[0]);
        cmd.args(&spawn_cmd[1..]);
        cmd
    })
    .await
    {
        anyhow::bail!("no se pudo arrancar/alcanzar el daemon: {e}");
    }
    eprintln!(
        "{}",
        norte_i18n::ta("cli-mcp-serving", &[("session", &session)])
    );
    norte_mcp::bridge::serve_stdio(&socket, &session)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("el puente MCP terminó con error")?;
    Ok(ExitCode::SUCCESS)
}

/// `norte policy grant <request_id>`: a human grants a scope an agent
/// requested. User connection (no `agent_session`).
#[cfg(unix)]
pub(crate) async fn policy_cmd(
    cmd: PolicyCmd,
    socket: Option<PathBuf>,
) -> anyhow::Result<ExitCode> {
    use norte_core::daemon::Client;
    let PolicyCmd::Grant { request_id } = cmd;
    let (socket, _spawn) = socket_and_spawn(socket).await?;
    let mut client = Client::connect(&socket)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("no hay daemon en marcha")?;
    client
        .initialize(norte_proto::methods::ClientInfo {
            name: "norte-cli".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let _: norte_proto::methods::GrantScopeResult = client
        .call(
            norte_proto::methods::POLICY_GRANT_SCOPE,
            &norte_proto::methods::GrantScopeParams { request_id },
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("no se pudo conceder el scope")?;
    println!("{}", norte_i18n::t("cli-scope-granted"));
    Ok(ExitCode::SUCCESS)
}

/// `norte undo <session>`: a human undoes an agent's whole session. Runs
/// as a Task; its terminal state is awaited with Ctrl-C = cancel (the
/// `cp` pattern).
#[cfg(unix)]
pub(crate) async fn undo_cmd(session: &str, socket: Option<PathBuf>) -> anyhow::Result<ExitCode> {
    use norte_core::backend::remote::RemoteBackend;
    let (socket, spawn_cmd) = socket_and_spawn(socket).await?;
    let backend = Backend::Remote(
        RemoteBackend::connect(
            socket,
            Some(spawn_cmd),
            norte_proto::methods::ClientInfo {
                name: "norte-cli".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("no se pudo hablar con el daemon")?,
    );
    let task = backend
        .undo_session(session)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context(norte_i18n::ta("cli-undo-failed", &[("error", "submit")]))?;
    let task_id = task.id();
    let outcome = crate::task::run_task(task, true).await;
    if outcome != ExitCode::SUCCESS {
        return Ok(outcome);
    }
    // QUALIFIED "Done" (#71): the Task's terminal state is Completed even
    // if the LIFO got blocked or everything was skipped — the report is
    // the ONLY integrity signal. Only an N-1 daemon (without the method →
    // Unsupported) degrades to the simple message; any other fetch
    // failure is NOT swallowed: warning + exit≠0 (the undo may have
    // worked, but it is left unverified).
    let report = match backend.undo_report(task_id).await {
        Ok(r) => r,
        Err(norte_proto::Error::Unsupported) => {
            println!(
                "{}",
                norte_i18n::ta("cli-undo-done", &[("session", session)])
            );
            return Ok(outcome);
        }
        Err(e) => {
            eprintln!(
                "{}",
                norte_i18n::ta("cli-undo-report-unavailable", &[("error", &e.to_string())])
            );
            return Ok(ExitCode::FAILURE);
        }
    };
    println!(
        "{}",
        norte_i18n::ta(
            "cli-undo-report",
            &[
                ("undone", &report.undone.to_string()),
                (
                    "skipped_irreversible",
                    &report.skipped_irreversible.to_string(),
                ),
            ],
        )
    );
    if report.skipped_created_no_trash > 0 {
        eprintln!(
            "{}",
            norte_i18n::ta(
                "cli-undo-left-in-place",
                &[("count", &report.skipped_created_no_trash.to_string())],
            )
        );
    }
    if let Some(blocked) = report.blocked {
        eprintln!(
            "{}",
            norte_i18n::ta(
                "cli-undo-blocked",
                &[
                    ("seq", &blocked.seq.to_string()),
                    ("error", &blocked.error.to_string()),
                ],
            )
        );
        return Ok(ExitCode::FAILURE);
    }
    println!(
        "{}",
        norte_i18n::ta("cli-undo-done", &[("session", session)])
    );
    Ok(outcome)
}

/// A startup or provisioning [`Aviso`](norte_core::equipo::Aviso), in the
/// operator's language. Used by `norte daemon run` and the CLI's embedded
/// engine: both provision with `norte_core::equipo` and both warn the
/// same way.
pub(crate) fn warning_text(aviso: &norte_core::equipo::Aviso) -> String {
    use norte_core::equipo::Aviso;
    use norte_i18n::ta;
    match aviso {
        Aviso::SpoolsBarridos(n) => ta("cli-spool-swept", &[("count", &n.to_string())]),
        Aviso::SpoolsAMedias {
            removed,
            failed,
            dir,
        } => ta(
            "cli-spool-partial",
            &[
                ("removed", &removed.to_string()),
                ("failed", &failed.to_string()),
                ("path", &dir.display().to_string()),
            ],
        ),
        Aviso::SpoolsSinBarrer { dir, error } => ta(
            "cli-spool-sweep-failed",
            &[("path", &dir.display().to_string()), ("error", error)],
        ),
        Aviso::SinIndice(error) => ta("cli-warn-no-index", &[("error", error)]),
        Aviso::IaNoDisponible(error) => ta("cli-warn-ai-unavailable", &[("error", error)]),
        // Already written out by `install_embed_provider`.
        Aviso::IaEmbeddings(w) => w.clone(),
        Aviso::IaInvalida(error) => ta("cli-warn-ai-invalid", &[("error", error)]),
        Aviso::IaNoCargo(error) => ta("cli-warn-ai-load-failed", &[("error", error)]),
        Aviso::ArchivoInvalido(error) => ta("cli-warn-archive-invalid", &[("error", error)]),
    }
}

/// Chooses the transport (rule 7: the logic is the same). `--daemon`
/// connects to the socket, starting `norte daemon run` if needed.
pub(crate) async fn make_backend(
    engine: Engine,
    daemon: bool,
    socket: Option<PathBuf>,
) -> anyhow::Result<Backend> {
    if !daemon {
        // #44: degradation warnings are drained at dispatch (top-level)
        // via `Backend::take_degraded`, which in embedded mode installs
        // the channel observer — same path as in daemon mode (rust M1 +
        // security m1).
        return Ok(Backend::Embedded(Arc::new(engine)));
    }
    #[cfg(not(unix))]
    {
        let _ = socket;
        anyhow::bail!("--daemon no está disponible en Windows todavía (issue #33)");
    }
    #[cfg(unix)]
    {
        use norte_core::backend::remote::RemoteBackend;
        // Both FS probes (default socket + current_exe) off the runtime
        // (hard rule 2, rust-reviewer m3).
        let (socket, exe) = tokio::task::spawn_blocking(move || {
            let socket = socket.unwrap_or_else(|| norte_core::daemon::default_socket_path(None));
            (socket, std::env::current_exe())
        })
        .await
        .context("resolución del socket/exe")?;
        let exe = exe.context("current_exe")?;
        // Auto-start: this SAME binary knows how to be a daemon, with the
        // shared argv — what starts a loose command shuts itself down.
        let spawn_cmd = norte_core::daemon::daemon_run_argv(exe, &socket);
        let remote = RemoteBackend::connect(
            socket,
            Some(spawn_cmd),
            norte_proto::methods::ClientInfo {
                name: "norte-cli".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("no se pudo hablar con el daemon")?;
        Ok(Backend::Remote(remote))
    }
}

/// `norte daemon run|stop` (ADR 0011). The engine that serves the daemon
/// is the SAME one embedded in this CLI: only the transport changes (rule
/// 7).
///
/// `ring` is the in-memory registry `run` set up (#328): the SAME one the
/// `tracing` layer writes to, and the one `log.tail` serves. `None` — the
/// setup failed because there was already a subscriber — leaves the
/// daemon answering `Unsupported` to both registry methods, which is what
/// the frontend needs to degrade while saying why.
#[cfg(unix)]
#[expect(clippy::too_many_lines, reason = "one arm per daemon subcommand")]
pub(crate) async fn daemon_cmd(
    cmd: DaemonCmd,
    ring: Option<norte_config::logring::LogRing>,
) -> anyhow::Result<ExitCode> {
    use norte_core::daemon::{Client, default_socket_path};
    match cmd {
        DaemonCmd::Run {
            socket,
            idle_timeout,
        } => {
            // WHAT the daemon carries is decided by the core (rule 7,
            // `norte_core::daemon::componer`); what stays here is the
            // binary's job: painting the warnings in the operator's
            // language, the log ring and the signals.
            use norte_core::daemon::componer::{ErrorDeArranque, Opciones};
            let mut avisos = Vec::new();
            let compuesto = norte_core::daemon::componer(
                Opciones {
                    socket,
                    idle_timeout: (idle_timeout > 0)
                        .then(|| std::time::Duration::from_secs(idle_timeout)),
                    // The UI session (L2) lives in the state directory.
                    // Without one — an environment with no HOME — the
                    // daemon serves the screen and does not save it.
                    state_dir: norte_config::dirs::state_dir(),
                    config_dir: None,
                },
                &mut avisos,
            )
            .await;
            // The warnings are said BEFORE checking whether it started:
            // what was found out along the way stays true even if
            // startup later fails.
            for aviso in &avisos {
                eprintln!("{}", warning_text(aviso));
            }
            let daemon = match compuesto {
                Ok(d) => d,
                // The warning NAMES the likely cause: since #167 an
                // embedded frontend (an `ntc` without `--daemon`) keeps
                // the exclusive lock, and without this sentence the
                // operator gets sqlx's text and no clue about what to
                // close.
                Err(ErrorDeArranque::Journal(causa)) => {
                    return Err(anyhow::Error::new(causa).context(
                        "no se pudo abrir el journal (si dice «database is locked», otro \
                         proceso norte lo tiene: ¿un `ntc` embebido, u otro daemon?)",
                    ));
                }
                Err(e) => return Err(e.into()),
            };
            // The ring is set up between the bind and the `run`, which is
            // where it can be set up: the process has it (the subscriber
            // created it), not the daemon's config.
            let daemon = match ring {
                Some(ring) => daemon.with_log_ring(ring),
                None => daemon,
            };
            eprintln!(
                "{}",
                norte_i18n::ta(
                    "cli-daemon-listening",
                    &[("socket", &daemon.socket_path().display().to_string())]
                )
            );
            // Ctrl-C/SIGTERM = graceful shutdown (tasks finish); the
            // SECOND signal escalates to hard (cancels tasks) — without
            // it, a hung task would only die with SIGKILL (M4
            // rust-reviewer). The registration goes BEFORE the spawn: if
            // it fails, a visible error, not a panic swallowed inside a
            // task (M5).
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .context("no se pudo registrar SIGTERM")?;
            let shutdown = daemon.shutdown_token();
            let hard = daemon.hard_shutdown_token();
            tokio::spawn(async move {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = sigterm.recv() => {}
                }
                shutdown.cancel();
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = sigterm.recv() => {}
                }
                eprintln!("{}", norte_i18n::t("cli-daemon-hard-shutdown"));
                hard.cancel();
            });
            daemon.run().await.context("el daemon terminó con error")?;
            Ok(ExitCode::SUCCESS)
        }
        DaemonCmd::Stop {
            socket,
            hard,
            handover,
        } => {
            // Resolving the default path can probe the FS (hard rule 2):
            // off the runtime thread.
            let socket = match socket {
                Some(s) => s,
                None => tokio::task::spawn_blocking(|| default_socket_path(None))
                    .await
                    .context("resolución del socket")?,
            };
            let mut client = Client::connect(&socket)
                .await
                .context("no hay daemon escuchando en el socket")?;
            let init = client
                .initialize(norte_proto::methods::ClientInfo {
                    name: "norte-cli".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                })
                .await
                .context("initialize")?;
            // A daemon older than 0.46 IGNORES `mode` and does an
            // ordinary stop: frontends are not warned and do not come
            // back on their own. Without this check the CLI would say
            // "done" about something that did not happen — and it is the
            // NORMAL case, because the first upgrade to 0.46 is, by
            // definition, received by a 0.45 daemon.
            let supports_handover =
                norte_proto::methods::version_at_least(&init.protocol_version, 0, 46);
            if handover && !supports_handover {
                eprintln!(
                    "{}",
                    norte_i18n::ta(
                        "cli-daemon-handover-unsupported",
                        &[("version", init.protocol_version.as_str())],
                    )
                );
            }
            let _: norte_proto::methods::DaemonShutdownResult = client
                .call(
                    norte_proto::methods::DAEMON_SHUTDOWN,
                    &norte_proto::methods::DaemonShutdownParams {
                        graceful: !hard,
                        mode: if handover {
                            norte_proto::methods::ShutdownMode::Handover
                        } else {
                            norte_proto::methods::ShutdownMode::Stop
                        },
                    },
                )
                .await
                .context("daemon.shutdown")?;
            eprintln!(
                "{}",
                if handover && supports_handover {
                    norte_i18n::t("cli-daemon-handover-requested")
                } else {
                    norte_i18n::t("cli-daemon-stopped")
                }
            );
            Ok(ExitCode::SUCCESS)
        }
    }
}
