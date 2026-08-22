//! Lo que el host le pide al mundo, en la forma MÁS pequeña que le sirve.
//!
//! No es una segunda fachada del SDK: es la lista corta de cosas que el
//! controlador necesita, y existe por una razón concreta —que sus tests sean
//! deterministas sin daemon—. Todo lo demás se le pide al
//! [`norte_client::RemoteBackend`] directamente.

use std::sync::Arc;

use futures::future::BoxFuture;
use norte_client::{ConnEvent, EntryStream};
use norte_proto::{
    AttrCatalog, CollisionPolicy, DeleteMode, Entry, Error, TaskId, TaskProgress, VPath, methods,
};
use tokio::sync::watch;

/// Una Task en marcha, en la forma mínima que el host necesita: su id, su
/// progreso y cómo pedirle que pare.
///
/// No es el `RemoteTask` del SDK a propósito. El host solo necesita estas
/// tres cosas, y pedirlas así es lo que permite que un test las fabrique sin
/// daemon —que es donde se comprueban las reglas que de verdad importan: que
/// un estado terminal no se pierda y que cancelar sea idempotente—.
pub struct HostTask {
    /// Id de la task en el daemon.
    pub id: TaskId,
    /// Snapshots vivos del progreso.
    pub progress: watch::Receiver<TaskProgress>,
    /// Pide la cancelación cooperativa. Llamarla dos veces no es un error:
    /// cancelar es idempotente por contrato.
    pub cancel: Arc<dyn Fn() + Send + Sync>,
    /// La lanzó OTRO cliente de la misma sesión. Se pinta igual y se puede
    /// cancelar igual —es la misma sesión—, pero el tablero lo dice: una
    /// operación que uno no ha pedido y no se distingue de las suyas es una
    /// sorpresa.
    pub foreign: bool,
}

impl std::fmt::Debug for HostTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A mano porque una función no es `Debug`, y `finish_non_exhaustive`
        // lo DICE en vez de dar a entender que la task son dos campos.
        f.debug_struct("HostTask")
            .field("id", &self.id)
            .field("progress", &self.progress.borrow().state)
            .finish_non_exhaustive()
    }
}

