//! Paridad del [`Backend`]: el MISMO guion por el brazo embebido y por el
//! remoto, y los dos transcritos tienen que coincidir línea a línea.
//!
//! Es la promesa del módulo («la MISMA superficie para el core embebido y el
//! daemon», regla 7) hecha comprobable método a método. Los demás tests del
//! `Backend` prueban cada brazo por su lado y casi siempre en copia, listado
//! y búsqueda; los informes de tarea, el journal, el índice o el registro no
//! los recorría nadie desde aquí, así que un brazo podía divergir del otro
//! sin que se viera. También es la red de la ola que reparta `backend.rs` por
//! áreas: el guion no sabe en qué fichero vive cada método.
#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use norte_core::Engine;
use norte_core::backend::remote::RemoteBackend;
use norte_core::backend::{Backend, TaskRef};
use norte_core::daemon::{Daemon, DaemonConfig};
use norte_proto::methods::{self, ClientInfo};
use norte_proto::{Error, Segment, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido de test")
}

fn seg(name: &str) -> Segment {
    Segment::new(name.as_bytes().to_vec()).expect("segmento válido")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write abre");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk entra");
    sink.commit().await.expect("commit publica");
}

/// Engine con journal en memoria y spool: lo que necesitan las mutaciones
/// deshacibles y el sync. Los dos brazos arrancan de uno igual.
async fn engine(spool: &std::path::Path) -> (Arc<Engine>, Arc<MemProvider>) {
    let journal = Arc::new(norte_core::SqliteJournal::new(
        norte_core::Journal::open_in_memory()
            .await
            .expect("journal"),
    ));
    let engine = Engine::with_journal(journal);
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    engine.set_spool(norte_core::sync::Spool::new(spool));
    (Arc::new(engine), mem)
}

/// El terminal de una tarea, con tope: si una tarea no termina, el test
/// falla aquí y no cuelga.
async fn join(task: TaskRef) -> TaskState {
    tokio::time::timeout(Duration::from_secs(10), task.join())
        .await
        .expect("terminal antes del timeout")
}

/// `ok` o la variante del error, sin sus campos: los mensajes pueden llevar
/// ids o rutas que no son parte del contrato.
fn resultado<T>(r: &Result<T, Error>) -> String {
    match r {
        Ok(_) => "ok".to_owned(),
        Err(e) => {
            let d = format!("{e:?}");
            d.split(|c: char| !c.is_alphanumeric())
                .next()
                .unwrap_or("")
                .to_owned()
        }
    }
}

/// El estado terminal de una tarea lanzada, o por qué no se lanzó.
async fn tarea(r: Result<TaskRef, Error>) -> (String, Option<norte_proto::TaskId>) {
    match r {
        Ok(t) => {
            let id = t.id();
            (format!("{:?}", join(t).await), Some(id))
        }
        Err(e) => (resultado::<()>(&Err(e)), None),
    }
}

