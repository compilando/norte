//! Persistir y restaurar la sesión de UI (#230, #235, #236).
//!
//! Vivía en el root del binario `ntc`, que es un crate DISTINTO de esta lib:
//! nada de esto se podía importar desde `tests/`, así que sus tests tenían que
//! ser un `#[cfg(test)] mod` dentro de `main.rs`. Movido tal cual —mismas
//! firmas, mismo orden, mismos comentarios— sin más cambio que el `pub` de lo
//! que el bucle de eventos llama.

use std::sync::Arc;

use norte_core::backend::Backend;
use norte_i18n::{t, ta};
use norte_proto::{Error, VPath};

use crate::app::App;
use crate::listing::initial_pane;

/// Trae la sesión guardada y la pone en pantalla (L2).
///
/// Tres cosas y en este orden: se pregunta quién es la dueña, se aplica el
/// cuerpo, y se listan los huecos que la sesión colocó —hasta que llega el
/// listado, el cursor guardado no tiene dónde ponerse—.
///
/// Nada de esto puede impedir arrancar. Un core que no sabe de sesiones, una
/// sesión ilegible o un directorio que ya no existe dejan lo que había: la
/// disposición de la configuración, que es lo que se tenía antes de que esto
/// existiera.
///
/// `start` es el DIR de la línea de órdenes cuando lo hubo: gana a la sesión
/// en el panel activo ([`App::pin_start_dir`]), y se lista con los demás.
pub async fn restore_session(app: &mut App, backend: &Backend, start: Option<&VPath>) {
    let (sesion, dueña) = match backend.session_get().await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(error = %e, "sin sesión guardada");
            return;
        }
    };
    app.session.detached = !dueña;
    app.session.revision = sesion.revision;
    if !dueña {
        app.message = Some(t("msg-session-detached"));
    }
    // Revisión 0 es «nadie la ha escrito todavía»: no hay nada que aplicar y
    // tampoco nada roto que contar.
    if sesion.revision == 0 {
        return;
    }
    // La versión del SOBRE, que es la que el protocolo documenta (#247): el
    // cuerpo llevaba una copia sin documentar y era la única que se leía, así
    // que un cliente ajeno que hiciera lo que dice el contrato tenía su cuerpo
    // interpretado como si fuera de la versión 0.
    app.apply_session_value(sesion.version, &sesion.body);
    if let Some(dir) = start {
        app.pin_start_dir(dir.clone());
    }
    restore_slots(app, backend, RESTORE_BUDGET).await;
}

/// Cuánto puede llevar el journal sin usarse antes de que esta sesión lo
/// suelte (#179).
///
/// Treinta segundos: bastante como para que una ráfaga de copias no pague un
/// cierre y una reapertura entre dos, y poco como para que un `ntc` abierto
/// toda la tarde no bloquee a `norte daemon run` ni a `norte audit` más allá
/// del rato en que de verdad estuvo escribiendo.
pub const JOURNAL_IDLE: std::time::Duration = std::time::Duration::from_secs(30);

