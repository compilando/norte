use super::*;

/// El informe de `archive.test` es de quien lanzó la Task: un id ajeno se
/// contesta igual que uno que no existió nunca, que es lo que hacen sus dos
/// gemelos (`fs.rename_batch_report`, `sync.report`).
#[tokio::test]
async fn el_informe_de_un_test_de_archivo_no_es_de_cualquiera() {
    let d = spawn_daemon(None).await;
    let humano = connected_client(&d).await;
    let err = humano
        .call::<_, methods::ArchiveTestResult>(
            methods::ARCHIVE_TEST_REPORT,
            &methods::ArchiveTestReportParams {
                task_id: norte_proto::TaskId::new(4242),
            },
        )
        .await
        .expect_err("ese id nunca fue un test");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(
                rpc.data,
                Some(norte_proto::Error::NotFound),
                "{:?}",
                rpc.data
            );
        }
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// #250 — el informe de un empaquetado tiene la MISMA disciplina que el de un
/// test de archivo: un id que jamás fue un `archive.pack` se contesta
/// `NotFound`, que es también lo que se contesta a uno ajeno.
///
/// Existe porque el handler es un gemelo del de al lado y «es un gemelo» no es
/// evidencia: lo que aquí se fija es que el método está CABLEADO en el dispatch
/// —renombrarlo o no enrutarlo pasaba la suite entera— y que su respuesta a lo
/// desconocido no filtra existencia.
#[tokio::test]
async fn el_informe_de_un_empaquetado_no_es_de_cualquiera() {
    let d = spawn_daemon(None).await;
    let humano = connected_client(&d).await;
    let err = humano
        .call::<_, methods::ArchivePackReportResult>(
            methods::ARCHIVE_PACK_REPORT,
            &methods::ArchivePackReportParams {
                task_id: norte_proto::TaskId::new(4242),
            },
        )
        .await
        .expect_err("ese id nunca fue un empaquetado");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(
                rpc.data,
                Some(norte_proto::Error::NotFound),
                "{:?}",
                rpc.data
            );
        }
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

// ---------- tasks: progreso, terminal, cancel, broadcast ----------

/// Espera notificaciones task.progress de una task hasta su estado
/// terminal; devuelve los snapshots vistos.
pub(super) async fn drain_task(c: &mut Client, task_id: u64) -> Vec<TaskProgress> {
    let mut seen = Vec::new();
    loop {
        let n = tokio::time::timeout(Duration::from_secs(5), c.notification())
            .await
            .expect("notificación antes del timeout")
            .expect("conexión viva");
        assert_eq!(n.method, methods::TASK_PROGRESS);
        let p: TaskProgress =
            serde_json::from_value(n.params.expect("params")).expect("TaskProgress");
        if p.task_id.get() != task_id {
            continue;
        }
        let terminal = p.state.is_terminal();
        seen.push(p);
        if terminal {
            return seen;
        }
    }
}

#[tokio::test]
async fn fs_copy_progresa_hasta_completed() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///src.bin", &vec![0xAB; 5000]).await;
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///src.bin"),
                to: vp("mem:///dst.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
            },
        )
        .await
        .expect("fs.copy");
    let seen = drain_task(&mut c, task.task_id.get()).await;
    let last = seen.last().expect("al menos el terminal");
    assert_eq!(last.state, TaskState::Completed);
    assert_eq!(last.bytes_done, 5000);
    let stat: FsStatResult = c
        .call(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("mem:///dst.bin"),
                attrs: Vec::new(),
            },
        )
        .await
        .expect("el destino existe");
    assert_eq!(stat.entry.size, Some(5000));
}

#[tokio::test]
async fn task_cancel_por_el_socket_cancela_limpio() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///grande.bin", &vec![0xCD; 100_000]).await;
    // Latencia por op: da tiempo a cancelar en mitad.
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(30)));
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///grande.bin"),
                to: vp("mem:///copia.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
            },
        )
        .await
        .expect("fs.copy");
    let _: TaskCancelResult = c
        .call(
            methods::TASK_CANCEL,
            &TaskCancelParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("task.cancel");
    let seen = drain_task(&mut c, task.task_id.get()).await;
    assert_eq!(
        seen.last().expect("terminal").state,
        TaskState::Cancelled,
        "cancelación cooperativa confirmada por notificación"
    );
}

/// La base de la fase 3: un SEGUNDO cliente ve el progreso de las tasks
/// que encoló el primero.
#[tokio::test]
async fn el_progreso_se_difunde_a_todos_los_clientes() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///src.bin", &vec![0xEE; 2000]).await;
    let c1 = connected_client(&d).await;
    let mut c2 = connected_client(&d).await;

    let task: FsTaskResult = c1
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///src.bin"),
                to: vp("mem:///dst.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
            },
        )
        .await
        .expect("fs.copy de c1");
    // c2 no pidió nada — y aun así ve la task de c1 hasta el terminal.
    let seen = drain_task(&mut c2, task.task_id.get()).await;
    assert_eq!(seen.last().expect("terminal").state, TaskState::Completed);
}

// ---------- métodos 0.5.0 (fase 3) ----------

/// `task.list` devuelve el snapshot de las tasks VIVAS: el resync de un
/// frontend que se conecta tarde.
#[tokio::test]
async fn task_list_da_el_snapshot_de_tasks_vivas() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///grande.bin", &vec![0xAB; 50_000]).await;
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(20)));
    let c1 = connected_client(&d).await;
    let task: FsTaskResult = c1
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///grande.bin"),
                to: vp("mem:///copia.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
            },
        )
        .await
        .expect("fs.copy");
    // Cliente TARDÍO: ve la task del primero por task.list.
    let mut c2 = connected_client(&d).await;
    let list: methods::TaskListResult = c2
        .call(methods::TASK_LIST, &methods::TaskListParams {})
        .await
        .expect("task.list");
    assert!(
        list.tasks.iter().any(|t| t.task_id == task.task_id),
        "la task viva del otro cliente aparece: {list:?}"
    );
    // Y sigue viéndola progresar hasta el terminal por broadcast.
    let seen = drain_task(&mut c2, task.task_id.get()).await;
    assert_eq!(seen.last().expect("terminal").state, TaskState::Completed);
}

