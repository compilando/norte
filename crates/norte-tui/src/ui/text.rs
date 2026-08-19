//! Recortar y medir texto para la celda en la que cabe.
//! 
//! Nada de aquí sabe de `App` ni de ratatui salvo `Span`: son las funciones que
//! deciden dónde entra la elipsis, cuántas celdas ocupa un glifo y qué badge
//! lleva delante un nombre hostil.

use super::HOSTILE_BADGE;
use ratatui::text::Span;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Prefijo de `s` que cabe en `max` CELDAS (review MN2/MN3): recorte
/// consciente de ancho — un char de doble celda jamás desborda el
/// presupuesto (el recorte por `chars()` sí lo hacía).
pub(crate) fn take_width(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw > max {
            break;
        }
        used += cw;
        out.push(c);
    }
    out
}

/// Cell width of `s`, the same budget [`take_width`] spends.
pub(crate) fn cells(s: &str) -> usize {
    s.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// Right-truncation to `max` CELLS, marking the cut with a single `…`.
///
/// The shape for a LABEL, where `middle_ellipsis` is the wrong tool: head
/// plus tail collides any two labels that agree on both ends (`Foo…bar` and
/// `Foo…bar` for two different plugin-supplied titles), while a right cut
/// keeps a distinct prefix distinct. Cell-aware, never char counts: a
/// double-width glyph that does not fit is dropped whole rather than
/// overflowing the column by one cell.
pub(crate) fn right_ellipsis(s: &str, max: usize) -> String {
    if cells(s) <= max {
        return s.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = take_width(s, max - 1);
    out.push('…');
    out
}

/// Splits a generated dialog hint into its whole `[chord] label` groups.
///
/// [`crate::hints::dialog_hints`] joins the groups with a single space and
/// every group starts with `[`, so the boundary is the ` [` join and NOT any
/// space: a label is prose and carries spaces of its own (`otro panel`).
pub(crate) fn hint_groups(hint: &str) -> Vec<&str> {
    let mut groups = Vec::new();
    let mut start = 0usize;
    for (i, _) in hint.match_indices(" [") {
        groups.push(&hint[start..i]);
        start = i + 1;
    }
    if start < hint.len() {
        groups.push(&hint[start..]);
    }
    groups
}

/// Fits a generated dialog hint into `max` CELLS by dropping whole
/// `[chord] label` groups, marking the loss with a trailing `…`.
///
/// Every other overlay measures its hint and grows its popup to fit
/// (`draw_nav_popup`, `draw_extensions`, …). The help overlay is full-screen
/// and cannot grow, so its footer has to be CUT — and a `middle_ellipsis`
/// there was actively lying twice over. At 80 columns it produced
/// `[enter] confirmar [esc]…kspace] atrás [/] filtrar`: the cut fell inside a
/// group and left the brackets balanced, so `[esc]…kspace]` reads as a chord
/// for a key called *kspace* that the app invented; and middle truncation
/// eats the MIDDLE of the list, which is exactly where `[tab] otro panel`
/// sat — the verb the whole two-pane design rests on, gone without a trace.
///
/// A group is therefore emitted WHOLE or not at all, and the `…` says that
/// something was dropped. Groups are kept in order, stopping at the first
/// that does not fit: the footer is then a true prefix of the real hint.
#[must_use]
pub fn fit_hint_groups(hint: &str, max: usize) -> String {
    if cells(hint) <= max {
        return hint.to_owned();
    }
    // Two cells held back: the `…` and the space that separates it from the
    // last group kept.
    let budget = max.saturating_sub(2);
    let mut out = String::new();
    for g in hint_groups(hint) {
        let sep = usize::from(!out.is_empty());
        if cells(&out) + sep + cells(g) > budget {
            break;
        }
        if sep == 1 {
            out.push(' ');
        }
        out.push_str(g);
    }
    if out.is_empty() {
        // Not even one group fits: say so rather than paint half a chord.
        return take_width("…", max);
    }
    out.push(' ');
    out.push('…');
    out
}

/// El texto con su badge de nombre hostil delante, si lo lleva.
pub(crate) fn with_badge(text: &str, hostile: bool) -> String {
    if hostile {
        format!("{HOSTILE_BADGE} {text}")
    } else {
        text.to_owned()
    }
}

/// Una fila de dos campos en `width` celdas: `left` a la izquierda, `right`
/// pegado a la derecha.
///
/// El campo de la derecha NUNCA se recorta, y esa es la regla que importa: es
/// un TAMAÑO, y un `38.2 GiB` recortado por la cabeza pinta `8.2 GiB`, que no
/// es una etiqueta rota sino un número FALSO. Si no cabe entero, se cae el
/// campo derecho y queda solo el nombre.
pub(crate) fn two_fields(left: &str, right: &str, width: usize, truncate: fn(&str, usize) -> String) -> String {
    /// Celdas por debajo de las cuales el nombre deja de identificar nada.
    const FLOOR: usize = 6;
    let d = norte_frontend::cells(right);
    // Aire a los dos lados del par, MÁS una celda de separación entre los dos
    // campos: sin ella un nombre que llena su sitio deja el `…` pegado al
    // número (`/home/os…1P`), que se lee como un dato y no como un recorte.
    if d + 3 + FLOOR >= width {
        return truncate(&format!(" {left}"), width);
    }
    let room = width - d - 3;
    let i = truncate(&format!(" {left}"), room);
    let slot = room.saturating_sub(norte_frontend::cells(&i)) + 1;
    format!("{i}{}{right} ", " ".repeat(slot))
}

/// Recorte por el MEDIO, para lo que se identifica por su cola: una ruta.
pub(crate) fn middle(text: &str, width: usize) -> String {
    norte_frontend::middle_ellipsis(text, width)
}

/// Recorta por la COLA a `width` celdas, marcando con `…`.
///
/// Por la cola y no por el medio ([`norte_frontend::middle_ellipsis`]) porque
/// aquí lo que identifica la fila está al principio: el nombre de un favorito
/// y el de una sección. La elipsis media existe para rutas, donde lo que
/// identifica es el final.
pub(crate) fn head(text: &str, width: usize) -> String {
    if norte_frontend::cells(text) <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for c in text.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push('…');
    out
}

/// Ancho del modal por CONTENIDO (H1 T3 follow-up): los pies GENERADOS
/// pueden superar las 60 col históricas — p. ej. colisión: `[esc] … [w] más
/// nuevo` — y truncarlos escondería teclas reales. Techo = ancho del frame
/// menos margen; suelo = las 60 históricas. MINOR-1 (H1 close): se mide en
/// Recorta una fila de spans a `max` CELDAS, cortando por la derecha y
/// respetando fronteras de carácter.
///
/// El último span que no cabe entero se corta por caracteres (jamás por
/// bytes): partir un carácter ancho por la mitad pinta media celda basura, y
/// partirlo por bytes ni siquiera es UTF-8.
pub(crate) fn clamp_spans(spans: Vec<Span<'static>>, max: usize) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut left = max;
    for sp in spans {
        if left == 0 {
            break;
        }
        let w = sp.content.width();
        if w <= left {
            left -= w;
            out.push(sp);
            continue;
        }
        let mut text = String::new();
        let mut acc = 0_usize;
        for c in sp.content.chars() {
            let cw = UnicodeWidthChar::width(c).unwrap_or(0);
            if acc + cw > left {
                break;
            }
            acc += cw;
            text.push(c);
        }
        // Cortar por carácter no basta: un ZWJ o un selector de variación
        // miden CERO, así que caben siempre y el trozo puede acabar en un
        // juntador que se compone con lo que se pinte a continuación —
        // `emoji_zwj_family` recortado a tres celdas dejaba la familia unida
        // al carácter siguiente (#246 m1). `middle_ellipsis` arregló el
        // espejo de esto drenando por delante; aquí se drena por detrás.
        while text
            .chars()
            .next_back()
            .is_some_and(|c| UnicodeWidthChar::width(c).unwrap_or(0) == 0 && !c.is_ascii())
        {
            text.pop();
        }
        if !text.is_empty() {
            out.push(Span::styled(text, sp.style));
        }
        break;
    }
    out
}

/// La COLA de `s`, con `…` delante cuando algo se quedó fuera.
///
/// Por chars y no por bytes: cortar por bytes parte un carácter multibyte, y
/// lo que se pinta son chars ya enmascarados (`display_name` no deja
/// controles ni bidi crudos, así que ninguno de los que quedan puede
/// reconfigurar la terminal al aparecer a media secuencia).
pub(crate) fn tail_window(s: &str, max: usize) -> String {
    let total = s.chars().count();
    if total <= max {
        return s.to_owned();
    }
    let tail: String = s.chars().skip(total - max.saturating_sub(1)).collect();
    format!("…{tail}")
}

/// Prefija el badge hostil FUERA de la traducción (audit MINOR-5: el
/// mecanismo del badge no puede depender de que cada locale conserve un
/// `{ $badge }` — concatenación Rust-side, translation-proof).
pub(crate) fn badge_prefixed(hostile: bool, line: String) -> String {
    if hostile {
        format!("{HOSTILE_BADGE}{line}")
    } else {
        line
    }
}

pub(crate) fn clamp_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Cuántas filas ocupa `text` envuelto a `width` columnas.
///
/// Cuenta CELDAS, no bytes ni `char`s: medir en bytes reservaría de más y en
/// `char`s de menos — y de menos es lo que corta la frase que dice que esto no
/// se puede deshacer.
pub(crate) fn wrapped_rows(text: &str, width: u16) -> u16 {
    if width == 0 {
        return 1;
    }
    let cells = u16::try_from(text.width()).unwrap_or(u16::MAX);
    let exact = cells.div_ceil(width).max(1);
    // Una fila de holgura en cuanto la frase envuelve: `Wrap` parte por
    // PALABRAS, así que `ceil(cells / width)` es una cota INFERIOR y quedarse
    // en ella recorta la última línea — que es la que dice que esto no se puede
    // deshacer. El tope de `sync_layout` acota lo que la holgura puede costar.
    if cells > width {
        exact.saturating_add(1)
    } else {
        exact
    }
}

#[cfg(test)]
mod ellipsis_tests {
    use norte_frontend::middle_ellipsis;
    use unicode_width::UnicodeWidthStr;

    /// Una cadena que ya cabe en `max` celdas vuelve intacta.
    #[test]
    fn cabe_intacta() {
        assert_eq!(middle_ellipsis("file:///d/a.txt", 46), "file:///d/a.txt");
    }

    /// ASCII que desborda: comportamiento idéntico al anterior (celdas==chars),
    /// cabeza + `…` + cola, sin exceder `max`.
    #[test]
    fn ascii_conserva_cabeza_y_cola() {
        let s = "file:///muy/larga/ruta/hacia/un/archivo/final.txt";
        let out = middle_ellipsis(s, 20);
        assert!(out.contains('…'));
        assert!(out.starts_with("file:"), "conserva el scheme (cabeza)");
        let tail = out.rsplit_once('…').expect("hay elipsis").1;
        assert!(
            !tail.is_empty() && s.ends_with(tail),
            "la cola es un sufijo REAL del original: {out:?}"
        );
        assert!(out.width() <= 20, "no excede el ancho: {out:?}");
    }

    /// CJK (cada char = 2 celdas): NUNCA excede `max` celdas y CONSERVA la
    /// cola —el bug #79 la perdía porque presupuestaba por chars—.
    #[test]
    fn cjk_no_excede_y_conserva_cola() {
        let s = "日本語".repeat(20); // 60 chars, 120 celdas
        let out = middle_ellipsis(&s, 21);
        assert!(out.width() <= 21, "ancho {} > 21 en {out:?}", out.width());
        assert!(out.contains('…'));
        assert!(out.ends_with('語'), "la cola sobrevive: {out:?}");
        assert!(out.starts_with('日'), "la cabeza sobrevive: {out:?}");
    }

    /// Emoji ancho (2 celdas): tampoco desborda.
    #[test]
    fn emoji_no_excede() {
        let s = "a😀b😀c😀d😀e😀f😀g";
        let out = middle_ellipsis(s, 9);
        assert!(out.width() <= 9, "ancho {} en {out:?}", out.width());
        assert!(out.contains('…'));
    }

    /// `max` menor que un solo char ancho: no se parte la celda → solo `…`.
    #[test]
    fn max_menor_que_un_char_ancho() {
        let out = middle_ellipsis("日本", 1);
        assert_eq!(out, "…");
        assert!(out.width() <= 1);
    }

    /// P1 encoding audit F2 (LOW): un flood de combining marks (ancho CERO
    /// cada uno) desborda el caminante por celdas SIN nunca tocar su
    /// presupuesto — el early-return de ancho, o el propio caminante,
    /// podían devolver/procesar el string ENTERO sin acotar, con `max`
    /// celdas satisfecho pero el tamaño real sin tope. `nfd_e_acute` del
    /// corpus (`e` + combining acute) es el par base+combining canónico —
    /// aquí se inunda a 100 000× para ejercer el backstop por CUENTA de
    /// chars, no solo por ancho.
    #[test]
    fn flood_de_combining_marks_no_desborda() {
        let fixture = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "nfd_e_acute")
            .expect("fixture del corpus");
        let text = String::from_utf8(fixture.bytes).expect("nfd_e_acute es UTF-8 válido");
        let (base, combining) = text.split_at(1); // "e" + "\u{0301}"
        let flood: String = std::iter::once(base)
            .chain(std::iter::repeat_n(combining, 100_000))
            .collect();
        assert_eq!(flood.width(), 1, "control: el flood entero pesa 1 celda");
        let out = middle_ellipsis(&flood, 10);
        // Cota: el backstop pre-recorta a `char_cap = 4*max` chars, pero el
        // caminante de cabeza Y el de cola operan cada uno sobre TODO ese
        // precorte (no sobre mitades separadas) — con ancho cero ninguno
        // frena por presupuesto, así que cada uno puede consumirlo entero.
        // Bounded (2*char_cap + 1), no perfecto — lo que pide F2 (LOW) es
        // dejar de ser ILIMITADO, no una cota ajustada.
        let bound = 2 * (10 * 4) + 1;
        assert!(
            out.chars().count() <= bound,
            "el backstop de cuenta de chars no acotó la salida: {} chars (cota {bound})",
            out.chars().count()
        );
    }
}

#[cfg(test)]
mod clamp_spans_tests {
    use super::clamp_spans;
    use ratatui::text::Span;
    use unicode_width::UnicodeWidthStr as _;

    fn truncate(text: &str, max: usize) -> String {
        clamp_spans(vec![Span::raw(text.to_owned())], max)
            .into_iter()
            .map(|s| s.content.into_owned())
            .collect()
    }

    /// Lo que cabe entero pasa entero, y lo que no se corta por CELDAS.
    #[test]
    fn recorta_por_celdas_y_no_por_bytes() {
        assert_eq!(truncate("abcdef", 10), "abcdef");
        assert_eq!(truncate("abcdef", 3), "abc");
        // CJK: dos celdas por carácter, así que en tres celdas cabe uno.
        assert_eq!(truncate("日本語", 3), "日");
        assert!(truncate("日本語", 3).width() <= 3);
    }

    /// Un juntador mide CERO, así que cabía siempre y el trozo acababa en él:
    /// lo que se pintara detrás se componía con la familia recortada (#246
    /// m1). `middle_ellipsis` drena por delante; esto drena por detrás.
    #[test]
    fn no_termina_en_un_juntador() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        for max in 0..=8 {
            let output = truncate(family, max);
            assert!(
                !output.ends_with('\u{200D}'),
                "a {max} celdas quedó un ZWJ al final: {output:?}"
            );
        }
    }

    /// Un `max` de cero no pinta nada, y nunca pánico.
    #[test]
    fn cero_celdas_no_pinta_nada() {
        assert_eq!(truncate("hola", 0), "");
        assert_eq!(truncate("", 5), "");
    }

    /// Los spans que caben se conservan como SPANS, con su estilo: el
    /// recorte no puede fundir en uno lo que el badge hostil separa.
    #[test]
    fn conserva_los_spans_que_caben() {
        let spans = vec![
            Span::raw("ab".to_owned()),
            Span::raw("cd".to_owned()),
            Span::raw("ef".to_owned()),
        ];
        let output = clamp_spans(spans, 5);
        assert_eq!(output.len(), 3);
        assert_eq!(output[2].content.as_ref(), "e");
    }
}
