//! Empaquetar, comprobar, partir y juntar (#132), de punta a punta por el
//! engine.
//!
//! El oráculo del empaquetado es el LECTOR de este mismo repositorio: si lo
//! que sale se lista y se lee por el provider de archivos, el archivo es al
//! menos tan bueno como los que norte acepta de fuera.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt as _;
use norte_core::Engine;
use norte_proto::{Segment, TaskState, VPath, methods};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(w: &str) -> VPath {
    VPath::parse(w).expect("wire de test")
}

/// Un engine sobre un `MemProvider` sembrado, y el provider para mirar dentro.
async fn engine_con(ficheros: &[(&str, &[u8])]) -> (Engine, Arc<MemProvider>) {
    let mem = Arc::new(MemProvider::new());
    for (wire, datos) in ficheros {
        let p = vp(wire);
        if let Some(padre) = p.parent()
            && !padre.is_root()
        {
            let _ = mem.mkdir(&padre).await;
        }
        let mut sink = mem.write(&p).await.expect("write");
        sink.write(Bytes::copy_from_slice(datos))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }
    let engine = Engine::new();
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem)
}

/// Lee un fichero entero del provider.
async fn lee(mem: &MemProvider, wire: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut s = mem.read(&vp(wire), None).await.expect("read");
    while let Some(c) = s.next().await {
        out.extend_from_slice(&c.expect("chunk"));
    }
    out
}

/// El árbol interior del archivo, como pares `(nombre relativo, contenido)`.
async fn dentro(engine: &Engine, contenedor: &str, token: &str) -> Vec<(String, Vec<u8>)> {
    let raiz = VPath::archive_compose(token, &vp(contenedor), &[]).expect("compose");
    let mut out = Vec::new();
    let mut pend = vec![raiz.clone()];
    while let Some(dir) = pend.pop() {
        let mut stream = engine.list(&dir).await.expect("list");
        while let Some(e) = stream.next().await {
            let e = e.expect("entrada");
            match e.kind {
                norte_proto::EntryKind::Dir => pend.push(e.path),
                norte_proto::EntryKind::File => {
                    let nombre = String::from_utf8_lossy(
                        &e.path
                            .segments()
                            .skip(raiz.segments().count())
                            .collect::<Vec<_>>()
                            .join(&b'/'),
                    )
                    .into_owned();
                    let mut datos = Vec::new();
                    let mut bs = engine.read(&e.path, None).await.expect("read");
                    while let Some(c) = bs.next().await {
                        datos.extend_from_slice(&c.expect("chunk"));
                    }
                    out.push((nombre, datos));
                }
                _ => {}
            }
        }
    }
    out.sort();
    out
}

fn pack_params(sources: &[&str], dest: &str, base: &str) -> methods::ArchivePackParams {
    methods::ArchivePackParams {
        sources: sources.iter().map(|s| vp(s)).collect(),
        dest: vp(dest),
        format: methods::ArchiveFormat::Zip,
        level: Some(6),
        base: vp(base),
    }
}

/// El caso entero: se empaqueta un árbol y el propio provider de archivos lo
/// lee de vuelta, con los nombres relativos a la base.
///
/// La base es el directorio del que CUELGAN las fuentes, no la fuente misma:
/// con `base == source` la raíz no tendría nombre dentro del archivo, y eso es
/// justo lo que el op rehúsa en vez de inventárselo.
#[tokio::test]
async fn empaquetar_y_volver_a_leer() {
    let (engine, mem) = engine_con(&[
        ("mem:///proj/LEEME", b"hola"),
        ("mem:///proj/src/main.rs", b"fn main() {}"),
    ])
    .await;
    let handle = engine
        .pack_as(
            pack_params(&["mem:///proj"], "mem:///proj.zip", "mem:///"),
            norte_core::journal::Actor::User,
        )
        .await
        .expect("arranca");
    assert!(
        matches!(handle.join().await, TaskState::Completed),
        "la task termina"
    );
    assert!(!lee(&mem, "mem:///proj.zip").await.is_empty());

    assert_eq!(
        dentro(&engine, "mem:///proj.zip", "zip").await,
        vec![
            ("proj/LEEME".to_owned(), b"hola".to_vec()),
            ("proj/src/main.rs".to_owned(), b"fn main() {}".to_vec()),
        ],
        "los nombres cuelgan de la base, y el contenido vuelve entero"
    );
}

