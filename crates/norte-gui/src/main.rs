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
//!   **Marcado con el ratón** (ctrl+click, shift+click, arrastre): las REGLAS
//!   no viven aquí, viven en [`norte_frontend::mouse`] — la misma máquina de
//!   gestos que consume la TUI (regla 7), para que los dos frontends no
//!   deriven a dos file managers distintos. Esta capa solo hace lo que es de
//!   GPUI: el hit test (que sale GRATIS, `on_mouse_move`/`on_mouse_up` de una
//!   fila solo disparan con el puntero sobre su hitbox — ver
//!   `HitboxId::is_hovered`, rev f14fea9) y aplicar los `Effect` sobre el
//!   modelo. El `on_mouse_up` de la RAÍZ cierra el gesto cuando se suelta
//!   fuera de toda fila: los listeners de la raíz se registran ANTES que los
//!   de las filas (`Interactivity::paint_mouse_listeners` corre antes de
//!   pintar los hijos) y la fase de burbuja los recorre en orden INVERSO
//!   (`Window::dispatch_mouse_event`), así que la fila siempre gana y la
//!   raíz solo ve los releases que ninguna fila consumió.
//!   **Menú contextual** (botón derecho, tarea 4 del plan de ratón):
//!   `on_mouse_down(MouseButton::Right)` por fila — el mismo hit test
//!   gratuito — y `MouseDownEvent.position` como ancla del panel. Qué
//!   entradas hay, sobre qué actúan y cuáles pueden correr lo decide
//!   [`context_menu`] (puro, testeable sin GPU); esta capa pinta y despacha
//!   por `run_command`, el MISMO camino que el teclado. El scrim del menú
//!   sí `occlude()` (a diferencia de los heredados): un click fuera lo
//!   cierra sin colarse a la fila de debajo. Una entrada deshabilitada
//!   registra un listener que sólo hace `cx.stop_propagation()`, para que
//!   pulsarla no cierre el menú por ese scrim.
//!   **Drag & drop entre panes** (tarea 5): soltar sobre el otro pane abre
//!   el MISMO modal de confirmación que `pane.copy`/`pane.move`
//!   ([`transfer_modal`], fuente única) — un drop es una mutación y no tiene
//!   una ruta más silenciosa. Qué filas viajan y si copia o mueve lo decide
//!   la máquina compartida; esta capa pinta el aviso de lo que haría soltar
//!   ahora ([`norte_frontend::mouse::Drag::pending`]) y lo refresca con
//!   `on_modifiers_changed` de la raíz, porque shift baja y sube sin que el
//!   puntero se mueva.
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
    Animation, AnimationExt, AnyElement, App, Bounds, BoxShadow, Context, FocusHandle, IntoElement,
    KeyDownEvent, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Render, RenderImage, ScrollDelta, ScrollStrategy, ScrollWheelEvent,
    SharedString, Styled, UniformListScrollHandle, Window, WindowBounds, WindowOptions, canvas,
    div, fill, hsla, img, linear_color_stop, linear_gradient, point, prelude::*, pulsating_between,
    px, rgb, rgba, size, uniform_list,
};
use gpui_platform::application;

use std::ops::Range;

use norte_config::ConfirmQuit;
use norte_frontend::availability::scheme_is_read_only;
use norte_frontend::mouse::{Drag, Effect, Mods, Pending, Press, Spot};
use norte_frontend::settings::PendingWrite;
use norte_frontend::{PaneState, nav::Mode};
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_theme::{FileKind, Role, Theme};

mod columns_view;
mod compare_view;
mod context_menu;
mod effects;
mod extensions_view;
mod help_render;
mod help_view;
mod keymap;
mod keys;
mod modal;
mod palette_view;
mod session;
mod settings_view;
mod shortcuts_view;
mod theme_map;

use context_menu::{ContextMenu, MenuOutcome};
use modal::{Modal, ModalOutcome, PendingOp, PendingTransfer, TransferKind};
use session::{LoadConfig, SessionCmd, SessionEvent};

/// Badge local que prefija un nombre alterado en el display (regla 1 / spec §6:
/// display siempre lossy y MARCADO). No hay `ui.rs` de la TUI aquí, así que la
/// GUI define su propio marcador; el criterio de «hostil» sí es el compartido
/// ([`norte_frontend::display_name`] devuelve el bool).
const HOSTILE_BADGE: &str = "⚠";

/// Tope de rutas por tanda de hidratación (#123): una pantalla visible más
/// su pre-carga cabe de sobra; el tope solo evita que un pane gigantesco
/// (o un rango raro) dispare cientos de `fs.stat` de golpe.
const STAT_BATCH_MAX: usize = 128;

/// Filas de chrome que le restamos al alto del viewport para derivar cuántas
/// filas de contenido pedirle a `Viewer::rows` en `render_viewer`: cabecera +
/// barra de estado (una fila cada una) + margen de redondeo.
const VIEWER_CHROME_ROWS: usize = 3;

/// Marcador de fila con marca (prefijo visible; el bool lo expone
/// `PaneState::is_marked`, la GUI solo lo pinta).
const MARK_MARKER: &str = "●";

/// Escala de espaciado (GP): los ÚNICOS valores en px permitidos para
/// paddings/gaps en código de render — ritmo en vez de improvisación. Un
/// `px(n)` con `n` fuera de esta escala en una llamada a `.px`/`.py`/`.p`/
/// `.gap`/`.mt` (etc.) en `render_*` es una regresión de este barrido, salvo
/// los acentos de 1px documentados en el sitio (más finos que `sp::XS`,
/// deliberadamente fuera de la escala).
mod sp {
    pub const XS: f32 = 2.0;
    pub const S: f32 = 4.0;
    pub const M: f32 = 8.0;
    pub const L: f32 = 12.0;

    /// Redondeo de fila (listado/franja de tasks) — GP review: nombra los dos
    /// `px(...)` de `.rounded(...)` que antes eran literales dispersos.
    pub const RADIUS_ROW: f32 = 3.0;
    /// Redondeo de panel flotante (modal) — más pronunciado que una fila.
    pub const RADIUS_PANEL: f32 = 6.0;
}

/// El *root view*: dos panes navegables, cuál tiene el foco, el tema cacheado y
/// el canal hacia el hilo de sesión persistente (para relistar en cada `cd`).
struct NorteGui {
    /// Los dos panes (modelo puro compartido con la TUI).
    panes: [PaneState; 2],
    /// El lector tiene agarrado el pulgar de la barra del cuerpo de la ayuda.
    ///
    /// Un `bool` y no las bounds del canal: la geometría se DERIVA (ver
    /// `help_body_track`), así que lo único que hay que recordar entre eventos
    /// es que el botón sigue abajo.
    help_dragging: bool,
    /// Config de columnas resuelta (#108 b4): hoy la GUI solo consume el
    /// SORT por scheme (las celdas llegan en el bloque 6). OJO: `columns`
    /// (a secas) son las columnas de PLUGIN por pane (G3c) — otra cosa.
    column_settings: norte_frontend::columns::ColumnsSettings,
    /// Catálogo de attrs por SCHEME (#117): lo puebla `SessionEvent::AttrCatalog`
    /// (una petición por scheme nuevo y sesión, decidida en `cd`/`refresh_dir`);
    /// alimenta hints y cabeceras del render. Sin entrada = defaults Opaque.
    attr_catalogs: std::collections::HashMap<String, norte_proto::AttrCatalog>,
    /// Orden elegido por click en la cabecera (#108 b6), POR PANE y de
    /// SESIÓN: sobrevive al cd (gana a `column_settings.sort_for`) pero no
    /// se persiste — persistir es del picker (bloque 7, `persist_set`).
    sort_override: [Option<norte_frontend::SortSpec>; 2],
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
    /// Rutas de las que YA se pidió un `fs.stat` de hidratación (#52/#123),
    /// por pane: el listado local llega LAZY y la GUI sondea las filas
    /// VISIBLES, así que sin esta dedup cada frame volvería a pedir lo
    /// mismo — y un stat FALLIDO (la ruta se queda sin `size`) se
    /// reintentaría en bucle. Se vacía con cada listado nuevo, que
    /// re-lazifica las entradas.
    probed: [std::collections::HashSet<VPath>; 2],
    /// Relist coalescido pendiente por pane (#84): un read-after-write que se
    /// saltó porque el pane YA cargaba ese dir se re-dispara al aterrizar la
    /// list en vuelo — así la list superviviente no puede preceder a escrituras
    /// posteriores del burst (correctitud) sin pagar N lists redundantes.
    relist_pending: [bool; 2],
    /// La list EN VUELO de este pane es un refresco (`relist_dirs`), no un
    /// `cd` (#103): al aterrizar se aplica con `refill`, que CONSERVA las
    /// marcas, en vez de `set_listing`, que las limpia por diseño.
    ///
    /// Invariante: solo lo escriben los dos únicos sitios que lanzan una list
    /// —`cd` (a `false`) y `refresh_dir` (a `true`)—, y ambos incrementan la
    /// generación, así que el flag SIEMPRE describe la petición más reciente.
    /// El aterrizaje lo consume (`mem::take`) DESPUÉS del guard de generación,
    /// para que un resultado stale no se lleve el flag de la petición viva.
    refreshing: [bool; 2],
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
    /// `Backend::is_journalled()` del backend conectado (#161): nace en
    /// `true` — el valor que la conexión real siempre confirma, porque esta
    /// GUI solo construye `Backend::Remote` — y `apply_event` lo REEMPLAZA
    /// (no lo combina) en cuanto llega `SessionEvent::Connected`, la única
    /// vez que llega, porque este canal jamás reconecta (ver `cmds` arriba).
    /// Alimenta `context_menu::facts_for`, que antes llevaba el mismo `true`
    /// como literal.
    journalled: bool,
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
    /// Plan de rename IA retenido (M4-IA, molde TUI `pending_ai_plan`): llegó
    /// con otro modal abierto (aprobación, colisión…) y espera su turno —
    /// jamás lo pisa. Se abre al cerrarse el modal activo, DESPUÉS de drenar
    /// `conflict_backlog` (los conflictos van primero, como en la TUI).
    ///
    /// Lleva el plan del LOTE (§17) que llegó en el mismo evento: sin él, el
    /// modal abriría sin `plan_hash` aprobado y confirmar quedaría mudo
    /// hasta un segundo viaje que nadie dispara.
    pending_ai_plan: Option<(
        VPath,
        Vec<norte_proto::methods::AiRenameEntry>,
        norte_frontend::BatchPlan,
    )>,
    /// Hits semánticos retenidos (M4-IA-2, molde `pending_ai_plan`): llegaron
    /// con otro modal abierto y esperan su turno — jamás lo pisan. Se abren
    /// al cerrarse el modal activo, DESPUÉS de `conflict_backlog` y de
    /// `pending_ai_plan` (mismo orden que la llegada de sus drenadores).
    /// Invariante anti-stale (lección TUI Task 8): mandar una NUEVA
    /// `SemanticSearch` lo limpia — unos hits viejos jamás aterrizan como si
    /// respondieran a la consulta nueva.
    pending_semantic: Option<Vec<norte_proto::methods::SemanticHit>>,
    /// Volúmenes retenidos (2026-08-10-volumes.md task V4, molde
    /// `pending_semantic`): llegaron con otro modal abierto y esperan su
    /// turno — jamás lo pisan. Se abren al cerrarse el modal activo, tras
    /// `conflict_backlog`/`pending_ai_plan`/`pending_semantic` (mismo orden
    /// de llegada de sus drenadores). Carga `pane`/`include_pseudo` junto a
    /// la lista: `Modal::Volumes` los necesita al abrir y una respuesta
    /// vieja no debe aterrizar con los de una petición más nueva.
    pending_volumes: Option<(usize, bool, Vec<norte_proto::methods::Volume>)>,
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
    /// The `Dialog`-screen effective (K3b): NOT a [`norte_frontend::keymap::Resolver`]
    /// like [`Self::resolver`]/[`Self::viewer_resolver`], because nothing
    /// dispatches through it — this GUI's overlays resolve fixed keys in code
    /// (`help_view::GuiChords::chord`'s rustdoc says why). Its one reader is
    /// [`help_view::keys_lines`], which needed the section to exist at all
    /// (K3b: "the GUI's sheet gains the dialog section it never had").
    /// Rebuilt everywhere [`Self::resolver`] is, from the same layers, so the
    /// three screens of the reference sheet can never drift out of sync with
    /// each other.
    dialog_effective: norte_frontend::keymap::Effective,
    /// The which-key panel's CACHED rows (K3a), or `None` while nothing is
    /// pending. Written ONLY by [`NorteGui::refresh_which_key`] /
    /// [`NorteGui::refresh_which_key_viewer`] (the resolver-transition arms
    /// of `on_key`) and by [`NorteGui::apply_keymap_live`] (a hot-reloaded
    /// keymap makes any cached rows describe a map that no longer exists).
    ///
    /// `render` reads this field but never builds one:
    /// [`norte_frontend::whichkey::WhichKeyRows::build`] costs several
    /// `String`s and one or two Fluent formats PER ROW (see its doc), and
    /// `render` runs at 60 Hz — building a twenty-row panel there would be
    /// exactly the `means_command` allocation pattern K2a deleted from this
    /// crate, rebuilt in a new place.
    ///
    /// This alone does not prove the panel is CURRENT: the resolver that owns
    /// the keyboard can change without going through either refresh method —
    /// a mouse double-click opens the viewer directly, no key involved. So
    /// `render` gates painting on the LIVE `active_resolver.pending()` used
    /// for the plain-text strip (`#91`) as well: a resolver switch away from
    /// the one these rows describe empties that slice at once, and a stale
    /// `Some` here simply does not get painted until the next transition
    /// refreshes or clears it.
    which_key: Option<norte_frontend::whichkey::WhichKeyRows>,
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
    /// Epoch del flicker (G2 decisión 3): fijado UNA vez en `new` con
    /// `Instant::now()` — NO el reloj de pared, así que reajustar la hora
    /// del sistema en caliente no salta la fase. `render` computa
    /// `motion_epoch.elapsed()` cada frame y lo pasa a [`flicker_factor`];
    /// nunca se reinicia durante la vida de la ventana (un flicker
    /// "reiniciado" en cada frame no oscilaría, `sin(0)` siempre).
    motion_epoch: std::time::Instant,
    /// `[ui] confirm_quit` (S2), resuelto UNA vez en `new` junto al resto de
    /// `[ui]`. `Default` = `Auto`, el comportamiento pre-S2 (confirma solo
    /// con trabajo pendiente). A diferencia del TUI (que hot-recarga
    /// `norte.toml` con un watcher), esta GUI no tenía NINGÚN camino de
    /// recarga hasta S4: `app.settings` es el ÚNICO sitio donde este campo
    /// (y `quick_mode`/`theme`/`effects`/`fonts`/`resolver`/`viewer_resolver`)
    /// puede cambiar en caliente ahora — ver
    /// `NorteGui::apply_settings_write_result`.
    confirm_quit: ConfirmQuit,
    /// `[ui] quick_search` (S2), resuelto UNA vez en `new` — GUI-c dejó este
    /// campo SIN homólogo (hardcodeaba `Mode::Filter` en `maybe_open_quick`,
    /// una brecha real: la TUI sí honra este ajuste desde S3). S4 lo cierra:
    /// `norte_frontend::config::FrontendConfig::quick_search_mode` YA lo
    /// resolvía desde `load` — solo faltaba leerlo aquí, mismo patrón que
    /// `confirm_quit`.
    quick_mode: Mode,
    /// Snapshot de los escalares MERGED de `norte.toml` (S4): construida UNA
    /// vez en `new` a partir de `loaded` (o vacía si la config no cargó,
    /// [`empty_frontend_config`]) y refrescada tras cada escritura de
    /// ajustes con OK (`apply_settings_write_result`). Las filas de
    /// `settings_view` (`norte_frontend::settings::build_rows`) se
    /// construyen SIEMPRE desde este campo, nunca releyendo disco al abrir
    /// la vista (F11) — eso sería I/O bloqueante en el hilo de UI (regla 2).
    cfg_snapshot: norte_frontend::config::FrontendConfig,
    /// La vista de ajustes abierta (`app.settings`, F11, S4), o `None` =
    /// cerrada. Mismo patrón de swap a pantalla completa que `viewer`
    /// (mutuamente excluyente con él y con el dual-pane — `on_key`/`render`
    /// la comprueban ANTES que `viewer`, mismo criterio de prioridad que el
    /// modal: si un `ViewerOpened` async aterriza mientras la vista de
    /// ajustes está abierta, la vista de ajustes sigue ganando la pantalla
    /// hasta que el usuario la cierre).
    settings_view: Option<settings_view::SettingsView>,
    /// The shortcut editor (K3c c4, `ctrl+k` from the settings view), or
    /// `None` = closed.
    ///
    /// It sits IN FRONT of `settings_view`, which stays open behind it and
    /// gets the screen back when this closes: the editor is a screen OF
    /// settings, not a replacement for it. `on_key` and `render` therefore
    /// both check it BEFORE `settings_view`.
    ///
    /// Unlike the TUI's twin this one has no file watcher behind it — this
    /// frontend watches nothing — so the write path itself
    /// (`apply_shortcut_write_result`) is what reloads the config, rebuilds
    /// the effectives and refreshes these rows. If that rebuild fails the
    /// OLD keymap stays and the editor says so; it never reports a key it
    /// did not change.
    shortcuts_view: Option<shortcuts_view::ShortcutsView>,
    /// Which `keymap.toml` write of this window is the newest
    /// (`NorteGui::next_shortcut_write`). Only that one's result may install
    /// effectives or speak; see that method for why two can be in flight at
    /// all when the TUI's equivalent cannot.
    shortcut_write_gen: u64,
    /// The command palette overlay (G3c, `ctrl+p`): `Some` while open,
    /// painted ON TOP of the dual-pane/viewer (an overlay, not a
    /// full-view swap — mirrors the modal's z-order, see `render`), but
    /// with the SAME key-capture priority as `settings_view` in `on_key`
    /// (modal still wins over everything, same as the TUI's
    /// `modal_preempts_palette` guard).
    palette: Option<palette_view::PaletteView>,
    /// The extension manager full view (G3c, `F12`): mutually exclusive
    /// with `settings_view`/`viewer`/dual-pane, same swap pattern as
    /// `settings_view`.
    extensions: Option<extensions_view::ExtensionsView>,
    /// Generación de la ÚLTIMA comparación pedida (guard anti-stale, mismo
    /// papel que `generation`/`viewer_gen`): cada `Compare` es un RPC en su
    /// propia task de tokio, así que dos peticiones seguidas pueden contestar
    /// en orden inverso y solo la vigente puede abrir panel.
    compare_gen: u64,
    /// El panel de diferencias abierto (#158, `pane.compare-dirs`), o `None`
    /// = cerrado.
    ///
    /// NO es un `PaneState`: es otro modelo pintado en el mismo sitio, igual
    /// que en la TUI. Lo abre `SessionEvent::CompareStarted` —no la tecla—
    /// porque hasta que hay Task no hay `task_id` con el que decidir de quién
    /// son las filas que lleguen.
    compare: Option<compare_view::CompareView>,
    /// Handle de scroll de la lista VIRTUALIZADA del panel de diferencias,
    /// gemelo de [`Self::scrolls`] y por la misma razón: tiene que persistir
    /// entre frames para que `scroll_to_item` (llamado tras mover el cursor)
    /// tenga efecto. Propio y no uno de los dos de los panes — el panel
    /// sustituye a los DOS, y compartir el handle dejaría el listado
    /// desplazado a donde estaba la comparación al cerrarla.
    compare_scroll: UniformListScrollHandle,
    /// The column picker overlay (#108 7c, `alt+c`): `Some` while open,
    /// same z-order and key-capture slot as the palette (modal wins).
    /// Esc discards; Enter applies in-session and persists (TUI parity).
    columns_picker: Option<columns_view::ColumnsView>,
    /// El catálogo vivo de plugins (`plugin.list`), cacheado: el picker de
    /// columnas ofrece las que declaran los aprobados y activados (#120) y
    /// llega asíncrono, así que se guarda al recibirlo en vez de re-pedirlo
    /// cada vez que se pinta.
    plugins: Vec<norte_proto::methods::PluginInfo>,
    /// The help overlay (H3f, `F1`): `Some` while open, same z-order and
    /// key-capture slot as the palette (modal still wins).
    help: Option<help_view::HelpView>,
    /// The resolver the open help page was painted through, FROZEN when the
    /// overlay opened ([`help_view::HelpView::freeze`]).
    ///
    /// Beside the view rather than inside it because Enter has to be answered
    /// by the verdict the reader can SEE: a row dimmed under the facts of the
    /// moment the page opened must not become runnable because a task finished
    /// underneath the overlay. It dies with the view.
    help_chords: Option<help_view::GuiChords>,
    /// Transient one-line notice `(message, is_error)` — today only the
    /// picker's save outcome (#108 7c; the TUI uses `app.message`, the
    /// settings status only renders inside its own view). Cleared on the
    /// NEXT keypress: honest without new chrome.
    flash: Option<(String, bool)>,
    /// `app.terminal` (#135) con un lanzamiento en vuelo. Sin esta guardia,
    /// mantener el chord pulsado a la tasa de repetición del teclado le pide
    /// al escritorio una ventana por evento (review de S4, MINOR-5).
    terminal_launching: bool,
    /// Gesto de ratón armado (ctrl/shift+click y arrastre de marcado): la
    /// máquina COMPARTIDA con la TUI ([`norte_frontend::mouse`]). Vive en el
    /// modelo y no en el árbol de render porque un gesto sobrevive a los
    /// frames que lo atraviesan — y porque el `render_row` que lo alimenta se
    /// reconstruye entero en cada uno.
    mouse: MouseState,
    /// El menú contextual abierto (botón derecho sobre una fila), o `None`.
    /// Overlay anclado al puntero: captura el teclado como la paleta (el
    /// modal sigue ganando), lo cierra un click fuera, y caduca solo si el
    /// listado se mueve bajo él (ver [`NorteGui::expire_stale_context_menu`]).
    context_menu: Option<ContextMenu>,
    /// Última respuesta de `SessionCmd::PluginConfigSummaries` (G3c),
    /// cacheada para que `set_settings_status` (que refresca las filas
    /// tras CUALQUIER escritura general, no solo un cambio de plugin)
    /// pueda reconstruir la sección Plugins sin perderla — sin este cache,
    /// editar `ui.theme` blanquearía la sección Plugins hasta el próximo
    /// `PluginConfigSummariesReady`.
    plugin_config_summaries: Vec<norte_frontend::settings::PluginConfigSummary>,
}

/// El gesto de puntero en curso. Un envoltorio de la máquina compartida
/// ([`norte_frontend::mouse::Drag`]) y nada más: aquí NO se decide qué marca
/// un arrastre ni cuándo un gesto es transferencia — eso lo decide
/// `norte-frontend` para los dos frontends a la vez (regla 7).
#[derive(Debug, Default)]
struct MouseState {
    drag: Drag,
    /// La [`MouseValidity`] del frame anterior, para detectar el cambio.
    validity: MouseValidity,
    /// Los modificadores VIVOS, para el aviso de lo que haría soltar ahora
    /// ([`Drag::pending`]). Se refrescan con cada evento de ratón y también
    /// con el `on_modifiers_changed` de la raíz: shift puede bajar o subir
    /// sin que el puntero se mueva ni un píxel, y el aviso tiene que
    /// cambiar de «copiar» a «mover» en ese mismo instante — la decisión se
    /// lee AL SOLTAR, así que un aviso rancio sería una promesa falsa.
    mods: Mods,
}

/// Todo lo que tiene que seguir siendo verdad para que un gesto en vuelo
/// signifique algo. Gemelo de la `Vigencia` de `norte-tui/src/mouse.rs`.
///
/// Un gesto solo lleva índices ([`Spot`]), y un índice nombra una fila del
/// listado que se pintó. Cuando ese listado se mueve —otro directorio, un
/// refill tras una mutación, una página de un relleno paginado, un
/// re-ordenado— el índice pasa a nombrar otro fichero, y el gesto ha dejado
/// de ser el que el usuario hizo. En la GUI eso llega ASÍNCRONO
/// (`SessionEvent::Listed`), a mitad de un arrastre y sin que nadie toque
/// nada.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct MouseValidity {
    /// `PaneState::listing_epoch` de cada pane.
    epochs: [u64; 2],
    /// El dual-pane no está a la vista (visor/ajustes/extensiones) o hay un
    /// overlay delante. Un modal que se abre a mitad de un arrastre se lleva
    /// el gesto por delante: cuando se cierre, el usuario ya está a otra
    /// cosa. Y sin filas pintadas tampoco hay dónde soltar.
    hidden: bool,
}

/// Qué dejó tras de sí una tanda de [`Effect`]s: lo único que el modelo puro
/// no puede hacer por sí mismo y que el caller (con `Context`) sí.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct MouseApplied {
    /// El pane cuyo cursor se movió, para que el caller le haga
    /// `follow_cursor` (la lista virtualizada de GPUI no sigue al cursor
    /// sola). `None` = ningún cursor cambió.
    moved_cursor: Option<usize>,
    /// El gesto soltó una transferencia entre panes (`None` = ninguna).
    transfer: Option<DropRequest>,
    /// La tanda no estaba vacía. Un `motion` que no cambia de fila no emite
    /// NADA (contrato de `Drag::motion`), y repintar por cada píxel que
    /// recorre el puntero sobre la misma fila sería el coste que ese
    /// contrato existe para evitar.
    changed: bool,
}

/// Un drop consumado: lo que [`Effect::Transfer`] pide, tal cual, sin
/// interpretarlo. Existe para que el cableado de GPUI (`on_mouse_up`) y la
/// apertura del modal sean dos pasos separables — uno testeable sin ventana.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DropRequest {
    /// Pane del que salen las entradas.
    from_pane: usize,
    /// Pane sobre el que se soltó.
    to_pane: usize,
    /// `true` = mover, `false` = copiar (modificadores AL SOLTAR).
    move_files: bool,
    /// `Some(idx)` = arrastre PROMOVIDO desde una fila sin marcar: viaja
    /// esa fila sola y las marcas del pane no se leen ni se tocan. `None` =
    /// las marcas del pane de origen (el arrastre de una selección).
    promoted: Option<usize>,
}

/// Los dos modificadores que el marcado entiende. `platform` (⌘) se deja
/// fuera a propósito: es asunto del keymap (ver [`keymap_mods`]), no de estos
/// gestos.
fn mouse_mods(m: Modifiers) -> Mods {
    Mods::new(m.control, m.shift)
}

/// Modificadores de GPUI → los del keymap NEUTRO, `platform` (⌘/Super)
/// incluido.
///
/// La GUI es el único frontend que puede OBSERVAR ⌘: gpui lo reporta en
/// `Modifiers::platform`. La TUI no puede (crossterm no entrega super sin
/// `PushKeyboardEnhancementFlags`, que norte no activa), y por eso `mod+` es
/// Ctrl allí en todas las plataformas. Aquí ⌘ viaja hasta el chord y el
/// keymap decide, en vez de morir en un early-return de `on_key`.
fn keymap_mods(m: Modifiers) -> norte_frontend::keymap::Mods {
    norte_frontend::keymap::Mods {
        ctrl: m.control,
        alt: m.alt,
        shift: m.shift,
        cmd: m.platform,
    }
}

/// El índice ABSOLUTO de la fila RESALTADA de un pane: el ancla desde la que
/// marca un shift+click.
///
/// Es lo que el usuario VE resaltado, no el cursor real: bajo un quick search
/// en modo filtro el resaltado sale de la selección del filtro y el cursor
/// real puede estar en cualquier parte del listado completo, así que tomarlo
/// a él como ancla marcaría un rango que empieza en una fila que nadie está
/// mirando. Gemelo de `painted_anchor` en `norte-tui/src/mouse.rs`.
fn painted_anchor(pane: &PaneState) -> usize {
    pane.quick()
        .and_then(norte_frontend::nav::QuickSearch::selected_entry_index)
        .unwrap_or_else(|| pane.cursor())
}

/// Aplica los efectos que devuelve la máquina compartida. Cada uno mapea
/// sobre UNA operación que ya existía en [`PaneState`]: esta capa no inventa
/// ninguna, y en particular no toca el quick search — marcar con el filtro
/// puesto es lo que hace que el marcado no alcance lo que el filtro esconde
/// (ver el rustdoc de `PaneState::mark_range`). Quien lo cierra es
/// [`NorteGui::on_row_click`], y solo para el click limpio.
fn apply_mouse_effects(
    panes: &mut [PaneState; 2],
    focus: &mut usize,
    effects: &[Effect],
) -> MouseApplied {
    let mut out = MouseApplied {
        changed: !effects.is_empty(),
        ..MouseApplied::default()
    };
    for effect in effects {
        match *effect {
            Effect::MoveCursor { pane, index } => {
                *focus = pane;
                panes[pane].set_cursor(index);
                out.moved_cursor = Some(pane);
            }
            Effect::SetMark {
                pane,
                index,
                marked,
            } => panes[pane].set_mark(index, marked),
            Effect::MarkRange { pane, from, to } => {
                panes[pane].mark_range(from, to);
            }
            Effect::BeginSweep { pane } => panes[pane].begin_sweep(),
            Effect::SweepRange { pane, from, to } => {
                panes[pane].apply_sweep(from, to);
            }
            // El barrido cruzó al otro pane y la máquina lo promovió a
            // transferencia: devuelve lo que llevara marcado. Una promoción
            // cambia lo que el gesto HACE, no lo que está seleccionado.
            Effect::RevertSweep { pane } => panes[pane].revert_sweep(),
            // El drop. Aquí NO se abre nada: el modelo puro no puede, y
            // sobre todo no debe decidirlo solo — el caller comprueba que no
            // haya nada delante y enruta por el MISMO modal que el teclado
            // (ver `NorteGui::on_mouse_release`).
            Effect::Transfer {
                from_pane,
                to_pane,
                move_files,
                promoted,
            } => {
                out.transfer = Some(DropRequest {
                    from_pane,
                    to_pane,
                    move_files,
                    promoted,
                });
            }
        }
    }
    out
}

/// Fija el OBJETIVO de un menú contextual sobre la fila `idx` de `pane` y lo
/// devuelve junto al tipo de esa entrada (`None` si el índice no nombra
/// ninguna: un listado que encogió entre el evento y esto).
///
/// Es la regla del menú, y muta el modelo a propósito: una fila pulsada que
/// NO está marcada se convierte en la selección entera del pane (las marcas
/// se sueltan), que es lo que hace cualquier file manager de escritorio y lo
/// que hace que «actúa sobre esta fila» sea VERDAD y no una promesa que la
/// primera copia rompería — `marked_paths` (la fuente única de sobre qué
/// opera cada comando) devuelve las marcas si las hay, así que dejarlas
/// puestas haría que el menú dijera una cosa y la op tocara otra. Una fila
/// pulsada que SÍ está marcada no toca nada: el objetivo son las marcas.
///
/// **El precio, asumido a sabiendas**: esa selección se DESCARTA y no hay
/// forma de recuperarla — ni Esc sobre el menú la devuelve (cuando el menú se
/// abre, ya se soltó). Se acepta porque el fallo que evita es peor y
/// silencioso (operar sobre once ficheros creyendo señalar uno) mientras que
/// este es visible al instante: las once filas se apagan a la vez que aparece
/// el menú, y su cabecera nombra la única que queda. Por eso la línea de
/// objetivo del panel no es decoración: es lo único que se interpone entre el
/// usuario y una operación sobre el conjunto equivocado, y no debe poder
/// perderse de vista (va la PRIMERA, con el fondo de cabecera, y jamás
/// comparte renglón con una entrada).
///
/// Pura respecto a GPUI (sólo `PaneState`), para poder clavar la regla sin
/// levantar ventana.
fn context_target(
    pane: &mut PaneState,
    idx: usize,
) -> Option<(context_menu::Target, norte_proto::EntryKind)> {
    let entry = pane.entries().get(idx).cloned()?;
    // El cursor va a la fila pulsada ANTES de decidir nada: sin marcas,
    // `marked_paths` cae en el cursor, así que «actúa sobre esta fila» sólo
    // es verdad si el cursor está EN ella.
    pane.set_cursor(idx);
    if !pane.is_marked(&entry) {
        pane.clear_marks();
    }
    let target = if pane.marks_len() > 1 {
        context_menu::Target::Marks(pane.marks_len())
    } else {
        let bytes = entry.path.file_name().map_or(&b""[..], Segment::as_bytes);
        context_menu::Target::Entry(row_label(bytes, entry.kind))
    };
    Some((target, entry.kind))
}

/// Cierra el menú si el listado de SU pane se movió bajo él. Ver
/// [`NorteGui::expire_stale_context_menu`], el único caller.
fn expire_stale_menu(menu: &mut Option<ContextMenu>, epochs: [u64; 2]) {
    let stale = menu
        .as_ref()
        .is_some_and(|m| epochs.get(m.pane).is_some_and(|e| m.is_stale(*e)));
    if stale {
        *menu = None;
    }
}

/// El modal de renombrado para `from`, o `None` si `from` no se puede
/// renombrar porque no tiene nombre ni padre (una raíz).
///
/// El destino es el PADRE de la propia entrada, no el `dir` del pane: son lo
/// mismo en el listado normal, pero no en un pane virtual (un listado de
/// resultados, donde el `dir` es la raíz del recorrido) — ahí tomar el del
/// pane renombraría MOVIENDO el fichero de sitio. Mismo criterio que la TUI
/// (`open_rename`), y por eso es una función aparte: es la parte que se
/// puede equivocar en silencio.
#[must_use]
fn rename_modal_for(from: &VPath) -> Option<Modal> {
    let to_dir = from.parent()?;
    // Los bytes REALES del nombre actual siembran el campo (regla 1): ni
    // decodificados ni pasados por lossy — ver [`Modal::RenamePrompt`].
    let name = from.file_name()?.as_bytes().to_vec();
    Some(Modal::RenamePrompt {
        from: from.clone(),
        to_dir,
        name,
        error: None,
    })
}

/// El texto que `pane.copy-path` deja en el portapapeles: una ruta por línea,
/// en forma WIRE. Ver [`NorteGui::copy_paths_to_clipboard`] para por qué wire
/// y no `display_lossy`.
#[must_use]
fn clipboard_text(paths: &[VPath]) -> String {
    paths
        .iter()
        .map(VPath::to_wire)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Suelta el gesto armado si su [`MouseValidity`] cambió desde el frame
/// anterior. Ver [`NorteGui::expire_stale_mouse_gesture`], el único caller.
fn expire_stale_gesture(mouse: &mut MouseState, validity: MouseValidity) {
    if validity != mouse.validity {
        mouse.drag.cancel();
    }
    mouse.validity = validity;
}

/// El modal de confirmación de una transferencia `from` → `to`, o `None` si
/// no hay nada que transferir (un pane vacío, un índice que ya no nombra
/// ninguna fila).
///
/// **Fuente ÚNICA de qué se somete en una copia o un movimiento**, la
/// tecla y el arrastre por igual. El plan lo exige por una razón que no es
/// de estilo: un drop es una mutación, y una segunda ruta —aunque hoy
/// naciera idéntica— se quedaría sin la confirmación, sin el modal de
/// colisión, sin la entrada de journal o sin el undo en cuanto una de las
/// dos cambiara. Por eso el drop no construye `Modal::ConfirmTransfer`: pide
/// el mismo que pediría `pane.copy`.
///
/// `promoted` es la única diferencia entre las dos entradas, y solo dice
/// SOBRE QUÉ actúa: `None` = las marcas del pane (`marked_paths`, la fuente
/// única de siempre); `Some(idx)` = esa fila sola, porque el gesto se
/// promovió desde una fila SIN marcar y las marcas del pane —si las hay—
/// son otra cosa que el usuario no está arrastrando.
fn transfer_modal(
    panes: &[PaneState; 2],
    from: usize,
    to: usize,
    kind: TransferKind,
    promoted: Option<usize>,
) -> Option<Modal> {
    let items: Vec<VPath> = match promoted {
        Some(idx) => panes[from]
            .entries()
            .get(idx)
            .map(|e| vec![e.path.clone()])
            .unwrap_or_default(),
        None => panes[from].marked_paths(),
    };
    if items.is_empty() {
        return None;
    }
    Some(Modal::ConfirmTransfer {
        kind,
        items,
        to: panes[to].dir().clone(),
    })
}

/// El modal que abre un drop: [`transfer_modal`] con lo que pidió el gesto.
fn drop_modal(panes: &[PaneState; 2], req: DropRequest) -> Option<Modal> {
    let kind = if req.move_files {
        TransferKind::Move
    } else {
        TransferKind::Copy
    };
    transfer_modal(panes, req.from_pane, req.to_pane, kind, req.promoted)
}

/// Cuántas entradas viajarían en `pending` y a qué dir, para el aviso que se
/// pinta ANTES de soltar. `None` = no hay drop pendiente (o no hay nada que
/// llevar), y entonces no se anuncia nada.
///
/// Sale de la MISMA lectura que [`drop_modal`] (marcas o la fila promovida),
/// así que el aviso no puede prometer un número distinto del que acabará en
/// el modal.
fn drop_hint(panes: &[PaneState; 2], pending: Option<Pending>) -> Option<(usize, String)> {
    let Some(Pending::Drop {
        from_pane,
        to_pane,
        move_files,
        promoted,
    }) = pending
    else {
        return None;
    };
    let n = match promoted {
        Some(idx) => usize::from(panes[from_pane].entries().get(idx).is_some()),
        None => panes[from_pane].marked_paths().len(),
    };
    if n == 0 {
        return None;
    }
    // El dir destino se pinta con el MISMO saneado que la cabecera del pane
    // (regla 1: display siempre lossy y marcado si es hostil).
    let (to_txt, hostile) = norte_frontend::path_display(panes[to_pane].dir());
    let to_txt = if hostile {
        format!("{HOSTILE_BADGE} {to_txt}")
    } else {
        to_txt
    };
    let key = if move_files { "drag-move" } else { "drag-copy" };
    Some((
        to_pane,
        norte_i18n::ta(key, &[("n", &n.to_string()), ("to", &to_txt)]),
    ))
}

/// Botón izquierdo abajo sobre una fila: alimenta la máquina compartida con
/// lo que solo el frontend sabe (si la fila estaba marcada, dónde está el
/// ancla visible) y aplica lo que devuelve.
fn mouse_press(
    mouse: &mut MouseState,
    panes: &mut [PaneState; 2],
    focus: &mut usize,
    at: Spot,
    mods: Mods,
) -> MouseApplied {
    mouse.mods = mods;
    let marked = panes[at.pane]
        .entries()
        .get(at.index)
        .is_some_and(|e| panes[at.pane].is_marked(e));
    let cursor = painted_anchor(&panes[at.pane]);
    let fx = mouse.drag.press(Press {
        at,
        marked,
        cursor,
        mods,
    });
    apply_mouse_effects(panes, focus, &fx)
}

/// El puntero pasó sobre `at` con el botón pulsado.
fn mouse_motion(
    mouse: &mut MouseState,
    panes: &mut [PaneState; 2],
    focus: &mut usize,
    at: Spot,
    mods: Mods,
) -> MouseApplied {
    mouse.mods = mods;
    let fx = mouse.drag.motion(at);
    apply_mouse_effects(panes, focus, &fx)
}

/// El botón subió. `at` es `None` cuando el release no cayó sobre ninguna
/// fila (cromo, franja de tasks, fuera de la ventana): el gesto se CANCELA
/// en vez de adivinar un destino.
fn mouse_release(
    mouse: &mut MouseState,
    panes: &mut [PaneState; 2],
    focus: &mut usize,
    at: Option<Spot>,
    mods: Mods,
) -> MouseApplied {
    mouse.mods = mods;
    let fx = mouse.drag.release(at, mods);
    let applied = apply_mouse_effects(panes, focus, &fx);
    // Suelta la baseline del barrido (una foto del conjunto de marcas): no
    // hace falta para la corrección —el siguiente `BeginSweep` la re-arma—,
    // solo evita que sobreviva al gesto.
    for pane in &mut *panes {
        pane.end_sweep();
    }
    applied
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
        inicio: &Startup,
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

        // Movimiento (G2 decisión 2, spec §17 a11y): `[ui] reduce_motion`
        // fijado UNA vez aquí, ANTES del primer `render`. `Context<Self>`
        // derefa a `App` (mismo precedente que `cx.text_system()` más abajo
        // para la tipografía), así que `set_reduce_motion` YA es alcanzable
        // en el constructor. GPUI mata TODO `with_animation` gratis a partir
        // de aquí (`App::reduce_motion`, `animation.rs`); el camino directo
        // de `render` (flicker) repite el chequeo como cinturón, per la doc
        // de `Window::request_animation_frame`. `unwrap_or(false)` cuando la
        // config no resolvió o la clave está ausente: GPUI no expone ninguna
        // pista de plataforma "prefiere menos movimiento" en este rev — "si
        // no, false" es la única rama disponible (documentado también en
        // `norte-config`).
        let reduce_motion = loaded
            .as_ref()
            .ok()
            .and_then(|cfg| cfg.common.ui_reduce_motion)
            .unwrap_or(false);
        cx.set_reduce_motion(reduce_motion);
        // #107: `[ui] show_hidden` siembra el estado INICIAL de ambos panes
        // (mismo contrato que la TUI; Ctrl+H lo cambia por pane después).
        let show_hidden = loaded
            .as_ref()
            .ok()
            .and_then(|cfg| cfg.common.ui_show_hidden)
            .unwrap_or(true);
        // #108 b4: columnas/orden resueltos del `[ui.columns]` cargado.
        let columns_settings = loaded
            .as_ref()
            .ok()
            .map(|cfg| norte_frontend::columns::ColumnsSettings::resolve(&cfg.common.ui_columns))
            .unwrap_or_default();

        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        let ((browse_eff, viewer_eff, dialog_eff), mut keymap_error) =
            match keymap::build_effectives3(&preset_name) {
                Ok(triple) => (triple, startup_banner),
                Err(e) => {
                    let msg = norte_i18n::ta(
                        "gui-banner-keymap-error",
                        &[("error", keymap_error_detail(&e).as_str())],
                    );
                    (
                        keymap::build_effectives3_preset_only(&preset_name),
                        Some(push_banner(startup_banner, msg)),
                    )
                }
            };
        // rust-reviewer MAJOR-3: `lua:` bindings coming from the PROJECT
        // layer (`./.norte`, content that arrives with a cloned repo) are
        // dropped for security — rebinding a common key to a command from
        // the USER's unsandboxed `init.lua` would be repo-directed execution.
        // Dropping is right; dropping in SILENCE is the bug K1 spent five
        // commits removing. The TUI has warned since M4; the GUI threw the
        // count away, so a user saw F5 behave normally and was never told a
        // binding from that repo had been refused.
        let discarded = browse_eff
            .discarded_lua_bindings()
            .max(viewer_eff.discarded_lua_bindings());
        if discarded > 0 {
            let msg = norte_i18n::ta(
                "msg-lua-keymap-project",
                &[("n", discarded.to_string().as_str())],
            );
            keymap_error = Some(push_banner(keymap_error, msg));
        }
        let resolver = norte_frontend::keymap::Resolver::new(browse_eff);
        let viewer_resolver = norte_frontend::keymap::Resolver::new(viewer_eff);

        // Tipografía (GP; GP review fix 2): resuelta UNA vez aquí, junto al
        // resto de la sesión. `Context<Self>` derefa a `App` (ver
        // `gpui::app::context::Context::deref`), así que el `TextSystem`
        // YA es alcanzable en este constructor — no hace falta mover la
        // resolución a otro sitio. Cada familia de `[ui]` se valida contra
        // el fontdb REAL (`all_font_names()`, que incluye "JetBrains Mono"
        // porque `main` ya la registró vía `add_fonts` antes de abrir la
        // ventana) antes de pasarla a `FontSet::resolve`; una familia
        // desconocida sustituye al default Y deja un aviso en el banner de
        // arranque (mismo acumulador que config/tema/keymap/effects).
        // Config inválida (rama `Err` de `loaded`) degrada a los defaults
        // sin intentar validar nada — igual que el resto del arranque
        // (nunca aborta por esto).
        let known_font_families = cx.text_system().all_font_names();
        let (ui_family, mono_family, font_size) = match loaded {
            Ok(cfg) => {
                let (ui, ui_warn) = validated_family(
                    cfg.common.ui_font.as_deref(),
                    &known_font_families,
                    ".SystemUIFont",
                );
                let (mono, mono_warn) = validated_family(
                    cfg.common.ui_mono_font.as_deref(),
                    &known_font_families,
                    "JetBrains Mono",
                );
                for warn in [ui_warn, mono_warn].into_iter().flatten() {
                    let msg = norte_i18n::ta(
                        "gui-banner-font-unknown",
                        &[("family", banner_safe(&warn).as_str())],
                    );
                    keymap_error = Some(push_banner(keymap_error, msg));
                }
                (ui, mono, cfg.common.ui_font_size)
            }
            Err(_) => (
                ".SystemUIFont".to_owned(),
                "JetBrains Mono".to_owned(),
                None,
            ),
        };
        let fonts = FontSet::resolve(&ui_family, &mono_family, font_size);

        // `[ui] confirm_quit` (S2): resuelto aquí junto al resto de `[ui]`,
        // mismo criterio "config inválida degrada al default" que el resto
        // del arranque — jamás aborta.
        let confirm_quit = loaded
            .as_ref()
            .map(|cfg| cfg.common.ui_confirm_quit)
            .unwrap_or_default();

        // Snapshot de config (S4): `loaded.clone()` si cargó, si no una
        // config VACÍA (nunca los defaults ad hoc de arriba — `build_rows`
        // necesita el `FrontendConfig` completo, no solo tema/fuente/preset
        // sueltos). Mismo criterio "jamás aborta" que el resto del arranque:
        // una `settings_view` sobre una config vacía sigue siendo útil (el
        // usuario puede FIJAR valores aunque la carga inicial fallara).
        let cfg_snapshot = match loaded {
            Ok(cfg) => cfg.clone(),
            Err(_) => empty_frontend_config(),
        };
        // `[ui] quick_search` (ver doc del campo): mismo patrón que
        // `confirm_quit`, pero derivado del snapshot en vez de `loaded`
        // directamente — `quick_search_mode` ya vive ahí.
        let quick_mode = cfg_snapshot.quick_search_mode;

        match LoadConfig::resolve(inicio.dir.clone(), inicio.socket.clone()) {
            Ok(cfg) => {
                let LoadConfig { socket, dir } = cfg;
                let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
                let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
                session::spawn(socket, cmd_rx, event_tx);

                let mut gui = Self {
                    help_dragging: false,
                    panes: [
                        PaneState::new(dir.clone(), Vec::new()),
                        PaneState::new(dir.clone(), Vec::new()),
                    ],
                    plugins: Vec::new(),
                    column_settings: norte_frontend::columns::ColumnsSettings::default(),
                    attr_catalogs: std::collections::HashMap::new(),
                    sort_override: [None, None],
                    focus: 0,
                    query: [String::new(), String::new()],
                    errors: [None, None],
                    generation: [0, 0],
                    probed: [
                        std::collections::HashSet::new(),
                        std::collections::HashSet::new(),
                    ],
                    relist_pending: [false, false],
                    refreshing: [false, false],
                    theme,
                    effects,
                    cmds: cmd_tx,
                    journalled: true,
                    focus_handle,
                    modal: None,
                    inflight: std::collections::HashMap::new(),
                    task_progress: std::collections::HashMap::new(),
                    conflict_backlog: Vec::new(),
                    pending_ai_plan: None,
                    pending_semantic: None,
                    pending_volumes: None,
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
                    dialog_effective: dialog_eff,
                    which_key: None,
                    viewer_gen: 0,
                    viewer_loading: false,
                    fonts,
                    motion_epoch: std::time::Instant::now(),
                    confirm_quit,
                    quick_mode,
                    cfg_snapshot,
                    settings_view: None,
                    shortcuts_view: None,
                    shortcut_write_gen: 0,
                    palette: None,
                    columns_picker: None,
                    help: None,
                    help_chords: None,
                    flash: None,
                    terminal_launching: false,
                    mouse: MouseState::default(),
                    context_menu: None,
                    extensions: None,
                    compare: None,
                    compare_gen: 0,
                    compare_scroll: UniformListScrollHandle::new(),
                    plugin_config_summaries: Vec::new(),
                };
                gui.column_settings = columns_settings;
                for pane in &mut gui.panes {
                    pane.set_show_hidden(show_hidden);
                }
                let spec = gui.column_settings.sort_for(dir.scheme());
                for pane in &mut gui.panes {
                    pane.set_sort(spec);
                }
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
                    help_dragging: false,
                    plugins: Vec::new(),
                    panes: [
                        PaneState::new(placeholder.clone(), Vec::new()),
                        PaneState::new(placeholder, Vec::new()),
                    ],
                    column_settings: norte_frontend::columns::ColumnsSettings::default(),
                    attr_catalogs: std::collections::HashMap::new(),
                    sort_override: [None, None],
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
                    probed: [
                        std::collections::HashSet::new(),
                        std::collections::HashSet::new(),
                    ],
                    relist_pending: [false, false],
                    refreshing: [false, false],
                    theme,
                    effects,
                    cmds: cmd_tx,
                    journalled: true,
                    focus_handle,
                    modal: None,
                    inflight: std::collections::HashMap::new(),
                    task_progress: std::collections::HashMap::new(),
                    conflict_backlog: Vec::new(),
                    pending_ai_plan: None,
                    pending_semantic: None,
                    pending_volumes: None,
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
                    dialog_effective: dialog_eff,
                    which_key: None,
                    viewer_gen: 0,
                    viewer_loading: false,
                    fonts,
                    motion_epoch: std::time::Instant::now(),
                    confirm_quit,
                    quick_mode,
                    cfg_snapshot,
                    settings_view: None,
                    shortcuts_view: None,
                    shortcut_write_gen: 0,
                    palette: None,
                    columns_picker: None,
                    help: None,
                    help_chords: None,
                    flash: None,
                    terminal_launching: false,
                    mouse: MouseState::default(),
                    context_menu: None,
                    extensions: None,
                    compare: None,
                    compare_gen: 0,
                    compare_scroll: UniformListScrollHandle::new(),
                    plugin_config_summaries: Vec::new(),
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
        // #103: esto es un `cd`, no un refresco — el listado que aterrice va
        // por `set_listing` (limpia las marcas), aunque `refresh_dir` hubiera
        // marcado el flag para una list anterior ya obsoleta.
        self.refreshing[pane] = false;
        // #108 b4: el orden del scheme destino se aplica ANTES de que
        // aterrice el listado (set_listing ingiere bajo el spec del pane).
        // #108 b6: el override de sesión (click en cabecera) gana a la config.
        let spec =
            self.sort_override[pane].unwrap_or_else(|| self.column_settings.sort_for(dir.scheme()));
        self.panes[pane].set_sort(spec);
        self.panes[pane].begin_loading(dir.clone());
        self.errors[pane] = None;
        self.query[pane].clear();
        // #117: los ids attr configurados del scheme destino viajan con la
        // list (sin pedirlos, las celdas attr pintan blanco); el catálogo se
        // pide solo si este scheme aún no está en caché (una vez por sesión).
        let attrs = self.column_settings.attr_ids_for(dir.scheme());
        let fetch_catalog = !self.attr_catalogs.contains_key(dir.scheme());
        let sent = self
            .cmds
            .send(SessionCmd::List {
                pane,
                generation,
                dir,
                attrs,
                fetch_catalog,
            })
            .is_ok();
        if !sent {
            // Revisión S, M2: el hilo de sesión murió (p. ej. `connect`
            // falló al arrancar) — ningún `SessionEvent::Listed` llegará
            // jamás para esta generación, así que `set_listing` (el ÚNICO
            // consumidor de `pending_focus`, ver su doc) tampoco se llama.
            // Sin este guard, un hint fijado por `nav.parent` justo antes de
            // este `cd` sobreviviría indefinidamente y podría aterrizar en
            // un `cd` futuro sin relación.
            self.panes[pane].clear_pending_focus();
        }
    }

    /// RE-lista el dir en el que `pane` YA está (read-after-write tras una
    /// mutación): manda el mismo `List` que un `cd`, pero SIN `begin_loading`
    /// — ese es el camino del `cd` y limpia las marcas por diseño (#103). El
    /// listado que aterrice se aplicará con `refill`, que las conserva y poda
    /// las que ya no existan (ver `apply_landed_listing`).
    ///
    /// Sube el flag de carga a mano porque `relist_dirs` lo usa para coalescer
    /// (#84): sin él, un burst de tasks terminales sobre el mismo dir mandaría
    /// una list por task en vez de coalescer a una.
    ///
    /// El caller debe haber comprobado que `dir` es el dir ACTUAL del pane.
    fn refresh_dir(&mut self, pane: usize, dir: VPath, _cx: &mut Context<Self>) {
        self.generation[pane] = self.generation[pane].wrapping_add(1);
        let generation = self.generation[pane];
        self.refreshing[pane] = true;
        self.panes[pane].set_loading(true);
        // #117: mismo par attrs/catálogo que `cd` — un refresco también debe
        // pedir los valores attr que el pane pinta.
        let attrs = self.column_settings.attr_ids_for(dir.scheme());
        let fetch_catalog = !self.attr_catalogs.contains_key(dir.scheme());
        let sent = self
            .cmds
            .send(SessionCmd::List {
                pane,
                generation,
                dir,
                attrs,
                fetch_catalog,
            })
            .is_ok();
        if !sent {
            // El hilo de sesión murió: ningún `Listed` llegará para esta
            // generación (mismo razonamiento que el guard de `cd`), así que
            // deshace el estado transitorio en vez de dejar el pane "cargando"
            // para siempre.
            self.refreshing[pane] = false;
            self.panes[pane].set_loading(false);
        }
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
                // #103: consumir el flag SOLO tras el guard de generación (ver
                // el campo) — este resultado es el de la petición viva.
                let refresh = std::mem::take(&mut self.refreshing[pane]);
                match outcome {
                    Ok((entries, skipped)) => {
                        // set_listing/refill ya normalizan (ordenan) internamente
                        // (#54); pre-ordenar aquí era un doble sort (#94).
                        let refilled = apply_landed_listing(
                            &mut self.panes[pane],
                            dir.clone(),
                            entries,
                            refresh,
                        );
                        // #96: badge de omitidas del contenedor (#93) — un
                        // listado incompleto jamás es silencioso, tampoco
                        // en la GUI.
                        self.panes[pane].set_skipped(skipped);
                        // #123: listado nuevo = entradas otra vez lazy, así
                        // que la dedup de la hidratación caduca entera.
                        self.probed[pane].clear();
                        self.errors[pane] = None;
                        if !refilled {
                            // Solo el camino del `cd` mata el quick search vivo
                            // (`set_listing`); `refill` lo RE-APLICA, así que
                            // limpiar aquí su espejo dejaría la línea `/{query}`
                            // del pie desincronizada del filtro real (#103).
                            self.query[pane].clear();
                        }
                        // G3b (ADR 0037): pide decoraciones de plugin para la
                        // página VISIBLE recién aterrizada — asíncrono, NUNCA
                        // bloquea el listado; llega tarde por
                        // `SessionEvent::Decorated` y solo pinta si el pane
                        // sigue en el MISMO dir/generación (guard abajo). Sin
                        // guardia de "algún decorator activo": intentar cada
                        // listado es barato (una RPC) y más honesto que un
                        // flag cacheado que un F12 dejaría obsoleto.
                        let paths: Vec<VPath> = self.panes[pane]
                            .entries()
                            .iter()
                            .map(|e| e.path.clone())
                            .collect();
                        let _ = self.cmds.send(SessionCmd::Decorate {
                            pane,
                            generation,
                            dir: dir.clone(),
                            paths: paths.clone(),
                        });
                        // #117-follow-up (antes G3c incondicional): SOLO las
                        // columnas plugin: CONFIGURADAS del scheme, mismo
                        // criterio que Decorate sobre la MISMA página.
                        let requested = self
                            .column_settings
                            .plugin_ids_for(self.panes[pane].dir().scheme());
                        let _ = self.cmds.send(SessionCmd::Columns {
                            pane,
                            generation,
                            dir,
                            paths,
                            requested,
                        });
                        // H3f review (rust MAJOR 4): a listing landing UNDER the
                        // open help re-freezes its facts. The freeze buys "no
                        // verdict changes because the reader moved" and nothing
                        // more: a copy finishing re-lists both panes, and
                        // without this the page would keep judging `nav.enter`
                        // and friends against an entry that no longer exists —
                        // and dispatch against it on Enter.
                        self.refreeze_help_facts();
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
                    self.refresh_dir(pane, cur, cx);
                }
            }
            SessionEvent::AttrCatalog { scheme, catalog } => {
                // #117: sin guard de generación — el catálogo es por scheme,
                // no por cd, y un catálogo "tardío" sigue siendo el correcto.
                self.attr_catalogs.insert(scheme, catalog);
                cx.notify();
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
                        lossy,
                    } => Viewer::with_plugin_preview(path, plugin_name, &output, lossy),
                    ViewerContent::PluginStyled {
                        plugin_name,
                        lines,
                        lossy,
                    } => Viewer::with_plugin_preview_styled(path, plugin_name, &lines, lossy),
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
            SessionEvent::Decorated {
                pane,
                generation,
                dir,
                decorations,
            } => {
                // Doble guard anti-stale (G3b): la generación Y el dir vigente
                // del pane deben casar — un cd MÁS NUEVO ya pudo aterrizar
                // (generación) o, más sutil, un `Decorate` en vuelo puede
                // resolver DESPUÉS de que el pane vuelva a este MISMO dir tras
                // pasar por otro (dir casa pero la generación vieja no debe
                // pisar el listado actual igualmente) — ambos deben casar.
                if !generation_is_current(self.generation[pane], generation)
                    || self.panes[pane].dir() != &dir
                {
                    return;
                }
                self.panes[pane].set_decorations(decorations);
            }
            SessionEvent::Hydrated {
                pane,
                generation,
                dir,
                entries,
            } => {
                // Mismo guard doble anti-stale que `Decorated`: un cd más
                // nuevo (generación) o una vuelta al MISMO dir con otra
                // generación no deben recibir stats del listado viejo.
                if !generation_is_current(self.generation[pane], generation)
                    || self.panes[pane].dir() != &dir
                {
                    return;
                }
                for (path, size, mtime_ms) in entries {
                    self.panes[pane].hydrate(&path, size, mtime_ms);
                }
                cx.notify();
            }
            SessionEvent::Connected { journalled } => {
                // Sin `cx.notify()` a propósito: hoy `journalled` nace en
                // `true` y esto SIEMPRE llega con `true` (la GUI solo
                // construye `Backend::Remote`, ver doc del campo), así que
                // esta escritura nunca cambia lo que ya se pintó. Si algún
                // día esta GUI alcanza `Backend::Embedded`, ESTE arm
                // necesitará repintar lo que ya esté en pantalla
                // (menú/ayuda abiertos) para no dejarlo desfasado.
                self.journalled = journalled;
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
            SessionEvent::ColumnsReady {
                pane,
                generation,
                dir,
                values,
            } => {
                // Mismo guard doble anti-stale que `Decorated`.
                if !generation_is_current(self.generation[pane], generation)
                    || self.panes[pane].dir() != &dir
                {
                    return;
                }
                // #117-follow-up: al side-map compartido del pane — el
                // funnel pinta desde ahí (paridad TUI).
                self.panes[pane].set_plugin_columns(values);
            }
            SessionEvent::PluginsListed(res) => match res {
                Ok((plugins, errors)) => {
                    if let Some(palette) = &mut self.palette {
                        palette.extend(norte_frontend::palette::plugin_rows(&plugins));
                    }
                    // #120: el picker de columnas ofrece las columnas que los
                    // plugins declaran, y el catálogo llega ASÍNCRONO. Se
                    // cachea, y si el picker ya está abierto se reconstruye
                    // con él — abrirlo y ver aparecer las filas un instante
                    // después es mejor que no verlas nunca.
                    self.plugins = plugins.clone();
                    if self.columns_picker.is_some() {
                        self.open_columns_picker();
                    }
                    // H3f: the help's Extensions group and its `plugin:` rows
                    // come from this same catalogue.
                    self.install_help_plugins(&plugins);
                    if let Some(ext) = &mut self.extensions {
                        ext.plugins = plugins;
                        ext.errors = errors;
                        ext.loading = false;
                    }
                }
                Err(_) => {
                    // Best-effort (mismo criterio que Decorate/Columns): la
                    // paleta se queda con solo los comandos built-in; el
                    // gestor de extensiones sale de "cargando" a "vacío" —
                    // indistinguible de un catálogo genuinamente vacío, pero
                    // nunca un banner de error que tumbe la vista.
                    if let Some(ext) = &mut self.extensions {
                        ext.loading = false;
                    }
                }
            },
            SessionEvent::PluginConfigReady { id, rows } => {
                if let Some(ext) = &mut self.extensions {
                    if rows.is_empty() {
                        self.errors[self.focus] = Some(norte_i18n::t("msg-plugin-config-empty"));
                        return;
                    }
                    let raw_name = ext
                        .plugins
                        .iter()
                        .find(|p| p.id == id)
                        .map(|p| p.name.clone())
                        .unwrap_or_default();
                    let plugin_name = norte_frontend::display_name(raw_name.as_bytes()).0;
                    ext.config = Some(extensions_view::ConfigPanel {
                        plugin_id: id,
                        plugin_name,
                        state: norte_frontend::plugin_config::PluginConfigState::new(rows),
                    });
                }
            }
            SessionEvent::PluginConfigFailed(msg) => {
                self.errors[self.focus] = Some(msg);
            }
            SessionEvent::PluginHelpReady { id, result } => {
                // Late answers are harmless: the reader may have closed the
                // overlay, or moved to another page, while it was in flight —
                // installing a page nobody is looking at costs a parse.
                if let Some(view) = &mut self.help {
                    view.install_plugin_page(&id, &result);
                }
            }
            SessionEvent::PluginConfigSummariesReady(summaries) => {
                self.plugin_config_summaries = summaries;
                if let Some(view) = &mut self.settings_view {
                    view.state.refresh(norte_frontend::settings::build_rows(
                        &self.cfg_snapshot,
                        &self.plugin_config_summaries,
                    ));
                }
            }
            SessionEvent::PluginConfigSaved { key, value } => {
                self.errors[self.focus] = Some(norte_i18n::ta(
                    "msg-plugin-config-saved",
                    &[("key", key.as_str()), ("value", value.as_str())],
                ));
            }
            SessionEvent::PluginConfigSaveFailed(msg) => {
                self.errors[self.focus] = Some(msg);
            }
            SessionEvent::PluginGovernanceSet {
                id,
                approved,
                enabled,
            } => {
                if let Some(ext) = &mut self.extensions {
                    if let Some(approved) = approved {
                        ext.set_local_approved(&id, approved);
                    }
                    if let Some(enabled) = enabled {
                        ext.set_local_enabled(&id, enabled);
                    }
                }
            }
            SessionEvent::PluginGovernanceFailed(msg) => {
                self.errors[self.focus] = Some(msg);
            }
            SessionEvent::PluginRunResult(output) => {
                self.errors[self.focus] = Some(norte_i18n::ta(
                    "msg-plugin-run-ok",
                    &[("output", banner_safe(&output).as_str())],
                ));
            }
            SessionEvent::PluginRunFailed(msg) => {
                self.errors[self.focus] = Some(msg);
            }
            // deuda (#121): el banner llega al pane ENFOCADO, no al solicitante
            // (igual que PluginRunResult) — un `pane.switch` durante el
            // "pensando…" deja el aviso en el pane equivocado.
            SessionEvent::AiRenamePlan { dir, result, plan } => match result {
                Ok(entries) if entries.is_empty() => {
                    self.errors[self.focus] = Some(norte_i18n::t("msg-ai-rename-empty"));
                }
                // Cinturón de INGESTIÓN (quality review 78eb243 MINOR-5): un
                // plan legítimo del engine queda muy por debajo del tope;
                // superarlo delata un daemon hostil/N+1 inflando la
                // respuesta — rechazo en bloque, ni se abre el modal.
                Ok(entries) if entries.len() > norte_frontend::MAX_AI_PLAN_ENTRIES => {
                    self.errors[self.focus] = Some(norte_i18n::t("msg-ai-rename-invalid-plan"));
                }
                Ok(entries) => {
                    // Retira el "pensando…" del banner: el plan ES la
                    // respuesta. Si el plan del LOTE (§17) falló, el banner
                    // lo dice — el modal abre igual, con las parejas
                    // visibles y confirmar deshabilitado: sin `plan_hash`
                    // aprobado no hay nada honesto que mandar.
                    let plan = match plan {
                        Some(Ok(p)) => {
                            self.errors[self.focus] = None;
                            norte_frontend::BatchPlan::Ready(Box::new(p))
                        }
                        Some(Err(msg)) => {
                            self.errors[self.focus] = Some(norte_i18n::ta(
                                "msg-rename-batch-plan-failed",
                                &[("error", banner_safe(&msg).as_str())],
                            ));
                            norte_frontend::BatchPlan::Failed
                        }
                        // No se intentó. Los otros dos motivos (plan vacío,
                        // plan desbordado) ya los contestaron los arms de
                        // arriba, así que aquí solo queda el tercero: una
                        // pareja que no es un `Segment`. Es el MISMO
                        // veredicto que dará el cinturón de `on_key` al
                        // confirmar — decirlo ya evita dejar el "pensando…"
                        // colgado en el banner.
                        None => {
                            self.errors[self.focus] =
                                Some(norte_i18n::t("msg-ai-rename-invalid-plan"));
                            norte_frontend::BatchPlan::Failed
                        }
                    };
                    if self.modal.is_none() {
                        self.open_modal(Modal::AiRenamePlan {
                            dir,
                            entries,
                            offset: 0,
                            plan,
                        });
                    } else {
                        // Otro modal abierto (colisión…): el plan espera su
                        // turno, jamás pisa al modal activo (molde TUI
                        // `pending_ai_plan`). Si YA había un plan retenido,
                        // gana el más NUEVO y la pérdida se DICE (quality
                        // review 78eb243 MINOR-3: jamás un descarte mudo).
                        if self.pending_ai_plan.replace((dir, entries, plan)).is_some() {
                            self.errors[self.focus] =
                                Some(norte_i18n::t("gui-msg-ai-rename-superseded"));
                        }
                    }
                }
                Err(msg) => {
                    // `msg` es el `Display` categórico del error proto (lo
                    // aplanó la sesión); `banner_safe` es cinturón, como en
                    // `msg-plugin-run-ok`.
                    self.errors[self.focus] = Some(norte_i18n::ta(
                        "msg-ai-rename-failed",
                        &[("error", banner_safe(&msg).as_str())],
                    ));
                }
            },
            // Espejo del arm de arriba (M4-IA-2); mismo caveat #121 (el
            // banner va al pane ENFOCADO, no al solicitante).
            SessionEvent::SemanticHits { result } => match result {
                Ok(hits) if hits.is_empty() => {
                    self.errors[self.focus] = Some(norte_i18n::t("msg-semantic-empty"));
                }
                // Cinturón de INGESTIÓN compartido con la TUI
                // (`norte_frontend::validate_semantic_hits`): superar el
                // techo contractual del server o colar un score no finito
                // delata un daemon hostil/N+1 — rechazo en bloque, ni se
                // abre el modal.
                Ok(hits) => match norte_frontend::validate_semantic_hits(hits) {
                    None => {
                        self.errors[self.focus] = Some(norte_i18n::t("msg-semantic-invalid"));
                    }
                    Some(hits) => {
                        // Retira el "pensando…" del banner: los hits SON la
                        // respuesta.
                        self.errors[self.focus] = None;
                        if self.modal.is_none() {
                            self.open_modal(Modal::SemanticHits {
                                hits,
                                offset: 0,
                                cursor: 0,
                            });
                        } else {
                            // Otro modal abierto: los hits esperan su turno,
                            // jamás pisan al modal activo. Si YA había hits
                            // retenidos, ganan los NUEVOS y la pérdida se
                            // DICE (molde ai-rename, quality review 78eb243
                            // MINOR-3: jamás un descarte mudo).
                            if self.pending_semantic.replace(hits).is_some() {
                                self.errors[self.focus] =
                                    Some(norte_i18n::t("gui-msg-semantic-superseded"));
                            }
                        }
                    }
                },
                Err(msg) => {
                    self.errors[self.focus] = Some(norte_i18n::ta(
                        "msg-semantic-failed",
                        &[("error", banner_safe(&msg).as_str())],
                    ));
                }
            },
            // 2026-08-10-volumes.md task V4, molde `SemanticHits` above —
            // minus the empty-result special case: an empty volume list is
            // still a valid answer to show (paridad TUI `App::
            // open_volumes_popup`, whose picker paints `volumes-empty`
            // INSIDE itself rather than refusing to open), not a "nothing
            // found" banner the way zero semantic hits is.
            SessionEvent::VolumesReady {
                pane,
                include_pseudo,
                result,
            } => match result {
                Ok(volumes) => {
                    // Retira el "cargando…" del banner: los volúmenes SON la
                    // respuesta (posiblemente vacía).
                    self.errors[self.focus] = None;
                    if self.modal.is_none() {
                        self.open_modal(Modal::Volumes {
                            pane,
                            include_pseudo,
                            volumes,
                            offset: 0,
                            cursor: 0,
                        });
                    } else {
                        // Otro modal abierto: espera su turno, jamás lo
                        // pisa. Si YA había una lista retenida, gana la
                        // NUEVA y la pérdida se DICE (molde semantic hits).
                        if self
                            .pending_volumes
                            .replace((pane, include_pseudo, volumes))
                            .is_some()
                        {
                            self.errors[self.focus] =
                                Some(norte_i18n::t("gui-msg-volumes-superseded"));
                        }
                    }
                }
                Err(msg) => {
                    self.errors[self.focus] = Some(norte_i18n::ta(
                        "gui-msg-volumes-failed",
                        &[("error", banner_safe(&msg).as_str())],
                    ));
                }
            },
            // #158, spec 3 fase C1. El panel se abre AQUÍ, con la Task ya
            // creada: es el `task_id` lo que después distingue sus lotes de
            // los de una comparación anterior que el lector cancelara.
            SessionEvent::CompareStarted {
                task_id,
                left_pane,
                generation,
                left_root,
                right_root,
            } => {
                // GUARD de generación, el mismo que `List`/`OpenViewer`: cada
                // `Compare` es un RPC propio en su `tokio::spawn`, así que dos
                // teclas seguidas pueden contestar en orden INVERSO. Sin esto
                // el arranque VIEJO llegaba el último, cancelaba el panel
                // nuevo y dejaba abierto el de las raíces anteriores — o sea,
                // la petición más reciente del lector es la que moría
                // (revisión MAJOR-2).
                if !generation_is_current(self.compare_gen, generation) {
                    let _ = self.cmds.send(SessionCmd::Cancel(task_id));
                    // Y se retira el «comparando…» que ESTA petición dejó
                    // puesto: nadie más lo va a quitar, porque la petición que
                    // la superó limpia el SUYO, en el pane que ella lanzó — y
                    // si el foco cambió entre las dos teclas, ese no es este
                    // (revisión de rama, MINOR-3). Con las dos en el mismo
                    // pane esto se adelanta un instante al aviso de la nueva,
                    // que llega con su propio `CompareStarted`; un banner que
                    // parpadea es mejor que uno que se queda para siempre.
                    self.errors[left_pane & 1] = None;
                    return;
                }
                // El índice cruza un canal: acotarlo es más barato que el
                // panic que evita (revisión MINOR-2).
                let left_pane = left_pane & 1;
                // Retira el «comparando…» que puso `start_compare`: el panel
                // ES la respuesta a partir de aquí. En el pane que LANZÓ, que
                // es donde se puso — el foco pudo moverse mientras el RPC iba
                // y venía, y un aviso que se queda pegado para siempre es
                // peor que no haberlo puesto.
                self.errors[left_pane] = None;
                // El visor no puede quedarse vivo detrás (revisión rust
                // BLOCKER-1): las dos pantallas sustituyen a los panes
                // enteros, así que con ambas abiertas una se pinta y la otra
                // se queda el teclado. La otra mitad de la exclusión está en
                // `open_viewer`.
                self.close_viewer();
                // El teclado acaba de cambiar de dueño SIN que se pulsara
                // ninguna tecla, así que un prefijo (o un contador) a medio
                // teclear en el resolver de Browse se queda huérfano: ni este
                // panel lo puede continuar ni cerrarlo lo cancela, y la
                // siguiente tecla de vuelta al dual-pane lo REANUDARÍA
                // (revisión rust MAJOR-4). Mismo caso que la K3a documenta
                // para el modal y la ayuda.
                self.resolver.reset();
                self.which_key = None;
                // Y la lista arranca arriba: el handle sobrevive al panel
                // anterior, y heredar su desplazamiento deja la comparación
                // nueva mirando a una altura que nadie pidió, con el cursor
                // en la fila 0 (revisión rust MINOR-3).
                self.compare_scroll
                    .scroll_to_item(0, ScrollStrategy::Nearest);
                // Regla 3: la comparación a la que sustituye se cancela — dos
                // flujos alimentando un panel serían dos comparaciones a la
                // vez (mismo criterio que `launch_compare` en la TUI).
                if let Some(superseded) = compare_view::open(
                    &mut self.compare,
                    compare_view::Started {
                        task_id,
                        left_root,
                        right_root,
                        left_pane,
                        left_encoding: self.panes[left_pane].name_encoding(),
                        right_encoding: self.panes[left_pane ^ 1].name_encoding(),
                    },
                ) {
                    let _ = self.cmds.send(SessionCmd::Cancel(superseded));
                }
            }
            SessionEvent::CompareRows { task_id, rows } => {
                // No cancela nada: cancelar es de quien SUELTA el panel, y
                // los dos caminos que lo sueltan ya lo hacen (ver
                // `compare_view::route_rows`).
                compare_view::route_rows(&mut self.compare, task_id, rows);
            }
            SessionEvent::CompareDone {
                task_id,
                state,
                entries_done,
            } => {
                if let Some(view) = self.compare.as_mut()
                    && view.on_done(task_id, &state, entries_done)
                    && let Some(error) = view.run.error.clone()
                {
                    // El fallo se DICE una vez en el banner del pane que
                    // lanzó, además de quedarse pintado de forma persistente
                    // en el panel (tarea 3). Por `banner_safe`, como todo lo
                    // que entra en un banner y como hace la TUI con
                    // `detail_for_bar` (revisión de rama, MINOR-6): la
                    // categoría no lleva datos del peer, pero el enmascarado
                    // y el tope son del BANNER, no de su contenido.
                    let pane = view.run.left_pane & 1;
                    self.errors[pane] = Some(norte_i18n::ta(
                        "compare-status-failed",
                        &[("error", banner_safe(&error).as_str())],
                    ));
                }
            }
            // Rechazada antes de existir Task: la frase, y NINGÚN panel (uno
            // vacío que dice «fallo» es peor, porque además hay que cerrarlo).
            SessionEvent::CompareFailed {
                left_pane,
                generation,
                error,
            } => {
                // El MISMO guard que `CompareStarted`: la negativa de una
                // petición ya superada describe algo que el lector reemplazó,
                // así que solo retira su propio «comparando…» y se calla
                // (revisión de rama, MINOR-3).
                let pane = left_pane & 1;
                if !generation_is_current(self.compare_gen, generation) {
                    self.errors[pane] = None;
                    return;
                }
                self.errors[pane] = Some(norte_i18n::ta(
                    "compare-status-failed",
                    &[(
                        "error",
                        banner_safe(&norte_frontend::error::error_category(&error)).as_str(),
                    )],
                ));
            }
        }
    }

    /// Abre el modal de copia/movimiento: origen = marcas/cursor del pane
    /// activo, destino = dir del pane inactivo. No-op si no hay nada que mover.
    ///
    /// El camino del TECLADO (`pane.copy`/`pane.move`); el del arrastre entra
    /// por [`Self::drop_transfer`]. Los dos pasan por [`transfer_modal`], que
    /// es lo que garantiza que un drop someta exactamente lo mismo.
    fn open_transfer_modal(&mut self, kind: TransferKind) {
        let f = self.focus;
        if let Some(modal) = transfer_modal(&self.panes, f, 1 - f, kind, None) {
            self.open_modal(modal);
        }
    }

    /// Un drop consumado sobre el otro pane: abre el MISMO modal de
    /// confirmación que la tecla de copiar o mover (ver [`transfer_modal`]).
    ///
    /// Con cualquier overlay delante no abre nada (mismo criterio, overlay o
    /// menú contextual, que caduca los gestos). En la práctica no llega — un
    /// modal ya caducó el gesto en `render`, ver
    /// [`Self::expire_stale_mouse_gesture`] — pero el guard es barato y lo
    /// que evita no lo es: reemplazar el modal abierto por este perdería la
    /// decisión que el usuario tenía delante — y si el que estaba abierto era
    /// un `ConflictResolve`, perdería además la transferencia que lo abrió.
    fn drop_transfer(&mut self, req: DropRequest) {
        if self.overlay_in_front() || self.context_menu.is_some() {
            return;
        }
        if let Some(modal) = drop_modal(&self.panes, req) {
            self.open_modal(modal);
        }
    }

    /// Abre el modal de borrado sobre las marcas/cursor del pane activo.
    fn open_delete_modal(&mut self) {
        let f = self.focus;
        let items = self.panes[f].marked_paths();
        if items.is_empty() {
            return;
        }
        self.open_modal(Modal::ConfirmDelete {
            items,
            permanent: false,
        });
    }

    /// Abre el modal de conflicto, o lo encola si ya hay un modal abierto.
    fn queue_conflict(&mut self, pending: PendingTransfer, conflict: norte_proto::ConflictKind) {
        if self.modal.is_some() {
            self.conflict_backlog.push((pending, conflict));
        } else {
            self.open_modal(Modal::ConflictResolve { pending, conflict });
        }
    }

    /// Al cerrar un modal, abre el siguiente conflicto encolado (si hay).
    fn open_next_conflict(&mut self) {
        if self.modal.is_some() {
            return;
        }
        if let Some((pending, conflict)) = self.conflict_backlog.pop() {
            self.open_modal(Modal::ConflictResolve { pending, conflict });
        }
    }

    /// Al cerrar un modal, abre el plan IA retenido (M4-IA, molde TUI) si
    /// ningún otro modal ganó el turno — `open_next_conflict` se llama ANTES
    /// que esto en todos los cierres (los conflictos drenan primero).
    fn open_pending_ai_plan(&mut self) {
        if self.modal.is_some() {
            return;
        }
        if let Some((dir, entries, plan)) = self.pending_ai_plan.take() {
            self.open_modal(Modal::AiRenamePlan {
                dir,
                entries,
                offset: 0,
                plan,
            });
        }
    }

    /// Al cerrar un modal, abre los hits semánticos retenidos (M4-IA-2) si
    /// ningún otro modal ganó el turno — se llama SIEMPRE después de
    /// `open_next_conflict` y `open_pending_ai_plan` (conflictos primero,
    /// luego el plan IA, luego esto).
    fn open_pending_semantic(&mut self) {
        if self.modal.is_some() {
            return;
        }
        if let Some(hits) = self.pending_semantic.take() {
            self.open_modal(Modal::SemanticHits {
                hits,
                offset: 0,
                cursor: 0,
            });
        }
    }

    /// Al cerrar un modal, abre los volúmenes retenidos (2026-08-10-
    /// volumes.md task V4, molde `open_pending_semantic`) si ningún otro
    /// modal ganó el turno — se llama SIEMPRE último (conflictos, plan IA,
    /// hits semánticos, luego esto).
    fn open_pending_volumes(&mut self) {
        if self.modal.is_some() {
            return;
        }
        if let Some((pane, include_pseudo, volumes)) = self.pending_volumes.take() {
            self.open_modal(Modal::Volumes {
                pane,
                include_pseudo,
                volumes,
                offset: 0,
                cursor: 0,
            });
        }
    }

    /// Drena lo retenido al cerrarse un modal, en el ORDEN de prioridad del
    /// contrato (única fuente — cada `ModalOutcome` que cierra el modal
    /// llama aquí, jamás a los `open_*` sueltos): (1) conflictos
    /// (`conflict_backlog`, bloquean transferencias vivas), (2) el plan IA
    /// retenido (`pending_ai_plan`, molde TUI), (3) los hits semánticos
    /// retenidos (`pending_semantic`), (4) los volúmenes retenidos
    /// (`pending_volumes`, task V4). Cada `open_*` es no-op si el anterior
    /// ya ocupó el turno — solo UNO abre por cierre, el resto sigue
    /// esperando.
    fn drain_pending_modals(&mut self) {
        self.open_next_conflict();
        self.open_pending_ai_plan();
        self.open_pending_semantic();
        self.open_pending_volumes();
    }

    /// The ONE way a modal opens in this window (K3c c4).
    ///
    /// It exists so that "no modal opens while the shortcut editor is asking
    /// for a blind keypress" is structural rather than a rule thirteen call
    /// sites have to remember. Capture mode paints "press the new key" and
    /// the reader is primed to press ANYTHING; a modal that arrives on its
    /// own — a conflict at the end of a copy, an AI plan off the session bus
    /// — takes the keyboard (`on_key` checks `self.modal` first) and is
    /// painted on top, so that next key answers a question the reader did not
    /// know was being asked. The key still reaches the modal; what norte must
    /// not do is keep inviting it.
    ///
    /// The editor itself SURVIVES — the reader gets their list back after
    /// answering — because the modal is not a reason to lose a filter and a
    /// cursor. Only the capture goes.
    fn open_modal(&mut self, modal: Modal) {
        if let Some(view) = &mut self.shortcuts_view {
            view.state.cancel_capture();
        }
        self.modal = Some(modal);
    }

    /// Pide una comparación de los dos panes (#158, `pane.compare-dirs`,
    /// spec 3 fase C1): paridad con `App::request_compare` de la TUI, hasta
    /// la frase de la negativa.
    ///
    /// El pane con FOCO es el lado izquierdo, y viaja en el comando: el panel
    /// congela ese lado al abrirse, y el foco puede haberse movido mientras
    /// la petición estaba en vuelo. Dos raíces iguales se niegan AQUÍ, sin
    /// vuelta por la red: el daemon contesta lo mismo (`-32602`), pero la
    /// frase no depende de que haya daemon y ninguna Task llega a existir.
    /// El resto de params son los mismos que pide la TUI, por el mismo
    /// motivo (ver `norte_frontend::compare::MTIME_TOLERANCE_MS` y los dos
    /// toggles que no existen).
    fn start_compare(&mut self) {
        let left = self.panes[self.focus].dir().clone();
        let right = self.panes[self.focus ^ 1].dir().clone();
        if left == right {
            self.errors[self.focus] = Some(norte_i18n::t("compare-same-path"));
            return;
        }
        self.compare_gen = self.compare_gen.wrapping_add(1);
        // Decirlo mientras el RPC va y viene, molde `request_volumes`: sin
        // esto la tecla no contesta nada hasta que hay Task. Con la clave
        // COMPARTIDA y `n = 0` —que es la verdad, todavía no ha llegado
        // ninguna fila—, no con una `gui-*` propia: la frase existe.
        self.errors[self.focus] = Some(norte_i18n::ta("compare-status-running", &[("n", "0")]));
        let _ = self.cmds.send(SessionCmd::Compare {
            left_pane: self.focus,
            generation: self.compare_gen,
            params: Box::new(norte_proto::methods::FsCompareParams {
                left,
                right,
                criteria: norte_proto::methods::CompareCriteria::default(),
                max_depth: None,
                mtime_tolerance_ms: norte_frontend::compare::MTIME_TOLERANCE_MS,
                // Sin toggle, y a propósito: `Backend::compare` responde
                // `Unsupported` a `true` antes de que exista Task alguna,
                // porque el engine acepta el campo y lo ignora. Ofrecerlo
                // sería ofrecer una promesa que nadie cumple.
                follow_symlinks: false,
                // Tampoco: el panel enseña un huérfano como UNA fila, y
                // descenderlo es lo que un plan de sincronización pide por su
                // cuenta (spec 2).
                descend_orphans: None,
            }),
        });
    }

    /// Despacha una tecla del panel de diferencias (#158, fase C1 tarea 3).
    ///
    /// Teclas FIJAS, igual que el mismo panel en la TUI: no hay vocabulario
    /// `dialog.*` para «cambia de lado» ni para «esconde los iguales», y
    /// dentro del panel el teclado es entero suyo, así que no hay nada con lo
    /// que chocar. Lo que una tecla SIGNIFICA lo decide
    /// [`compare_view::key_meaning`], que es puro y se testea sin ventana;
    /// esto solo lo ejecuta.
    fn on_compare_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) {
        use norte_frontend::compare::CATEGORIES;

        let m = ks.modifiers;
        let Some(view) = self.compare.as_ref() else {
            return;
        };
        let meaning = compare_view::key_meaning(
            &ks.key,
            m.control || m.alt || m.platform,
            compare_view::is_running(&view.run),
            view.run.cancel_requested,
        );
        match meaning {
            compare_view::Key::Ignore => {}
            // El primer `Esc`: cancela la Task y CONSERVA las filas (una
            // comparación cancelada no ha perdido nada, simplemente no
            // siguió). El panel se queda, y el pie pasa a decir «cancelada».
            compare_view::Key::CancelTask => {
                let task_id = view.task_id;
                let _ = self.cmds.send(SessionCmd::Cancel(task_id));
                if let Some(v) = self.compare.as_mut() {
                    v.run.cancel_requested = true;
                }
            }
            compare_view::Key::Close => self.close_compare(),
            compare_view::Key::Open => self.compare_enter(cx),
            otra => {
                let Some(v) = self.compare.as_mut() else {
                    return;
                };
                match otra {
                    compare_view::Key::SwapSide => v.run.pane.swap_active_side(),
                    compare_view::Key::Move(delta) => v.run.pane.move_by(delta),
                    compare_view::Key::First => v.run.pane.select_first(),
                    compare_view::Key::Last => v.run.pane.select_last(),
                    compare_view::Key::Filter(i) => {
                        if let Some(cat) = CATEGORIES.get(i) {
                            v.run.pane.toggle_filter(*cat);
                        }
                    }
                    _ => {}
                }
                // La lista está virtualizada: un cursor fuera de la ventana
                // no se ve, así que moverlo tiene que traerlo (mismo gesto
                // que `reveal_cursor` en los panes). Una selección que un
                // filtro esconde deja `visible_index` en `None` y no se
                // desplaza nada, que es la respuesta honesta.
                if let Some(i) = self
                    .compare
                    .as_ref()
                    .and_then(|v| v.run.pane.visible_index())
                {
                    self.compare_scroll
                        .scroll_to_item(i, ScrollStrategy::Nearest);
                }
            }
        }
    }

    /// Cierra el panel de diferencias y **cancela siempre** la Task que lo
    /// alimentaba.
    ///
    /// Soltar el estado no cancela nada por su cuenta: en remoto el daemon
    /// seguiría recorriendo los dos árboles enteros para un panel que ya no
    /// existe (el MAJOR-4 que la TUI pagó). Un `task.cancel` sobre una Task ya
    /// terminada es un no-op en el daemon, así que no hace falta mirar el
    /// estado antes.
    fn close_compare(&mut self) {
        if let Some(view) = self.compare.take() {
            let _ = self.cmds.send(SessionCmd::Cancel(view.task_id));
        }
    }

    /// Suelta el visor y **invalida cualquier apertura en vuelo**: un
    /// `ViewerOpened` que llegue después no puede reabrirlo por sorpresa,
    /// porque su generación ya quedó vieja (mismo gesto que `viewer.close`).
    ///
    /// Lo llama el panel de diferencias al abrirse: las dos pantallas son
    /// excluyentes (ver [`Self::open_viewer`]).
    fn close_viewer(&mut self) {
        self.viewer = None;
        self.viewer_image = None;
        self.viewer_gen = self.viewer_gen.wrapping_add(1);
        self.viewer_loading = false;
    }

    /// `Enter` sobre una fila: navega al directorio REAL del lado ACTIVO y
    /// cierra el panel (paridad con `on_compare_enter` de la TUI —incluidos
    /// el foco sembrado y el aviso sin destino, que se back-portaron allí en
    /// la revisión de rama, MINOR-8 y MAJOR-4).
    ///
    /// Un huérfano que el walk emitió como UNA fila sin enumerar su subárbol
    /// se expande así, que es el motivo por el que la fila lleva el `Entry`
    /// entero y no solo un nombre. Sin nada en el lado activo NO se cae al
    /// otro: se dice. El aviso va al flash y no a `errors[…]`, porque el
    /// panel tapa los dos panes y un banner debajo de él no lo lee nadie.
    fn compare_enter(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.compare.as_ref() else {
            return;
        };
        let pane = &view.run.pane;
        if pane.target_entry().is_none() {
            let side =
                norte_frontend::compare::side_label(pane.active_side(), norte_i18n::active());
            self.flash = Some((
                norte_i18n::ta("compare-no-target", &[("side", &side)]),
                true,
            ));
            return;
        }
        // El directorio al que ir lo decide el MODELO (regla 7): el propio
        // path si la fila es un directorio, su padre si es un fichero.
        let Some(destino) = pane.navigation_target() else {
            // Hay entrada pero no hay a dónde ir: un fichero colgado de la
            // raíz de su scheme no tiene padre. Se DICE, igual que el caso de
            // arriba — un `Enter` que no hace nada y no explica por qué se lee
            // como que la tecla está rota (revisión rust MINOR-1).
            let side =
                norte_frontend::compare::side_label(pane.active_side(), norte_i18n::active());
            self.flash = Some((
                norte_i18n::ta("compare-no-target", &[("side", &side)]),
                true,
            ));
            return;
        };
        // Y el cursor cae sobre la entrada de la que se salió, byte-exacto
        // (lo consume el listado al aterrizar; si ya no existe, cae al
        // default).
        let foco = pane.target_path().cloned();
        // Al pane del lado ACTIVO, y el foco con él: mandar SIEMPRE al pane
        // con foco le costaría al lector el otro directorio para ir a ver
        // este.
        let destino_pane = match pane.active_side() {
            norte_proto::methods::Side::Right => view.run.left_pane ^ 1,
            _ => view.run.left_pane,
        } & 1;
        self.close_compare();
        self.focus = destino_pane;
        if let Some(p) = foco {
            self.panes[destino_pane].set_pending_focus(p);
        }
        self.cd(destino_pane, destino, cx);
    }

    /// Pide `Backend::volumes` para `pane` (2026-08-10-volumes.md task V4,
    /// design §D): `pane.select-drive*` y el toggle "mostrar todo" DENTRO
    /// del picker (`ModalOutcome::RequestVolumes`) son la MISMA operación —
    /// una snapshot fresca para el modo pedido — así que ambos llaman aquí,
    /// molde `SessionCmd::SemanticSearch`. La respuesta llega async
    /// (`SessionEvent::VolumesReady`) y abre el modal, o se encola si otro
    /// ya está abierto (`pending_volumes`).
    fn request_volumes(&mut self, pane: usize, include_pseudo: bool) {
        let _ = self.cmds.send(SessionCmd::Volumes {
            pane,
            include_pseudo,
        });
        // Clave gui-* propia (misma doctrina que `gui-msg-semantic-running`):
        // la GUI no tiene camino para abortar la petición en vuelo — jamás
        // una affordance falsa. En `self.focus`, no en `pane`: el banner es
        // por-pane-VISIBLE, y lo que el lector tiene delante es el pane con
        // foco, sea o no el que `-left`/`-right` van a navegar.
        self.errors[self.focus] = Some(norte_i18n::t("gui-msg-volumes-running"));
    }

    /// Abre el renombrado in situ (`pane.rename`, shift+F6): el destino es el
    /// PADRE de la entrada bajo el cursor, no el dir del pane — renombrar no
    /// mueve de sitio. Siempre sobre el cursor (`selected`, que respeta el
    /// filtro quick como el visor): las marcas no renombran en bloque, eso
    /// sería un batch-rename. No-op sobre una raíz (no tiene padre ni nombre)
    /// y no-op sin selección.
    ///
    /// El nombre nace sembrado con los BYTES reales del actual — ver
    /// [`Modal::RenamePrompt`] para por qué bytes y no texto.
    fn open_rename(&mut self) {
        let Some(from) = self.panes[self.focus].selected().map(|e| e.path.clone()) else {
            return;
        };
        if let Some(modal) = rename_modal_for(&from) {
            self.open_modal(modal);
        }
    }

    /// Abre el prompt de instrucción del rename IA (M4-IA, `pane.ai-rename`):
    /// el plan aterrizará sobre el dir VIVO del pane activo en este instante
    /// (viaja dentro del modal y de la petición — un `cd` posterior no lo
    /// cambia de sitio).
    fn open_ai_rename(&mut self) {
        self.open_modal(Modal::AiRenamePrompt {
            dir: self.panes[self.focus].dir().clone(),
            query: Vec::new(),
        });
    }

    /// Abre el prompt de la búsqueda semántica (M4-IA-2,
    /// `pane.semantic-search`): sin dir — la búsqueda es global (root =
    /// None, paridad TUI `SEMANTIC_K`).
    fn open_semantic_search(&mut self) {
        self.open_modal(Modal::SemanticQuery { query: Vec::new() });
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
            // Solo un transfer tiene modal de resolución. Un lote de renames
            // (§17) que muere en conflicto NO lo tiene — y es justo el caso
            // en el que el directorio puede haber quedado a medias (un
            // rollback atascado contesta `Conflict{Exists}`), así que el
            // read-after-write de abajo tiene que correr igual.
            if let PendingOp::Transfer {
                kind: tk, from, to, ..
            } = op
            {
                self.queue_conflict(PendingTransfer { kind: tk, from, to }, kind);
                return;
            }
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
                // #103: `cur` ES el dir actual del pane (lo acabamos de leer de
                // ahí y `dirs.contains` lo casó byte-exacto), así que esto es un
                // REFRESCO, no un `cd`: las marcas sobreviven a la operación.
                self.refresh_dir(pane, cur, cx);
            }
        }
    }

    /// Pide hidratar (`fs.stat`) las filas de `pane` cuyos índices ABSOLUTOS
    /// están en `indices` y llegaron LAZY (#52: `norte-vfs-local` no statea
    /// por entrada, así que `size`/`mtime` vienen en `None` y las columnas
    /// Tamaño/Fecha se pintarían en blanco para siempre — #123).
    ///
    /// Se llama desde el processor de `uniform_list`, que da el rango
    /// VISIBLE EXACTO: así también cubre el scroll de rueda, que mueve la
    /// ventana sin tocar el cursor. Barato de repetir por frame: `probed`
    /// deja pasar cada ruta UNA vez por listado, así que en régimen
    /// estacionario no manda nada. Asíncrono y fail-soft como `Decorate`:
    /// llega tarde por `SessionEvent::Hydrated` con el mismo guard
    /// anti-stale, o no llega y la celda se queda en blanco.
    fn request_hydration(&mut self, pane: usize, indices: impl IntoIterator<Item = usize>) {
        let paths = hydration_batch(
            self.panes[pane].needs_stat_at(indices),
            &self.probed[pane],
            STAT_BATCH_MAX,
        );
        if paths.is_empty() {
            return;
        }
        self.probed[pane].extend(paths.iter().cloned());
        let _ = self.cmds.send(SessionCmd::StatBatch {
            pane,
            generation: self.generation[pane],
            dir: self.panes[pane].dir().clone(),
            paths,
        });
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

    /// `app.quit` (revisión C2/G0 IMPORTANT 3; S2 `[ui] confirm_quit`): con
    /// trabajo pendiente (tasks visibles en la franja o marcas activas),
    /// abre [`Modal::ConfirmQuit`] en vez de cerrar de inmediato — "y" en el
    /// modal manda `ModalOutcome::Quit`, que el dispatcher de `on_key` ya
    /// traduce a `cx.quit()`. `self.confirm_quit` decide el resto: `Never`
    /// cierra siempre YA (ni siquiera consulta lo pendiente — a diferencia
    /// del gate pre-S2, que solo conocía "auto"), `Always` abre el modal
    /// incluso sin nada pendiente (título genérico, ver `modal_lines`),
    /// `Auto` es el comportamiento pre-S2 (paridad con la TUI en su modo por
    /// defecto — `crates/norte-tui/src/main.rs`, `quit_needs_confirm`).
    fn quit_or_confirm(&mut self, cx: &mut Context<Self>) {
        if self.confirm_quit == ConfirmQuit::Never {
            cx.quit();
            return;
        }
        let marks = self.panes[0].marks_len() + self.panes[1].marks_len();
        let tasks = confirm_quit_task_count(self.task_progress.len(), marks, self.inflight.len());
        // `inflight` cubre la ventana entre submit y el primer evento de
        // task: una op recién lanzada aún sin progreso también debe frenar
        // el quit (solo el GATE; los contadores del modal siguen siendo los
        // visibles, ver `confirm_quit_task_count`).
        let pending = has_pending_work(tasks, marks) || !self.inflight.is_empty();
        if confirm_quit_should_open(self.confirm_quit, pending) {
            self.open_modal(Modal::ConfirmQuit { tasks, marks });
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
            "app.settings" => self.open_settings(),
            "app.palette" => self.open_palette(),
            "app.extensions" => self.open_extensions(),
            "app.help" => self.open_help(),
            "pane.columns" => self.open_columns_picker(),
            "pane.switch" => self.focus = 1 - self.focus,
            "cursor.up" => self.panes[f].cursor_up(),
            "cursor.down" => self.panes[f].cursor_down(),
            "cursor.top" => self.panes[f].home(),
            "cursor.bottom" => self.panes[f].end(),
            // #124: una PÁGINA es una pantalla del pane (menos una fila de
            // contexto): el alto real lo devuelve `uniform_list` en cada
            // frame; hasta el primero manda el fallback del modelo
            // (`norte_frontend::pane::DEFAULT_PAGE`).
            "cursor.page-up" => {
                let paso = self.panes[f].page_step();
                self.panes[f].page_up(paso);
            }
            "cursor.page-down" => {
                let paso = self.panes[f].page_step();
                self.panes[f].page_down(paso);
            }
            "nav.enter" => self.activate_enter(cx),
            "nav.parent" => {
                let dir = self.panes[f].dir().clone();
                if let Some(p) = dir.parent() {
                    // Foco pendiente (spec 2026-07-24 §S1): al aterrizar en
                    // el listado del padre, seleccionar el dir del que
                    // venimos.
                    self.panes[f].set_pending_focus(dir);
                    self.cd(f, p, cx);
                }
            }
            // mc/Total Commander: marca la selección visible y avanza — la
            // TUI y la GUI deben significar lo MISMO al despachar el mismo
            // nombre de comando (#103 review MAJOR-2), así que la GUI llama
            // a la misma composición del modelo compartido en vez de un
            // `toggle_mark` suelto.
            "mark.toggle" => self.panes[f].toggle_mark_and_advance(),
            "mark.all" => self.panes[f].mark_all(),
            "mark.invert" => self.panes[f].invert_marks(),
            "mark.clear" => self.panes[f].clear_marks(),
            "pane.copy" => self.open_transfer_modal(TransferKind::Copy),
            "pane.move" => self.open_transfer_modal(TransferKind::Move),
            "pane.delete" => self.open_delete_modal(),
            "pane.rename" => self.open_rename(),
            "pane.ai-rename" => self.open_ai_rename(),
            // #135 (S4, design §E). SOLO `app.terminal`: esta GUI no puede
            // suspenderse, así que lanza el emulador del escritorio. Los
            // otros dos comandos de #135 (`app.toggle-panels`,
            // `pane.command-line`) NO están en `COMMANDS` a propósito — sin
            // terminal anfitriona que enseñar ni que ceder, no significan
            // nada aquí, y dejarlos fuera es lo que los resuelve a
            // `Availability::NotHere` («no en este frontend»), que es la
            // verdad y ya se pinta en gris.
            "app.terminal" => self.open_terminal(cx),
            // Plan de ratón (tarea 4): al portapapeles, no al daemon — es la
            // única op de esta lista que no toca ningún backend.
            "pane.copy-path" => self.copy_paths_to_clipboard(cx),
            "pane.semantic-search" => self.open_semantic_search(),
            // #158, spec 3 fase C1. Tarea 4 metió el id en `keymap::COMMANDS`
            // (chord compartido `shift+f2` en cinco presets, `alt+d` de
            // `gui_supplement` en los dos que lo dejan sin ligar), así que
            // ahora el teclado y la paleta llegan aquí igual que la AYUDA:
            // antes de la tarea 4 SOLO la ayuda llegaba, porque sus filas se
            // resuelven contra `norte_frontend::availability` —la tabla
            // compartida entre frontends, que ya daba este comando por
            // disponible (revisión MAJOR-1)— y no contra `COMMANDS`. Ese
            // Enter caía antes en el brazo `other`, cuyo `debug_assert`
            // tumbaba una GUI de debug; ahora arranca la comparación de
            // verdad por cualquiera de las tres puertas.
            "pane.compare-dirs" => self.start_compare(),
            // 2026-08-10-volumes.md task V4 (design §D): `-left`/`-right`
            // name a SIDE, not the focus — Total Commander's `Alt+F1`/
            // `Alt+F2`, paridad TUI `Command::PaneSelectDrive{,Left,Right}`.
            // The list starts filtered (`include_pseudo: false`); the modal's
            // own `tab` toggle re-requests the other mode.
            "pane.select-drive" => self.request_volumes(f, false),
            "pane.select-drive-left" => self.request_volumes(0, false),
            "pane.select-drive-right" => self.request_volumes(1, false),
            "task.cancel" => self.cancel_task_under_cursor(),
            "task.next" => {
                if !self.task_order.is_empty() {
                    self.task_cursor = (self.task_cursor + 1).min(self.task_order.len() - 1);
                }
            }
            "task.prev" => self.task_cursor = self.task_cursor.saturating_sub(1),
            "task.dismiss" => self.dismiss_terminal_tasks(),
            "pane.view" => self.open_viewer(cx),
            // #106: recarga manual de AMBOS panes por el mismo camino que el
            // read-after-write (refresh same-dir → refill: marcas sobreviven
            // con poda visible).
            "pane.refresh" => {
                for pane in 0..self.panes.len() {
                    let dir = self.panes[pane].dir().clone();
                    self.refresh_dir(pane, dir, cx);
                }
            }
            // #107: presentación-solo, el pane aparta/devuelve dotfiles.
            "pane.toggle-hidden" => {
                self.panes[f].toggle_hidden();
            }
            // rust-reviewer MAJOR-2: este brazo NO era inalcanzable. Un
            // `lua:<nombre>` con charset válido sale SIEMPRE
            // `Availability::Here` (el registro Lua es dinámico, jamás puede
            // estar en un catálogo estático — ADR 0043 decisión 2), y la GUI
            // no tiene host Lua: ni mlua, ni `norte.command`, ni un brazo
            // `lua:` en ningún sitio. Un `keymap.toml` del PROPIO usuario con
            // `run = "lua:foo"` abortaba una GUI compilada en debug a la
            // primera pulsación, y no hacía nada en release. Ahora lo dice
            // con la misma frase que el resto de K1.
            other => {
                debug_assert!(
                    other.starts_with("lua:"),
                    "comando validado sin brazo y no es lua: {other}"
                );
                self.flash = Some((
                    norte_frontend::keymap::unavailable_message(
                        other,
                        norte_frontend::keymap::Availability::NotHere,
                    ),
                    true,
                ));
            }
        }
        // Tras un movimiento de cursor, sigue el scroll (issue #87).
        self.follow_cursor(f);
    }

    /// Abre la vista de ajustes (`app.settings`, F11, S4): construida
    /// SÍNCRONAMENTE desde `cfg_snapshot` (nunca releyendo disco — regla 2),
    /// mismo criterio que abrir la paleta en la TUI. Reemplaza cualquier
    /// vista anterior con una fresca (sin filtro/edición, igual que F11
    /// repetido en la TUI cerraría y reabriría). La sección Plugins (G3c)
    /// nace VACÍA (informativa) y se rellena async al llegar
    /// `PluginConfigSummariesReady` (`SessionCmd::PluginConfigSummaries`,
    /// disparado aquí) — mismo criterio "muestra algo YA, enriquece
    /// después" que `Decorate`/`Columns`.
    fn open_settings(&mut self) {
        self.settings_view = Some(settings_view::SettingsView::new(
            norte_frontend::settings::build_rows(&self.cfg_snapshot, &[]),
        ));
        let _ = self.cmds.send(SessionCmd::PluginConfigSummaries);
    }

    /// Maneja UNA tecla con la vista de ajustes abierta (`on_key`, tramo
    /// dedicado): ctrl/alt/platform se descartan ANTES de llegar al filtro
    /// (un ctrl-chord no debe teclearse en el buffer — mismo gate que el
    /// visor aplica solo a platform; aquí se extiende a ctrl/alt porque esta
    /// pantalla SÍ acepta tecleo libre). El resto delega en
    /// [`settings_view::on_key`] (puro) y actúa sobre el
    /// [`settings_view::SettingsOutcome`].
    ///
    /// K3c c4: `ctrl+k` opens the shortcut editor, and it is checked BEFORE
    /// that gate — deliberately and as narrowly as possible. The gate drops
    /// every ctrl-chord so that none is typed into the filter, which is
    /// right for text and would otherwise mean this screen can never grow a
    /// verb. One key, one modifier, no alt and no ⌘.
    fn on_settings_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) {
        if ks.modifiers.control
            && !ks.modifiers.alt
            && !ks.modifiers.platform
            && !ks.modifiers.shift
            && ks.key == "k"
        {
            self.open_shortcuts();
            return;
        }
        if ks.modifiers.control || ks.modifiers.alt || ks.modifiers.platform {
            return;
        }
        let Some(view) = &mut self.settings_view else {
            return;
        };
        let outcome = settings_view::on_key(view, &ks.key, ks.key_char.as_deref());
        match outcome {
            settings_view::SettingsOutcome::None => {}
            // The editor lives IN FRONT of this view (see the field's doc),
            // so closing the view underneath it would strand it over nothing.
            // Unreachable while the editor owns the keyboard — it is checked
            // first in `on_key` — and cheap insurance against the next path
            // that closes settings without going through a key.
            settings_view::SettingsOutcome::Close => {
                self.settings_view = None;
                self.shortcuts_view = None;
            }
            settings_view::SettingsOutcome::Write(w) => self.commit_settings_write(*w, cx),
            settings_view::SettingsOutcome::Invalid(e) => {
                if let Some(view) = &mut self.settings_view {
                    view.status = Some(settings_view::SettingsStatus {
                        message: norte_frontend::settings::edit_error_message(&e),
                        error: true,
                    });
                }
            }
        }
    }

    /// Click en una fila de la vista de ajustes: fija el cursor sobre `idx`
    /// (posición DENTRO de `visible()`, mismo contrato que
    /// `SettingsState::set_cursor`) y activa (mismo camino que Enter) — un
    /// click sobre un `Bool`/`Enum`/`ThemeName`/`PresetName` cicla de
    /// inmediato, sobre `Text`/`Int` abre la edición inline. No-op mientras
    /// se edita OTRA fila (mismo guard que `set_cursor`/`up`/`down`): un
    /// click perdido no debe tirar lo que el usuario ya tecleó.
    fn on_settings_row_click(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(view) = &mut self.settings_view else {
            return;
        };
        if view.state.is_editing() {
            return;
        }
        view.state.set_cursor(idx);
        match settings_view::activate(&mut view.state) {
            settings_view::SettingsOutcome::Write(w) => self.commit_settings_write(*w, cx),
            settings_view::SettingsOutcome::None
            | settings_view::SettingsOutcome::Close
            | settings_view::SettingsOutcome::Invalid(_) => {}
        }
        cx.notify();
    }

    /// The LIVE maps a shortcut row and every verdict are read off — the ones
    /// the resolvers are using RIGHT NOW, never a copy taken when the editor
    /// opened. A write replaces them (`apply_keymap_live`), and a verdict
    /// read from a replaced map is a verdict about somebody else's keyboard.
    ///
    /// Not a `&self` method on purpose: the callers need it alongside a
    /// `&mut self.shortcuts_view`, and only a per-FIELD borrow is disjoint
    /// from that.
    fn shortcut_maps(&self) -> shortcuts_view::Maps<'_> {
        shortcuts_view::Maps {
            browse: self.resolver.effective(),
            viewer: self.viewer_resolver.effective(),
        }
    }

    /// Opens the shortcut editor (K3c c4, `ctrl+k` from the settings view),
    /// which stays open behind it.
    ///
    /// Rows are built SYNCHRONOUSLY from the live effectives — no disk (rule
    /// 2), the same criterion as `open_settings` building from
    /// `cfg_snapshot`.
    fn open_shortcuts(&mut self) {
        let rows = shortcuts_view::rows(self.shortcut_maps(), norte_i18n::active());
        self.shortcuts_view = Some(shortcuts_view::ShortcutsView::new(rows));
    }

    /// Rebuilds the editor's rows from the CURRENT effectives, keeping the
    /// filter and re-anchoring the cursor on the row it was on
    /// (`ShortcutsState::refresh`, whose doc explains why an index would be
    /// the wrong thing to keep). No-op when the editor is closed.
    ///
    /// It also cancels any capture, which is the point after a write: the
    /// verdict was read off the map that has just been replaced.
    fn refresh_shortcuts(&mut self) {
        if self.shortcuts_view.is_none() {
            return;
        }
        let rows = shortcuts_view::rows(self.shortcut_maps(), norte_i18n::active());
        if let Some(view) = &mut self.shortcuts_view {
            view.state.refresh(rows);
        }
    }

    /// The status line under the editor's list.
    fn set_shortcuts_status(&mut self, message: String, error: bool) {
        if let Some(view) = &mut self.shortcuts_view {
            view.status = Some(shortcuts_view::ShortcutsStatus { message, error });
        }
    }

    /// Handles ONE key with the shortcut editor open (`on_key`, dedicated
    /// branch): the routing is pure ([`shortcuts_view::on_key`]) and this
    /// acts on its outcome.
    fn on_shortcuts_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) {
        // Built INLINE and not through `Self::shortcut_maps`: that borrows
        // the whole of `self`, and this needs the two resolver fields next to
        // a `&mut self.shortcuts_view`. Only a per-field borrow is disjoint.
        let maps = shortcuts_view::Maps {
            browse: self.resolver.effective(),
            viewer: self.viewer_resolver.effective(),
        };
        let mods = keymap_mods(ks.modifiers);
        let Some(view) = &mut self.shortcuts_view else {
            return;
        };
        // A key that says something clears the last write's status: leaving
        // "saved" under a list the reader is now filtering would keep
        // claiming a thing about a row that is no longer on screen.
        view.status = None;
        let outcome = shortcuts_view::on_key(view, &ks.key, ks.key_char.as_deref(), mods, maps);
        match outcome {
            shortcuts_view::ShortcutsOutcome::None => {}
            shortcuts_view::ShortcutsOutcome::Close => self.shortcuts_view = None,
            shortcuts_view::ShortcutsOutcome::Confirm => self.confirm_shortcut(cx),
            shortcuts_view::ShortcutsOutcome::Unbind => self.unbind_shortcut(cx),
            shortcuts_view::ShortcutsOutcome::NotBindable => {
                self.set_shortcuts_status(norte_i18n::t("msg-shortcut-not-bindable"), true);
            }
        }
    }

    /// Click on an editor row: SELECTS it (`idx` is a position within
    /// `visible()`, the same contract as `ShortcutsState::set_cursor`).
    ///
    /// It does NOT start a capture, unlike the settings view's click, and the
    /// asymmetry is deliberate: capture is a mode in which every key becomes
    /// a binding, and dropping a reader into it from a stray click — with no
    /// keystroke of their own to say they meant it — is how a mis-click
    /// becomes a rebind. `enter` opens it, and the footer says so.
    /// `set_cursor` is already a no-op while capturing.
    fn on_shortcuts_row_click(&mut self, idx: usize, cx: &mut Context<Self>) {
        // The modal scrim does not `.occlude()` yet (known GUI debt), so a
        // click meant for a modal painted on top still reaches these rows.
        // Moving the cursor from under a question the reader is answering
        // would move what `ctrl+u` deletes.
        if self.modal.is_some() {
            return;
        }
        if let Some(view) = &mut self.shortcuts_view {
            view.state.set_cursor(idx);
        }
        cx.notify();
    }

    /// THE DOOR, as this frontend calls it: the active preset's name, the
    /// loaded layers and the command set this screen was VALIDATED with.
    ///
    /// The door itself is `norte_frontend::shortcuts::plan_rebind` — the
    /// layer cut is not a frontend's to improvise, and its documentation
    /// names the two ways of guessing it that fail in silence. What is left
    /// here is what is genuinely the GUI's:
    ///
    /// **The supplement is a layer, and the door is given it**
    /// ([`rebind_layers`]).
    fn plan_rebind(
        &self,
        screen: norte_frontend::keymap::Screen,
        seq: &[norte_frontend::keymap::Chord],
        command: &str,
    ) -> Result<norte_frontend::keymap::RebindWrite, norte_frontend::shortcuts::PlanError> {
        let (kinds, layers) = rebind_layers(&self.cfg_snapshot);
        norte_frontend::shortcuts::plan_rebind(
            &self.cfg_snapshot.common.preset,
            &kinds,
            &layers,
            keymap::screen_commands(screen),
            screen,
            seq,
            command,
        )
    }

    /// Confirms the capture: the door ([`Self::plan_rebind`]) and, only if it
    /// passes, the writer — on the BACKGROUND executor (rule 2:
    /// `persist_keymap_bind` takes a file lock and does synchronous I/O, and
    /// this runs on the render thread), the same `cx.background_spawn` shape
    /// `commit_settings_write` uses and for the same reasons its doc gives.
    ///
    /// What reaches the writer is what the door returned, VERBATIM: the
    /// section, the list (`prepend_keymap` — an append would not outrank the
    /// preset and would never fire) and the SPELLING of the chords.
    /// Re-rendering the captured sequence here would reopen the very gap the
    /// door closes.
    ///
    /// And then the reload, because this frontend has no file watcher: the
    /// same background task re-reads the merged config and rebuilds the three
    /// effectives, and `apply_shortcut_write_result` installs them. If that
    /// rebuild fails, the old keymap stays and the message says so.
    fn confirm_shortcut(&mut self, cx: &mut Context<Self>) {
        let captured = self.shortcuts_view.as_ref().and_then(|v| {
            v.state
                .confirmable()
                .map(|(screen, command, seq)| (screen, command.to_owned(), seq.to_vec()))
        });
        // `None` = a refusal verdict, or nothing captured yet: nothing is
        // written and the capture stays alive so another key can be tried —
        // but the `enter` just pressed cannot go silent, so the verdict is
        // repeated, since it IS the reason nothing was saved.
        let Some((screen, command, seq)) = captured else {
            let echo = self
                .shortcuts_view
                .as_ref()
                .and_then(|v| v.state.capture())
                .and_then(norte_frontend::shortcuts::Capture::verdict)
                .map(|v| norte_frontend::shortcuts::verdict_message(v, norte_i18n::active()));
            if let Some(msg) = echo {
                self.set_shortcuts_status(msg, true);
            }
            return;
        };
        let Some(dir) = norte_config::user_config_dir() else {
            self.set_shortcuts_status(norte_i18n::t("msg-settings-no-config-dir"), true);
            return;
        };
        let write = match self.plan_rebind(screen, &seq, &command) {
            Ok(w) => w,
            Err(e) => {
                let msg = norte_frontend::shortcuts::plan_error_message(&e, norte_i18n::active());
                self.set_shortcuts_status(msg, true);
                if let Some(view) = &mut self.shortcuts_view {
                    view.state.cancel_capture();
                }
                return;
            }
        };
        let lang = norte_i18n::active();
        let painted = norte_frontend::keymap::paint_chord(&write.chords.join(" "));
        let label = norte_frontend::whichkey::command_label(&command, lang);
        let mut ok = norte_i18n::ta(
            "msg-shortcut-bound",
            &[("chord", painted.as_str()), ("command", label.as_str())],
        );
        // The one thing this window can bind that the terminal can never
        // press. Not a refusal — it works here — so it is said, once, next to
        // the confirmation rather than discovered months later in the TUI.
        if shortcuts_view::captures_cmd(&seq) {
            ok = format!("{ok} — {}", norte_i18n::t("gui-shortcuts-cmd-note"));
        }
        // The decision is made; the capture goes NOW, not when the result
        // lands. This frontend's write is detached (`cx.spawn`), unlike the
        // TUI's, which `.await`s inline in the key loop — leaving the capture
        // alive across the flight window let a held `enter` issue one full
        // write-and-reload per auto-repeat.
        let write_gen = self.next_shortcut_write();
        if let Some(view) = &mut self.shortcuts_view {
            view.state.cancel_capture();
        }
        cx.spawn(async move |this, cx| {
            let (written, reloaded) = cx
                .background_spawn(async move {
                    let written = norte_config::persist_keymap_bind(
                        &dir,
                        write.section,
                        write.list,
                        &write.chords,
                        &write.command,
                    );
                    let reloaded = written.is_ok().then(reload_after_keymap_write);
                    (written, reloaded)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.apply_shortcut_write_result(write_gen, written, reloaded, ok, None, cx);
            });
        })
        .detach();
    }

    /// Removes the binding of the row under the cursor from BOTH of the
    /// user's lists — the reason c1 wrote `persist_keymap_unbind`: an editor
    /// that can only add is an editor that cannot fix a mistake.
    ///
    /// No door, deliberately, for half the load: removing an entry cannot
    /// introduce an illegal shape, so there is no load to simulate.
    ///
    /// What is NOT checked is the EFFECT, which is why both messages talk
    /// about the FILE and not about the key — "removed from your
    /// keymap.toml", never "this key no longer does X". Three cases this
    /// path cannot tell apart, all tracked in #141: the binding lives in
    /// `[global]`, which `Screen::section` never names; the file spells it
    /// another legal way (`mod+p` for `ctrl+p`) and the writer matches byte
    /// for byte; or another layer binds the same key and it keeps working.
    /// Same wording as the TUI's, and for the same reason — promising more
    /// here would make the GUI the surface that lies.
    fn unbind_shortcut(&mut self, cx: &mut Context<Self>) {
        let selected = self
            .shortcuts_view
            .as_ref()
            .and_then(|v| v.state.selected())
            .map(|r| {
                (
                    r.screen,
                    r.seq.iter().map(ToString::to_string).collect::<Vec<_>>(),
                    r.command.clone(),
                    r.chord.clone(),
                )
            });
        // Nothing selected (an empty filter result): the key must still say
        // something. A `ctrl+u` that neither removes nor speaks reads as a
        // dead key, and this one is destructive when it is not.
        let Some((screen, chords, command, painted)) = selected else {
            self.set_shortcuts_status(norte_i18n::t("msg-shortcut-nothing-to-unbind"), true);
            return;
        };
        if chords.is_empty() {
            self.set_shortcuts_status(norte_i18n::t("msg-shortcut-nothing-to-unbind"), true);
            return;
        }
        let Some(dir) = norte_config::user_config_dir() else {
            self.set_shortcuts_status(norte_i18n::t("msg-settings-no-config-dir"), true);
            return;
        };
        let lang = norte_i18n::active();
        let label = norte_frontend::whichkey::command_label(&command, lang);
        let ok = norte_i18n::ta(
            "msg-shortcut-unbound",
            &[("chord", painted.as_str()), ("command", label.as_str())],
        );
        let nothing = norte_i18n::t("msg-shortcut-nothing-to-unbind");
        let section = screen.section();
        let write_gen = self.next_shortcut_write();
        cx.spawn(async move |this, cx| {
            let (written, reloaded) = cx
                .background_spawn(async move {
                    let written =
                        norte_config::persist_keymap_unbind(&dir, section, &chords, &command);
                    let reloaded = written.is_ok().then(reload_after_keymap_write);
                    (written, reloaded)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.apply_shortcut_write_result(
                    write_gen,
                    written,
                    reloaded,
                    ok,
                    Some(nothing),
                    cx,
                );
            });
        })
        .detach();
    }

    /// Claims the next `keymap.toml` write of this window, and makes every
    /// earlier one STALE.
    ///
    /// Both write paths are detached (`cx.spawn(...).detach()`), so two of
    /// them really do overlap here — the TUI cannot get into this state, its
    /// `spawn_blocking` is awaited inline in the key loop. Two overlapping
    /// writes serialise correctly on the file lock, but their RESULTS come
    /// back in whatever order, and each result installs a full set of
    /// effectives plus a `cfg_snapshot`. The older one landing last would
    /// install a keymap that predates the newer write: the window would then
    /// resolve keys the file does not contain, after a status line that had
    /// already said the newer binding was applied. A generation is the
    /// cheapest thing that cannot be got wrong.
    fn next_shortcut_write(&mut self) -> u64 {
        self.shortcut_write_gen = self.shortcut_write_gen.wrapping_add(1);
        self.shortcut_write_gen
    }

    /// Applies the result of a `keymap.toml` write (UI thread).
    ///
    /// The order matters and is the whole point of the method. A write that
    /// SUCCEEDED still proves nothing about the keyboard: the rebuild is
    /// all-or-nothing, so a file that will not load leaves the OLD map in
    /// place, and an editor that reported "F5 now runs X" on the strength of
    /// the write alone would be describing a key that did not change. So the
    /// message is only the plain one when the rebuild landed; otherwise it
    /// carries the honest qualifier ([`shortcut_write_message`]).
    ///
    /// `write_gen` is [`Self::next_shortcut_write`]'s stamp: a result that is not
    /// the newest is DROPPED whole, message included, because the newer write
    /// both supersedes its file state and will report its own.
    ///
    /// `unchanged` is the message for `KeymapWrite::changed == false`, and
    /// only the unbind passes one: for a BIND, "already bound to that" and
    /// "just bound" are the same statement about the key, while for an
    /// UNBIND "nothing matched" and "removed" are opposite ones.
    fn apply_shortcut_write_result(
        &mut self,
        write_gen: u64,
        written: std::io::Result<norte_config::KeymapWrite>,
        reloaded: Option<ReloadedKeymap>,
        ok: String,
        unchanged: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if write_gen != self.shortcut_write_gen {
            return;
        }
        let changed = match written {
            Ok(w) => w.changed,
            Err(e) => {
                self.announce_shortcut_write(
                    norte_i18n::ta(
                        "msg-settings-save-failed",
                        &[("error", &io_error_category(&e))],
                    ),
                    true,
                );
                cx.notify();
                return;
            }
        };
        let mut applied = false;
        if let Some((Ok(cfg), fresh_keymap)) = reloaded {
            applied = self.apply_keymap_live(fresh_keymap);
            self.cfg_snapshot = cfg;
        }
        let (message, error) = shortcut_write_message(changed, applied, ok, unchanged);
        self.announce_shortcut_write(message, error);
        cx.notify();
    }

    /// Says what a `keymap.toml` write did, WHEREVER the reader now is.
    ///
    /// The editor's own status line when it is open, and the settings view's
    /// when it is not — because the write is detached and the reader can
    /// close the editor while it is in flight. Dropping the message then is
    /// what turns a failed write (a root-owned `keymap.toml`, a read-only
    /// mount) into a silent one, and a mutation that fails silently is the
    /// thing this repository refuses hardest. Settings is always still open
    /// underneath (see `render_shortcuts`'s invariant), so there is always
    /// somewhere for it to go.
    fn announce_shortcut_write(&mut self, message: String, error: bool) {
        if self.shortcuts_view.is_some() {
            self.set_shortcuts_status(message, error);
        } else {
            self.set_settings_status(message, error);
        }
    }

    /// Persiste un [`PendingWrite`] (S4) fuera del hilo de UI (regla 2,
    /// mismo criterio que el `spawn_blocking` de la TUI): esta GUI no tiene
    /// runtime tokio en el hilo de render (a diferencia del hilo de sesión,
    /// `session.rs`, que sí lo tiene, pero es un canal AJENO — persistir
    /// config no tiene nada que ver con la conexión al daemon, y ese hilo
    /// puede no estar sirviendo nada si `connect` falló; usarlo colgaría la
    /// escritura para siempre). En vez de un `std::thread::spawn` nuevo, usa
    /// el executor de FONDO que GPUI ya trae (`cx.background_spawn`, dentro
    /// de un `cx.spawn` normal — el mismo idioma "async → UI" que
    /// `spawn_event_loop` documenta al principio del fichero): el I/O
    /// bloqueante (escribir + releer la config fusionada, y si el ajuste
    /// tocado es `keymap.preset`, reconstruir los efectivos) corre en el
    /// pool de fondo; el resultado vuelve al hilo de UI vía `this.update`.
    /// Solo se reconstruye el keymap cuando hace falta (evita releer
    /// `keymap.toml` de cada capa en cada escritura de un ajuste no
    /// relacionado).
    fn commit_settings_write(&mut self, write: PendingWrite, cx: &mut Context<Self>) {
        let PendingWrite {
            section,
            key,
            value,
            name,
            display,
        } = write;
        let Some(dir) = norte_config::user_config_dir() else {
            self.set_settings_status(norte_i18n::t("msg-settings-no-config-dir"), true);
            cx.notify();
            return;
        };
        let needs_keymap_rebuild = section == "keymap" && key == "preset";
        cx.spawn(async move |this, cx| {
            let key_for_bg = key.clone();
            let (persisted, fresh_cfg, fresh_keymap) = cx
                .background_spawn(async move {
                    // Escrituras de norte.toml SERIALIZADAS (review 7c
                    // MAJOR-1); ver CONFIG_WRITE_SERIAL.
                    let _guard = CONFIG_WRITE_SERIAL
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let persisted = norte_config::persist_set(&dir, section, &key_for_bg, value);
                    let fresh_cfg = persisted
                        .is_ok()
                        .then(|| norte_frontend::config::load(&norte_config::standard_layers()));
                    // K3b: the THREE-screen builder, so a preset switch
                    // rebuilds `dialog_effective` in the same breath as
                    // `resolver`/`viewer_resolver` — one call, one moment,
                    // never a Dialog section one preset behind the other two.
                    let fresh_keymap = match (&fresh_cfg, needs_keymap_rebuild) {
                        (Some(Ok(cfg)), true) => {
                            Some(keymap::build_effectives3(&cfg.common.preset))
                        }
                        _ => None,
                    };
                    (persisted, fresh_cfg, fresh_keymap)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.apply_settings_write_result(
                    section,
                    &key,
                    &name,
                    &display,
                    persisted,
                    fresh_cfg,
                    fresh_keymap,
                    cx,
                );
            });
        })
        .detach();
    }

    /// Aplica el resultado de una escritura de ajustes (S4, hilo de UI): un
    /// fallo de escritura anuncia el error y no toca nada más. Un OK
    /// despacha por `(section, key)` (forma WIRE — ver
    /// `norte_frontend::settings::wire_key`) qué re-resolver EN CALIENTE
    /// desde la config fresca; cualquier `(section, key)` sin brazo propio
    /// (hoy solo `("ui","lang")`: Fluent negocia el idioma una vez al
    /// arrancar, sin camino de recarga en esta GUI) queda "requiere
    /// reinicio" — mismo vocabulario que `settings_view::gui_applies_live`,
    /// que pinta el aviso ESTÁTICO por fila (mantener las dos ramas
    /// sincronizadas si un futuro ajuste se vuelve, o deja de ser, en
    /// caliente).
    #[allow(clippy::too_many_arguments)]
    fn apply_settings_write_result(
        &mut self,
        section: &'static str,
        key: &str,
        name: &str,
        display: &str,
        persisted: std::io::Result<std::path::PathBuf>,
        fresh_cfg: Option<
            Result<norte_frontend::config::FrontendConfig, norte_config::ConfigError>,
        >,
        fresh_keymap: Option<
            Result<
                (
                    norte_frontend::keymap::Effective,
                    norte_frontend::keymap::Effective,
                    norte_frontend::keymap::Effective,
                ),
                norte_frontend::keymap::KeymapError,
            >,
        >,
        cx: &mut Context<Self>,
    ) {
        if let Err(e) = persisted {
            self.set_settings_status(
                norte_i18n::ta(
                    "msg-settings-save-failed",
                    &[("error", &io_error_category(&e))],
                ),
                true,
            );
            cx.notify();
            return;
        }
        let mut applied_live = true;
        match fresh_cfg {
            Some(Ok(cfg)) => {
                match (section, key) {
                    ("ui", "theme") => applied_live = self.apply_theme_live(&cfg),
                    ("ui", "reduce_motion") => {
                        cx.set_reduce_motion(cfg.common.ui_reduce_motion.unwrap_or(false));
                    }
                    ("ui", "confirm_quit") => self.confirm_quit = cfg.common.ui_confirm_quit,
                    ("ui", "quick_search") => self.quick_mode = cfg.quick_search_mode,
                    ("ui", "font" | "mono_font" | "font_size") => {
                        self.apply_fonts_live(&cfg, cx);
                    }
                    ("keymap", "preset") => applied_live = self.apply_keymap_live(fresh_keymap),
                    _ => applied_live = false, // ui.lang y cualquier id futuro sin brazo propio.
                }
                self.cfg_snapshot = cfg;
            }
            Some(Err(_)) | None => applied_live = false,
        }
        let mut msg = norte_i18n::ta("msg-settings-saved", &[("name", name), ("value", display)]);
        if !applied_live {
            msg = format!("{msg} — {}", norte_i18n::t("settings-restart-badge"));
        }
        self.set_settings_status(msg, false);
        cx.notify();
    }

    /// Re-resuelve `theme`+`effects` desde `cfg.common.ui_theme` (S4, tras
    /// una escritura de `ui.theme` con OK): mismo par que `new` resuelve al
    /// arrancar. Si el nombre/ruta ya no resuelve (edge case: un tema
    /// personalizado borrado entre el ciclo y el commit), CONSERVA el tema
    /// vigente — un cambio a medias sería más sorprendente que "no cambió
    /// del todo" — y devuelve `false` (el caller lo marca "requiere
    /// reinicio" en el mensaje, aunque en la práctica un reinicio tampoco lo
    /// arreglaría; es la categoría más honesta disponible sin inventar un
    /// tercer estado). Los avisos de `[effects]` degradados (`from_theme`)
    /// se descartan aquí a propósito: son ruido de arranque, no algo que la
    /// vista de ajustes necesite mostrar en su status de una línea.
    fn apply_theme_live(&mut self, cfg: &norte_frontend::config::FrontendConfig) -> bool {
        match norte_frontend::theme::resolve_theme(cfg.common.ui_theme.as_deref()) {
            Ok(theme) => {
                let (effects, _warnings) = effects::EffectsV1::from_theme(&theme);
                self.theme = theme;
                self.effects = effects;
                true
            }
            Err(_) => false,
        }
    }

    /// Re-resuelve `fonts` desde `cfg` (S4, tras `ui.font`/`ui.mono-font`/
    /// `ui.font-size` con OK): `FontSet::resolve` es barato — issue #87 no
    /// aplica, esto NO corre por frame — así que, a diferencia del supuesto
    /// pesimista del doc de `SettingDef` (fuentes "resueltas solo al
    /// arrancar"), la GUI SÍ lo aplica en caliente. Revalida la familia
    /// contra el fontdb REAL (`cx.text_system().all_font_names()`, mismo
    /// criterio que `NorteGui::new`/`validated_family`) — una familia que ya
    /// no exporta ese nombre sustituye al default en silencio (sin banner:
    /// el status de una línea de la vista ya confirma "guardado", un
    /// segundo aviso de sustitución sería ruido para un caso raro).
    fn apply_fonts_live(
        &mut self,
        cfg: &norte_frontend::config::FrontendConfig,
        cx: &mut Context<Self>,
    ) {
        let known = cx.text_system().all_font_names();
        let (ui, _) = validated_family(cfg.common.ui_font.as_deref(), &known, ".SystemUIFont");
        let (mono, _) =
            validated_family(cfg.common.ui_mono_font.as_deref(), &known, "JetBrains Mono");
        self.fonts = FontSet::resolve(&ui, &mono, cfg.common.ui_font_size);
    }

    /// Publishes the PANE resolver's pending state to [`Self::which_key`]
    /// (K3a). The ONE place that decides whether the panel is open for the
    /// dual-pane screen, and it decides it from the resolver rather than from
    /// which [`Resolution`](norte_frontend::keymap::Resolution) arm called
    /// it:
    ///
    /// - a pending SEQUENCE opens it — at once, with no delay of any kind
    ///   (ADR 0006's resolution is timing-free);
    /// - a bare COUNT does not: its pending prefix is empty, so there are no
    ///   rows, and the continuation of a count is any key at all.
    ///
    /// Called from the `Pending`/`Counting` arm of the pane match in
    /// `on_key`. Every OTHER arm of that match, and the non-modelled-key
    /// `else`, clear the field directly instead — the sequence just ended, so
    /// there is nothing left to build.
    fn refresh_which_key(&mut self) {
        self.which_key = which_key_for(&self.resolver, norte_i18n::active());
    }

    /// [`Self::refresh_which_key`]'s twin for the VIEWER resolver: while the
    /// viewer owns the keyboard, the panel must describe what the viewer's
    /// keymap does next, not the pane's (`render`'s `active_resolver` makes
    /// the same swap for the plain-text pending strip, `#91`).
    fn refresh_which_key_viewer(&mut self) {
        self.which_key = which_key_for(&self.viewer_resolver, norte_i18n::active());
    }

    /// Reemplaza `resolver`/`viewer_resolver`/`dialog_effective` con los
    /// efectivos frescos (S4, tras `keymap.preset` con OK): `fresh_keymap` ya
    /// viene calculado desde el hilo de fondo (`commit_settings_write`, evita
    /// releer `keymap.toml` en el hilo de UI) — las TRES pantallas, K3b, así
    /// que `dialog_effective` nunca queda un preset por detrás de las otras
    /// dos. Un preset roto/capa de usuario inválida en el momento del commit
    /// CONSERVA los tres efectivos vigentes (nunca deja la GUI sin bindings)
    /// y devuelve `false`.
    fn apply_keymap_live(
        &mut self,
        fresh_keymap: Option<
            Result<
                (
                    norte_frontend::keymap::Effective,
                    norte_frontend::keymap::Effective,
                    norte_frontend::keymap::Effective,
                ),
                norte_frontend::keymap::KeymapError,
            >,
        >,
    ) -> bool {
        match fresh_keymap {
            Some(Ok((browse, viewer, dialog))) => {
                self.resolver = norte_frontend::keymap::Resolver::new(browse);
                self.viewer_resolver = norte_frontend::keymap::Resolver::new(viewer);
                self.dialog_effective = dialog;
                // K3a: the resolvers were just REPLACED — any cached rows
                // describe the map that no longer exists, and the fresh
                // resolvers start with nothing pending regardless.
                self.which_key = None;
                self.rebuild_help_chords();
                // K3c c4: and the shortcut editor, if open, is looking at a
                // list DERIVED from those three maps — the same obligation
                // `rebuild_help_chords` documents, for the one screen whose
                // rows ARE the keymap. Refreshing here rather than at each
                // caller is what keeps a rebind (which lands via
                // `apply_shortcut_write_result`) and a preset switch (via
                // `apply_settings_write_result`) from needing to remember it
                // separately. It also drops any capture, whose verdict was
                // read off the map that has just been replaced.
                self.refresh_shortcuts();
                true
            }
            _ => false,
        }
    }

    /// Lleva un keymap recién cargado a la página de ayuda ABIERTA, si la hay.
    ///
    /// `GuiChords`' own rustdoc states the obligation — «rebuild it wherever
    /// the effectives are rebuilt: a rebind that does not reach this resolver
    /// is a help page teaching the OLD key» — y hasta aquí solo se cumplía al
    /// abrir. La página se quedaba enseñando las teclas viejas, y su hoja de
    /// teclado entera (generada de los efectivos, no del corpus) era un
    /// listado obsoleto.
    ///
    /// # La ventana por la que esto pasa
    ///
    /// Estrecha, y por eso conviene dejarla escrita en vez de que el siguiente
    /// lector la busque. Esta GUI no vigila ficheros de configuración (la TUI
    /// sí): el keymap solo cambia desde la pantalla de ajustes, que no puede
    /// estar abierta a la vez que la ayuda —el overlay abierto se queda con las
    /// teclas—. Lo que sí es asíncrono es la ESCRITURA
    /// (`commit_settings_write` va por `cx.background_spawn`), así que basta
    /// cambiar el preset, cerrar ajustes y abrir la ayuda antes de que el hilo
    /// de fondo conteste: el swap de resolvers aterriza con la página delante.
    ///
    /// Los HECHOS no se re-leen: `HelpView::refreeze` los arrastra del
    /// resolver anterior a propósito. Congelarlos al abrir es lo que impide
    /// que una fila cambie de veredicto bajo el cursor del lector, y guardar
    /// un keymap no es una razón para re-juzgar qué puede ejecutarse.
    fn rebuild_help_chords(&mut self) {
        if self.help.is_none() {
            return;
        }
        let base = help_view::GuiChords::new(
            self.resolver.effective(),
            self.viewer_resolver.effective(),
            norte_i18n::active(),
        );
        let keys = help_view::keys_lines(
            self.resolver.effective(),
            self.viewer_resolver.effective(),
            &self.dialog_effective,
        );
        let prev = self
            .help_chords
            .clone()
            .unwrap_or_else(|| base.with_facts(self.help_facts()));
        if let Some(view) = &mut self.help {
            view.set_keys_lines(keys);
            self.help_chords = Some(view.refreeze(&base, &prev));
        }
    }

    /// Fija el status de la vista de ajustes y refresca sus filas desde
    /// `cfg_snapshot` VIGENTE (que `apply_settings_write_result` ya
    /// actualizó cuando hubo config fresca) — mismo criterio que
    /// `SettingsState::refresh`: conserva la query/cursor/edición del
    /// usuario, solo recalcula los VALORES. No-op si la vista ya se cerró
    /// mientras la escritura estaba en vuelo (el usuario pulsó Esc antes de
    /// que la respuesta async llegara).
    fn set_settings_status(&mut self, message: String, error: bool) {
        if let Some(view) = &mut self.settings_view {
            view.status = Some(settings_view::SettingsStatus { message, error });
            view.state.refresh(norte_frontend::settings::build_rows(
                &self.cfg_snapshot,
                &self.plugin_config_summaries,
            ));
        }
    }

    /// Abre la paleta de comandos (`app.palette`, `ctrl+p`, G3c): las filas
    /// built-in nacen SÍNCRONAS (del `resolver` vigente), las de comando de
    /// plugin llegan ASYNC (`SessionCmd::PluginsList` → `PaletteView::extend`
    /// en `apply_event`) — mismo criterio "muestra algo YA, enriquece
    /// después" que `Decorate`/`Columns`. Reemplaza cualquier paleta
    /// anterior con una fresca.
    fn open_palette(&mut self) {
        self.palette = Some(palette_view::PaletteView::new(palette_view::build_rows(
            self.resolver.effective(),
        )));
        let _ = self.cmds.send(SessionCmd::PluginsList);
    }

    /// Maneja UNA tecla con la paleta abierta — mismo gate ctrl/alt/platform
    /// que [`Self::on_settings_key`] (la paleta también acepta tecleo
    /// libre). `Run(key)` distingue un comando built-in (`key` es un
    /// nombre de [`crate::keymap::COMMANDS`], se re-despacha vía
    /// [`Self::run_command`]) de una fila de plugin (`key` empieza con
    /// `"plugin:"`, JAMÁS un nombre de comando real — se envía
    /// `SessionCmd::PluginRunCommand`) — mismo criterio inequívoco que la
    /// TUI's `parse_plugin_key`.
    fn on_palette_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) {
        // `app.help` sobre una fila abre LA PÁGINA DE ESE COMANDO — la otra
        // dirección del puente que ya existía (desde la ayuda, `ctrl+p` lleva
        // el filtro a la paleta). Dos vistas del mismo modelo a dos
        // densidades: cruzar entre ellas no debería costar una re-lectura.
        //
        // Sin página no se abre NADA y se dice: aterrizar en el índice tras
        // preguntar por un comando concreto deja al lector sin saber si su
        // comando está ahí dentro o simplemente no está documentado. Una fila
        // de PLUGIN toma ese mismo camino — su clave es de un manifiesto de
        // terceros y ningún tema del corpus la documenta.
        if self.means_help(ks) {
            let target = self
                .palette
                .as_ref()
                .and_then(palette_view::PaletteView::selected_key)
                .filter(|k| parse_plugin_palette_key(k).is_none())
                .and_then(|k| norte_help::topic_for_command(norte_i18n::active(), k))
                .map(|t| t.id.clone());
            match target {
                Some(id) => {
                    self.palette = None;
                    self.open_help();
                    if let Some(view) = &mut self.help {
                        view.state.open_as_root(&id);
                    }
                }
                None => self.flash = Some((norte_i18n::t("msg-palette-no-help"), false)),
            }
            cx.notify();
            return;
        }
        if ks.modifiers.control || ks.modifiers.alt || ks.modifiers.platform {
            return;
        }
        let Some(view) = &mut self.palette else {
            return;
        };
        let outcome = palette_view::on_key(view, &ks.key, ks.key_char.as_deref());
        match outcome {
            palette_view::PaletteOutcome::None => {}
            palette_view::PaletteOutcome::Close => self.palette = None,
            palette_view::PaletteOutcome::Run(key) => {
                self.palette = None;
                if let Some((id, command)) = parse_plugin_palette_key(&key) {
                    let _ = self.cmds.send(SessionCmd::PluginRunCommand {
                        id: id.to_owned(),
                        command: command.to_owned(),
                        arg: String::new(),
                    });
                } else {
                    self.run_command(&key, cx);
                }
            }
        }
    }

    /// Body rows the help overlay shows at its fixed height, for the scroll
    /// arithmetic the model does in LINES.
    ///
    /// An estimate, deliberately: the panel is 520 px tall with a ~20 px line,
    /// and GPUI wraps long prose into rows this count cannot see. Being off
    /// costs a page key that scrolls slightly less than a screenful, or a
    /// revealed row landing a line or two from the edge — never a row the
    /// reader cannot reach, because `reveal` only ever scrolls TOWARDS it.
    /// Proporción de la ventana que ocupa el marco de la ayuda.
    ///
    /// Relativo y no fijo: un panel de 720×520 en una ventana de 2560 px es un
    /// sello en medio de una pared, y en una ventana pequeña se salía. Los
    /// topes de abajo evitan los dos extremos — una ayuda tan ancha que la
    /// prosa se lea en renglones de 200 caracteres, y una tan pequeña que no
    /// quepa nada.
    const HELP_FRAME_FRAC: f32 = 0.86;
    /// Ancho máximo del marco: más allá, la MEDIDA tipográfica sufre.
    const HELP_FRAME_MAX_W: f32 = 1180.0;
    /// Mínimos por debajo de los cuales el overlay deja de ser usable.
    const HELP_FRAME_MIN_W: f32 = 480.0;
    /// Mínimo de alto, por lo mismo.
    const HELP_FRAME_MIN_H: f32 = 320.0;

    /// El tamaño del marco de la ayuda para ESTE viewport.
    fn help_frame(&self, viewport: gpui::Size<gpui::Pixels>) -> (f32, f32) {
        let w = (f32::from(viewport.width) * Self::HELP_FRAME_FRAC)
            .clamp(Self::HELP_FRAME_MIN_W, Self::HELP_FRAME_MAX_W);
        let h = (f32::from(viewport.height) * Self::HELP_FRAME_FRAC).max(Self::HELP_FRAME_MIN_H);
        (w, h)
    }

    /// Cuántas filas caben DE VERDAD en la lateral y en el cuerpo.
    ///
    /// Derivado, no estimado, y ese es el arreglo: `HELP_BODY_ROWS` era 24 por
    /// aproximación y la ventana pinta menos, así que
    /// [`Self::sidebar_offset`] daba por visible un cursor que ya estaba fuera
    /// del panel — bajando por el índice con el teclado, el cursor
    /// desaparecía y no había forma de saber dónde estaba.
    ///
    /// El marco son [`Self::HELP_FRAME_H`] px menos la cabecera y el pie, que
    /// son una línea con su `py` cada uno, y el resto se reparte en filas de
    /// `row_h`. Suelo de 1: una ventana absurda (fuente gigante) tiene que
    /// dejar al menos una fila, o `skip`/`take` no enseñan nada.
    fn help_rows_for(&self, frame_h: f32) -> usize {
        let row_h = f32::from(self.fonts.row_h);
        // Cabecera, pie y TÍTULO del detalle miden EXACTAMENTE una fila cada
        // uno porque se lo fijamos al pintarlos, y cada fila de la lateral
        // mide una fila por lo mismo. Sin esas alturas definidas esto volvería
        // a ser una estimación, y una estimación de más esconde el cursor por
        // debajo del borde.
        let libre = frame_h - 3.0 * row_h;
        ((libre / row_h).floor() as usize).max(1)
    }

    /// Las filas que caben con el viewport VIGENTE.
    fn help_rows(&self, viewport: gpui::Size<gpui::Pixels>) -> usize {
        self.help_rows_for(self.help_frame(viewport).1)
    }

    /// Rows of the shortcut editor that FIT (K3c c4) — derived, exactly like
    /// [`Self::help_rows_for`] and for a sharper version of its reason.
    ///
    /// This screen is a full-view swap, so the frame is the viewport itself,
    /// minus the header and the footer (one row each, both given a definite
    /// height when painted) and minus whatever chrome `render` already put
    /// above this tree — today the start-up banner, `banner_rows`, which is
    /// the same correction `render_viewer` takes as `extra_chrome_rows`
    /// (K3a MAJOR-1).
    ///
    /// A constant here was a real defect and not a rounding one: with
    /// `[ui] font_size = 24` in a default 1000×640 window, twenty rows do not
    /// fit, `sidebar_offset` would call a cursor visible that is under the
    /// bottom edge, and `ctrl+u` deletes the binding under the cursor. Floor
    /// of one, for the same reason the help has one.
    fn shortcut_rows(&self, viewport: gpui::Size<gpui::Pixels>, banner_rows: usize) -> usize {
        let row_h = f32::from(self.fonts.row_h);
        #[allow(clippy::cast_precision_loss)] // a banner is single digits of rows
        let taken = (2.0 + banner_rows as f32) * row_h;
        let free = f32::from(viewport.height) - taken;
        ((free / row_h).floor() as usize).max(1)
    }

    /// Líneas por muesca de rueda en la ayuda.
    ///
    /// Tres, como el desplazamiento por defecto de casi cualquier lista: una
    /// sola línea obliga a girar la rueda una eternidad en una página larga, y
    /// una pantalla entera por muesca convierte la rueda en `PgDn`.
    const HELP_WHEEL_LINES: isize = 3;

    /// The facts the help overlay is frozen against: the same table the
    /// context menu builds from, seeded from the FOCUSED pane's cursor.
    ///
    /// Same syntactic read-only criterion the context menu uses — the GUI does
    /// not cache `Capabilities` per connection, so the scheme is the answer
    /// here and not a fallback. An empty pane has nothing under the cursor, and
    /// the honest answer there is "no impediment known" rather than a made-up
    /// entry kind: the table is a list of known impediments, and it fails open.
    fn help_facts(&self) -> norte_frontend::availability::Facts {
        let f = self.focus;
        let Some(entry) = self.panes[f].entries().get(self.panes[f].cursor()) else {
            return help_view::NO_IMPEDIMENT;
        };
        let count = self.panes[f].marks_len().max(1);
        context_menu::facts_for(
            entry.kind,
            count,
            context_menu::ReadOnly {
                source: scheme_is_read_only(self.panes[f].dir().scheme()),
                dest: scheme_is_read_only(self.panes[1 - f].dir().scheme()),
            },
            self.journalled,
        )
    }

    /// Opens the help on the index (`app.help`, `F1`, H3f).
    ///
    /// The resolver is frozen HERE, with the facts of this moment: a page whose
    /// rows change verdict while the reader walks it disagrees with itself, and
    /// Enter would then run something the page shows dimmed.
    ///
    /// The plugin catalogue is asked for async, exactly as the palette does:
    /// the corpus pages are there instantly and the Extensions group fills in
    /// when `plugin.list` answers. A catalogue that never arrives leaves that
    /// group empty and every `plugin:` row dimmed — the honest answer to "I
    /// could not find out", and fail-closed is the direction to fail in.
    /// Dónde está el lector, en el vocabulario CERRADO de contextos del corpus.
    ///
    /// El equivalente del `help_context` de la TUI, con su misma disciplina: un
    /// `match` sin comodín sobre `Modal`, para que un modal nuevo no compile
    /// hasta que alguien decida qué página lo explica. Los ids son los del
    /// corpus, no de esta GUI: quien decide qué explica cada pantalla es la
    /// prosa, no el frontend.
    fn help_context(&self) -> &'static str {
        if let Some(m) = &self.modal {
            return match m {
                // Borrar y transferir son la MISMA pregunta ("¿toco estos
                // ficheros?"); salir no lo es —no muta nada— y tiene id propio.
                Modal::ConfirmTransfer { .. } | Modal::ConfirmDelete { .. } => "dialog.confirm",
                Modal::ConflictResolve { .. } => "dialog.collision",
                Modal::ConfirmQuit { .. } => "dialog.quit",
                // El nombre editable de una transferencia y el renombrado son
                // el mismo diálogo.
                Modal::RenamePrompt { .. } => "dialog.transfer-name",
                // Prompt y plan son dos pasos de UNA función, y una página
                // explica los dos: partirlos pediría media página cada uno.
                Modal::AiRenamePrompt { .. } | Modal::AiRenamePlan { .. } => "dialog.ai-rename",
                Modal::SemanticQuery { .. } | Modal::SemanticHits { .. } => {
                    "dialog.semantic-search"
                }
                // The volumes picker (2026-08-10-volumes.md task V4) has no
                // `Modal` counterpart in the TUI at all — there it is a
                // separate `NavPopup` overlay, outside `help_context.rs`'s
                // closed vocabulary entirely (history/hotlist share the same
                // exemption). `panes.md`, the topic `pane.select-drive*`'s
                // own Fluent help ids live under, declares `context =
                // ["browse"]` rather than a dedicated dialog id — the picker
                // is a quick, non-destructive overlay over ordinary
                // browsing, not a question with its own page the way a
                // delete confirmation is. Reusing `"browse"` here keeps `F1`
                // landing on that same page instead of inventing a context
                // id nothing else in the corpus would ever ask for.
                Modal::Volumes { .. } => "browse",
            };
        }
        if self.viewer.is_some() {
            return "viewer";
        }
        "browse"
    }

    /// Abre la ayuda por la página que explica DONDE ESTÁ el lector.
    ///
    /// Sobre un modal, un contexto sin página no abre NADA y lo dice: tapar una
    /// pregunta viva con el índice —«Esto es norte. Dos paneles…»— le roba las
    /// teclas al diálogo para contarle al lector algo que no preguntó. Desde un
    /// pane o el visor, en cambio, el índice es un aterrizaje razonable: allí
    /// nadie está esperando una decisión. Mismo criterio que la TUI.
    /// ¿Esta pulsación es `app.help` según el keymap VIGENTE?
    ///
    /// La usan las dos mitades del mismo interruptor —abrir sobre un modal y
    /// cerrar desde dentro— y por eso pregunta al keymap y no a un resolver
    /// congelado: la tecla que cuenta es la que el lector tiene ahora.
    fn means_help(&self, ks: &gpui::Keystroke) -> bool {
        crate::keymap::means_command(
            self.resolver.effective(),
            "app.help",
            &ks.key,
            keymap_mods(ks.modifiers),
            ks.key_char.as_deref(),
        )
    }

    fn open_help(&mut self) {
        let lang = norte_i18n::active();
        let context = self.help_context();
        let over_modal = self.modal.is_some();
        let page = norte_help::topic_for_context(lang, context);
        if over_modal && page.is_none() {
            self.flash = Some((norte_i18n::t("msg-help-no-dialog-page"), false));
            return;
        }
        let mut view = help_view::HelpView::new(
            lang,
            help_view::keys_lines(
                self.resolver.effective(),
                self.viewer_resolver.effective(),
                &self.dialog_effective,
            ),
        );
        view.over_modal = over_modal;
        if let Some(topic) = page {
            // Como RAÍZ del rastro: un `Esc` sale, en vez de dejar al lector
            // caminando hacia atrás hasta un índice que nunca pidió.
            view.state.open_as_root(&topic.id);
        }
        let base = help_view::GuiChords::new(
            self.resolver.effective(),
            self.viewer_resolver.effective(),
            lang,
        );
        self.help_chords = Some(view.freeze(&base, self.help_facts()));
        self.help = Some(view);
        // The catalogue arrives async (`SessionEvent::PluginsListed`), which
        // re-installs the snapshot and re-freezes the PLUGIN half only. The GUI
        // does not cache a plugin list of its own — the palette and the
        // extension manager ask for it when they open, and so does this.
        //
        // Un hot-reload de keymap con el overlay abierto SÍ lo alcanza desde
        // H3h: `apply_keymap_live` llama a `rebuild_help_chords`, que rehace
        // estos dos snapshots arrastrando los hechos congelados.
        let _ = self.cmds.send(SessionCmd::PluginsList);
    }

    /// Re-installs the plugin snapshot on the open help and re-freezes its
    /// resolver — the answer to the `plugin.list` [`Self::open_help`] sent.
    ///
    /// Both halves together: a view holding a fresh catalogue beside a
    /// resolver that never saw it would offer rows for plugins it then dims,
    /// and name their commands by their raw dispatch keys.
    fn install_help_plugins(&mut self, plugins: &[norte_proto::methods::PluginInfo]) {
        // Only the PLUGIN half is re-frozen. Re-reading the facts here would
        // silently replace the ones captured when the overlay opened, so rows
        // the reader is looking at would change verdict the moment a catalogue
        // answered — which is the exact promise the frozen resolver exists to
        // keep. The facts ride along untouched (`with_plugins` carries them),
        // the same split the TUI settled on.
        //
        // Nothing to do with the help closed, and this event also fires for the
        // palette and the extension manager: the guard comes first so the
        // common case does not rebuild a keymap-wide resolver for nobody.
        if self.help.is_none() {
            return;
        }
        let base = help_view::GuiChords::new(
            self.resolver.effective(),
            self.viewer_resolver.effective(),
            norte_i18n::active(),
        );
        let facts = self
            .help_chords
            .as_ref()
            .map_or_else(|| self.help_facts(), help_view::GuiChords::facts);
        if let Some(view) = &mut self.help {
            view.set_plugins(plugins);
            self.help_chords = Some(view.freeze(&base, facts));
        }
    }

    /// Handles ONE key with the help open — same ctrl/alt/platform gate as
    /// [`Self::on_palette_key`], with `ctrl+p` intercepted BEFORE it as the
    /// bridge into the palette (the pure router never sees a modifier).
    fn on_help_key(&mut self, ks: &gpui::Keystroke, window: &Window, cx: &mut Context<Self>) {
        // La tecla que ABRE la ayuda la CIERRA, y la decide el keymap
        // (`help_view::closes_help`), no una `f1` escrita en el router. Va
        // ANTES del gate de modificadores de abajo: ese gate existe para que
        // un chord con ctrl/alt no se teclee en el filtro, no para impedir que
        // cierre el overlay — un `app.help` en `ctrl+h` es perfectamente
        // legal. Y antes también del puente `ctrl+p`, que jamás puede ser
        // `app.help` (`ctrl+p` es `app.palette` en los tres presets) pero cuyo
        // orden no debería depender de eso.
        if self.means_help(ks) {
            self.close_help();
            cx.notify();
            return;
        }
        if ks.modifiers.control && ks.key == "p" {
            let out = self.help.as_ref().map(help_view::handoff);
            if let Some(help_view::HelpOutcome::Palette(filter)) = out {
                self.close_help();
                self.open_palette();
                if let Some(p) = &mut self.palette {
                    for c in filter.chars() {
                        p.push_char(c);
                    }
                }
            }
            return;
        }
        if ks.modifiers.control || ks.modifiers.alt || ks.modifiers.platform {
            return;
        }
        let Some(chords) = self.help_chords.clone() else {
            return;
        };
        let Some(view) = &mut self.help else {
            return;
        };
        // The footer's answer lives until the NEXT key, like the app's flash:
        // it answers one question and must not outlive it.
        view.status = None;
        let outcome = help_view::on_key(view, &ks.key, ks.key_char.as_deref(), &chords);
        match outcome {
            help_view::HelpOutcome::None => {}
            // `handoff` is the only producer of `Palette`, and it is handled
            // above, before the modifier gate — the pure router cannot return
            // it. Kept as an explicit arm rather than folded into `None` so a
            // future producer has to decide what it means here.
            help_view::HelpOutcome::Palette(_) => {}
            help_view::HelpOutcome::Close => self.close_help(),
            help_view::HelpOutcome::Run(key) => {
                // Same dispatch the palette uses, built-in or plugin: one path,
                // so nothing here can bypass policy or plugin approval.
                self.close_help();
                if let Some((id, command)) = parse_plugin_palette_key(&key) {
                    let _ = self.cmds.send(SessionCmd::PluginRunCommand {
                        id: id.to_owned(),
                        command: command.to_owned(),
                        arg: String::new(),
                    });
                } else {
                    self.run_command(&key, cx);
                }
            }
            help_view::HelpOutcome::Blocked(reason) => {
                // The dimming raised a question; this answers it in the same
                // wording the context menu uses for the same veto — in the
                // overlay's OWN footer, because the window-level flash is
                // painted under this overlay's scrim.
                view.status = Some(norte_i18n::t(norte_frontend::availability::reason_key(
                    reason,
                )));
            }
        }
        // The model moves the scroll and the focus in LINES, and only the
        // painter knows how many there are: re-establish both invariants after
        // every key, the way the TUI's `HelpView::refresh` does before every
        // paint. Computed after the borrow above ends.
        let (total, focused_line) = self.help.as_ref().map_or((0, None), |v| {
            let lines = self.help_body(v);
            // Which LINE the focused action landed on is a fact of the layout,
            // so ask the layout rather than deriving it from the action index:
            // prose between two rows makes the two disagree.
            let focused = (v.state.focus() == norte_frontend::help::Focus::Body)
                .then(|| {
                    lines
                        .iter()
                        .position(|l| l.action == Some(v.state.action_cursor()))
                })
                .flatten();
            (lines.len(), focused)
        });
        let rows = self.help_rows(window.viewport_size());
        if let Some(view) = &mut self.help {
            view.state.clamp_scroll(total);
            if let Some(line) = focused_line {
                view.state.reveal(line, rows);
            }
        }
        self.fetch_help_page();
    }

    /// Re-freezes the open help against the CURRENT facts, keeping its plugin
    /// snapshot.
    ///
    /// Called where the panes are re-listed, and nowhere else. The freeze is
    /// about the READER not moving the world, not about the world standing
    /// still: two of the facts describe the entry under the cursor, and a task
    /// finishing underneath the overlay re-lists both panes, so a page left
    /// open would otherwise explain a selection that is gone.
    fn refreeze_help_facts(&mut self) {
        if self.help.is_none() {
            return;
        }
        let facts = self.help_facts();
        if let Some(chords) = self.help_chords.take() {
            self.help_chords = Some(chords.with_facts(facts));
        }
    }

    /// Closes the overlay and drops the frozen resolver with it — the two are
    /// one photograph, and a resolver outliving its page would freeze the NEXT
    /// one against facts nobody looked at.
    /// Un clic sobre una fila ejecutable de la página, o sobre un enlace.
    ///
    /// Mueve el cursor de acciones y ACTIVA, por el mismo `help_view::activate`
    /// que usa `Enter` — incluida la rama de fila atenuada, que contesta en el
    /// pie del propio overlay porque el flash de ventana se pinta debajo de su
    /// scrim. El despacho de un comando es el de la paleta, como en la rama de
    /// teclado: un solo camino, así que nada de esto puede saltarse la política
    /// ni la aprobación de un plugin.
    fn on_help_action_click(&mut self, action: usize, cx: &mut Context<Self>) {
        let Some(chords) = self.help_chords.clone() else {
            return;
        };
        let outcome = {
            let Some(view) = &mut self.help else {
                return;
            };
            view.status = None;
            view.state.click_action(action);
            help_view::activate(view, &chords)
        };
        match outcome {
            help_view::HelpOutcome::None | help_view::HelpOutcome::Palette(_) => {}
            help_view::HelpOutcome::Close => self.close_help(),
            help_view::HelpOutcome::Run(key) => {
                self.close_help();
                if let Some((id, command)) = parse_plugin_palette_key(&key) {
                    let _ = self.cmds.send(SessionCmd::PluginRunCommand {
                        id: id.to_owned(),
                        command: command.to_owned(),
                        arg: String::new(),
                    });
                } else {
                    self.run_command(&key, cx);
                }
            }
            help_view::HelpOutcome::Blocked(reason) => {
                if let Some(view) = &mut self.help {
                    view.status = Some(norte_i18n::t(norte_frontend::availability::reason_key(
                        reason,
                    )));
                }
            }
        }
        cx.notify();
    }

    fn close_help(&mut self) {
        self.help = None;
        // Un arrastre no puede sobrevivir a lo que estaba arrastrando: sin
        // esto, cerrar con el botón pulsado dejaba el flag puesto y el
        // siguiente movimiento sobre la ayuda REABIERTA la desplazaba sola.
        self.help_dragging = false;
        self.help_chords = None;
    }

    /// Asks for the page of the plugin node the reader just opened, once
    /// ([`help_view::HelpView::claim_plugin_fetch`]).
    ///
    /// On demand: 64 KiB per plugin must not ride every `plugin.list`. The
    /// claim is what makes this callable after every key without re-asking a
    /// daemon that cannot answer.
    fn fetch_help_page(&mut self) {
        if let Some(id) = self
            .help
            .as_mut()
            .and_then(help_view::HelpView::claim_plugin_fetch)
        {
            let _ = self.cmds.send(SessionCmd::PluginHelp { id });
        }
    }

    /// Abre el picker de columnas (`pane.columns`, `alt+c`, #108 7c) sobre
    /// el scheme y el sort VIVO del pane enfocado (el sort del pane ya
    /// pliega `sort_override`, misma semilla que la TUI). Con el catálogo
    /// cacheado del scheme (#117): el picker OFRECE los attrs anunciados
    /// por el provider y cicla sus formatos por hint.
    fn open_columns_picker(&mut self) {
        // Pide el catálogo: se abre con lo cacheado (posiblemente nada la
        // primera vez) y `PluginsListed` reconstruye el picker al llegar.
        let _ = self.cmds.send(SessionCmd::PluginsList);
        let f = self.focus;
        let scheme = self.panes[f].dir().scheme().to_owned();
        let sort = self.panes[f].sort();
        self.columns_picker = Some(columns_view::ColumnsView::new(
            norte_frontend::columns_picker::ColumnsPicker::open_with_catalog(
                &self.column_settings,
                &scheme,
                sort,
                self.attr_catalogs.get(&scheme),
                &self.plugins,
            ),
        ));
    }

    /// Maneja UNA tecla con el picker de columnas abierto. A diferencia de
    /// la paleta, aquí SOLO se gatea platform/alt: shift es reorden y ctrl+s
    /// es sinónimo de sort (paridad TUI).
    fn on_columns_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) {
        if ks.modifiers.platform || ks.modifiers.alt {
            return;
        }
        let Some(view) = &mut self.columns_picker else {
            return;
        };
        match view.on_key(ks.key.as_str(), ks.modifiers.shift, ks.modifiers.control) {
            columns_view::ColumnsOutcome::None => {}
            columns_view::ColumnsOutcome::Close => self.columns_picker = None,
            columns_view::ColumnsOutcome::Apply(picked) => {
                self.columns_picker = None;
                self.apply_picked_columns(picked, cx);
            }
        }
    }

    /// Los ids attr CONFIGURADOS de cada pane (#117): la huella que decide
    /// si un cambio de columnas exige re-listar — los valores attr solo
    /// llegan pidiéndolos en `fs.list`, así que un id nuevo con el listado
    /// viejo pintaría blanco (ausencia) hasta el próximo cd. La huella
    /// ordenada vive en el modelo (`attr_fingerprint`, review tarea 3):
    /// una única definición para ambos frontends.
    fn pane_attr_ids(&self) -> [Vec<String>; 2] {
        // #117-follow-up (review MAJOR-1): huella COMBINADA attr+plugin,
        // única definición en el modelo (`pane_fingerprint`) para ambos
        // frontends — un cambio solo de plugins también re-lista.
        std::array::from_fn(|i| {
            self.column_settings
                .pane_fingerprint(self.panes[i].dir().scheme())
        })
    }

    /// Aplica el resultado del picker (#108 7c): SESIÓN primero (settings
    /// compartidos + formatos + re-seed del sort por pane + re-list de los
    /// panes cuyo set de attrs pintado cambió, #117), DISCO después
    /// (un solo background task: `persist_columns` + N×
    /// `persist_column_format` — regla 2, precedente
    /// `commit_settings_write`: nada de I/O en el hilo de render). El sort
    /// persistido SUPERSEDE el click de sesión: `sort_override` se limpia
    /// en los panes cuyo scheme cubre el guardado (deuda del bloque 6).
    fn apply_picked_columns(
        &mut self,
        picked: norte_frontend::columns_picker::Picked,
        cx: &mut Context<Self>,
    ) {
        let attrs_before = self.pane_attr_ids();
        self.column_settings.apply_picked(
            picked.scheme_target.as_deref(),
            &picked.ids,
            picked.sort,
        );
        for (id, fmt) in &picked.formats {
            self.column_settings.apply_format(id, fmt);
        }
        for pane in 0..self.panes.len() {
            let scheme_matches = picked
                .scheme_target
                .as_deref()
                .is_none_or(|s| self.panes[pane].dir().scheme() == s);
            if scheme_matches {
                // Con guardado GLOBAL, un pane cuyo scheme tiene sort propio
                // re-siembra a ESE sort (el global queda enmascarado) y aun
                // así pierde su click de sesión — PARIDAD deliberada con la
                // TUI (apply_scheme_sort barre todos los panes); review 7c
                // MINOR-3 lo registra como decisión, no bug.
                self.sort_override[pane] = None;
                let spec = self
                    .column_settings
                    .sort_for(self.panes[pane].dir().scheme());
                self.panes[pane].set_sort(spec);
            }
        }
        // #117: los panes cuyo set de attrs PINTADO cambió se re-listan por
        // el mismo camino que un read-after-write (`refresh_dir`: bump de
        // generación + `refill`, que conserva las marcas). ANTES del persist
        // — un fallo de disco no debe dejar celdas attr en blanco.
        let attrs_after = self.pane_attr_ids();
        for pane in 0..self.panes.len() {
            if attrs_after[pane] != attrs_before[pane] {
                let dir = self.panes[pane].dir().clone();
                self.refresh_dir(pane, dir, cx);
            }
        }
        let Some(dir) = norte_config::user_config_dir() else {
            self.flash = Some((norte_i18n::t("msg-settings-no-config-dir"), true));
            return;
        };
        let scheme = picked.scheme_target.clone();
        let ids = picked.ids.clone();
        let sort = picked.sort;
        let formats = picked.formats.clone();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_spawn(async move {
                    // Escrituras de norte.toml SERIALIZADAS (review 7c
                    // MAJOR-1); ver CONFIG_WRITE_SERIAL.
                    let _guard = CONFIG_WRITE_SERIAL
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    norte_config::persist_columns(
                        &dir,
                        scheme.as_deref(),
                        &ids,
                        norte_config::PersistSort {
                            column: match sort.column {
                                norte_frontend::SortColumn::Name => "name",
                                norte_frontend::SortColumn::Size => "size",
                                norte_frontend::SortColumn::Mtime => "mtime",
                            },
                            descending: sort.dir == norte_frontend::SortDir::Desc,
                            dirs_first: sort.dirs_first,
                        },
                    )?;
                    for (id, fmt) in &formats {
                        norte_config::persist_column_format(&dir, id, fmt)?;
                    }
                    Ok::<(), std::io::Error>(())
                })
                .await;
            this.update(cx, |view, cx| {
                view.flash = Some(match outcome {
                    Ok(()) => (norte_i18n::t("msg-columns-saved"), false),
                    // Categoría del error, jamás el Display crudo del OS
                    // (mismo criterio que apply_settings_write_result).
                    Err(e) => (
                        norte_i18n::ta(
                            "msg-settings-save-failed",
                            &[("error", &io_error_category(&e))],
                        ),
                        true,
                    ),
                });
                cx.notify();
            })
        })
        .detach();
    }

    /// Abre el gestor de extensiones (`app.extensions`, `f12`, G3c): nace
    /// en estado "cargando" (`ExtensionsView::loading`) — el catálogo
    /// llega ASYNC (`SessionCmd::PluginsList` → `apply_event`,
    /// `SessionEvent::PluginsListed`). Reemplaza cualquier vista anterior.
    fn open_extensions(&mut self) {
        self.extensions = Some(extensions_view::ExtensionsView::loading());
        let _ = self.cmds.send(SessionCmd::PluginsList);
    }

    /// Maneja UNA tecla con el gestor de extensiones abierto — mismo gate
    /// ctrl/alt/platform que [`Self::on_settings_key`]. Delega en
    /// [`extensions_view::on_key`] (puro) y traduce su
    /// [`extensions_view::ExtensionsOutcome`] a comandos async por el
    /// canal de sesión — nunca I/O directa aquí (regla 2).
    fn on_extensions_key(&mut self, ks: &gpui::Keystroke, _cx: &mut Context<Self>) {
        if ks.modifiers.control || ks.modifiers.alt || ks.modifiers.platform {
            return;
        }
        let Some(view) = &mut self.extensions else {
            return;
        };
        let outcome = extensions_view::on_key(view, &ks.key, ks.key_char.as_deref());
        match outcome {
            extensions_view::ExtensionsOutcome::None => {}
            extensions_view::ExtensionsOutcome::Close => self.extensions = None,
            extensions_view::ExtensionsOutcome::RequestApprove { id, approved } => {
                let _ = self
                    .cmds
                    .send(SessionCmd::PluginSetApproval { id, approved });
            }
            extensions_view::ExtensionsOutcome::RequestEnable { id, enabled } => {
                let _ = self.cmds.send(SessionCmd::PluginSetEnabled { id, enabled });
            }
            extensions_view::ExtensionsOutcome::RequestConfig { id } => {
                let _ = self.cmds.send(SessionCmd::PluginGetConfig { id });
            }
            extensions_view::ExtensionsOutcome::RequestConfigWrite { plugin_id, write } => {
                let _ = self.cmds.send(SessionCmd::PluginSetConfig {
                    id: plugin_id,
                    key: write.key,
                    value: write.value,
                });
            }
            extensions_view::ExtensionsOutcome::Invalid(e) => {
                self.errors[self.focus] = Some(norte_frontend::settings::edit_error_message(&e));
            }
            // Absorbido dentro de `extensions_view::on_key` (cierra el
            // panel, no la vista) — nunca surge hasta aquí en la práctica,
            // pero el match debe ser exhaustivo.
            extensions_view::ExtensionsOutcome::CloseConfigPanel => {}
        }
    }

    /// Abre el visor sobre la entrada seleccionada si es un archivo (F3 sobre
    /// un dir/symlink/otro = no-op — el visor solo lee archivos). Avanza
    /// `viewer_gen` (invalida cualquier open anterior en vuelo — dos F3
    /// seguidos no encolan dos aperturas) y marca `viewer_loading` para el
    /// estado «abriendo visor…» del render mientras llega la respuesta.
    fn open_viewer(&mut self, _cx: &mut Context<Self>) {
        let f = self.focus;
        // El path se saca del pane ANTES de tocar nada: `close_compare` de
        // abajo necesita `&mut self` y la entrada seguía prestada.
        let Some(path) = self.panes[f]
            .selected()
            .filter(|e| e.kind == EntryKind::File)
            .map(|e| e.path.clone())
        else {
            return;
        };
        // El visor y el panel de diferencias son EXCLUYENTES, y por eso se
        // cierra el que hubiera (revisión rust BLOCKER-1). Los dos sustituyen
        // a la pantalla entera, así que con ambos vivos uno se pinta y el
        // otro se queda el teclado —la GUI llegó a tener exactamente eso, y
        // un `Enter` invisible hacía un `cd` que se lleva por delante las
        // marcas del pane destino—. La exclusión se hace en los DOS sitios
        // que pueden abrir: aquí y en `SessionEvent::CompareStarted`.
        self.close_compare();
        self.viewer_gen = self.viewer_gen.wrapping_add(1);
        self.viewer_loading = true;
        let _ = self.cmds.send(SessionCmd::OpenViewer {
            path,
            generation: self.viewer_gen,
        });
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
            // Las mismas cuatro líneas por las que existe `close_viewer`
            // (revisión de rama, MINOR-5): una sola definición de «soltar el
            // visor», o el día que sea cinco líneas lo será en un sitio.
            self.close_viewer();
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
                // nombre, no como carácter suelto (`keys::typed_char`).
                match keys::typed_char(&ks.key, ks.key_char.as_deref()) {
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
        let ch =
            keys::single_char(ks.key_char.as_deref()).or_else(|| keys::single_char(Some(&ks.key)));
        if let Some(c) = ch
            && c.is_alphanumeric()
        {
            self.panes[f].quick_start(self.quick_mode);
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

        // El flash (aviso transitorio de una línea, #108 7c) vive hasta la
        // SIGUIENTE tecla: cualquier pulsación lo despide.
        self.flash = None;

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

        // La ayuda ABIERTA SOBRE un modal se queda con las teclas, y va ANTES
        // de la rama del modal por eso: mientras el lector lee la página que
        // explica la pregunta, los verbos de la pregunta son inalcanzables —
        // que es la garantía, no un efecto colateral: nada se confirma a
        // través de una página que lo tapa. El modal se sigue pintando debajo
        // (este overlay se dibuja el último), así que la pregunta nunca
        // desaparece; solo espera.
        if self
            .help
            .as_ref()
            .is_some_and(|v| v.over_modal && self.modal.is_some())
        {
            self.on_help_key(ks, window, cx);
            cx.notify();
            return;
        }
        // Y `app.help` ALCANZA a abrirla aunque haya un modal delante: es la
        // tecla con la que se pregunta «¿qué es esto que me está preguntando?»,
        // y hasta aquí la comía la captura del modal.
        if self.modal.is_some() && self.means_help(ks) {
            self.open_help();
            cx.notify();
            return;
        }

        // Con un modal abierto, la tecla va al modal (captura fija).
        if let Some(m) = &mut self.modal {
            // Los prompts IA/semántico aceptan tecleo libre: un
            // ctrl/alt/super-chord no debe teclearse en el buffer (mismo
            // gate que `on_settings_key`; los modales de decisión conservan
            // su comportamiento previo).
            if matches!(
                m,
                Modal::AiRenamePrompt { .. }
                    | Modal::SemanticQuery { .. }
                    | Modal::RenamePrompt { .. }
            ) && (ks.modifiers.control || ks.modifiers.alt || ks.modifiers.platform)
            {
                cx.notify();
                return;
            }
            // Cuántos renames anunciará el banner si esta tecla confirma:
            // los pasos REALES del lote (§17), no las parejas pedidas — el
            // planificador tira las nulas (`from == to`) y prometer más de lo
            // que va a pasar es mentir. Se lee ANTES de `on_key`, que cierra
            // el modal.
            let ai_batch_renames = match &*m {
                Modal::AiRenamePlan { plan, .. } => Some(plan.real_steps()),
                _ => None,
            };
            match modal::on_key(m, &ks.key, ks.key_char.as_deref()) {
                ModalOutcome::Ignored | ModalOutcome::StayOpen => {}
                ModalOutcome::Dismiss => {
                    self.modal = None;
                    self.drain_pending_modals();
                }
                ModalOutcome::Submit(ops) => {
                    self.modal = None;
                    if let Some(renames) = ai_batch_renames {
                        // Paridad TUI (`msg-rename-batch-applied`): el banner
                        // resume cuántos renames salieron — del LOTE, no de
                        // `ops`, que ahora es UNA op para todos ellos (§17).
                        let n = renames.to_string();
                        self.errors[self.focus] = Some(norte_i18n::ta(
                            "msg-rename-batch-applied",
                            &[("n", n.as_str())],
                        ));
                    }
                    for op in ops {
                        let _ = self.cmds.send(SessionCmd::Submit(op));
                    }
                    self.drain_pending_modals();
                }
                ModalOutcome::Quit => cx.quit(),
                ModalOutcome::RequestAiPlan { dir, instruction } => {
                    self.modal = None;
                    let _ = self
                        .cmds
                        .send(SessionCmd::AiRenamePlan { dir, instruction });
                    // Clave gui-* propia: la de la TUI promete "Esc cancela"
                    // y la GUI (hoy) no tiene camino para abortar la petición
                    // en vuelo — jamás una affordance falsa.
                    self.errors[self.focus] = Some(norte_i18n::t("gui-msg-ai-rename-running"));
                    self.drain_pending_modals();
                }
                ModalOutcome::InvalidPlan => {
                    // Cinturón fail-loud (paridad TUI audit MAJOR-2): plan
                    // adulterado — NADA se sometió.
                    self.modal = None;
                    self.errors[self.focus] = Some(norte_i18n::t("msg-ai-rename-invalid-plan"));
                    self.drain_pending_modals();
                }
                ModalOutcome::RequestSemantic { query } => {
                    self.modal = None;
                    // Invariante anti-stale (lección TUI Task 8): una nueva
                    // consulta INVALIDA cualquier hit retenido de la
                    // anterior — sin esto, unos hits viejos esperando turno
                    // se abrirían como si respondieran a ESTA consulta.
                    self.pending_semantic = None;
                    let _ = self.cmds.send(SessionCmd::SemanticSearch { query });
                    // Clave gui-* propia (misma doctrina que
                    // `gui-msg-ai-rename-running`): la de la TUI promete
                    // "Esc cancela" y la GUI no tiene camino para abortar la
                    // petición en vuelo — jamás una affordance falsa.
                    self.errors[self.focus] = Some(norte_i18n::t("gui-msg-semantic-running"));
                    self.drain_pending_modals();
                }
                ModalOutcome::NavigateTo(path) => {
                    self.modal = None;
                    // Navega a la UBICACIÓN del hit: cd al padre con el
                    // cursor pendiente sobre la entrada (mismo mecanismo que
                    // `nav.parent`, spec 2026-07-24 §S1: `set_pending_focus`
                    // se consume al aterrizar el listado — byte-exacto, y si
                    // el hit ya no existe el cursor cae al default). Un hit
                    // raíz sin padre no navega (paridad TUI `Cd::Cancelled`).
                    if let Some(parent) = path.parent() {
                        let f = self.focus;
                        self.panes[f].set_pending_focus(path);
                        self.cd(f, parent, cx);
                    }
                    self.drain_pending_modals();
                }
                ModalOutcome::NavigateToPane { pane, target } => {
                    self.modal = None;
                    // Unlike `NavigateTo`, `target` IS the destination
                    // itself (a volume's mount point, not an entry inside
                    // one) — a straight `cd`, no parent/pending-focus dance.
                    // `pane` is the LADO the modal froze at open time (design
                    // §D), not necessarily `self.focus` now.
                    self.cd(pane, target, cx);
                    self.drain_pending_modals();
                }
                ModalOutcome::RequestVolumes {
                    pane,
                    include_pseudo,
                } => {
                    self.modal = None;
                    self.request_volumes(pane, include_pseudo);
                    self.drain_pending_modals();
                }
            }
            cx.notify();
            return;
        }

        // Menú contextual abierto (botón derecho, tarea 4 del plan de ratón):
        // captura fija como cualquier overlay. Va justo tras el modal porque
        // es lo más reciente que el usuario abrió y porque no puede coexistir
        // con las demás pantallas (`open_context_menu` no abre con ninguna
        // delante, y activar una entrada cierra el menú antes de despachar).
        if self.context_menu.is_some() {
            self.on_context_menu_key(ks, cx);
            cx.notify();
            return;
        }

        // Editor de atajos abierto (K3c c4, `ctrl+k` desde ajustes): se pinta
        // POR ENCIMA de la vista de ajustes (que sigue abierta detrás), así
        // que también se queda las teclas ANTES que ella. En modo captura son
        // TODAS suyas — eso es lo que significa capturar.
        if self.shortcuts_view.is_some() {
            self.on_shortcuts_key(ks, cx);
            cx.notify();
            return;
        }

        // Vista de ajustes abierta (F11, S4): captura fija, mismo criterio de
        // prioridad que el modal — gana incluso sobre el visor (ver doc del
        // campo `settings_view`).
        if self.settings_view.is_some() {
            self.on_settings_key(ks, cx);
            cx.notify();
            return;
        }

        // Gestor de extensiones abierto (F12, G3c): MISMA prioridad de
        // captura fija que la vista de ajustes (mutuamente excluyentes por
        // construcción — `open_extensions`/`open_settings` nunca se llaman
        // con la otra ya abierta).
        if self.extensions.is_some() {
            self.on_extensions_key(ks, cx);
            cx.notify();
            return;
        }

        // Picker de columnas abierto (alt+c, #108 7c): overlay como la
        // paleta, captura fija — nada cae al dual-pane de abajo.
        if self.columns_picker.is_some() {
            self.on_columns_key(ks, cx);
            cx.notify();
            return;
        }

        // Ayuda abierta (`F1`, H3f): overlay con la MISMA prioridad de
        // captura que la paleta — el modal, comprobado arriba, sigue ganando.
        // Va ANTES de la paleta porque `ctrl+p` desde la ayuda ABRE la paleta
        // (el puente del filtro), y con el orden inverso esa tecla nunca
        // llegaría aquí.
        if self.help.is_some() {
            self.on_help_key(ks, window, cx);
            cx.notify();
            return;
        }

        // Paleta de comandos abierta (`ctrl+p`, G3c): un OVERLAY, no un
        // full-view swap (se pinta encima del dual-pane, ver `render`) —
        // pero captura teclado con la MISMA prioridad que ajustes/
        // extensiones (el modal, comprobado ARRIBA de todo, sigue ganando
        // — mismo "modal preempts palette" que la TUI's
        // `modal_preempts_palette`).
        if self.palette.is_some() {
            self.on_palette_key(ks, cx);
            cx.notify();
            return;
        }

        // Visor abierto: las teclas van al contexto Viewer (no hay modal/quick
        // aquí — el visor y el dual-pane son pantallas mutuamente excluyentes).
        if self.viewer.is_some() {
            // K1 T5: aquí había el MISMO early-return sobre `platform` que el
            // del dual-pane de abajo, y por la misma razón (⌘ no tenía dónde
            // ir y la tecla colapsaba al chord desnudo). Ahora `Mods` tiene
            // `cmd`, así que ⌘ viaja hasta el resolver y, si nada lo liga,
            // sale `Reset` — que es lo que hace cualquier otra tecla suelta.
            // La ayuda ALCANZA al visor. El visor resuelve por su propio
            // contexto y `app.help` no vive en él, así que la tecla se perdía:
            // la única pantalla desde la que no se podía preguntar era la que
            // tiene su propio juego de teclas que aprender.
            if self.means_help(ks) {
                self.open_help();
                cx.notify();
                return;
            }
            if let Some(chord) =
                keymap::gpui_chord(&ks.key, keymap_mods(ks.modifiers), ks.key_char.as_deref())
            {
                match self.viewer_resolver.push(chord) {
                    norte_frontend::keymap::Resolution::Run {
                        command: cmd,
                        count,
                    } => {
                        // K3a: la secuencia terminó — con ella, el panel.
                        self.which_key = None;
                        // K2a: el contador repite el DESPACHO (ninguna firma
                        // de comando cambia). `viewer.up/down/page-*` son los
                        // que lo aceptan; el resto llega como `Ignored`. K3a
                        // pagó la deuda de la rama `Unavailable` de abajo: el
                        // flash SÍ se pinta con el visor abierto ahora
                        // (`render_viewer`).
                        if let norte_frontend::keymap::Count::Ignored(n) = count {
                            self.flash = Some((
                                norte_frontend::keymap::count_ignored_message(&cmd, n),
                                true,
                            ));
                        }
                        // `Count::times` es la ÚNICA política de repetición
                        // (ADR 0044): tres sitios con su propio `match` es
                        // como los tres acaban discrepando.
                        for _ in 0..count.times() {
                            self.run_viewer_command(&cmd, cx);
                            // `viewer.close` deja `self.viewer` en None: lo
                            // que quede del contador correría contra un visor
                            // que ya no existe.
                            if self.viewer.is_none() {
                                break;
                            }
                        }
                    }
                    // K3a: la secuencia sigue viva — el panel describe el
                    // resolver del VISOR mientras es él quien tiene el
                    // teclado (`refresh_which_key_viewer`, gemela de
                    // `refresh_which_key` para el pane). Sin temporizador: el
                    // panel aparece con la misma tecla que deja el prefijo
                    // pendiente (ADR 0006). Un contador a medio teclear no
                    // abre nada (su prefijo pendiente está vacío); el pie lo
                    // sigue pintando (`render`, #91).
                    norte_frontend::keymap::Resolution::Pending(_)
                    | norte_frontend::keymap::Resolution::Counting(_) => {
                        self.refresh_which_key_viewer();
                    }
                    // K1 T4 + rust-reviewer MAJOR-1. Esta rama SÍ se alcanza:
                    // cada pantalla se valida contra el set que ELLA despacha
                    // (`screen_commands`), así que F9/F11/F12/Ctrl+P/Tab —
                    // bindings de `[global]` que el visor ve pero
                    // `apply_viewer_command` no atiende — salen `NotHere` en
                    // vez de fingir `Here` y morir en un `_ => {}`.
                    // K3a pagó la deuda: el flash SÍ se pinta con el visor
                    // abierto (`render_viewer`), y con él se va el panel.
                    norte_frontend::keymap::Resolution::Unavailable { command, why } => {
                        self.which_key = None;
                        self.flash = Some((
                            norte_frontend::keymap::unavailable_message(&command, why),
                            true,
                        ));
                    }
                    norte_frontend::keymap::Resolution::Reset => {
                        self.which_key = None;
                    }
                }
            } else {
                self.viewer_resolver.reset();
                // K3a: como el `Reset` de arriba — la tecla no la modela el
                // adaptador, y el resolver se resetea EXPLÍCITAMENTE, así que
                // cualquier panel construido sobre el prefijo anterior queda
                // obsoleto.
                self.which_key = None;
            }
            cx.notify();
            return;
        }

        // Panel de diferencias abierto (#158): captura fija, igual que el
        // visor — sustituye a los dos panes, así que las teclas del listado
        // no tienen nada debajo a lo que ir. La ayuda SÍ lo alcanza, por lo
        // mismo que alcanza al visor: la pantalla con su propio juego de
        // teclas es justo desde la que hay que poder preguntar.
        //
        // DESPUÉS del visor, y ese orden es el de `render`: las dos pantallas
        // pueden coexistir (un `fs.view` pedido antes de arrancar la
        // comparación aterriza cuando el panel ya está abierto, y la ayuda
        // despacha comandos desde encima de cualquiera de las dos), y quien
        // se queda las teclas tiene que ser quien SE PINTA. Con el orden
        // inverso, `Esc` cerraba un panel que el lector no estaba viendo.
        if self.compare.is_some() {
            if self.means_help(ks) {
                self.open_help();
                cx.notify();
                return;
            }
            self.on_compare_key(ks, cx);
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

        // K1 T5: aquí había un early-return sobre `mods.platform`. Existía
        // porque el keymap NO modelaba ⌘ — `gpui_chord` solo recibía
        // ctrl/alt/shift, así que `Cmd+q` llegaba al resolver como la `q`
        // DESNUDA y disparaba `app.quit` (MINOR 2, review T3). El bail-out
        // era la única forma de que no lo hiciera. Ya no: `Mods` tiene `cmd`
        // y ⌘ tiene dónde ir, así que `Cmd+q` es el chord `cmd+q`, no casa
        // ningún binding y cae en `Reset` — sin abrir el quick search, que
        // sigue gateado por `platform` en su rama. Sustituir el bail-out por
        // el campo, y no simplemente quitarlo, es lo que hace segura la
        // eliminación.

        // (3) keymap: nombre GPUI → Chord → resolver.
        if let Some(chord) = keymap::gpui_chord(&ks.key, keymap_mods(mods), ks.key_char.as_deref())
        {
            match self.resolver.push(chord) {
                norte_frontend::keymap::Resolution::Run {
                    command: cmd,
                    count,
                } => {
                    resolution_dbg = "run";
                    // K3a: la secuencia terminó — con ella, el panel.
                    self.which_key = None;
                    // K2a: un contador sobre un comando que no lo acepta NO se
                    // traga — corre UNA vez y el flash lo dice. Antes del
                    // despacho: si el comando tiene algo que decir, su flash
                    // manda sobre este.
                    if let norte_frontend::keymap::Count::Ignored(n) = count {
                        self.flash =
                            Some((norte_frontend::keymap::count_ignored_message(&cmd, n), true));
                    }
                    // El contador repite el DESPACHO, no llega al comando:
                    // ninguna firma cambia y ninguno puede olvidarse de
                    // honrarlo. `run_command` es un `match cmd` sin returns
                    // tempranos del caller, así que el bucle no puede saltarse.
                    // Misma política única que el visor de arriba y que la TUI:
                    // `Count::times` (ADR 0044).
                    for _ in 0..count.times() {
                        self.run_command(&cmd, cx);
                        // Parar en seco si algo se puso DELANTE del dual-pane
                        // (modal de confirmación de salida, visor, ajustes…):
                        // lo que quede del contador dispararía comandos por
                        // detrás de una pantalla que ya tiene el teclado.
                        if self.overlay_in_front() || self.help.is_some() {
                            break;
                        }
                    }
                }
                // K2a: un contador a medio teclear no ejecuta nada todavía; el
                // pie lo pinta junto a la secuencia pendiente (ver `render`).
                // K3a: y el MISMO estado abre (o no) el panel which-key —
                // `refresh_which_key` es quien sabe que un contador suelto no
                // tiene panel (su prefijo pendiente está vacío), no este
                // `match`. Sin temporizador de ningún tipo: el panel aparece
                // con la tecla que deja el prefijo pendiente (ADR 0006).
                norte_frontend::keymap::Resolution::Pending(_)
                | norte_frontend::keymap::Resolution::Counting(_) => {
                    // Secuencia en curso: nada que ejecutar todavía. El
                    // indicador de secuencia pendiente lo pinta `render` al
                    // pie leyendo `resolver.pending()` (#91).
                    resolution_dbg = "pending";
                    self.refresh_which_key();
                }
                norte_frontend::keymap::Resolution::Unavailable { command, why } => {
                    // 16 de los 47 bindings de Browse son `NotHere` en este
                    // frontend. Antes de K1 desaparecían al cargar; entre la
                    // tarea 3 de K1 y aquí despachaban un nombre de comando
                    // sin brazo. Ahora dicen lo que son. El flash vive hasta
                    // la siguiente tecla (`on_key` lo limpia arriba) y SÍ se
                    // pinta sobre el dual-pane, que es donde estamos.
                    resolution_dbg = "unavailable";
                    self.which_key = None;
                    self.flash = Some((
                        norte_frontend::keymap::unavailable_message(&command, why),
                        true,
                    ));
                }
                norte_frontend::keymap::Resolution::Reset => {
                    resolution_dbg = "reset";
                    self.which_key = None;
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
            // K3a: ídem — el panel construido sobre el prefijo anterior
            // queda obsoleto en el mismo instante.
            self.which_key = None;
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

    /// Botón izquierdo abajo sobre una fila: foco a ese pane + cursor a esa
    /// fila; doble-click sobre un directorio hace `cd`; ctrl/shift/arrastre
    /// marcan, vía la máquina COMPARTIDA con la TUI ([`norte_frontend::mouse`]).
    ///
    /// El doble click va ANTES de la máquina y solo SIN modificadores (mismo
    /// orden y misma condición que la TUI): entrar en un directorio no es un
    /// gesto de marcado, y con ctrl/shift pulsados lo que el usuario pide es
    /// marcar, no navegar. `click_count` lo cuenta GPUI a nivel de plataforma
    /// — no hace falta la ventana de tiempo a mano que sí necesita la TUI
    /// (ningún protocolo de ratón de terminal reporta dobles clicks).
    fn on_row_click(
        &mut self,
        pane: usize,
        idx: usize,
        dir_target: Option<VPath>,
        click_count: usize,
        mods: Mods,
        cx: &mut Context<Self>,
    ) {
        // El flash también se despide con el ratón (review 7c MINOR-4a).
        self.flash = None;
        if mods == Mods::NONE && click_count >= 2 {
            // Desarma lo que armara el primer click de la pareja: el listado
            // está a punto de cambiar entero bajo el puntero, y un ancla
            // rancia barrería contra entradas que ya no son las mismas.
            self.mouse.drag.cancel();
            self.focus = pane;
            // Un click cancela cualquier filtro (el cursor real vuelve a
            // mandar) y se posa en `idx`.
            self.panes[pane].quick_cancel();
            self.query[pane].clear();
            self.panes[pane].set_cursor(idx);
            self.follow_cursor(pane);
            if let Some(dir) = dir_target {
                self.cd(pane, dir, cx);
            }
            cx.notify();
            return;
        }
        let applied = mouse_press(
            &mut self.mouse,
            &mut self.panes,
            &mut self.focus,
            Spot::new(pane, idx),
            mods,
        );
        // Un click LIMPIO cierra el quick search del pane pulsado, y solo él.
        //
        // El orden importa y la excepción también. Con el filtro puesto se
        // pinta un SUBCONJUNTO: el resaltado sale de la selección del filtro,
        // así que mover el cursor real no movería nada visible y la siguiente
        // operación actuaría sobre la fila del filtro y no sobre la pulsada.
        // Cerrarlo arregla eso — el índice es ABSOLUTO y sobrevive a que
        // vuelva el listado entero.
        //
        // Pero cerrarlo ANTES de marcar sería mucho peor que no cerrarlo:
        // `mark_range`/`set_mark`/`apply_sweep` consultan el filtro para no
        // alcanzar lo que esconde (ver su rustdoc), y sin filtro un
        // shift+click marcaría TODOS los índices intermedios —los ocultos
        // incluidos—, que es justo el ensanchamiento silencioso de la
        // siguiente copia o borrado que esos guards existen para impedir. Por
        // eso va DESPUÉS del press, y por eso solo para el gesto que no marca
        // nada: la pulsación limpia arma el barrido pero no marca (contrato de
        // `Drag::press`).
        if mods == Mods::NONE {
            self.panes[pane].quick_cancel();
            self.query[pane].clear();
        }
        if let Some(p) = applied.moved_cursor {
            self.follow_cursor(p);
        }
        cx.notify();
    }

    /// Cierra el frame para el ratón: suelta el gesto en vuelo si ha dejado
    /// de significar algo. **Es el ÚNICO sitio donde un gesto caduca**, y va
    /// en `render` porque es por donde pasa la GUI después de cada cambio de
    /// estado y antes de atender ningún evento del siguiente.
    ///
    /// Cubre las dos formas de quedarse con un gesto rancio. Una es el
    /// listado que se mueve bajo el puntero: aquí llega ASÍNCRONO (un
    /// `SessionEvent::Listed` de un refresh tras una mutación) sin que el
    /// usuario suelte el botón, y los índices del gesto pasan a nombrar otros
    /// ficheros. La otra es el release que no llega — un botón soltado con la
    /// ventana ya sin foco (alt+tab a mitad de arrastre) o fuera de ella: sin
    /// esto el gesto seguiría armado y la siguiente pasada del ratón, minutos
    /// y un directorio después, continuaría el barrido.
    ///
    /// Las marcas que un barrido ya aplicó SE QUEDAN: soltar el gesto no es
    /// deshacerlo (contrato de `Drag::cancel`).
    fn expire_stale_mouse_gesture(&mut self) {
        let validity = MouseValidity {
            epochs: [self.panes[0].listing_epoch(), self.panes[1].listing_epoch()],
            // El menú contextual cuenta como overlay delante (tarea 4): se
            // abre con el botón derecho a mitad de un arrastre igual que un
            // modal, y el gesto deja de significar lo que el usuario hizo.
            hidden: self.overlay_in_front() || self.context_menu.is_some(),
        };
        expire_stale_gesture(&mut self.mouse, validity);
    }

    /// El puntero cruzó una fila con el botón izquierdo pulsado: extiende el
    /// gesto armado. Un barrido re-enuncia su rango ENTERO en cada motion, y
    /// uno que no cambia de fila no emite nada (contrato de `Drag::motion`),
    /// así que esto no necesita throttling propio por encima del que ya trae
    /// la máquina — GPUI reporta el puntero por píxel, pero la máquina lo
    /// deduplica por fila.
    fn on_row_drag(&mut self, pane: usize, idx: usize, mods: Mods, cx: &mut Context<Self>) {
        let applied = mouse_motion(
            &mut self.mouse,
            &mut self.panes,
            &mut self.focus,
            Spot::new(pane, idx),
            mods,
        );
        if let Some(p) = applied.moved_cursor {
            self.follow_cursor(p);
        }
        if applied.changed {
            cx.notify();
        }
    }

    /// El botón izquierdo subió. `at` es `None` cuando el release no cayó
    /// sobre ninguna fila (el `on_mouse_up` de la raíz): el gesto se cancela
    /// en vez de adivinar un destino — un destino inferido es una operación
    /// de ficheros que nadie pidió.
    ///
    /// Un drop sobre el OTRO pane abre el modal de confirmación de copiar o
    /// mover; sobre el propio pane de origen no hace nada (contrato de
    /// `Drag::release`, que ni siquiera emite el efecto).
    fn on_mouse_release(&mut self, at: Option<Spot>, mods: Mods, cx: &mut Context<Self>) {
        let applied = mouse_release(&mut self.mouse, &mut self.panes, &mut self.focus, at, mods);
        if let Some(req) = applied.transfer {
            self.drop_transfer(req);
        }
        if let Some(p) = applied.moved_cursor {
            self.follow_cursor(p);
        }
        if applied.changed {
            cx.notify();
        }
    }

    /// Botón DERECHO sobre una fila: abre el menú contextual en el puntero
    /// (plan de ratón, tarea 4).
    ///
    /// Antes de abrirlo fija el OBJETIVO en el modelo, y ahí está la decisión
    /// que hace que el menú no mienta: si la fila pulsada NO está marcada, el
    /// menú actúa sobre ella sola, así que las marcas de ese pane se sueltan
    /// (lo que hace cualquier file manager de escritorio al pulsar con el
    /// derecho fuera de la selección) y la fila pasa a ser el cursor. Si SÍ
    /// está marcada, no se toca nada: el menú actúa sobre las marcas y lo
    /// dice. Sin ese paso, el menú diría «1 elemento» mientras once filas
    /// siguen resaltadas y la copia se llevaría las once.
    ///
    /// Con cualquier overlay/modal delante NO abre: sus scrims no ocluyen el
    /// ratón, así que un click derecho podría alcanzar una fila de debajo.
    fn open_context_menu(
        &mut self,
        pane: usize,
        idx: usize,
        at: gpui::Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.overlay_in_front() {
            return;
        }
        self.flash = None;
        // Un menú delante caduca cualquier gesto de marcado en vuelo (mismo
        // criterio que `expire_stale_mouse_gesture`); desarmarlo aquí evita
        // además que el release del botón derecho lo continúe.
        self.mouse.drag.cancel();
        self.focus = pane;
        // Como el click limpio: el filtro se cierra (el índice es ABSOLUTO).
        self.panes[pane].quick_cancel();
        self.query[pane].clear();

        // La regla «marcas o esta fila» (cursor incluido) vive en
        // `context_target` (pura).
        let Some((target, kind)) = context_target(&mut self.panes[pane], idx) else {
            return;
        };
        self.follow_cursor(pane);
        // El criterio de solo-lectura de la GUI es SINTÁCTICO y punto: a
        // diferencia de la TUI no cachea `Capabilities` por conexión, así que
        // aquí `scheme_is_read_only` es la respuesta, no el respaldo.
        let facts = context_menu::facts_for(
            kind,
            target.count(),
            context_menu::ReadOnly {
                source: scheme_is_read_only(self.panes[pane].dir().scheme()),
                dest: scheme_is_read_only(self.panes[1 - pane].dir().scheme()),
            },
            self.journalled,
        );
        self.context_menu = Some(ContextMenu::open(
            pane,
            self.panes[pane].listing_epoch(),
            (f32::from(at.x), f32::from(at.y)),
            target,
            &facts,
        ));
        cx.notify();
    }

    /// ¿Hay algo delante del dual-pane? (modal, visor, ajustes, extensiones,
    /// paleta, picker de columnas, panel de diferencias). Fuente única del
    /// criterio que comparten el guard de apertura del menú contextual y la
    /// vigencia de los gestos de ratón — dos listas separadas se
    /// desincronizarían al añadir la octava pantalla.
    ///
    /// El panel de diferencias (#158) cuenta por lo mismo que el visor:
    /// sustituye a los DOS panes, así que un arrastre armado sobre una fila
    /// del listado deja de tener sobre qué soltarse en cuanto se abre.
    fn overlay_in_front(&self) -> bool {
        self.modal.is_some()
            || self.viewer.is_some()
            || self.viewer_loading
            || self.settings_view.is_some()
            || self.extensions.is_some()
            || self.palette.is_some()
            || self.columns_picker.is_some()
            || self.compare.is_some()
    }

    /// Activa la entrada `i` del menú: despacha su comando por el MISMO
    /// `run_command` que ejecuta el teclado — el menú no tiene camino propio.
    /// Una entrada DESHABILITADA no despacha nada y deja el menú abierto
    /// (cerrarlo sería indistinguible de haber ejecutado algo).
    fn activate_context_menu(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(cmd) = self.context_menu.as_ref().and_then(|m| m.activate(i)) else {
            cx.notify();
            return;
        };
        self.context_menu = None;
        self.run_command(cmd, cx);
        cx.notify();
    }

    /// Enruta UNA tecla con el menú abierto (captura fija de overlay): la
    /// decisión es de [`ContextMenu::on_key`], aquí sólo se aplica.
    fn on_context_menu_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) {
        let Some(menu) = &mut self.context_menu else {
            return;
        };
        match menu.on_key(&ks.key) {
            MenuOutcome::None => {}
            MenuOutcome::Close => self.context_menu = None,
            MenuOutcome::Run(cmd) => {
                self.context_menu = None;
                self.run_command(cmd, cx);
            }
        }
    }

    /// Cierra el menú si el listado de su pane se movió bajo él (un `cd`, un
    /// refresh asíncrono tras una mutación): el objetivo se fijó contra el
    /// listado que había al abrirlo. Gemelo de
    /// [`Self::expire_stale_mouse_gesture`], y por el mismo motivo va en
    /// `render`.
    fn expire_stale_context_menu(&mut self) {
        expire_stale_menu(
            &mut self.context_menu,
            [self.panes[0].listing_epoch(), self.panes[1].listing_epoch()],
        );
    }

    /// `app.terminal` (#135, design §E): abre el emulador de terminal del
    /// escritorio en el directorio del pane activo.
    ///
    /// La GUI no suspende nada —no tiene terminal anfitriona que ceder—, así
    /// que aquí «abrir un shell» es lanzar un proceso DESACOPLADO, sin stdio
    /// heredado y sin esperarlo.
    ///
    /// El orden de candidatos lo decide `norte_frontend::shell`
    /// (`$TERMINAL`, `xdg-terminal-exec`, y una lista corta); lo que se hace
    /// AQUÍ es sondearlos en el PATH, que es I/O de disco y va al executor de
    /// fondo (regla 2, mismo criterio que el `spawn_blocking` de la TUI). Que
    /// no haya ninguno NO es un no-op silencioso: se dice qué se intentó, con
    /// la misma forma que `msg-open-missing-program`.
    ///
    /// Un pane que no es `file://` declina, como en la TUI: un emulador
    /// abierto «ahí» aterrizaría en el home del usuario y parecería que norte
    /// se inventó el directorio.
    fn open_terminal(&mut self, cx: &mut Context<Self>) {
        // Sin guardia, mantener `alt+t` pulsado a la tasa de repetición del
        // teclado le pide sesenta ventanas al escritorio en dos segundos
        // (review de S4, MINOR-5). Una en vuelo basta.
        if self.terminal_launching {
            return;
        }
        let vdir = self.panes[self.focus].dir().clone();
        let native = match norte_vfs_local::vpath_to_native(&vdir)
            .ok()
            .and_then(|n| norte_frontend::shell::child_cwd(&n))
        {
            Some(n) => n,
            None => {
                // Dos negativas distintas: el pane no es local, o lo es y su
                // forma nativa no se le puede dar a un hijo (Windows
                // verbatim). Se distinguen porque mandan a mirar sitios
                // distintos.
                let id = if norte_vfs_local::vpath_to_native(&vdir).is_ok() {
                    "msg-shell-cwd-unsupported"
                } else {
                    "msg-shell-remote"
                };
                let (texto, hostil) = norte_frontend::path_display(&vdir);
                self.flash = Some((
                    norte_i18n::ta(id, &[("path", &Self::badged(&texto, hostil))]),
                    true,
                ));
                cx.notify();
                return;
            }
        };
        let candidatos = norte_frontend::shell::terminal_candidates(&native);
        let configurado = std::env::var_os("TERMINAL").is_some();
        self.terminal_launching = true;
        cx.spawn(async move |this, cx| {
            let resultado = cx
                .background_spawn(async move { spawn_terminal(&candidatos, &native, configurado) })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.terminal_launching = false;
                // Un lanzamiento que salió no dice nada: la ventana del
                // emulador apareciendo ES el acuse. Solo se habla al fallar.
                if let Err(msg) = resultado {
                    view.flash = Some((msg, true));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// La ruta ya saneada para un flash: badge (el de ESTA crate) fuera de la
    /// traducción y elipsis media.
    ///
    /// Las dos mitades son findings de S4. El badge, porque esta función
    /// hardcodeaba `"!"` —el de la TUI— mientras el resto del fichero usa
    /// [`HOSTILE_BADGE`] (`⚠`), justo el segundo literal divergente que la
    /// otra mitad del cambio subió a `pub` para evitar. El tope, porque el
    /// flash trunca por la derecha SIN marca y la ruta va a mitad de frase:
    /// sin acotarla, lo que desaparece es la explicación.
    fn badged(texto: &str, hostil: bool) -> String {
        let corto = norte_frontend::middle_ellipsis(texto, 48);
        if hostil {
            format!("{HOSTILE_BADGE} {corto}")
        } else {
            corto
        }
    }

    /// `pane.copy-path`: copia al portapapeles la ruta de lo que la op
    /// tocaría — las marcas del pane con foco, o el cursor si no hay ninguna
    /// (`marked_paths`, la MISMA fuente que copiar/mover/borrar: el menú no
    /// puede copiar la ruta de algo distinto de lo que borraría).
    ///
    /// Forma WIRE, una por línea. No `display_lossy`: esa es la vista humana
    /// (lleva el `⟨scheme⟩` y sustituye por `�` lo que no se puede pintar),
    /// y una ruta con `�` dentro no nombra ningún fichero. El wire es
    /// LOSSLESS (bytes no-UTF8 → `%XX`), reparseable por el propio norte y,
    /// por construcción, sin controles crudos (`vpath_codec` escapa C0/DEL).
    /// Los formateadores bidi SÍ pasan: son parte de la identidad del
    /// nombre, y enmascararlos daría una ruta que no existe — misma doctrina
    /// que `norte-help` con sus claves de despacho (una identidad no se
    /// enmascara, se cita entera o no se cita).
    fn copy_paths_to_clipboard(&mut self, cx: &mut Context<Self>) {
        let paths = self.panes[self.focus].marked_paths();
        if paths.is_empty() {
            return;
        }
        let n = paths.len();
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(clipboard_text(&paths)));
        self.flash = Some((
            norte_i18n::ta("gui-menu-copied", &[("n", &n.to_string())]),
            false,
        ));
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

    /// El canal vertical de la barra del cuerpo, en píxeles de ventana.
    ///
    /// DERIVADO de la misma aritmética con la que se pinta el marco, y no
    /// medido: el overlay se centra en el viewport (`items_center` +
    /// `justify_center` sobre `inset_0`), así que su origen es una resta y su
    /// canal es el marco menos el borde, la cabecera, el título del detalle y
    /// el pie — cada uno de altura FIJA y conocida, que es justo por lo que se
    /// les fijó.
    ///
    /// El acoplamiento con el pintor es real y por eso está escrito aquí: si
    /// alguien mueve una de esas piezas, esta cuenta se entera por un salto
    /// del pulgar. Medir de verdad pediría guardar las bounds desde un
    /// `canvas`, que es más maquinaria de la que este gesto merece.
    fn help_body_track(&self, viewport: gpui::Size<gpui::Pixels>) -> (f32, f32) {
        let (_, frame_h) = self.help_frame(viewport);
        let row_h = f32::from(self.fonts.row_h);
        let frame_y = (f32::from(viewport.height) - frame_h) / 2.0;
        // borde superior + cabecera del overlay + título del panel de detalle
        let y0 = frame_y + 2.0 + row_h + row_h;
        // …hasta el pie, con su borde inferior.
        let y1 = frame_y + frame_h - 2.0 - row_h;
        (y0, (y1 - y0).max(1.0))
    }

    /// Lleva el cuerpo al punto del canal donde está el puntero.
    fn help_scroll_to(&mut self, y: gpui::Pixels, window: &Window, cx: &mut Context<Self>) {
        let rows = self.help_rows(window.viewport_size());
        let (y0, alto) = self.help_body_track(window.viewport_size());
        let total = self.help.as_ref().map_or(0, |v| self.help_body(v).len());
        let fraccion = ((f32::from(y) - y0) / alto).clamp(0.0, 1.0);
        let destino = scroll_offset_at(fraccion, rows, total);
        if let Some(view) = &mut self.help {
            view.state.scroll_body_to(destino);
            view.state.clamp_scroll(total);
        }
        cx.notify();
    }

    /// Rueda del ratón sobre la ayuda: desplaza el CUERPO, tenga el foco donde
    /// tenga.
    ///
    /// La regla es la de los panes —se desplaza lo que está bajo el puntero— y
    /// la página `mouse` del corpus ya la promete. Con una salvedad honesta: la
    /// barra lateral no tiene scroll propio (su ventana la deriva
    /// `sidebar_offset` del cursor), así que la rueda sobre ella mueve el
    /// cuerpo igual. Mover la lateral pediría un scroll independiente en el
    /// modelo, que es más que lo que esta deuda pedía.
    fn on_help_scroll(&mut self, delta: ScrollDelta, cx: &mut Context<Self>) {
        let Some(view) = self.help.as_mut() else {
            return;
        };
        let y = scroll_y(delta);
        if y > 0.0 {
            view.state.scroll_body(-Self::HELP_WHEEL_LINES);
        } else if y < 0.0 {
            view.state.scroll_body(Self::HELP_WHEEL_LINES);
        }
        // El tope de abajo lo pone el pintor, que es quien sabe cuántas líneas
        // tiene la página maquetada — mismo cierre que la rama de teclado.
        let total = self.help.as_ref().map_or(0, |v| self.help_body(v).len());
        if let Some(view) = self.help.as_mut() {
            view.state.clamp_scroll(total);
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

    /// Click en una cabecera ordenable (#108 b6): aplica `after_click` al
    /// orden ACTUAL del pane (no al de config — dos clicks seguidos deben
    /// alternar), re-ordena in place (`set_sort` re-ancla cursor y quick) y
    /// recuerda la elección para los próximos cd de este pane.
    fn on_sort_click(
        &mut self,
        pane: usize,
        col: norte_frontend::SortColumn,
        cx: &mut Context<Self>,
    ) {
        // El scrim del modal PINTA pero no ocluye eventos de ratón en GPUI
        // (no hay `.occlude()` en este árbol): sin este guard, un click en
        // una cabecera detrás de un confirm mutaría el orden del pane. El
        // picker de columnas (7c) comparte scrim no-ocluyente → mismo guard.
        // El barrido `.occlude()` de todos los overlays queda como follow-up.
        if self.modal.is_some() || self.columns_picker.is_some() {
            return;
        }
        // El flash también se despide con el ratón (review 7c MINOR-4a).
        self.flash = None;
        let spec = self.panes[pane].sort().after_click(col);
        self.panes[pane].set_sort(spec);
        self.sort_override[pane] = Some(spec);
        self.focus = pane;
        cx.notify();
    }

    /// Pinta una columna (un pane).
    ///
    /// `drop_pane` es el pane sobre el que caería un drop AHORA MISMO (tarea
    /// 5): el destino se resalta mientras el puntero lo sobrevuela con el
    /// botón pulsado. El aviso de qué haría (cuántas entradas, copiar o
    /// mover) va en la línea que pinta `render`; esto solo dice DÓNDE.
    fn render_pane(
        &self,
        i: usize,
        chrome: &ChromeColors,
        drop_pane: Option<usize>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let pane = &self.panes[i];
        let focused = self.focus == i;
        let drop_target = drop_pane == Some(i);
        // #108 b6: geometría de columnas del frame — el advance del mono ('0';
        // en una monoespaciada todo glifo simple mide la celda), las celdas
        // interiores aproximadas y el MISMO column_widths() que pinta la TUI.
        let ts = cx.text_system();
        let font_id = ts.resolve_font(&self.fonts.mono);
        let ch: f32 = ts
            .advance(font_id, self.fonts.size, '0')
            .map_or(f32::from(self.fonts.size) * 0.6, |a| f32::from(a.width));
        let cells = pane_inner_cells(
            f32::from(window.viewport_size().width),
            ch,
            f32::from(self.fonts.size),
        );
        // #108 7b: el ESTILO de cada columna se resuelve aquí, UNA vez por
        // columna y frame (`style_for_id` pliega mapas y clona el header —
        // por fila × columna sería O(filas × columnas) de lookups
        // idénticos). Catálogo del scheme (#117): cacheado por sesión
        // (`attr_catalogs`); sin entrada aún = defaults Opaque.
        let scheme = pane.dir().scheme();
        let catalog = self.attr_catalogs.get(scheme);
        let cols: Vec<(
            norte_frontend::columns::ColumnId,
            u16,
            norte_frontend::columns::ColumnStyle,
        )> = norte_frontend::columns::column_widths(&self.column_settings, scheme, cells)
            .into_iter()
            .map(|(id, w)| {
                let s = self.column_settings.style_for_id(scheme, &id, catalog);
                (id, w, s)
            })
            .collect();
        let now_ms: i64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        // Copia barata (todo `Copy`) para moverla dentro del closure
        // `'static` de `cx.processor` — no puede capturar `&ChromeColors`
        // prestado de este frame, que no vive tanto como el closure.
        let chrome_owned = *chrome;
        // #108 b6: copia de `cols` para el closure (la original queda para
        // la fila de cabeceras de más abajo).
        let cols_owned = cols.clone();

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
            cx.processor(move |this, range: Range<usize>, window, cx| {
                // #124: `uniform_list` solo pide las filas que va a pintar,
                // así que este rango ES el viewport — el modelo deja de
                // adivinar su alto (paginación y radio de la sonda salen de
                // ahí).
                this.panes[i].set_viewport_rows(range.len());
                let pane = &this.panes[i];
                let sel_path = pane.selected().map(|e| e.path.clone());
                // Mapea el rango (índices dentro de la lista VISIBLE) a índices
                // ABSOLUTOS de `entries()`, respetando el filtro quick.
                let abs: Vec<usize> = match pane.quick_visible() {
                    Some(vis) => range.clone().filter_map(|k| vis.get(k).copied()).collect(),
                    None => range.clone().collect(),
                };
                // #123: hidrata lo que se ve (y una pantalla de pre-carga a
                // cada lado, para que desplazarse no estrene celdas en
                // blanco). Deduplicado por `probed`: en régimen estacionario
                // esta llamada no manda nada.
                let margen = range.len();
                let pre = range.start.saturating_sub(margen)..range.start;
                let post = range.end..range.end.saturating_add(margen);
                let vecinos: Vec<usize> = match this.panes[i].quick_visible() {
                    Some(vis) => pre
                        .chain(post)
                        .filter_map(|k| vis.get(k).copied())
                        .collect(),
                    None => pre.chain(post).collect(),
                };
                this.request_hydration(i, abs.iter().copied().chain(vecinos));
                abs.into_iter()
                    .map(|j| {
                        // Clona la entrada (barata: VPath + kind + dos
                        // Option) para no retener un préstamo de `this.panes`
                        // mientras se llama a `this.render_row` más abajo.
                        let e = this.panes[i].entries()[j].clone();
                        let hl = sel_path.as_ref() == Some(&e.path);
                        let marked = this.panes[i].is_marked(&e);
                        // `window` (G2 decisión 3): `render_row` lo necesita
                        // para `is_window_active()` — el blink de cursor solo
                        // se anima con la ventana enfocada (`with_animation`
                        // de GPUI NO lo comprueba solo; ver doc de
                        // `render_row`).
                        this.render_row(
                            i,
                            j,
                            &e,
                            hl,
                            marked,
                            &chrome_owned,
                            &cols_owned,
                            now_ms,
                            ch,
                            window,
                            cx,
                        )
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
            // El destino de un drop gana sobre el foco: el pane con foco es
            // casi siempre el ORIGEN del arrastre (la pulsación se lo lleva),
            // así que pintar el destino con `border_focus` los dejaría
            // idénticos justo cuando importa distinguirlos. `sel_bg` es el
            // color de «esto es lo que estás señalando» del tema.
            .border_color(if drop_target {
                chrome.sel_bg
            } else if focused {
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
        // `py` en `sp::S` (bump deliberado del look-and-feel GP, antes 2px) +
        // separador de 1px al pie en `border_unfocus` — línea fina bajo la
        // cabecera, independiente de si el pane tiene foco (el foco ya se
        // marca con el borde del pane entero, ver `col` más arriba).
        col = col.child(
            div()
                .px(px(sp::S))
                .py(px(sp::S))
                .border_b_1()
                .border_color(chrome.border_unfocus)
                .bg(chrome.header_bg)
                .text_color(chrome.header_fg)
                .truncate()
                .child(SharedString::from(header)),
        );

        // Estado transitorio: cargando / error / vacío.
        if pane.loading() {
            col = col.child(
                div()
                    .px(px(sp::S))
                    .child(SharedString::from(norte_i18n::t("gui-loading"))),
            );
        } else if let Some(err) = &self.errors[i] {
            col = col.child(div().px(px(sp::S)).text_color(chrome.err_fg).child(
                SharedString::from(norte_i18n::ta(
                    "gui-banner-error",
                    &[("error", err.as_str())],
                )),
            ));
        } else if pane.entries().is_empty() {
            col = col.child(
                div()
                    .px(px(sp::S))
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
                    .px(px(sp::S))
                    .bg(chrome.quick_bg)
                    .text_color(chrome.quick_fg)
                    .child(SharedString::from(norte_i18n::ta(
                        "status-archive-skipped",
                        &[("n", &n.to_string())],
                    ))),
            );
        }

        // #107: ocultación activa con entradas apartadas — misma disciplina
        // y vecindad que `status-archive-skipped`: el listado enseña menos
        // de lo que hay y eso jamás es silencioso. Informativo, no aviso:
        // hereda el fg del pane (quick_fg SIN su fondo es ilegible en el
        // tema default — el comentario del badge de arriba ya lo veta).
        if pane.hidden_count() > 0 {
            col = col.child(div().px(px(sp::S)).child(SharedString::from(norte_i18n::ta(
                "status-hidden",
                &[("n", &pane.hidden_count().to_string())],
            ))));
        }

        // #108 b6: fila de cabeceras de columna sobre el listado — mono (misma
        // geometría de celda que las filas), dim, con ▲/▼ en la columna del
        // orden activo. Las cabeceras ordenables son botones (`Role::Button`,
        // cursor pointer, hover); `Kind` y las columnas de plugin no (sin
        // SortColumn). El canalón de marca se replica como hueco fijo para que
        // la cabecera del nombre arranque donde arranca el nombre.
        {
            let sort = pane.sort();
            let dim = gpui::Rgba {
                a: 0.55,
                ..chrome.fg
            };
            let arrow = if sort.dir == norte_frontend::SortDir::Asc {
                "▲"
            } else {
                "▼"
            };
            let mut header_row = div()
                .id(format!("col-header-{i}"))
                .flex_none()
                .h(self.fonts.row_h)
                .px(px(sp::S))
                .flex()
                .flex_row()
                .items_center()
                .font(self.fonts.mono.clone())
                .text_color(dim)
                .child(
                    div()
                        .flex_none()
                        .w(self.fonts.size)
                        .child(SharedString::from("")),
                );
            for (k, (col_b, w, style)) in cols.iter().enumerate() {
                let is_name = matches!(
                    col_b,
                    norte_frontend::columns::ColumnId::Builtin(
                        norte_frontend::columns::Builtin::Name
                    )
                );
                let sortable = norte_frontend::columns::sort_column_id(col_b);
                let active = sortable == Some(sort.column);
                // #117: etiqueta compartida TUI/GUI (header custom del spec
                // → Fluent → catálogo enmascarado → id) — `header_label` ya
                // devuelve texto seguro, sin re-enmascarar aquí.
                let base = norte_frontend::columns::header_label(col_b, style, catalog);
                let label = if active {
                    format!("{base}{arrow}")
                } else {
                    base.clone()
                };
                let mut cell = div()
                    .id(format!("col-header-{i}-{k}"))
                    .overflow_hidden()
                    .child(div().truncate().child(SharedString::from(label)));
                cell = if is_name {
                    cell.flex_1()
                } else {
                    // #108 7b: el `align` del estilo elige el lado, en paso
                    // con las celdas de las filas; el separador (pl) sigue
                    // abriendo el ancho en ambos casos.
                    let cell = cell
                        .flex_none()
                        .w(px(f32::from(*w) * ch))
                        .pl(px(ch))
                        .flex()
                        .flex_row();
                    match style.align {
                        norte_frontend::columns::Align::Left => cell.justify_start(),
                        norte_frontend::columns::Align::Right => cell.justify_end(),
                    }
                };
                if let Some(sc) = sortable {
                    let hover_bg = chrome.hover_bg;
                    cell = cell
                        .role(gpui::Role::Button)
                        .aria_label(base)
                        .cursor_pointer()
                        .hover(move |s| s.bg(hover_bg))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev: &MouseDownEvent, _w, cx| {
                                this.on_sort_click(i, sc, cx);
                            }),
                        );
                }
                header_row = header_row.child(cell);
            }
            // #117-follow-up: las cabeceras de columnas plugin: salen del
            // funnel (header_label, no ordenables — sort_column_id=None);
            // el loop fijo de 96px de G3c murió con la config-driven.
            col = col.child(header_row);
        }

        // Lista de entradas, virtualizada (issue #87): `uniform_list` solo
        // construye el rango visible, no las N entradas del dir.
        col = col.child(list);

        // Pie de SELECCIÓN (#103): cuántas marcas hay y si un refresh se comió
        // alguna. Va POR PANE, no en una barra global, porque las marcas son
        // estado del pane y la GUI enseña los dos a la vez — mismo criterio
        // (y misma vecindad) que el badge `status-archive-skipped` de arriba.
        //
        // Una línea POR SEGMENTO, cada una `truncate()`: en una ventana
        // estrecha un solo renglón compartido recortaría la COLA, y el aviso
        // de poda es justo lo que no puede desaparecer. El aviso va PRIMERO
        // (mismo orden que la TUI: `{pruned}{marked}`) y en `err_fg` sobre el
        // fondo del pane (el par ya vetado por el banner de error de arriba),
        // así que es al menos tan prominente como el recuento — es la
        // advertencia; el recuento es informativo.
        let (marked_txt, pruned_txt) = marks_status_segments(
            pane.marks_len(),
            pane.marked_bytes(),
            pane.marked_dirs(),
            pane.pruned_marks(),
        );
        if let Some(txt) = pruned_txt {
            col = col.child(
                div()
                    .px(px(sp::S))
                    .truncate()
                    .text_color(chrome.err_fg)
                    .child(SharedString::from(txt)),
            );
        }
        if let Some(txt) = marked_txt {
            col = col.child(
                div()
                    .px(px(sp::S))
                    .truncate()
                    .child(SharedString::from(txt)),
            );
        }

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
                    .px(px(sp::S))
                    .py(px(1.0)) // sub-XS: acento fino de una línea, fuera de la escala a propósito
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
    // G2 decisión 3: el retorno pasó de `impl IntoElement` a `AnyElement`
    // (antes la única salida posible era `Stateful<Div>`; ahora la rama de
    // blink de cursor envuelve esa misma fila en `AnimationElement<..>`, un
    // tipo DISTINTO — `uniform_list::<R>` exige un `Vec<R>` uniforme, así
    // que las dos ramas necesitan converger a un único tipo concreto vía
    // `.into_any_element()`, el patrón estándar de GPUI para esto). Efecto
    // colateral bienvenido: ya no hace falta acotar el RPIT con `use<>` para
    // no atrapar lifetimes prestados (`AnyElement` es dueño de su contenido
    // en un arena, sin lifetime que capturar).
    #[allow(clippy::too_many_arguments)]
    fn render_row(
        &self,
        pane: usize,
        idx: usize,
        entry: &Entry,
        highlighted: bool,
        marked: bool,
        chrome: &ChromeColors,
        cols: &[(
            norte_frontend::columns::ColumnId,
            u16,
            norte_frontend::columns::ColumnStyle,
        )],
        now_ms: i64,
        ch: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let bytes = entry.path.file_name().map_or(&b""[..], Segment::as_bytes);
        // #111: el marcador vive en un canalón propio (abajo), no en el
        // label — el nombre accesible queda limpio (`aria_toggled` ya
        // lleva el estado de marca a AT) y el nombre no salta de columna
        // al marcar.
        let label = row_label(bytes, entry.kind);
        let color = entry_color(&self.theme, entry, self.effects.and_then(|e| e.glow));
        let dir_target = (entry.kind == EntryKind::Dir).then(|| entry.path.clone());

        // G3b (ADR 0037): decoración de plugin para esta entrada, ya
        // SANEADA (`norte_frontend::sanitize_decoration`) por el momento en
        // que llegó a `PaneState` — este render solo PINTA, no vuelve a
        // sanear. `role` resuelve al color del tema (mismo `styled_span_
        // color` que ya pinta los spans de un preview con estilo, G3a — un
        // único punto de resolución `Role → color`, no una copia paralela);
        // sin `role` reconocido, un tono DERIVADO del propio color de la
        // fila a alfa reducido hace de "dim" (GPUI no tiene un atributo DIM
        // relativo como el terminal — este es el análogo más simple, sin
        // añadir un campo nuevo a `ChromeColors` solo para esto).
        let decoration_badge = self.panes[pane].decoration_for(&entry.path).and_then(|d| {
            let badge = d.badge.as_deref()?;
            let resolved = decoration_badge_color(
                &self.theme,
                d.role,
                color,
                self.effects.and_then(|e| e.glow),
            );
            Some((badge.to_owned(), resolved))
        });

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
            // ANCHO COMPLETO, y no es cosmético: `uniform_list` maqueta cada
            // fila con `layout_as_root`, donde el espacio disponible es
            // definido pero el ancho de la fila sigue siendo `auto` — es
            // decir, el de su CONTENIDO. Sin esto el `flex_1` del nombre no
            // tenía nada que absorber, las celdas de tamaño y fecha salían
            // pegadas al nombre (cada fila en una x distinta) y la cabecera
            // —que se pinta FUERA de la lista y sí recibe el ancho del pane—
            // quedaba alineada con nada.
            .w_full()
            .px(px(sp::S))
            .py(px(1.0)) // sub-XS: acento fino de una línea, fuera de la escala a propósito
            // Redondeo sutil (GP): constante en TODAS las filas en vez de
            // condicionarlo a selección/hover — más barato (un solo estilo,
            // sin ramas) y visualmente inapreciable en una fila sin fondo.
            .rounded(px(sp::RADIUS_ROW))
            .cursor_pointer()
            .text_color(color)
            // G3b: fila en flex ROW con dos hijos (nombre + badge) en vez de
            // un único hijo de texto — el nombre sigue truncando solo (child
            // interior con `.truncate()`), el badge nunca se recorta.
            .flex()
            .flex_row()
            .items_center()
            // Canalón de marca (#111): pista TEXTUAL además del fondo —
            // el equivalente del `*` del gutter de la TUI, que no depende
            // de percibir el matiz. Ancho FIJO y presente SIEMPRE (vacío
            // sin marca), para que marcar no desplace el nombre; ~1em del
            // tamaño configurado (`ui_font_size` es del usuario — un px
            // fijo recortaría el glifo a tamaños grandes); el color sale
            // de `Role::Mark` como en la TUI, con fallback propio legible
            // sobre `mark_bg`.
            .child(
                div()
                    .flex_none()
                    .w(self.fonts.size)
                    .text_color(chrome_mark_fg(&self.theme))
                    .child(SharedString::from(if marked { MARK_MARKER } else { "" })),
            )
            .child(div().flex_1().truncate().child(SharedString::from(label)));
        if let Some((badge_text, badge_color)) = decoration_badge {
            row = row.child(
                div()
                    .pl(px(sp::XS))
                    .text_color(badge_color)
                    .child(SharedString::from(badge_text)),
            );
        }
        // #108 b6: celdas builtin tras el bloque del nombre (canalón+nombre+
        // badge, que absorbe el resto vía flex_1) y ANTES de las celdas de
        // plugin (G3c) — mismo orden que la TUI. Ancho FIJO en px = celdas de
        // layout() × advance del mono; contenido a la DERECHA con ≥1 celda de
        // separador (pl), presupuesto idéntico al de la TUI (el ancho INCLUYE
        // el separador). Ausencia (`None` de styled_cell — el size de un dir,
        // un mtime desconocido) = celda en blanco, jamás un 0 fabricado. Color:
        // el de la fila a alfa reducido — el mismo "dim relativo" que el
        // fallback del badge de decoración (GPUI no tiene Modifier::DIM).
        for (col, w, style) in cols.iter().filter(|(id, _, _)| {
            !matches!(
                id,
                norte_frontend::columns::ColumnId::Builtin(norte_frontend::columns::Builtin::Name)
            )
        }) {
            // #108 7b: formato del estilo resuelto (hoisted por frame en
            // `render_pane`) y `align` eligiendo el lado — el separador
            // (pl) sigue abriendo el ancho en ambos casos.
            // #117-follow-up: las celdas plugin: salen del side-map del
            // pane (re-enmascaradas allí); el resto, de la Entry.
            let cell = match col {
                norte_frontend::columns::ColumnId::Plugin { .. } => self.panes[pane]
                    .plugin_cell(&col.to_string(), &entry.path)
                    .unwrap_or_default(),
                _ => row_cell_text(entry, col, now_ms, style),
            };
            let celda = div()
                .flex_none()
                .w(px(f32::from(*w) * ch))
                .pl(px(ch))
                .flex()
                .flex_row();
            let celda = match style.align {
                norte_frontend::columns::Align::Left => celda.justify_start(),
                norte_frontend::columns::Align::Right => celda.justify_end(),
            };
            row = row.child(
                celda
                    .overflow_hidden()
                    .text_color(gpui::Rgba { a: 0.55, ..color })
                    .child(div().truncate().child(SharedString::from(cell))),
            );
        }
        // #117-follow-up: las celdas plugin: viven ahora en el funnel de
        // arriba (config-driven, side-map del pane) — el loop fijo de 96px
        // de G3c murió con ellas.
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
        } else {
            // Hover SOLO si la fila no es la seleccionada bajo cursor: encima
            // de `sel_bg` (que ya es el fondo más fuerte de la paleta), el
            // `hover_bg` derivado (más tenue, un lerp HACIA `sel_bg`) se
            // perdía o se veía como un parpadeo sin sentido — gatear aquí es
            // más simple y honesto que forzar un tercer tono para ese caso.
            // Fila marcada-y-no-seleccionada SÍ recibe hover (se ve como una
            // variación legible sobre `mark_bg`).
            row = row.hover(|s| s.bg(chrome.hover_bg));
        }
        // El hit test del ratón sale GRATIS: `on_mouse_move`/`on_mouse_up` de
        // una fila solo disparan con el puntero sobre SU hitbox (bubble +
        // `hitbox.is_hovered`, `Interactivity::on_mouse_move` en el rev
        // f14fea9), así que el (pane, índice) de un evento es el de esta fila
        // y no hay que traducir píxeles a filas como en la TUI.
        let row = row
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
                    this.on_row_click(
                        pane,
                        idx,
                        dir_target.clone(),
                        ev.click_count,
                        mouse_mods(ev.modifiers),
                        cx,
                    );
                }),
            )
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _w, cx| {
                // Solo con el botón izquierdo pulsado: `on_mouse_move` llega
                // también al pasear el ratón sin pulsar nada, y un barrido que
                // arrancara ahí marcaría por su cuenta.
                if ev.pressed_button == Some(MouseButton::Left) {
                    this.on_row_drag(pane, idx, mouse_mods(ev.modifiers), cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseUpEvent, _w, cx| {
                    this.on_mouse_release(Some(Spot::new(pane, idx)), mouse_mods(ev.modifiers), cx);
                }),
            )
            // Botón derecho: el menú contextual (tarea 4). El hit test sale
            // igual de gratis que el del izquierdo — el evento ya sabe su
            // (pane, índice) — y `ev.position` es el ancla del panel.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
                    this.open_context_menu(pane, idx, ev.position, cx);
                }),
            );

        // Blink de cursor (G2 decisión 3, ADR 0036 amendment): SOLO la fila
        // bajo el cursor real (`highlighted`), SOLO con `cursor_blink =
        // true` en el tema, y SOLO con la ventana enfocada.
        // `with_animation` de GPUI honra `reduce_motion` NATIVAMENTE
        // (renderiza el frame ESTÁTICO — el estado de INICIO de una
        // animación `.repeat()` — y no pide más frames, ver
        // `elements/animation.rs`), así que no hace falta comprobarlo aquí
        // (decisión 2: "reduce_motion: free via GPUI"). Pero NO comprueba
        // el foco de la ventana por su cuenta — `AnimationElement::
        // request_layout` pide su próximo frame incondicionalmente cuando
        // la fila se pinta; sin este gate, una ventana desenfocada seguiría
        // pidiendo frames por una fila que nadie ve, violando el Goal de G2
        // ("frame loop alive ONLY while ... focused"). El id de la
        // animación es POR PANE (`cursor-blink-{pane}`), no por fila:
        // únicamente puede haber una fila resaltada por pane a la vez, así
        // que el estado retenido (fase del pulso) sobrevive al cursor
        // moviéndose entre filas — un id por índice de fila reiniciaría la
        // fase cada vez que el cursor se mueve, que se leería como un
        // parpadeo cortado en vez de un pulso continuo.
        let want_blink = highlighted
            && self.effects.and_then(|e| e.cursor_blink) == Some(true)
            && window.is_window_active();
        if want_blink {
            // Anima la OPACIDAD de `sel_bg` (no su color): un pulso de
            // alpha entre 0.7 y 1.0 (`pulsating_between`, easing nativo de
            // GPUI) lee como "cursor respirando" sin desaturar ni cambiar
            // de tono el fondo de selección del tema.
            let sel_bg = chrome.sel_bg;
            row.with_animation(
                format!("cursor-blink-{pane}"),
                Animation::new(std::time::Duration::from_millis(1000))
                    .repeat()
                    .with_easing(pulsating_between(0.7, 1.0)),
                move |el, delta| {
                    el.bg(gpui::Rgba {
                        a: sel_bg.a * delta,
                        ..sel_bg
                    })
                },
            )
            .into_any_element()
        } else {
            row.into_any_element()
        }
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
            .px(px(sp::S))
            .py(px(sp::XS));
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
                // Bump deliberado del look-and-feel GP (antes 2px/`sp::XS`):
                // la franja de tasks se lee más cómoda con el mismo aire
                // horizontal que una fila de pane.
                .px(px(sp::S))
                // Mismo tratamiento que `render_row` (redondeo constante,
                // hover gateado fuera de la fila bajo cursor de franja) —
                // consistencia visual entre las dos únicas listas de
                // fila-por-fila de la GUI. GP review: SIN `.cursor_pointer()`
                // aquí — a diferencia de `render_row` (que sí navega al
                // click), el click en una fila de tasks no hace nada (F9
                // cancela por teclado, ver `task_cursor`); un cursor de mano
                // habría sido una afordancia falsa.
                .rounded(px(sp::RADIUS_ROW))
                .child(SharedString::from(line));
            if selected {
                row = row.bg(chrome.sel_bg);
                // Sobrescribe el `header_fg` heredado del contenedor: la fila
                // seleccionada necesita SU propio contraste sobre `sel_bg`,
                // no el pensado para `header_bg` (ver doc de `ChromeColors`).
                if let Some(fg) = chrome.sel_fg {
                    row = row.text_color(fg);
                }
            } else {
                // Mismo gate que `render_row`: no apilar `hover_bg` sobre
                // `sel_bg` (ver comentario allí).
                row = row.hover(|s| s.bg(chrome.hover_bg));
            }
            strip = strip.child(row);
        }
        strip
    }

    /// Pinta la vista de ajustes a pantalla COMPLETA (`app.settings`, F11,
    /// S4): cabecera con la búsqueda/buffer de edición, la lista agrupada
    /// (General/Plugins, mismo criterio de cabeceras intercaladas que
    /// `draw_settings` en la TUI — `ui.rs`) y dos líneas de pie (descripción
    /// de la fila seleccionada + status/hint). Nombre/descripción son texto
    /// PROPIO del binario (Fluent, jamás de un tercero) — no hace falta
    /// `display_name` en las filas, solo en la query/buffer (SÍ son tecleo
    /// del usuario). Sin `uniform_list`: el catálogo es de un puñado de
    /// filas (issue #87 no aplica, mismo criterio que el visor).
    fn render_settings(&self, chrome: &ChromeColors, cx: &mut Context<Self>) -> impl IntoElement {
        // INVARIANTE: solo se llama desde `render` cuando `self.settings_view`
        // es `Some` (comprobado justo antes de esta llamada).
        let view = self.settings_view.as_ref().expect(
            "render_settings: self.settings_view es Some (invariante del caller, ver `render`)",
        );
        let s = &view.state;

        let header_text = if let Some(buf) = s.edit_buffer() {
            let row_name = s
                .visible()
                .get(s.cursor())
                .map(|&real| s.rows()[real].name.as_str())
                .unwrap_or_default();
            let (buf_txt, _) = norte_frontend::display_name(buf.as_bytes());
            format!("{row_name}: {buf_txt}_")
        } else {
            let (q, _) = norte_frontend::display_name(s.query_display().as_bytes());
            format!("⌕ {q}")
        };

        let mut body = div()
            .id("settings-rows")
            .role(gpui::Role::List)
            .aria_label(norte_i18n::t("settings-title"))
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .font(self.fonts.ui.clone());
        if s.visible().is_empty() {
            body = body.child(div().px(px(sp::S)).child(SharedString::from("—")));
        } else {
            let mut general_header = false;
            let mut plugins_header = false;
            for (pos, &real) in s.visible().iter().enumerate() {
                let row = &s.rows()[real];
                if row.is_plugins_note() {
                    if !plugins_header {
                        body = body.child(settings_section_header(
                            norte_i18n::t("settings-section-plugins"),
                            chrome,
                        ));
                        plugins_header = true;
                    }
                } else if !general_header {
                    body = body.child(settings_section_header(
                        norte_i18n::t("settings-section-general"),
                        chrome,
                    ));
                    general_header = true;
                }
                let selected = pos == s.cursor();
                body = body.child(self.render_settings_row(pos, row, selected, chrome, cx));
            }
        }

        let desc = s.selected_desc().unwrap_or_default();
        let hint = if s.is_editing() {
            norte_i18n::t("settings-edit-hint")
        } else {
            norte_i18n::t("settings-hint-gui")
        };
        let status = view.status.as_ref();

        div()
            .id("settings-view")
            .role(gpui::Role::Document)
            .aria_label(norte_i18n::t("settings-title"))
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .border_2()
            .border_color(chrome.border_focus)
            .bg(chrome.pane_bg_focus)
            .child(
                div()
                    .px(px(sp::S))
                    .py(px(sp::XS))
                    .bg(chrome.header_bg)
                    .text_color(chrome.header_fg)
                    .truncate()
                    .child(SharedString::from(format!(
                        "{}  {header_text}",
                        norte_i18n::t("settings-title")
                    ))),
            )
            .child(body)
            .child(
                div()
                    .px(px(sp::S))
                    .py(px(1.0)) // sub-XS: acento fino de una línea, fuera de la escala a propósito
                    .truncate()
                    .child(SharedString::from(desc.to_owned())),
            )
            .child(
                div()
                    .px(px(sp::S))
                    .py(px(1.0)) // sub-XS: acento fino de una línea, fuera de la escala a propósito
                    .bg(chrome.quick_bg)
                    .text_color(if status.is_some_and(|st| st.error) {
                        chrome.err_fg
                    } else {
                        chrome.quick_fg
                    })
                    .truncate()
                    .child(SharedString::from(
                        status.map_or(hint, |st| st.message.clone()),
                    )),
            )
    }

    /// Pinta UNA fila de la vista de ajustes: nombre (columna fija) + valor
    /// mono (flex, con el aviso "requiere reinicio" pegado si
    /// `settings_view::gui_applies_live` es `false` para su id) para las
    /// filas General; solo el nombre para la nota informativa de Plugins.
    /// Mismo idioma visual que `render_row` (hover/selección/redondeo) —
    /// click fija el cursor en `pos` y activa (`on_settings_row_click`),
    /// mismo criterio uniforme para TODAS las filas: sobre la nota de
    /// Plugins `activate` ya es un no-op seguro (`SettingsState::activate`).
    fn render_settings_row(
        &self,
        pos: usize,
        row: &norte_frontend::settings::Row,
        selected: bool,
        chrome: &ChromeColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut value_text = row.value.clone();
        if let Some(id) = row.id()
            && !settings_view::gui_applies_live(id)
        {
            value_text = format!(
                "{value_text}  ({})",
                norte_i18n::t("settings-restart-badge")
            );
        }

        let mut r = div()
            .id(format!("settings-row-{pos}"))
            .role(gpui::Role::ListItem)
            .aria_label(row.name.clone())
            .aria_selected(selected)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(sp::S))
            .px(px(sp::S))
            .py(px(1.0)) // sub-XS: acento fino de una línea, fuera de la escala a propósito
            .rounded(px(sp::RADIUS_ROW))
            .cursor_pointer()
            .child(
                div()
                    .w(px(240.0))
                    .truncate()
                    .child(SharedString::from(row.name.clone())),
            );
        if !row.is_plugins_note() {
            r = r.child(
                div()
                    .flex_1()
                    .truncate()
                    .font(self.fonts.mono.clone())
                    .child(SharedString::from(value_text)),
            );
        }
        if selected {
            r = r.bg(chrome.sel_bg);
            if let Some(fg) = chrome.sel_fg {
                r = r.text_color(fg);
            }
        } else {
            r = r.hover(|s| s.bg(chrome.hover_bg));
        }
        r.on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _ev: &MouseDownEvent, _w, cx| {
                this.on_settings_row_click(pos, cx);
            }),
        )
        .into_any_element()
    }

    /// Paints the shortcut editor (K3c c4, `ctrl+k` from settings): a
    /// full-screen swap with the same visual language as `render_settings`,
    /// which it covers.
    ///
    /// Three things it says that the settings view does not have to:
    ///
    /// - the FILTER header, which is also the capture prompt: while a
    ///   capture is in flight the header stops being a filter and says what
    ///   was captured and what the map thinks of it, because that is the
    ///   decision the reader is holding;
    /// - the FOOTER, which changes with the mode — browsing, waiting for a
    ///   key, holding a verdict — since the three have different ways out
    ///   and a footer that named only one would leave a reader pressing
    ///   `esc` at a screen that had already told them something else;
    /// - a WINDOW over the rows. This list is hundreds of rows long (every
    ///   bound key of two screens, plus every command with none), so unlike
    ///   the fifteen-row settings catalogue it cannot be laid out whole
    ///   every frame. `rows` is DERIVED from the viewport
    ///   ([`Self::shortcut_rows`]), never a constant, for the reason
    ///   [`Self::help_rows_for`] documents at length — and here it is worse
    ///   than a lost cursor: `ctrl+u` deletes the binding under the cursor,
    ///   so a cursor scrolled off the bottom of a short window would be a
    ///   row the reader never saw being named confidently as removed.
    ///
    /// # Invariant
    ///
    /// `settings_view` is `Some` whenever this paints: the only opener is
    /// `on_settings_key`. That is what lets the flash suppression, the
    /// pending-sequence strip and `overlay_in_front` keep testing
    /// `settings_view` alone and still be right about this screen.
    fn render_shortcuts(
        &self,
        chrome: &ChromeColors,
        rows: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // INVARIANT: only called from `render` when `self.shortcuts_view` is
        // `Some` (checked immediately before the call).
        let view = self.shortcuts_view.as_ref().expect(
            "render_shortcuts: self.shortcuts_view es Some (invariante del caller, ver `render`)",
        );
        let s = &view.state;
        let lang = norte_i18n::active();
        let capture = s.capture();

        let header_text = match capture {
            None => {
                let (q, _) = norte_frontend::display_name(s.query_display().as_bytes());
                format!("⌕ {q}")
            }
            Some(c) if c.is_waiting() => norte_i18n::t("shortcuts-capture-hint"),
            Some(c) => {
                // Every chord here is already painted (`paint_chord`) and
                // every command is a catalogue name, so nothing needs masking
                // again — `verdict_message`'s own doc says so.
                let chord = norte_frontend::keymap::paint_chord(
                    &c.seq()
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" "),
                );
                let verdict = c.verdict().map_or_else(String::new, |v| {
                    norte_frontend::shortcuts::verdict_message(v, lang)
                });
                format!("{chord} — {verdict}")
            }
        };

        // The window over the rows: same arithmetic the help sidebar uses,
        // over a height that is measured rather than assumed.
        let offset = Self::sidebar_offset(s.cursor(), s.visible().len(), rows);
        let mut body = div()
            .id("shortcuts-rows")
            .role(gpui::Role::List)
            .aria_label(norte_i18n::t("shortcuts-title"))
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .font(self.fonts.ui.clone());
        if s.visible().is_empty() {
            // Same as `render_settings`: an empty body is indistinguishable
            // from a render that failed.
            body = body.child(div().px(px(sp::S)).child(SharedString::from("—")));
        }
        for (n, &real) in s.visible().iter().enumerate().skip(offset).take(rows) {
            body = body.child(self.render_shortcut_row(
                n,
                &s.rows()[real],
                n == s.cursor(),
                chrome,
                cx,
            ));
        }

        let footer = match capture {
            None => norte_i18n::t("gui-shortcuts-hint"),
            Some(c) if c.is_waiting() => norte_i18n::t("gui-shortcuts-capture-note"),
            Some(_) => norte_i18n::t("shortcuts-confirm-hint"),
        };
        let status = view.status.as_ref();

        div()
            .id("shortcuts-view")
            .role(gpui::Role::Document)
            .aria_label(norte_i18n::t("shortcuts-title"))
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .border_2()
            .border_color(chrome.border_focus)
            .bg(chrome.pane_bg_focus)
            .child(
                div()
                    .px(px(sp::S))
                    .py(px(sp::XS))
                    .bg(chrome.header_bg)
                    .text_color(chrome.header_fg)
                    .truncate()
                    .child(SharedString::from(format!(
                        "{}  {header_text}",
                        norte_i18n::t("shortcuts-title")
                    ))),
            )
            .child(body)
            .child(
                div()
                    .px(px(sp::S))
                    .py(px(1.0)) // sub-XS: acento fino de una línea, fuera de la escala a propósito
                    .bg(chrome.quick_bg)
                    .text_color(if status.is_some_and(|st| st.error) {
                        chrome.err_fg
                    } else {
                        chrome.quick_fg
                    })
                    .truncate()
                    .child(SharedString::from(
                        status.map_or(footer, |st| st.message.clone()),
                    )),
            )
    }

    /// One row of the shortcut editor: the painted chord in a fixed column
    /// (or "(no key)" for a command nothing presses), the catalogue label,
    /// and — when this build cannot run it — the short reason, with the whole
    /// row dimmed.
    ///
    /// Every string here arrives ALREADY safe: `ShortcutRow::chord` is
    /// `paint_chord`ed by the shared builder (a project `keymap.toml` can
    /// bind any lone codepoint) and the label comes from the catalogue or,
    /// for a `lua:` command, from a name that passed the charset. The dim
    /// survives selection: an unavailable row that the cursor made look
    /// ordinary would offer a key that does nothing.
    fn render_shortcut_row(
        &self,
        pos: usize,
        row: &norte_frontend::shortcuts::ShortcutRow,
        selected: bool,
        chrome: &ChromeColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let unavailable = row.avail != norte_frontend::keymap::Availability::Here;
        let chord = if row.is_bound() {
            row.chord.clone()
        } else {
            norte_i18n::t("shortcuts-no-key")
        };
        let label = if row.reason.is_empty() {
            row.label.clone()
        } else {
            format!("{} — {}", row.label, row.reason)
        };
        let mut r = div()
            .id(format!("shortcut-row-{pos}"))
            .role(gpui::Role::ListItem)
            .aria_label(format!("{chord} {label}"))
            .aria_selected(selected)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(sp::S))
            .px(px(sp::S))
            .py(px(1.0)) // sub-XS: acento fino de una línea, fuera de la escala a propósito
            .rounded(px(sp::RADIUS_ROW))
            .cursor_pointer()
            .child(
                div()
                    .w(px(160.0))
                    .truncate()
                    .font(self.fonts.mono.clone())
                    .child(SharedString::from(chord)),
            )
            .child(div().flex_1().truncate().child(SharedString::from(label)));
        if selected {
            r = r.bg(chrome.sel_bg);
            if let Some(fg) = chrome.sel_fg
                && !unavailable
            {
                r = r.text_color(fg);
            }
        } else {
            r = r.hover(|st| st.bg(chrome.hover_bg));
        }
        if unavailable {
            r = r.text_color(chrome.quick_fg);
        }
        r.on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _ev: &MouseDownEvent, _w, cx| {
                this.on_shortcuts_row_click(pos, cx);
            }),
        )
        .into_any_element()
    }

    /// Pinta la paleta de comandos (G3c, `ctrl+p`): panel CENTRADO de ancho
    /// fijo (a diferencia de `render_settings`, que es a pantalla completa
    /// — la paleta es un OVERLAY sobre el dual-pane, mismo idioma visual
    /// que `render_modal`), lista filtrada con la fila bajo cursor
    /// resaltada. Sin click por fila (teclado-only, mismo criterio
    /// alcanzable que la TUI): el volumen de esta pasada ya cubre filtro +
    /// navegación + Enter, que es la superficie que el plan pide.
    fn render_palette(
        &self,
        view: &palette_view::PaletteView,
        chrome: &ChromeColors,
    ) -> impl IntoElement {
        let (q, _) = norte_frontend::display_name(view.query_display().as_bytes());
        let mut body = div()
            .id("palette-rows")
            .role(gpui::Role::List)
            .aria_label(norte_i18n::t("palette-title"))
            .flex()
            .flex_col()
            .overflow_hidden()
            .max_h(px(420.0))
            .font(self.fonts.ui.clone());
        if view.visible().is_empty() {
            body = body.child(div().px(px(sp::S)).child(SharedString::from("—")));
        } else {
            for (pos, &real) in view.visible().iter().enumerate() {
                let row = &view.rows()[real];
                let selected = pos == view.cursor();
                let mut r = div()
                    .id(format!("palette-row-{pos}"))
                    .role(gpui::Role::ListItem)
                    .aria_label(row.text.clone())
                    .aria_selected(selected)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(sp::S))
                    .px(px(sp::S))
                    .py(px(1.0)) // sub-XS: acento fino de una línea
                    .rounded(px(sp::RADIUS_ROW))
                    .child(
                        div()
                            .w(px(72.0))
                            .truncate()
                            .text_color(chrome.quick_fg)
                            .child(SharedString::from(row.chord.clone())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .child(SharedString::from(row.text.clone())),
                    );
                if selected {
                    r = r.bg(chrome.sel_bg);
                    if let Some(fg) = chrome.sel_fg {
                        r = r.text_color(fg);
                    }
                }
                body = body.child(r);
            }
        }
        div()
            .id("palette-view")
            .role(gpui::Role::Document)
            .aria_label(norte_i18n::t("palette-title"))
            .w(px(560.0))
            .flex()
            .flex_col()
            .border_2()
            .border_color(chrome.border_focus)
            .bg(chrome.pane_bg_focus)
            .child(
                div()
                    .px(px(sp::S))
                    .py(px(sp::XS))
                    .bg(chrome.header_bg)
                    .text_color(chrome.header_fg)
                    .truncate()
                    .child(SharedString::from(format!("⌕ {q}"))),
            )
            .child(body)
            .child(
                div()
                    .px(px(sp::S))
                    .py(px(1.0)) // sub-XS: acento fino de una línea
                    .bg(chrome.quick_bg)
                    .text_color(chrome.quick_fg)
                    .truncate()
                    .child(SharedString::from(norte_i18n::t("palette-hint"))),
            )
    }

    /// Text of one sidebar row, or `None` for a row that must not be painted.
    ///
    /// The `None` is the whole point. A group header is named by
    /// `help-group-{tag}`, and `norte_i18n::t` answers a MISSING message with the
    /// id itself — so a tag with no catalogue entry paints a literal
    /// `help-group-…` at the reader. That is not hypothetical: the model always
    /// emits a group for the synthetic keyboard page (`KEYS_TAG`), and both
    /// catalogues deliberately have no entry for it, because the row underneath
    /// already wears that name. Suppressing the header is what the TUI does too
    /// (`keys_only_group`).
    ///
    /// Answering `None` for ANY untranslated tag rather than for `keys` by name
    /// means a future tag cannot regress into painting its id — and
    /// `los_grupos_del_corpus_tienen_nombre_traducido` is what stops a corpus group
    /// from disappearing silently instead.
    #[must_use]
    fn sidebar_label(row: &norte_frontend::help::SidebarRow) -> Option<String> {
        match row {
            norte_frontend::help::SidebarRow::Group { tag } => {
                let id = format!("help-group-{tag}");
                let text = norte_i18n::t(&id);
                (text != id).then_some(text)
            }
            // The title verbatim: it is already the corpus' own, and a plugin's was
            // masked at parse and made non-blank at ingest.
            norte_frontend::help::SidebarRow::Topic { title, .. } => Some(title.clone()),
        }
    }

    /// First sidebar row to paint so that `cursor` stays inside a window of
    /// `height` rows.
    ///
    /// The minimum that keeps the selection visible: it scrolls only when the
    /// cursor would leave the window, so a reader arrowing through a short list
    /// never sees the list move under them.
    #[must_use]
    fn sidebar_offset(cursor: usize, len: usize, height: usize) -> usize {
        if height == 0 || len <= height {
            return 0;
        }
        let last_start = len - height;
        // Keep the cursor one row inside the bottom edge where there is room, so
        // the next row down is visible before it is selected.
        cursor
            .saturating_sub(height.saturating_sub(1))
            .min(last_start)
    }

    /// Colour of a help fragment's theme role, resolved against the active
    /// theme with the same fallbacks the rest of the chrome uses.
    ///
    /// A dimmed row overrides it wholesale with `quick_fg`: the reason it
    /// carries is prose about why nothing will happen, and painting half of
    /// that row in the key colour would keep promising a key.
    fn help_role_color(&self, role: Role, chrome: &ChromeColors) -> gpui::Rgba {
        match role {
            Role::Title => chrome.header_fg,
            Role::Mark => chrome_mark_fg(&self.theme),
            Role::Info => chrome.quick_fg,
            Role::Warning => chrome.err_fg,
            _ => chrome.fg,
        }
    }

    /// Paints the help overlay (H3f, `F1`): sidebar, body, filter header and
    /// hint footer — the same visual language as the palette, because it is
    /// the same model at a lower density and not a second design.
    ///
    /// Nothing here masks: every string arrives already safe (see
    /// `help_render`'s module doc, and `help_view::plugin_text` for the
    /// snapshot's strings).
    fn render_help(
        &self,
        view: &help_view::HelpView,
        chrome: &ChromeColors,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let state = &view.state;
        let focus_topics = state.focus() == norte_frontend::help::Focus::Topics;
        let (frame_w, frame_h) = self.help_frame(window.viewport_size());
        let rows = self.help_rows_for(frame_h);
        // La lateral crece con el marco pero no sin freno: pasado cierto ancho
        // solo aleja el índice de la página que describe.
        let side_w = (frame_w * 0.26).clamp(180.0, 320.0);
        // Texto SECUNDARIO derivado de la prosa, no del color de quick-search.
        // Ese era `quick_fg` y un tema puede darle el contraste que quiera
        // contra SU fondo (el del resaltado), no contra este panel: los
        // enlaces y los nombres de comando salían casi ilegibles. Derivarlo
        // del `fg` que el tema sí garantiza aquí lo hace legible en cualquier
        // tema, y sigue leyéndose como secundario.
        let secondary = gpui::Rgba {
            a: 0.8,
            ..chrome.fg
        };

        // ── sidebar ──────────────────────────────────────────────────────
        let mut side = div()
            .id("help-topics")
            .role(gpui::Role::List)
            .aria_label(norte_i18n::t("help-title"))
            .w(px(side_w))
            .relative()
            .flex()
            .flex_col()
            .overflow_hidden()
            .border_r_1()
            .border_color(if focus_topics {
                chrome.border_focus
            } else {
                chrome.border_unfocus
            })
            .font(self.fonts.ui.clone());
        // The sidebar scrolls like the body does: with enough plugin pages the
        // list outgrows the panel, and without an offset the cursor walks off
        // the bottom while the body keeps changing for a row nobody can see.
        let side_first = Self::sidebar_offset(state.cursor(), state.rows().len(), rows);
        for (i, row) in state.rows().iter().enumerate().skip(side_first).take(rows) {
            let Some(label) = Self::sidebar_label(row) else {
                // A group header with nothing to say: today only the synthetic
                // keyboard group, whose single member already wears the same
                // name. Skipped rather than painted, because `help-group-keys`
                // does not exist in either catalogue — `norte_i18n::t` answers
                // a missing message with the id, so this row shipped a literal
                // `help-group-keys` to every reader who pressed F1. The TUI
                // suppresses the same header structurally (`keys_only_group`).
                continue;
            };
            let selected = i == state.cursor() && focus_topics;
            let mut r = div()
                .id(format!("help-topic-{i}"))
                .role(gpui::Role::ListItem)
                // Una fila = una fila, por la misma razón que la cabecera:
                // `help_rows` cuenta con ello, y con filas de alto variable el
                // cursor se salía por debajo del panel sin que nada lo dijera.
                .h(self.fonts.row_h)
                .flex()
                .items_center()
                // Mismo motivo que en el cuerpo: una fila que encoge deja su
                // texto pintado sobre la de abajo. Aquí no se ha visto porque
                // cada fila es una línea `truncate()`, pero con bastantes
                // páginas de extensión la lista desborda la altura del panel y
                // el defecto es idéntico.
                .flex_shrink_0()
                .px(px(sp::S))
                .py(px(1.0)) // sub-XS: acento fino de una línea
                .rounded(px(sp::RADIUS_ROW))
                .truncate();
            r = match row {
                // Cabecera de GRUPO: separada por arriba, con línea, en el
                // color secundario y SIN la sangría de los temas. Antes era
                // una fila más en un color parecido, así que el índice se leía
                // como una lista plana de catorce cosas — la jerarquía estaba
                // en los datos y no en la pantalla.
                norte_frontend::help::SidebarRow::Group { .. } => r
                    .mt(px(sp::M))
                    .border_t_1()
                    .border_color(chrome.border_unfocus)
                    .text_color(secondary)
                    .child(SharedString::from(label)),
                // Un TEMA cuelga de su grupo: sangría mayor, y el seleccionado
                // lleva además una barra de acento a la izquierda — el fondo
                // solo ya se perdía contra el del panel en los temas oscuros.
                norte_frontend::help::SidebarRow::Topic { .. } => r
                    .pl(px(sp::L))
                    .aria_selected(selected)
                    .child(SharedString::from(label)),
            };
            if selected {
                r = r
                    .bg(chrome.sel_bg)
                    .border_l_2()
                    .border_color(chrome.border_focus)
                    .pl(px(sp::L - 2.0));
                if let Some(fg) = chrome.sel_fg {
                    r = r.text_color(fg);
                }
            }
            // El puntero (deuda que oscar cazó mirando la ventana): un clic
            // ATERRIZA donde aterrizaría la flecha — enseña la página y no
            // empuja un paso al rastro. Solo en filas de tema: una cabecera de
            // grupo no es una página, y `HelpState::click_row` lo ignora de
            // todos modos (el pintor puede ir un frame por detrás del modelo).
            if matches!(row, norte_frontend::help::SidebarRow::Topic { .. }) {
                let hover_bg = chrome.hover_bg;
                r = r
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover_bg))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev: &MouseDownEvent, _w, cx| {
                            if let Some(view) = &mut this.help {
                                view.state.click_row(i);
                            }
                            cx.notify();
                        }),
                    );
            }
            side = side.child(r);
        }
        // La barra de la lateral: su ventana la deriva `sidebar_offset` del
        // cursor, así que el pulgar dice dónde cae esa ventana en la lista
        // entera — con muchas extensiones, la lista no cabe y hasta ahora nada
        // lo decía.
        // Indicador, no control: la ventana de la lateral la DERIVA
        // `sidebar_offset` del cursor, así que arrastrar este pulgar solo
        // podría mover el cursor — y mover el cursor ABRE la página que pisa.
        // Una barra de scroll que cambia lo que estás leyendo no es una barra
        // de scroll. La rueda y las flechas hacen el trabajo.
        if let Some((arriba, alto)) = scroll_geometry(side_first, rows, state.rows().len()) {
            side = side.child(
                div()
                    .absolute()
                    .top(gpui::relative(arriba))
                    .right_0()
                    .w(px(sp::XS))
                    .h(gpui::relative(alto))
                    .rounded(px(sp::XS / 2.0))
                    .bg(chrome.border_unfocus),
            );
        }

        // ── body ─────────────────────────────────────────────────────────
        let cuerpo = self.help_body(view);
        let cuerpo_total = cuerpo.len();
        // El TÍTULO sale del scroll y se convierte en la cabecera del panel de
        // detalle, con el mismo texto que la fila del índice: la página se
        // queda etiquetada por larga que sea, en vez de perder su nombre en
        // cuanto el lector baja un párrafo.
        let titulo = cuerpo
            .iter()
            .find(|l| l.title)
            .map(|l| l.spans.iter().map(|s| s.text.as_str()).collect::<String>())
            .unwrap_or_default();
        let mut body = div()
            .id("help-body")
            .role(gpui::Role::Document)
            .relative()
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .px(px(sp::S))
            .font(self.fonts.ui.clone());
        // `body_scroll` is what `pagedown` and `reveal` move; skipping is how
        // an `overflow_hidden` column honours it. The `take` is not cosmetic:
        // `Limits::untrusted()` bounds a plugin page at 64 KiB and 512 blocks
        // but NOT at line count — one bullet block of 64 KiB is ~16 000 lines —
        // and without a bound every one of them was laid out and shaped by GPUI
        // on every frame the overlay stayed open. The window height is already
        // known, so the bound already existed; it simply was not applied.
        for (n, line) in cuerpo
            .into_iter()
            // El título ya vive en la cabecera del panel: pintarlo otra vez
            // aquí sería el mismo texto dos veces, una de ellas moviéndose.
            .filter(|l| !l.title)
            .skip(state.body_scroll())
            .take(rows)
            .enumerate()
        {
            let dim = line.dim;
            let focused = !focus_topics && line.action == Some(state.action_cursor());
            // Con `id` SIEMPRE, y no solo en las filas ejecutables: `id`
            // cambia el tipo del elemento (`Div` → `Stateful<Div>`), así que
            // ponerlo en una rama daría dos tipos para el mismo hijo. El
            // índice es la posición en la VENTANA visible, que es lo único
            // estable dentro de un frame.
            // BLOQUE, no fila flex: el hijo de un contenedor de bloque recibe
            // el ancho del contenedor, que es la restricción con la que
            // `StyledText` decide dónde partir. Como flex-row, el texto se
            // medía a su ancho natural (MaxContent) y no partía nunca. Ya no
            // hay varios hijos que colocar en línea — los fragmentos son runs
            // de UN texto, ver abajo.
            let mut l = div()
                .id(("help-line", n))
                // Las dos mitades del mismo defecto, y ninguna es cosmética.
                //
                // `w_full`: sin ancho definido la fila se dimensiona a su
                // CONTENIDO, así que `flex_wrap` no tenía dónde envolver y una
                // línea larga se salía de la caja — el `overflow_hidden` de la
                // columna la cortaba a media palabra. Una página de plugin,
                // que es la que trae texto que nadie escribió pensando en este
                // ancho, se leía a la mitad.
                //
                // `flex_shrink_0`: por defecto un hijo de flex ENCOGE, y el
                // texto no encoge con él — se sale de su caja y se pinta
                // ENCIMA de la fila siguiente. Con párrafos largos la página
                // entera aparecía duplicada y superpuesta. Que una fila
                // conserve su alto natural es lo que hace que apilarlas sea
                // apilarlas.
                .w_full()
                .flex_shrink_0()
                // NO `gap` between the fragments of one line: they are the
                // pieces of a running sentence (`Block::Paragraph` emits one
                // line whose spans split at every `code`/`**strong**`), and a
                // gap inserts word boundaries the author never wrote. The
                // spacing that belongs there is already inside `Span::Text`.
                .pl(px(f32::from(line.indent) * sp::S))
                .rounded(px(sp::RADIUS_ROW));
            if line.mono {
                l = l.font(self.fonts.mono.clone());
            }
            if line.badge {
                // The provenance line is the ONE thing on screen that tells
                // third-party prose from norte's own, so it does not read as
                // one more `Role::Info` paragraph: it gets the page's width to
                // itself and a rule under it. Painting it identically to the
                // body is how a page that says "from an extension" still looks
                // like something norte wrote.
                l = l.border_b_1().border_color(chrome.border_unfocus);
            }
            if focused {
                l = l.bg(chrome.sel_bg);
            }
            // Una fila EJECUTABLE (o un enlace) responde al clic por el MISMO
            // camino que `Enter`: `help_view::activate`, contra el resolver
            // congelado. Un ratón que ejecuta lo que el teclado rechaza es
            // exactamente lo que ese camino compartido impide.
            if let Some(k) = line.action {
                let hover_bg = chrome.hover_bg;
                l = l
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover_bg))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev: &MouseDownEvent, _w, cx| {
                            this.on_help_action_click(k, cx);
                        }),
                    );
            }
            // UN elemento de texto por línea, con los colores como RUNS, y no
            // un `div` por fragmento.
            //
            // El motivo es la envoltura, no el ahorro: un `div` por fragmento
            // se mide sin restricción de ancho, así que un párrafo largo —un
            // solo fragmento— se salía de la caja y el `overflow_hidden` lo
            // cortaba a media palabra; lo único que envolvía era la frontera
            // ENTRE fragmentos, que es por lo que un `F1` acababa solo en su
            // renglón con la frase debajo. `StyledText` recibe el ancho de la
            // fila y parte por palabras, que es lo que hace un párrafo.
            if line.columns {
                // Una fila de TABLA: sus fragmentos son CELDAS. Van en una fila
                // flex, cada una con su parte del ancho, porque concatenarlas
                // en un solo texto las pega («confirmationshall I touch…») y
                // porque una tabla sin columnas no es una tabla. Cada celda
                // envuelve dentro de lo suyo: el ancho definido de la celda es
                // la restricción, igual que el de la fila lo es para la prosa.
                let mut fila = div().flex().flex_row().w_full().gap(px(sp::M));
                for span in &line.spans {
                    let color = if dim {
                        secondary
                    } else {
                        self.help_role_color(span.role, chrome)
                    };
                    fila = fila.child(
                        div()
                            .flex_1()
                            .text_color(color)
                            .child(SharedString::from(span.text.clone())),
                    );
                }
                l = l.child(fila);
            } else if !line.spans.is_empty() {
                let mut texto = String::new();
                let mut runs: Vec<(std::ops::Range<usize>, gpui::HighlightStyle)> = Vec::new();
                for span in &line.spans {
                    let color = if dim {
                        secondary
                    } else if span.link {
                        // Un enlace se ve como un enlace: el acento del tema.
                        chrome.border_focus
                    } else if span.role == Role::Info {
                        secondary
                    } else {
                        self.help_role_color(span.role, chrome)
                    };
                    let desde = texto.len();
                    texto.push_str(&span.text);
                    runs.push((
                        desde..texto.len(),
                        gpui::HighlightStyle {
                            color: Some(color.into()),
                            // Subrayado, y no solo color: un tema es libre de
                            // elegir sus colores y el subrayado no se lo puede
                            // quitar. Es lo que distingue un enlace de una
                            // palabra en otro tono.
                            underline: span.link.then(|| gpui::UnderlineStyle {
                                thickness: px(1.0),
                                color: Some(chrome.border_focus.into()),
                                wavy: false,
                            }),
                            ..Default::default()
                        },
                    ));
                }
                l = l.child(gpui::StyledText::new(SharedString::from(texto)).with_highlights(runs));
            }
            // A blank line still occupies one: it is the paragraph separation
            // the renderer emitted, and collapsing it would run the prose
            // together.
            if line.spans.is_empty() {
                l = l.child(div().child(SharedString::from(" ")));
            }
            body = body.child(l);
        }
        // La del cuerpo, con la misma salvedad que `HELP_BODY_ROWS`: el total
        // son LÍNEAS del modelo y una línea larga ocupa varios renglones al
        // envolver, así que el pulgar es una aproximación por arriba. Dice
        // "queda página", que es lo que no decía nada.
        if let Some((arriba, alto)) = scroll_geometry(state.body_scroll(), rows, cuerpo_total) {
            // El canal ENTERO es la superficie de ratón, no solo el pulgar:
            // pulsar en el canal salta ahí (el gesto que espera cualquiera) y
            // el arrastre continúa aunque el puntero se salga de ocho píxeles
            // de ancho, porque el movimiento se escucha en el marco.
            body = body.child(
                div()
                    .id("help-body-scroll")
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .w(px(sp::M))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                            this.help_dragging = true;
                            this.help_scroll_to(ev.position.y, window, cx);
                        }),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(gpui::relative(arriba))
                            .right_0()
                            .w(px(sp::XS))
                            .h(gpui::relative(alto))
                            .rounded(px(sp::XS / 2.0))
                            .bg(chrome.border_focus),
                    ),
            );
        }

        // ── frame ────────────────────────────────────────────────────────
        let header = if state.filtering() {
            format!("⌕ {}", state.filter_display())
        } else {
            norte_i18n::t("help-title")
        };
        div()
            .id("help-view")
            .role(gpui::Role::Document)
            .aria_label(norte_i18n::t("help-title"))
            .on_scroll_wheel(cx.listener(|this, ev: &gpui::ScrollWheelEvent, _w, cx| {
                this.on_help_scroll(ev.delta, cx);
            }))
            // El arrastre se escucha en el MARCO y no en el canal: ocho píxeles
            // de ancho son imposibles de seguir con el ratón, y quien arrastra
            // una barra se sale de ella constantemente. Soltar cuenta aquí por
            // lo mismo.
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, window, cx| {
                if this.help_dragging {
                    this.help_scroll_to(ev.position.y, window, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _ev: &MouseUpEvent, _w, cx| {
                    if this.help_dragging {
                        this.help_dragging = false;
                        cx.notify();
                    }
                }),
            )
            .w(px(frame_w))
            .h(px(frame_h))
            .flex()
            .flex_col()
            .border_2()
            .border_color(chrome.border_focus)
            .bg(chrome.pane_bg_focus)
            .child(
                div()
                    // Alto de UNA fila, y no un `py` que lo deje al azar del
                    // interlineado: `help_rows` divide por él para saber
                    // cuántas filas caben, y una cabecera de alto desconocido
                    // convertía esa cuenta en una estimación.
                    .h(self.fonts.row_h)
                    .flex()
                    .items_center()
                    .px(px(sp::S))
                    .bg(chrome.header_bg)
                    .text_color(chrome.header_fg)
                    .truncate()
                    .child(SharedString::from(header)),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .overflow_hidden()
                    .child(side)
                    .child(
                        // El panel de detalle: su propio título arriba, con
                        // regla debajo, y el cuerpo desplazable por debajo.
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .child(
                                div()
                                    .h(self.fonts.row_h)
                                    .flex()
                                    .items_center()
                                    .px(px(sp::M))
                                    .border_b_1()
                                    .border_color(chrome.border_unfocus)
                                    .text_color(chrome.fg)
                                    .truncate()
                                    .child(SharedString::from(titulo)),
                            )
                            .child(body),
                    ),
            )
            .child(
                div()
                    // Alto de UNA fila, como la cabecera y por lo mismo:
                    // `help_rows` resta las dos para saber qué queda.
                    .h(self.fonts.row_h)
                    .flex()
                    .items_center()
                    .px(px(sp::S))
                    .bg(chrome.quick_bg)
                    // The footer says why a dimmed row did nothing when there
                    // is something to say, and the key hints otherwise. INSIDE
                    // the frame on purpose: the window-level flash is painted
                    // before this overlay's scrim, so a reason routed there was
                    // covered by 67% black the moment it appeared — dimming
                    // that cannot explain itself is decorative.
                    .text_color(if view.status.is_some() {
                        chrome.err_fg
                    } else {
                        chrome.quick_fg
                    })
                    .truncate()
                    .child(SharedString::from(
                        view.status
                            .clone()
                            .unwrap_or_else(|| norte_i18n::t("help-hint-gui")),
                    )),
            )
    }

    /// The lines of the open page.
    ///
    /// Three cases, and the third is the one worth stating: the synthetic
    /// keyboard page comes from the generated cheatsheet, a corpus or plugin
    /// page from the renderer — and anything else (a plugin page still in
    /// flight) is EMPTY, never the cheatsheet. Nothing else on screen tells the
    /// two apart, and the whole keymap appearing under an extension's name
    /// would read as that extension's own documentation.
    fn help_body(&self, view: &help_view::HelpView) -> Vec<help_render::HelpLine> {
        let state = &view.state;
        if state.current().as_str() == norte_frontend::help::KEYS_ID {
            // Already `HelpLine`s (K3b, `help_view::keys_lines`), MONOSPACED
            // because the cheatsheet is a TABLE — its chord column is padded
            // with spaces, and space padding under a proportional face aligns
            // nothing, which is the one thing a key sheet must not be.
            return view.keys_lines.clone();
        }
        let Some(chords) = &self.help_chords else {
            return Vec::new();
        };
        state.current_topic().map_or_else(Vec::new, |topic| {
            help_render::render_topic(topic, state.lang(), chords)
        })
    }

    /// Pinta el picker de columnas (#108 7c, `alt+c`): mismo idioma visual
    /// que la paleta (header/lista/footer, 560px, teclado-only). Labels de
    /// builtins vía Fluent `col-header-*`; ids opacos (`attr:`/`plugin:`/
    /// basura) ENMASCARADOS con `mask_terminal_hazards` y preservados
    /// verbatim en el modelo (#73 — limpiar config es de doctor, no del
    /// picker). El formato es vocabulario ASCII cerrado: seguro en crudo.
    fn render_columns_picker(
        &self,
        view: &columns_view::ColumnsView,
        chrome: &ChromeColors,
    ) -> impl IntoElement {
        let p = &view.picker;
        let target = if p.scheme_override() {
            p.scheme().to_owned()
        } else {
            norte_i18n::t("columns-picker-target-default")
        };
        let title = norte_i18n::ta("columns-picker-title", &[("target", &target)]);
        let mut body = div()
            .id("columns-rows")
            .role(gpui::Role::List)
            .aria_label(title.clone())
            .flex()
            .flex_col()
            .overflow_hidden()
            .max_h(px(420.0))
            .font(self.fonts.ui.clone());
        for (pos, row) in p.rows().iter().enumerate() {
            let selected = pos == p.cursor();
            // Choke point compartido y testeable (encoding 7c H1/H2):
            // masking + cap del label viven en columns_view::row_display.
            let (text, label) = columns_view::row_display(row, p.sort());
            let mut r = div()
                .id(format!("columns-row-{pos}"))
                .role(gpui::Role::ListItem)
                .aria_label(label.clone())
                .aria_selected(selected)
                .flex()
                .flex_row()
                .items_center()
                .px(px(sp::S))
                .py(px(1.0)) // sub-XS: acento fino de una línea
                .rounded(px(sp::RADIUS_ROW))
                .child(div().flex_1().truncate().child(SharedString::from(text)));
            if row.format_locked {
                // Formato fijado por scheme-spec: fila atenuada (7b).
                r = r.opacity(0.6);
            }
            if selected {
                r = r.bg(chrome.sel_bg);
                if let Some(fg) = chrome.sel_fg {
                    r = r.text_color(fg);
                }
            }
            body = body.child(r);
        }
        div()
            .id("columns-picker-view")
            .role(gpui::Role::Document)
            .aria_label(title.clone())
            .w(px(560.0))
            .flex()
            .flex_col()
            .border_2()
            .border_color(chrome.border_focus)
            .bg(chrome.pane_bg_focus)
            .child(
                div()
                    .px(px(sp::S))
                    .py(px(sp::XS))
                    .bg(chrome.header_bg)
                    .text_color(chrome.header_fg)
                    .truncate()
                    .child(SharedString::from(title)),
            )
            .child(body)
            .child(
                div()
                    .px(px(sp::S))
                    .py(px(1.0)) // sub-XS: acento fino de una línea
                    .bg(chrome.quick_bg)
                    .text_color(chrome.quick_fg)
                    .truncate()
                    .child(SharedString::from(norte_i18n::t("columns-picker-hint-gui"))),
            )
    }

    /// Pinta el gestor de extensiones a pantalla COMPLETA (G3c, `f12`):
    /// mismo idioma visual que `render_settings` (header/lista/footer);
    /// dos sub-vistas mutuamente excluyentes — la lista de plugins, o (si
    /// `ExtensionsView::config` está abierto) el panel de `[config]` de UN
    /// plugin, con la description YA enmascarada
    /// (`norte_frontend::plugin_config::sanitize_config_keys`, aplicada al
    /// recibir `SessionEvent::PluginConfigReady`).
    fn render_extensions(&self, chrome: &ChromeColors) -> impl IntoElement {
        let view = self
            .extensions
            .as_ref()
            .expect("render_extensions: self.extensions es Some (invariante del caller)");

        let mut body = div()
            .id("extensions-rows")
            .role(gpui::Role::List)
            .aria_label(norte_i18n::t("ext-title"))
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .font(self.fonts.ui.clone());

        let header_text;
        if let Some(panel) = &view.config {
            header_text = panel.plugin_name.clone();
            let rows = panel.state.rows();
            if rows.is_empty() {
                body = body.child(div().px(px(sp::S)).child(SharedString::from("—")));
            } else {
                for (i, row) in rows.iter().enumerate() {
                    let selected = i == panel.state.cursor();
                    let mut value_text = row.value.clone();
                    if selected && panel.state.is_editing() {
                        let (buf, _) = norte_frontend::display_name(
                            panel.state.edit_buffer().unwrap_or("").as_bytes(),
                        );
                        value_text = format!("{buf}_");
                    }
                    let mut r = div()
                        .id(format!("plugin-config-row-{i}"))
                        .role(gpui::Role::ListItem)
                        .aria_label(row.key.clone())
                        .aria_selected(selected)
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(sp::S))
                        .px(px(sp::S))
                        .py(px(1.0)) // sub-XS: acento fino de una línea
                        .rounded(px(sp::RADIUS_ROW))
                        .child(
                            div()
                                .w(px(180.0))
                                .truncate()
                                .child(SharedString::from(row.key.clone())),
                        )
                        .child(
                            div()
                                .flex_1()
                                .truncate()
                                .child(SharedString::from(value_text)),
                        );
                    if selected {
                        r = r.bg(chrome.sel_bg);
                        if let Some(fg) = chrome.sel_fg {
                            r = r.text_color(fg);
                        }
                    }
                    body = body.child(r);
                    if !row.description.is_empty() {
                        body = body.child(
                            div()
                                .pl(px(sp::S + 180.0))
                                .truncate()
                                .text_color(chrome.quick_fg)
                                .child(SharedString::from(row.description.clone())),
                        );
                    }
                }
            }
        } else {
            header_text = norte_i18n::t("ext-title");
            if view.loading {
                body = body.child(div().px(px(sp::S)).child(SharedString::from("…")));
            } else if view.plugins.is_empty() && view.errors.is_empty() {
                body = body.child(
                    div()
                        .px(px(sp::S))
                        .child(SharedString::from(norte_i18n::t("ext-empty"))),
                );
            } else {
                for (i, p) in view.plugins.iter().enumerate() {
                    let selected = i == view.cursor;
                    let (name, _) = norte_frontend::display_name(p.name.as_bytes());
                    let mut status = if p.enabled { "✓" } else { "" }.to_owned();
                    if !p.approved {
                        status = format!("{status} ⚠ {}", norte_i18n::t("ext-unapproved"));
                    }
                    let mut r = div()
                        .id(format!("extensions-row-{i}"))
                        .role(gpui::Role::ListItem)
                        .aria_label(name.clone())
                        .aria_selected(selected)
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(sp::S))
                        .px(px(sp::S))
                        .py(px(1.0)) // sub-XS: acento fino de una línea
                        .rounded(px(sp::RADIUS_ROW))
                        .child(div().flex_1().truncate().child(SharedString::from(name)))
                        .child(SharedString::from(status));
                    if selected {
                        r = r.bg(chrome.sel_bg);
                        if let Some(fg) = chrome.sel_fg {
                            r = r.text_color(fg);
                        }
                    }
                    body = body.child(r);
                }
                for e in &view.errors {
                    let (dir, _) = norte_frontend::display_name(e.dir.as_bytes());
                    let (reason, _) = norte_frontend::display_name(e.reason.as_bytes());
                    body = body.child(
                        div()
                            .px(px(sp::S))
                            .text_color(chrome.err_fg)
                            .child(SharedString::from(format!("{dir}: {reason}"))),
                    );
                }
            }
        }

        div()
            .id("extensions-view")
            .role(gpui::Role::Document)
            .aria_label(norte_i18n::t("ext-title"))
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .border_2()
            .border_color(chrome.border_focus)
            .bg(chrome.pane_bg_focus)
            .child(
                div()
                    .px(px(sp::S))
                    .py(px(sp::XS))
                    .bg(chrome.header_bg)
                    .text_color(chrome.header_fg)
                    .truncate()
                    .child(SharedString::from(format!(
                        "{}  {header_text}",
                        norte_i18n::t("ext-title")
                    ))),
            )
            .child(body)
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
        extra_chrome_rows: usize,
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
        // (`VIEWER_CHROME_ROWS`) y menos `extra_chrome_rows` — el banner de
        // arranque y/o el flash (K3a MAJOR-1) que `render` pueda haber
        // pintado ARRIBA de este árbol esta misma vuelta, y que por tanto se
        // comen del `flex_1` real del visor sin que este `h` lo supiera. Sin
        // este término, el cuerpo pedía más filas de las que el contenedor
        // `overflow_hidden` iba a dejar sitio, y la última línea real
        // desaparecía en vez de que el visor pidiera una menos — silencioso,
        // y desde K3a rutinario (cualquiera de los 16/47 bindings `NotHere`
        // de Browse deja un flash mientras el visor está abierto). El
        // sobrante restante (redondeo, chrome del root) lo sigue recortando
        // `overflow_hidden`. Mínimo 1: una ventana minúscula no debe pedir un
        // rango vacío a `v.rows`.
        let viewport_rows = (window.viewport_size().height / self.fonts.row_h) as usize;
        let h = viewport_rows
            .saturating_sub(VIEWER_CHROME_ROWS)
            .saturating_sub(extra_chrome_rows)
            .max(1);

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
            } else if let Some(styled) = v.plugin_styled_rows(h) {
                // G3a (ADR 0037): preview de plugin CON ESTILO — flex-row por
                // línea, un div hijo por span. `styled_span_color` resuelve
                // `role`→tema (con glow) o `fg` crudo, `None` = sin
                // `.text_color()` (hereda `chrome.fg` del div raíz, mismo
                // criterio que un span plano).
                let glow = self.effects.and_then(|e| e.glow);
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .children(styled.into_iter().map(|line| {
                        div()
                            .h(self.fonts.row_h)
                            .px(px(sp::S))
                            .flex()
                            .overflow_hidden()
                            .children(line.iter().map(|span| {
                                let mut cell = div().child(SharedString::from(span.text.clone()));
                                if let Some(color) = styled_span_color(&self.theme, span, glow) {
                                    cell = cell.text_color(color);
                                }
                                cell
                            }))
                    }))
            } else {
                div().flex_1().flex().flex_col().overflow_hidden().children(
                    v.rows(h).into_iter().map(|row| {
                        div()
                            .h(self.fonts.row_h)
                            .px(px(sp::S))
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
                    .px(px(sp::S))
                    .py(px(sp::XS))
                    .bg(chrome.header_bg)
                    .text_color(chrome.header_fg)
                    .truncate()
                    .child(SharedString::from(header)),
            )
            .child(body)
            .child(
                div()
                    .px(px(sp::S))
                    .py(px(1.0)) // sub-XS: acento fino de una línea, fuera de la escala a propósito
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

    /// The which-key panel (K3a): while a chord sequence is PENDING, what can
    /// follow it — every continuation, the unavailable ones included and
    /// dimmed, with the reason they do nothing.
    ///
    /// `wk` is the CACHED [`Self::which_key`] — this method only PAINTS it,
    /// never builds it (see the field's doc for the allocation cost that
    /// rules out calling [`WhichKeyRows::build`](norte_frontend::whichkey::WhichKeyRows::build)
    /// from here).
    ///
    /// Inline flow at the same slot as the plain-text pending strip (`#91`),
    /// not an absolute overlay: it reflows the dual-pane/viewer above it
    /// exactly like that strip always did, and it is gated by the same
    /// suppression and the same LIVE `pending` check (see the call site in
    /// `render`).
    fn render_which_key(
        &self,
        wk: &norte_frontend::whichkey::WhichKeyRows,
        chrome: &ChromeColors,
    ) -> impl IntoElement {
        let mut panel = div()
            .id("which-key")
            .role(gpui::Role::List)
            .aria_label(wk.title.clone())
            .flex()
            .flex_col()
            .max_h(px(240.0))
            .overflow_hidden()
            .border_1()
            .border_color(chrome.border_focus)
            .bg(chrome.pane_bg_focus)
            .child(
                div()
                    .px(px(sp::S))
                    .py(px(1.0)) // sub-XS: acento fino de una línea
                    .bg(chrome.header_bg)
                    .text_color(chrome.header_fg)
                    .truncate()
                    .child(SharedString::from(wk.title.clone())),
            );
        for (i, row) in wk.rows.iter().enumerate() {
            let tail = if row.opens_sequence { " …" } else { "" };
            let label = if row.reason.is_empty() {
                format!("{}{tail}", row.label)
            } else {
                format!("{}{tail} — {}", row.label, row.reason)
            };
            // Dimmed, not hidden: the key IS bound, it just cannot run — the
            // panel says why instead of pretending the key does not exist.
            // Same "dim relativo" alpha as the context menu's disabled rows
            // (GPUI has no terminal DIM attribute) — the CHORD itself stays
            // full-strength either way, same as the TUI panel: the key does
            // something (it resolves), only the command it would run is what
            // the dimming is about.
            let label_fg = if row.avail == norte_frontend::keymap::Availability::Here {
                chrome.fg
            } else {
                gpui::Rgba {
                    a: 0.45,
                    ..chrome.fg
                }
            };
            // `ListItem` paired with the panel's `List` (N1): every other
            // list container in this file pairs the two.
            let aria = format!("{} {label}", row.chord);
            panel = panel.child(
                div()
                    .id(format!("which-key-row-{i}"))
                    .role(gpui::Role::ListItem)
                    .aria_label(aria)
                    .flex()
                    .flex_row()
                    .px(px(sp::S))
                    .py(px(1.0)) // sub-XS: acento fino de una línea
                    .gap(px(sp::S))
                    .child(
                        div()
                            .text_color(chrome.header_fg)
                            .child(SharedString::from(row.chord.clone())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .text_color(label_fg)
                            .child(SharedString::from(label)),
                    ),
            );
        }
        panel
    }

    /// Pinta el panel del modal activo (overlay centrado, ver `render`): título
    /// y cuerpo saneados por [`modal_lines`] (mapeados 1:1 a divs, sin volver a
    /// tocar bytes de usuario aquí), más el pie de teclas fijo por variante.
    /// Pinta el menú contextual: scrim transparente que OCLUYE el ratón (un
    /// click fuera cierra y no se cuela a la fila de debajo) + panel anclado
    /// al puntero, recolocado para que quepa entero ([`menu_origin`]).
    ///
    /// Una entrada deshabilitada se pinta atenuada CON su motivo y, aun así,
    /// registra un listener: el suyo sólo corta la propagación, para que
    /// pulsarla no cierre el menú por el scrim de detrás — un menú que se
    /// cierra al pulsar algo que no hizo nada se lee como que sí lo hizo.
    fn render_context_menu(
        &self,
        menu: &ContextMenu,
        chrome: &ChromeColors,
        viewport: gpui::Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Alto ESTIMADO (cabecera + entradas + pie, más el padding vertical):
        // sólo sirve para recolocar el panel dentro de la ventana, así que
        // una estimación por lo alto es la buena — desplazar de más deja el
        // menú visible; de menos, lo saca por abajo.
        let filas = menu.items.len() + 2;
        #[allow(clippy::cast_precision_loss)] // ≤ 16 filas: exacto en f32.
        let alto = filas as f32 * f32::from(self.fonts.row_h) + 4.0 * sp::M;
        let (x, y) = menu_origin(
            menu.anchor,
            (CONTEXT_MENU_W, alto),
            (f32::from(viewport.width), f32::from(viewport.height)),
        );
        let (panel_bg, panel_fg) = modal_panel_colors(chrome);
        let (title_bg, title_fg) = modal_title_colors(chrome);
        let hover_bg = chrome.hover_bg;

        let mut panel = div()
            .id("context-menu")
            .role(gpui::Role::Menu)
            .aria_label(menu.target.text())
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(CONTEXT_MENU_W))
            .flex()
            .flex_col()
            .overflow_hidden()
            .border_1()
            .border_color(chrome.border_focus)
            .rounded(px(sp::RADIUS_PANEL))
            .bg(panel_bg)
            .text_color(panel_fg)
            .py(px(sp::XS))
            // Cabecera: SOBRE QUÉ actúa el menú (el recuento de marcas o el
            // nombre de la fila). Es la línea que impide que una copia se
            // lleve once ficheros cuando el usuario señalaba uno.
            .child(
                div()
                    .px(px(sp::M))
                    .py(px(1.0)) // sub-XS: acento fino de una línea
                    .bg(title_bg)
                    .text_color(title_fg)
                    .truncate()
                    .child(SharedString::from(menu.target.text())),
            );

        for (i, item) in menu.items.iter().enumerate() {
            let disponible = item.avail.is_available();
            let mut row = div()
                .id(format!("context-menu-item-{i}"))
                .role(gpui::Role::MenuItem)
                // GPUI (rev f14fea9) no expone `aria_disabled`: el estado ya
                // va DENTRO del nombre accesible, porque `item.text()` de una
                // entrada apagada incluye su motivo — un lector de pantalla
                // anuncia «Borrar — backend de solo lectura», que dice más
                // que un flag.
                .aria_label(item.text())
                .aria_selected(i == menu.cursor)
                .px(px(sp::M))
                .py(px(1.0)) // sub-XS: acento fino de una línea
                .truncate()
                .child(SharedString::from(item.text()));
            if disponible {
                row = row
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover_bg))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev: &MouseDownEvent, _w, cx| {
                            this.activate_context_menu(i, cx);
                        }),
                    );
            } else {
                // Atenuado: el mismo «dim relativo» que las celdas y el badge
                // de decoración (GPUI no tiene un atributo DIM del terminal).
                row = row
                    .text_color(gpui::Rgba {
                        a: 0.45,
                        ..panel_fg
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_this, _ev: &MouseDownEvent, _w, cx| {
                            cx.stop_propagation();
                        }),
                    );
            }
            if i == menu.cursor {
                row = row.bg(chrome.sel_bg);
            }
            panel = panel.child(row);
        }

        let (footer_bg, footer_fg) = modal_footer_colors(chrome);
        let mut footer = div()
            .mt(px(sp::XS))
            .px(px(sp::M))
            .py(px(1.0)) // sub-XS: acento fino de una línea
            .bg(footer_bg)
            .truncate()
            .child(SharedString::from(norte_i18n::t("gui-menu-hint")));
        if let Some(fg) = footer_fg {
            footer = footer.text_color(fg);
        }
        panel = panel.child(footer);

        div()
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev: &MouseDownEvent, _w, cx| {
                    this.context_menu = None;
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _ev: &MouseDownEvent, _w, cx| {
                    this.context_menu = None;
                    cx.notify();
                }),
            )
            .child(panel)
    }

    fn render_modal(&self, m: &Modal, chrome: &ChromeColors) -> impl IntoElement {
        let lines = modal_lines(m);
        // Nombre accesible = título (1.ª línea, siempre presente); el resto
        // (cuerpo saneado por `modal_lines`, ítems/mode/conflict) va como
        // `aria_description` — un lector anuncia diálogo → título → cuerpo.
        // `.get(1..)` (no indexado directo) por si `lines` alguna vez trajera
        // solo el título (defensivo, sin panic).
        let a11y_label = lines.first().cloned().unwrap_or_default();
        let a11y_description = modal_a11y_description(&lines);
        let footer = norte_i18n::t(match m {
            Modal::ConfirmTransfer { .. } => "gui-modal-footer-transfer",
            Modal::ConfirmDelete { .. } => "gui-modal-footer-delete",
            Modal::ConflictResolve { .. } => "gui-modal-footer-conflict",
            Modal::ConfirmQuit { .. } => "gui-modal-footer-quit",
            // Claves COMPARTIDAS con la TUI (mismas teclas y semántica en
            // GPUI: Enter/Esc, y/n, ↓/↑).
            Modal::RenamePrompt { .. } => "gui-modal-rename-footer",
            Modal::AiRenamePrompt { .. } => "modal-ai-rename-hint",
            // §17 (paridad con el pie de la TUI): con un lote que no se
            // puede aplicar, `y`/`enter` está mudo (`modal::on_key` contesta
            // `Ignored`) — ofrecerlo sería una affordance falsa.
            Modal::AiRenamePlan { plan, .. } if plan.confirmable() => "modal-ai-rename-plan-hint",
            Modal::AiRenamePlan { .. } => "modal-rename-batch-plan-hint-blocked",
            // "Esc cancela" aquí es honesto: cancela el MODAL (la petición
            // solo se manda al pulsar Enter) — a diferencia del banner
            // `gui-msg-semantic-running`, que no puede prometer aborto.
            Modal::SemanticQuery { .. } => "modal-semantic-hint",
            Modal::SemanticHits { .. } => "modal-semantic-hits-hint",
            // 2026-08-10-volumes.md task V4: the TUI's footer here is
            // GENERATED from the live keymap (`hints.rs::nav_volumes`), not a
            // fixed string, so there is no shared key to reuse — this one is
            // GUI-only, molde `gui-modal-footer-*` above.
            Modal::Volumes { .. } => "gui-modal-footer-volumes",
        });
        // Líneas que van en rojo: el modo de un borrado PERMANENTE (índice 1
        // en ConfirmDelete) y el diagnóstico del renombrado, que `modal_lines`
        // pone SIEMPRE el último. Un error del mismo color que el resto se
        // lee como una línea más del formulario.
        let alert_line = match m {
            Modal::ConfirmDelete {
                permanent: true, ..
            } => Some(1),
            Modal::RenamePrompt { error: Some(_), .. } => Some(lines.len().saturating_sub(1)),
            _ => None,
        };

        let mut panel = div()
            .id("modal")
            .role(gpui::Role::Dialog)
            .aria_label(a11y_label)
            .aria_description(a11y_description)
            .flex()
            .flex_col()
            .min_w(px(360.0))
            .max_w(px(560.0))
            .max_h(px(MODAL_MAX_H))
            .overflow_hidden()
            .border_2()
            .border_color(chrome.border_focus)
            // Esquinas redondeadas (GP): distingue el panel flotante del
            // resto del chrome, que va todo en ángulo recto.
            .rounded(px(sp::RADIUS_PANEL))
            .px(px(sp::L))
            .py(px(sp::M))
            .gap(px(sp::XS));

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
                    .px(px(sp::XS))
                    .py(px(1.0)) // sub-XS: acento fino de una línea, fuera de la escala a propósito
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
            .mt(px(sp::S))
            .px(px(sp::XS))
            .py(px(1.0)) // sub-XS: acento fino de una línea, fuera de la escala a propósito
            .bg(footer_bg)
            .child(SharedString::from(footer));
        if let Some(fg) = footer_fg {
            footer_row = footer_row.text_color(fg);
        }
        panel.child(footer_row)
    }
}

/// El cuerpo del modal como UNA cadena para `aria_description`.
///
/// Separador `\n` y NO `"; "`: `modal_lines` sanea cada línea con
/// `display_name`, que enmascara todo `Cc` (incluido `\n`) y `Zl`/`Zp`, así
/// que un nombre hostil no puede FABRICAR una línea aquí. Un `"; "` sí es
/// texto legal en un nombre — con él, `x; lote: aplicable.txt` le dictaba a
/// un lector de pantalla un veredicto que nadie emitió. Es la doctrina
/// `arrow_join_spoof`, aplicada a la superficie que usa justo quien no puede
/// ver el layout que la protege.
///
/// La primera línea es el título y va aparte, en `aria_label`. `.get(1..)`
/// (no indexado) por si `lines` alguna vez trajera solo el título.
///
/// PURA (sin GPUI): testeable sin levantar ventana.
#[must_use]
fn modal_a11y_description(lines: &[String]) -> String {
    lines
        .get(1..)
        .map(|rest| rest.join("\n"))
        .unwrap_or_default()
}

/// Alto máximo del panel de un modal, en px.
///
/// El panel es `overflow_hidden` y NO tiene scroll: lo que no cabe se pierde
/// SIN marca, así que este número es un presupuesto que hay que sostener, no
/// una preferencia estética. El modal más alto es el del plan IA con el
/// veredicto de su lote (§17): título + dir + estado + 5 parejas × 2 +
/// indicador + 5 colisiones + resumen + pie = 20 filas.
/// `modal_lines_caben_en_el_panel` lo pinea contra
/// [`MODAL_ROW_H`] para que una línea nueva no lo desborde en silencio.
const MODAL_MAX_H: f32 = 560.0;

/// Alto asumido de una fila del modal, en px: `font_size * 1.5` con la fuente
/// por defecto (14 px). No lo IMPONE nadie —GPUI mide el texto—, es la base
/// del presupuesto de [`MODAL_MAX_H`], y por eso solo lo usa el test que
/// sostiene ese presupuesto. Con una fuente configurada mucho mayor el panel
/// vuelve a poder recortar; eso es deuda conocida, no un descuido.
#[cfg(test)]
const MODAL_ROW_H: f32 = 21.0;

/// Ancho del panel del menú contextual. Fijo a propósito: medir el texto más
/// largo por frame costaría lo que cuesta medir texto en GPUI, y un menú que
/// cambia de ancho al cambiar de fila se lee como un fallo. Las etiquetas
/// truncan dentro (`.truncate()`), nunca desbordan.
const CONTEXT_MENU_W: f32 = 320.0;

/// Dónde pintar el panel del menú para que quepa ENTERO: en el puntero, o
/// desplazado lo justo si se saldría por la derecha o por abajo. Con una
/// ventana más pequeña que el panel cae a `(0, 0)` — recortado por arriba
/// antes que fuera de la vista.
///
/// PURA (sin GPUI): la aritmética de colocación es lo único de este overlay
/// que puede equivocarse en silencio.
#[must_use]
fn menu_origin(anchor: (f32, f32), panel: (f32, f32), viewport: (f32, f32)) -> (f32, f32) {
    let x = anchor.0.min(viewport.0 - panel.0).max(0.0);
    let y = anchor.1.min(viewport.1 - panel.1).max(0.0);
    (x, y)
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
        // §17: el lote entero vive en UN directorio, origen y destino — ese
        // es el único que hay que relistar.
        PendingOp::RenameBatch { dir, .. } => vec![dir.clone()],
    }
}

/// Applies a listing that just landed to `pane`, choosing between the TWO
/// entry points of `PaneState` (#103). PURA (sin GPUI): testeable sin levantar
/// ventana.
///
/// - `refill` — the REFRESH path: the same dir re-listed after a mutation. It
///   KEEPS the marks and prunes the ones whose entry is gone.
/// - `set_listing` — the `cd` path: a different dir. It CLEARS the marks by
///   design, because a selection does not survive navigating away.
///
/// `refresh` says the list was asked for by `relist_dirs` (read-after-write),
/// not by a `cd`. It is NOT enough on its own: the dir that came back must
/// also be the pane's current dir, compared BYTE-exactly (hard rule 1) — two
/// directory names can render identically while differing in bytes, and
/// keeping marks across that boundary would apply them to another dir's
/// entries.
///
/// Returns `true` when it kept the marks (refill), `false` when it took the
/// `cd` path.
fn apply_landed_listing(
    pane: &mut PaneState,
    dir: VPath,
    entries: Vec<Entry>,
    refresh: bool,
) -> bool {
    if refresh && *pane.dir() == dir {
        pane.refill(entries);
        // `refill` no toca el flag de carga (un fill paginado lo gestiona
        // aparte, ADR 0017); el refresco de la GUI sí lo había subido para que
        // el coalescing de #84 lo vea, así que lo baja aquí.
        pane.set_loading(false);
        return true;
    }
    pane.set_listing(dir, entries);
    false
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
///
/// RENDER-side duty (encoding audit H1): un chord de una capa hostil (`./
/// .norte/keymap.toml`, capa de PROYECTO sin trust — `parse_chord` acepta
/// CUALQUIER codepoint suelto como `KeyCode::Char`) llega aquí vía el
/// resolver pendiente y se pinta en el pie de la ventana (#91); `Chord`'s
/// `Display` lo escribe crudo A PROPÓSITO (logs/debug quieren el chord
/// real), así que se enmascara aquí — mismo mecanismo que la TUI
/// (`hints::dialog_hints`/`palette::first_chord`).
#[must_use]
fn pending_hint(chords: &[norte_frontend::keymap::Chord]) -> String {
    chords
        .iter()
        .map(|c| norte_encoding::mask_terminal_hazards(&format!("{c} ")))
        .collect()
}

/// El indicador del pie completo (#91 + K2a): el contador tecleado hasta ahora
/// seguido de los chords pendientes y el «…». `None` cuando no hay ni lo uno
/// ni lo otro — el pie calla.
///
/// Los dos van JUNTOS porque en `12gg` conviven: el `12` sigue vivo mientras
/// la secuencia `g g` se teclea, y pintar solo uno miente sobre lo que hará la
/// próxima tecla. El contador es un `u32` que rendereamos nosotros (solo
/// dígitos ASCII), así que no necesita el enmascarado que sí necesita cada
/// chord — ver [`pending_hint`]. PURA (sin GPUI): testeable sin ventana.
#[must_use]
fn pending_indicator(
    count: Option<u32>,
    chords: &[norte_frontend::keymap::Chord],
) -> Option<String> {
    if count.is_none() && chords.is_empty() {
        return None;
    }
    let head = match count {
        Some(n) => format!("{n} "),
        None => String::new(),
    };
    Some(format!("{head}{}…", pending_hint(chords)))
}

/// The which-key rows for `resolver`'s CURRENT pending state (K3a), or
/// `None` while nothing is pending — same "a bare count opens nothing" rule
/// as [`norte_frontend::whichkey::WhichKeyRows::build`] (its own doc has the
/// reasoning): `resolver.pending()` is empty for a bare count, so this
/// returns `None` right along with it.
///
/// PURA (sin GPUI): the one thing [`NorteGui::refresh_which_key`] and
/// [`NorteGui::refresh_which_key_viewer`] do beyond a plain field read, and
/// pulled out so it is testable against a VIEWER-context resolver without a
/// window — the mistake this exists to catch is reading `self.resolver` in
/// the viewer's arm, which would silently paint the pane's rows while the
/// viewer owns the keyboard.
#[must_use]
fn which_key_for(
    resolver: &norte_frontend::keymap::Resolver,
    lang: norte_i18n::Lang,
) -> Option<norte_frontend::whichkey::WhichKeyRows> {
    if resolver.pending().is_empty() {
        None
    } else {
        Some(norte_frontend::whichkey::WhichKeyRows::build(
            resolver.effective(),
            resolver.pending(),
            resolver.count(),
            lang,
        ))
    }
}

/// Whether the transient flash line (`#108` 7c, K3a) paints THIS frame,
/// given which full-screen views are open. PURA (sin GPUI): testeable sin
/// ventana.
///
/// The viewer is deliberately NOT a parameter: it used to be
/// (`main.rs` pre-K3a), which is exactly the debt K3a paid — the viewer has
/// no status line of its own to protect (its `status`, see `render_viewer`,
/// is read-only content, not a place for a transient notice), so a flash
/// landing while it is open pushes it down like any other banner instead of
/// vanishing unseen.
#[must_use]
fn flash_paints(settings_open: bool, extensions_open: bool) -> bool {
    !settings_open && !extensions_open
}

/// Whether the which-key panel (K3a) paints THIS frame. PURA (sin GPUI):
/// testeable sin ventana.
///
/// `pending_is_empty`/`wk_present` come from the LIVE resolver read already
/// done at the call site (see its comment): they catch a resolver SWITCH
/// (settings/extensions/palette/picker/menu opening, or the viewer opening
/// from a mouse double-click) because the new resolver's `pending()` is
/// empty until the next keystroke.
///
/// `modal_open`/`help_open` exist because that live-`pending` guard alone is
/// NOT enough: a modal can open from a background task landing (a conflict,
/// an AI-rename reply, a semantic-search reply — none of them touch a
/// resolver), and F1 opens help from inside the viewer's key handler before
/// `viewer_resolver.push` ever runs. Neither clears `self.which_key`, and
/// neither should — the pending prefix underneath is still live, and the
/// panel is meant to reappear once the overlay closes (same design as the
/// TUI's `app.modal.is_none()` draw guard). This function is the difference
/// between "reappears when the overlay closes" and "bleeds through the
/// overlay's translucent scrim while it's still up."
#[must_use]
fn which_key_paints(
    pending_is_empty: bool,
    wk_present: bool,
    modal_open: bool,
    help_open: bool,
) -> bool {
    !pending_is_empty && wk_present && !modal_open && !help_open
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

/// S2 (`[ui] confirm_quit`): si `quit_or_confirm` debe abrir el modal en vez
/// de cerrar, dado el modo configurado y si HAY trabajo pendiente (ya
/// calculado por el caller — [`has_pending_work`]/`inflight`). `Never` NO
/// pasa por aquí (el caller corta antes, ver `quit_or_confirm`); mantenerlo
/// fuera de este `match` sería redundante con ese corte temprano, así que
/// esta función solo cubre `Always`/`Auto` — llamarla con `Never` es
/// correcto igualmente (`false` incondicional) pero nunca ocurre en el
/// camino real. Puro: testeable sin GPUI. Envoltorio fino (revisión S, M6):
/// byte-idéntica a la de la TUI (`quit_needs_confirm`) — hoisteada a
/// [`norte_frontend::settings::quit_needs_confirm`].
#[must_use]
fn confirm_quit_should_open(mode: ConfirmQuit, pending: bool) -> bool {
    norte_frontend::settings::quit_needs_confirm(mode, pending)
}

/// ¿Sigue vigente el resultado de un `fs.list`? Solo si su generación coincide
/// con la vigente del pane: un cd más nuevo ya incrementó el contador, dejando
/// stale a cualquier list en vuelo anterior. Comparar la generación (y no el
/// `dir`) es robusto ante A→B→A — dos cds distintos al MISMO dir tienen
/// generaciones distintas, un dir-compare los confundiría.
#[must_use]
/// Tanda de hidratación a pedir (#123): las candidatas que NO se pidieron ya
/// para este listado, hasta `max`. Puro para poder fijarlo en un test — la
/// dedup es lo que hace barato llamar a `request_hydration` en cada frame, y
/// lo que impide que un stat FALLIDO (la fila se queda sin `size`, así que
/// vuelve a ser candidata) se reintente en bucle.
fn hydration_batch(
    candidates: Vec<VPath>,
    probed: &std::collections::HashSet<VPath>,
    max: usize,
) -> Vec<VPath> {
    candidates
        .into_iter()
        .filter(|p| !probed.contains(p))
        .take(max)
        .collect()
}

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

/// Texto de una celda no-nombre de la fila (#108 b6/#117): el `styled_cell`
/// compartido con la TUI, con la ausencia (`None` — el size de un dir, un
/// attr que el provider no mandó) aplanada a blanco, jamás un valor
/// fabricado. Función PURA (sin GPUI), como `row_label`, para poder testear
/// el camino de celdas de la GUI con valores attr hostiles sin ventana/GPU.
#[must_use]
fn row_cell_text(
    entry: &Entry,
    col: &norte_frontend::columns::ColumnId,
    now_ms: i64,
    style: &norte_frontend::columns::ColumnStyle,
) -> String {
    norte_frontend::columns::styled_cell(entry, col, now_ms, style).unwrap_or_default()
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

/// Los dos textos con los que el pie del pane habla de la SELECCIÓN (#103):
/// `(marcadas, podadas)`, cada uno `None` cuando no hay nada que decir.
/// Espejo exacto de `marks_status_segments` en `crates/norte-tui/src/ui.rs`
/// (commit `b84ee16`) — mismas claves Fluent, mismas reglas de composición,
/// para que los dos frontends digan LO MISMO del mismo estado. Lo que no
/// comparten es la disposición: la TUI concatena en su barra global, la GUI
/// pinta una línea por segmento en el pane que las tiene (ver `render_pane`).
///
/// - Con 0 marcas y 0 podadas devuelve `(None, None)`: quien no marca nada no
///   gana chrome nuevo.
/// - `dirs > 0` cambia la clave a `status-marked-with-dirs`, porque
///   `PaneState::marked_bytes` cuenta SOLO no-directorios a propósito (nada
///   recorre el árbol): pintar «2 marked, 10 B» con un directorio dentro
///   insinuaría un total que nadie calculó.
/// - `pruned > 0` avisa SIEMPRE, con marcas vivas o sin ellas. Un refresh que
///   se comió marcas jamás es silencioso: con la selección vacía
///   `marked_paths` cae al cursor, así que callarlo redirigiría la siguiente
///   op en masa a algo que nadie marcó.
///
/// PURA (sin GPUI): toma los contadores ya calculados por `PaneState` y
/// formatea con `norte_frontend::human_bytes` — la GUI compone y pinta, no
/// cuenta (regla 7). Testeable sin levantar ventana.
#[must_use]
fn marks_status_segments(
    marks: usize,
    bytes: u64,
    dirs: usize,
    pruned: usize,
) -> (Option<String>, Option<String>) {
    let marked = (marks > 0).then(|| {
        let n = marks.to_string();
        let size = norte_frontend::human_bytes(bytes);
        if dirs == 0 {
            norte_i18n::ta("status-marked", &[("n", &n), ("size", &size)])
        } else {
            norte_i18n::ta(
                "status-marked-with-dirs",
                &[("n", &n), ("size", &size), ("dirs", &dirs.to_string())],
            )
        }
    });
    let pruned =
        (pruned > 0).then(|| norte_i18n::ta("status-marks-pruned", &[("n", &pruned.to_string())]));
    (marked, pruned)
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
        TaskKind::Mkdir => "gui-task-kind-mkdir",
        TaskKind::Index => "gui-task-kind-index",
        TaskKind::Embed => "gui-task-kind-embed",
        TaskKind::RenameBatch => "gui-task-kind-rename-batch",
        // `Unknown` es la clase de un daemon N+1 que este proto YA conocía
        // como desconocida (vía `serde(other)`); el `_` es
        // `#[non_exhaustive]` (#126) — una variante de un norte-proto más
        // nuevo que este binario no reconoce en absoluto. Mismo caso de cara
        // al usuario, misma etiqueta genérica.
        TaskKind::Unknown | _ => "gui-task-kind-unknown",
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
            lines.extend(norte_frontend::item_lines(items));
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
            lines.extend(norte_frontend::item_lines(items));
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
            // sanear aquí, a diferencia del resto de modales. S2: con
            // `confirm_quit = "always"` este modal también se abre SIN nada
            // pendiente — "Quit with 0 task(s) running and 0 mark(s)?" es
            // gramatical pero raro; un título genérico evita la falsa
            // sensación de que "0 tasks" es una advertencia real.
            if *tasks == 0 && *marks == 0 {
                vec![norte_i18n::t("gui-modal-quit-title-empty")]
            } else {
                vec![norte_i18n::ta(
                    "gui-modal-quit-title",
                    &[
                        ("tasks", tasks.to_string().as_str()),
                        ("marks", marks.to_string().as_str()),
                    ],
                )]
            }
        }
        // Renombrado in situ (`pane.rename`): tres líneas etiquetadas FUERA
        // de banda, una por nombre — el actual y el que se está tecleando —
        // con badge por línea y el cursor `_` al final del editable. Jamás
        // en el mismo renglón con un `→` en medio: la doctrina del corpus
        // hostil (`arrow_join_spoof`) es que un nombre no puede llevarse por
        // delante la etiqueta de otro. Los dos van ENMASCARADOS: el actual
        // sale del disco y el editable puede llegar por paste.
        Modal::RenamePrompt {
            from, name, error, ..
        } => {
            let actual = from.file_name().map_or(&b""[..], Segment::as_bytes);
            let (actual_txt, actual_hostil) = norte_frontend::display_name(actual);
            let (nuevo_txt, nuevo_hostil) = norte_frontend::display_name(name);
            let mut lines = vec![
                norte_i18n::t("gui-modal-rename-title"),
                hostile_badged(
                    actual_hostil,
                    norte_i18n::ta("gui-modal-rename-from", &[("name", actual_txt.as_str())]),
                ),
                hostile_badged(
                    nuevo_hostil,
                    norte_i18n::ta(
                        "gui-modal-rename-to",
                        &[("name", format!("{nuevo_txt}_").as_str())],
                    ),
                ),
            ];
            // El diagnóstico del último intento: `banner_safe` por si un
            // error futuro llegara a citar texto de terceros (los de hoy
            // —`VPathError`, Fluent— son taxonomía cerrada).
            if let Some(e) = error {
                lines.push(banner_safe(e));
            }
            lines
        }
        Modal::AiRenamePrompt { query, .. } => {
            // Molde TUI `ai_rename_modal_text`: la instrucción es texto de
            // usuario (un paste trae bidi/invisibles tan fácil como un
            // nombre) — enmascarada SIEMPRE, con badge y cursor `_`.
            let (masked, hostil) = norte_frontend::display_name(query);
            vec![
                norte_i18n::t("modal-ai-rename"),
                hostile_badged(hostil, format!("{masked}_")),
            ]
        }
        // Paridad TUI `ai_rename_plan_modal_text` (doctrina
        // encoding-auditor): primera línea del cuerpo = dir OBJETIVO
        // etiquetado fuera de banda (audit MAJOR-1); después la VENTANA de
        // `AI_RENAME_PAIR_LIMIT` parejas desde `offset` (audit MAJOR-3: el
        // plan entero es revisable por scroll ↓/↑). Cada nombre en SU línea
        // — el `from` con etiqueta numerada ABSOLUTA fuera de banda (audit
        // MINOR-4, corpus `arrow_join_spoof`) y el `→` del destino al INICIO
        // de su línea; badge Rust-side ([`hostile_badged`]) y truncado por
        // los divs (`.truncate()` de `render_modal` — el badge va prefijado,
        // jamás se lo come el corte). El indicador de desbordamiento lleva
        // badge si alguna pareja OCULTA es hostil (lo escondido no se cuela
        // limpio). Aunque el engine garantiza UTF-8 en el wire, un daemon
        // N+1/comprometido podría mandar cualquier cosa — defensivo SIEMPRE.
        Modal::AiRenamePlan {
            dir,
            entries,
            offset,
            plan,
        } => {
            // Cinturón de render: el clamp vive en `modal::on_key`, pero un
            // offset fuera de rango jamás debe pintar una ventana vacía.
            let offset = (*offset).min(entries.len().saturating_sub(modal::AI_RENAME_PAIR_LIMIT));
            let last = (offset + modal::AI_RENAME_PAIR_LIMIT).min(entries.len());
            let (dir_txt, dir_hostil) = norte_frontend::path_display(dir);
            let mut lines = vec![
                norte_i18n::t("modal-ai-rename-plan"),
                hostile_badged(
                    dir_hostil,
                    norte_i18n::ta("modal-ai-rename-dir", &[("dir", dir_txt.as_str())]),
                ),
                // El VEREDICTO del lote va arriba, pegado al dir y ANTES de
                // las parejas (§17, paridad TUI): de todas las líneas del
                // cuerpo es la que no puede perderse — dice si esto va a
                // renombrar algo.
                norte_i18n::t(plan.status_key()),
            ];
            for (i, e) in entries.iter().enumerate().take(last).skip(offset) {
                let (from, from_hostil) = norte_frontend::display_name(e.from.as_bytes());
                let (to, to_hostil) = norte_frontend::display_name(e.to.as_bytes());
                let n = (i + 1).to_string();
                lines.push(hostile_badged(
                    from_hostil,
                    norte_i18n::ta(
                        "modal-ai-rename-pair-from",
                        &[("n", n.as_str()), ("from", from.as_str())],
                    ),
                ));
                lines.push(hostile_badged(
                    to_hostil,
                    norte_i18n::ta("modal-ai-rename-pair-to", &[("to", to.as_str())]),
                ));
            }
            if entries.len() > modal::AI_RENAME_PAIR_LIMIT {
                let hidden_hostil = entries.iter().enumerate().any(|(i, e)| {
                    (i < offset || i >= last)
                        && (norte_frontend::display_name(e.from.as_bytes()).1
                            || norte_frontend::display_name(e.to.as_bytes()).1)
                });
                let shown = last.to_string();
                let total = entries.len().to_string();
                lines.push(hostile_badged(
                    hidden_hostil,
                    norte_i18n::ta(
                        "modal-ai-rename-more",
                        &[("shown", shown.as_str()), ("total", total.as_str())],
                    ),
                ));
            }
            // El saneado del detalle (enmascarado, elipsis, índice de pareja,
            // tope de colisiones) vive en `norte-frontend`, compartido byte a
            // byte con la TUI: `norte-gui` está FUERA del workspace y `just
            // ci` no la compila, así que una política duplicada aquí se
            // desviaría sin que nada avisara. Esta GUI solo pone SU badge.
            lines.extend(
                plan.detail_lines(entries.len())
                    .into_iter()
                    .map(|(linea, hostil)| hostile_badged(hostil, linea)),
            );
            lines
        }
        Modal::SemanticQuery { query } => {
            // Molde `AiRenamePrompt`: la consulta es texto de usuario (un
            // paste trae bidi/invisibles tan fácil como un nombre) —
            // enmascarada SIEMPRE, con badge y cursor `_`.
            let (masked, hostil) = norte_frontend::display_name(query);
            vec![
                norte_i18n::t("modal-semantic"),
                hostile_badged(hostil, format!("{masked}_")),
            ]
        }
        // Paridad TUI `semantic_hits_modal_text` (doctrina encoding-auditor,
        // molde del plan IA de arriba): un hit POR LÍNEA con marcador de
        // cursor (`>`) FUERA de banda en columna fija ANTES del badge (un
        // path no puede imitarlo: va enmascarado y tras la etiqueta numerada
        // ABSOLUTA), path por `path_display` (mask + flag hostil) con badge
        // Rust-side y score `{:.2}` al final. El path se acota con
        // `middle_ellipsis` ANTES de interpolarlo (audit H1): el score va el
        // ÚLTIMO y NO puede depender del `.truncate()` del div, que recorta
        // por la derecha en silencio y lo expulsaría de la caja. El
        // indicador de desbordamiento lleva badge si algún hit OCULTO es
        // hostil. Los hits ya pasaron `validate_semantic_hits` al ingerirse,
        // pero un daemon N+1/comprometido podría mandar cualquier cosa — se
        // pinta a la defensiva SIEMPRE.
        Modal::SemanticHits {
            hits,
            offset,
            cursor,
        } => {
            // Cinturón de render: el clamp vive en `modal::on_key`, pero un
            // offset fuera de rango jamás debe pintar una ventana vacía.
            let offset = (*offset).min(hits.len().saturating_sub(modal::SEMANTIC_HIT_LIMIT));
            let last = (offset + modal::SEMANTIC_HIT_LIMIT).min(hits.len());
            let mut lines = vec![norte_i18n::t("modal-semantic-hits")];
            for (i, h) in hits.iter().enumerate().take(last).skip(offset) {
                let (path, hostil) = norte_frontend::path_display(&h.path);
                // Encoding audit M4-IA-2 H1: el path se ACOTA aquí, ANTES de
                // interpolarlo — jamás se delega el recorte al `.truncate()`
                // del div. El score va el ÚLTIMO en `modal-semantic-hit`, así
                // que un path kilométrico lo empujaba fuera de la caja y el
                // corte por la derecha se lo comía; si además el path llevaba
                // incrustado un `· 0.99` (middle dot + dígitos: chars
                // imprimibles, NO enmascarables → NI SIQUIERA hay badge que
                // avise), el único texto con pinta de score que quedaba
                // visible era el del atacante. Presupuesto = el MISMO 44 de
                // la TUI (`semantic_hits_modal_text`): en GPUI no hay modelo
                // de columnas, pero 44 celdas + la etiqueta `N.` + ` · 0.42`
                // entran de sobra en el modal (max_w 560px) y compartir la
                // cifra mantiene el invariante idéntico en ambos frontends.
                // Es reserva de sitio para el campo de cola, no cosmética.
                let path = norte_frontend::middle_ellipsis(&path, 44);
                let n = (i + 1).to_string();
                let score = format!("{:.2}", h.score);
                let line = hostile_badged(
                    hostil,
                    norte_i18n::ta(
                        "modal-semantic-hit",
                        &[
                            ("n", n.as_str()),
                            ("path", path.as_str()),
                            ("score", score.as_str()),
                        ],
                    ),
                );
                lines.push(if i == *cursor {
                    format!("> {line}")
                } else {
                    format!("  {line}")
                });
            }
            if hits.len() > modal::SEMANTIC_HIT_LIMIT {
                let hidden_hostil = hits.iter().enumerate().any(|(i, h)| {
                    (i < offset || i >= last) && norte_frontend::path_display(&h.path).1
                });
                let shown = last.to_string();
                let total = hits.len().to_string();
                lines.push(hostile_badged(
                    hidden_hostil,
                    norte_i18n::ta(
                        "modal-semantic-more",
                        &[("shown", shown.as_str()), ("total", total.as_str())],
                    ),
                ));
            }
            lines
        }
        // 2026-08-10-volumes.md task V4 (design §D), molde `SemanticHits`
        // above. Every text field the platform hands us — label, mount AND
        // `fs_type` — is masked (encoding-auditor V3 review, paridad TUI
        // `volume_item_display`): `fs_type` is not the closed ASCII
        // vocabulary it looks like (a FUSE mount's `fuse.<subtype>` is an
        // unprivileged user's string), and `label` is `Option<Vec<u8>>`
        // (V3.5) reaching here as raw wire bytes, no `String` upstream to
        // have already thrown one away.
        Modal::Volumes {
            include_pseudo,
            volumes,
            offset,
            cursor,
            ..
        } => {
            let mode = norte_i18n::t(if *include_pseudo {
                "volumes-mode-all"
            } else {
                "volumes-mode-filtered"
            });
            let mut lines = vec![format!("{} — {mode}", norte_i18n::t("volumes-title"))];
            if volumes.is_empty() {
                lines.push(norte_i18n::t("volumes-empty"));
                return lines;
            }
            // Cinturón de render: el clamp vive en `modal::on_key`, pero un
            // offset fuera de rango jamás debe pintar una ventana vacía.
            let offset = (*offset).min(volumes.len().saturating_sub(modal::VOLUMES_LIMIT));
            let last = (offset + modal::VOLUMES_LIMIT).min(volumes.len());
            for (i, v) in volumes.iter().enumerate().take(last).skip(offset) {
                let (path_txt, path_hostil) = norte_frontend::path_display(&v.mount);
                let (label_prefix, label_hostil) = match v.label.as_deref() {
                    Some(l) => {
                        let (nt, nh) = norte_frontend::display_name(l);
                        (format!("{nt} — "), nh)
                    }
                    None => (String::new(), false),
                };
                let (fs_type_txt, fs_type_hostil) =
                    norte_frontend::display_name(v.fs_type.as_bytes());
                let free = v.free_bytes.map_or_else(
                    || norte_i18n::t("volumes-size-unknown"),
                    norte_frontend::human_bytes,
                );
                let total = v.total_bytes.map_or_else(
                    || norte_i18n::t("volumes-size-unknown"),
                    norte_frontend::human_bytes,
                );
                let line = hostile_badged(
                    path_hostil || label_hostil || fs_type_hostil,
                    format!("{label_prefix}{path_txt}  {fs_type_txt}  {free} / {total}"),
                );
                lines.push(if i == *cursor {
                    format!("> {line}")
                } else {
                    format!("  {line}")
                });
            }
            if volumes.len() > modal::VOLUMES_LIMIT {
                let hidden_hostil = volumes.iter().enumerate().any(|(i, v)| {
                    (i < offset || i >= last)
                        && (norte_frontend::path_display(&v.mount).1
                            || v.label
                                .as_deref()
                                .is_some_and(|l| norte_frontend::display_name(l).1)
                            || norte_frontend::display_name(v.fs_type.as_bytes()).1)
                });
                let shown = last.to_string();
                let total = volumes.len().to_string();
                lines.push(hostile_badged(
                    hidden_hostil,
                    norte_i18n::ta(
                        "modal-volumes-more",
                        &[("shown", shown.as_str()), ("total", total.as_str())],
                    ),
                ));
            }
            lines
        }
    }
}

/// Prefija el badge hostil FUERA de la traducción (paridad TUI
/// `badge_prefixed`, audit MINOR-5: el mecanismo del badge no puede depender
/// de que cada locale conserve un `{ $badge }` — concatenación Rust-side,
/// translation-proof). Con espacio, como el resto de badges de esta GUI.
#[must_use]
fn hostile_badged(hostil: bool, line: String) -> String {
    if hostil {
        format!("{HOSTILE_BADGE} {line}")
    } else {
        line
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

/// Cabecera de sección de la vista de ajustes (`render_settings`): "General"
/// / "Plugins" intercaladas entre las filas, mismo criterio visual que
/// `draw_settings` en la TUI (`ui.rs`).
fn settings_section_header(text: String, chrome: &ChromeColors) -> impl IntoElement {
    div()
        .px(px(sp::S))
        .py(px(1.0)) // sub-XS: acento fino de una línea, fuera de la escala a propósito
        .text_color(chrome.header_fg)
        .child(SharedString::from(text))
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
        // #101: aviso de decodificación lossy junto al «via …» (misma clave
        // que la TUI).
        if v.preview_lossy() {
            header.push(' ');
            header.push_str(&norte_i18n::t("viewer-plugin-preview-lossy"));
        }
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

/// Color de UN span de un preview de plugin con estilo (G3a, ADR 0037):
/// `role` GANA sobre `fg` (el tema del usuario tiene precedencia sobre el
/// color fijo de un plugin, decisión 3 del ADR — MISMO criterio que la TUI,
/// `norte-tui::ui::draw_viewer`). `None` = ni rol ni fg aplican (o el rol
/// resuelto no trae color en el tema activo, p. ej. `Role::Regular` en
/// muchos presets): el caller NO fija `.text_color()`, el div hereda el
/// `chrome.fg` del contenedor — no un blanco a pelo como el fallback de
/// [`entry_color`] (ahí SIEMPRE hay una entrada que pintar; aquí "sin
/// color" es un estado legítimo, el texto normal del viewer). El glow de G1
/// (ADR 0036 D3) se aplica DESPUÉS de resolver el color, igual que
/// `entry_color`, tanto si viene de `role` como de `fg`.
fn styled_span_color(
    theme: &Theme,
    span: &norte_frontend::ansi::StyledSpan,
    glow: Option<effects::Glow>,
) -> Option<gpui::Rgba> {
    let resolved = match span.role {
        Some(role) => theme.style(role).fg,
        None => span.fg.map(|(r, g, b)| norte_theme::Color::rgb(r, g, b)),
    };
    resolved
        .map(theme_map::to_gpui_rgba)
        .map(|c| glowed(c, glow))
}

/// Color de un badge de decoración de plugin (G3b, ADR 0037): reutiliza
/// [`styled_span_color`] (mismo criterio "el rol gana" que un span de
/// preview con estilo, G3a — un único punto de resolución `Role → color`).
/// `DecorationWire` no lleva `fg` crudo (solo `role`, a diferencia de
/// `SpanWire`), así que sin rol reconocido cae a un tono DERIVADO de `base`
/// (el color por-tipo de la fila que ya se pinta) a alfa reducido — el
/// análogo GPUI más simple de un `Modifier::DIM` de terminal relativo, sin
/// justificar un campo nuevo en `ChromeColors` solo para este caso.
fn decoration_badge_color(
    theme: &Theme,
    role: Option<norte_theme::Role>,
    base: gpui::Rgba,
    glow: Option<effects::Glow>,
) -> gpui::Rgba {
    let span = norte_frontend::ansi::StyledSpan {
        text: String::new(),
        role,
        fg: None,
    };
    styled_span_color(theme, &span, glow).unwrap_or(gpui::Rgba { a: 0.55, ..base })
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

/// Interpola linealmente `a` hacia `b` por canal, `t` en `[0, 1]` (sin clamp:
/// call-sites del look-and-feel GP siempre pasan una constante fija dentro de
/// rango). Usado por `ChromeColors::hover_bg` — a diferencia de `glowed`, esto
/// mezcla DOS colores del tema entre sí, no un color hacia blanco, así que no
/// reutiliza esa función. El canal `a` también se interpola (a diferencia de
/// `glowed`, que lo deja intacto a propósito): `hover_bg` es un fondo sólido
/// nuevo, no un ajuste de brillo sobre un fg existente.
fn lerp_rgba(a: gpui::Rgba, b: gpui::Rgba, t: f32) -> gpui::Rgba {
    gpui::Rgba {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
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

/// Tasa del ciclo de flicker, en Hz (G2 decisión 3): cuántos ciclos
/// completos de seno por segundo. Elegida a ojo — bastante rápida para
/// leerse como "zumbido de CRT", bastante lenta para no leerse como un
/// estroboscopio (la AMPLITUD ya está acotada por
/// `effects::FLICKER_STRENGTH_RANGE` = `[0, 0.15]`, razón de accesibilidad;
/// esta constante es la mitad de FRECUENCIA de esa misma cautela, no
/// mandatada por el ADR, documentada aquí en vez de allí).
const FLICKER_HZ: f32 = 1.2;

/// Pura: el multiplicador de flicker para `elapsed_secs` transcurridos
/// desde `NorteGui::motion_epoch` (un `Instant`, NO el reloj de pared — sin
/// saltos de NTP/zona horaria que preocupar). Acotada a
/// `[1 - strength, 1 + strength]` (el seno está acotado a `[-1, 1]`) para
/// CUALQUIER `strength`; `strength = 0.0` es la identidad `1.0` exacta, así
/// un caller nunca necesita ramificar "sin flicker" antes de multiplicar
/// (ver `render`, que multiplica scanlines/vignette por este factor
/// incondicionalmente cuando hay `flicker` en el tema). `elapsed_secs` se
/// reduce módulo el período (`1 / FLICKER_HZ`) ANTES de entrar al seno
/// (rust-reviewer MINOR): un `f32` sin reducir acumula error de precisión
/// tras horas de sesión (~7 dígitos significativos), que se leería como
/// una deriva de fase — el módulo lo evita sin cambiar el resultado (el
/// seno ya es periódico).
fn flicker_factor(strength: f32, elapsed_secs: f32) -> f32 {
    let period = 1.0 / FLICKER_HZ;
    let phase_secs = elapsed_secs.rem_euclid(period);
    1.0 + strength * (2.0 * std::f32::consts::PI * FLICKER_HZ * phase_secs).sin()
}

/// Pura: `base` escalado por un `factor` de flicker, re-clampado a `cap`
/// (el TECHO estático per-key de `effects.rs` —
/// `SCANLINES_OPACITY_RANGE.1`/`VIGNETTE_STRENGTH_RANGE.1`). El flicker
/// jamás debe empujar un valor de tema YA clampado por encima de su PROPIO
/// tope de accesibilidad (ADR 0036 §2) — solo modula DENTRO de él. Piso
/// SIEMPRE `0.0`: ni el flicker ni un `base`/`factor` extremo producen una
/// opacidad/strength negativa.
fn flicker_scale(base: f32, factor: f32, cap: f32) -> f32 {
    (base * factor).clamp(0.0, cap)
}

/// Pura: ¿necesita este frame que el render loop siga pidiendo frames por
/// un efecto decorativo (G2 decisión 3)? `flicker` los necesita
/// DIRECTAMENTE — el overlay `canvas` de este módulo es dueño de su propia
/// fase de seno, sin maquinaria de GPUI detrás. El blink de cursor pide sus
/// PROPIOS frames vía `with_animation` de GPUI
/// (`AnimationElement::request_layout` llama a
/// `window.request_animation_frame()` cada vez que la fila parpadeante se
/// pinta de verdad) — incluirlo aquí TAMBIÉN es una aproximación
/// documentada, no una comprobación exacta: `cursor_blink` activo Y al
/// menos un pane con entradas (`any_pane_nonempty`) se toma como
/// "probablemente hay una fila resaltada en pantalla", sin verificar que
/// esa fila esté REALMENTE dentro del rango visible que `uniform_list`
/// virtualiza (rust-reviewer MINOR: en el borde raro de un `scroll_to_item`
/// aún no asentado, esto puede pedir un frame de más). El coste de acertar
/// de más NO es una fuga sostenida — el siguiente frame vuelve a evaluar
/// esta misma función con datos frescos — pero SÍ es un repintado real de
/// ambos panes ese frame, no gratis: la llamada a `request_animation_frame`
/// en sí se coalesce (ver su propia doc), el trabajo de repintado que
/// dispara no.
fn motion_active(flicker: bool, cursor_blink: bool, any_pane_nonempty: bool) -> bool {
    flicker || (cursor_blink && any_pane_nonempty)
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
/// Fallback de `Role::Warning`: ámbar de «mira esto», distinto del `err_fg`
/// rojo y del `fg` normal. Sin un canal propio, el panel de diferencias
/// pintaba «difiere» con el MISMO color que su texto atenuado — el ranking
/// de énfasis al revés (revisión rust MAJOR-1).
const WARN_FG: u32 = 0xfbbf24;
/// Fallback de `Role::Info`: gris de atenuación, para lo que acompaña sin
/// competir (cabeceras de columna, cuentas, líneas de teclas).
const INFO_FG: u32 = 0x9ca3af;
const QUICK_FG: u32 = 0xfbbf24;
/// Fondo de una fila MARCADA (distinto de `SEL_BG`, que es la selección bajo
/// cursor — marca y selección son ortogonales, ver `render_row`).
const MARK_BG: u32 = 0x3d3315;
/// Fallback del GLIFO de marca del canalón (#111): ámbar que lee bien
/// sobre `MARK_BG` y sobre el fondo del pane.
const MARK_FG: u32 = 0xd7af5f;

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

/// Color del glifo del canalón de marca (#111): el `fg` de [`Role::Mark`]
/// del tema, con fallback ámbar legible sobre [`MARK_BG`]. Función y no un
/// campo de `ChromeColors` porque solo lo usa el canalón — el struct pinea
/// sus campos en tests y crecerlo por un único caller no compra nada.
fn chrome_mark_fg(theme: &Theme) -> gpui::Rgba {
    chrome(theme, Role::Mark, true, MARK_FG)
}

/// Geometría del pulgar de una barra de scroll, en FRACCIONES del canal.
///
/// Una sola función para las dos mitades: el pintor la usa para colocar el
/// pulgar y el ratón para traducir una posición en un desplazamiento. Con dos
/// cuentas paralelas, arrastrar el pulgar lo dejaría donde el pintor no lo
/// pinta, que es peor que no poder arrastrarlo.
///
/// `None` cuando cabe todo: una barra que ocupa el canal entero no informa de
/// nada y solo roba ancho a la prosa.
fn scroll_geometry(offset: usize, visible: usize, total: usize) -> Option<(f32, f32)> {
    if total == 0 || visible >= total {
        return None;
    }
    let total_f = total as f32;
    // Un mínimo visible: con 2000 líneas la proporción exacta sería medio
    // píxel, que es lo mismo que no pintar nada.
    let alto = (visible as f32 / total_f).max(0.04);
    // El tope de arriba se acota para que el pulgar no se salga por abajo
    // cuando el `offset` está al final.
    let arriba = (offset as f32 / total_f).min(1.0 - alto);
    Some((arriba, alto))
}

/// La primera línea visible que corresponde a soltar el pulgar en `fraccion`
/// del canal.
///
/// La inversa de [`scroll_geometry`], y por eso vive a su lado: el pulgar se
/// agarra por su CENTRO, así que la fracción se corrige por media altura de
/// pulgar antes de convertirla en líneas. Sin esa corrección, agarrar el
/// pulgar por el medio lo tira hacia arriba en cuanto el ratón se mueve un
/// píxel.
fn scroll_offset_at(fraccion: f32, visible: usize, total: usize) -> usize {
    let Some((_, alto)) = scroll_geometry(0, visible, total) else {
        return 0;
    };
    let util = (1.0 - alto).max(f32::EPSILON);
    let rel = ((fraccion - alto / 2.0) / util).clamp(0.0, 1.0);
    let ultimo = total.saturating_sub(visible);
    (rel * ultimo as f32).round() as usize
}

/// Chrome horizontal fijo de un pane (#108 b6): `border_2` a ambos lados
/// (2px × 2) + `px(sp::S)` de padding a ambos lados + la MITAD del hueco
/// entre panes (`gap(px(sp::XS))` de la fila de panes — viewport/2 lo
/// ignora, y cada pane paga media). El canalón de marca va aparte (depende
/// de `fonts.size`).
const PANE_CHROME_PX: f32 = 4.0 + 2.0 * sp::S + sp::XS / 2.0;

/// Celdas mono que caben en el interior de un pane (#108 b6), aproximando
/// el ancho del pane como viewport/2 (los dos panes son `flex_1` iguales).
/// PURA a propósito (testeable sin `TextSystem`): el caller mide `ch` con
/// `cx.text_system().advance(…, '0')`. El error de aproximación lo absorbe
/// el nombre (`flex_1`) — `layout()` solo decide qué columnas CABEN.
fn pane_inner_cells(viewport_w: f32, ch: f32, gutter: f32) -> u16 {
    if ch <= 0.0 {
        return 0;
    }
    let inner = viewport_w / 2.0 - PANE_CHROME_PX - gutter;
    if inner <= 0.0 {
        return 0;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let cells = (inner / ch).floor() as u16;
    cells
}

/// Tipografía resuelta para la sesión (GP; corregido en la revisión final del
/// GP, hallazgo CRÍTICO): fuente de chrome (UI) para cabeceras/banners/franja
/// de tasks y fuente mono para listados/visor — alineación de columnas
/// (tamaños, hex del visor) exige ancho fijo, algo que una fuente UI
/// proporcional no garantiza. Se construye UNA vez en `new` a partir de
/// `[ui]` (`cfg.common.ui_font`/`ui_mono_font`/`ui_font_size`).
///
/// Mecanismo REAL (auditado contra GPUI en la rev `f14fea9` — el comentario
/// viejo de este módulo afirmaba lo contrario y era FALSO):
/// - **mono**: "JetBrains Mono", bundled DENTRO del binario (`include_bytes!`
///   en `main`, registrada con `cx.text_system().add_fonts(...)` antes de
///   abrir la ventana) — GPUI no embebe ninguna fuente propia; `.ZedMono` es
///   solo un ALIAS de nombre hacia el fontdb del SISTEMA (en la práctica
///   resuelve a "Lilex" si esa familia está instalada — casi nunca lo está).
///   Bundlear la fuente es lo único que hace el mono default resoluble
///   SIEMPRE, en cualquier máquina.
/// - **ui**: sigue siendo `.SystemUIFont` — un alias de GPUI que camina su
///   pila global de fuentes de chrome (algún sans del sistema, depende de la
///   plataforma). Para el chrome cualquier sans razonable sirve, así que no
///   hace falta bundlear nada aquí.
/// - **familias de usuario** (`[ui] font`/`mono_font`): validadas contra el
///   fontdb real (`cx.text_system().all_font_names()`, en `NorteGui::new`,
///   ver [`validated_family`]) — una familia que no exporta ese nombre
///   sustituye al default Y deja un aviso en el banner de arranque
///   (`gui-banner-font-unknown`), en vez de construir un `Font` cuya familia
///   primaria simplemente no existe (lo que antes degradaba en silencio a
///   cualquier fuente que GPUI encontrara al resolver, típicamente
///   proporcional).
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
    /// Construye el set a partir de familias YA VALIDADAS (ver
    /// [`validated_family`] — el caller, `NorteGui::new`, resuelve la
    /// familia final ANTES de llamar aquí; esta función no conoce el fontdb,
    /// así que es pura y testable sin un `TextSystem`) y el tamaño base
    /// (`None` = default de `[ui] font_size`).
    fn resolve(ui_family: &str, mono_family: &str, font_size: Option<f32>) -> Self {
        let size = font_size.unwrap_or(14.0);
        let row_h = (size * 1.5).round().max(18.0);
        Self {
            ui: family_with_fallback(ui_family, ".SystemUIFont"),
            mono: family_with_fallback(mono_family, "JetBrains Mono"),
            size: gpui::px(size),
            row_h: gpui::px(row_h),
        }
    }
}

/// Construye el `Font` de `family`. Cuando `family` difiere de `default` (una
/// familia de usuario, ya validada contra el fontdb real por
/// [`validated_family`] — jamás una familia inexistente, eso ahora se
/// sustituye ANTES de llegar aquí), adjunta `default` como
/// `Font::fallbacks`: cobertura per-glifo dentro de esa familia (p. ej. un
/// glifo que "Custom Mono" no tenga cae al glifo equivalente de "JetBrains
/// Mono"), NUNCA una fuente de repuesto para una familia primaria rota — si
/// `family` no existiera, la resolución de fuente de GPUI ignoraría este
/// `fallbacks` (es una cadena sobre las CARAS de la familia primaria) y
/// caminaría su pila global en su lugar, que es exactamente el hallazgo
/// CRÍTICO que este fix cierra.
/// What a finished `keymap.toml` write says, and whether it says it as an
/// error (K3c c4). Returns `(message, error)`.
///
/// PURE, and extracted for exactly the reason `flash_paints` and
/// `confirm_quit_should_open` are: this three-way decision is the one thing
/// c4 does that c3 did not have to, and a test that stops at the door — which
/// is every other test of this path — cannot see it.
///
/// - `changed` is `KeymapWrite::changed`: `false` means the file's bytes were
///   already what the write wanted.
/// - `applied` is whether the rebuild landed. It is a SEPARATE claim from the
///   write's success, because this frontend watches no files: the rebuild is
///   all-or-nothing, so a `keymap.toml` that will not load leaves the old
///   keyboard in place, and "saved" alone would then describe a key that did
///   not change.
/// - `unchanged` is passed by the UNBIND only. For a bind, "already bound to
///   that" and "just bound" are the same statement about the key; for an
///   unbind, "nothing matched" and "removed" are opposite ones — and only
///   that case is an error, because there "nothing happened" IS the answer
///   (#141: the entry may live in `[global]`, or be spelled another legal
///   way, and this path cannot reach either).
fn shortcut_write_message(
    changed: bool,
    applied: bool,
    ok: String,
    unchanged: Option<String>,
) -> (String, bool) {
    match unchanged {
        Some(nothing) if !changed => (nothing, true),
        _ if applied => (ok, false),
        // Saved but not applied is a WARNING, not a failure: the file really
        // did change, and the qualifier carries the rest.
        _ => (
            format!(
                "{ok} — {}",
                norte_i18n::t("gui-msg-shortcut-saved-not-applied")
            ),
            false,
        ),
    }
}

/// The layer stack the shortcut editor's door plans against, in ASCENDING
/// precedence and parallel to its kinds (K3c c4).
///
/// It is the config's own stack with `keymap::gui_supplement()` inserted
/// UNDERNEATH it, because that is what the loader really merges
/// (`keymap::build_effectives_layers3`): the map the resolver is using
/// carries bindings — `insert`, `ctrl+n`, `ctrl+b`, `ctrl+l`, `delete`, and
/// the viewer's `e`/`E`/`x` — that no `keymap.toml` anywhere contains. It
/// goes in as [`norte_config::Layer::System`], the lowest precedence
/// `RebindSources::split_at` models.
///
/// It changes no verdict TODAY, and saying so is more useful than implying
/// it does: the supplement sits below the write target, so it cannot shadow
/// one, and every entry in it is a single chord, so it cannot collide with
/// the prefix-free rule either. What it buys is that the door models the
/// stack that exists rather than a subset of it — the day the supplement
/// grows a two-chord sequence, or a layer order changes, the omission would
/// have been silent, and this frontend is the one that cannot absorb a
/// silent one: it watches no files, so a `keymap.toml` that fails to load
/// leaves the reader with the old keyboard and a message about a key that
/// changed.
fn rebind_layers(
    cfg: &norte_frontend::config::FrontendConfig,
) -> (
    Vec<norte_config::Layer>,
    Vec<norte_frontend::keymap::KeymapFile>,
) {
    let mut kinds = Vec::with_capacity(1 + cfg.keymap_layer_kinds.len());
    let mut layers = Vec::with_capacity(1 + cfg.keymap_layers.len());
    kinds.push(norte_config::Layer::System);
    layers.push(keymap::gui_supplement());
    kinds.extend_from_slice(&cfg.keymap_layer_kinds);
    layers.extend_from_slice(&cfg.keymap_layers);
    (kinds, layers)
}

/// What [`reload_after_keymap_write`] brings back: the merged config, and
/// the three effectives rebuilt from it (absent when the config itself did
/// not reload — there is no preset name to build from then).
type ReloadedKeymap = (
    Result<norte_frontend::config::FrontendConfig, norte_config::ConfigError>,
    Option<
        Result<
            (
                norte_frontend::keymap::Effective,
                norte_frontend::keymap::Effective,
                norte_frontend::keymap::Effective,
            ),
            norte_frontend::keymap::KeymapError,
        >,
    >,
);

/// Re-reads the merged config and rebuilds the three effectives, the way a
/// file watcher would if this frontend had one (K3c c4).
///
/// It does not: the GUI resolves its layers at start-up and after a settings
/// write, so a `keymap.toml` the shortcut editor just wrote reaches the
/// keyboard only because the write path calls this. BLOCKING (two rounds of
/// config I/O) — callers run it on the background executor, rule 2.
///
/// Two seams worth knowing about, both shared with `commit_settings_write`
/// and neither introduced here:
///
/// - it reads the config TWICE (here, and again inside `build_effectives3`),
///   under no lock. Each read is atomic — every writer renames — so neither
///   can tear, but another norte writing between them leaves `cfg_snapshot`
///   (what the door plans from) describing a different file state than the
///   effectives (what the verdicts are read off);
/// - the layer set comes from `standard_layers()` while the write went to
///   `user_config_dir()`. With no `HOME` at all the two can disagree, which
///   in this frontend shows up as a write that "did not apply" rather than
///   as an error.
fn reload_after_keymap_write() -> ReloadedKeymap {
    let cfg = norte_frontend::config::load(&norte_config::standard_layers());
    // `build_effectives3` re-reads the layers itself, from the same
    // `standard_layers()`; only the PRESET name comes from the config just
    // loaded, which is why an unreadable config leaves this `None` rather
    // than guessing `orthodox` and installing somebody else's keyboard.
    let keymap = cfg
        .as_ref()
        .ok()
        .map(|c| keymap::build_effectives3(&c.common.preset));
    (cfg, keymap)
}

/// An empty [`norte_frontend::config::FrontendConfig`] (S4): the fallback
/// for `NorteGui::cfg_snapshot` when startup's `loaded` was `Err`. Safe to
/// `expect` — an EMPTY `Layers` never touches the filesystem, so
/// `norte_frontend::config::load` cannot fail on it (the same invariant
/// `norte-frontend`'s own tests rely on for their `cfg_vacia()` fixture,
/// e.g. `settings::tests::cfg_vacia`).
fn empty_frontend_config() -> norte_frontend::config::FrontendConfig {
    norte_frontend::config::load(&norte_config::Layers { dirs: Vec::new() })
        .expect("Layers vacío nunca toca el sistema de ficheros")
}

fn family_with_fallback(family: &str, default: &'static str) -> gpui::Font {
    let mut font = gpui::font(family);
    if family != default {
        font.fallbacks = Some(gpui::FontFallbacks::from_fonts(vec![default.to_owned()]));
    }
    font
}

/// Valida `requested` contra `known` (el fontdb REAL —
/// `cx.text_system().all_font_names()`, ver `NorteGui::new`): `None` o una
/// familia ausente de `known` caen a `default` (mono: `"JetBrains Mono"`,
/// bundled — siempre presente; ui: `".SystemUIFont"`). Devuelve la familia
/// resuelta más, si hubo sustitución por familia DESCONOCIDA (no por `None`
/// — eso es simplemente "sin config", no un error de usuario), el nombre
/// pedido para el banner de arranque (`gui-banner-font-unknown`).
///
/// Pura a propósito (GP review fix 2): sin esto, la ÚNICA forma de probar la
/// sustitución de familia sería levantar un `TextSystem` de GPUI completo.
fn validated_family(
    requested: Option<&str>,
    known: &[String],
    default: &'static str,
) -> (String, Option<String>) {
    match requested {
        None => (default.to_owned(), None),
        Some(f) if known.iter().any(|k| k == f) => (f.to_owned(), None),
        Some(f) => (default.to_owned(), Some(f.to_owned())),
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
/// `hover_bg` NO es un rol de tema — es DERIVADO (`lerp(pane_bg_focus,
/// sel_bg, 0.35)`, ver `resolve`): a diferencia de los pares de arriba, ningún
/// tema declara un color de hover, así que en vez de inventar un rol nuevo
/// (que cada preset tendría que rellenar) se interpola entre dos que YA
/// existen — se lee como "casi seleccionado" en cualquier tema sin tocar
/// `norte-theme`.
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
    /// `Role::Warning`: «hay algo que mirar aquí», sin llegar a error.
    /// PARA TEXTO SOBRE `bg`/`pane_bg`, como `err_fg` — no sobre
    /// `header_bg`, que trae su propio par.
    warn_fg: gpui::Rgba,
    /// `Role::Info`: atenuación. Un rol de tema de verdad, y no `quick_fg`
    /// prestado — ese es la MITAD de un par (`Match.fg` + `Match.bg`) y
    /// usarlo suelto sobre otro fondo es la pareja de contraste sin auditar
    /// que este repo ya envió una vez (ver el banner de `keymap_error`).
    info_fg: gpui::Rgba,
    quick_fg: gpui::Rgba,
    quick_bg: gpui::Rgba,
    mark_bg: gpui::Rgba,
    hover_bg: gpui::Rgba,
}

impl ChromeColors {
    fn resolve(theme: &Theme) -> Self {
        let pane_bg_focus = chrome(theme, Role::PaneFocusBackground, false, PANE_BG_FOCUS);
        let sel_bg = chrome(theme, Role::Selection, false, SEL_BG);
        Self {
            bg: chrome(theme, Role::Background, false, BG),
            fg: chrome(theme, Role::Regular, true, FG),
            pane_bg: chrome(theme, Role::PaneBackground, false, PANE_BG),
            pane_bg_focus,
            // Par cabecera: ambos canales de `StatusBar` (ver doc del struct).
            header_bg: chrome(theme, Role::StatusBar, false, HEADER_BG),
            header_fg: chrome(theme, Role::StatusBar, true, FG),
            border_focus: chrome(theme, Role::BorderFocus, true, BORDER_FOCUS),
            border_unfocus: chrome(theme, Role::BorderUnfocused, true, BORDER_UNFOCUS),
            sel_bg,
            // Sin fallback histórico: si el tema no declara `Selection.fg`,
            // `None` = la fila seleccionada conserva su color por-tipo
            // (comportamiento de siempre; nunca hubo un fg de selección).
            sel_fg: theme.style(Role::Selection).fg.map(theme_map::to_gpui_rgba),
            err_fg: chrome(theme, Role::Error, true, ERR_FG),
            warn_fg: chrome(theme, Role::Warning, true, WARN_FG),
            info_fg: chrome(theme, Role::Info, true, INFO_FG),
            // Par quick-search: ambos canales de `Match` (ver doc del struct).
            // El fallback de `quick_bg` es `HEADER_BG`: el combo histórico
            // (3 de los 4 usos) ya pintaba el resaltado sobre ese fondo.
            quick_fg: chrome(theme, Role::Match, true, QUICK_FG),
            quick_bg: chrome(theme, Role::Match, false, HEADER_BG),
            mark_bg: chrome(theme, Role::Mark, false, MARK_BG),
            // Derivado, no un rol de tema — ver doc del struct.
            hover_bg: lerp_rgba(pane_bg_focus, sel_bg, 0.35),
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
        self.warn_fg = glowed(self.warn_fg, g);
        self.info_fg = glowed(self.info_fg, g);
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

        // Un menú contextual abierto caduca si el listado se movió bajo él
        // (va ANTES del gesto: cerrarlo devuelve la vigencia del ratón a
        // «sin overlay delante» en este mismo frame).
        self.expire_stale_context_menu();

        // Un gesto de ratón en vuelo caduca aquí si el listado se movió bajo
        // el puntero o si algo se puso delante (ver el método).
        self.expire_stale_mouse_gesture();

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
            // Release fuera de toda fila (cromo, franja de tasks, hueco bajo
            // el listado): cierra el gesto CANCELÁNDOLO. Los listeners de la
            // raíz se registran antes que los de las filas y la fase de
            // burbuja los recorre al revés, así que si el release cayó sobre
            // una fila ella ya lo consumió y esto no encuentra nada armado
            // (ver la cabecera del módulo).
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseUpEvent, _w, cx| {
                    this.on_mouse_release(None, mouse_mods(ev.modifiers), cx);
                }),
            )
            // Shift baja o sube A MITAD del arrastre y el puntero no se
            // mueve: sin esto el aviso seguiría diciendo «copiar» mientras
            // el drop ya movería (la decisión se lee AL SOLTAR). GPUI
            // despacha `ModifiersChanged` por el camino del FOCO, y la raíz
            // lo tiene (`track_focus`), así que un solo listener aquí cubre
            // la ventana entera. Solo repinta si hay un gesto armado: el
            // modificador se pulsa mil veces por sesión fuera de un
            // arrastre.
            .on_modifiers_changed(
                cx.listener(|this, ev: &gpui::ModifiersChangedEvent, _w, cx| {
                    this.mouse.mods = mouse_mods(ev.modifiers);
                    if this.mouse.drag.kind().is_some() {
                        cx.notify();
                    }
                }),
            )
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
            .p(px(sp::S))
            .gap(px(sp::XS));

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
        //
        // K3a MAJOR-1: el número de líneas se guarda (`keymap_error_lines`)
        // — `render_viewer` lo necesita para no pedirle a `v.rows` más filas
        // de las que este banner (si aparece) va a dejarle sitio.
        let keymap_error_lines = self
            .keymap_error
            .as_deref()
            .map_or(0, |m| m.split('\n').count());
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
                        .px(px(sp::S))
                        .py(px(1.0)) // sub-XS: acento fino de una línea, fuera de la escala a propósito
                        .truncate()
                        .child(SharedString::from(line.to_owned())),
                );
            }
            root = root.child(banner);
        }

        // Flash transitorio (#108 7c): resultado del guardado del picker —
        // una línea, mismo lenguaje visual que el banner de arranque
        // (err_fg sobre bg para errores; header para éxito). Se despide con
        // la siguiente tecla o click (`on_key`/`on_row_click`/
        // `on_sort_click`). Excluido de ajustes/extensiones (F11/F12): esas
        // vistas a pantalla completa tienen su PROPIA línea de estado y un
        // persist que aterriza mientras están abiertas no debe pintar una
        // línea suelta encima de la suya (review 7c MINOR-4b).
        //
        // K3a pagó la deuda del visor: hasta aquí el visor SE EXCLUÍA
        // también, así que un `Resolution::Unavailable` tecleado con el
        // visor abierto ponía este campo a `Some` sin que nada lo pintara
        // (`main.rs` T4/T5 arriba). El visor no tiene su propia franja de
        // estado editable (su `status` es de solo lectura, ver
        // `render_viewer`), así que esta línea es EXACTAMENTE donde debía
        // aparecer: en flujo normal, empuja el visor hacia abajo como
        // cualquier otro banner (`keymap_error` ya lo hacía sin caso
        // especial) — nunca lo tapa, porque no es un overlay absoluto.
        //
        // K3a MAJOR-1: `flash_shown` se guarda por la misma razón que
        // `keymap_error_lines` — `render_viewer` necesita saber SI esta
        // línea va a aparecer antes de decidir cuántas filas pedirle a
        // `v.rows`.
        let flash_shown = flash_paints(self.settings_view.is_some(), self.extensions.is_some())
            && self.flash.is_some();
        if flash_shown && let Some((msg, is_error)) = &self.flash {
            let (bg, fg) = if *is_error {
                (chrome.bg, chrome.err_fg)
            } else {
                (chrome.header_bg, chrome.header_fg)
            };
            root = root.child(
                div()
                    .px(px(sp::S))
                    .py(px(1.0)) // sub-XS: acento fino de una línea
                    .bg(bg)
                    .text_color(fg)
                    .truncate()
                    .child(SharedString::from(msg.clone())),
            );
        }

        // Aviso de lo que haría SOLTAR ahora mismo (tarea 5): a qué pane,
        // cuántas entradas y si copia o mueve. No es decoración — el mismo
        // gesto significa marcar o transferir según dónde acabe (promoción,
        // ver `norte_frontend::mouse`) y el flag copiar/mover se lee al
        // soltar, así que esta línea es lo único que se interpone entre el
        // usuario y una mutación que creía otra. Sale de `Drag::pending`,
        // que responde con las MISMAS reglas que el drop: la etiqueta no
        // puede prometer una cosa y la operación hacer otra.
        let drop_target = drop_hint(&self.panes, self.mouse.drag.pending(self.mouse.mods));
        let drop_pane: Option<usize> = drop_target.as_ref().map(|(p, _)| *p);
        if let Some((_, msg)) = &drop_target {
            root = root.child(
                div()
                    .px(px(sp::S))
                    .py(px(1.0)) // sub-XS: acento fino de una línea
                    // Par honesto de `Match` (fondo Y texto), el mismo que
                    // usa el quick search: un estado VIVO del puntero, no un
                    // resultado como el flash.
                    .bg(chrome.quick_bg)
                    .text_color(chrome.quick_fg)
                    .truncate()
                    .child(SharedString::from(msg.clone())),
            );
        }

        // Vista de ajustes (F11, S4) a pantalla completa, gestor de
        // extensiones (F12, G3c) a pantalla completa, visor (F3), el
        // estado «abriendo…» mientras llega, o el dual-pane: pantallas
        // mutuamente excluyentes (ver `on_key`, que las enruta con la misma
        // prioridad: ajustes de config > extensiones > visor > dual-pane).
        if self.shortcuts_view.is_some() {
            // K3c c4: se pinta EN LUGAR de la vista de ajustes, que sigue
            // abierta en el estado y recupera la pantalla al cerrarse este —
            // el editor es una pantalla DE ajustes, no un reemplazo.
            root = root.child(self.render_shortcuts(
                &chrome,
                self.shortcut_rows(window.viewport_size(), keymap_error_lines),
                cx,
            ));
        } else if self.settings_view.is_some() {
            root = root.child(self.render_settings(&chrome, cx));
        } else if self.extensions.is_some() {
            root = root.child(self.render_extensions(&chrome));
        } else if self.viewer.is_some() {
            // K3a MAJOR-1: cuántas filas de chrome EXTRA (más allá de las
            // propias del visor, `VIEWER_CHROME_ROWS`) ya se pintaron ARRIBA
            // de este árbol esta misma vuelta — el banner de arranque, el
            // flash — para que `render_viewer` deje de pedirle a `v.rows`
            // más filas de las que el `flex_1` real le va a dejar sitio.
            let extra_chrome_rows = keymap_error_lines + usize::from(flash_shown);
            root = root.child(self.render_viewer(window, &chrome, extra_chrome_rows, cx));
        } else if self.viewer_loading {
            root = root.child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(SharedString::from(norte_i18n::t("gui-viewer-opening"))),
            );
        } else if let Some(view) = &self.compare {
            // #158 fase C1 tarea 3. Ocupa el sitio de los DOS panes: una fila
            // tiene dos caras y un veredicto en medio, así que no cabe en
            // media pantalla (mismo reparto que la TUI). La franja de tasks se
            // queda debajo — la comparación no entra en ella (su progreso lo
            // pinta el pie del propio panel), pero las OTRAS tasks siguen
            // corriendo y el lector tiene que poder verlas.
            //
            // El árbol se construye en `compare_view::render`, no aquí: este
            // fichero ya reparte diez pantallas y ninguna de ellas cabe dos
            // veces.
            root = root
                .child(compare_view::render(
                    view,
                    &chrome,
                    &self.fonts,
                    &self.compare_scroll,
                    cx,
                ))
                .child(self.render_task_strip(&chrome));
        } else {
            let panes_row = div()
                .flex_1()
                .flex()
                .flex_row()
                .overflow_hidden()
                .gap(px(sp::XS))
                .child(self.render_pane(0, &chrome, drop_pane, window, cx))
                .child(self.render_pane(1, &chrome, drop_pane, window, cx));
            root = root.child(panes_row).child(self.render_task_strip(&chrome));
        }

        // Indicador de secuencia multi-tecla en curso (#91): si el resolver
        // ACTIVO (el del visor cuando está abierto, si no el de Browse — mismo
        // criterio de ruteo que `on_key`) tiene una secuencia pendiente
        // (`pending()` no vacío), pinta al pie los chords tecleados +«…». El
        // preset orthodox no trae secuencias, así que esto se ejerce con un
        // `keymap.toml` de usuario que ligue una (p. ej. `g g`).
        // La vista de ajustes no tiene un `Resolver` de secuencias (edita
        // tecla a tecla, `settings_view::on_key`) — sin indicador aquí
        // mientras está abierta.
        let active_resolver = if self.viewer.is_some() {
            &self.viewer_resolver
        } else {
            &self.resolver
        };
        // K2a: el CONTADOR a medio teclear se pinta en el mismo sitio y por la
        // misma razón — un contador que no se ve es un contador que no se
        // puede cancelar. Se suprime con los mismos overlays que la secuencia.
        let (count, pending) = if self.settings_view.is_some()
            || self.extensions.is_some()
            || self.palette.is_some()
            || self.columns_picker.is_some()
            || self.context_menu.is_some()
            // El panel de diferencias también (revisión rust MAJOR-4): se
            // queda el teclado entero y no pasa NADA por el resolver de
            // Browse, así que un prefijo pendiente pintado sobre él describe
            // teclas que ahí no significan lo que dice — y con un contador a
            // medio teclear es peor, porque `5` dentro del panel enciende un
            // filtro mientras el indicador anuncia un 5 que era otra cosa.
            // `CompareStarted` además lo resetea, así que esto es el cinturón.
            || self.compare.is_some()
        {
            (None, &[][..])
        } else {
            (active_resolver.count(), active_resolver.pending())
        };
        // K3a: the which-key panel — what a pending PREFIX can do next. Same
        // anchor and suppression as the strip above; a bare count has no
        // prefix (`pending` empty) so it stays with the plain-text line,
        // matching "a bare count does not open it" (`whichkey`'s rule).
        //
        // Gated on the LIVE `pending` just computed, not on `self.which_key`
        // alone: `active_resolver` above already reflects whichever resolver
        // owns the keyboard THIS frame, including a switch that happened
        // with no keystroke at all (settings/extensions/palette/picker/menu
        // opening, or the viewer opening from a mouse double-click). When
        // that switch lands, the new resolver's `pending()` is what changes
        // — the cached rows may still describe the OLD one for one frame,
        // but `!pending.is_empty()` is false in that case and nothing about
        // them gets painted. The cache catches up on the next transition
        // that calls `refresh_which_key`/`refresh_which_key_viewer`.
        //
        // `self.modal`/`self.help` are NOT in the suppression list above —
        // rust-reviewer BLOCKER: both can take the keyboard away from the
        // active resolver WITHOUT a keystroke that would have cleared
        // `self.which_key`, so the live-`pending` guard alone does not catch
        // them. A modal can open from a background task landing (a
        // conflict, an AI-rename reply, a semantic-search reply — none of
        // them go through `on_key`); F1 opens help from INSIDE the viewer's
        // key handler, before `viewer_resolver.push` ever runs. Both leave
        // the resolver's pending prefix (and these cached rows) exactly as
        // they were — correct, mirroring the TUI's `app.modal.is_none()`
        // draw guard (`norte-tui/src/ui.rs`): the sequence is still live
        // underneath and the panel reappears once the overlay closes. It
        // just must not bleed through the overlay's translucent scrim
        // (`rgba(0x000000aa)`, not opaque) while that overlay is up —
        // `which_key_paints` is the extracted, testable gate for that.
        let wk_ready = self.which_key.as_ref().filter(|wk| !wk.is_empty());
        if which_key_paints(
            pending.is_empty(),
            wk_ready.is_some(),
            self.modal.is_some(),
            self.help.is_some(),
        ) && let Some(wk) = wk_ready
        {
            root = root.child(self.render_which_key(wk, &chrome));
        } else if let Some(hint) = pending_indicator(count, pending) {
            root = root.child(
                div()
                    .px(px(sp::S))
                    .py(px(1.0)) // sub-XS: acento fino de una línea, fuera de la escala a propósito
                    .bg(chrome.quick_bg)
                    .text_color(chrome.quick_fg)
                    .child(SharedString::from(hint)),
            );
        }

        // Menú contextual (tarea 4 del plan de ratón): panel ANCLADO al
        // puntero sobre un scrim transparente que ocluye el ratón — así un
        // click fuera lo cierra sin que además mueva el cursor de la fila que
        // hay debajo. Va antes de los overlays centrados (paleta/picker/
        // modal), que siguen ganando encima si llegaran a coexistir.
        if let Some(menu) = &self.context_menu {
            root = root.child(self.render_context_menu(menu, &chrome, window.viewport_size(), cx));
        }

        // Overlay de la paleta de comandos (G3c, `ctrl+p`): un scrim +
        // panel centrado, MISMO patrón absoluto que el modal — pero pintado
        // ANTES de él (el modal, comprobado justo debajo, sigue ganando
        // visualmente si ambos llegaran a coexistir; `on_key` ya lo impide
        // por construcción, esto es defensa en profundidad del render).
        if let Some(view) = &self.palette {
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgba(0x000000aa))
                    .child(self.render_palette(view, &chrome)),
            );
        }

        // Overlay de la ayuda (H3f, `F1`): mismo patrón que la paleta —
        // scrim + panel centrado, pintado antes del modal, que sigue ganando
        // encima (`on_key` ya lo impide por construcción).
        if let Some(view) = &self.help {
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .occlude()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgba(0x000000aa))
                    .child(self.render_help(view, &chrome, window, cx)),
            );
        }

        // Overlay del picker de columnas (#108 7c): mismo patrón que la
        // paleta, pintado antes del modal (el modal sigue ganando encima).
        // `.occlude()`: a diferencia de los scrims heredados, este SÍ come
        // los eventos de ratón — sin él, un click detrás robaba foco/cursor
        // y un doble-click hacía cd bajo el picker (review 7c MINOR-2; el
        // barrido de los demás overlays sigue siendo follow-up).
        if let Some(view) = &self.columns_picker {
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .occlude()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgba(0x000000aa))
                    .child(self.render_columns_picker(view, &chrome)),
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
        // Fase de flicker (G2 decisión 3): UN `elapsed()` por frame desde
        // `motion_epoch` (`Instant`, ver doc del campo), capturado por el
        // closure `'static` del `canvas` de abajo. Barato incondicionalmente
        // (una resta de `Instant`) — no vale la pena ramificar "solo si hay
        // flicker" antes de calcularlo.
        let elapsed = self.motion_epoch.elapsed().as_secs_f32();
        let has_overlay = self
            .effects
            .is_some_and(|e| e.scanlines.is_some() || e.vignette.is_some());
        // El flicker sin scanlines/vignette no tiene NADA que modular — el
        // overlay `canvas` de abajo ni se crea en ese caso, así que el
        // gate de `motion_active` de más abajo debe reflejar eso, no solo
        // "el tema declara `[effects.flicker]`".
        let flicker_paints = has_overlay && self.effects.is_some_and(|e| e.flicker.is_some());

        if let Some(eff) = self.effects
            && has_overlay
        {
            root = root.child(
                canvas(
                    move |_bounds, _window, _cx| {},
                    move |bounds, (), window, cx| {
                        // El chequeo directo de `reduce_motion`/foco (belt,
                        // decisión 2/3): este `canvas` NO pasa por
                        // `with_animation` (pinta primitivas de escena
                        // crudas, no un elemento GPUI animable), así que
                        // `App::reduce_motion`'s gate nativo no lo cubre —
                        // hay que replicarlo a mano, como pide la doc de
                        // `Window::request_animation_frame` para callers
                        // directos.
                        let factor = match eff.flicker {
                            Some(f) if !cx.reduce_motion() && window.is_window_active() => {
                                flicker_factor(f.strength, elapsed)
                            }
                            _ => 1.0,
                        };
                        let scanlines = eff.scanlines.map(|s| effects::Scanlines {
                            opacity: flicker_scale(
                                s.opacity,
                                factor,
                                effects::SCANLINES_OPACITY_RANGE.1,
                            ),
                            ..s
                        });
                        let vignette = eff.vignette.map(|v| effects::Vignette {
                            strength: flicker_scale(
                                v.strength,
                                factor,
                                effects::VIGNETTE_STRENGTH_RANGE.1,
                            ),
                        });
                        paint_scanlines(window, bounds, scanlines);
                        paint_vignette(window, bounds, vignette);
                    },
                )
                .absolute()
                .inset_0(),
            );
        }

        // Frame loop del movimiento (G2 decisión 3): pide el PRÓXIMO frame
        // SOLO si hace falta (`motion_active`) Y la ventana tiene foco Y
        // `reduce_motion` está OFF — así una sesión sin flicker/cursor_blink
        // activos (o con `reduce_motion = true`, o desenfocada) sigue siendo
        // puramente dirigida por eventos, exactamente como antes de G2 (el
        // Goal del plan: "frame loop alive ONLY while an animated effect is
        // active and the window focused"). El blink de cursor YA pide sus
        // PROPIOS frames vía `with_animation` cuando su fila se pinta
        // (`render_row`) — esta llamada directa es la única forma de que el
        // flicker del `canvas` de arriba (que no pasa por `with_animation`)
        // se re-pinte en el SIGUIENTE frame; `Window::
        // request_animation_frame`'s propia doc pide gatear a mano sobre
        // `reduce_motion` para callers directos (rev f14fea9,
        // window.rs:2229).
        let cursor_blink_on = self.effects.and_then(|e| e.cursor_blink) == Some(true);
        let any_pane_nonempty = self.panes.iter().any(|p| !p.entries().is_empty());
        if motion_active(flicker_paints, cursor_blink_on, any_pane_nonempty)
            && window.is_window_active()
            && !cx.reduce_motion()
        {
            window.request_animation_frame();
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

/// Parsea una `key` de fila de plugin de la paleta
/// (`plugin:{plugin_id}:{command_id}`, [`norte_frontend::palette::plugin_rows`])
/// de vuelta a `(plugin_id, command_id)` — mirror EXACTO de la TUI's
/// `parse_plugin_key` (`norte-tui/src/main.rs`): el `plugin_id` es
/// reverse-DNS charset-validado por el core (nunca lleva `:`), el
/// `command_id` del manifiesto NO tiene charset validado y puede llevar
/// cualquier byte incluido `:` — el PRIMER `:` tras el prefijo `plugin:`
/// separa sin ambigüedad, el resto (sin volver a partir) es el
/// `command_id` crudo.
fn parse_plugin_palette_key(cmd: &str) -> Option<(&str, &str)> {
    let (id, command) = cmd.strip_prefix("plugin:")?.split_once(':')?;
    (!id.is_empty()).then_some((id, command))
}

/// Tope en chars de la etiqueta de un candidato dentro de `msg-terminal-*`.
const TERMINAL_LABEL_MAX: usize = 40;

/// Sonda + lanzamiento del emulador de terminal (#135, §E). Corre en el
/// executor de FONDO: `resolve_program` mira el disco (regla 2).
///
/// Devuelve el nombre del que arrancó, o el mensaje ya localizado del fallo.
/// Un no-op mudo aquí sería indistinguible de una tecla rota.
///
/// # Lo que la review de S4 cambió
///
/// - Se lanza la ruta ABSOLUTA que devolvió la sonda, no el nombre a secas
///   (MAJOR-2). En unix `current_dir` se aplica ANTES de resolver el
///   programa, así que un `kitty` suelto lo resolvería `execvp` contra el
///   directorio que el usuario está navegando: con un `.` en el `PATH`, un
///   fichero llamado `kitty` dentro de un archivo recién extraído. Sonda y
///   lanzamiento miraban directorios distintos por construcción.
/// - Un candidato que falla al lanzarse NO aborta la lista (MINOR-4/L2):
///   se anota y se sigue. Antes, un `xdg-terminal-exec` presente pero roto
///   dejaba sin probar todo lo demás.
/// - `$TERMINAL` se INTENTA siempre, aunque la sonda diga que no está (L2).
///   La sonda tiene falsos negativos reales —una GUI lanzada sin `PATH` en
///   su entorno, o un binario guardado en NFD frente a un `$TERMINAL` en NFC
///   en APFS— y descartar en silencio la respuesta EXPLÍCITA del usuario es
///   el peor sitio donde tenerlos.
/// - El informe final no junta el `$TERMINAL` del usuario con la lista
///   propia en un `", "` (encoding M3): un `TERMINAL='kitty, konsole'` se
///   leería como dos entradas. Van en argumentos separados.
///
/// El hijo va DESACOPLADO —stdio a `null`, sin esperarlo— y se entierra con
/// un hilo que solo hace `wait`: sin él quedaría zombi hasta que muriese la
/// propia GUI. Ese hilo además REGISTRA una salida no-cero, que es la única
/// pista de que el emulador arrancó y se rindió (un flag de cwd que su
/// versión no acepta, un `xdg-terminal-exec` sin entrada de escritorio).
fn spawn_terminal(
    candidatos: &[Vec<std::ffi::OsString>],
    cwd: &std::path::Path,
    configurado: bool,
) -> Result<String, String> {
    let mut propios: Vec<String> = Vec::new();
    let mut etiqueta_configurada = String::new();
    for (i, argv) in candidatos.iter().enumerate() {
        let Some(programa) = argv.first() else {
            continue;
        };
        // `$TERMINAL`, cuando lo hay, es SIEMPRE el primer candidato
        // (`terminal_candidates_from`). Es entrada del usuario, así que su
        // etiqueta va saneada Y marcada, como cualquier otro texto ajeno de
        // esta GUI (encoding M1: perder el flag deja `term\xFF` y `term\xFE`
        // indistinguibles y sin aviso de que lo mostrado no es lo probado).
        let del_usuario = configurado && i == 0;
        let (masked, hostil) = norte_frontend::display_name(programa.as_encoded_bytes());
        // Acotada además de enmascarada (encoding m10): `display_name` no
        // topa, y un `$TERMINAL` kilométrico daría un flash kilométrico.
        let etiqueta = norte_frontend::middle_ellipsis(&masked, TERMINAL_LABEL_MAX);
        if del_usuario {
            etiqueta_configurada = if hostil {
                format!("{HOSTILE_BADGE} {etiqueta}")
            } else {
                etiqueta.clone()
            };
        } else {
            propios.push(etiqueta.clone());
        }
        // La sonda necesita un `&str`; un nombre que no es UTF-8 no se puede
        // sondear. Y la respuesta EXPLÍCITA del usuario se intenta pase lo
        // que pase. En ambos casos se lanza el nombre tal cual y el error del
        // spawn es la respuesta honesta.
        let resuelto = programa
            .to_str()
            .and_then(norte_frontend::openers::resolve_program);
        let ejecutable: &std::ffi::OsStr = match &resuelto {
            Some(abs) => abs.as_os_str(),
            None if del_usuario || programa.to_str().is_none() => programa.as_os_str(),
            None => continue,
        };
        let hijo = std::process::Command::new(ejecutable)
            .args(&argv[1..])
            .current_dir(cwd)
            .env(
                norte_frontend::shell::LEVEL_VAR,
                norte_frontend::shell::next_norte_level(),
            )
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match hijo {
            Ok(mut c) => {
                let nombre = etiqueta.clone();
                std::thread::spawn(move || match c.wait() {
                    Ok(st) if !st.success() => {
                        tracing::warn!(
                            program = %nombre,
                            code = st.code().unwrap_or(-1),
                            "terminal emulator exited non-zero; the window may never have opened"
                        );
                    }
                    _ => {}
                });
                return Ok(etiqueta);
            }
            Err(e) => {
                // Un candidato roto no puede llevarse por delante los que
                // vienen detrás.
                tracing::warn!(program = %etiqueta, error = %e, "terminal candidate failed to spawn");
            }
        }
    }
    let tried = propios.join(", ");
    if configurado {
        Err(norte_i18n::ta(
            "msg-terminal-none",
            &[("configured", &etiqueta_configurada), ("tried", &tried)],
        ))
    } else {
        Err(norte_i18n::ta(
            "msg-terminal-none-unset",
            &[("tried", &tried)],
        ))
    }
}

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
/// Serializa TODA escritura de `norte.toml` del GUI (review 7c MAJOR-1):
/// las tareas de `background_spawn` van detached en un pool multihilo, y dos
/// writers intercalados perderían la actualización del primero. Se bloquea
/// DENTRO del background task, jamás en el hilo de render.
///
/// K3c c4 — corrección de este comentario, que decía «los persist son
/// read-modify-write SIN lock ni tmp+rename». Desde #116 SÍ los tienen:
/// `persist_set` toma `norte.toml.lock` y escribe por tmp+rename, así que
/// este mutex es hoy un cinturón sobre un tirante, no la única barrera. Y la
/// regla que se deduce de él NO es «todo persist necesita este mutex»: es
/// **un fichero, un lock, nunca anidados**. `persist_keymap_bind` toma su
/// PROPIO `keymap.toml.lock` (jamás el de `norte.toml`), que ya serializa
/// dos escrituras del mismo proceso — `File::lock` es `flock` sobre la
/// descripción de fichero abierta en Unix y `LockFileEx` sobre el handle en
/// Windows, y `lock_config_file` abre uno nuevo cada vez —, así que el
/// editor de atajos no lo toma y no debe. Tomar los dos desde caminos
/// distintos es lo único que podría construir un ciclo.
static CONFIG_WRITE_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
        // K2a: las dos reglas de carga nuevas traen DOS fragmentos de usuario
        // cada una (el chord y el comando), igual que `AmbiguousPrefix` — el
        // mismo par saneado. `reserved_for` de `SacredKey` NO entra: es un
        // literal `&'static str` del motor (`"pane.switch"`), sin payload de
        // usuario, y repetir el nombre del comando reservado no dice nada que
        // el banner no diga ya.
        KeymapError::AmbiguousPrefix {
            shorter: a,
            longer: b,
        }
        | KeymapError::DigitBoundWithCounts { chord: a, run: b }
        | KeymapError::SacredKey {
            chord: a, run: b, ..
        } => {
            format!("{} / {}", banner_safe(a), banner_safe(b))
        }
        // `dialog_from` is refused outright in a user/project layer
        // (`parse_keymap_layer` → `WrongLayerKey`, before it is ever
        // resolved), so these three can only fire while resolving a
        // FACTORY preset's own `dialog_from` — a build-time bug in a
        // bundled `.toml`, never end-user content from `./.norte`. Same
        // footing as `WrongLayerKey`: no untrusted payload, full `Display`
        // is fine as-is (K2b Task 2, `just gui-ci` first compiled this
        // arm — Task 1 verified with `just t norte-frontend`/`just c`
        // only, which do not build `norte-gui`).
        KeymapError::WrongLayerKey { .. }
        | KeymapError::DialogFromAndDialog { .. }
        | KeymapError::UnknownDialogFrom { .. }
        | KeymapError::DialogFromChain { .. } => banner_safe(&e.to_string()),
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

/// Lo que la línea de comandos fija para el arranque de la sesión, ya
/// validado: gana al entorno en [`LoadConfig::resolve`].
struct Startup {
    /// Directorio inicial de ambos panes.
    dir: Option<norte_proto::VPath>,
    /// Socket del daemon.
    socket: Option<std::path::PathBuf>,
}

/// Flags con valor de la GUI. Sin `--daemon`: la GUI SIEMPRE habla con el
/// daemon (no tiene modo embebido), así que el flag no significaría nada.
const VALUE_FLAGS: &[&str] = &["--socket"];

/// Texto de `--help`. En inglés y sin Fluent, igual que el del TUI: se
/// imprime antes de negociar el idioma (que sale de la config).
const USAGE: &str = "\
norte-gui — orthodox file manager, graphical frontend

Usage: norte-gui [OPTIONS] [DIR]

Arguments:
  [DIR]  Directory to start in (default: $NORTE_DIR, else the current directory)

Options:
      --socket <PATH>  Daemon socket (default: $NORTE_SOCKET, else the daemon's own)
  -h, --help           Print help
  -V, --version        Print version
";

fn main() {
    // Qué significa `mod+` en ESTE proceso — LA PRIMERA sentencia de `main`,
    // antes incluso de parsear argumentos. El valor solo depende de
    // `cfg!(target_os)`, así que nada obliga a que sea tarde, y tarde es
    // frágil: `mod_key()` es un `OnceLock` con `get_or_init`, de modo que el
    // primer `parse_chord` que corriese antes de esta línea congelaría la
    // política en Ctrl para siempre, sin que ningún test fallase
    // (rust-reviewer MINOR-2 — la garantía era posicional y estaba sostenida
    // por un comentario). La GUI puede OBSERVAR ⌘ (gpui lo reporta en
    // `Modifiers::platform`); la TUI no, así que allí no se llama a esto y el
    // default —Ctrl— es su única respuesta honesta. Un preset con `mod+c` es
    // Cmd+C en la GUI de macOS y Ctrl+C en el resto, siendo UN fichero.
    //
    // El bool se comprueba: es la única señal de que la política ya estaba
    // fijada a otra cosa, y este es el único sitio que la fija.
    // OJO: la llamada va FUERA del `debug_assert!`, que en release no
    // compila su argumento — dentro, `mod+` se quedaría en Ctrl en el
    // binario que se distribuye y en ningún otro.
    let mod_key_fixed = norte_frontend::keymap::set_mod_key(if cfg!(target_os = "macos") {
        norte_frontend::keymap::ModKey::Cmd
    } else {
        norte_frontend::keymap::ModKey::Ctrl
    });
    debug_assert!(
        mod_key_fixed,
        "la política de `mod+` ya estaba fijada a otro valor"
    );

    // Argumentos (mismo parser COMPARTIDO que el TUI,
    // `norte_frontend::cli`): `[DIR]` posicional y `--socket`. `--help`/
    // `--version` salen antes de abrir ventana; un flag desconocido se
    // NOMBRA y aborta, jamás se ignora en silencio.
    let args = norte_frontend::cli::parse(std::env::args_os().skip(1), &[], VALUE_FLAGS);
    if args.help {
        print!("{USAGE}");
        return;
    }
    if args.version {
        println!("norte-gui {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if let Some(flag) = &args.unknown {
        eprintln!("norte-gui: unknown flag `{flag}` — try `norte-gui --help`");
        std::process::exit(2);
    }
    // El DIR de la línea de comandos se valida AQUÍ para fallar con un
    // mensaje en el terminal en vez de con un banner dentro de una ventana
    // ya abierta. Viaja (con el socket) hasta `LoadConfig::resolve`, que es
    // el único sitio que decide entre línea de comandos, entorno y default.
    let dir_cli = args.dir.as_ref().map(|dir| {
        let meta = std::fs::metadata(dir).unwrap_or_else(|e| {
            eprintln!("norte-gui: no se puede abrir {}: {e}", dir.display());
            std::process::exit(2);
        });
        if !meta.is_dir() {
            eprintln!("norte-gui: {} no es un directorio", dir.display());
            std::process::exit(2);
        }
        let absoluto = std::path::absolute(dir).unwrap_or_else(|_| dir.clone());
        norte_vfs_local::vpath_from_native(&absoluto).unwrap_or_else(|e| {
            eprintln!(
                "norte-gui: {} no es representable como VPath: {e}",
                dir.display()
            );
            std::process::exit(2);
        })
    });
    let inicio = Startup {
        dir: dir_cli,
        socket: args.path("--socket"),
    };

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

    // (`set_mod_key` ya corrió: es la primera sentencia de `main`.)

    application().run(move |cx: &mut App| {
        // Fuente mono bundled (GP review, hallazgo CRÍTICO): registrada
        // ANTES de abrir la ventana para que `NorteGui::new` (que resuelve
        // `FontSet` en su primer frame) ya la vea en
        // `cx.text_system().all_font_names()`. Sin esto ".ZedMono" es solo un
        // alias hacia una familia del SISTEMA ("Lilex") que en general no
        // está instalada — GPUI no embebe fuentes propias — y la GUI
        // degradaba en silencio a una fuente proporcional cualquiera para
        // los listados, rompiendo la alineación de columnas. `add_fonts`
        // devuelve `Result`: un fallo (excepcional — los bytes son estáticos
        // y vienen de un TTF válido) se loguea y se sigue: la validación de
        // familia de `NorteGui::new` no encuentra "JetBrains Mono" en el
        // fontdb en ese caso y cae honestamente al alias `.SystemUIFont`
        // (ver `validated_family`), en vez de fingir que el mono bundled
        // existe.
        if let Err(e) = cx.text_system().add_fonts(vec![
            std::borrow::Cow::Borrowed(
                include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf").as_slice(),
            ),
            std::borrow::Cow::Borrowed(
                include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf").as_slice(),
            ),
        ]) {
            eprintln!("[norte-gui] no se pudo registrar la fuente JetBrains Mono empaquetada: {e}");
        }

        let bounds = Bounds::centered(None, size(px(1000.0), px(640.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| NorteGui::new(window, cx, &loaded, &inicio)),
        )
        .expect("no se pudo abrir la ventana GPUI");

        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {

    /// Cada pantalla que esta GUI sabe abrir tiene su página, y `F1` abre ESA.
    ///
    /// La otra mitad de lo que la puerta de documentación cruza en la TUI,
    /// comprobada aquí desde el lado del lector: el `match` sin comodín de
    /// `help_context` impide que un modal nuevo llegue sin que alguien decida
    /// qué lo explica, y esto impide que decida un id que el corpus no tiene.
    #[test]
    fn todo_contexto_de_esta_gui_tiene_pagina() {
        for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
            for context in ["browse", "viewer"] {
                assert!(
                    norte_help::topic_for_context(lang, context).is_some(),
                    "[{lang:?}] el contexto `{context}` no tiene página"
                );
            }
            for context in [
                "dialog.confirm",
                "dialog.collision",
                "dialog.quit",
                "dialog.transfer-name",
                "dialog.ai-rename",
                "dialog.semantic-search",
            ] {
                assert!(
                    norte_help::topic_for_context(lang, context).is_some(),
                    "[{lang:?}] el contexto de modal `{context}` no tiene página"
                );
            }
        }
    }
    use super::MouseValidity;
    use super::effects;
    use super::expire_stale_gesture;
    use super::{
        BANNER_DETAIL_MAX_CHARS, BG, BORDER_FOCUS, BORDER_UNFOCUS, ERR_FG, FG, HEADER_BG, MARK_BG,
        MARK_FG, PANE_BG, PANE_BG_FOCUS, QUICK_FG, SEL_BG,
    };
    use super::{
        ChromeColors, ConfirmQuit, FontSet, ImagePreview, MouseState, affected_dirs,
        apply_viewer_command, banner_safe, chrome_mark_fg, confirm_quit_should_open,
        confirm_quit_task_count, decoration_badge_color, first_cancelable, flash_paints,
        flicker_factor, flicker_scale, generation_is_current, glowed, has_pending_work,
        hydration_batch, image_preview_from, image_status, keymap_error_detail,
        modal_footer_colors, modal_panel_colors, modal_title_colors, motion_active, mouse_motion,
        mouse_press, mouse_release, pane_inner_cells, pending_hint, pending_indicator,
        retain_active, row_label, styled_span_color, task_at_cursor, theme_map,
        unknown_preset_banner, validated_family, viewer_header, viewer_status, which_key_for,
        which_key_paints,
    };
    use super::{empty_frontend_config, rebind_layers, shortcut_write_message};
    // Menú contextual (tarea 4 del plan de ratón).
    use super::{
        ContextMenu, clipboard_text, context_menu, context_target, expire_stale_menu, keymap,
        menu_origin, rename_modal_for,
    };
    // Drag & drop entre panes (tarea 5 del plan de ratón).
    use super::transfer_modal;
    use super::{DropRequest, Modal, ModalOutcome, TransferKind, drop_hint, drop_modal, modal};
    use gpui::rgb;
    use norte_frontend::mouse::{Mods, Pending, Spot};
    use norte_frontend::viewer::Viewer;
    use norte_proto::{EntryKind, Segment, VPath};
    use norte_theme::Theme;

    fn vp() -> VPath {
        VPath::parse("mem:///a.txt").unwrap()
    }

    /// #108 b6: celdas interiores aproximadas del pane — viewport/2 menos el
    /// chrome fijo (bordes + padding) y el canalón de marca, a suelo 0.
    #[test]
    fn pane_inner_cells_aritmetica_y_suelos() {
        // 1280px de ventana, celda de 8.4px, canalón de 14px:
        // (640 − 13 − 14) / 8.4 = 72.97… → 72.
        assert_eq!(pane_inner_cells(1280.0, 8.4, 14.0), 72);
        // Ventana absurda de 10px: jamás pánico, 0 celdas.
        assert_eq!(pane_inner_cells(10.0, 8.4, 14.0), 0);
        // Celda no-positiva (advance imposible): 0, no división por cero.
        assert_eq!(pane_inner_cells(1280.0, 0.0, 14.0), 0);
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

    /// #117 tarea 3: el camino de celdas de la GUI (`row_cell_text`, el
    /// MISMO que pinta `render_row`) con valores attr HOSTILES de un
    /// provider: jamás un char peligroso crudo, lossy MARCADO (U+FFFD)
    /// para Bytes no-UTF8, ausencia = celda en blanco y cabecera con el id
    /// como fallback sin catálogo — las mismas aserciones que el test
    /// gemelo de la TUI (`celdas_attr_hostiles_…` en `ui.rs`), sobre
    /// funciones puras (sin ventana/GPU).
    #[test]
    fn celdas_attr_hostiles_enmascaradas_y_ausencia_en_blanco() {
        use norte_frontend::columns::ColumnId;
        use norte_proto::attrs::AttrValue;
        fn entry(dir: &VPath, name: &str) -> norte_proto::Entry {
            norte_proto::Entry {
                attrs: std::collections::BTreeMap::new(),
                path: dir.join(norte_proto::Segment::new(name.as_bytes().to_vec()).unwrap()),
                kind: EntryKind::File,
                size: Some(1),
                mtime_ms: None,
            }
        }
        // Config: name + attr:mem.owner (Bytes no-UTF8) + attr:mem.note
        // (bidi RTL + ZWJ).
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec![
                "name".into(),
                "attr:mem.owner".into(),
                "attr:mem.note".into(),
            ]),
            ..Default::default()
        };
        let settings = norte_frontend::columns::ColumnsSettings::resolve(&cfg);
        let dir = VPath::parse("mem:///d").unwrap();
        let mut e1 = entry(&dir, "aaa");
        e1.attrs.insert(
            "mem.owner".into(),
            AttrValue::Bytes(b"due\xf1o-\xff\xfe".to_vec()),
        );
        e1.attrs.insert(
            "mem.note".into(),
            AttrValue::Text("\u{202e}at\u{f3}n\u{202c} a\u{200d}b".into()),
        );
        let e2 = entry(&dir, "bbb"); // SIN attrs: celdas en blanco
        let owner = ColumnId::Attr("mem.owner".to_owned());
        let note = ColumnId::Attr("mem.note".to_owned());
        for id in [&owner, &note] {
            let style = settings.style_for_id("mem", id, None);
            // 1. Ninguna celda pintada lleva un char peligroso crudo
            //    (controles, overrides bidi, invisibles — spec §6).
            let celda = super::row_cell_text(&e1, id, 0, &style);
            assert!(
                !celda.chars().any(norte_encoding::is_terminal_hazard),
                "hazard crudo en la celda de {id}: {celda:?}"
            );
            // 3. Sin el attr (e2), la celda es BLANCO (ausencia), jamás un
            //    valor fabricado.
            assert_eq!(
                super::row_cell_text(&e2, id, 0, &style),
                "",
                "ausencia debe ser blanco en {id}"
            );
        }
        // 2. El owner (Bytes no-UTF8) pinta LOSSY y MARCADO (U+FFFD visible).
        let style = settings.style_for_id("mem", &owner, None);
        let celda_owner = super::row_cell_text(&e1, &owner, 0, &style);
        assert!(
            celda_owner.contains('\u{FFFD}'),
            "owner lossy sin marcar: {celda_owner:?}"
        );
        // La note hostil se enmascara pero NO desaparece (presente ≠ blanco).
        let style = settings.style_for_id("mem", &note, None);
        assert!(
            !super::row_cell_text(&e1, &note, 0, &style).is_empty(),
            "un attr presente-pero-hostil jamás pinta blanco"
        );
        // 4. La cabecera cae al id saneado como fallback (sin catálogo aquí).
        let label = norte_frontend::columns::header_label(&owner, &style, None);
        assert!(label.contains("mem.owner"), "cabecera sin id: {label:?}");
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

    /// `modal_lines` sobre TODO el corpus hostil, para TODAS las variantes de
    /// `Modal` con contenido de usuario: ninguna línea del panel deja un
    /// `is_terminal_hazard` crudo (título, item saneado, el `from → to` de un
    /// conflicto, el plan IA — from Y to — o la query pegada del prompt IA) —
    /// mismo patrón que `task_line_nunca_deja_hazards_crudos_del_corpus_
    /// hostil` (review encoding: el modal es la superficie que confirma un
    /// BORRADO — o un plan de renames de un modelo — a ciegas si se pinta
    /// mal).
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
            // M4-IA: el wire garantiza UTF-8 (String) — la vista lossy del
            // fixture es exactamente lo que un daemon hostil podría colar.
            let hostile_name = String::from_utf8_lossy(&fixture.bytes).into_owned();

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
                // Plan IA: hostil en el dir objetivo Y en AMBAS posiciones
                // de la pareja (un `to` hostil renombraría A un hazard).
                Modal::AiRenamePlan {
                    dir: item.clone(),
                    entries: vec![
                        norte_proto::methods::AiRenameEntry {
                            from: hostile_name.clone(),
                            to: "limpio.txt".into(),
                        },
                        norte_proto::methods::AiRenameEntry {
                            from: "limpio.txt".into(),
                            to: hostile_name.clone(),
                        },
                    ],
                    offset: 0,
                    plan: norte_frontend::BatchPlan::Pending,
                },
                // Prompt IA con la query PEGADA en bytes crudos (el buffer
                // admite cualquier cosa que entre por un paste).
                Modal::AiRenamePrompt {
                    dir: to.clone(),
                    query: fixture.bytes.clone(),
                },
                // Rename: hostil en el nombre ACTUAL (sale del disco) y en
                // el editable (entra por paste), en el mismo modal — y con
                // un diagnóstico pendiente, que es una línea más que pintar.
                Modal::RenamePrompt {
                    from: item.clone(),
                    to_dir: VPath::parse("mem:///").unwrap(),
                    name: fixture.bytes.clone(),
                    error: Some(norte_i18n::t("msg-transfer-name-same")),
                },
                // M4-IA-2: prompt semántico con la misma query pegada…
                Modal::SemanticQuery {
                    query: fixture.bytes.clone(),
                },
                // …y hits con el path hostil (visible Y bajo el cursor).
                Modal::SemanticHits {
                    hits: vec![norte_proto::methods::SemanticHit {
                        path: item.clone(),
                        score: 0.87,
                    }],
                    offset: 0,
                    cursor: 0,
                },
                // 2026-08-10-volumes.md task V4: hostil en el mount (path) Y
                // en el label (bytes crudos del wire, V3.5) del MISMO
                // volumen, bajo el cursor — encoding-auditor V3 review
                // exigió que las tres columnas se enmascaren, no solo el
                // path.
                Modal::Volumes {
                    pane: 0,
                    include_pseudo: false,
                    volumes: vec![norte_proto::methods::Volume {
                        mount: item.clone(),
                        label: Some(fixture.bytes.clone()),
                        fs_type: hostile_name.clone(),
                        kind: norte_proto::methods::VolumeKind::Fixed,
                        total_bytes: Some(1_000_000),
                        free_bytes: Some(500_000),
                        read_only: false,
                    }],
                    offset: 0,
                    cursor: 0,
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

    /// El peor modal que esta GUI puede pintar CABE en su panel.
    ///
    /// El panel es `overflow_hidden` y no tiene scroll: lo que no cabe se
    /// pierde SIN marca. El plan IA con el veredicto de su lote (§17) es el
    /// más alto que hay, así que su peor caso es el presupuesto —y este test
    /// es lo que impide que una línea nueva lo desborde en silencio.
    ///
    /// (Mutación de control: subir `RENAME_COLLISION_LIMIT` o meter otra
    /// línea en el cuerpo sin subir `MODAL_MAX_H` rompe este test.)
    #[test]
    fn el_peor_modal_del_plan_cabe_en_el_panel() {
        use super::{MODAL_MAX_H, MODAL_ROW_H, Modal};
        use norte_proto::methods::{RenameCollision, RenameCollisionKind, RenameStep};
        let _ = norte_i18n::force(norte_i18n::Lang::Es);
        let entries: Vec<_> = (0..norte_frontend::MAX_AI_PLAN_ENTRIES)
            .map(|i| norte_proto::methods::AiRenameEntry {
                from: format!("f{i}.txt"),
                to: format!("t{i}.txt"),
            })
            .collect();
        let collisions: Vec<_> = (0..entries.len())
            .map(|i| RenameCollision {
                pair_index: u32::try_from(i).unwrap(),
                name: norte_proto::Segment::new(format!("t{i}.txt").into_bytes()).unwrap(),
                kind: RenameCollisionKind::Unknown,
            })
            .collect();
        let m = Modal::AiRenamePlan {
            dir: VPath::parse("mem:///docs").unwrap(),
            entries,
            offset: 0,
            plan: norte_frontend::BatchPlan::Ready(Box::new(
                norte_proto::methods::FsRenameBatchPlanResult {
                    // Un temporal ADEMÁS de las colisiones: el peor caso de
                    // este test no tiene por qué ser alcanzable por el core,
                    // solo tiene que acotarlo.
                    steps: vec![RenameStep {
                        from: norte_proto::Segment::new(b"a".to_vec()).unwrap(),
                        to: norte_proto::Segment::new(b".norte-rename-0".to_vec()).unwrap(),
                        temp: true,
                    }],
                    collisions,
                    executable: false,
                    plan_hash: norte_proto::methods::PlanHash::parse(&"0".repeat(64)).unwrap(),
                },
            )),
        };
        // Cuerpo + la fila del pie, que es hija del MISMO panel.
        let filas = super::modal_lines(&m).len() + 1;
        let alto = filas as f32 * MODAL_ROW_H;
        assert!(
            alto <= MODAL_MAX_H,
            "{filas} filas × {MODAL_ROW_H}px = {alto}px > {MODAL_MAX_H}px: el panel \
             recortaría el modal SIN marca",
        );
    }

    /// §17 + doctrina `arrow_join_spoof` en la superficie ACCESIBLE: la
    /// descripción del diálogo une líneas con `\n`, que `display_name`
    /// enmascara y por tanto ningún nombre puede contener. Un nombre con
    /// `"; "` —texto perfectamente legal— no puede dictarle a un lector de
    /// pantalla un veredicto que nadie emitió.
    ///
    /// (Mutación de control: volver a `join("; ")` rompe este test.)
    #[test]
    fn la_descripcion_accesible_no_deja_fabricar_una_linea() {
        use super::Modal;
        use norte_proto::methods::{RenameCollision, RenameCollisionKind};
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let señuelo = "x; batch: applicable";
        let m = Modal::AiRenamePlan {
            dir: VPath::parse("mem:///docs").unwrap(),
            entries: vec![norte_proto::methods::AiRenameEntry {
                from: señuelo.into(),
                to: "b.txt".into(),
            }],
            offset: 0,
            plan: norte_frontend::BatchPlan::Ready(Box::new(
                norte_proto::methods::FsRenameBatchPlanResult {
                    steps: Vec::new(),
                    collisions: vec![RenameCollision {
                        pair_index: 0,
                        name: norte_proto::Segment::new(señuelo.as_bytes().to_vec()).unwrap(),
                        kind: RenameCollisionKind::External,
                    }],
                    executable: false,
                    plan_hash: norte_proto::methods::PlanHash::parse(&"0".repeat(64)).unwrap(),
                },
            )),
        };
        let lines = super::modal_lines(&m);
        let descripcion = super::modal_a11y_description(&lines);
        assert_eq!(
            descripcion.lines().count(),
            lines.len() - 1,
            "un nombre no puede añadir ni quitar una línea: {descripcion:?}",
        );
        // Y el separador que el señuelo imita NO es el que se usa.
        assert!(
            descripcion.contains("; "),
            "el señuelo llega entero (enmascarado), pero como TEXTO: {descripcion:?}",
        );
    }

    /// §17: una colisión es VISIBLE con su veredicto y su índice de pareja,
    /// y el modal dice que el lote NO se puede aplicar (paridad TUI
    /// `una_colision_se_pinta_y_el_plan_se_marca_inaplicable`).
    #[test]
    fn ai_plan_modal_pinta_la_colision_y_marca_el_lote_inaplicable() {
        use super::Modal;
        use norte_proto::methods::{RenameCollision, RenameCollisionKind};
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let m = Modal::AiRenamePlan {
            dir: VPath::parse("mem:///docs").unwrap(),
            entries: vec![norte_proto::methods::AiRenameEntry {
                from: "a.txt".into(),
                to: "z.txt".into(),
            }],
            offset: 0,
            plan: norte_frontend::BatchPlan::Ready(Box::new(
                norte_proto::methods::FsRenameBatchPlanResult {
                    steps: Vec::new(),
                    collisions: vec![RenameCollision {
                        pair_index: 0,
                        name: norte_proto::Segment::new(b"z.txt".to_vec()).unwrap(),
                        kind: RenameCollisionKind::External,
                    }],
                    executable: false,
                    plan_hash: norte_proto::methods::PlanHash::parse(&"0".repeat(64)).unwrap(),
                },
            )),
        };
        let lines = super::modal_lines(&m);
        // título + dir + estado + pareja × 2 + colisión = 6.
        assert_eq!(lines.len(), 6, "{lines:?}");
        assert_eq!(
            lines[2],
            norte_i18n::t("modal-rename-batch-not-applicable"),
            "{lines:?}"
        );
        assert!(
            lines[5].contains(&norte_i18n::t("modal-rename-batch-collision-external")),
            "el veredicto se enseña: {lines:?}"
        );
        assert!(lines[5].contains("z.txt"), "y el nombre ofensor: {lines:?}");
        // `pair_index` 0 se pinta 1-based, como la etiqueta del `from`.
        assert!(lines[5].contains("1."), "{lines:?}");
    }

    /// §17: un veredicto de un daemon MÁS NUEVO degrada UNA línea a una
    /// etiqueta genérica, jamás el modal entero; y un paso temporal se
    /// CUENTA, jamás se nombra (un `.norte-rename-…` entre las parejas haría
    /// creer que norte va a dejar ese nombre en el disco).
    #[test]
    fn ai_plan_modal_degrada_el_veredicto_desconocido_y_no_nombra_temporales() {
        use super::Modal;
        use norte_proto::Segment;
        use norte_proto::methods::{RenameCollision, RenameCollisionKind, RenameStep};
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        // El fallback `serde(other)` al que deserializa una clase de un
        // protocolo más nuevo (el round-trip por JSON se pinea en el proto y
        // en la TUI; aquí solo interesa cómo se PINTA).
        let futuro = RenameCollisionKind::Unknown;
        let entries = vec![
            norte_proto::methods::AiRenameEntry {
                from: "a".into(),
                to: "b".into(),
            },
            norte_proto::methods::AiRenameEntry {
                from: "b".into(),
                to: "a".into(),
            },
        ];
        let seg = |b: &[u8]| Segment::new(b.to_vec()).unwrap();

        // Ciclo aplicable: DOS pasos temporales, ningún nombre a la vista.
        let m = Modal::AiRenamePlan {
            dir: VPath::parse("mem:///docs").unwrap(),
            entries: entries.clone(),
            offset: 0,
            plan: norte_frontend::BatchPlan::Ready(Box::new(
                norte_proto::methods::FsRenameBatchPlanResult {
                    steps: vec![
                        RenameStep {
                            from: seg(b"a"),
                            to: seg(b".norte-rename-0a1b2c3d-0"),
                            temp: true,
                        },
                        RenameStep {
                            from: seg(b"b"),
                            to: seg(b"a"),
                            temp: false,
                        },
                        RenameStep {
                            from: seg(b".norte-rename-0a1b2c3d-0"),
                            to: seg(b"b"),
                            temp: true,
                        },
                    ],
                    collisions: Vec::new(),
                    executable: true,
                    plan_hash: norte_proto::methods::PlanHash::parse(&"0".repeat(64)).unwrap(),
                },
            )),
        };
        let lines = super::modal_lines(&m);
        assert!(
            !lines.iter().any(|l| l.contains(".norte-rename-")),
            "un temporal jamás se pinta como propuesta: {lines:?}"
        );
        assert!(
            lines.contains(&norte_i18n::ta("modal-rename-batch-temp", &[("n", "2")])),
            "{lines:?}"
        );
        assert!(
            lines.contains(&norte_i18n::t("modal-rename-batch-applicable")),
            "el rodeo no es una colisión: {lines:?}"
        );

        // Veredicto del futuro: UNA línea genérica, el modal sigue entero.
        let m = Modal::AiRenamePlan {
            dir: VPath::parse("mem:///docs").unwrap(),
            entries,
            offset: 0,
            plan: norte_frontend::BatchPlan::Ready(Box::new(
                norte_proto::methods::FsRenameBatchPlanResult {
                    steps: Vec::new(),
                    collisions: vec![RenameCollision {
                        pair_index: 1,
                        name: seg(b"a"),
                        kind: futuro,
                    }],
                    executable: false,
                    plan_hash: norte_proto::methods::PlanHash::parse(&"0".repeat(64)).unwrap(),
                },
            )),
        };
        let lines = super::modal_lines(&m);
        // título + dir + 2 parejas × 2 + estado + colisión = 8.
        assert_eq!(lines.len(), 8, "el modal sigue entero: {lines:?}");
        assert!(
            lines[7].contains(&norte_i18n::t("modal-rename-batch-collision-unknown")),
            "{lines:?}"
        );
        assert!(
            lines[7].contains("2."),
            "señala la fila culpable: {lines:?}"
        );
    }

    /// §17: el nombre ofensor de una colisión pasa por el MISMO saneado que
    /// el resto del modal — barrido del corpus canónico: ningún hazard
    /// sobrevive, el enmascarado MARCA la línea, y el veredicto (lo
    /// accionable) jamás se lo come el nombre.
    #[test]
    fn colision_hostil_se_enmascara_marca_y_no_desplaza_el_veredicto() {
        use super::Modal;
        use norte_proto::methods::{RenameCollision, RenameCollisionKind};
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let verdicto = norte_i18n::t("modal-rename-batch-collision-internal");
        for fixture in norte_testkit::corpus::hostile_names() {
            let m = Modal::AiRenamePlan {
                dir: VPath::parse("mem:///docs").unwrap(),
                entries: vec![norte_proto::methods::AiRenameEntry {
                    from: "a".into(),
                    to: "b".into(),
                }],
                offset: 0,
                plan: norte_frontend::BatchPlan::Ready(Box::new(
                    norte_proto::methods::FsRenameBatchPlanResult {
                        steps: Vec::new(),
                        collisions: vec![RenameCollision {
                            pair_index: 0,
                            name: norte_proto::Segment::new(fixture.bytes.clone()).unwrap(),
                            kind: RenameCollisionKind::Internal,
                        }],
                        executable: false,
                        plan_hash: norte_proto::methods::PlanHash::parse(&"0".repeat(64)).unwrap(),
                    },
                )),
            };
            let lines = super::modal_lines(&m);
            assert!(
                !lines
                    .iter()
                    .any(|l| l.chars().any(norte_encoding::is_terminal_hazard)),
                "corpus {}: un hazard sobrevivió al render: {lines:?}",
                fixture.id
            );
            // UNA línea por colisión: un nombre no puede fabricar otra.
            assert_eq!(lines.len(), 6, "corpus {}: {lines:?}", fixture.id);
            assert!(
                lines[5].contains(&verdicto),
                "corpus {}: el veredicto sobrevive al nombre: {lines:?}",
                fixture.id
            );
            if norte_frontend::display_name(&fixture.bytes).1 {
                assert!(
                    lines[5].starts_with(super::HOSTILE_BADGE),
                    "corpus {}: enmascarado SIN badge: {lines:?}",
                    fixture.id
                );
            }
        }
    }

    /// M4-IA (paridad TUI audit MAJOR-3/MINOR-5): la ventana del plan pinta
    /// [`crate::modal::AI_RENAME_PAIR_LIMIT`] parejas desde `offset`, el
    /// indicador de desbordamiento dice `shown/total` y lleva el badge si
    /// alguna pareja OCULTA es hostil — lo escondido jamás se cuela limpio.
    #[test]
    fn ai_plan_modal_ventana_overflow_y_badge_de_ocultas() {
        use super::Modal;
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let limit = crate::modal::AI_RENAME_PAIR_LIMIT;
        let mut entries: Vec<norte_proto::methods::AiRenameEntry> = (1..=6)
            .map(|i| norte_proto::methods::AiRenameEntry {
                from: format!("f{i}.txt"),
                to: format!("t{i}.txt"),
            })
            .collect();
        // La 6.ª (OCULTA con offset 0) es hostil (bidi RLO).
        entries[5].from = "\u{202E}evil.txt".into();
        let m = Modal::AiRenamePlan {
            dir: VPath::parse("mem:///docs").unwrap(),
            entries: entries.clone(),
            offset: 0,
            plan: norte_frontend::BatchPlan::Pending,
        };
        let lines = super::modal_lines(&m);
        // título + dir + estado del lote (§17: sin plan todavía,
        // «comprobando…») + 5 parejas × 2 líneas + desbordamiento.
        assert_eq!(lines.len(), 3 + limit * 2 + 1, "{lines:?}");
        assert!(
            lines[3].contains("1.") && lines[3].contains("f1.txt"),
            "from numerado fuera de banda: {:?}",
            lines[3]
        );
        assert!(
            lines[4].starts_with('→') && lines[4].contains("t1.txt"),
            "flecha al INICIO de la línea del to: {:?}",
            lines[4]
        );
        let more = lines.last().unwrap();
        assert!(
            more.starts_with(super::HOSTILE_BADGE),
            "pareja oculta hostil ⇒ badge en el desbordamiento: {more:?}"
        );
        assert!(
            more.contains("5/6"),
            "el desbordamiento dice shown/total: {more:?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("evil")),
            "la pareja oculta NO se pinta con offset 0: {lines:?}"
        );

        // Con offset 1 la hostil ENTRA en la ventana (ya no está oculta):
        // su línea lleva el badge y el desbordamiento ya no.
        let m = Modal::AiRenamePlan {
            dir: VPath::parse("mem:///docs").unwrap(),
            entries,
            offset: 1,
            plan: norte_frontend::BatchPlan::Pending,
        };
        let lines = super::modal_lines(&m);
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with(super::HOSTILE_BADGE) && l.contains("6.")),
            "la pareja hostil visible lleva SU badge: {lines:?}"
        );
        let more = lines.last().unwrap();
        assert!(
            more.contains("6/6") && !more.starts_with(super::HOSTILE_BADGE),
            "sin ocultas hostiles el desbordamiento va limpio: {more:?}"
        );
    }

    /// M4-IA-2 (molde `ai_plan_modal_ventana_overflow_y_badge_de_ocultas`):
    /// la ventana de hits pinta [`crate::modal::SEMANTIC_HIT_LIMIT`] hits
    /// desde `offset` con marcador de cursor `>` FUERA de banda y score
    /// `{:.2}`, el indicador de desbordamiento dice `shown/total` y lleva el
    /// badge si algún hit OCULTO es hostil — lo escondido jamás se cuela
    /// limpio.
    #[test]
    fn semantic_hits_modal_ventana_cursor_overflow_y_badge_de_ocultos() {
        use super::Modal;
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let limit = crate::modal::SEMANTIC_HIT_LIMIT;
        let total = limit + 2;
        let mut hits: Vec<norte_proto::methods::SemanticHit> = (1..=total)
            .map(|i| norte_proto::methods::SemanticHit {
                path: VPath::parse(&format!("mem:///docs/f{i}.txt")).unwrap(),
                score: 0.5,
            })
            .collect();
        // El último (OCULTO con offset 0) es hostil (bidi RLO).
        hits[total - 1].path = VPath::parse("mem:///docs")
            .unwrap()
            .join(norte_proto::Segment::new("\u{202E}evil.txt".as_bytes().to_vec()).unwrap());
        let m = Modal::SemanticHits {
            hits: hits.clone(),
            offset: 0,
            cursor: 1,
        };
        let lines = super::modal_lines(&m);
        // título + `limit` hits + desbordamiento.
        assert_eq!(lines.len(), 1 + limit + 1, "{lines:?}");
        assert!(
            lines[1].starts_with("  ") && lines[1].contains("1.") && lines[1].contains("f1.txt"),
            "hit sin cursor: etiqueta numerada absoluta fuera de banda: {:?}",
            lines[1]
        );
        assert!(
            lines[2].starts_with("> ") && lines[2].contains("2."),
            "el hit bajo el cursor lleva el marcador `>` en columna fija: {:?}",
            lines[2]
        );
        assert!(
            lines[1].contains("0.50"),
            "el score va con dos decimales: {:?}",
            lines[1]
        );
        let more = lines.last().unwrap();
        assert!(
            more.starts_with(super::HOSTILE_BADGE),
            "hit oculto hostil ⇒ badge en el desbordamiento: {more:?}"
        );
        assert!(
            more.contains(&format!("{limit}/{total}")),
            "el desbordamiento dice shown/total: {more:?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains('\u{202E}')),
            "el bidi crudo del hit oculto no se cuela en NINGUNA línea: {lines:?}"
        );

        // Con offset 2 el hostil ENTRA en la ventana (ya no está oculto):
        // su línea lleva el badge tras el marcador y el desbordamiento no.
        let m = Modal::SemanticHits {
            hits,
            offset: 2,
            cursor: total - 1,
        };
        let lines = super::modal_lines(&m);
        assert!(
            lines.iter().any(|l| {
                l.starts_with(&format!("> {}", super::HOSTILE_BADGE)) && l.contains("12.")
            }),
            "el hit hostil visible lleva SU badge tras el marcador: {lines:?}"
        );
        let more = lines.last().unwrap();
        assert!(
            more.contains(&format!("{total}/{total}")) && !more.starts_with(super::HOSTILE_BADGE),
            "sin ocultos hostiles el desbordamiento va limpio: {more:?}"
        );
    }

    /// Encoding audit M4-IA-2 H1/S1 (espejo del pin de la TUI
    /// `score_spoof_inband_jamas_desplaza_al_score_real`): la GUI NO puede
    /// delegar el recorte del path al `.truncate()` del div. El score va el
    /// ÚLTIMO campo de `modal-semantic-hit`, así que un path kilométrico lo
    /// empujaba fuera de la caja; si además el path lleva incrustado el
    /// fixture `score_spoof_inband` (`informe · 0.99.txt`: middle dot +
    /// decimales IMPRIMIBLES — nada enmascarable, NI SIQUIERA hay badge), el
    /// único texto con pinta de score que quedaba visible era el falso.
    /// `modal_lines` acota el path con `middle_ellipsis` ANTES de
    /// interpolarlo: el score REAL (`0.91`, distinguible del señuelo `0.99`)
    /// sigue presente y final, y el recorte del path va MARCADO con `…`.
    #[test]
    fn semantic_hit_largo_con_score_spoof_no_expulsa_el_score_real() {
        use super::Modal;
        use norte_proto::Segment;
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let fixture = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "score_spoof_inband")
            .expect("fixture del corpus");
        let señuelo = String::from_utf8(fixture.bytes.clone()).expect("el fixture es UTF-8");
        let dir = VPath::parse("mem:///docs").unwrap();

        // 1) El fixture tal cual: cabe entero, el score REAL cierra la línea.
        let m = Modal::SemanticHits {
            hits: vec![norte_proto::methods::SemanticHit {
                path: dir
                    .clone()
                    .join(Segment::new(fixture.bytes.clone()).unwrap()),
                score: 0.91,
            }],
            offset: 0,
            cursor: 0,
        };
        let lines = super::modal_lines(&m);
        let hit = &lines[1];
        assert!(
            hit.contains(&señuelo),
            "el señuelo se pinta tal cual (es un nombre legítimo): {hit:?}"
        );
        assert!(
            hit.trim_end().ends_with("0.91"),
            "el score REAL es el campo FINAL: {hit:?}"
        );

        // 2) Kilométrico (>120 chars) con el señuelo al final: el recorte se
        // come el PATH (elipsis media, marcada), JAMÁS el score.
        let mut largo = b"x".repeat(120);
        largo.extend_from_slice(&fixture.bytes);
        let m = Modal::SemanticHits {
            hits: vec![norte_proto::methods::SemanticHit {
                path: dir.join(Segment::new(largo).unwrap()),
                score: 0.91,
            }],
            offset: 0,
            cursor: 0,
        };
        let lines = super::modal_lines(&m);
        let hit = &lines[1];
        assert!(
            hit.trim_end().ends_with("0.91"),
            "path kilométrico: el score REAL sigue siendo el campo FINAL: {hit:?}"
        );
        assert!(
            hit.contains('…'),
            "el recorte del path se MARCA (spec §6): {hit:?}"
        );
        assert!(
            hit.chars().count() < 120,
            "el path se acotó ANTES de interpolar, no se dejó al div: {hit:?}"
        );
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
            false,
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

    /// A pane on `mem:///` listing `names`, all files (helper of the
    /// `apply_landed_listing` tests, #103).
    fn pane_with(names: &[&str]) -> super::PaneState {
        super::PaneState::new(VPath::parse("mem:///").unwrap(), entries_named(names))
    }

    /// `names` as `mem:///{name}` file entries (#103).
    fn entries_named(names: &[&str]) -> Vec<norte_proto::Entry> {
        names
            .iter()
            .map(|n| norte_proto::Entry {
                attrs: std::collections::BTreeMap::new(),
                path: VPath::parse(&format!("mem:///{n}")).unwrap(),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            })
            .collect()
    }

    /// #103: el read-after-write de la GUI (`relist_dirs` → list → este
    /// aplicador) sobre el MISMO dir es un REFRESCO, no un `cd` — conserva las
    /// marcas. Antes iba por `cd` (`begin_loading` + `set_listing`) y cada
    /// copy/move/delete borraba la selección, al revés que la TUI.
    #[test]
    fn a_post_operation_relist_of_the_same_dir_keeps_the_marks() {
        let mut pane = pane_with(&["a", "b"]);
        pane.mark_all();
        assert_eq!(pane.marks_len(), 2);
        let refilled = super::apply_landed_listing(
            &mut pane,
            VPath::parse("mem:///").unwrap(),
            entries_named(&["a", "b"]),
            true,
        );
        assert!(refilled, "el mismo dir relistado debe ir por refill");
        assert_eq!(
            pane.marks_len(),
            2,
            "un refresco del mismo dir conserva las marcas"
        );
    }

    /// #103, el caso negativo: un listado de OTRO dir sigue siendo un `cd` y
    /// LIMPIA las marcas, aunque el flag de refresco venga puesto. Sin este
    /// test, un "siempre refill" pasaría el test de arriba.
    #[test]
    fn a_listing_for_a_different_dir_still_clears_the_marks() {
        let mut pane = pane_with(&["a", "b"]);
        pane.mark_all();
        let refilled = super::apply_landed_listing(
            &mut pane,
            VPath::parse("mem:///otro").unwrap(),
            entries_named(&["a", "b"]),
            true,
        );
        assert!(!refilled, "otro dir jamás va por refill");
        assert_eq!(pane.marks_len(), 0, "un cd limpia las marcas por diseño");
        assert_eq!(pane.dir(), &VPath::parse("mem:///otro").unwrap());
    }

    /// #103: un `cd` al MISMO dir (F5 sobre el propio directorio, o un
    /// `nav.enter` que vuelve donde ya estabas) NO es un refresco — sin
    /// `refresh` el aplicador toma el camino del `cd` y limpia.
    #[test]
    fn a_cd_to_the_same_dir_is_not_a_refresh() {
        let mut pane = pane_with(&["a", "b"]);
        pane.mark_all();
        let refilled = super::apply_landed_listing(
            &mut pane,
            VPath::parse("mem:///").unwrap(),
            entries_named(&["a", "b"]),
            false,
        );
        assert!(!refilled);
        assert_eq!(pane.marks_len(), 0);
    }

    /// #103 + regla 1: la identidad del dir son los BYTES. Dos nombres que se
    /// pintan igual (aquí `dir` ASCII contra su gemelo con un espacio de ancho
    /// cero) NO son el mismo dir, así que el listado del gemelo es un `cd` y
    /// limpia las marcas — jamás se conservan cruzando ese límite.
    #[test]
    fn the_same_dir_check_is_byte_exact_not_a_lookalike() {
        let root = VPath::parse("mem:///").unwrap();
        let dir = root
            .clone()
            .join(norte_proto::Segment::new(b"dir".to_vec()).unwrap());
        // `dir` + U+200B ZERO WIDTH SPACE: se pinta igual, otros bytes.
        let twin = root.join(norte_proto::Segment::new("dir\u{200B}".as_bytes().to_vec()).unwrap());
        assert_ne!(twin, dir);
        let mut pane = super::PaneState::new(dir, entries_named(&["a", "b"]));
        pane.mark_all();
        let refilled = super::apply_landed_listing(&mut pane, twin, entries_named(&["a"]), true);
        assert!(!refilled, "un gemelo visual no es el mismo dir");
        assert_eq!(pane.marks_len(), 0);
    }

    /// #103: el refresco PODA las marcas cuyas entradas desaparecieron (un
    /// delete de lo marcado) y lo REPORTA por `pruned_marks` — una marca es una
    /// afirmación sobre algo que EXISTE.
    #[test]
    fn a_refresh_prunes_the_marks_whose_entries_are_gone() {
        let mut pane = pane_with(&["a", "b"]);
        pane.mark_all();
        let refilled = super::apply_landed_listing(
            &mut pane,
            VPath::parse("mem:///").unwrap(),
            entries_named(&["a"]),
            true,
        );
        assert!(refilled);
        assert_eq!(pane.marks_len(), 1);
        assert_eq!(pane.pruned_marks(), 1);
    }

    /// Los tests de `marks_status_segments` afirman literales INGLESES, así
    /// que fijan el idioma en vez de heredar el del entorno (`norte_i18n`
    /// resuelve por locale: en una máquina en español leerían el `es.ftl`).
    /// `force` es de una sola vez por proceso — con nextest cada test corre
    /// en el suyo, así que no compite con los tests que fijan `Es`.
    fn en() {
        let _ = norte_i18n::force(norte_i18n::Lang::En);
    }

    /// #103: quien no marca nada no gana chrome nuevo. Sin marcas y sin poda
    /// la barra del pane no dice NADA de marcas.
    #[test]
    fn no_marks_and_no_prune_render_nothing() {
        en();
        assert_eq!(super::marks_status_segments(0, 0, 0, 0), (None, None));
    }

    /// #103: con marcas, cuántas son y cuánto pesan (`human_bytes`
    /// compartido, jamás un formateador propio de la GUI).
    #[test]
    fn marks_render_the_count_and_the_byte_total() {
        en();
        let (marked, pruned) = super::marks_status_segments(3, 1536, 0, 0);
        assert_eq!(marked.as_deref(), Some("3 marked, 1.5 KiB"));
        assert_eq!(pruned, None);
    }

    /// #103: `marked_bytes` cuenta SOLO no-directorios a propósito (nada
    /// recorre el árbol), así que un directorio marcado se nombra aparte —
    /// pintar «2 marked, 10 B» con un dir dentro insinuaría un total que
    /// nadie calculó.
    #[test]
    fn a_marked_directory_is_named_separately() {
        en();
        let (marked, pruned) = super::marks_status_segments(2, 10, 1, 0);
        assert_eq!(marked.as_deref(), Some("2 marked, 10 B + 1 dirs"));
        assert_eq!(pruned, None);
        // Y NO degrada al texto sin dirs: ese sería justamente el total
        // mentiroso.
        assert_ne!(marked.as_deref(), Some("2 marked, 10 B"));
    }

    /// #103: una poda JAMÁS es silenciosa. Con la selección vacía
    /// `marked_paths` cae al cursor, así que callar la poda redirigiría la
    /// siguiente op en masa a algo que nadie marcó.
    #[test]
    fn a_prune_is_never_silent() {
        en();
        let (marked, pruned) = super::marks_status_segments(0, 0, 0, 2);
        assert_eq!(marked, None);
        assert_eq!(
            pruned.as_deref(),
            Some("2 marks dropped, their entries are gone")
        );
    }

    /// #103: una poda PARCIAL deja marcas vivas — se reportan las dos cosas,
    /// el recuento y el aviso.
    #[test]
    fn marks_and_a_prune_are_reported_together() {
        en();
        let (marked, pruned) = super::marks_status_segments(1, 10, 0, 1);
        assert_eq!(marked.as_deref(), Some("1 marked, 10 B"));
        assert_eq!(
            pruned.as_deref(),
            Some("1 marks dropped, their entries are gone")
        );
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

    /// S2 (`[ui] confirm_quit`): las tres combinaciones modo × pendiente,
    /// cada una por separado — mismo estilo que
    /// `has_pending_work_tasks_o_marcas_o_ninguno` de arriba. `Never` no pasa
    /// por esta función en el camino real (`quit_or_confirm` corta antes),
    /// pero se pinza igual (`false` incondicional, documentado en su doc).
    #[test]
    fn confirm_quit_should_open_los_tres_modos() {
        assert!(
            !confirm_quit_should_open(ConfirmQuit::Never, true),
            "never nunca abre, ni con trabajo pendiente"
        );
        assert!(
            !confirm_quit_should_open(ConfirmQuit::Never, false),
            "never nunca abre"
        );
        assert!(
            confirm_quit_should_open(ConfirmQuit::Always, false),
            "always SIEMPRE abre, incluso sin nada pendiente"
        );
        assert!(
            confirm_quit_should_open(ConfirmQuit::Always, true),
            "always SIEMPRE abre"
        );
        assert!(
            !confirm_quit_should_open(ConfirmQuit::Auto, false),
            "auto sin pendiente: cierra directo"
        );
        assert!(
            confirm_quit_should_open(ConfirmQuit::Auto, true),
            "auto con pendiente: abre (comportamiento pre-S2)"
        );
    }

    /// Revisión C2/G0 IMPORTANT 2: los tres presets de fábrica NO avisan;
    /// uno inventado sí, con su propio nombre y la lista de disponibles en
    /// el mensaje (accionable, no un aviso mudo).
    #[test]
    fn preset_desconocido_avisa() {
        for &name in keymap::KNOWN_PRESETS {
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

    /// K2a: las DOS reglas de carga nuevas entran por el mismo saneado que
    /// `AmbiguousPrefix` — dos fragmentos de usuario, enmascarados y topados,
    /// sin la prosa castellana del `Display`. `reserved_for` de `SacredKey` es
    /// un literal del motor y NO se pinta: no aporta y no está saneado como
    /// contenido de usuario porque no lo es.
    #[test]
    fn keymap_error_detail_sanea_las_dos_reglas_de_carga_de_k2a() {
        use norte_frontend::keymap::KeymapError;

        let detail = keymap_error_detail(&KeymapError::DigitBoundWithCounts {
            chord: "5".to_owned(),
            run: "cursor.down".to_owned(),
        });
        assert_eq!(detail, "5 / cursor.down");

        let detail = keymap_error_detail(&KeymapError::SacredKey {
            chord: "tab".to_owned(),
            reserved_for: "pane.switch",
            run: "cursor.down".to_owned(),
        });
        assert_eq!(detail, "tab / cursor.down");

        // Un `run` hostil de un `./.norte/keymap.toml` ajeno: enmascarado,
        // jamás crudo en el banner (mismo contrato que `banner_safe` ya tiene
        // para el resto de las variantes).
        let detail = keymap_error_detail(&KeymapError::SacredKey {
            chord: "tab".to_owned(),
            reserved_for: "pane.switch",
            run: "cursor\u{202e}down".to_owned(),
        });
        assert!(
            !detail.contains('\u{202e}'),
            "override bidi crudo en el banner: {detail:?}"
        );
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

    /// K2a: el pie pinta el contador a medio teclear, solo o junto a la
    /// secuencia — un contador que no se ve es un contador que no se puede
    /// cancelar. Sin nada de lo uno ni de lo otro, `None`: el pie calla (el
    /// comportamiento de antes de K2a, que el `!pending.is_empty()` de
    /// `render` daba por sentado).
    #[test]
    fn pending_indicator_pinta_contador_secuencia_o_calla() {
        use norte_frontend::keymap::{Chord, KeyCode, Mods};
        let g = Chord::new(Mods::default(), KeyCode::Char('g'));
        assert_eq!(pending_indicator(None, &[]), None, "nada → el pie calla");
        assert_eq!(pending_indicator(None, &[g]), Some("g …".to_owned()));
        assert_eq!(
            pending_indicator(Some(12), &[]),
            Some("12 …".to_owned()),
            "el contador solo YA se ve"
        );
        assert_eq!(
            pending_indicator(Some(12), &[g]),
            Some("12 g …".to_owned()),
            "el contador SOBREVIVE a la secuencia a medias"
        );
    }

    /// Encoding audit H1: un chord hostil (ligado desde un `keymap.toml` de
    /// usuario/proyecto) resuelto por el resolver ACTIVO llega crudo a
    /// `pending_hint` — se pinta al pie de la ventana (#91). Mismo defecto
    /// que la TUI (`hints::dialog_hints`/`palette::first_chord`): el chord
    /// se enmascara aquí, no en el motor.
    #[test]
    fn pending_hint_enmascara_chords_hostiles() {
        use norte_frontend::keymap::{Chord, KeyCode, Mods};
        for hazard in norte_testkit::corpus::hostile_chords() {
            let c = Chord::new(Mods::default(), KeyCode::Char(hazard.token));
            let hint = pending_hint(&[c]);
            assert!(
                hint.contains('\u{FFFD}'),
                "[{}] debe enmascararse a U+FFFD: {hint:?}",
                hazard.id
            );
            assert!(
                !hint.contains(hazard.token),
                "[{}] el chord crudo no debe sobrevivir: {hint:?}",
                hazard.id
            );
        }
    }

    /// K3a: `which_key_for` reads whatever [`Resolver`](norte_frontend::keymap::Resolver)
    /// it is GIVEN — proof that [`NorteGui::refresh_which_key_viewer`] cannot
    /// silently paint the pane's rows while the viewer owns the keyboard, the
    /// mistake `which_key_for` exists to catch: build it against a
    /// VIEWER-context resolver, and the row is the viewer's own command, not
    /// anything from Browse.
    #[test]
    fn which_key_for_reads_the_viewer_resolver_not_the_pane() {
        use norte_frontend::keymap::{Effective, Resolver, Screen, parse_chord, parse_keymap};
        let src = r#"
[viewer]
keymap = [
    { on = ["g", "g"], run = "viewer.top" },
]
"#;
        let preset = parse_keymap(src).expect("fixture parses");
        let known = ["viewer.top"];
        let eff =
            Effective::build_for(&preset, &[], &known, Screen::Viewer).expect("fixture builds");
        let mut resolver = Resolver::new(eff);
        resolver.push(parse_chord("g").expect("chord"));

        let panel =
            which_key_for(&resolver, norte_i18n::Lang::En).expect("g is pending: panel opens");
        assert_eq!(panel.title, "g");
        assert_eq!(panel.rows.len(), 1);
        assert_eq!(panel.rows[0].chord, "g");

        // A bare count opens nothing (whichkey's own rule) — same guard the
        // pane path relies on, exercised here against the viewer resolver.
        let mut counting = Resolver::new(
            Effective::build_for(&preset, &[], &known, Screen::Viewer).expect("fixture builds"),
        );
        counting.push(parse_chord("1").expect("chord"));
        assert!(
            which_key_for(&counting, norte_i18n::Lang::En).is_none(),
            "a bare count has no panel"
        );
    }

    /// K3c c4: the door plans against the stack the LOADER merges, and the
    /// GUI's stack has one layer no `keymap.toml` contains
    /// (`keymap::gui_supplement`). Parallel arrays, ascending precedence, and
    /// the supplement at the bottom — anything else and
    /// `RebindSources::split_at` cuts in the wrong place, which is the one
    /// failure its own documentation says is silent.
    #[test]
    fn la_puerta_planifica_con_el_suplemento_de_la_gui_debajo() {
        let cfg = empty_frontend_config();
        let (kinds, layers) = rebind_layers(&cfg);
        assert_eq!(kinds.len(), layers.len(), "paralelos, uno por capa");
        assert_eq!(kinds[0], norte_config::Layer::System, "y el más bajo");
        // `KeymapFile` keeps its fields `pub(super)` (there is no public
        // accessor for the binding lists), so the comparison is the `Debug`
        // shape — enough to say WHICH layer this is, which is the claim.
        assert_eq!(
            format!("{:?}", layers[0]),
            format!("{:?}", keymap::gui_supplement()),
            "la primera capa ES el suplemento"
        );
    }

    /// K3c c4: the door, called exactly as `NorteGui::plan_rebind` calls it,
    /// on the two screens this window resolves keys through.
    ///
    /// The viewer half is the one worth having: its map is validated against
    /// `VIEWER_COMMANDS`, which does NOT contain the `app.*` verbs `[global]`
    /// merges into it — those survive as `NotHere` (K1 decision 4) rather
    /// than failing the load, and a door handed a different set would refuse
    /// a binding the loader accepts.
    #[test]
    fn la_puerta_deja_pasar_en_las_dos_pantallas_y_rechaza_la_tecla_sagrada() {
        use norte_frontend::keymap::{Screen, parse_chord};
        use norte_frontend::shortcuts::{PlanError, plan_rebind};

        let cfg = empty_frontend_config();
        let (kinds, layers) = rebind_layers(&cfg);
        let door = |screen: Screen, chord: &str, command: &str| {
            plan_rebind(
                norte_config::DEFAULT_PRESET,
                &kinds,
                &layers,
                keymap::screen_commands(screen),
                screen,
                &[parse_chord(chord).expect("chord")],
                command,
            )
        };

        let w = door(Screen::Browse, "ctrl+alt+n", "pane.mkdir").expect("libre");
        assert_eq!(w.section, "pane");
        assert_eq!(
            w.list,
            norte_config::KeymapList::Prepend,
            "un append no pisaría al preset"
        );
        assert_eq!(w.chords, ["ctrl+alt+n".to_owned()]);

        let w = door(Screen::Viewer, "ctrl+alt+j", "viewer.close").expect("libre en el visor");
        assert_eq!(w.section, "viewer");

        // Sacred (spec §12): the verdict refuses it and so does the door —
        // the verdict is the convenience, the door is the guarantee.
        assert!(matches!(
            door(Screen::Browse, "tab", "pane.mkdir"),
            Err(PlanError::Door(_))
        ));
    }

    /// K3c c4: the one decision this frontend has to make that the TUI does
    /// not — "the file changed" and "your keyboard changed" are separate
    /// claims here, because nothing watches files, and the rebuild that
    /// bridges them is all-or-nothing.
    ///
    /// Four rows, and each is a different sentence to the reader.
    #[test]
    fn el_mensaje_de_una_escritura_distingue_guardado_de_aplicado() {
        let ok = || "F5 → pane.mkdir".to_owned();
        let nothing = || "nada casó".to_owned();
        let qualifier = norte_i18n::t("gui-msg-shortcut-saved-not-applied");

        // A bind that landed: the plain sentence, no qualifier.
        let (m, err) = shortcut_write_message(true, true, ok(), None);
        assert_eq!((m, err), (ok(), false));

        // A bind whose file was already what it wanted still landed: for a
        // BIND the two are the same statement about the key.
        let (m, err) = shortcut_write_message(false, true, ok(), None);
        assert_eq!((m, err), (ok(), false));

        // Written, but the rebuild did not land: this window kept the old
        // keymap and must NOT claim the key changed.
        let (m, err) = shortcut_write_message(true, false, ok(), None);
        assert!(m.starts_with(&ok()) && m.contains(&qualifier), "{m}");
        assert!(!err, "guardado-sin-aplicar es un aviso, no un fallo");

        // An unbind that matched nothing (#141) is the ONLY error: there
        // "nothing happened" is the answer, not a qualifier on a success.
        let (m, err) = shortcut_write_message(false, true, ok(), Some(nothing()));
        assert_eq!((m, err), (nothing(), true));

        // ...and an unbind that DID remove something behaves like a bind.
        let (m, err) = shortcut_write_message(true, true, ok(), Some(nothing()));
        assert_eq!((m, err), (ok(), false));
    }

    /// K3a paid the documented debt: the viewer used to be a THIRD exclusion
    /// alongside settings/extensions (`main.rs`, pre-K3a), so an
    /// `unavailable_message` set while the viewer was open landed in
    /// `self.flash` and never painted. `flash_paints` is the extracted gate
    /// `render` now calls, and it takes only the two screens that still have
    /// their OWN status line to protect — the viewer is not a parameter, so
    /// there is no way for it to suppress this line again.
    #[test]
    fn the_flash_reaches_the_viewer_now() {
        assert!(
            flash_paints(false, false),
            "dual-pane AND the viewer: both paint it"
        );
        assert!(!flash_paints(true, false), "settings has its own status");
        assert!(!flash_paints(false, true), "extensions has its own status");
    }

    /// K3a BLOCKER fix: a modal opened by a background task landing (a
    /// conflict, an AI-rename reply) or help opened from inside the viewer's
    /// key handler (F1, before `viewer_resolver.push` runs) can take the
    /// keyboard away from the resolver `self.which_key` describes WITHOUT
    /// going through the two `refresh_which_key*` methods, and without
    /// emptying its live `pending()` either — so those two flags are the
    /// only thing that can still suppress the panel once modal/help are up.
    #[test]
    fn which_key_paints_is_suppressed_by_modal_and_help() {
        assert!(
            which_key_paints(false, true, false, false),
            "pending prefix, rows ready, nothing else in front: paints"
        );
        assert!(
            !which_key_paints(true, true, false, false),
            "nothing pending: no panel, even with stale rows cached"
        );
        assert!(
            !which_key_paints(false, false, false, false),
            "no cached rows yet: nothing to paint"
        );
        assert!(
            !which_key_paints(false, true, true, false),
            "a modal took the keyboard without a keystroke"
        );
        assert!(
            !which_key_paints(false, true, false, true),
            "F1 opened help without ever reaching the resolver"
        );
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
    /// H3f: the four roles the help paints with resolve to a VISIBLE colour in
    /// every shipped theme, and never to the page's own background.
    ///
    /// A help page painted in the background colour is a page the reader cannot
    /// read, and it fails silently — the overlay opens, the keys work, and the
    /// text is not there. The check is per THEME because each one declares its
    /// own roles: a preset that forgets `warning` falls back through `chrome`,
    /// and this is what says the fallback is still legible.
    #[test]
    fn los_roles_de_la_ayuda_son_visibles_en_todos_los_temas() {
        for name in norte_theme::preset_names() {
            let theme = norte_theme::Theme::preset(name)
                .expect("preset parseable")
                .expect("preset existente");
            let c = ChromeColors::resolve(&theme);
            let bg = c.pane_bg_focus;
            for role in [
                norte_theme::Role::Title,
                norte_theme::Role::Mark,
                norte_theme::Role::Info,
                norte_theme::Role::Warning,
            ] {
                // Same resolution `NorteGui::help_role_color` performs, minus
                // the `self` it needs — kept in sync by using the same helpers.
                let fg = match role {
                    norte_theme::Role::Title => c.header_fg,
                    norte_theme::Role::Mark => chrome_mark_fg(&theme),
                    norte_theme::Role::Info => c.quick_fg,
                    _ => c.err_fg,
                };
                assert_ne!(
                    (fg.r, fg.g, fg.b),
                    (bg.r, bg.g, bg.b),
                    "{name}/{role:?} paints the help in its own background"
                );
            }
        }
    }

    /// H3f review (rust BLOCKER 1): the sidebar never paints a Fluent id.
    ///
    /// The model always emits a group for the synthetic keyboard page, and
    /// neither catalogue defines `help-group-keys` — deliberately, because the
    /// row underneath already wears that name. `norte_i18n::t` answers a
    /// missing message WITH THE ID, so the overlay shipped a literal
    /// `help-group-keys` to every reader who pressed F1, in both locales, with
    /// no plugin and no filter involved.
    #[test]
    fn la_barra_lateral_jamas_pinta_un_id_de_fluent() {
        let state = norte_frontend::help::HelpState::new(
            norte_i18n::active(),
            norte_i18n::t("help-topic-keys"),
        );
        for row in state.rows() {
            let Some(label) = crate::NorteGui::sidebar_label(row) else {
                continue;
            };
            assert!(
                !label.starts_with("help-group-"),
                "raw Fluent id in the sidebar: {label:?}"
            );
        }
        // The keyboard group in particular is suppressed, not translated.
        let keys_group = state.rows().iter().find(
            |r| matches!(r, norte_frontend::help::SidebarRow::Group { tag } if tag == "keys"),
        );
        assert!(keys_group.is_some(), "the model still emits that group");
        assert_eq!(
            keys_group.and_then(crate::NorteGui::sidebar_label),
            None,
            "and the painter drops it"
        );
    }

    /// The other half of the same rule: a group the CORPUS declares must have a
    /// name, or suppressing untranslated tags would make it vanish silently
    /// instead of painting its id.
    #[test]
    fn los_grupos_del_corpus_tienen_nombre_traducido() {
        for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
            for topic in norte_help::topics(lang) {
                let Some(tag) = topic.tags.first() else {
                    continue;
                };
                let id = format!("help-group-{tag}");
                assert_ne!(
                    norte_i18n::t_in(lang, &id),
                    id,
                    "{lang:?}: group {tag:?} of topic {} has no name",
                    topic.id.as_str()
                );
            }
        }
    }

    /// H3f review (rust MAJOR 7): the sidebar scrolls, so the selection cannot
    /// leave the window while the body keeps changing for a row nobody sees.
    #[test]
    fn la_barra_lateral_mantiene_el_cursor_a_la_vista() {
        const H: usize = 5;
        // Short list: never scrolls.
        assert_eq!(crate::NorteGui::sidebar_offset(3, 4, H), 0);
        // Cursor inside the first window: still anchored at the top.
        assert_eq!(crate::NorteGui::sidebar_offset(4, 20, H), 0);
        // Past it: the window follows, one row at a time.
        assert_eq!(crate::NorteGui::sidebar_offset(5, 20, H), 1);
        // At the end: clamped so the last window is full, never past it.
        assert_eq!(crate::NorteGui::sidebar_offset(19, 20, H), 15);
        // And the invariant that matters, for every position of a long list.
        for cursor in 0..40 {
            let first = crate::NorteGui::sidebar_offset(cursor, 40, H);
            assert!(
                (first..first + H).contains(&cursor),
                "cursor {cursor} fell outside the window starting at {first}"
            );
        }
    }

    /// H3f: the GUI's help footer is a fixed string, so nothing generates it
    /// and nothing would notice it missing from a locale until a reader saw a
    /// raw `help-hint-gui` in the overlay.
    #[test]
    fn el_pie_de_la_ayuda_existe_en_ambos_locales() {
        for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
            assert_ne!(
                norte_i18n::t_in(lang, "help-hint-gui"),
                "help-hint-gui",
                "{lang:?}"
            );
        }
    }

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

    /// Glifo del canalón de marca (#111): el preset `default` declara solo
    /// `mark.bg`, así que el glifo cae al fallback ámbar `MARK_FG`; un tema
    /// que declara `mark.fg` lo reemplaza.
    #[test]
    fn el_glifo_de_marca_resuelve_fg_del_tema_o_fallback() {
        let d = norte_theme::Theme::preset_default();
        assert_eq!(chrome_mark_fg(&d), gpui::rgb(MARK_FG));
        let t = norte_theme::Theme::from_toml(
            "name = \"x\"\n[roles]\nmark = { fg = \"#ff00ff\", bg = \"#101010\" }\n",
        )
        .expect("tema con mark.fg parsea");
        assert_eq!(chrome_mark_fg(&t), gpui::rgb(0xff00ff));
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

    // --- G3a: `styled_span_color` (preview de plugin con estilo) -----------

    fn ansi_span(
        role: Option<norte_theme::Role>,
        fg: Option<(u8, u8, u8)>,
    ) -> norte_frontend::ansi::StyledSpan {
        norte_frontend::ansi::StyledSpan {
            text: "x".into(),
            role,
            fg,
        }
    }

    /// El preset `default` (ADR 0037: fuente única, sin repetir el hex a
    /// mano en dos sitios) fija `title = #5fafd7`: un span con `role` Y
    /// `fg` distintos debe pintar el DEL TEMA — `role` gana (decisión 3).
    #[test]
    fn styled_span_color_role_gana_a_fg() {
        let theme = Theme::preset_default();
        let span = ansi_span(Some(norte_theme::Role::Title), Some((255, 0, 0)));
        let got = styled_span_color(&theme, &span, None).expect("title tiene color");
        assert_eq!(
            got,
            rgb(0x5fafd7),
            "pinta el color del tema, no el fg crudo"
        );
    }

    /// Sin `role`, `fg` crudo pasa tal cual (vía `Color::rgb` +
    /// `to_gpui_rgba`, MISMA conversión que el resto del theming — no un
    /// camino paralelo).
    #[test]
    fn styled_span_color_sin_role_usa_fg_crudo() {
        let theme = Theme::preset_default();
        let span = ansi_span(None, Some((10, 20, 30)));
        let got = styled_span_color(&theme, &span, None).expect("fg presente");
        assert_eq!(
            got,
            theme_map::to_gpui_rgba(norte_theme::Color::rgb(10, 20, 30))
        );
    }

    /// Un `role` VÁLIDO cuyo tema activo no lo colorea (fallback monocromo,
    /// sin `fg`) resuelve a `None` — AUNQUE el span traiga `fg`: `role` gana
    /// incluso para "perder" el fallback, coherente con la precedencia (el
    /// tema manda, no un truco de "si no hay color, usa el crudo").
    #[test]
    fn styled_span_color_role_sin_color_en_tema_es_none_aunque_haya_fg() {
        let vacio = norte_theme::Theme::from_toml("").expect("tema vacío parsea");
        let span = ansi_span(Some(norte_theme::Role::Regular), Some((1, 2, 3)));
        assert_eq!(
            styled_span_color(&vacio, &span, None),
            None,
            "Regular sin tema no tiene color: None, ni el fg crudo se usa"
        );
    }

    /// Sin `role` ni `fg`: `None` (el caller no fija `.text_color()`, hereda
    /// el color del contenedor).
    #[test]
    fn styled_span_color_sin_nada_es_none() {
        let theme = Theme::preset_default();
        let span = ansi_span(None, None);
        assert_eq!(styled_span_color(&theme, &span, None), None);
    }

    // --- G3b: `decoration_badge_color` (badge de decorator de plugin) -----

    /// Con un rol RECONOCIDO por el tema, el badge pinta ESE color (mismo
    /// criterio que `styled_span_color_role_gana_a_fg`): el `base` de la
    /// fila NUNCA se usa cuando el tema resuelve el rol.
    #[test]
    fn decoration_badge_color_con_rol_usa_el_tema() {
        let theme = Theme::preset_default();
        let base = rgb(0x112233);
        let got = decoration_badge_color(&theme, Some(norte_theme::Role::Title), base, None);
        assert_eq!(
            got,
            rgb(0x5fafd7),
            "el color del tema para Title, no `base`"
        );
    }

    /// Sin rol reconocido (o el tema no lo colorea): cae a `base` con alfa
    /// reducido — el análogo de "dim" (nunca opaco al 100%, nunca un color
    /// inventado que no venga del propio color de la fila).
    #[test]
    fn decoration_badge_color_sin_rol_deriva_de_base_con_alfa_reducido() {
        let theme = Theme::preset_default();
        let base = rgb(0x112233);
        let got = decoration_badge_color(&theme, None, base, None);
        assert_eq!(got.r, base.r);
        assert_eq!(got.g, base.g);
        assert_eq!(got.b, base.b);
        assert!(
            got.a < base.a,
            "el alfa se reduce: {got:?} vs base {base:?}"
        );
    }

    /// El glow se aplica DESPUÉS de resolver por rol (mismo criterio que
    /// `styled_span_color_aplica_glow_encima`) — no en la rama "sin rol"
    /// (esa ya deriva de `base`, que el caller ya glowed si aplicaba).
    #[test]
    fn decoration_badge_color_con_rol_aplica_glow() {
        let theme = Theme::preset_default();
        let base = rgb(0x112233);
        let glow = effects::Glow { strength: 1.0 };
        let sin_glow = decoration_badge_color(&theme, Some(norte_theme::Role::Title), base, None);
        let con_glow =
            decoration_badge_color(&theme, Some(norte_theme::Role::Title), base, Some(glow));
        assert_ne!(
            sin_glow, con_glow,
            "el glow debe mover el color hacia blanco"
        );
    }

    /// El glow de G1 se aplica DESPUÉS de resolver el color, tanto por
    /// `role` como por `fg` — mismo criterio que `entry_color`.
    #[test]
    fn styled_span_color_aplica_glow_encima() {
        let theme = Theme::preset_default();
        let glow = Some(effects::Glow { strength: 1.0 });
        let span = ansi_span(Some(norte_theme::Role::Title), None);
        let got = styled_span_color(&theme, &span, glow).expect("title tiene color");
        assert_eq!(
            got,
            glowed(rgb(0x5fafd7), glow),
            "glow aplicado sobre el color del rol"
        );
    }

    // --- G2 motion: helpers puros (decisión 3) -----------------------------

    /// `flicker_factor` (G2 decisión 3): `strength = 0.0` es la identidad
    /// exacta para CUALQUIER tiempo transcurrido — así un caller nunca
    /// necesita ramificar "flicker ausente" antes de multiplicar.
    #[test]
    fn flicker_factor_strength_cero_es_identidad() {
        for t in [0.0_f32, 0.3, 1.0, 7.5, 100.0] {
            let f = flicker_factor(0.0, t);
            assert!((f - 1.0).abs() < f32::EPSILON, "t={t}: f={f}");
        }
    }

    /// `flicker_factor` está acotado a `[1-strength, 1+strength]` (seno
    /// acotado a `[-1, 1]`) para CUALQUIER tiempo — el clamp de
    /// `effects::FLICKER_STRENGTH_RANGE` ([0, 0.15]) ya garantiza que el
    /// peor caso real sea `[0.85, 1.15]`, pero esta prueba no asume ese
    /// clamp: cubre el rango completo de `strength` que la firma acepta.
    #[test]
    fn flicker_factor_acotado_por_strength() {
        for s in [0.0_f32, 0.05, 0.15, 0.5, 1.0] {
            for i in 0..200_i32 {
                #[allow(clippy::cast_precision_loss)]
                let t = i as f32 * 0.037;
                let f = flicker_factor(s, t);
                assert!(
                    (1.0 - s..=1.0 + s).contains(&f),
                    "s={s} t={t}: f={f} fuera de [{}, {}]",
                    1.0 - s,
                    1.0 + s
                );
            }
        }
    }

    /// `flicker_scale` (G2 decisión 3): el resultado NUNCA excede `cap` — ni
    /// con un `factor` extremo (por encima de lo que `flicker_factor` puede
    /// producir con un `strength` ya clampado) ni negativo (piso `0.0`,
    /// nunca una opacidad/strength negativa). Este es el "clamp compuesto"
    /// que evita que el flicker empuje un valor de tema YA clampado por
    /// encima de su propio tope de accesibilidad (ADR 0036 §2).
    #[test]
    fn flicker_scale_nunca_excede_el_cap_estatico() {
        let cap = effects::SCANLINES_OPACITY_RANGE.1;
        for base in [0.0_f32, 0.1, cap] {
            for factor in [-5.0_f32, 0.0, 0.85, 1.0, 1.15, 100.0] {
                let v = flicker_scale(base, factor, cap);
                assert!(
                    (0.0..=cap).contains(&v),
                    "base={base} factor={factor}: v={v} fuera de [0, {cap}]"
                );
            }
        }
    }

    /// `flicker_scale` con `factor = 1.0` es la identidad para un `base` ya
    /// dentro de `[0, cap]` (el caso "sin flicker" no debe alterar el valor
    /// clampado que `effects.rs` ya produjo).
    #[test]
    fn flicker_scale_factor_uno_es_identidad_dentro_del_cap() {
        let cap = effects::VIGNETTE_STRENGTH_RANGE.1;
        let base = cap * 0.5;
        assert!((flicker_scale(base, 1.0, cap) - base).abs() < f32::EPSILON);
    }

    /// `motion_active` (G2 decisión 3): tabla de verdad completa. El flicker
    /// solo importa cuando ya pinta algo (`main.rs` solo llama con
    /// `flicker=true` cuando además hay scanlines/vignette); el cursor blink
    /// es una aproximación documentada (`cursor_blink && !panes vacíos`, ver
    /// doc de la función) — errar hacia `true` cuando no hay fila resaltada
    /// de verdad es inofensivo (un `request_animation_frame` de más no
    /// cuesta nada, GPUI los coalesce).
    #[test]
    fn motion_active_tabla_de_verdad() {
        let casos = [
            (false, false, false, false),
            (false, false, true, false),
            (false, true, false, false),
            (false, true, true, true),
            (true, false, false, true),
            (true, false, true, true),
            (true, true, false, true),
            (true, true, true, true),
        ];
        for (flicker, blink, nonempty, expected) in casos {
            assert_eq!(
                motion_active(flicker, blink, nonempty),
                expected,
                "flicker={flicker} blink={blink} nonempty={nonempty}"
            );
        }
    }

    /// `ChromeColors::hover_bg` (GP look-and-feel): DERIVADO, no un rol de
    /// tema — `lerp(pane_bg_focus, sel_bg, 0.35)` calculado aquí de forma
    /// INDEPENDIENTE del cuerpo de `resolve` (no basta con confiar en que el
    /// código de producción llame a la misma fórmula; si alguien cambia el
    /// factor o los colores base en `resolve` sin querer, este test lo pilla).
    #[test]
    fn chrome_hover_derivado() {
        let t = norte_theme::Theme::preset_default();
        let c = ChromeColors::resolve(&t);
        let expected = gpui::Rgba {
            r: c.pane_bg_focus.r + (c.sel_bg.r - c.pane_bg_focus.r) * 0.35,
            g: c.pane_bg_focus.g + (c.sel_bg.g - c.pane_bg_focus.g) * 0.35,
            b: c.pane_bg_focus.b + (c.sel_bg.b - c.pane_bg_focus.b) * 0.35,
            a: c.pane_bg_focus.a + (c.sel_bg.a - c.pane_bg_focus.a) * 0.35,
        };
        assert!(
            (c.hover_bg.r - expected.r).abs() < 1e-6,
            "r: got {}, want {}",
            c.hover_bg.r,
            expected.r
        );
        assert!(
            (c.hover_bg.g - expected.g).abs() < 1e-6,
            "g: got {}, want {}",
            c.hover_bg.g,
            expected.g
        );
        assert!(
            (c.hover_bg.b - expected.b).abs() < 1e-6,
            "b: got {}, want {}",
            c.hover_bg.b,
            expected.b
        );
        assert!(
            (c.hover_bg.a - expected.a).abs() < 1e-6,
            "a: got {}, want {}",
            c.hover_bg.a,
            expected.a
        );
        assert_ne!(
            c.hover_bg, c.pane_bg_focus,
            "con t=0.35 y sel_bg != pane_bg_focus en el tema default, hover_bg debe distinguirse"
        );
    }

    /// Con las familias default (ya resueltas — `FontSet::resolve` ya no
    /// valida nada, ver `validated_family`), `FontSet` cae a la fuente mono
    /// BUNDLED ("JetBrains Mono", GP review CRÍTICO — ya no `.ZedMono`, que
    /// era un alias de sistema casi siempre irresoluble) y al alias de chrome
    /// `.SystemUIFont`, más el tamaño/alto por defecto.
    #[test]
    fn fontset_defaults() {
        let f = FontSet::resolve(".SystemUIFont", "JetBrains Mono", None);
        assert_eq!(f.ui.family.as_ref(), ".SystemUIFont");
        assert_eq!(f.mono.family.as_ref(), "JetBrains Mono");
        assert_eq!(f.size, gpui::px(14.0));
        assert_eq!(f.row_h, gpui::px(21.0));
    }

    /// Una familia YA VALIDADA (distinta del default) ocupa `family`, pero
    /// lleva el default como `Font::fallbacks` — cobertura per-glifo, no un
    /// mecanismo de "familia rota degrada a esto" (esa degradación ahora
    /// vive en `validated_family`, ANTES de llegar aquí).
    #[test]
    fn fontset_familia_custom_lleva_fallback_al_default() {
        let f = FontSet::resolve(".SystemUIFont", "Custom Mono", None);
        assert_eq!(f.mono.family.as_ref(), "Custom Mono");
        let fb = f.mono.fallbacks.as_ref().expect("fallback presente");
        assert_eq!(fb.fallback_list(), ["JetBrains Mono".to_owned()]);
    }

    /// El piso de 18px evita filas ilegiblemente finas con tamaños de fuente
    /// muy pequeños (config válida solo desde 8.0, ver `norte-config`, pero
    /// el piso vive aquí porque `FontSet` no conoce ese rango).
    #[test]
    fn row_h_minimo() {
        let f = FontSet::resolve(".SystemUIFont", "JetBrains Mono", Some(8.0));
        assert_eq!(f.row_h, gpui::px(18.0));
    }

    /// `validated_family` (GP review fix 2): sin request, cae al default sin
    /// aviso — no es un error de usuario, es "sin config".
    #[test]
    fn familia_sin_pedido_cae_al_default_sin_avisar() {
        let known = ["JetBrains Mono".to_owned(), "Iosevka".to_owned()];
        let (resolved, warn) = validated_family(None, &known, "JetBrains Mono");
        assert_eq!(resolved, "JetBrains Mono");
        assert!(warn.is_none());
    }

    /// `validated_family` (GP review fix 2): una familia que SÍ está en el
    /// fontdb real se conserva tal cual, sin aviso.
    #[test]
    fn familia_conocida_se_conserva_sin_avisar() {
        let known = ["JetBrains Mono".to_owned(), "Iosevka".to_owned()];
        let (resolved, warn) = validated_family(Some("Iosevka"), &known, "JetBrains Mono");
        assert_eq!(resolved, "Iosevka");
        assert!(warn.is_none());
    }

    /// `validated_family` (GP review fix 2, test pedido explícitamente por el
    /// review): una familia AUSENTE del fontdb real cae al default Y devuelve
    /// el nombre pedido para el banner de arranque — nunca construye un
    /// `Font` sobre una familia primaria inexistente (el hallazgo CRÍTICO
    /// original: la resolución entonces caminaba la pila global de GPUI en
    /// silencio).
    #[test]
    fn familia_desconocida_cae_al_default_y_avisa() {
        let known = ["JetBrains Mono".to_owned(), "Iosevka".to_owned()];
        let (resolved, warn) = validated_family(Some("NopeFont 9000"), &known, "JetBrains Mono");
        assert_eq!(resolved, "JetBrains Mono");
        assert_eq!(warn.as_deref(), Some("NopeFont 9000"));
    }

    /// #123: la tanda de hidratación deja fuera lo YA pedido y respeta el
    /// tope. Sin la dedup, cada frame volvería a pedir las mismas rutas —
    /// y una que el daemon no supo statear (se queda sin `size`, así que
    /// sigue siendo candidata) se reintentaría en bucle.
    #[test]
    fn hydration_batch_deduplica_y_acota() {
        let vp = |w: &str| VPath::parse(w).expect("wire de test");
        let candidatas = vec![vp("mem:///a"), vp("mem:///b"), vp("mem:///c")];
        let mut probed = std::collections::HashSet::new();
        probed.insert(vp("mem:///b"));
        assert_eq!(
            hydration_batch(candidatas.clone(), &probed, 10),
            vec![vp("mem:///a"), vp("mem:///c")],
            "lo ya pedido no se repite"
        );
        assert_eq!(
            hydration_batch(candidatas.clone(), &probed, 1),
            vec![vp("mem:///a")],
            "el tope corta la tanda"
        );
        let todas: std::collections::HashSet<VPath> = candidatas.iter().cloned().collect();
        assert!(
            hydration_batch(candidatas, &todas, 10).is_empty(),
            "en régimen estacionario no se manda nada"
        );
    }

    // --- Ratón: marcado con ctrl/shift y arrastre (plan de ratón, tarea 3) --
    //
    // Las REGLAS viven en `norte_frontend::mouse` y están clavadas allí; lo
    // que se clava aquí es el cableado de la GUI: que los efectos de la máquina
    // compartida caen sobre el `PaneState` correcto, que el ancla de un
    // shift+click es el CURSOR y no la fila pulsada, y que un click limpio
    // sigue sin tocar las marcas.

    /// Un pane con `n` entradas (`mem:///e0`…`mem:///e{n-1}`).
    fn pane_con(n: usize) -> norte_frontend::PaneState {
        let dir = VPath::parse("mem:///").unwrap();
        let entries = (0..n)
            .map(|i| norte_proto::Entry {
                attrs: std::collections::BTreeMap::new(),
                path: dir.join(
                    norte_proto::Segment::new(format!("e{i}").into_bytes())
                        .expect("segmento no vacío y sin '/'"),
                ),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            })
            .collect();
        norte_frontend::PaneState::new(dir, entries)
    }

    /// Los dos panes del modelo, con `n` entradas cada uno.
    fn panes_con(n: usize) -> [norte_frontend::PaneState; 2] {
        [pane_con(n), pane_con(n)]
    }

    /// ¿Está marcada la fila `i` de `pane`? Lo mismo que le pasa
    /// `render_pane` a `render_row` como flag `marked` (el fondo `mark_bg` y
    /// el `●` del canalón salen de ahí), así que asertarlo es asertar el
    /// tratamiento visual de la fila sin necesidad de GPU.
    fn marcada(pane: &norte_frontend::PaneState, i: usize) -> bool {
        pane.entries().get(i).is_some_and(|e| pane.is_marked(e))
    }

    /// ctrl+click togglea EXACTAMENTE una entrada, en los dos sentidos, y no
    /// arrastra tras de sí ni el cursor ni las vecinas.
    #[test]
    fn ctrl_click_togglea_exactamente_una_entrada() {
        let mut mouse = MouseState::default();
        let mut panes = panes_con(6);
        let mut focus = 1usize;

        let _ = mouse_press(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 3),
            Mods::CTRL,
        );
        assert_eq!(focus, 0, "el pane pulsado toma el foco");
        assert_eq!(panes[0].marks_len(), 1, "una marca, ni una más");
        assert!(marcada(&panes[0], 3));
        assert!(panes[1].marks_len() == 0, "el otro pane no se entera");

        // Un ctrl+click más sobre la MISMA fila la desmarca (si no, una fila
        // marcada no tendría forma de desmarcarse con el ratón: cualquier
        // otra lectura de una fila marcada arma una transferencia).
        let _ = mouse_press(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 3),
            Mods::CTRL,
        );
        assert_eq!(panes[0].marks_len(), 0);

        // Y otra fila distinta suma en vez de reemplazar.
        for i in [1usize, 4] {
            let _ = mouse_press(
                &mut mouse,
                &mut panes,
                &mut focus,
                Spot::new(0, i),
                Mods::CTRL,
            );
        }
        assert_eq!(panes[0].marks_len(), 2);
        assert!(marcada(&panes[0], 1) && marcada(&panes[0], 4));
        assert!(!marcada(&panes[0], 2), "el hueco entre las dos NO se marca");
    }

    /// shift+click marca el rango desde el CURSOR, no desde el punto de la
    /// pulsación. La diferencia es todo el gesto: anclar en la fila pulsada
    /// marcaría UNA sola entrada y el rango entero se perdería en silencio.
    #[test]
    fn shift_click_marca_el_rango_desde_el_cursor_no_desde_la_pulsacion() {
        let mut mouse = MouseState::default();
        let mut panes = panes_con(10);
        let mut focus = 0usize;

        // Click limpio en la 6: deja el cursor ahí (y no marca nada).
        let _ = mouse_press(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 6),
            Mods::NONE,
        );
        let _ = mouse_release(
            &mut mouse,
            &mut panes,
            &mut focus,
            Some(Spot::new(0, 6)),
            Mods::NONE,
        );
        assert_eq!(panes[0].cursor(), 6);
        assert_eq!(panes[0].marks_len(), 0);

        // shift+click en la 2: marca 2..=6 (cinco filas). Anclado en la fila
        // pulsada habría marcado una sola.
        let _ = mouse_press(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 2),
            Mods::SHIFT,
        );
        assert_eq!(panes[0].marks_len(), 5, "rango 2..=6 desde el cursor");
        for i in 2..=6 {
            assert!(marcada(&panes[0], i), "la fila {i} debe quedar marcada");
        }
        assert!(!marcada(&panes[0], 1) && !marcada(&panes[0], 7));
        assert_eq!(
            panes[0].cursor(),
            6,
            "el cursor se queda en el ancla: el extremo sigue a la vista y un \
             segundo shift+click extiende el MISMO rango"
        );
    }

    /// Un arrastre marca lo que barre, y RETROCEDER devuelve el exceso. Es la
    /// propiedad que el usuario nota: pasarse quince filas y volver no puede
    /// dejar quince ficheros marcados fuera de pantalla.
    #[test]
    fn el_arrastre_barre_marcando_y_al_retroceder_devuelve_el_exceso() {
        let mut mouse = MouseState::default();
        let mut panes = panes_con(20);
        let mut focus = 1usize;

        let aplicado = mouse_press(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 2),
            Mods::NONE,
        );
        assert_eq!(aplicado.moved_cursor, Some(0));
        assert_eq!(
            panes[0].marks_len(),
            0,
            "la pulsación arma el barrido pero todavía no marca"
        );

        let _ = mouse_motion(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 15),
            Mods::NONE,
        );
        assert_eq!(panes[0].marks_len(), 14, "2..=15");
        assert!(marcada(&panes[0], 15));

        // Retroceso: el rango se encoge y el exceso se SUELTA.
        let _ = mouse_motion(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 5),
            Mods::NONE,
        );
        assert_eq!(panes[0].marks_len(), 4, "2..=5");
        for i in 6..=15 {
            assert!(!marcada(&panes[0], i), "la fila {i} debía soltarse");
        }

        // El release re-enuncia el rango final (los motions pueden venir
        // coalescidos) y no lo amplía.
        let _ = mouse_release(
            &mut mouse,
            &mut panes,
            &mut focus,
            Some(Spot::new(0, 5)),
            Mods::NONE,
        );
        assert_eq!(panes[0].marks_len(), 4);
        assert_eq!(panes[0].cursor(), 5, "el cursor sigue al puntero");
        assert_eq!(panes[1].marks_len(), 0);
    }

    /// Una marca hecha ANTES del gesto sobrevive al retroceso del barrido: el
    /// rubber-band solo devuelve lo que puso ESTE arrastre.
    #[test]
    fn el_barrido_no_se_lleva_por_delante_las_marcas_previas() {
        let mut mouse = MouseState::default();
        let mut panes = panes_con(20);
        let mut focus = 0usize;

        let _ = mouse_press(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 12),
            Mods::CTRL,
        );
        assert!(marcada(&panes[0], 12));

        let _ = mouse_press(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 2),
            Mods::NONE,
        );
        let _ = mouse_motion(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 15),
            Mods::NONE,
        );
        let _ = mouse_motion(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 4),
            Mods::NONE,
        );
        let _ = mouse_release(
            &mut mouse,
            &mut panes,
            &mut focus,
            Some(Spot::new(0, 4)),
            Mods::NONE,
        );
        assert!(
            marcada(&panes[0], 12),
            "la marca previa al gesto no la toca el retroceso"
        );
        assert_eq!(panes[0].marks_len(), 4, "2..=4 del barrido + la 12");
    }

    /// Un click limpio sigue siendo lo que siempre fue: foco + cursor, marcas
    /// intactas. Ni la pulsación ni el release marcan nada.
    #[test]
    fn el_click_limpio_solo_enfoca_y_mueve_el_cursor() {
        let mut mouse = MouseState::default();
        let mut panes = panes_con(8);
        let mut focus = 1usize;

        let aplicado = mouse_press(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 3),
            Mods::NONE,
        );
        assert_eq!(focus, 0);
        assert_eq!(panes[0].cursor(), 3);
        assert_eq!(aplicado.moved_cursor, Some(0));
        assert!(aplicado.transfer.is_none());
        assert_eq!(panes[0].marks_len(), 0);

        let aplicado = mouse_release(
            &mut mouse,
            &mut panes,
            &mut focus,
            Some(Spot::new(0, 3)),
            Mods::NONE,
        );
        assert!(
            !aplicado.changed,
            "un gesto que no salió de su fila es un click: el release no emite \
             nada"
        );
        assert_eq!(
            panes[0].marks_len(),
            0,
            "un click JAMÁS cambia la selección"
        );
    }

    /// Arrastrar desde una fila YA marcada hasta el otro pane es una
    /// TRANSFERENCIA, no un barrido: no marca nada y se anuncia (tarea 5 del
    /// plan). Mismo criterio que la TUI, que lo dice por la barra de estado.
    #[test]
    fn el_arrastre_desde_una_fila_marcada_pide_transferencia_y_no_marca() {
        let mut mouse = MouseState::default();
        let mut panes = panes_con(8);
        let mut focus = 0usize;

        let _ = mouse_press(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 2),
            Mods::CTRL,
        );
        assert_eq!(panes[0].marks_len(), 1);

        // Press SIN modificadores sobre esa misma fila marcada: transferencia.
        let _ = mouse_press(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 2),
            Mods::NONE,
        );
        let cruce = mouse_motion(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(1, 4),
            Mods::NONE,
        );
        assert!(!cruce.changed, "una transferencia no marca por el camino");
        let soltar = mouse_release(
            &mut mouse,
            &mut panes,
            &mut focus,
            Some(Spot::new(1, 4)),
            Mods::NONE,
        );
        assert_eq!(
            soltar.transfer,
            Some(DropRequest {
                from_pane: 0,
                to_pane: 1,
                move_files: false,
                promoted: None,
            }),
            "el gesto pide transferencia de las MARCAS del pane 0"
        );
        assert_eq!(panes[0].marks_len(), 1, "las marcas del origen intactas");
        assert_eq!(panes[1].marks_len(), 0, "y el destino sin marcar nada");
    }

    /// Soltar fuera de toda fila (el `on_mouse_up` de la raíz) cancela el
    /// gesto en vez de adivinar un destino — pero lo que el barrido ya marcó
    /// se queda: cancelar un gesto no es deshacerlo.
    #[test]
    fn soltar_fuera_de_toda_fila_cancela_sin_deshacer() {
        let mut mouse = MouseState::default();
        let mut panes = panes_con(10);
        let mut focus = 0usize;

        let _ = mouse_press(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 1),
            Mods::NONE,
        );
        let _ = mouse_motion(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 4),
            Mods::NONE,
        );
        assert_eq!(panes[0].marks_len(), 4);

        let aplicado = mouse_release(&mut mouse, &mut panes, &mut focus, None, Mods::NONE);
        assert!(!aplicado.changed && aplicado.transfer.is_none());
        assert_eq!(panes[0].marks_len(), 4, "lo ya marcado se queda");

        // Y el gesto quedó DESARMADO: un motion posterior no marca sola.
        let huerfano = mouse_motion(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 9),
            Mods::NONE,
        );
        assert!(!huerfano.changed);
        assert_eq!(panes[0].marks_len(), 4);
    }

    /// Un gesto caduca cuando el listado se mueve bajo el puntero (un
    /// refresh asíncrono tras una mutación, un `cd`) o cuando algo se pone
    /// delante: sin esto la siguiente pasada del ratón continuaría un barrido
    /// contra índices que ya nombran otros ficheros. Lo que el barrido ya
    /// marcó SE QUEDA — caducar el gesto no es deshacerlo.
    #[test]
    fn un_gesto_caduca_cuando_el_listado_se_mueve_bajo_el_puntero() {
        let mut mouse = MouseState::default();
        let mut panes = panes_con(10);
        let mut focus = 0usize;
        let vigente = MouseValidity {
            epochs: [panes[0].listing_epoch(), panes[1].listing_epoch()],
            hidden: false,
        };
        expire_stale_gesture(&mut mouse, vigente);

        let _ = mouse_press(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 1),
            Mods::NONE,
        );
        let _ = mouse_motion(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 3),
            Mods::NONE,
        );
        assert_eq!(panes[0].marks_len(), 3, "1..=3");

        // Aterriza un listado nuevo en ese pane: los índices del gesto dejan
        // de nombrar lo que el usuario pulsó.
        let dir = VPath::parse("mem:///otro").unwrap();
        panes[0].set_listing(dir, Vec::new());
        let nueva = MouseValidity {
            epochs: [panes[0].listing_epoch(), panes[1].listing_epoch()],
            hidden: false,
        };
        assert_ne!(nueva, vigente, "un listado nuevo cambia la vigencia");
        expire_stale_gesture(&mut mouse, nueva);

        let mut panes = panes_con(10);
        let huerfano = mouse_motion(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 9),
            Mods::NONE,
        );
        assert!(
            !huerfano.changed,
            "el gesto caducó: una motion posterior no marca por su cuenta"
        );
        assert_eq!(panes[0].marks_len(), 0);
    }

    /// Un overlay/visor delante también caduca el gesto: cuando se cierre, el
    /// usuario ya está a otra cosa.
    #[test]
    fn un_overlay_delante_caduca_el_gesto() {
        let mut mouse = MouseState::default();
        let mut panes = panes_con(10);
        let mut focus = 0usize;
        let epochs = [panes[0].listing_epoch(), panes[1].listing_epoch()];
        expire_stale_gesture(
            &mut mouse,
            MouseValidity {
                epochs,
                hidden: false,
            },
        );
        let _ = mouse_press(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 1),
            Mods::NONE,
        );
        expire_stale_gesture(
            &mut mouse,
            MouseValidity {
                epochs,
                hidden: true,
            },
        );
        assert!(
            !mouse_motion(
                &mut mouse,
                &mut panes,
                &mut focus,
                Spot::new(0, 6),
                Mods::NONE
            )
            .changed
        );
        assert_eq!(panes[0].marks_len(), 0);
    }

    // --- Drag & drop entre panes (plan de ratón, tarea 5) ------------------
    //
    // QUÉ ficheros, en qué dirección y si copia o mueve son decisiones de
    // `norte_frontend::mouse` y están clavadas allí. Lo que se clava aquí es
    // lo único que es de la GUI: que el drop somete EXACTAMENTE lo mismo que
    // la tecla, que un modal abierto lo desactiva, y que un arrastre
    // promovido no cambia la selección.

    /// El drop y la tecla de copiar someten lo MISMO. No solo el mismo
    /// modal: las mismas operaciones al confirmarlo, que es lo que acaba en
    /// el daemon (y por tanto en el journal, en el undo y en el gate de
    /// policy). Un drop es una mutación como cualquier otra; una segunda
    /// ruta más silenciosa es justo lo que este test existe para impedir.
    #[test]
    fn el_drop_somete_exactamente_lo_mismo_que_la_tecla_de_copiar() {
        let mut panes = panes_con(6);
        for i in [1usize, 3, 4] {
            panes[0].set_mark(i, true);
        }

        // Camino del TECLADO (`pane.copy` con el foco en el pane 0).
        let por_teclado =
            transfer_modal(&panes, 0, 1, TransferKind::Copy, None).expect("hay marcas");
        // Camino del ARRASTRE: soltar esas marcas sobre el pane 1.
        let por_arrastre = drop_modal(
            &panes,
            DropRequest {
                from_pane: 0,
                to_pane: 1,
                move_files: false,
                promoted: None,
            },
        )
        .expect("hay marcas");
        assert_eq!(por_teclado, por_arrastre, "el mismo modal");

        // Y lo que se somete al confirmarlo, también.
        let confirmar = |mut m: Modal| match modal::on_key(&mut m, "y", None) {
            ModalOutcome::Submit(ops) => ops,
            otro => panic!("confirmar debía someter, dio {otro:?}"),
        };
        let ops = confirmar(por_teclado);
        assert_eq!(ops.len(), 3, "las tres marcas");
        assert_eq!(ops, confirmar(por_arrastre));

        // Y con shift al soltar es el mismo camino con `Move`.
        let mover = drop_modal(
            &panes,
            DropRequest {
                from_pane: 0,
                to_pane: 1,
                move_files: true,
                promoted: None,
            },
        )
        .expect("hay marcas");
        assert_eq!(
            mover,
            transfer_modal(&panes, 0, 1, TransferKind::Move, None).expect("hay marcas"),
            "mover pasa por la MISMA función que `pane.move`"
        );
    }

    /// El flag copiar/mover lo decide el shift que hay AL SOLTAR, aunque se
    /// pulsara después de empezar el arrastre: quien cambia de idea a mitad
    /// no debe acabar MOVIENDO (mutación destructiva en el origen) lo que
    /// creía copiar. Gesto completo, extremo a extremo.
    #[test]
    fn el_shift_del_release_decide_aunque_se_pulse_a_mitad_del_arrastre() {
        let mut mouse = MouseState::default();
        let mut panes = panes_con(8);
        let mut focus = 0usize;
        panes[0].set_mark(2, true);

        // Pulsa SIN shift sobre la fila marcada y arrastra al otro pane.
        let _ = mouse_press(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 2),
            Mods::NONE,
        );
        let _ = mouse_motion(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(1, 1),
            Mods::NONE,
        );
        // …y solo entonces baja el shift.
        let soltar = mouse_release(
            &mut mouse,
            &mut panes,
            &mut focus,
            Some(Spot::new(1, 1)),
            Mods::SHIFT,
        );
        let req = soltar.transfer.expect("soltó sobre el otro pane");
        assert!(req.move_files, "shift al soltar: MUEVE");
        assert_eq!(
            drop_modal(&panes, req),
            transfer_modal(&panes, 0, 1, TransferKind::Move, None),
            "y sale por el mismo modal que la tecla de mover"
        );
    }

    /// Soltar sobre el pane de ORIGEN no somete nada: copiar un directorio
    /// sobre sí mismo no es lo que pidió quien se arrepintió a medio camino.
    #[test]
    fn soltar_en_el_pane_de_origen_no_somete_nada() {
        let mut mouse = MouseState::default();
        let mut panes = panes_con(8);
        let mut focus = 0usize;
        panes[0].set_mark(2, true);

        let _ = mouse_press(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 2),
            Mods::NONE,
        );
        let _ = mouse_motion(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(1, 1),
            Mods::NONE,
        );
        let soltar = mouse_release(
            &mut mouse,
            &mut panes,
            &mut focus,
            Some(Spot::new(0, 5)), // vuelve a casa y suelta
            Mods::NONE,
        );
        assert!(soltar.transfer.is_none(), "no hay drop que abrir");
        assert_eq!(panes[0].marks_len(), 1, "y la selección, intacta");
    }

    /// Arrastrar una fila SIN marcar al otro pane la transfiere a ella sola
    /// (promoción, ver `norte_frontend::mouse`) — y no toca las marcas del
    /// pane, que son otra cosa. Sin esta distinción el arrastre más común de
    /// cualquier file manager copiaría los once ficheros marcados en vez del
    /// que el usuario tiene cogido.
    #[test]
    fn un_arrastre_promovido_lleva_su_fila_y_no_las_marcas() {
        let mut mouse = MouseState::default();
        let mut panes = panes_con(10);
        let mut focus = 0usize;
        for i in [7usize, 8] {
            panes[0].set_mark(i, true);
        }
        let arrastrada = panes[0].entries()[2].path.clone();

        let _ = mouse_press(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 2),
            Mods::NONE,
        );
        let _ = mouse_motion(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(1, 4),
            Mods::NONE,
        );
        let soltar = mouse_release(
            &mut mouse,
            &mut panes,
            &mut focus,
            Some(Spot::new(1, 4)),
            Mods::NONE,
        );
        let req = soltar.transfer.expect("cruzó de pane: es un drop");
        assert_eq!(req.promoted, Some(2));

        let Some(Modal::ConfirmTransfer { kind, items, to }) = drop_modal(&panes, req) else {
            panic!("un drop promovido abre el modal de confirmación");
        };
        assert_eq!(kind, TransferKind::Copy);
        assert_eq!(items, vec![arrastrada], "SOLO la fila arrastrada");
        assert_eq!(&to, panes[1].dir());
        assert_eq!(
            panes[0].marks_len(),
            2,
            "las marcas del pane no se tocan: la promoción cambia lo que el \
             gesto HACE, no lo que está seleccionado"
        );
        assert!(!marcada(&panes[0], 2), "ni marca la fila arrastrada");
    }

    /// Un arrastre promovido CANCELADO deja las marcas exactamente como
    /// estaban — incluidas las filas que el barrido llegó a marcar antes de
    /// cruzar de pane. Es la mitad del contrato que hace aceptable que un
    /// mismo gesto signifique dos cosas según dónde acabe.
    #[test]
    fn un_arrastre_promovido_cancelado_deja_las_marcas_como_estaban() {
        let mut mouse = MouseState::default();
        let mut panes = panes_con(12);
        let mut focus = 0usize;
        panes[0].set_mark(9, true); // marca previa, ajena al gesto

        let _ = mouse_press(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 2),
            Mods::NONE,
        );
        // Barre 2..=5 de camino…
        let _ = mouse_motion(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 5),
            Mods::NONE,
        );
        assert_eq!(panes[0].marks_len(), 5, "2..=5 + la previa");
        // …y cruza al otro pane: el gesto pasa a ser una transferencia y
        // devuelve lo barrido.
        let _ = mouse_motion(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(1, 3),
            Mods::NONE,
        );
        assert_eq!(panes[0].marks_len(), 1, "solo sobrevive la marca previa");

        // Soltar sobre el cromo (fuera de toda fila) cancela.
        let soltar = mouse_release(&mut mouse, &mut panes, &mut focus, None, Mods::NONE);
        assert!(soltar.transfer.is_none(), "cancelado: no hay drop");
        assert_eq!(panes[0].marks_len(), 1);
        assert!(
            marcada(&panes[0], 9),
            "y es EXACTAMENTE la que había antes del gesto"
        );
    }

    /// Un drop sobre el otro pane con un modal abierto no hace NADA: el
    /// modal caduca el gesto en `render` (`expire_stale_mouse_gesture`), así
    /// que el release ya no encuentra nada armado. `drop_transfer` repite el
    /// guard por si acaso — reemplazar el modal abierto por el del drop
    /// perdería la decisión que el usuario tenía delante.
    #[test]
    fn un_drop_con_un_modal_abierto_no_somete_nada() {
        let mut mouse = MouseState::default();
        let mut panes = panes_con(10);
        let mut focus = 0usize;
        let epochs = [panes[0].listing_epoch(), panes[1].listing_epoch()];
        panes[0].set_mark(2, true);
        expire_stale_gesture(
            &mut mouse,
            MouseValidity {
                epochs,
                hidden: false,
            },
        );
        let _ = mouse_press(
            &mut mouse,
            &mut panes,
            &mut focus,
            Spot::new(0, 2),
            Mods::NONE,
        );
        // Se abre un modal a mitad del arrastre (una task que falla por
        // colisión, p. ej.): el frame siguiente caduca el gesto.
        expire_stale_gesture(
            &mut mouse,
            MouseValidity {
                epochs,
                hidden: true,
            },
        );
        let soltar = mouse_release(
            &mut mouse,
            &mut panes,
            &mut focus,
            Some(Spot::new(1, 4)),
            Mods::NONE,
        );
        assert!(soltar.transfer.is_none(), "el gesto ya no existe");
        assert_eq!(panes[0].marks_len(), 1, "y nada cambió de selección");
    }

    /// El aviso dice las dos cosas que el usuario necesita ANTES de soltar:
    /// cuántas entradas viajan y si va a copiar o a mover. Y cambia con el
    /// shift, que se lee vivo: sin eso la etiqueta prometería una copia
    /// mientras el drop movería.
    #[test]
    fn el_aviso_del_drop_dice_cuantas_y_si_copia_o_mueve() {
        let mut panes = panes_con(10);
        for i in [1usize, 3, 4] {
            panes[0].set_mark(i, true);
        }
        let drop = |move_files, promoted| {
            drop_hint(
                &panes,
                Some(Pending::Drop {
                    from_pane: 0,
                    to_pane: 1,
                    move_files,
                    promoted,
                }),
            )
        };

        let (destino, copiar) = drop(false, None).expect("hay marcas que llevar");
        assert_eq!(destino, 1, "resalta el pane de DESTINO");
        assert!(copiar.contains('3'), "las tres marcas: {copiar:?}");
        let (_, mover) = drop(true, None).expect("hay marcas que llevar");
        assert_ne!(copiar, mover, "copiar y mover no pueden leerse igual");
        assert_eq!(
            copiar,
            norte_i18n::ta(
                "drag-copy",
                &[
                    ("n", "3"),
                    ("to", &norte_frontend::path_display(panes[1].dir()).0)
                ]
            )
        );

        // Promovido: UNA fila, aunque el pane tenga tres marcas — el mismo
        // número que acabará en el modal.
        let (_, promovido) = drop(false, Some(6)).expect("la fila 6 existe");
        assert!(promovido.contains('1'), "una sola fila: {promovido:?}");

        // Sin drop pendiente no se anuncia nada.
        assert!(drop_hint(&panes, None).is_none());
        assert!(drop_hint(&panes, Some(Pending::Marking { pane: 0 })).is_none());
        assert!(drop_hint(&panes, Some(Pending::Carrying { from_pane: 0 })).is_none());
        // Ni cuando no hay nada que llevar (índice que ya no existe).
        assert!(drop(false, Some(99)).is_none());
    }

    // --- Menú contextual del botón derecho (plan de ratón, tarea 4) --------
    //
    // Qué entradas hay y cuál puede correr está clavado en `context_menu`
    // (puro). Lo que se clava aquí es el cableado de la GUI: sobre QUÉ queda
    // apuntando el modelo tras pulsar con el derecho, dónde cabe el panel, y
    // qué texto sale hacia el portapapeles.

    /// La fila pulsada está MARCADA: el menú actúa sobre las marcas, y no
    /// toca ninguna.
    #[test]
    fn el_menu_apunta_a_las_marcas_cuando_la_fila_pulsada_esta_marcada() {
        let mut pane = pane_con(6);
        for i in [1usize, 3, 4] {
            pane.set_mark(i, true);
        }
        let (target, kind) = context_target(&mut pane, 3).expect("la fila 3 existe");
        assert_eq!(target, context_menu::Target::Marks(3));
        assert_eq!(target.count(), 3);
        assert_eq!(kind, EntryKind::File);
        assert_eq!(pane.marks_len(), 3, "las marcas se quedan tal cual");
        assert_eq!(
            pane.marked_paths().len(),
            3,
            "y la op tocaría exactamente esas tres"
        );
    }

    /// La fila pulsada NO está marcada: el menú actúa sobre ella sola, y las
    /// marcas de ese pane se sueltan — si no, el menú diría «1» y la copia se
    /// llevaría once (`marked_paths` prefiere las marcas SIEMPRE).
    #[test]
    fn el_menu_apunta_a_la_fila_sola_cuando_no_esta_marcada() {
        let mut pane = pane_con(12);
        for i in 0..11 {
            pane.set_mark(i, true);
        }
        assert_eq!(pane.marks_len(), 11);

        // El nombre de la fila 11 sale del pane (que ORDENA), nunca de
        // suponer que la entrada 11 se llama «e11».
        let pulsada = pane.entries()[11].path.clone();
        let esperado = row_label(
            pulsada.file_name().map_or(&b""[..], Segment::as_bytes),
            EntryKind::File,
        );
        let (target, _) = context_target(&mut pane, 11).expect("la fila 11 existe");
        assert_eq!(
            target,
            context_menu::Target::Entry(esperado),
            "el objetivo es la fila pulsada"
        );
        assert_eq!(target.count(), 1);
        assert_eq!(pane.marks_len(), 0, "las once marcas se sueltan");
        let paths = pane.marked_paths();
        assert_eq!(paths.len(), 1, "la op tocaría una sola entrada");
        assert_eq!(paths[0], pulsada, "y es la pulsada");
    }

    /// Un índice que ya no nombra ninguna fila (el listado encogió entre el
    /// evento y esto) no abre menú y no toca las marcas.
    #[test]
    fn una_fila_que_ya_no_existe_no_abre_menu() {
        let mut pane = pane_con(3);
        pane.set_mark(0, true);
        assert!(context_target(&mut pane, 9).is_none());
        assert_eq!(pane.marks_len(), 1);
    }

    /// El menú se cierra cuando el listado de SU pane se mueve (un `cd`, un
    /// refresh asíncrono tras una mutación): el objetivo se fijó contra el
    /// listado que había al abrirlo. El del OTRO pane no le incumbe.
    #[test]
    fn el_menu_se_cierra_con_un_listado_nuevo_en_su_pane() {
        let facts = context_menu::facts_for(
            EntryKind::File,
            1,
            context_menu::ReadOnly {
                source: false,
                dest: false,
            },
            true,
        );
        let abierto = || {
            Some(ContextMenu::open(
                1,
                7,
                (0.0, 0.0),
                context_menu::Target::Entry("e0".into()),
                &facts,
            ))
        };

        let mut menu = abierto();
        expire_stale_menu(&mut menu, [3, 7]);
        assert!(menu.is_some(), "su pane no se movió: sigue abierto");
        expire_stale_menu(&mut menu, [3, 9]);
        assert!(menu.is_none(), "su pane relistó: se cierra");

        let mut menu = abierto();
        expire_stale_menu(&mut menu, [99, 7]);
        assert!(menu.is_some(), "el otro pane no le incumbe");
    }

    /// El panel se recoloca para caber entero: pegado al puntero mientras
    /// quepa, desplazado lo justo si no, y jamás con origen negativo.
    #[test]
    fn el_panel_del_menu_se_recoloca_para_caber() {
        let panel = (320.0, 200.0);
        let viewport = (1000.0, 700.0);
        assert_eq!(
            menu_origin((100.0, 100.0), panel, viewport),
            (100.0, 100.0),
            "si cabe, va en el puntero"
        );
        assert_eq!(
            menu_origin((900.0, 650.0), panel, viewport),
            (680.0, 500.0),
            "si no cabe, se desplaza lo justo"
        );
        assert_eq!(
            menu_origin((10.0, 10.0), panel, (200.0, 100.0)),
            (0.0, 0.0),
            "ventana más pequeña que el panel: nunca origen negativo"
        );
    }

    // `el_scheme_de_archivo_es_de_solo_lectura` se fue con la función a
    // `norte_frontend::availability` (H3d tarea 2), donde vive su test: aquí
    // habría probado el crate de al lado a través de un `use`.

    /// El texto que va al portapapeles es WIRE: una ruta por línea, lossless
    /// incluso con un nombre que no es UTF-8 (se reparsea a los MISMOS
    /// bytes), y sin controles crudos. `display_lossy` habría metido un `�`
    /// y la ruta ya no nombraría ningún fichero.
    #[test]
    fn el_portapapeles_lleva_wire_lossless_con_nombres_hostiles() {
        let dir = VPath::parse("mem:///").unwrap();
        let hostil = dir.join(Segment::new(vec![0xFF, 0xFE, b'a']).expect("segmento no vacío"));
        let control = dir.join(Segment::new(b"x\x1b[31my".to_vec()).expect("segmento no vacío"));
        let texto = clipboard_text(&[hostil.clone(), control.clone()]);

        let lineas: Vec<&str> = texto.split('\n').collect();
        assert_eq!(lineas.len(), 2, "una ruta por línea: {texto:?}");
        assert!(
            lineas.iter().all(|l| !l.chars().any(char::is_control)),
            "el wire escapa C0/DEL (el único control es MI separador): {texto:?}"
        );
        assert!(
            !texto.contains('\u{FFFD}'),
            "nada de reemplazos: una ruta con � no nombra ningún fichero"
        );
        assert_eq!(
            VPath::parse(lineas[0]).expect("el wire se reparsea"),
            hostil,
            "round-trip byte-exacto del nombre no-UTF8"
        );
        assert_eq!(
            VPath::parse(lineas[1]).expect("el wire se reparsea"),
            control
        );
    }

    // --- Renombrado in situ (`pane.rename`) --------------------------------

    /// El destino de un rename es el PADRE de la entrada, no el `dir` del
    /// pane, y el nombre nace con los BYTES reales del actual. En un pane
    /// virtual (resultados de búsqueda) el `dir` es la raíz del recorrido:
    /// tomarlo movería el fichero de sitio en vez de renombrarlo.
    #[test]
    fn el_rename_aterriza_en_el_padre_con_los_bytes_del_nombre_actual() {
        let from = VPath::parse("mem:///hondo/sub/%FF%FE.bin").unwrap();
        let Some(super::Modal::RenamePrompt {
            to_dir,
            name,
            error,
            from: f,
        }) = rename_modal_for(&from)
        else {
            panic!("una entrada con padre y nombre abre el modal");
        };
        assert_eq!(f, from);
        assert_eq!(
            to_dir,
            VPath::parse("mem:///hondo/sub").unwrap(),
            "el padre de la ENTRADA, no el dir del pane"
        );
        assert_eq!(
            name.as_slice(),
            b"\xFF\xFE.bin",
            "sembrado con los bytes reales, sin decodificar ni lossy"
        );
        assert!(error.is_none());
    }

    /// Una raíz no tiene nombre ni padre: no hay nada que renombrar y no se
    /// abre modal (jamás un panic).
    #[test]
    fn una_raiz_no_se_renombra() {
        assert!(rename_modal_for(&VPath::parse("mem:///").unwrap()).is_none());
    }

    /// Las claves Fluent del modal de rename son claves REALES en los dos
    /// locales (una errata se pintaría como el propio id, y el test de
    /// paridad de `norte-i18n` no la vería: comprueba que los catálogos
    /// coinciden, no que este fichero los nombre bien).
    #[test]
    fn las_claves_del_modal_de_rename_existen_en_ambos_locales() {
        use norte_i18n::{Lang, t_in};
        for clave in [
            "gui-modal-rename-title",
            "gui-modal-rename-from",
            "gui-modal-rename-to",
            "gui-modal-rename-footer",
            // Compartida con la TUI: mismo mensaje, mismo significado.
            "msg-transfer-name-same",
        ] {
            for lang in [Lang::Es, Lang::En] {
                assert_ne!(t_in(lang, clave), clave, "falta {clave} en {lang:?}");
            }
        }
    }

    /// TODA entrada del menú despacha un comando que el TECLADO también
    /// alcanza (existe en `COMMANDS` y tiene chord en el preset de fábrica):
    /// el menú no puede ser el único camino a una operación, ni inventarse
    /// una que el teclado no tenga.
    #[test]
    fn cada_entrada_del_menu_tiene_equivalente_de_teclado() {
        let facts = context_menu::facts_for(
            EntryKind::File,
            1,
            context_menu::ReadOnly {
                source: false,
                dest: false,
            },
            true,
        );
        let (browse, _) = keymap::build_effectives_preset_only("orthodox");
        let con_chord: std::collections::HashSet<&str> =
            browse.bindings().iter().map(|(_, cmd)| *cmd).collect();
        for item in context_menu::items(&facts) {
            assert!(
                keymap::COMMANDS.contains(&item.command),
                "{:?} no es un comando de la GUI",
                item.command
            );
            assert!(
                con_chord.contains(item.command),
                "{:?} no tiene chord: el menú sería su único camino",
                item.command
            );
        }
    }
}
