//! Las etiquetas: de un valor del plan al texto que lee un humano.
//!
//! Todo lo que aquí se devuelve sale de Fluent, y por eso vive junto y no
//! repartido por las superficies: dos frontends traduciendo el mismo veredicto
//! por su cuenta es como uno de los dos acaba enseñando el id crudo.

use norte_i18n::{Lang, t_in, ta_in};
use norte_proto::methods::{
    DestTrash, SyncBlockerKind, SyncFailureCause, SyncMode, SyncReason, SyncStepKind,
};

use super::{IncludeError, RelAnchor, StepUndo};

/// The sentence for a refusal from [`crate::sync::include_from_rows`].
///
/// Here and not in a frontend because BOTH have to say it: the terminal grew
/// the marks first and kept this translation to itself, and when the graphical
/// diff pane learnt to mark (#249) the alternative was a second copy of three
/// phrases about a destructive plan. All three REFUSE rather than narrowing: a
/// selection that shrinks by itself leaves the reader approving something else
/// — or, in the root's case, the whole tree.
///
/// ```
/// use norte_frontend::sync::{IncludeError, include_error_message};
/// use norte_i18n::Lang;
/// let msg = include_error_message(&IncludeError::RootSelected, Lang::En);
/// assert!(!msg.is_empty());
/// ```
#[must_use]
pub fn include_error_message(e: &IncludeError, lang: Lang) -> String {
    match e {
        IncludeError::TooMany { marked, max } => ta_in(
            lang,
            "msg-sync-too-many-marks",
            &[("n", &marked.to_string()), ("max", &max.to_string())],
        ),
        IncludeError::Unrooted => t_in(lang, "msg-sync-mark-outside-roots"),
        IncludeError::RootSelected => t_in(lang, "msg-sync-mark-is-the-root"),
    }
}

/// The reader's word for a [`StepUndo`].
///
/// ```
/// use norte_frontend::sync::{StepUndo, undo_label};
/// use norte_i18n::Lang;
/// assert!(!undo_label(StepUndo::LeftBehind, Lang::En).is_empty());
/// ```
#[must_use]
pub fn undo_label(undo: StepUndo, lang: Lang) -> String {
    let id = match undo {
        StepUndo::Reverts => "reverts",
        StepUndo::LeftBehind => "left-behind",
        StepUndo::Irreversible => "irreversible",
        StepUndo::Nothing => "nothing",
        StepUndo::Unclear => "unclear",
    };
    t_in(lang, &format!("sync-undo-{id}"))
}

/// The glyph for what a step DOES.
///
/// ```
/// use norte_frontend::sync::step_glyph;
/// use norte_proto::methods::SyncStepKind;
/// assert_eq!(step_glyph(SyncStepKind::Copy), '+');
/// assert_eq!(step_glyph(SyncStepKind::DeleteTree), '-');
/// ```
#[must_use]
pub fn step_glyph(kind: SyncStepKind) -> char {
    match kind {
        SyncStepKind::CreateDir => 'D',
        SyncStepKind::Copy => '+',
        SyncStepKind::Overwrite => '#',
        SyncStepKind::DeleteTree => '-',
        SyncStepKind::Skip => '.',
        // A class from a newer daemon: it has a name this build does not know,
        // so it does not borrow another class's mark.
        _ => '?',
    }
}

/// The reader's word for what a step does.
///
/// ```
/// use norte_frontend::sync::step_label;
/// use norte_i18n::Lang;
/// use norte_proto::methods::SyncStepKind;
/// assert!(!step_label(SyncStepKind::Overwrite, Lang::En).is_empty());
/// ```
#[must_use]
pub fn step_label(kind: SyncStepKind, lang: Lang) -> String {
    let id = match kind {
        SyncStepKind::CreateDir => "create-dir",
        SyncStepKind::Copy => "copy",
        SyncStepKind::Overwrite => "overwrite",
        SyncStepKind::DeleteTree => "delete-tree",
        SyncStepKind::Skip => "skip",
        _ => "unknown",
    };
    t_in(lang, &format!("sync-step-{id}"))
}

