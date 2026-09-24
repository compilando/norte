//! LIST policy for a confirmation modal: how many items get painted before
//! summarizing the rest, and how each one is sanitized.
//!
//! It used to live duplicated in the GUI (#103 T10 brings it up here): a
//! batch copy, move or delete is confirmed over a LIST of names, and that
//! list is attack surface — a hostile name smuggling in a `\n`, a bidi
//! override or a separator could FORGE a fake entry and make the human
//! approve something they did not read. There is a single rule for both
//! frontends: one path PER LINE, always through [`display_name_with`]
//! (`crate::display_name_with`), and the hostile-flag badge.

use norte_proto::{Segment, VPath};

/// How many items a modal lists before summarizing the rest as "... and N
/// more".
///
/// It is a READABILITY cap, not a security one: the final summary never
/// hides how many are left out (a batch of 500 cannot look like one of 10).
pub const MODAL_ITEM_LIMIT: usize = 10;

/// AI rename plan (M4-IA) pairs visible at once in the plan modal (a scroll
/// window, audit MAJOR-3: the WHOLE plan is reviewable by scrolling —
/// without a window, the tail of a long plan would apply without ever being
/// seen). Single source for the render, the modal's height (TUI) and the
/// scroll clamp in both frontends.
pub const AI_RENAME_PAIR_LIMIT: usize = 5;

/// Semantic search hits (M4-IA-2) visible at once in the hits modal (a
/// cursor scroll window, molded on [`AI_RENAME_PAIR_LIMIT`]). Single source
/// for the render, the modal's height (TUI) and the cursor clamp in both
/// frontends. The `k` REQUESTED from the server is [`SEMANTIC_K`]: larger
/// than this window (the rest is left to a scroll).
pub const SEMANTIC_HIT_LIMIT: usize = 10;

/// `k` both frontends request from `index.search_semantic` (M4-IA-2): larger
/// than the modal's window ([`SEMANTIC_HIT_LIMIT`] — the rest is left to a
/// scroll) and well below the server's contractual ceiling
/// (`INDEX_SEMANTIC_MAX_K` = 100, which also clips on its own). Single
/// source for TUI and GUI: requesting different `k`s would make the SAME
/// query return different results per frontend.
///
/// ```
/// assert!(norte_frontend::SEMANTIC_K as usize > norte_frontend::SEMANTIC_HIT_LIMIT);
/// assert!(norte_frontend::SEMANTIC_K <= norte_proto::methods::INDEX_SEMANTIC_MAX_K);
/// ```
pub const SEMANTIC_K: u32 = 20;

/// Cap on the pairs a frontend ACCEPTS from `ai.rename_plan` (M4-IA, an
/// ingestion belt): the engine bounds legitimate plans MUCH lower (the
/// basenames of ONE directory), so a plan that exceeds it gives away a
/// hostile/N+1 daemon inflating the response — it is rejected AS A WHOLE
/// (same message as a tampered plan), never chunked nor reviewed for "what
/// fits".
pub const MAX_AI_PLAN_ENTRIES: usize = 256;

/// Validates ALL of the plan's pairs as [`Segment`] (a fail-loud belt, audit
/// MAJOR-2, shared by TUI and GUI): a well-formed plan from the engine NEVER
/// carries an invalid segment (the daemon validated them when building it),
/// so ONE rejection here gives away a hostile/broken daemon — `None` aborts
/// the WHOLE batch, never a silent skip that applies "the rest" of a
/// tampered plan.
///
/// PURE on purpose: testable without a backend (audit MINOR-6e).
///
/// ```
/// use norte_proto::methods::AiRenameEntry;
/// let ok = AiRenameEntry { from: "a.txt".into(), to: "b.txt".into() };
/// assert!(norte_frontend::validate_ai_plan(std::slice::from_ref(&ok)).is_some());
/// let evil = AiRenameEntry { from: "c.txt".into(), to: "../evil".into() };
/// // ONE invalid pair brings down the WHOLE plan, even if the rest is legitimate.
/// assert!(norte_frontend::validate_ai_plan(&[ok, evil]).is_none());
/// ```
#[must_use]
pub fn validate_ai_plan(
    entries: &[norte_proto::methods::AiRenameEntry],
) -> Option<Vec<(Segment, Segment)>> {
    validate_ai_plan_in(entries, None)
}

/// Like [`validate_ai_plan`], and additionally requires that each `from`
/// EXISTS among `names` when it is passed.
///
/// The belt exists to survive a hostile or broken daemon, and it used to be
/// looser than the validator it defends against (#275): `norte_core::ai`
/// checks three more things that were not looked at here.
///
/// - **`!` is not a name**: it is the file-as-directory marker (ADR 0018),
///   and letting it through turns a rename into a traversal into a file.
/// - **Neither is `\`**: it is a separator on Windows, so `..\evil` is a
///   traversal `Segment` does not see because it only looks at `/`. Having
///   `norte-vfs-local::native_path` reject it afterward does not fix it: it
///   is an error far from its cause, and the belt is here precisely so the
///   error surfaces where it can be explained.
/// - **`from` has to exist** where it is going to be applied. Without that
///   check a tampered plan can rename something the reader is not looking
///   at. `None` in `names` means "this caller does not have the listing in
///   front of it", not "it does not matter": both frontends do have it and
///   pass it.
///
/// ONE invalid pair brings down the WHOLE plan, as before: never a silent
/// skip that applies "the rest" of a tampered plan.
///
/// ```
/// use norte_proto::methods::AiRenameEntry;
/// use norte_frontend::validate_ai_plan_in;
///
/// let e = AiRenameEntry { from: "a.txt".into(), to: "b.txt".into() };
/// assert!(validate_ai_plan_in(std::slice::from_ref(&e), Some(&[b"a.txt".to_vec()])).is_some());
/// // The same plan over a directory where `a.txt` is not: it is rejected.
/// assert!(validate_ai_plan_in(std::slice::from_ref(&e), Some(&[b"other.txt".to_vec()])).is_none());
/// ```
#[must_use]
pub fn validate_ai_plan_in(
    entries: &[norte_proto::methods::AiRenameEntry],
    names: Option<&[Vec<u8>]>,
) -> Option<Vec<(Segment, Segment)>> {
    entries
        .iter()
        .map(|e| {
            let from = Segment::new(e.from.as_bytes().to_vec()).ok()?;
            let to = Segment::new(e.to.as_bytes().to_vec()).ok()?;
            if from.as_bytes() == b"!" || to.as_bytes() == b"!" {
                return None;
            }
            if to.as_bytes().contains(&b'\\') {
                return None;
            }
            if let Some(names) = names
                && !names.iter().any(|n| n.as_slice() == from.as_bytes())
            {
                return None;
            }
            Some((from, to))
        })
        .collect()
}