/// Lo que el arranque dedica ENTERO a listar los huecos de la sesión (#235).
///
/// Cinco segundos para TODOS los huecos, no cinco por hueco: lo que se acota
/// es cuánto puede tardar la ventana en aparecer, y eso no depende de cuántos
/// huecos tenga la disposición.
const RESTORE_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// Rellena los huecos de la sesión con un listado real, dentro de `presupuesto`
/// (#235).
///
/// Esto corre ANTES de que exista el bucle de eventos: no hay `Ctrl+C`
/// cableado todavía, así que un hueco sobre un SFTP muerto colgaba el arranque
/// entero y la única salida era otra terminal. La regla 3 es sobre esto mismo,
/// en un camino anterior a la maquinaria de tasks.
///
/// **Los listados van EN PARALELO bajo un plazo común**, y eso es lo que hace
/// que el presupuesto sea del arranque y no de cada hueco. En serie, un solo
/// panel aparcado en un host caído se comía los cinco segundos enteros y los
/// otros tres —locales, de milisegundos— se quedaban sin listar por haber
/// llegado tarde a un reparto que nunca fue suyo: la forma corriente del bug
/// (un remoto muerto entre locales) dejaba media pantalla en blanco en cada
/// arranque mientras durase la avería.
///
/// Lo que no se pudo listar se queda sobre su ruta y **marcado**
/// ([`Pane::unlisted`]): una pantalla con listados vacíos y sin explicación
/// afirma que esos directorios están vacíos, que es justo lo que no se sabe.
/// La marca dura hasta que alguien liste de verdad, porque el estado dura
/// hasta entonces.
async fn restore_slots(app: &mut App, backend: &Backend, presupuesto: std::time::Duration) {
    let deadline = tokio::time::Instant::now() + presupuesto;
    // Se recogen primero las peticiones: el `&mut App` de la aplicación no
    // puede vivir dentro de los futures.
    let requests: Vec<(norte_frontend::layout::SlotId, VPath, Vec<String>)> = app
        .layout
        .slot_ids()
        .into_iter()
        .filter_map(|id| {
            let dir = app.panes.browser(id).map(|p| p.dir().clone())?;
            let attrs = app.columns.attr_ids_for(dir.scheme());
            Some((id, dir, attrs))
        })
        .collect();
    let listed = futures::future::join_all(requests.into_iter().map(|(id, dir, attrs)| {
        let backend = backend.clone();
        async move {
            let r = tokio::time::timeout_at(deadline, initial_pane(&backend, &dir, &attrs)).await;
            (id, r)
        }
    }))
    .await;

    for (id, listado) in listed {
        let Ok(listado) = listado else {
            tracing::warn!("un hueco de la sesión no listó dentro del presupuesto");
            if let Some(p) = app.panes.browser_mut(id) {
                p.unlisted = true;
            }
            continue;
        };
        match listado {
            Ok(pane) => {
                // El orden y los ocultos son de la SESIÓN, no del listado
                // nuevo: se conservan al reemplazar el pane.
                let (sort, hidden) = app
                    .panes
                    .browser(id)
                    .map_or((None, None), |p| (Some(p.sort()), Some(p.show_hidden())));
                // Por la puerta de adopción: el orden y los ocultos son de la
                // SESIÓN y se reponen aquí; la fila `..` es de la config y la
                // pone `adoptar_pane`. Insertándolo a pelo, la fila se caía en
                // cada arranque con sesión guardada.
                app.adoptar_pane(id, pane);
                if let Some(p) = app.panes.browser_mut(id) {
                    if let Some(s) = sort {
                        p.set_sort(s);
                    }
                    if let Some(h) = hidden {
                        p.set_show_hidden(h);
                    }
                }
                app.restore_cursor(id);
            }
            // Un directorio que ya no está NO deja el arranque a medias: el
            // pane se queda vacío en esa ruta y el lector navega desde ahí,
            // que es lo mismo que pasa si lo borran contigo dentro.
            //
            // Pero se MARCA, y esto se corrigió: la marca no era por «no dio
            // tiempo», es por «esto no es el contenido del directorio». Un
            // listado que falló y un directorio vacío se pintaban idénticos, y
            // el caso corriente no es un directorio borrado — es una conexión
            // remota que al reabrir pide su contraseña. El lector veía un
            // panel vacío sobre `s3://…` y nada más: ni el motivo, ni que
            // hubiera algo que hacer.
            //
            // No se pregunta aquí. Restaurar una sesión no es pedir
            // conectarse, y una contraseña pedida antes de que la pantalla
            // exista, por algo que nadie acaba de hacer, es la forma que el
            // ADR 0015 llama phishing. La pregunta la abre el primer gesto
            // sobre ese panel — `pane.refresh` o navegar.
            Err(e) => {
                tracing::warn!(error = %e, "un hueco de la sesión no se pudo listar");
                if let Some(p) = app.panes.browser_mut(id) {
                    p.unlisted = true;
                }
                if let Error::SecretNeeded { conn, .. } = &e {
                    // Y se DICE cuál, porque con dos paneles remotos «hace
                    // falta una contraseña» no es contestable.
                    app.message = Some(ta(
                        "msg-session-secret-needed",
                        &[("conn", &norte_frontend::display_name(conn.as_bytes()).0)],
                    ));
                }
            }
        }
    }
}

/// Cada cuántos ticks una ventana SUELTA vuelve a preguntar si ya puede
/// escribir (#234).
///
/// Treinta segundos. No hay notificación que avise —no la hay a propósito: el
/// único que puede escribir es el que cambió algo, así que un
/// `session.changed` no tendría destinatario correcto— y sin volver a
/// preguntar, la ventana que sobrevive a la dueña no guarda nada nunca más y
/// su pantalla muere con ella. Preguntar cada segundo sería un viaje por
/// segundo para siempre a cambio de enterarse antes de algo que pasa una vez.
const OWNER_RETRY_TICKS: u32 = 30;

/// Lo que dura la espera por el último volcado al salir.
///
/// Salir no se cuelga por una sesión: si el core no contesta, se pierde la
/// última foto y ya.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Lo que la pantalla le manda al escritor de la sesión.
enum SessionOrder {
    /// Escribe esto.
    ///
    /// `Arc` porque el cuerpo lleva el árbol y el estado de cada hueco: la
    /// pantalla lo comparte con el escritor en vez de copiarle hasta 1 MiB una
    /// vez por segundo, y de paso la variante no engorda el enum
    /// (`clippy::large_enum_variant`).
    Write(Arc<norte_frontend::session::SessionBody>),
    /// ¿Ya puedo escribir? La hace una ventana suelta cada [`OWNER_RETRY_TICKS`]
    /// ticks.
    Ask,
}

