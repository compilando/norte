//! The transducer: comparison rows go in, plan steps come out.
//!
//! A row produces ZERO or ONE element: a step, or a blocker, or nothing. Two
//! identical trees produce zero steps, not a million `Skip`s: what is NOTABLE
//! is emitted, what is boring is not.
//!
//! The order is the walk's, which is pre-order, so a `CreateDir` always
//! precedes what goes inside it with nothing to sort here. That pre-order is
//! not just a presentation convenience: the transducer's only two pieces of
//! state — the prefix pruned by an overlap, and the spellings of paired
//! directories — rely on the parent arriving BEFORE its children.
//!
//! And on that, and on NOTHING else: the walk emits rows by DIRECTORY — all
//! of one level, then each subdirectory's — so between a folder's row and its
//! children's, all of its siblings slip in. State that assumes "the children
//! come right after" breaks with the first sibling, silently and over a
//! user's tree.

use std::collections::{BTreeMap, VecDeque};
use std::pin::Pin;

use futures::stream::{self, FusedStream, Stream, StreamExt};
use norte_compare::CompareError;
use norte_proto::methods::{
    CompareConfidence, CompareReason, CompareRow, CompareVerdict, OnUnknown, RelPath, Side,
    StepReversal, SyncBlocker, SyncBlockerKind, SyncMode, SyncReason, SyncStep, SyncStepKind,
};
use norte_proto::{Entry, EntryKind, VPath};
use tokio_util::sync::CancellationToken;

use crate::{SyncError, SyncOptions};

/// What the plan's flow carries: a step that is going to be executed, or a
/// reason the whole plan cannot be approved.
///
/// Both travel over the SAME flow and not over two channels: a blocker
/// appears where the walk found it, so the panel can show it in its place,
/// and whoever accumulates the plan does not have to reconcile two
/// sequences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanItem {
    /// A plan step.
    Step {
        /// What travels over the wire and what enters the `plan_hash`.
        step: SyncStep,
        /// What the comparison saw at the destination, for the steps that
        /// are going to destroy it. See [`DestWitness`].
        dest: Option<DestWitness>,
    },
    /// A reason the plan is not executable.
    Blocker(SyncBlocker),
}

/// What the comparison saw at the DESTINATION entry a destructive step is
/// going to land on.
///
/// # Why it exists, and why it is not on the wire
/// The executor has to revalidate before destroying: up to ten minutes pass
/// between a human approving a plan and it being applied, and a `stat` that
/// compares the destination against **what the plan noted about it** is the
/// only thing standing between that TTL and a lost file. Nothing in
/// [`SyncStep`] serves as a reference: [`SyncStep::size`] is the bytes the
/// step MOVES, i.e. the SOURCE's, and there is no field describing the
/// destination's prior state.
///
/// It does not travel on the wire because nobody on the other side needs
/// it — the panel paints the step, not a snapshot of the destination — and
/// because putting it there would publish a second description of the
/// destination tree with its sizes and dates. It travels in the spool, which
/// belongs to this process, and does not go into the `plan_hash`: it is
/// where the conclusion CAME FROM, not the conclusion.
///
/// # What it can and cannot do
/// A provider that lists without size or date — `file://` is one — leaves
/// both fields at `None`, and then revalidation is left with "it still
/// exists and is still the same class". It is less, and it is honest:
/// faking a zero would mean declaring a conflict on every step. An entry
/// from a pair DOES arrive hydrated (the cascade needs size and date to
/// decide), so the case that matters — [`SyncStepKind::Overwrite`] — carries
/// it populated.
///
/// ```
/// use norte_proto::{Entry, EntryKind, VPath};
/// use norte_sync::DestWitness;
///
/// let entry = Entry {
///     path: VPath::parse("file:///destino/a.txt").expect("path"),
///     kind: EntryKind::File,
///     size: Some(1234),
///     mtime_ms: Some(1_726_000_000_000),
///     attrs: Default::default(),
/// };
/// let snapshot = DestWitness::of(&entry);
/// assert_eq!(snapshot.kind, EntryKind::File);
/// assert_eq!(snapshot.size, Some(1234));
///
/// // A provider that does not measure leaves both at `None`, never a faked
/// // zero: revalidation is then left with "exists and is the same class".
/// let sparse = Entry { size: None, mtime_ms: None, ..entry };
/// assert_eq!(DestWitness::of(&sparse).size, None);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DestWitness {
    /// The class it had. A class change is always a conflict: the step was
    /// approved over a file and now there is a directory, or the other way
    /// around.
    pub kind: EntryKind,
    /// The size it had; `None` = the provider did not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// The date it had, in ms; `None` = the provider did not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime_ms: Option<i64>,
    /// How many entries its FIRST LEVEL had, for a directory (#176).
    ///
    /// `None` = not counted: it is not a directory, or it had more than what
    /// gets counted without counting being the job. A `None` does **not**
    /// relax anything on its own — revalidation only compares what both
    /// snapshots carry, same as with size and date.
    ///
    /// Exists because a directory's `stat` only moves when its DIRECT
    /// children change, so a `DeleteTree` used to revalidate clean against a
    /// subtree that had gained a hundred files two levels down. The count
    /// does not close that case — it still cannot see a grandchild — and it
    /// does catch the common one: someone put something there while the
    /// human was deciding. It is the loosest check of the step with the
    /// widest reach, and now it is less loose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entries: Option<u64>,
}

impl DestWitness {
    /// The snapshot of `entry`, with no child count: whoever can count
    /// them — the one with a provider — adds it with [`Self::with_entries`].
    #[must_use]
    pub fn of(entry: &Entry) -> Self {
        Self {
            kind: entry.kind,
            size: entry.size,
            mtime_ms: entry.mtime_ms,
            entries: None,
        }
    }

    /// The same snapshot, with the first level's count (#176).
    #[must_use]
    pub fn with_entries(self, entries: Option<u64>) -> Self {
        Self { entries, ..self }
    }
}

/// Plans: transduces the row flow into a step flow.
///
/// `rows` is the flow [`norte_compare::compare`] returns over the SAME two
/// roots `opts` carries. Nothing in the signature can require it and
/// everything depends on it: if the roots do not match byte for byte, every
/// `rel` coming out of here names something else (hence
/// [`SyncError::OutsideRoot`] and [`SyncError::RootIsNotAStep`], which is how
/// it is found out).
///
/// `cancel` is the Task's token (hard rule 3) and must be **the same one**
/// given to the comparison: it is checked once PER ROW, before requesting the
/// next one, so a different token cuts nothing until upstream produces
/// something. With the same token, the cut is immediate because the walk sees
/// it too.
///
/// It opens nothing, lists nothing and writes nothing.
///
/// The flow is FUSED ([`FusedStream`]): asking it for another element after
/// the end returns `None` instead of panicking, which is what `futures`'s raw
/// `Unfold` does. A `select!` over it is legal.
///
/// The flow ends as soon as it emits an [`Err`]: a cancellation, or a caller
/// failure ([`SyncError::SourceSideUnknown`], [`SyncError::OutsideRoot`],
/// [`SyncError::RootIsNotAStep`], [`SyncError::ModeNotPlanned`]). Everything
/// else that goes wrong in a tree is a step or a blocker.
///
/// # THIS flow decides the overlap, not just the caller
/// Copying `/a` onto `/a/sub` copies a tree inside itself. The STRUCTURAL
/// check of the two roots belongs to the caller (`sync.plan` does it with
/// `Error::OverlappingRoots`) and is not assumed done here: as soon as a row
/// carries a source path that reaches the DESTINATION's root — or the other
/// way around — a [`SyncBlockerKind::OverlapDetected`] comes out and that
/// whole subtree is pruned, without a single step.
///
/// What this flow CANNOT see is two different [`VPath`]s naming the same tree
/// (a symlink, an SFTP root under two authorities, a file opened by two
/// paths): that requires canonicalizing, which costs a trip per comparison
/// and not every provider offers it (ADR 0048). And conversely, two EQUAL
/// roots are not taken for an overlap: the transducer knows nothing about
/// providers, so two different providers that spell their root the same —
/// two `mem:///`s in a test, for instance — would arrive here
/// indistinguishable from a tree against itself. That case does not write
/// anything extra anyway: a tree compared against itself gives `Same` rows,
/// i.e. zero steps. What is dangerous is CONTAINMENT, which is what gets
/// pruned.
///
/// # A plan is not atomic while it is being produced
/// Steps come out BEFORE it is known whether the plan is going to die, so
/// whoever accumulates them will see steps of a plan that ends in an error.
/// It is harmless because then `sync.plan_done` never arrives and
/// `sync.apply` carries nothing but a `plan_hash` that was never emitted —
/// but whoever stores the steps cannot assume the opposite.
///
/// # The two spellings of the same pair
/// `rel` comes from the SOURCE's bytes. `norte-compare` pairs by a FOLDED
/// key — always NFC, uppercase when either side is not case sensitive — so
/// two entries with DIFFERENT bytes get paired without the row saying so
/// (`reason: None`): an NFC `café` against an NFD `café`, a `README` against
/// an APFS's `readme`. The row carries BOTH `Entry`s, so it can be seen here,
/// and what comes out is a [`SyncStep::dest_rel`] populated with the
/// destination path when its bytes are not the source's. The executor then
/// writes onto the file that EXISTS instead of creating a second one
/// alongside it, and the trash its reversal promises really buries something
/// (issue #152).
///
/// The destination is NOT renamed to the source's spelling: that would turn
/// every macOS↔Linux synchronization into a dance of renames.
///
/// **And what is only on the SOURCE inherits its folder's spelling.** A new
/// file inside a directory the two sides spell differently carries no
/// destination entry, so there is no second spelling to READ from the row —
/// but there is one to REMEMBER: the row of the directory pair, which is
/// `Same` and produces no step, is the one that knows it. The transducer
/// records `(source → destination)` for every directory pair whose two paths
/// differ and resolves each row by its deepest ancestor, so `café/new.txt`
/// comes out with `dest_rel = café(NFD)/new.txt` and the executor writes
/// INSIDE the directory that exists instead of creating a second `café`
/// alongside it (the other half of issue #152).
///
/// Ancestor comparison is by SEGMENTS and by bytes, never by string prefix:
/// `café` is not a prefix of `cafétière`.
///
/// # The flow has to arrive COMPLETE
/// From that come the two requirements the signature cannot enforce. `rows`
/// must carry, for every row, all of its ANCESTORS' — because a folder's
/// spelling travels in the folder's own row, which is `Same` and produces no
/// step — and it must arrive in walk order, which is pre-order. A caller that
/// wants to plan only a selection (`sync.plan`'s `include`) has to filter the
/// OUTPUT flow, never the input one: dropping `café`'s `Same` row leaves
/// `café/new.txt` without a `dest_rel` and reopens #152 right where it was
/// closed.
///
/// ```
/// use futures::StreamExt;
/// use norte_proto::VPath;
/// use norte_proto::methods::{
///     CompareConfidence, CompareCriterion, CompareRow, CompareVerdict,
/// };
/// use norte_proto::{Entry, EntryKind};
/// use norte_sync::{OnUnknown, PlanItem, Side, SyncMode, SyncOptions, SyncStepKind, plan};
/// use tokio_util::sync::CancellationToken;
///
/// let opts = SyncOptions {
///     source_root: VPath::parse("file:///origen").expect("path"),
///     dest_root: VPath::parse("file:///destino").expect("path"),
///     mode: SyncMode::Update,
///     on_unknown: OnUnknown::Copy,
///     source_side: Side::Left,
///     dest_has_trash: true,
///     dest_trash_restorable: true,
///     dest_writable: true,
/// };
/// let row = CompareRow {
///     id: 0,
///     left: Some(Entry {
///         path: VPath::parse("file:///origen/informe%FF%FE.dat").expect("path"),
///         kind: EntryKind::File,
///         size: Some(1234),
///         mtime_ms: None,
///         attrs: std::collections::BTreeMap::default(),
///     }),
///     right: None,
///     verdict: CompareVerdict::OnlyLeft,
///     criterion: CompareCriterion::Presence,
///     confidence: CompareConfidence::Certain,
///     newer: None,
///     reason: None,
///     side: None,
///     paired_under: None,
/// };
///
/// let items: Vec<_> = futures::executor::block_on(
///     plan(futures::stream::iter(vec![Ok(row)]), opts, CancellationToken::new()).collect(),
/// );
/// let PlanItem::Step { step, .. } = items[0].as_ref().expect("no error") else {
///     panic!("a step");
/// };
/// assert_eq!(step.kind, SyncStepKind::Copy);
/// // And the non-UTF-8 name arrives whole, relative to the roots (hard rule 1).
/// assert_eq!(step.rel.to_wire(), "informe%FF%FE.dat");
/// ```
pub fn plan<'a, S>(
    rows: S,
    opts: SyncOptions,
    cancel: CancellationToken,
) -> impl FusedStream<Item = Result<PlanItem, SyncError>> + 'a
where
    S: Stream<Item = Result<CompareRow, CompareError>> + 'a,
{
    // The two WIRING failures are decided here and not per row: a plan whose
    // mode this binary does not know how to plan, or whose source names no
    // side, must not end up empty-and-approvable just because the comparison
    // produced no rows.
    let mirroring = matches!(opts.mode, SyncMode::Mirror);
    let fatal = if !matches!(opts.mode, SyncMode::Update | SyncMode::Mirror) {
        Some(SyncError::ModeNotPlanned(opts.mode))
    } else if opts.source_side == Side::Unknown {
        Some(SyncError::SourceSideUnknown)
    } else {
        None
    };
    // A destination that does not accept writes blocks the WHOLE plan, and it
    // is said before requesting the first row: the blocker does not depend on
    // the tree having anything inside it, so an empty source must produce it
    // just the same. And not a single row gets looked at — there is nothing a
    // tree could say that would change the result, and dragging the whole
    // walk through for it costs minutes of network time.
    let mut pending = VecDeque::new();
    if !opts.dest_writable && fatal.is_none() {
        pending.push_back(PlanItem::Blocker(SyncBlocker {
            rel: RelPath::default(),
            kind: SyncBlockerKind::DestReadOnly,
            // Not about one place: it is about the whole tree, and the tree
            // is the DESTINATION (plan convention: source `Left`, destination
            // `Right`).
            side: Some(Side::Right),
        }));
    }
    let source_is_left = opts.source_side == Side::Left;
    let transducer = Transducer {
        rows: Box::pin(rows),
        opts,
        cancel,
        pending,
        next_id: 0,
        finished: false,
        overlap: None,
        spellings: BTreeMap::new(),
        fatal,
        source_is_left,
        mirroring,
    };
    stream::unfold(transducer, |mut t| async move {
        let item = t.step().await?;
        Some((item, t))
    })
    // `Unfold` is not fusable on its own and `FusedStream` is not an
    // auto-trait that leaks through the `impl Trait`: without this, a caller
    // that polls it once too many — any loop with a `select!` and a flush
    // `tick` does — gets a panic AFTER having planned correctly.
    .fuse()
}

/// The transducer's state: the input flow, what a row left half-emitted, and
/// the `id` counter.
struct Transducer<S> {
    /// The row flow, already pinned (`Box::pin`) to be able to ask it for the
    /// next one without requiring `Unpin` of the caller.
    rows: Pin<Box<S>>,
    /// What the plan needs that the rows do not carry.
    opts: SyncOptions,
    /// The Task's token.
    cancel: CancellationToken,
    /// What the last row produced and has not been delivered yet.
    pending: VecDeque<PlanItem>,
    /// The next step `id`. Monotonic within ONE plan.
    next_id: u64,
    /// Once `true` the flow never produces anything again.
    finished: bool,
    /// The subtree an overlap pruned, when there was one.
    ///
    /// A SINGLE one, not a set, because what is kept is not the path of the
    /// row that uncovered it but the ROOT that was reached, which is what
    /// contains the whole overlapping subtree. Pre-order does the rest: the
    /// parent arrives before its children, so the first row that enters the
    /// subtree raises it and every other one falls within the same prefix.
    overlap: Option<Overlap>,
    /// How each paired directory whose two spellings do NOT match byte for
    /// byte is spelled at the DESTINATION.
    ///
    /// A MAP and not a stack, and that is the only shape that works: the walk
    /// emits rows by DIRECTORY — all of one level, then, one by one, its
    /// subdirectories' — so between the `café` pair and its `café/new.txt`
    /// child, all of `café`'s siblings slip in. A stack that got popped at
    /// the first sibling would lose the translation right before needing it
    /// (or worse, with two levels: it would be left with the grandparent's
    /// and would name a directory that exists on neither side). A map only
    /// needs the parent to arrive BEFORE its children, which is exactly what
    /// pre-order guarantees.
    ///
    /// The key is the SOURCE path and the value the DESTINATION's, WHOLE: the
    /// deepest entry that is an ancestor of a row already carries inside it
    /// the translation of all its ancestors, so it is resolved with ONE
    /// lookup.
    ///
    /// Costs memory proportional to the number of paired directories spelled
    /// differently — zero in a homogeneous tree, one per accented folder in a
    /// macOS↔Linux synchronization — and is anyway much less than the plan
    /// this flow produces and that whoever consumes it retains whole. Without
    /// it, what is only on the source comes out naming a folder that does not
    /// exist at the destination (issue #152).
    spellings: BTreeMap<RelPath, RelPath>,
    /// The WIRING failure that ends the plan as soon as the first element is
    /// requested, if there is one.
    ///
    /// Decided at construction and not when absorbing the first row: a plan
    /// whose mode or whose source come in wrong must not come out
    /// empty-and-approvable just because both trees were empty.
    fatal: Option<SyncError>,
    /// Is the LEFT side of the rows the source? Resolved once, here, instead
    /// of reinterpreting [`SyncOptions::source_side`] per row.
    source_is_left: bool,
    /// Does this plan delete what is left over at the destination
    /// ([`SyncMode::Mirror`])?
    mirroring: bool,
}

