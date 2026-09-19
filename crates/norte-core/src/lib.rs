//! Núcleo de norte: scheduler de tasks con cancelación y progreso, copy engine,
//! y (en M0) modo embebido como biblioteca — el daemon llega en hitos posteriores.
#![forbid(unsafe_code)]

pub mod ai;
pub mod anchor;
pub mod approval;
pub mod archive_config;
pub mod audit;
pub mod backend;
/// `spawn_blocking` que conserva el span (ADR 0127).
mod blocking;
/// La Task de `fs.compare`: lotes coalescidos sobre el motor `norte-compare`.
mod compare;
pub mod connect;
#[cfg(unix)]
pub mod daemon;
pub mod embedded;
mod engine;
pub mod ftp_plugin;
mod hashing;
pub mod hooks;
mod index_build;
mod index_embed;
pub mod journal;
/// El montaje de `tracing` vive en `norte-config` desde #255: la ventana
/// gráfica lo necesita igual y no puede depender del motor (ADR 0066). Se
/// re-exporta aquí porque éste era su sitio y la CLI lo llama así.
pub use norte_config::logging;
mod observer;
mod ops;
pub mod organize;
mod pack;
pub mod plugin_provider;
pub mod plugins;
pub mod policy;
mod progress;
pub mod rename;
mod scheduler;
pub mod search;
mod sessions;
pub mod sync;
pub mod ui_session;
mod undo;
pub mod volumes;

pub use engine::{
    BATCH_REPORTS_MAX, Engine, RENAME_BATCH_MAX_LISTING, SYNC_REPORTS_MAX, TransferOptions,
    UNDO_REPORTS_MAX,
};
pub use journal::{
    Actor, ChainStatus, JOURNAL_FORMAT, Journal, JournalEntry, JournalFormat, NewEntry, Reversal,
    SqliteJournal,
};
pub use norte_index::Index;
/// ¿Es esto un id de plugin válido? Re-export de `norte-plugin-host` para que
/// un frontend pueda DESCARTAR en su punto de entrada un id que llegó por el
/// wire, sin depender del runtime de plugins (regla 7: los frontends hablan con
/// el core, y este crate ya les da [`PluginRegistry`] por el mismo camino).
pub use norte_plugin_host::is_valid_plugin_id;
/// Límites anti-bomba de los providers archive (#95.2): re-export para que
/// los frontends configuren [`Engine::set_archive_limits`] sin depender de
/// `norte-vfs-archive` (regla 7: hablan con el core).
pub use norte_vfs_archive::Limits as ArchiveLimits;
pub use observer::{Mutation, MutationObserver};
pub use ops::OnExists;
pub use plugins::{PluginRegistry, PluginRunError};
pub use policy::{
    AllowAll, Decision, DenyReason, OpSet, PolicyConfig, PolicyGate, PolicyOp, Scope,
    ScopeRegistry, ScopedPolicy, scope_key,
};
pub use progress::ProgressReporter;
pub use scheduler::{Priority, Scheduler, TaskBody, TaskCtx, TaskHandle};
pub use undo::{OP_ORGANIZED, UNDO_MAX_UNREVERTED_PATHS, UndoReport};
