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
    Block, Borders, List, ListItem, ListState, Paragraph, Wrap,
};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Pane, display_name};
use crate::theme::TuiTheme;
use norte_i18n::{t, ta};

mod help;
mod modals;
mod text;

// `tests/` los llama por `norte_tui::ui::..`, asi que siguen siendo API.
pub use help::{help_body_size, help_group_is_painted, help_sidebar_width};
use help::draw_help;
use modals::draw_modal;

use text::{
    cells, clamp_spans, head,
    middle, take_width, two_fields, with_badge, wrapped_rows,
};

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
        let active = norte_frontend::columns::sort_column_id(col) == Some(sort.column);
        let w = usize::from(*w);
        let arrow = if sort.dir == SortDir::Asc {
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
            let budget = if active { w.saturating_sub(1) } else { w };
            let mut cab = take_width(&label, budget);
            if active {
                cab.push(arrow);
            }
            let pad = w.saturating_sub(cab.width());
            out.push_str(&cab);
            out.push_str(&" ".repeat(pad));
        } else {
            // No-nombre: el ancho incluye el separador — contenido dentro
            // de w-1, misma cuenta que la celda. Derecha: relleno delante.
            // Izquierda (#108 7b): el separador sigue ABRIENDO el ancho,
            // el contenido va tras él y el relleno cae a la derecha.
            let content = w.saturating_sub(1);
            let budget = if active {
                content.saturating_sub(1)
            } else {
                content
            };
            let mut cab = take_width(&label, budget);
            if active {
                cab.push(arrow);
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
    let min_width = u16::try_from(footer_w.saturating_add(4)).unwrap_or(u16::MAX);
    let width = frame
        .area()
        .width
        .saturating_sub(6)
        .clamp(24, 80)
        .max(min_width)
        .min(frame.area().width);
    let area = centered(
        frame.area(),
        width,
        frame.area().height.saturating_sub(4).max(6),
    );
    clear_themed(frame, area, theme);
    // Ancho útil para la segunda línea (description, P1): igual criterio que
    // `draw_palette` (borde + margen), NO el `width` de la caja completa.
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
    let min_width = u16::try_from(footer_w.saturating_add(4)).unwrap_or(u16::MAX);
    let width = frame
        .area()
        .width
        .saturating_sub(6)
        .clamp(24, 80)
        .max(min_width)
        .min(frame.area().width);
    let area = centered(
        frame.area(),
        width,
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
    let text = format!("   {}", middle_ellipsis(&masked, inner.saturating_sub(3)));
    Some(Line::styled(text, theme.role(Role::BorderUnfocused)))
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
    let width = u16::try_from(footer_w.saturating_add(4))
        .unwrap_or(u16::MAX)
        .max(34)
        .min(frame.area().width);
    let rows = u16::try_from(picker.names.len()).unwrap_or(8) + 2;
    let area = centered(frame.area(), width, rows.min(frame.area().height.max(3)));
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
            let mark = if r.enabled { "[x]" } else { "[ ]" };
            let label = match r.builtin {
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
            let arrow = match r.builtin.and_then(sort_column) {
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
            format!(" {mark} {label}{arrow}{formato}")
        })
        .collect();
    let footer_w = Line::raw(format!(" {hint} ")).width();
    let content_w = filas
        .iter()
        .map(|f| Line::raw(f.as_str()).width())
        .max()
        .unwrap_or(0);
    let width = u16::try_from(footer_w.max(content_w).saturating_add(4))
        .unwrap_or(u16::MAX)
        .max(34)
        .min(frame.area().width);
    // M2 revisión 7a: saturante — una config hostil de 65k ids desbordaría
    // el `+ 2` en debug; el `.min(alto del frame)` de abajo sigue clampando.
    let rows = u16::try_from(p.rows().len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = centered(frame.area(), width, rows.min(frame.area().height.max(3)));
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

/// Ancho, en celdas, de la vista previa del selector de disposiciones.
///
/// Fijo, y no proporcional al frame: la vista previa es un DIBUJO a escala de
/// la pantalla, y su parecido con lo que saldrá no mejora por ser más grande.
const LAYOUT_PREVIEW_W: u16 = 30;

/// Alto de esa misma vista previa. La proporción importa más que el tamaño —
/// una vista previa cuadrada haría pasar por alto un `simple` por un
/// `orthodox`.
const LAYOUT_PREVIEW_H: u16 = 10;

/// Fase A: el selector de disposiciones. Las filas a la izquierda y, a la
/// derecha, la pantalla que daría la que está bajo el cursor.
///
/// **La vista previa sale del REPARTO del árbol**, no de un dibujo guardado al
/// lado del fichero: un dibujo guardado empieza a mentir en cuanto alguien
/// toca un tamaño, y el lector no tiene forma de saber cuál de los dos es la
/// pantalla de verdad.
///
/// Solo se dibuja la de un preset DE FÁBRICA, cuyo TOML va embebido. Una
/// disposición del usuario vive en disco, y leer un fichero en el camino de
/// pintado —una vez por frame— es la clase de coste que no se ve hasta que la
/// config está en un directorio de red.
/// El selector de conexiones (#140).
///
/// Nombre y dirección, que es lo que hay en `connections.toml`: jamás un
/// secreto — las credenciales se referencian (ADR 0015) y aquí no llegan. Las
/// dos cosas se enmascaran igual: son texto de un fichero que el usuario
/// escribió, y un nombre con bidi no reordena este cuadro.
fn draw_connections_picker(
    frame: &mut Frame<'_>,
    p: &norte_frontend::connections_picker::ConnectionsPicker,
    theme: &TuiTheme,
    hint: &str,
) {
    let rows: Vec<String> = p
        .rows()
        .iter()
        .map(|r| {
            format!(
                " {} · {}",
                norte_encoding::mask_terminal_hazards(&r.name),
                norte_encoding::mask_terminal_hazards(&r.url)
            )
        })
        .collect();
    // Sin conexiones se enseña POR QUÉ está vacío y dónde se ponen: una caja
    // vacía deja al lector pensando que la tecla se rompió.
    let body: Vec<String> = if rows.is_empty() {
        vec![format!(" {}", t("connections-picker-empty"))]
    } else {
        rows
    };
    let width = body
        .iter()
        .map(|f| Line::raw(f.as_str()).width())
        .max()
        .unwrap_or(0);
    let width = u16::try_from(width).unwrap_or(u16::MAX).max(24);
    let footer = format!(" {hint} ");
    let width = width.max(u16::try_from(footer.chars().count()).unwrap_or(u16::MAX));
    let height = u16::try_from(body.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = centered(frame.area(), width.saturating_add(2), height);
    clear_themed(frame, area, theme);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("connections-picker-title")))
        .title_style(theme.role(Role::Title))
        .title_bottom(Line::raw(footer))
        .border_style(theme.role(Role::ModalBorder));
    let inside = block.inner(area);
    frame.render_widget(block, area);
    let lines: Vec<Line<'_>> = body
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let l = Line::raw(f.as_str());
            if i == p.cursor() && !p.rows().is_empty() {
                l.style(theme.role(Role::Selection))
            } else {
                l
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inside);
}

fn draw_layout_picker(
    frame: &mut Frame<'_>,
    p: &norte_frontend::layout_picker::LayoutPicker,
    theme: &TuiTheme,
    hint: &str,
) {
    let rows: Vec<String> = p
        .rows()
        .iter()
        .map(|r| {
            let origin = if r.factory {
                t("layout-picker-factory")
            } else {
                t("layout-picker-mine")
            };
            // El nombre es un STEM de fichero y puede no ser texto: lossy
            // MARCADO con su badge y hazards enmascarados, como cualquier
            // otro nombre de la pantalla (#246 m2/m3).
            let (name, hostile) = norte_frontend::display_os_name(&r.name);
            let name = norte_encoding::mask_terminal_hazards(&name);
            let badge = if hostile { " ⚠" } else { "" };
            format!(" {name}{badge} · {origin}")
        })
        .collect();
    // La nota del keymap habla de la fila BAJO EL CURSOR, no de la lista: es
    // un aviso sobre lo que el lector está a punto de elegir.
    // La nota va DENTRO de la caja, en su propia línea, y no en el pie: un
    // aviso que se corta a media frase por no caber en el borde es peor que
    // no darlo, y a 80 columnas el pie no da para las dos cosas.
    let note_text = format!(" {}", t("layout-picker-keymap-note"));
    let has_note = p
        .rows()
        .get(p.cursor())
        .is_some_and(|r| r.shares_keymap_name);
    let footer = format!(" {hint} ");

    let list_w = rows
        .iter()
        .map(|f| Line::raw(f.as_str()).width())
        .max()
        .unwrap_or(0);
    let list_w = u16::try_from(list_w).unwrap_or(u16::MAX).max(18);
    let inside = list_w.saturating_add(LAYOUT_PREVIEW_W).saturating_add(1);
    // `+ 2` por los bordes, y el pie se mide DENTRO de ellos: sin sumarlos
    // aquí la nota del keymap se corta a media palabra, que es peor que no
    // darla.
    // El ancho se reserva para la nota SIEMPRE que alguna fila pueda pedirla,
    // no solo cuando la pide la de ahora: si no, la caja se encoge y se
    // ensancha mientras el cursor recorre las filas, y lo que se compara es
    // justamente el dibujo de dentro.
    let note_w = if p.rows().iter().any(|r| r.shares_keymap_name) {
        u16::try_from(Line::raw(note_text.as_str()).width()).unwrap_or(u16::MAX)
    } else {
        0
    };
    let width = inside
        .max(u16::try_from(Line::raw(footer.as_str()).width()).unwrap_or(u16::MAX))
        .max(note_w)
        .saturating_add(2)
        .min(frame.area().width);
    let rows_height = u16::try_from(rows.len()).unwrap_or(u16::MAX);
    // La línea de la nota se reserva SIEMPRE que la lista pueda pedirla, por
    // lo mismo que el ancho: la caja no debe cambiar de alto al moverse.
    let note_height = u16::from(note_w > 0);
    let height = rows_height
        .max(LAYOUT_PREVIEW_H)
        .saturating_add(2)
        .saturating_add(note_height)
        .min(frame.area().height.max(3));
    let area = centered(frame.area(), width, height);
    clear_themed(frame, area, theme);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("layout-picker-title")))
        .title_style(theme.role(Role::Title))
        .title_bottom(Line::raw(footer))
        .border_style(theme.role(Role::ModalBorder));
    let inside_area = block.inner(area);
    frame.render_widget(block, area);

    let bands = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(note_height)])
        .split(inside_area);
    if has_note && bands[1].height > 0 {
        frame.render_widget(
            Paragraph::new(Line::raw(note_text.as_str())).style(theme.role(Role::Info)),
            bands[1],
        );
    }
    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(list_w.min(bands[0].width)),
            Constraint::Min(0),
        ])
        .split(bands[0]);

    let items: Vec<ListItem<'_>> = rows.into_iter().map(ListItem::new).collect();
    let list = List::new(items).highlight_style(theme.role(Role::Selection));
    let mut state = ListState::default();
    state.select(Some(p.cursor()));
    frame.render_stateful_widget(list, halves[0], &mut state);

    if halves[1].width == 0 || halves[1].height == 0 {
        return; // un frame estrecho se queda con la lista, que es lo que se elige
    }
    if let Some(row) = p.current() {
        draw_layout_preview(frame, halves[1], row, theme);
    }
}