/// `fs.read` con rango: bytes exactos en base64 y eof honesto.
#[tokio::test]
async fn fs_read_devuelve_tramos_con_eof_honesto() {
    use base64::Engine as _;
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///f.bin", b"0123456789").await;
    let c = connected_client(&d).await;

    let r: methods::FsReadResult = c
        .call(
            methods::FS_READ,
            &methods::FsReadParams {
                path: vp("mem:///f.bin"),
                range: Some(norte_proto::ByteRange {
                    offset: 2,
                    len: Some(3),
                }),
            },
        )
        .await
        .expect("fs.read");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&r.content_b64)
        .expect("base64 válido");
    assert_eq!(bytes, b"234");
    assert!(!r.eof, "quedan bytes detrás del tramo");

    // Tramo hasta el final: eof true.
    let r: methods::FsReadResult = c
        .call(
            methods::FS_READ,
            &methods::FsReadParams {
                path: vp("mem:///f.bin"),
                range: Some(norte_proto::ByteRange {
                    offset: 5,
                    len: Some(100),
                }),
            },
        )
        .await
        .expect("fs.read");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&r.content_b64)
        .expect("base64 válido");
    assert_eq!(bytes, b"56789");
    assert!(r.eof);

    // Sin rango: el archivo entero (cabe de sobra en el tope).
    let r: methods::FsReadResult = c
        .call(
            methods::FS_READ,
            &methods::FsReadParams {
                path: vp("mem:///f.bin"),
                range: None,
            },
        )
        .await
        .expect("fs.read");
    assert!(r.eof);
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(&r.content_b64)
            .expect("base64"),
        b"0123456789"
    );
}

/// `task.cancel` de un AGENTE sobre una task ajena: ack (el contrato no
/// filtra existencia) pero SIN efecto — la copia del humano completa.
#[tokio::test]
async fn task_cancel_de_agente_no_toca_task_del_humano() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///grande.bin", &vec![0xCD; 100_000]).await;
    // Latencia por op GENEROSA (la copia son ~4-6 ops, no proporcional al
    // tamaño): la task sigue viva cuando llega el cancel hostil incluso en
    // un runner cargado — si terminara antes, el test pasaría en vacío.
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(500)));
    let mut human = connected_client(&d).await;
    let agent = connected_agent(&d, "sess-cancel").await;

    let task: FsTaskResult = human
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///grande.bin"),
                to: vp("mem:///copia.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
            },
        )
        .await
        .expect("fs.copy del humano");
    let _: TaskCancelResult = agent
        .call(
            methods::TASK_CANCEL,
            &TaskCancelParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("ack: no se filtra existencia de tasks ajenas");
    let seen = drain_task(&mut human, task.task_id.get()).await;
    assert_eq!(
        seen.last().expect("terminal").state,
        TaskState::Completed,
        "la cancelación de un agente sobre una task ajena NO surte efecto"
    );
}

/// Un agente SÍ cancela su propia task (el gate no bloquea de más).
#[tokio::test]
async fn task_cancel_de_agente_cancela_la_suya() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///grande.bin", &vec![0xEF; 100_000]).await;
    // Latencia generosa: si la copia completara antes del cancel, el
    // terminal sería Completed y el test fallaría por timing, no por gate.
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(500)));
    let mut agent = connected_agent(&d, "sess-own").await;

    let task: FsTaskResult = agent
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///grande.bin"),
                to: vp("mem:///copia.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
            },
        )
        .await
        .expect("fs.copy del agente");
    let _: TaskCancelResult = agent
        .call(
            methods::TASK_CANCEL,
            &TaskCancelParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("task.cancel propio");
    let seen = drain_task(&mut agent, task.task_id.get()).await;
    assert_eq!(
        seen.last().expect("terminal").state,
        TaskState::Cancelled,
        "cancelar lo propio sigue funcionando"
    );
}

/// Un `task_id` desconocido (o expulsado del anillo) es `NotFound` de la
/// taxonomía (0.79.0), lo mismo que su gemelo `fs.rename_batch_report` y lo
/// mismo que contesta el brazo embebido. Antes era `INVALID_PARAMS` pelado, que
/// el cliente leía como `Internal` —la respuesta de un provider que panica—.
#[tokio::test]
async fn undo_report_task_desconocida_es_not_found() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::PolicyUndoReportResult>(
            methods::POLICY_UNDO_REPORT,
            &methods::PolicyUndoReportParams {
                task_id: norte_proto::TaskId::new(424_242),
            },
        )
        .await
        .expect_err("sin undo no hay informe");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::NotFound)),
            "NotFound de la taxonomía, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// #72 (borde): `rpc.cancel` de un id DESCONOCIDO (nada en vuelo) es un no-op
/// benigno — ni cuelga ni rompe la conexión. Cubre también el caso de un
/// `rpc.cancel` errante mientras NADA está suspendido.
#[tokio::test]
async fn rpc_cancel_de_id_desconocido_es_no_op() {
    let d = spawn_daemon_ask(Duration::from_secs(30)).await;
    let agent = connected_agent(&d, "s1").await;

    agent
        .notify(
            methods::RPC_CANCEL,
            &methods::RpcCancelParams {
                id: norte_proto::wire::RequestId::Num(9999),
            },
        )
        .expect("rpc.cancel notify");

    // La conexión sigue sirviendo: el cancel de un id inexistente se dropea.
    let _: methods::TaskListResult = agent
        .call(methods::TASK_LIST, &methods::TaskListParams {})
        .await
        .expect("la conexión sigue viva tras un rpc.cancel de id desconocido");
}

