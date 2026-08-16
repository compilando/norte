//! Fase 8f: resolución de schemes compuestos (ADR 0018) en el Engine — un
//! `tar+mem`/`zip+mem` se sirve componiendo un `ArchiveProvider` sobre el
//! provider del contenedor, sin registro previo.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use norte_core::Engine;
use norte_proto::{Error, VPath};
use norte_testkit::{MemProvider, TarSmith, ZipSmith};
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido de test")
}

async fn engine_with_container(name: &str, bytes: &[u8]) -> Engine {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    let mut sink = mem
        .write(&vp(&format!("mem:///{name}")))
        .await
        .expect("write abre");
    sink.write(Bytes::copy_from_slice(bytes))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
    engine.register_provider(mem as Arc<dyn Provider>);
    engine
}

#[tokio::test]
async fn lista_y_lee_dentro_de_un_tar() {
    let tar = TarSmith::new().file(b"docs/x.txt", b"dentro").build();
    let engine = engine_with_container("a.tar", &tar).await;
    let entries: Vec<_> = engine
        .list(&vp("tar+mem:///a.tar/!"))
        .await
        .expect("list raíz interior")
        .map(|e| e.expect("entrada ok"))
        .collect()
        .await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path.to_wire(), "tar+mem:///a.tar/!/docs");
    let e = engine
        .stat(&vp("tar+mem:///a.tar/!/docs/x.txt"))
        .await
        .expect("stat interior");
    assert_eq!(e.size, Some(6));
}

#[tokio::test]
async fn lee_dentro_de_un_zip() {
    let zip = ZipSmith::new().file(b"hola.txt", b"desde el zip").build();
    let engine = engine_with_container("a.zip", &zip).await;
    let mut stream = engine
        .read(&vp("zip+mem:///a.zip/!/hola.txt"), None)
        .await
        .expect("read interior");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk ok"));
    }
    assert_eq!(out, b"desde el zip");
}

/// Gzipea `bytes` en un único miembro gzip (mismo idioma que
/// `norte-vfs-archive/tests/common::gzip`, no reexportado fuera del crate).
fn gzip(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write as _;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(bytes).expect("write gz");
    enc.finish().expect("finish gz")
}

/// #55: `tar+gz` compuesto (`tar+gz+mem://…`) resuelve por el mismo camino
/// del Engine que `tar`/`zip` — wiring del match de `provider_for` (ADR
/// 0028). List + read byte-exacto a través del provider compuesto real.
#[tokio::test]
async fn lista_y_lee_dentro_de_un_targz() {
    let tar = TarSmith::new()
        .file(b"docs/x.txt", b"dentro del tgz")
        .build();
    let tgz = gzip(&tar);
    let engine = engine_with_container("a.tar.gz", &tgz).await;
    let entries: Vec<_> = engine
        .list(&vp("tar+gz+mem:///a.tar.gz/!"))
        .await
        .expect("list raíz interior")
        .map(|e| e.expect("entrada ok"))
        .collect()
        .await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path.to_wire(), "tar+gz+mem:///a.tar.gz/!/docs");
    let mut stream = engine
        .read(&vp("tar+gz+mem:///a.tar.gz/!/docs/x.txt"), None)
        .await
        .expect("read interior");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk ok"));
    }
    assert_eq!(out, b"dentro del tgz");
}

#[tokio::test]
async fn el_arbol_virtual_declara_read_only() {
    let tar = TarSmith::new().file(b"x", b"1").build();
    let engine = engine_with_container("a.tar", &tar).await;
    let caps = engine
        .capabilities(&vp("tar+mem:///a.tar/!"))
        .await
        .expect("capabilities del compuesto");
    assert!(
        caps.flags.contains(norte_proto::CapabilityFlags::READ_ONLY),
        "la UI y el copy engine vetan mutaciones sin round-trip (ADR 0018 E2)"
    );
}

