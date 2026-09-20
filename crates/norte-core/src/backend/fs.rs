//! El área de ficheros de [`Backend`](super::Backend): listar, leer,
//! capabilities/atributos, stat, y las mutaciones de `fs.*` (copiar, mover,
//! borrar, crear, permisos), más `fs.search`, `fs.dir_size`, `fs.checksum` y
//! `fs.dir_usage` con sus informes.

use futures::StreamExt;
use norte_proto::{ByteRange, Capabilities, DeleteMode, Entry, Error, TaskId, VPath};
use tokio::sync::mpsc;

use crate::Engine;
use crate::engine::TransferOptions;

use super::{Backend, EntryStream, TaskRef};

impl Backend {
    /// Listado de un directorio como STREAM perezoso (ADR 0017). Embebido =
    /// el stream del engine tal cual; remoto = primera página EAGER (paridad
    /// de errores: `NotFound`/`TypeMismatch` en el `Result`, no como primer
    /// item) + páginas siguientes por cursor. Soltar el stream lo cancela.
    ///
    /// Devuelve además las omitidas del CONTENEDOR (#93): entradas que su
    /// índice descartó por nombres hostiles/límites (providers archive) y que
    /// por tanto JAMÁS saldrán del stream. `None` = no aplica (el backend
    /// lista todo lo que existe). Disponible al abrir en ambos modos: el
    /// embebido consulta el índice ya caliente; el remoto lo trae la primera
    /// página (todas la repiten).
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn list_stream(&self, dir: &VPath) -> Result<(EntryStream, Option<u64>), Error> {
        self.list_stream_with(dir, &[]).await
    }

