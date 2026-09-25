//! The table backend the tests share: a deterministic directory tree, with
//! no daemon and no network.
//!
//! Each test uses the part it needs — the parity one does not delete, the
//! controller one does not compare trees — so there is spare code here for
//! any one of them on its own. That is the price of having ONE fake instead
//! of three.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::future::BoxFuture;
use norte_proto::{DeleteMode, Entry, EntryKind, Error, VPath};
use norte_ui_host::backend::{HostBackend, HostTask};

/// A one-way latch: it opens once and stays open.
///
/// `notify_waiters` only wakes whoever is ALREADY waiting, so the flag is
/// what rules and the notice only avoids polling. Once open, any later
/// request just passes through — which is what is needed when the host
/// re-requests the listing and the new stream arrives here again.
#[derive(Default)]
pub struct Gate {
    open: std::sync::atomic::AtomicBool,
    notice: tokio::sync::Notify,
}

impl Gate {
    /// Lets the drain through, now and forever.
    pub fn open(&self) {
        self.open.store(true, Ordering::SeqCst);
        self.notice.notify_waiters();
    }

    async fn wait(&self) {
        loop {
            if self.open.load(Ordering::SeqCst) {
                return;
            }
            // The future is armed BEFORE the second check: arming it after
            // would lose an `open` that landed right in between.
            let waiting = self.notice.notified();
            if self.open.load(Ordering::SeqCst) {
                return;
            }
            waiting.await;
        }
    }
}

