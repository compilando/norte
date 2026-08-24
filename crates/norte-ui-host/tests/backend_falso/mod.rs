//! El backend de tabla que comparten los tests: un árbol de directorios
//! determinista, sin daemon y sin red.
//!
//! Cada test usa la parte que necesita —el de paridad no borra, el del
//! controlador no compara árboles— así que aquí sobra código para cualquiera
//! de ellos por separado. Es el precio de tener UN falso y no tres.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::future::BoxFuture;
use norte_proto::{DeleteMode, Entry, EntryKind, Error, VPath};
use norte_ui_host::backend::{HostBackend, HostTask};

/// Un pestillo de un solo sentido: se abre una vez y se queda abierto.
///
/// `notify_waiters` solo despierta a quien YA espera, así que la bandera es
/// la que manda y el aviso solo evita el sondeo. Una vez abierta, cualquier
/// petición posterior pasa de largo — que es lo que hace falta cuando el
/// host repide el listado y el stream nuevo vuelve a llegar aquí.
#[derive(Default)]
pub struct Puerta {
    abierta: std::sync::atomic::AtomicBool,
    aviso: tokio::sync::Notify,
}

impl Puerta {
    /// Deja pasar el drenaje, ahora y para siempre.
    pub fn abrir(&self) {
        self.abierta.store(true, Ordering::SeqCst);
        self.aviso.notify_waiters();
    }

    async fn esperar(&self) {
        loop {
            if self.abierta.load(Ordering::SeqCst) {
                return;
            }
            // El futuro se arma ANTES de la segunda comprobación: armarlo
            // después perdería un `abrir` que cayera justo en medio.
            let esperando = self.aviso.notified();
            if self.abierta.load(Ordering::SeqCst) {
                return;
            }
            esperando.await;
        }
    }
}