// ---------- fs.search (liveSearch T4) ----------

/// Params de `fs.search` con solo un glob de nombre (helper de test).
pub(super) fn search_by_name(root: &str, name_glob: &str) -> FsSearchParams {
    FsSearchParams {
        root: vp(root),
        name_glob: Some(name_glob.into()),
        name_regex: None,
        content: None,
        content_regex: None,
        case_sensitive: false,
        max_hits: None,
    }
}

/// Drena `search.hits` + `task.progress` de una búsqueda hasta su terminal.
/// Tras ver el terminal sigue vaciando brevemente los `search.hits` ya
/// encolados (la bomba de hits y la de progreso son tasks distintas: el orden
/// entre el último lote y el terminal no está garantizado). Devuelve las
/// entries acumuladas y el estado terminal.
pub(super) async fn drain_search(c: &mut Client, task_id: u64) -> (Vec<Entry>, TaskState) {
    let mut hits: Vec<Entry> = Vec::new();
    let mut terminal: Option<TaskState> = None;
    loop {
        // Antes del terminal, esperamos generoso; después, solo drenamos lo ya
        // encolado (el walker terminó, no llegará nada nuevo).
        let to = if terminal.is_some() {
            Duration::from_millis(400)
        } else {
            Duration::from_secs(5)
        };
        let n = match tokio::time::timeout(to, c.notification()).await {
            Ok(Some(n)) => n,
            Ok(None) => break,
            Err(_) => {
                assert!(terminal.is_some(), "timeout esperando la búsqueda");
                break;
            }
        };
        if n.method == methods::SEARCH_HITS {
            let sh: SearchHits =
                serde_json::from_value(n.params.expect("params")).expect("SearchHits");
            if sh.task_id.get() == task_id {
                hits.extend(sh.entries);
            }
        } else if n.method == methods::TASK_PROGRESS {
            let p: TaskProgress =
                serde_json::from_value(n.params.expect("params")).expect("TaskProgress");
            if p.task_id.get() == task_id && p.state.is_terminal() {
                terminal = Some(p.state);
            }
        }
    }
    (hits, terminal.expect("estado terminal de la búsqueda"))
}

/// Round-trip: A lanza `fs.search`, recibe `FsTaskResult`, luego `search.hits`
/// con sus entries y un `task.progress` terminal Completed.
#[tokio::test]
async fn fs_search_round_trip() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///sub")).await.expect("mkdir");
    write_file(&d.mem, "mem:///x.rs", b"fn main() {}").await;
    write_file(&d.mem, "mem:///y.txt", b"nope").await;
    write_file(&d.mem, "mem:///sub/z.rs", b"mod z;").await;
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(methods::FS_SEARCH, &search_by_name("mem:///", "*.rs"))
        .await
        .expect("fs.search");
    assert!(task.task_id.get() > 0);

    let (hits, state) = drain_search(&mut c, task.task_id.get()).await;
    assert_eq!(state, TaskState::Completed);
    let mut names: Vec<String> = hits
        .iter()
        .map(|e| {
            String::from_utf8_lossy(e.path.file_name().expect("nombre").as_bytes()).into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(names, vec!["x.rs".to_string(), "z.rs".to_string()]);
}

/// Los hits van SOLO al dueño: A busca, B (otra conexión) jamás recibe un
/// `search.hits` (aunque sí ve el `task.progress`, que se difunde a humanos).
#[tokio::test]
async fn fs_search_hits_solo_al_dueno() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///a.rs", b"x").await;
    write_file(&d.mem, "mem:///b.rs", b"y").await;
    let mut a = connected_client(&d).await;
    let mut b = connected_client(&d).await;

    let task: FsTaskResult = a
        .call(methods::FS_SEARCH, &search_by_name("mem:///", "*.rs"))
        .await
        .expect("fs.search de A");
    let task_id = task.task_id.get();

    // B observa hasta el terminal de la task de A y NUNCA ve un search.hits.
    let mut b_saw_hits = false;
    loop {
        let notif = tokio::time::timeout(Duration::from_secs(5), b.notification())
            .await
            .expect("notif de B antes del timeout")
            .expect("conexión de B viva");
        if notif.method == methods::SEARCH_HITS {
            b_saw_hits = true;
        } else if notif.method == methods::TASK_PROGRESS {
            let prog: TaskProgress =
                serde_json::from_value(notif.params.expect("params")).expect("TaskProgress");
            if prog.task_id.get() == task_id && prog.state.is_terminal() {
                break;
            }
        }
    }
    assert!(!b_saw_hits, "B jamás recibe los hits de la búsqueda de A");

    // A sí los recibió.
    let (hits, state) = drain_search(&mut a, task_id).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(hits.len(), 2);
}

/// Cancelación por el wire: A busca (con latencia), manda `task.cancel` →
/// terminal Cancelled; la task sale de las vivas (no fuga).
#[tokio::test]
async fn fs_search_cancel_por_wire() {
    let d = spawn_daemon(None).await;
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(20)));
    for i in 0..200 {
        write_file(&d.mem, &format!("mem:///f{i}.rs"), b"x").await;
    }
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(methods::FS_SEARCH, &search_by_name("mem:///", "*.rs"))
        .await
        .expect("fs.search");
    let _: TaskCancelResult = c
        .call(
            methods::TASK_CANCEL,
            &TaskCancelParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("task.cancel");
    let (_hits, state) = drain_search(&mut c, task.task_id.get()).await;
    assert_eq!(state, TaskState::Cancelled);

    // No queda como task VIVA (solo puede aparecer su terminal en `recent`).
    let list: methods::TaskListResult = c
        .call(methods::TASK_LIST, &methods::TaskListParams::default())
        .await
        .expect("task.list");
    assert!(
        list.tasks
            .iter()
            .all(|t| t.task_id != task.task_id || t.state.is_terminal()),
        "la task no sigue viva"
    );
}

