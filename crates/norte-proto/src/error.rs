//! Protocol error taxonomy (spec §17.7): stable, documented, renderable by
//! category. Frontends NEVER parse error strings; the mapping from OS/provider
//! errors happens at the edge (vfs-local, core).

use std::fmt;

use serde::{Deserialize, Serialize};

/// Subtype of a conflict at the destination of an operation.
/// N/N-1 tolerance (ADR 0004's pattern; ADR 0005 introduces the fallback): an
/// unknown subtype deserializes to [`ConflictKind::Unknown`] — the old client
/// degrades to "generic conflict", it does not break.
///
/// ```
/// use norte_proto::ConflictKind;
/// let future: ConflictKind = serde_json::from_str(r#""subtipo_del_futuro""#).unwrap();
/// assert_eq!(future, ConflictKind::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConflictKind {
    /// The destination already exists.
    Exists,
    /// Case-only collision on a case-insensitive FS (evaluated against the
    /// DESTINATION FS, not the source).
    CaseCollision,
    /// Unicode-normalization-only collision: the bytes differ but the NFC
    /// form matches (macOS stores NFD; issue #8, ADR 0005).
    Normalization,
    /// The destination exists with a different type (a dir where a file
    /// would go, or vice versa).
    TypeMismatch,
    /// The relative path escapes its confined root (0.45.0, #164, ADR 0054):
    /// an INTERMEDIATE component is a symlink, and following it would write
    /// outside the root the caller named.
    ///
    /// Deliberately not [`Self::Exists`] nor `Error::NotFound`: a caller that
    /// sees `NotFound` responds by creating the parent, which is exactly the
    /// operation this subtype exists to prevent.
    EscapesRoot,
    /// The DESTINATION DIRECTORY stopped being where it was requested, with
    /// the task already running (0.84.0, ADR 0151).
    ///
    /// It was deleted, moved, or replaced by something else while it was
    /// being copied to. This really happens and needs no bad faith: it is
    /// enough to delete the destination folder from somewhere else — another
    /// manager, an `rm` in a terminal, another machine on the same mount —
    /// while the progress bar is running.
    ///
    /// **It is different from [`Self::EscapesRoot`], and the difference
    /// matters.** `EscapesRoot` says the path leads somewhere else through a
    /// link, i.e. that writing there would be escaping: it is an answer about
    /// the SHAPE of the path, and whoever reads it thinks about security.
    /// This one says the place you named is no longer that place: there is
    /// nothing wrong with the path, it is that the folder is gone. The remedy
    /// is different too — recreate it and retry — and that is why they could
    /// not share a subtype.
    ///
    /// Nor is it `Error::NotFound`: that does not say WHAT was not found, and
    /// in the middle of copying thousands of files it reads as "cannot find a
    /// file from the source", which is the opposite of what happened.
    ///
    /// **What emits it**: copying a TREE, copying a single file (#367), and
    /// `sync.apply` (#368). Before 0.84.0 the case had no answer: all three
    /// returned `Completed` with the files in the deleted folder.
    ///
    /// A destination directory that is a LINK is not exempt: it is checked
    /// THROUGH the link. So an intact `~/copias -> /mnt/disco/copias` — or a
    /// macOS `/tmp` — passes without noise, and the two cases that do matter
    /// are detected: if what was on the other end went to the trash, the link
    /// ends up broken; if someone repoints it, it leads somewhere else. Both
    /// are this subtype, because in both the place you named is no longer
    /// that place.
    ///
    /// An N-1 client degrades it to [`Self::Unknown`] and shows plain
    /// "conflict". What it loses is the phrase, not the protection: the one
    /// that checks is the daemon, so the task fails instead of claiming it
    /// copied. What was already written stays in the deleted folder, for
    /// both alike (#369).
    DestinationGone,
    /// The `revision` the writer carried is not the current one (0.48.0, L2):
    /// another client wrote the UI session between its read and its write.
    ///
    /// It is a conflict and not a parameter error because it keeps this
    /// category's promise: NOTHING was written, and the caller fixes it by
    /// re-reading. An N-1 client degrades it to [`Self::Unknown`] and shows
    /// plain "conflict", which is still the correct behavior: read again.
    StaleRevision,
    /// Subtype of a newer protocol (deserialization fallback). The core NEVER
    /// emits it.
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