/// A table backend: for each directory, the names it contains and what
/// class they are.
//
// `clippy::struct_excessive_bools`: allowed on purpose. These are
// INDEPENDENT knobs of a test double — the listing comes lazy, the delete
// really removes, the provider writes the parent under another spelling —
// and any combination of them is a real scenario. Folding them into a state
// machine would invent states that do not exist; wrapping each in a
// two-variant enum would leave every test writing `Lazy::Yes,
// RealDelete::No` for nothing: the field name already says which question
// it answers.
#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts: each field says which question it answers"
)]
pub struct Fake {
    /// A notice for every thing the double RECORDS.
    ///
    /// It is what turns "sleep 30 ms and look" into "wait until it happens".
    /// The actor queues every mutation with `tokio::spawn` and answers the
    /// ack before the task runs, so the test that wants to see the queued
    /// work has to wait for SOMETHING; without this, that something was the
    /// clock.
    ///
    /// `notify_waiters` only wakes whoever is ALREADY waiting, so whoever
    /// waits arms the future before looking again (`Fake::until`), the same
    /// way `Gate::wait` does.
    ///
    /// `Arc` because some recordings happen OUTSIDE `&self`: a task's
    /// cancellation closure stays alive after the double is no longer at
    /// hand, and it also has to notify.
    pub pulse: Arc<tokio::sync::Notify>,
    /// `dir wire` → `(name, is_dir)`.
    pub tree: HashMap<String, Vec<(Vec<u8>, bool)>>,
    /// The EXACT kind of an entry, by its wire path. See
    /// [`Fake::put_kind`]: `tree` only knows about directories and files.
    pub kinds: HashMap<String, EntryKind>,
    pub listings: AtomicUsize,
    /// Reads REQUESTED and already SERVED: listings, probes and contents.
    ///
    /// The two numbers only diverge with `delay_ms`, and that is exactly
    /// where it is needed: a test that wants to see what the host does with
    /// a LATE response has to know when it arrived. It used to guess that by
    /// sleeping longer than the delay. `served == requests` is "nothing is
    /// in flight anymore", which is the question those tests actually ask —
    /// and it does not require counting by hand how many responses each case
    /// puts in flight.
    pub requests: AtomicUsize,
    /// The other half of `requests`: `Arc` because what bumps it is the
    /// response, which runs in its own task once the double is no longer at
    /// hand.
    pub served: Arc<AtomicUsize>,
    /// Artificial delay, to provoke the race of a late response.
    ///
    /// This `sleep` stays: it is the latency the double SIMULATES, not a
    /// test's bet on how long the actor takes. What is not guessed is when
    /// it finished — `served` says that.
    pub delay_ms: u64,
    /// Stops the stream RIGHT AFTER the first page, until the test opens it.
    ///
    /// It is the only way to be INSIDE the window where `in_flight` has
    /// already cleared and `draining` is still alive, which is where the bug
    /// this knob exists to test lives. A `sleep` would work by coincidence;
    /// this does not depend on the clock.
    pub gate_drain: Option<Arc<Gate>>,
    /// The session the daemon returns, and whether this window owns it.
    pub session: std::sync::Mutex<(norte_proto::methods::Session, bool)>,
    /// The LAST thing written, to check what the host saves.
    pub written: std::sync::Mutex<Option<serde_json::Value>>,
    /// The last transfer asked for the QUEUE (ADR 0149).
    pub queued: std::sync::Mutex<bool>,
    /// ALL the bodies that were tried, in order — rejected ones included. It
    /// is what lets you see that a retry sends something DIFFERENT (#316),
    /// which is the difference between degrading and repeating the same
    /// error.
    pub placed: std::sync::Mutex<Vec<serde_json::Value>>,
    /// How many consecutive `session_put` calls are rejected for SIZE before
    /// one is accepted. `0` (the default) = none.
    pub rejections_by_size: std::sync::Mutex<u32>,
    /// The write fails with a conflict: another window wrote in between.
    pub conflict: bool,
    /// The listing comes LAZY, like the local provider's: no size, no date.
    /// Whoever wants them can probe.
    pub lazy: bool,
    /// `stat` answers with the name in UPPERCASE: another spelling of the
    /// same thing, like a case-insensitive server or an HFS+ in NFD.
    pub stat_grita: bool,
    /// The `attrs` requested in each listing, in order.
    pub attrs_requests: std::sync::Mutex<Vec<Vec<String>>>,
    /// The batches `dir_size` requested, in order: it is what lets you check
    /// that what is MARKED is counted, and in a SINGLE Task.
    pub counts: std::sync::Mutex<Vec<Vec<VPath>>>,
    /// What was requested to be packed, with its format and its base.
    pub packed: std::sync::Mutex<Vec<norte_proto::methods::ArchivePackParams>>,
    /// The containers that were asked to be tested.
    pub checked: std::sync::Mutex<Vec<norte_proto::methods::ArchiveTestParams>>,
    /// What was asked to be split, with its chunk size already in bytes.
    pub split: std::sync::Mutex<Vec<norte_proto::methods::FileSplitParams>>,
    /// The chunks that were asked to be combined.
    pub joined: std::sync::Mutex<Vec<norte_proto::methods::FileCombineParams>>,
    /// What the daemon answers to `connection.list` (#264). Defaults to an
    /// empty list, which is what whoever has none configured sees.
    pub connections:
        std::sync::Mutex<Option<Result<norte_proto::methods::ConnectionListResult, Error>>>,
    /// The sessions that were asked to be CLOSED, in order (#140).
    pub closed: std::sync::Mutex<Vec<VPath>>,
    /// What `connection.close` answers. `None` = "yes, there was one".
    pub close: std::sync::Mutex<Option<Result<bool, Error>>>,
    /// Content by path, for the viewer.
    pub content: HashMap<String, Vec<u8>>,
    /// The paths that were probed, in order: it is what lets you check that
    /// a failed probe does not repeat in a loop.
    pub probes: std::sync::Mutex<Vec<VPath>>,
    /// What was asked to be deleted, in order.
    pub deleted: std::sync::Mutex<Vec<(VPath, DeleteMode)>>,
    /// A delete REMOVES the entry from the tree, like in real life.
    ///
    /// Off by default so as not to move the tests that only look at what was
    /// requested. On, it is the only way to check what a listing does when
    /// it arrives with ONE FEWER entry — which is where a cursor by index
    /// stops naming the same file.
    pub delete_for_real: bool,
    /// The error a delete is REJECTED with before queuing anything. `None` =
    /// the delete gets queued.
    ///
    /// Behind a `Mutex` so a test can FIX IT midway: the case that matters is
    /// a daemon that refuses once and accepts the next time.
    pub error_on_delete: std::sync::Mutex<Option<Error>>,
    /// The wire paths of what has already been deleted, which `list` skips.
    pub disappeared: std::sync::Mutex<std::collections::HashSet<String>>,
    /// The provider writes the PARENT of its entries under another spelling
    /// than the one it was asked for (the last component in uppercase).
    ///
    /// This is what really happens on macOS (NFD vs NFC) and against a
    /// case-insensitive server, and what makes an entry's parent and the
    /// pane's directory two strings for the same place.
    pub padre_different: bool,
    /// How many times cancelling the launched task was requested.
    pub cancellations: Arc<AtomicUsize>,
    /// The progress sender of the last task, for the test to move it.
    pub progress: std::sync::Mutex<Option<tokio::sync::watch::Sender<norte_proto::TaskProgress>>>,
    /// The sender of the `create_file` task, kept only so it does NOT drop.
    ///
    /// It is the only one of the double's tasks that is born running and
    /// finishes behind, so it is the only one whose channel has to stay open
    /// while the host is about to read the outcome. Separate from `progress`
    /// because tests MOVE that one, and nobody moves this one.
    pub progress_create:
        std::sync::Mutex<Option<tokio::sync::watch::Sender<norte_proto::TaskProgress>>>,
    /// The senders of ALL the transfer tasks, by id.
    ///
    /// A single slot does not work for a batch: when the second one arrived
    /// it dropped the first one's `Sender`, the host's pumping saw
    /// `changed()` fail, and that row stayed `Running` forever. In other
    /// words the double could not move a batch, which is exactly the
    /// expensive case.
    pub progress_by_task:
        std::sync::Mutex<HashMap<u64, tokio::sync::watch::Sender<norte_proto::TaskProgress>>>,
    /// The extension catalogue `plugin.list` answers.
    ///
    /// Behind a `Mutex` because governance CHANGES it: the host re-requests
    /// the catalogue after approving or enabling, and a fake that always
    /// answered the same thing would let a screen say "approved" through
    /// without the daemon having confirmed it.
    pub plugins: std::sync::Mutex<Vec<norte_proto::methods::PluginInfo>>,
    /// Each extension's `help.md`, by id. An absent id answers the way a
    /// daemon without that page would: empty markdown.
    pub pages: HashMap<String, String>,
    /// The styled preview a previewer answers, by wire. Absent = no
    /// previewer applies, which is NOT an error.
    pub previews: HashMap<String, norte_proto::methods::PluginPreviewStyled>,
    /// The THUMBNAILS a plugin would give by wire path (ADR 0107).
    pub thumbnails: HashMap<String, norte_proto::methods::PluginThumbnail>,
    /// The width each styled-preview request said (0.66.0), in order. It is
    /// what lets you check that the viewport CROSSES over.
    pub preview_widths: std::sync::Mutex<Vec<Option<u32>>>,
    /// How many entries the provider says it skipped. `None` = it does not
    /// keep count, which is NOT the same as zero.
    pub skipped: Option<u64>,
    /// The badge a decorator puts on each path, by wire. Empty = NO decorator
    /// consented, which is what the daemon answers.
    pub decorations: HashMap<String, String>,
    /// The ICON a second decorator, of `icon` slot (ADR 0105), puts on each
    /// path, by wire. Empty = no icon decorator.
    pub icons: HashMap<String, String>,
    /// The classes that arrived with each decorated batch, in order: what
    /// lets you check that the window SENDS the class, without which an icon
    /// decorator does not know what is a folder.
    pub classes_decorated: std::sync::Mutex<Vec<Vec<norte_proto::EntryKind>>>,
    /// The batches that were asked to be decorated, in order. It is what
    /// lets you check that only the WINDOW is requested.
    pub decorated: std::sync::Mutex<Vec<Vec<VPath>>>,
    /// The value of a plugin column, by `(column, wire)`.
    pub column_values: HashMap<(String, String), String>,
    /// What was requested from `plugin.column_values`, in order.
    pub columns_requested: std::sync::Mutex<Vec<(String, String, Vec<VPath>)>>,
    /// The frame `plugin.panel_render` answers (phase 3). `None` = no
    /// consented plugin paints that panel, which is the case for almost all
    /// tests.
    pub marco_de_panel: Option<norte_proto::methods::PanelFrame>,
    /// What was requested from `plugin.panel_render`, in order: this checks
    /// WHAT the guest is told — the directory, the size without the frame,
    /// the row under the cursor — and that it is not asked for the same
    /// thing twice.
    pub panels_requests: std::sync::Mutex<Vec<norte_proto::methods::PluginPanelRenderParams>>,
    /// What a search answers, by pattern: `(glob, hits)`.
    pub findings: HashMap<String, Vec<VPath>>,
    /// The patterns that were searched, in order.
    pub searches: std::sync::Mutex<Vec<String>>,
    /// And the FULL PARAMETERS of each one, also in order.
    ///
    /// Separate from the pattern because since bridge 91 the window sends
    /// seven fields and four switches: a test that can only look at the glob
    /// cannot tell "the filter was applied" from "it was ignored", which is
    /// exactly what needs to be shown.
    pub params_search: std::sync::Mutex<Vec<norte_proto::methods::FsSearchParams>>,
    /// The volumes `host.volumes` answers.
    pub volumes: Vec<norte_proto::methods::Volume>,
    /// How each location FOLDS names (#268/#274). Key: the directory's wire.
    /// Absent = what `Capabilities::default()` says.
    ///
    /// This is the knob that was missing to be able to write these tests:
    /// without it no double could pretend to be an APFS, an NTFS or an
    /// exFAT, and the corpus's case-twin fixtures had nothing to run
    /// against.
    pub capabilities: std::collections::HashMap<String, norte_proto::Capabilities>,
    /// `fs.capabilities` FAILS, so the slot never gets any.
    ///
    /// It is the state that loses data if someone confuses it with "no
    /// trash", and without this knob it could not be written: the double
    /// always answered something.
    pub capabilities_error: bool,
    /// Plugin directories that failed to load: `(dir, reason)`.
    pub load_errors: Vec<(String, String)>,
    /// The BYTES of a load error's directory (#265), by its string. What a
    /// 0.53 daemon sends; absent = a 0.52 peer.
    pub payload_bytes: std::collections::HashMap<String, Vec<u8>>,
    /// Each extension's `[config]` schema, by id.
    pub schemes: HashMap<String, Vec<norte_proto::methods::PluginConfigKeyWire>>,
    /// What `ai.rename_plan` answers. `None` = the daemon fails.
    pub plan_ia: Option<Vec<(String, String)>>,
    /// What `plugin.rename_plan` answers (C3). `None` = the daemon fails.
    pub plan_renamer: Option<Vec<(String, String)>>,
    /// The phrase the renamer REFUSES with (#332): wins over `plan_renamer`.
    pub renamer_refuses: Option<String>,
    /// Which renamer was requested, with which names: `(plugin, renamer,
    /// names)`.
    pub renamers_requests: std::sync::Mutex<Vec<(String, String, Vec<String>)>>,
    /// How long the model TAKES. It is what opens the window in which the
    /// reader can dismiss the review before the plan arrives.
    pub delay_ia_ms: u64,
    /// Holds back the AI plan until the test opens it.
    ///
    /// `delay_ia_ms` simulates latency, and that is good for seeing what
    /// the window does WHILE the model thinks. What it is not good for is
    /// synchronizing: a test that needs the plan to stay in flight while it
    /// types is betting that its keystrokes take less time than the clock,
    /// and under load that bet loses. With the gate, "still thinking" is a
    /// fact and not a time window. Same latch [`Gate`] uses for the drain,
    /// and for the same reason.
    pub gate_ia: Option<Arc<Gate>>,
    /// The instructions that were requested, in order.
    pub instructions: std::sync::Mutex<Vec<String>>,
    /// The NAMES that travelled with each plan (#121): empty = the whole
    /// directory. It is what lets you check that marking five files does not
    /// send the provider the thousand in the directory.
    pub names_ia: std::sync::Mutex<Vec<Vec<String>>>,
    /// What `session.release` answers (phase 9): whether this connection
    /// owned it. `false` is the branch that matters — the one that must NOT
    /// launch anything.
    pub drops_the_session: bool,
    /// How many times releasing the session was requested.
    pub loose: std::sync::atomic::AtomicUsize,
    /// What an ORGANIZE plan answers (phase 8), whether from the model or a
    /// plugin: `(current name, relative destination)`. `None` = the daemon
    /// fails.
    pub plan_organize: Option<Vec<(String, String)>>,
    /// The phrase the organize-plan producer REFUSES with: wins over
    /// `plan_organize`.
    pub organize_refuses: Option<String>,
    /// The token that comes with the organize plan. `None` = a daemon that
    /// sends a plan WITHOUT a token, which is a plan that cannot be approved
    /// — and this is the way to check that the review does not open.
    pub organize_hash: Option<norte_proto::methods::PlanHash>,
    /// Which organizer was requested and with which names: `(plugin,
    /// organizer, names)`.
    pub organizers_requests: std::sync::Mutex<Vec<(String, String, Vec<String>)>>,
    /// The organize plans that were sent to EXECUTE: `(dir, moves, hash)`.
    pub organized: std::sync::Mutex<
        Vec<(
            VPath,
            Vec<norte_proto::methods::OrganizeMove>,
            norte_proto::methods::PlanHash,
        )>,
    >,
    /// The verdict `fs.rename_batch_plan` answers. `None` = it fails.
    pub verdict: Option<norte_proto::methods::FsRenameBatchPlanResult>,
    /// The pairs the verdict was requested with, in order.
    pub verdicts_requests: std::sync::Mutex<Vec<Vec<norte_proto::methods::RenamePair>>>,
    /// The batches that were sent to EXECUTE: `(dir, pairs, hash)`.
    pub batches: std::sync::Mutex<
        Vec<(
            VPath,
            Vec<norte_proto::methods::RenamePair>,
            norte_proto::methods::PlanHash,
        )>,
    >,
    /// The report `fs.rename_batch_report` answers. `None` = the daemon does
    /// not know how to report (`Unsupported`), which is its own case: it
    /// must not be confused with "the batch went fine".
    pub report: std::sync::Mutex<Option<norte_proto::methods::FsRenameBatchReportResult>>,
    /// The task ids whose report was requested, in order.
    pub informes_requests: std::sync::Mutex<Vec<u64>>,
    /// What `sync.plan` answers: its steps and the closing. `None` = the
    /// method fails with `Unsupported`.
    pub plan_de_sync: std::sync::Mutex<
        Option<(
            Vec<norte_proto::methods::SyncStep>,
            norte_proto::methods::SyncPlanDone,
        )>,
    >,
    /// The report `sync.report` answers. `None` = `Unsupported`.
    pub sync_report: std::sync::Mutex<Option<norte_proto::methods::SyncReportResult>>,
    /// The agent sessions that were asked to be undone, in order.
    pub undone: std::sync::Mutex<Vec<String>>,
    /// What `journal.list` answers, newest to oldest, and really paginated by
    /// `before_seq`. `None` = `Unsupported` (a daemon with no journal).
    pub journal: Option<Vec<norte_proto::methods::JournalRow>>,
    /// The cuts that were asked to be undone (`journal.undo_after`), with
    /// their ceiling, in order.
    pub undone_until: std::sync::Mutex<Vec<(i64, Option<i64>)>>,
    /// How many times the extension catalogue has been requested.
    pub catalogos_requests: std::sync::atomic::AtomicU64,
    /// What OUTCOME a search finishes with.
    ///
    /// The double always completed them, so "failed" and "was cancelled"
    /// could not be written as a test — which is exactly why the window
    /// painted all three the same ("N hits") without anything complaining.
    pub search_outcome: Option<norte_proto::TaskState>,
    /// The search does not even get QUEUED, and with this error.
    ///
    /// It is a different path from the one above: there, there is a Task and
    /// its progress carries the outcome; here there is no Task, so there is
    /// no progress to carry it — and without this knob that path could not
    /// be written as a test, which is why the view kept saying "searching…"
    /// forever.
    pub search_error: Option<Error>,
    /// How many times the volumes have been enumerated.
    ///
    /// It counts this so a NEGATIVE test can anchor on it: "the dialog says
    /// nothing" stays green if nobody asked, and then it does not prove that
    /// staying silent is the answer — only that there was no question.
    pub volumes_requests: std::sync::atomic::AtomicU64,
    /// The governance changes requested, in order (`approval:id:true`…).
    pub governance: std::sync::Mutex<Vec<String>>,
    /// What a governance change fails with, if it fails.
    pub governance_error: std::sync::Mutex<Option<Error>>,
    /// The keys written, in order: `(plugin, key, value)`.
    pub writes: std::sync::Mutex<Vec<(String, String, String)>>,
    /// What `plugin.set_config` fails with, if it fails.
    pub error_on_write: std::sync::Mutex<Option<Error>>,
    /// The commands run, in order: `(plugin, command)`.
    pub executed: std::sync::Mutex<Vec<(String, String)>>,
    /// What `plugin.run_command` answers. `None` = empty output, which is
    /// NOT an error: a command can print nothing.
    pub command_output: std::sync::Mutex<Option<Result<String, Error>>>,
    /// What `sync.apply` fails with, if it fails.
    pub error_on_apply: std::sync::Mutex<Option<Error>>,
    /// The task ids that were asked to stop, in order.
    pub canceled_by_id: Arc<std::sync::Mutex<Vec<u64>>>,
    /// The hashes apply was requested with, in order.
    pub applied: std::sync::Mutex<Vec<norte_proto::methods::PlanHash>>,
    /// The plans that were requested: `(source, destination, mode)`.
    pub planes_requests: std::sync::Mutex<Vec<(VPath, VPath, norte_proto::methods::SyncMode)>>,
    /// The rows `fs.compare` answers, in a single batch. `None` = the method
    /// fails with `Unsupported`.
    pub rows_compared: std::sync::Mutex<Option<Vec<norte_proto::methods::CompareRow>>>,
    /// The comparisons that were requested: `(left, right)`.
    pub comparisons: std::sync::Mutex<Vec<(VPath, VPath)>>,
    /// What `index.search_semantic` answers. `None` = `NotFound` (no index),
    /// which is the case that needs to be read correctly.
    pub semantic: std::sync::Mutex<Option<Vec<norte_proto::methods::SemanticHit>>>,
    /// The semantic queries that were requested, with their `k`.
    pub semanticas_requested: std::sync::Mutex<Vec<(String, u32)>>,
    /// The report `policy.undo_report` answers. `None` = `Unsupported`.
    pub report_undo: std::sync::Mutex<Option<norte_proto::methods::PolicyUndoReportResult>>,
    /// The task ids whose undo report was requested, in order.
    pub informes_undo_requests: std::sync::Mutex<Vec<u64>>,
    /// The report `archive.pack_report` answers (#250). `None` =
    /// `Unsupported`, which is what an N-1 daemon answers.
    pub report_pack: std::sync::Mutex<Option<norte_proto::methods::ArchivePackReportResult>>,
    /// The ids whose pack report was requested, in order.
    pub informes_pack_requests: std::sync::Mutex<Vec<u64>>,
    /// The ids whose card was requested, in order.
    pub cards_requested: std::sync::Mutex<Vec<String>>,
    /// The ids that were requested from `plugin.help`, in order: it is what
    /// lets you check that a page is requested ONCE and that an invalid id
    /// never reaches the wire.
    pub pages_requested: std::sync::Mutex<Vec<String>>,
    /// The connection's channels, so the test can push events and foreign
    /// tasks the way a daemon would.
    pub eventos:
        std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<norte_client::ConnEvent>>>,
    pub foreign: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<HostTask>>>,
    /// The `connection.degraded` channel, so the test can push one.
    pub degraded: std::sync::Mutex<
        Option<tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::ConnectionDegraded>>,
    >,
    /// The FIRST `list` on a directory that exists fails with this (#327),
    /// and it is consumed: the retry after handing over the secret goes
    /// through.
    pub asks_secret: std::sync::Mutex<Option<Error>>,
    /// What was handed over by `provide_secret` (#327): `(conn, secret)`, in
    /// order.
    ///
    /// The secret is stored IN THE CLEAR here on purpose: it is what the test
    /// needs to be able to check — that it arrives as-is and to the
    /// connection that asked for it — and this double only lives inside a
    /// test.
    pub secrets_dados: std::sync::Mutex<Vec<(String, String)>>,
    /// What `provide_secret` answers. `None` = it accepts it.
    pub secret: std::sync::Mutex<Option<Result<(), Error>>>,
    /// The `connection.failed` channel (#322), so the test can push one.
    /// Separate from the one above, like the real backend.
    pub failed: std::sync::Mutex<
        Option<tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::ConnectionFailed>>,
    >,
    /// The `plugin.notice` channel (ADR 0100), so the test can push one.
    pub notices_plugin: std::sync::Mutex<
        Option<tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::PluginNotice>>,
    >,
    /// The directories that were asked to be created.
    pub created: std::sync::Mutex<Vec<VPath>>,
    /// What a `stat` finds on something this fake CREATED (#303).
    ///
    /// `EntryKind::File` — the default — is normal life: the file the daemon
    /// just put there is still there and still a file. Setting it to
    /// `Symlink` is the whole attack: between creating the name and opening
    /// it, someone with write permission on that directory unlinks it and
    /// leaves a symlink with the same name. The `put` tree does not work for
    /// this: what gets created is not in it, and whoever checks it asks about
    /// the created path.
    pub created_appears_as: Option<EntryKind>,
    /// The permission batches that were requested: paths and mode (#314).
    pub permissions: std::sync::Mutex<Vec<(Vec<VPath>, u32)>>,
    /// The paths of each checksum batch that was requested (#311).
    pub checksums_requested: std::sync::Mutex<Vec<Vec<VPath>>>,
    /// The directory of each disk-usage measurement that was requested
    /// (phase 4).
    ///
    /// It exists so you can assert it was measured ONCE: the probe runs
    /// after every message from the actor, so half its value is in never
    /// asking for the same thing again. Without this list, "measured" and
    /// "measures in a loop" look the same.
    pub maps_requests: std::sync::Mutex<Vec<VPath>>,
    /// The task ids whose checksum REPORT was requested, in order.
    ///
    /// It exists so you can wait for the report to have come back: a test
    /// that asserts a half-finished report does NOT open anything has to
    /// have had it in hand, or it would be checking that it simply had not
    /// arrived yet.
    pub checksums_informes_requests: std::sync::Mutex<Vec<u64>>,
    /// The report `checksum_report` returns. Empty and complete by default —
    /// a test that wants digests sets it.
    pub checksums_report: std::sync::Mutex<norte_proto::methods::FsChecksumReportResult>,
    /// The attribute catalogue the fake daemon returns.
    pub catalog: std::sync::Mutex<norte_proto::AttrCatalog>,
    /// What `policy.decide` fails with. `None` = the decision arrives.
    pub error_on_decide: std::sync::Mutex<Option<Error>>,
    /// The approvals channel, so the test can push one.
    pub approvals: std::sync::Mutex<
        Option<tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::PolicyApprovalRequired>>,
    >,
    /// The decisions that were sent: `(id, approved)`.
    pub decisiones: std::sync::Mutex<Vec<(u64, bool)>>,
    /// What was requested to be transferred, in order:
    /// `(source, destination, move, collision policy)`.
    ///
    /// The policy is recorded because it is the ONLY parameter that separates
    /// "the task fails" from "the destination file disappears": without
    /// pinning it, changing it to `Overwrite` would leave the whole suite
    /// green.
    pub transfers: std::sync::Mutex<Vec<(VPath, VPath, bool, norte_proto::CollisionPolicy)>>,
    /// The state a transfer's task is BORN in. `Running` (the default) lets
    /// the test move it; `Failed` is the collision a daemon with
    /// `on_collision = Fail` returns.
    pub state_transfer: Option<norte_proto::TaskState>,
    /// Ids a transfer has already handed out: two copies are two tasks, and
    /// returning the same id would merge them into one board row.
    pub next_task: AtomicUsize,
    /// QUEUING a transfer fails with this error: a read-only provider, a
    /// scope that does not reach. It is not the same as a task that fails —
    /// this happens before there is a task — and the screen has to tell
    /// them apart.
    pub transfer_rejected: Option<Error>,
    /// What `log.tail` delivers on the NEXT round: `(lines, next)` (#328).
    /// Served ONCE and then emptied.
    ///
    /// It is emptied because a real ring never delivers again what it
    /// already gave: a double that repeated would make an extra poll
    /// duplicate lines in the panel, and then a test that counts occurrences
    /// would be measuring the clock instead of the cursor's chaining.
    pub log_remote: std::sync::Mutex<Option<(Vec<norte_proto::methods::LogLine>, u64)>>,
    /// The `next` of the last response served. `None` = this daemon has NO
    /// log to serve and answers `Unsupported`, which is what one of the same
    /// version compiled without the `logging` feature does — the only
    /// reachable degradation case, because an older one dies at
    /// `initialize`.
    pub log_next: std::sync::Mutex<Option<u64>>,
    /// The cursors `log.tail` was requested with, in order. It is what lets
    /// you check that the first round sends `None` ("whatever there is") and
    /// the following ones chain.
    pub log_cursors: std::sync::Mutex<Vec<Option<u64>>>,
    /// The level the daemon says it has set: BOTH `log.level` and every
    /// `log.tail` answer it, just like the real daemon. `None` = it knows
    /// nothing about logging and `log.level` answers `Unsupported`.
    pub level_remote: std::sync::Mutex<Option<String>>,
    /// The levels that were requested from the daemon, in order.
    pub levels_requests: std::sync::Mutex<Vec<String>>,
    /// Holds back the `log.tail` response until the test releases it.
    ///
    /// It is the only way to be INSIDE the window where a request is still
    /// in flight while the panel closes and reopens, which is where the
    /// question of whether a stale response can slip into the new panel
    /// lives. A `sleep` would work by coincidence; this does not depend on
    /// the clock.
    pub gate_log: Option<Arc<Gate>>,
}

