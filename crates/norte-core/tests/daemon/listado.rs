use super::*;

// ---------- dispatch fs.* ----------

#[tokio::test]
async fn fs_list_y_stat_responden_por_el_socket() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///f.txt", b"hola").await;
    let c = connected_client(&d).await;

    let list: FsListResult = c
        .call(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
                attrs: Vec::new(),
            },
        )
        .await
        .expect("fs.list");
    assert_eq!(list.entries.len(), 1);

    let stat: FsStatResult = c
        .call(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("mem:///f.txt"),
                attrs: Vec::new(),
            },
        )
        .await
        .expect("fs.stat");
    assert_eq!(stat.entry.size, Some(4));
}

// ---------- attrs por el wire (#108 bloque 2, ADR 0039) ----------
// (Sustituye al pin del bloque 1 «el daemon ignora los ids pedidos»: desde
// este bloque el daemon valida, cruza con lo anunciado y materializa.)

pub(super) fn assert_rpc_code(err: &ClientError, code: i64) {
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, code, "código RPC: {rpc:?}"),
        other => panic!("esperaba error RPC {code}, fue {other:?}"),
    }
}

/// Daemon con `MemProvider` de attrs sintéticos: el catálogo llega por
/// `fs.capabilities` (saneado por `AttrCatalog::new`) y `fs.list`/`fs.stat`
/// materializan lo pedido∩anunciado.
pub(super) async fn spawn_daemon_attrs() -> TestDaemon {
    spawn_daemon_mem(
        None,
        Duration::from_mins(2),
        MemProvider::new().with_synthetic_attrs(),
    )
    .await
}

#[tokio::test]
async fn fs_capabilities_publica_el_catalogo_del_provider() {
    let d = spawn_daemon_attrs().await;
    let c = connected_client(&d).await;
    let r: methods::FsCapabilitiesResult = c
        .call(
            methods::FS_CAPABILITIES,
            &methods::FsCapabilitiesParams {
                path: vp("mem:///"),
            },
        )
        .await
        .expect("fs.capabilities");
    assert!(
        r.attrs.iter().any(|a| a.id == "mem.owner"),
        "catálogo publicado: {:?}",
        r.attrs
    );
}

#[tokio::test]
async fn fs_list_attrs_malformado_o_sobre_tope_es_invalid_params() {
    let d = spawn_daemon_attrs().await;
    let c = connected_client(&d).await;
    // Id malformado (mayúsculas) → -32602.
    let err = c
        .call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
                attrs: vec!["MAYUS.no".into()],
            },
        )
        .await
        .expect_err("id malformado debe ser error");
    assert_rpc_code(&err, codes::INVALID_PARAMS);
    // 17 ids válidos (el deserializador materializa 16+1 como testigo).
    let err = c
        .call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
                attrs: (0..17).map(|i| format!("a.b{i}")).collect(),
            },
        )
        .await
        .expect_err("sobre-tope debe ser error");
    assert_rpc_code(&err, codes::INVALID_PARAMS);
}

#[tokio::test]
async fn fs_list_paginado_conserva_los_attrs_del_arranque() {
    let d = spawn_daemon_attrs().await;
    seed(&d.mem, 3).await;
    let c = connected_client(&d).await;
    // Primera página CON attrs; continuaciones SIN re-mandarlos.
    let mut r: FsListResult = c
        .call(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: Some(1),
                cursor: None,
                attrs: vec!["mem.mode".into()],
            },
        )
        .await
        .expect("fs.list");
    let mut total = 0;
    loop {
        // TODA entrada de TODA página lleva el attr del arranque (la última
        // página puede venir vacía: el stream no sabe que acabó hasta
        // drenarla).
        for e in &r.entries {
            assert!(
                e.attrs.contains_key("mem.mode"),
                "entrada sin mem.mode: {e:?}"
            );
        }
        total += r.entries.len();
        let Some(cursor) = r.next_cursor.clone() else {
            break;
        };
        // La continuación no re-manda attrs: el stream retenido ya los lleva.
        r = list_page(&c, "mem:///", Some(1), Some(cursor)).await;
    }
    assert_eq!(total, 3);
}

/// `limit = 0` es error de params (evita páginas vacías en bucle).
#[tokio::test]
async fn fs_list_limit_cero_es_invalid_params() {
    let d = spawn_daemon(None).await;
    seed(&d.mem, 2).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: Some(0),
                cursor: None,
                attrs: Vec::new(),
            },
        )
        .await
        .expect_err("limit 0");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}

