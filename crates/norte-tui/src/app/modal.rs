//! The modals, their length limits and the decision on whether quitting
//! asks for confirmation.

use super::trail::Trail;
use norte_i18n::ta;
use norte_proto::VPath;

/// What's typed in [`Modal::AskSecret`]: a half-typed password.
///
/// Re-exported from the SHARED crate since #327, when the window needed the
/// same field. It's a SECURITY type — `Debug` that redacts, buffer wiped on
/// drop, capacity reserved upfront — and two implementations are two spots
/// where one of the three guarantees gets forgotten. The name stays here so
/// this frontend's thirty call sites don't need to change.
pub use norte_frontend::secret::TypedSecret;

/// What a [`Modal::Report`] reports on. Its title and help page come from
/// here, via an exhaustive `match`: a third report is a compile error at
/// every spot that has to decide something, not a string falling through to
/// the wrong page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportKind {
    /// A rename batch that left something half done.
    Batch,
    /// An undo that didn't return everything.
    Undo,
}

impl ReportKind {
    /// The title's Fluent key.
    #[must_use]
    pub fn title_key(self) -> &'static str {
        match self {
            Self::Batch => "modal-batch-report-title",
            Self::Undo => "modal-undo-report-title",
        }
    }
}

/// Kind of transfer pending confirmation/collision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    /// Copy (F5).
    Copy,
    /// Move (F6).
    Move,
}

/// Which checksum batch dispatch requested (#311).
///
/// Two shapes and not a `bool`: computing operates on a SELECTION and
/// verifying operates on ONE file — and that one has to be read before
/// anything can even be requested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChecksumRequest {
    /// Compute the digest of these paths and show it.
    Compute {
        /// What's marked, or the cursor: the usual operand.
        paths: Vec<VPath>,
    },
    /// Read this checksum file and verify what it lists.
    Verify {
        /// The checksum file. Names are resolved against ITS directory.
        sums: VPath,
    },
}

/// A row of the checksums modal (#311).
///
/// The name goes in BYTES: a checksum file names files, and a name doesn't
/// have to be text (rule 1). It's painted with the usual sanitizing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChecksumRow {
    /// The name, as it is on disk.
    pub name: Vec<u8>,
    /// Its digest, or `None` if it couldn't be computed.
    pub digest: Option<String>,
    /// The verdict against what was published. `None` when only computed.
    pub verdict: Option<norte_frontend::checksums::Verdict>,
}