impl Fake {
    /// A directory with loose files.
    pub fn con(names: &[&'static str]) -> Arc<Self> {
        let mut f = Self::default();
        f.put(
            "mem:///casa",
            names.iter().map(|n| (n.as_bytes().to_vec(), false)),
        );
        Arc::new(f)
    }

    pub fn put(&mut self, dir: &str, entries: impl IntoIterator<Item = (Vec<u8>, bool)>) {
        self.tree
            .insert(dir.to_owned(), entries.into_iter().collect());
    }

    /// The exact KIND of an entry, when "directory or file" is not enough.
    ///
    /// `put` only distinguishes those two things, which is what almost every
    /// test needs. A SYMLINK is another: `Enter` on it does not mean the same
    /// as on a file, and without being able to make one that divergence
    /// between frontends could not be written as a test.
    pub fn put_kind(&mut self, wire: &str, kind: EntryKind) {
        self.kinds.insert(wire.to_owned(), kind);
    }

    /// Arms the NEXT `log.tail` response (#328).
    ///
    /// From here on the double knows about logging: rounds after this one
    /// answer with no new lines and the same `next`, which is what a ring
    /// whose queue has already been drained does.
    pub fn answers_log_tail(&self, lines: Vec<norte_proto::methods::LogLine>, next: u64) {
        *self.log_remote.lock().expect("registro") = Some((lines, next));
    }

    /// This daemon has no log to serve: both methods answer `Unsupported`.
    /// It is the default state, written so the test that checks it SAYS so
    /// instead of relying on a `Default`.
    pub fn log_tail_no_supported(&self) {
        *self.log_remote.lock().expect("registro") = None;
        *self.log_next.lock().expect("next") = None;
    }

    /// The level the daemon says it has set, in `log.level` and in every
    /// `log.tail`. Both answer the same thing, like the real daemon: the
    /// level is ONE and global to the process.
    pub fn log_level_answers(&self, the_level: &str) {
        *self.level_remote.lock().expect("nivel") = Some(the_level.to_owned());
    }

    /// The levels that were requested from the daemon, in order.
    pub fn log_level_requests(&self) -> Vec<String> {
        self.levels_requests.lock().expect("niveles").clone()
    }

    /// The cursors `log.tail` was requested with, in order.
    pub fn cursors_requests(&self) -> Vec<Option<u64>> {
        self.log_cursors.lock().expect("cursores").clone()
    }

    /// The double just recorded something: let whoever was waiting look.
    ///
    /// Always goes AFTER the recording. Notifying before would wake a test
    /// that would see the old state again and go back to sleep, and that
    /// race is exactly what this mechanism exists to remove.
    pub fn heartbeat(&self) {
        self.pulse.notify_waiters();
    }

    /// What the TWO producers of an organize plan answer (phase 8).
    ///
    /// Just one, because the host treats their responses the same on
    /// purpose: a plugin plan and a model plan land in the same review, and
    /// two different doubles would let that equality break without any test
    /// noticing.
    fn organize_response(
        &self,
    ) -> BoxFuture<'static, Result<norte_proto::methods::AiOrganizePlanResult, Error>> {
        let plan = self.plan_organize.clone();
        let refuses = self.organize_refuses.clone();
        let hash = self.organize_hash.clone();
        Box::pin(async move {
            if let Some(why) = refuses {
                return Ok(norte_proto::methods::AiOrganizePlanResult {
                    moves: Vec::new(),
                    refused: Some(why),
                    plan_hash: None,
                });
            }
            let Some(pares) = plan else {
                return Err(Error::NotFound);
            };
            Ok(norte_proto::methods::AiOrganizePlanResult {
                moves: pares
                    .into_iter()
                    .map(
                        |(current, proposed_rel)| norte_proto::methods::OrganizeMove {
                            current,
                            proposed_rel,
                        },
                    )
                    .collect(),
                refused: None,
                plan_hash: hash,
            })
        })
    }

