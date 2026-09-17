//! Dónde va cada cosa: resolver el reparto para un frame y traducirlo a `Rect`.
//!
//! Nada de aquí pinta. Es lo que `draw` consulta antes de repartir el frame, y
//! también lo que consulta el enrutado de ratón para saber qué hay bajo el
//! cursor sin haber pintado.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::widgets::{Block, Borders};

use super::chrome::TabStrip;
use super::compare::compare_layout;
use super::sync::sync_layout;
use crate::app::App;

/// Filas del panel de tasks en un frame (tope 6): parte del layout de
/// [`draw`], extraída para que [`pane_list_rows`] cuente lo MISMO que se
/// pinta.
pub(crate) fn tasks_rows(app: &App) -> u16 {
    u16::try_from(app.board.rows().len().min(6)).unwrap_or(6)
}

/// Filas de LISTADO que cada pane pinta en un frame de alto `frame_height`
/// (#124): el alto del frame menos el panel de tasks y la barra de estado
/// (layout de [`super::draw`]), menos los dos bordes del bloque del pane y su línea
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
    // El registro (#323), por el mismo motivo y con el mismo remedio. Nació
    // con un alto ADIVINADO —diez, el que trae el hueco al abrirse— mientras
    // su `draw` usaba el interior real, que son ocho: cada página se saltaba
    // dos líneas y la primera, cuatro. Adivinar el viewport rompe el scroll en
    // silencio, que es justo lo que esta función existe para no dejar hacer.
    if let Some((_, rect)) = placed_of_kind(&res, &app.layout, crate::logview::KIND) {
        let inner = block_inner(rect);
        app.log_panel.set_viewport_rows(usize::from(inner.height));
    }
}

/// El reparto de ESTE frame, con los `Auto` ya sustituidos.
///
/// El árbol guardado (`app.layout`) conserva sus `Auto`; el del frame no.
/// Sustituir aquí y no dentro de `resolve` es lo que mantiene al motor puro y
/// sin closures en su firma.
/// Los BORDES que se pueden arrastrar en el frame de `area`.
///
/// Un borde es el hueco entre dos huecos ADYACENTES del reparto: el de la
/// izquierda (o el de arriba) es quien lo lleva, porque es el que
/// `Node::drag_border` sabe nombrar. Lo que se guarda es dónde empieza la
/// pareja y cuánto ocupa junta, que es lo que convierte una columna del
/// puntero en una fracción.
///
/// Sale del MISMO reparto que pinta, no de una segunda cuenta: dos cálculos
/// de dónde está un borde son un borde que se agarra en un sitio y se mueve
/// desde otro.
#[must_use]
pub fn resize_borders(app: &App, area: Rect) -> Vec<crate::mouse::ResizeBorder> {
    use norte_frontend::layout::Dir;
    let res = resolved_frame(app, area);
    // Solo entre PANELES. La barra de estado y la franja de tareas también
    // son huecos del reparto y también tienen bordes, pero miden una fila fija
    // y arrastrarlas no significa nada — y ofrecerlas se comía la última fila
    // del panel de encima, que sí es suya. «Panel» es lo que el registro
    // compartido llama enfocable.
    let panel = |id| {
        app.layout
            .kind_of(id)
            .and_then(|k| app.kinds.get(k))
            .is_some_and(|d| d.focusable)
    };
    let mut out = Vec::new();
    for (a, ra) in &res.placements {
        if !panel(*a) {
            continue;
        }
        for (b, rb) in &res.placements {
            if !panel(*b) {
                continue;
            }
            // Vertical: `b` empieza justo donde acaba `a`, y se solapan en
            // filas. El `+ 1` es la columna del borde, que en el TUI es el
            // marco que los dos pintan.
            if rb.x == ra.x + ra.width && solapan(ra.y, ra.height, rb.y, rb.height) {
                out.push(crate::mouse::ResizeBorder {
                    slot: *a,
                    dir: Dir::Horizontal,
                    linea: ra.x + ra.width,
                    desde: ra.y.max(rb.y),
                    hasta: (ra.y + ra.height).min(rb.y + rb.height),
                    inicio: ra.x,
                    largo: ra.width + rb.width,
                });
            }
            if rb.y == ra.y + ra.height && solapan(ra.x, ra.width, rb.x, rb.width) {
                out.push(crate::mouse::ResizeBorder {
                    slot: *a,
                    dir: Dir::Vertical,
                    linea: ra.y + ra.height,
                    desde: ra.x.max(rb.x),
                    hasta: (ra.x + ra.width).min(rb.x + rb.width),
                    inicio: ra.y,
                    largo: ra.height + rb.height,
                });
            }
        }
    }
    out
}

