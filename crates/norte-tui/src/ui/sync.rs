//! La pantalla de sincronización: el reparto, el resumen, y la fila por paso con
//! su estilo de deshacer.

use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::text::wrapped_rows;
use super::HOSTILE_BADGE;
use crate::theme::TuiTheme;
use norte_i18n::t;

/// Pinta el panel de sincronización: el resumen del plan, sus pasos y la
/// pregunta que falte.
///
/// Todo lo que dice sale de [`norte_frontend::sync`] (regla dura 7): el
/// resumen, las tres marcas de cada paso, qué devuelve el undo y la segunda
/// pregunta. Aquí solo se reparte el sitio y se elige el color, y el color
/// nunca es lo único que distingue nada (§17) — las marcas son glifos ASCII.
pub(crate) fn draw_sync(frame: &mut Frame<'_>, area: Rect, view: &crate::app::SyncView, theme: &TuiTheme) {
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
pub(crate) fn sync_layout(outer: Rect, view: &crate::app::SyncView) -> (Option<Rect>, Rect, Option<Rect>) {
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
pub(crate) fn sync_summary(view: &crate::app::SyncView, theme: &TuiTheme) -> Paragraph<'static> {
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
pub(crate) fn sync_step_item(
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

/// El color de las marcas de un paso. El GLIFO ya lo distingue sin color
/// ninguno (§17); esto solo lo refuerza para quien sí lo ve.
pub(crate) fn sync_undo_style(theme: &TuiTheme, undo: norte_frontend::sync::StepUndo) -> Style {
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
pub(crate) fn sync_status_line(view: &crate::app::SyncView) -> String {
    format!(
        " {} ",
        norte_frontend::sync::status_line(view, norte_i18n::active())
    )
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
