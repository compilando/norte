//! The transducer's and the `plan_hash`'s properties, over trees and plans
//! nobody wrote by hand.
//!
//! `plan.rs`'s and `hash.rs`'s table tests pin CASES; this pins what has to
//! hold for ANY flow: an identical tree plans nothing, `Update` never
//! deletes, no `rel` comes from a place the rows did not name, irreversible
//! is exactly what is destructive with no trash, a step's target is the file
//! that EXISTS at the destination, and two different plans do not share a
//! fingerprint.
//!
//! Names come from the ugly corpus and, above all, in PAIRS spelled
//! differently — an NFC `café` against an NFD `café`, `README` against
//! `readme` — since that is where the comparison's pairing folds and where
//! issue #152 lives, so a corpus that only varies names BETWEEN rows never
//! touches the dangerous part.

use std::collections::BTreeSet;

use futures::StreamExt;
use futures::executor::block_on;
use norte_proto::methods::{
    CompareConfidence, CompareCriterion, CompareReason, CompareRow, CompareVerdict,
    SyncCompareOptions,
};
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_sync::{
    OnUnknown, PlanHasher, PlanItem, RelPath, Side, StepReversal, SyncBlocker, SyncBlockerKind,
    SyncMode, SyncOptions, SyncReason, SyncStep, SyncStepKind, plan,
};
use proptest::prelude::*;
use tokio_util::sync::CancellationToken;

fn vpath(wire: &str) -> VPath {
    VPath::parse(wire).expect("path")
}

fn source_root() -> VPath {
    vpath("file:///origen")
}

fn dest_root() -> VPath {
    vpath("file:///destino")
}

/// A plan's wiring, which is what the rows do NOT carry.
#[derive(Debug, Clone, Copy)]
struct Wiring {
    mode: SyncMode,
    on_unknown: OnUnknown,
    source_side: Side,
    trash: bool,
    /// And does that trash name what it buries? One that does not turns the
    /// WHOLE plan IRREVERSIBLE, not just the destructive part.
    trash_restorable: bool,
    writable: bool,
}

impl Wiring {
    /// Is the RIGHT side of the rows the source? Decides where each
    /// generated entry hangs off.
    fn source_right(self) -> bool {
        self.source_side == Side::Right
    }
}

fn opts_of(w: Wiring) -> SyncOptions {
    SyncOptions {
        source_root: source_root(),
        dest_root: dest_root(),
        mode: w.mode,
        on_unknown: w.on_unknown,
        source_side: w.source_side,
        dest_has_trash: w.trash,
        dest_trash_restorable: w.trash_restorable,
        dest_writable: w.writable,
    }
}

fn opts_update() -> SyncOptions {
    opts_of(Wiring {
        mode: SyncMode::Update,
        on_unknown: OnUnknown::Copy,
        source_side: Side::Left,
        trash: true,
        trash_restorable: true,
        writable: true,
    })
}

fn opts_mirror() -> SyncOptions {
    SyncOptions {
        mode: SyncMode::Mirror,
        ..opts_update()
    }
}

/// COMPLETE wirings: both modes, both confidence policies, both source sides,
/// with and without a trash, with and without write access.
fn wiring() -> impl Strategy<Value = Wiring> {
    (
        prop_oneof![Just(SyncMode::Update), Just(SyncMode::Mirror)],
        prop_oneof![Just(OnUnknown::Copy), Just(OnUnknown::Skip)],
        prop_oneof![Just(Side::Left), Just(Side::Right)],
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(
            |(mode, on_unknown, source_side, trash, trash_restorable, writable)| Wiring {
                mode,
                on_unknown,
                source_side,
                trash,
                trash_restorable,
                writable,
            },
        )
}

/// A path under `root`, segment by segment and by its BYTES.
fn under(root: &VPath, segments: &[Vec<u8>]) -> VPath {
    segments.iter().fold(root.clone(), |acc, segment| {
        acc.join(Segment::new(segment.clone()).expect("segment"))
    })
}

fn entry(root: &VPath, segments: &[Vec<u8>], kind: EntryKind, size: Option<u64>) -> Entry {
    Entry {
        path: under(root, segments),
        kind,
        size,
        mtime_ms: None,
        attrs: std::collections::BTreeMap::default(),
    }
}

/// How the DESTINATION spells a source name when the pairing key folds them
/// into one: NFC against NFD, and the two cases of the same name. Everything
/// else is spelled the same on both sides.
fn twin_of(name: &[u8]) -> Vec<u8> {
    match name {
        b"README" => b"readme".to_vec(),
        n if n == "caf\u{e9}".as_bytes() => "cafe\u{301}".as_bytes().to_vec(),
        other => other.to_vec(),
    }
}

fn twin_path(segments: &[Vec<u8>]) -> Vec<Vec<u8>> {
    segments.iter().map(|s| twin_of(s)).collect()
}

/// A name from the ugly corpus: the two that have a twin at the destination,
/// a name that is not UTF-8, and two ordinary ones.
fn name() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        Just(b"a.txt".to_vec()),
        Just(b"sub".to_vec()),
        Just(b"README".to_vec()),
        Just("caf\u{e9}".as_bytes().to_vec()),
        Just(b"informe\xff\xfe.dat".to_vec()),
    ]
}

