//! Matriz test-first del copy engine v0 (fase 10 de M0), contra `MemProvider`
//! con fallos inyectados: feliz, recursivo hostil, colisiones, cancelación
//! limpia por chunk, fallos en byte exacto, desconexión, move=rename,
//! delete post-order y `copy_native`.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use norte_core::Engine;
use norte_proto::{CapabilityFlags, ConflictKind, Error, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido de test")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write abre");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk entra");
    sink.commit().await.expect("commit publica");
}

async fn read_all(mem: &MemProvider, wire: &str) -> Result<Vec<u8>, Error> {
    let mut stream = mem.read(&vp(wire), None).await?;
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk?);
    }
    Ok(out)
}

/// Engine con un `MemProvider` registrado; devuelve también el provider.
fn engine_with_mem() -> (Engine, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem)
}

// ---------- copy ----------

#[tokio::test]
async fn copy_file_happy_path() {
    let (engine, mem) = engine_with_mem();
    let content = vec![0xAB; 5000];
    write_file(&mem, "mem:///src.bin", &content).await;

    let handle = engine
        .copy(&vp("mem:///src.bin"), &vp("mem:///dst.bin"))
        .expect("submit");
    let rx = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);

    assert_eq!(read_all(&mem, "mem:///dst.bin").await.unwrap(), content);
    let last = rx.borrow().clone();
    assert_eq!(last.bytes_done, 5000);
    assert_eq!(last.bytes_total, Some(5000));
    assert_eq!(last.entries_total, Some(1));
}

#[tokio::test]
async fn copy_dir_recursive_with_hostile_names() {
    let (engine, mem) = engine_with_mem();
    // Árbol de 3 niveles con nombres hostiles del corpus.
    let hostiles = norte_testkit::corpus::hostile_names();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    mem.mkdir(&vp("mem:///src/sub")).await.unwrap();
    mem.mkdir(&vp("mem:///src/sub/deep")).await.unwrap();
    let root = MemProvider::root();
    let mut paths = Vec::new();
    for (i, h) in hostiles.iter().take(3).enumerate() {
        let dir = ["src", "src/sub", "src/sub/deep"][i];
        let seg = norte_proto::Segment::new(h.bytes.clone()).unwrap();
        let mut p = vp(&format!("mem:///{dir}"));
        p = p.join(seg);
        let mut sink = mem.write(&p).await.expect("write hostil");
        sink.write(Bytes::from_static(b"data")).await.unwrap();
        sink.commit().await.unwrap();
        paths.push(p);
    }
    drop(root);

    let handle = engine
        .copy(&vp("mem:///src"), &vp("mem:///dst"))
        .expect("submit");
    assert_eq!(handle.join().await, TaskState::Completed);

    // Cada archivo hostil existe en el destino con bytes intactos.
    for (i, h) in hostiles.iter().take(3).enumerate() {
        let dir = ["dst", "dst/sub", "dst/sub/deep"][i];
        let seg = norte_proto::Segment::new(h.bytes.clone()).unwrap();
        let p = vp(&format!("mem:///{dir}")).join(seg);
        let e = mem.stat(&p).await.unwrap_or_else(|err| {
            panic!("[{}] falta en el destino: {err:?}", h.id);
        });
        assert_eq!(
            e.path.file_name().unwrap().as_bytes(),
            h.bytes.as_slice(),
            "[{}] bytes intactos",
            h.id
        );
    }
}

#[tokio::test]
async fn copy_collision_fails_without_writing() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///src", b"nuevo").await;
    write_file(&mem, "mem:///dst", b"precioso contenido previo").await;

    let handle = engine.copy(&vp("mem:///src"), &vp("mem:///dst")).unwrap();
    match handle.join().await {
        TaskState::Failed {
            error: Error::Conflict { conflict },
        } => assert_eq!(conflict, ConflictKind::Exists),
        other => panic!("esperaba Conflict, fue {other:?}"),
    }
    // El destino queda EXACTAMENTE como estaba.
    assert_eq!(
        read_all(&mem, "mem:///dst").await.unwrap(),
        b"precioso contenido previo"
    );
}

#[tokio::test]
async fn copy_cancel_leaves_no_partial_destination() {
    let (engine, mem) = engine_with_mem();
    let content = vec![0x5A; 512 * 1024];
    write_file(&mem, "mem:///grande", &content).await;

    let handle = engine
        .copy(&vp("mem:///grande"), &vp("mem:///copia"))
        .unwrap();
    // Cancela en cuanto haya progreso de bytes (el engine chequea por chunk).
    let mut rx = handle.progress();
    loop {
        let snap = rx.borrow_and_update().clone();
        if snap.bytes_done > 0 || snap.state.is_terminal() {
            break;
        }
        if rx.changed().await.is_err() {
            break;
        }
    }
    handle.cancel();
    // Puede haber terminado ya (carrera legítima); si no, debe ser Cancelled.
    let final_state = handle.join().await;
    match final_state {
        TaskState::Cancelled => {
            // Cancelación limpia: ni archivo ni rastro en el destino.
            assert_eq!(
                mem.stat(&vp("mem:///copia")).await.unwrap_err(),
                Error::NotFound,
                "el destino debe quedar limpio tras cancelar"
            );
        }
        TaskState::Completed => {
            assert_eq!(read_all(&mem, "mem:///copia").await.unwrap(), content);
        }
        other => panic!("estado inesperado: {other:?}"),
    }
}

