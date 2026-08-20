//! El enrutado de los LOTES de un feed: quién recibe qué, y qué pasa cuando
//! el que recibe no drena.
//!
//! Un `fs.search`, un `fs.compare` y un `sync.plan` entregan su resultado en
//! lotes por notificación, y las notificaciones llegan por una sola conexión.
//! Aquí está la mesa de rutas —id de task → canal— con las tres cosas que la
//! hacen no perder nada: los lotes que llegan ANTES de que su ruta exista se
//! retienen (acotados), la retirada de una ruta espera una gracia por si el
//! lote final viene detrás del progreso terminal, y un consumidor que no
//! drena se corta a él, no a la conexión.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;

use super::Inner;

/// Buffer del canal de lotes de un feed remoto (`search.hits` de una
/// `fs.search`, `compare.rows` de una `fs.compare`). Absorbe el burst de
/// lotes que ya coalesció el daemon (`SEARCH_HITS_MAX_BATCH` /
/// `COMPARE_ROWS_MAX_BATCH` por lote) mientras el frontend drena; holgado
/// para que `try_send` no descarte por backpressure en el caso normal.
pub(super) const BATCH_BUF: usize = 64;

/// Tope de lotes retenidos SIN route (carrera de arranque: un lote puede
/// adelantar al registro del route). Acota la memoria ante un daemon que
/// emita lotes de `task_id`s que este proceso jamás registró.
///
/// Es POR FEED, no global: cada `BatchRoutes` lleva su propio `pending`, y
/// hay dos, así que lo retenido en el peor caso es el doble de este número.
pub(super) const BATCH_PENDING_CAP: usize = 64;

/// Gracia tras el terminal de un feed antes de retirar su route. En el
/// daemon la bomba de lotes y la de progreso son tasks INDEPENDIENTES que
/// escriben al mismo sink: un `search.hits`/`compare.rows` puede llegar
/// tras el `task.progress` terminal. La gracia deja que esos lotes
/// rezagados aún se enruten; pasada, el sender se suelta y `rx` se cierra
/// (parida con el embebido). Retirar en seco al terminal perdería el lote
/// rezagado.
pub(super) const BATCH_ROUTE_GRACE: Duration = Duration::from_millis(500);

/// Enrutado de los lotes de UN feed vivo (los `search.hits` de una
/// `fs.search`, las `compare.rows` de una `fs.compare`) por `task_id`.
/// Todo detrás de UN Mutex para que registrar el route (drenar lo
/// pendiente + insertar) sea ATÓMICO frente a la bomba — sin ventana en la
/// que un lote se pierda entre el drenaje y el insert.
///
/// Genérico en el lote y no duplicado por feed: los dos tienen el mismo
/// ciclo de vida (route, carrera de arranque, gracia tras el terminal) y
/// dos copias del mismo razonamiento sutil se desincronizan.
pub(super) struct BatchRoutes<T> {
    /// `task_id` → sender del `rx` que devolvió el método que lo lanzó.
    pub(super) routes: HashMap<u64, mpsc::Sender<T>>,
    /// Lotes llegados ANTES de que su route se registrara (carrera de
    /// arranque): el registro los drena en orden. Acotado por
    /// [`BATCH_PENDING_CAP`] lotes en total.
    pub(super) pending: HashMap<u64, Vec<T>>,
    /// `task_id`s cuyo `task.progress` TERMINAL ya se vio. El terminal puede
    /// ADELANTAR al registro del route (el frame sale del daemon antes que
    /// la respuesta de `fs.search`, y en el cliente la bomba y `search`
    /// corren en paralelo): sin esto, la retirada del route se perdería y el
    /// `rx` no se cerraría jamás. Espejo del anillo `finished` de `own_task`.
    /// Acotado; una entrada se limpia al retirar su route.
    pub(super) terminated: std::collections::HashSet<u64>,
}

/// Enruta UN lote de un feed vivo (`search.hits`, `compare.rows`) a su
/// Task por `task_id`. Si el route existe, envía; `Closed` (el frontend
/// soltó su `rx`) retira el route; `Full` descarta el lote con aviso
/// (backpressure: el frontend va por detrás — estos lotes son un feed de
/// Qué hacer con un lote que no cabe en el buffer del consumidor.
///
/// La diferencia no es de estilo: depende de para qué sirven los lotes.
#[derive(Clone, Copy)]
pub(super) enum OnFull {
    /// Descartar el lote y seguir. Los hits de una búsqueda y las filas de
    /// una comparación son PINTURA: perder un lote empobrece una lista que
    /// nadie va a usar para escribir, y cerrar el feed entero castigaría
    /// más de lo que protege.
    DropBatch,
    /// Cerrar el feed. Los pasos de un plan de sincronización NO son
    /// pintura: son las operaciones que el `plan_hash` va a ejecutar, y
    /// entre ellas hay `DeleteTree` y `Overwrite`. Un lote descartado en
    /// silencio con el cierre entregado detrás dejaría a un humano
    /// aprobando un hash que cubre pasos que nunca vio — que es exactamente
    /// lo que este diseño existe para impedir. Cerrar el feed hace que el
    /// `sync.plan_done` no llegue, y sin él no hay hash con el que aprobar
    /// nada: se falla del lado seguro.
    ///
    /// (El brazo EMBEBIDO no tiene este problema: usa `send().await`, o sea
    /// contrapresión de verdad, y no pierde un paso.)
    CloseFeed,
}

