//! Lo que el host le pide al mundo, en la forma MÁS pequeña que le sirve.
//!
//! No es una segunda fachada del SDK: es la lista corta de cosas que el
//! controlador necesita, y existe por una razón concreta —que sus tests sean
//! deterministas sin daemon—. Todo lo demás se le pide al
//! [`norte_client::RemoteBackend`] directamente.

use std::sync::Arc;

use futures::future::BoxFuture;
use norte_client::{ConnEvent, EntryStream};
use norte_proto::{AttrCatalog, DeleteMode, Entry, Error, TaskId, TaskProgress, VPath, methods};
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
    fn list(
        &self,
        dir: VPath,
        attrs: Vec<String>,
    ) -> BoxFuture<'static, Result<EntryStream, Error>>;

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

    /// Borra UNA entrada: a la papelera o permanente. Devuelve la Task ya
    /// encolada — el desenlace llega por su progreso, no por esta llamada.
    ///
    /// Una por entrada y no un lote porque el método del wire es así; un
    /// borrado de varias marcas son varias Tasks, y el tablero las enseña
    /// todas.
    fn delete(&self, path: VPath, mode: DeleteMode) -> BoxFuture<'static, Result<HostTask, Error>>;

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
}

/// El backend de verdad: el SDK.
impl HostBackend for norte_client::RemoteBackend {
    fn list(
        &self,
        dir: VPath,
        attrs: Vec<String>,
    ) -> BoxFuture<'static, Result<EntryStream, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let (stream, _total) = backend.list_stream(&dir, attrs).await?;
            Ok(stream)
        })
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
}