/// El guion. Cada paso deja una línea; lo que se afirma aquí es lo que un
/// frontend da por hecho, y la comparación entre brazos cubre el resto.
#[expect(clippy::too_many_lines, reason = "un guion: un paso tras otro")]
async fn guion(b: &Backend, mem: &MemProvider) -> Vec<String> {
    let mut t = Vec::new();
    for d in ["mem:///d", "mem:///e", "mem:///partes"] {
        mem.mkdir(&vp(d)).await.expect("mkdir");
    }
    write_file(mem, "mem:///d/a.txt", b"hola").await;
    write_file(mem, "mem:///d/b.txt", b"adios").await;
    write_file(mem, "mem:///e/a.txt", b"hola").await;

    // Sumas de comprobación y su informe.
    let (estado, id) = tarea(
        b.checksum(methods::FsChecksumParams {
            paths: vec![vp("mem:///d/a.txt")],
            algo: methods::ChecksumAlgo::Sha256,
        })
        .await,
    )
    .await;
    assert_eq!(estado, "Completed");
    let informe = b
        .checksum_report(id.expect("id"))
        .await
        .expect("checksum_report");
    assert_eq!(informe.entries.len(), 1);
    assert_eq!(
        informe.entries[0].digest.as_deref(),
        Some("b221d9dbb083a7f33428d7c2a3c3198ae925614d70210e28716ccaa7cd4ddb79"),
        "sha256(\"hola\")"
    );
    t.push(format!("checksum {estado} {informe:?}"));

    // Tamaño y uso de un directorio.
    let (estado, _) = tarea(
        b.dir_size(methods::FsDirSizeParams {
            paths: vec![vp("mem:///d")],
        })
        .await,
    )
    .await;
    t.push(format!("dir_size {estado}"));
    let (estado, id) = tarea(
        b.dir_usage(methods::FsDirUsageParams {
            path: vp("mem:///d"),
            depth: 1,
        })
        .await,
    )
    .await;
    let uso = b
        .dir_usage_report(id.expect("id"))
        .await
        .expect("dir_usage_report");
    assert_eq!(uso.total_bytes, 9, "hola + adios");
    t.push(format!("dir_usage {estado} {uso:?}"));

    // Empaquetar, comprobar lo empaquetado, y sus informes.
    let (estado, id) = tarea(
        b.pack(methods::ArchivePackParams {
            sources: vec![vp("mem:///d/a.txt"), vp("mem:///d/b.txt")],
            dest: vp("mem:///p.zip"),
            format: methods::ArchiveFormat::Zip,
            level: None,
            base: vp("mem:///d"),
        })
        .await,
    )
    .await;
    assert_eq!(estado, "Completed");
    let empaquetado = b
        .archive_pack_report(id.expect("id"))
        .await
        .expect("archive_pack_report");
    assert_eq!(empaquetado.entries, 2);
    t.push(format!("pack {estado} {empaquetado:?}"));
    let (estado, id) = tarea(
        b.test_archive(methods::ArchiveTestParams {
            path: vp("mem:///p.zip"),
        })
        .await,
    )
    .await;
    let prueba = b
        .archive_test_report(id.expect("id"))
        .await
        .expect("archive_test_report");
    assert!(prueba.failed.is_empty(), "{prueba:?}");
    t.push(format!("test_archive {estado} {prueba:?}"));

    // Partir y juntar: tres trozos del mínimo que se admite.
    let trozo = methods::FILE_SPLIT_MIN_BYTES;
    let grande: Vec<u8> = (0..trozo * 2 + 5).map(|i| (i % 251) as u8).collect();
    write_file(mem, "mem:///grande.bin", &grande).await;
    let (estado, _) = tarea(
        b.split_file(methods::FileSplitParams {
            path: vp("mem:///grande.bin"),
            part_bytes: trozo,
            dest_dir: vp("mem:///partes"),
        })
        .await,
    )
    .await;
    assert_eq!(estado, "Completed");
    let mut trozos: Vec<VPath> = b
        .list(&vp("mem:///partes"))
        .await
        .expect("list partes")
        .into_iter()
        .map(|e| e.path)
        .collect();
    trozos.sort_by_key(VPath::display_lossy);
    assert_eq!(trozos.len(), 3);
    t.push(format!(
        "split {estado} {:?}",
        trozos.iter().map(VPath::display_lossy).collect::<Vec<_>>()
    ));
    let (estado, _) = tarea(
        b.combine_files(methods::FileCombineParams {
            first: trozos[0].clone(),
            dest: vp("mem:///junto.bin"),
        })
        .await,
    )
    .await;
    assert_eq!(estado, "Completed");
    let size = b.stat(&vp("mem:///junto.bin")).await.expect("stat").size;
    assert_eq!(size, Some(grande.len() as u64));
    t.push(format!("combine {estado}"));

    // Permisos: el provider en memoria no los tiene, y los dos brazos lo
    // dicen igual.
    let r = b
        .set_mode(methods::FsSetModeParams {
            paths: vec![vp("mem:///d/a.txt")],
            mode: 0o600,
            recursive: false,
            dir_mode: None,
        })
        .await;
    t.push(format!("set_mode {}", tarea(r).await.0));

    // Renombrado por lotes: plan, ejecución con su hash, informe.
    let pares = [methods::RenamePair {
        from: seg("a.txt"),
        to: seg("c.txt"),
    }];
    let plan = b
        .rename_batch_plan(&vp("mem:///d"), &pares)
        .await
        .expect("rename_batch_plan");
    assert!(plan.executable, "{plan:?}");
    t.push(format!(
        "rename_plan {:?} {:?}",
        plan.steps, plan.collisions
    ));
    let (estado, lote) = tarea(
        b.rename_batch(&vp("mem:///d"), &pares, &plan.plan_hash)
            .await,
    )
    .await;
    let lote = lote.expect("id");
    assert_eq!(estado, "Completed");
    let renombrado = b
        .rename_batch_report(lote)
        .await
        .expect("rename_batch_report");
    assert_eq!(renombrado.applied, 1);
    t.push(format!("rename {estado} {renombrado:?}"));

    // El journal lo registró, y deshacer desde ahí lo devuelve.
    let diario = b.journal_list(None, 50, None).await.expect("journal_list");
    let ops: Vec<String> = diario
        .rows
        .iter()
        .map(|r| format!("{} {}", r.actor_kind, r.op))
        .collect();
    assert!(!diario.rows.is_empty());
    t.push(format!("journal {ops:?}"));
    let ultima = diario.rows.iter().map(|r| r.seq).max().expect("una fila");
    let (estado, id) = tarea(b.undo_after(ultima - 1).await).await;
    assert_eq!(estado, "Completed");
    let deshecho = b.undo_report(id.expect("id")).await.expect("undo_report");
    assert_eq!(deshecho.undone, 1, "{deshecho:?}");
    t.push(format!("undo_after {estado} {deshecho:?}"));
    // Un id que existe pero no fue un undo. Divergencia de WIRE, no deseada:
    // el daemon contesta `INVALID_PARAMS` (que el cliente lee como `Internal`)
    // donde su gemelo `fs.rename_batch_report` contesta `NotFound`, que es lo
    // que da el embebido. Igualarlo cambia un código de error del protocolo,
    // así que va con su versión y sus goldens, no aquí. Se fija para que el
    // día que se iguale, este test lo diga.
    let ajeno = resultado(&b.undo_report(lote).await);
    match b {
        Backend::Embedded(_) => assert_eq!(ajeno, "NotFound"),
        Backend::Remote(_) => assert_eq!(ajeno, "Internal"),
    }
    // Divergencia documentada, y a propósito: la sesión que se deshace
    // es de un agente, y los agentes viven en el daemon. Se afirma por brazo
    // y no entra en el transcrito.
    let sesion = tarea(b.undo_session("nadie").await).await.0;
    let esperado = match b {
        Backend::Embedded(_) => "Unsupported",
        Backend::Remote(_) => "Completed",
    };
    assert_eq!(sesion, esperado, "undo_session");

    // Comparar dos carpetas.
    match b
        .compare(methods::FsCompareParams {
            left: vp("mem:///d"),
            right: vp("mem:///e"),
            criteria: methods::CompareCriteria::default(),
            max_depth: None,
            mtime_tolerance_ms: 0,
            follow_symlinks: false,
            descend_orphans: None,
        })
        .await
    {
        Ok((task, mut rx)) => {
            let estado = join(task).await;
            let mut filas = 0;
            while let Ok(Some(lote)) = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await
            {
                filas += lote.rows.len();
            }
            t.push(format!("compare {estado:?} {filas}"));
        }
        Err(e) => t.push(format!("compare {}", resultado::<()>(&Err(e)))),
    }

    // Índice.
    t.push(format!(
        "index_build {}",
        tarea(b.index_build(&vp("mem:///d")).await).await.0
    ));
    let hits = b.index_query(&vp("mem:///d"), "c", 10).await;
    t.push(format!(
        "index_query {} {:?}",
        resultado(&hits),
        hits.map(|h| h.len()).ok()
    ));

    // Lo que no es de ficheros.
    t.push(format!("volumes {}", resultado(&b.volumes(false).await)));
    // Segunda divergencia documentada: no hay método de wire para el GC
    // (cambio de protocolo diferido hasta que haya demanda).
    let gc = b.gc_partials(&vp("mem:///d"), Duration::ZERO).await;
    match b {
        Backend::Embedded(_) => assert_eq!(gc.ok(), Some(0), "nada que barrer"),
        Backend::Remote(_) => assert_eq!(resultado(&gc), "Unsupported"),
    }
    let plugins = b.plugins_list().await;
    t.push(format!("plugins_list {}", resultado(&plugins)));
    t.push(format!(
        "close_connection {:?}",
        b.close_connection(&vp("mem:///")).await.ok()
    ));
    // Tercera, y también a propósito: dice lo que el TIPO puede prometer
    // («hay un daemon detrás»), no si este engine concreto lleva journal —y
    // este lo lleva—. Ver su rustdoc.
    assert_eq!(b.is_journalled(), matches!(b, Backend::Remote(_)));
    b.drop_retained_plans().await;

    t
}

#[tokio::test(flavor = "multi_thread")]
async fn los_dos_brazos_cuentan_lo_mismo() {
    let dir_e = tempfile::tempdir().expect("tempdir");
    let (e, mem_e) = engine(dir_e.path()).await;
    let embebido = guion(&Backend::Embedded(e), &mem_e).await;

    let dir_r = tempfile::tempdir().expect("tempdir");
    let socket = dir_r.path().join("d.sock");
    let (e, mem_r) = engine(dir_r.path()).await;
    let daemon = Daemon::bind(
        e,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: Some(dir_r.path().to_path_buf()),
            state_dir: None,
        },
    )
    .await
    .expect("bind");
    let _run = tokio::spawn(daemon.run());
    let r = RemoteBackend::connect(
        socket,
        None,
        ClientInfo {
            name: "paridad".into(),
            version: "0.0.0".into(),
        },
    )
    .await
    .expect("connect");
    let remoto = guion(&Backend::Remote(r), &mem_r).await;

    for (i, (a, b)) in embebido.iter().zip(&remoto).enumerate() {
        assert_eq!(a, b, "paso {i}: embebido ≠ remoto");
    }
    assert_eq!(embebido.len(), remoto.len());
}
