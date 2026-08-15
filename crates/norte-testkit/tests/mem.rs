//! Tests de `MemProvider`: semántica del contrato (colisiones, caja, rename
//! de subárboles, abort sin rastro) y de la inyección de fallos (byte exacto,
//! desconexión, latencia determinista con reloj pausado).

use bytes::Bytes;
use futures::StreamExt;
use norte_proto::{CapabilityFlags, ConflictKind, EntryKind, Error, Segment, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido de test")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write abre");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk entra");
    sink.commit().await.expect("commit publica");
}

async fn read_all(mem: &MemProvider, wire: &str) -> Result<Vec<u8>, Error> {
    let mut stream = mem.read(&vp(wire), None).await?;
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk?);
    }
    Ok(out)
}

// ---------- roundtrip y visibilidad ----------

#[tokio::test]
async fn write_commit_read_roundtrip() {
    let mem = MemProvider::new();
    write_file(&mem, "mem:///f.txt", b"hola").await;
    assert_eq!(read_all(&mem, "mem:///f.txt").await.unwrap(), b"hola");
    let e = mem.stat(&vp("mem:///f.txt")).await.unwrap();
    assert_eq!(e.kind, EntryKind::File);
    assert_eq!(e.size, Some(4));
}

#[tokio::test]
async fn nothing_visible_before_commit() {
    let mem = MemProvider::new();
    let mut sink = mem.write(&vp("mem:///f")).await.unwrap();
    sink.write(Bytes::from_static(b"data")).await.unwrap();
    // Sin commit: el path final no existe.
    assert_eq!(
        mem.stat(&vp("mem:///f")).await.unwrap_err(),
        Error::NotFound
    );
    sink.commit().await.unwrap();
    assert!(mem.stat(&vp("mem:///f")).await.is_ok());
}

#[tokio::test]
async fn abort_leaves_no_trace() {
    let mem = MemProvider::new();
    let mut sink = mem.write(&vp("mem:///f")).await.unwrap();
    sink.write(Bytes::from_static(b"data")).await.unwrap();
    sink.abort().await.unwrap();
    assert_eq!(
        mem.stat(&vp("mem:///f")).await.unwrap_err(),
        Error::NotFound
    );
    let mut list = mem.list(&vp("mem:///")).await.unwrap();
    assert!(list.next().await.is_none(), "raíz vacía tras abort");
}

#[tokio::test]
async fn hostile_names_roundtrip_byte_exact() {
    let mem = MemProvider::new();
    let root = MemProvider::root();
    for name in norte_testkit::corpus::hostile_names() {
        let seg = norte_proto::Segment::new(name.bytes.clone()).expect("segmento válido");
        let path = root.join(seg);
        let mut sink = mem.write(&path).await.expect("write abre");
        sink.write(Bytes::from_static(b"x")).await.unwrap();
        sink.commit().await.unwrap();
        let e = mem.stat(&path).await.unwrap_or_else(|err| {
            panic!("[{}] stat tras commit: {err:?}", name.id);
        });
        assert_eq!(
            e.path.file_name().unwrap().as_bytes(),
            name.bytes.as_slice(),
            "[{}] bytes intactos",
            name.id
        );
    }
}

// ---------- colisiones y caja ----------

#[tokio::test]
async fn write_collision_is_conflict() {
    let mem = MemProvider::new();
    write_file(&mem, "mem:///f", b"1").await;
    assert_eq!(
        mem.write(&vp("mem:///f"))
            .await
            .err()
            .map(|e| format!("{e:?}")),
        Some("Conflict { conflict: Exists }".to_owned())
    );
}

