//! Tests específicos del provider local que el contrato genérico no cubre:
//! symlinks (nunca seguidos), chunking multi-chunk, limpieza del partial,
//! sondeo de capabilities.

use bytes::Bytes;
use futures::StreamExt;
// EntryKind solo lo usan los tests de symlinks, que son cfg(unix).
#[cfg(unix)]
use norte_proto::EntryKind;
use norte_proto::{CapabilityFlags, Segment, VPath};
use norte_vfs::Provider;
use norte_vfs_local::LocalProvider;

fn provider() -> (LocalProvider, VPath, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_path_buf();
    let p = LocalProvider::rooted(base.clone()).with_guard(Box::new(dir));
    (p, LocalProvider::root(), base)
}

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(Segment::new(name.to_vec()).expect("segmento válido"))
}

/// Cuenta ficheros de staging en `base` (nombre `.norte-partial.<hash>.…`:
/// corto y único, jamás derivado del nombre final — issue #4).
fn partials_with_prefix(base: &std::path::Path, prefix: &str) -> usize {
    std::fs::read_dir(base)
        .expect("read_dir")
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .count()
}

#[tokio::test]
async fn capabilities_are_probed() {
    let (p, root, _) = provider();
    // El sondeo es lazy: corre con la primera operación async (regla 2).
    let _ = p.stat(&root).await;
    let caps = p.capabilities();
    assert!(caps.flags.contains(CapabilityFlags::RENAME_ATOMIC));
    assert!(caps.flags.contains(CapabilityFlags::CASE_PRESERVING));
    // El sondeo decide CASE_SENSITIVE según el FS real del tempdir: solo
    // exigimos coherencia con el default del OS en CI (linux=sí, macos=no).
    if cfg!(target_os = "linux") {
        assert!(caps.flags.contains(CapabilityFlags::CASE_SENSITIVE));
    }
    if cfg!(windows) {
        assert_eq!(caps.max_path, Some(32767));
    }
}

/// Issue #5: un archivo AJENO que coincida con el nombre de sonda no puede
/// mentirle al sondeo de caja (en M0 `.norte-probe-cs-a` residual volvía
/// "insensitive" un ext4). La sonda usa sufijo único e identidad (dev,ino).
#[cfg(target_os = "linux")]
#[tokio::test]
async fn probe_not_fooled_by_leftover_lowercase_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".norte-probe-cs-a"), b"del usuario").unwrap();
    let p = LocalProvider::rooted(dir.path().to_path_buf());
    // Primera operación async: dispara el sondeo real.
    let _ = p.stat(&LocalProvider::root()).await;
    assert!(
        p.capabilities()
            .flags
            .contains(CapabilityFlags::CASE_SENSITIVE),
        "archivo ajeno homónimo no puede volver 'insensitive' un ext4"
    );
    assert_eq!(
        std::fs::read(dir.path().join(".norte-probe-cs-a")).unwrap(),
        b"del usuario",
        "el archivo del usuario queda intacto"
    );
}

/// Issue #5: la construcción NO muta el directorio base — el sondeo es lazy
/// (ocurre, como mucho, en la primera `capabilities()`).
#[cfg(unix)]
#[tokio::test]
async fn construction_does_not_touch_base_dir() {
    let dir = tempfile::tempdir().unwrap();
    let before = std::fs::metadata(dir.path()).unwrap().modified().unwrap();
    let _p = LocalProvider::rooted(dir.path().to_path_buf());
    let after = std::fs::metadata(dir.path()).unwrap().modified().unwrap();
    assert_eq!(before, after, "construir no crea ni borra sondas");
}

#[tokio::test]
async fn read_streams_multiple_chunks() {
    let (p, root, _) = provider();
    let f = child(&root, b"grande.bin");
    // 600 KiB > 2 chunks de 256 KiB.
    let content: Vec<u8> = (0..600_usize * 1024)
        .map(|i| u8::try_from(i % 251).expect("i % 251 < 256"))
        .collect();
    let mut sink = p.write(&f).await.unwrap();
    sink.write(Bytes::from(content.clone())).await.unwrap();
    sink.commit().await.unwrap();

    let mut stream = p.read(&f, None).await.unwrap();
    let mut chunks = 0usize;
    let mut got = Vec::new();
    while let Some(item) = stream.next().await {
        got.extend_from_slice(&item.expect("chunk ok"));
        chunks += 1;
    }
    assert!(
        chunks >= 3,
        "600 KiB deben llegar en ≥3 chunks, fueron {chunks}"
    );
    assert_eq!(got, content, "bytes idénticos");
}

#[tokio::test]
async fn partial_file_cleaned_on_abort() {
    let (p, root, base) = provider();
    let f = child(&root, b"obra");
    let mut sink = p.write(&f).await.unwrap();
    sink.write(Bytes::from_static(b"a medias")).await.unwrap();
    assert_eq!(
        partials_with_prefix(&base, ".norte-partial"),
        1,
        "el staging existe durante la escritura"
    );
    sink.abort().await.unwrap();
    assert_eq!(
        partials_with_prefix(&base, ".norte-partial"),
        0,
        "abort no deja rastro"
    );
    assert!(!base.join("obra").exists());
}

#[tokio::test]
async fn partial_file_cleaned_on_drop() {
    let (p, root, base) = provider();
    let f = child(&root, b"tirada");
    {
        let mut sink = p.write(&f).await.unwrap();
        sink.write(Bytes::from_static(b"x")).await.unwrap();
        assert_eq!(partials_with_prefix(&base, ".norte-partial"), 1);
        // Soltar sin commit: abort best-effort en Drop.
    }
    assert_eq!(
        partials_with_prefix(&base, ".norte-partial"),
        0,
        "Drop limpia el staging"
    );
}

#[tokio::test]
async fn commit_renames_partial_to_final() {
    let (p, root, base) = provider();
    let f = child(&root, b"final");
    let mut sink = p.write(&f).await.unwrap();
    sink.write(Bytes::from_static(b"contenido")).await.unwrap();
    sink.commit().await.unwrap();
    assert_eq!(
        partials_with_prefix(&base, ".norte-partial"),
        0,
        "sin staging tras commit"
    );
    assert_eq!(std::fs::read(base.join("final")).unwrap(), b"contenido");
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_stat_never_follows() {
    let (p, root, base) = provider();
    std::fs::write(base.join("destino"), b"real").unwrap();
    std::os::unix::fs::symlink(base.join("destino"), base.join("enlace")).unwrap();

    let e = p.stat(&child(&root, b"enlace")).await.unwrap();
    assert_eq!(
        e.kind,
        EntryKind::Symlink,
        "describe el LINK, no el destino"
    );

    // En list también.
    let kinds: Vec<(Vec<u8>, EntryKind)> = p
        .list(&root)
        .await
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (e.path.file_name().unwrap().as_bytes().to_vec(), e.kind)
        })
        .collect()
        .await;
    let enlace = kinds.iter().find(|(n, _)| n == b"enlace").expect("listado");
    assert_eq!(enlace.1, EntryKind::Symlink);
}

