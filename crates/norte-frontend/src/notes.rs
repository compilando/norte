//! Las frases que dicen que un listado NO está completo, o que no es lo que
//! parece.
//!
//! Todas obedecen la misma regla, escrita en su día para la barra del
//! terminal: **un listado que enseña menos de lo que hay jamás es
//! silencioso.** Lo que falta no está, así que no hay ninguna fila donde el
//! lector pueda tropezarse con ello — si nadie lo dice, la pantalla afirma
//! que eso es todo.
//!
//! Viven aquí porque los dos frontends las necesitan y cada uno las redactaba
//! por su cuenta, que es la forma que tiene una decisión de divergir sin que
//! nadie lo note (ADR 0077). Y ya habían divergido: la ventana decía «se
//! saltaron N entradas» sin el ⚠ y **también con N igual a cero**, o sea que
//! anunciaba un listado incompleto que estaba completo; y la marca de
//! reinterpretación de nombres, que en el terminal es permanente porque los
//! nombres pintados no son los bytes, allí no existía.
//!
//! Cada una devuelve la cadena VACÍA cuando no hay nada que decir. Eso es la
//! otra mitad del contrato y no un detalle: una frase que sale siempre es
//! ruido, y el ruido enseña a saltarse la línea justo el día que dice algo.
//!
//! # El contrato del espaciado
//!
//! **Una frase no lleva separador ni espacios en los extremos.** Cómo se
//! separan es de quien las pone en fila: el terminal las une con dos espacios
//! en una barra de texto, la ventana las manda en nodos distintos y las
//! separa con CSS. Sin esta regla escrita, un `.ftl` con un espacio delante
//! le da al terminal tres y a la ventana un hueco doble, y ningún test se
//! entera — así que la fija `ninguna_frase_trae_su_propio_espaciado`, aquí
//! abajo.

use norte_i18n::{Lang, t_in, ta_in};

/// Lo que el provider se SALTÓ del listado: nombres que no pudo statear,
/// entradas por encima de un tope suyo (#93, #96).
///
/// Con cero se calla, y ese es el bug que esta función existe para no volver
/// a cometer: «se saltaron 0 entradas» describe un listado completo como si
/// estuviera incompleto, que gasta la única señal que hay para cuando de
/// verdad falta algo.
///
/// ```
/// use norte_frontend::notes::skipped;
/// use norte_i18n::Lang;
///
/// assert!(skipped(Some(3), Lang::En).contains('3'));
/// assert_eq!(skipped(Some(0), Lang::En), "", "cero omitidas no es un aviso");
/// assert_eq!(skipped(None, Lang::En), "", "el provider no lleva la cuenta");
/// ```
#[must_use]
pub fn skipped(n: Option<u64>, lang: Lang) -> String {
    match n {
        Some(n) if n > 0 => ta_in(lang, "status-archive-skipped", &[("n", &n.to_string())]),
        _ => String::new(),
    }
}

/// Los nombres se están REINTERPRETANDO con otra codificación (#57).
///
/// Permanente mientras dure, no solo en el mensaje del toggle: lo que se
/// pinta no son los bytes que hay en el disco, y el lector tiene que poder
/// saberlo en el momento en el que decide copiar o borrar algo, no medio
/// minuto antes.
///
/// ```
/// use norte_frontend::notes::names_encoding;
/// use norte_i18n::Lang;
///
/// assert_eq!(names_encoding(None, Lang::En), "", "sin reinterpretar, nada");
/// let cp437 = norte_encoding::NameEncoding::Cp437;
/// assert!(names_encoding(Some(cp437), Lang::En).contains("cp437"));
/// ```
#[must_use]
pub fn names_encoding(enc: Option<norte_encoding::NameEncoding>, lang: Lang) -> String {
    enc.map_or_else(String::new, |e| {
        ta_in(lang, "status-names-encoding", &[("enc", e.label())])
    })
}

/// Marcas que el último refresco descartó porque su entrada ya no está.
///
/// Jamás silencioso, y por un motivo más duro que el resto: con la selección
/// vacía el embudo del operando cae al CURSOR, así que callar que la
/// selección se vació redirige la siguiente operación en masa a algo que
/// nadie marcó.
///
/// ```
/// use norte_frontend::notes::pruned_marks;
/// use norte_i18n::Lang;
///
/// assert!(pruned_marks(2, Lang::En).contains('2'));
/// assert_eq!(pruned_marks(0, Lang::En), "");
/// ```
#[must_use]
pub fn pruned_marks(n: usize, lang: Lang) -> String {
    if n == 0 {
        return String::new();
    }
    ta_in(lang, "status-marks-pruned", &[("n", &n.to_string())])
}

/// Cuántas entradas hay marcadas y cuánto pesan.
///
/// La única de este módulo que no es un aviso, sino un contador, y por eso va
/// DETRÁS de los demás donde el sitio se reparte: un aviso recortado deja de
/// avisar y un contador recortado solo deja de contar.
///
/// Los directorios se nombran aparte porque no suman bytes: un «3 marcadas,
/// 12 KB» sobre dos carpetas y un fichero describe mal lo que se va a mover.
///
/// ```
/// use norte_frontend::notes::marked;
/// use norte_i18n::Lang;
///
/// assert_eq!(marked(0, 0, 0, Lang::En), "", "quien no marca no gana ruido");
/// assert!(marked(2, 2048, 0, Lang::En).contains('2'));
/// // Con directorios de por medio, la frase los cuenta aparte.
/// assert!(marked(3, 2048, 2, Lang::En).contains('3'));
/// ```
#[must_use]
pub fn marked(n: usize, bytes: u64, dirs: usize, lang: Lang) -> String {
    if n == 0 {
        return String::new();
    }
    let n = n.to_string();
    let size = crate::human_bytes(bytes);
    if dirs == 0 {
        return ta_in(lang, "status-marked", &[("n", &n), ("size", &size)]);
    }
    ta_in(
        lang,
        "status-marked-with-dirs",
        &[("n", &n), ("size", &size), ("dirs", &dirs.to_string())],
    )
}

