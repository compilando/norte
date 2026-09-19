//! La pantalla de ayuda: su reparto en barra lateral + cuerpo, el scrollbar que
//! comparten los dos, y el pie que resume los atajos vivos.
//!
//! El ancho de la barra lateral depende del IDIOMA (`help_sidebar_desired`),
//! porque las etiquetas traducidas no miden lo mismo.

use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState,
};

use super::text::{cells, fit_hint_groups, right_ellipsis};
use super::{centered, clear_themed};
use crate::app::display_name;
use crate::theme::TuiTheme;
use norte_frontend::middle_ellipsis;
use norte_i18n::t;

/// Lower bound in CELLS of the help sidebar: the width it used to have,
/// unconditionally. Kept as a FLOOR so an 80-column frame never gets a
/// narrower list of topics than it had before the sidebar was sized to its
/// content.
pub(crate) const HELP_SIDEBAR_MIN: u16 = 24;

/// Upper bound of the sidebar, as a percentage of the FRAME's width. The
/// sidebar is a table of contents: past roughly a third of the screen it is
/// taking width from the prose it exists to point at.
pub(crate) const HELP_SIDEBAR_PCT: u16 = 35;

/// Cells between the sidebar and the body. Without it a title that fills the
/// sidebar sits against the first letter of the prose and the two columns
/// read as one broken line.
pub(crate) const HELP_GUTTER: u16 = 2;

/// Ancho de una barra de scroll: una celda.
///
/// La ayuda es la única pantalla con DOS listas que se desplazan a la vez —el
/// índice y la página— y hasta ahora ninguna de las dos decía por dónde iba ni
/// cuánto le quedaba. El indicador `N/M` del pie habla solo del cuerpo, y solo
/// cuando no cabe.
pub(crate) const HELP_SCROLLBAR: u16 = 1;

/// Cells of air between the prose and the body's scrollbar. Without it a line
/// wrapped to the full width ends against the bar (`bajo el cursor║`) and the
/// last letter reads as part of it.
pub(crate) const HELP_BODY_PAD: u16 = 1;

/// Typographic measure of the body in CELLS. Prose is read at 60–72 cells; at
/// 90 the eye loses the line on the return sweep, and the surplus is exactly
/// what the sidebar needs to stop truncating its titles.
pub(crate) const HELP_MEASURE: u16 = 72;

/// Cells the body keeps whatever the sidebar asks for. Only bites on frames
/// too narrow for the overlay to be useful at all, and only to keep the body
/// from being laid out at zero width.
pub(crate) const HELP_BODY_MIN: u16 = 20;