#[tokio::test]
async fn case_insensitive_collision_detected() {
    let mem =
        MemProvider::with_flags(CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING);
    write_file(&mem, "mem:///File", b"1").await;
    // En FS case-insensitive, "file" colisiona con "File".
    match mem.write(&vp("mem:///file")).await {
        Err(Error::Conflict { conflict }) => assert_eq!(conflict, ConflictKind::CaseCollision),
        Err(e) => panic!("esperaba CaseCollision, fue {e:?}"),
        Ok(_) => panic!("esperaba CaseCollision, el write abrió"),
    }
    // Y resuelve al mismo nodo al leer.
    assert_eq!(read_all(&mem, "mem:///file").await.unwrap(), b"1");
}

#[tokio::test]
async fn case_sensitive_no_collision() {
    let mem = MemProvider::new(); // CASE_SENSITIVE
    write_file(&mem, "mem:///File", b"1").await;
    write_file(&mem, "mem:///file", b"2").await;
    assert_eq!(read_all(&mem, "mem:///File").await.unwrap(), b"1");
    assert_eq!(read_all(&mem, "mem:///file").await.unwrap(), b"2");
}

#[tokio::test]
async fn case_change_rename_allowed_when_insensitive() {
    let mem = MemProvider::with_flags(CapabilityFlags::CASE_PRESERVING);
    write_file(&mem, "mem:///readme", b"x").await;
    // a → A sobre el mismo nodo: rename de caja, permitido.
    mem.rename(&vp("mem:///readme"), &vp("mem:///README"))
        .await
        .expect("cambio de caja permitido");
    let e = mem.stat(&vp("mem:///README")).await.unwrap();
    assert_eq!(e.path.file_name().unwrap().as_bytes(), b"README");
}

// ---------- estructura ----------

#[tokio::test]
async fn mkdir_requires_parent() {
    let mem = MemProvider::new();
    assert_eq!(
        mem.mkdir(&vp("mem:///a/b")).await.unwrap_err(),
        Error::NotFound
    );
    mem.mkdir(&vp("mem:///a")).await.unwrap();
    mem.mkdir(&vp("mem:///a/b")).await.unwrap();
    let e = mem.stat(&vp("mem:///a/b")).await.unwrap();
    assert_eq!(e.kind, EntryKind::Dir);
}

#[tokio::test]
async fn remove_nonempty_dir_refused() {
    let mem = MemProvider::new();
    mem.mkdir(&vp("mem:///d")).await.unwrap();
    write_file(&mem, "mem:///d/f", b"x").await;
    assert!(mem.remove(&vp("mem:///d")).await.is_err());
    mem.remove(&vp("mem:///d/f")).await.unwrap();
    mem.remove(&vp("mem:///d")).await.unwrap();
    assert_eq!(
        mem.stat(&vp("mem:///d")).await.unwrap_err(),
        Error::NotFound
    );
}