/// Las filas son de quien lanzó la comparación: otra conexión NUNCA ve un
/// `compare.rows` ajeno (mismo criterio direccional que `search.hits`).
#[tokio::test]
async fn fs_compare_filas_solo_al_dueno() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    write_file(&d.mem, "mem:///l/a.txt", b"x").await;
    write_file(&d.mem, "mem:///r/a.txt", b"x").await;
    let mut duena = connected_client(&d).await;
    let mut ajena = connected_client(&d).await;

    let task: FsTaskResult = duena
        .call(methods::FS_COMPARE, &compare_params("mem:///l", "mem:///r"))
        .await
        .expect("fs.compare de la dueña");
    let task_id = task.task_id.get();

    // La otra conexión observa hasta el terminal y jamás ve un compare.rows.
    loop {
        let notif = tokio::time::timeout(Duration::from_secs(5), ajena.notification())
            .await
            .expect("timeout esperando el terminal en la conexión ajena")
            .expect("canal vivo");
        assert_ne!(
            notif.method,
            methods::COMPARE_ROWS,
            "una conexión ajena recibió filas que no son suyas"
        );
        if notif.method == methods::TASK_PROGRESS {
            let p: TaskProgress =
                serde_json::from_value(notif.params.expect("params")).expect("TaskProgress");
            if p.task_id.get() == task_id && p.state.is_terminal() {
                break;
            }
        }
    }
    let (batches, state) = drain_compare(&mut duena, task_id).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(batches.iter().map(|b| b.rows.len()).sum::<usize>(), 1);
}

/// Cancelación por el wire (regla dura 3): `task.cancel` termina la Task como
/// `Cancelled` —el único `Err` del motor ES la cancelación, no un fallo— y los
/// lotes paran.
#[tokio::test]
async fn fs_compare_cancel_por_wire() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    for i in 0..200 {
        write_file(&d.mem, &format!("mem:///l/f{i}.txt"), b"x").await;
        write_file(&d.mem, &format!("mem:///r/f{i}.txt"), b"x").await;
    }
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(20)));
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(methods::FS_COMPARE, &compare_params("mem:///l", "mem:///r"))
        .await
        .expect("fs.compare");
    let _: TaskCancelResult = c
        .call(
            methods::TASK_CANCEL,
            &TaskCancelParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("task.cancel");
    let (batches, state) = drain_compare(&mut c, task.task_id.get()).await;
    assert_eq!(state, TaskState::Cancelled);
    assert!(
        batches.iter().map(|b| b.rows.len()).sum::<usize>() < 200,
        "siguieron llegando filas tras el cancel"
    );

    // No queda como task VIVA (no fuga).
    let list: methods::TaskListResult = c
        .call(methods::TASK_LIST, &methods::TaskListParams::default())
        .await
        .expect("task.list");
    assert!(
        list.tasks
            .iter()
            .all(|t| t.task_id != task.task_id || t.state.is_terminal()),
        "la task no sigue viva"
    );
}

/// Dos raíces que resuelven al mismo sitio son `-32602` y NO crean Task:
/// comparar algo contra sí mismo durante una hora no es una petición, es una
/// errata de quien llama.
#[tokio::test]
async fn fs_compare_contra_si_misma_es_invalid_params_sin_task() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///data")).await.expect("mkdir");
    let c = connected_client(&d).await;
    let before: methods::TaskListResult = c
        .call(methods::TASK_LIST, &methods::TaskListParams::default())
        .await
        .expect("task.list");

    let err = c
        .call::<_, FsTaskResult>(
            methods::FS_COMPARE,
            &compare_params("mem:///data", "mem:///data"),
        )
        .await
        .expect_err("rechazada");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS),
        other => panic!("esperaba Rpc INVALID_PARAMS, fue {other:?}"),
    }
    let after: methods::TaskListResult = c
        .call(methods::TASK_LIST, &methods::TaskListParams::default())
        .await
        .expect("task.list");
    assert_eq!(
        after.tasks.len(),
        before.tasks.len(),
        "no puede crearse Task alguna"
    );
}

/// Y el lado BIEN escrito llega hasta el motor: el huérfano de la izquierda se
/// enumera, y el de la derecha sigue siendo una fila. Es el cable entero —wire,
/// engine, walk— y no solo la struct.
#[tokio::test]
async fn fs_compare_descend_orphans_llega_al_motor() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    d.mem.mkdir(&vp("mem:///l/solo")).await.expect("mkdir solo");
    write_file(&d.mem, "mem:///l/solo/dentro.txt", b"x").await;
    d.mem.mkdir(&vp("mem:///r/otro")).await.expect("mkdir otro");
    // Con hijo: sin él, «el otro lado no se descendió» se cumpliría solo.
    write_file(&d.mem, "mem:///r/otro/dentro-derecha.txt", b"y").await;
    let mut c = connected_client(&d).await;

    let mut p = compare_params("mem:///l", "mem:///r");
    p.descend_orphans = Some(methods::DescendSide::Left);
    let task: FsTaskResult = c
        .call(methods::FS_COMPARE, &p)
        .await
        .expect("fs.compare aceptada");
    let (batches, terminal) = drain_compare(&mut c, task.task_id.get()).await;
    assert_eq!(terminal, TaskState::Completed);

    let nombres: Vec<Vec<u8>> = batches
        .iter()
        .flat_map(|b| &b.rows)
        .filter_map(|row| {
            [row.left.as_ref(), row.right.as_ref()]
                .into_iter()
                .flatten()
                .next()
                .and_then(|e| e.path.file_name())
                .map(|s| s.as_bytes().to_vec())
        })
        .collect();
    assert!(nombres.contains(&b"solo".to_vec()), "{nombres:?}");
    assert!(
        nombres.contains(&b"dentro.txt".to_vec()),
        "el huérfano del origen no se enumeró: {nombres:?}"
    );
    assert!(nombres.contains(&b"otro".to_vec()), "{nombres:?}");
    assert!(
        !nombres.contains(&b"dentro-derecha.txt".to_vec()),
        "el huérfano del DESTINO no se descendió: {nombres:?}"
    );
}