/// **Regla 1 de punta a punta**: un nombre que no es UTF-8 entra en el archivo
/// y sale byte a byte, pasando por el engine entero.
#[tokio::test]
async fn un_nombre_hostil_sobrevive_al_engine() {
    let hostil = b"cafe\xff.txt";
    let mem = Arc::new(MemProvider::new());
    let dir = vp("mem:///d");
    mem.mkdir(&dir).await.expect("mkdir");
    let p = dir.join(Segment::new(hostil.to_vec()).expect("seg"));
    let mut sink = mem.write(&p).await.expect("write");
    sink.write(Bytes::from_static(b"dentro"))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
    let engine = Engine::new();
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);

    let handle = engine
        .pack_as(
            pack_params(&["mem:///d"], "mem:///d.zip", "mem:///"),
            norte_core::journal::Actor::User,
        )
        .await
        .expect("arranca");
    assert!(matches!(handle.join().await, TaskState::Completed));

    let raiz = VPath::archive_compose("zip", &vp("mem:///d.zip"), &[]).expect("compose");
    let mut nombres = Vec::new();
    let mut pend = vec![raiz];
    while let Some(d) = pend.pop() {
        let mut stream = engine.list(&d).await.expect("list");
        while let Some(e) = stream.next().await {
            let e = e.expect("entrada");
            if e.kind == norte_proto::EntryKind::Dir {
                pend.push(e.path);
            } else if let Some(n) = e.path.file_name() {
                nombres.push(n.as_bytes().to_vec());
            }
        }
    }
    assert_eq!(nombres, vec![hostil.to_vec()], "byte a byte");
}

/// Una fuente que no cuelga de la base no tiene nombre dentro del archivo, y
/// inventárselo pondría la entrada donde nadie la espera al desempaquetar.
#[tokio::test]
async fn una_fuente_fuera_de_la_base_se_niega() {
    let (engine, _mem) = engine_con(&[("mem:///a/x", b"1"), ("mem:///b/y", b"2")]).await;
    let handle = engine
        .pack_as(
            pack_params(&["mem:///b/y"], "mem:///out.zip", "mem:///a"),
            norte_core::journal::Actor::User,
        )
        .await
        .expect("la task arranca; el rechazo es suyo");
    assert!(
        matches!(
            handle.join().await,
            TaskState::Failed {
                error: norte_proto::Error::InvalidPath,
                ..
            }
        ),
        "una fuente fuera de la base es InvalidPath"
    );
}

/// El destino no se sobrescribe: fabricar un archivo encima de un fichero que
/// ya está es pérdida silenciosa.
#[tokio::test]
async fn no_se_empaqueta_encima_de_algo() {
    let (engine, _mem) = engine_con(&[("mem:///a/x", b"1"), ("mem:///ya.zip", b"soy yo")]).await;
    let handle = engine
        .pack_as(
            pack_params(&["mem:///a"], "mem:///ya.zip", "mem:///"),
            norte_core::journal::Actor::User,
        )
        .await
        .expect("arranca");
    assert!(matches!(
        handle.join().await,
        TaskState::Failed {
            error: norte_proto::Error::Conflict { .. },
            ..
        }
    ));
}

/// **Regla 3**: cancelar deja el destino LIMPIO. Un archivo a medias que
/// parece un archivo es peor que ninguno.
#[tokio::test]
async fn cancelar_no_deja_medio_archivo() {
    let ficheros: Vec<(String, Vec<u8>)> = (0..64)
        .map(|i| (format!("mem:///grande/f{i:03}"), vec![b'x'; 200_000]))
        .collect();
    let refs: Vec<(&str, &[u8])> = ficheros
        .iter()
        .map(|(n, d)| (n.as_str(), d.as_slice()))
        .collect();
    let (engine, mem) = engine_con(&refs).await;

    let handle = engine
        .pack_as(
            pack_params(&["mem:///grande"], "mem:///grande.zip", "mem:///"),
            norte_core::journal::Actor::User,
        )
        .await
        .expect("arranca");
    handle.cancel();
    let estado = handle.join().await;
    assert!(
        matches!(estado, TaskState::Cancelled | TaskState::Completed),
        "o se canceló o ganó la carrera: {estado:?}"
    );
    if matches!(estado, TaskState::Cancelled) {
        assert!(
            mem.stat(&vp("mem:///grande.zip")).await.is_err(),
            "cancelado = destino limpio, ni un fichero a medias"
        );
    }
}