#[tokio::test]
async fn rename_moves_subtree() {
    let mem = MemProvider::new();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    mem.mkdir(&vp("mem:///src/sub")).await.unwrap();
    write_file(&mem, "mem:///src/sub/f", b"x").await;
    mem.rename(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .unwrap();
    assert_eq!(
        mem.stat(&vp("mem:///src")).await.unwrap_err(),
        Error::NotFound
    );
    assert_eq!(read_all(&mem, "mem:///dst/sub/f").await.unwrap(), b"x");
}

#[tokio::test]
async fn list_is_deterministic_byte_order() {
    let mem = MemProvider::new();
    for name in ["b", "a", "c"] {
        write_file(&mem, &format!("mem:///{name}"), b"x").await;
    }
    let names: Vec<Vec<u8>> = mem
        .list(&vp("mem:///"))
        .await
        .unwrap()
        .map(|e| e.unwrap().path.file_name().unwrap().as_bytes().to_vec())
        .collect()
        .await;
    assert_eq!(names, vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
}

#[tokio::test]
async fn logical_clock_makes_mtimes_deterministic() {
    let run = || async {
        let mem = MemProvider::new();
        write_file(&mem, "mem:///a", b"1").await;
        write_file(&mem, "mem:///b", b"2").await;
        (
            mem.stat(&vp("mem:///a")).await.unwrap().mtime_ms,
            mem.stat(&vp("mem:///b")).await.unwrap().mtime_ms,
        )
    };
    assert_eq!(run().await, run().await, "dos ejecuciones idénticas");
}

// ---------- copy_native ----------

#[tokio::test]
async fn copy_native_none_without_capability() {
    let mem = MemProvider::new();
    write_file(&mem, "mem:///a", b"x").await;
    assert!(
        mem.copy_native(&vp("mem:///a"), &vp("mem:///b"))
            .await
            .is_none()
    );
}

#[tokio::test]
async fn copy_native_copies_when_declared() {
    let mem =
        MemProvider::with_flags(CapabilityFlags::SERVER_COPY | CapabilityFlags::CASE_SENSITIVE);
    write_file(&mem, "mem:///a", b"contenido").await;
    mem.copy_native(&vp("mem:///a"), &vp("mem:///b"))
        .await
        .expect("SERVER_COPY declarado")
        .expect("copia ok");
    assert_eq!(read_all(&mem, "mem:///b").await.unwrap(), b"contenido");
}

// ---------- inyección de fallos ----------

#[tokio::test]
async fn fail_read_at_exact_byte() {
    let mem = MemProvider::new();
    let content = vec![0xAB; 5000];
    write_file(&mem, "mem:///big", &content).await;
    mem.faults().fail_read_at(&vp("mem:///big"), 3000);

    let mut stream = mem.read(&vp("mem:///big"), None).await.unwrap();
    let mut got = Vec::new();
    let mut err = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(chunk) => got.extend_from_slice(&chunk),
            Err(e) => {
                err = Some(e);
                break;
            }
        }
    }
    assert!(got.len() < 3000 + 1, "jamás entrega más de N bytes");
    assert_eq!(err, Some(Error::Io { retryable: false }));
}

#[tokio::test]
async fn fail_write_at_exact_byte() {
    let mem = MemProvider::new();
    mem.faults().fail_write_at(&vp("mem:///out"), 100);
    let mut sink = mem.write(&vp("mem:///out")).await.unwrap();
    let err = sink.write(Bytes::from(vec![0u8; 200])).await.unwrap_err();
    assert_eq!(err, Error::Io { retryable: false });
    // Tras el fallo, abort limpia y no queda rastro.
    sink.abort().await.unwrap();
    assert_eq!(
        mem.stat(&vp("mem:///out")).await.unwrap_err(),
        Error::NotFound
    );
}

#[tokio::test]
async fn disconnect_after_n_ops() {
    let mem = MemProvider::new();
    write_file(&mem, "mem:///f", b"x").await;
    mem.faults().disconnect_after(2);
    assert!(mem.stat(&vp("mem:///f")).await.is_ok()); // op 1
    assert!(mem.stat(&vp("mem:///f")).await.is_ok()); // op 2
    assert_eq!(
        mem.stat(&vp("mem:///f")).await.unwrap_err(),
        Error::ProviderUnavailable { retryable: true }
    );
    // Y se queda desconectado.
    assert!(mem.list(&vp("mem:///")).await.is_err());
    mem.faults().clear();
    assert!(mem.stat(&vp("mem:///f")).await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn latency_uses_tokio_clock() {
    let mem = MemProvider::new();
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(500)));
    let before = tokio::time::Instant::now();
    // Con reloj pausado, tokio avanza el tiempo virtual automáticamente.
    let _ = mem.stat(&vp("mem:///nope")).await;
    assert!(before.elapsed() >= std::time::Duration::from_millis(500));
}

// ---------- coherencia de caja (hallazgos del encoding-auditor, fase 6) ----------

#[tokio::test]
async fn list_resolves_case_insensitively() {
    let mem = MemProvider::with_flags(CapabilityFlags::CASE_PRESERVING);
    mem.mkdir(&vp("mem:///Dir")).await.unwrap();
    write_file(&mem, "mem:///Dir/f", b"x").await;
    // list con otra caja debe ver lo mismo que stat.
    let n = mem
        .list(&vp("mem:///dir"))
        .await
        .expect("resuelve como stat")
        .count()
        .await;
    assert_eq!(n, 1, "list resuelve la caja igual que stat");
}