#[tokio::test]
async fn copy_read_fault_fails_and_cleans_destination() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///src", &vec![1u8; 4000]).await;
    mem.faults().fail_read_at(&vp("mem:///src"), 2000);

    let handle = engine.copy(&vp("mem:///src"), &vp("mem:///dst")).unwrap();
    match handle.join().await {
        TaskState::Failed { error } => assert_eq!(error, Error::Io { retryable: false }),
        other => panic!("esperaba Failed{{Io}}, fue {other:?}"),
    }
    assert_eq!(
        mem.stat(&vp("mem:///dst")).await.unwrap_err(),
        Error::NotFound,
        "destino limpio tras fallo de lectura"
    );
}

#[tokio::test]
async fn copy_write_fault_fails_and_cleans_destination() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///src", &vec![2u8; 4000]).await;
    mem.faults().fail_write_at(&vp("mem:///dst"), 1000);

    let handle = engine.copy(&vp("mem:///src"), &vp("mem:///dst")).unwrap();
    match handle.join().await {
        TaskState::Failed { error } => assert_eq!(error, Error::Io { retryable: false }),
        other => panic!("esperaba Failed{{Io}}, fue {other:?}"),
    }
    assert_eq!(
        mem.stat(&vp("mem:///dst")).await.unwrap_err(),
        Error::NotFound,
        "destino limpio tras fallo de escritura"
    );
}

#[tokio::test]
async fn copy_disconnect_maps_to_provider_unavailable() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///src", b"x").await;
    mem.faults().disconnect_after(1);

    let handle = engine.copy(&vp("mem:///src"), &vp("mem:///dst")).unwrap();
    match handle.join().await {
        TaskState::Failed { error } => {
            assert_eq!(error, Error::ProviderUnavailable { retryable: true });
        }
        other => panic!("esperaba ProviderUnavailable, fue {other:?}"),
    }
}

#[tokio::test]
async fn copy_native_used_when_server_copy_declared() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::with_flags(
        CapabilityFlags::SERVER_COPY | CapabilityFlags::CASE_SENSITIVE,
    ));
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    write_file(&mem, "mem:///src", b"contenido nativo").await;
    // Si el engine intentara streaming, este fallo lo tumbaría: copy_native
    // no lee por stream, así que debe completar igual.
    mem.faults().fail_read_at(&vp("mem:///src"), 0);

    let handle = engine.copy(&vp("mem:///src"), &vp("mem:///dst")).unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    mem.faults().clear();
    assert_eq!(
        read_all(&mem, "mem:///dst").await.unwrap(),
        b"contenido nativo"
    );
}

// ---------- move ----------

#[tokio::test]
async fn move_same_provider_is_rename_zero_bytes() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///origen", b"contenido").await;

    let handle = engine
        .move_(&vp("mem:///origen"), &vp("mem:///destino"))
        .unwrap();
    let rx = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);

    assert_eq!(
        mem.stat(&vp("mem:///origen")).await.unwrap_err(),
        Error::NotFound
    );
    assert_eq!(
        read_all(&mem, "mem:///destino").await.unwrap(),
        b"contenido"
    );
    assert_eq!(rx.borrow().bytes_done, 0, "rename no copia bytes");
}

#[tokio::test]
async fn move_collision_fails_and_source_intact() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///a", b"1").await;
    write_file(&mem, "mem:///b", b"2").await;

    let handle = engine.move_(&vp("mem:///a"), &vp("mem:///b")).unwrap();
    match handle.join().await {
        TaskState::Failed {
            error: Error::Conflict { .. },
        } => {}
        other => panic!("esperaba Conflict, fue {other:?}"),
    }
    assert_eq!(read_all(&mem, "mem:///a").await.unwrap(), b"1");
    assert_eq!(read_all(&mem, "mem:///b").await.unwrap(), b"2");
}

// ---------- delete ----------

#[tokio::test]
async fn delete_tree_post_order() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///d")).await.unwrap();
    mem.mkdir(&vp("mem:///d/sub")).await.unwrap();
    write_file(&mem, "mem:///d/f1", b"x").await;
    write_file(&mem, "mem:///d/sub/f2", b"y").await;

    let handle = engine.delete(&vp("mem:///d")).unwrap();
    let rx = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        mem.stat(&vp("mem:///d")).await.unwrap_err(),
        Error::NotFound
    );
    // 4 entradas: d, d/sub, d/f1, d/sub/f2.
    assert_eq!(rx.borrow().entries_total, Some(4));
    assert_eq!(rx.borrow().entries_done, 4);
}

#[tokio::test]
async fn delete_missing_fails_not_found() {
    let (engine, mem) = engine_with_mem();
    let _ = &mem;
    let handle = engine.delete(&vp("mem:///nada")).unwrap();
    assert_eq!(
        handle.join().await,
        TaskState::Failed {
            error: Error::NotFound
        }
    );
}

// ---------- registro / passthrough ----------

#[tokio::test]
async fn unknown_scheme_rejected_at_submit() {
    let (engine, _mem) = engine_with_mem();
    assert!(
        engine
            .copy(&vp("sftp://h/x"), &vp("mem:///y"))
            .is_err_and(|e| e == Error::Unsupported)
    );
    assert!(
        engine
            .copy(&vp("mem:///x"), &vp("sftp://h/y"))
            .is_err_and(|e| e == Error::Unsupported)
    );
}

