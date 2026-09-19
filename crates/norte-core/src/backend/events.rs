//! El área de canales de eventos de [`Backend`](super::Backend): tasks
//! foráneas, eventos de conexión, aprobaciones de policy, degradación/fallo
//! de conexión, avisos de plugins (`hook`) y avisos del journal perezoso.

use std::sync::Arc;

use tokio::sync::mpsc;

use super::{
    Backend, ChannelConnectionObserver, ChannelFailureObserver, ChannelHookSink,
    ChannelJournalSink, ConnEvent, TaskRef,
};

impl Backend {
    /// Canal de tasks FORÁNEAS (encoladas por otros frontends de la misma
    /// sesión). `None` en embebido o si ya se tomó. Solo el dueño original de
    /// la conexión debe llamarlo; un clon (scripting) no.
    pub fn take_foreign_tasks(&mut self) -> Option<mpsc::UnboundedReceiver<TaskRef>> {
        match self {
            Self::Embedded(_) => None,
            #[cfg(unix)]
            Self::Remote(r) => {
                // El SDK entrega tasks REMOTAS; un frontend habla de
                // `TaskRef` y no quiere saber de dónde vino. El puente es
                // una task de reenvío porque un canal no se puede mapear en
                // el sitio: muere cuando muere el canal de origen, así que
                // no sobrevive a la conexión que lo alimentaba.
                let mut origen = r.take_foreign_tasks()?;
                let (tx, rx) = mpsc::unbounded_channel();
                tokio::spawn(async move {
                    while let Some(t) = origen.recv().await {
                        if tx.send(TaskRef::from(t)).is_err() {
                            break;
                        }
                    }
                });
                Some(rx)
            }
        }
    }

    /// Canal de eventos de conexión (aviso de reconexión). `None` en
    /// embebido o si ya se tomó. Solo el dueño original de la conexión debe
    /// llamarlo; un clon (scripting) no.
    pub fn take_conn_events(&mut self) -> Option<mpsc::UnboundedReceiver<ConnEvent>> {
        match self {
            Self::Embedded(_) => None,
            #[cfg(unix)]
            Self::Remote(r) => r.take_conn_events(),
        }
    }

    /// Canal de aprobaciones de policy pendientes (M3-3b T5): cada
    /// `policy.approval_required` del daemon (y el resync por
    /// `policy.pending` al (re)conectar) llega aquí para que el frontend
    /// pregunte al humano. `None` en embebido (sin agentes que aprobar por
    /// esta vía) o si ya se tomó. Solo el dueño original de la conexión debe
    /// llamarlo; un clon (scripting) no.
    pub fn take_approvals(
        &mut self,
    ) -> Option<mpsc::UnboundedReceiver<norte_proto::methods::PolicyApprovalRequired>> {
        match self {
            Self::Embedded(_) => None,
            #[cfg(unix)]
            Self::Remote(r) => r.take_approvals(),
        }
    }

    /// Receptor de avisos `connection.degraded` (#44). En `Remote` viene del
    /// pump del daemon; en `Embedded` INSTALA un observer en el engine que
    /// empuja a un canal — así AMBOS modos surfacean la degradación de forma
    /// uniforme (rust MAJOR M1 + security m1: antes el embebido era silencioso).
    /// One-shot por su naturaleza (instala/toma una vez); en `Embedded` el
    /// aviso es SÍNCRONO (el observer dispara dentro del `provider_for` del
    /// comando en curso), así que un drenado posterior lo ve sin carrera.
    pub fn take_degraded(
        &mut self,
    ) -> Option<mpsc::UnboundedReceiver<norte_proto::methods::ConnectionDegraded>> {
        match self {
            Self::Embedded(engine) => {
                let (tx, rx) = mpsc::unbounded_channel();
                // Encadenando: la ranura es de UNO y este `take_*` no puede
                // dejar mudo al del otro hecho (#322).
                engine.chain_connection_observer(|previo| {
                    Arc::new(ChannelConnectionObserver { tx, previo })
                });
                Some(rx)
            }
            #[cfg(unix)]
            Self::Remote(r) => r.take_degraded(),
        }
    }

    /// Receptor de fallos `connection.failed` (#322): POR QUÉ una conexión NO
    /// se abrió. Gemelo de [`Backend::take_degraded`] y con el mismo trato en
    /// los dos brazos — en `Remote` viene del pump del daemon, en `Embedded`
    /// instala un observer.
    ///
    /// Existe en `Embedded` y no solo en `Remote` porque el diagnóstico se
    /// perdía en los DOS: en el daemon se quedaba en su log, y en el embebido
    /// salía por el stderr del propio proceso — que en la TUI se lo come la
    /// pantalla alternativa. Un fallo que se diagnostica o no según el
    /// transporte es la peor forma de que dependa.
    ///
    /// One-shot en `Remote`, donde el receptor se lo lleva el primer dueño. En
    /// `Embedded` NO lo es —igual que [`Backend::take_degraded`]—: cada
    /// llamada encadena otro observer y devuelve otro receptor, y el que nadie
    /// drene es un canal sin techo que solo crece. Llámalo UNA vez, en el
    /// arranque.
    ///
    /// Por `&self` y no `&mut self` como [`Backend::take_degraded`]: ninguna
    /// de las dos ramas lo necesitaba, y `norte connect` —el comando que se
    /// teclea justo para diagnosticar esto— tiene el backend por referencia
    /// compartida. Pedir `&mut` habría dejado fuera al único sitio donde el
    /// humano está preguntando explícitamente «¿por qué no entra?».
    pub fn take_failed(
        &self,
    ) -> Option<mpsc::UnboundedReceiver<norte_proto::methods::ConnectionFailed>> {
        match self {
            Self::Embedded(engine) => {
                let (tx, rx) = mpsc::unbounded_channel();
                engine.chain_connection_observer(|previo| {
                    Arc::new(ChannelFailureObserver { tx, previo })
                });
                Some(rx)
            }
            #[cfg(unix)]
            Self::Remote(r) => r.take_failed(),
        }
    }

