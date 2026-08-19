//! Render ratatui del estado (`app`): cero lógica de negocio — pinta lo que
//! hay. El marcado de nombres hostiles sigue la spec §6 (lossy y MARCADO). Los
//! colores salen del tema resuelto (`app.theme`, ADR 0020): un frontend sin
//! tema ve el fallback monocromo de M1.

use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, List, ListItem, ListState, Paragraph, Wrap,
};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, display_name};
use crate::theme::TuiTheme;
use norte_i18n::{t, ta};

mod compare;
mod help;
mod modals;
mod pane;
mod status;
mod sync;
mod text;

// `tests/` los llama por `norte_tui::ui::..`, asi que siguen siendo API.
pub use help::{help_body_size, help_group_is_painted, help_sidebar_width};
use help::draw_help;
use compare::{compare_layout, draw_compare};
use modals::draw_modal;
use pane::draw_pane;
use status::draw_status;
use sync::{draw_sync, sync_layout};

use text::{
    head,
    middle, two_fields, with_badge, };

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
