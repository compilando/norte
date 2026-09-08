use super::*;

// ---------- fs.compare (C6 del plan de comparación de directorios) ----------

/// Params de `fs.compare` con los criterios por defecto (sin hash).
pub(super) fn compare_params(left: &str, right: &str) -> methods::FsCompareParams {
    methods::FsCompareParams {
        left: vp(left),
        right: vp(right),
        criteria: methods::CompareCriteria::default(),
        max_depth: None,
        mtime_tolerance_ms: 2000,
        follow_symlinks: false,
        descend_orphans: None,
    }
}

/// Drena `compare.rows` + `task.progress` de una comparación hasta su
/// terminal. Mismo criterio que [`drain_search`]: tras el terminal aún se
/// vacía brevemente lo ya encolado (las dos bombas son tasks distintas).
/// Devuelve los LOTES y el estado terminal.
pub(super) async fn drain_compare(
    c: &mut Client,
    task_id: u64,
) -> (Vec<methods::CompareRowsBatch>, TaskState) {
    let mut batches: Vec<methods::CompareRowsBatch> = Vec::new();
    let mut terminal: Option<TaskState> = None;
    loop {
        let to = if terminal.is_some() {
            Duration::from_millis(400)
        } else {
            Duration::from_secs(5)
        };
        let n = match tokio::time::timeout(to, c.notification()).await {
            Ok(Some(n)) => n,
            Ok(None) => break,
            Err(_) => {
                assert!(terminal.is_some(), "timeout esperando la comparación");
                break;
            }
        };
        if n.method == methods::COMPARE_ROWS {
            let b: methods::CompareRowsBatch =
                serde_json::from_value(n.params.expect("params")).expect("CompareRowsBatch");
            if b.task_id.get() == task_id {
                batches.push(b);
            }
        } else if n.method == methods::TASK_PROGRESS {
            let p: TaskProgress =
                serde_json::from_value(n.params.expect("params")).expect("TaskProgress");
            if p.task_id.get() == task_id && p.state.is_terminal() {
                terminal = Some(p.state);
            }
        }
    }
    (
        batches,
        terminal.expect("estado terminal de la comparación"),
    )
}

/// Round-trip por el socket: las filas llegan en lotes ACOTADOS por
/// `COMPARE_ROWS_MAX_BATCH`, coalescidos, y la Task acaba `Completed`.
#[tokio::test]
async fn fs_compare_round_trip_en_lotes_acotados() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    for i in 0..600 {
        write_file(&d.mem, &format!("mem:///l/f{i}.txt"), b"x").await;
        write_file(&d.mem, &format!("mem:///r/f{i}.txt"), b"x").await;
    }
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(methods::FS_COMPARE, &compare_params("mem:///l", "mem:///r"))
        .await
        .expect("fs.compare");
    let (batches, state) = drain_compare(&mut c, task.task_id.get()).await;
    assert_eq!(state, TaskState::Completed);
    assert!(
        batches
            .iter()
            .all(|b| b.rows.len() <= methods::COMPARE_ROWS_MAX_BATCH),
        "lote por encima del tope"
    );
    let rows: usize = batches.iter().map(|b| b.rows.len()).sum();
    assert_eq!(rows, 600, "una fila por pareja");
    assert!(batches.len() < 600, "una frame por fila no es coalescer");
    assert!(
        batches
            .iter()
            .flat_map(|b| &b.rows)
            .all(|r| r.sides_are_consistent() && r.reason_is_consistent()),
        "el daemon no puede emitir filas incoherentes"
    );
}

/// `follow_symlinks: true` es `-32602`: el motor acepta el campo y lo IGNORA,
/// y servir en silencio un recorrido distinto del pedido es peor que no
/// ofrecerlo.
#[tokio::test]
async fn fs_compare_follow_symlinks_es_invalid_params() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    let c = connected_client(&d).await;

    let mut p = compare_params("mem:///l", "mem:///r");
    p.follow_symlinks = true;
    let err = c
        .call::<_, FsTaskResult>(methods::FS_COMPARE, &p)
        .await
        .expect_err("rechazada");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS),
        other => panic!("esperaba Rpc INVALID_PARAMS, fue {other:?}"),
    }
}