#[cfg(unix)]
#[tokio::test]
async fn remove_symlink_not_target() {
    let (p, root, base) = provider();
    std::fs::write(base.join("destino"), b"real").unwrap();
    std::os::unix::fs::symlink(base.join("destino"), base.join("enlace")).unwrap();
    p.remove(&child(&root, b"enlace")).await.unwrap();
    assert!(!base.join("enlace").exists(), "el link se fue");
    assert!(base.join("destino").exists(), "el destino queda intacto");
}

/// Nombre en el límite de `NAME_MAX` (fixture `name_max_255` del corpus:
/// 255 bytes, legal en ext4/APFS/NTFS): el staging no puede derivar del
/// nombre final o revienta con ENAMETOOLONG opaco (issue #4). Verificación
/// vía provider (el path nativo sin verbatim superaría `MAX_PATH` en Windows).
#[tokio::test]
async fn write_commits_names_at_name_max() {
    let (p, root, _) = provider();
    let name = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "name_max_255")
        .expect("fixture en el corpus");
    let f = child(&root, &name.bytes);
    let mut sink = p.write(&f).await.expect("write abre pese a NAME_MAX");
    sink.write(Bytes::from_static(b"cabe")).await.unwrap();
    sink.commit().await.expect("commit publica");
    let mut stream = p.read(&f, None).await.expect("read abre");
    let mut got = Vec::new();
    while let Some(chunk) = stream.next().await {
        got.extend_from_slice(&chunk.expect("chunk ok"));
    }
    assert_eq!(got, b"cabe");
}

#[tokio::test]
async fn mtime_is_recent_and_positive() {
    let (p, root, _) = provider();
    let f = child(&root, b"con-fecha");
    let mut sink = p.write(&f).await.unwrap();
    sink.write(Bytes::from_static(b"x")).await.unwrap();
    sink.commit().await.unwrap();
    let e = p.stat(&f).await.unwrap();
    let mtime = e.mtime_ms.expect("el FS local siempre tiene mtime");
    // Posterior a 2020-01-01 y anterior a 2100: sanity, no exactitud.
    assert!(mtime > 1_577_836_800_000, "mtime sospechoso: {mtime}");
    assert!(mtime < 4_102_444_800_000, "mtime sospechoso: {mtime}");
}

/// #52: el listado no statea (kind por `d_type`, size/mtime None); `stat()`
/// sigue trayendo los metadatos completos on-demand.
#[tokio::test]
async fn list_es_lazy_y_stat_hidrata() {
    let (p, root, _) = provider();
    let f = child(&root, b"cinco");
    let mut sink = p.write(&f).await.unwrap();
    sink.write(Bytes::from_static(b"12345")).await.unwrap();
    sink.commit().await.unwrap();

    let entries: Vec<norte_proto::Entry> = p
        .list(&root)
        .await
        .unwrap()
        .map(Result::unwrap)
        .collect()
        .await;
    let e = entries
        .iter()
        .find(|e| e.path.file_name().unwrap().as_bytes() == b"cinco")
        .expect("listado");
    assert_eq!(e.kind, norte_proto::EntryKind::File);
    assert!(
        e.size.is_none() && e.mtime_ms.is_none(),
        "listado lazy (#52)"
    );

    let st = p.stat(&f).await.expect("stat");
    assert_eq!(st.size, Some(5));
    assert!(st.mtime_ms.is_some());
}

/// Encoding B2: round-trip corpus hostil list→stat. La hidratación lazy
/// (#52) statea con los BYTES que devolvió el `list`, no con los que se
/// pidieron al crear el archivo — en un FS que normaliza (APFS/NFD) esos
/// dos difieren y un stat con los bytes "originales" podría fallar o, peor,
/// acertar por casualidad sin probar nada. Por cada nombre del corpus: si el
/// OS lo acepta, listar el dir y statear TODOS los paths que devolvió,
/// esperando `size == Some(1)`.
#[tokio::test]
async fn list_lazy_stat_hidrata_nombres_del_corpus() {
    for name in norte_testkit::corpus::hostile_names() {
        let (p, root, _guard) = provider();
        let f = child(&root, &name.bytes);
        // Rechazo limpio del OS al nombre: skip (no es lo que este test
        // prueba — ver prop_filename_bytes_survive_fs para esa cobertura).
        let Ok(mut sink) = p.write(&f).await else {
            continue;
        };
        sink.write(Bytes::from_static(b"1")).await.expect("chunk");
        match sink.commit().await {
            Ok(()) => {}
            Err(norte_proto::Error::InvalidPath | norte_proto::Error::Conflict { .. }) => {
                continue;
            }
            Err(e) => panic!("{}: commit inesperado: {e:?}", name.id),
        }

        let listed: Vec<norte_proto::VPath> = p
            .list(&root)
            .await
            .unwrap_or_else(|e| panic!("{}: list: {e:?}", name.id))
            .map(|r| r.unwrap_or_else(|e| panic!("{}: entrada: {e:?}", name.id)))
            .map(|e| e.path)
            .collect()
            .await;
        assert!(
            !listed.is_empty(),
            "{}: el listado debe ver el archivo recién escrito",
            name.id
        );
        for path in &listed {
            let st = p
                .stat(path)
                .await
                .unwrap_or_else(|e| panic!("{}: stat de {path:?}: {e:?}", name.id));
            assert_eq!(
                st.size,
                Some(1),
                "{}: stat de un path LISTADO debe hidratar el tamaño real",
                name.id
            );
        }
    }
}

/// Case-rename (`caja` → `CAJA`) en FS case-insensitive: el "destino" es el
/// propio origen con otra caja y debe proceder (issue #2: en Windows M0
/// devolvía `Conflict` por no poder comprobar la identidad real del archivo).
#[cfg(any(windows, target_os = "macos"))]
#[tokio::test]
async fn case_rename_succeeds_on_insensitive_fs() {
    let (p, root, base) = provider();
    if p.capabilities()
        .flags
        .contains(CapabilityFlags::CASE_SENSITIVE)
    {
        // El tempdir vive en un FS case-sensitive (posible en macOS):
        // el caso lo cubre el contract test de colisión por caja.
        return;
    }
    std::fs::write(base.join("caja"), b"x").unwrap();
    p.rename(&child(&root, b"caja"), &child(&root, b"CAJA"))
        .await
        .expect("case-rename de archivo permitido");
    // También para DIRECTORIOS (nlink de un dir nunca es 1: la guarda de
    // hardlinks no puede bloquearlo).
    std::fs::create_dir(base.join("carpeta")).unwrap();
    p.rename(&child(&root, b"carpeta"), &child(&root, b"CARPETA"))
        .await
        .expect("case-rename de dir permitido");
    let mut names: Vec<String> = std::fs::read_dir(&base)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["CAJA".to_owned(), "CARPETA".to_owned()],
        "dos dirents, caja nueva preservada"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn rename_between_hardlinks_is_conflict() {
    // rename(2) entre dos hardlinks del mismo inode es un no-op con éxito:
    // reportarlo como move sería mentirle al journal. Debe ser Conflict.
    let (p, root, base) = provider();
    std::fs::write(base.join("a"), b"x").unwrap();
    std::fs::hard_link(base.join("a"), base.join("b")).unwrap();
    match p.rename(&child(&root, b"a"), &child(&root, b"b")).await {
        Err(norte_proto::Error::Conflict { .. }) => {}
        other => panic!("esperaba Conflict, fue {other:?}"),
    }
    assert!(base.join("a").exists(), "origen intacto");
    assert!(base.join("b").exists(), "destino intacto");
}

// ---------- issue #10: soltar un stream libera el fd del productor ----------

/// Número de fds abiertos del proceso (incluye el del propio `read_dir`:
/// constante entre llamadas, válido para comparar).
#[cfg(target_os = "linux")]
fn open_fds() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .expect("/proc/self/fd")
        .count()
}

