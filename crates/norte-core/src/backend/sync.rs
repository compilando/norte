//! [`Backend`](super::Backend)'s tree comparison and sync area (ADR
//! 0048/0049): `fs.compare`, and the `sync.plan` / `sync.apply` /
//! `sync.report` cycle.

use norte_proto::{Error, TaskId};
use tokio::sync::mpsc;

use super::{Backend, EMBEDDED_CONN_ID, TaskRef};

impl Backend {
    /// Comparison of two trees (`fs.compare`, 0.39.0, ADR 0048): returns the
    /// Task ([`TaskRef`], cancelable) and the row-batch STREAM
    /// ([`norte_proto::methods::CompareRowsBatch`]).
    ///
    /// Same channel lifecycle as [`Self::search`]: embedded, the walk
    /// closes `tx` on finishing; remote, the pump routes each `compare.rows`
    /// by `task_id` and the route is retired after the terminal (with the
    /// same grace). The criterion for "comparison finished" is the
    /// [`TaskRef`]'s terminal state; the `rx` closing is the convenient
    /// signal.
    ///
    /// **Mutates nothing**: no journal, no undo (hard rule 4 does not
    /// apply).
    ///
    /// # When ALL the rows are in
    /// `rx` closing does NOT mean "all arrived": a notification can be
    /// lost (the daemon evicts a subscriber that doesn't drain, the
    /// client's pump discards a batch if its buffer fills, and a
    /// reconnection drops the routes, closing `rx` indistinguishably from a
    /// clean end). The signal is
    /// [`TaskProgress::entries_done`](norte_proto::TaskProgress::entries_done),
    /// which on a [`TaskKind::Compare`](norte_proto::TaskKind::Compare) Task
    /// counts ROWS emitted: received rows are compared against that number,
    /// and **AFTER `rx` closes**, not when the terminal snapshot arrives
    /// —the row pump and the progress pump are different tasks, so the
    /// terminal can get ahead of the last batch—. Whoever is going to WRITE
    /// from these rows (spec 2's sync plan) has to make that check.
    ///
    /// # Errors
    /// Two equal roots → [`Error::InvalidPath`]; `follow_symlinks: true` →
    /// [`Error::Unsupported`]. Both are checked HERE, before picking an
    /// arm, so embedded and remote answer the same thing: the daemon
    /// rejects them with a bare `-32602` (that's its published contract)
    /// and `to_taxonomy` would turn that into `Internal`, i.e. the same
    /// answer a panicking provider gives. The daemon keeps checking them on
    /// its own: that is the boundary, this is parity between the two paths
    /// (same criterion as `check_pairs_cap`).
    ///
    /// An N-1 daemon (0.38.x) with no such method answers `METHOD_NOT_FOUND`,
    /// delivered as [`Error::Unsupported`] — "your daemon is older", not a
    /// real failure. Otherwise, protocol taxonomy; daemon down =
    /// `ProviderUnavailable`.
    pub async fn compare(
        &self,
        params: norte_proto::methods::FsCompareParams,
    ) -> Result<
        (
            TaskRef,
            mpsc::Receiver<norte_proto::methods::CompareRowsBatch>,
        ),
        Error,
    > {
        if params.follow_symlinks {
            return Err(Error::Unsupported);
        }
        if params.left == params.right {
            return Err(Error::InvalidPath);
        }
        match self {
            Self::Embedded(engine) => {
                let (handle, rx) = engine
                    .compare_as(params, crate::journal::Actor::User)
                    .await?;
                Ok((TaskRef::from_handle(&handle), rx))
            }
            #[cfg(unix)]
            Self::Remote(r) => r
                .compare(params)
                .await
                .map(|(t, rx)| (TaskRef::from(t), rx)),
        }
    }