/// `archive.test` sobre un archivo sano: sin fallos, y diciendo QUÉ comprobó.
#[tokio::test]
async fn comprobar_un_archivo_sano() {
    let (engine, _mem) = engine_con(&[("mem:///a/x", b"contenido"), ("mem:///a/y", b"otro")]).await;
    let h = engine
        .pack_as(
            pack_params(&["mem:///a"], "mem:///a.zip", "mem:///"),
            norte_core::journal::Actor::User,
        )
        .await
        .expect("arranca");
    assert!(matches!(h.join().await, TaskState::Completed));

    let (h, informe) = engine
        .test_archive_as(
            methods::ArchiveTestParams {
                path: vp("mem:///a.zip"),
            },
            norte_core::journal::Actor::User,
        )
        .await
        .expect("arranca");
    assert!(matches!(h.join().await, TaskState::Completed));
    let r = informe.lock().expect("informe").clone();
    assert_eq!(r.entries, 2, "las dos entradas");
    assert!(r.failed.is_empty(), "sano: {:?}", r.failed);
    assert!(!r.truncated);
    assert_eq!(r.checked, vec!["crc".to_owned()], "un zip trae CRC");
}

/// Un fichero que no es de un formato conocido no se «comprueba»: decir que
/// pasa no significaría nada.
#[tokio::test]
async fn comprobar_algo_que_no_es_un_archivo_es_unsupported() {
    let (engine, _mem) = engine_con(&[("mem:///notas.txt", b"hola")]).await;
    let r = engine
        .test_archive_as(
            methods::ArchiveTestParams {
                path: vp("mem:///notas.txt"),
            },
            norte_core::journal::Actor::User,
        )
        .await;
    match r {
        Err(e) => assert_eq!(e, norte_proto::Error::Unsupported),
        Ok(_) => panic!("un .txt no es un archivo que comprobar"),
    }
}

/// Partir y volver a juntar da el fichero original, byte a byte.
#[tokio::test]
async fn partir_y_juntar_da_el_original() {
    let datos: Vec<u8> = (0..30_000_u32).map(|i| (i % 251) as u8).collect();
    let (engine, mem) = engine_con(&[("mem:///g.bin", &datos)]).await;
    mem.mkdir(&vp("mem:///trozos")).await.expect("mkdir");

    let h = engine
        .split_as(
            methods::FileSplitParams {
                path: vp("mem:///g.bin"),
                part_bytes: 8192,
                dest_dir: vp("mem:///trozos"),
            },
            norte_core::journal::Actor::User,
        )
        .await
        .expect("arranca");
    assert!(matches!(h.join().await, TaskState::Completed));

    // 30 000 / 8192 = 3 trozos y pico → cuatro, y el último más corto.
    assert_eq!(lee(&mem, "mem:///trozos/g.bin.001").await.len(), 8192);
    assert_eq!(
        lee(&mem, "mem:///trozos/g.bin.004").await.len(),
        30_000 - 3 * 8192
    );

    let h = engine
        .combine_as(
            methods::FileCombineParams {
                first: vp("mem:///trozos/g.bin.001"),
                dest: vp("mem:///vuelta.bin"),
            },
            norte_core::journal::Actor::User,
        )
        .await
        .expect("arranca");
    assert!(matches!(h.join().await, TaskState::Completed));
    assert_eq!(lee(&mem, "mem:///vuelta.bin").await, datos, "byte a byte");
}

/// Una división EXACTA no deja un trozo vacío al final: un `.004` de cero
/// bytes es un fichero que nadie sabe si sobra o falta.
#[tokio::test]
async fn una_division_exacta_no_deja_trozo_vacio() {
    let datos = vec![b'z'; 16_384];
    let (engine, mem) = engine_con(&[("mem:///e.bin", &datos)]).await;
    mem.mkdir(&vp("mem:///t")).await.expect("mkdir");
    let h = engine
        .split_as(
            methods::FileSplitParams {
                path: vp("mem:///e.bin"),
                part_bytes: 8192,
                dest_dir: vp("mem:///t"),
            },
            norte_core::journal::Actor::User,
        )
        .await
        .expect("arranca");
    assert!(matches!(h.join().await, TaskState::Completed));
    assert!(mem.stat(&vp("mem:///t/e.bin.002")).await.is_ok());
    assert!(
        mem.stat(&vp("mem:///t/e.bin.003")).await.is_err(),
        "no hay un tercer trozo vacío"
    );
}

