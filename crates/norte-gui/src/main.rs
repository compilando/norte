//! norte-gui — SPIKE M5 (hito 1) → GUI-a Task 4: **dual-pane navegable**.
//!
//! Sobre el scaffold del spike (ventana GPUI que listaba UN dir real del
//! daemon) GUI-a construye un file manager ortodoxo mínimo: DOS panes, foco
//! conmutable, cursor, `cd`/enter/parent, quick search por tipeo, teclado y
//! ratón — todo READ-ONLY (`fs.list`). La lógica de pane (cursor + quick
//! search) NO vive aquí: es [`norte_frontend::PaneState`], el mismo modelo puro
//! que consumirá la TUI (GUI-a T3). La GUI solo: (1) pinta el estado de los dos
//! panes con GPUI, (2) traduce input a mutaciones del `PaneState`, (3) lanza los
//! `fs.list` contra el daemon y cruza el resultado al hilo de render.
//!
//! # API de GPUI usada (rev f14fea9)
//!
//! - **Input de teclado**: `div().track_focus(&handle).on_key_down(cx.listener(
//!   Self::on_key))`. El root pide foco en `new` (`window.focus(&handle, cx)`),
//!   así los `KeyDownEvent` llegan al root. `KeyDownEvent.keystroke.key` es el
//!   nombre de la tecla (`"up"`, `"tab"`, `"escape"`, `"a"`…, ver
//!   `keymap::gpui_chord`); `key_char` el carácter realmente tecleado
//!   (fidelidad de layout/shift). Descubierto en
//!   `crates/gpui/examples/{focus_visible,input}.rs`. Las teclas de
//!   navegación/mutación se resuelven vía el motor de keymap compartido
//!   (GUI-c T3, `norte_frontend::keymap`) — configurable por capas, NO
//!   hardcodeadas.
//! - **Ratón**: `div().on_mouse_down(MouseButton::Left, cx.listener(...))` con
//!   `MouseDownEvent.click_count` (1 = foco+cursor, 2 = `cd`). `on_scroll_wheel`
//!   con `ScrollWheelEvent.delta` mueve el cursor. Se usa `on_mouse_down` (no
//!   `on_click`) para no exigir un `.id()` estable por fila.
//! - **async → UI**: `cx.spawn` + `this.update` + `cx.notify()`, con un canal
//!   `tokio::mpsc` que cruza desde el hilo de sesión tokio (ver `session.rs`).
//! - **Accesibilidad (AccessKit, GUI-e T2)**: `div().id(...)` (convierte a
//!   `Stateful<Div>`, único que implementa `StatefulInteractiveElement`) +
//!   `.role(gpui::Role::X)` + `.aria_label(...)`/`.aria_description(...)`/
//!   `.aria_selected(...)`/`.aria_toggled(...)`, EXACTO idioma de
//!   `crates/gpui/examples/a11y.rs` (rev f14fea9) — sin `a11y_synthetic_children`:
//!   `.aria_label` ya adjunta el nombre accesible (`write_a11y_info` en
//!   `div.rs` llama `node.set_label(...)` desde ahí), así que la ruta
//!   `A11ySubtreeBuilder` no hace falta para este pase. `gpui::Role`/
//!   `gpui::Toggled` son el re-export de `accesskit` que ya trae `gpui`
//!   (`pub use accesskit;` en `gpui.rs`) — CALIFICADOS siempre como
//!   `gpui::Role`/`gpui::Toggled` porque `norte_theme::Role` (tema de
//!   colores) ya ocupa el nombre corto `Role` en este módulo. Los nodos a11y
//!   solo los materializa GPUI cuando `Window::is_a11y_active()` es `true`
//!   (un AT real conectado al bus AT-SPI) — eso NO lo controla esta capa;
//!   `.role()`/`.aria_label()` son metadata barata que se fija SIEMPRE
//!   (coherente con "roles on divs are cheap, can be unconditional"). Volcado
//!   estructural sin lector: F12 bajo `NORTE_GUI_DEBUG` (ver `on_key`).
#![forbid(unsafe_code)]

use gpui::{
    App, Bounds, Context, FocusHandle, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    ParentElement, Render, RenderImage, ScrollDelta, ScrollStrategy, ScrollWheelEvent,
    SharedString, Styled, UniformListScrollHandle, Window, WindowBounds, WindowOptions, div, img,
    prelude::*, px, rgb, rgba, size, uniform_list,
};
use gpui_platform::application;

use std::ops::Range;

use norte_frontend::{PaneState, nav::Mode};
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_theme::{FileKind, Role, Theme};

mod keymap;
mod modal;
mod session;
mod theme_map;

use modal::{Modal, ModalOutcome, PendingOp, PendingTransfer, TransferKind};
use session::{LoadConfig, SessionCmd, SessionEvent};

/// Badge local que prefija un nombre alterado en el display (regla 1 / spec §6:
/// display siempre lossy y MARCADO). No hay `ui.rs` de la TUI aquí, así que la
/// GUI define su propio marcador; el criterio de «hostil» sí es el compartido
/// ([`norte_frontend::display_name`] devuelve el bool).
const HOSTILE_BADGE: &str = "⚠";

/// Salto de página (↑↓ de 10 en 10) — orden de magnitud de una pantalla del
/// spike; el fill/scroll fino es optimización posterior.
const PAGE: usize = 10;

/// Alto FIJO de cada fila (px), requerido por `uniform_list` (issue #87): sin
/// una altura uniforme no puede medir un elemento y derivar el resto por
/// aritmética en vez de layout completo. También usado por el visor (F3),
/// que NO usa `uniform_list` (ver `render_viewer`) pero sí quiere filas de
/// alto uniforme.
const ROW_H: f32 = 22.0;

/// Filas de chrome que le restamos al alto del viewport para derivar cuántas
/// filas de contenido pedirle a `Viewer::rows` en `render_viewer`: cabecera +
/// barra de estado (una fila cada una) + margen de redondeo.
const VIEWER_CHROME_ROWS: usize = 3;

// Colores del chrome del dual-pane (constantes locales; el color por TIPO de
// archivo sí sale del tema, ver `entry_color`). El theming completo del chrome
// por rol es fuera de alcance del spike.
const BG: u32 = 0x121212;
const FG: u32 = 0xffffff;
const PANE_BG: u32 = 0x1e1e1e;
const PANE_BG_FOCUS: u32 = 0x252526;
const HEADER_BG: u32 = 0x2d2d2d;
const BORDER_FOCUS: u32 = 0x3b82f6;
const BORDER_UNFOCUS: u32 = 0x3a3a3a;
const SEL_BG: u32 = 0x264f78;
const ERR_FG: u32 = 0xf87171;
const QUICK_FG: u32 = 0xfbbf24;
/// Fondo de una fila MARCADA (distinto de `SEL_BG`, que es la selección bajo
/// cursor — marca y selección son ortogonales, ver `render_row`).
const MARK_BG: u32 = 0x3d3315;

/// Marcador de fila con marca (prefijo visible; el bool lo expone
/// `PaneState::is_marked`, la GUI solo lo pinta).
const MARK_MARKER: &str = "●";

/// El *root view*: dos panes navegables, cuál tiene el foco, el tema cacheado y
/// el canal hacia el hilo de sesión persistente (para relistar en cada `cd`).
struct NorteGui {
    /// Los dos panes (modelo puro compartido con la TUI).
    panes: [PaneState; 2],
    /// Pane con el foco (0|1): recibe el input de teclado.
    focus: usize,
    /// Texto del quick search por pane, PARALELO a `PaneState` solo para
    /// pintar la línea `/{query}` al pie: `PaneState` expone los índices
    /// visibles pero no el texto de la consulta, así que la GUI lo espeja al
    /// dirigir `quick_char`/`quick_backspace` (fuente de verdad = el pane; esto
    /// es solo su reflejo para render).
    query: [String; 2],
    /// Último error de carga por pane (banner), o `None` si el listado está OK.
    errors: [Option<String>; 2],
    /// Contador de generación por pane: cada `cd` (incluido el `begin_loading`)
    /// lo incrementa y captura el valor. Un `fs.list` en vuelo lleva su
    /// generación; al llegar solo se aplica si sigue vigente. Así un cd viejo
    /// (A) que termina TARDE no pisa el listado de un cd nuevo (B) lanzado en el
    /// mismo pane — robusto incluso ante A→B→A, que un simple compare de `dir`
    /// no distingue (ver `generation_is_current`).
    generation: [u64; 2],
    /// Relist coalescido pendiente por pane (#84): un read-after-write que se
    /// saltó porque el pane YA cargaba ese dir se re-dispara al aterrizar la
    /// list en vuelo — así la list superviviente no puede preceder a escrituras
    /// posteriores del burst (correctitud) sin pagar N lists redundantes.
    relist_pending: [bool; 2],
    /// Tema cacheado UNA vez (parsea TOML; no es gratis por-frame).
    theme: Theme,
    /// Canal hacia el hilo de sesión (conexión persistente al daemon, ver
    /// `session.rs`): cada `cd` manda un `SessionCmd::List`, jamás reconecta.
    cmds: tokio::sync::mpsc::UnboundedSender<SessionCmd>,
    /// Handle de foco del root: sin él los `KeyDownEvent` no llegan.
    focus_handle: FocusHandle,
    /// Modal activo (confirmación/conflicto), o `None`.
    modal: Option<modal::Modal>,
    /// Tasks en curso/terminadas por id, con la op que las originó (para el
    /// read-after-write posterior y el reintento de conflicto).
    inflight: std::collections::HashMap<norte_proto::TaskId, modal::PendingOp>,
    /// Último snapshot de progreso por task.
    task_progress: std::collections::HashMap<norte_proto::TaskId, norte_proto::TaskProgress>,
    /// Conflictos pendientes de resolver cuando ya hay un modal abierto (cola
    /// simple: se drenan al cerrar el modal actual). Evita que un 2.º conflicto
    /// pise al 1.º y se pierda sin aviso.
    conflict_backlog: Vec<(modal::PendingTransfer, norte_proto::ConflictKind)>,
    /// Orden de llegada de las tasks (render estable; `task_progress` no ordena).
    task_order: Vec<norte_proto::TaskId>,
    /// Cursor de la franja de tasks (#91): índice dentro de `task_order` que
    /// F9 (`task.cancel`) cancela y `render_task_strip` resalta. Se mueve con
    /// `task.next`/`task.prev` y se CLAMPA a `task_order.len()-1` cuando la
    /// franja se poda (Completed autopodadas / `task.dismiss`) — jamás indexa
    /// fuera de rango (ver `clamp_task_cursor`).
    task_cursor: usize,
    /// Handle de scroll de la lista virtualizada de cada pane (issue #87): debe
    /// persistir entre renders (no se puede recrear cada frame) para que
    /// `scroll_to_item` (llamado tras mover el cursor) tenga efecto.
    scrolls: [UniformListScrollHandle; 2],
    /// Motor de resolución de teclas (GUI-c T3): preset orthodox + capas del
    /// usuario, ya validado. Cada tecla que no la consume el modal/quick pasa
    /// por aquí (`keymap::gpui_chord` → `resolver.push`).
    resolver: norte_frontend::keymap::Resolver,
    /// Si la carga del keymap efectivo falló (capa de usuario/proyecto rota):
    /// el mensaje para el banner. `resolver` en ese caso corre solo con el
    /// preset (`keymap::build_effective_preset_only`) — la GUI sigue viva.
    keymap_error: Option<String>,
    /// El visor abierto (F3), o `None` = dual-pane (o cargando, ver
    /// `viewer_loading`). `v.scroll` (dentro del `Viewer` core) es el ÚNICO
    /// dueño del scroll del visor — sin lista virtualizada de GPUI de por
    /// medio (`Viewer::rows(height)` ya está acotado a `height` filas, O(H)
    /// no O(total): issue #87 no aplica aquí, a diferencia de los panes).
    viewer: Option<norte_frontend::viewer::Viewer>,
    /// Imagen decodificada del visor (cache): `Some` sólo cuando el `viewer`
    /// abierto es una imagen (se decodifica UNA vez en `ViewerOpened`, no en
    /// cada frame). `None` en texto/hex/plugin o sin visor. El render la usa
    /// cuando `viewer.is_image()`; un decode fallido queda como `Unreadable`.
    viewer_image: Option<ImagePreview>,
    /// Resolver del contexto Viewer (teclas del visor, GUI-d T3).
    viewer_resolver: norte_frontend::keymap::Resolver,
    /// Generación del `OpenViewer` en vuelo (guard anti-stale, como
    /// `generation` de los panes): un `ViewerOpened`/`ViewerFailed` con una
    /// generación vieja se descarta (F3 tardío no reabre por sorpresa; dos
    /// F3 seguidos no encolan dos aperturas).
    viewer_gen: u64,
    /// `true` mientras un `OpenViewer` está en vuelo (para el estado
    /// «abriendo visor…» del render, ver `render`).
    viewer_loading: bool,
}

impl NorteGui {
    /// Construye el view con los dos panes en el mismo directorio inicial
    /// (`NORTE_DIR` o el `cwd`) y lanza sus dos cargas. Toma el foco de la
    /// ventana para recibir teclado. Si la config no resuelve, nace con ambos
    /// panes en error (sin lanzar cargas), nunca panic.
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let theme = Theme::preset_default();
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        let ((browse_eff, viewer_eff), keymap_error) = match keymap::build_effectives() {
            Ok(pair) => (pair, None),
            Err(e) => {
                let error = e.to_string();
                (
                    keymap::build_effectives_preset_only(),
                    Some(norte_i18n::ta(
                        "gui-banner-keymap-error",
                        &[("error", error.as_str())],
                    )),
                )
            }
        };
        let resolver = norte_frontend::keymap::Resolver::new(browse_eff);
        let viewer_resolver = norte_frontend::keymap::Resolver::new(viewer_eff);