/// Cells a topic row is indented by in the sidebar, so that a title never
/// lines up with the group header above it.
pub(crate) const HELP_ROW_INDENT: usize = 2;

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
pub(crate) fn help_sidebar_desired(lang: norte_help::Lang) -> u16 {
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
/// enteras (`help_sidebar_desired`); llega como parámetro para que esto siga
/// siendo una función de números, medible a cualquier tamaño sin corpus.
#[must_use]
pub fn help_layout(base: Rect, sidebar_desired: u16) -> (Rect, Rect, Rect, Rect) {
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
        usize::from(body.width.saturating_sub(HELP_SCROLLBAR + HELP_BODY_PAD)),
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
/// draw no vuelve a filtrarlas, igual que `draw_palette` con sus filas. La
/// ÚNICA entrada libre es el filtro tecleado por el usuario, que pasa por el
/// mismo doble filtro que la barra de quick search (`filter_display` — jamás
/// `filter_raw` — más [`display_name`]).
pub fn draw_help(
    frame: &mut Frame<'_>,
    help: &crate::app::HelpView,
    theme: &TuiTheme,
    hint: &str,
    version_line: &str,
) {
    use norte_frontend::help::Focus;

    let (area, sidebar, body_area, footer_area) =
        help_layout(frame.area(), help_sidebar_desired(help.state.lang()));
    clear_themed(frame, area, theme);
    let mut marco = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("help-title")))
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(Role::ModalBorder));
    // Qué binario es este, arriba a la derecha: versión y revisión del árbol.
    // No es texto de interfaz sino un identificador, así que no pasa por
    // Fluent; y vacío no se pinta (los tests construyen `App` así).
    if !version_line.is_empty() {
        marco = marco.title_top(Line::raw(format!(" {version_line} ")).right_aligned());
    }
    frame.render_widget(marco, area);

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
    let (filas, painted) = lateral_pintada(state.rows());
    let items: Vec<ListItem<'_>> = filas
        .into_iter()
        .map(|fila| match fila {
            FilaLateral::Blanco => ListItem::new(Line::default()),
            // El modelo entrega TAGS, no texto: la traducción es cosa del
            // frontend (una misma fila se llama distinto en la TUI y en la
            // GUI). Un tag sin entrada Fluent pintaría su propia clave, que
            // es lo que la suite de i18n impide.
            FilaLateral::Grupo(tag) => ListItem::new(Line::styled(
                right_ellipsis(&t(&format!("help-group-{tag}")), side_w),
                theme.role(Role::Title),
            )),
            // El título de la entrada sintética `keys` es la etiqueta que
            // `HelpView::new` le dio al modelo (`help-topic-keys`), así que
            // aquí no hay caso especial: la lateral pinta lo mismo que el
            // filtro busca.
            FilaLateral::Tema(title) => ListItem::new(Line::raw(right_ellipsis(
                &format!("{}{title}", " ".repeat(HELP_ROW_INDENT)),
                side_w,
            ))),
        })
        .collect();
    // Cuántas filas tiene el índice PINTADO (con sus separadores): es el
    // total contra el que se dimensiona su barra, y hay que leerlo antes de
    // que el widget se lleve la lista.
    let rows_index = items.len();
    let mut list_state = ListState::default();
    // `HelpState` garantiza que el cursor se apoya SIEMPRE en una fila
    // seleccionable (nunca en una cabecera); con el filtro sin resultados no
    // hay fila alguna que resaltar.
    list_state.select(painted.get(state.cursor()).copied());
    // Cuál de las dos mitades recibe las teclas, dicho como lo dicen los dos
    // paneles del listado (spec 2026-09-10): el cursor de la que NO tiene el
    // foco se queda apagado. Antes las dos resaltaban igual de vivas y la
    // pantalla no decía a dónde iban las flechas.
    //
    // Es el rol y no un borde porque la ayuda es UN marco: partirlo en dos
    // se comería una columna de los títulos, que es la que hace que un
    // índice se lea. Y sin tema el apagado sigue viéndose, porque el
    // `fallback` de `SelectionUnfocused` es `reverse().dim()`.
    let (rol_indice, rol_cuerpo) = if state.focus() == Focus::Topics {
        (Role::Selection, Role::SelectionUnfocused)
    } else {
        (Role::SelectionUnfocused, Role::Selection)
    };
    frame.render_stateful_widget(
        List::new(items).highlight_style(theme.role(rol_indice)),
        sidebar,
        &mut list_state,
    );

    let (lines, action_lines) = help.body();
    // La línea de la acción bajo el cursor del cuerpo. Se resalta SIEMPRE que
    // exista, con el foco puesto aquí o no — lo que cambia es el rol. Antes
    // desaparecía al irse el foco, y entonces la mitad sin foco no era «un
    // cursor apagado» sino «ningún cursor»: volver con Tab no decía a qué
    // línea volvías.
    let focused = action_lines.get(state.action_cursor()).copied();
    let body: Vec<Line<'_>> = lines
        .iter()
        .enumerate()
        .skip(state.body_scroll())
        .take(usize::from(body_area.height))
        .map(|(i, line)| {
            if Some(i) == focused {
                line.clone().style(theme.role(rol_cuerpo))
            } else {
                line.clone()
            }
        })
        .collect();
    // La última columna del cuerpo es su barra: la prosa ya viene envuelta a
    // una celda menos (`help_body_size`), así que aquí solo se reparte.
    let (text_area, body_bar) = split_body(body_area);
    frame.render_widget(Paragraph::new(body), text_area);
    // Las DOS columnas dicen por dónde van. Hasta ahora ninguna lo decía: el
    // `N/M` del pie habla solo del cuerpo y solo cuando no cabe, así que en el
    // índice no había NADA que dijera que quedaban filas debajo.
    render_scrollbar(
        frame,
        body_bar,
        theme,
        lines.len(),
        state.body_scroll(),
        usize::from(body_area.height),
    );
    render_scrollbar(
        frame,
        sidebar_scrollbar,
        theme,
        rows_index,
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
pub(crate) fn draw_help_footer(
    frame: &mut Frame<'_>,
    footer_area: Rect,
    theme: &TuiTheme,
    state: &norte_frontend::help::HelpState,
    hint: &str,
    total: usize,
    body_height: u16,
) {
    // Dónde está el lector dentro de la página, con el MISMO idioma que el
    // visor (`{row}/{total}`, `draw_viewer`). Solo cuando la página NO cabe:
    // un `1/9` sobre nueve líneas visibles es ruido. Importa más aquí que en
    // el visor porque las filas ejecutables — la columna de chords y el
    // `Enter` para el que existe este overlay — se pintan DETRÁS de toda la
    // prosa, así que en una página larga no se ven en el primer render y sin
    // esto nada dice que estén ahí.
    //
    // En PORCENTAJE LEÍDO, hasta el pie de la ventana, y no en líneas: un
    // `11/663` no le dice a nadie cuánto queda, porque 663 no es un número
    // que el lector tenga en la cabeza. «17 %» sí; «100 %» es que ya ha visto
    // el final. Lo mismo que enseña `less`.
    let height = usize::from(body_height);
    let pos = (total > height).then(|| {
        let leido = state.body_scroll().saturating_add(height).min(total);
        format!(" {} % ", leido.saturating_mul(100) / total.max(1))
    });
    let pos = pos.unwrap_or_default();
    // El indicador se lleva su trozo del pie ANTES de recortar el hint: a la
    // derecha jamás le disputa el borde izquierdo al hint, y el hint jamás se
    // le come a él (`fit_hint_groups` tira grupos enteros, no celdas sueltas).
    let width = usize::from(footer_area.width);
    let left_max = width.saturating_sub(cells(&pos));
    let left = if state.filtering() {
        let (query, _) = display_name(state.filter_display().as_bytes());
        middle_ellipsis(&format!(" /{query}"), left_max)
    } else {
        // NUNCA `middle_ellipsis` sobre un hint generado: ver
        // [`fit_hint_groups`]. Una celda del pie es del margen izquierdo.
        format!(" {}", fit_hint_groups(hint, left_max.saturating_sub(1)))
    };
    let slot = width.saturating_sub(cells(&left) + cells(&pos));
    let footer = Line::from(vec![
        Span::raw(left),
        Span::raw(" ".repeat(slot)),
        Span::raw(pos),
    ]);
    frame.render_widget(
        Paragraph::new(footer).style(theme.role(Role::BorderUnfocused)),
        footer_area,
    );
}

/// Una fila PINTADA de la lateral de la ayuda.
enum FilaLateral<'a> {
    /// El aire entre dos grupos.
    Blanco,
    /// Una cabecera de grupo, por su tag.
    Grupo(&'a str),
    /// Una página, por su título.
    Tema(&'a str),
}

/// Lo que pinta la lateral y, para cada fila del MODELO, en qué fila pintada
/// cae.
///
/// Las filas pintadas NO son las del modelo: entre grupo y grupo va una línea
/// en blanco. Va aquí y no en `HelpState::rows`, que es la lista NAVEGABLE —
/// sus índices son los que direcciona `cursor()`, y meter separadores ahí
/// rompería el cursor y de paso la GUI hermana. Por eso se lleva el mapa
/// fila→ítem: traduce el cursor del modelo al índice del widget, y un clic de
/// vuelta. Lo usan el pintor y [`help_zones`]: lo pulsable sale de lo pintado.
fn lateral_pintada(
    rows: &[norte_frontend::help::SidebarRow],
) -> (Vec<FilaLateral<'_>>, Vec<usize>) {
    use norte_frontend::help::SidebarRow;
    let mut filas: Vec<FilaLateral<'_>> = Vec::with_capacity(rows.len() + 4);
    let mut painted: Vec<usize> = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        match row {
            SidebarRow::Group { tag } => {
                if keys_only_group(rows, i) {
                    // Una cabecera que se llama igual que su única entrada no
                    // informa de nada y cuesta una fila. Se apunta el ítem que
                    // vendrá — el cursor jamás se apoya en una cabecera, así
                    // que el mapa solo tiene que quedar bien formado.
                    painted.push(filas.len());
                    continue;
                }
                // Aire entre grupos, menos antes del primero: un blanco
                // arriba del todo se lee como una lateral descuadrada.
                if !filas.is_empty() {
                    filas.push(FilaLateral::Blanco);
                }
                painted.push(filas.len());
                filas.push(FilaLateral::Grupo(tag));
            }
            SidebarRow::Topic { title, .. } => {
                painted.push(filas.len());
                filas.push(FilaLateral::Tema(title));
            }
        }
    }
    (filas, painted)
}

/// Dónde cae cada cosa de la ayuda en el frame pintado, para el ratón.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HelpZones {
    /// La lateral de temas.
    pub sidebar: Rect,
    /// El cuerpo, con su barra de scroll.
    pub body: Rect,
    /// Cada página VISIBLE de la lateral: `(fila de pantalla, fila del modelo)`.
    /// Las cabeceras y el aire no están: no se pulsan.
    pub rows: Vec<(u16, usize)>,
}

