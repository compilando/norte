//! CLI de humo de M0: `ls`, `cp`, `mv`, `rm` sobre el core embebido.
//!
//! Frontend SIN lógica de negocio (regla dura 7): todo pasa por `Engine`.
//! Strings hardcodeados a propósito: Fluent llega en M1 (deuda registrada).
#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};
use norte_core::backend::{Backend, TaskRef};
use norte_core::{Engine, TransferOptions};
use norte_proto::{Entry, EntryKind, SymlinkPolicy, TaskState, VPath};
use norte_vfs::Provider;
use norte_vfs_local::LocalProvider;

/// Código de salida convencional para "interrumpido por SIGINT".
const EXIT_CANCELLED: u8 = 130;

#[derive(Parser)]
#[command(
    name = "norte",
    version,
    about = "file manager ortodoxo — CLI de humo (M0)"
)]
struct Cli {
    /// Opera contra el daemon (arrancándolo si hace falta) en vez del
    /// core embebido. Solo unix (ADR 0011).
    #[arg(long, global = true)]
    daemon: bool,
    /// Socket del daemon (default: `$XDG_RUNTIME_DIR/norte/daemon.sock`)
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Lista un directorio
    Ls {
        /// Directorio a listar
        path: PathBuf,
        /// Salida JSON (paths en forma wire, lossless)
        #[arg(long)]
        json: bool,
    },
    /// Copia archivo o directorio (recursivo), con progreso y Ctrl-C limpio
    Cp {
        /// Origen
        src: PathBuf,
        /// Destino EXACTO (si existe: conflicto, jamás sobrescribe)
        dst: PathBuf,
        /// Política de symlinks (ADR 0005)
        #[arg(long, value_enum, default_value = "preserve")]
        symlinks: SymlinksArg,
        /// Reanudar una copia interrumpida (deja/usa `.norte-partial`, ADR 0012)
        #[arg(long)]
        resume: bool,
    },
    /// Mueve/renombra, con progreso y Ctrl-C limpio
    Mv {
        /// Origen
        src: PathBuf,
        /// Destino exacto
        dst: PathBuf,
        /// Política de symlinks (ADR 0005; solo aplica al camino copy+delete)
        #[arg(long, value_enum, default_value = "preserve")]
        symlinks: SymlinksArg,
        /// Reanudar un movimiento interrumpido (camino copy+delete, ADR 0012)
        #[arg(long)]
        resume: bool,
    },
    /// Borra archivo o directorio (recursivo), con progreso y Ctrl-C limpio
    /// Borra PERMANENTE (banco de pruebas del engine; la papelera vive
    /// en el TUI — ADR 0009).
    Rm {
        /// Nodo a borrar
        path: PathBuf,
    },
    /// Establece una conexión remota (por nombre de `connections.toml` o
    /// URL `sftp://…`/`ftp://…`), con el flujo TOFU interactivo (fase 6e)
    Connect {
        /// Nombre de la conexión o URL remota
        target: String,
    },
    /// Daemon JSON-RPC sobre UDS (ADR 0011; solo unix en M2)
    #[cfg(unix)]
    Daemon {
        #[command(subcommand)]
        cmd: DaemonCmd,
    },
}

/// Subcomandos del daemon.
#[cfg(unix)]
#[derive(Subcommand)]
enum DaemonCmd {
    /// Sirve en primer plano hasta shutdown (petición, SIGTERM/Ctrl-C o
    /// inactividad)
    Run {
        /// Path del socket (default: `$XDG_RUNTIME_DIR/norte/daemon.sock`)
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Apagado tras N segundos sin clientes ni tasks (0 = nunca)
        #[arg(long, default_value_t = 300)]
        idle_timeout: u64,
    },
    /// Pide el apagado al daemon en marcha
    Stop {
        /// Path del socket (default: el mismo que run)
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Cancela las tasks vivas en vez de esperarlas
        #[arg(long)]
        hard: bool,
    },
}

/// Política de symlinks de `cp`/`mv` (mapea 1:1 a la del protocolo).
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum SymlinksArg {
    /// Copia el LINK tal cual (como `cp -a`)
    Preserve,
    /// Los symlinks no se copian
    Skip,
    /// Copia el CONTENIDO apuntado; los dir-symlinks se expanden como
    /// dirs reales (un ciclo de links aborta la operación)
    Follow,
}

