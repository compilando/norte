//! Saneado de nombres para pintar en cualquier frontend: un nombre es bytes
//! (spec §6) y el texto que se pinta es SIEMPRE lossy y MARCADO — jamás
//! pérdida silenciosa, jamás controles/bidi crudos.

use norte_proto::VPath;

/// ¿Debe enmascararse en un terminal? DELEGA en
/// [`norte_encoding::is_terminal_hazard`] (fuente ÚNICA del set — antes vivía
/// atrapado aquí; ahora lo comparte con el saneo de preview de `fs.search`).
/// Cubre Cc (controles: `\n`, ESC — ratatui los BORRA en silencio y un
/// frontend directo los ejecutaría), los overrides bidi Cf (spoofing RTL del
/// orden visual) y los INVISIBLES Cf/Zl/Zp (encoding-auditor H4 de M3-3b: dos
/// nombres visualmente idénticos que difieren en bytes engañan a un humano que
/// aprueba "el que ya vio"): ZWSP/ZWNJ, LRM/RLM/ALM, WORD JOINER, BOM/ZWNBSP,
/// SOFT HYPHEN, TAG chars y los separadores Zl/Zp. ZWJ (U+200D) se PERMITE a
/// sabiendas: enmascararlo rompería los emoji compuestos legítimos (fixture
/// `emoji_zwj_family`) — fidelidad de emoji > el residual de un twin invisible.
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
        let (texto, h) = display_name(seg);
        hostil |= h;
        out.push_str(&texto);
    }
    (out, hostil)
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
        for n in norte_testkit::corpus::hostile_names() {
            let p = root().join(norte_proto::Segment::new(n.bytes.clone()).unwrap());
            let (texto, _) = path_display(&p);
            assert!(
                !texto.chars().any(must_mask),
                "{}: el texto de path_display no lleva chars de must_mask: {texto:?}",
                n.id
            );
        }
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
}
