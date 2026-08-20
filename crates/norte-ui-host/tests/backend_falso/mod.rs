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
    /// Lo que se pidió borrar, en orden.
    pub borrados: std::sync::Mutex<Vec<(VPath, DeleteMode)>>,
    /// Cuántas veces se pidió cancelar la task que se lanzó.
    pub cancelaciones: Arc<AtomicUsize>,
    /// El emisor del progreso de la última task, para que el test lo mueva.
    pub progreso: std::sync::Mutex<Option<tokio::sync::watch::Sender<norte_proto::TaskProgress>>>,
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
                size: Some(1),
                mtime_ms: None,
                attrs: std::collections::BTreeMap::new(),
            })
            .collect();
        norte_frontend::sort_entries(&mut out);
        out
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
    fn list(&self, dir: VPath) -> BoxFuture<'static, Result<norte_client::EntryStream, Error>> {
        self.listados.fetch_add(1, Ordering::SeqCst);
        if !self.arbol.contains_key(&dir.to_wire()) {
            return Box::pin(async { Err(Error::NotFound) });
        }
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
                size: Some(1),
                mtime_ms: None,
                attrs: std::collections::BTreeMap::new(),
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
            })
        })
    }
}
