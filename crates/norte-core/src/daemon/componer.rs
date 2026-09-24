//! Compose the daemon for `norte daemon run`: which pieces it carries and in
//! what order.
//!
//! It used to live in the CLI (rule 7): the journal, the spool sweep, the
//! policy, the index, the AI, the archive limits and the `bind` are core
//! decisions, and the binary that made them was the only place where a
//! second startup — a test, another frontend, an embedder — could not repeat
//! them without copying them.
//!
//! Here it COMPOSES and does not speak: what the operator needs to know
//! comes back as a typed [`Aviso`], and whoever launches it prints it, in
//! their own language. What prevents startup comes back as
//! [`ErrorDeArranque`].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::{Daemon, DaemonApprovalResolver, DaemonConfig, DaemonError};
use crate::Engine;

/// What whoever launches the daemon decides.
#[derive(Debug, Clone, Default)]
pub struct Opciones {
    /// The socket; `None` = the system's.
    pub socket: Option<PathBuf>,
    /// How long without clients before exiting; `None` = never.
    pub idle_timeout: Option<Duration>,
    /// Where the UI session persists; `None` = it is not persisted.
    pub state_dir: Option<PathBuf>,
    /// Where the journal, the index, the spools, the connections and the
    /// AI's secrets come from; `None` = the user's (`connect::config_dir`).
    ///
    /// It does NOT cover configuration: `policy.toml`, `[ai]` and `[archive]`
    /// are read from the user's standard layers. A test that passed this for
    /// the journal would read the REAL config, so this module's tests stop
    /// short of that.
    pub config_dir: Option<PathBuf>,
}

/// Something the operator should know that does NOT prevent startup. The
/// same type returned by [`crate::equipo::equipar`], which is where the AI
/// ones come from.
pub use crate::equipo::Aviso;

/// What prevents startup.
///
/// The message does NOT repeat the cause: that goes in `source()`, and
/// whoever prints the chain (anyhow's `{e:#}`) already adds it. With `{0}` in
/// the message it came out twice.
#[derive(Debug, thiserror::Error)]
pub enum ErrorDeArranque {
    /// The journal does not open; with "database is locked", another process
    /// holds it.
    #[error("could not open the journal")]
    Journal(#[source] crate::journal::JournalError),
    /// `policy.toml` could not be read, or is not valid.
    #[error("policy.toml")]
    Policy(#[source] std::io::Error),
    /// `[archive]` in `norte.toml` could not be read, or is not valid.
    #[error("norte.toml ([archive])")]
    Archivo(#[source] std::io::Error),
    /// The socket could not be bound.
    #[error("could not bind the daemon")]
    Bind(#[source] DaemonError),
}

/// Composes the daemon with everything it needs, ready for `run`.
///
/// The order matters and is spelled out at each step: the journal FIRST,
/// because its exclusive lock is what guarantees there is no other daemon
/// over this state directory; the spool sweep AFTER, for the same reason.
///
/// Notices go to `avisos` AS THEY HAPPEN, not in the final `Ok`: if startup
/// fails later — a broken `[archive]`, the socket already in use — whatever
/// was already found out along the way (a spool that could not be swept, an
/// index that did not open) is still true, and whoever launches it needs to
/// be able to say so.
///
/// # Errors
/// [`ErrorDeArranque`]: the journal, the policy, the archive limits or the
/// `bind`. Everything else degrades with its [`Aviso`].
pub async fn componer(o: Opciones, avisos: &mut Vec<Aviso>) -> Result<Daemon, ErrorDeArranque> {
    let dir = o.config_dir.unwrap_or_else(crate::connect::config_dir);
    // The daemon is the SOLE owner of the journal (spec §4, ADR 0024) and
    // installs policy + approvals: MCP agents are governed here, never at
    // the bridge.
    let journal = crate::SqliteJournal::open(&dir.join("journal.db"))
        .await
        .map_err(ErrorDeArranque::Journal)?;
    // Sweep of sync spools (ADR 0049), right next to the journal and for the
    // same reason: a violent shutdown leaves files that AUTHORIZE writes,
    // and nobody else is going to pick them up. All of them are swept: there
    // is no live connection yet at this point. A sweep that fails does NOT
    // prevent startup — what prevents applying an old plan is that the plan
    // registry is born empty.
    //
    // The `Spool` is built ONCE: two `Spool::new` calls are two emission
    // registries that cannot see each other, and this is the one installed
    // in the engine.
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
    // policy.toml: absent = no rules = an agent INSIDE scope still gets
    // denied (fail-closed, `no-rule`).
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
    // Search index (M4, ADR 0034). If it does not open, we proceed without it.
    let engine = crate::equipo::con_indice(engine, &dir, avisos).await;
    engine.set_spool(spool);
    // A broken `[archive]` ABORTS the daemon (fail-loud, like `policy.toml`);
    // an embedded frontend, by contrast, degrades it to a notice.
    crate::archive_config::aplicar(&engine)
        .await
        .map_err(ErrorDeArranque::Archivo)?;
    // Local provider, connector and AI (ADR 0031: opt-in, never aborts): what
    // equips the whole engine, same as the embedded ones.
    avisos.extend(
        crate::equipo::equipar(&engine, &dir, crate::equipo::Ia::TODA)
            .await
            .avisos,
    );
    let daemon = Daemon::bind_with_policy(
        Arc::new(engine),
        scopes,
        approvals,
        DaemonConfig {
            socket_path: o.socket,
            idle_timeout: o.idle_timeout,
            // Passed EXPLICITLY: the default persists nothing, so that no
            // test or embedder writes the real state by accident.
            state_dir: o.state_dir,
            ..DaemonConfig::default()
        },
    )
    .await
    .map_err(ErrorDeArranque::Bind)?;
    Ok(daemon)
}

#[cfg(test)]
mod tests {
    use super::{ErrorDeArranque, Opciones, componer};

    /// The journal goes FIRST and its lock is exclusive: with another owner
    /// alive, the daemon does not start, and says so with its own error
    /// class. It is the only thing that prevents two daemons over the same
    /// directory.
    ///
    /// It is also the only path through `componer` that can be tested
    /// without reading the user's REAL config: the following steps load
    /// `policy.toml` and `[ai]` through the standard layers.
    ///
    /// Takes as long as the journal's `busy_timeout` (5s): `SQLite` retries
    /// the lock before giving up. Accepted instead of adding an option that
    /// only this test would use — it is the same wait the operator sees.
    #[tokio::test]
    async fn does_not_start_with_the_journal_already_held() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _owner = crate::SqliteJournal::open(&dir.path().join("journal.db"))
            .await
            .expect("first owner");
        let mut avisos = Vec::new();
        let r = componer(
            Opciones {
                config_dir: Some(dir.path().to_path_buf()),
                socket: Some(dir.path().join("d.sock")),
                ..Opciones::default()
            },
            &mut avisos,
        )
        .await;
        assert!(
            matches!(r, Err(ErrorDeArranque::Journal(_))),
            "expected Journal: {:?}",
            r.err()
        );
        assert!(avisos.is_empty(), "nothing happened before the journal");
    }
}
