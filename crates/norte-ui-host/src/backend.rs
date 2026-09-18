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
    AttrCatalog, Capabilities, CollisionPolicy, DeleteMode, Entry, Error, TaskId, TaskProgress,
    VPath, methods,
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

    /// Las capacidades de UNA UBICACIÓN (#215): las contesta el mount, no el
    /// provider, así que un pincho FAT bajo un `/home` sensible a la caja no
    /// hereda la respuesta de `/home`.
    ///
    /// Lo que la ventana hace con ellas es plegar nombres como los plegaría el
    /// destino (#268): dos marcas que en un ext4 son `README.txt` y
    /// `readme.txt` son UN nombre en NTFS o APFS, y encolarlas las dos deja
    /// que una gane de forma no determinista mientras la otra falla sin
    /// explicación.
    fn capabilities(&self, path: VPath) -> BoxFuture<'static, Result<Capabilities, Error>>;

    /// Crea UN directorio. Devuelve la Task ya encolada.
    fn mkdir(&self, path: VPath) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// Crea un fichero VACÍO, como Task (#290).
    ///
    /// Falla si el destino existe: crear es una afirmación sobre un nombre
    /// libre, y un método que trunca en silencio es una pérdida de datos con
    /// nombre inocente.
    fn create_file(&self, path: VPath) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// Calcula el sha256 del CONTENIDO de un lote, como Task (#311).
    ///
    /// Los digests NO vuelven aquí: no caben en el desenlace de una Task ni en
    /// su progreso. Se recogen con [`Self::checksum_report`] cuando termina.
    fn checksum(
        &self,
        params: norte_proto::methods::FsChecksumParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// Los digests que calculó esa Task (#311).
    ///
    /// Solo es DEFINITIVO con la Task `Completed` y `pending == 0`: uno de una
    /// Task cancelada está a medias, y compararlo contra un fichero de sumas
    /// acusaría a ficheros que nadie llegó a leer.
    fn checksum_report(
        &self,
        task: norte_proto::TaskId,
    ) -> BoxFuture<'static, Result<norte_proto::methods::FsChecksumReportResult, Error>>;

    /// De qué está hecho un directorio, hijo a hijo, como Task (fase 4).
    ///
    /// Los hijos NO vuelven aquí: una lista no cabe en el desenlace de una
    /// Task ni en su progreso. Se recogen con [`Self::dir_usage_report`].
    fn dir_usage(
        &self,
        params: norte_proto::methods::FsDirUsageParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// El mapa que lleva medido esa Task (fase 4).
    ///
    /// Es un SNAPSHOT: parcial mientras corre —que es lo que hace útil pedirlo,
    /// porque un mapa se va pintando— y definitivo cuando la Task es terminal.
    /// Quien lo aterrice tiene que mirar el estado: uno de una Task cancelada
    /// está a medias, y pintarlo como completo convierte un directorio enorme
    /// en uno pequeño.
    fn dir_usage_report(
        &self,
        task: norte_proto::TaskId,
    ) -> BoxFuture<'static, Result<norte_proto::methods::FsDirUsageReportResult, Error>>;

    /// Cambia los PERMISOS POSIX de un lote, como Task (#314).
    ///
    /// Muta: el core la registra en el journal con su reversa —el modo
    /// anterior— y la pasa por la política. Una ubicación sin permisos POSIX
    /// responde `Unsupported` sin cambiar nada.
    fn set_mode(
        &self,
        params: norte_proto::methods::FsSetModeParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

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

    /// El registro del DAEMON desde `cursor`, como mucho `max` líneas (#328).
    ///
    /// `cursor: None` pide «lo que haya», que es lo que manda un panel al
    /// abrirse, y NO es lo mismo que `Some(0)`: contra un anillo que ya ha
    /// dado la vuelta, un cero reportaría un `lost` falso en el primer sondeo.
    ///
    /// # Errors
    /// [`Error::Unsupported`] cuando el otro extremo no tiene registro que
    /// servir. El caso alcanzable no es un daemon MÁS VIEJO —un cliente 0.65
    /// nunca completa el `initialize` contra uno 0.64— sino uno de la misma
    /// versión compilado sin la feature `logging`. No hay comparación de
    /// versiones en ningún lado: la respuesta al método es la única señal.
    fn log_tail(
        &self,
        cursor: Option<u64>,
        max: u32,
    ) -> BoxFuture<'static, Result<methods::LogTailResult, Error>>;

    /// Sube el nivel que el anillo del daemon guarda, y devuelve el que de
    /// verdad quedó puesto (#328).
    ///
    /// El nivel es GLOBAL al daemon y solo SUBE: pedir menos verbosidad no es
    /// un error y no baja nada, contesta el que ya había. Por eso lo aplica él
    /// y no el cliente — la cota que impide que ahí dentro aparezca una
    /// contraseña vive en el proceso que tiene el anillo.
    ///
    /// # Errors
    /// [`Error::Unsupported`] igual que [`Self::log_tail`]; un nivel fuera del
    /// vocabulario es `InvalidParams`, que es otra pregunta.
    fn log_level(&self, level: String) -> BoxFuture<'static, Result<String, Error>>;

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

    /// El canal de fallos `connection.failed` (#322): POR QUÉ una conexión NO
    /// se pudo abrir.
    ///
    /// Aparte de [`Self::take_degraded`] porque son dos hechos distintos —una
    /// sesión abierta que viaja mal, y una que no llegó a abrirse—, y
    /// mezclarlos hace que uno se pinte como el otro. Sin esto, el fallo llega
    /// como la CATEGORÍA del error (casi siempre `PermissionDenied`), que no
    /// distingue un secreto vacío de una clave equivocada.
    fn take_failed(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<methods::ConnectionFailed>>;

    /// El canal de avisos `plugin.notice` (0.69.0, ADR 0100): lo que un
    /// plugin `hook` quiso decirle al humano sobre una mutación que el
    /// journal ya registró, o que el daemon apagó los hooks de un plugin.
    ///
    /// Aparte de los dos de arriba porque habla de otra cosa: ni de una
    /// sesión ni de una conexión, sino de un fichero que ya cambió. Es un
    /// aviso efímero atribuido a un tercero, jamás un banner.
    fn take_plugin_notices(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<methods::PluginNotice>>;

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

    /// Empaqueta `sources` dentro de un contenedor nuevo, como Task (#132).
    ///
    /// El FORMATO viaja explícito y sale del nombre que se tecleó: empaquetar
    /// en uno que el usuario no pidió es peor que rehusar, así que quien llama
    /// resuelve el nombre ANTES y un nombre sin extensión conocida no llega
    /// aquí.
    fn pack(
        &self,
        params: methods::ArchivePackParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// Comprueba un contenedor, como Task (#132).
    ///
    /// No muta nada: lee el archivo entero y contesta si está sano. Su
    /// resultado, como el de un recuento, viaja en el progreso terminal.
    fn test_archive(
        &self,
        params: methods::ArchiveTestParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// Las conexiones NOMBRADAS que el daemon tiene configuradas (#264).
    ///
    /// Se pregunta en vez de leer `connections.toml`: leerlo metería la pila
    /// de red entera en un binario que solo quiere pintar nombres, y el daemon
    /// ya la tiene porque es quien abre las sesiones.
    ///
    /// No conecta. Devuelve a dónde se PODRÍA ir; ir es navegar a esa URL.
    fn connections(&self) -> BoxFuture<'static, Result<Vec<methods::ConnectionEntry>, Error>>;

    /// Cierra la SESIÓN de una conexión, nombrada por cualquiera de sus rutas
    /// (#140).
    ///
    /// El core la tiene cacheada por `scheme://authority`, así que quien llama
    /// manda el sitio donde está el panel y no tiene que saber cómo se llavea
    /// una sesión por dentro.
    ///
    /// `false` = no había ninguna abierta. No es un fallo, y decir «cerrada»
    /// cuando no se cerró nada enseña a no fiarse del mensaje.
    fn close_connection(&self, path: VPath) -> BoxFuture<'static, Result<bool, Error>>;

    /// Entrega el secreto que una conexión pidió (#325/#327).
    ///
    /// `conn` es el nombre de `connections.toml` que vino en el
    /// `Error::SecretNeeded`, no algo que el servidor remoto haya dicho.
    ///
    /// `secret` viaja en claro porque el core lo necesita en claro para
    /// autenticar; lo que este frontend puede prometer es que su copia se pisa
    /// con ceros al soltarla (`norte_frontend::secret::TypedSecret`) y que
    /// nunca llega a la capa de pintado. De las copias de más allá de aquí
    /// —los params, el frame, el `Value` del daemon— habla el ADR 0015.
    ///
    /// Un core que se NIEGUE a guardarlo llega como error y no como `Ok`: el
    /// SDK ya traduce ese `stored: false`. Tratarlo como éxito dejaría al
    /// usuario reintentando una navegación que nunca va a tener el secreto.
    fn provide_secret(&self, conn: String, secret: String)
    -> BoxFuture<'static, Result<(), Error>>;

    /// Parte un fichero en trozos de `part_bytes`, como Task (#132).
    fn split_file(
        &self,
        params: methods::FileSplitParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// Junta los trozos a partir del PRIMERO, como Task (#132).
    ///
    /// Solo desde el `.001`: el core busca hacia delante, así que empezar por
    /// otro uniría media cosa. Quien llama ya lo comprobó.
    fn combine_files(
        &self,
        params: methods::FileCombineParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// Cuenta lo que ocupan `paths` — bytes y entradas — como Task (#139).
    ///
    /// Es de las pocas Tasks cuyo RESULTADO **es** su progreso: no publica
    /// nada, no muta nada, y lo que quien la lanzó quiere saber viaja en el
    /// progreso terminal. Por eso devuelve la Task y no un total.
    ///
    /// Un lote de verdad y no una Task por ruta, al revés que
    /// [`Self::delete`] y [`Self::copy`]: el método del wire toma una lista,
    /// y contar dos árboles por separado obligaría a quien pregunta a sumar
    /// —y a sumar también los saltados, que no se suman igual—.
    fn dir_size(&self, paths: Vec<VPath>) -> BoxFuture<'static, Result<HostTask, Error>>;

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
    ///
    /// `names` son los basenames MARCADOS (#121). Vacío = el directorio
    /// entero: pedir un plan sobre cinco ficheros no puede mandar los mil del
    /// directorio al proveedor.
    fn ai_rename_plan(
        &self,
        dir: VPath,
        instruction: String,
        names: Vec<String>,
    ) -> BoxFuture<'static, Result<methods::AiRenamePlanResult, Error>>;

    /// El plan que PROPONE un plugin `renamer` (C3, ADR 0095): el mismo
    /// resultado que [`Self::ai_rename_plan`], por otro productor, y con la
    /// misma disciplina al volver — se valida entero antes de enseñarlo.
    fn plugin_rename_plan(
        &self,
        plugin_id: String,
        renamer_id: String,
        dir: VPath,
        names: Vec<String>,
    ) -> BoxFuture<'static, Result<methods::AiRenamePlanResult, Error>>;

    /// El plan de ORGANIZAR que propone un modelo (fase 8): el mismo trato
    /// que renombrar con una libertad más —el destino puede llevar
    /// carpetas—, y por eso su token viaja CON el plan: no hay un segundo
    /// viaje que comprobar.
    fn ai_organize_plan(
        &self,
        dir: VPath,
        instruction: String,
        names: Vec<String>,
    ) -> BoxFuture<'static, Result<methods::AiOrganizePlanResult, Error>>;

    /// El mismo plan, propuesto por un plugin del kind `organizer` (fase 8).
    /// Mismo reparto que el `renamer`: el plugin propone y el core ejecuta.
    ///
    /// **`names` es el operando, y vacío significa vacío**, no «todo»: un
    /// plugin no lista directorios (regla 9), así que lo que no le den no
    /// existe para él y contesta que no mueve nada.
    fn plugin_organize_plan(
        &self,
        plugin_id: String,
        organizer_id: String,
        dir: VPath,
        names: Vec<String>,
    ) -> BoxFuture<'static, Result<methods::AiOrganizePlanResult, Error>>;

    /// Aplica un plan de organizar ya revisado (fase 8): crea las carpetas
    /// que falten y mueve, TODO bajo un solo `batch_id`, así que se deshace
    /// como una unidad.
    fn organize(
        &self,
        dir: VPath,
        moves: Vec<methods::OrganizeMove>,
        plan_hash: methods::PlanHash,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

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

    /// El informe de una Task de UNDO ya terminada (`policy.undo_report`).
    ///
    /// Mismo papel que [`Self::rename_batch_report`] y por el mismo motivo:
    /// el desenlace de la Task dice si el undo corrió, y lo que NO volvió
    /// —una entrada irreversible, un bloqueo a mitad del LIFO, una unidad que
    /// la policy denegó— lo cuenta solo el informe.
    fn undo_report(
        &self,
        task_id: TaskId,
    ) -> BoxFuture<'static, Result<methods::PolicyUndoReportResult, Error>>;

    /// El informe de un `archive.pack` (#250): qué guardó ese empaquetado que
    /// no sobrevive a salir de aquí.
    ///
    /// El tercero de la misma familia, y el que más lejos lleva su motivo: los
    /// otros dos cuentan lo que salió MAL, y este cuenta algo que salió BIEN y
    /// aun así hay que decir — un `a\b.txt` guardado, que en 7-Zip y en el
    /// Explorador es un `b.txt` dentro de una carpeta `a`.
    fn archive_pack_report(
        &self,
        task_id: TaskId,
    ) -> BoxFuture<'static, Result<methods::ArchivePackReportResult, Error>>;

    /// Deshace lo que una sesión de AGENTE hizo, entero, en orden inverso.
    ///
    /// La sesión es una clave OPACA: viene del daemon (en la petición de
    /// aprobación que el agente disparó) y vuelve tal cual. No se compone ni
    /// se recorta — se pinta enmascarada, pero lo que viaja es lo que llegó.
    ///
    /// Devuelve una Task: es una operación larga con su propio informe
    /// (`undo_report`), y lo que no volvió —irreversible, denegado, un LIFO
    /// que paró a mitad— se dice ahí y no en el desenlace de la Task.
    fn undo_session(&self, session: String) -> BoxFuture<'static, Result<HostTask, Error>>;

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

    /// Aprueba o REVOCA las capabilities de un plugin.
    ///
    /// Es LA decisión de seguridad del sistema de extensiones: lo que separa
    /// «este código está en tu disco» de «este código puede leer tus
    /// ficheros». Quien la llame tiene que haberla pedido a un humano —esta
    /// puerta no pregunta— y el core es quien la persiste.
    ///
    /// Revocar no es lo mismo que apagar: apagar deja las capabilities
    /// aprobadas para la próxima vez, revocar las retira.
    /// `expected_digest` es el ancla que la ventana ENSEÑÓ (#282): el core
    /// rehúsa si ya no casa, de modo que lo que se concede sea lo que se leyó.
    fn plugin_set_approval(
        &self,
        id: String,
        approved: bool,
        expected_digest: Option<String>,
    ) -> BoxFuture<'static, Result<(), Error>>;

    /// Enciende o apaga un plugin YA aprobado.
    fn plugin_set_enabled(
        &self,
        id: String,
        enabled: bool,
    ) -> BoxFuture<'static, Result<(), Error>>;

    /// Desinstala un plugin (ADR 0104): borra sus ficheros y retira su
    /// consentimiento. Devuelve si lo tenía. Quien la llame tiene que haberlo
    /// preguntado a un humano — esta puerta no pregunta, y no tiene vuelta.
    fn plugin_uninstall(&self, id: String) -> BoxFuture<'static, Result<bool, Error>>;

    /// Fija UNA clave `[config.<key>]` de un plugin.
    ///
    /// `value` viaja como String SIEMPRE, en la codificación canónica del
    /// wire (`bool` → `"true"`/`"false"`, `int` → decimal). El daemon la
    /// valida contra el ESQUEMA antes de persistirla: la validación de este
    /// lado es para no mandar lo que ya se sabe malo, jamás lo que permite.
    fn plugin_set_config(
        &self,
        id: String,
        key: String,
        value: String,
    ) -> BoxFuture<'static, Result<(), Error>>;

    /// Ejecuta UN comando de un plugin y devuelve su salida.
    ///
    /// La autorización es del SERVIDOR: `plugin.run_command` resuelve el
    /// comando contra el catálogo y exige aprobado + activo por su cuenta.
    /// Lo que una comprobación de este lado compra es coherencia con lo que
    /// el lector está mirando, nunca el permiso.
    ///
    /// La salida es texto de TERCERO: se enmascara y se acota antes de
    /// pintarse, como cualquier otra cosa que escriba un plugin.
    fn plugin_run_command(
        &self,
        id: String,
        command: String,
        arg: String,
    ) -> BoxFuture<'static, Result<String, Error>>;

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
        columns: Option<u32>,
    ) -> BoxFuture<'static, Result<Option<methods::PluginPreviewStyled>, Error>>;

    /// La MINIATURA de un fichero por un plugin (ADR 0107): una imagen ya
    /// verificada por el plugin-host, o `None` si ningún plugin consentido
    /// casa o el que casa no supo. Cosmética y fail-soft como la preview:
    /// sin miniatura, el visor se queda con lo que tenía.
    fn plugin_thumbnail(
        &self,
        path: VPath,
        max_edge: u32,
    ) -> BoxFuture<'static, Result<Option<methods::PluginThumbnail>, Error>>;

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
        kinds: Vec<norte_proto::EntryKind>,
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

    /// El marco que un plugin pinta para su panel (0.74.0, fase 3).
    ///
    /// `None` cuando ningún plugin consentido pinta ese panel, que es el mismo
    /// caso que un daemon más viejo sin el método: en los dos el hueco se
    /// queda con lo que tuviera. Fail-soft como todo lo que decora.
    fn plugin_panel_render(
        &self,
        params: methods::PluginPanelRenderParams,
    ) -> BoxFuture<'static, Result<Option<methods::PanelFrame>, Error>>;

    /// Los volúmenes del HOST: discos, montajes de red, medios extraíbles.
    ///
    /// No es una llamada de provider y por eso no vive en la familia `fs.*`:
    /// la tabla de montaje es del host, y el daemon solo la contesta a una
    /// conexión de humano — un agente bajo scope no la necesita.
    fn volumes(&self) -> BoxFuture<'static, Result<Vec<methods::Volume>, Error>>;

    /// Pide un PLAN de sincronización: su Task y el canal de eventos.
    ///
    /// El plan NO escribe un byte: dice qué haría. Lo que escribe es
    /// `sync.apply` —que este trait todavía no expone, y esa ausencia ES la
    /// frontera de la fase A— y solo contra el `plan_hash` que este plan
    /// cerró.
    fn sync_plan(
        &self,
        params: methods::SyncPlanParams,
    ) -> BoxFuture<
        'static,
        Result<
            (
                HostTask,
                tokio::sync::mpsc::Receiver<norte_client::SyncPlanEvent>,
            ),
            Error,
        >,
    >;

    /// APLICA un plan ya revisado, por su `plan_hash`.
    ///
    /// El hash es un token de FRESCURA, no de aprobación: es público y
    /// determinista, así que lo que garantiza es que se ejecuta el plan que
    /// el re-plan produce AHORA y que un hash aprobado para un directorio no
    /// vale contra otro. Quién puede canjearlo lo decide la policy del core.
    ///
    /// Esto ESCRIBE: es la única llamada de esta superficie que lo hace.
    fn sync_apply(
        &self,
        plan_hash: methods::PlanHash,
    ) -> BoxFuture<'static, Result<HostTask, Error>>;

    /// El informe de una sincronización ya terminada.
    ///
    /// Mismo papel que el informe de un lote de renombrado: el desenlace de
    /// la Task dice si corrió, y lo que NO se hizo —los pasos que fallaron,
    /// lo que quedó sin deshacer— lo cuenta solo el informe.
    fn sync_report(
        &self,
        task_id: TaskId,
    ) -> BoxFuture<'static, Result<methods::SyncReportResult, Error>>;

    /// Compara dos árboles y devuelve su Task Y el canal de LOTES de filas.
    ///
    /// Los dos juntos por lo mismo que en [`Self::search`]: la comparación es
    /// una tarea larga cuyo desenlace va por el progreso y cuyas filas van por
    /// el canal, y quedarse con uno solo es no poder pararla o no ver nada.
    ///
    /// Cancelarla es el ÚNICO freno: el motor emite una fila por nombre
    /// emparejado de todo el árbol y no hay tope —un tope convertiría «¿son
    /// iguales estos dos árboles?» en una respuesta a medias, que es lo único
    /// que esta pregunta no admite—.
    fn compare(
        &self,
        params: methods::FsCompareParams,
    ) -> BoxFuture<
        'static,
        Result<
            (
                HostTask,
                tokio::sync::mpsc::Receiver<methods::CompareRowsBatch>,
            ),
            Error,
        >,
    >;

    /// Búsqueda SEMÁNTICA contra el índice (`index.search_semantic`).
    ///
    /// Respuesta directa y no una Task: el core embebe la consulta y barre el
    /// índice, y lo que vuelve es la lista entera, mejor primero.
    ///
    /// **Sale del proceso**: la consulta va al proveedor de IA configurado.
    /// El daemon solo la atiende a una conexión de humano, y exige que el
    /// índice esté construido y embebido — sin filas contesta `NotFound`, que
    /// es una respuesta que hay que saber leer y no un fallo cualquiera.
    fn semantic_search(
        &self,
        query: String,
        k: u32,
    ) -> BoxFuture<'static, Result<Vec<methods::SemanticHit>, Error>>;

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

    fn capabilities(&self, path: VPath) -> BoxFuture<'static, Result<Capabilities, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.capabilities(&path).await })
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

    fn checksum(
        &self,
        params: norte_proto::methods::FsChecksumParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.checksum(params).await?;
            let canceller = task.canceller();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                foreign: false,
            })
        })
    }

    fn checksum_report(
        &self,
        task: norte_proto::TaskId,
    ) -> BoxFuture<'static, Result<norte_proto::methods::FsChecksumReportResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.checksum_report(task).await })
    }

    fn dir_usage(
        &self,
        params: norte_proto::methods::FsDirUsageParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.dir_usage(params).await?;
            let canceller = task.canceller();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                foreign: false,
            })
        })
    }

    fn dir_usage_report(
        &self,
        task: norte_proto::TaskId,
    ) -> BoxFuture<'static, Result<norte_proto::methods::FsDirUsageReportResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.dir_usage_report(task).await })
    }

    fn set_mode(
        &self,
        params: norte_proto::methods::FsSetModeParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.set_mode(params).await?;
            let canceller = task.canceller();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                foreign: false,
            })
        })
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

    fn create_file(&self, path: VPath) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.create_file(&path).await?;
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

    fn log_tail(
        &self,
        cursor: Option<u64>,
        max: u32,
    ) -> BoxFuture<'static, Result<methods::LogTailResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.log_tail(cursor, max).await })
    }

    fn log_level(&self, level: String) -> BoxFuture<'static, Result<String, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.log_level(&level).await })
    }

    fn take_conn_events(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<ConnEvent>> {
        norte_client::RemoteBackend::take_conn_events(self)
    }

    fn take_degraded(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<methods::ConnectionDegraded>> {
        norte_client::RemoteBackend::take_degraded(self)
    }

    fn take_failed(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<methods::ConnectionFailed>> {
        norte_client::RemoteBackend::take_failed(self)
    }

    fn take_plugin_notices(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<methods::PluginNotice>> {
        norte_client::RemoteBackend::take_plugin_notices(self)
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

    fn plugin_set_approval(
        &self,
        id: String,
        approved: bool,
        expected_digest: Option<String>,
    ) -> BoxFuture<'static, Result<(), Error>> {
        let backend = self.clone();
        Box::pin(async move {
            backend
                .plugins_set_approval(&id, approved, expected_digest.as_deref())
                .await
        })
    }

    fn plugin_set_enabled(
        &self,
        id: String,
        enabled: bool,
    ) -> BoxFuture<'static, Result<(), Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugins_set_enabled(&id, enabled).await })
    }

    fn plugin_uninstall(&self, id: String) -> BoxFuture<'static, Result<bool, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugins_uninstall(&id).await.map(|r| r.was_approved) })
    }

    fn plugin_set_config(
        &self,
        id: String,
        key: String,
        value: String,
    ) -> BoxFuture<'static, Result<(), Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugin_set_config(&id, &key, &value).await })
    }

    fn plugin_run_command(
        &self,
        id: String,
        command: String,
        arg: String,
    ) -> BoxFuture<'static, Result<String, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugin_run_command(&id, &command, &arg).await })
    }

    fn plugin_preview_styled(
        &self,
        path: VPath,
        columns: Option<u32>,
    ) -> BoxFuture<'static, Result<Option<methods::PluginPreviewStyled>, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugin_preview_styled(&path, columns).await })
    }

    fn plugin_thumbnail(
        &self,
        path: VPath,
        max_edge: u32,
    ) -> BoxFuture<'static, Result<Option<methods::PluginThumbnail>, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugin_thumbnail(&path, max_edge).await })
    }

    fn plugin_decorate(
        &self,
        paths: Vec<VPath>,
        kinds: Vec<norte_proto::EntryKind>,
    ) -> BoxFuture<'static, Result<Vec<methods::PluginDecorations>, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.plugin_decorate(&paths, &kinds).await })
    }

    fn semantic_search(
        &self,
        query: String,
        k: u32,
    ) -> BoxFuture<'static, Result<Vec<methods::SemanticHit>, Error>> {
        let backend = self.clone();
        // Sin `root`: el índice entero, igual que el TUI. Acotar por el
        // directorio del panel prometería un alcance que el índice puede no
        // tener — se construye por raíces, no por lo que se está mirando.
        Box::pin(async move { backend.index_search_semantic(None, &query, k).await })
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

    fn plugin_panel_render(
        &self,
        params: methods::PluginPanelRenderParams,
    ) -> BoxFuture<'static, Result<Option<methods::PanelFrame>, Error>> {
        let backend = self.clone();
        // El método INHERENTE del `RemoteBackend`, que gana a este del trait
        // por tener el mismo nombre y la misma firma. Sus vecinos se
        // distinguen solos porque toman referencias; este no, así que si
        // alguien renombra o borra el inherente, esta línea pasa a llamarse a
        // sí misma —compila, y revienta la pila del actor en la primera
        // llamada—.
        Box::pin(
            async move { norte_client::RemoteBackend::plugin_panel_render(&backend, params).await },
        )
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

    fn sync_plan(
        &self,
        params: methods::SyncPlanParams,
    ) -> BoxFuture<
        'static,
        Result<
            (
                HostTask,
                tokio::sync::mpsc::Receiver<norte_client::SyncPlanEvent>,
            ),
            Error,
        >,
    > {
        let backend = self.clone();
        Box::pin(async move {
            let (task, rx) = backend.sync_plan(params).await?;
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

    fn sync_apply(
        &self,
        plan_hash: methods::PlanHash,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.sync_apply(&plan_hash).await?;
            let canceller = task.canceller();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                foreign: false,
            })
        })
    }

    fn sync_report(
        &self,
        task_id: TaskId,
    ) -> BoxFuture<'static, Result<methods::SyncReportResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.sync_report(task_id).await })
    }

    fn compare(
        &self,
        params: methods::FsCompareParams,
    ) -> BoxFuture<
        'static,
        Result<
            (
                HostTask,
                tokio::sync::mpsc::Receiver<methods::CompareRowsBatch>,
            ),
            Error,
        >,
    > {
        let backend = self.clone();
        Box::pin(async move {
            let (task, rx) = backend.compare(params).await?;
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

    fn pack(
        &self,
        params: methods::ArchivePackParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.pack(params).await?;
            let canceller = task.canceller();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                foreign: false,
            })
        })
    }

    fn test_archive(
        &self,
        params: methods::ArchiveTestParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.test_archive(params).await?;
            let canceller = task.canceller();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                foreign: false,
            })
        })
    }

    fn connections(&self) -> BoxFuture<'static, Result<Vec<methods::ConnectionEntry>, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.connections().await })
    }

    fn close_connection(&self, path: VPath) -> BoxFuture<'static, Result<bool, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.close_connection(&path).await })
    }

    fn provide_secret(
        &self,
        conn: String,
        secret: String,
    ) -> BoxFuture<'static, Result<(), Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.provide_secret(&conn, &secret).await })
    }

    fn split_file(
        &self,
        params: methods::FileSplitParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.split_file(params).await?;
            let canceller = task.canceller();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                foreign: false,
            })
        })
    }

    fn combine_files(
        &self,
        params: methods::FileCombineParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.combine_files(params).await?;
            let canceller = task.canceller();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                foreign: false,
            })
        })
    }

    fn dir_size(&self, paths: Vec<VPath>) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.dir_size(methods::FsDirSizeParams { paths }).await?;
            let canceller = task.canceller();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                foreign: false,
            })
        })
    }

    fn ai_rename_plan(
        &self,
        dir: VPath,
        instruction: String,
        names: Vec<String>,
    ) -> BoxFuture<'static, Result<methods::AiRenamePlanResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.ai_rename_plan(&dir, &instruction, &names).await })
    }

    fn plugin_rename_plan(
        &self,
        plugin_id: String,
        renamer_id: String,
        dir: VPath,
        names: Vec<String>,
    ) -> BoxFuture<'static, Result<methods::AiRenamePlanResult, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            backend
                .plugin_rename_plan(&plugin_id, &renamer_id, &dir, &names)
                .await
        })
    }

    fn ai_organize_plan(
        &self,
        dir: VPath,
        instruction: String,
        names: Vec<String>,
    ) -> BoxFuture<'static, Result<methods::AiOrganizePlanResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.ai_organize_plan(&dir, &instruction, &names).await })
    }

    fn plugin_organize_plan(
        &self,
        plugin_id: String,
        organizer_id: String,
        dir: VPath,
        names: Vec<String>,
    ) -> BoxFuture<'static, Result<methods::AiOrganizePlanResult, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            backend
                .plugin_organize_plan(&plugin_id, &organizer_id, &dir, &names)
                .await
        })
    }

    fn organize(
        &self,
        dir: VPath,
        moves: Vec<methods::OrganizeMove>,
        plan_hash: methods::PlanHash,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.organize(&dir, &moves, &plan_hash).await?;
            let canceller = task.canceller();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                foreign: false,
            })
        })
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

    fn undo_session(&self, session: String) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.undo_session(&session).await?;
            let canceller = task.canceller();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
                foreign: false,
            })
        })
    }

    fn undo_report(
        &self,
        task_id: TaskId,
    ) -> BoxFuture<'static, Result<methods::PolicyUndoReportResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.undo_report(task_id).await })
    }

    fn archive_pack_report(
        &self,
        task_id: TaskId,
    ) -> BoxFuture<'static, Result<methods::ArchivePackReportResult, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.archive_pack_report(task_id).await })
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
    // Aquí no puede pasar porque lo que entra es un enum de dos variantes, y
    // desde #270 el SDK también toma un enum: el `else` que convertía
    // cualquier método desconocido en un movimiento ya no existe.
    let metodo = match verbo {
        Verbo::Copiar => norte_client::Transfer::Copy,
        Verbo::Mover => norte_client::Transfer::Move,
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
