//! Render ratatui del estado (`app`): cero lógica de negocio — pinta lo que
//! hay. El marcado de nombres hostiles sigue la spec §6 (lossy y MARCADO). Los
//! colores salen del tema resuelto (`app.theme`, ADR 0020): un frontend sin
//! tema ve el fallback monocromo de M1.

use norte_proto::EntryKind;
use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{AI_RENAME_PAIR_LIMIT, App, Pane, SEMANTIC_HIT_LIMIT, display_name};
use crate::theme::TuiTheme;
use norte_i18n::{t, ta};

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

/// Presupuesto en CHARS de una ruta dentro de un modal, antes de la elipsis
/// media. El mismo que ya usaban el modal de aprobación y el de colisión:
/// `modal_width` crece hasta el ancho del frame, así que el recorte lo pone
/// el contenido — jamás el borde de la caja, que corta a pelo.
const MODAL_PATH_CHARS: usize = 46;

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

/// Filas del panel de tasks en un frame (tope 6): parte del layout de
/// [`draw`], extraída para que [`pane_list_rows`] cuente lo MISMO que se
/// pinta.
fn tasks_rows(app: &App) -> u16 {
    u16::try_from(app.board.rows().len().min(6)).unwrap_or(6)
}

/// Filas de LISTADO que cada pane pinta en un frame de alto `frame_height`
/// (#124): el alto del frame menos el panel de tasks y la barra de estado
/// (layout de [`draw`]), menos los dos bordes del bloque del pane y su línea
/// de cabecera de columnas (`draw_pane`, privado). El run loop la devuelve al
/// modelo (`PaneState::set_viewport_rows`) para que la paginación y la sonda
/// de stat dejen de adivinar el viewport. Un test de render la ancla contra
/// las filas que aparecen de verdad en el buffer — si el layout cambia, ese
/// test cae aquí.
#[must_use]
pub fn pane_list_rows(app: &App, frame_height: u16) -> u16 {
    // Con el visor abierto no se pinta ningún pane: 0 filas visibles.
    if app.viewer.is_some() {
        return 0;
    }
    frame_height
        .saturating_sub(tasks_rows(app))
        .saturating_sub(1) // barra de estado
        .saturating_sub(3) // bordes del bloque (2) + cabecera de columnas (1)
}

/// Primer índice PINTADO de un listado de `total` items con `selected`
/// seleccionado en un área de `height` filas.
///
/// Existe para que el DRAW y el HIT TEST del ratón ([`pane_geometry`])
/// compartan UNA sola fuente del scroll. `draw_pane` se lo pasa al
/// `ListState` en vez de dejar que ratatui lo deduzca: la deducción de
/// ratatui coincide hoy con esta fórmula (arranca del offset del estado —
/// 0 en un `ListState` nuevo — y baja lo justo para que la selección
/// entre), pero si algún día dejara de coincidir, el ratón resolvería
/// clicks contra un scroll que la pantalla no tiene, y eso no se ve: se
/// marca el fichero de al lado.
///
/// El TUI no guarda scroll independiente del cursor — el listado se
/// desplaza porque el cursor se sale de la ventana, y por eso el cursor
/// acaba pegado al borde inferior en cuanto se pasa de la primera página.
/// Es también lo que hace que la rueda (que mueve el cursor) desplace el
/// listado.
#[must_use]
fn list_offset(selected: Option<usize>, total: usize, height: u16) -> usize {
    let height = usize::from(height);
    let (Some(selected), true) = (selected, height > 0) else {
        return 0;
    };
    // `min(total-1)` calca el clamp de ratatui: un selected fuera de rango
    // no debe pintar (ni resolver) una ventana vacía.
    let selected = selected.min(total.saturating_sub(1));
    selected.saturating_sub(height - 1)
}

/// Lo que hay que hacer al MODELO justo antes de pintar un frame de `height`
/// filas: dejar la ventana de cada pane lista.
///
/// Va aquí y no suelto en el run loop porque los tests pintan por su cuenta y
/// tienen que pasar por lo mismo — si esto vive solo en el bucle, un test
/// pinta con una ventana que nadie reconcilió y comprueba una pantalla que
/// ningún usuario ve.
///
/// ANTES del draw y no después: el cursor ya está donde lo dejó la tecla, así
/// que esto decide qué filas se ven y el draw las pinta. Al revés costaba un
/// frame de retraso, y el frame retrasado es justo el que el usuario mira
/// cuando el cursor toca el borde.
pub fn before_frame(app: &mut App, height: u16) {
    let filas = usize::from(pane_list_rows(app, height));
    for pane in &mut app.panes {
        pane.reconcile_viewport(filas);
    }
}

/// La geometría PINTADA de los dos panes en un frame de `area`, o `None`
/// cuando este frame no pinta panes (visor abierto).
///
/// Mismo trato que [`pane_list_rows`] (#124): el draw es quien sabe dónde
/// cayó cada cosa, así que el run loop devuelve esto al modelo
/// ([`crate::mouse::after_frame`]) tras cada frame y el ratón resuelve sus
/// clicks contra la ÚLTIMA pantalla que el usuario vio, no contra una
/// recalculada a ojo. Se computa aquí, junto al layout que replica, para
/// que cambiarlo rompa el test de geometría de al lado y no el ratón en
/// silencio.
///
/// Las filas de un pane, de arriba abajo: borde superior (1), cabecera de
/// columnas (1), el listado, borde inferior (1). Las columnas: borde
/// izquierdo (1), contenido, borde derecho (1). Todo lo que no sea listado
/// es CROMO, y un click ahí resuelve a «este pane, ninguna fila».
#[must_use]
pub fn pane_geometry(app: &App, area: Rect) -> Option<[crate::mouse::PaneGeometry; 2]> {
    // Ni con el visor ni con el panel de diferencias: los dos sustituyen a
    // los panes, y una geometría de algo que no está pintado es un click
    // resuelto contra una fila que el lector no puede ver.
    if app.viewer.is_some() || app.compare.is_some() {
        return None;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(tasks_rows(app)),
            Constraint::Length(1),
        ])
        .split(area);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[0]);
    let mut out = [crate::mouse::PaneGeometry::default(); 2];
    for (i, pane) in app.panes.iter().enumerate() {
        let block = cols[i];
        // Interior del bloque con `Borders::ALL`, sin construir el bloque:
        // un margen de 1 por lado. `title_bottom` (el input del quick
        // search) NO consume filas — se pinta sobre el borde inferior.
        let inner_w = block.width.saturating_sub(2);
        let inner_h = block.height.saturating_sub(2);
        // La cabecera de columnas se come la primera fila del interior.
        let list_rows = inner_h.saturating_sub(1);
        out[i] = crate::mouse::PaneGeometry {
            x: block.x,
            y: block.y,
            width: block.width,
            height: block.height,
            first_list_row: block.y.saturating_add(2),
            list_rows: if inner_w == 0 || inner_h == 0 {
                0
            } else {
                list_rows
            },
            // La ventana la decide el MODELO (pegajosa), y el hit test lee
            // exactamente la misma que se pintó: deducirla aquí otra vez es
            // como se resuelve un click contra la fila de al lado.
            offset: pane.viewport_offset(),
        };
    }
    Some(out)
}

/// Cuántos items pinta un pane y cuál va resaltado, EN COORDENADAS DE LO
/// PINTADO (posición dentro del filtro cuando hay quick search en modo
/// filtro, índice absoluto si no). Lo comparten `draw_pane` y
/// [`pane_geometry`] para que el scroll salga del mismo cálculo.
fn painted_len_and_selection(pane: &Pane) -> (usize, Option<usize>) {
    match pane.quick_visible() {
        Some(vis) => (
            vis.len(),
            pane.quick()
                .and_then(crate::nav::QuickSearch::selected_entry_index)
                .and_then(|s| vis.iter().position(|&i| i == s)),
        ),
        None => (
            pane.entries().len(),
            (!pane.entries().is_empty()).then_some(pane.cursor()),
        ),
    }
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
        let tasks_h = tasks_rows(app);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(tasks_h),
                Constraint::Length(1),
            ])
            .split(frame.area());
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(rows[0]);
        // #108 L5: `now` de las celdas de tiempo relativo — UNA lectura por
        // frame; los tests lo fijan (`App::render_now_ms`) para snapshots
        // estables.
        let now_ms = app.render_now_ms.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        });

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
            draw_sync(frame, rows[0], view, &app.theme);
        } else if let Some(view) = &app.compare {
            draw_compare(frame, rows[0], view, &app.theme, &app.compare_size_hints);
        } else {
            for (i, pane) in app.panes.iter().enumerate() {
                draw_pane(
                    frame,
                    cols[i],
                    pane,
                    app.focus() == i,
                    &app.theme,
                    now_ms,
                    &app.columns,
                    // #117 tarea 2: el catálogo cacheado del scheme del pane (hints
                    // y cabeceras); sin él se pinta con defaults, jamás se espera.
                    app.attr_catalog(pane.dir().scheme()),
                );
            }
        }
        draw_tasks(frame, rows[1], app);
        draw_status(frame, rows[2], app);
    }
    if let Some(help) = &app.help {
        draw_help(frame, help, &app.theme, &app.dialog_hints.help);
    }
    if let Some(picker) = &app.theme_picker {
        draw_theme_picker(frame, picker, &app.theme, &app.dialog_hints.picker);
    }
    if let Some(p) = &app.columns_picker {
        draw_columns_picker(frame, p, &app.theme, &app.dialog_hints.columns);
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
        let inertes = app
            .help
            .as_ref()
            .is_some_and(|help| help.over_modal)
            .then(|| app.dialog_hints.with_modals_inert());
        draw_modal(
            frame,
            modal,
            &app.theme,
            app.focused().name_encoding(),
            inertes.as_ref().unwrap_or(&app.dialog_hints),
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
    let (root_txt, root_hostil) = norte_frontend::path_display_with(root, enc);
    let root_line = if root_hostil {
        format!("{HOSTILE_BADGE} {root_txt}")
    } else {
        root_txt
    };
    let cuerpo = [
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
        Paragraph::new(cuerpo).block(
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
    let ancho = u16::try_from(footer_w.saturating_add(4))
        .unwrap_or(u16::MAX)
        .max(64);
    let rows = u16::try_from(popup.items().len().max(1)).unwrap_or(8) + 2;
    let area = centered(frame.area(), ancho, rows.min(frame.area().height.max(3)));
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

/// Prefijo de `s` que cabe en `max` CELDAS (review MN2/MN3): recorte
/// consciente de ancho — un char de doble celda jamás desborda el
/// presupuesto (el recorte por `chars()` sí lo hacía).
fn take_width(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut usado = 0usize;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if usado + cw > max {
            break;
        }
        usado += cw;
        out.push(c);
    }
    out
}

/// Cell width of `s`, the same budget [`take_width`] spends.
fn cells(s: &str) -> usize {
    s.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// Right-truncation to `max` CELLS, marking the cut with a single `…`.
///
/// The shape for a LABEL, where `middle_ellipsis` is the wrong tool: head
/// plus tail collides any two labels that agree on both ends (`Foo…bar` and
/// `Foo…bar` for two different plugin-supplied titles), while a right cut
/// keeps a distinct prefix distinct. Cell-aware, never char counts: a
/// double-width glyph that does not fit is dropped whole rather than
/// overflowing the column by one cell.
fn right_ellipsis(s: &str, max: usize) -> String {
    if cells(s) <= max {
        return s.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = take_width(s, max - 1);
    out.push('…');
    out
}

/// Splits a generated dialog hint into its whole `[chord] label` groups.
///
/// [`crate::hints::dialog_hints`] joins the groups with a single space and
/// every group starts with `[`, so the boundary is the ` [` join and NOT any
/// space: a label is prose and carries spaces of its own (`otro panel`).
fn hint_groups(hint: &str) -> Vec<&str> {
    let mut groups = Vec::new();
    let mut start = 0usize;
    for (i, _) in hint.match_indices(" [") {
        groups.push(&hint[start..i]);
        start = i + 1;
    }
    if start < hint.len() {
        groups.push(&hint[start..]);
    }
    groups
}

/// Fits a generated dialog hint into `max` CELLS by dropping whole
/// `[chord] label` groups, marking the loss with a trailing `…`.
///
/// Every other overlay measures its hint and grows its popup to fit
/// (`draw_nav_popup`, `draw_extensions`, …). The help overlay is full-screen
/// and cannot grow, so its footer has to be CUT — and a `middle_ellipsis`
/// there was actively lying twice over. At 80 columns it produced
/// `[enter] confirmar [esc]…kspace] atrás [/] filtrar`: the cut fell inside a
/// group and left the brackets balanced, so `[esc]…kspace]` reads as a chord
/// for a key called *kspace* that the app invented; and middle truncation
/// eats the MIDDLE of the list, which is exactly where `[tab] otro panel`
/// sat — the verb the whole two-pane design rests on, gone without a trace.
///
/// A group is therefore emitted WHOLE or not at all, and the `…` says that
/// something was dropped. Groups are kept in order, stopping at the first
/// that does not fit: the footer is then a true prefix of the real hint.
fn fit_hint_groups(hint: &str, max: usize) -> String {
    if cells(hint) <= max {
        return hint.to_owned();
    }
    // Two cells held back: the `…` and the space that separates it from the
    // last group kept.
    let budget = max.saturating_sub(2);
    let mut out = String::new();
    for g in hint_groups(hint) {
        let sep = usize::from(!out.is_empty());
        if cells(&out) + sep + cells(g) > budget {
            break;
        }
        if sep == 1 {
            out.push(' ');
        }
        out.push_str(g);
    }
    if out.is_empty() {
        // Not even one group fits: say so rather than paint half a chord.
        return take_width("…", max);
    }
    out.push(' ');
    out.push('…');
    out
}

/// La línea de cabecera (#108 L5): etiquetas Fluent (o la `header` custom
/// del spec, #108 7b — YA saneada y capada al resolver, aquí solo el
/// recorte por ancho), la del orden activo con `▲`/`▼`. Ancho fiel al de
/// las celdas de las filas; el `align` del estilo elige el lado del
/// relleno en las no-nombre, en paso con sus celdas.
fn column_header_line(
    cols: &[(
        norte_frontend::columns::ColumnId,
        u16,
        norte_frontend::columns::ColumnStyle,
    )],
    sort: norte_frontend::SortSpec,
    catalog: Option<&norte_proto::AttrCatalog>,
) -> String {
    use norte_frontend::SortDir;
    use norte_frontend::columns::Align;
    let mut out = String::new();
    for (i, (col, w, style)) in cols.iter().enumerate() {
        // #117: etiqueta compartida TUI/GUI (header custom del spec →
        // Fluent → catálogo enmascarado → id). NO se re-enmascara aquí:
        // `header_label` ya devuelve texto seguro.
        let label = norte_frontend::columns::header_label(col, style, catalog);
        let activa = norte_frontend::columns::sort_column_id(col) == Some(sort.column);
        let w = usize::from(*w);
        let flecha = if sort.dir == SortDir::Asc {
            '▲'
        } else {
            '▼'
        };
        if i == 0 {
            // Nombre: alineado a la izquierda (deja el hueco del canalón).
            // La flecha se añade TRAS recortar (review MN2): el indicador
            // de dirección sobrevive a cualquier locale; recorte por ANCHO
            // (take_width), jamás por chars. El layout del nombre no lo
            // toca ningún `align` (#108 7b): su bloque manda.
            let budget = if activa { w.saturating_sub(1) } else { w };
            let mut cab = take_width(&label, budget);
            if activa {
                cab.push(flecha);
            }
            let pad = w.saturating_sub(cab.width());
            out.push_str(&cab);
            out.push_str(&" ".repeat(pad));
        } else {
            // No-nombre: el ancho incluye el separador — contenido dentro
            // de w-1, misma cuenta que la celda. Derecha: relleno delante.
            // Izquierda (#108 7b): el separador sigue ABRIENDO el ancho,
            // el contenido va tras él y el relleno cae a la derecha.
            let contenido = w.saturating_sub(1);
            let budget = if activa {
                contenido.saturating_sub(1)
            } else {
                contenido
            };
            let mut cab = take_width(&label, budget);
            if activa {
                cab.push(flecha);
            }
            match style.align {
                Align::Right => {
                    let pad = w.saturating_sub(cab.width());
                    out.push_str(&" ".repeat(pad));
                    out.push_str(&cab);
                }
                Align::Left => {
                    // m1 revisión 7b: emisión clampada a EXACTAMENTE `w`
                    // celdas — con `w == 1` y flecha activa, «espacio +
                    // flecha» emitía 2 y corría toda la cabecera a su
                    // derecha (el separador gana: abre el ancho, como en
                    // las celdas).
                    let clamped = take_width(&format!(" {cab}"), w);
                    let pad = w.saturating_sub(clamped.width());
                    out.push_str(&clamped);
                    out.push_str(&" ".repeat(pad));
                }
            }
        }
    }
    out
}

/// Overlay del catálogo de extensiones (M4-P3): la lista de plugins AGRUPADA
/// por categoría (una cabecera al cambiar de grupo, ya que llegan ordenados)
/// más los directorios que fallaron al cargar. CRÍTICO: `name` y `publisher`
/// son texto LIBRE de un tercero y esto es superficie de decisión de seguridad
/// (aprobar) — se pasan por [`display_name`] (mismo enmascarado de
/// controles/bidi/invisibles que los panes) antes de pintar. El id ya está
/// charset-validado en el core; name/publisher no. `hint` (H1 T3, #24) es
/// el hint GENERADO (`app.dialog_hints.extensions`).
fn draw_extensions(
    frame: &mut Frame<'_>,
    mgr: &crate::app::ExtensionManager,
    theme: &TuiTheme,
    hint: &str,
) {
    // MAJOR-1(c) H1 close: el ancho por CONTENIDO (igual que antes,
    // clamp(24, 80)) puede quedarse corto para el footer GENERADO — mismo
    // criterio de sizing que [`draw_nav_popup`] (medir el footer en CELDAS,
    // `Line::width`, y crecer si hace falta), tope en el ancho del frame.
    let footer_w = Line::raw(format!(" {hint} ")).width();
    let ancho_min = u16::try_from(footer_w.saturating_add(4)).unwrap_or(u16::MAX);
    let ancho = frame
        .area()
        .width
        .saturating_sub(6)
        .clamp(24, 80)
        .max(ancho_min)
        .min(frame.area().width);
    let area = centered(
        frame.area(),
        ancho,
        frame.area().height.saturating_sub(4).max(6),
    );
    clear_themed(frame, area, theme);
    // Ancho útil para la segunda línea (description, P1): igual criterio que
    // `draw_palette` (borde + margen), NO el `ancho` de la caja completa.
    let inner = usize::from(area.width.saturating_sub(4));
    let mut lines: Vec<Line<'_>> = Vec::new();
    if mgr.plugins.is_empty() && mgr.errors.is_empty() {
        lines.push(Line::raw(t("ext-empty")));
    } else {
        let mut last_cat: Option<&str> = None;
        for (i, p) in mgr.plugins.iter().enumerate() {
            if last_cat != Some(p.category.as_str()) {
                last_cat = Some(p.category.as_str());
                let (cat, _) = display_name(p.category.as_bytes());
                lines.push(Line::styled(cat, theme.role(Role::Title)));
            }
            lines.push(plugin_line(p, i == mgr.cursor, theme));
            if let Some(desc_line) = plugin_description_line(p, theme, inner) {
                lines.push(desc_line);
            }
        }
        for e in &mgr.errors {
            let (dir, _) = display_name(e.dir.as_bytes());
            let (reason, _) = display_name(e.reason.as_bytes());
            lines.push(Line::styled(
                format!(" {HOSTILE_BADGE} {dir}: {reason}"),
                theme.role(Role::Error),
            ));
        }
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("ext-title")))
        .title_style(theme.role(Role::Title))
        .title_bottom(Line::raw(format!(" {hint} ")))
        .border_style(theme.role(Role::ModalBorder));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Panel de `[config]` de UN plugin (G3c, drill-down de
/// [`draw_extensions`]): una línea `<key>: <value>` por
/// [`norte_frontend::plugin_config::ConfigKeyRow`], la seleccionada
/// resaltada; si se está editando (`state.is_editing()`), el buffer RAW se
/// pinta bajo la fila con un cursor `_` (mismo idioma visual que un
/// name-input popup). `key`/`kind`/`value` son charset-safe o vocabulario
/// de norte (nunca texto libre del plugin — ver el rustdoc de
/// [`norte_frontend::plugin_config::ConfigKeyRow`]); `description` llega YA
/// enmascarada (`sanitize_config_keys`), se pinta como segunda línea
/// atenuada igual que [`plugin_description_line`].
fn draw_plugin_config_panel(
    frame: &mut Frame<'_>,
    panel: &crate::app::PluginConfigPanel,
    theme: &TuiTheme,
    hint: &str,
) {
    let footer_w = Line::raw(format!(" {hint} ")).width();
    let ancho_min = u16::try_from(footer_w.saturating_add(4)).unwrap_or(u16::MAX);
    let ancho = frame
        .area()
        .width
        .saturating_sub(6)
        .clamp(24, 80)
        .max(ancho_min)
        .min(frame.area().width);
    let area = centered(
        frame.area(),
        ancho,
        frame.area().height.saturating_sub(4).max(6),
    );
    clear_themed(frame, area, theme);
    let mut lines: Vec<Line<'_>> = Vec::new();
    let rows = panel.state.rows();
    if rows.is_empty() {
        lines.push(Line::raw(t("ext-empty")));
    } else {
        for (i, row) in rows.iter().enumerate() {
            let selected = i == panel.state.cursor();
            let cursor = if selected { ">" } else { " " };
            let mut line = Line::raw(format!("{cursor} {}: {}", row.key, row.value));
            if selected {
                line = line.style(theme.role(Role::Selection));
            }
            lines.push(line);
            if selected && panel.state.is_editing() {
                let buf = panel.state.edit_buffer().unwrap_or_default();
                lines.push(Line::styled(
                    format!("   {buf}_"),
                    theme.role(Role::BorderUnfocused),
                ));
            } else if !row.description.is_empty() {
                lines.push(Line::styled(
                    format!("   {}", row.description),
                    theme.role(Role::BorderUnfocused),
                ));
            }
        }
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", panel.plugin_name))
        .title_style(theme.role(Role::Title))
        .title_bottom(Line::raw(format!(" {hint} ")))
        .border_style(theme.role(Role::ModalBorder));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Una línea de plugin: `<nombre> v<version> [<badges>] <estado>`. `name` y
/// `publisher` van enmascarados ([`display_name`]); badges = capabilities
/// unidas (o `-` si vacío); estado = `✓` si activo y aviso `⚠` (rol Warning)
/// si NO está aprobado. La línea seleccionada se resalta como el theme picker.
fn plugin_line<'a>(
    p: &'a norte_proto::methods::PluginInfo,
    selected: bool,
    theme: &TuiTheme,
) -> Line<'a> {
    let (name, _) = display_name(p.name.as_bytes());
    let (version, _) = display_name(p.version.as_bytes());
    let badges = if p.capabilities.is_empty() {
        "-".to_owned()
    } else {
        p.capabilities.join(" ")
    };
    let cursor = if selected { ">" } else { " " };
    let mut spans = vec![Span::raw(format!("{cursor} {name} v{version} [{badges}] "))];
    if p.enabled {
        spans.push(Span::styled("✓", theme.role(Role::Info)));
        spans.push(Span::raw(" "));
    }
    if !p.approved {
        spans.push(Span::styled(
            format!("⚠ {}", t("ext-unapproved")),
            theme.role(Role::Warning),
        ));
    }
    let mut line = Line::from(spans);
    if selected {
        line = line.style(theme.role(Role::Selection));
    }
    line
}

/// Segunda línea BAJO cada plugin con su `description` (P1), si la declara
/// — `None` si el plugin no tiene una. El camino normal (`main::dispatch`,
/// brazo `app.extensions`) ya llega con `description` clampada+enmascarada
/// por `app::clamp_plugin_descriptions` (P1 encoding audit F1: UNA vez por
/// plugin al ingest, no por frame) — pero este draw NO confía ciegamente en
/// eso: re-clampa+enmascara aquí también, self-contained como `plugin_line`
/// con `name`/`publisher` (y como `palette::plugin_rows`). Un control/bidi
/// crudo que llegara a `ratatui` sin pasar por [`display_name`] no se pinta
/// como `�` — un char de control/override es INVISIBLE en la celda, así que
/// desaparecería en silencio (justo lo que el enmascarado existe para
/// evitar); confiar ciegamente en el caller cambiaría "marcado" por
/// "silencioso" ante cualquier ruta que construya `ExtensionManager` sin
/// pasar por el ingest (tests, un futuro caller). Sobre un string YA
/// acotado (el caso normal) esto es barato e idempotente. Elipsis MEDIA
/// ([`middle_ellipsis`]) al ancho útil del popup para no desbordar la caja.
/// Sin badge de hostil (el badge es para diagnóstico de fallos de carga,
/// [`HOSTILE_BADGE`], no para cosmética de terceros — mismo criterio que
/// `plugin_line`). Estilo atenuado (`Role::BorderUnfocused`, "presente pero
/// no activo" — mismo criterio que documenta ese rol): es contexto, no el
/// dato principal de la fila.
fn plugin_description_line(
    p: &norte_proto::methods::PluginInfo,
    theme: &TuiTheme,
    inner: usize,
) -> Option<Line<'static>> {
    let raw = p.description.as_deref()?;
    let clamped: String = raw
        .chars()
        .take(crate::app::PLUGIN_DESCRIPTION_WIRE_CAP)
        .collect();
    let (masked, _) = display_name(clamped.as_bytes());
    let texto = format!("   {}", middle_ellipsis(&masked, inner.saturating_sub(3)));
    Some(Line::styled(texto, theme.role(Role::BorderUnfocused)))
}

/// Popup selector de tema: lista de presets con el vigente resaltado (ADR
/// 0020). El preview en vivo lo hace el bucle de eventos; aquí solo se
/// pinta. `hint` (H1 T3, #24) es el hint GENERADO (`app.dialog_hints.picker`).
/// MAJOR-1(c) H1 close: 34 columnas era un ancho FIJO que no crecía con el
/// hint generado (se cortaba en terminales angostas) — mismo criterio de
/// sizing que [`draw_nav_popup`]/[`draw_extensions`], footer en CELDAS
/// (`Line::width`), suelo 34 (el listado de nombres de preset ya cabía),
/// tope el ancho del frame.
/// The which-key panel (K3a): while a chord sequence is PENDING, what can
/// follow it — every continuation, the unavailable ones included and dimmed,
/// with the reason they do nothing.
///
/// It appears with the keystroke that leaves the prefix pending and vanishes
/// with the one that ends it. No delay, ever: ADR 0006's resolution is
/// timing-free, and a panel on a 400 ms timer would make the same keystrokes
/// show different things depending on how fast they were typed.
///
/// Anchored at the bottom left, just above the status bar, where the pending
/// segment it explains is already painted — the panes stay readable above it.
/// It takes NO keys: the resolver keeps the keyboard, so the reader carries on
/// typing the sequence and watches the panel narrow.
///
/// Every string it paints is masked at the source: chords come through
/// `paint_chord` (a project layer can bind any lone codepoint) and the rest is
/// Fluent text or a catalogue command name.
fn draw_which_key(
    frame: &mut Frame<'_>,
    wk: &norte_frontend::whichkey::WhichKeyRows,
    theme: &TuiTheme,
) {
    use norte_frontend::keymap::Availability;

    if wk.is_empty() {
        return;
    }
    let base = frame.area();
    // The status bar owns the last line and the panel never paints over it:
    // that segment is what survives when there is no room for the box, which
    // is exactly the case below. FOUR rows above the bar and not three,
    // because with three the only line inside the borders would be the
    // "… 0/12" counter — three of the reader's rows spent saying that there
    // was no room to say anything. The floor is "at least one real key".
    let outside = base.height.saturating_sub(1);
    if outside < 4 {
        return;
    }
    let cap = usize::from(outside.saturating_sub(2)); // lines inside the borders
    let total = wk.rows.len();
    // A dropped row is a key the panel does not mention, so the count of what
    // was dropped COSTS a line of its own: taking `cap` rows and then adding
    // the count on top is how the count itself gets clipped, and a box that
    // just ends implies the list ended with it.
    let (shown, truncated) = if total <= cap {
        (total, false)
    } else {
        (cap.saturating_sub(1), true)
    };
    let body = shown + usize::from(truncated);
    let chord_w = wk
        .rows
        .iter()
        .take(shown)
        .map(|r| r.chord.width())
        .max()
        .unwrap_or(0);
    let text = |r: &norte_frontend::whichkey::WhichKeyRow| {
        let sep = if r.reason.is_empty() {
            String::new()
        } else {
            format!(" — {}", r.reason)
        };
        let tail = if r.opens_sequence { " …" } else { "" };
        format!("{}{}{}", r.label, tail, sep)
    };
    let mut lines: Vec<Line<'_>> = wk
        .rows
        .iter()
        .take(shown)
        .map(|r| {
            let pad = " ".repeat(chord_w.saturating_sub(r.chord.width()));
            let chord = Span::styled(format!(" {}{pad}  ", r.chord), theme.role(Role::Title));
            // Dimmed, not hidden: the key IS bound, it just cannot run — the
            // panel says why instead of pretending the key does not exist.
            let style = if r.avail == Availability::Here {
                Style::default()
            } else {
                Style::default().add_modifier(ratatui::style::Modifier::DIM)
            };
            Line::from(vec![chord, Span::styled(text(r), style)])
        })
        .collect();
    if truncated {
        lines.push(Line::raw(format!(
            " {}",
            ta(
                "whichkey-truncated",
                &[("shown", &shown.to_string()), ("total", &total.to_string()),],
            )
        )));
    }
    let title = format!(" {} … ", wk.title);
    let content = lines.iter().map(Line::width).max().unwrap_or(0);
    let width = u16::try_from(content.max(title.width()).saturating_add(2))
        .unwrap_or(u16::MAX)
        .min(base.width);
    let height = u16::try_from(body.saturating_add(2))
        .unwrap_or(u16::MAX)
        .min(outside.max(1));
    let area = Rect {
        x: base.x,
        y: base.y + outside.saturating_sub(height),
        width,
        height,
    };
    clear_themed(frame, area, theme);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(Role::ModalBorder));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_theme_picker(
    frame: &mut Frame<'_>,
    picker: &crate::app::ThemePicker,
    theme: &TuiTheme,
    hint: &str,
) {
    let footer_w = Line::raw(format!(" {hint} ")).width();
    let ancho = u16::try_from(footer_w.saturating_add(4))
        .unwrap_or(u16::MAX)
        .max(34)
        .min(frame.area().width);
    let rows = u16::try_from(picker.names.len()).unwrap_or(8) + 2;
    let area = centered(frame.area(), ancho, rows.min(frame.area().height.max(3)));
    clear_themed(frame, area, theme);
    let items: Vec<ListItem<'_>> = picker
        .names
        .iter()
        .map(|n| ListItem::new(Line::raw(format!(" {n}"))))
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("theme-picker-title")))
        .title_style(theme.role(Role::Title))
        .title_bottom(Line::raw(format!(" {hint} ")))
        .border_style(theme.role(Role::ModalBorder));
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.role(Role::Selection));
    let mut state = ListState::default();
    state.select(Some(picker.cursor));
    frame.render_stateful_widget(list, area, &mut state);
}

/// Overlay del picker de columnas (#108 7a): lista con cursor — checkbox,
/// etiqueta (Fluent para builtins; `label` del modelo para attr/plugin,
/// #117; el id CRUDO enmascarado para los que no parsean — texto de config
/// del usuario, #73: se pinta con `mask_terminal_hazards`) y la flecha del
/// sort en la fila de su columna. Mismo esqueleto que [`draw_theme_picker`] (Clear + centrado,
/// `List` + `ListState` con highlight `Role::Selection`, hint generado en
/// `title_bottom`, ancho por contenido en CELDAS con suelo del footer).
fn draw_columns_picker(
    frame: &mut Frame<'_>,
    p: &norte_frontend::columns_picker::ColumnsPicker,
    theme: &TuiTheme,
    hint: &str,
) {
    use norte_frontend::columns::{Builtin, sort_column};
    let target = if p.scheme_override() {
        p.scheme().to_owned()
    } else {
        t("columns-picker-target-default")
    };
    let titulo = ta("columns-picker-title", &[("target", &target)]);
    let filas: Vec<String> = p
        .rows()
        .iter()
        .map(|r| {
            let marca = if r.enabled { "[x]" } else { "[ ]" };
            let etiqueta = match r.builtin {
                Some(Builtin::Name) => t("col-header-name"),
                Some(Builtin::Size) => t("col-header-size"),
                Some(Builtin::Mtime) => t("col-header-mtime"),
                Some(Builtin::Kind) => t("col-header-kind"),
                // #117: attr/plugin traen `label` (header_label, YA
                // enmascarada al abrir); los que no parsean caen al id. El
                // re-enmascarado es cinturón, no el choke point; el cap
                // (encoding-audit L1, paridad GUI) evita que un id
                // kilométrico de config ensanche el overlay entero.
                None => norte_encoding::mask_terminal_hazards(r.label.as_deref().unwrap_or(&r.id))
                    .chars()
                    .take(norte_frontend::columns::HEADER_MAX_CHARS)
                    .collect(),
            };
            let flecha = match r.builtin.and_then(sort_column) {
                Some(sc) if sc == p.sort().column => {
                    if p.sort().dir == norte_frontend::SortDir::Asc {
                        " ▲"
                    } else {
                        " ▼"
                    }
                }
                _ => "",
            };
            // #108 7b: el formato vigente de la fila (vocabulario ASCII
            // cerrado — sin enmascarar), ciclable con `f`.
            let formato = r
                .format
                .as_deref()
                .map(|f| format!(" · {f}"))
                .unwrap_or_default();
            format!(" {marca} {etiqueta}{flecha}{formato}")
        })
        .collect();
    let footer_w = Line::raw(format!(" {hint} ")).width();
    let contenido_w = filas
        .iter()
        .map(|f| Line::raw(f.as_str()).width())
        .max()
        .unwrap_or(0);
    let ancho = u16::try_from(footer_w.max(contenido_w).saturating_add(4))
        .unwrap_or(u16::MAX)
        .max(34)
        .min(frame.area().width);
    // M2 revisión 7a: saturante — una config hostil de 65k ids desbordaría
    // el `+ 2` en debug; el `.min(alto del frame)` de abajo sigue clampando.
    let rows = u16::try_from(p.rows().len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = centered(frame.area(), ancho, rows.min(frame.area().height.max(3)));
    clear_themed(frame, area, theme);
    let items: Vec<ListItem<'_>> = filas.into_iter().map(ListItem::new).collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {titulo} "))
        .title_style(theme.role(Role::Title))
        .title_bottom(Line::raw(format!(" {hint} ")))
        .border_style(theme.role(Role::ModalBorder));
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.role(Role::Selection));
    let mut state = ListState::default();
    state.select(Some(p.cursor()));
    frame.render_stateful_widget(list, area, &mut state);
}

/// Lower bound in CELLS of the help sidebar: the width it used to have,
/// unconditionally. Kept as a FLOOR so an 80-column frame never gets a
/// narrower list of topics than it had before the sidebar was sized to its
/// content.
const HELP_SIDEBAR_MIN: u16 = 24;

/// Upper bound of the sidebar, as a percentage of the FRAME's width. The
/// sidebar is a table of contents: past roughly a third of the screen it is
/// taking width from the prose it exists to point at.
const HELP_SIDEBAR_PCT: u16 = 35;

/// Cells between the sidebar and the body. Without it a title that fills the
/// sidebar sits against the first letter of the prose and the two columns
/// read as one broken line.
const HELP_GUTTER: u16 = 2;

/// Ancho de una barra de scroll: una celda.
///
/// La ayuda es la única pantalla con DOS listas que se desplazan a la vez —el
/// índice y la página— y hasta ahora ninguna de las dos decía por dónde iba ni
/// cuánto le quedaba. El indicador `N/M` del pie habla solo del cuerpo, y solo
/// cuando no cabe.
const HELP_SCROLLBAR: u16 = 1;

/// Typographic measure of the body in CELLS. Prose is read at 60–72 cells; at
/// 90 the eye loses the line on the return sweep, and the surplus is exactly
/// what the sidebar needs to stop truncating its titles.
const HELP_MEASURE: u16 = 72;

/// Cells the body keeps whatever the sidebar asks for. Only bites on frames
/// too narrow for the overlay to be useful at all, and only to keep the body
/// from being laid out at zero width.
const HELP_BODY_MIN: u16 = 20;

/// Cells a topic row is indented by in the sidebar, so that a title never
/// lines up with the group header above it.
const HELP_ROW_INDENT: usize = 2;

/// Cells the sidebar would need to paint every row of `lang` IN FULL: the
/// indent plus the widest title, and the widest group header.
///
/// Measured over the whole corpus and not over `HelpState::rows()`, which is
/// what the filter narrows: a sidebar sized to the rows that survive would
/// change width on every keystroke, and the body — pre-rendered at the width
/// left over — would re-wrap its prose under the reader while they type.
///
/// The synthetic `keys` GROUP is deliberately not measured: its header is not
/// painted (see [`draw_help`]).
fn help_sidebar_desired(lang: norte_help::Lang) -> u16 {
    let mut want = HELP_ROW_INDENT + cells(&t("help-topic-keys"));
    for topic in norte_help::topics(lang) {
        want = want.max(HELP_ROW_INDENT + cells(&topic.title));
        match topic.tags.first() {
            Some(tag) if !tag.is_empty() => {
                want = want.max(cells(&t(&format!("help-group-{tag}"))));
            }
            _ => {}
        }
    }
    u16::try_from(want).unwrap_or(u16::MAX)
}

/// Width in CELLS of the help sidebar over a frame of `base`, for the corpus
/// of `lang`.
///
/// Public for the test that pins the sizing decision: the sidebar grows with
/// its content, floors at the 24 cells it used to have fixed, and never takes
/// more than a 35% share of the frame. See `help_layout`, where that is
/// decided and where the two bounds are named.
#[must_use]
pub fn help_sidebar_width(base: Rect, lang: norte_help::Lang) -> u16 {
    let (_, sidebar, _, _) = help_layout(base, help_sidebar_desired(lang));
    sidebar.width
}

/// Geometría del overlay de ayuda: `(caja, lateral, cuerpo, pie)`.
///
/// Una sola función porque el pintor y el PRE-RENDER
/// ([`crate::app::App::refresh_help`]) tienen que medir lo mismo: el modelo
/// acota `body_scroll` contra el número de líneas que se maquetaron para un
/// ancho, y maquetar para un ancho distinto del pintado deja el scroll fuera
/// del cuerpo justo en los bordes (el fallo que el pre-render evita).
///
/// `sidebar_desired` es lo que la lateral necesitaría para pintar sus filas
/// enteras ([`help_sidebar_desired`]); llega como parámetro para que esto siga
/// siendo una función de números, medible a cualquier tamaño sin corpus.
fn help_layout(base: Rect, sidebar_desired: u16) -> (Rect, Rect, Rect, Rect) {
    let area = centered(
        base,
        base.width.saturating_sub(4).max(20),
        base.height.saturating_sub(2).max(6),
    );
    let inner = Block::default().borders(Borders::ALL).inner(area);
    // El corte VERTICAL va PRIMERO (review MAJOR): la caja reserva su última
    // línea para el pie (el filtro o el hint generado), como `draw_settings`
    // reserva la suya para la descripción — el pie del borde (`title_bottom`)
    // no cabría con la lateral delante. Cortando la horizontal antes, el pie
    // se quedaba con el ancho del CUERPO (50 celdas en un frame de 80) y
    // `fit_hint_groups` tiraba el grupo que abre el cuerpo, `[tab]`, que es
    // la única entrada a la mitad donde `Enter` toca el sistema de ficheros.
    // Así el pie ocupa el ancho ENTERO (74 celdas en ese mismo frame).
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    // El corte HORIZONTAL: lateral, canalón y cuerpo. La lateral pide lo que
    // mide su contenido, con suelo en lo que siempre tuvo y techo en una parte
    // del frame; el cuerpo se queda el resto, capado a su MEDIDA. Lo que sobre
    // —un terminal muy ancho— sencillamente no se usa: 90 celdas de prosa se
    // leen peor que 72, no mejor.
    let avail = rows[0].width;
    let pct = u16::try_from(u32::from(base.width) * u32::from(HELP_SIDEBAR_PCT) / 100)
        .unwrap_or(u16::MAX);
    let ceiling = pct
        .max(HELP_SIDEBAR_MIN)
        .min(avail.saturating_sub(HELP_GUTTER + HELP_BODY_MIN));
    // `max` DESPUÉS de `min`: en un frame demasiado estrecho para el suelo
    // manda el techo — una lateral más ancha que la caja dejaría el cuerpo a
    // cero celdas, y un `clamp` con el rango invertido entra en pánico.
    let side = sidebar_desired
        .max(HELP_SIDEBAR_MIN.min(ceiling))
        .min(ceiling);
    let gutter = HELP_GUTTER.min(avail.saturating_sub(side));
    let body = avail.saturating_sub(side + gutter).min(HELP_MEASURE);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(side),
            Constraint::Length(gutter),
            Constraint::Length(body),
            Constraint::Min(0),
        ])
        .split(rows[0]);
    // El CUERPO conserva exactamente la misma altura que antes (`inner` menos
    // la fila del pie): `help_body_size` la publica y el pre-render acota
    // contra ella. Lo que cambia es la lateral, que ahora también cede esa
    // fila — el pie es de la caja, no de una columna.
    (area, cols[0], cols[2], rows[1])
}

