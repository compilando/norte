//! [`Backend`](super::Backend)'s host area: GC of orphaned staging, host
//! volumes, and the daemon's log (`log.tail`/`log.level`).

use norte_proto::{Error, VPath};

use super::{Backend, volume_to_proto};

impl Backend {
    /// GC of orphaned `.norte-partial` staging under `dir` (#11, ADR 0012):
    /// a ONE-OFF operation, not a Task nor a journal mutation. Returns how
    /// many it swept.
    ///
    /// # Errors
    /// On `Remote` it's [`Error::Unsupported`]: there is (yet) no wire
    /// method for the GC — exposing it needs a protocol change, deferred
    /// until there's demand. On `Embedded`, the provider's own.
    pub async fn gc_partials(
        &self,
        dir: &VPath,
        older_than: std::time::Duration,
    ) -> Result<usize, Error> {
        match self {
            Self::Embedded(engine) => engine.gc_partials(dir, older_than).await,
            Self::Remote(_) => Err(Error::Unsupported),
        }
    }

    /// Host volumes (`host.volumes`, 0.37.0, #131): mount point, filesystem
    /// type, kind and free/total space. `include_pseudo` is the picker's
    /// "show everything" toggle (design §E of
    /// `2026-08-10-volumes-design.md`).
    ///
    /// Embedded: calls [`crate::volumes::enumerate`] directly — a volume
    /// belongs to the HOST, not to a provider, so there's no engine to
    /// consult (design §A). NO actor gate: an embedded core has no
    /// connection or daemon, so whoever calls it is ALREADY the human
    /// sitting in front of it — there is no remote surface to sandbox.
    ///
    /// Remote: `host.volumes` against the daemon, which DOES gate by
    /// connection actor (design §C) — an agent connection sees
    /// [`Error::PolicyDenied`].
    ///
    /// # Errors
    /// Protocol taxonomy; with the daemon down,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn volumes(
        &self,
        include_pseudo: bool,
    ) -> Result<Vec<norte_proto::methods::Volume>, Error> {
        match self {
            Self::Embedded(_) => {
                let volumes = crate::volumes::enumerate(include_pseudo)
                    .await
                    .map_err(|_| Error::Io { retryable: false })?;
                Ok(volumes.into_iter().map(volume_to_proto).collect())
            }
            Self::Remote(r) => r.volumes(include_pseudo).await,
        }
    }

    /// The DAEMON's log from `cursor` (`log.tail`, 0.65.0, #328, ADR 0092).
    ///
    /// `cursor: None` means "give me whatever there is" and is NOT the same
    /// as zero: against a ring that has already wrapped, a zero would
    /// falsely report `lost` on the first pass. After that the returned
    /// `next` is chained.
    ///
    /// # Errors
    /// Protocol taxonomy. On `Embedded` it's always
    /// [`Error::Unsupported`], and that is NOT a shortcoming: the embedded
    /// core's ring is in THIS process, so it's already what the frontend
    /// reads — there's no second source to offer. On `Remote`, a
    /// same-version daemon compiled without the `logging` feature answers
    /// the same thing, and that answer cannot change while that daemon
    /// lives.
    ///
    /// The two answers are written the same and **do not mean the same
    /// thing**, so whoever paints a panel decides with [`Self::is_remote`]
    /// before asking: with no daemon there's nothing to report on, and a
    /// sentence about "this daemon" where there is none is worse than
    /// silence.
    ///
    /// ```
    /// use norte_core::{Engine, backend::Backend};
    /// use norte_proto::Error;
    /// use std::sync::Arc;
    /// let rt = tokio::runtime::Runtime::new().expect("runtime");
    /// let backend = Backend::Embedded(Arc::new(Engine::new()));
    /// assert!(matches!(
    ///     rt.block_on(backend.log_tail(None, 10)),
    ///     Err(Error::Unsupported)
    /// ));
    /// ```
    pub async fn log_tail(
        &self,
        cursor: Option<u64>,
        max: u32,
    ) -> Result<norte_proto::methods::LogTailResult, Error> {
        match self {
            Self::Embedded(_) => Err(Error::Unsupported),
            Self::Remote(r) => r.log_tail(cursor, max).await,
        }
    }

    /// Raises the level the daemon's ring captures (`log.level`, 0.65.0,
    /// #328) and returns what actually ended up set.
    ///
    /// Its ring is ITS OWN: it's global to all its clients and never goes
    /// down, so what was requested and what got set need not match — hence
    /// this returns a level instead of a `()`.
    ///
    /// # Errors
    /// Protocol taxonomy; [`Error::Unsupported`] on `Embedded` and against a
    /// daemon with no log to serve (see [`Self::log_tail`]).
    ///
    /// ```
    /// use norte_core::{Engine, backend::Backend};
    /// use norte_proto::Error;
    /// use std::sync::Arc;
    /// let rt = tokio::runtime::Runtime::new().expect("runtime");
    /// let backend = Backend::Embedded(Arc::new(Engine::new()));
    /// assert!(matches!(
    ///     rt.block_on(backend.log_level("debug")),
    ///     Err(Error::Unsupported)
    /// ));
    /// ```
    pub async fn log_level(&self, level: &str) -> Result<String, Error> {
        match self {
            Self::Embedded(_) => Err(Error::Unsupported),
            Self::Remote(r) => r.log_level(level).await,
        }
    }
}
