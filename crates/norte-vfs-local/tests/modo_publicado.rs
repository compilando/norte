//! El modo del fichero que se PUBLICA (#299).
//!
//! El staging estable nace `0o600` y no puede nacer de otra forma: su nombre
//! es predecible, así que mientras dure tiene que ser nuestro y de nadie más
//! (#297, #298). Pero publicar es un `rename`, que no toca el modo, así que
//! una copia REANUDADA acababa en `0o600` mientras la misma copia sin cortes
//! acababa en `0o644`. Misma operación, dos resultados — y el reanudable es el
//! camino de una hoja desde #219, o sea el más común del producto.
//!
//! Las aserciones son por IGUALDAD entre los dos caminos y no contra un modo
//! escrito a mano: el modo correcto depende de la umask de quien corre el
//! test, y fijar `0o644` haría rojo un CI con otra umask por un motivo que no
//! es el bug.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;

use bytes::Bytes;
use norte_proto::{Segment, VPath};
use norte_vfs::Provider;
use norte_vfs_local::LocalProvider;

fn seg(b: &[u8]) -> Segment {
    Segment::new(b.to_vec()).expect("segmento válido")
}

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(seg(name))
}

fn provider() -> (LocalProvider, VPath, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_path_buf();
    let p = LocalProvider::rooted(base.clone()).with_guard(Box::new(dir));
    (p, LocalProvider::root(), base)
}

fn modo(path: &std::path::Path) -> u32 {
    std::fs::metadata(path)
        .expect("existe")
        .permissions()
        .mode()
        & 0o777
}

/// **El bug.** Copia normal y copia reanudada publican el MISMO fichero con el
/// mismo contenido; tienen que publicarlo con el mismo modo.
#[tokio::test]
async fn una_copia_reanudada_se_publica_con_el_modo_de_una_normal() {
    let (p, root, base) = provider();

    let normal = child(&root, b"normal.bin");
    let mut sink = p.write(&normal).await.expect("write");
    sink.write(Bytes::from_static(b"contenido"))
        .await
        .expect("bytes");
    sink.commit().await.expect("commit");

    let reanudada = child(&root, b"reanudada.bin");
    let (mut sink, ya) = p.open_resumable(&reanudada).await.expect("open_resumable");
    assert_eq!(ya, 0, "no había parcial previo");
    sink.write(Bytes::from_static(b"contenido"))
        .await
        .expect("bytes");
    sink.commit().await.expect("commit");

    assert_eq!(
        modo(&base.join("reanudada.bin")),
        modo(&base.join("normal.bin")),
        "misma operación, mismo modo: sin esto la reanudada queda en 0o600"
    );
}

/// La reanudación DE VERDAD —dos sesiones sobre el mismo staging— publica
/// igual. El caso de arriba abre el staging y lo publica de una; este lo deja
/// a medias, lo reencuentra y lo termina, que es lo que hace una copia que se
/// corta.
#[tokio::test]
async fn una_reanudacion_en_dos_tramos_tambien_publica_con_el_modo_normal() {
    let (p, root, base) = provider();

    let normal = child(&root, b"normal.bin");
    let mut sink = p.write(&normal).await.expect("write");
    sink.write(Bytes::from_static(b"unodos"))
        .await
        .expect("bytes");
    sink.commit().await.expect("commit");

    let destino = child(&root, b"grande.bin");
    let (mut sink, _) = p.open_resumable(&destino).await.expect("primer tramo");
    sink.write(Bytes::from_static(b"uno")).await.expect("bytes");
    // `keep` conserva el staging para el resume siguiente (ADR 0012).
    sink.keep().await.expect("conserva");

    let (mut sink, ya) = p.open_resumable(&destino).await.expect("segundo tramo");
    assert_eq!(ya, 3, "reencuentra los bytes del primer tramo");
    sink.write(Bytes::from_static(b"dos")).await.expect("bytes");
    sink.commit().await.expect("commit");

    assert_eq!(
        std::fs::read(base.join("grande.bin")).expect("leer"),
        b"unodos",
        "y los bytes son los dos tramos"
    );
    assert_eq!(
        modo(&base.join("grande.bin")),
        modo(&base.join("normal.bin")),
    );
}

/// **Mientras dura, el staging sigue siendo NUESTRO.** El arreglo no puede
/// consistir en crearlo más abierto: su nombre es predecible, así que un
/// `0o644` durante la copia deja que cualquiera lea lo que se está copiando —y
/// un fichero a medias, además. El modo se repone DESPUÉS de publicar.
#[tokio::test]
async fn el_staging_a_medias_no_se_relaja() {
    let (p, root, base) = provider();
    let destino = child(&root, b"grande.bin");

    let (mut sink, _) = p.open_resumable(&destino).await.expect("open_resumable");
    sink.write(Bytes::from_static(b"a medias"))
        .await
        .expect("bytes");
    sink.keep().await.expect("conserva");

    let staging = std::fs::read_dir(&base)
        .expect("listar")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(".norte-partial."))
        })
        .expect("el staging conservado");

    assert_eq!(
        modo(&staging),
        0o600,
        "el parcial es nuestro y de nadie más"
    );
}

/// El camino CONFINADO (#297) es el que usa una hoja desde #219, así que es el
/// que de verdad ve el usuario. Misma promesa.
#[tokio::test]
async fn el_camino_confinado_publica_con_el_mismo_modo() {
    let (p, root, base) = provider();
    std::fs::create_dir(base.join("dest")).expect("dest");
    let raiz = child(&root, b"dest");

    let croot = p.open_root(&raiz).await.expect("raíz confinada");
    let mut sink = croot.write(&[seg(b"normal.bin")]).await.expect("write");
    sink.write(Bytes::from_static(b"contenido"))
        .await
        .expect("bytes");
    sink.commit().await.expect("commit");

    let (mut sink, _) = croot
        .open_resumable(&[seg(b"reanudada.bin")])
        .await
        .expect("open_resumable confinado");
    sink.write(Bytes::from_static(b"contenido"))
        .await
        .expect("bytes");
    sink.commit().await.expect("commit");

    assert_eq!(
        modo(&base.join("dest/reanudada.bin")),
        modo(&base.join("dest/normal.bin")),
    );
}