/// Ancho y alto EN CELDAS del cuerpo del overlay de ayuda sobre un frame de
/// `base`, para que el run loop maquete la página con
/// [`App::refresh_help`](crate::app::App::refresh_help) justo antes de
/// pintarla. Ver `help_layout`, de donde sale.
///
/// `lang` es el locale del corpus con el que se abrió el overlay
/// (`HelpState::lang`): la lateral se dimensiona a los títulos que tiene que
/// pintar, así que el ancho que le queda al cuerpo depende de él. El ALTO no.
#[must_use]
pub fn help_body_size(base: Rect, lang: norte_help::Lang) -> (usize, usize) {
    let (_, _, body, _) = help_layout(base, help_sidebar_desired(lang));
    // La última columna del cuerpo es su barra de scroll, así que la prosa se
    // envuelve a una celda menos. Sale de aquí y no del pintado porque quien
    // maqueta la página es el run loop, y una anchura que no case con la
    // pintada parte las líneas por donde no toca.
    (
        usize::from(body.width.saturating_sub(HELP_SCROLLBAR)),
        usize::from(body.height),
    )
}

/// Overlay de ayuda (H3b), a pantalla (casi) completa y por encima de todo:
/// lateral de temas a la izquierda, cuerpo del tema abierto a la derecha y
/// pie de una línea bajo el cuerpo.
///
/// **No maqueta nada**: el cuerpo llega YA renderizado en
/// [`crate::app::HelpView`] (ver su doc — el modelo necesita saber cuántas
/// líneas salieron para acotar su scroll, y un `draw_*` solo recibe `&App`).
/// Aquí se recorta por scroll y se resalta, nada más.
///
/// Enmascarado: los títulos del corpus vienen del binario (built-in) o ya
/// enmascarados por `norte_help::parse_untrusted` (plugin), y las líneas del
/// cuerpo las produjo [`crate::help_render`] sobre esa misma entrada — este
/// draw no vuelve a filtrarlas, igual que [`draw_palette`] con sus filas. La
/// ÚNICA entrada libre es el filtro tecleado por el usuario, que pasa por el
/// mismo doble filtro que la barra de quick search (`filter_display` — jamás
/// `filter_raw` — más [`display_name`]).
fn draw_help(frame: &mut Frame<'_>, help: &crate::app::HelpView, theme: &TuiTheme, hint: &str) {
    use norte_frontend::help::{Focus, SidebarRow};

    let (area, sidebar, body_area, footer_area) =
        help_layout(frame.area(), help_sidebar_desired(help.state.lang()));
    clear_themed(frame, area, theme);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", t("help-title")))
            .title_style(theme.role(Role::Title))
            .border_style(theme.role(Role::ModalBorder)),
        area,
    );

    let state = &help.state;
    // La barra del índice vive en la PRIMERA celda del canalón: pegada a la
    // lateral y sin quitarle ni una columna a los títulos, que es lo que el
    // canalón estaba para dar.
    let sidebar_scrollbar = Rect {
        x: sidebar.x.saturating_add(sidebar.width),
        y: sidebar.y,
        width: HELP_SCROLLBAR.min(frame.area().width.saturating_sub(sidebar.x + sidebar.width)),
        height: sidebar.height,
    };
    // El canalón es una COLUMNA propia del layout, así que la lateral puede
    // gastarse su ancho entero en el título.
    let side_w = usize::from(sidebar.width);
    // Las filas pintadas NO son las del modelo: entre grupo y grupo va una
    // línea en blanco. Va aquí y no en `HelpState::rows`, que es la lista
    // NAVEGABLE — sus índices son los que direcciona `cursor()`, y meter
    // separadores ahí rompería el cursor y de paso la GUI hermana. Por eso se
    // lleva el mapa fila→ítem: es lo que traduce el cursor del modelo al
    // índice del widget.
    let mut items: Vec<ListItem<'_>> = Vec::with_capacity(state.rows().len() + 4);
    let mut painted: Vec<usize> = Vec::with_capacity(state.rows().len());
    for (i, row) in state.rows().iter().enumerate() {
        match row {
            // El modelo entrega TAGS, no texto: la traducción es cosa del
            // frontend (una misma fila se llama distinto en la TUI y en la
            // GUI). Un tag sin entrada Fluent pintaría su propia clave, que
            // es lo que la suite de i18n impide.
            SidebarRow::Group { tag } => {
                if keys_only_group(state.rows(), i) {
                    // Una cabecera que se llama igual que su única entrada no
                    // informa de nada y cuesta una fila. Se apunta el ítem que
                    // vendrá — el cursor jamás se apoya en una cabecera, así
                    // que el mapa solo tiene que quedar bien formado.
                    painted.push(items.len());
                    continue;
                }
                // Aire entre grupos, menos antes del primero: un blanco
                // arriba del todo se lee como una lateral descuadrada.
                if !items.is_empty() {
                    items.push(ListItem::new(Line::default()));
                }
                painted.push(items.len());
                items.push(ListItem::new(Line::styled(
                    right_ellipsis(&t(&format!("help-group-{tag}")), side_w),
                    theme.role(Role::Title),
                )));
            }
            // El título de la entrada sintética `keys` es la etiqueta que
            // `HelpView::new` le dio al modelo (`help-topic-keys`), así que
            // aquí no hay caso especial: la lateral pinta lo mismo que el
            // filtro busca.
            SidebarRow::Topic { title, .. } => {
                painted.push(items.len());
                items.push(ListItem::new(Line::raw(right_ellipsis(
                    &format!("{}{title}", " ".repeat(HELP_ROW_INDENT)),
                    side_w,
                ))));
            }
        }
    }
    // Cuántas filas tiene el índice PINTADO (con sus separadores): es el
    // total contra el que se dimensiona su barra, y hay que leerlo antes de
    // que el widget se lleve la lista.
    let filas_indice = items.len();
    let mut list_state = ListState::default();
    // `HelpState` garantiza que el cursor se apoya SIEMPRE en una fila
    // seleccionable (nunca en una cabecera); con el filtro sin resultados no
    // hay fila alguna que resaltar.
    list_state.select(painted.get(state.cursor()).copied());
    frame.render_stateful_widget(
        List::new(items).highlight_style(theme.role(Role::Selection)),
        sidebar,
        &mut list_state,
    );

    let (lines, action_lines) = help.body();
    // La línea de la acción con foco. Con el foco en la lateral no se resalta
    // ninguna: el cursor del cuerpo existe, pero no es el que mueven las
    // flechas, y resaltarlo diría lo contrario.
    let focused = (state.focus() == Focus::Body)
        .then(|| action_lines.get(state.action_cursor()).copied())
        .flatten();
    let cuerpo: Vec<Line<'_>> = lines
        .iter()
        .enumerate()
        .skip(state.body_scroll())
        .take(usize::from(body_area.height))
        .map(|(i, line)| {
            if Some(i) == focused {
                line.clone().style(theme.role(Role::Selection))
            } else {
                line.clone()
            }
        })
        .collect();
    // La última columna del cuerpo es su barra: la prosa ya viene envuelta a
    // una celda menos (`help_body_size`), así que aquí solo se reparte.
    let (texto_area, barra_cuerpo) = split_scrollbar(body_area);
    frame.render_widget(Paragraph::new(cuerpo), texto_area);
    // Las DOS columnas dicen por dónde van. Hasta ahora ninguna lo decía: el
    // `N/M` del pie habla solo del cuerpo y solo cuando no cabe, así que en el
    // índice no había NADA que dijera que quedaban filas debajo.
    render_scrollbar(
        frame,
        barra_cuerpo,
        theme,
        lines.len(),
        state.body_scroll(),
        usize::from(body_area.height),
    );
    render_scrollbar(
        frame,
        sidebar_scrollbar,
        theme,
        filas_indice,
        list_state.offset(),
        usize::from(sidebar.height),
    );

    draw_help_footer(
        frame,
        footer_area,
        theme,
        state,
        hint,
        lines.len(),
        body_area.height,
    );
}

/// El pie del overlay de ayuda: el hint (o el filtro) a la izquierda y dónde
/// va el lector a la derecha.
fn draw_help_footer(
    frame: &mut Frame<'_>,
    footer_area: Rect,
    theme: &TuiTheme,
    state: &norte_frontend::help::HelpState,
    hint: &str,
    total: usize,
    body_height: u16,
) {
    // Dónde está el lector dentro de la página, con el MISMO idioma que el
    // visor (`{fila}/{total}`, `draw_viewer`). Solo cuando la página NO cabe:
    // un `1/9` sobre nueve líneas visibles es ruido. Importa más aquí que en
    // el visor porque las filas ejecutables — la columna de chords y el
    // `Enter` para el que existe este overlay — se pintan DETRÁS de toda la
    // prosa, así que en una página larga no se ven en el primer render y sin
    // esto nada dice que estén ahí.
    let pos = (total > usize::from(body_height)).then(|| {
        format!(
            " {}/{} ",
            (state.body_scroll() + 1).min(total.max(1)),
            total.max(1)
        )
    });
    let pos = pos.unwrap_or_default();
    // El indicador se lleva su trozo del pie ANTES de recortar el hint: a la
    // derecha jamás le disputa el borde izquierdo al hint, y el hint jamás se
    // le come a él (`fit_hint_groups` tira grupos enteros, no celdas sueltas).
    let ancho = usize::from(footer_area.width);
    let izq_max = ancho.saturating_sub(cells(&pos));
    let izq = if state.filtering() {
        let (query, _) = display_name(state.filter_display().as_bytes());
        middle_ellipsis(&format!(" /{query}"), izq_max)
    } else {
        // NUNCA `middle_ellipsis` sobre un hint generado: ver
        // [`fit_hint_groups`]. Una celda del pie es del margen izquierdo.
        format!(" {}", fit_hint_groups(hint, izq_max.saturating_sub(1)))
    };
    let hueco = ancho.saturating_sub(cells(&izq) + cells(&pos));
    let footer = Line::from(vec![
        Span::raw(izq),
        Span::raw(" ".repeat(hueco)),
        Span::raw(pos),
    ]);
    frame.render_widget(
        Paragraph::new(footer).style(theme.role(Role::BorderUnfocused)),
        footer_area,
    );
}

/// Parte un área en (contenido, barra de scroll): la ÚLTIMA columna es la
/// barra. Con menos de dos celdas no hay barra que pintar y se devuelve el
/// área entera — una barra que se come el texto es peor que no tenerla.
fn split_scrollbar(area: Rect) -> (Rect, Rect) {
    if area.width < 2 {
        return (area, Rect::new(area.x, area.y, 0, area.height));
    }
    let texto = Rect {
        width: area.width - HELP_SCROLLBAR,
        ..area
    };
    let barra = Rect {
        x: area.x + area.width - HELP_SCROLLBAR,
        width: HELP_SCROLLBAR,
        ..area
    };
    (texto, barra)
}

/// Pinta una barra de scroll vertical en `area` para un contenido de `total`
/// filas del que se ven `visible` desde `offset`.
///
/// No pinta nada cuando cabe todo: una barra llena de arriba abajo no informa
/// de nada y encima invita a arrastrarla.
fn render_scrollbar(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &TuiTheme,
    total: usize,
    offset: usize,
    visible: usize,
) {
    if area.width == 0 || area.height == 0 || total <= visible {
        return;
    }
    let mut estado = ScrollbarState::new(total.saturating_sub(visible)).position(offset);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .style(theme.role(Role::BorderUnfocused)),
        area,
        &mut estado,
    );
}

/// Whether the sidebar paints a header for the group row at `header`.
///
/// Public because it is also what says which `help-group-{tag}` lookups the
/// painter can make, and the i18n sweep over those lookups
/// (`norte-tui/tests/keymap.rs`) must ask rather than re-derive: a tag whose
/// header is never painted needs no Fluent entry, and one that is painted
/// needs one in every locale.
#[must_use]
pub fn help_group_is_painted(rows: &[norte_frontend::help::SidebarRow], header: usize) -> bool {
    !keys_only_group(rows, header)
}

/// Whether the group header at `header` heads a group whose only member is
/// the synthetic keyboard entry.
///
/// Keyed off [`norte_frontend::help::KEYS_ID`] and never off the STRING: the
/// header and the row are both painted from Fluent, and in every locale so far
/// they are the same word — but that is a fact about the catalogue, not
/// something to branch on.
fn keys_only_group(rows: &[norte_frontend::help::SidebarRow], header: usize) -> bool {
    use norte_frontend::help::{KEYS_ID, SidebarRow};

    let mut members = rows
        .get(header.saturating_add(1)..)
        .unwrap_or_default()
        .iter()
        .take_while(|row| matches!(row, SidebarRow::Topic { .. }));
    let only = matches!(
        members.next(),
        Some(SidebarRow::Topic { id, .. }) if id.as_str() == KEYS_ID
    );
    only && members.next().is_none()
}

/// Command palette (`Ctrl+P`/vim `:`, H1 T4, spec-promised): filtro libre
/// sobre TODOS los comandos, mismo idioma visual que [`draw_nav_popup`]
/// (centrado, input al pie, `Clear` antes de pintar) pero MÁS ancha (60
/// columnas: `{texto} {descripción} {chord}` no cabe en el ancho de un
/// popup normal). Una fila built-in ([`crate::palette::build_rows`]) trae
/// `text`/`desc`/`chord` CONFIABLES (constantes del binario + catálogo
/// Fluent) — este draw jamás los enmascara. Una fila de plugin (P1,
/// [`crate::palette::plugin_rows`]) trae texto de TERCEROS, pero YA
/// enmascarado en la fila misma (mismo criterio que `first_chord` con la
/// columna chord: el enmascarado vive donde se CONSTRUYE la fila, no aquí)
/// — este draw sigue sin diferenciar, solo pinta lo que ya es seguro. La
/// `key` de despacho (P1: puede llevar el `command_id` crudo de un plugin,
/// sin charset validado) NUNCA se lee aquí — [`crate::app::Palette::rows`]
/// solo se consulta por `text`/`desc`/`chord`. La query (tecleada por el
/// usuario) pasa por [`crate::app::Palette::query_display`] (mismo
/// contrato que `QuickSearch::query_display`: un paste hostil no pinta
/// bidi/invisibles crudos en el borde) + [`display_name`] (mismo doble
/// filtro que la barra de quick search del pane, línea de abajo). El hint
/// es ESTÁTICO (`palette-hint`): la palette NO resuelve por el contexto
/// `dialog` (decisión 8 del plan H1 — es un editor de filtro libre como el
/// diálogo de búsqueda), así que no hay hint GENERADO que mostrar aquí.
///
/// Ese pie se une con `palette-hint-help` (H3c: `F1` sobre una fila abre la
/// página que documenta su comando). Van en dos claves y se juntan AQUÍ porque
/// `palette-hint` lo pinta también la GUI, que todavía no tiene overlay de
/// ayuda (fase H3f): una sola cadena le haría anunciar una tecla inerte.
fn draw_palette(frame: &mut Frame<'_>, palette: &crate::app::Palette, theme: &TuiTheme) {
    let rows = u16::try_from(palette.visible().len().max(1))
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = centered(frame.area(), 60, rows.min(frame.area().height.max(3)));
    clear_themed(frame, area, theme);
    let inner = usize::from(area.width.saturating_sub(3));
    let (items, selected): (Vec<ListItem<'_>>, Option<usize>) = if palette.visible().is_empty() {
        (vec![ListItem::new(Line::raw(" —"))], None)
    } else {
        (
            palette
                .visible()
                .iter()
                .map(|&i| {
                    let row = &palette.rows()[i];
                    let texto = format!(" {:<24} {:<32} {}", row.text, row.desc, row.chord);
                    ListItem::new(Line::raw(middle_ellipsis(&texto, inner)))
                })
                .collect(),
            Some(palette.cursor()),
        )
    };
    let (query, _) = display_name(palette.query_display().as_bytes());
    let footer = Line::raw(format!(
        " /{query}  {} · {} ",
        t("palette-hint"),
        t("palette-hint-help")
    ));
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("palette-title")))
        .title_style(theme.role(Role::Title))
        .title_bottom(footer)
        .border_style(theme.role(Role::ModalBorder));
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.role(Role::Selection));
    let mut state = ListState::default();
    state.select(selected);
    frame.render_stateful_widget(list, area, &mut state);
}

/// Overlay de ajustes (`app.settings`, S3): mismo idioma visual que
/// [`draw_extensions`] (Paragraph con cabeceras de sección intercaladas,
/// NO `List`/`ListState` — hay DOS grupos heterogéneos, General y Plugins,
/// y `draw_extensions` ya resolvió ese patrón) más una línea de descripción
/// RESERVADA bajo la lista (la de la fila seleccionada, [`Settings::
/// selected_desc`]) y un footer que alterna entre el filtro (navegando) y el
/// buffer de edición inline (`Settings::is_editing`). Nombre/descripción son
/// Fluent — texto PROPIO del binario, jamás de un tercero (a diferencia de
/// `draw_extensions`, que sí enmascara `name`/`publisher` de un plugin): no
/// hace falta `display_name` aquí, solo `middle_ellipsis` por ancho. El
/// buffer de edición SÍ es entrada del usuario vía terminal (paste incluido)
/// — se enmascara igual que la query, mismo contrato que `NavPopup::
/// name_input`.
fn draw_settings(frame: &mut Frame<'_>, settings: &crate::app::Settings, theme: &TuiTheme) {
    let ancho = frame
        .area()
        .width
        .saturating_sub(6)
        .clamp(30, 80)
        .min(frame.area().width);
    let alto = frame.area().height.saturating_sub(4).max(6);
    let area = centered(frame.area(), ancho, alto);
    clear_themed(frame, area, theme);

    let footer = if settings.is_editing() {
        let (buf, _) = display_name(settings.edit_buffer().unwrap_or_default().as_bytes());
        Line::raw(format!(" {buf}_  {} ", t("settings-edit-hint")))
    } else {
        let (query, _) = display_name(settings.query_display().as_bytes());
        Line::raw(format!(" /{query}  {} ", t("settings-hint")))
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("settings-title")))
        .title_style(theme.role(Role::Title))
        .title_bottom(footer)
        .border_style(theme.role(Role::ModalBorder));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    let inner_w = usize::from(inner.width);

    let mut lines: Vec<Line<'_>> = Vec::new();
    if settings.visible().is_empty() {
        lines.push(Line::raw(" —"));
    } else {
        let mut general_header = false;
        let mut plugins_header = false;
        for (pos, &real) in settings.visible().iter().enumerate() {
            let row = &settings.rows()[real];
            if row.is_plugins_note() {
                if !plugins_header {
                    lines.push(Line::styled(
                        t("settings-section-plugins"),
                        theme.role(Role::Title),
                    ));
                    plugins_header = true;
                }
            } else if !general_header {
                lines.push(Line::styled(
                    t("settings-section-general"),
                    theme.role(Role::Title),
                ));
                general_header = true;
            }
            let selected = pos == settings.cursor();
            let cursor = if selected { ">" } else { " " };
            let texto = if row.is_plugins_note() {
                format!("{cursor} {}", row.name)
            } else {
                format!("{cursor} {:<28} {}", row.name, row.value)
            };
            let mut line = Line::raw(middle_ellipsis(&texto, inner_w));
            if selected {
                line = line.style(theme.role(Role::Selection));
            }
            lines.push(line);
        }
    }
    frame.render_widget(Paragraph::new(lines), split[0]);

    let desc = settings.selected_desc().unwrap_or_default();
    let desc_line = Line::raw(format!(
        " {}",
        middle_ellipsis(desc, inner_w.saturating_sub(1))
    ));
    frame.render_widget(
        Paragraph::new(desc_line).style(theme.role(Role::BorderUnfocused)),
        split[1],
    );
}

