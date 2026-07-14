//! Papelera lógica `.norte-trash/` de sftp (fase 9b, ADR 0019) contra el
//! servidor sftp in-process.
#![cfg(target_os = "linux")]

mod common;

use bytes::Bytes;
use futures::StreamExt;
use norte_proto::{Authority, CapabilityFlags, Segment, VPath};
use norte_vfs::Provider;
use norte_vfs::trash;
use norte_vfs_sftp::SftpProvider;

/// Provider fresco sobre tempdir + servidor in-process, con la papelera
/// lógica en el estado pedido.
async fn fresh(logical_trash: bool) -> SftpProvider {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_path_buf();
    let session = common::connect(&base, common::Mode::Honest).await;
    std::mem::forget(dir);
    SftpProvider::new(session, "/").with_logical_trash(logical_trash)
}

/// La raíz remota del cliente (`/`), con authority de test.
fn root() -> VPath {
    SftpProvider::root(Authority::new("test:22").expect("authority"))
}

/// Drena un `ByteStream` a bytes.
async fn read_all(p: &SftpProvider, path: &VPath) -> Vec<u8> {
    let mut rd = p.read(path, None).await.expect("read");
    let mut out = Vec::new();
    while let Some(c) = rd.next().await {
        out.extend_from_slice(&c.expect("chunk"));
    }
    out
}

/// Lista los nombres (bytes) de los hijos de un dir remoto (drena el
/// `EntryStream`).
async fn child_names(p: &SftpProvider, dir: &VPath) -> Vec<Vec<u8>> {
    let mut stream = p.list(dir).await.expect("list");
    let mut names = Vec::new();
    while let Some(item) = stream.next().await {
        let entry = item.expect("entry");
        names.push(
            entry
                .path
                .file_name()
                .expect("hijo con nombre")
                .as_bytes()
                .to_vec(),
        );
    }
    names
}

#[tokio::test]
async fn trash_capability_follows_the_flag() {
    let off = fresh(false).await;
    assert!(!off.capabilities().flags.contains(CapabilityFlags::TRASH));

    let on = fresh(true).await;
    assert!(on.capabilities().flags.contains(CapabilityFlags::TRASH));
}

#[tokio::test]
async fn trash_moves_tree_and_writes_info() {
    let p = fresh(true).await;
    let victim = root().join(Segment::new(b"victim.txt".to_vec()).unwrap());

    // Siembra el archivo.
    let mut sink = p.write(&victim).await.expect("write");
    sink.write(Bytes::from_static(b"contenido"))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");

    // A la papelera.
    p.trash(&victim).await.expect("trash");

    // El origen desaparece.
    assert!(matches!(
        p.stat(&victim).await,
        Err(norte_proto::Error::NotFound)
    ));

    // `.norte-trash/<id>/` existe con UNA entrada.
    let trash_dir = root().join(Segment::new(trash::TRASH_DIR.to_vec()).unwrap());
    let ids = child_names(&p, &trash_dir).await;
    assert_eq!(ids.len(), 1, "una entrada de papelera");
    let entry = trash_dir.join(Segment::new(ids[0].clone()).unwrap());

    // Contiene el payload + `.norte-info`.
    let mut names = child_names(&p, &entry).await;
    names.sort();
    let mut expected = vec![b".norte-info".to_vec(), b"victim.txt".to_vec()];
    expected.sort();
    assert_eq!(names, expected);

    // El payload conserva el contenido.
    let payload = entry.join(Segment::new(b"victim.txt".to_vec()).unwrap());
    assert_eq!(read_all(&p, &payload).await, b"contenido");

    // El `.norte-info` decodifica a la ruta original (anclado a la conexión).
    let info_path = entry.join(Segment::new(trash::INFO_NAME.to_vec()).unwrap());
    let info_bytes = read_all(&p, &info_path).await;
    let info = trash::info_decode(&info_bytes, &root()).expect("decode");
    assert_eq!(info.original, victim);
}

#[tokio::test]
async fn trash_without_capability_is_unsupported() {
    let p = fresh(false).await;
    let victim = root().join(Segment::new(b"x.txt".to_vec()).unwrap());
    let mut sink = p.write(&victim).await.expect("write");
    sink.write(Bytes::from_static(b"y")).await.expect("chunk");
    sink.commit().await.expect("commit");

    assert!(matches!(
        p.trash(&victim).await,
        Err(norte_proto::Error::Unsupported)
    ));
    // El origen sigue ahí (no se degradó a permanente).
    assert!(p.stat(&victim).await.is_ok());
}

#[tokio::test]
async fn trash_preserves_hostile_basename() {
    let p = fresh(true).await;
    // sftp (russh-sftp) usa paths String → NO representa bytes no-UTF8
    // (rechaza con InvalidPath, issue #37); el nombre hostil que SÍ maneja
    // es UTF-8 retorcido: espacios, unicode, emoji, punto inicial.
    let hostile = "año 名前 😀 .txt".as_bytes().to_vec();
    let victim = root().join(Segment::new(hostile.clone()).unwrap());

    let mut sink = p.write(&victim).await.expect("write");
    sink.write(Bytes::from_static(b"z")).await.expect("chunk");
    sink.commit().await.expect("commit");

    p.trash(&victim).await.expect("trash");
    assert!(matches!(
        p.stat(&victim).await,
        Err(norte_proto::Error::NotFound)
    ));

    // El payload dentro de la papelera conserva los bytes hostiles.
    let trash_dir = root().join(Segment::new(trash::TRASH_DIR.to_vec()).unwrap());
    let ids = child_names(&p, &trash_dir).await;
    let entry = trash_dir.join(Segment::new(ids[0].clone()).unwrap());
    let names = child_names(&p, &entry).await;
    assert!(
        names.iter().any(|n| n == &hostile),
        "basename hostil preservado"
    );
}
