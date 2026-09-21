//! El cromo de la ventana: la barra de menú con sus zonas de clic, y la tira de
//! pestañas de cada lado.
//!
//! Las dos siguen la misma forma: una función MIDE las zonas (`menu_zones`,
//! `tab_zones`) y otra PINTA, porque quien enruta un clic necesita la geometría
//! sin haber pintado nada.

use norte_frontend::panelbar::cifra;
use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::clear_themed;
use super::geometry::{pane_rects, tab_strip_for};
use crate::app::App;
use crate::theme::TuiTheme;

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
pub(crate) struct MenuGeom {
    /// La caja del desplegable.
    drop: Rect,
    /// Las líneas del desplegable, de arriba abajo: órdenes y separaciones.
    lines: Vec<MenuLine>,
}

/// Una línea del desplegable.
pub(crate) enum MenuLine {
    /// El principio de una sección: una raya, con su rótulo si lo tiene.
    Section(Option<String>),
    /// Una orden: su índice en la lista plana del menú, etiqueta, tecla y
    /// papel.
    Item {
        index: usize,
        label: String,
        chord: String,
        role: norte_frontend::menu::ItemRole,
    },
}

/// La marca de una orden que hace un modelo de IA.
pub(crate) const AI_MARK: &str = " ✦";

/// Tope de ancho del desplegable: un menú es una lista de etiquetas cortas,
/// así que uno ancho es siempre un síntoma. El tope evita que una traducción
/// larga vuelva a tapar la pantalla, que es lo que pasaba cuando las etiquetas
/// eran las frases de `help-cmd-*`.
pub(crate) const DROP_MAX: u16 = 44;

/// Calcula la geometría del menú abierto, o `None` si no hay ninguno.
/// Los títulos de la barra y dónde cae cada uno, ABIERTO O NO.
///
/// Separado de [`menu_geom`] porque aquello sale por `?` en cuanto el menú
/// está cerrado —tiene que hacerlo: sin menú abierto no hay desplegable que
/// medir— y con la barra fijada eso dejaba la fila en blanco. Los títulos no
/// dependen de que haya nada abierto; el desplegable sí.
pub(crate) fn menu_titles(area: Rect) -> Vec<(String, u16, u16)> {
    let nombres: Vec<String> = norte_frontend::menu::MENUS
        .iter()
        .map(|m| norte_i18n::t(m.title))
        .collect();
    // Dos espacios entre títulos cuando caben, uno cuando no. Con diez menús
    // la barra en castellano mide 82 columnas a doble espacio, y en un
    // terminal de 80 el último —«Ayuda», justo el que un lector nuevo busca—
    // desaparecía. Apretar la barra antes que amputarla: lo que tiene que
    // decir es qué menús HAY.
    let ancho = |sep: usize| -> usize {
        nombres
            .iter()
            .map(|n| UnicodeWidthStr::width(n.as_str()) + sep)
            .sum()
    };
    let holgado = ancho(2) <= usize::from(area.width);
    let mut titles = Vec::new();
    let mut x = area.x;
    for nombre in nombres {
        let label = if holgado {
            format!(" {nombre} ")
        } else {
            format!(" {nombre}")
        };
        let w = u16::try_from(UnicodeWidthStr::width(label.as_str())).unwrap_or(0);
        let x1 = x.saturating_add(w).saturating_sub(1);
        titles.push((label, x, x1));
        x = x.saturating_add(w);
    }
    titles
}

