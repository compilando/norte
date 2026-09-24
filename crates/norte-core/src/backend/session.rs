//! [`Backend`](super::Backend)'s UI session area (L2): reading, replacing
//! and releasing the stored session.

use norte_proto::Error;

use super::Backend;

impl Backend {
    /// The stored UI session and whether THIS surface can write it (L2).
    ///
    /// Against the daemon this is `session.get`. EMBEDDED has no socket, so
    /// the process is its own store: the file under `<state_dir>` and the
    /// same lock the daemon uses, taken once per process. With no
    /// `state_dir` —an environment with no `HOME`— an empty session that
    /// nobody writes is served, which is exactly what a startup with no
    /// stored session does today.
    ///
    /// # Errors
    ///
    /// Whatever the transport returns. A failure is NOT a reason not to
    /// start: the caller continues to the settings screen.
    pub async fn session_get(&self) -> Result<(norte_proto::methods::Session, bool), Error> {
        match self {
            Self::Embedded(_) => Ok(crate::embedded::session_get().await),
            #[cfg(unix)]
            Self::Remote(r) => r.session_get().await,
        }
    }

    /// Replaces the UI session and returns the NEW revision (L2).
    ///
    /// # Errors
    ///
    /// [`Error::Conflict`] if the revision came in stale (re-read and
    /// retry), [`Error::LimitExceeded`] if the body exceeds the cap, and
    /// whatever the transport gives otherwise.
    pub async fn session_put(
        &self,
        version: u32,
        revision: u64,
        body: serde_json::Value,
    ) -> Result<u64, Error> {
        match self {
            Self::Embedded(_) => crate::embedded::session_put(version, revision, body).await,
            #[cfg(unix)]
            Self::Remote(r) => r.session_put(version, revision, body).await,
        }
    }

    /// Releases ownership of the UI session (0.78.0, phase 9). Returns
    /// whether this connection WAS the owner.
    ///
    /// **In EMBEDDED there's nobody to release it to**: the process is the
    /// only one touching that session, so it answers `false` without
    /// touching anything. This isn't a silent degradation — it's the reason
    /// `app.handoff` is declared unavailable outside daemon mode, with its
    /// reason.
    ///
    /// # Errors
    /// Whatever the transport gives. A 0.77 daemon answers `Unsupported`.
    pub async fn session_release(&self) -> Result<bool, Error> {
        match self {
            Self::Embedded(_) => Ok(false),
            #[cfg(unix)]
            Self::Remote(r) => r.session_release().await,
        }
    }
}