/// The overlap the walk found: which root was reached and which side the
/// path that reached it came from.
///
/// The side matters. If a SOURCE path reached `dest_root`, what has to be
/// pruned are the rows whose SOURCE side falls inside it; the destination's
/// all fall under `dest_root` by definition — that is what the whole tree
/// hangs off — and pruning them too would take down the whole plan instead
/// of the subtree.
#[derive(Debug, Clone)]
struct Overlap {
    /// The root reached: everything at or below it is pruned.
    prefix: VPath,
    /// `true` if SOURCE paths are looked at, `false` if the destination's.
    from_source: bool,
}

impl<S> Transducer<S>
where
    S: Stream<Item = Result<CompareRow, CompareError>>,
{
    /// The plan's next element, or `None` when it is over.
    async fn step(&mut self) -> Option<Result<PlanItem, SyncError>> {
        loop {
            if self.finished {
                return None;
            }
            // The check goes BEFORE draining the pending queue, and the
            // pending queue is thrown away: no step comes out after the cut,
            // not even one already computed. Same contract as
            // `norte_compare::walk`. Today a row produces at most one
            // element, so pending is at most the blocker seeded at
            // construction — but the contract is about what COMES OUT, not
            // about how many can fit.
            if self.cancel.is_cancelled() {
                self.finished = true;
                self.pending.clear();
                return Some(Err(SyncError::Cancelled));
            }
            // The wiring was checked at construction: it ends the flow
            // without looking at a single row, and also when there is none.
            if let Some(fatal) = self.fatal.take() {
                self.finished = true;
                self.pending.clear();
                return Some(Err(fatal));
            }
            if let Some(item) = self.pending.pop_front() {
                return Some(Ok(item));
            }
            if !self.opts.dest_writable {
                // The blocker already came out (`plan` seeded it) and there
                // is nothing more to say: no row can change that the
                // destination is not writable. Not one is requested, and
                // dropping the flow stops the walk feeding it too.
                self.finished = true;
                return None;
            }
            match self.rows.next().await {
                None => {
                    self.finished = true;
                    return None;
                }
                Some(Err(CompareError::Cancelled)) => {
                    self.finished = true;
                    return Some(Err(SyncError::Cancelled));
                }
                Some(Err(other)) => {
                    self.finished = true;
                    return Some(Err(SyncError::Compare(other)));
                }
                Some(Ok(row)) => {
                    if let Err(e) = self.absorb(&row) {
                        self.finished = true;
                        return Some(Err(e));
                    }
                }
            }
        }
    }

    /// Translates ONE row into zero or one element in `pending`.
    ///
    /// The mode, the source's side and the destination's writability were
    /// already resolved when the transducer was built: there is no option to
    /// interpret here, only the row.
    fn absorb(&mut self, row: &CompareRow) -> Result<(), SyncError> {
        let (source_orphan, dest_orphan) = if self.source_is_left {
            (CompareVerdict::OnlyLeft, CompareVerdict::OnlyRight)
        } else {
            (CompareVerdict::OnlyRight, CompareVerdict::OnlyLeft)
        };
        let (source, dest) = if self.source_is_left {
            (row.left.as_ref(), row.right.as_ref())
        } else {
            (row.right.as_ref(), row.left.as_ref())
        };
        let mirroring = self.mirroring;

        // The overlap, before anything else: inside the pruned subtree
        // nothing is planned, not even a `Skip`.
        if self.overlap_prunes(source, dest) {
            return Ok(());
        }
        if let Some(blocker) = self.overlap_reached(source, dest)? {
            self.pending.push_back(PlanItem::Blocker(blocker));
            return Ok(());
        }

        // The source's `rel` is computed ONCE per row, and before the early
        // returns: the spelling map is also fed from rows that produce no
        // step (a directory pair is `Same`).
        let source_rel = match source {
            Some(entry) => Some(rel_under(&self.opts.source_root, &entry.path)?),
            None => None,
        };
        if let Some(rel) = source_rel.as_ref() {
            self.remember_spelling(rel, source, dest)?;
        }

        // An error row does not translate into anything that acts: it
        // translates into a `Skip` that NAMES it — or, if it is a destination
        // directory that does not fit, into a blocker. It goes before the
        // verdict because it has none worth using (`Error` is neither `Same`
        // nor `Different`).
        if row.verdict == CompareVerdict::Error {
            return self.absorb_error(row, source, dest, source_rel);
        }
        // And a name collision has no verdict to map either: it is a `Skip`
        // if the source collided and a BLOCKER if the destination did.
        if row.verdict == CompareVerdict::Ambiguous {
            return self.absorb_ambiguous(row, source, dest, source_rel);
        }
        // And a pair that only holds together through a NON-INJECTIVE
        // transformation is not touched (#207, ADR 0053): `K.txt` with U+212A
        // KELVIN SIGN against `K.txt` with the ASCII `K` are two files for
        // ext4 and one for Unicode, so the `Overwrite` that used to come out
        // of here wrote one's bytes over the OTHER's. That is #152's data
        // loss, and 0.42.0 put the data on the wire precisely so it could be
        // stopped here.
        //
        // The criterion is `names_one_text()` and not the specific variant:
        // every transformation this binary cannot assert names a single text
        // is skipped, including one that names a newer daemon. `CaseFold` and
        // `Normalization` STILL act — they are the pairs for which the key
        // exists, and denying them would break the macOS↔Linux case they
        // serve.
        //
        // It goes before the verdict because it does not depend on it: what
        // is not trustworthy is the PAIR, and an untrustworthy pair is
        // neither overwritten nor declared equal.
        if row.paired_under.is_some_and(|t| !t.names_one_text()) {
            return self.absorb_non_injective(row, source, dest, source_rel);
        }

        // The `Skip`'s reason, when the verdict ends in one. Set by whoever
        // decides the class, the only one who knows it.
        let mut skip_reason: Option<SyncReason> = None;
        // The destination entry THIS row paired with, the only one a second
        // spelling can come from. A source orphan paired with nothing, so it
        // is nulled out down there instead of trusting the row to carry
        // `None`: one that said "only on the source" while also carrying a
        // destination side — what `CompareRow::sides_are_consistent` calls
        // contradictory — would send the copy to a name nobody paired,
        // inside the approved tree. It is closed off, not trusted.
        let mut paired_dest = dest;
        let kind = if row.verdict == source_orphan {
            paired_dest = None;
            let Some(entry) = source else {
                // The row contradicts itself (see
                // `CompareRow::sides_are_consistent`): it says "only on the
                // source" and does not carry the source entry. There is
                // nothing to copy and no `rel` to compute, so it produces no
                // step. `norte-compare`'s rows are never like this.
                return Ok(());
            };
            if entry.kind == EntryKind::Dir {
                // An orphan directory is ONE `CreateDir`, never a recursive
                // copy: what is inside arrives in its own rows when the
                // comparison was requested with `descend_orphans`, and if it
                // was not, the plan says exactly what it is going to do.
                SyncStepKind::CreateDir
            } else {
                SyncStepKind::Copy
            }
        } else if row.verdict == dest_orphan {
            // What is left over at the destination: under `Update` nothing is
            // deleted — that is the whole reason the mode exists — under
            // `Mirror` it is ONE `DeleteTree`.
            if !mirroring {
                return Ok(());
            }
            return self.absorb_dest_orphan(row, dest);
        } else {
            match row.verdict {
                // A CLASS mismatch with a directory in the middle is not an
                // `Overwrite`: `Overwrite` normatively means "to the trash
                // and COPY bytes", the step carries no `EntryKind` to tell it
                // apart, and the subtree involved is not even in the plan —
                // the walk does not descend into a pair that is not two
                // directories. Turning a tree into a file is a destructive
                // structural change this spec did not promise: a human
                // decides it, and that is why it is a blocker.
                //
                // A file against a symlink IS overwritten: that is replacing
                // bytes, exactly what the step says.
                CompareVerdict::TypeMismatch => {
                    if let Some(side) = directory_side(source, dest) {
                        return self.absorb_type_mismatch_dir(side, source_rel, dest);
                    }
                    SyncStepKind::Overwrite
                }
                // A difference is a difference whatever the confidence:
                // `on_unknown` does not break ties here, it breaks them on
                // `Same` — "looks the same but nobody can promise it" — and
                // not on "it is different".
                CompareVerdict::Different => SyncStepKind::Overwrite,
                // Two sides held to be equal produce NOTHING… except when
                // nobody backs up that equality ([`CompareConfidence::Unknown`]:
                // a provider giving no size or date, a symlink with an
                // unreadable target, a socket). There a choice does have to
                // be made, and it is the user's.
                CompareVerdict::Same => {
                    if row.confidence != CompareConfidence::Unknown {
                        return Ok(());
                    }
                    // Only an EXPLICIT `Copy` writes. `OnUnknown` is
                    // `#[non_exhaustive]`, so the wildcard is mandatory, and
                    // that it falls on the side of touching nothing is
                    // deliberate: a policy this binary does not understand
                    // cannot authorize an overwrite, and the `Skip` is visible
                    // in the plan before it is approved.
                    if matches!(self.opts.on_unknown, OnUnknown::Copy) {
                        SyncStepKind::Overwrite
                    } else {
                        skip_reason = Some(SyncReason::UnknownConfidence);
                        SyncStepKind::Skip
                    }
                }
                // A verdict this decoder does not know (a daemon one version
                // ahead) produces nothing: there is no table to apply to it
                // and guessing would write. `CompareVerdict::Ambiguous` does
                // NOT fall here — `absorb_ambiguous`, above, handles it.
                _ => return Ok(()),
            }
        };

        let (Some(entry), Some(rel)) = (source, source_rel) else {
            // `Different`/`TypeMismatch` with no source side: the same
            // contradiction as above.
            return Ok(());
        };
        if rel.is_root() && kind != SyncStepKind::Skip {
            // The row NAMES the root, not something under it. A step like
            // that acts on the destination's entire tree. A `Skip` CAN name
            // it: it does not act, and saying "I did not touch the root, and
            // why" is informing, not pointing at a target.
            return Err(SyncError::RootIsNotAStep {
                root: Box::new(self.opts.source_root.clone()),
            });
        }
        // The size is copied AS IS: a lazy provider — `file://` among
        // them — lists without it, and a faked zero in the approval dialog
        // is worse than "unknown" (ADR 0048; the counters add it up
        // separately).
        //
        // The guard looks at the ENTRY's class and not the step's: a
        // directory moves no bytes whether the plan calls it `CreateDir` or
        // `Overwrite` (a pair of directories held equal without anyone able
        // to be sure is the latter), and whatever `size` a provider gives a
        // directory is not bytes that are going to be written.
        //
        // A `Skip` does not carry it either, for the same reason: it writes
        // nothing.
        let size = if entry.kind == EntryKind::Dir || kind == SyncStepKind::Skip {
            None
        } else {
            entry.size
        };
        let dest_rel = self.dest_rel_of(&rel, paired_dest)?;
        self.push(row, kind, rel, dest_rel, size, skip_reason, paired_dest);
        Ok(())
    }

    /// A [`CompareVerdict::Error`] row is a [`SyncStepKind::Skip`] that names
    /// what could not be read.
    ///
    /// The reason is ALWAYS [`SyncReason::Unreadable`], and the vocabulary is
    /// closed on purpose: `Unreadable`, `ReadFailed` and `DirTooLarge` are
    /// three ways the walk could not answer for that entry, and none
    /// authorizes writing over it.
    ///
    /// # The one error that is not a `Skip`
    /// A DESTINATION directory above
    /// [`COMPARE_MAX_DIR_ENTRIES`](norte_proto::methods::COMPARE_MAX_DIR_ENTRIES)
    /// is not an entry that gets skipped: it is a piece of the destination
    /// whose content NOBODY has seen, and planning writes inside it is
    /// writing blind. It comes out as
    /// [`SyncBlockerKind::DirTooLarge`](norte_proto::methods::SyncBlockerKind::DirTooLarge),
    /// and the same cap on the SOURCE side is still a `Skip`: not knowing
    /// what is in a source directory only means nothing gets copied from
    /// there.
    ///
    /// `rel` comes from whichever side the row carries: the walk emits the
    /// entry of the side that failed and `None` on the other when it was a
    /// listing, and both when what failed was hydrating an already-paired
    /// pair. A row with neither names nothing and produces no step.
    ///
    /// This `Skip` CAN name the root: it is exactly what comes out when the
    /// walk could not list the comparison's own root, and saying so is much
    /// better than dying or going silent over a whole tree.
    ///
    /// # The `rel` of a row that only has a destination side
    /// It is measured against `dest_root`, the only root it hangs off, and
    /// the step carries nothing that says so: `SyncStep` has no side, and
    /// adding one for two shapes that do not write was not worth it. The
    /// consequence is presentational and has to be known — a panel that
    /// anchors every `rel` to the source side will paint a name there that
    /// does not exist on the source — and it is also written in
    /// [`SyncStep::rel`]'s rustdoc, which is where whoever consumes the plan
    /// will look for it. [`SyncStepKind::DeleteTree`] from `Mirror` follows
    /// the same convention.
    fn absorb_error(
        &mut self,
        row: &CompareRow,
        source: Option<&Entry>,
        dest: Option<&Entry>,
        source_rel: Option<RelPath>,
    ) -> Result<(), SyncError> {
        if row.reason == Some(CompareReason::DirTooLarge)
            && self.speaks_for_the_destination(row, source, dest)
        {
            let rel = match dest {
                Some(entry) => rel_under(&self.opts.dest_root, &entry.path)?,
                None => source_rel.unwrap_or_default(),
            };
            self.push_blocker(rel, SyncBlockerKind::DirTooLarge, Some(Side::Right));
            return Ok(());
        }
        let rel = match (source_rel, dest) {
            (Some(rel), _) => rel,
            (None, Some(entry)) => rel_under(&self.opts.dest_root, &entry.path)?,
            (None, None) => return Ok(()),
        };
        let dest_rel = self.dest_rel_of(&rel, dest)?;
        self.push(
            row,
            SyncStepKind::Skip,
            rel,
            dest_rel,
            None,
            Some(SyncReason::Unreadable),
            dest,
        );
        Ok(())
    }

    /// A [`CompareVerdict::TypeMismatch`] with a DIRECTORY in the middle: a
    /// blocker, not a step.
    ///
    /// `side` names the side that has the directory, and `rel` is measured
    /// against THAT side's root — same as `AmbiguousDest` and `DirTooLarge`
    /// do: if the tree that will not be touched is on the destination, naming
    /// it with the source's spelling would paint a path that does not exist
    /// there, the same hole [`SyncStep::dest_rel`] plugs on steps.
    ///
    /// A `TypeMismatch` ALWAYS carries both entries; the `None`s below are in
    /// case a hand-built row does not carry them, in which case it names
    /// whatever there is. A blocker with nowhere to point still blocks.
    fn absorb_type_mismatch_dir(
        &mut self,
        side: Side,
        source_rel: Option<RelPath>,
        dest: Option<&Entry>,
    ) -> Result<(), SyncError> {
        let dest_rel = match dest {
            Some(entry) => Some(rel_under(&self.opts.dest_root, &entry.path)?),
            None => None,
        };
        let (first, second) = if side == Side::Right {
            (dest_rel, source_rel)
        } else {
            (source_rel, dest_rel)
        };
        let rel = first.or(second).unwrap_or_default();
        self.push_blocker(rel, SyncBlockerKind::TypeMismatchDir, Some(side));
        Ok(())
    }

    /// A [`CompareVerdict::Ambiguous`] row: two names on ONE side that
    /// collapse to the same pairing key.
    ///
    /// The two sides are not the same problem, and only one can lose data:
    ///
    /// - **On the SOURCE** it is not known which of the two files to copy, so
    ///   neither is copied: a [`SyncStepKind::Skip`] with
    ///   [`SyncReason::AmbiguousSource`], and the rest of the plan stands.
    /// - **On the DESTINATION** writing there means writing over one of two
    ///   files without knowing which: a blocker, and the whole plan stops
    ///   being executable.
    ///
    /// This is what keeps two source spellings the destination folds into one
    /// from overwriting each other: without a `Skip` or a blocker the
    /// collision leaves the plan WITHOUT ANYONE SEEING IT, which is exactly
    /// the collision ADR 0048 says a synchronization has to see before
    /// writing anything.
    ///
    /// A row that names no side — or names one this decoder does not know —
    /// is decided by the entry it carries, and the tie falls on the
    /// DESTINATION side: failing toward the blocker costs a plan that has to
    /// be redone, failing toward the `Skip` costs a file.
    fn absorb_ambiguous(
        &mut self,
        row: &CompareRow,
        source: Option<&Entry>,
        dest: Option<&Entry>,
        source_rel: Option<RelPath>,
    ) -> Result<(), SyncError> {
        if self.speaks_for_the_destination(row, source, dest) {
            let rel = match dest {
                Some(entry) => rel_under(&self.opts.dest_root, &entry.path)?,
                None => source_rel.unwrap_or_default(),
            };
            self.push_blocker(rel, SyncBlockerKind::AmbiguousDest, Some(Side::Right));
            return Ok(());
        }
        // With no source entry there is nothing to name, and a `Skip` with no
        // `rel` reports nothing.
        let Some(rel) = source_rel else { return Ok(()) };
        // A collision is on ONE side, so the row carries no pair: `dest_rel`,
        // if it comes out, comes out of the folder that wraps them.
        let dest_rel = self.dest_rel_of(&rel, None)?;
        self.push(
            row,
            SyncStepKind::Skip,
            rel,
            dest_rel,
            None,
            Some(SyncReason::AmbiguousSource),
            None,
        );
        Ok(())
    }

    /// A pair that only holds together through a NON-INJECTIVE
    /// transformation: a [`SyncStepKind::Skip`] that names it (#207, ADR
    /// 0053).
    ///
    /// Mold of [`Self::absorb_ambiguous`] and for the same reason: what fails
    /// is not the verdict but the PAIR, so there is no verdict table to
    /// apply — there is a row to report and a tree that is not touched.
    ///
    /// Unlike a collision, this one has no "side": the transformation joins
    /// one name from EACH side, so the `AmbiguousDest` case that is a blocker
    /// there does not exist here. And `rel` comes from the source when there
    /// is one, which is where every `rel` of a step that speaks of a pair
    /// comes from.
    fn absorb_non_injective(
        &mut self,
        row: &CompareRow,
        source: Option<&Entry>,
        dest: Option<&Entry>,
        source_rel: Option<RelPath>,
    ) -> Result<(), SyncError> {
        // With nothing to name there is no `Skip` to report. With a
        // destination side and no source side — a pair cannot be like that,
        // but the row comes from the wire — the destination is named rather
        // than saying nothing.
        let rel = match (source_rel, dest) {
            (Some(rel), _) => rel,
            (None, Some(entry)) => rel_under(&self.opts.dest_root, &entry.path)?,
            (None, None) => return Ok(()),
        };
        // The second spelling is THE point of the row: the two names are
        // written differently, and whoever reads the plan has to be able to
        // see both.
        let dest_rel = self.dest_rel_of(&rel, dest)?;
        let _ = source;
        self.push(
            row,
            SyncStepKind::Skip,
            rel,
            dest_rel,
            None,
            Some(SyncReason::NonInjectivePairing),
            None,
        );
        Ok(())
    }

    /// A DESTINATION orphan under [`SyncMode::Mirror`]: ONE
    /// [`SyncStepKind::DeleteTree`], and it is not descended into.
    ///
    /// A move to the trash, a journal entry, one thing to restore. Splitting
    /// it into forty thousand steps makes the undo worse and costs forty
    /// thousand listings to learn nothing the plan needs (spec, "Mirror").
    /// The plan relies on the comparison NOT having descended into
    /// destination orphans — `sync.plan` sets `descend_orphans` to the source
    /// side — if someone requested it with the destination descended into,
    /// each child would carry its own `DeleteTree` inside a tree its parent
    /// already deletes.
    ///
    /// `rel` is measured against `dest_root`, same as the `Skip` for an
    /// unreadable destination listing: it is the only root it hangs off.
    /// [`SyncStep::size`] is ABSENT even if the provider gives a size — a
    /// deletion moves no bytes, and
    /// [`SyncCounts::bytes`](norte_proto::methods::SyncCounts::bytes) is the
    /// sum of that field.
    fn absorb_dest_orphan(
        &mut self,
        row: &CompareRow,
        dest: Option<&Entry>,
    ) -> Result<(), SyncError> {
        let Some(entry) = dest else {
            // The row contradicts itself: it says "only on the destination"
            // and does not carry the destination entry.
            return Ok(());
        };
        let rel = rel_under(&self.opts.dest_root, &entry.path)?;
        if rel.is_root() {
            // A `DeleteTree` with an empty `rel` deletes the ENTIRE
            // destination tree.
            return Err(SyncError::RootIsNotAStep {
                root: Box::new(self.opts.dest_root.clone()),
            });
        }
        self.push(
            row,
            SyncStepKind::DeleteTree,
            rel,
            None,
            None,
            None,
            Some(entry),
        );
        Ok(())
    }

    /// Does this row speak for the DESTINATION side?
    ///
    /// [`CompareRow::side`] rules, which is what the walk populates on every
    /// `Ambiguous` row and every `Error` row. When it names no side — or
    /// names a [`Side::Unknown`] that can only come from a newer peer — it is
    /// decided by which entry the row carries, and the tie falls on the
    /// DESTINATION side: the two shapes that ask this (`Ambiguous`,
    /// `DirTooLarge`) block the plan if they are on the destination and only
    /// skip one entry if they are on the source, so failing toward the
    /// destination fails toward the side that does not write.
    fn speaks_for_the_destination(
        &self,
        row: &CompareRow,
        source: Option<&Entry>,
        dest: Option<&Entry>,
    ) -> bool {
        // `source_side` cannot be `Unknown` here anymore: `absorb` ends the
        // plan with `SourceSideUnknown` before getting here.
        let dest_side = match self.opts.source_side {
            Side::Left => Side::Right,
            _ => Side::Left,
        };
        match row.side {
            Some(side) if side == self.opts.source_side => false,
            Some(side) if side == dest_side => true,
            _ => dest.is_some() || source.is_none(),
        }
    }

    /// What `rel` — named on the source — is called on the DESTINATION, when
    /// it is not called the same.
    ///
    /// [`Some`] only when the two relative paths differ BYTE FOR BYTE — done
    /// by `Segment`'s `Eq`, with no `to_str`, no normalizing and no folding
    /// (hard rule 1) — which is the field's normative rule.
    ///
    /// The WHOLE paths are compared, not just the last segment, and that is
    /// more than the field's name suggests: the pairing key folds at EVERY
    /// level, so a `café/x.txt` on the source can hang off an NFD `café` on
    /// the destination even if `x.txt` is spelled the same on both. Pasting
    /// `rel` onto `dest_root` would then name a directory that does not exist
    /// on ext4, exactly as in the loose-name case.
    ///
    /// Without a destination entry the row says nothing… but the STACK does:
    /// what is only on the source inherits the spelling of the paired folder
    /// it hangs off (issue #152). Same data, remembered instead of read.
    ///
    /// The field's normative rule applies in exactly one place, here: `Some`
    /// only when the two paths differ, wherever the destination one came
    /// from.
    fn dest_rel_of(
        &self,
        rel: &RelPath,
        dest: Option<&Entry>,
    ) -> Result<Option<RelPath>, SyncError> {
        let dest_rel = match dest {
            Some(dest) => rel_under(&self.opts.dest_root, &dest.path)?,
            None => match self.spelt_at_the_destination(rel) {
                Some(dest_rel) => dest_rel,
                None => return Ok(None),
            },
        };
        Ok((dest_rel != *rel).then_some(dest_rel))
    }

    /// How `rel` is written at the destination according to the DEEPEST
    /// paired folder it hangs off, when that folder is spelled differently.
    ///
    /// Ancestors are tried from the inside out and it stops at the first one
    /// in the map: its value is the WHOLE destination path, so it already
    /// carries the translation of all its ancestors and nothing needs to be
    /// composed. The comparison is [`RelPath`]'s `Eq`/`Ord`, which is
    /// [`Segment`]'s, which are BYTES (hard rule 1): `café` and `cafétière`
    /// are two different keys no matter that one starts with the other's
    /// bytes.
    ///
    /// `rel` itself is NOT tried: the row of a paired directory carries its
    /// pair and needs nobody to remind it.
    ///
    /// When not a single folder differs — the common case, and the only one
    /// in a homogeneous tree — this is a comparison and nothing more.
    fn spelt_at_the_destination(&self, rel: &RelPath) -> Option<RelPath> {
        if self.spellings.is_empty() {
            return None;
        }
        let segments = rel.segments();
        for split in (1..segments.len()).rev() {
            let ancestor = RelPath::new(segments[..split].to_vec());
            if let Some(dest_dir) = self.spellings.get(&ancestor) {
                let mut translated = dest_dir.segments().to_vec();
                translated.extend_from_slice(&segments[split..]);
                return Some(RelPath::new(translated));
            }
        }
        None
    }

    /// Records this row's folder if it is a pair of directories the two sides
    /// spell DIFFERENTLY.
    ///
    /// Called on every row that has a source side, including ones that
    /// produce no step: the row of a pair of identical directories is
    /// `Same`, and it is precisely the one that knows both spellings.
    ///
    /// A record's two keys are always valid UTF-8, and not by accident:
    /// `norte-compare` does not fold a name that is not one (its key comes
    /// out raw), so a non-UTF-8 name only pairs with another byte-identical
    /// one — and then there is nothing to record. What CAN be non-UTF-8 is
    /// whatever hangs off the folder: the suffix is copied byte for byte.
    fn remember_spelling(
        &mut self,
        source_rel: &RelPath,
        source: Option<&Entry>,
        dest: Option<&Entry>,
    ) -> Result<(), SyncError> {
        let (Some(source), Some(dest)) = (source, dest) else {
            return Ok(());
        };
        if source.kind != EntryKind::Dir || dest.kind != EntryKind::Dir {
            return Ok(());
        }
        let dest_rel = rel_under(&self.opts.dest_root, &dest.path)?;
        if dest_rel != *source_rel {
            self.spellings.insert(source_rel.clone(), dest_rel);
        }
        Ok(())
    }

    /// Does this row fall inside the subtree an overlap already pruned?
    ///
    /// ONLY the side that reached the other root is looked at. The other one
    /// hangs off it whole — if the source reached `dest_root`, every
    /// destination path is under `dest_root` by definition — so looking at
    /// it too would prune the whole plan instead of the subtree.
    fn overlap_prunes(&self, source: Option<&Entry>, dest: Option<&Entry>) -> bool {
        let Some(overlap) = self.overlap.as_ref() else {
            return false;
        };
        let side = if overlap.from_source { source } else { dest };
        side.is_some_and(|entry| is_at_or_under(&overlap.prefix, &entry.path))
    }

    /// Has the walk reached the OTHER root?
    ///
    /// A source path that is at or under `dest_root` — or a destination one
    /// that is at or under `source_root` — means the two roots name the same
    /// tree and that copying from one to the other would copy a subtree
    /// inside itself. ONE blocker comes out and it is pruned: what is
    /// remembered is not this row's path but the ROOT reached, which is what
    /// contains the whole overlapping subtree, so a single prefix covers
    /// everything that comes after.
    ///
    /// Two EQUAL roots do not count: see [`plan`]'s rustdoc.
    fn overlap_reached(
        &mut self,
        source: Option<&Entry>,
        dest: Option<&Entry>,
    ) -> Result<Option<SyncBlocker>, SyncError> {
        if self.overlap.is_some() {
            // ONCE. With the two roots nested, checking the other side is
            // true for EVERY row — if the destination is inside the source,
            // every destination path is under the source's root — so
            // checking it again would raise a blocker per row and move the
            // pruning to a root that swallows the whole plan. One is enough:
            // the plan is already not executable.
            return Ok(None);
        }
        if self.opts.source_root == self.opts.dest_root {
            return Ok(None);
        }
        let reached = source
            .filter(|entry| is_at_or_under(&self.opts.dest_root, &entry.path))
            .map(|entry| (entry, &self.opts.source_root, &self.opts.dest_root, true))
            .or_else(|| {
                dest.filter(|entry| is_at_or_under(&self.opts.source_root, &entry.path))
                    .map(|entry| (entry, &self.opts.dest_root, &self.opts.source_root, false))
            });
        let Some((entry, walked_root, other_root, from_source)) = reached else {
            return Ok(None);
        };
        // The blocker's `rel` is measured against the root the walk was
        // going along, which is where the panel is going to paint it.
        let rel = rel_under(walked_root, &entry.path)?;
        self.overlap = Some(Overlap {
            prefix: other_root.clone(),
            from_source,
        });
        Ok(Some(SyncBlocker {
            rel,
            kind: SyncBlockerKind::OverlapDetected,
            // The overlap belongs to BOTH roots at once: there is no side to
            // name, and it is omitted instead of inventing one.
            side: None,
        }))
    }

    /// Enqueues a blocker. It carries no `id`: a blocker is not a step, and
    /// nothing executes or enumerates it.
    fn push_blocker(&mut self, rel: RelPath, kind: SyncBlockerKind, side: Option<Side>) {
        self.pending
            .push_back(PlanItem::Blocker(SyncBlocker { rel, kind, side }));
    }

    /// Packages the step and gives it its `id`. The reversal is a function of
    /// the class and the trash, except on a `Skip`, which has none and owes a
    /// reason.
    ///
    /// `dest` is the DESTINATION entry the row carried, when it carried one.
    /// The [`DestWitness`] comes from it, and only for the two classes that
    /// are going to destroy it: whoever does not destroy has nothing to
    /// revalidate, and a witness per step in a half-million plan is a spool
    /// file nobody reads.
    // Eight arguments because a step has eight things to say. Grouping them
    // into an intermediate struct would only move where a field gets
    // mismatched, and this is private to the module: not API anyone else
    // will call.
    #[expect(
        clippy::too_many_arguments,
        reason = "private to the module, not API: grouping into a struct would only move the fields"
    )]
    fn push(
        &mut self,
        row: &CompareRow,
        kind: SyncStepKind,
        rel: RelPath,
        dest_rel: Option<RelPath>,
        size: Option<u64>,
        skip_reason: Option<SyncReason>,
        dest: Option<&Entry>,
    ) {
        let (reversal, reason) = match skip_reason {
            Some(why) => (None, Some(why)),
            None => reversal_for(
                kind,
                self.opts.dest_has_trash,
                self.opts.dest_trash_restorable,
            ),
        };
        let step = SyncStep {
            id: self.next_id,
            kind,
            rel,
            dest_rel,
            size,
            criterion: row.criterion,
            confidence: row.confidence,
            reversal,
            reason,
        };
        debug_assert!(
            step.shape_is_consistent(),
            "step with an impossible shape: {step:?}"
        );
        let dest = match kind {
            SyncStepKind::Overwrite | SyncStepKind::DeleteTree => dest.map(DestWitness::of),
            _ => None,
        };
        self.pending.push_back(PlanItem::Step { step, dest });
        self.next_id += 1;
    }
}

