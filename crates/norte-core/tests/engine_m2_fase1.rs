//! Matriz test-first de la fase 1 de M2 — deuda dura del engine:
//! #16 identidad real de nodo en los guards, #17 retry de mutaciones con
//! desambiguación post-efecto, #18 kind real al preservar symlinks,
//! #19 Follow sobre dir-symlinks con visited set.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use norte_core::{Engine, Mutation, MutationObserver, TransferOptions};
use norte_proto::{
    ByteRange, Capabilities, CapabilityFlags, CollisionPolicy, DeleteMode, Entry, Error,
    SymlinkPolicy, TaskState, VPath,
};
use norte_testkit::MemProvider;
use norte_vfs::{ByteSink, ByteStream, EntryStream, FollowLinks, NodeId, Provider, SymlinkKind};

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

async fn read_all(p: &dyn Provider, wire: &str) -> Result<Vec<u8>, Error> {
    let mut stream = p.read(&vp(wire), None).await?;
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk?);
    }
    Ok(out)
}

fn engine_with_mem() -> (Engine, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem)
}

fn on_collision(c: CollisionPolicy) -> TransferOptions {
    TransferOptions {
        on_collision: c,
        ..TransferOptions::default()
    }
}

fn follow() -> TransferOptions {
    TransferOptions {
        symlinks: SymlinkPolicy::Follow,
        ..TransferOptions::default()
    }
}

/// Observador que registra cada mutación (como hará el journal en M3):
/// verifica que el retry con desambiguación emite EXACTAMENTE una vez.
#[derive(Default)]
struct RecordingObserver {
    events: Mutex<Vec<String>>,
}

impl RecordingObserver {
    fn events(&self) -> Vec<String> {
        self.events.lock().expect("events lock sano").clone()
    }
}

#[async_trait::async_trait]
impl MutationObserver for RecordingObserver {
    async fn on_mutation(
        &self,
        mutation: &Mutation<'_>,
        _actor: &norte_core::journal::Actor,
    ) -> Result<(), norte_proto::Error> {
        let repr = match mutation {
            Mutation::Created(p) => format!("created:{}", p.display_lossy()),
            Mutation::Removed(p) => format!("removed:{}", p.display_lossy()),
            Mutation::Trashed { path, .. } => format!("trashed:{}", path.display_lossy()),
            Mutation::Renamed { from, to, .. } => {
                format!("renamed:{}>{}", from.display_lossy(), to.display_lossy())
            }
            // #314: con el modo ANTERIOR dentro, que es lo que la reversa
            // necesita y lo único que un observador no puede reconstruir.
            Mutation::ModeChanged { path, from, to } => format!(
                "mode:{}:{}>{to:o}",
                path.display_lossy(),
                from.map_or_else(|| "?".to_owned(), |m| format!("{m:o}")),
            ),
        };
        self.events.lock().expect("events lock sano").push(repr);
        Ok(())
    }
}

fn engine_recording(mem: &Arc<MemProvider>) -> (Engine, Arc<RecordingObserver>) {
    let observer = Arc::new(RecordingObserver::default());
    let engine = Engine::with_observer(Arc::clone(&observer) as Arc<dyn MutationObserver>);
    engine.register_provider(Arc::clone(mem) as Arc<dyn Provider>);
    (engine, observer)
}

/// Provider que DELEGA todo en un Mem pero con scheme y capabilities
/// propios. Dos usos: (a) anunciar caja insensible sobre un Mem sensible —
/// el caso WSL/NTFS donde la heurística de caja MIENTE y solo la identidad
/// real (#16) responde bien; (b) scheme distinto para forzar el camino
/// cross-provider (como el `Alias` de engine.rs).
struct CapsMask {
    inner: Arc<MemProvider>,
    scheme: &'static str,
    flags: CapabilityFlags,
}

