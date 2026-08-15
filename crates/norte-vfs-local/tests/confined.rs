//! Escrituras que no pueden salirse de su raíz (#164, ADR 0054).
//!
//! El caso que da nombre al issue —un symlink en un componente INTERMEDIO que
//! redirige un `Copy` o un `CreateDir` fuera del destino— y las dos maneras de
//! resolverlo que este provider tiene: `openat2(RESOLVE_BENEATH)` y, donde ese
//! syscall no está, el paseo componente a componente. Las dos se ejercitan en
//! la misma máquina: la segunda con la costura que desactiva la primera.

#![cfg(unix)]

use std::os::unix::ffi::OsStrExt as _;

use bytes::Bytes;
use norte_proto::{ConflictKind, Error, Segment, VPath};
use norte_vfs::Provider;
use norte_vfs_local::LocalProvider;

fn seg(b: &[u8]) -> Segment {
    Segment::new(b.to_vec()).expect("segmento válido")
}

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(seg(name))
}

/// Provider enraizado en un tempdir, con `dest/` dentro y `fuera/` de hermano.
fn escenario() -> (LocalProvider, VPath, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_path_buf();
    let dentro = base.join("dest");
    let fuera = base.join("fuera");
    std::fs::create_dir(&dentro).expect("dest");
    std::fs::create_dir(&fuera).expect("fuera");
    let p = LocalProvider::rooted(base).with_guard(Box::new(dir));
    let raiz = child(&LocalProvider::root(), b"dest");
    (p, raiz, dentro, fuera)
}

async fn escribe(
    root: &dyn norte_vfs::ConfinedRoot,
    rel: &[Segment],
    bytes: &[u8],
) -> Result<(), Error> {
    let mut sink = root.write(rel).await?;
    sink.write(Bytes::copy_from_slice(bytes)).await?;
    sink.commit().await
}

/// EL test de #164: el componente intermedio es un symlink hacia fuera, y la
/// escritura no aterriza allí.
#[tokio::test]
async fn un_symlink_intermedio_no_redirige_la_escritura_fuera_de_la_raiz() {
    let (p, raiz, dentro, fuera) = escenario();
    std::os::unix::fs::symlink(&fuera, dentro.join("sub")).expect("symlink hostil");

    let root = p.open_root(&raiz).await.expect("raíz confinada");
    let err = escribe(root.as_ref(), &[seg(b"sub"), seg(b"botin.txt")], b"x")
        .await
        .expect_err("tiene que negarse");

    assert!(
        matches!(
            err,
            Error::Conflict {
                conflict: ConflictKind::EscapesRoot
            }
        ),
        "respondió {err:?}"
    );
    assert!(
        !fuera.join("botin.txt").exists(),
        "y sobre todo: no escribió fuera"
    );
}

/// `CreateDir` tiene el mismo agujero y la misma respuesta.
#[tokio::test]
async fn un_symlink_intermedio_tampoco_redirige_un_mkdir() {
    let (p, raiz, dentro, fuera) = escenario();
    std::os::unix::fs::symlink(&fuera, dentro.join("sub")).expect("symlink hostil");

    let root = p.open_root(&raiz).await.expect("raíz confinada");
    let err = root
        .mkdir(&[seg(b"sub"), seg(b"nuevo")])
        .await
        .expect_err("tiene que negarse");

    assert!(
        matches!(
            err,
            Error::Conflict {
                conflict: ConflictKind::EscapesRoot
            }
        ),
        "respondió {err:?}"
    );
    assert!(!fuera.join("nuevo").exists(), "no creó fuera");
}

/// Lo que se prohíbe es SALIRSE, no que haya symlinks: uno RELATIVO que apunta
/// a otro sitio DENTRO de la raíz se sigue. Prohibirlo rompería un `dst/data ->
/// almacen` corriente sin ganar seguridad ninguna.
#[tokio::test]
async fn un_symlink_relativo_que_no_sale_de_la_raiz_se_sigue() {
    let (p, raiz, dentro, _fuera) = escenario();
    std::fs::create_dir(dentro.join("real")).expect("real");
    std::os::unix::fs::symlink("real", dentro.join("sub")).expect("symlink interno");

    let root = p.open_root(&raiz).await.expect("raíz confinada");
    escribe(root.as_ref(), &[seg(b"sub"), seg(b"ok.txt")], b"hola")
        .await
        .expect("un symlink interno no es un escape");

    assert_eq!(
        std::fs::read(dentro.join("real/ok.txt")).expect("leer"),
        b"hola"
    );
}

