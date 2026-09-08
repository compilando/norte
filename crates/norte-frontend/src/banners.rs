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

/// Cuántas celdas se le dan al detalle de un motivo DESCONOCIDO.
///
/// Corto a propósito: es texto libre del wire, sirve para orientar y no para
/// decidir, y lo que decide —el scheme y el host— va en su propio campo.
const DETAIL_MAX: usize = 64;

/// Las degradaciones retenidas, con su regla dentro.
///
/// **El tipo existe para que la regla no sea opcional.** Antes esto era una
/// `VecDeque` pelada y una función libre que la ordenaba: el techo y la clave
/// de dedupe solo se aplicaban si quien llamaba pasaba por ahí, y nada impedía
/// un `push_back` directo que se saltara los dos. Con los datos dentro, no hay
/// forma de escribir en la colección sin pasar por [`DegradedSet::note`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DegradedSet(std::collections::VecDeque<ConnectionDegraded>);

impl DegradedSet {
    /// Anota una degradación, UNA por SESIÓN.
    ///
    /// Y una sesión es `(scheme, host)`, no `scheme`. Deduplicar por el scheme
    /// a secas era la regla vieja del TUI, con la justificación de que «el
    /// viejo hablaba de la misma sesión»: con dos FTP a hosts distintos eso es
    /// falso, y el segundo aviso BORRABA el primero dejando el contador de «y
    /// otras N» en cero. El que desaparecía era justo el host que el lector no
    /// estaba mirando, y «¿cuál?» es la única pregunta que este indicador
    /// contesta.
    ///
    /// La identidad se decide plegando a minúsculas ASCII: `FTP` y `ftp` del
    /// wire son la misma sesión. Los BYTES que se pintan son los del informe
    /// último — plegar sirve para decidir, no para reescribir lo que se
    /// enseña.
    ///
    /// Pasado [`DEGRADED_MAX`] se cae el más antiguo.
    pub fn note(&mut self, d: ConnectionDegraded) {
        let clave =
            |x: &ConnectionDegraded| (x.scheme.to_ascii_lowercase(), x.host.to_ascii_lowercase());
        let nueva = clave(&d);
        self.0.retain(|old| clave(old) != nueva);
        self.0.push_back(d);
        while self.0.len() > DEGRADED_MAX {
            self.0.pop_front();
        }
    }

    /// La degradación informada para `scheme`, si la hay: la más reciente.
    ///
    /// No veta nada por sí sola — el vocabulario del wire dice «sin cifrar»,
    /// no «inservible».
    #[must_use]
    pub fn for_scheme(&self, scheme: &str) -> Option<&ConnectionDegraded> {
        self.0.iter().rev().find(|d| d.scheme == scheme)
    }

    /// Cuántas sesiones degradadas se retienen.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// ¿Ninguna?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// El aviso persistente, o `None` si no hay ninguna degradación.
    #[must_use]
    pub fn banner(&self, lang: norte_i18n::Lang) -> Option<DegradedBanner> {
        connection_banner(lang, &self.0)
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
    /// POR QUÉ está degradada, ya traducido (#279).
    ///
    /// El vocabulario del wire es cerrado y comparable por igualdad, y puede
    /// CRECER de forma aditiva. Un motivo que este binario no conoce cae en la
    /// frase genérica —«sesión degradada»— en vez de heredar la de
    /// `ftp-plaintext`, que es lo que pasaba antes: un daemon más nuevo
    /// informando de una degradación NUEVA se leía como «FTP en claro», o sea
    /// un aviso de seguridad afirmando algo que nadie había dicho.
    pub reason: String,
    /// El detalle humano del wire, enmascarado y acotado, y SOLO cuando el
    /// motivo es desconocido.
    ///
    /// Es lo que el contrato del proto pide: ante un `reason` que no se
    /// conoce, degradar «apoyándose en `detail`». Con un motivo conocido no
    /// aporta nada y sería texto libre del otro extremo dentro de un indicador
    /// de seguridad, así que no viaja.
    pub detail: Option<String>,
    /// Lo pintado difiere de lo que hay, en el esquema, el host o el detalle.
    pub hostile: bool,
}

/// La clave i18n del motivo, o `None` si este binario no lo conoce.
///
/// Un `match` sobre el vocabulario CERRADO del wire y no un
/// `format!("degraded-reason-{reason}")`: componer la clave con la cadena del
/// otro extremo deja que un daemon elija qué mensaje del catálogo se pinta, y
/// un motivo inventado saldría como su propio identificador crudo en la barra.
fn clave_de_motivo(reason: &str) -> Option<&'static str> {
    match reason {
        "ftp-plaintext" => Some("degraded-reason-ftp-plaintext"),
        "tls-auth-rejected" => Some("degraded-reason-tls-auth-rejected"),
        _ => None,
    }
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
    // El MOTIVO de la que se nombra (#279). Un motivo desconocido no hereda
    // la frase de `ftp-plaintext`: cae en la genérica y se apoya en `detail`,
    // que es lo que el contrato del proto pide.
    let conocido = clave_de_motivo(&last.reason);
    let reason = norte_i18n::t_in(lang, conocido.unwrap_or("degraded-reason-unknown"));
    let (detail, detail_hostil) = match (conocido, last.detail.as_deref()) {
        (None, Some(d)) if !d.is_empty() => {
            let (pintable, hostil) = crate::display_name(d.as_bytes());
            (Some(crate::middle_ellipsis(&pintable, DETAIL_MAX)), hostil)
        }
        _ => (None, false),
    };
    Some(DegradedBanner {
        text,
        scheme,
        host,
        reason,
        detail,
        hostile: scheme_hostil || host_hostil || detail_hostil,
    })
}