        match LoadConfig::from_env() {
            Ok(cfg) => {
                let LoadConfig { socket, dir } = cfg;
                let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
                let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
                session::spawn(socket, cmd_rx, event_tx);

                let mut gui = Self {
                    panes: [
                        PaneState::new(dir.clone(), Vec::new()),
                        PaneState::new(dir.clone(), Vec::new()),
                    ],
                    focus: 0,
                    query: [String::new(), String::new()],
                    errors: [None, None],
                    generation: [0, 0],
                    relist_pending: [false, false],
                    theme,
                    cmds: cmd_tx,
                    focus_handle,
                    modal: None,
                    inflight: std::collections::HashMap::new(),
                    task_progress: std::collections::HashMap::new(),
                    conflict_backlog: Vec::new(),
                    task_order: Vec::new(),
                    task_cursor: 0,
                    scrolls: [
                        UniformListScrollHandle::new(),
                        UniformListScrollHandle::new(),
                    ],
                    resolver,
                    keymap_error,
                    viewer: None,
                    viewer_image: None,
                    viewer_resolver,
                    viewer_gen: 0,
                    viewer_loading: false,
                };
                gui.spawn_event_loop(event_rx, cx);
                gui.cd(0, dir.clone(), cx);
                gui.cd(1, dir, cx);
                gui
            }
            Err(e) => {
                // INVARIANTE: "file:///" es un VPath raíz siempre válido; solo
                // es un placeholder para pintar el banner de error.
                let placeholder = VPath::parse("file:///").expect("'file:///' es un VPath válido");
                // Sin config resuelta no hay socket que conectar: el canal de
                // comandos nace sin sesión al otro lado (receptor soltado),
                // `cd` seguirá funcionando sin panic (el `send` simplemente
                // falla en silencio, no hay `cd` de por medio aquí).
                let (cmd_tx, _cmd_rx) = tokio::sync::mpsc::unbounded_channel();
                Self {
                    panes: [
                        PaneState::new(placeholder.clone(), Vec::new()),
                        PaneState::new(placeholder, Vec::new()),
                    ],
                    focus: 0,
                    query: [String::new(), String::new()],
                    errors: {
                        let error = e.to_string();
                        let msg = norte_i18n::ta(
                            "gui-banner-config-invalid",
                            &[("error", error.as_str())],
                        );
                        [Some(msg.clone()), Some(msg)]
                    },
                    generation: [0, 0],
                    relist_pending: [false, false],
                    theme,
                    cmds: cmd_tx,
                    focus_handle,
                    modal: None,
                    inflight: std::collections::HashMap::new(),
                    task_progress: std::collections::HashMap::new(),
                    conflict_backlog: Vec::new(),
                    task_order: Vec::new(),
                    task_cursor: 0,
                    scrolls: [
                        UniformListScrollHandle::new(),
                        UniformListScrollHandle::new(),
                    ],
                    resolver,
                    keymap_error,
                    viewer: None,
                    viewer_image: None,
                    viewer_resolver,
                    viewer_gen: 0,
                    viewer_loading: false,
                }
            }
        }
    }

    /// Cambia el directorio de `pane` a `dir`: lo marca como cargando y manda
    /// un `List` al hilo de sesión; el resultado llega por el drenador de
    /// eventos (`spawn_event_loop`) y se aplica con el guard de generación.
    fn cd(&mut self, pane: usize, dir: VPath, _cx: &mut Context<Self>) {
        self.generation[pane] = self.generation[pane].wrapping_add(1);
        let generation = self.generation[pane];
        self.panes[pane].begin_loading(dir.clone());
        self.errors[pane] = None;
        self.query[pane].clear();
        let _ = self.cmds.send(SessionCmd::List {
            pane,
            generation,
            dir,
        });
    }

    /// Drena los eventos del hilo de sesión y los aplica al estado (UN solo
    /// `cx.spawn` para toda la vida de la ventana). Sale si la entidad muere.
    fn spawn_event_loop(
        &self,
        mut rx: tokio::sync::mpsc::UnboundedReceiver<SessionEvent>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            while let Some(ev) = rx.recv().await {
                let alive = this
                    .update(cx, |view, cx| {
                        view.apply_event(ev, cx);
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    /// Aplica UN evento de sesión al estado.
    fn apply_event(&mut self, ev: SessionEvent, cx: &mut Context<Self>) {
        match ev {
            SessionEvent::Listed {
                pane,
                generation,
                dir,
                outcome,
            } => {
                // GUARD de generación: si el pane ya está en un cd más nuevo,
                // este resultado es stale y se DESCARTA (no pisa el listado
                // vigente ni resetea cursor/filtro).
                if !generation_is_current(self.generation[pane], generation) {
                    if std::env::var_os("NORTE_GUI_DEBUG").is_some() {
                        eprintln!(
                            "[norte-gui] pane {pane}: resultado stale (gen {generation} != {}) descartado",
                            self.generation[pane],
                        );
                    }
                    return; // stale: un cd más nuevo ya avanzó la generación.
                }
                match outcome {
                    Ok(entries) => {
                        let mut entries = entries;
                        norte_frontend::sort_entries(&mut entries);
                        self.panes[pane].set_listing(dir, entries);
                        self.errors[pane] = None;
                        self.query[pane].clear();
                        if std::env::var_os("NORTE_GUI_DEBUG").is_some() {
                            eprintln!(
                                "[norte-gui] pane {pane} aplicó listado: {} entradas, cursor={}",
                                self.panes[pane].entries().len(),
                                self.panes[pane].cursor(),
                            );
                        }
                    }
                    Err(msg) => {
                        self.panes[pane].set_listing(dir, Vec::new());
                        self.errors[pane] = Some(msg);
                    }
                }
                // Coalesce (#84): si se saltó un relist mientras este list volaba,
                // re-relista ahora (una sola vez; el dir ya no está `loading`).
                if self.relist_pending[pane] {
                    self.relist_pending[pane] = false;
                    let cur = self.panes[pane].dir().clone();
                    self.cd(pane, cur, cx);
                }
            }
            SessionEvent::Submitted { task_id, op } => {
                self.inflight.insert(task_id, op);
            }
            SessionEvent::SubmitFailed { op, error } => {
                // Rechazo inmediato: banner en el pane activo (los conflictos
                // reales llegan por Task terminal Failed, ver abajo). El
                // `Display` del `Error` categórico es seguro de interpolar
                // (taxonomía cerrada, sin bytes crudos — auditado en GUI-b).
                let error = error.to_string();
                self.errors[self.focus] = Some(norte_i18n::ta(
                    "gui-banner-op-rejected",
                    &[("error", error.as_str())],
                ));
                let _ = op; // la op no se reintenta automáticamente.
            }
            SessionEvent::Task(p) => {
                let id = p.task_id;
                let terminal = p.state.is_terminal();
                let conflict = conflict_kind_of(&p.state);
                if !self.task_progress.contains_key(&id) {
                    self.task_order.push(id);
                }
                self.task_progress.insert(id, p);
                if terminal {
                    self.on_task_terminal(id, conflict, cx);
                    // Poda las tasks completadas OK (crecimiento acotado); los
                    // fallos/cancelaciones se quedan visibles (dismiss-key = deuda).
                    if matches!(
                        self.task_progress.get(&id).map(|p| &p.state),
                        Some(norte_proto::TaskState::Completed)
                    ) {
                        self.task_progress.remove(&id);
                        self.task_order.retain(|t| *t != id);
                        self.clamp_task_cursor();
                    }
                }
            }
            SessionEvent::ViewerOpened {
                path,
                content,
                generation,
            } => {
                // Guard anti-stale (como `Listed`): un open tardío (F3 dos
                // veces, o un read lento tras cerrar el visor) ya no coincide
                // con la generación vigente — se descarta sin tocar el
                // estado actual.
                if generation != self.viewer_gen {
                    return;
                }
                use norte_frontend::viewer::Viewer;
                use session::ViewerContent;
                self.viewer_loading = false;
                let v = match content {
                    ViewerContent::Plugin {
                        plugin_name,
                        output,
                    } => Viewer::with_plugin_preview(path, plugin_name, &output),
                    ViewerContent::Raw { bytes, truncated } => Viewer::new(path, bytes, truncated),
                };
                // Decodifica la imagen UNA vez (no en cada frame de render).
                self.viewer_image = v.image_bytes().map(decode_image_preview);
                self.viewer = Some(v);
            }
            SessionEvent::ViewerFailed {
                path,
                error,
                generation,
            } => {
                if generation != self.viewer_gen {
                    return;
                }
                self.viewer_loading = false;
                let (name, hostile) = norte_frontend::path_display(&path);
                let name = if hostile {
                    format!("{HOSTILE_BADGE} {name}")
                } else {
                    name
                };
                self.errors[self.focus] = Some(norte_i18n::ta(
                    "gui-banner-viewer-error",
                    &[("name", name.as_str()), ("error", error.as_str())],
                ));
            }
            SessionEvent::ConnectFailed(msg) => {
                // Sin esto `loading` queda clavado en `true` (nunca llega un
                // `Listed` que lo baje) y el render pinta "cargando…" para
                // siempre, suprimiendo el banner de error (ver `render_pane`).
                // `set_listing` baja `loading` y vacía entries preservando el
                // `dir` vigente del pane.
                for pane in 0..2 {
                    let dir = self.panes[pane].dir().clone();
                    self.panes[pane].set_listing(dir, Vec::new());
                    self.errors[pane] = Some(msg.clone());
                }
            }
        }
    }

    /// Abre el modal de copia/movimiento: origen = marcas/cursor del pane
    /// activo, destino = dir del pane inactivo. No-op si no hay nada que mover.
    fn open_transfer_modal(&mut self, kind: TransferKind) {
        let f = self.focus;
        let items = self.panes[f].marked_paths();
        if items.is_empty() {
            return;
        }
        let to = self.panes[1 - f].dir().clone();
        self.modal = Some(Modal::ConfirmTransfer { kind, items, to });
    }

    /// Abre el modal de borrado sobre las marcas/cursor del pane activo.
    fn open_delete_modal(&mut self) {
        let f = self.focus;
        let items = self.panes[f].marked_paths();
        if items.is_empty() {
            return;
        }
        self.modal = Some(Modal::ConfirmDelete {
            items,
            permanent: false,
        });
    }

    /// Abre el modal de conflicto, o lo encola si ya hay un modal abierto.
    fn queue_conflict(&mut self, pending: PendingTransfer, conflict: norte_proto::ConflictKind) {
        if self.modal.is_some() {
            self.conflict_backlog.push((pending, conflict));
        } else {
            self.modal = Some(Modal::ConflictResolve { pending, conflict });
        }
    }

    /// Al cerrar un modal, abre el siguiente conflicto encolado (si hay).
    fn open_next_conflict(&mut self) {
        if self.modal.is_some() {
            return;
        }
        if let Some((pending, conflict)) = self.conflict_backlog.pop() {
            self.modal = Some(Modal::ConflictResolve { pending, conflict });
        }
    }

    /// Una task llegó a terminal: si falló por conflicto, encola/abre el modal
    /// de resolución; en éxito/cancelación/fallo-no-conflicto relista los dirs
    /// afectados (read-after-write). Retira la op de `inflight` en todos los
    /// caminos.
    fn on_task_terminal(
        &mut self,
        id: norte_proto::TaskId,
        conflict: Option<norte_proto::ConflictKind>,
        cx: &mut Context<Self>,
    ) {
        let Some(op) = self.inflight.remove(&id) else {
            return;
        };
        if let Some(kind) = conflict {
            if let PendingOp::Transfer {
                kind: tk, from, to, ..
            } = op
            {
                self.queue_conflict(PendingTransfer { kind: tk, from, to }, kind);
            }
            return;
        }
        // Éxito/cancelación/fallo-no-conflicto: relista los dirs afectados
        // (read-after-write).
        self.relist_dirs(&affected_dirs(&op), cx);
    }

    /// Relista cualquier pane cuyo `dir` esté en `dirs` (read-after-write).
    ///
    /// #84: si el pane YA está cargando ese mismo dir, NO duplica la list —
    /// marca `relist_pending` para RE-relistar cuando aterrice (la list en
    /// vuelo pudo leer el dir antes de escrituras posteriores del burst). Un
    /// burst de N tasks terminales sobre el mismo dir coalesce a la list en
    /// vuelo + UNA re-list final, no N redundantes, sin dejar el pane stale.
    fn relist_dirs(&mut self, dirs: &[VPath], cx: &mut Context<Self>) {
        for pane in 0..2 {
            let cur = self.panes[pane].dir().clone();
            if !dirs.contains(&cur) {
                continue;
            }
            if self.panes[pane].loading() {
                // Coalesce (#84): ya hay una list en vuelo para este dir. NO la
                // dupliques, pero MARCA que hay que re-relistar al aterrizar —
                // esa list pudo leer el dir ANTES de las escrituras de este
                // burst; el re-relist final garantiza ver el estado completo.
                self.relist_pending[pane] = true;
            } else {
                self.cd(pane, cur, cx);
            }
        }
    }

    /// Tras mover el cursor de `pane`, hace que la lista virtualizada
    /// (`uniform_list`, issue #87) lo mantenga visible — scroll no-estricto:
    /// no-op si ya está en pantalla.
    fn follow_cursor(&self, pane: usize) {
        self.scrolls[pane].scroll_to_item(self.panes[pane].cursor(), ScrollStrategy::Nearest);
    }

    /// `nav.enter`: confirma el quick search si está abierto (fija el cursor
    /// real al match) y, si la entrada resultante es un directorio, hace `cd`.
    /// Extraído del viejo brazo `Action::Enter` de `on_key` (GUI-c T3).
    fn activate_enter(&mut self, cx: &mut Context<Self>) {
        let f = self.focus;
        let target = {
            let pane = &mut self.panes[f];
            if pane.quick_visible().is_some() {
                pane.quick_confirm();
            }
            pane.selected()
                .filter(|e| e.kind == EntryKind::Dir)
                .map(|e| e.path.clone())
        };
        self.query[f].clear();
        // `quick_confirm` fija el cursor real al índice absoluto del match;
        // si es un archivo (sin `cd`) la lista se re-renderiza con el cursor
        // movido pero el scroll quedaría arriba — inocuo llamarlo siempre: un
        // `cd` también resetea el scroll (review, caso borde #2).
        self.follow_cursor(f);
        if let Some(dir) = target {
            self.cd(f, dir, cx);
        }
    }

    /// `task.cancel` (#91): cancela la task bajo el cursor de la franja
    /// (`task_cursor`) SI no es terminal; si la franja está vacía o el cursor
    /// apunta a una terminal, cae a la primera cancelable ([`first_cancelable`])
    /// — F9 siempre hace algo sensato. La selección bajo cursor vive en
    /// [`task_at_cursor`] (pura, #91); el fallback en [`first_cancelable`]
    /// (pura, #85).
    fn cancel_task_under_cursor(&mut self) {
        let under_cursor = task_at_cursor(&self.task_order, self.task_cursor).filter(|id| {
            self.task_progress
                .get(id)
                .is_some_and(|p| !p.state.is_terminal())
        });
        let target =
            under_cursor.or_else(|| first_cancelable(&self.task_order, &self.task_progress));
        if let Some(id) = target {
            let _ = self.cmds.send(SessionCmd::Cancel(id));
        }
    }

    /// Clampa `task_cursor` a un índice válido de `task_order` tras podar la
    /// franja (Completed autopodadas / `task.dismiss`): si el cursor quedó
    /// más allá del último, lo baja al último (o a 0 si la franja se vació).
    /// Así ni `cancel_task_under_cursor` ni `render_task_strip` indexan fuera
    /// de rango.
    fn clamp_task_cursor(&mut self) {
        let max = self.task_order.len().saturating_sub(1);
        if self.task_cursor > max {
            self.task_cursor = max;
        }
    }

    /// `task.dismiss` (#83): quita de la franja TODAS las tasks TERMINALES
    /// (`state.is_terminal()`). Las `Completed` ya se autopodan al llegar
    /// (ver `apply_event`); esto cubre `Failed`/`Cancelled`, que hasta ahora
    /// se acumulaban en la franja para siempre. Delega la mutación pura a
    /// [`retain_active`] (testeable sin GPUI).
    fn dismiss_terminal_tasks(&mut self) {
        retain_active(&mut self.task_order, &mut self.task_progress);
        self.clamp_task_cursor();
    }

    /// Ejecuta un comando del keymap (contexto Browse) sobre el estado.
    /// Reemplaza el `key_to_action` hardcodeado para las acciones nombradas
    /// (GUI-c T3): `on_key` resuelve la tecla vía `resolver` y llama aquí.
    fn run_command(&mut self, cmd: &str, cx: &mut Context<Self>) {
        let f = self.focus;
        match cmd {
            "app.quit" => cx.quit(),
            "pane.switch" => self.focus = 1 - self.focus,
            "cursor.up" => self.panes[f].cursor_up(),
            "cursor.down" => self.panes[f].cursor_down(),
            "cursor.top" => self.panes[f].home(),
            "cursor.bottom" => self.panes[f].end(),
            "cursor.page-up" => self.panes[f].page_up(PAGE),
            "cursor.page-down" => self.panes[f].page_down(PAGE),
            "nav.enter" => self.activate_enter(cx),
            "nav.parent" => {
                if let Some(p) = self.panes[f].dir().parent() {
                    self.cd(f, p, cx);
                }
            }
            "mark.toggle" => self.panes[f].toggle_mark(),
            "pane.copy" => self.open_transfer_modal(TransferKind::Copy),
            "pane.move" => self.open_transfer_modal(TransferKind::Move),
            "pane.delete" => self.open_delete_modal(),
            "task.cancel" => self.cancel_task_under_cursor(),
            "task.next" => {
                if !self.task_order.is_empty() {
                    self.task_cursor = (self.task_cursor + 1).min(self.task_order.len() - 1);
                }
            }
            "task.prev" => self.task_cursor = self.task_cursor.saturating_sub(1),
            "task.dismiss" => self.dismiss_terminal_tasks(),
            "pane.view" => self.open_viewer(cx),
            _ => {} // comando desconocido en runtime: no-op (el keymap ya validó)
        }
        // Tras un movimiento de cursor, sigue el scroll (issue #87).
        self.follow_cursor(f);
    }

    /// Abre el visor sobre la entrada seleccionada si es un archivo (F3 sobre
    /// un dir/symlink/otro = no-op — el visor solo lee archivos). Avanza
    /// `viewer_gen` (invalida cualquier open anterior en vuelo — dos F3
    /// seguidos no encolan dos aperturas) y marca `viewer_loading` para el
    /// estado «abriendo visor…» del render mientras llega la respuesta.
    fn open_viewer(&mut self, _cx: &mut Context<Self>) {
        let f = self.focus;
        if let Some(e) = self.panes[f].selected()
            && e.kind == EntryKind::File
        {
            self.viewer_gen = self.viewer_gen.wrapping_add(1);
            self.viewer_loading = true;
            let _ = self.cmds.send(SessionCmd::OpenViewer {
                path: e.path.clone(),
                generation: self.viewer_gen,
            });
        }
    }

    /// Ejecuta un comando del contexto Viewer sobre `self.viewer`: delega el
    /// efecto puro a [`apply_viewer_command`] (testeable sin GPUI). `v.scroll`
    /// es el ÚNICO dueño del scroll (sin `uniform_list`/handle de por medio,
    /// ver el campo `viewer` del struct) — no hace falta seguir nada aparte.
    /// `"viewer.close"` también avanza `viewer_gen` y baja `viewer_loading`:
    /// así un `ViewerOpened` tardío que llegue DESPUÉS de cerrar no reabre
    /// por sorpresa (su generación ya quedó vieja).
    fn run_viewer_command(&mut self, cmd: &str, _cx: &mut Context<Self>) {
        let Some(v) = self.viewer.as_mut() else {
            return;
        };
        if !apply_viewer_command(v, cmd) {
            self.viewer = None;
            self.viewer_image = None;
            self.viewer_gen = self.viewer_gen.wrapping_add(1);
            self.viewer_loading = false;
        }
    }

    /// Maneja UNA tecla dentro del quick search (filtro activo): tipeo →
    /// filtro, Backspace lo acorta, Esc lo cancela, ↑↓ mueven la selección
    /// del quick, Enter confirma. Devuelve `true` si la tecla se consumió
    /// (el caller NO debe pasarla al resolver de keymap) — extraído del
    /// viejo despacho de `Action::{Char,Backspace,Esc,Up,Down,Enter}` de
    /// `on_key` (GUI-c T3, contrato del plan: quick abierto = fallthrough,
    /// no binding).
    fn quick_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) -> bool {
        let f = self.focus;
        match ks.key.as_str() {
            "backspace" => {
                self.panes[f].quick_backspace();
                self.query[f].pop();
                true
            }
            "escape" => {
                self.panes[f].quick_cancel();
                self.query[f].clear();
                true
            }
            "up" => {
                self.panes[f].quick_up();
                true
            }
            "down" => {
                self.panes[f].quick_down();
                true
            }
            "enter" => {
                self.activate_enter(cx);
                true
            }
            // Home/End/Page saltan el cursor REAL: sin efecto con el filtro
            // abierto (la selección vive en el quick, que solo tiene ↑↓) —
            // se CONSUMEN aquí como no-op, no caen al keymap (que sí movería
            // el cursor real por debajo del filtro).
            "home" | "end" | "pageup" | "pagedown" => true,
            _ => {
                // Imprimible: fidelidad al carácter REALMENTE tecleado
                // (`key_char`, respeta shift/layout); `"space"` llega con
                // nombre, no como carácter suelto.
                let ch = if ks.key == "space" {
                    Some(' ')
                } else {
                    single_char(ks.key_char.as_deref()).or_else(|| single_char(Some(&ks.key)))
                };
                match ch {
                    Some(c) if !c.is_control() => {
                        self.panes[f].quick_char(c);
                        self.query[f].push(c);
                        true
                    }
                    _ => false,
                }
            }
        }
    }

    /// Sin binding de keymap (`Resolution::Reset`) y quick CERRADO: un
    /// carácter imprimible ALFANUMÉRICO abre el quick search (mismo criterio
    /// que el viejo `input::printable` con `quick_active = false` — un signo
    /// de puntuación o el espacio sueltos NO abren búsqueda por accidente).
    /// Teclas de navegación con nombre (`"up"`, `"f5"`…) nunca llegan aquí
    /// con más de un carácter, así que el filtro por longitud ya las excluye.
    fn maybe_open_quick(&mut self, ks: &gpui::Keystroke) {
        let f = self.focus;
        let ch = single_char(ks.key_char.as_deref()).or_else(|| single_char(Some(&ks.key)));
        if let Some(c) = ch
            && c.is_alphanumeric()
        {
            self.panes[f].quick_start(Mode::Filter);
            self.query[f].clear();
            self.panes[f].quick_char(c);
            self.query[f].push(c);
        }
    }

    /// Maneja una tecla en el pane con foco (GUI-c T3, contrato del plan):
    /// (1) modal abierto → captura fija; (2) quick search activo (sin
    /// ctrl/alt) → fallthrough al filtro (`quick_key`); (3) si no,
    /// `keymap::gpui_chord` → `resolver.push` → `run_command`, o
    /// `maybe_open_quick` si es un imprimible sin binding.
    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;

        // Volcado estructural del árbol a11y (Step 3, GUI-e T2): F12 SOLO
        // bajo `NORTE_GUI_DEBUG` (si la variable no está, F12 sigue su curso
        // normal — no está en el preset orthodox, así que hoy es no-op, sin
        // regresión). Permite inspeccionar roles/labels sin un lector de
        // pantalla real. `is_a11y_active()` refleja si el bus AT-SPI activó
        // DE VERDAD el árbol — GPUI no expone una forma de forzarlo desde la
        // app (solo `App::new_inaccessible` para forzar lo contrario), así
        // que en un dev box sin AT conectado el volcado trae normalmente solo
        // el nodo raíz. Para forzarlo en Linux sin instalar un lector de
        // pantalla, se puede activar la propiedad que dispara la activación
        // en `accesskit_unix` (`ScreenReaderEnabled` en `org.a11y.Status`):
        //   busctl --user set-property org.a11y.Bus /org/a11y/bus \
        //     org.a11y.Status ScreenReaderEnabled b true
        // Verificación con AT real (orca/AT-SPI) queda para oscar (Linux).
        if std::env::var_os("NORTE_GUI_DEBUG").is_some() && ks.key == "f12" {
            eprintln!(
                "[norte-gui] a11y activo={} árbol={}",
                window.is_a11y_active(),
                window
                    .debug_a11y_tree_json()
                    .unwrap_or_else(|| "(sin datos)".to_string()),
            );
            cx.notify();
            return;
        }

        // Con un modal abierto, la tecla va al modal (captura fija).
        if let Some(m) = &mut self.modal {
            match modal::on_key(m, &ks.key) {
                ModalOutcome::Ignored | ModalOutcome::StayOpen => {}
                ModalOutcome::Dismiss => {
                    self.modal = None;
                    self.open_next_conflict();
                }
                ModalOutcome::Submit(ops) => {
                    self.modal = None;
                    for op in ops {
                        let _ = self.cmds.send(SessionCmd::Submit(op));
                    }
                    self.open_next_conflict();
                }
            }
            cx.notify();
            return;
        }

        // Visor abierto: las teclas van al contexto Viewer (no hay modal/quick
        // aquí — el visor y el dual-pane son pantallas mutuamente excluyentes).
        if self.viewer.is_some() {
            if ks.modifiers.platform {
                cx.notify();
                return;
            }
            if let Some(chord) = keymap::gpui_chord(
                &ks.key,
                ks.modifiers.control,
                ks.modifiers.alt,
                ks.modifiers.shift,
                ks.key_char.as_deref(),
            ) {
                match self.viewer_resolver.push(chord) {
                    norte_frontend::keymap::Resolution::Run(cmd) => {
                        self.run_viewer_command(&cmd, cx);
                    }
                    norte_frontend::keymap::Resolution::Pending(_) => {}
                    norte_frontend::keymap::Resolution::Reset => {}
                }
            } else {
                self.viewer_resolver.reset();
            }
            cx.notify();
            return;
        }

        let f = self.focus;
        let quick_active = self.panes[f].quick_visible().is_some();
        let mods = ks.modifiers;
        let mut resolution_dbg = "n/a";

        // (2) Quick search activo Y sin ctrl/alt: el tipeo va al FILTRO, no
        // al keymap (un ctrl+algo con el filtro abierto sigue siendo un
        // comando — p. ej. ctrl+c). Si `quick_key` consume la tecla, termina
        // aquí (contrato del plan GUI-c T3).
        if quick_active && !(mods.control || mods.alt || mods.platform) && self.quick_key(ks, cx) {
            self.debug_log_key(&ks.key, "quick");
            cx.notify();
            return;
        }

        // Super/Cmd no lo modela el keymap (`gpui_chord` solo recibe
        // ctrl/alt/shift): no lo rutees al resolver — evita que Cmd+q
        // colapse al chord `q` desnudo y dispare su binding (MINOR 2,
        // review T3).
        if mods.platform {
            cx.notify();
            return;
        }

        // (3) keymap: nombre GPUI → Chord → resolver.
        if let Some(chord) = keymap::gpui_chord(
            &ks.key,
            mods.control,
            mods.alt,
            mods.shift,
            ks.key_char.as_deref(),
        ) {
            match self.resolver.push(chord) {
                norte_frontend::keymap::Resolution::Run(cmd) => {
                    resolution_dbg = "run";
                    self.run_command(&cmd, cx);
                }
                norte_frontend::keymap::Resolution::Pending(_) => {
                    // Secuencia en curso: nada que ejecutar todavía. El
                    // indicador de secuencia pendiente lo pinta `render` al
                    // pie leyendo `resolver.pending()` (#91).
                    resolution_dbg = "pending";
                }
                norte_frontend::keymap::Resolution::Reset => {
                    resolution_dbg = "reset";
                    // Sin binding: si es un imprimible sin ctrl/alt/super,
                    // ABRE el quick search.
                    if !(mods.control || mods.alt || mods.platform) {
                        self.maybe_open_quick(ks);
                    }
                }
            }
        } else {
            // El adaptador no modela la tecla (rara/exótica): rompe
            // cualquier secuencia pendiente, como un Miss del resolver
            // (ver el test `reset_rompe_la_secuencia_pendiente` del motor).
            self.resolver.reset();
        }

        self.debug_log_key(&ks.key, resolution_dbg);
        cx.notify();
    }

    /// Log de diagnóstico (`NORTE_GUI_DEBUG`) de una tecla procesada: la
    /// tecla cruda de GPUI + qué rama la resolvió (`"quick"`/`"run"`/
    /// `"pending"`/`"reset"`), más el estado del pane con foco. Extraído del
    /// viejo log inline de `on_key` (GUI-c T3: ya no hay un `Action` único
    /// que loguear).
    fn debug_log_key(&self, key: &str, via: &str) {
        if std::env::var_os("NORTE_GUI_DEBUG").is_none() {
            return;
        }
        let nf = self.focus;
        let pane = &self.panes[nf];
        let sel = pane
            .selected()
            .and_then(|e| e.path.file_name())
            .map(|s| String::from_utf8_lossy(s.as_bytes()).into_owned())
            .unwrap_or_default();
        eprintln!(
            "[norte-gui] key={key:?} via={via} | focus={nf} dir={} cursor={} quick={:?} sel={sel:?}",
            pane.dir(),
            pane.cursor(),
            self.query[nf],
        );
    }

    /// Click en una fila: foco a ese pane + cursor a esa fila; doble-click sobre
    /// un directorio hace `cd`.
    fn on_row_click(
        &mut self,
        pane: usize,
        idx: usize,
        dir_target: Option<VPath>,
        click_count: usize,
        cx: &mut Context<Self>,
    ) {
        self.focus = pane;
        // Un click cancela cualquier filtro (el cursor real vuelve a mandar) y
        // se posa en `idx`: sin `set_cursor` en PaneState, se emula home + page.
        self.panes[pane].quick_cancel();
        self.query[pane].clear();
        self.panes[pane].home();
        self.panes[pane].page_down(idx);
        self.follow_cursor(pane);
        if click_count >= 2
            && let Some(dir) = dir_target
        {
            self.cd(pane, dir, cx);
        }
        cx.notify();
    }

    /// Rueda del ratón sobre un pane: le da el foco y mueve el cursor. Con
    /// filtro quick activo NO mueve el cursor real (mismo guard que
    /// `on_key`): la lista virtualizada indexa por posición VISIBLE
    /// (`0..vis.len()`), pero `follow_cursor` usa el cursor ABSOLUTO — sin
    /// este guard la rueda desplazaría la lista filtrada a una posición
    /// espuria (review, caso borde #1).
    fn on_pane_scroll(&mut self, pane: usize, delta: ScrollDelta, cx: &mut Context<Self>) {
        self.focus = pane;
        if self.panes[pane].quick_visible().is_none() {
            let y = scroll_y(delta);
            if y > 0.0 {
                self.panes[pane].cursor_up();
            } else if y < 0.0 {
                self.panes[pane].cursor_down();
            }
            self.follow_cursor(pane);
        }
        cx.notify();
    }

    /// Rueda del ratón sobre el visor abierto: mueve `v.scroll` (única fuente
    /// de verdad del scroll del visor, ver el campo `viewer`). No-op si el
    /// visor no está abierto (guard defensivo; en la práctica solo se
    /// registra sobre el contenedor de `render_viewer`).
    fn on_viewer_scroll(&mut self, delta: ScrollDelta, cx: &mut Context<Self>) {
        if let Some(v) = self.viewer.as_mut() {
            let y = scroll_y(delta);
            if y > 0.0 {
                v.scroll_up(1);
            } else if y < 0.0 {
                v.scroll_down(1);
            }
        }
        cx.notify();
    }

    /// Pinta una columna (un pane).
    fn render_pane(&self, i: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let pane = &self.panes[i];
        let focused = self.focus == i;

        let (path_txt, path_hostile) = norte_frontend::path_display(pane.dir());
        let header = if path_hostile {
            format!("{HOSTILE_BADGE} {path_txt}")
        } else {
            path_txt
        };

        // Cuenta de items de la lista virtualizada (issue #87): respeta el
        // filtro quick (solo los índices visibles) o TODAS las entradas.
        // `uniform_list` solo invoca el processor de abajo para el rango
        // VISIBLE, así que esto es O(1) por frame — el O(N) desapareció.
        let item_count = pane
            .quick_visible()
            .map_or_else(|| pane.entries().len(), <[usize]>::len);

        let list = uniform_list(
            SharedString::from(format!("entries-{i}")),
            item_count,
            cx.processor(move |this, range: Range<usize>, _window, cx| {
                let pane = &this.panes[i];
                let sel_path = pane.selected().map(|e| e.path.clone());
                // Mapea el rango (índices dentro de la lista VISIBLE) a índices
                // ABSOLUTOS de `entries()`, respetando el filtro quick.
                let abs: Vec<usize> = match pane.quick_visible() {
                    Some(vis) => range.filter_map(|k| vis.get(k).copied()).collect(),
                    None => range.collect(),
                };
                abs.into_iter()
                    .map(|j| {
                        // Clona la entrada (barata: VPath + kind + dos
                        // Option) para no retener un préstamo de `this.panes`
                        // mientras se llama a `this.render_row` más abajo.
                        let e = this.panes[i].entries()[j].clone();
                        let hl = sel_path.as_ref() == Some(&e.path);
                        let marked = this.panes[i].is_marked(&e);
                        this.render_row(i, j, &e, hl, marked, cx)
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.scrolls[i])
        .flex_1();

        // Envuelve la lista (NO el `uniform_list` directamente: su propio id
        // "entries-{i}" alimenta el scroll/measure virtualizado — pisarlo con
        // `.id()` para colgar el role sería arriesgar esa identidad) en un div
        // `Role::List` con el nombre accesible «panel izquierdo/derecho»
        // (i18n `gui-a11y-pane-*`, GUI-e T2). El pane con foco se marca
        // `aria_selected` (el primitivo disponible más cercano a "lista
        // activa" — no hay un rol dedicado de "pane" en AccessKit).
        let pane_a11y_label = norte_i18n::t(if i == 0 {
            "gui-a11y-pane-left"
        } else {
            "gui-a11y-pane-right"
        });
        let list = div()
            .id(format!("pane-list-{i}"))
            .role(gpui::Role::List)
            .aria_label(pane_a11y_label)
            .aria_selected(focused)
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(list);

        let mut col = div()
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .border_2()
            .border_color(if focused {
                rgb(BORDER_FOCUS)
            } else {
                rgb(BORDER_UNFOCUS)
            })
            .bg(if focused {
                rgb(PANE_BG_FOCUS)
            } else {
                rgb(PANE_BG)
            });

        // Cabecera: el path saneado del dir.
        col = col.child(
            div()
                .px(px(4.0))
                .py(px(2.0))
                .bg(rgb(HEADER_BG))
                .truncate()
                .child(SharedString::from(header)),
        );

        // Estado transitorio: cargando / error / vacío.
        if pane.loading() {
            col = col.child(
                div()
                    .px(px(4.0))
                    .child(SharedString::from(norte_i18n::t("gui-loading"))),
            );
        } else if let Some(err) = &self.errors[i] {
            col = col.child(
                div()
                    .px(px(4.0))
                    .text_color(rgb(ERR_FG))
                    .child(SharedString::from(norte_i18n::ta(
                        "gui-banner-error",
                        &[("error", err.as_str())],
                    ))),
            );
        } else if pane.entries().is_empty() {
            col = col.child(
                div()
                    .px(px(4.0))
                    .child(SharedString::from(norte_i18n::t("gui-dir-empty"))),
            );
        }

        // Lista de entradas, virtualizada (issue #87): `uniform_list` solo
        // construye el rango visible, no las N entradas del dir.
        col = col.child(list);

        // Línea `/{query}` al pie si el quick search está activo. La query la
        // tecleó el usuario, pero con IME/paste puede llegar con bidi/invisibles
        // crudos (review encoding BAJA): se sanea AL PINTAR, una sola vez, con
        // el mismo criterio que `display_name` (`is_terminal_hazard`) — nunca
        // se guarda saneada porque el filtro compara contra el nombre real.
        if pane.quick_visible().is_some() {
            let query_display: String = self.query[i]
                .chars()
                .map(|c| {
                    if norte_encoding::is_terminal_hazard(c) {
                        '\u{FFFD}'
                    } else {
                        c
                    }
                })
                .collect();
            col = col.child(
                div()
                    .px(px(4.0))
                    .py(px(1.0))
                    .bg(rgb(HEADER_BG))
                    .text_color(rgb(QUICK_FG))
                    .child(SharedString::from(format!("/{query_display}"))),
            );
        }

        // Rueda + click en zona vacía de la columna dan foco a este pane.
        col.on_scroll_wheel(cx.listener(move |this, ev: &ScrollWheelEvent, _w, cx| {
            this.on_pane_scroll(i, ev.delta, cx);
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _ev: &MouseDownEvent, _w, cx| {
                this.focus = i;
                cx.notify();
            }),
        )
    }

    /// Pinta una fila: marcador de marca + badge hostil + nombre saneado +
    /// indicador de tipo, coloreado por tipo de archivo; fondo distinto si está
    /// marcada, resaltado (que gana) si es la selección bajo cursor.
    fn render_row(
        &self,
        pane: usize,
        idx: usize,
        entry: &Entry,
        highlighted: bool,
        marked: bool,
        cx: &mut Context<Self>,
        // ed. 2024: RPIT captura TODOS los lifetimes en scope; `render_row` NO
        // retiene préstamos (clona nombre/color, el listener es 'static), así
        // que acota la captura a vacío para no atrapar el `&entry` (que en el
        // processor de `uniform_list` es un clon local que escaparía).
    ) -> impl IntoElement + use<> {
        let bytes = entry.path.file_name().map_or(&b""[..], Segment::as_bytes);
        let mut label = row_label(bytes, entry.kind);
        if marked {
            label = format!("{MARK_MARKER} {label}");
        }
        let color = entry_color(&self.theme, entry);
        let dir_target = (entry.kind == EntryKind::Dir).then(|| entry.path.clone());

        // `Role::ListItem` con nombre accesible = `label` YA saneado (mismo
        // texto que se pinta — regla 1, jamás bytes crudos). La selección
        // bajo cursor (`highlighted`) es `aria_selected`; la marca de
        // multi-selección (`marked`, ortogonal a la selección) es
        // `aria_toggled`, como pide el plan de GUI-e T2.
        let mut row = div()
            .id(format!("row-{pane}-{idx}"))
            .role(gpui::Role::ListItem)
            .aria_label(label.clone())
            .aria_selected(highlighted)
            .aria_toggled(if marked {
                gpui::Toggled::True
            } else {
                gpui::Toggled::False
            })
            .h(px(ROW_H))
            .px(px(4.0))
            .py(px(1.0))
            .text_color(color)
            .truncate()
            .child(SharedString::from(label));
        if marked {
            row = row.bg(rgb(MARK_BG));
        }
        if highlighted {
            row = row.bg(rgb(SEL_BG));
        }
        row.on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
                this.on_row_click(pane, idx, dir_target.clone(), ev.click_count, cx);
            }),
        )
    }

    /// Franja de tasks al pie: una fila por task en orden de llegada (kind + % +
    /// estado + entrada en curso saneada). La fila bajo el cursor de franja
    /// (`task_cursor`, #91) se resalta (`SEL_BG`, como el cursor de pane) y se
    /// marca `aria_selected`; F9 cancela esa fila. El cursor se clampa aquí a
    /// un índice válido (la franja encoge al podar Completed/dismiss) — jamás
    /// indexa fuera de rango.
    fn render_task_strip(&self) -> impl IntoElement {
        let mut strip = div()
            .id("task-strip")
            .role(gpui::Role::List)
            .aria_label(norte_i18n::t("gui-a11y-tasks"))
            .flex()
            .flex_col()
            .max_h(px(120.0))
            .overflow_hidden()
            .bg(rgb(HEADER_BG))
            .px(px(4.0))
            .py(px(2.0));
        if self.task_order.is_empty() {
            return strip.child(SharedString::from(norte_i18n::t("gui-tasks-empty")));
        }
        // Clamp defensivo por si el cursor quedó tras la última poda antes de un
        // relayout (el campo se clampa al podar, pero render no debe asumirlo).
        let cursor = self.task_cursor.min(self.task_order.len() - 1);
        for (i, id) in self.task_order.iter().enumerate() {
            let Some(p) = self.task_progress.get(id) else {
                continue;
            };
            let line = task_line(p);
            let selected = i == cursor;
            let mut row = div()
                .id(format!("task-row-{}", id.get()))
                .role(gpui::Role::ListItem)
                .aria_label(line.clone())
                .aria_selected(selected)
                .px(px(2.0))
                .child(SharedString::from(line));
            if selected {
                row = row.bg(rgb(SEL_BG));
            }
            strip = strip.child(row);
        }
        strip
    }

    /// Pinta el visor a pantalla COMPLETA (F3): cabecera (path saneado +
    /// «via <plugin>» si es preview de plugin) + `v.rows(h)` (una fila por
    /// `div`, SIN `uniform_list`: `Viewer::rows(height)` ya está acotado a
    /// `height` filas — ventana desde `v.scroll`, O(H) no O(total) — issue
    /// #87 no aplica aquí, a diferencia de los panes, que sí listan TODAS las
    /// entradas del dir) + barra de estado (`viewer_status`, fn pura). `h` se
    /// deriva del alto real del viewport de la ventana. `v.scroll` es el
    /// ÚNICO dueño del scroll (la rueda lo mueve vía `on_viewer_scroll`,
    /// registrada sobre el contenedor).
    fn render_viewer(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        // INVARIANTE: solo se llama desde `render` cuando `self.viewer` es
        // `Some` (comprobado justo antes de esta llamada).
        let v = self
            .viewer
            .as_ref()
            .expect("render_viewer: self.viewer es Some (invariante del caller, ver `render`)");

        let header = viewer_header(v);
        // En modo imagen el estado lo compone `image_status` (formato + dims o
        // «ilegible»); si no, el estado de texto/hex habitual.
        let status = if v.is_image() {
            image_status(v, self.viewer_image.as_ref())
        } else {
            viewer_status(v)
        };

        // Filas que caben en el viewport real, menos la cabecera+status
        // (`VIEWER_CHROME_ROWS`); el sobrante (redondeo, chrome del root) lo
        // recorta `overflow_hidden` del contenedor. Mínimo 1: una ventana
        // minúscula no debe pedir un rango vacío a `v.rows`.
        let viewport_rows = (f32::from(window.viewport_size().height) / ROW_H) as usize;
        let h = viewport_rows.saturating_sub(VIEWER_CHROME_ROWS).max(1);

        // Cuerpo del visor: en modo imagen, el elemento `img` (o un aviso si el
        // decode falló); si no, las filas de texto/hex. `object_fit` de GPUI es
        // `Contain` por defecto → conserva el aspecto y centra sin recortar.
        let body =
            if v.is_image() {
                // Contenedor con tamaño real (`flex_1`) que centra el contenido; el
                // `img` se acota con `max_w_full`/`max_h_full` RELATIVOS a él (por
                // eso es hijo directo, sin envoltorio auto-dimensionado). `object_fit`
                // por defecto de GPUI es `Contain` → conserva el aspecto sin recortar.
                let container = div()
                    .flex_1()
                    .flex()
                    .justify_center()
                    .items_center()
                    .overflow_hidden();
                match &self.viewer_image {
                    Some(ImagePreview::Ready { image, .. }) => {
                        container.child(img(std::sync::Arc::clone(image)).max_w_full().max_h_full())
                    }
                    // `Unreadable` o cache ausente → aviso i18n, jamás panic/OOM.
                    _ => container.child(SharedString::from(norte_i18n::t(
                        "gui-viewer-image-unreadable",
                    ))),
                }
            } else {
                div().flex_1().flex().flex_col().overflow_hidden().children(
                    v.rows(h).into_iter().map(|row| {
                        div()
                            .h(px(ROW_H))
                            .px(px(4.0))
                            .truncate()
                            .child(SharedString::from(row))
                    }),
                )
            };

        // `Role::Document` + nombre accesible = cabecera saneada (path +
        // «via <plugin>» si aplica); la barra de estado va como
        // `aria_description` (información suplementaria, se anuncia DESPUÉS
        // del nombre — mismo criterio que el ejemplo de gpui para hints).
        div()
            .id("viewer")
            .role(gpui::Role::Document)
            .aria_label(header.clone())
            .aria_description(status.clone())
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .border_2()
            .border_color(rgb(BORDER_FOCUS))
            .bg(rgb(PANE_BG_FOCUS))
            .child(
                div()
                    .px(px(4.0))
                    .py(px(2.0))
                    .bg(rgb(HEADER_BG))
                    .truncate()
                    .child(SharedString::from(header)),
            )
            .child(body)
            .child(
                div()
                    .px(px(4.0))
                    .py(px(1.0))
                    .bg(rgb(HEADER_BG))
                    .text_color(rgb(QUICK_FG))
                    .truncate()
                    .child(SharedString::from(status)),
            )
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _w, cx| {
                this.on_viewer_scroll(ev.delta, cx);
            }))
    }

    /// Pinta el panel del modal activo (overlay centrado, ver `render`): título
    /// y cuerpo saneados por [`modal_lines`] (mapeados 1:1 a divs, sin volver a
    /// tocar bytes de usuario aquí), más el pie de teclas fijo por variante.
    fn render_modal(&self, m: &Modal) -> impl IntoElement {
        let lines = modal_lines(m);
        // Nombre accesible = título (1.ª línea, siempre presente); el resto
        // (cuerpo saneado por `modal_lines`, ítems/mode/conflict) va como
        // `aria_description` — un lector anuncia diálogo → título → cuerpo.
        // `.get(1..)` (no indexado directo) por si `lines` alguna vez trajera
        // solo el título (defensivo, sin panic).
        let a11y_label = lines.first().cloned().unwrap_or_default();
        let a11y_description = lines
            .get(1..)
            .map(|rest| rest.join("; "))
            .unwrap_or_default();
        let footer = norte_i18n::t(match m {
            Modal::ConfirmTransfer { .. } => "gui-modal-footer-transfer",
            Modal::ConfirmDelete { .. } => "gui-modal-footer-delete",
            Modal::ConflictResolve { .. } => "gui-modal-footer-conflict",
        });
        // La línea de modo (índice 1 en ConfirmDelete) se alerta en rojo si es
        // borrado PERMANENTE.
        let alert_line = matches!(
            m,
            Modal::ConfirmDelete {
                permanent: true,
                ..
            }
        )
        .then_some(1);

        let mut panel = div()
            .id("modal")
            .role(gpui::Role::Dialog)
            .aria_label(a11y_label)
            .aria_description(a11y_description)
            .flex()
            .flex_col()
            .min_w(px(360.0))
            .max_w(px(560.0))
            .max_h(px(360.0))
            .overflow_hidden()
            .border_2()
            .border_color(rgb(BORDER_FOCUS))
            .bg(rgb(HEADER_BG))
            .text_color(rgb(FG))
            .px(px(12.0))
            .py(px(8.0))
            .gap(px(2.0));

        let mut lines = lines.into_iter();
        if let Some(title) = lines.next() {
            panel = panel.child(
                div()
                    .px(px(2.0))
                    .py(px(1.0))
                    .bg(rgb(BORDER_FOCUS))
                    .text_color(rgb(FG))
                    .truncate()
                    .child(SharedString::from(title)),
            );
        }
        for (i, line) in lines.enumerate() {
            let mut row = div().truncate().child(SharedString::from(line));
            if Some(i + 1) == alert_line {
                row = row.text_color(rgb(ERR_FG));
            }
            panel = panel.child(row);
        }

        panel.child(
            div()
                .mt(px(4.0))
                .px(px(2.0))
                .py(px(1.0))
                .bg(rgb(SEL_BG))
                .child(SharedString::from(footer)),
        )
    }
}

/// El primer carácter de `s` si `s` es exactamente uno.
fn single_char(s: Option<&str>) -> Option<char> {
    let s = s?;
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// El `ConflictKind` de un estado terminal fallido por conflicto, o `None` si
/// el estado no es `Failed{Conflict}`. Puro: testeable sin GPUI.
#[must_use]
fn conflict_kind_of(state: &norte_proto::TaskState) -> Option<norte_proto::ConflictKind> {
    match state {
        norte_proto::TaskState::Failed {
            error: norte_proto::Error::Conflict { conflict, .. },
        } => Some(*conflict),
        _ => None,
    }
}

/// Directorios afectados por una `PendingOp` YA terminada, para el
/// read-after-write de `on_task_terminal` (#85, extraída del brazo que vivía
/// inline ahí): `Copy` solo el destino (el origen queda intacto — no pisar
/// su cursor/marcas relistándolo sin necesidad); `Move` ambos (desaparece
/// del origen, aparece en el destino); `Delete` el padre del path borrado.
/// Filtra los `None` (una raíz sin padre no relista nada). PURA (sin GPUI):
/// testeable sin levantar ventana.
#[must_use]
fn affected_dirs(op: &PendingOp) -> Vec<VPath> {
    match op {
        PendingOp::Transfer {
            kind: TransferKind::Copy,
            to,
            ..
        } => to.parent().into_iter().collect(),
        PendingOp::Transfer { from, to, .. } => {
            [from.parent(), to.parent()].into_iter().flatten().collect()
        }
        PendingOp::Delete { path, .. } => path.parent().into_iter().collect(),
    }
}

/// La primera task, en orden de llegada de `order`, que NO está en estado
/// terminal (#85). FALLBACK de `task.cancel`/F9 cuando el cursor de franja
/// apunta a una terminal o la franja está vacía (#91, ver
/// `cancel_task_under_cursor`). PURA (sin GPUI): testeable sin levantar ventana.
#[must_use]
fn first_cancelable(
    order: &[norte_proto::TaskId],
    progress: &std::collections::HashMap<norte_proto::TaskId, norte_proto::TaskProgress>,
) -> Option<norte_proto::TaskId> {
    order
        .iter()
        .copied()
        .find(|id| progress.get(id).is_some_and(|p| !p.state.is_terminal()))
}

/// La task en el índice `cursor` de `order`, o `None` si `order` está vacío o
/// el índice cae fuera (#91, para `task.cancel` bajo cursor). Defensiva: no
/// asume que `cursor` sea válido (aunque `clamp_task_cursor` lo mantenga así),
/// solo indexa con `get`. PURA (sin GPUI): testeable sin levantar ventana.
#[must_use]
fn task_at_cursor(order: &[norte_proto::TaskId], cursor: usize) -> Option<norte_proto::TaskId> {
    order.get(cursor).copied()
}

/// Texto del indicador de secuencia multi-tecla pendiente (#91): cada chord
/// pendiente por su `Display`, seguido de un espacio (`"g g "`), o vacío si no
/// hay secuencia en curso. PURA (sin GPUI): testeable sin levantar ventana.
#[must_use]
fn pending_hint(chords: &[norte_frontend::keymap::Chord]) -> String {
    chords.iter().map(|c| format!("{c} ")).collect()
}

/// Retiene solo las tasks NO terminales en `order` y `progress`, mutando
/// ambos in place (#83, `task.dismiss`): usada por `dismiss_terminal_tasks`.
/// PURA (sin GPUI): testeable sin levantar ventana.
fn retain_active(
    order: &mut Vec<norte_proto::TaskId>,
    progress: &mut std::collections::HashMap<norte_proto::TaskId, norte_proto::TaskProgress>,
) {
    order.retain(|id| progress.get(id).is_some_and(|p| !p.state.is_terminal()));
    progress.retain(|_, p| !p.state.is_terminal());
}

/// ¿Sigue vigente el resultado de un `fs.list`? Solo si su generación coincide
/// con la vigente del pane: un cd más nuevo ya incrementó el contador, dejando
/// stale a cualquier list en vuelo anterior. Comparar la generación (y no el
/// `dir`) es robusto ante A→B→A — dos cds distintos al MISMO dir tienen
/// generaciones distintas, un dir-compare los confundiría.
#[must_use]
fn generation_is_current(current: u64, incoming: u64) -> bool {
    current == incoming
}

/// Componente vertical del delta de scroll (líneas o píxeles → f32).
fn scroll_y(delta: ScrollDelta) -> f32 {
    match delta {
        ScrollDelta::Pixels(p) => f32::from(p.y),
        ScrollDelta::Lines(p) => p.y,
    }
}

/// Etiqueta completa de una fila: badge hostil (si `display_name` marcó el
/// nombre) + nombre saneado + indicador de tipo. Extraída como función PURA
/// (sin GPUI) para poder testearla contra el corpus hostil de `norte-testkit`
/// sin levantar ventana/GPU (review encoding GAP).
#[must_use]
fn row_label(bytes: &[u8], kind: EntryKind) -> String {
    let (name, hostile) = norte_frontend::display_name(bytes);
    format!(
        "{}{}{}",
        if hostile {
            format!("{HOSTILE_BADGE} ")
        } else {
            String::new()
        },
        name,
        kind_indicator(kind),
    )
}

/// Indicador de tipo: «/» dir, «@» symlink, nada para archivo, «?» para lo demás.
fn kind_indicator(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Dir => "/",
        EntryKind::Symlink => "@",
        EntryKind::File => "",
        EntryKind::Other => "?",
    }
}

/// Línea de una task para la franja: `[copy] 42% running X`. `kind`/`state`
/// van por Fluent (GUI-e T1, `gui-task-kind-*`/`gui-task-state-*`); `X` = la
/// entrada en curso saneada con `display_name` (jamás bytes crudos). PURA
/// (sin GPUI).
#[must_use]
fn task_line(p: &norte_proto::TaskProgress) -> String {
    use norte_i18n::t;
    use norte_proto::{TaskKind, TaskState};
    let kind = t(match p.kind {
        TaskKind::Copy => "gui-task-kind-copy",
        TaskKind::Move => "gui-task-kind-move",
        TaskKind::Delete => "gui-task-kind-delete",
        TaskKind::Undo => "gui-task-kind-undo",
        TaskKind::Search => "gui-task-kind-search",
        TaskKind::Unknown => "gui-task-kind-unknown",
    });
    let pct = match p.entries_total {
        Some(total) if total > 0 => {
            format!("{}%", p.entries_done.saturating_mul(100) / total)
        }
        _ => "…".to_string(),
    };
    let state = t(match &p.state {
        TaskState::Pending => "gui-task-state-pending",
        TaskState::Running => "gui-task-state-running",
        TaskState::Paused => "gui-task-state-paused",
        TaskState::Completed => "gui-task-state-done",
        TaskState::Cancelled => "gui-task-state-cancelled",
        TaskState::Failed { .. } => "gui-task-state-failed",
        _ => "gui-task-state-unknown",
    });
    let current = p
        .current
        .as_ref()
        .and_then(|v| v.file_name().map(norte_proto::Segment::as_bytes))
        .map(|b| {
            let (name, hostile) = norte_frontend::display_name(b);
            if hostile {
                format!("{HOSTILE_BADGE} {name}")
            } else {
                name
            }
        })
        .unwrap_or_default();
    format!("[{kind}] {pct} {state} {current}")
        .trim_end()
        .to_string()
}

/// Cuántos items lista `modal_lines` antes de resumir el resto en "… y N más".
const MODAL_ITEM_LIMIT: usize = 10;

/// Hasta [`MODAL_ITEM_LIMIT`] nombres saneados (`display_name` por el nombre
/// de archivo, jamás bytes crudos); si sobran, una línea final localizada
/// (`gui-modal-more`, GUI-e T1). PURA (sin GPUI).
fn item_lines(items: &[VPath]) -> Vec<String> {
    let mut lines: Vec<String> = items
        .iter()
        .take(MODAL_ITEM_LIMIT)
        .map(|p| {
            let bytes = p.file_name().map_or(&b""[..], Segment::as_bytes);
            let (name, hostile) = norte_frontend::display_name(bytes);
            if hostile {
                format!("{HOSTILE_BADGE} {name}")
            } else {
                name
            }
        })
        .collect();
    if items.len() > MODAL_ITEM_LIMIT {
        let n = (items.len() - MODAL_ITEM_LIMIT).to_string();
        lines.push(norte_i18n::ta("gui-modal-more", &[("n", n.as_str())]));
    }
    lines
}

/// Líneas de texto del cuerpo del modal activo (título + detalle), YA
/// SANEADAS con `display_name`/`path_display` (jamás bytes crudos). Los
/// verbos/título/modo van por Fluent (GUI-e T1, `gui-modal-*`); el `conflict`
/// (`ConflictKind`) se interpola por su `Display` categórico (mismo criterio
/// que el `Error` de los banners: taxonomía cerrada, sin bytes de usuario —
/// ya auditado). PURA (sin GPUI): testeable contra el corpus hostil sin
/// levantar ventana. El pie de teclas es fijo (sin contenido de usuario) y lo
/// pinta `render_modal` directamente, no vive aquí.
#[must_use]
fn modal_lines(m: &Modal) -> Vec<String> {
    match m {
        Modal::ConfirmTransfer { kind, items, to } => {
            let title_key = match kind {
                TransferKind::Copy => "gui-modal-copy-title",
                TransferKind::Move => "gui-modal-move-title",
            };
            let (to_txt, to_hostile) = norte_frontend::path_display(to);
            let to_line = if to_hostile {
                format!("{HOSTILE_BADGE} {to_txt}")
            } else {
                to_txt
            };
            let n = items.len().to_string();
            let mut lines = vec![norte_i18n::ta(
                title_key,
                &[("n", n.as_str()), ("to", to_line.as_str())],
            )];
            lines.extend(item_lines(items));
            lines
        }
        Modal::ConfirmDelete { items, permanent } => {
            let mode = norte_i18n::t(if *permanent {
                "gui-modal-mode-permanent"
            } else {
                "gui-modal-mode-trash"
            });
            let n = items.len().to_string();
            let mut lines = vec![
                norte_i18n::ta("gui-modal-delete-title", &[("n", n.as_str())]),
                mode,
            ];
            lines.extend(item_lines(items));
            lines
        }
        Modal::ConflictResolve { pending, conflict } => {
            let from_bytes = pending.from.file_name().map_or(&b""[..], Segment::as_bytes);
            let (from_txt, from_hostile) = norte_frontend::display_name(from_bytes);
            let from_line = if from_hostile {
                format!("{HOSTILE_BADGE} {from_txt}")
            } else {
                from_txt
            };
            let (to_txt, to_hostile) = norte_frontend::path_display(&pending.to);
            let to_line = if to_hostile {
                format!("{HOSTILE_BADGE} {to_txt}")
            } else {
                to_txt
            };
            let conflict_txt = conflict.to_string();
            vec![
                norte_i18n::ta(
                    "gui-modal-conflict-title",
                    &[("conflict", conflict_txt.as_str())],
                ),
                format!("{from_line} → {to_line}"),
            ]
        }
    }
}

/// Aplica UN comando del contexto Viewer sobre `v` (la parte PURA de
/// `run_viewer_command`, sin GPUI — testeable sola). Devuelve `false` para
/// `"viewer.close"` (el caller debe soltar `self.viewer = None`; con `v`
/// prestado no se puede hacer aquí), `true` en cualquier otro caso (incluido
/// un comando desconocido, no-op).
fn apply_viewer_command(v: &mut norte_frontend::viewer::Viewer, cmd: &str) -> bool {
    use norte_frontend::viewer::PAGE as VPAGE;
    match cmd {
        "viewer.close" => return false,
        "viewer.up" => v.scroll_up(1),
        "viewer.down" => v.scroll_down(1),
        "viewer.page-up" => v.scroll_up(VPAGE),
        "viewer.page-down" => v.scroll_down(VPAGE),
        "viewer.top" => v.scroll_top(),
        "viewer.bottom" => v.scroll_bottom(),
        "viewer.encoding" => v.cycle_encoding(),
        "viewer.encoding-auto" => v.reset_encoding(),
        "viewer.hex" => v.toggle_hex(),
        _ => {} // comando desconocido en runtime: no-op (el keymap ya validó)
    }
    true
}

/// Barra de estado del visor (GUI-e T1: por Fluent, UNIFICADA con
/// `norte_tui::viewer::status` — mismas claves `viewer-*`/`eol-*` de
/// `norte-i18n`). Compone desde los getters del `Viewer` core (encoding/
/// binario, forzado, EOL, lossy, truncado) — el usuario SIEMPRE sabe qué ve
/// (spec §6). LF/CRLF/CR son literales técnicos (no i18n). Fn pura
/// (testeable sin GPUI); el render solo mapea su salida + `v.rows()`.
/// Cabecera/nombre accesible del visor: el path SANEADO (`path_display` + badge
/// hostil) + «via <plugin>» si es preview de plugin (el nombre del plugin YA
/// viene enmascarado por `with_plugin_preview`). Fn PURA para poder testear
/// contra el corpus hostil que la composición no reintroduce hazards crudos
/// (defensa en profundidad: es a la vez el título visual Y el `aria_label` a11y
/// — un lector de pantalla jamás debe leer bidi/controles crudos).
#[must_use]
fn viewer_header(v: &norte_frontend::viewer::Viewer) -> String {
    let (path_txt, path_hostile) = norte_frontend::path_display(&v.path);
    let mut header = if path_hostile {
        format!("{HOSTILE_BADGE} {path_txt}")
    } else {
        path_txt
    };
    if let Some(plugin) = v.preview_plugin() {
        // Reusa la clave de la TUI (`viewer-plugin-preview` = "via { $plugin }").
        header.push_str("  ");
        header.push_str(&norte_i18n::ta(
            "viewer-plugin-preview",
            &[("plugin", plugin)],
        ));
    }
    header
}

/// Presupuesto de píxeles del preview de imagen (~32 MP, cubre 8K de sobra): por
/// encima se rechaza (bomba de descompresión → OOM). Los bytes ya vienen
/// acotados por la sesión a `FS_READ_MAX_CHUNK`, pero un PNG diminuto puede
/// declarar dimensiones enormes; por eso se comprueban las dimensiones (solo la
/// cabecera) ANTES de decodificar. OJO al PICO transitorio: `into_rgba8()` puede
/// allocar una 2.ª copia mientras coexiste con el buffer del decoder, así que el
/// pico real ≈ 32 MP × 4 × 2 ≈ 256 MiB (una sola imagen cacheada a la vez).
const MAX_IMAGE_PIXELS: u64 = 32_000_000;

/// Imagen del viewer decodificada UNA vez al abrir (cache en [`NorteGui`]): en
/// modo imagen la GUI la pinta. `Unreadable` = no se pudo decodificar (truncada,
/// corrupta o excede [`MAX_IMAGE_PIXELS`]) → el render cae a un aviso i18n. Nunca
/// se decodifica en `render_viewer` (se haría en cada frame): se hace al abrir.
enum ImagePreview {
    /// Decodificada: frame BGRA listo para `img(Arc<RenderImage>)` + dimensiones.
    Ready {
        /// El frame decodificado (compartido con GPUI, que cachea la textura).
        image: std::sync::Arc<RenderImage>,
        /// Ancho en píxeles (para la barra de estado).
        width: u32,
        /// Alto en píxeles (para la barra de estado).
        height: u32,
    },
    /// El decode falló (truncada/corrupta/excede el presupuesto).
    Unreadable,
}

/// Decodifica los bytes de una imagen a un [`RenderImage`] de GPUI, con guardia
/// anti-bomba: primero lee SOLO las dimensiones (cabecera) y rechaza por encima
/// de [`MAX_IMAGE_PIXELS`] antes de asignar el buffer; luego decodifica a RGBA8 y
/// permuta a BGRA (el orden que espera `RenderImage`). Cualquier fallo (formato,
/// truncado, presupuesto) → [`ImagePreview::Unreadable`]; jamás panic ni OOM.
/// Para GIF/WebP animados se muestra el primer frame (preview estático).
fn decode_image_preview(bytes: &[u8]) -> ImagePreview {
    // 1) Dimensiones desde la cabecera, sin decodificar el cuerpo.
    let dims = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok());
    let Some((width, height)) = dims else {
        return ImagePreview::Unreadable;
    };
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return ImagePreview::Unreadable;
    }
    // 2) Decode completo con Limits (defensa en profundidad: acota las
    //    allocaciones INTERNAS del codec, no solo las dimensiones declaradas) →
    //    RGBA8 → BGRA in-place.
    let Ok(mut reader) = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()
    else {
        return ImagePreview::Unreadable;
    };
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_IMAGE_PIXELS.saturating_mul(4));
    reader.limits(limits);
    let Ok(decoded) = reader.decode() else {
        return ImagePreview::Unreadable;
    };
    let mut rgba = decoded.into_rgba8();
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let frame = image::Frame::new(rgba);
    let image = std::sync::Arc::new(RenderImage::new(vec![frame]));
    ImagePreview::Ready {
        image,
        width,
        height,
    }
}

