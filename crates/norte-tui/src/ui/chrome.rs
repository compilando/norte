//! El cromo de la ventana: la barra de menú con sus zonas de clic, y la tira de
//! pestañas de cada lado.
//!
//! Las dos siguen la misma forma: una función MIDE las zonas (`menu_zones`,
//! `tab_zones`) y otra PINTA, porque quien enruta un clic necesita la geometría
//! sin haber pintado nada.

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
    /// `(label, chord)` de cada elemento del menú abierto.
    items: Vec<(String, String)>,
}

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
    let etiqueta = app.theme.role(Role::StatusBar);
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
fn kind_con_teclado(app: &App) -> Option<&'static str> {
    match app.key_owner() {
        crate::app::KeyOwner::Panes => None,
        crate::app::KeyOwner::Places => Some("places"),
        crate::app::KeyOwner::Preview => Some(crate::preview::KIND),
        crate::app::KeyOwner::Processes => Some(crate::processes::KIND),
        crate::app::KeyOwner::Tree => Some(crate::tree::KIND),
        crate::app::KeyOwner::Log => Some(crate::logview::KIND),
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
    let mut novedad: Vec<&str> = Vec::new();
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
    if !abiertos.contains(&crate::processes::KIND) && !app.board.rows().is_empty() {
        novedad.push(crate::processes::KIND);
    }
    // Errores o avisos en el registro que el lector no ha tenido delante: si
    // el panel está abierto ya los está viendo, así que la marca sobra.
    //
    // `has_at_or_above` y no `snapshot`: esto corre en cada frame, y clonar el
    // anillo entero para preguntar «¿hay algún aviso?» eran dos mil líneas con
    // sus dos `String` cada una, diez veces por segundo.
    if !abiertos.contains(&crate::logview::KIND)
        && app
            .log_ring
            .as_ref()
            .is_some_and(|r| r.has_at_or_above(norte_config::logline::LogLevel::Warn))
    {
        novedad.push(crate::logview::KIND);
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
#[must_use]
pub fn panel_zones(app: &App, area: Rect) -> Vec<PanelZone> {
    let Some(bar) = crate::ui::geometry::panel_bar_visible(app, area) else {
        return Vec::new();
    };
    let mut x = bar.x;
    let mut out = Vec::new();
    let botones = panel_buttons(app, area);
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
    for b in botones {
        let celda = norte_frontend::panelbar::button_cell(&b, nombres);
        let ancho_boton = u16::try_from(celda.width).unwrap_or(u16::MAX);
        if ancho.saturating_add(ancho_boton) > bar.width {
            break;
        }
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
        spans.push(if b.attention {
            ratatui::text::Span::styled("·", app.theme.role(Role::Warning))
        } else {
            ratatui::text::Span::styled(" ", estilo)
        });
        ancho = ancho.saturating_add(ancho_boton);
    }
    frame.render_widget(Paragraph::new(ratatui::text::Line::from(spans)), bar);
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
        if bar.width > usado.saturating_add(w) {
            let hint = Rect {
                x: bar.x.saturating_add(bar.width).saturating_sub(w),
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
