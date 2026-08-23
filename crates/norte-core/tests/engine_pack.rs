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
        // Toda la cadena de padres, no solo el inmediato: `mem:///p/a/b`
        // necesita `p` y `p/a`.
        let mut cadena = Vec::new();
        let mut actual = p.parent();
        while let Some(d) = actual {
            if d.is_root() {
                break;
            }
            actual = d.parent();
            cadena.push(d);
        }
        for d in cadena.into_iter().rev() {
            let _ = mem.mkdir(&d).await;
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

/// Dos fuentes que dan el MISMO nombre dentro del archivo se rehúsan.
///
/// Pasa con raíces que se solapan, que el wire acepta aunque las marcas de la
/// TUI no las formen: el archivo llevaría la entrada dos veces con su
/// contenido dos veces, nuestro índice resolvería «gana la última» y otras
/// herramientas la extraerían dos veces.
#[tokio::test]
async fn dos_fuentes_con_el_mismo_nombre_se_niegan() {
    let (engine, _mem) = engine_con(&[("mem:///p/a/b", b"x")]).await;
    let h = engine
        .pack_as(
            methods::ArchivePackParams {
                sources: vec![vp("mem:///p/a"), vp("mem:///p/a/b")],
                dest: vp("mem:///out.zip"),
                format: methods::ArchiveFormat::Zip,
                level: None,
                base: vp("mem:///p"),
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

/// **Cancelar un split no deja medio conjunto.**
///
/// Y es lo contrario de lo que hace una copia cancelada, a propósito: un árbol
/// copiado a medias se ve a simple vista, pero medio conjunto de trozos es
/// indistinguible de uno entero —todos del tamaño pedido, sin huecos— y
/// juntarlo da un fichero corto que pasa todos los guardas. Así que los que ya
/// estaban publicados se retiran.
#[tokio::test]
async fn cancelar_un_split_retira_los_trozos_ya_escritos() {
    let datos = vec![b'x'; 4096 * 40];
    let (engine, mem) = engine_con(&[("mem:///big.bin", &datos)]).await;
    mem.mkdir(&vp("mem:///piezas")).await.expect("mkdir");
    let h = engine
        .split_as(
            methods::FileSplitParams {
                path: vp("mem:///big.bin"),
                part_bytes: 4096,
                dest_dir: vp("mem:///piezas"),
            },
            norte_core::journal::Actor::User,
        )
        .await
        .expect("arranca");
    h.cancel();
    let estado = h.join().await;
    if matches!(estado, TaskState::Cancelled) {
        assert!(
            mem.stat(&vp("mem:///piezas/big.bin.001")).await.is_err(),
            "cancelado = ni un trozo suelto que parezca el principio de un conjunto"
        );
    }
}

/// Un trozo del conjunto que YA existe para el intento anterior se dice antes
/// de escribir el primero: mezclar trozos nuevos con rancios produce un
/// conjunto que se junta sin que nada chirríe.
#[tokio::test]
async fn un_trozo_preexistente_para_el_split_antes_de_empezar() {
    let datos = vec![b'y'; 4096 * 3];
    let (engine, mem) = engine_con(&[
        ("mem:///d.bin", &datos[..]),
        ("mem:///p/d.bin.002", &vec![b'z'; 10][..]),
    ])
    .await;
    let h = engine
        .split_as(
            methods::FileSplitParams {
                path: vp("mem:///d.bin"),
                part_bytes: 4096,
                dest_dir: vp("mem:///p"),
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
        mem.stat(&vp("mem:///p/d.bin.001")).await.is_err(),
        "ni el primero se escribió"
    );
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

/// Un hueco en la numeración NO se une a través, **y tampoco se une lo que
/// hay antes de él**.
///
/// Este test afirmaba lo contrario y pasaba: el paseo se paraba en el número
/// que falta, veía un conjunto de UN trozo y lo unía. La task decía
/// `Completed`, el journal anotaba un `Created`, y en disco quedaba el 20 % de
/// una ISO que monta como imagen corrupta — sin error, sin marca de parcial, y
/// contra lo que prometen el ADR 0060 y cuatro rustdocs. Lo encontró
/// `rust-reviewer` leyendo el nombre del test contra su assert.
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
    assert!(
        matches!(
            h.join().await,
            TaskState::Failed {
                error: norte_proto::Error::Conflict { .. },
                ..
            }
        ),
        "un conjunto incompleto es un error, no un fichero corto"
    );
    assert!(
        mem.stat(&vp("mem:///x.bin")).await.is_err(),
        "y no se creó el destino"
    );
}

/// Un conjunto COMPLETO de tres se une entero: la comprobación del hueco no
/// puede rechazar lo que sí está bien.
#[tokio::test]
async fn juntar_un_conjunto_completo_los_une_todos() {
    let (engine, mem) = engine_con(&[
        ("mem:///t/y.bin.001", &vec![b'a'; 100][..]),
        ("mem:///t/y.bin.002", &vec![b'b'; 100][..]),
        ("mem:///t/y.bin.003", &vec![b'c'; 40][..]),
        // Un vecino que NO es un trozo no estorba.
        ("mem:///t/y.bin.001.bak", &vec![b'z'; 5][..]),
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
    assert!(matches!(h.join().await, TaskState::Completed));
    assert_eq!(lee(&mem, "mem:///y.bin").await.len(), 240);
}

/// Un trozo intermedio más corto que el primero es un trozo que se copió a
/// medias: se rechaza ANTES de crear el destino.
#[tokio::test]
async fn juntar_con_un_trozo_corto_en_medio_se_niega() {
    let (engine, mem) = engine_con(&[
        ("mem:///t/z.bin.001", &vec![b'a'; 100][..]),
        ("mem:///t/z.bin.002", &vec![b'b'; 40][..]),
        ("mem:///t/z.bin.003", &vec![b'c'; 100][..]),
    ])
    .await;
    let h = engine
        .combine_as(
            methods::FileCombineParams {
                first: vp("mem:///t/z.bin.001"),
                dest: vp("mem:///z.bin"),
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
        mem.stat(&vp("mem:///z.bin")).await.is_err(),
        "y no se creó el destino"
    );
}

/// Un trozo por debajo del mínimo produciría un millón de ficheros: se rehúsa
/// antes de escribir nada.
#[tokio::test]
async fn un_trozo_minusculo_se_niega() {
    let (engine, _mem) = engine_con(&[("mem:///p.bin", b"12345678")]).await;
    // **En el SUBMIT, no en la Task.** Con la negativa dentro del cuerpo, el
    // RPC contestaba `{task_id}` y un cliente con guion leía un éxito: escribía
    // «partiendo…», salía, y no había pasado nada.
    let r = engine
        .split_as(
            methods::FileSplitParams {
                path: vp("mem:///p.bin"),
                part_bytes: 2,
                dest_dir: vp("mem:///"),
            },
            norte_core::journal::Actor::User,
        )
        .await;
    match r {
        Err(e) => assert_eq!(e, norte_proto::Error::InvalidPath),
        Ok(_) => panic!("un trozo de dos bytes no es una petición"),
    }
}

/// Más de 999 trozos no cabe en la convención `.001`, y se dice ANTES de
/// escribir: descubrirlo en el trozo 1000 deja un conjunto que nadie puede
/// volver a juntar.
#[tokio::test]
async fn demasiados_trozos_se_niegan_antes_de_escribir() {
    let datos = vec![b'x'; 4096 * 1001];
    let (engine, mem) = engine_con(&[("mem:///enorme.bin", &datos)]).await;
    mem.mkdir(&vp("mem:///td")).await.expect("mkdir");
    let r = engine
        .split_as(
            methods::FileSplitParams {
                path: vp("mem:///enorme.bin"),
                part_bytes: 4096,
                dest_dir: vp("mem:///td"),
            },
            norte_core::journal::Actor::User,
        )
        .await;
    match r {
        Err(norte_proto::Error::LimitExceeded { .. }) => {}
        Err(otro) => panic!("esperaba LimitExceeded, fue {otro:?}"),
        Ok(_) => panic!("más de 999 trozos se dice en el SUBMIT, no después"),
    }
    assert!(
        mem.stat(&vp("mem:///td/enorme.bin.001")).await.is_err(),
        "ni el primero se llegó a escribir"
    );
}

/// #250 — dos entradas que PLIEGAN al mismo nombre tampoco se empaquetan.
///
/// Los bytes distintos no bastan: lo que decide es si colisionan allí donde el
/// archivo se extraiga, y un archivo no puede saberlo — se manda por ahí. Se
/// pliega con el modo más ancho a propósito, así que la pregunta no es «¿aquí?»
/// sino «¿en alguna parte?». Extraído allí, uno de los dos desaparece sin decir
/// nada, y ésa es la dirección que ADR 0005 dice no tomar.
///
/// Cuatro parejas del corpus canónico, y son cuatro pliegues distintos:
/// normalización NFD/NFC, singleton NFC, mu contra micro, y el pliegue completo
/// de un ext4 `+F`. Un arreglo que solo mirase la caja no pasaría ninguno.
///
/// La quinta que el issue lista —`win_trailing_dot` contra
/// `win_trailing_space`— NO entra, y es deliberado: esos dos no pliegan a lo
/// mismo bajo ninguna clave Unicode. Lo que hace que colisionen es que Windows
/// RECORTA la cola de un nombre sin el prefijo `\\?\`, que es mangling de
/// rutas y no plegado. Cazarlo pide otra comprobación, y va en el punto 2 de
/// #250 —el aviso de «este nombre significa otra cosa allí»— junto a `a\b`,
/// `f:ads` y `CON`.
#[tokio::test]
async fn dos_entradas_que_pliegan_al_mismo_nombre_no_se_empaquetan() {
    let corpus = norte_testkit::corpus::hostile_names();
    let bytes_de = |id: &str| {
        corpus
            .iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("la fixture {id} está"))
            .bytes
            .clone()
    };
    let parejas = [
        ("nfd_e_acute", "nfc_e_acute"),
        ("singleton_kelvin_sign", "ascii_capital_k"),
        ("micro_sign_mu", "greek_mu_twin"),
        ("ext4_full_fold_es_zett", "ext4_full_fold_ss"),
    ];
    for (a, b) in parejas {
        let (uno, otro) = (bytes_de(a), bytes_de(b));
        assert_ne!(uno, otro, "[{a}/{b}] la premisa: bytes distintos");

        let mem = Arc::new(MemProvider::new());
        mem.mkdir(&vp("mem:///p")).await.expect("p");
        for nombre in [&uno, &otro] {
            let p = vp("mem:///p").join(Segment::new(nombre.clone()).expect("segmento"));
            let mut sink = mem.write(&p).await.expect("write");
            sink.write(Bytes::from_static(b"x")).await.expect("chunk");
            sink.commit().await.expect("commit");
        }
        let engine = Engine::new();
        engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);

        let h = engine
            .pack_as(
                pack_params(&["mem:///p"], "mem:///out.zip", "mem:///"),
                norte_core::journal::Actor::User,
            )
            .await
            .expect("arranca");
        assert!(
            matches!(
                h.join().await,
                TaskState::Failed {
                    error: norte_proto::Error::Conflict { .. },
                    ..
                }
            ),
            "[{a}/{b}] se empaquetaron las dos: una se pierde al extraer"
        );
    }
}

/// Y dos nombres que NO pliegan a lo mismo se empaquetan, que es lo normal. La
/// comprobación no puede costar la operación legítima.
#[tokio::test]
async fn dos_nombres_distintos_de_verdad_si_se_empaquetan() {
    let (engine, mem) = engine_con(&[("mem:///p/uno.txt", b"1"), ("mem:///p/dos.txt", b"2")]).await;
    let h = engine
        .pack_as(
            pack_params(&["mem:///p"], "mem:///out.zip", "mem:///"),
            norte_core::journal::Actor::User,
        )
        .await
        .expect("arranca");
    assert_eq!(h.join().await, TaskState::Completed);
    assert!(!lee(&mem, "mem:///out.zip").await.is_empty());
}