/// Lo que el controlador necesita saber pedir.
///
/// Objeto-seguro a propósito (futuros en caja): el host guarda un
/// `Arc<dyn HostBackend>` y un test mete el suyo sin genéricos que se
/// propaguen por toda la API.
pub trait HostBackend: Send + Sync + 'static {
    /// El listado de un directorio, como STREAM.
    ///
    /// Paginado y no completo: un directorio de medio millón de entradas no
    /// puede viajar entero antes de pintar la primera fila. El host toma la
    /// primera página, pinta, y sigue drenando el resto por detrás
    /// ([`crate::controller`] lo extiende con `PaneState::extend`, el mismo
    /// camino que el TUI).
    ///
    /// `attrs` son los ids de atributo que las columnas configuradas piden:
    /// un provider solo manda lo que se le pide, así que pedir de menos deja
    /// una columna en blanco para siempre.
    /// Lista un directorio, y dice CUÁNTAS entradas se saltó.
    ///
    /// La cuenta viaja con el listado y no aparte porque describe A ESE
    /// listado: un provider que se salta entradas —sin permiso para
    /// statearlas, por encima de un tope suyo— devuelve menos filas de las
    /// que hay, y sin decirlo la pantalla miente por omisión. `None` = el
    /// provider no lleva la cuenta, que NO es lo mismo que cero.
    fn list(
        &self,
        dir: VPath,
        attrs: Vec<String>,
    ) -> BoxFuture<'static, Result<(EntryStream, Option<u64>), Error>>;

    /// Crea UN directorio. Devuelve la Task ya encolada.
    fn mkdir(&self, path: VPath) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// Los datos de UNA entrada.
    ///
    /// Un listado puede venir PEREZOSO —el provider local devuelve `size` y
    /// `mtime` a `None` y los rellena quien los necesite (#52)—, así que sin
    /// esto las columnas de tamaño y fecha se quedan en blanco para siempre
    /// sobre `file://`, que es la vista por defecto. El TUI ya sondea su
    /// ventana visible; este es el mismo camino para el host.
    fn stat(&self, path: VPath, attrs: Vec<String>) -> BoxFuture<'static, Result<Entry, Error>>;

    /// Lee un TROZO de un fichero.
    ///
    /// Acotado siempre: el visor enseña una cabecera, no el fichero entero
    /// (el resto no se lee), y quien llama decide el presupuesto.
    fn read(
        &self,
        path: VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> BoxFuture<'static, Result<Vec<u8>, Error>>;

    /// El catálogo de atributos de una localización.
    ///
    /// Sin él, una columna `attr:` no sabe si lo que trae es un tamaño, una
    /// fecha o un modo, y se pinta como el número crudo que es: el catálogo
    /// es lo que convierte `33188` en `-rw-r--r--`.
    fn attr_catalog(&self, dir: VPath) -> BoxFuture<'static, Result<AttrCatalog, Error>>;

    /// El canal de aprobaciones de policy pendientes: cada op de agente bajo
    /// regla `ask` que el daemon difunde, y que espera una respuesta humana.
    fn take_approvals(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<methods::PolicyApprovalRequired>>;

    /// Responde a una aprobación. `approve = false` deniega.
    fn policy_decide(
        &self,
        approval_id: u64,
        approve: bool,
    ) -> BoxFuture<'static, Result<(), Error>>;

    /// La sesión de UI y si ESTA conexión es su dueña (ADR 0059).
    ///
    /// El core la guarda y la versiona pero no la lee: el documento es de los
    /// frontends, y por eso viaja como JSON opaco.
    fn session_get(&self) -> BoxFuture<'static, Result<(methods::Session, bool), Error>>;

    /// Escribe la sesión sobre la revisión que se leyó. Devuelve la nueva.
    ///
    /// Un `Conflict` significa que otra ventana escribió en medio: se relee,
    /// jamás se pisa.
    fn session_put(
        &self,
        version: u32,
        revision: u64,
        body: serde_json::Value,
    ) -> BoxFuture<'static, Result<u64, Error>>;

    /// El canal de eventos de conexión (perdida y restaurada), si esta
    /// conexión lo tiene y nadie lo ha tomado ya.
    fn take_conn_events(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<ConnEvent>>;

    /// El canal de tasks AJENAS: las que otro cliente de la misma sesión
    /// lanzó y este observa.
    fn take_foreign_tasks(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<HostTask>>;

    /// El canal de avisos `connection.degraded` (#44): una sesión de un
    /// provider que viaja SIN cifrar.
    ///
    /// No habla del daemon —eso es [`Self::take_conn_events`]— sino de la
    /// conexión que un provider abrió por debajo, y es un hecho de
    /// SEGURIDAD: mientras no se diga, el listado de un FTP en claro se lee
    /// igual que el de un SFTP.
    fn take_degraded(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<methods::ConnectionDegraded>>;

    /// Borra UNA entrada: a la papelera o permanente. Devuelve la Task ya
    /// encolada — el desenlace llega por su progreso, no por esta llamada.
    ///
    /// Una por entrada y no un lote porque el método del wire es así; un
    /// borrado de varias marcas son varias Tasks, y el tablero las enseña
    /// todas.
    fn delete(&self, path: VPath, mode: DeleteMode) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// Copia UNA entrada a un destino EXACTO. Devuelve la Task ya encolada.
    ///
    /// `to` es la ruta final, no el directorio: quien llama ya compuso el
    /// nombre. El core solo inventa un nombre libre con
    /// [`norte_proto::CollisionPolicy::RenameAuto`], y con el resto de
    /// políticas jamás lo hace — así que un `to` que sea un directorio
    /// copiaría DENTRO de él sin decirlo, y eso no es lo que este método
    /// promete.
    ///
    /// Una por entrada y no un lote, por el mismo motivo que
    /// [`Self::delete`]: el método del wire es así, y un lote de marcas son
    /// varias Tasks que el tablero enseña todas.
    fn copy(
        &self,
        from: VPath,
        to: VPath,
        on_collision: CollisionPolicy,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// Le pide al modelo un plan de renombrado para un DIRECTORIO.
    ///
    /// NO muta nada: lo que vuelve es una propuesta que hay que revisar,
    /// comprobar contra el core y aprobar. Respuesta DIRECTA y no Task
    /// (ADR 0042): abandonar la espera corta el dispatch en el daemon.
    ///
    /// Lo que vuelve es de un MODELO, o sea lo menos confiable que hay en
    /// todo el sistema: quien llama lo valida entero antes de enseñarlo
    /// (`norte_frontend::validate_ai_plan`), y una sola pareja inválida tumba
    /// el lote — jamás se aplica «lo que valga» de un plan adulterado.
    fn ai_rename_plan(
        &self,
        dir: VPath,
        instruction: String,
    ) -> BoxFuture<'static, Result<methods::AiRenamePlanResult, Error>>;

    /// El plan REVISABLE de un lote de renombrados dentro de `dir`.
    ///
    /// Tampoco muta: lo que se manda es INTENCIÓN —parejas de nombres base—
    /// y lo que vuelve es el veredicto del core (si es aplicable, por qué
    /// no, cuántos pasos son maquinaria) más el `plan_hash` que hay que
    /// devolver para ejecutar EXACTAMENTE lo que se enseñó.
    fn rename_batch_plan(
        &self,
        dir: VPath,
        pairs: Vec<methods::RenamePair>,
    ) -> BoxFuture<'static, Result<methods::FsRenameBatchPlanResult, Error>>;

    /// Ejecuta el lote: UNA Task para todas las parejas, un solo deshacer.
    ///
    /// Se manda la MISMA intención que produjo el `plan_hash`; el ORDEN de
    /// los pasos y los temporales que rompen un ciclo los decide el core y
    /// jamás cruzan el wire.
    fn rename_batch(
        &self,
        dir: VPath,
        pairs: Vec<methods::RenamePair>,
        plan_hash: methods::PlanHash,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// El informe de un lote ya terminado.
    ///
    /// Lo pide el controlador en cuanto una task de clase `rename-batch`
    /// llega a un estado terminal, y lo enseña en la fila del tablero (y
    /// delante, si el lote dejó algo a medias).
    ///
    /// Es la ÚNICA señal de que un lote dejó el directorio a medias, así que
    /// no se degrada en silencio: un daemon que no conozca el método
    /// contesta [`Error::Unsupported`], que quien llama distingue de un fallo
    /// de verdad.
    fn rename_batch_report(
        &self,
        task_id: TaskId,
    ) -> BoxFuture<'static, Result<methods::FsRenameBatchReportResult, Error>>;

    /// Mueve UNA entrada a un destino EXACTO. Mismas reglas que
    /// [`Self::copy`].
    ///
    /// Método aparte y no un `bool` porque son dos verbos distintos en el
    /// wire (`fs.copy` y `fs.move`), dos `TaskKind` distintos en el tablero y
    /// dos entradas de journal distintas. Un parámetro que elige entre
    /// ambos es un sitio donde una copia se convierte en un movimiento.
    fn move_(
        &self,
        from: VPath,
        to: VPath,
        on_collision: CollisionPolicy,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// El catálogo de plugins descubiertos, con su estado aprobado/activo.
    ///
    /// Lo pide la AYUDA, para saber qué extensiones tienen página y cuáles
    /// están encendidas. Un fallo aquí no es un fallo de la ayuda: se pinta
    /// sin páginas de extensión, porque la documentación es cosmética y
    /// jamás tumba nada.
    fn plugin_list(&self) -> BoxFuture<'static, Result<methods::PluginListResult, Error>>;

    /// El `help.md` de UN plugin, bajo demanda.
    ///
    /// `id` es una CLAVE DE BÚSQUEDA contra el catálogo, jamás un trozo de
    /// ruta: quien la manda tiene que haberla validado
    /// ([`norte_proto::methods::is_valid_plugin_id`]), y el daemon la resuelve
    /// contra lo que descubrió.
    ///
    /// El markdown que vuelve NO está enmascarado: es texto de tercero y se
    /// PARSEA antes de pintarse (`norte_help::parse_untrusted`), nunca se
    /// vuelca crudo.
    fn plugin_help(
        &self,
        id: String,
    ) -> BoxFuture<'static, Result<methods::PluginHelpResult, Error>>;

    /// El esquema `[config]` de UN plugin con sus valores EFECTIVOS.
    ///
    /// Las dos cosas en un viaje porque el wire las manda juntas a propósito
    /// (ADR 0037): pintar unos ajustes necesita el tipo y el valor, y pedirlos
    /// por separado es una segunda ida y vuelta para nada.
    ///
    /// Un id desconocido contesta con CERO claves, jamás un error: el mismo
    /// criterio indulgente que `plugin.list` con un catálogo vacío.
    fn plugin_config(
        &self,
        id: String,
    ) -> BoxFuture<'static, Result<methods::PluginGetConfigResult, Error>>;

    /// La PREVIEW con estilo del primer plugin `previewer` que aplique.
    ///
    /// `None` = ninguno aplicó, que no es un error: el visor cae entonces a
    /// leer los bytes él mismo. Un previewer roto tampoco lo es — un plugin
    /// no puede dejar un fichero sin poder mirarse.
    ///
    /// Devuelve LÍNEAS DE SPANS y no HTML ni bytes: el plugin describe y el
    /// host pinta (ADR 0037). El `role` de cada span viene del vocabulario
    /// CERRADO de `norte-theme`, así que un plugin no elige su color, y el
    /// texto es suyo, o sea NO confiable: se enmascara antes de pintarse.
    fn plugin_preview_styled(
        &self,
        path: VPath,
    ) -> BoxFuture<'static, Result<Option<methods::PluginPreviewStyled>, Error>>;

    /// Las DECORACIONES que los plugins ponen sobre un lote de rutas.
    ///
    /// Cosmético y fail-soft por contrato: sin decoradores consentidos, con
    /// el catálogo caído o con la RPC rota, la respuesta es «ninguna» y el
    /// listado se pinta igual. Una insignia que no llega no puede tumbar una
    /// pantalla.
    ///
    /// El lote es la VENTANA VISIBLE, no el directorio: cada llamada levanta
    /// una instancia de wasm por plugin (#224 midió 167 ms por página de 20
    /// sobre 2000 entradas), así que pedirlas para lo que no se ve es pagar
    /// ese precio por nada.
    fn plugin_decorate(
        &self,
        paths: Vec<VPath>,
    ) -> BoxFuture<'static, Result<Vec<methods::PluginDecorations>, Error>>;

    /// Los valores de UNA columna aportada por un plugin, para un lote.
    ///
    /// La forma «sin datos» es un vector de `None` del TAMAÑO de `paths`, no
    /// un vector vacío: el contrato es posicional y quien lo consume espera
    /// siempre una celda por ruta, también cuando la columna no aplica.
    ///
    /// Fail-soft igual que [`Self::plugin_decorate`]: una columna que falla
    /// se queda en blanco, jamás convierte el listado en un error.
    fn plugin_column_values(
        &self,
        plugin: String,
        column: String,
        paths: Vec<VPath>,
    ) -> BoxFuture<'static, Result<Vec<Option<String>>, Error>>;

    /// Los volúmenes del HOST: discos, montajes de red, medios extraíbles.
    ///
    /// No es una llamada de provider y por eso no vive en la familia `fs.*`:
    /// la tabla de montaje es del host, y el daemon solo la contesta a una
    /// conexión de humano — un agente bajo scope no la necesita.
    fn volumes(&self) -> BoxFuture<'static, Result<Vec<methods::Volume>, Error>>;

    /// Lanza una búsqueda por el subárbol y devuelve su Task Y el canal por
    /// el que llegan los LOTES de resultados.
    ///
    /// Los dos juntos porque son una sola cosa: una búsqueda es una tarea
    /// larga cuyo desenlace va por el progreso y cuyos hallazgos van por el
    /// canal. Quedarse con uno solo es no poder cancelarla, o no ver nada.
    fn search(
        &self,
        params: methods::FsSearchParams,
    ) -> BoxFuture<
        'static,
        Result<(HostTask, tokio::sync::mpsc::Receiver<methods::SearchHits>), Error>,
    >;
}

/// El backend de verdad: el SDK.
impl HostBackend for norte_client::RemoteBackend {
    fn list(
        &self,
        dir: VPath,
        attrs: Vec<String>,
    ) -> BoxFuture<'static, Result<(EntryStream, Option<u64>), Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.list_stream(&dir, attrs).await })
    }

    fn stat(&self, path: VPath, attrs: Vec<String>) -> BoxFuture<'static, Result<Entry, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.stat(&path, attrs).await })
    }

    fn read(
        &self,
        path: VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> BoxFuture<'static, Result<Vec<u8>, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.read(&path, range).await })
    }

    fn attr_catalog(&self, dir: VPath) -> BoxFuture<'static, Result<AttrCatalog, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.attr_catalog(&dir).await })
    }

    fn take_approvals(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<methods::PolicyApprovalRequired>> {
        norte_client::RemoteBackend::take_approvals(self)
    }

    fn policy_decide(
        &self,
        approval_id: u64,
        approve: bool,
    ) -> BoxFuture<'static, Result<(), Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.policy_decide(approval_id, approve).await })
    }

    fn mkdir(&self, path: VPath) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.mkdir(&path).await?;
            let canceller = task.canceller();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                foreign: false,
            })
        })
    }

    fn session_get(&self) -> BoxFuture<'static, Result<(methods::Session, bool), Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.session_get().await })
    }

    fn session_put(
        &self,
        version: u32,
        revision: u64,
        body: serde_json::Value,
    ) -> BoxFuture<'static, Result<u64, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.session_put(version, revision, body).await })
    }

    fn take_conn_events(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<ConnEvent>> {
        norte_client::RemoteBackend::take_conn_events(self)
    }

    fn take_degraded(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<methods::ConnectionDegraded>> {
        norte_client::RemoteBackend::take_degraded(self)
    }

    fn take_foreign_tasks(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<HostTask>> {
        let mut origen = norte_client::RemoteBackend::take_foreign_tasks(self)?;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        // Un canal no se mapea en el sitio: el puente es una task de reenvío
        // que muere con el canal que la alimenta.
        tokio::spawn(async move {
            while let Some(t) = origen.recv().await {
                let canceller = t.canceller();
                let task = HostTask {
                    id: t.id(),
                    progress: t.progress(),
                    cancel: Arc::new(move || canceller.cancel()),
                    foreign: true,
                };
                if tx.send(task).is_err() {
                    return;
                }
            }
        });
        Some(rx)
    }

    fn plugin_list(&self) -> BoxFuture<'static, Result<methods::PluginListResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugins_list().await })
    }

    fn plugin_help(
        &self,
        id: String,
    ) -> BoxFuture<'static, Result<methods::PluginHelpResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugin_help(&id).await })
    }

    fn plugin_config(
        &self,
        id: String,
    ) -> BoxFuture<'static, Result<methods::PluginGetConfigResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugin_get_config(&id).await })
    }

    fn plugin_preview_styled(
        &self,
        path: VPath,
    ) -> BoxFuture<'static, Result<Option<methods::PluginPreviewStyled>, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugin_preview_styled(&path).await })
    }

    fn plugin_decorate(
        &self,
        paths: Vec<VPath>,
    ) -> BoxFuture<'static, Result<Vec<methods::PluginDecorations>, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugin_decorate(&paths).await })
    }

    fn plugin_column_values(
        &self,
        plugin: String,
        column: String,
        paths: Vec<VPath>,
    ) -> BoxFuture<'static, Result<Vec<Option<String>>, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugin_column_values(&plugin, &column, &paths).await })
    }

    fn search(
        &self,
        params: methods::FsSearchParams,
    ) -> BoxFuture<
        'static,
        Result<(HostTask, tokio::sync::mpsc::Receiver<methods::SearchHits>), Error>,
    > {
        let backend = self.clone();
        Box::pin(async move {
            let (task, rx) = backend.search(params).await?;
            let canceller = task.canceller();
            Ok((
                HostTask {
                    id: task.id(),
                    progress: task.progress(),
                    cancel: Arc::new(move || canceller.cancel()),
                    foreign: false,
                },
                rx,
            ))
        })
    }

    fn volumes(&self) -> BoxFuture<'static, Result<Vec<methods::Volume>, Error>> {
        let backend = self.clone();
        // Sin los pseudo-sistemas: `proc`, `sysfs` y compañía llenan la lista
        // de sitios a los que nadie quiere ir.
        Box::pin(async move { backend.volumes(false).await })
    }

    fn delete(&self, path: VPath, mode: DeleteMode) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.delete(&path, mode).await?;
            let canceller = task.canceller();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                foreign: false,
            })
        })
    }

    fn copy(
        &self,
        from: VPath,
        to: VPath,
        on_collision: CollisionPolicy,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        transferir(self, Verbo::Copiar, from, to, on_collision)
    }

    fn ai_rename_plan(
        &self,
        dir: VPath,
        instruction: String,
    ) -> BoxFuture<'static, Result<methods::AiRenamePlanResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.ai_rename_plan(&dir, &instruction).await })
    }

    fn rename_batch_plan(
        &self,
        dir: VPath,
        pairs: Vec<methods::RenamePair>,
    ) -> BoxFuture<'static, Result<methods::FsRenameBatchPlanResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.rename_batch_plan(&dir, &pairs).await })
    }

    fn rename_batch(
        &self,
        dir: VPath,
        pairs: Vec<methods::RenamePair>,
        plan_hash: methods::PlanHash,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.rename_batch(&dir, &pairs, &plan_hash).await?;
            let canceller = task.canceller();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                foreign: false,
            })
        })
    }

    fn rename_batch_report(
        &self,
        task_id: TaskId,
    ) -> BoxFuture<'static, Result<methods::FsRenameBatchReportResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.rename_batch_report(task_id).await })
    }

    fn move_(
        &self,
        from: VPath,
        to: VPath,
        on_collision: CollisionPolicy,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        transferir(self, Verbo::Mover, from, to, on_collision)
    }
}