pub(crate) fn menu_geom(app: &App, area: Rect) -> Option<MenuGeom> {
    let titles = menu_titles(area);
    let st = app.menu.as_ref()?;
    let m = norte_frontend::menu::MENUS.get(st.menu())?;
    let mut lines: Vec<MenuLine> = Vec::new();
    for (index, id) in m.items().enumerate() {
        if let Some(titulo) = m.section_at(index) {
            lines.push(MenuLine::Section(titulo.map(norte_i18n::t)));
        }
        // La etiqueta es CORTA y propia (`menu-item-*`), no la frase de
        // `help-cmd-*`: esa es una descripción, y usarla hacía el
        // desplegable de setenta columnas y tapaba los dos paneles. Lo
        // destapó pilotar la TUI en tmux, no la suite.
        let label = norte_i18n::t(&format!("menu-item-{}", id.replace('.', "-")));
        // Sin tecla, nada: una raya en cada orden sin atajo era ruido que
        // se leía como «deshabilitada».
        let chord = app
            .palette_rows
            .iter()
            .find(|r| r.key == id)
            .map_or_else(String::new, |r| r.chord.clone());
        lines.push(MenuLine::Item {
            index,
            label,
            chord,
            role: norte_frontend::menu::role(id),
        });
    }
    // Un menú que no cabe en alto pierde antes las rayas que las órdenes:
    // primero las separaciones sin nombre, luego los rótulos. Las órdenes se
    // quedan todas, que es para lo que está el menú.
    let alto_max = usize::from(area.height.saturating_sub(3));
    if lines.len() > alto_max {
        lines.retain(|l| !matches!(l, MenuLine::Section(None)));
    }
    if lines.len() > alto_max {
        lines.retain(|l| matches!(l, MenuLine::Item { .. }));
    }
    // Ancho: la etiqueta más larga, su tecla, dos bordes y el hueco entre
    // ambas columnas; y el rótulo de sección más largo con sus rayas.
    let text_width = lines
        .iter()
        .map(|l| match l {
            MenuLine::Item {
                label, chord, role, ..
            } => {
                let marca = if *role == norte_frontend::menu::ItemRole::Ai {
                    UnicodeWidthStr::width(AI_MARK)
                } else {
                    0
                };
                UnicodeWidthStr::width(label.as_str())
                    + marca
                    + UnicodeWidthStr::width(chord.as_str())
                    + 3
            }
            MenuLine::Section(Some(t)) => UnicodeWidthStr::width(t.as_str()) + 4,
            MenuLine::Section(None) => 0,
        })
        .max()
        .unwrap_or(10);
    let w = u16::try_from(text_width + 2)
        .unwrap_or(u16::MAX)
        .min(area.width)
        .min(DROP_MAX);
    let h = u16::try_from(lines.len() + 2)
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(1));
    let x0 = titles
        .get(st.menu())
        .map_or(area.x, |(_, x0, _)| *x0)
        .min(area.x.saturating_add(area.width).saturating_sub(w));
    Some(MenuGeom {
        drop: Rect {
            x: x0,
            y: area.y.saturating_add(1),
            width: w,
            height: h,
        },
        lines,
    })
}

/// Las zonas pulsables de la barra de menús.
#[must_use]
pub fn menu_zones(app: &App, area: Rect) -> Vec<MenuZone> {
    // Los TÍTULOS son pulsables siempre que la barra esté en pantalla, esté
    // el menú abierto o no. Salían de `menu_geom`, que devuelve `None` con el
    // menú cerrado —tiene que hacerlo, sin nada abierto no hay desplegable que
    // medir— así que con la barra fijada se veía y no se podía pulsar: una
    // barra que existe para que encuentres el menú y en la que el clic no
    // hace nada.
    let mut out: Vec<MenuZone> = if app.menu_bar || app.menu.is_some() {
        menu_titles(area)
            .iter()
            .enumerate()
            .map(|(i, (_, x0, x1))| MenuZone {
                row: area.y,
                x0: *x0,
                x1: *x1,
                hit: MenuHit::Title(i),
            })
            .collect()
    } else {
        Vec::new()
    };
    let Some(g) = menu_geom(app, area) else {
        return out;
    };
    // Por LÍNEA pintada, no por orden: con secciones, la orden `i` ya no
    // cae en la fila `i`, y una raya no es pulsable.
    for (fila, linea) in g.lines.iter().enumerate() {
        let row = g
            .drop
            .y
            .saturating_add(1)
            .saturating_add(u16::try_from(fila).unwrap_or(0));
        if row >= g.drop.y.saturating_add(g.drop.height).saturating_sub(1) {
            break;
        }
        let MenuLine::Item { index, .. } = linea else {
            continue;
        };
        out.push(MenuZone {
            row,
            x0: g.drop.x.saturating_add(1),
            x1: g.drop.x.saturating_add(g.drop.width).saturating_sub(2),
            hit: MenuHit::Item(*index),
        });
    }
    out
}