/// The SAME pairs from [`validate_ai_plan`], now in the shape
/// `fs.rename_batch_plan` and `fs.rename_batch` ask for (spec §17, ADR 0042).
///
/// The single AI-plan → batch-pairs converter for TUI and GUI: both
/// frontends send exactly the same INTENT, and so the core answers them the
/// same `plan_hash`. `None` under the same fail-loud criterion as
/// [`validate_ai_plan`] — an invalid segment brings down the WHOLE batch.
///
/// ```
/// use norte_proto::methods::AiRenameEntry;
/// let e = AiRenameEntry { from: "ep1.mkv".into(), to: "ep01.mkv".into() };
/// let pairs = norte_frontend::rename_pairs(std::slice::from_ref(&e)).expect("valid");
/// assert_eq!(pairs[0].from.as_bytes(), b"ep1.mkv");
/// assert_eq!(pairs[0].to.as_bytes(), b"ep01.mkv");
/// ```
#[must_use]
pub fn rename_pairs(
    entries: &[norte_proto::methods::AiRenameEntry],
) -> Option<Vec<norte_proto::methods::RenamePair>> {
    rename_pairs_in(entries, None)
}

/// Like [`rename_pairs`], with the existence check from
/// [`validate_ai_plan_in`].
#[must_use]
pub fn rename_pairs_in(
    entries: &[norte_proto::methods::AiRenameEntry],
    names: Option<&[Vec<u8>]>,
) -> Option<Vec<norte_proto::methods::RenamePair>> {
    Some(
        validate_ai_plan_in(entries, names)?
            .into_iter()
            .map(|(from, to)| norte_proto::methods::RenamePair { from, to })
            .collect(),
    )
}

/// Batch plan collisions painted before summarizing the rest (molded on
/// [`AI_RENAME_PAIR_LIMIT`]). A READABILITY cap, not a security one: the
/// final summary never hides how many are left out, and no collision makes
/// applicable a plan the core marked as not applicable.
pub const RENAME_COLLISION_LIMIT: usize = 5;

/// Cells the WHOLE line of a collision is bounded to.
///
/// The budget belongs to the LINE, not the name, because what has to be
/// prevented is the silent right-side truncation each frontend does when
/// the line does not fit (the TUI box's width — a 60-column floor, 56 of
/// interior once borders and padding are subtracted —, the GUI's div
/// `.truncate()`). What the ALREADY-TRANSLATED prefix takes up — mark, pair
/// index and verdict — is subtracted from the budget, and what is left over
/// is what the name gets: a long verdict (or a locale with long labels)
/// shortens the name instead of pushing it out of the box unmarked.
const COLLISION_LINE_COLS: usize = 56;

/// Floor of cells for the offending name: no matter how long the verdict is,
/// the name never gets whittled down to nothing. If the prefix eats the
/// budget, what gets truncated is the line — marked by `middle_ellipsis` —
/// not the name until it disappears.
const COLLISION_NAME_MIN_COLS: usize = 12;

/// Fluent key for the VERDICT of a batch collision (spec §17): single source
/// for TUI and GUI — the frontend paints the label, never infers the
/// verdict.
///
/// [`RenameCollisionKind::Unknown`](norte_proto::methods::RenameCollisionKind)
/// (the class for an N+1 daemon) has its own
/// generic key: it degrades ONE line, never the whole modal.
///
/// ```
/// use norte_proto::methods::RenameCollisionKind as K;
/// assert_eq!(
///     norte_frontend::collision_kind_key(K::External),
///     "modal-rename-batch-collision-external",
/// );
/// // A verdict this binary does not know still gets a label.
/// assert_eq!(
///     norte_frontend::collision_kind_key(K::Unknown),
///     "modal-rename-batch-collision-unknown",
/// );
/// ```
#[must_use]
pub fn collision_kind_key(kind: norte_proto::methods::RenameCollisionKind) -> &'static str {
    use norte_proto::methods::RenameCollisionKind as K;
    match kind {
        K::Internal => "modal-rename-batch-collision-internal",
        K::External => "modal-rename-batch-collision-external",
        K::AbsentSource => "modal-rename-batch-collision-absent-source",
        K::AmbiguousSource => "modal-rename-batch-collision-ambiguous-source",
        // Any future class falls here (the enum is `non_exhaustive` and
        // `Unknown` is its deserialization fallback): "rejected, reason I
        // don't understand" is honest; guessing would not be.
        _ => "modal-rename-batch-collision-unknown",
    }
}

/// What point the batch plan (`fs.rename_batch_plan`, spec §17) is at, that
// TODO(translation): review — this line reads as leftover text from a stale
// doc-comment merge in the original Spanish; translated as-is.
/// A line of a verdict's DETAIL, IN PARTS.
///
/// In parts and not in a single string (#273): the earlier form composed
/// `✗ { $n }. { $kind }: { $name }` here, and `display_name` masks neither
/// the `✗`, nor the digits, nor the `.`, nor the `:` — they are all legal in
/// a name — so a file named `✗ 4. already exists: other.txt` produced
/// `✗ 3. already exists: ✗ 4. already exists: other.txt`. This is the exact
/// shape the corpus's `cause_join_spoof` fixture exists to forbid: the cause
/// and the destination's spelling are kept OUT OF BAND. It gets worse
/// because `norte-i18n` calls `set_use_isolating(false)`, so Fluent does not
/// put FSI/PDI around the placeable, and a name with strong RTL letters
/// reorders the `✗`, the index and the `:` inside the line.
///
/// Each frontend places them however it can: the window with one element
/// per part, the terminal putting the name on its own line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetailPart {
    /// The planner needed temporary steps. It carries nothing from anyone.
    Temp {
        /// How many.
        count: usize,
    },
    /// A specific collision.
    Collision {
        /// 1-based index of the pair, if it really points to a row.
        index: Option<usize>,
        /// Fluent key of the verdict.
        kind_key: &'static str,
        /// The name, ALREADY sanitized and truncated. It is the only thing a
        /// third party controls, and that is why it travels alone.
        name: String,
        /// The name differs from the real one.
        hostile: bool,
    },
    /// The collisions that do not fit.
    More {
        /// How many are shown.
        shown: usize,
        /// How many there are.
        total: usize,
        /// One of the HIDDEN ones has a hostile name.
        hostile: bool,
    },
}

/// what the AI rename modal needs in order to be able to confirm.
// TODO(translation): review — this summary line reads as truncated in the
// original Spanish (missing a leading clause); translated as-is.
///
/// Three states and not an `Option`, because "has not answered yet" and "is
/// not going to answer" cannot be shown to the human the same way: the first
/// resolves on its own, the second does not, and a "checking…" label that
/// never advances is a lie shaped like a spinner.
///
/// The type is SHARED by every surface, and with it the whole verdict
/// presentation policy ([`Self::status_key`], [`Self::detail_parts`]): every
/// surface paints names an attacker controls, and one that derives its own
/// is exactly how the sanitizing gets lost in it without anyone noticing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchPlan {
    /// Requested from the core and in flight: the modal opens and fills in.
    Pending,
    /// The core answered.
    Ready(Box<norte_proto::methods::FsRenameBatchPlanResult>),
    /// The core could not answer (or could not even be asked). The specific
    /// reason went to the status bar; here all that is known is that there
    /// is NO plan, and without a plan there is no approved `plan_hash` to
    /// send.
    Failed,
}