/// Las zonas de la ayuda en el frame de `area`, o nada si no está abierta.
///
/// La ayuda de la TUI nació muda al ratón —con ella abierta, la rueda y los
/// clics se tiraban enteros—, y la forma de que no vuelva a pasar es la de
/// [`super::extension_zones`]: las zonas salen del MISMO reparto que pinta
/// ([`help_layout`] y el de las filas de la lateral, `lateral_pintada`).
///
/// El desplazamiento de la lista es el que aplica `ratatui` a un `ListState`
/// recién hecho, que es lo que usa el pintor: cero mientras el cursor quepa, y
/// si no, el que deja el cursor en la última fila.
#[must_use]
pub fn help_zones(app: &crate::app::App, area: Rect) -> Option<HelpZones> {
    let help = app.help.as_ref()?;
    let (_, sidebar, body, _) = help_layout(area, help_sidebar_desired(help.state.lang()));
    let rows = help.state.rows();
    let (_, painted) = lateral_pintada(rows);
    let alto = usize::from(sidebar.height);
    let offset = painted
        .get(help.state.cursor())
        .map_or(0, |&sel| (sel + 1).saturating_sub(alto));
    let visibles = painted
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            matches!(
                rows.get(*i),
                Some(norte_frontend::help::SidebarRow::Topic { .. })
            )
        })
        .filter_map(|(i, &item)| {
            let fila = item.checked_sub(offset)?;
            let fila = u16::try_from(fila).ok().filter(|f| *f < sidebar.height)?;
            Some((sidebar.y.saturating_add(fila), i))
        })
        .collect();
    Some(HelpZones {
        sidebar,
        body,
        rows: visibles,
    })
}