#[async_trait]
impl Provider for CapsMask {
    fn scheme(&self) -> &str {
        self.scheme
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            flags: self.flags,
            max_path: None,
        }
    }
    async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        self.inner.stat(p).await
    }
    async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
        self.inner.list(p).await
    }
    async fn read(&self, p: &VPath, range: Option<ByteRange>) -> Result<ByteStream, Error> {
        self.inner.read(p, range).await
    }
    async fn node_id(&self, p: &VPath, follow: FollowLinks) -> Result<Option<NodeId>, Error> {
        self.inner.node_id(p, follow).await
    }
    async fn read_link(&self, p: &VPath) -> Result<Vec<u8>, Error> {
        self.inner.read_link(p).await
    }
    async fn symlink(&self, link: &VPath, target: &[u8], kind: SymlinkKind) -> Result<(), Error> {
        self.inner.symlink(link, target, kind).await
    }
    async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, Error> {
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

// ---------- #16: identidad real en el guard anti-autodestrucción ----------

/// El FS anuncia caja insensible pero DISTINGUE estos dos nombres (WSL
/// sobre NTFS case-sensitive, folds exóticos): la identidad real dice que
/// son nodos DISTINTOS y la copia con Overwrite debe PROCEDER — la
/// heurística de `to_lowercase` los bloqueaba como falso positivo.
#[tokio::test]
async fn overwrite_entre_variantes_de_caja_que_el_fs_distingue_procede() {
    let engine = Engine::new();
    let inner = Arc::new(MemProvider::new()); // case-SENSITIVE de verdad
    write_file(&inner, "mem:///CASA", b"mayus").await;
    write_file(&inner, "mem:///casa", b"minus").await;
    let masked = Arc::new(CapsMask {
        inner: Arc::clone(&inner),
        scheme: "mem",
        // Anuncia insensible (sin CASE_SENSITIVE): la heurística sospecharía.
        flags: CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING,
    });
    engine.register_provider(masked as Arc<dyn Provider>);

    let handle = engine
        .copy_with(
            &vp("mem:///CASA"),
            &vp("mem:///casa"),
            on_collision(CollisionPolicy::Overwrite),
        )
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&*inner, "mem:///casa").await.unwrap(), b"mayus");
    assert_eq!(read_all(&*inner, "mem:///CASA").await.unwrap(), b"mayus");
}

/// Sin identidad (provider `without_node_ids`), la heurística conservadora
/// de M1 sigue vigente: la variante de caja sobre sí mismo se rechaza.
#[tokio::test]
async fn sin_identidad_la_heuristica_conservadora_sigue_bloqueando() {
    let engine = Engine::new();
    let mem = Arc::new(
        MemProvider::with_flags(CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING)
            .without_node_ids(),
    );
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    write_file(&mem, "mem:///unico", b"precioso").await;

    let handle = engine
        .copy_with(
            &vp("mem:///UNICO"),
            &vp("mem:///unico"),
            on_collision(CollisionPolicy::Overwrite),
        )
        .await
        .unwrap();
    assert!(matches!(handle.join().await, TaskState::Failed { .. }));
    assert_eq!(read_all(&*mem, "mem:///unico").await.unwrap(), b"precioso");
}

// ---------- #17: retry de mutaciones con desambiguación ----------

/// El fallo transitorio PRE-efecto en una mutación ahora se reintenta (M1
/// solo reintentaba lecturas): un delete con el provider parpadeando
/// termina bien.
#[tokio::test]
async fn mutacion_con_fallo_transitorio_pre_efecto_se_reintenta() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///victima", b"x").await;
    mem.faults().unavailable_for_next(1);

    let handle = engine
        .delete_with(&vp("mem:///victima"), DeleteMode::Permanent)
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        mem.stat(&vp("mem:///victima")).await.unwrap_err(),
        Error::NotFound
    );
}

/// Timeout POST-efecto en remove: el reintento ve `NotFound` y lo lee como
/// "el efecto se aplicó" — éxito, y el journal recibe UN solo Removed.
#[tokio::test]
async fn remove_ambiguo_se_desambigua_como_exito() {
    let mem = Arc::new(MemProvider::new());
    write_file(&mem, "mem:///victima", b"x").await;
    let (engine, observer) = engine_recording(&mem);
    mem.faults().ambiguous_mutations(1);

    let handle = engine
        .delete_with(&vp("mem:///victima"), DeleteMode::Permanent)
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        mem.stat(&vp("mem:///victima")).await.unwrap_err(),
        Error::NotFound
    );
    let removed: Vec<_> = observer
        .events()
        .into_iter()
        .filter(|e| e.starts_with("removed:"))
        .collect();
    assert_eq!(removed.len(), 1, "UN Removed, jamás cero ni dos");
}