impl BatchPlan {
    /// The plan, if there is one. `None` in [`Self::Pending`] and
    /// [`Self::Failed`].
    #[must_use]
    pub fn ready(&self) -> Option<&norte_proto::methods::FsRenameBatchPlanResult> {
        match self {
            Self::Ready(p) => Some(p),
            _ => None,
        }
    }

    /// Whether confirming can do anything: a plan is needed and the CORE
    /// has to have marked it applicable. Single source for the gate in both
    /// frontends — the TUI mutes its confirm commands and the GUI its key,
    /// and both ask here.
    ///
    /// Reads `executable`, NEVER `collisions.is_empty()`: the field is
    /// normative (see its rustdoc in `norte_proto`) and a future verdict can
    /// stop a plan with no offending name to list.
    ///
    /// ```
    /// use norte_frontend::BatchPlan;
    /// assert!(!BatchPlan::Pending.confirmable());
    /// assert!(!BatchPlan::Failed.confirmable());
    /// ```
    #[must_use]
    pub fn confirmable(&self) -> bool {
        self.ready().is_some_and(|p| p.executable)
    }

    /// Fluent key of the STATUS, the line that goes ABOVE the modal (next to
    /// the dir, before the pairs): a modal taller than the terminal gets cut
    /// off at the bottom, and of the whole body this is the line that must
    /// not be lost.
    ///
    /// ```
    /// use norte_frontend::BatchPlan;
    /// assert_eq!(BatchPlan::Pending.status_key(), "modal-rename-batch-pending");
    /// assert_eq!(BatchPlan::Failed.status_key(), "modal-rename-batch-unchecked");
    /// ```
    #[must_use]
    pub fn status_key(&self) -> &'static str {
        match self {
            Self::Pending => "modal-rename-batch-pending",
            Self::Failed => "modal-rename-batch-unchecked",
            Self::Ready(p) if p.executable => "modal-rename-batch-applicable",
            Self::Ready(_) => "modal-rename-batch-not-applicable",
        }
    }

    /// How many of the plan's steps are planner MACHINERY (temporaries that
    /// break a cycle). They are counted, the name is never shown: a
    /// `.norte-rename-…` is nothing the human asked for, and painting it
    /// among their pairs would make them think norte is going to leave that
    /// name on their disk.
    #[must_use]
    pub fn temp_steps(&self) -> usize {
        self.ready()
            .map_or(0, |p| p.steps.iter().filter(|s| s.temp).count())
    }

    /// Renames this batch is REALLY going to do: the steps that are not
    /// machinery. It is not `pairs.len()`: the planner drops null pairs
    /// (`from == to`), so counting what was REQUESTED would promise the
    /// human more renames than the core committed to doing.
    #[must_use]
    pub fn real_steps(&self) -> usize {
        self.ready()
            .map_or(0, |p| p.steps.iter().filter(|s| !s.temp).count())
    }

    /// The verdict's DETAIL, which goes BELOW the pairs: the planner's
    /// machinery (its count) and the collisions, ONE PER LINE, up to
    /// [`RENAME_COLLISION_LIMIT`] plus a summary of the ones that do not fit.
    ///
    /// Each line comes back with its HOSTILE flag: each frontend puts on the
    /// badge (the TUI uses ASCII `!`, the GUI `⚠`), but the sanitizing —
    /// who gets masked, who gets shortened and where — is decided here just
    /// once.
    ///
    /// `pair_count` is how many pairs the request has, and it is used for
    /// ONE thing: a `pair_index` that falls outside it is not painted. A
    /// hostile daemon answering "pair 41" about a plan of 3 cannot make the
    /// modal point at a row that does not exist; the line loses the index
    /// and keeps the verdict.
    #[must_use]
    pub fn detail_parts(&self, pair_count: usize, lang: norte_i18n::Lang) -> Vec<DetailPart> {
        let mut out = Vec::new();
        let Some(plan) = self.ready() else {
            return out;
        };
        let temps = self.temp_steps();
        if temps > 0 {
            out.push(DetailPart::Temp { count: temps });
        }
        let shown = plan.collisions.len().min(RENAME_COLLISION_LIMIT);
        for c in plan.collisions.iter().take(shown) {
            let (name, hostile) = crate::display_name(c.name.as_bytes());
            // The name's budget comes from what the already-translated
            // prefix MEASURES, not from a constant guessed against the
            // shortest label.
            let kind_key = collision_kind_key(c.kind);
            let prefix = crate::cells(&norte_i18n::ta_in(
                lang,
                "modal-rename-batch-collision-prefix",
                &[
                    ("n", &c.pair_index.saturating_add(1).to_string()),
                    ("kind", &norte_i18n::t_in(lang, kind_key)),
                ],
            ));
            let budget = COLLISION_LINE_COLS
                .saturating_sub(prefix)
                .max(COLLISION_NAME_MIN_COLS);
            out.push(DetailPart::Collision {
                // The pair index is 1-based, like the `from`'s label, and it
                // only travels if it really points to a row of the request:
                // a hostile daemon answering "pair 41" about a plan of 3
                // cannot make the modal point at a row that does not exist.
                index: ((c.pair_index as usize) < pair_count)
                    .then(|| c.pair_index.saturating_add(1) as usize),
                kind_key,
                name: crate::middle_ellipsis(&name, budget),
                hostile,
            });
        }
        if plan.collisions.len() > shown {
            // What is hidden does not slip through clean: a HIDDEN collision
            // with a hostile name marks the summary.
            let hostile = plan
                .collisions
                .iter()
                .skip(shown)
                .any(|c| crate::display_name(c.name.as_bytes()).1);
            out.push(DetailPart::More {
                shown,
                total: plan.collisions.len(),
                hostile,
            });
        }
        out
    }

    /// How many lines [`Self::detail_parts`] paints, without building them.
    /// The TUI modal's height is recomputed on EVERY frame; interpolating
    /// Fluent and allocating a `Vec<String>` just to count would be
    /// per-frame work.
    #[must_use]
    pub fn detail_line_count(&self) -> usize {
        let Some(plan) = self.ready() else {
            return 0;
        };
        let shown = plan.collisions.len().min(RENAME_COLLISION_LIMIT);
        // TWO per collision since #273: the cause and the name do not share
        // a line, so a name cannot forge another one's cause.
        usize::from(self.temp_steps() > 0) + shown * 2 + usize::from(plan.collisions.len() > shown)
    }
}

