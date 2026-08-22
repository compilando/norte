//! Los avisos PERSISTENTES de la barra de estado, compartidos.
//!
//! Viven aquí y no en un frontend porque son presentación pura sobre datos
//! del wire, y porque la regla D14 del plan del frontend nuevo prohíbe
//! escribirlos dos veces: dos implementaciones de un indicador de SEGURIDAD
//! son dos sitios donde el enmascarado se olvida, y la ventana gráfica
//! heredaría la versión sin él.

use norte_proto::methods::ConnectionDegraded;

/// Cuántas degradaciones se retienen como mucho.
///
/// Es una colección alimentada por el WIRE: un servidor que reconecte en
/// bucle mandaría un aviso por intento, y sin techo la barra de estado se
/// convierte en un canal de memoria de crecimiento libre.
pub const DEGRADED_MAX: usize = 32;

/// Cuántas celdas se le dan al host antes de la elipsis del medio.
///
/// Largo para un FQDN de verdad, corto para que el aviso no eche de la barra
/// a todo lo demás.
const HOST_MAX: usize = 48;

/// Lo mismo para el esquema. Siete letras son un scheme de verdad; el tope
/// existe porque el wire puede mandar cualquier cosa.
const SCHEME_MAX: usize = 16;

/// Anota una degradación en la colección, UNA por SESIÓN.
///
/// Y una sesión es `(scheme, host)`, no `scheme`. Deduplicar por el scheme a
/// secas era la regla vieja del TUI, con la justificación de que «el viejo
/// hablaba de la misma sesión»: con dos FTP a hosts distintos eso es falso, y
/// el segundo aviso BORRABA el primero dejando el contador de «y otras N» en
/// cero. El que desaparecía era justo el host que el lector no estaba
/// mirando, y «¿cuál?» es la única pregunta que este indicador contesta.
///
/// La identidad se decide plegando a minúsculas ASCII: `FTP` y `ftp` del wire
/// son la misma sesión. Los BYTES que se pintan son los del informe último —
/// plegar sirve para decidir, no para reescribir lo que se enseña.
///
/// Pasado [`DEGRADED_MAX`] se cae el más antiguo.
pub fn note_degraded(
    degraded: &mut std::collections::VecDeque<ConnectionDegraded>,
    d: ConnectionDegraded,
) {
    let clave =
        |x: &ConnectionDegraded| (x.scheme.to_ascii_lowercase(), x.host.to_ascii_lowercase());
    let nueva = clave(&d);
    degraded.retain(|old| clave(old) != nueva);
    degraded.push_back(d);
    while degraded.len() > DEGRADED_MAX {
        degraded.pop_front();
    }
}

/// El aviso persistente de conexiones en claro, o `None` si no hay ninguna.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DegradedBanner {
    /// La frase, ya traducida, SIN nada que venga del wire dentro.
    pub text: String,
    /// El esquema, enmascarado.
    pub scheme: String,
    /// El host, enmascarado y acortado.
    pub host: String,
    /// Lo pintado difiere de lo que hay, en el esquema o en el host.
    pub hostile: bool,
}

