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
    App, Bounds, BoxShadow, Context, FocusHandle, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, ParentElement, Pixels, Render, RenderImage, ScrollDelta, ScrollStrategy,
    ScrollWheelEvent, SharedString, Styled, UniformListScrollHandle, Window, WindowBounds,
    WindowOptions, canvas, div, fill, hsla, img, linear_color_stop, linear_gradient, point,
    prelude::*, px, rgb, rgba, size, uniform_list,
};
use gpui_platform::application;

use std::ops::Range;

use norte_frontend::{PaneState, nav::Mode};
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_theme::{FileKind, Role, Theme};

mod effects;
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

/// Filas de chrome que le restamos al alto del viewport para derivar cuántas
/// filas de contenido pedirle a `Viewer::rows` en `render_viewer`: cabecera +
/// barra de estado (una fila cada una) + margen de redondeo.
const VIEWER_CHROME_ROWS: usize = 3;

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
    ///
    /// Auditoría de encoding final (#73, INFO 4, no corregido a propósito):
    /// los sitios que rellenan este campo con un `Error` del protocolo
    /// (`SubmitFailed` ~445, `ViewerOpened`/`ViewerFailed` ~518) interpolan su
    /// `Display` SIN pasar por `banner_safe` — se apoyan en la reclamación de
    /// "taxonomía cerrada, sin bytes crudos" (auditada en GUI-b, ver el
    /// comentario junto a `gui-banner-op-rejected`). No se toca aquí: la
    /// auditoría solo señala que la garantía descansa en esa reclamación, no
    /// la re-verifica.
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
    /// Interpretación de `theme.effects` (ADR 0036, G1 Task 4), resuelta UNA
    /// vez junto a `theme` (mismo lugar, `new`) — nunca por frame. `None`
    /// cuando el tema no declara `[effects]`: cada rama de `render()` que
    /// pinta un efecto está detrás de un `if let Some`, así que un tema sin
    /// `[effects]` deja el árbol de render byte-idéntico al de antes de G1.
    effects: Option<effects::EffectsV1>,
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
    /// Tipografía resuelta UNA vez en `new` (GP), a partir de `[ui]`. Ver
    /// doc de [`FontSet`].
    fonts: FontSet,
}