/// Un backend de tabla: para cada directorio, los nombres que contiene y de
/// qué clase son.
//
// `clippy::struct_excessive_bools`: permitido a propósito. Son MANDOS
// independientes de un doble de test —el listado viene perezoso, el borrado
// quita de verdad, el provider escribe el padre distinto— y cualquier
// combinación de ellos es un escenario real. Plegarlos en una máquina de
// estados sería inventar estados que no existen; envolver cada uno en un enum
// de dos variantes dejaría cada test escribiendo `Lazy::Si, BorrarDeVerdad::No`
// para nada: el nombre del campo ya dice a qué pregunta contesta.
#[derive(Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Falso {
    /// `wire del dir` → `(nombre, es_dir)`.
    pub arbol: HashMap<String, Vec<(Vec<u8>, bool)>>,
    pub listados: AtomicUsize,
    /// Retraso artificial, para provocar la carrera de una respuesta tardía.
    pub retraso_ms: u64,
    /// Detiene el stream JUSTO después de la primera página, hasta que el
    /// test la abre.
    ///
    /// Es la única forma de estar DENTRO de la ventana en la que `en_vuelo`
    /// ya se limpió y `drenando` sigue vivo, que es donde vive el bug que
    /// este mando existe para probar. Un `sleep` valdría de casualidad; esto
    /// no depende del reloj.
    pub puerta_drenaje: Option<Arc<Puerta>>,
    /// La sesión que el daemon devuelve, y si esta ventana es su dueña.
    pub sesion: std::sync::Mutex<(norte_proto::methods::Session, bool)>,
    /// Lo ÚLTIMO que se escribió, para comprobar qué guarda el host.
    pub escrito: std::sync::Mutex<Option<serde_json::Value>>,
    /// La escritura falla con conflicto: otra ventana escribió en medio.
    pub conflicto: bool,
    /// El listado viene PEREZOSO, como el del provider local: sin tamaño ni
    /// fecha. Quien las quiera, que sondee.
    pub lazy: bool,
    /// El `stat` contesta con el nombre en MAYÚSCULAS: otra ortografía de lo
    /// mismo, como un servidor sin distinción de caja o un HFS+ en NFD.
    pub stat_grita: bool,
    /// Los `attrs` que se pidieron en cada listado, en orden.
    pub attrs_pedidos: std::sync::Mutex<Vec<Vec<String>>>,
    /// Los lotes que pidió `dir_size`, en orden: es lo que permite comprobar
    /// que se cuenta lo MARCADO y en UNA sola Task.
    pub recuentos: std::sync::Mutex<Vec<Vec<VPath>>>,
    /// Lo que se pidió empaquetar, con su formato y su base.
    pub empaquetados: std::sync::Mutex<Vec<norte_proto::methods::ArchivePackParams>>,
    /// Los contenedores que se mandó comprobar.
    pub comprobados: std::sync::Mutex<Vec<norte_proto::methods::ArchiveTestParams>>,
    /// Lo que se mandó partir, con su tamaño de trozo ya en bytes.
    pub partidos: std::sync::Mutex<Vec<norte_proto::methods::FileSplitParams>>,
    /// Los trozos que se mandó juntar.
    pub juntados: std::sync::Mutex<Vec<norte_proto::methods::FileCombineParams>>,
    /// Contenido por path, para el visor.
    pub contenido: HashMap<String, Vec<u8>>,
    /// Los paths que se sondearon, en orden: es lo que permite comprobar que
    /// un sondeo fallido no se repite en bucle.
    pub sondeos: std::sync::Mutex<Vec<VPath>>,
    /// Lo que se pidió borrar, en orden.
    pub borrados: std::sync::Mutex<Vec<(VPath, DeleteMode)>>,
    /// Un borrado QUITA la entrada del árbol, como en la vida real.
    ///
    /// Apagado por defecto para no mover los tests que solo miran qué se
    /// pidió. Encendido, es lo único que permite comprobar qué hace un
    /// listado que llega con una entrada MENOS — que es donde un cursor por
    /// índice deja de nombrar el mismo fichero.
    pub borrar_de_verdad: bool,
    /// Con qué error se RECHAZA un borrado antes de encolar nada. `None` =
    /// el borrado se encola.
    ///
    /// Tras un `Mutex` para que un test pueda ARREGLARLO a mitad: el caso que
    /// importa es el de un daemon que rehúsa una vez y acepta la siguiente.
    pub error_al_borrar: std::sync::Mutex<Option<Error>>,
    /// Los wire de lo ya borrado, que `list` se salta.
    pub desaparecidos: std::sync::Mutex<std::collections::HashSet<String>>,
    /// El provider escribe el PADRE de sus entradas con otra ortografía que
    /// la que se le pidió (la última componente en mayúsculas).
    ///
    /// Es lo que pasa de verdad en macOS (NFD contra NFC) y contra un
    /// servidor sin distinción de caja, y lo que hace que el padre de una
    /// entrada y el directorio del panel sean dos cadenas para el mismo
    /// sitio.
    pub padre_distinto: bool,
    /// Cuántas veces se pidió cancelar la task que se lanzó.
    pub cancelaciones: Arc<AtomicUsize>,
    /// El emisor del progreso de la última task, para que el test lo mueva.
    pub progreso: std::sync::Mutex<Option<tokio::sync::watch::Sender<norte_proto::TaskProgress>>>,
    /// Los emisores de TODAS las tasks de transferencia, por id.
    ///
    /// Un solo hueco no vale para un lote: al llegar la segunda se soltaba el
    /// `Sender` de la primera, el bombeo del host veía `changed()` fallar y
    /// esa fila se quedaba `Running` para siempre. O sea que el doble no
    /// podía mover un lote, que es justo el caso caro.
    pub progresos:
        std::sync::Mutex<HashMap<u64, tokio::sync::watch::Sender<norte_proto::TaskProgress>>>,
    /// El catálogo de extensiones que contesta `plugin.list`.
    ///
    /// Tras un `Mutex` porque el gobierno lo CAMBIA: el host repide el
    /// catálogo tras aprobar o encender, y un falso que contestara siempre lo
    /// mismo dejaría pasar una pantalla que dice «aprobada» sin que el
    /// daemon lo hubiera confirmado.
    pub plugins: std::sync::Mutex<Vec<norte_proto::methods::PluginInfo>>,
    /// El `help.md` de cada extensión, por id. Un id ausente contesta como
    /// un daemon que no tiene la página: markdown vacío.
    pub paginas: HashMap<String, String>,
    /// La preview con estilo que contesta un previewer, por wire. Ausente =
    /// ningún previewer aplica, que NO es un error.
    pub previews: HashMap<String, norte_proto::methods::PluginPreviewStyled>,
    /// Cuántas entradas dice el provider que se saltó. `None` = no lleva la
    /// cuenta, que NO es lo mismo que cero.
    pub omitidas: Option<u64>,
    /// La insignia que un decorador pone en cada ruta, por wire. Vacío =
    /// NINGÚN decorador consentido, que es lo que contesta el daemon.
    pub decoraciones: HashMap<String, String>,
    /// Los lotes que se pidieron decorar, en orden. Es lo que permite
    /// comprobar que solo se pide la VENTANA.
    pub decorados: std::sync::Mutex<Vec<Vec<VPath>>>,
    /// El valor de una columna de plugin, por `(columna, wire)`.
    pub valores_de_columna: HashMap<(String, String), String>,
    /// Lo que se pidió a `plugin.column_values`, en orden.
    pub columnas_pedidas: std::sync::Mutex<Vec<(String, String, Vec<VPath>)>>,
    /// Lo que contesta una búsqueda, por patrón: `(glob, hallazgos)`.
    pub hallazgos: HashMap<String, Vec<VPath>>,
    /// Los patrones que se buscaron, en orden.
    pub busquedas: std::sync::Mutex<Vec<String>>,
    /// Los volúmenes que contesta `host.volumes`.
    pub volumenes: Vec<norte_proto::methods::Volume>,
    /// Cómo PLIEGA nombres cada ubicación (#268/#274). Clave: el wire del
    /// directorio. Ausente = lo que dice `Capabilities::default()`.
    ///
    /// Es el mando que faltaba para poder escribir estos tests: sin él ningún
    /// doble podía fingir un APFS, un NTFS o un exFAT, y las fixtures de
    /// gemelos de caja del corpus no tenían contra qué correr.
    pub capacidades: std::collections::HashMap<String, norte_proto::Capabilities>,
    /// Directorios de plugin que no cargaron: `(dir, motivo)`.
    pub errores_de_carga: Vec<(String, String)>,
    /// Los BYTES del directorio de un error de carga (#265), por su cadena.
    /// Lo que un daemon 0.53 manda; ausente = un peer 0.52.
    pub bytes_de_carga: std::collections::HashMap<String, Vec<u8>>,
    /// El esquema `[config]` de cada extensión, por id.
    pub esquemas: HashMap<String, Vec<norte_proto::methods::PluginConfigKeyWire>>,
    /// Lo que contesta `ai.rename_plan`. `None` = el daemon falla.
    pub plan_ia: Option<Vec<(String, String)>>,
    /// Lo que TARDA el modelo. Es lo que abre la ventana en la que el lector
    /// puede descartar la revisión antes de que llegue el plan.
    pub retraso_ia_ms: u64,
    /// Las instrucciones que se pidieron, en orden.
    pub instrucciones: std::sync::Mutex<Vec<String>>,
    /// El veredicto que contesta `fs.rename_batch_plan`. `None` = falla.
    pub veredicto: Option<norte_proto::methods::FsRenameBatchPlanResult>,
    /// Las parejas con las que se pidió el veredicto, en orden.
    pub veredictos_pedidos: std::sync::Mutex<Vec<Vec<norte_proto::methods::RenamePair>>>,
    /// Los lotes que se mandaron EJECUTAR: `(dir, parejas, hash)`.
    pub lotes: std::sync::Mutex<
        Vec<(
            VPath,
            Vec<norte_proto::methods::RenamePair>,
            norte_proto::methods::PlanHash,
        )>,
    >,
    /// El informe que contesta `fs.rename_batch_report`. `None` = el daemon
    /// no sabe informar (`Unsupported`), que es un caso propio: no se puede
    /// confundir con «el lote fue bien».
    pub informe: std::sync::Mutex<Option<norte_proto::methods::FsRenameBatchReportResult>>,
    /// Los ids de task cuyo informe se pidió, en orden.
    pub informes_pedidos: std::sync::Mutex<Vec<u64>>,
    /// Lo que contesta `sync.plan`: sus pasos y el cierre. `None` = el
    /// método falla con `Unsupported`.
    pub plan_de_sync: std::sync::Mutex<
        Option<(
            Vec<norte_proto::methods::SyncStep>,
            norte_proto::methods::SyncPlanDone,
        )>,
    >,
    /// El informe que contesta `sync.report`. `None` = `Unsupported`.
    pub informe_de_sync: std::sync::Mutex<Option<norte_proto::methods::SyncReportResult>>,
    /// Las sesiones de agente que se pidió deshacer, en orden.
    pub deshechas: std::sync::Mutex<Vec<String>>,
    /// Cuántas veces se ha pedido el catálogo de extensiones.
    pub catalogos_pedidos: std::sync::atomic::AtomicU64,
    /// Los cambios de gobierno pedidos, en orden (`approval:id:true`…).
    pub gobierno: std::sync::Mutex<Vec<String>>,
    /// Con qué falla un cambio de gobierno, si falla.
    pub error_al_gobernar: std::sync::Mutex<Option<Error>>,
    /// Las claves escritas, en orden: `(plugin, clave, valor)`.
    pub escrituras: std::sync::Mutex<Vec<(String, String, String)>>,
    /// Con qué falla `plugin.set_config`, si falla.
    pub error_al_escribir: std::sync::Mutex<Option<Error>>,
    /// Los comandos ejecutados, en orden: `(plugin, comando)`.
    pub ejecutados: std::sync::Mutex<Vec<(String, String)>>,
    /// Qué contesta `plugin.run_command`. `None` = la salida vacía, que NO
    /// es un error: un comando puede no imprimir nada.
    pub salida_de_comando: std::sync::Mutex<Option<Result<String, Error>>>,
    /// Con qué falla `sync.apply`, si falla.
    pub error_al_aplicar: std::sync::Mutex<Option<Error>>,
    /// Los ids de task a los que se les pidió parar, en orden.
    pub canceladas_por_id: Arc<std::sync::Mutex<Vec<u64>>>,
    /// Los hashes con los que se pidió aplicar, en orden.
    pub aplicados: std::sync::Mutex<Vec<norte_proto::methods::PlanHash>>,
    /// Los planes que se pidieron: `(origen, destino, modo)`.
    pub planes_pedidos: std::sync::Mutex<Vec<(VPath, VPath, norte_proto::methods::SyncMode)>>,
    /// Las filas que contesta `fs.compare`, en un solo lote. `None` = el
    /// método falla con `Unsupported`.
    pub filas_comparadas: std::sync::Mutex<Option<Vec<norte_proto::methods::CompareRow>>>,
    /// Las comparaciones que se pidieron: `(izquierda, derecha)`.
    pub comparaciones: std::sync::Mutex<Vec<(VPath, VPath)>>,
    /// Lo que contesta `index.search_semantic`. `None` = `NotFound` (no hay
    /// índice), que es el caso que hay que saber leer.
    pub semanticos: std::sync::Mutex<Option<Vec<norte_proto::methods::SemanticHit>>>,
    /// Las consultas semánticas que se pidieron, con su `k`.
    pub semanticas_pedidas: std::sync::Mutex<Vec<(String, u32)>>,
    /// El informe que contesta `policy.undo_report`. `None` = `Unsupported`.
    pub informe_undo: std::sync::Mutex<Option<norte_proto::methods::PolicyUndoReportResult>>,
    /// Los ids de task cuyo informe de undo se pidió, en orden.
    pub informes_undo_pedidos: std::sync::Mutex<Vec<u64>>,
    /// Los ids cuya ficha se pidió, en orden.
    pub fichas_pedidas: std::sync::Mutex<Vec<String>>,
    /// Los ids que se pidieron a `plugin.help`, en orden: es lo que permite
    /// comprobar que una página se pide UNA vez y que un id inválido jamás
    /// llega al wire.
    pub paginas_pedidas: std::sync::Mutex<Vec<String>>,
    /// Los canales de la conexión, para que el test empuje eventos y tasks
    /// ajenas como haría un daemon.
    pub eventos:
        std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<norte_client::ConnEvent>>>,
    pub ajenas: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<HostTask>>>,
    /// El canal de `connection.degraded`, para que el test empuje uno.
    pub degradadas: std::sync::Mutex<
        Option<tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::ConnectionDegraded>>,
    >,
    /// Los directorios que se pidió crear.
    pub creados: std::sync::Mutex<Vec<VPath>>,
    /// El catálogo de atributos que devuelve el falso daemon.
    pub catalogo: std::sync::Mutex<norte_proto::AttrCatalog>,
    /// Con qué error falla `policy.decide`. `None` = la decisión llega.
    pub error_al_decidir: std::sync::Mutex<Option<Error>>,
    /// El canal de aprobaciones, para que el test empuje una.
    pub aprobaciones: std::sync::Mutex<
        Option<tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::PolicyApprovalRequired>>,
    >,
    /// Las decisiones que se mandaron: `(id, aprobada)`.
    pub decisiones: std::sync::Mutex<Vec<(u64, bool)>>,
    /// Lo que se pidió transferir, en orden:
    /// `(origen, destino, mover, política de colisión)`.
    ///
    /// La política se apunta porque es el ÚNICO parámetro que separa «la task
    /// falla» de «el fichero del destino desaparece»: sin clavarla, cambiarla
    /// a `Overwrite` dejaría toda la suite verde.
    pub transferencias: std::sync::Mutex<Vec<(VPath, VPath, bool, norte_proto::CollisionPolicy)>>,
    /// El estado en que NACE la task de una transferencia. `Running` (el
    /// default) deja que el test la mueva; `Failed` es la colisión que
    /// devuelve un daemon con `on_collision = Fail`.
    pub estado_transferencia: Option<norte_proto::TaskState>,
    /// Ids que ya repartió una transferencia: dos copias son dos tasks, y
    /// devolver el mismo id las fundiría en una fila del tablero.
    pub siguiente_task: AtomicUsize,
    /// ENCOLAR una transferencia falla con este error: un provider de solo
    /// lectura, un scope que no llega. No es lo mismo que una task que falla
    /// —esto pasa antes de que haya task— y la pantalla tiene que
    /// distinguirlo.
    pub transferencia_rechazada: Option<Error>,
}