/// Barra de estado en modo imagen: formato + dimensiones (`PNG  1920×1080`), o
/// el formato + «imagen ilegible» si el decode falló; añade el marcador de
/// truncado si la lectura se quedó en la cabecera. Formato/dimensiones son
/// literales técnicos (no i18n), como LF/CRLF en `viewer_status`.
#[must_use]
fn image_status(v: &norte_frontend::viewer::Viewer, preview: Option<&ImagePreview>) -> String {
    let fmt = v
        .image_kind()
        .map_or("", norte_frontend::viewer::ImageFmt::label);
    let mut out = match preview {
        Some(ImagePreview::Ready { width, height, .. }) => format!("{fmt}  {width}×{height}"),
        _ => format!("{fmt}  {}", norte_i18n::t("gui-viewer-image-unreadable")),
    };
    if v.truncated {
        out.push_str("  ");
        out.push_str(&norte_i18n::t("viewer-truncated"));
    }
    out
}

#[must_use]
fn viewer_status(v: &norte_frontend::viewer::Viewer) -> String {
    use norte_encoding::Eol;
    use norte_i18n::t;
    let mut out = if v.encoding_name().is_empty() {
        t("viewer-binary")
    } else {
        v.encoding_name().to_owned()
    };
    if v.is_forced() {
        out.push(' ');
        out.push_str(&t("viewer-forced"));
    }
    if !v.hex {
        let eol = match v.eol() {
            Eol::Lf => "LF".to_owned(),
            Eol::CrLf => "CRLF".to_owned(),
            Eol::Cr => "CR".to_owned(),
            Eol::Mixed => t("eol-mixed"),
            Eol::None => t("eol-none"),
        };
        out.push_str(&format!("  {eol}"));
    }
    if v.had_errors() {
        out.push_str(&format!("  {}", t("viewer-lossy")));
    }
    if v.truncated {
        out.push_str(&format!("  {}", t("viewer-truncated")));
    }
    out
}