/// Timeout POST-efecto en el mkdir del árbol destino, con política que
/// permite merge: el Conflict del reintento se absorbe como merge y la
/// copia termina.
#[tokio::test]
async fn mkdir_ambiguo_bajo_merge_completa_la_copia() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    write_file(&mem, "mem:///src/f", b"data").await;
    mem.faults().ambiguous_mutations(1);

    let handle = engine
        .copy_with(
            &vp("mem:///src"),
            &vp("mem:///dst"),
            on_collision(CollisionPolicy::Overwrite),
        )
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&*mem, "mem:///dst/f").await.unwrap(), b"data");
}

/// Mismo timeout post-efecto bajo `Fail`: con el pre-stat de #32.2 el
/// engine YA no adivina — SABE que el destino no preexistía, así que el
/// Conflict del retry es nuestra primera aplicación: la copia COMPLETA
/// (antes: Conflict fail-safe con el dir bien creado y la task fallida).
#[tokio::test]
async fn mkdir_ambiguo_bajo_fail_completa() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    write_file(&mem, "mem:///src/f", b"data").await;
    mem.faults().ambiguous_mutations(1);

    let handle = engine
        .copy(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&*mem, "mem:///dst/f").await.unwrap(), b"data");
}

/// Timeout POST-efecto al preservar un symlink: el reintento ve `Conflict`,
/// verifica por `read_link` que el link es EL NUESTRO (mismo target) y lo da
/// por creado. UN Created para el journal.
#[tokio::test]
async fn symlink_ambiguo_preserve_se_desambigua() {
    let mem = Arc::new(MemProvider::new());
    write_file(&mem, "mem:///target", b"x").await;
    mem.symlink(&vp("mem:///ln"), b"target", SymlinkKind::File)
        .await
        .unwrap();
    let (engine, observer) = engine_recording(&mem);
    mem.faults().ambiguous_mutations(1);

    let handle = engine
        .copy(&vp("mem:///ln"), &vp("mem:///ln2"))
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(mem.read_link(&vp("mem:///ln2")).await.unwrap(), b"target");
    let created: Vec<_> = observer
        .events()
        .into_iter()
        .filter(|e| e.starts_with("created:"))
        .collect();
    assert_eq!(created.len(), 1, "UN Created para el journal");
}

/// Timeout POST-efecto en rename (move same-provider): el reintento ve
/// `NotFound` en el origen, verifica por `node_id` que el destino ES el nodo
/// original y lo da por renombrado. UN Renamed para el journal.
#[tokio::test]
async fn rename_ambiguo_se_desambigua_como_exito() {
    let mem = Arc::new(MemProvider::new());
    write_file(&mem, "mem:///antes", b"contenido").await;
    let (engine, observer) = engine_recording(&mem);
    mem.faults().ambiguous_mutations(1);

    let handle = engine
        .move_(&vp("mem:///antes"), &vp("mem:///despues"))
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        read_all(&*mem, "mem:///despues").await.unwrap(),
        b"contenido"
    );
    assert_eq!(
        mem.stat(&vp("mem:///antes")).await.unwrap_err(),
        Error::NotFound
    );
    let renamed: Vec<_> = observer
        .events()
        .into_iter()
        .filter(|e| e.starts_with("renamed:"))
        .collect();
    assert_eq!(renamed.len(), 1, "UN Renamed para el journal");
}

/// Sin identidad de nodo NO se puede verificar un rename ambiguo: el
/// engine no adivina — surge el error transitorio original (fail-safe;
/// el efecto pudo aplicarse y el usuario reintenta contra el estado real).
#[tokio::test]
async fn rename_ambiguo_sin_identidad_no_adivina() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new().without_node_ids());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    write_file(&mem, "mem:///antes", b"contenido").await;
    mem.faults().ambiguous_mutations(1);

    let handle = engine
        .move_(&vp("mem:///antes"), &vp("mem:///despues"))
        .await
        .unwrap();
    match handle.join().await {
        TaskState::Failed {
            error: Error::ProviderUnavailable { .. },
        } => {}
        other => panic!("esperaba el transitorio original, fue {other:?}"),
    }
    // Sin pérdida: el contenido está EN ALGÚN LADO (aquí: ya renombrado).
    assert_eq!(
        read_all(&*mem, "mem:///despues").await.unwrap(),
        b"contenido"
    );
}

// ---------- #18: kind real al preservar symlinks ----------