/// Active modal dialog. Its keys resolve against the keymap's `dialog`
/// context (H1, issue #24 — CLOSED): the run loop passes the key through
/// the effective `dialog`'s [`Resolver`](crate::keymap::Resolver) and the
/// resulting command is filtered by the concrete modal's ALLOWLIST
/// ([`crate::app::dialog_action`]) — the SECURITY semantics (what confirms,
/// what denies, what's inert) live in code, never in the keymap; only the
/// key→command ASSIGNMENT is rebindable. Sole exception:
/// `Modal::TrustLuaInit`, which the run loop intercepts BEFORE (it needs
/// the `LuaHost`) and resolves with [`crate::app::trust_lua_key`] — decision
/// 8 of plan H1, not migrated.
///
/// No `Eq` (M4-IA-2): [`Modal::SemanticHits`] carries
/// [`norte_proto::methods::SemanticHit`]'s `score: f64`, which is only
/// `PartialEq` — like its proto type.
#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    /// The properties of the entry under the cursor (#139).
    ///
    /// What it shows comes from the LISTING, which already has it: name,
    /// kind, size, date and whatever attributes the provider reported.
    /// Opening it requests nothing — except one thing, and it's exactly
    /// what a listing can't know: what a folder takes up. That gets
    /// counted, and while it's counting the dialog says so.
    Properties {
        /// The entry, as it is in the listing.
        entry: Box<norte_proto::Entry>,
        /// The count in progress, if one was launched (folders only).
        size_task: Option<norte_proto::TaskId>,
        /// `(bytes, entries)` once the count finished.
        size: Option<(u64, u64)>,
    },
    /// The report of something that did NOT come back whole: a rename batch
    /// half done or an undo that left entries un-undone (or whose outcome
    /// couldn't be verified).
    ///
    /// Read and closed, like properties. The lines are decided by
    /// `norte_frontend` (`batch_report_lines`, `undo_report_lines`), the
    /// same ones the window's dialog uses: a half-renamed or half-undone
    /// directory can't go unnoticed in either one.
    Report {
        /// What it reports on: gives the title and the help page.
        kind: ReportKind,
        /// Phrases and paths, each path on its own line.
        lines: Vec<norte_frontend::ReportLine>,
    },
    /// Granting capabilities to an extension (#280).
    ///
    /// It's THE extension system's security decision: what's being granted
    /// is reading files, running programs or reaching the network on the
    /// user's behalf. Here it used to be approved with one key and without
    /// listing anything, while the graphical window already asked.
    /// REVOKING doesn't go through here: it goes in the safe direction.
    ConfirmPluginApproval {
        /// The extension's id, as the core names it.
        id: String,
        /// Its name, already sanitized to paint.
        name: String,
        /// The name differs from the real one and has to be marked.
        name_hostile: bool,
        /// The capabilities being granted, each one masked on its own and
        /// with its own flag: gluing them into one sentence would let one
        /// pretend to be another.
        caps: Vec<(String, bool)>,
        /// The anchor of the manifest CURRENTLY BEING SHOWN (#282), if the
        /// core sends it. Travels with the yes, and the core refuses if
        /// `plugin.toml` changed between the question and the answer.
        digest: Option<String>,
    },
    /// Confirmation to UNINSTALL an extension (ADR 0104): deletes its files
    /// and withdraws its consent, and there's no going back — there's no
    /// `plugin.install` over the wire. It asks because of that, and the body
    /// says both things that get lost: a bare "uninstall?" reads as "just
    /// turn it off?".
    ConfirmPluginUninstall {
        /// The extension's id, as the core names it.
        id: String,
        /// Its name, already sanitized to paint.
        name: String,
        /// The name differs from the real one and has to be marked.
        name_hostile: bool,
    },
    /// Review of an ORGANIZE plan (phase 8, `ai.organize_plan` /
    /// `plugin.organize_plan`).
    ///
    /// Reviewed as a TREE and not as a list of pairs, which is the
    /// difference with [`Self::AiRenamePlan`]: what changes is the
    /// directory's shape, and forty rows of `a.pdf → invoices/2026/a.pdf`
    /// don't let you see how many folders show up nor what ends up in each
    /// one.
    ///
    /// Carries the SAME approval discipline as the rename plan: until the
    /// reader has reached the end, confirming stays mute. A two-hundred-move
    /// plan approved after seeing ten isn't a reviewed plan.
    OrganizePlan {
        /// Directory it applies to.
        dir: VPath,
        /// The moves, exactly as the producer proposed them.
        moves: Vec<norte_proto::methods::OrganizeMove>,
        /// The already-computed tree, which is what gets painted.
        lines: Vec<norte_frontend::organize::TreeLine>,
        /// The token of the reviewed plan: the only thing `fs.organize`
        /// accepts.
        plan_hash: norte_proto::methods::PlanHash,
        /// First visible line of the window.
        offset: usize,
        /// How far the reader has ever gotten. A HIGH watermark, not the
        /// current position: scrolling back up doesn't undo having read.
        seen: usize,
    },
    /// Confirmation to UNDO up to a point in the timeline (phase 7,
    /// `journal.undo_after`).
    ///
    /// It asks because it reverts work, and the body carries the COUNT:
    /// what's going to be undone, what's going to be skipped and what
    /// doesn't belong to the reader — three numbers that don't add up,
    /// because promising just one would be promising something that isn't
    /// going to happen. This screen's rule is that a confirmation that
    /// doesn't say how much isn't a confirmation.
    ConfirmUndoAfter {
        /// The cut: undoes the human's entries AFTER this `seq`, and the
        /// entry naming it stays.
        seq: i64,
        /// How many entries are going to be attempted to undo.
        to_undo: usize,
        /// How many are going to be skipped (no way back, already undone,
        /// or compensations).
        irreversible: usize,
        /// How many above the cut are NOT the reader's, and that this undo
        /// therefore doesn't touch.
        ajenas: usize,
        /// The ceiling (`upto_seq`, 0.80.0): the newest thing this count
        /// counted. The undo doesn't go past it, so anything done after the
        /// list got painted doesn't get in without being counted.
        techo: Option<i64>,
    },
    /// Delete confirmation (F8) over the MARKS. `permanent = false` → trash.
    ConfirmDelete {
        /// The items to delete, in listing order.
        items: Vec<VPath>,
        /// Permanent (shift+F8, or no trash in the provider): the dialog
        /// WARNS (ADR 0009).
        permanent: bool,
    },
    /// Copy/move confirmation over the MARKS (#103). `to` is the destination
    /// DIRECTORY (the other pane's): with several items there's no single
    /// name to edit. A single item's editable destination, and the rename
    /// it carries, live in #105.
    ConfirmTransfer {
        /// Copy or Move.
        kind: TransferKind,
        /// The sources, in listing order.
        items: Vec<VPath>,
        /// Destination directory.
        to: VPath,
        /// The space warning, when there is one (#149): written by the run
        /// loop — asking about volumes is I/O — and painted by this modal.
        ///
        /// `None` is the NORMAL case, and honestly means all three at once:
        /// it fits, or the destination can't say how much room is left, or
        /// it isn't known how much is going to move. None of the three gets
        /// announced.
        space: Option<String>,
        /// The confinement warning, when there is one (#164): like
        /// [`Modal::ConfirmTransfer::space`], written by the run loop and
        /// painted by this modal.
        ///
        /// `None` = this destination knows how to confine its writes, which
        /// is the normal case on Linux and macOS and doesn't get announced.
        confine: Option<String>,
    },
    /// Collision: choosing a policy and RESUBMITTING the whole operation
    /// (ADR 0005: the engine treats Ask as Fail; the TUI asks at the task
    /// level). Carries the WHOLE `RetrySpec`: the retry keeps the original
    /// options, only the collision policy changes.
    Collision {
        /// The transfer that collided, ready to resubmit.
        retry: crate::tasks::RetrySpec,
    },
    /// Approving an AGENT op under the `ask` rule (M3-3b T5): the daemon
    /// broadcast `policy.approval_required` and waits for `policy.decide`.
    /// Paths are DISPLAY ONLY (redacted server-side): never reparsed. `y`
    /// approves, `n`/Esc deny; Enter does NOT approve (approving an agent
    /// mutation isn't an innocuous answer that deserves firing on its own —
    /// same principle as the collision).
    ApproveAgentOp {
        /// The pending approval exactly as it arrived from the daemon.
        req: norte_proto::methods::PolicyApprovalRequired,
    },
    /// First TOFU contact with an unknown SSH host (#45, ADR 0015 D): an
    /// `Error::HostKeyUnknown` while navigating to `dir`. Shows
    /// host/algo/fingerprint so the user can COMPARE them out of band; `y`
    /// trusts (`connection.trust_host_key`) and retries the navigation,
    /// `n`/Esc cancel. Enter does NOT trust (a security decision, same
    /// principle as approving an agent op). host/algo/fingerprint come from
    /// the remote server (untrusted): masked when painted.
    TrustHostKey {
        /// Bare host being connected to (the `HostKeyUnknown`'s).
        host: String,
        /// Port (absent = the scheme's default).
        port: Option<u16>,
        /// Key algorithm (e.g. `ssh-ed25519`).
        algo: String,
        /// OpenSSH `SHA256:<base64>` fingerprint — the SAME string that goes
        /// to `connection.trust_host_key`.
        fingerprint: String,
        /// The remote path to retry navigating to after trusting.
        dir: VPath,
        /// The pane that was navigating when the TOFU triggered. The modal
        /// CARRIES it because the interrupted navigation isn't necessarily
        /// the focused pane's (`pane.mirror` sends the OTHER pane somewhere
        /// while focus stays put): retrying against the focus would resume
        /// on the WRONG pane.
        pane: usize,
        /// Whether the interrupted navigation is RECORDED into the trail or
        /// is the trail replaying itself — and, in that case, WHICH step it
        /// was on ([`Trail::step`]). Carried for the same reason as `pane`:
        /// the retry must be the SAME navigation the TOFU interrupted, not a
        /// new one.
        ///
        /// The step travels because this modal is the ONE place where a
        /// navigation outlives whoever started it: `walk_trail` already
        /// returned `Suspended` and rewound nothing (the retry was going to
        /// finish the step), so if the modal's answer ends up abandoning the
        /// navigation — denying, or a retry that fails — the trail is left
        /// believing the reader left a place they're still at. Whoever
        /// answers the modal rewinds, and for that it needs the direction.
        trail: Trail,
    },
    /// Connection `conn` declares `secret = "prompt"` and none of the usual
    /// three sources has it (#325): an `Error::SecretNeeded` while
    /// navigating to `dir`. The password gets typed, Enter delivers it
    /// (`connection.provide_secret`) and RETRIES the navigation; Esc
    /// cancels.
    ///
    /// The mold is [`Modal::TrustHostKey`] — it carries `dir`/`pane`/`trail`
    /// for the same reasons, and whoever answers rewinds the trail — with
    /// two differences that come from a secret being WRITTEN here:
    ///
    /// * Enter DOES confirm. Not in the TOFU one, because confirming there
    ///   is a security decision that must not fire on its own; here Enter
    ///   over an empty field delivers nothing (there's no decision to fire),
    ///   and over a typed field it's what the user's finger was already
    ///   going to do.
    /// * What's typed does NOT get painted: the dialog draws a dot per
    ///   character.
    AskSecret {
        /// Name of the `connections.toml` entry asking for the secret — the
        /// SAME string that goes into `connection.provide_secret`. Comes
        /// from the core's error, not from the remote server.
        conn: String,
        /// Where it's connecting to, `scheme://host[:port]` and already
        /// redacted by the core (no userinfo). For DISPLAY only, never
        /// reparsed — but mandatory: without it the question isn't
        /// answerable, because a file that may have been edited chose the
        /// name above.
        endpoint: String,
        /// What's been typed so far. [`TypedSecret`] and not `String`: it's
        /// neither printed in a `Debug` nor left on the heap after the drop.
        input: TypedSecret,
        /// The remote path to retry navigating to after delivering it.
        dir: VPath,
        /// The pane that was navigating (see [`Modal::TrustHostKey::pane`]).
        pane: usize,
        /// That navigation's trail direction (see
        /// [`Modal::TrustHostKey::trail`]).
        trail: Trail,
    },
    /// TOFU for a PROJECT's `./.norte/init.lua` (M4 Lua, ADR 0026): a
    /// FOREIGN repo brings a script that would run with the user's
    /// permissions — first contact asks. `y` trusts and evaluates, `n`/Esc
    /// deny (persisted by (path, hash) until the file changes); Enter does
    /// NOT approve (a security decision, same principle as
    /// [`Modal::ApproveAgentOp`]). The approved BYTES live in
    /// [`crate::app::App::lua_pending_trust`] (anti-TOCTOU: what's approved
    /// = what's evaluated).
    TrustLuaInit {
        /// Script path ALREADY SANITIZED by whoever builds the modal
        /// (`detail_for_bar`): display only, never reparsed.
        path: String,
        /// Abbreviated sha256 (32 hex = 128 bits — forging a short collision
        /// costs minutes; the human compares what they see) of the content,
        /// to correlate against `lua-trust.toml` by eye.
        hash_abbrev: String,
    },
    /// Confirming `app.quit` (S2, `[ui] confirm_quit`): opened by
    /// `app.quit`'s dispatch arm in `main.rs` when [`quit_needs_confirm`]
    /// asks for it — with NO data of its own (unlike the GUI's equivalent,
    /// which counts tasks/marks for the title): no security risk to mask,
    /// so it reuses [`crate::app::ALLOW_CONFIRM`]/`DialogHints::confirm`'s
    /// ALLOWLIST/hint without needing its own. The rest of `main.rs`'s
    /// hardcoded Ctrl+Cs do NOT go through here on purpose (see the comment
    /// next to the dispatch arm): that emergency exit stays immediate in
    /// every overlay, same as before S2.
    ConfirmQuit,
    /// Marking (`mark = true`) or unmarking by pattern (`+`/`-`, #103). The
    /// text is the user's RAW query; it gets masked when painted, same as
    /// quick search (a pattern can arrive via PASTE with bidi or invisibles
    /// just as easily).
    MarkPattern {
        /// Mark, or unmark.
        mark: bool,
        /// What's been typed so far.
        pattern: String,
        /// The last failed attempt's diagnostic, to paint under the field.
        /// `None` = nothing confirmed yet.
        error: Option<String>,
    },
    /// Editable destination name (#105): F5/F6 for a SINGLE item, and the
    /// in-place rename (shift+F6 — `to_dir` is the SAME dir). Multi-item
    /// stays in [`Modal::ConfirmTransfer`]: there's no single name to edit.
    /// Free text like [`Modal::Mkdir`].
    TransferName {
        /// Copy or Move (rename = Move with `to_dir` == `from`'s dir).
        kind: TransferKind,
        /// Source, exact bytes.
        from: VPath,
        /// Destination directory (the other pane's; the same one in
        /// rename).
        to_dir: VPath,
        /// The name as EDITABLE text (what gets painted, masked). Only
        /// wins if `touched`; untouched, confirm uses `original`.
        name: String,
        /// `from`'s name's ORIGINAL bytes (rule 1): an unedited F5 copies
        /// these bytes, never the prefill's lossy form.
        original: Vec<u8>,
        /// Was it ever edited? The first push/pop sets it: from then on the
        /// name is the text (#103 doctrine: you edit what you SEE).
        touched: bool,
        /// The source was the MARK (not the cursor): the submit that queues
        /// it CONSUMES it (#105 review MAJOR-1 — mc/TC: the selection gets
        /// consumed on submit, even with a single item). A rename (cursor)
        /// never does.
        from_marks: bool,
        /// The pane's name reinterpretation on OPENING (#98/M1 and #105
        /// review MAJOR-2): a non-UTF8 name's prefill is the TEXT the pane
        /// paints under it (decode #57), not the lossy one — without this a
        /// cp437 file was unrenameable (every edit tripped the U+FFFD
        /// guard). The destination dir's render uses the same one.
        enc: Option<norte_encoding::NameEncoding>,
        /// The last invalid attempt's diagnostic.
        error: Option<String>,
        /// "Doesn't fit" (#149), or `None` if it fits or it isn't known how
        /// much room it takes.
        ///
        /// The SAME two lines as [`Self::ConfirmTransfer`], and that's why
        /// they're here: this dialog is the one that shows up copying ONE
        /// file, and spreading the warnings by item count made copying a
        /// single one say nothing (#343). Space's silence means "fits, or I
        /// don't know"; confinement's silence means "this destination DOES
        /// confine its writes", which is an assertion, not an absence.
        space: Option<String>,
        /// "This destination can't confine writes" (#164, #219).
        confine: Option<String>,
    },
    /// TYPED destination of a transfer: F5/F6 when there's no "other panel"
    /// to copy to.
    ///
    /// With a single listing — the `simple` preset — the `target` role has
    /// no candidate, and L1's rule for that is that the operation ASKS
    /// instead of failing. The address gets typed in its wire form (the
    /// same one you write in `[[hotlist]]`), pre-filled with the panel's
    /// own: the normal thing is editing its tail, not writing the whole
    /// thing.
    ///
    /// Free text like [`Modal::Mkdir`], and for the same reason: what's
    /// typed gets masked when painted. Confirming does NOT transfer — it
    /// opens the modal an F5 with two panels would have opened, which is
    /// where the confirmation lives.
    TransferDest {
        /// Copy or Move.
        kind: TransferKind,
        /// What's been typed so far, in wire form.
        input: String,
        /// The last invalid attempt's diagnostic, under the field.
        error: Option<String>,
    },
    /// Packing (#132). Free text: the NAME of the archive about to be
    /// created, pre-filled with the starting directory or entry's plus the
    /// zip extension.
    ///
    /// The format comes from the name and is shown in the dialog itself:
    /// what travels over the wire is the decision already made, not a name
    /// for the server to guess from (see `ARCHIVE_PACK`).
    Pack {
        /// What's been typed so far.
        name: String,
        /// The last invalid attempt's diagnostic.
        error: Option<String>,
    },
    /// Changing POSIX PERMISSIONS (#314). Free text: the mode in octal,
    /// pre-filled with what's under the cursor.
    ///
    /// In octal and not with `rwx` checkboxes because that's what someone
    /// who knows what they want types — `755`, `600` — and because it's the
    /// form the listing itself shows. A checkbox editor is a different
    /// surface, and this one doesn't block it.
    Chmod {
        /// What's been typed so far.
        mode: String,
        /// What it's going to apply to, resolved on OPENING: what's marked,
        /// or what's under the cursor. Frozen here because between opening
        /// the dialog and confirming it the listing can refresh, and then
        /// "what's marked" would be something else.
        targets: Vec<VPath>,
        /// The last invalid attempt's diagnostic, under the field.
        error: Option<String>,
    },
    /// Splitting a file (#132). Free text: each chunk's size, with a suffix
    /// (`10M`, `700M`, `4096`).
    Split {
        /// What's been typed so far.
        size: String,
        /// The last invalid attempt's diagnostic.
        error: Option<String>,
    },
    /// Create a directory (F7, #104). Free text like [`Modal::MarkPattern`]:
    /// the user's RAW name, masked when painted (a name arrives via paste
    /// with bidi/invisibles just as easily as a pattern).
    Mkdir {
        /// What's been typed so far.
        name: String,
        /// The last invalid attempt's diagnostic (`VPath`'s or the
        /// engine's), painted under the field.
        error: Option<String>,
    },
    /// Saving what's on screen as a new PROFILE (#306, ADR 0079).
    ///
    /// Same mold as [`Modal::Mkdir`]: a field and a diagnostic. What gets
    /// typed is the profile's name, which ends up being a DIRECTORY
    /// (`profiles/<name>/`), so it goes through `valid_profile_name` before
    /// touching disk and the error is painted under the field instead of
    /// being refused silently.
    ProfileSaveAs {
        /// What's been typed so far.
        name: String,
        /// The last invalid attempt's diagnostic.
        error: Option<String>,
    },
    /// Creating an EMPTY file (Shift+F4, #290). Same mold as
    /// [`Modal::Mkdir`] with the other kind of node, and for the same
    /// reason: the DAEMON creates the file (`fs.create`), not the editor, so
    /// a name is needed before launching anything.
    ///
    /// The editor opens AFTERWARD, over the file that already exists.
    /// Letting it create the file — what this key used to do — skipped the
    /// journal and the policy: a `pane.edit-new` over a directory where
    /// policy forbids writing created the file anyway.
    EditNew {
        /// The directory it's created in, BOUND on opening the modal.
        ///
        /// The pane isn't asked again on confirming, and it's the same
        /// decision the window makes (`Pending::CreateFile { dir }`):
        /// between opening the dialog and confirming it, the place under the
        /// pane can have changed, and creating in "wherever focus is now"
        /// creates in a directory the reader wasn't looking at when they
        /// typed the name.
        dir: VPath,
        /// What's been typed so far.
        name: String,
        /// The last invalid attempt's diagnostic.
        error: Option<String>,
    },
    /// `pane.command-line` (#135). Free text, [`Modal::Mkdir`]'s mold: the
    /// user's RAW line, masked when painted.
    ///
    /// What Enter does with it does NOT go through the core: the shell takes
    /// it with the TUI suspended, which is the user acting with their own
    /// permissions and not a norte mutation (design §D — the journal sees
    /// none of this, and saying so is more honest than putting irreversible
    /// entries in the chain).
    ///
    /// # It's the one place in norte where what's painted is code to approve
    ///
    /// Two consequences the S4 review left decided, not inherited:
    ///
    /// - **Multi-line paste confirms on the first newline** (encoding H2).
    ///   The TUI has no bracketed paste — a paste arrives as loose
    ///   keystrokes and crossterm maps `\n` to `Enter` — so the first line
    ///   gets sent alone. The REST doesn't execute: [`crate::app::PendingShell`]
    ///   gets drained with the type-ahead already discarded, so it doesn't
    ///   reach the child nor the TUI's dispatch as commands. It's stated in
    ///   the `shell` topic's honest limits. The full fix (turning on
    ///   bracketed paste and routing `Event::Paste` in the SIX free-text
    ///   surfaces there are) is work for the whole TUI, not this item, and
    ///   doing it halfway would break paste in the other five.
    /// - **ZWJ and NBSP pass unmarked.** `must_mask` allows them knowingly
    ///   (emoji fidelity), which is correct for a file NAME. Here
    ///   `git\u{200D}status` reads the same as `git status` and the shell
    ///   splits it differently. It gets the same treatment as the rest of
    ///   the fields — an exception per surface would be worse to reason
    ///   about — and it's stated: what's truly dangerous (RLO and friends)
    ///   DOES get masked.
    CommandLine {
        /// What's been typed so far.
        command: String,
        /// The last invalid attempt's diagnostic, under the field.
        error: Option<String>,
    },
    /// A batch's checksums, already computed (#311): a READ surface.
    ///
    /// Two faces of the same modal, and that's why they aren't two:
    /// computing shows each file's checksum, and verifying additionally
    /// shows the verdict against what the checksum file published. The list
    /// is the same thing.
    ///
    /// Confirming COPIES the lines to the clipboard in the format
    /// `sha256sum -c` reads; cancelling closes. There's nothing to apply:
    /// nothing gets mutated here.
    Checksums {
        /// What was done: compute or verify (the title's Fluent key).
        title_key: &'static str,
        /// One row per path, in the order they were requested.
        rows: Vec<ChecksumRow>,
        /// First visible row: the whole list is scrollable, and without
        /// this the tail of a large batch couldn't be seen.
        offset: usize,
    },
    /// Prompt for the batch rename TEMPLATE (#310). Free text,
    /// [`Modal::AiRenameInstruction`]'s mold — and its sibling by design:
    /// both produce the SAME reviewable plan, and the only thing that
    /// changes is who proposes the names, a model or a template the human
    /// writes.
    RenameBatchPattern {
        /// The template typed so far (`[N]`, `[E]`, `[C]`).
        pattern: String,
        /// The last invalid attempt's diagnostic, under the field.
        error: Option<String>,
    },
    /// AI rename instruction prompt (M4-IA). Free text, [`Modal::Mkdir`]'s
    /// mold: the user's RAW instruction, masked when painted (an
    /// instruction arrives via paste with bidi/invisibles just as easily as
    /// a name).
    AiRenameInstruction {
        /// What's been typed so far.
        instruction: String,
        /// The last failed attempt's diagnostic, under the field.
        error: Option<String>,
    },
    /// Reviewable rename plan: a DECISION surface. Confirming applies
    /// (content reviewed by the human); Esc/cancel discards.
    ///
    /// The name says `Ai` from its origin (M4-IA) and it's no longer only
    /// its own: since #310 the TEMPLATE batch shares it, which produces the
    /// same plan through the same path. What makes the operation safe isn't
    /// where the names came from, so the review is one, not two.
    AiRenamePlan {
        /// Dir the renames apply to.
        dir: VPath,
        /// from→to pairs from the model (proto, UTF-8 guaranteed).
        entries: Vec<norte_proto::methods::AiRenameEntry>,
        /// First visible pair of the window (audit MAJOR-3): the WHOLE plan
        /// is reviewable by scroll ([`crate::app::App::ai_plan_scroll`]) —
        /// without this, the tail of a plan bigger than
        /// [`crate::app::AI_RENAME_PAIR_LIMIT`] got applied without being
        /// seen.
        offset: usize,
        /// How far the reader has ever gotten.
        ///
        /// A HIGH watermark and not the current position: scrolling back up
        /// doesn't un-read what was already read. Without this, a
        /// two-hundred-rename plan could be approved having seen the first
        /// ten, and the ones that matter could be at row one hundred eighty.
        /// The window demanded it and the terminal didn't: the same
        /// question with two answers, on the surface where it costs the
        /// most (ADR 0077).
        seen: usize,
        /// The BATCH plan that `fs.rename_batch_plan` answered (§17,
        /// ADR 0042): verdicts, whether it's applicable and the `plan_hash`
        /// that has to be returned to run EXACTLY what was shown.
        ///
        /// Born [`norte_frontend::BatchPlan::Pending`] — the modal opens and
        /// fills in once the core answers — and with no APPLICABLE plan,
        /// confirming is DISABLED ([`crate::app::dialog_action`]): there's
        /// no approved hash to send.
        plan: norte_frontend::BatchPlan,
    },
    /// Semantic search query prompt (M4-IA-2). Free text,
    /// [`Modal::AiRenameInstruction`]'s mold: the user's RAW query, masked
    /// when painted (a query arrives via paste with bidi/invisibles just as
    /// easily as an instruction).
    SemanticQuery {
        /// What's been typed so far.
        query: String,
        /// The last failed attempt's diagnostic, under the field.
        error: Option<String>,
    },
    /// Semantic search hits (M4-IA-2): a DECISION surface with a cursor.
    /// Confirming NAVIGATES to the hit under the cursor (cd to the parent +
    /// re-anchor, `on_search_enter`'s mold); Esc/cancel closes.
    SemanticHits {
        /// Index hits, best first (proto, score always finite).
        hits: Vec<norte_proto::methods::SemanticHit>,
        /// First visible hit of the window (follows the cursor).
        offset: usize,
        /// Highlighted hit — the one Enter opens.
        cursor: usize,
    },
}

