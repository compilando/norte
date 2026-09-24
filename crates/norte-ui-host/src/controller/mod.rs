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
use crate::commands::{Efecto, efecto_de};
use crate::dto::{
    BrowserSlotView, ColumnHeader, ConnectionView, DialogChoice, DialogView, LayoutView,
    PendingView, RowKind, RowView, SlotPlacement, SlotRole, SlotState, SlotView, StatusView,
    TaskStateView, TaskView, UiNotice, UiUpdate, ViewChange, ViewPatch, ViewSnapshot,
};

// The `impl Estado` blocks split by topic (ADR 0086). The actor, the
// mailbox, `Estado` and action dispatch stay here; every child module sees
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
const PLAZO_VISOR: std::time::Duration = std::time::Duration::from_secs(20);

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
type Rotulos = std::collections::HashMap<
    String,
    (
        crate::extensions::Texto,
        std::collections::HashMap<String, crate::extensions::Texto>,
    ),
>;

/// What it takes to paint a command's output: who, what, and what it
/// answered.
struct SalidaPedida {
    /// The extension's id, reverse-DNS validated.
    id: String,
    /// Its name, already masked, with its flag.
    plugin: crate::extensions::Texto,
    /// The command's title, already masked, with its flag.
    comando: crate::extensions::Texto,
    /// What it printed, or why not.
    res: Result<String, Error>,
}

/// What this window has from the DESKTOP.
///
/// Together because they are the same thing seen twice: where something is
/// asked of the hosting process, and what that process answered and is
/// still on screen.
#[derive(Debug, Default)]
struct Escritorio {
    /// Where NATIVE effects go out through, when someone is listening.
    ///
    /// `Option` because the state is built before the channel — the first
    /// snapshot comes from it — and because a host with nobody subscribed
    /// has to be able to keep going: an effect nobody picks up is a gesture
    /// that does nothing, not an error.
    nativos: Option<broadcast::Sender<crate::dto::NativeEffect>>,
    /// The last extension command's output, if it is still on screen.
    ///
    /// Here and not in the extensions manager: a command is launched from
    /// the PALETTE, which does not need the manager open — and does not
    /// open it — and an output stored inside a closed screen is seen by
    /// nobody.
    salida: Option<crate::dto::ExtensionOutputView>,
    /// The last waited-for PROGRAM's output (#312), if it is still on
    /// screen.
    programa: Option<crate::dto::ProgramOutputView>,
}

/// What this window knows about AGENT sessions.
///
/// Together and not loose in the state: all three describe the same thing —
/// who has asked for permission, whether it is being looked at, and what is
/// being undone — and splitting them apart meant having to remember all
/// three every time one changes.
#[derive(Debug, Default)]
struct Agencia {
    /// What has been seen. ALWAYS present: a request arrives when it
    /// arrives, and the panel only decides whether to paint it.
    sesiones: crate::agents::Agentes,
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
enum Cambio {
    /// Grant or revoke its capabilities.
    Aprobacion,
    /// Turn it on or off.
    Encendido,
    /// Uninstall it (ADR 0104).
    Desinstalacion,
}

impl From<crate::action::ExtensionChange> for Cambio {
    fn from(c: crate::action::ExtensionChange) -> Self {
        use crate::action::ExtensionChange as E;
        match c {
            E::Approval => Self::Aprobacion,
            E::Enabled => Self::Encendido,
            E::Uninstall => Self::Desinstalacion,
        }
    }
}

/// The change already resolved to a concrete value.
///
/// Separate from [`Cambio`] on purpose: `a` over a row means different
/// things depending on its state, and what travels to the daemon is the
/// resolved one.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Gobierno {
    /// `plugin.set_approval`, with the anchor that was SHOWN (#282). `None`
    /// when revoking: taking away a permission grants nothing, and refusing
    /// it over a stale digest would keep alive exactly what someone is
    /// trying to remove.
    Aprobar(bool, Option<String>),
    /// `plugin.set_enabled`.
    Encender(bool),
    /// `plugin.uninstall` (ADR 0104). Already confirmed by a human.
    Desinstalar,
}

/// Cap on capabilities a grant question can show.
///
/// Not a trim: above this, it does NOT ask. A manifest declaring more
/// capabilities than fit on one screen does not produce an informed
/// decision, and granting what was not read is what this question exists to
/// prevent.
const MAX_CAPABILIDADES: usize = 32;

/// Cap on characters of an extension command's output.
///
/// What a plugin prints has no cap on its own side: a command can return a
/// megabyte and letting it through hands it the window. It is applied
/// BEFORE masking: otherwise, a 100 MB output gets masked whole — and
/// materializes whole in the writer's task — just so four thousand
/// characters survive.
const MAX_SALIDA: usize = 4_000;

/// Cap on LINES of that output.
///
/// Lines cross loose so that a line break does not mark an honest output as
/// hostile, and a list also needs its own cap.
const MAX_SALIDA_LINEAS: usize = 200;

/// Deadline for RUNNING an extension command.
///
/// Separate from the one for reading the catalog, and much longer: on the
/// other side runs third-party code that can be indexing or talking over
/// the network, and cutting it off at five seconds does not stop it — it
/// keeps running on the daemon, with its effects — it only leaves this
/// window not knowing how it ended.
const PLAZO_COMANDO: std::time::Duration = std::time::Duration::from_mins(1);

/// Deadline for an extensions call (catalog or page).
///
/// Help is painted without waiting for it, so this deadline does not govern
/// a screen: it governs a task that, if it never came back, would leave an
/// `id` claimed and a page blank forever.
const PLAZO_PLUGINS: std::time::Duration = std::time::Duration::from_secs(5);

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
const SESION_TIC: std::time::Duration = std::time::Duration::from_secs(1);

/// How often what the terminal pane's shell has written is flushed.
///
/// Thirty times a second: that is what makes typing in there feel like
/// typing in a terminal and not like sending a telegram. A tick that finds
/// no bytes produces no patch and wakes no renderer, so an idle shell costs
/// no more than checking an empty mailbox.
///
/// And the pump only runs while the panel exists: see `Mensaje::TerminalTic`.
const TERMINAL_TIC: std::time::Duration = std::time::Duration::from_millis(33);

/// Deadline for a plan request to a model.
///
/// Generous: thinking is what it does. It is the UPPER cap, so a call that
/// never comes back does not leave the request in flight forever — with
/// `Escape` as the only exit and nothing on screen saying it is still alive.
const PLAZO_IA: std::time::Duration = std::time::Duration::from_mins(2);

/// Cap on a typed name, in bytes. Neither `NAME_MAX` (which belongs to the
/// filesystem and is not known here) nor the screen's: a generous cap that
/// keeps a renderer from sending a megabyte, and that REJECTS instead of
/// trimming — trimming a name is inventing another one.
const MAX_NOMBRE: usize = 4096;

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
const SONDEOS_A_LA_VEZ: usize = 8;

/// How long ONE probe is awaited. A hung provider cannot take the rest of
/// the batch down with it.
const PLAZO_SONDEO: std::time::Duration = std::time::Duration::from_secs(5);

/// How many entries are probed per batch. It is a SCREEN with slack: more
/// is not seen, and every probe is a trip to the daemon.
const MAX_SONDEOS: usize = 200;

/// Updates retained for a slow subscriber. Once past that, the subscriber
/// finds out it fell behind and requests a snapshot: it is the cheap
/// recovery, the one that spends no host memory.
const UPDATE_BUFFER: usize = 64;

/// How many rows a page moves in help.
///
/// The renderer owns the body's scroll — a corpus page crosses whole — so
/// this number only governs the CURSORS, the ones the host carries.
const PAGINA_DE_AYUDA: usize = 10;

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
    pub initial_dir_pedido: bool,
    /// This window is the other end of a HANDOFF (`--attach`, phase 9), so
    /// besides the screen it claims the MARKS the other frontend left.
    ///
    /// The same nature as [`Self::initial_dir_pedido`] — how the process was
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
    /// [`crate::commands::Efectos::SoloLectura`] until task 5.4's security
    /// review lifted its toggle; today all three setups (window, TUI and
    /// tests) use [`crate::commands::Efectos::Completo`], and `SoloLectura`
    /// remains the position a setup that wants neither destructive nor
    /// policy authority can choose.
    pub effects: crate::commands::Efectos,
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
/// (`aplicar_disposicion_con`, `aplicar_arbol`). Dies with the mailbox, like
/// the other pumps.
fn bombear_tic_de_sesion(buzon: mpsc::Sender<Mensaje>) {
    tokio::spawn(async move {
        let mut tic = tokio::time::interval(SESION_TIC);
        // `interval`'s first tick is immediate, and there is nothing to
        // write an instant after starting.
        tic.tick().await;
        loop {
            tic.tick().await;
            if buzon.send(Mensaje::SesionTic).await.is_err() {
                return;
            }
        }
    });
}