/// Dónde caen los botones de disposición (ADR 0133): en el borde DERECHO de
/// la barra de menús, si caben enteros sin pisar un título. UNA medida para
/// el pintado y para el ratón.
pub(crate) fn layout_button_cells(
    app: &App,
    area: Rect,
) -> Vec<(u16, &'static norte_frontend::layoutbar::LayoutButton)> {
    // Con un overlay delante o el menú desplegado, ni se pintan ni se
    // pulsan: los overlays no tapan la fila 0, así que pintados quedarían a
    // la vista y muertos (revisión de ADR 0133; la barra de paneles tuvo el
    // mismo BLOCKER). La comprobación vive AQUÍ para que pintado y ratón no
    // puedan separarse.
    if !app.menu_bar || app.menu.is_some() || crate::mouse::overlay_open(app) {
        return Vec::new();
    }
    let total = norte_frontend::layoutbar::width();
    let usado: usize = menu_titles(area)
        .iter()
        .filter(|(_, _, x1)| *x1 < area.x.saturating_add(area.width))
        .map(|(l, _, _)| UnicodeWidthStr::width(l.as_str()))
        .sum();
    // Un título vale más que un botón: sin sitio para los cuatro enteros y
    // un espacio de separación, no sale ninguno.
    if usize::from(area.width) < usado + total + 1 {
        return Vec::new();
    }
    let mut x = area
        .x
        .saturating_add(area.width)
        .saturating_sub(u16::try_from(total).unwrap_or(u16::MAX));
    let mut out = Vec::new();
    for b in &norte_frontend::layoutbar::BUTTONS {
        out.push((x, b));
        x = x.saturating_add(u16::try_from(b.glyph.len() + 1).unwrap_or(u16::MAX));
    }
    out
}

/// Pinta los botones de disposición (ADR 0133) en el borde derecho de `bar`
/// y devuelve cuántas celdas reservan, separación incluida — lo que la
/// pista del atajo del menú tiene que dejarles.
fn draw_layout_buttons(frame: &mut Frame<'_>, app: &App, area: Rect, bar: Rect) -> u16 {
    let botones = layout_button_cells(app, area);
    for (x, b) in &botones {
        let w = u16::try_from(b.glyph.len()).unwrap_or(0);
        frame.render_widget(
            Paragraph::new(ratatui::text::Line::styled(
                b.glyph,
                app.theme.role(Role::Title),
            )),
            Rect {
                x: *x,
                width: w,
                ..bar
            },
        );
    }
    if botones.is_empty() {
        0
    } else {
        u16::try_from(norte_frontend::layoutbar::width() + 1).unwrap_or(u16::MAX)
    }
}

/// Las zonas de los botones de disposición: las MISMAS celdas que se
/// pintan (`layout_button_cells`), que ya callan con un overlay delante o
/// el menú abierto.
#[must_use]
pub fn layout_zones(app: &App, area: Rect) -> Vec<PanelZone> {
    layout_button_cells(app, area)
        .into_iter()
        .map(|(x0, b)| PanelZone {
            row: area.y,
            x0,
            x1: x0.saturating_add(u16::try_from(b.glyph.len()).unwrap_or(u16::MAX) - 1),
            command: b.command.to_owned(),
        })
        .collect()
}

/// Una casilla pulsable de la barra de paneles (#324).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelZone {
    /// Fila.
    pub row: u16,
    /// Primera columna, inclusive.
    pub x0: u16,
    /// Última columna, inclusive.
    pub x1: u16,
    /// El comando que dispara pulsarla.
    pub command: String,
}

/// Una celda pulsable de la barra de teclas (spec 2026-09-10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyZone {
    /// Fila.
    pub row: u16,
    /// Primera columna, inclusive.
    pub x0: u16,
    /// Última columna, inclusive.
    pub x1: u16,
    /// La tecla de función, `1`..=`10`.
    pub key: u8,
}

/// Las celdas pulsables de la barra de teclas: el MISMO reparto que el
/// pintado (`keybar::layout`), así que miden lo mismo. Una celda vacía —una
/// tecla que no ata nada en esta pantalla— no es una zona: pulsarla no
/// haría nada, y una zona que no hace nada confunde.
#[must_use]
pub fn key_zones(app: &App, area: Rect) -> Vec<KeyZone> {
    let Some(bar) = crate::ui::geometry::key_bar_area(app, area) else {
        return Vec::new();
    };
    let cells = app.key_bar_cells();
    norte_frontend::keybar::layout(usize::from(bar.width))
        .into_iter()
        .zip(cells)
        .filter(|(_, c)| c.command.is_some())
        .map(|((x0, w), c)| KeyZone {
            row: bar.y,
            x0: bar.x.saturating_add(u16::try_from(x0).unwrap_or(u16::MAX)),
            x1: bar
                .x
                .saturating_add(u16::try_from(x0 + w).unwrap_or(u16::MAX))
                .saturating_sub(1),
            key: c.key,
        })
        .collect()
}