/// This screen's character cap for a TEXT field:
/// [`Modal::MarkPattern`]'s pattern, a name, an instruction, a template.
///
/// In `chars()`, not bytes — same criterion as
/// [`crate::app::DETAIL_MAX_CHARS`], a multibyte character counts once.
///
/// Used to be called `TEXT_FIELD_MAX_CHARS` because it was born with the
/// mark pattern (#103), and by the time nine modals shared it the name said
/// where it came from instead of what it measures (#121).
pub const TEXT_FIELD_MAX_CHARS: usize = 256;

/// A password's cap is the same one, and now it's CHECKED (#327).
///
/// Since `TypedSecret` lives in the shared crate these are two constants in
/// two crates, and their rustdoc claims they're equal. A claim like that,
/// with nothing tying it down, lasts until someone moves one: then the
/// TUI's password field stops at one length and the window's at another,
/// and no test says so.
const _: () = assert!(
    TEXT_FIELD_MAX_CHARS == norte_frontend::secret::SECRET_MAX_CHARS,
    "a text field's cap and a password's drifted apart"
);

/// Erases the last CHARACTER of a WIRE-form text.
///
/// A character can be up to four bytes and each non-ASCII byte travels as
/// `%XX`, so "erase one character" is between one and twelve text
/// characters. Continuation escapes (`%80`–`%BF`) get removed first and
/// then the leading one; whatever isn't an escape gets erased as usual.
pub(crate) fn pop_wire_char(s: &mut String) {
    /// The byte of a trailing `%XX`, if there is one.
    fn escape_final(s: &str) -> Option<u8> {
        let tail = s.get(s.len().checked_sub(3)?..)?;
        let rest = tail.strip_prefix('%')?;
        u8::from_str_radix(rest, 16).ok().filter(|_| {
            // `from_str_radix` accepts `+7f` and spaces; only hex here.
            rest.len() == 2 && rest.bytes().all(|b| b.is_ascii_hexdigit())
        })
    }

    // A UTF-8 character is at most four bytes: three continuations.
    for _ in 0..3 {
        match escape_final(s) {
            Some(b) if (0x80..=0xBF).contains(&b) => {
                s.truncate(s.len() - 3);
            }
            Some(_) => {
                s.truncate(s.len() - 3);
                return;
            }
            None => {
                s.pop();
                return;
            }
        }
    }
    // Only continuations: the leading one, if present, goes with them.
    if escape_final(s).is_some() {
        s.truncate(s.len() - 3);
    }
}