/// Espera (con deadline) a que el productor bloqueante note el canal
/// cerrado y suelte sus recursos.
#[cfg(target_os = "linux")]
async fn wait_fds_back_to(baseline: usize) -> usize {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let now = open_fds();
        if now <= baseline || std::time::Instant::now() > deadline {
            return now;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// Soltar un `ByteStream` a mitad de lectura debe liberar el fd del archivo
/// (issue #10): el productor nota el canal cerrado en el siguiente send.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn dropping_byte_stream_mid_read_releases_fd() {
    let (p, root, _) = provider();
    let f = child(&root, b"gordo.bin");
    // 8 MiB = 32 chunks: mucho más que el buffer del canal (8) — el
    // productor queda BLOQUEADO con el archivo abierto al soltar el stream.
    let mut sink = p.write(&f).await.unwrap();
    sink.write(Bytes::from(vec![0x5A; 8 * 1024 * 1024]))
        .await
        .unwrap();
    sink.commit().await.unwrap();

    let baseline = open_fds();
    let mut stream = p.read(&f, None).await.unwrap();
    let first = stream.next().await.expect("hay datos").expect("chunk ok");
    assert!(!first.is_empty());
    assert!(
        open_fds() > baseline,
        "sanidad: el productor tiene el archivo abierto"
    );
    drop(stream);
    let now = wait_fds_back_to(baseline).await;
    assert!(
        now <= baseline,
        "fd del productor filtrado tras soltar el ByteStream: {now} > {baseline}"
    );
}

/// Soltar un `EntryStream` a mitad de listado debe liberar el fd del
/// `read_dir` del productor (issue #10).
#[cfg(target_os = "linux")]
#[tokio::test]
async fn dropping_entry_stream_mid_list_releases_fd() {
    let (p, root, base) = provider();
    // Más entradas (200) que el buffer del canal (64): productor bloqueado.
    for i in 0..200 {
        std::fs::write(base.join(format!("f{i:03}")), b"x").unwrap();
    }
    let baseline = open_fds();
    let mut stream = p.list(&root).await.unwrap();
    let first = stream.next().await.expect("hay entradas");
    assert!(first.is_ok());
    drop(stream);
    let now = wait_fds_back_to(baseline).await;
    assert!(
        now <= baseline,
        "fd del productor filtrado tras soltar el EntryStream: {now} > {baseline}"
    );
}

/// Variante Windows del issue #10: si el productor filtrara su handle, el
/// borrado del árbol no terminaría (archivo delete-pending → dir no vacío).
#[cfg(windows)]
#[tokio::test]
async fn dropping_byte_stream_mid_read_releases_handle() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().to_path_buf();
    let p = LocalProvider::rooted(base.clone());
    let f = child(&LocalProvider::root(), b"gordo.bin");
    let mut sink = p.write(&f).await.unwrap();
    sink.write(Bytes::from(vec![0x5A; 8 * 1024 * 1024]))
        .await
        .unwrap();
    sink.commit().await.unwrap();

    let mut stream = p.read(&f, None).await.unwrap();
    let _ = stream.next().await.expect("hay datos").expect("chunk ok");
    drop(stream);
    drop(p);

    // El borrado solo culmina cuando el productor suelta el handle.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match std::fs::remove_dir_all(&base) {
            Ok(()) => break,
            Err(e) if std::time::Instant::now() > deadline => {
                panic!("handle del productor filtrado: {e}");
            }
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
        }
    }
}

/// Purga de la papelera REAL tras la suite (hallazgo M3 de fase 8): el
/// contrato trasheaba `norte-contract-trash-<pid>` en cada run — sin esto,
/// la papelera del desarrollador crecía para siempre. macOS no tiene
/// `os_limited`: aceptado y documentado en ADR 0009.
///
/// Desde la tarea 11b el contrato local se lleva su papelera dentro del
/// tempdir (`with_trash_home`), así que ya no ensucia nada; esto queda para
/// barrer lo que dejaron las runs anteriores, y porque los tests de
/// `restore_trashed` de aquí abajo SÍ usan la papelera de verdad (es lo que
/// prueban: que el crate `trash` sabe leer lo que escribimos).
#[cfg(any(target_os = "linux", windows))]
#[test]
fn purga_los_restos_del_contrato_en_la_papelera() {
    let Ok(items) = trash::os_limited::list() else {
        return; // sin papelera consultable: nada que purgar
    };
    let nuestros: Vec<_> = items
        .into_iter()
        .filter(|i| {
            i.name
                .to_string_lossy()
                .starts_with("norte-contract-trash-")
        })
        .collect();
    if !nuestros.is_empty() {
        let _ = trash::os_limited::purge_all(nuestros);
    }
}

/// Regla 3 vía `read`: una FIFO (o symlink a FIFO) jamás cuelga el hilo —
/// `read` la rechaza con `Unsupported` ANTES del open (un open de FIFO sin
/// escritor bloquea para siempre y la cancelación no lo interrumpe).
#[cfg(unix)]
#[tokio::test]
async fn read_de_fifo_no_cuelga() {
    use norte_proto::Error;
    use norte_vfs::Provider;
    let dir = tempfile::tempdir().expect("tempdir");
    let fifo = dir.path().join("pipe");
    let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY del test: CString NUL-terminada válida; mkfifo no retiene el puntero.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0, "mkfifo");
    std::os::unix::fs::symlink("pipe", dir.path().join("lpipe")).unwrap();

    let p = norte_vfs_local::LocalProvider::rooted(dir.path());
    let root = norte_vfs_local::LocalProvider::root();
    let seg = |b: &[u8]| norte_proto::Segment::new(b.to_vec()).unwrap();
    let deadline = std::time::Duration::from_secs(5);
    for name in [&b"pipe"[..], &b"lpipe"[..]] {
        let path = root.join(seg(name));
        let res = tokio::time::timeout(deadline, p.read(&path, None))
            .await
            .expect("read responde, jamás cuelga");
        let err = res.err().expect("no-regular rechazado honesto");
        assert_eq!(err, Error::Unsupported);
    }
}