/// Column the label starts at, in CELLS — a chord wider than this pushes it
/// right instead of overlapping, same rule as the generated keys page.
const SHORTCUT_CHORD_COLUMN: usize = 16;

/// La cabecera de sección de una pantalla, la MISMA que la página de teclas
/// generada (`crate::help::build`): dos superficies que listan lo mismo no
/// pueden llamarlo distinto.
fn shortcuts_section(screen: norte_frontend::keymap::Screen) -> String {
    match screen {
        norte_frontend::keymap::Screen::Browse => t("help-section-browse"),
        norte_frontend::keymap::Screen::Viewer => t("help-section-viewer"),
        norte_frontend::keymap::Screen::Dialog => t("help-section-dialog"),
    }
}

/// Editor de atajos (`app.shortcuts`, K3c): mismo idioma visual que
/// [`draw_settings`] —Paragraph con cabeceras de sección, filtro en el pie,
/// línea de detalle reservada abajo— con dos diferencias que son el editor:
///
/// - la lista SCROLLEA. Ajustes cabe en una pantalla; esto son todas las
///   teclas de las tres pantallas MÁS cada comando que no pulsa ninguna, y una
///   lista sin ventana dejaría el cursor fuera de la caja a las veinte filas.
/// - la línea de detalle lleva el VEREDICTO mientras se captura, que es lo que
///   el lector necesita ANTES de confirmar, y el resto del tiempo lleva las dos
///   verdades de esta terminal: `esc` cancela (así que es el único chord que no
///   se puede capturar aquí) y `mod+` es Ctrl, porque crossterm no entrega ⌘.
///
/// Chords y etiquetas ya vienen pintados y traducidos del modelo compartido
/// (`norte_frontend::shortcuts`), incluido el enmascarado de
/// [`paint_chord`](norte_frontend::keymap::paint_chord) — una capa de proyecto
/// puede bindear cualquier codepoint suelto y esto va a una terminal. Aquí solo
/// queda el ancho.
fn draw_shortcuts(frame: &mut Frame<'_>, sc: &crate::app::Shortcuts, theme: &TuiTheme) {
    let ancho = frame
        .area()
        .width
        .saturating_sub(4)
        .clamp(30, 92)
        .min(frame.area().width);
    let alto = frame.area().height.saturating_sub(2).max(6);
    let area = centered(frame.area(), ancho, alto);
    clear_themed(frame, area, theme);

    let capture = sc.capture();
    let footer = match capture {
        Some(c) if c.is_waiting() => Line::raw(format!(" {} ", t("shortcuts-capture-hint"))),
        Some(_) => Line::raw(format!(" {} ", t("shortcuts-confirm-hint"))),
        None => {
            let (query, _) = display_name(sc.query_display().as_bytes());
            Line::raw(format!(" /{query}  {} ", t("shortcuts-hint")))
        }
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("shortcuts-title")))
        .title_style(theme.role(Role::Title))
        .title_bottom(footer)
        .border_style(theme.role(Role::ModalBorder));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    let inner_w = usize::from(inner.width);

    let mut items: Vec<Line<'static>> = Vec::new();
    let mut cursor_line = 0usize;
    if sc.visible().is_empty() {
        items.push(Line::raw(" —"));
    } else {
        let mut last: Option<norte_frontend::keymap::Screen> = None;
        for (pos, &real) in sc.visible().iter().enumerate() {
            let row = &sc.rows()[real];
            if last != Some(row.screen) {
                items.push(Line::styled(
                    format!("── {} ──", shortcuts_section(row.screen)),
                    theme.role(Role::Title),
                ));
                last = Some(row.screen);
            }
            let selected = pos == sc.cursor();
            if selected {
                cursor_line = items.len();
            }
            let marker = if selected { ">" } else { " " };
            // Un comando sin tecla NO se atenúa: se puede ejecutar, es solo que
            // nada lo pulsa — y esa es justo la fila que el lector vino a
            // buscar. Atenuada se leería como «no disponible», que es la otra
            // cosa.
            let chord = if row.is_bound() {
                row.chord.clone()
            } else {
                t("shortcuts-no-key")
            };
            let pad = " ".repeat(SHORTCUT_CHORD_COLUMN.saturating_sub(chord.width()));
            let text = if row.reason.is_empty() {
                format!("{marker} {chord}{pad} {}", row.label)
            } else {
                format!("{marker} {chord}{pad} {} — {}", row.label, row.reason)
            };
            let mut line = Line::raw(middle_ellipsis(&text, inner_w));
            // La selección se PARCHEA sobre el atenuado, no lo sustituye: un
            // `Line::style` reemplaza el estilo entero, y una fila no
            // construida bajo el cursor dejaría de parecerlo justo cuando el
            // lector está a punto de actuar sobre ella.
            if selected {
                line = line.patch_style(theme.role(Role::Selection));
            }
            if row.avail != norte_frontend::keymap::Availability::Here {
                line =
                    line.patch_style(Style::default().add_modifier(ratatui::style::Modifier::DIM));
            }
            items.push(line);
        }
    }
    // Ventana alrededor del cursor: sin ella la fila seleccionada desaparece
    // por debajo del borde en cuanto la lista pasa del alto de la caja.
    let h = usize::from(split[0].height).max(1);
    let start = cursor_line
        .saturating_sub(h / 2)
        .min(items.len().saturating_sub(h));
    let end = (start + h).min(items.len());
    frame.render_widget(Paragraph::new(items[start..end].to_vec()), split[0]);

    let detail = match capture {
        Some(c) => {
            let target = norte_frontend::whichkey::pending_title(c.seq(), None);
            match c.verdict() {
                Some(v) => format!(
                    "{target} → {}",
                    norte_frontend::shortcuts::verdict_message(v, norte_i18n::active())
                ),
                None => t("shortcuts-capture-hint"),
            }
        }
        None => t("shortcuts-capture-note"),
    };
    let detail_line = Line::raw(format!(
        " {}",
        middle_ellipsis(&detail, inner_w.saturating_sub(1))
    ));
    frame.render_widget(
        Paragraph::new(detail_line).style(theme.role(Role::BorderUnfocused)),
        split[1],
    );
}