/// Los pasos son de quien lanzó el plan: otra conexión NUNCA ve un `sync.steps`
/// ni un `sync.plan_done` ajeno (mismo criterio direccional que
/// `compare.rows`). Y es más grave aquí: el `plan_hash` ES la autorización para
/// escribir.
#[tokio::test]
async fn sync_plan_pasos_y_hash_solo_al_dueno() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    d.mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    write_file(&d.mem, "mem:///s/a.txt", b"x").await;
    let mut duena = connected_client(&d).await;
    let mut ajena = connected_client(&d).await;

    let task: FsTaskResult = duena
        .call(methods::SYNC_PLAN, &sync_params("mem:///s", "mem:///d"))
        .await
        .expect("sync.plan de la dueña");
    let task_id = task.task_id.get();

    loop {
        let notif = tokio::time::timeout(Duration::from_secs(5), ajena.notification())
            .await
            .expect("timeout esperando el terminal en la conexión ajena")
            .expect("canal vivo");
        assert_ne!(notif.method, methods::SYNC_STEPS, "pasos que no son suyos");
        assert_ne!(
            notif.method,
            methods::SYNC_PLAN_DONE,
            "un plan_hash ajeno es una autorización de escritura ajena"
        );
        if notif.method == methods::TASK_PROGRESS {
            let p: TaskProgress =
                serde_json::from_value(notif.params.expect("params")).expect("TaskProgress");
            if p.task_id.get() == task_id && p.state.is_terminal() {
                break;
            }
        }
    }
    let (batches, done, state) = drain_sync(&mut duena, task_id).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(batches.iter().map(|b| b.steps.len()).sum::<usize>(), 1);
    assert!(done.is_some());
}