/// Resume real sobre el FS (ADR 0012): keep conserva el `.norte-partial`
/// con nombre ESTABLE, `open_resumable` lo reencuentra y reanuda; el GC
/// barre los huérfanos por edad.
#[tokio::test]
async fn resume_local_conserva_reanuda_y_gc() {
    use norte_vfs::Provider;
    let dir = tempfile::tempdir().expect("tempdir");
    let p = norte_vfs_local::LocalProvider::rooted(dir.path());
    let root = norte_vfs_local::LocalProvider::root();
    let seg = |b: &[u8]| norte_proto::Segment::new(b.to_vec()).unwrap();
    let f = root.join(seg(b"grande.bin"));

    // Primer tramo: 4 bytes, keep (conserva el parcial, no publica).
    let (mut sink, already) = p.open_resumable(&f).await.expect("open 1");
    assert_eq!(already, 0);
    sink.write(Bytes::from_static(b"hola")).await.unwrap();
    sink.keep().await.expect("keep");
    assert_eq!(
        p.stat(&f).await.unwrap_err(),
        norte_proto::Error::NotFound,
        "keep no publica"
    );
    // Hay UN .norte-partial en disco (nombre estable).
    let partials: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|d| {
            d.file_name()
                .to_string_lossy()
                .starts_with(".norte-partial.")
        })
        .collect();
    assert_eq!(partials.len(), 1, "un parcial estable conservado");

    // Segundo tramo: reanuda desde los 4 bytes.
    let (mut sink, already) = p.open_resumable(&f).await.expect("open 2");
    assert_eq!(already, 4, "reanuda tras lo conservado");
    sink.write(Bytes::from_static(b"mundo")).await.unwrap();
    sink.commit().await.expect("commit");
    assert_eq!(
        std::fs::read(dir.path().join("grande.bin")).unwrap(),
        b"holamundo"
    );
    // El parcial desapareció al commitear.
    let quedan = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|d| {
            d.file_name()
                .to_string_lossy()
                .starts_with(".norte-partial.")
        })
        .count();
    assert_eq!(quedan, 0, "commit publica y limpia el parcial");

    // GC de un huérfano: keep otro parcial y bárrelo con older_than=0.
    let g = root.join(seg(b"otro.bin"));
    let (mut sink, _) = p.open_resumable(&g).await.expect("open g");
    sink.write(Bytes::from_static(b"x")).await.unwrap();
    sink.keep().await.expect("keep g");
    let removed = p
        .gc_partials(&root, std::time::Duration::ZERO)
        .await
        .expect("gc");
    assert_eq!(removed, 1, "el huérfano se barre");
}

/// H2 del encoding-auditor: `gc_partials` reconoce el staging por su FORMA
/// exacta, no por el prefijo — un archivo REAL del usuario que empiece por
/// `.norte-partial.` JAMÁS se borra.
#[tokio::test]
async fn gc_partials_no_toca_archivos_del_usuario() {
    use norte_vfs::Provider;
    let dir = tempfile::tempdir().expect("tempdir");
    let p = norte_vfs_local::LocalProvider::rooted(dir.path());
    let root = norte_vfs_local::LocalProvider::root();
    let seg = |b: &[u8]| norte_proto::Segment::new(b.to_vec()).unwrap();

    // Archivo del usuario con el prefijo pero NO la forma de un staging.
    std::fs::write(dir.path().join(".norte-partial.backup"), b"mio").unwrap();
    std::fs::write(dir.path().join(".norte-partial.notas.txt"), b"mio").unwrap();
    // Un parcial de verdad (forma estable: 32 hex).
    let g = root.join(seg(b"grande.bin"));
    let (mut sink, _) = p.open_resumable(&g).await.expect("open");
    sink.write(Bytes::from_static(b"x")).await.unwrap();
    sink.keep().await.expect("keep");

    let removed = p
        .gc_partials(&root, std::time::Duration::ZERO)
        .await
        .expect("gc");
    assert_eq!(removed, 1, "solo el parcial de verdad se barre");
    assert!(
        dir.path().join(".norte-partial.backup").exists(),
        "el archivo del usuario sobrevive"
    );
    assert!(dir.path().join(".norte-partial.notas.txt").exists());
}

#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "ios"),
    not(target_os = "android")
))]
#[tokio::test]
async fn restore_trashed_brings_back_by_original_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("v.txt"), b"data").expect("seed");
    let p = LocalProvider::rooted(dir.path().to_path_buf());
    let victim = child(&LocalProvider::root(), b"v.txt");

    // Si la papelera del OS no está disponible en el runner, skip limpio.
    if p.trash(
        &victim,
        &norte_vfs::trash::TrashId::new(0, u64::from(line!())),
    )
    .await
    .is_err()
    {
        eprintln!("skip: papelera del OS no disponible");
        return;
    }
    assert!(matches!(
        p.stat(&victim).await,
        Err(norte_proto::Error::NotFound)
    ));

    p.restore_trashed(&victim).await.expect("restore");
    assert_eq!(
        std::fs::read(dir.path().join("v.txt")).expect("restaurado"),
        b"data"
    );
}

/// Regla 1: el match por ruta original y la restauración preservan bytes
/// hostiles (nombre no-UTF8) — round-trip byte-exacto por la papelera real.
#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "ios"),
    not(target_os = "android")
))]
#[tokio::test]
async fn restore_trashed_preserves_hostile_bytes() {
    use std::os::unix::ffi::OsStrExt;
    let name: &[u8] = b"h\xffstil.bin"; // 0xFF: inválido en cualquier UTF-8
    let dir = tempfile::tempdir().expect("tempdir");
    let native = dir.path().join(std::ffi::OsStr::from_bytes(name));
    std::fs::write(&native, b"payload").expect("seed");
    let p = LocalProvider::rooted(dir.path().to_path_buf());
    let victim = child(&LocalProvider::root(), name);
    if p.trash(
        &victim,
        &norte_vfs::trash::TrashId::new(0, u64::from(line!())),
    )
    .await
    .is_err()
    {
        eprintln!("skip: papelera del OS no disponible");
        return;
    }
    assert!(matches!(
        p.stat(&victim).await,
        Err(norte_proto::Error::NotFound)
    ));
    p.restore_trashed(&victim).await.expect("restore");
    assert_eq!(
        std::fs::read(&native).expect("restaurado byte-exacto"),
        b"payload"
    );
}