/// INGESTION belt for the semantic hits (M4-IA-2, parity with the AI plan's
/// belt, shared by TUI and GUI): a COMPLIANT daemon never exceeds
/// [`norte_proto::methods::INDEX_SEMANTIC_MAX_K`] (the server clips `k` to
/// that contractual ceiling) nor emits non-finite scores (the engine
/// filters them) — exceeding the ceiling or slipping in a NaN/∞ gives away
/// a hostile/N+1 daemon inflating or poisoning the response. `None` = a
/// BLOCK rejection (zero hits painted, never a silent clip); `Some` returns
/// the hits intact.
///
/// PURE on purpose: testable without a backend, like [`validate_ai_plan`].
///
/// ```
/// use norte_proto::VPath;
/// use norte_proto::methods::SemanticHit;
/// let ok = SemanticHit { path: VPath::parse("mem:///a").unwrap(), score: 0.9 };
/// assert!(norte_frontend::validate_semantic_hits(vec![ok.clone()]).is_some());
/// // ONE non-finite score brings down the WHOLE response, even if the rest is legitimate.
/// let evil = SemanticHit { path: VPath::parse("mem:///b").unwrap(), score: f64::NAN };
/// assert!(norte_frontend::validate_semantic_hits(vec![ok, evil]).is_none());
/// ```
#[must_use]
pub fn validate_semantic_hits(
    hits: Vec<norte_proto::methods::SemanticHit>,
) -> Option<Vec<norte_proto::methods::SemanticHit>> {
    (hits.len() <= norte_proto::methods::INDEX_SEMANTIC_MAX_K as usize
        && hits.iter().all(|h| h.score.is_finite()))
    .then_some(hits)
}

/// Whether a plan can be APPROVED: the core accepts it **and** the reader
/// has reached the end.
///
/// Two questions, and neither can answer the other. Whether the plan is
/// executable is known by the core and not the reader; whether the reader
/// has seen it is not known by the core. An approval is a signature, and the
/// signature of something that has not been read is not an approval — with
/// a plan of two hundred renames, the ones that matter can be at row one
/// hundred eighty.
///
/// It used to live only in the window: the terminal let you approve without
/// scrolling down, so the same question had two answers on the surface
/// where getting it wrong costs the most (ADR 0077). Here there is one
/// answer.
///
/// `seen` is the HIGH watermark — how far it has ever been scrolled — not
/// the current position: scrolling back up does not un-read what was
/// already read.
///
/// ```
/// use norte_frontend::approval_ready;
///
/// // Seen in full and the core accepts it.
/// assert!(approval_ready(true, 20, 20));
/// // Seen in full but the core rejects it: no hash to send.
/// assert!(!approval_ready(false, 20, 20));
/// // The core accepts it and the reader stopped halfway.
/// assert!(!approval_ready(true, 10, 20));
/// // An empty plan is seen by definition.
/// assert!(approval_ready(true, 0, 0));
/// ```
#[must_use]
pub fn approval_ready(plan_confirmable: bool, seen: usize, total: usize) -> bool {
    plan_confirmable && seen >= total
}

/// Would any of the paths that are NOT shown paint as altered?
///
/// A visible path's badge says "what you read is not the bytes that are
/// there". That cannot be said about what got truncated — it is not there
/// to look at — but it CAN be said that something like that is out there,
/// which is what decides whether it is worth expanding before approving.
/// `skip` is how many are shown.
///
/// For both frontends because it is the same question about the same paths
/// and on the surface where getting it wrong costs the most: the terminal
/// always said so and the window did not (ADR 0077).
///
/// ```
/// use norte_proto::VPath;
/// use norte_frontend::overflow_hostile;
///
/// let clean = VPath::parse("file:///casa/a.txt").unwrap();
/// let weird = VPath::parse("file:///casa/a%E2%80%AE.txt").unwrap();
///
/// // The hostile one is SHOWN: the badge is its own, not the summary's.
/// assert!(!overflow_hostile(&[weird.clone(), clean.clone()], 2));
/// // The hostile one is left out: the summary says so.
/// assert!(overflow_hostile(&[clean.clone(), weird], 1));
/// // Nothing truncated, nothing to say.
/// assert!(!overflow_hostile(&[clean], 9));
/// ```
#[must_use]
pub fn overflow_hostile(paths: &[VPath], skip: usize) -> bool {
    paths.iter().skip(skip).any(|p| crate::path_display(p).1)
}

/// Would this ALREADY-REDACTED text paint as altered?
///
/// For paths that arrive as TEXT and not as [`VPath`] — the ones from an
/// approval request, which the daemon sends redacted because the original
/// bytes do not leave there.
///
/// Two reasons to flag, and the second is the one that gets forgotten:
/// comparing against the original detects nothing, because the daemon
/// already ran the bytes through its `display_lossy` and controls, bidi
/// overrides and invalid bytes are ALREADY `U+FFFD`. That character IS the
/// signal that what is read is not what is there; what was there cannot be
/// recovered, but it can be said that it is not faithful.
///
/// ```
/// use norte_frontend::redacted_hostile;
/// assert!(!redacted_hostile("casa/a.txt"));
/// // What the daemon already substituted.
/// assert!(redacted_hostile("casa/a\u{FFFD}.txt"));
/// // And what arrives whole and has to be masked here.
/// assert!(redacted_hostile("casa/a\u{200B}.txt"));
/// ```
#[must_use]
pub fn redacted_hostile(text: &str) -> bool {
    norte_encoding::mask_terminal_hazards(text) != text || text.contains('\u{FFFD}')
}

/// [`overflow_hostile`] for paths that arrive as redacted text.
///
/// ```
/// use norte_frontend::overflow_hostile_redacted;
/// let paths = ["a.txt".to_owned(), "b\u{FFFD}.txt".to_owned()];
/// assert!(overflow_hostile_redacted(&paths, 1), "the weird one is left out");
/// assert!(!overflow_hostile_redacted(&paths, 2), "both are shown");
/// ```
#[must_use]
pub fn overflow_hostile_redacted(paths: &[String], skip: usize) -> bool {
    paths.iter().skip(skip).any(|p| redacted_hostile(p))
}

/// Default badge for [`item_lines`]: the warning the GUI's modal already
/// used. Frontends with their own badge (the TUI uses ASCII `!`, for
/// terminals that do not render `⚠`) pass theirs to [`item_lines_with`] —
/// the crate does not choose a badge, it only guarantees the hostile flag
/// gets MARKED.
const DEFAULT_BADGE: &str = "⚠";

/// Up to [`MODAL_ITEM_LIMIT`] sanitized names (one line per item, never two
/// paths on the same one); if there are more, a final localized line with
/// how many are left out.
///
/// Paints the NAME of each item, not the whole path: in a batch they all
/// share a directory (the pane's) and the destination goes on its own line.
///
/// ```
/// use norte_proto::VPath;
/// let items: Vec<VPath> = (0..12)
///     .map(|i| VPath::parse(&format!("mem:///f{i}")).unwrap())
///     .collect();
/// let lines = norte_frontend::item_lines(&items);
/// assert_eq!(lines.len(), norte_frontend::MODAL_ITEM_LIMIT + 1);
/// assert_eq!(lines[0], "f0");
/// // The last one SUMMARIZES the ones that do not fit: 12 - 10 = 2.
/// assert!(lines.last().unwrap().contains('2'));
/// ```
#[must_use]
pub fn item_lines(items: &[VPath]) -> Vec<String> {
    item_lines_with(items, DEFAULT_BADGE, None)
}