/// Character cap for [`Modal::TransferDest`]'s destination.
///
/// SEPARATE from [`TEXT_FIELD_MAX_CHARS`] and much larger, because what's
/// measured here is NOT a pattern but an address in WIRE form, which is
/// percent-encoded: an invalid byte costs three characters, so the
/// `name_max_255_invalid_tail` fixture takes up 765 in a SINGLE segment and
/// a deep directory alone goes past 256. With the patterns' cap, the prompt
/// could OPEN already over the limit and then every key was a silent no-op
/// (#246 M3).
pub const TRANSFER_DEST_MAX_CHARS: usize = 8192;

/// S2 (`[ui] confirm_quit`): whether `app.quit`'s dispatch arm must open
/// [`Modal::ConfirmQuit`] instead of closing right away. Pure — the run loop
/// supplies `board_has_active`
/// ([`crate::tasks::TaskBoard::has_active`]), so it's testable without
/// ratatui/tokio. `Auto` (default) is pre-S2 behavior: it confirms only if
/// the task panel has work in flight; `Always`/`Never` are unconditional.
/// Thin wrapper (S review, M6): the three-way decision was byte-identical
/// to the GUI's (`confirm_quit_should_open`) — hoisted to
/// [`norte_frontend::settings::quit_needs_confirm`].
#[must_use]
pub fn quit_needs_confirm(mode: crate::config::ConfirmQuit, board_has_active: bool) -> bool {
    norte_frontend::settings::quit_needs_confirm(mode, board_has_active)
}

