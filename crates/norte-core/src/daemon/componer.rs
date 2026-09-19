//! Componer el daemon de `norte daemon run`: qué piezas lleva y en qué orden.
//!
//! Vivía en la CLI (regla 7): el journal, el barrido de spools, la policy, el
//! índice, la IA, los límites de archivo y el `bind` son decisiones del core,
//! y el binario que las hacía era el único sitio donde un segundo arranque —un
//! test, otro frontend, un embebedor— no las podía repetir sin copiarlas.
//!
//! Aquí se COMPONE y no se habla: lo que el operador tiene que saber vuelve
//! como [`Aviso`] tipado y lo pinta quien lanza, en su idioma. Lo que impide
//! arrancar vuelve como [`ErrorDeArranque`].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use norte_vfs::Provider;

use super::{Daemon, DaemonApprovalResolver, DaemonConfig, DaemonError};
use crate::Engine;

/// Lo que decide quien lanza el daemon.
#[derive(Debug, Clone, Default)]
pub struct Opciones {
    /// El socket; `None` = el del sistema.
    pub socket: Option<PathBuf>,
    /// Cuánto sin clientes antes de salir; `None` = nunca.
    pub idle_timeout: Option<Duration>,
    /// Dónde persiste la sesión de UI; `None` = no se persiste.
    pub state_dir: Option<PathBuf>,
}

/// Algo que el operador debe saber y que NO impide arrancar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Aviso {
    /// Se barrieron `n` planes de sincronización huérfanos.
    SpoolsBarridos(usize),
    /// El barrido se llevó unos y otros no se dejaron borrar.
    SpoolsAMedias {
        /// Barridos.
        removed: usize,
        /// Los que se resistieron.
        failed: usize,
        /// Dónde.
        dir: PathBuf,
    },
    /// El barrido no pudo ni empezar.
    SpoolsSinBarrer {
        /// Dónde.
        dir: PathBuf,
        /// Por qué.
        error: String,
    },
    /// Sin índice de búsqueda: `index.*` contestará `Unsupported`.
    SinIndice(String),
    /// El proveedor de IA de renombrado no está disponible.
    IaNoDisponible(String),
    /// Lo que dijo la instalación del proveedor de embeddings.
    IaEmbeddings(String),
    /// `[ai]` no es válido.
    IaInvalida(String),
    /// `[ai]` no se pudo leer.
    IaNoCargo(String),
}