/// The reader's word for why a step is a `Skip` or is irreversible.
///
/// [`SyncReason::NoTrashOnTarget`] covers BOTH "there is no trash" and "there
/// is one that cannot say where it put things", so its message must not claim
/// the first — the wire token deliberately does not distinguish them.
///
/// ```
/// use norte_frontend::sync::reason_label;
/// use norte_i18n::Lang;
/// use norte_proto::methods::SyncReason;
/// assert!(!reason_label(SyncReason::NoTrashOnTarget, Lang::En).is_empty());
/// ```
#[must_use]
pub fn reason_label(reason: SyncReason, lang: Lang) -> String {
    let id = match reason {
        SyncReason::AmbiguousSource => "ambiguous-source",
        SyncReason::UnknownConfidence => "unknown-confidence",
        SyncReason::Unreadable => "unreadable",
        SyncReason::NoTrashOnTarget => "no-trash-on-target",
        SyncReason::NonInjectivePairing => "non-injective-pairing",
        _ => "unknown",
    };
    t_in(lang, &format!("sync-reason-{id}"))
}

/// The reader's word for why a plan cannot run.
///
/// ```
/// use norte_frontend::sync::blocker_label;
/// use norte_i18n::Lang;
/// use norte_proto::methods::SyncBlockerKind;
/// assert!(!blocker_label(SyncBlockerKind::DestReadOnly, Lang::En).is_empty());
/// ```
#[must_use]
pub fn blocker_label(kind: SyncBlockerKind, lang: Lang) -> String {
    let id = match kind {
        SyncBlockerKind::AmbiguousDest => "ambiguous-dest",
        SyncBlockerKind::OverlapDetected => "overlap-detected",
        SyncBlockerKind::DestReadOnly => "dest-read-only",
        SyncBlockerKind::DirTooLarge => "dir-too-large",
        SyncBlockerKind::TypeMismatchDir => "type-mismatch-dir",
        _ => "unknown",
    };
    t_in(lang, &format!("sync-blocker-{id}"))
}

/// El calificador de un [`RelAnchor`], o `None` cuando la ruta cuelga del
/// ORIGEN y no hace falta decir nada.
///
/// Vive aquí y no en cada painter por lo mismo que [`failure_cause_label`]:
/// la revisión de rama de C2 lo encontró transcrito a mano en tres sitios
/// —`norte-tui/src/ui.rs` y dos veces en `norte-gui/src/sync_view.rs`— y a la
/// CLI se le había olvidado, que es la forma en que esta clase de duplicado
/// se nota tarde (rust MAJOR-3, encoding MAJOR-1).
///
/// `None` para [`RelAnchor::Source`] a propósito: un calificador vacío
/// pintado igualmente mete un espacio en banda, y esta pantalla ya tiene un
/// problema con los separadores que un nombre puede llevar dentro.
///
/// ```
/// use norte_frontend::sync::{RelAnchor, anchor_label};
/// use norte_i18n::Lang;
/// assert!(anchor_label(RelAnchor::Source, Lang::En).is_none());
/// assert_ne!(
///     anchor_label(RelAnchor::Dest, Lang::En),
///     anchor_label(RelAnchor::Either, Lang::En),
/// );
/// ```
#[must_use]
pub fn anchor_label(anchor: RelAnchor, lang: Lang) -> Option<String> {
    match anchor {
        RelAnchor::Source => None,
        RelAnchor::Dest => Some(t_in(lang, "sync-anchor-dest")),
        RelAnchor::Either => Some(t_in(lang, "sync-anchor-either")),
    }
}

/// The reader's parenthetical for [`crate::sync::StepCells::dest_rel_twin`] /
/// [`crate::sync::FailureCells::dest_rel_twin`] (#192), shaped exactly like
/// [`anchor_label`] so a painter drops it in the same way: `None` when there
/// is nothing to say, `Some` otherwise.
///
/// A hostile badge would be a LIE about the name — `café.txt` (NFC) and
/// `café.txt` (NFD) are both valid UTF-8 and neither is hostile — so this is
/// a separate sentence, never a badge folded into [`crate::sync::RelDisplay::hostile`].
///
/// ```
/// use norte_frontend::sync::dest_twin_label;
/// use norte_i18n::Lang;
/// assert!(dest_twin_label(false, Lang::En).is_none());
/// assert!(dest_twin_label(true, Lang::En).is_some());
/// ```
#[must_use]
pub fn dest_twin_label(twin: bool, lang: Lang) -> Option<String> {
    twin.then(|| t_in(lang, "sync-dest-twin"))
}