/// Tercera de las cuatro muertes del spool (ADR 0049): al cerrarse la conexión
/// se van sus planes. Un plan sin dueño no lo puede aplicar nadie, y lo que
/// quedaría en disco es un listado relativo de dos árboles.
#[tokio::test]
async fn sync_plan_al_cerrar_la_conexion_se_van_sus_planes() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    d.mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    write_file(&d.mem, "mem:///s/a.txt", b"x").await;
    // El socket vive en el mismo tempdir que el estado del daemon.
    let spool_dir = d
        .socket
        .parent()
        .expect("el socket cuelga del tempdir")
        .join(norte_core::sync::SPOOL_DIR_NAME);
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(methods::SYNC_PLAN, &sync_params("mem:///s", "mem:///d"))
        .await
        .expect("sync.plan");
    let (_, done, state) = drain_sync(&mut c, task.task_id.get()).await;
    assert_eq!(state, TaskState::Completed);
    assert!(done.is_some());
    assert_eq!(
        std::fs::read_dir(&spool_dir)
            .expect("dir de spools")
            .count(),
        1,
        "el plan aprobado se retiene mientras vive su conexión"
    );

    drop(c);
    // El desmontaje de la conexión es asíncrono. Se espera A LA CONDICIÓN con
    // un presupuesto, jamás un plazo fijo: un `sleep` calibrado a ojo es lo que
    // convierte un test en intermitente bajo carga.
    let hasta = tokio::time::Instant::now() + Duration::from_secs(10);
    let restantes = loop {
        let n = std::fs::read_dir(&spool_dir)
            .expect("dir de spools")
            .count();
        if n == 0 || tokio::time::Instant::now() >= hasta {
            break n;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert_eq!(restantes, 0, "un plan sin dueño no lo aplica nadie");
}

/// Camino feliz por el socket: build → embed → `search_semantic` devuelve el
/// fichero sembrado como primer hit, con un score que es un número JSON
/// FINITO (el cinturón anti-NaN del engine es contractual: un `NaN`
/// serializaría como `null` y rompería la respuesta en el cliente).
#[tokio::test]
async fn semantic_por_el_socket_devuelve_hits() {
    let d = spawn_daemon_embed(None).await;
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    write_file(&d.mem, "mem:///r/a.txt", b"contenido alfa").await;
    let mut c = connected_client(&d).await;

    let t: FsTaskResult = c
        .call(
            methods::INDEX_BUILD,
            &methods::IndexBuildParams {
                root: vp("mem:///r"),
            },
        )
        .await
        .expect("index.build");
    let seen = drain_task(&mut c, t.task_id.get()).await;
    assert_eq!(seen.last().expect("terminal").state, TaskState::Completed);

    let t: FsTaskResult = c
        .call(
            methods::INDEX_EMBED,
            &methods::IndexEmbedParams {
                root: vp("mem:///r"),
            },
        )
        .await
        .expect("index.embed");
    let seen = drain_task(&mut c, t.task_id.get()).await;
    assert_eq!(seen.last().expect("terminal").state, TaskState::Completed);

    let r: methods::IndexSearchSemanticResult = c
        .call(
            methods::INDEX_SEARCH_SEMANTIC,
            &methods::IndexSearchSemanticParams {
                root: Some(vp("mem:///r")),
                query: "contenido alfa".into(),
                k: 5,
            },
        )
        .await
        .expect("index.search_semantic");
    let top = r.hits.first().expect("al menos un hit");
    assert_eq!(top.path, vp("mem:///r/a.txt"));
    assert!(
        top.score.is_finite(),
        "score finito por contrato del wire, fue {}",
        top.score
    );
}

/// Y con un nombre HOSTIL, byte a byte por el socket (#122).
///
/// El test de arriba redondea `a.txt`, así que la ruta que vuelve cabe en
/// ASCII y no dice nada del camino NDJSON. Aquí el fichero lleva bytes que no
/// son UTF-8 (`%FF%FE` en el wire), que es lo que un `to_string_lossy` de más
/// convertiría en `\u{FFFD}` — dando un hit que apunta a un fichero que no
/// existe, y sobre el que un frontend haría `cd` sin encontrar nada.
#[tokio::test]
async fn semantic_por_el_socket_sobrevive_a_un_nombre_hostil() {
    let d = spawn_daemon_embed(None).await;
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    let hostil = "mem:///r/%FF%FE.txt";
    write_file(&d.mem, hostil, b"contenido alfa").await;
    let mut c = connected_client(&d).await;

    let t: FsTaskResult = c
        .call(
            methods::INDEX_BUILD,
            &methods::IndexBuildParams {
                root: vp("mem:///r"),
            },
        )
        .await
        .expect("index.build");
    let seen = drain_task(&mut c, t.task_id.get()).await;
    assert_eq!(seen.last().expect("terminal").state, TaskState::Completed);

    let t: FsTaskResult = c
        .call(
            methods::INDEX_EMBED,
            &methods::IndexEmbedParams {
                root: vp("mem:///r"),
            },
        )
        .await
        .expect("index.embed");
    let seen = drain_task(&mut c, t.task_id.get()).await;
    assert_eq!(seen.last().expect("terminal").state, TaskState::Completed);

    let r: methods::IndexSearchSemanticResult = c
        .call(
            methods::INDEX_SEARCH_SEMANTIC,
            &methods::IndexSearchSemanticParams {
                root: Some(vp("mem:///r")),
                query: "contenido alfa".into(),
                k: 5,
            },
        )
        .await
        .expect("index.search_semantic");
    let top = r.hits.first().expect("al menos un hit");
    assert_eq!(
        top.path,
        vp(hostil),
        "la ruta volvió del socket con otros bytes"
    );
    // Y los bytes son los de verdad, no un reemplazo: `\u{FFFD}` en UTF-8 es
    // `efbfbd`, y comparar la ruta reconstruida no lo distinguiría si el
    // parser hubiera aceptado el escape de otra cosa.
    assert_eq!(
        top.path.file_name().expect("nombre").as_bytes(),
        b"\xff\xfe.txt"
    );
}

/// El caso que el bucle de un `fs.move` por pareja NUNCA pudo hacer: una
/// permutación `a→b, b→a` como UNA Task por el socket.
#[tokio::test]
async fn rename_batch_ejecuta_una_permutacion_por_el_socket() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///a", b"soy-a").await;
    write_file(&d.mem, "mem:///b", b"soy-b").await;
    let c = connected_client(&d).await;
    let pairs = vec![pair(b"a", b"b"), pair(b"b", b"a")];

    let plan: methods::FsRenameBatchPlanResult = c
        .call(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: vp("mem:///"),
                pairs: pairs.clone(),
            },
        )
        .await
        .expect("plan");
    assert!(plan.executable, "{:?}", plan.collisions);
    assert_eq!(plan.steps.len(), 3, "dos renames y un temporal");

    let task: FsTaskResult = c
        .call(
            methods::FS_RENAME_BATCH,
            &methods::FsRenameBatchParams {
                dir: vp("mem:///"),
                pairs,
                plan_hash: plan.plan_hash.clone(),
            },
        )
        .await
        .expect("fs.rename_batch");
    assert_eq!(wait_terminal(&c, task.task_id).await, TaskState::Completed);
    assert_eq!(read_all(&d.mem, "mem:///a").await, b"soy-b");
    assert_eq!(read_all(&d.mem, "mem:///b").await, b"soy-a");

    // El informe del lote por el wire: la corrida fue limpia.
    let report: methods::FsRenameBatchReportResult = c
        .call(
            methods::FS_RENAME_BATCH_REPORT,
            &methods::FsRenameBatchReportParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("fs.rename_batch_report");
    assert_eq!(report.applied, 3);
    assert_eq!(report.rolled_back, 0);
    assert!(report.stuck.is_none());
    assert!(report.uncertain.is_none());
    assert_eq!(report.compensations_lost, 0);
}

/// Un lote que falla a mitad se desanda entero, y el INFORME lo cuenta por el
/// wire: el `Failed` de la Task solo dice la causa.
#[tokio::test]
async fn rename_batch_fallido_cuenta_su_rollback_por_el_socket() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///a", b"1").await;
    write_file(&d.mem, "mem:///b", b"2").await;
    let c = connected_client(&d).await;
    let pairs = vec![pair(b"a", b"x"), pair(b"b", b"y")];
    let plan: methods::FsRenameBatchPlanResult = c
        .call(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: vp("mem:///"),
                pairs: pairs.clone(),
            },
        )
        .await
        .expect("plan");
    // El SEGUNDO paso muere; el primero ya se aplicó y hay que desandarlo.
    d.mem.faults().fail_rename_at(&vp("mem:///b"));

    let task: FsTaskResult = c
        .call(
            methods::FS_RENAME_BATCH,
            &methods::FsRenameBatchParams {
                dir: vp("mem:///"),
                pairs,
                plan_hash: plan.plan_hash,
            },
        )
        .await
        .expect("fs.rename_batch");
    assert!(
        matches!(
            wait_terminal(&c, task.task_id).await,
            TaskState::Failed { .. }
        ),
        "el lote falla entero"
    );
    assert!(d.mem.stat(&vp("mem:///a")).await.is_ok(), "a volvió");
    assert!(matches!(
        d.mem.stat(&vp("mem:///x")).await,
        Err(Error::NotFound)
    ));

    let report: methods::FsRenameBatchReportResult = c
        .call(
            methods::FS_RENAME_BATCH_REPORT,
            &methods::FsRenameBatchReportParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("fs.rename_batch_report");
    assert_eq!(report.applied, 1);
    assert_eq!(report.rolled_back, 1);
    assert_eq!(report.failed_pair, Some(1), "la fila `b → y`");
    assert!(report.stuck.is_none(), "el rollback SÍ pudo terminar");
}

