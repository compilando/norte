//! Contención de un servidor SFTP HOSTIL (ADR 0013, threat model §14): un
//! servidor que inyecta nombres trampa (`../../`) y symlinks fuera de la
//! base no puede hacer que el provider escape la raíz ni corrompa.

mod common;

use bytes::Bytes;
use futures::StreamExt;
use norte_proto::{Authority, ConflictKind, EntryKind, Error, VPath};
use norte_vfs::{FollowLinks, Provider};
use norte_vfs_sftp::SftpProvider;

fn vp(p: &str) -> VPath {
    VPath::parse(&format!("sftp://test:22{p}")).expect("wire válido")
}

async fn provider(base: &std::path::Path, mode: common::Mode) -> SftpProvider {
    let session = common::connect(base, mode).await;
    SftpProvider::new(session, "/")
}

/// Un servidor que inyecta `../../escape` en cada `readdir`: el provider
/// RECHAZA la entrada con `/` (jamás la reconstruye como hijo) y corta el
/// listado — el resto de entradas legítimas no se sirven a ciegas.
#[tokio::test]
async fn readdir_con_nombre_trampa_se_rechaza() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("legit.txt"), b"ok").unwrap();
    let p = provider(dir.path(), common::Mode::Hostile).await;

    let mut stream = p.list(&vp("/")).await.expect("list abre");
    let mut vio_error = false;
    while let Some(item) = stream.next().await {
        match item {
            Ok(entry) => {
                // Ninguna entrada servida contiene `/` ni `..` en su nombre.
                let name = entry.path.file_name().expect("con nombre");
                assert!(
                    !name.as_bytes().contains(&b'/'),
                    "una entrada con `/` escaparía la base"
                );
                assert_ne!(name.as_bytes(), b"..");
            }
            Err(Error::InvalidPath) => {
                // El nombre trampa `../../escape` se rechaza: contención OK.
                vio_error = true;
            }
            Err(other) => panic!("error inesperado: {other:?}"),
        }
    }
    assert!(
        vio_error,
        "el servidor hostil inyectó `../../escape` y debió rechazarse"
    );
}

/// El provider construye los paths SIEMPRE desde segmentos validados: aunque
/// el servidor mienta en un listado, un `stat` posterior usa el path que
/// arma el cliente, jamás uno ecoado — no hay escape.
#[tokio::test]
async fn stat_usa_path_propio_no_el_ecoado() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("f.txt"), b"data").unwrap();
    let p = provider(dir.path(), common::Mode::Hostile).await;

    // Stat de un hijo legítimo funciona (el path lo arma el cliente).
    let e = p.stat(&vp("/f.txt")).await.expect("stat del hijo legítimo");
    assert_eq!(e.kind, EntryKind::File);
    // Un VPath jamás admite `..` como segmento, así que el cliente no puede
    // pedir `sftp://test:22/../etc` — no hay forma de construirlo.
    assert!(VPath::parse("sftp://test:22/../etc").is_err());
}

/// Un symlink trampa que apunta FUERA de la base (`/etc/passwd`) se ve como
/// symlink (lstat, jamás seguido); `read_link` da los bytes crudos del
/// target sin resolverlo, y como `node_id` es `None`, el `Follow` del engine
/// no puede recorrerlo.
#[tokio::test]
async fn symlink_trampa_no_se_sigue() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::os::unix::fs::symlink("/etc/passwd", dir.path().join("trampa")).unwrap();
    let p = provider(dir.path(), common::Mode::Honest).await;

    let e = p.stat(&vp("/trampa")).await.expect("stat del symlink");
    assert_eq!(e.kind, EntryKind::Symlink, "se ve como LINK, no se sigue");
    // read_link da el target CRUDO, sin resolver.
    let target = p.read_link(&vp("/trampa")).await.expect("read_link");
    assert_eq!(target, b"/etc/passwd");
    // node_id es None → el engine no puede seguir dir-symlinks (contención).
    assert_eq!(
        p.node_id(&vp("/trampa"), FollowLinks::No).await.unwrap(),
        None
    );
    // Y JAMÁS se leyó el contenido de /etc/passwd por el provider.
}