/// How a step is undone, as a function of its class and of the DESTINATION's
/// trash.
///
/// A `CreateDir` and a `Copy` destroy nothing, so they are undone by deleting
/// what they created, trash or no trash. An `Overwrite` and a `DeleteTree`
/// bury something: with a trash it is taken out of it, without a trash it is
/// taken out of nowhere and the plan has to say so BEFORE anyone approves it
/// (hard rule 4).
///
/// # And a trash that does not say where it put things is USELESS
/// When the destination has a trash but does not name it
/// ([`SyncOptions::dest_trash_restorable`](crate::SyncOptions::dest_trash_restorable)
/// at `false`), the undo is left without a `reversal_ref` and cannot get it
/// right: it neither unearths what was overwritten — it would match by
/// original path and pull out the file it just buried — nor undoes a
/// creation, because undoing it means burying it and that goes through the
/// same trash (#65). So **every** step comes out `Irreversible`, not just the
/// destructive ones.
///
/// What this case does NOT change is a destination WITHOUT a trash, where a
/// `Copy` still announces itself as reversible: there the undo skips the
/// entry for a different reason (deleting "whatever lives at that path today"
/// with no trash can destroy the human's later work) and counts it in
/// `skipped_created_no_trash`. That asymmetry is a decision of task 11 of the
/// plan, not an oversight of this one.
fn reversal_for(
    kind: SyncStepKind,
    dest_has_trash: bool,
    dest_trash_restorable: bool,
) -> (Option<StepReversal>, Option<SyncReason>) {
    let acts = matches!(
        kind,
        SyncStepKind::CreateDir
            | SyncStepKind::Copy
            | SyncStepKind::Overwrite
            | SyncStepKind::DeleteTree
    );
    if acts && dest_has_trash && !dest_trash_restorable {
        return (
            Some(StepReversal::Irreversible),
            Some(SyncReason::NoTrashOnTarget),
        );
    }
    match kind {
        SyncStepKind::CreateDir | SyncStepKind::Copy => (Some(StepReversal::Delete), None),
        SyncStepKind::Overwrite | SyncStepKind::DeleteTree => {
            if dest_has_trash {
                (Some(StepReversal::RestoreTrash), None)
            } else {
                (
                    Some(StepReversal::Irreversible),
                    Some(SyncReason::NoTrashOnTarget),
                )
            }
        }
        // A `Skip` has no reversal (it did nothing) and its reason is set by
        // whoever emits it, who is the only one who knows it. This crate
        // never emits `Unknown`.
        _ => (None, None),
    }
}

