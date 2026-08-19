//! Integración `Engine::dir_size_as` (#139): cuánto ocupa un árbol, contado
//! como Task cancelable, con el TOTAL en el progreso y no en un tipo nuevo.
//!
//! `MemProvider` in-memory → determinista, sin tocar disco.

use std::sync::Arc;

use bytes::Bytes;
use norte_core::{Actor, Engine};
use norte_proto::methods::FsDirSizeParams;
use norte_proto::{Error as ProtoError, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

async fn write_file(mem: &MemProvider, wire: &str, bytes: usize) {
    let mut sink = mem.write(&vp(wire)).await.expect("write abre");
    sink.write(Bytes::from(vec![b'x'; bytes]))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

async fn mkdir(mem: &MemProvider, wire: &str) {
    mem.mkdir(&vp(wire)).await.expect("mkdir");
}

fn setup() -> (Engine, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn norte_vfs::Provider>);
    (engine, mem)
}

fn params(paths: &[&str]) -> FsDirSizeParams {
    FsDirSizeParams {
        paths: paths.iter().map(|p| vp(p)).collect(),
    }
}

/// Lo que la feature promete: el tamaño de un árbol entero, con sus
/// subdirectorios, en el progreso de la Task. Nada de tipos nuevos — el último
/// snapshot ES el resultado.
#[tokio::test]
async fn el_total_de_un_arbol_viaja_en_el_progreso() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///raiz").await;
    mkdir(&mem, "mem:///raiz/sub").await;
    write_file(&mem, "mem:///raiz/a", 10).await;
    write_file(&mem, "mem:///raiz/sub/b", 32).await;
    write_file(&mem, "mem:///raiz/sub/c", 8).await;

    let handle = engine
        .dir_size_as(params(&["mem:///raiz"]), Actor::User)
        .await
        .expect("lanza");
    let prog = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);
    let p = prog.borrow().clone();
    assert_eq!(p.bytes_done, 50, "10 + 32 + 8");
    assert_eq!(p.entries_done, 4, "tres ficheros y el subdirectorio");
    assert_eq!(
        p.bytes_total,
        Some(50),
        "al terminar, el total es lo contado"
    );
    assert_eq!(p.entries_total, Some(4));
}

/// Medir un FICHERO suelto es una pregunta legítima: cuenta su tamaño y no
/// recorre nada.
#[tokio::test]
async fn un_fichero_suelto_cuenta_su_propio_tamano() {
    let (engine, mem) = setup();
    write_file(&mem, "mem:///solo", 7).await;
    let handle = engine
        .dir_size_as(params(&["mem:///solo"]), Actor::User)
        .await
        .expect("lanza");
    let prog = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(prog.borrow().bytes_done, 7);
    assert_eq!(prog.borrow().entries_done, 1);
}

/// VARIAS raíces suman UN número: lo que el humano tiene marcado es una
/// selección, y la pregunta que hace es «¿cuánto ocupa TODO esto?».
/// Dos raíces que se solapan se RECHAZAN, como en `fs.compare` y `sync.plan`
/// (#247).
///
/// Sin esto, `["mem:///p", "mem:///p/sub"]` contaba `sub` DOS veces y devolvía
/// un número mayor que el sitio que ocupa — lo contrario de lo que el método
/// existe para contestar («¿cabe esto en el destino?»). Se rechaza en vez de
/// deduplicar: una selección de panel no anida nunca (son hermanos), así que
/// unas raíces anidadas vienen de un guion, y ahí un error es una respuesta.
#[tokio::test]
async fn dos_raices_que_se_solapan_se_rechazan() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    mkdir(&mem, "mem:///p").await;
    mkdir(&mem, "mem:///p/sub").await;
    write_file(&mem, "mem:///p/sub/b", 23).await;

    let Err(err) = engine
        .dir_size_as(params(&["mem:///p", "mem:///p/sub"]), Actor::User)
        .await
    else {
        panic!("unas raíces anidadas no pueden lanzarse")
    };
    assert!(
        matches!(err, ProtoError::OverlappingRoots { .. }),
        "y lo dice por su nombre: {err:?}"
    );
    // La misma raíz dos veces es el mismo problema con otra cara.
    let Err(err) = engine
        .dir_size_as(params(&["mem:///p", "mem:///p"]), Actor::User)
        .await
    else {
        panic!("la misma raíz dos veces tampoco")
    };
    assert!(
        matches!(err, ProtoError::OverlappingRoots { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn varias_raices_dan_un_solo_total() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///a").await;
    mkdir(&mem, "mem:///b").await;
    write_file(&mem, "mem:///a/uno", 5).await;
    write_file(&mem, "mem:///b/dos", 6).await;
    let handle = engine
        .dir_size_as(params(&["mem:///a", "mem:///b"]), Actor::User)
        .await
        .expect("lanza");
    let prog = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(prog.borrow().bytes_done, 11);
}

/// Sin rutas no hay petición, y se rechaza ANTES de crear Task alguna: es un
/// error del REQUEST, no el fallo de algo ya lanzado.
#[tokio::test]
async fn sin_rutas_no_se_crea_task() {
    let (engine, _mem) = setup();
    let Err(err) = engine.dir_size_as(params(&[]), Actor::User).await else {
        panic!("medir la nada no es una petición");
    };
    assert!(matches!(err, ProtoError::InvalidPath), "{err:?}");
}

/// Regla 3: el recuento se cancela limpiamente, y el estado lo dice.
#[tokio::test]
async fn contar_se_cancela_y_lo_dice() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///grande").await;
    for i in 0..400 {
        write_file(&mem, &format!("mem:///grande/f{i}"), 4).await;
    }
    let handle = engine
        .dir_size_as(params(&["mem:///grande"]), Actor::User)
        .await
        .expect("lanza");
    handle.cancel();
    assert_eq!(handle.join().await, TaskState::Cancelled);
}

/// Una raíz que no existe no tumba el recuento de las demás: una selección de
/// veinte carpetas no se pierde por una. Lo que sale es el tamaño de lo que se
/// pudo leer, que es la respuesta honesta a una pregunta que ya no puede ser
/// exacta.
#[tokio::test]
async fn una_raiz_ilegible_no_tumba_el_recuento() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///buena").await;
    write_file(&mem, "mem:///buena/x", 9).await;
    let handle = engine
        .dir_size_as(params(&["mem:///no-existe", "mem:///buena"]), Actor::User)
        .await
        .expect("lanza");
    let prog = handle.progress();
    assert_eq!(
        handle.join().await,
        TaskState::Completed,
        "la que falta no es un fallo de la Task"
    );
    assert_eq!(prog.borrow().bytes_done, 9);
}