impl NorteGui {
    /// Construye el view con los dos panes en el mismo directorio inicial
    /// (`NORTE_DIR` o el `cwd`) y lanza sus dos cargas. Toma el foco de la
    /// ventana para recibir teclado. Si la config no resuelve, nace con ambos
    /// panes en error (sin lanzar cargas), nunca panic.
    ///
    /// `loaded`: el resultado de la carga de config REAL (C2), ya hecha una
    /// vez en `main` (antes de abrir la ventana — ahí es donde también se
    /// negocia el idioma, que debe estar fijado ANTES de que este
    /// constructor arme los banners localizados). `Err` degrada a preset
    /// `orthodox`/tema por defecto y avisa por el MISMO banner que el error
    /// de keymap (unidos si ambos fallan) — jamás aborta el arranque.
    fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        loaded: &Result<norte_frontend::config::FrontendConfig, norte_config::ConfigError>,
    ) -> Self {
        let (preset_name, theme_spec, mut startup_banner): (
            String,
            Option<String>,
            Option<String>,
        ) = match loaded {
            Ok(cfg) => (cfg.common.preset.clone(), cfg.common.ui_theme.clone(), None),
            Err(e) => (
                norte_config::DEFAULT_PRESET.to_owned(),
                None,
                Some(config_error_banner(e)),
            ),
        };

        // Preset desconocido (revisión C2/G0 IMPORTANT 2): ver
        // `unknown_preset_banner` — debe ir ANTES de construir el keymap.
        if let Some(msg) = unknown_preset_banner(&preset_name) {
            startup_banner = Some(push_banner(startup_banner, msg));
        }

        let theme = match norte_frontend::theme::resolve_theme(theme_spec.as_deref()) {
            Ok(theme) => theme,
            Err(e) => {
                let msg = theme_error_banner(&e);
                startup_banner = Some(push_banner(startup_banner, msg));
                Theme::preset_default()
            }
        };
        // Resuelto UNA vez, junto al tema (ver doc del campo `effects`): el
        // único lugar donde este tema puede cambiar es aquí, en el arranque
        // (grep de `self.theme`/`theme:` en el resto del archivo — no hay
        // selector de tema en caliente en esta GUI).
        //
        // MINOR de review: antes las claves `[effects]` degradadas SOLO
        // salían por un `eprintln!` — invisible al lanzar desde un desktop
        // entry sin terminal adjunta. `from_theme` ahora devuelve también la
        // lista de warnings (clave + motivo corto, NUNCA el valor
        // malformado — ver rustdoc de `effects::record`); cada una entra al
        // MISMO acumulador de banner de arranque que ya usan los errores de
        // config/tema/keymap, vía la clave Fluent nueva
        // `gui-banner-effects-key-skipped`, con el texto saneado por
        // `banner_safe` (los nombres de clave desconocida SON contenido de
        // un `.toml` de usuario).
        let (effects, effects_warnings) = effects::EffectsV1::from_theme(&theme);
        for w in &effects_warnings {
            let msg = norte_i18n::ta(
                "gui-banner-effects-key-skipped",
                &[("key", banner_safe(w).as_str())],
            );
            startup_banner = Some(push_banner(startup_banner, msg));
        }
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        let ((browse_eff, viewer_eff), keymap_error) = match keymap::build_effectives(&preset_name)
        {
            Ok(pair) => (pair, startup_banner),
            Err(e) => {
                let msg = norte_i18n::ta(
                    "gui-banner-keymap-error",
                    &[("error", keymap_error_detail(&e).as_str())],
                );
                (
                    keymap::build_effectives_preset_only(&preset_name),
                    Some(push_banner(startup_banner, msg)),
                )
            }
        };
        let resolver = norte_frontend::keymap::Resolver::new(browse_eff);
        let viewer_resolver = norte_frontend::keymap::Resolver::new(viewer_eff);

        // Tipografía (GP): resuelta UNA vez aquí, junto al resto de la
        // sesión — config inválida (rama `Err` de `loaded`) degrada a la
        // fuente/tamaño por defecto, igual que el resto del arranque
        // (nunca aborta por esto).
        let fonts = match loaded {
            Ok(cfg) => FontSet::resolve(
                cfg.common.ui_font.as_deref(),
                cfg.common.ui_mono_font.as_deref(),
                cfg.common.ui_font_size,
            ),
            Err(_) => FontSet::resolve(None, None, None),
        };

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
                    effects,
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
                    fonts,
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
                        // Mismo saneado que el resto de sitios del banner:
                        // aunque venga del entorno, jamás texto crudo.
                        let error = banner_safe(&e.to_string());
                        let msg = norte_i18n::ta(
                            "gui-banner-config-invalid",
                            &[("error", error.as_str())],
                        );
                        [Some(msg.clone()), Some(msg)]
                    },
                    generation: [0, 0],
                    relist_pending: [false, false],
                    theme,
                    effects,
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
                    fonts,
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
                    Ok((entries, skipped)) => {
                        // set_listing ya normaliza (ordena) internamente (#54);
                        // pre-ordenar aquí era un doble sort (#94).
                        self.panes[pane].set_listing(dir, entries);
                        // #96: badge de omitidas del contenedor (#93) — un
                        // listado incompleto jamás es silencioso, tampoco
                        // en la GUI.
                        self.panes[pane].set_skipped(skipped);
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
                image,
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
                // #92: la imagen llega YA decodificada del hilo de sesión —
                // aquí solo se envuelve en el tipo de render (O(1), sin jank).
                self.viewer_image = image.map(image_preview_from);
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

    /// `app.quit` (revisión C2/G0 IMPORTANT 3): con trabajo pendiente
    /// (tasks visibles en la franja o marcas activas), abre
    /// [`Modal::ConfirmQuit`] en vez de cerrar de inmediato — "y" en el
    /// modal manda `ModalOutcome::Quit`, que el dispatcher de `on_key` ya
    /// traduce a `cx.quit()`. Estado vacío: cierra YA (paridad con la TUI,
    /// que jamás confirma — `crates/norte-tui/src/main.rs` hace
    /// `app.quit = true` sin preguntar).
    fn quit_or_confirm(&mut self, cx: &mut Context<Self>) {
        let marks = self.panes[0].marks_len() + self.panes[1].marks_len();
        let tasks = confirm_quit_task_count(self.task_progress.len(), marks, self.inflight.len());
        // `inflight` cubre la ventana entre submit y el primer evento de
        // task: una op recién lanzada aún sin progreso también debe frenar
        // el quit (solo el GATE; los contadores del modal siguen siendo los
        // visibles, ver `confirm_quit_task_count`).
        if has_pending_work(tasks, marks) || !self.inflight.is_empty() {
            self.modal = Some(Modal::ConfirmQuit { tasks, marks });
        } else {
            cx.quit();
        }
    }

    /// Ejecuta un comando del keymap (contexto Browse) sobre el estado.
    /// Reemplaza el `key_to_action` hardcodeado para las acciones nombradas
    /// (GUI-c T3): `on_key` resuelve la tecla vía `resolver` y llama aquí.
    fn run_command(&mut self, cmd: &str, cx: &mut Context<Self>) {
        let f = self.focus;
        match cmd {
            "app.quit" => self.quit_or_confirm(cx),
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
                ModalOutcome::Quit => cx.quit(),
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
    fn render_pane(
        &self,
        i: usize,
        chrome: &ChromeColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let pane = &self.panes[i];
        let focused = self.focus == i;
        // Copia barata (todo `Copy`) para moverla dentro del closure
        // `'static` de `cx.processor` — no puede capturar `&ChromeColors`
        // prestado de este frame, que no vive tanto como el closure.
        let chrome_owned = *chrome;

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
                        this.render_row(i, j, &e, hl, marked, &chrome_owned, cx)
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
            // Mono (GP): tamaños/columnas del listado necesitan ancho fijo
            // para alinear — cascada a las filas (`render_row`) vía
            // `TextStyleRefinement`.
            .font(self.fonts.mono.clone())
            .line_height(self.fonts.row_h)
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
                chrome.border_focus
            } else {
                chrome.border_unfocus
            })
            .bg(if focused {
                chrome.pane_bg_focus
            } else {
                chrome.pane_bg
            });

        // Cabecera: el path saneado del dir. Par honesto con el tema: fondo Y
        // texto de `StatusBar` (ver doc de `ChromeColors`), no solo el fondo.
        col = col.child(
            div()
                .px(px(4.0))
                .py(px(2.0))
                .bg(chrome.header_bg)
                .text_color(chrome.header_fg)
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
                    .text_color(chrome.err_fg)
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

        // #96: el contenedor omitió entradas de su índice (#93) — el listado
        // que se ve NO es todo lo que el archivo contiene. Mismo contrato que
        // el badge de la status bar del TUI (clave i18n compartida), jamás
        // silencioso; solo se pinta `Some(n)` con n > 0. Par honesto
        // quick-search (`Match`): fondo Y texto, no solo el texto — un texto
        // casi negro (tema `default`) sobre el fondo oscuro del pane sin su
        // propio fondo sería ilegible.
        if let Some(n) = pane.skipped().filter(|n| *n > 0) {
            col = col.child(
                div()
                    .px(px(4.0))
                    .bg(chrome.quick_bg)
                    .text_color(chrome.quick_fg)
                    .child(SharedString::from(norte_i18n::ta(
                        "status-archive-skipped",
                        &[("n", &n.to_string())],
                    ))),
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
                    .bg(chrome.quick_bg)
                    .text_color(chrome.quick_fg)
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
    // 8 parámetros: posición (pane/idx), datos de la entrada+estado
    // (entry/highlighted/marked), la paleta resuelta del frame (chrome) y el
    // contexto de GPUI (cx) — todos necesarios, ninguno agrupable sin una
    // indirección artificial (`RowState { entry, highlighted, marked }` solo
    // movería el problema a un tipo nuevo con un único call site).
    #[allow(clippy::too_many_arguments)]
    fn render_row(
        &self,
        pane: usize,
        idx: usize,
        entry: &Entry,
        highlighted: bool,
        marked: bool,
        chrome: &ChromeColors,
        cx: &mut Context<Self>,
        // ed. 2024: RPIT captura TODOS los lifetimes en scope; `render_row` NO
        // retiene préstamos (clona nombre/color, el listener es 'static), así
        // que acota la captura a vacío para no atrapar el `&entry` (que en el
        // processor de `uniform_list` es un clon local que escaparía) ni
        // `&chrome` (misma razón: solo se leen sus campos `Copy`, nunca se
        // guarda la referencia).
    ) -> impl IntoElement + use<> {
        let bytes = entry.path.file_name().map_or(&b""[..], Segment::as_bytes);
        let mut label = row_label(bytes, entry.kind);
        if marked {
            label = format!("{MARK_MARKER} {label}");
        }
        let color = entry_color(&self.theme, entry, self.effects.and_then(|e| e.glow));
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
            .h(self.fonts.row_h)
            .px(px(4.0))
            .py(px(1.0))
            .text_color(color)
            .truncate()
            .child(SharedString::from(label));
        if marked {
            row = row.bg(chrome.mark_bg);
        }
        if highlighted {
            row = row.bg(chrome.sel_bg);
            // Par honesto de selección: si el tema declara `Selection.fg`,
            // reemplaza el color por-tipo de la fila; si no, se conserva
            // (comportamiento histórico, ver doc de `ChromeColors`).
            if let Some(fg) = chrome.sel_fg {
                row = row.text_color(fg);
            }
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
    fn render_task_strip(&self, chrome: &ChromeColors) -> impl IntoElement {
        let mut strip = div()
            .id("task-strip")
            .role(gpui::Role::List)
            .aria_label(norte_i18n::t("gui-a11y-tasks"))
            // Mono (GP): la % y el estado de cada task se leen mejor
            // alineados en columna, igual que un listado.
            .font(self.fonts.mono.clone())
            .flex()
            .flex_col()
            .max_h(px(120.0))
            .overflow_hidden()
            .bg(chrome.header_bg)
            .text_color(chrome.header_fg)
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
                row = row.bg(chrome.sel_bg);
                // Sobrescribe el `header_fg` heredado del contenedor: la fila
                // seleccionada necesita SU propio contraste sobre `sel_bg`,
                // no el pensado para `header_bg` (ver doc de `ChromeColors`).
                if let Some(fg) = chrome.sel_fg {
                    row = row.text_color(fg);
                }
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
    fn render_viewer(
        &self,
        window: &Window,
        chrome: &ChromeColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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
        let viewport_rows = (window.viewport_size().height / self.fonts.row_h) as usize;
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
                            .h(self.fonts.row_h)
                            .px(px(4.0))
                            .truncate()
                            .child(SharedString::from(row))
                    }),
                )
            };
        // Mono (GP): columnas hex/texto necesitan ancho fijo para alinear.
        // Se fija también en modo imagen (sin efecto visible: no hay texto
        // que alinear ahí) para mantener `body` de un solo tipo concreto sin
        // un tercer branch — más simple que condicionar el font por modo.
        let body = body
            .font(self.fonts.mono.clone())
            .line_height(self.fonts.row_h);

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
            .border_color(chrome.border_focus)
            .bg(chrome.pane_bg_focus)
            .child(
                div()
                    .px(px(4.0))
                    .py(px(2.0))
                    .bg(chrome.header_bg)
                    .text_color(chrome.header_fg)
                    .truncate()
                    .child(SharedString::from(header)),
            )
            .child(body)
            .child(
                div()
                    .px(px(4.0))
                    .py(px(1.0))
                    .bg(chrome.quick_bg)
                    .text_color(chrome.quick_fg)
                    .font(self.fonts.mono.clone())
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
    fn render_modal(&self, m: &Modal, chrome: &ChromeColors) -> impl IntoElement {
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
            Modal::ConfirmQuit { .. } => "gui-modal-footer-quit",
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
            .border_color(chrome.border_focus)
            .px(px(12.0))
            .py(px(8.0))
            .gap(px(2.0));

        // Revisión final (contraste WCAG): `header_bg`+`fg` medía 1.1-1.9:1
        // en los 6 presets — `StatusBar` está pensado para texto en
        // `header_fg`, no en el `fg` genérico. `modal_panel_colors`/
        // `modal_title_colors`/`modal_footer_colors` (funciones puras, ver
        // sus docs) fijan las COMBINACIONES honestas; `modal_usa_pares_
        // honestos` las pinea.
        let (panel_bg, panel_fg) = modal_panel_colors(chrome);
        panel = panel.bg(panel_bg).text_color(panel_fg);

        let mut lines = lines.into_iter();
        if let Some(title) = lines.next() {
            let (title_bg, title_fg) = modal_title_colors(chrome);
            panel = panel.child(
                div()
                    .px(px(2.0))
                    .py(px(1.0))
                    .bg(title_bg)
                    .text_color(title_fg)
                    .truncate()
                    .child(SharedString::from(title)),
            );
        }
        for (i, line) in lines.enumerate() {
            let mut row = div().truncate().child(SharedString::from(line));
            if Some(i + 1) == alert_line {
                row = row.text_color(chrome.err_fg);
            }
            panel = panel.child(row);
        }

        let (footer_bg, footer_fg) = modal_footer_colors(chrome);
        let mut footer_row = div()
            .mt(px(4.0))
            .px(px(2.0))
            .py(px(1.0))
            .bg(footer_bg)
            .child(SharedString::from(footer));
        if let Some(fg) = footer_fg {
            footer_row = footer_row.text_color(fg);
        }
        panel.child(footer_row)
    }
}

/// Colores de la SUPERFICIE del panel del modal: `pane_bg_focus` + `fg` — el
/// mismo par que ya usan los panes (`render_pane`), porque `Regular.fg` está
/// diseñado para fondos de pane, no para `header_bg` (que trae su propio
/// `header_fg`, ver `modal_title_colors`). La línea de alerta (`err_fg`)
/// queda legible sobre este fondo sin ningún ajuste extra.
#[must_use]
fn modal_panel_colors(chrome: &ChromeColors) -> (gpui::Rgba, gpui::Rgba) {
    (chrome.pane_bg_focus, chrome.fg)
}

/// Colores de la franja de TÍTULO del modal: el par completo de `StatusBar`
/// (fondo Y texto), igual que cualquier otra franja de cabecera de la GUI.
#[must_use]
fn modal_title_colors(chrome: &ChromeColors) -> (gpui::Rgba, gpui::Rgba) {
    (chrome.header_bg, chrome.header_fg)
}

/// Colores del PIE del modal: fondo de selección + su texto emparejado
/// cuando el tema lo declara (mismo criterio que la fila seleccionada de
/// `render_row`); si el tema no declara `Selection.fg`, `None` deja el texto
/// heredado del panel (`modal_panel_colors`).
#[must_use]
fn modal_footer_colors(chrome: &ChromeColors) -> (gpui::Rgba, Option<gpui::Rgba>) {
    (chrome.sel_bg, chrome.sel_fg)
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

/// ¿Hay trabajo que se perdería de vista si la GUI cierra AHORA? (revisión
/// C2/G0 IMPORTANT 3): `tasks` = tasks visibles en la franja (las
/// `Completed` ya se autopodan al llegar — lo que queda son en vuelo o
/// terminales-sin-descartar; cerrar no las cancela, pero deja de poder verlas
/// ni cancelarlas desde esta ventana); `marks` = marcas activas en CUALQUIER
/// pane (solo de sesión — se pierden de verdad al cerrar). Puro: no decide
/// la UI, solo la condición.
#[must_use]
fn has_pending_work(tasks: usize, marks: usize) -> bool {
    tasks > 0 || marks > 0
}

/// El contador de tasks que muestra [`Modal::ConfirmQuit`] (revisión final
/// del review, MINOR 3): normalmente `task_progress_len` (lo que ya tiene un
/// evento de progreso). Pero el gate de `quit_or_confirm` también frena por
/// `inflight` no vacío — una op recién lanzada AÚN sin su primer evento — y
/// mostrar «0 task(s)» con trabajo real en vuelo es una mentira visible. Solo
/// se sustituye por `inflight_len` cuando `task_progress_len` Y `marks` son
/// CERO: en ese caso las ops de `inflight` no han producido progreso todavía,
/// así que no pueden solaparse con `task_progress_len` (que ya sería > 0 si
/// alguna lo hubiera hecho) — nunca se suman ambos números. Puro: no decide
/// la UI, solo el conteo.
#[must_use]
fn confirm_quit_task_count(task_progress_len: usize, marks: usize, inflight_len: usize) -> usize {
    if task_progress_len == 0 && marks == 0 && inflight_len > 0 {
        inflight_len
    } else {
        task_progress_len
    }
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
        Modal::ConfirmQuit { tasks, marks } => {
            // Contadores puros (`usize`), sin bytes de usuario — nada que
            // sanear aquí, a diferencia del resto de modales.
            vec![norte_i18n::ta(
                "gui-modal-quit-title",
                &[
                    ("tasks", tasks.to_string().as_str()),
                    ("marks", marks.to_string().as_str()),
                ],
            )]
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

/// Imagen del viewer decodificada UNA vez al abrir (en el HILO DE SESIÓN,
/// #92 — aquí solo se envuelve): en modo imagen la GUI la pinta. `Unreadable`
/// = el decode falló (truncada, corrupta o excede el presupuesto de
/// `session::decode_image`) → el render cae a un aviso i18n. Nunca se
/// decodifica en `render_viewer` (sería cada frame) NI en este hilo (jank).
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

/// Envuelve el frame BGRA ya decodificado por la sesión (#92) en el tipo de
/// render de GPUI. O(1) sobre los datos (mueve el buffer, sin re-decode); un
/// buffer inconsistente (len ≠ w×h×4, imposible salvo bug) cae a
/// `Unreadable`, jamás panic.
fn image_preview_from(d: session::ImageDecode) -> ImagePreview {
    match d {
        session::ImageDecode::Ready(di) => {
            let (width, height) = (di.width, di.height);
            let Some(buf) =
                image::ImageBuffer::<image::Rgba<u8>, Vec<u8>>::from_raw(width, height, di.bgra)
            else {
                return ImagePreview::Unreadable;
            };
            let image = std::sync::Arc::new(RenderImage::new(vec![image::Frame::new(buf)]));
            ImagePreview::Ready {
                image,
                width,
                height,
            }
        }
        session::ImageDecode::Unreadable => ImagePreview::Unreadable,
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
/// de fallback. `Theme::file_style` ya resuelve la prioridad. `glow`: el
/// post-proceso de brillo de G1 (ADR 0036 decisión 3) — `entry_color` es el
/// único sitio de color POR FILA, así que el glow entra aquí en vez de en
/// `ChromeColors` (que se resuelve una vez por frame, no por fila).
fn entry_color(theme: &Theme, entry: &Entry, glow: Option<effects::Glow>) -> gpui::Rgba {
    let name = entry.path.file_name().map_or(&b""[..], Segment::as_bytes);
    let style = theme.file_style(name, file_kind_of(entry.kind));
    let fg = style.fg.or_else(|| theme.style(Role::Regular).fg);
    let color = fg.map_or_else(|| rgb(0xffffff), theme_map::to_gpui_rgba);
    glowed(color, glow)
}

/// Aplica el brillo v1 de G1 (ADR 0036 decisión 3): `lerp(fg, white, strength
/// times 0.25)` por canal, canal `a` intacto (el glow no toca la opacidad).
/// `g = None` es un no-op explícito — así todo call-site puede pasar
/// `self.effects.and_then(|e| e.glow)` sin ramificar. Costo: 3
/// multiplicaciones por canal, aceptable incluso por-fila (ver
/// `entry_color`).
fn glowed(c: gpui::Rgba, g: Option<effects::Glow>) -> gpui::Rgba {
    let Some(g) = g else { return c };
    let t = g.strength * 0.25;
    gpui::Rgba {
        r: c.r + (1.0 - c.r) * t,
        g: c.g + (1.0 - c.g) * t,
        b: c.b + (1.0 - c.b) * t,
        a: c.a,
    }
}

/// Pinta las scanlines (ADR 0036 / G1 Task 4): franjas horizontales de 1px
/// cada `spacing_px`, negro puro a `opacity`. Primitivas de escena crudas
/// (`Window::paint_quad`) en vez de un `div()` por línea a propósito: en una
/// ventana típica (~600-900px de alto) con `spacing_px` en `[2, 16]` esto es
/// entre ~40 y ~450 quads por frame — barato como primitiva de pintado, pero
/// hubiera sido cientos de elementos GPUI reales (layout+medida+arena) si
/// cada línea fuera un `div()`, que es justo lo que el plan G1 descarta.
fn paint_scanlines(window: &mut Window, bounds: Bounds<Pixels>, s: Option<effects::Scanlines>) {
    let Some(s) = s else { return };
    let spacing = px(f32::from(s.spacing_px));
    // Belt-and-suspenders (BLOCKER de review): `s.spacing_px` ya pasó por
    // `clamp_u8([2, 16])` en `effects::decode_scanlines`, que en teoría
    // nunca deja pasar 0 — pero esa garantía vive lejos de este bucle, y
    // `while y < bottom { ... y += spacing }` con `spacing == px(0.0)`
    // colgaría la ventana (congelación por datos de tema editables por el
    // usuario). Corte defensivo local, independiente de que el piso de
    // arriba se mantenga.
    if spacing < px(1.0) {
        return;
    }
    let color = hsla(0.0, 0.0, 0.0, s.opacity);
    let bottom = bounds.origin.y + bounds.size.height;
    let mut y = bounds.origin.y;
    while y < bottom {
        let line = Bounds {
            origin: point(bounds.origin.x, y),
            size: size(bounds.size.width, px(1.0)),
        };
        window.paint_quad(fill(line, color));
        y += spacing;
    }
}

/// Pinta la viñeta (ADR 0036 / G1 Task 4) como 4 bandas de borde, cada una
/// con un `linear_gradient` de 2 paradas (negro a `strength` en el borde →
/// transparente hacia el centro). `gpui::linear_gradient` en este rev es una
/// línea recta de 2 paradas (`crates/gpui/src/color.rs`), sin repetición ni
/// N paradas — no hay radial ni "viñeta real" disponible en la API de
/// pintado sin un shader a medida; 4 bandas de borde es la aproximación
/// honesta más simple que SÍ ofrece. El alcance de cada banda (18% de su
/// dimensión) es una constante ajustada a ojo, sin mandato del ADR — se
/// documenta aquí, no allí.
///
/// Ángulos: la convención de `linear_gradient` es la de CSS
/// (`0.`=hacia arriba, giro horario) — la PRIMERA parada se ancla en el
/// extremo OPUESTO al ángulo, la última en el extremo que el ángulo señala.
/// Así, banda superior (negro en el borde superior, transparente hacia
/// abajo) pide ángulo 180 (que apunta "hacia abajo", ancla el negro arriba);
/// simétrico para las otras tres.
fn paint_vignette(window: &mut Window, bounds: Bounds<Pixels>, v: Option<effects::Vignette>) {
    let Some(v) = v else { return };
    // Los porcentajes de parada IMPORTAN aunque `linear_gradient` reciba los
    // colores posicionalmente (`from`/`to`): el shader usa el `percentage`
    // propio de cada `LinearColorStop` para re-normalizar `t` — si no
    // coincide con la posición (`from` en 0.0, `to` en 1.0), `t` sale
    // invertido (`1 - t`), que fue exactamente el bug detectado en el smoke
    // manual (negro en el borde INTERNO de la banda en vez del externo).
    let black = linear_color_stop(hsla(0.0, 0.0, 0.0, v.strength), 0.0);
    let transparent = linear_color_stop(hsla(0.0, 0.0, 0.0, 0.0), 1.0);
    let reach_y = bounds.size.height * 0.18;
    let reach_x = bounds.size.width * 0.18;

    let top = Bounds {
        origin: bounds.origin,
        size: size(bounds.size.width, reach_y),
    };
    window.paint_quad(fill(top, linear_gradient(180.0, black, transparent)));

    let bottom_band = Bounds {
        origin: point(
            bounds.origin.x,
            bounds.origin.y + bounds.size.height - reach_y,
        ),
        size: size(bounds.size.width, reach_y),
    };
    window.paint_quad(fill(bottom_band, linear_gradient(0.0, black, transparent)));

    let left = Bounds {
        origin: bounds.origin,
        size: size(reach_x, bounds.size.height),
    };
    window.paint_quad(fill(left, linear_gradient(90.0, black, transparent)));

    let right = Bounds {
        origin: point(
            bounds.origin.x + bounds.size.width - reach_x,
            bounds.origin.y,
        ),
        size: size(reach_x, bounds.size.height),
    };
    window.paint_quad(fill(right, linear_gradient(270.0, black, transparent)));
}

// Colores del chrome del dual-pane; pre-C2 look, usados SOLO como fallback
// para el canal que un tema deja sin declarar (`ChromeColors::resolve`, ver
// `chrome`) — el tema es canónico (mismo principio que los presets de
// keymap compartidos), no una piel sobre un aspecto congelado: el preset
// `default` declara casi todos los roles, así que el aspecto por defecto de
// la GUI pasa a ser el del tema `default` (consistente con la TUI), y estas
// constantes solo se ven cuando un tema deliberadamente minimalista deja un
// canal sin fijar.
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

/// Color de chrome del tema, con la constante pre-C2 como fallback: SOLO el
/// canal que el tema deja sin declarar para el rol cae al valor histórico —
/// un tema que declara el rol (el preset `default` declara casi todos, ver
/// `ChromeColors`) lo reemplaza entero. `fg`=false toma el canal de fondo del
/// rol.
fn chrome(theme: &Theme, role: Role, fg: bool, fallback: u32) -> gpui::Rgba {
    let style = theme.style(role);
    let c = if fg { style.fg } else { style.bg };
    c.map_or(gpui::rgb(fallback), theme_map::to_gpui_rgba)
}

/// Tipografía resuelta para la sesión (GP): fuente de chrome (UI) para
/// cabeceras/banners/franja de tasks y fuente mono para listados/visor —
/// alineación de columnas (tamaños, hex del visor) exige ancho fijo, algo que
/// una fuente UI proporcional no garantiza. Se construye UNA vez en `new` a
/// partir de `[ui]` (`cfg.common.ui_font`/`ui_mono_font`/`ui_font_size`); una
/// familia de usuario lleva SIEMPRE un fallback a la fuente empaquetada por
/// defecto, así que una familia mal tecleada degrada al look por defecto, no
/// a una fuente arbitraria del sistema (`family_with_fallback`).
struct FontSet {
    /// Fuente de chrome: cabeceras de pane/visor, banners, franja de tasks.
    ui: gpui::Font,
    /// Fuente monoespaciada: listados de pane, cuerpo del visor (texto/hex).
    mono: gpui::Font,
    /// Tamaño base (`[ui] font_size`, validado a `[8, 32]` en config).
    size: gpui::Pixels,
    /// Alto de fila derivado de `size` (1.5x, redondeado, con piso 18px —
    /// mismo rol que tenía `ROW_H` antes de GP, ahora dependiente de la
    /// config en vez de una constante fija).
    row_h: gpui::Pixels,
}

impl FontSet {
    /// Resuelve el set a partir de los tres escalares de `[ui]` (ya
    /// validados por `norte-config`). `None` en cualquiera = el default
    /// documentado en el campo correspondiente.
    fn resolve(font: Option<&str>, mono_font: Option<&str>, font_size: Option<f32>) -> Self {
        let size = font_size.unwrap_or(14.0);
        let row_h = (size * 1.5).round().max(18.0);
        Self {
            ui: family_with_fallback(font, ".SystemUIFont"),
            mono: family_with_fallback(mono_font, ".ZedMono"),
            size: gpui::px(size),
            row_h: gpui::px(row_h),
        }
    }
}

/// Familia de usuario con la fuente empaquetada como fallback EXPLÍCITO
/// (`Font::fallbacks`); `None` = la fuente empaquetada directamente, sin
/// fallback (no hace falta: ya es el default). `.SystemUIFont`/`.ZedMono` son
/// nombres reservados de GPUI (siempre resolubles, ver comentario de rev en
/// la cabecera del módulo) — jamás caen a una fuente arbitraria del sistema
/// que el usuario no pidió.
fn family_with_fallback(family: Option<&str>, bundled: &'static str) -> gpui::Font {
    match family {
        None => gpui::font(bundled),
        Some(f) => {
            let mut font = gpui::font(f);
            font.fallbacks = Some(gpui::FontFallbacks::from_fonts(vec![bundled.to_owned()]));
            font
        }
    }
}

/// La paleta de chrome resuelta para UN frame. Se construye UNA vez al
/// principio de `render()` — `render_row` corre por cada entrada visible y no
/// debe resolver el tema por fila.
///
/// Tres campos son PARES honestos con el tema (no canales sueltos), porque
/// el tema los declara como pares con sentido conjunto (p. ej. el TUI empareja
/// texto oscuro sobre la barra azul; usar un canal solo era lo que rompía la
/// legibilidad):
/// - `header_bg`/`header_fg`: la franja de cabecera (path del pane, cabecera
///   del visor) — ambos canales de `StatusBar`.
/// - `quick_fg`/`quick_bg`: resaltado de quick-search — ambos canales de
///   `Match`.
/// - `sel_bg` (fondo de fila seleccionada) + `sel_fg` opcional: si el tema
///   declara `Selection.fg`, ese color reemplaza el color por-tipo de la fila
///   seleccionada; si no lo declara, `sel_fg` es `None` y la fila conserva su
///   color de entrada (comportamiento histórico).
///
/// `Copy` a propósito: `render_pane` necesita mover una copia dentro del
/// closure `'static` de `cx.processor` (no puede prestarla del frame).
#[derive(Clone, Copy)]
struct ChromeColors {
    bg: gpui::Rgba,
    fg: gpui::Rgba,
    pane_bg: gpui::Rgba,
    pane_bg_focus: gpui::Rgba,
    header_bg: gpui::Rgba,
    header_fg: gpui::Rgba,
    border_focus: gpui::Rgba,
    border_unfocus: gpui::Rgba,
    sel_bg: gpui::Rgba,
    sel_fg: Option<gpui::Rgba>,
    err_fg: gpui::Rgba,
    quick_fg: gpui::Rgba,
    quick_bg: gpui::Rgba,
    mark_bg: gpui::Rgba,
}

impl ChromeColors {
    fn resolve(theme: &Theme) -> Self {
        Self {
            bg: chrome(theme, Role::Background, false, BG),
            fg: chrome(theme, Role::Regular, true, FG),
            pane_bg: chrome(theme, Role::PaneBackground, false, PANE_BG),
            pane_bg_focus: chrome(theme, Role::PaneFocusBackground, false, PANE_BG_FOCUS),
            // Par cabecera: ambos canales de `StatusBar` (ver doc del struct).
            header_bg: chrome(theme, Role::StatusBar, false, HEADER_BG),
            header_fg: chrome(theme, Role::StatusBar, true, FG),
            border_focus: chrome(theme, Role::BorderFocus, true, BORDER_FOCUS),
            border_unfocus: chrome(theme, Role::BorderUnfocused, true, BORDER_UNFOCUS),
            sel_bg: chrome(theme, Role::Selection, false, SEL_BG),
            // Sin fallback histórico: si el tema no declara `Selection.fg`,
            // `None` = la fila seleccionada conserva su color por-tipo
            // (comportamiento de siempre; nunca hubo un fg de selección).
            sel_fg: theme.style(Role::Selection).fg.map(theme_map::to_gpui_rgba),
            err_fg: chrome(theme, Role::Error, true, ERR_FG),
            // Par quick-search: ambos canales de `Match` (ver doc del struct).
            // El fallback de `quick_bg` es `HEADER_BG`: el combo histórico
            // (3 de los 4 usos) ya pintaba el resaltado sobre ese fondo.
            quick_fg: chrome(theme, Role::Match, true, QUICK_FG),
            quick_bg: chrome(theme, Role::Match, false, HEADER_BG),
            mark_bg: chrome(theme, Role::Mark, false, MARK_BG),
        }
    }

    /// Aplica el glow de G1 (ADR 0036 decisión 3) a cada campo FG — BG queda
    /// intacto, incluido `mark_bg` (es un fondo, pese al nombre). Builder
    /// consumidor: el ÚNICO sitio donde `ChromeColors` recibe el
    /// post-proceso de brillo, llamado una vez por frame justo tras
    /// `resolve` (ver `render`) — nunca por fila (eso es `entry_color`).
    /// `g = None` es un no-op (cada campo pasa por `glowed`, que ya lo trata
    /// como identidad), así que los call-sites no necesitan ramificar.
    fn with_glow(mut self, g: Option<effects::Glow>) -> Self {
        self.fg = glowed(self.fg, g);
        self.header_fg = glowed(self.header_fg, g);
        self.border_focus = glowed(self.border_focus, g);
        self.border_unfocus = glowed(self.border_unfocus, g);
        self.sel_fg = self.sel_fg.map(|c| glowed(c, g));
        self.err_fg = glowed(self.err_fg, g);
        self.quick_fg = glowed(self.quick_fg, g);
        self
    }
}

impl Render for NorteGui {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Instrumentación (medición del lag, gated por NORTE_GUI_DEBUG): tiempo
        // de CONSTRUCCIÓN del árbol de elementos (nuestro coste; el layout/paint
        // de GPUI ocurre después de devolver). Si escala con las entradas, el
        // culpable es el O(N)-por-frame (display_name/tema recomputados por fila
        // sin virtualizar). Ver issue #87.
        let _t0 = std::time::Instant::now();

        // Paleta de chrome resuelta UNA vez por frame (ver doc de
        // `ChromeColors`): `render_row` corre por cada fila visible y no debe
        // resolver el tema por fila.
        let chrome =
            ChromeColors::resolve(&self.theme).with_glow(self.effects.and_then(|e| e.glow));

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
            .bg(chrome.bg)
            .text_color(chrome.fg)
            // Tipografía (GP): fuente de chrome + tamaño base para TODO el
            // árbol por cascada (`TextStyleRefinement`); los contenedores
            // mono (listados/visor/franja de tasks) se reponen encima, más
            // abajo en el árbol.
            .font(self.fonts.ui.clone())
            .text_size(self.fonts.size)
            .p(px(4.0))
            .gap(px(2.0));

        // Bezel (ADR 0036 / G1 Task 4): radio de esquina paramétrico —
        // `Styled::rounded(AbsoluteLength)` acepta un `px(n)` cualquiera (a
        // diferencia de los `rounded_lg`/`rounded_full` fijos de Tailwind
        // que trae GPUI, ver `crates/gpui_macros/src/styles.rs`
        // `corner_prefixes`), así que no hace falta cuantizar `radius_px` a
        // un preset. `inset`: `BoxShadow` en este rev SÍ trae un flag
        // `inset` real (`crates/gpui/src/style.rs`, pinta DENTRO del
        // bounds), así que el marco es una sombra insertada de verdad, no
        // un borde disfrazándola.
        if let Some(b) = self.effects.and_then(|e| e.bezel) {
            root = root.rounded(px(f32::from(b.radius_px)));
            if b.inset {
                root = root.shadow(vec![
                    BoxShadow::new(px(0.0), px(0.0), hsla(0.0, 0.0, 0.0, 0.55))
                        .blur_radius(px(8.0))
                        .spread_radius(px(-3.0))
                        .inset(),
                ]);
            }
        }

        // Banner de arranque (GUI-c T3 + C2 revisión): keymap roto, config
        // inválida, tema inválido o preset desconocido — ninguno tumba la
        // GUI, todos avisan aquí una vez por sesión. `keymap_error` puede
        // traer VARIOS mensajes unidos por `'\n'` (`push_banner`, MINOR 5):
        // un div truncado POR LÍNEA en vez de un separador textual — así
        // cada aviso se lee entero (hasta el ancho) sin competir por el
        // mismo renglón.
        if let Some(msg) = &self.keymap_error {
            // Revisión final (contraste WCAG): `header_bg`+`err_fg` medía
            // 1.1:1 en catppuccin — `err_fg` está pensado para el fondo
            // PRINCIPAL (`bg`), como el error en pane de más abajo (~1239),
            // no para `header_bg` (que trae su propio `header_fg`).
            let mut banner = div()
                .flex()
                .flex_col()
                .bg(chrome.bg)
                .text_color(chrome.err_fg);
            for line in msg.split('\n') {
                banner = banner.child(
                    div()
                        .px(px(4.0))
                        .py(px(1.0))
                        .truncate()
                        .child(SharedString::from(line.to_owned())),
                );
            }
            root = root.child(banner);
        }

        // Visor (F3) a pantalla completa, el estado «abriendo…» mientras
        // llega, o el dual-pane: pantallas mutuamente excluyentes (ver
        // `on_key`, que enruta al visor primero cuando ya está abierto).
        if self.viewer.is_some() {
            root = root.child(self.render_viewer(window, &chrome, cx));
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
                .child(self.render_pane(0, &chrome, cx))
                .child(self.render_pane(1, &chrome, cx));
            root = root.child(panes_row).child(self.render_task_strip(&chrome));
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
                    .bg(chrome.quick_bg)
                    .text_color(chrome.quick_fg)
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
                    .child(self.render_modal(m, &chrome)),
            );
        }

        // Overlay de efectos (ADR 0036 / G1 Task 4): scanlines + viñeta,
        // pintados AL FINAL (por encima del scrim del modal, si hay uno —
        // un CRT tiene su viñeta siempre encima, modal incluido). Un solo
        // `canvas()` (no un `div()` por línea de scanline: cientos de
        // elementos GPUI reales sería un costo por-frame de verdad; un
        // `PaintQuad` es una primitiva de escena cruda, barata incluso en
        // cientos — ver `paint_scanlines`).
        //
        // Transparencia al input POR CONSTRUCCIÓN, no por una API opt-out:
        // `gpui::canvas` (`crates/gpui/src/elements/canvas.rs`, rev
        // f14fea9) es un `Element` que jamás llama `Window::insert_hitbox`
        // — ni en `prepaint` ni en `paint`. El despacho de mouse de GPUI
        // solo considera los hitboxes que un elemento registró durante su
        // `prepaint` (`Div`/`Interactivity::should_insert_hitbox` decide
        // caso a caso si vale la pena para UN div interactivo,
        // `crates/gpui/src/elements/div.rs`); un `canvas` sin listeners no
        // participa en absoluto en el hit-test, así que no puede capturar
        // clicks/scroll/teclas sin importar lo que pinte. Confirmado en el
        // smoke manual (Task 4 paso 5): click selecciona fila, la rueda
        // mueve el pane y las teclas actúan con scanlines+viñeta activos.
        if let Some(eff) = self.effects
            && (eff.scanlines.is_some() || eff.vignette.is_some())
        {
            root = root.child(
                canvas(
                    move |_bounds, _window, _cx| {},
                    move |bounds, (), window, _cx| {
                        paint_scanlines(window, bounds, eff.scanlines);
                        paint_vignette(window, bounds, eff.vignette);
                    },
                )
                .absolute()
                .inset_0(),
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

/// Tope de un detalle de banner (auditoría de encoding final, #73): mismo
/// valor que `norte_tui::app::DETAIL_MAX_CHARS` — un `preset`/tema/TOML
/// hostil bajo `./.norte` puede citar valores kilométricos, y sin tope
/// desbordarían la línea del banner igual que desbordarían la barra de la
/// TUI.
const BANNER_DETAIL_MAX_CHARS: usize = 160;

/// Sanea un mensaje de error para el banner de arranque (revisión C2/G0
/// MINOR 4; auditoría de encoding final #73 — MEDIUM-LOW 1: ahora también
/// TOPA, espejo de `norte_tui::app::detail_for_bar`): la config (o el nombre
/// del preset) puede venir de la capa de PROYECTO (`./.norte` de un repo
/// AJENO/clonado) y los diagnósticos citan fragmentos crudos del propio
/// fichero — bidi/invisibles sin enmascarar serían un hazard de terminal, y
/// sin tope un valor kilométrico desbordaría el banner. Reusa el MISMO saneo
/// que el resto de superficies de esta GUI (`norte_frontend::display_name`,
/// vía bytes: el mismo camino que un nombre de fichero hostil), luego recorta
/// a [`BANNER_DETAIL_MAX_CHARS`] con una elipsis marcando el corte.
fn banner_safe(s: &str) -> String {
    let masked = norte_frontend::display_name(s.as_bytes()).0;
    let mut out: String = masked.chars().take(BANNER_DETAIL_MAX_CHARS).collect();
    if masked.chars().nth(BANNER_DETAIL_MAX_CHARS).is_some() {
        out.push('…');
    }
    out
}

/// Categoría LOCALIZADA de un `io::Error` del SO (auditoría de encoding
/// final #73 — MEDIUM 3): espejo de `norte_tui::app::io_error_category`
/// (mismas claves Fluent COMPARTIDAS, `err-not-found`/`err-permission-
/// denied`/`err-no-space`/`err-io`) — jamás el `Display` del `io::Error`, que
/// el SO redacta en SU idioma («Permission denied (os error 13)»,
/// «Permiso denegado», …) sin que Fluent tenga ninguna oportunidad de
/// traducirlo: mezclarlo en un banner por lo demás localizado rompe la
/// paridad de idioma (regla 1: nunca texto crudo del sistema).
fn io_error_category(e: &std::io::Error) -> String {
    let key = match e.kind() {
        std::io::ErrorKind::NotFound => "err-not-found",
        std::io::ErrorKind::PermissionDenied => "err-permission-denied",
        std::io::ErrorKind::StorageFull => "err-no-space",
        _ => "err-io",
    };
    norte_i18n::t(key)
}

/// Banner LOCALIZADO de un [`norte_config::ConfigError`] (auditoría de
/// encoding final #73 — MEDIUM 3): antes se interpolaba `e.to_string()`
/// entero en `gui-banner-config-invalid` — el `Display` de `Io` incluye el
/// `io::Error` del SO sin traducir (ver [`io_error_category`]) y el de
/// `Toml` cita el mensaje del parser SIN tope (un `norte.toml` hostil de
/// `./.norte`, un repo AJENO, puede citar valores kilométricos). Cada
/// variante mapea a su propia clave (`gui-banner-config-io`/`-parse`), con
/// el `path`/mensaje siempre por [`banner_safe`] (enmascara Y topa).
fn config_error_banner(e: &norte_config::ConfigError) -> String {
    use norte_config::ConfigError;
    match e {
        ConfigError::Io { path, source } => norte_i18n::ta(
            "gui-banner-config-io",
            &[
                ("path", banner_safe(&path.display().to_string()).as_str()),
                ("error", io_error_category(source).as_str()),
            ],
        ),
        ConfigError::Toml { path, message } => norte_i18n::ta(
            "gui-banner-config-parse",
            &[
                ("path", banner_safe(&path.display().to_string()).as_str()),
                ("detail", banner_safe(message).as_str()),
            ],
        ),
    }
}

/// Banner LOCALIZADO de un [`norte_frontend::theme::ResolveError`]
/// (auditoría de encoding final #73 — MEDIUM 3): espejo de
/// [`config_error_banner`] (mismo par de variantes `{spec/path, source}` /
/// `{spec/path, detail}` documentado en el contrato de
/// `norte_frontend::theme` — un `[ui].theme` hostil de `./.norte` cae por el
/// mismo camino saneado).
fn theme_error_banner(e: &norte_frontend::theme::ResolveError) -> String {
    use norte_frontend::theme::ResolveError;
    match e {
        ResolveError::Io { spec, source } => norte_i18n::ta(
            "gui-banner-theme-io",
            &[
                ("spec", banner_safe(spec).as_str()),
                ("error", io_error_category(source).as_str()),
            ],
        ),
        ResolveError::Parse { spec, detail } => norte_i18n::ta(
            "gui-banner-theme-parse",
            &[
                ("spec", banner_safe(spec).as_str()),
                ("detail", banner_safe(detail).as_str()),
            ],
        ),
    }
}

/// Detalle accionable de un [`norte_frontend::keymap::KeymapError`] para el
/// banner (auditoría de encoding final #73 — MEDIUM 3): su `Display`
/// (thiserror) es prosa en CASTELLANO fija — mezclada en un banner que el
/// resto del tiempo sale en el idioma del usuario (`gui-banner-keymap-error`,
/// ya Fluent), rompe la paridad de idioma para un usuario en inglés. Las
/// variantes con contenido de USUARIO embebido (chord/comando/secuencia/
/// diagnóstico TOML — cualquiera puede venir de un `keymap.toml` hostil de
/// `./.norte`) devuelven SOLO ese contenido, por [`banner_safe`] (enmascara Y
/// topa) — sin la prosa española alrededor. `WrongLayerKey` es la única
/// variante SIN payload de usuario (`layer`/`key` son literales `&'static
/// str` en inglés: `"preset"`/`"usuario"`, `"keymap"`/`"prepend/append"`): su
/// `Display` completo es aceptable tal cual (nada de SO, nada sin
/// enmascarar) — reescribir esa única frase a Fluent queda fuera de alcance
/// de esta auditoría.
fn keymap_error_detail(e: &norte_frontend::keymap::KeymapError) -> String {
    use norte_frontend::keymap::KeymapError;
    match e {
        KeymapError::Toml(msg) => banner_safe(msg),
        KeymapError::BadChord { chord } | KeymapError::ShiftWithChar { chord } => {
            banner_safe(chord)
        }
        KeymapError::EmptySequence { run } | KeymapError::UnknownCommand { run } => {
            banner_safe(run)
        }
        KeymapError::EscInSequence { sequence } => banner_safe(sequence),
        KeymapError::AmbiguousPrefix { shorter, longer } => {
            format!("{} / {}", banner_safe(shorter), banner_safe(longer))
        }
        KeymapError::WrongLayerKey { .. } => banner_safe(&e.to_string()),
    }
}

/// Añade `msg` al banner de arranque acumulado (revisión C2/G0 MINOR 5): UN
/// mensaje por LÍNEA — sin separador textual (nada de `"; "` ni una frase
/// localizada de por medio): `render` (ver el bloque `keymap_error`) parte
/// por `'\n'` y pinta cada mensaje en su propio div truncado, así que el
/// salto de línea YA es la separación visual.
fn push_banner(existing: Option<String>, msg: String) -> String {
    match existing {
        Some(prev) => format!("{prev}\n{msg}"),
        None => msg,
    }
}

/// Aviso de preset desconocido (revisión C2/G0 IMPORTANT 2): la TUI ERRA
/// ruidosamente (`KeymapsError::UnknownPreset`,
/// `norte-tui/src/app.rs::keymaps_error_category`); `keymap::preset` aquí
/// degrada EN SILENCIO al orthodox compartido (contrato del catálogo: "cae
/// al default + avisa" — el aviso es cosa del CALLER, ver
/// `keymap::is_known_preset`). `None` si `preset_name` es uno de los tres
/// presets embebidos; si no, el mensaje LISTO para el banner (misma clave
/// Fluent compartida que ya usa esa categoría de error de la TUI,
/// `err-keymap-preset-unknown`). Extraída pura (sin GPUI) para test directo.
fn unknown_preset_banner(preset_name: &str) -> Option<String> {
    if keymap::is_known_preset(preset_name) {
        return None;
    }
    let available = keymap::KNOWN_PRESETS.join(", ");
    Some(norte_i18n::ta(
        "err-keymap-preset-unknown",
        &[
            ("name", banner_safe(preset_name).as_str()),
            ("available", available.as_str()),
        ],
    ))
}

fn main() {
    // Configuración real (C2): capas compartidas — escalares + keymap.
    // Bloqueante A PROPÓSITO: arranque, antes de que exista la ventana; no
    // hay runtime async aquí todavía. UNA sola carga (el keymap.rs de la GUI
    // vuelve a leer `keymap.toml` por su cuenta dentro de `build_effectives`
    // — segunda lectura redundante ACEPTADA, ver su rustdoc).
    let loaded = norte_frontend::config::load(&norte_config::standard_layers());

    // Idioma (GUI-e T1 + C2): mismo orden que la TUI (`crates/norte-tui/src/
    // main.rs:224-225`): `NORTE_LANG` explícito > `[ui] lang` de la config >
    // entorno (`LC_ALL`/`LC_MESSAGES`/`LANG`). Debe ir ANTES de construir la
    // ventana: los banners de arranque (`keymap_error`, config inválida) ya
    // salen localizados desde `NorteGui::new`.
    let lang = if std::env::var("NORTE_LANG").is_ok_and(|v| !v.is_empty()) {
        norte_i18n::Lang::from_env()
    } else if let Some(l) = loaded
        .as_ref()
        .ok()
        .and_then(|c| c.common.ui_lang.as_deref())
    {
        norte_i18n::Lang::negotiate(Some(l))
    } else {
        norte_i18n::Lang::from_env()
    };
    let _ = norte_i18n::force(lang);

    application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1000.0), px(640.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| NorteGui::new(window, cx, &loaded)),
        )
        .expect("no se pudo abrir la ventana GPUI");

        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::effects;
    use super::{
        BANNER_DETAIL_MAX_CHARS, BG, BORDER_FOCUS, BORDER_UNFOCUS, ERR_FG, FG, HEADER_BG, MARK_BG,
        PANE_BG, PANE_BG_FOCUS, QUICK_FG, SEL_BG,
    };
    use super::{
        ChromeColors, FontSet, ImagePreview, affected_dirs, apply_viewer_command, banner_safe,
        confirm_quit_task_count, first_cancelable, generation_is_current, glowed, has_pending_work,
        image_preview_from, image_status, keymap_error_detail, modal_footer_colors,
        modal_panel_colors, modal_title_colors, pending_hint, retain_active, row_label,
        task_at_cursor, unknown_preset_banner, viewer_header, viewer_status,
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
        let preview = image_preview_from(crate::session::decode_image(&png));
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
        let bad = image_preview_from(crate::session::decode_image(&head));
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
            image_preview_from(crate::session::decode_image(&png)),
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

    /// Revisión C2/G0 IMPORTANT 3: solo tasks + solo marcas + ambos + ninguno
    /// — cada combinación por separado, no solo el `||` agregado.
    #[test]
    fn has_pending_work_tasks_o_marcas_o_ninguno() {
        assert!(!has_pending_work(0, 0), "nada pendiente → false");
        assert!(has_pending_work(1, 0), "solo tasks → true");
        assert!(has_pending_work(0, 1), "solo marcas → true");
        assert!(has_pending_work(3, 2), "ambos → true");
    }

    /// Revisión C2/G0 IMPORTANT 2: los tres presets de fábrica NO avisan;
    /// uno inventado sí, con su propio nombre y la lista de disponibles en
    /// el mensaje (accionable, no un aviso mudo).
    #[test]
    fn preset_desconocido_avisa() {
        for name in ["orthodox", "vim", "cua"] {
            assert!(
                unknown_preset_banner(name).is_none(),
                "preset de fábrica {name:?}: no debe avisar"
            );
        }
        let msg = unknown_preset_banner("vintage").expect("nombre desconocido: avisa");
        assert!(msg.contains("vintage"), "nombra lo pedido: {msg}");
        assert!(msg.contains("orthodox"), "lista lo disponible: {msg}");
        assert!(msg.contains("vim"), "lista lo disponible: {msg}");
        assert!(msg.contains("cua"), "lista lo disponible: {msg}");
    }

    /// `unknown_preset_banner` sobre TODO el corpus hostil de `norte-testkit`,
    /// usado como nombre de PRESET (auditoría de encoding final #73 — hueco
    /// de test 2): un `[ui].preset` hostil de `./.norte`, un repo AJENO, pasa
    /// por AQUÍ. Dos propiedades, cada una sobre lo que de verdad las
    /// garantiza:
    /// - el banner COMPUESTO (mismo criterio que el pin de `row_label`: sin
    ///   ningún carácter `is_terminal_hazard` crudo) — la prosa del propio
    ///   `.ftl` es ASCII controlado por nosotros, así que un hazard ahí solo
    ///   puede venir del nombre interpolado.
    /// - el nombre ENMASCARADO (`banner_safe`, lo que de verdad entra al
    ///   argumento `name`) respeta `BANNER_DETAIL_MAX_CHARS` (+1 por la
    ///   elipsis del corte, mismo margen que el pin equivalente de la TUI,
    ///   `norte-tui/src/app.rs`) — medir la longitud del banner COMPLETO no
    ///   tendría sentido: la prosa fija alrededor del nombre ya suma más que
    ///   el tope por sí sola.
    #[test]
    fn unknown_preset_banner_sobre_el_corpus_hostil_no_deja_hazards_ni_desborda() {
        for fixture in norte_testkit::corpus::hostile_names() {
            let name = String::from_utf8_lossy(&fixture.bytes).into_owned();

            let masked = banner_safe(&name);
            assert!(
                masked.chars().count() <= BANNER_DETAIL_MAX_CHARS + 1,
                "{}: banner_safe no topó el nombre ({} chars)",
                fixture.id,
                masked.chars().count(),
            );

            // Un fixture hostil que por casualidad IGUALARA "orthodox"/"vim"/
            // "cua" no avisaría (contrato de `unknown_preset_banner`); ningún
            // fixture del corpus lo hace, pero no se asume — se salta limpio.
            let Some(banner) = unknown_preset_banner(&name) else {
                continue;
            };
            assert!(
                !banner.chars().any(norte_encoding::is_terminal_hazard),
                "{}: unknown_preset_banner dejó un hazard crudo en {banner:?}",
                fixture.id,
            );
        }
    }

    /// `config_error_banner` (auditoría de encoding final #73 — MEDIUM 3):
    /// `Io` jamás interpola el `Display` crudo del `io::Error` del SO —
    /// aunque ese `Display` traiga prosa que PARECE ya localizada (simulado
    /// aquí a propósito), el banner debe llevar la categoría de
    /// `io_error_category` (`err-permission-denied`, Fluent, ES forzado) y
    /// NO el texto del SO. El path (que puede venir de `./.norte`, un repo
    /// AJENO) sale por `banner_safe`.
    #[test]
    fn config_error_banner_nunca_interpola_el_display_del_so() {
        let _ = norte_i18n::force(norte_i18n::Lang::Es);
        let so_dice = "esto NO debe aparecer en el banner (os error 13)";
        let e = norte_config::ConfigError::Io {
            path: std::path::PathBuf::from("./.norte/norte.toml"),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, so_dice),
        };
        let banner = super::config_error_banner(&e);
        assert!(
            !banner.contains(so_dice),
            "el Display crudo del SO se filtró: {banner:?}"
        );
        assert!(
            banner.contains("permiso denegado"),
            "falta la categoría localizada (ES): {banner:?}"
        );
    }

    /// `config_error_banner` (auditoría de encoding final #73 — MEDIUM 3):
    /// `Toml` topa el mensaje del parser — un `norte.toml` hostil de
    /// `./.norte` puede citar un valor kilométrico, y el diagnóstico
    /// (`message`, ya compacto por `toml_diag` — ver `norte-config`) se
    /// enmascara de todas formas.
    #[test]
    fn config_error_banner_topa_un_mensaje_toml_kilometrico() {
        let kilometrico = "x".repeat(BANNER_DETAIL_MAX_CHARS * 4);
        let e = norte_config::ConfigError::Toml {
            path: std::path::PathBuf::from("norte.toml"),
            message: kilometrico,
        };
        let banner = super::config_error_banner(&e);
        assert!(
            banner.chars().count() <= BANNER_DETAIL_MAX_CHARS * 2,
            "el mensaje TOML no se topó: {} chars",
            banner.chars().count(),
        );
        assert!(banner.contains('…'), "falta la marca de corte: {banner:?}");
    }

    /// `theme_error_banner` (auditoría de encoding final #73 — MEDIUM 3):
    /// espejo de `config_error_banner_nunca_interpola_el_display_del_so`
    /// para `ResolveError::Io` (un `[ui].theme` hostil de `./.norte`).
    #[test]
    fn theme_error_banner_nunca_interpola_el_display_del_so() {
        let _ = norte_i18n::force(norte_i18n::Lang::Es);
        let so_dice = "esto NO debe aparecer en el banner (os error 2)";
        let e = norte_frontend::theme::ResolveError::Io {
            spec: "./.norte/tema-hostil.toml".to_owned(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, so_dice),
        };
        let banner = super::theme_error_banner(&e);
        assert!(
            !banner.contains(so_dice),
            "el Display crudo del SO se filtró: {banner:?}"
        );
        assert!(
            banner.contains("no encontrado"),
            "falta la categoría localizada (ES): {banner:?}"
        );
    }

    /// `theme_error_banner` (auditoría de encoding final #73 — MEDIUM 3):
    /// `Parse` enmascara y topa TANTO `spec` como `detail` — ambos pueden
    /// venir de un tema hostil de `./.norte`.
    #[test]
    fn theme_error_banner_topa_spec_y_detail() {
        let kilometrico = "y".repeat(BANNER_DETAIL_MAX_CHARS * 4);
        let e = norte_frontend::theme::ResolveError::Parse {
            spec: kilometrico.clone(),
            detail: kilometrico,
        };
        let banner = super::theme_error_banner(&e);
        assert!(
            banner.chars().count() <= BANNER_DETAIL_MAX_CHARS * 4,
            "spec+detail no se toparon: {} chars",
            banner.chars().count(),
        );
        // Dos cortes: uno en spec, otro en detail — cada uno con su propia
        // elipsis (`banner_safe` topa cada argumento por separado).
        assert_eq!(
            banner.matches('…').count(),
            2,
            "esperaba una elipsis por cada campo topado: {banner:?}"
        );
    }

    /// `keymap_error_detail` (auditoría de encoding final #73 — MEDIUM 3):
    /// las variantes con contenido de USUARIO (chord/comando/secuencia/TOML)
    /// devuelven SOLO ese contenido — nunca la prosa castellana fija que
    /// trae `Display` alrededor (rompería la paridad de idioma en un banner
    /// EN inglés).
    #[test]
    fn keymap_error_detail_extrae_el_contenido_de_usuario_sin_prosa_castellana() {
        use norte_frontend::keymap::KeymapError;
        let cases: &[(KeymapError, &str)] = &[
            (
                KeymapError::BadChord {
                    chord: "megatecla".to_owned(),
                },
                "megatecla",
            ),
            (
                KeymapError::UnknownCommand {
                    run: "pane.teletransportar".to_owned(),
                },
                "pane.teletransportar",
            ),
            (
                KeymapError::Toml("unknown field `bindingz`".to_owned()),
                "unknown field `bindingz`",
            ),
        ];
        for (e, expected_content) in cases {
            let detail = keymap_error_detail(e);
            assert_eq!(
                detail, *expected_content,
                "debía ser SOLO el contenido de usuario, sin prosa: {detail:?}"
            );
            assert!(
                !detail.contains("inválida") && !detail.contains("desconocido"),
                "se coló prosa castellana del Display: {detail:?}"
            );
        }

        // Sin payload de usuario (literales `&'static str` en inglés): el
        // `Display` completo es aceptable (juicio explícito del audit).
        let sin_payload = KeymapError::WrongLayerKey {
            layer: "usuario",
            key: "keymap",
        };
        let detail = keymap_error_detail(&sin_payload);
        assert_eq!(detail, sin_payload.to_string());
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

    /// El tema es CANÓNICO (G0, decisión del 2026-07-23): el preset `default`
    /// declara casi todos los roles, así que el aspecto por defecto de la GUI
    /// pasa a ser el del tema `default.toml` (consistente con la TUI) — NO el
    /// aspecto histórico pre-tema. Valores leídos directamente de
    /// `crates/norte-theme/presets/default.toml`.
    #[test]
    fn tema_default_reproduce_el_tema_default() {
        let t = norte_theme::Theme::preset_default();
        let c = ChromeColors::resolve(&t);
        assert_eq!(c.bg, gpui::rgb(0x1c1c1c), "background.bg");
        assert_eq!(c.fg, gpui::rgb(0xd0d0d0), "regular.fg");
        // pane-background/pane-focus-background/mark: coinciden con las
        // constantes históricas (se fijaron a esos valores a propósito en
        // `fc8f5a5`, el commit que añadió estos tres roles nuevos).
        assert_eq!(c.pane_bg, gpui::rgb(0x1e1e1e), "pane-background.bg");
        assert_eq!(
            c.pane_bg_focus,
            gpui::rgb(0x252526),
            "pane-focus-background.bg"
        );
        assert_eq!(c.mark_bg, gpui::rgb(0x3d3315), "mark.bg");
        // Par cabecera: status-bar empareja texto oscuro sobre barra azul.
        assert_eq!(c.header_bg, gpui::rgb(0x5fafd7), "status-bar.bg");
        assert_eq!(c.header_fg, gpui::rgb(0x1c1c1c), "status-bar.fg");
        assert_eq!(c.border_focus, gpui::rgb(0x5fafd7), "border-focus.fg");
        assert_eq!(c.border_unfocus, gpui::rgb(0x6c6c6c), "border-unfocused.fg");
        assert_eq!(c.sel_bg, gpui::rgb(0x3a3a3a), "selection.bg");
        assert_eq!(
            c.sel_fg,
            Some(gpui::rgb(0xffffff)),
            "selection.fg declarado → Some"
        );
        assert_eq!(c.err_fg, gpui::rgb(0xd75f5f), "error.fg");
        // Par quick-search: match empareja texto oscuro sobre resaltado dorado.
        assert_eq!(c.quick_fg, gpui::rgb(0x1c1c1c), "match.fg");
        assert_eq!(c.quick_bg, gpui::rgb(0xd7af5f), "match.bg");
    }

    /// Un tema mínimo que no declara `[roles]` dispara el fallback de
    /// `Role::fallback()` para TODOS los roles (sin color) — cada canal de
    /// `ChromeColors` debe entonces caer a su constante histórica pre-C2
    /// (`chrome`, ver su doc), y `sel_fg` a `None` (nunca hubo un fg de
    /// selección separado del color por-tipo antes de esta migración).
    #[test]
    fn canal_ausente_cae_a_la_constante_historica() {
        let t = norte_theme::Theme::from_toml("name = \"x\"\n").expect("tema mínimo parsea");
        let c = ChromeColors::resolve(&t);
        assert_eq!(c.bg, gpui::rgb(BG));
        assert_eq!(c.fg, gpui::rgb(FG));
        assert_eq!(c.pane_bg, gpui::rgb(PANE_BG));
        assert_eq!(c.pane_bg_focus, gpui::rgb(PANE_BG_FOCUS));
        assert_eq!(c.header_bg, gpui::rgb(HEADER_BG));
        assert_eq!(c.header_fg, gpui::rgb(FG));
        assert_eq!(c.border_focus, gpui::rgb(BORDER_FOCUS));
        assert_eq!(c.border_unfocus, gpui::rgb(BORDER_UNFOCUS));
        assert_eq!(c.sel_bg, gpui::rgb(SEL_BG));
        assert_eq!(c.sel_fg, None, "sin Selection.fg declarado → sin override");
        assert_eq!(c.err_fg, gpui::rgb(ERR_FG));
        assert_eq!(c.quick_fg, gpui::rgb(QUICK_FG));
        assert_eq!(c.quick_bg, gpui::rgb(HEADER_BG));
        assert_eq!(c.mark_bg, gpui::rgb(MARK_BG));
    }

    /// Revisión final del review (contraste WCAG, medido 1.1-1.9:1 con el
    /// par roto `header_bg`+`fg`): las COMBINACIONES que usa el modal en el
    /// tema `default`, no canales sueltos (esos ya los pinea `tema_default_
    /// reproduce_el_tema_default`). El panel usa el MISMO par que los panes
    /// (`pane_bg_focus`+`fg`), no el de la cabecera; el título SÍ usa el par
    /// completo de cabecera; el pie usa el par de selección declarado por
    /// el tema.
    #[test]
    fn modal_usa_pares_honestos() {
        let t = norte_theme::Theme::preset_default();
        let c = ChromeColors::resolve(&t);

        let (panel_bg, panel_fg) = modal_panel_colors(&c);
        assert_eq!(panel_bg, c.pane_bg_focus, "panel bg == pane_bg_focus");
        assert_eq!(panel_fg, c.fg, "panel fg == regular fg");
        assert_ne!(
            panel_bg, c.header_bg,
            "el panel NO debe reusar el fondo de cabecera (por eso era ilegible)"
        );

        let (title_bg, title_fg) = modal_title_colors(&c);
        assert_eq!(title_bg, c.header_bg, "título bg == header_bg");
        assert_eq!(
            title_fg, c.header_fg,
            "título fg == header_fg cuando el fondo es header_bg (el par completo)"
        );

        let (footer_bg, footer_fg) = modal_footer_colors(&c);
        assert_eq!(footer_bg, c.sel_bg, "pie bg == sel_bg");
        assert_eq!(
            footer_fg,
            Some(gpui::rgb(0xffffff)),
            "el tema default declara selection.fg → el pie lo hereda"
        );
    }

    /// `confirm_quit_task_count` (MINOR 3 del review final): normalmente el
    /// conteo de `task_progress`; solo cae a `inflight_len` cuando NO hay
    /// progreso NI marcas (así nunca se solapan los dos números).
    #[test]
    fn confirm_quit_task_count_sustituye_solo_cuando_no_hay_progreso_ni_marcas() {
        assert_eq!(
            confirm_quit_task_count(0, 0, 2),
            2,
            "0 tasks, 0 marcas, 2 inflight → muestra las 2 en vuelo, no «0 task(s)»"
        );
        assert_eq!(
            confirm_quit_task_count(3, 0, 5),
            3,
            "ya hay progreso → NUNCA se suma/reemplaza por inflight"
        );
        assert_eq!(
            confirm_quit_task_count(0, 1, 5),
            0,
            "hay marcas (hay algo que confirmar igual) → no se sustituye por inflight"
        );
        assert_eq!(
            confirm_quit_task_count(0, 0, 0),
            0,
            "nada en absoluto → 0, tal cual (el gate ni siquiera abriría el modal)"
        );
    }

    /// G1 Task 4 pin: el tema por defecto no declara `[effects]`, así que
    /// `self.effects` (poblado en `new` con esta misma llamada) nace `None`
    /// — cada rama de `render()` que pinta un efecto está detrás de un
    /// `if let Some`, así que esto basta para que el árbol de render sea
    /// byte-idéntico al de antes de G1 (identidad por construcción, no algo
    /// que un test de render tenga que reverificar).
    #[test]
    fn sin_effects_no_hay_overlay() {
        let t = norte_theme::Theme::preset_default();
        assert!(effects::EffectsV1::from_theme(&t).0.is_none());
    }

    /// G1 Task 4 pin: el preset `retro-crt` (Task 2) trae las 4 claves —
    /// bezel incluido, distinto del test más granular de `effects.rs`
    /// (que solo cubre scanlines) — este confirma que EL RENDER tiene todo
    /// lo que necesita para pintar overlay + bezel + glow a la vez.
    #[test]
    fn retro_crt_activa_effects() {
        let t = norte_theme::Theme::preset("retro-crt")
            .unwrap()
            .expect("preset registrado");
        let e = effects::EffectsV1::from_theme(&t)
            .0
            .expect("retro-crt trae [effects]");
        assert!(e.scanlines.is_some(), "scanlines");
        assert!(e.vignette.is_some(), "vignette");
        assert!(e.glow.is_some(), "glow");
        assert!(e.bezel.is_some(), "bezel");
    }

    /// `glowed` (ADR 0036 decisión 3): `lerp(fg, white, strength * 0.25)` por
    /// canal, canal `a` sin tocar. `strength = 1.0` sobre negro puro debe dar
    /// ~0.25 en los tres canales (el máximo de brillo del esquema v1, no
    /// blanco puro — el glow es sutil a propósito). `g = None` es la
    /// identidad exacta.
    #[test]
    fn glowed_lerp_correcto() {
        let black = gpui::Rgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        };
        let g = glowed(black, Some(effects::Glow { strength: 1.0 }));
        assert!((g.r - 0.25).abs() < 1e-6, "r ~0.25, got {}", g.r);
        assert!((g.g - 0.25).abs() < 1e-6, "g ~0.25, got {}", g.g);
        assert!((g.b - 0.25).abs() < 1e-6, "b ~0.25, got {}", g.b);
        assert!((g.a - 1.0).abs() < 1e-6, "alpha jamás se toca");

        let c = gpui::rgb(0x336699);
        assert_eq!(glowed(c, None), c, "sin glow, identidad exacta");
    }

    /// Sin config, `FontSet` cae a las familias empaquetadas de GPUI
    /// (`.SystemUIFont`/`.ZedMono`, siempre resolubles — ver rustdoc del
    /// módulo) y al tamaño/alto por defecto.
    #[test]
    fn fontset_defaults() {
        let f = FontSet::resolve(None, None, None);
        assert_eq!(f.ui.family.as_ref(), ".SystemUIFont");
        assert_eq!(f.mono.family.as_ref(), ".ZedMono");
        assert_eq!(f.size, gpui::px(14.0));
        assert_eq!(f.row_h, gpui::px(21.0));
    }

    /// Una familia de usuario ocupa `family`, pero lleva SIEMPRE la fuente
    /// empaquetada de fallback — así una familia mal tecleada degrada al
    /// look por defecto, no a una fuente arbitraria del sistema.
    #[test]
    fn fontset_familia_usuario_con_fallback() {
        let f = FontSet::resolve(None, Some("Nope Mono"), None);
        assert_eq!(f.mono.family.as_ref(), "Nope Mono");
        let fb = f.mono.fallbacks.as_ref().expect("fallback presente");
        assert_eq!(fb.fallback_list(), [".ZedMono".to_owned()]);
    }

    /// El piso de 18px evita filas ilegiblemente finas con tamaños de fuente
    /// muy pequeños (config válida solo desde 8.0, ver `norte-config`, pero
    /// el piso vive aquí porque `FontSet` no conoce ese rango).
    #[test]
    fn row_h_minimo() {
        let f = FontSet::resolve(None, None, Some(8.0));
        assert_eq!(f.row_h, gpui::px(18.0));
    }
}
