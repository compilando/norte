//! Los avisos persistentes de la barra: degradación de una conexión (#44),
//! el estado del journal (ocupado, recuperado, ausente) y el de la sesión,
//! más la frase que resume los tres.

use super::{App, DEGRADED_MAX, JournalIndicator};
use norte_i18n::{t, ta};

impl App {
    /// Records a `connection.degraded` notification (#44).
    ///
    /// One entry per scheme, so a second scheme does not evict the first, and a
    /// repeat for the SAME scheme replaces the entry AND becomes the newest:
    /// the latest report is the one worth naming, and the old one described the
    /// same session. Past `DEGRADED_MAX` the oldest is dropped — see the
    /// `degraded` field for why a wire-fed collection needs a ceiling.
    pub fn note_degraded(&mut self, d: norte_proto::methods::ConnectionDegraded) {
        self.degraded.retain(|old| old.scheme != d.scheme);
        self.degraded.push_back(d);
        while self.degraded.len() > DEGRADED_MAX {
            self.degraded.pop_front();
        }
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

    /// The persistent status-bar banner, or `None` when nothing degraded.
    ///
    /// It always NAMES a connection — the most recent one — and appends how
    /// many others there are. Reporting a bare count ("2 connections in
    /// plaintext") beats silently overwriting one report with another, but
    /// combined with never clearing it means the identity of every degraded
    /// session is lost for the rest of the session, and "which one?" is the
    /// only question this indicator exists to answer.
    ///
    /// Scheme and host are masked (`norte_frontend::display_name`) and the
    /// host is clamped: both are wire-supplied strings, and the status bar is
    /// the one place in the TUI they reach unfiltered. A host of control
    /// characters or bidi overrides is exactly what an attacker sends to a
    /// security indicator.
    ///
    /// Never cleared once set — see the `degraded` field for why that is a
    /// decision and not an omission.
    #[must_use]
    pub fn connection_banner(&self) -> Option<String> {
        /// Cells the host gets before the middle ellipsis takes over. Long
        /// enough for a real FQDN, short enough that the banner cannot push
        /// everything else off the status bar.
        const HOST_MAX: usize = 48;

        let last = self.degraded.back()?;
        let scheme = norte_frontend::display_name(last.scheme.as_bytes()).0;
        let host = norte_frontend::middle_ellipsis(
            &norte_frontend::display_name(last.host.as_bytes()).0,
            HOST_MAX,
        );
        let others = self.degraded.len() - 1;
        if others == 0 {
            return Some(ta(
                "status-connection-degraded",
                &[("scheme", &scheme), ("host", &host)],
            ));
        }
        Some(ta(
            "status-connections-degraded",
            &[
                ("scheme", &scheme),
                ("host", &host),
                ("n", &others.to_string()),
            ],
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
