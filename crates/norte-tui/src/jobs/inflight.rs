//! What the event loop has IN FLIGHT: the background jobs, the probes and
//! the paginated fills that the `select!` arms harvest.
//!
//! These used to be seventeen local variables of `run`, and that's why every
//! function that wanted to leave that loop was born with fifteen parameters.
//! Together they have a name: they are the work THIS process left requested
//! and hasn't arrived yet. Of each kind there is at most ONE — the panel that
//! shows it is one — except the ones that go per slot
//! ([`norte_frontend::layout::BySlot`]), where the pane that is paginating
//! must not be able to starve the other.

use std::collections::VecDeque;

use norte_frontend::layout::BySlot;
use tokio_util::sync::CancellationToken;

use crate::fill::Fill;
use crate::jobs::{CompareRun, SearchRun, SyncRun};
use crate::lua::CommandRun;
use crate::probes::{
    CompareStatProbe, DecorateFetch, LogLevelProbe, LogTailProbe, PanelRenderProbe, PanelsProbe,
    PreviewFetch, Probed, StatProbe,
};
use norte_proto::{Error, VPath};

/// `ai.rename_plan` request IN FLIGHT (M4-IA). Aborting the `JoinHandle`
/// cancels (rule 3): the abort drops the backend's future in the runtime →
/// `CancelOnAbandon` sends `rpc.cancel` (remote) / the timeout+drop aborts
/// the stream (embedded). NOTE: DROPPING the handle only DETACHES the task
/// from tokio — cancelling requires an explicit `abort()`.
pub struct AiRenameRun {
    /// The call to the model, spawned (it's the only long call of the loop).
    pub handle: tokio::task::JoinHandle<Result<norte_proto::methods::AiRenamePlanResult, Error>>,
    /// The pane's dir at LAUNCH time; the plan is applied HERE even if the
    /// user navigates while the model is thinking.
    pub dir: VPath,
    /// The names that were in that dir at launch time.
    ///
    /// The belt requires every `from` in the plan to EXIST where it's going
    /// to be applied (#275), and by the time the model answers the reader
    /// may be somewhere else: asking the pane then would validate the plan
    /// against a directory that isn't its own.
    pub names: Vec<Vec<u8>>,
}

/// ORGANIZE plan request in flight (phase 8), whether it comes from the
/// model (`ai.organize_plan`) or from an `organizer` plugin
/// (`plugin.organize_plan`).
///
/// Only one, and on purpose: the two requests produce the SAME plan and the
/// same modal, so whoever distinguished them here would have to merge them
/// back together at harvest time. Same cancellation discipline as
/// [`AiRenameRun`].
pub struct OrganizeRun {
    /// The request, spawned.
    pub handle: tokio::task::JoinHandle<Result<norte_proto::methods::AiOrganizePlanResult, Error>>,
    /// The pane's dir at LAUNCH time: the plan is applied THERE even if the
    /// reader navigates while the producer is thinking.
    pub dir: VPath,
    /// The names that were in that dir at launch time, to know which folder
    /// of the tree already existed. Asking the pane at harvest time would
    /// paint the tree against a directory that isn't its own.
    pub existentes: Vec<String>,
}

/// An AI plan ALREADY harvested that is waiting for whichever modal is up to
/// close (M4-IA). It carries the BATCH's plan state (§17), which is
/// requested as soon as the AI plan arrives: without it, the modal would
/// open without an approved hash and confirming would stay mute until a
/// second round trip that nobody triggers.
pub struct PendingAiPlan {
    /// The pane's dir at LAUNCH time (where the batch lands).
    pub dir: VPath,
    /// That dir's names at launch time, for the same reason as
    /// [`AiRenameRun::names`].
    pub names: Vec<Vec<u8>>,
    /// from→to pairs from the model.
    pub entries: Vec<norte_proto::methods::AiRenameEntry>,
    /// The batch's verdict: in flight, resolved, or failed.
    pub plan: norte_frontend::BatchPlan,
}

/// `fs.rename_batch_plan` request IN FLIGHT (§17). Spawned for the same
/// reason as [`AiRenameRun`]: it's an `fs.list` of the whole dir against
/// whichever provider applies, and waiting for it inside the `select!` would
/// leave the loop without drawing, without reading keys and without being
/// able to cancel. At most one — the AI rename prompt doesn't open over
/// another modal, so there are never two AI plans alive at once that could
/// step on each other.
pub struct RenameBatchRun {
    /// The call to the core, spawned.
    pub handle:
        tokio::task::JoinHandle<Result<norte_proto::methods::FsRenameBatchPlanResult, Error>>,
}

/// `index.search_semantic` request IN FLIGHT (M4-IA-2). Same cancellation
/// contract as [`AiRenameRun`] (rule 3): `abort()` drops the backend's
/// future → `rpc.cancel` (remote) / drop (embedded); DROPPING the handle
/// only detaches. No dir captured: the query runs against ALL of the
/// index's roots (`root = None`), navigating while it thinks doesn't
/// invalidate it.
pub struct SemanticRun {
    /// The call to the index+model, spawned.
    pub handle: tokio::task::JoinHandle<Result<Vec<norte_proto::methods::SemanticHit>, Error>>,
}