/// Un lado MAL ESCRITO es `-32602` por el socket, no «ningún lado».
///
/// Quien lo rechaza es el TIPO (`DescendSide` no tiene `serde(other)`), no un
/// `if` del handler: `parse_params` no llega a construir la petición. El test
/// vive aquí igualmente porque lo que hay que garantizar es la respuesta que ve
/// el cliente, y porque si alguien ablandara el tipo a `Side` —que sí degrada—
/// este test es el que se pone rojo. `"unknown"` va en la lista a propósito: es
/// el valor que `Side` aceptaría y que significa «ningún lado».
#[tokio::test]
async fn fs_compare_un_lado_mal_escrito_es_invalid_params() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    let c = connected_client(&d).await;

    for malo in ["lft", "unknown", "both"] {
        let err = c
            .call::<_, FsTaskResult>(
                methods::FS_COMPARE,
                &serde_json::json!({
                    "left": "mem:///l",
                    "right": "mem:///r",
                    "descend_orphans": malo,
                }),
            )
            .await
            .expect_err("rechazada");
        match err {
            ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS, "{malo}"),
            other => panic!("esperaba Rpc INVALID_PARAMS para {malo}, fue {other:?}"),
        }
    }
}

// ---------------------------------------------------------------- sync.plan

pub(super) fn sync_params(source: &str, dest: &str) -> methods::SyncPlanParams {
    methods::SyncPlanParams {
        source: vp(source),
        dest: vp(dest),
        mode: methods::SyncMode::Update,
        compare: methods::SyncCompareOptions::default(),
        on_unknown: methods::OnUnknown::Copy,
        include: None,
    }
}

/// Drena `sync.steps` + `sync.plan_done` + `task.progress` de un plan hasta su
/// terminal. Devuelve los lotes EN ORDEN, el cierre (si lo hubo) y el estado.
///
/// El orden importa y por eso no se descarta: `sync.plan_done` CIERRA el plan, y
/// un lote después de él sería un cliente aprobando un hash de un plan que
/// todavía estaba llegando.
pub(super) async fn drain_sync(
    c: &mut Client,
    task_id: u64,
) -> (
    Vec<methods::SyncStepsBatch>,
    Option<methods::SyncPlanDone>,
    TaskState,
) {
    let mut batches = Vec::new();
    let mut done: Option<methods::SyncPlanDone> = None;
    let mut terminal = None;
    let mut progreso = None;
    loop {
        let next = tokio::time::timeout(Duration::from_secs(10), c.notification()).await;
        let n = match next {
            Ok(Some(n)) => n,
            Ok(None) => break,
            Err(_) => {
                assert!(terminal.is_some(), "timeout esperando el plan");
                break;
            }
        };
        if n.method == methods::SYNC_STEPS {
            let b: methods::SyncStepsBatch =
                serde_json::from_value(n.params.expect("params")).expect("SyncStepsBatch");
            if b.task_id.get() == task_id {
                assert!(
                    done.is_none(),
                    "un sync.steps DESPUÉS del sync.plan_done: el cierre tiene que ser el último"
                );
                batches.push(b);
            }
        } else if n.method == methods::SYNC_PLAN_DONE {
            let d: methods::SyncPlanDone =
                serde_json::from_value(n.params.expect("params")).expect("SyncPlanDone");
            if d.task_id.get() == task_id {
                assert!(done.is_none(), "dos cierres para un plan");
                done = Some(d);
            }
        } else if n.method == methods::TASK_PROGRESS {
            let p: TaskProgress =
                serde_json::from_value(n.params.expect("params")).expect("TaskProgress");
            if p.task_id.get() == task_id && p.state.is_terminal() {
                // El contrato de progreso de una Task `SyncPlan`, tal y como lo
                // publica su rustdoc: cuenta PASOS y no bytes, y `entries_done`
                // es la ÚNICA señal con la que un cliente detecta un `sync.steps`
                // perdido. Sin esto, las dos frases son solo prosa.
                assert_eq!(p.kind, norte_proto::TaskKind::SyncPlan);
                assert_eq!(p.bytes_done, 0, "planificar no escribe un byte");
                assert!(p.current.is_none(), "ninguna ruta al broadcast");
                progreso = Some(p.entries_done);
                terminal = Some(p.state);
                // El cierre puede ir DETRÁS del terminal: se sigue drenando
                // hasta que el timeout corto de arriba dice que no queda nada.
            }
        }
    }
    if let (Some(entries), Some(TaskState::Completed)) = (progreso, terminal.as_ref()) {
        let vistos: u64 = batches
            .iter()
            .map(|b| u64::try_from(b.steps.len()).expect("cabe"))
            .sum();
        assert_eq!(
            entries, vistos,
            "entries_done tiene que cuadrar con los pasos entregados"
        );
    }
    (batches, done, terminal.expect("estado terminal del plan"))
}