/// Pinta la barra de teclas: diez celdas con el número y lo que hace cada
/// tecla en la pantalla que tiene el teclado. El número lleva el estilo de
/// la barra de estado y la etiqueta el de selección invertido, como en mc:
/// dos tonos para que se lean como diez botones y no como una frase.
pub(crate) fn draw_key_bar(frame: &mut Frame<'_>, app: &App) {
    let Some(bar) = crate::ui::geometry::key_bar_area(app, frame.area()) else {
        return;
    };
    clear_themed(frame, bar, &app.theme);
    let cells = app.key_bar_cells();
    let numero = app.theme.role(Role::Regular);
    // `Button` y no `StatusBar` (2026-09-11): en el tema de serie la barra de
    // estado y el cursor llevan el mismo par de colores, y la fila de teclas
    // encima de la de estado se leía como una sola franja. Una celda de esta
    // barra es un botón, y ese rol ya existe para los de los modales.
    let etiqueta = app.theme.role(Role::Button);
    let mut spans: Vec<ratatui::text::Span<'static>> = Vec::new();
    for ((_, w), c) in norte_frontend::keybar::layout(usize::from(bar.width))
        .into_iter()
        .zip(cells)
    {
        let text = norte_frontend::keybar::cell_text(c, w);
        let n = c.key.to_string().len();
        let (num, label) = text.split_at(n);
        spans.push(ratatui::text::Span::styled(num.to_owned(), numero));
        // Una celda vacía se queda con el fondo base: una tecla que no hace
        // nada no se pinta como un botón.
        let estilo = if c.command.is_some() {
            etiqueta
        } else {
            numero
        };
        spans.push(ratatui::text::Span::styled(label.to_owned(), estilo));
    }
    frame.render_widget(Paragraph::new(ratatui::text::Line::from(spans)), bar);
}

/// ¿Se pintan los NOMBRES de los paneles? `[ui] panel_bar_style = "names"`
/// y que quepan todos en la fila; si no, letras (spec 2026-09-10). Una sola
/// respuesta para el pintado y para las zonas del ratón, que así miden lo
/// mismo.
fn con_nombres(app: &App, buttons: &[norte_frontend::panelbar::PanelButton], bar: Rect) -> bool {
    app.chrome.panel_bar_style() == norte_config::PanelBarStyle::Names
        && norte_frontend::panelbar::names_fit(buttons, usize::from(bar.width))
}

/// El kind del panel que tiene el teclado, si lo tiene un panel.
///
/// Traduce `KeyOwner` a kind: la barra razona en kinds porque es lo que el
/// registro le da, y `KeyOwner` es cosa de la TUI.
/// El préstamo es de `app` y no `'static` desde la fase 3: el kind de un panel
/// de plugin es `plugin:<id>:<kind>`, una cadena que vive en el árbol y no se
/// conoce al compilar.
fn kind_con_teclado(app: &App) -> Option<&str> {
    match app.key_owner() {
        crate::app::KeyOwner::Panes => None,
        crate::app::KeyOwner::Places => Some("places"),
        crate::app::KeyOwner::Preview => Some(crate::preview::KIND),
        crate::app::KeyOwner::Processes => Some(crate::processes::KIND),
        crate::app::KeyOwner::Tree => Some(crate::tree::KIND),
        crate::app::KeyOwner::Log => Some(crate::logview::KIND),
        crate::app::KeyOwner::DiskMap => Some(crate::diskmap::KIND),
        crate::app::KeyOwner::Timeline => Some(crate::timeline::KIND),
        // Cuál es lo dice el reparto, no el enum: hay como mucho uno visible.
        crate::app::KeyOwner::Panel => app.panel_kind(),
    }
}