/// Result of a key over a modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogOutcome {
    /// Irrelevant key: the dialog stays open.
    Open,
    /// Closed without doing anything.
    Cancelled,
    /// Confirmed (Enter/y).
    Confirmed,
    /// Retry the transfer with this policy.
    Retry(norte_proto::CollisionPolicy),
}

/// Which of the ten free-text prompts is open.
///
/// The methods with their own names (`mkdir_push`, `pack_set_error`…) keep
/// existing because they're what the dispatch tables name; what they share
/// is ONE implementation, and this is the label each one uses to say which
/// prompt it's about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// [`Modal::MarkPattern`].
    MarkPattern,
    /// [`Modal::TransferName`].
    TransferName,
    /// [`Modal::TransferDest`].
    TransferDest,
    /// [`Modal::Pack`].
    Pack,
    /// [`Modal::Split`].
    Split,
    /// [`Modal::Chmod`].
    Chmod,
    /// [`Modal::Mkdir`].
    Mkdir,
    /// [`Modal::EditNew`].
    EditNew,
    /// [`Modal::ProfileSaveAs`].
    ProfileSaveAs,
    /// [`Modal::CommandLine`].
    CommandLine,
    /// [`Modal::AiRenameInstruction`].
    AiRename,
    /// [`Modal::RenameBatchPattern`].
    RenameBatch,
    /// [`Modal::SemanticQuery`].
    Semantic,
}

