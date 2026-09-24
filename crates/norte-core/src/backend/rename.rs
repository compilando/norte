//! [`Backend`](super::Backend)'s rename and organize area: the reviewable
//! `fs.rename_batch*` batch, and the `ai.rename_plan` / `ai.organize_plan`
//! AI plans with their application (`organize`).

use norte_proto::{Error, TaskId, VPath};

use super::{AI_CALL_TIMEOUT, Backend, TaskRef};

impl Backend {
    /// The REVIEWABLE plan for a batch of renames inside `dir` (spec §17,
    /// ADR 0042). Mutates NOTHING: no Task, no journal.
    ///
    /// What's sent is INTENT — pairs of base names. The order, the
    /// temporaries and the verdicts are decided by the core (hard rule 7),
    /// and the returned `plan_hash` is what has to be given back to
    /// [`Backend::rename_batch`] to execute EXACTLY what was shown.
    ///
    /// # Errors
    /// [`Error::InvalidPath`] if a name isn't a legal directory entry or if
    /// there are more than `FS_RENAME_BATCH_MAX_PAIRS` pairs;
    /// [`Error::Unsupported`] with no provider or with a read-only one;
    /// [`Error::LimitExceeded`] on an unmanageable directory;
    /// [`Error::PolicyDenied`] from the read gate (remote, a scopeless
    /// agent); protocol taxonomy.
    pub async fn rename_batch_plan(
        &self,
        dir: &VPath,
        pairs: &[norte_proto::methods::RenamePair],
    ) -> Result<norte_proto::methods::FsRenameBatchPlanResult, Error> {
        match self {
            Self::Embedded(engine) => {
                let raw = crate::rename::pairs_from_wire(pairs);
                let plan = engine.rename_batch_plan(dir, &raw).await?;
                crate::rename::plan_to_proto(&plan)
            }
            #[cfg(unix)]
            Self::Remote(r) => r.rename_batch_plan(dir, pairs).await,
        }
    }