/// La mitad derecha del selector: la pantalla de la fila bajo el cursor.
///
/// Sale de la fila, sea de fábrica o del usuario. Filtrar por `factory`
/// dejaba la mitad derecha en blanco para los ficheros propios —y para uno de
/// fábrica TAPADO por un fichero— mientras la ayuda prometía que cada fila
/// dibuja su pantalla (#244 M3).
fn draw_layout_preview(
    frame: &mut Frame<'_>,
    area: Rect,
    row: &norte_frontend::layout_picker::Row,
    theme: &TuiTheme,
) {
    use norte_frontend::layout::KindRegistry;
    use norte_frontend::layout_picker::preview;

    if let Some(tree) = row.tree.as_ref() {
        let lines = preview(tree, area.width, area.height, &KindRegistry::builtin());
        let text: Vec<Line<'_>> = lines.into_iter().map(Line::raw).collect();
        frame.render_widget(Paragraph::new(text), area);
    } else if let Some(problema) = row.problem.as_deref() {
        // Un fichero que no parsea DICE por qué, en el sitio donde iría su
        // pantalla: un hueco en blanco no se distingue de una disposición
        // vacía. El diagnóstico viene de un fichero, así que se enmascara.
        let text = norte_encoding::mask_terminal_hazards(problema);
        frame.render_widget(
            Paragraph::new(text)
                .style(theme.role(Role::Warning))
                .wrap(Wrap { trim: false }),
            area,
        );
    }
}

