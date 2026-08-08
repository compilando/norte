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
    /// What the planner decided: ordered steps, verdicts, and its own hash
    /// (which is NOT the token — see [`Self::hash`]).
    pub plan: RenamePlan,
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
    /// [`Engine::rename_batch`](crate::Engine::rename_batch).
    #[must_use]
    pub fn hash(&self) -> &PlanHash {
        &self.hash
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
}

/// Records through the mutation observer: no batch, no `seq`.
pub(crate) struct ObserverJournal {
    /// Where the mutation goes.
    pub observer: Arc<dyn MutationObserver>,
    /// Who caused it.
    pub actor: Actor,
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

/// The step the rollback could not undo. `Some` in a [`BatchReport`] means the
/// directory is HALF RENAMED and the user has to be told exactly where — a
/// bare error would leave them hunting for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StuckStep {
    /// The name the file had before the batch — where it could not be put
    /// back.
    pub from: VPath,
    /// The name the file carries NOW.
    pub to: VPath,
    /// The requested pair this step descends from.
    pub pair_index: u32,
    /// Why the reversal was refused.
    pub error: Error,
    /// How many steps of this batch are still applied, this one included.
    /// Every one of them is still described by its journal entry, so a later
    /// `undo` can finish the job once the obstacle is gone.
    pub still_applied: u64,
}

/// What a batch did. Complete once the task reaches a terminal state.
///
/// A clean run leaves `applied == steps` and everything else empty. Any other
/// shape is the executor telling the truth about a directory it could not
/// leave the way it found it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatchReport {
    /// Steps applied AND journalled.
    pub applied: u64,
    /// Steps the rollback put back.
    pub rolled_back: u64,
    /// The requested pair whose step failed, if one did.
    pub failed_pair: Option<u32>,
    /// Set when the rollback itself was refused; see [`StuckStep`].
    pub stuck: Option<StuckStep>,
}

/// `true` when `p` does not exist.
async fn is_free(provider: &dyn Provider, p: &VPath) -> Result<bool, Error> {
    match provider.stat(p).await {
        Err(Error::NotFound) => Ok(true),
        Ok(_) => Ok(false),
        Err(e) => Err(e),
    }
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
}

/// Did `s` take effect despite the provider reporting failure?
///
/// Only `true` when the destination is there AND the source is gone: a
/// destination alone could be a file somebody else just created, and guessing
/// wrong here means renaming a stranger's file during a rollback. Anything the
/// probe cannot establish — a `stat` that errors too — answers `false`, which
/// leaves the step alone rather than acting on a guess.
async fn landed(provider: &dyn Provider, s: &PlannedStep) -> bool {
    matches!(is_free(provider, &s.to).await, Ok(false))
        && matches!(is_free(provider, &s.from).await, Ok(true))
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
            unwind(provider, recorder, &mut applied, report).await;
            return Err(Error::Cancelled);
        }
        if let Err(e) = provider.rename(&s.from, &s.to).await {
            report.lock().expect("batch report lock").failed_pair = Some(s.pair_index);
            // A remote provider can answer a transient error AFTER applying the
            // effect (issue #17): sftp acknowledges and the connection dies,
            // object storage is copy+delete. Believing the error would leave
            // that one rename applied, unjournalled and outside the report —
            // the exact state this module exists to make impossible. Two stats
            // on the error path settle it.
            if landed(provider, s).await {
                tracing::warn!(
                    pair_index = s.pair_index,
                    "el rename falló DESPUÉS de aplicarse; se desanda igual",
                );
                applied.push(Applied::unjournalled(s));
            }
            unwind(provider, recorder, &mut applied, report).await;
            return Err(e);
        }
        match recorder.renamed(&s.from, &s.to, s.undoes).await {
            Ok(seq) => {
                applied.push(Applied {
                    from: s.from.clone(),
                    to: s.to.clone(),
                    pair_index: s.pair_index,
                    seq,
                    journalled: true,
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
                unwind(provider, recorder, &mut applied, report).await;
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Renames every applied step back, newest first, journalling each reversal as
/// a compensation of the entry it undoes (the M3-2 mechanism, not a new one).
///
/// It is deliberately NOT cancellable: a cancelled rollback is the half-renamed
/// directory the whole feature exists to prevent, and the token that got us
/// here is already cancelled.
async fn unwind(
    provider: &dyn Provider,
    recorder: &dyn StepJournal,
    applied: &mut Vec<Applied>,
    report: &Mutex<BatchReport>,
) {
    while let Some(step) = applied.pop() {
        // No-clobber, checked explicitly and only on this path. `rename` is
        // contractually non-overwriting and local closes it atomically with
        // `renameat2(NOREPLACE)`, but sftp and object cannot (posix-rename
        // clobbers; object is copy+delete), and here the file that would be
        // destroyed is one the user created WHILE the batch was failing. One
        // `stat` per rolled-back step, on the error path only, is worth it.
        // The same check is not made on the way forward: there `rename`'s own
        // refusal is the guard, it costs nothing, and it is atomic where a
        // stat is merely a guess about the next instant.
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
                "rollback del lote bloqueado: el directorio queda a medio renombrar",
            );
            report.lock().expect("batch report lock").stuck = Some(StuckStep {
                from: step.from,
                to: step.to,
                pair_index: step.pair_index,
                error,
                // This step plus everything still under it in the stack.
                still_applied: applied.len() as u64 + 1,
            });
            return;
        }
        if step.journalled {
            // The compensation is itself a mutation. If IT cannot be recorded
            // the effect has already happened, so the loop carries on and the
            // journal is left describing more than the tree holds — logged
            // loudly, because that is the one direction a `verify_chain` will
            // not catch.
            if let Err(e) = recorder.renamed(&step.to, &step.from, step.seq).await {
                tracing::error!(
                    error = %e,
                    "paso de rollback aplicado pero NO registrado en el journal",
                );
            }
        }
        report.lock().expect("batch report lock").rolled_back += 1;
    }
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
        assert_eq!(here.plan.hash, there.plan.hash, "el planner es puro");
        assert_ne!(here.hash(), there.hash(), "el token no lo es");
        // And it is stable: the same directory and the same plan, twice.
        let again = DirPlan::bind(&VPath::parse("mem:///here").expect("path"), plan);
        assert_eq!(here.hash(), again.hash());
    }
}
