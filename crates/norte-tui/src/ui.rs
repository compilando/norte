//! Render ratatui del estado (`app`): cero lógica de negocio — pinta lo que
//! hay. El marcado de nombres hostiles sigue la spec §6 (lossy y MARCADO). Los
//! colores salen del tema resuelto (`app.theme`, ADR 0020): un frontend sin
//! tema ve el fallback monocromo de M1.

use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::app::{App, display_name};
use crate::theme::TuiTheme;
use norte_i18n::{t, ta};

mod chrome;
mod compare;
mod geometry;
mod help;
mod modals;
mod overlays;
mod pane;
mod panels;
mod pickers;
mod status;
mod sync;
mod text;

// `tests/`, `mouse.rs` y `event_loop.rs` nombran todo esto por `ui::..`, asi
// que es la API de este modulo y no baja a `pub(crate)`.
pub use chrome::{
    MenuHit, MenuZone, PanelZone, TabAction, TabZone, menu_zones, panel_zones, tab_zones,
};
pub use compare::draw_compare;
pub use geometry::{
    before_frame, pane_geometry, pane_list_rows, panel_slots, resize_borders, resolved_for,
    tab_strip_for,
};
pub use help::{draw_help, help_body_size, help_group_is_painted, help_layout, help_sidebar_width};
pub use overlays::{draw_shortcuts, draw_which_key, plugin_description_line};
pub use pane::painted_len_and_selection;
pub use panels::{PlaceZone, TreeZone, places_zones, tree_zones};
pub use pickers::draw_theme_picker;
pub use text::fit_hint_groups;

pub(crate) use chrome::{TARGET_BADGE, TabStrip, draw_tab_strip};
use chrome::{draw_menu, draw_panel_bar};
pub(crate) use geometry::{
    body_rect, centered, chrome_body, pane_cols, placed_of_kind, resolved_frame, slot_rect,
};
use modals::draw_modal;
use overlays::{draw_extensions, draw_palette, draw_plugin_config_panel, draw_settings};
use pane::draw_pane;
use panels::{
    draw_log, draw_metadata, draw_places, draw_preview, draw_processes, draw_tasks, draw_tree,
    draw_viewer,
};
use pickers::{
    draw_columns_picker, draw_connections_picker, draw_layout_picker, draw_profile_picker,
};
use status::draw_status;
use sync::draw_sync;

/// Badge de nombre hostil: PREFIJO en columna fija (al final moriría en el
/// truncado por ancho de ratatui y el nombre se pintaría "limpio") y en
/// ASCII (`⚠` es ambiguous-width: 2 celdas en muchos terminales). Va
/// estilado (rol `hostile-badge`) — fuera de banda: un archivo llamado "! x"
/// no lo imita. EXCEPCIÓN documentada: los popups de navegación llevan el
/// badge in-band dentro del display del item (como los títulos de modal);
/// un favorito llamado "! x" puede imitarlo — superficie de solo-lectura
/// propia del usuario, riesgo aceptado.
/// `pub` desde S4 (#135): el binario (`main.rs`, otra crate) compone la línea
/// `msg-shell-remote` con la ruta ya saneada, y un literal `"!"` copiado allí
/// sería un segundo badge que puede desincronizarse de este.
pub const HOSTILE_BADGE: &str = "!";

/// Estilo BASE del tema: fondo de [`Role::Background`] más el frente de
/// [`Role::Regular`]. Es lo que hace que un span SIN `fg` propio
/// (`Span::raw`/`Line::raw`, o un `Modifier::DIM` a secas) herede el frente
/// del TEMA y no el del TERMINAL — con un tema claro en un terminal oscuro
/// eso último pinta texto casi del color del fondo. Un tema sin `background`
/// no fija frente base: se queda con el del terminal, que es el que le pega.
fn base_style(theme: &TuiTheme) -> ratatui::style::Style {
    let base = theme.role(Role::Background);
    if base.bg.is_some() {
        base.patch(theme.role(Role::Regular))
    } else {
        base
    }
}