/// #56 (antes: v1 rechazaba con InvalidPath): zip DENTRO de tar navega y
/// lee byte-exacto — el engine compone capa a capa (recursión de
/// `provider_for`) y el interior comprimido del zip se sirve por rangos sobre
/// la capa tar (componible, ADR 0018 A3).
#[tokio::test]
async fn zip_dentro_de_tar_lista_y_lee() {
    use norte_testkit::ZipSmith;
    let zip = ZipSmith::new()
        .file(b"uno.txt", b"contenido interior")
        .build();
    let tar = TarSmith::new().file(b"i.zip", &zip).build();
    let engine = engine_with_container("a.tar", &tar).await;

    let names: Vec<Vec<u8>> = engine
        .list(&vp("zip+tar+mem:///a.tar/!/i.zip/!"))
        .await
        .expect("list anidado")
        .map(|e| {
            e.expect("entry")
                .path
                .segments()
                .last()
                .expect("segmento")
                .to_vec()
        })
        .collect()
        .await;
    assert_eq!(names, vec![b"uno.txt".to_vec()]);

    let mut stream = engine
        .read(&vp("zip+tar+mem:///a.tar/!/i.zip/!/uno.txt"), None)
        .await
        .expect("read anidado");
    let mut got = Vec::new();
    while let Some(chunk) = stream.next().await {
        got.extend_from_slice(&chunk.expect("chunk"));
    }
    assert_eq!(
        got, b"contenido interior",
        "byte-exacto a través de 2 capas"
    );
}

/// #56: el tope de capas (`max_nesting`) corta ANTES de componer — con el
/// tope en 1, un path de dos capas responde `LimitExceeded("nesting")`.
#[tokio::test]
async fn anidamiento_sobre_el_tope_es_limit_exceeded() {
    use norte_testkit::ZipSmith;
    let zip = ZipSmith::new().file(b"uno.txt", b"x").build();
    let tar = TarSmith::new().file(b"i.zip", &zip).build();
    let engine = engine_with_container("a.tar", &tar).await;
    engine.set_archive_limits(norte_core::ArchiveLimits {
        max_nesting: 1,
        ..norte_core::ArchiveLimits::default()
    });
    match engine
        .stat(&vp("zip+tar+mem:///a.tar/!/i.zip/!/uno.txt"))
        .await
    {
        Err(Error::LimitExceeded { limit }) if limit == "nesting" => {}
        other => panic!("esperaba LimitExceeded(nesting), fue {other:?}"),
    }
    // La capa ÚNICA sigue funcionando bajo el mismo tope.
    engine
        .stat(&vp("tar+mem:///a.tar/!/i.zip"))
        .await
        .expect("una capa dentro del tope");
}

#[tokio::test]
async fn compuesto_sin_marcador_es_invalid_path() {
    let engine = engine_with_container("a.tar", b"da igual").await;
    assert_eq!(
        engine.stat(&vp("tar+mem:///a.tar")).await.unwrap_err(),
        Error::InvalidPath
    );
}

#[tokio::test]
async fn contenedor_en_scheme_sin_provider_ni_conector() {
    let engine = Engine::new();
    assert_eq!(
        engine
            .stat(&vp("tar+sftp://host/a.tar/!/x"))
            .await
            .unwrap_err(),
        Error::Unsupported,
        "el interior necesita provider/conector: el error es el de siempre"
    );
}

#[tokio::test]
async fn dos_contenedores_comparten_provider_sin_mezclarse() {
    // El ArchiveProvider se cachea por SCHEME (`tar+mem`), no por
    // contenedor: cada path re-desmonta su exterior y el índice se clava
    // por (wire, mtime, size). Dos tars distintos no pueden mezclarse.
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    for (name, content) in [("a.tar", b"soy A".as_slice()), ("b.tar", b"soy B!")] {
        let tar = TarSmith::new().file(b"quien.txt", content).build();
        let mut sink = mem
            .write(&vp(&format!("mem:///{name}")))
            .await
            .expect("write");
        sink.write(Bytes::copy_from_slice(&tar))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    for (name, content) in [("a.tar", b"soy A".as_slice()), ("b.tar", b"soy B!")] {
        let mut stream = engine
            .read(&vp(&format!("tar+mem:///{name}/!/quien.txt")), None)
            .await
            .expect("read");
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.expect("chunk ok"));
        }
        assert_eq!(out, content, "{name}");
    }
}

#[tokio::test]
async fn copy_hacia_dentro_de_un_archivo_falla_unsupported() {
    let tar = TarSmith::new().file(b"x", b"1").build();
    let engine = engine_with_container("a.tar", &tar).await;
    // Un origen local cualquiera (el propio contenedor sirve).
    let handle = engine
        .copy(&vp("mem:///a.tar"), &vp("tar+mem:///a.tar/!/copia"))
        .await
        .expect("la task arranca; el fallo es del write del destino");
    match handle.join().await {
        norte_proto::TaskState::Failed { error, .. } => {
            assert_eq!(error, Error::Unsupported, "READ_ONLY veta el write");
        }
        other => panic!("esperaba Failed(Unsupported), fue {other:?}"),
    }
}