    /// Plans a one-way sync (`sync.plan`, 0.40.0, ADR 0049): returns the
    /// Task ([`TaskRef`], cancelable) and the plan's event STREAM — capped
    /// batches of steps and, at the end, the
    /// [`SyncPlanDone`](norte_proto::methods::SyncPlanDone) that CLOSES it
    /// and carries the `plan_hash`.
    ///
    /// **Mutates nothing**: underneath it's a comparison with a
    /// per-row decision. [`Self::sync_apply`] is the one that writes, and
    /// only with the hash that arrives here.
    ///
    /// # Event order is channel order
    /// `sync.plan_done` ALWAYS arrives after the last batch of steps, on
    /// both arms: the core puts both into an `mpsc` and the client's pump
    /// routes them to the same `rx`. A close arriving before a batch would
    /// be a client approving the hash of a plan that was still arriving.
    ///
    /// # When ALL the steps are in
    /// `sync.plan_done` is the signal, and its absence is the protection:
    /// without it there's no `plan_hash`, and with no `plan_hash` nothing
    /// can be applied. All THREE ways of losing a batch fail on that side:
    ///
    /// 1. the daemon evicts whoever doesn't drain its outbox → its pump
    ///    stops and its retained plans are swept;
    /// 2. a reconnection drops the routes → `rx` closes;
    /// 3. **this process's buffer fills up** because whoever consumes `rx`
    ///    is slower than the daemon. This is the only one a client does to
    ///    itself, and that's why the routing CLOSES the feed instead of
    ///    discarding the batch (`OnFull::CloseFeed`): discarding it and
    ///    delivering the close afterward —which is what `search.hits` and
    ///    `compare.rows` do, where a batch is just paint— would leave a
    ///    human approving a hash that covers steps they never saw.
    ///
    /// Even so, whoever paints these steps should tally them:
    /// `SyncPlanDone::counts` sums the WHOLE plan (`create_dir + copy +
    /// overwrite + delete_tree + skip`), so comparing that sum against the
    /// received steps detects any future loss without depending on the
    /// transport signaling it. `TaskProgress::entries_done` counts the same
    /// thing from the other side.
    ///
    /// # The plan is left RETAINED
    /// Approving costs a file in the daemon's state directory, alive for
    /// [`SYNC_PLAN_TTL_MS`](norte_proto::methods::SYNC_PLAN_TTL_MS) and
    /// bound to this connection. There's a cap on retained plans per
    /// connection: past it, the daemon answers `OVERLOADED` with no
    /// taxonomy —the request is valid, the moment isn't— and this arm
    /// delivers it as [`Error::Internal`], same as the rest of the
    /// daemon's bare `-32602`/`-32603`s. This cannot be anticipated here
    /// because only the daemon knows how many plans this connection is
    /// retaining.
    ///
    /// # Errors
    /// [`Error::Unsupported`] if `compare.follow_symlinks` or
    /// `compare.descend_orphans` are set (neither belongs to the caller:
    /// the planner sets the second one alongside the source);
    /// [`Error::InvalidPath`] if `include` exceeds
    /// [`SYNC_MAX_INCLUDE`](norte_proto::methods::SYNC_MAX_INCLUDE). All
    /// three are checked HERE, before picking an arm, for the same reason
    /// as in [`Self::compare`]: the daemon rejects them with a bare
    /// `-32602` and `to_taxonomy` would turn that into `Internal`, i.e. the
    /// same answer a panicking provider gives. The engine keeps checking
    /// them on its own.
    ///
    /// Anticipating them changes the ORDER of two rejections, worth
    /// knowing: against an engine with no spool, this answers by the
    /// parameter (`InvalidPath`) where the engine would have answered by
    /// retention (`Unsupported`); and against a daemon, a scopeless agent
    /// gets the parameter complaint from its own process instead of the
    /// daemon's `PolicyDenied`, which gates before validating. Neither one
    /// filters anything —these three checks don't look at paths— and it's
    /// the same asymmetry [`Self::compare`] already has.
    ///
    /// Also: [`Error::OverlappingRoots`] if the two roots overlap (that one
    /// IS a wire category and comes from the engine, no copy here),
    /// [`Error::Unsupported`] if the daemon has no spool installed or is an
    /// N-1 daemon with no such method; otherwise, protocol taxonomy.
    pub async fn sync_plan(
        &self,
        params: norte_proto::methods::SyncPlanParams,
    ) -> Result<(TaskRef, mpsc::Receiver<crate::sync::SyncPlanEvent>), Error> {
        if params.compare.follow_symlinks || params.compare.descend_orphans.is_some() {
            return Err(Error::Unsupported);
        }
        if params
            .include
            .as_ref()
            .is_some_and(|inc| inc.len() > norte_proto::methods::SYNC_MAX_INCLUDE)
        {
            return Err(Error::InvalidPath);
        }
        match self {
            Self::Embedded(engine) => {
                let (handle, rx) = engine
                    .sync_plan_as(params, EMBEDDED_CONN_ID, crate::journal::Actor::User)
                    .await?;
                Ok((TaskRef::from_handle(&handle), rx))
            }
            #[cfg(unix)]
            Self::Remote(r) => r
                .sync_plan(params)
                .await
                .map(|(t, rx)| (TaskRef::from(t), rx)),
        }
    }

