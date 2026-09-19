//! `norte daemon run|stop`, `norte mcp serve`, `norte policy grant` y
//! `norte undo` (ADR 0011, M3-4).

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context;
use norte_core::Engine;
use norte_core::backend::Backend;
use norte_vfs::Provider;
use norte_vfs_local::LocalProvider;

use crate::cmd::connect::apply_archive_limits;
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
    use norte_core::daemon::{
        Client, Daemon, DaemonApprovalResolver, DaemonConfig, default_socket_path,
    };
    match cmd {
        DaemonCmd::Run {
            socket,
            idle_timeout,
        } => {
            // El daemon es el dueño ÚNICO del journal (spec §4, ADR 0024) y
            // quien instala policy + approvals: los agentes MCP se gobiernan
            // aquí, jamás en el puente.
            let journal_path = norte_core::connect::config_dir().join("journal.db");
            // El aviso NOMBRA la causa probable: desde #167 un frontend embebido
            // (un `ntc` sin `--daemon`) se queda el lock exclusivo, y sin esta
            // frase el operador recibe un texto de sqlx y ninguna pista de qué
            // cerrar. Desde #177 ese frontend solo lo tiene si YA MUTÓ algo, así
            // que el caso corriente —arrancar el daemon con un `ntc` abierto—
            // volvió a funcionar.
            let journal = norte_core::SqliteJournal::open(&journal_path)
                .await
                .context(
                    "no se pudo abrir el journal (si dice «database is locked», otro \
                     proceso norte lo tiene: ¿un `ntc` embebido, u otro daemon?)",
                )?;
            // Barrido de spools de sincronización (ADR 0049), junto al journal
            // y por lo mismo: un cierre violento deja detrás ficheros que
            // AUTORIZAN escrituras, y nadie más los va a recoger. Se llevan
            // todos, no solo los caducados — aquí no hay ninguna conexión viva
            // todavía, así que todo spool que exista es de una conexión muerta.
            //
            // Va DESPUÉS del journal a propósito: su lock exclusivo es lo que
            // garantiza que no hay otro daemon sobre este directorio de estado
            // al que le estemos barriendo un plan vivo.
            //
            // Un barrido que falla NO impide arrancar. Lo que impide aplicar un
            // plan de un arranque anterior no es esto, es que el registro de
            // planes emitidos vive en memoria y nace vacío; lo que queda en
            // disco es basura, y un daemon que se niega a arrancar por un
            // fichero que no se deja borrar es peor fallo que el que evita.
            //
            // El `Spool` se construye UNA vez y se clona: dos `Spool::new` son
            // dos registros de emisión que no se ven. Este de aquí barre Y es el
            // que se le instala al engine unas líneas más abajo, que es de donde
            // lo saca `sync.plan` y el cierre de cada conexión.
            let spool = norte_core::sync::Spool::new(norte_core::connect::config_dir());
            match spool.sweep().await {
                Ok(r) if r.removed == 0 && r.is_clean() => {}
                Ok(r) if r.is_clean() => {
                    eprintln!(
                        "{}",
                        norte_i18n::ta("cli-spool-swept", &[("count", &r.removed.to_string())])
                    );
                }
                Ok(r) => eprintln!(
                    "aviso: barridos {} planes de sync huérfanos y {} no se dejaron borrar en {}",
                    r.removed,
                    r.failed,
                    spool.dir().display()
                ),
                Err(e) => eprintln!(
                    "{}",
                    norte_i18n::ta(
                        "cli-spool-sweep-failed",
                        &[
                            ("path", &spool.dir().display().to_string()),
                            ("error", &e.to_string()),
                        ]
                    )
                ),
            }
            // policy.toml: ausente = sin reglas = un agente DENTRO de scope
            // aún deniega (fail-closed, `no-rule`). docs/policy-example.toml
            // trae el punto de partida (`action = "ask"`).
            let cfg = tokio::task::spawn_blocking(norte_core::PolicyConfig::load)
                .await
                .context("carga de policy.toml")?
                .context("policy.toml inválido")?;
            let scopes = norte_core::ScopeRegistry::new();
            let approvals = std::sync::Arc::new(DaemonApprovalResolver::default());
            let engine = Engine::with_journal(std::sync::Arc::new(journal)).with_policy(
                std::sync::Arc::new(norte_core::ScopedPolicy::new(scopes.clone(), cfg)),
                std::sync::Arc::clone(&approvals) as _,
            );
            // Índice de búsqueda (M4, ADR 0034): junto al journal en config_dir.
            // Si no abre, se sigue sin él (index.* → Unsupported, fail-closed).
            let index_path = norte_core::connect::config_dir().join("index.db");
            let engine = match norte_core::Index::open(&index_path).await {
                Ok(idx) => engine.with_index(std::sync::Arc::new(idx)),
                Err(e) => {
                    eprintln!(
                        "{}",
                        norte_i18n::ta("cli-warn-no-index", &[("error", &e.to_string())])
                    );
                    engine
                }
            };
            // EL spool, el mismo que acaba de barrer: sin él `sync.plan`
            // responde `Unsupported` (un plan que no se puede retener tampoco se
            // puede aplicar).
            engine.set_spool(spool);
            engine.register_provider(Arc::new(LocalProvider::os_root()) as Arc<dyn Provider>);
            apply_archive_limits(&engine).await?;
            engine.set_connector(std::sync::Arc::new(
                norte_core::connect::ConnectionManager::new(norte_core::connect::config_dir()),
            ));
            // IA (M4-IA, ADR 0031): opt-in. Sin [ai], sin proveedor o con
            // config rota el engine degrada (ai.* → Unsupported / gate
            // PolicyDenied); jamás aborta el arranque del daemon.
            match tokio::task::spawn_blocking(norte_core::ai::AiConfig::load).await {
                Ok(Ok(config)) => {
                    if let Some(pcfg) = config.rename_provider_config().cloned() {
                        match norte_core::ai::resolve_and_build(
                            &pcfg,
                            norte_core::connect::config_dir(),
                        )
                        .await
                        {
                            Ok(provider) => engine.set_ai_provider(provider),
                            Err(e) => eprintln!(
                                "aviso: proveedor de IA no disponible ({e}); ai.* dará Unsupported"
                            ),
                        }
                    }
                    // Embeddings (M4-IA-2): proveedor propio, opt-in igual.
                    if let Some(w) = norte_core::ai::install_embed_provider(&engine, &config).await
                    {
                        eprintln!("{w}");
                    }
                    engine.set_ai_config(config);
                }
                Ok(Err(e)) => eprintln!(
                    "{}",
                    norte_i18n::ta("cli-warn-ai-invalid", &[("error", &e.to_string())])
                ),
                Err(e) => eprintln!(
                    "{}",
                    norte_i18n::ta("cli-warn-ai-load-failed", &[("error", &e.to_string())])
                ),
            }
            let daemon = Daemon::bind_with_policy(
                std::sync::Arc::new(engine),
                scopes,
                approvals,
                DaemonConfig {
                    socket_path: socket,
                    idle_timeout: (idle_timeout > 0)
                        .then(|| std::time::Duration::from_secs(idle_timeout)),
                    // La sesión de UI (L2) vive en el directorio de estado, y
                    // se pasa EXPLÍCITA: el default no persiste nada, para que
                    // ningún test ni embebedor escriba el estado real por
                    // descuido. Sin directorio de estado —un entorno sin HOME—
                    // el daemon sirve la pantalla y no la guarda.
                    state_dir: norte_config::dirs::state_dir(),
                    ..DaemonConfig::default()
                },
            )
            .await
            .context("no se pudo enlazar el daemon")?;
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
