//! `readonly_provider_contract!` sobre un `.rar` forjado con [`RarSmith`] y
//! leído por el delegado que haya instalado.
//!
//! Esta suite NECESITA `7z` o `unrar` en la máquina: sin ninguno de los dos no
//! hay forma de leer un RAR y no hay nada que contrastar. Falla diciéndolo en
//! vez de pasar en verde sin haber probado nada.

use std::sync::OnceLock;

use norte_proto::{Scheme, Segment, VPath};
use norte_testkit::RarSmith;
use norte_vfs_rar::{Delegate, RarLimits, RarProvider};

/// Nombres del corpus que un listado POR LÍNEAS no puede llevar de vuelta.
///
/// No es una debilidad del índice: la salida del delegado es texto por líneas
/// y un nombre con `\n` (o un `\r` que un `\n` acompañe) no se puede
/// reconstruir sin adivinar dónde acaba. La regla del provider es saltarlos y
/// contarlos, así que aquí se excluyen del contrato — que exige round-trip
/// byte-exacto — y `listing::tests::un_nombre_con_salto_de_linea_se_salta_y_se_cuenta`
/// pinea la frontera.
///
/// `archive_marker_literal` (`!`) se excluye por la misma razón que en la
/// suite de zip/tar: es el marcador de ADR 0018, indireccionable por diseño.
fn no_representable(id: &str, bytes: &[u8]) -> bool {
    id == "archive_marker_literal" || bytes.contains(&b'\n') || bytes.contains(&b'\r')
}

/// Nombres hostiles que el `.rar` de la fixture SÍ trae.
fn hostile_names() -> Vec<Vec<u8>> {
    norte_testkit::corpus::hostile_names()
        .into_iter()
        .filter(|n| !no_representable(&n.id, &n.bytes))
        .map(|n| n.bytes)
        .collect()
}

/// El árbol canónico que exige la macro RO, forjado como RAR5.
fn canonical_rar() -> Vec<u8> {
    let mut smith = RarSmith::new()
        .dir(b"docs")
        .file(b"docs/hello.txt", b"hola norte\n")
        .dir(b"docs/sub")
        .file(b"docs/sub/nested.bin", b"\x00\x01\x02\xff")
        .file(b"vacio.txt", b"")
        .dir(b"hostile");
    for name in hostile_names() {
        let mut full = b"hostile/".to_vec();
        full.extend_from_slice(&name);
        smith = smith.file(&full, &name);
    }
    smith.build()
}

/// El `.rar` de la fixture, escrito UNA vez por proceso: el delegado necesita
/// una ruta de verdad, así que el temporal tiene que sobrevivir a los tests.
fn fixture() -> &'static std::path::Path {
    static FIXTURE: OnceLock<(tempfile::TempDir, std::path::PathBuf)> = OnceLock::new();
    let (_dir, path) = FIXTURE.get_or_init(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("contrato.rar");
        std::fs::write(&path, canonical_rar()).expect("escribir la fixture");
        (dir, path)
    });
    path
}

fn fresh() -> RarProvider {
    let delegate = Delegate::discover()
        .expect("esta suite necesita `7z` o `unrar` instalado: sin delegado no hay RAR que leer");
    RarProvider::new(fixture().to_path_buf(), delegate, RarLimits::default())
}

fn root() -> VPath {
    use std::os::unix::ffi::OsStrExt;
    let mut outer = VPath::root(Scheme::new("file").expect("scheme"), None);
    for comp in fixture().components().skip(1) {
        outer = outer.join(Segment::new(comp.as_os_str().as_bytes().to_vec()).expect("segmento"));
    }
    VPath::archive_compose("rar", &outer, &[]).expect("compose")
}

norte_vfs::readonly_provider_contract! {
    mod rar_ro,
    factory: fresh(),
    root: root(),
    hostile_names: hostile_names(),
}
