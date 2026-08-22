//! Los avisos persistentes de la barra: degradación de una conexión (#44),
//! el estado del journal (ocupado, recuperado, ausente) y el de la sesión,
//! más la frase que resume los tres.

use super::{App, JournalIndicator};
use norte_i18n::t;

impl App {
    /// Records a `connection.degraded` notification (#44).
    ///
    /// La regla —una entrada por scheme, la repetida pasa a ser la más nueva,
    /// techo en `DEGRADED_MAX`— vive en `norte_frontend::banners`: la ventana
    /// gráfica tiene el mismo indicador, y dos copias de un aviso de
    /// SEGURIDAD son dos sitios donde el enmascarado se olvida.
    pub fn note_degraded(&mut self, d: norte_proto::methods::ConnectionDegraded) {
        norte_frontend::banners::note_degraded(&mut self.degraded, d);
    }

    /// The degradation reported for `scheme`, if any.
    ///
    /// This is the fact the help's [`norte_frontend::availability::Facts`]
    /// carries. It vetoes nothing on its own — see that field's rustdoc: the
    /// wire vocabulary means "unencrypted", not "unusable".
    #[must_use]
    pub fn degraded_for(&self, scheme: &str) -> Option<&norte_proto::methods::ConnectionDegraded> {
        self.degraded.iter().rev().find(|d| d.scheme == scheme)
    }

    /// El aviso persistente de conexiones en claro, o `None` si no hay
    /// ninguna. La frase la compone el módulo COMPARTIDO.
    ///
    /// Nunca se apaga una vez encendido — ver el campo `degraded` para por
    /// qué eso es una decisión y no un olvido.
    #[must_use]
    pub fn connection_banner(&self) -> Option<String> {
        let b = norte_frontend::banners::connection_banner(norte_i18n::active(), &self.degraded)?;
        // La barra del TUI es UNA línea de texto, así que aquí sí hay que
        // juntar la frase y la conexión — pero no como una URL: `scheme://host`
        // convierte a `banco.example@malo.example` en algo que se lee como
        // userinfo de un host legítimo. Etiquetado y separado, que es lo que
        // el resto de los modales de este frontend ya hacen.
        Some(norte_i18n::ta(
            "status-degraded-subject",
            &[("banner", &b.text), ("scheme", &b.scheme), ("host", &b.host)],
        ))
    }

    /// Anota que esta sesión no está registrando sus mutaciones (#177).
    ///
    /// Idempotente: el core avisa una vez por EPISODIO, y si alguna vez avisara
    /// dos, la segunda solo reescribe el mismo hecho.
    pub fn note_no_journal(&mut self, why: norte_core::embedded::NoJournal) {
        self.no_journal = Some(JournalIndicator::NotRecorded(why));
    }

    /// El journal lleva minutos ocupado y NO hay daemon escuchando (#203).
    ///
    /// Es el MISMO hecho que un `Busy` —la sesión muta sin registro— con una
    /// explicación distinta, así que enciende el indicador de siempre y además
    /// marca que ya no hay una razón inocente a mano. La barra lo dice con otra
    /// frase: la suave sale también cuando no pasa nada, y es la que el lector
    /// ya aprendió a no mirar.
    pub fn note_journal_squatted(&mut self) {
        self.no_journal = Some(JournalIndicator::Squatted);
    }

    /// Y que volvió a registrarlas (#179): la ventana de propiedad se reabrió.
    ///
    /// Apagar el indicador es la mitad que importa. Un «NO se registra» que no
    /// sabe volverse «ya sí» miente en cuanto el ocupante de paso suelta el
    /// fichero, y miente sobre lo único que la barra dice de TODA la sesión.
    ///
    /// **Lo que el indicador no sabe decir** es que una operación ya en marcha
    /// conserva el veredicto con el que empezó (#205): si se recupera el
    /// journal mientras un borrado largo sigue corriendo sin registrar, la
    /// barra se apaga y ese borrado sigue sin dejar filas. El aviso de
    /// recuperación lo dice con todas las letras —«desde tu PRÓXIMA
    /// operación»— pero lo borra la siguiente tecla. Distinguirlo en la barra
    /// pediría que el core expusiera cuántas Tasks van fijadas a no-registrar,
    /// y no lo hace.
    pub fn note_journal_recovered(&mut self) {
        self.no_journal = None;
    }