/// Command palette (`Ctrl+P`/vim `:`, H1 T4, spec-promised): filtro libre
/// sobre TODOS los comandos, mismo idioma visual que [`draw_nav_popup`]
/// (centrado, input al pie, `Clear` antes de pintar) pero MÁS ancha (60
/// columnas: `{text} {descripción} {chord}` no cabe en el ancho de un
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
                    let text = format!(" {:<24} {:<32} {}", row.text, row.desc, row.chord);
                    ListItem::new(Line::raw(middle_ellipsis(&text, inner)))
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
    let width = frame
        .area()
        .width
        .saturating_sub(6)
        .clamp(30, 80)
        .min(frame.area().width);
    let height = frame.area().height.saturating_sub(4).max(6);
    let area = centered(frame.area(), width, height);
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
            let text = if row.is_plugins_note() {
                format!("{cursor} {}", row.name)
            } else {
                format!("{cursor} {:<28} {}", row.name, row.value)
            };
            let mut line = Line::raw(middle_ellipsis(&text, inner_w));
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
    let width = frame
        .area()
        .width
        .saturating_sub(4)
        .clamp(30, 92)
        .min(frame.area().width);
    let height = frame.area().height.saturating_sub(2).max(6);
    let area = centered(frame.area(), width, height);
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
    let (title, hostile) =
        norte_frontend::path_display_with(&viewer.path, app.focused().name_encoding());
    let title = if hostile {
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

/// El visor ACOPLADO (L3): el fichero bajo el cursor, en su hueco.
///
/// El mismo renderer que [`draw_viewer`] —mismas filas, mismo preview de
/// plugin— dentro de un bloque del tamaño del hueco en vez de la pantalla
/// entera. La línea de estado del visor (encoding, EOL, pérdidas, truncado) va
/// en el borde de abajo: es el único sitio que dice QUÉ se está viendo, y un
/// visor que no lo dice miente por omisión.
///
/// Sin fichero, el hueco lleva un texto: un directorio, un listado vacío, o el
/// motivo por el que la lectura no pudo hacerse. Una denegación se PINTA aquí
/// y no abre nada — el preview sigue al cursor, así que un diálogo por
/// pulsación convertiría bajar por un directorio en una ráfaga de modales.
fn draw_preview(
    frame: &mut Frame<'_>,
    area: Rect,
    preview: &crate::preview::Preview,
    con_teclado: bool,
    app: &App,
) {
    let border = if con_teclado {
        Role::BorderFocus
    } else {
        Role::BorderUnfocused
    };
    let Some(viewer) = preview.viewer() else {
        let text = preview.note().unwrap_or_default().to_owned();
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", t("preview-title")))
            .title_style(app.theme.role(Role::Title))
            .border_style(app.theme.role(border));
        frame.render_widget(
            Paragraph::new(Line::styled(text, app.theme.role(Role::Info))).block(block),
            area,
        );
        return;
    };
    let (title, hostile) =
        norte_frontend::path_display_with(&viewer.path, app.focused().name_encoding());
    let title = if hostile {
        format!("{HOSTILE_BADGE} {title}")
    } else {
        title
    };
    let width = usize::from(area.width.saturating_sub(2));
    let mut block = Block::default()
        .borders(Borders::ALL)
        // La ruta se recorta por el MEDIO: en un hueco estrecho lo que
        // identifica un fichero es su nombre, o sea la cola.
        .title(norte_frontend::middle_ellipsis(&title, width))
        .title_style(app.theme.role(Role::Title))
        .border_style(app.theme.role(border))
        .title_bottom(Line::raw(norte_frontend::middle_ellipsis(
            &crate::viewer::status(viewer),
            width,
        )));
    if let Some(plugin) = viewer.preview_plugin() {
        block = block.title(
            Line::from(Span::styled(
                ta("viewer-plugin-preview", &[("plugin", plugin)]),
                app.theme.role(Role::Info),
            ))
            .right_aligned(),
        );
    }
    let inner_h = area.height.saturating_sub(2) as usize;
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
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// El sidebar de sitios (L3): discos y favoritos en un panel que se queda.
///
/// Todo lo que la plataforma nos da —la etiqueta de un volumen, su punto de
/// montaje, el nombre que el usuario le puso a un favorito— pasa por el mismo
/// enmascarado que el popup de unidades (`display_name`/`path_display`): un
/// `fuse.<subtype>` lo elige un usuario sin privilegios, y una etiqueta de
/// FAT es tan hostil como un nombre de fichero.
///
/// Un favorito roto se pinta ATENUADO y con su motivo traducido, nunca se
/// esconde: un favorito que desaparece solo es un fallo de config invisible.
fn draw_places(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &norte_frontend::places::PlacesState,
    con_teclado: bool,
    theme: &TuiTheme,
) {
    use norte_frontend::places::PlaceRow;

    let border = if con_teclado {
        Role::BorderFocus
    } else {
        Role::BorderUnfocused
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("places-title")))
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(border));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let width = inner.width as usize;
    let items: Vec<ListItem<'_>> = state
        .rows()
        .iter()
        .map(|row| match row {
            PlaceRow::Header { section, folded } => {
                let arrow = if *folded { '▸' } else { '▾' };
                ListItem::new(Line::styled(
                    head(&format!("{arrow} {}", t(section.label_key())), width),
                    theme.role(Role::Title),
                ))
            }
            PlaceRow::Drive {
                label, mount, free, ..
            } => {
                let (nombre, hostile) = if label.is_empty() {
                    mount_name(mount)
                } else {
                    display_name(label)
                };
                // Corto y sin decimales: catorce celdas tienen que llevar el
                // nombre del montaje Y su espacio. Un `?` cuando el
                // filesystem no contestó — jamás un cero, que se leería como
                // «lleno» (la palabra entera la sigue diciendo el popup, que
                // sí tiene sitio).
                let libre = free.map_or_else(|| "?".to_owned(), norte_frontend::human_bytes_short);
                // El nombre de un montaje se recorta por el MEDIO: lo que
                // identifica `/home/oscar/.cache` es la cola, y con seis
                // montajes bajo `/home` una lista recortada por delante son
                // seis filas que ponen lo mismo.
                ListItem::new(Line::raw(two_fields(
                    &with_badge(&nombre, hostile),
                    &libre,
                    width,
                    middle,
                )))
            }
            PlaceRow::Favorite { name, target } => {
                let (text, hostile) = display_name(name.as_bytes());
                let left = with_badge(&text, hostile);
                match target {
                    Ok(_) => ListItem::new(Line::raw(head(&format!(" {left}"), width))),
                    // Roto: marca `!` y fila ATENUADA. El motivo entero no
                    // cabe en catorce celdas —«ruta inválida» son trece— y
                    // recortarlo dejaría media palabra diciendo nada, así que
                    // la fila dice QUE está roto y la barra de estado dice por
                    // qué cuando el cursor cae encima. Lo que no se hace es
                    // esconderla: un favorito que desaparece solo es un fallo
                    // de config invisible.
                    Err(_) => ListItem::new(Line::styled(
                        two_fields(&left, "!", width, head),
                        theme.role(Role::Info),
                    )),
                }
            }
        })
        .collect();
    let list = List::new(items).highlight_style(theme.role(Role::Selection));
    // El desplazamiento se calcula AQUÍ y no lo decide el widget, para que
    // `places_zones` pueda decir con qué fila del modelo se corresponde cada
    // fila de la pantalla (#226). Es el mismo número que ratatui elegía por su
    // cuenta —desplazamiento mínimo para que el cursor se vea, partiendo de
    // cero en cada frame—, así que la pantalla no cambia; lo que cambia es que
    // ahora hay UNA fuente y el ratón la puede leer.
    let mut estado = ListState::default().with_offset(if con_teclado {
        places_offset(state.cursor(), inner.height as usize)
    } else {
        0
    });
    estado.select(con_teclado.then(|| state.cursor()));
    frame.render_stateful_widget(list, inner, &mut estado);
}