impl fmt::Display for ConflictKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Exists => "destination exists",
            Self::CaseCollision => "case-insensitive collision",
            Self::Normalization => "unicode normalization collision",
            Self::TypeMismatch => "destination type mismatch",
            Self::EscapesRoot => "path escapes its confined root",
            // As terse as its siblings, and not cosmetic: this string ends up
            // in an `RpcError`'s `message` and `norte-mcp` hands it to an
            // agent verbatim.
            Self::DestinationGone => "destination directory is gone",
            Self::StaleRevision => "stale revision",
            Self::Unknown => "unknown conflict kind (newer protocol)",
        })
    }
}

/// How the two roots of a `sync.plan` overlap (0.40.0, ADR 0049): the payload
/// of [`Error::OverlappingRoots`].
///
/// There are THREE cases, not two, and that is why this is not a
/// [`Side`](crate::methods::Side): "they are the same tree" is not "one is
/// inside the other", and it is exactly the phrase a frontend needs to
/// render. With two values one would have to be chosen by convention and the
/// message would lie in that case.
///
/// Names `source` and `dest`, not `left` and `right`: comparing is symmetric
/// and syncing is not (design §"Params name sides, not hands").
///
/// N/N-1 tolerance (ADR 0004), like [`ConflictKind`]: it travels daemon→client,
/// so an unknown relation degrades to [`RootOverlap::Unknown`] instead of
/// breaking the error's parse.
///
/// ```
/// use norte_proto::RootOverlap;
/// assert_eq!(
///     serde_json::to_string(&RootOverlap::DestInsideSource).expect("json"),
///     r#""dest_inside_source""#
/// );
/// let future: RootOverlap = serde_json::from_str(r#""braided""#).expect("degrades");
/// assert_eq!(future, RootOverlap::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RootOverlap {
    /// The two roots name the SAME tree. Neither is inside the other: they
    /// are the same.
    Same,
    /// The SOURCE is inside the destination.
    SourceInsideDest,
    /// The DESTINATION is inside the source. The one that would turn a copy
    /// into a loop.
    DestInsideSource,
    /// Relation of a newer protocol (deserialization fallback). The core
    /// NEVER emits it.
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

impl fmt::Display for RootOverlap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Same => "source and destination are the same tree",
            Self::SourceInsideDest => "the source is inside the destination",
            Self::DestInsideSource => "the destination is inside the source",
            Self::Unknown => "unknown overlap relation (newer protocol)",
        })
    }
}