    /// El aviso PERSISTENTE de sesión sin journal, o `None` si sí se registra.
    ///
    /// Frase fija y sin el motivo: el motivo salió por `message` cuando ocurrió
    /// (con el error del core saneado), y la barra de estado tiene que caber.
    ///
    /// **DOS frases, porque son dos hechos distintos (#178).** `Busy` es «esto
    /// pasó y no quedó anotado» — la sesión muta, sin registro. `Failed` es
    /// «esto NO va a pasar»: la sesión rehúsa mutar hasta que el fichero se
    /// arregle. Enseñar «no se puede deshacer» sobre la segunda diría lo
    /// contrario de lo que ocurre, y esa clase de indicador es justo lo que
    /// #178 vino a quitar.
    #[must_use]
    pub fn journal_banner(&self) -> Option<String> {
        use norte_core::embedded::NoJournal as N;
        self.no_journal.as_ref().map(|state| match state {
            // #203: el mismo hecho que un `Busy` con otra explicación. La
            // frase suave sale también cuando hay un daemon vivo —el caso
            // corriente— así que sobre un ocupante sin explicar dice
            // demasiado poco.
            JournalIndicator::Squatted => t("status-journal-squatted"),
            JournalIndicator::NotRecorded(N::Failed(_)) => t("status-journal-refused"),
            // `Busy` y cualquier motivo futuro: el mensaje conservador es el
            // que no promete que la mutación se haya parado.
            JournalIndicator::NotRecorded(_) => t("status-no-journal"),
        })
    }

    /// Los dos indicadores persistentes de la barra, JUNTOS.
    ///
    /// Juntos y no en ramas distintas del `if` de la barra: son dos hechos
    /// simultáneos y de la misma clase —seguridad, hasta el final de la
    /// sesión—, así que elegir uno escondería el otro para siempre. El del
    /// journal va primero: «nada de esto se puede deshacer» pesa más que «esta
    /// conexión va en claro», y es el único que habla de TODA la sesión.
    #[must_use]
    pub fn persistent_banner(&self) -> Option<String> {
        let parts: Vec<String> = [
            self.journal_banner(),
            self.connection_banner(),
            self.session_banner(),
        ]
        .into_iter()
        .flatten()
        .collect();
        (!parts.is_empty()).then(|| parts.join("  "))
    }