/// Estricto: sin ítem en la papelera → `NotFound`; destino ocupado → `Conflict`
/// (jamás pisa — la invariante de seguridad del undo).
#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "ios"),
    not(target_os = "android")
))]
#[tokio::test]
async fn restore_trashed_is_strict() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = LocalProvider::rooted(dir.path().to_path_buf());
    let ghost = child(&LocalProvider::root(), b"jamas_borrado.bin");
    // Sin ítem correspondiente en la papelera → NotFound (o Unsupported si el
    // runner no tiene papelera; ambos son fallos limpios, nunca pisa).
    assert!(matches!(
        p.restore_trashed(&ghost).await,
        Err(norte_proto::Error::NotFound | norte_proto::Error::Unsupported)
    ));

    // Destino ocupado: trashear, recrear algo en su sitio, restaurar → Conflict.
    std::fs::write(dir.path().join("v.txt"), b"orig").expect("seed");
    let victim = child(&LocalProvider::root(), b"v.txt");
    if p.trash(
        &victim,
        &norte_vfs::trash::TrashId::new(0, u64::from(line!())),
    )
    .await
    .is_err()
    {
        eprintln!("skip: papelera del OS no disponible");
        return;
    }
    std::fs::write(dir.path().join("v.txt"), b"nuevo").expect("recrea");
    assert!(matches!(
        p.restore_trashed(&victim).await,
        Err(norte_proto::Error::Conflict { .. })
    ));
    // No pisó: el nuevo contenido sigue intacto.
    assert_eq!(std::fs::read(dir.path().join("v.txt")).unwrap(), b"nuevo");
}

// ---------- attrs posix (#108 bloque 2) ----------

#[cfg(unix)]
#[tokio::test]
async fn attrs_posix_en_stat_y_list() {
    use norte_proto::AttrValue;
    use norte_vfs::{AttrRequest, ListOptions};
    use std::os::unix::fs::MetadataExt;
    let (p, root, base) = provider();
    std::fs::write(base.join("a.txt"), b"hola").expect("seed");
    let opt = ListOptions {
        attrs: AttrRequest::sanitized(
            [
                "posix.mode",
                "posix.uid",
                "posix.gid",
                "posix.nlink",
                "posix.ctime_ms",
            ]
            .map(str::to_owned),
        ),
    };
    let e = p
        .stat_with(&child(&root, b"a.txt"), &opt)
        .await
        .expect("stat_with");
    let md = std::fs::symlink_metadata(base.join("a.txt")).expect("md");
    assert_eq!(
        e.attrs.get("posix.mode"),
        Some(&AttrValue::Uint(u64::from(md.mode())))
    );
    assert_eq!(
        e.attrs.get("posix.uid"),
        Some(&AttrValue::Uint(u64::from(md.uid())))
    );
    assert_eq!(
        e.attrs.get("posix.gid"),
        Some(&AttrValue::Uint(u64::from(md.gid())))
    );
    assert_eq!(
        e.attrs.get("posix.nlink"),
        Some(&AttrValue::Uint(md.nlink()))
    );
    assert!(matches!(
        e.attrs.get("posix.ctime_ms"),
        Some(AttrValue::TimeMs(_))
    ));

    // list_with promociona: attrs presentes Y size/mtime hidratados de paso.
    let mut s = p.list_with(&root, &opt).await.expect("list_with");
    let le = s.next().await.expect("una entrada").expect("ok");
    assert!(le.attrs.contains_key("posix.mode"));
    assert!(le.size.is_some(), "la promoción a metadata llena size");

    // Camino rápido intacto (#52): sin petición, lazy como siempre.
    let mut s = p.list(&root).await.expect("list");
    let le = s.next().await.expect("una entrada").expect("ok");
    assert!(le.attrs.is_empty() && le.size.is_none());

    // Petición SIN attr local anunciado: también camino lazy.
    let ajeno = ListOptions {
        attrs: AttrRequest::sanitized(["s3.etag".to_owned()]),
    };
    let mut s = p.list_with(&root, &ajeno).await.expect("list_with");
    let le = s.next().await.expect("una entrada").expect("ok");
    assert!(le.attrs.is_empty() && le.size.is_none());
}

// ---------------------------------------------------------------------------
// Papelera freedesktop (`trash_fdo`): la que SABE dónde dejó el fichero.
//
// Todos estos tests inyectan su propia raíz XDG bajo el tempdir: ni tocan la
// papelera de verdad del desarrollador ni dependen de en qué dispositivo vive.
// ---------------------------------------------------------------------------

#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "ios"),
    not(target_os = "android")
))]
mod papelera_freedesktop {
    use super::{LocalProvider, Provider, Segment, VPath, child};
    use std::os::unix::ffi::OsStrExt;

    /// Provider enraizado en un tempdir con su papelera DENTRO: así el destino
    /// recuperable es una ruta que este mismo provider sabe resolver.
    fn provider() -> (tempfile::TempDir, LocalProvider) {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = LocalProvider::rooted(dir.path()).with_trash_home(dir.path().join(".xdg"));
        (dir, p)
    }

    /// Un id de engine distinto por operación. El instante manda —es lo que
    /// viaja al sidecar, con resolución de SEGUNDO—, así que los ids de test
    /// se separan por segundos enteros.
    fn id(n: u64) -> norte_vfs::trash::TrashId {
        norte_vfs::trash::TrashId::new(1_726_000_000_000 + n * 1000, n)
    }

    /// Raíz de la papelera en el disco real del test.
    fn trash_root(dir: &tempfile::TempDir) -> std::path::PathBuf {
        dir.path().join(".xdg").join("Trash")
    }

    /// Siembra un fichero con BYTES de nombre arbitrarios (regla 1: jamás pasa
    /// por `str`).
    fn seed(dir: &tempfile::TempDir, name: &[u8], body: &[u8]) -> VPath {
        let native = dir.path().join(std::ffi::OsStr::from_bytes(name));
        std::fs::write(&native, body).expect("seed");
        child(&LocalProvider::root(), name)
    }

    fn native_of(dir: &tempfile::TempDir, p: &VPath) -> std::path::PathBuf {
        let mut out = dir.path().to_path_buf();
        for seg in p.segments() {
            out.push(std::ffi::OsStr::from_bytes(seg));
        }
        out
    }

    /// El sidecar que le toca a un destino `…/files/<n>`.
    fn sidecar_of(dir: &tempfile::TempDir, dest: &VPath) -> std::path::PathBuf {
        let native = native_of(dir, dest);
        let name = native.file_name().expect("nombre");
        let mut file = name.as_bytes().to_vec();
        file.extend_from_slice(b".trashinfo");
        trash_root(dir)
            .join("info")
            .join(std::ffi::OsStr::from_bytes(&file))
    }