/// Un hueco en la numeración NO se une a través: un fichero mal unido es un
/// fichero corrupto con buena pinta.
#[tokio::test]
async fn juntar_con_un_hueco_se_niega() {
    let (engine, mem) = engine_con(&[
        ("mem:///t/x.bin.001", &vec![b'a'; 100][..]),
        ("mem:///t/x.bin.003", &vec![b'c'; 100][..]),
    ])
    .await;
    let h = engine
        .combine_as(
            methods::FileCombineParams {
                first: vp("mem:///t/x.bin.001"),
                dest: vp("mem:///x.bin"),
            },
            norte_core::journal::Actor::User,
        )
        .await
        .expect("arranca");
    // Con un hueco, lo que hay es UN trozo: se une ese y ya. Lo que no puede
    // pasar es que el `.003` entre como si fuera el segundo.
    assert!(matches!(h.join().await, TaskState::Completed));
    assert_eq!(
        lee(&mem, "mem:///x.bin").await.len(),
        100,
        "solo el primero, jamás saltando el hueco"
    );
}

/// Un trozo intermedio más corto que el primero es un trozo que se copió a
/// medias: se rechaza ANTES de crear el destino.
#[tokio::test]
async fn juntar_con_un_trozo_corto_en_medio_se_niega() {
    let (engine, mem) = engine_con(&[
        ("mem:///t/y.bin.001", &vec![b'a'; 100][..]),
        ("mem:///t/y.bin.002", &vec![b'b'; 40][..]),
        ("mem:///t/y.bin.003", &vec![b'c'; 100][..]),
    ])
    .await;
    let h = engine
        .combine_as(
            methods::FileCombineParams {
                first: vp("mem:///t/y.bin.001"),
                dest: vp("mem:///y.bin"),
            },
            norte_core::journal::Actor::User,
        )
        .await
        .expect("arranca");
    assert!(matches!(
        h.join().await,
        TaskState::Failed {
            error: norte_proto::Error::Conflict { .. },
            ..
        }
    ));
    assert!(
        mem.stat(&vp("mem:///y.bin")).await.is_err(),
        "y no se creó el destino"
    );
}

/// Un trozo por debajo del mínimo produciría un millón de ficheros: se rehúsa
/// antes de escribir nada.
#[tokio::test]
async fn un_trozo_minusculo_se_niega() {
    let (engine, _mem) = engine_con(&[("mem:///p.bin", b"12345678")]).await;
    let h = engine
        .split_as(
            methods::FileSplitParams {
                path: vp("mem:///p.bin"),
                part_bytes: 2,
                dest_dir: vp("mem:///"),
            },
            norte_core::journal::Actor::User,
        )
        .await
        .expect("arranca");
    assert!(matches!(
        h.join().await,
        TaskState::Failed {
            error: norte_proto::Error::InvalidPath,
            ..
        }
    ));
}

/// Más de 999 trozos no cabe en la convención `.001`, y se dice ANTES de
/// escribir: descubrirlo en el trozo 1000 deja un conjunto que nadie puede
/// volver a juntar.
#[tokio::test]
async fn demasiados_trozos_se_niegan_antes_de_escribir() {
    let datos = vec![b'x'; 4096 * 1001];
    let (engine, mem) = engine_con(&[("mem:///enorme.bin", &datos)]).await;
    mem.mkdir(&vp("mem:///td")).await.expect("mkdir");
    let h = engine
        .split_as(
            methods::FileSplitParams {
                path: vp("mem:///enorme.bin"),
                part_bytes: 4096,
                dest_dir: vp("mem:///td"),
            },
            norte_core::journal::Actor::User,
        )
        .await
        .expect("arranca");
    assert!(matches!(
        h.join().await,
        TaskState::Failed {
            error: norte_proto::Error::LimitExceeded { .. },
            ..
        }
    ));
    assert!(
        mem.stat(&vp("mem:///td/enorme.bin.001")).await.is_err(),
        "ni el primero se llegó a escribir"
    );
}