    /// Waits until the double has recorded what it is asked about. Without a
    /// clock.
    ///
    /// `that` looks at the double and returns `Some` once it is there: the
    /// value comes out cloned, because the `MutexGuard` cannot cross an
    /// `await`.
    ///
    /// The relief deadline is NOT a wait: it is the FAILURE budget. On the
    /// green path it does not consume a single millisecond — the notice
    /// arrives and the function returns — and when it runs out the test says
    /// WHAT it was waiting for instead of blowing up twenty lines further
    /// down in an assertion that explains nothing. Under load it does not
    /// get flaky either: fifteen seconds is three orders of magnitude more
    /// than a `spawn` takes to run.
    pub async fn until<T>(&self, that_expected: &str, that: impl Fn(&Self) -> Option<T>) -> T {
        const RELIEF: std::time::Duration = std::time::Duration::from_secs(15);
        let wait = async {
            loop {
                if let Some(v) = that(self) {
                    return v;
                }
                // The future is armed BEFORE the second check: arming it
                // after would lose a notification that landed right in
                // between.
                let notified = self.pulse.notified();
                if let Some(v) = that(self) {
                    return v;
                }
                notified.await;
            }
        };
        let Ok(v) = tokio::time::timeout(RELIEF, wait).await else {
            panic!("the double never recorded it: {that_expected}")
        };
        v
    }

    /// The shared body of copy and move in the fake: records what was
    /// requested and returns a Task with its OWN id.
    /// An archive task (pack or test) with its own id, so two gestures in a
    /// row do not step on each other's progress channel.
    fn archive_task(
        &self,
        kind: norte_proto::TaskKind,
        id: u64,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let progress = norte_proto::TaskProgress {
            task_id: norte_proto::TaskId::new(id),
            kind,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progress);
        *self.progress.lock().expect("progreso") = Some(tx);
        let cancellations = Arc::clone(&self.cancellations);
        Box::pin(async move {
            Ok(HostTask {
                id: norte_proto::TaskId::new(id),
                progress: rx,
                cancel: Arc::new(move || {
                    cancellations.fetch_add(1, Ordering::SeqCst);
                }),
                pause: None,
                cola: None,
                foreign: false,
            })
        })
    }

    fn transfer(
        &self,
        from: VPath,
        to: VPath,
        mover: bool,
        on_collision: norte_proto::CollisionPolicy,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        if let Some(e) = self.transfer_rejected.clone() {
            return Box::pin(async move { Err(e) });
        }
        self.transfers
            .lock()
            .expect("transferencias")
            .push((from, to, mover, on_collision));
        self.heartbeat();
        let n = self.next_task.fetch_add(1, Ordering::SeqCst);
        let id = norte_proto::TaskId::new(100 + n as u64);
        let progress = norte_proto::TaskProgress {
            task_id: id,
            kind: if mover {
                norte_proto::TaskKind::Move
            } else {
                norte_proto::TaskKind::Copy
            },
            state: self
                .state_transfer
                .clone()
                .unwrap_or(norte_proto::TaskState::Running),
            bytes_done: 0,
            bytes_total: Some(10),
            entries_done: 0,
            entries_total: Some(1),
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progress);
        *self.progress.lock().expect("progreso") = Some(tx.clone());
        self.progress_by_task
            .lock()
            .expect("progresos")
            .insert(id.get(), tx);
        let cancellations = Arc::clone(&self.cancellations);
        Box::pin(async move {
            Ok(HostTask {
                id,
                progress: rx,
                cancel: Arc::new(move || {
                    cancellations.fetch_add(1, Ordering::SeqCst);
                }),
                pause: None,
                cola: None,
                foreign: false,
            })
        })
    }

    pub fn listings(&self) -> usize {
        self.listings.load(Ordering::SeqCst)
    }

    /// How many reads have already RETURNED (listings, probes and contents).
    pub fn served(&self) -> usize {
        self.served.load(Ordering::SeqCst)
    }

    /// How many reads were REQUESTED.
    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    /// Is no read in flight? Everything requested has already returned.
    pub fn en_calma(&self) -> bool {
        self.served() >= self.requests()
    }

    /// A directory's entries, the way the listing would return them.
    pub fn entries_of(&self, dir: &VPath) -> Vec<Entry> {
        let mut out: Vec<Entry> = self
            .tree
            .get(&dir.to_wire())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|(name, es_dir)| {
                let path = dir.join(norte_proto::Segment::new(name).expect("segment"));
                let kind = self
                    .kinds
                    .get(&path.to_wire())
                    .copied()
                    .unwrap_or(if es_dir {
                        EntryKind::Dir
                    } else {
                        EntryKind::File
                    });
                Entry {
                    kind,
                    path,
                    // A directory has no size, like in real life: it is what
                    // makes the ABSENCE of a cell testable.
                    size: if es_dir || self.lazy { None } else { Some(1) },
                    mtime_ms: None,
                    attrs: std::collections::BTreeMap::new(),
                }
            })
            .collect();
        norte_frontend::sort_entries(&mut out);
        out
    }
}

/// The same path with its last segment in uppercase.
fn other_spelling(path: &VPath) -> VPath {
    let Some(name) = path.file_name() else {
        return path.clone();
    };
    let shouted: Vec<u8> = name.as_bytes().to_ascii_uppercase();
    let Some(padre) = path.parent() else {
        return path.clone();
    };
    match norte_proto::Segment::new(shouted) {
        Ok(seg) => padre.join(seg),
        Err(_) => path.clone(),
    }
}

/// The tree the parity scenarios use: a directory with two subdirectories
/// and a hostile name.
pub fn test_tree() -> Fake {
    let mut f = Fake::default();
    f.put(
        "mem:///casa",
        vec![
            (b"docs".to_vec(), true),
            (b"fotos".to_vec(), true),
            (b"notas.txt".to_vec(), false),
            (vec![0x63, 0x61, 0x66, 0xC3, 0x28], false),
            // A COMPRESSED file and a SYMLINK, the two entries on which
            // `Enter` means something other than "it's a file, nothing
            // happens". The parity harness could not touch the number-one
            // divergence in the inventory because this tree only had
            // directories and files; its own header said so and pointed out
            // that the double needed to know about kinds. Now it does.
            (b"cosas.zip".to_vec(), false),
            (b"atajo".to_vec(), false),
        ],
    );
    f.put(
        "mem:///casa/docs",
        vec![(b"a.md".to_vec(), false), (b"b.md".to_vec(), false)],
    );
    f.put("mem:///casa/fotos", vec![(b"gato.png".to_vec(), false)]);
    // The symlink points to a directory that DOES get listed: a symlink to a
    // file does not get resolved — `cd` fails and it is absorbed — and that
    // is a different case, not the one this tree has to be able to describe.
    f.put_kind("mem:///casa/atajo", EntryKind::Symlink);
    f.put("mem:///casa/atajo", vec![(b"dentro.md".to_vec(), false)]);
    // And the container's virtual root, which is where `Enter` composes to.
    f.put(
        "zip+mem:///casa/cosas.zip!/",
        vec![(b"leeme.txt".to_vec(), false)],
    );
    f
}

