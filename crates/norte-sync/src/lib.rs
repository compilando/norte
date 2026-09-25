//! `norte-sync`: turns spec 1's comparison rows into a PLAN
//! (spec `docs/superpowers/specs/2026-08-11-directory-sync-design.md`, ADR
//! 0049).
//!
//! It is a **transducer**, not a walk:
//!
//! ```text
//! Stream<CompareRow> + both sides' capabilities + SyncOptions
//!     →  Stream<PlanItem>
//! ```
//!
//! It opens no file, lists no directory, and touches no provider: all it
//! knows about them is the three booleans [`SyncOptions`] already brings
//! resolved — does the destination have a trash?, does that trash name what
//! it buries?, is it writable? — read ONCE from the provider before starting.
//! That is what lets the whole matrix — five step classes × two modes ×
//! trash/no-trash/mute-trash × three confidences — be tested exhaustively
//! without standing up a daemon.
//!
//! It mutates nothing: planning writes not a byte. Whoever executes the
//! plan — `norte_core::sync` — is the one that goes through the journal and
//! the policy engine (hard rules 4 and 9).
//!
//! The plan's vocabulary lives in `norte-proto` because it travels over the
//! wire, and is re-exported here so whoever uses the planner does not have to
//! depend on the protocol by hand.
//!
//! Alongside the transducer sits [`PlanHasher`]: the `plan_hash` that
//! summarizes what a human approves, computed in STREAMING fashion over the
//! same flow (O(1) memory, without assembling the plan). The COUNTS live in
//! `norte-proto`, with the type that travels: [`SyncCounts::add`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod hash;
pub mod plan;

pub use hash::PlanHasher;
pub use plan::{DestWitness, PlanItem, plan, rel_under};

pub use norte_proto::methods::{
    OnUnknown, PlanHash, RelPath, Side, StepReversal, SyncBlocker, SyncBlockerKind,
    SyncCompareOptions, SyncCounts, SyncMode, SyncReason, SyncStep, SyncStepKind,
};

use norte_proto::VPath;