/// Lo que el escritor le cuenta a la pantalla.
enum SessionNotice {
    /// No cabía; se ha tirado el historial. Se dice UNA vez.
    TooLarge,
    /// El cuerpo no llegó porque otra ventana escribió antes.
    ///
    /// `orphans` son los huecos que ELLA guardaba y esta pantalla no tenía:
    /// vuelven aquí en vez de tirarse, porque el único camino que llega a este
    /// aviso es un relevo de propiedad, o sea justo cuando lo guardado NO es
    /// nuestro (#231).
    Retry {
        /// Los huecos ajenos que había que conservar.
        orphans: std::collections::BTreeMap<u32, norte_frontend::session::SlotState>,
    },
    /// Esta ventana ya es la dueña: puede volver a escribir desde la revisión
    /// que viene.
    Owner {
        /// La revisión vigente en el momento de tomarla.
        revision: u64,
        /// Los huecos que guardaba quien la tenía y esta pantalla no conoce.
        ///
        /// Un relevo NO pasa por `Conflict` —la revisión que se adopta es
        /// justo la vigente, así que la siguiente escritura encaja— y ese era
        /// el agujero: la ventana que tomaba el relevo pisaba en su primer tick
        /// todo lo que la otra hubiera guardado mientras ésta corría suelta.
        orphans: std::collections::BTreeMap<u32, norte_frontend::session::SlotState>,
    },
    /// Esta ventana ha DEJADO de ser la dueña: otra la tiene, o el daemon que
    /// la atendía se fue y la conexión nueva no reclamó nada.
    ///
    /// Sin este aviso, el escritor se apagaba solo y nadie volvía a preguntar:
    /// tras un relevo de daemon la ventana dejaba de guardar para el resto de
    /// su vida, creyéndose la dueña y sin decir una palabra.
    Released,
}

/// El lado de la PANTALLA del escritor de sesión (L2).
///
/// Lo que era una función `async` dentro del `select!` es ahora un canal, y el
/// motivo es medible: el volcado acaba en un `fsync` (brazo embebido) o en un
/// viaje por el socket (daemon), y mientras eso estaba en vuelo el bucle de
/// eventos no procesaba una tecla. Una vez por segundo, y justo mientras
/// navegas, que es cuando el cuerpo cambia. Ahora el bucle solo hace
/// `try_send` y `try_recv`: **esta struct no tiene ni un `await`, y por eso el
/// arreglo no se puede deshacer sin que se note** (#230).
pub struct SessionPush {
    /// Qué se manda, qué no se repite y cuándo se vuelve a pedir la
    /// propiedad. Vive en `norte-frontend` (#236): es política de sesión, no
    /// del bucle de eventos de la TUI, y el siguiente frontend la hereda en
    /// vez de reinventarla.
    policy: norte_frontend::session::PushPolicy,
    /// Hacia el escritor. Capacidad 1: si está ocupado, este tick se salta, que
    /// es coalescing y no pérdida —el cuerpo siguiente lleva lo mismo y más—.
    ordenes: tokio::sync::mpsc::Sender<SessionOrder>,
    /// Desde el escritor.
    avisos: tokio::sync::mpsc::Receiver<SessionNotice>,
    /// El escritor, para esperarlo al salir.
    task: Option<tokio::task::JoinHandle<()>>,
}

impl SessionPush {
    /// El lado de la pantalla SIN escritor, para probar lo que decide el bucle
    /// sin un core al otro lado.
    ///
    /// Devuelve los dos extremos que se queda el escritor de verdad, así que un
    /// test puede leer lo que se manda y fingir lo que se contesta.
    #[cfg(test)]
    fn for_test() -> (
        Self,
        tokio::sync::mpsc::Receiver<SessionOrder>,
        tokio::sync::mpsc::Sender<SessionNotice>,
    ) {
        let (ordenes_tx, ordenes_rx) = tokio::sync::mpsc::channel(1);
        let (avisos_tx, avisos_rx) = tokio::sync::mpsc::channel(4);
        (
            Self {
                policy: norte_frontend::session::PushPolicy::new(OWNER_RETRY_TICKS),
                ordenes: ordenes_tx,
                avisos: avisos_rx,
                task: None,
            },
            ordenes_rx,
            avisos_tx,
        )
    }

    /// Arranca el escritor de la sesión de este run.
    pub fn start(backend: &Backend, revision: u64) -> Self {
        let (ordenes_tx, ordenes_rx) = tokio::sync::mpsc::channel(1);
        let (avisos_tx, avisos_rx) = tokio::sync::mpsc::channel(4);
        let b = backend.clone();
        let task = tokio::spawn(write_session(b, revision, ordenes_rx, avisos_tx));
        Self {
            policy: norte_frontend::session::PushPolicy::new(OWNER_RETRY_TICKS),
            ordenes: ordenes_tx,
            avisos: avisos_rx,
            task: Some(task),
        }
    }

    /// Manda la última foto, suelta el canal y espera al escritor.
    ///
    /// La foto va con `send` y un plazo, no con `try_send`: al salir no hay un
    /// «tick siguiente» que lo reintente, así que con el escritor ocupado —un
    /// `fsync` lento, un daemon parado— un `try_send` habría tirado justo la
    /// escritura que este camino existe para no perder.
    pub async fn close(&mut self, last: Option<Arc<norte_frontend::session::SessionBody>>) {
        if let Some(body) = last {
            let _ =
                tokio::time::timeout(SHUTDOWN_GRACE, self.ordenes.send(SessionOrder::Write(body)))
                    .await;
        }
        let (empty, _) = tokio::sync::mpsc::channel(1);
        // Soltar el emisor es lo que termina el bucle del escritor.
        self.ordenes = empty;
        if let Some(task) = self.task.take() {
            let _ = tokio::time::timeout(SHUTDOWN_GRACE, task).await;
        }
    }
}