/// Un directorio no suma bytes y sí se cuenta: el número de abajo dice cuántas
/// cosas hay, y el de arriba cuánto ocupan las que ocupan algo.
#[tokio::test]
async fn una_entrada_sin_tamano_se_cuenta_y_no_suma() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///d").await;
    mkdir(&mem, "mem:///d/sub").await;
    write_file(&mem, "mem:///d/f", 3).await;
    let handle = engine
        .dir_size_as(params(&["mem:///d"]), Actor::User)
        .await
        .expect("lanza");
    let prog = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(prog.borrow().bytes_done, 3, "el dir no suma bytes");
    assert_eq!(prog.borrow().entries_done, 2, "pero sí se cuenta");
}

/// Un provider PEREZOSO: su listado no trae tamaños (`None`), como el local
/// real (#52 — `readdir` da el tipo y nada más), y solo `stat` los sabe.
struct Perezoso {
    inner: Arc<MemProvider>,
    /// Cuántos `stat` se pidieron: lo que demuestra que la hidratación existe.
    stats: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl Provider for Perezoso {
    fn scheme(&self) -> &str {
        self.inner.scheme()
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        self.inner.capabilities()
    }
    async fn stat(&self, p: &VPath) -> Result<norte_proto::Entry, ProtoError> {
        self.stats
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.stat(p).await
    }
    async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, ProtoError> {
        use futures::StreamExt as _;
        let mut inner = self.inner.list(p).await?;
        let mut items: Vec<Result<norte_proto::Entry, ProtoError>> = Vec::new();
        while let Some(e) = inner.next().await {
            items.push(e.map(|mut e| {
                e.size = None;
                e.mtime_ms = None;
                e
            }));
        }
        Ok(Box::pin(futures::stream::iter(items)))
    }
    async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, ProtoError> {
        self.inner.read(p, range).await
    }
    async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, ProtoError> {
        self.inner.write(p).await
    }
    async fn mkdir(&self, p: &VPath) -> Result<(), ProtoError> {
        self.inner.mkdir(p).await
    }
    async fn remove(&self, p: &VPath) -> Result<(), ProtoError> {
        self.inner.remove(p).await
    }
    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), ProtoError> {
        self.inner.rename(from, to).await
    }
}

/// **La regresión que encontró pilotar la TUI de verdad**: contra el provider
/// local, un listado NO trae tamaños (#52), así que sumar lo que venía en el
/// listado daba «0 B» para un árbol entero — la respuesta más equivocada
/// posible a la única pregunta que se hizo. Se piden con `stat`, como hace
/// `du` y como hace la hidratación de una copia.
#[tokio::test]
async fn con_un_listado_perezoso_los_tamanos_se_piden() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    let stats = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    engine.register_provider(Arc::new(Perezoso {
        inner: Arc::clone(&mem),
        stats: Arc::clone(&stats),
    }) as Arc<dyn Provider>);
    mkdir(&mem, "mem:///p").await;
    mkdir(&mem, "mem:///p/sub").await;
    write_file(&mem, "mem:///p/a", 100).await;
    write_file(&mem, "mem:///p/sub/b", 23).await;

    let handle = engine
        .dir_size_as(params(&["mem:///p"]), Actor::User)
        .await
        .expect("lanza");
    let prog = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(prog.borrow().bytes_done, 123, "los tamaños se pidieron");
    assert_eq!(prog.borrow().entries_done, 3);
    assert!(
        stats.load(std::sync::atomic::Ordering::Relaxed) >= 2,
        "un stat por fichero sin tamaño"
    );
}