/// [`item_lines`] with the frontend's badge and the source pane's name
/// REINTERPRETATION (#57): the dialog has to paint the SAME text the user
/// navigated by — with a pane in cp866, confirming a delete by showing the
/// lossy `�����` instead of `Папка` would be asking about something else.
///
/// ```
/// use norte_encoding::NameEncoding;
/// use norte_proto::VPath;
/// let items = vec![VPath::parse("mem:///CAF%90.TXT").unwrap()];
/// let lines = norte_frontend::item_lines_with(&items, "!", Some(NameEncoding::Cp437));
/// // Reinterpreted AND flagged: the text painted is not the bytes.
/// assert_eq!(lines, vec!["! CAFÉ.TXT".to_string()]);
/// ```
#[must_use]
pub fn item_lines_with(
    items: &[VPath],
    badge: &str,
    reinterpret: Option<norte_encoding::NameEncoding>,
) -> Vec<String> {
    let mut lines: Vec<String> = items
        .iter()
        .take(MODAL_ITEM_LIMIT)
        .map(|p| {
            let bytes = p.file_name().map_or(&b""[..], Segment::as_bytes);
            let (name, hostile) = crate::display_name_with(bytes, reinterpret);
            if hostile {
                format!("{badge} {name}")
            } else {
                name
            }
        })
        .collect();
    if items.len() > MODAL_ITEM_LIMIT {
        let n = (items.len() - MODAL_ITEM_LIMIT).to_string();
        // Key inherited from the GUI (GUI-e T1): now shared by the TUI —
        // renaming it would not change the text and would break the
        // translations.
        lines.push(norte_i18n::ta("gui-modal-more", &[("n", n.as_str())]));
    }
    lines
}

/// A line of a rename batch's report (`fs.rename_batch_report`).
///
/// Phrases travel translated; paths are NOT turned into text here, because
/// each frontend paints them with its own sanitizing and badge. What IS
/// decided here is that a path goes ALONE on its line: embedded in a
/// phrase, another path can impersonate it (#273).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportLine {
    /// A phrase from the report, already translated. It carries nothing
    /// from anyone.
    Phrase(String),
    /// A path to look for: the name that whatever got stuck halfway now
    /// carries.
    Path(norte_proto::VPath),
}

/// `true` if the batch left nothing to look for or finish off.
///
/// ```
/// use norte_proto::methods::FsRenameBatchReportResult;
/// let r = FsRenameBatchReportResult {
///     applied: 2, rolled_back: 0, failed_pair: None, stuck: None,
///     uncertain: None, compensations_lost: 0,
/// };
/// assert!(norte_frontend::batch_report_is_clean(&r));
/// ```
#[must_use]
pub fn batch_report_is_clean(r: &norte_proto::methods::FsRenameBatchReportResult) -> bool {
    r.stuck.is_none()
        && r.uncertain.is_none()
        && r.failed_pair.is_none()
        && r.compensations_lost == 0
        && r.rolled_back == 0
}

/// The body of a batch's report: what was applied, what could not be
/// reverted, and WHAT IT IS NOW CALLED, whatever got stuck halfway.
///
/// `stuck` and `uncertain` can BOTH show up, and they say different things:
/// one is "could not be reverted", the other "not known whether it took
/// effect". Lost compensations are the only warning that a session undo
/// will stop there.
#[must_use]
pub fn batch_report_lines(
    r: &norte_proto::methods::FsRenameBatchReportResult,
    lang: norte_i18n::Lang,
) -> Vec<ReportLine> {
    let phrase = |key: &str| ReportLine::Phrase(norte_i18n::t_in(lang, key));
    let mut body = vec![ReportLine::Phrase(norte_i18n::ta_in(
        lang,
        "modal-batch-summary",
        &[
            ("applied", &r.applied.to_string()),
            ("back", &r.rolled_back.to_string()),
        ],
    ))];
    if let Some(step) = &r.stuck {
        body.push(phrase("modal-batch-stuck"));
        body.push(ReportLine::Path(step.to.clone()));
        body.push(phrase(if step.journalled {
            "modal-batch-stuck-journalled"
        } else {
            "modal-batch-stuck-unjournalled"
        }));
    }
    if let Some(step) = &r.uncertain {
        body.push(phrase("modal-batch-uncertain"));
        body.push(ReportLine::Path(step.to.clone()));
    }
    if r.compensations_lost > 0 {
        body.push(ReportLine::Phrase(norte_i18n::ta_in(
            lang,
            "modal-batch-compensations-lost",
            &[("n", &r.compensations_lost.to_string())],
        )));
    }
    body
}

/// `true` if the undo reverted EVERYTHING it was supposed to.
///
/// What got skipped counts as not-clean: an irreversible entry or a
/// creation that stays because the destination has no trash are things that
/// did NOT come back, and a report that hid them would say the tree is as
/// it was.
#[must_use]
pub fn undo_report_is_clean(r: &norte_proto::methods::PolicyUndoReportResult) -> bool {
    r.blocked.is_none()
        && r.batch_stuck.is_none()
        && r.compensations_lost == 0
        && r.denied_total == 0
        && r.skipped_irreversible == 0
        && r.skipped_created_no_trash == 0
}

/// The body of an undo's report: what came back and what did not. The same
/// lines in the window and in the terminal.
#[must_use]
pub fn undo_report_lines(
    r: &norte_proto::methods::PolicyUndoReportResult,
    lang: norte_i18n::Lang,
) -> Vec<ReportLine> {
    let phrase = |text: String| ReportLine::Phrase(text);
    let mut body = vec![phrase(norte_i18n::ta_in(
        lang,
        "modal-undo-summary",
        &[
            ("undone", &r.undone.to_string()),
            ("skipped", &r.skipped_irreversible.to_string()),
        ],
    ))];
    if r.skipped_created_no_trash > 0 {
        body.push(phrase(norte_i18n::ta_in(
            lang,
            "modal-undo-left-in-place",
            &[("n", &r.skipped_created_no_trash.to_string())],
        )));
    }
    if let Some(b) = &r.blocked {
        // The `seq` is an OPAQUE reference: it is used to CITE the entry
        // against the server's journal, not to interpret it here.
        body.push(phrase(norte_i18n::ta_in(
            lang,
            "modal-undo-blocked",
            &[
                ("seq", &b.seq.to_string()),
                (
                    "error",
                    &norte_i18n::t_in(lang, crate::error::error_key(&b.error)),
                ),
            ],
        )));
    }
    if let Some(step) = &r.batch_stuck {
        body.push(phrase(norte_i18n::t_in(lang, "modal-undo-batch-stuck")));
        body.push(ReportLine::Path(step.to.clone()));
    }
    if r.compensations_lost > 0 {
        body.push(phrase(norte_i18n::ta_in(
            lang,
            "modal-batch-compensations-lost",
            &[("n", &r.compensations_lost.to_string())],
        )));
    }
    if r.denied_total > 0 {
        body.push(phrase(norte_i18n::ta_in(
            lang,
            "modal-undo-denied",
            &[("n", &r.denied_total.to_string())],
        )));
    }
    body
}

