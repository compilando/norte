//! The impure half of a batch rename: walk an ordered plan against ONE
//! provider, record every step in the journal under ONE batch id, and unwind
//! everything if any step fails or the task is cancelled.
//!
//! **The transaction is the feature.** A per-move loop that dies on the fifth
//! rename leaves four applied and the user holding a directory that is neither
//! the old one nor the new one. Here there are exactly two outcomes: the whole
//! plan landed, or the directory is byte-identical to how it started — and if
//! even THAT could not be achieved, the report names the step that stayed
//! applied instead of returning a bare error.
//!
//! **Rule 4 is what makes the failure shapes one path.** A rename that
//! succeeded but whose journal entry did not become durable DID NOT HAPPEN: an
//! undo could never find it, so leaving it applied would be a mutation outside
//! the journal. It is unwound like any other failure, and — since there is no
//! entry to compensate — its rollback is not journalled either. A provider that
//! reports failure AFTER applying the effect (issue #17: sftp acknowledges and
//! the connection dies, object storage is copy+delete) is the same shape seen
//! from the other side, and it is settled by probing rather than believed.
//!
//! **Nothing here ever overwrites.** Every step is a bare
//! [`Provider::rename`], never the move path with a collision policy: the
//! planner refuses a plan with ANY collision, so an executable plan lands every
//! step on a free name and the executor has no reason to want a "clobber on
//! conflict" option. Adding one would also smuggle a permanent delete —
//! classified `Irreversible` — inside an operation the wire announces as one
//! undoable unit (see [`crate::observer::Mutation::Renamed`]).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use norte_proto::methods::PlanHash;
use norte_proto::{ConflictKind, Error, Segment, VPath};
use norte_vfs::Provider;
use tokio_util::sync::CancellationToken;

use crate::hashing::{feed, hex_lower};
use crate::journal::{Actor, NewEntry, Reversal, SqliteJournal};
use crate::observer::{Mutation, MutationObserver};
use crate::progress::ProgressReporter;
use crate::rename::plan::{RenamePlan, Step};
use crate::undo::is_free;

/// A plan plus the token that binds it to the DIRECTORY it was planned
/// against.
///
/// The planner is pure and never sees a `VPath`, so its own hash covers the
/// steps and the verdicts and nothing else. Two different directories whose
/// re-plan produces identical steps therefore produce an identical plan hash —
/// and a hash a human approved for `~/photos` would be accepted against
/// `/etc`. Binding happens exactly once, here, on the way out of the engine:
/// what the caller receives and hands back is always the bound form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirPlan {
    /// What the planner decided. PRIVATE on purpose: `RenamePlan` carries its
    /// own unbound hash, which is another 64-char lowercase hex string and is
    /// indistinguishable from the token at a call site. Handing the wrong one
    /// to the wire would either break the feature outright or, worse, be
    /// "fixed" by comparing the unbound hash — which is the cross-directory
    /// replay this type exists to prevent, with no test to notice.
    plan: RenamePlan,
    /// The token, bound to the directory.
    hash: PlanHash,
}

impl DirPlan {
    /// Binds `plan` to `dir`. The domain string keeps this digest from ever
    /// colliding with another of the core's digests over the same bytes.
    pub(crate) fn bind(dir: &VPath, plan: RenamePlan) -> Self {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"norte-rename-plan-dir-v1");
        feed(&mut h, dir.to_wire().as_bytes());
        feed(&mut h, &plan.hash);
        // INVARIANT: `hex_lower` of a 32-byte digest is exactly 64 lowercase
        // hex digits, which is `PlanHash`'s whole contract.
        let hash = PlanHash::parse(&hex_lower(&h.finalize()))
            .expect("a sha256 in lowercase hex is a PlanHash");
        Self { plan, hash }
    }

    /// The token to approve and hand back to
    /// [`Engine::rename_batch`](crate::Engine::rename_batch). The ONLY hash
    /// that method accepts.
    #[must_use]
    pub fn hash(&self) -> &PlanHash {
        &self.hash
    }

    /// The steps and the verdicts, for rendering.
    #[must_use]
    pub fn plan(&self) -> &RenamePlan {
        &self.plan
    }

    /// Can this plan run as it is?
    #[must_use]
    pub fn executable(&self) -> bool {
        self.plan.executable()
    }
}

/// One step of the plan resolved to absolute paths.
#[derive(Debug, Clone)]
pub(crate) struct PlannedStep {
    /// The name to rename FROM, as the directory spells it.
    pub from: VPath,
    /// The name to rename TO.
    pub to: VPath,
    /// The requested pair this step descends from — what a mid-batch failure
    /// is attributed to, because the user typed rows and not steps.
    pub pair_index: u32,
    /// For a COMPENSATING batch (the undo of §6): the `seq` of the entry this
    /// step reverts. `None` for a fresh batch.
    pub undoes: Option<i64>,
}

/// Resolves a planner step to absolute paths under `dir`.
///
/// # Errors
/// [`Error::InvalidPath`] if a planned name is not a legal segment. It cannot
/// happen for names that came out of a listing, but the planner is `pub` and
/// takes opaque bytes, so this is the boundary where the guarantee becomes a
/// checked fact rather than an assumption (rule 6).
pub(crate) fn absolute(dir: &VPath, s: &Step) -> Result<PlannedStep, Error> {
    let seg = |b: &[u8]| Segment::new(b.to_vec()).map_err(|_| Error::InvalidPath);
    Ok(PlannedStep {
        from: dir.join(seg(&s.from)?),
        to: dir.join(seg(&s.to)?),
        pair_index: s.pair_index,
        undoes: None,
    })
}

/// How one step gets recorded.
///
/// Two implementations, because the engine has two shapes. With a journal
/// wired the recorder writes straight into it and hands back the `seq`, which
/// the rollback needs in order to compensate the right entry. Without one
/// ([`Engine::new`](crate::Engine::new), embedded tests) the renames still
/// reach the observer, but there is no `seq` and no compensation link — which
/// is honest, since without a journal there is no undo either.
#[async_trait]
pub(crate) trait StepJournal: Send + Sync {
    /// Records `from → to`. Returns the assigned `seq` where there is one.
    ///
    /// # Errors
    /// The sink's error. Rule 4: a step whose record does not land did not
    /// happen, and the caller unwinds it.
    async fn renamed(
        &self,
        from: &VPath,
        to: &VPath,
        undoes: Option<i64>,
    ) -> Result<Option<i64>, Error>;

    /// Does anything this recorder writes to actually END UP in the journal?
    ///
    /// Not a rhetorical question since #205 pinned the verdict: an embedded
    /// batch that starts while another process holds `journal.db` records
    /// through a no-op for its whole run, deliberately — and used to report
    /// every step as `journalled: true` anyway. `StuckStep::journalled` is what
    /// tells the operator "a later `undo` can finish the job once the obstacle
    /// is gone", so saying it over rows that do not exist sends them looking
    /// for an undo that has nothing to undo.
    fn records(&self) -> bool {
        true
    }
}

/// Records through the mutation observer: no batch, no `seq`.
pub(crate) struct ObserverJournal {
    /// Where the mutation goes.
    pub observer: Arc<dyn MutationObserver>,
    /// Who caused it.
    pub actor: Actor,
    /// Whether `observer` is anything but a no-op — see
    /// [`StepJournal::records`]. The engine knows this at construction and the
    /// observer cannot be asked, so it travels as a flag.
    pub records: bool,
}

#[async_trait]
impl StepJournal for ObserverJournal {
    async fn renamed(
        &self,
        from: &VPath,
        to: &VPath,
        _undoes: Option<i64>,
    ) -> Result<Option<i64>, Error> {
        self.observer
            .on_mutation(
                &Mutation::Renamed {
                    from,
                    to,
                    batch: None,
                },
                &self.actor,
            )
            .await?;
        Ok(None)
    }

    fn records(&self) -> bool {
        self.records
    }
}