/// Everything the transducer needs that the rows do NOT carry.
///
/// The two roots are absolute and can belong to different providers; each
/// step's `rel` is relative to both ([`RelPath`]), which is exactly what lets
/// a plan from `file://` to `sftp://` be a single vocabulary.
///
/// ```
/// use norte_proto::VPath;
/// use norte_sync::{OnUnknown, Side, SyncMode, SyncOptions};
/// let o = SyncOptions {
///     source_root: VPath::parse("file:///origen").expect("path"),
///     dest_root: VPath::parse("file:///destino").expect("path"),
///     mode: SyncMode::Update,
///     on_unknown: OnUnknown::Copy,
///     source_side: Side::Left,
///     dest_has_trash: true,
///     dest_trash_restorable: true,
///     dest_writable: true,
/// };
/// assert_eq!(o.mode, SyncMode::Update);
/// ```
///
/// # Serializable, and still NOT a wire type
/// It carries `Serialize`/`Deserialize` for ONE reason: `norte_core::sync`'s
/// spool retains the approved plan in a file, and the executor needs both
/// roots — `sync.apply` carries nothing more than the hash, on purpose
/// (ADR 0049). That file is written and read by the SAME binary within
/// `SYNC_PLAN_TTL_MS`'s window: it travels over no socket, is not in the
/// published JSON Schema, and no peer parses it, so adding a field here is
/// not a protocol change.
///
/// `deny_unknown_fields` because a spool that is not understood WHOLE is not
/// understood: a half-interpreted plan authorizes writes nobody approved.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncOptions {
    /// Where the bytes come from.
    pub source_root: VPath,
    /// …and where they go. Each step's `rel` is relative to these two.
    ///
    /// `rel` is computed over `source_root` and the executor pastes it onto
    /// this one: being LEGAL under the source does not make it legal under
    /// the destination, and the planner does not check that yet. An NFC name
    /// of 172 bytes takes up 258 when decomposed (fixture
    /// `name_max_nfd_overflow`, above ext4's and APFS's `NAME_MAX`), `CON`
    /// and a trailing dot are not names on Windows, and `f:ads` on NTFS
    /// writes an alternate stream instead of a file. Today that fails at
    /// EXECUTION, over a plan the human already approved; neither
    /// [`SyncBlockerKind`] nor [`SyncReason`] has vocabulary yet to say so
    /// earlier.
    pub dest_root: VPath,
    /// What the plan does with what is left over at the destination
    /// ([`SyncMode`]).
    ///
    /// [`SyncMode::Update`] deletes nothing; [`SyncMode::Mirror`] turns every
    /// orphan at the destination into ONE [`SyncStepKind::DeleteTree`]. A mode
    /// this planner does not know — only a future `norte-proto` version can
    /// add one, because the wire rejects ones it does not name — is
    /// [`SyncError::ModeNotPlanned`] and does not degrade to either of the
    /// two.
    pub mode: SyncMode,
    /// What it does with a row whose confidence is
    /// [`CompareConfidence::Unknown`](norte_proto::methods::CompareConfidence::Unknown).
    pub on_unknown: OnUnknown,
    /// Which of the two sides of a [`CompareRow`](norte_proto::methods::CompareRow)
    /// is the SOURCE.
    ///
    /// The comparison is symmetric and the synchronization is not. The
    /// frontend, which is the one that knows which pane the user was in,
    /// translated the direction ONCE; from here on it is a fact, not a
    /// convention every layer reinterprets.
    ///
    /// [`Side::Unknown`] names no side: there is no source, so there is no
    /// plan ([`SyncError::SourceSideUnknown`]). That is what an `"lft"` that
    /// made it this far would produce, and it ends the flow instead of
    /// silently serving an empty plan.
    pub source_side: Side,
    /// Does the DESTINATION provider have a trash? Decides the
    /// [`StepReversal`] of every overwrite and every deletion, and therefore
    /// how many steps the human will see marked irreversible BEFORE approving
    /// (hard rule 4).
    pub dest_has_trash: bool,
    /// And does that trash NAME what it buries? (`Provider::trash_restorable`
    /// from `norte-vfs`, which this crate cannot link: it does not depend on
    /// it.)
    ///
    /// Having a trash and being able to undo are not the same thing. A trash
    /// that answers `None` gives no recoverable destination, the journal is
    /// left without a `reversal_ref`, and the undo has to match by ORIGINAL
    /// path: over an overwrite's `trashed`+`created` pair that unearths the
    /// very file the undo just buried. So when the destination HAS a trash
    /// but does not name it, **every** step comes out
    /// [`StepReversal::Irreversible`] — not just the destructive ones:
    /// undoing a creation also goes through the trash (#65), so not even a
    /// `Copy` comes back.
    ///
    /// It is a promise from the provider, not a per-victim measurement: see
    /// `trash_restorable`'s contract. With `dest_has_trash` at `false` this
    /// field decides nothing (there is no trash to speak of).
    pub dest_trash_restorable: bool,
    /// Can the destination be written to? A read-only destination produces no
    /// steps, it produces a blocker.
    pub dest_writable: bool,
}