/// Preserve de un dir-symlink: el kind que llega al provider destino ya no
/// es `File` hardcodeado — el destino lo resuelve (Unknown) y el link
/// queda como DIR-symlink. El target ("asub") se copia ANTES que el link
/// ("zln") por orden de walk: la resolución lo encuentra.
#[tokio::test]
async fn preserve_resuelve_el_kind_del_dir_symlink() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    mem.mkdir(&vp("mem:///src/asub")).await.unwrap();
    mem.symlink(&vp("mem:///src/zln"), b"asub", SymlinkKind::Dir)
        .await
        .unwrap();

    let handle = engine
        .copy(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        mem.symlink_kind_of(&vp("mem:///dst/zln")),
        Some(SymlinkKind::Dir),
        "el kind se resolvió contra el target real, no File a ciegas"
    );
}

// ---------- #19: Follow sobre dir-symlinks ----------

/// Follow expande un dir-symlink como directorio REAL en el destino, con
/// su contenido copiado (semántica `cp -RL`). En M1 esto era Unsupported.
#[tokio::test]
async fn follow_expande_dir_symlink_como_directorio() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    mem.mkdir(&vp("mem:///src/at")).await.unwrap();
    write_file(&mem, "mem:///src/at/f", b"data").await;
    mem.symlink(&vp("mem:///src/zln"), b"at", SymlinkKind::Dir)
        .await
        .unwrap();

    let handle = engine
        .copy_with(&vp("mem:///src"), &vp("mem:///dst"), follow())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&*mem, "mem:///dst/at/f").await.unwrap(), b"data");
    assert_eq!(read_all(&*mem, "mem:///dst/zln/f").await.unwrap(), b"data");
    let e = mem.stat(&vp("mem:///dst/zln")).await.unwrap();
    assert_eq!(
        e.kind,
        norte_proto::EntryKind::Dir,
        "el link expandido es un dir REAL"
    );
}

/// Copiar el dir-symlink RAÍZ con Follow: el destino es el árbol del
/// target, como dir real.
#[tokio::test]
async fn follow_de_un_dir_symlink_raiz_copia_el_arbol() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///at")).await.unwrap();
    write_file(&mem, "mem:///at/f", b"data").await;
    mem.symlink(&vp("mem:///zln"), b"at", SymlinkKind::Dir)
        .await
        .unwrap();

    let handle = engine
        .copy_with(&vp("mem:///zln"), &vp("mem:///dst"), follow())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&*mem, "mem:///dst/f").await.unwrap(), b"data");
}

/// Un ciclo de symlinks bajo Follow falla LIMPIO (visited set por `node_id`,
/// spec §17.9): jamás recursión infinita ni cuelgue.
#[tokio::test]
async fn follow_ciclo_de_symlinks_falla_limpio() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    mem.mkdir(&vp("mem:///src/d")).await.unwrap();
    // Target vacío: resuelve al PADRE del link (src/d) — ciclo d → d.
    mem.symlink(&vp("mem:///src/d/loop"), b"", SymlinkKind::Dir)
        .await
        .unwrap();

    let handle = engine
        .copy_with(&vp("mem:///src"), &vp("mem:///dst"), follow())
        .await
        .unwrap();
    assert_eq!(
        handle.join().await,
        TaskState::Failed { error: Error::Loop },
        "ciclo detectado y rechazado con su categoría propia (#31)"
    );
}

/// Un DAG (dos caminos al mismo dir SIN ciclo) NO es un ciclo: se copia
/// dos veces, como `cp -RL`.
#[tokio::test]
async fn follow_dag_sin_ciclo_copia_dos_veces() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    mem.mkdir(&vp("mem:///src/at")).await.unwrap();
    write_file(&mem, "mem:///src/at/f", b"data").await;
    mem.symlink(&vp("mem:///src/y1"), b"at", SymlinkKind::Dir)
        .await
        .unwrap();
    mem.symlink(&vp("mem:///src/y2"), b"at", SymlinkKind::Dir)
        .await
        .unwrap();

    let handle = engine
        .copy_with(&vp("mem:///src"), &vp("mem:///dst"), follow())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&*mem, "mem:///dst/y1/f").await.unwrap(), b"data");
    assert_eq!(read_all(&*mem, "mem:///dst/y2/f").await.unwrap(), b"data");
}