#[tokio::test]
async fn stat_and_list_passthrough() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///f", b"xyz").await;
    let e = engine.stat(&vp("mem:///f")).await.unwrap();
    assert_eq!(e.size, Some(3));
    let n = engine.list(&vp("mem:///")).await.unwrap().count().await;
    assert_eq!(n, 1);
}

// ---------- cancelación limpia por Task (regla dura 3, hallazgos rust-reviewer) ----------

/// Árbol src con `n` archivos bajo `mem:///src`.
async fn build_tree(mem: &MemProvider, n: usize) {
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    for i in 0..n {
        write_file(mem, &format!("mem:///src/f{i:03}"), b"data").await;
    }
}

#[tokio::test]
async fn copy_tree_cancel_leaves_complete_files_only() {
    let (engine, mem) = engine_with_mem();
    build_tree(&mem, 30).await;
    // Latencia real por op: la task avanza despacio y la cancelación
    // aterriza a mitad de árbol de forma fiable.
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(3)));

    let handle = engine.copy(&vp("mem:///src"), &vp("mem:///dst")).unwrap();
    let mut rx = handle.progress();
    loop {
        let snap = rx.borrow_and_update().clone();
        if snap.entries_done >= 2 || snap.state.is_terminal() {
            break;
        }
        if rx.changed().await.is_err() {
            break;
        }
    }
    handle.cancel();
    let final_state = handle.join().await;
    mem.faults().clear();
    assert_eq!(final_state, TaskState::Cancelled);
    // Árbol parcial permitido (doc de copy_task), pero CADA archivo presente
    // está completo: jamás un archivo a medias sin marcar.
    let mut listed = mem.list(&vp("mem:///dst")).await.unwrap();
    while let Some(e) = listed.next().await {
        let e = e.unwrap();
        if e.kind == norte_proto::EntryKind::File {
            let name = String::from_utf8(e.path.file_name().unwrap().as_bytes().to_vec()).unwrap();
            assert_eq!(
                read_all(&mem, &format!("mem:///dst/{name}")).await.unwrap(),
                b"data",
                "archivo a medias en el destino: {name}"
            );
        }
    }
}

#[tokio::test]
async fn delete_tree_cancel_keeps_root_and_rest_intact() {
    let (engine, mem) = engine_with_mem();
    build_tree(&mem, 30).await;
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(3)));

    let handle = engine.delete(&vp("mem:///src")).unwrap();
    let mut rx = handle.progress();
    loop {
        let snap = rx.borrow_and_update().clone();
        if snap.entries_done >= 2 || snap.state.is_terminal() {
            break;
        }
        if rx.changed().await.is_err() {
            break;
        }
    }
    handle.cancel();
    let final_state = handle.join().await;
    mem.faults().clear();
    assert_eq!(final_state, TaskState::Cancelled);
    // Post-order: la raíz cae la ÚLTIMA — cancelado a mitad, sigue ahí.
    assert!(
        mem.stat(&vp("mem:///src")).await.is_ok(),
        "la raíz solo cae al final; cancelar a mitad la deja"
    );
}

#[tokio::test]
async fn move_cancel_before_start_leaves_everything_intact() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///origen", b"contenido").await;
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(20)));

    let handle = engine
        .move_(&vp("mem:///origen"), &vp("mem:///destino"))
        .unwrap();
    // Cancela inmediatamente: la task lo observa antes del rename.
    handle.cancel();
    let final_state = handle.join().await;
    mem.faults().clear();
    match final_state {
        TaskState::Cancelled => {
            assert_eq!(read_all(&mem, "mem:///origen").await.unwrap(), b"contenido");
            assert_eq!(
                mem.stat(&vp("mem:///destino")).await.unwrap_err(),
                Error::NotFound
            );
        }
        // Carrera legítima: el rename ganó a la cancelación (atómico, limpio).
        TaskState::Completed => {
            assert_eq!(
                read_all(&mem, "mem:///destino").await.unwrap(),
                b"contenido"
            );
        }
        other => panic!("estado inesperado: {other:?}"),
    }
}

#[tokio::test]
async fn copy_dir_into_itself_rejected() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///a")).await.unwrap();
    let handle = engine.copy(&vp("mem:///a"), &vp("mem:///a/b")).unwrap();
    assert_eq!(
        handle.join().await,
        TaskState::Failed {
            error: Error::InvalidPath
        }
    );
    // El árbol queda intacto: sin copia anidada fantasma.
    let n = mem.list(&vp("mem:///a")).await.unwrap().count().await;
    assert_eq!(n, 0);
}

// ---------- deuda dura M0: EXDEV (#3) y move con walk único (#9) ----------

/// Delega TODO en un [`MemProvider`] salvo `rename`, que devuelve
/// `Unsupported` — como un FS real ante EXDEV (montajes distintos).
struct SinRename(Arc<MemProvider>);

