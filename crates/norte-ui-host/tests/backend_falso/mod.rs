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
#[derive(Default)]
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
    /// Contenido por path, para el visor.
    pub contenido: HashMap<String, Vec<u8>>,
    /// Los paths que se sondearon, en orden: es lo que permite comprobar que
    /// un sondeo fallido no se repite en bucle.
    pub sondeos: std::sync::Mutex<Vec<VPath>>,
    /// Lo que se pidió borrar, en orden.
    pub borrados: std::sync::Mutex<Vec<(VPath, DeleteMode)>>,
    /// Cuántas veces se pidió cancelar la task que se lanzó.
    pub cancelaciones: Arc<AtomicUsize>,
    /// El emisor del progreso de la última task, para que el test lo mueva.
    pub progreso: std::sync::Mutex<Option<tokio::sync::watch::Sender<norte_proto::TaskProgress>>>,
    /// Los canales de la conexión, para que el test empuje eventos y tasks
    /// ajenas como haría un daemon.
    pub eventos:
        std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<norte_client::ConnEvent>>>,
    pub ajenas: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<HostTask>>>,
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
    fn read(
        &self,
        path: VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> BoxFuture<'static, Result<Vec<u8>, Error>> {
        let bytes = self.contenido.get(&path.to_wire()).cloned();
        Box::pin(async move {
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
        _attrs: Vec<String>,
    ) -> BoxFuture<'static, Result<norte_client::EntryStream, Error>> {
        self.listados.fetch_add(1, Ordering::SeqCst);
        if !self.arbol.contains_key(&dir.to_wire()) {
            return Box::pin(async { Err(Error::NotFound) });
        }
        let lazy = self.lazy;
        // Sin ordenar: ordenar es cosa de `PaneState`, y devolverlo ya
        // ordenado escondería que el host lo delega.
        let entradas: Vec<Entry> = self
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
            .collect();
        let retraso = self.retraso_ms;
        Box::pin(async move {
            if retraso > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(retraso)).await;
            }
            let stream: norte_client::EntryStream =
                Box::pin(futures::stream::iter(entradas.into_iter().map(Ok)));
            Ok(stream)
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

    fn delete(&self, path: VPath, mode: DeleteMode) -> BoxFuture<'static, Result<HostTask, Error>> {
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