/// Follow sobre dir-symlink en un provider SIN identidad de nodo: sin
/// visited set posible → Unsupported (el comportamiento M1 se preserva
/// exactamente donde no hay red de seguridad).
#[tokio::test]
async fn follow_sin_identidad_es_unsupported() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new().without_node_ids());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    mem.mkdir(&vp("mem:///at")).await.unwrap();
    mem.symlink(&vp("mem:///zln"), b"at", SymlinkKind::Dir)
        .await
        .unwrap();

    let handle = engine
        .copy_with(&vp("mem:///zln"), &vp("mem:///dst"), follow())
        .await
        .unwrap();
    assert_eq!(
        handle.join().await,
        TaskState::Failed {
            error: Error::Unsupported
        }
    );
}

/// Move con Follow: el delete de la fase 2 borra el LINK, jamás el
/// contenido del target A TRAVÉS del link (sería pérdida fuera del árbol
/// movido). El origen queda completamente vacío; el destino, expandido.
#[tokio::test]
async fn move_follow_no_borra_a_traves_del_link() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    mem.mkdir(&vp("mem:///src/at")).await.unwrap();
    write_file(&mem, "mem:///src/at/f", b"data").await;
    mem.symlink(&vp("mem:///src/zln"), b"at", SymlinkKind::Dir)
        .await
        .unwrap();

    let handle = engine
        .move_with(&vp("mem:///src"), &vp("mem:///dst2"), follow())
        .await
        .unwrap();
    // Mismo provider: el rename gana y no hay expansión — forzar el camino
    // copy+delete con un destino CROSS-provider sería el caso puro; aquí
    // basta verificar que el move terminó sin tocar nada de más.
    assert_eq!(handle.join().await, TaskState::Completed);
    assert!(mem.stat(&vp("mem:///src")).await.is_err(), "origen movido");
}

/// El caso PURO de move+Follow (copy+delete cross-provider): el link se
/// expande en el destino; en el ORIGEN el link se borra COMO LINK — su
/// contenido jamás se recorre para borrar (sería pérdida a través del
/// link) ni queda nada atrás.
#[tokio::test]
async fn move_follow_cross_provider_expande_y_borra_solo_el_link() {
    let engine = Engine::new();
    let src_tree = Arc::new(MemProvider::new());
    let dst_tree = Arc::new(MemProvider::new());
    engine.register_provider(Arc::new(CapsMask {
        inner: Arc::clone(&src_tree),
        scheme: "src",
        flags: MemProvider::new().capabilities().flags,
    }) as Arc<dyn Provider>);
    engine.register_provider(Arc::clone(&dst_tree) as Arc<dyn Provider>);

    src_tree.mkdir(&vp("src:///m")).await.unwrap();
    src_tree.mkdir(&vp("src:///m/at")).await.unwrap();
    write_file(&src_tree, "src:///m/at/f", b"data").await;
    src_tree
        .symlink(&vp("src:///m/zln"), b"at", SymlinkKind::Dir)
        .await
        .unwrap();

    let handle = engine
        .move_with(&vp("src:///m"), &vp("mem:///dst"), follow())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    // Destino: expandido de verdad (at real + zln expandido como dir real).
    assert_eq!(
        read_all(&*dst_tree, "mem:///dst/at/f").await.unwrap(),
        b"data"
    );
    assert_eq!(
        read_all(&*dst_tree, "mem:///dst/zln/f").await.unwrap(),
        b"data"
    );
    // Origen: TODO fuera (at, su contenido y el link — borrado como link).
    assert_eq!(
        src_tree.stat(&vp("src:///m")).await.unwrap_err(),
        Error::NotFound
    );
}

/// Cancelación durante una copia con Follow: limpia, sin cuelgue (regla 3
/// aplica también al walk con expansión).
#[tokio::test]
async fn follow_cancelacion_durante_walk_es_limpia() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    mem.mkdir(&vp("mem:///src/at")).await.unwrap();
    for i in 0..50 {
        write_file(&mem, &format!("mem:///src/at/f{i}"), b"data").await;
    }
    mem.symlink(&vp("mem:///src/zln"), b"at", SymlinkKind::Dir)
        .await
        .unwrap();
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(5)));

    let handle = engine
        .copy_with(&vp("mem:///src"), &vp("mem:///dst"), follow())
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    handle.cancel();
    assert_eq!(handle.join().await, TaskState::Cancelled);
}