/// Which of the two sides of a [`CompareVerdict::TypeMismatch`] is the
/// DIRECTORY, under a PLAN's side convention: [`Side::Left`] the source,
/// [`Side::Right`] the destination (the same one
/// [`SyncBlocker::side`](norte_proto::methods::SyncBlocker::side) uses, and
/// NOT the comparison's side, which depends on which pane the user was in).
///
/// `None` when there is neither: a file against a symlink is a byte
/// substitution and gets overwritten.
///
/// The two cannot both be it at once — two directories do not mismatch in
/// class — so the `or`'s order hides nothing; if a hand-built row said
/// otherwise, the source wins and a blocker comes out just the same.
fn directory_side(source: Option<&Entry>, dest: Option<&Entry>) -> Option<Side> {
    if source.is_some_and(|entry| entry.kind == EntryKind::Dir) {
        Some(Side::Left)
    } else if dest.is_some_and(|entry| entry.kind == EntryKind::Dir) {
        Some(Side::Right)
    } else {
        None
    }
}

/// Is `path` AT `root` or below it?
///
/// Delegated to [`RelPath::under`], same as [`rel_under`] and for the same
/// reason — it has been the only implementation since #172. The root itself
/// counts as contained, which is what this module's two callers need: if the
/// walk reached the other root itself, the whole subtree is inside just the
/// same (see [`Transducer::overlap_reached`] and
/// [`Transducer::overlap_prunes`]).
fn is_at_or_under(root: &VPath, path: &VPath) -> bool {
    RelPath::under(root, path).is_some()
}

