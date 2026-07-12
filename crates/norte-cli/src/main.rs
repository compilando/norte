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
use futures::StreamExt;
use norte_core::{Engine, TaskHandle, TransferOptions};
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
    },
    /// Borra archivo o directorio (recursivo), con progreso y Ctrl-C limpio
    /// Borra PERMANENTE (banco de pruebas del engine; la papelera vive
    /// en el TUI — ADR 0009).
    Rm {
        /// Nodo a borrar
        path: PathBuf,
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
    let engine = Engine::new();
    engine.register_provider(Arc::new(LocalProvider::os_root()) as Arc<dyn Provider>);

    match cli.cmd {
        Cmd::Ls { path, json } => ls(&engine, &path, json).await,
        Cmd::Cp { src, dst, symlinks } => {
            let (from, to) = (vpath(&src)?, vpath(&dst)?);
            let opts = TransferOptions {
                symlinks: symlinks.into(),
                ..TransferOptions::default()
            };
            let handle = engine
                .copy_with(&from, &to, opts)
                .context(norte_i18n::t("cli-enqueue-copy"))?;
            Ok(run_task(handle, true).await)
        }
        Cmd::Mv { src, dst, symlinks } => {
            let (from, to) = (vpath(&src)?, vpath(&dst)?);
            let opts = TransferOptions {
                symlinks: symlinks.into(),
                ..TransferOptions::default()
            };
            let handle = engine
                .move_with(&from, &to, opts)
                .context(norte_i18n::t("cli-enqueue-move"))?;
            Ok(run_task(handle, false).await)
        }
        Cmd::Rm { path } => {
            let target = vpath(&path)?;
            let handle = engine
                .delete(&target)
                .context(norte_i18n::t("cli-enqueue-delete"))?;
            Ok(run_task(handle, false).await)
        }
    }
}

fn vpath(path: &std::path::Path) -> anyhow::Result<VPath> {
    norte_vfs_local::vpath_from_native(path)
        .with_context(|| format!("path no representable: {}", path.display()))
}

async fn ls(engine: &Engine, path: &std::path::Path, json: bool) -> anyhow::Result<ExitCode> {
    let target = vpath(path)?;
    let mut stream = engine
        .list(&target)
        .await
        .context(norte_i18n::t("cli-list-failed"))?;
    let mut entries: Vec<Entry> = Vec::new();
    while let Some(item) = stream.next().await {
        entries.push(item.context(norte_i18n::t("cli-entry-unreadable"))?);
    }
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
async fn run_task(handle: TaskHandle, show_bytes: bool) -> ExitCode {
    let cancel = handle.cancel_token();
    let sig = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\n{}", norte_i18n::t("cli-cancelling"));
            cancel.cancel();
        }
    });

    let mut rx = handle.progress();
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