impl Falso {
    /// Un directorio con ficheros sueltos.
    pub fn con(nombres: &[&'static str]) -> Arc<Self> {
        let mut f = Self::default();
        f.pon(
            "mem:///casa",
            nombres.iter().map(|n| (n.as_bytes().to_vec(), false)),
        );
        Arc::new(f)
    }

    pub fn pon(&mut self, dir: &str, entradas: impl IntoIterator<Item = (Vec<u8>, bool)>) {
        self.arbol
            .insert(dir.to_owned(), entradas.into_iter().collect());
    }

    /// El cuerpo compartido de copiar y mover en el falso: apunta lo que se
    /// pidió y devuelve una Task con id PROPIO.
    /// Una task de archivo (empaquetar o comprobar) con su propio id, para que
    /// dos gestos seguidos no se pisen el canal de progreso.
    fn task_de_archivo(
        &self,
        kind: norte_proto::TaskKind,
        id: u64,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let progreso = norte_proto::TaskProgress {
            task_id: norte_proto::TaskId::new(id),
            kind,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progreso);
        *self.progreso.lock().expect("progreso") = Some(tx);
        let cancelaciones = Arc::clone(&self.cancelaciones);
        Box::pin(async move {
            Ok(HostTask {
                id: norte_proto::TaskId::new(id),
                progress: rx,
                cancel: Arc::new(move || {
                    cancelaciones.fetch_add(1, Ordering::SeqCst);
                }),
                foreign: false,
            })
        })
    }

    fn transferir(
        &self,
        from: VPath,
        to: VPath,
        mover: bool,
        on_collision: norte_proto::CollisionPolicy,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        if let Some(e) = self.transferencia_rechazada.clone() {
            return Box::pin(async move { Err(e) });
        }
        self.transferencias
            .lock()
            .expect("transferencias")
            .push((from, to, mover, on_collision));
        let n = self.siguiente_task.fetch_add(1, Ordering::SeqCst);
        let id = norte_proto::TaskId::new(100 + n as u64);
        let progreso = norte_proto::TaskProgress {
            task_id: id,
            kind: if mover {
                norte_proto::TaskKind::Move
            } else {
                norte_proto::TaskKind::Copy
            },
            state: self
                .estado_transferencia
                .clone()
                .unwrap_or(norte_proto::TaskState::Running),
            bytes_done: 0,
            bytes_total: Some(10),
            entries_done: 0,
            entries_total: Some(1),
            current: None,
            unreadable: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progreso);
        *self.progreso.lock().expect("progreso") = Some(tx.clone());
        self.progresos
            .lock()
            .expect("progresos")
            .insert(id.get(), tx);
        let cancelaciones = Arc::clone(&self.cancelaciones);
        Box::pin(async move {
            Ok(HostTask {
                id,
                progress: rx,
                cancel: Arc::new(move || {
                    cancelaciones.fetch_add(1, Ordering::SeqCst);
                }),
                foreign: false,
            })
        })
    }

    pub fn listados(&self) -> usize {
        self.listados.load(Ordering::SeqCst)
    }

    /// Las entradas de un directorio, tal como las devolvería el listado.
    pub fn entradas_de(&self, dir: &VPath) -> Vec<Entry> {
        let mut out: Vec<Entry> = self
            .arbol
            .get(&dir.to_wire())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|(nombre, es_dir)| Entry {
                path: dir.join(norte_proto::Segment::new(nombre).expect("segmento")),
                kind: if es_dir {
                    EntryKind::Dir
                } else {
                    EntryKind::File
                },
                // Un directorio no tiene tamaño, como en la vida real: es lo
                // que hace que la AUSENCIA de celda se pueda probar.
                size: if es_dir || self.lazy { None } else { Some(1) },
                mtime_ms: None,
                attrs: std::collections::BTreeMap::new(),
            })
            .collect();
        norte_frontend::sort_entries(&mut out);
        out
    }
}