/// What a prompt does when what's typed hits the cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverLimit {
    /// Stops and stays quiet: the field looks full and the user sees it.
    Silent,
    /// SAYS SO (`modal-command-line-too-long`): in a prompt that can OPEN
    /// already long — a wire address, a command line — stopping silently
    /// turns every key into an unexplained no-op (#246 M3).
    Say,
}

/// What backspace erases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PopMode {
    /// One character of the text.
    Char,
    /// One character of the NAME, whole percent escape included
    /// ([`pop_wire_char`]).
    WireChar,
}

/// An open prompt's text field, with the policy governing it.
///
/// Requested from [`Modal::text_prompt`] and consumed in one operation: it's
/// a mutable borrow of the modal, not state that gets saved.
pub struct TextPrompt<'a> {
    text: &'a mut String,
    error: &'a mut Option<String>,
    touched: Option<&'a mut bool>,
    limit: usize,
    over_limit: OverLimit,
    pop: PopMode,
}

impl TextPrompt<'_> {
    /// What's been typed so far.
    #[must_use]
    pub fn text(&self) -> &str {
        self.text
    }

    /// The diagnostic painted under the field, if there is one.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Adds a character. At the cap, it stops — and says so or not depending
    /// on the prompt.
    pub fn push(self, c: char) {
        if self.text.chars().count() >= self.limit {
            if self.over_limit == OverLimit::Say {
                *self.error = Some(ta(
                    "modal-command-line-too-long",
                    &[("max", &self.limit.to_string())],
                ));
            }
            return;
        }
        self.text.push(c);
        if let Some(touched) = self.touched {
            *touched = true;
        }
        *self.error = None;
    }

    /// Erases backward.
    ///
    /// The diagnostic goes away even if there's nothing to erase: whoever
    /// presses backspace is correcting, and the previous attempt's warning
    /// no longer describes what's there. The editable name's `touched`
    /// does NOT: that only gets set if it actually erased something (#105
    /// review MINOR-5, an empty pop must not narrow the original bytes'
    /// path).
    pub fn pop(self) {
        let erased = match self.pop {
            PopMode::Char => self.text.pop().is_some(),
            PopMode::WireChar => {
                let before_len = self.text.len();
                pop_wire_char(self.text);
                self.text.len() != before_len
            }
        };
        if erased && let Some(touched) = self.touched {
            *touched = true;
        }
        *self.error = None;
    }

    /// Leaves the diagnostic and KEEPS what's typed: a submit that fails
    /// gets corrected and retried, not rewritten from scratch.
    pub fn set_error(self, msg: String) {
        *self.error = Some(msg);
    }
}