#[tokio::test]
async fn mixed_case_write_lands_under_real_parent() {
    let mem = MemProvider::with_flags(CapabilityFlags::CASE_PRESERVING);
    mem.mkdir(&vp("mem:///Dir")).await.unwrap();
    // Escribir vía caja distinta: el hijo cuelga del dir REAL, jamás huérfano.
    write_file(&mem, "mem:///dir/g", b"x").await;
    let n = mem.list(&vp("mem:///Dir")).await.unwrap().count().await;
    assert_eq!(n, 1, "el hijo es visible bajo el dir real");
    // Y el dir ya no está vacío: remove debe negarse.
    assert!(mem.remove(&vp("mem:///Dir")).await.is_err());
}

#[tokio::test]
async fn commit_after_parent_removed_fails() {
    let mem = MemProvider::new();
    mem.mkdir(&vp("mem:///d")).await.unwrap();
    let mut sink = mem.write(&vp("mem:///d/f")).await.unwrap();
    sink.write(Bytes::from_static(b"x")).await.unwrap();
    mem.remove(&vp("mem:///d")).await.unwrap();
    // El padre desapareció entre write() y commit(): jamás huérfanos.
    assert_eq!(sink.commit().await.unwrap_err(), Error::NotFound);
}

#[tokio::test]
async fn rename_into_own_subtree_refused() {
    let mem = MemProvider::new();
    mem.mkdir(&vp("mem:///a")).await.unwrap();
    assert_eq!(
        mem.rename(&vp("mem:///a"), &vp("mem:///a/b"))
            .await
            .unwrap_err(),
        Error::InvalidPath,
        "mover un dir dentro de sí mismo es EINVAL"
    );
    // El árbol queda intacto.
    assert!(mem.stat(&vp("mem:///a")).await.is_ok());
}

#[tokio::test]
async fn entry_paths_preserve_authority() {
    let mem = MemProvider::new();
    write_file(&mem, "mem://conn1/f", b"x").await;
    let e = mem.stat(&vp("mem://conn1/f")).await.unwrap();
    assert_eq!(e.path.authority(), Some("conn1"), "stat preserva authority");
    let listed: Vec<_> = mem
        .list(&vp("mem://conn1/"))
        .await
        .unwrap()
        .map(|e| e.unwrap().path.authority().map(str::to_owned))
        .collect()
        .await;
    assert_eq!(listed, vec![Some("conn1".to_owned())], "list también");
}

#[tokio::test]
async fn commit_detects_late_case_collision() {
    let mem = MemProvider::with_flags(CapabilityFlags::CASE_PRESERVING);
    let mut sink = mem.write(&vp("mem:///file")).await.unwrap();
    sink.write(Bytes::from_static(b"1")).await.unwrap();
    // Aparece "File" entre write() y commit(): colisión de caja, no Exists.
    write_file(&mem, "mem:///File", b"2").await;
    match sink.commit().await {
        Err(Error::Conflict { conflict }) => assert_eq!(conflict, ConflictKind::CaseCollision),
        Err(e) => panic!("esperaba CaseCollision, fue {e:?}"),
        Ok(()) => panic!("esperaba CaseCollision, el commit publicó"),
    }
}

// ---------- eje de normalización NFC/NFD (issue #7) ----------