/// El escritor de la sesión: el ÚNICO que habla con el core de esto.
///
/// Tiene la revisión, el recorte y la parada porque es quien ve las respuestas.
/// Las dos negativas se contestan distinto y por eso están aquí y no en el
/// `Backend`: un conflicto se arregla releyendo —otra ventana escribió— y un
/// exceso de tamaño se arregla tirando historial, que es lo que más ocupa y lo
/// que menos duele perder.
async fn write_session(
    backend: Backend,
    mut revision: u64,
    mut ordenes: tokio::sync::mpsc::Receiver<SessionOrder>,
    avisos: tokio::sync::mpsc::Sender<SessionNotice>,
) {
    use norte_frontend::session::{SCHEMA_VERSION, SessionBody};

    // Ya se supo que no cabe: a partir de aquí se escribe SIN historial.
    // Recortar solo la copia de un tick era no recortar nada — el tick
    // siguiente volvía a capturar el historial entero y lo que salía era un
    // `put` rehusado y un aviso POR SEGUNDO.
    let mut truncating = false;
    // No cupo ni sin historial: se deja de escribir en este run.
    let mut stopped = false;
    // Lo último que se mandó a escribir, para saber qué de un documento ajeno
    // no teníamos.
    let mut last: Option<SessionBody> = None;
    while let Some(order) = ordenes.recv().await {
        let mut body = match order {
            SessionOrder::Ask => {
                if let Ok((sesion, duena)) = backend.session_get().await
                    && duena
                {
                    revision = sesion.revision;
                    stopped = false;
                    // El documento que había: lo que guardó quien tenía la
                    // sesión y esta pantalla no conoce viaja de vuelta, o el
                    // primer volcado del relevo se lo lleva por delante.
                    let orphans = SessionBody::from_value(sesion.version, &sesion.body)
                        .map(|remote| {
                            foreign_orphans(
                                last.as_ref().unwrap_or(&SessionBody::default()),
                                &remote,
                            )
                        })
                        .unwrap_or_default();
                    let _ = avisos
                        .send(SessionNotice::Owner { revision, orphans })
                        .await;
                }
                continue;
            }
            SessionOrder::Write(body) => (*body).clone(),
        };
        if stopped {
            continue;
        }
        if truncating {
            body.degrade_for_size();
        }
        match backend
            .session_put(SCHEMA_VERSION, revision, body.to_value())
            .await
        {
            Ok(rev) => {
                revision = rev;
                last = Some(body);
            }
            // Otra ventana escribió entre nuestro último `get` y este `put`.
            // Se re-lee para saber contra qué, y lo que ella guardaba y esta
            // pantalla no tiene se conserva por DOS vías: se mete en el cuerpo
            // que se reintenta ahora mismo —si esto es el último volcado, no
            // hay un «luego»— y se le devuelve a la pantalla, que es quien
            // tiene que llevarlo en los siguientes (#231).
            Err(Error::Conflict { .. }) => {
                let mut orphans = std::collections::BTreeMap::new();
                if let Ok((sesion, _)) = backend.session_get().await {
                    revision = sesion.revision;
                    if let Ok(remote) = SessionBody::from_value(sesion.version, &sesion.body) {
                        orphans = foreign_orphans(&body, &remote);
                        for (id, estado) in &orphans {
                            body.slots.insert(*id, estado.clone());
                        }
                    }
                    // Un reintento INMEDIATO y uno solo: con la revisión de
                    // verdad delante, no reintentarlo aquí dejaba la última
                    // foto de una salida en el aire.
                    if let Ok(rev) = backend
                        .session_put(SCHEMA_VERSION, revision, body.to_value())
                        .await
                    {
                        revision = rev;
                        last = Some(body);
                    }
                }
                let _ = avisos.send(SessionNotice::Retry { orphans }).await;
            }
            Err(Error::LimitExceeded { .. }) => {
                if truncating {
                    // Ni sin historial cabe: reintentarlo cada segundo sería un
                    // error por segundo.
                    stopped = true;
                } else {
                    truncating = true;
                    // La decisión de QUÉ se tira es compartida (#316): la
                    // ventana degrada con la misma, y antes no degradaba.
                    body.degrade_for_size();
                    let _ = avisos.send(SessionNotice::TooLarge).await;
                    match backend
                        .session_put(SCHEMA_VERSION, revision, body.to_value())
                        .await
                    {
                        Ok(rev) => revision = rev,
                        Err(_) => stopped = true,
                    }
                }
            }
            // Esta ventana ya no escribe: perdió la propiedad, o el daemon se
            // está apagando (o se fue y la conexión nueva no reclamó nada).
            // Se para Y SE DICE: sin el aviso nadie volvía a preguntar nunca
            // —`Ask` solo sale de una ventana que se sabe suelta— y la
            // pantalla se perdía en silencio tras cualquier relevo de daemon.
            Err(Error::PermissionDenied | Error::Cancelled) => {
                stopped = true;
                let _ = avisos.send(SessionNotice::Released).await;
            }
            // Cualquier otro fallo —transporte caído, un `Io`, un plazo— NO da
            // el cuerpo por escrito: la pantalla lo dio por mandado al meterlo
            // en el canal, así que sin esto se perdía hasta que el lector
            // volviera a mover algo.
            Err(e) => {
                tracing::debug!(error = %e, "la sesión no se pudo escribir");
                let _ = avisos
                    .send(SessionNotice::Retry {
                        orphans: std::collections::BTreeMap::new(),
                    })
                    .await;
            }
        }
    }
}