/// Viewer a pantalla completa: contenido + status propia (encoding, EOL,
/// pérdidas, truncado — el usuario SIEMPRE sabe qué mira, spec §6).
fn draw_viewer(frame: &mut Frame<'_>, viewer: &crate::viewer::Viewer, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(frame.area());
    let (title, hostil) =
        norte_frontend::path_display_with(&viewer.path, app.focused().name_encoding());
    let title = if hostil {
        format!("{HOSTILE_BADGE} {title}")
    } else {
        title
    };
    // M4-P5: indicador «via <plugin>» cuando la vista viene de un preview de
    // plugin (el plugin_name ya viene enmascarado desde el viewer).
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_style(app.theme.role(Role::Title))
        .border_style(app.theme.role(Role::BorderFocus));
    if let Some(plugin) = viewer.preview_plugin() {
        // #101: cuando la decodificación host-side fue LOSSY, un aviso (rol
        // Warning) SIGUE al «via …» — misma honestidad que el status de
        // encoding del viewer crudo, y mismo orden que la GUI
        // (`viewer_header`). ASCII (`⚠` es ambiguous-width).
        let mut spans = vec![Span::styled(
            ta("viewer-plugin-preview", &[("plugin", plugin)]),
            app.theme.role(Role::Info),
        )];
        if viewer.preview_lossy() {
            spans.push(Span::styled(
                format!(" {}", t("viewer-plugin-preview-lossy")),
                app.theme.role(Role::Warning),
            ));
        }
        block = block.title(Line::from(spans).right_aligned());
    }
    let inner_h = rows[0].height.saturating_sub(2) as usize;
    // #29/G3a (ADR 0037): un preview de plugin trae color, por ANSI-SGR
    // saneado (`fg` únicamente) o por WIT estructurado (`role` VALIDADO +
    // `fg` de respaldo). `role` GANA sobre `fg` cuando ambos están
    // presentes (el tema del usuario tiene precedencia sobre el color fijo
    // de un plugin, ADR 0037 decisión 3) — se resuelve por el tema
    // (`app.theme.role`), no como RGB crudo. Sin ninguno de los dos, el
    // color por defecto del tema (sin `.style()`).
    let lines: Vec<Line<'_>> = match viewer.plugin_styled_rows(inner_h) {
        Some(styled) => styled
            .into_iter()
            .map(|line| {
                Line::from(
                    line.iter()
                        .map(|span| {
                            let s = Span::raw(span.text.clone());
                            if let Some(role) = span.role {
                                s.style(app.theme.role(role))
                            } else if let Some((r, g, b)) = span.fg {
                                s.style(Style::default().fg(Color::Rgb(r, g, b)))
                            } else {
                                s
                            }
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect(),
        None => viewer.rows(inner_h).into_iter().map(Line::raw).collect(),
    };
    frame.render_widget(Paragraph::new(lines).block(block), rows[0]);
    let pos = format!(
        "{}/{}",
        (viewer.scroll + 1).min(viewer.total_rows().max(1)),
        viewer.total_rows().max(1)
    );
    let text = match &app.message {
        Some(msg) => format!(" {msg}"),
        None => format!(" {}  {pos}", crate::viewer::status(viewer)),
    };
    frame.render_widget(
        Paragraph::new(text).style(app.theme.role(Role::StatusBar)),
        rows[1],
    );
}

fn draw_tasks(frame: &mut Frame<'_>, area: Rect, app: &App) {
    if area.height == 0 {
        return;
    }
    let lines: Vec<Line<'_>> = app
        .board
        .rows()
        .iter()
        .rev()
        .take(area.height as usize)
        .map(|row| {
            let p = &row.last;
            let pct = match (p.bytes_total, p.entries_total) {
                (Some(total), _) if total > 0 => {
                    (p.bytes_done.saturating_mul(100) / total).min(100)
                }
                (_, Some(total)) if total > 0 => {
                    (p.entries_done.saturating_mul(100) / total).min(100)
                }
                _ => 0,
            };
            // Por CATEGORÍA (Display estable), jamás Debug de cara al usuario.
            // El estado se colorea por rol (error rojo, hecho info).
            let (estado, role) = match &p.state {
                norte_proto::TaskState::Completed => ("✓".to_owned(), Some(Role::Info)),
                norte_proto::TaskState::Cancelled => (t("task-cancelled"), Some(Role::Warning)),
                norte_proto::TaskState::Failed { error } => {
                    (format!("✗ {error}"), Some(Role::Error))
                }
                _ => (format!("{pct}%"), None),
            };
            let kind = match p.kind {
                norte_proto::TaskKind::Copy => "copy",
                norte_proto::TaskKind::Move => "move",
                norte_proto::TaskKind::Delete => "delete",
                norte_proto::TaskKind::Undo => "undo",
                // Etiqueta mínima; el diálogo/pane virtual de Alt+F7 llega en
                // T6 de liveSearch — aquí solo evita el `match` no exhaustivo.
                norte_proto::TaskKind::Search => "search",
                norte_proto::TaskKind::Index => "index",
                norte_proto::TaskKind::Mkdir => "mkdir",
                norte_proto::TaskKind::Embed => "embed",
                norte_proto::TaskKind::RenameBatch => "rename",
                // Etiqueta mínima, como la de `Search` en su día: el pane de
                // comparación llega en C7 de este mismo plan; esto solo evita
                // que una Task de `fs.compare` se pinte como genérica.
                norte_proto::TaskKind::Compare => "compare",
                // `Unknown` es la clase de un daemon N+1 que este proto YA
                // conocía como desconocida (vía `serde(other)`); el `_` es
                // `#[non_exhaustive]` (#126) — una variante de un norte-proto
                // más nuevo que este BINARIO no reconoce en absoluto. Mismo
                // caso de cara al usuario, misma etiqueta genérica.
                norte_proto::TaskKind::Unknown | _ => "task",
            };
            let head = Span::raw(format!(" {kind} #{} ", p.task_id.get()));
            let tail = match role {
                Some(r) => Span::styled(estado, app.theme.role(r)),
                None => Span::raw(estado),
            };
            Line::from(vec![head, tail])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// Ancho del modal por CONTENIDO (H1 T3 follow-up): los pies GENERADOS
/// pueden superar las 60 col históricas — p. ej. colisión: `[esc] … [w] más
/// nuevo` — y truncarlos escondería teclas reales. Techo = ancho del frame
/// menos margen; suelo = las 60 históricas. MINOR-1 (H1 close): se mide en
/// CELDAS de terminal (`UnicodeWidthStr::width`, mismo idioma que
/// [`draw_nav_popup`]/[`middle_ellipsis`]), no en `chars` — un cuerpo con
/// CJK (dos celdas por char, p. ej. un path con `日本語`) desbordaba la caja
/// con el conteo de chars antiguo.
fn modal_width(titulo: &str, cuerpo: &str, frame_width: u16) -> u16 {
    let contenido_max = cuerpo
        .lines()
        .map(UnicodeWidthStr::width)
        .chain(std::iter::once(titulo.width() + 2))
        .max()
        .unwrap_or(0);
    u16::try_from(contenido_max + 4)
        .unwrap_or(u16::MAX)
        .clamp(60, frame_width.saturating_sub(4).max(60))
}

/// Alto del modal por variante (líneas de contenido + bordes).
fn modal_height(modal: &crate::app::Modal) -> u16 {
    use crate::app::Modal;
    match modal {
        // Review H3c MINOR-5: cabecera + la VENTANA de rutas (más el resumen,
        // si el lote no cabe entero) + el pie, más los bordes — el MISMO
        // cómputo acotado que ConfirmDelete, y por la misma razón: cuántas
        // rutas trae la petición lo elige el AGENTE, y `centered` recorta
        // contra el frame, así que un alto sin tope dejaba las últimas líneas
        // sin pintar. La última es el aviso de que las teclas están inertes.
        Modal::ApproveAgentOp { req } => {
            // Mismo cómputo que `approval_modal_text`: la línea de resumen
            // aparece cuando la DECISIÓN cubre más rutas de las que se pintan,
            // aunque el recorte lo haya hecho el server (`paths_total`).
            let mostradas = req.paths.len().min(norte_frontend::MODAL_ITEM_LIMIT);
            let total = usize::try_from(req.paths_total)
                .unwrap_or(usize::MAX)
                .max(req.paths.len());
            let lineas = 1 + mostradas + usize::from(total > mostradas) + 1;
            // `+ 2` (los bordes), no el `+ 3` de ConfirmDelete: este modal
            // siempre ajustó exacto y acotar la lista no es motivo para
            // moverle la caja una fila.
            u16::try_from(lineas).unwrap_or(u16::MAX).saturating_add(2)
        }
        // #103 T10: una línea POR ítem listado (más la de resumen, si el
        // lote no cabe entero), más las dos fijas (destino/modo + teclas) y
        // los bordes — el mismo `body_lines + 3` que el resto. `centered`
        // recorta contra el frame: en un terminal enano el lote se ve a
        // medias, nunca desborda.
        Modal::ConfirmDelete { items, .. } | Modal::ConfirmTransfer { items, .. } => {
            let listadas = items.len().min(norte_frontend::MODAL_ITEM_LIMIT)
                + usize::from(items.len() > norte_frontend::MODAL_ITEM_LIMIT);
            u16::try_from(listadas)
                .unwrap_or(u16::MAX)
                .saturating_add(5)
        }
        // TrustHostKey: host + algo + fingerprint + nota + teclas (5 líneas)
        // + bordes. TransferName con error (#105): origen + dir destino +
        // campo + hint + teclas + error (6 líneas), +3.
        Modal::TrustHostKey { .. } | Modal::TransferName { error: Some(_), .. } => 9,
        // TrustLuaInit: un mensaje largo con wrap (~4 líneas a 58 cols) +
        // bordes. TransferName sin error: 5 líneas de cuerpo (origen y dir
        // destino incluidos), +3.
        Modal::TrustLuaInit { .. } | Modal::TransferName { .. } => 8,
        // Patrón/mkdir + hint + teclas (3 líneas) o + la línea de error (4),
        // más bordes (#103 T9: mismo cómputo `body_lines + 3` que el resto).
        // Sin error caen al comodín `6` de abajo (match_same_arms).
        Modal::MarkPattern { error: Some(_), .. }
        | Modal::Mkdir { error: Some(_), .. }
        | Modal::CommandLine { error: Some(_), .. }
        | Modal::AiRenameInstruction { error: Some(_), .. }
        | Modal::SemanticQuery { error: Some(_), .. } => 7,
        // M4-IA: la línea del dir (audit MAJOR-1) + el veredicto del LOTE
        // (§17) + dos por pareja de la VENTANA + el indicador (si el plan no
        // cabe entero) + el detalle del lote (contado por
        // `rename_batch_detail_lines` — la MISMA función que lo pinta, no una
        // fórmula paralela que se desincronice) + el hint, más bordes — mismo
        // cómputo dinámico `body_lines + 3` que
        // ConfirmDelete/ConfirmTransfer. Estable al scroll: la ventana
        // clampada siempre pinta `min(len, LIMIT)` parejas.
        Modal::AiRenamePlan { entries, plan, .. } => {
            let lineas = 2
                + 2 * entries.len().min(AI_RENAME_PAIR_LIMIT)
                + usize::from(entries.len() > AI_RENAME_PAIR_LIMIT)
                + plan.detail_line_count()
                + 1;
            u16::try_from(lineas).unwrap_or(u16::MAX).saturating_add(3)
        }
        // M4-IA-2: un hit POR LÍNEA de la ventana + el indicador (si el
        // lote no cabe entero) + el hint — mismo cómputo dinámico
        // `body_lines + 3` que el plan IA. Estable al scroll.
        Modal::SemanticHits { hits, .. } => {
            let lineas = hits.len().min(SEMANTIC_HIT_LIMIT)
                + usize::from(hits.len() > SEMANTIC_HIT_LIMIT)
                + 1;
            u16::try_from(lineas).unwrap_or(u16::MAX).saturating_add(3)
        }
        _ => 6,
    }
}

/// Si `modal` tiñe el borde de aviso (rol `warning`): un borrado PERMANENTE
/// o una decisión de seguridad (aprobar una op de agente, confiar en una
/// host key o en un `init.lua` de proyecto). Factorizado fuera de
/// `draw_modal` (clippy `too_many_lines`).
fn is_warning_modal(modal: &crate::app::Modal) -> bool {
    use crate::app::Modal;
    matches!(
        modal,
        Modal::ConfirmDelete {
            permanent: true,
            ..
        } | Modal::ApproveAgentOp { .. }
            | Modal::TrustHostKey { .. }
            | Modal::TrustLuaInit { .. }
    )
}

/// Caja centrada del modal.
/// `reinterpret` = enc del pane con FOCO al pintar: correcto para los
/// modales SÍNCRONOS (confirmar copy/move/delete se crea desde el pane con
/// foco y un modal abierto congela el foco — creación ≡ draw). Los ASYNC
/// (colisión) llevan su enc capturado al lanzar (`RetrySpec`, #98/M1). Los
/// paths de agentes (`ApproveAgentOp`) JAMÁS se reinterpretan: otra
/// frontera de confianza (van por `display_name` crudo a propósito). `hints`
/// (H1 T3, #24) trae los pies de página GENERADOS de cada modal — uno por
/// campo, ya resueltos del efectivo `dialog` vigente.
/// Título+cuerpo del modal activo, extraído de `draw_modal` (clippy
/// `too_many_lines` al crecer la familia de modales).
///
/// Y con S4 (#135) vuelve a pasarse del tope, esta vez sin sitio al que
/// extraer: lo que queda es una TABLA modal→texto, un brazo por variante y
/// exhaustiva a propósito (un modal nuevo no compila hasta que alguien decide
/// cómo se pinta). Partirla en dos mitades solo movería la frontera a un
/// punto arbitrario y haría más difícil ver que no falta ninguna. Mismo
/// criterio, y misma excepción, que la tabla de despacho de `main.rs`.
#[allow(clippy::too_many_lines)] // tabla modal→texto, no lógica
fn modal_title_body(
    modal: &crate::app::Modal,
    reinterpret: Option<norte_encoding::NameEncoding>,
    hints: &crate::hints::DialogHints,
) -> (String, String) {
    use crate::app::{Modal, TransferKind};
    match modal {
        // #103 T10: el lote va como LISTA — una ruta por línea, saneada y
        // truncada por la política COMPARTIDA con la GUI
        // (`norte_frontend::item_lines_with`), jamás dos rutas en la misma
        // línea (un nombre hostil fabricaría una entrada de la lista).
        Modal::ConfirmDelete { items, permanent } => (
            if *permanent {
                t("modal-delete-permanent-title")
            } else {
                t("modal-trash-title")
            },
            [
                norte_frontend::item_lines_with(items, HOSTILE_BADGE, reinterpret),
                vec![
                    if *permanent {
                        t("modal-delete-permanent-warning")
                    } else {
                        t("modal-trash-note")
                    },
                    hints.confirm.clone(),
                ],
            ]
            .concat()
            .join("\n"),
        ),
        Modal::ConfirmTransfer { kind, items, to } => (
            match kind {
                TransferKind::Copy => t("modal-copy-title"),
                TransferKind::Move => t("modal-move-title"),
            },
            [
                norte_frontend::item_lines_with(items, HOSTILE_BADGE, reinterpret),
                vec![
                    // El destino es un DIRECTORIO y va en SU línea, con la
                    // flecha FUERA de banda: ningún nombre de la lista de
                    // arriba puede imitar esta línea.
                    format!("→ {}", norte_frontend::path_display_with(to, reinterpret).0),
                    hints.confirm.clone(),
                ],
            ]
            .concat()
            .join("\n"),
        ),
        // #98/M1: la colisión llega ASYNC — usa el enc capturado al LANZAR
        // la operación (RetrySpec), jamás el del pane con foco al llegar.
        Modal::Collision { retry } => (
            t("modal-collision-title"),
            format!(
                "{}
{}
{}",
                t("modal-collision-body"),
                norte_frontend::path_display_with(&retry.to, retry.name_encoding).0,
                hints.collision
            ),
        ),
        Modal::ApproveAgentOp { req } => approval_modal_text(req, &hints.approval),
        Modal::TrustHostKey {
            host,
            port,
            algo,
            fingerprint,
            ..
        } => trust_host_modal_text(host, *port, algo, fingerprint, &hints.trust_host),
        // TOFU Lua (M4): `path` viene YA saneado por el constructor del
        // modal (`detail_for_bar`); el cuerpo es un solo mensaje largo y el
        // Paragraph de este modal lleva wrap (abajo).
        Modal::TrustLuaInit { path, hash_abbrev } => (
            t("modal-lua-trust-title"),
            ta(
                "modal-lua-trust-body",
                &[("path", path.as_str()), ("hash", hash_abbrev.as_str())],
            ),
        ),
        // S2 (`[ui] confirm_quit`): sin datos propios — un título+cuerpo
        // fijos más el hint (`hints.confirm`, ALLOW_CONFIRM reutilizado).
        Modal::ConfirmQuit => (
            t("modal-confirm-quit-title"),
            format!("{}\n{}", t("modal-confirm-quit-body"), hints.confirm),
        ),
        // #103 T9: ver `mark_pattern_modal_text` (enmascarado, no un texto
        // fijo — el patrón/error son de usuario).
        Modal::MarkPattern {
            mark,
            pattern,
            error,
        } => mark_pattern_modal_text(*mark, pattern, error.as_deref()),
        // #104: mismo enmascarado que el patrón — nombre y error son de
        // usuario (paste con bidi/invisibles incluido).
        Modal::Mkdir { name, error } => {
            free_text_modal_text("modal-mkdir", "modal-mkdir-hint", name, error.as_deref())
        }
        // M4-IA: mismo enmascarado que mkdir — instrucción y error son texto
        // de usuario (paste con bidi/invisibles incluido).
        // #135: mismo enmascarado que la instrucción IA — la línea de
        // comandos y su diagnóstico son texto de usuario.
        Modal::CommandLine { command, error } => free_text_modal_text(
            "modal-command-line",
            "modal-command-line-hint",
            command,
            error.as_deref(),
        ),
        Modal::AiRenameInstruction { instruction, error } => free_text_modal_text(
            "modal-ai-rename",
            "modal-ai-rename-hint",
            instruction,
            error.as_deref(),
        ),
        // M4-IA: dir objetivo + ventana de parejas from→to del plan
        // revisable (enmascarado defensivo, ver `ai_rename_plan_modal_text`).
        Modal::AiRenamePlan {
            dir,
            entries,
            offset,
            plan,
        } => ai_rename_plan_modal_text(dir, entries, *offset, hints, plan),
        // M4-IA-2: mismo enmascarado que la instrucción IA — consulta y
        // error son texto de usuario.
        Modal::SemanticQuery { query, error } => free_text_modal_text(
            "modal-semantic",
            "modal-semantic-hint",
            query,
            error.as_deref(),
        ),
        // M4-IA-2: ventana de hits con cursor (enmascarado defensivo, ver
        // `semantic_hits_modal_text`).
        Modal::SemanticHits {
            hits,
            offset,
            cursor,
        } => semantic_hits_modal_text(hits, *offset, *cursor, hints),
        // #105: nombre de destino editable — dir destino + campo + error,
        // todo de usuario y todo enmascarado.
        Modal::TransferName {
            kind,
            from,
            to_dir,
            name,
            error,
            enc,
            ..
        } => transfer_name_modal_text(*kind, from, to_dir, name, error.as_deref(), *enc),
    }
}

/// Pinta el modal activo: borde (de aviso en las superficies de decisión
/// duras), título y cuerpo de `modal_title_body`. `reinterpret` es la
/// reinterpretación del pane con foco AL PINTAR — los modales que capturan
/// la suya al abrir (`Collision` #98/M1, `TransferName` #105) la ignoran a
/// favor de la capturada.
fn draw_modal(
    frame: &mut Frame<'_>,
    modal: &crate::app::Modal,
    theme: &TuiTheme,
    reinterpret: Option<norte_encoding::NameEncoding>,
    hints: &crate::hints::DialogHints,
) {
    use crate::app::Modal;
    let (titulo, cuerpo) = modal_title_body(modal, reinterpret, hints);
    // Un borrado PERMANENTE (o aprobar una mutación de agente) tiñe el borde
    // de aviso (rol `warning`).
    let border = if is_warning_modal(modal) {
        theme.role(Role::Warning)
    } else {
        theme.role(Role::ModalBorder)
    };
    // Altura: fija salvo la aprobación (una línea POR ruta, H2 del auditor).
    let alto = modal_height(modal);
    let area = centered(
        frame.area(),
        modal_width(&titulo, &cuerpo, frame.area().width),
        alto,
    );
    clear_themed(frame, area, theme);
    let mut cuerpo = Paragraph::new(cuerpo).block(
        Block::default()
            .borders(Borders::ALL)
            .title(titulo)
            .title_style(theme.role(Role::Title))
            .border_style(border),
    );
    // Solo este modal envuelve: su cuerpo es UN mensaje largo; el resto ya
    // viene troceado por líneas (y el wrap podría partir un path por
    // cualquier char, cosa que los modales de rutas evitan con elipsis).
    if matches!(modal, Modal::TrustLuaInit { .. }) {
        cuerpo = cuerpo.wrap(ratatui::widgets::Wrap { trim: false });
    }
    frame.render_widget(cuerpo, area);
}

/// Bytes de la sesión para display; `None` = `?`. Sin colisión con una sesión
/// literal `"?"`: el daemon valida el charset `[A-Za-z0-9._-]` en el
/// handshake, así que `?` no es un id alcanzable.
fn session_bytes(req: &norte_proto::methods::PolicyApprovalRequired) -> &[u8] {
    req.session.as_deref().map_or(b"?", str::as_bytes)
}

/// Recorta a `max` CHARS (no bytes) con `…` final. Para strings ya
/// enmascarados que aún podrían ser kilométricos (clamp de layout, H1).
/// Texto `(título, cuerpo)` del modal de aprobación de agente (M3-3b T5).
/// TODO lo interpolado lo controla el AGENTE (encoding-auditor H1/H2/H3) y
/// esto es una decisión humana de seguridad: session y rutas pasan por el
/// MISMO enmascarado que los nombres de pane (controles/bidi/invisibles → �)
/// MÁS clamp; cada ruta va en SU línea con etiqueta fuera de banda (jamás un
/// joiner in-band que un nombre pueda imitar) y elipsis media (un `from`
/// kilométrico no expulsa el destino de la caja); el enmascarado se MARCA con
/// el badge (spec §6).
///
/// La lista se ENVENTANA en [`norte_frontend::MODAL_ITEM_LIMIT`] rutas más una
/// línea de resumen, como `ConfirmDelete`/`ConfirmTransfer` (review H3c
/// MINOR-5). Cuántas rutas trae la petición lo elige el AGENTE, y sin tope el
/// alto crecía con ellas: `centered` recorta contra el frame, así que las
/// líneas de sobra no se pintaban — incluida la ÚLTIMA, que bajo H3c es la
/// única explicación de por qué las teclas del modal no responden. El resumen
/// lleva badge si alguna ruta OCULTA es hostil (misma doctrina que el plan IA y
/// los hits semánticos: lo escondido jamás se cuela "limpio").
fn approval_modal_text(
    req: &norte_proto::methods::PolicyApprovalRequired,
    hint: &str,
) -> (String, String) {
    let session = clamp_chars(&display_name(session_bytes(req)).0, 40);
    let op = clamp_chars(&display_name(req.op.as_bytes()).0, 16);
    let mut lineas = vec![ta(
        "modal-approval-body",
        &[("session", &session), ("op", &op)],
    )];
    let limite = norte_frontend::MODAL_ITEM_LIMIT;
    for (i, p) in req.paths.iter().take(limite).enumerate() {
        let (texto, hostil) = display_name(p.as_bytes());
        lineas.push(ta(
            "modal-approval-path",
            &[
                ("badge", if hostil { HOSTILE_BADGE } else { "" }),
                ("n", &(i + 1).to_string()),
                ("path", &middle_ellipsis(&texto, 46)),
            ],
        ));
    }
    // Cuántas cubre la DECISIÓN, no cuántas llegaron: el server recorta la
    // notificación (un lote de renames gatea miles de rutas) y sin
    // `paths_total` el modal enseñaría 32 rutas inocentes como si fueran todas
    // — que es aprobar a ciegas creyendo que se aprueba a la vista. `0` =
    // server N-1 que no lo mandaba: entonces lo recibido ES todo lo que hubo.
    let total = usize::try_from(req.paths_total)
        .unwrap_or(usize::MAX)
        .max(req.paths.len());
    let mostradas = req.paths.len().min(limite);
    if total > mostradas {
        // El badge solo puede hablar de lo que se PUEDE mirar: las rutas que
        // el server recortó no están aquí para inspeccionarlas. Lo que no se
        // calla es el NÚMERO, que es lo que decide el consentimiento.
        let oculta_hostil = req
            .paths
            .iter()
            .skip(mostradas)
            .any(|p| display_name(p.as_bytes()).1);
        // Clave COMPARTIDA con `item_lines_with` (la de ConfirmDelete): el
        // resumen dice lo mismo en los dos sitios o el lector aprende dos
        // frases para un solo hecho.
        lineas.push(badge_prefixed(
            oculta_hostil,
            ta("gui-modal-more", &[("n", &(total - mostradas).to_string())]),
        ));
    }
    lineas.push(hint.to_owned());
    (t("modal-approval-title"), lineas.join("\n"))
}

/// Título+cuerpo de `Modal::MarkPattern` (#103 T9), factorizado fuera de
/// `draw_modal` (clippy `too_many_lines`). Texto libre, NO una superficie de
/// decisión de seguridad — sigue la MISMA disciplina que el resto
/// (enmascarado con `display_name`, jamás crudo): un patrón llega por paste
/// tan fácil como tecleado, y `PatternError` EMBEBE el patrón verbatim en su
/// mensaje (rustdoc de `PatternError::Glob`) — el enmascarado alcanza
/// también a la línea de error.
fn mark_pattern_modal_text(mark: bool, pattern: &str, error: Option<&str>) -> (String, String) {
    let (masked, hostil) = display_name(pattern.as_bytes());
    // #103 T9 review MINOR: `PaneState::mark_glob` compila el patrón CRUDO,
    // no el enmascarado — aquí el display difiere de verdad de lo que
    // decide el match, así que un patrón hostil lleva el mismo badge que un
    // nombre de fichero hostil (mismo idioma que `draw_search_dialog`'s
    // root line).
    let campo = if hostil {
        format!("{HOSTILE_BADGE} {masked}_")
    } else {
        format!("{masked}_")
    };
    // #103 T9 review MINOR: este modal no pasa por `DialogHints` (texto
    // libre, sin ALLOWLIST que generar un pie de página) — como
    // `search-hint`/`palette-hint`, sus teclas van fijas en Fluent.
    let mut lines = vec![
        campo,
        t("modal-mark-pattern-hint"),
        t("modal-mark-pattern-keys"),
    ];
    if let Some(err) = error {
        let (masked_err, _) = display_name(err.as_bytes());
        lines.push(masked_err);
    }
    let title = if mark {
        t("modal-mark-pattern-add")
    } else {
        t("modal-mark-pattern-remove")
    };
    (title, lines.join("\n"))
}

/// Título+cuerpo de CUALQUIER prompt de texto libre de una sola línea:
/// campo enmascarado + hint + la línea de teclas compartida + el diagnóstico
/// si lo hay.
///
/// Los tres prompts que había (`Mkdir` #104, `AiRenameInstruction` M4-IA,
/// `SemanticQuery` M4-IA-2) eran ya LA MISMA función con ids distintos, y S4
/// (#135) traía un cuarto: cuatro copias son cuatro sitios donde olvidar el
/// enmascarado, que es lo único que aquí importa (el campo y el diagnóstico
/// son texto de USUARIO — un paste con bidi/invisibles llega tan fácil a una
/// consulta como a un nombre, y el error del engine puede embeber el nombre).
/// La línea de teclas es compartida a propósito (FIX-A de la review T4): así
/// los cuatro cuadran con el brazo de altura conjunto (7 con error / 6 sin
/// él) en vez de pintar uno una línea menos.
fn free_text_modal_text(
    title_id: &str,
    hint_id: &str,
    value: &str,
    error: Option<&str>,
) -> (String, String) {
    let (masked, hostil) = display_name(value.as_bytes());
    // Ventana anclada a la DERECHA (review de S4, M4): el cuerpo del modal es
    // un `Paragraph` sin wrap y de ancho acotado, así que un valor largo
    // pintaba solo su cabeza y dejaba el cursor `_` fuera de pantalla — con
    // una línea de comandos eso es pulsar Enter sin ver lo que se ejecuta.
    // Se recorta por delante, marcando el corte, que es lo que hace cualquier
    // editor de una línea.
    let visible = tail_window(&masked, FREE_TEXT_FIELD_MAX);
    let campo = if hostil {
        format!("{HOSTILE_BADGE} {visible}_")
    } else {
        format!("{visible}_")
    };
    let mut lines = vec![campo, t(hint_id), t("modal-mark-pattern-keys")];
    if let Some(err) = error {
        let (masked_err, _) = display_name(err.as_bytes());
        lines.push(masked_err);
    }
    (t(title_id), lines.join("\n"))
}

/// Chars visibles del campo de un prompt de texto libre. Mismo presupuesto
/// que [`MODAL_PATH_CHARS`] (la caja del modal mide 60 y los bordes se llevan
/// cuatro columnas), con holgura para el badge y la marca de corte.
const FREE_TEXT_FIELD_MAX: usize = 50;

/// La COLA de `s`, con `…` delante cuando algo se quedó fuera.
///
/// Por chars y no por bytes: cortar por bytes parte un carácter multibyte, y
/// lo que se pinta son chars ya enmascarados (`display_name` no deja
/// controles ni bidi crudos, así que ninguno de los que quedan puede
/// reconfigurar la terminal al aparecer a media secuencia).
fn tail_window(s: &str, max: usize) -> String {
    let total = s.chars().count();
    if total <= max {
        return s.to_owned();
    }
    let cola: String = s.chars().skip(total - max.saturating_sub(1)).collect();
    format!("…{cola}")
}

/// Prefija el badge hostil FUERA de la traducción (audit MINOR-5: el
/// mecanismo del badge no puede depender de que cada locale conserve un
/// `{ $badge }` — concatenación Rust-side, translation-proof).
fn badge_prefixed(hostil: bool, line: String) -> String {
    if hostil {
        format!("{HOSTILE_BADGE}{line}")
    } else {
        line
    }
}

/// Título+cuerpo de `Modal::AiRenamePlan` (M4-IA, doctrina encoding-auditor):
/// primera línea = el dir OBJETIVO etiquetado fuera de banda (audit MAJOR-1
/// — el humano decide sabiendo DÓNDE aterriza el plan); después la VENTANA
/// de [`AI_RENAME_PAIR_LIMIT`] parejas desde `offset` (audit MAJOR-3: el
/// plan entero es revisable por scroll). Cada nombre en SU línea — el `from`
/// con etiqueta numerada ABSOLUTA fuera de banda (audit MINOR-4, corpus
/// `arrow_join_spoof`: un nombre puede imitar la flecha, no el `n.` al
/// margen), el `→` del destino al INICIO de su línea — elipsis media (un
/// `from` kilométrico no expulsa el `to` de la caja) y enmascarado MARCADO
/// con badge ([`badge_prefixed`], Rust-side). El indicador de desbordamiento
/// lleva badge si alguna pareja OCULTA es hostil (lo escondido no se cuela
/// limpio). Aunque el engine garantiza UTF-8 en el wire, un daemon
/// N+1/comprometido podría mandar cualquier cosa — se pinta a la defensiva
/// SIEMPRE, como el modal de aprobación.
///
/// Bajo las parejas va el veredicto del LOTE (spec §17): el estado del plan
/// que contestó `fs.rename_batch_plan` (en vuelo / aplicable / no
/// aplicable), cuántos pasos son maquinaria del planificador —el NÚMERO, no
/// los nombres `.norte-rename-…`, que nadie pidió— y las colisiones, UNA POR
/// LÍNEA con el nombre ofensor el ÚLTIMO campo (un recorte jamás puede
/// comerse el veredicto) y su índice de pareja ABSOLUTO fuera de banda, que
/// es lo que hace señalable la fila culpable. Un veredicto de un daemon más
/// nuevo degrada ESA línea a una etiqueta genérica, jamás el modal entero.
fn ai_rename_plan_modal_text(
    dir: &norte_proto::VPath,
    entries: &[norte_proto::methods::AiRenameEntry],
    offset: usize,
    dialog_hints: &crate::hints::DialogHints,
    plan: &norte_frontend::BatchPlan,
) -> (String, String) {
    // Cinturón de render: el clamp vive en `App::ai_plan_scroll`, pero un
    // offset fuera de rango jamás debe pintar una ventana vacía.
    let offset = offset.min(entries.len().saturating_sub(AI_RENAME_PAIR_LIMIT));
    let last = (offset + AI_RENAME_PAIR_LIMIT).min(entries.len());
    let (dir_txt, dir_hostil) = norte_frontend::path_display(dir);
    let mut lines = vec![badge_prefixed(
        dir_hostil,
        ta(
            "modal-ai-rename-dir",
            &[("dir", &middle_ellipsis(&dir_txt, 46))],
        ),
    )];
    // El VEREDICTO del lote va arriba, pegado al dir y ANTES de las parejas
    // (§17): un modal más alto que el terminal lo recorta `centered` por
    // ABAJO, y de todas las líneas del cuerpo esta es la que no puede
    // perderse — es la que dice si esto va a renombrar algo.
    lines.push(t(plan.status_key()));
    for (i, e) in entries.iter().enumerate().take(last).skip(offset) {
        let (from, from_hostil) = display_name(e.from.as_bytes());
        let (to, to_hostil) = display_name(e.to.as_bytes());
        lines.push(badge_prefixed(
            from_hostil,
            ta(
                "modal-ai-rename-pair-from",
                &[
                    ("n", &(i + 1).to_string()),
                    ("from", &middle_ellipsis(&from, 46)),
                ],
            ),
        ));
        lines.push(badge_prefixed(
            to_hostil,
            ta(
                "modal-ai-rename-pair-to",
                &[("to", &middle_ellipsis(&to, 44))],
            ),
        ));
    }
    if entries.len() > AI_RENAME_PAIR_LIMIT {
        let hidden_hostil = entries.iter().enumerate().any(|(i, e)| {
            (i < offset || i >= last)
                && (display_name(e.from.as_bytes()).1 || display_name(e.to.as_bytes()).1)
        });
        lines.push(badge_prefixed(
            hidden_hostil,
            ta(
                "modal-ai-rename-more",
                &[
                    ("shown", &last.to_string()),
                    ("total", &entries.len().to_string()),
                ],
            ),
        ));
    }
    // El saneado del detalle (enmascarado, elipsis, índice de pareja, tope
    // de colisiones) vive en `norte-frontend`, compartido byte a byte con la
    // GUI: `norte-gui` está FUERA del workspace y `just ci` no la compila, así
    // que una política duplicada aquí se le desviaría sin que nada avisara.
    // Este frontend solo pone SU badge.
    lines.extend(
        plan.detail_lines(entries.len())
            .into_iter()
            .map(|(linea, hostil)| badge_prefixed(hostil, linea)),
    );
    // H3c: con una ayuda encima, `y`/`n` no responden — el pie dice eso en
    // vez de ofrecerlos (gemelo de `DialogHints::with_modals_inert`, para los
    // dos modales cuya pista es prosa y no hint generado).
    //
    // Sin ayuda encima el pie sigue al gate de `dialog_action`: con un plan
    // que no se puede aplicar, confirmar está mudo y ofrecerlo sería un pie
    // que miente (misma doctrina que `modals_inert`).
    lines.push(if dialog_hints.modals_inert {
        t("modal-hint-help-open")
    } else if plan.confirmable() {
        t("modal-ai-rename-plan-hint")
    } else {
        t("modal-rename-batch-plan-hint-blocked")
    });
    (t("modal-ai-rename-plan"), lines.join("\n"))
}

/// Título+cuerpo de `Modal::SemanticHits` (M4-IA-2, doctrina
/// encoding-auditor, molde `ai_rename_plan_modal_text`): la VENTANA de
/// [`SEMANTIC_HIT_LIMIT`] hits desde `offset`, un hit POR LÍNEA con marcador
/// de cursor (`>`) y etiqueta numerada ABSOLUTA fuera de banda, path por
/// `norte_frontend::path_display` (mask + flag hostil) con badge Rust-side
/// ([`badge_prefixed`]) y elipsis media (un path kilométrico no expulsa el
/// score de la caja); el score `{:.2}` al final. El indicador de
/// desbordamiento lleva badge si algún hit OCULTO es hostil (lo escondido no
/// se cuela limpio). Aunque el engine garantiza el wire, un daemon
/// N+1/comprometido podría mandar cualquier cosa — se pinta a la defensiva
/// SIEMPRE.
fn semantic_hits_modal_text(
    hits: &[norte_proto::methods::SemanticHit],
    offset: usize,
    cursor: usize,
    dialog_hints: &crate::hints::DialogHints,
) -> (String, String) {
    // Cinturón de render: el clamp vive en `App::semantic_cursor`, pero un
    // offset fuera de rango jamás debe pintar una ventana vacía.
    let offset = offset.min(hits.len().saturating_sub(SEMANTIC_HIT_LIMIT));
    let last = (offset + SEMANTIC_HIT_LIMIT).min(hits.len());
    let mut lines = Vec::new();
    for (i, h) in hits.iter().enumerate().take(last).skip(offset) {
        let (path, hostil) = norte_frontend::path_display(&h.path);
        let line = badge_prefixed(
            hostil,
            ta(
                "modal-semantic-hit",
                &[
                    ("n", &(i + 1).to_string()),
                    ("path", &middle_ellipsis(&path, 44)),
                    ("score", &format!("{:.2}", h.score)),
                ],
            ),
        );
        // Marcador de cursor FUERA de banda, en columna fija ANTES del badge
        // (un path no puede imitarlo: va enmascarado y tras la etiqueta).
        lines.push(if i == cursor {
            format!("> {line}")
        } else {
            format!("  {line}")
        });
    }
    if hits.len() > SEMANTIC_HIT_LIMIT {
        let hidden_hostil = hits
            .iter()
            .enumerate()
            .any(|(i, h)| (i < offset || i >= last) && norte_frontend::path_display(&h.path).1);
        lines.push(badge_prefixed(
            hidden_hostil,
            ta(
                "modal-semantic-more",
                &[
                    ("shown", &last.to_string()),
                    ("total", &hits.len().to_string()),
                ],
            ),
        ));
    }
    // H3c: ver `ai_rename_plan_modal_text` — misma razón, misma cadena.
    lines.push(if dialog_hints.modals_inert {
        t("modal-hint-help-open")
    } else {
        t("modal-semantic-hits-hint")
    });
    (t("modal-semantic-hits"), lines.join("\n"))
}

/// Título+cuerpo de `Modal::TransferName` (#105): mismo contrato de
/// enmascarado que `mkdir_modal_text` — el dir destino, el nombre y el
/// diagnóstico son texto/bytes de usuario. El dir va en su propia línea
/// (jamás un joiner in-band con el nombre — disciplina de los modales de
/// #103).
fn transfer_name_modal_text(
    kind: crate::app::TransferKind,
    from: &norte_proto::VPath,
    to_dir: &norte_proto::VPath,
    name: &str,
    error: Option<&str>,
    enc: Option<norte_encoding::NameEncoding>,
) -> (String, String) {
    let (masked, hostil) = display_name(name.as_bytes());
    let campo = if hostil {
        format!("{HOSTILE_BADGE} {masked}_")
    } else {
        format!("{masked}_")
    };
    // #105 review MAJOR-2/MINOR-1: origen y dir destino, cada uno en SU
    // línea con la flecha fuera de banda, bajo la reinterpretación
    // CAPTURADA al abrir (#98/M1 — jamás la del pane al pintar). La ruta
    // va con ELIPSIS MEDIA, como en el modal de aprobación y el de
    // colisión: `modal_width` topa contra el ancho del frame y el
    // `Paragraph` de `draw_modal` no envuelve, así que una ruta honda se
    // cortaba a pelo contra el borde y expulsaba de la caja la COLA del
    // destino — justo lo que el usuario necesita ver para saber dónde
    // aterriza la copia — sin ni un `…` que lo delatara.
    let badge_line = |p: &norte_proto::VPath| {
        let (line, hostil) = norte_frontend::path_display_with(p, enc);
        let line = middle_ellipsis(&line, MODAL_PATH_CHARS);
        if hostil {
            format!("{HOSTILE_BADGE} {line}")
        } else {
            line
        }
    };
    let mut lines = vec![
        badge_line(from),
        format!("→ {}", badge_line(to_dir)),
        campo,
        t("modal-transfer-name-hint"),
        t("modal-mark-pattern-keys"),
    ];
    if let Some(err) = error {
        let (masked_err, _) = display_name(err.as_bytes());
        lines.push(masked_err);
    }
    let title = match kind {
        crate::app::TransferKind::Copy => t("modal-transfer-name-copy"),
        crate::app::TransferKind::Move => t("modal-transfer-name-move"),
    };
    (title, lines.join("\n"))
}

#[cfg(test)]
mod entry_item_columns_tests {
    use super::*;
    use norte_proto::{EntryKind, VPath};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::{List, Widget as _};

    /// review #108-5 M2: una decoración CJK (16 celdas) con la columna del
    /// nombre a su mínimo NO desplaza las celdas — la decoración cae antes
    /// que romper la alineación, y el ancho total de la fila es EXACTO.
    #[test]
    fn una_decoracion_ancha_jamas_desplaza_las_columnas() {
        use norte_frontend::columns::{Builtin, ColumnId, ColumnStyle, LayoutItem, WidthPolicy};
        let entry = norte_proto::Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse("mem:///f.txt").unwrap(),
            kind: EntryKind::File,
            size: Some(7),
            mtime_ms: None,
        };
        let deco = norte_frontend::Decoration {
            badge: Some("全全全全全全全全".to_owned()),
            role: None,
        };
        let theme = TuiTheme::default();
        let widths = [
            (
                ColumnId::Builtin(Builtin::Name),
                10u16,
                ColumnStyle::default_for(Builtin::Name),
            ),
            (
                ColumnId::Builtin(Builtin::Size),
                11u16,
                ColumnStyle::default_for(Builtin::Size),
            ),
        ];
        let _ = LayoutItem {
            policy: WidthPolicy::Auto,
            measured: 0,
            is_name: false,
        };
        let item = entry_item(&entry, &theme, None, Some(&deco), false, &widths, None, 0);
        // Renderiza a un buffer del ancho EXACTO del presupuesto: si la
        // fila desbordara, la celda de tamaño perdería su cola.
        let area = Rect::new(0, 0, 21, 1);
        let mut buf = Buffer::empty(area);
        List::new(vec![item]).render(area, &mut buf);
        let fila: String = (0..21).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert_eq!(fila, "   f.txt          7 B", "{fila:?}");
    }
}

#[cfg(test)]
mod draw_pane_attr_tests {
    use super::*;
    use norte_proto::{Segment, VPath};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn entry(dir: &VPath, name: &str) -> norte_proto::Entry {
        norte_proto::Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(name.as_bytes().to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        }
    }

    /// #117 tarea 2: celdas attr con valores HOSTILES de un provider pintadas
    /// end-to-end por `draw_pane` (config resuelta → layout → celda): jamás
    /// un char peligroso crudo, lossy MARCADO (U+FFFD) para Bytes no-UTF8,
    /// ausencia = blanco y cabecera con el id como fallback (sin catálogo).
    #[test]
    fn celdas_attr_hostiles_enmascaradas_y_ausencia_en_blanco() {
        use norte_proto::attrs::AttrValue;
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
        let pane = Pane::new(dir, vec![e1, e2]);
        let theme = TuiTheme::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 8)).expect("terminal de test");
        terminal
            .draw(|f| draw_pane(f, f.area(), &pane, true, &theme, 0, &settings, None))
            .expect("draw");
        let text = terminal.backend().to_string();
        // 1. Ninguna celda del buffer lleva un char peligroso crudo
        //    (controles, overrides bidi, invisibles — spec §6). Por línea:
        //    los `\n` que une `to_string` son del harness, no del buffer.
        assert!(
            text.lines()
                .all(|l| l.chars().all(|c| !norte_encoding::is_terminal_hazard(c))),
            "hazard crudo en el render: {text:?}"
        );
        // 2. La fila de e1 pinta el owner LOSSY y MARCADO (U+FFFD visible).
        let fila_e1 = text
            .lines()
            .find(|l| l.contains("aaa"))
            .expect("fila de aaa");
        assert!(
            fila_e1.contains('\u{FFFD}'),
            "owner lossy sin marcar: {fila_e1:?}"
        );
        // 3. La fila de e2 (sin attrs) pinta las columnas attr EN BLANCO:
        //    quitando el nombre, los bordes y los espacios no queda nada
        //    (blanco = AUSENTE, jamás un valor fabricado).
        let fila_e2 = text
            .lines()
            .find(|l| l.contains("bbb"))
            .expect("fila de bbb");
        // (Las comillas por línea las pone el Display de `TestBackend`.)
        let resto: String = fila_e2
            .replace("bbb", "")
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '│' && *c != '"')
            .collect();
        assert_eq!(resto, "", "ausencia debe ser blanco: {fila_e2:?}");
        // 4. La cabecera lleva el id como fallback (sin catálogo aquí).
        assert!(text.contains("mem.owner"), "cabecera sin id: {text}");
    }

    /// #117 encoding-audit L2: una celda attr ANCHA (CJK double-width + la
    /// familia emoji ZWJ del corpus — el valor `mem.wide` de `MemProvider`)
    /// JAMÁS desplaza la columna vecina: la x de la celda del tamaño es
    /// idéntica entre la fila ancha y una fila en blanco (espejo de
    /// `una_decoracion_ancha_jamas_desplaza_las_columnas`). Lo pineado es
    /// la alineación de celdas del buffer de ratatui; el colapso de ZWJ en
    /// un terminal real es la limitación preexistente que ya comparte la
    /// columna del nombre.
    #[test]
    fn celda_attr_ancha_jamas_desplaza_la_columna_vecina() {
        use norte_proto::attrs::AttrValue;
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec!["name".into(), "attr:mem.wide".into(), "size".into()]),
            ..Default::default()
        };
        let settings = norte_frontend::columns::ColumnsSettings::resolve(&cfg);
        let dir = VPath::parse("mem:///d").unwrap();
        let mut e1 = entry(&dir, "aaa");
        e1.attrs.insert(
            "mem.wide".into(),
            AttrValue::Text("日本語👨\u{200d}👩\u{200d}👧\u{200d}👦".into()),
        );
        let e2 = entry(&dir, "bbb"); // SIN attrs: la celda ancha en blanco
        let pane = Pane::new(dir, vec![e1, e2]);
        let theme = TuiTheme::default();
        let mut terminal = Terminal::new(TestBackend::new(60, 8)).expect("terminal de test");
        terminal
            .draw(|f| draw_pane(f, f.area(), &pane, true, &theme, 0, &settings, None))
            .expect("draw");
        let buf = terminal.backend().buffer();
        // La x (en CELDAS del buffer, no chars) del «1» del tamaño en la
        // fila que contiene `name`.
        let size_x = |name: &str| -> u16 {
            for y in 0..buf.area.height {
                let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
                if row.contains(name) {
                    for x in 0..buf.area.width {
                        if buf[(x, y)].symbol() == "1" {
                            return x;
                        }
                    }
                }
            }
            panic!("fila {name} sin celda de tamaño");
        };
        assert_eq!(
            size_x("aaa"),
            size_x("bbb"),
            "la celda ancha desplazó la columna del tamaño"
        );
    }
}

#[cfg(test)]
mod transfer_name_modal_text_tests {
    use super::transfer_name_modal_text;
    use crate::app::TransferKind;
    use norte_proto::VPath;

    /// #105 review MINOR-2 (misma clase que el M4 del patrón): fn PURA — un
    /// RLO crudo en nombre y error sale enmascarado, y un byte hostil en el
    /// ORIGEN y el dir destino jamás llega crudo (`path_display` los enmascara
    /// y llevan badge).
    #[test]
    fn masks_every_user_surface() {
        let hostile = "abc\u{202E}rid";
        let from = VPath::parse("mem:///src/a%FF.txt").unwrap();
        let to_dir = VPath::parse("mem:///dst%FE").unwrap();
        let (_, cuerpo) = transfer_name_modal_text(
            TransferKind::Move,
            &from,
            &to_dir,
            hostile,
            Some(hostile),
            None,
        );
        assert!(!cuerpo.contains('\u{202E}'), "{cuerpo:?}");
        assert!(
            cuerpo.matches('\u{FFFD}').count() >= 4,
            "nombre + error (RLO) y origen + destino (bytes): {cuerpo:?}"
        );
        assert!(
            cuerpo.matches(super::HOSTILE_BADGE).count() >= 2,
            "{cuerpo:?}"
        );
    }
}

#[cfg(test)]
mod free_text_modal_text_tests {
    use super::{free_text_modal_text, tail_window};

    /// Cada prompt de texto libre usa SUS ids y los cuatro existen en ambos
    /// locales (review de S4, m5). Sin esto, un id con typo se pintaría tal
    /// cual en pantalla —Fluent cae al propio id— con el gate en verde.
    #[test]
    fn cada_prompt_resuelve_su_titulo_y_su_hint() {
        let casos = [
            ("modal-mkdir", "modal-mkdir-hint"),
            ("modal-command-line", "modal-command-line-hint"),
            ("modal-ai-rename", "modal-ai-rename-hint"),
            ("modal-semantic", "modal-semantic-hint"),
        ];
        for (titulo, hint) in casos {
            let (t, cuerpo) = free_text_modal_text(titulo, hint, "x", None);
            assert_ne!(t, titulo, "{titulo} sin traducción: sale el id crudo");
            let linea_hint = cuerpo.lines().nth(1).expect("hint");
            assert_ne!(linea_hint, hint, "{hint} sin traducción: sale el id crudo");
        }
    }

    /// El campo enseña la COLA, con la marca del corte, para que el cursor
    /// esté siempre a la vista: un comando cuyo final no se ve es un comando
    /// que se ejecuta a ciegas.
    #[test]
    fn un_valor_largo_ensena_su_cola_y_marca_el_corte() {
        let largo = "a".repeat(300);
        let (_, cuerpo) = free_text_modal_text(
            "modal-command-line",
            "modal-command-line-hint",
            &largo,
            None,
        );
        let campo = cuerpo.lines().next().expect("campo");
        assert!(campo.starts_with('…'), "el corte se marca: {campo:?}");
        assert!(campo.ends_with('_'), "y el cursor se ve: {campo:?}");
        assert!(
            campo.chars().count() <= 52,
            "acotado: {}",
            campo.chars().count()
        );
    }

    /// `tail_window` cuenta CHARS, no bytes: cortar por bytes partiría un
    /// carácter multibyte por la mitad.
    #[test]
    fn la_ventana_de_cola_cuenta_chars() {
        assert_eq!(tail_window("abc", 10), "abc");
        assert_eq!(tail_window("abcdef", 3), "…ef");
        let cjk = "日本語のファイル";
        let w = tail_window(cjk, 4);
        assert_eq!(w.chars().count(), 4);
        assert!(w.starts_with('…'));
    }

    /// Mismo pin que el del patrón (#103 M4): fn PURA — un RLO crudo en el
    /// nombre Y en el error sale enmascarado en AMBAS líneas.
    #[test]
    fn masks_a_raw_rtl_override_in_name_and_error() {
        let hostile = "abc\u{202E}rid";
        let (_, cuerpo) =
            free_text_modal_text("modal-mkdir", "modal-mkdir-hint", hostile, Some(hostile));
        assert!(!cuerpo.contains('\u{202E}'), "{cuerpo:?}");
        assert_eq!(cuerpo.matches('\u{FFFD}').count(), 2, "{cuerpo:?}");
    }
}

#[cfg(test)]
mod mark_pattern_modal_text_tests {
    use super::mark_pattern_modal_text;

    /// Review MAJOR M4: `mark_pattern_modal_text` es pura — testear el
    /// enmascarado directamente en vez de a través de un buffer
    /// `TestBackend`, donde el renderer de párrafo de ratatui se COME los
    /// grafemas de ancho cero: U+202E jamás sobrevive AHÍ, enmascarado o
    /// no, así que una aserción de test de render contra él no puede fallar
    /// nunca (la clase de bug que motivó este test). Un patrón Y un error
    /// que llevan un RLO crudo deben salir enmascarados los DOS: ni un
    /// U+202E sobrevive, y U+FFFD aparece exactamente dos veces — una por
    /// línea enmascarada.
    #[test]
    fn masks_a_raw_rtl_override_in_both_the_pattern_and_the_error() {
        let hostile = "abc\u{202E}gpj.exe";
        let (_, cuerpo) = mark_pattern_modal_text(true, hostile, Some(hostile));
        assert!(
            !cuerpo.contains('\u{202E}'),
            "raw RTL override must not survive: {cuerpo:?}"
        );
        assert_eq!(
            cuerpo.matches('\u{FFFD}').count(),
            2,
            "one U+FFFD per masked line (pattern + error): {cuerpo:?}"
        );
    }

    /// Sin error, solo la línea del patrón se enmascara: un solo U+FFFD.
    #[test]
    fn masks_only_the_pattern_line_when_there_is_no_error() {
        let hostile = "abc\u{202E}gpj.exe";
        let (_, cuerpo) = mark_pattern_modal_text(true, hostile, None);
        assert_eq!(cuerpo.matches('\u{FFFD}').count(), 1);
    }
}

/// Texto `(título, cuerpo)` del modal TOFU (#45). host/algo/fingerprint
/// vienen del SERVIDOR REMOTO (no confiable) y esto es una decisión de
/// seguridad: mismo enmascarado que las rutas de agente (controles/bidi/
/// invisibles → �) + clamp. El fingerprint legítimo es ASCII
/// (`SHA256:<base64>`), así que el enmascarado es un no-op salvo que el
/// server intente ocultar caracteres — en cuyo caso el � DELATA la
/// manipulación.
fn trust_host_modal_text(
    host: &str,
    port: Option<u16>,
    algo: &str,
    fingerprint: &str,
    hint: &str,
) -> (String, String) {
    let (host_txt, host_hostil) = display_name(host.as_bytes());
    let hostport = match port {
        Some(p) => format!("{}:{p}", clamp_chars(&host_txt, 48)),
        None => clamp_chars(&host_txt, 48),
    };
    let (algo_disp, algo_hostil) = display_name(algo.as_bytes());
    let algo_txt = clamp_chars(&algo_disp, 24);
    let (fp_txt, fp_hostil) = display_name(fingerprint.as_bytes());
    let lineas = [
        ta(
            "modal-trust-host-host",
            &[
                ("badge", if host_hostil { HOSTILE_BADGE } else { "" }),
                ("host", &hostport),
            ],
        ),
        ta(
            "modal-trust-host-algo",
            &[
                ("badge", if algo_hostil { HOSTILE_BADGE } else { "" }),
                ("algo", &algo_txt),
            ],
        ),
        ta(
            "modal-trust-host-fp",
            &[
                ("badge", if fp_hostil { HOSTILE_BADGE } else { "" }),
                ("fingerprint", &clamp_chars(&fp_txt, 52)),
            ],
        ),
        t("modal-trust-host-note"),
        hint.to_owned(),
    ];
    (t("modal-trust-host-title"), lineas.join("\n"))
}

fn clamp_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Elipsis MEDIA por ancho de celda: AHORA vive en `norte-frontend`
/// (encoding audit M4-IA-2 H1) — el invariante «un path kilométrico jamás
/// expulsa el campo que va detrás» no es propio de un terminal, la GUI lo
/// necesitaba igual. Re-import local para que todo el módulo (y sus tests)
/// la llame por su nombre corto, sin cambiar una sola salida de render.
use norte_frontend::middle_ellipsis;

fn centered(base: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(base.width);
    let h = h.min(base.height);
    Rect {
        x: base.x + (base.width - w) / 2,
        y: base.y + (base.height - h) / 2,
        width: w,
        height: h,
    }
}

/// Columnas VIVAS de un pane con su estilo resuelto (#108 7b, #117 sobre
/// `ColumnId`): los anchos del layout compartido más `style_for_id`, UNA
/// vez por columna y por frame (`style_for_id` pliega mapas y clona el
/// header — por fila × columna sería O(filas × columnas) de lookups
/// idénticos). El catálogo viene del cache por scheme de `App` (#117
/// tarea 2): refina los defaults de las columnas attr (hint); `None` =
/// aún no llegó o falló — defaults Opaque, jamás bloquea el render.
fn styled_columns(
    settings: &norte_frontend::columns::ColumnsSettings,
    scheme: &str,
    inner_w: u16,
    catalog: Option<&norte_proto::AttrCatalog>,
) -> Vec<(
    norte_frontend::columns::ColumnId,
    u16,
    norte_frontend::columns::ColumnStyle,
)> {
    norte_frontend::columns::column_widths(settings, scheme, inner_w)
        .into_iter()
        .map(|(id, w)| {
            let s = settings.style_for_id(scheme, &id, catalog);
            (id, w, s)
        })
        .collect()
}

/// Pinta el panel de diferencias (`Shift+F2`): cabecera con las dos raíces,
/// una fila por pareja con las dos caras y las dos marcas entre ellas, y un
/// pie con el lado activo, los filtros y el estado del run.
///
/// Nada de lo que decide QUÉ se ve está aquí (regla dura 7): las filas
/// visibles, la selección y las marcas salen de
/// [`norte_frontend::compare`], que se testea sin terminal. Este lado reparte
/// anchos y elige colores.
///
/// `size_hints` es la caché de presentación de la sonda #157
/// (`App::compare_size_hints`): una superposición sobre `RowFace::size`, NO
/// una mutación de las filas del modelo (`ComparePane` no expone ninguna vía
/// para eso, a propósito — sus filas no cambian tras `extend`). Solo se
/// consulta cuando el propio `Entry` no trajo tamaño; un tamaño real del
/// listado nunca se pisa.
fn draw_compare(
    frame: &mut Frame<'_>,
    area: Rect,
    view: &crate::app::CompareView,
    theme: &TuiTheme,
    size_hints: &std::collections::HashMap<norte_proto::VPath, u64>,
) {
    use norte_frontend::compare::cells_for;

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.role(Role::BorderFocus))
        // #185: cada raíz llega en su propio span, con el separador en el
        // suyo — ver `compare_title` para el porqué.
        .title(compare_title(view, area.width, theme))
        .title_bottom(Span::styled(
            compare_status_line(view),
            theme.role(Role::Info),
        ));
    let outer = block.inner(area);
    frame.render_widget(block, area);
    if outer.width == 0 || outer.height == 0 {
        return;
    }
    // Dos filas de pie DENTRO del marco: los filtros con sus cuentas, y las
    // teclas. Iban las tres cosas en el título de abajo y a 80 columnas se
    // cortaba a media palabra — el snapshot lo cazó, que es exactamente para
    // lo que está. Con el marco tan corto que no caben, la lista se queda con
    // todo: un panel sin filas no explica nada.
    let (cabecera, inner, filtros_area, teclas_area) = compare_layout(outer);

    // Anchos: las dos marcas y su separación en el centro, el resto a partes
    // iguales entre las dos caras. `saturating_sub` porque un terminal
    // estrecho es un terminal, no un panic.
    let sides = inner.width.saturating_sub(COMPARE_MARKS_W + 1);
    let face_w = usize::from(sides / 2).max(1);

    if let Some(a) = cabecera {
        frame.render_widget(compare_header(face_w, theme), a);
    }

    // Solo se CONSTRUYE lo que cabe en pantalla (review BLOCKER-2). Antes se
    // construía un `ListItem` —tres spans y dos `format!`— por cada fila
    // VISIBLE, no por cada fila pintada: a cien mil filas eso es medio millón
    // de asignaciones por frame, diez veces por segundo mientras el walk
    // sigue alimentando. En remoto el propio pintor era entonces lo que
    // llenaba el canal de filas, cuyos lotes `route_batch` DESCARTA — es
    // decir, el cliente destruía la completitud de la respuesta y luego
    // culpaba al transporte con «se perdieron algunas por el camino».
    let visibles = view.pane.visible_len();
    let alto = usize::from(inner.height);
    let selected = view.pane.visible_index();
    let offset = list_offset(selected, visibles, inner.height);
    let rows: Vec<ListItem<'_>> = view
        .pane
        .visible()
        .skip(offset)
        .take(alto)
        .map(|row| {
            let mut cells = cells_for(row, view.left_encoding, view.right_encoding);
            // #157: el `Entry` no trajo tamaño (huérfano, directorio o
            // enlace — ningún rung de la comparación lo mira), pero la
            // sonda de la fila seleccionada puede haberlo hidratado desde
            // entonces. Solo se rellena el HUECO: un tamaño que el listado
            // sí trajo no se toca.
            for (face, entry) in [
                (cells.left.as_mut(), row.left.as_ref()),
                (cells.right.as_mut(), row.right.as_ref()),
            ] {
                if let (Some(face), Some(entry)) = (face, entry)
                    && face.size.is_none()
                    && let Some(&hinted) = size_hints.get(&entry.path)
                {
                    face.size = Some(hinted);
                }
            }
            // La marca de selección va a la IZQUIERDA del todo, fuera de las
            // dos caras: es una decisión del lector sobre la fila entera, no
            // sobre uno de los dos lados.
            let marca = if view.pane.is_marked(row.id) {
                '*'
            } else {
                ' '
            };
            ListItem::new(Line::from(vec![
                Span::styled(marca.to_string(), theme.role(Role::Selection)),
                compare_face_span(cells.left.as_ref(), face_w, theme),
                Span::styled(
                    format!(" {}{} ", cells.glyphs.verdict, cells.glyphs.confidence),
                    compare_mark_style(theme, row.verdict),
                ),
                compare_face_span(cells.right.as_ref(), face_w, theme),
            ]))
        })
        .collect();
    if visibles == 0 {
        // «Todavía no hay filas» y «están todas ocultas» no son lo mismo: la
        // segunda la desmienten las propias cuentas de la línea de filtros, y
        // lo que toca hacer después es distinto (revisión rust MINOR-2 de la
        // GUI; la TUI tenía el mismo hueco).
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                t(if view.pane.is_empty() {
                    "compare-empty"
                } else {
                    "compare-all-filtered"
                }),
                theme.role(Role::Info),
            ))),
            inner,
        );
        return;
    }
    // La ventana ya está recortada, así que el índice del widget es relativo
    // a ella. Una selección que un filtro esconde no resalta nada, que es la
    // respuesta honesta.
    let mut state = ListState::default();
    state.select(
        selected
            .and_then(|i| i.checked_sub(offset))
            .filter(|i| *i < alto),
    );
    frame.render_stateful_widget(
        List::new(rows).highlight_style(theme.role(Role::Selection)),
        inner,
        &mut state,
    );
    if let Some(a) = filtros_area {
        frame.render_widget(
            Paragraph::new(Line::from(compare_filter_spans(view, theme))),
            a,
        );
    }
    if let Some(a) = teclas_area {
        // Las teclas de sincronizar caben en la MISMA línea, y esa es la razón
        // de que la línea entera perdiera los corchetes: a 80 columnas el
        // marco tiene 78 y la versión con corchetes se cortaba a media
        // palabra. El recuento de marcas no está aquí sino en el pie del
        // marco, que sí tiene sitio — el snapshot es lo que lo destapó, que es
        // exactamente para lo que está.
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                t("compare-hint"),
                theme.role(Role::Info),
            ))),
            a,
        );
    }
}