/// Records straight into the journal under one batch id.
pub(crate) struct BatchJournal {
    /// The journal, which is also the engine's observer.
    pub journal: Arc<SqliteJournal>,
    /// Who caused it.
    pub actor: Actor,
    /// The id shared by every entry of this batch — what makes them ONE
    /// undoable unit ([`crate::journal::Journal::alloc_batch`]).
    pub batch_id: i64,
}

#[async_trait]
impl StepJournal for BatchJournal {
    async fn renamed(
        &self,
        from: &VPath,
        to: &VPath,
        undoes: Option<i64>,
    ) -> Result<Option<i64>, Error> {
        let (path, path_to) = (to.to_wire().into_bytes(), from.to_wire().into_bytes());
        let seq = self
            .journal
            .journal()
            .record_entry(&NewEntry {
                op: "renamed",
                path: &path,
                path_to: Some(&path_to),
                reversal: Reversal::RenameBack,
                reversal_ref: None,
                actor: &self.actor,
                undoes_seq: undoes,
                // The compensations of a rollback stay in the SAME batch: the
                // group has to remain readable after the fact, and an audit
                // that saw only half of it would be reading a fiction.
                batch_id: Some(self.batch_id),
            })
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "batch rename: fallo al escribir el journal");
                Error::from(e)
            })?;
        Ok(Some(seq))
    }
}

/// A step this batch could not clean up after, and exactly where it is.
///
/// `Some` in a [`BatchReport`] means the directory is HALF RENAMED. A bare
/// error would leave the user hunting for the file; this names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StuckStep {
    /// The name the file had before the batch — where it could not be put
    /// back.
    pub from: VPath,
    /// The name the file carries NOW.
    pub to: VPath,
    /// The requested pair this step descends from.
    pub pair_index: u32,
    /// Why the reversal was refused, or why the step's fate is unknown.
    pub error: Error,
    /// Whether this step has a journal entry behind it.
    ///
    /// It decides who has to clean up. `true`: the entry describes the rename,
    /// so a later `undo` can finish the job once the obstacle is gone. `false`:
    /// the rename took effect but its entry never landed (rule 4 — the journal
    /// does not know it happened), so no undo will ever find it and only a
    /// human can put this one back.
    pub journalled: bool,
    /// How many steps of this batch are still applied, this one included.
    /// Everything under it in the stack is journalled — the unjournalled step
    /// is always the last one pushed and therefore the first one popped.
    pub still_applied: u64,
}

/// What a batch did. Complete once the task reaches a terminal state.
///
/// A clean run leaves `applied == steps` and everything else empty. **Any
/// other shape is the executor telling the truth about a directory it could
/// not leave the way it found it** — read it even when the task failed, and
/// especially when the task says `Cancelled`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatchReport {
    /// Steps applied AND journalled.
    pub applied: u64,
    /// Steps the rollback put back.
    pub rolled_back: u64,
    /// The requested pair whose step failed, if one did.
    pub failed_pair: Option<u32>,
    /// The rollback was refused here and stopped; see [`StuckStep`].
    pub stuck: Option<StuckStep>,
    /// A step whose provider reported failure and which could not then be
    /// PROBED, so whether it took effect is unknown — the connection that
    /// dropped mid-rename is also the connection the two `stat`s need.
    ///
    /// It is reported rather than assumed in either direction: assuming it
    /// landed would rename a file that may not be there, and assuming it did
    /// not is how an unjournalled rename ends up outside both the journal and
    /// this report. Nothing was done about it. `to` is where to look.
    pub uncertain: Option<StuckStep>,
    /// Reversals that were APPLIED but whose compensating journal entry could
    /// not be written.
    ///
    /// The tree is back; the journal still says the forward rename is in
    /// effect. That is not cosmetic: the forward entry keeps `undoes_seq IS
    /// NULL`, so a later `undo_session` will reach it, find the destination
    /// already vacated, and block — and `undo_session` is strict LIFO, so it
    /// stops there and everything older in that session stops with it. A
    /// non-zero count here is the only warning of that.
    pub compensations_lost: u64,
}

/// One step that has been applied and may have to be put back.
struct Applied {
    from: VPath,
    to: VPath,
    pair_index: u32,
    /// The `seq` of its journal entry, when the recorder assigns one.
    seq: Option<i64>,
    /// `false` for a step that took effect WITHOUT its journal entry landing —
    /// the write failed, or the provider reported failure after applying it.
    /// Rule 4 says it did not happen, so its rollback compensates nothing and
    /// is not journalled.
    journalled: bool,
}

impl Applied {
    /// A step that took effect but has no journal entry behind it.
    fn unjournalled(s: &PlannedStep) -> Self {
        Self {
            from: s.from.clone(),
            to: s.to.clone(),
            pair_index: s.pair_index,
            seq: None,
            journalled: false,
        }
    }

    /// This step as a report entry, with the rest of the stack counted in.
    fn stuck(&self, error: Error, below: usize) -> StuckStep {
        StuckStep {
            from: self.from.clone(),
            to: self.to.clone(),
            pair_index: self.pair_index,
            error,
            journalled: self.journalled,
            still_applied: below as u64 + 1,
        }
    }
}

/// What a probe could establish about a step whose `rename` reported failure.
enum Landed {
    /// It took effect: the destination is there and the source is gone.
    Yes,
    /// It did not: the destination is free, or the source is still there.
    No,
    /// The probe itself could not answer.
    Unknown,
}

/// Did `s` take effect despite the provider reporting failure?
///
/// BOTH halves are load-bearing. A destination that exists proves nothing on
/// its own — it could be a file somebody else just created — and acting on it
/// would rename a stranger's file during a rollback. A source that is still
/// there proves the rename did not happen whatever the destination looks like.
async fn landed(provider: &dyn Provider, s: &PlannedStep) -> Landed {
    // `Ok(false)` = the destination exists; `Ok(true)` = the source is gone.
    let dest = is_free(provider, &s.to).await;
    let src = is_free(provider, &s.from).await;
    match (dest, src) {
        (Ok(false), Ok(true)) => Landed::Yes,
        (Ok(true), _) | (_, Ok(false)) => Landed::No,
        _ => Landed::Unknown,
    }
}

/// Could this `rename` error have been reported AFTER the effect landed?
///
/// A closed list of the categories that mean "nothing happened", and probe for
/// everything else — the safe direction, since the cost of probing needlessly
/// is two `stat`s on a path that already failed. `NotFound` matters most: it is
/// what a racing delete of the source produces, and probing there is what would
/// let a third party who then creates the destination steer the rollback.
fn may_have_applied(e: &Error) -> bool {
    !matches!(
        e,
        Error::NotFound
            | Error::Conflict { .. }
            | Error::InvalidPath
            | Error::PermissionDenied
            | Error::Unsupported
            | Error::PolicyDenied { .. }
    )
}