/// From one to THREE components: always at least one, because a path that
/// was the root itself is a different contract (`SyncError::RootIsNotAStep`)
/// and the table tests pin it. Three is what is needed for the by-ancestor
/// translation to have two levels to walk.
fn segments() -> impl Strategy<Value = Vec<Vec<u8>>> {
    prop::collection::vec(name(), 1..4)
}

fn kind() -> impl Strategy<Value = EntryKind> {
    prop_oneof![
        Just(EntryKind::File),
        Just(EntryKind::Dir),
        Just(EntryKind::Symlink),
    ]
}

fn criterion() -> impl Strategy<Value = CompareCriterion> {
    prop_oneof![
        Just(CompareCriterion::Presence),
        Just(CompareCriterion::Kind),
        Just(CompareCriterion::Size),
        Just(CompareCriterion::Mtime),
        Just(CompareCriterion::Hash),
    ]
}

fn confidence() -> impl Strategy<Value = CompareConfidence> {
    prop_oneof![
        Just(CompareConfidence::Certain),
        Just(CompareConfidence::Probable),
        Just(CompareConfidence::Unknown),
    ]
}

/// Every verdict, including the one an N+1 daemon could send.
fn verdict() -> impl Strategy<Value = CompareVerdict> {
    prop_oneof![
        Just(CompareVerdict::Same),
        Just(CompareVerdict::Different),
        Just(CompareVerdict::OnlyLeft),
        Just(CompareVerdict::OnlyRight),
        Just(CompareVerdict::TypeMismatch),
        Just(CompareVerdict::Ambiguous),
        Just(CompareVerdict::Error),
        Just(CompareVerdict::Unknown),
    ]
}

fn reason() -> impl Strategy<Value = Option<CompareReason>> {
    prop_oneof![
        Just(None),
        Just(Some(CompareReason::CaseFold)),
        Just(Some(CompareReason::Normalization)),
        Just(Some(CompareReason::Unreadable)),
        Just(Some(CompareReason::ReadFailed)),
        Just(Some(CompareReason::DirTooLarge)),
    ]
}

fn side() -> impl Strategy<Value = Option<Side>> {
    prop_oneof![Just(None), Just(Some(Side::Left)), Just(Some(Side::Right))]
}

/// Any row, with the SOURCE entry hanging off `source_root` and the
/// DESTINATION's off `dest_root` — which side each falls on is decided by the
/// wiring — and with the destination's spelling folded when the name has a
/// twin.
fn any_row(source_right: bool) -> impl Strategy<Value = CompareRow> {
    (
        segments(),
        any::<bool>(),
        verdict(),
        criterion(),
        confidence(),
        kind(),
        kind(),
        prop::option::of(0u64..1_000_000),
        reason(),
        side(),
    )
        .prop_map(
            move |(
                segs,
                twinned,
                verdict,
                criterion,
                confidence,
                source_kind,
                dest_kind,
                size,
                reason,
                side,
            )| {
                let dest_segs = if twinned {
                    twin_path(&segs)
                } else {
                    segs.clone()
                };
                let source = entry(&source_root(), &segs, source_kind, size);
                let dest = entry(&dest_root(), &dest_segs, dest_kind, size);
                let (left, right) = if source_right {
                    (dest, source)
                } else {
                    (source, dest)
                };
                let (left, right) = match verdict {
                    CompareVerdict::OnlyLeft => (Some(left), None),
                    CompareVerdict::OnlyRight => (None, Some(right)),
                    _ => (Some(left), Some(right)),
                };
                CompareRow {
                    id: 0,
                    left,
                    right,
                    verdict,
                    criterion,
                    confidence,
                    newer: None,
                    reason,
                    side,
                    paired_under: None,
                }
            },
        )
}