/// Ancho que se llevan las dos marcas del centro, con su separación.
const COMPARE_MARKS_W: u16 = 5;

/// El separador ESTRUCTURAL del título del panel de comparación (#185): va
/// en su propio `Span`, con su propio rol, para que un `↔` incrustado en un
/// nombre de raíz (fixture `arrow_join_spoof`) no se pueda confundir con él.
const COMPARE_TITLE_SEP: &str = " ↔ ";

/// El título del marco del panel de comparación: las dos raíces, cada una en
/// su propio `Span`.
///
/// #185: antes las dos raíces iban UNIDAS en una sola cadena, y eso se podía
/// falsificar. `↔` es imprimible corriente —`display_name_with` no lo
/// enmascara y no sale badge—, así que un directorio llamado
/// `docs ↔ ⟨file⟩/home/victima/backup` se leía como OTRO par de raíces; y una
/// raíz izquierda larga expulsaba a la derecha entera por el truncado del
/// bloque, sin `…`. Un título de `Block` de ratatui no se puede partir en
/// elementos como hace la GUI (`compare_view::title_text`): se maqueta como
/// una sola línea que el marco recorta ENTERA por la derecha si no cabe,
/// aunque esa línea lleve varios `Span`s. Por eso `compare_title_halves`
/// reparte el ancho ANTES de construir ningún span —igual que
/// `sync_step_item` reparte `ruta_w` antes de separar origen y destino
/// (commit d984f83)— y el separador va en su PROPIO span con un rol
/// distinto: un `↔` incrustado en un nombre es texto de raíz y se pinta como
/// tal, así que el de verdad se distingue por estilo aunque el glifo sea el
/// mismo.
fn compare_title(
    view: &crate::app::CompareView,
    frame_width: u16,
    theme: &TuiTheme,
) -> Line<'static> {
    let (izq, der) = compare_title_halves(view, usize::from(frame_width));
    let badge_span = |h: bool| {
        Span::styled(
            if h { HOSTILE_BADGE } else { "" },
            theme.role(Role::Warning),
        )
    };
    Line::from(vec![
        Span::styled(
            format!(" {} — ", t("compare-title")),
            theme.role(Role::Title),
        ),
        badge_span(izq.hostile),
        Span::styled(izq.text, theme.role(Role::Title)),
        Span::styled(COMPARE_TITLE_SEP, theme.role(Role::Info)),
        badge_span(der.hostile),
        Span::styled(der.text, theme.role(Role::Title)),
        Span::raw(" "),
    ])
}

/// Una de las dos raíces del título del panel de comparación, ya recortada
/// para caber en el presupuesto que le tocó.
struct CompareTitleHalf {
    /// El texto YA acotado por celdas (`middle_ellipsis`).
    text: String,
    /// Si el saneado alteró el nombre — el badge va en un span propio.
    hostile: bool,
}

/// Reparte el ancho disponible del título del marco entre las dos raíces,
/// ANTES de construir ningún span.
///
/// Esto es lo que evita los dos defectos de #185 a la vez: el presupuesto
/// para el prefijo, el separador, el sufijo y las dos marcas se descuenta
/// PRIMERO, y lo que sobra se reparte a la mitad entre las dos raíces — así
/// una raíz izquierda larga nunca se come a la derecha (se recorta con `…`,
/// nunca en silencio), y el `↔` real siempre llega en su propio span porque
/// nunca compite por espacio con el texto de una raíz.
fn compare_title_halves(
    view: &crate::app::CompareView,
    frame_width: usize,
) -> (CompareTitleHalf, CompareTitleHalf) {
    let (left_txt, left_hostil) =
        norte_frontend::path_display_with(&view.left_root, view.left_encoding);
    let (right_txt, right_hostil) =
        norte_frontend::path_display_with(&view.right_root, view.right_encoding);
    let badge_w = |h: bool| if h { HOSTILE_BADGE.width() } else { 0 };
    let prefix_w = format!(" {} — ", t("compare-title")).width();
    // Bordes del marco (2) + prefijo + separador + el espacio final + las
    // dos marcas — todo lo que NO es texto de raíz, reservado antes de
    // repartir lo que queda.
    let fixed =
        2 + prefix_w + COMPARE_TITLE_SEP.width() + 1 + badge_w(left_hostil) + badge_w(right_hostil);
    let roots_w = frame_width.saturating_sub(fixed).max(2);
    let left_w = (roots_w / 2).max(1);
    let right_w = roots_w.saturating_sub(left_w).max(1);
    (
        CompareTitleHalf {
            text: norte_frontend::middle_ellipsis(&left_txt, left_w),
            hostile: left_hostil,
        },
        CompareTitleHalf {
            text: norte_frontend::middle_ellipsis(&right_txt, right_w),
            hostile: right_hostil,
        },
    )
}

#[cfg(test)]
mod compare_title_tests {
    use super::compare_title_halves;
    use norte_proto::VPath;

    fn vp(s: &str) -> VPath {
        VPath::parse(s).expect("vpath")
    }

    fn vista(left: VPath, right: VPath) -> crate::app::CompareView {
        crate::app::CompareView::new(left, right, 0, None, None)
    }

    /// #185: un nombre con una flecha DENTRO (fixture `arrow_join_spoof`) se
    /// queda en su propia mitad — nunca se confunde con el separador real, y
    /// la otra raíz llega intacta.
    #[test]
    fn arrow_join_spoof_no_fabrica_pareja() {
        let spoof = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "arrow_join_spoof")
            .expect("fixture del corpus");
        let seg = norte_proto::Segment::new(spoof.bytes.clone()).expect("segmento");
        let izquierda = vp("mem:///izquierda").join(seg);
        let derecha = vp("mem:///derecha/de/verdad");
        let (izq, der) = compare_title_halves(&vista(izquierda, derecha), 200);
        assert!(
            izq.text.contains('→'),
            "la flecha se queda DENTRO de su mitad: {}",
            izq.text
        );
        assert!(
            der.text.ends_with("de/verdad"),
            "y la derecha llega intacta a la suya: {}",
            der.text
        );
    }

    /// Una raíz izquierda kilométrica se recorta CON marca (`…`), nunca en
    /// silencio, y no se come a la derecha: el reparto de ancho es POR
    /// MITAD, reservado antes de construir ningún span.
    #[test]
    fn raiz_larga_se_recorta_y_no_expulsa_a_la_otra() {
        let larga =
            vp("mem:///").join(norte_proto::Segment::new(vec![b'x'; 4096]).expect("segmento"));
        let derecha = vp("mem:///derecha/de/verdad");
        let (izq, der) = compare_title_halves(&vista(larga, derecha), 60);
        assert!(izq.text.contains('…'), "el corte se MARCA: {}", izq.text);
        assert!(
            der.text.ends_with("de/verdad") || der.text.contains("de/verdad"),
            "la otra raíz sigue intacta: {}",
            der.text
        );
    }

    /// Con espacio de sobra las dos raíces llegan completas, sin badge (no
    /// son hostiles).
    #[test]
    fn sin_saneado_las_dos_raices_llegan_completas() {
        let (izq, der) =
            compare_title_halves(&vista(vp("mem:///izquierda"), vp("mem:///derecha")), 200);
        assert!(!izq.hostile);
        assert!(!der.hostile);
        assert!(izq.text.contains("izquierda"));
        assert!(der.text.contains("derecha"));
    }
}

/// Reparte el interior del marco: cabecera de columnas, lista, filtros y
/// teclas.
///
/// Con el marco tan corto que no caben las tres filas de cromo, la LISTA se
/// las queda todas: un panel sin filas no explica nada, y las teclas ya están
/// en la ayuda.
fn compare_layout(outer: Rect) -> (Option<Rect>, Rect, Option<Rect>, Option<Rect>) {
    if outer.height < 5 {
        return (None, outer, None, None);
    }
    let filas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(outer);
    (Some(filas[0]), filas[1], Some(filas[2]), Some(filas[3]))
}

/// La cabecera de columnas del panel de diferencias.
///
/// Es CHROME, fuera de la lista: dentro de ella era la fila 0 y se iba con el
/// scroll en cuanto se pasaba de la primera pantalla. Los panes normales la
/// pintan así por lo mismo.
fn compare_header(face_w: usize, theme: &TuiTheme) -> Paragraph<'static> {
    Paragraph::new(Line::from(Span::styled(
        format!(
            " {:<face_w$} {:^3} {:<face_w$}",
            norte_frontend::middle_ellipsis(&t("compare-header-left"), face_w),
            "",
            norte_frontend::middle_ellipsis(&t("compare-header-right"), face_w),
        ),
        theme
            .role(Role::Regular)
            .add_modifier(ratatui::style::Modifier::DIM),
    )))
}

/// Una cara de una fila del panel de diferencias: el nombre YA enmascarado
/// (badgeado si el saneado lo alteró) y su tamaño pegado a la derecha.
///
/// El lado vacío de un huérfano se pinta en BLANCO y no con un guion ni un
/// «—»: la columna de al lado ya dice `<` o `>`, y un relleno inventado en la
/// cara vacía es lo que hace que un huérfano se lea como una pareja.
fn compare_face_span(
    face: Option<&norte_frontend::compare::RowFace>,
    face_w: usize,
    theme: &TuiTheme,
) -> Span<'static> {
    let Some(f) = face else {
        return Span::raw(" ".repeat(face_w));
    };
    let name = if f.hostile {
        format!("{HOSTILE_BADGE} {}", f.name)
    } else {
        f.name.clone()
    };
    let size = f.size.map_or_else(String::new, norte_frontend::human_bytes);
    // El nombre se recorta por el MEDIO (#79: por CELDAS y no por chars — un
    // nombre CJK desbordaría el presupuesto y se comería la cola por la
    // derecha).
    let room = face_w.saturating_sub(size.chars().count() + 1).max(1);
    let name = norte_frontend::middle_ellipsis(&name, room);
    let pad = face_w.saturating_sub(UnicodeWidthStr::width(name.as_str()) + size.chars().count());
    Span::styled(
        format!("{name}{}{size}", " ".repeat(pad.max(1))),
        // Del nombre CRUDO y no del enmascarado: el tema casa la extensión
        // contra los bytes reales, y casarla contra la forma pintada daría a
        // un nombre no-UTF8 un color en el listado y otro en la comparación
        // de ese mismo listado.
        theme.entry(&f.raw_name, f.kind),
    )
}

/// El título de abajo: cómo va (o cómo acabó) la comparación, y sobre qué
/// lado actúan los comandos de siempre.
///
/// La frase la compone [`norte_frontend::compare::status_line`], COMPARTIDA
/// con la GUI: es la que dice si la respuesta está completa, y en una
/// comparación eso es toda la respuesta — dos superficies componiéndola por
/// su cuenta es exactamente lo que hizo que el CLI (fase A) y la tool MCP
/// (fase B) dieran por completa una respuesta a la que le faltaban lotes.
/// Aquí solo quedan los espacios del título del marco.
fn compare_status_line(view: &crate::app::CompareView) -> String {
    format!(
        " {} ",
        norte_frontend::compare::status_line(view, view.pane.marked_len(), norte_i18n::active())
    )
}

/// La fila de filtros: la tecla, si está encendido o apagado, el nombre y la
/// cuenta de filas que hay en esa categoría.
///
/// Un filtro APAGADO se marca con un glifo (`-` frente a `+`) y no solo con
/// un color (spec §17), y la cuenta se sigue enseñando: esconder categorías
/// es justo lo que haría mentir al panel si no lo dijera.
fn compare_filter_spans(view: &crate::app::CompareView, theme: &TuiTheme) -> Vec<Span<'static>> {
    use norte_frontend::compare::CATEGORIES;

    let mut spans = Vec::new();
    for (i, c) in CATEGORIES.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", theme.role(Role::Info)));
        }
        let apagado = view.pane.is_hidden(*c);
        let marca = if apagado { '-' } else { '+' };
        spans.push(Span::styled(
            format!(
                "{}{marca}{} {}",
                i + 1,
                c.label(norte_i18n::active()),
                view.pane.count_of(*c)
            ),
            if apagado {
                theme
                    .role(Role::Info)
                    .add_modifier(ratatui::style::Modifier::DIM)
            } else {
                theme.role(Role::Regular)
            },
        ));
    }
    spans
}

/// El color de las dos marcas de una fila. El GLIFO ya distingue el veredicto
/// sin color ninguno (spec §17, `norte_frontend::compare::verdict_glyph`);
/// esto solo lo refuerza para quien sí lo ve.
fn compare_mark_style(theme: &TuiTheme, verdict: norte_proto::methods::CompareVerdict) -> Style {
    use norte_proto::methods::CompareVerdict as V;
    match verdict {
        V::Same => theme.role(Role::Regular),
        V::Different | V::OnlyLeft | V::OnlyRight => theme.role(Role::Warning),
        V::TypeMismatch | V::Ambiguous | V::Error => theme.role(Role::Error),
        _ => theme.role(Role::Info),
    }
}

/// Pinta el panel de sincronización: el resumen del plan, sus pasos y la
/// pregunta que falte.
///
/// Todo lo que dice sale de [`norte_frontend::sync`] (regla dura 7): el
/// resumen, las tres marcas de cada paso, qué devuelve el undo y la segunda
/// pregunta. Aquí solo se reparte el sitio y se elige el color, y el color
/// nunca es lo único que distingue nada (§17) — las marcas son glifos ASCII.
fn draw_sync(frame: &mut Frame<'_>, area: Rect, view: &crate::app::SyncView, theme: &TuiTheme) {
    let (source_txt, source_hostil) =
        norte_frontend::path_display_with(&view.source_root, view.source_encoding);
    // Con la reinterpretación del DESTINO, no la del origen: un share CP1251
    // en el otro pane se pintaba `????` en el título aunque el lector hubiera
    // pulsado `Alt+E` sobre él.
    let (dest_txt, dest_hostil) =
        norte_frontend::path_display_with(&view.dest_root, view.dest_encoding);
    let badge = |h: bool| if h { HOSTILE_BADGE } else { "" };
    // El brazo `_` NO cae en «actualizar»: `SyncMode` es `#[non_exhaustive]`,
    // y decir «esto no borra» de un modo que esta build no sabe nombrar es
    // afirmar la mitad SEGURA de lo que hay que aprobar. Misma regla que
    // `RelAnchor::Either` y `StepUndo::Unclear` en el mismo modelo.
    // Por el compartido: esta decisión estaba escrita también en la GUI, con
    // su misma regla de que el `_` NO cae a «update» (revisión de rama de C2,
    // rust MAJOR-3).
    let modo = norte_frontend::sync::mode_label(view.mode, norte_i18n::active());
    // La FLECHA es el sentido, y es la mitad de lo que se aprueba: origen a la
    // izquierda del `→`, destino a la derecha, siempre, sin depender de qué
    // pane sea cuál.
    //
    // #185 cubre TAMBIÉN este título, y aquí es la línea que dice qué árbol se
    // sobrescribe: las dos raíces van unidas en una sola cadena, así que un
    // `→` dentro de un nombre finge la pareja, y una raíz de origen larga
    // expulsa la de destino entera —sin `…`— porque el recorte del título del
    // bloque es de ratatui. El panel de diferencias tiene la misma nota sobre
    // su `↔`; la GUI cierra los dos con separadores estructurales.
    let title = format!(
        " {} ({modo}) — {}{} → {}{} ",
        t("sync-title"),
        badge(source_hostil),
        source_txt,
        badge(dest_hostil),
        dest_txt
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.role(if view.confirming.is_some() {
            Role::Warning
        } else {
            Role::BorderFocus
        }))
        .title(Span::styled(title, theme.role(Role::Title)))
        .title_bottom(Span::styled(sync_status_line(view), theme.role(Role::Info)));
    let outer = block.inner(area);
    frame.render_widget(block, area);
    if outer.width == 0 || outer.height == 0 {
        return;
    }
    let (resumen_area, inner, teclas_area) = sync_layout(outer, view);
    if let Some(a) = resumen_area {
        frame.render_widget(sync_summary(view, theme), a);
    }
    // Los pasos se pintan LLEGANDO, no solo cerrados: mientras el plan viaja
    // `SyncState::plan()` contesta `None` y el pie ya está contando «6 pasos»
    // — un hueco vacío debajo era la pantalla contradiciéndose.
    let steps = view.steps();
    if steps.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                t("sync-empty"),
                theme.role(Role::Info),
            ))),
            inner,
        );
    } else {
        let alto = usize::from(inner.height);
        let selected = view
            .state
            .plan()
            .and_then(norte_frontend::sync::SyncPlan::selected_id)
            .and_then(|id| steps.iter().position(|s| s.id == id));
        let offset = list_offset(selected, steps.len(), inner.height);
        // Solo se construye lo que cabe, por lo mismo que en el panel de
        // diferencias: un plan puede tener cientos de miles de pasos y esto se
        // repinta diez veces por segundo mientras siguen llegando.
        let rows: Vec<ListItem<'_>> = steps
            .iter()
            .skip(offset)
            .take(alto)
            .map(|step| sync_step_item(step, view, usize::from(inner.width), theme))
            .collect();
        let mut state = ListState::default();
        state.select(
            selected
                .and_then(|i| i.checked_sub(offset))
                .filter(|i| *i < alto),
        );
        frame.render_stateful_widget(
            List::new(rows).highlight_style(theme.role(Role::Selection)),
            inner,
            &mut state,
        );
    }
    if let Some(a) = teclas_area {
        // La pregunta y CÓMO se contesta van en dos líneas, no en una: a 80
        // columnas la pregunta sola ya llena la fila, y la versión unida se
        // cortaba justo por donde decía qué tecla la contesta — que es la
        // mitad que hace falta. Lo cazó el snapshot.
        //
        // Qué línea toca lo decide `norte_frontend::sync::hint_id`, la
        // COMPARTIDA (#161): este `match` tenía el brazo de `sync-hint`
        // condicionado solo a `awaiting_approval()`, así que un plan cerrado
        // pero NO aprobable —bloqueado por el daemon, o con la Task
        // cancelada— seguía ofreciendo «a aprobar» encima de un pie que ya
        // decía «este plan no se puede aprobar». Es el mismo desacuerdo que
        // la revisión MAJOR-1 arregló entre el pie y la tecla; ahora hay UNA
        // respuesta y la comparten los dos frontends.
        let id = norte_frontend::sync::hint_id(view);
        let lineas = match &view.confirming {
            Some(c) => vec![
                Line::from(Span::styled(c.text.clone(), theme.role(Role::Warning))),
                Line::from(Span::styled(t(id), theme.role(Role::Warning))),
            ],
            None => vec![Line::from(Span::styled(t(id), theme.role(Role::Info)))],
        };
        // ENVUELTA, y el hueco lo reserva `sync_layout` con la misma cuenta:
        // la frase creció al decir que un árbol se re-comprueba en el
        // directorio y no por dentro, y sin envolver se cortaba justo antes
        // del «¿Seguir?» — la pregunta desaparecía de la pantalla que la
        // hace. Lo cazó el snapshot, otra vez.
        frame.render_widget(
            Paragraph::new(lineas).wrap(ratatui::widgets::Wrap { trim: false }),
            a,
        );
    }
}

/// Cuántas filas ocupa `texto` envuelto a `ancho` columnas.
///
/// Cuenta CELDAS, no bytes ni `char`s: medir en bytes reservaría de más y en
/// `char`s de menos — y de menos es lo que corta la frase que dice que esto no
/// se puede deshacer.
fn wrapped_rows(texto: &str, ancho: u16) -> u16 {
    if ancho == 0 {
        return 1;
    }
    let celdas = u16::try_from(texto.width()).unwrap_or(u16::MAX);
    let exactas = celdas.div_ceil(ancho).max(1);
    // Una fila de holgura en cuanto la frase envuelve: `Wrap` parte por
    // PALABRAS, así que `ceil(celdas / ancho)` es una cota INFERIOR y quedarse
    // en ella recorta la última línea — que es la que dice que esto no se puede
    // deshacer. El tope de `sync_layout` acota lo que la holgura puede costar.
    if celdas > ancho {
        exactas.saturating_add(1)
    } else {
        exactas
    }
}