/// Mapea el tipo de nodo del protocolo al tipo de archivo del tema. `File`/
/// `Other` → `Regular` (proto no trae `st_mode`; el theming por extensión sigue
/// aplicando encima).
fn file_kind_of(kind: EntryKind) -> FileKind {
    match kind {
        EntryKind::Dir => FileKind::Dir,
        EntryKind::Symlink => FileKind::Symlink,
        EntryKind::File | EntryKind::Other => FileKind::Regular,
    }
}

/// Color de texto de una entrada: extensión > kind > rol `regular`, con blanco
/// de fallback. `Theme::file_style` ya resuelve la prioridad.
fn entry_color(theme: &Theme, entry: &Entry) -> gpui::Rgba {
    let name = entry.path.file_name().map_or(&b""[..], Segment::as_bytes);
    let style = theme.file_style(name, file_kind_of(entry.kind));
    let fg = style.fg.or_else(|| theme.style(Role::Regular).fg);
    fg.map_or_else(|| rgb(0xffffff), theme_map::to_gpui_rgba)
}

impl Render for NorteGui {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Instrumentación (medición del lag, gated por NORTE_GUI_DEBUG): tiempo
        // de CONSTRUCCIÓN del árbol de elementos (nuestro coste; el layout/paint
        // de GPUI ocurre después de devolver). Si escala con las entradas, el
        // culpable es el O(N)-por-frame (display_name/tema recomputados por fila
        // sin virtualizar). Ver issue #87.
        let _t0 = std::time::Instant::now();