/// `Clear` + repintado de la base del tema sobre `area`: el widget `Clear` de
/// ratatui deja las celdas en el estilo POR DEFECTO (frente y fondo del
/// terminal), así que un overlay que solo hace `Clear` pierde el fondo Y el
/// frente del tema, y su texto sin `fg` vuelve a caer al del terminal.
fn clear_themed(frame: &mut Frame<'_>, area: Rect, theme: &TuiTheme) {
    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(Block::default().style(base_style(theme)), area);
}

/// El cuerpo del frame: los dos panes —o el panel que los sustituye—, la
/// franja de tareas y la barra de estado.
///
/// Aparte de [`draw`] porque un reparto, dos ramas de sustitución y tres
/// pintados no caben en una función que además monta todos los overlays.
fn draw_body(frame: &mut Frame<'_>, app: &App) {
    // UN reparto por frame: de él salen el cuerpo, los dos panes, la
    // franja de tareas y la barra de estado.
    let res = resolved_frame(app, frame.area());
    let body = body_rect(&res, &app.layout).unwrap_or_else(|| chrome_body(app, frame.area()));
    let tasks_area = slot_rect(&res, crate::panel::SLOT_TASKS).unwrap_or(Rect {
        x: body.x,
        y: body.y.saturating_add(body.height),
        width: body.width,
        height: 0,
    });
    let status_area = slot_rect(&res, crate::panel::SLOT_STATUS).unwrap_or(Rect {
        x: body.x,
        y: frame.area().height.saturating_sub(1),
        width: body.width,
        height: 1,
    });
    let cols = pane_cols(&res, &app.layout);
    // #108 L5: `now` de las celdas de tiempo relativo — UNA lectura por
    // frame; los tests lo fijan (`App::render_now_ms`) para snapshots
    // estables.
    let now_ms = app.now_ms();

    // El panel de diferencias ocupa el sitio de los DOS panes: una fila
    // tiene dos caras y un veredicto en medio, así que no cabe en media
    // pantalla. La franja de tasks y la barra se quedan debajo, aunque la
    // comparación no entre en el `TaskBoard` (igual que la búsqueda viva:
    // su progreso lo pinta el pie del propio panel) — lo que se ve ahí
    // debajo son las OTRAS tasks, que siguen corriendo.
    if let Some(view) = &app.sync {
        // Encima del de diferencias, que sigue vivo detrás con sus marcas:
        // el plan es lo que hay que mirar mientras se decide, y volver a
        // las filas es cerrar el plan.
        draw_sync(frame, body, view, &app.theme);
    } else if let Some(view) = &app.compare {
        draw_compare(frame, body, view, &app.theme, &app.compare_size_hints);
    } else {
        for (i, pane) in app.panes.iter().enumerate() {
            // Un pane que el reparto no colocó no se pinta: el `Split`
            // colapsó y su sitio lo ocupa entero el otro.
            let Some(rect) = cols.get(i).copied() else {
                continue;
            };
            draw_pane(
                frame,
                rect,
                pane,
                app.focus() == i,
                &app.theme,
                now_ms,
                &app.columns,
                // #117 tarea 2: el catálogo cacheado del scheme del pane (hints
                // y cabeceras); sin él se pinta con defaults, jamás se espera.
                app.attr_catalog(pane.dir().scheme()),
                tab_strip_for(app, i).as_ref(),
                // Solo a partir de TRES paneles: con dos, el destino es el
                // otro y el marcador sería ruido en el caso de siempre.
                app.panes.len() > 2 && app.target_index() == Some(i),
                // La espera, solo si es de ESTE panel y ya pasa del umbral: un
                // trabajo de sesión no puede poner a girar una cabecera a la
                // que no le está pasando nada.
                app.busy.as_ref().filter(|b| b.visible() && b.affects(i)),
            );
        }
    }
    // El sidebar va DESPUÉS de los listados y antes del cromo de abajo: su
    // hueco sale del mismo reparto, así que si no se colocó —cerrado, o
    // colapsado por falta de sitio— aquí no hay nada que hacer.
    if let Some((id, rect)) = placed_of_kind(&res, &app.layout, "places")
        && let Some(state) = app.panes.places(id)
    {
        draw_places(
            frame,
            rect,
            state,
            app.key_owner() == crate::app::KeyOwner::Places,
            &app.theme,
        );
    }
    if let Some((id, rect)) = placed_of_kind(&res, &app.layout, crate::preview::KIND)
        && let Some(p) = app.panes.preview(id)
    {
        draw_preview(
            frame,
            rect,
            p,
            app.key_owner() == crate::app::KeyOwner::Preview,
            app,
        );
    }
    if let Some((id, rect)) = placed_of_kind(&res, &app.layout, crate::processes::KIND)
        && let Some(p) = app.panes.processes(id)
    {
        draw_processes(
            frame,
            rect,
            p,
            app,
            app.key_owner() == crate::app::KeyOwner::Processes,
        );
    }
    // El registro no lleva estado POR HUECO —hay uno, y su nivel y su filtro
    // son de la sesión— así que basta el rectángulo donde cayó.
    if let Some((_, rect)) = placed_of_kind(&res, &app.layout, crate::logview::KIND) {
        draw_log(
            frame,
            rect,
            app,
            app.key_owner() == crate::app::KeyOwner::Log,
        );
    }
    if let Some((id, rect)) = placed_of_kind(&res, &app.layout, crate::tree::KIND)
        && let Some(t) = app.panes.tree(id)
    {
        draw_tree(
            frame,
            rect,
            t,
            app,
            app.key_owner() == crate::app::KeyOwner::Tree,
        );
    }
    if let Some((id, rect)) = placed_of_kind(&res, &app.layout, crate::metadata::KIND)
        && let Some(e) = app.panes.metadata(id)
    {
        // Sin borde de foco NUNCA: la hoja no toma el teclado, y un borde
        // resaltado sobre un panel que no lee ninguna tecla era la mitad
        // visible del control que no hacía lo que decía (#243).
        draw_metadata(frame, rect, e.as_ref(), app, false);
    }
    draw_tasks(frame, tasks_area, app);
    draw_status(frame, status_area, app);
}