/// Un cursor desconocido (o no-numérico) → `CursorExpired`: el cliente reinicia.
#[tokio::test]
async fn fs_list_cursor_desconocido_es_cursor_expired() {
    let d = spawn_daemon(None).await;
    seed(&d.mem, 2).await;
    let c = connected_client(&d).await;
    for cur in ["999", "no-numerico"] {
        let err = c
            .call::<_, FsListResult>(
                methods::FS_LIST,
                &FsListParams {
                    path: vp("mem:///"),
                    limit: Some(1),
                    cursor: Some(cur.to_string()),
                    attrs: Vec::new(),
                },
            )
            .await
            .expect_err("cursor inválido");
        match err {
            ClientError::Rpc(rpc) => {
                assert_eq!(rpc.data, Some(norte_proto::Error::CursorExpired), "{cur}");
            }
            other => panic!("esperaba Rpc, fue {other:?}"),
        }
    }
}

/// Un cursor de OTRO path → `INVALID_PARAMS` (el cursor valida contra su dir).
#[tokio::test]
async fn fs_list_cursor_de_otro_path_es_invalid_params() {
    let d = spawn_daemon(None).await;
    seed(&d.mem, 3).await;
    let c = connected_client(&d).await;
    // Abre un listado de la raíz que RETIENE (limit 1, hay 3 entradas).
    let page = list_page(&c, "mem:///", Some(1), None).await;
    let cur = page.next_cursor.expect("retiene");
    // Continuarlo con OTRO path: el check de path va ANTES de listar, así que
    // el otro path ni siquiera necesita existir.
    let err = c
        .call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///otro"),
                limit: Some(1),
                cursor: Some(cur),
                attrs: Vec::new(),
            },
        )
        .await
        .expect_err("cursor de otro path");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}

/// LRU: al abrir el 9º listado retenido se expulsa el más viejo (su cursor →
/// `CursorExpired`).
#[tokio::test]
async fn fs_list_lru_expulsa_el_mas_viejo() {
    let d = spawn_daemon(None).await;
    seed(&d.mem, 3).await; // ≥2 para que cada limit=1 retenga
    let c = connected_client(&d).await;
    // Abre 9 listados (MAX_OPEN_LISTINGS = 8): el 9º expulsa el 1º.
    let mut cursores = Vec::new();
    for _ in 0..9 {
        let page = list_page(&c, "mem:///", Some(1), None).await;
        cursores.push(page.next_cursor.expect("retiene"));
    }
    // El primer cursor fue expulsado.
    let err = c
        .call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: Some(1),
                cursor: Some(cursores[0].clone()),
                attrs: Vec::new(),
            },
        )
        .await
        .expect_err("el 1º fue expulsado");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.data, Some(norte_proto::Error::CursorExpired));
        }
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
    // El último sigue vivo.
    let ok = list_page(&c, "mem:///", Some(1), Some(cursores[8].clone())).await;
    assert!(!ok.entries.is_empty(), "el más reciente sobrevive");
}

/// TTL: un listado retenido sin continuar caduca (su cursor → `CursorExpired`).
#[tokio::test]
async fn fs_list_ttl_expira_el_listado() {
    let d = spawn_daemon_ttl(None, Duration::from_millis(150)).await;
    seed(&d.mem, 3).await;
    let c = connected_client(&d).await;
    let page = list_page(&c, "mem:///", Some(1), None).await;
    let cur = page.next_cursor.expect("retiene");
    // Un temporizador DEL SISTEMA BAJO PRUEBA, no una espera nuestra: lo que
    // este test comprueba es que el TTL de 150 ms caduca el listado, así que
    // hay que dejar pasar ese tiempo. No se puede sondear —caducar es dejar de
    // estar— ni saltar con reloj virtual: el daemon corre en su propio runtime
    // y sus temporizadores no los controla el test.
    //
    // Hacerlo determinista pide inyectar el reloj en el daemon, que es cambio
    // de producción y no lo vale por un test. Los 400 ms son 2,6× el plazo.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let err = c
        .call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: Some(1),
                cursor: Some(cur),
                attrs: Vec::new(),
            },
        )
        .await
        .expect_err("caducó por TTL");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.data, Some(norte_proto::Error::CursorExpired));
        }
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// B1 del rust-reviewer: una `call()` DESPUÉS de morir la conexión falla
/// con `ConnectionClosed` en vez de colgarse para siempre.
#[tokio::test]
async fn call_tras_el_cierre_no_se_cuelga() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    // Apagar el daemon deja la conexión muerta.
    let _: DaemonShutdownResult = c
        .call(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams {
                graceful: true,
                ..Default::default()
            },
        )
        .await
        .expect("shutdown");
    tokio::time::timeout(Duration::from_secs(5), d.run)
        .await
        .expect("apagado")
        .expect("join")
        .expect("run ok");
    // Sin margen fijo: lo que este test afirma es que la llamada CONTESTA y
    // jamás se cuelga, y eso ya lo sostiene el `timeout` de abajo. Dormir
    // antes solo hacía que el caso interesante —llamar ANTES de que el reader
    // vea el EOF— nunca se probara.
    let err = tokio::time::timeout(
        Duration::from_secs(5),
        c.call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
                attrs: Vec::new(),
            },
        ),
    )
    .await
    .expect("responde, JAMÁS se cuelga")
    .expect_err("la conexión está muerta");
    assert!(
        matches!(err, ClientError::ConnectionClosed | ClientError::Io(_)),
        "{err:?}"
    );
}

