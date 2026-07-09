//! Tests específicos del provider local que el contrato genérico no cubre:
//! symlinks (nunca seguidos), chunking multi-chunk, limpieza del partial,
//! sondeo de capabilities.

use bytes::Bytes;
use futures::StreamExt;
use norte_proto::{CapabilityFlags, EntryKind, Segment, VPath};
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

/// Cuenta ficheros en `base` cuyo nombre empieza por `prefix` (el staging
/// lleva sufijo único, no un nombre fijo).
fn partials_with_prefix(base: &std::path::Path, prefix: &str) -> usize {
    std::fs::read_dir(base)
        .expect("read_dir")
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .count()
}

#[tokio::test]
async fn capabilities_are_probed() {
    let (p, _, _) = provider();
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

    let mut stream = p.read(&f).await.unwrap();
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
        partials_with_prefix(&base, "obra.norte-partial"),
        1,
        "el staging existe durante la escritura"
    );
    sink.abort().await.unwrap();
    assert_eq!(
        partials_with_prefix(&base, "obra.norte-partial"),
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
        assert_eq!(partials_with_prefix(&base, "tirada.norte-partial"), 1);
        // Soltar sin commit: abort best-effort en Drop.
    }
    assert_eq!(
        partials_with_prefix(&base, "tirada.norte-partial"),
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
        partials_with_prefix(&base, "final.norte-partial"),
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