/// Un `task_id` que jamás fue un lote es `INVALID_PARAMS`, no un informe en
/// blanco que se pudiera leer como «fue todo bien».
#[tokio::test]
async fn rename_batch_report_de_una_task_desconocida_es_invalid_params() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::FsRenameBatchReportResult>(
            methods::FS_RENAME_BATCH_REPORT,
            &methods::FsRenameBatchReportParams {
                task_id: norte_proto::TaskId::new(4242),
            },
        )
        .await
        .expect_err("id desconocido");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::NotFound)),
            "NotFound de la taxonomía — la MISMA que da el brazo embebido, \
             fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// El informe de un lote AJENO contesta lo mismo que un id que nunca existió:
/// lleva rutas del directorio de otro actor, y distinguir «no es tuya» de «no
/// existe» ya sería confirmar que existió (mismo criterio que `task.cancel`).
/// El humano, en cambio, ve el informe del lote del agente — es quien tiene que
/// limpiar si se atascó.
#[tokio::test]
async fn un_agente_no_lee_el_informe_de_un_lote_ajeno() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/a", b"1").await;
    let human = connected_client(&d).await;
    let agent = connected_agent(&d, "s1").await;

    // Lote del HUMANO.
    let pairs = vec![pair(b"a", b"b")];
    let plan: methods::FsRenameBatchPlanResult = human
        .call(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: vp("mem:///proj"),
                pairs: pairs.clone(),
            },
        )
        .await
        .expect("plan");
    let task: FsTaskResult = human
        .call(
            methods::FS_RENAME_BATCH,
            &methods::FsRenameBatchParams {
                dir: vp("mem:///proj"),
                pairs,
                plan_hash: plan.plan_hash,
            },
        )
        .await
        .expect("lote del humano");
    assert_eq!(
        wait_terminal(&human, task.task_id).await,
        TaskState::Completed
    );

    // El agente pide ESE informe: respuesta indistinguible de un id inventado.
    let ajeno = agent
        .call::<_, methods::FsRenameBatchReportResult>(
            methods::FS_RENAME_BATCH_REPORT,
            &methods::FsRenameBatchReportParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect_err("informe ajeno");
    let inventado = agent
        .call::<_, methods::FsRenameBatchReportResult>(
            methods::FS_RENAME_BATCH_REPORT,
            &methods::FsRenameBatchReportParams {
                task_id: norte_proto::TaskId::new(9999),
            },
        )
        .await
        .expect_err("id inventado");
    match (ajeno, inventado) {
        (ClientError::Rpc(a), ClientError::Rpc(b)) => {
            assert!(matches!(a.data, Some(Error::NotFound)), "{:?}", a.data);
            assert_eq!(
                (a.code, a.message),
                (b.code, b.message),
                "las dos respuestas tienen que ser LA MISMA",
            );
        }
        other => panic!("esperaba dos Rpc, fue {other:?}"),
    }

    // Y el humano sí lo lee.
    let report: methods::FsRenameBatchReportResult = human
        .call(
            methods::FS_RENAME_BATCH_REPORT,
            &methods::FsRenameBatchReportParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("el dueño lee su informe");
    assert_eq!(report.applied, 1);
}

// -------------------------------------------------- sync.apply / sync.report

/// El plan de `sync.plan` sobre un árbol de tres ficheros, ya cerrado, por la
/// conexión que lo pidió. Devuelve el cierre —el `plan_hash` está ahí y en
/// ningún otro sitio— para poder aplicarlo.
pub(super) async fn plan_sobre(d: &TestDaemon, c: &mut Client) -> methods::SyncPlanDone {
    d.mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    d.mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    write_file(&d.mem, "mem:///s/a.txt", b"aaa").await;
    write_file(&d.mem, "mem:///s/b.txt", b"bbbb").await;
    // Y uno que YA existe en el destino con otro TAMAÑO: una sobrescritura por
    // el rung de tamaño (`Certain`), para que el informe cuente algo más que
    // copias. Con los dos del mismo tamaño la cascada bajaría a la fecha, y el
    // reloj lógico del `MemProvider` avanza de uno en uno: dos ficheros escritos
    // seguidos caen dentro de la tolerancia y salen `Same`, o sea sin paso.
    write_file(&d.mem, "mem:///s/c.txt", b"nuevo, y mas largo").await;
    write_file(&d.mem, "mem:///d/c.txt", b"viejo").await;

    let task: FsTaskResult = c
        .call(methods::SYNC_PLAN, &sync_params("mem:///s", "mem:///d"))
        .await
        .expect("sync.plan");
    let (_batches, done, state) = drain_sync(c, task.task_id.get()).await;
    assert_eq!(state, TaskState::Completed);
    done.expect("el plan cerró con su hash")
}

/// Aplicar el plan aprobado lo EJECUTA, y el informe cuenta lo que pasó: tantos
/// pasos hechos como copias y sobrescrituras traía el plan, y un lote del
/// journal bajo el que buscarlos — sin él no hay undo que pedir.
#[tokio::test]
async fn sync_apply_ejecuta_el_plan_que_se_aprobo() {
    let d = spawn_daemon_journal().await;
    let mut c = connected_client(&d).await;
    let done = plan_sobre(&d, &mut c).await;
    assert!(done.executable);

    let task: FsTaskResult = c
        .call(
            methods::SYNC_APPLY,
            &methods::SyncApplyParams {
                plan_hash: done.plan_hash.clone(),
            },
        )
        .await
        .expect("sync.apply aceptado");
    assert_eq!(
        wait_terminal(&c, task.task_id).await,
        TaskState::Completed,
        "el plan se aplicó entero"
    );

    let report: methods::SyncReportResult = c
        .call(
            methods::SYNC_REPORT,
            &methods::SyncReportParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("sync.report");
    assert_eq!(report.done, done.counts.copy + done.counts.overwrite);
    assert_eq!(report.failed, 0, "{:?}", report.failures);
    assert!(report.batch_id.is_some(), "el undo lo necesita");
    // Y el árbol de verdad: el destino tiene los bytes del origen.
    let mut stream = d
        .mem
        .read(&vp("mem:///d/c.txt"), None)
        .await
        .expect("read del destino");
    let mut bytes = Vec::new();
    while let Some(chunk) = futures::StreamExt::next(&mut stream).await {
        bytes.extend_from_slice(&chunk.expect("chunk"));
    }
    assert_eq!(bytes, b"nuevo, y mas largo", "la sobrescritura ocurrió");
    assert_eq!(done.counts.overwrite, 1, "el plan traía una sobrescritura");
}

/// El informe lo ve quien podría ver la Task: su dueño o cualquier conexión
/// HUMANA. Un agente que pregunta por el informe de otro recibe la MISMA
/// respuesta que ante un id inventado — el informe lleva rutas relativas de dos
/// árboles ajenos, y distinguir «no es tuya» de «no existe» ya sería filtrar que
/// existió.
#[tokio::test]
async fn sync_report_ajeno_contesta_lo_mismo_que_un_id_inventado() {
    let d = spawn_daemon_journal().await;
    let mut duena = connected_client(&d).await;
    let done = plan_sobre(&d, &mut duena).await;
    let task: FsTaskResult = duena
        .call(
            methods::SYNC_APPLY,
            &methods::SyncApplyParams {
                plan_hash: done.plan_hash,
            },
        )
        .await
        .expect("sync.apply");
    assert_eq!(
        wait_terminal(&duena, task.task_id).await,
        TaskState::Completed
    );

    let fisgon = connected_agent(&d, "s-fisgona").await;
    let ajeno = fisgon
        .call::<_, methods::SyncReportResult>(
            methods::SYNC_REPORT,
            &methods::SyncReportParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect_err("no es suya");
    let inventado = fisgon
        .call::<_, methods::SyncReportResult>(
            methods::SYNC_REPORT,
            &methods::SyncReportParams {
                task_id: norte_proto::TaskId::new(99_999),
            },
        )
        .await
        .expect_err("nunca existió");
    for err in [ajeno, inventado] {
        match err {
            ClientError::Rpc(rpc) => assert!(
                matches!(rpc.data, Some(Error::NotFound)),
                "NotFound, fue {:?}",
                rpc.data
            ),
            other => panic!("esperaba Rpc, fue {other:?}"),
        }
    }
    // Otra conexión HUMANA sí lo ve: la simetría de `may_observe`.
    let otro_humano = connected_client(&d).await;
    let _: methods::SyncReportResult = otro_humano
        .call(
            methods::SYNC_REPORT,
            &methods::SyncReportParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("un humano ve los informes del daemon que gobierna");
}

/// Cada span nuevo, con la cadena de nombres de sus antepasados (de dentro
/// afuera) y los campos con que nació.
/// `(nombre, antepasados de dentro afuera, campos)`.
type SpanVisto = (String, Vec<String>, String);

#[derive(Clone, Default)]
struct Spans(Arc<std::sync::Mutex<Vec<SpanVisto>>>);

impl<S> tracing_subscriber::Layer<S> for Spans
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        struct Campos(String);
        impl tracing::field::Visit for Campos {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                use std::fmt::Write as _;
                let _ = write!(self.0, "{}={v:?};", f.name());
            }
        }
        let mut campos = Campos(String::new());
        attrs.record(&mut campos);
        let Some(span) = ctx.span(id) else { return };
        let antepasados = span.scope().skip(1).map(|s| s.name().to_owned()).collect();
        self.0
            .lock()
            .expect("spans")
            .push((span.name().to_owned(), antepasados, campos.0));
    }
}

/// **Una tarea que pide el daemon cuelga de la petición que la pidió**
/// (ADR 0127), y la petición es UN span `rpc` — no dos: `dispatch` llevaba ya
/// su propio `#[instrument]`, y con los dos apilados la cadena era
/// `dispatch → rpc → task`, con el método repetido y `spans[0]` equivocado.
#[tokio::test]
async fn la_tarea_de_una_peticion_cuelga_de_su_rpc() {
    use tracing_subscriber::layer::SubscriberExt as _;

    let spans = Spans::default();
    let _guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(spans.clone()));

    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///src.bin", &[0xAB; 10]).await;
    let mut c = connected_client(&d).await;
    let task: FsTaskResult = c
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///src.bin"),
                to: vp("mem:///dst.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
            },
        )
        .await
        .expect("fs.copy");
    let _ = drain_task(&mut c, task.task_id.get()).await;

    let vistos = spans.0.lock().expect("spans").clone();
    assert!(
        !vistos.iter().any(|(n, _, _)| n == "dispatch"),
        "dispatch abre UN span, `rpc`: {vistos:?}"
    );
    let copia = vistos
        .iter()
        .find(|(n, _, c)| n == "rpc" && c.contains("method=fs.copy"))
        .expect("el rpc de fs.copy");
    assert!(copia.1.is_empty(), "rpc es la raíz: {copia:?}");
    assert!(copia.2.contains("conn_id="), "{copia:?}");
    assert!(copia.2.contains("req_id="), "{copia:?}");
    let tarea = vistos
        .iter()
        .find(|(n, _, c)| n == "task" && c.contains(&format!("task_id={}", task.task_id)))
        .expect("el span de la tarea");
    assert!(
        tarea.1.iter().any(|a| a == "rpc"),
        "la tarea cuelga de su petición: {tarea:?}"
    );
}