/// Reparte el interior del marco del panel de sincronización: resumen, lista y
/// teclas.
///
/// El resumen se lleva lo que sus líneas pidan, hasta un tercio del alto: son
/// las frases que deciden la aprobación, y recortarlas a una sola línea es
/// esconder justamente el «esto no se puede deshacer». Con el marco tan corto
/// que no cabe nada, la LISTA se lo queda todo.
fn sync_layout(outer: Rect, view: &crate::app::SyncView) -> (Option<Rect>, Rect, Option<Rect>) {
    if outer.height < 5 {
        return (None, outer, None);
    }
    // Las líneas se ENVUELVEN, así que el alto no es su número: a 80 columnas
    // «el destino no tiene papelera: …» son dos filas, y reservar una la
    // cortaba por la mitad. El snapshot es lo que lo destapó.
    let alto_resumen: u16 = view
        .state
        .plan()
        .map(|p| p.summary_lines(norte_i18n::active()))
        .unwrap_or_default()
        .iter()
        .map(|l| wrapped_rows(l, outer.width))
        .sum();
    // Hasta la MITAD del marco: son las frases que deciden la aprobación, y
    // recortarlas para que quepan más pasos esconde justamente el «esto no se
    // puede deshacer». Los pasos tienen barra; el resumen no.
    let resumen = alto_resumen.min((outer.height / 2).max(1));
    // La segunda pregunta se lleva la pregunta ENVUELTA más la fila de la
    // tecla que la contesta. Dos fijas no bastan: a 80 columnas la frase de un
    // borrado irreversible son dos filas ella sola, y la de más abajo es la
    // que dice «¿Seguir?». Acotada como el resumen —la mitad del marco—, y con
    // el suelo en 2 para que la tecla no se quede nunca sin sitio.
    let teclas = view.confirming.as_ref().map_or(1, |c| {
        wrapped_rows(&c.text, outer.width)
            .saturating_add(1)
            .min((outer.height / 2).max(2))
    });
    let filas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(resumen),
            Constraint::Min(1),
            Constraint::Length(teclas),
        ])
        .split(outer);
    ((resumen > 0).then(|| filas[0]), filas[1], Some(filas[2]))
}

/// El resumen del plan: lo que [`norte_frontend::sync::SyncPlan::summary_lines`]
/// dijo, envuelto.
fn sync_summary(view: &crate::app::SyncView, theme: &TuiTheme) -> Paragraph<'static> {
    let lineas: Vec<Line<'static>> = view
        .state
        .plan()
        .map(|p| p.summary_lines(norte_i18n::active()))
        .unwrap_or_default()
        .into_iter()
        .map(|l| Line::from(Span::styled(l, theme.role(Role::Regular))))
        .collect();
    // Envuelto y NO recortado a lo ancho: la primera línea es lo que el
    // deshacer devuelve y la segunda de qué papelera se habla. Cortarlas deja
    // al lector aprobando con media frase.
    Paragraph::new(lineas).wrap(ratatui::widgets::Wrap { trim: false })
}

/// Una fila del panel: las tres marcas, la ruta y el tamaño.
///
/// Las tres marcas son de columnas DISTINTAS y se separan, porque el alfabeto
/// no es único entre ellas a propósito (`!` es `Certain` en una e
/// `Irreversible` en otra): juntas se leerían como una palabra.
fn sync_step_item(
    step: &norte_proto::methods::SyncStep,
    view: &crate::app::SyncView,
    ancho: usize,
    theme: &TuiTheme,
) -> ListItem<'static> {
    // Las DOS reinterpretaciones, de una pieza: `render_step` lee cada ruta
    // con la del lado del que cuelga (#152). Aquí se le pasaba solo la del
    // ORIGEN y se recomponía `dest_rel` a mano — lo que dejaba el `rel` de un
    // `DeleteTree`, que es una ruta del DESTINO, leído con el codepage del
    // árbol que no se toca.
    let cells = norte_frontend::sync::render_step(step, view.dest_trash(), view.encodings());
    let dest_rel = cells.dest_rel.clone();
    // Las marcas, el ancla y el tamaño; lo que sobra es para la ruta.
    // Ídem: tres copias de este match en esta rama, y la CLI sin ninguna.
    let ancla =
        norte_frontend::sync::anchor_label(cells.anchor, norte_i18n::active()).unwrap_or_default();
    let tam = cells
        .size
        .map(norte_frontend::human_bytes)
        .unwrap_or_default();
    let marcas = format!(
        "{} {} {} ",
        cells.glyphs.kind, cells.glyphs.confidence, cells.glyphs.undo
    );
    // Por CELDAS y no por `char`s: un ancla o un tamaño con caracteres anchos
    // presupuestaría de menos y la fila desbordaría el marco (#79).
    let ruta_w = ancho
        .saturating_sub(marcas.width() + ancla.width() + tam.width() + 2)
        .max(1);
    // La ortografía del DESTINO cuando la hay (#152): la escritura cae sobre
    // ELLA, así que enseñar solo la del origen sería nombrar un fichero que no
    // es el que se va a tocar. Quién decide que «la hay» es `render_step`, por
    // BYTES y una sola vez para los tres frontends: aquí se comparaba el texto
    // PINTADO, que es lossy, así que dos ficheros distintos con un byte
    // inválido cada uno plegaban a uno solo y el campo desaparecía de la
    // pantalla (auditoría de encoding MAJOR-1).
    //
    // El badge va POR MITAD y no solo en la del origen: una ruta de origen
    // limpia con una ortografía de destino hostil —el caso normal cuando solo
    // el pane destino lleva override, porque reinterpretar siempre marca— se
    // pintaba sin marca ninguna (auditoría de encoding MAJOR-3). El CLI ya lo
    // hacía por mitades y la GUI también; esta era la única de las tres que no.
    // #185, y aquí pesa más que en el panel de diferencias: las dos
    // ortografías van UNIDAS por un `→` en la misma cadena, y `→` es un
    // imprimible corriente que `display_name_with` no enmascara — o sea que un
    // fichero llamado `a → b.txt` (corpus `arrow_join_spoof`) llega SIN badge
    // y finge la pareja. La GUI lo cierra con un separador estructural (cada
    // ortografía en su elemento); una `Line` de ratatui no tiene esa
    // posibilidad, así que la decisión de diseño es la misma que #185 lista
    // para el título del panel de diferencias.
    //
    // Y los badges y el `→` van en SPANS PROPIOS, fuera de lo que se trunca
    // (auditoría de encoding de la revisión de rama, MAJOR-4). Construirlos
    // dentro de una sola cadena y pasarla por `middle_ellipsis` los ponía en
    // el MEDIO, que es exactamente lo que esa función tira: a pane estrecho,
    // `⚠ caf<FFFD>.txt → ⚠ caf<FFFD>2.txt` quedaba `⚠ caf…2.txt` y se leía
    // como UN nombre truncado. El `…` dice «se cortó algo», no «la pareja se
    // colapsó», y el campo que desaparecía es justo el que nombra el fichero
    // sobre el que cae la escritura. Se trunca el TEXTO de cada mitad, nunca
    // su marca ni el separador.
    let estilo_ruta = theme.entry(&cells.rel.raw, norte_proto::EntryKind::File);
    let badge_de = |d: &norte_frontend::sync::RelDisplay| {
        if d.hostile { HOSTILE_BADGE } else { "" }
    };
    let mut spans = vec![Span::styled(marcas, sync_undo_style(theme, cells.undo))];
    if let Some(d) = &dest_rel {
        const SEP: &str = " → ";
        let fijo = badge_de(&cells.rel).width() + SEP.width() + badge_de(d).width();
        let texto_w = ruta_w.saturating_sub(fijo).max(2);
        // Se reparte a la mitad: las dos ortografías valen lo mismo, y la
        // del destino es la que dice dónde cae la escritura.
        let mitad = (texto_w / 2).max(1);
        spans.push(Span::styled(
            badge_de(&cells.rel),
            theme.role(Role::Warning),
        ));
        spans.push(Span::styled(
            norte_frontend::middle_ellipsis(&cells.rel.text, mitad),
            estilo_ruta,
        ));
        // El separador con su propio rol: un `→` DENTRO de un nombre
        // (corpus `arrow_join_spoof`) es texto de fichero y se pinta como
        // tal, así que el de la pareja se distingue por estilo aunque los
        // dos glifos sean el mismo. Es lo más que da una `Line` de
        // ratatui; el separador estructural de verdad es lo que #185
        // lista para esta misma clase de fila.
        spans.push(Span::styled(SEP, theme.role(Role::Info)));
        spans.push(Span::styled(badge_de(d), theme.role(Role::Warning)));
        spans.push(Span::styled(
            norte_frontend::middle_ellipsis(&d.text, texto_w - mitad),
            estilo_ruta,
        ));
    } else {
        let fijo = badge_de(&cells.rel).width();
        spans.push(Span::styled(
            badge_de(&cells.rel),
            theme.role(Role::Warning),
        ));
        spans.push(Span::styled(
            norte_frontend::middle_ellipsis(&cells.rel.text, ruta_w.saturating_sub(fijo).max(1)),
            estilo_ruta,
        ));
    }
    if !ancla.is_empty() {
        spans.push(Span::styled(format!(" {ancla}"), theme.role(Role::Info)));
    }
    if !tam.is_empty() {
        spans.push(Span::styled(format!(" {tam}"), theme.role(Role::Info)));
    }
    ListItem::new(Line::from(spans))
}

#[cfg(test)]
mod sync_step_item_tests {
    use super::{HOSTILE_BADGE, TuiTheme, sync_step_item};
    use ratatui::widgets::ListItem;

    /// El texto de CADA span, sin renderizar a buffer: aquí importa la
    /// estructura de spans (qué es marca, qué es separador y qué es nombre),
    /// que es justo lo que un buffer plano borra.
    fn spans(item: &ListItem<'_>) -> Vec<String> {
        // `ListItem` no expone sus líneas; se reconstruye el mismo item.
        // Se compara sobre el render, que es lo que el lector ve.
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::widgets::{List, Widget as _};
        let area = Rect::new(0, 0, 28, 1);
        let mut buf = Buffer::empty(area);
        List::new(vec![item.clone()]).render(area, &mut buf);
        vec![
            (0..area.width)
                .map(|x| buf[(x, 0)].symbol().to_string())
                .collect::<String>(),
        ]
    }

    fn vista() -> crate::app::SyncView {
        crate::app::SyncView::new(
            norte_proto::TaskId::new(1),
            norte_proto::methods::SyncMode::Update,
            norte_proto::VPath::parse("file:///origen").expect("vpath"),
            norte_proto::VPath::parse("file:///destino").expect("vpath"),
            None,
            None,
        )
    }

    /// Un paso cuyas DOS ortografías son hostiles y largas.
    fn paso_hostil() -> norte_proto::methods::SyncStep {
        // Bytes inválidos: `render_step` los decodifica lossy y marca las dos
        // mitades como hostiles, que es el caso normal cuando el pane destino
        // lleva un override #57 y el origen no.
        let seg = |b: &[u8]| {
            norte_proto::methods::RelPath::new(vec![
                norte_proto::Segment::new(b.to_vec()).expect("segmento"),
            ])
        };
        let rel = seg(b"caf\xff_origen_largo.txt");
        let dest = seg(b"caf\xfe_destino_largo.txt");
        norte_proto::methods::SyncStep {
            id: 1,
            kind: norte_proto::methods::SyncStepKind::Overwrite,
            rel,
            dest_rel: Some(dest),
            size: None,
            criterion: norte_proto::methods::CompareCriterion::Size,
            confidence: norte_proto::methods::CompareConfidence::Certain,
            reversal: Some(norte_proto::methods::StepReversal::Delete),
            reason: None,
        }
    }

    /// Auditoría de encoding de la revisión de rama, MAJOR-4. El badge y el
    /// `→` estaban DENTRO de la cadena que se trunca, y `middle_ellipsis` tira
    /// el medio: a pane estrecho la fila quedaba `⚠ caf…largo.txt`, o sea un
    /// nombre truncado. Desaparecían el separador de la pareja y la marca de
    /// la ortografía del DESTINO — la que dice sobre qué fichero cae la
    /// escritura— sin que nada dijera que la pareja se había colapsado.
    #[test]
    fn a_pane_estrecho_sobreviven_los_dos_badges_y_la_flecha() {
        let theme = TuiTheme::default();
        let v = vista();
        let item = sync_step_item(&paso_hostil(), &v, 28, &theme);
        let pintado = spans(&item).join("");
        assert!(
            pintado.contains('\u{2192}'),
            "el separador de la pareja sobrevive al truncado: {pintado:?}"
        );
        // El badge PEGADO a cada mitad, y no el recuento a secas: el glifo de
        // confianza de la columna de marcas es el mismo carácter, así que
        // contarlo suelto cuenta tres y no dice nada de dónde están.
        assert_eq!(
            pintado.matches(&format!("{HOSTILE_BADGE}caf")).count(),
            2,
            "las DOS mitades siguen marcadas, cada una en su sitio: {pintado:?}"
        );
    }
}

/// El color de las marcas de un paso. El GLIFO ya lo distingue sin color
/// ninguno (§17); esto solo lo refuerza para quien sí lo ve.
fn sync_undo_style(theme: &TuiTheme, undo: norte_frontend::sync::StepUndo) -> Style {
    use norte_frontend::sync::StepUndo as U;
    match undo {
        U::Reverts | U::Nothing => theme.role(Role::Regular),
        U::LeftBehind => theme.role(Role::Warning),
        U::Irreversible | U::Unclear => theme.role(Role::Error),
    }
}

/// El pie del panel de sincronización: en qué punto está el diálogo.
///
/// La frase entera la compone [`norte_frontend::sync::status_line`],
/// COMPARTIDA con la GUI desde #161 — aquí estaban sus ocho brazos, y uno de
/// ellos es dónde «este plan se puede aprobar» llega a un humano como palabras.
/// Aquí solo quedan los espacios: pegado al `└` se lee como parte del marco,
/// igual que el pie del panel de diferencias.
fn sync_status_line(view: &crate::app::SyncView) -> String {
    format!(
        " {} ",
        norte_frontend::sync::status_line(view, norte_i18n::active())
    )
}

#[allow(clippy::too_many_arguments)] // wiring del render, no API
fn draw_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    pane: &Pane,
    focused: bool,
    theme: &TuiTheme,
    now_ms: i64,
    settings: &norte_frontend::columns::ColumnsSettings,
    catalog: Option<&norte_proto::AttrCatalog>,
) {
    let border_style = if focused {
        theme.role(Role::BorderFocus)
    } else {
        theme.role(Role::BorderUnfocused)
    };
    let (title, title_hostil) = norte_frontend::path_display_with(pane.dir(), pane.name_encoding());
    let mut title = if title_hostil {
        format!("{HOSTILE_BADGE} {title}")
    } else {
        title
    };
    // Un listado RELLENÁNDOSE (paginación, ADR 0017) se marca SIEMPRE: un
    // listado incompleto jamás es silencioso.
    if pane.loading() {
        use std::fmt::Write as _;
        let _ = write!(
            title,
            " [{}]",
            norte_i18n::ta("pane-loading", &[("n", &pane.entries().len().to_string())])
        );
    }
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title_style(theme.role(Role::Title))
        .title(title);
    // Quick search activo (spec 2026-07-18): línea de input al pie del pane
    // `/{query} n/m` (+ «parcial» si el fill sigue: filtra sobre lo YA
    // drenado, jamás en silencio). La query pasa por el MISMO mask que los
    // nombres (review MINOR-1 T4): «la tecleó el usuario» se rompe con un
    // PASTE — sin bracketed paste llega como stream de Chars y un nombre
    // hostil pegado pintaría bidi/invisibles crudos en el borde.
    if let Some(q) = pane.quick() {
        let (query, _) = display_name(q.query_display().as_bytes());
        let mut input = format!(" /{} {}/{}", query, q.visible().len(), pane.entries().len());
        if pane.loading() {
            input.push(' ');
            input.push_str(&t("quicksearch-partial"));
        }
        input.push(' ');
        block = block.title_bottom(Line::styled(input, theme.role(Role::Title)));
    }
    // Filtro activo: SOLO los índices visibles, con el cursor visual en la
    // posición DENTRO del filtrado. En Jump (quick_visible = None) el
    // listado va entero y manda el cursor real.
    let reinterpret = pane.name_encoding();
    // #108 L5: anchos de columna del ancho INTERIOR del pane, una vez por
    // frame — las filas y la cabecera comparten el mismo layout (con el
    // estilo 7b resuelto por columna, ver `styled_columns`).
    let inner_w = block.inner(area).width;
    let cols = &styled_columns(settings, pane.dir().scheme(), inner_w, catalog);
    // La selección PINTADA sale de la misma función que la usa el hit test
    // del ratón ([`painted_len_and_selection`]): el scroll de abajo se
    // deriva de ella, y dos cálculos distintos harían que un click cayera
    // en la fila de al lado.
    let (painted_len, selected) = painted_len_and_selection(pane);
    let items: Vec<ListItem<'_>> = match pane.quick_visible() {
        Some(vis) => vis
            .iter()
            .filter_map(|&i| pane.entries().get(i))
            .map(|e| {
                entry_item(
                    e,
                    theme,
                    reinterpret,
                    pane.decoration_for(&e.path),
                    pane.is_marked(e),
                    cols,
                    Some(pane),
                    now_ms,
                )
            })
            .collect(),
        None => pane
            .entries()
            .iter()
            .map(|e| {
                entry_item(
                    e,
                    theme,
                    reinterpret,
                    pane.decoration_for(&e.path),
                    pane.is_marked(e),
                    cols,
                    Some(pane),
                    now_ms,
                )
            })
            .collect(),
    };
    // #108 L5: bloque a mano — dentro, UNA línea de cabecera de columnas
    // (dim, con el indicador ▲/▼ del orden activo) y el listado debajo.
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let (header_area, list_area) = {
        let mut cab = inner;
        cab.height = 1;
        let mut lst = inner;
        lst.y = inner.y.saturating_add(1);
        lst.height = inner.height.saturating_sub(1);
        (cab, lst)
    };
    frame.render_widget(
        Paragraph::new(column_header_line(cols, pane.sort(), catalog))
            .style(ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::DIM)),
        header_area,
    );
    let list = List::new(items).highlight_style(theme.role(Role::Selection));
    let mut state = ListState::default();
    state.select(selected);
    // Scroll EXPLÍCITO y no deducido por ratatui: la ventana es del MODELO
    // (`PaneState::reconcile_viewport`, pegajosa) y el hit test del ratón lee
    // esa misma, así que las dos salen del mismo sitio — deducirla dos veces
    // es como un click acaba en la fila de al lado.
    //
    // El clamp contra `painted_len` sigue haciendo falta: el `filter_map` de
    // arriba puede descartar un índice imposible del filtro, y una ventana
    // más allá del final pintaría el listado vacío.
    let _ = painted_len;
    *state.offset_mut() = pane.viewport_offset().min(painted_len.saturating_sub(1));
    frame.render_stateful_widget(list, list_area, &mut state);
}

#[allow(clippy::too_many_arguments)] // fila de render: cada arg es una fuente de pintado, no API
fn entry_item<'a>(
    entry: &'a norte_proto::Entry,
    theme: &TuiTheme,
    reinterpret: Option<norte_encoding::NameEncoding>,
    decoration: Option<&norte_frontend::Decoration>,
    marked: bool,
    cols: &[(
        norte_frontend::columns::ColumnId,
        u16,
        norte_frontend::columns::ColumnStyle,
    )],
    // #117-follow-up: fuente de las celdas `plugin:` (side-map del pane —
    // sus valores no viven en la `Entry`). `None` solo en tests de formato
    // sin columnas de plugin.
    plugin_cells: Option<&Pane>,
    now_ms: i64,
) -> ListItem<'a> {
    let name = entry.path.file_name().map_or(&[][..], |n| n.as_bytes());
    // #57: con reinterpretación activa, los nombres no-UTF8 se decodifican
    // con el encoding elegido (display-only; el badge hostil se conserva —
    // el texto pintado difiere de los bytes reales).
    let (texto, hostil) = norte_frontend::display_name_with(name, reinterpret);
    let kind_glyph = match entry.kind {
        EntryKind::Dir => "/",
        EntryKind::Symlink => "@",
        EntryKind::File | EntryKind::Other => " ",
    };
    let badge = Span::styled(
        if hostil { HOSTILE_BADGE } else { " " },
        theme.role(Role::HostileBadge),
    );
    // Color por tipo/extensión de la entrada (ADR 0020 D2).
    let body = Span::styled(
        format!("{kind_glyph}{texto}"),
        theme.entry(name, entry.kind),
    );
    // Canalón de marca (#103): señal TEXTUAL, jamás solo color — el fallback
    // monocromo de `Role::Mark` es `dim`, que por sí solo se lee «inactivo»,
    // no «seleccionado». Va ANTES del badge hostil para que ni el badge ni la
    // decoración cambien de columna respecto a como se pintaban.
    //
    // El ESTILO también debe ser condicional, no solo el glyph (review
    // BLOCKER): cada preset embarcado define `mark` como SOLO un `bg` (ver
    // `crates/norte-theme/presets/*.toml`), así que un `Span::styled`
    // incondicional pintaba esa franja de color en la columna 1 de CADA fila
    // sin marcar — una franja permanente, no una señal de marca.
    let gutter = if marked {
        Span::styled("*", theme.role(Role::Mark))
    } else {
        Span::raw(" ")
    };
    let mut spans = vec![gutter, badge, body];
    // G3b (ADR 0037): badge de decorator, TRAS el hueco del badge hostil —
    // ya SANEADO y acotado (`norte_frontend::sanitize_decoration`, aplicado
    // antes de llegar aquí). Sin decoración para esta entrada, ningún span
    // extra (ni siquiera un hueco): la fila se ve EXACTAMENTE igual que
    // antes de G3b para quien no usa decoradores.
    if let Some(badge_text) = decoration.and_then(|d| d.badge.as_deref()) {
        let style = match decoration.and_then(|d| d.role) {
            Some(role) => theme.role(role),
            // Sin rol reconocido: dim por defecto — visible pero discreto,
            // nunca el color "normal" de la entrada (se confundiría con el
            // nombre) ni un color inventado por este frontend (ADR 0037: el
            // tema del usuario manda, jamás un color crudo que no pidió).
            None => ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::DIM),
        };
        spans.push(Span::raw(" "));
        spans.push(Span::styled(badge_text.to_string(), style));
    }
    // #108 L5: celdas de columnas tras el nombre. El bloque del nombre
    // (canalón+badge+glyph+texto+decoración) se TRUNCA a su ancho de layout
    // (elipsis central, consciente de celdas — CJK/emoji no desbordan) y
    // se rellena; cada celda no-nombre va alineada según su estilo (#108
    // 7b, derecha por defecto) en su ancho, dim, con un espacio separador.
    // Ausencia = celda en blanco, jamás un 0 fabricado.
    if let Some((_, name_w, _)) = cols.first() {
        let name_w = usize::from(*name_w);
        // review #108-5 M2: la DECORACIÓN también entra en el presupuesto
        // del nombre — un badge CJK (8 chars = 16 celdas) desplazaba todas
        // las celdas de la fila. Si no cabe dejando ≥3 celdas de nombre,
        // fuera la decoración entera (separador incluido): el nombre manda.
        if spans.len() > 3 {
            let deco: usize = spans[3..].iter().map(|sp| sp.content.width()).sum();
            let fijos: usize = spans[..2].iter().map(|sp| sp.content.width()).sum();
            if fijos + deco + 3 > name_w {
                spans.truncate(3);
            }
        }
        let usado: usize = spans.iter().map(|sp| sp.content.width()).sum();
        if usado > name_w {
            // Recorta el TEXTO del nombre (el span del body, índice 2) con
            // elipsis central a lo que quede tras los demás spans — los
            // fijos (canalón/badge) y la decoración se quedan.
            let otros: usize = spans
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != 2)
                .map(|(_, sp)| sp.content.width())
                .sum();
            let body_w = name_w.saturating_sub(otros);
            let recortado = middle_ellipsis(&spans[2].content, body_w);
            spans[2] = Span::styled(recortado, spans[2].style);
        }
        let usado: usize = spans.iter().map(|sp| sp.content.width()).sum();
        debug_assert!(
            usado <= name_w,
            "el bloque del nombre desborda su columna: {usado} > {name_w}"
        );
        if usado < name_w {
            spans.push(Span::raw(" ".repeat(name_w - usado)));
        }
        for (col, w, style) in cols.iter().skip(1) {
            // #117-follow-up: las celdas `plugin:` salen del side-map del
            // pane (re-enmascaradas allí); el resto, de la Entry como
            // siempre. Ausencia = blanco en ambos caminos.
            let cell = match col {
                norte_frontend::columns::ColumnId::Plugin { .. } => plugin_cells
                    .and_then(|p| p.plugin_cell(&col.to_string(), &entry.path))
                    .unwrap_or_default(),
                _ => norte_frontend::columns::styled_cell(entry, col, now_ms, style)
                    .unwrap_or_default(),
            };
            // El ancho INCLUYE el separador (default_layout_items): el
            // contenido vive dentro de w-1 y siempre queda ≥1 espacio de
            // separador. Derecha (default): relleno delante. Izquierda
            // (#108 7b): el separador sigue ABRIENDO el presupuesto, el
            // contenido va tras él y el relleno cae a la derecha — la
            // misma cuenta, invertida.
            let w = usize::from(*w);
            let contenido = w.saturating_sub(1);
            let cw = cell.width();
            let recortada: String = if cw > contenido {
                take_width(&cell, contenido)
            } else {
                cell
            };
            let texto = match style.align {
                norte_frontend::columns::Align::Right => {
                    let pad = w.saturating_sub(recortada.width());
                    format!("{}{recortada}", " ".repeat(pad))
                }
                norte_frontend::columns::Align::Left => {
                    let pad = w.saturating_sub(recortada.width().saturating_add(1));
                    format!(" {recortada}{}", " ".repeat(pad))
                }
            };
            spans.push(Span::styled(
                texto,
                ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::DIM),
            ));
        }
    }
    ListItem::new(Line::from(spans))
}

#[cfg(test)]
mod entry_item_tests {
    use super::{HOSTILE_BADGE, entry_item};
    use crate::theme::TuiTheme;
    use norte_proto::{Entry, EntryKind, VPath};
    use ratatui::widgets::ListItem;

    fn e(wire: &str, k: EntryKind) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse(wire).unwrap(),
            kind: k,
            size: None,
            mtime_ms: None,
        }
    }

    /// Nombre no-UTF8 (bytes crudos vía `Segment`): dispara el badge hostil
    /// sin pasar por reinterpretación.
    fn e_hostile() -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse("mem:///")
                .unwrap()
                .join(norte_proto::Segment::new(b"\xFF\xFE".to_vec()).unwrap()),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
        }
    }

    /// `ListItem`'s span content is private to ratatui, so — like the
    /// crate's other render tests (`tests/theme_render.rs`,
    /// `tests/render.rs`) — this renders the row into a real `Buffer` and
    /// reads it back cell by cell. The gutter and the hostile badge are each
    /// exactly one cell wide by construction, so `span_texts()[0]` and `[1]`
    /// are the true first two spans' text; later cells belong to the
    /// (possibly multi-char) name span and are not meant to be compared
    /// one-for-one with spans.
    fn span_texts(item: &ListItem<'_>) -> Vec<String> {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::widgets::{List, Widget as _};
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        List::new(vec![item.clone()]).render(area, &mut buf);
        (0..area.width)
            .map(|x| buf[(x, 0)].symbol().to_string())
            .collect()
    }

    fn first_span_text(item: &ListItem<'_>) -> String {
        span_texts(item).into_iter().next().unwrap_or_default()
    }

    /// A marked row carries a TEXTUAL cue, never colour alone: `Role::Mark`'s
    /// monochrome fallback is `dim`, which on its own reads as "inactive"
    /// rather than "selected" (#103).
    #[test]
    fn a_marked_row_starts_with_the_mark_gutter() {
        let entry = e("mem:///a", EntryKind::File);
        let theme = TuiTheme::default();
        let marked = entry_item(&entry, &theme, None, None, true, &[], None, 0);
        let plain = entry_item(&entry, &theme, None, None, false, &[], None, 0);
        assert_eq!(first_span_text(&marked), "*");
        assert_eq!(first_span_text(&plain), " ");
    }

    /// The gutter goes BEFORE the hostile badge, so the badge column and the
    /// decorator badge keep the positions they have today.
    #[test]
    fn the_gutter_precedes_the_hostile_badge() {
        let entry = e_hostile();
        let theme = TuiTheme::default();
        let item = entry_item(&entry, &theme, None, None, true, &[], None, 0);
        let texts = span_texts(&item);
        assert_eq!(texts[0], "*");
        assert_eq!(texts[1], HOSTILE_BADGE);
    }
}

/// Segmentos `(marked, pruned)` de la status bar sobre las marcas (#103).
/// Extraído de `draw_status` (que ya rozaba `too_many_lines`) — pura
/// composición de texto, sin efecto de render.
fn marks_status_segments(pane: &Pane) -> (String, String) {
    // Un refresh que se comió marcas JAMÁS es silencioso: con la selección
    // vacía, `marked_paths` cae al cursor, así que callarlo redirigiría la
    // siguiente op en masa a algo que nadie marcó.
    let pruned = if pane.pruned_marks() == 0 {
        String::new()
    } else {
        format!(
            "  {}",
            ta(
                "status-marks-pruned",
                &[("n", &pane.pruned_marks().to_string())]
            )
        )
    };
    // Cuántas marcas y cuánto pesan. Se calla con 0 marcas — la barra no
    // gana ruido para quien no marca nada.
    let marked = if pane.marks_len() == 0 {
        String::new()
    } else {
        let n = pane.marks_len().to_string();
        let size = norte_frontend::human_bytes(pane.marked_bytes());
        let dirs = pane.marked_dirs();
        if dirs == 0 {
            format!("  {}", ta("status-marked", &[("n", &n), ("size", &size)]))
        } else {
            format!(
                "  {}",
                ta(
                    "status-marked-with-dirs",
                    &[("n", &n), ("size", &size), ("dirs", &dirs.to_string())],
                )
            )
        }
    };
    (marked, pruned)
}

