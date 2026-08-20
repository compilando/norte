//! Dónde ANCLA cada paso: si su ruta relativa habla del origen, del destino o
//! de los dos.
//!
//! No es un detalle de pintado. Un paso que borra en el destino y otro que
//! copia desde el origen enseñan rutas que se PARECEN, y confundirlas es
//! confundir qué árbol se toca.

use norte_i18n::{Lang, t_in};
use norte_proto::methods::{
    RelPath, Side, SyncBlocker, SyncBlockerKind, SyncReason, SyncStep, SyncStepKind,
};

/// Which root a step's [`SyncStep::rel`] hangs from.
///
/// A pane has two columns and the wire has no side field, so this is what
/// stops a `DeleteTree` from being painted under the source. See
/// [`anchor_of`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelAnchor {
    /// The source root (and, through `dest_rel`, the destination too).
    Source,
    /// The destination root only: there is no source path to name.
    Dest,
    /// Could be either, and the step does not say. Paint it in neither
    /// column, or in both — but do not claim one.
    Either,
}

/// Which root `step`'s `rel` is measured against.
///
/// Normative on the wire ([`SyncStep::rel`]): "almost always the source". The
/// two exceptions are a [`SyncStepKind::DeleteTree`], which only ever speaks
/// about the destination, and the [`SyncReason::Unreadable`] `Skip` — which is
/// emitted for an unreadable listing on EITHER side and carries nothing to
/// tell them apart. Answering `Source` for it would paint a destination path
/// under the source column, which is why the third variant exists.
///
/// ```
/// use norte_frontend::sync::{RelAnchor, anchor_of};
/// use norte_proto::methods::SyncStepKind;
/// # use norte_proto::methods::{CompareConfidence, CompareCriterion, RelPath, StepReversal,
/// #     SyncStep};
/// let del = SyncStep {
///     id: 1,
///     kind: SyncStepKind::DeleteTree,
///     rel: RelPath::parse_wire("old").expect("rel"),
///     dest_rel: None,
///     size: None,
///     criterion: CompareCriterion::Presence,
///     confidence: CompareConfidence::Certain,
///     reversal: Some(StepReversal::RestoreTrash),
///     reason: None,
/// };
/// assert_eq!(anchor_of(&del), RelAnchor::Dest);
/// ```
#[must_use]
pub fn anchor_of(step: &SyncStep) -> RelAnchor {
    anchor_for(step.kind, step.dest_rel.is_some(), step.reason)
}

/// The rule itself, stated ONCE (#208): [`anchor_of`] answers it for a step and
/// [`render_failure`] for a failure row, and since 0.42.0 both have the same
/// input — the class.
///
/// Before that bump a failure row carried no class, so `render_failure` had to
/// guess from the only evidence left (`dest_rel` present ⟹ `rel` is the
/// source's half) and answered `Either` for everything else. Two rules that
/// start out agreeing do not stay agreeing, which is the whole reason this is
/// one function.
///
/// `reason` is `None` for a failure row: the wire does not carry one, and the
/// `Skip` arm falls through to `Either`, which is what "we cannot tell" means.
pub(super) fn anchor_for(
    kind: SyncStepKind,
    has_dest_rel: bool,
    reason: Option<SyncReason>,
) -> RelAnchor {
    match kind {
        // FIRST, before the `dest_rel` test: a `DeleteTree` only ever speaks
        // about the destination, whatever else it carries.
        SyncStepKind::DeleteTree => RelAnchor::Dest,
        // The step names a destination path explicitly, so whatever the class
        // is, `rel` is the source's half of the pair.
        _ if has_dest_rel => RelAnchor::Source,
        SyncStepKind::CreateDir | SyncStepKind::Copy | SyncStepKind::Overwrite => RelAnchor::Source,
        // A collision or a confidence the caller asked to skip is a fact about
        // the SOURCE. Everything else a `Skip` can say — an unreadable listing
        // (emitted for either side, with nothing to tell them apart), a reason
        // this build cannot name, no reason at all — could be the
        // destination's.
        SyncStepKind::Skip => match reason {
            Some(SyncReason::AmbiguousSource | SyncReason::UnknownConfidence) => RelAnchor::Source,
            _ => RelAnchor::Either,
        },
        // A class a newer daemon named. `DeleteTree` proves a destination-only
        // class is a shape this family HAS, so painting an unknown one under
        // the source column would put a path in a tree it may not be in.
        _ => RelAnchor::Either,
    }
}