    /// Executes the approved batch as ONE Task and ONE undoable journal
    /// unit (spec §17, ADR 0042).
    ///
    /// `plan_hash` is [`Backend::rename_batch_plan`]'s FRESHNESS token,
    /// bound to the directory. The core re-plans the directory AS IT
    /// STANDS NOW and compares: if it drifted, this is [`Error::PlanStale`]
    /// and nothing is touched. It isn't proof of approval —the digest is
    /// public and computable without ever having requested the plan—: it
    /// guarantees WHAT runs, not that someone looked at it. What happened is
    /// requested with [`Backend::rename_batch_report`] — the terminal Task
    /// tells the cause, not what was left half-done.
    ///
    /// # Errors
    /// [`Error::PlanStale`] if the directory drifted since the plan;
    /// [`Error::PlanNotExecutable`] if the approved plan had collisions;
    /// [`Error::PolicyDenied`] from the mutation gate; plus
    /// [`Backend::rename_batch_plan`]'s.
    pub async fn rename_batch(
        &self,
        dir: &VPath,
        pairs: &[norte_proto::methods::RenamePair],
        plan_hash: &norte_proto::methods::PlanHash,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let raw = crate::rename::pairs_from_wire(pairs);
                let (handle, _report) = engine.rename_batch(dir, &raw, plan_hash).await?;
                // The report stays in the engine's ring, which is where
                // `rename_batch_report` reads it from: both arms are
                // requested the same way.
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r
                .rename_batch(dir, pairs, plan_hash)
                .await
                .map(TaskRef::from),
        }
    }

    /// The report for an already-launched batch (`fs.rename_batch_report`,
    /// 0.36.0): how many steps applied, how many were undone and —what no
    /// bare error can say— WHICH step stayed applied and under what name.
    ///
    /// Check it too when the Task says `cancelled`: cancelling a batch
    /// undoes it, and a rollback can get stuck too.
    ///
    /// # Errors
    /// [`Error::NotFound`] if that `task_id` was never a batch of this
    /// process or if the ring already evicted it — and the remote arm
    /// answers the SAME thing, because the daemon sends that category and
    /// not an untaxonomized `-32602`; [`Error::Unsupported`] against an N-1
    /// daemon that doesn't know the method; protocol taxonomy.
    pub async fn rename_batch_report(
        &self,
        task_id: TaskId,
    ) -> Result<norte_proto::methods::FsRenameBatchReportResult, Error> {
        match self {
            Self::Embedded(engine) => engine
                .rename_batch_report(task_id)
                .map(|(_owner, r)| crate::rename::report_to_proto(&r))
                // Embedded has no actor to check: this `Backend` IS the
                // human in-process (same criterion as
                // `plugins_set_approval`).
                .ok_or(Error::NotFound),
            #[cfg(unix)]
            Self::Remote(r) => r.rename_batch_report(task_id).await,
        }
    }

    /// `dir`'s reviewable rename plan via AI (M4-IA, ADR 0031). Mutates
    /// NOTHING: applying it is a batch, [`Backend::rename_batch_plan`] and
    /// then [`Backend::rename_batch`] (that's how the TUI, the window and
    /// the CLI do it). BOTH arms are bounded by `AI_CALL_TIMEOUT` (2 min): a
    /// provider endpoint gone dead-air never hangs the embedded or the
    /// remote frontend.
    ///
    /// # Errors
    /// [`Error::Unsupported`] with no AI provider; [`Error::PolicyDenied`]
    /// from the AI gate (off, local-only, denied prefix);
    /// [`Error::ProviderUnavailable`] (retryable) when the timeout runs
    /// out; protocol taxonomy for provider failures.
    /// `names` are the MARKED basenames (#121). Empty = the whole directory,
    /// which is what this method used to do: with first-class selection,
    /// requesting a plan over five files used to send the directory's
    /// thousand to the provider.
    pub async fn ai_rename_plan(
        &self,
        dir: &VPath,
        instruction: &str,
        names: &[String],
    ) -> Result<norte_proto::methods::AiRenamePlanResult, Error> {
        match self {
            Self::Embedded(engine) => {
                let plan = tokio::time::timeout(
                    AI_CALL_TIMEOUT,
                    engine.ai_rename_plan_for(dir, instruction, names),
                )
                .await
                .map_err(|_| Error::ProviderUnavailable { retryable: true })??;
                Ok(crate::ai::ai_plan_to_proto(plan))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.ai_rename_plan(dir, instruction, names).await,
        }
    }

    /// The AI ORGANIZE plan (0.77.0, phase 8): reviewable, mutates nothing.
    ///
    /// # Errors
    /// `Unsupported` with no provider; the AI gate with its reason; protocol
    /// taxonomy. `ProviderUnavailable` when the timeout runs out, like its
    /// sibling.
    pub async fn ai_organize_plan(
        &self,
        dir: &VPath,
        instruction: &str,
        names: &[String],
    ) -> Result<norte_proto::methods::AiOrganizePlanResult, Error> {
        match self {
            Self::Embedded(engine) => {
                let plan = tokio::time::timeout(
                    AI_CALL_TIMEOUT,
                    engine.ai_organize_plan_for(dir, instruction, names),
                )
                .await
                .map_err(|_| Error::ProviderUnavailable { retryable: true })??;
                // The token travels WITH the plan (see `organize::plan_hash`):
                // without it the modal would open over something that
                // cannot be approved.
                let plan_hash = if plan.moves.is_empty() {
                    None
                } else {
                    Some(crate::organize::plan_hash(dir, &plan.moves)?)
                };
                Ok(norte_proto::methods::AiOrganizePlanResult {
                    moves: plan.moves,
                    refused: None,
                    plan_hash,
                })
            }
            #[cfg(unix)]
            Self::Remote(r) => r.ai_organize_plan(dir, instruction, names).await,
        }
    }

    /// Applies an organize plan (0.77.0, phase 8): creates the folders and
    /// moves, as ONE undoable batch.
    ///
    /// # Errors
    /// `PlanStale` if the token isn't the reviewed plan's; `InvalidPath` if
    /// some destination escapes the directory; protocol taxonomy.
    pub async fn organize(
        &self,
        dir: &VPath,
        moves: &[norte_proto::methods::OrganizeMove],
        plan_hash: &norte_proto::methods::PlanHash,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let handle = engine
                    .organize(dir, moves, plan_hash, crate::journal::Actor::User)
                    .await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.organize(dir, moves, plan_hash).await.map(TaskRef::from),
        }
    }
}