/// #95.2: `Engine::set_archive_limits` gobierna los providers compuestos —
/// con `max_entries` bajado, un tar de 3 entradas responde
/// `LimitExceeded("entries")` en vez de listarse. Con los defaults, el
/// mismo tar se lista sin drama.
#[tokio::test]
async fn set_archive_limits_gobierna_la_composicion() {
    let tar = TarSmith::new()
        .file(b"uno", b"1")
        .file(b"dos", b"2")
        .file(b"tres", b"3")
        .build();
    let engine = engine_with_container("a.tar", &tar).await;
    engine.set_archive_limits(norte_core::ArchiveLimits {
        max_entries: 1,
        ..norte_core::ArchiveLimits::default()
    });
    match engine.list(&vp("tar+mem:///a.tar/!")).await.map(|_| ()) {
        Err(Error::LimitExceeded { limit }) if limit == "entries" => {}
        other => panic!("esperaba LimitExceeded(entries), fue {other:?}"),
    }

    // Sin tocar límites: el mismo contenedor se lista entero.
    let engine = engine_with_container("a.tar", &tar).await;
    let n = engine
        .list(&vp("tar+mem:///a.tar/!"))
        .await
        .expect("list con defaults")
        .count()
        .await;
    assert_eq!(n, 3);
}

// ---------- rar (roadmap ítem 11): solo sobre un fichero LOCAL ----------

/// El delegado necesita una ruta del sistema de ficheros. Traerse el `.rar`
/// entero desde sftp/s3 sería una descarga que nadie pidió, así que la
/// composición se niega ANTES de ocurrir, con `Unsupported`.
#[tokio::test]
async fn rar_sobre_un_interior_remoto_se_niega_con_motivo() {
    let engine = Engine::new();
    let p = vp("rar+sftp://host/a.rar/!/x.txt");
    assert!(matches!(engine.stat(&p).await, Err(Error::Unsupported)));
}

/// Un `.rar` DENTRO de otro archivo tampoco es un fichero local: no hay ruta
/// que darle al delegado, y la capa exterior no se materializa a un temporal
/// a espaldas de nadie.
#[tokio::test]
async fn rar_anidado_en_otro_archivo_tampoco_es_local() {
    let engine = Engine::new();
    let p = vp("rar+zip+file:///o.zip/!/a.rar/!/x.txt");
    assert!(matches!(engine.stat(&p).await, Err(Error::Unsupported)));
}

/// Un `rar+mem://` es la misma negativa: `mem` es un provider de tests, no un
/// sistema de ficheros, y el arm de dispatch no mira quién está registrado.
#[tokio::test]
async fn rar_sobre_mem_se_niega_aunque_el_provider_este_registrado() {
    let engine = engine_with_container("a.rar", b"Rar!\x1a\x07\x01\x00").await;
    assert!(matches!(
        engine.stat(&vp("rar+mem:///a.rar/!/x.txt")).await,
        Err(Error::Unsupported)
    ));
}

/// Y sobre un fichero local SÍ compone: el engine lista lo que el delegado
/// lee. Sin `7z` ni `unrar` en la máquina, el test se retira diciéndolo.
#[tokio::test]
async fn rar_sobre_un_fichero_local_lista_de_verdad() {
    if norte_testkit::which_7z().is_none() {
        eprintln!("sin 7z instalado: test retirado");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let archive = dir.path().join("a.rar");
    std::fs::write(
        &archive,
        norte_testkit::RarSmith::new()
            .file(b"docs/hello.txt", b"hola norte\n")
            .build(),
    )
    .expect("escribir");
    let engine = Engine::new();
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted("/")) as Arc<dyn Provider>
    );
    let root = VPath::archive_compose(
        "rar",
        &norte_vfs_local::vpath_from_native(&archive).expect("vpath del fichero"),
        &[],
    )
    .expect("compose");
    let entries: Vec<_> = engine
        .list(&root)
        .await
        .expect("list de la raíz del rar")
        .map(|e| e.expect("entrada ok"))
        .collect()
        .await;
    assert_eq!(entries.len(), 1, "el directorio `docs`");
    assert_eq!(entries[0].path.file_name().unwrap().as_bytes(), b"docs");
}