/// UI, no dato autoritativo). Sin route todavía (carrera de arranque), lo
/// retiene en `pending` acotado para que el método que lo lanzó lo drene
/// al registrar; `task_id` desconocido con `pending` lleno = descarte con
/// traza (un daemon no debería emitir lotes de Tasks que no lanzamos).
///
/// `feed` es solo la etiqueta de las trazas. `on_full` decide qué pasa
/// cuando el consumidor no drena, que es donde los feeds DEJAN de parecerse.
pub(super) fn route_batch<T>(
    routes: &Mutex<BatchRoutes<T>>,
    id: u64,
    batch: T,
    feed: &'static str,
    on_full: OnFull,
) {
    let mut sr = routes.lock().expect("batch routes lock sano");
    if let Some(tx) = sr.routes.get(&id) {
        match tx.try_send(batch) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Closed(_)) => {
                // El frontend soltó su Receiver: el route ya no sirve.
                sr.routes.remove(&id);
            }
            Err(mpsc::error::TrySendError::Full(_)) => match on_full {
                OnFull::DropBatch => tracing::warn!(
                    task_id = id,
                    feed,
                    "buffer del cliente lleno, lote descartado (backpressure)"
                ),
                OnFull::CloseFeed => {
                    tracing::warn!(
                        task_id = id,
                        feed,
                        "buffer del cliente lleno: se CIERRA el feed en vez de \
                         descartar el lote"
                    );
                    // Soltar el sender cierra el `rx` del frontend. Lo que
                    // venga detrás —incluido el `sync.plan_done`— ya no se
                    // entrega, así que el cliente se queda sin `plan_hash` y
                    // no puede aprobar un plan que vio incompleto.
                    sr.routes.remove(&id);
                    sr.pending.remove(&id);
                }
            },
        }
    } else if sr.pending_len() < BATCH_PENDING_CAP {
        sr.pending.entry(id).or_default().push(batch);
    } else {
        tracing::debug!(
            task_id = id,
            feed,
            "lote sin route y pending lleno: descartado"
        );
    }
}

/// Registra el route de un feed recién lanzado y devuelve su `rx`.
///
/// Orden ANTI-CARRERA: el route se registra ANTES de que puedan llegar más
/// lotes. La bomba (otra task) puede haber enrutado ya lotes que
/// adelantaron a la respuesta del método —el frame puede salir del daemon
/// antes que la respuesta de `fs.search`/`fs.compare`, y en el cliente la
/// bomba y la llamada corren en paralelo—: esos lotes se quedaron en
/// `pending`. El registro (drenar `pending` + insertar el route) es
/// ATÓMICO bajo el lock, así que ni un lote se pierde entre ambos pasos.
/// Es el mismo patrón con el que `own_task` cierra la carrera del terminal
/// adelantado vía el anillo `finished`.
///
/// Si el TERMINAL se adelantó al registro, `route` no pudo programar la
/// retirada (aún no había route): la programa aquí.
pub(super) fn register_route<T: Send + 'static>(
    inner: &Arc<Inner>,
    id: u64,
    feed: &'static str,
    sel: fn(&Inner) -> &Mutex<BatchRoutes<T>>,
) -> mpsc::Receiver<T> {
    let (tx, rx) = mpsc::channel::<T>(BATCH_BUF);
    let (already_terminal, discarded) = {
        let mut sr = sel(inner).lock().expect("batch routes lock sano");
        // Drena los lotes que se adelantaron al registro (en orden). El
        // buffer se dimensiona para absorber el arranque; si aun así se
        // llenara, un lote de UI se pierde (honesto). El log va DESPUÉS de
        // soltar el guard: este lock lo toma también la bomba (ruta
        // caliente) y no debe esperar por un `tracing::warn!`.
        let mut discarded = 0usize;
        if let Some(early) = sr.pending.remove(&id) {
            for batch in early {
                if tx.try_send(batch).is_err() {
                    discarded += 1;
                }
            }
        }
        sr.routes.insert(id, tx);
        (sr.terminated.contains(&id), discarded)
    };
    if discarded > 0 {
        tracing::warn!(
            task_id = id,
            discarded,
            feed,
            "lotes de arranque descartados (buffer del cliente lleno)"
        );
    }
    if already_terminal {
        schedule_route_removal(inner, id, sel);
    }
    rx
}

/// Programa la retirada del route de un feed terminal tras
/// [`BATCH_ROUTE_GRACE`]. Sostiene un [`Weak`] (no mantiene vivo a
/// `Inner`): si el backend ya murió, no hay nada que limpiar. Al retirar el
/// sender, el `rx` del frontend se cierra (fin del stream de lotes).
///
/// `sel` elige el mapa del feed dentro de `Inner` — un puntero a función,
/// para que la task de gracia no capture nada más que el `Weak` y el id.
pub(super) fn schedule_route_removal<T: Send + 'static>(
    inner: &Arc<Inner>,
    id: u64,
    sel: fn(&Inner) -> &Mutex<BatchRoutes<T>>,
) {
    let weak = Arc::downgrade(inner);
    tokio::spawn(async move {
        tokio::time::sleep(BATCH_ROUTE_GRACE).await;
        if let Some(inner) = weak.upgrade() {
            let mut sr = sel(&inner).lock().expect("batch routes lock sano");
            sr.routes.remove(&id);
            sr.pending.remove(&id);
            sr.terminated.remove(&id);
        }
    });
}