/// Por qué NO se pudo conectar, ya compuesto para pintar (#322).
///
/// Misma forma que [`DegradedBanner`] y por la misma razón: es un aviso sobre
/// una CONEXIÓN, y la autoridad no se interpola en la frase. Lo que cambia es
/// el momento — la degradación describe una sesión que existe, esto describe
/// una que no llegó a existir — y por eso no se retiene: no hay nada abierto
/// de lo que seguir avisando.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureNotice {
    /// La frase, ya traducida, SIN nada que venga del wire dentro.
    pub text: String,
    /// El nombre de `connections.toml`, enmascarado y acortado, si lo había.
    ///
    /// Es el único identificador que el humano ESCRIBIÓ, así que es el que
    /// reconoce; el scheme y el host los dedujo norte. Va en su propio campo
    /// como todo lo demás: viene del fichero de configuración, que es un
    /// origen tan poco de fiar como el wire para lo que se pinta.
    pub conn: Option<String>,
    /// El esquema, enmascarado.
    pub scheme: String,
    /// El host, enmascarado y acortado.
    pub host: String,
    /// POR QUÉ falló, ya traducido. Vocabulario cerrado; uno desconocido cae
    /// en la frase genérica y se apoya en `detail`.
    pub reason: String,
    /// El detalle humano del wire, enmascarado y acotado, y SOLO cuando el
    /// motivo es desconocido — igual que en [`DegradedBanner::detail`]. Con un
    /// motivo conocido la frase traducida ya lo dice, y el detalle sería texto
    /// del otro extremo repitiéndola en el idioma del daemon.
    pub detail: Option<String>,
    /// Lo pintado difiere de lo que hay, en algún campo.
    pub hostile: bool,
}

/// La clave i18n del motivo de un fallo, o `None` si este binario no lo conoce.
///
/// Un `match` sobre el vocabulario cerrado y no un `format!`, por lo mismo que
/// [`clave_de_motivo`]: componer la clave con la cadena del otro extremo deja
/// que el daemon elija qué mensaje del catálogo se pinta.
fn clave_de_fallo(reason: &str) -> Option<&'static str> {
    match reason {
        "secret-missing" => Some("failed-reason-secret-missing"),
        "secret-empty" => Some("failed-reason-secret-empty"),
        "secret-not-utf8" => Some("failed-reason-secret-not-utf8"),
        "secret-store" => Some("failed-reason-secret-store"),
        "auth-rejected" => Some("failed-reason-auth-rejected"),
        "no-user" => Some("failed-reason-no-user"),
        "agent" => Some("failed-reason-agent"),
        _ => None,
    }
}