#[async_trait::async_trait]
impl Provider for SinRename {
    // La firma del trait es `-> &str`; el literal aquí es correcto.
    #[allow(clippy::unnecessary_literal_bound)]
    fn scheme(&self) -> &str {
        "mem"
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        self.0.capabilities()
    }
    async fn stat(&self, p: &VPath) -> Result<norte_proto::Entry, Error> {
        self.0.stat(p).await
    }
    async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, Error> {
        self.0.list(p).await
    }
    async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, Error> {
        self.0.read(p, range).await
    }
    async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
        self.0.write(p).await
    }
    async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
        self.0.mkdir(p).await
    }
    async fn remove(&self, p: &VPath) -> Result<(), Error> {
        self.0.remove(p).await
    }
    async fn rename(&self, _from: &VPath, _to: &VPath) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
}

/// EXDEV (issue #3): rename imposible en el mismo provider NO es un error
/// terminal — el move degrada a copy+delete.
#[tokio::test]
async fn move_degrades_to_copy_delete_when_rename_unsupported() {
    let engine = Engine::new();
    let inner = Arc::new(MemProvider::new());
    engine.register_provider(Arc::new(SinRename(Arc::clone(&inner))) as Arc<dyn Provider>);
    write_file(&inner, "mem:///origen", b"contenido").await;

    let handle = engine
        .move_(&vp("mem:///origen"), &vp("mem:///destino"))
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);

    assert_eq!(
        read_all(&inner, "mem:///destino").await.unwrap(),
        b"contenido"
    );
    assert_eq!(
        inner.stat(&vp("mem:///origen")).await.unwrap_err(),
        Error::NotFound
    );
}

/// Origen cuyo primer `read` INYECTA un archivo nuevo en el directorio en
/// movimiento: simula una entrada aparecida después del walk de la copia
/// (la ventana del issue #9).
struct InyectaEnRead {
    inner: Arc<MemProvider>,
    hecho: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl Provider for InyectaEnRead {
    // La firma del trait es `-> &str`; el literal aquí es correcto.
    #[allow(clippy::unnecessary_literal_bound)]
    fn scheme(&self) -> &str {
        "src"
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        self.inner.capabilities()
    }
    async fn stat(&self, p: &VPath) -> Result<norte_proto::Entry, Error> {
        self.inner.stat(p).await
    }
    async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, Error> {
        self.inner.list(p).await
    }
    async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, Error> {
        if !self.hecho.swap(true, std::sync::atomic::Ordering::SeqCst) {
            write_file(&self.inner, "src:///dir/tardio", b"llegue tras el walk").await;
        }
        self.inner.read(p, range).await
    }
    async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
        self.inner.write(p).await
    }
    async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
        self.inner.mkdir(p).await
    }
    async fn remove(&self, p: &VPath) -> Result<(), Error> {
        self.inner.remove(p).await
    }
    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
        self.inner.rename(from, to).await
    }
}

/// Issue #9: lo aparecido en el origen DESPUÉS del walk de la copia jamás se
/// borra sin haberse copiado. Con walks separados de copy y delete, `tardio`
/// se borraba en silencio; con el plan único sobrevive (y el move falla con
/// Conflict al no poder vaciar el dir — sin pérdida, jamás en silencio).
#[tokio::test]
async fn move_cross_provider_never_deletes_uncopied_entries() {
    let engine = Engine::new();
    let src_inner = Arc::new(MemProvider::new());
    let dst = Arc::new(MemProvider::new());
    engine.register_provider(Arc::new(InyectaEnRead {
        inner: Arc::clone(&src_inner),
        hecho: std::sync::atomic::AtomicBool::new(false),
    }) as Arc<dyn Provider>);
    engine.register_provider(Arc::clone(&dst) as Arc<dyn Provider>);

    src_inner.mkdir(&vp("src:///dir")).await.unwrap();
    write_file(&src_inner, "src:///dir/a", b"planificado").await;

    let handle = engine.move_(&vp("src:///dir"), &vp("mem:///dir")).unwrap();
    let state = handle.join().await;

    // Lo planificado llegó al destino.
    assert_eq!(
        read_all(&dst, "mem:///dir/a").await.unwrap(),
        b"planificado"
    );
    // `tardio` existe en ALGÚN lado (origen o destino): jamás pérdida muda.
    let en_origen = src_inner.stat(&vp("src:///dir/tardio")).await.is_ok();
    let en_destino = dst.stat(&vp("mem:///dir/tardio")).await.is_ok();
    assert!(
        en_origen || en_destino,
        "entrada tardía borrada sin copiarse (estado: {state:?})"
    );
}

/// Regla 3 para el camino nuevo copy+delete del move (issues #3/#9):
/// cancelar en plena fase DELETE deja destino completo + origen parcial —
/// duplicado, jamás pérdida ni archivo a medias.
#[tokio::test]
async fn move_by_copy_cancel_mid_delete_loses_nothing() {
    let engine = Engine::new();
    let inner = Arc::new(MemProvider::new());
    engine.register_provider(Arc::new(SinRename(Arc::clone(&inner))) as Arc<dyn Provider>);
    build_tree(&inner, 12).await;
    inner
        .faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(3)));

    let handle = engine.move_(&vp("mem:///src"), &vp("mem:///dst")).unwrap();
    let mut rx = handle.progress();
    // Fase copy = 13 pasos (12 archivos + raíz); a partir de 14 la task está
    // borrando el origen.
    loop {
        let snap = rx.borrow_and_update().clone();
        if snap.entries_done >= 14 || snap.state.is_terminal() {
            break;
        }
        if rx.changed().await.is_err() {
            break;
        }
    }
    handle.cancel();
    let state = handle.join().await;
    inner.faults().clear();
    assert_eq!(state, TaskState::Cancelled);
    // Invariante: cada archivo original existe COMPLETO en origen o destino.
    for i in 0..12 {
        let name = format!("f{i:03}");
        let contenido = match read_all(&inner, &format!("mem:///dst/{name}")).await {
            Ok(c) => c,
            Err(_) => read_all(&inner, &format!("mem:///src/{name}"))
                .await
                .unwrap_or_else(|_| panic!("{name} perdido en la cancelación")),
        };
        assert_eq!(contenido, b"data", "{name} a medias");
    }
}