/// The same question to the index, but for the "go to" SECTION (phase 6),
/// not for [`SemanticRun`]'s modal.
///
/// Two runs and not one because what's done with the answer is different —
/// one opens a modal, the other fills a section of an open screen — and
/// because they can overlap: nothing stops "go to" being open right after
/// launching a semantic search, and putting them in the same slot would make
/// one abort the other without anyone having asked for that.
pub struct GotoIndexRun {
    /// The call to the index+model, spawned.
    pub handle: tokio::task::JoinHandle<Result<Vec<norte_proto::methods::SemanticHit>, Error>>,
    /// What was typed when it was launched. The answer is only used if it's
    /// still what's typed: otherwise, it's the answer to another question.
    pub query: String,
}

/// A disk map measurement IN FLIGHT (phase 4).
///
/// Same mold as [`ChecksumRun`] — waiting for the report is spawned and the
/// STATE travels with it, because a report from a cancelled Task is partial
/// — with two extra fields the checksums don't need.
pub struct DiskMapRun {
    /// Waiting for the report, spawned.
    pub handle: tokio::task::JoinHandle<(
        norte_proto::TaskState,
        Result<norte_proto::methods::FsDirUsageReportResult, Error>,
    )>,
    /// The Task, to CANCEL it if another measurement takes over. Aborting
    /// only the wait would leave the core walking a whole `$HOME` with
    /// nobody to collect it — and measuring is exactly the slowest part of
    /// all this.
    pub task: norte_core::backend::TaskObserver,
    /// The slot whose map is being measured.
    pub slot: norte_frontend::layout::SlotId,
    /// The directory that was sent off to be measured.
    ///
    /// It travels with the measurement so late arrivals can be DISCARDED:
    /// measuring a large tree takes time, and in that time the panel may
    /// already be pointing elsewhere. A report landed without checking this
    /// would paint the sizes of one directory under another's title, which
    /// is the kind of lie this panel exists to not tell.
    pub dir: norte_proto::VPath,
}

/// A checksum batch IN FLIGHT (#311).
///
/// The Task is already launched and on the board; what's waited for here is
/// the REPORT, which only makes sense to ask for once the Task finishes —
/// digests don't fit in the progress. `publicado` distinguishes the batch's
/// two faces: `None` is "compute and show me", `Some` is "compare against
/// this".
pub struct ChecksumRun {
    /// Waiting for the report, spawned. Returns the Task's final STATE
    /// alongside the report: a report from a cancelled Task is partial, and
    /// painting it as definitive would accuse files nobody got to read.
    pub handle: tokio::task::JoinHandle<(
        norte_proto::TaskState,
        Result<norte_proto::methods::FsChecksumReportResult, Error>,
    )>,
    /// The Task, so it can be CANCELLED if another batch takes over.
    /// Aborting only the wait would leave the core hashing gigabytes with
    /// nobody to collect them.
    pub task: norte_core::backend::TaskObserver,
    /// What the checksum file published, if this is a verification.
    pub publicado: Option<Publicado>,
}

/// The checksum file, read as needed to judge it (#311).
pub struct Publicado {
    /// The understood lines, in the file's order.
    pub lines: Vec<norte_frontend::checksums::SumLine>,
    /// For each line, which position of the REQUEST its path ended up at, or
    /// `None` if its name can't be written on this system — and that isn't
    /// "missing": it's "can't be named here", and it's fixed another way.
    ///
    /// By index and not by name: the report keeps the requested order, and
    /// matching by base name gave "missing" for a `sub/inside.txt` that was
    /// right there.
    pub asked: Vec<Option<usize>>,
    /// How many lines looked like checksums and weren't understood. With
    /// this greater than zero, "all correct" cannot be said.
    pub refused: usize,
}