/// El provider nunca sale de su base: los paths se componen bajo ella y el
/// servidor de test rebasa todo dentro del tempdir. Escribir crea el archivo
/// DENTRO, no en el FS del test runner.
#[tokio::test]
async fn escritura_queda_dentro_de_la_base() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = provider(dir.path(), common::Mode::Honest).await;

    let mut sink = p.write(&vp("/nuevo.bin")).await.expect("write abre");
    sink.write(Bytes::from_static(b"contenido")).await.unwrap();
    sink.commit().await.expect("commit");
    // El archivo aparece DENTRO del tempdir, en ningún otro sitio.
    assert_eq!(
        std::fs::read(dir.path().join("nuevo.bin")).unwrap(),
        b"contenido"
    );
}

/// Un nombre no-UTF8 se rechaza LIMPIO con `InvalidPath` (limitación de
/// russh-sftp = String; ADR 0013 D2), jamás lossy.
#[tokio::test]
async fn nombre_no_utf8_se_rechaza_limpio() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = provider(dir.path(), common::Mode::Honest).await;
    let seg = norte_proto::Segment::new(vec![0xFF, 0xFE]).unwrap();
    let hostil = SftpProvider::root(Authority::new("test:22").unwrap()).join(seg);
    // Cualquier operación con un nombre no-UTF8 rechaza limpio.
    assert_eq!(p.stat(&hostil).await.unwrap_err(), Error::InvalidPath);
    assert!(p.write(&hostil).await.is_err());
}
/// Resume sobre sftp (ADR 0012): `keep` conserva el `.norte-partial`, una
/// segunda apertura reanuda desde el offset y el commit concatena. (La
/// cancelación limpia del sink la cubre `contract_abort_leaves_no_trace` de
/// la macro de contrato, que corre contra este mismo provider.)
#[tokio::test]
async fn resume_sobre_sftp_reanuda() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = provider(dir.path(), common::Mode::Honest).await;

    // Primer tramo: open_resumable, escribe "hola", keep (conserva).
    let (mut sink, already) = p.open_resumable(&vp("/big.bin")).await.expect("open 1");
    assert_eq!(already, 0);
    sink.write(Bytes::from_static(b"hola")).await.unwrap();
    sink.keep().await.expect("keep");
    // El destino final no existe todavía.
    assert_eq!(p.stat(&vp("/big.bin")).await.unwrap_err(), Error::NotFound);

    // Segundo tramo: reanuda desde 4 bytes.
    let (mut sink, already) = p.open_resumable(&vp("/big.bin")).await.expect("open 2");
    assert_eq!(already, 4, "reanuda tras lo conservado");
    sink.write(Bytes::from_static(b"mundo")).await.unwrap();
    sink.commit().await.expect("commit");
    // El contenido es la concatenación.
    let mut stream = p.read(&vp("/big.bin"), None).await.expect("read");
    let mut out = Vec::new();
    while let Some(c) = stream.next().await {
        out.extend_from_slice(&c.unwrap());
    }
    assert_eq!(out, b"holamundo");
}

/// H1 (write): el staging efímero tiene nombre PREDECIBLE
/// (`.norte-partial.eph.0`). Un servidor/co-tenant hostil lo pre-planta como
/// symlink a un fichero FUERA de la base; `write()` abre con `EXCLUDE`
/// (create-new atómico) → FALLA sin seguir el symlink. El víctima queda intacto
/// (contención de escritura — ADR 0013, threat model §14).
#[tokio::test]
async fn staging_symlink_pre_plantado_no_se_sigue_en_write() {
    let dir = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("tempdir víctima");
    let victim = outside.path().join("victima");
    std::fs::write(&victim, b"intacto").unwrap();
    // El hostil pre-planta el staging predecible como symlink al víctima.
    std::os::unix::fs::symlink(&victim, dir.path().join(".norte-partial.eph.0")).unwrap();

    let p = provider(dir.path(), common::Mode::Honest).await;
    // El open con EXCLUDE falla ante el path existente → write() da error.
    assert!(
        p.write(&vp("/dest")).await.is_err(),
        "abrir un staging pre-plantado (symlink) debe fallar, no seguirlo"
    );
    assert_eq!(
        std::fs::read(&victim).unwrap(),
        b"intacto",
        "jamás se escribió a través del symlink fuera de la base"
    );
}