// ---------- fase 2: políticas de colisión, symlinks y reintentos (ADR 0005) ----------

use norte_core::TransferOptions;
use norte_proto::{CollisionPolicy, SymlinkPolicy};

fn on_collision(c: CollisionPolicy) -> TransferOptions {
    TransferOptions {
        on_collision: c,
        ..Default::default()
    }
}

fn on_symlinks(s: SymlinkPolicy) -> TransferOptions {
    TransferOptions {
        symlinks: s,
        ..Default::default()
    }
}

/// Wrapper de delegación pura con scheme propio: dos "providers" distintos
/// sobre árboles Mem independientes para forzar el camino cross-provider.
struct Alias(Arc<MemProvider>);

#[async_trait::async_trait]
impl Provider for Alias {
    // La firma del trait es `-> &str`; el literal aquí es correcto.
    #[allow(clippy::unnecessary_literal_bound)]
    fn scheme(&self) -> &str {
        "src"
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        self.0.capabilities()
    }
    async fn stat(&self, p: &VPath) -> Result<norte_proto::Entry, Error> {
        self.0.stat(p).await
    }
    async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, Error> {
        self.0.list(p).await
    }
    async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, Error> {
        self.0.read(p, range).await
    }
    async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
        self.0.write(p).await
    }
    async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
        self.0.mkdir(p).await
    }
    async fn remove(&self, p: &VPath) -> Result<(), Error> {
        self.0.remove(p).await
    }
    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
        self.0.rename(from, to).await
    }
    async fn read_link(&self, p: &VPath) -> Result<Vec<u8>, Error> {
        self.0.read_link(p).await
    }
    async fn symlink(
        &self,
        link: &VPath,
        target: &[u8],
        kind: norte_vfs::SymlinkKind,
    ) -> Result<(), Error> {
        self.0.symlink(link, target, kind).await
    }
}

/// Dos árboles Mem con schemes distintos, registrados en un engine.
fn engine_cross() -> (Engine, Arc<MemProvider>, Arc<MemProvider>) {
    let engine = Engine::new();
    let src = Arc::new(MemProvider::new());
    let dst = Arc::new(MemProvider::new());
    engine.register_provider(Arc::new(Alias(Arc::clone(&src))) as Arc<dyn Provider>);
    engine.register_provider(Arc::clone(&dst) as Arc<dyn Provider>);
    (engine, src, dst)
}

#[tokio::test]
async fn copy_skip_merges_and_keeps_existing() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    write_file(&mem, "mem:///src/a", b"nuevo").await;
    write_file(&mem, "mem:///src/b", b"extra").await;
    mem.mkdir(&vp("mem:///dst")).await.unwrap();
    write_file(&mem, "mem:///dst/a", b"viejo").await;

    let handle = engine
        .copy_with(
            &vp("mem:///src"),
            &vp("mem:///dst"),
            on_collision(CollisionPolicy::Skip),
        )
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&mem, "mem:///dst/a").await.unwrap(), b"viejo");
    assert_eq!(read_all(&mem, "mem:///dst/b").await.unwrap(), b"extra");
}

#[tokio::test]
async fn copy_overwrite_replaces_colliding_file() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    write_file(&mem, "mem:///src/a", b"nuevo").await;
    mem.mkdir(&vp("mem:///dst")).await.unwrap();
    write_file(&mem, "mem:///dst/a", b"viejo").await;

    let handle = engine
        .copy_with(
            &vp("mem:///src"),
            &vp("mem:///dst"),
            on_collision(CollisionPolicy::Overwrite),
        )
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&mem, "mem:///dst/a").await.unwrap(), b"nuevo");
}

#[tokio::test]
async fn copy_overwrite_file_over_dir_is_type_mismatch() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///a", b"archivo").await;
    mem.mkdir(&vp("mem:///dst")).await.unwrap();
    mem.mkdir(&vp("mem:///dst/a")).await.unwrap();

    let handle = engine
        .copy_with(
            &vp("mem:///a"),
            &vp("mem:///dst/a"),
            on_collision(CollisionPolicy::Overwrite),
        )
        .unwrap();
    match handle.join().await {
        TaskState::Failed {
            error:
                Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                },
        } => {}
        other => panic!("esperaba TypeMismatch, fue {other:?}"),
    }
    // El dir sobrevive: jamás se borra un dir para plantar un archivo.
    assert!(mem.stat(&vp("mem:///dst/a")).await.is_ok());
}

