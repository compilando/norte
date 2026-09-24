//! Equipping an engine: the pieces that EVERY norte engine carries, whether
//! it's the daemon's or one embedded in a frontend.
//!
//! It used to live copied in four places —the daemon's startup, the embedded
//! TUI, the embedded CLI and `norte ai rename`—, and the AI part in all four
//! (rule 7). The copies had already diverged: one stayed quiet about what
//! another warned about, and each decided on its own what happened if
//! `[ai]` failed to load. It's decided once, here.
//!
//! Like [`crate::daemon::compose()`], this COMPOSES and does not speak:
//! whatever needs to be known comes back as [`Notice`], and whoever equips
//! paints it, on their own channel (stderr in the CLI, the log in the TUI,
//! which has the screen taken).
//!
//! What is NOT here, and why: the journal and the policy (the daemon
//! installs them with the engine, embedded decides them via
//! `embedded::engine_in`), the index (consumes the engine, `with_index`),
//! the spool (only whoever syncs), and the `[archive]` limits (the daemon
//! requires them, a frontend degrades them: see
//! [`crate::archive_config::apply`]).

use std::path::Path;
use std::sync::Arc;

use norte_vfs::Provider;

use crate::Engine;

/// Something whoever starts up should know, and that does NOT prevent
/// starting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// `n` orphaned sync plans were swept.
    SpoolsSweeps(usize),
    /// The sweep took some and others refused to be deleted.
    SpoolsAMedias {
        /// Swept.
        removed: usize,
        /// The ones that resisted.
        failed: usize,
        /// Where.
        dir: std::path::PathBuf,
    },
    /// The sweep could not even start.
    UnsweptSpools {
        /// Where.
        dir: std::path::PathBuf,
        /// Why.
        error: String,
    },
    /// No search index: `index.*` will answer `Unsupported`.
    NoIndex(String),
    /// The rename AI provider is not available.
    IaNoAvailable(String),
    /// What the embeddings provider's installation said.
    IaEmbeddings(String),
    /// `[ai]` is not valid.
    IaInvalid(String),
    /// `[ai]` could not be read.
    IaNoCargo(String),
    /// `[archive]` could not be read: continuing with the default limits.
    ArchiveInvalid(String),
}

/// The text for a LOG, untranslated: whoever paints it for a person (the
/// CLI) uses their own Fluent catalog.
impl std::fmt::Display for Notice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SpoolsSweeps(n) => write!(f, "swept {n} orphaned sync plans"),
            Self::SpoolsAMedias {
                removed,
                failed,
                dir,
            } => write!(
                f,
                "swept {removed} orphaned sync plans and {failed} refused to be deleted in {}",
                dir.display()
            ),
            Self::UnsweptSpools { dir, error } => {
                write!(f, "could not sweep {}: {error}", dir.display())
            }
            Self::NoIndex(e) => write!(f, "index not available: {e}"),
            Self::IaNoAvailable(e) => write!(f, "AI provider not available: {e}"),
            Self::IaEmbeddings(w) => f.write_str(w),
            Self::IaInvalid(e) => write!(f, "[ai] invalid: {e}"),
            Self::IaNoCargo(e) => write!(f, "loading [ai] failed: {e}"),
            Self::ArchiveInvalid(e) => write!(f, "[archive] could not be read: {e}"),
        }
    }
}

/// What ended up set, plus what has to be said.
#[derive(Debug, Default)]
pub struct Equipped {
    /// What did not prevent equipping but has to be reported.
    pub notices: Vec<Notice>,
    /// Whether a RENAME AI provider ended up installed. Whoever requires it
    /// (`norte ai rename`) checks this; unconfigured is not a warning,
    /// because AI is opt-in.
    pub ia_renamed: bool,
}