fn draw_status(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let pane = app.focused();
    let total = pane.entries().len();
    let pos = if total == 0 { 0 } else { pane.cursor() + 1 };
    // Con el FILTRO activo la selección no es el cursor real: un `pos/total`
    // sería engañoso (review MINOR-2 T4) — se suprime; el pie del pane ya
    // da el contador honesto `n/m`.
    let pos_total = if pane.quick_visible().is_some() {
        String::new()
    } else {
        format!("  {pos}/{total}")
    };
    let (marked, pruned) = marks_status_segments(pane);
    let (dir_texto, dir_hostil) =
        norte_frontend::path_display_with(pane.dir(), pane.name_encoding());
    let marca = if dir_hostil { HOSTILE_BADGE } else { "" };
    // Sin chuleta de teclas: mentiría según el preset. La secuencia pendiente
    // SÍ se pinta (ADR 0006), y desde K3a el panel which-key
    // ([`draw_which_key`]) pinta encima de esta barra lo que puede SEGUIR a
    // esa secuencia. Este segmento no desaparece con él: es la única línea que
    // sobrevive a un panel recortado en un terminal bajo.
    let seq = if app.pending.is_empty() {
        String::new()
    } else {
        format!("  [{} …]", app.pending)
    };
    // Un mensaje pendiente (error por categoría, resultado) desplaza al
    // resto de la barra hasta la siguiente tecla (issue #20). Sin mensaje: un
    // pane de búsqueda viva (liveSearch T6) pinta `search-status-*` (los hits
    // = `entries.len()`); si no, el hook Lua de statusbar (M4, ya saneado por
    // el host) sustituye la línea default del pane con foco.
    // Un arrastre EN VUELO manda sobre todo lo demás mientras dure. Es lo
    // único de esta línea que anuncia una MUTACIÓN a punto de proponerse, y
    // el gesto pide la decisión (copiar o mover) ANTES de que el botón suba:
    // sin este renglón el usuario suelta a ciegas. Dura lo que dura el botón
    // pulsado y no consume nada — el mensaje que tape sigue ahí al soltar.
    let text = if let Some(drag) = crate::mouse::drop_hint(app) {
        format!(" {drag}")
    } else if let Some(msg) = &app.message {
        format!(" {msg}")
    } else if pane.virtual_search {
        use crate::app::SearchState;
        // `Failed` es PERSISTENTE (review MINOR-2): tras limpiarse
        // `app.message`, el pane sigue pintando `search-status-failed` con la
        // categoría del error (guardada en `search_error`) — un fallo jamás
        // degrada a «done» en la siguiente tecla.
        if pane.search_state == SearchState::Failed {
            format!(
                " {}{seq}",
                ta(
                    "search-status-failed",
                    &[("error", pane.search_error.as_deref().unwrap_or(""))],
                )
            )
        } else {
            let key = match pane.search_state {
                SearchState::Running => "search-status-running",
                SearchState::Truncated => "search-status-truncated",
                SearchState::Cancelled => "search-status-cancelled",
                // `Failed` ya se trató arriba; `Done` es el resto.
                SearchState::Done | SearchState::Failed => "search-status-done",
            };
            // #81: contexto del match de contenido del hit BAJO EL CURSOR
            // (línea + preview — saneado en origen por el core; se pasa por
            // detail_for_bar como cinturón, mismo criterio que los errores).
            let hit = pane
                .entries()
                .get(pane.cursor())
                .and_then(|e| pane.search_matches.get(&e.path))
                .map_or_else(String::new, |m| {
                    let linea = m.line.map_or_else(String::new, |l| format!(":{l}"));
                    let preview = m.preview.as_deref().map_or_else(String::new, |p| {
                        format!(" {}", crate::app::detail_for_bar(p))
                    });
                    format!("  [{linea}{preview}]")
                });
            format!(
                " {}{hit}{seq}",
                ta(key, &[("n", &pane.entries().len().to_string())])
            )
        }
    } else if let Some(lua) = &app.lua_status {
        format!(" {lua}{seq}")
    } else if let Some(warn) = app.persistent_banner() {
        // #44: sesión remota degradada a texto plano, y #177: sesión que muta
        // sin quedar registrada en el journal. PERSISTENTES (como
        // `search-status-failed`): sobreviven a las teclas — sin `message`, sin
        // búsqueda viva y sin hook Lua siguen avisando en cada frame.
        // H3d: la frase se COMPONE aquí desde el valor estructurado (una
        // conexión: la nombra; varias: cuántas), en vez de guardarse ya escrita.
        format!(" {warn}{seq}")
    } else {
        // #93: el contenedor omitió entradas de su índice — el listado que
        // se ve NO es todo lo que el archivo contiene. Persistente mientras
        // el pane esté dentro (paralelo del badge hostil, jamás silencioso).
        let omitidas = match pane.skipped() {
            Some(n) if n > 0 => {
                format!(
                    "  {}",
                    ta("status-archive-skipped", &[("n", &n.to_string())])
                )
            }
            _ => String::new(),
        };
        // #57: modo de reinterpretación activo — PERSISTENTE mientras dure
        // (los nombres pintados no son los bytes; el usuario debe saberlo
        // en todo momento, no solo en el mensaje del toggle).
        let nombres = match pane.name_encoding() {
            Some(enc) => format!("  {}", ta("status-names-encoding", &[("enc", enc.label())])),
            None => String::new(),
        };
        // #107: ocultación activa con entradas apartadas — misma disciplina
        // que `omitidas`: un listado que enseña menos de lo que hay jamás
        // es silencioso. Se calla con 0 apartadas (dir sin dotfiles) y con
        // la ocultación apagada. Va DETRÁS de `pruned` en la línea (#107
        // review MINOR-3): ocultar con marcas produce ambos, y el aviso de
        // poda es el que no puede recortarse primero.
        let ocultas = match pane.hidden_count() {
            0 => String::new(),
            n => format!("  {}", ta("status-hidden", &[("n", &n.to_string())])),
        };
        // Review MAJOR M3: los AVISOS (`omitidas` — listado incompleto,
        // "jamás silencioso" — y `nombres` — el badge de reinterpretación,
        // "el usuario debe saberlo en todo momento") van ANTES que el
        // contador informativo de marcas. La línea no tiene presupuesto de
        // ancho y ratatui recorta la cola: con `marked`/`pruned` primero (25+
        // celdas fácil) un path largo a 80 columnas empujaba el badge de
        // encoding fuera del recorte. Deuda real (#103): un presupuesto de
        // ancho que elipsise `dir_texto` para que NINGÚN campo posterior se
        // recorte jamás, en vez de solo reordenar por prioridad.
        format!(" {marca}{dir_texto}{pos_total}{omitidas}{nombres}{pruned}{ocultas}{marked}{seq}")
    };
    frame.render_widget(
        Paragraph::new(text).style(app.theme.role(Role::StatusBar)),
        area,
    );
}

#[cfg(test)]
mod help_footer_tests {
    use super::{cells, fit_hint_groups, hint_groups};
    use crate::app::ALLOW_HELP;
    use crate::hints::{dialog_hints, without_navigation};
    use crate::keymap::{COMMANDS, DIALOG_COMMANDS, Effective, Screen, presets};

    /// The `dialog` effective of the shipped default preset — the very one
    /// the help overlay's footer is generated from at runtime.
    fn dialog_eff() -> Effective {
        let (_, preset) = presets()
            .into_iter()
            .find(|(n, _)| *n == "orthodox")
            .expect("preset orthodox");
        let known: Vec<&str> = COMMANDS
            .iter()
            .copied()
            .chain(DIALOG_COMMANDS.iter().copied())
            .collect();
        Effective::build_for(&preset, &[], &known, Screen::Dialog).expect("efectivo del preset")
    }

    /// FIX 1: the footer must never paint a `[` that does not open a WHOLE
    /// `[chord] label` group.
    ///
    /// `middle_ellipsis` cut inside a group and left the brackets balanced —
    /// `[esc]…kspace]` at 80 columns, a chord for a key the app invented. A
    /// group is data (the effective keymap × the Fluent label); half of one
    /// is a fabrication. Swept over EVERY budget from one cell to the full
    /// hint, so no width has a special case hiding in it.
    #[test]
    fn el_pie_de_la_ayuda_jamas_parte_un_grupo() {
        let eff = dialog_eff();
        let hint = dialog_hints(&without_navigation(ALLOW_HELP), &eff);
        let grupos = hint_groups(&hint);
        assert!(
            grupos.len() > 2,
            "el hint de la ayuda tiene varios grupos: {hint:?}"
        );
        for max in 1..=cells(&hint) {
            let out = fit_hint_groups(&hint, max);
            assert!(
                cells(&out) <= max,
                "max={max}: {} celdas en {out:?}",
                cells(&out)
            );
            // Lo que queda tras quitar la marca de recorte tiene que ser una
            // secuencia de grupos ENTEROS del hint real.
            let cuerpo = out
                .strip_suffix('…')
                .map_or(out.as_str(), str::trim_end)
                .to_owned();
            for (i, _) in cuerpo.match_indices('[') {
                assert!(
                    grupos.iter().any(|g| cuerpo[i..].starts_with(g)),
                    "max={max}: un `[` que no abre un grupo entero: {out:?}"
                );
            }
            if cuerpo != hint {
                assert!(
                    out.ends_with('…'),
                    "max={max}: se descartó algo sin marcarlo: {out:?}"
                );
            }
        }
    }

    /// Y lo que se descarta se descarta por la COLA: el pie es un prefijo
    /// real del hint, nunca un trozo del medio (que es donde caían los
    /// verbos nuevos de H3b — `[tab] otro panel` desaparecía entero).
    #[test]
    fn el_pie_de_la_ayuda_es_un_prefijo_del_hint() {
        let eff = dialog_eff();
        let hint = dialog_hints(&without_navigation(ALLOW_HELP), &eff);
        for max in 1..=cells(&hint) {
            let out = fit_hint_groups(&hint, max);
            let cuerpo = out.strip_suffix('…').map_or(out.as_str(), str::trim_end);
            assert!(
                hint.starts_with(cuerpo),
                "max={max}: {cuerpo:?} no es prefijo de {hint:?}"
            );
        }
    }
}

#[cfg(test)]
mod ellipsis_tests {
    use super::middle_ellipsis;
    use unicode_width::UnicodeWidthStr;

    /// Una cadena que ya cabe en `max` celdas vuelve intacta.
    #[test]
    fn cabe_intacta() {
        assert_eq!(middle_ellipsis("file:///d/a.txt", 46), "file:///d/a.txt");
    }

    /// ASCII que desborda: comportamiento idéntico al anterior (celdas==chars),
    /// cabeza + `…` + cola, sin exceder `max`.
    #[test]
    fn ascii_conserva_cabeza_y_cola() {
        let s = "file:///muy/larga/ruta/hacia/un/archivo/final.txt";
        let out = middle_ellipsis(s, 20);
        assert!(out.contains('…'));
        assert!(out.starts_with("file:"), "conserva el scheme (cabeza)");
        let cola = out.rsplit_once('…').expect("hay elipsis").1;
        assert!(
            !cola.is_empty() && s.ends_with(cola),
            "la cola es un sufijo REAL del original: {out:?}"
        );
        assert!(out.width() <= 20, "no excede el ancho: {out:?}");
    }

    /// CJK (cada char = 2 celdas): NUNCA excede `max` celdas y CONSERVA la
    /// cola —el bug #79 la perdía porque presupuestaba por chars—.
    #[test]
    fn cjk_no_excede_y_conserva_cola() {
        let s = "日本語".repeat(20); // 60 chars, 120 celdas
        let out = middle_ellipsis(&s, 21);
        assert!(out.width() <= 21, "ancho {} > 21 en {out:?}", out.width());
        assert!(out.contains('…'));
        assert!(out.ends_with('語'), "la cola sobrevive: {out:?}");
        assert!(out.starts_with('日'), "la cabeza sobrevive: {out:?}");
    }

    /// Emoji ancho (2 celdas): tampoco desborda.
    #[test]
    fn emoji_no_excede() {
        let s = "a😀b😀c😀d😀e😀f😀g";
        let out = middle_ellipsis(s, 9);
        assert!(out.width() <= 9, "ancho {} en {out:?}", out.width());
        assert!(out.contains('…'));
    }

    /// `max` menor que un solo char ancho: no se parte la celda → solo `…`.
    #[test]
    fn max_menor_que_un_char_ancho() {
        let out = middle_ellipsis("日本", 1);
        assert_eq!(out, "…");
        assert!(out.width() <= 1);
    }

    /// P1 encoding audit F2 (LOW): un flood de combining marks (ancho CERO
    /// cada uno) desborda el caminante por celdas SIN nunca tocar su
    /// presupuesto — el early-return de ancho, o el propio caminante,
    /// podían devolver/procesar el string ENTERO sin acotar, con `max`
    /// celdas satisfecho pero el tamaño real sin tope. `nfd_e_acute` del
    /// corpus (`e` + combining acute) es el par base+combining canónico —
    /// aquí se inunda a 100 000× para ejercer el backstop por CUENTA de
    /// chars, no solo por ancho.
    #[test]
    fn flood_de_combining_marks_no_desborda() {
        let fixture = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "nfd_e_acute")
            .expect("fixture del corpus");
        let texto = String::from_utf8(fixture.bytes).expect("nfd_e_acute es UTF-8 válido");
        let (base, combining) = texto.split_at(1); // "e" + "\u{0301}"
        let flood: String = std::iter::once(base)
            .chain(std::iter::repeat_n(combining, 100_000))
            .collect();
        assert_eq!(flood.width(), 1, "control: el flood entero pesa 1 celda");
        let out = middle_ellipsis(&flood, 10);
        // Cota: el backstop pre-recorta a `char_cap = 4*max` chars, pero el
        // caminante de cabeza Y el de cola operan cada uno sobre TODO ese
        // precorte (no sobre mitades separadas) — con ancho cero ninguno
        // frena por presupuesto, así que cada uno puede consumirlo entero.
        // Bounded (2*char_cap + 1), no perfecto — lo que pide F2 (LOW) es
        // dejar de ser ILIMITADO, no una cota ajustada.
        let cota = 2 * (10 * 4) + 1;
        assert!(
            out.chars().count() <= cota,
            "el backstop de cuenta de chars no acotó la salida: {} chars (cota {cota})",
            out.chars().count()
        );
    }
}

#[cfg(test)]
mod ai_rename_plan_modal_tests {
    use super::{HOSTILE_BADGE, ai_rename_plan_modal_text, display_name, modal_height};
    use norte_proto::methods::{
        AiRenameEntry, FsRenameBatchPlanResult, PlanHash, RenameCollision, RenameCollisionKind,
        RenameStep,
    };
    use norte_proto::{Segment, VPath};

    fn dir() -> VPath {
        VPath::parse("mem:///proyecto").expect("wire válido")
    }

    fn entry(from: &str, to: &str) -> AiRenameEntry {
        AiRenameEntry {
            from: from.into(),
            to: to.into(),
        }
    }

    fn seg(b: &[u8]) -> Segment {
        Segment::new(b.to_vec()).expect("segmento")
    }

    fn hash() -> PlanHash {
        PlanHash::parse(&"0".repeat(64)).expect("64 hex")
    }

    /// El caso NORMAL: el core contestó que el lote se puede ejecutar.
    fn plan_ok() -> norte_frontend::BatchPlan {
        listo(FsRenameBatchPlanResult {
            steps: vec![RenameStep {
                from: seg(b"a"),
                to: seg(b"b"),
                temp: false,
            }],
            collisions: vec![],
            executable: true,
            plan_hash: hash(),
        })
    }

    /// Envuelve un plan del core en el estado «ya contestó».
    fn listo(p: FsRenameBatchPlanResult) -> norte_frontend::BatchPlan {
        norte_frontend::BatchPlan::Ready(Box::new(p))
    }

    /// Un lote PARADO por un veredicto (`steps` vacío: el invariante del
    /// proto — un plan no ejecutable jamás viene ordenado a medias).
    fn plan_con_colision(kind: RenameCollisionKind, name: &[u8]) -> norte_frontend::BatchPlan {
        listo(FsRenameBatchPlanResult {
            steps: vec![],
            collisions: vec![RenameCollision {
                pair_index: 0,
                name: seg(name),
                kind,
            }],
            executable: false,
            plan_hash: hash(),
        })
    }

    /// H3c: con una ayuda abierta ENCIMA, las teclas del modal no responden,
    /// así que su pie no puede seguir ofreciéndolas.
    ///
    /// Este modal y el de hits semánticos son los dos únicos cuya pista es
    /// PROSA de Fluent en vez de un hint generado, y por eso necesitan esta
    /// rama: los generados ya los sustituye `DialogHints::with_modals_inert`.
    /// NO son los dos únicos que una ayuda puede tapar — eso lo decide
    /// `help_context::help_over_modal_allowed`, e incluye la aprobación de
    /// agente y el TOFU de host key. Sin esta rama, un lector con la ayuda
    /// delante veía «y/Enter: aplicar» y ninguna de las dos hacía nada: un pie
    /// que miente, que es exactamente lo que el diseño de `hints.rs` existe
    /// para no tener.
    #[test]
    fn el_pie_del_plan_no_ofrece_teclas_inertes_bajo_la_ayuda() {
        use norte_i18n::t;
        let vivas = crate::hints::DialogHints::default();
        let (_, normal) =
            ai_rename_plan_modal_text(&dir(), &[entry("a", "b")], 0, &vivas, &plan_ok());
        assert!(
            normal.contains(&t("modal-ai-rename-plan-hint")),
            "sin ayuda encima, el pie ofrece sus teclas: {normal}"
        );

        let inertes = vivas.with_modals_inert();
        let (_, tapado) =
            ai_rename_plan_modal_text(&dir(), &[entry("a", "b")], 0, &inertes, &plan_ok());
        assert!(
            !tapado.contains(&t("modal-ai-rename-plan-hint")),
            "con la ayuda encima NO puede ofrecer y/n: {tapado}"
        );
        assert!(
            tapado.contains(&t("modal-hint-help-open")),
            "y tiene que decir por qué: {tapado}"
        );
    }

    /// Audit MINOR-6a (corpus canónico, molde del sweep de `app.rs`): cada
    /// nombre hostil, en la posición `from` Y en la `to` — ningún char de
    /// `is_terminal_hazard` sobrevive en el texto pintado, y cuando el
    /// enmascarado altera el nombre la línea va MARCADA con el badge.
    #[test]
    fn barrido_corpus_ningun_hazard_sobrevive_y_el_enmascarado_marca() {
        for n in norte_testkit::corpus::hostile_names() {
            let name = String::from_utf8_lossy(&n.bytes).into_owned();
            let casos = [
                (name.clone(), "limpio.txt".to_owned()),
                ("limpio.txt".to_owned(), name.clone()),
            ];
            for (from, to) in casos {
                let hostil = display_name(from.as_bytes()).1 || display_name(to.as_bytes()).1;
                let (_, body) = ai_rename_plan_modal_text(
                    &dir(),
                    &[entry(&from, &to)],
                    0,
                    &crate::hints::DialogHints::default(),
                    &plan_ok(),
                );
                // Por LÍNEA: el `\n` que separa las líneas del cuerpo es un
                // control legítimo del formato, no contenido pintado.
                assert!(
                    !body
                        .lines()
                        .any(|l| l.chars().any(norte_encoding::is_terminal_hazard)),
                    "corpus {}: un hazard sobrevivió al render: {body:?}",
                    n.id
                );
                if hostil {
                    assert!(
                        body.contains(HOSTILE_BADGE),
                        "corpus {}: enmascarado SIN badge: {body:?}",
                        n.id
                    );
                }
            }
        }
    }

    /// Audit MINOR-4 (corpus `arrow_join_spoof`): un `from` que IMITA la
    /// flecha no fabrica una pareja falsa — el `from` lleva su etiqueta
    /// numerada fuera de banda en SU línea y el destino REAL conserva la
    /// suya con la flecha al inicio.
    #[test]
    fn arrow_join_spoof_no_fabrica_pareja() {
        let spoof = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "arrow_join_spoof")
            .expect("fixture del corpus");
        let from = String::from_utf8_lossy(&spoof.bytes).into_owned();
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &[entry(&from, "real.txt")],
            0,
            &crate::hints::DialogHints::default(),
            &plan_ok(),
        );
        let lines: Vec<&str> = body.lines().collect();
        // dir + estado + from + to + hint = 5 líneas exactas: el spoof no
        // añade una.
        assert_eq!(lines.len(), 5, "{body:?}");
        assert!(lines[2].contains("1."), "etiqueta fuera de banda: {body:?}");
        assert!(
            lines[3].starts_with('→') && lines[3].contains("real.txt"),
            "el destino real conserva SU línea: {body:?}"
        );
    }

    /// Audit MINOR-6c: un destino hostil (RLO del corpus) se enmascara y su
    /// línea va marcada — el badge antecede incluso a la flecha.
    #[test]
    fn destino_hostil_enmascara_y_marca() {
        let rtl = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "rtl_override")
            .expect("fixture del corpus");
        let to = String::from_utf8_lossy(&rtl.bytes).into_owned();
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &[entry("limpio.txt", &to)],
            0,
            &crate::hints::DialogHints::default(),
            &plan_ok(),
        );
        let to_line = body.lines().nth(3).expect("línea del destino");
        assert!(to_line.starts_with(HOSTILE_BADGE), "{body:?}");
        assert!(to_line.contains('\u{FFFD}'), "{body:?}");
        assert!(
            !to_line.chars().any(norte_encoding::is_terminal_hazard),
            "{body:?}"
        );
    }

    /// Audit MAJOR-3: con 7 parejas la ventana pinta 5 desde `offset` con
    /// numeración ABSOLUTA, el indicador dice posición/total y el alto del
    /// modal cuadra con las líneas pintadas.
    #[test]
    fn plan_largo_ventana_indicador_y_alto() {
        let entries: Vec<AiRenameEntry> = (1..=7)
            .map(|i| entry(&format!("f{i}"), &format!("t{i}")))
            .collect();
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &entries,
            0,
            &crate::hints::DialogHints::default(),
            &plan_ok(),
        );
        let lines: Vec<&str> = body.lines().collect();
        // dir + estado del lote + 5 parejas × 2 + indicador + hint = 14.
        assert_eq!(lines.len(), 14, "{body:?}");
        assert!(
            lines[2].contains("1.") && lines[2].contains("f1"),
            "{body:?}"
        );
        assert!(lines[12].contains("5/7"), "indicador: {body:?}");
        assert!(!body.contains("f6"), "la cola espera al scroll: {body:?}");
        // offset 2 = parejas 3..=7, numeración absoluta, indicador al tope.
        let (_, body2) = ai_rename_plan_modal_text(
            &dir(),
            &entries,
            2,
            &crate::hints::DialogHints::default(),
            &plan_ok(),
        );
        let lines2: Vec<&str> = body2.lines().collect();
        assert_eq!(lines2.len(), 14, "alto ESTABLE al scroll: {body2:?}");
        assert!(
            lines2[2].contains("3.") && lines2[2].contains("f3"),
            "{body2:?}"
        );
        assert!(body2.contains("f7"), "{body2:?}");
        assert!(lines2[12].contains("7/7"), "{body2:?}");
        // Un offset desbocado se clampa en el render (cinturón).
        let (_, body3) = ai_rename_plan_modal_text(
            &dir(),
            &entries,
            999,
            &crate::hints::DialogHints::default(),
            &plan_ok(),
        );
        assert!(body3.contains("f7"), "{body3:?}");
        // Alto: 14 líneas de cuerpo + 3 de marco.
        let modal = crate::app::Modal::AiRenamePlan {
            dir: dir(),
            entries,
            offset: 0,
            plan: plan_ok(),
        };
        assert_eq!(modal_height(&modal), 17);
    }

    /// Audit MAJOR-3: el indicador de desbordamiento delata una pareja
    /// hostil OCULTA (lo no visible jamás se cuela "limpio"), y deja de
    /// marcar cuando el scroll la pone a la vista.
    #[test]
    fn indicador_marca_hostil_oculto() {
        let mut entries: Vec<AiRenameEntry> = (1..=6)
            .map(|i| entry(&format!("f{i}"), &format!("t{i}")))
            .collect();
        entries[5] = entry("x\u{202e}y", "limpio.txt");
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &entries,
            0,
            &crate::hints::DialogHints::default(),
            &plan_ok(),
        );
        let ind = body.lines().nth(12).expect("indicador");
        assert!(ind.starts_with(HOSTILE_BADGE), "{body:?}");
        // offset 1: la hostil entra en la ventana; la oculta (pareja 1) es
        // limpia — el indicador ya no marca.
        let (_, body2) = ai_rename_plan_modal_text(
            &dir(),
            &entries,
            1,
            &crate::hints::DialogHints::default(),
            &plan_ok(),
        );
        let ind2 = body2.lines().nth(12).expect("indicador");
        assert!(!ind2.starts_with(HOSTILE_BADGE), "{body2:?}");
    }

    /// §17: una colisión es VISIBLE con su veredicto y su índice de pareja,
    /// y el modal dice que el plan NO se puede aplicar — un humano no puede
    /// confirmar un lote que va a rebotar sin saber por qué.
    #[test]
    fn una_colision_se_pinta_y_el_plan_se_marca_inaplicable() {
        use norte_i18n::t;
        let plan = plan_con_colision(RenameCollisionKind::External, b"z.txt");
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &[entry("a.txt", "z.txt")],
            0,
            &crate::hints::DialogHints::default(),
            &plan,
        );
        let lines: Vec<&str> = body.lines().collect();
        // dir + estado + pareja × 2 + colisión + hint = 6.
        assert_eq!(lines.len(), 6, "{body:?}");
        assert_eq!(lines[1], t("modal-rename-batch-not-applicable"), "{body:?}");
        assert!(
            lines[4].contains(&t("modal-rename-batch-collision-external")),
            "el veredicto se enseña: {body:?}"
        );
        assert!(lines[4].contains("z.txt"), "y el nombre ofensor: {body:?}");
        // `pair_index` 0 se pinta 1-based, como la etiqueta del `from`: la
        // fila culpable es señalable.
        assert!(lines[4].contains("1."), "{body:?}");
        // El pie NO ofrece una tecla muda.
        assert_eq!(
            lines[5],
            t("modal-rename-batch-plan-hint-blocked"),
            "{body:?}"
        );
        assert!(!body.contains(&t("modal-ai-rename-plan-hint")), "{body:?}");
    }

    /// Un veredicto de un daemon MÁS NUEVO degrada UNA línea a una etiqueta
    /// genérica, jamás el modal entero: el resto del plan se sigue leyendo y
    /// el lote sigue marcado como no aplicable.
    #[test]
    fn un_veredicto_desconocido_degrada_una_linea_no_el_modal() {
        use norte_i18n::t;
        let futuro: RenameCollisionKind =
            serde_json::from_str(r#""clase_del_futuro""#).expect("fallback");
        assert_eq!(futuro, RenameCollisionKind::Unknown);
        let plan = plan_con_colision(futuro, b"z.txt");
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &[entry("a.txt", "z.txt")],
            0,
            &crate::hints::DialogHints::default(),
            &plan,
        );
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 6, "el modal sigue entero: {body:?}");
        assert!(lines[2].contains("a.txt"), "las parejas se siguen viendo");
        assert!(
            lines[4].contains(&t("modal-rename-batch-collision-unknown")),
            "{body:?}"
        );
        assert_eq!(lines[1], t("modal-rename-batch-not-applicable"), "{body:?}");
    }

    /// Un paso temporal es MAQUINARIA del planificador: se dice CUÁNTOS hay,
    /// jamás cómo se llaman. Un `.norte-rename-…` entre las parejas haría
    /// creer al humano que norte va a dejar ese nombre en su disco.
    #[test]
    fn un_paso_temporal_se_cuenta_jamas_se_nombra() {
        use norte_i18n::t;
        let plan = listo(FsRenameBatchPlanResult {
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
            collisions: vec![],
            executable: true,
            plan_hash: hash(),
        });
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &[entry("a", "b"), entry("b", "a")],
            0,
            &crate::hints::DialogHints::default(),
            &plan,
        );
        assert!(
            !body.contains(".norte-rename-"),
            "un temporal jamás se pinta como propuesta: {body:?}"
        );
        // Las DOS mitades del ciclo llevan `temp`, y las dos son maquinaria.
        assert!(
            body.contains(&norte_i18n::ta("modal-rename-batch-temp", &[("n", "2")])),
            "{body:?}"
        );
        // Aplicable: el rodeo no es una colisión.
        assert!(
            body.contains(&t("modal-rename-batch-applicable")),
            "{body:?}"
        );
        assert!(body.contains(&t("modal-ai-rename-plan-hint")), "{body:?}");
    }

    /// El plan en vuelo (`None`): el modal abre con las parejas y dice que
    /// está comprobando — sin ofrecer una tecla de confirmar que está muda.
    #[test]
    fn sin_plan_todavia_el_pie_no_ofrece_confirmar() {
        use norte_i18n::t;
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &[entry("a", "b")],
            0,
            &crate::hints::DialogHints::default(),
            &norte_frontend::BatchPlan::Pending,
        );
        assert!(body.contains(&t("modal-rename-batch-pending")), "{body:?}");
        assert!(!body.contains(&t("modal-ai-rename-plan-hint")), "{body:?}");
        // Y el alto cuadra con lo pintado (dir + pareja × 2 + estado + hint).
        let modal = crate::app::Modal::AiRenamePlan {
            dir: dir(),
            entries: vec![entry("a", "b")],
            offset: 0,
            plan: norte_frontend::BatchPlan::Pending,
        };
        assert_eq!(modal_height(&modal), 8);
        assert_eq!(body.lines().count(), 5, "{body:?}");
    }

    /// Barrido del corpus sobre el NOMBRE OFENSOR de una colisión: ningún
    /// hazard sobrevive, el enmascarado MARCA la línea, y el veredicto —que
    /// es lo accionable— jamás se lo come el nombre.
    #[test]
    fn barrido_corpus_en_el_nombre_de_la_colision() {
        use norte_i18n::t;
        let verdicto = t("modal-rename-batch-collision-internal");
        for n in norte_testkit::corpus::hostile_names() {
            let plan = plan_con_colision(RenameCollisionKind::Internal, &n.bytes);
            let (_, body) = ai_rename_plan_modal_text(
                &dir(),
                &[entry("a", "b")],
                0,
                &crate::hints::DialogHints::default(),
                &plan,
            );
            assert!(
                !body
                    .lines()
                    .any(|l| l.chars().any(norte_encoding::is_terminal_hazard)),
                "corpus {}: un hazard sobrevivió al render: {body:?}",
                n.id
            );
            // UNA línea por colisión: un nombre no puede fabricar otra.
            assert_eq!(body.lines().count(), 6, "corpus {}: {body:?}", n.id);
            let linea = body.lines().nth(4).expect("línea de la colisión");
            assert!(
                linea.contains(&verdicto),
                "corpus {}: el veredicto sobrevive al nombre: {body:?}",
                n.id
            );
            if display_name(&n.bytes).1 {
                assert!(
                    linea.starts_with(HOSTILE_BADGE),
                    "corpus {}: enmascarado SIN badge: {body:?}",
                    n.id
                );
            }
        }
    }

    /// Un lote con MUCHAS colisiones no desborda el modal: se pintan hasta
    /// [`norte_frontend::RENAME_COLLISION_LIMIT`] y el resumen dice cuántas
    /// quedan fuera — y marca si alguna OCULTA es hostil (lo escondido no se
    /// cuela limpio).
    #[test]
    fn muchas_colisiones_se_resumen_y_lo_oculto_hostil_se_marca() {
        let mut collisions: Vec<RenameCollision> = (0..8)
            .map(|i| RenameCollision {
                pair_index: i,
                name: seg(format!("f{i}").as_bytes()),
                kind: RenameCollisionKind::Internal,
            })
            .collect();
        collisions[7].name = seg("x\u{202e}y".as_bytes());
        let total = collisions.len();
        let plan = listo(FsRenameBatchPlanResult {
            steps: vec![],
            collisions,
            executable: false,
            plan_hash: hash(),
        });
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &[entry("a", "b")],
            0,
            &crate::hints::DialogHints::default(),
            &plan,
        );
        let lines: Vec<&str> = body.lines().collect();
        // dir + estado + pareja × 2 + 5 colisiones + resumen + hint = 11.
        assert_eq!(lines.len(), 11, "{body:?}");
        let resumen = lines[9];
        assert!(
            resumen.contains(&norte_frontend::RENAME_COLLISION_LIMIT.to_string())
                && resumen.contains(&total.to_string()),
            "el resumen no calla cuántas quedan fuera: {body:?}"
        );
        assert!(
            resumen.starts_with(HOSTILE_BADGE),
            "una colisión OCULTA hostil marca el resumen: {body:?}"
        );
        assert!(!body.contains("f7"), "la cola queda resumida: {body:?}");
    }
}

