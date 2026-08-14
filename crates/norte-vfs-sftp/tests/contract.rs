//! `provider_contract!` sobre un servidor SFTP IN-PROCESS (ADR 0013): la
//! MISMA suite que pasan `MemProvider` y `LocalProvider`, ahora contra un
//! provider REMOTO de verdad, con el corpus de nombres hostiles. Corre en
//! CI normal, sin Docker (el openssh real es un job nightly aparte).
//!
//! Solo-Linux: el servidor in-process mapea las ops sftp sobre el FS del HOST,
//! así que su fidelidad exige un FS POSIX (case-sensitive, byte-preserving).
//! macOS (APFS case-insensitive, rechaza nombres no-UTF8) y Windows (NTFS
//! case-insensitive, sin symlinks POSIX) NO pueden respaldar el harness fiel
//! —darían un "servidor" no representativo—. El provider es OS-agnóstico (Rust
//! puro, sin `cfg`), así que el run de Linux es autoritativo y el nightly
//! openssh valida un servidor POSIX de producción.
#![cfg(target_os = "linux")]

mod common;

use norte_proto::Authority;
use norte_vfs_sftp::SftpProvider;

/// Construye un provider sftp fresco sobre un tempdir + servidor in-process.
/// El bloque es async, así que se envuelve en un runtime propio (la macro
/// `provider_contract!` evalúa `factory` en un test `#[tokio::test]`).
async fn fresh() -> SftpProvider {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_path_buf();
    let session = common::connect(&base, common::Mode::Honest).await;
    // El tempdir debe vivir tanto como el provider (tests efímeros; el SO
    // limpia /tmp).
    std::mem::forget(dir);
    // El servidor mapea `/` al tempdir, así que la base REMOTA del cliente
    // es `/` (los paths se componen /segmento, el servidor los rebasa).
    SftpProvider::new(session, "/")
}

fn hostile_names() -> Vec<Vec<u8>> {
    norte_testkit::corpus::hostile_names()
        .into_iter()
        .map(|n| n.bytes)
        .collect()
}

norte_vfs::provider_contract! {
    mod sftp_inproc,
    factory: fresh().await,
    root: SftpProvider::root(Authority::new("test:22").expect("authority válida")),
    hostile_names: hostile_names(),
}

/// El mismo provider con la papelera lógica ENCENDIDA (ADR 0019).
async fn fresh_con_papelera() -> SftpProvider {
    fresh().await.with_logical_trash(true)
}

// La suite entera, otra vez, con la papelera lógica puesta (#168).
//
// Mismo motivo que en `norte-vfs-object`: es la única configuración en la que
// corre la rama del contrato que dice «el destino existe y se restaura», o
// sea la que comprueba que `trash()` nombra lo que entierra, que
// `reversal_ref` es `Some` y que `restore_from` devuelve el nodo exacto —
// bytes y nombre, nombres no-UTF8 incluidos.
//
// Los overrides de este provider se creían correctos porque se habían LEÍDO.
// El de object estaba en ese mismo estado cuando se escribió el fallo que
// #168 documenta.
norte_vfs::provider_contract! {
    mod sftp_inproc_papelera,
    factory: fresh_con_papelera().await,
    root: SftpProvider::root(Authority::new("test:22").expect("authority válida")),
    hostile_names: hostile_names(),
}

// ---------- attrs posix (#108 bloque 2) ----------

#[tokio::test]
async fn attrs_posix_desde_file_attributes() {
    use futures::StreamExt;
    use norte_proto::{AttrValue, Segment};
    use norte_vfs::{AttrRequest, ListOptions, Provider};

    let p = fresh().await;
    let root = SftpProvider::root(Authority::new("test:22").expect("authority válida"));
    let f = root.join(Segment::new(b"f.txt".to_vec()).expect("segmento válido"));
    {
        let mut sink = p.write(&f).await.expect("write abre");
        norte_vfs::ByteSink::write(&mut *sink, bytes::Bytes::from_static(b"x"))
            .await
            .expect("chunk entra");
        sink.commit().await.expect("commit publica");
    }
    let opt = ListOptions {
        attrs: AttrRequest::sanitized(["posix.mode", "posix.uid", "posix.gid"].map(str::to_owned)),
    };
    let e = p.stat_with(&f, &opt).await.expect("stat_with");
    // El server in-proc sirve el FS del host: mode SIEMPRE presente.
    assert!(
        matches!(e.attrs.get("posix.mode"), Some(AttrValue::Uint(_))),
        "posix.mode presente y Uint: {:?}",
        e.attrs
    );
    for id in ["posix.uid", "posix.gid"] {
        if let Some(v) = e.attrs.get(id) {
            assert!(matches!(v, AttrValue::Uint(_)), "{id} debe ser Uint");
        }
    }

    // list_with lleva lo mismo por entrada.
    let mut s = p.list_with(&root, &opt).await.expect("list_with");
    let le = s.next().await.expect("una entrada").expect("ok");
    assert!(le.attrs.contains_key("posix.mode"));

    // Sin pedir → nada.
    assert!(p.stat(&f).await.expect("stat").attrs.is_empty());
}