/// El nombre de un [`SyncMode`], que la cabecera pinta.
///
/// El `_` NO cae a «update»: un modo que este build no sabe nombrar tiene que
/// decirlo, porque la diferencia entre los dos que sí conoce es si BORRA.
/// Estaba escrito dos veces en esta rama, una por frontend (rust MAJOR-3).
///
/// ```
/// use norte_frontend::sync::mode_label;
/// use norte_i18n::Lang;
/// use norte_proto::methods::SyncMode;
/// assert_ne!(
///     mode_label(SyncMode::Update, Lang::En),
///     mode_label(SyncMode::Mirror, Lang::En),
/// );
/// ```
#[must_use]
pub fn mode_label(mode: SyncMode, lang: Lang) -> String {
    match mode {
        SyncMode::Update => t_in(lang, "sync-mode-update"),
        SyncMode::Mirror => t_in(lang, "sync-mode-mirror"),
        _ => t_in(lang, "sync-mode-unknown"),
    }
}

/// The reader's word for why ONE step of an applied plan did not happen
/// ([`norte_proto::methods::SyncFailure::cause`]).
///
/// Shared, and not a `match` per frontend, for the reason the rest of this
/// module is shared: the CLI wrote this table first (phase A) and the GUI
/// needed the same five sentences (phase C2). Two copies of a table whose
/// `_` arm exists precisely to survive a NEWER daemon is two chances for one
/// of them to name a cause the other calls "unrecognised".
///
/// The `_` arm covers both `Unknown` (the decoder's `#[serde(other)]`, which
/// the core never emits) and the enum's `#[non_exhaustive]`: a wire that
/// grows a sixth cause lands there instead of failing to compile.
///
/// ```
/// use norte_frontend::sync::failure_cause_label;
/// use norte_i18n::Lang;
/// use norte_proto::methods::SyncFailureCause;
/// assert_ne!(
///     failure_cause_label(SyncFailureCause::Denied, Lang::En),
///     failure_cause_label(SyncFailureCause::Io, Lang::En),
/// );
/// ```
#[must_use]
pub fn failure_cause_label(cause: SyncFailureCause, lang: Lang) -> String {
    let id = match cause {
        SyncFailureCause::Conflict => "conflict",
        SyncFailureCause::Denied => "denied",
        SyncFailureCause::IllegalName => "illegal-name",
        SyncFailureCause::Io => "io",
        _ => "unknown",
    };
    t_in(lang, &format!("sync-cause-{id}"))
}

/// What the destination's trash means for the human, in words.
///
/// [`crate::sync::UndoOutlook`] answers "does the undo give it back", and for both bad
/// trashes that answer is no — but they are not the same situation and a
/// dialog must not print one sentence for both: with a [`DestTrash::Opaque`]
/// trash (macOS, Windows) what was replaced is sitting in the system trash and
/// can be fished out by hand, and with [`DestTrash::Absent`] it is gone. That
/// distinction is the reason the wire carries three values instead of a
/// boolean, and this is where it reaches the reader.
///
/// ```
/// use norte_frontend::sync::trash_label;
/// use norte_i18n::Lang;
/// use norte_proto::methods::DestTrash;
/// assert_ne!(
///     trash_label(DestTrash::Opaque, Lang::En),
///     trash_label(DestTrash::Absent, Lang::En),
///     "una papelera del sistema y ninguna papelera no son la misma frase"
/// );
/// ```
#[must_use]
pub fn trash_label(dest_trash: DestTrash, lang: Lang) -> String {
    let id = match dest_trash {
        DestTrash::Restorable => "restorable",
        DestTrash::Opaque => "opaque",
        DestTrash::Absent => "absent",
        _ => "unknown",
    };
    t_in(lang, &format!("sync-trash-{id}"))
}