#[cfg(test)]
mod batch_report_tests {
    use super::{ReportLine, batch_report_is_clean, batch_report_lines};
    use norte_proto::VPath;
    use norte_proto::methods::{FsRenameBatchReportResult, RenameStuckStep};

    fn step(to: &str) -> RenameStuckStep {
        RenameStuckStep {
            from: VPath::parse("mem:///d/.norte-rename-1").expect("vpath"),
            to: VPath::parse(to).expect("vpath"),
            pair_index: 0,
            error: norte_proto::Error::Io { retryable: false },
            journalled: true,
            still_applied: 1,
        }
    }

    /// With `stuck` AND `uncertain`, both come out, each with its own
    /// phrase and its own path on its own line.
    #[test]
    fn stuck_and_uncertain_both_show_each_path_on_its_own_line() {
        let r = FsRenameBatchReportResult {
            applied: 1,
            rolled_back: 1,
            failed_pair: Some(1),
            stuck: Some(step("mem:///d/a")),
            uncertain: Some(step("mem:///d/b")),
            compensations_lost: 2,
        };
        assert!(!batch_report_is_clean(&r));
        let lines = batch_report_lines(&r, norte_i18n::Lang::En);
        let paths: Vec<_> = lines
            .iter()
            .filter_map(|l| match l {
                ReportLine::Path(p) => Some(p.clone()),
                ReportLine::Phrase(_) => None,
            })
            .collect();
        assert_eq!(
            paths,
            vec![
                VPath::parse("mem:///d/a").expect("vpath"),
                VPath::parse("mem:///d/b").expect("vpath")
            ]
        );
        assert!(
            lines
                .iter()
                .any(|l| matches!(l, ReportLine::Phrase(t) if t.contains("compensations"))),
            "the lost compensations are stated: {lines:?}"
        );
    }
}

#[cfg(test)]
mod ai_plan_tests {
    use super::validate_ai_plan;
    use norte_proto::methods::AiRenameEntry;

    pub(super) fn e(from: &str, to: &str) -> AiRenameEntry {
        AiRenameEntry {
            from: from.into(),
            to: to.into(),
        }
    }

    /// Audit MAJOR-2 (fail-loud): ONE invalid pair — a `..` traversal, an
    /// embedded separator or an empty name — brings down the WHOLE plan
    /// (`None`), never a silent skip that applies "the rest" of a plan
    /// tampered with by a hostile/broken daemon.
    #[test]
    fn one_invalid_pair_brings_down_the_whole_plan() {
        assert!(validate_ai_plan(&[e("a", "b"), e("c", "..")]).is_none());
        assert!(validate_ai_plan(&[e("a/b", "c"), e("d", "e")]).is_none());
        assert!(validate_ai_plan(&[e("", "x")]).is_none());
        // #275: what the engine's validator rejects and the belt used to let
        // through. `!` is the file-as-directory marker (ADR 0018) and `\\`
        // is a separator on Windows, i.e. a traversal `Segment` does not see
        // because it only looks at `/`.
        assert!(validate_ai_plan(&[e("a.txt", "!")]).is_none());
        assert!(validate_ai_plan(&[e("!", "a.txt")]).is_none());
        assert!(validate_ai_plan(&[e("a.txt", "..\\evil")]).is_none());
        assert!(validate_ai_plan(&[e("a.txt", "sub\\x")]).is_none());
        assert!(validate_ai_plan(&[e("ok", "also-ok"), e("x", "a/b")]).is_none());
    }

    /// A well-formed plan keeps order and length, exact bytes.
    #[test]
    fn a_valid_plan_keeps_order_and_length() {
        let pairs = validate_ai_plan(&[e("a", "b"), e("c", "d")]).expect("valid plan");
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].0.as_bytes(), b"a");
        assert_eq!(pairs[0].1.as_bytes(), b"b");
        assert_eq!(pairs[1].0.as_bytes(), b"c");
        assert_eq!(pairs[1].1.as_bytes(), b"d");
    }

    /// The empty plan is valid (frontends do not queue anything with it).
    #[test]
    fn an_empty_plan_is_valid() {
        assert_eq!(validate_ai_plan(&[]).expect("empty is valid").len(), 0);
    }

    /// Rule 1, at the seam: `AiRenameEntry` travels as `String` because the
    /// core fail-loud rejects a dir with non-UTF8 names BEFORE calling the
    /// provider, so its `from_utf8_lossy` is the identity. That held while
    /// the value was only PAINTED; now it is the `from` of a real rename,
    /// and what has to be pinned down is what happens if that premise broke:
    /// a `U+FFFD` is a perfectly legal `Segment`, so the pair travels as-is
    /// and the core answers `AbsentSource` — the batch executes NOTHING.
    /// It fails closed, it never renames the wrong file.
    #[test]
    fn a_name_with_lossy_residue_travels_as_is_and_dies_in_the_core() {
        let pairs = super::rename_pairs(&[e("caf\u{FFFD}.txt", "cafe.txt")]).expect("segment");
        assert_eq!(
            pairs[0].from.as_bytes(),
            "caf\u{FFFD}.txt".as_bytes(),
            "the frontend does not invent bytes: it sends what it was given",
        );
    }
}

#[cfg(test)]
mod batch_plan_tests {
    use super::{BatchPlan, COLLISION_LINE_COLS, RENAME_COLLISION_LIMIT};
    use norte_i18n::Lang;
    use norte_proto::Segment;
    use norte_proto::methods::{
        FsRenameBatchPlanResult, PlanHash, RenameCollision, RenameCollisionKind, RenameStep,
    };

    fn seg(b: &[u8]) -> Segment {
        Segment::new(b.to_vec()).expect("segment")
    }

    fn ready(
        collisions: Vec<RenameCollision>,
        steps: Vec<RenameStep>,
        executable: bool,
    ) -> BatchPlan {
        BatchPlan::Ready(Box::new(FsRenameBatchPlanResult {
            steps,
            collisions,
            executable,
            plan_hash: PlanHash::parse(&"0".repeat(64)).expect("64 hex"),
        }))
    }

    fn collision(pair_index: u32, name: &[u8], kind: RenameCollisionKind) -> RenameCollision {
        RenameCollision {
            pair_index,
            name: seg(name),
            kind,
        }
    }

    /// The three states say DIFFERENT things and only one lets you confirm.
    /// Without this, "in flight" and "could not be checked" would paint the
    /// same, and the second would be a spinner that never advances.
    #[test]
    fn the_three_states_are_not_confused() {
        assert!(!BatchPlan::Pending.confirmable());
        assert!(!BatchPlan::Failed.confirmable());
        assert!(!ready(vec![], vec![], false).confirmable());
        assert!(ready(vec![], vec![], true).confirmable());
        let keys = [
            BatchPlan::Pending.status_key(),
            BatchPlan::Failed.status_key(),
            ready(vec![], vec![], true).status_key(),
            ready(vec![], vec![], false).status_key(),
        ];
        for (i, a) in keys.iter().enumerate() {
            for b in &keys[i + 1..] {
                assert_ne!(a, b, "two states with the same label: {a}");
            }
        }
        // With no plan there is no detail to paint (not even a ghost line).
        assert!(
            BatchPlan::Pending
                .detail_parts(1, norte_i18n::active())
                .is_empty()
        );
        assert!(
            BatchPlan::Failed
                .detail_parts(1, norte_i18n::active())
                .is_empty()
        );
    }