/// `fs.capabilities`: el frontend decide (F8 papelera, ADR 0009) sin
/// lógica propia.
#[tokio::test]
async fn fs_capabilities_viaja_por_el_socket() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let r: methods::FsCapabilitiesResult = c
        .call(
            methods::FS_CAPABILITIES,
            &methods::FsCapabilitiesParams {
                path: vp("mem:///"),
            },
        )
        .await
        .expect("fs.capabilities");
    assert!(
        r.capabilities
            .flags
            .contains(norte_proto::CapabilityFlags::TRASH),
        "MemProvider declara TRASH: {:?}",
        r.capabilities.flags
    );
}

/// #53 (M2, regla 3 + DoD): cerrar una conexión con listings retenidos los
/// SUELTA (RAII: drop de `ConnState` → drop de `OpenListing` → guard
/// decrementa el tope global y muere el productor). Observable extremo-a-
/// extremo por la degradación del tope global: saturado, `fs.list` paginado
/// degrada a listado-completo (`next_cursor=None`); liberado, vuelve a
/// paginar.
#[tokio::test]
async fn cerrar_conexion_libera_sus_listings_retenidos() {
    let d = spawn_daemon(None).await;
    for i in 0..3u32 {
        write_file(&d.mem, &format!("mem:///f{i}.bin"), b"x").await;
    }
    let page = |c: &'static str| FsListParams {
        path: vp(c),
        limit: Some(1),
        cursor: None,
        attrs: Vec::new(),
    };

    // Satura el tope GLOBAL (256): 32 conexiones × 8 listings retenidos.
    let mut hoarders = Vec::new();
    for _ in 0..32 {
        let c = connected_client(&d).await;
        for _ in 0..8 {
            let r: FsListResult = c
                .call(methods::FS_LIST, &page("mem:///"))
                .await
                .expect("fs.list");
            assert!(r.next_cursor.is_some(), "retenido (aún bajo el tope)");
        }
        hoarders.push(c);
    }
    // Saturado: una página nueva DEGRADA a listado-completo (no retiene).
    let probe = connected_client(&d).await;
    let r: FsListResult = probe
        .call(methods::FS_LIST, &page("mem:///"))
        .await
        .expect("fs.list degradado");
    assert!(r.next_cursor.is_none(), "saturado degrada a completo");
    assert_eq!(r.entries.len(), 3, "degradado = TODO el listado");

    // Cae UNA conexión acaparadora: sus 8 listings deben soltarse (RAII).
    drop(hoarders.pop());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let r: FsListResult = probe
            .call(methods::FS_LIST, &page("mem:///"))
            .await
            .expect("fs.list tras liberar");
        if r.next_cursor.is_some() {
            break; // volvió a paginar: el tope global bajó — liberado.
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "los listings de la conexión muerta no se liberaron"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Criterios vacíos: `INVALID_PARAMS` con detalle y SIN crear Task.
#[tokio::test]
async fn fs_search_params_invalidos_no_crean_task() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let params = FsSearchParams {
        root: vp("mem:///"),
        name_glob: None,
        name_regex: None,
        content: None,
        content_regex: None,
        case_sensitive: false,
        max_hits: None,
    };
    let err = c
        .call::<_, FsTaskResult>(methods::FS_SEARCH, &params)
        .await
        .expect_err("sin criterios");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS),
        other => panic!("esperaba Rpc INVALID_PARAMS, fue {other:?}"),
    }
    // Ninguna task viva ni reciente: la validación falló ANTES del submit.
    let list: FsListResult = c
        .call(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
                attrs: Vec::new(),
            },
        )
        .await
        .expect("fs.list");
    let _ = list; // (sin entradas sembradas)
    let tasks: methods::TaskListResult = c
        .call(methods::TASK_LIST, &methods::TaskListParams::default())
        .await
        .expect("task.list");
    assert!(tasks.tasks.is_empty(), "no se creó ninguna Task");
}

/// m2 (protocol-guardian #108-b2): garantía del bloque 1 que sigue viva —
/// pedir un id BIEN FORMADO a un provider con catálogo VACÍO sale bien en
/// `fs.list` (no -32602) y las entradas vienen peladas.
#[tokio::test]
async fn fs_list_id_valido_sobre_catalogo_vacio_no_es_error() {
    let d = spawn_daemon(None).await; // MemProvider SIN attrs sintéticos
    write_file(&d.mem, "mem:///f.txt", b"hola").await;
    let c = connected_client(&d).await;
    let list: FsListResult = c
        .call(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
                attrs: vec!["posix.mode".into()],
            },
        )
        .await
        .expect("id válido sobre catálogo vacío jamás es error");
    assert_eq!(list.entries.len(), 1);
    for e in &list.entries {
        assert!(e.attrs.is_empty(), "catálogo vacío = entradas peladas");
    }
}