/// Los HUECOS que se colocaron en el frame de `area`, con su rectángulo.
///
/// Es lo que convierte un click en «qué panel señaló el puntero». Sale del
/// MISMO reparto que pinta, por lo mismo que los bordes: una segunda cuenta de
/// dónde está cada panel es un click que enfoca el de al lado.
///
/// Van TODOS los huecos colocados, incluidos los que no toman teclas: quién
/// escucha lo decide `App::focus_slot` con el registro compartido, y no una
/// segunda tabla escrita aquí.
#[must_use]
pub fn panel_slots(app: &App, area: Rect) -> Vec<crate::mouse::PanelSlot> {
    resolved_frame(app, area)
        .placements
        .into_iter()
        .map(|(slot, r)| crate::mouse::PanelSlot {
            slot,
            x: r.x,
            y: r.y,
            width: r.width,
            height: r.height,
        })
        .collect()
}

/// ¿Se solapan dos tramos `[a, a+la)` y `[b, b+lb)`?
const fn solapan(a: u16, la: u16, b: u16, lb: u16) -> bool {
    a < b + lb && b < a + la
}

pub(crate) fn resolved_frame(app: &App, area: Rect) -> norte_frontend::layout::Resolved {
    let tree = app.layout.substitute_auto(&|id| natural(app, id));
    norte_frontend::layout::resolve(
        crate::panel::from_ratatui(body_area(app, area)),
        &tree,
        &app.kinds,
    )
}

/// El área que le queda al CUERPO: la del frame menos la barra de menú, si
/// está fijada.
///
/// La resta se hace AQUÍ y en ningún otro sitio. Este es el único punto por el
/// que pasan el pintado, el mapeo de clics del ratón y las decisiones de «qué
/// hueco se colocó» del bucle de eventos, así que restando una vez las tres
/// cuadran solas — y restando en el pintor, el ratón habría seguido creyendo
/// que la fila 0 es del panel de arriba y cada clic habría caído una fila más
/// abajo de donde el lector lo dio.
///
/// Un terminal de una sola fila se queda sin cuerpo antes que sin barra, y por
/// eso la resta es saturante: es preferible una pantalla degradada a un
/// reparto sobre un rectángulo de altura negativa.
#[must_use]
pub(crate) fn body_area(app: &App, area: Rect) -> Rect {
    // Dos filas de cromo posibles arriba, cada una opcional por su cuenta: la
    // de menús y la de paneles (#324). Se restan las que estén, y saturando —
    // es preferible una pantalla degradada a un reparto sobre altura negativa.
    let filas = u16::from(app.menu_bar) + u16::from(app.panel_bar);
    // Y la de teclas ABAJO (spec 2026-09-10): se resta del alto, no del
    // origen. Mismo criterio que las dos de arriba: una sola resta, aquí.
    let abajo = u16::from(key_bar_area(app, area).is_some());
    if filas + abajo == 0 || area.height == 0 {
        return area;
    }
    Rect {
        y: area.y.saturating_add(filas),
        height: area.height.saturating_sub(filas).saturating_sub(abajo),
        ..area
    }
}

/// La fila donde va la barra de teclas (spec 2026-09-10), si está: la ÚLTIMA
/// del frame, como en mc, far y norton, y la de estado queda encima. La fila
/// se RESERVA aunque haya un overlay delante —abrir un modal no recoloca la
/// pantalla de detrás, como con la barra de paneles—; lo que se pinta en
/// ella lo decide `App::key_bar_cells`, y con un modal es nada.
#[must_use]
pub(crate) fn key_bar_area(app: &App, area: Rect) -> Option<Rect> {
    // Con las tres barras en un terminal de tres filas no queda cuerpo; la
    // de teclas es la que cede: `<=` para que no caiga fuera del búfer.
    let arriba = u16::from(app.menu_bar) + u16::from(app.panel_bar);
    if !app.chrome.key_bar() || area.height <= arriba.saturating_add(1) {
        return None;
    }
    Some(Rect {
        y: area.y.saturating_add(area.height).saturating_sub(1),
        height: 1,
        ..area
    })
}

/// La fila donde va la barra de paneles, si está.
///
/// Debajo de la de menús cuando las dos están: el menú nombra lo que se puede
/// hacer y la barra enseña dónde está, así que el orden de arriba abajo es de
/// lo general a lo concreto.
#[must_use]
pub(crate) fn panel_bar_area(app: &App, area: Rect) -> Option<Rect> {
    // `<=` y no `== 0`: con las dos barras encendidas en un terminal de una
    // fila, la de paneles caería FUERA del búfer. Ratatui recorta y no
    // revienta, pero las zonas pulsables se publicarían sobre una fila que no
    // existe.
    if !app.panel_bar || area.height <= u16::from(app.menu_bar) {
        return None;
    }
    Some(Rect {
        y: area.y.saturating_add(u16::from(app.menu_bar)),
        height: 1,
        ..area
    })
}