/// Compone el aviso de un fallo de conexión (#322).
///
/// Las mismas reglas que [`connection_banner`], y no por simetría: son las que
/// impiden que un host como `banco.example@malo.example` se lea como userinfo
/// de un host legítimo. Un fallo de conexión es justo donde alguien querría
/// que se leyera así — «no pude entrar en tu banco, mete la clave otra vez».
#[must_use]
pub fn failure_notice(
    lang: norte_i18n::Lang,
    f: &norte_proto::methods::ConnectionFailed,
) -> FailureNotice {
    let (scheme, scheme_hostil) = crate::display_name(f.scheme.as_bytes());
    let (host_pintable, host_hostil) = crate::display_name(f.host.as_bytes());
    let scheme = crate::middle_ellipsis(&scheme, SCHEME_MAX);
    let host = crate::middle_ellipsis(&host_pintable, HOST_MAX);
    let (conn, conn_hostil) = match f.conn.as_deref() {
        Some(c) if !c.is_empty() => {
            let (pintable, hostil) = crate::display_name(c.as_bytes());
            (Some(crate::middle_ellipsis(&pintable, HOST_MAX)), hostil)
        }
        _ => (None, false),
    };
    let conocido = clave_de_fallo(&f.reason);
    let reason = norte_i18n::t_in(lang, conocido.unwrap_or("failed-reason-unknown"));
    let (detail, detail_hostil) = match (conocido, f.detail.as_deref()) {
        (None, Some(d)) if !d.is_empty() => {
            let (pintable, hostil) = crate::display_name(d.as_bytes());
            (Some(crate::middle_ellipsis(&pintable, DETAIL_MAX)), hostil)
        }
        _ => (None, false),
    };
    FailureNotice {
        text: norte_i18n::t_in(lang, "status-connection-failed"),
        conn,
        scheme,
        host,
        reason,
        detail,
        hostile: scheme_hostil || host_hostil || detail_hostil || conn_hostil,
    }
}

/// La línea de UN fallo de conexión, lista para pintar (#322).
///
/// Vive aquí y no en cada frontend por la regla D14, y esta vez con un motivo
/// que ya se cobró una pieza: la TUI y la ventana tienen que decir lo MISMO
/// sobre por qué no se pudo entrar en una máquina, y dos composiciones
/// divergen en silencio (ADR 0077). La ventana la manda como el `detail` de un
/// `Notice`, la TUI la pone en su barra: mismo texto, dos sitios.
///
/// La autoridad va ETIQUETADA —«esquema X, host Y»— y nunca como
/// `scheme://host`: ver el rustdoc de [`connection_banner`] para qué se
/// consigue mintiendo con la segunda forma.
#[must_use]
pub fn failure_line(lang: norte_i18n::Lang, f: &norte_proto::methods::ConnectionFailed) -> String {
    let n = failure_notice(lang, f);
    let linea = norte_i18n::ta_in(
        lang,
        "status-failed-subject",
        &[
            ("banner", &n.text),
            ("scheme", &n.scheme),
            ("host", &n.host),
            ("reason", &n.reason),
        ],
    );
    // El nombre de `connections.toml` DETRÁS, nunca delante. Es el único
    // identificador que el humano escribió y por eso viaja — pero sale de un
    // fichero, y `display_name` no enmascara lo imprimible: un nombre como
    // `banco.example» — ✗ no se pudo conectar` puesto en cabeza se lee como un
    // aviso COMPLETO sobre otra máquina, con el de verdad empujado detrás.
    // Detrás del motivo no puede suplantar a nada: lo que va delante ya lo
    // escribió norte.
    let linea = match &n.conn {
        Some(c) => format!("{linea} — «{c}»"),
        None => linea,
    };
    match &n.detail {
        Some(d) => format!("{linea}: {d}"),
        None => linea,
    }
}

/// Cuántas celdas se le dan al id de un plugin en un aviso. Un id
/// reverse-DNS real cabe; el techo existe porque el wire puede mandar
/// cualquier cosa.
const PLUGIN_ID_MAX: usize = 48;

/// Cuántas celdas se le dan a la frase de un hook. El daemon ya la acotó a
/// 200 caracteres; esto es el ancho de una barra de estado.
const PLUGIN_TEXT_MAX: usize = 160;