/// A wiring and a row flow consistent with it.
fn scenario() -> impl Strategy<Value = (Wiring, Vec<CompareRow>)> {
    wiring().prop_flat_map(|w| {
        (
            Just(w),
            prop::collection::vec(any_row(w.source_right()), 0..8),
        )
    })
}

/// Rows that all say "equal, and with certainty": the tree compared against
/// itself, with the same spelling on both sides.
fn same_rows_strategy(source_right: bool) -> impl Strategy<Value = Vec<CompareRow>> {
    let row = (segments(), kind(), criterion(), 0u64..1_000_000).prop_map(
        move |(segs, kind, criterion, size)| {
            let source = entry(&source_root(), &segs, kind, Some(size));
            let dest = entry(&dest_root(), &segs, kind, Some(size));
            let (left, right) = if source_right {
                (dest, source)
            } else {
                (source, dest)
            };
            CompareRow {
                id: 0,
                left: Some(left),
                right: Some(right),
                verdict: CompareVerdict::Same,
                criterion,
                confidence: CompareConfidence::Certain,
                newer: None,
                reason: None,
                side: None,
                paired_under: None,
            }
        },
    );
    prop::collection::vec(row, 0..8)
}

/// A wiring and a tree identical to itself, consistent with each other.
fn same_scenario() -> impl Strategy<Value = (Wiring, Vec<CompareRow>)> {
    wiring().prop_flat_map(|w| (Just(w), same_rows_strategy(w.source_right())))
}

/// PAIRED rows — both entries, always — with the destination's spelling
/// folded. These are the ones that exercise `dest_rel`: with no pair there is
/// no second spelling to read.
fn paired_rows_strategy() -> impl Strategy<Value = Vec<CompareRow>> {
    let row = (
        segments(),
        any::<bool>(),
        prop_oneof![
            Just(CompareVerdict::Same),
            Just(CompareVerdict::Different),
            Just(CompareVerdict::TypeMismatch),
            Just(CompareVerdict::Error),
        ],
        criterion(),
        confidence(),
        kind(),
        prop::option::of(0u64..1_000_000),
    )
        .prop_map(
            |(segs, twinned, verdict, criterion, confidence, kind, size)| {
                let dest_segs = if twinned {
                    twin_path(&segs)
                } else {
                    segs.clone()
                };
                CompareRow {
                    id: 0,
                    left: Some(entry(&source_root(), &segs, kind, size)),
                    right: Some(entry(&dest_root(), &dest_segs, kind, size)),
                    verdict,
                    criterion,
                    confidence,
                    newer: None,
                    reason: (verdict == CompareVerdict::Error).then_some(CompareReason::Unreadable),
                    side: None,
                    paired_under: None,
                }
            },
        );
    prop::collection::vec(row, 0..8)
}

/// A PAIRED folder the two sides spell differently, and inside it a file only
/// on the source: the heart of issue #152, in the order the walk produces it
/// (the parent before the child).
fn folder_then_child() -> impl Strategy<Value = (Vec<u8>, Vec<u8>, Vec<CompareRow>)> {
    (name(), name()).prop_map(|(folder, leaf)| {
        let dest_folder = twin_of(&folder);
        let pair = CompareRow {
            id: 0,
            left: Some(entry(
                &source_root(),
                std::slice::from_ref(&folder),
                EntryKind::Dir,
                None,
            )),
            right: Some(entry(
                &dest_root(),
                std::slice::from_ref(&dest_folder),
                EntryKind::Dir,
                None,
            )),
            verdict: CompareVerdict::Same,
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Certain,
            newer: None,
            reason: None,
            side: None,
            paired_under: None,
        };
        let child = CompareRow {
            id: 1,
            left: Some(entry(
                &source_root(),
                &[folder.clone(), leaf.clone()],
                EntryKind::File,
                Some(10),
            )),
            right: None,
            verdict: CompareVerdict::OnlyLeft,
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Certain,
            newer: None,
            reason: None,
            side: None,
            paired_under: None,
        };
        (folder, leaf, vec![pair, child])
    })
}