/// Pinta el frame completo: panes (o viewer) + panel de tasks + barra de
/// estado + modal por encima.
pub fn draw(frame: &mut Frame<'_>, app: &App) {
    // Fondo BASE del tema (ADR 0020): se pinta primero; los estilos de texto
    // (solo fg) lo conservan. Sin `background` en el tema = fondo del terminal.
    //
    // El FRENTE de `Regular` va en la misma base: un span sin `fg` propio
    // (`Span::raw`/`Line::raw`, o un `Modifier::DIM` a secas como el de las
    // celdas de columna y la cabecera) hereda el frente por defecto del
    // TERMINAL, que no tiene por qué pegar con el fondo del TEMA — con un
    // tema claro en un terminal oscuro salía texto casi del color del fondo
    // (Tamaño/Fecha/Tipo, la cabecera de columnas y los cuerpos de los
    // overlays, invisibles). Pintarlo aquí lo arregla para TODO el frame de
    // una vez, sin tocar cada span: quien quiera otro color sigue fijando el
    // suyo. Un tema sin `background` tampoco pinta frente base (se queda con
    // el del terminal, que es el que hace juego).
    frame.render_widget(Block::default().style(base_style(&app.theme)), frame.area());
    // El viewer sustituye a los panes, NUNCA a los overlays: antes este
    // brazo hacía `return` y CUALQUIER overlay abierto con el viewer
    // encima quedaba invisible aunque el run loop ya le hubiera dado la
    // tecla (su brazo va ANTES del viewer en la cadena) — la ayuda (F1),
    // el selector de tema, los ajustes, la palette y hasta un modal
    // asíncrono de aprobación se comían el teclado sin pintar un píxel:
    // el viewer parecía colgado y F1 "dejaba de funcionar". Los píxeles
    // deben decir quién manda (mismo criterio que el modal pintado el
    // último, más abajo).
    if let Some(viewer) = &app.viewer {
        draw_viewer(frame, viewer, app);
    } else {
        draw_body(frame, app);
    }
    // La barra se pinta si está FIJADA (aunque el menú esté cerrado: para eso
    // está, para que se vea que hay un menú) o si el menú está abierto.
    if app.menu_bar || app.menu.is_some() {
        draw_menu(frame, app);
    }
    // #324: y la fila de paneles debajo. Después del cuerpo por lo mismo que
    // el menú: es cromo, y el cuerpo ya se repartió el sitio que le queda.
    draw_panel_bar(frame, app);
    if let Some(help) = &app.help {
        draw_help(frame, help, &app.theme, &app.dialog_hints.help);
    }
    if let Some(picker) = &app.theme_picker {
        draw_theme_picker(frame, picker, &app.theme, &app.dialog_hints.picker);
    }
    if let Some(p) = &app.columns_picker {
        draw_columns_picker(frame, p, &app.theme, &app.dialog_hints.columns);
    }
    // Fase A: el selector de disposiciones, con el mismo allowlist de teclas
    // que el de temas (`ALLOW_PICKER`) y por eso el mismo hint.
    if let Some(p) = &app.profile_picker {
        draw_profile_picker(frame, p, &app.theme, &app.dialog_hints.picker);
    }
    if let Some(p) = &app.layout_picker {
        draw_layout_picker(frame, p, &app.theme, &app.dialog_hints.picker);
    }
    // #140: el selector de conexiones, mismo allowlist y mismo hint que los
    // otros dos — es una lista con cursor que no muta nada.
    if let Some(p) = &app.connections_picker {
        draw_connections_picker(frame, p, &app.theme, &app.dialog_hints.picker);
    }
    if let Some(mgr) = &app.extensions {
        if let Some(panel) = &mgr.config {
            draw_plugin_config_panel(frame, panel, &app.theme, &app.dialog_hints.plugin_config);
        } else {
            draw_extensions(frame, mgr, &app.theme, &app.dialog_hints.extensions);
        }
    }
    if let Some(popup) = &app.nav_popup {
        draw_nav_popup(frame, popup, &app.theme, &app.dialog_hints);
    }
    if let Some(dialog) = &app.search_dialog {
        draw_search_dialog(
            frame,
            dialog,
            app.focused().dir(),
            app.focused().name_encoding(),
            &app.theme,
        );
    }
    if let Some(palette) = &app.palette {
        draw_palette(frame, palette, &app.theme);
    }
    if let Some(settings) = &app.settings {
        draw_settings(frame, settings, &app.theme);
    }
    // K3c: el editor de atajos se abre DESDE ajustes y se pinta encima, con
    // el overlay de ajustes abierto detrás — es una pantalla suya, no un
    // reemplazo, y al cerrarla el lector vuelve donde estaba. También se queda
    // las teclas antes que él (`main`), así que los píxeles y el enrutado
    // dicen lo mismo.
    if let Some(sc) = &app.shortcuts {
        draw_shortcuts(frame, sc, &app.theme);
    }
    // K3a: el panel which-key va tras los overlays y ANTES del modal. Es el
    // único que no se queda ninguna tecla —el resolver del pane las conserva
    // mientras está arriba—, así que no compite por el teclado con nada de lo
    // de arriba; se pinta encima porque describe la secuencia que el lector
    // está tecleando AHORA, y taparla con un overlay abierto antes sería
    // esconder la respuesta a la pregunta que acaba de hacer.
    //
    // CON UN MODAL ABIERTO NO se pinta, y esta es la excepción que confirma lo
    // anterior: un modal puede abrirse SOLO (una aprobación de policy que
    // llega por el bus, una colisión al terminar una copia) sin que nadie haya
    // tocado una tecla, y a partir de ahí las teclas van al `dialog_resolver`.
    // Un panel que siguiera diciendo «g → ir arriba» junto a un diálogo que se
    // queda la `g` es exactamente la mentira de píxeles que documenta el
    // comentario del modal, más abajo. La secuencia sigue viva en el resolver
    // del pane (el modal no la cancela, como no cancela el `[g …]` de la
    // barra): se vuelve a ver al cerrarse el diálogo.
    if let Some(wk) = &app.which_key
        && app.modal.is_none()
    {
        draw_which_key(frame, wk, &app.theme);
    }
    // Revisión S, M3: el modal se pinta ÚLTIMO, por encima de CUALQUIER otro
    // overlay — el enrutado de teclas ya lo trata como AUTORITATIVO en
    // presencia de la palette o el overlay de ajustes (`modal_preempts_
    // palette`/`modal_preempts_settings`, `main.rs`: un modal en vuelo p.ej.
    // una aprobación de policy async SIEMPRE gana la tecla). Antes se
    // pintaba justo tras la barra de estado, así que cualquier overlay
    // posterior en esta lista lo TAPABA visualmente — los píxeles mentían
    // sobre quién manda. Cierra la clase de H1 MINOR-4 (aceptada entonces
    // solo para la palette) para AMBOS overlays.
    if let Some(modal) = &app.modal {
        // H3c: con una página de ayuda ABIERTA ENCIMA (`over_modal`), la ayuda
        // se queda las teclas y los verbos del modal son INERTES. El pie deja
        // de ofrecerlos y dice lo que es verdad (`with_modals_inert`): un
        // `[y] aprobar [n] denegar` que no hace nada es la misma mentira que
        // `hints.rs` existe para que un rebind no pueda contar. La caja y la
        // pregunta NO se tocan — se siguen pintando aquí, las últimas, encima
        // de la página.
        let inert = app
            .help
            .as_ref()
            .is_some_and(|help| help.over_modal)
            .then(|| app.dialog_hints.with_modals_inert());
        draw_modal(
            frame,
            modal,
            &app.theme,
            app.focused().name_encoding(),
            inert.as_ref().unwrap_or(&app.dialog_hints),
        );
    }
}