/// Lo corriente sigue funcionando.
#[tokio::test]
async fn una_escritura_anidada_normal_funciona() {
    let (p, raiz, dentro, _fuera) = escenario();
    let root = p.open_root(&raiz).await.expect("raíz confinada");

    root.mkdir(&[seg(b"sub")]).await.expect("mkdir");
    escribe(root.as_ref(), &[seg(b"sub"), seg(b"ok.txt")], b"hola")
        .await
        .expect("write");

    assert_eq!(
        std::fs::read(dentro.join("sub/ok.txt")).expect("leer"),
        b"hola"
    );
    let e = root
        .stat(&[seg(b"sub"), seg(b"ok.txt")])
        .await
        .expect("stat");
    assert_eq!(e.kind, norte_proto::EntryKind::File);
    assert_eq!(e.size, Some(4));
}

/// El paseo de emulación (kernel <5.6, seccomp, macOS) da los MISMOS
/// veredictos que el syscall. Se fuerza con la costura de test, para que las
/// dos ramas se ejerciten en la misma máquina.
#[tokio::test]
async fn el_paseo_de_emulacion_da_los_mismos_veredictos() {
    let _forzado = LocalProvider::force_component_walk_for_test();
    let (p, raiz, dentro, fuera) = escenario();
    std::os::unix::fs::symlink(&fuera, dentro.join("sub")).expect("symlink hostil");
    std::fs::create_dir(dentro.join("real")).expect("real");
    std::os::unix::fs::symlink("real", dentro.join("dentro")).expect("symlink interno");

    let root = p.open_root(&raiz).await.expect("raíz confinada");

    let err = escribe(root.as_ref(), &[seg(b"sub"), seg(b"botin.txt")], b"x")
        .await
        .expect_err("el paseo también se niega");
    assert!(
        matches!(
            err,
            Error::Conflict {
                conflict: ConflictKind::EscapesRoot
            }
        ),
        "respondió {err:?}"
    );
    assert!(!fuera.join("botin.txt").exists());

    escribe(root.as_ref(), &[seg(b"dentro"), seg(b"ok.txt")], b"hola")
        .await
        .expect("y sigue el symlink que no sale");
    assert_eq!(
        std::fs::read(dentro.join("real/ok.txt")).expect("leer"),
        b"hola"
    );
}

/// La PUBLICACIÓN va confinada igual que la escritura: el staging se crea con
/// `openat` en el directorio ya resuelto y se publica con `renameat` en ese
/// mismo descriptor, así que un symlink colado entre medias no manda el rename
/// a otro sitio.
#[tokio::test]
async fn la_publicacion_no_la_desvia_un_symlink_colado_a_medias() {
    let (p, raiz, dentro, fuera) = escenario();
    let root = p.open_root(&raiz).await.expect("raíz confinada");

    let mut sink = root.write(&[seg(b"f.txt")]).await.expect("write");
    sink.write(Bytes::from_static(b"contenido"))
        .await
        .expect("chunk");
    // Carrera: alguien sustituye el destino por un symlink ANTES del commit.
    std::os::unix::fs::symlink(fuera.join("otro.txt"), dentro.join("f.txt"))
        .expect("symlink hostil");
    let err = sink.commit().await.expect_err("el publish no lo pisa");

    assert!(matches!(err, Error::Conflict { .. }), "respondió {err:?}");
    assert!(
        !fuera.join("otro.txt").exists(),
        "y no escribió al otro lado"
    );
}

/// Cancelar deja el destino limpio o un `.norte-partial`, jamás un parcial sin
/// marcar (la promesa de siempre, también por este camino).
#[tokio::test]
async fn un_abort_no_deja_un_parcial_sin_marcar() {
    let (p, raiz, dentro, _fuera) = escenario();
    let root = p.open_root(&raiz).await.expect("raíz confinada");

    let mut sink = root.write(&[seg(b"g.txt")]).await.expect("write");
    sink.write(Bytes::from_static(b"a medias"))
        .await
        .expect("chunk");
    sink.abort().await.expect("abort");

    let restos: Vec<_> = std::fs::read_dir(&dentro)
        .expect("listar")
        .map(|e| e.expect("entrada").file_name())
        .collect();
    assert!(restos.is_empty(), "el abort barre su staging: {restos:?}");
}

/// Una raíz que no existe no se abre.
#[tokio::test]
async fn una_raiz_que_no_existe_no_se_abre() {
    let (p, _raiz, _dentro, _fuera) = escenario();
    // `Box<dyn ConfinedRoot>` no lleva `Debug`, así que `expect_err` no sirve.
    let Err(err) = p
        .open_root(&child(&LocalProvider::root(), b"no-existe"))
        .await
    else {
        panic!("no hay raíz que abrir y aun así se abrió")
    };
    assert_eq!(err, Error::NotFound);
}

/// Y quien la abre lo DECLARA por esa ubicación.
#[tokio::test]
async fn confinar_se_anuncia_en_las_capabilities_de_la_ubicacion() {
    let (p, raiz, _dentro, _fuera) = escenario();
    assert!(
        p.capabilities_at(&raiz)
            .await
            .expect("responde")
            .flags
            .contains(norte_proto::CapabilityFlags::CONFINED_WRITES),
        "en Linux y macOS se confina, y se dice"
    );
    assert!(
        !p.capabilities()
            .flags
            .contains(norte_proto::CapabilityFlags::CONFINED_WRITES),
        "y jamás sin ubicación: depende del mount y del kernel"
    );
}