/// El cuerpo de la ayuda en (texto, barra): [`split_scrollbar`] y, además,
/// el margen entre la prosa y la barra ([`HELP_BODY_PAD`]). Sale del área de
/// texto, no de la barra: la prosa ya viene envuelta a ese ancho
/// (`help_body_size`).
fn split_body(area: Rect) -> (Rect, Rect) {
    let (text, bar) = split_scrollbar(area);
    let text = Rect {
        width: text.width.saturating_sub(HELP_BODY_PAD),
        ..text
    };
    (text, bar)
}

/// Parte un área en (contenido, barra de scroll): la ÚLTIMA columna es la
/// barra. Con menos de dos celdas no hay barra que pintar y se devuelve el
/// área entera — una barra que se come el texto es peor que no tenerla.
pub(crate) fn split_scrollbar(area: Rect) -> (Rect, Rect) {
    if area.width < 2 {
        return (area, Rect::new(area.x, area.y, 0, area.height));
    }
    let text = Rect {
        width: area.width - HELP_SCROLLBAR,
        ..area
    };
    let bar = Rect {
        x: area.x + area.width - HELP_SCROLLBAR,
        width: HELP_SCROLLBAR,
        ..area
    };
    (text, bar)
}

/// Pinta una barra de scroll vertical en `area` para un contenido de `total`
/// filas del que se ven `visible` desde `offset`.
///
/// No pinta nada cuando cabe todo: una barra llena de arriba abajo no informa
/// de nada y encima invita a arrastrarla.
pub(crate) fn render_scrollbar(
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
    let mut state = ScrollbarState::new(total.saturating_sub(visible)).position(offset);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .style(theme.role(Role::BorderUnfocused)),
        area,
        &mut state,
    );
}

/// Pinta una barra de scroll HORIZONTAL en `area` para un contenido de `total`
/// columnas del que se ven `visible` desde `offset`.
///
/// Gemela de [`render_scrollbar`] y con la misma regla: nada cuando cabe todo.
/// Existe porque el visor no envuelve —una línea puede seguir a la derecha— y
/// sin una barra que lo diga, un fichero recortado se lee como un fichero
/// corto.
pub(crate) fn render_hscrollbar(
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
    let mut state = ScrollbarState::new(total.saturating_sub(visible)).position(offset);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::HorizontalBottom)
            .begin_symbol(None)
            .end_symbol(None)
            .style(theme.role(Role::BorderUnfocused)),
        area,
        &mut state,
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
pub(crate) fn keys_only_group(rows: &[norte_frontend::help::SidebarRow], header: usize) -> bool {
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

#[cfg(test)]
mod help_footer_tests {
    use crate::app::ALLOW_HELP;
    use crate::hints::{dialog_hints, without_navigation};
    use crate::keymap::{COMMANDS, DIALOG_COMMANDS, Effective, Screen, presets};
    use crate::ui::text::{cells, fit_hint_groups, hint_groups};

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
        let groups = hint_groups(&hint);
        assert!(
            groups.len() > 2,
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
            let body = out
                .strip_suffix('…')
                .map_or(out.as_str(), str::trim_end)
                .to_owned();
            for (i, _) in body.match_indices('[') {
                assert!(
                    groups.iter().any(|g| body[i..].starts_with(g)),
                    "max={max}: un `[` que no abre un grupo entero: {out:?}"
                );
            }
            if body != hint {
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
            let body = out.strip_suffix('…').map_or(out.as_str(), str::trim_end);
            assert!(
                hint.starts_with(body),
                "max={max}: {body:?} no es prefijo de {hint:?}"
            );
        }
    }
}