#[tokio::test]
async fn copy_rename_auto_creates_numbered_variant() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///origen.txt", b"v2").await;
    write_file(&mem, "mem:///destino.txt", b"v1").await;

    for esperado in ["mem:///destino (1).txt", "mem:///destino (2).txt"] {
        let handle = engine
            .copy_with(
                &vp("mem:///origen.txt"),
                &vp("mem:///destino.txt"),
                on_collision(CollisionPolicy::RenameAuto),
            )
            .unwrap();
        assert_eq!(handle.join().await, TaskState::Completed);
        assert_eq!(read_all(&mem, esperado).await.unwrap(), b"v2");
    }
    assert_eq!(
        read_all(&mem, "mem:///destino.txt").await.unwrap(),
        b"v1",
        "el original jamás se toca"
    );
}

#[tokio::test]
async fn copy_newer_replaces_only_older_destination() {
    // Caso A: el origen es MÁS NUEVO (se escribió después) → reemplaza.
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///dst-viejo", b"v1").await;
    write_file(&mem, "mem:///src-nuevo", b"v2").await;
    let handle = engine
        .copy_with(
            &vp("mem:///src-nuevo"),
            &vp("mem:///dst-viejo"),
            on_collision(CollisionPolicy::Newer),
        )
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&mem, "mem:///dst-viejo").await.unwrap(), b"v2");

    // Caso B: el destino es más nuevo → skip, contenido intacto.
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///src-viejo", b"v1").await;
    write_file(&mem, "mem:///dst-nuevo", b"v2").await;
    let handle = engine
        .copy_with(
            &vp("mem:///src-viejo"),
            &vp("mem:///dst-nuevo"),
            on_collision(CollisionPolicy::Newer),
        )
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&mem, "mem:///dst-nuevo").await.unwrap(), b"v2");
}

#[tokio::test]
async fn copy_ask_behaves_as_fail_in_m1() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///a", b"1").await;
    write_file(&mem, "mem:///b", b"2").await;
    let handle = engine
        .copy_with(
            &vp("mem:///a"),
            &vp("mem:///b"),
            on_collision(CollisionPolicy::Ask),
        )
        .unwrap();
    match handle.join().await {
        TaskState::Failed {
            error: Error::Conflict { .. },
        } => {}
        other => panic!("esperaba Conflict, fue {other:?}"),
    }
}

#[tokio::test]
async fn symlink_preserve_recreates_link_bytes() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    write_file(&mem, "mem:///src/f", b"contenido").await;
    mem.symlink(&vp("mem:///src/ln"), b"f", norte_vfs::SymlinkKind::File)
        .await
        .unwrap();

    let handle = engine.copy(&vp("mem:///src"), &vp("mem:///dst")).unwrap();
    assert_eq!(
        handle.join().await,
        TaskState::Completed,
        "default Preserve"
    );
    assert_eq!(
        mem.read_link(&vp("mem:///dst/ln")).await.unwrap(),
        b"f",
        "bytes del target intactos"
    );
    assert_eq!(read_all(&mem, "mem:///dst/f").await.unwrap(), b"contenido");
}

#[tokio::test]
async fn symlink_skip_copies_the_rest() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    write_file(&mem, "mem:///src/f", b"contenido").await;
    mem.symlink(&vp("mem:///src/ln"), b"f", norte_vfs::SymlinkKind::File)
        .await
        .unwrap();

    let handle = engine
        .copy_with(
            &vp("mem:///src"),
            &vp("mem:///dst"),
            on_symlinks(SymlinkPolicy::Skip),
        )
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&mem, "mem:///dst/f").await.unwrap(), b"contenido");
    assert_eq!(
        mem.stat(&vp("mem:///dst/ln")).await.unwrap_err(),
        Error::NotFound,
        "el link no se copia"
    );
}

#[tokio::test]
async fn symlink_follow_copies_target_content_as_file() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    write_file(&mem, "mem:///src/f", b"contenido").await;
    mem.symlink(&vp("mem:///src/ln"), b"f", norte_vfs::SymlinkKind::File)
        .await
        .unwrap();

    let handle = engine
        .copy_with(
            &vp("mem:///src"),
            &vp("mem:///dst"),
            on_symlinks(SymlinkPolicy::Follow),
        )
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    let e = mem.stat(&vp("mem:///dst/ln")).await.unwrap();
    assert_eq!(e.kind, norte_proto::EntryKind::File, "contenido, no link");
    assert_eq!(read_all(&mem, "mem:///dst/ln").await.unwrap(), b"contenido");
}

/// M2 fase 1 (#19): Follow sobre dir-symlink ya NO es Unsupported — se
/// expande como dir real (la matriz fina vive en `engine_m2_fase1.rs`).
#[tokio::test]
async fn symlink_follow_dir_symlink_expands() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    mem.mkdir(&vp("mem:///src/sub")).await.unwrap();
    mem.symlink(&vp("mem:///src/ln"), b"sub", norte_vfs::SymlinkKind::Dir)
        .await
        .unwrap();

    let handle = engine
        .copy_with(
            &vp("mem:///src"),
            &vp("mem:///dst"),
            on_symlinks(SymlinkPolicy::Follow),
        )
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    let e = mem.stat(&vp("mem:///dst/ln")).await.unwrap();
    assert_eq!(
        e.kind,
        norte_proto::EntryKind::Dir,
        "expandido como dir real"
    );
}