/// Diálogo de búsqueda viva (`Alt+F7`, liveSearch T6): dos campos de texto
/// (nombre/contenido) con un `_` en el activo, los dos toggles regex/case y la
/// raíz del walk (el `cwd` del pane, no editable) — todo saneado, jamás
/// bidi/controles crudos (los campos pasan por [`display_name`], la raíz por
/// [`path_display`]; un paste hostil no pinta invisibles en el borde).
fn draw_search_dialog(
    frame: &mut Frame<'_>,
    dialog: &crate::app::SearchDialog,
    root: &norte_proto::VPath,
    enc: Option<norte_encoding::NameEncoding>,
    theme: &TuiTheme,
) {
    use crate::app::SearchField;
    let on_txt = |b: bool| if b { t("on-yes") } else { t("on-no") };
    let field = |label: &str, value: &str, active: bool| {
        let (masked, _) = display_name(value.as_bytes());
        let cursor = if active { "_" } else { "" };
        format!("{label} {masked}{cursor}")
    };
    let name_active = dialog.field == SearchField::Name;
    // #98/F4: la raíz del walk es superficie de decisión — sigue la
    // reinterpretación del pane (la barra de abajo pinta el mismo dir así).
    let (root_txt, root_hostile) = norte_frontend::path_display_with(root, enc);
    let root_line = if root_hostile {
        format!("{HOSTILE_BADGE} {root_txt}")
    } else {
        root_txt
    };
    let body = [
        field(&t("search-name"), &dialog.name, name_active),
        field(&t("search-content"), &dialog.content, !name_active),
        ta("search-regex", &[("on", &on_txt(dialog.regex))]),
        ta("search-case", &[("on", &on_txt(dialog.case))]),
        middle_ellipsis(&root_line, 56),
        t("search-hint"),
    ]
    .join("\n");
    let area = centered(frame.area(), 60, 8);
    clear_themed(frame, area, theme);
    frame.render_widget(
        Paragraph::new(body).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", t("search-title")))
                .title_style(theme.role(Role::Title))
                .border_style(theme.role(Role::ModalBorder)),
        ),
        area,
    );
}