/// `--resume` → política (opt-in; sin el flag, contrato de M1).
fn resume_policy(on: bool) -> norte_proto::ResumePolicy {
    if on {
        norte_proto::ResumePolicy::On
    } else {
        norte_proto::ResumePolicy::Off
    }
}

impl From<SymlinksArg> for SymlinkPolicy {
    fn from(a: SymlinksArg) -> Self {
        match a {
            SymlinksArg::Preserve => SymlinkPolicy::Preserve,
            SymlinksArg::Skip => SymlinkPolicy::Skip,
            SymlinksArg::Follow => SymlinkPolicy::Follow,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!(
                "{}",
                norte_i18n::ta("cli-runtime-error", &[("error", &e.to_string())])
            );
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(run(cli)) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("norte: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    // Tracing con el cap de seguridad `suppaftp=info` (issue #43, regla 10):
    // sin esto un `RUST_LOG=trace` volcaría `PASS <password>` de suppaftp.
    norte_core::logging::init();

    let engine = Engine::new();
    engine.register_provider(Arc::new(LocalProvider::os_root()) as Arc<dyn Provider>);
    // Conexiones remotas bajo demanda (fase 6e): connections.toml +
    // known_hosts + secretos en el dir de config del usuario. Aplica tanto
    // al modo embebido como al engine que sirve `norte daemon run`.
    engine.set_connector(Arc::new(norte_core::connect::ConnectionManager::new(
        norte_core::connect::config_dir(),
    )));

    // El subcomando daemon usa el engine directo (ES el daemon).
    #[cfg(unix)]
    if let Cmd::Daemon { cmd } = cli.cmd {
        return daemon_cmd(engine, cmd).await;
    }

    let backend = make_backend(engine, cli.daemon, cli.socket).await?;
    match cli.cmd {
        Cmd::Ls { path, json } => ls(&backend, &path, json).await,
        Cmd::Connect { target } => connect_cmd(&backend, &target, cli.daemon).await,
        Cmd::Cp {
            src,
            dst,
            symlinks,
            resume,
        } => {
            let (from, to) = (vpath(&src)?, vpath(&dst)?);
            let opts = TransferOptions {
                symlinks: symlinks.into(),
                resume: resume_policy(resume),
                ..TransferOptions::default()
            };
            let task = match backend.copy(&from, &to, opts).await {
                // Primer contacto TOFU: confirmar y reintentar UNA vez.
                Err(e) if tofu_confirm(&backend, &e).await? => backend.copy(&from, &to, opts).await,
                other => other,
            }
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context(norte_i18n::t("cli-enqueue-copy"))?;
            Ok(run_task(task, true).await)
        }
        Cmd::Mv {
            src,
            dst,
            symlinks,
            resume,
        } => {
            let (from, to) = (vpath(&src)?, vpath(&dst)?);
            let opts = TransferOptions {
                symlinks: symlinks.into(),
                resume: resume_policy(resume),
                ..TransferOptions::default()
            };
            let task = match backend.move_(&from, &to, opts).await {
                Err(e) if tofu_confirm(&backend, &e).await? => {
                    backend.move_(&from, &to, opts).await
                }
                other => other,
            }
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context(norte_i18n::t("cli-enqueue-move"))?;
            Ok(run_task(task, false).await)
        }
        Cmd::Rm { path } => {
            let target = vpath(&path)?;
            let task = match backend
                .delete(&target, norte_proto::DeleteMode::Permanent)
                .await
            {
                Err(e) if tofu_confirm(&backend, &e).await? => {
                    backend
                        .delete(&target, norte_proto::DeleteMode::Permanent)
                        .await
                }
                other => other,
            }
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context(norte_i18n::t("cli-enqueue-delete"))?;
            Ok(run_task(task, false).await)
        }
        #[cfg(unix)]
        Cmd::Daemon { .. } => unreachable!("manejado arriba"),
    }
}

/// Elige el transporte (regla 7: la lógica es la misma). `--daemon`
/// conecta al socket, arrancando `norte daemon run` si hace falta.
async fn make_backend(
    engine: Engine,
    daemon: bool,
    socket: Option<PathBuf>,
) -> anyhow::Result<Backend> {
    if !daemon {
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
        // Autoarranque: este MISMO binario sabe ser daemon.
        let mut spawn_cmd: Vec<std::ffi::OsString> =
            vec![exe.into(), "daemon".into(), "run".into()];
        spawn_cmd.push("--socket".into());
        spawn_cmd.push(socket.clone().into());
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
#[cfg(unix)]
async fn daemon_cmd(engine: Engine, cmd: DaemonCmd) -> anyhow::Result<ExitCode> {
    use norte_core::daemon::{Client, Daemon, DaemonConfig, default_socket_path};
    match cmd {
        DaemonCmd::Run {
            socket,
            idle_timeout,
        } => {
            let daemon = Daemon::bind(
                std::sync::Arc::new(engine),
                DaemonConfig {
                    socket_path: socket,
                    idle_timeout: (idle_timeout > 0)
                        .then(|| std::time::Duration::from_secs(idle_timeout)),
                    ..DaemonConfig::default()
                },
            )
            .await
            .context("no se pudo enlazar el daemon")?;
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
        DaemonCmd::Stop { socket, hard } => {
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
            client
                .initialize(norte_proto::methods::ClientInfo {
                    name: "norte-cli".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                })
                .await
                .context("initialize")?;
            let _: norte_proto::methods::DaemonShutdownResult = client
                .call(
                    norte_proto::methods::DAEMON_SHUTDOWN,
                    &norte_proto::methods::DaemonShutdownParams { graceful: !hard },
                )
                .await
                .context("daemon.shutdown")?;
            eprintln!("{}", norte_i18n::t("cli-daemon-stopped"));
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Schemes remotos que la CLI enruta por URL. ALLOWLIST explícita: un path
/// local puede llamarse legalmente `a://b` (o `./x://y`) y debe seguir
/// siendo un fichero — solo lo que empieza EXACTAMENTE por estos prefijos se
/// trata como URL remota.
const REMOTE_SCHEMES: [&str; 3] = ["sftp://", "ftp://", "s3://"];

/// ¿El arg es una URL de archivo-como-directorio (ADR 0018)? Exige la forma
/// completa `<formato>+<scheme>://…` con formato de la whitelist de proto
/// (la reserva normativa garantiza que ningún provider legítimo empieza
/// así, test abajo): un path local raro tipo `zip+dir/sub://y` sigue
/// siendo nativo.
fn is_archive_url(s: &str) -> bool {
    let Some((scheme, _)) = s.split_once("://") else {
        return false;
    };
    let Some((fmt, inner)) = scheme.split_once('+') else {
        return false;
    };
    norte_proto::ARCHIVE_FORMATS.contains(&fmt) && !inner.is_empty() && !inner.contains('/')
}

fn vpath(path: &std::path::Path) -> anyhow::Result<VPath> {
    // Una URL remota va por el parser wire; todo lo demás es un path NATIVO
    // local (bytes, jamás forzados a UTF-8 — un arg no-UTF8 no puede ser URL
    // y cae al camino nativo).
    if let Some(s) = path.to_str()
        && (REMOTE_SCHEMES.iter().any(|p| s.starts_with(p)) || is_archive_url(s))
    {
        reject_inline_password(s)?;
        return VPath::parse(s).with_context(|| norte_i18n::ta("cli-invalid-url", &[("url", s)]));
    }
    norte_vfs_local::vpath_from_native(path)
        .with_context(|| format!("path no representable: {}", path.display()))
}

/// Rechaza `user:pass@host` en una URL ANTES de que entre a `VPath::parse`
/// (que la aceptaría) y por tanto a spans/errores: mensaje ESTÁTICO, sin
/// ecoar la URL (regla 10). El parser de conexiones la rechazaría después,
/// pero para entonces ya habría tocado logs.
fn reject_inline_password(url: &str) -> anyhow::Result<()> {
    let after_scheme = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = after_scheme.split('/').next().unwrap_or(after_scheme);
    if let Some((userinfo, _)) = authority.rsplit_once('@')
        && userinfo.contains(':')
    {
        anyhow::bail!(norte_i18n::t("cli-inline-password"));
    }
    Ok(())
}

/// Flujo TOFU interactivo (ADR 0015 D/G): ante un `Error::HostKeyUnknown`
/// muestra la huella, pide confirmación por el terminal y, si el usuario
/// acepta, la registra (el core re-verifica anti-TOCTOU) y devuelve `true`
/// (reintentar la operación). Errores que no son TOFU → `false` (el caller
/// reporta el original). Sin terminal NO se confía nada: instrucciones y error.
async fn tofu_confirm(backend: &Backend, err: &norte_proto::Error) -> anyhow::Result<bool> {
    use std::io::IsTerminal;
    let norte_proto::Error::HostKeyUnknown {
        host,
        port,
        algo,
        fingerprint,
    } = err
    else {
        return Ok(false);
    };
    let port_shown = port.unwrap_or(22).to_string();
    eprintln!(
        "{}",
        norte_i18n::ta(
            "cli-hostkey-unknown",
            &[("host", host.as_str()), ("port", port_shown.as_str())]
        )
    );
    eprintln!(
        "{}",
        norte_i18n::ta(
            "cli-hostkey-fingerprint",
            &[
                ("algo", algo.as_str()),
                ("fingerprint", fingerprint.as_str())
            ]
        )
    );
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(norte_i18n::t("cli-hostkey-noninteractive"));
    }
    eprint!("{} ", norte_i18n::t("cli-hostkey-prompt"));
    // stdin es bloqueante: fuera del reactor (regla 2).
    let line = tokio::task::spawn_blocking(|| {
        let mut s = String::new();
        std::io::stdin().read_line(&mut s).map(|_| s)
    })
    .await
    .context(norte_i18n::t("cli-confirm-read"))??;
    let yes = matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "s" | "si" | "sí" | "y" | "yes"
    );
    if !yes {
        anyhow::bail!(norte_i18n::t("cli-hostkey-refused"));
    }
    backend
        .trust_host_key(host, *port, algo, fingerprint)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    eprintln!("{}", norte_i18n::t("cli-hostkey-trusted"));
    Ok(true)
}

/// `norte connect <nombre|url>`: establece la conexión (disparando el flujo
/// TOFU si es el primer contacto) y confirma. El valor duradero es el
/// registro de la host key + la validación de credenciales.
async fn connect_cmd(backend: &Backend, target: &str, daemon: bool) -> anyhow::Result<ExitCode> {
    if daemon {
        // La resolución por nombre lee el config LOCAL; contra un daemon
        // remoto la semántica cambia — se difiere (mínimo viable, ADR 0015 G).
        anyhow::bail!(norte_i18n::t("cli-connect-daemon-unsupported"));
    }
    let url = if target.contains("://") {
        target.to_string()
    } else {
        // Nombre de connections.toml → su URL (el core la resuelve).
        norte_core::connect::named_url(&norte_core::connect::config_dir(), target)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context(norte_i18n::t("cli-connect-failed"))?
    };
    reject_inline_password(&url)?;
    let root = VPath::parse(&url)
        .with_context(|| norte_i18n::ta("cli-invalid-url", &[("url", url.as_str())]))?;
    // capabilities fuerza el establecimiento por el camino normal del engine.
    let result = match backend.capabilities(&root).await {
        Err(e) if tofu_confirm(backend, &e).await? => backend.capabilities(&root).await,
        other => other,
    };
    result
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context(norte_i18n::t("cli-connect-failed"))?;
    println!(
        "{}",
        norte_i18n::ta("cli-connect-ok", &[("target", target)])
    );
    Ok(ExitCode::SUCCESS)
}

async fn ls(backend: &Backend, path: &std::path::Path, json: bool) -> anyhow::Result<ExitCode> {
    let target = vpath(path)?;
    let entries: Vec<Entry> = match backend.list(&target).await {
        // Primer contacto TOFU: confirmar y reintentar UNA vez.
        Err(e) if tofu_confirm(backend, &e).await? => backend.list(&target).await,
        other => other,
    }
    .map_err(|e| anyhow::anyhow!("{e}"))
    .context(norte_i18n::t("cli-list-failed"))?;
    if json {
        // Forma wire (lossless); el consumidor decodifica con el codec.
        serde_json::to_writer_pretty(std::io::stdout().lock(), &entries)
            .context(norte_i18n::t("cli-serialize-failed"))?;
        println!();
    } else {
        for e in &entries {
            let marker = match e.kind {
                EntryKind::Dir => "d",
                EntryKind::File => "-",
                EntryKind::Symlink => "l",
                EntryKind::Other => "?",
            };
            let size = e.size.map_or_else(String::new, |s| s.to_string());
            println!("{marker}\t{size}\t{}", e.path.display_lossy());
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Corre una Task pintando progreso en stderr; Ctrl-C cancela cooperativamente
/// (la task deja destino limpio o `.norte-partial`, regla dura 3).
async fn run_task(task: TaskRef, show_bytes: bool) -> ExitCode {
    let canceller = task.canceller();
    let sig = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\n{}", norte_i18n::t("cli-cancelling"));
            canceller.cancel();
        }
    });

    let mut rx = task.progress();
    loop {
        let snap = rx.borrow_and_update().clone();
        render(&snap, show_bytes);
        if snap.state.is_terminal() {
            break;
        }
        if rx.changed().await.is_err() {
            break;
        }
    }
    sig.abort();
    let final_state = rx.borrow().state.clone();
    eprintln!();
    match final_state {
        TaskState::Completed => ExitCode::SUCCESS,
        TaskState::Cancelled => {
            eprintln!("{}", norte_i18n::t("cli-cancelled-clean"));
            ExitCode::from(EXIT_CANCELLED)
        }
        TaskState::Failed { error } => {
            eprintln!(
                "{}",
                norte_i18n::ta("cli-final-error", &[("error", &error.to_string())])
            );
            ExitCode::FAILURE
        }
        other => {
            eprintln!(
                "{}",
                norte_i18n::ta("cli-unexpected-state", &[("state", &format!("{other:?}"))])
            );
            ExitCode::FAILURE
        }
    }
}

fn render(p: &norte_proto::TaskProgress, show_bytes: bool) {
    let entries = match p.entries_total {
        Some(t) => format!("{}/{t}", p.entries_done),
        None => format!("{}/?", p.entries_done),
    };
    if show_bytes {
        let bytes = match p.bytes_total {
            Some(t) if t > 0 => {
                let pct = p.bytes_done.saturating_mul(100) / t;
                format!("{} / {t} bytes ({pct}%)", p.bytes_done)
            }
            _ => format!("{} bytes", p.bytes_done),
        };
        eprint!("\r{bytes} — {entries} entradas   ");
    } else {
        eprint!("\r{entries} entradas   ");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reserva normativa de ADR 0018: ningún scheme remoto de la allowlist
    /// puede empezar por `<formato>+` — el registro de formatos manda.
    #[test]
    fn remote_schemes_respetan_la_reserva_de_formatos() {
        for scheme in REMOTE_SCHEMES {
            for format in norte_proto::ARCHIVE_FORMATS {
                assert!(
                    !scheme.starts_with(&format!("{format}+")),
                    "{scheme} invade el namespace del formato {format}"
                );
            }
        }
    }

    #[test]
    fn urls_de_archivo_van_por_el_parser_wire() {
        let p = vpath(std::path::Path::new("zip+file:///tmp/a.zip/!/x")).expect("parsea");
        assert_eq!(p.scheme(), "zip+file");
        // Un path local que solo se PARECE (sin `://`) sigue siendo nativo.
        let p = vpath(std::path::Path::new("zip+file")).expect("nativo");
        assert_eq!(p.scheme(), "file");
        // Password inline en un compuesto remoto: mismo guard que siempre.
        // Directo contra el guard (el parse TAMBIÉN lo rechaza desde #46,
        // pero este test protege la defensa en profundidad de la CLI).
        assert!(reject_inline_password("tar+sftp://u:pass@h/a.tar/!").is_err());
        assert!(vpath(std::path::Path::new("tar+sftp://u:pass@h/a.tar/!")).is_err());
        // Paths locales patológicos que se PARECEN: nativos, no URL.
        for nativo in ["zip+dir/sub://y", "tar+xz", "zip+://x"] {
            assert!(!is_archive_url(nativo), "{nativo} debe ser nativo");
        }
    }
}