        // Raíz: `Role::Application` (idioma del ejemplo `a11y.rs`, div "root").
        // `.aria_label("norte")` es el nombre del producto (proper noun, como
        // el título de una ventana) — no pasa por Fluent a propósito, igual
        // que el resto del chrome de norte no localiza su propio nombre.
        let mut root = div()
            .id("root")
            .role(gpui::Role::Application)
            .aria_label("norte")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key))
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(BG))
            .text_color(rgb(FG))
            .p(px(4.0))
            .gap(px(2.0));

        // Banner de keymap roto (GUI-c T3): una capa de usuario/proyecto con
        // TOML inválido, tecla mal formada, comando desconocido o secuencia
        // ambigua no tumba la GUI — corre con el preset orthodox puro
        // (`keymap::build_effective_preset_only`, fijado en `new`) y avisa
        // aquí una vez por sesión.
        if let Some(msg) = &self.keymap_error {
            root = root.child(
                div()
                    .px(px(4.0))
                    .py(px(2.0))
                    .bg(rgb(HEADER_BG))
                    .text_color(rgb(ERR_FG))
                    .truncate()
                    .child(SharedString::from(msg.clone())),
            );
        }

        // Visor (F3) a pantalla completa, el estado «abriendo…» mientras
        // llega, o el dual-pane: pantallas mutuamente excluyentes (ver
        // `on_key`, que enruta al visor primero cuando ya está abierto).
        if self.viewer.is_some() {
            root = root.child(self.render_viewer(window, cx));
        } else if self.viewer_loading {
            root = root.child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(SharedString::from(norte_i18n::t("gui-viewer-opening"))),
            );
        } else {
            let panes_row = div()
                .flex_1()
                .flex()
                .flex_row()
                .overflow_hidden()
                .gap(px(2.0))
                .child(self.render_pane(0, cx))
                .child(self.render_pane(1, cx));
            root = root.child(panes_row).child(self.render_task_strip());
        }

        // Indicador de secuencia multi-tecla en curso (#91): si el resolver
        // ACTIVO (el del visor cuando está abierto, si no el de Browse — mismo
        // criterio de ruteo que `on_key`) tiene una secuencia pendiente
        // (`pending()` no vacío), pinta al pie los chords tecleados +«…». El
        // preset orthodox no trae secuencias, así que esto se ejerce con un
        // `keymap.toml` de usuario que ligue una (p. ej. `g g`).
        let active_resolver = if self.viewer.is_some() {
            &self.viewer_resolver
        } else {
            &self.resolver
        };
        let pending = active_resolver.pending();
        if !pending.is_empty() {
            root = root.child(
                div()
                    .px(px(4.0))
                    .py(px(1.0))
                    .bg(rgb(HEADER_BG))
                    .text_color(rgb(QUICK_FG))
                    .child(SharedString::from(format!("{}…", pending_hint(pending)))),
            );
        }

        // Overlay del modal activo: sin esto F5/F6/F8 capturaban teclado pero
        // no pintaban nada (el borrado se confirmaba a ciegas — CRITICAL).
        // Scrim oscuro sobre TODO el root (`.absolute().inset_0()`, contenedor
        // de posicionamiento por defecto en GPUI: `Position::Relative`) con el
        // panel centrado encima.
        if let Some(m) = &self.modal {
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgba(0x000000aa))
                    .child(self.render_modal(m)),
            );
        }

        if std::env::var_os("NORTE_GUI_DEBUG").is_some() {
            eprintln!(
                "[norte-gui] render construido en {}µs (panes: {}+{} entradas, modal={})",
                _t0.elapsed().as_micros(),
                self.panes[0].entries().len(),
                self.panes[1].entries().len(),
                self.modal.is_some(),
            );
        }

        root
    }
}