/// Hallazgo ALTA del encoding-auditor: copiar un symlink SOBRE su propio
/// target con Follow+Overwrite destruiría el target (remove antes de leer
/// a través del link). El guard compara la identidad RESUELTA del origen.
#[tokio::test]
async fn follow_overwrite_de_link_sobre_su_target_no_destruye() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///f", b"precioso").await;
    mem.symlink(&vp("mem:///ln"), b"f", SymlinkKind::File)
        .await
        .unwrap();

    let handle = engine
        .copy_with(
            &vp("mem:///ln"),
            &vp("mem:///f"),
            TransferOptions {
                on_collision: CollisionPolicy::Overwrite,
                symlinks: SymlinkPolicy::Follow,
                ..TransferOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        handle.join().await,
        TaskState::Failed {
            error: Error::InvalidPath
        }
    );
    assert_eq!(read_all(&*mem, "mem:///f").await.unwrap(), b"precioso");
}

/// Variante dir del mismo hallazgo: expandir un dir-symlink sobre su
/// propio target dir.
#[tokio::test]
async fn follow_overwrite_de_dir_link_sobre_su_target_no_destruye() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///d")).await.unwrap();
    write_file(&mem, "mem:///d/hijo", b"precioso").await;
    mem.symlink(&vp("mem:///ln"), b"d", SymlinkKind::Dir)
        .await
        .unwrap();

    let handle = engine
        .copy_with(
            &vp("mem:///ln"),
            &vp("mem:///d"),
            TransferOptions {
                on_collision: CollisionPolicy::Overwrite,
                symlinks: SymlinkPolicy::Follow,
                ..TransferOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        handle.join().await,
        TaskState::Failed {
            error: Error::InvalidPath
        }
    );
    assert_eq!(read_all(&*mem, "mem:///d/hijo").await.unwrap(), b"precioso");
}

/// La expansión Follow no pierde BYTES: nombre de link no-UTF8 y
/// contenido con nombres hostiles (NFD, control) llegan byte-exactos al
/// destino bajo el path del link expandido.
#[tokio::test]
async fn follow_expande_con_nombres_hostiles_byte_exactos() {
    let (engine, mem) = engine_with_mem();
    let seg = |b: &[u8]| norte_proto::Segment::new(b.to_vec()).expect("segmento hostil válido");
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    mem.mkdir(&vp("mem:///src/at")).await.unwrap();
    // Hijos hostiles: NFD (e + combinante) y salto de línea.
    let nfd: &[u8] = b"e\xCC\x81.txt";
    let ctrl: &[u8] = b"a\nb";
    for name in [nfd, ctrl] {
        let p = vp("mem:///src/at").join(seg(name));
        let mut sink = mem.write(&p).await.expect("write hostil");
        sink.write(Bytes::from_static(b"data")).await.unwrap();
        sink.commit().await.unwrap();
    }
    // Link con nombre no-UTF8 (latin1 é crudo).
    let link_name: &[u8] = b"z\xE9ln";
    let link = vp("mem:///src").join(seg(link_name));
    mem.symlink(&link, b"at", SymlinkKind::Dir).await.unwrap();

    let handle = engine
        .copy_with(&vp("mem:///src"), &vp("mem:///dst"), follow())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    for name in [nfd, ctrl] {
        let expanded = vp("mem:///dst").join(seg(link_name)).join(seg(name));
        let e = mem.stat(&expanded).await.unwrap_or_else(|err| {
            panic!("falta {name:?} bajo el link expandido: {err:?}");
        });
        assert_eq!(
            e.path.file_name().unwrap().as_bytes(),
            name,
            "bytes intactos bajo la expansión"
        );
    }
}

/// Cancelar DURANTE el backoff de un retry de mutación responde rápido
/// (regla 3): jamás espera a agotar los reintentos.
#[tokio::test]
async fn cancelacion_durante_backoff_de_mutacion_es_rapida() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///victima", b"x").await;
    // Transitorios de sobra: sin cancelación tardaría 100+200+400 ms.
    mem.faults().unavailable_for_next(10);

    let inicio = std::time::Instant::now();
    let handle = engine
        .delete_with(&vp("mem:///victima"), DeleteMode::Permanent)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    handle.cancel();
    assert_eq!(handle.join().await, TaskState::Cancelled);
    assert!(
        inicio.elapsed() < std::time::Duration::from_millis(500),
        "la cancelación no espera al backoff"
    );
}