/// Primera fila del modelo que se ve, para un cursor y un alto.
///
/// Desplazamiento MÍNIMO para que el cursor entre, empezando de cero: es lo
/// que hacía el widget con un `ListState` nuevo en cada frame, escrito para
/// que el hit test del ratón no tenga que adivinarlo.
const fn places_offset(cursor: usize, height: usize) -> usize {
    cursor.saturating_sub(height.saturating_sub(1))
}

/// Una fila pulsable del sidebar de sitios, en el frame de `area`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaceZone {
    /// Fila de la pantalla.
    pub row: u16,
    /// Primera columna, inclusive.
    pub x0: u16,
    /// Última columna, inclusive.
    pub x1: u16,
    /// Índice dentro de [`norte_frontend::places::PlacesState::rows`].
    pub index: usize,
}

/// Las filas pulsables del sidebar, en el frame de `area`.
///
/// Vive junto al pintado y comparte con él el reparto y el desplazamiento
/// —igual que [`tab_zones`] y por lo mismo—: medir por un lado y pintar por
/// otro es cómo un click acaba activando la fila de al lado.
#[must_use]
pub fn places_zones(app: &App, area: Rect) -> Vec<PlaceZone> {
    let res = resolved_for(app, area);
    let Some((id, rect)) = placed_of_kind(&res, &app.layout, "places") else {
        return Vec::new();
    };
    let Some(state) = app.panes.places(id) else {
        return Vec::new();
    };
    // El interior del bloque: el marco no es pulsable.
    let inner = Block::default().borders(Borders::ALL).inner(rect);
    if inner.width == 0 || inner.height == 0 {
        return Vec::new();
    }
    let offset = if app.key_owner() == crate::app::KeyOwner::Places {
        places_offset(state.cursor(), inner.height as usize)
    } else {
        0
    };
    (0..inner.height as usize)
        .filter_map(|row| {
            let index = offset.checked_add(row)?;
            if index >= state.rows().len() {
                return None;
            }
            Some(PlaceZone {
                row: inner
                    .y
                    .saturating_add(u16::try_from(row).unwrap_or(u16::MAX)),
                x0: inner.x,
                x1: inner.x.saturating_add(inner.width).saturating_sub(1),
                index,
            })
        })
        .collect()
}

/// Cómo se llama un punto de montaje en catorce celdas.
///
/// Sin el prefijo `⟨file⟩` de [`norte_frontend::path_display`] cuando el
/// esquema es local, que es SIEMPRE en `host.volumes`: en un panel de catorce
/// celdas ese prefijo se come la ruta entera y deja al lector mirando seis
/// filas que ponen lo mismo. El enmascarado no se pierde — cada segmento pasa
/// por `display_name` igual que hace `path_display`.
fn mount_name(mount: &norte_proto::VPath) -> (String, bool) {
    if mount.scheme() != "file" || mount.authority().is_some() {
        return norte_frontend::path_display(mount);
    }
    let mut text = String::new();
    let mut hostile = false;
    for seg in mount.segments() {
        let (t, h) = display_name(seg);
        text.push('/');
        text.push_str(&t);
        hostile |= h;
    }
    if text.is_empty() {
        text.push('/');
    }
    (text, hostile)
}

/// El porcentaje de una tarea: por bytes si se conocen, si no por entradas.
///
/// Una sola copia porque la franja y el panel de procesos pintan lo mismo, y
/// dos aritméticas del mismo número acaban dividiendo una de ellas por un
/// total que puede ser cero.
fn progress_pct(p: &norte_proto::TaskProgress) -> u64 {
    match (p.bytes_total, p.entries_total) {
        (Some(total), _) if total > 0 => (p.bytes_done.saturating_mul(100) / total).min(100),
        (_, Some(total)) if total > 0 => (p.entries_done.saturating_mul(100) / total).min(100),
        _ => 0,
    }
}

/// El panel de procesos (fase A): una fila por tarea, con barra y estado.
///
/// Las filas salen del `TaskBoard` que ya pinta la franja — este panel no
/// guarda una segunda lista — y el cursor se acota AQUÍ contra las filas de
/// este frame: una tarea puede terminar y desaparecer entre dos pinturas.
/// El árbol de directorios (#136).
///
/// Un nombre por fila, sangrado por profundidad, con un indicador de tres
/// estados: desplegada, plegada-con-hijos, y sin leer. El tercero importa —
/// pintar «hoja» a algo que todavía no se ha listado sería inventarse la
/// respuesta— y es el mismo criterio que el resto de la pantalla: lo que no se
/// sabe se dice, no se rellena.
///
/// Los nombres van por `display_name`, como el listado: un directorio con bidi
/// o invisibles no reordena esta columna.
fn draw_tree(
    frame: &mut Frame<'_>,
    area: Rect,
    tree: &crate::tree::Tree,
    app: &App,
    con_teclado: bool,
) {
    let theme = &app.theme;
    let border = if con_teclado {
        Role::BorderFocus
    } else {
        Role::BorderUnfocused
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("tree-title")))
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(border));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let rows = tree.rows();
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(t("tree-loading"), theme.role(Role::Title))),
            inner,
        );
        return;
    }
    let cursor = tree.cursor();
    let items: Vec<ListItem<'_>> = rows
        .iter()
        .map(|r| {
            let mark = match (r.expanded, r.children) {
                (true, _) => "▾",
                (false, Some(true)) => "▸",
                // Leída y sin hijos: una hoja de verdad.
                (false, Some(false)) => " ",
                // Sin leer: ni hoja ni rama, todavía.
                (false, None) => "·",
            };
            let (name, hostile) = display_name(
                r.path
                    .file_name()
                    .map_or(b"/".as_slice(), norte_proto::Segment::as_bytes),
            );
            let indent = "  ".repeat(r.depth);
            let text = if hostile {
                format!("{indent}{mark} {HOSTILE_BADGE} {name}")
            } else {
                format!("{indent}{mark} {name}")
            };
            ListItem::new(Line::raw(text))
        })
        .collect();
    let mut list_state = ListState::default();
    list_state.select(Some(cursor));
    let list = List::new(items).highlight_style(theme.role(Role::Selection));
    frame.render_stateful_widget(list, inner, &mut list_state);
}