#[tokio::test]
async fn move_skip_keeps_skipped_in_source() {
    let (engine, src, dst) = engine_cross();
    src.mkdir(&vp("src:///dir")).await.unwrap();
    write_file(&src, "src:///dir/a", b"colisiona").await;
    write_file(&src, "src:///dir/b", b"pasa").await;
    dst.mkdir(&vp("mem:///dir")).await.unwrap();
    write_file(&dst, "mem:///dir/a", b"viejo").await;

    let handle = engine
        .move_with(
            &vp("src:///dir"),
            &vp("mem:///dir"),
            on_collision(CollisionPolicy::Skip),
        )
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    // Lo saltado SIGUE en el origen (jamás se borra sin copiarse).
    assert_eq!(read_all(&src, "src:///dir/a").await.unwrap(), b"colisiona");
    // Lo movido se fue del origen y está en el destino.
    assert_eq!(
        src.stat(&vp("src:///dir/b")).await.unwrap_err(),
        Error::NotFound
    );
    assert_eq!(read_all(&dst, "mem:///dir/b").await.unwrap(), b"pasa");
    assert_eq!(read_all(&dst, "mem:///dir/a").await.unwrap(), b"viejo");
}

#[tokio::test]
async fn move_overwrite_same_provider_replaces() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///a", b"nuevo").await;
    write_file(&mem, "mem:///b", b"viejo").await;
    let handle = engine
        .move_with(
            &vp("mem:///a"),
            &vp("mem:///b"),
            on_collision(CollisionPolicy::Overwrite),
        )
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&mem, "mem:///b").await.unwrap(), b"nuevo");
    assert_eq!(
        mem.stat(&vp("mem:///a")).await.unwrap_err(),
        Error::NotFound
    );
}

// ---------- reintentos con backoff (ADR 0005) ----------

#[tokio::test]
async fn retry_recovers_from_transient_unavailability() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///src.bin", b"datos").await;
    mem.faults().unavailable_for_next(2);

    let handle = engine
        .copy(&vp("mem:///src.bin"), &vp("mem:///dst.bin"))
        .unwrap();
    assert_eq!(
        handle.join().await,
        TaskState::Completed,
        "reintenta y pasa"
    );
    assert_eq!(read_all(&mem, "mem:///dst.bin").await.unwrap(), b"datos");
}

#[tokio::test]
async fn retry_gives_up_against_permanent_outage() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///src.bin", b"datos").await;
    mem.faults().disconnect_after(0);

    let handle = engine
        .copy(&vp("mem:///src.bin"), &vp("mem:///dst.bin"))
        .unwrap();
    match handle.join().await {
        TaskState::Failed {
            error: Error::ProviderUnavailable { .. },
        } => {}
        other => panic!("esperaba ProviderUnavailable, fue {other:?}"),
    }
}

#[tokio::test]
async fn cancel_during_backoff_is_prompt() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///src.bin", b"datos").await;
    mem.faults().unavailable_for_next(u64::MAX);

    let inicio = std::time::Instant::now();
    let handle = engine
        .copy(&vp("mem:///src.bin"), &vp("mem:///dst.bin"))
        .unwrap();
    // Deja a la task entrar en la espera del backoff y cancela.
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    handle.cancel();
    assert_eq!(handle.join().await, TaskState::Cancelled);
    assert!(
        inicio.elapsed() < std::time::Duration::from_millis(500),
        "la cancelación no espera a que el backoff termine"
    );
}

// ---------- hallazgos de revisión fase 2 ----------

/// B1: copiar algo SOBRE SÍ MISMO con Overwrite jamás puede destruir el
/// origen — es `InvalidPath`, con el contenido intacto.
#[tokio::test]
async fn copy_overwrite_onto_itself_never_destroys() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///unico", b"precioso").await;
    let handle = engine
        .copy_with(
            &vp("mem:///unico"),
            &vp("mem:///unico"),
            on_collision(CollisionPolicy::Overwrite),
        )
        .unwrap();
    assert_eq!(
        handle.join().await,
        TaskState::Failed {
            error: Error::InvalidPath
        }
    );
    assert_eq!(read_all(&mem, "mem:///unico").await.unwrap(), b"precioso");
}

/// B1 variante caja: en provider case-insensitive, `a → A` es el MISMO nodo.
#[tokio::test]
async fn copy_overwrite_case_variant_of_itself_never_destroys() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::with_flags(
        CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING,
    ));
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    write_file(&mem, "mem:///unico", b"precioso").await;
    let handle = engine
        .copy_with(
            &vp("mem:///UNICO"),
            &vp("mem:///unico"),
            on_collision(CollisionPolicy::Overwrite),
        )
        .unwrap();
    let state = handle.join().await;
    assert!(
        matches!(state, TaskState::Failed { .. }),
        "copiarse sobre sí mismo no puede 'funcionar': {state:?}"
    );
    assert_eq!(read_all(&mem, "mem:///unico").await.unwrap(), b"precioso");
}

/// B1 variante normalización: NFC → NFD del mismo nodo en Mem insensitive.
#[tokio::test]
async fn copy_overwrite_normalization_variant_never_destroys() {
    use norte_testkit::Normalization;
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new().with_normalization(Normalization::Insensitive));
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    // é NFC
    let nfc = "mem:///%C3%A9";
    let nfd = "mem:///e%CC%81";
    write_file(&mem, nfc, b"precioso").await;
    let handle = engine
        .copy_with(&vp(nfc), &vp(nfd), on_collision(CollisionPolicy::Overwrite))
        .unwrap();
    let state = handle.join().await;
    assert!(
        matches!(state, TaskState::Failed { .. }),
        "variante de normalización del propio origen: {state:?}"
    );
    assert_eq!(read_all(&mem, nfc).await.unwrap(), b"precioso");
}