impl HostBackend for Fake {
    fn capabilities(
        &self,
        path: VPath,
    ) -> BoxFuture<'static, Result<norte_proto::Capabilities, Error>> {
        if self.capabilities_error {
            return Box::pin(async move { Err(Error::ProviderUnavailable { retryable: true }) });
        }
        // By LOCATION, not by provider: the exact directory is looked up
        // and, if absent, its parent — which is what a real mount does.
        let caps = self
            .capabilities
            .get(&path.to_wire())
            .or_else(|| {
                path.parent()
                    .and_then(|p| self.capabilities.get(&p.to_wire()))
            })
            .copied()
            // With no knob set: what a plain ext4 says — it distinguishes
            // case — which is the honest floor for a double running on
            // Linux.
            .unwrap_or(norte_proto::Capabilities {
                flags: norte_proto::CapabilityFlags::CASE_SENSITIVE,
                max_path: None,
            });
        Box::pin(async move { Ok(caps) })
    }

    fn plugin_list(
        &self,
    ) -> BoxFuture<'static, Result<norte_proto::methods::PluginListResult, Error>> {
        self.catalogos_requests
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.heartbeat();
        let plugins = self.plugins.lock().expect("plugins").clone();
        let errors = self
            .load_errors
            .iter()
            .map(|(dir, reason)| norte_proto::methods::PluginLoadError {
                dir: dir.clone(),
                reason: reason.clone(),
                dir_bytes: self.payload_bytes.get(dir).cloned(),
            })
            .collect();
        Box::pin(async move { Ok(norte_proto::methods::PluginListResult { plugins, errors }) })
    }

    fn plugin_help(
        &self,
        id: String,
    ) -> BoxFuture<'static, Result<norte_proto::methods::PluginHelpResult, Error>> {
        self.pages_requested
            .lock()
            .expect("pages mutex")
            .push(id.clone());
        self.heartbeat();
        let markdown = self.pages.get(&id).cloned().unwrap_or_default();
        Box::pin(async move {
            Ok(norte_proto::methods::PluginHelpResult {
                markdown,
                truncated: false,
                lossy: false,
            })
        })
    }

    fn search(
        &self,
        params: norte_proto::methods::FsSearchParams,
    ) -> BoxFuture<
        'static,
        Result<
            (
                norte_ui_host::backend::HostTask,
                tokio::sync::mpsc::Receiver<norte_proto::methods::SearchHits>,
            ),
            Error,
        >,
    > {
        let patron = params.name_glob.clone().unwrap_or_default();
        self.searches
            .lock()
            .expect("searches mutex")
            .push(patron.clone());
        self.params_search
            .lock()
            .expect("params mutex")
            .push(params.clone());
        self.heartbeat();
        if let Some(e) = self.search_error.clone() {
            return Box::pin(async move { Err(e) });
        }
        let findings = self.findings.get(&patron).cloned().unwrap_or_default();
        let outcome = self
            .search_outcome
            .clone()
            .unwrap_or(norte_proto::TaskState::Completed);
        let cancellations = Arc::clone(&self.cancellations);
        Box::pin(async move {
            let id = norte_proto::TaskId::new(77);
            let (tx, rx) = tokio::sync::mpsc::channel(8);
            let (ptx, prx) = tokio::sync::watch::channel(norte_proto::TaskProgress {
                task_id: id,
                kind: norte_proto::TaskKind::Search,
                state: norte_proto::TaskState::Running,
                bytes_done: 0,
                bytes_total: None,
                entries_done: 0,
                entries_total: None,
                current: None,
                unreadable: None,
                unvisited: None,
            });
            tokio::spawn(async move {
                let entries: Vec<norte_proto::Entry> = findings
                    .into_iter()
                    .map(|path| norte_proto::Entry {
                        path,
                        kind: norte_proto::EntryKind::File,
                        size: Some(1),
                        mtime_ms: Some(0),
                        attrs: std::collections::BTreeMap::new(),
                    })
                    .collect();
                // An EMPTY batch is not sent: `norte-core` cuts it off
                // before (`if batch.is_empty() { return
                // FlushOutcome::Continue }`), and a double that did send it
                // would hide everything that depends on the first batch
                // arriving. This is the divergence that covered up a search
                // with no hits never getting cancelled.
                if !entries.is_empty() {
                    let _ = tx
                        .send(norte_proto::methods::SearchHits {
                            task_id: id,
                            entries,
                            matches: None,
                        })
                        .await;
                }
                // And it ends: the view stops saying "searching…". WITH its
                // outcome, which is not cosmetic — "finished", "was
                // stopped" and "broke" say three different things about the
                // disk.
                let _ = ptx.send(norte_proto::TaskProgress {
                    task_id: id,
                    kind: norte_proto::TaskKind::Search,
                    state: outcome,
                    bytes_done: 0,
                    bytes_total: None,
                    entries_done: 1,
                    entries_total: Some(1),
                    current: None,
                    unreadable: None,
                    unvisited: None,
                });
                // The sender lives as long as the task: dropping it closes
                // the channel and that IS the end of the search.
                std::mem::forget(ptx);
            });
            Ok((
                norte_ui_host::backend::HostTask {
                    id,
                    progress: prx,
                    cancel: Arc::new(move || {
                        cancellations.fetch_add(1, Ordering::SeqCst);
                    }),
                    pause: None,
                    cola: None,
                    foreign: false,
                },
                rx,
            ))
        })
    }

    fn plugin_preview_styled(
        &self,
        path: VPath,
        columns: Option<u32>,
    ) -> BoxFuture<'static, Result<Option<norte_proto::methods::PluginPreviewStyled>, Error>> {
        self.preview_widths
            .lock()
            .expect("widths mutex")
            .push(columns);
        let p = self.previews.get(&path.to_wire()).cloned();
        Box::pin(async move { Ok(p) })
    }

    fn plugin_thumbnail(
        &self,
        path: VPath,
        _max_edge: u32,
    ) -> BoxFuture<'static, Result<Option<norte_proto::methods::PluginThumbnail>, Error>> {
        let t = self.thumbnails.get(&path.to_wire()).cloned();
        Box::pin(async move { Ok(t) })
    }

    fn plugin_decorate(
        &self,
        paths: Vec<VPath>,
        kinds: Vec<norte_proto::EntryKind>,
    ) -> BoxFuture<'static, Result<Vec<norte_proto::methods::PluginDecorations>, Error>> {
        self.decorated
            .lock()
            .expect("decorated mutex")
            .push(paths.clone());
        self.classes_decorated
            .lock()
            .expect("classes mutex")
            .push(kinds);
        self.heartbeat();
        let table = self.decorations.clone();
        let icons = self.icons.clone();
        // If the catalogue knows `acme.git` and it is DISABLED, it does not
        // decorate: that is what the real daemon does, and what lets you
        // check that disabling a plugin from the manager removes its badges
        // from the rows. A catalogue that does not name it decorates as
        // always.
        let git_off = self
            .plugins
            .lock()
            .expect("plugins")
            .iter()
            .any(|p| p.id == "acme.git" && !p.enabled);
        Box::pin(async move {
            let mut out = Vec::new();
            // With no consented decorators: "none", which is what the real
            // daemon answers. NOT a list of empties.
            if !table.is_empty() && !git_off {
                out.push(norte_proto::methods::PluginDecorations {
                    plugin_id: "acme.git".to_owned(),
                    slot: norte_proto::methods::DecorationSlot::Badge,
                    decorations: paths
                        .iter()
                        .map(|p| {
                            let d = table.get(&p.to_wire()).cloned();
                            norte_proto::methods::DecorationWire {
                                badge: d.clone(),
                                role: d.map(|_| "warning".to_owned()),
                            }
                        })
                        .collect(),
                });
            }
            if !icons.is_empty() {
                out.push(norte_proto::methods::PluginDecorations {
                    plugin_id: "acme.icons".to_owned(),
                    slot: norte_proto::methods::DecorationSlot::Icon,
                    decorations: paths
                        .iter()
                        .map(|p| norte_proto::methods::DecorationWire {
                            badge: icons.get(&p.to_wire()).cloned(),
                            role: None,
                        })
                        .collect(),
                });
            }
            Ok(out)
        })
    }

    fn plugin_column_values(
        &self,
        plugin: String,
        column: String,
        paths: Vec<VPath>,
    ) -> BoxFuture<'static, Result<Vec<Option<String>>, Error>> {
        self.columns_requested.lock().expect("columns mutex").push((
            plugin,
            column.clone(),
            paths.clone(),
        ));
        self.heartbeat();
        let table = self.column_values.clone();
        Box::pin(async move {
            // Positional 1:1 with `paths`, ALWAYS: that is the contract, and
            // a short vector is how to break it without it showing.
            Ok(paths
                .iter()
                .map(|p| table.get(&(column.clone(), p.to_wire())).cloned())
                .collect())
        })
    }

    fn plugin_panel_render(
        &self,
        params: norte_proto::methods::PluginPanelRenderParams,
    ) -> BoxFuture<'static, Result<Option<norte_proto::methods::PanelFrame>, Error>> {
        self.panels_requests
            .lock()
            .expect("panels mutex")
            .push(params);
        self.heartbeat();
        let marco = self.marco_de_panel.clone();
        Box::pin(async move { Ok(marco) })
    }

    fn volumes(&self) -> BoxFuture<'static, Result<Vec<norte_proto::methods::Volume>, Error>> {
        self.volumes_requests
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let vols = self.volumes.clone();
        Box::pin(async move { Ok(vols) })
    }

    fn plugin_config(
        &self,
        id: String,
    ) -> BoxFuture<'static, Result<norte_proto::methods::PluginGetConfigResult, Error>> {
        self.cards_requested
            .lock()
            .expect("cards mutex")
            .push(id.clone());
        self.heartbeat();
        let keys = self.schemes.get(&id).cloned().unwrap_or_default();
        Box::pin(async move { Ok(norte_proto::methods::PluginGetConfigResult { keys }) })
    }

    fn undo_session(&self, session: String) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.undone.lock().expect("deshechas").push(session);
        self.heartbeat();
        let n = self.next_task.fetch_add(1, Ordering::SeqCst);
        let id = norte_proto::TaskId::new(900 + n as u64);
        let progress = norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::Undo,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progress);
        self.progress_by_task
            .lock()
            .expect("progresos")
            .insert(id.get(), tx);
        Box::pin(async move {
            Ok(HostTask {
                id,
                progress: rx,
                cancel: Arc::new(|| {}),
                pause: None,
                cola: None,
                foreign: false,
            })
        })
    }

    fn journal_list(
        &self,
        before_seq: Option<i64>,
        limit: u32,
    ) -> BoxFuture<'static, Result<norte_proto::methods::JournalListResult, Error>> {
        self.heartbeat();
        let res = self
            .journal
            .as_ref()
            .map_or(Err(Error::Unsupported), |the_rows| {
                let cap = usize::try_from(limit).unwrap_or(usize::MAX);
                let remain: Vec<_> = the_rows
                    .iter()
                    .filter(|f| before_seq.is_none_or(|b| f.seq < b))
                    .cloned()
                    .collect();
                let rows: Vec<_> = remain.iter().take(cap).cloned().collect();
                let next_before_seq = (remain.len() > cap)
                    .then(|| rows.last().map(|f| f.seq))
                    .flatten();
                Ok(norte_proto::methods::JournalListResult {
                    rows,
                    next_before_seq,
                })
            });
        Box::pin(async move { res })
    }

    fn undo_after(
        &self,
        seq: i64,
        upto_seq: Option<i64>,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.undone_until
            .lock()
            .expect("deshechos_hasta")
            .push((seq, upto_seq));
        self.heartbeat();
        let n = self.next_task.fetch_add(1, Ordering::SeqCst);
        let id = norte_proto::TaskId::new(950 + n as u64);
        let progress = norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::Undo,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progress);
        self.progress_by_task
            .lock()
            .expect("progresos")
            .insert(id.get(), tx);
        Box::pin(async move {
            Ok(HostTask {
                id,
                progress: rx,
                cancel: Arc::new(|| {}),
                pause: None,
                cola: None,
                foreign: false,
            })
        })
    }

    fn plugin_set_approval(
        &self,
        id: String,
        approved: bool,
        expected_digest: Option<String>,
    ) -> BoxFuture<'static, Result<(), Error>> {
        // The anchor is RECORDED (#282): that the window sends it is what a
        // test can assert from here, and without recording it the whole
        // thread would be a chain of signatures nobody reads.
        self.governance.lock().expect("gobierno").push(format!(
            "approval:{id}:{approved}:{}",
            expected_digest.as_deref().unwrap_or("-")
        ));
        self.heartbeat();
        let failure = self.governance_error.lock().expect("gobierno").clone();
        // And the catalogue changes: the host RE-REQUESTS it after an OK, so
        // a fake that always answered the same thing would let a screen say
        // "approved" through without anyone confirming it.
        if failure.is_none() {
            for p in self.plugins.lock().expect("plugins").iter_mut() {
                if p.id == id {
                    p.approved = approved;
                }
            }
        }
        Box::pin(async move { failure.map_or(Ok(()), Err) })
    }

    fn plugin_set_enabled(
        &self,
        id: String,
        enabled: bool,
    ) -> BoxFuture<'static, Result<(), Error>> {
        self.governance
            .lock()
            .expect("gobierno")
            .push(format!("enabled:{id}:{enabled}"));
        self.heartbeat();
        let failure = self.governance_error.lock().expect("gobierno").clone();
        if failure.is_none() {
            for p in self.plugins.lock().expect("plugins").iter_mut() {
                if p.id == id {
                    p.enabled = enabled;
                }
            }
        }
        Box::pin(async move { failure.map_or(Ok(()), Err) })
    }

    fn plugin_uninstall(&self, id: String) -> BoxFuture<'static, Result<bool, Error>> {
        self.governance
            .lock()
            .expect("gobierno")
            .push(format!("uninstall:{id}"));
        self.heartbeat();
        let failure = self.governance_error.lock().expect("gobierno").clone();
        let mut had = false;
        if failure.is_none() {
            // And it disappears from the catalogue: the host RE-REQUESTS it
            // after an OK, and a row that stayed there would be the screen
            // showing what was deleted.
            let mut plugins = self.plugins.lock().expect("plugins");
            had = plugins.iter().any(|p| p.id == id && p.approved);
            plugins.retain(|p| p.id != id);
        }
        Box::pin(async move { failure.map_or(Ok(had), Err) })
    }

    fn plugin_set_config(
        &self,
        id: String,
        key: String,
        value: String,
    ) -> BoxFuture<'static, Result<(), Error>> {
        self.writes
            .lock()
            .expect("escrituras")
            .push((id, key, value));
        self.heartbeat();
        let failure = self.error_on_write.lock().expect("escribir").clone();
        Box::pin(async move { failure.map_or(Ok(()), Err) })
    }

    fn plugin_run_command(
        &self,
        id: String,
        command: String,
        _arg: String,
    ) -> BoxFuture<'static, Result<String, Error>> {
        self.executed
            .lock()
            .expect("ejecutados")
            .push((id, command));
        self.heartbeat();
        let output = self.command_output.lock().expect("salida").clone();
        Box::pin(async move { output.unwrap_or_else(|| Ok(String::new())) })
    }

    fn read(
        &self,
        path: VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> BoxFuture<'static, Result<Vec<u8>, Error>> {
        let bytes = self.content.get(&path.to_wire()).cloned();
        let delay = self.delay_ms;
        self.requests.fetch_add(1, Ordering::SeqCst);
        let served = Arc::clone(&self.served);
        let pulse = Arc::clone(&self.pulse);
        Box::pin(async move {
            if delay > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            served.fetch_add(1, Ordering::SeqCst);
            pulse.notify_waiters();
            let mut b = bytes.ok_or(Error::NotFound)?;
            if let Some(r) = range {
                let off = usize::try_from(r.offset).unwrap_or(usize::MAX).min(b.len());
                b = b.split_off(off);
                if let Some(len) = r.len {
                    b.truncate(usize::try_from(len).unwrap_or(usize::MAX));
                }
            }
            Ok(b)
        })
    }

    fn stat(&self, path: VPath, _attrs: Vec<String>) -> BoxFuture<'static, Result<Entry, Error>> {
        self.probes.lock().expect("sondeos").push(path.clone());
        self.requests.fetch_add(1, Ordering::SeqCst);
        self.heartbeat();
        let grita = self.stat_grita;
        let delay = self.delay_ms;
        // The path's parent says which directory to look it up in; the entry
        // comes from the same tree, but NOW with a size: that is what `stat`
        // does.
        let entry = path.parent().and_then(|dir| {
            self.tree.get(&dir.to_wire()).and_then(|entries| {
                entries
                    .iter()
                    .find(|(n, _)| path.file_name().is_some_and(|f| f.as_bytes() == n))
                    .map(|(_, es_dir)| Entry {
                        // A provider can answer with ANOTHER spelling of the
                        // same name; the host has to hydrate the entry it
                        // asked for, not the one it gets back.
                        path: if grita {
                            other_spelling(&path)
                        } else {
                            path.clone()
                        },
                        kind: if *es_dir {
                            EntryKind::Dir
                        } else {
                            EntryKind::File
                        },
                        size: if *es_dir { None } else { Some(1) },
                        mtime_ms: Some(1_700_000_000_000),
                        attrs: std::collections::BTreeMap::new(),
                    })
            })
        });
        // What this fake CREATED exists, even if it is not in the `put`
        // tree: the tree is the listing from before anything was created
        // (#303).
        let entry = entry.or_else(|| {
            let creado = self
                .created
                .lock()
                .expect("creados")
                .iter()
                .any(|c| c.to_wire() == path.to_wire());
            creado.then(|| Entry {
                path: path.clone(),
                kind: self.created_appears_as.unwrap_or(EntryKind::File),
                size: Some(0),
                mtime_ms: Some(1_700_000_000_000),
                attrs: std::collections::BTreeMap::new(),
            })
        });
        let served = Arc::clone(&self.served);
        let pulse = Arc::clone(&self.pulse);
        Box::pin(async move {
            if delay > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            served.fetch_add(1, Ordering::SeqCst);
            pulse.notify_waiters();
            entry.ok_or(Error::NotFound)
        })
    }

    fn attr_catalog(
        &self,
        _dir: VPath,
    ) -> BoxFuture<'static, Result<norte_proto::AttrCatalog, Error>> {
        let c = self.catalog.lock().expect("catalog").clone();
        Box::pin(async move { Ok(c) })
    }

    fn take_approvals(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::PolicyApprovalRequired>>
    {
        self.approvals.lock().expect("aprobaciones").take()
    }

    fn policy_decide(
        &self,
        approval_id: u64,
        approve: bool,
    ) -> BoxFuture<'static, Result<(), Error>> {
        self.decisiones
            .lock()
            .expect("decisiones")
            .push((approval_id, approve));
        self.heartbeat();
        let failure = self.error_on_decide.lock().expect("decide error").clone();
        Box::pin(async move { failure.map_or(Ok(()), Err) })
    }

    fn take_conn_events(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<norte_client::ConnEvent>> {
        self.eventos.lock().expect("eventos").take()
    }

    fn take_foreign_tasks(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<HostTask>> {
        self.foreign.lock().expect("ajenas").take()
    }

    fn sync_apply(
        &self,
        plan_hash: norte_proto::methods::PlanHash,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.applied.lock().expect("aplicados").push(plan_hash);
        self.heartbeat();
        if let Some(e) = self.error_on_apply.lock().expect("apply error").clone() {
            return Box::pin(async move { Err(e) });
        }
        let n = self.next_task.fetch_add(1, Ordering::SeqCst);
        let id = norte_proto::TaskId::new(500 + n as u64);
        let progress = norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::Sync,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progress);
        *self.progress.lock().expect("progreso") = Some(tx.clone());
        self.progress_by_task
            .lock()
            .expect("progresos")
            .insert(id.get(), tx);
        // REALLY cancellable: with a canceller that does not count, a
        // cancellation test would pass just the same with the panel frozen.
        let canceled = Arc::clone(&self.canceled_by_id);
        let pulse = Arc::clone(&self.pulse);
        Box::pin(async move {
            Ok(HostTask {
                id,
                progress: rx,
                cancel: Arc::new(move || {
                    canceled.lock().expect("canceladas").push(id.get());
                    pulse.notify_waiters();
                }),
                pause: None,
                cola: None,
                foreign: false,
            })
        })
    }

    fn sync_report(
        &self,
        _task_id: norte_proto::TaskId,
    ) -> BoxFuture<'static, Result<norte_proto::methods::SyncReportResult, Error>> {
        let report = self.sync_report.lock().expect("informe").clone();
        Box::pin(async move { report.ok_or(Error::Unsupported) })
    }

    fn sync_plan(
        &self,
        params: norte_proto::methods::SyncPlanParams,
    ) -> BoxFuture<
        'static,
        Result<
            (
                HostTask,
                tokio::sync::mpsc::Receiver<norte_client::SyncPlanEvent>,
            ),
            Error,
        >,
    > {
        self.planes_requests.lock().expect("planes").push((
            params.source,
            params.dest,
            params.mode,
        ));
        self.heartbeat();
        let plan = self.plan_de_sync.lock().expect("plan").clone();
        let n = self.next_task.fetch_add(1, Ordering::SeqCst);
        let id = norte_proto::TaskId::new(400 + n as u64);
        let progress = norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::SyncPlan,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progress);
        *self.progress.lock().expect("progreso") = Some(tx.clone());
        self.progress_by_task
            .lock()
            .expect("progresos")
            .insert(id.get(), tx);
        Box::pin(async move {
            let (steps, mut done) = plan.ok_or(Error::Unsupported)?;
            // The closing carries ITS OWN Task: the shared model discards
            // another plan's for this id, which is exactly what it must do.
            done.task_id = id;
            let (etx, erx) = tokio::sync::mpsc::channel(4);
            tokio::spawn(async move {
                let _ = etx
                    .send(norte_client::SyncPlanEvent::Steps(
                        norte_proto::methods::SyncStepsBatch { task_id: id, steps },
                    ))
                    .await;
                let _ = etx.send(norte_client::SyncPlanEvent::Done(done)).await;
            });
            Ok((
                HostTask {
                    id,
                    progress: rx,
                    cancel: Arc::new(|| {}),
                    pause: None,
                    cola: None,
                    foreign: false,
                },
                erx,
            ))
        })
    }

    fn compare(
        &self,
        params: norte_proto::methods::FsCompareParams,
    ) -> BoxFuture<
        'static,
        Result<
            (
                HostTask,
                tokio::sync::mpsc::Receiver<norte_proto::methods::CompareRowsBatch>,
            ),
            Error,
        >,
    > {
        self.comparisons
            .lock()
            .expect("comparaciones")
            .push((params.left, params.right));
        self.heartbeat();
        let the_rows = self.rows_compared.lock().expect("filas").clone();
        let n = self.next_task.fetch_add(1, Ordering::SeqCst);
        let id = norte_proto::TaskId::new(300 + n as u64);
        let progress = norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::Compare,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progress);
        *self.progress.lock().expect("progreso") = Some(tx.clone());
        self.progress_by_task
            .lock()
            .expect("progresos")
            .insert(id.get(), tx);
        Box::pin(async move {
            let the_rows = the_rows.ok_or(Error::Unsupported)?;
            let (ftx, frx) = tokio::sync::mpsc::channel(4);
            tokio::spawn(async move {
                let _ = ftx
                    .send(norte_proto::methods::CompareRowsBatch {
                        task_id: id,
                        rows: the_rows,
                    })
                    .await;
            });
            Ok((
                HostTask {
                    id,
                    progress: rx,
                    cancel: Arc::new(|| {}),
                    pause: None,
                    cola: None,
                    foreign: false,
                },
                frx,
            ))
        })
    }

    fn semantic_search(
        &self,
        query: String,
        k: u32,
    ) -> BoxFuture<'static, Result<Vec<norte_proto::methods::SemanticHit>, Error>> {
        self.semanticas_requested
            .lock()
            .expect("semantics")
            .push((query, k));
        self.heartbeat();
        let hits = self.semantic.lock().expect("semantics").clone();
        Box::pin(async move { hits.ok_or(Error::NotFound) })
    }

    fn take_degraded(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::ConnectionDegraded>>
    {
        self.degraded.lock().expect("degradadas").take()
    }

    fn take_failed(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::ConnectionFailed>> {
        self.failed.lock().expect("fallidas").take()
    }

    fn take_plugin_notices(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::PluginNotice>> {
        self.notices_plugin.lock().expect("avisos_plugin").take()
    }

    /// #311: records the checksum batch and returns an already-finished
    /// Task. The report is served by `checksum_report` with whatever
    /// `checksums_report` says.
    fn checksum(
        &self,
        params: norte_proto::methods::FsChecksumParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.checksums_requested
            .lock()
            .expect("sumas")
            .push(params.paths.clone());
        self.heartbeat();
        let progress = norte_proto::TaskProgress {
            task_id: norte_proto::TaskId::new(10),
            kind: norte_proto::TaskKind::Checksum,
            state: norte_proto::TaskState::Completed,
            bytes_done: 0,
            bytes_total: None,
            entries_done: params.paths.len() as u64,
            entries_total: Some(params.paths.len() as u64),
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (_tx, rx) = tokio::sync::watch::channel(progress);
        Box::pin(async move {
            Ok(HostTask {
                id: norte_proto::TaskId::new(10),
                progress: rx,
                cancel: Arc::new(|| {}),
                pause: None,
                cola: None,
                foreign: false,
            })
        })
    }

    fn dir_usage(
        &self,
        params: norte_proto::methods::FsDirUsageParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.maps_requests
            .lock()
            .expect("mapas")
            .push(params.path.clone());
        self.heartbeat();
        // Already finished: the host asks for the report as soon as the Task
        // is terminal, so a double that left it running would never manage
        // to land anything and the test would be measuring silence.
        let progress = norte_proto::TaskProgress {
            task_id: norte_proto::TaskId::new(11),
            kind: norte_proto::TaskKind::DirUsage,
            state: norte_proto::TaskState::Completed,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: Some(params.path),
            unreadable: None,
            unvisited: None,
        };
        let (_tx, rx) = tokio::sync::watch::channel(progress);
        Box::pin(async move {
            Ok(HostTask {
                id: norte_proto::TaskId::new(11),
                progress: rx,
                cancel: Arc::new(|| {}),
                pause: None,
                cola: None,
                foreign: false,
            })
        })
    }

    fn dir_usage_report(
        &self,
        task: norte_proto::TaskId,
    ) -> BoxFuture<'static, Result<norte_proto::methods::FsDirUsageReportResult, Error>> {
        let _ = task;
        self.heartbeat();
        // An empty map but LISTED: the slot paints with no rectangles and
        // without saying it is measuring, which is what an empty directory
        // really produces.
        Box::pin(async move {
            Ok(norte_proto::methods::FsDirUsageReportResult {
                listed: true,
                ..norte_proto::methods::FsDirUsageReportResult::default()
            })
        })
    }

    fn checksum_report(
        &self,
        task: norte_proto::TaskId,
    ) -> BoxFuture<'static, Result<norte_proto::methods::FsChecksumReportResult, Error>> {
        self.checksums_informes_requests
            .lock()
            .expect("checksum reports")
            .push(task.get());
        self.heartbeat();
        let report = self.checksums_report.lock().expect("informe").clone();
        Box::pin(async move { Ok(report) })
    }

    /// #314: records the permission batch that was requested, so a test can
    /// assert WHICH paths and with WHICH mode — which is the only thing the
    /// host decides; the core decides the rest.
    fn set_mode(
        &self,
        params: norte_proto::methods::FsSetModeParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.permissions
            .lock()
            .expect("permisos")
            .push((params.paths.clone(), params.mode));
        self.heartbeat();
        let progress = norte_proto::TaskProgress {
            task_id: norte_proto::TaskId::new(9),
            kind: norte_proto::TaskKind::SetMode,
            state: norte_proto::TaskState::Completed,
            bytes_done: 0,
            bytes_total: None,
            entries_done: params.paths.len() as u64,
            entries_total: Some(params.paths.len() as u64),
            current: None,
            unreadable: Some(0),
            unvisited: None,
        };
        let (_tx, rx) = tokio::sync::watch::channel(progress);
        Box::pin(async move {
            Ok(HostTask {
                id: norte_proto::TaskId::new(9),
                progress: rx,
                cancel: Arc::new(|| {}),
                pause: None,
                cola: None,
                foreign: false,
            })
        })
    }

    fn mkdir(&self, path: VPath) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.created.lock().expect("creados").push(path);
        self.heartbeat();
        let progress = norte_proto::TaskProgress {
            task_id: norte_proto::TaskId::new(8),
            kind: norte_proto::TaskKind::Mkdir,
            state: norte_proto::TaskState::Completed,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 1,
            entries_total: Some(1),
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (_tx, rx) = tokio::sync::watch::channel(progress);
        Box::pin(async move {
            Ok(HostTask {
                id: norte_proto::TaskId::new(8),
                progress: rx,
                cancel: Arc::new(|| {}),
                pause: None,
                cola: None,
                foreign: false,
            })
        })
    }

    fn create_file(&self, path: VPath) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.created.lock().expect("creados").push(path);
        self.heartbeat();
        // With its OWN id: the "edit a new one" gesture watches ITS task's
        // outcome to open the file, and sharing the 8 with `mkdir` would make
        // a create-directory test trigger that opening.
        //
        // And it is born RUNNING, with its own terminal behind it. The
        // fake's other tasks are born already finished with the sender
        // dropped, and that is not what a real backend does: the host pumps
        // the channel's CHANGES, so a dead channel never delivers it an
        // outcome. What is needed here is exactly the outcome.
        let vivo = norte_proto::TaskProgress {
            task_id: norte_proto::TaskId::new(14),
            kind: norte_proto::TaskKind::Create,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: Some(1),
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(vivo.clone());
        // The sender stays alive as long as the double lives. Dropping it
        // after sending closes the channel before the host has read the
        // change, and that is exactly the race this fake exists to avoid;
        // it used to be bought by sleeping fifty milliseconds, which is a
        // bet on when the host pumps.
        *self.progress_create.lock().expect("create progress") = Some(tx.clone());
        tokio::spawn(async move {
            let _ = tx.send(norte_proto::TaskProgress {
                state: norte_proto::TaskState::Completed,
                entries_done: 1,
                ..vivo
            });
        });
        Box::pin(async move {
            Ok(HostTask {
                id: norte_proto::TaskId::new(14),
                progress: rx,
                cancel: Arc::new(|| {}),
                pause: None,
                cola: None,
                foreign: false,
            })
        })
    }

    fn list(
        &self,
        dir: VPath,
        attrs: Vec<String>,
    ) -> BoxFuture<'static, Result<(norte_client::EntryStream, Option<u64>), Error>> {
        self.attrs_requests.lock().expect("attrs").push(attrs);
        self.listings.fetch_add(1, Ordering::SeqCst);
        self.heartbeat();
        // #327: the connection asks for its password. It is consumed ONCE —
        // whoever hands it over lists again and this time has to get in —
        // which is exactly the flow that must be testable.
        if let Some(failure) = self
            .asks_secret
            .lock()
            .expect("pide_secreto")
            .take()
            .filter(|_| self.tree.contains_key(&dir.to_wire()))
        {
            return Box::pin(async move { Err(failure) });
        }
        if !self.tree.contains_key(&dir.to_wire()) {
            return Box::pin(async { Err(Error::NotFound) });
        }
        // Counted AFTER the `NotFound`: what does not fly is not waited for.
        self.requests.fetch_add(1, Ordering::SeqCst);
        let lazy = self.lazy;
        // The directory the provider hangs its entries under. With
        // `padre_different`, ANOTHER spelling of the same place.
        let padre = if self.padre_different {
            match dir.file_name() {
                Some(seg) => dir.parent().unwrap_or_else(|| dir.clone()).join(
                    norte_proto::Segment::new(seg.as_bytes().to_ascii_uppercase())
                        .expect("segmento"),
                ),
                None => dir.clone(),
            }
        } else {
            dir.clone()
        };
        let idos = self.disappeared.lock().expect("desaparecidos").clone();
        // Unsorted: sorting is `PaneState`'s job, and returning it already
        // sorted would hide that the host delegates it.
        let entries: Vec<Entry> = self
            .tree
            .get(&dir.to_wire())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|(name, es_dir)| {
                let path = padre.join(norte_proto::Segment::new(name).expect("segmento"));
                // The exact kind, if someone set it (`put_kind`). This double
                // builds entries in TWO places — here and in `entries_of` —
                // and the override has to be in both: patching only one
                // leaves the test looking at a listing the host never sees.
                let kind = self
                    .kinds
                    .get(&path.to_wire())
                    .copied()
                    .unwrap_or(if es_dir {
                        EntryKind::Dir
                    } else {
                        EntryKind::File
                    });
                Entry {
                    kind,
                    path,
                    // A directory has no size, like in real life: it is what
                    // makes the ABSENCE of a cell testable. With `lazy`, a
                    // file does not have one either: it is the local
                    // provider's listing (#52), where the size gets probed
                    // separately.
                    size: if es_dir || lazy { None } else { Some(1) },
                    mtime_ms: None,
                    attrs: {
                        let mut m = std::collections::BTreeMap::new();
                        // 0o100644: what a real POSIX provider actually
                        // sends, and what would paint as "33188" without a
                        // catalogue.
                        m.insert("posix.mode".to_owned(), norte_proto::AttrValue::Uint(33188));
                        m
                    },
                }
            })
            .filter(|e| !idos.contains(&e.path.to_wire()))
            .collect();
        let delay = self.delay_ms;
        let skipped = self.skipped;
        let gate = self.gate_drain.clone();
        let served = Arc::clone(&self.served);
        let pulse = Arc::clone(&self.pulse);
        Box::pin(async move {
            if delay > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            served.fetch_add(1, Ordering::SeqCst);
            pulse.notify_waiters();
            // 100 = the host's `FIRST_PAGE`: entry 101 is the first of the
            // DRAIN, and that is where it cuts off.
            let stream: norte_client::EntryStream = Box::pin(futures::stream::unfold(
                (entries.into_iter().enumerate(), gate),
                |(mut it, gate)| async move {
                    let (i, e) = it.next()?;
                    if i == 100
                        && let Some(p) = &gate
                    {
                        p.wait().await;
                    }
                    Some((Ok(e), (it, gate)))
                },
            ));
            Ok((stream, skipped))
        })
    }

    fn session_get(
        &self,
    ) -> BoxFuture<'static, Result<(norte_proto::methods::Session, bool), Error>> {
        let (session, owner) = self.session.lock().expect("session").clone();
        // With no session set — revision 0, what `Default` gives — this
        // window owns it, like on a fresh install: the daemon answers
        // `owner: true` to the first connection even with nothing saved. A
        // test that wants a DETACHED window sets a session and says `false`.
        let owner = owner || session.revision == 0;
        Box::pin(async move { Ok((session, owner)) })
    }

    fn session_release(&self) -> BoxFuture<'static, Result<bool, Error>> {
        self.loose.fetch_add(1, Ordering::SeqCst);
        self.heartbeat();
        // What the real daemon answers: `true` if this connection owned it.
        // The double says so by flag, so both branches can be tested — and
        // the `false` one is the one that matters, because it is the one
        // that must NOT launch anything.
        let detached = self.drops_the_session;
        Box::pin(async move { Ok(detached) })
    }

    fn session_put(
        &self,
        _version: u32,
        _revision: u64,
        body: serde_json::Value,
    ) -> BoxFuture<'static, Result<u64, Error>> {
        if self.conflict {
            return Box::pin(async {
                Err(Error::Conflict {
                    conflict: norte_proto::ConflictKind::Exists,
                })
            });
        }
        // The core refuses the WHOLE body for size (#316). The knob counts
        // the rejections it has left, so a test can ask for "the first no,
        // the second yes", which is degrade-with-retry.
        {
            let mut remain = self.rejections_by_size.lock().expect("rechazos");
            if *remain > 0 {
                *remain -= 1;
                self.placed.lock().expect("puestas").push(body);
                self.heartbeat();
                return Box::pin(async {
                    Err(Error::LimitExceeded {
                        limit: Error::LIMIT_SESSION_BODY.to_owned(),
                    })
                });
            }
        }
        self.placed.lock().expect("puestas").push(body.clone());
        *self.written.lock().expect("escrito") = Some(body);
        self.heartbeat();
        Box::pin(async { Ok(9) })
    }

    fn copy(
        &self,
        from: VPath,
        to: VPath,
        on_collision: norte_proto::CollisionPolicy,
        queued: bool,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        *self.queued.lock().expect("encoladas") = queued;
        self.transfer(from, to, false, on_collision)
    }

    fn ai_rename_plan(
        &self,
        _dir: VPath,
        instruction: String,
        names: Vec<String>,
    ) -> BoxFuture<'static, Result<norte_proto::methods::AiRenamePlanResult, Error>> {
        self.instructions
            .lock()
            .expect("instrucciones")
            .push(instruction);
        self.heartbeat();
        // The names that travelled (#121): it is what lets you see that a
        // plan requested over five files does not send the directory's
        // thousand.
        self.names_ia.lock().expect("nombres_ia").push(names);
        self.heartbeat();
        let plan = self.plan_ia.clone();
        let delay = self.delay_ia_ms;
        let gate = self.gate_ia.clone();
        // Requesting a plan is a READ: the model mutates nothing. It counts
        // toward the same total as listings, which is what lets you wait for
        // "nothing in flight" without counting each case's responses by
        // hand.
        self.requests.fetch_add(1, Ordering::SeqCst);
        let served = Arc::clone(&self.served);
        let pulse = Arc::clone(&self.pulse);
        Box::pin(async move {
            // With the gate, the plan does not answer until the test opens
            // it: the model "keeps thinking" as a FACT, not as a
            // millisecond window a loaded machine can skip past.
            if let Some(gate) = gate {
                gate.wait().await;
            }
            if delay > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            served.fetch_add(1, Ordering::SeqCst);
            pulse.notify_waiters();
            let Some(pares) = plan else {
                return Err(Error::Unsupported);
            };
            Ok(norte_proto::methods::AiRenamePlanResult {
                entries: pares
                    .into_iter()
                    .map(|(from, to)| norte_proto::methods::AiRenameEntry { from, to })
                    .collect(),
                refused: None,
            })
        })
    }

    fn plugin_rename_plan(
        &self,
        plugin_id: String,
        renamer_id: String,
        _dir: VPath,
        names: Vec<String>,
    ) -> BoxFuture<'static, Result<norte_proto::methods::AiRenamePlanResult, Error>> {
        self.renamers_requests
            .lock()
            .expect("renamers")
            .push((plugin_id, renamer_id, names));
        self.heartbeat();
        let plan = self.plan_renamer.clone();
        let refuses = self.renamer_refuses.clone();
        Box::pin(async move {
            if let Some(why) = refuses {
                return Ok(norte_proto::methods::AiRenamePlanResult {
                    entries: Vec::new(),
                    refused: Some(why),
                });
            }
            let Some(pares) = plan else {
                return Err(Error::NotFound);
            };
            Ok(norte_proto::methods::AiRenamePlanResult {
                entries: pares
                    .into_iter()
                    .map(|(from, to)| norte_proto::methods::AiRenameEntry { from, to })
                    .collect(),
                refused: None,
            })
        })
    }

    fn ai_organize_plan(
        &self,
        _dir: VPath,
        _instruction: String,
        _names: Vec<String>,
    ) -> BoxFuture<'static, Result<norte_proto::methods::AiOrganizePlanResult, Error>> {
        self.heartbeat();
        self.organize_response()
    }

    fn plugin_organize_plan(
        &self,
        plugin_id: String,
        organizer_id: String,
        _dir: VPath,
        names: Vec<String>,
    ) -> BoxFuture<'static, Result<norte_proto::methods::AiOrganizePlanResult, Error>> {
        // The NAMES that travelled: a plugin lists nothing, so with an empty
        // list it answers that it moves nothing — and that is a caller
        // failure no test would see if this were not recorded.
        self.organizers_requests
            .lock()
            .expect("organizers")
            .push((plugin_id, organizer_id, names));
        self.heartbeat();
        self.organize_response()
    }

    fn organize(
        &self,
        dir: VPath,
        moves: Vec<norte_proto::methods::OrganizeMove>,
        plan_hash: norte_proto::methods::PlanHash,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.organized
            .lock()
            .expect("organizados")
            .push((dir, moves, plan_hash));
        self.heartbeat();
        let n = self.next_task.fetch_add(1, Ordering::SeqCst);
        let id = norte_proto::TaskId::new(300 + n as u64);
        let progress = norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::Move,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: Some(1),
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progress);
        *self.progress.lock().expect("progreso") = Some(tx.clone());
        self.progress_by_task
            .lock()
            .expect("progresos")
            .insert(id.get(), tx);
        Box::pin(async move {
            Ok(HostTask {
                id,
                progress: rx,
                cancel: Arc::new(|| {}),
                pause: None,
                cola: None,
                foreign: false,
            })
        })
    }

    fn rename_batch_plan(
        &self,
        _dir: VPath,
        pairs: Vec<norte_proto::methods::RenamePair>,
    ) -> BoxFuture<'static, Result<norte_proto::methods::FsRenameBatchPlanResult, Error>> {
        self.verdicts_requests
            .lock()
            .expect("veredictos")
            .push(pairs);
        self.heartbeat();
        let v = self.verdict.clone();
        Box::pin(async move { v.ok_or(Error::Unsupported) })
    }

    fn rename_batch(
        &self,
        dir: VPath,
        pairs: Vec<norte_proto::methods::RenamePair>,
        plan_hash: norte_proto::methods::PlanHash,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.batches
            .lock()
            .expect("lotes")
            .push((dir, pairs, plan_hash));
        self.heartbeat();
        let n = self.next_task.fetch_add(1, Ordering::SeqCst);
        let id = norte_proto::TaskId::new(200 + n as u64);
        let progress = norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::RenameBatch,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: Some(1),
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progress);
        *self.progress.lock().expect("progreso") = Some(tx.clone());
        self.progress_by_task
            .lock()
            .expect("progresos")
            .insert(id.get(), tx);
        Box::pin(async move {
            Ok(HostTask {
                id,
                progress: rx,
                cancel: Arc::new(|| {}),
                pause: None,
                cola: None,
                foreign: false,
            })
        })
    }

    fn rename_batch_report(
        &self,
        task_id: norte_proto::TaskId,
    ) -> BoxFuture<'static, Result<norte_proto::methods::FsRenameBatchReportResult, Error>> {
        self.informes_requests
            .lock()
            .expect("informes")
            .push(task_id.get());
        self.heartbeat();
        let report = self.report.lock().expect("informe").clone();
        Box::pin(async move { report.ok_or(Error::Unsupported) })
    }

    fn undo_report(
        &self,
        task_id: norte_proto::TaskId,
    ) -> BoxFuture<'static, Result<norte_proto::methods::PolicyUndoReportResult, Error>> {
        self.informes_undo_requests
            .lock()
            .expect("undo reports")
            .push(task_id.get());
        self.heartbeat();
        let report = self.report_undo.lock().expect("undo report").clone();
        Box::pin(async move { report.ok_or(Error::Unsupported) })
    }

    fn archive_pack_report(
        &self,
        task_id: norte_proto::TaskId,
    ) -> BoxFuture<'static, Result<norte_proto::methods::ArchivePackReportResult, Error>> {
        self.informes_pack_requests
            .lock()
            .expect("pack reports")
            .push(task_id.get());
        self.heartbeat();
        let report = self.report_pack.lock().expect("pack report").clone();
        Box::pin(async move { report.ok_or(Error::Unsupported) })
    }

    fn move_(
        &self,
        from: VPath,
        to: VPath,
        on_collision: norte_proto::CollisionPolicy,
        queued: bool,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        *self.queued.lock().expect("encoladas") = queued;
        self.transfer(from, to, true, on_collision)
    }

    fn delete(&self, path: VPath, mode: DeleteMode) -> BoxFuture<'static, Result<HostTask, Error>> {
        if let Some(e) = self.error_on_delete.lock().expect("delete error").clone() {
            self.deleted.lock().expect("borrados").push((path, mode));
            self.heartbeat();
            return Box::pin(async move { Err(e) });
        }
        if self.delete_for_real {
            self.disappeared
                .lock()
                .expect("desaparecidos")
                .insert(path.to_wire());
        }
        self.deleted.lock().expect("borrados").push((path, mode));
        self.heartbeat();
        let progress = norte_proto::TaskProgress {
            task_id: norte_proto::TaskId::new(7),
            kind: norte_proto::TaskKind::Delete,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: Some(10),
            entries_done: 0,
            entries_total: Some(1),
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progress);
        *self.progress.lock().expect("progreso") = Some(tx);
        let cancellations = Arc::clone(&self.cancellations);
        Box::pin(async move {
            Ok(HostTask {
                id: norte_proto::TaskId::new(7),
                progress: rx,
                cancel: Arc::new(move || {
                    cancellations.fetch_add(1, Ordering::SeqCst);
                }),
                pause: None,
                cola: None,
                foreign: false,
            })
        })
    }

    fn pack(
        &self,
        params: norte_proto::methods::ArchivePackParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.packed.lock().expect("empaquetados").push(params);
        self.heartbeat();
        self.archive_task(norte_proto::TaskKind::Pack, 11)
    }

    fn test_archive(
        &self,
        params: norte_proto::methods::ArchiveTestParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.checked.lock().expect("comprobados").push(params);
        self.heartbeat();
        self.archive_task(norte_proto::TaskKind::TestArchive, 12)
    }

    fn connections(
        &self,
    ) -> BoxFuture<'static, Result<norte_proto::methods::ConnectionListResult, Error>> {
        let cs = self
            .connections
            .lock()
            .expect("conexiones")
            .clone()
            .unwrap_or_else(|| {
                Ok(norte_proto::methods::ConnectionListResult {
                    connections: Vec::new(),
                    unusable: Vec::new(),
                })
            });
        Box::pin(async move { cs })
    }

    fn provide_secret(
        &self,
        conn: String,
        secret: String,
    ) -> BoxFuture<'static, Result<(), Error>> {
        // What was handed over is recorded so the test can check it arrives
        // AS IS: the whole point of #327 is that nobody touches the password
        // between the field and the core.
        self.secrets_dados
            .lock()
            .expect("secretos_dados")
            .push((conn, secret));
        self.heartbeat();
        let res = self
            .secret
            .lock()
            .expect("secreto")
            .clone()
            .unwrap_or(Ok(()));
        Box::pin(async move { res })
    }

    fn close_connection(&self, path: VPath) -> BoxFuture<'static, Result<bool, Error>> {
        self.closed.lock().expect("cerradas").push(path);
        self.heartbeat();
        let res = self
            .close
            .lock()
            .expect("cierre")
            .clone()
            .unwrap_or(Ok(true));
        Box::pin(async move { res })
    }

    fn split_file(
        &self,
        params: norte_proto::methods::FileSplitParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.split.lock().expect("partidos").push(params);
        self.heartbeat();
        self.archive_task(norte_proto::TaskKind::Split, 13)
    }

    fn combine_files(
        &self,
        params: norte_proto::methods::FileCombineParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.joined.lock().expect("juntados").push(params);
        self.heartbeat();
        self.archive_task(norte_proto::TaskKind::Combine, 14)
    }

    fn log_tail(
        &self,
        cursor: Option<u64>,
        _max: u32,
    ) -> BoxFuture<'static, Result<norte_proto::methods::LogTailResult, Error>> {
        self.log_cursors.lock().expect("cursores").push(cursor);
        self.heartbeat();
        // The response is resolved HERE, not inside the future: what the
        // test arms is whatever was set when the request WENT OUT, and with
        // the gate shut there are two requests alive at once.
        let armed = self.log_remote.lock().expect("registro").take();
        let the_level = self
            .level_remote
            .lock()
            .expect("nivel")
            .clone()
            .unwrap_or_else(|| "info".to_owned());
        let next = {
            let mut last = self.log_next.lock().expect("next");
            if let Some((_, n)) = &armed {
                *last = Some(*n);
            }
            *last
        };
        let gate = self.gate_log.clone();
        Box::pin(async move {
            if let Some(p) = gate {
                p.wait().await;
            }
            // With no `next`, nothing has ever been served: this daemon has
            // no ring to serve.
            let Some(next) = next else {
                return Err(Error::Unsupported);
            };
            Ok(norte_proto::methods::LogTailResult {
                lines: armed.map(|(l, _)| l).unwrap_or_default(),
                next,
                lost: 0,
                level: the_level,
                capacity: 64,
            })
        })
    }

    fn log_level(&self, level: String) -> BoxFuture<'static, Result<String, Error>> {
        self.levels_requests
            .lock()
            .expect("niveles")
            .push(level.clone());
        self.heartbeat();
        // What it answers is what the daemon HAS set, not what was
        // requested: its ring never lowers its level, so asking for less
        // verbosity leaves whatever was already there.
        let the_level = self.level_remote.lock().expect("nivel").clone();
        Box::pin(async move { the_level.ok_or(Error::Unsupported) })
    }

    fn dir_size(&self, paths: Vec<VPath>) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.counts.lock().expect("recuentos").push(paths);
        self.heartbeat();
        let progress = norte_proto::TaskProgress {
            task_id: norte_proto::TaskId::new(9),
            kind: norte_proto::TaskKind::DirSize,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progress);
        *self.progress.lock().expect("progreso") = Some(tx);
        let cancellations = Arc::clone(&self.cancellations);
        Box::pin(async move {
            Ok(HostTask {
                id: norte_proto::TaskId::new(9),
                progress: rx,
                cancel: Arc::new(move || {
                    cancellations.fetch_add(1, Ordering::SeqCst);
                }),
                pause: None,
                cola: None,
                foreign: false,
            })
        })
    }
}
