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