fn main() {
    // Idioma (GUI-e T1): mismo patrón que la TUI (`crates/norte-tui/src/
    // main.rs`), sin flag CLI — solo entorno (`NORTE_LANG` > `LC_ALL` >
    // `LC_MESSAGES` > `LANG`). Debe ir ANTES de construir la ventana: los
    // banners de arranque (`keymap_error`, config inválida) ya salen
    // localizados desde `NorteGui::new`.
    let _ = norte_i18n::force(norte_i18n::Lang::from_env());
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1000.0), px(640.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| NorteGui::new(window, cx)),
        )
        .expect("no se pudo abrir la ventana GPUI");

        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::{
        ImagePreview, affected_dirs, apply_viewer_command, decode_image_preview, first_cancelable,
        generation_is_current, image_status, pending_hint, retain_active, row_label,
        task_at_cursor, viewer_header, viewer_status,
    };
    use norte_frontend::viewer::Viewer;
    use norte_proto::{EntryKind, VPath};

    fn vp() -> VPath {
        VPath::parse("mem:///a.txt").unwrap()
    }

    /// El guard de generación: dos cds sobre el mismo pane (A luego B) →
    /// B incrementa la generación vigente; cuando A (viejo) llega tarde, su
    /// generación ya no coincide y su resultado se descarta, aunque A→B→A
    /// vuelva al mismo dir.
    #[test]
    fn resultado_stale_se_descarta_por_generacion() {
        // gen vigente del pane tras dos cds: 2. El outcome del primer cd (gen 1)
        // es stale.
        let vigente = 2u64;
        assert!(
            !generation_is_current(vigente, 1),
            "un resultado de gen 1 con el pane en gen 2 debe descartarse"
        );
        // El outcome del cd vigente (gen 2) sí se aplica.
        assert!(
            generation_is_current(vigente, 2),
            "el resultado de la generación vigente se aplica"
        );
        // A→B→A: el segundo A es gen 3, no la 1 del primero; el primer A (gen 1)
        // sigue stale aunque el dir coincida.
        let tras_a_b_a = 3u64;
        assert!(!generation_is_current(tras_a_b_a, 1));
        assert!(generation_is_current(tras_a_b_a, 3));
    }

    /// `row_label` sobre TODO el corpus hostil de `norte-testkit`: la etiqueta
    /// pintada jamás lleva un carácter de `is_terminal_hazard` crudo (bidi,
    /// control, invisible) — el saneado de `display_name` cubre la fila
    /// entera, no solo el nombre suelto (review encoding GAP, sin GPU).
    #[test]
    fn row_label_nunca_deja_hazards_crudos_del_corpus_hostil() {
        for fixture in norte_testkit::corpus::hostile_names() {
            let label = row_label(&fixture.bytes, EntryKind::File);
            assert!(
                !label.chars().any(norte_encoding::is_terminal_hazard),
                "{}: row_label dejó un hazard crudo en {label:?}",
                fixture.id,
            );
        }
    }

    /// El badge hostil (`⚠`) aparece cuando `display_name` marca el nombre
    /// como alterado — no se pierde la señal de "esto no es exactamente el
    /// byte original" al extraer `row_label`.
    #[test]
    fn row_label_badge_cuando_display_name_es_hostil() {
        for fixture in norte_testkit::corpus::hostile_names() {
            let (_, hostile) = norte_frontend::display_name(&fixture.bytes);
            let label = row_label(&fixture.bytes, EntryKind::File);
            assert_eq!(
                label.starts_with(super::HOSTILE_BADGE),
                hostile,
                "{}: badge debe aparecer sii display_name es hostil (label={label:?})",
                fixture.id,
            );
        }
    }

    /// `conflict_kind_of` extrae el subtipo de un `Failed{Conflict}`, y `None`
    /// para cualquier otro estado (incluidos otros `Failed` sin conflicto).
    #[test]
    fn conflict_kind_of_extrae_el_subtipo() {
        use norte_proto::{ConflictKind, Error, TaskState};
        let s = TaskState::Failed {
            error: Error::Conflict {
                conflict: ConflictKind::Exists,
            },
        };
        assert_eq!(super::conflict_kind_of(&s), Some(ConflictKind::Exists));
        assert_eq!(super::conflict_kind_of(&TaskState::Completed), None);
        assert_eq!(super::conflict_kind_of(&TaskState::Cancelled), None);
    }

    /// `task_line` sobre TODO el corpus hostil de `norte-testkit`: la línea
    /// pintada en la franja de tasks jamás lleva un carácter de
    /// `is_terminal_hazard` crudo, ni siquiera cuando la entrada en curso trae
    /// bytes hostiles (review encoding).
    #[test]
    fn task_line_nunca_deja_hazards_crudos_del_corpus_hostil() {
        use norte_proto::{Segment, TaskId, TaskKind, TaskProgress, TaskState, VPath};
        for fixture in norte_testkit::corpus::hostile_names() {
            let seg = match Segment::new(fixture.bytes.clone()) {
                Ok(s) => s,
                Err(_) => continue, // bytes no válidos como segmento (/, NUL, ., ..)
            };
            let current = VPath::parse("mem:///").unwrap().join(seg);
            let p = TaskProgress {
                task_id: TaskId::new(1),
                kind: TaskKind::Copy,
                state: TaskState::Running,
                bytes_done: 0,
                bytes_total: None,
                entries_done: 1,
                entries_total: Some(2),
                current: Some(current),
            };
            let line = super::task_line(&p);
            assert!(
                !line.chars().any(norte_encoding::is_terminal_hazard),
                "{}: task_line dejó un hazard crudo en {line:?}",
                fixture.id,
            );
        }
    }

    /// `modal_lines` sobre TODO el corpus hostil, para las TRES variantes de
    /// `Modal`: ninguna línea del panel deja un `is_terminal_hazard` crudo
    /// (título, item saneado, o el `from → to` de un conflicto) — mismo patrón
    /// que `task_line_nunca_deja_hazards_crudos_del_corpus_hostil` (review
    /// encoding: el modal es la superficie que confirma un BORRADO a ciegas si
    /// se pinta mal).
    #[test]
    fn modal_lines_nunca_deja_hazards_crudos_del_corpus_hostil() {
        use super::{Modal, PendingTransfer, TransferKind};
        use norte_proto::{ConflictKind, Segment, VPath};
        for fixture in norte_testkit::corpus::hostile_names() {
            let seg = match Segment::new(fixture.bytes.clone()) {
                Ok(s) => s,
                Err(_) => continue, // bytes no válidos como segmento (/, NUL, ., ..)
            };
            let item = VPath::parse("mem:///").unwrap().join(seg);
            let to = VPath::parse("mem:///dst").unwrap();

            let modals = [
                Modal::ConfirmTransfer {
                    kind: TransferKind::Copy,
                    items: vec![item.clone()],
                    to: to.clone(),
                },
                Modal::ConfirmDelete {
                    items: vec![item.clone()],
                    permanent: true,
                },
                Modal::ConflictResolve {
                    pending: PendingTransfer {
                        kind: TransferKind::Copy,
                        from: item.clone(),
                        to: to.clone(),
                    },
                    conflict: ConflictKind::Exists,
                },
            ];
            for m in &modals {
                for line in super::modal_lines(m) {
                    assert!(
                        !line.chars().any(norte_encoding::is_terminal_hazard),
                        "{}: modal_lines dejó un hazard crudo en {line:?}",
                        fixture.id,
                    );
                }
            }
        }
    }

    /// GUI-e T1 (i18n): con el locale forzado a ES, el modal de borrado
    /// PERMANENTE sale con el título/modo localizados (`Borrar N
    /// elemento(s)`/`PERMANENTE`, claves `gui-modal-delete-title`/
    /// `gui-modal-mode-permanent`) Y, sobre TODO el corpus hostil de
    /// `norte-testkit`, la línea del item saneado sigue sin un
    /// `is_terminal_hazard` crudo — el paso por Fluent no reintroduce bytes
    /// crudos (mismo criterio que `modal_lines_nunca_deja_hazards_crudos_del_
    /// corpus_hostil`, ahora con i18n de por medio).
    #[test]
    fn modal_i18n_es_localiza_y_no_deja_hazards_con_nombre_hostil() {
        let _ = norte_i18n::force(norte_i18n::Lang::Es);
        use super::Modal;
        use norte_proto::Segment;
        for fixture in norte_testkit::corpus::hostile_names() {
            let seg = match Segment::new(fixture.bytes.clone()) {
                Ok(s) => s,
                Err(_) => continue, // bytes no válidos como segmento (/, NUL, ., ..)
            };
            let item = VPath::parse("mem:///").unwrap().join(seg);
            let m = Modal::ConfirmDelete {
                items: vec![item],
                permanent: true,
            };
            let lines = super::modal_lines(&m);
            assert!(
                lines[0].contains("Borrar") && lines[0].contains('1'),
                "{}: título no localizado en ES: {:?}",
                fixture.id,
                lines[0],
            );
            assert_eq!(
                lines[1], "PERMANENTE",
                "{}: modo no localizado en ES: {:?}",
                fixture.id, lines[1],
            );
            for line in &lines {
                assert!(
                    !line.chars().any(norte_encoding::is_terminal_hazard),
                    "{}: modal_lines(i18n ES) dejó un hazard crudo en {line:?}",
                    fixture.id,
                );
            }
        }
    }

    /// `viewer_status` (GUI-d T3, i18n GUI-e T1) sobre un archivo de texto:
    /// encoding detectado + EOL, sin marcas de forzado/errores/truncado
    /// (todas en su cero). Locale fijado a ES (determinismo — el proceso de
    /// nextest es fresco por test, `norte_i18n::force` es de una sola vez).
    #[test]
    fn viewer_status_texto_limpio() {
        let _ = norte_i18n::force(norte_i18n::Lang::Es);
        let v = Viewer::new(vp(), b"hola\n".to_vec(), false);
        let s = viewer_status(&v);
        assert!(s.contains("UTF-8"), "{s}");
        assert!(s.contains("LF"), "{s}");
        assert!(!s.contains("forzado"), "{s}");
        assert!(!s.contains("truncado"), "{s}");
        assert!(!s.contains("pérdidas"), "{s}");
    }

    /// `viewer_status` sobre un binario NO-imagen: cae a `t("viewer-binary")`
    /// (sin nombre de encoding ni EOL, que no aplica en hexview). Locale ES.
    #[test]
    fn viewer_status_binario() {
        let _ = norte_i18n::force(norte_i18n::Lang::Es);
        let bin = b"\x00\x01\x02\x03payload\x00".to_vec();
        let v = Viewer::new(vp(), bin, false);
        assert!(v.hex && !v.is_image(), "binario no-imagen = hexview");
        let s = viewer_status(&v);
        assert_eq!(s, "binario", "sin EOL en hexview: {s}");
    }

    /// `image_status` compone formato + dimensiones cuando el decode va bien,
    /// y formato + «imagen ilegible» cuando falla; con marcador de truncado si
    /// aplica. Usa un PNG 1×1 real generado por el crate `image`. Locale ES.
    #[test]
    fn image_status_formato_y_dimensiones() {
        let _ = norte_i18n::force(norte_i18n::Lang::Es);
        // PNG 1×1 real (evita fixtures binarias en el árbol).
        let mut buf = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 255]))
            .write_to(&mut buf, image::ImageFormat::Png)
            .expect("encode PNG de test");
        let png = buf.into_inner();
        let v = Viewer::new(vp(), png.clone(), false);
        assert!(v.is_image());
        let preview = decode_image_preview(&png);
        assert!(
            matches!(
                preview,
                ImagePreview::Ready {
                    width: 1,
                    height: 1,
                    ..
                }
            ),
            "decode 1×1"
        );
        assert_eq!(image_status(&v, Some(&preview)), "PNG  1×1");
        // Bytes truncados (solo la cabecera mágica): reconocido como imagen,
        // pero el decode falla → «imagen ilegible», sin panic.
        let head = b"\x89PNG\r\n\x1a\n\x00\x00".to_vec();
        let vt = Viewer::new(vp(), head.clone(), true);
        assert!(vt.is_image());
        let bad = decode_image_preview(&head);
        assert!(matches!(bad, ImagePreview::Unreadable));
        let s = image_status(&vt, Some(&bad));
        assert!(s.starts_with("PNG  imagen ilegible"), "{s}");
        assert!(s.contains("cabecera"), "marcador de truncado: {s}");
    }

    /// Guardia anti-bomba: un PNG que declara dimensiones enormes (> presupuesto
    /// de píxeles) se rechaza en la cabecera, antes de asignar el buffer.
    #[test]
    fn decode_rechaza_por_presupuesto_de_pixeles() {
        // Cabecera PNG válida con IHDR declarando 60000×60000 (= 3.6 GP).
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0, 0, 0, 13]); // len chunk IHDR
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&60_000u32.to_be_bytes()); // width
        png.extend_from_slice(&60_000u32.to_be_bytes()); // height
        png.extend_from_slice(&[8, 6, 0, 0, 0]); // bit depth/color/…
        assert!(matches!(
            decode_image_preview(&png),
            ImagePreview::Unreadable
        ));
    }

    /// `viewer_status` marca "[cabecera]" (clave `viewer-truncated`,
    /// UNIFICADA con la TUI: la palabra cambia respecto al viejo literal
    /// "truncado" de la GUI, GUI-e T1) cuando el viewer se abrió con el tope
    /// de lectura alcanzado — el usuario SIEMPRE sabe que puede haber más
    /// archivo (spec §6). Locale ES.
    #[test]
    fn viewer_status_truncado() {
        let _ = norte_i18n::force(norte_i18n::Lang::Es);
        let v = Viewer::new(vp(), b"hola\n".to_vec(), true);
        assert!(viewer_status(&v).contains("cabecera"));
    }

    /// `viewer_status` marca "(forzado)" (clave `viewer-forced`) tras
    /// `cycle_encoding` («recargar como…»), y lo pierde tras
    /// `reset_encoding`. Locale ES.
    #[test]
    fn viewer_status_forzado_round_trip() {
        let _ = norte_i18n::force(norte_i18n::Lang::Es);
        let mut v = Viewer::new(vp(), b"hola\n".to_vec(), false);
        v.cycle_encoding();
        assert!(viewer_status(&v).contains("forzado"));
        v.reset_encoding();
        assert!(!viewer_status(&v).contains("forzado"));
    }

    /// `viewer_status` NUNCA deja un `is_terminal_hazard` crudo: el nombre del
    /// encoding y el texto EOL son literales fijos (sin bytes de usuario), así
    /// que basta un caso — no hace falta el corpus hostil completo (a
    /// diferencia de `row_label`/`task_line`, que sí interpolan nombres de
    /// archivo). Locale ES fijado: el catálogo es texto humano estático en
    /// ambos locales (paridad la verifica `norte-i18n`), sin bytes de usuario
    /// en ningún caso.
    #[test]
    fn viewer_status_sin_hazards_crudos() {
        let _ = norte_i18n::force(norte_i18n::Lang::Es);
        for (bytes, truncated) in [
            (b"hola\n".to_vec(), false),
            (b"\x89PNG\r\n\x1a\n".to_vec(), true),
        ] {
            let v = Viewer::new(vp(), bytes, truncated);
            let s = viewer_status(&v);
            assert!(
                !s.chars().any(norte_encoding::is_terminal_hazard),
                "viewer_status dejó un hazard crudo en {s:?}"
            );
        }
    }

    /// `viewer_header` (título + `aria_label` a11y): ni una ruta hostil ni un
    /// nombre de plugin hostil dejan un hazard crudo en la cabecera compuesta —
    /// un lector de pantalla jamás debe leer bidi/controles crudos (encoding H1).
    #[test]
    fn viewer_header_sin_hazards_con_ruta_y_plugin_hostiles() {
        use norte_proto::{Segment, VPath};
        let _ = norte_i18n::force(norte_i18n::Lang::Es);
        for fixture in norte_testkit::corpus::hostile_names() {
            let seg = match Segment::new(fixture.bytes.clone()) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let path = VPath::parse("mem:///").unwrap().join(seg);
            let raw = Viewer::new(path, b"x".to_vec(), false);
            assert!(
                !viewer_header(&raw)
                    .chars()
                    .any(norte_encoding::is_terminal_hazard),
                "{}: header con ruta hostil dejó un hazard",
                fixture.id
            );
        }
        // Nombre de plugin hostil (UTF-8 válido con bidi RLO + invisible ZWSP).
        let prev = Viewer::with_plugin_preview(
            VPath::parse("mem:///a").unwrap(),
            "plug\u{202E}in\u{200B}".to_string(),
            "salida",
        );
        assert!(
            !viewer_header(&prev)
                .chars()
                .any(norte_encoding::is_terminal_hazard),
            "header con plugin hostil dejó un hazard"
        );
    }

    /// `apply_viewer_command`: scroll (down/up), hex toggle y close, sobre un
    /// `Viewer` de texto multilinea. `"viewer.close"` devuelve `false` (el
    /// caller suelta `self.viewer`) sin tocar el estado del `Viewer`; el
    /// resto devuelve `true` y muta como el método correspondiente de
    /// `Viewer`.
    #[test]
    fn apply_viewer_command_scroll_hex_y_close() {
        use std::fmt::Write;
        let mut texto = String::new();
        for i in 0..50 {
            let _ = writeln!(texto, "linea {i}");
        }
        let mut v = Viewer::new(vp(), texto.into_bytes(), false);
        assert_eq!(v.scroll, 0);

        assert!(apply_viewer_command(&mut v, "viewer.down"));
        assert_eq!(v.scroll, 1, "viewer.down avanza una fila");

        assert!(apply_viewer_command(&mut v, "viewer.page-down"));
        assert_eq!(
            v.scroll,
            1 + norte_frontend::viewer::PAGE,
            "viewer.page-down avanza PAGE filas"
        );

        assert!(apply_viewer_command(&mut v, "viewer.up"));
        assert_eq!(
            v.scroll,
            norte_frontend::viewer::PAGE,
            "viewer.up retrocede 1"
        );

        assert!(apply_viewer_command(&mut v, "viewer.top"));
        assert_eq!(v.scroll, 0, "viewer.top vuelve al principio");

        assert!(apply_viewer_command(&mut v, "viewer.bottom"));
        assert_eq!(v.scroll, v.total_rows() - 1, "viewer.bottom va al final");

        assert!(!v.hex, "texto: no arranca en hexview");
        assert!(apply_viewer_command(&mut v, "viewer.hex"));
        assert!(v.hex, "viewer.hex activa el hexview");
        assert!(apply_viewer_command(&mut v, "viewer.hex"));
        assert!(!v.hex, "viewer.hex es un toggle");

        assert!(
            !apply_viewer_command(&mut v, "viewer.close"),
            "viewer.close devuelve false: el caller suelta self.viewer"
        );

        // Comando desconocido: no-op, sigue devolviendo true (el keymap ya
        // validó el nombre; run_command tiene el mismo contrato para Browse).
        let scroll_antes = v.scroll;
        assert!(apply_viewer_command(&mut v, "comando.inventado"));
        assert_eq!(v.scroll, scroll_antes);
    }

    /// `affected_dirs` (#85): un `Copy` solo relista el DESTINO — el origen
    /// queda intacto, no hace falta pisar su cursor/marcas.
    #[test]
    fn affected_dirs_copy_solo_destino() {
        use super::{PendingOp, TransferKind};
        use norte_core::TransferOptions;
        let op = PendingOp::Transfer {
            kind: TransferKind::Copy,
            from: VPath::parse("mem:///a/x.txt").unwrap(),
            to: VPath::parse("mem:///b/x.txt").unwrap(),
            opts: TransferOptions::default(),
        };
        assert_eq!(affected_dirs(&op), vec![VPath::parse("mem:///b").unwrap()]);
    }

    /// `affected_dirs`: un `Move` relista AMBOS — desaparece del origen,
    /// aparece en el destino.
    #[test]
    fn affected_dirs_move_ambos() {
        use super::{PendingOp, TransferKind};
        use norte_core::TransferOptions;
        let op = PendingOp::Transfer {
            kind: TransferKind::Move,
            from: VPath::parse("mem:///a/x.txt").unwrap(),
            to: VPath::parse("mem:///b/x.txt").unwrap(),
            opts: TransferOptions::default(),
        };
        assert_eq!(
            affected_dirs(&op),
            vec![
                VPath::parse("mem:///a").unwrap(),
                VPath::parse("mem:///b").unwrap(),
            ]
        );
    }

    /// `affected_dirs`: un `Delete` relista el PADRE del path borrado.
    #[test]
    fn affected_dirs_delete_padre() {
        use super::PendingOp;
        use norte_proto::DeleteMode;
        let op = PendingOp::Delete {
            path: VPath::parse("mem:///a/x.txt").unwrap(),
            mode: DeleteMode::Trash,
        };
        assert_eq!(affected_dirs(&op), vec![VPath::parse("mem:///a").unwrap()]);
    }

    /// Construye un `TaskProgress` mínimo con `id`/`state` dados (helper de
    /// los tests de `first_cancelable`/`retain_active`, #85/#83).
    fn task_progress_with(id: u64, state: norte_proto::TaskState) -> norte_proto::TaskProgress {
        norte_proto::TaskProgress {
            task_id: norte_proto::TaskId::new(id),
            kind: norte_proto::TaskKind::Copy,
            state,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
        }
    }

    /// `first_cancelable` (#85): salta las terminales en orden de llegada y
    /// toma la primera task activa.
    #[test]
    fn first_cancelable_salta_terminales_y_toma_la_primera_activa() {
        use norte_proto::{TaskId, TaskState};
        let mut progress = std::collections::HashMap::new();
        progress.insert(TaskId::new(1), task_progress_with(1, TaskState::Completed));
        progress.insert(TaskId::new(2), task_progress_with(2, TaskState::Running));
        progress.insert(TaskId::new(3), task_progress_with(3, TaskState::Running));
        let order = vec![TaskId::new(1), TaskId::new(2), TaskId::new(3)];
        assert_eq!(
            first_cancelable(&order, &progress),
            Some(TaskId::new(2)),
            "salta la 1 (terminal) y toma la 2 (primera activa en orden)"
        );
    }

    /// `first_cancelable`: `None` si todas las tasks de `order` están en
    /// estado terminal (nada que cancelar).
    #[test]
    fn first_cancelable_none_si_todas_terminales() {
        use norte_proto::{TaskId, TaskState};
        let mut progress = std::collections::HashMap::new();
        progress.insert(TaskId::new(1), task_progress_with(1, TaskState::Completed));
        progress.insert(TaskId::new(2), task_progress_with(2, TaskState::Cancelled));
        let order = vec![TaskId::new(1), TaskId::new(2)];
        assert_eq!(first_cancelable(&order, &progress), None);
    }

    /// `task_at_cursor` (#91): devuelve la task en el índice dado, y `None`
    /// para un índice fuera de rango o una franja vacía (defensivo).
    #[test]
    fn task_at_cursor_indexa_o_none_fuera_de_rango() {
        use norte_proto::TaskId;
        let order = vec![TaskId::new(10), TaskId::new(20), TaskId::new(30)];
        assert_eq!(task_at_cursor(&order, 0), Some(TaskId::new(10)));
        assert_eq!(task_at_cursor(&order, 2), Some(TaskId::new(30)));
        assert_eq!(task_at_cursor(&order, 3), None, "fuera de rango → None");
        assert_eq!(task_at_cursor(&[], 0), None, "franja vacía → None");
    }

    /// `pending_hint` (#91): los chords pendientes por su `Display` con espacio
    /// final (`"g g "`); vacío → cadena vacía.
    #[test]
    fn pending_hint_formatea_los_chords_o_vacio() {
        use norte_frontend::keymap::{Chord, KeyCode, Mods};
        assert_eq!(pending_hint(&[]), "", "sin pendiente → vacío");
        let g = Chord::new(Mods::default(), KeyCode::Char('g'));
        assert_eq!(pending_hint(&[g]), "g ");
        assert_eq!(pending_hint(&[g, g]), "g g ");
        let ctrl_k = Chord::new(
            Mods {
                ctrl: true,
                ..Default::default()
            },
            KeyCode::Char('k'),
        );
        assert_eq!(pending_hint(&[ctrl_k, g]), "ctrl+k g ");
    }

    /// `retain_active` (#83, `task.dismiss`): quita de `order`/`progress`
    /// TODAS las tasks terminales (Completed/Cancelled/Failed), conserva las
    /// activas (Running) en ambos.
    #[test]
    fn retain_active_quita_terminales_conserva_activas() {
        use norte_proto::{TaskId, TaskState};
        let mut progress = std::collections::HashMap::new();
        progress.insert(TaskId::new(1), task_progress_with(1, TaskState::Completed));
        progress.insert(TaskId::new(2), task_progress_with(2, TaskState::Running));
        progress.insert(TaskId::new(3), task_progress_with(3, TaskState::Cancelled));
        let mut order = vec![TaskId::new(1), TaskId::new(2), TaskId::new(3)];

        retain_active(&mut order, &mut progress);

        assert_eq!(
            order,
            vec![TaskId::new(2)],
            "solo la task activa queda en order"
        );
        assert_eq!(progress.len(), 1, "solo la task activa queda en progress");
        assert!(progress.contains_key(&TaskId::new(2)));
    }
}