    /// [`Backend::list_stream`] pidiendo atributos por entrada (#108 bloque
    /// 2). `attrs` son ids del catálogo (`Backend::attr_catalog`); un id no
    /// anunciado viene ausente, jamás es error.
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn list_stream_with(
        &self,
        dir: &VPath,
        attrs: &[String],
    ) -> Result<(EntryStream, Option<u64>), Error> {
        match self {
            Self::Embedded(engine) => {
                let opt = norte_vfs::ListOptions {
                    attrs: norte_vfs::AttrRequest::sanitized(attrs.to_vec()),
                };
                let stream = engine.list_with(dir, &opt).await?;
                // Mismo cinturón de emisión que el daemon (ADR 0039 §5): un
                // provider con bug no cuela ids no pedidos ni valores sobre
                // tope por la ruta in-process.
                let belt = opt.attrs.clone();
                let stream = stream
                    .map(move |item| {
                        item.map(|mut e| {
                            belt.retain_conforming(&mut e);
                            e
                        })
                    })
                    .boxed();
                // Best-effort: un fallo aquí no tumba un listado que ya abrió
                // (mismo contrato que el daemon) — degrada a "desconocido",
                // pero JAMÁS en silencio (el punto de #93 es la señal).
                let skipped = engine.list_skipped(dir).await.unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "list_skipped falló; omitidas = desconocido");
                    None
                });
                Ok((stream, skipped))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.list_stream(dir, attrs.to_vec()).await,
        }
    }

    /// Listado COMPLETO (drena [`Backend::list_stream`]). El `ls` remoto de un
    /// dir gigante ya no arriesga el `CALL_TIMEOUT` ni un frame monstruoso: son
    /// N páginas acotadas por debajo.
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn list(&self, dir: &VPath) -> Result<Vec<Entry>, Error> {
        Ok(self.list_with_skipped(dir).await?.0)
    }

    /// [`Backend::list`] + las omitidas del contenedor (#93) — para frontends
    /// que quieran señalizarlas (`ls` de la CLI).
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn list_with_skipped(&self, dir: &VPath) -> Result<(Vec<Entry>, Option<u64>), Error> {
        self.list_with_skipped_attrs(dir, &[]).await
    }

    /// [`Backend::list_with_skipped`] pidiendo atributos por entrada (#108
    /// bloque 2) — el `ls --attrs` de la CLI.
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn list_with_skipped_attrs(
        &self,
        dir: &VPath,
        attrs: &[String],
    ) -> Result<(Vec<Entry>, Option<u64>), Error> {
        let (mut stream, skipped) = self.list_stream_with(dir, attrs).await?;
        let mut entries = Vec::new();
        while let Some(item) = stream.next().await {
            entries.push(item?);
        }
        Ok((entries, skipped))
    }

    /// Lectura de PRESENTACIÓN (viewer): junta el rango pedido en memoria.
    /// El caller acota (`len`) — esto no es el camino de las copias.
    ///
    /// # Errors
    /// Taxonomía del protocolo.
    pub async fn read(&self, path: &VPath, range: Option<ByteRange>) -> Result<Vec<u8>, Error> {
        match self {
            Self::Embedded(engine) => {
                let mut stream = engine.read(path, range).await?;
                let mut out = Vec::new();
                while let Some(chunk) = stream.next().await {
                    out.extend_from_slice(&chunk?);
                }
                Ok(out)
            }
            #[cfg(unix)]
            Self::Remote(r) => r.read(path, range).await,
        }
    }

    /// Capabilities del provider que sirve `path` (F8/papelera, ADR 0009).
    ///
    /// # Errors
    /// Taxonomía del protocolo.
    pub async fn capabilities(&self, path: &VPath) -> Result<Capabilities, Error> {
        match self {
            Self::Embedded(engine) => engine.capabilities(path).await,
            #[cfg(unix)]
            Self::Remote(r) => r.capabilities(path).await,
        }
    }

    /// Catálogo de attrs del provider que sirve `path` (#108 bloque 2),
    /// SIEMPRE saneado: el embebido pasa por `Engine::attr_catalog`
    /// (`AttrCatalog::new`, ADR 0039 §4) y el remoto por el deserializador
    /// del wire (mismo saneo por el tipo).
    ///
    /// # Errors
    /// Taxonomía del protocolo.
    pub async fn attr_catalog(&self, path: &VPath) -> Result<norte_proto::AttrCatalog, Error> {
        match self {
            Self::Embedded(engine) => engine.attr_catalog(path).await,
            #[cfg(unix)]
            Self::Remote(r) => r.attr_catalog(path).await,
        }
    }

    /// Both halves of `fs.capabilities` for `path`: the capability flags AND
    /// the attribute catalogue, in ONE round trip.
    ///
    /// [`Self::capabilities`] and [`Self::attr_catalog`] each throw the other
    /// half of that response away, so a frontend that wants both — the TUI
    /// caches the catalogue for its columns and the flags to answer
    /// "read-only?" without asking again — paid two round trips for one
    /// message. Remote mode makes a single `fs.capabilities` call here;
    /// embedded mode asks the engine twice, which is two provider lookups
    /// instead of one and no extra round trip on the wire. Not "no I/O at
    /// all": both halves go through `Engine::provider_for`, which for a remote
    /// scheme can resolve or establish the connection first — true of
    /// `file://`, false of an embedded `sftp` pane.
    ///
    /// # Errors
    /// Taxonomía del protocolo.
    pub async fn capabilities_and_attrs(
        &self,
        path: &VPath,
    ) -> Result<(Capabilities, norte_proto::AttrCatalog), Error> {
        match self {
            Self::Embedded(engine) => Ok((
                engine.capabilities(path).await?,
                engine.attr_catalog(path).await?,
            )),
            #[cfg(unix)]
            Self::Remote(r) => {
                let full = r.capabilities_full(path).await?;
                Ok((full.capabilities, full.attrs))
            }
        }
    }

    /// Metadatos de un nodo (`fs.stat`).
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn stat(&self, path: &VPath) -> Result<Entry, Error> {
        self.stat_attrs(path, &[]).await
    }

    /// [`Backend::stat`] pidiendo atributos por entrada (#108 bloque 2).
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn stat_attrs(&self, path: &VPath, attrs: &[String]) -> Result<Entry, Error> {
        match self {
            Self::Embedded(engine) => {
                let opt = norte_vfs::ListOptions {
                    attrs: norte_vfs::AttrRequest::sanitized(attrs.to_vec()),
                };
                let mut entry = engine.stat_with(path, &opt).await?;
                // Mismo cinturón de emisión que el daemon (ADR 0039 §5).
                opt.attrs.retain_conforming(&mut entry);
                Ok(entry)
            }
            #[cfg(unix)]
            Self::Remote(r) => r.stat(path, attrs.to_vec()).await,
        }
    }

    /// Retiene el ancla de un directorio que un PANEL acaba de listar (#301,
    /// ADR 0073), para que la escritura que venga después pueda decir «el
    /// destino era ESE».
    ///
    /// Es lo que el daemon pone en la respuesta de `fs.list` y el SDK guarda
    /// por su cuenta. Aquí no hay wire, así que lo guarda el engine — y sin
    /// esto `ntc`, que corre embebido por DEFECTO, hacía toda operación
    /// anclada SIN ancla: la comprobación que ADR 0076 pidió justo para
    /// `fs.create` no la tenía el único frontend que lanza un `$EDITOR` sobre
    /// lo creado.
    ///
    /// # Se llama a mano, y ese es el punto
    ///
    /// No lo hace `list_stream_with`, que es el embudo de TODOS los listados:
    /// por ahí pasan el árbol lateral (una rama por vuelta del bucle) y el
    /// `fs.list` de un script Lua, y como recordar SOBRESCRIBE, cualquiera de
    /// ellos rebendecía el ancla del panel con el nodo que viera en ese
    /// momento. El ancla dice **quién miró**; un listado que no es una
    /// pantalla no ha mirado nadie.
    ///
    /// Contra el daemon no hace nada: allí el ancla la manda el listado en su
    /// respuesta y la guarda el SDK, que es de quien listó de verdad.
    ///
    /// Best-effort: un provider que no sabe dar identidad de nodo (un bucket,
    /// un SFTP sin extensiones) no puede impedir un listado, y un fallo BORRA
    /// la que hubiera —mandar una vieja sería que la escritura se rechazara a
    /// sí misma—, así que la escritura siguiente se comporta como en 0.53.
    pub async fn remember_listing_anchor(&self, dir: &VPath) {
        match self {
            Self::Embedded(engine) => {
                let ancla = engine.dir_anchor(dir).await.unwrap_or_else(|e| {
                    tracing::debug!(error = %e, "dir_anchor falló; sin ancla para este listado");
                    None
                });
                engine.remember_dir_anchor(dir, ancla);
            }
            #[cfg(unix)]
            Self::Remote(_) => {}
        }
    }

    /// El ancla retenida del directorio en el que `destino` va a escribirse
    /// (#301).
    ///
    /// `destino` es la ruta EXACTA de lo que se escribe, así que lo que se
    /// busca es su PADRE: es el directorio que el humano listó y aprobó. La
    /// misma cuenta que hace el SDK en el camino remoto.
    ///
    /// `None` —nadie listó ese directorio en esta sesión, o su provider no
    /// sabe dar identidad de nodo— se comporta exactamente como 0.53: se
    /// confina igual y esa comprobación no ocurre.
    fn ancla_del_destino(engine: &Engine, destino: &VPath) -> Option<norte_proto::DirAnchor> {
        engine.remembered_dir_anchor(&destino.parent()?)
    }

    /// Copia como task.
    ///
    /// El ancla del directorio DESTINO viaja con la operación cuando este
    /// backend lo listó (#301, ADR 0073) — igual que la pone el SDK en el
    /// camino remoto, y por el mismo motivo: entre listar y escribir, ese
    /// directorio puede haber dejado de ser el nodo que el humano miraba.
    ///
    /// # Errors
    /// Taxonomía del protocolo.
    pub async fn copy(
        &self,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => Ok(TaskRef::from_handle(
                &engine
                    .copy_anchored(
                        from,
                        to,
                        opts,
                        crate::journal::Actor::User,
                        Self::ancla_del_destino(engine, to),
                    )
                    .await?,
            )),
            #[cfg(unix)]
            Self::Remote(r) => r
                .transfer(norte_client::Transfer::Copy, from, to, opts.into())
                .await
                .map(TaskRef::from),
        }
    }

    /// Move como task. Con el ancla del destino, como [`Self::copy`].
    ///
    /// # Errors
    /// Taxonomía del protocolo.
    pub async fn move_(
        &self,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => Ok(TaskRef::from_handle(
                &engine
                    .move_anchored(
                        from,
                        to,
                        opts,
                        crate::journal::Actor::User,
                        Self::ancla_del_destino(engine, to),
                    )
                    .await?,
            )),
            #[cfg(unix)]
            Self::Remote(r) => r
                .transfer(norte_client::Transfer::Move, from, to, opts.into())
                .await
                .map(TaskRef::from),
        }
    }

    /// Borrado como task (papelera o permanente, ADR 0009).
    ///
    /// # Errors
    /// Taxonomía del protocolo.
    pub async fn delete(&self, path: &VPath, mode: DeleteMode) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                Ok(TaskRef::from_handle(&engine.delete_with(path, mode).await?))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.delete(path, mode).await.map(TaskRef::from),
        }
    }

    /// Creación de UN directorio como Task (#104, F7). Sin `-p`; destino
    /// ocupado = `Conflict{Exists}`.
    ///
    /// # Errors
    /// Taxonomía del protocolo.
    pub async fn mkdir(&self, path: &VPath) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => Ok(TaskRef::from_handle(&engine.mkdir(path).await?)),
            #[cfg(unix)]
            Self::Remote(r) => r.mkdir(path).await.map(TaskRef::from),
        }
    }

    /// Creación de UN fichero VACÍO como Task (#290). Destino ocupado =
    /// `Conflict{Exists}`; la exclusividad la aporta el provider (atómica en
    /// local y en objetos, con ventana en SFTP v3).
    ///
    /// Con el ancla del directorio, como [`Self::copy`] — y aquí es donde más
    /// falta hace (#301): `fs.create` es el único método cuyo éxito entrega
    /// una ruta a un programa de FUERA de norte (`$EDITOR`), que es el motivo
    /// con el que ADR 0076 justificó ponerle ancla.
    ///
    /// # Errors
    /// Taxonomía del protocolo.
    pub async fn create_file(&self, path: &VPath) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => Ok(TaskRef::from_handle(
                &engine
                    .create_file_as(
                        path,
                        Self::ancla_del_destino(engine, path),
                        crate::journal::Actor::User,
                    )
                    .await?,
            )),
            #[cfg(unix)]
            Self::Remote(r) => r.create_file(path).await.map(TaskRef::from),
        }
    }

    /// Cambia los permisos POSIX de un lote de rutas como Task (#314).
    ///
    /// Muta: journal con reversa —el modo anterior— y gate de política. Una
    /// ubicación sin permisos POSIX responde `Unsupported` y no cambia nada.
    ///
    /// # Errors
    /// Taxonomía del protocolo: [`Error::InvalidPath`] sin rutas, por encima
    /// del tope o con bits que no son de permiso; [`Error::PolicyDenied`];
    /// [`Error::Unsupported`].
    pub async fn set_mode(
        &self,
        params: norte_proto::methods::FsSetModeParams,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => Ok(TaskRef::from_handle(&engine.set_mode(params).await?)),
            #[cfg(unix)]
            Self::Remote(r) => r.set_mode(params).await.map(TaskRef::from),
        }
    }

    /// Búsqueda viva (`fs.search`, live search): devuelve la Task
    /// ([`TaskRef`], cancelable con `TaskRef::cancel`) y el STREAM de lotes de
    /// hits ([`norte_proto::methods::SearchHits`]).
    ///
    /// El humano de un frontend es siempre `User` (sin sandbox): el embebido
    /// lo pasa tal cual a [`Engine::search_as`]; el remoto lo lanza contra el
    /// daemon, que fija el actor server-side por la conexión.
    ///
    /// # Ciclo de vida del canal de hits
    /// - **Embebido:** el walker del engine cierra el `tx` al terminar, así que
    ///   `rx` se cierra solo (drena hasta `None`).
    /// - **Remoto:** la bomba del `RemoteBackend` enruta cada notificación
    ///   `search.hits` por `task_id` a este `rx`. El route se retira —cerrando
    ///   `rx`— cuando la Task llega a terminal (con una gracia que cubre la
    ///   carrera hits-vs-terminal; ver `RemoteBackend::search`). En ambos casos
    ///   el criterio de "búsqueda terminada" es el estado terminal de la
    ///   [`TaskRef`]; el cierre de `rx` es la señal cómoda de que ya no llegan
    ///   más lotes.
    ///
    /// # Errors
    /// Criterios inválidos (cero criterios y cero filtros, glob y regex del
    /// mismo eje, o una codificación que no se reconoce) →
    /// [`Error::InvalidPath`] embebido / `INVALID_PARAMS` del daemon; resto,
    /// taxonomía del protocolo; daemon caído = `ProviderUnavailable`.
    ///
    /// Y contra un daemon anterior a 0.81 con cualquiera de los filtros
    /// puestos, [`Error::Unsupported`]: ese daemon los ignoraría y
    /// contestaría el SUPERCONJUNTO, que se lee igual que un resultado. El
    /// rechazo vive en el SDK (`RemoteClient::search`) y por eso alcanza a
    /// todo el mundo: éste es el único camino al cable, y el embebido no
    /// cruza ninguno.
    pub async fn search(
        &self,
        params: norte_proto::methods::FsSearchParams,
    ) -> Result<(TaskRef, mpsc::Receiver<norte_proto::methods::SearchHits>), Error> {
        match self {
            Self::Embedded(engine) => {
                let (handle, rx) = engine
                    .search_as(params, crate::journal::Actor::User)
                    .await?;
                Ok((TaskRef::from_handle(&handle), rx))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.search(params).await.map(|(t, rx)| (TaskRef::from(t), rx)),
        }
    }

    /// Cuánto ocupa lo que se le pase, como Task (`fs.dir_size`, 0.49.0,
    /// #139).
    ///
    /// El TOTAL no vuelve por aquí: viaja en el progreso de la Task
    /// (`bytes_done`/`entries_done`), que es lo que el frontend ya escucha para
    /// pintar cualquier otra. El último snapshot es el resultado.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPath`] sin rutas, y lo que devuelva el core. Un daemon
    /// N-1 sin el método contesta `METHOD_NOT_FOUND` → [`Error::Unsupported`],
    /// para que el frontend distinga «tu daemon es más viejo» de un fallo real.
    pub async fn dir_size(
        &self,
        params: norte_proto::methods::FsDirSizeParams,
    ) -> Result<TaskRef, Error> {
        if params.paths.is_empty() {
            return Err(Error::InvalidPath);
        }
        match self {
            Self::Embedded(engine) => {
                let handle = engine
                    .dir_size_as(params, crate::journal::Actor::User)
                    .await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.dir_size(params).await.map(TaskRef::from),
        }
    }

    /// El digest del contenido de un lote de ficheros (`fs.checksum`, 0.59.0,
    /// #311): devuelve la Task, y los digests se recogen con
    /// [`Self::checksum_report`].
    ///
    /// **No muta nada**: leer no es escribir (regla dura 4 no aplica).
    ///
    /// # Errors
    /// [`Error::InvalidPath`] con la lista vacía; la taxonomía del protocolo
    /// para el resto.
    pub async fn checksum(
        &self,
        params: norte_proto::methods::FsChecksumParams,
    ) -> Result<TaskRef, Error> {
        if params.paths.is_empty() {
            return Err(Error::InvalidPath);
        }
        match self {
            Self::Embedded(engine) => {
                let handle = engine
                    .checksum_as(params, crate::journal::Actor::User)
                    .await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.checksum(params).await.map(TaskRef::from),
        }
    }

    /// Los digests que lleva calculados esa Task (`fs.checksum_report`,
    /// 0.59.0, #311). SNAPSHOT: parcial mientras corre, definitivo cuando la
    /// Task es terminal.
    ///
    /// # Errors
    /// [`Error::NotFound`] si ese id nunca fue un lote de sumas de esta
    /// instancia o si el anillo ya lo desalojó.
    pub async fn checksum_report(
        &self,
        task_id: TaskId,
    ) -> Result<norte_proto::methods::FsChecksumReportResult, Error> {
        match self {
            // Embebido no hay actor que comprobar: este `Backend` ES el humano
            // en proceso (mismo criterio que `rename_batch_report`).
            Self::Embedded(engine) => engine
                .checksum_report(task_id)
                .map(|(_owner, r)| r)
                .ok_or(Error::NotFound),
            #[cfg(unix)]
            Self::Remote(r) => r.checksum_report(task_id).await,
        }
    }

    /// De qué está hecho un directorio, hijo a hijo (`fs.dir_usage`, 0.75.0,
    /// fase 4): devuelve la Task, y el mapa se recoge con
    /// [`Self::dir_usage_report`].
    ///
    /// **No muta nada**: medir no es escribir (regla dura 4 no aplica).
    ///
    /// # Errors
    /// [`Error::InvalidPath`] con `depth` en cero o por encima de
    /// [`DIR_USAGE_MAX_DEPTH`](norte_proto::methods::DIR_USAGE_MAX_DEPTH). Los
    /// dos se comprueban AQUÍ, antes de elegir brazo, para que el embebido y el
    /// remoto contesten lo mismo — la lección de `check_pairs_cap`. El daemon
    /// los sigue comprobando por su cuenta: aquello es la frontera, esto es la
    /// paridad de las dos vías.
    ///
    /// **Lo que NO se comprueba aquí es hasta dónde sabe bajar el servidor.**
    /// Que hoy solo se sirva `depth: 1` es una capacidad del daemon, no el
    /// contrato del tipo: cablearla en el cliente haría que un `Backend` 0.75
    /// rechazara por su cuenta un `depth: 2` que un daemon 0.76 sí sirve, sin
    /// llegar a preguntárselo. Eso lo contesta quien lo sabe, y llega como
    /// [`Error::Unsupported`].
    ///
    /// Un daemon N-1 sin el método contesta `METHOD_NOT_FOUND` → también
    /// [`Error::Unsupported`]: quien necesite distinguir «no conoce el método»
    /// de «esa profundidad no se sirve» lo sabe por la `depth` que pidió.
    pub async fn dir_usage(
        &self,
        params: norte_proto::methods::FsDirUsageParams,
    ) -> Result<TaskRef, Error> {
        if params.depth == 0 || params.depth > norte_proto::methods::DIR_USAGE_MAX_DEPTH {
            return Err(Error::InvalidPath);
        }
        match self {
            Self::Embedded(engine) => {
                let handle = engine
                    .dir_usage_as(params, crate::journal::Actor::User)
                    .await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.dir_usage(params).await.map(TaskRef::from),
        }
    }

    /// El mapa que lleva medido esa Task (`fs.dir_usage_report`, 0.75.0, fase
    /// 4). SNAPSHOT: parcial mientras corre, definitivo cuando la Task es
    /// terminal.
    ///
    /// # Errors
    /// [`Error::NotFound`] si ese id nunca fue un mapa de esta instancia o si
    /// el anillo ya lo desalojó.
    pub async fn dir_usage_report(
        &self,
        task_id: TaskId,
    ) -> Result<norte_proto::methods::FsDirUsageReportResult, Error> {
        match self {
            // Embebido no hay actor que comprobar: este `Backend` ES el humano
            // en proceso (mismo criterio que `checksum_report`).
            Self::Embedded(engine) => engine
                .dir_usage_report(task_id)
                .map(|(_owner, r)| r)
                .ok_or(Error::NotFound),
            #[cfg(unix)]
            Self::Remote(r) => r.dir_usage_report(task_id).await,
        }
    }
}