/// El listado se está RELLENANDO todavía (paginación, ADR 0017): esto es lo
/// que hay POR AHORA.
///
/// ```
/// use norte_frontend::notes::filling;
/// use norte_i18n::Lang;
///
/// assert!(filling(true, 120, Lang::En).contains("120"));
/// assert_eq!(filling(false, 120, Lang::En), "", "ya está entero");
/// ```
#[must_use]
pub fn filling(loading: bool, so_far: usize, lang: Lang) -> String {
    if !loading {
        return String::new();
    }
    ta_in(lang, "pane-loading", &[("n", &so_far.to_string())])
}

/// Este hueco NO se pudo listar (#235).
///
/// Sin esto la pantalla afirma que el directorio está vacío, que es
/// precisamente lo que no se sabe.
///
/// ```
/// use norte_frontend::notes::unlisted;
/// use norte_i18n::Lang;
///
/// assert!(!unlisted(true, Lang::En).is_empty());
/// assert_eq!(unlisted(false, Lang::En), "");
/// ```
#[must_use]
pub fn unlisted(yes: bool, lang: Lang) -> String {
    if yes {
        return t_in(lang, "pane-unlisted");
    }
    String::new()
}

/// Cuántas entradas aparta la OCULTACIÓN activa (#107).
///
/// ```
/// use norte_frontend::notes::hidden;
/// use norte_i18n::Lang;
///
/// assert!(hidden(4, Lang::En).contains('4'));
/// assert_eq!(hidden(0, Lang::En), "", "un dir sin ocultos no dice nada");
/// ```
#[must_use]
pub fn hidden(n: usize, lang: Lang) -> String {
    if n == 0 {
        return String::new();
    }
    ta_in(lang, "status-hidden", &[("n", &n.to_string())])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ninguna clave se queda sin traducir en ninguno de los dos idiomas.
    ///
    /// `t_in` contesta la clave misma cuando no la tiene, así que una entrada
    /// que falte en un `.ftl` se pintaría como `status-archive-skipped` al
    /// lector — el eco que el resto del crate se cuida de no producir.
    #[test]
    fn las_frases_estan_en_los_dos_idiomas() {
        let cp437 = norte_encoding::NameEncoding::Cp437;
        for lang in [Lang::Es, Lang::En] {
            for frase in [
                skipped(Some(2), lang),
                names_encoding(Some(cp437), lang),
                pruned_marks(1, lang),
                marked(2, 10, 0, lang),
                marked(2, 10, 1, lang),
                filling(true, 3, lang),
                unlisted(true, lang),
                hidden(5, lang),
            ] {
                assert!(!frase.is_empty(), "sin frase en {lang:?}");
                assert!(
                    !frase.starts_with("status-")
                        && !frase.starts_with("pane-")
                        && !frase.contains('{'),
                    "clave cruda o marca sin sustituir en {lang:?}: {frase}"
                );
            }
        }
    }

    /// Ninguna trae separador ni espacios en los extremos: eso es de quien
    /// las pone en fila, y cada frontend lo hace distinto. Un `.ftl` con un
    /// espacio delante le da al terminal tres y a la ventana un hueco doble.
    #[test]
    fn ninguna_frase_trae_su_propio_espaciado() {
        let cp437 = norte_encoding::NameEncoding::Cp437;
        for lang in [Lang::Es, Lang::En] {
            for frase in [
                skipped(Some(2), lang),
                names_encoding(Some(cp437), lang),
                pruned_marks(1, lang),
                marked(2, 10, 1, lang),
                filling(true, 3, lang),
                unlisted(true, lang),
                hidden(5, lang),
            ] {
                assert_eq!(frase.trim(), frase, "trae espaciado propio: {frase:?}");
            }
        }
    }

    /// El aviso de omitidas lleva la MARCA: es lo que lo distingue de un
    /// contador a un metro de la pantalla, y es la mitad de por qué el aviso
    /// de la ventana no se leía como un aviso.
    #[test]
    fn el_aviso_de_omitidas_va_marcado() {
        for lang in [Lang::Es, Lang::En] {
            assert!(
                skipped(Some(9), lang).contains('⚠'),
                "sin marca no se lee como aviso en {lang:?}"
            );
        }
    }

    /// Y todas callan cuando no hay nada que decir. Es lo que hace que la que
    /// habla signifique algo.
    #[test]
    fn el_silencio_es_el_caso_normal() {
        let l = Lang::Es;
        assert_eq!(skipped(None, l), "");
        assert_eq!(skipped(Some(0), l), "");
        assert_eq!(names_encoding(None, l), "");
        assert_eq!(pruned_marks(0, l), "");
        assert_eq!(marked(0, 999, 3, l), "", "sin marcas no hay nada que sumar");
        assert_eq!(filling(false, 9, l), "");
        assert_eq!(unlisted(false, l), "");
        assert_eq!(hidden(0, l), "");
    }
}