/// `path`'s path RELATIVE to `root`, or the error saying it does not hang off
/// it.
///
/// The comparison — scheme, authority and segments by their raw bytes — lives
/// in [`RelPath::under`], which belongs to `norte-proto` because that is
/// where the three types it touches live and because the answer has to be
/// ONE: the core's `include` filter and the frontend that assembles the
/// request ask the same thing, and two differing implementations would send
/// an `include` that does not select what the reader marked. This just gives
/// it this crate's error.
///
/// What it CAN return is the root itself (`path == root`), which is not an
/// upward escape but is the plan's most destructive target: whoever calls it
/// rejects it, since they are the only one who knows whether an empty `rel`
/// makes sense (a `Skip` does, an `Overwrite` does not).
///
/// # Errors
/// [`SyncError::OutsideRoot`] when `path` does not hang off `root` — another
/// scheme, another authority, or another branch.
///
/// ```
/// use norte_proto::VPath;
/// use norte_sync::rel_under;
/// let root = VPath::parse("file:///origen").expect("root");
/// let path = VPath::parse("file:///origen/sub/a.txt").expect("path");
/// assert_eq!(rel_under(&root, &path).expect("rel").to_wire(), "sub/a.txt");
/// // By SEGMENTS, not by string prefix: `…/ab` does not hang off `…/a`.
/// let other = VPath::parse("file:///origenes/a.txt").expect("path");
/// assert!(rel_under(&root, &other).is_err());
/// ```
pub fn rel_under(root: &VPath, path: &VPath) -> Result<RelPath, SyncError> {
    RelPath::under(root, path).ok_or_else(|| SyncError::OutsideRoot {
        root: Box::new(root.clone()),
        path: Box::new(path.clone()),
    })
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    // The module no longer uses `Segment`: `rel_under` delegates the
    // comparison to `RelPath::under`. The tests do, to build paths.
    use norte_proto::Segment;
    use norte_proto::methods::{CompareCriterion, CompareReason};

    use super::*;

    fn vpath(wire: &str) -> VPath {
        VPath::parse(wire).expect("path")
    }

    fn source_root() -> VPath {
        vpath("file:///origen")
    }

    fn dest_root() -> VPath {
        vpath("file:///destino")
    }

    fn opts_update() -> SyncOptions {
        SyncOptions {
            source_root: source_root(),
            dest_root: dest_root(),
            mode: SyncMode::Update,
            on_unknown: OnUnknown::Copy,
            source_side: Side::Left,
            dest_has_trash: true,
            dest_trash_restorable: true,
            dest_writable: true,
        }
    }

    fn opts_mirror() -> SyncOptions {
        SyncOptions {
            mode: SyncMode::Mirror,
            ..opts_update()
        }
    }

    fn entry_at(root: &VPath, name: &[u8], kind: EntryKind, size: Option<u64>) -> Entry {
        Entry {
            path: root.join(Segment::new(name).expect("segment")),
            kind,
            size,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        }
    }

    fn src_file(name: &str, size: u64) -> Entry {
        entry_at(&source_root(), name.as_bytes(), EntryKind::File, Some(size))
    }

    fn dst_file(name: &str, size: u64) -> Entry {
        entry_at(&dest_root(), name.as_bytes(), EntryKind::File, Some(size))
    }

    fn src_dir(name: &str) -> Entry {
        entry_at(&source_root(), name.as_bytes(), EntryKind::Dir, None)
    }

    fn dst_dir(name: &str) -> Entry {
        entry_at(&dest_root(), name.as_bytes(), EntryKind::Dir, None)
    }

    fn dst_link(name: &str) -> Entry {
        entry_at(&dest_root(), name.as_bytes(), EntryKind::Symlink, None)
    }

    /// An entry at an ARBITRARY path, for the overlap tests: there the point
    /// is exactly that the path does not hang off where it should.
    fn entry_wire(wire: &str, kind: EntryKind, size: Option<u64>) -> Entry {
        Entry {
            path: vpath(wire),
            kind,
            size,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        }
    }

    fn row(
        verdict: CompareVerdict,
        criterion: CompareCriterion,
        confidence: CompareConfidence,
        left: Option<Entry>,
        right: Option<Entry>,
    ) -> CompareRow {
        CompareRow {
            id: 0,
            left,
            right,
            verdict,
            criterion,
            confidence,
            newer: None,
            reason: None,
            side: None,
            paired_under: None,
        }
    }

    fn rel(wire: &str) -> RelPath {
        RelPath::parse_wire(wire).expect("rel")
    }

    /// Each segment's BYTES. The wire form is no good for comparing two
    /// spellings: NFC and NFD are both valid UTF-8, so the codec leaves them
    /// as is and both strings render the SAME (hard rule 1 — bytes are
    /// compared).
    fn bytes_of(rel: &RelPath) -> Vec<&[u8]> {
        rel.segments().iter().map(Segment::as_bytes).collect()
    }

    async fn run_raw(
        rows: Vec<CompareRow>,
        opts: SyncOptions,
        cancel: CancellationToken,
    ) -> Vec<Result<PlanItem, SyncError>> {
        plan(stream::iter(rows.into_iter().map(Ok)), opts, cancel)
            .collect()
            .await
    }

    async fn run(rows: Vec<CompareRow>, opts: SyncOptions) -> Vec<PlanItem> {
        run_raw(rows, opts, CancellationToken::new())
            .await
            .into_iter()
            .map(|r| r.expect("no error"))
            .collect()
    }

    fn steps_of(items: &[PlanItem]) -> Vec<&SyncStep> {
        items
            .iter()
            .filter_map(|i| match i {
                PlanItem::Step { step, .. } => Some(step),
                PlanItem::Blocker(_) => None,
            })
            .collect()
    }

    fn one_step(items: &[PlanItem]) -> &SyncStep {
        let steps = steps_of(items);
        assert_eq!(steps.len(), 1, "expected ONE step: {items:?}");
        steps[0]
    }

    /// (`rel`, `dest_rel`) of a step, in BYTES.
    type Spellings<'a> = (Vec<&'a [u8]>, Option<Vec<&'a [u8]>>);

    fn blockers_of(items: &[PlanItem]) -> Vec<&SyncBlocker> {
        items
            .iter()
            .filter_map(|i| match i {
                PlanItem::Blocker(b) => Some(b),
                PlanItem::Step { .. } => None,
            })
            .collect()
    }

    fn one_blocker(items: &[PlanItem]) -> &SyncBlocker {
        let blockers = blockers_of(items);
        assert_eq!(blockers.len(), 1, "expected ONE blocker: {items:?}");
        blockers[0]
    }

    /// #207 (ADR 0053): a pair that only holds together through a
    /// NON-INJECTIVE transformation is not overwritten.
    ///
    /// `K.txt` with U+212A KELVIN SIGN against `K.txt` with the ASCII `K`:
    /// Unicode declares them canonically equivalent, ext4 stores them as TWO
    /// files. Without this, a plain `Different` used to come out as
    /// `Overwrite` and write one's bytes over the other's — #152's data loss,
    /// with the data already on the wire since 0.42.0 and nobody reading it.
    #[tokio::test]
    async fn a_non_injective_pair_is_not_overwritten() {
        use norte_proto::methods::PairTransform;

        let mut row_ = row(
            CompareVerdict::Different,
            CompareCriterion::Size,
            CompareConfidence::Certain,
            Some(src_file("K.txt", 10)),
            Some(dst_file("K.txt", 20)),
        );
        row_.paired_under = Some(PairTransform::NormalizationSingleton);
        let items = run(vec![row_], opts_update()).await;
        let steps = steps_of(&items);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].kind, SyncStepKind::Skip, "never an Overwrite");
        assert_eq!(steps[0].reason, Some(SyncReason::NonInjectivePairing));
    }

    /// And a transformation this binary does NOT know falls on the same
    /// side: the criterion is `names_one_text()`, not the variant. A daemon
    /// one version ahead cannot authorize an overwrite by omission.
    #[tokio::test]
    async fn an_unknown_transformation_does_not_act_either() {
        use norte_proto::methods::PairTransform;

        // `#[serde(other)]`: the variant this binary uses for "I don't know
        // it". Built through the same path it would arrive by from the
        // wire — the enum's `Unknown`.
        let unknown = PairTransform::Unknown;
        assert!(!unknown.names_one_text());
        let mut row_ = row(
            CompareVerdict::Different,
            CompareCriterion::Size,
            CompareConfidence::Certain,
            Some(src_file("x.txt", 10)),
            Some(dst_file("x.txt", 20)),
        );
        row_.paired_under = Some(unknown);
        let items = run(vec![row_], opts_update()).await;
        assert_eq!(steps_of(&items)[0].kind, SyncStepKind::Skip);
    }

    /// The COMMON ones keep acting: `CaseFold` and `Normalization` are the
    /// pairs for which the pairing key exists, and denying them would break
    /// the macOS↔Linux case they serve.
    #[tokio::test]
    async fn an_nfc_nfd_pair_still_overwrites() {
        use norte_proto::methods::PairTransform;

        for transform in [PairTransform::Normalization, PairTransform::CaseFold] {
            let mut row_ = row(
                CompareVerdict::Different,
                CompareCriterion::Size,
                CompareConfidence::Certain,
                Some(src_file("cafe.txt", 10)),
                Some(dst_file("cafe.txt", 20)),
            );
            row_.paired_under = Some(transform);
            let items = run(vec![row_], opts_update()).await;
            assert_eq!(
                steps_of(&items)[0].kind,
                SyncStepKind::Overwrite,
                "{transform:?} names ONE text and does act"
            );
        }
    }

    #[tokio::test]
    async fn only_on_the_source_becomes_a_copy() {
        let items = run(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(src_file("a.txt", 10)),
                None,
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Copy);
        assert_eq!(s.rel, rel("a.txt"));
        assert_eq!(s.size, Some(10));
        assert_eq!(s.reversal, Some(StepReversal::Delete));
        assert_eq!(s.reason, None);
        assert_eq!(s.criterion, CompareCriterion::Presence);
        assert!(s.shape_is_consistent());
    }

    #[tokio::test]
    async fn a_directory_only_on_the_source_becomes_create_dir() {
        let items = run(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(src_dir("sub")),
                None,
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::CreateDir);
        assert_eq!(s.rel, rel("sub"));
        assert_eq!(s.reversal, Some(StepReversal::Delete));
        assert_eq!(s.size, None, "creating a directory moves no bytes");
    }

    #[tokio::test]
    async fn a_descended_orphan_is_one_step_per_row_and_never_a_recursive_copy() {
        // What task 2 makes possible: the container first and its children
        // after, each in its own row. The plan does not "optimize" the
        // directory into a recursive copy — the executor journals and
        // isolates failures PER STEP.
        let deep = Entry {
            path: source_root()
                .join(Segment::new(b"sub".to_vec()).expect("segment"))
                .join(Segment::new(b"1.txt".to_vec()).expect("segment")),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        };
        let items = run(
            vec![
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(src_dir("sub")),
                    None,
                ),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(deep),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        let steps = steps_of(&items);
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].kind, SyncStepKind::CreateDir);
        assert_eq!(steps[1].kind, SyncStepKind::Copy);
        assert_eq!(
            steps[1].rel,
            rel("sub/1.txt"),
            "the rel carries both levels"
        );
        assert_eq!(
            steps[1].size, None,
            "an orphan row is not hydrated: `None` travels as `None`, never as 0"
        );
        assert_eq!(steps[0].id, 0);
        assert_eq!(steps[1].id, 1, "ids are monotonic within the plan");
    }

    #[tokio::test]
    async fn different_becomes_overwrite_and_keeps_the_criterion_that_decided_it() {
        let items = run(
            vec![row(
                CompareVerdict::Different,
                CompareCriterion::Mtime,
                CompareConfidence::Probable,
                Some(src_file("a.txt", 10)),
                Some(dst_file("a.txt", 9)),
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Overwrite);
        assert_eq!(s.criterion, CompareCriterion::Mtime);
        assert_eq!(
            s.confidence,
            CompareConfidence::Probable,
            "the report has to be able to say WHY it overwrote"
        );
        assert_eq!(s.size, Some(10), "the bytes are the SOURCE's");
        assert_eq!(s.reversal, Some(StepReversal::RestoreTrash));
        assert_eq!(s.reason, None, "a reversible step has nothing to justify");
        assert_eq!(s.dest_rel, None, "the destination is named the same");
        assert!(s.shape_is_consistent());
    }

    #[tokio::test]
    async fn an_overwrite_without_a_trash_is_irreversible_and_says_why() {
        let opts = SyncOptions {
            dest_has_trash: false,
            ..opts_update()
        };
        let items = run(
            vec![row(
                CompareVerdict::Different,
                CompareCriterion::Size,
                CompareConfidence::Certain,
                Some(src_file("a.txt", 10)),
                Some(dst_file("a.txt", 9)),
            )],
            opts,
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.reversal, Some(StepReversal::Irreversible));
        assert_eq!(s.reason, Some(SyncReason::NoTrashOnTarget));
        assert!(s.shape_is_consistent());
    }

    /// Four rows that produce the four step classes that ACT. This is the
    /// input for the reversal tests: the whole matrix in one call.
    fn one_row_of_each_class() -> Vec<CompareRow> {
        vec![
            // Copy: only on the source.
            row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(src_file("nuevo.txt", 10)),
                None,
            ),
            // CreateDir: a directory only on the source.
            row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(src_dir("nueva")),
                None,
            ),
            // Overwrite: different on both sides.
            row(
                CompareVerdict::Different,
                CompareCriterion::Size,
                CompareConfidence::Certain,
                Some(src_file("a.txt", 10)),
                Some(dst_file("a.txt", 9)),
            ),
            // DeleteTree (Mirror only): a destination orphan.
            row(
                CompareVerdict::OnlyRight,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                None,
                Some(dst_file("sobra.txt", 1)),
            ),
        ]
    }

    /// **A trash that does not say where it put things undoes NOTHING.**
    /// Neither the overwrite (the undo would pull out the file it just
    /// buried) nor the copy (undoing it means burying it, and that goes
    /// through the same trash, #65). This is what used to happen to
    /// `file://` on Linux before the freedesktop trash named its
    /// destination.
    #[tokio::test]
    async fn nothing_is_reversible_when_the_destination_cannot_restore() {
        let opts = SyncOptions {
            dest_has_trash: true,
            dest_trash_restorable: false,
            ..opts_mirror()
        };
        let items = run(one_row_of_each_class(), opts).await;
        let steps = steps_of(&items);
        assert!(steps.len() >= 4, "all four classes: {items:?}");
        for s in steps {
            if s.kind == SyncStepKind::Skip {
                continue;
            }
            assert_eq!(s.reversal, Some(StepReversal::Irreversible), "{s:?}");
            assert_eq!(s.reason, Some(SyncReason::NoTrashOnTarget), "{s:?}");
            assert!(s.shape_is_consistent(), "{s:?}");
        }
    }

    /// And with a trash that DOES name its destination, the reversal table
    /// is the usual one: nothing extra is marked irreversible.
    #[tokio::test]
    async fn a_restorable_trash_keeps_the_promises_the_table_makes() {
        let opts = SyncOptions {
            dest_has_trash: true,
            dest_trash_restorable: true,
            ..opts_mirror()
        };
        let items = run(one_row_of_each_class(), opts).await;
        let steps = steps_of(&items);
        assert!(
            steps
                .iter()
                .any(|s| s.reversal == Some(StepReversal::RestoreTrash))
        );
        assert!(
            steps
                .iter()
                .any(|s| s.reversal == Some(StepReversal::Delete))
        );
        assert!(
            !steps
                .iter()
                .any(|s| s.reversal == Some(StepReversal::Irreversible)),
            "{items:?}"
        );
    }

    #[tokio::test]
    async fn a_copy_is_reversible_even_without_a_trash() {
        let opts = SyncOptions {
            dest_has_trash: false,
            ..opts_update()
        };
        let items = run(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(src_file("a.txt", 10)),
                None,
            )],
            opts,
        )
        .await;
        // Nothing was destroyed: undoing means deleting what was created.
        assert_eq!(one_step(&items).reversal, Some(StepReversal::Delete));
    }

    #[tokio::test]
    async fn same_produces_nothing_at_all() {
        let items = run(
            vec![row(
                CompareVerdict::Same,
                CompareCriterion::Hash,
                CompareConfidence::Certain,
                Some(src_file("a.txt", 10)),
                Some(dst_file("a.txt", 10)),
            )],
            opts_update(),
        )
        .await;
        assert!(
            items.is_empty(),
            "an identical tree cannot produce a million no-ops"
        );
    }

    #[tokio::test]
    async fn only_on_the_destination_produces_nothing_under_update() {
        let items = run(
            vec![row(
                CompareVerdict::OnlyRight,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                None,
                Some(dst_file("gone.txt", 3)),
            )],
            opts_update(),
        )
        .await;
        assert!(items.is_empty(), "`Update` never deletes");
    }

    #[tokio::test]
    async fn a_type_mismatch_between_a_file_and_a_symlink_overwrites_and_says_so() {
        // Replacing a symlink with a file (or the other way around) is
        // replacing bytes, exactly what `Overwrite` means. With a DIRECTORY
        // in the middle it is not, and that is a blocker (below).
        let items = run(
            vec![row(
                CompareVerdict::TypeMismatch,
                CompareCriterion::Kind,
                CompareConfidence::Certain,
                Some(src_file("x", 1)),
                Some(dst_link("x")),
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Overwrite);
        assert_eq!(s.criterion, CompareCriterion::Kind);
        assert!(blockers_of(&items).is_empty());
    }

    #[tokio::test]
    async fn the_source_can_be_the_right_side() {
        // The frontend translated the direction ONCE; here it is a fact, and
        // the mirror image has to give the same plan with the sides swapped.
        let opts = SyncOptions {
            source_root: dest_root(),
            dest_root: source_root(),
            source_side: Side::Right,
            ..opts_update()
        };
        let items = run(
            vec![
                row(
                    CompareVerdict::OnlyRight,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    None,
                    Some(dst_file("a.txt", 10)),
                ),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(src_file("b.txt", 3)),
                    None,
                ),
            ],
            opts,
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Copy);
        assert_eq!(s.rel, rel("a.txt"));
    }

    #[tokio::test]
    async fn rel_is_relative_to_the_roots_and_keeps_every_byte() {
        // The WHOLE hostile corpus, not a sample name: if deriving `rel`
        // loses or translates a byte, it shows here (hard rule 1).
        for name in norte_testkit::corpus::hostile_names() {
            let entry = entry_at(&source_root(), &name.bytes, EntryKind::File, Some(1));
            let items = run(
                vec![row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(entry),
                    None,
                )],
                opts_update(),
            )
            .await;
            let s = one_step(&items);
            assert_eq!(s.rel.segments().len(), 1, "{}", name.id);
            assert_eq!(
                s.rel.segments()[0].as_bytes(),
                name.bytes.as_slice(),
                "{} lost bytes turning relative",
                name.id
            );
            // And it is still a legal wire `rel`, round trip.
            let back = RelPath::parse_wire(&s.rel.to_wire()).expect("wire");
            assert_eq!(back, s.rel, "{}", name.id);
        }
    }

    #[tokio::test]
    async fn the_same_hostile_name_on_both_sides_never_invents_a_second_spelling() {
        // The WHOLE hostile corpus again, now paired with itself: if the
        // comparison of the two relative paths stopped being byte for byte —
        // a normalization, a fold, one `to_str` too many — one of the 47
        // would come out with a populated `dest_rel` and the executor would
        // write somewhere else for a name that is THE SAME.
        for name in norte_testkit::corpus::hostile_names() {
            let items = run(
                vec![row(
                    CompareVerdict::Different,
                    CompareCriterion::Size,
                    CompareConfidence::Certain,
                    Some(entry_at(
                        &source_root(),
                        &name.bytes,
                        EntryKind::File,
                        Some(2),
                    )),
                    Some(entry_at(
                        &dest_root(),
                        &name.bytes,
                        EntryKind::File,
                        Some(1),
                    )),
                )],
                opts_update(),
            )
            .await;
            assert_eq!(one_step(&items).dest_rel, None, "{}", name.id);
        }
    }

    #[tokio::test]
    async fn a_root_whose_name_is_a_prefix_of_another_is_not_a_root() {
        // `…/origen2/x` does NOT hang off `…/source`: compared by SEGMENTS,
        // not by string prefix.
        let intruder = entry_at(
            &vpath("file:///origen2"),
            b"x.txt",
            EntryKind::File,
            Some(1),
        );
        let out = run_raw(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(intruder),
                None,
            )],
            opts_update(),
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(
            out.as_slice(),
            [Err(SyncError::OutsideRoot { .. })]
        ));
    }

    #[tokio::test]
    async fn a_path_from_another_provider_is_not_under_the_root() {
        let foreign = entry_at(&vpath("sftp://nas/origen"), b"x.txt", EntryKind::File, None);
        let out = run_raw(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(foreign),
                None,
            )],
            opts_update(),
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(
            out.as_slice(),
            [Err(SyncError::OutsideRoot { .. })]
        ));
    }

    #[tokio::test]
    async fn a_row_that_contradicts_its_own_verdict_produces_nothing() {
        // "Only on the source" with no source entry: there is nothing to
        // copy and nowhere to get a `rel` from. No `norte-compare` row is
        // like this.
        let items = run(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                None,
                None,
            )],
            opts_update(),
        )
        .await;
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn a_wiring_error_ends_the_plan_even_with_no_rows_at_all() {
        // The two wiring failures are decided at construction, not when
        // absorbing the first row. If they were decided per row, two empty
        // trees — or a comparison that produced none — would give an EMPTY
        // and approvable plan for a mode this binary does not know how to
        // plan: exactly the silent failure the two variants exist to avoid.
        let no_side = SyncOptions {
            source_side: Side::Unknown,
            ..opts_update()
        };
        assert_eq!(
            run_raw(vec![], no_side, CancellationToken::new()).await,
            vec![Err(SyncError::SourceSideUnknown)]
        );
    }

    #[tokio::test]
    async fn a_read_only_destination_does_not_hide_a_wiring_error() {
        // An immutable destination is no excuse to swallow bad wiring: the
        // tree's blocker never comes out, because there is no plan to block.
        let opts = SyncOptions {
            source_side: Side::Unknown,
            dest_writable: false,
            ..opts_update()
        };
        assert_eq!(
            run_raw(vec![], opts, CancellationToken::new()).await,
            vec![Err(SyncError::SourceSideUnknown)]
        );
    }

    #[tokio::test]
    async fn a_source_side_that_names_no_side_ends_the_plan() {
        let opts = SyncOptions {
            source_side: Side::Unknown,
            ..opts_update()
        };
        let out = run_raw(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(src_file("a.txt", 1)),
                None,
            )],
            opts,
            CancellationToken::new(),
        )
        .await;
        assert_eq!(out, vec![Err(SyncError::SourceSideUnknown)]);
    }

    #[tokio::test]
    async fn cancellation_ends_the_stream_with_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let rows: Vec<_> = (0..64)
            .map(|_| {
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(src_file("a.txt", 1)),
                    None,
                )
            })
            .collect();
        let out = run_raw(rows, opts_update(), cancel).await;
        assert_eq!(
            out,
            vec![Err(SyncError::Cancelled)],
            "it cuts at the next row, not at the end"
        );
    }

    #[tokio::test]
    async fn a_cancelled_row_stream_cancels_the_plan() {
        // The cancellation can come from upstream: the walk saw it first.
        let out: Vec<_> = plan(
            stream::iter(vec![
                Ok(row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(src_file("a.txt", 1)),
                    None,
                )),
                Err(CompareError::Cancelled),
            ]),
            opts_update(),
            CancellationToken::new(),
        )
        .collect()
        .await;
        assert_eq!(out.len(), 2);
        assert!(out[0].is_ok());
        assert_eq!(out[1], Err(SyncError::Cancelled));
    }

    #[tokio::test]
    async fn polling_past_the_end_gives_none_instead_of_panicking() {
        // `futures`'s raw `Unfold` PANICS if polled after `None`, and any
        // loop with a `select!` and a flush tick does that. `plan()`'s
        // `.fuse()` is what prevents it.
        let mut s = Box::pin(plan(
            stream::iter(Vec::new()),
            opts_update(),
            CancellationToken::new(),
        ));
        assert!(s.next().await.is_none());
        assert!(s.next().await.is_none(), "and again, no panic");
        assert!(s.is_terminated());
    }

    #[test]
    fn the_plan_stream_is_send_so_a_task_can_own_it() {
        // Relies on `Send` leaking through the `impl Trait`. An `Rc` or a
        // `RefCell` inside `Transducer` would compile here and break at a
        // distance, in task 8, with an unreadable error.
        fn assert_send<T: Send>(_: &T) {}
        let s = plan(
            stream::iter(Vec::new()),
            opts_update(),
            CancellationToken::new(),
        );
        assert_send(&s);
    }

    #[tokio::test]
    async fn a_row_that_names_the_root_itself_is_not_a_step() {
        // Comes from a caller whose roots are deeper than the comparison's. A
        // step with an empty `rel` acts on the WHOLE tree.
        let root = Entry {
            path: source_root(),
            kind: EntryKind::Dir,
            size: None,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        };
        let out = run_raw(
            vec![row(
                CompareVerdict::Different,
                CompareCriterion::Mtime,
                CompareConfidence::Probable,
                Some(root),
                Some(dst_dir("origen")),
            )],
            opts_update(),
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(
            out.as_slice(),
            [Err(SyncError::RootIsNotAStep { .. })]
        ));
    }

    // ---------- a class mismatch with a directory in the middle ----------

    #[tokio::test]
    async fn a_type_mismatch_whose_source_is_a_directory_blocks_instead_of_overwriting() {
        // `Overwrite` means "to the trash and copy BYTES" and the step
        // carries no `EntryKind` to say otherwise, so the executor could not
        // tell it apart from overwriting a file — and the directory's subtree
        // is not even in the plan, because the walk does not descend into a
        // pair that is not two directories. A human decides turning a file
        // into a tree.
        let items = run(
            vec![row(
                CompareVerdict::TypeMismatch,
                CompareCriterion::Kind,
                CompareConfidence::Certain,
                Some(src_dir("build")),
                Some(dst_file("build", 4)),
            )],
            opts_update(),
        )
        .await;
        let b = one_blocker(&items);
        assert_eq!(b.kind, SyncBlockerKind::TypeMismatchDir);
        assert_eq!(b.rel, rel("build"));
        assert_eq!(
            b.side,
            Some(Side::Left),
            "the tree is on the SOURCE (plan convention: source=Left)"
        );
        assert!(
            steps_of(&items).is_empty(),
            "and no step comes out doing it anyway"
        );
    }

    #[tokio::test]
    async fn a_type_mismatch_whose_destination_is_a_directory_blocks_too() {
        // The worse of the two cases: here what would be destroyed is a
        // DESTINATION tree, and `Overwrite` would bury it whole with a single
        // line of the plan.
        let items = run(
            vec![row(
                CompareVerdict::TypeMismatch,
                CompareCriterion::Kind,
                CompareConfidence::Certain,
                Some(src_file("build", 4)),
                Some(dst_dir("build")),
            )],
            opts_update(),
        )
        .await;
        let b = one_blocker(&items);
        assert_eq!(b.kind, SyncBlockerKind::TypeMismatchDir);
        assert_eq!(b.side, Some(Side::Right));
        assert!(steps_of(&items).is_empty());
    }

    #[tokio::test]
    async fn a_blocker_about_the_destination_names_the_destination_s_spelling() {
        // A blocker's `rel` is measured against the root of the side it
        // SPEAKS about, same as in `AmbiguousDest` and `DirTooLarge`. With a
        // folded pair — an NFC `café` from the source against the
        // destination's NFD `café` — naming it with the source's spelling
        // would paint a path that does not exist at the destination: the
        // same hole `dest_rel` plugs on steps.
        let items = run(
            vec![row(
                CompareVerdict::TypeMismatch,
                CompareCriterion::Kind,
                CompareConfidence::Certain,
                Some(entry_at(
                    &source_root(),
                    "café".as_bytes(),
                    EntryKind::File,
                    Some(1),
                )),
                Some(entry_at(
                    &dest_root(),
                    b"cafe\xcc\x81",
                    EntryKind::Dir,
                    None,
                )),
            )],
            opts_update(),
        )
        .await;
        let b = one_blocker(&items);
        assert_eq!(b.side, Some(Side::Right));
        assert_eq!(
            b.rel.segments()[0].as_bytes(),
            b"cafe\xcc\x81",
            "the tree that is not touched is at the destination, and is named that way THERE"
        );
    }

    #[tokio::test]
    async fn a_type_mismatch_with_a_directory_blocks_under_mirror_as_well() {
        // The mode does not change what a directory mismatch means.
        let items = run(
            vec![row(
                CompareVerdict::TypeMismatch,
                CompareCriterion::Kind,
                CompareConfidence::Certain,
                Some(src_dir("build")),
                Some(dst_file("build", 4)),
            )],
            opts_mirror(),
        )
        .await;
        assert_eq!(one_blocker(&items).kind, SyncBlockerKind::TypeMismatchDir);
    }

    // ---------- the two spellings of a pair (issue #152) ----------

    #[tokio::test]
    async fn a_pair_whose_two_names_differ_in_bytes_names_the_destination_entry_too() {
        // `norte-compare`'s pairing key folds (always NFC, uppercase if
        // either side is not case sensitive), so two entries with DIFFERENT
        // bytes come out in a `Different` row with no mark at all. If the
        // step only carried the source's `rel`, the executor would paste an
        // NFC `café` onto `dest_root` and on ext4 would write a SECOND file
        // alongside the one it meant to overwrite — with a reversal that
        // promises to pull out of the trash something nobody buried.
        let nfc = entry_at(&source_root(), "café".as_bytes(), EntryKind::File, Some(10));
        let nfd = entry_at(&dest_root(), b"cafe\xcc\x81", EntryKind::File, Some(9));
        let items = run(
            vec![row(
                CompareVerdict::Different,
                CompareCriterion::Size,
                CompareConfidence::Certain,
                Some(nfc),
                Some(nfd),
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Overwrite);
        assert_eq!(
            s.rel.segments()[0].as_bytes(),
            "café".as_bytes(),
            "the rel is still the SOURCE's: that is where it is read from"
        );
        assert_eq!(
            s.dest_rel.as_ref().expect("two spellings").segments()[0].as_bytes(),
            b"cafe\xcc\x81",
            "and the destination is written where it IS, without renaming it to NFC"
        );
        assert_eq!(s.size, Some(10), "the bytes moved are the source's");
        assert!(s.shape_is_consistent());
    }

    #[tokio::test]
    async fn a_pair_that_is_spelt_the_same_carries_no_dest_rel() {
        // The COMMON case, and that is why the key travels absent: half a
        // million steps do not pay for a repeated path.
        let items = run(
            vec![row(
                CompareVerdict::Different,
                CompareCriterion::Size,
                CompareConfidence::Certain,
                Some(src_file("a.txt", 10)),
                Some(dst_file("a.txt", 9)),
            )],
            opts_update(),
        )
        .await;
        assert_eq!(one_step(&items).dest_rel, None);
    }

    #[tokio::test]
    async fn a_case_folded_pair_writes_over_the_name_the_destination_really_has() {
        // `README` against an APFS's `readme`: the key pairs them because the
        // destination is not case sensitive. Writing `README` there is
        // writing over `readme` regardless — but the plan has to SAY SO,
        // because the journal and the trash name the entry that exists.
        let items = run(
            vec![row(
                CompareVerdict::Different,
                CompareCriterion::Mtime,
                CompareConfidence::Probable,
                Some(src_file("README", 10)),
                Some(dst_file("readme", 9)),
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.rel, rel("README"));
        assert_eq!(s.dest_rel, Some(rel("readme")));
    }

    #[tokio::test]
    async fn a_pair_under_a_folder_spelt_differently_names_the_whole_destination_path() {
        // The key folds at EVERY level, so the difference can be in an
        // ancestor and not in the name: `café/x.txt` against
        // `café(NFD)/x.txt`. The last segment is identical and they are still
        // two different paths — pasting `rel` onto the destination would name
        // a directory that does not exist on ext4.
        let under_ = |root: &VPath, dir: &[u8]| Entry {
            path: root
                .join(Segment::new(dir.to_vec()).expect("segment"))
                .join(Segment::new(b"x.txt".to_vec()).expect("segment")),
            kind: EntryKind::File,
            size: Some(3),
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        };
        let items = run(
            vec![row(
                CompareVerdict::Different,
                CompareCriterion::Size,
                CompareConfidence::Certain,
                Some(under_(&source_root(), "café".as_bytes())),
                Some(under_(&dest_root(), b"cafe\xcc\x81")),
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(bytes_of(&s.rel), vec!["café".as_bytes(), b"x.txt"]);
        assert_eq!(
            bytes_of(s.dest_rel.as_ref().expect("two spellings")),
            vec![b"cafe\xcc\x81".as_slice(), b"x.txt"],
            "the WHOLE path, not just the last segment"
        );
    }

    #[tokio::test]
    async fn something_only_on_the_source_never_carries_a_dest_rel() {
        // There is no destination entry: there is no second spelling to name,
        // and a `dest_rel` invented there would send the copy somewhere else.
        let items = run(
            vec![
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(src_file("a.txt", 10)),
                    None,
                ),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(src_dir("sub")),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        assert!(steps_of(&items).iter().all(|s| s.dest_rel.is_none()));
    }

    /// An entry hanging off a folder, on whichever side it is told.
    fn under(
        root: &VPath,
        dirs: &[&[u8]],
        name: &[u8],
        kind: EntryKind,
        size: Option<u64>,
    ) -> Entry {
        let mut path = root.clone();
        for dir in dirs {
            path = path.join(Segment::new(dir.to_vec()).expect("segment"));
        }
        Entry {
            path: path.join(Segment::new(name.to_vec()).expect("segment")),
            kind,
            size,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        }
    }

    /// The row of a directory pair the two sides spell differently: it is
    /// `Same` and produces no step, and it is the ONLY one that knows both
    /// spellings.
    fn dir_pair(source: &[u8], dest: &[u8]) -> CompareRow {
        row(
            CompareVerdict::Same,
            CompareCriterion::Kind,
            CompareConfidence::Certain,
            Some(entry_at(&source_root(), source, EntryKind::Dir, None)),
            Some(entry_at(&dest_root(), dest, EntryKind::Dir, None)),
        )
    }

    #[tokio::test]
    async fn a_copy_under_a_folder_the_two_sides_spell_differently_takes_the_destination_spelling()
    {
        // The other half of issue #152. The two `café` directories pair — the
        // key normalizes — and the walk descends into them, so a file only on
        // the source arrives as an orphan: its row carries no destination
        // side and there is no second spelling to READ. But the directory
        // pair's row, which arrived first because the walk is pre-order, did
        // know it: it is remembered in a map and applied here. Without it the
        // executor would create a SECOND `café` alongside the one already
        // there, on ext4.
        let items = run(
            vec![
                dir_pair("café".as_bytes(), b"cafe\xcc\x81"),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(under(
                        &source_root(),
                        &["café".as_bytes()],
                        b"nuevo.txt",
                        EntryKind::File,
                        Some(4),
                    )),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Copy);
        assert_eq!(bytes_of(&s.rel), vec!["café".as_bytes(), b"nuevo.txt"]);
        assert_eq!(
            bytes_of(s.dest_rel.as_ref().expect("the folder's spelling")),
            vec![b"cafe\xcc\x81".as_slice(), b"nuevo.txt"],
            "it is written INSIDE the directory that exists"
        );
        assert!(s.shape_is_consistent());
    }

    #[tokio::test]
    async fn a_directory_created_under_a_differently_spelt_folder_inherits_it_too() {
        // Not just copies: a `CreateDir` hanging off the folder also has to
        // be created INSIDE the one that exists, or the whole subtree is born
        // in the wrong place.
        let items = run(
            vec![
                dir_pair("café".as_bytes(), b"cafe\xcc\x81"),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(under(
                        &source_root(),
                        &["café".as_bytes()],
                        b"sub",
                        EntryKind::Dir,
                        None,
                    )),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::CreateDir);
        assert_eq!(
            bytes_of(s.dest_rel.as_ref().expect("the folder's spelling")),
            vec![b"cafe\xcc\x81".as_slice(), b"sub"]
        );
    }

    #[tokio::test]
    async fn the_deepest_folder_wins_and_carries_the_ones_above_it() {
        // Two levels that differ: the top of the map holds the WHOLE
        // destination path, so translating with it alone already carries its
        // ancestors' translation.
        let deep = Entry {
            path: source_root()
                .join(Segment::new("café".as_bytes().to_vec()).expect("segment"))
                .join(Segment::new("RESUMÉ".as_bytes().to_vec()).expect("segment"))
                .join(Segment::new(b"nuevo.txt".to_vec()).expect("segment")),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        };
        let inner_pair = row(
            CompareVerdict::Same,
            CompareCriterion::Kind,
            CompareConfidence::Certain,
            Some(under(
                &source_root(),
                &["café".as_bytes()],
                "RESUMÉ".as_bytes(),
                EntryKind::Dir,
                None,
            )),
            Some(under(
                &dest_root(),
                &[b"cafe\xcc\x81"],
                b"resume\xcc\x81",
                EntryKind::Dir,
                None,
            )),
        );
        let items = run(
            vec![
                dir_pair("café".as_bytes(), b"cafe\xcc\x81"),
                inner_pair,
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(deep),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        assert_eq!(
            bytes_of(one_step(&items).dest_rel.as_ref().expect("both folders")),
            vec![b"cafe\xcc\x81".as_slice(), b"resume\xcc\x81", b"nuevo.txt"]
        );
    }

    #[tokio::test]
    async fn a_sibling_of_the_folder_inherits_nothing_and_the_folder_survives_it() {
        // The order the walk REALLY emits: the folder's row, then ALL of its
        // siblings, and only then the ones inside it. A stack that got popped
        // at the first sibling would lose the spelling right before needing
        // it — which is what this test's first version did, with the three
        // tests built by hand in the one order the walk does not produce.
        //
        // And the other way around: what is OUTSIDE the folder cannot inherit
        // it, or `other.txt` would end up inside `café` at the destination.
        let items = run(
            vec![
                dir_pair("café".as_bytes(), b"cafe\xcc\x81"),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(src_file("otro.txt", 1)),
                    None,
                ),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(under(
                        &source_root(),
                        &["café".as_bytes()],
                        b"nuevo.txt",
                        EntryKind::File,
                        Some(4),
                    )),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        let steps = steps_of(&items);
        assert_eq!(steps.len(), 2);
        assert_eq!(
            steps[0].dest_rel, None,
            "what is outside the folder does not inherit its spelling"
        );
        assert_eq!(
            bytes_of(steps[1].dest_rel.as_ref().expect("the spelling survives")),
            vec![b"cafe\xcc\x81".as_slice(), b"nuevo.txt"],
            "…and the folder still knows its own once its children finally arrive"
        );
    }

    #[tokio::test]
    async fn a_deeper_folder_is_remembered_even_when_a_sibling_comes_between() {
        // The case where a stack would not just lose it but LIE: with `café`
        // and `café/RESUMÉ` both spelled differently, `café/zz.txt`'s row
        // slips in between `RESUMÉ` and its children. Popping the stack,
        // `RESUMÉ` is lost and `café/RESUMÉ/new.txt` comes out translated
        // to `café(NFD)/RESUMÉ/new.txt` — a path that exists on neither
        // side.
        let inner_pair = row(
            CompareVerdict::Same,
            CompareCriterion::Kind,
            CompareConfidence::Certain,
            Some(under(
                &source_root(),
                &["café".as_bytes()],
                "RESUMÉ".as_bytes(),
                EntryKind::Dir,
                None,
            )),
            Some(under(
                &dest_root(),
                &[b"cafe\xcc\x81"],
                b"RESUME\xcc\x81",
                EntryKind::Dir,
                None,
            )),
        );
        let items = run(
            vec![
                dir_pair("café".as_bytes(), b"cafe\xcc\x81"),
                inner_pair,
                // The sibling that slips in.
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(under(
                        &source_root(),
                        &["café".as_bytes()],
                        b"zz.txt",
                        EntryKind::File,
                        Some(1),
                    )),
                    None,
                ),
                // And only now, what is inside `RESUMÉ`.
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(under(
                        &source_root(),
                        &["café".as_bytes(), "RESUMÉ".as_bytes()],
                        b"nuevo.txt",
                        EntryKind::File,
                        Some(1),
                    )),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        let steps = steps_of(&items);
        assert_eq!(steps.len(), 2);
        assert_eq!(
            bytes_of(steps[0].dest_rel.as_ref().expect("the outer folder")),
            vec![b"cafe\xcc\x81".as_slice(), b"zz.txt"]
        );
        assert_eq!(
            bytes_of(steps[1].dest_rel.as_ref().expect("both folders")),
            vec![b"cafe\xcc\x81".as_slice(), b"RESUME\xcc\x81", b"nuevo.txt"]
        );
    }

    #[tokio::test]
    async fn a_non_utf8_name_under_a_folded_folder_travels_byte_for_byte() {
        // The mix: the folder pairs because the key normalizes to NFC — which
        // can only happen with UTF-8 names — and what hangs off it is NOT
        // UTF-8 and never folds. The suffix is copied raw (hard rule 1).
        let items = run(
            vec![
                dir_pair("café".as_bytes(), b"cafe\xcc\x81"),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(under(
                        &source_root(),
                        &["café".as_bytes()],
                        b"informe\xff\xfe.dat",
                        EntryKind::File,
                        Some(1),
                    )),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        assert_eq!(
            bytes_of(
                one_step(&items)
                    .dest_rel
                    .as_ref()
                    .expect("the folder's spelling")
            ),
            vec![b"cafe\xcc\x81".as_slice(), b"informe\xff\xfe.dat"]
        );
    }

    #[tokio::test]
    async fn a_sibling_whose_name_starts_with_the_same_bytes_inherits_nothing() {
        // `café` is the first bytes of `cafétière`, so a string `starts_with`
        // over the path would accept the prefix and send
        // `cafétière/x.txt` to `café(NFD)tière/x.txt` — a directory that
        // exists on neither side. Compared by SEGMENTS.
        let items = run(
            vec![
                dir_pair("café".as_bytes(), b"cafe\xcc\x81"),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(under(
                        &source_root(),
                        &["cafétière".as_bytes()],
                        b"x.txt",
                        EntryKind::File,
                        Some(2),
                    )),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(
            bytes_of(&s.rel),
            vec!["cafétière".as_bytes(), b"x.txt".as_slice()]
        );
        assert_eq!(s.dest_rel, None);
    }

    #[tokio::test]
    async fn a_folder_pair_spelt_the_same_records_nothing() {
        // The common case: if the folder is named the same on both sides,
        // whatever hangs off it has no second spelling either. Nothing to
        // record and nothing to translate.
        let items = run(
            vec![
                dir_pair(b"sub", b"sub"),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(under(
                        &source_root(),
                        &[b"sub"],
                        b"nuevo.txt",
                        EntryKind::File,
                        Some(4),
                    )),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        assert_eq!(one_step(&items).dest_rel, None);
    }

    #[tokio::test]
    async fn a_contradictory_orphan_row_cannot_redirect_the_copy() {
        // "Only on the source" AND with a destination side: the row
        // contradicts itself (`sides_are_consistent`). If a `dest_rel` came
        // out of that, the copy would go to a name nobody paired. It is
        // closed off in the transducer, not trusted to arrive well-formed.
        let items = run(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(src_file("a.txt", 10)),
                Some(dst_file("OTRA-COSA.txt", 1)),
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Copy);
        assert_eq!(s.rel, rel("a.txt"));
        assert_eq!(s.dest_rel, None);
    }

    #[tokio::test]
    async fn a_destination_entry_outside_the_destination_root_ends_the_plan() {
        // If `dest_rel` were computed wrong, the step would write outside the
        // approved tree. Closed off same as the source side: the plan dies.
        let outside = entry_at(&vpath("file:///otro"), b"a.txt", EntryKind::File, Some(9));
        let out = run_raw(
            vec![row(
                CompareVerdict::Different,
                CompareCriterion::Size,
                CompareConfidence::Certain,
                Some(src_file("a.txt", 10)),
                Some(outside),
            )],
            opts_update(),
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(
            out.as_slice(),
            [Err(SyncError::OutsideRoot { .. })]
        ));
    }

    // ---------- unknown confidence ----------

    #[tokio::test]
    async fn unknown_confidence_copies_by_default() {
        let items = run(
            vec![row(
                CompareVerdict::Same,
                CompareCriterion::Mtime,
                CompareConfidence::Unknown,
                Some(src_file("a", 1)),
                Some(dst_file("a", 1)),
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Overwrite);
        assert_eq!(
            s.confidence,
            CompareConfidence::Unknown,
            "the report has to be able to say it copied because nobody could confirm anything"
        );
        assert_eq!(s.reversal, Some(StepReversal::RestoreTrash));
        assert_eq!(s.size, Some(1));
        assert!(s.shape_is_consistent());
    }

    #[tokio::test]
    async fn unknown_confidence_skips_when_asked_to() {
        let opts = SyncOptions {
            on_unknown: OnUnknown::Skip,
            ..opts_update()
        };
        let items = run(
            vec![row(
                CompareVerdict::Same,
                CompareCriterion::Mtime,
                CompareConfidence::Unknown,
                Some(src_file("a", 1)),
                Some(dst_file("a", 1)),
            )],
            opts,
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Skip);
        assert_eq!(s.reversal, None);
        assert_eq!(s.reason, Some(SyncReason::UnknownConfidence));
        assert_eq!(
            s.size, None,
            "a `Skip` moves no bytes, and `counts.bytes` sums them up"
        );
        assert_eq!(s.confidence, CompareConfidence::Unknown);
        assert!(s.shape_is_consistent());
    }

    #[tokio::test]
    async fn a_difference_is_overwritten_whatever_on_unknown_says() {
        // `on_unknown` breaks the tie of "looks the same and nobody can
        // promise it". A row that says DIFFERENT has no tie to break.
        for on_unknown in [OnUnknown::Copy, OnUnknown::Skip] {
            let opts = SyncOptions {
                on_unknown,
                ..opts_update()
            };
            let items = run(
                vec![row(
                    CompareVerdict::Different,
                    CompareCriterion::Mtime,
                    CompareConfidence::Unknown,
                    Some(src_file("a", 1)),
                    Some(dst_file("a", 2)),
                )],
                opts,
            )
            .await;
            assert_eq!(
                one_step(&items).kind,
                SyncStepKind::Overwrite,
                "{on_unknown:?}"
            );
        }
    }

    #[tokio::test]
    async fn on_unknown_does_not_decide_what_is_missing_from_the_destination() {
        // Not copying what is not there because "it could not be verified"
        // would skip over a CERTAIN fact: there is nothing to verify where
        // there is nothing.
        let opts = SyncOptions {
            on_unknown: OnUnknown::Skip,
            ..opts_update()
        };
        let items = run(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Unknown,
                Some(src_file("a.txt", 10)),
                None,
            )],
            opts,
        )
        .await;
        assert_eq!(one_step(&items).kind, SyncStepKind::Copy);
    }

    #[tokio::test]
    async fn a_certain_same_is_never_a_skip_step() {
        // The volume of `Skip`s is bounded by how ODD the tree is, not how
        // big: a thousand identical pairs produce zero elements.
        let rows: Vec<_> = (0..1000)
            .map(|i| {
                let name = format!("f{i}.txt");
                row(
                    CompareVerdict::Same,
                    CompareCriterion::Size,
                    CompareConfidence::Certain,
                    Some(src_file(&name, 10)),
                    Some(dst_file(&name, 10)),
                )
            })
            .collect();
        assert!(run(rows, opts_update()).await.is_empty());
    }

    // ---------- error rows ----------

    /// An error row the way the walk emits it: reason and side mandatory,
    /// confidence `Unknown`, and the entry only for the side that failed.
    fn error_row(
        reason: CompareReason,
        side: Side,
        left: Option<Entry>,
        right: Option<Entry>,
    ) -> CompareRow {
        CompareRow {
            reason: Some(reason),
            side: Some(side),
            ..row(
                CompareVerdict::Error,
                CompareCriterion::Presence,
                CompareConfidence::Unknown,
                left,
                right,
            )
        }
    }

    #[tokio::test]
    async fn an_error_row_becomes_a_skip_that_names_the_read_that_failed() {
        let items = run(
            vec![error_row(
                CompareReason::Unreadable,
                Side::Left,
                Some(src_dir("secreto")),
                None,
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Skip);
        assert_eq!(s.reason, Some(SyncReason::Unreadable));
        assert_eq!(s.rel, rel("secreto"));
        assert_eq!(s.reversal, None);
        assert_eq!(s.size, None);
        assert!(s.shape_is_consistent());
    }

    #[tokio::test]
    async fn every_way_the_walk_can_fail_a_row_is_a_skip_and_none_of_them_writes() {
        // All three are "the walk could not answer for this entry", and none
        // authorizes writing over it. (Task 5 pulls the DESTINATION's
        // `DirTooLarge` out of here, which is a blocker of the whole plan.)
        for reason in [
            CompareReason::Unreadable,
            CompareReason::ReadFailed,
            CompareReason::DirTooLarge,
            CompareReason::Unknown,
        ] {
            let items = run(
                vec![error_row(
                    reason,
                    Side::Left,
                    Some(src_file("x", 1)),
                    Some(dst_file("x", 1)),
                )],
                opts_update(),
            )
            .await;
            let s = one_step(&items);
            assert_eq!(s.kind, SyncStepKind::Skip, "{reason:?}");
            assert_eq!(s.reason, Some(SyncReason::Unreadable), "{reason:?}");
        }
    }

    #[tokio::test]
    async fn an_error_row_that_only_has_a_destination_entry_still_names_it() {
        // A DESTINATION listing that would not be read: the row carries its
        // entry and nothing from the source. `rel` comes from the
        // destination's root, the one it belongs to — measuring it against
        // the source's would name something else.
        let items = run(
            vec![error_row(
                CompareReason::Unreadable,
                Side::Right,
                None,
                Some(dst_dir("privado")),
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Skip);
        assert_eq!(s.rel, rel("privado"));
        assert_eq!(s.dest_rel, None, "the rel is ALREADY the destination's");
    }

    #[tokio::test]
    async fn an_error_row_that_names_the_root_is_a_skip_at_the_root() {
        // This is the row that comes out when the walk could not list its own
        // root. A step that acts on it kills the plan (`RootIsNotAStep`); a
        // `Skip` does not act, so here it CAN carry it — and saying "I could
        // not look at the tree" is much better than staying silent or dying.
        let root = Entry {
            path: source_root(),
            kind: EntryKind::Dir,
            size: None,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        };
        let items = run(
            vec![error_row(
                CompareReason::Unreadable,
                Side::Left,
                Some(root),
                None,
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Skip);
        assert!(s.rel.is_root());
        assert_eq!(s.reason, Some(SyncReason::Unreadable));
    }

    #[tokio::test]
    async fn an_error_row_with_no_entry_on_either_side_produces_nothing() {
        // There is no path to measure, so there is no step to name. The walk
        // does not produce rows like this; a hand-built one cannot sneak in a
        // step with no `rel`.
        let items = run(
            vec![error_row(CompareReason::Unreadable, Side::Left, None, None)],
            opts_update(),
        )
        .await;
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn an_error_row_that_failed_to_hydrate_a_pair_keeps_both_spellings() {
        // The other origin of an error row: the pair DID get paired and what
        // failed was reading what the cascade needed, so the row carries both
        // sides. The `Skip` names both, because the panel paints them.
        let nfc = entry_at(&source_root(), "café".as_bytes(), EntryKind::File, None);
        let nfd = entry_at(&dest_root(), b"cafe\xcc\x81", EntryKind::File, None);
        let items = run(
            vec![error_row(
                CompareReason::Unreadable,
                Side::Right,
                Some(nfc),
                Some(nfd),
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Skip);
        assert_eq!(bytes_of(&s.rel), vec!["café".as_bytes()]);
        assert_eq!(
            bytes_of(s.dest_rel.as_ref().expect("two spellings")),
            vec![b"cafe\xcc\x81".as_slice()]
        );
    }

    // ---------- Mirror: what is left over at the destination ----------

    #[tokio::test]
    async fn mirror_turns_a_destination_orphan_into_one_delete_tree() {
        let items = run(
            vec![row(
                CompareVerdict::OnlyRight,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                None,
                Some(dst_dir("stale")),
            )],
            opts_mirror(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::DeleteTree);
        assert_eq!(s.rel, rel("stale"));
        assert_eq!(s.reversal, Some(StepReversal::RestoreTrash));
        assert_eq!(s.reason, None);
        assert_eq!(
            s.dest_rel, None,
            "a deletion's `rel` is ALREADY the destination's"
        );
        assert!(s.shape_is_consistent());
    }

    /// **The invariant a painter depends on, pinned where it is produced.**
    /// `norte_frontend::sync::render_failure` decides a failure row's anchor
    /// with the ONLY proof left on the wire: if the report sends `dest_rel`,
    /// `rel` is the SOURCE's half. A `SyncFailure` carried no class, so that
    /// rule was only correct as long as a `DeleteTree` — whose `rel` hangs
    /// off the DESTINATION — never carried `dest_rel`. Today it does not, and
    /// `anchor_of` knows it because for a STEP it does have the class and
    /// looks at it first.
    ///
    /// **0.42.0 (#195) gives the failure its class, and this test STAYS.**
    /// With `SyncFailure::kind` on the wire, the anchor stops being deduced
    /// and is read instead, so the painter no longer depends on this
    /// invariant — but a `DeleteTree` that started carrying `dest_rel` would
    /// still contradict what the field says about itself ("the destination
    /// path WHEN it is not spelled the same as `rel`"), and this test costs
    /// zero seconds. It is the cheap guard of a planner property now, no
    /// longer a frontend's scaffolding.
    ///
    /// Without this test, adding `dest_rel` to a `DeleteTree` — reasonable
    /// the day someone wants to show the destination's spelling — would
    /// silently change the anchor of a `Mirror`'s most common hostile row: a
    /// deletion denied by permissions, which would start saying "from the
    /// source" and send the operator to fix the wrong tree (C2 branch
    /// review, rust MINOR-1).
    #[tokio::test]
    async fn a_delete_tree_never_carries_dest_rel() {
        let items = run(
            vec![
                row(
                    CompareVerdict::OnlyRight,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    None,
                    Some(dst_dir("subarbol")),
                ),
                row(
                    CompareVerdict::OnlyRight,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    None,
                    Some(dst_file("suelto.bin", 10)),
                ),
            ],
            opts_mirror(),
        )
        .await;
        let deletions: Vec<_> = steps_of(&items)
            .into_iter()
            .filter(|s| s.kind == SyncStepKind::DeleteTree)
            .collect();
        assert_eq!(deletions.len(), 2, "both destination orphans");
        for s in deletions {
            assert_eq!(
                s.dest_rel, None,
                "a DeleteTree carries no dest_rel: render_failure reads that \
                 absence as \"this path is not the source's\""
            );
        }
    }

    #[tokio::test]
    async fn a_deletion_is_one_step_for_the_whole_tree_and_moves_no_bytes() {
        // ONE move to the trash, ONE journal entry, ONE thing to restore
        // (spec). And `size` absent even if the provider knows it:
        // `counts.bytes` is the sum of that field, and a deletion moves none.
        let items = run(
            vec![row(
                CompareVerdict::OnlyRight,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                None,
                Some(dst_file("gone.bin", 4096)),
            )],
            opts_mirror(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::DeleteTree);
        assert_eq!(s.size, None);
    }

    #[tokio::test]
    async fn a_delete_without_a_trash_is_irreversible_and_says_why() {
        let opts = SyncOptions {
            dest_has_trash: false,
            ..opts_mirror()
        };
        let items = run(
            vec![row(
                CompareVerdict::OnlyRight,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                None,
                Some(dst_dir("stale")),
            )],
            opts,
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.reversal, Some(StepReversal::Irreversible));
        assert_eq!(s.reason, Some(SyncReason::NoTrashOnTarget));
        assert!(s.shape_is_consistent());
    }

    #[tokio::test]
    async fn mirror_copies_exactly_what_update_copies() {
        // `Mirror` ADDS a rule; it does not change any that already existed.
        let rows = || {
            vec![
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(src_file("a.txt", 10)),
                    None,
                ),
                row(
                    CompareVerdict::Different,
                    CompareCriterion::Size,
                    CompareConfidence::Certain,
                    Some(src_file("b.txt", 10)),
                    Some(dst_file("b.txt", 9)),
                ),
                row(
                    CompareVerdict::Same,
                    CompareCriterion::Hash,
                    CompareConfidence::Certain,
                    Some(src_file("c.txt", 1)),
                    Some(dst_file("c.txt", 1)),
                ),
            ]
        };
        let as_update: Vec<_> = steps_of(&run(rows(), opts_update()).await)
            .iter()
            .map(|s| (s.kind, s.rel.to_wire()))
            .collect();
        let as_mirror: Vec<_> = steps_of(&run(rows(), opts_mirror()).await)
            .iter()
            .map(|s| (s.kind, s.rel.to_wire()))
            .collect();
        assert_eq!(as_update, as_mirror);
    }

    #[tokio::test]
    async fn update_never_emits_a_delete_tree() {
        // Every verdict, once each, under `Update`.
        let rows = vec![
            row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(src_file("a.txt", 1)),
                None,
            ),
            row(
                CompareVerdict::OnlyRight,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                None,
                Some(dst_file("b.txt", 1)),
            ),
            row(
                CompareVerdict::Different,
                CompareCriterion::Size,
                CompareConfidence::Certain,
                Some(src_file("c.txt", 2)),
                Some(dst_file("c.txt", 1)),
            ),
            row(
                CompareVerdict::Same,
                CompareCriterion::Hash,
                CompareConfidence::Certain,
                Some(src_file("d.txt", 1)),
                Some(dst_file("d.txt", 1)),
            ),
            row(
                CompareVerdict::TypeMismatch,
                CompareCriterion::Kind,
                CompareConfidence::Certain,
                Some(src_file("e", 1)),
                Some(dst_link("e")),
            ),
            ambiguous_row(
                Side::Left,
                CompareReason::CaseFold,
                Some(src_file("F", 1)),
                None,
            ),
            error_row(
                CompareReason::Unreadable,
                Side::Left,
                Some(src_dir("g")),
                None,
            ),
            row(
                CompareVerdict::Unknown,
                CompareCriterion::Unknown,
                CompareConfidence::Unrecognised,
                Some(src_file("h.txt", 1)),
                Some(dst_file("h.txt", 1)),
            ),
        ];
        let items = run(rows, opts_update()).await;
        assert!(
            steps_of(&items)
                .iter()
                .all(|s| s.kind != SyncStepKind::DeleteTree),
            "`Update` NEVER deletes: {items:?}"
        );
    }

    #[tokio::test]
    async fn mirror_will_not_delete_the_destination_root_itself() {
        // A `DeleteTree` with an empty `rel` deletes the ENTIRE destination
        // tree. Comes from a caller whose roots are deeper than the
        // comparison's, and kills the plan.
        let root = Entry {
            path: dest_root(),
            kind: EntryKind::Dir,
            size: None,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        };
        let out = run_raw(
            vec![row(
                CompareVerdict::OnlyRight,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                None,
                Some(root),
            )],
            opts_mirror(),
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(
            out.as_slice(),
            [Err(SyncError::RootIsNotAStep { .. })]
        ));
    }

    #[tokio::test]
    async fn a_destination_orphan_outside_the_destination_root_ends_the_plan() {
        let out = run_raw(
            vec![row(
                CompareVerdict::OnlyRight,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                None,
                Some(entry_wire("file:///otro/x.txt", EntryKind::File, Some(1))),
            )],
            opts_mirror(),
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(
            out.as_slice(),
            [Err(SyncError::OutsideRoot { .. })]
        ));
    }

    // ---------- name collisions ----------

    /// An `Ambiguous` row the way the walk emits it (normative shape of
    /// `CompareVerdict::Ambiguous`): ONE row per entry involved, with the
    /// entry in ITS side's field and the other at `None`.
    fn ambiguous_row(
        side: Side,
        reason: CompareReason,
        left: Option<Entry>,
        right: Option<Entry>,
    ) -> CompareRow {
        CompareRow {
            reason: Some(reason),
            side: Some(side),
            ..row(
                CompareVerdict::Ambiguous,
                CompareCriterion::Presence,
                CompareConfidence::Unknown,
                left,
                right,
            )
        }
    }

    #[tokio::test]
    async fn an_ambiguous_source_is_skipped_and_the_rest_of_the_plan_stands() {
        // It is not known which of the two files to copy, so neither is
        // copied — and it is SAID. Without this `Skip` the collision would
        // leave the plan with no step and no blocker, i.e. with nobody
        // seeing it.
        let items = run(
            vec![
                ambiguous_row(
                    Side::Left,
                    CompareReason::CaseFold,
                    Some(src_file("README", 1)),
                    None,
                ),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(src_file("a.txt", 10)),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        let steps = steps_of(&items);
        assert_eq!(steps[0].kind, SyncStepKind::Skip);
        assert_eq!(steps[0].reason, Some(SyncReason::AmbiguousSource));
        assert_eq!(steps[0].rel, rel("README"));
        assert_eq!(steps[0].reversal, None);
        assert_eq!(steps[0].size, None);
        assert!(steps[0].shape_is_consistent());
        assert_eq!(
            steps[1].kind,
            SyncStepKind::Copy,
            "a collision does not stop the plan"
        );
        assert!(blockers_of(&items).is_empty());
    }

    #[tokio::test]
    async fn two_source_spellings_the_destination_folds_together_never_overwrite_each_other() {
        // Why the `Skip` above is what makes SAFE what used to be merely
        // harmless: the destination is not case sensitive, so the source's
        // `README` and `readme` point at the same file there. If either came
        // out as `Copy`, the second would write over the first inside the
        // approved tree.
        let items = run(
            vec![
                ambiguous_row(
                    Side::Left,
                    CompareReason::CaseFold,
                    Some(src_file("README", 1)),
                    None,
                ),
                ambiguous_row(
                    Side::Left,
                    CompareReason::CaseFold,
                    Some(src_file("readme", 2)),
                    None,
                ),
            ],
            opts_update(),
        )
        .await;
        let steps = steps_of(&items);
        assert_eq!(steps.len(), 2, "one row per entry, none deduplicated");
        assert!(steps.iter().all(|s| s.kind == SyncStepKind::Skip));
        assert!(
            steps
                .iter()
                .all(|s| s.reason == Some(SyncReason::AmbiguousSource))
        );
    }

    #[tokio::test]
    async fn an_ambiguous_destination_blocks_the_plan() {
        // Writing there means writing over one of two files without knowing
        // which.
        let items = run(
            vec![ambiguous_row(
                Side::Right,
                CompareReason::Normalization,
                None,
                Some(dst_file("README", 1)),
            )],
            opts_update(),
        )
        .await;
        let b = one_blocker(&items);
        assert_eq!(b.kind, SyncBlockerKind::AmbiguousDest);
        assert_eq!(b.rel, rel("README"));
        assert_eq!(b.side, Some(Side::Right));
        assert!(steps_of(&items).is_empty());
    }

    #[tokio::test]
    async fn an_ambiguous_row_that_does_not_name_its_side_falls_towards_the_blocker() {
        // The walk always names the side. A row that did not would be
        // decided by which entry it carries, and the TIE falls on the
        // destination side: failing toward the blocker costs redoing a plan,
        // failing toward the `Skip` costs a file.
        let no_side = CompareRow {
            side: None,
            ..ambiguous_row(
                Side::Right,
                CompareReason::CaseFold,
                Some(src_file("README", 1)),
                Some(dst_file("readme", 1)),
            )
        };
        let items = run(vec![no_side], opts_update()).await;
        assert_eq!(one_blocker(&items).kind, SyncBlockerKind::AmbiguousDest);

        // And with an entry ONLY from the source there is no possible
        // destination collision: there the evidence is conclusive and a
        // `Skip` comes out.
        let source_only = CompareRow {
            side: None,
            ..ambiguous_row(
                Side::Left,
                CompareReason::CaseFold,
                Some(src_file("README", 1)),
                None,
            )
        };
        let items = run(vec![source_only], opts_update()).await;
        assert_eq!(one_step(&items).reason, Some(SyncReason::AmbiguousSource));
    }

    #[tokio::test]
    async fn the_side_of_a_collision_is_the_row_s_and_not_the_panel_s() {
        // With the source on the RIGHT, a collision on the left side belongs
        // to the DESTINATION and blocks, even though the blocker is reported
        // as `Right` (plan convention: source `Left`, destination `Right`).
        let opts = SyncOptions {
            source_root: dest_root(),
            dest_root: source_root(),
            source_side: Side::Right,
            ..opts_update()
        };
        let items = run(
            vec![ambiguous_row(
                Side::Left,
                CompareReason::CaseFold,
                Some(src_file("README", 1)),
                None,
            )],
            opts,
        )
        .await;
        let b = one_blocker(&items);
        assert_eq!(b.kind, SyncBlockerKind::AmbiguousDest);
        assert_eq!(b.side, Some(Side::Right));
    }

    // ---------- whole-tree blockers ----------

    #[tokio::test]
    async fn a_read_only_destination_blocks_before_a_single_step() {
        let opts = SyncOptions {
            dest_writable: false,
            ..opts_update()
        };
        let items = run(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(src_file("a.txt", 10)),
                None,
            )],
            opts,
        )
        .await;
        let b = one_blocker(&items);
        assert_eq!(b.kind, SyncBlockerKind::DestReadOnly);
        assert!(
            b.rel.is_root(),
            "not about one place: it is about the whole tree"
        );
        assert_eq!(b.side, Some(Side::Right));
        assert!(
            steps_of(&items).is_empty(),
            "no writes are planned against a tree that refuses them"
        );
    }

    #[tokio::test]
    async fn a_read_only_destination_does_not_even_look_at_the_rows() {
        // There is nothing a tree could say that would change the result, and
        // dragging the whole walk through for it costs minutes against a
        // network. This test's row would kill the plan with `OutsideRoot` if
        // it ever got looked at.
        let opts = SyncOptions {
            dest_writable: false,
            ..opts_update()
        };
        let out = run_raw(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(entry_wire("file:///otro/x.txt", EntryKind::File, Some(1))),
                None,
            )],
            opts,
            CancellationToken::new(),
        )
        .await;
        assert_eq!(out.len(), 1, "only the blocker: {out:?}");
        assert!(out[0].is_ok());
    }

    #[tokio::test]
    async fn a_read_only_destination_blocks_even_when_the_comparison_is_empty() {
        // The blocker does not depend on there being rows: it comes out
        // before requesting the first one. An empty source against a
        // read-only destination is a plan that cannot be executed, not an
        // empty plan that gets approved on its own.
        let opts = SyncOptions {
            dest_writable: false,
            ..opts_update()
        };
        let items = run(vec![], opts).await;
        assert_eq!(one_blocker(&items).kind, SyncBlockerKind::DestReadOnly);
    }

    #[tokio::test]
    async fn a_destination_directory_over_the_entry_limit_blocks() {
        // It is not known what is in that directory, and the plan was going
        // to write inside it.
        let items = run(
            vec![error_row(
                CompareReason::DirTooLarge,
                Side::Right,
                None,
                Some(dst_dir("fotos")),
            )],
            opts_update(),
        )
        .await;
        let b = one_blocker(&items);
        assert_eq!(b.kind, SyncBlockerKind::DirTooLarge);
        assert_eq!(b.rel, rel("fotos"));
        assert_eq!(b.side, Some(Side::Right));
        assert!(steps_of(&items).is_empty());
    }

    #[tokio::test]
    async fn the_same_limit_on_the_source_is_still_only_a_skip() {
        // Not knowing what is in a SOURCE directory only means nothing gets
        // copied from there. There is nothing to lose at the destination.
        let items = run(
            vec![error_row(
                CompareReason::DirTooLarge,
                Side::Left,
                Some(src_dir("fotos")),
                None,
            )],
            opts_update(),
        )
        .await;
        let s = one_step(&items);
        assert_eq!(s.kind, SyncStepKind::Skip);
        assert_eq!(s.reason, Some(SyncReason::Unreadable));
        assert!(blockers_of(&items).is_empty());
    }

    // ---------- the overlap the walk finds ----------

    #[tokio::test]
    async fn reaching_the_other_root_prunes_and_blocks() {
        // `/a` against `/a/sub`: two different `VPath`s naming the same tree.
        // Copying the first onto the second copies a subtree inside itself.
        // The daemon's structural check can be defeated with a symlink; this
        // one does not depend on it.
        let opts = SyncOptions {
            source_root: vpath("file:///a"),
            dest_root: vpath("file:///a/sub"),
            ..opts_update()
        };
        let items = run(
            vec![
                // The walk reached the destination's root…
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(entry_wire("file:///a/sub", EntryKind::Dir, None)),
                    None,
                ),
                // …and everything underneath it.
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(entry_wire("file:///a/sub/x.txt", EntryKind::File, Some(1))),
                    None,
                ),
            ],
            opts,
        )
        .await;
        let b = one_blocker(&items);
        assert_eq!(b.kind, SyncBlockerKind::OverlapDetected);
        assert_eq!(
            b.rel,
            rel("sub"),
            "measured against the root it was walking along"
        );
        assert_eq!(b.side, None, "the overlap belongs to BOTH roots");
        assert!(
            steps_of(&items).is_empty(),
            "the subtree is pruned, not copied inside itself"
        );
    }

    #[tokio::test]
    async fn what_is_outside_the_overlap_is_still_planned() {
        // The SUBTREE is pruned, not the plan: the rest of the tree still
        // comes out, and the blocker is what prevents approving it.
        let opts = SyncOptions {
            source_root: vpath("file:///a"),
            dest_root: vpath("file:///a/sub"),
            ..opts_update()
        };
        let items = run(
            vec![
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(entry_wire("file:///a/sub/x.txt", EntryKind::File, Some(1))),
                    None,
                ),
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(entry_wire("file:///a/otro.txt", EntryKind::File, Some(1))),
                    None,
                ),
                // And a PAIRED row, which is the one that uncovers whether the
                // pruning looks at the wrong side: every destination path
                // hangs off `dest_root` by definition, so pruning by it would
                // take down the whole plan instead of the subtree.
                row(
                    CompareVerdict::Different,
                    CompareCriterion::Size,
                    CompareConfidence::Certain,
                    Some(entry_wire("file:///a/par.txt", EntryKind::File, Some(2))),
                    Some(entry_wire(
                        "file:///a/sub/par.txt",
                        EntryKind::File,
                        Some(1),
                    )),
                ),
            ],
            opts,
        )
        .await;
        assert_eq!(one_blocker(&items).kind, SyncBlockerKind::OverlapDetected);
        let steps: Vec<_> = steps_of(&items)
            .iter()
            .map(|s| (s.kind, s.rel.to_wire()))
            .collect();
        assert_eq!(
            steps,
            vec![
                (SyncStepKind::Copy, "otro.txt".to_owned()),
                (SyncStepKind::Overwrite, "par.txt".to_owned()),
            ]
        );
    }

    #[tokio::test]
    async fn the_overlap_is_reported_once_however_deep_the_subtree_is() {
        // The ROOT reached is kept, not the path of the row that uncovered
        // it, so a single prefix swallows the whole subtree: a hundred rows
        // inside are not a hundred blockers.
        let opts = SyncOptions {
            source_root: vpath("file:///a"),
            dest_root: vpath("file:///a/sub"),
            ..opts_update()
        };
        let mut rows = vec![row(
            CompareVerdict::OnlyLeft,
            CompareCriterion::Presence,
            CompareConfidence::Certain,
            Some(entry_wire("file:///a/sub", EntryKind::Dir, None)),
            None,
        )];
        for i in 0..100 {
            rows.push(row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(entry_wire(
                    &format!("file:///a/sub/d{i}/f{i}.txt"),
                    EntryKind::File,
                    Some(1),
                )),
                None,
            ));
        }
        let items = run(rows, opts).await;
        assert_eq!(blockers_of(&items).len(), 1);
        assert!(steps_of(&items).is_empty());
    }

    #[tokio::test]
    async fn a_destination_row_that_reaches_the_source_root_is_overlap_too() {
        // The mirror image: the SOURCE's root is inside the destination's, so
        // the one reaching the other root is a row on the destination side.
        let opts = SyncOptions {
            source_root: vpath("file:///a/sub"),
            dest_root: vpath("file:///a"),
            ..opts_update()
        };
        let items = run(
            vec![row(
                CompareVerdict::OnlyRight,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                None,
                Some(entry_wire("file:///a/sub/x.txt", EntryKind::File, Some(1))),
            )],
            opts,
        )
        .await;
        assert_eq!(one_blocker(&items).kind, SyncBlockerKind::OverlapDetected);
        assert!(steps_of(&items).is_empty());
    }

    #[tokio::test]
    async fn a_root_whose_bytes_prefix_the_other_is_not_an_overlap() {
        // `file:///a/su` is a STRING prefix of `file:///a/sub` and not a
        // segment one. By string, the whole source tree would "reach" the
        // destination's root and the whole plan would block for nothing
        // (hard rule 1).
        let opts = SyncOptions {
            source_root: vpath("file:///a"),
            dest_root: vpath("file:///a/su"),
            ..opts_update()
        };
        let items = run(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(entry_wire("file:///a/sub", EntryKind::Dir, None)),
                None,
            )],
            opts,
        )
        .await;
        assert!(blockers_of(&items).is_empty());
        assert_eq!(one_step(&items).kind, SyncStepKind::CreateDir);
    }

    #[tokio::test]
    async fn two_providers_that_spell_their_root_the_same_are_not_an_overlap() {
        // The transducer knows nothing about providers: two `mem:///`s of two
        // different providers arrive here indistinguishable from a tree
        // against itself. It gets planned, and nothing is lost by it — a tree
        // compared against itself gives `Same` rows, i.e. zero steps. What is
        // dangerous is CONTAINMENT.
        let root = vpath("mem:///");
        let opts = SyncOptions {
            source_root: root.clone(),
            dest_root: root,
            ..opts_update()
        };
        let items = run(
            vec![row(
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
                Some(entry_wire("mem:///a.txt", EntryKind::File, Some(1))),
                None,
            )],
            opts,
        )
        .await;
        assert!(blockers_of(&items).is_empty());
        assert_eq!(one_step(&items).kind, SyncStepKind::Copy);
    }

    #[tokio::test]
    async fn nothing_at_all_is_planned_under_a_pruned_subtree() {
        // Not a `Skip`, not a blocker of another class: inside the pruned
        // subtree the plan says nothing, because nothing there belongs to the
        // tree that was meant to be synchronized.
        let opts = SyncOptions {
            source_root: vpath("file:///a"),
            dest_root: vpath("file:///a/sub"),
            ..opts_update()
        };
        let items = run(
            vec![
                row(
                    CompareVerdict::OnlyLeft,
                    CompareCriterion::Presence,
                    CompareConfidence::Certain,
                    Some(entry_wire("file:///a/sub", EntryKind::Dir, None)),
                    None,
                ),
                CompareRow {
                    reason: Some(CompareReason::Unreadable),
                    side: Some(Side::Left),
                    ..row(
                        CompareVerdict::Error,
                        CompareCriterion::Presence,
                        CompareConfidence::Unknown,
                        Some(entry_wire("file:///a/sub/secreto", EntryKind::Dir, None)),
                        None,
                    )
                },
            ],
            opts,
        )
        .await;
        assert_eq!(items.len(), 1, "only the overlap blocker: {items:?}");
    }

    // ---------- real rows: compare → plan ----------

    /// Seeds a file in a `MemProvider`, creating the path's directories.
    /// Names travel in BYTES.
    async fn seed(mem: &norte_testkit::MemProvider, segments: &[&[u8]], content: &[u8]) {
        use norte_vfs::Provider as _;
        let segs: Vec<Segment> = segments
            .iter()
            .map(|s| Segment::new(*s).expect("segment"))
            .collect();
        let (name, dirs) = segs.split_last().expect("non-empty path");
        let mut at = norte_testkit::MemProvider::root();
        for dir in dirs {
            at = at.join(dir.clone());
            let _ = mem.mkdir(&at).await;
        }
        let mut sink = mem.write(&at.join(name.clone())).await.expect("write");
        sink.write(bytes::Bytes::copy_from_slice(content))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }

    #[tokio::test]
    async fn real_compare_rows_carry_the_destination_spelling_down_the_tree() {
        // The test no table test can give: the WALK sets the row order, and
        // the walk emits them by directory — all of one level, then each
        // subdirectory's — so between `café` and its child its siblings slip
        // in, and between `café/RESUMÉ` and its own too. This test's first
        // version was a stack, passed the five hand-built tests and failed
        // here: `new.txt` came out with no `dest_rel` or, worse, with one
        // naming a folder that exists on neither side.
        use norte_compare::{CompareOptions, Sides, compare};
        use norte_vfs::Provider as _;
        let source = norte_testkit::MemProvider::new();
        let dest = norte_testkit::MemProvider::new();
        // Source in NFC; destination in NFD, which is what macOS returns. The
        // pairing key normalizes to NFC ALWAYS, so the two folders pair even
        // though their bytes differ.
        seed(
            &source,
            &["café".as_bytes(), "RESUMÉ".as_bytes(), b"nuevo.txt"],
            b"nuevo",
        )
        .await;
        seed(&source, &["café".as_bytes(), b"zz.txt"], b"hermana").await;
        seed(
            &dest,
            &[b"cafe\xcc\x81", b"RESUME\xcc\x81", b"viejo.txt"],
            b"viejo",
        )
        .await;

        let root = norte_testkit::MemProvider::root();
        let sides = Sides::from_capabilities(source.capabilities(), dest.capabilities());
        let rows = compare(
            &source,
            &root,
            &dest,
            &root,
            CompareOptions::cheap(),
            sides,
            Vec::new(),
            CancellationToken::new(),
        );
        let opts = SyncOptions {
            source_root: root.clone(),
            dest_root: root,
            ..opts_update()
        };
        let items: Vec<PlanItem> = plan(rows, opts, CancellationToken::new())
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.expect("no row from the walk escapes its root"))
            .collect();

        let copies: Vec<Spellings<'_>> = steps_of(&items)
            .iter()
            .filter(|s| s.kind == SyncStepKind::Copy)
            .map(|s| (bytes_of(&s.rel), s.dest_rel.as_ref().map(bytes_of)))
            .collect();
        assert_eq!(
            copies,
            vec![
                // The order is the walk's: first the whole `café` level — the
                // sibling included — and only then what is inside `RESUMÉ`.
                // That `zz.txt` slips in between is precisely what broke this
                // test's first version.
                (
                    vec!["café".as_bytes(), b"zz.txt"],
                    Some(vec![b"cafe\xcc\x81".as_slice(), b"zz.txt"])
                ),
                (
                    vec!["café".as_bytes(), "RESUMÉ".as_bytes(), b"nuevo.txt"],
                    Some(vec![
                        b"cafe\xcc\x81".as_slice(),
                        b"RESUME\xcc\x81",
                        b"nuevo.txt"
                    ])
                ),
            ],
            "every copy goes into the folder that EXISTS at the destination"
        );
    }

    #[tokio::test]
    async fn real_compare_rows_plan_without_a_single_outside_root() {
        // The 17 table tests build their rows by hand, so none of them
        // touches the contract that crosses the two crates: EVERY path the
        // walk emits hangs off the root it was given. If that stops holding,
        // the whole plan dies with `OutsideRoot` — and it is only seen here.
        use norte_compare::{CompareOptions, Sides, compare};
        use norte_vfs::Provider as _;
        let source = norte_testkit::MemProvider::new();
        let dest = norte_testkit::MemProvider::new();
        seed(&source, &[b"sub", b"informe\xff\xfe.dat"], b"nuevo").await;
        seed(&source, &[b"raiz.txt"], b"nuevo").await;
        seed(&dest, &[b"raiz.txt"], b"viejo mas largo").await;

        let root = norte_testkit::MemProvider::root();
        let sides = Sides::from_capabilities(source.capabilities(), dest.capabilities());
        let rows = compare(
            &source,
            &root,
            &dest,
            &root,
            CompareOptions {
                descend_orphans: Some(Side::Left),
                ..CompareOptions::cheap()
            },
            sides,
            Vec::new(),
            CancellationToken::new(),
        );
        let opts = SyncOptions {
            source_root: root.clone(),
            dest_root: root,
            ..opts_update()
        };
        let out: Vec<_> = plan(rows, opts, CancellationToken::new()).collect().await;
        let items: Vec<PlanItem> = out
            .into_iter()
            .map(|r| r.expect("no row from the walk escapes its root"))
            .collect();

        // The walk is pre-order and the merge is sorted by key, so the order
        // is a given fact and needs no sorting: `root.txt` before `sub`, and
        // `sub` before what is inside it.
        let output: Vec<(SyncStepKind, String)> = steps_of(&items)
            .iter()
            .map(|s| (s.kind, s.rel.to_wire()))
            .collect();
        assert_eq!(
            output,
            vec![
                (SyncStepKind::Overwrite, "raiz.txt".to_owned()),
                (SyncStepKind::CreateDir, "sub".to_owned()),
                (SyncStepKind::Copy, "sub/informe%FF%FE.dat".to_owned()),
            ],
            "a CreateDir always precedes what goes inside it"
        );
    }
}