/// La frase de un `plugin.notice` (0.69.0, ADR 0100), o `None` si no hay
/// nada que enseñar: una clase desconocida sin texto.
///
/// Compartida por lo mismo que las de arriba: es texto de un tercero (el
/// hook) atribuido a un id que también viene del wire, y dos frontends son
/// dos sitios donde el enmascarado se olvida. El id va DELANTE y etiquetado
/// por norte —«⚑ org.x.y: …»—, así que la frase del plugin no puede
/// hacerse pasar por una de norte.
#[must_use]
pub fn plugin_notice_line(
    lang: norte_i18n::Lang,
    n: &norte_proto::methods::PluginNotice,
) -> Option<String> {
    let (id, _) = crate::display_name(n.plugin_id.as_bytes());
    let id = crate::middle_ellipsis(&id, PLUGIN_ID_MAX);
    let text = n.text.as_deref().map(|t| {
        let (pintable, _) = crate::display_name(t.as_bytes());
        crate::middle_ellipsis(&pintable, PLUGIN_TEXT_MAX)
    });
    match (n.kind.as_str(), text) {
        ("hooks-disabled", _) => Some(norte_i18n::ta_in(
            lang,
            "msg-plugin-hooks-disabled",
            &[("plugin", &id)],
        )),
        // `notify`, y cualquier clase que este binario no conozca pero
        // traiga texto: el contrato del proto dice apoyarse en él.
        (_, Some(text)) => Some(norte_i18n::ta_in(
            lang,
            "msg-plugin-notice",
            &[("plugin", &id), ("text", &text)],
        )),
        (_, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ninguna AUTORIDAD hostil del corpus llega entera a la FRASE (#277).
    ///
    /// Lo que este aviso existe para contestar es «¿cuál?», y todas las formas
    /// de mentir sobre eso viven en la autoridad: userinfo antes de un `@`,
    /// una frase in-band, un override bidi, homógrafos cirílicos, dos FQDN que
    /// colisionan al cortar y un scheme sin techo. La afirmación es la misma
    /// para las seis y es estructural, no por caso: la frase NO contiene la
    /// autoridad, cada parte va en su campo, lo que se enmascara se dice y lo
    /// que se corta se acota.
    #[test]
    fn ninguna_autoridad_hostil_del_corpus_entra_en_la_frase() {
        let hosts = norte_testkit::corpus::hostile_hosts();
        assert!(hosts.len() >= 6, "el corpus canónico no encoge");
        for h in &hosts {
            let mut cola = std::collections::VecDeque::new();
            cola.push_back(degradacion(h.scheme, h.host));
            let aviso = connection_banner(norte_i18n::Lang::Es, &cola).expect("hay aviso");
            assert!(
                !aviso.text.contains(h.host),
                "[{}] la autoridad se interpoló en la frase: {:?}",
                h.id,
                aviso.text
            );
            // Acotados los DOS: el techo estaba solo en el host, y un scheme
            // largo echa de la barra a la parte que identifica la máquina.
            assert!(aviso.scheme.chars().count() <= SCHEME_MAX, "[{}]", h.id);
            assert!(aviso.host.chars().count() <= HOST_MAX, "[{}]", h.id);
            // Y lo que se enmascara se dice.
            let alterado = aviso.scheme != h.scheme && !aviso.scheme.contains('\u{2026}')
                || aviso.host != h.host && !aviso.host.contains('\u{2026}');
            assert!(
                !alterado || aviso.hostile,
                "[{}] se alteró y no lo dice: {:?} / {:?}",
                h.id,
                aviso.scheme,
                aviso.host
            );
            // El par que colisiona al cortar sigue distinguiéndose a 48
            // celdas, y el corte se MARCA cuando lo hay.
            if let Some(gemelo) = h.twin {
                let mut otra = std::collections::VecDeque::new();
                otra.push_back(degradacion(h.scheme, gemelo));
                let b = connection_banner(norte_i18n::Lang::Es, &otra).expect("hay aviso");
                if aviso.host == b.host {
                    assert!(
                        aviso.host.contains('\u{2026}'),
                        "[{}] dos autoridades distintas se pintan igual SIN marca de corte",
                        h.id
                    );
                }
            }
        }
    }

    fn degradacion(scheme: &str, host: &str) -> ConnectionDegraded {
        ConnectionDegraded {
            scheme: scheme.to_owned(),
            host: host.to_owned(),
            reason: "ftp-plaintext".to_owned(),
            detail: None,
        }
    }

    fn fallo(scheme: &str, host: &str, reason: &str) -> norte_proto::methods::ConnectionFailed {
        norte_proto::methods::ConnectionFailed {
            conn: None,
            scheme: scheme.to_owned(),
            host: host.to_owned(),
            reason: reason.to_owned(),
            detail: None,
        }
    }

    /// #322: el aviso de fallo hereda las reglas del de degradación, y eso se
    /// comprueba con el MISMO corpus. Un fallo de conexión es el sitio con más
    /// premio para mentir sobre a qué máquina se intentó entrar.
    #[test]
    fn ninguna_autoridad_hostil_del_corpus_entra_en_la_frase_de_un_fallo() {
        let hosts = norte_testkit::corpus::hostile_hosts();
        assert!(hosts.len() >= 6, "el corpus canónico no encoge");
        for h in &hosts {
            let aviso = failure_notice(
                norte_i18n::Lang::Es,
                &fallo(h.scheme, h.host, "auth-rejected"),
            );
            assert!(
                !aviso.text.contains(h.host),
                "[{}] la autoridad se interpoló en la frase: {:?}",
                h.id,
                aviso.text
            );
            assert!(aviso.scheme.chars().count() <= SCHEME_MAX, "[{}]", h.id);
            assert!(aviso.host.chars().count() <= HOST_MAX, "[{}]", h.id);
            let alterado = aviso.scheme != h.scheme && !aviso.scheme.contains('\u{2026}')
                || aviso.host != h.host && !aviso.host.contains('\u{2026}');
            assert!(
                !alterado || aviso.hostile,
                "[{}] se alteró y no lo dice: {:?} / {:?}",
                h.id,
                aviso.scheme,
                aviso.host
            );
        }
    }

    /// El motivo se traduce por vocabulario CERRADO: uno que este binario no
    /// conoce cae en la frase genérica y NO hereda la del de al lado.
    ///
    /// Es la misma regla que #279 puso en la degradación, y por la misma
    /// razón: un daemon más nuevo informando de un motivo nuevo se leería
    /// como el motivo viejo, o sea un aviso afirmando algo que nadie dijo.
    #[test]
    fn un_motivo_desconocido_cae_en_la_generica_y_se_apoya_en_el_detalle() {
        let conocido = failure_notice(
            norte_i18n::Lang::Es,
            &fallo("sftp", "example.test", "secret-empty"),
        );
        assert_eq!(
            conocido.reason,
            norte_i18n::t_in(norte_i18n::Lang::Es, "failed-reason-secret-empty")
        );

        let mut f = fallo("sftp", "example.test", "motivo-del-futuro");
        f.detail = Some("algo que este binario no sabe nombrar".to_owned());
        let raro = failure_notice(norte_i18n::Lang::Es, &f);
        assert_eq!(
            raro.reason,
            norte_i18n::t_in(norte_i18n::Lang::Es, "failed-reason-unknown"),
            "un motivo desconocido no puede heredar la frase de otro"
        );
        assert_eq!(
            raro.detail.as_deref(),
            Some("algo que este binario no sabe nombrar"),
            "sin motivo conocido, el detalle es lo único que orienta"
        );
        assert!(
            conocido.detail.is_none(),
            "con motivo conocido el detalle sobra: sería la misma frase en el idioma del daemon"
        );
    }

    /// Este binario sabe traducir TODO el vocabulario que el proto declara.
    ///
    /// La otra mitad del cierre (la primera está en `norte-core`: lo que el
    /// core emite es lo que el proto declara). Sin esto, olvidar una clave
    /// i18n al añadir un motivo era invisible: `clave_de_fallo` devolviendo
    /// `None` es indistinguible de «un daemon más nuevo», así que el motivo
    /// nuevo se pintaba «desconocido» para siempre y nada se ponía rojo.
    #[test]
    fn se_traduce_todo_el_vocabulario_de_fallos() {
        for reason in norte_proto::methods::CONNECTION_FAILURE_REASONS {
            let clave = clave_de_fallo(reason)
                .unwrap_or_else(|| panic!("el proto declara {reason:?} y aquí no se traduce"));
            for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
                let frase = norte_i18n::t_in(lang, clave);
                assert_ne!(
                    frase, clave,
                    "[{lang:?}] {clave} no está en el catálogo: saldría su propio identificador"
                );
            }
        }
    }

    /// El nombre de `connections.toml` va en su CAMPO, acotado y enmascarado.
    ///
    /// Lo escribió el humano, pero en un fichero: un nombre con un salto de
    /// línea o un override bidi dentro rompe la barra igual que uno del wire.
    #[test]
    fn el_nombre_de_la_conexion_se_acota_y_se_enmascara() {
        let mut f = fallo("s3", "example.test", "auth-rejected");
        f.conn = Some(format!("mi\u{202e}conn{}", "x".repeat(200)));
        let aviso = failure_notice(norte_i18n::Lang::Es, &f);
        let conn = aviso.conn.expect("el nombre viaja");
        assert!(
            conn.chars().count() <= HOST_MAX,
            "sin techo: {}",
            conn.len()
        );
        assert!(!conn.contains('\u{202e}'), "el override bidi se enmascara");
        assert!(aviso.hostile, "se alteró y no lo decía");
    }

    /// Un scheme repetido no ocupa dos huecos, y el que se nombra es el
    /// último informe.
    #[test]
    fn la_misma_sesion_repetida_reemplaza_su_entrada() {
        let mut d = DegradedSet::default();
        d.note(degradacion("ftp", "uno.example"));
        d.note(degradacion("FTP", "UNO.example"));
        assert_eq!(d.len(), 1, "la caja del wire no hace dos sesiones");
        let aviso = d.banner(norte_i18n::active()).expect("hay aviso");
        assert_eq!(aviso.host, "UNO.example", "pinta los bytes del último");
    }

    /// Dos HOSTS del mismo scheme son dos sesiones: el segundo no puede
    /// borrar al primero, porque el que desaparece es justo el que no se
    /// está mirando.
    #[test]
    fn dos_hosts_del_mismo_scheme_no_se_pisan() {
        let mut d = DegradedSet::default();
        d.note(degradacion("ftp", "banco.example"));
        d.note(degradacion("ftp", "otro.example"));
        assert_eq!(d.len(), 2);
        let aviso = d.banner(norte_i18n::active()).expect("hay aviso");
        assert_eq!(aviso.host, "otro.example");
        assert!(aviso.text.contains('1'), "y cuenta la otra: {}", aviso.text);
    }

    /// Con varias, se nombra una Y se dice cuántas más hay.
    #[test]
    fn con_varias_se_nombra_una_y_se_cuentan_las_otras() {
        let mut d = DegradedSet::default();
        d.note(degradacion("ftp", "uno.example"));
        d.note(degradacion("sftp", "dos.example"));
        let aviso = d.banner(norte_i18n::active()).expect("hay aviso");
        assert_eq!(aviso.host, "dos.example");
        assert!(aviso.text.contains('1'), "{}", aviso.text);
    }

    /// Un host con caracteres de control NO llega crudo a la barra: es una
    /// cadena del wire, y esto es un indicador de seguridad.
    #[test]
    fn un_host_hostil_se_enmascara() {
        let mut d = DegradedSet::default();
        d.note(degradacion("ftp", "ma\u{7}lo\u{202e}.example"));
        let aviso = d.banner(norte_i18n::active()).expect("hay aviso");
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
            DegradedSet::default()
                .banner(norte_i18n::active())
                .is_none()
        );
    }

    /// **Un motivo DESCONOCIDO no hereda la frase de `ftp-plaintext`** (#279).
    ///
    /// El vocabulario del wire puede crecer, y antes de esto un daemon más
    /// nuevo informando de una degradación nueva se leía como «FTP en claro»:
    /// un indicador de seguridad afirmando algo que nadie había dicho.
    #[test]
    fn un_motivo_desconocido_cae_en_la_frase_generica() {
        let mut d = DegradedSet::default();
        let mut nueva = degradacion("sftp", "uno.example");
        nueva.reason = "quantum-downgrade".to_owned();
        nueva.detail = Some("el servidor negoció un perfil antiguo".to_owned());
        d.note(nueva);
        let aviso = d.banner(norte_i18n::Lang::Es).expect("hay aviso");

        let conocido = {
            let mut d = DegradedSet::default();
            d.note(degradacion("ftp", "uno.example"));
            d.banner(norte_i18n::Lang::Es).expect("hay aviso").reason
        };
        assert_ne!(aviso.reason, conocido, "no puede leerse como FTP en claro");
        assert_eq!(
            aviso.detail.as_deref(),
            Some("el servidor negoció un perfil antiguo"),
            "y se apoya en `detail`, que es lo que el proto pide"
        );
    }

    /// Con un motivo CONOCIDO el detalle no viaja: no aporta nada y sería
    /// texto libre del otro extremo dentro de un indicador de seguridad.
    #[test]
    fn un_motivo_conocido_no_arrastra_el_detalle() {
        let mut d = DegradedSet::default();
        let mut conocida = degradacion("ftp", "uno.example");
        conocida.detail = Some("cualquier cosa".to_owned());
        d.note(conocida);
        let aviso = d.banner(norte_i18n::Lang::Es).expect("hay aviso");
        assert_eq!(aviso.detail, None);
    }

    /// El detalle es una cadena del WIRE, así que pasa por el mismo
    /// enmascarado que el host — y cuando se altera, se dice.
    #[test]
    fn el_detalle_de_un_motivo_desconocido_se_enmascara() {
        let mut d = DegradedSet::default();
        let mut nueva = degradacion("sftp", "uno.example");
        nueva.reason = "nuevo".to_owned();
        nueva.detail = Some("ma\u{7}lo\u{202e}".to_owned());
        d.note(nueva);
        let aviso = d.banner(norte_i18n::Lang::Es).expect("hay aviso");
        let detalle = aviso.detail.clone().expect("hay detalle");
        assert!(
            !detalle.contains('\u{7}') && !detalle.contains('\u{202e}'),
            "{aviso:?}"
        );
        assert!(aviso.hostile, "y se dice: {aviso:?}");
    }

    /// El techo se aplica SIEMPRE, porque ya no hay forma de escribir en la
    /// colección sin pasar por `note`: era el agujero del punto 4 de #279.
    #[test]
    fn el_techo_no_se_puede_esquivar() {
        let mut d = DegradedSet::default();
        for i in 0..(DEGRADED_MAX + 10) {
            d.note(degradacion("ftp", &format!("h{i}.example")));
        }
        assert_eq!(d.len(), DEGRADED_MAX);
        let aviso = d.banner(norte_i18n::active()).expect("hay aviso");
        let ultimo = DEGRADED_MAX + 9;
        assert_eq!(
            aviso.host,
            format!("h{ultimo}.example"),
            "y el último sigue"
        );
    }

    /// ADR 0100: la frase de un hook lleva el id DELANTE, enmascarado; una
    /// clase desconocida se apoya en el texto y sin texto no se enseña nada.
    #[test]
    fn el_aviso_de_un_plugin_se_atribuye_y_se_enmascara() {
        let n = norte_proto::methods::PluginNotice {
            plugin_id: "org.norte.rename-log".to_owned(),
            kind: "notify".to_owned(),
            text: Some("renamed 3 files\u{1b}[31m\u{202e}".to_owned()),
        };
        let l = plugin_notice_line(norte_i18n::Lang::En, &n).expect("hay frase");
        assert!(l.starts_with("⚑ org.norte.rename-log"), "{l}");
        assert!(l.contains("renamed 3 files"), "{l}");
        assert!(
            !l.contains('\u{1b}') && !l.contains('\u{202e}'),
            "enmascarado: {l}"
        );

        let off = norte_proto::methods::PluginNotice {
            plugin_id: "org.norte.rename-log".to_owned(),
            kind: "hooks-disabled".to_owned(),
            text: None,
        };
        let l = plugin_notice_line(norte_i18n::Lang::Es, &off).expect("hay frase");
        assert_eq!(
            l,
            norte_i18n::ta_in(
                norte_i18n::Lang::Es,
                "msg-plugin-hooks-disabled",
                &[("plugin", "org.norte.rename-log")]
            )
        );

        let raro = norte_proto::methods::PluginNotice {
            plugin_id: "org.x".to_owned(),
            kind: "sing".to_owned(),
            text: None,
        };
        assert!(plugin_notice_line(norte_i18n::Lang::En, &raro).is_none());
    }
}