fn draw_processes(
    frame: &mut Frame<'_>,
    area: Rect,
    processes: &crate::processes::Processes,
    app: &App,
    con_teclado: bool,
) {
    let theme = &app.theme;
    let border = if con_teclado {
        Role::BorderFocus
    } else {
        Role::BorderUnfocused
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("processes-title")))
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(border));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let rows = app.board.rows();
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(t("processes-empty"), theme.role(Role::Title))),
            inner,
        );
        return;
    }
    let cursor = processes.cursor(rows.len());
    let items: Vec<ListItem<'_>> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let p = &row.last;
            let pct = progress_pct(p);
            // Diez celdas de barra: cabe en un panel estrecho y sigue
            // diciendo de un vistazo por dónde va.
            let full = usize::try_from(pct / 10).unwrap_or(0).min(10);
            let bar: String = "█".repeat(full) + &"░".repeat(10 - full);
            let (state_txt, role) = match &p.state {
                norte_proto::TaskState::Completed => ("✓".to_owned(), Some(Role::Info)),
                norte_proto::TaskState::Cancelled => (t("task-cancelled"), Some(Role::Warning)),
                norte_proto::TaskState::Failed { .. } => (t("task-failed"), Some(Role::Error)),
                _ => (format!("{pct}%"), None),
            };
            let header = format!(
                "{} #{} {bar} ",
                if i == cursor { '▶' } else { ' ' },
                p.task_id.get()
            );
            let tail = match role {
                Some(r) => Span::styled(state_txt, theme.role(r)),
                None => Span::raw(state_txt),
            };
            ListItem::new(Line::from(vec![Span::raw(header), tail]))
        })
        .collect();
    frame.render_widget(List::new(items), inner);
}