/// norte protocol error (spec §17.7).
///
/// Wire: an object tagged `{"kind": "...", …fields}`. The variant is the API:
/// frontends match by category and the human-readable detail travels
/// separately (the JSON-RPC error's `message` field), never inside this
/// taxonomy.
///
/// N/N-1 tolerance (ADR 0004): an unknown category deserializes to
/// [`Error::Unknown`] — the old client degrades to "generic error", it does
/// not break. `#[non_exhaustive]` also forces the `_` arm in Rust.
///
/// ```
/// use norte_proto::Error;
/// let e: Error = serde_json::from_str(r#"{"kind": "not_found"}"#).unwrap();
/// assert_eq!(e, Error::NotFound);
/// let future: Error = serde_json::from_str(r#"{"kind": "quota_del_futuro"}"#).unwrap();
/// assert_eq!(future, Error::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Error {
    /// The path does not exist.
    #[error("not found")]
    NotFound,
    /// The provider/OS denied access.
    #[error("permission denied")]
    PermissionDenied,
    /// Conflict at the destination; the operation did NOT write anything.
    #[error("conflict: {conflict}")]
    Conflict {
        /// Subtype of the conflict.
        conflict: ConflictKind,
    },
    /// The provider is not responding (network down, remote daemon dead…).
    #[error("provider unavailable (retryable: {retryable})")]
    ProviderUnavailable {
        /// `true` if retrying with backoff makes sense.
        retryable: bool,
    },
    /// No space or quota left at the destination (ENOSPC/EDQUOT) — the most
    /// common copy failure after permissions; it deserves its own rendering,
    /// not a generic bucket.
    #[error("no space left on destination")]
    NoSpace,
    /// I/O failed mid-operation (EIO, connection reset…): the provider
    /// responds, but this particular operation died.
    #[error("i/o error (retryable: {retryable})")]
    Io {
        /// `true` if repeating the operation might work.
        retryable: bool,
    },
    /// Cancelled by the user or by shutdown; clean state guaranteed.
    #[error("cancelled")]
    Cancelled,
    /// The policy engine denied the operation (agents/plugins, M3-3).
    #[error("denied by policy rule `{rule}`")]
    PolicyDenied {
        /// COARSE category of the reason for denial — NOT the identifier of
        /// the specific `policy.toml` rule (which is not leaked, for
        /// security). Closed vocabulary, comparable by equality:
        /// `"out-of-scope"` (path/op outside the agent's scope),
        /// `"scope-expired"` (the applicable scope expired),
        /// `"policy-rule"` (a `policy.toml` `deny` rule matched),
        /// `"no-rule"` (fail-closed: inside the scope but no rule applies),
        /// `"not-approved"` (an `ask` was denied or its TTL expired).
        ///
        /// Those six are the ones that CROSS THE WIRE. The SDK additionally
        /// synthesizes the METHOD NAME when a two-state result comes back
        /// false (`connection.trust_host_key`, `connection.provide_secret`):
        /// that value does not come from the daemon, it is built on the
        /// client so a rejection is never treated as a success. A reader of
        /// the wire will never see it, and the vocabulary above stays closed.
        rule: String,
    },
    /// The approval being decided is no longer there (#279, since 0.55.0).
    ///
    /// Exists because the three ways of "no longer there" call for different
    /// answers from whoever is looking at the screen, and before they
    /// collapsed into an `INVALID_PARAMS` with an English text inside
    /// `message`: a frontend could only say "the approval never reached the
    /// daemon", which is true in ONE of the three cases and false in the
    /// other two. "It arrived and the daemon had already denied it by TTL"
    /// read as "your click got lost".
    #[error("the approval is gone: {reason}")]
    ApprovalGone {
        /// Which of the three. CLOSED vocabulary, comparable by equality,
        /// like `PolicyDenied.rule`: `"unknown"` (that id never existed, or
        /// the daemon restarted), `"expired"` (it was pending but whoever
        /// asked for it is no longer listening), and `"already-decided"`
        /// (that id existed and someone resolved it before — another
        /// window, its TTL, or the requester itself withdrawing it).
        ///
        /// A value this binary does not know is treated as `"unknown"`: the
        /// set can GROW additively, and whoever does not recognize it must
        /// not invent an explanation.
        reason: String,
    },
    /// A transcoding would have lost data and was aborted.
    #[error("encoding loss")]
    EncodingLoss,
    /// The operation is not supported by this provider (see `Capabilities`).
    #[error("unsupported operation")]
    Unsupported,
    /// The `VPath` received does not parse or violates invariants.
    #[error("invalid path")]
    InvalidPath,
    /// Internal core error; `panic: true` = a supervised task that panicked
    /// (the daemon stays alive, spec §17.7).
    #[error("internal error (panic: {panic})")]
    Internal {
        /// `true` if the source was a panic caught in a task.
        panic: bool,
    },
    /// Symlink loop detected while walking with `Follow` (the visited set of
    /// spec §17.9; issue #31). An N-1 client degrades to `Unknown`.
    #[error("symlink loop")]
    Loop,
    /// BROKEN container/format (0.17.0, #58): a corrupt, truncated or
    /// structurally lying zip/tar. Not an I/O failure (retrying does not
    /// help; a failure of the underlying provider propagates with its own
    /// category, never as `Corrupt`); the honest UX is "not a valid
    /// container". Since 0.23.0 (#95) exceeding the LOCAL anti-bomb caps is
    /// no longer `Corrupt`: it is [`Error::LimitExceeded`] — the container
    /// may be perfectly valid. An N-1 client degrades to `Unknown`.
    #[error("corrupt or invalid container/format")]
    Corrupt,
    /// The container exceeds a LOCAL anti-bomb limit of the index (0.23.0,
    /// #95, ADR 0018 D2). Different from [`Error::Corrupt`]: the container
    /// may be VALID (a legitimately huge tar.gz) — norte REFUSES to pay its
    /// cost under the current limits, it does not declare it broken. An N-1
    /// client degrades to `Unknown` (the same coarse UX as the old
    /// `Corrupt`).
    #[error("container exceeds local limit: {limit}")]
    LimitExceeded {
        /// WHICH limit was exceeded — CLOSED vocabulary, comparable by
        /// equality (never the numeric value, which is local
        /// configuration): [`Error::LIMIT_ENTRIES`] (index entries — or ones
        /// announced by the EOCD — above `max_entries`, including the
        /// skipped-entries budget), [`Error::LIMIT_DECOMPRESSED_BYTES`]
        /// (accumulated inflation above `max_decompressed_bytes`), or
        /// [`Error::LIMIT_NESTING`] (nested archive layers above
        /// `max_nesting`, #56/proto 0.24), or
        /// [`Error::LIMIT_RETAINED_SYNC_PLANS`] (sync plans retained by a
        /// connection, 0.44.0), or [`Error::LIMIT_SESSION_BODY`] (UI
        /// session body above 1 MiB, 0.48.0). Emitters use the constants,
        /// never bare literals (single source, pinned by a test).
        /// Forward-compat: an UNKNOWN token (a newer peer) is treated as a
        /// generic limit — show the string as is, never fail the parse nor
        /// guess.
        limit: String,
    },
    /// UNKNOWN SSH host key on first contact (TOFU — ADR 0015 D). The
    /// frontend shows the `fingerprint` and, if the user trusts it, calls
    /// `connection.trust_host_key` and retries. An N-1 client degrades to
    /// `Unknown` (0.7.0, phase 6).
    #[error("unknown host key for {host} ({algo})")]
    HostKeyUnknown {
        /// BARE host being connected to (no port; the port travels
        /// separately so the error→`connection.trust_host_key` mapping is
        /// 1:1).
        host: String,
        /// Port (absent = the scheme's default).
        #[serde(default)]
        port: Option<u16>,
        /// Key algorithm (e.g. `ssh-ed25519`).
        algo: String,
        /// Fingerprint in OpenSSH `SHA256:<base64>` format — the SAME string
        /// the core and the frontend compare and that goes into
        /// `trust_host_key`.
        fingerprint: String,
    },
    /// The connection needs a secret that is nowhere to be found, and its
    /// `connections.toml` says to ASK for it (`secret = "prompt"`, #325).
    ///
    /// Same flow as the TOFU above, on purpose: the frontend opens its
    /// dialog, sends what was typed with `connection.provide_secret`, and
    /// RETRIES this same navigation. The core cannot ask on its own — the
    /// secret resolver has no user interface and must not have one — so the
    /// only way a human gets asked is for the question to travel up this way.
    ///
    /// Carries the connection's name AND its destination, and the
    /// destination is mandatory: **a password dialog that does not say who
    /// it is going to hand it to is not answerable.** The name was chosen by
    /// `connections.toml`, which may come from someone else's dotfiles or an
    /// edited line; `trabajo` says nothing about whether that entry points
    /// today at the usual machine or at `ftp://evil.example`. It is the same
    /// reason the TOFU above carries host, algorithm and fingerprint: whoever
    /// answers verifies the OTHER END, not a local label.
    ///
    /// And the risk is not theoretical across every scheme: with `sftp` the
    /// SSH handshake still goes through TOFU before sending anything, but an
    /// `ftp` goes in the clear and an `s3` with a foreign `endpoint` signs a
    /// request against whatever server the entry chose.
    ///
    /// A 0.62 client degrades to `Unknown` and shows an error instead of a
    /// dialog — i.e. it cannot connect that connection, same as today.
    #[error("connection {conn} ({endpoint}) needs a secret")]
    SecretNeeded {
        /// Connection name in `connections.toml`, the same one that goes in
        /// [`crate::methods::CONNECTION_PROVIDE_SECRET`].
        conn: String,
        /// Where it would connect to, `scheme://host[:port]`, **without
        /// userinfo** (the same redaction as
        /// [`crate::methods::ConnectionDegraded`]: a `user:pass@` in the URL
        /// is not forwarded to the screen or the log). For DISPLAY only: the
        /// frontend does not reparse it or use it to connect.
        endpoint: String,
    },
    /// The SSH host key CHANGED from the one registered in `known_hosts`:
    /// possible MITM. NEVER accepted silently (0.7.0, phase 6).
    #[error("host key MISMATCH for {host} ({algo}) — possible MITM")]
    HostKeyMismatch {
        /// Affected BARE host (no port).
        host: String,
        /// Port (absent = the scheme's default).
        #[serde(default)]
        port: Option<u16>,
        /// Algorithm of the presented key.
        algo: String,
        /// OpenSSH `SHA256:<base64>` fingerprint of the presented key.
        fingerprint: String,
    },
    /// An `fs.list` pagination `cursor` is no longer valid: it expired (TTL),
    /// was evicted (LRU), or died with the connection (ADR 0017, 0.8.0). The
    /// client restarts the listing from scratch. An N-1 client (0.7.x) never
    /// sees it — it does not send cursors — but it would degrade to `Unknown`
    /// if it received one.
    #[error("list cursor expired; restart the listing")]
    CursorExpired,
    /// The directory changed between the preview and the execution of a
    /// rename batch (`fs.rename_batch`, 0.36.0): re-planning the SAME pairs
    /// produced a plan different from the `plan_hash` the caller approved.
    /// Nothing was attempted. Actionable: re-plan and confirm again. NOT
    /// retryable as is — the human has to see the new plan. An N-1 client
    /// (0.35.x) never sees it (it does not call the method) but would
    /// degrade to `Unknown`.
    #[error("rename plan is stale; re-plan and confirm again")]
    PlanStale,
    /// The rename plan has collisions, so nothing was attempted
    /// (`fs.rename_batch`, 0.36.0). Actionable: fix the names (or the source
    /// directory) and re-plan. An N-1 client (0.35.x) never sees it but would
    /// degrade to `Unknown`.
    #[error("rename plan has collisions; nothing was attempted")]
    PlanNotExecutable,
    /// The two roots of a `sync.plan` are THE SAME TREE: equal, or one inside
    /// the other (0.40.0, ADR 0049). No Task was created.
    /// Actionable: choose another pair of roots.
    ///
    /// It is a STRUCTURAL rejection, prior to the walk, and that is why it is
    /// a category and not a `-32602` with a message: frontends match by
    /// category and never parse error strings, so "these two folders are the
    /// same" can only be rendered — and translated — if it travels as a
    /// variant. The twin check that runs DURING the walk, and that catches
    /// what a symlink or a second authority hides, is not an error but a
    /// [`SyncBlockerKind::OverlapDetected`](crate::methods::SyncBlockerKind::OverlapDetected):
    /// by then there is already a plan to belong to.
    ///
    /// `fs.compare` does NOT emit it: comparing `/a` against `/a/sub` costs a
    /// walk and writes not a byte. An N-1 client (0.39.x) never sees it — it
    /// does not call the method — but would degrade to `Unknown`.
    #[error("sync roots overlap: {relation}")]
    OverlappingRoots {
        /// HOW they overlap: they are the same, or one contains the other and
        /// which one. The three render differently and the first is not a
        /// degenerate case of the other two.
        relation: RootOverlap,
    },
    /// THIS session's journal cannot be opened, so the mutation was REJECTED
    /// and nothing was touched (0.41.0, #178).
    ///
    /// This is not "the file is busy": a journal that has another process on
    /// it — a live daemon, another embedded session — lets it through, with a
    /// warning, because refusing it would turn "there is a daemon" into "the
    /// CLI does not work". This is the other case: no permissions, corrupt,
    /// not-a-database, or from an era before today's chain. There, continuing
    /// would mean mutating with no record and no undo, which is exactly what
    /// hard rule 4 forbids and what `norte daemon run` already refuses with
    /// this same entry.
    ///
    /// **This is the variant an attacker with write access to the state
    /// directory can make appear.** Corrupting `journal.db` used to silently
    /// disable the recording of ALL embedded sessions — including
    /// `norte ai rename --yes`'s, which needs it the most; now it stops
    /// them. Actionable: fix or remove `journal.db` from the state
    /// directory.
    ///
    /// No fields ON PURPOSE: the file's path is local to the process that
    /// emits it and means nothing on the other end of a socket, and the
    /// reason is `SQLite`'s raw error — text partly shaped by whoever can
    /// write the file — which has no business crossing the boundary. Both
    /// travel through the journal's warning channel
    /// (`norte_core::embedded::NoJournal`), which is in-process and
    /// sanitized.
    ///
    /// That does not leave the detail with nowhere to go: if naming the file
    /// OVER THE WIRE is ever needed, the `RpcError`'s `message` already
    /// carries this error's `Display`, which is where ADR 0004 puts what is
    /// human-readable. The taxonomy keeps the category; adding a field here
    /// would not be needed.
    ///
    /// Today only the EMBEDDED transport emits it: the daemon with this same
    /// entry does not even start. An N-1 client (0.40.x) would degrade to
    /// `Unknown`.
    #[error("this session's journal cannot be opened; the mutation was refused")]
    JournalUnavailable,
    /// Category of a newer protocol (deserialization fallback). The core
    /// NEVER emits it; it exists so an N client degrades gracefully in the
    /// face of N+1 categories.
    #[doc(hidden)]
    #[error("unknown error category (newer protocol)")]
    #[serde(other)]
    Unknown,
}