impl Modal {
    /// Which free-text prompt this modal is, or `None` if it's a DECISION
    /// one.
    ///
    /// The boundary matters: a decision modal never closes through the
    /// prompts' path (`cancel_*`), it has to be denied by `on_dialog_key`.
    #[must_use]
    pub const fn prompt_kind(&self) -> Option<PromptKind> {
        Some(match self {
            Self::MarkPattern { .. } => PromptKind::MarkPattern,
            Self::TransferName { .. } => PromptKind::TransferName,
            Self::TransferDest { .. } => PromptKind::TransferDest,
            Self::Pack { .. } => PromptKind::Pack,
            Self::Split { .. } => PromptKind::Split,
            Self::Chmod { .. } => PromptKind::Chmod,
            Self::Mkdir { .. } => PromptKind::Mkdir,
            Self::EditNew { .. } => PromptKind::EditNew,
            Self::ProfileSaveAs { .. } => PromptKind::ProfileSaveAs,
            Self::CommandLine { .. } => PromptKind::CommandLine,
            Self::AiRenameInstruction { .. } => PromptKind::AiRename,
            Self::RenameBatchPattern { .. } => PromptKind::RenameBatch,
            Self::SemanticQuery { .. } => PromptKind::Semantic,
            _ => return None,
        })
    }

    /// This modal's text field and its policy, or `None` if it isn't a
    /// prompt.
    ///
    /// Here, in one place, is EVERYTHING that tells the ten apart: what the
    /// field is called, how much it accepts, whether the cap gets stated,
    /// what backspace erases and whether there's a `touched` to set.
    pub fn text_prompt(&mut self) -> Option<TextPrompt<'_>> {
        let (text, error, touched, limit, over_limit, pop) = match self {
            // The batch template shares its mold with the mark pattern:
            // free text, same cap and same erasing.
            Self::MarkPattern { pattern, error, .. }
            | Self::RenameBatchPattern { pattern, error } => (
                pattern,
                error,
                None,
                TEXT_FIELD_MAX_CHARS,
                OverLimit::Silent,
                PopMode::Char,
            ),
            Self::TransferName {
                name,
                touched,
                error,
                ..
            } => (
                name,
                error,
                Some(touched),
                TEXT_FIELD_MAX_CHARS,
                OverLimit::Silent,
                PopMode::Char,
            ),
            Self::TransferDest { input, error, .. } => (
                input,
                error,
                None,
                TRANSFER_DEST_MAX_CHARS,
                OverLimit::Say,
                PopMode::WireChar,
            ),
            Self::Pack { name, error }
            | Self::Mkdir { name, error }
            | Self::ProfileSaveAs { name, error }
            | Self::EditNew { name, error, .. } => (
                name,
                error,
                None,
                TEXT_FIELD_MAX_CHARS,
                OverLimit::Silent,
                PopMode::Char,
            ),
            Self::Split { size, error } => (
                size,
                error,
                None,
                SPLIT_SIZE_MAX_CHARS,
                OverLimit::Silent,
                PopMode::Char,
            ),
            // #314: four octal digits and not one more. The cap stops and
            // stays quiet, which is what a full field already says on its
            // own.
            Self::Chmod { mode, error, .. } => (
                mode,
                error,
                None,
                CHMOD_MAX_CHARS,
                OverLimit::Silent,
                PopMode::Char,
            ),
            Self::CommandLine { command, error } => (
                command,
                error,
                None,
                TEXT_FIELD_MAX_CHARS,
                OverLimit::Say,
                PopMode::Char,
            ),
            Self::AiRenameInstruction { instruction, error } => (
                instruction,
                error,
                None,
                TEXT_FIELD_MAX_CHARS,
                OverLimit::Silent,
                PopMode::Char,
            ),
            Self::SemanticQuery { query, error } => (
                query,
                error,
                None,
                TEXT_FIELD_MAX_CHARS,
                OverLimit::Silent,
                PopMode::Char,
            ),
            _ => return None,
        };
        Some(TextPrompt {
            text,
            error,
            touched,
            limit,
            over_limit,
            pop,
        })
    }
}

