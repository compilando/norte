//! Fidelidad de bytes del sidecar de la papelera lógica (ADR 0019 D2) ante
//! nombres NO-UTF8. Va SOLO sobre `MemProvider` (byte-exacto): sftp/object
//! rechazan nombres no-UTF8 en la frontera —russh-sftp y las keys S3 exigen
//! UTF-8— así que este invariante no puede vivir en el `logical_trash_contract!`
//! compartido (allí sftp lo haría fallar). El corpus hostil canónico entra
//! aquí como exige la regla del proyecto (fixture de encoding ANTES/junto al
//! camino que la ejercita).

use bytes::Bytes;
use futures::StreamExt;
use norte_testkit::MemProvider;
use norte_vfs::proto::{Segment, VPath};
use norte_vfs::Provider;

/// Mismo instante fijo que el contrato → `<id>` determinista.
const NOW_MS: u64 = 0x0123_4567_89ab;

fn seg(bytes: &[u8]) -> Segment {
    Segment::new(bytes.to_vec()).expect("segmento válido")
}

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(seg(name))
}

fn trash_id() -> Vec<u8> {
    format!("{NOW_MS:016x}").into_bytes()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex par"))
        .collect()
}

async fn write_all(p: &MemProvider, path: &VPath, content: &[u8]) {
    let mut sink = p.write(path).await.expect("write abre");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk entra");
    sink.commit().await.expect("commit publica");
}

async fn read_all(p: &MemProvider, path: &VPath) -> Vec<u8> {
    let mut stream = p.read(path, None).await.expect("read abre");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk sin error"));
    }
    out
}

/// El sidecar preserva un nombre de origen no-UTF8 y reconstruye el `VPath`
/// BYTE A BYTE. Recorre el corpus hostil canónico: Latin-1, NFD y bytes crudos
/// no-decodificables en una sola pasada.
#[tokio::test]
async fn trash_sidecar_roundtrips_hostile_origins() {
    // Nombres del corpus que son un Segment legal (excluye los que llevan `/`,
    // nul o son `.`/`..`): interesa la fidelidad de bytes, no la validación.
    let hostiles: Vec<Vec<u8>> = norte_testkit::corpus::hostile_names()
        .into_iter()
        .map(|n| n.bytes)
        .filter(|b| Segment::new(b.clone()).is_ok())
        // Al menos un nombre NO-UTF8 debe entrar, o el test no probaría nada.
        .collect();
    assert!(
        hostiles.iter().any(|b| std::str::from_utf8(b).is_err()),
        "el corpus debe traer algún nombre no-UTF8"
    );

    for bytes in hostiles {
        let p = MemProvider::new();
        let root = MemProvider::root();
        let victima = child(&root, &bytes);
        write_all(&p, &victima, b"contenido").await;

        norte_vfs::logical_trash(&p, &victima, NOW_MS)
            .await
            .expect("trash lógico");

        // El dato sobrevive bajo files/<id> (nombre = timestamp, no el original).
        let mut id = trash_id();
        let dest = child(&child(&child(&root, b".norte-trash"), b"files"), &id);
        assert_eq!(
            read_all(&p, &dest).await,
            b"contenido",
            "dato perdido para {bytes:?}"
        );

        // El sidecar es ASCII pese al nombre no-UTF8, y su orig_hex revierte al
        // VPath original byte-a-byte.
        id.extend_from_slice(b".json");
        let meta_path = child(&child(&child(&root, b".norte-trash"), b"meta"), &id);
        let meta = String::from_utf8(read_all(&p, &meta_path).await)
            .expect("sidecar ASCII pese a origen no-UTF8");

        let expected_hex = hex(victima.to_wire().as_bytes());
        assert!(
            meta.contains(&format!(r#""orig_hex":"{expected_hex}""#)),
            "sidecar sin orig_hex correcto para {bytes:?}: {meta}"
        );
        let wire = String::from_utf8(unhex(&expected_hex)).expect("wire UTF-8");
        assert_eq!(
            VPath::parse(&wire).expect("wire reparsea"),
            victima,
            "el round-trip hex→parse no reconstruye el VPath para {bytes:?}"
        );
    }
}