/// M1: move same-provider de un DIR sobre un ARCHIVO con Overwrite es
/// `TypeMismatch` — jamás se borra el archivo para plantar el dir.
#[tokio::test]
async fn move_overwrite_dir_over_file_is_type_mismatch() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///carpeta")).await.unwrap();
    write_file(&mem, "mem:///ocupado", b"archivo").await;
    let handle = engine
        .move_with(
            &vp("mem:///carpeta"),
            &vp("mem:///ocupado"),
            on_collision(CollisionPolicy::Overwrite),
        )
        .unwrap();
    match handle.join().await {
        TaskState::Failed {
            error:
                Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                },
        } => {}
        other => panic!("esperaba TypeMismatch, fue {other:?}"),
    }
    assert_eq!(read_all(&mem, "mem:///ocupado").await.unwrap(), b"archivo");
}

/// M2: Follow + Overwrite sobre un dir-symlink NO destruye el destino:
/// el sondeo del target ocurre ANTES de cualquier acción destructiva.
#[tokio::test]
async fn follow_dir_symlink_with_overwrite_leaves_destination_intact() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    mem.mkdir(&vp("mem:///src/sub")).await.unwrap();
    mem.symlink(&vp("mem:///src/ln"), b"sub", norte_vfs::SymlinkKind::Dir)
        .await
        .unwrap();
    mem.mkdir(&vp("mem:///dst")).await.unwrap();
    write_file(&mem, "mem:///dst/ln", b"no me borres").await;

    let handle = engine
        .copy_with(
            &vp("mem:///src"),
            &vp("mem:///dst"),
            TransferOptions {
                on_collision: CollisionPolicy::Overwrite,
                symlinks: SymlinkPolicy::Follow,
            },
        )
        .unwrap();
    // M2 fase 1 (#19): el dir-symlink se expande como DIR, y un dir jamás
    // pisa un archivo ni con Overwrite (TypeMismatch, ADR 0005). El
    // invariante que este test pinnea sigue intacto: el destino NO se toca.
    assert_eq!(
        handle.join().await,
        TaskState::Failed {
            error: Error::Conflict {
                conflict: ConflictKind::TypeMismatch
            }
        }
    );
    assert_eq!(
        read_all(&mem, "mem:///dst/ln").await.unwrap(),
        b"no me borres",
        "el fallo era 100% predecible: el destino no se toca"
    );
}

/// Fase 7: el TUI lee vía el core (regla 7) — passthrough con rango.
#[tokio::test]
async fn engine_read_respeta_el_rango() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///f", b"0123456789").await;
    let mut stream = engine
        .read(
            &vp("mem:///f"),
            Some(norte_proto::ByteRange {
                offset: 2,
                len: Some(3),
            }),
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(out, b"234");
}

// ---------- fase 8: papelera (ADR 0009) ----------

/// Trash es UNA operación: el árbol entero desaparece, recuperable.
#[tokio::test]
async fn delete_trash_se_lleva_el_arbol() {
    let (engine, mem) = engine_with_mem();
    build_tree(&mem, 3).await;
    let handle = engine
        .delete_with(&vp("mem:///src"), norte_proto::DeleteMode::Trash)
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        mem.stat(&vp("mem:///src")).await.unwrap_err(),
        Error::NotFound
    );
}

/// Sin capability TRASH el engine JAMÁS degrada: Unsupported y el árbol
/// queda intacto (la degradación es decisión del usuario, ADR 0009).
#[tokio::test]
async fn delete_trash_sin_capability_no_degrada() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::with_flags(
        CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_SENSITIVE,
    ));
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    write_file(&mem, "mem:///valioso", b"datos").await;
    let handle = engine
        .delete_with(&vp("mem:///valioso"), norte_proto::DeleteMode::Trash)
        .unwrap();
    assert_eq!(
        handle.join().await,
        TaskState::Failed {
            error: Error::Unsupported
        }
    );
    assert_eq!(read_all(&mem, "mem:///valioso").await.unwrap(), b"datos");
}

/// Regla 3 para el camino Trash (una sola op): cancelable ANTES de
/// disparar — o gana la cancelación (árbol intacto) o ganó el trash
/// (carrera legítima, como en el move).
#[tokio::test]
async fn delete_trash_cancel_before_start_leaves_tree_intact() {
    let (engine, mem) = engine_with_mem();
    build_tree(&mem, 3).await;
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(30)));
    let handle = engine
        .delete_with(&vp("mem:///src"), norte_proto::DeleteMode::Trash)
        .unwrap();
    handle.cancel();
    let state = handle.join().await;
    mem.faults().clear();
    match state {
        TaskState::Cancelled => {
            assert!(mem.stat(&vp("mem:///src")).await.is_ok(), "árbol intacto");
        }
        TaskState::Completed => {
            assert_eq!(
                mem.stat(&vp("mem:///src")).await.unwrap_err(),
                Error::NotFound,
                "el trash ganó la carrera: fue ENTERO"
            );
        }
        other => panic!("estado inesperado: {other:?}"),
    }
}