fn run(rows: Vec<CompareRow>, opts: SyncOptions) -> Vec<PlanItem> {
    block_on(
        plan(
            futures::stream::iter(rows.into_iter().map(Ok)),
            opts,
            CancellationToken::new(),
        )
        .collect::<Vec<_>>(),
    )
    .into_iter()
    .map(|item| item.expect("rows hang off their roots: there is no error to give"))
    .collect()
}

fn steps(items: &[PlanItem]) -> Vec<SyncStep> {
    items
        .iter()
        .filter_map(|item| match item {
            PlanItem::Step { step, .. } => Some(step.clone()),
            PlanItem::Blocker(_) => None,
        })
        .collect()
}

fn blockers(items: &[PlanItem]) -> Vec<SyncBlocker> {
    items
        .iter()
        .filter_map(|item| match item {
            PlanItem::Blocker(blocker) => Some(blocker.clone()),
            PlanItem::Step { .. } => None,
        })
        .collect()
}

/// The path that comes out of pasting `rel` onto `root`, in its wire form.
fn joined(root: &VPath, rel: &RelPath) -> String {
    let segments: Vec<Vec<u8>> = rel
        .segments()
        .iter()
        .map(|segment| segment.as_bytes().to_vec())
        .collect();
    under(root, &segments).to_wire()
}

/// Every path the input rows NAMED, in wire form.
fn reported(rows: &[CompareRow]) -> BTreeSet<String> {
    rows.iter()
        .flat_map(|row| row.left.iter().chain(row.right.iter()))
        .map(|entry| entry.path.to_wire())
        .collect()
}

/// A step's target at the destination: `dest_root + dest_rel.unwrap_or(rel)`,
/// i.e. what the executor is going to open.
fn target(step: &SyncStep) -> String {
    joined(&dest_root(), step.dest_rel.as_ref().unwrap_or(&step.rel))
}

// ---------- the `plan_hash` ----------

fn rel(wire: &str) -> RelPath {
    RelPath::parse_wire(wire).expect("rel")
}

fn hash_of(items: &[PlanItem]) -> norte_proto::methods::PlanHash {
    let mut hasher = PlanHasher::new(&opts_update(), &SyncCompareOptions::default());
    for item in items {
        hasher.item(item);
    }
    hasher.finish()
}

/// The same elements with `id` zeroed: what the hash MUST distinguish.
fn without_ids(items: &[PlanItem]) -> Vec<PlanItem> {
    items
        .iter()
        .map(|item| match item {
            PlanItem::Step { step, dest } => PlanItem::Step {
                step: SyncStep {
                    id: 0,
                    ..step.clone()
                },
                dest: *dest,
            },
            PlanItem::Blocker(blocker) => PlanItem::Blocker(blocker.clone()),
        })
        .collect()
}

