//! The values the SDK needs to name on its own.
//!
//! None of these is a convenience copy: they are the things a remote client
//! uses and that lived in `norte-core` only because the client lived there.
//! Each one is here for a different reason, and the reason is written beside
//! it.

use std::time::Duration;

use norte_proto::{CollisionPolicy, Entry, Error, ResumePolicy, SymlinkPolicy, VerifyPolicy};

/// Timeout for AI calls: the provider (remote model) legitimately takes much
/// longer than an `fs.*`.
pub const AI_CALL_TIMEOUT: Duration = Duration::from_mins(2);

/// The stream of a paginated listing.
///
/// Its OWN alias and not `norte-vfs`'s (which is identical) because dragging
/// the provider contract crate into a client that only talks over a socket
/// would be paying for a whole tree for one alias (ADR 0066).
pub type EntryStream = futures::stream::BoxStream<'static, Result<Entry, Error>>;

/// Copy or move: a transfer's two verbs (#270).
///
/// An enum, not the method name as a string, because when the verb was a
/// `&str` the dispatch was `if method == FS_COPY { … } else { … }`: anything
/// that was not exactly `fs.copy` turned into a MOVE, which also deletes the
/// source. A typo's failure was not a visible error but the other operation.
/// `norte-ui-host` already interposed its own enum on its side to make this
/// mistake impossible; the SDK did not have one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transfer {
    /// `fs.copy`.
    Copy,
    /// `fs.move` — DELETES the source.
    Move,
}

/// A transfer's options, as they travel over the wire.
///
/// A twin of `norte_core::engine::TransferOptions`, deliberately: the core's
/// is the ENGINE's input and can grow with things only the engine
/// understands; this one is what a remote client puts in the params. The
/// core converts between the two with an exhaustive `From`, so a new field in
/// either one is a compile error, not an option silently lost.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransferOptions {
    /// What to do if the destination already exists.
    pub on_collision: CollisionPolicy,
    /// What to do with the source's symlinks.
    pub symlinks: SymlinkPolicy,
    /// Resuming interrupted transfers (ADR 0012).
    pub resume: ResumePolicy,
    /// Verifying the partial file on resume (only with `resume=On`).
    pub verify: VerifyPolicy,
    /// Into the QUEUE instead of in parallel (ADR 0149): one at a time.
    pub queued: bool,
}

/// What a `sync.plan` keeps emitting.
///
/// Lives in the SDK and `norte_core::sync` re-exports it — instead of each
/// having its own — because its two variants ARE wire types: the embedded
/// plan and the remote one emit exactly the same thing, and two definitions
/// would be two places to add a variant.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncPlanEvent {
    /// A batch of steps, bounded by `SYNC_STEPS_MAX_BATCH`.
    Steps(norte_proto::methods::SyncStepsBatch),
    /// The plan's close. At most ONE per Task, and always the last.
    Done(norte_proto::methods::SyncPlanDone),
}

/// Connection event of the remote backend (for the message bar).
///
/// `#[non_exhaustive]`: this crate is the SDK's publishable surface (ADR
/// 0066), and adding `GoingAway` already forced touching every `match`
/// outside it. The next event has to be able to be additive.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnEvent {
    /// The connection to the daemon was lost; reconnecting in the
    /// background.
    Lost,
    /// Reconnected (and resynced via `task.list`).
    Restored,
    /// The daemon warned it is leaving (`daemon.going_away`, 0.46.0).
    ///
    /// Arrives BEFORE the connection closes, and is the only thing that
    /// distinguishes a handoff from a stop: from the disconnect on, the two
    /// look the same. The frontend needs it to say which of the two is
    /// happening instead of painting "reconnecting…" over a daemon that is
    /// not coming back.
    GoingAway {
        /// The daemon says it is coming back (a handoff, e.g. an update).
        reconnect: bool,
    },
}
