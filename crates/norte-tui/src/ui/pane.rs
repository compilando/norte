//! Pintar un pane: la fila por entrada, la cabecera de columnas y el reparto de
//! anchos que las dos comparten.
//! 
//! `entry_item` es la función más caliente del render — se llama una vez por fila
//! visible y por frame — y por eso recibe todo por parámetro en vez de mirar
//! `App`: agrupar sus argumentos en una struct de un solo uso solo movería la
//! lista de sitio.

use norte_proto::EntryKind;
use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::text::{
    clamp_spans, take_width,
    };
use super::{HOSTILE_BADGE, TARGET_BADGE, TabStrip, draw_tab_strip};
use crate::app::{Pane, display_name};
use crate::theme::TuiTheme;
use norte_frontend::middle_ellipsis;
use norte_i18n::t;

/// Cuántos items pinta un pane y cuál va resaltado, EN COORDENADAS DE LO
/// PINTADO (posición dentro del filtro cuando hay quick search en modo
/// filtro, índice absoluto si no). Lo comparten `draw_pane` y
/// [`pane_geometry`] para que el scroll salga del mismo cálculo.
pub(crate) fn painted_len_and_selection(pane: &Pane) -> (usize, Option<usize>) {
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

/// La línea de cabecera (#108 L5): etiquetas Fluent (o la `header` custom
/// del spec, #108 7b — YA saneada y capada al resolver, aquí solo el
/// recorte por ancho), la del orden activo con `▲`/`▼`. Ancho fiel al de
/// las celdas de las filas; el `align` del estilo elige el lado del
/// relleno en las no-nombre, en paso con sus celdas.
pub(crate) fn column_header_line(
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

/// Columnas VIVAS de un pane con su estilo resuelto (#108 7b, #117 sobre
/// `ColumnId`): los anchos del layout compartido más `style_for_id`, UNA
/// vez por columna y por frame (`style_for_id` pliega mapas y clona el
/// header — por fila × columna sería O(filas × columnas) de lookups
/// idénticos). El catálogo viene del cache por scheme de `App` (#117
/// tarea 2): refina los defaults de las columnas attr (hint); `None` =
/// aún no llegó o falló — defaults Opaque, jamás bloquea el render.
pub(crate) fn styled_columns(
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

#[allow(
    clippy::too_many_arguments,
    reason = "pintar un pane necesita su área, su modelo, el foco, el tema, el               reloj del frame, las columnas, el catálogo de atributos y sus               pestañas; agruparlos en una struct de un solo uso solo movería               la lista de sitio"
)]
pub(crate) fn draw_pane(
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
pub(crate) fn entry_item<'a>(
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