/// Los botones de la barra de paneles, con lo que sabe la `App`.
///
/// Vive aquí y no en `norte-frontend` la parte de RECOGER el estado; el QUÉ y
/// el ORDEN los decide `panelbar::buttons`, compartido con la ventana.
#[must_use]
pub fn panel_buttons(app: &App, area: Rect) -> Vec<norte_frontend::panelbar::PanelButton> {
    // Del REPARTO y no del árbol (#331). Fue en dos pasos, y los dos hacían
    // falta: #329 cambió `slot_ids` por `visible_slot_ids` porque un hueco
    // detrás de una pestaña inactiva existe y no se ve; pero `visible_slot_ids`
    // contesta qué pestaña está activa, no qué CABE. Un panel cuya pestaña sí
    // está activa y que el reparto descarta por falta de sitio se seguía
    // pintando abierto. Las colocaciones son literalmente lo que se pinta, así
    // que cubren las dos preguntas de una vez.
    //
    // Cuesta un reparto más por frame, como `tab_zones` y sus vecinas: es el
    // precio de que el cromo diga la verdad sobre un cuerpo que ya se repartió.
    let res = crate::ui::geometry::resolved_frame(app, area);
    // En ORDEN DE PANTALLA, que es el de los botones: de arriba abajo y, a
    // igual altura, de izquierda a derecha. El reparto los da en el orden en
    // que recorre el árbol, que casi siempre coincide y no lo garantiza; y
    // «casi siempre» en una fila que se aprende con el dedo no vale.
    let mut colocados: Vec<_> = res.placements.iter().collect();
    colocados.sort_by_key(|(_, r)| (r.y, r.x));
    let abiertos: Vec<&str> = colocados
        .iter()
        .map(|(id, _)| *id)
        .filter_map(|id| {
            app.layout
                .kind_of(id)
                .map(norte_frontend::layout::KindId::as_str)
        })
        .collect();
    let foco = kind_con_teclado(app);
    // Novedad: el registro con errores sin ver, y procesos con tareas vivas.
    // Es lo que hace mirar la barra en vez de recordarla.
    let mut novedad: Vec<(&str, u32)> = Vec::new();
    // Con el panel A LA VISTA ya las estás viendo: la marca sobra, y además le
    // robaba el estilo al estado mientras durase la tarea. Mismo criterio que
    // el registro, aquí abajo.
    //
    // «A la vista» y no «existente» desde #329: escondido en una pestaña no lo
    // estás viendo, y callar la marca ahí apagaba el aviso justo en el caso en
    // que sirve para algo. Se pregunta a `abiertos`, que ya ES el conjunto de
    // kinds visibles: recorrer el árbol otra vez costaría dos pasadas más por
    // frame y dejaría la misma pregunta contestada en dos sitios, libres de
    // separarse.
    if !abiertos.contains(&crate::processes::KIND) {
        novedad.push((crate::processes::KIND, cifra(app.board.rows().len())));
    }
    // Errores o avisos en el registro que el lector no ha tenido delante: si
    // el panel está abierto ya los está viendo, así que la marca sobra.
    //
    // `count_at_or_above` y no `snapshot`: esto corre en cada frame, y clonar
    // el anillo entero para contar avisos eran dos mil líneas con sus dos
    // `String` cada una, diez veces por segundo.
    if !abiertos.contains(&crate::logview::KIND)
        && let Some(r) = app.log_ring.as_ref()
    {
        novedad.push((
            crate::logview::KIND,
            cifra(r.count_at_or_above(norte_config::logline::LogLevel::Warn)),
        ));
    }
    norte_frontend::panelbar::buttons(
        &app.kinds,
        norte_frontend::panelbar::PanelBarInput {
            open: &abiertos,
            focused: foco,
            attention: &novedad,
        },
    )
}

/// Las casillas pulsables de la barra de paneles.
///
/// Y las de los botones de disposición de la barra de menús (ADR 0133): son
/// la misma cosa —una casilla del cromo que corre una orden por el despacho
/// de su atajo— y así el ratón las resuelve por el mismo camino.
#[must_use]
pub fn panel_zones(app: &App, area: Rect) -> Vec<PanelZone> {
    let mut out = panel_bar_zones(app, area);
    out.extend(layout_zones(app, area));
    out.extend(zonas_de_tiras(app, area));
    out
}

/// Pinta las tiras de pestañas de los grupos de paneles (ADR 0134): la de
/// delante con el estilo de título y subrayada, las otras atenuadas.
pub(crate) fn draw_tiras_de_paneles(frame: &mut Frame<'_>, app: &App) {
    for (fila, pestanas) in crate::ui::geometry::tiras_de_paneles(app, frame.area()) {
        clear_themed(frame, fila, &app.theme);
        let spans: Vec<ratatui::text::Span<'static>> = pestanas
            .into_iter()
            .map(|p| {
                let estilo = if p.activa {
                    app.theme
                        .role(Role::Title)
                        .add_modifier(ratatui::style::Modifier::UNDERLINED)
                } else {
                    app.theme
                        .role(Role::Regular)
                        .add_modifier(ratatui::style::Modifier::DIM)
                };
                ratatui::text::Span::styled(p.texto, estilo)
            })
            .collect();
        frame.render_widget(Paragraph::new(ratatui::text::Line::from(spans)), fila);
    }
}

/// Las pestañas ESCONDIDAS de los grupos de paneles como zonas pulsables:
/// pulsar una corre la orden de su panel, que con el panel escondido lo
/// ENSEÑA (#329). La de delante no es zona — pulsarla lo cerraría.
fn zonas_de_tiras(app: &App, area: Rect) -> Vec<PanelZone> {
    if crate::mouse::overlay_open(app) || app.menu.is_some() {
        return Vec::new();
    }
    let botones = panel_buttons(app, area);
    let mut out = Vec::new();
    for (fila, pestanas) in crate::ui::geometry::tiras_de_paneles(app, area) {
        for p in pestanas.into_iter().filter(|p| !p.activa) {
            let Some(kind) = app.layout.kind_of(p.slot) else {
                continue;
            };
            // Sin botón no hay comando que la traiga delante: `layout.<kind>`
            // no existe para el panel de un plugin, y una zona que despacha
            // un comando desconocido es un clic muerto con aviso.
            let Some(command) = botones
                .iter()
                .find(|b| b.kind == kind.as_str())
                .map(|b| b.command.clone())
            else {
                continue;
            };
            out.push(PanelZone {
                row: fila.y,
                x0: p.x0,
                x1: p.x1,
                command,
            });
        }
    }
    out
}