/// Los huecos que `remote` guarda y `local` no tiene.
///
/// Es lo que hay que conservar de un cuerpo ajeno: nuestros huecos son los
/// buenos —esta pantalla es la que acaba de moverse— pero los que solo están
/// en el suyo no los conoce nadie más, y tirarlos es tirar el historial de un
/// panel al que su dueña iba a volver.
fn foreign_orphans(
    local: &norte_frontend::session::SessionBody,
    remote: &norte_frontend::session::SessionBody,
) -> std::collections::BTreeMap<u32, norte_frontend::session::SlotState> {
    remote
        .slots
        .iter()
        .filter(|(id, _)| !local.slots.contains_key(*id))
        .map(|(id, s)| (*id, s.clone()))
        .collect()
}

/// Manda la sesión a escribir si ha cambiado, y atiende lo que el escritor
/// tenga que decir (L2).
///
/// Se llama una vez por segundo. Coalescer es el punto: el cursor se mueve en
/// cada flecha, y esto acaba en un fichero.
///
/// **No es `async`, y eso es el arreglo de #230.** Todo lo que puede tardar
/// —el `put`, el `fsync`, el viaje por el socket— vive en
/// `write_session` (privada); aquí solo se captura, se compara y se empuja por un
/// canal. Volver a poner un `await` en esta función es volver a trabar el
/// bucle de eventos una vez por segundo.
pub fn push_session(app: &mut App, st: &mut SessionPush) {
    drain_notices(app, st);
    // La decisión —suelta, tapada por un modal, o toca capturar— es de la
    // política; la fontanería del canal es de aquí.
    match st.policy.tick(app.session.detached, app.modal.is_some()) {
        norte_frontend::session::PushStep::Skip => return,
        norte_frontend::session::PushStep::Ask => {
            let _ = st.ordenes.try_send(SessionOrder::Ask);
            return;
        }
        norte_frontend::session::PushStep::Capture => {}
    }
    let Some(body) = capture_session(app, st) else {
        return;
    };
    // `try_send` y no `send`: con el escritor ocupado, este tick se salta y el
    // siguiente manda un cuerpo más nuevo. Y `last` solo se actualiza si de
    // verdad se mandó, o un cuerpo saltado se daría por escrito.
    match st.ordenes.try_send(SessionOrder::Write(Arc::clone(&body))) {
        Ok(()) => st.policy.sent(body),
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
        // El escritor se murió (un panic dentro de la task). Sin esto la
        // pantalla reintentaba contra un canal cerrado el resto del run sin
        // decir una palabra.
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            tracing::warn!("el escritor de la sesión no está: esta ventana deja de guardar");
            app.session.detached = true;
            app.message = Some(t("msg-session-detached"));
        }
    }
}

/// Lo que el escritor contó desde la última vuelta.
pub fn drain_notices(app: &mut App, st: &mut SessionPush) {
    while let Ok(notice) = st.avisos.try_recv() {
        match notice {
            SessionNotice::TooLarge => app.message = Some(t("msg-session-too-large")),
            SessionNotice::Retry { orphans } => {
                app.adopt_session_orphans(orphans);
                // Lo mandado no llegó: que la comparación no lo dé por escrito.
                st.policy.resend();
            }
            SessionNotice::Owner { revision, orphans } => {
                app.session.detached = false;
                app.session.revision = revision;
                app.adopt_session_orphans(orphans);
                app.message = Some(t("msg-session-owned"));
            }
            SessionNotice::Released => {
                app.session.detached = true;
                app.message = Some(t("msg-session-detached"));
                // Se vuelve a preguntar en el tick siguiente y no dentro de
                // treinta segundos: esto suele ser un relevo de daemon, y la
                // sesión ya está libre.
                st.policy.ask_soon();
            }
        }
    }
}

/// La pantalla de AHORA, si ha cambiado desde lo último que se mandó.
///
/// `Arc` y no un `Box` clonado: el cuerpo puede llegar a 1 MiB y esto corre en
/// el bucle de eventos una vez por segundo. Compartirlo con el escritor no
/// cuesta nada; copiarlo sí.
pub fn capture_session(
    app: &mut App,
    st: &mut SessionPush,
) -> Option<Arc<norte_frontend::session::SessionBody>> {
    let now = now_ms();
    let mut body = app.session_body();
    let alive = app.layout.slot_ids();
    // La política recorta, compara y sella. Lo que queda aquí es lo único que
    // ella no puede hacer: llevar el mismo sello al estado de la pantalla,
    // porque capturar no puede TOCAR —dos capturas seguidas de la misma
    // pantalla tienen que dar el mismo documento, o el coalescing de un
    // segundo no coalesce nada.
    let sealed = st.policy.prepare(&mut body, &alive, now)?;
    for id in sealed {
        app.touch_session_slot(id, now);
    }
    Some(Arc::new(body))
}

/// Ahora, en milisegundos desde el epoch. Cero si el reloj del sistema está
/// antes de 1970, que solo hace que la barrida por edad no barra nada.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
#[cfg(test)]
mod session_push_tests {
    use super::*;
    use crate::app::Pane;

    fn app() -> App {
        let d = VPath::parse("file:///x").expect("wire de test");
        App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
    }

