//! Saneado de nombres para pintar en cualquier frontend: un nombre es bytes
//! (spec §6) y el texto que se pinta es SIEMPRE lossy y MARCADO — jamás
//! pérdida silenciosa, jamás controles/bidi crudos.

use norte_proto::VPath;
use unicode_width::UnicodeWidthChar;

/// ¿Debe enmascararse en un terminal? DELEGA en
/// [`norte_encoding::is_terminal_hazard`] (fuente ÚNICA del set — antes vivía
/// atrapado aquí; ahora lo comparte con el saneo de preview de `fs.search`).
/// Cubre Cc (controles: `\n`, ESC — ratatui los BORRA en silencio y un
/// frontend directo los ejecutaría), los overrides bidi Cf (spoofing RTL del
/// orden visual) y los INVISIBLES Cf/Zl/Zp (encoding-auditor H4 de M3-3b: dos
/// nombres visualmente idénticos que difieren en bytes engañan a un humano que
/// aprueba "el que ya vio"). Los invisibles se deciden por la propiedad
/// Unicode `Default_Ignorable_Code_Point` más los que se pintan en blanco sin
/// serlo (BRAILLE BLANK, las anotaciones interlineales, Zl/Zp) — antes era una
/// lista escrita a mano que se dejaba fuera los rellenos Hangul, que ni
/// siquiera son Cf (#125). ZWJ (U+200D) y los selectores de variación se
/// PERMITEN a sabiendas: enmascararlos rompería los emoji compuestos legítimos
/// (fixture `emoji_zwj_family`) — fidelidad de emoji > el residual de un twin
/// invisible.
fn must_mask(c: char) -> bool {
    norte_encoding::is_terminal_hazard(c)
}