/// Simulación APFS: lookup insensible a la normalización, bytes preservados.
/// La colisión solo-por-normalización se etiqueta `Normalization` (#8).
#[tokio::test]
async fn normalization_insensitive_collides_with_label() {
    use norte_testkit::Normalization;
    let mem = MemProvider::new().with_normalization(Normalization::Insensitive);
    let root = MemProvider::root();
    let nfc = root.join(Segment::new(vec![0xC3, 0xA9]).unwrap()); // é NFC
    let nfd = root.join(Segment::new(vec![0x65, 0xCC, 0x81]).unwrap()); // é NFD

    let mut sink = mem.write(&nfc).await.unwrap();
    sink.write(bytes::Bytes::from_static(b"x")).await.unwrap();
    sink.commit().await.unwrap();

    // El lookup NFD resuelve al archivo NFC (insensible, preservando bytes).
    let e = mem.stat(&nfd).await.expect("lookup normalizado resuelve");
    assert_eq!(
        e.path.file_name().unwrap().as_bytes(),
        &[0xC3, 0xA9],
        "los bytes ALMACENADOS (NFC) se preservan"
    );

    // Escribir la variante NFD colisiona con la etiqueta correcta.
    match mem.write(&nfd).await {
        Err(Error::Conflict {
            conflict: ConflictKind::Normalization,
        }) => {}
        Err(other) => panic!("esperaba Conflict::Normalization, fue {other:?}"),
        Ok(_) => panic!("esperaba Conflict::Normalization, fue Ok(sink)"),
    }
}

/// Default (byte-exact, como ext4): NFC y NFD son archivos DISTINTOS.
#[tokio::test]
async fn normalization_byte_exact_keeps_both() {
    let mem = MemProvider::new();
    let root = MemProvider::root();
    let nfc = root.join(Segment::new(vec![0xC3, 0xA9]).unwrap());
    let nfd = root.join(Segment::new(vec![0x65, 0xCC, 0x81]).unwrap());
    for p in [&nfc, &nfd] {
        let mut sink = mem.write(p).await.unwrap();
        sink.write(bytes::Bytes::from_static(b"x")).await.unwrap();
        sink.commit().await.unwrap();
    }
    assert!(mem.stat(&nfc).await.is_ok());
    assert!(mem.stat(&nfd).await.is_ok());
}

/// Indisponibilidad TRANSITORIA (issue de reintentos, ADR 0005): las
/// próximas n ops fallan retryable y el provider se recupera solo.
#[tokio::test]
async fn unavailable_for_next_recovers() {
    let mem = MemProvider::new();
    mem.faults().unavailable_for_next(2);
    let root = MemProvider::root();
    for _ in 0..2 {
        match mem.stat(&root).await {
            Err(Error::ProviderUnavailable { retryable: true }) => {}
            other => panic!("esperaba ProviderUnavailable retryable, fue {other:?}"),
        }
    }
    assert!(mem.stat(&root).await.is_ok(), "tras n ops, recupera");
}

// ---------- identidad de nodo (issue #16) ----------

/// `without_node_ids()` simula un backend SIN identidad estable (object
/// storage, ftp): `node_id` = `Ok(None)` siempre, aun existiendo el nodo.
#[tokio::test]
async fn without_node_ids_devuelve_none() {
    use norte_vfs::FollowLinks;
    let mem = MemProvider::new().without_node_ids();
    write_file(&mem, "mem:///f", b"x").await;
    assert_eq!(
        mem.node_id(&vp("mem:///f"), FollowLinks::No).await.unwrap(),
        None
    );
}

/// La identidad distingue nodos aunque los PATHS se plieguen: en un Mem
/// case-insensitive, `a` y `A` resuelven al MISMO nodo → mismo id. Es lo
/// que el guard del engine no podía saber con heurísticas (issue #16).
#[tokio::test]
async fn node_id_es_el_mismo_para_variantes_de_caja_plegadas() {
    use norte_vfs::FollowLinks;
    let mem =
        MemProvider::with_flags(CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING);
    write_file(&mem, "mem:///Mismo", b"x").await;
    let a = mem
        .node_id(&vp("mem:///Mismo"), FollowLinks::No)
        .await
        .unwrap()
        .expect("Mem tiene identidad");
    let b = mem
        .node_id(&vp("mem:///mismo"), FollowLinks::No)
        .await
        .unwrap()
        .expect("Mem tiene identidad");
    assert_eq!(a, b, "mismo nodo bajo cualquier caja que el FS pliegue");
}