/// ¿Se ve la barra de paneles AHORA?
///
/// Distinto de [`panel_bar_area`], que es geometría: el hueco de la fila se
/// resta del cuerpo esté quien esté encima —si no, abrir un modal recolocaría
/// toda la pantalla detrás—, pero con un overlay delante la barra ni se pinta
/// ni se puede pulsar.
///
/// Existe porque no tenerlo fue un BLOCKER: la barra se pintaba antes que los
/// overlays y sus zonas seguían activas debajo, así que con la ayuda abierta un
/// clic en la barra de título de la ayuda —fila 1— caía en un botón y abría o
/// cerraba un panel invisible. Pintada y pulsable tienen que ser lo mismo, y la
/// forma de garantizarlo es que las dos pregunten aquí.
#[must_use]
pub(crate) fn panel_bar_visible(app: &App, area: Rect) -> Option<Rect> {
    if crate::mouse::overlay_open(app) || app.menu.is_some() {
        return None;
    }
    panel_bar_area(app, area)
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
pub(crate) fn natural(app: &App, id: norte_frontend::layout::SlotId) -> (u16, u16) {
    if id == crate::panel::SLOT_TASKS {
        (0, tasks_rows(app))
    } else {
        (0, 0)
    }
}

/// Dónde cayó un hueco en este reparto.
pub(crate) fn slot_rect(
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
pub(crate) fn placed_of_kind(
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
pub(crate) fn body_rect(
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
pub(crate) fn chrome_body(app: &App, area: Rect) -> Rect {
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
pub(crate) fn overlay_body(app: &App, area: Rect) -> Rect {
    let res = resolved_frame(app, area);
    body_rect(&res, &app.layout).unwrap_or_else(|| chrome_body(app, area))
}

/// Los `browser` que este reparto SÍ pinta, de izquierda a derecha.
///
/// Con pestañas hay más de dos listados vivos y solo dos visibles, así que
/// «el pane izquierdo» deja de ser un id fijo y pasa a ser una POSICIÓN: el
/// browser colocado más a la izquierda. Ordenar por `(x, y)` es exactamente lo
/// que el usuario ve, y es lo que mantiene el significado de `app.panes[0]`.
pub(crate) fn visible_browsers(
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
pub(crate) fn pane_cols(
    res: &norte_frontend::layout::Resolved,
    tree: &norte_frontend::layout::Node,
) -> Vec<Rect> {
    visible_browsers(res, tree)
        .into_iter()
        .map(|(_, r)| r)
        .collect()
}

/// Como [`pane_cols`], resolviendo el frame por su cuenta.
pub(crate) fn pane_rects(app: &App, area: Rect) -> Vec<Rect> {
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
pub(crate) fn pane_chrome_rows(app: &App, side: usize) -> u16 {
    3 + u16::from(tab_strip_for(app, side).is_some())
}

/// El interior de un bloque con borde por los cuatro lados./// El interior de un bloque con borde por los cuatro lados./// El interior de un bloque con borde por los cuatro lados.
pub(crate) fn block_inner(area: Rect) -> Rect {
    Block::default().borders(Borders::ALL).inner(area)
}

/// El reparto del visor a pantalla completa en sus DOS filas: el marco de
/// contenido (con sus bordes — lo que recibe el `Block` de `draw_viewer`) y
/// la barra de estado de una fila debajo.
///
/// Única función que hace esta cuenta: [`rect_del_visor`] es su primera
/// mitad, y `draw_viewer` toma las dos de aquí en vez de repetir el
/// `Layout::split` a mano — dos cuentas del mismo hueco divergen en
/// silencio (memoria `funcion-compartida-no-basta`).
pub(crate) fn visor_split(app: &App, area: Rect) -> (Rect, Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(body_area(app, area));
    (rows[0], rows[1])
}

/// El hueco donde el visor a pantalla completa pinta su CONTENIDO: el marco
/// completo, bordes incluidos — lo mismo que recibe el `Block` de
/// `draw_viewer`.
///
/// `pub` porque el run loop (T4, fase 5 WOW) la necesita tras
/// `terminal.draw` para saber dónde colocar los píxeles de una imagen: el
/// interior sin bordes es `block_inner` (privado, sin enlazar) de este mismo
/// rect, que es exactamente lo que `draw_viewer` usa para su `inner_h`. Sale
/// de `visor_split` (privado también), la MISMA cuenta que pinta el marco —
/// no una copia.
#[must_use]
pub fn rect_del_visor(app: &App, area: Rect) -> Rect {
    visor_split(app, area).0
}

/// Como [`sync_layout`], desde el área EXTERNA del panel (la que recibe
/// `draw_sync`): descuenta el borde antes de repartir.
pub(crate) fn sync_layout_rows(
    area: Rect,
    view: &crate::app::SyncView,
) -> (Option<Rect>, Rect, Option<Rect>) {
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

pub(crate) fn centered(base: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(base.width);
    let h = h.min(base.height);
    Rect {
        x: base.x + (base.width - w) / 2,
        y: base.y + (base.height - h) / 2,
        width: w,
        height: h,
    }
}