/// Nombre listo para pintar: `(texto, hostil)`. `hostil = true` cuando el
/// texto pintado DIFIERE del nombre real: bytes no-UTF8 (lossy `�`),
/// controles o bidi enmascarados a `�` (spec §6: display siempre lossy y
/// MARCADO — jamás pérdida silenciosa, jamás controles crudos).
#[must_use]
pub fn display_name(bytes: &[u8]) -> (String, bool) {
    let (raw, lossy) = match std::str::from_utf8(bytes) {
        Ok(s) => (std::borrow::Cow::Borrowed(s), false),
        Err(_) => (String::from_utf8_lossy(bytes), true),
    };
    let mut masked = false;
    let texto: String = raw
        .chars()
        .map(|c| {
            if must_mask(c) {
                masked = true;
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect();
    (texto, lossy || masked)
}

/// [`display_name`] con REINTERPRETACIÓN opcional (#57, spec §6.1): con
/// `Some(enc)`, un nombre NO-UTF8 se decodifica con `enc` para display en
/// vez de al lossy `�` — los bytes jamás se mutan (regla 1) y el flag
/// hostil queda en `true` (el texto pintado DIFIERE del nombre real: es
/// una VISTA elegida por el usuario, el badge lo delata igual).
///
/// Un nombre UTF-8 VÁLIDO no se reinterpreta nunca: ya es texto — en un
/// contenedor mixto (entradas UTF-8 + entradas cp866) reinterpretar las
/// UTF-8 fabricaría mojibake donde no había problema. El enmascarado de
/// hazards aplica igual en ambos caminos.
///
/// ```
/// use norte_encoding::NameEncoding;
/// use norte_frontend::display_name_with;
/// // No-UTF8 en cp437: decodifica Y marca (el texto no son los bytes).
/// let (texto, hostil) = display_name_with(b"CAF\x90.TXT", Some(NameEncoding::Cp437));
/// assert_eq!((texto.as_str(), hostil), ("CAFÉ.TXT", true));
/// // UTF-8 válido: JAMÁS se reinterpreta (contenedor mixto sin mojibake).
/// let (texto, hostil) = display_name_with("año.txt".as_bytes(), Some(NameEncoding::Cp437));
/// assert_eq!((texto.as_str(), hostil), ("año.txt", false));
/// ```
#[must_use]
pub fn display_name_with(
    bytes: &[u8],
    reinterpret: Option<norte_encoding::NameEncoding>,
) -> (String, bool) {
    let (Some(enc), Err(_)) = (reinterpret, std::str::from_utf8(bytes)) else {
        return display_name(bytes);
    };
    let decoded = norte_encoding::decode_name(bytes, enc);
    let texto: String = decoded
        .chars()
        .map(|c| if must_mask(c) { '\u{FFFD}' } else { c })
        .collect();
    (texto, true)
}

/// Path completo listo para pintar: prefijo `⟨scheme authority⟩/` (formato
/// calcado EXACTO de `VPath::display_lossy`, proto vpath.rs — con segmentos
/// limpios ambos textos coinciden) + cada segmento por [`display_name`], y
/// marca si CUALQUIER segmento saldría alterado.
///
/// El texto se construye segmento a segmento con `display_name` (no con
/// `display_lossy`, review encoding MEDIA-2): el criterio de enmascarado del
/// TEXTO es el MISMO que el del flag — `display_lossy` solo tapa Cc+bidi y
/// dejaba ZWSP/TAG crudos (twins invisibles idénticos, ambos con badge).
/// Nota ZWNJ: proto lo PERMITE en `display_lossy` (legítimo en persa);
/// `must_mask` lo enmascara — aquí gana `must_mask` a sabiendas: en la TUI
/// un twin invisible en una superficie de decisión pesa más que la
/// fidelidad tipográfica (el badge ya delata la alteración).
#[must_use]
pub fn path_display(p: &VPath) -> (String, bool) {
    path_display_with(p, None)
}

/// [`path_display`] con reinterpretación opcional (#98/F2): cada segmento
/// pasa por [`display_name_with`] — las superficies de DECISIÓN (modales de
/// confirmar/colisión, título del viewer, dir de la barra) muestran el mismo
/// texto por el que el usuario navega, no el lossy crudo. Mismo contrato de
/// badge: cualquier segmento alterado (incluida la reinterpretación) marca.
///
/// ```
/// use norte_encoding::NameEncoding;
/// use norte_frontend::path_display_with;
/// let p = norte_proto::VPath::parse("mem:///CAF%90.TXT").unwrap();
/// let (texto, hostil) = path_display_with(&p, Some(NameEncoding::Cp437));
/// assert_eq!((texto.as_str(), hostil), ("⟨mem⟩/CAFÉ.TXT", true));
/// ```
#[must_use]
pub fn path_display_with(
    p: &VPath,
    reinterpret: Option<norte_encoding::NameEncoding>,
) -> (String, bool) {
    let mut out = String::from("⟨");
    out.push_str(p.scheme());
    if let Some(a) = p.authority() {
        out.push(' ');
        out.push_str(a);
    }
    out.push_str("⟩/");
    let mut hostil = false;
    let mut first = true;
    for seg in p.segments() {
        if !first {
            out.push('/');
        }
        first = false;
        let (texto, h) = display_name_with(seg, reinterpret);
        hostil |= h;
        out.push_str(&texto);
    }
    (out, hostil)
}

/// Ancho de DISPLAY de `s` en celdas de terminal.
///
/// La misma medida contra la que presupuesta [`middle_ellipsis`], expuesta al
/// crate para que quien reserve sitio a un campo POSTERIOR lo mida igual que lo
/// mide el truncador. Un char sin ancho asignado (code point no asignado) pesa
/// 0, que es lo que hace también el caminante del truncador.
#[must_use]
pub fn cells(s: &str) -> usize {
    s.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

/// Elipsis MEDIA a `max` CELDAS de terminal: conserva cabeza (scheme) y cola
/// (nombre) —lo que identifica la ruta ante un humano— y marca el recorte con
/// `…`. Presupuesta por ANCHO DE CELDA (CJK/emoji ocupan 2 columnas), no por
/// chars: contar chars desbordaba `max` con nombres densos y ratatui
/// re-truncaba por la DERECHA, comiéndose justo la cola que la elipsis media
/// existe para preservar (#79). Para ASCII (celdas == chars) el resultado es
/// idéntico al anterior.
///
/// COMPARTIDA por ambos frontends (encoding audit M4-IA-2 H1): vivía privada
/// en la TUI, pero el motivo por el que existe no es cosmético ni propio de un
/// terminal — es que un path kilométrico JAMÁS expulse de la caja el campo que
/// va DESPUÉS de él (el score de un hit semántico, el `→ destino` de un plan).
/// La GUI se apoyaba en el `.truncate()` del div, que recorta por la derecha
/// en silencio: un path largo con un `· 0.99` incrustado (chars imprimibles,
/// sin badge) dejaba visible SOLO el score falso. En GPUI el presupuesto por
/// celdas no mide píxeles, pero es una cota CONSERVADORA (un char ancho cuenta
/// 2) y suficiente para reservar sitio al campo de cola.
///
/// P1 encoding audit F2 (LOW): backstop por CUENTA DE CHARS antes del
/// caminante por ancho. Un combining mark (`U+0301`…) o un ZWJ pesa CERO
/// celdas — un flood de millones de ellos pegados a un solo char visible
/// tiene ancho total ≤ `max` (el early-return de abajo lo devolvería
/// INTACTO, sin cortar nada) o, si desborda por el char visible, el
/// caminante de cabeza/cola seguiría acumulando chars de ancho 0 sin nunca
/// tocar su presupuesto — en ningún caso el tamaño del STRING (memoria,
/// trabajo de `display_name`/render aguas arriba) queda acotado por `max`
/// aunque el ANCHO sí. Si `s` trae más de `4*max` chars, se pre-recorta por
/// CHARS (generoso: bastante mayor que cualquier `max` de celdas real de la
/// TUI hoy) a cabeza+cola ANTES de medir nada — el resto de la función seguía
/// igual sobre esa entrada ya acotada.
///
/// ```
/// use norte_frontend::middle_ellipsis;
/// // Lo que ya cabe vuelve INTACTO.
/// assert_eq!(middle_ellipsis("file:///d/a.txt", 46), "file:///d/a.txt");
/// // Lo que desborda conserva cabeza y cola, y MARCA el recorte.
/// let out = middle_ellipsis("file:///muy/larga/ruta/hacia/final.txt", 20);
/// assert!(out.starts_with("file:") && out.ends_with(".txt") && out.contains('…'));
/// ```
#[must_use]
pub fn middle_ellipsis(s: &str, max: usize) -> String {
    if max == 0 {
        // review #108-5 M2: con presupuesto 0 devolvía "…" (ancho 1 > 0) y
        // rompía por una celda el invariante del caller.
        return String::new();
    }
    let char_cap = max.saturating_mul(4);
    let chars: Vec<char> = s.chars().collect();
    // `true` si el backstop tuvo que descartar chars por CUENTA (no por
    // ancho) — en ese caso se FUERZA la elipsis más abajo aunque el ancho
    // resultante quepa en `max`: spec §6, jamás pérdida silenciosa. Sin
    // esto, un flood de zero-width recortado a `char_cap` podría terminar
    // pesando 0 celdas y devolverse INTACTO (ya recortado, pero sin marcar)
    // por el early-return de ancho.
    let (chars, cortado_por_chars) = if chars.len() > char_cap {
        let head_n = char_cap / 2;
        let tail_n = char_cap - head_n;
        let recorte: Vec<char> = chars[..head_n]
            .iter()
            .chain(chars[chars.len() - tail_n..].iter())
            .copied()
            .collect();
        (recorte, true)
    } else {
        (chars, false)
    };
    let cell = |c: char| UnicodeWidthChar::width(c).unwrap_or(0);
    if !cortado_por_chars && chars.iter().copied().map(cell).sum::<usize>() <= max {
        return chars.into_iter().collect();
    }
    let s = &chars[..];
    // Una celda para el `…`; el resto se reparte cabeza/cola. Cada mitad
    // acumula chars mientras el siguiente QUEPA entero en su presupuesto: un
    // char ancho que no cabe se descarta (nunca se parte una celda).
    let budget = max.saturating_sub(1);
    let head_budget = budget / 2;
    let tail_budget = budget - head_budget;

    let mut head = String::new();
    let mut used = 0usize;
    for &c in s {
        let w = cell(c);
        if used + w > head_budget {
            break;
        }
        used += w;
        head.push(c);
    }

    let mut tail: Vec<char> = Vec::new();
    let mut used_tail = 0usize;
    for &c in s.iter().rev() {
        let w = cell(c);
        if used_tail + w > tail_budget {
            break;
        }
        used_tail += w;
        tail.push(c);
    }
    tail.reverse();
    // H3b encoding audit, FIX 3(b): the tail walker breaks on the first char
    // that does not FIT, and a combining mark is width 0 — it never breaks.
    // So when the base character it belongs to is the one that overflows, the
    // tail starts with a bare `U+0301`, which the terminal composes onto the
    // `…` we are about to write: the accent migrates from its letter to the
    // ellipsis. The same shape leaves a ZWJ emoji cluster starting on its
    // joiner. Leading zero-width chars in the tail have lost their base by
    // construction, so they are dropped rather than reparented.
    let first_visible = tail.iter().position(|&c| cell(c) > 0).unwrap_or(tail.len());
    tail.drain(..first_visible);

    let mut out = head;
    out.push('…');
    out.extend(tail);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::Scheme;

    fn root() -> VPath {
        VPath::root(Scheme::new("mem").unwrap(), None)
    }

    /// Encoding MEDIA-2: el TEXTO de `path_display` no puede contener NINGÚN
    /// char del set `must_mask` — el flag ya salía de `display_name`
    /// (criterio amplio), pero el texto era `display_lossy` (solo Cc+bidi):
    /// ZWSP/TAG crudos pintaban twins invisibles idénticos en los popups de
    /// navegación, ambos con badge. Corpus-driven: todo nombre hostil
    /// canónico, como segmento de un `VPath` real.
    #[test]
    fn path_display_jamas_pinta_chars_enmascarables() {
        // #98/F2 del audit: el sweep itera TAMBIÉN todo el ciclo de
        // reinterpretación — sin esto, `w1252_c1_controls` era inerte (con
        // enc=None los bytes caen a U+FFFD ANTES de que must_mask vea los
        // controles C1 que windows-1252 sí decodifica: U+009D es OSC).
        let encs = std::iter::once(None).chain(
            norte_encoding::name_reinterpret_cycle()
                .iter()
                .copied()
                .map(Some),
        );
        for enc in encs {
            for n in norte_testkit::corpus::hostile_names() {
                let p = root().join(norte_proto::Segment::new(n.bytes.clone()).unwrap());
                let (texto, _) = path_display_with(&p, enc);
                assert!(
                    !texto.chars().any(must_mask),
                    "{} bajo {enc:?}: sin chars de must_mask: {texto:?}",
                    n.id
                );
            }
        }
    }

    /// #98 (fixture `utf8_accidental_cp866`): bytes legacy que TAMBIÉN son
    /// UTF-8 válido («а» cirílica) JAMÁS se reinterpretan — bajo cualquier
    /// encoding del ciclo salen intactos y sin badge (regla mixto-sin-
    /// mojibake, indistinguible sin metadatos).
    #[test]
    fn utf8_accidental_no_se_reinterpreta_bajo_ningun_encoding() {
        let accidental = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "utf8_accidental_cp866")
            .expect("fixture del corpus")
            .bytes;
        for enc in norte_encoding::name_reinterpret_cycle() {
            let (texto, hostil) = display_name_with(&accidental, Some(*enc));
            assert_eq!(
                (texto.as_str(), hostil),
                ("а", false),
                "{}: UTF-8 válido intacto",
                enc.label()
            );
        }
    }

    /// #57: la reinterpretación decodifica SOLO nombres no-UTF8 (display),
    /// conserva el badge hostil, jamás toca un nombre UTF-8 válido y el
    /// enmascarado de hazards sobrevive a la decodificación (un cp437 que
    /// produzca un char de control no se pinta crudo).
    #[test]
    fn display_name_with_reinterpreta_solo_no_utf8_y_enmascara() {
        use norte_encoding::NameEncoding;
        // "CAFÉ.TXT" en cp437 (É = 0x90): decodifica y MARCA.
        let (texto, hostil) = display_name_with(b"CAF\x90.TXT", Some(NameEncoding::Cp437));
        assert_eq!(texto, "CAFÉ.TXT");
        assert!(hostil, "reinterpretado = pintado difiere de los bytes");
        // UTF-8 válido: intacto aunque haya reinterpretación activa.
        let (texto, hostil) = display_name_with("año.txt".as_bytes(), Some(NameEncoding::Cp437));
        assert_eq!(texto, "año.txt");
        assert!(!hostil);
        // None = display_name de siempre (lossy marcado).
        assert_eq!(
            display_name_with(b"\xFF\xFE", None),
            display_name(b"\xFF\xFE")
        );
        // Hazards post-decodificación: IBM866 decodifica 0x1B… no — 0x1B es
        // ASCII (ESC pasa tal cual por la mitad baja de cp437): debe salir
        // enmascarado, jamás un ESC crudo en el terminal.
        let (texto, hostil) = display_name_with(b"\x1b]0;x\x90", Some(NameEncoding::Cp437));
        assert!(!texto.contains('\u{1b}'), "ESC jamás crudo: {texto:?}");
        assert!(hostil);
    }

    /// El prefijo `⟨scheme authority⟩/` de `path_display` calca EXACTO el
    /// formato de `VPath::display_lossy` (proto vpath.rs): con segmentos
    /// limpios ambos textos son idénticos — los snapshots de panes no
    /// cambian.
    #[test]
    fn path_display_calca_el_prefijo_de_display_lossy() {
        let limpio = VPath::parse("sftp://oscar-host/docs/notas.txt").unwrap();
        assert_eq!(path_display(&limpio).0, limpio.display_lossy());
        let sin_auth = VPath::parse("mem:///a/b").unwrap();
        assert_eq!(path_display(&sin_auth).0, sin_auth.display_lossy());
        let raiz = root();
        assert_eq!(path_display(&raiz).0, raiz.display_lossy());
    }

    /// H3b encoding audit, FIX 3(b): the tail of a middle-truncated string
    /// must never BEGIN on a zero-width char.
    ///
    /// A combining mark is width 0, so the tail walker never breaks on one:
    /// when the base character it belongs to is the one that overflows the
    /// budget, the mark survives alone at the front of the tail and the
    /// terminal composes it onto the `…` — the accent migrates off its
    /// letter. The same shape leaves a ZWJ emoji cluster starting on its
    /// joiner. Corpus-driven: `nfd_e_acute` and `emoji_zwj_family` are the
    /// canonical fixtures for exactly this.
    #[test]
    fn middle_ellipsis_jamas_deja_la_cola_empezando_en_ancho_cero() {
        let corpus = norte_testkit::corpus::hostile_names();
        for id in ["nfd_e_acute", "emoji_zwj_family"] {
            let n = corpus
                .iter()
                .find(|n| n.id == id)
                .unwrap_or_else(|| panic!("{id} está en el corpus canónico"));
            let pieza =
                String::from_utf8(n.bytes.clone()).unwrap_or_else(|_| panic!("{id} es UTF-8"));
            // Repetida: así el corte cae en TODAS las posiciones posibles
            // dentro y entre clusters según el presupuesto, sin fijar a mano
            // el `max` que reproduce el fallo.
            let s = pieza.repeat(8);
            for max in 1..=32 {
                let out = middle_ellipsis(&s, max);
                let Some(cola) = out.split('…').nth(1) else {
                    continue;
                };
                if let Some(c) = cola.chars().next() {
                    assert!(
                        UnicodeWidthChar::width(c).unwrap_or(0) > 0,
                        "[{id}] max={max}: la cola empieza en U+{:04X} (ancho 0), \
                         que se compone sobre el `…`: {out:?}",
                        c as u32
                    );
                }
            }
        }
    }

    /// FIX 3(b) otra vez, pero sobre un TÍTULO de display en vez de sobre un
    /// nombre de fichero: la superficie que estrena H3b (la lateral de la
    /// ayuda, las filas de una página) y que H3f alimentará desde manifiestos
    /// de plugin.
    ///
    /// El corpus canónico tenía nombres hostiles (bytes) y chords hostiles
    /// (un codepoint), pero nada con la forma de un título recortado en una
    /// columna estrecha: `hostile_titles` es esa clase, y `nfd_accent_on_the_
    /// cut` la fija aquí. Un acento NFD pesa CERO celdas, así que el caminante
    /// de la cola no rompe nunca sobre él; cuando la letra a la que pertenece
    /// es la que desborda el presupuesto, el acento sobrevive SOLO al frente
    /// de la cola y el terminal lo compone sobre el `…` — el acento migra de
    /// su letra a la elipsis. El cluster ZWJ tiene la misma forma.
    ///
    /// Los tres títulos se barren a todos los anchos: cuál es el `max` que
    /// reproduce el fallo depende del texto, y fijarlo a mano es fijar el bug
    /// de hoy en vez del invariante.
    #[test]
    fn middle_ellipsis_sobre_titulos_hostiles_no_orfana_marcas() {
        for t in norte_testkit::corpus::hostile_titles() {
            for texto in std::iter::once(t.text).chain(t.twin) {
                for max in 1..=40 {
                    let out = middle_ellipsis(texto, max);
                    let Some(cola) = out.split('…').nth(1) else {
                        continue;
                    };
                    if let Some(c) = cola.chars().next() {
                        assert!(
                            UnicodeWidthChar::width(c).unwrap_or(0) > 0,
                            "[{}] max={max}: la cola empieza en U+{:04X} (ancho \
                             0), que se compone sobre el `…`: {out:?}",
                            t.id,
                            c as u32
                        );
                    }
                    assert!(
                        out.chars()
                            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
                            .sum::<usize>()
                            <= max,
                        "[{}] max={max}: el recorte desborda su presupuesto: {out:?}",
                        t.id
                    );
                }
            }
        }
    }

    /// La otra mitad de `truncation_twins`: la colisión es REAL y no se puede
    /// prevenir, así que lo que se exige es que el recorte se MARQUE.
    ///
    /// Dos títulos distintos que comparten todo hasta el corte se pintan
    /// idénticos en una columna estrecha — eso es geometría, no un bug. Lo que
    /// spec §6 no permite es que la pérdida sea SILENCIOSA: el `…` es lo que
    /// le dice al lector que lo que ve no es el título entero y que la fila de
    /// al lado puede ser otra cosa.
    #[test]
    fn dos_titulos_que_colisionan_al_recortarse_llevan_marca() {
        let par = norte_testkit::corpus::hostile_titles()
            .into_iter()
            .find(|t| t.id == "truncation_twins")
            .expect("fixture del corpus");
        let gemelo = par.twin.expect("una colisión necesita dos cadenas");
        assert_ne!(par.text, gemelo, "la fixture tiene que ser un PAR distinto");
        let a = middle_ellipsis(par.text, 20);
        let b = middle_ellipsis(gemelo, 20);
        assert!(
            a.contains('…') && b.contains('…'),
            "el recorte se marca SIEMPRE: {a:?} / {b:?}"
        );
        // Y sin recorte, no colisionan: la colisión es del ancho, no de los
        // datos.
        assert_ne!(
            middle_ellipsis(par.text, 200),
            middle_ellipsis(gemelo, 200),
            "con sitio de sobra los dos títulos son distinguibles"
        );
    }
}