/// Character cap for [`Modal::Split`]'s chunk size: short on purpose,
/// because what fits there is `700M`, not a sentence.
pub const SPLIT_SIZE_MAX_CHARS: usize = 32;

/// Cap for [`Modal::Chmod`]'s field (#314): four octal digits — the usual
/// three plus setuid/setgid/sticky's.
pub const CHMOD_MAX_CHARS: usize = 4;

#[cfg(test)]
mod tests {
    use super::*;

    fn mark_pattern() -> Modal {
        Modal::MarkPattern {
            mark: true,
            pattern: String::new(),
            error: None,
        }
    }

    fn transfer_name() -> Modal {
        Modal::TransferName {
            kind: TransferKind::Move,
            from: VPath::root(norte_proto::Scheme::new("mem").unwrap(), None),
            to_dir: VPath::root(norte_proto::Scheme::new("mem").unwrap(), None),
            name: String::from("ab"),
            original: b"ab".to_vec(),
            touched: false,
            from_marks: false,
            enc: None,
            error: None,
            space: None,
            confine: None,
        }
    }

    fn all_ten() -> Vec<(PromptKind, Modal)> {
        vec![
            (PromptKind::MarkPattern, mark_pattern()),
            (PromptKind::TransferName, transfer_name()),
            (
                PromptKind::TransferDest,
                Modal::TransferDest {
                    kind: TransferKind::Copy,
                    input: String::new(),
                    error: None,
                },
            ),
            (
                PromptKind::Pack,
                Modal::Pack {
                    name: String::new(),
                    error: None,
                },
            ),
            (
                PromptKind::Split,
                Modal::Split {
                    size: String::new(),
                    error: None,
                },
            ),
            (
                PromptKind::Chmod,
                Modal::Chmod {
                    mode: String::new(),
                    targets: Vec::new(),
                    error: None,
                },
            ),
            (
                PromptKind::Mkdir,
                Modal::Mkdir {
                    name: String::new(),
                    error: None,
                },
            ),
            (
                PromptKind::EditNew,
                Modal::EditNew {
                    dir: VPath::parse("mem:///").unwrap_or_else(|_| unreachable!("test wire")),
                    name: String::new(),
                    error: None,
                },
            ),
            (
                PromptKind::CommandLine,
                Modal::CommandLine {
                    command: String::new(),
                    error: None,
                },
            ),
            (
                PromptKind::AiRename,
                Modal::AiRenameInstruction {
                    instruction: String::new(),
                    error: None,
                },
            ),
            (
                PromptKind::Semantic,
                Modal::SemanticQuery {
                    query: String::new(),
                    error: None,
                },
            ),
        ]
    }

    /// All ten text prompts say they're prompts, and typing reaches the
    /// field each one calls something different.
    #[test]
    fn the_ten_prompts_expose_their_field() {
        for (kind, mut m) in all_ten() {
            assert_eq!(
                m.prompt_kind(),
                Some(kind),
                "{kind:?} doesn't say it's a prompt"
            );
            m.text_prompt().expect("text field").push('x');
            let tp = m.text_prompt().expect("text field");
            assert!(tp.text().ends_with('x'), "{kind:?} didn't receive the key");
        }
    }

    /// A DECISION modal has no field to type into.
    #[test]
    fn a_decision_modal_is_not_a_prompt() {
        let mut m = Modal::ConfirmQuit;
        assert_eq!(m.prompt_kind(), None);
        assert!(m.text_prompt().is_none());
    }

    /// The cap is per prompt, and the split one's is the short one.
    #[test]
    fn splits_cap_stops_silently() {
        let mut m = Modal::Split {
            size: "9".repeat(32),
            error: None,
        };
        m.text_prompt().expect("field").push('9');
        let tp = m.text_prompt().expect("field");
        assert_eq!(tp.text().chars().count(), 32, "the cap didn't stop it");
        assert!(tp.error().is_none(), "the split cap is silent");
    }

    /// The command line's DOES say so (#246 M3).
    #[test]
    fn the_command_lines_cap_gets_stated() {
        let mut m = Modal::CommandLine {
            command: "x".repeat(TEXT_FIELD_MAX_CHARS),
            error: None,
        };
        m.text_prompt().expect("field").push('y');
        let tp = m.text_prompt().expect("field");
        assert_eq!(tp.text().chars().count(), TEXT_FIELD_MAX_CHARS);
        assert!(tp.error().is_some(), "the line's cap gets stated");
    }

    /// The destination's backspace erases the WHOLE escape, not one
    /// character of the wire text (#246 M3).
    #[test]
    fn the_destinations_backspace_erases_a_whole_escape() {
        let mut m = Modal::TransferDest {
            kind: TransferKind::Copy,
            input: String::from("mem:///caf%C3%A9"),
            error: None,
        };
        m.text_prompt().expect("field").pop();
        let tp = m.text_prompt().expect("field");
        assert_eq!(tp.text(), "mem:///caf");
    }

    /// The editable name marks `touched` only if backspace erased something
    /// (#105 review MINOR-5).
    #[test]
    fn the_editable_name_marks_touched_only_if_it_erased() {
        let mut m = transfer_name();
        m.text_prompt().expect("field").pop();
        assert!(
            matches!(m, Modal::TransferName { touched: true, .. }),
            "a pop that erases sets touched"
        );

        let mut empty = transfer_name();
        if let Modal::TransferName { name, touched, .. } = &mut empty {
            name.clear();
            *touched = false;
        }
        empty.text_prompt().expect("field").pop();
        assert!(
            matches!(empty, Modal::TransferName { touched: false, .. }),
            "an empty pop doesn't narrow the original bytes' path"
        );
    }

    /// Backspace clears the diagnostic even if it erases nothing.
    #[test]
    fn backspace_on_empty_clears_the_diagnostic() {
        let mut m = Modal::Mkdir {
            name: String::new(),
            error: Some(String::from("already exists")),
        };
        m.text_prompt().expect("field").pop();
        assert!(m.text_prompt().expect("field").error().is_none());
    }
}