/// Lo que impide arrancar.
#[derive(Debug, thiserror::Error)]
pub enum ErrorDeArranque {
    /// El journal no abre; con «database is locked», otro proceso lo tiene.
    #[error("no se pudo abrir el journal: {0}")]
    Journal(#[source] crate::journal::JournalError),
    /// `policy.toml` no se leyó o no es válido.
    #[error("policy.toml: {0}")]
    Policy(#[source] std::io::Error),
    /// `[archive]` de `norte.toml` no se leyó o no es válido.
    #[error("norte.toml ([archive]): {0}")]
    Archivo(#[source] std::io::Error),
    /// El socket no se pudo enlazar.
    #[error("no se pudo enlazar el daemon: {0}")]
    Bind(#[source] DaemonError),
}

/// Compone el daemon con todo lo suyo, listo para `run`.
///
/// El orden importa y está escrito en cada paso: el journal PRIMERO, porque su
/// lock exclusivo es lo que garantiza que no hay otro daemon sobre este
/// directorio de estado; el barrido de spools DESPUÉS, por eso mismo.
///
/// # Errors
/// [`ErrorDeArranque`]: el journal, la policy, los límites de archivo o el
/// `bind`. Todo lo demás degrada con su [`Aviso`].
pub async fn componer(o: Opciones) -> Result<(Daemon, Vec<Aviso>), ErrorDeArranque> {
    let mut avisos = Vec::new();
    let dir = crate::connect::config_dir();
    // El daemon es el dueño ÚNICO del journal (spec §4, ADR 0024) y quien
    // instala policy + approvals: los agentes MCP se gobiernan aquí, jamás en
    // el puente.
    let journal = crate::SqliteJournal::open(&dir.join("journal.db"))
        .await
        .map_err(ErrorDeArranque::Journal)?;
    // Barrido de spools de sincronización (ADR 0049), junto al journal y por
    // lo mismo: un cierre violento deja ficheros que AUTORIZAN escrituras, y
    // nadie más los va a recoger. Se llevan todos: aquí no hay ninguna conexión
    // viva todavía. Un barrido que falla NO impide arrancar —lo que impide
    // aplicar un plan viejo es que el registro de planes nace vacío—.
    //
    // El `Spool` se construye UNA vez: dos `Spool::new` son dos registros de
    // emisión que no se ven, y este es el que se instala en el engine.
    let spool = crate::sync::Spool::new(dir.clone());
    match spool.sweep().await {
        Ok(r) if r.removed == 0 && r.is_clean() => {}
        Ok(r) if r.is_clean() => avisos.push(Aviso::SpoolsBarridos(r.removed)),
        Ok(r) => avisos.push(Aviso::SpoolsAMedias {
            removed: r.removed,
            failed: r.failed,
            dir: spool.dir().to_path_buf(),
        }),
        Err(e) => avisos.push(Aviso::SpoolsSinBarrer {
            dir: spool.dir().to_path_buf(),
            error: e.to_string(),
        }),
    }
    // policy.toml: ausente = sin reglas = un agente DENTRO de scope aún
    // deniega (fail-closed, `no-rule`).
    let cfg = crate::blocking::spawn_blocking(crate::PolicyConfig::load)
        .await
        .map_err(|e| ErrorDeArranque::Policy(std::io::Error::other(e)))?
        .map_err(ErrorDeArranque::Policy)?;
    let scopes = crate::ScopeRegistry::new();
    let approvals = Arc::new(DaemonApprovalResolver::default());
    let engine = Engine::with_journal(Arc::new(journal)).with_policy(
        Arc::new(crate::ScopedPolicy::new(scopes.clone(), cfg)),
        Arc::clone(&approvals) as _,
    );
    // Índice de búsqueda (M4, ADR 0034). Si no abre, se sigue sin él.
    let engine = match crate::Index::open(&dir.join("index.db")).await {
        Ok(idx) => engine.with_index(Arc::new(idx)),
        Err(e) => {
            avisos.push(Aviso::SinIndice(e.to_string()));
            engine
        }
    };
    engine.set_spool(spool);
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::os_root()) as Arc<dyn Provider>
    );
    crate::archive_config::aplicar(&engine)
        .await
        .map_err(ErrorDeArranque::Archivo)?;
    engine.set_connector(Arc::new(crate::connect::ConnectionManager::new(
        dir.clone(),
    )));
    // IA (ADR 0031): opt-in, y jamás aborta el arranque.
    match crate::blocking::spawn_blocking(crate::ai::AiConfig::load).await {
        Ok(Ok(config)) => {
            if let Some(pcfg) = config.rename_provider_config().cloned() {
                match crate::ai::resolve_and_build(&pcfg, dir.clone()).await {
                    Ok(provider) => engine.set_ai_provider(provider),
                    Err(e) => avisos.push(Aviso::IaNoDisponible(e)),
                }
            }
            if let Some(w) = crate::ai::install_embed_provider(&engine, &config).await {
                avisos.push(Aviso::IaEmbeddings(w));
            }
            engine.set_ai_config(config);
        }
        Ok(Err(e)) => avisos.push(Aviso::IaInvalida(e.to_string())),
        Err(e) => avisos.push(Aviso::IaNoCargo(e.to_string())),
    }
    let daemon = Daemon::bind_with_policy(
        Arc::new(engine),
        scopes,
        approvals,
        DaemonConfig {
            socket_path: o.socket,
            idle_timeout: o.idle_timeout,
            // Se pasa EXPLÍCITO: el default no persiste nada, para que ningún
            // test ni embebedor escriba el estado real por descuido.
            state_dir: o.state_dir,
            ..DaemonConfig::default()
        },
    )
    .await
    .map_err(ErrorDeArranque::Bind)?;
    Ok((daemon, avisos))
}