/// The only thing that can end a plan early.
///
/// What is deliberately NOT here: a colliding name, an unreadable entry or a
/// read-only destination. That is a [`SyncStepKind::Skip`] or a
/// [`SyncBlocker`], and the plan continues — a three-hour tree does not die
/// at leaf 40,000, same as the comparison it comes from does not.
///
/// ```
/// use norte_sync::SyncError;
/// assert_eq!(SyncError::Cancelled.to_string(), "planning cancelled");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SyncError {
    /// The Task's token fired (hard rule 3), or the comparison's feeding the
    /// flow did. Emitted ONCE and the flow ends.
    ///
    /// There is nothing to clean up: planning writes not a byte.
    #[error("planning cancelled")]
    Cancelled,
    /// [`SyncOptions::source_side`] is [`Side::Unknown`]: it names no side, so
    /// no row has a source.
    ///
    /// It is a CALLER failure, not a data one, and that is why it kills the
    /// plan instead of skipping the rows: an empty plan is approved just as
    /// easily as a full one, and having copied nothing because the mode came
    /// with a typo is exactly the silent failure ADR 0048 forbids.
    #[error("the source names no side")]
    SourceSideUnknown,
    /// A row carries a path that does not hang off the root it belonged to,
    /// so there is no `rel` to compute.
    ///
    /// With the rows `norte-compare` produces over this plan's roots it
    /// cannot happen; with others (a caller that paired the roots with the
    /// flow wrong, a provider that returns paths from another tree) it can.
    /// It ends the plan: writing under the destination with a made-up `rel`
    /// is exactly the class of failure this spec exists to not have.
    ///
    /// Also covers the case — impossible in practice, because a [`VPath`]
    /// already validated them — of a segment that does not re-validate as a
    /// [`Segment`](norte_proto::Segment).
    ///
    /// Both [`VPath`]s go in a `Box` because a plan returns this error inside
    /// a `Result` that moves per row: two inline paths make the `Err` more
    /// than 128 bytes and fatten the HAPPY path (`clippy::result_large_err`).
    ///
    /// The message uses [`VPath::display_lossy`] — never raw bytes toward a
    /// terminal (issue #21) — and that has a price whoever logs it must
    /// compensate for: NFC and NFD render the same, a space or a trailing dot
    /// is invisible, and two different invalid bytes collapse into the same
    /// `�`. The most common cause is exactly one of those, so whoever logs it
    /// must also attach the wire forms ([`VPath::to_wire`], lossless) as
    /// `tracing` fields.
    #[error("path {} does not hang off {}", .path.display_lossy(), .root.display_lossy())]
    OutsideRoot {
        /// The root under which it was expected to be found.
        root: Box<VPath>,
        /// The path that arrived.
        path: Box<VPath>,
    },
    /// A row would produce a step whose `rel` IS the ROOT
    /// ([`RelPath::is_root`]): its path is exactly the plan's root, not
    /// something under it.
    ///
    /// A step that acts on the destination's root overwrites or deletes it
    /// WHOLE, and that is the plan's most destructive target. It happens with
    /// a caller whose [`SyncOptions`] roots are deeper than the ones the
    /// feeding comparison used, and with the error row the walk emits when it
    /// cannot list its own root.
    ///
    /// The row is not skipped, the plan is ended: the roots compared against
    /// and the roots planned against have to be the same, and their not being
    /// so invalidates every `rel`, not just this one.
    #[error("root {} is not a step: a step names something UNDER it", .root.display_lossy())]
    RootIsNotAStep {
        /// The root that was about to be acted on.
        root: Box<VPath>,
    },
    /// This binary does not know how to plan the requested mode.
    ///
    /// [`SyncMode::Update`] and [`SyncMode::Mirror`] have a table; this
    /// variant is the wildcard [`SyncMode`] forces you to write because it is
    /// `#[non_exhaustive]`, and what it does is REFUSE. It is not reachable
    /// from the wire — a mode this peer does not name dies in the
    /// deserializer, which is why it carries no `#[serde(other)]` — so only a
    /// future `norte-proto` that adds a mode without this crate knowing
    /// reaches it.
    ///
    /// That this case falls into an error and not into `Update` is this
    /// variant's whole reason: `Update`'s plan is a SUBSET of `Mirror`'s, so
    /// whoever requested a new mode and got an update back would approve a
    /// plan that does not do what they asked with no way to notice — the same
    /// silent failure [`SyncError::SourceSideUnknown`] avoids. The wildcard
    /// falls on the side of planning nothing, same as [`OnUnknown`]'s falls
    /// on the side of writing nothing.
    #[error("this planner does not know how to plan mode {0:?}")]
    ModeNotPlanned(SyncMode),
    /// The row flow ended with a comparison failure that is not its
    /// cancellation. None exists today
    /// ([`CompareError`](norte_compare::CompareError) only has `Cancelled`);
    /// the variant is there so a future one does not get translated into
    /// "cancelled", which is what a wildcard would do.
    #[error("the comparison ended in failure")]
    Compare(#[source] norte_compare::CompareError),
}