    /// Executes the APPROVED plan `plan_hash` names (`sync.apply`, 0.40.0,
    /// ADR 0049) as ONE Task and ONE undoable journal batch.
    ///
    /// **The hash is the only parameter**, and that's the guarantee: there's
    /// no way to ask for something other than what [`Self::sync_plan`]
    /// showed to be executed. The two roots, the mode and the criteria come
    /// from the retained plan.
    ///
    /// **The plan is spent**: applying it consumes it, no matter what
    /// happens. A second `sync_apply` of the same hash is
    /// [`Error::PlanStale`], which is true.
    ///
    /// What really happened is requested with [`Self::sync_report`]: a
    /// step that fails is a ROW of the report and not the Task's ending, so
    /// the terminal state doesn't tell even half of it.
    ///
    /// # Errors
    /// [`Error::PlanStale`] if the hash doesn't name a live plan of this
    /// process (doesn't exist, expired, was tampered with, or was already
    /// applied); [`Error::PlanNotExecutable`] if the plan carried blocks;
    /// [`Error::PolicyDenied`] from the gate, which runs over the roots read
    /// from the plan and NOW, not when it was planned; [`Error::Unsupported`]
    /// with no spool or no journal (applying with no journal would be
    /// burying with no way back, hard rule 4), or against an N-1 daemon;
    /// otherwise, protocol taxonomy.
    pub async fn sync_apply(
        &self,
        plan_hash: &norte_proto::methods::PlanHash,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let (handle, _report) = engine
                    .sync_apply_as(plan_hash, EMBEDDED_CONN_ID, crate::journal::Actor::User)
                    .await?;
                // The report stays in the engine's ring, which is where
                // `sync_report` reads it from: both arms are requested the
                // same way.
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.sync_apply(plan_hash).await.map(TaskRef::from),
        }
    }

    /// The report for an already-launched application (`sync.report`,
    /// 0.40.0): how many steps ran, how many failed and why —with each
    /// one's path— and under which journal batch what did apply landed.
    ///
    /// It's a SNAPSHOT: final once the Task is terminal, partial before.
    /// Check it too when it says `cancelled`: what was applied up to the
    /// cutoff stays, journalled — half a sync is a real state.
    ///
    /// # Who sees what
    /// The daemon serves the report to whoever could see the Task: its
    /// owner, or any HUMAN connection. A `Backend::Remote` opened with
    /// [`super::remote::RemoteBackend::connect`] is human, and through it
    /// AGENTS' application reports are also visible — deliberate, and
    /// undo's symmetry: a human who governs the daemon can read what an
    /// agent did. One opened with
    /// [`super::remote::RemoteBackend::connect_as_agent`] is NOT (the MCP
    /// bridge introduced it): it sees only its own and nothing else, and
    /// for it "not yours" and "doesn't exist" are the same answer.
    ///
    /// Note the asymmetry, which isn't an oversight: AUTHORIZATION (the
    /// plan) is per connection, and its report is per ACTOR. Two human
    /// connections are the same `Actor::User`, so one reads the other's
    /// report even though it couldn't have applied its plan.
    ///
    /// # Errors
    /// [`Error::NotFound`] if that `task_id` was never an application of
    /// this process, if the ring already evicted it, or if whoever's asking
    /// couldn't see it. [`Error::Unsupported`] against an N-1 daemon;
    /// otherwise, protocol taxonomy.
    pub async fn sync_report(
        &self,
        task_id: TaskId,
    ) -> Result<norte_proto::methods::SyncReportResult, Error> {
        match self {
            Self::Embedded(engine) => engine
                .sync_report(task_id)
                .map(|(_owner, r)| r)
                // Embedded has no actor to check: this `Backend` IS the
                // human in-process (same criterion as `rename_batch_report`).
                .ok_or(Error::NotFound),
            #[cfg(unix)]
            Self::Remote(r) => r.sync_report(task_id).await,
        }
    }
}