    /// The VERDICT is the actionable part and goes before the name, in
    /// BOTH locales: a translator reordering `{ $name }` ahead of
    /// `{ $kind }` would leave the verdict at the mercy of silent
    /// right-side truncation.
    #[test]
    fn the_verdict_precedes_the_name_in_both_locales() {
        let plan = ready(
            vec![collision(0, b"zzzzz.txt", RenameCollisionKind::External)],
            vec![],
            false,
        );
        for lang in [Lang::En, Lang::Es] {
            let _ = norte_i18n::force(lang);
            let parts = plan.detail_parts(1, lang);
            let super::DetailPart::Collision { kind_key, name, .. } = &parts[0] else {
                panic!("{lang:?}: the part is a collision: {parts:?}");
            };
            assert_eq!(
                *kind_key,
                super::collision_kind_key(RenameCollisionKind::External)
            );
            assert_eq!(name, "zzzzz.txt");
            // The verdict goes in ITS OWN part and the name in its own: the
            // order is decided by the frontend, and neither can truncate
            // the other.
            assert!(
                !name.contains(&norte_i18n::t_in(lang, kind_key)),
                "{lang:?}: the name does not carry the verdict inside it"
            );
        }
        let _ = norte_i18n::force(Lang::En);
    }

    /// The budget belongs to the LINE: with each locale's longest label and
    /// a mile-long name, the whole line still fits — what gets shortened is
    /// the name, and the truncation is MARKED.
    #[test]
    fn the_whole_line_fits_its_budget_in_both_locales() {
        let long_name = vec![b'x'; 300];
        for lang in [Lang::En, Lang::Es] {
            let _ = norte_i18n::force(lang);
            for kind in [
                RenameCollisionKind::Internal,
                RenameCollisionKind::External,
                RenameCollisionKind::AbsentSource,
                RenameCollisionKind::AmbiguousSource,
                RenameCollisionKind::Unknown,
            ] {
                let plan = ready(vec![collision(0, &long_name, kind)], vec![], false);
                let parts = plan.detail_parts(1, lang);
                let super::DetailPart::Collision { kind_key, name, .. } = &parts[0] else {
                    panic!("{lang:?} {kind:?}: {parts:?}");
                };
                // The budget is still that of the WHOLE line: cause plus
                // name. What gets shortened is the name, and it is MARKED.
                let cause = norte_i18n::ta_in(
                    lang,
                    "modal-rename-batch-collision-prefix",
                    &[("n", "1"), ("kind", &norte_i18n::t_in(lang, kind_key))],
                );
                let cells = crate::cells(&cause) + crate::cells(name);
                assert!(
                    cells <= COLLISION_LINE_COLS,
                    "{lang:?} {kind:?}: {cells} cells > {COLLISION_LINE_COLS}"
                );
                assert!(name.contains('…'), "the truncation is MARKED: {name}");
            }
        }
        let _ = norte_i18n::force(Lang::En);
    }

    /// Two names that only differ by the TAIL cannot render identically: if
    /// the budget eats them, the middle ellipsis keeps the tail. (Mutation
    /// control: swapping `middle_ellipsis` for a right-side truncation
    /// breaks this test.)
    #[test]
    fn two_names_twin_by_the_tail_do_not_render_identical() {
        let _ = norte_i18n::force(Lang::Es);
        let v2 = b"invoice-2024-january-final-revised-v2.pdf";
        let v3 = b"invoice-2024-january-final-revised-v3.pdf";
        let render = |n: &[u8]| {
            ready(
                vec![collision(0, n, RenameCollisionKind::Unknown)],
                vec![],
                false,
            )
            .detail_parts(1, norte_i18n::active())
            .remove(0)
        };
        assert_ne!(
            render(v2),
            render(v3),
            "the tail distinguishes, and survives"
        );
        let _ = norte_i18n::force(Lang::En);
    }

    /// A `pair_index` that does not point at any row of the request is
    /// DROPPED: a hostile daemon cannot make the modal point at a pair that
    /// does not exist. The verdict and the name are still there.
    #[test]
    fn an_out_of_range_index_does_not_point_at_a_nonexistent_row() {
        let _ = norte_i18n::force(Lang::En);
        let plan = ready(
            vec![collision(u32::MAX, b"z.txt", RenameCollisionKind::Internal)],
            vec![],
            false,
        );
        let parts = plan.detail_parts(3, norte_i18n::active());
        let super::DetailPart::Collision { index, name, .. } = &parts[0] else {
            panic!("{parts:?}");
        };
        assert_eq!(*index, None, "an impossible index does not travel");
        assert_eq!(name, "z.txt", "the name is still there");
        // With the real request behind it, the index DOES travel 1-based.
        let plan = ready(
            vec![collision(1, b"z.txt", RenameCollisionKind::Internal)],
            vec![],
            false,
        );
        let super::DetailPart::Collision { index, .. } =
            &plan.detail_parts(3, norte_i18n::active())[0]
        else {
            panic!("collision");
        };
        assert_eq!(*index, Some(2));
    }

    /// The collision cap is respected, the summary does not hide how many
    /// are left out, and `detail_line_count` counts EXACTLY what is painted
    /// (the TUI modal's height is computed with it).
    #[test]
    fn the_collision_cap_and_the_counter_agree() {
        let _ = norte_i18n::force(Lang::En);
        for n in [
            0usize,
            1,
            RENAME_COLLISION_LIMIT,
            RENAME_COLLISION_LIMIT + 3,
        ] {
            let cs: Vec<_> = (0..n)
                .map(|i| {
                    collision(
                        u32::try_from(i).expect("fits"),
                        format!("f{i}.txt").as_bytes(),
                        RenameCollisionKind::Internal,
                    )
                })
                .collect();
            let plan = ready(
                cs,
                vec![RenameStep {
                    from: seg(b"a"),
                    to: seg(b".norte-rename-0"),
                    temp: true,
                }],
                false,
            );
            let parts = plan.detail_parts(n.max(1), norte_i18n::active());
            // Each collision paints TWO lines (cause and name); the
            // temporaries notice one, and the summary another.
            let painted: usize = parts
                .iter()
                .map(|p| usize::from(matches!(p, super::DetailPart::Collision { .. })) + 1)
                .sum();
            assert_eq!(painted, plan.detail_line_count(), "n={n}");
            assert!(
                parts.len() <= 1 + RENAME_COLLISION_LIMIT + 1,
                "n={n}: {parts:?}"
            );
            if n > RENAME_COLLISION_LIMIT {
                let super::DetailPart::More { total, .. } = parts.last().expect("summary") else {
                    panic!("n={n}: the last one is the summary: {parts:?}");
                };
                assert_eq!(*total, n, "n={n}");
            }
            // A temporary gets COUNTED, never named.
            assert!(!parts.iter().any(|p| matches!(
                p,
                super::DetailPart::Collision { name, .. } if name.contains(".norte-rename-")
            )));
        }
    }

