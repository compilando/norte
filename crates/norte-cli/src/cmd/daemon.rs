//! `norte daemon run|stop`, `norte mcp serve`, `norte policy grant` y
//! `norte undo` (ADR 0011, M3-4).

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context;
use norte_core::Engine;
use norte_core::backend::Backend;

use crate::{DaemonCmd, McpCmd, PolicyCmd};

/// Resuelve el socket del daemon y un `spawn_cmd` de autoarranque (este mismo
/// binario sabe ser daemon). Sondas de FS fuera del runtime (regla 2).
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

/// `norte mcp serve`: sirve MCP por stdio, arrancando el daemon si hace falta.
/// El puente conecta como sesión de agente; el tracing va a stderr (el
/// `logging::init` global ya lo fija), stdout es EXCLUSIVO del transporte MCP.
#[cfg(unix)]
pub(crate) async fn mcp_cmd(cmd: McpCmd, socket: Option<PathBuf>) -> anyhow::Result<ExitCode> {
    let McpCmd::Serve { session } = cmd;
    let (socket, spawn_cmd) = socket_and_spawn(socket).await?;
    // Autoarranque idempotente: connect_or_spawn arranca el daemon si el
    // socket no responde, luego el puente reconecta.
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

/// `norte policy grant <request_id>`: un humano concede un scope pedido por un
/// agente. Conexión User (sin `agent_session`).
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

/// `norte undo <session>`: un humano deshace la sesión completa de un agente.
/// Corre como Task; se espera su terminal con Ctrl-C = cancelar (patrón `cp`).
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
    // «Done» CUALIFICADO (#71): el estado terminal de la Task es Completed
    // incluso si el LIFO se bloqueó o todo se saltó — el informe es la ÚNICA
    // señal de integridad. Solo un daemon N-1 (sin el método → Unsupported)
    // degrada al mensaje simple; cualquier otro fallo del fetch NO se traga:
    // aviso + exit≠0 (el undo pudo funcionar, pero queda sin verificar).
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

/// Elige el transporte (regla 7: la lógica es la misma). `--daemon`
/// conecta al socket, arrancando `norte daemon run` si hace falta.
pub(crate) async fn make_backend(
    engine: Engine,
    daemon: bool,
    socket: Option<PathBuf>,
) -> anyhow::Result<Backend> {
    if !daemon {
        // #44: los avisos de degradación se drenan en el dispatch (top-level)
        // vía `Backend::take_degraded`, que en embebido instala el observer del
        // canal — misma vía que en modo daemon (rust M1 + security m1).
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
        // Ambas sondas de FS (socket por defecto + current_exe) fuera del
        // runtime (regla 2, m3 del rust-reviewer).
        let (socket, exe) = tokio::task::spawn_blocking(move || {
            let socket = socket.unwrap_or_else(|| norte_core::daemon::default_socket_path(None));
            (socket, std::env::current_exe())
        })
        .await
        .context("resolución del socket/exe")?;
        let exe = exe.context("current_exe")?;
        // Autoarranque: este MISMO binario sabe ser daemon, con el argv
        // compartido — lo que arranca una orden suelta se apaga solo.
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

/// `norte daemon run|stop` (ADR 0011). El engine que sirve el daemon es el
/// MISMO embebido de esta CLI: solo cambia el transporte (regla 7).
///
/// `anillo` es el registro en memoria que montó `run` (#328): el MISMO en el
/// que escribe la capa de `tracing`, y el que `log.tail` sirve. `None` —el
/// montaje falló porque ya había subscriber— deja al daemon contestando
/// `Unsupported` a los dos métodos de registro, que es lo que el frontend
/// necesita para degradar diciendo por qué.
#[cfg(unix)]
#[expect(clippy::too_many_lines, reason = "un brazo por subcomando del daemon")]
pub(crate) async fn daemon_cmd(
    cmd: DaemonCmd,
    anillo: Option<norte_config::logring::LogRing>,
) -> anyhow::Result<ExitCode> {
    use norte_core::daemon::{Client, default_socket_path};
    match cmd {
        DaemonCmd::Run {
            socket,
            idle_timeout,
        } => {
            // QUÉ lleva el daemon lo decide el core (regla 7,
            // `norte_core::daemon::componer`); aquí queda lo del binario: pintar
            // los avisos en el idioma del operador, el anillo de registro y las
            // señales.
            use norte_core::daemon::componer::{Aviso, ErrorDeArranque, Opciones};
            let compuesto = norte_core::daemon::componer(Opciones {
                socket,
                idle_timeout: (idle_timeout > 0)
                    .then(|| std::time::Duration::from_secs(idle_timeout)),
                // La sesión de UI (L2) vive en el directorio de estado. Sin él
                // —un entorno sin HOME— el daemon sirve la pantalla y no la
                // guarda.
                state_dir: norte_config::dirs::state_dir(),
            })
            .await;
            let (daemon, avisos) = match compuesto {
                Ok(c) => c,
                // El aviso NOMBRA la causa probable: desde #167 un frontend
                // embebido (un `ntc` sin `--daemon`) se queda el lock exclusivo,
                // y sin esta frase el operador recibe un texto de sqlx y ninguna
                // pista de qué cerrar.
                Err(e @ ErrorDeArranque::Journal(_)) => {
                    return Err(anyhow::Error::new(e).context(
                        "no se pudo abrir el journal (si dice «database is locked», otro \
                         proceso norte lo tiene: ¿un `ntc` embebido, u otro daemon?)",
                    ));
                }
                Err(e) => return Err(e.into()),
            };
            for aviso in avisos {
                match aviso {
                    Aviso::SpoolsBarridos(n) => eprintln!(
                        "{}",
                        norte_i18n::ta("cli-spool-swept", &[("count", &n.to_string())])
                    ),
                    Aviso::SpoolsAMedias {
                        removed,
                        failed,
                        dir,
                    } => eprintln!(
                        "aviso: barridos {removed} planes de sync huérfanos y {failed} no se \
                         dejaron borrar en {}",
                        dir.display()
                    ),
                    Aviso::SpoolsSinBarrer { dir, error } => eprintln!(
                        "{}",
                        norte_i18n::ta(
                            "cli-spool-sweep-failed",
                            &[("path", &dir.display().to_string()), ("error", &error)]
                        )
                    ),
                    Aviso::SinIndice(error) => eprintln!(
                        "{}",
                        norte_i18n::ta("cli-warn-no-index", &[("error", &error)])
                    ),
                    Aviso::IaNoDisponible(e) => eprintln!(
                        "aviso: proveedor de IA no disponible ({e}); ai.* dará Unsupported"
                    ),
                    Aviso::IaEmbeddings(w) => eprintln!("{w}"),
                    Aviso::IaInvalida(error) => eprintln!(
                        "{}",
                        norte_i18n::ta("cli-warn-ai-invalid", &[("error", &error)])
                    ),
                    Aviso::IaNoCargo(error) => eprintln!(
                        "{}",
                        norte_i18n::ta("cli-warn-ai-load-failed", &[("error", &error)])
                    ),
                }
            }
            // El anillo se monta entre el bind y el `run`, que es donde puede
            // montarse: lo tiene el proceso (lo creó el subscriber), no la
            // config del daemon.
            let daemon = match anillo {
                Some(anillo) => daemon.with_log_ring(anillo),
                None => daemon,
            };
            eprintln!(
                "{}",
                norte_i18n::ta(
                    "cli-daemon-listening",
                    &[("socket", &daemon.socket_path().display().to_string())]
                )
            );
            // Ctrl-C/SIGTERM = shutdown graceful (las tasks terminan);
            // la SEGUNDA señal escala a hard (cancela tasks) — sin ella,
            // una task colgada solo moriría con SIGKILL (M4 rust-reviewer).
            // El registro va ANTES del spawn: si falla, error visible, no
            // un panic tragado dentro de un task (M5).
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
            // La resolución del path por defecto puede sondear el FS
            // (regla 2): fuera del hilo del runtime.
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
            // Un daemon anterior a 0.46 IGNORA `mode` y hace una parada
            // corriente: a los frontends no se les avisa y no vuelven solos.
            // Sin esta comprobación la CLI diría «hecho» de algo que no pasó —
            // y es el caso NORMAL, porque la primera actualización a 0.46 la
            // recibe por definición un daemon 0.45.
            let sabe_relevar =
                norte_proto::methods::version_at_least(&init.protocol_version, 0, 46);
            if handover && !sabe_relevar {
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
                if handover && sabe_relevar {
                    norte_i18n::t("cli-daemon-handover-requested")
                } else {
                    norte_i18n::t("cli-daemon-stopped")
                }
            );
            Ok(ExitCode::SUCCESS)
        }
    }
}