/// Elements chosen to collide if the framing is loose: the same bytes split
/// differently between `rel` and `dest_rel`, a root `rel`, a `Skip` and a
/// blocker at the same spot, and prefixes of one another.
fn candidate_item() -> impl Strategy<Value = PlanItem> {
    let step = |kind, rel_wire: &'static str, dest: Option<&'static str>, size| {
        let (reversal, reason) = match kind {
            SyncStepKind::Skip => (None, Some(SyncReason::Unreadable)),
            _ => (Some(StepReversal::Delete), None),
        };
        move |id: u64| PlanItem::Step {
            step: SyncStep {
                id,
                kind,
                rel: rel(rel_wire),
                dest_rel: dest.map(rel),
                size,
                criterion: CompareCriterion::Presence,
                confidence: CompareConfidence::Certain,
                reversal,
                reason,
            },
            dest: None,
        }
    };
    let shapes: Vec<Box<dyn Fn(u64) -> PlanItem>> = vec![
        Box::new(step(SyncStepKind::Copy, "ab", None, Some(1))),
        Box::new(step(SyncStepKind::Copy, "ab", Some("c"), Some(1))),
        Box::new(step(SyncStepKind::Copy, "a", Some("bc"), Some(1))),
        Box::new(step(SyncStepKind::Copy, "a/bc", None, Some(1))),
        Box::new(step(SyncStepKind::Copy, "ab/c", None, Some(1))),
        Box::new(step(SyncStepKind::Copy, "ab", None, None)),
        Box::new(step(SyncStepKind::Overwrite, "ab", None, Some(1))),
        Box::new(step(SyncStepKind::Skip, "sub/x", None, None)),
        Box::new(|id| PlanItem::Step {
            step: SyncStep {
                id,
                kind: SyncStepKind::Skip,
                rel: RelPath::default(),
                dest_rel: Some(RelPath::default()),
                size: None,
                criterion: CompareCriterion::Presence,
                confidence: CompareConfidence::Certain,
                reversal: None,
                reason: Some(SyncReason::Unreadable),
            },
            dest: None,
        }),
        Box::new(|id| PlanItem::Step {
            step: SyncStep {
                id,
                kind: SyncStepKind::Skip,
                rel: RelPath::default(),
                dest_rel: None,
                size: None,
                criterion: CompareCriterion::Presence,
                confidence: CompareConfidence::Certain,
                reversal: None,
                reason: Some(SyncReason::Unreadable),
            },
            dest: None,
        }),
        Box::new(|_| {
            PlanItem::Blocker(SyncBlocker {
                rel: rel("sub/x"),
                kind: SyncBlockerKind::AmbiguousDest,
                side: Some(Side::Right),
            })
        }),
        Box::new(|_| {
            PlanItem::Blocker(SyncBlocker {
                rel: rel("sub/x"),
                kind: SyncBlockerKind::TypeMismatchDir,
                side: Some(Side::Left),
            })
        }),
    ];
    let count = shapes.len();
    (0..count, 0u64..4).prop_map(move |(which, id)| shapes[which](id))
}

fn candidate_plan() -> impl Strategy<Value = Vec<PlanItem>> {
    prop::collection::vec(candidate_item(), 0..5)
}