/// Everything the loop requested and hasn't harvested yet.
#[derive(Default)]
pub struct InFlight {
    /// Paginated listings filling in the background (ADR 0017): one slot PER
    /// PANE — both panes can be paginating at once, and with a global slot
    /// one's `cd` killed the other's drainer.
    pub fill: BySlot<Fill>,
    /// Where [`Self::fill`]'s sweep is up to (see the `select!` arm).
    pub fill_cursor: usize,
    /// Live search in progress (liveSearch T6): at most one, the virtual
    /// pane is one. Drained in the select and dropped on exit.
    pub search: Option<SearchRun>,
    /// Directory comparison in progress (`Shift+F2`): at most one, the diff
    /// panel is one.
    pub compare: Option<CompareRun>,
    /// Sync in progress (`Ctrl+Y`): at most one — approving a plan while
    /// another applies would be approving blind.
    pub sync: Option<SyncRun>,
    /// `ai.rename_plan` request in flight (M4-IA): at most one — relaunching
    /// aborts the previous one; Esc (BROWSE) cancels it.
    pub ai_rename: Option<AiRenameRun>,
    /// ORGANIZE plan request in flight (phase 8): at most one, for the same
    /// reason as [`Self::ai_rename`] — the review modal is one, and
    /// approving one tree while another is being proposed would be
    /// approving blind.
    pub organize: Option<OrganizeRun>,
    /// AI plan ready that arrived with ANOTHER modal open: it's HELD here
    /// (`App`'s queue is specific to approvals) and opened as soon as there
    /// is no modal — never overwrite it (the `open_next_pending`
    /// discipline).
    pub pending_ai_plan: Option<PendingAiPlan>,
    /// `fs.rename_batch_plan` request in flight (§17): at most one,
    /// harvested in the select like [`Self::ai_rename`].
    pub rename_batch: Option<RenameBatchRun>,
    /// Semantic search in flight (M4-IA-2): same mold as
    /// [`Self::ai_rename`].
    pub semantic: Option<SemanticRun>,
    /// The query to the index from the "go to" screen (phase 6): at most
    /// one, and typing another letter ABORTS the previous one.
    pub goto_index: Option<GotoIndexRun>,
    /// Checksum batch in flight (#311): at most one — the results modal is
    /// one, and launching another CANCELS the previous one's Task on top of
    /// aborting its wait.
    pub checksum: Option<ChecksumRun>,
    /// Disk map measurement in flight (phase 4): at most one — the panel is
    /// one, and launching another CANCELS the previous one's Task. Without
    /// that, navigating fast through a large tree left the core measuring
    /// three directories nobody was going to look at anymore.
    pub disk_map: Option<DiskMapRun>,
    /// Checksums ready that arrived with ANOTHER modal open: they're HELD
    /// here and opened as soon as there is no modal ([`Self::pending_ai_plan`]
    /// discipline). They used to be dropped, and the status bar promised
    /// "close the dialog to see them" over rows that no longer existed.
    pub pending_checksums: Option<(&'static str, Vec<crate::app::ChecksumRow>)>,
    /// Hits ready that arrived with ANOTHER modal open: they're HELD here
    /// and opened as soon as there is no modal ([`Self::pending_ai_plan`]
    /// discipline).
    pub pending_semantic: Option<Vec<norte_proto::methods::SemanticHit>>,
    /// Lua command in flight (M4, ADR 0026): at most ONE — the Lua state is
    /// one — and it's polled inline in the select, because `CommandRun` is
    /// !Send and its future runs in `block_on`, never in a spawn.
    pub lua: Option<(CommandRun, CancellationToken)>,
    /// Lua commands queued while another was running.
    pub lua_queue: VecDeque<String>,
    /// stat-on-focus probe (#52, lazy listing): at most one in flight.
    pub stat: Option<StatProbe>,
    /// Dedup of the probe above, by (pane, path): two panes on the SAME dir
    /// each hydrate their own, and a failed stat isn't retried until the
    /// selection changes.
    pub probed: Probed,
    /// stat probe of the diff panel's SELECTED row (#157). Its dedup lives
    /// in `App::compare_size_probed` and not here, because
    /// `App::compare_size_probe_targets` already consults it to decide
    /// what's missing to request.
    pub compare_stat: Option<CompareStatProbe>,
    /// Plugin decoration fetch in flight (G3b, ADR 0037), per slot.
    pub decorate: BySlot<DecorateFetch>,
    /// L3: one preview read in flight per slot, superseded on move.
    pub preview: BySlot<PreviewFetch>,
    /// One round of `log.tail` in flight (#328): at most one — the log
    /// panel is one, and with two a slow daemon would pile up a request per
    /// loop turn forever.
    pub log_tail: Option<LogTailProbe>,
    /// The plugin catalogue in flight, to declare the panels they
    /// contribute (phase 3): at most one, and it's requested ONCE per
    /// session — what it brings is which slots exist, not any one's
    /// content.
    pub panels: Option<PanelsProbe>,
    /// The repaint of a plugin panel in flight (phase 3): at most one — its
    /// kind's `multi: false` guarantees there is at most one visible plugin
    /// panel — and requesting another REPLACES the previous one, dropping
    /// its receiver.
    pub panel_render: Option<PanelRenderProbe>,
    /// When the next one is due (see [`crate::probes::LOG_TAIL_PERIODO`]).
    /// `None` = now, which is what makes opening the panel ask right away.
    pub log_next_at: Option<tokio::time::Instant>,
    /// A `log.level` request to the daemon in flight (#328): at most one,
    /// and the latest keystroke supersedes the previous one — asking a ring
    /// that only goes up for two levels in a row is asking for the higher
    /// one.
    pub log_level: Option<LogLevelProbe>,
    /// The persistent subshell (#142): ONE per session, started lazily the
    /// first time `app.toggle-panels` is requested and alive until exit.
    ///
    /// It lives here and not in `App` for the same reason as the rest of
    /// this struct: it's a run-loop resource — a process, a pty and a
    /// reader thread — and key dispatch must not be able to touch it. Being
    /// lazy matters: whoever never presses the key doesn't pay for a `fork`
    /// or a pty.
    ///
    /// POSIX: on Windows there is no subshell and `app.toggle-panels`
    /// declines.
    #[cfg(unix)]
    pub subshell: Option<crate::subshell::Subshell>,
}
