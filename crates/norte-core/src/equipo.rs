//! Equipar un engine: las piezas que lleva TODO engine de norte, sea el del
//! daemon o uno embebido en un frontend.
//!
//! Vivía copiado en cuatro sitios —el arranque del daemon, la TUI embebida, la
//! CLI embebida y `norte ai rename`—, y la IA en los cuatro (regla 7). Las
//! copias ya habían divergido: una se callaba lo que otra avisaba, y cada una
//! decidía por su cuenta qué pasaba si `[ai]` no cargaba. Aquí se decide una
//! vez.
//!
//! Como [`crate::daemon::componer()`], esto COMPONE y no habla: lo que hay que
//! saber vuelve como [`Aviso`], y lo pinta quien equipa, en su canal (stderr en
//! la CLI, el log en la TUI, que tiene la pantalla tomada).
//!
//! Lo que NO está aquí, y por qué: el journal y la policy (el daemon los
//! instala con el engine, embebido los decide `embedded::engine_in`), el índice
//! (consume el engine, `with_index`), el spool (solo quien sincroniza) y los
//! límites de `[archive]` (el daemon los exige, un frontend los degrada: ver
//! [`crate::archive_config::aplicar`]).

use std::path::Path;
use std::sync::Arc;

use norte_vfs::Provider;

use crate::Engine;

/// Algo que quien arranca debe saber y que NO impide arrancar.
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
        dir: std::path::PathBuf,
    },
    /// El barrido no pudo ni empezar.
    SpoolsSinBarrer {
        /// Dónde.
        dir: std::path::PathBuf,
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
    /// `[archive]` no se pudo leer: se sigue con los límites por defecto.
    ArchivoInvalido(String),
}

/// El texto para un LOG, sin traducir: quien lo pinta para una persona
/// (la CLI) usa su propio catálogo de Fluent.
impl std::fmt::Display for Aviso {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SpoolsBarridos(n) => write!(f, "barridos {n} planes de sync huérfanos"),
            Self::SpoolsAMedias {
                removed,
                failed,
                dir,
            } => write!(
                f,
                "barridos {removed} planes de sync huérfanos y {failed} no se dejaron borrar en {}",
                dir.display()
            ),
            Self::SpoolsSinBarrer { dir, error } => {
                write!(f, "no se pudo barrer {}: {error}", dir.display())
            }
            Self::SinIndice(e) => write!(f, "índice no disponible: {e}"),
            Self::IaNoDisponible(e) => write!(f, "proveedor de IA no disponible: {e}"),
            Self::IaEmbeddings(w) => f.write_str(w),
            Self::IaInvalida(e) => write!(f, "[ai] inválido: {e}"),
            Self::IaNoCargo(e) => write!(f, "la carga de [ai] falló: {e}"),
            Self::ArchivoInvalido(e) => write!(f, "[archive] no se pudo leer: {e}"),
        }
    }
}

/// Lo que quedó puesto, más lo que hay que decir.
#[derive(Debug, Default)]
pub struct Equipado {
    /// Lo que no impidió equipar pero hay que contar.
    pub avisos: Vec<Aviso>,
    /// Si quedó instalado un proveedor de IA de RENOMBRADO. Quien lo exige
    /// (`norte ai rename`) lo mira aquí; sin configurar no es un aviso,
    /// porque la IA es opt-in.
    pub ia_renombrado: bool,
}

/// Qué proveedores de IA instalar. Cada uno resuelve SU secreto (keyring,
/// quizá con un diálogo del sistema), así que se instala solo lo que se va a
/// usar: `norte ai rename` no tiene por qué desbloquear la clave de los
/// embeddings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ia {
    /// El de renombrado (`rename_provider`).
    pub renombrado: bool,
    /// El de embeddings (`embed_provider`).
    pub embeddings: bool,
}