/// Round-trip por el socket: los pasos llegan en lotes acotados y el
/// `sync.plan_done` los CIERRA — nunca al revés.
#[tokio::test]
async fn sync_plan_round_trip_pasos_y_despues_el_cierre() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    d.mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    for i in 0..600 {
        write_file(&d.mem, &format!("mem:///s/f{i}.txt"), b"x").await;
    }
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(methods::SYNC_PLAN, &sync_params("mem:///s", "mem:///d"))
        .await
        .expect("sync.plan");
    let (batches, done, state) = drain_sync(&mut c, task.task_id.get()).await;
    assert_eq!(state, TaskState::Completed);
    assert!(
        batches
            .iter()
            .all(|b| b.steps.len() <= methods::SYNC_STEPS_MAX_BATCH),
        "lote por encima del tope"
    );
    let steps: usize = batches.iter().map(|b| b.steps.len()).sum();
    assert_eq!(steps, 600);
    assert!(batches.len() < 600, "una frame por paso no es coalescer");
    let done = done.expect("el plan cerró");
    assert_eq!(done.counts.copy, 600);
    assert!(done.executable);
    assert_eq!(done.plan_hash.as_str().len(), methods::PLAN_HASH_LEN);
}

/// Los dos campos de `compare` que no son del llamante, y el tope de `include`:
/// `-32602` SIN crear Task.
#[tokio::test]
async fn sync_plan_params_que_no_son_del_llamante_son_invalid_params() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    d.mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    let c = connected_client(&d).await;

    let mut p = sync_params("mem:///s", "mem:///d");
    p.compare.follow_symlinks = true;
    assert_invalid_params(c.call::<_, FsTaskResult>(methods::SYNC_PLAN, &p).await);

    let mut p = sync_params("mem:///s", "mem:///d");
    p.compare.descend_orphans = Some(methods::DescendSide::Right);
    assert_invalid_params(c.call::<_, FsTaskResult>(methods::SYNC_PLAN, &p).await);

    let mut p = sync_params("mem:///s", "mem:///d");
    p.include = Some(vec![
        methods::RelPath::parse_wire("x").expect("rel");
        methods::SYNC_MAX_INCLUDE + 1
    ]);
    assert_invalid_params(c.call::<_, FsTaskResult>(methods::SYNC_PLAN, &p).await);
}

/// Raíces solapadas: categoría del wire (`OverlappingRoots`), no `-32602`, y con
/// la relación dentro — un frontend pinta las tres distinto.
#[tokio::test]
async fn sync_plan_raices_solapadas_viajan_con_su_relacion() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///a")).await.expect("mkdir a");
    d.mem.mkdir(&vp("mem:///a/sub")).await.expect("mkdir sub");
    let c = connected_client(&d).await;

    let err = c
        .call::<_, FsTaskResult>(methods::SYNC_PLAN, &sync_params("mem:///a", "mem:///a/sub"))
        .await
        .expect_err("solapadas");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(
                rpc.data,
                Some(Error::OverlappingRoots {
                    relation: norte_proto::RootOverlap::DestInsideSource
                })
            ),
            "fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// Un hash que este daemon no emitió jamás es `PlanStale`: no existe plan vivo
