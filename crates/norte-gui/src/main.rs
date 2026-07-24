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
    Animation, AnimationExt, AnyElement, App, Bounds, BoxShadow, Context, FocusHandle, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, ParentElement, Pixels, Render, RenderImage,
    ScrollDelta, ScrollStrategy, ScrollWheelEvent, SharedString, Styled, UniformListScrollHandle,
    Window, WindowBounds, WindowOptions, canvas, div, fill, hsla, img, linear_color_stop,
    linear_gradient, point, prelude::*, pulsating_between, px, rgb, rgba, size, uniform_list,
};
use gpui_platform::application;

use std::ops::Range;

use norte_config::ConfirmQuit;
use norte_frontend::settings::PendingWrite;
use norte_frontend::{PaneState, nav::Mode};
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_theme::{FileKind, Role, Theme};

mod effects;
mod keymap;
mod modal;
mod session;
mod settings_view;
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

        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        let ((browse_eff, viewer_eff), mut keymap_error) =
            match keymap::build_effectives(&preset_name) {
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
                    motion_epoch: std::time::Instant::now(),
                    confirm_quit,
                    quick_mode,
                    cfg_snapshot,
                    settings_view: None,
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
                    motion_epoch: std::time::Instant::now(),
                    confirm_quit,
                    quick_mode,
                    cfg_snapshot,
                    settings_view: None,
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
        let sent = self
            .cmds
            .send(SessionCmd::List {
                pane,
                generation,
                dir,
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
                    ViewerContent::PluginStyled { plugin_name, lines } => {
                        Viewer::with_plugin_preview_styled(path, plugin_name, &lines)
                    }
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
            "app.settings" => self.open_settings(),
            "pane.switch" => self.focus = 1 - self.focus,
            "cursor.up" => self.panes[f].cursor_up(),
            "cursor.down" => self.panes[f].cursor_down(),
            "cursor.top" => self.panes[f].home(),
            "cursor.bottom" => self.panes[f].end(),
            "cursor.page-up" => self.panes[f].page_up(PAGE),
            "cursor.page-down" => self.panes[f].page_down(PAGE),
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

    /// Abre la vista de ajustes (`app.settings`, F11, S4): construida
    /// SÍNCRONAMENTE desde `cfg_snapshot` (nunca releyendo disco — regla 2),
    /// mismo criterio que abrir la paleta en la TUI. Reemplaza cualquier
    /// vista anterior con una fresca (sin filtro/edición, igual que F11
    /// repetido en la TUI cerraría y reabriría).
    fn open_settings(&mut self) {
        self.settings_view = Some(settings_view::SettingsView::new(
            norte_frontend::settings::build_rows(&self.cfg_snapshot),
        ));
    }

    /// Maneja UNA tecla con la vista de ajustes abierta (`on_key`, tramo
    /// dedicado): ctrl/alt/platform se descartan ANTES de llegar al filtro
    /// (un ctrl-chord no debe teclearse en el buffer — mismo gate que el
    /// visor aplica solo a platform; aquí se extiende a ctrl/alt porque esta
    /// pantalla SÍ acepta tecleo libre). El resto delega en
    /// [`settings_view::on_key`] (puro) y actúa sobre el
    /// [`settings_view::SettingsOutcome`].
    fn on_settings_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) {
        if ks.modifiers.control || ks.modifiers.alt || ks.modifiers.platform {
            return;
        }
        let Some(view) = &mut self.settings_view else {
            return;
        };
        let outcome = settings_view::on_key(view, &ks.key, ks.key_char.as_deref());
        match outcome {
            settings_view::SettingsOutcome::None => {}
            settings_view::SettingsOutcome::Close => self.settings_view = None,
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
                    let persisted = norte_config::persist_set(&dir, section, &key_for_bg, value);
                    let fresh_cfg = persisted
                        .is_ok()
                        .then(|| norte_frontend::config::load(&norte_config::standard_layers()));
                    let fresh_keymap = match (&fresh_cfg, needs_keymap_rebuild) {
                        (Some(Ok(cfg)), true) => Some(keymap::build_effectives(&cfg.common.preset)),
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

    /// Reemplaza `resolver`/`viewer_resolver` con los efectivos frescos
    /// (S4, tras `keymap.preset` con OK): `fresh_keymap` ya viene calculado
    /// desde el hilo de fondo (`commit_settings_write`, evita releer
    /// `keymap.toml` en el hilo de UI). Un preset roto/capa de usuario
    /// inválida en el momento del commit CONSERVA el resolver vigente
    /// (nunca deja la GUI sin bindings) y devuelve `false`.
    fn apply_keymap_live(
        &mut self,
        fresh_keymap: Option<
            Result<
                (
                    norte_frontend::keymap::Effective,
                    norte_frontend::keymap::Effective,
                ),
                norte_frontend::keymap::KeymapError,
            >,
        >,
    ) -> bool {
        match fresh_keymap {
            Some(Ok((browse, viewer))) => {
                self.resolver = norte_frontend::keymap::Resolver::new(browse);
                self.viewer_resolver = norte_frontend::keymap::Resolver::new(viewer);
                true
            }
            _ => false,
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
            view.state
                .refresh(norte_frontend::settings::build_rows(&self.cfg_snapshot));
        }
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

        // Vista de ajustes abierta (F11, S4): captura fija, mismo criterio de
        // prioridad que el modal — gana incluso sobre el visor (ver doc del
        // campo `settings_view`).
        if self.settings_view.is_some() {
            self.on_settings_key(ks, cx);
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
            cx.processor(move |this, range: Range<usize>, window, cx| {
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
                        // `window` (G2 decisión 3): `render_row` lo necesita
                        // para `is_window_active()` — el blink de cursor solo
                        // se anima con la ventana enfocada (`with_animation`
                        // de GPUI NO lo comprueba solo; ver doc de
                        // `render_row`).
                        this.render_row(i, j, &e, hl, marked, &chrome_owned, window, cx)
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
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
            .px(px(sp::S))
            .py(px(1.0)) // sub-XS: acento fino de una línea, fuera de la escala a propósito
            // Redondeo sutil (GP): constante en TODAS las filas en vez de
            // condicionarlo a selección/hover — más barato (un solo estilo,
            // sin ramas) y visualmente inapreciable en una fila sin fondo.
            .rounded(px(sp::RADIUS_ROW))
            .cursor_pointer()
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
        let row = row.on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
                this.on_row_click(pane, idx, dir_target.clone(), ev.click_count, cx);
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
        TaskKind::Index => "gui-task-kind-index",
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

        // Vista de ajustes (F11, S4) a pantalla completa, visor (F3), el
        // estado «abriendo…» mientras llega, o el dual-pane: pantallas
        // mutuamente excluyentes (ver `on_key`, que las enruta con la misma
        // prioridad: ajustes > visor > dual-pane).
        if self.settings_view.is_some() {
            root = root.child(self.render_settings(&chrome, cx));
        } else if self.viewer.is_some() {
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
                .gap(px(sp::XS))
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
        // La vista de ajustes no tiene un `Resolver` de secuencias (edita
        // tecla a tecla, `settings_view::on_key`) — sin indicador aquí
        // mientras está abierta.
        let active_resolver = if self.viewer.is_some() {
            &self.viewer_resolver
        } else {
            &self.resolver
        };
        let pending = if self.settings_view.is_some() {
            &[][..]
        } else {
            active_resolver.pending()
        };
        if !pending.is_empty() {
            root = root.child(
                div()
                    .px(px(sp::S))
                    .py(px(1.0)) // sub-XS: acento fino de una línea, fuera de la escala a propósito
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
        ChromeColors, ConfirmQuit, FontSet, ImagePreview, affected_dirs, apply_viewer_command,
        banner_safe, confirm_quit_should_open, confirm_quit_task_count, first_cancelable,
        flicker_factor, flicker_scale, generation_is_current, glowed, has_pending_work,
        image_preview_from, image_status, keymap_error_detail, modal_footer_colors,
        modal_panel_colors, modal_title_colors, motion_active, pending_hint, retain_active,
        row_label, styled_span_color, task_at_cursor, theme_map, unknown_preset_banner,
        validated_family, viewer_header, viewer_status,
    };
    use gpui::rgb;
    use norte_frontend::viewer::Viewer;
    use norte_proto::{EntryKind, VPath};
    use norte_theme::Theme;

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
}