    #[tokio::test]
    async fn trashing_returns_the_path_it_actually_used() {
        let (dir, p) = provider();
        let victim = seed(&dir, b"a.txt", b"datos");
        let dest = p
            .trash(&victim, &id(1))
            .await
            .expect("trash")
            .expect("freedesktop nombra su destino");
        assert!(
            dest.to_wire().contains("/Trash/files/"),
            "{}",
            dest.to_wire()
        );
        p.stat(&dest).await.expect("el fichero ESTÁ ahí");
        assert_eq!(
            std::fs::read(native_of(&dir, &dest)).expect("bytes"),
            b"datos"
        );
        assert!(p.trash_restorable(), "y el provider lo promete");
    }

    /// El bug en un test: sin destinos distintos, el undo de una pareja
    /// `trashed`+`created` desentierra su propio entierro.
    #[tokio::test]
    async fn two_victims_with_one_name_get_two_destinations() {
        let (dir, p) = provider();
        let victim = seed(&dir, b"a.txt", b"first");
        let first = p
            .trash(&victim, &id(1))
            .await
            .expect("trash")
            .expect("dest");
        let victim = seed(&dir, b"a.txt", b"second");
        let second = p
            .trash(&victim, &id(2))
            .await
            .expect("trash")
            .expect("dest");

        assert_ne!(first.to_wire(), second.to_wire());
        assert_eq!(std::fs::read(native_of(&dir, &first)).expect("1"), b"first");
        assert_eq!(
            std::fs::read(native_of(&dir, &second)).expect("2"),
            b"second"
        );
        // La deduplicación que la spec describe.
        assert!(
            second.to_wire().ends_with("a.txt.2"),
            "{}",
            second.to_wire()
        );
    }

    #[tokio::test]
    async fn the_trashinfo_sidecar_is_written_and_names_the_original() {
        let (dir, p) = provider();
        let victim = seed(&dir, b"a.txt", b"x");
        let dest = p
            .trash(&victim, &id(1))
            .await
            .expect("trash")
            .expect("dest");

        let bytes = std::fs::read(sidecar_of(&dir, &dest)).expect("sidecar");
        let text = String::from_utf8(bytes).expect("el trashinfo es UTF-8 por spec");
        assert!(text.starts_with("[Trash Info]\n"), "{text}");
        let esperada = dir.path().canonicalize().expect("canon").join("a.txt");
        assert!(
            text.contains(&format!("Path={}\n", esperada.display())),
            "{text}"
        );
        assert!(text.contains("DeletionDate="), "{text}");
    }

    /// Regla 1. El sidecar percent-codifica; el FICHERO conserva sus bytes. Lo
    /// que este test comprueba de verdad —y lo que la primera versión NO
    /// comprobaba (MINOR-1 del encoding-auditor)— es que la ruta del sidecar,
    /// DECODIFICADA, es byte a byte la ruta original: `restore_from` no lee el
    /// sidecar, así que sin esta aserción un fallo sistemático de escapado
    /// pasaría verde y solo lo notaría una papelera gráfica.
    #[tokio::test]
    async fn a_hostile_name_survives_the_round_trip() {
        let (dir, p) = provider();
        let raiz = dir.path().canonicalize().expect("canon");
        let mut probados = 0usize;
        for (n, name) in norte_testkit::corpus::hostile_names()
            .into_iter()
            .enumerate()
        {
            let native = dir.path().join(std::ffi::OsStr::from_bytes(&name.bytes));
            // Un nombre que este FS no acepta simplemente no está (APFS/NTFS).
            // El payload es DISTINTO por fixture: con uno común, devolver el
            // destino de otra entrada pasaría desapercibido.
            if std::fs::write(&native, name.id.as_bytes()).is_err() {
                continue;
            }
            let Ok(victim) =
                Segment::new(name.bytes.clone()).map(|s| LocalProvider::root().join(s))
            else {
                continue;
            };
            probados += 1;
            let dest = p
                .trash(&victim, &id(1000 + n as u64))
                .await
                .expect("trash")
                .expect("dest");

            // El sidecar es ASCII puro aunque el nombre no sea ni UTF-8, y son
            // tres líneas exactas: un `\n` en un nombre no puede inyectar una
            // cuarta ni un segundo `Path=`.
            let text = std::fs::read_to_string(sidecar_of(&dir, &dest)).expect("sidecar");
            assert!(text.is_ascii(), "{}: {text}", name.id);
            assert_eq!(text.lines().count(), 3, "{}: {text}", name.id);
            assert_eq!(text.lines().next(), Some("[Trash Info]"), "{}", name.id);
            assert_eq!(
                text.lines().filter(|l| l.starts_with("Path=")).count(),
                1,
                "{}: {text}",
                name.id
            );

            // Y la ruta que guarda es, decodificada, la original BYTE A BYTE.
            let codificada = text
                .lines()
                .find_map(|l| l.strip_prefix("Path="))
                .expect("Path=");
            assert_eq!(
                percent_decode(codificada),
                raiz.join(std::ffi::OsStr::from_bytes(&name.bytes))
                    .as_os_str()
                    .as_bytes(),
                "{}",
                name.id
            );

            // Y vuelve a SU ruta, con SUS bytes.
            p.restore_from(&dest, &victim).await.expect("restore_from");
            assert_eq!(
                std::fs::read(&native).expect("de vuelta"),
                name.id.as_bytes(),
                "{}",
                name.id
            );
            std::fs::remove_file(&native).expect("limpia");
        }
        assert!(probados >= 40, "el corpus se saltó casi entero: {probados}");
    }

    /// Decodifica el `Path=` de un `.trashinfo` a BYTES (jamás a `String`: la
    /// ruta original puede no ser UTF-8).
    fn percent_decode(s: &str) -> Vec<u8> {
        let raw = s.as_bytes();
        let mut out = Vec::with_capacity(raw.len());
        let mut i = 0;
        while i < raw.len() {
            if raw[i] == b'%' && i + 2 < raw.len() {
                let hex = std::str::from_utf8(&raw[i + 1..i + 3]).expect("ascii");
                out.push(u8::from_str_radix(hex, 16).expect("hex válido"));
                i += 3;
            } else {
                out.push(raw[i]);
                i += 1;
            }
        }
        out
    }

    #[tokio::test]
    async fn restoring_from_the_recorded_destination_needs_no_guessing() {
        let (dir, p) = provider();
        let victim = seed(&dir, b"a.txt", b"x");
        let dest = p
            .trash(&victim, &id(1))
            .await
            .expect("trash")
            .expect("dest");
        let sidecar = sidecar_of(&dir, &dest);
        assert!(sidecar.exists(), "el sidecar estaba");

        p.restore_from(&dest, &victim).await.expect("restore");
        assert_eq!(
            std::fs::read(dir.path().join("a.txt")).expect("vuelto"),
            b"x"
        );
        assert!(!sidecar.exists(), "el sidecar se va con él");
        assert!(p.stat(&dest).await.is_err(), "y el payload ya no está");
    }

