//! `provider_contract!` sobre `services-fs` de opendal (ADR 0016 J): la MISMA
//! suite que pasan Mem/Local/Sftp/Ftp, contra la lógica completa del provider
//! (validación de keys, modelo de dirs, sink, mapeo de errores) sin HTTP. Lo
//! S3-específico que este harness no ejercita (multipart, conditional write,
//! delimiter real) vive en tests/s3.rs contra s3s-fs, y el nightly (`reals3`)
//! valida contra `MinIO`.
//!
//! Solo-Linux (como sftp/ftp): el harness se respalda en el FS del host y
//! solo es fiel en POSIX (case-sensitive, byte-preserving)… con una
//! asimetría CONSCIENTE: las keys S3 son UTF-8-only, así que las fixtures
//! no-UTF8 del corpus se rechazan limpio en el provider (skip del contrato),
//! no llegan al FS.
#![cfg(target_os = "linux")]

mod common;

use norte_proto::Authority;
use norte_vfs_object::ObjectProvider;

/// Provider fresco sobre un tempdir vía `services-fs` (con `atomic_write_dir`
/// FUERA de la raíz listable: un `.tmp` del writer no es una entrada).
fn fresh() -> ObjectProvider {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("root");
    let atomic = dir.path().join("staging");
    std::fs::create_dir_all(&root).expect("root");
    std::fs::create_dir_all(&atomic).expect("staging");
    let op = common::fs_operator(&root, &atomic);
    // El tempdir vive tanto como el provider (tests efímeros; el SO limpia /tmp).
    std::mem::forget(dir);
    ObjectProvider::new(op, "s3")
}

fn hostile_names() -> Vec<Vec<u8>> {
    norte_testkit::corpus::hostile_names()
        .into_iter()
        .map(|n| n.bytes)
        // Los fixtures name_max (255/256 bytes) son keys S3 LEGALES (límite
        // 1024, sin tope por segmento) que este harness de FS no puede
        // almacenar: NAME_MAX POSIX = 255 y el atomic_write_dir de opendal
        // añade ".XXXXXXXX" (9 bytes) al tempfile → tope efectivo 246.
        // Limitación del harness, no del provider — el fs revienta DESPUÉS
        // del open y el contrato exige rechazo limpio o éxito. Los cubre el
        // nightly contra `MinIO` real (tests/reals3.rs).
        .filter(|bytes| bytes.len() <= 246)
        .collect()
}

norte_vfs::provider_contract! {
    mod object_fs,
    factory: fresh(),
    root: ObjectProvider::root("s3", Authority::new("norte-test").expect("authority válida")),
    hostile_names: hostile_names(),
}

/// El mismo provider con la papelera lógica ENCENDIDA (ADR 0019).
fn fresh_con_papelera() -> ObjectProvider {
    fresh().with_logical_trash(true)
}

// La suite entera, otra vez, con la papelera lógica puesta (#168).
//
// No es duplicación: es la ÚNICA configuración en la que corre la rama del
// contrato que dice «el destino existe y se restaura» — la que exige que
// `trash()` nombre lo que entierra, que `reversal_ref` sea `Some`, y que
// `restore_from` devuelva el nodo exacto con sus bytes y su nombre, nombres
// no-UTF8 incluidos.
//
// Esta pasada existe porque su ausencia ya costó un fallo real: este provider
// devolvía `Some` de `trash()` y nunca sobreescribió `trash_restorable()`,
// que por defecto es `false`. Una sincronización contra S3 con la papelera
// encendida se habría planificado ENTERA como irreversible —cada paso, las
// copias incluidas— tirando su `reversal_ref`, mientras la papelera era
// perfectamente restaurable. El plan le habría dicho al humano «nada de esto
// se puede deshacer» y luego habría enterrado cosas en un sitio que sabía
// alcanzar.
norte_vfs::provider_contract! {
    mod object_fs_papelera,
    factory: fresh_con_papelera(),
    root: ObjectProvider::root("s3", Authority::new("norte-test").expect("authority válida")),
    hostile_names: hostile_names(),
}

// ---------- attrs s3 (#108 bloque 2) ----------

#[tokio::test]
async fn attrs_s3_etag_y_content_type() {
    use futures::StreamExt;
    use norte_proto::{AttrValue, Segment};
    use norte_vfs::{AttrRequest, ListOptions, Provider};

    let p = fresh();
    let root = ObjectProvider::root(
        "s3",
        Authority::new("norte-test").expect("authority válida"),
    );
    let f = root.join(Segment::new(b"o.txt".to_vec()).expect("segmento válido"));
    {
        let mut sink = p.write(&f).await.expect("write abre");
        norte_vfs::ByteSink::write(&mut *sink, bytes::Bytes::from_static(b"x"))
            .await
            .expect("chunk entra");
        sink.commit().await.expect("commit publica");
    }
    let opt = ListOptions {
        attrs: AttrRequest::sanitized(["s3.etag", "s3.content_type"].map(str::to_owned)),
    };
    // services-fs puede no dar etag/content_type: si están, son Text acotado
    // (la forma la pinea el contrato; el valor REAL lo cubre el nightly MinIO).
    let e = p.stat_with(&f, &opt).await.expect("stat_with");
    for id in ["s3.etag", "s3.content_type"] {
        if let Some(v) = e.attrs.get(id) {
            let AttrValue::Text(s) = v else {
                panic!("{id} debe ser Text, fue {v:?}");
            };
            assert!(s.len() <= norte_proto::ATTR_TEXT_MAX);
        }
    }
    // list_with: mismas reglas por entrada, y jamás un id no pedido.
    let mut s = p.list_with(&root, &opt).await.expect("list_with");
    while let Some(e) = s.next().await {
        let e = e.expect("entrada");
        for id in e.attrs.keys() {
            assert!(opt.attrs.wants(id), "id no pedido: {id}");
        }
    }
    // Sin pedir → nada.
    assert!(p.stat(&f).await.expect("stat").attrs.is_empty());
}