impl Error {
    /// Vocabulary of [`Error::LimitExceeded`]: index entries (or ones
    /// announced by the EOCD) above `max_entries`, including the
    /// skipped-entries budget. SINGLE source — emitters do not write
    /// literals.
    ///
    /// ```
    /// use norte_proto::Error;
    /// let e = Error::LimitExceeded { limit: Error::LIMIT_ENTRIES.into() };
    /// assert_eq!(serde_json::to_string(&e).unwrap(),
    ///            r#"{"kind":"limit_exceeded","limit":"entries"}"#);
    /// ```
    pub const LIMIT_ENTRIES: &'static str = "entries";
    /// Vocabulary of [`Error::LimitExceeded`]: accumulated inflation above
    /// `max_decompressed_bytes` (gzip bomb or a legitimately huge container).
    pub const LIMIT_DECOMPRESSED_BYTES: &'static str = "decompressed-bytes";
    /// Vocabulary of [`Error::LimitExceeded`] (#56, proto 0.24): nested
    /// archive layers above `max_nesting` (governed by the engine — the
    /// addressing itself is syntactically unbounded).
    pub const LIMIT_NESTING: &'static str = "nesting";
    /// Vocabulary of [`Error::LimitExceeded`] (0.44.0, #182): sync plans
    /// RETAINED by a connection, above the daemon's cap.
    ///
    /// Not a container — the other three are about archives — and yet it
    /// lives here, because what the client needs to know is exactly the
    /// same: "this is NOT broken; norte refuses to pay its cost under
    /// today's limits". The alternative was a new taxonomy variant to say the
    /// same thing with another word.
    ///
    /// What it fixes is concrete (#182): that rejection used to travel with
    /// its phrase in `message` and WITHOUT a taxonomy in `data`, so the
    /// client received it as `Internal { panic: false }` — "internal error",
    /// which is exactly the text that makes a model retry, and retrying was
    /// what filled the cap. An N-1 client reads it as a generic limit and
    /// shows the token as is, which has been this field's contract since it
    /// existed.
    pub const LIMIT_RETAINED_SYNC_PLANS: &'static str = "retained-sync-plans";

    /// A UI session's body goes over
    /// [`crate::methods::SESSION_BODY_MAX`] (0.48.0, L2). The core measures it
    /// in serialized bytes and refuses it whole: it does not truncate a
    /// document whose schema it does not know. The client discards history
    /// and retries ONCE.
    pub const LIMIT_SESSION_BODY: &'static str = "session-body";
}