// ---------- kind de symlink (issue #18) ----------

/// `SymlinkKind::Unknown`: el provider resuelve el target en SU árbol.
/// Target dir → Dir; target archivo → File; roto → File (documentado).
#[tokio::test]
async fn symlink_unknown_resuelve_el_kind_del_target() {
    use norte_vfs::SymlinkKind;
    let mem = MemProvider::new();
    mem.mkdir(&vp("mem:///d")).await.unwrap();
    write_file(&mem, "mem:///f", b"x").await;

    mem.symlink(&vp("mem:///ld"), b"d", SymlinkKind::Unknown)
        .await
        .unwrap();
    mem.symlink(&vp("mem:///lf"), b"f", SymlinkKind::Unknown)
        .await
        .unwrap();
    mem.symlink(&vp("mem:///lroto"), b"nada", SymlinkKind::Unknown)
        .await
        .unwrap();

    assert_eq!(
        mem.symlink_kind_of(&vp("mem:///ld")),
        Some(SymlinkKind::Dir)
    );
    assert_eq!(
        mem.symlink_kind_of(&vp("mem:///lf")),
        Some(SymlinkKind::File)
    );
    assert_eq!(
        mem.symlink_kind_of(&vp("mem:///lroto")),
        Some(SymlinkKind::File),
        "roto degrada a File, jamás error"
    );
    // Kind explícito: se respeta tal cual, sin resolver nada.
    mem.symlink(&vp("mem:///lex"), b"nada", SymlinkKind::Dir)
        .await
        .unwrap();
    assert_eq!(
        mem.symlink_kind_of(&vp("mem:///lex")),
        Some(SymlinkKind::Dir)
    );
}

// ---------- mutación ambigua (issue #17) ----------

/// El fallo POST-efecto: la mutación se aplica Y devuelve
/// `ProviderUnavailable` retryable — el "timeout tras commit" de un remoto.
/// Es la fixture del retry con desambiguación del engine.
#[tokio::test]
async fn ambiguous_mutation_aplica_el_efecto_y_falla_transitorio() {
    let mem = MemProvider::new();
    mem.faults().ambiguous_mutations(1);
    match mem.mkdir(&vp("mem:///d")).await {
        Err(Error::ProviderUnavailable { retryable: true }) => {}
        other => panic!("esperaba ProviderUnavailable retryable, fue {other:?}"),
    }
    // El efecto SÍ se aplicó (esa es la ambigüedad).
    let e = mem.stat(&vp("mem:///d")).await.unwrap();
    assert_eq!(e.kind, EntryKind::Dir);
    // Consumido: la siguiente mutación es normal.
    mem.mkdir(&vp("mem:///d2")).await.unwrap();
}

/// El fallo ambiguo cubre las 4 mutaciones puntuales del trait
/// (mkdir/remove/rename/symlink); las lecturas NO lo consumen.
#[tokio::test]
async fn ambiguous_mutation_cubre_las_cuatro_mutaciones() {
    use norte_vfs::SymlinkKind;
    let mem = MemProvider::new();
    write_file(&mem, "mem:///a", b"x").await;

    // Una lectura de por medio no consume el fallo armado.
    mem.faults().ambiguous_mutations(1);
    mem.stat(&vp("mem:///a")).await.unwrap();
    assert!(mem.rename(&vp("mem:///a"), &vp("mem:///b")).await.is_err());
    assert!(mem.stat(&vp("mem:///b")).await.is_ok(), "rename aplicado");

    mem.faults().ambiguous_mutations(1);
    assert!(mem.remove(&vp("mem:///b")).await.is_err());
    assert_eq!(
        mem.stat(&vp("mem:///b")).await.unwrap_err(),
        Error::NotFound,
        "remove aplicado"
    );

    mem.faults().ambiguous_mutations(1);
    assert!(
        mem.symlink(&vp("mem:///l"), b"t", SymlinkKind::File)
            .await
            .is_err()
    );
    assert_eq!(mem.read_link(&vp("mem:///l")).await.unwrap(), b"t");
}

