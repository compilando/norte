//! El área de comparación y sincronización de árboles de
//! [`Backend`](super::Backend) (ADR 0048/0049): `fs.compare`, y el ciclo
//! `sync.plan` / `sync.apply` / `sync.report`.

use norte_proto::{Error, TaskId};
use tokio::sync::mpsc;

use super::{Backend, EMBEDDED_CONN_ID, TaskRef};

impl Backend {
    /// Comparación de dos árboles (`fs.compare`, 0.39.0, ADR 0048): devuelve
    /// la Task ([`TaskRef`], cancelable) y el STREAM de lotes de filas
    /// ([`norte_proto::methods::CompareRowsBatch`]).
    ///
    /// Mismo ciclo de vida del canal que [`Self::search`]: embebido, el walk
    /// cierra el `tx` al terminar; remoto, la bomba enruta cada `compare.rows`
    /// por `task_id` y el route se retira tras el terminal (con la misma
    /// gracia). El criterio de "comparación terminada" es el estado terminal
    /// de la [`TaskRef`]; el cierre del `rx` es la señal cómoda.
    ///
    /// **No muta nada**: sin journal, sin undo (regla dura 4 no aplica).
    ///
    /// # Cuándo están TODAS las filas
    /// El cierre del `rx` NO significa «llegaron todas»: una notificación se
    /// puede perder (el daemon expulsa a un suscriptor que no drena, la bomba
    /// del cliente descarta un lote si su buffer se llena, y una reconexión
    /// suelta los routes cerrando el `rx` de forma indistinguible de un final
    /// limpio). La señal es
    /// [`TaskProgress::entries_done`](norte_proto::TaskProgress::entries_done),
    /// que en una Task [`TaskKind::Compare`](norte_proto::TaskKind::Compare)
    /// cuenta FILAS emitidas: se comparan las recibidas con ese número, y
    /// **DESPUÉS de que el `rx` se cierre**, no al llegar el snapshot terminal
    /// —la bomba de filas y la de progreso son tasks distintas, así que el
    /// terminal puede adelantar al último lote—. Quien vaya a ESCRIBIR a
    /// partir de estas filas (el plan de sincronización de la spec 2) tiene
    /// que hacer esa comprobación.
    ///
    /// # Errors
    /// Dos raíces iguales → [`Error::InvalidPath`]; `follow_symlinks: true` →
    /// [`Error::Unsupported`]. Los dos se comprueban AQUÍ, antes de elegir
    /// brazo, para que el embebido y el remoto contesten lo mismo: el daemon
    /// los rechaza con `-32602` pelado (es su contrato publicado) y
    /// `to_taxonomy` convertiría eso en `Internal`, o sea la misma respuesta
    /// que da un provider que panica. El daemon los sigue comprobando por su
    /// cuenta: aquello es la frontera, esto es la paridad de las dos vías
    /// (mismo criterio que `check_pairs_cap`).
    ///
    /// Un daemon N-1 (0.38.x) sin el método responde `METHOD_NOT_FOUND`, que
    /// se entrega como [`Error::Unsupported`] — «tu daemon es más viejo», no
    /// un fallo real. Resto, taxonomía del protocolo; daemon caído =
    /// `ProviderUnavailable`.
    pub async fn compare(
        &self,
        params: norte_proto::methods::FsCompareParams,
    ) -> Result<
        (
            TaskRef,
            mpsc::Receiver<norte_proto::methods::CompareRowsBatch>,
        ),
        Error,
    > {
        if params.follow_symlinks {
            return Err(Error::Unsupported);
        }
        if params.left == params.right {
            return Err(Error::InvalidPath);
        }
        match self {
            Self::Embedded(engine) => {
                let (handle, rx) = engine
                    .compare_as(params, crate::journal::Actor::User)
                    .await?;
                Ok((TaskRef::from_handle(&handle), rx))
            }
            #[cfg(unix)]
            Self::Remote(r) => r
                .compare(params)
                .await
                .map(|(t, rx)| (TaskRef::from(t), rx)),
        }
    }