/// La hoja de atributos (fase A): lo que se sabe de la entrada bajo el cursor.
///
/// Todo sale de la `Entry` que el listado ya tenía, así que esta función no
/// puede pedir nada aunque quisiera. El tamaño va por `human_bytes_short`, que
/// redondea hacia ABAJO y no se recorta: un tamaño cortado por la cabeza es un
/// número FALSO, no una etiqueta truncada (la lección de L3).
fn draw_metadata(
    frame: &mut Frame<'_>,
    area: Rect,
    entry: Option<&norte_proto::Entry>,
    app: &App,
    con_teclado: bool,
) {
    let theme = &app.theme;
    let border = if con_teclado {
        Role::BorderFocus
    } else {
        Role::BorderUnfocused
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("metadata-title")))
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(border));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let Some(e) = entry else {
        frame.render_widget(
            Paragraph::new(Line::styled(t("metadata-empty"), theme.role(Role::Title))),
            inner,
        );
        return;
    };

    let mut lines: Vec<Line<'_>> = Vec::new();
    let mut field = |clave: &str, value: String| {
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", t(clave)), theme.role(Role::Title)),
            Span::raw(value),
        ]));
    };

    let nombre = e
        .path
        .file_name()
        .map_or_else(Vec::new, |s| s.as_bytes().to_vec());
    let (text, hostile) = display_name(&nombre);
    field("metadata-name", with_badge(&text, hostile));
    field(
        "metadata-kind",
        t(match e.kind {
            norte_proto::EntryKind::Dir => "metadata-kind-dir",
            norte_proto::EntryKind::File => "metadata-kind-file",
            norte_proto::EntryKind::Symlink => "metadata-kind-symlink",
            norte_proto::EntryKind::Other => "metadata-kind-other",
        }),
    );
    if let Some(n) = e.size {
        field(
            "metadata-size",
            format!("{} ({n})", norte_frontend::human_bytes_short(n)),
        );
    }
    if let Some(ms) = e.mtime_ms {
        field(
            "metadata-mtime",
            norte_frontend::columns::format_mtime(ms, norte_frontend::columns::TimeFormat::Iso, ms),
        );
    }
    // Los atributos que el provider YA había traído con el listado. Se pintan
    // por la misma puerta que la columna equivalente —`styled_cell`, con el
    // estilo por defecto del id— para que la hoja y la columna no puedan
    // discrepar sobre lo que vale un atributo.
    let catalog = app.attr_catalog(e.path.scheme());
    let now = e.mtime_ms.unwrap_or(0);
    for id in e.attrs.keys() {
        let col: norte_frontend::columns::ColumnId =
            norte_frontend::columns::ColumnId::Attr(id.clone());
        let style = norte_frontend::columns::ColumnStyle::default_for_id(&col, catalog);
        let label = norte_frontend::columns::header_label(&col, &style, catalog);
        if let Some(celda) = norte_frontend::columns::styled_cell(e, &col, now, &style) {
            free_field(&mut lines, theme, &label, &celda);
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Una fila etiqueta/valor cuya etiqueta no sale de Fluent sino del catálogo.
fn free_field(lines: &mut Vec<Line<'static>>, theme: &TuiTheme, label: &str, value: &str) {
    lines.push(Line::from(vec![
        Span::styled(format!("{label} "), theme.role(Role::Title)),
        Span::raw(value.to_owned()),
    ]));
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
            let pct = progress_pct(p);
            // Por CATEGORÍA (Display estable), jamás Debug de cara al usuario.
            // El estado se colorea por rol (error rojo, hecho info).
            let (state, role) = match &p.state {
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
                Some(r) => Span::styled(state, app.theme.role(r)),
                None => Span::raw(state),
            };
            Line::from(vec![head, tail])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
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
        let row: String = (0..21).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert_eq!(row, "   f.txt          7 B", "{row:?}");
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
            .draw(|f| {
                draw_pane(
                    f,
                    f.area(),
                    &pane,
                    true,
                    &theme,
                    0,
                    &settings,
                    None,
                    None,
                    false,
                );
            })
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
        let row_e1 = text
            .lines()
            .find(|l| l.contains("aaa"))
            .expect("fila de aaa");
        assert!(
            row_e1.contains('\u{FFFD}'),
            "owner lossy sin marcar: {row_e1:?}"
        );
        // 3. La fila de e2 (sin attrs) pinta las columnas attr EN BLANCO:
        //    quitando el nombre, los bordes y los espacios no queda nada
        //    (blanco = AUSENTE, jamás un valor fabricado).
        let row_e2 = text
            .lines()
            .find(|l| l.contains("bbb"))
            .expect("fila de bbb");
        // (Las comillas por línea las pone el Display de `TestBackend`.)
        let rest: String = row_e2
            .replace("bbb", "")
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '│' && *c != '"')
            .collect();
        assert_eq!(rest, "", "ausencia debe ser blanco: {row_e2:?}");
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
            .draw(|f| {
                draw_pane(
                    f,
                    f.area(),
                    &pane,
                    true,
                    &theme,
                    0,
                    &settings,
                    None,
                    None,
                    false,
                );
            })
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
    let (header, inner, filtros_area, keys_area) = compare_layout(outer);

    // Anchos: las dos marcas y su separación en el centro, el resto a partes
    // iguales entre las dos caras. `saturating_sub` porque un terminal
    // estrecho es un terminal, no un panic.
    let sides = inner.width.saturating_sub(COMPARE_MARKS_W + 1);
    let face_w = usize::from(sides / 2).max(1);

    if let Some(a) = header {
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
    let visible = view.pane.visible_len();
    let height = usize::from(inner.height);
    let selected = view.pane.visible_index();
    // La ventana la decide el MODELO (#210, pegajosa como la del listado).
    let offset = view.pane.viewport_offset().min(visible.saturating_sub(1));
    let rows: Vec<ListItem<'_>> = view
        .pane
        .visible()
        .skip(offset)
        .take(height)
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
            let mark = if view.pane.is_marked(row.id) {
                '*'
            } else {
                ' '
            };
            ListItem::new(Line::from(vec![
                Span::styled(mark.to_string(), theme.role(Role::Selection)),
                compare_face_span(cells.left.as_ref(), face_w, theme),
                Span::styled(
                    format!(" {}{} ", cells.glyphs.verdict, cells.glyphs.confidence),
                    compare_mark_style(theme, row.verdict),
                ),
                compare_face_span(cells.right.as_ref(), face_w, theme),
            ]))
        })
        .collect();
    if visible == 0 {
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
            .filter(|i| *i < height),
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
    if let Some(a) = keys_area {
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
    let (left, right) = compare_title_halves(view, usize::from(frame_width));
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
        badge_span(left.hostile),
        Span::styled(left.text, theme.role(Role::Title)),
        Span::styled(COMPARE_TITLE_SEP, theme.role(Role::Info)),
        badge_span(right.hostile),
        Span::styled(right.text, theme.role(Role::Title)),
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
    let (left_txt, left_hostile) =
        norte_frontend::path_display_with(&view.left_root, view.left_encoding);
    let (right_txt, right_hostile) =
        norte_frontend::path_display_with(&view.right_root, view.right_encoding);
    let badge_w = |h: bool| if h { HOSTILE_BADGE.width() } else { 0 };
    let prefix_w = format!(" {} — ", t("compare-title")).width();
    // Bordes del marco (2) + prefijo + separador + el espacio final + las
    // dos marcas — todo lo que NO es texto de raíz, reservado antes de
    // repartir lo que queda.
    let fixed =
        2 + prefix_w + COMPARE_TITLE_SEP.width() + 1 + badge_w(left_hostile) + badge_w(right_hostile);
    let roots_w = frame_width.saturating_sub(fixed).max(2);
    let left_w = (roots_w / 2).max(1);
    let right_w = roots_w.saturating_sub(left_w).max(1);
    (
        CompareTitleHalf {
            text: norte_frontend::middle_ellipsis(&left_txt, left_w),
            hostile: left_hostile,
        },
        CompareTitleHalf {
            text: norte_frontend::middle_ellipsis(&right_txt, right_w),
            hostile: right_hostile,
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
        let (left, right) = compare_title_halves(&vista(izquierda, derecha), 200);
        assert!(
            left.text.contains('→'),
            "la flecha se queda DENTRO de su mitad: {}",
            left.text
        );
        assert!(
            right.text.ends_with("de/verdad"),
            "y la derecha llega intacta a la suya: {}",
            right.text
        );
    }

    /// Una raíz izquierda kilométrica se recorta CON marca (`…`), nunca en
    /// silencio, y no se come a la derecha: el reparto de ancho es POR
    /// MITAD, reservado antes de construir ningún span.
    #[test]
    fn raiz_larga_se_recorta_y_no_expulsa_a_la_otra() {
        let long =
            vp("mem:///").join(norte_proto::Segment::new(vec![b'x'; 4096]).expect("segmento"));
        let derecha = vp("mem:///derecha/de/verdad");
        let (left, right) = compare_title_halves(&vista(long, derecha), 60);
        assert!(left.text.contains('…'), "el corte se MARCA: {}", left.text);
        assert!(
            right.text.ends_with("de/verdad") || right.text.contains("de/verdad"),
            "la otra raíz sigue intacta: {}",
            right.text
        );
    }

    /// Con espacio de sobra las dos raíces llegan completas, sin badge (no
    /// son hostiles).
    #[test]
    fn sin_saneado_las_dos_raices_llegan_completas() {
        let (left, right) =
            compare_title_halves(&vista(vp("mem:///izquierda"), vp("mem:///derecha")), 200);
        assert!(!left.hostile);
        assert!(!right.hostile);
        assert!(left.text.contains("izquierda"));
        assert!(right.text.contains("derecha"));
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
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(outer);
    (Some(rows[0]), rows[1], Some(rows[2]), Some(rows[3]))
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
        let off = view.pane.is_hidden(*c);
        let mark = if off { '-' } else { '+' };
        spans.push(Span::styled(
            format!(
                "{}{mark}{} {}",
                i + 1,
                c.label(norte_i18n::active()),
                view.pane.count_of(*c)
            ),
            if off {
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
    let (source_txt, source_hostile) =
        norte_frontend::path_display_with(&view.source_root, view.source_encoding);
    // Con la reinterpretación del DESTINO, no la del origen: un share CP1251
    // en el otro pane se pintaba `????` en el título aunque el lector hubiera
    // pulsado `Alt+E` sobre él.
    let (dest_txt, dest_hostile) =
        norte_frontend::path_display_with(&view.dest_root, view.dest_encoding);
    let badge = |h: bool| if h { HOSTILE_BADGE } else { "" };
    // El brazo `_` NO cae en «actualizar»: `SyncMode` es `#[non_exhaustive]`,
    // y decir «esto no borra» de un modo que esta build no sabe nombrar es
    // afirmar la mitad SEGURA de lo que hay que aprobar. Misma regla que
    // `RelAnchor::Either` y `StepUndo::Unclear` en el mismo modelo.
    // Por el compartido: esta decisión estaba escrita también en la GUI, con
    // su misma regla de que el `_` NO cae a «update» (revisión de rama de C2,
    // rust MAJOR-3).
    let mode = norte_frontend::sync::mode_label(view.mode, norte_i18n::active());
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
        " {} ({mode}) — {}{} → {}{} ",
        t("sync-title"),
        badge(source_hostile),
        source_txt,
        badge(dest_hostile),
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
    let (summary_area, inner, keys_area) = sync_layout(outer, view);
    if let Some(a) = summary_area {
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
        let height = usize::from(inner.height);
        let selected = view
            .state
            .plan()
            .and_then(norte_frontend::sync::SyncPlan::selected_id)
            .and_then(|id| steps.iter().position(|s| s.id == id));
        let offset = view
            .state
            .plan()
            .map_or(0, norte_frontend::sync::SyncPlan::viewport_offset)
            .min(steps.len().saturating_sub(1));
        // Solo se construye lo que cabe, por lo mismo que en el panel de
        // diferencias: un plan puede tener cientos de miles de pasos y esto se
        // repinta diez veces por segundo mientras siguen llegando.
        let rows: Vec<ListItem<'_>> = steps
            .iter()
            .skip(offset)
            .take(height)
            .map(|step| sync_step_item(step, view, usize::from(inner.width), theme))
            .collect();
        let mut state = ListState::default();
        state.select(
            selected
                .and_then(|i| i.checked_sub(offset))
                .filter(|i| *i < height),
        );
        frame.render_stateful_widget(
            List::new(rows).highlight_style(theme.role(Role::Selection)),
            inner,
            &mut state,
        );
    }
    if let Some(a) = keys_area {
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
        let lines = match &view.confirming {
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
            Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
            a,
        );
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
    let summary_height: u16 = view
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
    let summary = summary_height.min((outer.height / 2).max(1));
    // La segunda pregunta se lleva la pregunta ENVUELTA más la fila de la
    // tecla que la contesta. Dos fijas no bastan: a 80 columnas la frase de un
    // borrado irreversible son dos filas ella sola, y la de más abajo es la
    // que dice «¿Seguir?». Acotada como el resumen —la mitad del marco—, y con
    // el suelo en 2 para que la tecla no se quede nunca sin sitio.
    let keys = view.confirming.as_ref().map_or(1, |c| {
        wrapped_rows(&c.text, outer.width)
            .saturating_add(1)
            .min((outer.height / 2).max(2))
    });
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(summary),
            Constraint::Min(1),
            Constraint::Length(keys),
        ])
        .split(outer);
    ((summary > 0).then(|| rows[0]), rows[1], Some(rows[2]))
}

/// El resumen del plan: lo que [`norte_frontend::sync::SyncPlan::summary_lines`]
/// dijo, envuelto.
fn sync_summary(view: &crate::app::SyncView, theme: &TuiTheme) -> Paragraph<'static> {
    let lines: Vec<Line<'static>> = view
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
    Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false })
}

/// Una fila del panel: las tres marcas, la ruta y el tamaño.
///
/// Las tres marcas son de columnas DISTINTAS y se separan, porque el alfabeto
/// no es único entre ellas a propósito (`!` es `Certain` en una e
/// `Irreversible` en otra): juntas se leerían como una palabra.
fn sync_step_item(
    step: &norte_proto::methods::SyncStep,
    view: &crate::app::SyncView,
    width: usize,
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
    let anchor =
        norte_frontend::sync::anchor_label(cells.anchor, norte_i18n::active()).unwrap_or_default();
    let tam = cells
        .size
        .map(norte_frontend::human_bytes)
        .unwrap_or_default();
    let marks = format!(
        "{} {} {} ",
        cells.glyphs.kind, cells.glyphs.confidence, cells.glyphs.undo
    );
    // Por CELDAS y no por `char`s: un ancla o un tamaño con caracteres anchos
    // presupuestaría de menos y la fila desbordaría el marco (#79).
    let path_w = width
        .saturating_sub(marks.width() + anchor.width() + tam.width() + 2)
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
    let path_style = theme.entry(&cells.rel.raw, norte_proto::EntryKind::File);
    let badge_de = |d: &norte_frontend::sync::RelDisplay| {
        if d.hostile { HOSTILE_BADGE } else { "" }
    };
    let mut spans = vec![Span::styled(marks, sync_undo_style(theme, cells.undo))];
    if let Some(d) = &dest_rel {
        const SEP: &str = " → ";
        let fixed = badge_de(&cells.rel).width() + SEP.width() + badge_de(d).width();
        let text_w = path_w.saturating_sub(fixed).max(2);
        // Se reparte a la mitad: las dos ortografías valen lo mismo, y la
        // del destino es la que dice dónde cae la escritura.
        let half = (text_w / 2).max(1);
        spans.push(Span::styled(
            badge_de(&cells.rel),
            theme.role(Role::Warning),
        ));
        spans.push(Span::styled(
            norte_frontend::middle_ellipsis(&cells.rel.text, half),
            path_style,
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
            norte_frontend::middle_ellipsis(&d.text, text_w - half),
            path_style,
        ));
    } else {
        let fixed = badge_de(&cells.rel).width();
        spans.push(Span::styled(
            badge_de(&cells.rel),
            theme.role(Role::Warning),
        ));
        spans.push(Span::styled(
            norte_frontend::middle_ellipsis(&cells.rel.text, path_w.saturating_sub(fixed).max(1)),
            path_style,
        ));
    }
    if !anchor.is_empty() {
        spans.push(Span::styled(format!(" {anchor}"), theme.role(Role::Info)));
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
        let painted = spans(&item).join("");
        assert!(
            painted.contains('\u{2192}'),
            "el separador de la pareja sobrevive al truncado: {painted:?}"
        );
        // El badge PEGADO a cada mitad, y no el recuento a secas: el glifo de
        // confianza de la columna de marcas es el mismo carácter, así que
        // contarlo suelto cuenta tres y no dice nada de dónde están.
        assert_eq!(
            painted.matches(&format!("{HOSTILE_BADGE}caf")).count(),
            2,
            "las DOS mitades siguen marcadas, cada una en su sitio: {painted:?}"
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

#[allow(
    clippy::too_many_arguments,
    reason = "pintar un pane necesita su área, su modelo, el foco, el tema, el               reloj del frame, las columnas, el catálogo de atributos y sus               pestañas; agruparlos en una struct de un solo uso solo movería               la lista de sitio"
)]
fn draw_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    pane: &Pane,
    focused: bool,
    theme: &TuiTheme,
    now_ms: i64,
    settings: &norte_frontend::columns::ColumnsSettings,
    catalog: Option<&norte_proto::AttrCatalog>,
    tabs: Option<&TabStrip>,
    is_dest: bool,
) {
    let border_style = if focused {
        theme.role(Role::BorderFocus)
    } else {
        theme.role(Role::BorderUnfocused)
    };
    let (title, title_hostile) = norte_frontend::path_display_with(pane.dir(), pane.name_encoding());
    let mut title = if title_hostile {
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
    // Un pane que NO se pudo listar al restaurar la sesión lo dice mientras
    // dure (#235): sin esto la pantalla afirma que el directorio está vacío,
    // que es precisamente lo que no se sabe. Va donde la paginación y por la
    // misma razón — un listado que no es el listado jamás es silencioso.
    if pane.unlisted {
        use std::fmt::Write as _;
        let _ = write!(title, " [{}]", norte_i18n::t("pane-unlisted"));
    }
    // El DESTINO se marca en el cromo, y solo cuando hace falta: con dos
    // paneles el destino es el otro y nadie necesita que se lo digan, pero a
    // partir de tres una copia hacia un panel que el lector no tenía en la
    // cabeza es pérdida de datos silenciosa (ADR 0058 D7). El marcador va en
    // el título y FUERA del nombre del directorio, como el badge hostil: un
    // directorio llamado «→» no puede fingirlo.
    if is_dest {
        title = format!("{TARGET_BADGE} {title}");
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
    let (header_area, list_area) = draw_tab_strip(frame, inner, tabs, theme);
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
    let (text, hostile) = norte_frontend::display_name_with(name, reinterpret);
    let kind_glyph = match entry.kind {
        EntryKind::Dir => "/",
        EntryKind::Symlink => "@",
        EntryKind::File | EntryKind::Other => " ",
    };
    let badge = Span::styled(
        if hostile { HOSTILE_BADGE } else { " " },
        theme.role(Role::HostileBadge),
    );
    // Color por tipo/extensión de la entrada (ADR 0020 D2).
    let body = Span::styled(
        format!("{kind_glyph}{text}"),
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
            let fixed: usize = spans[..2].iter().map(|sp| sp.content.width()).sum();
            if fixed + deco + 3 > name_w {
                spans.truncate(3);
            }
        }
        let used: usize = spans.iter().map(|sp| sp.content.width()).sum();
        if used > name_w {
            // Recorta el TEXTO del nombre (el span del body, índice 2) con
            // elipsis central a lo que quede tras los demás spans — los
            // fijos (canalón/badge) y la decoración se quedan.
            let others: usize = spans
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != 2)
                .map(|(_, sp)| sp.content.width())
                .sum();
            let body_w = name_w.saturating_sub(others);
            let truncated = middle_ellipsis(&spans[2].content, body_w);
            spans[2] = Span::styled(truncated, spans[2].style);
        }
        let used: usize = spans.iter().map(|sp| sp.content.width()).sum();
        // Ni con el nombre recortado a cero cabe siempre: en una columna de
        // una o dos celdas —lo que deja `full` en un terminal de 40— el
        // canalón y el badge ya la llenan solos. Se recorta el bloque ENTERO
        // por la derecha. Antes esto era un `debug_assert`, que en tests es un
        // panic y en release una fila pintando fuera de su columna.
        if used > name_w {
            spans = clamp_spans(std::mem::take(&mut spans), name_w);
        }
        let used: usize = spans.iter().map(|sp| sp.content.width()).sum();
        debug_assert!(
            used <= name_w,
            "el bloque del nombre desborda su columna: {used} > {name_w}"
        );
        if used < name_w {
            spans.push(Span::raw(" ".repeat(name_w - used)));
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
            let content = w.saturating_sub(1);
            let cw = cell.width();
            let truncated: String = if cw > content {
                take_width(&cell, content)
            } else {
                cell
            };
            let text = match style.align {
                norte_frontend::columns::Align::Right => {
                    let pad = w.saturating_sub(truncated.width());
                    format!("{}{truncated}", " ".repeat(pad))
                }
                norte_frontend::columns::Align::Left => {
                    let pad = w.saturating_sub(truncated.width().saturating_add(1));
                    format!(" {truncated}{}", " ".repeat(pad))
                }
            };
            spans.push(Span::styled(
                text,
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
    let (dir_text, dir_hostile) =
        norte_frontend::path_display_with(pane.dir(), pane.name_encoding());
    let mark = if dir_hostile { HOSTILE_BADGE } else { "" };
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
                    let line = m.line.map_or_else(String::new, |l| format!(":{l}"));
                    let preview = m.preview.as_deref().map_or_else(String::new, |p| {
                        format!(" {}", crate::app::detail_for_bar(p))
                    });
                    format!("  [{line}{preview}]")
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
        // ancho que elipsise `dir_text` para que NINGÚN campo posterior se
        // recorte jamás, en vez de solo reordenar por prioridad.
        // La RUTA cede, y ceden ella sola: todo lo demás de esta línea es un
        // aviso o un contador, y recortar la cola —que es lo que hacía
        // ratatui— se llevaba lo que decía cuántas entradas hay o que el
        // listado está incompleto. Con una ruta larga, lo que se veía del
        // `pos/total` era un dígito suelto.
        //
        // `middle_ellipsis` recorta por el MEDIO: el principio de una ruta
        // dice dónde estás y el final dice qué carpeta es, y perder cualquiera
        // de los dos extremos es perder la mitad útil.
        let tail = format!("{pos_total}{omitidas}{nombres}{pruned}{ocultas}{marked}{seq}");
        let room = usize::from(area.width)
            .saturating_sub(cells(&tail))
            .saturating_sub(cells(mark))
            .saturating_sub(1); // el margen izquierdo
        let dir_text = norte_frontend::middle_ellipsis(&dir_text, room);
        format!(" {mark}{dir_text}{tail}")
    };
    frame.render_widget(
        Paragraph::new(text).style(app.theme.role(Role::StatusBar)),
        area,
    );
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
        let line = column_header_line(&cols, sort, None);
        assert_eq!(line.width(), 7, "exactamente la suma de anchos: {line:?}");
        assert_eq!(line, "N      ");
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
        let line = column_header_line(&cols, sort, None);
        assert_eq!(line.width(), 8, "{line:?}");
        assert_eq!(line, "N      ▲");
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
        let text = line_text(&line);
        assert_eq!(
            text.chars().filter(|&c| c == 'a').count(),
            crate::app::PLUGIN_DESCRIPTION_WIRE_CAP,
            "el draw procesó más de PLUGIN_DESCRIPTION_WIRE_CAP chars del original: {text:?}"
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
        let text = line_text(&line);
        assert!(!text.contains('\u{202E}'));
        assert!(text.contains('\u{FFFD}'));
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
        // Este test afirma los strings del corpus INGLÉS. Sin fijar el idioma
        // resolvía por entorno (`LANG`), así que era verde en CI y rojo en
        // cualquier máquina con `LANG=es_*` — la misma línea que el resto de
        // los tests de render de este crate ya llevaba.
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let theme = TuiTheme::default();
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).expect("terminal de test");
        terminal
            .draw(|f| draw_which_key(f, &panel(Some(12)), &theme))
            .expect("draw");
        let text = terminal.backend().to_string();
        assert!(text.contains("12 g"), "the count in flight: {text}");
        assert!(text.contains("go to top"), "the available row: {text}");
        // The unavailable row still names its command and says why. It used
        // to be a `Planned` one, with its issue number; #132 built the last of
        // those, so what is unavailable now is a command this frontend does
        // not implement — the row and the reason work the same way, which is
        // the property under test.
        assert!(
            text.contains("pack into an archive"),
            "the unavailable row: {text}"
        );
        assert!(!text.contains("help-cmd-"), "a raw Fluent id: {text}");
        assert!(
            text.contains(&norte_i18n::t("keymap-short-not-here")),
            "and why: {text}"
        );
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
        assert!(
            dim_of("pack into an archive"),
            "an unavailable row is dimmed"
        );
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
    ///
    /// El ejemplo de «no ejecutable» era una capacidad `Planned` con su número
    /// de issue. Con #132 construido no quedan: la razón que se pinta ahora es
    /// la del comando que existe y este frontend no implementa, que es la otra
    /// mitad de lo mismo — y sigue siendo una fila con explicación en vez de
    /// una tecla que no hace nada.
    #[test]
    fn se_ven_la_fila_sin_tecla_y_la_no_construida() {
        let eff = eff();
        let text = painted(&state(&eff));
        assert!(text.contains(&norte_i18n::t("shortcuts-no-key")), "{text}");
        assert!(
            text.contains(&norte_i18n::t("keymap-short-not-here")),
            "la razón de la fila que este build no ejecuta: {text}"
        );
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
