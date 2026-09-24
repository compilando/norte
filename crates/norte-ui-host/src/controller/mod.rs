//! The state's only writer.
//!
//! Everything that can change the screen — a renderer action, a daemon
//! answer, a timer — comes in through ONE bounded mailbox and ONE task
//! applies it. There are no locks to cross, and therefore no lock left held
//! during a backend call: what there is, is a queue.
//!
//! From that come the two guarantees the bridge promises and the renderer
//! needs: actions are applied IN ORDER, and the update sequence never skips.
//! When a subscriber does not drain and falls behind, patches do not pile up
//! for it: it is sent a snapshot and is caught up again (ADR 0066).

use std::sync::Arc;

use norte_frontend::PaneState;
use norte_frontend::keymap::{Availability, Effective, Resolution, Resolver};
use norte_frontend::layout::{KindRegistry, Node, Rect, Resolved, RoleId, Roles, SlotId, resolve};
use norte_frontend::nav::{History, Trail, TrailStep};
use norte_proto::{Entry, EntryKind, Error, VPath};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::action::UiAction;
use crate::backend::HostBackend;
use crate::bridge::{
    ActionAck, BridgeEnvelope, InstanceId, MAX_DIALOGS, MAX_ROWS_PER_BATCH, MAX_TASKS,
    MAX_TASKS_RETAINED, MAX_TRANSFER_BATCH, ModalId, RequestToken, RowKey, StaleAction,
    clamp_display,
};
use crate::commands::{Effect, effect_of};
use crate::dto::{
    BrowserSlotView, ColumnHeader, ConnectionView, DialogChoice, DialogView, LayoutView,
    PendingView, RowKind, RowView, SlotPlacement, SlotRole, SlotState, SlotView, StatusView,
    TaskStateView, TaskView, UiNotice, UiUpdate, ViewChange, ViewPatch, ViewSnapshot,
};

// The `impl State` blocks split by topic (ADR 0086). The actor, the
// mailbox, `State` and action dispatch stay here; every child module sees
// this one's private items because it is a descendant, so the move has
// needed no visibility opened up.
mod agents;
mod ai;
mod approvals;
mod dialogs;
mod diskmap;
mod effects;
mod extensions;
mod fileops;
mod gestures;
mod goto;
mod help;
mod input;
mod layout;
mod lifecycle;
mod listing;
mod logpanel;
mod menu;
mod nav;
mod organize;
mod palette;
mod panel;
mod panelplugin;
mod patches;
mod places;
mod preview;
mod profiles;
mod search;
mod selectors;
mod session;
mod settings;
mod sums;
mod sync;
mod tabs;
mod tasks;
mod termpanel;
mod timeline;
mod transfer;
mod tree;
mod viewer;
mod views;
mod wizard;

/// The actor's mailbox capacity. Bounded on purpose: if the renderer sends
/// faster than the host applies, it is made to wait — it is never grown
/// without limit.
const INBOX: usize = 256;

/// FIRST page entries: what is painted before continuing to drain.
///
/// The same number the TUI uses (`navigate::FIRST_PAGE`) and for the same
/// reason: the first frame does not wait for the whole listing, and a
/// directory of half a million entries looks just as fast as one of ten.
const FIRST_PAGE: usize = 100;

/// Entries per batch while the rest drains. Neither one at a time — one
/// message per entry chokes the actor's mailbox — nor all at once.
const FILL_BATCH: usize = 500;

/// How long the viewer's read is awaited.
const DEADLINE_VISOR: std::time::Duration = std::time::Duration::from_secs(20);

/// The help contexts THIS window can be living in.
///
/// Written by hand and closed, like the TUI's (`norte_tui::help_context`),
/// and for the same reason: one derived from the corpus would match itself
/// by construction and catch nothing. A test checks it against the corpus's
/// pages in BOTH DIRECTIONS — a context no page claims is a finding, and a
/// page claiming a context that is not here is one too — which is what
/// keeps renaming a front page from leaving `F1` silently opening the
/// index.
pub const CONTEXTOS: &[&str] = &[
    "browse",
    "viewer",
    "dialog.confirm",
    "dialog.approval",
    "dialog.mkdir",
    "dialog.ai-rename",
];

/// What extensions and their commands are called, already masked:
/// `id → (name, command → title)`.
type Labels = std::collections::HashMap<
    String,
    (
        crate::extensions::Text,
        std::collections::HashMap<String, crate::extensions::Text>,
    ),
>;

/// What it takes to paint a command's output: who, what, and what it
/// answered.
struct OutputRequested {
    /// The extension's id, reverse-DNS validated.
    id: String,
    /// Its name, already masked, with its flag.
    plugin: crate::extensions::Text,
    /// The command's title, already masked, with its flag.
    command: crate::extensions::Text,
    /// What it printed, or why not.
    res: Result<String, Error>,
}

/// What this window has from the DESKTOP.
///
/// Together because they are the same thing seen twice: where something is
/// asked of the hosting process, and what that process answered and is
/// still on screen.
#[derive(Debug, Default)]
struct Desktop {
    /// Where NATIVE effects go out through, when someone is listening.
    ///
    /// `Option` because the state is built before the channel — the first
    /// snapshot comes from it — and because a host with nobody subscribed
    /// has to be able to keep going: an effect nobody picks up is a gesture
    /// that does nothing, not an error.
    native: Option<broadcast::Sender<crate::dto::NativeEffect>>,
    /// The last extension command's output, if it is still on screen.
    ///
    /// Here and not in the extensions manager: a command is launched from
    /// the PALETTE, which does not need the manager open — and does not
    /// open it — and an output stored inside a closed screen is seen by
    /// nobody.
    output: Option<crate::dto::ExtensionOutputView>,
    /// The last waited-for PROGRAM's output (#312), if it is still on
    /// screen.
    program: Option<crate::dto::ProgramOutputView>,
}

/// What this window knows about AGENT sessions.
///
/// Together and not loose in the state: all three describe the same thing —
/// who has asked for permission, whether it is being looked at, and what is
/// being undone — and splitting them apart meant having to remember all
/// three every time one changes.
#[derive(Debug, Default)]
struct Agency {
    /// What has been seen. ALWAYS present: a request arrives when it
    /// arrives, and the panel only decides whether to paint it.
    sessions: crate::agents::Agents,
    /// The panel is open.
    panel: bool,
    /// Which session each in-progress undo task undoes, by task id.
    ///
    /// The outcome arrives through progress, which only carries the id:
    /// without this map there is no way to know which session to release
    /// "undoing" for.
    undos: std::collections::HashMap<u64, String>,
}

/// What is changed about an extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    /// Grant or revoke its capabilities.
    Approval,
    /// Turn it on or off.
    On,
    /// Uninstall it (ADR 0104).
    Uninstallation,
}

impl From<crate::action::ExtensionChange> for Change {
    fn from(c: crate::action::ExtensionChange) -> Self {
        use crate::action::ExtensionChange as E;
        match c {
            E::Approval => Self::Approval,
            E::Enabled => Self::On,
            E::Uninstall => Self::Uninstallation,
        }
    }
}

/// The change already resolved to a concrete value.
///
/// Separate from [`Change`] on purpose: `a` over a row means different
/// things depending on its state, and what travels to the daemon is the
/// resolved one.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Governance {
    /// `plugin.set_approval`, with the anchor that was SHOWN (#282). `None`
    /// when revoking: taking away a permission grants nothing, and refusing
    /// it over a stale digest would keep alive exactly what someone is
    /// trying to remove.
    Approve(bool, Option<String>),
    /// `plugin.set_enabled`.
    TurnOn(bool),
    /// `plugin.uninstall` (ADR 0104). Already confirmed by a human.
    Uninstall,
}

/// Cap on capabilities a grant question can show.
///
/// Not a trim: above this, it does NOT ask. A manifest declaring more
/// capabilities than fit on one screen does not produce an informed
/// decision, and granting what was not read is what this question exists to
/// prevent.
const MAX_CAPABILITIES: usize = 32;

/// Cap on characters of an extension command's output.
///
/// What a plugin prints has no cap on its own side: a command can return a
/// megabyte and letting it through hands it the window. It is applied
/// BEFORE masking: otherwise, a 100 MB output gets masked whole — and
/// materializes whole in the writer's task — just so four thousand
/// characters survive.
const MAX_OUTPUT: usize = 4_000;

/// Cap on LINES of that output.
///
/// Lines cross loose so that a line break does not mark an honest output as
/// hostile, and a list also needs its own cap.
const MAX_OUTPUT_LINES: usize = 200;

/// Deadline for RUNNING an extension command.
///
/// Separate from the one for reading the catalog, and much longer: on the
/// other side runs third-party code that can be indexing or talking over
/// the network, and cutting it off at five seconds does not stop it — it
/// keeps running on the daemon, with its effects — it only leaves this
/// window not knowing how it ended.
const DEADLINE_COMMAND: std::time::Duration = std::time::Duration::from_mins(1);

/// Deadline for an extensions call (catalog or page).
///
/// Help is painted without waiting for it, so this deadline does not govern
/// a screen: it governs a task that, if it never came back, would leave an
/// `id` claimed and a page blank forever.
const DEADLINE_PLUGINS: std::time::Duration = std::time::Duration::from_secs(5);

/// How long an already-FINISHED task stays on the board.
///
/// Without this a terminal one stayed until another pushed it out by the row
/// cap, so what the panel showed at a glance was the session's history and
/// not what is happening. Ten seconds are enough to read the "✓" or the
/// error, and below that the board goes back to talking about the present.
/// The same number as the TUI, on purpose: two frontends that expire
/// differently are two answers to "is this still running?".
const TTL_TASK_TERMINAL: std::time::Duration = std::time::Duration::from_secs(10);

/// How often it is checked whether the session changed and, if it did,
/// written.
///
/// The same second as the terminal (`session_tick` in its loop), on purpose:
/// two frontends saving at different rates are two answers to "where was I"
/// after a close that did not make it in time. And a second is cheap:
/// comparing the body against the last one sent is all a tick with no
/// changes does.
const SESSION_TIC: std::time::Duration = std::time::Duration::from_secs(1);

/// How often what the terminal pane's shell has written is flushed.
///
/// Thirty times a second: that is what makes typing in there feel like
/// typing in a terminal and not like sending a telegram. A tick that finds
/// no bytes produces no patch and wakes no renderer, so an idle shell costs
/// no more than checking an empty mailbox.
///
/// And the pump only runs while the panel exists: see `Message::TerminalTic`.
const TERMINAL_TIC: std::time::Duration = std::time::Duration::from_millis(33);

/// Deadline for a plan request to a model.
///
/// Generous: thinking is what it does. It is the UPPER cap, so a call that
/// never comes back does not leave the request in flight forever — with
/// `Escape` as the only exit and nothing on screen saying it is still alive.
const DEADLINE_IA: std::time::Duration = std::time::Duration::from_mins(2);

/// Cap on a typed name, in bytes. Neither `NAME_MAX` (which belongs to the
/// filesystem and is not known here) nor the screen's: a generous cap that
/// keeps a renderer from sending a megabyte, and that REJECTS instead of
/// trimming — trimming a name is inventing another one.
const MAX_NAME: usize = 4096;

/// What the viewer reads from a file: a 256 KiB header. The rest is NOT
/// read — the same budget as the TUI, and for the same reason (ADR 0005): a
/// viewer is not an excuse to pull in a gigabyte file.
const VISOR_CAP: u64 = 256 * 1024;

/// The most read from an IMAGE to preview it: 8 MiB.
///
/// Separate from the text viewer's cap, which is a HEADER on purpose — an
/// image cannot be shown halfway. A phone photo fits with room to spare; a
/// scanner TIFF does not, and then it is not painted, and it is reported
/// (ADR 0069).
const IMAGEN_CAP: u64 = 8 * 1024 * 1024;

/// Simultaneous probes against the daemon. The same number as the TUI, and
/// for the same reason: a remote session cannot afford N trips in series.
const POLLS_AT_ONCE: usize = 8;

/// How long ONE probe is awaited. A hung provider cannot take the rest of
/// the batch down with it.
const DEADLINE_PROBE: std::time::Duration = std::time::Duration::from_secs(5);

/// How many entries are probed per batch. It is a SCREEN with slack: more
/// is not seen, and every probe is a trip to the daemon.
const MAX_PROBES: usize = 200;

/// Updates retained for a slow subscriber. Once past that, the subscriber
/// finds out it fell behind and requests a snapshot: it is the cheap
/// recovery, the one that spends no host memory.
const UPDATE_BUFFER: usize = 64;

/// How many rows a page moves in help.
///
/// The renderer owns the body's scroll — a corpus page crosses whole — so
/// this number only governs the CURSORS, the ones the host carries.
const HELP_PAGE: usize = 10;

/// How to start the host.
pub struct UiHostOptions {
    /// Who it talks to.
    pub backend: Arc<dyn HostBackend>,
    /// Where the listing starts.
    pub initial_dir: VPath,
    /// [`Self::initial_dir`] was TYPED by a human on the command line.
    ///
    /// With `false` it is the process's current directory, i.e. a default
    /// the session has every right to overwrite. With `true` it is an
    /// intent, and it wins: `norte-gui /usr/bin` with a saved session used
    /// to open where you were yesterday and silently swallow the argument.
    ///
    /// Only the ACTIVE pane. The other one stays where the session left it:
    /// half a screen of memory nobody asked to be dropped. It is the same
    /// rule as `App::pin_start_dir` in the terminal.
    pub initial_dir_requested: bool,
    /// This window is the other end of a HANDOFF (`--attach`, phase 9), so
    /// besides the screen it claims the MARKS the other frontend left.
    ///
    /// The same nature as [`Self::initial_dir_requested`] — how the process was
    /// launched — and that is why it lives next to it. Without it, a
    /// startup is a startup: marks from a handoff left halfway do not come
    /// back to life the next day.
    pub attach: bool,
    /// Language already negotiated, so the renderer can request its
    /// catalog.
    pub locale: String,
    /// The listing screen's EFFECTIVE keymap, already merged
    /// (preset + user layers). Built by whoever starts the host — reading
    /// configuration is not its business; [`crate::keys::keymap_de_preset`]
    /// does the minimum for a test or a startup with no configuration.
    pub keymap: Effective,
    /// The VIEWER screen's effective keymap, with the same layers. It goes
    /// apart because it is a different screen: with the viewer open the
    /// keys are its own, same as in the TUI, and mixing them would be
    /// inventing a third input context that exists in no preset.
    pub keymap_viewer: Effective,
    /// A DIALOG's effective keymap, with the same layers.
    ///
    /// Third screen, for the same reason as the viewer's: with a question in
    /// front, the keys are its own. Without this, this window handled its
    /// dialogs with fixed keys and a preset rebinding `dialog.confirm`
    /// changed the TUI and not the window (#287).
    pub keymap_dialog: Effective,
    /// The layout: the slot tree. From a factory preset
    /// (`norte_frontend::layout::presets::tree`) or from the user's
    /// configuration; the host reads no files.
    pub layout: Node,
    /// The window's INITIAL size, in layout cells. The renderer corrects it
    /// as soon as it knows its own ([`UiAction::SetViewport`]).
    pub viewport: (u16, u16),
    /// How far this frontend goes: only looking, or also writing.
    ///
    /// Not an amputation of the host — it knows how to mutate and its tests
    /// prove it — but a decision made by whoever assembles it. The graphical
    /// window started at
    /// [`crate::commands::Effects::SoloRead`] until task 5.4's security
    /// review lifted its toggle; today all three setups (window, TUI and
    /// tests) use [`crate::commands::Effects::Full`], and `SoloRead`
    /// remains the position a setup that wants neither destructive nor
    /// policy authority can choose.
    pub effects: crate::commands::Effects,
    /// The ALREADY loaded configuration, for read-only settings.
    ///
    /// Read by whoever starts the host, ONCE, like everything else: a
    /// frontend that re-reads it on its own ends up showing settings that
    /// are not the ones it is using.
    pub settings: norte_frontend::config::FrontendConfig,
    /// Where each thing lives, already resolved. See
    /// [`crate::settings::HostPaths`].
    pub paths: crate::settings::HostPaths,
    /// The active theme, already resolved to role → color pairs by whoever
    /// starts it.
    pub theme: crate::pickers::HostTheme,
    /// The layouts the user has in `layouts/*.toml`, ALREADY read.
    ///
    /// Read, and not by name: the selector PAINTS each one's shape, and
    /// reading them on cursor move would be I/O in the event loop. Whoever
    /// has the disk in front of them is whoever starts the window, not the
    /// host.
    pub user_layouts: Vec<norte_frontend::layout_picker::UserLayout>,
    /// The WHOLE column configuration, not an already-resolved list.
    ///
    /// Columns are configured PER SCHEME (`[ui.columns.schemes.sftp]`), and
    /// resolving them once at startup left that half of the configuration
    /// dead: an `attr:` column that only exists in `sftp` was never
    /// requested and never painted, because the attributes travelling in
    /// every listing had been frozen with the startup scheme's.
    pub columns: norte_frontend::columns::ColumnsSettings,
    /// The profile it STARTED with, if any (`--profile`, #307).
    ///
    /// Already applied: its layers entered the configuration arriving in
    /// [`Self::settings`], because a profile named on the command line is
    /// known before connecting to anything, and so reaches all the way to
    /// `[ui] lang`. What the host needs is to KNOW it, to mark it active in
    /// the selector and so the session remembers it; without this, starting
    /// with `--profile` gave a correct window whose selector said none was
    /// set.
    ///
    /// `OsString` because it is a directory name (#245).
    pub profile: Option<std::ffi::OsString>,
    /// The log ring the `log` panel reads from (#326).
    ///
    /// Mounted by the PROCESS at startup — the `tracing` subscriber is
    /// installed once — and it arrives here because the host mounts no
    /// subscribers: it reads them. `None` = this binary did not mount it,
    /// and then the panel says so instead of showing itself empty, which
    /// would be indistinguishable from "nothing has happened".
    ///
    /// It comes as an option and not through a later `set_*` because the
    /// panel can exist in the startup layout: mounting it afterward would
    /// leave the first snapshot with a log that is not the real one.
    pub log_ring: Option<norte_config::logring::LogRing>,
}

/// What a subscriber receives.
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    /// An update in its envelope.
    Message(Box<BridgeEnvelope<UiUpdate>>),
    /// This subscriber fell behind and missed updates. What it has to do is
    /// request a snapshot ([`UiAction::Resync`]), not try to guess what was
    /// missing.
    Lagged,
}

/// Subscription to the host's updates.
pub struct UiSubscription {
    rx: broadcast::Receiver<BridgeEnvelope<UiUpdate>>,
}

impl UiSubscription {
    /// The next update, or `None` if the host shut down.
    pub async fn recv(&mut self) -> Option<Update> {
        match self.rx.recv().await {
            Ok(m) => Some(Update::Message(Box::new(m))),
            Err(broadcast::error::RecvError::Lagged(_)) => Some(Update::Lagged),
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }
}

/// What was left unfinished on shutdown.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShutdownReport {
    /// Something was left unfinished: a queued or running task, or the
    /// session unwritten. It IS SAID: shutting down silently with a copy
    /// halfway is how an operation gets lost without anyone knowing.
    pub incomplete: bool,
}

/// The session's tick, every second, like the terminal's: the screen is
/// saved while it is being used, not only on close. A close that does not
/// arrive — the process dead, the socket stuck past the deadline — used to
/// lose everything walked since startup. Changes to the TREE are also
/// written immediately, without waiting for the tick
/// (`apply_layout_with`, `apply_tree`). Dies with the mailbox, like
/// the other pumps.
fn pump_session_tick(mailbox: mpsc::Sender<Message>) {
    tokio::spawn(async move {
        let mut tic = tokio::time::interval(SESSION_TIC);
        // `interval`'s first tick is immediate, and there is nothing to
        // write an instant after starting.
        tic.tick().await;
        loop {
            tic.tick().await;
            if mailbox.send(Message::SessionTic).await.is_err() {
                return;
            }
        }
    });
}

/// Forwards the backend's `plugin.notice`s to the host's mailbox (ADR 0100).
/// A function separate from `start` because the pump list was already
/// filling the line limit, and its shape is the same as the others': a task
/// that dies with the channel that feeds it.
fn pump_plugin_notices(backend: &dyn HostBackend, mailbox: mpsc::Sender<Message>) {
    let Some(mut notices) = backend.take_plugin_notices() else {
        return;
    };
    tokio::spawn(async move {
        while let Some(n) = notices.recv().await {
            if mailbox
                .send(Message::NoticePlugin(Box::new(n)))
                .await
                .is_err()
            {
                return;
            }
        }
    });
}