/// Popup de navegación (spec 2026-07-18): historial `Alt+↓` / hotlist
/// `Ctrl+D` / volúmenes `Alt+F1`/`Alt+F2` (design 2026-08-10 §D), calcando
/// [`draw_theme_picker`]. Los items llegan YA saneados de
/// [`crate::app::App::open_nav_popup`]/[`crate::app::App::open_volumes_popup`]
/// — aquí solo se pintan. El footer de teclas solo aplica a hotlist (`a`/`d`)
/// y volúmenes (el toggle "mostrar todo", más el modo actual); con el input
/// de nombre activo lo sustituye la línea `nombre: …` (el input pasa por el
/// MISMO mask que la query del quick search: un paste hostil no pinta bidi
/// crudo). `hints` trae el hint GENERADO de cada kind
/// (`app.dialog_hints.nav_list`/`.nav_volumes`, H1 T3/#24 y design §D) — el
/// historial no pinta footer, igual que antes de H1.
fn draw_nav_popup(
    frame: &mut Frame<'_>,
    popup: &crate::app::NavPopup,
    theme: &TuiTheme,
    hints: &crate::hints::DialogHints,
) {
    use crate::app::NavPopupKind;
    let title = match popup.kind {
        NavPopupKind::History => t("history-title"),
        NavPopupKind::Hotlist => t("hotlist-title"),
        NavPopupKind::Volumes => t("volumes-title"),
    };
    // El footer se construye ANTES para dimensionar el popup con su ancho
    // REAL (celdas unicode vía `Line::width`, no bytes): 64 de mínimo — el
    // footer de teclas de hotlist en ES son 60 celdas y a 60 el borde lo
    // truncaría («cerra…») — y crece si el footer (p.ej. un nombre largo en
    // el input) lo necesita.
    let footer: Option<Line<'_>> = if let Some(input) = &popup.name_input {
        let (masked, _) = display_name(input.as_bytes());
        Some(Line::raw(format!(
            " {} {masked}_ ",
            t("hotlist-name-prompt")
        )))
    } else if popup.kind == NavPopupKind::Hotlist {
        Some(Line::raw(format!(" {} ", hints.nav_list)))
    } else if popup.kind == NavPopupKind::Volumes {
        // design §D: el footer dice en qué MODO está la lista, no solo qué
        // teclas hay — un toggle sin indicador deja al lector adivinando si
        // ya lo pulsó.
        let mode = if popup.include_pseudo() {
            t("volumes-mode-all")
        } else {
            t("volumes-mode-filtered")
        };
        Some(Line::raw(format!(" {mode} — {} ", hints.nav_volumes)))
    } else {
        None
    };
    let footer_w = footer.as_ref().map_or(0, Line::width);
    let width = u16::try_from(footer_w.saturating_add(4))
        .unwrap_or(u16::MAX)
        .max(64);
    let rows = u16::try_from(popup.items().len().max(1)).unwrap_or(8) + 2;
    let area = centered(frame.area(), width, rows.min(frame.area().height.max(3)));
    clear_themed(frame, area, theme);
    // Items largos: elipsis MEDIA (cabeza + cola, como los modales de
    // rutas) al ancho interior — el truncado derecho de ratatui haría
    // indistinguibles dos rutas con prefijo común (BAJA-3).
    let inner = usize::from(area.width.saturating_sub(3));
    let (items, selected): (Vec<ListItem<'_>>, Option<usize>) = if popup.items().is_empty() {
        let empty = match popup.kind {
            NavPopupKind::History => t("history-empty"),
            NavPopupKind::Hotlist => t("hotlist-empty"),
            NavPopupKind::Volumes => t("volumes-empty"),
        };
        (vec![ListItem::new(Line::raw(format!(" {empty}")))], None)
    } else {
        (
            popup
                .items()
                .iter()
                .map(|it| {
                    ListItem::new(Line::raw(format!(
                        " {}",
                        middle_ellipsis(&it.display, inner)
                    )))
                })
                .collect(),
            Some(popup.cursor()),
        )
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(Role::ModalBorder));
    if let Some(footer) = footer {
        block = block.title_bottom(footer);
    }
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.role(Role::Selection));
    let mut state = ListState::default();
    state.select(selected);
    frame.render_stateful_widget(list, area, &mut state);
}

/// Elipsis MEDIA por ancho de celda: AHORA vive en `norte-frontend`
/// (encoding audit M4-IA-2 H1) — el invariante «un path kilométrico jamás
/// expulsa el campo que va detrás» no es propio de un terminal, la GUI lo
/// necesitaba igual. Re-import local para que todo el módulo (y sus tests)
/// la llame por su nombre corto, sin cambiar una sola salida de render.
use norte_frontend::middle_ellipsis;