/// Las casillas de la barra de paneles, sola.
fn panel_bar_zones(app: &App, area: Rect) -> Vec<PanelZone> {
    let Some(bar) = crate::ui::geometry::panel_bar_visible(app, area) else {
        return Vec::new();
    };
    let mut x = bar.x;
    let mut out = Vec::new();
    let botones = panel_buttons(app, area);
    // En columna, un botón por fila y el raíl entero de ancho: las mismas
    // filas que pinta `draw_panel_bar`.
    if crate::ui::geometry::barra_en_columna(app) {
        for (y, b) in (bar.y..bar.y.saturating_add(bar.height)).zip(botones) {
            out.push(PanelZone {
                row: y,
                x0: bar.x,
                x1: bar.x.saturating_add(bar.width).saturating_sub(1),
                command: b.command,
            });
        }
        return out;
    }
    let nombres = con_nombres(app, &botones, bar);
    for b in botones {
        let ancho = u16::try_from(norte_frontend::panelbar::button_cell(&b, nombres).width)
            .unwrap_or(u16::MAX);
        let fin = x.saturating_add(ancho);
        // Un botón que no cabe ENTERO no se pinta ni se puede pulsar: media
        // letra no es un botón. Mismo criterio que los títulos del menú.
        if fin > bar.x.saturating_add(bar.width) {
            break;
        }
        out.push(PanelZone {
            row: bar.y,
            x0: x,
            x1: fin.saturating_sub(1),
            command: b.command,
        });
        x = fin;
    }
    out
}

/// Pinta la barra de paneles: qué paneles hay, cómo están y con qué tecla.
pub(crate) fn draw_panel_bar(frame: &mut Frame<'_>, app: &App) {
    use norte_frontend::panelbar::PanelState;
    let Some(bar) = crate::ui::geometry::panel_bar_visible(app, frame.area()) else {
        return;
    };
    clear_themed(frame, bar, &app.theme);
    let mut spans: Vec<ratatui::text::Span<'static>> = Vec::new();
    let mut ancho = 0_u16;
    let botones = panel_buttons(app, frame.area());
    let nombres = con_nombres(app, &botones, bar);
    // En COLUMNA (spec 2026-09-21) cada botón es una línea con su celda de
    // letras —` S·`, tres de ancho— y los nombres no caben.
    let columna = crate::ui::geometry::barra_en_columna(app);
    let nombres = nombres && !columna;
    let mut lineas: Vec<ratatui::text::Line<'static>> = Vec::new();
    for (i, b) in botones.into_iter().enumerate() {
        let celda = norte_frontend::panelbar::button_cell(&b, nombres);
        let ancho_boton = u16::try_from(celda.width).unwrap_or(u16::MAX);
        if columna {
            if u16::try_from(i).unwrap_or(u16::MAX) >= bar.height {
                break;
            }
        } else if ancho.saturating_add(ancho_boton) > bar.width {
            break;
        }
        let desde = spans.len();
        // Tres estilos para tres estados. Que un panel tenga el TECLADO no es
        // lo mismo que esté abierto, y es la mitad de lo que se pregunta al
        // mirar la barra: dónde van a ir mis teclas.
        let estilo = match b.state {
            PanelState::Focused => app.theme.role(Role::Selection),
            PanelState::Open => app.theme.role(Role::Title),
            // APAGADO, no otro color: el texto base de la barra atenuado.
            //
            // Era `Role::StatusBar`, que es el estilo de la BARRA DE ESTADO —
            // en la mitad de los temas, fondo vivo y texto oscuro. Esta barra
            // se limpia con el fondo base, así que los botones CERRADOS
            // salían como bloques encendidos sobre ella y los ABIERTOS como
            // texto normal: el peso visual, exactamente al revés. Mirarla
            // contestaba lo contrario de lo que preguntas, que es lo que hace
            // que parezca que el estado va por libre.
            //
            // El menú de al lado nunca cayó en esto: usa `Title` para lo que
            // no está abierto y `Selection` para lo que sí, y jamás el rol de
            // otra superficie.
            PanelState::Closed => app
                .theme
                .role(Role::Regular)
                .add_modifier(ratatui::style::Modifier::DIM),
        };
        // La letra conserva SIEMPRE el estilo de su estado, y la marca de
        // novedad es un span aparte. Pintar el botón entero de aviso —como
        // hacía la primera versión— le quitaba al lector la respuesta a «¿a
        // dónde van a ir mis teclas?» justo mientras algo estaba pasando, que
        // es cuando más se pregunta.
        //
        // La marca va DENTRO del ancho del botón (ocupa el espacio de la
        // derecha) para que la fila no cambie de tamaño según lo que pase: una
        // barra que baila se lee peor que una fija.
        //
        // Con nombres, la letra de acceso va SUBRAYADA dentro del nombre
        // (spec 2026-09-10): tres spans —antes, la letra, después— y el
        // mismo estilo de estado en los tres.
        let subrayado = estilo.add_modifier(ratatui::style::Modifier::UNDERLINED);
        let antes: String = celda.text.chars().take(celda.letter_at).collect();
        let letra: String = celda.text.chars().skip(celda.letter_at).take(1).collect();
        let despues: String = celda.text.chars().skip(celda.letter_at + 1).collect();
        spans.push(ratatui::text::Span::styled(format!(" {antes}"), estilo));
        spans.push(ratatui::text::Span::styled(letra, subrayado));
        spans.push(ratatui::text::Span::styled(despues, estilo));
        spans.push(if b.attention > 0 {
            ratatui::text::Span::styled("·", app.theme.role(Role::Warning))
        } else {
            ratatui::text::Span::styled(" ", estilo)
        });
        if columna {
            lineas.push(ratatui::text::Line::from(spans.split_off(desde)));
        }
        ancho = ancho.saturating_add(ancho_boton);
    }
    if !columna {
        lineas.push(ratatui::text::Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lineas), bar);
}