/// Cadena link→link con follow: `NotFound`, coherente con `read()` (la
/// resolución mínima de Mem no sigue cadenas — límite documentado).
#[tokio::test]
async fn node_id_follow_sobre_cadena_es_notfound() {
    use norte_vfs::{FollowLinks, SymlinkKind};
    let mem = MemProvider::new();
    write_file(&mem, "mem:///f", b"x").await;
    mem.symlink(&vp("mem:///l1"), b"f", SymlinkKind::File)
        .await
        .unwrap();
    mem.symlink(&vp("mem:///l2"), b"l1", SymlinkKind::File)
        .await
        .unwrap();
    assert_eq!(
        mem.node_id(&vp("mem:///l2"), FollowLinks::Yes)
            .await
            .unwrap_err(),
        Error::NotFound
    );
    // Un nivel sí resuelve.
    assert!(
        mem.node_id(&vp("mem:///l1"), FollowLinks::Yes)
            .await
            .unwrap()
            .is_some()
    );
}

/// La travesía de symlinks compone con el eje de normalización: dir
/// almacenado en NFD, lookup en NFC a través de un link intermedio.
#[tokio::test]
async fn travesia_compone_con_normalizacion_insensible() {
    use norte_testkit::Normalization;
    let mem = MemProvider::new().with_normalization(Normalization::Insensitive);
    // Dir con nombre NFD (e + combinante).
    mem.mkdir(&vp("mem:///e%CC%81")).await.unwrap();
    write_file(&mem, "mem:///e%CC%81/f", b"x").await;
    // Link apuntando al dir por su forma NFC (é precompuesto).
    mem.symlink(&vp("mem:///ln"), &[0xC3, 0xA9], norte_vfs::SymlinkKind::Dir)
        .await
        .unwrap();
    // Lectura A TRAVÉS del link (target NFC → dirent NFD).
    assert_eq!(read_all(&mem, "mem:///ln/f").await.unwrap(), b"x");
    let e = mem.stat(&vp("mem:///ln/f")).await.unwrap();
    assert_eq!(e.kind, EntryKind::File);
}

/// `SymlinkKind::Unknown` con targets hostiles: absoluto y `..` degradan
/// a File (resolución mínima → Unsupported); un target no-UTF8 que
/// apunta a un dir de nombre no-UTF8 resuelve Dir.
#[tokio::test]
async fn symlink_unknown_con_targets_hostiles() {
    use norte_vfs::SymlinkKind;
    let mem = MemProvider::new();
    mem.symlink(&vp("mem:///labs"), b"/etc", SymlinkKind::Unknown)
        .await
        .unwrap();
    mem.symlink(&vp("mem:///ldot"), b"../fuera", SymlinkKind::Unknown)
        .await
        .unwrap();
    assert_eq!(
        mem.symlink_kind_of(&vp("mem:///labs")),
        Some(SymlinkKind::File)
    );
    assert_eq!(
        mem.symlink_kind_of(&vp("mem:///ldot")),
        Some(SymlinkKind::File)
    );

    let seg = norte_proto::Segment::new(vec![0xE9]).unwrap();
    let hostile_dir = MemProvider::root().join(seg.clone());
    mem.mkdir(&hostile_dir).await.unwrap();
    let link = MemProvider::root().join(norte_proto::Segment::new(b"lhost".to_vec()).unwrap());
    mem.symlink(&link, &[0xE9], SymlinkKind::Unknown)
        .await
        .unwrap();
    assert_eq!(mem.symlink_kind_of(&link), Some(SymlinkKind::Dir));
}