/// Copiar o mover: los dos verbos de una transferencia.
///
/// Un enum y no el nombre del método como cadena. La diferencia importa
/// porque el destino del `else` no es un error visible: es la otra
/// operación, la que además BORRA el origen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verbo {
    /// `fs.copy`.
    Copiar,
    /// `fs.move`.
    Mover,
}

/// El cuerpo COMPARTIDO de copiar y mover sobre el SDK.
///
/// Uno solo porque las dos llamadas se diferencian en el nombre del método y
/// en nada más: el resto —opciones por defecto, envoltura de la Task, el
/// cancelador— tiene que ser idéntico, y dos copias del mismo bloque es el
/// sitio donde `fs.move` se queda sin la política de colisión que `fs.copy`
/// sí manda.
fn transferir(
    backend: &norte_client::RemoteBackend,
    verbo: Verbo,
    from: VPath,
    to: VPath,
    on_collision: CollisionPolicy,
) -> BoxFuture<'static, Result<HostTask, Error>> {
    let backend = backend.clone();
    // El SDK sigue tomando el método como CADENA, y su cuerpo es
    // `if method == FS_COPY { copiar } else { mover }`: cualquier cosa que no
    // sea exactamente la constante de copiar se convierte en un movimiento.
    // Aquí no puede pasar porque lo que entra es un enum de dos variantes y
    // la conversión vive en un sitio; el `else` del SDK está anotado en la
    // issue que propone el enum también allí.
    let metodo = match verbo {
        Verbo::Copiar => norte_proto::methods::FS_COPY,
        Verbo::Mover => norte_proto::methods::FS_MOVE,
    };
    Box::pin(async move {
        let task = backend
            .transfer(
                metodo,
                &from,
                &to,
                norte_client::TransferOptions {
                    on_collision,
                    // El resto, el default del wire: preservar symlinks y no
                    // reanudar. Reanudar es una decisión del usuario (ADR
                    // 0012) y esta ventana todavía no tiene dónde tomarla,
                    // así que se manda lo que el daemon entiende por «no se
                    // pidió» en vez de elegir por él.
                    ..norte_client::TransferOptions::default()
                },
            )
            .await?;
        let canceller = task.canceller();
        Ok(HostTask {
            id: task.id(),
            progress: task.progress(),
            cancel: Arc::new(move || canceller.cancel()),
            foreign: false,
        })
    })
}
