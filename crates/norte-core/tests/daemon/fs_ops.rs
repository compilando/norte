use super::*;

#[tokio::test]
async fn fs_stat_devuelve_solo_lo_pedido_y_anunciado() {
    let d = spawn_daemon_attrs().await;
    write_file(&d.mem, "mem:///f.txt", b"hola").await;
    let c = connected_client(&d).await;
    let r: FsStatResult = c
        .call(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("mem:///f.txt"),
                attrs: vec!["mem.mode".into(), "zz.desconocido".into()],
            },
        )
        .await
        .expect("fs.stat");
    assert!(
        matches!(
            r.entry.attrs.get("mem.mode"),
            Some(norte_proto::AttrValue::Uint(_))
        ),
        "mem.mode materializado: {:?}",
        r.entry.attrs
    );
    // Id válido pero no anunciado: AUSENTE, jamás error.
    assert!(!r.entry.attrs.contains_key("zz.desconocido"));
    assert_eq!(r.entry.attrs.len(), 1);
}

#[tokio::test]
async fn call_tracked_reporta_el_id_asignado() {
    let d = spawn_daemon(None).await;
    let client = connected_client(&d).await;
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
    let s = std::sync::Arc::clone(&seen);
    // fs.stat de un path inexistente: da igual el desenlace (Err), lo que se
    // comprueba es que on_id se invocó exactamente una vez con un id > 0.
    let _res: Result<norte_proto::methods::FsStatResult, _> = client
        .call_tracked(
            norte_proto::methods::FS_STAT,
            &norte_proto::methods::FsStatParams {
                path: vp("mem:///nope"),
                attrs: Vec::new(),
            },
            move |id| s.lock().expect("lock").push(id),
        )
        .await;
    let seen = seen.lock().expect("lock");
    assert_eq!(seen.len(), 1, "on_id se invoca exactamente una vez");
    assert!(seen[0] > 0, "id asignado > 0");
}

// ---------- paginación de fs.list (ADR 0017) ----------

/// Una página de `fs.list` con `limit`/`cursor`.
pub(super) async fn list_page(
    c: &Client,
    path: &str,
    limit: Option<u32>,
    cursor: Option<String>,
) -> FsListResult {
    c.call(
        methods::FS_LIST,
        &FsListParams {
            path: vp(path),
            limit,
            cursor,
            attrs: Vec::new(),
        },
    )
    .await
    .expect("fs.list")
}

pub(super) async fn seed(mem: &MemProvider, n: usize) {
    for i in 0..n {
        write_file(mem, &format!("mem:///f{i:03}.txt"), b"x").await;
    }
}

/// Paginar por cursor devuelve EXACTAMENTE las mismas entradas que el listado
/// completo, sin duplicar ni perder.
#[tokio::test]
async fn fs_list_paginado_concatena_igual_que_completo() {
    let d = spawn_daemon(None).await;
    seed(&d.mem, 5).await;
    let c = connected_client(&d).await;

    let completo = list_page(&c, "mem:///", None, None).await;
    assert_eq!(completo.entries.len(), 5);
    assert!(
        completo.next_cursor.is_none(),
        "sin cursor = listado completo"
    );

    // Páginas de 2.
    let mut acumulado = Vec::new();
    let mut cursor = None;
    loop {
        let page = list_page(&c, "mem:///", Some(2), cursor).await;
        assert!(page.entries.len() <= 2, "respeta el limit");
        acumulado.extend(page.entries);
        match page.next_cursor {
            Some(cur) => cursor = Some(cur),
            None => break,
        }
    }
    // Mismo conjunto de paths (el orden del provider puede variar).
    let mut a: Vec<_> = acumulado.iter().map(|e| e.path.clone()).collect();
    let mut b: Vec<_> = completo.entries.iter().map(|e| e.path.clone()).collect();
    a.sort();
    b.sort();
    assert_eq!(a, b, "paginado == completo");
}

/// #93: `skipped` (omitidas del contenedor) viaja en el result de `fs.list` y
/// se REPITE en cada página (el cliente puede engancharse en cualquiera). Con
/// un provider normal (sin omitidas) el campo va ausente (`None`).
#[tokio::test]
async fn fs_list_skipped_viaja_en_todas_las_paginas() {
    // Daemon con un MemProvider que simula un contenedor con 7 omitidas.
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let engine = Arc::new(Engine::new());
    let mem = Arc::new(MemProvider::new().with_list_skipped(7));
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: Some(dir.path().to_path_buf()),
            state_dir: None,
        },
    )
    .await
    .expect("bind");
    let run = tokio::spawn(daemon.run());
    let d = TestDaemon {
        socket,
        run,
        dir,
        mem,
    };
    seed(&d.mem, 5).await;
    let c = connected_client(&d).await;

    // Listado completo (sin cursor): lo lleva.
    let completo = list_page(&c, "mem:///", None, None).await;
    assert_eq!(completo.skipped, Some(7));

    // Paginado: TODAS las páginas lo repiten (primera, intermedias y última).
    let mut cursor = None;
    let mut paginas = 0;
    loop {
        let page = list_page(&c, "mem:///", Some(2), cursor).await;
        assert_eq!(page.skipped, Some(7), "página {paginas}");
        paginas += 1;
        match page.next_cursor {
            Some(cur) => cursor = Some(cur),
            None => break,
        }
    }
    assert!(paginas >= 3, "hubo continuaciones de verdad");

    // Provider sin omitidas (spawn normal): el campo va ausente.
    let d2 = spawn_daemon(None).await;
    seed(&d2.mem, 1).await;
    let c2 = connected_client(&d2).await;
    assert_eq!(list_page(&c2, "mem:///", None, None).await.skipped, None);
}

#[tokio::test]
async fn fs_stat_de_inexistente_viaja_como_taxonomia_en_data() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, FsStatResult>(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("mem:///nada"),
                attrs: Vec::new(),
            },
        )
        .await
        .expect_err("no existe");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert_eq!(rpc.data, Some(norte_proto::Error::NotFound));
        }
        other => panic!("esperaba Rpc con data, fue {other:?}"),
    }
}

/// H1 (encoding review #108-b2): los valores HOSTILES cruzan el socket de
/// verdad — `Bytes` no-UTF-8 byte-exacto tras `encode(bytes_b64)+decode`, y
/// `Text` con RTL override/ZWJ char-exacto tras el cinturón de emisión.
#[tokio::test]
async fn attrs_hostiles_cruzan_el_socket_byte_exactos() {
    let d = spawn_daemon_attrs().await;
    write_file(&d.mem, "mem:///f.txt", b"x").await;
    let c = connected_client(&d).await;
    let r: FsStatResult = c
        .call(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("mem:///f.txt"),
                attrs: vec!["mem.owner".into(), "mem.note".into()],
            },
        )
        .await
        .expect("fs.stat");
    assert_eq!(
        r.entry.attrs.get("mem.owner"),
        Some(&norte_proto::AttrValue::Bytes(
            b"due\xf1o-\xff\xfe".to_vec()
        )),
        "bytes crudos byte-exactos tras el wire"
    );
    assert_eq!(
        r.entry.attrs.get("mem.note"),
        Some(&norte_proto::AttrValue::Text(
            "\u{202e}atón\u{202c} a\u{200d}b".to_owned()
        )),
        "texto hostil char-exacto tras el wire"
    );
}