    /// Planifica una sincronización de un sentido (`sync.plan`, 0.40.0, ADR
    /// 0049): devuelve la Task ([`TaskRef`], cancelable) y el STREAM de eventos
    /// del plan — lotes de pasos acotados y, al final, el
    /// [`SyncPlanDone`](norte_proto::methods::SyncPlanDone) que lo CIERRA y trae
    /// el `plan_hash`.
    ///
    /// **No muta nada**: por debajo es una comparación con una decisión por
    /// fila. Quien escribe es [`Self::sync_apply`], y solo con el hash que llega
    /// aquí.
    ///
    /// # El orden de los eventos es el del canal
    /// `sync.plan_done` llega SIEMPRE después del último lote de pasos, en los
    /// dos brazos: el core mete ambos en un `mpsc` y la bomba del cliente los
    /// enruta al mismo `rx`. Un cierre que llegara antes que un lote sería un
    /// cliente aprobando el hash de un plan que todavía estaba llegando.
    ///
    /// # Cuándo están TODOS los pasos
    /// El `sync.plan_done` es la señal, y su ausencia es la protección: sin él
    /// no hay `plan_hash`, y sin `plan_hash` no se puede aplicar nada. Las TRES
    /// formas de perder un lote fallan por ese lado:
    ///
    /// 1. el daemon expulsa a quien no drena su outbox → su bomba para y sus
    ///    planes retenidos se barren;
    /// 2. una reconexión suelta los routes → el `rx` se cierra;
    /// 3. **el buffer de este proceso se llena** porque quien consume el `rx` va
    ///    más lento que el daemon. Este es el único que un cliente se hace a sí
    ///    mismo, y por eso el enrutado CIERRA el feed en vez de descartar el
    ///    lote (`OnFull::CloseFeed`): descartarlo y entregar el cierre detrás
    ///    —que es lo que hacen `search.hits` y `compare.rows`, donde un lote es
    ///    pintura— dejaría a un humano aprobando un hash que cubre pasos que
    ///    nunca vio.
    ///
    /// Aun así, quien pinte estos pasos debería cuadrarlos:
    /// `SyncPlanDone::counts` suma el plan ENTERO (`create_dir + copy +
    /// overwrite + delete_tree + skip`), así que comparar esa suma con los pasos
    /// recibidos detecta cualquier pérdida futura sin depender de que el
    /// transporte la señale. `TaskProgress::entries_done` cuenta lo mismo desde
    /// el otro lado.
    ///
    /// # El plan queda RETENIDO
    /// Aprobar cuesta un fichero en el directorio de estado del daemon, vivo
    /// durante [`SYNC_PLAN_TTL_MS`](norte_proto::methods::SYNC_PLAN_TTL_MS) y
    /// atado a esta conexión. Hay un tope de planes retenidos por conexión:
    /// pasado, el daemon contesta `OVERLOADED` sin taxonomía —la petición es
    /// válida, el momento no— y este brazo lo entrega como
    /// [`Error::Internal`], igual que el resto de los `-32602`/`-32603` pelados
    /// del daemon. No se puede adelantar aquí porque solo el daemon sabe cuántos
    /// planes retiene esta conexión.
    ///
    /// # Errors
    /// [`Error::Unsupported`] si `compare.follow_symlinks` o
    /// `compare.descend_orphans` vienen puestos (ninguno de los dos es del
    /// llamante: el planificador fija el segundo al lado del origen);
    /// [`Error::InvalidPath`] si `include` pasa de
    /// [`SYNC_MAX_INCLUDE`](norte_proto::methods::SYNC_MAX_INCLUDE). Los tres se
    /// comprueban AQUÍ, antes de elegir brazo, por lo mismo que en
    /// [`Self::compare`]: el daemon los rechaza con `-32602` pelado y
    /// `to_taxonomy` convertiría eso en `Internal`, o sea la misma respuesta que
    /// da un provider que panica. El engine los sigue comprobando por su cuenta.
    ///
    /// Adelantarlos cambia el ORDEN de dos rechazos, y conviene saberlo: contra
    /// un engine sin spool, esto contesta por el parámetro (`InvalidPath`)
    /// donde el engine habría contestado por la retención (`Unsupported`); y
    /// contra un daemon, un agente sin scope recibe la queja del parámetro desde
    /// su propio proceso en vez del `PolicyDenied` del daemon, que gatea antes
    /// de validar. Ninguno filtra nada —estas tres comprobaciones no miran las
    /// rutas— y es la misma asimetría que [`Self::compare`] ya tiene.
    ///
    /// Además: [`Error::OverlappingRoots`] si las dos raíces se solapan (esa sí
    /// es categoría del wire y viene del engine, sin copia aquí),
    /// [`Error::Unsupported`] si el daemon no tiene spool instalado o es un
    /// daemon N-1 sin el método; resto, taxonomía del protocolo.
    pub async fn sync_plan(
        &self,
        params: norte_proto::methods::SyncPlanParams,
    ) -> Result<(TaskRef, mpsc::Receiver<crate::sync::SyncPlanEvent>), Error> {
        if params.compare.follow_symlinks || params.compare.descend_orphans.is_some() {
            return Err(Error::Unsupported);
        }
        if params
            .include
            .as_ref()
            .is_some_and(|inc| inc.len() > norte_proto::methods::SYNC_MAX_INCLUDE)
        {
            return Err(Error::InvalidPath);
        }
        match self {
            Self::Embedded(engine) => {
                let (handle, rx) = engine
                    .sync_plan_as(params, EMBEDDED_CONN_ID, crate::journal::Actor::User)
                    .await?;
                Ok((TaskRef::from_handle(&handle), rx))
            }
            #[cfg(unix)]
            Self::Remote(r) => r
                .sync_plan(params)
                .await
                .map(|(t, rx)| (TaskRef::from(t), rx)),
        }
    }