/// Pinta la barra de menús y su desplegable.
pub(crate) fn draw_menu(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    // La BARRA se pinta con el menú abierto o cerrado: fijada, su trabajo es
    // decir que el menú existe. El desplegable, obviamente, solo abierto — y
    // por eso los títulos se miden aparte, sin pasar por `menu_geom`.
    let abierto = app.menu.as_ref();
    let titles = menu_titles(area);
    let bar = Rect { height: 1, ..area };
    clear_themed(frame, bar, &app.theme);
    // Un título que no cabe ENTERO no se pinta a medias: en cuarenta columnas
    // la barra acababa en «Bus», que no es un menú, es un ruido. Se cae el
    // último que sobra y ya está — lo que la barra tiene que decir es que HAY
    // menú, y para eso los primeros bastan.
    let spans: Vec<ratatui::text::Span<'static>> = titles
        .iter()
        .filter(|(_, _, x1)| *x1 < area.x.saturating_add(area.width))
        .enumerate()
        .map(|(i, (label, _, _))| {
            let style = if abierto.is_some_and(|st| i == st.menu()) {
                app.theme.role(Role::Selection)
            } else {
                app.theme.role(Role::Title)
            };
            ratatui::text::Span::styled(label.clone(), style)
        })
        .collect();
    frame.render_widget(Paragraph::new(ratatui::text::Line::from(spans)), bar);

    let reserva = draw_layout_buttons(frame, app, area, bar);

    // Y la tecla que lo abre, a la derecha, SACADA DEL KEYMAP VIVO.
    //
    // Una barra que enseña siete títulos y no dice cómo se entra en ellos deja
    // al lector con el ratón como única puerta. La tecla no se escribe a mano
    // —es `alt+m` en unos presets y otra cosa en los que alguien reate— así
    // que sale de donde salen las de los items del desplegable.
    //
    // Solo con el menú CERRADO: abierto, la tecla ya no hace falta y ese hueco
    // lo quiere el título más a la derecha.
    if abierto.is_none()
        && let Some(chord) = app
            .palette_rows
            .iter()
            .find(|r| r.key == "app.menu")
            .map(|r| r.chord.clone())
    {
        let texto = format!("{chord} ");
        let w = u16::try_from(UnicodeWidthStr::width(texto.as_str())).unwrap_or(0);
        let usado = titles
            .iter()
            .filter(|(_, _, x1)| *x1 < area.x.saturating_add(area.width))
            .map(|(l, _, _)| u16::try_from(UnicodeWidthStr::width(l.as_str())).unwrap_or(0))
            .sum::<u16>();
        // Solo si cabe SIN pisar los títulos: el nombre de un menú vale más
        // que su atajo, y medio atajo no vale nada.
        // A la izquierda de los botones, si los hay.
        if bar.width > usado.saturating_add(w).saturating_add(reserva) {
            let hint = Rect {
                x: bar
                    .x
                    .saturating_add(bar.width)
                    .saturating_sub(w)
                    .saturating_sub(reserva),
                width: w,
                ..bar
            };
            frame.render_widget(
                Paragraph::new(ratatui::text::Line::styled(
                    texto,
                    app.theme.role(Role::Info),
                )),
                hint,
            );
        }
    }

    let (Some(st), Some(g)) = (abierto, menu_geom(app, area)) else {
        return;
    };
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
        .lines
        .iter()
        .map(|linea| menu_line(app, linea, width, st.item()))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
    // La raya de una sección se une al borde (`├───┤`), como en mc: flotando
    // entre dos `│` se lee como un subrayado, no como una división.
    let derecha = g.drop.x.saturating_add(g.drop.width).saturating_sub(1);
    for (i, linea) in g.lines.iter().enumerate() {
        let fila = inner.y.saturating_add(u16::try_from(i).unwrap_or(u16::MAX));
        if !matches!(linea, MenuLine::Section(_)) || fila >= inner.y.saturating_add(inner.height) {
            continue;
        }
        let buf = frame.buffer_mut();
        for (x, s) in [(g.drop.x, "├"), (derecha, "┤")] {
            if let Some(c) = buf.cell_mut((x, fila)) {
                c.set_symbol(s);
            }
        }
    }
}