#[cfg(test)]
mod semantic_hits_modal_tests {
    use super::{HOSTILE_BADGE, modal_height, semantic_hits_modal_text};
    use norte_proto::methods::SemanticHit;
    use norte_proto::{Segment, VPath};

    fn hit(path: VPath, score: f64) -> SemanticHit {
        SemanticHit { path, score }
    }

    fn hits(n: u16) -> Vec<SemanticHit> {
        (1..=n)
            .map(|i| {
                hit(
                    VPath::parse(&format!("mem:///d/f{i}")).expect("wire válido"),
                    1.0 - f64::from(i) / 100.0,
                )
            })
            .collect()
    }

    /// M4-IA-2 (corpus canónico, molde del sweep del plan IA): cada nombre
    /// hostil como último segmento del path de un hit — ningún char de
    /// `is_terminal_hazard` sobrevive en el texto pintado, y cuando el
    /// enmascarado altera el path la línea va MARCADA con el badge.
    #[test]
    fn barrido_corpus_ningun_hazard_sobrevive_y_el_enmascarado_marca() {
        for n in norte_testkit::corpus::hostile_names() {
            let path = VPath::parse("mem:///d")
                .expect("wire válido")
                .join(Segment::new(n.bytes.clone()).expect("segmento del corpus"));
            let hostil = norte_frontend::path_display(&path).1;
            let (_, body) = semantic_hits_modal_text(
                &[hit(path, 0.5)],
                0,
                0,
                &crate::hints::DialogHints::default(),
            );
            // Por LÍNEA: el `\n` que separa las líneas del cuerpo es un
            // control legítimo del formato, no contenido pintado.
            assert!(
                !body
                    .lines()
                    .any(|l| l.chars().any(norte_encoding::is_terminal_hazard)),
                "corpus {}: un hazard sobrevivió al render: {body:?}",
                n.id
            );
            if hostil {
                assert!(
                    body.contains(HOSTILE_BADGE),
                    "corpus {}: enmascarado SIN badge: {body:?}",
                    n.id
                );
            }
        }
    }

    /// Encoding audit M4-IA-2 S1 (fixture `score_spoof_inband`): un nombre
    /// que IMITA la columna de score (`informe · 0.99.txt`: middle dot +
    /// decimales, todo imprimible — NO hay badge que avise) jamás desplaza
    /// al score REAL. Se pinea en dos formas: el fixture tal cual (cabe
    /// entero, el score genuino queda el ÚLTIMO campo) y el fixture inflado
    /// a >120 chars (fuerza la elipsis media: el path se RECORTA, marcado,
    /// pero el score sigue ahí — jamás al revés).
    #[test]
    fn score_spoof_inband_jamas_desplaza_al_score_real() {
        let fixture = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "score_spoof_inband")
            .expect("fixture del corpus");
        let dir = VPath::parse("mem:///d").expect("wire válido");
        let señuelo = String::from_utf8(fixture.bytes.clone()).expect("el fixture es UTF-8");

        let path = dir
            .clone()
            .join(Segment::new(fixture.bytes.clone()).expect("segmento del corpus"));
        let (_, body) = semantic_hits_modal_text(
            &[hit(path, 0.91)],
            0,
            0,
            &crate::hints::DialogHints::default(),
        );
        let linea = body.lines().next().expect("la línea del hit");
        assert!(
            linea.contains(&señuelo),
            "el señuelo se pinta tal cual (es un nombre legítimo): {linea:?}"
        );
        assert!(
            linea.trim_end().ends_with("0.91"),
            "el score REAL es el campo FINAL: {linea:?}"
        );

        // Inflado: el señuelo al final de un nombre kilométrico. El recorte
        // se come el PATH (elipsis media, marcada), nunca el score.
        let mut largo = b"x".repeat(120);
        largo.extend_from_slice(&fixture.bytes);
        let path = dir.join(Segment::new(largo).expect("segmento válido"));
        let (_, body) = semantic_hits_modal_text(
            &[hit(path, 0.91)],
            0,
            0,
            &crate::hints::DialogHints::default(),
        );
        let linea = body.lines().next().expect("la línea del hit");
        assert!(
            linea.trim_end().ends_with("0.91"),
            "path kilométrico: el score REAL sigue siendo el campo FINAL: {linea:?}"
        );
        assert!(
            linea.contains('…'),
            "el recorte del path se MARCA (spec §6): {linea:?}"
        );
    }

    /// M4-IA-2: con 12 hits la ventana pinta 10 desde `offset` con
    /// numeración ABSOLUTA y marcador `>` en la fila del cursor; el
    /// indicador dice posición/total, el score va al final de la línea y el
    /// alto del modal cuadra con las líneas pintadas.
    #[test]
    fn hits_largos_ventana_cursor_indicador_y_alto() {
        let hits = hits(12);
        let (_, body) =
            semantic_hits_modal_text(&hits, 0, 3, &crate::hints::DialogHints::default());
        let lines: Vec<&str> = body.lines().collect();
        // 10 hits + indicador + hint = 12.
        assert_eq!(lines.len(), 12, "{body:?}");
        assert!(
            lines[0].contains("1.") && lines[0].contains("f1"),
            "{body:?}"
        );
        assert!(
            lines[3].starts_with("> ") && lines[3].contains("4."),
            "marcador en la fila del cursor: {body:?}"
        );
        assert_eq!(
            lines.iter().filter(|l| l.starts_with("> ")).count(),
            1,
            "un solo cursor: {body:?}"
        );
        assert!(lines[0].contains("0.99"), "score al final: {body:?}");
        assert!(lines[10].contains("10/12"), "indicador: {body:?}");
        assert!(!body.contains("f11"), "la cola espera al scroll: {body:?}");
        // La ventana sigue al cursor: offset 2 = hits 3..=12, numeración
        // absoluta, cursor al fondo visible.
        let (_, body2) =
            semantic_hits_modal_text(&hits, 2, 11, &crate::hints::DialogHints::default());
        let lines2: Vec<&str> = body2.lines().collect();
        assert_eq!(lines2.len(), 12, "alto ESTABLE al scroll: {body2:?}");
        assert!(
            lines2[0].contains("3.") && lines2[0].contains("f3"),
            "{body2:?}"
        );
        assert!(
            lines2[9].starts_with("> ") && lines2[9].contains("12."),
            "{body2:?}"
        );
        assert!(lines2[10].contains("12/12"), "{body2:?}");
        // Un offset desbocado se clampa en el render (cinturón).
        let (_, body3) =
            semantic_hits_modal_text(&hits, 999, 0, &crate::hints::DialogHints::default());
        assert!(body3.contains("f12"), "{body3:?}");
        // Alto: 12 líneas de cuerpo + 3 de marco.
        let modal = crate::app::Modal::SemanticHits {
            hits,
            offset: 0,
            cursor: 0,
        };
        assert_eq!(modal_height(&modal), 15);
    }

    /// M4-IA-2: el indicador de desbordamiento delata un hit hostil OCULTO
    /// (lo no visible jamás se cuela "limpio"), y deja de marcar cuando el
    /// scroll lo pone a la vista.
    #[test]
    fn indicador_marca_hostil_oculto() {
        let mut hits = hits(11);
        hits[10] = hit(
            VPath::parse("mem:///d")
                .expect("wire válido")
                .join(Segment::new(b"x\xe2\x80\xaey".to_vec()).expect("segmento")),
            0.1,
        );
        let (_, body) =
            semantic_hits_modal_text(&hits, 0, 0, &crate::hints::DialogHints::default());
        let ind = body.lines().nth(10).expect("indicador");
        assert!(ind.starts_with(HOSTILE_BADGE), "{body:?}");
        // offset 1: el hostil entra en la ventana; el oculto (hit 1) es
        // limpio — el indicador ya no marca.
        let (_, body2) =
            semantic_hits_modal_text(&hits, 1, 10, &crate::hints::DialogHints::default());
        let ind2 = body2.lines().nth(10).expect("indicador");
        assert!(!ind2.starts_with(HOSTILE_BADGE), "{body2:?}");
    }
}

/// El modal de aprobación de agente: la lista de rutas y su ALTO (review H3c
/// MINOR-5).
#[cfg(test)]
mod approval_modal_tests {
    use super::{HOSTILE_BADGE, approval_modal_text, modal_height};

    fn req(paths: Vec<String>) -> norte_proto::methods::PolicyApprovalRequired {
        norte_proto::methods::PolicyApprovalRequired {
            approval_id: 1,
            session: Some("s1".into()),
            op: "copy".into(),
            paths_total: paths.len() as u64,
            paths,
            ttl_ms: 60_000,
        }
    }

    fn rutas(n: usize) -> Vec<String> {
        (1..=n).map(|i| format!("mem:///proj/f{i}.txt")).collect()
    }

    /// El recorte del SERVER también se cuenta (0.36.0). Un lote de renames
    /// gatea miles de rutas y el daemon difunde solo las primeras: si el modal
    /// pintara `paths.len()` como si fuera todo, el humano aprobaría 32 rutas
    /// inocentes sin saber que la decisión cubría ocho mil. Eso no es una
    /// aprobación informada, es una aprobación engañada.
    #[test]
    fn el_recorte_del_server_se_le_dice_al_humano() {
        let mut r = req(rutas(3));
        r.paths_total = 8192;
        let (_, body) = approval_modal_text(&r, "PIE");
        let lines: Vec<&str> = body.lines().collect();
        // cabecera + 3 rutas + resumen + pie.
        assert_eq!(lines.len(), 6, "{body:?}");
        assert!(
            lines[4].contains(&(8192 - 3).to_string()),
            "el resumen cuenta las que la DECISIÓN cubre y no se ven: {body:?}"
        );
        assert_eq!(lines[5], "PIE", "y el pie sigue siendo la última: {body:?}");
        assert_eq!(
            modal_height(&crate::app::Modal::ApproveAgentOp { req: r }),
            8,
            "el alto cuenta la línea de resumen que acaba de aparecer",
        );
    }

    /// `paths_total: 0` es un server N-1 que no lo mandaba: lo recibido ES
    /// todo lo que hubo, y no se inventa un resumen que mentiría al revés.
    #[test]
    fn sin_paths_total_no_se_inventa_recorte() {
        let mut r = req(rutas(2));
        r.paths_total = 0;
        let (_, body) = approval_modal_text(&r, "PIE");
        assert_eq!(
            body.lines().count(),
            4,
            "cabecera + 2 rutas + pie: {body:?}"
        );
    }

    /// Review MINOR-5: el número de rutas lo elige el AGENTE, y el alto no
    /// podía crecer con él sin tope.
    ///
    /// `centered` recorta contra el frame, así que las líneas de sobra
    /// simplemente no se pintaban — incluida la ÚLTIMA, que bajo H3c es la
    /// única explicación de por qué `y`/`n` no hacen nada. Un `paths` de 400
    /// entradas borraba el aviso de la pantalla. Ahora se enventana como
    /// `ConfirmDelete`: `MODAL_ITEM_LIMIT` rutas más una línea de resumen.
    #[test]
    fn la_lista_de_rutas_se_enventana_y_el_pie_siempre_cabe() {
        let limite = norte_frontend::MODAL_ITEM_LIMIT;
        let total = limite + 7;
        let (_, body) = approval_modal_text(&req(rutas(total)), "PIE-DEL-MODAL");
        let lines: Vec<&str> = body.lines().collect();

        // cabecera + LIMITE rutas + resumen + pie.
        assert_eq!(lines.len(), limite + 3, "{body:?}");
        assert!(lines[1].contains("f1.txt"), "{body:?}");
        assert!(
            lines[limite].contains(&format!("f{limite}.txt")),
            "la última ruta de la ventana: {body:?}"
        );
        assert!(
            !body.contains(&format!("f{}.txt", limite + 1)),
            "la cola NO se pinta: {body:?}"
        );
        assert!(
            lines[limite + 1].contains(&(total - limite).to_string()),
            "el resumen dice cuántas quedan fuera: {body:?}"
        );
        assert_eq!(
            lines[limite + 2],
            "PIE-DEL-MODAL",
            "y el pie es la ÚLTIMA línea, siempre presente: {body:?}"
        );

        // El alto lo dice el mismo cómputo acotado: cuerpo + marco, jamás
        // `paths.len()` crudo.
        let modal = crate::app::Modal::ApproveAgentOp {
            req: req(rutas(total)),
        };
        let alto = modal_height(&modal);
        assert_eq!(alto, u16::try_from(limite + 3).expect("cabe") + 2, "{alto}");
        assert_eq!(
            modal_height(&crate::app::Modal::ApproveAgentOp { req: req(rutas(1)) }),
            5,
            "un lote que cabe conserva su alto de siempre: acotar la lista no \
             le mueve la caja"
        );
        assert_eq!(
            alto,
            modal_height(&crate::app::Modal::ApproveAgentOp {
                req: req(rutas(400)),
            }),
            "el agente no elige el alto: 17 rutas y 400 miden lo mismo"
        );
    }

    /// Un lote que CABE se pinta entero y sin línea de resumen: enventanar no
    /// puede inventarse un «y N más» que no existe.
    #[test]
    fn un_lote_que_cabe_no_lleva_resumen() {
        let (_, body) = approval_modal_text(&req(rutas(2)), "PIE");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 4, "{body:?}"); // cabecera + 2 rutas + pie
        assert!(body.contains("f2.txt"), "{body:?}");
        assert_eq!(lines[3], "PIE", "{body:?}");
    }

    /// Y lo ESCONDIDO no se cuela limpio (misma doctrina que el plan IA y los
    /// hits semánticos): si alguna ruta fuera de la ventana es hostil, la línea
    /// de resumen va MARCADA — el humano decide sabiendo que hay algo raro que
    /// no está viendo.
    #[test]
    fn el_resumen_marca_una_ruta_hostil_escondida() {
        let limite = norte_frontend::MODAL_ITEM_LIMIT;
        let mut paths = rutas(limite + 2);
        paths[limite + 1] = "mem:///proj/x\u{202e}y.txt".to_owned();
        let (_, body) = approval_modal_text(&req(paths), "PIE");
        let resumen = body.lines().nth(limite + 1).expect("resumen");
        assert!(
            resumen.starts_with(HOSTILE_BADGE),
            "el resumen delata la hostil oculta: {body:?}"
        );

        // Con TODAS las ocultas limpias, no marca (o el badge no diría nada).
        let (_, limpio) = approval_modal_text(&req(rutas(limite + 2)), "PIE");
        let resumen_limpio = limpio.lines().nth(limite + 1).expect("resumen");
        assert!(!resumen_limpio.starts_with(HOSTILE_BADGE), "{limpio:?}");
    }
}

#[cfg(test)]
mod column_header_line_tests {
    use super::column_header_line;
    use norte_frontend::columns::{Align, Builtin, ColumnId, ColumnStyle};
    use norte_frontend::{SortColumn, SortDir, SortSpec};
    use unicode_width::UnicodeWidthStr;

    fn estilo(b: Builtin, align: Align, header: &str) -> ColumnStyle {
        ColumnStyle {
            align,
            header: Some(header.to_owned()),
            ..ColumnStyle::default_for(b)
        }
    }

    /// m1 revisión 7b: columna IZQUIERDA de una celda con la flecha del
    /// sort activa — la emisión queda clampada a exactamente `w` (antes
    /// «espacio + flecha» eran 2 celdas y corrían toda la cabecera a su
    /// derecha; con 2 celdas la flecha sí cabe tras el separador).
    #[test]
    fn header_izquierda_de_una_celda_con_flecha_no_desborda() {
        let sort = SortSpec {
            column: SortColumn::Size,
            dir: SortDir::Asc,
            dirs_first: true,
        };
        let cols = [
            (
                ColumnId::Builtin(Builtin::Name),
                6,
                estilo(Builtin::Name, Align::Left, "N"),
            ),
            (
                ColumnId::Builtin(Builtin::Size),
                1,
                estilo(Builtin::Size, Align::Left, "S"),
            ),
        ];
        let linea = column_header_line(&cols, sort, None);
        assert_eq!(linea.width(), 7, "exactamente la suma de anchos: {linea:?}");
        assert_eq!(linea, "N      ");
        let cols = [
            (
                ColumnId::Builtin(Builtin::Name),
                6,
                estilo(Builtin::Name, Align::Left, "N"),
            ),
            (
                ColumnId::Builtin(Builtin::Size),
                2,
                estilo(Builtin::Size, Align::Left, "S"),
            ),
        ];
        let linea = column_header_line(&cols, sort, None);
        assert_eq!(linea.width(), 8, "{linea:?}");
        assert_eq!(linea, "N      ▲");
    }
}

#[cfg(test)]
mod plugin_description_line_tests {
    use super::plugin_description_line;
    use crate::theme::TuiTheme;

    fn sample_plugin(description: Option<&str>) -> norte_proto::methods::PluginInfo {
        norte_proto::methods::PluginInfo {
            id: "org.norte.demo".into(),
            name: "Demo".into(),
            publisher: "norte".into(),
            version: "1.0.0".into(),
            category: "previewer".into(),
            capabilities: Vec::new(),
            approved: true,
            enabled: true,
            description: description.map(str::to_owned),
            commands: Vec::new(),
            columns: Vec::new(),
            has_help: false,
        }
    }

    fn line_text(line: &ratatui::text::Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn sin_description_es_none() {
        let p = sample_plugin(None);
        assert!(plugin_description_line(&p, &TuiTheme::default(), 100).is_none());
    }

    /// P1 encoding audit F1 (MEDIUM): un daemon hostil/comprometido puede
    /// mandar una `description` sin tope por el wire — este draw NO confía
    /// en que el caller (`main::dispatch`'s ingest,
    /// `app::clamp_plugin_descriptions`) ya la haya clampado, y la acota
    /// aquí también (self-contained, como `plugin_line`). Con un `inner`
    /// GRANDE (que no fuerce elipsis por ancho) el contenido final refleja
    /// EXACTAMENTE `PLUGIN_DESCRIPTION_WIRE_CAP` caracteres del original —
    /// ni uno más, sin pasar por el layout del popup.
    #[test]
    fn clampa_al_tope_del_wire_incluso_sin_ingest() {
        let p = sample_plugin(Some(&"a".repeat(50_000)));
        let line =
            plugin_description_line(&p, &TuiTheme::default(), 10_000).expect("hay description");
        let texto = line_text(&line);
        assert_eq!(
            texto.chars().filter(|&c| c == 'a').count(),
            crate::app::PLUGIN_DESCRIPTION_WIRE_CAP,
            "el draw procesó más de PLUGIN_DESCRIPTION_WIRE_CAP chars del original: {texto:?}"
        );
    }

    /// Un override RTL crudo (sin pasar por ingest) se enmascara a U+FFFD
    /// AQUÍ — nunca llega intacto a `ratatui` (donde un control/override es
    /// invisible: desaparecería en silencio en vez de marcarse).
    #[test]
    fn enmascara_override_rtl_incluso_sin_ingest() {
        let p = sample_plugin(Some("abc\u{202E}gpj.exe"));
        let line =
            plugin_description_line(&p, &TuiTheme::default(), 10_000).expect("hay description");
        let texto = line_text(&line);
        assert!(!texto.contains('\u{202E}'));
        assert!(texto.contains('\u{FFFD}'));
    }
}

#[cfg(test)]
mod which_key_render_tests {
    use super::draw_which_key;
    use crate::theme::TuiTheme;
    use norte_frontend::keymap::{Effective, Screen, parse_chord, parse_keymap};
    use norte_frontend::whichkey::WhichKeyRows;
    use norte_i18n::Lang;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Modifier;

    fn panel(count: Option<u32>) -> WhichKeyRows {
        let src = r#"
counts = true

[pane]
keymap = [
    { on = ["g", "g"], run = "cursor.top" },
    { on = ["g", "p"], run = "pane.pack" },
    { on = ["g", "a", "b"], run = "mark.all" },
]
"#;
        let preset = parse_keymap(src).expect("fixture parses");
        let known = ["cursor.top", "mark.all"];
        let eff =
            Effective::build_for(&preset, &[], &known, Screen::Browse).expect("fixture builds");
        WhichKeyRows::build(&eff, &[parse_chord("g").expect("chord")], count, Lang::En)
    }

    /// The panel paints its title (count included), one row per continuation,
    /// and the unavailable row DIMMED with its reason — the reader is told why
    /// the key does nothing instead of not finding the key at all.
    #[test]
    fn the_panel_paints_every_continuation_and_dims_the_unavailable_one() {
        let theme = TuiTheme::default();
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).expect("terminal de test");
        terminal
            .draw(|f| draw_which_key(f, &panel(Some(12)), &theme))
            .expect("draw");
        let text = terminal.backend().to_string();
        assert!(text.contains("12 g"), "the count in flight: {text}");
        assert!(text.contains("go to top"), "the available row: {text}");
        // No help text exists for a command that is not built: the row falls
        // back to the NAME, never to a raw `help-cmd-…` id.
        assert!(text.contains("pane.pack"), "the unavailable row: {text}");
        assert!(!text.contains("help-cmd-"), "a raw Fluent id: {text}");
        assert!(text.contains("#132"), "the issue that tracks it: {text}");
        assert!(text.contains('…'), "the row that opens more keys: {text}");

        // The `p` row is dimmed; the `g` row is not.
        let buf = terminal.backend().buffer();
        let dim_of = |needle: &str| -> bool {
            for y in 0..buf.area.height {
                let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
                if row.contains(needle) {
                    return (0..buf.area.width)
                        .any(|x| buf[(x, y)].modifier.contains(Modifier::DIM));
                }
            }
            panic!("no row painted {needle}");
        };
        assert!(dim_of("pane.pack"), "an unavailable row is dimmed");
        assert!(!dim_of("go to top"), "an available one is not");
    }

    /// A terminal too short for the rows keeps the status line free, stays
    /// inside the frame and COUNTS what it dropped: a box that just ends
    /// implies the list ended with it.
    #[test]
    fn a_short_terminal_truncates_with_a_count_and_never_overflows() {
        let theme = TuiTheme::default();
        // Five rows: one for the status bar, two borders, and two lines
        // inside — one real key and the count of the two that did not fit.
        let mut terminal = Terminal::new(TestBackend::new(24, 5)).expect("terminal de test");
        terminal
            .draw(|f| draw_which_key(f, &panel(None), &theme))
            .expect("draw");
        let text = terminal.backend().to_string();
        assert!(text.contains("1/3"), "the rows it could not show: {text}");
        // The last line — the status bar's — was not painted over.
        let last = text.lines().last().expect("a last line").to_owned();
        assert!(
            last.chars().all(|c| c.is_whitespace() || c == '"'),
            "the status line was overwritten: {last:?}"
        );

        // Too short for even one real key: the panel does not open at all, and
        // the bar's pending segment is what the reader is left with — a box
        // whose one line says "… 0/3" would spend three rows saying nothing.
        let mut squeezed = Terminal::new(TestBackend::new(24, 4)).expect("terminal de test");
        squeezed
            .draw(|f| draw_which_key(f, &panel(None), &theme))
            .expect("draw");
        let painted = squeezed.backend().to_string();
        assert!(
            painted.chars().all(|c| c.is_whitespace() || c == '"'),
            "nothing is painted: {painted}"
        );

        // Degenerate geometry must not panic or paint outside the frame. The
        // narrow-but-TALL one is the interesting case: it is the only one that
        // reaches the drawing code, with `width` clamped to a box that is all
        // border and no inside.
        for (w, h) in [(4_u16, 1_u16), (1, 3), (2, 2), (1, 10), (3, 12)] {
            let mut tiny = Terminal::new(TestBackend::new(w, h)).expect("terminal de test");
            tiny.draw(|f| draw_which_key(f, &panel(None), &theme))
                .expect("draw");
        }
    }
}

#[cfg(test)]
mod draw_shortcuts_tests {
    use super::{TuiTheme, draw_shortcuts};
    use norte_frontend::keymap::{Effective, Screen, parse_chord, parse_keymap};
    use norte_frontend::shortcuts::{ScreenKeys, ShortcutsState, build_rows};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    // `pane.move` no lo bindea nadie: es la fila SIN TECLA que la hoja de
    // referencia no puede tener.
    const BINDABLE: &[&str] = &["pane.copy", "pane.mkdir", "pane.move"];

    fn eff() -> Effective {
        // Un chord HOSTIL (U+202E RIGHT-TO-LEFT OVERRIDE) bindeado como
        // codepoint suelto: legal, y sin confianza — una capa de proyecto
        // llega con un repositorio clonado.
        let src = "[pane]\nkeymap = [\n  { on = [\"f5\"], run = \"pane.copy\" },\n  { on = [\"alt+f5\"], run = \"pane.pack\" },\n  { on = [\"\u{202e}\"], run = \"pane.mkdir\" },\n]\n";
        let preset = parse_keymap(src).expect("fixture parsea");
        Effective::build_for(&preset, &[], BINDABLE, Screen::Browse).expect("fixture construye")
    }

    fn state(eff: &Effective) -> ShortcutsState {
        ShortcutsState::new(build_rows(
            &[ScreenKeys {
                screen: Screen::Browse,
                eff,
                bindable: BINDABLE,
            }],
            norte_i18n::active(),
        ))
    }

    fn painted(sc: &ShortcutsState) -> String {
        let mut terminal = Terminal::new(TestBackend::new(90, 14)).expect("terminal de test");
        terminal
            .draw(|f| draw_shortcuts(f, sc, &TuiTheme::default()))
            .expect("draw");
        terminal.backend().to_string()
    }

    /// La pantalla DICE las dos cosas que esta terminal no puede hacer: `esc`
    /// cancela (así que es el único chord no capturable) y `mod+` es Ctrl,
    /// porque crossterm no entrega ⌘ sin el protocolo de Kitty. Sin esa línea
    /// el lector descubre ambas cosas pulsando.
    #[test]
    fn la_pantalla_dice_lo_que_esta_terminal_no_puede_capturar() {
        let eff = eff();
        let text = painted(&state(&eff));
        assert!(text.contains("esc"), "{text}");
        assert!(text.contains("mod+"), "{text}");
    }

    /// Un comando sin tecla se ve (la fila que la hoja de referencia no puede
    /// tener), y una tecla que este build no puede ejecutar se ve con su
    /// razón — nada se cae en silencio.
    #[test]
    fn se_ven_la_fila_sin_tecla_y_la_no_construida() {
        let eff = eff();
        let text = painted(&state(&eff));
        assert!(text.contains(&norte_i18n::t("shortcuts-no-key")), "{text}");
        assert!(text.contains("132"), "la razón con su issue: {text}");
    }

    /// El veredicto se pinta ANTES de confirmar, y el chord capturado va
    /// PINTADO: un codepoint hostil no llega crudo a la terminal por la línea
    /// de detalle más de lo que llega por la lista.
    #[test]
    fn el_veredicto_se_pinta_y_los_chords_van_enmascarados() {
        let eff = eff();
        let mut sc = state(&eff);
        assert!(sc.begin_capture());
        sc.capture_chord(parse_chord("\u{202e}").expect("chord"), &eff);
        let text = painted(&sc);
        // Por LÍNEA: los `\n` que une `to_string` son del harness, no del
        // buffer (mismo criterio que el resto de tests de render de aquí).
        assert!(
            text.lines()
                .all(|l| !l.chars().any(norte_encoding::is_terminal_hazard)),
            "{text}"
        );
        // `Replaces`: el codepoint hostil ya está ligado a `pane.mkdir`, y el
        // veredicto que se pinta es EL del modelo, no una frase paralela.
        // Fluent aísla sus argumentos con marcas de dirección (U+2066..U+2069)
        // que el buffer de ratatui, de ancho cero, no llega a pintar: se
        // quitan para comparar, en vez de comparar contra otra cosa.
        let verdict = norte_frontend::shortcuts::verdict_message(
            sc.capture()
                .and_then(norte_frontend::shortcuts::Capture::verdict)
                .expect("hay veredicto"),
            norte_i18n::active(),
        );
        let want: String = verdict
            .chars()
            .filter(|c| !('\u{2066}'..='\u{2069}').contains(c))
            .collect();
        assert!(text.contains(&want), "{want:?} en {text}");
    }

    /// Geometrías degeneradas: ni pánico ni pintar fuera del frame. La caja
    /// tiene una ventana sobre la lista, y una ventana mal calculada es la
    /// forma habitual de salirse por abajo.
    #[test]
    fn geometrias_degeneradas_no_revientan() {
        let eff = eff();
        let mut sc = state(&eff);
        for _ in 0..20 {
            sc.down();
        }
        for (w, h) in [(4_u16, 1_u16), (1, 3), (2, 2), (1, 10), (3, 12), (30, 5)] {
            let mut tiny = Terminal::new(TestBackend::new(w, h)).expect("terminal de test");
            tiny.draw(|f| draw_shortcuts(f, &sc, &TuiTheme::default()))
                .expect("draw");
        }
    }
}