/// Forwards to the host's mailbox EVERYTHING the backend pushes on its own:
/// connection, plaintext session, entry failures, plugin notices, approvals
/// and unrelated tasks.
///
/// Together because they are the same decision six times over — a task that
/// dies with the channel feeding it — and because they come in through the
/// SAME mailbox: a lost-connection notice has to be ordered with whatever was
/// happening when it was lost. Kept out of `start` because of the line
/// limit.
fn pump_backend_channels(
    backend: &dyn HostBackend,
    mailbox: &mpsc::Sender<Message>,
    effects: crate::commands::Effects,
) {
    // The connection's two channels belong to the FIRST owner, so they are
    // taken once, here.
    if let Some(mut eventos) = backend.take_conn_events() {
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            while let Some(ev) = eventos.recv().await {
                if mailbox.send(Message::Connection(ev)).await.is_err() {
                    return;
                }
            }
        });
    }
    // Plaintext-session notices (#44) are ALWAYS taken: they do not depend
    // on whether this window can write. That a listing being READ travels
    // unencrypted is a fact for whoever is looking at it, not a permission.
    if let Some(mut degraded) = backend.take_degraded() {
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            while let Some(d) = degraded.recv().await {
                if mailbox.send(Message::Degraded(Box::new(d))).await.is_err() {
                    return;
                }
            }
        });
    }
    // And failures (#322), with the same criterion: why a machine could NOT
    // be entered is told to whoever tried, whether this window can write or
    // not.
    if let Some(mut failed) = backend.take_failed() {
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            while let Some(f) = failed.recv().await {
                if mailbox.send(Message::Failed(Box::new(f))).await.is_err() {
                    return;
                }
            }
        });
    }
    // Hook notices (ADR 0100) talk about files that have already changed, so
    // they are read whether it can write or not.
    pump_plugin_notices(backend, mailbox.clone());
    // Policy approvals are a MUTATION by delegation: saying yes to an
    // agent's operation. A frontend that cannot write yet cannot authorize
    // another one to write either, so in read-only the channel is not even
    // taken (and the dialog does not exist, which is more honest than one
    // that does not respond).
    if effects == crate::commands::Effects::Full
        && let Some(mut approvals) = backend.take_approvals()
    {
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            while let Some(req) = approvals.recv().await {
                if mailbox
                    .send(Message::Approval(Box::new(req)))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
    }
    if let Some(mut foreign) = backend.take_foreign_tasks() {
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            while let Some(task) = foreign.recv().await {
                if mailbox
                    .send(Message::TaskNew(Box::new((task, Vec::new(), None))))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
    }
}

/// The host: a cheap-to-clone handle over the single writer.
#[derive(Clone)]
pub struct UiHost {
    inbox: mpsc::Sender<Message>,
    updates: broadcast::Sender<BridgeEnvelope<UiUpdate>>,
    native: broadcast::Sender<crate::dto::NativeEffect>,
    instance: InstanceId,
}

/// What comes back from a listing: the token that requested it, the slot it
/// goes to, the directory and the result.
/// What the viewer requested: token, path, the header read and the plugin
/// preview if one applied.
type Content = (
    RequestToken,
    VPath,
    Result<Vec<u8>, Error>,
    Option<norte_proto::methods::PluginPreviewStyled>,
);

/// The same, for the DOCKED viewer (#291): the slot that requested it goes
/// in front, and it is not resolved on arrival — the slot may have closed,
/// and then the answer is dropped.
type PreviewContent = (u32, Content);

/// What comes back from a listing: its token, the slot, the directory, and
/// the first page's entries with HOW MANY the provider skipped.
type ResponseListing = (
    RequestToken,
    u32,
    VPath,
    Result<(Vec<Entry>, Option<u64>), Error>,
);

/// What comes back from a probe batch: the directory being probed, the slot,
/// and the pairs `(what was requested, what the provider answered)`.
type Probes = (VPath, u32, Vec<(VPath, Entry)>);

/// What comes back from measuring a disk map (phase 4).
///
/// The slot that requested it, the TOKEN for that request — one that is not
/// the live one belongs to a directory already left behind — and the result,
/// with the Task's STATE attached to the report: one from a cancelled Task
/// is partial, and painting it as complete turns a huge directory into a
/// small one.
type MapMeasure = (
    u32,
    RequestToken,
    Result<
        (
            norte_proto::TaskState,
            norte_proto::methods::FsDirUsageReportResult,
        ),
        Error,
    >,
);

enum Message {
    Action(Box<UiAction>, oneshot::Sender<ActionAck>),
    /// The BYTES of the image the viewer has open, if any.
    ///
    /// A query and not an action: it changes nothing and produces no patch.
    /// It still goes through the mailbox because the state belongs to the
    /// actor, and answering it from outside would be reading what someone
    /// else is writing.
    BytesDeImagen(oneshot::Sender<Option<std::sync::Arc<Vec<u8>>>>),
    /// The answer to a listing requested earlier. Comes back to the actor as
    /// just another message: that way the state keeps being touched by a
    /// single writer.
    ///
    /// Carries the SLOT that requested it and is not resolved on arrival: if
    /// focus moved to the neighboring pane while it was in flight, landing
    /// "on the active one" would put a directory on the wrong screen.
    Listing(Box<ResponseListing>),
    /// A scheme's attribute catalog, already resolved.
    Catalog(Box<(String, norte_proto::AttrCatalog)>),
    /// An agent op awaits a human decision.
    Approval(Box<norte_proto::methods::PolicyApprovalRequired>),
    /// This approval's TTL ran out: the daemon no longer accepts it.
    ApprovalExpired(u64),
    /// The chosen theme is (or is not) in `norte.toml` now. `Some(key)` is
    /// the reason it could not be saved; `None` means it was saved.
    ///
    /// Only the failure IS SAID. A "saved" for every chosen theme would be a
    /// message per Enter on a screen whose result is already visible: the
    /// colors changed.
    ThemePersisted(Option<&'static str>),
    /// A column's width is (or is not) in `norte.toml` now (bridge 64). Same
    /// treatment as the theme: only the failure is reported.
    WidthPersisted(Option<&'static str>),
    /// A `[ui] theme` that was a PATH, already read outside the actor.
    ///
    /// Carries the spec so it can be named in the native effect — whoever
    /// hosts it resolves it again on their own, because the colors cross to
    /// the webview converted into CSS variables and that conversion is not
    /// the host's — and, boxed, because a `Theme` is large next to the rest
    /// of the enum.
    ///
    /// The error is a KEY, not the error: its own carries the path inside
    /// (#73).
    ThemeResolved(Box<(String, Result<norte_theme::Theme, &'static str>)>),
    /// The session's tick: every second, like the terminal. If the screen
    /// changed since the last write, it is written; if not, nothing.
    SessionTic,
    /// The TERMINAL panel's tick (#362): flushes what the shell has written
    /// and, if something changed, republishes its slot.
    ///
    /// Its own tick, much faster than the session's, because a shell is
    /// watched while it responds: at one second, typing in there feels
    /// broken.
    ///
    /// **Not a perpetual pump**: it carries its EPOCH and rearms in the
    /// handler, only if the panel is still on screen — the same mechanism as
    /// the log's polling, and for the same reason. A 30 Hz timer that
    /// outlived the panel would be waking up the actor to paint nothing,
    /// which is the "spins at rest" the terminal already paid for once.
    TerminalTic(u64),
    /// A `session.put` answered: what the daemon said and the body that was
    /// sent, to record it as written only if it really went in.
    SessionPlaced(
        Box<(
            Result<u64, Error>,
            std::sync::Arc<norte_frontend::session::SessionBody>,
        )>,
    ),
    /// The session re-read after a conflict: another window wrote in
    /// between and the revision being written over is no longer valid.
    SessionReread(Result<(norte_proto::methods::Session, bool), Error>),
    /// The HANDOFF to the terminal finished (phase 9): the screen is
    /// written and the session, released — or it could not be, and then
    /// nothing happens and it is reported.
    HandedOff {
        /// This window was the owner and has stopped being one.
        released: bool,
    },
    /// This FINISHED task's time on the board ran out
    /// ([`TTL_TASK_TERMINAL`]). Carries the connection EPOCH it was
    /// registered in: after a daemon handoff the ids start over at 1, and
    /// expiring by number would evict a live task that only shares its
    /// number with the one that left.
    TaskExpired(u64, u64),
    /// Something requested outside the actor finished and has to be SAID:
    /// the message's key (today, a pause the daemon does not know how to
    /// do).
    Say(&'static str),
    /// The light progress bar changes with no progress arriving: it passed
    /// its threshold, the panel's, or the "✓"'s time ran out (ADR 0146).
    Strip,
    /// The `policy.decide` that WAS APPROVING did not reach the daemon.
    /// A `policy.decide` that did not go well: which approval and under
    /// which key it is counted (#279).
    ApprovalNoDelivered(u64, &'static str),
    /// More entries from the listing draining in the background.
    ///
    /// The `bool` says whether it is the LAST batch. Without it, `draining`
    /// was raised on requesting the listing and nobody ever lowered it —
    /// not even when the stream ran out within the first page — so the
    /// field did not mean "still arriving" but "this was requested at some
    /// point", and anyone consulting it to decide got it wrong.
    MoreEntries(Box<(RequestToken, u32, Vec<Entry>, bool)>),
    /// What a request launched for an OVERLAY answered.
    ///
    /// The five travel together because they are the same story: a surface
    /// opened WITHOUT waiting — the documentation and the mount table are
    /// cosmetic, and a blank window until the daemon answers is worse than a
    /// list gaining rows half a second later — and this is the answer
    /// arriving late. Each one checks its surface is still open before
    /// touching anything.
    Background(Box<Background>),
    /// The content the viewer requested.
    /// What the viewer requested: the file's header and, if some
    /// `previewer` plugin applied, its styled preview.
    ///
    /// Both in the SAME message because they are a single answer to a single
    /// key: sending them separately would open the raw viewer and swap it
    /// for the preview an instant later, a flicker nobody asked for.
    Content(Box<Content>),
    /// What a preview slot requested (#291): same as [`Self::Content`] but
    /// for the docked viewer, and with the slot in front.
    PreviewContent(Box<PreviewContent>),
    /// The frame a plugin pane painted (phase 3), with the token for the
    /// request that asked for it: one that is not the live one belongs to a
    /// cursor that has already moved.
    PanelContent(
        Box<(
            u32,
            RequestToken,
            Result<Option<norte_proto::methods::PanelFrame>, Error>,
        )>,
    ),
    /// What measuring a disk map found (phase 4), with the token for the
    /// request that asked for it: one that is not the live one belongs to a
    /// directory already left behind. The STATE travels with the report
    /// because one from a cancelled Task is partial, and painting it as
    /// complete turns a huge directory into a small one.
    MapContent(Box<MapMeasure>),
    /// What a probe found out about a few entries (a lazy listing's size and
    /// date).
    ///
    /// Carries the DIRECTORY being probed, not a token or an epoch. The
    /// token reads `None` as soon as the first page lands — i.e. the guard
    /// watching it was not storing anything — and the epoch goes up with
    /// every fill batch, which does not invalidate a probe: what invalidates
    /// a size is the listing belonging to a DIFFERENT place. Since hydration
    /// matches by path, a shifted index does not matter.
    ///
    /// And it carries PAIRS `(requested, answered)`, because a provider can
    /// answer with a different spelling of the same name, and the entry that
    /// needs hydrating is the one that was requested.
    Hydrated(Box<Probes>),
    /// A freshly enqueued Task, with its progress, its cancellation and the
    /// directories it will leave out of date.
    TaskNew(Box<(crate::backend::HostTask, Vec<VPath>, Option<Retry>)>),
    /// What a slot's location accepts: how it folds names (#268) and
    /// whether it refuses writes.
    Capabilities(u32, VPath, norte_proto::Capabilities),
    /// Enqueuing it failed. The user has to find out: they requested a
    /// delete.
    TaskFailed(Box<Error>),
    /// A rejection on enqueuing ONE batch entry (#271). Separate from
    /// [`Self::TaskFailed`] on purpose: that one is sent by anyone enqueuing
    /// anything — a search, a plan, an undo — and its place is the status
    /// bar; this one is only sent by a batch's loop, and its place is the
    /// batch's COUNT.
    BatchTaskRejected(Box<Error>),
    /// The connection to the daemon changed state.
    Connection(norte_client::ConnEvent),
    /// A provider session travels UNENCRYPTED (#44).
    Degraded(Box<norte_proto::methods::ConnectionDegraded>),
    /// A connection could NOT be opened, and why (#322).
    Failed(Box<norte_proto::methods::ConnectionFailed>),
    /// A `hook` plugin said something about an already-registered mutation,
    /// or the daemon turned off its hooks (0.69.0, ADR 0100).
    NoticePlugin(Box<norte_proto::methods::PluginNotice>),
    /// The secret was delivered (or not), and with it what to do with the
    /// navigation `SecretNeeded` had suspended (#327).
    SecretDelivered(Box<(u32, VPath, Result<(), Error>)>),
    /// Time to check whether the log has anything new (#326). Carries the
    /// EPOCH of the opening that scheduled it: one from a previous opening
    /// is left to die instead of rearming forever.
    LogTic(u64),
    /// What the daemon answered to `log.tail` (#328), with the EPOCH of the
    /// opening that requested it.
    ///
    /// The epoch is not decoration: a close and an open fit between asking
    /// and answering, and lines from the previous session landing on the
    /// new panel would be history nobody asked for, ahead of the real one.
    LogRemote(u64, Box<Result<norte_proto::methods::LogTailResult, Error>>),
    /// What the daemon answered to `log.level` (#328): the level really left
    /// set, which may not be the one requested.
    LogLevel(u64, Box<Result<String, Error>>),
    /// A progress snapshot. Through the SAME queue as everything else,
    /// which is what guarantees a terminal state neither jumps ahead nor
    /// gets lost.
    Progress(Box<norte_proto::TaskProgress>),
    /// The report of a Task that has already finished and DOES have a
    /// report.
    ///
    /// Carries the whole `Result` and not an `Option`: "went fine" and "the
    /// daemon does not know how to report" are two different things, and
    /// collapsing them is exactly what these reports exist not to do.
    Report(Box<(u64, u64, tasks::Report)>),
    /// The `fs.stat` done between creating a file and opening it (#303): the
    /// path that was created, and whether what is there is still a regular
    /// file.
    ///
    /// With no epoch: what is decided with this is opening an ABSOLUTE path
    /// on this machine's desktop, which does not mean something different
    /// depending on which daemon answers — unlike reports, whose task ids
    /// start over at 1 after a handoff.
    CreatedChecked(Box<(norte_proto::VPath, Verdict)>),
    /// A favorite was saved (#309): its name, where it points to and, if it
    /// failed, the reason's key. The in-memory copy is not touched until the
    /// disk answers.
    ///
    /// The destination travels in the message and is not re-read on
    /// arrival: between requesting the name and saving, the pane may have
    /// navigated, and reflecting "where I am now" would put a different
    /// favorite in the list than the one just written to the file.
    FavoritePersisted(Box<(String, norte_proto::VPath, Option<&'static str>)>),
    /// The profile was written (or not): name and the failure's key (#318).
    ProfileSaved(Box<(String, Option<&'static str>)>),
    /// A favorite was removed, in the same shape.
    FavoriteRemoved(Box<(String, Option<&'static str>)>),
    TurnOff(oneshot::Sender<ShutdownReport>),
}

/// What the `fs.stat` done between creating a file and opening it answered
/// (#303).
///
/// Three values and not a `bool` because "it is not the file" and "could not
/// be checked" are different things and are said differently: calling a
/// handed-off daemon tampering is a false accusation, and teaching the
/// reader to ignore that message is what makes it useless the day it is
/// true.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// Still a regular file: go ahead.
    IsTheFile,
    /// A link, a folder or nothing. All three are said the same way: naming
    /// which one would confirm the link to whoever planted it.
    NoLongerTheFile,
    /// The `stat` failed. Nothing opens, and it is reported that the check
    /// could not be made.
    Unknown,
}

impl From<bool> for Verdict {
    fn from(regular: bool) -> Self {
        if regular {
            Self::IsTheFile
        } else {
            Self::NoLongerTheFile
        }
    }
}

/// A background request's answer, by surface.
///
/// A separate enum and not five [`Message`] variants: the actor is a
/// dispatcher, and five arms that do the same thing — checking their
/// surface is still open and returning patches — are one arm with five
/// cases.
/// A decoration batch's answer: which decoration GENERATION (a batch
/// requested before the manager turned off a plugin does not describe what
/// there is), which slot, which directory, the badges by path and the cells
/// for each `plugin:` column.
type Adornos = (
    u64,
    u32,
    VPath,
    std::collections::HashMap<VPath, norte_frontend::Decoration>,
    std::collections::HashMap<String, std::collections::HashMap<VPath, String>>,
    // The label the manifest gave each plugin column, already sanitized:
    // the header prefers it over its id.
    std::collections::BTreeMap<String, String>,
);

enum Background {
    /// The plugin catalog HELP requested, for its side panel.
    HelpPlugins(Result<norte_proto::methods::PluginListResult, Error>),
    /// The catalog requested on STARTUP, to declare which PANES the plugins
    /// contribute (phase 3).
    ///
    /// Separate from help's and the manager's, and not by whim: those two
    /// exit early if their surface is closed, and a plugin pane has to be
    /// able to paint without anyone having opened help or the manager. What
    /// it brings is the DECLARATION of which slots exist, not any of their
    /// content.
    PluginPanes(Result<norte_proto::methods::PluginListResult, Error>),
    /// A plugin's page, requested on opening it in help.
    PluginPage(
        String,
        Result<norte_proto::methods::PluginHelpResult, Error>,
    ),
    /// The catalog the extensions MANAGER requested, with the OPENING that
    /// requested it.
    ///
    /// Separate from help's: they are two surfaces with two lifetimes, and
    /// sharing the answer would force each to check whether the other is
    /// still open.
    ///
    /// The opening is needed because "still open" is not "is the same one".
    /// Open (request A, slow), `esc`, reopen: A used to time out and its
    /// `unwrap_or(empty)` turned off B's "loading" and said "none
    /// installed" until B arrived.
    Catalog(
        u64,
        u64,
        Result<norte_proto::methods::PluginListResult, Error>,
    ),
    /// What needs to be known about a transfer's DESTINATION before the
    /// human says yes: whether it fits (#149) and whether it knows to
    /// secure its writes (#164). Carries the modal it belongs to, because it
    /// arrives late.
    ///
    /// Both go together because they are the same question asked to the
    /// same place at the same moment, and splitting them would cost two I/O
    /// round trips per dialog to paint two adjoining lines — the same split
    /// the terminal makes in `DestCheck`.
    DestinationNotices(ModalId, Vec<String>),
    /// A `policy.undo_session`'s task already has an id: it is tied to its
    /// session.
    SessionUndo(u64, String),
    /// The profiles in `profiles/`, already read, and what to do with them:
    /// `None` = open the selector, `Some(forward)` = jump to the neighbor
    /// without opening anything.
    ///
    /// Reading them is disk I/O — a directory and a `norte.toml` per
    /// profile — so it comes through here like everything that cannot run
    /// in the actor.
    Profiles(
        Vec<norte_frontend::profile_picker::UserProfile>,
        Option<bool>,
    ),
    /// A profile's configuration, already loaded, with its name.
    ///
    /// `Err` is the reason's key: a profile that fails to load changes
    /// NOTHING — it stays on the one you were on, which is what ADR 0079 D7
    /// asks for a switch.
    ProfileLoaded(
        std::ffi::OsString,
        Box<Result<norte_frontend::config::FrontendConfig, &'static str>>,
    ),
    /// An F11 setting is (or is not) in `norte.toml` now, and the
    /// configuration re-read with it. Boxed because a `FrontendConfig` is
    /// large next to the rest of the enum.
    SettingWritten(Box<settings::SettingWritten>),
    /// An F11 key is no longer in `norte.toml` — reset.
    SettingRestored(Box<settings::SettingRestored>),
    /// The catalog the PALETTE requested, for its plugin rows.
    PalettePlugins(u64, Result<norte_proto::methods::PluginListResult, Error>),
    /// A governance change (approve/revoke, turn on/off) answered.
    ///
    /// Carries the OPENING for the same reason as the catalog: the answer
    /// can arrive over a manager that has already closed and reopened.
    Governed(u64, Result<(), Error>),
    /// A `[config.<key>]` write answered: the opening of the manager that
    /// requested it, which extension, and what the daemon said.
    ///
    /// The opening is needed for the same reason as in the catalog: closing
    /// the manager and reopening it while a write is in flight let the
    /// first one's failure close the second one's card.
    ConfigWritten(u64, String, Result<(), Error>),
    /// An extension command's output: the opening that requested it, the
    /// extension's id, its two labels WITH their flag, and what it
    /// answered.
    ///
    /// The labels travel with their flag and not just masked because there
    /// is no coming back from a mask: a flag computed afterward, over
    /// already-masked text, always comes out `false` and the panel claims
    /// to be faithful.
    CommandOutput(u64, Box<OutputRequested>),
    /// An extension's `[config]` schema, requested on opening its card.
    PluginTab(
        String,
        Result<norte_proto::methods::PluginGetConfigResult, Error>,
    ),
    /// The host's volumes, with the OPENING of the selector that requested
    /// them. See [`Background::Catalog`].
    Volumes(u64, Result<Vec<norte_proto::methods::Volume>, Error>),
    /// A timeline page (#359): the slot, the request's token and where it
    /// was requested from (`None` = the first one).
    TimelinePage(
        u32,
        RequestToken,
        Option<i64>,
        Result<norte_proto::methods::JournalListResult, Error>,
    ),
    /// The connections for "go to" (#357), with the OPENING that requested
    /// them: an answer from a previous opening does not fill in the current
    /// one.
    GoToConnections(
        u64,
        Result<Vec<norte_proto::methods::ConnectionEntry>, Error>,
    ),
    /// What the index answered to a "go to" query (#357): the opening and
    /// the query that were asked about, to drop the answer if it is no
    /// longer what is written.
    GoToIndex(
        u64,
        String,
        Result<Vec<norte_proto::methods::SemanticHit>, Error>,
    ),
    /// The configured connections, with the OPENING that requested them
    /// (#264).
    ///
    /// The whole result: since #365 it also brings the ones the daemon
    /// could not read, and the selector shows them with no destination.
    Connections(
        u64,
        Result<norte_proto::methods::ConnectionListResult, Error>,
    ),
    /// A batch of results, with the search epoch that requested it.
    Results(u64, Box<norte_proto::methods::SearchHits>),
    /// A SEMANTIC query's answer: whole, all at once.
    Semantic(u64, Result<Vec<norte_proto::methods::SemanticHit>, Error>),
    /// The comparison has a Task: it is christened so it can be cancelled.
    ComparisonViva(u64, norte_proto::TaskId),
    /// The synchronization plan has a Task.
    PlanDeSyncVivo(u64, norte_proto::TaskId),
    /// The `sync.apply` was accepted and this is its Task.
    SyncApplying(u64, norte_proto::TaskId),
    /// The `sync.apply` failed. The `bool` says whether it is KNOWN that it
    /// wrote nothing: a rejection (policy, conflict, invalid path) knows,
    /// because the daemon answered; a dropped transport does NOT, because
    /// the request may have arrived and be running right now. Releasing the
    /// latch in the second case invites applying the same plan twice over
    /// the same destination.
    SyncNoApplied(u64, bool),
    /// A finished synchronization's report.
    SyncReport(
        u64,
        norte_proto::TaskState,
        Box<Result<norte_proto::methods::SyncReportResult, Error>>,
    ),
    /// The daemon rejected the plan: there will be no Task and no panel.
    FailedSyncPlan(u64),
    /// The bytes of the checksums file about to be checked (#311).
    ChecksumsFile(Box<VPath>, Box<Result<Vec<u8>, Error>>),
    /// The digests that Task computed, with the STATE it finished with
    /// (#311): a cancelled Task's report is partial, and comparing it would
    /// accuse files nobody got around to reading.
    ChecksumsReport(
        norte_proto::TaskId,
        norte_proto::TaskState,
        Box<Result<norte_proto::methods::FsChecksumReportResult, Error>>,
    ),
    /// A plan event: a batch of steps, or its closing.
    SyncEvent(u64, Box<norte_client::SyncPlanEvent>),
    /// A batch of compared rows.
    RowsCompared(u64, Box<norte_proto::methods::CompareRowsBatch>),
    /// What the model proposed, with the epoch of the request that asked
    /// for it.
    PlanIa(
        u64,
        Box<Result<norte_proto::methods::AiRenamePlanResult, Error>>,
    ),
    /// The ORGANIZE plan a producer proposed (phase 8), with the epoch of
    /// the request that asked for it. One single one for the model and for
    /// a plugin: both produce the same plan and the same review.
    PlanOrganize(
        u64,
        Box<Result<norte_proto::methods::AiOrganizePlanResult, Error>>,
    ),
    /// The core's verdict on that plan, with the SAME epoch: between
    /// requesting one and the other the reader may have discarded the
    /// review, and a verdict on a plan no longer on screen does not apply.
    BatchPlan(
        u64,
        Box<Result<norte_proto::methods::FsRenameBatchPlanResult, Error>>,
    ),
    /// What the plugins said about a slot's visible window.
    ///
    /// Badges and column values together: they are the same question about
    /// the same paths and travel in the same answer.
    Adornos(Box<Adornos>),
    /// An image's whole bytes, accepted by the viewer.
    Imagen(RequestToken, Result<Vec<u8>, Error>),
    /// The STYLED view a previewer gave of the viewer's file (ADR 0141), or
    /// `None` if none matched. Arrives AFTER opening: the viewer opens with
    /// the raw view as soon as it is read and this replaces it.
    Style(
        RequestToken,
        Option<norte_proto::methods::PluginPreviewStyled>,
    ),
    /// The THUMBNAIL a plugin gave of the viewer's file (ADR 0107), or
    /// `None` if none matched or the one that matched did not know how.
    Thumbnail(RequestToken, Option<norte_proto::methods::PluginThumbnail>),
    /// This epoch's search already has a Task: this is its id.
    ///
    /// Arrives on its own and not inside the first batch because there may
    /// be no first batch: the core sends no empty batches.
    SearchViva(u64, norte_proto::TaskId),
    /// This epoch's search never made it to being enqueued, and with which
    /// error.
    ///
    /// There is no Task there, so the outcome cannot arrive through
    /// progress: without this the view kept saying "searching…" forever
    /// about a search that does not exist, while the error passed through
    /// the status bar and the next key swept it away.
    SearchBroken(u64, Box<Error>),
    /// The volumes, requested by the SIDE PANEL.
    ///
    /// Separate from the selector's for the same reason as the two plugin
    /// catalogs: they are two surfaces with two lifetimes.
    PlacesVolumes(Result<Vec<norte_proto::methods::Volume>, Error>),
    /// The volumes for the listings' FOOTER (spec 2026-09-10). Separate
    /// from places' and the selector's for the same reason: a different
    /// lifetime, and it arrives with nobody having opened anything.
    FooterVolumes(Result<Vec<norte_proto::methods::Volume>, Error>),
    /// A TREE branch's subdirectories, already filtered and sorted.
    ///
    /// `None` = the branch would not let itself be read; decided by
    /// `Tree::branch_unreadable` (empty, or re-anchor if it was the root).
    TreeBranches(VPath, Option<Vec<VPath>>),
    /// A pane's session closed (#140): the slot, how it went, and where
    /// that pane goes now.
    ///
    /// The destination travels INSIDE the message because it was decided
    /// before releasing the session: afterward, the pane's path no longer
    /// works to choose it.
    Disconnected(u32, Result<bool, Error>, VPath),
}

impl UiHost {
    /// Starts the host and returns its FIRST snapshot.
    ///
    /// The initial snapshot is sequence 0 and there is exactly one: a
    /// renderer starting up does not have to ask for the state, it already
    /// has it.
    ///
    /// # Errors
    /// [`UiError::NoBrowserSlot`] if the layout declares no `browser`: with
    /// no listing there is no screen to paint.
    pub async fn start(options: UiHostOptions) -> Result<(Self, ViewSnapshot), UiError> {
        let instance = InstanceId::new(new_instance());
        let (updates, _) = broadcast::channel(UPDATE_BUFFER);
        // NATIVE effects go through their own channel: they carry paths and
        // go to the hosting process, not to the webview. A small buffer
        // because they are one person's gestures — copying a path, opening
        // a file — and not a stream: if it ever filled up, what is lost is
        // a gesture that can be repeated, not a piece of the screen.
        let (native, _) = broadcast::channel(16);
        let (tx, rx) = mpsc::channel(INBOX);
        // The actor keeps a return address to ITS OWN mailbox: that is
        // where slow answers come back through.
        let tx2 = tx.clone();

        let (mut state, backend) = State::new(instance.clone(), options);
        if state.slots.is_empty() {
            return Err(UiError::NoBrowserSlot);
        }
        // The session first: it says WHERE each slot was, and listing
        // before that would be bringing in a directory only to drop it.
        state.leer_session(backend.as_ref()).await;
        // What the session said about this window — owner or loose — goes
        // to the status bar from the first frame: the change is discarded
        // because the startup snapshot carries the whole status bar.
        let _ = state.banner_change();
        // And afterward `[profile.start]`, OUTSIDE `leer_session` on purpose:
        // that one returns early through four paths — no session, from a
        // future version, revision 0, unreadable body — and three of those
        // are exactly the case the key exists for: a fresh install, or a
        // profile copied from another machine (ADR 0098). Inside, it never
        // seeded anything.
        //
        // The order IS the precedence: the session, then what the profile
        // says about slots it does not know, and on top of that the
        // directory a human just typed.
        for (id, dest) in state.profile_seed() {
            if let Some(target_slot) = state.slots.get_mut(&id) {
                target_slot.pane.begin_loading(dest);
            }
        }
        state.pin_dir_requested();
        // The first listing is requested BEFORE publishing anything:
        // snapshot 0 describes a screen that already exists, not a promise.
        state.list_initial(&backend, &tx2).await;
        // The side panel, if the layout places one: favorites come from the
        // already-loaded configuration, and volumes are REQUESTED and
        // arrive later — asking about them mounts and queries space on every
        // filesystem, and the window does not wait for that to paint.
        if state.places_slot().is_some() {
            state.seed_places();
            state.request_places(&backend, &tx2);
        }
        // And which PANES the plugins contribute (phase 3). Without asking
        // whether there is a slot for one: the saved layout can bring one
        // and that slot does not get placed until its kind is declared. The
        // only gate is the effects one, put there by `request_panels`.
        //
        // Arrives after the first snapshot, like the volumes: declaring a
        // kind repaints, and waiting for an RPC to show the screen would be
        // paying for something that is almost never there.
        State::request_panels(&backend, &tx2);
        // And what is already visible is probed: the local listing carries
        // no size or date (#52), so without this the first screen is born
        // with two blank columns that do not fill in until something moves
        // it.
        let visible: Vec<u32> = state.slots.keys().copied().collect();
        for slot in visible {
            state.probe(slot, &backend, &tx2);
        }
        let first_one = state.snapshot();

        pump_session_tick(tx.clone());

        pump_backend_channels(backend.as_ref(), &tx, state.effects);
        let host = Self {
            inbox: tx,
            updates: updates.clone(),
            native: native.clone(),
            instance,
        };
        state.desktop.native = Some(native);
        tokio::spawn(actor(rx, state, backend, updates, tx2));
        Ok((host, first_one))
    }

    /// This instance's identity. Any action not carrying it belongs to
    /// another life of the host.
    #[must_use]
    pub fn instance(&self) -> &InstanceId {
        &self.instance
    }

    /// Sends an action and waits for its acknowledgment.
    ///
    /// # Errors
    /// [`UiError::Down`] if the host is no longer there.
    pub async fn dispatch(&self, action: UiAction) -> Result<ActionAck, UiError> {
        let (tx, rx) = oneshot::channel();
        self.inbox
            .send(Message::Action(Box::new(action), tx))
            .await
            .map_err(|_| UiError::Down)?;
        rx.await.map_err(|_| UiError::Down)
    }

    /// The bytes of the image the viewer has open, if they have arrived
    /// yet.
    ///
    /// Separate from the snapshot ON PURPOSE: eight megs in the patch flow
    /// is a message resent whole on every `Resync`. The renderer requests
    /// them through here, makes a `blob:` and revokes it on close
    /// (ADR 0069).
    ///
    /// Carries no PATH. The renderer names no files — not here, not
    /// anywhere else — so what is served is the image the host itself
    /// decided to open, not whichever one someone asks for.
    ///
    /// # Errors
    /// [`UiError::Down`] if the actor is no longer there.
    pub async fn image_bytes(&self) -> Result<Option<std::sync::Arc<Vec<u8>>>, UiError> {
        let (tx, rx) = oneshot::channel();
        self.inbox
            .send(Message::BytesDeImagen(tx))
            .await
            .map_err(|_| UiError::Down)?;
        rx.await.map_err(|_| UiError::Down)
    }

    /// Hooks into the updates. Several subscribers are legal; the writer is
    /// still one.
    #[must_use]
    pub fn subscribe(&self) -> UiSubscription {
        UiSubscription {
            rx: self.updates.subscribe(),
        }
    }

    /// The NATIVE effects the host requests: clipboard, opening with the
    /// desktop, terminal.
    ///
    /// A channel separate from the view's on purpose: this carries paths
    /// and goes to the hosting PROCESS, not to the webview, which has no
    /// permission to run anything (ADR 0066 D11). A frontend that does not
    /// subscribe simply performs none, which is what is wanted from a
    /// frontend that does not know how to perform them.
    #[must_use]
    pub fn native_effects(&self) -> broadcast::Receiver<crate::dto::NativeEffect> {
        self.native.subscribe()
    }

    /// Shuts down the host and reports what was left unfinished.
    ///
    /// # Errors
    /// [`UiError::Down`] if it was already shut down.
    pub async fn shutdown(&self) -> Result<ShutdownReport, UiError> {
        let (tx, rx) = oneshot::channel();
        self.inbox
            .send(Message::TurnOff(tx))
            .await
            .map_err(|_| UiError::Down)?;
        rx.await.map_err(|_| UiError::Down)
    }
}

/// What can fail when talking to the host.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UiError {
    /// The host shut down (or crashed): its state no longer exists.
    #[error("the host is not there")]
    Down,
    /// The layout has no listing at all.
    ///
    /// A screen with no listing is not a screen: it is a hang with borders.
    /// It is rejected HERE and not on the first key, where the panic would
    /// fall inside the actor's task — with no log and no visible crash —
    /// and would leave the window dead, answering `Down` forever (it is
    /// #242's shape on this surface).
    #[error("the layout has no listing")]
    NoBrowserSlot,
}

/// The SOLE writer's loop.
// The actor's message DISPATCH: one arm per variant, and each arm
// delegates. Long by variant count, not by logic — splitting it into two
// arbitrary halves would only hide where each message is handled.
#[expect(
    clippy::too_many_lines,
    reason = "actor message dispatch: long by variant count, not by logic"
)]
async fn actor(
    mut rx: mpsc::Receiver<Message>,
    mut state: State,
    backend: Arc<dyn HostBackend>,
    updates: broadcast::Sender<BridgeEnvelope<UiUpdate>>,
    mailbox: mpsc::Sender<Message>,
) {
    while let Some(msg) = rx.recv().await {
        match msg {
            Message::Action(action, answers) => {
                let (ack, outputs) = state.apply(&action, &backend, &mailbox);
                for u in outputs {
                    // With no subscribers it is not an error: the host is
                    // still alive even if the renderer has gone off to do
                    // something else.
                    let _ = updates.send(u);
                }
                let _ = answers.send(ack);
            }
            Message::BytesDeImagen(answers) => {
                let _ = answers.send(state.imagen.clone());
            }
            Message::Catalog(data) => {
                let (scheme, catalog) = *data;
                let _ = updates.send(state.apply_catalog(scheme, catalog));
            }
            Message::Approval(req) => {
                // And through the DESKTOP if the window is not up front
                // (#285). It is the notice that justifies the mechanism: an
                // approval expires on its own if nobody answers, so not
                // finding out changes the outcome — unlike a copy, which
                // stays finished when you come back.
                state.notify_of_approval(&req);
                for u in state.open_approval(&req, &mailbox) {
                    let _ = updates.send(u);
                }
            }
            Message::ApprovalExpired(approval_id) => {
                for u in state.expires_approval(approval_id) {
                    let _ = updates.send(u);
                }
            }
            Message::ApprovalNoDelivered(approval_id, key) => {
                // NAMES the approval (#279): with two stacked, "the approval
                // did not arrive" does not say which of the two, and they
                // are security decisions over different operands.
                for u in state.say_with(key, &[("id", &approval_id.to_string())]) {
                    let _ = updates.send(u);
                }
            }
            Message::Listing(data) => {
                for u in state.land_listing(*data, &backend, &mailbox) {
                    let _ = updates.send(u);
                }
            }
            Message::LogTic(epoch) => {
                for u in state.log_tick(epoch, &backend, &mailbox) {
                    let _ = updates.send(u);
                }
            }
            Message::LogRemote(epoch, res) => {
                for u in state.land_log_remote(epoch, *res) {
                    let _ = updates.send(u);
                }
            }
            Message::LogLevel(epoch, res) => {
                for u in state.land_level_remote(epoch, *res) {
                    let _ = updates.send(u);
                }
            }
            Message::SecretDelivered(data) => {
                let (slot, dir, res) = *data;
                for u in state.secret_delivered(slot, &dir, res, &backend, &mailbox) {
                    let _ = updates.send(u);
                }
            }
            Message::Content(data) => {
                let (token, path, read, preview) = *data;
                if let Some(u) = state.open_visor(token, path, read, preview, &backend, &mailbox) {
                    let _ = updates.send(u);
                }
            }
            Message::PreviewContent(data) => {
                let (slot, (token, path, read, preview)) = *data;
                if let Some(u) = state.land_preview(slot, token, path, read, preview) {
                    let _ = updates.send(u);
                }
            }
            Message::PanelContent(data) => {
                let (slot, token, res) = *data;
                if let Some(u) = state.land_panel(slot, token, res) {
                    let _ = updates.send(u);
                }
            }
            Message::MapContent(data) => {
                let (slot, token, res) = *data;
                if let Some(u) = state.land_map(slot, token, res) {
                    let _ = updates.send(u);
                }
            }
            Message::Background(f) => {
                for u in state.apply_in_background(*f, &backend, &mailbox) {
                    let _ = updates.send(u);
                }
            }
            Message::Hydrated(data) => {
                if let Some(u) = state.land_probes(*data, &backend, &mailbox) {
                    let _ = updates.send(u);
                }
            }
            Message::MoreEntries(data) => {
                for u in state.land_batch(*data, &backend, &mailbox) {
                    let _ = updates.send(u);
                }
            }
            Message::Connection(ev) => {
                for u in state.connection_change(ev, &backend, &mailbox) {
                    let _ = updates.send(u);
                }
            }
            Message::Degraded(d) => {
                let _ = updates.send(state.session_degraded(*d));
            }
            Message::Failed(f) => {
                for u in state.connection_failed(&f) {
                    let _ = updates.send(u);
                }
            }
            Message::NoticePlugin(n) => {
                for u in state.plugin_notice(&n) {
                    let _ = updates.send(u);
                }
            }
            Message::TaskNew(task) => {
                let (task, affected, retry) = *task;
                for u in state.registrar_task(task, affected, retry, &backend, &mailbox) {
                    let _ = updates.send(u);
                }
            }
            Message::Capabilities(slot, dir, caps) => {
                if let Some(u) = state.apply_capabilities(slot, &dir, caps) {
                    let _ = updates.send(u);
                }
            }
            Message::TaskFailed(e) => {
                for u in state.task_failed(&e) {
                    let _ = updates.send(u);
                }
            }
            Message::BatchTaskRejected(e) => {
                for u in state.batch_rejection(&e) {
                    let _ = updates.send(u);
                }
            }
            Message::Progress(p) => {
                for u in state.progress(&p, &backend, &mailbox) {
                    let _ = updates.send(u);
                }
            }
            Message::TaskExpired(id, epoch) => {
                for u in state.expire_task(id, epoch, &backend, &mailbox) {
                    let _ = updates.send(u);
                }
            }
            Message::Say(key) => {
                for u in state.say(key) {
                    let _ = updates.send(u);
                }
            }
            Message::Strip => {
                for u in state.wake_strip(&backend, &mailbox) {
                    let _ = updates.send(u);
                }
            }
            Message::ThemePersisted(failure) | Message::WidthPersisted(failure) => {
                if let Some(key) = failure {
                    for u in state.say(key) {
                        let _ = updates.send(u);
                    }
                }
            }
            Message::TerminalTic(epoch) => {
                for u in state.terminal_tic(epoch, &mailbox) {
                    let _ = updates.send(u);
                }
            }
            Message::SessionTic => {
                state.push_session(&backend, &mailbox);
                // And one more second for the status bar's notice (spec
                // 2026-09-10).
                if let Some(u) = state.expire_notice() {
                    let _ = updates.send(u);
                }
            }
            Message::SessionPlaced(data) => {
                let (res, body) = *data;
                for u in state.session_placed(res, body, &backend, &mailbox) {
                    let _ = updates.send(u);
                }
            }
            Message::HandedOff { released: soltada } => {
                for u in state.handoff_finished(soltada) {
                    let _ = updates.send(u);
                }
            }
            Message::SessionReread(res) => {
                for u in state.session_reread(res) {
                    let _ = updates.send(u);
                }
            }
            Message::ThemeResolved(data) => {
                let (spec, result) = *data;
                match result {
                    Ok(theme) => {
                        state.theme_placed(&spec, &theme);
                        // Snapshot and not a patch: changing theme moves the
                        // colors of the WHOLE screen, and the renderer plugs
                        // them back in from the catalog, not from a view
                        // field.
                        let snap = state.snapshot();
                        let _ = updates.send(state.over(UiUpdate::Snapshot(Box::new(snap))));
                    }
                    // A theme that cannot be read does NOT leave the window
                    // with no colors: it stays on the one there was and
                    // reports why.
                    Err(key) => {
                        for u in state.say(key) {
                            let _ = updates.send(u);
                        }
                    }
                }
            }
            Message::Report(report) => {
                let (epoch, task_id, which) = *report;
                for u in state.report(epoch, task_id, &which) {
                    let _ = updates.send(u);
                }
            }
            Message::CreatedChecked(checked) => {
                let (path, verdict) = *checked;
                for u in state.open_the_checked(path, verdict) {
                    let _ = updates.send(u);
                }
            }
            Message::FavoritePersisted(done) => {
                let (name, dest, failure) = *done;
                for u in state.favorite_persisted(&name, Some(dest), failure) {
                    let _ = updates.send(u);
                }
            }
            Message::FavoriteRemoved(done) => {
                let (name, failure) = *done;
                for u in state.favorite_persisted(&name, None, failure) {
                    let _ = updates.send(u);
                }
            }
            Message::ProfileSaved(done) => {
                let (name, failure) = *done;
                for u in state.profile_saved(&name, failure) {
                    let _ = updates.send(u);
                }
            }
            Message::TurnOff(answers) => {
                let report = state.turn_off(backend.as_ref()).await;
                let _ = updates.send(state.over(UiUpdate::Notice(UiNotice::Shutdown {
                    incomplete: report.incomplete,
                })));
                let _ = answers.send(report);
                return;
            }
        }
        // The docked viewer follows the cursor (#291), and the cursor is
        // moved by any message: a key, a listing landing, a panel opening.
        // It is asked AFTER each one, the way the TUI asks it every frame:
        // what each placed preview slot should be showing, and if it is not
        // what it shows, it is requested.
        for u in state.probe_previews(&backend, &mailbox) {
            let _ = updates.send(u);
        }
        // And a plugin's pane (phase 3), for the same reason and in the same
        // place: its guest receives the directory and the row under the
        // cursor, so any message can change what it should be showing.
        for u in state.probe_panels(&backend, &mailbox) {
            let _ = updates.send(u);
        }
        // And the disk map (phase 4), in the same place and for the same
        // reason: it follows the DIRECTORY of the listing it is tied to, so
        // a `cd` — wherever it comes from — changes what it should be
        // showing. It does not follow the cursor: moving a row does not
        // change what the directory is made of, and probing per cursor
        // would mean measuring a `$HOME` on every arrow.
        for u in state.probe_maps(&backend, &mailbox) {
            let _ = updates.send(u);
        }
        // And the timeline (#359): the first page when its slot appears,
        // and the next one when the cursor reaches the bottom.
        state.probe_lines(&backend, &mailbox);
        // And the attribute sheet, for the SAME reason and in the same
        // place: it also follows the cursor and also has no other path to
        // the renderer. It goes after the viewer so that, when both change
        // at once, the snapshot sent already carries both up to date.
        for u in state.probe_leaves() {
            let _ = updates.send(u);
        }
    }
}

/// The area the renderer says it has, in layout cells.
///
/// The tree is laid out on a grid and not in pixels on purpose: each kind's
/// minimums are declared that way and shared with the TUI, which is what
/// makes "this pane does not fit" mean the same thing on both surfaces.
/// A column's id: an IDENTITY, whole or empty.
///
/// NOT masked and NOT trimmed, unlike everything else. It is what comes back
/// to sort by and what names each cell's column, so both transformations
/// break it, and in different ways:
///
/// - Masking is not injective. A `norte.toml` with two `attr:` columns
///   differing only by an invisible character gave TWO headers with the
///   SAME masked id, and resolution does a `find`: pressing the second one
///   sorted by the first. It is ADR 0061's rule — "trimming is not
///   injective, and this is a key" — applied to masking, on a surface the
///   ADR did not cover.
/// - Trimming was also ASYMMETRIC: the id came out with `clamp_display` and
///   was compared without it, so a long one never matched its own column and
///   fell back to a `parse` over a string ending in `…`.
///
/// An id that does not fit the bridge's cap is sent EMPTY: a key that
/// matches nothing is a visible failure; one that matches the wrong one is
/// not. What gets PAINTED is `label`, which is masked, and the renderer only
/// uses the id in a `data-` and to send it back.
fn column_identity(id: &norte_frontend::columns::ColumnId) -> String {
    let s = id.to_string();
    if s.len() > crate::bridge::MAX_STRING_BYTES {
        return String::new();
    }
    s
}

fn rect((width, height): (u16, u16)) -> Rect {
    Rect {
        x: 0,
        y: 0,
        width,
        height,
    }
}

/// Is this tree slot a listing?
fn es_listing(tree: &Node, slot: SlotId, kinds: &KindRegistry) -> bool {
    kind_de(tree, slot).is_some_and(|k| k.as_str() == "browser" && kinds.get(&k).is_some())
}

/// A tree slot's declared kind.
fn kind_de(tree: &Node, slot: SlotId) -> Option<norte_frontend::layout::KindId> {
    fn search(n: &Node, slot: SlotId) -> Option<norte_frontend::layout::KindId> {
        match n {
            Node::Slot { id, kind, .. } if *id == slot => Some(kind.clone()),
            Node::Slot { .. } => None,
            Node::Split { children, .. } | Node::Tabs { children, .. } => {
                children.iter().find_map(|c| search(c, slot))
            }
        }
    }
    search(tree, slot)
}

/// Now, in milliseconds. Injected by the projector so a relative date's
/// format ("3 days ago") does not depend on when it was serialized.
/// The policy matching each collision-dialog output (#274).
///
/// `None` for `cancel` and for anything else: not choosing is a valid answer
/// — the failed task stays as it is — and an option the dialog did not offer
/// is not interpreted.
///
/// The ids are the shared `dialog.*` catalog's, the same ones the TUI binds:
/// two vocabularies for the same question would be two places where a key
/// ends up doing something else.
fn collision_policy(choice: &str) -> Option<norte_proto::CollisionPolicy> {
    use norte_proto::CollisionPolicy as P;
    match choice {
        "overwrite" => Some(P::Overwrite),
        "newer" => Some(P::Newer),
        "rename" => Some(P::RenameAuto),
        "skip" => Some(P::Skip),
        _ => None,
    }
}

/// Sends the "yes" to the daemon, and if it does not go well, COUNTS it with
/// the right phrase.
///
/// Separate because the outcome has three different ways to go wrong and
/// none of them is the business of the function that decides what to do
/// with a dialog (#279).
fn launch_approval(
    approval_id: u64,
    backend: &Arc<dyn HostBackend>,
    mailbox: &mpsc::Sender<Message>,
) {
    let backend = Arc::clone(backend);
    let mailbox = mailbox.clone();
    tokio::spawn(async move {
        if let Err(e) = backend.policy_decide(approval_id, true).await {
            let _ = mailbox
                .send(Message::ApprovalNoDelivered(
                    approval_id,
                    lost_approval_key(&e),
                ))
                .await;
        }
    });
}

/// Which phrase applies when a `policy.decide` does not go well (#279).
///
/// The three shapes of "that approval is no longer there" call for
/// different advice, and they used to all be painted as the first one:
/// "your click did not arrive" about an approval that did arrive and
/// expired is a phrase that sends the reader to retry something already
/// decided without them.
///
/// A `reason` this binary does not know falls into "unknown" and never into
/// one of the others: the wire's vocabulary can grow, and guessing on a
/// security surface is worse than saying it is not known.
fn lost_approval_key(e: &norte_proto::Error) -> &'static str {
    match e {
        norte_proto::Error::ApprovalGone { reason } => match reason.as_str() {
            "expired" => "msg-approval-expired",
            "already-decided" => "msg-approval-already-decided",
            _ => "msg-approval-unknown",
        },
        // Any other error means the request did NOT arrive (the daemon went
        // down between the question and the answer), which is the original
        // case.
        _ => "msg-approval-not-delivered",
    }
}

/// A task's CLASS, in the bridge's vocabulary.
///
/// A `match` and not `format!("{:?}").to_lowercase()`. `Debug` gave
/// `renamebatch` and `dirsize` for variants whose catalog key is
/// `rename-batch` and `dir-size`, so those two painted as their own
/// identifier.
///
/// `TaskKind` is `#[non_exhaustive]`, so the wildcard is mandatory and this
/// does NOT stop compiling when a variant appears: what happens is it falls
/// into `unknown`, a key that DOES EXIST in the catalog. A task from a newer
/// daemon reads "task" instead of reading `gui-task-kind-frobnicate`.
fn task_class(kind: norte_proto::TaskKind) -> &'static str {
    use norte_proto::TaskKind as K;
    match kind {
        K::Copy => "copy",
        K::Move => "move",
        K::Delete => "delete",
        K::Undo => "undo",
        K::Search => "search",
        K::Mkdir => "mkdir",
        K::Create => "create",
        K::Index => "index",
        K::Embed => "embed",
        K::RenameBatch => "rename-batch",
        K::Compare => "compare",
        K::DirSize => "dir-size",
        K::Pack => "pack",
        K::TestArchive => "test-archive",
        K::Split => "split",
        K::Combine => "combine",
        K::SyncPlan => "sync-plan",
        K::Sync => "sync",
        // #311 and #314: fell into `unknown`, meaning a checksum check and a
        // permission change read "task" in the strip.
        K::Checksum => "checksum",
        K::SetMode => "set-mode",
        // `Unknown` and whatever a newer daemon brings, together: see the
        // doc above. `unknown` is a real key, not a raw identifier painted
        // as is.
        K::Unknown | _ => "unknown",
    }
}

/// The scheme's CONFIGURED `plugin:` columns' values.
///
/// Membership validation and collision dedup live in the SHARED model
/// (`validated_plugin_requests`): one single definition for both frontends,
/// and a bare-id collision is left blank rather than attributing a column to
/// the wrong plugin.
///
/// Fail-soft PER COLUMN: one that fails leaves empty cells, it never turns
/// the listing into an error. And `exceeded` cuts off between RPCs, because
/// a batch the listing has already superseded has no reason to spend the
/// ones it has left.
async fn plugin_cells(
    backend: &Arc<dyn HostBackend>,
    requested: &[(String, String)],
    paths: &[VPath],
    exceeded: impl Fn() -> bool,
) -> (
    std::collections::HashMap<String, std::collections::HashMap<VPath, String>>,
    std::collections::BTreeMap<String, String>,
) {
    let mut out = std::collections::HashMap::new();
    let mut labels = std::collections::BTreeMap::new();
    if requested.is_empty() {
        return (out, labels);
    }
    let Ok(list) = backend.plugin_list().await else {
        return (out, labels);
    };
    for (plugin, column) in
        norte_frontend::columns::validated_plugin_requests(requested, &list.plugins)
    {
        // The LABEL the manifest gave it, so the header does not say the id
        // (`acme.git/status`). A plugin's text: it is masked and clamped
        // like any header. A manifest leaving it empty stays with no label
        // and the header falls back to the id, as before.
        if let Some(h) = list
            .plugins
            .iter()
            .find(|p| p.id == plugin)
            .and_then(|p| p.columns.iter().find(|c| c.id == column))
        {
            let sano: String = norte_frontend::columns::sanitize_header(&h.header)
                .chars()
                .take(norte_frontend::columns::HEADER_MAX_CHARS)
                .collect();
            if !sano.is_empty() {
                labels.insert(
                    norte_frontend::columns::plugin_display_id(&plugin, &column),
                    sano,
                );
            }
        }
        if exceeded() {
            return (out, labels);
        }
        let raw = backend
            .plugin_column_values(plugin.clone(), column.clone(), paths.to_vec())
            .await
            .unwrap_or_default();
        let sanos = norte_frontend::columns::sanitize_column_values(paths, &raw);
        out.insert(
            norte_frontend::columns::plugin_display_id(&plugin, &column),
            sanos,
        );
    }
    (out, labels)
}

/// What a column is called in the selector, and whether that differs from
/// reality.
///
/// Through `header_label`, the SAME function that paints the listing's
/// header: what a column is called cannot depend on where it is read from.
/// An id that does not parse is shown as is — it is the user's configuration
/// intent and the selector never cleans it up — and that is why it is masked
/// too.
fn column_label(
    r: &norte_frontend::columns_picker::PickerRow,
    scheme: &str,
    columns: &norte_frontend::columns::ColumnsSettings,
    lang: norte_i18n::Lang,
) -> (String, bool) {
    use norte_frontend::columns::{ColumnId, header_label_in};
    let Ok(cid) = r.id.parse::<ColumnId>() else {
        // Does not parse: the raw id is all that can be shown, and it is
        // text from a configuration file.
        return norte_frontend::display_name(r.id.as_bytes());
    };
    let style = columns.style_for_id(scheme, &cid, None);
    norte_frontend::display_name(header_label_in(&cid, &style, None, lang).as_bytes())
}

/// A text IDENTITY that crosses the bridge: whole, or empty.
///
/// Same rule as [`column_identity`] and for the same reason (ADR 0061):
/// trimming is not injective, and a trimmed key matches the wrong one.
fn text_identity(id: &str) -> String {
    if id.len() > crate::bridge::MAX_STRING_BYTES {
        return String::new();
    }
    id.to_owned()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// An instance's unique identity: pid plus the startup instant. It does not
/// need to be unpredictable — it authorizes nothing — only different from
/// the process's previous life.
fn new_instance() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("host-{}-{now}", std::process::id())
}

/// A listing open in a slot.
///
/// The listing's state does NOT belong to this crate: it is
/// [`norte_frontend::pane::PaneState`], the same one the TUI uses. Cursor,
/// marks, hidden ones, per-directory cursor memory and the listing's EPOCH
/// come from there, so the two surfaces cannot diverge on what "move the
/// cursor down" means (ADR 0066, D14).
struct Slot {
    pane: PaneState,
    /// How the directory this slot is in FOLDS names (#268).
    ///
    /// By LOCATION and not by scheme (#215): a FAT thumb drive mounted under
    /// the same `file://` as a case-sensitive `/home` gives a different
    /// answer, and answering by the provider would be answering for the
    /// wrong place.
    ///
    /// Requested on LANDING and not in front of every dialog: asking at copy
    /// time would put a daemon round trip on F5's path, the most-pressed
    /// key. `None` = it has not arrived yet, and then nothing folds — the
    /// check is a courtesy and the core is the authority.
    ///
    /// Stored WHOLE and not already distilled into a `FoldMode`. The answer
    /// cost a round trip to the daemon and carries more than one thing this
    /// frontend needs: the destination's folding and, since help's leveling,
    /// the `READ_ONLY` used to dim what this location is not going to
    /// accept. Keeping only the first is what left the window's help
    /// declaring you can write anywhere.
    ///
    /// And they are TIED to the path they were asked about, instead of being
    /// cleared every time others are requested. What invalidates them is
    /// changing DIRECTORY, not re-listing the same one: clearing them on
    /// request left a deterministic window — landing re-freezes help's facts
    /// three lines after requesting them — where the slot said "unknown"
    /// about a place that had already answered. Matching by path also
    /// renders a late answer harmless: if it is from a different directory,
    /// it is not read.
    ///
    /// `None`, or a path that does not match, means "still unknown", and
    /// then folding does not apply and read-only is answered by the scheme
    /// (`norte_frontend::availability::read_only`).
    caps: Option<(VPath, norte_proto::Capabilities)>,
    /// The scheme whose order `pane` currently has set (#108).
    ///
    /// `[ui.columns] sort` can give an order PER SCHEME, and it is
    /// reapplied when the slot lands on a different scheme — not on every
    /// `cd`, which is what the TUI does: here the session RESTORES the
    /// order, and reapplying it on the first landing would erase it before
    /// it is seen.
    order_scheme: String,
    /// Where I come from and where I go back to. Also shared.
    history: History,
    first_visible: u64,
    visible: u32,
    /// The listing request IN FLIGHT, if any. An answer with a different
    /// token arrived late: it is discarded here, in Rust, not hidden in the
    /// renderer.
    in_flight: Option<RequestToken>,
    /// WHERE the in-flight request is going, if any.
    ///
    /// Not the same as `pane.dir()`, and confusing them was a bug: `dir()`
    /// only changes when the listing LANDS, so during a navigation the slot
    /// "is" still in the directory it is leaving. A refresh going by `dir()`
    /// would re-list the old one and overwrite the navigation's token, which
    /// would be silently discarded; and a slot ENTERING the directory a
    /// mutation just changed would not be recognized as affected, and would
    /// land on a listing from before the mutation with nothing to correct
    /// it.
    dir_requested: Option<VPath>,
    /// The marks that need to be set again when a REFRESH lands.
    ///
    /// Empty whenever what is in flight is a navigation: there the rows
    /// belong to another directory and a mark means nothing. Consumed on
    /// landing.
    marks_to_restore: Vec<VPath>,
    /// The row the SESSION left under the cursor, until its listing arrives.
    ///
    /// Waits for the same reason as the marks: over an empty pane, putting
    /// the cursor on row 12 is putting it on row 0. Consumed on the first
    /// landing — good or bad — so it never falls on a later listing from
    /// somewhere else. It is an INDEX, the same one the terminal saves and
    /// restores: a handoff has to land on the same row in both directions.
    cursor_to_restore: Option<usize>,
    /// There are merged rows not yet published.
    ///
    /// Filling stays quiet when the merging batch does not change the
    /// visible window (#252), but every merge bumps the listing's EPOCH and
    /// the renderer names rows by epoch: if ALL patches stay quiet, it is
    /// left on a stale epoch and its clicks are rejected as outdated. This
    /// flag is the debt, and the last batch settles it.
    rows_to_publish: bool,
    /// The drain still bringing batches in from behind, if any.
    ///
    /// SEPARATE from `in_flight` because they are two different lifetimes:
    /// the first page lands and clears `in_flight`, but the rest of the
    /// stream keeps arriving. Sharing a token made `apply_batch` reject ALL
    /// of a navigation's batches — a five-thousand-entry directory got
    /// stuck at a hundred — and startup had to restore it by hand to work.
    draining: Option<RequestToken>,
    state: SlotState,
    /// There is a probe batch in flight for this slot.
    probing: bool,
    /// Raised when the listing changes: whatever comes back from the batch
    /// in flight no longer describes this screen.
    cancel_probe: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The paths already probed (whether they answered or not). Without
    /// this memory, a failing `stat` gets requested again on every repaint
    /// and probing turns into a loop against the daemon.
    probed: std::collections::HashSet<VPath>,
    /// The decoration plugins put on each path.
    ///
    /// By PATH and not by index: decorations arrive asynchronously and the
    /// listing reorders underneath, so an index would name a different row
    /// by the time they land.
    adornos: std::collections::HashMap<VPath, norte_frontend::Decoration>,
    /// The values of each `plugin:` column, by column id and path.
    cells_plugin: std::collections::HashMap<String, std::collections::HashMap<VPath, String>>,
    /// There is a decoration batch in flight for this slot.
    decorating: bool,
    /// The directory a navigation that COUNTS as a step is going to, until
    /// its listing arrives (spec 2026-09-15 D6): it is then added to the
    /// popular ones, and forgotten if it fails. The slot's next navigation
    /// and the landing itself replace it.
    visita_pending: Option<VPath>,
    /// The paths already requested for decoration (whether they answered or
    /// not). Same memory as `probed` and for the same reason: without it,
    /// a plugin that decorates nothing gets asked again on every repaint.
    decorated: std::collections::HashSet<VPath>,
    /// The decorations' generation: bumps every time they are FORGOTTEN. A
    /// batch in flight carries its own, and if it lands with another one it
    /// is dropped and requested again: turning off a decorator from the
    /// manager with a batch half-way left its badges stuck to the rows.
    gen_adornos: u64,
}

/// An open dialog and what it will do if confirmed.
struct Dialog {
    id: ModalId,
    vista: DialogView,
    /// What the confirmation runs. `None` = it only informs.
    on_confirm: Option<Pending>,
    /// What the user typed, AS IS.
    ///
    /// Separate from `vista.input`, which is its projection for painting —
    /// masked and clipped —, because this text ends up as a FILE NAME.
    /// Passing it through the screen's clipping created directories with an
    /// ellipsis inside: the same mistake ADR 0061 decided not to repeat, in
    /// miniature.
    ///
    /// An enum and not a `String` since #327: there is a dialog that asks for
    /// a PASSWORD, and storing it here as plain text would send it to a
    /// `Debug`, to the heap unredacted, and — worst of all — to the paint
    /// projection through the same path as a file name. With two variants,
    /// whoever writes to it has to say which one it is.
    typed: Typed,
    /// This dialog OPENED BY ITSELF, and has not been acknowledged yet.
    ///
    /// An approval and a batch's report appear without anyone finishing a
    /// press: they arrive when the daemon answers, on top of whatever the
    /// reader was doing, and they take over the input. With this, the first
    /// RESPONSE only says "I see it" — the same rule as a plan review, and
    /// for the same reason.
    ///
    /// A response, not a key: the check lives in `responder_dialog`, which
    /// is where both inputs pass through. When it only covered the keyboard,
    /// a click already in flight on a confirmation's "Confirm" landed on the
    /// "Approve" of an agent approval that had just painted itself in the
    /// same spot.
    ///
    /// Deny and cancel are exempt, same as `Escape`: getting rid of something
    /// you did not ask for has to work on the first try.
    ///
    /// `true` on a dialog that a gesture opened: there, the next response IS
    /// a response, because the question was asked by whoever is in front of
    /// it.
    recognized: bool,
}
/// What was typed into a dialog's field, according to what it is.
///
/// Two variants and not a `String` because the treatment is DIFFERENT and the
/// difference cannot be left to the caller's judgment: a file name is painted
/// masked, a password is painted as dots and never painted at all. With a
/// single type, the only safeguard was remembering — and the failure mode was
/// a password leaking through the same `display_name` as a name, or inside a
/// `Debug` of the whole state.
#[derive(Debug)]
enum Typed {
    /// A name, an instruction, a template: text that is shown.
    Text(String),
    /// A password, and the host does NOT have it while it is being typed.
    ///
    /// No data inside, and that is the decision: the field is masked by the
    /// renderer's `input type=password`, so there is nothing here to hold or
    /// paint, and the password crosses over exactly once — with the response,
    /// in `UiAction::Dialog::secret` — the instant the reader decides to hand
    /// it over. What the host does not have cannot leak through a `Debug`, a
    /// screenshot, or a log.
    ///
    /// The variant exists anyway because it is the TYPE barrier: `text()`
    /// returns nothing about it, so a text pending action that landed on this
    /// dialog by mistake cannot read a secret — there is none to read.
    Secret,
    /// A FORM: several fields at once (bridge 91).
    ///
    /// The model is the SHARED one (`norte_frontend::search::SearchForm`),
    /// not one of this window's own: the terminal asks the same search, and
    /// two models diverging silently is what already happened with a
    /// search's outcome (ADR 0077).
    ///
    /// `Box` because it is twice as big as the other two variants combined
    /// and there is one `Typed` per open dialog.
    Form(Box<norte_frontend::search::SearchForm>),
}

/// The cap on a password, from the SHARED crate: what gets rejected here is
/// exactly what that one can store without reallocating.
use norte_frontend::secret::SECRET_MAX_CHARS;

impl Typed {
    /// The text, for the pending actions that work with text.
    ///
    /// Empty for a secret, on purpose: if a text pending action ever landed
    /// on a password dialog, what it receives is nothing. A `panic!` would be
    /// worse — bringing the window down over a wiring bug — and there is
    /// nothing more to return here.
    fn text(&self) -> &str {
        match self {
            Self::Text(s) => s,
            // A form does not have "the" text: it has seven fields, and a
            // text pending action that landed here by mistake cannot walk
            // off with just any one of them by passing it off as the one it
            // asked for.
            Self::Secret | Self::Form(_) => "",
        }
    }
}

/// What a dialog has pending to do.
enum Pending {
    /// Delete these entries, to the trash or permanently.
    Delete {
        /// What gets deleted, in listing order.
        paths: Vec<VPath>,
        /// Permanent (no trash): the dialog WARNS about it.
        permanent: bool,
    },
    /// Copy to the pane what was DROPPED from the desktop (#283).
    ///
    /// Separate from [`Pending::Transferir`] for two reasons, and neither
    /// is cosmetic: here the sources do not come out of any pane — so there
    /// are no marks to consume, and consuming them would erase a selection
    /// the reader made for something else —, and the verb is always COPY:
    /// moving what another application dragged would mean deleting it from
    /// wherever that process keeps it, and this window has not asked about
    /// that.
    Release {
        /// What arrived, already converted and filtered.
        paths: Vec<VPath>,
        /// Where it falls, which is the active pane's directory when it was
        /// dropped.
        dest: VPath,
    },
    /// Ask the index by MEANING. What is typed is the query, and it carries
    /// no other operands: the scope is the whole index.
    QuerySemantic,
    /// CLOSE the window, already confirmed (`[ui] confirm_quit`).
    Exit,
    /// Hand over a connection's secret and RETRY the navigation that
    /// `Error::SecretNeeded` interrupted (#325/#327).
    ///
    /// Carries where the pane was going because this pending action is the
    /// only place in this window where a navigation survives the response
    /// that interrupted it: the listing already came back with an error, and
    /// the slot was left showing the directory it was leaving. Without the
    /// destination here, handing over the secret would leave the reader with
    /// the password given and the pane where it was.
    DeliverSecret {
        /// Name of the `connections.toml` entry asking for it — the SAME
        /// string that goes in `connection.provide_secret`. It comes from
        /// the core's error, not from the remote server.
        conn: String,
        /// The slot that was navigating.
        slot: u32,
        /// Where to retry to.
        dir: VPath,
    },
    /// Mark — or unmark — by pattern. What is typed is the glob.
    Patron {
        /// `true` adds marks, `false` removes them.
        mark: bool,
    },
    /// Retry a transfer that COLLIDED, with another policy (#274).
    ///
    /// The question is not whether to continue: it is WHICH of the four
    /// outcomes, so the policy comes from the `choice` the reader pressed and
    /// not from here. Cancel means choosing none, and then the failed task
    /// stays as it was — which is what always happened before this.
    Retry {
        /// What to relaunch with.
        con: Retry,
    },
    /// Split the file under the cursor into chunks of the size that is typed
    /// (#132).
    Split {
        /// What gets split.
        path: VPath,
        /// Where the chunks fall. The DESTINATION pane, as with a copy:
        /// splitting a gigabyte file where it already is usually does not
        /// fit.
        dest_dir: VPath,
    },
    /// Pack what is MARKED into the container that is typed (#132).
    ///
    /// Carries the directory and not the name: the name is what the reader
    /// types, and the format comes from it. It is resolved on confirm, not on
    /// open, because until then there is nothing to resolve.
    Pack {
        /// Where the container falls and, also, the BASE of the names stored
        /// inside: whoever unpacks it expects to see what it saw on screen,
        /// not absolute paths.
        dir: VPath,
        /// What goes in, in listing order.
        sources: Vec<VPath>,
    },
    /// Copy to the clipboard the list of checksums the dialog shows (#311).
    ///
    /// The BYTES already assembled — with coreutils escaping — and not the
    /// rows: what is painted is sanitized, and copying that would give a
    /// `SHA256SUMS` that does not check the files it names.
    CopyChecksums {
        /// What goes to the clipboard, as is.
        bytes: Vec<u8>,
    },
    /// Change the PERMISSIONS of these entries to the mode that is typed
    /// (#314).
    ///
    /// The paths freeze on OPENING the dialog, like the rest of the ones that
    /// carry operands: between the question and the yes the listing can
    /// refresh, and then "what is marked" would be something else.
    Permissions {
        /// On what, in listing order.
        targets: Vec<VPath>,
    },
    /// Undo EVERYTHING an agent session did (#276).
    UndoSession {
        /// The OPAQUE key the core resolves it with, raw.
        session: String,
    },
    /// Undo the human's actions AFTER a point in the timeline (#359,
    /// `journal.undo_after`). The marked row stays.
    UndoUntil {
        /// The cut: the marked row's newest `seq`.
        seq: i64,
        /// The ceiling: the newest thing the count reported (`upto_seq`). It
        /// freezes on ASKING, like the operands of any dialog.
        techo: Option<i64>,
    },
    /// Grant an extension's capabilities.
    ///
    /// It is the only one of the manager's four operations that ASKS: revoke,
    /// turn on and turn off go in the safe direction and need no second
    /// gesture. The question lists the capabilities one per line — outside
    /// the sentence, like any operand in this host — because "approve
    /// org.ejemplo.foo" without saying what it grants is not a decision.
    ApproveExtension {
        /// Who they are granted to.
        id: String,
        /// WHAT was shown when asking, in the order it was shown.
        ///
        /// Kept to check it again on confirm: the dialog holds onto the
        /// KEYSTROKES, not the background messages, so a catalogue that
        /// lands between the question and the yes may have changed that
        /// extension's capabilities — and then the yes would grant something
        /// nobody read. If they changed, it asks again.
        capabilities: Vec<String>,
        /// The manifest's anchor EXACTLY AS IT WAS WHEN ASKED (#282).
        ///
        /// Here, and not re-read on confirm, for the same reason as the
        /// capabilities above: reading it at the moment of the yes would
        /// return the anchor of whatever catalogue landed in the meantime,
        /// meaning the host would be certifying to the core "this is what the
        /// human read" about something the human did not read. And comparing
        /// capabilities does not cover it: `category` and `contributions` —
        /// when and how it fires — go into the anchor and NOT into the list
        /// that is painted.
        digest: Option<String>,
    },
    /// Uninstall an extension (ADR 0104): delete its files and withdraw its
    /// consent. It asks because it has no way back — there is no
    /// `plugin.install` over the wire — and because a plugin installed later
    /// under the same id is born without the approval this one had.
    UninstallExtension {
        /// Which one.
        id: String,
    },
    /// Decide on an agent op. The daemon holds the actual op tied to the id:
    /// only the yes or no travels here.
    Decide {
        /// The id the daemon expects back.
        approval_id: u64,
        /// The agent session that asked for it, RAW, if the request carried
        /// it.
        ///
        /// Raw and not the dialog's: what the dialog paints is masked, and
        /// masking is not injective — using that as a key would point the yes
        /// at another session's row, or at none.
        session: Option<String>,
    },
    /// Search the subtree of this directory. What is typed is the pattern.
    Search {
        /// Where the walk starts.
        root: VPath,
    },
    /// Create an EMPTY file and open it with the desktop (#290).
    CreateFile {
        /// Where it is created. The name is what is typed.
        dir: VPath,
    },
    /// Save a BOOKMARK that points here (#309). The name is what is typed,
    /// and comes prefilled with the shared suggestion.
    ///
    /// Carries the destination and does not read it on confirm: between
    /// opening the dialog and accepting, the pane may have navigated, and
    /// saving "where I am now" would make a bookmark that points somewhere
    /// other than what was being looked at when it was requested.
    SaveFavorite {
        /// Where the bookmark points to.
        dest: VPath,
    },
    /// Save the workspace as a profile (#318, ADR 0079).
    ///
    /// Carries nothing: what gets saved is what is SEEN, and that is read on
    /// confirm. The difference from the bookmark above is not an oversight —
    /// there the destination is an answer to "what were you looking at?", and
    /// here the question is "how is the screen set up?", which only makes
    /// sense NOW.
    SaveProfile,
    /// The value of a TEXT entry in the settings (F11). What is typed is the
    /// value; `id` is which entry was asked about.
    ///
    /// An ID and not a row: the cursor can move with the dialog in front, and
    /// the search behind it can change which rows there are — a position
    /// stops naming the same entry, and confirming would write the value into
    /// another one.
    EditSetting {
        /// The catalogue id that was asked about.
        id: &'static str,
    },
    /// Create a directory inside this other one. The user types the name and
    /// it is validated on confirm, not on typing: correcting a name halfway
    /// is worse than seeing it rejected at the end.
    CreateDirectory {
        /// Where it is created.
        dir: VPath,
    },
    /// Ask a model for a renaming plan for this directory. What is typed is
    /// the INSTRUCTION, not a name: nothing mutates yet.
    InstructionIa {
        /// The directory to plan over.
        dir: VPath,
    },
    /// The batch rename's TEMPLATE (#310). What is typed is a template, not a
    /// name: the plan is generated here and reviewed before anything, like
    /// the AI one.
    TemplateBatch {
        /// The directory to plan over.
        dir: VPath,
        /// The names the batch acts on: what is marked, or the cursor's.
        /// Fixed on opening the prompt, like the operand of any other
        /// operation.
        names: Vec<String>,
    },
    /// Rename ONE entry inside its own directory.
    ///
    /// Carries the field's SEED, not just the path, and that piece is what
    /// makes rule 1 hold here: if what is confirmed is EXACTLY what was
    /// seeded, nothing has been touched and what travels is the same old
    /// bytes. Comparing against the seed instead of carrying a "touched"
    /// `bool` is what survives the renderer returning the whole text on every
    /// event instead of a delta.
    Rename {
        /// The entry being renamed.
        from: VPath,
        /// What was put in the field, AS IS (the name's paintable projection,
        /// which for a name that is not UTF-8 carries a U+FFFD).
        seed: String,
    },
    /// Copy or move these entries TO another slot's directory.
    ///
    /// The destination travels already resolved — the directory of the slot
    /// with the `Target` role at the moment the dialog was opened — and not
    /// as a slot id: between opening the question and answering it the
    /// reader may have navigated that pane, and then "the other one" would be
    /// a different place than the one that was shown.
    Transferir {
        /// The slot the marks came from.
        ///
        /// Travels for the same reason as `dest`, and its absence was a
        /// bug: `UiAction::FocusSlot` is NOT blocked while a dialog is open —
        /// only the keys are —, so a click on the other pane between the
        /// question and the response made the marks consumed be another
        /// slot's. The real one stayed marked, and the reader pressed F5
        /// again on the same thing.
        source: u32,
        /// The DIRECTORY they came from, exactly as the slot writes it.
        ///
        /// Not derived from each entry's parent: the parent is written by the
        /// PROVIDER and the slot's directory can come from the config or from
        /// the session, so on macOS (NFD against NFC) or on a server with no
        /// case distinction they are two different strings for the same
        /// place — and a byte-by-byte comparison refresh would not find the
        /// source pane (ADR 0061).
        source_dir: VPath,
        /// What is transferred, in listing order.
        paths: Vec<VPath>,
        /// The DIRECTORY they go to. The final name is composed here, never
        /// in the renderer.
        dest: VPath,
        /// `true` = move.
        mover: bool,
    },
}

/// Cap on what is read from a checksums file (#311): 1 MiB.
///
/// Above it, it is REJECTED instead of checking half the list — the same
/// criterion as the terminal, and the same as the cap at the other end.
const SUMS_MAX_BYTES: u64 = 1024 * 1024;

/// What a transfer that COLLIDED can be retried with (#274).
///
/// The window always sends `CollisionPolicy::Fail`, which is the safe
/// default: overwriting or renaming are the reader's decisions. What was
/// missing was where to make them — a failed task and no way forward — and
/// offering them requires remembering WHAT was requested: the task's progress
/// says which file is currently going through, not what the source or
/// destination were.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retry {
    /// The source, exactly as requested.
    from: VPath,
    /// The EXACT destination, with its name already composed.
    to: VPath,
    /// Move instead of copy: the retry has to repeat the same verb, or an
    /// "overwrite" over a copy would turn into a move.
    mover: bool,
    /// The name reinterpretation that was in effect WHEN LAUNCHED.
    ///
    /// Captured here and not read on arrival, and that is the point: the
    /// collision arrives ASYNCHRONOUSLY, on top of whatever the reader is
    /// doing, and between the send and the question there is room to switch
    /// slots or cycle the encoding. The dialog has to paint the SAME text the
    /// navigation was done with, or it is approving a different name than the
    /// one that was seen. The terminal has carried this in its `RetrySpec`
    /// since #98 and says it there in these words.
    enc: Option<norte_encoding::NameEncoding>,
}

/// The Fluent key for a LOCAL io error.
///
/// The KEY and not the text: the host localizes with ITS OWN language
/// (`norte_i18n::t_in`), not the process's. And never the system's `Display`,
/// which the OS translates on a whim — "Permission denied (os error 13)" is
/// not a norte message (#73).
fn io_key(e: &std::io::Error) -> &'static str {
    match e.kind() {
        std::io::ErrorKind::NotFound => "err-not-found",
        std::io::ErrorKind::PermissionDenied => "err-permission-denied",
        std::io::ErrorKind::StorageFull => "err-no-space",
        _ => "err-io",
    }
}

/// The semantic state. Only the actor touches it.
// Four flags INDEPENDENT from each other: whether the window has focus,
// whether the desktop asks for dark, whether the journal refused... They are
// states of different things that coexist, not the values of a single
// machine, which is what the lint proposes and would be false here — folding
// them into two-variant enums would give four enums, not one.
#[expect(
    clippy::struct_excessive_bools,
    reason = "controller state: flags for different things, not a single machine"
)]
struct State {
    instance: InstanceId,
    sequence: u64,
    /// Request counter. Every listing carries its own, and a response with an
    /// old token is discarded.
    token: u64,
    locale: String,
    /// The key resolver, with ITS effective keymap inside (same type and same
    /// contract as the TUI's).
    resolver: Resolver,
    /// The VIEWER's effective keymap, so the palette can say a command's
    /// shortcut for that screen.
    effective_visor: Effective,
    /// The command palette, if it is open.
    ///
    /// It is one more input context, like the incremental search and the
    /// viewer: while it is open, text keys belong to it.
    palette: Option<norte_frontend::palette_state::Palette>,
    /// "Go to anywhere", if it is open (#357). Another free-text input
    /// context, like the palette.
    ir_a: Option<norte_frontend::goto::Goto>,
    /// How many times "go to" has been opened: connections and the index that
    /// answer an earlier opening are dropped.
    gen_ir_a: u64,
    /// The question to the index in flight, if any. Every key ABORTS it and
    /// launches another: typing fast does not leave three questions alive
    /// against a provider that costs time and can cost money.
    go_to_index: Option<tokio::task::JoinHandle<()>>,
    /// The first-run wizard (spec 2026-09-10), while it is open. One more
    /// overlay: it keeps the keys.
    wizard: Option<norte_frontend::wizard::Wizard>,
    /// The splash screen (spec 2026-09-15, ADR 0115), while it is up. A LAYER
    /// and not an overlay with its own keys: any key or click removes it, and
    /// the wizard beats it.
    splash: Option<norte_frontend::splash::SplashView>,
    /// When it stops covering the `brief`, in epoch milliseconds. `None` = it
    /// does not expire on its own (`home`), or there is no screen up.
    splash_until_ms: Option<i64>,
    /// The splash screen has already been shown in THIS host session.
    ///
    /// The host survives the webview — a reload, a renderer that restarts —,
    /// and the renderer sends `splash_open` every time it starts up. Without
    /// this flag, reloading mid-session covered what you were looking at with
    /// a welcome screen that in `home` mode stays until you touch it.
    splash_seen: bool,
    /// The processes panel was opened by the AUTOMATIC trigger
    /// (`[ui] processes_panel`), so the automatic trigger can close it. One
    /// the reader opened stays.
    processes_auto: bool,
    /// The lightweight progress bar of the `tasks` item (ADR 0146).
    strip: norte_frontend::task_strip::TaskStrip,
    /// Transfers that get launched go to the QUEUE (ADR 0149). From the
    /// session, not from the config: it is turned on for a while of moving
    /// things and turned off afterward.
    enqueue: bool,
    /// The clock source for [`Self::strip`]: tokio's, which tests can pause
    /// and advance.
    strip_base: tokio::time::Instant,
    /// For when a wake-up is already scheduled, so as not to stack one per
    /// progress update.
    strip_wake: Option<i64>,
    /// The latest keys launched from the palette, most recent first (spec
    /// 2026-09-10). They live in the UI session, like in the terminal.
    palette_recent: Vec<String>,
    /// The session's popular directories (spec 2026-09-15 D6). They live in
    /// the UI session, like in the terminal.
    popular: norte_frontend::history::Popular,
    /// The host's volumes, cached for each listing's footer (spec
    /// 2026-09-10). Requested when a listing lands, never on a screenshot:
    /// `host.volumes` mounts and queries space on every filesystem.
    volumes_pie: Vec<norte_proto::methods::Volume>,
    /// There is a [`Self::volumes_pie`] request in flight: no second one is
    /// stacked.
    footer_in_flight: bool,
    /// Which menu was last opened. It reopens from there: always starting
    /// from the first one forces walking the whole bar on every gesture, and
    /// whoever uses two entries of the same menu pays for it every time.
    menu_last: usize,
    /// The OPEN menu, if there is one.
    ///
    /// The bar is always painted (or never, per `[ui] menu_bar`); this is
    /// only the dropdown. While it is up, the keys belong to it — same as the
    /// palette, and for the same reason: an arrow key that escaped would move
    /// the listing underneath.
    menu: Option<norte_frontend::menu::MenuState>,
    /// The configuration this window started with, to show it.
    config: norte_frontend::config::FrontendConfig,
    /// Where each thing lives.
    paths: crate::settings::HostPaths,
    /// The settings, if they are open.
    settings: Option<crate::settings::Settings>,
    /// The extensions manager, if it is open.
    extensions: Option<crate::extensions::Extensions>,
    /// Everything about AGENT sessions: what has been seen, whether the panel
    /// is open, and which undo runs on whose behalf.
    agency: Agency,

    /// What the extensions and their commands are called, already masked,
    /// for the output panel: `id → (name, command → title)`.
    ///
    /// Composed with the catalogue the PALETTE requested, not the manager's:
    /// a command is launched from the palette with the manager closed, and
    /// then there is nowhere to pull a label from. A panel that says who
    /// printed what without being able to name either one says nothing.
    labels_plugin: Labels,
    /// What this window has from the DESKTOP: where the native effects and
    /// the last extension command's output go out through.
    desktop: Desktop,
    /// The window has the desktop's focus (#285).
    ///
    /// Starts at `true` and not `false`: a renderer that does not send
    /// `WindowFocus` behaves as before — it always notifies — instead of
    /// going quiet. Missing a notification is worse than repeating one.
    focused: bool,
    /// A folder picker is open, and this says whether what was requested was
    /// MOVE (#284). `None` = none was requested.
    ///
    /// Only the verb: the operands are recomputed when the response comes
    /// back. Freezing them here would promise an operation on a listing the
    /// reader could have changed while the picker was up.
    dest_pending: Option<bool>,

    /// The theme, as startup resolved it.
    theme: crate::pickers::HostTheme,
    /// The desktop asks for a DARK scheme (`prefers-color-scheme`).
    ///
    /// The renderer says it with [`UiAction::SetColorScheme`], on startup and
    /// every time it changes. It chooses which theme variant an entry's color
    /// is resolved against (bridge 66): without this data, the window painted
    /// the chrome with the correct variant and the NAMES with the other one.
    ///
    /// Starts at `false` and not "whatever the system says" because the host
    /// has a desktop to ask: the renderer's first message corrects it, and it
    /// arrives before anything is painted.
    scheme_dark: bool,
    /// The theme is being looked at from the inside.
    /// The theme picker, if it is open.
    theme_chosen: Option<profiles::ThemeSelection>,
    /// The active PROFILE (ADR 0079), or none.
    ///
    /// `OsString` because it is a directory name: passing it through text
    /// changes which one opens (#245).
    profile_active: Option<std::ffi::OsString>,
    /// The profile picker, if it is open.
    selector_profile: Option<norte_frontend::profile_picker::ProfilePicker>,
    /// The profile list's generation: bumps every time it is re-read.
    gen_profiles: u64,
    /// Bumps every time the sidebar's set of rows changes.
    gen_places: u64,
    /// Bumps every time the picker's set of rows changes. Also serves as the
    /// OPENING id: the picker opens empty.
    gen_selector: u64,
    /// How many times the extensions manager has been opened.
    gen_extensions: u64,
    /// How many catalogues have been REQUESTED, and which was the last one
    /// APPLIED.
    ///
    /// Apart from the opening: two governance changes in a row request two
    /// catalogues with the same opening, and they can answer in any order.
    /// Without this, the old one overwrote the new one and the "approved"
    /// column was left stale with nothing left to move it again.
    gen_catalog: u64,
    /// The last catalogue applied, to discard the ones that arrive late.
    catalog_applied: u64,
    /// How many times the palette has been opened.
    gen_palette: u64,
    /// How many extension commands have been launched.
    gen_output: u64,
    /// The bytes of the image the viewer shows, if they have already arrived.
    ///
    /// They do NOT travel in the screenshot: an eight-megabyte image in the
    /// patch stream is a message resent whole on every `Resync`, and it
    /// breaks the size guarantee `payload.rs` watches over. The renderer
    /// requests them separately and makes a `blob:` out of them (ADR 0069).
    imagen: Option<std::sync::Arc<Vec<u8>>>,
    /// The thumbnail a plugin gave for the viewer's file (ADR 0107): what the
    /// projection announces as the image and whose it is, while the bytes go
    /// in `imagen`. `None` = the viewer paints its own.
    thumbnail: Option<(crate::dto::ImageView, String)>,
    /// The open search, if there is one.
    search: Option<search::Search>,
    /// How many searches this window has launched. It is the search's
    /// identity while the daemon has not said its own yet.
    epoch_search: u64,
    /// The user's layouts, already read by whoever started the host.
    layouts: Vec<norte_frontend::layout_picker::UserLayout>,
    /// The layout picker, if it is open.
    selector_layout: Option<norte_frontend::layout_picker::LayoutPicker>,
    /// The COLUMNS picker, if it is open.
    selector_columns: Option<norte_frontend::columns_picker::ColumnsPicker>,
    /// The places sidebar, if the layout places one. There is AT MOST ONE: two
    /// identical lists of disks are not a layout, they are a bug (the shared
    /// registry says so, `multi: false`).
    places: Option<norte_frontend::places::PlacesState>,
    /// What each preview slot shows (#291), by slot: which path, the viewer
    /// with what was read or the note replacing it, and what is in flight. By
    /// slot and not a single one: the registry allows several.
    previews: std::collections::BTreeMap<u32, preview::StatePreview>,
    /// What each PLUGIN panel has alive (phase 3), by slot: its last frame,
    /// its guest's opaque state, and what is in flight.
    ///
    /// The opaque state is the ONLY thing that survives between repaints —
    /// permission to read it is minted per call —, so it is pruned along with
    /// the tree: a `SlotId` gets reused, and without pruning another plugin's
    /// panel would inherit what the first one stored.
    panels: std::collections::BTreeMap<u32, panelplugin::StatePanel>,
    /// The disk map of each slot showing one (phase 4).
    ///
    /// The state is the SHARED one (`norte_frontend::diskmap`), the same one
    /// the terminal uses: which directory it describes, what has been
    /// measured, and which child is chosen. A decision written twice diverges
    /// silently (ADR 0077).
    maps: std::collections::BTreeMap<u32, diskmap::StateMap>,
    /// The timeline of each slot that has one (#359).
    lines: std::collections::BTreeMap<u32, timeline::StateLine>,
    /// The LAST thing sent for each attributes sheet, by slot.
    ///
    /// The sheet requests nothing and is computed whole from the listing, so
    /// it has no state of its own to keep — but it is still necessary to know
    /// what the renderer saw, because any message moves the cursor and the
    /// sheet only travels in the whole screenshot. Without this, it traveled
    /// for free in the screenshot the VIEWER triggered on changing notes, and
    /// in a layout with a sheet and no viewer it stayed frozen (what could be
    /// seen: clicking a row did not move "Details").
    leaves: std::collections::BTreeMap<u32, crate::dto::MetadataSlotView>,
    /// The directory tree, if the layout places one. There is AT MOST ONE,
    /// for the same reason as the places sidebar.
    branches: Option<norte_frontend::tree::Tree>,
    /// Bumps every time the set of visible branches changes.
    gen_branches: u64,
    /// The file to open as soon as it exists (#290), with the task creating
    /// it.
    ///
    /// At most one: the gesture asks for a name, and until that dialog is
    /// answered there is no other.
    open_on_create: Option<fileops::Creation>,
    /// The processes panel's cursor.
    ///
    /// The SAME type the TUI uses, with its rule inside: it is clamped on
    /// READ and not on move, because rows appear and disappear on their own —
    /// a task finishes and is swept away after ten seconds —, so a stored
    /// cursor can always have ended up out of bounds. Here it was written by
    /// hand in five places, which is the same duplicated decision ADR 0077
    /// exists to avoid.
    cursor_processes: norte_frontend::processes::Processes,
    /// The log panel's state: level, filter and follow (#326).
    ///
    /// The SAME type the TUI uses, with its rule about the two levels inside
    /// (the one that is captured and the one that is shown) and the rule that
    /// lowering the second does not stop capturing. Duplicating it here would
    /// have meant duplicating those two.
    log_panel: norte_frontend::logpanel::LogPanel,
    /// The ring the lines come from. `None` = this process did not mount it,
    /// and then the panel SAYS so instead of showing itself empty as if
    /// nothing had happened.
    log_ring: Option<norte_config::logring::LogRing>,
    /// How many log rows fit in the last frame.
    ///
    /// Set by the renderer (`LogSetVisibleRange`), like the listing's window:
    /// guessing it here is what in the TUI made every page skip two lines and
    /// the first one skip four, and what neither window showed could not be
    /// read at all.
    log_rows: usize,
    /// Bumps on every OPENING of the panel. Distinguishes this opening's
    /// timer from the previous one's: opening, closing and reopening would
    /// leave two alive on the same panel, and the old one would keep
    /// rearming itself forever.
    log_epoch: u64,
    /// The ring's entry counter the last time it was painted, so as not to
    /// send a screenshot on a poll when nothing has happened.
    log_seen: u64,
    /// The REMOTE half of the log panel: what the daemon has delivered from
    /// its ring and what is known about it (#328).
    ///
    /// Grouped and not six loose fields: they are a single matter — a source
    /// of lines with its cursor, its state and its request in flight — and
    /// loose they would turn `State` into the kind of struct that is
    /// described with a list of flags.
    log_remote: logpanel::LogRemote,
    /// The open picker, if there is one.
    selector: Option<crate::pickers::Selector>,
    /// Help, if it is open. It covers the screen and keeps the keys, like the
    /// viewer: its keys are FIXED (there is no `dialog.*` vocabulary for
    /// "filter this list" or "follow this link"), which is the same thing the
    /// TUI and the palette do.
    help: Option<crate::help::Help>,
    /// A COPY of the listing's effective keymap.
    ///
    /// The resolver keeps its own, and building the continuations panel needs
    /// the whole effective one (what follows a prefix, and what availability
    /// each continuation has). `Effective` is `Clone` and the TUI does
    /// exactly this for the same reason.
    effective: Effective,
    /// The negotiated language, for the continuations' labels.
    lang: norte_i18n::Lang,
    /// The continuations of the half-typed prefix, if there is one.
    ///
    /// Built on the TRANSITION — the key that opens the sequence and each one
    /// that deepens it — and not when projecting: `WhichKeyRows::build` costs
    /// several strings and one or two Fluent formats PER ROW, and its own
    /// rustdoc warns about what happens if it is called from painting.
    whichkey: Option<norte_frontend::whichkey::WhichKeyRows>,
    /// The viewer screen's resolver. While the viewer is open, keys pass
    /// through HERE.
    resolver_visor: Resolver,
    /// The DIALOG screen's resolver.
    resolver_dialog: Resolver,
    /// Whether this frontend can write.
    effects: crate::commands::Effects,
    /// How many lines fit in the viewer, according to the renderer.
    ///
    /// `None` while it has not said so: it falls back to the size in cells
    /// minus the chrome, which is an estimate and behaves like one.
    visor_rows: Option<usize>,
    /// Width in cells of the viewer's BODY, measured by the renderer the last
    /// time it painted it. `None` until then: the first opening uses the
    /// viewport, which goes through the chrome.
    visor_columns: Option<u32>,
    /// The token of the viewer's read in flight, if there is one.
    ///
    /// Without it, a slow read opened the viewer AFTER the user closed it or
    /// moved elsewhere — and since keys are routed by "there is a viewer",
    /// the next key was interpreted by another map without anyone having
    /// asked for anything.
    viewer_in_flight: Option<RequestToken>,
    /// The token of the OPEN viewer, not of the one being requested.
    ///
    /// Separate from `viewer_in_flight`, which is cleared on opening: the
    /// image's bytes arrive AFTERWARD, and without this there would be
    /// nothing to check that they belong to this viewer and not the previous
    /// one.
    visor_token: Option<RequestToken>,
    /// The open viewer, if there is one. The model is the SHARED one
    /// (`norte_frontend::viewer::Viewer`): decoding, hex, and scrolling are
    /// its own.
    visor: Option<norte_frontend::viewer::Viewer>,
    /// The layout: the tree the user configured. It is NOT touched on
    /// resizing — a saved layout is its intent, and rewriting it because the
    /// window shrank would mean opening the host for a minute eats the TUI's
    /// layout (ADR 0058 D5).
    tree: Node,
    /// The kinds this host knows how to declare (minimums, focus, roles).
    kinds: KindRegistry,
    /// The last panel bar that crossed the bridge. `parche` compares it
    /// against the current one and sends the new one if it differs: that is
    /// what makes the bar update through any path without each path having to
    /// know it.
    ultima_bar: Option<crate::dto::PanelBarView>,
    /// The last status bar items that crossed (ADR 0132), for the same reason
    /// as the panel bar.
    last_items: Option<Vec<crate::dto::StatusItemView>>,
    /// The last thin line that crossed per slot (ADR 0148), to send only what
    /// changes.
    ultima_line: std::collections::HashMap<u32, Option<u8>>,
    /// The last column fit that crossed, per slot
    /// (`norte_frontend::columns::fitted_columns`). It depends on the slot's
    /// width and its listing's names, and both change through paths that do
    /// not send a header; `parche` compares it and, if it differs, sends the
    /// header AND the rows together — a row with a cell whose header no
    /// longer has it would be painted without a width.
    last_setting: std::collections::HashMap<u32, Vec<norte_frontend::columns::Fitted>>,
    /// How many one-second ticks `status.message` has been on the bar (spec
    /// 2026-09-10): in TICKS so a test can advance it without sleeping.
    message_ticks: u32,
    /// The terminal panel's shell (#362), if one is alive.
    ///
    /// Here and not in the slot because the kind is `multi: false`: there is
    /// one, and it survives the panel being hidden behind a tab. What kills
    /// it is closing the slot, and its `Drop` does it.
    terminal: Option<norte_term::pty::Shell>,
    /// The terminal panel's epoch: bumps on closing it, and the timer in
    /// flight is left to die without rearming.
    terminal_epoch: u64,
    /// The text that was being counted: if it changes, the count goes back to
    /// zero.
    message_counted: Option<String>,
    /// The LAST known size's layout: who gets painted, who does not, and in
    /// what order they are tabbed through. Lives and dies with the size, not
    /// with the tree.
    split: Resolved,
    /// The last size distributed, in cells. It travels to the renderer with
    /// the layout: without it, it cannot know against which grid the
    /// rectangles it receives are measured.
    viewport: (u16, u16),
    /// Who has the focus and who is the destination.
    roles: Roles,
    /// The column configuration, per scheme.
    columns: norte_frontend::columns::ColumnsSettings,
    /// Each slot's location's attribute catalogue, cached by SCHEME: it is
    /// what says whether an `attr:` is a size, a date or a mode, and without
    /// it the raw number is painted.
    catalogos: std::collections::HashMap<String, norte_proto::AttrCatalog>,
    /// The slots with state, by id.
    slots: std::collections::BTreeMap<u32, Slot>,
    /// The open dialogs, in opening order. Each with its id: a second
    /// `Confirm` with the same id does not launch anything again, and one
    /// with an old id does not close the current one.
    dialogs: Vec<Dialog>,
    /// The next dialog id. Monotonic: an id is never reused, which is what
    /// makes "old" distinguishable from "current".
    next_modal: u64,
    /// The renaming plan under review, if there is one.
    revision_ia: Option<ai::RevisionIa>,
    /// The review's epoch: bumps on every REQUEST and on abandoning one in
    /// flight. A response with a different epoch arrived late and is
    /// discarded on Rust.
    epoch_ia: u64,
    /// SYNCHRONIZED navigation (`pane.sync-nav`): while it is on, every
    /// navigation of the active slot is repeated by the destination slot.
    ///
    /// Execution state and not config nor session: it is a mode turned on to
    /// do one thing and turned off afterward, like in Krusader.
    mirror_permanent: bool,
    /// The plan request IN FLIGHT: its epoch and the DIRECTORY it was
    /// requested for.
    ///
    /// The directory travels here and is not read from the slot on landing,
    /// because between requesting the plan and its arrival the reader may
    /// have navigated: a plan for `series/` opened while showing
    /// `downloads/` would be promising to rename what is shown, and would
    /// rename something else.
    ai_in_flight: Option<(u64, VPath, Vec<Vec<u8>>)>,
    /// The ORGANIZE plan under review (phase 8), if there is one.
    revision_organize: Option<organize::RevisionOrganize>,
    /// Its epoch: bumps on every request, and a response with a different one
    /// arrived late.
    epoch_organize: u64,
    /// The organize plan request IN FLIGHT: epoch, directory, and the names
    /// that were in it when requested.
    ///
    /// The names travel here for the same reason as the directory: between
    /// requesting the plan and its arrival, the reader may have navigated,
    /// and asking the slot then would paint the tree against a directory that
    /// is not its own — calling "new" a folder that did exist, or the other
    /// way around.
    organize_in_flight: Option<(u64, VPath, Vec<String>)>,
    /// The board: what is running, by task id.
    tasks: std::collections::BTreeMap<u64, tasks::TaskViva>,
    /// The transfer batch in progress, if there is one (#271).
    batch: Option<tasks::Batch>,
    /// The UI session: which revision was read, whether this window owns it,
    /// and whether the saved schema is from a version this host does not
    /// understand (ADR 0059).
    session: Session,
    /// The directory a human TYPED in on startup, if they typed one.
    ///
    /// Saved because the session is read after mounting the slots and
    /// overwrites everyone's location: without this, `norte-gui /usr/bin`
    /// ended up wherever you were yesterday. Consumed by
    /// [`Self::leer_session`] and never needed again — a startup intent is
    /// worth once.
    dir_requested: Option<VPath>,
    /// Comes from a HANDOFF (`--attach`, phase 9): the session's marks are
    /// claimed. Without it they are ignored — a startup is not a handoff.
    attach: bool,
    /// This window has handed over the screen and is waiting to know whether
    /// the terminal opened (phase 9). It is the only thing that authorizes a
    /// `HandoffFailed`: anyone can send the action, and without a handoff in
    /// progress there is nothing to recover nor to say.
    handoff_in_progress: bool,
    /// The last LISTING that had the focus. When the focus is on a pane that
    /// is not a listing — the tree, the places —, commands act on it and the
    /// tree navigates it ([`Self::active`]). Without this, `active` fell back
    /// to the listing with the lowest id, which can be the one on the RIGHT:
    /// choosing a branch moved the pane that did not have the focus.
    last_listing: Option<u32>,
    status: StatusView,
    connection: ConnectionView,
    /// The provider sessions traveling unencrypted (#44), tracked by the
    /// shared module.
    degraded: norte_frontend::banners::DegradedSet,
    /// What the daemon said about itself before leaving: handoff or stop.
    /// `None` = it has said nothing, or it already came back.
    daemon_notice: Option<&'static str>,
    /// The open comparison, if there is one.
    comparison: Option<sync::Comparison>,
    /// The open sync plan, if there is one.
    sync: Option<sync::Sync>,
    /// The checksum batch in flight, if there is one (#311). AT MOST ONE: the
    /// results dialog is a single one, and launching another supersedes the
    /// previous one.
    checksums: Option<tasks::ChecksumsInFlight>,
    /// The QUEUED checksum batch that still has no id (#311). `None` = none.
    checksums_pending: Option<sums::ChecksumsQueued>,
    /// A REQUESTED plan whose Task has not answered yet.
    sync_requested: Option<sync::SyncRequested>,
    /// The semantic query in flight, so it can be ABORTED.
    ///
    /// Aborting is not just ceasing to listen: the SDK sends `rpc.cancel`
    /// when the call is dropped, and on the other end there is an embed and
    /// an index sweep that cost something. Relaunching or closing the view
    /// stops them.
    semantics_in_flight: Option<tokio::task::JoinHandle<()>>,
    /// How many times the connection to the daemon has been (re)established.
    ///
    /// Task ids are handed out by a process's SCHEDULER and start at 1 on
    /// every startup, so after a handoff — which this window now knows is
    /// coming, `ConnEvent::GoingAway` — the new daemon hands out the SAME
    /// ids. Without distinguishing the epoch, the new task 3 inherited from
    /// the old one that its report had already been requested (and it was
    /// never requested), its affected directories, and even its detail.
    /// Approvals do not have this problem because the daemon seeds THEIR ids
    /// with the clock on purpose.
    epoch_connection: u64,
    /// The engine REFUSED a mutation because it could not open its journal.
    ///
    /// Persistent and not a message: hard rule 4 says nothing mutates without
    /// a journal, so this describes what is going to happen to the WHOLE
    /// session, not to the operation that was just attempted.
    ///
    /// **Who turns it on, exactly**: `Error::JournalUnavailable` is only
    /// produced by the EMBEDDED engine (the TUI's and the CLI's lazy
    /// journal). A daemon with that problem never gets to start, so a window
    /// mounted over a socket — today's case — cannot see this notice. It is
    /// still projected because the host does not choose who mounts it, and
    /// whoever mounted it over the embedded engine would have the same right
    /// to know it.
    ///
    /// And what this notice does NOT say: a journal that is simply BUSY lets
    /// the mutation through without recording it, and that does not produce
    /// this error nor turn this on. The notice talks about an engine that
    /// REFUSES, not one that fails to record.
    journal_refused: bool,
}

/// What the host knows about the saved session.
#[derive(Debug)]
struct Session {
    /// The revision being written to. Writing to a different one means
    /// overwriting whoever wrote in between, and the core rejects it.
    revision: u64,
    /// This window is the owner. A DETACHED one does not write: the session
    /// is a document with a single writer.
    owner: bool,
    /// What is saved is from a schema NEWER than what this host understands.
    /// Then it is not applied and — above all — not overwritten: starting
    /// from the config is recoverable; clobbering a future version's session
    /// is not.
    future: bool,
    /// The shared write policy: what to trim, when not to repeat, and how
    /// often a detached window asks again.
    policy: norte_frontend::session::PushPolicy,
    /// The body exactly as it was READ, to preserve what belongs to others.
    ///
    /// Writing from a `SessionBody::default()` discarded everything this
    /// window does not understand — another frontend's slots, and saved
    /// layouts — instead of preserving it. Wave #229–#234 put in "preserve
    /// what belongs to others on a handoff" for exactly this, and this window
    /// was not doing it.
    read: norte_frontend::session::SessionBody,
    /// Which slots the data read from disk KNEW ABOUT.
    ///
    /// The `[profile.start]` veto (ADR 0098). Separate from [`Self::read`]
    /// and not derived from it on the fly because they are two different
    /// questions: that one is what has to be written back, and this is what
    /// the session already knew — and it cannot move once the process starts
    /// saving its own.
    known: std::collections::BTreeSet<u32>,
    /// Each slot's age stamp, exactly as it was WRITTEN last.
    ///
    /// The shared policy stamps the slots that changed when preparing the
    /// body, and the caller has to remember that stamp for the next capture:
    /// stamping every capture with "now" made no body ever equal the
    /// previous one, and the tick wrote every second even though nothing had
    /// changed. It is the same map the terminal carries (`session.touched`).
    touched: std::collections::BTreeMap<u32, u64>,
    /// The body of a `session.put` in flight, if there is one: the next tick
    /// does not send another on top of it — two crossed writes with the same
    /// revision are a guaranteed conflict — and shutdown knows what was being
    /// written to say whether its own made it or not.
    in_flight: Option<std::sync::Arc<norte_frontend::session::SessionBody>>,
    /// The daemon refused the body for size (#316): from then on it is sent
    /// without history, which is what gets degraded. What had to be saved is
    /// where the reader is, and that fits.
    no_history: bool,
    /// The slots this process has already seeded from `[profile.start]`.
    ///
    /// Seeding happens the FIRST time. Without this count, a reader with no
    /// saved session went back to the profile's startup directory every time
    /// it entered and left it: for it, [`Self::known`] is always empty.
    seeded: std::collections::BTreeSet<u32>,
}

impl Slot {
    /// A newborn slot: no listing, no history, and LOADING.
    ///
    /// Just one, because the three places that built it — startup, opening a
    /// new slot on a layout change, and the test one — had to match field by
    /// field, and a new field forgotten in one of them is a slot that behaves
    /// differently depending on where it was born.
    ///
    /// `hidden` is the INITIAL state of `[ui] show_hidden` (#107). Given by
    /// the caller because it is configuration, and the pane is born showing
    /// everything: without this, a window with `show_hidden = false` in its
    /// config started up showing the dotfiles anyway, and `pane.toggle-hidden`
    /// hid them "for the first time" on every startup.
    fn empty(dir: VPath, hidden: bool, orden: norte_frontend::SortSpec, upload_row: bool) -> Self {
        let scheme = dir.scheme().to_owned();
        let mut pane = PaneState::new(dir, Vec::new());
        pane.set_show_hidden(hidden);
        pane.set_sort(orden);
        // `[ui] parent_entry`: the `..` row is born with the slot and is not
        // added to it afterward — a slot that opens without it and gains it
        // on the next listing would show two different screens for the same
        // config.
        pane.set_parent_row(upload_row);
        Self {
            pane,
            caps: None,
            order_scheme: scheme,
            history: History::default(),
            first_visible: 0,
            visible: 64,
            in_flight: None,
            dir_requested: None,
            marks_to_restore: Vec::new(),
            cursor_to_restore: None,
            rows_to_publish: false,
            draining: None,
            probing: false,
            cancel_probe: std::sync::Arc::default(),
            // No destination: a newborn slot is not going anywhere, it is
            // already where it is going to be.
            state: State::loading_toward(None, None),
            probed: std::collections::HashSet::new(),
            adornos: std::collections::HashMap::new(),
            cells_plugin: std::collections::HashMap::new(),
            decorating: false,
            visita_pending: None,
            decorated: std::collections::HashSet::new(),
            gen_adornos: 0,
        }
    }

    /// Forgets what the plugins said: the listing is a DIFFERENT one.
    ///
    /// A `git status` badge from one directory cannot survive a `cd`: the
    /// path would be different and would not match, but the MEMORY of
    /// "already requested" would survive and would leave the new listing
    /// undecorated forever.
    fn forget_adornos(&mut self) {
        self.adornos.clear();
        self.cells_plugin.clear();
        self.decorated.clear();
        self.gen_adornos += 1;
    }
}

impl State {
    /// Builds the state from the startup options.
    ///
    /// Takes the WHOLE options struct and not eight loose parameters: they
    /// are the same data, and a list of eight positions is where two
    /// `Effective` of the same type get swapped without the compiler saying
    /// anything. One listing slot per `browser` in the tree, all in the same
    /// directory: where each one starts from is the session's business (and
    /// until it exists, starting both where the host started is the honest
    /// thing to do).
    ///
    /// `[ui] show_hidden` (#107) seeds each one's initial state, same as in
    /// the TUI: absent = show everything.
    fn slots_initial(
        tree: &Node,
        kinds: &KindRegistry,
        dir: &VPath,
        settings: &norte_frontend::config::FrontendConfig,
        columns: &norte_frontend::columns::ColumnsSettings,
    ) -> std::collections::BTreeMap<u32, Slot> {
        let hidden = settings.common.ui_show_hidden.unwrap_or(true);
        let up = settings.common.ui_parent_entry.unwrap_or(true);
        let orden = columns.sort_for(dir.scheme());
        let mut slots = std::collections::BTreeMap::new();
        for SlotId(id) in tree.slot_ids() {
            if es_listing(tree, SlotId(id), kinds) {
                slots.insert(id, Slot::empty(dir.clone(), hidden, orden.clone(), up));
            }
        }
        slots
    }

    /// The negotiated language, for the continuations' labels.
    fn lang_de(locale: &str) -> norte_i18n::Lang {
        match locale {
            "es" => norte_i18n::Lang::Es,
            _ => norte_i18n::Lang::En,
        }
    }

    /// The startup roles.
    ///
    /// The TARGET is resolved by the shared layer, and is NOT set by hand.
    /// Setting it with `Roles::set` marked it as EXPLICIT — meaning "a person
    /// chose it" — when nobody had chosen it, and then it survived more
    /// candidates appearing: with three listings, the first one kept the role
    /// forever and copying sent things there without anyone having said so
    /// (ADR 0058 D7).
    fn roles_initial(
        tree: &Node,
        split: &norte_frontend::layout::Resolved,
        kinds: &KindRegistry,
        active: u32,
    ) -> Roles {
        let mut roles = Roles::con_active(SlotId(active));
        roles.reconcile(tree, split, kinds, SlotId(active));
        roles
    }

    /// Long because it is a struct LITERAL: one field per line, with the why
    /// for the ones that are not obvious. There is nothing to extract that
    /// is not just moving fields into a function that returns them one at a
    /// time.
    #[expect(
        clippy::too_many_lines,
        reason = "constructor: one field per line with its why, nothing to extract"
    )]
    fn new(instance: InstanceId, options: UiHostOptions) -> (Self, Arc<dyn HostBackend>) {
        let UiHostOptions {
            backend,
            initial_dir,
            initial_dir_requested,
            attach,
            locale,
            keymap,
            keymap_viewer: keymap_visor,
            keymap_dialog,
            layout: tree,
            viewport,
            columns,
            effects,
            settings,
            paths,
            theme,
            user_layouts,
            profile: startup_profile,
            log_ring,
        } = options;
        let dir = &initial_dir;
        let lang = Self::lang_de(&locale);
        let kinds = KindRegistry::builtin();
        let split = resolve(rect(viewport), &tree, &kinds);
        let slots = Self::slots_initial(&tree, &kinds, dir, &settings, &columns);
        let active = slots.keys().copied().next().unwrap_or(1);
        let roles = Self::roles_initial(&tree, &split, &kinds, active);
        let state = Self {
            labels_plugin: Labels::new(),
            instance,
            sequence: 0,
            token: 0,
            locale,
            palette: None,
            ir_a: None,
            gen_ir_a: 0,
            go_to_index: None,
            wizard: None,
            splash: None,
            splash_until_ms: None,
            splash_seen: false,
            processes_auto: false,
            strip: norte_frontend::task_strip::TaskStrip::default(),
            enqueue: false,
            strip_base: tokio::time::Instant::now(),
            strip_wake: None,
            palette_recent: Vec::new(),
            popular: norte_frontend::history::Popular::default(),
            volumes_pie: Vec::new(),
            footer_in_flight: false,
            menu: None,
            help: None,
            settings: None,
            extensions: None,
            agency: Agency::default(),
            desktop: Desktop::default(),
            focused: true,
            dest_pending: None,
            theme,
            scheme_dark: false,
            theme_chosen: None,
            menu_last: 0,
            // What `--profile` named is already APPLIED in `settings`; what
            // is missing is for the host to know it (#307).
            profile_active: startup_profile,
            selector_profile: None,
            gen_profiles: 0,
            cursor_processes: norte_frontend::processes::Processes::default(),
            log_panel: norte_frontend::logpanel::LogPanel::default(),
            log_ring,
            // One until the first frame tells the truth: never zero, so a
            // page keypress before painting moves something instead of
            // nothing.
            log_rows: 1,
            log_epoch: 0,
            log_seen: 0,
            log_remote: logpanel::LogRemote::default(),
            places: None,
            previews: std::collections::BTreeMap::new(),
            panels: std::collections::BTreeMap::new(),
            maps: std::collections::BTreeMap::new(),
            lines: std::collections::BTreeMap::new(),
            leaves: std::collections::BTreeMap::new(),
            gen_places: 0,
            branches: None,
            gen_branches: 0,
            open_on_create: None,
            gen_selector: 0,
            gen_extensions: 0,
            gen_catalog: 0,
            catalog_applied: 0,
            gen_palette: 0,
            gen_output: 0,
            imagen: None,
            thumbnail: None,
            search: None,
            epoch_search: 0,
            layouts: user_layouts,
            selector_layout: None,
            selector_columns: None,
            selector: None,
            config: settings,
            paths,
            effective_visor: keymap_visor.clone(),
            effective: keymap.clone(),
            lang,
            whichkey: None,
            resolver: Resolver::new(keymap),
            resolver_visor: Resolver::new(keymap_visor),
            resolver_dialog: Resolver::new(keymap_dialog),
            effects,
            visor_rows: None,
            visor_columns: None,
            viewer_in_flight: None,
            visor_token: None,
            visor: None,
            tree,
            kinds,
            ultima_bar: None,
            last_items: None,
            ultima_line: std::collections::HashMap::new(),
            last_setting: std::collections::HashMap::new(),
            message_ticks: 0,
            // Lazy, like in the terminal: a per-session shell nobody is going
            // to use is a process, a pty and someone's `.bashrc` running just
            // in case.
            terminal: None,
            terminal_epoch: 0,
            message_counted: None,
            split,
            viewport,
            roles,
            columns,
            catalogos: std::collections::HashMap::new(),
            slots,
            dialogs: Vec::new(),
            next_modal: 1,
            revision_ia: None,
            epoch_ia: 0,
            mirror_permanent: false,
            ai_in_flight: None,
            revision_organize: None,
            epoch_organize: 0,
            organize_in_flight: None,
            tasks: std::collections::BTreeMap::new(),
            batch: None,
            session: Session {
                revision: 0,
                owner: false,
                future: false,
                // A detached window asks about ownership again every thirty
                // ticks: the owner can close at any moment and then someone
                // has to pick it up.
                policy: norte_frontend::session::PushPolicy::new(30),
                read: norte_frontend::session::SessionBody::default(),
                known: std::collections::BTreeSet::new(),
                touched: std::collections::BTreeMap::new(),
                in_flight: None,
                no_history: false,
                seeded: std::collections::BTreeSet::new(),
            },
            dir_requested: initial_dir_requested.then(|| initial_dir.clone()),
            attach,
            handoff_in_progress: false,
            last_listing: None,
            status: StatusView::default(),
            connection: ConnectionView::Connected,
            degraded: norte_frontend::banners::DegradedSet::default(),
            epoch_connection: 0,
            semantics_in_flight: None,
            comparison: None,
            sync: None,
            checksums: None,
            checksums_pending: None,
            sync_requested: None,
            daemon_notice: None,
            journal_refused: false,
        };
        (state, backend)
    }

    /// The slot with the focus. There is always one: if the role points to a
    /// slot that no longer exists, it falls back to the first one there is.
    fn active(&self) -> u32 {
        let preferred = self.roles.get(RoleId::Active).map(|SlotId(id)| id);
        preferred
            .filter(|id| self.slots.contains_key(id))
            .or_else(|| self.last_listing.filter(|id| self.slots.contains_key(id)))
            .or_else(|| self.slots.keys().copied().next())
            .unwrap_or(1)
    }

    /// The slot with the FOCUS, whatever type it is.
    ///
    /// Not the same as [`Self::active`], and confusing them was a bug: that
    /// one answers "the LISTING commands act on" and skips whatever is not a
    /// listing, which is exactly what is needed for `F5` to copy something
    /// while the sidebar has the focus. This one answers where the keyboard
    /// is, which is what decides who receives a key and which slot is
    /// painted focused.
    fn focused(&self) -> u32 {
        self.roles
            .get(RoleId::Active)
            .map_or_else(|| self.active(), |SlotId(id)| id)
    }

    /// The ACTIVE slot, which always exists.
    ///
    /// The invariant (rule 6): `slots` is seeded from `tree.slot_ids()` —
    /// in `State::new` and in `apply_layout`, the only two places
    /// that touch it — and `validate` rejects a tree with no `browser`, so
    /// there is at least one. `active()` comes from `Roles`, and
    /// `reconciles_roles` runs after every layout change leaving them pointing
    /// at slots that exist.
    ///
    /// The invariant was FALSE until this wave: it was seeded from
    /// `split.placements`, which does not include hidden slots, so choosing
    /// a layout that places no listing emptied the map and the next key
    /// panicked inside the actor's task. It is pinned down in
    /// `una_disposicion_que_esconde_el_listado_deja_el_hueco_vivo`.
    fn slot(&self) -> &Slot {
        let id = self.active();
        self.slots.get(&id).expect("the active slot exists")
    }

    /// The active slot, mutable. Same invariant as [`Self::slot`].
    fn slot_mut(&mut self) -> &mut Slot {
        let id = self.active();
        self.slots.get_mut(&id).expect("the active slot exists")
    }

    /// Leaves the roles pointing at slots that EXIST and are VISIBLE.
    ///
    /// The focus is decided HERE (it only moves if the slot that had it is no
    /// longer valid) and the TARGET is decided by the shared layer
    /// ([`norte_frontend::layout::roles::Roles::reconcile`]), which is what
    /// implements ADR 0058 D7. This method used to have its own rule — "the
    /// first other visible slot" — and that rule was wrong for two reasons
    /// that do not show up with two panes: with THREE it guessed the one with
    /// the lowest id, and it overwrote a target a person had designated by
    /// hand on every focus change. Since copy and move read that role,
    /// guessing means sending files somewhere nobody chose. The shared rule
    /// preserves what is explicit and, with several candidates and none
    /// chosen, leaves the role UNSET: the transfer then asks for one to be
    /// designated instead of breaking the tie on its own.
    fn reconciles_roles(&mut self) {
        // The focus only MOVES when the slot that had it is no longer valid:
        // it got hidden, disappeared from the layout, or stopped being
        // focusable. Always overwriting it with "the first listing" — which
        // is what it used to do — turned Tab into a switch between two
        // panes: it fell onto the sidebar or the processes panel and bounced
        // back on its own before anyone saw it.
        let focus = self.roles.get(RoleId::Active).map(|SlotId(id)| id);
        let serves = focus.is_some_and(|id| {
            !self.hidden(id)
                && self.split.placements.iter().any(|(s, _)| s.0 == id)
                && kind_de(&self.tree, SlotId(id))
                    .and_then(|k| self.kinds.get(&k).map(|d| d.focusable))
                    .unwrap_or(false)
        });
        if !serves {
            self.roles.set(RoleId::Active, SlotId(self.active()));
        }
        let focus = SlotId(self.focused());
        self.roles
            .reconcile(&self.tree, &self.split, &self.kinds, focus);
        // Focus on a LISTING is remembered: it is where commands and the tree
        // return to while the focus is on another pane.
        if let Some(SlotId(id)) = self.roles.get(RoleId::Active)
            && self.slots.contains_key(&id)
        {
            self.last_listing = Some(id);
        }
    }

    /// Is this slot outside the layout for THIS size?
    ///
    /// A hidden slot — a tab behind another, a pane that does not fit — does
    /// not request listings nor project rows: what is not seen is not
    /// fetched.
    fn hidden(&self, id: u32) -> bool {
        self.split.hidden.contains(&SlotId(id))
    }

    /// Requests the listing of the slots that are VISIBLE and have not
    /// requested it yet.
    ///
    /// A hidden slot is not listed — what is not seen is not fetched —, so
    /// when the layout brings it into view it has to be requested THEN.
    /// Nobody was doing it: a `browser` hidden on startup that appeared when
    /// the window was enlarged stayed in `Loading` forever, with zero rows,
    /// and after a layout change its `Slot` did not even exist, so
    /// `snapshot()` fell through to the default arm and painted it as
    /// `Unsupported { kind_name: "browser" }`.
    ///
    /// Idempotent by design: it only wakes up what is in `Loading` WITHOUT a
    /// request in flight, so calling it on every layout change duplicates
    /// nothing.
    fn wake_visible(&mut self, backend: &Arc<dyn HostBackend>, mailbox: &mpsc::Sender<Message>) {
        let asleep: Vec<u32> = self
            .slots
            .iter()
            .filter(|(id, h)| {
                !self.hidden(**id)
                    && h.in_flight.is_none()
                    && matches!(h.state, SlotState::Loading { .. })
            })
            .map(|(id, _)| *id)
            .collect();
        for id in asleep {
            let dir = self.slots[&id].pane.dir().clone();
            self.token += 1;
            let token = RequestToken(self.token);
            if let Some(h) = self.slots.get_mut(&id) {
                h.in_flight = Some(token);
                h.draining = Some(token);
            }
            self.request_listing(id, &dir, token, backend, mailbox);
        }
    }

    /// Takes a stream's first page and leaves the rest draining toward the
    /// actor.
    ///
    /// The rest arrives through the SAME mailbox as everything else, with its
    /// request's token: a batch from an abandoned navigation is discarded
    /// just like its first page was.
    async fn first_page(
        listing: Result<(norte_client::EntryStream, Option<u64>), Error>,
        slot: u32,
        token: RequestToken,
        mailbox: mpsc::Sender<Message>,
    ) -> Result<(Vec<Entry>, Option<u64>), Error> {
        use futures::StreamExt as _;
        let (mut stream, skipped) = listing?;
        let mut first = Vec::with_capacity(FIRST_PAGE);
        let mut exhausted = false;
        while first.len() < FIRST_PAGE {
            match stream.next().await {
                Some(Ok(e)) => first.push(e),
                // An error mid-page counts as the listing's error: half a
                // page is not a listing.
                Some(Err(e)) => return Err(e),
                None => {
                    exhausted = true;
                    break;
                }
            }
        }
        // The task is ALWAYS launched, even if the stream has already run
        // out: its last message is what lowers `draining`, and without it a
        // listing that fits in one page would leave the slot marked as
        // "still arriving" for the rest of the session. And it is launched
        // SEPARATELY instead of sending it here because the actor awaits this
        // future: sending to the mailbox from inside it would block against
        // the only one that empties it.
        tokio::spawn(async move {
            let mut the_batch = Vec::with_capacity(FILL_BATCH);
            if !exhausted {
                while let Some(entry) = stream.next().await {
                    let Ok(entry) = entry else {
                        // The rest was cut off. What has already been painted
                        // is still valid; staying quiet about it is better
                        // than throwing away the whole listing.
                        break;
                    };
                    the_batch.push(entry);
                    if the_batch.len() >= FILL_BATCH {
                        let batch = std::mem::take(&mut the_batch);
                        if mailbox
                            .send(Message::MoreEntries(Box::new((token, slot, batch, false))))
                            .await
                            .is_err()
                        {
                            return;
                        }
                        the_batch = Vec::with_capacity(FILL_BATCH);
                    }
                }
            }
            let _ = mailbox
                .send(Message::MoreEntries(Box::new((
                    token, slot, the_batch, true,
                ))))
                .await;
        });
        Ok((first, skipped))
    }

    /// The initial listing of each VISIBLE slot, the only one that is awaited
    /// INLINE: until it exists there is no screen to show, so there is
    /// nothing to freeze.
    ///
    /// A hidden slot is not listed: what is not seen is not fetched, and as
    /// soon as the layout brings it into view it will be requested then.
    async fn list_initial(
        &mut self,
        backend_arc: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        let backend = backend_arc.as_ref();
        let visible: Vec<u32> = self
            .slots
            .keys()
            .copied()
            .filter(|id| !self.hidden(*id))
            .collect();
        for id in visible {
            let dir = self.slots[&id].pane.dir().clone();
            self.token += 1;
            let token = RequestToken(self.token);
            if let Some(h) = self.slots.get_mut(&id) {
                h.in_flight = Some(token);
                h.draining = Some(token);
            }
            self.request_catalog(&dir, backend_arc, mailbox);
            let stream = backend.list(dir.clone(), self.attrs_de(&dir)).await;
            let res = Self::first_page(stream, id, token, mailbox.clone()).await;
            self.lands_on(id, dir, res);
            // The footer's free space (spec 2026-09-10): also on startup,
            // which does not go through `land_listing`. Without this the
            // window opened with no "free" figure until the first navigation.
            self.request_footer_volumes(backend_arc, mailbox);
            // The same thing a navigation's landing does, and that this path
            // was not doing: a slot's FIRST directory was left without
            // capabilities until the reader navigated somewhere else. Meaning
            // the window that had just opened inside a container offered
            // writes that container does not accept — and the destination's
            // folding was not recorded either (#268) while nobody moved.
            self.request_capabilities(id, backend_arc, mailbox);
        }
    }

    /// Applies a listing's result onto ITS slot. The order and the cursor are
    /// decided by `PaneState`, which is the one that knows what to do with
    /// the cursor's memory and with a pending focus.
    fn lands_on(&mut self, id: u32, dir: VPath, res: Result<(Vec<Entry>, Option<u64>), Error>) {
        // #108: `[ui.columns]`'s order is PER SCHEME, so it is reapplied when
        // the slot changes scheme — not on every `cd`, which is what the TUI
        // does. Here the SESSION restores the order, and reapplying it on the
        // first landing would erase it before it was ever seen.
        let changes_scheme = self
            .slots
            .get(&id)
            .is_some_and(|h| h.order_scheme != dir.scheme());
        let orden = changes_scheme.then(|| self.columns.sort_for(dir.scheme()));
        let Some(target_slot) = self.slots.get_mut(&id) else {
            return;
        };
        if changes_scheme {
            dir.scheme().clone_into(&mut target_slot.order_scheme);
        }
        target_slot.in_flight = None;
        target_slot.dir_requested = None;
        // The listing is a DIFFERENT one: what was probed before says nothing
        // about these entries, which are born lazy all over again. Without
        // this clearing, returning to an already-visited directory left the
        // size and date columns blank for the rest of the session — and along
        // the way the set grew by one `VPath` per file seen in the whole life
        // of the process.
        target_slot.probed.clear();
        // And whatever is in flight is no longer valid: it is marked so its
        // response gets discarded instead of sticking to another directory.
        target_slot
            .cancel_probe
            .store(true, std::sync::atomic::Ordering::SeqCst);
        target_slot.cancel_probe = std::sync::Arc::default();
        target_slot.probing = false;
        // And the same with what the plugins said: another directory's path
        // would not match, but the memory of "already requested" would, and
        // it would leave the new listing undecorated forever.
        target_slot.forget_adornos();
        target_slot.decorating = false;
        match res {
            Ok((entries, skipped)) => {
                if let Some(spec) = orden {
                    target_slot.pane.set_sort(spec);
                }
                target_slot.pane.set_listing(dir, entries);
                // A refresh keeps the selection; a `cd` has none to keep and
                // arrives with an empty list. What the operation took away is
                // not marked again.
                let marks = std::mem::take(&mut target_slot.marks_to_restore);
                target_slot.pane.restore_marks(&marks);
                // And the cursor the session left, also AFTER `set_listing`:
                // before that there are no rows and row 12 would be row 0.
                // `set_cursor` clamps it if the directory has fewer entries
                // today than it did then.
                if let Some(row) = target_slot.cursor_to_restore.take() {
                    target_slot.pane.set_cursor(row);
                }
                // AFTER `set_listing`, which clears it: it is data for THIS
                // listing, and carrying over the previous one's would mean
                // saying entries are missing from a directory where they were
                // missing from a different one.
                target_slot.pane.set_skipped(skipped);
                target_slot.first_visible = 0;
                target_slot.state = SlotState::Ready;
            }
            Err(e) => {
                // With no stream there is no drain that will answer, so
                // whoever raised the flag lowers it.
                target_slot.draining = None;
                target_slot.pane.set_listing(dir, Vec::new());
                target_slot.marks_to_restore.clear();
                // A failed listing CONSUMES the saved cursor: if it stayed
                // pending, it would land on the next listing that arrives,
                // which may be from somewhere else.
                target_slot.cursor_to_restore = None;
                target_slot.state = SlotState::Error {
                    reason_key: norte_frontend::error::error_key(&e).to_owned(),
                    // WHICH one is asking for the password. Without this, a
                    // startup with two remote panes said "a secret is
                    // needed" twice and there was no way to know which one to
                    // answer. The name comes from `connections.toml` — a
                    // file, not something trustworthy — so it is masked and
                    // clamped like everything else that gets painted.
                    detail: match &e {
                        Error::SecretNeeded { conn, .. } => Some(clamp_display(
                            norte_frontend::display_name(conn.as_bytes()).0,
                        )),
                        _ => None,
                    },
                };
            }
        }
    }

    /// Applies an action and returns its acknowledgment plus whatever has to
    /// be published.
    ///
    /// What TAKES TIME is not done here. A navigation leaves the request in
    /// flight and returns; its response comes back to the actor as one more
    /// message and is applied in [`State::aterriza`]. That is why the cursor
    /// keeps responding while a dead NFS is thinking: the only writer is not
    /// waiting on anyone.
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive dispatcher: one arm per action and no logic inside, \
                  like `ejecutar_pendiente`. Splitting it in half would only move \
                  the boundary to an arbitrary place"
    )]
    fn apply(
        &mut self,
        action: &UiAction,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match action {
            UiAction::MoveCursor { slot_id, delta } => self.mover_cursor(*slot_id, *delta),
            UiAction::SelectRow {
                slot_id,
                key,
                generation,
            } => self.set_cursor(*slot_id, *key, *generation),
            UiAction::ToggleMark {
                slot_id,
                key,
                generation,
            } => self.mark(*slot_id, *key, *generation),
            UiAction::SetVisibleRange {
                slot_id,
                first,
                count,
            } => {
                let (slot_id, first, count) = (*slot_id, *first, *count);
                // It is NOT required to be the active slot: declaring which
                // rows are visible is not acting on the listing, it is saying
                // where the user is looking. The wheel over the pane next to
                // it moves THAT pane and steals nobody's focus.
                if !self.slots.contains_key(&slot_id) || self.hidden(slot_id) {
                    return (Self::stale(StaleAction::Generation), Vec::new());
                }
                let cap = count.min(u32::try_from(MAX_ROWS_PER_BATCH).unwrap_or(u32::MAX));
                if let Some(h) = self.slots.get_mut(&slot_id) {
                    h.first_visible = first;
                    h.visible = cap;
                }
                // Scroll = new rows coming into view, and possibly without a
                // size yet: probing goes with the window, not with the
                // cursor.
                self.probe(slot_id, backend, mailbox);
                self.adornar(slot_id, backend, mailbox);
                (self.applied(), vec![self.patch_rows_of(slot_id)])
            }
            UiAction::SortBy { slot_id, column } => self.sort_by(*slot_id, column),
            UiAction::ResizeColumn {
                slot_id,
                column,
                cells,
            } => self.resize_column(*slot_id, column, *cells, mailbox),
            UiAction::FocusSlot { slot_id } => {
                let slot_id = *slot_id;
                // Focusing something not visible, or that does not accept
                // focus, is a race against an earlier layout, not a command.
                //
                // The criterion is the SHARED traversal (`focus_order`), the
                // same one Tab uses: while this required a `browser`, a CLICK
                // on the processes panel or on the places sidebar did not
                // focus them — only Tab could —, and the renderer sends
                // exactly this action on a press.
                if !self.split.focus_order.contains(&SlotId(slot_id)) || self.hidden(slot_id) {
                    return (Self::stale(StaleAction::Generation), Vec::new());
                }
                self.roles.set(RoleId::Active, SlotId(slot_id));
                self.reconciles_roles();
                // A focus change does NOT resend the screen: the only thing
                // that changes is who carries each role. Sending the whole
                // screenshot cost every row of every listing on every Tab —
                // the same waste the bridge caps for the cursor (decision
                // D7).
                let change = ViewChange::Layout(self.layout());
                (self.applied(), vec![self.parche(vec![change])])
            }
            UiAction::MarkRange {
                slot_id,
                from,
                to,
                generation,
            } => {
                let (slot_id, from, to, generation) = (*slot_id, *from, *to, *generation);
                // BOTH ends have to exist in THIS generation. A half-valid
                // range means marking up to a place that is no longer the one
                // the user pointed at — and `PaneState::mark_range` CLAMPS by
                // contract, so an out-of-bounds end would mark the whole
                // listing, including rows the renderer never received. What
                // gets marked is a delete's input.
                let (Some(a), Some(b)) = (
                    self.row_of(slot_id, from, generation),
                    self.row_of(slot_id, to, generation),
                ) else {
                    return (Self::stale(StaleAction::Generation), Vec::new());
                };
                self.slot_mut().pane.mark_range(a, b);
                (self.applied(), vec![self.parche_rows()])
            }
            UiAction::Activate { .. }
            | UiAction::Parent { .. }
            | UiAction::BreadcrumbActivate { .. }
            | UiAction::History { .. } => self.navigation(action, backend, mailbox),
            UiAction::SetViewport { width, height } => {
                self.viewport = (*width, *height);
                self.split = resolve(rect(self.viewport), &self.tree, &self.kinds);
                // The target cannot point at something not visible: a copy
                // that lands on a hidden pane is a copy the user will not see
                // arrive.
                self.reconciles_roles();
                // Enlarging the window brings slots out of `hidden`, and a
                // slot that appears with no listing stays loading forever.
                self.wake_visible(backend, mailbox);
                self.responds_with_snapshot()
            }
            UiAction::SetColorScheme { dark } => {
                // Same thing, it is nothing: the renderer sends it on startup
                // and on every change, and repainting every row for a message
                // that changes nothing is work for nothing.
                if self.scheme_dark == *dark {
                    return (self.applied(), Vec::new());
                }
                self.scheme_dark = *dark;
                // Only the ROWS: the variant's CSS variables are plugged in
                // by the renderer on its own, synchronously, to avoid a
                // flicker. What the host has to redo is what is baked into
                // the row (bridge 66).
                (self.applied(), self.patches_of_rows_from_all())
            }
            UiAction::Key(k) => self.key(k, backend, mailbox),
            UiAction::SetViewerRows { rows } => self.pin_viewer_rows(*rows),
            UiAction::SetViewerCols { cols } => {
                self.visor_columns = Some((*cols).clamp(1, u32::from(u16::MAX)));
                (self.applied(), Vec::new())
            }
            UiAction::AiRenameDecide { approve } => {
                self.decide_revision_ia(*approve, backend, mailbox)
            }
            UiAction::OrganizeDecide { approve } => {
                self.decide_revision_organize(*approve, backend, mailbox)
            }
            UiAction::OrganizeScroll { down } => self.walk_organize(*down),
            UiAction::HandoffFailed { no_terminal } => self.handoff_failed(*no_terminal),
            UiAction::Resync => self.responds_with_snapshot(),
            UiAction::RequestQuit => self.request_exit(),
            UiAction::MenuOpen { menu } => self.expand_menu(*menu),
            UiAction::MenuPointRow { row } => self.point_in_menu(*row),
            UiAction::MenuActivateRow { row } => self.activate_from_menu(*row, backend, mailbox),
            UiAction::MenuClose => self.close_menu(),
            UiAction::MenuToggle => self.toggle_menu(),
            UiAction::WizardOpen => self.open_wizard(),
            UiAction::SplashOpen => self.open_splash(),
            UiAction::SplashClose => (self.applied(), self.close_splash()),
            UiAction::SplashActivateRow { number } => {
                self.activate_splash_row(*number, backend, mailbox)
            }
            UiAction::WizardActivateRow { row } => self.activate_wizard_row(*row, backend, mailbox),
            UiAction::PanelBarActivate { button } => self.click_pane_bar(*button, backend, mailbox),
            UiAction::StatusItemActivate { id } => self.click_status_item(id, backend, mailbox),
            UiAction::LayoutButtonActivate { id } => self.click_layout_button(id, backend, mailbox),
            UiAction::TabAction { slot_id, verb } => {
                self.tab_button(*slot_id, *verb, backend, mailbox)
            }
            UiAction::MoveSlot {
                slot_id,
                target,
                zone,
            } => self.mover_slot(*slot_id, *target, *zone, backend, mailbox),
            UiAction::ResizeSlot { slot_id, cells } => {
                self.drag_edge(*slot_id, *cells, backend, mailbox)
            }
            UiAction::ProfileActivateRow { row, generation } => {
                self.activate_profile_from_row(*row, *generation, backend, mailbox)
            }
            UiAction::Dialog { id, choice, secret } => {
                self.responder_dialog(*id, choice, secret.as_deref(), backend, mailbox)
            }
            UiAction::RefreshSlot { slot_id } => {
                let changes = self.refresh(*slot_id, backend, mailbox);
                if changes.is_empty() {
                    // It already had something in flight: what is about to
                    // land is newer than this click.
                    (self.applied(), Vec::new())
                } else {
                    (self.applied(), vec![self.parche(changes)])
                }
            }
            UiAction::LogSetLevel { level } => self.log_level(level, backend, mailbox),
            UiAction::LogSetFilter { filter } => self.log_filter(filter),
            UiAction::LogScroll { delta } => self.scroll_log(*delta),
            UiAction::PanelClick { slot_id, row, col } => {
                // The SAME action for both, and it branches on the slot's
                // kind: the renderer sends a cell and does not know — nor
                // does it need to — whether there is a guest or a treemap
                // behind it. What changes is who resolves it and against
                // which frame.
                if kind_de(&self.tree, SlotId(*slot_id))
                    .is_some_and(|k| k.as_str() == diskmap::KIND)
                {
                    self.click_on_map(*slot_id, *row, *col, backend, mailbox)
                } else {
                    self.click_on_pane(*slot_id, *row, *col, backend, mailbox)
                }
            }
            UiAction::PreviewScroll { slot_id, delta } => self.scroll_preview(*slot_id, *delta),
            UiAction::ViewerScroll { lines, cols } => self.scroll_visor(*lines, *cols),
            UiAction::LogFollow => self.follow_log(),
            UiAction::LogCycleSource => self.log_source(),
            UiAction::LogSetVisibleRange { rows } => self.log_rows(*rows),
            UiAction::CancelTask { task_id } => self.cancel(*task_id),
            UiAction::CompareSelectRow { .. }
            | UiAction::CompareActivateRow { .. }
            | UiAction::CompareToggleFilter { .. }
            | UiAction::CompareSetVisibleRange { .. } => {
                self.comparison_action(action, backend, mailbox)
            }
            // A dialog with a text field arrives with the task that brought
            // it (create directory, rename). Saying so is more honest than
            // accepting text nobody is going to read.
            UiAction::DialogInput { id, text } => self.write_in_dialog(*id, text),
            // And a FORM dialog (bridge 91): it says WHICH of its fields was
            // touched, which is what the single-field one does not need to
            // say.
            UiAction::DialogField { id, field, value } => {
                self.touch_dialog_field(*id, field, value)
            }
            UiAction::DirectoryPicked { path } => self.dest_chosen(path.clone(), backend, mailbox),
            UiAction::ProgramFinished {
                title_key,
                command,
                output,
                truncated,
                failed,
            } => self.program_finished(title_key, command, output, *truncated, *failed),
            UiAction::FilesDropped { paths } => self.released(paths, backend, mailbox),
            UiAction::WindowFocus { focused } => {
                self.focused = *focused;
                (self.applied(), Vec::new())
            }
            other => self.row_by_index(other, backend, mailbox),
        }
    }

    /// The actions that name an OVERLAY row by its index.
    ///
    /// Grouped and separate because they share the same risk: the renderer
    /// paints a list and the user presses on the list it HAD in front of it,
    /// not the one the host has now. The two whose set of rows can change on
    /// its own — the sidebar and the picker, filled from a background task —
    /// carry a generation; the rest cannot change without a user gesture, and
    /// what all of them do is REJECT an index out of range instead of
    /// clamping it.
    fn row_by_index(
        &mut self,
        action: &UiAction,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match action {
            UiAction::HelpSelectTopic { row } => self.choose_page(*row, backend, mailbox),
            UiAction::SettingsSelectRow { row } => self.choose_setting(*row),
            UiAction::SettingsActivate { row } => self.activate_setting_by_mouse(*row, mailbox),
            UiAction::SettingsQuery { text } => self.search_setting(text),
            UiAction::SettingsJumpSection { section } => self.jump_to_section(section),
            UiAction::SettingsReset { row } => self.reset_setting(*row, mailbox),
            UiAction::SettingsSet { id, value } => self.set_setting(id, value, mailbox),
            UiAction::ExtensionSelectRow { row } => self.choose_extension(*row, backend, mailbox),
            UiAction::ExtensionGovern { row, id, change } => {
                self.govern_by_mouse(*row, id, (*change).into(), backend, mailbox)
            }
            UiAction::ExtensionHelp { row, id } => self.extension_help(*row, id, backend, mailbox),
            UiAction::SelectTab { slot_id } => self.choose_tab(*slot_id, backend, mailbox),
            UiAction::AgentSelectRow { row, generation } => self.choose_agent(*row, *generation),
            UiAction::PickerSelectRow { row, generation } => {
                self.choose_row_from_selector(*row, *generation)
            }
            UiAction::PlaceActivateRow { row, generation } => {
                self.activate_place(*row, *generation, backend, mailbox)
            }
            UiAction::TreeActivateRow { row, generation } => {
                self.touch_branch(*row, *generation, true, backend, mailbox)
            }
            UiAction::TreeToggleRow { row, generation } => {
                self.touch_branch(*row, *generation, false, backend, mailbox)
            }
            UiAction::LayoutActivateRow { row } => self.choose_layout(*row, backend, mailbox),
            UiAction::SearchActivateRow { row } => self.go_to_result(*row, backend, mailbox),
            UiAction::HelpActivate { index } => self.activate_in_help(*index, backend, mailbox),
            // The rest was handled by `apply`; getting here would be an arm
            // it forgot, and answering `Applied` to something that was not
            // done is worse than saying it could not be done.
            _ => (Self::stale(StaleAction::Modal), Vec::new()),
        }
    }
}