/// H1 (resume): `open_resumable` no puede usar `EXCLUDE` (reabre un parcial
/// legítimo), así que hace `lstat` y RECHAZA si el staging existente es un
/// symlink. Si no, reanudar en APPEND escribiría en el target fuera de base.
#[tokio::test]
async fn staging_symlink_pre_plantado_no_se_reanuda() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let p = provider(dir.path(), common::Mode::Honest).await;

    // Primera apertura: crea el staging (fichero regular), escribe, keep.
    let (mut sink, _) = p.open_resumable(&vp("/big.bin")).await.expect("open 1");
    sink.write(Bytes::from_static(b"hola")).await.unwrap();
    sink.keep().await.expect("keep");

    // Descubre el staging A TRAVÉS del provider: `list` es un round-trip
    // in-order que garantiza que las escrituras de sink1 ya se procesaron
    // server-side antes de tocar el tempdir por fuera (si no, un WRITE tardío
    // de sink1 seguiría el symlink que plantamos — artefacto del test, no del
    // provider). El nombre del staging es determinista pero interno; lo tomamos
    // del listado en vez de hardcodearlo.
    let mut stream = p.list(&vp("/")).await.expect("list");
    let mut staging_name = None;
    while let Some(item) = stream.next().await {
        let entry = item.expect("entrada válida");
        let name = entry
            .path
            .file_name()
            .expect("con nombre")
            .as_bytes()
            .to_vec();
        if name.starts_with(b".norte-partial.") {
            staging_name = Some(name);
        }
    }
    let staging_name = staging_name.expect("el staging conservado se lista");
    let staging = dir.path().join(std::ffi::OsStr::from_bytes(&staging_name));

    // El hostil sustituye el staging por un symlink fuera de base.
    let outside = tempfile::tempdir().expect("tempdir víctima");
    let victim = outside.path().join("victima");
    std::fs::write(&victim, b"intacto").unwrap();
    std::fs::remove_file(&staging).unwrap();
    std::os::unix::fs::symlink(&victim, &staging).unwrap();

    // Segunda apertura: el staging es ahora un symlink → se rechaza.
    let rechazo = p.open_resumable(&vp("/big.bin")).await;
    assert!(
        matches!(
            rechazo,
            Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch
            })
        ),
        "reanudar sobre un staging que es symlink debe rechazarse"
    );
    assert_eq!(
        std::fs::read(&victim).unwrap(),
        b"intacto",
        "jamás se añadió a través del symlink fuera de la base"
    );
}

/// Hallazgo A (encoding): russh-sftp decodifica los nombres del servidor con
/// `from_utf8_lossy`, así que un nombre no-UTF8 llega sustituido por U+FFFD y
/// los bytes originales se pierden BAJO la frontera. El provider lo RECHAZA
/// limpio (`InvalidPath`) en vez de emitir un `Entry` con bytes corruptos que
/// colisionaría o apuntaría a un fichero inexistente (regla 1 / ADR 0013 D2).
#[tokio::test]
async fn readdir_nombre_no_utf8_no_se_corrompe() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().expect("tempdir");
    // `café` en Latin-1: byte 0xE9 crudo — imposible de crear vía el provider.
    let raw = std::ffi::OsStr::from_bytes(b"caf\xE9.txt");
    std::fs::write(dir.path().join(raw), b"x").unwrap();
    let p = provider(dir.path(), common::Mode::Honest).await;

    let mut stream = p.list(&vp("/")).await.expect("list abre");
    let mut rechazado = false;
    while let Some(item) = stream.next().await {
        match item {
            Ok(entry) => {
                let name = entry.path.file_name().expect("con nombre");
                assert!(
                    !name.as_bytes().windows(3).any(|w| w == [0xEF, 0xBF, 0xBD]),
                    "un nombre no-UTF8 se emitió corrupto (U+FFFD) en silencio"
                );
            }
            Err(Error::InvalidPath) => rechazado = true,
            Err(other) => panic!("error inesperado: {other:?}"),
        }
    }
    assert!(
        rechazado,
        "el nombre no-UTF8 debió rechazarse limpio (InvalidPath), jamás corromperse"
    );
}