/// Forwards the backend's `plugin.notice`s to the host's mailbox (ADR 0100).
/// A function separate from `start` because the pump list was already
/// filling the line limit, and its shape is the same as the others': a task
/// that dies with the channel that feeds it.
fn bombear_avisos_de_plugin(backend: &dyn HostBackend, buzon: mpsc::Sender<Mensaje>) {
    let Some(mut avisos) = backend.take_plugin_notices() else {
        return;
    };
    tokio::spawn(async move {
        while let Some(n) = avisos.recv().await {
            if buzon.send(Mensaje::AvisoPlugin(Box::new(n))).await.is_err() {
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
fn bombear_canales_del_backend(
    backend: &dyn HostBackend,
    buzon: &mpsc::Sender<Mensaje>,
    efectos: crate::commands::Efectos,
) {
    // The connection's two channels belong to the FIRST owner, so they are
    // taken once, here.
    if let Some(mut eventos) = backend.take_conn_events() {
        let buzon = buzon.clone();
        tokio::spawn(async move {
            while let Some(ev) = eventos.recv().await {
                if buzon.send(Mensaje::Conexion(ev)).await.is_err() {
                    return;
                }
            }
        });
    }
    // Plaintext-session notices (#44) are ALWAYS taken: they do not depend
    // on whether this window can write. That a listing being READ travels
    // unencrypted is a fact for whoever is looking at it, not a permission.
    if let Some(mut degradadas) = backend.take_degraded() {
        let buzon = buzon.clone();
        tokio::spawn(async move {
            while let Some(d) = degradadas.recv().await {
                if buzon.send(Mensaje::Degradada(Box::new(d))).await.is_err() {
                    return;
                }
            }
        });
    }
    // And failures (#322), with the same criterion: why a machine could NOT
    // be entered is told to whoever tried, whether this window can write or
    // not.
    if let Some(mut fallidas) = backend.take_failed() {
        let buzon = buzon.clone();
        tokio::spawn(async move {
            while let Some(f) = fallidas.recv().await {
                if buzon.send(Mensaje::Fallida(Box::new(f))).await.is_err() {
                    return;
                }
            }
        });
    }
    // Hook notices (ADR 0100) talk about files that have already changed, so
    // they are read whether it can write or not.
    bombear_avisos_de_plugin(backend, buzon.clone());
    // Policy approvals are a MUTATION by delegation: saying yes to an
    // agent's operation. A frontend that cannot write yet cannot authorize
    // another one to write either, so in read-only the channel is not even
    // taken (and the dialog does not exist, which is more honest than one
    // that does not respond).
    if efectos == crate::commands::Efectos::Completo
        && let Some(mut aprobaciones) = backend.take_approvals()
    {
        let buzon = buzon.clone();
        tokio::spawn(async move {
            while let Some(req) = aprobaciones.recv().await {
                if buzon
                    .send(Mensaje::Aprobacion(Box::new(req)))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
    }
    if let Some(mut ajenas) = backend.take_foreign_tasks() {
        let buzon = buzon.clone();
        tokio::spawn(async move {
            while let Some(task) = ajenas.recv().await {
                if buzon
                    .send(Mensaje::TaskNueva(Box::new((task, Vec::new(), None))))
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
    inbox: mpsc::Sender<Mensaje>,
    updates: broadcast::Sender<BridgeEnvelope<UiUpdate>>,
    nativos: broadcast::Sender<crate::dto::NativeEffect>,
    instance: InstanceId,
}

/// What comes back from a listing: the token that requested it, the slot it
/// goes to, the directory and the result.
/// What the viewer requested: token, path, the header read and the plugin
/// preview if one applied.
type Contenido = (
    RequestToken,
    VPath,
    Result<Vec<u8>, Error>,
    Option<norte_proto::methods::PluginPreviewStyled>,
);

/// The same, for the DOCKED viewer (#291): the slot that requested it goes
/// in front, and it is not resolved on arrival — the slot may have closed,
/// and then the answer is dropped.
type PreviewContenido = (u32, Contenido);

/// What comes back from a listing: its token, the slot, the directory, and
/// the first page's entries with HOW MANY the provider skipped.
type RespuestaListado = (
    RequestToken,
    u32,
    VPath,
    Result<(Vec<Entry>, Option<u64>), Error>,
);

/// What comes back from a probe batch: the directory being probed, the slot,
/// and the pairs `(what was requested, what the provider answered)`.
type Sondas = (VPath, u32, Vec<(VPath, Entry)>);

/// What comes back from measuring a disk map (phase 4).
///
/// The slot that requested it, the TOKEN for that request — one that is not
/// the live one belongs to a directory already left behind — and the result,
/// with the Task's STATE attached to the report: one from a cancelled Task
/// is partial, and painting it as complete turns a huge directory into a
/// small one.
type MedidaDeMapa = (
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

enum Mensaje {
    Accion(Box<UiAction>, oneshot::Sender<ActionAck>),
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
    Listado(Box<RespuestaListado>),
    /// A scheme's attribute catalog, already resolved.
    Catalogo(Box<(String, norte_proto::AttrCatalog)>),
    /// An agent op awaits a human decision.
    Aprobacion(Box<norte_proto::methods::PolicyApprovalRequired>),
    /// This approval's TTL ran out: the daemon no longer accepts it.
    AprobacionCaducada(u64),
    /// The chosen theme is (or is not) in `norte.toml` now. `Some(key)` is
    /// the reason it could not be saved; `None` means it was saved.
    ///
    /// Only the failure IS SAID. A "saved" for every chosen theme would be a
    /// message per Enter on a screen whose result is already visible: the
    /// colors changed.
    TemaPersistido(Option<&'static str>),
    /// A column's width is (or is not) in `norte.toml` now (bridge 64). Same
    /// treatment as the theme: only the failure is reported.
    AnchoPersistido(Option<&'static str>),
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
    TemaResuelto(Box<(String, Result<norte_theme::Theme, &'static str>)>),
    /// The session's tick: every second, like the terminal. If the screen
    /// changed since the last write, it is written; if not, nothing.
    SesionTic,
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
    SesionPuesta(
        Box<(
            Result<u64, Error>,
            std::sync::Arc<norte_frontend::session::SessionBody>,
        )>,
    ),
    /// The session re-read after a conflict: another window wrote in
    /// between and the revision being written over is no longer valid.
    SesionReleida(Result<(norte_proto::methods::Session, bool), Error>),
    /// The HANDOFF to the terminal finished (phase 9): the screen is
    /// written and the session, released — or it could not be, and then
    /// nothing happens and it is reported.
    Relevado {
        /// This window was the owner and has stopped being one.
        soltada: bool,
    },
    /// This FINISHED task's time on the board ran out
    /// ([`TTL_TASK_TERMINAL`]). Carries the connection EPOCH it was
    /// registered in: after a daemon handoff the ids start over at 1, and
    /// expiring by number would evict a live task that only shares its
    /// number with the one that left.
    TaskCaducada(u64, u64),
    /// Something requested outside the actor finished and has to be SAID:
    /// the message's key (today, a pause the daemon does not know how to
    /// do).
    Decir(&'static str),
    /// The light progress bar changes with no progress arriving: it passed
    /// its threshold, the panel's, or the "✓"'s time ran out (ADR 0146).
    Tira,
    /// The `policy.decide` that WAS APPROVING did not reach the daemon.
    /// A `policy.decide` that did not go well: which approval and under
    /// which key it is counted (#279).
    AprobacionNoEntregada(u64, &'static str),
    /// More entries from the listing draining in the background.
    ///
    /// The `bool` says whether it is the LAST batch. Without it, `drenando`
    /// was raised on requesting the listing and nobody ever lowered it —
    /// not even when the stream ran out within the first page — so the
    /// field did not mean "still arriving" but "this was requested at some
    /// point", and anyone consulting it to decide got it wrong.
    MasEntradas(Box<(RequestToken, u32, Vec<Entry>, bool)>),
    /// What a request launched for an OVERLAY answered.
    ///
    /// The five travel together because they are the same story: a surface
    /// opened WITHOUT waiting — the documentation and the mount table are
    /// cosmetic, and a blank window until the daemon answers is worse than a
    /// list gaining rows half a second later — and this is the answer
    /// arriving late. Each one checks its surface is still open before
    /// touching anything.
    Fondo(Box<Fondo>),
    /// The content the viewer requested.
    /// What the viewer requested: the file's header and, if some
    /// `previewer` plugin applied, its styled preview.
    ///
    /// Both in the SAME message because they are a single answer to a single
    /// key: sending them separately would open the raw viewer and swap it
    /// for the preview an instant later, a flicker nobody asked for.
    Contenido(Box<Contenido>),
    /// What a preview slot requested (#291): same as [`Self::Contenido`] but
    /// for the docked viewer, and with the slot in front.
    PreviewContenido(Box<PreviewContenido>),
    /// The frame a plugin pane painted (phase 3), with the token for the
    /// request that asked for it: one that is not the live one belongs to a
    /// cursor that has already moved.
    PanelContenido(
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
    MapaContenido(Box<MedidaDeMapa>),
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
    Hidratado(Box<Sondas>),
    /// A freshly enqueued Task, with its progress, its cancellation and the
    /// directories it will leave out of date.
    TaskNueva(Box<(crate::backend::HostTask, Vec<VPath>, Option<Reintento>)>),
    /// What a slot's location accepts: how it folds names (#268) and
    /// whether it refuses writes.
    Capacidades(u32, VPath, norte_proto::Capabilities),
    /// Enqueuing it failed. The user has to find out: they requested a
    /// delete.
    TaskFallida(Box<Error>),
    /// A rejection on enqueuing ONE batch entry (#271). Separate from
    /// [`Self::TaskFallida`] on purpose: that one is sent by anyone enqueuing
    /// anything — a search, a plan, an undo — and its place is the status
    /// bar; this one is only sent by a batch's loop, and its place is the
    /// batch's COUNT.
    TaskDeLoteRechazada(Box<Error>),
    /// The connection to the daemon changed state.
    Conexion(norte_client::ConnEvent),
    /// A provider session travels UNENCRYPTED (#44).
    Degradada(Box<norte_proto::methods::ConnectionDegraded>),
    /// A connection could NOT be opened, and why (#322).
    Fallida(Box<norte_proto::methods::ConnectionFailed>),
    /// A `hook` plugin said something about an already-registered mutation,
    /// or the daemon turned off its hooks (0.69.0, ADR 0100).
    AvisoPlugin(Box<norte_proto::methods::PluginNotice>),
    /// The secret was delivered (or not), and with it what to do with the
    /// navigation `SecretNeeded` had suspended (#327).
    SecretoEntregado(Box<(u32, VPath, Result<(), Error>)>),
    /// Time to check whether the log has anything new (#326). Carries the
    /// EPOCH of the opening that scheduled it: one from a previous opening
    /// is left to die instead of rearming forever.
    RegistroTic(u64),
    /// What the daemon answered to `log.tail` (#328), with the EPOCH of the
    /// opening that requested it.
    ///
    /// The epoch is not decoration: a close and an open fit between asking
    /// and answering, and lines from the previous session landing on the
    /// new panel would be history nobody asked for, ahead of the real one.
    RegistroRemoto(u64, Box<Result<norte_proto::methods::LogTailResult, Error>>),
    /// What the daemon answered to `log.level` (#328): the level really left
    /// set, which may not be the one requested.
    RegistroNivel(u64, Box<Result<String, Error>>),
    /// A progress snapshot. Through the SAME queue as everything else,
    /// which is what guarantees a terminal state neither jumps ahead nor
    /// gets lost.
    Progreso(Box<norte_proto::TaskProgress>),
    /// The report of a Task that has already finished and DOES have a
    /// report.
    ///
    /// Carries the whole `Result` and not an `Option`: "went fine" and "the
    /// daemon does not know how to report" are two different things, and
    /// collapsing them is exactly what these reports exist not to do.
    Informe(Box<(u64, u64, tasks::Informe)>),
    /// The `fs.stat` done between creating a file and opening it (#303): the
    /// path that was created, and whether what is there is still a regular
    /// file.
    ///
    /// With no epoch: what is decided with this is opening an ABSOLUTE path
    /// on this machine's desktop, which does not mean something different
    /// depending on which daemon answers — unlike reports, whose task ids
    /// start over at 1 after a handoff.
    CreadoComprobado(Box<(norte_proto::VPath, Veredicto)>),
    /// A favorite was saved (#309): its name, where it points to and, if it
    /// failed, the reason's key. The in-memory copy is not touched until the
    /// disk answers.
    ///
    /// The destination travels in the message and is not re-read on
    /// arrival: between requesting the name and saving, the pane may have
    /// navigated, and reflecting "where I am now" would put a different
    /// favorite in the list than the one just written to the file.
    FavoritoPersistido(Box<(String, norte_proto::VPath, Option<&'static str>)>),
    /// The profile was written (or not): name and the failure's key (#318).
    PerfilGuardado(Box<(String, Option<&'static str>)>),
    /// A favorite was removed, in the same shape.
    FavoritoQuitado(Box<(String, Option<&'static str>)>),
    Apagar(oneshot::Sender<ShutdownReport>),
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
enum Veredicto {
    /// Still a regular file: go ahead.
    EsElFichero,
    /// A link, a folder or nothing. All three are said the same way: naming
    /// which one would confirm the link to whoever planted it.
    YaNoEsElFichero,
    /// The `stat` failed. Nothing opens, and it is reported that the check
    /// could not be made.
    NoSeSabe,
}

impl From<bool> for Veredicto {
    fn from(regular: bool) -> Self {
        if regular {
            Self::EsElFichero
        } else {
            Self::YaNoEsElFichero
        }
    }
}

/// A background request's answer, by surface.
///
/// A separate enum and not five [`Mensaje`] variants: the actor is a
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

enum Fondo {
    /// The plugin catalog HELP requested, for its side panel.
    PluginsDeAyuda(Result<norte_proto::methods::PluginListResult, Error>),
    /// The catalog requested on STARTUP, to declare which PANES the plugins
    /// contribute (phase 3).
    ///
    /// Separate from help's and the manager's, and not by whim: those two
    /// exit early if their surface is closed, and a plugin pane has to be
    /// able to paint without anyone having opened help or the manager. What
    /// it brings is the DECLARATION of which slots exist, not any of their
    /// content.
    PanelesDePlugin(Result<norte_proto::methods::PluginListResult, Error>),
    /// A plugin's page, requested on opening it in help.
    PaginaDePlugin(
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
    Catalogo(
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
    AvisosDeDestino(ModalId, Vec<String>),
    /// A `policy.undo_session`'s task already has an id: it is tied to its
    /// session.
    UndoDeSesion(u64, String),
    /// The profiles in `profiles/`, already read, and what to do with them:
    /// `None` = open the selector, `Some(forward)` = jump to the neighbor
    /// without opening anything.
    ///
    /// Reading them is disk I/O — a directory and a `norte.toml` per
    /// profile — so it comes through here like everything that cannot run
    /// in the actor.
    Perfiles(
        Vec<norte_frontend::profile_picker::UserProfile>,
        Option<bool>,
    ),
    /// A profile's configuration, already loaded, with its name.
    ///
    /// `Err` is the reason's key: a profile that fails to load changes
    /// NOTHING — it stays on the one you were on, which is what ADR 0079 D7
    /// asks for a switch.
    PerfilCargado(
        std::ffi::OsString,
        Box<Result<norte_frontend::config::FrontendConfig, &'static str>>,
    ),
    /// An F11 setting is (or is not) in `norte.toml` now, and the
    /// configuration re-read with it. Boxed because a `FrontendConfig` is
    /// large next to the rest of the enum.
    AjusteEscrito(Box<settings::AjusteEscrito>),
    /// An F11 key is no longer in `norte.toml` — reset.
    AjusteRestablecido(Box<settings::AjusteRestablecido>),
    /// The catalog the PALETTE requested, for its plugin rows.
    PluginsDePaleta(u64, Result<norte_proto::methods::PluginListResult, Error>),
    /// A governance change (approve/revoke, turn on/off) answered.
    ///
    /// Carries the OPENING for the same reason as the catalog: the answer
    /// can arrive over a manager that has already closed and reopened.
    Gobernada(u64, Result<(), Error>),
    /// A `[config.<key>]` write answered: the opening of the manager that
    /// requested it, which extension, and what the daemon said.
    ///
    /// The opening is needed for the same reason as in the catalog: closing
    /// the manager and reopening it while a write is in flight let the
    /// first one's failure close the second one's card.
    ConfigEscrita(u64, String, Result<(), Error>),
    /// An extension command's output: the opening that requested it, the
    /// extension's id, its two labels WITH their flag, and what it
    /// answered.
    ///
    /// The labels travel with their flag and not just masked because there
    /// is no coming back from a mask: a flag computed afterward, over
    /// already-masked text, always comes out `false` and the panel claims
    /// to be faithful.
    SalidaDeComando(u64, Box<SalidaPedida>),
    /// An extension's `[config]` schema, requested on opening its card.
    FichaDePlugin(
        String,
        Result<norte_proto::methods::PluginGetConfigResult, Error>,
    ),
    /// The host's volumes, with the OPENING of the selector that requested
    /// them. See [`Fondo::Catalogo`].
    Volumenes(u64, Result<Vec<norte_proto::methods::Volume>, Error>),
    /// A timeline page (#359): the slot, the request's token and where it
    /// was requested from (`None` = the first one).
    PaginaDeLinea(
        u32,
        RequestToken,
        Option<i64>,
        Result<norte_proto::methods::JournalListResult, Error>,
    ),
    /// The connections for "go to" (#357), with the OPENING that requested
    /// them: an answer from a previous opening does not fill in the current
    /// one.
    ConexionesDeIrA(
        u64,
        Result<Vec<norte_proto::methods::ConnectionEntry>, Error>,
    ),
    /// What the index answered to a "go to" query (#357): the opening and
    /// the query that were asked about, to drop the answer if it is no
    /// longer what is written.
    IndiceDeIrA(
        u64,
        String,
        Result<Vec<norte_proto::methods::SemanticHit>, Error>,
    ),
    /// The configured connections, with the OPENING that requested them
    /// (#264).
    ///
    /// The whole result: since #365 it also brings the ones the daemon
    /// could not read, and the selector shows them with no destination.
    Conexiones(
        u64,
        Result<norte_proto::methods::ConnectionListResult, Error>,
    ),
    /// A batch of results, with the search epoch that requested it.
    Resultados(u64, Box<norte_proto::methods::SearchHits>),
    /// A SEMANTIC query's answer: whole, all at once.
    Semanticos(u64, Result<Vec<norte_proto::methods::SemanticHit>, Error>),
    /// The comparison has a Task: it is christened so it can be cancelled.
    ComparacionViva(u64, norte_proto::TaskId),
    /// The synchronization plan has a Task.
    PlanDeSyncVivo(u64, norte_proto::TaskId),
    /// The `sync.apply` was accepted and this is its Task.
    SyncAplicando(u64, norte_proto::TaskId),
    /// The `sync.apply` failed. The `bool` says whether it is KNOWN that it
    /// wrote nothing: a rejection (policy, conflict, invalid path) knows,
    /// because the daemon answered; a dropped transport does NOT, because
    /// the request may have arrived and be running right now. Releasing the
    /// latch in the second case invites applying the same plan twice over
    /// the same destination.
    SyncNoAplicado(u64, bool),
    /// A finished synchronization's report.
    InformeDeSync(
        u64,
        norte_proto::TaskState,
        Box<Result<norte_proto::methods::SyncReportResult, Error>>,
    ),
    /// The daemon rejected the plan: there will be no Task and no panel.
    PlanDeSyncFallido(u64),
    /// The bytes of the checksums file about to be checked (#311).
    FicheroDeSumas(Box<VPath>, Box<Result<Vec<u8>, Error>>),
    /// The digests that Task computed, with the STATE it finished with
    /// (#311): a cancelled Task's report is partial, and comparing it would
    /// accuse files nobody got around to reading.
    InformeDeSumas(
        norte_proto::TaskId,
        norte_proto::TaskState,
        Box<Result<norte_proto::methods::FsChecksumReportResult, Error>>,
    ),
    /// A plan event: a batch of steps, or its closing.
    EventoDeSync(u64, Box<norte_client::SyncPlanEvent>),
    /// A batch of compared rows.
    FilasComparadas(u64, Box<norte_proto::methods::CompareRowsBatch>),
    /// What the model proposed, with the epoch of the request that asked
    /// for it.
    PlanIa(
        u64,
        Box<Result<norte_proto::methods::AiRenamePlanResult, Error>>,
    ),
    /// The ORGANIZE plan a producer proposed (phase 8), with the epoch of
    /// the request that asked for it. One single one for the model and for
    /// a plugin: both produce the same plan and the same review.
    PlanOrganizar(
        u64,
        Box<Result<norte_proto::methods::AiOrganizePlanResult, Error>>,
    ),
    /// The core's verdict on that plan, with the SAME epoch: between
    /// requesting one and the other the reader may have discarded the
    /// review, and a verdict on a plan no longer on screen does not apply.
    PlanDeLote(
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
    Estilo(
        RequestToken,
        Option<norte_proto::methods::PluginPreviewStyled>,
    ),
    /// The THUMBNAIL a plugin gave of the viewer's file (ADR 0107), or
    /// `None` if none matched or the one that matched did not know how.
    Miniatura(RequestToken, Option<norte_proto::methods::PluginThumbnail>),
    /// This epoch's search already has a Task: this is its id.
    ///
    /// Arrives on its own and not inside the first batch because there may
    /// be no first batch: the core sends no empty batches.
    BusquedaViva(u64, norte_proto::TaskId),
    /// This epoch's search never made it to being enqueued, and with which
    /// error.
    ///
    /// There is no Task there, so the outcome cannot arrive through
    /// progress: without this the view kept saying "searching…" forever
    /// about a search that does not exist, while the error passed through
    /// the status bar and the next key swept it away.
    BusquedaRota(u64, Box<Error>),
    /// The volumes, requested by the SIDE PANEL.
    ///
    /// Separate from the selector's for the same reason as the two plugin
    /// catalogs: they are two surfaces with two lifetimes.
    SitiosVolumenes(Result<Vec<norte_proto::methods::Volume>, Error>),
    /// The volumes for the listings' FOOTER (spec 2026-09-10). Separate
    /// from places' and the selector's for the same reason: a different
    /// lifetime, and it arrives with nobody having opened anything.
    VolumenesDePie(Result<Vec<norte_proto::methods::Volume>, Error>),
    /// A TREE branch's subdirectories, already filtered and sorted.
    ///
    /// `None` = the branch would not let itself be read; decided by
    /// `Tree::branch_unreadable` (empty, or re-anchor if it was the root).
    RamasDeArbol(VPath, Option<Vec<VPath>>),
    /// A pane's session closed (#140): the slot, how it went, and where
    /// that pane goes now.
    ///
    /// The destination travels INSIDE the message because it was decided
    /// before releasing the session: afterward, the pane's path no longer
    /// works to choose it.
    Desconectada(u32, Result<bool, Error>, VPath),
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
        let instance = InstanceId::new(nueva_instancia());
        let (updates, _) = broadcast::channel(UPDATE_BUFFER);
        // NATIVE effects go through their own channel: they carry paths and
        // go to the hosting process, not to the webview. A small buffer
        // because they are one person's gestures — copying a path, opening
        // a file — and not a stream: if it ever filled up, what is lost is
        // a gesture that can be repeated, not a piece of the screen.
        let (nativos, _) = broadcast::channel(16);
        let (tx, rx) = mpsc::channel(INBOX);
        // The actor keeps a return address to ITS OWN mailbox: that is
        // where slow answers come back through.
        let tx2 = tx.clone();

        let (mut estado, backend) = Estado::nuevo(instance.clone(), options);
        if estado.huecos.is_empty() {
            return Err(UiError::NoBrowserSlot);
        }
        // The session first: it says WHERE each slot was, and listing
        // before that would be bringing in a directory only to drop it.
        estado.leer_sesion(backend.as_ref()).await;
        // What the session said about this window — owner or loose — goes
        // to the status bar from the first frame: the change is discarded
        // because the startup snapshot carries the whole status bar.
        let _ = estado.cambio_de_banners();
        // And afterward `[profile.start]`, OUTSIDE `leer_sesion` on purpose:
        // that one returns early through four paths — no session, from a
        // future version, revision 0, unreadable body — and three of those
        // are exactly the case the key exists for: a fresh install, or a
        // profile copied from another machine (ADR 0098). Inside, it never
        // seeded anything.
        //
        // The order IS the precedence: the session, then what the profile
        // says about slots it does not know, and on top of that the
        // directory a human just typed.
        for (id, destino) in estado.siembra_de_perfil() {
            if let Some(hueco) = estado.huecos.get_mut(&id) {
                hueco.pane.begin_loading(destino);
            }
        }
        estado.fijar_dir_pedido();
        // The first listing is requested BEFORE publishing anything:
        // snapshot 0 describes a screen that already exists, not a promise.
        estado.listar_inicial(&backend, &tx2).await;
        // The side panel, if the layout places one: favorites come from the
        // already-loaded configuration, and volumes are REQUESTED and
        // arrive later — asking about them mounts and queries space on every
        // filesystem, and the window does not wait for that to paint.
        if estado.hueco_de_sitios().is_some() {
            estado.sembrar_sitios();
            estado.pedir_sitios(&backend, &tx2);
        }
        // And which PANES the plugins contribute (phase 3). Without asking
        // whether there is a slot for one: the saved layout can bring one
        // and that slot does not get placed until its kind is declared. The
        // only gate is the effects one, put there by `pedir_paneles`.
        //
        // Arrives after the first snapshot, like the volumes: declaring a
        // kind repaints, and waiting for an RPC to show the screen would be
        // paying for something that is almost never there.
        Estado::pedir_paneles(&backend, &tx2);
        // And what is already visible is probed: the local listing carries
        // no size or date (#52), so without this the first screen is born
        // with two blank columns that do not fill in until something moves
        // it.
        let visibles: Vec<u32> = estado.huecos.keys().copied().collect();
        for slot in visibles {
            estado.sondear(slot, &backend, &tx2);
        }
        let primero = estado.snapshot();

        bombear_tic_de_sesion(tx.clone());

        bombear_canales_del_backend(backend.as_ref(), &tx, estado.efectos);
        let host = Self {
            inbox: tx,
            updates: updates.clone(),
            nativos: nativos.clone(),
            instance,
        };
        estado.escritorio.nativos = Some(nativos);
        tokio::spawn(actor(rx, estado, backend, updates, tx2));
        Ok((host, primero))
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
            .send(Mensaje::Accion(Box::new(action), tx))
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
            .send(Mensaje::BytesDeImagen(tx))
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
        self.nativos.subscribe()
    }

    /// Shuts down the host and reports what was left unfinished.
    ///
    /// # Errors
    /// [`UiError::Down`] if it was already shut down.
    pub async fn shutdown(&self) -> Result<ShutdownReport, UiError> {
        let (tx, rx) = oneshot::channel();
        self.inbox
            .send(Mensaje::Apagar(tx))
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
    mut rx: mpsc::Receiver<Mensaje>,
    mut estado: Estado,
    backend: Arc<dyn HostBackend>,
    updates: broadcast::Sender<BridgeEnvelope<UiUpdate>>,
    buzon: mpsc::Sender<Mensaje>,
) {
    while let Some(msg) = rx.recv().await {
        match msg {
            Mensaje::Accion(accion, responde) => {
                let (ack, salidas) = estado.aplicar(&accion, &backend, &buzon);
                for u in salidas {
                    // With no subscribers it is not an error: the host is
                    // still alive even if the renderer has gone off to do
                    // something else.
                    let _ = updates.send(u);
                }
                let _ = responde.send(ack);
            }
            Mensaje::BytesDeImagen(responde) => {
                let _ = responde.send(estado.imagen.clone());
            }
            Mensaje::Catalogo(datos) => {
                let (scheme, catalogo) = *datos;
                let _ = updates.send(estado.aplicar_catalogo(scheme, catalogo));
            }
            Mensaje::Aprobacion(req) => {
                // And through the DESKTOP if the window is not up front
                // (#285). It is the notice that justifies the mechanism: an
                // approval expires on its own if nobody answers, so not
                // finding out changes the outcome — unlike a copy, which
                // stays finished when you come back.
                estado.avisar_de_aprobacion(&req);
                for u in estado.abrir_aprobacion(&req, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::AprobacionCaducada(approval_id) => {
                for u in estado.caduca_aprobacion(approval_id) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::AprobacionNoEntregada(approval_id, clave) => {
                // NAMES the approval (#279): with two stacked, "the approval
                // did not arrive" does not say which of the two, and they
                // are security decisions over different operands.
                for u in estado.decir_con(clave, &[("id", &approval_id.to_string())]) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Listado(datos) => {
                for u in estado.aterrizar_listado(*datos, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::RegistroTic(epoca) => {
                for u in estado.tic_de_registro(epoca, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::RegistroRemoto(epoca, res) => {
                for u in estado.aterrizar_registro_remoto(epoca, *res) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::RegistroNivel(epoca, res) => {
                for u in estado.aterrizar_nivel_remoto(epoca, *res) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::SecretoEntregado(datos) => {
                let (slot, dir, res) = *datos;
                for u in estado.secreto_entregado(slot, &dir, res, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Contenido(datos) => {
                let (token, path, leido, preview) = *datos;
                if let Some(u) = estado.abrir_visor(token, path, leido, preview, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::PreviewContenido(datos) => {
                let (slot, (token, path, leido, preview)) = *datos;
                if let Some(u) = estado.aterrizar_preview(slot, token, path, leido, preview) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::PanelContenido(datos) => {
                let (slot, token, res) = *datos;
                if let Some(u) = estado.aterrizar_panel(slot, token, res) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::MapaContenido(datos) => {
                let (slot, token, res) = *datos;
                if let Some(u) = estado.aterrizar_mapa(slot, token, res) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Fondo(f) => {
                for u in estado.aplicar_de_fondo(*f, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Hidratado(datos) => {
                if let Some(u) = estado.aterrizar_sondas(*datos, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::MasEntradas(datos) => {
                for u in estado.aterrizar_lote(*datos, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Conexion(ev) => {
                for u in estado.cambio_de_conexion(ev, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Degradada(d) => {
                let _ = updates.send(estado.sesion_degradada(*d));
            }
            Mensaje::Fallida(f) => {
                for u in estado.conexion_fallida(&f) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::AvisoPlugin(n) => {
                for u in estado.aviso_de_plugin(&n) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::TaskNueva(task) => {
                let (task, afectados, reintento) = *task;
                for u in estado.registrar_task(task, afectados, reintento, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Capacidades(slot, dir, caps) => {
                if let Some(u) = estado.aplicar_capacidades(slot, &dir, caps) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::TaskFallida(e) => {
                for u in estado.task_fallida(&e) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::TaskDeLoteRechazada(e) => {
                for u in estado.rechazo_de_lote(&e) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Progreso(p) => {
                for u in estado.progreso(&p, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::TaskCaducada(id, epoca) => {
                for u in estado.caducar_task(id, epoca, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Decir(clave) => {
                for u in estado.decir(clave) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Tira => {
                for u in estado.despertar_tira(&backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::TemaPersistido(fallo) | Mensaje::AnchoPersistido(fallo) => {
                if let Some(clave) = fallo {
                    for u in estado.decir(clave) {
                        let _ = updates.send(u);
                    }
                }
            }
            Mensaje::TerminalTic(epoca) => {
                for u in estado.terminal_tic(epoca, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::SesionTic => {
                estado.empujar_sesion(&backend, &buzon);
                // And one more second for the status bar's notice (spec
                // 2026-09-10).
                if let Some(u) = estado.caducar_aviso() {
                    let _ = updates.send(u);
                }
            }
            Mensaje::SesionPuesta(datos) => {
                let (res, cuerpo) = *datos;
                for u in estado.sesion_puesta(res, cuerpo, &backend, &buzon) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Relevado { soltada } => {
                for u in estado.relevo_terminado(soltada) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::SesionReleida(res) => {
                for u in estado.sesion_releida(res) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::TemaResuelto(datos) => {
                let (spec, resultado) = *datos;
                match resultado {
                    Ok(tema) => {
                        estado.tema_puesto(&spec, &tema);
                        // Snapshot and not a patch: changing theme moves the
                        // colors of the WHOLE screen, and the renderer plugs
                        // them back in from the catalog, not from a view
                        // field.
                        let snap = estado.snapshot();
                        let _ = updates.send(estado.sobre(UiUpdate::Snapshot(Box::new(snap))));
                    }
                    // A theme that cannot be read does NOT leave the window
                    // with no colors: it stays on the one there was and
                    // reports why.
                    Err(clave) => {
                        for u in estado.decir(clave) {
                            let _ = updates.send(u);
                        }
                    }
                }
            }
            Mensaje::Informe(informe) => {
                let (epoca, task_id, cual) = *informe;
                for u in estado.informe(epoca, task_id, &cual) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::CreadoComprobado(comprobado) => {
                let (path, veredicto) = *comprobado;
                for u in estado.abrir_lo_comprobado(path, veredicto) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::FavoritoPersistido(hecho) => {
                let (nombre, destino, fallo) = *hecho;
                for u in estado.favorito_persistido(&nombre, Some(destino), fallo) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::FavoritoQuitado(hecho) => {
                let (nombre, fallo) = *hecho;
                for u in estado.favorito_persistido(&nombre, None, fallo) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::PerfilGuardado(hecho) => {
                let (nombre, fallo) = *hecho;
                for u in estado.perfil_guardado(&nombre, fallo) {
                    let _ = updates.send(u);
                }
            }
            Mensaje::Apagar(responde) => {
                let informe = estado.apagar(backend.as_ref()).await;
                let _ = updates.send(estado.sobre(UiUpdate::Notice(UiNotice::Shutdown {
                    incomplete: informe.incomplete,
                })));
                let _ = responde.send(informe);
                return;
            }
        }
        // The docked viewer follows the cursor (#291), and the cursor is
        // moved by any message: a key, a listing landing, a panel opening.
        // It is asked AFTER each one, the way the TUI asks it every frame:
        // what each placed preview slot should be showing, and if it is not
        // what it shows, it is requested.
        for u in estado.sondear_previews(&backend, &buzon) {
            let _ = updates.send(u);
        }
        // And a plugin's pane (phase 3), for the same reason and in the same
        // place: its guest receives the directory and the row under the
        // cursor, so any message can change what it should be showing.
        for u in estado.sondear_paneles(&backend, &buzon) {
            let _ = updates.send(u);
        }
        // And the disk map (phase 4), in the same place and for the same
        // reason: it follows the DIRECTORY of the listing it is tied to, so
        // a `cd` — wherever it comes from — changes what it should be
        // showing. It does not follow the cursor: moving a row does not
        // change what the directory is made of, and probing per cursor
        // would mean measuring a `$HOME` on every arrow.
        for u in estado.sondear_mapas(&backend, &buzon) {
            let _ = updates.send(u);
        }
        // And the timeline (#359): the first page when its slot appears,
        // and the next one when the cursor reaches the bottom.
        estado.sondear_lineas(&backend, &buzon);
        // And the attribute sheet, for the SAME reason and in the same
        // place: it also follows the cursor and also has no other path to
        // the renderer. It goes after the viewer so that, when both change
        // at once, the snapshot sent already carries both up to date.
        for u in estado.sondear_hojas() {
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
fn identidad_de_columna(id: &norte_frontend::columns::ColumnId) -> String {
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
fn es_listado(arbol: &Node, slot: SlotId, kinds: &KindRegistry) -> bool {
    kind_de(arbol, slot).is_some_and(|k| k.as_str() == "browser" && kinds.get(&k).is_some())
}

/// A tree slot's declared kind.
fn kind_de(arbol: &Node, slot: SlotId) -> Option<norte_frontend::layout::KindId> {
    fn buscar(n: &Node, slot: SlotId) -> Option<norte_frontend::layout::KindId> {
        match n {
            Node::Slot { id, kind, .. } if *id == slot => Some(kind.clone()),
            Node::Slot { .. } => None,
            Node::Split { children, .. } | Node::Tabs { children, .. } => {
                children.iter().find_map(|c| buscar(c, slot))
            }
        }
    }
    buscar(arbol, slot)
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
fn politica_de_colision(choice: &str) -> Option<norte_proto::CollisionPolicy> {
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
fn lanzar_aprobacion(
    approval_id: u64,
    backend: &Arc<dyn HostBackend>,
    buzon: &mpsc::Sender<Mensaje>,
) {
    let backend = Arc::clone(backend);
    let buzon = buzon.clone();
    tokio::spawn(async move {
        if let Err(e) = backend.policy_decide(approval_id, true).await {
            let _ = buzon
                .send(Mensaje::AprobacionNoEntregada(
                    approval_id,
                    clave_de_aprobacion_perdida(&e),
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
fn clave_de_aprobacion_perdida(e: &norte_proto::Error) -> &'static str {
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
fn clase_de_task(kind: norte_proto::TaskKind) -> &'static str {
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
/// the listing into an error. And `superado` cuts off between RPCs, because
/// a batch the listing has already superseded has no reason to spend the
/// ones it has left.
async fn celdas_de_plugin(
    backend: &Arc<dyn HostBackend>,
    pedidas: &[(String, String)],
    paths: &[VPath],
    superado: impl Fn() -> bool,
) -> (
    std::collections::HashMap<String, std::collections::HashMap<VPath, String>>,
    std::collections::BTreeMap<String, String>,
) {
    let mut out = std::collections::HashMap::new();
    let mut rotulos = std::collections::BTreeMap::new();
    if pedidas.is_empty() {
        return (out, rotulos);
    }
    let Ok(lista) = backend.plugin_list().await else {
        return (out, rotulos);
    };
    for (plugin, columna) in
        norte_frontend::columns::validated_plugin_requests(pedidas, &lista.plugins)
    {
        // The LABEL the manifest gave it, so the header does not say the id
        // (`acme.git/status`). A plugin's text: it is masked and clamped
        // like any header. A manifest leaving it empty stays with no label
        // and the header falls back to the id, as before.
        if let Some(h) = lista
            .plugins
            .iter()
            .find(|p| p.id == plugin)
            .and_then(|p| p.columns.iter().find(|c| c.id == columna))
        {
            let sano: String = norte_frontend::columns::sanitize_header(&h.header)
                .chars()
                .take(norte_frontend::columns::HEADER_MAX_CHARS)
                .collect();
            if !sano.is_empty() {
                rotulos.insert(
                    norte_frontend::columns::plugin_display_id(&plugin, &columna),
                    sano,
                );
            }
        }
        if superado() {
            return (out, rotulos);
        }
        let crudos = backend
            .plugin_column_values(plugin.clone(), columna.clone(), paths.to_vec())
            .await
            .unwrap_or_default();
        let sanos = norte_frontend::columns::sanitize_column_values(paths, &crudos);
        out.insert(
            norte_frontend::columns::plugin_display_id(&plugin, &columna),
            sanos,
        );
    }
    (out, rotulos)
}

/// What a column is called in the selector, and whether that differs from
/// reality.
///
/// Through `header_label`, the SAME function that paints the listing's
/// header: what a column is called cannot depend on where it is read from.
/// An id that does not parse is shown as is — it is the user's configuration
/// intent and the selector never cleans it up — and that is why it is masked
/// too.
fn etiqueta_de_columna(
    r: &norte_frontend::columns_picker::PickerRow,
    esquema: &str,
    columnas: &norte_frontend::columns::ColumnsSettings,
    lang: norte_i18n::Lang,
) -> (String, bool) {
    use norte_frontend::columns::{ColumnId, header_label_in};
    let Ok(cid) = r.id.parse::<ColumnId>() else {
        // Does not parse: the raw id is all that can be shown, and it is
        // text from a configuration file.
        return norte_frontend::display_name(r.id.as_bytes());
    };
    let estilo = columnas.style_for_id(esquema, &cid, None);
    norte_frontend::display_name(header_label_in(&cid, &estilo, None, lang).as_bytes())
}

/// A text IDENTITY that crosses the bridge: whole, or empty.
///
/// Same rule as [`identidad_de_columna`] and for the same reason (ADR 0061):
/// trimming is not injective, and a trimmed key matches the wrong one.
fn identidad_de_texto(id: &str) -> String {
    if id.len() > crate::bridge::MAX_STRING_BYTES {
        return String::new();
    }
    id.to_owned()
}

fn ahora_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// An instance's unique identity: pid plus the startup instant. It does not
/// need to be unpredictable — it authorizes nothing — only different from
/// the process's previous life.
fn nueva_instancia() -> String {
    let ahora = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("host-{}-{ahora}", std::process::id())
}

/// A listing open in a slot.
///
/// The listing's state does NOT belong to this crate: it is
/// [`norte_frontend::pane::PaneState`], the same one the TUI uses. Cursor,
/// marks, hidden ones, per-directory cursor memory and the listing's EPOCH
/// come from there, so the two surfaces cannot diverge on what "move the
/// cursor down" means (ADR 0066, D14).
struct Hueco {
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
    esquema_del_orden: String,
    /// Where I come from and where I go back to. Also shared.
    historial: History,
    primera_visible: u64,
    visibles: u32,
    /// The listing request IN FLIGHT, if any. An answer with a different
    /// token arrived late: it is discarded here, in Rust, not hidden in the
    /// renderer.
    en_vuelo: Option<RequestToken>,
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
    dir_pedido: Option<VPath>,
    /// The marks that need to be set again when a REFRESH lands.
    ///
    /// Empty whenever what is in flight is a navigation: there the rows
    /// belong to another directory and a mark means nothing. Consumed on
    /// landing.
    marcas_a_restaurar: Vec<VPath>,
    /// The row the SESSION left under the cursor, until its listing arrives.
    ///
    /// Waits for the same reason as the marks: over an empty pane, putting
    /// the cursor on row 12 is putting it on row 0. Consumed on the first
    /// landing — good or bad — so it never falls on a later listing from
    /// somewhere else. It is an INDEX, the same one the terminal saves and
    /// restores: a handoff has to land on the same row in both directions.
    cursor_a_restaurar: Option<usize>,
    /// There are merged rows not yet published.
    ///
    /// Filling stays quiet when the merging batch does not change the
    /// visible window (#252), but every merge bumps the listing's EPOCH and
    /// the renderer names rows by epoch: if ALL patches stay quiet, it is
    /// left on a stale epoch and its clicks are rejected as outdated. This
    /// flag is the debt, and the last batch settles it.
    filas_por_publicar: bool,
    /// The drain still bringing batches in from behind, if any.
    ///
    /// SEPARATE from `en_vuelo` because they are two different lifetimes:
    /// the first page lands and clears `en_vuelo`, but the rest of the
    /// stream keeps arriving. Sharing a token made `aplicar_lote` reject ALL
    /// of a navigation's batches — a five-thousand-entry directory got
    /// stuck at a hundred — and startup had to restore it by hand to work.
    drenando: Option<RequestToken>,
    estado: SlotState,
    /// There is a probe batch in flight for this slot.
    sondeando: bool,
    /// Raised when the listing changes: whatever comes back from the batch
    /// in flight no longer describes this screen.
    cancelar_sondeo: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The paths already probed (whether they answered or not). Without
    /// this memory, a failing `stat` gets requested again on every repaint
    /// and probing turns into a loop against the daemon.
    sondeados: std::collections::HashSet<VPath>,
    /// The decoration plugins put on each path.
    ///
    /// By PATH and not by index: decorations arrive asynchronously and the
    /// listing reorders underneath, so an index would name a different row
    /// by the time they land.
    adornos: std::collections::HashMap<VPath, norte_frontend::Decoration>,
    /// Los valores de cada columna `plugin:`, por id de columna y ruta.
    celdas_plugin: std::collections::HashMap<String, std::collections::HashMap<VPath, String>>,
    /// Hay una tanda de decoración en vuelo para este hueco.
    adornando: bool,
    /// El directorio al que va una navegación que CUENTA como paso, hasta que
    /// su listado llegue (spec 2026-09-15 D6): entonces se suma a los
    /// populares, y si falla se olvida. Lo relevan la siguiente navegación del
    /// hueco y el propio aterrizaje.
    visita_pendiente: Option<VPath>,
    /// Las rutas que ya se pidieron decorar (hayan contestado o no). Misma
    /// memoria que `sondeados` y por el mismo motivo: sin ella, un plugin
    /// que no decora nada se vuelve a preguntar en cada repintado.
    adornadas: std::collections::HashSet<VPath>,
    /// La generación de los adornos: sube cada vez que se OLVIDAN. Una
    /// tanda en vuelo lleva la suya, y si aterriza con otra se tira y se
    /// vuelve a pedir: apagar un decorador desde el gestor con una tanda a
    /// medio camino dejaba sus insignias pegadas a las filas.
    gen_adornos: u64,
}

/// Un diálogo abierto y lo que hará si se confirma.
struct Dialogo {
    id: ModalId,
    vista: DialogView,
    /// Lo que la confirmación ejecuta. `None` = solo informa.
    al_confirmar: Option<Pendiente>,
    /// Lo que el usuario tecleó, TAL CUAL.
    ///
    /// Separado de `vista.input`, que es su proyección para pintar —
    /// enmascarada y acotada—, porque este texto acaba siendo un NOMBRE DE
    /// FICHERO. Pasarlo por el recorte de pantalla creaba directorios con
    /// una elipsis dentro: el mismo error que ADR 0061 decidió no volver a
    /// cometer, en miniatura.
    ///
    /// Un enum y no un `String` desde #327: hay un diálogo que pide una
    /// CONTRASEÑA, y guardarla aquí como texto normal la mandaría a un
    /// `Debug`, al heap sin pisar, y —lo peor— a la proyección de pintado por
    /// el mismo camino que un nombre de fichero. Con dos formas, quien escriba
    /// tiene que decir cuál es.
    tecleado: Tecleado,
    /// Este diálogo se ABRIÓ SOLO, y todavía no se le ha reconocido.
    ///
    /// Una aprobación y el informe de un lote aparecen sin que nadie acabe de
    /// pulsar nada: llegan cuando el daemon contesta, encima de lo que el
    /// lector estuviera haciendo, y se quedan la entrada. Con esto, la
    /// primera RESPUESTA solo dice «ya lo veo» —la misma regla que la
    /// revisión de un plan, y por el mismo motivo—.
    ///
    /// Respuesta, no tecla: la comprobación vive en `responder_dialogo`, que
    /// es por donde pasan las dos entradas. Cuando solo cubría el teclado, un
    /// clic ya en marcha sobre el «Confirmar» de una confirmación aterrizaba
    /// sobre el «Aprobar» de una aprobación de agente que acababa de
    /// pintarse en el mismo sitio.
    ///
    /// Denegar y cancelar están exentos, igual que `Escape`: quitarse de
    /// encima algo que uno no ha pedido tiene que salir a la primera.
    ///
    /// `true` en un diálogo que abrió un gesto: ahí la respuesta siguiente SÍ
    /// es una respuesta, porque la pregunta la hizo quien está delante.
    reconocido: bool,
}
/// Lo tecleado en el campo de un diálogo, según lo que sea.
///
/// Dos formas y no un `String` porque el trato es DISTINTO y la diferencia no
/// puede quedar a criterio de quien llama: un nombre de fichero se pinta
/// enmascarado, una contraseña se pinta como puntos y no se pinta nunca. Con
/// un solo tipo, la única barrera era acordarse — y el modo de fallo era una
/// contraseña saliendo por el mismo `display_name` que un nombre, o dentro de
/// un `Debug` del estado entero.
#[derive(Debug)]
enum Tecleado {
    /// Un nombre, una instrucción, una plantilla: texto que se enseña.
    Texto(String),
    /// Una contraseña, y el host NO la tiene mientras se escribe.
    ///
    /// Sin datos dentro, y eso es la decisión: el campo lo enmascara el
    /// `input type=password` del renderer, así que aquí no hay nada que contar
    /// ni que pintar, y la contraseña cruza una sola vez —con la respuesta,
    /// en `UiAction::Dialog::secret`— en el instante en que el lector decide
    /// entregarla. Lo que el host no tiene no se le puede escapar por un
    /// `Debug`, por una foto ni por un log.
    ///
    /// La variante existe igual porque es la barrera de TIPO: `texto()`
    /// devuelve nada sobre ella, así que una pendiente de texto que aterrizara
    /// por error sobre este diálogo no puede leer un secreto — no hay ninguno.
    Secreto,
    /// Un FORMULARIO: varios campos a la vez (puente 91).
    ///
    /// El modelo es el COMPARTIDO (`norte_frontend::search::SearchForm`), no
    /// uno de esta ventana: el terminal pregunta la misma búsqueda, y dos
    /// modelos divergen en silencio — que es lo que ya pasó con el desenlace
    /// de una búsqueda (ADR 0077).
    ///
    /// `Box` porque es el doble de grande que las otras dos variantes juntas
    /// y hay un `Tecleado` por diálogo abierto.
    Formulario(Box<norte_frontend::search::SearchForm>),
}

/// El tope de una contraseña, del crate COMPARTIDO: lo que se rechaza aquí es
/// exactamente lo que aquel puede guardar sin reasignar.
use norte_frontend::secret::SECRET_MAX_CHARS;

impl Tecleado {
    /// El texto, para las pendientes que trabajan con texto.
    ///
    /// Vacío para un secreto, a propósito: si alguna vez una pendiente de
    /// texto acabara sobre un diálogo de contraseña, lo que recibe es nada.
    /// Un `panic!` sería peor —tumbar la ventana por un error de cableado— y
    /// aquí no hay nada más que devolver.
    fn texto(&self) -> &str {
        match self {
            Self::Texto(s) => s,
            // Un formulario no tiene «el» texto: tiene siete campos, y una
            // pendiente de texto que aterrizara aquí por error no puede
            // llevarse uno cualquiera haciéndolo pasar por el que pidió.
            Self::Secreto | Self::Formulario(_) => "",
        }
    }
}

/// Lo que un diálogo tiene pendiente de hacer.
enum Pendiente {
    /// Borrar estas entradas, a la papelera o permanentemente.
    Borrar {
        /// Qué se borra, en orden de listado.
        paths: Vec<VPath>,
        /// Permanente (sin papelera): el diálogo lo AVISA.
        permanente: bool,
    },
    /// Copiar al panel lo que se SOLTÓ desde el escritorio (#283).
    ///
    /// Separada de [`Pendiente::Transferir`] por dos razones, y ninguna es
    /// cosmética: aquí los orígenes no salen de ningún panel —así que no hay
    /// marcas que consumir, y consumirlas borraría una selección que el lector
    /// hizo para otra cosa—, y el verbo es siempre COPIAR: mover lo que
    /// arrastró otra aplicación significaría borrarlo de donde ese proceso lo
    /// tenga, y esta ventana no ha preguntado eso.
    Soltar {
        /// Qué llegó, ya convertido y filtrado.
        paths: Vec<VPath>,
        /// Dónde cae, que es el directorio del panel activo cuando se soltó.
        destino: VPath,
    },
    /// Preguntar al índice por SIGNIFICADO. Lo que se teclea es la consulta,
    /// y no lleva más operandos: el alcance es el índice entero.
    ConsultaSemantica,
    /// CERRAR la ventana, ya confirmado (`[ui] confirm_quit`).
    Salir,
    /// Entregar el secreto de una conexión y REINTENTAR la navegación que
    /// `Error::SecretNeeded` interrumpió (#325/#327).
    ///
    /// Lleva a dónde iba el panel porque esta pendiente es el único sitio de
    /// esta ventana donde una navegación sobrevive a la respuesta que la
    /// interrumpió: el listado ya volvió con error, y el hueco se quedó
    /// enseñando el directorio que abandonaba. Sin el destino aquí, entregar
    /// el secreto dejaría al lector con la contraseña dada y el panel donde
    /// estaba.
    EntregarSecreto {
        /// Nombre de la entrada de `connections.toml` que lo pide — la MISMA
        /// cadena que va en `connection.provide_secret`. Sale del error del
        /// core, no del servidor remoto.
        conn: String,
        /// El hueco que estaba navegando.
        slot: u32,
        /// A dónde reintentar.
        dir: VPath,
    },
    /// Marcar —o desmarcar— por patrón. Lo que se teclea es el glob.
    Patron {
        /// `true` añade marcas, `false` las quita.
        marcar: bool,
    },
    /// Volver a intentar una transferencia que CHOCÓ, con otra política
    /// (#274).
    ///
    /// La pregunta no es si seguir: es CUÁL de las cuatro salidas, así que la
    /// política sale del `choice` que el lector pulsó y no de aquí. Cancelar
    /// es no elegir ninguna, y entonces la task fallida se queda como estaba —
    /// que es lo que pasaba siempre antes de esto.
    Reintentar {
        /// Con qué se relanza.
        con: Reintento,
    },
    /// Partir el fichero bajo el cursor en trozos del tamaño que se teclea
    /// (#132).
    Partir {
        /// Qué se parte.
        path: VPath,
        /// Dónde caen los trozos. El panel DESTINO, como una copia: partir un
        /// fichero de un giga donde ya está suele no caber.
        dest_dir: VPath,
    },
    /// Empaquetar lo MARCADO en el contenedor que se teclea (#132).
    ///
    /// Lleva el directorio y no el nombre: el nombre es lo que el lector
    /// escribe, y de él sale el formato. Se resuelve al confirmar, no al
    /// abrir, porque hasta entonces no hay nada que resolver.
    Empaquetar {
        /// Dónde cae el contenedor y, además, la BASE de los nombres que se
        /// guardan dentro: quien desempaquete espera ver lo que veía en
        /// pantalla, no rutas absolutas.
        dir: VPath,
        /// Qué se mete, en orden de listado.
        sources: Vec<VPath>,
    },
    /// Copiar al portapapeles la lista de sumas que el diálogo enseña (#311).
    ///
    /// Los BYTES ya montados —con el escapado de coreutils— y no las filas:
    /// lo que se pinta va saneado, y copiar eso daría un `SHA256SUMS` que no
    /// comprueba los ficheros que nombra.
    CopiarSumas {
        /// Lo que va al portapapeles, tal cual.
        bytes: Vec<u8>,
    },
    /// Cambiar los PERMISOS de estas entradas al modo que se teclee (#314).
    ///
    /// Las rutas se congelan al ABRIR el diálogo, como en el resto de los que
    /// llevan operando: entre la pregunta y el sí el listado puede refrescarse,
    /// y entonces «lo marcado» sería otra cosa.
    Permisos {
        /// Sobre qué, en orden de listado.
        targets: Vec<VPath>,
    },
    /// Deshacer TODO lo que hizo una sesión de agente (#276).
    DeshacerSesion {
        /// La clave OPACA con la que el core la resuelve, cruda.
        sesion: String,
    },
    /// Deshacer lo del humano POSTERIOR a un punto de la línea de tiempo
    /// (#359, `journal.undo_after`). La fila señalada se queda.
    DeshacerHasta {
        /// El corte: el `seq` más nuevo de la fila señalada.
        seq: i64,
        /// El techo: lo más nuevo que el recuento contó (`upto_seq`). Se
        /// congela al PREGUNTAR, como los operandos de cualquier diálogo.
        techo: Option<i64>,
    },
    /// Conceder las capabilities de una extensión.
    ///
    /// Es la única de las cuatro operaciones del gestor que PREGUNTA:
    /// revocar, encender y apagar van en la dirección segura y no hacen
    /// falta dos gestos. La pregunta enumera las capabilities una por línea
    /// —fuera de la frase, como cualquier operando de este host— porque
    /// «aprobar org.ejemplo.foo» sin decir qué concede no es una decisión.
    AprobarExtension {
        /// A quién se le conceden.
        id: String,
        /// QUÉ se enseñó al preguntar, en el orden en que se enseñó.
        ///
        /// Se guarda para volver a comprobarlo al confirmar: el diálogo se
        /// queda las TECLAS, no los mensajes de fondo, así que un catálogo
        /// que aterrice entre la pregunta y el sí puede haber cambiado las
        /// capabilities de esa extensión — y entonces el sí concedería algo
        /// que nadie leyó. Si han cambiado, se vuelve a preguntar.
        capabilities: Vec<String>,
        /// El ancla del manifiesto TAL COMO ESTABA AL PREGUNTAR (#282).
        ///
        /// Aquí, y no releída al confirmar, por la misma razón que las
        /// capabilities de arriba: leerla en el momento del sí devolvería el
        /// ancla del catálogo que haya aterrizado mientras tanto, o sea que el
        /// host certificaría al core «esto es lo que el humano leyó» sobre
        /// algo que el humano no leyó. Y la comparación de capabilities no lo
        /// tapa: `category` y `contributions` —cuándo y cómo se dispara—
        /// entran en el ancla y NO en la lista que se pinta.
        digest: Option<String>,
    },
    /// Desinstalar una extensión (ADR 0104): borrar sus ficheros y retirar
    /// su consentimiento. Pregunta porque no tiene vuelta —no hay
    /// `plugin.install` por el wire— y porque un plugin instalado después
    /// bajo el mismo id nace sin la aprobación que este tenía.
    DesinstalarExtension {
        /// Cuál.
        id: String,
    },
    /// Decidir sobre una op de agente. La op real la tiene el daemon ligada
    /// al id: aquí solo viaja el sí o el no.
    Decidir {
        /// El id que el daemon espera de vuelta.
        approval_id: u64,
        /// La sesión de agente que la pidió, CRUDA, si la petición la traía.
        ///
        /// Cruda y no la del diálogo: lo que el diálogo pinta está
        /// enmascarado, y enmascarar no es inyectivo — usar eso como clave
        /// apuntaría el sí en la fila de otra sesión, o en ninguna.
        session: Option<String>,
    },
    /// Buscar por el subárbol de este directorio. Lo que se teclea es el
    /// patrón.
    Buscar {
        /// Dónde empieza el walk.
        root: VPath,
    },
    /// Crear un fichero VACÍO y abrirlo con el escritorio (#290).
    CrearFichero {
        /// Dónde se crea. El nombre es lo que se teclea.
        dir: VPath,
    },
    /// Guardar un FAVORITO que apunta aquí (#309). El nombre es lo que se
    /// teclea, y viene prellenado con la sugerencia compartida.
    ///
    /// Lleva el destino y no lo lee al confirmar: entre abrir el diálogo y
    /// aceptar, el panel puede haber navegado, y guardar «donde estoy ahora»
    /// haría un favorito que apunta a otro sitio que el que se estaba mirando
    /// cuando se pidió.
    GuardarFavorito {
        /// A dónde apunta el favorito.
        destino: VPath,
    },
    /// Guardar el espacio de trabajo como un perfil (#318, ADR 0079).
    ///
    /// No lleva nada: lo que se guarda es lo que se VE, y eso se lee al
    /// confirmar. La diferencia con el favorito de arriba no es un descuido —
    /// allí el destino es una respuesta a «¿qué estabas mirando?», y aquí la
    /// pregunta es «¿cómo está la pantalla?», que solo tiene sentido AHORA.
    GuardarPerfil,
    /// El valor de una entrada de TEXTO de los ajustes (F11). Lo que se
    /// teclea es el valor; `id` es sobre qué entrada se preguntó.
    ///
    /// Un ID y no una fila: el cursor puede moverse con el diálogo delante,
    /// y el buscador de detrás puede cambiar qué filas hay — una posición
    /// deja de nombrar la misma entrada, y confirmar escribiría el valor en
    /// otra.
    EditarAjuste {
        /// El id del catálogo sobre el que se preguntó.
        id: &'static str,
    },
    /// Crear un directorio dentro de este otro. El nombre lo teclea el
    /// usuario y se valida al confirmar, no al teclear: corregir un nombre a
    /// medias es peor que verlo rechazado al final.
    CrearDirectorio {
        /// Dónde se crea.
        dir: VPath,
    },
    /// Pedirle a un modelo un plan de renombrado para este directorio. Lo
    /// que se teclea es la INSTRUCCIÓN, no un nombre: no muta nada todavía.
    InstruccionIa {
        /// El directorio sobre el que planear.
        dir: VPath,
    },
    /// La PLANTILLA del renombrado en lote (#310). Lo que se teclea es una
    /// plantilla, no un nombre: el plan se genera aquí y se revisa antes de
    /// nada, como el de la IA.
    PlantillaLote {
        /// El directorio sobre el que planear.
        dir: VPath,
        /// Los nombres sobre los que actúa el lote: lo marcado, o el del
        /// cursor. Se fijan al abrir el prompt, como el operando de
        /// cualquier otra operación.
        nombres: Vec<String>,
    },
    /// Renombrar UNA entrada dentro de su propio directorio.
    ///
    /// Lleva la SIEMBRA del campo, no solo la ruta, y esa es la pieza que
    /// hace que la regla 1 se sostenga aquí: si lo que se confirma es
    /// EXACTAMENTE lo que se sembró, no se ha tocado nada y lo que viaja son
    /// los bytes de siempre. Comparar contra la siembra en vez de llevar un
    /// `bool` de «tocado» es lo que sobrevive a que el renderer devuelva el
    /// texto entero en cada evento en vez de un delta.
    Renombrar {
        /// La entrada que se renombra.
        from: VPath,
        /// Lo que se puso en el campo, TAL CUAL (la proyección pintable del
        /// nombre, que para un nombre que no es UTF-8 lleva un U+FFFD).
        siembra: String,
    },
    /// Copiar o mover estas entradas AL directorio de otro hueco.
    ///
    /// El destino viaja ya resuelto —el directorio del hueco con el rol
    /// `Target` en el momento de abrir el diálogo— y no como un id de hueco:
    /// entre abrir la pregunta y responderla el lector puede haber navegado
    /// ese panel, y entonces «el otro» sería otro sitio del que se enseñó.
    Transferir {
        /// El hueco del que salieron las marcas.
        ///
        /// Viaja por el mismo motivo que `destino`, y su ausencia era un
        /// bug: `UiAction::FocusSlot` NO está vedada mientras hay un diálogo
        /// abierto —solo lo están las teclas—, así que un clic en el otro
        /// panel entre la pregunta y la respuesta hacía que las marcas que se
        /// consumían fueran las de OTRO hueco. El de verdad se quedaba
        /// marcado, y el lector volvía a pulsar F5 sobre lo mismo.
        origen: u32,
        /// El DIRECTORIO del que salieron, tal como lo escribe el hueco.
        ///
        /// No se deriva del padre de cada entrada: el padre lo escribe el
        /// PROVIDER y el directorio del hueco puede venir de la config o de
        /// la sesión, así que en macOS (NFD contra NFC) o en un servidor sin
        /// distinción de caja son dos cadenas distintas para el mismo sitio
        /// — y el refresco por comparación byte a byte no encontraría el
        /// panel de origen (ADR 0061).
        origen_dir: VPath,
        /// Qué se transfiere, en orden de listado.
        paths: Vec<VPath>,
        /// El DIRECTORIO al que van. El nombre final se compone aquí, jamás
        /// en el renderer.
        destino: VPath,
        /// `true` = mover.
        mover: bool,
    },
}

/// Tope de lo que se lee de un fichero de sumas (#311): 1 MiB.
///
/// Por encima se RECHAZA en vez de comprobar media lista — el mismo criterio
/// que la terminal, y el mismo que el tope del otro extremo.
const SUMS_MAX_BYTES: u64 = 1024 * 1024;

/// Con qué se puede volver a intentar una transferencia que CHOCÓ (#274).
///
/// La ventana manda siempre `CollisionPolicy::Fail`, que es el default
/// seguro: sobrescribir o renombrar son decisiones del lector. Lo que faltaba
/// era dónde tomarlas — una task fallida y ningún camino hacia delante— y para
/// ofrecerlas hay que recordar QUÉ se pidió: el progreso de la task dice qué
/// fichero va por dentro, no cuál era el origen ni el destino.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reintento {
    /// El origen, tal cual se pidió.
    from: VPath,
    /// El destino EXACTO, con su nombre ya compuesto.
    to: VPath,
    /// Mover en vez de copiar: el reintento tiene que repetir el mismo verbo,
    /// o un «sobrescribir» sobre una copia se convertiría en un movimiento.
    mover: bool,
    /// La reinterpretación de nombres que había AL LANZAR.
    ///
    /// Se captura aquí y no se lee al llegar, y ese es el punto: la colisión
    /// llega ASÍNCRONA, encima de lo que el lector esté haciendo, y entre el
    /// envío y la pregunta cabe cambiar de hueco o ciclar la codificación. El
    /// diálogo tiene que pintar el MISMO texto por el que se navegó, o se
    /// está aprobando un nombre distinto del que se vio. El terminal lo lleva
    /// en su `RetrySpec` desde #98 y lo dice ahí con estas palabras.
    enc: Option<norte_encoding::NameEncoding>,
}

/// La clave Fluent de un error de io LOCAL.
///
/// La CLAVE y no el texto: el host localiza con SU idioma
/// (`norte_i18n::t_in`), no con el del proceso. Y jamás el `Display` del
/// sistema, que el SO traduce a su antojo — «Permission denied (os error
/// 13)» no es un mensaje de norte (#73).
fn clave_de_io(e: &std::io::Error) -> &'static str {
    match e.kind() {
        std::io::ErrorKind::NotFound => "err-not-found",
        std::io::ErrorKind::PermissionDenied => "err-permission-denied",
        std::io::ErrorKind::StorageFull => "err-no-space",
        _ => "err-io",
    }
}

/// El estado semántico. Solo el actor lo toca.
// Cuatro banderas INDEPENDIENTES entre sí: si la ventana tiene el foco, si el
// escritorio pide oscuro, si el journal se rehusó… Son estados de cosas
// distintas que coexisten, no los valores de una sola máquina, que es lo que
// el lint propone y aquí sería falso — plegarlas en enums de dos variantes
// daría cuatro enums, no uno.
#[expect(
    clippy::struct_excessive_bools,
    reason = "estado del controlador: banderas de cosas distintas, no una máquina"
)]
struct Estado {
    instance: InstanceId,
    sequence: u64,
    /// Contador de peticiones. Cada listado se lleva el suyo, y una
    /// respuesta con un testigo viejo se descarta.
    token: u64,
    locale: String,
    /// El resolver de teclas, con SU keymap efectivo dentro (mismo tipo y
    /// mismo contrato que el del TUI).
    resolver: Resolver,
    /// El keymap efectivo del VISOR, para que la paleta pueda decir el
    /// atajo de un comando de esa pantalla.
    efectivo_visor: Effective,
    /// La paleta de comandos, si está abierta.
    ///
    /// Es un contexto de entrada más, como el buscador incremental y el
    /// visor: mientras esté abierta, las teclas de texto son suyas.
    paleta: Option<norte_frontend::palette_state::Palette>,
    /// «Ir a cualquier sitio», si está abierto (#357). Otro contexto de
    /// entrada con texto libre, como la paleta.
    ir_a: Option<norte_frontend::goto::Goto>,
    /// Cuántas veces se ha abierto «ir a»: las conexiones y el índice que
    /// contesten a una apertura anterior se tiran.
    gen_ir_a: u64,
    /// La pregunta al índice en vuelo, si la hay. Cada tecla la ABORTA y
    /// lanza otra: escribir deprisa no deja tres preguntas vivas contra un
    /// proveedor que cuesta tiempo y puede costar dinero.
    ir_a_indice: Option<tokio::task::JoinHandle<()>>,
    /// El asistente de primer arranque (spec 2026-09-10), mientras está
    /// abierto. Un overlay más: se queda las teclas.
    asistente: Option<norte_frontend::wizard::Wizard>,
    /// La pantalla de arranque (spec 2026-09-15, ADR 0115), mientras está
    /// puesta. Una CAPA y no un overlay con teclas propias: cualquier tecla o
    /// clic la quita, y el asistente le gana.
    splash: Option<norte_frontend::splash::SplashView>,
    /// Cuándo deja de tapar el `brief`, en milisegundos de época. `None` = no
    /// caduca solo (`home`), o no hay pantalla puesta.
    splash_hasta_ms: Option<i64>,
    /// La pantalla de arranque ya se enseñó en ESTA sesión del host.
    ///
    /// El host sobrevive al webview —una recarga, un renderer que se
    /// reinicia—, y el renderer manda `splash_open` cada vez que arranca. Sin
    /// esta marca, recargar a media sesión tapaba lo que estabas mirando con
    /// una pantalla de bienvenida que en modo `home` se queda hasta que la
    /// toques.
    splash_visto: bool,
    /// El panel de procesos lo abrió el AUTOMÁTICO (`[ui] processes_panel`),
    /// así que el automático puede cerrarlo. Uno que abrió el lector se queda.
    procesos_auto: bool,
    /// La barra de progreso ligera del item `tasks` (ADR 0146).
    tira: norte_frontend::task_strip::TaskStrip,
    /// Las transferencias que se lancen van a la COLA (ADR 0149). De la
    /// sesión, no de la configuración: se enciende para un rato de mover
    /// cosas y se apaga después.
    encolar: bool,
    /// El origen del reloj de [`Self::tira`]: el de tokio, que los tests
    /// pueden pausar y adelantar.
    tira_base: tokio::time::Instant,
    /// Para cuándo hay ya un despertar programado, para no apilar uno por
    /// cada progreso.
    tira_despertar: Option<i64>,
    /// Las últimas claves lanzadas desde la paleta, la más reciente primero
    /// (spec 2026-09-10). Viven en la sesión de UI, como en el terminal.
    paleta_recientes: Vec<String>,
    /// Los directorios populares de la sesión (spec 2026-09-15 D6). Viven en
    /// la sesión de UI, como en el terminal.
    popular: norte_frontend::history::Popular,
    /// Los volúmenes del host, cacheados para el pie de cada listado (spec
    /// 2026-09-10). Se piden cuando un listado aterriza, nunca por foto:
    /// `host.volumes` monta y consulta espacio en cada filesystem.
    volumenes_pie: Vec<norte_proto::methods::Volume>,
    /// Hay una petición de [`Self::volumenes_pie`] en vuelo: no se apila otra.
    pie_en_vuelo: bool,
    /// Por qué menú se desplegó la última vez. Se reabre por ahí: empezar
    /// siempre por el primero obliga a recorrer la barra entera en cada
    /// gesto, y quien usa dos entradas del mismo menú lo paga cada vez.
    menu_ultimo: usize,
    /// El menú DESPLEGADO, si hay alguno.
    ///
    /// La barra se pinta siempre (o nunca, según `[ui] menu_bar`); esto es
    /// solo el desplegable. Mientras esté, las teclas son suyas — igual que
    /// la paleta, y por lo mismo: una flecha que se escapara movería el
    /// listado de debajo.
    menu: Option<norte_frontend::menu::MenuState>,
    /// La configuración con la que arrancó esta ventana, para enseñarla.
    config: norte_frontend::config::FrontendConfig,
    /// Dónde vive cada cosa.
    paths: crate::settings::HostPaths,
    /// Los ajustes, si están abiertos.
    ajustes: Option<crate::settings::Ajustes>,
    /// El gestor de extensiones, si está abierto.
    extensiones: Option<crate::extensions::Extensiones>,
    /// Todo lo de las sesiones de AGENTE: lo visto, si el panel está
    /// abierto, y qué deshacer corre por quién.
    agencia: Agencia,

    /// Cómo se llaman las extensiones y sus comandos, ya enmascarados, para
    /// el panel de salida: `id → (nombre, comando → título)`.
    ///
    /// Se compone con el catálogo que la PALETA pidió, y no con el del
    /// gestor: un comando se lanza desde la paleta con el gestor cerrado, y
    /// entonces no hay de dónde sacar un rótulo. Un panel que dice quién
    /// imprimió qué sin poder nombrar a ninguno de los dos no dice nada.
    rotulos_plugin: Rotulos,
    /// Lo que esta ventana tiene del ESCRITORIO: por dónde salen los efectos
    /// nativos y la salida del último comando de extensión.
    escritorio: Escritorio,
    /// La ventana tiene el foco del escritorio (#285).
    ///
    /// Arranca en `true` y no en `false`: un renderer que no mande
    /// `WindowFocus` se comporta como antes —avisa siempre— en vez de
    /// callarse. Perder un aviso es peor que repetirlo.
    enfocada: bool,
    /// Hay un selector de carpeta abierto, y esto dice si lo que se pidió era
    /// MOVER (#284). `None` = no se pidió ninguno.
    ///
    /// Solo el verbo: los operandos se recalculan cuando la respuesta vuelve.
    /// Congelarlos aquí prometería una operación sobre un listado que el
    /// lector pudo cambiar mientras el selector estaba delante.
    destino_pendiente: Option<bool>,

    /// El tema, tal como lo resolvió el arranque.
    tema: crate::pickers::HostTheme,
    /// El escritorio pide esquema OSCURO (`prefers-color-scheme`).
    ///
    /// Lo dice el renderer con [`UiAction::SetColorScheme`], al arrancar y
    /// cada vez que cambia. Elige contra qué variante de tema se resuelve el
    /// color de una entrada (puente 66): sin este dato, la ventana pintaba
    /// el cromo con la variante correcta y los NOMBRES con la otra.
    ///
    /// Arranca en `false` y no en «lo que diga el sistema» porque el host no
    /// tiene escritorio al que preguntar: lo corrige el primer mensaje del
    /// renderer, que llega antes de que se pinte nada.
    esquema_oscuro: bool,
    /// Se está mirando el tema por dentro.
    /// El selector de tema, si está abierto.
    tema_elegido: Option<profiles::SeleccionDeTema>,
    /// El PERFIL activo (ADR 0079), o ninguno.
    ///
    /// `OsString` porque es un nombre de directorio: pasarlo por texto cambia
    /// cuál se abre (#245).
    perfil_activo: Option<std::ffi::OsString>,
    /// El selector de perfiles, si está abierto.
    selector_perfil: Option<norte_frontend::profile_picker::ProfilePicker>,
    /// La generación de la lista de perfiles: sube cada vez que se relee.
    gen_perfiles: u64,
    /// Sube cada vez que cambia el conjunto de filas de la barra lateral.
    gen_sitios: u64,
    /// Sube cada vez que cambia el conjunto de filas del selector. Sirve
    /// también de id de APERTURA: el selector se abre vacío.
    gen_selector: u64,
    /// Cuántas veces se ha abierto el gestor de extensiones.
    gen_extensiones: u64,
    /// Cuántos catálogos se han PEDIDO, y cuál fue el último APLICADO.
    ///
    /// Aparte de la apertura: dos gobiernos seguidos piden dos catálogos con
    /// la misma apertura, y pueden contestar en cualquier orden. Sin esto, el
    /// viejo pisaba al nuevo y la columna «aprobada» se quedaba atrás sin que
    /// nada volviera a moverla.
    gen_catalogo: u64,
    /// El último catálogo aplicado, para descartar los que llegan tarde.
    catalogo_aplicado: u64,
    /// Cuántas veces se ha abierto la paleta.
    gen_paleta: u64,
    /// Cuántos comandos de extensión se han lanzado.
    gen_salida: u64,
    /// Los bytes de la imagen que el visor enseña, si ya llegaron.
    ///
    /// NO viajan en la foto: una imagen de ocho megas en el flujo de parches
    /// es un mensaje que se reenvía entero en cada `Resync` y que rompe la
    /// garantía de tamaño que `payload.rs` vigila. El renderer los pide
    /// aparte y hace un `blob:` con ellos (ADR 0069).
    imagen: Option<std::sync::Arc<Vec<u8>>>,
    /// La miniatura que un plugin dio del fichero del visor (ADR 0107): lo
    /// que la proyección anuncia como imagen y de quién es, mientras los
    /// bytes van en `imagen`. `None` = el visor pinta lo suyo.
    miniatura: Option<(crate::dto::ImageView, String)>,
    /// La búsqueda abierta, si la hay.
    busqueda: Option<search::Busqueda>,
    /// Cuántas búsquedas ha lanzado esta ventana. Es la identidad de la
    /// búsqueda mientras el daemon no ha dicho la suya.
    epoca_busqueda: u64,
    /// Las disposiciones del usuario, ya leídas por quien arrancó el host.
    disposiciones: Vec<norte_frontend::layout_picker::UserLayout>,
    /// El selector de disposiciones, si está abierto.
    selector_disposicion: Option<norte_frontend::layout_picker::LayoutPicker>,
    /// El selector de COLUMNAS, si está abierto.
    selector_columnas: Option<norte_frontend::columns_picker::ColumnsPicker>,
    /// La barra lateral de sitios, si la disposición coloca una. Hay UNA
    /// como mucho: dos listas idénticas de discos no son una disposición,
    /// son un fallo (lo dice el registro compartido, `multi: false`).
    sitios: Option<norte_frontend::places::PlacesState>,
    /// Lo que cada hueco de preview enseña (#291), por hueco: qué ruta, el
    /// visor con lo leído o la nota que lo sustituye, y lo que está en
    /// vuelo. Por hueco y no uno solo: el registro permite varios.
    previews: std::collections::BTreeMap<u32, preview::EstadoPreview>,
    /// Lo que cada panel de PLUGIN tiene vivo (fase 3), por hueco: su último
    /// marco, el estado opaco de su guest y lo que está en vuelo.
    ///
    /// El estado opaco es lo ÚNICO que sobrevive entre repintados —el permiso
    /// de leer se acuña por llamada—, así que se poda con el árbol: un
    /// `SlotId` se reutiliza, y sin podar el panel de otro plugin heredaría lo
    /// que guardó el primero.
    paneles: std::collections::BTreeMap<u32, panelplugin::EstadoPanel>,
    /// El mapa de disco de cada hueco que lo enseñe (fase 4).
    ///
    /// El estado es el COMPARTIDO (`norte_frontend::diskmap`), el mismo que
    /// usa el terminal: qué directorio describe, lo medido y cuál es el hijo
    /// elegido. Una decisión escrita dos veces diverge en silencio (ADR 0077).
    mapas: std::collections::BTreeMap<u32, diskmap::EstadoMapa>,
    /// La línea de tiempo de cada hueco que la tiene (#359).
    lineas: std::collections::BTreeMap<u32, timeline::EstadoLinea>,
    /// Lo ÚLTIMO que se mandó de cada hoja de atributos, por hueco.
    ///
    /// La hoja no pide nada y se calcula entera del listado, así que no tiene
    /// estado propio que guardar — pero sí hace falta saber qué vio el
    /// renderer, porque el cursor lo mueve cualquier mensaje y la hoja solo
    /// viaja en la foto entera. Sin esto, viajaba de gorra en la foto que
    /// provocaba el VISOR al cambiar de nota, y en una disposición con hoja y
    /// sin visor se quedaba congelada (lo que se veía: pinchar una fila no
    /// movía «Detalles»).
    hojas: std::collections::BTreeMap<u32, crate::dto::MetadataSlotView>,
    /// El árbol de directorios, si la disposición coloca uno. Hay UNO como
    /// mucho, por lo mismo que la barra de sitios.
    ramas: Option<norte_frontend::tree::Tree>,
    /// Sube cada vez que cambia el conjunto de ramas visibles.
    gen_ramas: u64,
    /// El fichero que hay que abrir en cuanto exista (#290), con la task que
    /// lo está creando.
    ///
    /// Uno como mucho: el gesto pide un nombre, y hasta que ese diálogo se
    /// contesta no hay otro.
    abrir_al_crear: Option<fileops::Creacion>,
    /// El cursor del panel de procesos.
    ///
    /// El MISMO tipo que usa la TUI, con su regla dentro: se acota al LEER y
    /// no al mover, porque las filas aparecen y desaparecen solas —una tarea
    /// termina y se barre a los diez segundos—, así que un cursor guardado
    /// siempre puede haberse quedado fuera. Aquí estaba escrito a mano en
    /// cinco sitios, que es la misma decisión duplicada que la ADR 0077
    /// existe para no tener.
    cursor_procesos: norte_frontend::processes::Processes,
    /// El estado del panel de registro: nivel, filtro y seguimiento (#326).
    ///
    /// El MISMO tipo que usa la TUI, con su regla de los dos niveles dentro
    /// (el que se captura y el que se enseña) y la de que bajar el segundo no
    /// deja de capturar. Duplicarlo aquí habría sido duplicar esas dos.
    log_panel: norte_frontend::logpanel::LogPanel,
    /// El anillo del que salen las líneas. `None` = este proceso no lo montó,
    /// y entonces el panel lo DICE en vez de enseñarse vacío como si no
    /// hubiera pasado nada.
    log_ring: Option<norte_config::logring::LogRing>,
    /// Cuántas filas de registro cabían en el último frame.
    ///
    /// La pone el renderer (`LogSetVisibleRange`), como la ventana del
    /// listado: adivinarla aquí es lo que en la TUI hizo que cada página se
    /// saltara dos líneas y la primera cuatro, y lo que ninguna de las dos
    /// ventanas enseñaba no se podía leer de ninguna manera.
    log_filas: usize,
    /// Sube en cada APERTURA del panel. Distingue el temporizador de esta
    /// apertura del de la anterior: abrir, cerrar y volver a abrir dejaría dos
    /// vivos sobre el mismo panel, y el viejo se rearmaría para siempre.
    log_epoca: u64,
    /// El contador de entradas del anillo la última vez que se pintó, para no
    /// mandar una foto por sondeo cuando no ha pasado nada.
    log_visto: u64,
    /// La mitad REMOTA del panel de registro: lo que el daemon lleva
    /// entregado de su anillo y lo que se sabe de él (#328).
    ///
    /// Junta y no seis campos sueltos: son un solo asunto —una fuente de
    /// líneas con su cursor, su estado y su petición en vuelo— y sueltos
    /// convertían a `Estado` en la clase de estructura que se describe con
    /// una lista de banderas.
    log_remoto: logpanel::RegistroRemoto,
    /// El selector abierto, si lo hay.
    selector: Option<crate::pickers::Selector>,
    /// La ayuda, si está abierta. Tapa la pantalla y se queda las teclas,
    /// como el visor: sus teclas son FIJAS (no hay vocabulario `dialog.*`
    /// para «filtrar esta lista» ni para «seguir este enlace»), que es lo
    /// mismo que hacen el TUI y la paleta.
    ayuda: Option<crate::help::Ayuda>,
    /// Una COPIA del keymap efectivo del listado.
    ///
    /// El resolver se queda con el suyo, y construir el panel de
    /// continuaciones necesita el efectivo entero (qué sigue a un prefijo, y
    /// qué disponibilidad tiene cada continuación). `Effective` es `Clone` y
    /// el TUI hace exactamente esto por el mismo motivo.
    efectivo: Effective,
    /// El idioma negociado, para las etiquetas de las continuaciones.
    lang: norte_i18n::Lang,
    /// Las continuaciones del prefijo a medias, si lo hay.
    ///
    /// Se construye en la TRANSICIÓN —la tecla que abre la secuencia y cada
    /// una que la profundiza— y no al proyectar: `WhichKeyRows::build` cuesta
    /// varias cadenas y uno o dos formatos Fluent POR FILA, y su propio
    /// rustdoc avisa de lo que pasa si se llama desde el pintado.
    whichkey: Option<norte_frontend::whichkey::WhichKeyRows>,
    /// El resolver de la pantalla del visor. Mientras el visor esté abierto,
    /// las teclas pasan por AQUÍ.
    resolver_visor: Resolver,
    /// El resolutor de la pantalla de DIÁLOGO.
    resolver_dialogo: Resolver,
    /// Si este frontend puede escribir.
    efectos: crate::commands::Efectos,
    /// Cuántas líneas caben en el visor, según el renderer.
    ///
    /// `None` mientras no lo diga: se cae al tamaño en celdas menos el cromo,
    /// que es una estimación y se comporta como tal.
    visor_filas: Option<usize>,
    /// Celdas de ancho del CUERPO del visor, medidas por el renderer la
    /// última vez que lo pintó. `None` hasta entonces: la primera apertura
    /// usa el viewport, que se pasa por el cromo.
    visor_columnas: Option<u32>,
    /// El testigo de la lectura del visor en vuelo, si la hay.
    ///
    /// Sin él, una lectura lenta abría el visor DESPUÉS de que el usuario lo
    /// cerrara o se fuera a otro sitio — y como las teclas se enrutan por
    /// «hay visor», la siguiente tecla la interpretaba otro mapa sin que
    /// nadie hubiera pedido nada.
    visor_en_vuelo: Option<RequestToken>,
    /// El testigo del visor que está ABIERTO, no del que se está pidiendo.
    ///
    /// Separado de `visor_en_vuelo`, que se limpia al abrirse: los bytes de
    /// la imagen llegan DESPUÉS, y sin esto no habría con qué comprobar que
    /// son de este visor y no del anterior.
    visor_token: Option<RequestToken>,
    /// El visor abierto, si lo hay. El modelo es el COMPARTIDO
    /// (`norte_frontend::viewer::Viewer`): decodificación, hexadecimal y
    /// desplazamiento son suyos.
    visor: Option<norte_frontend::viewer::Viewer>,
    /// La disposición: el árbol que el usuario configuró. NO se toca al
    /// redimensionar — un layout guardado es su intención, y reescribirlo
    /// porque la ventana encogió significa que abrir el host un minuto se
    /// come el layout del TUI (ADR 0058 D5).
    arbol: Node,
    /// Los kinds que este host sabe declarar (mínimos, foco, roles).
    kinds: KindRegistry,
    /// La última barra de paneles que cruzó el puente. `parche` la compara
    /// con la de ahora y manda la nueva si difiere: es lo que hace que la
    /// barra se actualice por cualquier camino sin que cada camino lo sepa.
    ultima_barra: Option<crate::dto::PanelBarView>,
    /// Los últimos elementos de la barra de estado que cruzaron (ADR 0132),
    /// por lo mismo que la barra de paneles.
    ultimos_elementos: Option<Vec<crate::dto::StatusItemView>>,
    /// La última línea fina que cruzó por hueco (ADR 0148), para mandar solo
    /// lo que cambia.
    ultima_linea: std::collections::HashMap<u32, Option<u8>>,
    /// El último ajuste de columnas que cruzó, por hueco
    /// (`norte_frontend::columns::fitted_columns`). Depende del ancho del
    /// hueco y de los nombres de su listado, y los dos cambian por caminos
    /// que no mandan cabecera; `parche` lo compara y, si difiere, manda la
    /// cabecera Y las filas juntas — una fila con una celda que su cabecera
    /// ya no tiene se pintaría sin ancho.
    ultimo_ajuste: std::collections::HashMap<u32, Vec<norte_frontend::columns::Fitted>>,
    /// Cuántos tics de un segundo lleva `status.message` en la barra (spec
    /// 2026-09-10): en TICS para que un test lo haga avanzar sin dormir.
    mensaje_ticks: u32,
    /// El shell del panel de terminal (#362), si hay uno vivo.
    ///
    /// Aquí y no en el hueco porque el kind es `multi: false`: hay uno, y
    /// sobrevive a que el panel se esconda tras una pestaña. Lo que lo mata es
    /// cerrar el hueco, y lo hace su `Drop`.
    terminal: Option<norte_term::pty::Shell>,
    /// La época del panel de terminal: sube al cerrarlo, y el temporizador en
    /// vuelo se deja morir sin rearmarse.
    terminal_epoca: u64,
    /// El texto que se estaba contando: si cambia, la cuenta vuelve a cero.
    mensaje_contado: Option<String>,
    /// El reparto del ÚLTIMO tamaño conocido: quién se pinta, quién no, y en
    /// qué orden se tabula. Vive y muere con el tamaño, no con el árbol.
    reparto: Resolved,
    /// El último tamaño repartido, en celdas. Viaja al renderer con el
    /// reparto: sin él no puede saber sobre qué rejilla están medidos los
    /// rectángulos que recibe.
    viewport: (u16, u16),
    /// Quién tiene el foco y quién es el destino.
    roles: Roles,
    /// La configuración de columnas, por esquema.
    columnas: norte_frontend::columns::ColumnsSettings,
    /// El catálogo de atributos de la localización de cada hueco, cacheado
    /// por ESQUEMA: es lo que dice si un `attr:` es un tamaño, una fecha o un
    /// modo, y sin él se pinta el número crudo.
    catalogos: std::collections::HashMap<String, norte_proto::AttrCatalog>,
    /// Los huecos con estado, por id.
    huecos: std::collections::BTreeMap<u32, Hueco>,
    /// Los diálogos abiertos, en orden de apertura. Cada uno con su id: un
    /// segundo `Confirm` con el mismo id no vuelve a lanzar nada, y uno con
    /// un id viejo no cierra el que hay ahora.
    dialogos: Vec<Dialogo>,
    /// El siguiente id de diálogo. Monótono: un id no se reutiliza jamás,
    /// que es lo que hace que «viejo» se pueda distinguir de «actual».
    siguiente_modal: u64,
    /// El plan de renombrado en revisión, si lo hay.
    revision_ia: Option<ai::RevisionIa>,
    /// La época de la revisión: sube en cada PETICIÓN y al abandonar una en
    /// vuelo. Una respuesta con otra época llegó tarde y se descarta en Rust.
    epoca_ia: u64,
    /// Navegación SINCRONIZADA (`pane.sync-nav`): mientras está puesta, cada
    /// navegación del hueco activo la repite el hueco destino.
    ///
    /// Estado de ejecución y no configuración ni sesión: es un modo que se
    /// enciende para hacer una cosa y se apaga después, como en Krusader.
    espejo_permanente: bool,
    /// La petición de plan EN VUELO: su época y el DIRECTORIO para el que se
    /// pidió.
    ///
    /// El directorio viaja aquí y no se lee del hueco al aterrizar, porque
    /// entre pedir el plan y que llegue el lector puede haber navegado: un
    /// plan de `series/` abierto diciendo `descargas/` estaría prometiendo
    /// renombrar lo que se ve, y renombraría otra cosa.
    ia_en_vuelo: Option<(u64, VPath, Vec<Vec<u8>>)>,
    /// El plan de ORGANIZAR en revisión (fase 8), si lo hay.
    revision_organizar: Option<organize::RevisionOrganizar>,
    /// Su época: sube en cada petición, y una respuesta con otra llegó tarde.
    epoca_organizar: u64,
    /// La petición de plan de organizar EN VUELO: época, directorio y los
    /// nombres que había en él al pedir.
    ///
    /// Los nombres viajan aquí por lo mismo que el directorio: entre pedir el
    /// plan y que llegue, el lector puede haber navegado, y preguntarle al
    /// hueco entonces pintaría el árbol contra un directorio que no es el
    /// suyo — diciendo «nueva» de una carpeta que sí existía, o al revés.
    organizar_en_vuelo: Option<(u64, VPath, Vec<String>)>,
    /// El tablero: lo que está en marcha, por id de task.
    tasks: std::collections::BTreeMap<u64, tasks::TaskViva>,
    /// El lote de transferencias en curso, si lo hay (#271).
    lote: Option<tasks::Lote>,
    /// La sesión de UI: qué revisión se leyó, si esta ventana es su dueña, y
    /// si el esquema que hay guardado es de una versión que este host no
    /// entiende (ADR 0059).
    sesion: Sesion,
    /// El directorio que un humano ESCRIBIÓ al arrancar, si escribió alguno.
    ///
    /// Se guarda porque la sesión se lee después de montar los huecos y pisa
    /// el sitio de todos: sin esto, `norte-gui /usr/bin` acababa donde
    /// estuvieras ayer. Lo consume [`Self::leer_sesion`] y no vuelve a hacer
    /// falta — una intención de arranque vale una vez.
    dir_pedido: Option<VPath>,
    /// Viene de un RELEVO (`--attach`, fase 9): las marcas de la sesión se
    /// reclaman. Sin él se ignoran — un arranque no es un relevo.
    attach: bool,
    /// Esta ventana ha entregado la pantalla y espera a saber si la terminal
    /// se abrió (fase 9). Es lo único que autoriza un `HandoffFailed`: la
    /// acción la puede mandar cualquiera, y sin un relevo en curso no hay
    /// nada que recuperar ni que decir.
    relevo_en_curso: bool,
    /// El último LISTADO que tuvo el foco. Cuando el foco está en un panel
    /// que no es un listado —el árbol, los sitios—, es sobre él sobre el que
    /// actúan los comandos y a él navega el árbol ([`Self::activo`]). Sin
    /// esto, `activo` caía al listado de id más bajo, que puede ser el de la
    /// DERECHA: elegir una rama movía el panel que no tenía el foco.
    ultimo_listado: Option<u32>,
    status: StatusView,
    conexion: ConnectionView,
    /// Las sesiones de provider que viajan sin cifrar (#44), acotadas por el
    /// módulo compartido.
    degradadas: norte_frontend::banners::DegradedSet,
    /// Lo que el daemon dijo de sí mismo antes de irse: relevo o parada.
    /// `None` = no ha dicho nada, o ya volvió.
    aviso_de_daemon: Option<&'static str>,
    /// La comparación abierta, si la hay.
    comparacion: Option<sync::Comparacion>,
    /// El plan de sincronización abierto, si lo hay.
    sincronizacion: Option<sync::Sincronizacion>,
    /// El lote de sumas en vuelo, si lo hay (#311). A lo sumo UNO: el diálogo
    /// de resultados es uno, y lanzar otro releva al anterior.
    sumas: Option<tasks::SumasEnVuelo>,
    /// El lote de sumas ENCOLADO y todavía sin id (#311). `None` = ninguno.
    sumas_pendientes: Option<sums::SumasEncoladas>,
    /// Un plan PEDIDO cuya Task todavía no ha contestado.
    sync_pedida: Option<sync::SyncPedida>,
    /// La consulta semántica en vuelo, para poder ABORTARLA.
    ///
    /// Abortar no es solo dejar de escuchar: el SDK manda `rpc.cancel` al
    /// soltar la llamada, y al otro lado hay un embed y un barrido del índice
    /// que cuestan. Relanzar o cerrar la vista los para.
    semantica_en_vuelo: Option<tokio::task::JoinHandle<()>>,
    /// Cuántas veces se ha (re)establecido la conexión con el daemon.
    ///
    /// Los ids de task los reparte el SCHEDULER de un proceso y empiezan en 1
    /// en cada arranque, así que tras un relevo —que esta ventana ahora sabe
    /// que viene, `ConnEvent::GoingAway`— el daemon nuevo reparte los MISMOS
    /// ids. Sin distinguir la época, la task 3 nueva heredaba de la vieja
    /// que su informe ya se pidió (y no se pedía nunca), sus directorios
    /// afectados, y hasta su detalle. Las aprobaciones no tienen este
    /// problema porque el daemon siembra SUS ids con el reloj a propósito.
    epoca_conexion: u64,
    /// El motor RECHAZÓ una mutación por no poder abrir su journal.
    ///
    /// Persistente y no un mensaje: la regla dura 4 dice que sin registro no
    /// se muta, así que esto describe lo que le va a pasar a TODA la sesión,
    /// no a la operación que se acaba de intentar.
    ///
    /// **Quién lo enciende, exactamente**: `Error::JournalUnavailable` solo
    /// lo produce el motor EMBEBIDO (el journal perezoso del TUI y el CLI).
    /// Un daemon con ese problema no llega a arrancar, así que una ventana
    /// montada sobre un socket —el caso de hoy— no puede ver este aviso. Se
    /// proyecta igualmente porque el host no elige quién lo monta, y quien lo
    /// montara sobre el motor embebido tendría el mismo derecho a saberlo.
    ///
    /// Y lo que este aviso NO dice: un journal simplemente OCUPADO deja pasar
    /// la mutación sin registrarla, y eso no produce este error ni enciende
    /// esto. El aviso habla de un motor que REHÚSA, no de uno que no anota.
    journal_rehusado: bool,
}

/// Lo que el host sabe de la sesión guardada.
#[derive(Debug)]
struct Sesion {
    /// La revisión sobre la que se escribe. Escribir sobre otra es pisar a
    /// quien escribió en medio, y el core lo rechaza.
    revision: u64,
    /// Esta ventana es la dueña. Una SUELTA (`detached`) no escribe: la
    /// sesión es un documento con un solo escritor.
    owner: bool,
    /// Lo guardado es de un esquema MÁS NUEVO que el que este host entiende.
    /// Entonces no se aplica y —sobre todo— no se sobrescribe: arrancar de la
    /// configuración es recuperable; machacar la sesión de una versión futura
    /// no lo es.
    futuro: bool,
    /// La política compartida de escritura: qué recortar, cuándo no repetir
    /// y cada cuánto vuelve a preguntar una ventana suelta.
    policy: norte_frontend::session::PushPolicy,
    /// El cuerpo tal como se LEYÓ, para conservar lo ajeno.
    ///
    /// Escribir desde un `SessionBody::default()` tiraba todo lo que esta
    /// ventana no entiende —los huecos de otro frontend, y las disposiciones
    /// guardadas— en vez de conservarlo. La ola #229–#234 puso «conserva lo
    /// ajeno en un relevo» exactamente por esto, y esta ventana no lo hacía.
    leida: norte_frontend::session::SessionBody,
    /// De qué huecos SABÍA lo leído del disco.
    ///
    /// El veto de `[profile.start]` (ADR 0098). Aparte de [`Self::leida`] y no
    /// derivado de ella al vuelo porque son dos preguntas: aquélla es lo que
    /// hay que volver a escribir, y esto es lo que la sesión ya conocía —y no
    /// puede moverse cuando el proceso empieza a guardar lo suyo.
    conocidos: std::collections::BTreeSet<u32>,
    /// El sello de edad de cada hueco, tal como se ESCRIBIÓ la última vez.
    ///
    /// La política compartida sella los huecos que cambiaron al preparar el
    /// cuerpo, y el llamante tiene que recordar ese sello para la siguiente
    /// captura: sellar cada captura con «ahora» hacía que ningún cuerpo fuera
    /// igual al anterior, y el tic escribía cada segundo sin que nada hubiera
    /// cambiado. Es el mismo mapa que lleva el terminal (`session.touched`).
    touched: std::collections::BTreeMap<u32, u64>,
    /// El cuerpo de un `session.put` en vuelo, si lo hay: el tic siguiente
    /// no manda otro encima —dos escrituras cruzadas con la misma revisión
    /// son un conflicto seguro— y el apagado sabe qué se estaba escribiendo
    /// para decir si lo suyo llegó o no.
    en_vuelo: Option<std::sync::Arc<norte_frontend::session::SessionBody>>,
    /// El daemon rehusó el cuerpo por tamaño (#316): desde entonces se manda
    /// sin historial, que es lo que se degrada. Lo que había que salvar es
    /// dónde está el lector, y eso cabe.
    sin_historial: bool,
    /// Los huecos que este proceso ya sembró desde `[profile.start]`.
    ///
    /// Sembrar es de la PRIMERA vez. Sin esta cuenta, un lector sin sesión
    /// guardada volvía al directorio de arranque del perfil cada vez que
    /// entraba y salía de él: para él [`Self::conocidos`] está siempre vacío.
    sembrados: std::collections::BTreeSet<u32>,
}

impl Hueco {
    /// Un hueco recién nacido: sin listado, sin historial y CARGANDO.
    ///
    /// Uno solo, porque los tres sitios que lo construían —el arranque,
    /// estrenar un hueco al cambiar de disposición y el de prueba— tenían que
    /// coincidir campo a campo, y un campo nuevo que se olvide en uno de
    /// ellos es un hueco que se comporta distinto según por dónde naciera.
    ///
    /// `ocultos` es el estado INICIAL de `[ui] show_hidden` (#107). Lo trae
    /// quien construye porque es configuración, y el pane nace enseñándolo
    /// todo: sin esto, una ventana con `show_hidden = false` en su config
    /// arrancaba enseñando los dotfiles igual, y `pane.toggle-hidden` los
    /// apartaba «por primera vez» en cada arranque.
    fn vacio(
        dir: VPath,
        ocultos: bool,
        orden: norte_frontend::SortSpec,
        fila_de_subir: bool,
    ) -> Self {
        let esquema = dir.scheme().to_owned();
        let mut pane = PaneState::new(dir, Vec::new());
        pane.set_show_hidden(ocultos);
        pane.set_sort(orden);
        // `[ui] parent_entry`: la fila `..` nace con el hueco y no se le pone
        // después — un hueco que se estrena sin ella y la gana en el siguiente
        // listado enseñaría dos pantallas distintas para la misma config.
        pane.set_parent_row(fila_de_subir);
        Self {
            pane,
            caps: None,
            esquema_del_orden: esquema,
            historial: History::default(),
            primera_visible: 0,
            visibles: 64,
            en_vuelo: None,
            dir_pedido: None,
            marcas_a_restaurar: Vec::new(),
            cursor_a_restaurar: None,
            filas_por_publicar: false,
            drenando: None,
            sondeando: false,
            cancelar_sondeo: std::sync::Arc::default(),
            // Sin destino: un hueco recién nacido no va a ninguna parte, ya
            // está donde va a estar.
            estado: Estado::cargando_hacia(None, None),
            sondeados: std::collections::HashSet::new(),
            adornos: std::collections::HashMap::new(),
            celdas_plugin: std::collections::HashMap::new(),
            adornando: false,
            visita_pendiente: None,
            adornadas: std::collections::HashSet::new(),
            gen_adornos: 0,
        }
    }

    /// Olvida lo que los plugins dijeron: el listado es OTRO.
    ///
    /// Una insignia de `git status` de un directorio no puede sobrevivir a un
    /// `cd`: la ruta sería otra y no casaría, pero la MEMORIA de «ya se pidió»
    /// sí sobreviviría y dejaría el listado nuevo sin decorar para siempre.
    fn olvidar_adornos(&mut self) {
        self.adornos.clear();
        self.celdas_plugin.clear();
        self.adornadas.clear();
        self.gen_adornos += 1;
    }
}

impl Estado {
    /// Construye el estado a partir de las opciones de arranque.
    ///
    /// Toma las opciones ENTERAS y no ocho parámetros sueltos: son los
    /// mismos datos, y una lista de ocho posiciones es donde dos `Effective`
    /// del mismo tipo se intercambian sin que el compilador diga nada.
    /// Un hueco de listado por cada `browser` del árbol, todos en el mismo
    /// directorio: de dónde arranca cada uno es cosa de la sesión (y hasta
    /// que exista, arrancar los dos donde arrancó el host es lo honesto).
    ///
    /// `[ui] show_hidden` (#107) siembra el estado inicial de cada uno, igual
    /// que en el TUI: ausente = enseñarlo todo.
    fn huecos_iniciales(
        arbol: &Node,
        kinds: &KindRegistry,
        dir: &VPath,
        settings: &norte_frontend::config::FrontendConfig,
        columnas: &norte_frontend::columns::ColumnsSettings,
    ) -> std::collections::BTreeMap<u32, Hueco> {
        let ocultos = settings.common.ui_show_hidden.unwrap_or(true);
        let subir = settings.common.ui_parent_entry.unwrap_or(true);
        let orden = columnas.sort_for(dir.scheme());
        let mut huecos = std::collections::BTreeMap::new();
        for SlotId(id) in arbol.slot_ids() {
            if es_listado(arbol, SlotId(id), kinds) {
                huecos.insert(id, Hueco::vacio(dir.clone(), ocultos, orden.clone(), subir));
            }
        }
        huecos
    }

    /// El idioma negociado, para las etiquetas de las continuaciones.
    fn lang_de(locale: &str) -> norte_i18n::Lang {
        match locale {
            "es" => norte_i18n::Lang::Es,
            _ => norte_i18n::Lang::En,
        }
    }

    /// Los roles de arranque.
    ///
    /// El DESTINO lo resuelve la capa compartida, y NO se pone a mano.
    /// Ponerlo con `Roles::set` lo marcaba como EXPLÍCITO —o sea, «lo eligió
    /// una persona»— cuando no lo había elegido nadie, y entonces sobrevivía
    /// a que aparecieran más candidatos: con tres listados, el primero se
    /// quedaba el rol para siempre y copiar mandaba ahí sin que nadie lo
    /// hubiera dicho (ADR 0058 D7).
    fn roles_iniciales(
        arbol: &Node,
        reparto: &norte_frontend::layout::Resolved,
        kinds: &KindRegistry,
        activo: u32,
    ) -> Roles {
        let mut roles = Roles::con_active(SlotId(activo));
        roles.reconcile(arbol, reparto, kinds, SlotId(activo));
        roles
    }

    /// Largo porque es un LITERAL de estructura: un campo por línea, con el
    /// porqué de los que no son obvios. No hay nada que extraer que no sea
    /// mover campos a una función que los devuelva de uno en uno.
    #[expect(
        clippy::too_many_lines,
        reason = "constructor: un campo por línea con su porqué, nada que extraer"
    )]
    fn nuevo(instance: InstanceId, options: UiHostOptions) -> (Self, Arc<dyn HostBackend>) {
        let UiHostOptions {
            backend,
            initial_dir,
            initial_dir_pedido,
            attach,
            locale,
            keymap,
            keymap_viewer: keymap_visor,
            keymap_dialog,
            layout: arbol,
            viewport,
            columns: columnas,
            effects: efectos,
            settings,
            paths,
            theme,
            user_layouts,
            profile: perfil_de_arranque,
            log_ring,
        } = options;
        let dir = &initial_dir;
        let lang = Self::lang_de(&locale);
        let kinds = KindRegistry::builtin();
        let reparto = resolve(rect(viewport), &arbol, &kinds);
        let huecos = Self::huecos_iniciales(&arbol, &kinds, dir, &settings, &columnas);
        let activo = huecos.keys().copied().next().unwrap_or(1);
        let roles = Self::roles_iniciales(&arbol, &reparto, &kinds, activo);
        let estado = Self {
            rotulos_plugin: Rotulos::new(),
            instance,
            sequence: 0,
            token: 0,
            locale,
            paleta: None,
            ir_a: None,
            gen_ir_a: 0,
            ir_a_indice: None,
            asistente: None,
            splash: None,
            splash_hasta_ms: None,
            splash_visto: false,
            procesos_auto: false,
            tira: norte_frontend::task_strip::TaskStrip::default(),
            encolar: false,
            tira_base: tokio::time::Instant::now(),
            tira_despertar: None,
            paleta_recientes: Vec::new(),
            popular: norte_frontend::history::Popular::default(),
            volumenes_pie: Vec::new(),
            pie_en_vuelo: false,
            menu: None,
            ayuda: None,
            ajustes: None,
            extensiones: None,
            agencia: Agencia::default(),
            escritorio: Escritorio::default(),
            enfocada: true,
            destino_pendiente: None,
            tema: theme,
            esquema_oscuro: false,
            tema_elegido: None,
            menu_ultimo: 0,
            // Lo que `--profile` nombró ya está APLICADO en `settings`; lo que
            // falta es que el host lo sepa (#307).
            perfil_activo: perfil_de_arranque,
            selector_perfil: None,
            gen_perfiles: 0,
            cursor_procesos: norte_frontend::processes::Processes::default(),
            log_panel: norte_frontend::logpanel::LogPanel::default(),
            log_ring,
            // Uno hasta que el primer frame diga la verdad: nunca cero, para
            // que una página antes de pintar mueva algo en vez de nada.
            log_filas: 1,
            log_epoca: 0,
            log_visto: 0,
            log_remoto: logpanel::RegistroRemoto::default(),
            sitios: None,
            previews: std::collections::BTreeMap::new(),
            paneles: std::collections::BTreeMap::new(),
            mapas: std::collections::BTreeMap::new(),
            lineas: std::collections::BTreeMap::new(),
            hojas: std::collections::BTreeMap::new(),
            gen_sitios: 0,
            ramas: None,
            gen_ramas: 0,
            abrir_al_crear: None,
            gen_selector: 0,
            gen_extensiones: 0,
            gen_catalogo: 0,
            catalogo_aplicado: 0,
            gen_paleta: 0,
            gen_salida: 0,
            imagen: None,
            miniatura: None,
            busqueda: None,
            epoca_busqueda: 0,
            disposiciones: user_layouts,
            selector_disposicion: None,
            selector_columnas: None,
            selector: None,
            config: settings,
            paths,
            efectivo_visor: keymap_visor.clone(),
            efectivo: keymap.clone(),
            lang,
            whichkey: None,
            resolver: Resolver::new(keymap),
            resolver_visor: Resolver::new(keymap_visor),
            resolver_dialogo: Resolver::new(keymap_dialog),
            efectos,
            visor_filas: None,
            visor_columnas: None,
            visor_en_vuelo: None,
            visor_token: None,
            visor: None,
            arbol,
            kinds,
            ultima_barra: None,
            ultimos_elementos: None,
            ultima_linea: std::collections::HashMap::new(),
            ultimo_ajuste: std::collections::HashMap::new(),
            mensaje_ticks: 0,
            // Perezoso, como en la terminal: un shell por sesión que nadie va
            // a usar es un proceso, un pty y el `.bashrc` de alguien
            // corriendo por si acaso.
            terminal: None,
            terminal_epoca: 0,
            mensaje_contado: None,
            reparto,
            viewport,
            roles,
            columnas,
            catalogos: std::collections::HashMap::new(),
            huecos,
            dialogos: Vec::new(),
            siguiente_modal: 1,
            revision_ia: None,
            epoca_ia: 0,
            espejo_permanente: false,
            ia_en_vuelo: None,
            revision_organizar: None,
            epoca_organizar: 0,
            organizar_en_vuelo: None,
            tasks: std::collections::BTreeMap::new(),
            lote: None,
            sesion: Sesion {
                revision: 0,
                owner: false,
                futuro: false,
                // Una ventana suelta vuelve a preguntar por la propiedad
                // cada treinta ticks: la dueña puede cerrarse en cualquier
                // momento y entonces alguien tiene que recogerla.
                policy: norte_frontend::session::PushPolicy::new(30),
                leida: norte_frontend::session::SessionBody::default(),
                conocidos: std::collections::BTreeSet::new(),
                touched: std::collections::BTreeMap::new(),
                en_vuelo: None,
                sin_historial: false,
                sembrados: std::collections::BTreeSet::new(),
            },
            dir_pedido: initial_dir_pedido.then(|| initial_dir.clone()),
            attach,
            relevo_en_curso: false,
            ultimo_listado: None,
            status: StatusView::default(),
            conexion: ConnectionView::Connected,
            degradadas: norte_frontend::banners::DegradedSet::default(),
            epoca_conexion: 0,
            semantica_en_vuelo: None,
            comparacion: None,
            sincronizacion: None,
            sumas: None,
            sumas_pendientes: None,
            sync_pedida: None,
            aviso_de_daemon: None,
            journal_rehusado: false,
        };
        (estado, backend)
    }

    /// El hueco con el foco. Siempre hay uno: si el rol apunta a un hueco
    /// que ya no existe, cae al primero que haya.
    fn activo(&self) -> u32 {
        let preferido = self.roles.get(RoleId::Active).map(|SlotId(id)| id);
        preferido
            .filter(|id| self.huecos.contains_key(id))
            .or_else(|| {
                self.ultimo_listado
                    .filter(|id| self.huecos.contains_key(id))
            })
            .or_else(|| self.huecos.keys().copied().next())
            .unwrap_or(1)
    }

    /// El hueco con el FOCO, sea del tipo que sea.
    ///
    /// No es lo mismo que [`Self::activo`], y confundirlos fue un bug: aquel
    /// contesta «el LISTADO sobre el que actúan los comandos» y se salta lo
    /// que no es un listado, que es justo lo que hace falta para que `F5`
    /// copie algo con la barra lateral enfocada. Este contesta dónde está el
    /// teclado, que es lo que decide quién recibe una tecla y qué hueco se
    /// pinta enfocado.
    fn enfocado(&self) -> u32 {
        self.roles
            .get(RoleId::Active)
            .map_or_else(|| self.activo(), |SlotId(id)| id)
    }

    /// El hueco ACTIVO, que siempre existe.
    ///
    /// El invariante (regla 6): `huecos` se siembra desde `arbol.slot_ids()`
    /// —en `Estado::nuevo` y en `aplicar_disposicion`, los dos únicos sitios
    /// que lo tocan— y `validate` rechaza un árbol sin `browser`, así que hay
    /// al menos uno. `activo()` sale de `Roles`, y `reconcilia_roles` corre
    /// tras cada cambio de reparto dejándolos apuntando a huecos que existen.
    ///
    /// El invariante fue FALSO hasta esta ola: se sembraba desde
    /// `reparto.placements`, que no incluye lo oculto, así que elegir una
    /// disposición que no coloca ningún listado vaciaba el mapa y la
    /// siguiente tecla panicaba dentro de la task del actor. Está clavado en
    /// `una_disposicion_que_esconde_el_listado_deja_el_hueco_vivo`.
    fn hueco(&self) -> &Hueco {
        let id = self.activo();
        self.huecos.get(&id).expect("el hueco activo existe")
    }

    /// El hueco activo, mutable. Mismo invariante que [`Self::hueco`].
    fn hueco_mut(&mut self) -> &mut Hueco {
        let id = self.activo();
        self.huecos.get_mut(&id).expect("el hueco activo existe")
    }

    /// Deja los roles apuntando a huecos que EXISTEN y se VEN.
    ///
    /// El foco se decide AQUÍ (solo se mueve si el hueco que lo tenía ya no
    /// vale) y el DESTINO lo decide la capa compartida
    /// ([`norte_frontend::layout::roles::Roles::reconcile`]), que es la que
    /// implementa la ADR 0058 D7. Este método tenía su propia regla —«el
    /// primer otro hueco visible»— y esa regla estaba mal por dos motivos
    /// que no se ven con dos paneles: con TRES adivinaba el de id más bajo, y
    /// pisaba un destino que una persona había designado a mano en cada
    /// cambio de foco. Desde que copiar y mover leen ese rol, adivinar es
    /// mandar ficheros a un sitio que nadie eligió. La regla compartida
    /// conserva lo explícito y, con varios candidatos y ninguno elegido,
    /// deja el rol SIN FIJAR: entonces la transferencia pide que se designe
    /// uno en vez de desempatar sola.
    fn reconcilia_roles(&mut self) {
        // El foco solo se MUEVE cuando el hueco que lo tenía ya no vale: se
        // ocultó, desapareció del reparto, o dejó de poder enfocarse. Pisarlo
        // siempre con «el primer listado» —que es lo que hacía— convertía el
        // tabulador en un interruptor entre dos paneles: caía en la barra
        // lateral o en el de procesos y volvía sola antes de que nadie lo
        // viera.
        let foco = self.roles.get(RoleId::Active).map(|SlotId(id)| id);
        let sirve = foco.is_some_and(|id| {
            !self.oculto(id)
                && self.reparto.placements.iter().any(|(s, _)| s.0 == id)
                && kind_de(&self.arbol, SlotId(id))
                    .and_then(|k| self.kinds.get(&k).map(|d| d.focusable))
                    .unwrap_or(false)
        });
        if !sirve {
            self.roles.set(RoleId::Active, SlotId(self.activo()));
        }
        let foco = SlotId(self.enfocado());
        self.roles
            .reconcile(&self.arbol, &self.reparto, &self.kinds, foco);
        // El foco en un LISTADO se recuerda: es a donde vuelven los comandos
        // y el árbol mientras el foco está en otro panel.
        if let Some(SlotId(id)) = self.roles.get(RoleId::Active)
            && self.huecos.contains_key(&id)
        {
            self.ultimo_listado = Some(id);
        }
    }

    /// ¿Está este hueco fuera del reparto de ESTE tamaño?
    ///
    /// Un hueco oculto —una pestaña de atrás, un panel que no cabe— no pide
    /// listados ni proyecta filas: lo que no se ve no se trae.
    fn oculto(&self, id: u32) -> bool {
        self.reparto.hidden.contains(&SlotId(id))
    }

    /// Pide el listado de los huecos que se VEN y todavía no lo han pedido.
    ///
    /// Un hueco oculto no se lista —lo que no se ve no se trae—, así que
    /// cuando el reparto lo saca a la luz hay que pedirlo ENTONCES. Nadie lo
    /// hacía: un `browser` oculto al arrancar que aparecía al agrandar la
    /// ventana se quedaba en `Loading` para siempre, con cero filas, y tras
    /// cambiar de disposición ni siquiera existía su `Hueco`, así que
    /// `snapshot()` caía al brazo por defecto y lo pintaba como
    /// `Unsupported { kind_name: "browser" }`.
    ///
    /// Idempotente por diseño: solo despierta lo que está en `Loading` SIN
    /// petición en vuelo, así que llamarlo en cada reparto no duplica nada.
    fn despertar_visibles(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let dormidos: Vec<u32> = self
            .huecos
            .iter()
            .filter(|(id, h)| {
                !self.oculto(**id)
                    && h.en_vuelo.is_none()
                    && matches!(h.estado, SlotState::Loading { .. })
            })
            .map(|(id, _)| *id)
            .collect();
        for id in dormidos {
            let dir = self.huecos[&id].pane.dir().clone();
            self.token += 1;
            let token = RequestToken(self.token);
            if let Some(h) = self.huecos.get_mut(&id) {
                h.en_vuelo = Some(token);
                h.drenando = Some(token);
            }
            self.pedir_listado(id, &dir, token, backend, buzon);
        }
    }

    /// Toma la primera página de un stream y deja el resto drenando hacia el
    /// actor.
    ///
    /// El resto llega por el MISMO buzón que todo lo demás, con el testigo de
    /// su petición: un lote de una navegación abandonada se descarta igual
    /// que su primera página.
    async fn primera_pagina(
        listado: Result<(norte_client::EntryStream, Option<u64>), Error>,
        slot: u32,
        token: RequestToken,
        buzon: mpsc::Sender<Mensaje>,
    ) -> Result<(Vec<Entry>, Option<u64>), Error> {
        use futures::StreamExt as _;
        let (mut stream, omitidas) = listado?;
        let mut primera = Vec::with_capacity(FIRST_PAGE);
        let mut agotado = false;
        while primera.len() < FIRST_PAGE {
            match stream.next().await {
                Some(Ok(e)) => primera.push(e),
                // Un error a mitad de página se cuenta como el error del
                // listado: media página no es un listado.
                Some(Err(e)) => return Err(e),
                None => {
                    agotado = true;
                    break;
                }
            }
        }
        // La tarea se lanza SIEMPRE, aunque el stream ya se haya agotado: su
        // último mensaje es lo que baja `drenando`, y sin él un listado que
        // cabe en una página dejaría el hueco marcado como «sigue llegando»
        // para el resto de la sesión. Y se lanza APARTE en vez de mandarlo
        // aquí porque este futuro lo espera el actor: mandar al buzón desde
        // dentro se bloquearía contra el único que lo vacía.
        tokio::spawn(async move {
            let mut lote = Vec::with_capacity(FILL_BATCH);
            if !agotado {
                while let Some(entrada) = stream.next().await {
                    let Ok(entrada) = entrada else {
                        // El resto se cortó. Lo que ya se pintó sigue siendo
                        // válido; callarlo es mejor que tirar el listado
                        // entero.
                        break;
                    };
                    lote.push(entrada);
                    if lote.len() >= FILL_BATCH {
                        let batch = std::mem::take(&mut lote);
                        if buzon
                            .send(Mensaje::MasEntradas(Box::new((token, slot, batch, false))))
                            .await
                            .is_err()
                        {
                            return;
                        }
                        lote = Vec::with_capacity(FILL_BATCH);
                    }
                }
            }
            let _ = buzon
                .send(Mensaje::MasEntradas(Box::new((token, slot, lote, true))))
                .await;
        });
        Ok((primera, omitidas))
    }

    /// El listado inicial de cada hueco VISIBLE, el único que se espera EN
    /// LÍNEA: hasta que exista no hay pantalla que enseñar, así que no hay
    /// nada que congelar.
    ///
    /// Un hueco oculto no se lista: lo que no se ve no se trae, y en cuanto
    /// el reparto lo saque a la luz se pedirá entonces.
    async fn listar_inicial(
        &mut self,
        backend_arc: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let backend = backend_arc.as_ref();
        let visibles: Vec<u32> = self
            .huecos
            .keys()
            .copied()
            .filter(|id| !self.oculto(*id))
            .collect();
        for id in visibles {
            let dir = self.huecos[&id].pane.dir().clone();
            self.token += 1;
            let token = RequestToken(self.token);
            if let Some(h) = self.huecos.get_mut(&id) {
                h.en_vuelo = Some(token);
                h.drenando = Some(token);
            }
            self.pedir_catalogo(&dir, backend_arc, buzon);
            let stream = backend.list(dir.clone(), self.attrs_de(&dir)).await;
            let res = Self::primera_pagina(stream, id, token, buzon.clone()).await;
            self.aterriza_en(id, dir, res);
            // El espacio libre del pie (spec 2026-09-10): también en el
            // arranque, que no pasa por `aterrizar_listado`. Sin esto la
            // ventana abría sin «libres» hasta la primera navegación.
            self.pedir_volumenes_de_pie(backend_arc, buzon);
            // Lo mismo que hace el aterrizaje de una navegación, y que este
            // camino no hacía: el PRIMER directorio de un hueco se quedaba sin
            // capacidades hasta que el lector navegara a otro sitio. O sea que
            // la ventana que se acaba de abrir dentro de un contenedor
            // ofrecía escrituras que ese contenedor no acepta —y el plegado
            // del destino tampoco constaba (#268) mientras nadie se moviera.
            self.pedir_capacidades(id, backend_arc, buzon);
        }
    }

    /// Aplica el resultado de un listado sobre SU hueco. El orden y el
    /// cursor los decide `PaneState`, que es quien sabe qué hacer con la
    /// memoria del cursor y con un foco pendiente.
    fn aterriza_en(&mut self, id: u32, dir: VPath, res: Result<(Vec<Entry>, Option<u64>), Error>) {
        // #108: el orden de `[ui.columns]` es POR ESQUEMA, así que se
        // reaplica cuando el hueco cambia de esquema — no en cada `cd`, que
        // es lo que hace el TUI. Aquí la SESIÓN restaura el orden, y
        // reaplicarlo en el primer aterrizaje lo borraría antes de verse.
        let cambia_esquema = self
            .huecos
            .get(&id)
            .is_some_and(|h| h.esquema_del_orden != dir.scheme());
        let orden = cambia_esquema.then(|| self.columnas.sort_for(dir.scheme()));
        let Some(hueco) = self.huecos.get_mut(&id) else {
            return;
        };
        if cambia_esquema {
            dir.scheme().clone_into(&mut hueco.esquema_del_orden);
        }
        hueco.en_vuelo = None;
        hueco.dir_pedido = None;
        // El listado es OTRO: lo sondeado antes no dice nada de estas
        // entradas, que nacen perezosas otra vez. Sin este vaciado, volver a
        // un directorio ya visitado deja las columnas de tamaño y fecha en
        // blanco para el resto de la sesión — y de paso el conjunto crecía
        // con un `VPath` por fichero visto en toda la vida del proceso.
        hueco.sondeados.clear();
        // Y lo que esté volando ya no vale: se marca para que su respuesta se
        // descarte en vez de pegarse a otro directorio.
        hueco
            .cancelar_sondeo
            .store(true, std::sync::atomic::Ordering::SeqCst);
        hueco.cancelar_sondeo = std::sync::Arc::default();
        hueco.sondeando = false;
        // Y lo mismo con lo que dijeron los plugins: la ruta de otro
        // directorio no casaría, pero la memoria de «ya se pidió» sí, y
        // dejaría el listado nuevo sin decorar para siempre.
        hueco.olvidar_adornos();
        hueco.adornando = false;
        match res {
            Ok((entradas, omitidas)) => {
                if let Some(spec) = orden {
                    hueco.pane.set_sort(spec);
                }
                hueco.pane.set_listing(dir, entradas);
                // Un refresco conserva la selección; un `cd` no tiene ninguna
                // que conservar y llega con la lista vacía. Lo que la
                // operación se llevó no se vuelve a marcar.
                let marcas = std::mem::take(&mut hueco.marcas_a_restaurar);
                hueco.pane.restore_marks(&marcas);
                // Y el cursor que dejó la sesión, también TRAS `set_listing`:
                // antes no hay filas y la fila 12 sería la 0. `set_cursor` lo
                // acota si el directorio tiene hoy menos entradas que entonces.
                if let Some(fila) = hueco.cursor_a_restaurar.take() {
                    hueco.pane.set_cursor(fila);
                }
                // TRAS `set_listing`, que la limpia: es un dato de ESTE
                // listado y arrastrar el del anterior sería decir que faltan
                // entradas de un directorio en el que faltaban de otro.
                hueco.pane.set_skipped(omitidas);
                hueco.primera_visible = 0;
                hueco.estado = SlotState::Ready;
            }
            Err(e) => {
                // Sin stream no hay drenaje que vaya a contestar, así que la
                // bandera la baja quien la levantó.
                hueco.drenando = None;
                hueco.pane.set_listing(dir, Vec::new());
                hueco.marcas_a_restaurar.clear();
                // Un listado fallido CONSUME el cursor guardado: si quedara
                // pendiente, caería sobre el siguiente listado que llegue, que
                // puede ser de otro sitio.
                hueco.cursor_a_restaurar = None;
                hueco.estado = SlotState::Error {
                    reason_key: norte_frontend::error::error_key(&e).to_owned(),
                    // CUÁL pide la contraseña. Sin esto, un arranque con dos
                    // paneles remotos decía «hace falta un secreto» dos veces
                    // y no había forma de saber a cuál contestar. El nombre
                    // sale de `connections.toml` —un fichero, no algo de
                    // fiar— así que se enmascara y se acota como todo lo que
                    // se pinta.
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

    /// Aplica una acción y devuelve su acuse más lo que haya que publicar.
    ///
    /// Lo que TARDA no se hace aquí. Una navegación deja la petición en
    /// vuelo y devuelve; su respuesta vuelve al actor como un mensaje más y
    /// se aplica en [`Estado::aterriza`]. Por eso el cursor sigue
    /// respondiendo mientras un NFS muerto piensa: el único escritor no está
    /// esperando a nadie.
    #[expect(
        clippy::too_many_lines,
        reason = "despachador exhaustivo: un brazo por acción y sin lógica dentro, \
                  como `ejecutar_pendiente`. Partirlo por la mitad solo movería \
                  la frontera a un sitio arbitrario"
    )]
    fn aplicar(
        &mut self,
        accion: &UiAction,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match accion {
            UiAction::MoveCursor { slot_id, delta } => self.mover_cursor(*slot_id, *delta),
            UiAction::SelectRow {
                slot_id,
                key,
                generation,
            } => self.poner_cursor(*slot_id, *key, *generation),
            UiAction::ToggleMark {
                slot_id,
                key,
                generation,
            } => self.marcar(*slot_id, *key, *generation),
            UiAction::SetVisibleRange {
                slot_id,
                first,
                count,
            } => {
                let (slot_id, first, count) = (*slot_id, *first, *count);
                // NO se exige que sea el hueco activo: declarar qué filas se
                // ven no es actuar sobre el listado, es decir dónde está
                // mirando el usuario. La rueda sobre el panel de al lado
                // mueve ESE panel y no le roba el foco a nadie.
                if !self.huecos.contains_key(&slot_id) || self.oculto(slot_id) {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                }
                let tope = count.min(u32::try_from(MAX_ROWS_PER_BATCH).unwrap_or(u32::MAX));
                if let Some(h) = self.huecos.get_mut(&slot_id) {
                    h.primera_visible = first;
                    h.visibles = tope;
                }
                // Scroll = filas nuevas a la vista, y puede que sin tamaño
                // todavía: el sondeo va con la ventana, no con el cursor.
                self.sondear(slot_id, backend, buzon);
                self.adornar(slot_id, backend, buzon);
                (self.aplicada(), vec![self.parche_filas_de(slot_id)])
            }
            UiAction::SortBy { slot_id, column } => self.ordenar_por(*slot_id, column),
            UiAction::ResizeColumn {
                slot_id,
                column,
                cells,
            } => self.redimensionar_columna(*slot_id, column, *cells, buzon),
            UiAction::FocusSlot { slot_id } => {
                let slot_id = *slot_id;
                // Enfocar algo que no se ve, o que no recibe foco, es una
                // carrera con un reparto anterior, no una orden.
                //
                // El criterio es el recorrido COMPARTIDO (`focus_order`), el
                // mismo que usa el tabulador: mientras esto exigía un
                // `browser`, un CLIC sobre el panel de procesos o sobre la
                // barra de sitios no los enfocaba —solo el tabulador podía—,
                // y el renderer manda exactamente esta acción al pulsar.
                if !self.reparto.focus_order.contains(&SlotId(slot_id)) || self.oculto(slot_id) {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                }
                self.roles.set(RoleId::Active, SlotId(slot_id));
                self.reconcilia_roles();
                // Un cambio de foco NO reenvía la pantalla: lo único que
                // cambia es quién lleva cada papel. Mandar la foto entera
                // costaba todas las filas de todos los listados por cada
                // tabulador — el mismo derroche que el bridge acota en el
                // cursor (decisión D7).
                let cambio = ViewChange::Layout(self.disposicion());
                (self.aplicada(), vec![self.parche(vec![cambio])])
            }
            UiAction::MarkRange {
                slot_id,
                from,
                to,
                generation,
            } => {
                let (slot_id, from, to, generation) = (*slot_id, *from, *to, *generation);
                // Los DOS extremos tienen que existir en ESTA generación.
                // Medio rango válido significa marcar hasta un sitio que ya
                // no es el que el usuario señaló — y `PaneState::mark_range`
                // RECORTA por contrato, así que un extremo desbordado
                // marcaría el listado entero, incluidas filas que el renderer
                // nunca recibió. Lo marcado es la entrada de un borrado.
                let (Some(a), Some(b)) = (
                    self.fila_de(slot_id, from, generation),
                    self.fila_de(slot_id, to, generation),
                ) else {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                };
                self.hueco_mut().pane.mark_range(a, b);
                (self.aplicada(), vec![self.parche_filas()])
            }
            UiAction::Activate { .. }
            | UiAction::Parent { .. }
            | UiAction::BreadcrumbActivate { .. }
            | UiAction::History { .. } => self.navegacion(accion, backend, buzon),
            UiAction::SetViewport { width, height } => {
                self.viewport = (*width, *height);
                self.reparto = resolve(rect(self.viewport), &self.arbol, &self.kinds);
                // El destino no puede apuntar a algo que no se ve: una copia
                // que aterriza en un panel oculto es una copia que el usuario
                // no verá llegar.
                self.reconcilia_roles();
                // Agrandar la ventana saca huecos de `hidden`, y un hueco que
                // aparece sin listado se queda cargando para siempre.
                self.despertar_visibles(backend, buzon);
                self.responde_con_foto()
            }
            UiAction::SetColorScheme { dark } => {
                // Lo mismo, no es nada: el renderer la manda al arrancar y en
                // cada cambio, y repintar todas las filas por un mensaje que
                // no cambia nada es trabajo por nada.
                if self.esquema_oscuro == *dark {
                    return (self.aplicada(), Vec::new());
                }
                self.esquema_oscuro = *dark;
                // Solo las FILAS: las variables CSS de la variante las
                // enchufa el renderer por su cuenta, síncronamente, para no
                // parpadear. Lo que el host tiene que rehacer es lo que va
                // cocido en la fila (puente 66).
                (self.aplicada(), self.parches_de_filas_de_todos())
            }
            UiAction::Key(k) => self.tecla(k, backend, buzon),
            UiAction::SetViewerRows { rows } => self.fijar_filas_del_visor(*rows),
            UiAction::SetViewerCols { cols } => {
                self.visor_columnas = Some((*cols).clamp(1, u32::from(u16::MAX)));
                (self.aplicada(), Vec::new())
            }
            UiAction::AiRenameDecide { approve } => {
                self.decidir_revision_ia(*approve, backend, buzon)
            }
            UiAction::OrganizeDecide { approve } => {
                self.decidir_revision_organizar(*approve, backend, buzon)
            }
            UiAction::OrganizeScroll { down } => self.recorrer_organizar(*down),
            UiAction::HandoffFailed { no_terminal } => self.relevo_fallido(*no_terminal),
            UiAction::Resync => self.responde_con_foto(),
            UiAction::RequestQuit => self.pedir_salir(),
            UiAction::MenuOpen { menu } => self.desplegar_menu(*menu),
            UiAction::MenuPointRow { row } => self.apuntar_en_menu(*row),
            UiAction::MenuActivateRow { row } => self.activar_del_menu(*row, backend, buzon),
            UiAction::MenuClose => self.cerrar_menu(),
            UiAction::MenuToggle => self.alternar_menu(),
            UiAction::WizardOpen => self.abrir_asistente(),
            UiAction::SplashOpen => self.abrir_splash(),
            UiAction::SplashClose => (self.aplicada(), self.cerrar_splash()),
            UiAction::SplashActivateRow { number } => {
                self.activar_fila_de_splash(*number, backend, buzon)
            }
            UiAction::WizardActivateRow { row } => {
                self.activar_fila_de_asistente(*row, backend, buzon)
            }
            UiAction::PanelBarActivate { button } => {
                self.pulsar_barra_de_paneles(*button, backend, buzon)
            }
            UiAction::StatusItemActivate { id } => {
                self.pulsar_elemento_de_estado(id, backend, buzon)
            }
            UiAction::LayoutButtonActivate { id } => {
                self.pulsar_boton_de_disposicion(id, backend, buzon)
            }
            UiAction::TabAction { slot_id, verb } => {
                self.boton_de_pestana(*slot_id, *verb, backend, buzon)
            }
            UiAction::MoveSlot {
                slot_id,
                target,
                zone,
            } => self.mover_hueco(*slot_id, *target, *zone, backend, buzon),
            UiAction::ResizeSlot { slot_id, cells } => {
                self.arrastrar_borde(*slot_id, *cells, backend, buzon)
            }
            UiAction::ProfileActivateRow { row, generation } => {
                self.activar_perfil_de_fila(*row, *generation, backend, buzon)
            }
            UiAction::Dialog { id, choice, secret } => {
                self.responder_dialogo(*id, choice, secret.as_deref(), backend, buzon)
            }
            UiAction::RefreshSlot { slot_id } => {
                let cambios = self.refrescar(*slot_id, backend, buzon);
                if cambios.is_empty() {
                    // Ya tenía algo en vuelo: lo que va a aterrizar es más
                    // nuevo que este clic.
                    (self.aplicada(), Vec::new())
                } else {
                    (self.aplicada(), vec![self.parche(cambios)])
                }
            }
            UiAction::LogSetLevel { level } => self.nivel_de_registro(level, backend, buzon),
            UiAction::LogSetFilter { filter } => self.filtro_de_registro(filter),
            UiAction::LogScroll { delta } => self.desplazar_registro(*delta),
            UiAction::PanelClick { slot_id, row, col } => {
                // La MISMA acción para los dos, y se bifurca por el kind del
                // hueco: el renderer manda una celda y no sabe —ni tiene por
                // qué— si detrás hay un guest o un treemap. Lo que cambia es
                // quién resuelve y contra qué marco.
                if kind_de(&self.arbol, SlotId(*slot_id))
                    .is_some_and(|k| k.as_str() == diskmap::KIND)
                {
                    self.clic_en_mapa(*slot_id, *row, *col, backend, buzon)
                } else {
                    self.clic_en_panel(*slot_id, *row, *col, backend, buzon)
                }
            }
            UiAction::PreviewScroll { slot_id, delta } => self.desplazar_preview(*slot_id, *delta),
            UiAction::ViewerScroll { lines, cols } => self.desplazar_visor(*lines, *cols),
            UiAction::LogFollow => self.seguir_registro(),
            UiAction::LogCycleSource => self.fuente_de_registro(),
            UiAction::LogSetVisibleRange { rows } => self.filas_de_registro(*rows),
            UiAction::CancelTask { task_id } => self.cancelar(*task_id),
            UiAction::CompareSelectRow { .. }
            | UiAction::CompareActivateRow { .. }
            | UiAction::CompareToggleFilter { .. }
            | UiAction::CompareSetVisibleRange { .. } => {
                self.accion_de_comparacion(accion, backend, buzon)
            }
            // Un diálogo con campo de texto llega con la tarea que lo traiga
            // (crear directorio, renombrar). Decirlo es más honesto que
            // aceptar texto que nadie va a leer.
            UiAction::DialogInput { id, text } => self.escribir_en_dialogo(*id, text),
            // Y un diálogo-FORMULARIO (puente 91): dice CUÁL de sus campos se
            // tocó, que es lo que el de un solo campo no necesita decir.
            UiAction::DialogField { id, field, value } => {
                self.tocar_campo_de_dialogo(*id, field, value)
            }
            UiAction::DirectoryPicked { path } => {
                self.destino_elegido(path.clone(), backend, buzon)
            }
            UiAction::ProgramFinished {
                title_key,
                command,
                output,
                truncated,
                failed,
            } => self.programa_terminado(title_key, command, output, *truncated, *failed),
            UiAction::FilesDropped { paths } => self.soltados(paths, backend, buzon),
            UiAction::WindowFocus { focused } => {
                self.enfocada = *focused;
                (self.aplicada(), Vec::new())
            }
            otra => self.fila_por_indice(otra, backend, buzon),
        }
    }

    /// Las acciones que nombran una fila de un OVERLAY por su índice.
    ///
    /// Juntas y aparte porque comparten el mismo riesgo: el renderer pinta
    /// una lista y el usuario pulsa sobre la lista que TENÍA delante, no
    /// sobre la que el host tiene ahora. Las dos cuyo conjunto de filas puede
    /// cambiar solo —la barra lateral y el selector, que se llenan desde una
    /// tarea de fondo— llevan generación; las demás no pueden cambiar sin un
    /// gesto del usuario, y lo que sí hacen todas es RECHAZAR un índice fuera
    /// de rango en vez de recortarlo.
    fn fila_por_indice(
        &mut self,
        accion: &UiAction,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match accion {
            UiAction::HelpSelectTopic { row } => self.elegir_pagina(*row, backend, buzon),
            UiAction::SettingsSelectRow { row } => self.elegir_ajuste(*row),
            UiAction::SettingsActivate { row } => self.activar_ajuste_por_raton(*row, buzon),
            UiAction::SettingsQuery { text } => self.buscar_ajuste(text),
            UiAction::SettingsJumpSection { section } => self.saltar_a_seccion(section),
            UiAction::SettingsReset { row } => self.restablecer_ajuste(*row, buzon),
            UiAction::SettingsSet { id, value } => self.poner_ajuste(id, value, buzon),
            UiAction::ExtensionSelectRow { row } => self.elegir_extension(*row, backend, buzon),
            UiAction::ExtensionGovern { row, id, change } => {
                self.gobernar_por_raton(*row, id, (*change).into(), backend, buzon)
            }
            UiAction::ExtensionHelp { row, id } => {
                self.ayuda_de_extension(*row, id, backend, buzon)
            }
            UiAction::SelectTab { slot_id } => self.elegir_pestana(*slot_id, backend, buzon),
            UiAction::AgentSelectRow { row, generation } => self.elegir_agente(*row, *generation),
            UiAction::PickerSelectRow { row, generation } => {
                self.elegir_fila_del_selector(*row, *generation)
            }
            UiAction::PlaceActivateRow { row, generation } => {
                self.activar_sitio(*row, *generation, backend, buzon)
            }
            UiAction::TreeActivateRow { row, generation } => {
                self.tocar_rama(*row, *generation, true, backend, buzon)
            }
            UiAction::TreeToggleRow { row, generation } => {
                self.tocar_rama(*row, *generation, false, backend, buzon)
            }
            UiAction::LayoutActivateRow { row } => self.elegir_disposicion(*row, backend, buzon),
            UiAction::SearchActivateRow { row } => self.ir_al_resultado(*row, backend, buzon),
            UiAction::HelpActivate { index } => self.activar_en_ayuda(*index, backend, buzon),
            // El resto lo trató `aplicar`; llegar aquí sería un brazo que se
            // le olvidó, y contestar `Applied` a algo que no se hizo es peor
            // que decir que no se pudo.
            _ => (Self::obsoleta(StaleAction::Modal), Vec::new()),
        }
    }
}