    /// Every name in the canonical corpus: no line carries a hazard, none
    /// splits in two, and the masked one comes MARKED so the frontend can
    /// put its badge on it.
    #[test]
    fn corpus_sweep_on_the_offending_name() {
        let _ = norte_i18n::force(Lang::En);
        for fixture in norte_testkit::corpus::hostile_names() {
            let plan = ready(
                vec![collision(0, &fixture.bytes, RenameCollisionKind::External)],
                vec![],
                false,
            );
            let parts = plan.detail_parts(1, norte_i18n::active());
            assert_eq!(parts.len(), 1, "corpus {}: {parts:?}", fixture.id);
            let super::DetailPart::Collision { name, hostile, .. } = &parts[0] else {
                panic!("corpus {}: {parts:?}", fixture.id);
            };
            assert!(
                !name.chars().any(norte_encoding::is_terminal_hazard),
                "corpus {}: live hazard: {name:?}",
                fixture.id
            );
            // Not a single newline: a name cannot forge a line of the list.
            assert!(!name.contains('\n'), "corpus {}: {name:?}", fixture.id);
            assert_eq!(
                *hostile,
                crate::display_name(&fixture.bytes).1,
                "corpus {}: the hostile flag has to reach the frontend",
                fixture.id
            );
        }
    }
}

#[cfg(test)]
mod semantic_hits_tests {
    use super::validate_semantic_hits;
    use norte_proto::VPath;
    use norte_proto::methods::{INDEX_SEMANTIC_MAX_K, SemanticHit};

    fn hits(n: usize) -> Vec<SemanticHit> {
        (1..=n)
            .map(|i| SemanticHit {
                path: VPath::parse(&format!("mem:///d/f{i}")).expect("valid wire"),
                score: 0.5,
            })
            .collect()
    }

    /// M4-IA-2 (parity with the plan's belt, IA-1): the belt accepts up to
    /// the server's contractual ceiling (`INDEX_SEMANTIC_MAX_K` — a
    /// compliant daemon never exceeds it) with the hits INTACT, and rejects
    /// AS A WHOLE an inflated response (hostile/N+1 daemon) — never a
    /// silent clip.
    #[test]
    fn the_exact_ceiling_passes_and_one_more_is_rejected_as_a_whole() {
        let max = usize::try_from(INDEX_SEMANTIC_MAX_K).expect("small ceiling");
        let ok = validate_semantic_hits(hits(max));
        assert_eq!(
            ok.as_ref().map(Vec::len),
            Some(max),
            "the exact ceiling passes intact"
        );
        assert!(
            validate_semantic_hits(hits(max + 1)).is_none(),
            "one more = rejected as a whole"
        );
    }

    /// ONE non-finite score (NaN/∞ — the engine filters them, so only a
    /// hostile/broken daemon emits them) brings down the WHOLE response,
    /// even if the rest is legitimate.
    #[test]
    fn a_non_finite_score_brings_down_the_whole_response() {
        for evil in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut batch = hits(3);
            batch[1].score = evil;
            assert!(
                validate_semantic_hits(batch).is_none(),
                "score {evil} must reject as a whole"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use norte_proto::VPath;

    /// A modal's list is cut at [`super::MODAL_ITEM_LIMIT`] and SUMMARIZES
    /// the rest in one final localized line — it never paints 500 paths, nor,
    /// worse, hides the ones that do not fit.
    #[test]
    fn the_modal_lists_the_first_items_and_summarises_the_rest() {
        let many: Vec<VPath> = (0..20)
            .map(|i| VPath::parse(&format!("mem:///f{i}")).unwrap())
            .collect();
        let lines = crate::item_lines(&many);
        assert_eq!(lines.len(), crate::MODAL_ITEM_LIMIT + 1);
        assert!(
            lines
                .last()
                .unwrap()
                .contains(&(20 - crate::MODAL_ITEM_LIMIT).to_string())
        );
    }

    /// A batch that fits whole carries NO summary line (not even a "and 0
    /// more").
    #[test]
    fn a_batch_that_fits_has_no_summary_line() {
        let few: Vec<VPath> = (0..crate::MODAL_ITEM_LIMIT)
            .map(|i| VPath::parse(&format!("mem:///f{i}")).unwrap())
            .collect();
        assert_eq!(crate::item_lines(&few).len(), crate::MODAL_ITEM_LIMIT);
    }

    /// The whole hostile corpus: not one line leaks a raw hazard, not one
    /// line contains a newline (one path per line, always) — the list
    /// cannot be FORGED from a name.
    #[test]
    fn item_lines_never_leak_raw_hazards_nor_forge_a_line() {
        for fixture in norte_testkit::corpus::hostile_names() {
            let Ok(seg) = norte_proto::Segment::new(fixture.bytes.clone()) else {
                continue; // a name that cannot be represented as a segment does not reach here
            };
            let p = VPath::root(norte_proto::Scheme::new("mem").unwrap(), None).join(seg);
            let lines = crate::item_lines(std::slice::from_ref(&p));
            assert_eq!(lines.len(), 1, "{}: one line per item", fixture.id);
            let line = &lines[0];
            assert!(
                !line.chars().any(norte_encoding::is_terminal_hazard),
                "{}: raw hazard in {line:?}",
                fixture.id,
            );
            assert!(
                !line.contains('\n'),
                "{}: a name cannot forge a line: {line:?}",
                fixture.id,
            );
        }
    }
}

#[cfg(test)]
mod belt_tests {
    use super::ai_plan_tests::e;
    use super::validate_ai_plan_in;

    /// The `from` has to exist WHERE it is going to be applied (#275).
    ///
    /// Without this, a tampered plan renames something the reader is not
    /// looking at: the screen approving it shows one directory and the
    /// operation touches another file in the same one.
    #[test]
    fn a_from_not_in_the_listing_brings_down_the_plan() {
        let names = vec![b"a.txt".to_vec(), b"b.txt".to_vec()];
        assert!(validate_ai_plan_in(&[e("a.txt", "c.txt")], Some(&names)).is_some());
        assert!(validate_ai_plan_in(&[e("z.txt", "c.txt")], Some(&names)).is_none());
        // And a single bad one brings down the whole batch, like the rest
        // of the belt.
        assert!(
            validate_ai_plan_in(&[e("a.txt", "c.txt"), e("z.txt", "d.txt")], Some(&names))
                .is_none()
        );
    }

    /// With no listing in front, only the SHAPE is checked, nothing else:
    /// `None` means "this caller does not have it", not "it does not
    /// matter".
    #[test]
    fn with_no_listing_only_the_shape_is_checked() {
        assert!(validate_ai_plan_in(&[e("z.txt", "c.txt")], None).is_some());
        assert!(validate_ai_plan_in(&[e("z.txt", "..")], None).is_none());
    }
}