/// El mismo path con el último segmento en mayúsculas.
fn otra_ortografia(path: &VPath) -> VPath {
    let Some(nombre) = path.file_name() else {
        return path.clone();
    };
    let gritado: Vec<u8> = nombre.as_bytes().to_ascii_uppercase();
    let Some(padre) = path.parent() else {
        return path.clone();
    };
    match norte_proto::Segment::new(gritado) {
        Ok(seg) => padre.join(seg),
        Err(_) => path.clone(),
    }
}

/// El árbol que usan los escenarios de paridad: un directorio con dos
/// subdirectorios y un nombre hostil.
pub fn arbol_de_prueba() -> Falso {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![
            (b"docs".to_vec(), true),
            (b"fotos".to_vec(), true),
            (b"notas.txt".to_vec(), false),
            (vec![0x63, 0x61, 0x66, 0xC3, 0x28], false),
        ],
    );
    f.pon(
        "mem:///casa/docs",
        vec![(b"a.md".to_vec(), false), (b"b.md".to_vec(), false)],
    );
    f.pon("mem:///casa/fotos", vec![(b"gato.png".to_vec(), false)]);
    f
}

impl HostBackend for Falso {
    fn capabilities(
        &self,
        path: VPath,
    ) -> BoxFuture<'static, Result<norte_proto::Capabilities, Error>> {
        // Por UBICACIÓN, no por provider: se busca el directorio exacto y, si
        // no está, su padre — que es lo que hace un mount de verdad.
        let caps = self
            .capacidades
            .get(&path.to_wire())
            .or_else(|| {
                path.parent()
                    .and_then(|p| self.capacidades.get(&p.to_wire()))
            })
            .copied()
            // Sin mando: lo que dice un ext4 corriente —distingue la caja— que
            // es el suelo honesto para un doble que corre en Linux.
            .unwrap_or(norte_proto::Capabilities {
                flags: norte_proto::CapabilityFlags::CASE_SENSITIVE,
                max_path: None,
            });
        Box::pin(async move { Ok(caps) })
    }

    fn plugin_list(
        &self,
    ) -> BoxFuture<'static, Result<norte_proto::methods::PluginListResult, Error>> {
        self.catalogos_pedidos
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let plugins = self.plugins.lock().expect("plugins").clone();
        let errores = self
            .errores_de_carga
            .iter()
            .map(|(dir, reason)| norte_proto::methods::PluginLoadError {
                dir: dir.clone(),
                reason: reason.clone(),
                dir_bytes: self.bytes_de_carga.get(dir).cloned(),
            })
            .collect();
        Box::pin(async move {
            Ok(norte_proto::methods::PluginListResult {
                plugins,
                errors: errores,
            })
        })
    }

    fn plugin_help(
        &self,
        id: String,
    ) -> BoxFuture<'static, Result<norte_proto::methods::PluginHelpResult, Error>> {
        self.paginas_pedidas
            .lock()
            .expect("mutex de páginas")
            .push(id.clone());
        let markdown = self.paginas.get(&id).cloned().unwrap_or_default();
        Box::pin(async move {
            Ok(norte_proto::methods::PluginHelpResult {
                markdown,
                truncated: false,
                lossy: false,
            })
        })
    }

    fn search(
        &self,
        params: norte_proto::methods::FsSearchParams,
    ) -> BoxFuture<
        'static,
        Result<
            (
                norte_ui_host::backend::HostTask,
                tokio::sync::mpsc::Receiver<norte_proto::methods::SearchHits>,
            ),
            Error,
        >,
    > {
        let patron = params.name_glob.clone().unwrap_or_default();
        self.busquedas
            .lock()
            .expect("mutex de búsquedas")
            .push(patron.clone());
        let hallazgos = self.hallazgos.get(&patron).cloned().unwrap_or_default();
        let cancelaciones = Arc::clone(&self.cancelaciones);
        Box::pin(async move {
            let id = norte_proto::TaskId::new(77);
            let (tx, rx) = tokio::sync::mpsc::channel(8);
            let (ptx, prx) = tokio::sync::watch::channel(norte_proto::TaskProgress {
                task_id: id,
                kind: norte_proto::TaskKind::Search,
                state: norte_proto::TaskState::Running,
                bytes_done: 0,
                bytes_total: None,
                entries_done: 0,
                entries_total: None,
                current: None,
                unreadable: None,
            });
            tokio::spawn(async move {
                let entradas: Vec<norte_proto::Entry> = hallazgos
                    .into_iter()
                    .map(|path| norte_proto::Entry {
                        path,
                        kind: norte_proto::EntryKind::File,
                        size: Some(1),
                        mtime_ms: Some(0),
                        attrs: std::collections::BTreeMap::new(),
                    })
                    .collect();
                // Un lote VACÍO no se manda: `norte-core` corta antes
                // (`if batch.is_empty() { return FlushOutcome::Continue }`),
                // y un doble que sí lo mande esconde todo lo que dependa de
                // que el primer lote llegue. Es la divergencia que tapó que
                // una búsqueda sin hallazgos no se cancelaba nunca.
                if !entradas.is_empty() {
                    let _ = tx
                        .send(norte_proto::methods::SearchHits {
                            task_id: id,
                            entries: entradas,
                            matches: None,
                        })
                        .await;
                }
                // Y termina: la vista deja de decir «buscando…».
                let _ = ptx.send(norte_proto::TaskProgress {
                    task_id: id,
                    kind: norte_proto::TaskKind::Search,
                    state: norte_proto::TaskState::Completed,
                    bytes_done: 0,
                    bytes_total: None,
                    entries_done: 1,
                    entries_total: Some(1),
                    current: None,
                    unreadable: None,
                });
                // El emisor vive lo que la task: soltarlo cierra el canal y
                // eso ES el final de la búsqueda.
                std::mem::forget(ptx);
            });
            Ok((
                norte_ui_host::backend::HostTask {
                    id,
                    progress: prx,
                    cancel: Arc::new(move || {
                        cancelaciones.fetch_add(1, Ordering::SeqCst);
                    }),
                    foreign: false,
                },
                rx,
            ))
        })
    }

    fn plugin_preview_styled(
        &self,
        path: VPath,
    ) -> BoxFuture<'static, Result<Option<norte_proto::methods::PluginPreviewStyled>, Error>> {
        let p = self.previews.get(&path.to_wire()).cloned();
        Box::pin(async move { Ok(p) })
    }

    fn plugin_decorate(
        &self,
        paths: Vec<VPath>,
    ) -> BoxFuture<'static, Result<Vec<norte_proto::methods::PluginDecorations>, Error>> {
        self.decorados
            .lock()
            .expect("mutex de decorados")
            .push(paths.clone());
        let tabla = self.decoraciones.clone();
        Box::pin(async move {
            if tabla.is_empty() {
                // Sin decoradores consentidos: «ninguna», que es lo que
                // contesta el daemon de verdad. NO una lista de vacíos.
                return Ok(Vec::new());
            }
            Ok(vec![norte_proto::methods::PluginDecorations {
                plugin_id: "acme.git".to_owned(),
                decorations: paths
                    .iter()
                    .map(|p| {
                        let d = tabla.get(&p.to_wire()).cloned();
                        norte_proto::methods::DecorationWire {
                            badge: d.clone(),
                            role: d.map(|_| "warning".to_owned()),
                        }
                    })
                    .collect(),
            }])
        })
    }

    fn plugin_column_values(
        &self,
        plugin: String,
        column: String,
        paths: Vec<VPath>,
    ) -> BoxFuture<'static, Result<Vec<Option<String>>, Error>> {
        self.columnas_pedidas
            .lock()
            .expect("mutex de columnas")
            .push((plugin, column.clone(), paths.clone()));
        let tabla = self.valores_de_columna.clone();
        Box::pin(async move {
            // Posicional 1:1 con `paths`, SIEMPRE: es el contrato, y un
            // vector corto es la forma de romperlo sin que se note.
            Ok(paths
                .iter()
                .map(|p| tabla.get(&(column.clone(), p.to_wire())).cloned())
                .collect())
        })
    }

    fn volumes(&self) -> BoxFuture<'static, Result<Vec<norte_proto::methods::Volume>, Error>> {
        let vols = self.volumenes.clone();
        Box::pin(async move { Ok(vols) })
    }

    fn plugin_config(
        &self,
        id: String,
    ) -> BoxFuture<'static, Result<norte_proto::methods::PluginGetConfigResult, Error>> {
        self.fichas_pedidas
            .lock()
            .expect("mutex de fichas")
            .push(id.clone());
        let keys = self.esquemas.get(&id).cloned().unwrap_or_default();
        Box::pin(async move { Ok(norte_proto::methods::PluginGetConfigResult { keys }) })
    }

    fn undo_session(&self, session: String) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.deshechas.lock().expect("deshechas").push(session);
        let n = self.siguiente_task.fetch_add(1, Ordering::SeqCst);
        let id = norte_proto::TaskId::new(900 + n as u64);
        let progreso = norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::Undo,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progreso);
        self.progresos
            .lock()
            .expect("progresos")
            .insert(id.get(), tx);
        Box::pin(async move {
            Ok(HostTask {
                id,
                progress: rx,
                cancel: Arc::new(|| {}),
                foreign: false,
            })
        })
    }

    fn plugin_set_approval(
        &self,
        id: String,
        approved: bool,
        expected_digest: Option<String>,
    ) -> BoxFuture<'static, Result<(), Error>> {
        // El ancla se APUNTA (#282): que la ventana la mande es lo que un test
        // puede afirmar desde aquí, y sin apuntarla el hilo entero sería una
        // cadena de firmas sin nadie que las lea.
        self.gobierno.lock().expect("gobierno").push(format!(
            "approval:{id}:{approved}:{}",
            expected_digest.as_deref().unwrap_or("-")
        ));
        let fallo = self.error_al_gobernar.lock().expect("gobierno").clone();
        // Y el catálogo cambia: el host lo REPIDE tras un OK, así que un
        // falso que contestara siempre lo mismo dejaría pasar una pantalla
        // que dice «aprobada» sin que nadie lo confirmara.
        if fallo.is_none() {
            for p in self.plugins.lock().expect("plugins").iter_mut() {
                if p.id == id {
                    p.approved = approved;
                }
            }
        }
        Box::pin(async move { fallo.map_or(Ok(()), Err) })
    }

    fn plugin_set_enabled(
        &self,
        id: String,
        enabled: bool,
    ) -> BoxFuture<'static, Result<(), Error>> {
        self.gobierno
            .lock()
            .expect("gobierno")
            .push(format!("enabled:{id}:{enabled}"));
        let fallo = self.error_al_gobernar.lock().expect("gobierno").clone();
        if fallo.is_none() {
            for p in self.plugins.lock().expect("plugins").iter_mut() {
                if p.id == id {
                    p.enabled = enabled;
                }
            }
        }
        Box::pin(async move { fallo.map_or(Ok(()), Err) })
    }

    fn plugin_set_config(
        &self,
        id: String,
        key: String,
        value: String,
    ) -> BoxFuture<'static, Result<(), Error>> {
        self.escrituras
            .lock()
            .expect("escrituras")
            .push((id, key, value));
        let fallo = self.error_al_escribir.lock().expect("escribir").clone();
        Box::pin(async move { fallo.map_or(Ok(()), Err) })
    }

    fn plugin_run_command(
        &self,
        id: String,
        command: String,
        _arg: String,
    ) -> BoxFuture<'static, Result<String, Error>> {
        self.ejecutados
            .lock()
            .expect("ejecutados")
            .push((id, command));
        let salida = self.salida_de_comando.lock().expect("salida").clone();
        Box::pin(async move { salida.unwrap_or_else(|| Ok(String::new())) })
    }

    fn read(
        &self,
        path: VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> BoxFuture<'static, Result<Vec<u8>, Error>> {
        let bytes = self.contenido.get(&path.to_wire()).cloned();
        let retraso = self.retraso_ms;
        Box::pin(async move {
            if retraso > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(retraso)).await;
            }
            let mut b = bytes.ok_or(Error::NotFound)?;
            if let Some(r) = range {
                let off = usize::try_from(r.offset).unwrap_or(usize::MAX).min(b.len());
                b = b.split_off(off);
                if let Some(len) = r.len {
                    b.truncate(usize::try_from(len).unwrap_or(usize::MAX));
                }
            }
            Ok(b)
        })
    }

    fn stat(&self, path: VPath, _attrs: Vec<String>) -> BoxFuture<'static, Result<Entry, Error>> {
        self.sondeos.lock().expect("sondeos").push(path.clone());
        let grita = self.stat_grita;
        let retraso = self.retraso_ms;
        // El padre del path dice en qué directorio buscarlo; la entrada sale
        // del mismo árbol, pero AHORA con tamaño: es lo que hace un `stat`.
        let entrada = path.parent().and_then(|dir| {
            self.arbol.get(&dir.to_wire()).and_then(|entradas| {
                entradas
                    .iter()
                    .find(|(n, _)| path.file_name().is_some_and(|f| f.as_bytes() == n))
                    .map(|(_, es_dir)| Entry {
                        // Un provider puede contestar con OTRA ortografía del
                        // mismo nombre; el host tiene que hidratar la entrada
                        // que pidió, no la que le devuelven.
                        path: if grita {
                            otra_ortografia(&path)
                        } else {
                            path.clone()
                        },
                        kind: if *es_dir {
                            EntryKind::Dir
                        } else {
                            EntryKind::File
                        },
                        size: if *es_dir { None } else { Some(1) },
                        mtime_ms: Some(1_700_000_000_000),
                        attrs: std::collections::BTreeMap::new(),
                    })
            })
        });
        Box::pin(async move {
            if retraso > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(retraso)).await;
            }
            entrada.ok_or(Error::NotFound)
        })
    }

    fn attr_catalog(
        &self,
        _dir: VPath,
    ) -> BoxFuture<'static, Result<norte_proto::AttrCatalog, Error>> {
        let c = self.catalogo.lock().expect("catálogo").clone();
        Box::pin(async move { Ok(c) })
    }

    fn take_approvals(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::PolicyApprovalRequired>>
    {
        self.aprobaciones.lock().expect("aprobaciones").take()
    }

    fn policy_decide(
        &self,
        approval_id: u64,
        approve: bool,
    ) -> BoxFuture<'static, Result<(), Error>> {
        self.decisiones
            .lock()
            .expect("decisiones")
            .push((approval_id, approve));
        let fallo = self
            .error_al_decidir
            .lock()
            .expect("error al decidir")
            .clone();
        Box::pin(async move { fallo.map_or(Ok(()), Err) })
    }

    fn take_conn_events(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<norte_client::ConnEvent>> {
        self.eventos.lock().expect("eventos").take()
    }

    fn take_foreign_tasks(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<HostTask>> {
        self.ajenas.lock().expect("ajenas").take()
    }

    fn sync_apply(
        &self,
        plan_hash: norte_proto::methods::PlanHash,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.aplicados.lock().expect("aplicados").push(plan_hash);
        if let Some(e) = self
            .error_al_aplicar
            .lock()
            .expect("error al aplicar")
            .clone()
        {
            return Box::pin(async move { Err(e) });
        }
        let n = self.siguiente_task.fetch_add(1, Ordering::SeqCst);
        let id = norte_proto::TaskId::new(500 + n as u64);
        let progreso = norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::Sync,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progreso);
        *self.progreso.lock().expect("progreso") = Some(tx.clone());
        self.progresos
            .lock()
            .expect("progresos")
            .insert(id.get(), tx);
        // Cancelable DE VERDAD: con un cancelador que no cuenta, un test de
        // cancelación pasa igual con el panel congelado.
        let canceladas = Arc::clone(&self.canceladas_por_id);
        Box::pin(async move {
            Ok(HostTask {
                id,
                progress: rx,
                cancel: Arc::new(move || {
                    canceladas.lock().expect("canceladas").push(id.get());
                }),
                foreign: false,
            })
        })
    }

    fn sync_report(
        &self,
        _task_id: norte_proto::TaskId,
    ) -> BoxFuture<'static, Result<norte_proto::methods::SyncReportResult, Error>> {
        let informe = self.informe_de_sync.lock().expect("informe").clone();
        Box::pin(async move { informe.ok_or(Error::Unsupported) })
    }

    fn sync_plan(
        &self,
        params: norte_proto::methods::SyncPlanParams,
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
        self.planes_pedidos
            .lock()
            .expect("planes")
            .push((params.source, params.dest, params.mode));
        let plan = self.plan_de_sync.lock().expect("plan").clone();
        let n = self.siguiente_task.fetch_add(1, Ordering::SeqCst);
        let id = norte_proto::TaskId::new(400 + n as u64);
        let progreso = norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::SyncPlan,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progreso);
        *self.progreso.lock().expect("progreso") = Some(tx.clone());
        self.progresos
            .lock()
            .expect("progresos")
            .insert(id.get(), tx);
        Box::pin(async move {
            let (pasos, mut done) = plan.ok_or(Error::Unsupported)?;
            // El cierre lleva SU Task: el modelo compartido descarta el de
            // otro plan por este id, que es justo lo que tiene que hacer.
            done.task_id = id;
            let (etx, erx) = tokio::sync::mpsc::channel(4);
            tokio::spawn(async move {
                let _ = etx
                    .send(norte_client::SyncPlanEvent::Steps(
                        norte_proto::methods::SyncStepsBatch {
                            task_id: id,
                            steps: pasos,
                        },
                    ))
                    .await;
                let _ = etx.send(norte_client::SyncPlanEvent::Done(done)).await;
            });
            Ok((
                HostTask {
                    id,
                    progress: rx,
                    cancel: Arc::new(|| {}),
                    foreign: false,
                },
                erx,
            ))
        })
    }

    fn compare(
        &self,
        params: norte_proto::methods::FsCompareParams,
    ) -> BoxFuture<
        'static,
        Result<
            (
                HostTask,
                tokio::sync::mpsc::Receiver<norte_proto::methods::CompareRowsBatch>,
            ),
            Error,
        >,
    > {
        self.comparaciones
            .lock()
            .expect("comparaciones")
            .push((params.left, params.right));
        let filas = self.filas_comparadas.lock().expect("filas").clone();
        let n = self.siguiente_task.fetch_add(1, Ordering::SeqCst);
        let id = norte_proto::TaskId::new(300 + n as u64);
        let progreso = norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::Compare,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progreso);
        *self.progreso.lock().expect("progreso") = Some(tx.clone());
        self.progresos
            .lock()
            .expect("progresos")
            .insert(id.get(), tx);
        Box::pin(async move {
            let filas = filas.ok_or(Error::Unsupported)?;
            let (ftx, frx) = tokio::sync::mpsc::channel(4);
            tokio::spawn(async move {
                let _ = ftx
                    .send(norte_proto::methods::CompareRowsBatch {
                        task_id: id,
                        rows: filas,
                    })
                    .await;
            });
            Ok((
                HostTask {
                    id,
                    progress: rx,
                    cancel: Arc::new(|| {}),
                    foreign: false,
                },
                frx,
            ))
        })
    }

    fn semantic_search(
        &self,
        query: String,
        k: u32,
    ) -> BoxFuture<'static, Result<Vec<norte_proto::methods::SemanticHit>, Error>> {
        self.semanticas_pedidas
            .lock()
            .expect("semánticas")
            .push((query, k));
        let hits = self.semanticos.lock().expect("semánticos").clone();
        Box::pin(async move { hits.ok_or(Error::NotFound) })
    }

    fn take_degraded(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::ConnectionDegraded>>
    {
        self.degradadas.lock().expect("degradadas").take()
    }

    fn mkdir(&self, path: VPath) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.creados.lock().expect("creados").push(path);
        let progreso = norte_proto::TaskProgress {
            task_id: norte_proto::TaskId::new(8),
            kind: norte_proto::TaskKind::Mkdir,
            state: norte_proto::TaskState::Completed,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 1,
            entries_total: Some(1),
            current: None,
            unreadable: None,
        };
        let (_tx, rx) = tokio::sync::watch::channel(progreso);
        Box::pin(async move {
            Ok(HostTask {
                id: norte_proto::TaskId::new(8),
                progress: rx,
                cancel: Arc::new(|| {}),
                foreign: false,
            })
        })
    }

    fn list(
        &self,
        dir: VPath,
        attrs: Vec<String>,
    ) -> BoxFuture<'static, Result<(norte_client::EntryStream, Option<u64>), Error>> {
        self.attrs_pedidos.lock().expect("attrs").push(attrs);
        self.listados.fetch_add(1, Ordering::SeqCst);
        if !self.arbol.contains_key(&dir.to_wire()) {
            return Box::pin(async { Err(Error::NotFound) });
        }
        let lazy = self.lazy;
        // El directorio bajo el que el provider cuelga sus entradas. Con
        // `padre_distinto`, OTRA ortografía del mismo sitio.
        let padre = if self.padre_distinto {
            match dir.file_name() {
                Some(seg) => dir.parent().unwrap_or_else(|| dir.clone()).join(
                    norte_proto::Segment::new(seg.as_bytes().to_ascii_uppercase())
                        .expect("segmento"),
                ),
                None => dir.clone(),
            }
        } else {
            dir.clone()
        };
        let idos = self.desaparecidos.lock().expect("desaparecidos").clone();
        // Sin ordenar: ordenar es cosa de `PaneState`, y devolverlo ya
        // ordenado escondería que el host lo delega.
        let entradas: Vec<Entry> = self
            .arbol
            .get(&dir.to_wire())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|(nombre, es_dir)| Entry {
                path: padre.join(norte_proto::Segment::new(nombre).expect("segmento")),
                kind: if es_dir {
                    EntryKind::Dir
                } else {
                    EntryKind::File
                },
                // Un directorio no tiene tamaño, como en la vida real: es lo
                // que hace que la AUSENCIA de celda se pueda probar. Con
                // `lazy`, tampoco lo tiene un fichero: es el listado del
                // provider local (#52), donde el tamaño se sondea aparte.
                size: if es_dir || lazy { None } else { Some(1) },
                mtime_ms: None,
                attrs: {
                    let mut m = std::collections::BTreeMap::new();
                    // 0o100644: lo que un provider POSIX manda de verdad, y
                    // lo que sin catálogo se pintaría como «33188».
                    m.insert("posix.mode".to_owned(), norte_proto::AttrValue::Uint(33188));
                    m
                },
            })
            .filter(|e| !idos.contains(&e.path.to_wire()))
            .collect();
        let retraso = self.retraso_ms;
        let omitidas = self.omitidas;
        let puerta = self.puerta_drenaje.clone();
        Box::pin(async move {
            if retraso > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(retraso)).await;
            }
            // 100 = `FIRST_PAGE` del host: la entrada 101 es la primera del
            // DRENAJE, y es ahí donde se corta.
            let stream: norte_client::EntryStream = Box::pin(futures::stream::unfold(
                (entradas.into_iter().enumerate(), puerta),
                |(mut it, puerta)| async move {
                    let (i, e) = it.next()?;
                    if i == 100
                        && let Some(p) = &puerta
                    {
                        p.esperar().await;
                    }
                    Some((Ok(e), (it, puerta)))
                },
            ));
            Ok((stream, omitidas))
        })
    }

    fn session_get(
        &self,
    ) -> BoxFuture<'static, Result<(norte_proto::methods::Session, bool), Error>> {
        let s = self.sesion.lock().expect("sesión").clone();
        Box::pin(async move { Ok(s) })
    }

    fn session_put(
        &self,
        _version: u32,
        _revision: u64,
        body: serde_json::Value,
    ) -> BoxFuture<'static, Result<u64, Error>> {
        if self.conflicto {
            return Box::pin(async {
                Err(Error::Conflict {
                    conflict: norte_proto::ConflictKind::Exists,
                })
            });
        }
        *self.escrito.lock().expect("escrito") = Some(body);
        Box::pin(async { Ok(9) })
    }

    fn copy(
        &self,
        from: VPath,
        to: VPath,
        on_collision: norte_proto::CollisionPolicy,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.transferir(from, to, false, on_collision)
    }

    fn ai_rename_plan(
        &self,
        _dir: VPath,
        instruction: String,
    ) -> BoxFuture<'static, Result<norte_proto::methods::AiRenamePlanResult, Error>> {
        self.instrucciones
            .lock()
            .expect("instrucciones")
            .push(instruction);
        let plan = self.plan_ia.clone();
        let retraso = self.retraso_ia_ms;
        Box::pin(async move {
            if retraso > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(retraso)).await;
            }
            let Some(pares) = plan else {
                return Err(Error::Unsupported);
            };
            Ok(norte_proto::methods::AiRenamePlanResult {
                entries: pares
                    .into_iter()
                    .map(|(from, to)| norte_proto::methods::AiRenameEntry { from, to })
                    .collect(),
            })
        })
    }

    fn rename_batch_plan(
        &self,
        _dir: VPath,
        pairs: Vec<norte_proto::methods::RenamePair>,
    ) -> BoxFuture<'static, Result<norte_proto::methods::FsRenameBatchPlanResult, Error>> {
        self.veredictos_pedidos
            .lock()
            .expect("veredictos")
            .push(pairs);
        let v = self.veredicto.clone();
        Box::pin(async move { v.ok_or(Error::Unsupported) })
    }

    fn rename_batch(
        &self,
        dir: VPath,
        pairs: Vec<norte_proto::methods::RenamePair>,
        plan_hash: norte_proto::methods::PlanHash,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.lotes
            .lock()
            .expect("lotes")
            .push((dir, pairs, plan_hash));
        let n = self.siguiente_task.fetch_add(1, Ordering::SeqCst);
        let id = norte_proto::TaskId::new(200 + n as u64);
        let progreso = norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::RenameBatch,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: Some(1),
            current: None,
            unreadable: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progreso);
        *self.progreso.lock().expect("progreso") = Some(tx.clone());
        self.progresos
            .lock()
            .expect("progresos")
            .insert(id.get(), tx);
        Box::pin(async move {
            Ok(HostTask {
                id,
                progress: rx,
                cancel: Arc::new(|| {}),
                foreign: false,
            })
        })
    }

    fn rename_batch_report(
        &self,
        task_id: norte_proto::TaskId,
    ) -> BoxFuture<'static, Result<norte_proto::methods::FsRenameBatchReportResult, Error>> {
        self.informes_pedidos
            .lock()
            .expect("informes")
            .push(task_id.get());
        let informe = self.informe.lock().expect("informe").clone();
        Box::pin(async move { informe.ok_or(Error::Unsupported) })
    }

    fn undo_report(
        &self,
        task_id: norte_proto::TaskId,
    ) -> BoxFuture<'static, Result<norte_proto::methods::PolicyUndoReportResult, Error>> {
        self.informes_undo_pedidos
            .lock()
            .expect("informes undo")
            .push(task_id.get());
        let informe = self.informe_undo.lock().expect("informe undo").clone();
        Box::pin(async move { informe.ok_or(Error::Unsupported) })
    }

    fn move_(
        &self,
        from: VPath,
        to: VPath,
        on_collision: norte_proto::CollisionPolicy,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.transferir(from, to, true, on_collision)
    }

    fn delete(&self, path: VPath, mode: DeleteMode) -> BoxFuture<'static, Result<HostTask, Error>> {
        if let Some(e) = self
            .error_al_borrar
            .lock()
            .expect("error al borrar")
            .clone()
        {
            self.borrados.lock().expect("borrados").push((path, mode));
            return Box::pin(async move { Err(e) });
        }
        if self.borrar_de_verdad {
            self.desaparecidos
                .lock()
                .expect("desaparecidos")
                .insert(path.to_wire());
        }
        self.borrados.lock().expect("borrados").push((path, mode));
        let progreso = norte_proto::TaskProgress {
            task_id: norte_proto::TaskId::new(7),
            kind: norte_proto::TaskKind::Delete,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: Some(10),
            entries_done: 0,
            entries_total: Some(1),
            current: None,
            unreadable: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progreso);
        *self.progreso.lock().expect("progreso") = Some(tx);
        let cancelaciones = Arc::clone(&self.cancelaciones);
        Box::pin(async move {
            Ok(HostTask {
                id: norte_proto::TaskId::new(7),
                progress: rx,
                cancel: Arc::new(move || {
                    cancelaciones.fetch_add(1, Ordering::SeqCst);
                }),
                foreign: false,
            })
        })
    }

    fn pack(
        &self,
        params: norte_proto::methods::ArchivePackParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.empaquetados.lock().expect("empaquetados").push(params);
        self.task_de_archivo(norte_proto::TaskKind::Pack, 11)
    }

    fn test_archive(
        &self,
        params: norte_proto::methods::ArchiveTestParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.comprobados.lock().expect("comprobados").push(params);
        self.task_de_archivo(norte_proto::TaskKind::TestArchive, 12)
    }

    fn split_file(
        &self,
        params: norte_proto::methods::FileSplitParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.partidos.lock().expect("partidos").push(params);
        self.task_de_archivo(norte_proto::TaskKind::Split, 13)
    }

    fn combine_files(
        &self,
        params: norte_proto::methods::FileCombineParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.juntados.lock().expect("juntados").push(params);
        self.task_de_archivo(norte_proto::TaskKind::Combine, 14)
    }

    fn dir_size(&self, paths: Vec<VPath>) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.recuentos.lock().expect("recuentos").push(paths);
        let progreso = norte_proto::TaskProgress {
            task_id: norte_proto::TaskId::new(9),
            kind: norte_proto::TaskKind::DirSize,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progreso);
        *self.progreso.lock().expect("progreso") = Some(tx);
        let cancelaciones = Arc::clone(&self.cancelaciones);
        Box::pin(async move {
            Ok(HostTask {
                id: norte_proto::TaskId::new(9),
                progress: rx,
                cancel: Arc::new(move || {
                    cancelaciones.fetch_add(1, Ordering::SeqCst);
                }),
                foreign: false,
            })
        })
    }
}
