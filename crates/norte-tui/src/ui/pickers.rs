//! Los cuatro selectores modales —tema, columnas, conexiones y reparto— y la
//! vista previa del reparto.

use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use super::{centered, clear_themed};
use crate::theme::TuiTheme;
use norte_i18n::{t, ta};

pub(crate) fn draw_theme_picker(
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
pub(crate) fn draw_columns_picker(
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
pub(crate) const LAYOUT_PREVIEW_W: u16 = 30;

/// Alto de esa misma vista previa. La proporción importa más que el tamaño —
/// una vista previa cuadrada haría pasar por alto un `simple` por un
/// `orthodox`.
pub(crate) const LAYOUT_PREVIEW_H: u16 = 10;

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
pub(crate) fn draw_connections_picker(
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

pub(crate) fn draw_layout_picker(
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
pub(crate) fn draw_layout_preview(
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