proptest! {
    /// The plan of A against A is empty, whatever the wiring. This is the
    /// property that makes synchronizing an already-synchronized million-file
    /// tree cost zero steps and not a million `Skip`s.
    #[test]
    fn an_identical_tree_plans_nothing((w, rows) in same_scenario()) {
        // A read-only destination DOES produce something — its blocker — and
        // another test pins that: here what is being looked at is the tree.
        let opts = SyncOptions { dest_writable: true, ..opts_of(w) };
        prop_assert!(run(rows, opts).is_empty());
    }

    /// `Update` deletes nothing. This is the property the mode exists for.
    #[test]
    fn update_never_deletes((w, rows) in scenario()) {
        let opts = SyncOptions { mode: SyncMode::Update, ..opts_of(w) };
        for step in steps(&run(rows, opts)) {
            prop_assert_ne!(step.kind, SyncStepKind::DeleteTree);
        }
    }

    /// No `rel` escapes the roots, and the STRONG version of that: the plan
    /// does not INVENT paths. Pasted onto either of the two roots, every
    /// step's `rel` names something the input rows carried — the type already
    /// forbids `..`, what this checks is that no byte is lost or gained — and
    /// none that ACTS names the root itself, which would be the whole tree.
    ///
    /// `dest_rel` is left out on purpose: when it is composed with a folder's
    /// remembered spelling (issue #152), it names a destination path NO row
    /// carried — the new file's, inside the folder the two sides spell
    /// differently — and that is exactly what it has to do.
    #[test]
    fn rel_never_escapes((w, rows) in scenario()) {
        let paths = reported(&rows);
        for step in steps(&run(rows, opts_of(w))) {
            let from_source = joined(&source_root(), &step.rel);
            let from_dest = joined(&dest_root(), &step.rel);
            prop_assert!(
                paths.contains(&from_source) || paths.contains(&from_dest),
                "the step names a path no row carried: {from_source} / {from_dest}",
            );
            if step.kind != SyncStepKind::Skip {
                prop_assert!(!step.rel.is_root(),
                    "a step that acts on the root acts on the whole tree");
            }
        }
    }

    /// `Irreversible` appears if and only if the step cannot come back, in
    /// its two shapes: destroying something with no trash to put it in, or
    /// any step against a trash that does NOT NAME what it buries (with no
    /// `reversal_ref` the undo gets it wrong whether unearthing or deleting,
    /// #65). And it ALWAYS comes with its reason (hard rule 4).
    #[test]
    fn irreversible_iff_the_step_cannot_come_back((w, rows) in scenario()) {
        let opts = opts_of(w);
        let trash = opts.dest_has_trash;
        let mute = trash && !opts.dest_trash_restorable;
        for step in steps(&run(rows, opts)) {
            let destructive =
                matches!(step.kind, SyncStepKind::Overwrite | SyncStepKind::DeleteTree);
            let acts = destructive
                || matches!(step.kind, SyncStepKind::CreateDir | SyncStepKind::Copy);
            let irreversible = step.reversal == Some(StepReversal::Irreversible);
            prop_assert_eq!(
                irreversible,
                (destructive && !trash) || (acts && mute),
                "{:?}",
                step
            );
            prop_assert!(!irreversible || step.reason.is_some(),
                "an irreversible step owes its reason");
        }
    }

    /// Every BLOCKER's shape holds on its own. Steps are already asserted by
    /// a `debug_assert` inside the transducer; blockers are asserted by
    /// nobody, and a sideless `TypeMismatchDir` has no phrase to paint.
    #[test]
    fn every_blocker_is_shaped_consistently((w, rows) in scenario()) {
        for blocker in blockers(&run(rows, opts_of(w))) {
            prop_assert!(blocker.shape_is_consistent(), "{:?}", blocker);
        }
    }

    /// **A step's target is the file that EXISTS at the destination.** With
    /// the pair in front — which is when it can be known — `dest_root +
    /// dest_rel.unwrap_or(rel)` is exactly the destination entry's path, byte
    /// for byte: on ext4 that is the difference between overwriting the
    /// `café` that is there and creating a second one alongside it (issue
    /// #152).
    #[test]
    fn a_paired_step_targets_the_entry_that_exists(rows in paired_rows_strategy(), mirror in any::<bool>()) {
        let opts = if mirror { opts_mirror() } else { opts_update() };
        let destinations: BTreeSet<String> = rows
            .iter()
            .filter_map(|row| row.right.as_ref())
            .map(|entry| entry.path.to_wire())
            .collect();
        for step in steps(&run(rows, opts)) {
            prop_assert!(destinations.contains(&target(&step)),
                "the step would write to a path that does not exist at the destination: {}", target(&step));
        }
    }

    /// And what is only on the SOURCE inherits its folder's spelling: the
    /// other half of #152, the one not read from the row but remembered.
    #[test]
    fn a_child_of_a_folded_folder_lands_inside_it((folder, leaf, rows) in folder_then_child()) {
        let items = run(rows, opts_update());
        let steps = steps(&items);
        prop_assert_eq!(steps.len(), 1, "{:?}", items);
        let expected = joined(
            &dest_root(),
            &RelPath::new(
                [twin_of(&folder), leaf]
                    .into_iter()
                    .map(|s| Segment::new(s).expect("segment"))
                    .collect(),
            ),
        );
        prop_assert_eq!(target(&steps[0]), expected);
    }

    /// **The question a hasher cannot answer by hand: two different plans,
    /// two different fingerprints?** The candidates are chosen to collide if
    /// the framing is loose (the same bytes split differently, a root `rel`
    /// against an absent `dest_rel`, a `Skip` and a blocker at the same spot,
    /// prefixes of one another). Equality goes in BOTH directions: different
    /// ⟹ different fingerprints, and equal-except-`id` ⟹ same fingerprint,
    /// which is what pins `id` as the ONLY thing left out.
    #[test]
    fn the_digest_separates_any_two_different_plans(a in candidate_plan(), b in candidate_plan()) {
        prop_assert_eq!(hash_of(&a) == hash_of(&b), without_ids(&a) == without_ids(&b),
            "\na = {:?}\nb = {:?}", a, b);
    }
}