    /// Ejecuta el plan APROBADO que `plan_hash` nombra (`sync.apply`, 0.40.0,
    /// ADR 0049) como UNA Task y UN lote deshacible del journal.
    ///
    /// **El hash es el único parámetro**, y esa es la garantía: no hay forma de
    /// pedir que se ejecute algo distinto de lo que [`Self::sync_plan`] enseñó.
    /// Las dos raíces, el modo y los criterios salen del plan retenido.
    ///
    /// **El plan se gasta**: aplicarlo lo consume, pase lo que pase. Un segundo
    /// `sync_apply` del mismo hash es [`Error::PlanStale`], que es verdad.
    ///
    /// Qué pasó de verdad se pide con [`Self::sync_report`]: un paso que falla
    /// es una FILA del informe y no el final de la Task, así que el estado
    /// terminal no cuenta ni la mitad.
    ///
    /// # Errors
    /// [`Error::PlanStale`] si el hash no nombra un plan vivo de este proceso
    /// (no existe, caducó, está manipulado o ya se aplicó);
    /// [`Error::PlanNotExecutable`] si el plan traía bloqueos;
    /// [`Error::PolicyDenied`] del gate, que corre sobre las raíces leídas del
    /// plan y AHORA, no cuando se planificó; [`Error::Unsupported`] sin spool o
    /// sin journal (aplicar sin journal sería enterrar sin dejar vuelta atrás,
    /// regla dura 4), o contra un daemon N-1; resto, taxonomía del protocolo.
    pub async fn sync_apply(
        &self,
        plan_hash: &norte_proto::methods::PlanHash,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let (handle, _report) = engine
                    .sync_apply_as(plan_hash, EMBEDDED_CONN_ID, crate::journal::Actor::User)
                    .await?;
                // El informe queda en el anillo del engine, que es de donde lo
                // lee `sync_report`: los dos brazos se piden igual.
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.sync_apply(plan_hash).await.map(TaskRef::from),
        }
    }

    /// El informe de una aplicación ya lanzada (`sync.report`, 0.40.0): cuántos
    /// pasos se ejecutaron, cuántos fallaron y por qué —con la ruta de cada
    /// uno— y bajo qué lote del journal quedó lo que sí se aplicó.
    ///
    /// Es un SNAPSHOT: definitivo cuando la Task es terminal, parcial antes.
    /// Míralo también cuando diga `cancelled`: lo aplicado hasta el corte se
    /// queda, journalizado — media sincronización es un estado real.
    ///
    /// # Quién ve qué
    /// El daemon sirve el informe a quien podría ver la Task: su dueño, o
    /// cualquier conexión HUMANA. Un `Backend::Remote` abierto con
    /// [`super::remote::RemoteBackend::connect`] es humano, y por él se ven también
    /// los informes de las aplicaciones de los AGENTES — deliberado, y la
    /// simetría del undo: un humano que gobierna el daemon puede leer lo que un
    /// agente hizo. Uno abierto con
    /// [`super::remote::RemoteBackend::connect_as_agent`] NO lo es (lo estrenó el
    /// puente MCP): ve lo suyo y nada más, y para él «no es tuya» y «no existe»
    /// son la misma respuesta.
    ///
    /// Nótese la asimetría, que no es un descuido: la AUTORIZACIÓN (el plan) va
    /// por conexión, y su informe por ACTOR. Dos conexiones humanas son el mismo
    /// `Actor::User`, así que una lee el informe de la otra aunque no pudiera
    /// aplicar su plan.
    ///
    /// # Errors
    /// [`Error::NotFound`] si ese `task_id` nunca fue una aplicación de este
    /// proceso, si el anillo ya lo desalojó, o si el que pregunta no podía verla.
    /// [`Error::Unsupported`] contra un daemon N-1; resto, taxonomía del
    /// protocolo.
    pub async fn sync_report(
        &self,
        task_id: TaskId,
    ) -> Result<norte_proto::methods::SyncReportResult, Error> {
        match self {
            Self::Embedded(engine) => engine
                .sync_report(task_id)
                .map(|(_owner, r)| r)
                // Embebido no hay actor que comprobar: este `Backend` ES el
                // humano en proceso (mismo criterio que `rename_batch_report`).
                .ok_or(Error::NotFound),
            #[cfg(unix)]
            Self::Remote(r) => r.sync_report(task_id).await,
        }
    }
}