    fn slot(path: &str) -> norte_frontend::session::SlotState {
        norte_frontend::session::SlotState {
            path: VPath::parse(path).expect("wire de test"),
            cursor: 0,
            back: Vec::new(),
            forward: Vec::new(),
            sort: norte_frontend::SortSpec::default(),
            columns: Vec::new(),
            show_hidden: false,
            touched_ms: 7,
        }
    }

    /// #235: restaurar la sesión listaba TODOS los huecos en serie, sin plazo
    /// y ANTES de que existiera el bucle de eventos — así que un hueco sobre
    /// un SFTP muerto colgaba el arranque con `Ctrl+C` todavía sin cablear, y
    /// la única salida era otra terminal.
    ///
    /// Reloj de tokio pausado: la latencia del provider y el plazo son el
    /// mismo reloj virtual, así que esto es determinista y no duerme.
    #[tokio::test(start_paused = true)]
    async fn restaurar_la_sesion_no_puede_colgar_el_arranque() {
        use norte_core::backend::Backend;
        use std::sync::Arc;
        use std::time::Duration;

        let mem = norte_testkit::MemProvider::new();
        mem.faults()
            .set_latency_per_op(Some(Duration::from_hours(1)));
        let engine = norte_core::Engine::new();
        engine.register_provider(Arc::new(mem));
        let backend = Backend::Embedded(Arc::new(engine));

        let d = VPath::parse("mem:///").expect("wire de test");
        let mut app = App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()));
        // El conjunto esperado se calcula como lo calcula el código, no a
        // mano: si la disposición de fábrica gana un browser, esto sigue
        // diciendo la verdad en vez de fallar por una cadena.
        let browsers: Vec<_> = app
            .layout
            .slot_ids()
            .into_iter()
            .filter(|id| app.panes.browser(*id).is_some())
            .collect();
        assert!(browsers.len() >= 2, "la de fábrica tiene al menos dos");

        let budget = Duration::from_millis(50);
        let t0 = tokio::time::Instant::now();
        super::restore_slots(&mut app, &backend, budget).await;

        assert!(
            t0.elapsed() < Duration::from_secs(1),
            "el arranque se acota al presupuesto, no a la latencia del provider: {:?}",
            t0.elapsed()
        );
        // Y el plazo es COMÚN: en serie, el primer hueco se lo comía entero y
        // los demás ni se intentaban. Todos tienen que quedar marcados.
        for id in browsers {
            assert!(
                app.panes.browser(id).is_some_and(|p| p.unlisted),
                "el hueco {id:?} se queda marcado, no fingiendo un dir vacío"
            );
        }
    }

    /// Un hueco que falla al restaurar NO finge un directorio vacío, y si lo
    /// que falta es una contraseña lo DICE nombrando la conexión.
    ///
    /// Es el caso corriente al reabrir norte: el daemon anterior se apagó por
    /// inactividad y se llevó el secreto de sesión —vive solo en su memoria,
    /// ADR 0015—, así que el panel guardado sobre `s3://…` vuelve con
    /// `SecretNeeded`. Antes: un `warn!` al fichero y un panel vacío,
    /// indistinguible de un cubo sin objetos. Ni el motivo ni nada que hacer.
    ///
    /// No se PREGUNTA aquí, y es deliberado: restaurar una sesión no es pedir
    /// conectarse. La pregunta la abre el primer gesto sobre ese panel.
    #[tokio::test]
    async fn un_hueco_que_pide_secreto_al_restaurar_lo_dice_y_no_finge_vacio() {
        use norte_core::backend::Backend;
        use std::sync::Arc;

        /// Provider que solo sabe pedir la contraseña de `rosetta`.
        struct PideSecreto;

        #[async_trait::async_trait]
        impl norte_vfs::Provider for PideSecreto {
            fn scheme(&self) -> &'static str {
                "mem"
            }
            fn capabilities(&self) -> norte_proto::Capabilities {
                norte_proto::Capabilities {
                    flags: norte_proto::CapabilityFlags::empty(),
                    max_path: None,
                }
            }
            async fn stat(&self, _p: &VPath) -> Result<norte_proto::Entry, Error> {
                Err(Error::SecretNeeded {
                    conn: "rosetta".to_owned(),
                    endpoint: "s3://cubo.example".to_owned(),
                })
            }
            async fn list(&self, _p: &VPath) -> Result<norte_vfs::EntryStream, Error> {
                Err(Error::SecretNeeded {
                    conn: "rosetta".to_owned(),
                    endpoint: "s3://cubo.example".to_owned(),
                })
            }
            async fn read(
                &self,
                _p: &VPath,
                _r: Option<norte_proto::ByteRange>,
            ) -> Result<norte_vfs::ByteStream, Error> {
                Err(Error::Unsupported)
            }
            async fn write(&self, _p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
                Err(Error::Unsupported)
            }
            async fn mkdir(&self, _p: &VPath) -> Result<(), Error> {
                Err(Error::Unsupported)
            }
            async fn remove(&self, _p: &VPath) -> Result<(), Error> {
                Err(Error::Unsupported)
            }
            async fn rename(&self, _f: &VPath, _t: &VPath) -> Result<(), Error> {
                Err(Error::Unsupported)
            }
        }

        let engine = norte_core::Engine::new();
        engine.register_provider(Arc::new(PideSecreto));
        let backend = Backend::Embedded(Arc::new(engine));
        let d = VPath::parse("mem:///").expect("wire de test");
        let mut app = App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()));

        super::restore_slots(&mut app, &backend, std::time::Duration::from_secs(5)).await;

        let ids: Vec<_> = app
            .layout
            .slot_ids()
            .into_iter()
            .filter(|id| app.panes.browser(*id).is_some())
            .collect();
        for id in ids {
            assert!(
                app.panes.browser(id).is_some_and(|p| p.unlisted),
                "el hueco {id:?} finge un directorio vacío: un listado que \
                 falló y un cubo sin objetos se leían igual"
            );
        }
        let msg = app.message.clone().expect("se dice qué falta");
        assert!(
            msg.contains("rosetta"),
            "y CUÁL conexión: con dos paneles remotos, «hace falta una \
             contraseña» no es contestable. Decía: {msg}"
        );
        // Y NO se abrió ningún diálogo: restaurar no es pedir conectarse.
        assert!(
            app.modal.is_none(),
            "el arranque no pregunta solo; lo hace el primer gesto"
        );
    }

    /// Y la marca se APAGA en cuanto alguien lista de verdad: es un estado,
    /// no un aviso, así que ni la borra una tecla ni sobrevive al listado.
    #[test]
    fn la_marca_de_sin_listar_se_va_con_el_primer_listado() {
        let d = VPath::parse("mem:///").expect("wire de test");
        let mut p = Pane::new(d.clone(), Vec::new());
        p.unlisted = true;
        p.set_listing(d, Vec::new());
        assert!(!p.unlisted, "un listado real la apaga");
    }

    /// **#230, y es un test de FORMA**: `push_session` se llama desde un `#[test]`
    /// corriente, sin runtime y sin `await`. Si alguien le devuelve el `async`,
    /// esto no compila — que es exactamente la garantía que se quería, porque el
    /// coste de aquel `await` era una tecla perdida por segundo mientras se
    /// navegaba, y eso no lo enseña ningún assert.
    #[test]
    fn mandar_la_sesion_no_bloquea_el_bucle() {
        let mut app = app();
        let (mut st, mut ordenes, _avisos) = SessionPush::for_test();
        push_session(&mut app, &mut st);
        assert!(
            matches!(ordenes.try_recv(), Ok(SessionOrder::Write(_))),
            "la primera vuelta manda la pantalla"
        );
        // Y no se manda lo mismo dos veces: coalescer es el punto de todo esto.
        push_session(&mut app, &mut st);
        assert!(ordenes.try_recv().is_err(), "nada ha cambiado");
    }

    /// Una ventana SUELTA no escribe, pero vuelve a preguntar (#234): la dueña
    /// pudo cerrarse, y no hay notificación que lo cuente.
    #[test]
    fn una_ventana_suelta_no_escribe_y_vuelve_a_preguntar() {
        let mut app = app();
        app.session.detached = true;
        let (mut st, mut ordenes, _avisos) = SessionPush::for_test();
        for _ in 0..OWNER_RETRY_TICKS - 1 {
            push_session(&mut app, &mut st);
            assert!(ordenes.try_recv().is_err(), "suelta no escribe ni pregunta");
        }
        push_session(&mut app, &mut st);
        assert!(
            matches!(ordenes.try_recv(), Ok(SessionOrder::Ask)),
            "a los {OWNER_RETRY_TICKS} ticks pregunta"
        );
    }

    /// Y cuando el escritor dice que ya es la dueña, esta ventana vuelve a
    /// escribir desde la revisión que le den.
    #[test]
    fn al_tomar_la_propiedad_se_vuelve_a_escribir() {
        let mut app = app();
        app.session.detached = true;
        let (mut st, mut ordenes, avisos) = SessionPush::for_test();
        avisos
            .try_send(SessionNotice::Owner {
                revision: 9,
                orphans: std::collections::BTreeMap::new(),
            })
            .expect("cabe");
        push_session(&mut app, &mut st);
        assert!(!app.session.detached);
        assert_eq!(app.session.revision, 9);
        assert!(
            matches!(ordenes.try_recv(), Ok(SessionOrder::Write(_))),
            "y ya escribe"
        );
    }

    /// Un cuerpo que no llegó NO se da por escrito: sin esto, el conflicto de
    /// una sola vuelta dejaba la pantalla sin guardar hasta que el lector
    /// volviera a mover algo.
    #[test]
    fn lo_que_no_llego_se_vuelve_a_mandar() {
        let mut app = app();
        let (mut st, mut ordenes, avisos) = SessionPush::for_test();
        push_session(&mut app, &mut st);
        assert!(ordenes.try_recv().is_ok());
        avisos
            .try_send(SessionNotice::Retry {
                orphans: std::collections::BTreeMap::new(),
            })
            .expect("cabe");
        push_session(&mut app, &mut st);
        assert!(
            matches!(ordenes.try_recv(), Ok(SessionOrder::Write(_))),
            "se vuelve a mandar aunque la pantalla no haya cambiado"
        );
    }

    /// **La última foto al salir ESPERA su turno.**
    ///
    /// El canal tiene capacidad 1 y en la salida no hay un tick siguiente, así
    /// que mandarla con `try_send` la tiraba justo cuando el escritor estaba
    /// ocupado —un `fsync` lento, un daemon parado—, que es el caso para el que
    /// se añadió.
    #[tokio::test]
    async fn la_ultima_foto_al_salir_espera_su_turno() {
        let mut app = app();
        let (mut st, mut ordenes, _avisos) = SessionPush::for_test();
        // El escritor está ocupado: el canal ya lleva una orden sin consumir.
        st.ordenes.try_send(SessionOrder::Ask).expect("cabe una");
        let last = capture_session(&mut app, &mut st).expect("hay pantalla que guardar");
        let received = tokio::spawn(async move {
            let mut v = Vec::new();
            while let Some(o) = ordenes.recv().await {
                v.push(o);
            }
            v
        });
        st.close(Some(last)).await;
        let v = received.await.expect("join");
        assert_eq!(v.len(), 2, "la que ocupaba el canal y la última foto");
        assert!(matches!(v[1], SessionOrder::Write(_)));
    }

    /// Perder la propiedad a media vida se DICE, y se vuelve a preguntar en el
    /// tick siguiente.
    ///
    /// Es lo que pasa tras un relevo de daemon: la conexión nueva no ha
    /// reclamado nada, el `put` sale `PermissionDenied` y el escritor se apaga.
    /// Sin el aviso, la ventana se creía la dueña y no volvía a guardar en el
    /// resto de su vida — ni lo decía.
    #[test]
    fn perder_la_propiedad_se_dice_y_se_vuelve_a_preguntar() {
        let mut app = app();
        let (mut st, mut ordenes, avisos) = SessionPush::for_test();
        avisos.try_send(SessionNotice::Released).expect("cabe");
        push_session(&mut app, &mut st);
        assert!(app.session.detached, "esta ventana ya no manda");
        assert!(app.message.is_some(), "y lo dice");
        // Y en el tick siguiente pregunta, sin esperar los treinta segundos.
        push_session(&mut app, &mut st);
        assert!(matches!(ordenes.try_recv(), Ok(SessionOrder::Ask)));
    }

    /// **El relevo conserva lo que guardaba quien se fue.**
    ///
    /// Un relevo no pasa por `Conflict` —se adopta justo la revisión vigente,
    /// así que la siguiente escritura encaja—, y ese era el agujero: la ventana
    /// que tomaba la sesión pisaba en su primer volcado todo lo que la otra
    /// hubiera guardado mientras ésta corría suelta.
    #[test]
    fn al_tomar_el_relevo_no_se_pisa_lo_que_guardaba_la_otra() {
        let mut app = app();
        app.session.detached = true;
        let (mut st, _ordenes, avisos) = SessionPush::for_test();
        let mut orphans = std::collections::BTreeMap::new();
        orphans.insert(77, slot("file:///lo-suyo"));
        avisos
            .try_send(SessionNotice::Owner {
                revision: 5,
                orphans,
            })
            .expect("cabe");
        push_session(&mut app, &mut st);
        assert!(!app.session.detached);
        assert_eq!(app.session.revision, 5);
        assert_eq!(
            app.session_body().slots[&77].path,
            VPath::parse("file:///lo-suyo").expect("wire"),
            "lo de la otra ventana sigue ahí y se vuelve a escribir"
        );
    }

    /// Un escritor muerto no deja a la pantalla hablando sola: se dice y se
    /// deja de guardar.
    #[test]
    fn si_el_escritor_se_muere_la_pantalla_se_entera() {
        let mut app = app();
        let (mut st, ordenes, _avisos) = SessionPush::for_test();
        drop(ordenes);
        push_session(&mut app, &mut st);
        assert!(app.session.detached);
        assert!(app.message.is_some());
    }

    /// **#231**: de un cuerpo ajeno se conserva lo que solo estaba en él.    /// **#231**: de un cuerpo ajeno se conserva lo que solo estaba en él. Los
    /// huecos que el layout VIVO tiene son nuestros —esta pantalla es la que
    /// acaba de moverse—; los demás vuelven al rincón de huérfanos.
    #[test]
    fn de_un_conflicto_se_conservan_los_huecos_ajenos() {
        let mut local = norte_frontend::session::SessionBody::default();
        local.slots.insert(1, slot("file:///mio"));
        let mut remote = norte_frontend::session::SessionBody::default();
        remote.slots.insert(1, slot("file:///suyo"));
        remote.slots.insert(42, slot("file:///solo-suyo"));

        let foreign = foreign_orphans(&local, &remote);
        assert_eq!(foreign.len(), 1, "solo lo que no teníamos");
        assert!(foreign.contains_key(&42));

        let mut app = app();
        let alive = app.panes.slot_of(0).0;
        let mut with_alive = foreign.clone();
        with_alive.insert(alive, slot("file:///no-pises-mi-pantalla"));
        app.adopt_session_orphans(with_alive);
        let body = app.session_body();
        assert_eq!(
            body.slots[&42].path,
            VPath::parse("file:///solo-suyo").expect("wire"),
            "el huérfano ajeno se conserva y se vuelve a escribir"
        );
        assert_eq!(
            body.slots[&alive].path,
            VPath::parse("file:///x").expect("wire"),
            "y un hueco VIVO no lo pisa la sesión de otra ventana"
        );
    }
}