#[tokio::test]
async fn faults_cuenta_las_llamadas_a_read() {
    // Observabilidad #61: el contador de reads permite a los tests de caché
    // asertar "N ops concurrentes = los reads de UN solo build".
    let mem = MemProvider::new();
    write_file(&mem, "mem:///f", b"data").await;
    let faults = mem.faults();
    assert_eq!(faults.read_calls(), 0);
    let _ = read_all(&mem, "mem:///f").await.expect("read");
    let _ = read_all(&mem, "mem:///f").await.expect("read");
    assert_eq!(faults.read_calls(), 2);
}

// ---------- attrs sintéticos (#108 bloque 2) ----------

#[tokio::test]
async fn synthetic_attrs_hostiles_y_deterministas() {
    use norte_vfs::{AttrRequest, ListOptions};
    let mem = MemProvider::new().with_synthetic_attrs();
    write_file(&mem, "mem:///f.txt", b"x").await;
    let opt = ListOptions {
        attrs: AttrRequest::sanitized(
            ["mem.owner", "mem.note", "mem.mode", "mem.stamp"].map(str::to_owned),
        ),
    };
    let e = mem
        .stat_with(&vp("mem:///f.txt"), &opt)
        .await
        .expect("stat_with");
    // Dueño no-UTF-8: BYTES crudos, jamás String (regla 1).
    assert_eq!(
        e.attrs.get("mem.owner"),
        Some(&norte_proto::AttrValue::Bytes(
            b"due\xf1o-\xff\xfe".to_vec()
        ))
    );
    // Texto hostil: RTL override + ZWJ, dentro del tope.
    let Some(norte_proto::AttrValue::Text(note)) = e.attrs.get("mem.note") else {
        panic!("mem.note debe ser Text");
    };
    assert!(note.contains('\u{202e}') && note.contains('\u{200d}'));
    assert_eq!(
        e.attrs.get("mem.mode"),
        Some(&norte_proto::AttrValue::Uint(0o100_644))
    );
    assert!(matches!(
        e.attrs.get("mem.stamp"),
        Some(norte_proto::AttrValue::TimeMs(_))
    ));

    // Petición parcial: SOLO lo pedido.
    let solo = ListOptions {
        attrs: AttrRequest::sanitized(["mem.mode".to_owned()]),
    };
    let e = mem
        .stat_with(&vp("mem:///f.txt"), &solo)
        .await
        .expect("stat_with");
    assert_eq!(e.attrs.len(), 1);

    // Sin pedir → sin attrs, también en list.
    assert!(
        mem.stat(&vp("mem:///f.txt"))
            .await
            .expect("stat")
            .attrs
            .is_empty()
    );
    let mut s = mem.list(&vp("mem:///")).await.expect("list");
    while let Some(e) = s.next().await {
        assert!(e.expect("entrada").attrs.is_empty());
    }
}

// ---------- capabilities por ubicación (ADR 0054) ----------

#[tokio::test]
async fn capabilities_at_defaults_to_the_declaration() {
    let mem = MemProvider::new();
    let root = MemProvider::root();
    assert_eq!(
        mem.capabilities_at(&root).await.expect("responde"),
        mem.capabilities(),
        "sin guion, la ubicación responde lo que declara el backend"
    );
}

#[tokio::test]
async fn a_scripted_location_overrides_the_declaration() {
    let mem = MemProvider::new();
    let usb = vp("mem:///usb");
    mem.mkdir(&usb).await.expect("dir");
    mem.set_caps_at(
        &usb,
        norte_proto::Capabilities {
            flags: CapabilityFlags::CASE_PRESERVING | CapabilityFlags::FULL_FOLD,
            max_path: None,
        },
    );

    let at = mem.capabilities_at(&usb).await.expect("responde");
    assert!(at.flags.contains(CapabilityFlags::FULL_FOLD));
    assert!(
        !mem.capabilities().flags.contains(CapabilityFlags::FULL_FOLD),
        "y el backend sigue declarando lo suyo"
    );

    // Una ubicación sin guion propio no hereda el del vecino.
    assert_eq!(
        mem.capabilities_at(&MemProvider::root())
            .await
            .expect("responde"),
        mem.capabilities()
    );
}
