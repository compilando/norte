use super::*;

// ---------- fs.rename_batch{,_plan,_report} (0.36.0, ADR 0042) ----------

/// Un nombre base desde sus BYTES: lo que un `String` no habría podido llevar.
pub(super) fn sg(bytes: &[u8]) -> norte_proto::Segment {
    norte_proto::Segment::new(bytes.to_vec()).expect("segment de test")
}

/// Contenido completo de un fichero del `MemProvider` (para comprobar QUÉ
/// fichero acabó bajo cada nombre tras una permutación).
pub(super) async fn read_all(mem: &MemProvider, wire: &str) -> Vec<u8> {
    use futures::StreamExt;
    let mut stream = mem.read(&vp(wire), None).await.expect("read abre");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk"));
    }
    out
}

pub(super) fn pair(from: &[u8], to: &[u8]) -> methods::RenamePair {
    methods::RenamePair {
        from: sg(from),
        to: sg(to),
    }
}

/// El plan cruza el socket con un nombre NO-UTF8 intacto, y NO muta nada.
///
/// El nombre viaja percent-encoded (`caf%FF.txt`) y vuelve como los mismos
/// bytes: es el caso que motiva que `RenamePair` lleve `Segment` y no `String`
/// (regla dura 1).
#[tokio::test]
async fn rename_batch_plan_responde_por_el_socket() {
    let d = spawn_daemon(None).await;
    let hostile = b"caf\xff.txt";
    write_file(&d.mem, "mem:///caf%FF.txt", b"x").await;
    write_file(&d.mem, "mem:///b.txt", b"y").await;
    let c = connected_client(&d).await;

    let plan: methods::FsRenameBatchPlanResult = c
        .call(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: vp("mem:///"),
                pairs: vec![pair(hostile, b"cafe.txt")],
            },
        )
        .await
        .expect("fs.rename_batch_plan");

    assert!(plan.executable, "{:?}", plan.collisions);
    assert_eq!(plan.steps.len(), 1);
    assert_eq!(
        plan.steps[0].from.as_bytes(),
        hostile,
        "los bytes hostiles sobreviven al viaje de ida y vuelta",
    );
    assert_eq!(plan.steps[0].to.as_bytes(), b"cafe.txt");
    assert_eq!(plan.plan_hash.to_string().len(), 64);
    // Planificar NO muta: el fichero sigue con su nombre.
    assert!(d.mem.stat(&vp("mem:///caf%FF.txt")).await.is_ok());
    assert!(matches!(
        d.mem.stat(&vp("mem:///cafe.txt")).await,
        Err(Error::NotFound)
    ));
}

/// La DERIVA se rehúsa con la categoría accionable (`plan_stale`), no con un
/// error interno genérico: el humano sabe que tiene que volver a planificar.
#[tokio::test]
async fn rename_batch_con_hash_rancio_es_plan_stale() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///a", b"1").await;
    let c = connected_client(&d).await;
    let pairs = vec![pair(b"a", b"z")];
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
    assert!(plan.executable);

    // El destino aparece A ESPALDAS del daemon: el re-plan lo ve ocupado y
    // concluye otra cosa.
    write_file(&d.mem, "mem:///z", b"intruso").await;

    let err = c
        .call::<_, FsTaskResult>(
            methods::FS_RENAME_BATCH,
            &methods::FsRenameBatchParams {
                dir: vp("mem:///"),
                pairs,
                plan_hash: plan.plan_hash,
            },
        )
        .await
        .expect_err("el plan aprobado ya no vale");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PlanStale)),
            "PlanStale, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
    // Y no tocó nada.
    assert!(d.mem.stat(&vp("mem:///a")).await.is_ok());
    assert_eq!(read_all(&d.mem, "mem:///z").await, b"intruso");
}

/// El tope de parejas se impone EN LA FRONTERA y RECHAZA (no recorta): un lote
/// recortado ejecutaría un plan distinto del pedido. Los DOS métodos.
#[tokio::test]
async fn rename_batch_por_encima_del_tope_de_parejas_es_invalid_params() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let too_many: Vec<methods::RenamePair> = (0..=methods::FS_RENAME_BATCH_MAX_PAIRS)
        .map(|i| pair(format!("f{i}").as_bytes(), format!("g{i}").as_bytes()))
        .collect();
    assert_eq!(too_many.len(), methods::FS_RENAME_BATCH_MAX_PAIRS + 1);

    let err = c
        .call::<_, methods::FsRenameBatchPlanResult>(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: vp("mem:///"),
                pairs: too_many.clone(),
            },
        )
        .await
        .expect_err("por encima del tope");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::InvalidPath)),
            "InvalidPath, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }

    let err = c
        .call::<_, FsTaskResult>(
            methods::FS_RENAME_BATCH,
            &methods::FsRenameBatchParams {
                dir: vp("mem:///"),
                pairs: too_many,
                plan_hash: methods::PlanHash::parse(&"0".repeat(64)).expect("hash"),
            },
        )
        .await
        .expect_err("por encima del tope");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::InvalidPath)),
            "InvalidPath, fue {:?}",
            rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }

    // Justo en el tope NO es error de params (muere por otra cosa o pasa): el
    // rechazo es del EXCESO, no del tamaño legal.
    let at_cap: Vec<methods::RenamePair> = (0..methods::FS_RENAME_BATCH_MAX_PAIRS)
        .map(|i| pair(format!("f{i}").as_bytes(), format!("g{i}").as_bytes()))
        .collect();
    let plan: methods::FsRenameBatchPlanResult = c
        .call(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: vp("mem:///"),
                pairs: at_cap,
            },
        )
        .await
        .expect("el tope exacto se planifica");
    assert!(!plan.executable, "ninguno de esos ficheros existe");
}