/// Which root a [`SyncBlocker`]'s `rel` hangs from — the third member of the
/// family [`anchor_of`] and [`render_failure`] already form (#189).
///
/// [`SyncBlocker::side`] is normative when it is present: the wire's
/// convention is [`Side::Left`] for the source and [`Side::Right`] for the
/// destination, always, independent of which pane launched the plan. Three of
/// the four named kinds name a DESTINATION path by definition even without a
/// `side` — [`SyncBlockerKind::AmbiguousDest`], [`SyncBlockerKind::DestReadOnly`]
/// and [`SyncBlockerKind::DirTooLarge`] are never about the source.
/// [`SyncBlockerKind::OverlapDetected`] is about both roots at once, so it
/// answers [`RelAnchor::Either`] rather than pick one it cannot justify.
/// [`SyncBlockerKind::TypeMismatchDir`] carries `side` on the wire ALWAYS
/// (normative, see its rustdoc), so its fall-through here is defensive, not
/// reachable against a conforming daemon.
///
/// Without this, the obvious code for a pane that lists blockers is
/// `rel_display(&b.rel, view.source_encoding)`, which reproduces #152 against
/// three paths that are never the source's spelling.
///
/// ```
/// use norte_frontend::sync::{RelAnchor, blocker_anchor};
/// use norte_proto::methods::{RelPath, Side, SyncBlocker, SyncBlockerKind};
/// let dest_ro = SyncBlocker {
///     rel: RelPath::parse_wire("").expect("rel"),
///     kind: SyncBlockerKind::DestReadOnly,
///     side: None,
/// };
/// assert_eq!(blocker_anchor(&dest_ro), RelAnchor::Dest);
///
/// // `side` wins when present, even against a kind that would otherwise
/// // derive the opposite anchor.
/// let overlap_from_the_left = SyncBlocker {
///     rel: RelPath::parse_wire("shared").expect("rel"),
///     kind: SyncBlockerKind::OverlapDetected,
///     side: Some(Side::Left),
/// };
/// assert_eq!(blocker_anchor(&overlap_from_the_left), RelAnchor::Source);
/// ```
#[must_use]
pub fn blocker_anchor(blocker: &SyncBlocker) -> RelAnchor {
    match blocker.side {
        Some(Side::Right) => RelAnchor::Dest,
        Some(Side::Left) => RelAnchor::Source,
        // `Side::Unknown` is the decoder's `#[serde(other)]` fallback, which
        // the core never emits — treated the same as absent, since neither
        // root is provably named.
        Some(Side::Unknown) | None => match blocker.kind {
            SyncBlockerKind::AmbiguousDest
            | SyncBlockerKind::DestReadOnly
            | SyncBlockerKind::DirTooLarge => RelAnchor::Dest,
            _ => RelAnchor::Either,
        },
    }
}

/// A relative path ready to paint: masked text, the original bytes, and the
/// flag that says the two differ (rule 1, spec §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelDisplay {
    /// Display form: lossy, and safe to paint. NEVER empty for the root
    /// —which is what a whole-tree blocker such as
    /// [`SyncBlockerKind::DestReadOnly`] carries— when built through
    /// [`rel_display_or_root`]: a pane says "the whole tree" there rather
    /// than nothing. [`rel_display`] itself has no [`Lang`] to say it in and
    /// stays empty; it is the wrapper's job (#193).
    pub text: String,
    /// The path's ORIGINAL bytes, `/`-joined. A theme matches an extension
    /// against these, never against [`RelDisplay::text`].
    pub raw: Vec<u8>,
    /// Some segment was altered to be painted: badge it.
    pub hostile: bool,
}

/// One relative path, masked exactly the way a listing masks a name.
///
/// ```
/// use norte_frontend::sync::rel_display;
/// use norte_proto::methods::RelPath;
/// let d = rel_display(&RelPath::parse_wire("sub/a.txt").expect("rel"), None);
/// assert_eq!(d.text, "sub/a.txt");
/// assert!(!d.hostile);
/// ```
#[must_use]
pub fn rel_display(rel: &RelPath, reinterpret: Option<norte_encoding::NameEncoding>) -> RelDisplay {
    let mut text = String::new();
    let mut raw: Vec<u8> = Vec::new();
    let mut hostile = false;
    for (i, seg) in rel.segments().iter().enumerate() {
        if i > 0 {
            text.push('/');
            raw.push(b'/');
        }
        let bytes = seg.as_bytes();
        let (masked, h) = crate::display_name_with(bytes, reinterpret);
        hostile |= h;
        text.push_str(&masked);
        raw.extend_from_slice(bytes);
    }
    RelDisplay { text, raw, hostile }
}

/// The same as [`rel_display`], except the root reads as the localized
/// "whole tree" sentence instead of an empty string — the contract
/// [`RelDisplay::text`] documents and no painter honoured (#193).
///
/// Lives here, once, and not per painter: a root [`RelDisplay`] is not a
/// [`SyncBlocker`] concept, it is a [`RelPath::is_root`] one, so whichever
/// surface eventually paints a whole-tree blocker gets the same sentence
/// without writing it again. An EMPTY string painted in a pane reads as
/// "there is no row here", the opposite of what the wire is saying — a
/// [`SyncBlockerKind::DestReadOnly`] names the destination root precisely
/// because there IS something to say about every path under it.
///
/// [`RelDisplay::raw`] and [`RelDisplay::hostile`] are untouched: the root has
/// no bytes to badge, so `raw` stays empty and `hostile` stays `false` — the
/// sentence is this module's words, not a reading of the name.
///
/// ```
/// use norte_frontend::sync::rel_display_or_root;
/// use norte_i18n::Lang;
/// use norte_proto::methods::RelPath;
/// let whole = rel_display_or_root(&RelPath::parse_wire("").expect("rel"), None, Lang::En);
/// assert!(!whole.text.is_empty());
/// assert!(whole.raw.is_empty(), "no hay bytes que decir");
/// assert!(!whole.hostile, "la raíz no es un nombre hostil");
///
/// // Cualquier otra ruta se comporta exactamente como `rel_display`.
/// let named = rel_display_or_root(&RelPath::parse_wire("a.txt").expect("rel"), None, Lang::En);
/// assert_eq!(named.text, "a.txt");
/// ```
#[must_use]
pub fn rel_display_or_root(
    rel: &RelPath,
    reinterpret: Option<norte_encoding::NameEncoding>,
    lang: Lang,
) -> RelDisplay {
    let display = rel_display(rel, reinterpret);
    if rel.is_root() {
        RelDisplay {
            text: t_in(lang, "sync-rel-root"),
            ..display
        }
    } else {
        display
    }
}