    /// #99: el `id` lo genera el engine y un reintento tras un fallo
    /// transitorio tiene que CONVERGER en la misma entrada, no crear una
    /// segunda ni perder el `reversal_ref`.
    #[tokio::test]
    async fn a_retry_with_the_same_id_converges_on_the_same_entry() {
        let (dir, p) = provider();
        let victim = seed(&dir, b"a.txt", b"x");
        let first = p
            .trash(&victim, &id(7))
            .await
            .expect("trash")
            .expect("dest");
        // La víctima ya no está: el reintento reconoce su propia entrada.
        let again = p
            .trash(&victim, &id(7))
            .await
            .expect("el reintento converge")
            .expect("y conserva el destino");
        assert_eq!(first.to_wire(), again.to_wire());
        // Y no ha creado una segunda entrada.
        let n = std::fs::read_dir(trash_root(&dir).join("files"))
            .expect("files")
            .count();
        assert_eq!(n, 1, "una sola entrada");
    }

    /// Sin entrada previa nuestra, una víctima ausente es `NotFound` — no se
    /// reclama la entrada de OTRA operación sobre la misma ruta.
    #[tokio::test]
    async fn a_missing_victim_without_our_entry_is_not_found() {
        let (dir, p) = provider();
        let fantasma = child(&LocalProvider::root(), b"jamas.txt");
        assert_eq!(
            p.trash(&fantasma, &id(1)).await,
            Err(norte_proto::Error::NotFound)
        );
        // Y una entrada AJENA con el mismo nombre tampoco se reclama.
        let victim = seed(&dir, b"a.txt", b"del vecino");
        p.trash(&victim, &id(1))
            .await
            .expect("trash")
            .expect("dest");
        assert_eq!(
            p.trash(&victim, &id(2)).await,
            Err(norte_proto::Error::NotFound),
            "otra operación no hereda la entrada de nadie"
        );
    }

    /// Un sidecar SEMBRADO en `info/` no se pisa ni se reclama: la víctima se
    /// va al siguiente nombre libre.
    #[tokio::test]
    async fn a_planted_sidecar_is_never_overwritten() {
        let (dir, p) = provider();
        let info = trash_root(&dir).join("info");
        std::fs::create_dir_all(&info).expect("info");
        std::fs::write(info.join("a.txt.trashinfo"), b"[Trash Info]\nPath=/otro\n").expect("plant");

        let victim = seed(&dir, b"a.txt", b"mio");
        let dest = p
            .trash(&victim, &id(1))
            .await
            .expect("trash")
            .expect("dest");
        assert!(dest.to_wire().ends_with("a.txt.2"), "{}", dest.to_wire());
        assert_eq!(
            std::fs::read(info.join("a.txt.trashinfo")).expect("intacto"),
            b"[Trash Info]\nPath=/otro\n"
        );
    }

    /// Un `files/<n>` SEMBRADO (aquí un symlink a algo valioso) tampoco se
    /// pisa: el movimiento es no-replace y la víctima se va al siguiente
    /// nombre.
    #[tokio::test]
    async fn a_planted_payload_is_never_clobbered() {
        let (dir, p) = provider();
        let files = trash_root(&dir).join("files");
        std::fs::create_dir_all(&files).expect("files");
        let valioso = dir.path().join("valioso.txt");
        std::fs::write(&valioso, b"no me toques").expect("seed");
        std::os::unix::fs::symlink(&valioso, files.join("a.txt")).expect("plant");

        let victim = seed(&dir, b"a.txt", b"mio");
        let dest = p
            .trash(&victim, &id(1))
            .await
            .expect("trash")
            .expect("dest");
        assert!(dest.to_wire().ends_with("a.txt.2"), "{}", dest.to_wire());
        assert_eq!(
            std::fs::read(&valioso).expect("intacto"),
            b"no me toques",
            "el symlink sembrado no se siguió ni se pisó"
        );
    }
}

// ---------- capabilities por DIRECTORIO (ADR 0054, #153/#145) ----------

/// `capabilities_at` no escribe NUNCA, en ningún filesystem: se responde tras
/// el gate de LECTURA, así que una sonda de escritura ahí sería un fichero
/// creado por un actor que solo tiene permiso para mirar. El tempdir de este
/// test está en tmpfs, que la escalera de solo lectura NO reconoce — es decir,
/// es justo el caso que antes caía en la sonda de escritura.
#[tokio::test]
async fn capabilities_at_never_writes_anywhere() {
    let (p, root, base) = provider();
    let sub = base.join("sub");
    std::fs::create_dir(&sub).expect("mkdir");
    let vsub = child(&root, b"sub");

    let _ = p.capabilities_at(&vsub).await.expect("responde");

    let restos: Vec<_> = std::fs::read_dir(&sub)
        .expect("listar")
        .map(|e| e.expect("entrada").file_name())
        .collect();
    assert!(
        restos.is_empty(),
        "ni durante ni después: la escalera no muta nada ({restos:?})"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_directory_without_write_permission_still_gets_an_answer() {
    // Un mount de solo lectura no se puede fabricar en CI; un directorio sin
    // permiso de escritura sí, y es lo que rompía a la sonda de ESCRITURA:
    // fallaba y no distinguía "no escribible" de "no pliega".
    use std::os::unix::fs::PermissionsExt;
    let (p, root, base) = provider();
    let ro = base.join("ro");
    std::fs::create_dir(&ro).expect("mkdir");
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o500)).expect("chmod");

    let caps = p
        .capabilities_at(&child(&root, b"ro"))
        .await
        .expect("responde igual");

    // Qué responda depende del FS de CI; lo que se afirma es que RESPONDE lo
    // mismo que para la raíz, que está en el mismo filesystem — y sin haber
    // podido escribir en el directorio para averiguarlo.
    assert_eq!(
        caps.flags.contains(CapabilityFlags::CASE_SENSITIVE),
        p.capabilities_at(&root)
            .await
            .expect("responde")
            .flags
            .contains(CapabilityFlags::CASE_SENSITIVE)
    );
}

#[tokio::test]
async fn two_paths_to_one_directory_probe_once() {
    let (p, root, base) = provider();
    std::fs::create_dir(base.join("sub")).expect("mkdir");
    let directo = child(&root, b"sub");

    let antes = p.caps_at_probe_count();
    let _ = p.capabilities_at(&directo).await.expect("responde");
    let _ = p.capabilities_at(&directo).await.expect("responde");
    assert_eq!(
        p.caps_at_probe_count() - antes,
        1,
        "la clave es (dev, ino): la segunda pregunta sale de la caché"
    );
}

#[tokio::test]
async fn a_file_is_answered_by_its_containing_directory() {
    let (p, root, base) = provider();
    std::fs::write(base.join("f.txt"), b"x").expect("write");

    let del_fichero = p
        .capabilities_at(&child(&root, b"f.txt"))
        .await
        .expect("responde");
    let del_dir = p.capabilities_at(&root).await.expect("responde");

    assert_eq!(
        del_fichero, del_dir,
        "la pregunta es siempre sobre el directorio que lo contiene"
    );
}