/// Un symlink ABSOLUTO se rechaza aunque apunte dentro de la raíz: es lo que
/// hace `RESOLVE_BENEATH`, y el paseo de emulación tiene que decir lo mismo o
/// el confinamiento dependería de la versión del kernel.
#[tokio::test]
async fn un_symlink_absoluto_se_rechaza_aunque_apunte_dentro() {
    for forzar_paseo in [false, true] {
        let _forzado = forzar_paseo.then(LocalProvider::force_component_walk_for_test);
        let (p, raiz, dentro, _fuera) = escenario();
        std::fs::create_dir(dentro.join("real")).expect("real");
        std::os::unix::fs::symlink(dentro.join("real"), dentro.join("abs")).expect("symlink abs");

        let root = p.open_root(&raiz).await.expect("raíz confinada");
        let err = escribe(root.as_ref(), &[seg(b"abs"), seg(b"x.txt")], b"x")
            .await
            .expect_err("absoluto se rechaza");

        assert!(
            matches!(
                err,
                Error::Conflict {
                    conflict: ConflictKind::EscapesRoot
                }
            ),
            "paseo={forzar_paseo}: respondió {err:?}"
        );
        assert!(
            !dentro.join("real/x.txt").exists(),
            "paseo={forzar_paseo}: y no escribió"
        );
    }
}

/// Copiar un symlink es CREAR uno en el destino, y esa creación se confina
/// igual que las otras dos: el componente intermedio hostil no se la lleva.
#[tokio::test]
async fn un_symlink_no_se_crea_al_otro_lado_de_un_componente_hostil() {
    let (p, raiz, dentro, fuera) = escenario();
    std::os::unix::fs::symlink(&fuera, dentro.join("sub")).expect("symlink hostil");

    let root = p.open_root(&raiz).await.expect("raíz confinada");
    let err = root
        .symlink(
            &[seg(b"sub"), seg(b"enlace")],
            b"/etc/passwd",
            norte_vfs::SymlinkKind::Unknown,
        )
        .await
        .expect_err("tiene que negarse");

    assert!(
        matches!(
            err,
            Error::Conflict {
                conflict: ConflictKind::EscapesRoot
            }
        ),
        "respondió {err:?}"
    );
    assert!(
        fuera.join("enlace").symlink_metadata().is_err(),
        "no plantó el enlace fuera"
    );
}

/// Y dentro de la raíz se crea tal cual, con los BYTES del target sin tocar:
/// lo confinado es dónde CAE el enlace, no a dónde apunta.
#[tokio::test]
async fn un_symlink_dentro_de_la_raiz_conserva_su_target_crudo() {
    let (p, raiz, dentro, _fuera) = escenario();
    let root = p.open_root(&raiz).await.expect("raíz confinada");

    root.mkdir(&[seg(b"sub")]).await.expect("mkdir");
    root.symlink(
        &[seg(b"sub"), seg(b"enlace")],
        b"../caf\xe9",
        norte_vfs::SymlinkKind::Unknown,
    )
    .await
    .expect("symlink");

    let leido = std::fs::read_link(dentro.join("sub/enlace")).expect("read_link");
    assert_eq!(
        leido.as_os_str().as_bytes(),
        b"../caf\xe9",
        "los bytes del target salen tal cual, sin pasar por UTF-8"
    );

    // Y un nombre ya ocupado es conflicto, no un enlace pisado.
    let err = root
        .symlink(
            &[seg(b"sub"), seg(b"enlace")],
            b"otro",
            norte_vfs::SymlinkKind::Unknown,
        )
        .await
        .expect_err("ocupado");
    assert!(
        matches!(
            err,
            Error::Conflict {
                conflict: ConflictKind::Exists
            }
        ),
        "respondió {err:?}"
    );
}

/// El contrato de `Provider::write` sobre la raíz confinada: un destino ya
/// ocupado se sabe AL ABRIR, no después de haber transferido el fichero.
#[tokio::test]
async fn un_destino_ocupado_es_conflicto_al_abrir_el_sink() {
    let (p, raiz, dentro, _fuera) = escenario();
    std::fs::write(dentro.join("ya.txt"), b"lo que hab\xEDa").expect("ocupante");

    let root = p.open_root(&raiz).await.expect("raíz confinada");
    let err = root
        .write(&[seg(b"ya.txt")])
        .await
        .err()
        .expect("el sink no llega a abrirse");

    assert!(
        matches!(
            err,
            Error::Conflict {
                conflict: ConflictKind::Exists
            }
        ),
        "respondió {err:?}"
    );
    assert_eq!(
        std::fs::read(dentro.join("ya.txt")).expect("leer"),
        b"lo que hab\xEDa",
        "y no tocó lo que había"
    );
}