    /// Receptor de avisos `plugin.notice` (0.69.0, ADR 0100): la frase de un
    /// plugin `hook` sobre una mutación ya registrada, o que los hooks de un
    /// plugin se apagaron tras tres fallos. En `Remote` viene del pump del
    /// daemon; en `Embedded` ARRANCA el despachador de hooks sobre el journal
    /// de este engine y le da un canal — así ambos modos corren los mismos
    /// hooks y surfacean lo mismo. One-shot, como [`Backend::take_failed`]:
    /// el engine tiene UN hueco para el despachador y el segundo que lo pida
    /// recibe `None`, en vez de arrancar otro que pisara al primero.
    ///
    /// `None` también en `Embedded` si el engine no lleva journal (sin filas
    /// no hay hooks) o si el runtime WASM no se pudo crear: sin runtime no
    /// corre ningún plugin, y tampoco un hook (fail-closed, con traza). El
    /// despachador muere solo cuando el receptor devuelto se suelta.
    ///
    /// # Panics
    /// Fuera de un runtime de tokio: arranca tasks.
    pub fn take_plugin_notices(
        &self,
    ) -> Option<mpsc::UnboundedReceiver<norte_proto::methods::PluginNotice>> {
        match self {
            Self::Embedded(engine) => {
                if !engine.has_journal() || !engine.claim_hooks_slot() {
                    return None;
                }
                let runtime = match norte_plugin_host::PluginRuntime::new() {
                    Ok(r) => Arc::new(r),
                    Err(e) => {
                        tracing::warn!(error = %e, "hooks: sin runtime de plugins, no corren");
                        return None;
                    }
                };
                let (tx, rx) = mpsc::unbounded_channel();
                // Enchufar el journal es `async` (el perezoso guarda el
                // extremo bajo su lock) y leer `policy.toml` es I/O (regla 2):
                // las dos cosas en una task. Una mutación que se adelante
                // queda sin hook, y es el arranque: no hay ninguna. Sin token
                // de cancelación propio: la vida del despachador embebido es
                // la del receptor (`is_closed`), y el proceso que lo hospeda
                // termina con él.
                let engine = Arc::clone(engine);
                tokio::spawn(async move {
                    // Las reglas del humano valen también aquí (ADR 0101): el
                    // engine embebido no lleva gate, así que el despachador
                    // las mira para el actor `plugin`. Un fichero ilegible se
                    // dice y equivale a ninguno.
                    let policy = crate::blocking::spawn_blocking(crate::PolicyConfig::load)
                        .await
                        .ok()
                        .and_then(|r| match r {
                            Ok(p) => Some(Arc::new(p)),
                            Err(e) => {
                                tracing::warn!(error = %e, "hooks: policy.toml ilegible, sin reglas");
                                None
                            }
                        });
                    let (sender, _task) = crate::hooks::spawn_dispatcher(
                        crate::connect::config_dir(),
                        runtime,
                        Arc::new(ChannelHookSink { tx }),
                        tokio_util::sync::CancellationToken::new(),
                        Some(crate::hooks::SidecarWriter {
                            engine: Arc::downgrade(&engine),
                            scopes: None,
                            policy,
                        }),
                    );
                    engine.enable_hooks(sender).await;
                });
                Some(rx)
            }
            #[cfg(unix)]
            Self::Remote(r) => r.take_plugin_notices(),
        }
    }

    /// Receptor del aviso «esta sesión NO queda registrada en el journal»
    /// (#167/#177). Solo `Embedded` puede quedarse sin journal —el daemon se
    /// niega a arrancar sin él—, así que en `Remote` es `None`.
    ///
    /// Como [`Backend::take_degraded`], en `Embedded` INSTALA el sink en el
    /// engine en vez de tomar un canal ya hecho. Llamarlo en el arranque, antes
    /// de la primera mutación; y si una mutación se adelanta igual, el aviso no
    /// se pierde (el `LazyJournal` lo retiene hasta que hay sink).
    ///
    /// Llegan PÉRDIDAS Y RECUPERACIONES (#179): la ventana de propiedad se
    /// puede reabrir, así que un frontend que solo escuche
    /// [`JournalStatus::Lost`](crate::embedded::JournalStatus::Lost) acaba
    /// pintando «esta sesión no se registra» sobre una que sí.
    ///
    /// `None` también si el engine embebido no lleva journal perezoso —uno
    /// construido con `Engine::new()`, que no journaliza NADA y nunca va a
    /// avisar de ello—: devolver un canal ahí sería decirle al frontend que
    /// está cubierto por un aviso que no puede llegar.
    ///
    /// UNA sola vez, como sus hermanos: un segundo sink deja mudo al primer
    /// receptor (ver [`crate::embedded::LazyJournal::set_warning_sink`]).
    pub fn take_journal_warnings(
        &mut self,
    ) -> Option<mpsc::UnboundedReceiver<crate::embedded::JournalStatus>> {
        match self {
            Self::Embedded(engine) => {
                let (tx, rx) = mpsc::unbounded_channel();
                engine
                    .set_journal_warning_sink(Arc::new(ChannelJournalSink { tx }))
                    .then_some(rx)
            }
            #[cfg(unix)]
            Self::Remote(_) => None,
        }
    }
}
