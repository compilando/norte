//! [`Backend`](super::Backend)'s file area: listing, reading,
//! capabilities/attributes, stat, and `fs.*`'s mutations (copy, move,
//! delete, create, permissions), plus `fs.search`, `fs.dir_size`,
//! `fs.checksum` and `fs.dir_usage` with their reports.

use futures::StreamExt;
use norte_proto::{ByteRange, Capabilities, DeleteMode, Entry, Error, TaskId, VPath};
use tokio::sync::mpsc;

use crate::Engine;
use crate::engine::TransferOptions;

use super::{Backend, EntryStream, TaskRef};

impl Backend {
    /// Directory listing as a LAZY STREAM (ADR 0017). Embedded = the
    /// engine's stream as-is; remote = an EAGER first page (error parity:
    /// `NotFound`/`TypeMismatch` in the `Result`, not as the first item) +
    /// subsequent pages by cursor. Dropping the stream cancels it.
    ///
    /// Also returns the CONTAINER's skipped entries (#93): entries its index
    /// discarded for hostile names/limits (archive providers) and that will
    /// therefore NEVER come out of the stream. `None` = not applicable (the
    /// backend lists everything that exists). Available on open in both
    /// modes: embedded queries the already-warm index; remote gets it from
    /// the first page (every page repeats it).
    ///
    /// # Errors
    /// Protocol taxonomy; with the daemon down,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn list_stream(&self, dir: &VPath) -> Result<(EntryStream, Option<u64>), Error> {
        self.list_stream_with(dir, &[]).await
    }

    /// [`Backend::list_stream`] requesting per-entry attributes (#108 block
    /// 2). `attrs` are catalog ids (`Backend::attr_catalog`); an
    /// unadvertised id comes back absent, never an error.
    ///
    /// # Errors
    /// Protocol taxonomy; with the daemon down,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn list_stream_with(
        &self,
        dir: &VPath,
        attrs: &[String],
    ) -> Result<(EntryStream, Option<u64>), Error> {
        match self {
            Self::Embedded(engine) => {
                let opt = norte_vfs::ListOptions {
                    attrs: norte_vfs::AttrRequest::sanitized(attrs.to_vec()),
                };
                let stream = engine.list_with(dir, &opt).await?;
                // The same emission belt as the daemon (ADR 0039 §5): a
                // buggy provider cannot sneak unrequested ids or
                // over-the-cap values through the in-process path.
                let belt = opt.attrs.clone();
                let stream = stream
                    .map(move |item| {
                        item.map(|mut e| {
                            belt.retain_conforming(&mut e);
                            e
                        })
                    })
                    .boxed();
                // Best-effort: a failure here does not bring down a listing
                // that already opened (same contract as the daemon) —
                // degrades to "unknown", but NEVER silently (the whole
                // point of #93 is the signal).
                let skipped = engine.list_skipped(dir).await.unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "list_skipped failed; skipped = unknown");
                    None
                });
                Ok((stream, skipped))
            }
            Self::Remote(r) => r.list_stream(dir, attrs.to_vec()).await,
        }
    }

    /// FULL listing (drains [`Backend::list_stream`]). A remote `ls` on a
    /// giant dir no longer risks `CALL_TIMEOUT` nor a monster frame: it's N
    /// pages capped underneath.
    ///
    /// # Errors
    /// Protocol taxonomy; with the daemon down,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn list(&self, dir: &VPath) -> Result<Vec<Entry>, Error> {
        Ok(self.list_with_skipped(dir).await?.0)
    }

    /// [`Backend::list`] + the container's skipped entries (#93) — for
    /// frontends that want to flag them (the CLI's `ls`).
    ///
    /// # Errors
    /// Protocol taxonomy; with the daemon down,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn list_with_skipped(&self, dir: &VPath) -> Result<(Vec<Entry>, Option<u64>), Error> {
        self.list_with_skipped_attrs(dir, &[]).await
    }

    /// [`Backend::list_with_skipped`] requesting per-entry attributes (#108
    /// block 2) — the CLI's `ls --attrs`.
    ///
    /// # Errors
    /// Protocol taxonomy; with the daemon down,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn list_with_skipped_attrs(
        &self,
        dir: &VPath,
        attrs: &[String],
    ) -> Result<(Vec<Entry>, Option<u64>), Error> {
        let (mut stream, skipped) = self.list_stream_with(dir, attrs).await?;
        let mut entries = Vec::new();
        while let Some(item) = stream.next().await {
            entries.push(item?);
        }
        Ok((entries, skipped))
    }

    /// PRESENTATION read (a viewer): gathers the requested range in memory.
    /// The caller caps it (`len`) — this is not the copy path.
    ///
    /// # Errors
    /// Protocol taxonomy.
    pub async fn read(&self, path: &VPath, range: Option<ByteRange>) -> Result<Vec<u8>, Error> {
        match self {
            Self::Embedded(engine) => {
                let mut stream = engine.read(path, range).await?;
                let mut out = Vec::new();
                while let Some(chunk) = stream.next().await {
                    out.extend_from_slice(&chunk?);
                }
                Ok(out)
            }
            Self::Remote(r) => r.read(path, range).await,
        }
    }

    /// Capabilities of the provider serving `path` (F8/trash, ADR 0009).
    ///
    /// # Errors
    /// Protocol taxonomy.
    pub async fn capabilities(&self, path: &VPath) -> Result<Capabilities, Error> {
        match self {
            Self::Embedded(engine) => engine.capabilities(path).await,
            Self::Remote(r) => r.capabilities(path).await,
        }
    }

    /// The attr catalog of the provider serving `path` (#108 block 2),
    /// ALWAYS sanitized: embedded goes through `Engine::attr_catalog`
    /// (`AttrCatalog::new`, ADR 0039 §4) and remote through the wire's
    /// deserializer (same sanitizing via the type).
    ///
    /// # Errors
    /// Protocol taxonomy.
    pub async fn attr_catalog(&self, path: &VPath) -> Result<norte_proto::AttrCatalog, Error> {
        match self {
            Self::Embedded(engine) => engine.attr_catalog(path).await,
            Self::Remote(r) => r.attr_catalog(path).await,
        }
    }

    /// Both halves of `fs.capabilities` for `path`: the capability flags AND
    /// the attribute catalogue, in ONE round trip.
    ///
    /// [`Self::capabilities`] and [`Self::attr_catalog`] each throw the other
    /// half of that response away, so a frontend that wants both — the TUI
    /// caches the catalogue for its columns and the flags to answer
    /// "read-only?" without asking again — paid two round trips for one
    /// message. Remote mode makes a single `fs.capabilities` call here;
    /// embedded mode asks the engine twice, which is two provider lookups
    /// instead of one and no extra round trip on the wire. Not "no I/O at
    /// all": both halves go through `Engine::provider_for`, which for a remote
    /// scheme can resolve or establish the connection first — true of
    /// `file://`, false of an embedded `sftp` pane.
    ///
    /// # Errors
    /// Protocol taxonomy.
    pub async fn capabilities_and_attrs(
        &self,
        path: &VPath,
    ) -> Result<(Capabilities, norte_proto::AttrCatalog), Error> {
        match self {
            Self::Embedded(engine) => Ok((
                engine.capabilities(path).await?,
                engine.attr_catalog(path).await?,
            )),
            Self::Remote(r) => {
                let full = r.capabilities_full(path).await?;
                Ok((full.capabilities, full.attrs))
            }
        }
    }

    /// Metadata for a node (`fs.stat`).
    ///
    /// # Errors
    /// Protocol taxonomy; with the daemon down,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn stat(&self, path: &VPath) -> Result<Entry, Error> {
        self.stat_attrs(path, &[]).await
    }

    /// [`Backend::stat`] requesting per-entry attributes (#108 block 2).
    ///
    /// # Errors
    /// Protocol taxonomy; with the daemon down,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn stat_attrs(&self, path: &VPath, attrs: &[String]) -> Result<Entry, Error> {
        match self {
            Self::Embedded(engine) => {
                let opt = norte_vfs::ListOptions {
                    attrs: norte_vfs::AttrRequest::sanitized(attrs.to_vec()),
                };
                let mut entry = engine.stat_with(path, &opt).await?;
                // Same emission belt as the daemon (ADR 0039 §5).
                opt.attrs.retain_conforming(&mut entry);
                Ok(entry)
            }
            Self::Remote(r) => r.stat(path, attrs.to_vec()).await,
        }
    }

    /// Retains the anchor of a directory a PANEL just listed (#301, ADR
    /// 0073), so a later write can say "the destination was THAT ONE".
    ///
    /// This is what the daemon puts in `fs.list`'s response and the SDK
    /// stores on its own. There's no wire here, so the engine stores it —
    /// and without this, `ntc`, which runs embedded by DEFAULT, did every
    /// anchored operation WITHOUT an anchor: the check ADR 0076 asked for
    /// specifically for `fs.create` was missing from the one frontend that
    /// launches an `$EDITOR` over what it just created.
    ///
    /// # Called by hand, and that's the point
    ///
    /// `list_stream_with` doesn't do it, and it's the funnel for ALL
    /// listings: the side tree (one branch per loop iteration) and a Lua
    /// script's `fs.list` both go through it, and since remembering
    /// OVERWRITES, either of them would re-bless the panel's anchor with
    /// whatever node it happened to see at that moment. The anchor says
    /// **who looked**; a listing that isn't a screen has nobody looking.
    ///
    /// Does nothing against the daemon: there the listing's response sends
    /// the anchor and the SDK stores it, which is who really listed.
    ///
    /// Best-effort: a provider that can't give node identity (a bucket, an
    /// SFTP with no extensions) cannot block a listing, and a failure
    /// DELETES whatever was there —sending a stale one would make the write
    /// reject itself—, so the next write behaves as it did in 0.53.
    pub async fn remember_listing_anchor(&self, dir: &VPath) {
        match self {
            Self::Embedded(engine) => {
                let anchor = engine.dir_anchor(dir).await.unwrap_or_else(|e| {
                    tracing::debug!(error = %e, "dir_anchor failed; no anchor for this listing");
                    None
                });
                engine.remember_dir_anchor(dir, anchor);
            }
            Self::Remote(_) => {}
        }
    }

    /// The retained anchor of the directory `dest` is about to be
    /// written into (#301).
    ///
    /// `dest` is the EXACT path of what's being written, so what's
    /// looked up is its PARENT: that's the directory the human listed and
    /// approved. The same computation the SDK does on the remote path.
    ///
    /// `None` —nobody listed that directory this session, or its provider
    /// can't give node identity— behaves exactly like 0.53: confinement
    /// still applies and that check just doesn't happen.
    fn destination_anchor(engine: &Engine, dest: &VPath) -> Option<norte_proto::DirAnchor> {
        engine.remembered_dir_anchor(&dest.parent()?)
    }

    /// Copy as a task.
    ///
    /// The DESTINATION directory's anchor travels with the operation when
    /// this backend listed it (#301, ADR 0073) — same as the SDK sets it on
    /// the remote path, and for the same reason: between listing and
    /// writing, that directory may have stopped being the node the human
    /// was looking at.
    ///
    /// # Errors
    /// Protocol taxonomy.
    pub async fn copy(
        &self,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => Ok(TaskRef::from_handle(
                &engine
                    .copy_anchored(
                        from,
                        to,
                        opts,
                        crate::journal::Actor::User,
                        Self::destination_anchor(engine, to),
                    )
                    .await?,
            )),
            Self::Remote(r) => r
                .transfer(norte_client::Transfer::Copy, from, to, opts.into())
                .await
                .map(TaskRef::from),
        }
    }

    /// Move as a task. With the destination's anchor, like [`Self::copy`].
    ///
    /// # Errors
    /// Protocol taxonomy.
    pub async fn move_(
        &self,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => Ok(TaskRef::from_handle(
                &engine
                    .move_anchored(
                        from,
                        to,
                        opts,
                        crate::journal::Actor::User,
                        Self::destination_anchor(engine, to),
                    )
                    .await?,
            )),
            Self::Remote(r) => r
                .transfer(norte_client::Transfer::Move, from, to, opts.into())
                .await
                .map(TaskRef::from),
        }
    }

    /// Delete as a task (trash or permanent, ADR 0009).
    ///
    /// # Errors
    /// Protocol taxonomy.
    pub async fn delete(&self, path: &VPath, mode: DeleteMode) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                Ok(TaskRef::from_handle(&engine.delete_with(path, mode).await?))
            }
            Self::Remote(r) => r.delete(path, mode).await.map(TaskRef::from),
        }
    }

    /// Creation of ONE directory as a Task (#104, F7). No `-p`; an occupied
    /// destination = `Conflict{Exists}`.
    ///
    /// # Errors
    /// Protocol taxonomy.
    pub async fn mkdir(&self, path: &VPath) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => Ok(TaskRef::from_handle(&engine.mkdir(path).await?)),
            Self::Remote(r) => r.mkdir(path).await.map(TaskRef::from),
        }
    }

    /// Creation of ONE EMPTY file as a Task (#290). An occupied destination
    /// = `Conflict{Exists}`; exclusivity is the provider's to give (atomic
    /// on local and on objects, with a window on SFTP v3).
    ///
    /// With the directory's anchor, like [`Self::copy`] — and this is where
    /// it matters most (#301): `fs.create` is the only method whose success
    /// hands a path to a program OUTSIDE norte (`$EDITOR`), which is the
    /// reason ADR 0076 gave for anchoring it.
    ///
    /// # Errors
    /// Protocol taxonomy.
    pub async fn create_file(&self, path: &VPath) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => Ok(TaskRef::from_handle(
                &engine
                    .create_file_as(
                        path,
                        Self::destination_anchor(engine, path),
                        crate::journal::Actor::User,
                    )
                    .await?,
            )),
            Self::Remote(r) => r.create_file(path).await.map(TaskRef::from),
        }
    }

    /// Changes POSIX permissions on a batch of paths as a Task (#314).
    ///
    /// Mutates: journal with a reversal —the previous mode— and the policy
    /// gate. A location with no POSIX permissions answers `Unsupported` and
    /// changes nothing.
    ///
    /// # Errors
    /// Protocol taxonomy: [`Error::InvalidPath`] with no paths, over the
    /// cap, or with bits that aren't permission bits; [`Error::PolicyDenied`];
    /// [`Error::Unsupported`].
    pub async fn set_mode(
        &self,
        params: norte_proto::methods::FsSetModeParams,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => Ok(TaskRef::from_handle(&engine.set_mode(params).await?)),
            Self::Remote(r) => r.set_mode(params).await.map(TaskRef::from),
        }
    }

    /// Live search (`fs.search`, live search): returns the Task
    /// ([`TaskRef`], cancelable with `TaskRef::cancel`) and the hit-batch
    /// STREAM ([`norte_proto::methods::SearchHits`]).
    ///
    /// A frontend's human is always `User` (no sandbox): embedded passes it
    /// straight to [`Engine::search_as`]; remote launches it against the
    /// daemon, which sets the actor server-side by connection.
    ///
    /// # Hit channel lifecycle
    /// - **Embedded:** the engine's walker closes `tx` on finishing, so
    ///   `rx` closes on its own (drain to `None`).
    /// - **Remote:** the `RemoteBackend`'s pump routes each `search.hits`
    ///   notification by `task_id` to this `rx`. The route is retired
    ///   —closing `rx`— when the Task reaches terminal (with a grace period
    ///   covering the hits-vs-terminal race; see `RemoteBackend::search`).
    ///   In both cases the criterion for "search finished" is the
    ///   [`TaskRef`]'s terminal state; `rx` closing is the convenient
    ///   signal that no more batches are coming.
    ///
    /// # Errors
    /// Invalid criteria (zero criteria and zero filters, a glob and a regex
    /// on the same axis, or an unrecognized encoding) →
    /// [`Error::InvalidPath`] embedded / the daemon's `INVALID_PARAMS`;
    /// otherwise, protocol taxonomy; daemon down = `ProviderUnavailable`.
    ///
    /// And against a pre-0.81 daemon with any of the filters set,
    /// [`Error::Unsupported`]: that daemon would ignore them and answer
    /// with the SUPERSET, which reads exactly like a real result. The
    /// rejection lives in the SDK (`RemoteClient::search`) and that's why
    /// it reaches everyone: this is the only path to the wire, and embedded
    /// crosses none.
    pub async fn search(
        &self,
        params: norte_proto::methods::FsSearchParams,
    ) -> Result<(TaskRef, mpsc::Receiver<norte_proto::methods::SearchHits>), Error> {
        match self {
            Self::Embedded(engine) => {
                let (handle, rx) = engine
                    .search_as(params, crate::journal::Actor::User)
                    .await?;
                Ok((TaskRef::from_handle(&handle), rx))
            }
            Self::Remote(r) => r.search(params).await.map(|(t, rx)| (TaskRef::from(t), rx)),
        }
    }

    /// How much space what's passed in takes up, as a Task (`fs.dir_size`,
    /// 0.49.0, #139).
    ///
    /// The TOTAL doesn't come back here: it travels in the Task's progress
    /// (`bytes_done`/`entries_done`), which is what the frontend already
    /// listens to for painting any other. The last snapshot is the result.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPath`] with no paths, and whatever the core returns.
    /// An N-1 daemon with no such method answers `METHOD_NOT_FOUND` →
    /// [`Error::Unsupported`], so the frontend can tell "your daemon is
    /// older" apart from a real failure.
    pub async fn dir_size(
        &self,
        params: norte_proto::methods::FsDirSizeParams,
    ) -> Result<TaskRef, Error> {
        if params.paths.is_empty() {
            return Err(Error::InvalidPath);
        }
        match self {
            Self::Embedded(engine) => {
                let handle = engine
                    .dir_size_as(params, crate::journal::Actor::User)
                    .await?;
                Ok(TaskRef::from_handle(&handle))
            }
            Self::Remote(r) => r.dir_size(params).await.map(TaskRef::from),
        }
    }

    /// The content digest of a batch of files (`fs.checksum`, 0.59.0, #311):
    /// returns the Task, and the digests are collected with
    /// [`Self::checksum_report`].
    ///
    /// **Mutates nothing**: reading isn't writing (hard rule 4 does not
    /// apply).
    ///
    /// # Errors
    /// [`Error::InvalidPath`] with an empty list; protocol taxonomy for the
    /// rest.
    pub async fn checksum(
        &self,
        params: norte_proto::methods::FsChecksumParams,
    ) -> Result<TaskRef, Error> {
        if params.paths.is_empty() {
            return Err(Error::InvalidPath);
        }
        match self {
            Self::Embedded(engine) => {
                let handle = engine
                    .checksum_as(params, crate::journal::Actor::User)
                    .await?;
                Ok(TaskRef::from_handle(&handle))
            }
            Self::Remote(r) => r.checksum(params).await.map(TaskRef::from),
        }
    }

    /// The digests that Task has computed so far (`fs.checksum_report`,
    /// 0.59.0, #311). SNAPSHOT: partial while running, final once the Task
    /// is terminal.
    ///
    /// # Errors
    /// [`Error::NotFound`] if that id was never a checksum batch of this
    /// instance or if the ring already evicted it.
    pub async fn checksum_report(
        &self,
        task_id: TaskId,
    ) -> Result<norte_proto::methods::FsChecksumReportResult, Error> {
        match self {
            // Embedded has no actor to check: this `Backend` IS the human
            // in-process (same criterion as `rename_batch_report`).
            Self::Embedded(engine) => engine
                .checksum_report(task_id)
                .map(|(_owner, r)| r)
                .ok_or(Error::NotFound),
            Self::Remote(r) => r.checksum_report(task_id).await,
        }
    }

    /// What a directory is made of, child by child (`fs.dir_usage`, 0.75.0,
    /// phase 4): returns the Task, and the map is collected with
    /// [`Self::dir_usage_report`].
    ///
    /// **Mutates nothing**: measuring isn't writing (hard rule 4 does not
    /// apply).
    ///
    /// # Errors
    /// [`Error::InvalidPath`] with `depth` at zero or above
    /// [`DIR_USAGE_MAX_DEPTH`](norte_proto::methods::DIR_USAGE_MAX_DEPTH).
    /// Both are checked HERE, before picking an arm, so embedded and remote
    /// answer the same thing — `check_pairs_cap`'s lesson. The daemon keeps
    /// checking them on its own: that is the boundary, this is parity
    /// between the two paths.
    ///
    /// **What is NOT checked here is how deep the server knows how to go.**
    /// That only `depth: 1` is served today is a daemon capability, not the
    /// type's contract: wiring it into the client would make a 0.75
    /// `Backend` reject a `depth: 2` on its own that a 0.76 daemon would
    /// actually serve, without ever asking. Whoever knows answers that, and
    /// it arrives as [`Error::Unsupported`].
    ///
    /// An N-1 daemon with no such method answers `METHOD_NOT_FOUND` → also
    /// [`Error::Unsupported`]: whoever needs to tell "doesn't know the
    /// method" apart from "that depth isn't served" knows from the `depth`
    /// it asked for.
    pub async fn dir_usage(
        &self,
        params: norte_proto::methods::FsDirUsageParams,
    ) -> Result<TaskRef, Error> {
        if params.depth == 0 || params.depth > norte_proto::methods::DIR_USAGE_MAX_DEPTH {
            return Err(Error::InvalidPath);
        }
        match self {
            Self::Embedded(engine) => {
                let handle = engine
                    .dir_usage_as(params, crate::journal::Actor::User)
                    .await?;
                Ok(TaskRef::from_handle(&handle))
            }
            Self::Remote(r) => r.dir_usage(params).await.map(TaskRef::from),
        }
    }

    /// The map that Task has measured so far (`fs.dir_usage_report`,
    /// 0.75.0, phase 4). SNAPSHOT: partial while running, final once the
    /// Task is terminal.
    ///
    /// # Errors
    /// [`Error::NotFound`] if that id was never a map of this instance or if
    /// the ring already evicted it.
    pub async fn dir_usage_report(
        &self,
        task_id: TaskId,
    ) -> Result<norte_proto::methods::FsDirUsageReportResult, Error> {
        match self {
            // Embedded has no actor to check: this `Backend` IS the human
            // in-process (same criterion as `checksum_report`).
            Self::Embedded(engine) => engine
                .dir_usage_report(task_id)
                .map(|(_owner, r)| r)
                .ok_or(Error::NotFound),
            Self::Remote(r) => r.dir_usage_report(task_id).await,
        }
    }
}