impl Ia {
    /// Nada: ni se lee `[ai]`.
    pub const NADA: Self = Self {
        renombrado: false,
        embeddings: false,
    };
    /// Los dos: un engine de larga vida (el daemon, la TUI) sirve todo.
    pub const TODA: Self = Self {
        renombrado: true,
        embeddings: true,
    };
}

/// Equipa `engine` con el proveedor local, el conector de conexiones remotas
/// y los proveedores de IA de `[ai]` que pida `ia`.
///
/// `config_dir` es de donde salen las conexiones y los secretos de los
/// proveedores. `[ai]` en sí se lee de las capas estándar del usuario. La IA
/// es opt-in y JAMÁS falla aquí: lo que no se pudo instalar es un [`Aviso`].
#[tracing::instrument(skip_all, fields(renombrado = ia.renombrado, embeddings = ia.embeddings))]
pub async fn equipar(engine: &Engine, config_dir: &Path, ia: Ia) -> Equipado {
    let mut hecho = Equipado::default();
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::os_root()) as Arc<dyn Provider>
    );
    engine.set_connector(Arc::new(crate::connect::ConnectionManager::new(
        config_dir.to_path_buf(),
    )));
    if ia == Ia::NADA {
        return hecho;
    }
    match crate::blocking::spawn_blocking(crate::ai::AiConfig::load).await {
        Ok(Ok(config)) => {
            if ia.renombrado
                && let Some(pcfg) = config.rename_provider_config().cloned()
            {
                match crate::ai::resolve_and_build(&pcfg, config_dir.to_path_buf()).await {
                    Ok(provider) => {
                        engine.set_ai_provider(provider);
                        hecho.ia_renombrado = true;
                    }
                    Err(e) => hecho.avisos.push(Aviso::IaNoDisponible(e)),
                }
            }
            if ia.embeddings
                && let Some(w) =
                    crate::ai::install_embed_provider(engine, &config, config_dir.to_path_buf())
                        .await
            {
                hecho.avisos.push(Aviso::IaEmbeddings(w));
            }
            engine.set_ai_config(config);
        }
        Ok(Err(e)) => hecho.avisos.push(Aviso::IaInvalida(e.to_string())),
        Err(e) => hecho.avisos.push(Aviso::IaNoCargo(e.to_string())),
    }
    hecho
}

/// Abre el índice de búsqueda de `config_dir` y se lo pone a `engine`; si no
/// abre, devuelve el engine sin él y el [`Aviso`].
pub async fn con_indice(engine: Engine, config_dir: &Path, avisos: &mut Vec<Aviso>) -> Engine {
    match crate::Index::open(&config_dir.join("index.db")).await {
        Ok(idx) => engine.with_index(Arc::new(idx)),
        Err(e) => {
            avisos.push(Aviso::SinIndice(e.to_string()));
            engine
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Aviso, Ia, con_indice, equipar};

    /// Sin IA no se toca `[ai]`: ni aviso ni proveedor. Es lo que paga un
    /// `norte ls`, y tiene que ser nada.
    #[tokio::test]
    async fn sin_ia_no_hay_avisos_ni_proveedor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = crate::Engine::new();
        let hecho = equipar(&engine, dir.path(), Ia::NADA).await;
        assert!(hecho.avisos.is_empty());
        assert!(!hecho.ia_renombrado);
    }

    /// Un índice que no abre no impide seguir: vuelve el engine y el aviso.
    #[tokio::test]
    async fn un_indice_que_no_abre_es_un_aviso() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Un DIRECTORIO donde tendría que ir el fichero: no se puede abrir.
        std::fs::create_dir(dir.path().join("index.db")).expect("mkdir");
        let mut avisos = Vec::new();
        let _engine = con_indice(crate::Engine::new(), dir.path(), &mut avisos).await;
        assert!(
            matches!(avisos.as_slice(), [Aviso::SinIndice(_)]),
            "{avisos:?}"
        );
    }
}