    /// El aviso PERSISTENTE de ventana SUELTA, o `None` si ésta es la dueña
    /// de la sesión (#232).
    ///
    /// Una ventana suelta no escribe nunca: es una segunda ventana, un core
    /// sin el lock, o una que encontró un cuerpo de una versión más nueva. Se
    /// decía con un `message` al arrancar, y el primer mensaje que llegara
    /// después lo borraba — a partir de ahí la ventana dejaba de guardar la
    /// pantalla sin nada que lo dijera. Misma disciplina que el resto de esta
    /// línea: un estado que dura toda la sesión se pinta en cada frame, no
    /// una vez.
    #[must_use]
    pub fn session_banner(&self) -> Option<String> {
        self.session.detached.then(|| t("status-session-detached"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::pane::Pane;
    use crate::app::testutil::*;

    /// #44 guardaba la degradación como PROSA ya formateada: el scheme y el
    /// host se metían en el mensaje y se tiraban, así que «¿qué conexión se
    /// degradó?» no tenía respuesta. H3d la necesita por pane.
    #[test]
    fn la_degradacion_se_guarda_por_scheme() {
        let mut app = app_dos_panes();
        app.note_degraded(degradacion_de_test("sftp", "ejemplo.org"));
        let d = app.degraded_for("sftp").expect("la degradación se retuvo");
        assert_eq!(d.host, "ejemplo.org", "el host sobrevive, no solo la frase");
        assert_eq!(d.reason, "ftp-plaintext");
        assert!(app.degraded_for("file").is_none());
    }

    /// Y dos conexiones degradadas no se pisan: antes la última ganaba y la
    /// primera desaparecía de la barra sin que nada la hubiera resuelto.
    #[test]
    fn dos_degradaciones_conviven() {
        let mut app = app_dos_panes();
        app.note_degraded(degradacion_de_test("sftp", "a.org"));
        app.note_degraded(degradacion_de_test("ftp", "b.org"));
        assert!(app.degraded_for("sftp").is_some());
        assert!(app.degraded_for("ftp").is_some());
        // Y la barra deja de mentir sobre cuántas hay. Nombra la ÚLTIMA y dice
        // cuántas más: un recuento pelado («2 conexiones en texto plano»), con
        // el aviso que jamás se limpia, dejaba al lector sin poder averiguar
        // NUNCA cuáles eran — y esa es la única pregunta que este indicador
        // existe para contestar.
        let banner = app.connection_banner().expect("hay aviso");
        assert!(
            banner.contains("b.org"),
            "la más reciente se nombra: {banner}"
        );
        assert!(banner.contains('1'), "y cuántas más hay: {banner}");
    }

    /// #177: «esta sesión no queda registrada» tiene que sobrevivir a la
    /// siguiente tecla. Llega UNA vez, en mitad de una operación que el usuario
    /// acaba de lanzar, y `app.message` lo borra la pulsación siguiente — que
    /// es como decir que no se avisó.
    /// #203: el ocupante SIN daemon que lo explique se dice con otra frase.
    ///
    /// El hecho es el mismo que un `Busy` —la sesión muta sin quedar
    /// registrada— y por eso el indicador sigue encendido; lo que cambia es que
    /// la frase suave sale también cuando hay un daemon vivo, o sea casi
    /// siempre, y es la que el lector ya aprendió a no mirar.
    #[test]
    fn el_ocupante_sin_daemon_tiene_su_propia_frase() {
        let mut app = App::new(Pane::new(root(), vec![]), Pane::new(root(), vec![]));
        app.note_no_journal(norte_core::embedded::NoJournal::Busy);
        let soft = app.journal_banner().expect("indicador encendido");

        app.note_journal_squatted();
        let strong = app.journal_banner().expect("sigue encendido");
        assert_ne!(soft, strong, "dos hechos distintos, dos frases");

        // Y se apaga igual: una recuperación borra los dos.
        app.note_journal_recovered();
        assert!(app.journal_banner().is_none());

        // Un `Busy` posterior vuelve a la frase suave y no se queda con la
        // fuerte pegada.
        app.note_no_journal(norte_core::embedded::NoJournal::Busy);
        assert_eq!(app.journal_banner().as_deref(), Some(soft.as_str()));
    }

    #[test]
    fn la_sesion_sin_journal_tiene_indicador_persistente() {
        let mut app = app_dos_panes();
        assert!(app.journal_banner().is_none(), "por defecto sí se registra");

        app.message = Some("algo".to_owned());
        app.note_no_journal(norte_core::embedded::NoJournal::Busy);
        // Lo que borra el `message` en el run loop, tecla a tecla.
        app.message = None;
        assert!(
            app.journal_banner().is_some(),
            "el indicador no se va con el mensaje"
        );
    }

    /// Y no compite con el de #44: los dos son persistentes, de la misma clase
    /// y simultáneos, así que elegir uno escondería el otro para el resto de la
    /// sesión.
    #[test]
    fn los_dos_indicadores_persistentes_caben_juntos() {
        let mut app = app_dos_panes();
        app.note_no_journal(norte_core::embedded::NoJournal::Busy);
        app.note_degraded(degradacion_de_test("sftp", "a.org"));
        let banner = app.persistent_banner().expect("hay aviso");
        assert!(
            banner.contains("a.org"),
            "la conexión sigue nombrada: {banner}"
        );
        assert!(
            banner.starts_with(&app.journal_banner().expect("hay journal_banner")),
            "y el del journal va primero: {banner}"
        );
    }

    /// #232: una ventana SUELTA lo dice una vez y luego se le olvida.
    ///
    /// El mensaje de arranque lo borra la siguiente tecla, y a partir de ahí
    /// la ventana no guarda la pantalla sin nada en pantalla que lo diga.
    #[test]
    fn la_ventana_suelta_tiene_indicador_persistente() {
        let mut app = app_dos_panes();
        assert!(app.session_banner().is_none(), "la dueña no avisa de nada");

        app.session.detached = true;
        app.message = Some("algo".to_owned());
        // Lo que borra el `message` en el run loop, tecla a tecla.
        app.message = None;
        let banner = app.persistent_banner().expect("hay aviso");
        assert_eq!(
            banner,
            app.session_banner().expect("hay session_banner"),
            "sin nada más encendido, la barra es justo ese aviso: {banner}"
        );
    }

    /// Y convive con los otros dos: son tres hechos simultáneos de la misma
    /// clase, y el de la sesión es el que menos pesa, así que va el último.
    #[test]
    fn los_tres_indicadores_persistentes_caben_juntos() {
        let mut app = app_dos_panes();
        app.note_no_journal(norte_core::embedded::NoJournal::Busy);
        app.note_degraded(degradacion_de_test("sftp", "a.org"));
        app.session.detached = true;
        let banner = app.persistent_banner().expect("hay aviso");
        assert!(
            banner.starts_with(&app.journal_banner().expect("hay journal_banner")),
            "el del journal sigue primero: {banner}"
        );
        assert!(
            banner.contains("a.org"),
            "la conexión sigue nombrada: {banner}"
        );
        assert!(
            banner.ends_with(&app.session_banner().expect("hay session_banner")),
            "y el de la sesión cierra: {banner}"
        );
    }

    /// MINOR-5: el `Option<String>` de #44 estaba acotado por construcción;
    /// una colección con clave que viene del WIRE no lo está. El tope es
    /// generoso —hay siete schemes— así que solo lo alcanza algo anómalo, y
    /// cuando pasa se tira lo más viejo y se conserva lo que acaba de llegar.
    #[test]
    fn las_degradaciones_tienen_tope() {
        let mut app = app_dos_panes();
        for i in 0..(norte_frontend::banners::DEGRADED_MAX + 10) {
            app.note_degraded(degradacion_de_test(&format!("s{i}"), "host"));
        }
        assert_eq!(app.degraded.len(), norte_frontend::banners::DEGRADED_MAX);
        assert!(
            app.degraded_for("s0").is_none(),
            "la más vieja es la que se cae"
        );
        assert!(
            app.degraded_for(&format!("s{}", norte_frontend::banners::DEGRADED_MAX + 9))
                .is_some(),
            "la última en llegar se queda"
        );
    }

    /// El host lo elige el OTRO extremo, y la barra de estado es el sitio
    /// donde llegaba crudo mientras el resto de la TUI enmascara. Un host con
    /// controles o bidi es exactamente lo que se le manda a un indicador de
    /// seguridad para que mienta.
    #[test]
    fn el_aviso_enmascara_un_host_hostil() {
        let mut app = app_dos_panes();
        app.note_degraded(degradacion_de_test("sftp", "ma\u{202e}gro.org\n"));
        let banner = app.connection_banner().expect("hay aviso");
        assert!(
            !banner.contains('\u{202e}') && !banner.contains('\n'),
            "el host llegó crudo a la barra: {banner:?}"
        );
        assert!(
            banner.contains('\u{FFFD}'),
            "y el enmascarado se VE (jamás pérdida silenciosa): {banner:?}"
        );
    }

    /// Sin degradación no hay aviso, y con UNA el aviso es el de siempre
    /// (#44): scheme y host, formateados desde el valor estructurado.
    #[test]
    fn el_aviso_de_una_sola_degradacion_nombra_la_conexion() {
        let mut app = app_dos_panes();
        assert!(app.connection_banner().is_none());
        app.note_degraded(degradacion_de_test("sftp", "remoto.example"));
        let banner = app.connection_banner().expect("hay aviso");
        assert!(banner.contains("sftp"), "{banner}");
        assert!(banner.contains("remoto.example"), "{banner}");
    }
}