/// Executes `steps` in order against `provider`, recording each one.
///
/// Any failure — the rename, the journal write, or a cancellation observed
/// between steps — unwinds every applied step in reverse and returns the
/// original error. `report` is filled as it goes, so a caller holding it sees
/// what happened even when the answer is an error.
///
/// # Errors
/// The provider's error for the failing step, [`Error::Cancelled`], or the
/// journal's error. The error is the CAUSE; whether the directory came back is
/// in `report`.
///
/// # Panics
/// Only if `report`'s mutex is poisoned (another thread panicked holding it),
/// which is the same irrecoverable condition every other lock in the core
/// treats as a bug.
pub(crate) async fn run(
    provider: &dyn Provider,
    recorder: &dyn StepJournal,
    steps: &[PlannedStep],
    cancel: &CancellationToken,
    progress: &ProgressReporter,
    report: &Mutex<BatchReport>,
) -> Result<(), Error> {
    let total = steps.len() as u64;
    progress.update(|p| p.entries_total = Some(total));
    let mut applied: Vec<Applied> = Vec::with_capacity(steps.len());

    for s in steps {
        // Rule 3: cancellation is checked BETWEEN steps, never inside one — a
        // half-applied `rename` is not a thing, and a cancelled batch has to
        // roll back exactly like a failed one.
        if cancel.is_cancelled() {
            let blocked = unwind(provider, recorder, &mut applied, progress, report).await;
            // A rollback that got stuck must NOT answer `Cancelled`. The whole
            // repo reads a cancelled task as "the tree is as it was" (the same
            // promise a cancelled copy makes), and here it is not: the error
            // sends the task to `Failed`, where nobody assumes anything.
            return Err(blocked.unwrap_or(Error::Cancelled));
        }
        if let Err(e) = provider.rename(&s.from, &s.to).await {
            report.lock().expect("batch report lock").failed_pair = Some(s.pair_index);
            // A remote provider can answer a transient error AFTER applying the
            // effect (issue #17): sftp acknowledges and the connection dies,
            // object storage is copy+delete. Believing the error would leave
            // that one rename applied, unjournalled and outside the report —
            // the exact state this module exists to make impossible.
            if may_have_applied(&e) {
                match landed(provider, s).await {
                    Landed::Yes => {
                        tracing::warn!(
                            pair_index = s.pair_index,
                            "el rename falló DESPUÉS de aplicarse; se desanda igual",
                        );
                        applied.push(Applied::unjournalled(s));
                    }
                    Landed::No => {}
                    // The connection that dropped mid-rename is the same one
                    // the probe needs. Say so instead of guessing: guessing
                    // "landed" renames a file that may not be there, and
                    // guessing "did not" is how an unjournalled rename ends up
                    // outside both the journal and this report.
                    Landed::Unknown => {
                        tracing::error!(
                            pair_index = s.pair_index,
                            "no se pudo determinar si el rename llegó a aplicarse",
                        );
                        let unknown = Applied::unjournalled(s).stuck(e.clone(), applied.len());
                        report.lock().expect("batch report lock").uncertain = Some(unknown);
                    }
                }
            }
            unwind(provider, recorder, &mut applied, progress, report).await;
            return Err(e);
        }
        match recorder.renamed(&s.from, &s.to, s.undoes).await {
            Ok(seq) => {
                applied.push(Applied {
                    from: s.from.clone(),
                    to: s.to.clone(),
                    pair_index: s.pair_index,
                    seq,
                    // NO siempre `true` (#205): un lote embebido que empieza
                    // sin journal registra a través de un no-op de principio a
                    // fin, y decir que quedó journalizado manda al operador a
                    // buscar un undo que no existe.
                    journalled: recorder.records(),
                });
                report.lock().expect("batch report lock").applied += 1;
                let current = s.to.clone();
                progress.update(|p| {
                    p.entries_done += 1;
                    p.current = Some(current);
                });
            }
            Err(e) => {
                // Rule 4: not durable, so it did not happen. The name goes
                // back with everything else — and its reversal is NOT
                // journalled, because there is no entry to compensate and an
                // entry with no `undoes_seq` would read as a fresh mutation
                // that a later undo would dutifully try to reverse.
                report.lock().expect("batch report lock").failed_pair = Some(s.pair_index);
                applied.push(Applied::unjournalled(s));
                unwind(provider, recorder, &mut applied, progress, report).await;
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Renames every applied step back, newest first, journalling each reversal as
/// a compensation of the entry it undoes (the M3-2 mechanism, not a new one).
///
/// Returns the error that stopped it, if one did — the caller prefers that over
/// [`Error::Cancelled`], because a task that answers "cancelled" is promising a
/// tree that came back.
///
/// It is deliberately NOT cancellable: a cancelled rollback is the half-renamed
/// directory the whole feature exists to prevent, and the token that got us
/// here is already cancelled. It has no deadline either, so a batch that dies
/// on a hung connection holds its scheduler permit for the sum of the
/// reversals' timeouts — accepted, and the cost of the alternative (a partial
/// rollback on a slow but healthy link) is worse.
async fn unwind(
    provider: &dyn Provider,
    recorder: &dyn StepJournal,
    applied: &mut Vec<Applied>,
    progress: &ProgressReporter,
    report: &Mutex<BatchReport>,
) -> Option<Error> {
    while let Some(step) = applied.pop() {
        // No-clobber, checked explicitly and only on this path. `Provider::
        // rename` already refuses an occupied destination — atomically on
        // local (`renameat2(NOREPLACE)`), by its own check-then-act on sftp and
        // object. So this buys a clearer verdict and a slightly narrower window
        // on the providers that race, not a new guarantee; what makes it worth
        // one `stat` per rolled-back step is that the file at stake here is one
        // the user created WHILE the batch was failing. The same check is not
        // made on the way forward, where `rename`'s refusal is the guard and a
        // stat would only be a guess about the next instant.
        let occupied = match is_free(provider, &step.from).await {
            Ok(true) => None,
            Ok(false) => Some(Error::Conflict {
                conflict: ConflictKind::Exists,
            }),
            Err(e) => Some(e),
        };
        let blocked = match occupied {
            Some(e) => Some(e),
            None => provider.rename(&step.to, &step.from).await.err(),
        };
        if let Some(error) = blocked {
            tracing::error!(
                error = %error,
                pair_index = step.pair_index,
                journalled = step.journalled,
                "rollback del lote bloqueado: el directorio queda a medio renombrar",
            );
            let stuck = step.stuck(error.clone(), applied.len());
            report.lock().expect("batch report lock").stuck = Some(stuck);
            return Some(error);
        }
        if step.journalled {
            // The compensation is itself a mutation, and it is what marks the
            // forward entry as undone. If it does not land, the tree is back
            // but the journal is not: the forward entry stays revertible, a
            // later `undo_session` reaches it, finds the destination already
            // vacated and BLOCKS — and being strict LIFO it stops there, taking
            // everything older in that session with it. Hence the counter: this
            // is the one direction `verify_chain` cannot see.
            if let Err(e) = recorder.renamed(&step.to, &step.from, step.seq).await {
                tracing::error!(
                    error = %e,
                    pair_index = step.pair_index,
                    "reversa aplicada pero NO compensada en el journal: el undo de \
                     esta sesión se bloqueará aquí",
                );
                report.lock().expect("batch report lock").compensations_lost += 1;
            }
            // The step gave progress back, so progress gives it back too — a
            // fully rolled-back batch that still read `8/12 done` would be the
            // third teller of a different story.
            progress.update(|p| {
                p.entries_done = p.entries_done.saturating_sub(1);
                p.current = None;
            });
        }
        report.lock().expect("batch report lock").rolled_back += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use bytes::Bytes;
    use norte_proto::TaskKind;
    use norte_testkit::MemProvider;

    use super::*;
    use crate::rename::plan::{NameCaps, plan_batch};

    fn seg(b: &[u8]) -> Segment {
        Segment::new(b.to_vec()).expect("segment")
    }

    async fn write_file(mem: &MemProvider, path: &VPath, content: &[u8]) {
        let mut sink = mem.write(path).await.expect("write abre");
        sink.write(Bytes::copy_from_slice(content))
            .await
            .expect("chunk entra");
        sink.commit().await.expect("commit publica");
    }

    async fn read_all(mem: &MemProvider, path: &VPath) -> Vec<u8> {
        use futures::StreamExt;
        let mut stream = mem.read(path, None).await.expect("read abre");
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.expect("chunk"));
        }
        out
    }

    fn reporter() -> ProgressReporter {
        ProgressReporter::new(norte_proto::TaskId::new(1), TaskKind::RenameBatch).0
    }

    /// Plans `pairs` against `dir`'s real listing and resolves the steps.
    async fn steps_for(
        mem: &MemProvider,
        dir: &VPath,
        pairs: &[(Vec<u8>, Vec<u8>)],
    ) -> Vec<PlannedStep> {
        use futures::StreamExt;
        let mut stream = mem.list(dir).await.expect("list");
        let mut names = Vec::new();
        while let Some(e) = stream.next().await {
            let e = e.expect("entry");
            if let Some(n) = e.path.file_name() {
                names.push(n.as_bytes().to_vec());
            }
        }
        let plan = plan_batch(
            pairs,
            &names,
            NameCaps {
                case_sensitive: true,
            },
        );
        assert!(plan.executable(), "{:?}", plan.collisions);
        plan.steps
            .iter()
            .map(|s| absolute(dir, s).expect("segmento válido"))
            .collect()
    }

    /// A recorder that fails on the Nth call.
    struct FailAt {
        n: AtomicUsize,
        fail_on: usize,
    }

    #[async_trait]
    impl StepJournal for FailAt {
        async fn renamed(
            &self,
            _from: &VPath,
            _to: &VPath,
            _undoes: Option<i64>,
        ) -> Result<Option<i64>, Error> {
            let i = self.n.fetch_add(1, Ordering::SeqCst);
            if i + 1 == self.fail_on {
                return Err(Error::Internal { panic: false });
            }
            Ok(Some(i64::try_from(i).unwrap_or(i64::MAX) + 1))
        }
    }

    /// Rule 4: a step whose entry is not durable DID NOT HAPPEN, so the whole
    /// batch unwinds — that step included. Nothing else can prove this; a
    /// provider fault only exercises the rename half.
    #[tokio::test]
    async fn a_step_whose_journal_entry_fails_unwinds_the_batch() {
        let mem = MemProvider::new();
        let dir = MemProvider::root();
        write_file(&mem, &dir.join(seg(b"a")), b"a").await;
        write_file(&mem, &dir.join(seg(b"b")), b"b").await;
        let pairs = vec![
            (b"a".to_vec(), b"x".to_vec()),
            (b"b".to_vec(), b"y".to_vec()),
        ];
        let steps = steps_for(&mem, &dir, &pairs).await;
        assert_eq!(steps.len(), 2);

        let recorder = FailAt {
            n: AtomicUsize::new(0),
            fail_on: 2,
        };
        let report = Mutex::new(BatchReport::default());
        let err = run(
            &mem,
            &recorder,
            &steps,
            &CancellationToken::new(),
            &reporter(),
            &report,
        )
        .await
        .expect_err("el lote falla");
        assert!(matches!(err, Error::Internal { .. }), "{err:?}");

        // Both names are back: the step whose entry failed was rolled back too.
        assert!(mem.stat(&dir.join(seg(b"a"))).await.is_ok());
        assert!(mem.stat(&dir.join(seg(b"b"))).await.is_ok());
        assert!(mem.stat(&dir.join(seg(b"x"))).await.is_err());
        assert!(mem.stat(&dir.join(seg(b"y"))).await.is_err());
        let r = report.lock().expect("report lock");
        assert_eq!(r.rolled_back, 2, "los dos pasos desandados");
        assert_eq!(r.applied, 1, "solo el primero llegó a quedar registrado");
        assert_eq!(r.failed_pair, Some(1));
        assert!(r.stuck.is_none());
    }

    /// The rollback of the step that could not be journalled is NOT recorded:
    /// there is no entry to compensate, and an entry without `undoes_seq`
    /// would read as a fresh mutation that a later undo would try to reverse.
    #[tokio::test]
    async fn the_unjournalled_step_is_rolled_back_without_a_record() {
        let mem = MemProvider::new();
        let dir = MemProvider::root();
        write_file(&mem, &dir.join(seg(b"a")), b"a").await;
        write_file(&mem, &dir.join(seg(b"b")), b"b").await;
        let pairs = vec![
            (b"a".to_vec(), b"x".to_vec()),
            (b"b".to_vec(), b"y".to_vec()),
        ];
        let steps = steps_for(&mem, &dir, &pairs).await;
        let recorder = FailAt {
            n: AtomicUsize::new(0),
            fail_on: 2,
        };
        let report = Mutex::new(BatchReport::default());
        let _ = run(
            &mem,
            &recorder,
            &steps,
            &CancellationToken::new(),
            &reporter(),
            &report,
        )
        .await;
        // 1 forward record (ok) + 1 forward record (failed) + 1 compensation
        // for the ONE step that had an entry. The step that had none does not
        // get a compensation.
        assert_eq!(recorder.n.load(Ordering::SeqCst), 3);
    }

    /// A provider that fails AFTER applying the rename (issue #17) does not get
    /// to leave that rename behind. Believing the error would strand one step
    /// applied, unjournalled and unmentioned by the report — precisely the
    /// state this module exists to make impossible.
    #[tokio::test]
    async fn a_rename_that_failed_after_taking_effect_is_still_unwound() {
        let mem = MemProvider::new();
        let dir = MemProvider::root();
        write_file(&mem, &dir.join(seg(b"a")), b"a").await;
        write_file(&mem, &dir.join(seg(b"b")), b"b").await;
        let pairs = vec![
            (b"a".to_vec(), b"x".to_vec()),
            (b"b".to_vec(), b"y".to_vec()),
        ];
        let steps = steps_for(&mem, &dir, &pairs).await;
        // La PRIMERA mutación que se aplique devuelve un transitorio DESPUÉS
        // de aplicar su efecto: el `a → x` ocurre y el caller ve un error.
        mem.faults().ambiguous_mutations(1);
        let recorder = FailAt {
            n: AtomicUsize::new(0),
            fail_on: usize::MAX,
        };
        let report = Mutex::new(BatchReport::default());
        let err = run(
            &mem,
            &recorder,
            &steps,
            &CancellationToken::new(),
            &reporter(),
            &report,
        )
        .await
        .expect_err("el lote falla");
        assert!(matches!(err, Error::ProviderUnavailable { .. }), "{err:?}");
        assert!(mem.stat(&dir.join(seg(b"a"))).await.is_ok(), "`a` volvió");
        assert!(mem.stat(&dir.join(seg(b"x"))).await.is_err());
        let r = report.lock().expect("report lock").clone();
        assert_eq!(r.applied, 0, "nunca llegó a quedar registrado");
        assert_eq!(r.rolled_back, 1, "y aun así se desandó");
        assert!(r.stuck.is_none());
        // Sin entrada que compensar, la reversa tampoco se registra.
        assert_eq!(recorder.n.load(Ordering::SeqCst), 0);
    }

    /// A rollback the provider refuses stops there and NAMES the step that
    /// stayed applied. Never a bare error: the user has a half-renamed
    /// directory and needs to know where.
    ///
    /// The shape is a swap, because a swap is the only plan where one step's
    /// forward source is another step's rollback source: the temporary. Arming
    /// the fault at the temporary kills the LAST step on the way out and the
    /// FIRST one on the way back, which is exactly the "we could not undo what
    /// we did" corner.
    #[tokio::test]
    async fn a_blocked_rollback_reports_the_step_that_stayed_applied() {
        let mem = MemProvider::new();
        let dir = MemProvider::root();
        write_file(&mem, &dir.join(seg(b"a")), b"a").await;
        write_file(&mem, &dir.join(seg(b"b")), b"b").await;
        let pairs = vec![
            (b"a".to_vec(), b"b".to_vec()),
            (b"b".to_vec(), b"a".to_vec()),
        ];
        let steps = steps_for(&mem, &dir, &pairs).await;
        assert_eq!(steps.len(), 3, "dos renames y un rodeo");
        let temp = steps[0].to.clone();
        mem.faults().fail_rename_at(&temp);
        let report = Mutex::new(BatchReport::default());
        let recorder = FailAt {
            n: AtomicUsize::new(0),
            fail_on: usize::MAX,
        };
        let err = run(
            &mem,
            &recorder,
            &steps,
            &CancellationToken::new(),
            &reporter(),
            &report,
        )
        .await
        .expect_err("el lote falla");
        assert!(matches!(err, Error::Io { .. }), "{err:?}");
        let snapshot = report.lock().expect("report lock").clone();
        let stuck = snapshot.stuck.as_ref().expect("un paso atascado");
        assert_eq!(
            stuck.from,
            dir.join(seg(b"a")),
            "el fichero sigue en el temporal"
        );
        assert_eq!(stuck.to, temp);
        assert_eq!(stuck.pair_index, 0, "la fila que el usuario escribió");
        assert_eq!(stuck.still_applied, 1);
        assert_eq!(snapshot.rolled_back, 1, "el segundo paso sí volvió");
        assert!(matches!(stuck.error, Error::Io { .. }), "{:?}", stuck.error);
        // Y el fichero atascado sigue localizable por un nombre que un humano
        // reconoce como maquinaria (diseño §8: sin barrido automático).
        assert!(mem.stat(&temp).await.is_ok());
    }

    /// The rollback never clobbers. A file that appears at the old name WHILE
    /// the batch is failing survives, and its step is reported stuck.
    ///
    /// The squatter is created from inside the recorder — the exact instant
    /// between "the step landed" and "the rollback runs" — so the race is
    /// staged deterministically instead of being slept at.
    #[tokio::test]
    async fn the_rollback_refuses_to_overwrite_a_name_someone_took_back() {
        /// Writes a file at `squat` on the Nth call, then fails.
        struct SquatThenFail {
            mem: Arc<MemProvider>,
            squat: VPath,
            n: AtomicUsize,
            fail_on: usize,
        }

        #[async_trait]
        impl StepJournal for SquatThenFail {
            async fn renamed(
                &self,
                _from: &VPath,
                _to: &VPath,
                _undoes: Option<i64>,
            ) -> Result<Option<i64>, Error> {
                let i = self.n.fetch_add(1, Ordering::SeqCst);
                if i + 1 == self.fail_on {
                    write_file(&self.mem, &self.squat, b"mine").await;
                    return Err(Error::Internal { panic: false });
                }
                Ok(Some(i64::try_from(i).unwrap_or(i64::MAX) + 1))
            }
        }

        let mem = Arc::new(MemProvider::new());
        let dir = MemProvider::root();
        write_file(&mem, &dir.join(seg(b"a")), b"a").await;
        write_file(&mem, &dir.join(seg(b"b")), b"b").await;
        let pairs = vec![
            (b"a".to_vec(), b"x".to_vec()),
            (b"b".to_vec(), b"y".to_vec()),
        ];
        let steps = steps_for(&mem, &dir, &pairs).await;
        let recorder = SquatThenFail {
            mem: Arc::clone(&mem),
            squat: dir.join(seg(b"a")),
            n: AtomicUsize::new(0),
            fail_on: 2,
        };
        let report = Mutex::new(BatchReport::default());
        let err = run(
            &*mem,
            &recorder,
            &steps,
            &CancellationToken::new(),
            &reporter(),
            &report,
        )
        .await
        .expect_err("el lote falla");
        assert!(matches!(err, Error::Internal { .. }), "{err:?}");

        // `b → y` was undone; `a → x` could not be, because `a` is occupied.
        assert!(mem.stat(&dir.join(seg(b"b"))).await.is_ok());
        assert!(mem.stat(&dir.join(seg(b"y"))).await.is_err());
        assert!(mem.stat(&dir.join(seg(b"x"))).await.is_ok());
        assert_eq!(
            read_all(&mem, &dir.join(seg(b"a"))).await,
            b"mine".to_vec(),
            "el fichero del usuario sigue ahí, sin pisar",
        );
        let r = report.lock().expect("report lock");
        let stuck = r.stuck.as_ref().expect("un paso atascado");
        assert_eq!(stuck.from, dir.join(seg(b"a")));
        assert_eq!(stuck.to, dir.join(seg(b"x")));
        assert_eq!(stuck.still_applied, 1);
        assert_eq!(r.rolled_back, 1, "el otro paso sí volvió");
    }

    /// Rule 3: the token is observed BETWEEN steps and a cancelled batch
    /// leaves the tree exactly as it was — the same promise a cancelled copy
    /// makes.
    ///
    /// Cancelling from inside the recorder pins the interesting instant: one
    /// step applied and journalled, the rest not yet attempted. No sleep, no
    /// race to lose under load.
    #[tokio::test]
    async fn a_cancelled_batch_unwinds_what_it_applied() {
        /// Cancels the token after recording the Nth step.
        struct CancelAfter {
            cancel: CancellationToken,
            n: AtomicUsize,
            after: usize,
        }

        #[async_trait]
        impl StepJournal for CancelAfter {
            async fn renamed(
                &self,
                _from: &VPath,
                _to: &VPath,
                _undoes: Option<i64>,
            ) -> Result<Option<i64>, Error> {
                let i = self.n.fetch_add(1, Ordering::SeqCst);
                if i + 1 == self.after {
                    self.cancel.cancel();
                }
                Ok(Some(i64::try_from(i).unwrap_or(i64::MAX) + 1))
            }
        }

        let mem = MemProvider::new();
        let dir = MemProvider::root();
        write_file(&mem, &dir.join(seg(b"a")), b"a").await;
        write_file(&mem, &dir.join(seg(b"b")), b"b").await;
        write_file(&mem, &dir.join(seg(b"c")), b"c").await;
        let pairs = vec![
            (b"a".to_vec(), b"x".to_vec()),
            (b"b".to_vec(), b"y".to_vec()),
            (b"c".to_vec(), b"z".to_vec()),
        ];
        let steps = steps_for(&mem, &dir, &pairs).await;
        let cancel = CancellationToken::new();
        let recorder = CancelAfter {
            cancel: cancel.clone(),
            n: AtomicUsize::new(0),
            after: 1,
        };
        let report = Mutex::new(BatchReport::default());
        let err = run(&mem, &recorder, &steps, &cancel, &reporter(), &report)
            .await
            .expect_err("cancelado");
        assert_eq!(err, Error::Cancelled);
        for n in [b"a", b"b", b"c"] {
            assert!(
                mem.stat(&dir.join(seg(n))).await.is_ok(),
                "{} sigue en su sitio",
                String::from_utf8_lossy(n),
            );
        }
        for n in [b"x", b"y", b"z"] {
            assert!(mem.stat(&dir.join(seg(n))).await.is_err());
        }
        let r = report.lock().expect("report lock");
        assert_eq!(r.applied, 1);
        assert_eq!(r.rolled_back, 1, "el paso aplicado se desanda");
        assert!(r.stuck.is_none());
        // Rule 4: the compensation is journalled too — 1 forward + 1 back.
        assert_eq!(recorder.n.load(Ordering::SeqCst), 2);
    }

    // ---- issue #130: five documented claims with no test behind them ------

    /// #130 claim 1: a cancelled batch whose rollback got stuck must NOT
    /// answer `Cancelled`. The scheduler (`scheduler::run_job`) maps
    /// `Ok(Err(Error::Cancelled))` to `TaskState::Cancelled` and any other
    /// `Err` to `TaskState::Failed`, and `Cancelled` is read everywhere in
    /// this repo as "the tree came back" — the same promise a cancelled copy
    /// makes. A stuck rollback did not come back, so answering `Cancelled`
    /// here would be the executor lying about its own promise; the line this
    /// pins is `run`'s `blocked.unwrap_or(Error::Cancelled)`.
    ///
    /// Combines `CancelAfter` (cancel right after the first step lands) with
    /// `fail_rename_at` on that step's DESTINATION — the name the rollback has
    /// to rename FROM to put the file back. The forward rename is unaffected
    /// (its source is the original name, not the destination), so step one
    /// applies cleanly; only its own reversal is blocked.
    #[tokio::test]
    async fn a_cancelled_batch_whose_rollback_sticks_is_not_reported_cancelled() {
        /// Cancels the token after recording the Nth step (copy of the one in
        /// `a_cancelled_batch_unwinds_what_it_applied`: it is local to that
        /// test and this one needs its own).
        struct CancelAfter {
            cancel: CancellationToken,
            n: AtomicUsize,
            after: usize,
        }

        #[async_trait]
        impl StepJournal for CancelAfter {
            async fn renamed(
                &self,
                _from: &VPath,
                _to: &VPath,
                _undoes: Option<i64>,
            ) -> Result<Option<i64>, Error> {
                let i = self.n.fetch_add(1, Ordering::SeqCst);
                if i + 1 == self.after {
                    self.cancel.cancel();
                }
                Ok(Some(i64::try_from(i).unwrap_or(i64::MAX) + 1))
            }
        }

        let mem = MemProvider::new();
        let dir = MemProvider::root();
        write_file(&mem, &dir.join(seg(b"a")), b"a").await;
        write_file(&mem, &dir.join(seg(b"b")), b"b").await;
        let pairs = vec![
            (b"a".to_vec(), b"x".to_vec()),
            (b"b".to_vec(), b"y".to_vec()),
        ];
        let steps = steps_for(&mem, &dir, &pairs).await;
        // La reversa del primer paso tiene que renombrar `x → a`; se bloquea
        // justo eso, no la ida.
        mem.faults().fail_rename_at(&dir.join(seg(b"x")));
        let cancel = CancellationToken::new();
        let recorder = CancelAfter {
            cancel: cancel.clone(),
            n: AtomicUsize::new(0),
            after: 1,
        };
        let report = Mutex::new(BatchReport::default());
        let err = run(&mem, &recorder, &steps, &cancel, &reporter(), &report)
            .await
            .expect_err("la reversa se atasca");

        assert_ne!(
            err,
            Error::Cancelled,
            "una reversa atascada no puede contestar Cancelled: el árbol NO volvió",
        );
        assert!(matches!(err, Error::Io { .. }), "{err:?}");

        let r = report.lock().expect("report lock").clone();
        let stuck = r.stuck.as_ref().expect("un paso atascado");
        assert_eq!(stuck.from, dir.join(seg(b"a")));
        assert_eq!(stuck.to, dir.join(seg(b"x")));
        assert_eq!(stuck.still_applied, 1);
        assert!(stuck.journalled, "el paso sí llegó a apuntarse");
        assert_eq!(
            r.rolled_back, 0,
            "nada volvió: el único paso aplicado se atasca"
        );

        // El árbol queda a medio renombrar: `a` no volvió, `x` sigue ahí.
        assert!(mem.stat(&dir.join(seg(b"a"))).await.is_err());
        assert!(mem.stat(&dir.join(seg(b"x"))).await.is_ok());
        // `b` nunca se tocó: la cancelación se observó ANTES del segundo paso.
        assert!(mem.stat(&dir.join(seg(b"b"))).await.is_ok());
    }

    /// #130 claim 2: `Landed::Unknown` has to be able to reach
    /// `BatchReport::uncertain`. Nothing in the workspace produces
    /// `uncertain = Some` today — every existing assertion is `.is_none()`,
    /// which stays green even if the `Unknown` arm of `landed` were deleted.
    ///
    /// Stages it literally as the module doc describes: the rename fails with
    /// an error outside the closed list (`Io`, via `fail_rename_at`, which
    /// keeps `may_have_applied` true), and then BOTH probe `stat`s the
    /// executor sends to find out what really happened fail too —
    /// `disconnect_after` takes the provider down right after the rename call
    /// consumes its one good operation.
    #[tokio::test]
    async fn a_probe_that_cannot_answer_is_reported_uncertain() {
        let mem = MemProvider::new();
        let dir = MemProvider::root();
        write_file(&mem, &dir.join(seg(b"a")), b"a").await;
        let pairs = vec![(b"a".to_vec(), b"x".to_vec())];
        let steps = steps_for(&mem, &dir, &pairs).await;
        mem.faults().fail_rename_at(&dir.join(seg(b"a")));
        // Una operación (el propio rename) todavía pasa; los dos `stat` que
        // el probe necesita justo después ya no.
        mem.faults().disconnect_after(1);
        let recorder = FailAt {
            n: AtomicUsize::new(0),
            fail_on: usize::MAX,
        };
        let report = Mutex::new(BatchReport::default());
        let err = run(
            &mem,
            &recorder,
            &steps,
            &CancellationToken::new(),
            &reporter(),
            &report,
        )
        .await
        .expect_err("el lote falla");
        assert!(matches!(err, Error::Io { .. }), "{err:?}");

        let r = report.lock().expect("report lock").clone();
        let uncertain = r.uncertain.as_ref().expect("el probe no pudo responder");
        assert_eq!(uncertain.from, dir.join(seg(b"a")));
        assert_eq!(uncertain.to, dir.join(seg(b"x")));
        assert!(!uncertain.journalled, "nunca llegó a apuntarse");
        assert_eq!(uncertain.still_applied, 1);
        assert!(
            matches!(uncertain.error, Error::Io { .. }),
            "{:?}",
            uncertain.error
        );
        // Nada se supuso en ninguna dirección: ni aplicado ni descartado.
        assert!(r.stuck.is_none(), "{:?}", r.stuck);
        assert_eq!(r.applied, 0);
        assert_eq!(r.rolled_back, 0);
    }

    /// #130 claim 3: `compensations_lost` is documented as the ONLY warning
    /// that a later `undo_session` will block on an entry whose forward move
    /// already came back — every assertion in the suite today is `== 0`, so
    /// nothing would notice if the counter stopped incrementing.
    ///
    /// Shape of `a_step_whose_journal_entry_fails_unwinds_the_batch`, widened
    /// to three independent pairs so the third step can fail at the PROVIDER
    /// (leaving the first two forward-journalled) instead of at the recorder:
    /// `FailAt { fail_on: 3 }` then hits exactly the reversal of the second
    /// step — the first compensation the rollback attempts.
    #[tokio::test]
    async fn a_compensation_that_fails_to_write_is_counted() {
        let mem = MemProvider::new();
        let dir = MemProvider::root();
        write_file(&mem, &dir.join(seg(b"a")), b"a").await;
        write_file(&mem, &dir.join(seg(b"b")), b"b").await;
        write_file(&mem, &dir.join(seg(b"c")), b"c").await;
        let pairs = vec![
            (b"a".to_vec(), b"x".to_vec()),
            (b"b".to_vec(), b"y".to_vec()),
            (b"c".to_vec(), b"z".to_vec()),
        ];
        let steps = steps_for(&mem, &dir, &pairs).await;
        assert_eq!(steps.len(), 3);
        mem.faults().fail_rename_at(&dir.join(seg(b"c")));
        let recorder = FailAt {
            n: AtomicUsize::new(0),
            fail_on: 3,
        };
        let report = Mutex::new(BatchReport::default());
        let err = run(
            &mem,
            &recorder,
            &steps,
            &CancellationToken::new(),
            &reporter(),
            &report,
        )
        .await
        .expect_err("el tercer paso falla en el provider");
        assert!(matches!(err, Error::Io { .. }), "{err:?}");

        let r = report.lock().expect("report lock").clone();
        assert_eq!(r.applied, 2, "los dos primeros pasos sí se apuntaron");
        assert_eq!(r.rolled_back, 2, "y los dos volvieron físicamente");
        assert_eq!(
            r.compensations_lost, 1,
            "la reversa del segundo paso no pudo apuntarse",
        );
        assert!(r.stuck.is_none(), "{:?}", r.stuck);
        assert!(r.uncertain.is_none());

        // El árbol SÍ volvió: la pérdida es de contabilidad, no de ficheros.
        assert!(mem.stat(&dir.join(seg(b"a"))).await.is_ok());
        assert!(mem.stat(&dir.join(seg(b"b"))).await.is_ok());
        assert!(mem.stat(&dir.join(seg(b"c"))).await.is_ok());
        assert_eq!(recorder.n.load(Ordering::SeqCst), 4);
    }

    /// #130 claim 4: `may_have_applied`'s closed list treats `NotFound` as
    /// "nothing happened" and skips the probe entirely. That arm is
    /// documented as the one that matters most — a racing delete of the
    /// source plus a stranger creating the destination would otherwise let a
    /// probe read `Landed::Yes` and steer the rollback onto a file that is not
    /// ours. Mutating the function to `true` unconditionally turns no other
    /// test in this module red; this one is built to catch exactly that.
    ///
    /// The race is staged, not raced: the plan is resolved against a listing
    /// where `a` still exists, then `a` is removed and a stranger's file is
    /// written at the destination BEFORE the executor ever runs — the same
    /// end state a concurrent delete-and-recreate would leave, with none of
    /// the timing.
    #[tokio::test]
    async fn a_racing_delete_never_lets_the_rollback_touch_a_strangers_file() {
        let mem = MemProvider::new();
        let dir = MemProvider::root();
        write_file(&mem, &dir.join(seg(b"a")), b"a").await;
        let pairs = vec![(b"a".to_vec(), b"x".to_vec())];
        let steps = steps_for(&mem, &dir, &pairs).await;
        // La carrera: `a` desaparece y un desconocido ocupa `x` ANTES de que
        // el ejecutor llegue a tocar nada.
        mem.remove(&dir.join(seg(b"a")))
            .await
            .expect("simula el borrado ajeno");
        write_file(&mem, &dir.join(seg(b"x")), b"stranger").await;
        let recorder = FailAt {
            n: AtomicUsize::new(0),
            fail_on: usize::MAX,
        };
        let report = Mutex::new(BatchReport::default());
        let err = run(
            &mem,
            &recorder,
            &steps,
            &CancellationToken::new(),
            &reporter(),
            &report,
        )
        .await
        .expect_err("el origen ya no está");
        assert!(matches!(err, Error::NotFound), "{err:?}");

        let r = report.lock().expect("report lock").clone();
        assert!(r.stuck.is_none(), "{:?}", r.stuck);
        assert!(r.uncertain.is_none(), "{:?}", r.uncertain);
        assert_eq!(r.applied, 0);
        assert_eq!(r.rolled_back, 0, "nada que desandar: no se probó nada");

        // LA propiedad: el fichero del desconocido sigue siendo suyo.
        assert_eq!(
            read_all(&mem, &dir.join(seg(b"x"))).await,
            b"stranger".to_vec(),
        );
        assert!(
            mem.stat(&dir.join(seg(b"a"))).await.is_err(),
            "sigue sin volver"
        );
    }

    /// #130 claim 5: no test anywhere unwinds a plan that went through a
    /// planner-owned TEMPORARY cleanly. Every existing rollback test on
    /// independent renames never touches a temp; every existing test that
    /// DOES involve a temp arms `fail_rename_at` ON the temp itself to force
    /// the failure, which necessarily also blocks the temp's own reversal
    /// (its source, on the way back, IS the temp) — so those tests end STUCK
    /// by construction and never exercise the success path the temporary
    /// exists for.
    ///
    /// Failing the swap's LAST leg (`temp → b`) at the JOURNAL instead of the
    /// provider sidesteps that: the rename physically lands, so nothing is
    /// armed against the temp path, and its later reversal (`b → temp`, then
    /// `temp → a`) runs unobstructed. Content, not just names, proves the
    /// right bytes came home.
    #[tokio::test]
    async fn a_swap_through_a_temporary_unwinds_cleanly_with_the_right_bytes() {
        let mem = MemProvider::new();
        let dir = MemProvider::root();
        write_file(&mem, &dir.join(seg(b"a")), b"a").await;
        write_file(&mem, &dir.join(seg(b"b")), b"b").await;
        let pairs = vec![
            (b"a".to_vec(), b"b".to_vec()),
            (b"b".to_vec(), b"a".to_vec()),
        ];
        let steps = steps_for(&mem, &dir, &pairs).await;
        assert_eq!(steps.len(), 3, "dos renames y un rodeo");
        let temp = steps[0].to.clone();
        let third_pair = steps[2].pair_index;

        // El tercer paso (`temp → b`) ATERRIZA en el provider; solo su
        // apunte de journal falla, así que nada queda armado sobre `temp`.
        let recorder = FailAt {
            n: AtomicUsize::new(0),
            fail_on: 3,
        };
        let report = Mutex::new(BatchReport::default());
        let err = run(
            &mem,
            &recorder,
            &steps,
            &CancellationToken::new(),
            &reporter(),
            &report,
        )
        .await
        .expect_err("el tercer apunte falla");
        assert!(matches!(err, Error::Internal { .. }), "{err:?}");

        let r = report.lock().expect("report lock").clone();
        assert_eq!(r.applied, 2, "los dos primeros pasos se apuntaron");
        assert_eq!(r.rolled_back, 3, "los tres, rodeo incluido, volvieron");
        assert_eq!(r.compensations_lost, 0);
        assert!(r.stuck.is_none(), "{:?}", r.stuck);
        assert!(r.uncertain.is_none());
        assert_eq!(r.failed_pair, Some(third_pair));

        // LA propiedad: los bytes, no solo los nombres.
        assert_eq!(read_all(&mem, &dir.join(seg(b"a"))).await, b"a".to_vec());
        assert_eq!(read_all(&mem, &dir.join(seg(b"b"))).await, b"b".to_vec());
        assert!(mem.stat(&temp).await.is_err(), "el temporal no sobrevive");
        assert_eq!(recorder.n.load(Ordering::SeqCst), 5);
    }

    /// The executor's OWN no-clobber guard, proved against a provider that
    /// really does overwrite.
    ///
    /// `the_rollback_refuses_to_overwrite_a_name_someone_took_back` passes with
    /// the `is_free` check deleted, because `MemProvider::rename` refuses on its
    /// own — so what it proves is the provider's contract, not this belt. The
    /// providers the belt was written for (sftp posix-rename, object
    /// copy+delete) are exactly the ones that cannot refuse, and `rename_clobbers`
    /// is how the testkit can now speak for them.
    #[tokio::test]
    async fn the_rollback_holds_even_when_the_provider_would_overwrite() {
        struct SquatThenFail {
            mem: Arc<MemProvider>,
            squat: VPath,
            n: AtomicUsize,
        }

        #[async_trait]
        impl StepJournal for SquatThenFail {
            async fn renamed(
                &self,
                _from: &VPath,
                _to: &VPath,
                _undoes: Option<i64>,
            ) -> Result<Option<i64>, Error> {
                if self.n.fetch_add(1, Ordering::SeqCst) == 1 {
                    write_file(&self.mem, &self.squat, b"mine").await;
                    return Err(Error::Internal { panic: false });
                }
                Ok(Some(1))
            }
        }

        let mem = Arc::new(MemProvider::new());
        let dir = MemProvider::root();
        write_file(&mem, &dir.join(seg(b"a")), b"a").await;
        write_file(&mem, &dir.join(seg(b"b")), b"b").await;
        let pairs = vec![
            (b"a".to_vec(), b"x".to_vec()),
            (b"b".to_vec(), b"y".to_vec()),
        ];
        let steps = steps_for(&mem, &dir, &pairs).await;
        let recorder = SquatThenFail {
            mem: Arc::clone(&mem),
            squat: dir.join(seg(b"a")),
            n: AtomicUsize::new(0),
        };
        // A provider whose `rename` overwrites, like the two remote ones.
        mem.faults().rename_clobbers(true);
        let report = Mutex::new(BatchReport::default());
        let _ = run(
            &*mem,
            &recorder,
            &steps,
            &CancellationToken::new(),
            &reporter(),
            &report,
        )
        .await;
        assert_eq!(
            read_all(&mem, &dir.join(seg(b"a"))).await,
            b"mine".to_vec(),
            "el fichero del usuario sobrevive a un provider que pisa",
        );
        let r = report.lock().expect("report lock").clone();
        let stuck = r.stuck.as_ref().expect("un paso atascado");
        assert_eq!(
            stuck.error,
            Error::Conflict {
                conflict: ConflictKind::Exists
            },
            "y el veredicto es NUESTRO, no el del provider",
        );
        assert!(stuck.journalled, "este paso sí tiene apunte: el undo puede");
    }

    /// `landed` needs BOTH halves. A destination that exists proves nothing on
    /// its own: here the rename failed with the source still in place, and a
    /// third party owns the destination name. Probing only the destination
    /// would answer "it landed" and the rollback would rename a stranger's
    /// file — and rename it OUTSIDE the journal, since an unjournalled step
    /// records nothing.
    #[tokio::test]
    async fn a_destination_a_stranger_created_does_not_look_like_a_landed_rename() {
        let mem = MemProvider::new();
        let dir = MemProvider::root();
        write_file(&mem, &dir.join(seg(b"a")), b"a").await;
        write_file(&mem, &dir.join(seg(b"b")), b"b").await;
        let pairs = vec![
            (b"a".to_vec(), b"x".to_vec()),
            (b"b".to_vec(), b"y".to_vec()),
        ];
        let steps = steps_for(&mem, &dir, &pairs).await;
        // El segundo paso falla con `b` INTACTO, y alguien ocupa `y`.
        mem.faults().fail_rename_at(&dir.join(seg(b"b")));
        write_file(&mem, &dir.join(seg(b"y")), b"suya").await;
        let recorder = FailAt {
            n: AtomicUsize::new(0),
            fail_on: usize::MAX,
        };
        let report = Mutex::new(BatchReport::default());
        let err = run(
            &mem,
            &recorder,
            &steps,
            &CancellationToken::new(),
            &reporter(),
            &report,
        )
        .await
        .expect_err("el lote falla");
        assert!(matches!(err, Error::Io { .. }), "{err:?}");
        assert_eq!(
            read_all(&mem, &dir.join(seg(b"y"))).await,
            b"suya".to_vec(),
            "el fichero ajeno sigue siendo suyo",
        );
        assert!(
            mem.stat(&dir.join(seg(b"b"))).await.is_ok(),
            "`b` no se movió"
        );
        let r = report.lock().expect("report lock").clone();
        assert_eq!(r.rolled_back, 1, "solo el primer paso había que desandar");
        assert!(r.stuck.is_none());
        assert!(r.uncertain.is_none());
    }

    /// `still_applied` counts the whole stack, not just the blocked step. It is
    /// the number the user is told to go and clean up, so a batch stuck with
    /// three renames in effect has to say three.
    #[tokio::test]
    async fn still_applied_counts_everything_left_in_effect() {
        struct SquatOn {
            mem: Arc<MemProvider>,
            squat: VPath,
            n: AtomicUsize,
            at: usize,
        }

        #[async_trait]
        impl StepJournal for SquatOn {
            async fn renamed(
                &self,
                _from: &VPath,
                _to: &VPath,
                _undoes: Option<i64>,
            ) -> Result<Option<i64>, Error> {
                let i = self.n.fetch_add(1, Ordering::SeqCst);
                if i + 1 == self.at {
                    write_file(&self.mem, &self.squat, b"ocupado").await;
                    return Err(Error::Internal { panic: false });
                }
                Ok(Some(i64::try_from(i).unwrap_or(i64::MAX) + 1))
            }
        }

        let mem = MemProvider::new();
        let dir = MemProvider::root();
        for n in [b"a", b"b", b"c", b"d"] {
            write_file(&mem, &dir.join(seg(n)), n).await;
        }
        // Una permutación de cuatro: un rodeo por temporal, y el rodeo es el
        // único paso cuya reversa se puede bloquear sin bloquear la ida.
        let pairs = vec![
            (b"a".to_vec(), b"b".to_vec()),
            (b"b".to_vec(), b"c".to_vec()),
            (b"c".to_vec(), b"d".to_vec()),
            (b"d".to_vec(), b"a".to_vec()),
        ];
        let steps = steps_for(&mem, &dir, &pairs).await;
        assert_eq!(steps.len(), 5, "cuatro renames y un rodeo");
        let temp = steps[0].to.clone();
        mem.faults().fail_rename_at(&temp);
        let recorder = FailAt {
            n: AtomicUsize::new(0),
            fail_on: usize::MAX,
        };
        let report = Mutex::new(BatchReport::default());
        let _ = run(
            &mem,
            &recorder,
            &steps,
            &CancellationToken::new(),
            &reporter(),
            &report,
        )
        .await;
        let r = report.lock().expect("report lock").clone();
        let stuck = r.stuck.as_ref().expect("un paso atascado");
        // El último paso (temp → destino) muere; los tres de en medio vuelven;
        // el primero (origen → temp) no puede, y ES el único que sigue puesto.
        assert_eq!(r.rolled_back, 3);
        assert_eq!(stuck.still_applied, 1);
        assert!(stuck.journalled, "ese paso sí tiene apunte");

        // Y el caso feo: el ÚLTIMO paso es el que se queda puesto, con los
        // cuatro anteriores debajo — y es el paso SIN apunte de journal, que
        // por construcción se apila el último y se desapila el primero. Nadie
        // podrá deshacerlo nunca: el journal no sabe que ocurrió.
        let mem2 = Arc::new(MemProvider::new());
        for n in [b"a", b"b", b"c", b"d"] {
            write_file(&mem2, &dir.join(seg(n)), n).await;
        }
        let steps2 = steps_for(&mem2, &dir, &pairs).await;
        let temp2 = steps2[0].to.clone();
        // El quinto apunte falla Y deja ocupado el nombre al que ese mismo paso
        // tendría que volver (`temp`), así que la reversa se bloquea de entrada.
        let recorder2 = SquatOn {
            mem: Arc::clone(&mem2),
            squat: temp2,
            n: AtomicUsize::new(0),
            at: 5,
        };
        let report2 = Mutex::new(BatchReport::default());
        let _ = run(
            &*mem2,
            &recorder2,
            &steps2,
            &CancellationToken::new(),
            &reporter(),
            &report2,
        )
        .await;
        let r2 = report2.lock().expect("report lock").clone();
        let stuck2 = r2.stuck.as_ref().expect("un paso atascado");
        assert_eq!(
            stuck2.still_applied, 5,
            "el bloqueado más los cuatro que quedan debajo",
        );
        assert_eq!(r2.rolled_back, 0);
        assert!(
            !stuck2.journalled,
            "y sin apunte: este no lo arregla ningún undo",
        );
    }

    /// The observer path (`Engine::new`, no journal): the renames still reach
    /// the observer and so do the reversals. Without a journal there is no undo
    /// either, so recording the reversal as a plain `Renamed` is honest — but
    /// it should be pinned, because it is the path every embedded caller takes.
    #[tokio::test]
    async fn the_observer_path_reports_both_the_step_and_its_reversal() {
        struct Counting {
            n: AtomicUsize,
        }

        #[async_trait]
        impl MutationObserver for Counting {
            async fn on_mutation(
                &self,
                mutation: &Mutation<'_>,
                _actor: &Actor,
            ) -> Result<(), Error> {
                assert!(
                    matches!(mutation, Mutation::Renamed { batch: None, .. }),
                    "sin journal no hay lote que agrupar",
                );
                self.n.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }

        let mem = MemProvider::new();
        let dir = MemProvider::root();
        write_file(&mem, &dir.join(seg(b"a")), b"a").await;
        write_file(&mem, &dir.join(seg(b"b")), b"b").await;
        let pairs = vec![
            (b"a".to_vec(), b"x".to_vec()),
            (b"b".to_vec(), b"y".to_vec()),
        ];
        let steps = steps_for(&mem, &dir, &pairs).await;
        mem.faults().fail_rename_at(&dir.join(seg(b"b")));
        let observer = Arc::new(Counting {
            n: AtomicUsize::new(0),
        });
        let recorder = ObserverJournal {
            observer: Arc::clone(&observer) as Arc<dyn MutationObserver>,
            actor: Actor::User,
            records: true,
        };
        let report = Mutex::new(BatchReport::default());
        let _ = run(
            &mem,
            &recorder,
            &steps,
            &CancellationToken::new(),
            &reporter(),
            &report,
        )
        .await;
        assert!(mem.stat(&dir.join(seg(b"a"))).await.is_ok());
        assert_eq!(observer.n.load(Ordering::SeqCst), 2, "la ida y la vuelta");
    }

    /// Binding is what stops a hash approved for one directory from being
    /// replayed against another whose re-plan happens to produce the same
    /// steps.
    #[test]
    fn the_same_plan_in_two_directories_has_two_tokens() {
        let pairs = vec![(b"a".to_vec(), b"x".to_vec())];
        let listing = vec![b"a".to_vec()];
        let caps = NameCaps {
            case_sensitive: true,
        };
        let plan = plan_batch(&pairs, &listing, caps);
        let here = DirPlan::bind(&VPath::parse("mem:///here").expect("path"), plan.clone());
        let there = DirPlan::bind(&VPath::parse("mem:///there").expect("path"), plan.clone());
        assert_eq!(here.plan().hash, there.plan().hash, "el planner es puro");
        assert_ne!(here.hash(), there.hash(), "el token no lo es");
        // And it is stable: the same directory and the same plan, twice.
        let again = DirPlan::bind(&VPath::parse("mem:///here").expect("path"), plan);
        assert_eq!(here.hash(), again.hash());
    }
}