/// Which AI providers to install. Each one resolves ITS OWN secret (keyring,
/// maybe with a system dialog), so only what will actually be used gets
/// installed: `norte ai rename` has no reason to unlock the embeddings key.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ia {
    /// The rename one (`rename_provider`).
    pub renamed: bool,
    /// The embeddings one (`embed_provider`).
    pub embeddings: bool,
}

impl Ia {
    /// Nothing: `[ai]` isn't even read.
    pub const NOTHING: Self = Self {
        renamed: false,
        embeddings: false,
    };
    /// Both: a long-lived engine (the daemon, the TUI) serves everything.
    pub const ALL: Self = Self {
        renamed: true,
        embeddings: true,
    };
}

/// Equips `engine` with the local provider, the remote connection connector,
/// and the `[ai]` providers `ia` asks for.
///
/// `config_dir` is where the connections and the providers' secrets come
/// from. `[ai]` itself is read from the user's standard layers. AI is
/// opt-in and NEVER fails here: whatever could not be installed is an
/// [`Notice`].
#[tracing::instrument(skip_all, fields(renamed = ia.renamed, embeddings = ia.embeddings))]
pub async fn equipar(engine: &Engine, config_dir: &Path, ia: Ia) -> Equipped {
    let mut done = Equipped::default();
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::os_root()) as Arc<dyn Provider>
    );
    engine.set_connector(Arc::new(crate::connect::ConnectionManager::new(
        config_dir.to_path_buf(),
    )));
    if ia == Ia::NOTHING {
        return done;
    }
    match crate::blocking::spawn_blocking(crate::ai::AiConfig::load).await {
        Ok(Ok(config)) => {
            if ia.renamed
                && let Some(pcfg) = config.rename_provider_config().cloned()
            {
                match crate::ai::resolve_and_build(&pcfg, config_dir.to_path_buf()).await {
                    Ok(provider) => {
                        engine.set_ai_provider(provider);
                        done.ia_renamed = true;
                    }
                    Err(e) => done.notices.push(Notice::IaNoAvailable(e)),
                }
            }
            if ia.embeddings
                && let Some(w) =
                    crate::ai::install_embed_provider(engine, &config, config_dir.to_path_buf())
                        .await
            {
                done.notices.push(Notice::IaEmbeddings(w));
            }
            engine.set_ai_config(config);
        }
        Ok(Err(e)) => done.notices.push(Notice::IaInvalid(e.to_string())),
        Err(e) => done.notices.push(Notice::IaNoCargo(e.to_string())),
    }
    done
}

/// Opens `config_dir`'s search index and sets it on `engine`; if it doesn't
/// open, returns the engine without it plus the [`Notice`].
pub async fn with_index(engine: Engine, config_dir: &Path, warnings: &mut Vec<Notice>) -> Engine {
    match crate::Index::open(&config_dir.join("index.db")).await {
        Ok(idx) => engine.with_index(Arc::new(idx)),
        Err(e) => {
            warnings.push(Notice::NoIndex(e.to_string()));
            engine
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Ia, Notice, equipar, with_index};

    /// Without AI, `[ai]` isn't touched: no warning, no provider. That's
    /// what a `norte ls` costs, and it has to be nothing.
    #[tokio::test]
    async fn no_ai_means_no_warnings_and_no_provider() {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = crate::Engine::new();
        let done = equipar(&engine, dir.path(), Ia::NOTHING).await;
        assert!(done.notices.is_empty());
        assert!(!done.ia_renamed);
    }

    /// An index that fails to open does not prevent continuing: the engine
    /// and the warning come back.
    #[tokio::test]
    async fn an_index_that_fails_to_open_is_a_warning() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A DIRECTORY where the file should go: it cannot be opened.
        std::fs::create_dir(dir.path().join("index.db")).expect("mkdir");
        let mut warnings = Vec::new();
        let _engine = with_index(crate::Engine::new(), dir.path(), &mut warnings).await;
        assert!(
            matches!(warnings.as_slice(), [Notice::NoIndex(_)]),
            "{warnings:?}"
        );
    }
}
