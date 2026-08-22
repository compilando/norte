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
    pub error_al_borrar: Option<Error>,
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
    pub plugins: Vec<norte_proto::methods::PluginInfo>,
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
    /// Directorios de plugin que no cargaron: `(dir, motivo)`.
    pub errores_de_carga: Vec<(String, String)>,
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
    fn plugin_list(
        &self,
    ) -> BoxFuture<'static, Result<norte_proto::methods::PluginListResult, Error>> {
        let plugins = self.plugins.clone();
        let errores = self
            .errores_de_carga
            .iter()
            .map(|(dir, reason)| norte_proto::methods::PluginLoadError {
                dir: dir.clone(),
                reason: reason.clone(),
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
        Box::pin(async { Ok(()) })
    }

    fn take_conn_events(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<norte_client::ConnEvent>> {
        self.eventos.lock().expect("eventos").take()
    }

    fn take_foreign_tasks(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<HostTask>> {
        self.ajenas.lock().expect("ajenas").take()
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
        Box::pin(async move {
            if retraso > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(retraso)).await;
            }
            let stream: norte_client::EntryStream =
                Box::pin(futures::stream::iter(entradas.into_iter().map(Ok)));
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
        if let Some(e) = self.error_al_borrar.clone() {
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
}
