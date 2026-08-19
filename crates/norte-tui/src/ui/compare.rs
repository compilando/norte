//! La pantalla de comparación de dos panes: su reparto, el título con las dos
//! mitades, la cabecera de caras y el estilo por veredicto.

use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::HOSTILE_BADGE;
use crate::theme::TuiTheme;
use norte_i18n::t;

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
pub fn draw_compare<S: std::hash::BuildHasher>(
    frame: &mut Frame<'_>,
    area: Rect,
    view: &crate::app::CompareView,
    theme: &TuiTheme,
    size_hints: &std::collections::HashMap<norte_proto::VPath, u64, S>,
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
pub(crate) const COMPARE_MARKS_W: u16 = 5;

/// El separador ESTRUCTURAL del título del panel de comparación (#185): va
/// en su propio `Span`, con su propio rol, para que un `↔` incrustado en un
/// nombre de raíz (fixture `arrow_join_spoof`) no se pueda confundir con él.
pub(crate) const COMPARE_TITLE_SEP: &str = " ↔ ";

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
pub(crate) fn compare_title(
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
pub(crate) struct CompareTitleHalf {
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
pub(crate) fn compare_title_halves(
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
    let fixed = 2
        + prefix_w
        + COMPARE_TITLE_SEP.width()
        + 1
        + badge_w(left_hostile)
        + badge_w(right_hostile);
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

/// Reparte el interior del marco: cabecera de columnas, lista, filtros y
/// teclas.
///
/// Con el marco tan corto que no caben las tres filas de cromo, la LISTA se
/// las queda todas: un panel sin filas no explica nada, y las teclas ya están
/// en la ayuda.
pub(crate) fn compare_layout(outer: Rect) -> (Option<Rect>, Rect, Option<Rect>, Option<Rect>) {
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
pub(crate) fn compare_header(face_w: usize, theme: &TuiTheme) -> Paragraph<'static> {
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
pub(crate) fn compare_face_span(
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
pub(crate) fn compare_status_line(view: &crate::app::CompareView) -> String {
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
pub(crate) fn compare_filter_spans(
    view: &crate::app::CompareView,
    theme: &TuiTheme,
) -> Vec<Span<'static>> {
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
pub(crate) fn compare_mark_style(
    theme: &TuiTheme,
    verdict: norte_proto::methods::CompareVerdict,
) -> Style {
    use norte_proto::methods::CompareVerdict as V;
    match verdict {
        V::Same => theme.role(Role::Regular),
        V::Different | V::OnlyLeft | V::OnlyRight => theme.role(Role::Warning),
        V::TypeMismatch | V::Ambiguous | V::Error => theme.role(Role::Error),
        _ => theme.role(Role::Info),
    }
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