/// Una línea del desplegable, pintada a `width` celdas; `cursor` es la
/// orden resaltada.
fn menu_line(
    app: &App,
    linea: &MenuLine,
    width: usize,
    cursor: usize,
) -> ratatui::text::Line<'static> {
    match linea {
        MenuLine::Section(None) => {
            ratatui::text::Line::styled("─".repeat(width), app.theme.role(Role::Separator))
        }
        // El rótulo en el estilo apagado, entre rayas: se lee como cabecera
        // de grupo, no como una orden más que no hace nada.
        MenuLine::Section(Some(t)) => {
            let t = super::text::take_width(t, width.saturating_sub(4));
            let resto = width.saturating_sub(UnicodeWidthStr::width(t.as_str()) + 3);
            ratatui::text::Line::from(vec![
                ratatui::text::Span::styled("─ ", app.theme.role(Role::Separator)),
                ratatui::text::Span::styled(t, app.theme.role(Role::Muted)),
                ratatui::text::Span::styled(
                    format!(" {}", "─".repeat(resto)),
                    app.theme.role(Role::Separator),
                ),
            ])
        }
        MenuLine::Item {
            index,
            label,
            chord,
            role,
        } => {
            use norte_frontend::menu::ItemRole;
            let marca = if *role == ItemRole::Ai { AI_MARK } else { "" };
            let slot = width
                .saturating_sub(UnicodeWidthStr::width(label.as_str()))
                .saturating_sub(UnicodeWidthStr::width(marca))
                .saturating_sub(UnicodeWidthStr::width(chord.as_str()));
            let text = format!("{label}{marca}{}{chord}", " ".repeat(slot));
            // El color de peligro en lo que borra, salvo bajo el cursor: ahí
            // manda la selección, que es lo que dice DÓNDE estás.
            let style = if *index == cursor {
                app.theme.role(Role::Selection)
            } else if *role == ItemRole::Destructive {
                app.theme.role(Role::Error)
            } else {
                app.theme.role(Role::Regular)
            };
            ratatui::text::Line::styled(text, style)
        }
    }
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
pub(crate) const TAB_NEW: &str = "[+]";

/// El botón de cerrar la activa.
pub(crate) const TAB_CLOSE: &str = "[x]";

/// Los trozos de la barra, cada uno con su ancho y qué hace pulsarlo.
pub(crate) fn tab_pieces(t: &TabStrip) -> Vec<(String, TabAction)> {
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
/// [`super::geometry::pane_geometry`]: quien sabe dónde cayó cada cosa es el `draw`.
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
pub(crate) const TARGET_BADGE: &str = "->";

/// Pinta la barra de pestañas si la hay, y devuelve dónde caen la cabecera de
/// columnas y el listado.
///
/// Con pestañas, la PRIMERA fila del interior es la barra y todo lo demás baja
/// una: por eso `pane_chrome_rows` cuenta lo mismo, y el test de ancla lo
/// contrasta contra el buffer.
pub(crate) fn draw_tab_strip(
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
pub(crate) fn tab_strip_line<'a>(t: &TabStrip, theme: &TuiTheme) -> ratatui::text::Line<'a> {
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
