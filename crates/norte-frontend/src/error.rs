//! Vocabulary of protocol error CATEGORIES, shared by both frontends (#158,
//! phase C1 review — MAJOR-3).
//!
//! It used to live in `norte-tui`, so the GUI could not reach it and ended up
//! interpolating the ENGLISH `Display` of [`Error`] into otherwise localized
//! sentences. It is the same argument that moved
//! [`compare::CompareView`](crate::compare::CompareView) here: an error is
//! said the same way on both surfaces, or whichever falls behind lies in the
//! reader's own language.
//!
//! Two reasons for it to be a CATEGORY and not the `Display`:
//!
//! * the `Display` is not translated, and Fluent has no chance to translate
//!   it;
//! * several variants interpolate PEER data — `HostKeyUnknown` carries a
//!   host, algorithm and fingerprint; `LimitExceeded` its limit — and an
//!   arbitrary host in a bar is a bidi/control vector. The stable key
//!   discards them by pattern, so there is nothing to sanitize.

use norte_i18n::{Lang, t_in};
use norte_proto::Error;

/// The STABLE Fluent key for a protocol [`Error`]'s CATEGORY (spec §17.7,
/// #20). It is the basis of [`error_category`] and also the vocabulary Lua
/// scripts see (`nil, key` — M4 Lua): the script compares against stable
/// keys, never against localized text. Fields carrying detail (host, `rule`,
/// retryable…) are DISCARDED by pattern: `PolicyDenied` does not expose the
/// concrete rule (closed vocabulary); `HostKeyUnknown`/`Mismatch` do not leak
/// the host (also, a `Display` with an arbitrary host would be a bidi/control
/// vector in the bar). A future category (`Unknown`, an N-1 client) falls
/// back to `err-unknown`.
///
/// ```
/// use norte_frontend::error::error_key;
/// assert_eq!(error_key(&norte_proto::Error::PermissionDenied), "err-permission-denied");
/// // The peer's detail is discarded: the key is the SAME for every host.
/// assert_eq!(error_key(&norte_proto::Error::NotFound), "err-not-found");
/// ```
#[must_use]
pub fn error_key(e: &Error) -> &'static str {
    use norte_proto::{ConflictKind, RootOverlap};
    match e {
        Error::NotFound => "err-not-found",
        Error::PermissionDenied => "err-permission-denied",
        Error::Conflict { conflict } => match conflict {
            ConflictKind::Exists => "err-conflict-exists",
            ConflictKind::CaseCollision => "err-conflict-case",
            ConflictKind::Normalization => "err-conflict-normalization",
            ConflictKind::TypeMismatch => "err-conflict-type",
            // 0.84.0 (ADR 0151). With the generic key the reader would read a
            // plain "conflict", which is exactly what this subtype exists to
            // avoid saying: what happened is that its destination directory
            // is no longer there, and what to do follows from that.
            ConflictKind::DestinationGone => "err-conflict-destination-gone",
            _ => "err-conflict",
        },
        Error::ProviderUnavailable { .. } => "err-provider-unavailable",
        Error::NoSpace => "err-no-space",
        Error::Io { .. } => "err-io",
        Error::Cancelled => "err-cancelled",
        Error::PolicyDenied { .. } => "err-policy-denied",
        Error::EncodingLoss => "err-encoding-loss",
        Error::Unsupported => "err-unsupported",
        Error::InvalidPath => "err-invalid-path",
        Error::Internal { .. } => "err-internal",
        Error::Loop => "err-loop",
        Error::Corrupt => "err-corrupt",
        // #95.3: a local limit ≠ corruption. The sub-vocabulary (`entries`/
        // `decompressed-bytes`) is diagnostic, not UX: a single key.
        Error::LimitExceeded { .. } => "err-limit-exceeded",
        Error::HostKeyUnknown { .. } => "err-host-key-unknown",
        // 0.63.0 (#325): the TUI intercepts it and opens the dialog, so this
        // text is only seen by frontends that do not yet ask (the CLI, and
        // the window until #327). It has to say what to do without a dialog —
        // set the environment variable — not "unknown error".
        Error::SecretNeeded { .. } => "err-secret-needed",
        Error::HostKeyMismatch { .. } => "err-host-key-mismatch",
        Error::CursorExpired => "err-cursor-expired",
        // 0.36.0 (batch rename): both are ACTIONABLE — falling back to
        // `err-unknown` would be the opposite of what its rustdoc promises.
        Error::PlanStale => "err-plan-stale",
        Error::PlanNotExecutable => "err-plan-not-executable",
        // 0.40.0 (sync): the THREE relations are painted differently and the
        // first is not a degenerate case of the other two, so the
        // sub-vocabulary does travel — same as `Conflict`'s. What is
        // actionable differs in each: with `Same` another directory must be
        // chosen, with the other two you must leave the tree that contains
        // the other one. Without this arm the refusal fell into
        // `err-unknown`, which is exactly what the variant exists not to be.
        Error::OverlappingRoots { relation } => match relation {
            RootOverlap::Same => "err-overlapping-roots-same",
            RootOverlap::SourceInsideDest => "err-overlapping-roots-source-inside",
            RootOverlap::DestInsideSource => "err-overlapping-roots-dest-inside",
            _ => "err-overlapping-roots",
        },
        // 0.41.0 (#178): this session's journal cannot be opened and the
        // mutation was refused. It is in the actionable family — there is ONE
        // file to fix or remove — and falling into "unknown error" would
        // leave the user facing a session that suddenly does not mutate,
        // without saying why.
        Error::JournalUnavailable => "err-journal-unavailable",
        _ => "err-unknown",
    }
}

/// LOCALIZED text for a protocol [`Error`]'s category in the REQUESTED
/// language: [`error_key`]'s stable key run through Fluent — never the
/// hardcoded English `Display` nor an OS string.
///
/// It exists for the same reason `t_in` exists next to `t`: whoever composes
/// a sentence with an explicit `lang` —
/// [`compare::status_line`](crate::compare::status_line) receives one —
/// cannot fill one of its pieces with the AMBIENT language, or it returns a
/// half-translated sentence (phase C1 review, MINOR-6).
///
/// ```
/// use norte_frontend::error::error_category_in;
/// use norte_i18n::Lang;
/// let es = error_category_in(Lang::Es, &norte_proto::Error::NotFound);
/// let en = error_category_in(Lang::En, &norte_proto::Error::NotFound);
/// // The SAME key, said in two languages: neither of them is the key.
/// assert!(!es.starts_with("err-") && !en.starts_with("err-"));
/// ```
#[must_use]
pub fn error_category_in(lang: Lang, e: &Error) -> String {
    t_in(lang, error_key(e))
}

/// [`error_category_in`] in the AMBIENT language. A wrapper, for whoever has
/// no `lang` to pass.
#[must_use]
pub fn error_category(e: &Error) -> String {
    error_category_in(norte_i18n::active(), e)
}