/// Compone el aviso: la frase por un lado y la conexión por otro.
///
/// Siempre NOMBRA una conexión —la más reciente— y dice cuántas más hay. Un
/// recuento pelado («2 conexiones en texto plano») pierde la identidad de
/// todas, y «¿cuál?» es la única pregunta que este indicador existe para
/// contestar.
///
/// **La conexión NO se interpola en la frase**, y eso no es estilo. Montar
/// `{scheme}://{host}` dentro del texto convierte a un host como
/// `banco.example@malo.example` —que `Authority::new` acepta, y que no lleva
/// ni un carácter que se enmascare— en algo que se lee como userinfo de un
/// host legítimo, en el indicador donde más valor tiene mentir. Cada parte en
/// su campo, y quien pinte que las separe.
///
/// El idioma va como PARÁMETRO y no se lee del global: la ventana gráfica
/// tiene uno por instancia, y un aviso de seguridad en el idioma de otra
/// ventana es un aviso que no se lee.
///
/// Scheme y host se enmascaran y el host se acorta; la bandera dice que se
/// hizo, porque lo que se enmascara se dice.
#[must_use]
pub fn connection_banner(
    lang: norte_i18n::Lang,
    degraded: &std::collections::VecDeque<ConnectionDegraded>,
) -> Option<DegradedBanner> {
    let last = degraded.back()?;
    let (scheme, scheme_hostil) = crate::display_name(last.scheme.as_bytes());
    let (host_pintable, host_hostil) = crate::display_name(last.host.as_bytes());
    // El scheme también se acota: el techo estaba solo en el host, y un
    // scheme de sesenta kilobytes echa de la barra a todo lo demás.
    let scheme = crate::middle_ellipsis(&scheme, SCHEME_MAX);
    let host = crate::middle_ellipsis(&host_pintable, HOST_MAX);
    let others = degraded.len() - 1;
    let text = if others == 0 {
        norte_i18n::t_in(lang, "status-connection-degraded")
    } else {
        norte_i18n::ta_in(
            lang,
            "status-connections-degraded",
            &[("n", &others.to_string())],
        )
    };
    Some(DegradedBanner {
        text,
        scheme,
        host,
        hostile: scheme_hostil || host_hostil,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn degradacion(scheme: &str, host: &str) -> ConnectionDegraded {
        ConnectionDegraded {
            scheme: scheme.to_owned(),
            host: host.to_owned(),
            reason: "ftp-plaintext".to_owned(),
            detail: None,
        }
    }

    /// Un scheme repetido no ocupa dos huecos, y el que se nombra es el
    /// último informe.
    #[test]
    fn la_misma_sesion_repetida_reemplaza_su_entrada() {
        let mut d = std::collections::VecDeque::new();
        note_degraded(&mut d, degradacion("ftp", "uno.example"));
        note_degraded(&mut d, degradacion("FTP", "UNO.example"));
        assert_eq!(d.len(), 1, "la caja del wire no hace dos sesiones");
        let aviso = connection_banner(norte_i18n::active(), &d).expect("hay aviso");
        assert_eq!(aviso.host, "UNO.example", "pinta los bytes del último");
    }

    /// Dos HOSTS del mismo scheme son dos sesiones: el segundo no puede
    /// borrar al primero, porque el que desaparece es justo el que no se
    /// está mirando.
    #[test]
    fn dos_hosts_del_mismo_scheme_no_se_pisan() {
        let mut d = std::collections::VecDeque::new();
        note_degraded(&mut d, degradacion("ftp", "banco.example"));
        note_degraded(&mut d, degradacion("ftp", "otro.example"));
        assert_eq!(d.len(), 2);
        let aviso = connection_banner(norte_i18n::active(), &d).expect("hay aviso");
        assert_eq!(aviso.host, "otro.example");
        assert!(aviso.text.contains('1'), "y cuenta la otra: {}", aviso.text);
    }

    /// Con varias, se nombra una Y se dice cuántas más hay.
    #[test]
    fn con_varias_se_nombra_una_y_se_cuentan_las_otras() {
        let mut d = std::collections::VecDeque::new();
        note_degraded(&mut d, degradacion("ftp", "uno.example"));
        note_degraded(&mut d, degradacion("sftp", "dos.example"));
        let aviso = connection_banner(norte_i18n::active(), &d).expect("hay aviso");
        assert_eq!(aviso.host, "dos.example");
        assert!(aviso.text.contains('1'), "{}", aviso.text);
    }

    /// Un host con caracteres de control NO llega crudo a la barra: es una
    /// cadena del wire, y esto es un indicador de seguridad.
    #[test]
    fn un_host_hostil_se_enmascara() {
        let mut d = std::collections::VecDeque::new();
        note_degraded(&mut d, degradacion("ftp", "ma\u{7}lo\u{202e}.example"));
        let aviso = connection_banner(norte_i18n::active(), &d).expect("hay aviso");
        assert!(
            !aviso.host.contains('\u{7}') && !aviso.host.contains('\u{202e}'),
            "{aviso:?}"
        );
        assert!(aviso.hostile, "y se DICE que se enmascaró: {aviso:?}");
    }

    /// Sin degradaciones no hay aviso.
    #[test]
    fn sin_degradaciones_no_hay_aviso() {
        assert!(
            connection_banner(norte_i18n::active(), &std::collections::VecDeque::new()).is_none()
        );
    }
}