/// Una ruta que no está NO es un error: `capabilities()` jamás pudo fallar, y
/// hacer fallar a su versión por ubicación rompería el caso corriente de
/// planificar hacia un destino que todavía no existe.
///
/// Lo que sí lleva ese camino degradado es `CONFINED_WRITES`, y no es una
/// excepción caprichosa: confinar es de la PLATAFORMA —hay `openat` o no lo
/// hay—, no del árbol ni de si la ruta existe todavía. Sin esto,
/// `file:///destino-que-no-existe` contestaría «no sé confinar» y `file:///`
/// que sí, dos respuestas distintas de la misma máquina — y la primera es
/// justo la que ve un mirror al planificar (revisión de seguridad de W5 B).
#[tokio::test]
async fn a_missing_path_answers_the_declaration() {
    let (p, root, _) = provider();
    let mut esperado = p.capabilities();
    esperado
        .flags
        .set(norte_proto::CapabilityFlags::CONFINED_WRITES, cfg!(unix));
    assert_eq!(
        p.capabilities_at(&child(&root, b"no-existe"))
            .await
            .expect("responde igualmente"),
        esperado
    );
}

/// El caso que DISCRIMINA la escalera: un directorio sin permiso de escritura
/// **en ext4**. La sonda de escritura no puede responder ahí —es justo su
/// límite—, así que una respuesta correcta solo puede venir del peldaño de
/// solo lectura (`statfs` + `FS_IOC_GETFLAGS`).
///
/// Se enraíza en el árbol del repo y no en `/tmp`, que en esta máquina es
/// tmpfs: sobre tmpfs la escalera cae al peldaño de escritura y el test no
/// probaría nada.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_read_only_ext4_directory_is_answered_without_writing() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir en el repo");
    let base = dir.path().to_path_buf();
    let ro = base.join("ro");
    std::fs::create_dir(&ro).expect("mkdir");
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    let p = LocalProvider::rooted(base.clone()).with_guard(Box::new(dir));

    let caps = p
        .capabilities_at(&child(&LocalProvider::root(), b"ro"))
        .await
        .expect("responde");

    // Si el árbol del repo no está en ext4/f2fs (un contenedor con overlayfs,
    // por ejemplo), la escalera cae al peldaño de escritura, que sobre este
    // directorio no puede responder — y entonces el test no aplica.
    let en_ext4 = std::process::Command::new("stat")
        .args(["-f", "-c", "%T", base.to_str().expect("ruta de test ASCII")])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
    if en_ext4.as_deref() != Some("ext2/ext3") {
        eprintln!("skip: el árbol del repo no está en ext4 ({en_ext4:?})");
        return;
    }

    assert!(
        caps.flags.contains(CapabilityFlags::CASE_SENSITIVE),
        "ext4 sin +F distingue caja, y aquí no se pudo escribir para averiguarlo"
    );
    assert!(
        !caps.flags.contains(CapabilityFlags::FULL_FOLD),
        "y sin +F no expande"
    );
}

/// Roadmap ítem 8: un fichero con un agujero se copia SIN materializarlo.
///
/// Las dos aserciones dicen cosas distintas y las dos hacen falta: los bytes
/// son idénticos (que es la corrección) y los BLOQUES no (que es lo único que
/// demuestra que la optimización ocurrió). Sin la segunda, el test pasaría
/// igual con la implementación de antes.
#[cfg(unix)]
#[tokio::test]
async fn un_destino_con_agujero_no_se_materializa() {
    use std::os::unix::fs::MetadataExt as _;

    // 64 MiB de agujero: el caso de la imagen de VM que nombra el roadmap.
    const HUECO: u64 = 64 * 1024 * 1024;

    let (p, root, base) = provider();
    let f = std::fs::File::create(base.join("origen.img")).expect("crear");
    f.set_len(HUECO).expect("agujero");
    drop(f);

    let mut sink = p.write(&child(&root, b"destino.img")).await.expect("write");
    let mut leido = p
        .read(&child(&root, b"origen.img"), None)
        .await
        .expect("read");
    while let Some(chunk) = leido.next().await {
        sink.write(chunk.expect("chunk")).await.expect("escribe");
    }
    sink.commit().await.expect("commit");

    let md = std::fs::metadata(base.join("destino.img")).expect("stat");
    assert_eq!(md.len(), HUECO, "el tamaño LÓGICO se conserva entero");
    assert_eq!(
        std::fs::read(base.join("destino.img")).expect("leer"),
        vec![0u8; usize::try_from(HUECO).expect("cabe")],
        "y los bytes que se leen son los mismos"
    );
    assert!(
        md.blocks() * 512 < HUECO / 8,
        "pero el disco no los guarda: {} bloques para {HUECO} bytes",
        md.blocks()
    );
}

/// El caso que la optimización NO puede romper: ceros que alguien escribió a
/// propósito, en medio de datos. Que el destino salga disperso está permitido;
/// que un byte cambie, no.
#[tokio::test]
async fn unos_ceros_en_medio_se_leen_igual() {
    let (p, root, _base) = provider();
    let mut contenido = vec![b'a'; 1024];
    contenido.extend(std::iter::repeat_n(0u8, 256 * 1024));
    contenido.extend(std::iter::repeat_n(b'z', 1024));

    let mut sink = p.write(&child(&root, b"mixto.bin")).await.expect("write");
    sink.write(Bytes::from(contenido.clone()))
        .await
        .expect("escribe");
    sink.commit().await.expect("commit");

    let mut leido = p
        .read(&child(&root, b"mixto.bin"), None)
        .await
        .expect("read");
    let mut out = Vec::new();
    while let Some(chunk) = leido.next().await {
        out.extend_from_slice(&chunk.expect("chunk"));
    }
    assert_eq!(out, contenido, "byte a byte, sin excepciones");
}

/// Y la reanudación sigue sabiendo por dónde iba: el agujero tiene que contar
/// en la LONGITUD del staging desde que se escribe, no desde el commit — es lo
/// que leen `open_resumable` (su `already`) y `partial_digest`. Con la longitud
/// aplazada, un parcial que acabara en agujero diría tener menos bytes de los
/// que tiene y el reintento escribiría encima de lo ya hecho.
#[tokio::test]
async fn un_agujero_cuenta_en_el_offset_de_reanudacion() {
    let (p, root, _base) = provider();
    let destino = child(&root, b"resume.bin");

    let (mut sink, already) = p.open_resumable(&destino).await.expect("abre");
    assert_eq!(already, 0, "staging fresco");
    sink.write(Bytes::from(vec![0u8; 128 * 1024]))
        .await
        .expect("todo ceros");
    sink.keep().await.expect("conserva el parcial");

    let (_sink, already) = p.open_resumable(&destino).await.expect("reabre");
    assert_eq!(
        already,
        128 * 1024,
        "el agujero YA cuenta: reanudar desde 0 recopiaría lo hecho"
    );
}