/// con ese nombre, y esa es la única cosa que la respuesta dice.
#[tokio::test]
async fn sync_apply_de_un_hash_que_nadie_emitio_es_plan_stale() {
    let d = spawn_daemon_journal().await;
    let c = connected_client(&d).await;

    let inventado =
        methods::PlanHash::parse(&"0".repeat(methods::PLAN_HASH_LEN)).expect("hex válido");
    let err = c
        .call::<_, FsTaskResult>(
            methods::SYNC_APPLY,
            &methods::SyncApplyParams {
                plan_hash: inventado,
            },
        )
        .await
        .expect_err("nadie emitió ese plan");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PlanStale)),
            "PlanStale, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// Un hash MALFORMADO muere en el deserializador (`-32602`) y no se disfraza de
/// `PlanStale`: «esto no es un hash» y «el mundo se movió» son hechos distintos,
/// y contestar el segundo a quien mandó el primero le miente sobre el estado del
/// mundo.
#[tokio::test]
async fn sync_apply_con_hash_malformado_es_invalid_params_y_no_plan_stale() {
    let d = spawn_daemon_journal().await;
    let c = connected_client(&d).await;

    // Ni hexadecimal, ni de la longitud correcta, ni en minúsculas: las tres
    // formas de no ser un `PlanHash`.
    for basura in [
        serde_json::json!({"plan_hash": "nope"}),
        serde_json::json!({"plan_hash": "0".repeat(methods::PLAN_HASH_LEN - 1)}),
        serde_json::json!({"plan_hash": "A".repeat(methods::PLAN_HASH_LEN)}),
        serde_json::json!({}),
    ] {
        assert_invalid_params(
            c.call::<_, FsTaskResult>(methods::SYNC_APPLY, &basura)
                .await,
        );
    }
}

/// El plan de OTRA conexión es `PlanStale`, no una categoría propia: el plan
/// está atado a la conexión que lo produjo, y contestar algo distinto de «no hay
/// plan vivo con ese hash» construiría un oráculo de existencia sobre los planes
/// ajenos — que son autorizaciones de escritura.
#[tokio::test]
async fn sync_apply_de_un_plan_de_otra_conexion_es_plan_stale() {
    let d = spawn_daemon_journal().await;
    let mut duena = connected_client(&d).await;
    let done = plan_sobre(&d, &mut duena).await;
    let ajena = connected_client(&d).await;

    let err = ajena
        .call::<_, FsTaskResult>(
            methods::SYNC_APPLY,
            &methods::SyncApplyParams {
                plan_hash: done.plan_hash.clone(),
            },
        )
        .await
        .expect_err("el plan no es suyo");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PlanStale)),
            "PlanStale, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
    // Y la dueña sí puede: el rechazo era de la conexión, no del plan.
    let _: FsTaskResult = duena
        .call(
            methods::SYNC_APPLY,
            &methods::SyncApplyParams {
                plan_hash: done.plan_hash,
            },
        )
        .await
        .expect("su propio plan sí");
}

/// Un plan se aprueba UNA vez: al terminar la aplicación —en cualquier estado—
/// el plan retenido se ha ido, así que el mismo hash ya no ejecuta nada. Sin
/// esto, un hash filtrado sería una autorización de escritura reutilizable.
#[tokio::test]
async fn el_spool_se_gasta_en_cuanto_la_aplicacion_termina() {
    let d = spawn_daemon_journal().await;
    let mut c = connected_client(&d).await;
    let done = plan_sobre(&d, &mut c).await;

    let task: FsTaskResult = c
        .call(
            methods::SYNC_APPLY,
            &methods::SyncApplyParams {
                plan_hash: done.plan_hash.clone(),
            },
        )
        .await
        .expect("sync.apply aceptado");
    assert_eq!(wait_terminal(&c, task.task_id).await, TaskState::Completed);

    let err = c
        .call::<_, FsTaskResult>(
            methods::SYNC_APPLY,
            &methods::SyncApplyParams {
                plan_hash: done.plan_hash,
            },
        )
        .await
        .expect_err("un plan se aprueba una vez");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PlanStale)),
            "PlanStale, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}
