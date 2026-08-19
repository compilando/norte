//! Render ratatui del estado (`app`): cero lógica de negocio — pinta lo que
//! hay. El marcado de nombres hostiles sigue la spec §6 (lossy y MARCADO). Los
//! colores salen del tema resuelto (`app.theme`, ADR 0020): un frontend sin
//! tema ve el fallback monocromo de M1.

use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{
    Block, Borders, List, ListItem, ListState, Paragraph, };
use unicode_width::UnicodeWidthStr;

use crate::app::{App, display_name};
use crate::theme::TuiTheme;
use norte_i18n::{t, ta};

mod compare;
mod help;
mod modals;
mod overlays;
mod pane;
mod panels;
mod pickers;
mod status;
mod sync;
mod text;

// `tests/` los llama por `norte_tui::ui::..`, asi que siguen siendo API.
pub use help::{help_body_size, help_group_is_painted, help_sidebar_width};
use help::draw_help;
use compare::{compare_layout, draw_compare};
use modals::draw_modal;
use pane::draw_pane;
use overlays::{
    draw_extensions, draw_palette, draw_plugin_config_panel, draw_settings, draw_shortcuts,
    draw_which_key,
};
// `mouse.rs` y `event_loop.rs` los llaman por `ui::..`, y `tests/` tambien.
pub use panels::{PlaceZone, places_zones};
use panels::{
    draw_metadata, draw_places, draw_preview, draw_processes, draw_tasks, draw_tree, draw_viewer,
};
use pickers::{
    draw_columns_picker, draw_connections_picker, draw_layout_picker, draw_theme_picker,
};
use status::draw_status;
use sync::{draw_sync, sync_layout};


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
pub fn pane_list_rows(app: &App, area: Rect) -> u16 {
    // Con el visor abierto no se pinta ningún pane: 0 filas visibles.
    if app.viewer.is_some() {
        return 0;
    }
    // El alto sale del REPARTO, no de restar a mano la franja de tareas y la
    // barra: esas dos son ya huecos del árbol. Lo que queda aquí es el cromo
    // del propio pane, que el árbol no conoce.
    // Bordes del bloque (2) + cabecera de columnas (1). Un pane que este
    // frame no pinta (el `Split` colapsó) no tiene filas de listado.
    pane_rects(app, area)
        .first()
        .map_or(0, |r| r.height)
        .saturating_sub(pane_chrome_rows(app, 0))
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
pub fn before_frame(app: &mut App, area: Rect) {
    let res = resolved_frame(app, area);
    // Quién se ve dónde: con pestañas, el hueco de cada lado cambia.
    let vis = visible_browsers(&res, &app.layout);
    let order: Vec<_> = vis.iter().map(|(id, _)| *id).collect();
    // Las dos juntas, siempre: dos listas de orden que se puedan desincronizar
    // son un fallo que solo se ve al cambiar de pestaña.
    app.panes.set_visible(&order);
    app.history.set_order(&order);
    let cols = pane_cols(&res, &app.layout);
    // El foco no puede quedarse en un pane que este frame no pinta: sería un
    // teclado que mueve un cursor que nadie ve. Con dos lados esto es
    // `position`; cuando haya N huecos lo hará `layout::focus_next`.
    // El foco no puede señalar una posición que este frame no pinta.
    if app.focus() >= cols.len() && !cols.is_empty() {
        app.set_focus(0);
    }
    // Y los roles se ponen al día con lo que hay en pantalla: `active` es el
    // foco, `target` es el otro si sigue visible.
    let focus = app.panes.slot_of(app.focus());
    let (tree, kinds) = (app.layout.clone(), app.kinds.clone());
    app.roles.reconcile(&tree, &res, &kinds, focus);
    // Una ventana POR PANE: el que no se pinta no tiene filas, y reconciliar
    // el suyo contra el alto del otro le dejaría una ventana que nadie vio.
    let visor = app.viewer.is_some();
    for i in 0..app.panes.len() {
        let rows = if visor {
            0
        } else {
            usize::from(
                cols.get(i)
                    .map_or(0, |r| r.height)
                    .saturating_sub(pane_chrome_rows(app, i)),
            )
        };
        app.panes[i].reconcile_viewport(rows);
    }
    // Las OTRAS dos listas largas (#210). El alto sale de replicar aquí el
    // mismo recorte que hace su `draw`, por lo mismo que `pane_geometry`
    // replica el suyo: si el layout cambia, lo que rompe es el test de al
    // lado y no el scroll en silencio.
    let body = overlay_body(app, area);
    if let Some(view) = &mut app.sync {
        let (_, list, _) = sync_layout_rows(body, view);
        if let Some(plan) = view.state.plan_mut() {
            plan.reconcile_viewport(usize::from(list.height));
        }
    } else if let Some(view) = &mut app.compare {
        let (_, list, _, _) = compare_layout(block_inner(body));
        view.pane.reconcile_viewport(usize::from(list.height));
    }
}

/// El reparto de ESTE frame, con los `Auto` ya sustituidos.
///
/// El árbol guardado (`app.layout`) conserva sus `Auto`; el del frame no.
/// Sustituir aquí y no dentro de `resolve` es lo que mantiene al motor puro y
/// sin closures en su firma.
fn resolved_frame(app: &App, area: Rect) -> norte_frontend::layout::Resolved {
    let tree = app.layout.substitute_auto(&|id| natural(app, id));
    norte_frontend::layout::resolve(crate::panel::from_ratatui(area), &tree, &app.kinds)
}

/// El reparto de este frame, para quien no pinta.
///
/// `pub` porque el run loop necesita saber qué huecos se COLOCARON para
/// decidir qué pedir: un preview que no se colocó no lee (regla 2 del spec), y
/// eso solo lo sabe el reparto.
#[must_use]
pub fn resolved_for(app: &App, area: Rect) -> norte_frontend::layout::Resolved {
    resolved_frame(app, area)
}

/// El tamaño que pide un hueco por su CONTENIDO.
///
/// Solo la franja de tareas tiene uno: `min(tareas, 6)` filas, y cero en
/// reposo. Es lo único de la pantalla que el árbol no puede saber solo.
fn natural(app: &App, id: norte_frontend::layout::SlotId) -> (u16, u16) {
    if id == crate::panel::SLOT_TASKS {
        (0, tasks_rows(app))
    } else {
        (0, 0)
    }
}

/// Dónde cayó un hueco en este reparto.
fn slot_rect(
    res: &norte_frontend::layout::Resolved,
    id: norte_frontend::layout::SlotId,
) -> Option<Rect> {
    res.placements
        .iter()
        .find(|(i, _)| *i == id)
        .map(|(_, r)| crate::panel::to_ratatui(*r))
}

/// El primer hueco COLOCADO con ese kind, y dónde cayó.
///
/// Del REPARTO y no del árbol: quien pinta solo puede pintar lo que se colocó,
/// y un hueco detrás de una pestaña o dentro de un `Split` colapsado no se
/// colocó. Ahí es donde la suspensión de un hueco oculto deja de ser una regla
/// escrita y pasa a ser lo único que el código puede hacer.
fn placed_of_kind(
    res: &norte_frontend::layout::Resolved,
    tree: &norte_frontend::layout::Node,
    kind: &str,
) -> Option<(norte_frontend::layout::SlotId, Rect)> {
    res.placements
        .iter()
        .find(|(id, _)| tree.kind_of(*id).is_some_and(|k| k.as_str() == kind))
        .map(|(id, r)| (*id, crate::panel::to_ratatui(*r)))
}

/// El CUERPO: la caja envolvente de los `browser` colocados.
///
/// Con uno solo colocado —el `Split` colapsó— la caja es ese mismo, que es
/// exactamente el sitio que un visor o un panel de diferencias debe ocupar.
fn body_rect(
    res: &norte_frontend::layout::Resolved,
    tree: &norte_frontend::layout::Node,
) -> Option<Rect> {
    let mut bbox: Option<Rect> = None;
    for (_, r) in visible_browsers(res, tree) {
        bbox = Some(match bbox {
            None => r,
            Some(c) => {
                let x = c.x.min(r.x);
                let y = c.y.min(r.y);
                Rect {
                    x,
                    y,
                    width: (c.x + c.width).max(r.x + r.width) - x,
                    height: (c.y + c.height).max(r.y + r.height) - y,
                }
            }
        });
    }
    bbox
}

/// El cuerpo calculado a mano, para cuando el reparto no coloca ningún pane.
///
/// No pasa con el preset `orthodox`; existe porque un layout sin `browser` no
/// puede dejar sin sitio a un visor abierto.
fn chrome_body(app: &App, area: Rect) -> Rect {
    let alto = area
        .height
        .saturating_sub(tasks_rows(app))
        .saturating_sub(1);
    Rect {
        height: alto,
        ..area
    }
}

/// El área que ocupan los panes —o el panel que los sustituye— en `area`.
fn overlay_body(app: &App, area: Rect) -> Rect {
    let res = resolved_frame(app, area);
    body_rect(&res, &app.layout).unwrap_or_else(|| chrome_body(app, area))
}

/// Los `browser` que este reparto SÍ pinta, de izquierda a derecha.
///
/// Con pestañas hay más de dos listados vivos y solo dos visibles, así que
/// «el pane izquierdo» deja de ser un id fijo y pasa a ser una POSICIÓN: el
/// browser colocado más a la izquierda. Ordenar por `(x, y)` es exactamente lo
/// que el usuario ve, y es lo que mantiene el significado de `app.panes[0]`.
fn visible_browsers(
    res: &norte_frontend::layout::Resolved,
    tree: &norte_frontend::layout::Node,
) -> Vec<(norte_frontend::layout::SlotId, Rect)> {
    let mut v: Vec<_> = res
        .placements
        .iter()
        .filter(|(id, _)| {
            tree.kind_of(*id)
                .is_some_and(|k| *k == norte_frontend::layout::KindId::browser())
        })
        .map(|(id, r)| (*id, crate::panel::to_ratatui(*r)))
        .collect();
    v.sort_by_key(|(_, r)| (r.x, r.y));
    v
}

/// Dónde cae cada pane, o `None` si este frame no lo pinta.
///
/// Un `None` no es un error: el `Split` colapsó porque el cuerpo no da para
/// dos veces el mínimo del `browser`, y el otro se pinta a ancho completo.
/// Quien tuviera el foco ahí lo pierde en [`before_frame`].
fn pane_cols(
    res: &norte_frontend::layout::Resolved,
    tree: &norte_frontend::layout::Node,
) -> Vec<Rect> {
    visible_browsers(res, tree)
        .into_iter()
        .map(|(_, r)| r)
        .collect()
}

/// Como [`pane_cols`], resolviendo el frame por su cuenta.
fn pane_rects(app: &App, area: Rect) -> Vec<Rect> {
    pane_cols(&resolved_frame(app, area), &app.layout)
}

/// Las pestañas del pane del lado `side`, si está en un grupo.
///
/// `pub` porque el ratón necesita los mismos títulos para medir las zonas.
///
/// El título de cada una es el nombre del directorio de su hueco, saneado por
/// `display_name`: un directorio con nombre hostil dentro de una pestaña es
/// tan hostil como dentro de un listado (regla 1).
#[must_use]
pub fn tab_strip_for(app: &App, side: usize) -> Option<TabStrip> {
    let slot = app.panes.slot_of(side);
    let (huecos, active) = app.layout.tabs_of(slot)?;
    let titles = huecos
        .iter()
        .map(|id| {
            app.panes
                .browser(*id)
                .map(|p| {
                    let bytes = p
                        .dir()
                        .file_name()
                        .map_or_else(Vec::new, |s| s.as_bytes().to_vec());
                    if bytes.is_empty() {
                        p.dir().scheme().to_owned()
                    } else {
                        norte_frontend::display_name_with(&bytes, p.name_encoding()).0
                    }
                })
                .unwrap_or_default()
        })
        .collect();
    Some(TabStrip { titles, active })
}

/// Cuántas filas del pane son CROMO: los dos bordes, la cabecera de columnas
/// y, si está en un grupo, la barra de pestañas.
fn pane_chrome_rows(app: &App, side: usize) -> u16 {
    3 + u16::from(tab_strip_for(app, side).is_some())
}

/// El interior de un bloque con borde por los cuatro lados./// El interior de un bloque con borde por los cuatro lados./// El interior de un bloque con borde por los cuatro lados.
fn block_inner(area: Rect) -> Rect {
    Block::default().borders(Borders::ALL).inner(area)
}

/// Como [`sync_layout`], desde el área EXTERNA del panel (la que recibe
/// `draw_sync`): descuenta el borde antes de repartir.
fn sync_layout_rows(area: Rect, view: &crate::app::SyncView) -> (Option<Rect>, Rect, Option<Rect>) {
    sync_layout(block_inner(area), view)
}

/// La geometría PINTADA de los dos panes en un frame de `area`, o `None`
/// cuando este frame no pinta panes (visor abierto).
///
/// El reparto YA NO se calcula aquí: sale de `pane_rects`, la misma llamada
/// que usa `draw`. Lo que sigue viviendo aquí es el CROMO — los bordes del
/// bloque y la cabecera de columnas—, que es lo que convierte un rectángulo de
/// pane en filas de listado.
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
pub fn pane_geometry(app: &App, area: Rect) -> Option<Vec<crate::mouse::PaneGeometry>> {
    // Ni con el visor ni con el panel de diferencias: los dos sustituyen a
    // los panes, y una geometría de algo que no está pintado es un click
    // resuelto contra una fila que el lector no puede ver.
    if app.viewer.is_some() || app.compare.is_some() {
        return None;
    }
    let cols = pane_rects(app, area);
    // Un `PaneGeometry` por panel PINTADO. La longitud varía con el layout,
    // y el hit test resuelve contra la del último frame — que es lo que el
    // lector tenía delante.
    let mut out = vec![crate::mouse::PaneGeometry::default(); cols.len()];
    for (i, pane) in app.panes.iter().enumerate() {
        let Some(block) = cols.get(i).copied() else {
            continue;
        };
        // Interior del bloque con `Borders::ALL`, sin construir el bloque:
        // un margen de 1 por lado. `title_bottom` (el input del quick
        // search) NO consume filas — se pinta sobre el borde inferior.
        let inner_w = block.width.saturating_sub(2);
        let inner_h = block.height.saturating_sub(2);
        // La cabecera de columnas se come la primera fila del interior, y la
        // barra de pestañas —si el pane está en un grupo— otra por encima.
        let chrome = pane_chrome_rows(app, i).saturating_sub(2);
        let list_rows = inner_h.saturating_sub(chrome);
        out[i] = crate::mouse::PaneGeometry {
            x: block.x,
            y: block.y,
            width: block.width,
            height: block.height,
            first_list_row: block.y.saturating_add(1).saturating_add(chrome),
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
    if app.menu.is_some() {
        draw_menu(frame, app);
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
    // Fase A: el selector de disposiciones, con el mismo allowlist de teclas
    // que el de temas (`ALLOW_PICKER`) y por eso el mismo hint.
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

#[allow(clippy::too_many_arguments)] // wiring del render, no API
/// Las pestañas de un pane: el título de cada una y cuál está activa.
///
/// Los títulos vienen ya SANEADOS (`display_name`): el nombre de un directorio
/// hostil dentro de una pestaña es tan hostil como dentro de un listado.
pub struct TabStrip {
    /// Título de cada pestaña, en orden.
    pub titles: Vec<String>,
    /// Cuál está activa.
    pub active: usize,
}

/// Lo que se puede pulsar en la barra de menús.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuHit {
    /// Un título: lo abre.
    Title(usize),
    /// Un elemento del menú abierto: lo ejecuta.
    Item(usize),
}

/// Una zona pulsable de la barra de menús.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuZone {
    /// Fila.
    pub row: u16,
    /// Primera columna, inclusive.
    pub x0: u16,
    /// Última columna, inclusive.
    pub x1: u16,
    /// Qué hace pulsarla.
    pub hit: MenuHit,
}

/// La geometría del menú: títulos con su rango y el desplegable con el suyo.
///
/// UNA fuente para lo que se pinta y lo que se pulsa, por lo mismo que la
/// barra de pestañas: medirlo dos veces es cómo un click abre el menú de al
/// lado.
struct MenuGeom {
    /// `(label, x0, x1)` de cada título.
    titles: Vec<(String, u16, u16)>,
    /// La caja del desplegable.
    drop: Rect,
    /// `(label, chord)` de cada elemento del menú abierto.
    items: Vec<(String, String)>,
}

/// Tope de ancho del desplegable: un menú es una lista de etiquetas cortas,
/// así que uno ancho es siempre un síntoma. El tope evita que una traducción
/// larga vuelva a tapar la pantalla, que es lo que pasaba cuando las etiquetas
/// eran las frases de `help-cmd-*`.
const DROP_MAX: u16 = 44;

/// Calcula la geometría del menú abierto, o `None` si no hay ninguno.
fn menu_geom(app: &App, area: Rect) -> Option<MenuGeom> {
    let st = app.menu.as_ref()?;
    let mut titles = Vec::new();
    let mut x = area.x;
    for m in norte_frontend::menu::MENUS {
        let label = format!(" {} ", norte_i18n::t(m.title));
        let w = u16::try_from(UnicodeWidthStr::width(label.as_str())).unwrap_or(0);
        let x1 = x.saturating_add(w).saturating_sub(1);
        titles.push((label, x, x1));
        x = x.saturating_add(w);
    }
    let m = norte_frontend::menu::MENUS.get(st.menu())?;
    let items: Vec<(String, String)> = m
        .items
        .iter()
        .map(|id| {
            // La etiqueta es CORTA y propia (`menu-item-*`), no la frase de
            // `help-cmd-*`: esa es una descripción, y usarla hacía el
            // desplegable de setenta columnas y tapaba los dos paneles. Lo
            // destapó pilotar la TUI en tmux, no la suite.
            let label = norte_i18n::t(&format!("menu-item-{}", id.replace('.', "-")));
            let chord = app
                .palette_rows
                .iter()
                .find(|r| r.key == *id)
                .map_or_else(|| "—".to_owned(), |r| r.chord.clone());
            (label, chord)
        })
        .collect();
    // Ancho: la etiqueta más larga, su tecla, dos bordes y el hueco entre
    // ambas columnas.
    let text_width = items
        .iter()
        .map(|(l, c)| UnicodeWidthStr::width(l.as_str()) + UnicodeWidthStr::width(c.as_str()) + 3)
        .max()
        .unwrap_or(10);
    let w = u16::try_from(text_width + 2)
        .unwrap_or(u16::MAX)
        .min(area.width)
        .min(DROP_MAX);
    let h = u16::try_from(items.len() + 2)
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(1));
    let x0 = titles
        .get(st.menu())
        .map_or(area.x, |(_, x0, _)| *x0)
        .min(area.x.saturating_add(area.width).saturating_sub(w));
    Some(MenuGeom {
        titles,
        drop: Rect {
            x: x0,
            y: area.y.saturating_add(1),
            width: w,
            height: h,
        },
        items,
    })
}

/// Las zonas pulsables de la barra de menús.
#[must_use]
pub fn menu_zones(app: &App, area: Rect) -> Vec<MenuZone> {
    let Some(g) = menu_geom(app, area) else {
        return Vec::new();
    };
    let mut out: Vec<MenuZone> = g
        .titles
        .iter()
        .enumerate()
        .map(|(i, (_, x0, x1))| MenuZone {
            row: area.y,
            x0: *x0,
            x1: *x1,
            hit: MenuHit::Title(i),
        })
        .collect();
    for (i, _) in g.items.iter().enumerate() {
        let row = g
            .drop
            .y
            .saturating_add(1)
            .saturating_add(u16::try_from(i).unwrap_or(0));
        if row >= g.drop.y.saturating_add(g.drop.height).saturating_sub(1) {
            break;
        }
        out.push(MenuZone {
            row,
            x0: g.drop.x.saturating_add(1),
            x1: g.drop.x.saturating_add(g.drop.width).saturating_sub(2),
            hit: MenuHit::Item(i),
        });
    }
    out
}

/// Pinta la barra de menús y su desplegable.
fn draw_menu(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    let Some(g) = menu_geom(app, area) else {
        return;
    };
    let Some(st) = app.menu.as_ref() else {
        return;
    };
    let bar = Rect { height: 1, ..area };
    clear_themed(frame, bar, &app.theme);
    let spans: Vec<ratatui::text::Span<'static>> = g
        .titles
        .iter()
        .enumerate()
        .map(|(i, (label, _, _))| {
            let style = if i == st.menu() {
                app.theme.role(Role::Selection)
            } else {
                app.theme.role(Role::Title)
            };
            ratatui::text::Span::styled(label.clone(), style)
        })
        .collect();
    frame.render_widget(Paragraph::new(ratatui::text::Line::from(spans)), bar);

    clear_themed(frame, g.drop, &app.theme);
    let inner = Block::default().borders(Borders::ALL).inner(g.drop);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(app.theme.role(Role::BorderFocus)),
        g.drop,
    );
    let width = usize::from(inner.width);
    let lines: Vec<ratatui::text::Line<'static>> = g
        .items
        .iter()
        .enumerate()
        .map(|(i, (label, chord))| {
            let slot = width
                .saturating_sub(UnicodeWidthStr::width(label.as_str()))
                .saturating_sub(UnicodeWidthStr::width(chord.as_str()));
            let text = format!("{label}{}{chord}", " ".repeat(slot));
            let style = if i == st.item() {
                app.theme.role(Role::Selection)
            } else {
                app.theme.role(Role::Regular)
            };
            ratatui::text::Line::styled(text, style)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Lo que se puede pulsar en una barra de pestañas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabAction {
    /// Ir a la pestaña `n` (base 0).
    Goto(usize),
    /// Abrir una pestaña.
    New,
    /// Cerrar la activa.
    Close,
}

/// Una zona pulsable de la barra de pestañas de un panel.
///
/// Se calcula del MISMO sitio que pinta la barra, por lo mismo que la
/// geometría del listado: un rango deducido a ojo resuelve el click a la
/// pestaña de al lado, y eso no se ve como un bug de ratón.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabZone {
    /// Posición visible del panel.
    pub pane: usize,
    /// Fila donde está la barra.
    pub row: u16,
    /// Primera columna, inclusive.
    pub x0: u16,
    /// Última columna, inclusive.
    pub x1: u16,
    /// Qué hace pulsarla.
    pub action: TabAction,
}

/// El botón de abrir pestaña. ASCII: un `+` en una caja no puede medir dos
/// celdas en un terminal cualquiera, y un `⊕` sí.
const TAB_NEW: &str = "[+]";
/// El botón de cerrar la activa.
const TAB_CLOSE: &str = "[x]";

/// Los trozos de la barra, cada uno con su ancho y qué hace pulsarlo.
fn tab_pieces(t: &TabStrip) -> Vec<(String, TabAction)> {
    let mut v: Vec<(String, TabAction)> = t
        .titles
        .iter()
        .enumerate()
        .map(|(i, titulo)| (format!(" {titulo} "), TabAction::Goto(i)))
        .collect();
    v.push((TAB_NEW.to_owned(), TabAction::New));
    v.push((TAB_CLOSE.to_owned(), TabAction::Close));
    v
}

/// Las zonas pulsables de los paneles con pestañas, en el frame de `area`.
///
/// Vive junto al pintado —y no en el ratón— por lo mismo que
/// [`pane_geometry`]: quien sabe dónde cayó cada cosa es el `draw`.
#[must_use]
pub fn tab_zones(app: &App, area: Rect) -> Vec<TabZone> {
    let cols = pane_rects(app, area);
    let mut out = Vec::new();
    for (pane, rect) in cols.iter().enumerate() {
        let Some(t) = tab_strip_for(app, pane) else {
            continue;
        };
        // La barra es la PRIMERA fila del interior del bloque.
        let row = rect.y.saturating_add(1);
        let mut x = rect.x.saturating_add(1);
        let tope = rect.x.saturating_add(rect.width).saturating_sub(1);
        for (text, action) in tab_pieces(&t) {
            let w = u16::try_from(UnicodeWidthStr::width(text.as_str())).unwrap_or(0);
            if w == 0 || x >= tope {
                break;
            }
            let x1 = x.saturating_add(w).saturating_sub(1).min(tope - 1);
            out.push(TabZone {
                pane,
                row,
                x0: x,
                x1,
                action,
            });
            x = x.saturating_add(w);
        }
    }
    out
}

/// Marca del panel DESTINO en su título. ASCII a propósito, como el badge
/// hostil: una flecha unicode es ambiguous-width y ocuparía dos celdas en
/// muchos terminales.
const TARGET_BADGE: &str = "->";

/// Pinta la barra de pestañas si la hay, y devuelve dónde caen la cabecera de
/// columnas y el listado.
///
/// Con pestañas, la PRIMERA fila del interior es la barra y todo lo demás baja
/// una: por eso `pane_chrome_rows` cuenta lo mismo, y el test de ancla lo
/// contrasta contra el buffer.
fn draw_tab_strip(
    frame: &mut Frame<'_>,
    inner: Rect,
    tabs: Option<&TabStrip>,
    theme: &TuiTheme,
) -> (Rect, Rect) {
    let bar = u16::from(tabs.is_some());
    if let Some(t) = tabs
        && inner.height > 0
    {
        let mut bar_area = inner;
        bar_area.height = 1;
        frame.render_widget(Paragraph::new(tab_strip_line(t, theme)), bar_area);
    }
    let mut cab = inner;
    cab.y = inner.y.saturating_add(bar);
    cab.height = 1;
    let mut lst = inner;
    lst.y = inner.y.saturating_add(bar).saturating_add(1);
    lst.height = inner.height.saturating_sub(bar).saturating_sub(1);
    (cab, lst)
}

/// La línea de la barra de pestañas.
fn tab_strip_line<'a>(t: &TabStrip, theme: &TuiTheme) -> ratatui::text::Line<'a> {
    // Los MISMOS trozos que mide `tab_zones`: si los dos los calcularan por
    // su cuenta, un click resolvería a la pestaña de al lado.
    let spans = tab_pieces(t)
        .into_iter()
        .map(|(text, action)| {
            let estilo = if action == TabAction::Goto(t.active) {
                theme.role(Role::Selection)
            } else {
                ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::DIM)
            };
            ratatui::text::Span::styled(text, estilo)
        })
        .collect::<Vec<_>>();
    ratatui::text::Line::from(spans)
}
