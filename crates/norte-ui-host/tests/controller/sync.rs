use super::*;

// ---------------------------------------------------------------------------
// Sync: the PLAN (task 6.3, phase A).
// ---------------------------------------------------------------------------

/// Waits for the next update carrying the sync panel.
pub(super) async fn siguiente_sync(
    sub: &mut norte_ui_host::controller::UiSubscription,
) -> Option<norte_ui_host::dto::SyncView> {
    for _ in 0..40 {
        let Ok(Some(u)) = tokio::time::timeout(ESPERA_MAX, sub.recv()).await else {
            continue;
        };
        match u {
            Update::Message(m) => {
                if let UiUpdate::Patch(p) = &m.payload {
                    for c in &p.changes {
                        if let norte_ui_host::dto::ViewChange::Sync { sync } = c {
                            return sync.clone();
                        }
                    }
                }
            }
            Update::Lagged => {}
        }
    }
    panic!("no update with the plan ever arrived");
}

/// A plan step, with the minimum needed to paint it.
pub(super) fn paso_de_plan(
    id: u64,
    rel: &str,
    kind: norte_proto::methods::SyncStepKind,
) -> norte_proto::methods::SyncStep {
    norte_proto::methods::SyncStep {
        id,
        kind,
        rel: norte_proto::methods::RelPath::new(
            rel.split('/')
                .map(|s| norte_proto::Segment::new(s.as_bytes().to_vec()).expect("segment"))
                .collect(),
        ),
        dest_rel: None,
        size: Some(10),
        criterion: norte_proto::methods::CompareCriterion::Size,
        confidence: norte_proto::methods::CompareConfidence::Certain,
        // The reversal that belongs to a copy: undoing it DELETES what it
        // created. The shared model rejects a step whose shape contradicts
        // itself — a class that writes with no reversal, a `Skip` that
        // claims to have one — and that rejection is what stops a plan that
        // cannot be painted from being approved.
        reversal: Some(norte_proto::methods::StepReversal::Delete),
        reason: None,
    }
}

/// A plan's closing, with no blockers.
pub(super) fn plan_cerrado(pasos: u64) -> norte_proto::methods::SyncPlanDone {
    // The counts, as the daemon would count them: the model compares them
    // class by class against its own, and a plan that does not add up is
    // NOT approved. The bytes too: every step in this test measures ten.
    let counts = norte_proto::methods::SyncCounts {
        copy: pasos,
        bytes: pasos * 10,
        ..Default::default()
    };
    norte_proto::methods::SyncPlanDone {
        // Corrected on landing: the model matches the closing to ITS Task.
        task_id: norte_proto::TaskId::new(0),
        plan_hash: norte_proto::methods::PlanHash::parse(
            &"a".repeat(norte_proto::methods::PLAN_HASH_LEN),
        )
        .expect("test hash"),
        counts,
        blockers: Vec::new(),
        blockers_total: 0,
        executable: true,
        // With trash: it is what lets the undo column say something other
        // than "not known".
        dest_trash: norte_proto::methods::DestTrash::Restorable,
    }
}

/// Requesting a sync opens the panel with the plan the core answered, and
/// the plan says for each step whether undo brings it back.
#[tokio::test]
async fn pedir_sincronizar_abre_el_plan() {
    let falso = arbol_como_falso();
    *falso.plan_de_sync.lock().expect("plan") = Some((
        vec![
            // Both of the SAME class: the model compares the counts class
            // by class against the daemon's, and a plan that does not add
            // up is not approved — which is exactly what has to happen.
            paso_de_plan(1, "a.md", norte_proto::methods::SyncStepKind::Copy),
            paso_de_plan(2, "b.md", norte_proto::methods::SyncStepKind::Copy),
        ],
        plan_cerrado(2),
    ));
    let backend = Arc::new(falso);
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separar_los_paneles(&h, &mut sub).await;
    ejecutar_por_paleta(&h, &mut sub, "pane.sync-dirs").await;

    // Until the plan CLOSES: the steps arrive in one patch and the closing
    // in another, and what can be approved is a closed plan.
    let mut vista = siguiente_sync(&mut sub).await.expect("opens");
    for _ in 0..20 {
        if vista.can_approve {
            break;
        }
        vista = siguiente_sync(&mut sub).await.expect("still open");
    }
    assert_eq!(vista.steps.len(), 2, "{vista:?}");
    assert_eq!(vista.total, 2);
    // The mode is PAINTED before approving: a mirror deletes and an update
    // does not, and whoever approves has to see it.
    // The mode arrives already TRANSLATED through the shared label, not as
    // an id: a mode this build did not know how to name cannot fall back to
    // "update", which is the safe half of what is being approved.
    assert_eq!(
        vista.mode,
        norte_frontend::sync::mode_label(
            norte_proto::methods::SyncMode::Update,
            norte_i18n::Lang::Es
        )
    );
    // And every step says whether undo returns it: it never comes out of
    // `reversal` plain and simple, which is the half that lies with no
    // trash bin at the destination.
    assert!(vista.steps.iter().all(|p| !p.undo.is_empty()), "{vista:?}");
    assert!(
        vista.can_approve,
        "a closed plan with no blockers gets approved: {}",
        vista.status
    );
    let pedidos = backend.planes_pedidos.lock().expect("plans").clone();
    assert_eq!(pedidos.len(), 1);
    assert_ne!(
        pedidos[0].0, pedidos[0].1,
        "source and destination are different"
    );
}

/// A plan with BLOCKERS cannot be approved, and it says which ones they are.
#[tokio::test]
async fn un_plan_con_bloqueos_no_se_aprueba() {
    let mut done = plan_cerrado(1);
    done.blockers = vec![norte_proto::methods::SyncBlocker {
        kind: norte_proto::methods::SyncBlockerKind::DestReadOnly,
        // The root: a lock on the whole tree hangs off no step.
        rel: norte_proto::methods::RelPath::new(Vec::new()),
        side: None,
    }];
    done.executable = false;
    done.blockers_total = 1;
    let falso = arbol_como_falso();
    *falso.plan_de_sync.lock().expect("plan") = Some((
        vec![paso_de_plan(
            1,
            "a.md",
            norte_proto::methods::SyncStepKind::Copy,
        )],
        done,
    ));
    let (h, _snap) = host_con_layout(Arc::new(falso), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separar_los_paneles(&h, &mut sub).await;
    ejecutar_por_paleta(&h, &mut sub, "pane.sync-dirs").await;

    let mut vista = siguiente_sync(&mut sub).await.expect("opens");
    for _ in 0..20 {
        if !vista.blockers.is_empty() {
            break;
        }
        vista = siguiente_sync(&mut sub).await.expect("still open");
    }
    assert!(
        !vista.blockers.is_empty(),
        "it says what is stopping it: {vista:?}"
    );
    assert!(
        !vista.can_approve,
        "and approving is not offered: {vista:?}"
    );
}

/// Syncing both panes when they are in the SAME place queues nothing.
#[tokio::test]
async fn sincronizar_el_mismo_directorio_no_encola_nada() {
    let falso = arbol_como_falso();
    *falso.plan_de_sync.lock().expect("plan") = Some((Vec::new(), plan_cerrado(0)));
    let backend = Arc::new(falso);
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    // Without separating the panes: both look at `casa`. And the command
    // really RUNS — the previous version of this test pressed `Escape` on
    // the palette and asserted that nothing had been requested, which is
    // true whether the guard exists or not.
    let ack = ejecutar_por_paleta_ack(&h, &mut sub, "pane.sync-dirs").await;
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "host-same-directory".to_owned()
        },
        "{ack:?}"
    );
    assert!(
        backend.planes_pedidos.lock().expect("planes").is_empty(),
        "no plan was requested"
    );
}

/// Cancelling the plan's Task from the BOARD leaves the panel saying it was
/// cancelled, not "planning…" forever.
///
/// The Task's outcome was not reaching the model, so `run` was left in
/// `Running` forever: the panel did not know how to say "cancelled" nor
/// "failed", and — what matters for the next phase — kept saying the plan
/// can be approved after someone told it to stop.
#[tokio::test]
async fn cancelar_el_plan_desde_el_tablero_lo_dice_en_el_panel() {
    let falso = arbol_como_falso();
    *falso.plan_de_sync.lock().expect("plan") = Some((
        vec![paso_de_plan(
            1,
            "a.md",
            norte_proto::methods::SyncStepKind::Copy,
        )],
        plan_cerrado(1),
    ));
    let backend = Arc::new(falso);
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separar_los_paneles(&h, &mut sub).await;
    ejecutar_por_paleta(&h, &mut sub, "pane.sync-dirs").await;
    let vista = siguiente_sync(&mut sub).await.expect("opens");
    assert!(vista.running);

    // The daemon says the Task was cancelled.
    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Cancelled);

    for _ in 0..40 {
        let v = siguiente_sync(&mut sub).await.expect("still open");
        if !v.running {
            assert!(
                !v.can_approve,
                "a cancelled plan is not approved, whether it closed or not: {v:?}"
            );
            return;
        }
    }
    panic!("the panel kept saying it is planning");
}

/// With the plan panel up front, another one cannot be requested.
///
/// Relaunching left the previous panel without leaving and its Task
/// uncancelled — the daemon kept walking a tree for a plan nobody can see
/// anymore — and, with a request in flight, the second keystroke was
/// killing both panels.
#[tokio::test]
async fn con_el_panel_del_plan_delante_no_se_pide_otro() {
    let falso = arbol_como_falso();
    *falso.plan_de_sync.lock().expect("plan") = Some((
        vec![paso_de_plan(
            1,
            "a.md",
            norte_proto::methods::SyncStepKind::Copy,
        )],
        plan_cerrado(1),
    ));
    let backend = Arc::new(falso);
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separar_los_paneles(&h, &mut sub).await;
    ejecutar_por_paleta(&h, &mut sub, "pane.sync-dirs").await;
    let _ = siguiente_sync(&mut sub).await.expect("opens");

    // With the panel up front, the keys are ITS OWN: `ctrl+p` does not open
    // the palette, which is the path through which the command would be
    // repeated. It is the first of the two locks.
    h.dispatch(tecla_mod("p", true, false))
        .await
        .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let foto = siguiente_foto(&mut sub).await;
    assert!(
        foto.palette.is_none(),
        "the plan panel cannot let the palette's key through"
    );
    assert!(foto.sync.is_some(), "and the panel is still up front");
    assert_eq!(
        backend.planes_pedidos.lock().expect("planes").len(),
        1,
        "no second plan was requested"
    );
}

/// A daemon that does not know how to plan does not leave the request
/// hanging.
#[tokio::test]
async fn un_plan_que_el_daemon_rechaza_no_deja_nada_pendiente() {
    let falso = arbol_como_falso();
    // No plan: the fake answers `Unsupported`.
    let backend = Arc::new(falso);
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separar_los_paneles(&h, &mut sub).await;
    ejecutar_por_paleta(&h, &mut sub, "pane.sync-dirs").await;
    // The failure is SAID.
    for _ in 0..40 {
        if siguiente_aviso(&mut sub).await.starts_with("err-") {
            break;
        }
    }
    // And the next attempt can be made: the request did not get stuck.
    let ack = ejecutar_por_paleta_ack(&h, &mut sub, "pane.sync-dirs").await;
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "the previous request left the host stuck: {ack:?}"
    );
}

/// A plan that DELETES trees asks TWICE, and the second is only answered
/// with `y`.
///
/// The second question is not ceremony: it is composed by the shared model
/// and only appears when the plan deletes or leaves something with no way
/// back. Always asking is what teaches people to answer without reading.
#[tokio::test]
async fn un_plan_que_borra_pregunta_dos_veces() {
    let mut done = plan_cerrado(1);
    done.counts = norte_proto::methods::SyncCounts {
        delete_tree: 1,
        ..Default::default()
    };
    let falso = arbol_como_falso();
    *falso.plan_de_sync.lock().expect("plan") = Some((
        vec![norte_proto::methods::SyncStep {
            reversal: Some(norte_proto::methods::StepReversal::RestoreTrash),
            ..paso_de_plan(1, "viejo", norte_proto::methods::SyncStepKind::DeleteTree)
        }],
        done,
    ));
    let backend = Arc::new(falso);
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separar_los_paneles(&h, &mut sub).await;
    ejecutar_por_paleta(&h, &mut sub, "pane.sync-dirs").await;
    let mut vista = siguiente_sync(&mut sub).await.expect("opens");
    for _ in 0..20 {
        if vista.can_approve {
            break;
        }
        vista = siguiente_sync(&mut sub).await.expect("still open");
    }
    assert!(vista.can_approve, "{}", vista.status);

    // The first `a` only ASKS.
    // TODO(translation): review — the comment says `a`, but the dispatched
    // key below is "y"; kept as in the source.
    h.dispatch(tecla("y")).await.expect("host alive");
    let preguntando = siguiente_sync(&mut sub).await.expect("still open");
    assert!(
        preguntando.confirming.is_some(),
        "a plan that deletes trees asks again: {preguntando:?}"
    );
    asentar().await;
    assert!(
        backend.aplicados.lock().expect("aplicados").is_empty(),
        "and it has still applied nothing"
    );

    // A key that is not `y` WITHDRAWS the question and applies nothing.
    h.dispatch(tecla("n")).await.expect("host alive");
    let retirada = siguiente_sync(&mut sub).await.expect("still open");
    assert!(retirada.confirming.is_none());
    asentar().await;
    assert!(backend.aplicados.lock().expect("aplicados").is_empty());

    // `a` and then `y`: now it does, and with the hash the CORE returned.
    h.dispatch(tecla("y")).await.expect("host alive");
    let _ = siguiente_sync(&mut sub).await;
    h.dispatch(tecla("y")).await.expect("host alive");
    let aplicados = anotados(&backend, "the plan applied", 1, |f| {
        f.aplicados.lock().expect("aplicados").clone()
    })
    .await;
    assert_eq!(aplicados.len(), 1, "only once");
}

/// A REJECTED apply releases the latch; one with an UNKNOWN outcome does
/// not.
///
/// The daemon answering "no" and the connection dropping after asking are
/// different things: in the first case the destination is known to be
/// intact and retrying is correct; in the second the request may have
/// arrived, and offering `y` again is offering to apply the same plan twice
/// over the same destination.
#[tokio::test]
async fn un_apply_de_resultado_desconocido_no_se_reofrece() {
    for (error, se_reofrece) in [
        (
            norte_proto::Error::PolicyDenied {
                rule: "policy-rule".to_owned(),
            },
            true,
        ),
        (norte_proto::Error::Io { retryable: true }, false),
    ] {
        let falso = arbol_como_falso();
        *falso.plan_de_sync.lock().expect("plan") = Some((
            vec![paso_de_plan(
                1,
                "a.md",
                norte_proto::methods::SyncStepKind::Copy,
            )],
            plan_cerrado(1),
        ));
        *falso.error_al_aplicar.lock().expect("error") = Some(error.clone());
        let backend = Arc::new(falso);
        let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
        let mut sub = h.subscribe();
        separar_los_paneles(&h, &mut sub).await;
        ejecutar_por_paleta(&h, &mut sub, "pane.sync-dirs").await;
        let mut vista = siguiente_sync(&mut sub).await.expect("opens");
        for _ in 0..20 {
            if vista.can_approve {
                break;
            }
            vista = siguiente_sync(&mut sub).await.expect("still open");
        }
        assert!(vista.can_approve, "{}", vista.status);

        h.dispatch(tecla("y")).await.expect("host alive");
        anotados(&backend, "the apply requested", 1, |f| {
            f.aplicados.lock().expect("aplicados").clone()
        })
        .await;
        // The apply's outcome comes back through the mailbox: it is let run
        // before asking what the screen shows.
        asentar().await;
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let tras = siguiente_foto(&mut sub).await.sync.expect("still open");
        assert_eq!(
            tras.can_approve, se_reofrece,
            "{error:?} left the screen offering approve = {}",
            tras.can_approve
        );
    }
}

/// With the apply IN FLIGHT, `Escape` asks to cancel and does NOT close the
/// panel.
///
/// Closing it loses the report — and with it the count, the failures and
/// the undo handle — over a destination that is being rewritten.
#[tokio::test]
async fn con_el_apply_en_vuelo_escape_no_cierra() {
    let falso = arbol_como_falso();
    *falso.plan_de_sync.lock().expect("plan") = Some((
        vec![paso_de_plan(
            1,
            "a.md",
            norte_proto::methods::SyncStepKind::Copy,
        )],
        plan_cerrado(1),
    ));
    let backend = Arc::new(falso);
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separar_los_paneles(&h, &mut sub).await;
    ejecutar_por_paleta(&h, &mut sub, "pane.sync-dirs").await;
    let mut vista = siguiente_sync(&mut sub).await.expect("opens");
    for _ in 0..20 {
        if vista.can_approve {
            break;
        }
        vista = siguiente_sync(&mut sub).await.expect("still open");
    }
    // This plan deletes nothing and undoes entirely: there is no second
    // question. `y` is `dialog.approve` in the preset (#287): approving a
    // plan is saying yes to what is already up front, not a bare "confirm".
    h.dispatch(tecla("y")).await.expect("host alive");
    anotados(&backend, "the plan applied", 1, |f| {
        f.aplicados.lock().expect("aplicados").clone()
    })
    .await;
    assert_eq!(backend.aplicados.lock().expect("aplicados").len(), 1);

    // The FIRST `Escape` asks to stop and does NOT close: closing loses the
    // report on a destination half rewritten. And it asks to stop the
    // APPLY's task, not the plan's, which finished a while ago.
    h.dispatch(tecla("Escape")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let foto = siguiente_foto(&mut sub).await;
    let panel = foto.sync.expect("the panel stays");
    assert!(
        panel.cancel_requested,
        "and the screen acknowledges it was heard"
    );
    hasta(&backend, "the stop requested from the daemon", |f| {
        (!f.canceladas_por_id.lock().expect("canceladas").is_empty()).then_some(())
    })
    .await;
    let paradas = backend
        .canceladas_por_id
        .lock()
        .expect("canceladas")
        .clone();
    assert!(
        paradas.iter().any(|id| *id >= 500),
        "the apply's task was asked to stop: {paradas:?}"
    );

    // The SECOND one closes, whatever happens with the report: without this
    // exit, the writing screen was the only one in norte with no exit.
    h.dispatch(tecla("Escape")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    for _ in 0..20 {
        if siguiente_foto(&mut sub).await.sync.is_none() {
            return;
        }
    }
    panic!("the panel could not be closed");
}

/// The report arrives and the panel says so, with the failures one by one.
#[tokio::test]
async fn el_informe_de_la_sincronizacion_dice_lo_que_fallo() {
    let falso = arbol_como_falso();
    *falso.plan_de_sync.lock().expect("plan") = Some((
        vec![paso_de_plan(
            1,
            "a.md",
            norte_proto::methods::SyncStepKind::Copy,
        )],
        plan_cerrado(1),
    ));
    *falso.informe_de_sync.lock().expect("informe") =
        Some(norte_proto::methods::SyncReportResult {
            done: 0,
            failed: 1,
            skipped: 0,
            bytes: 0,
            failures: vec![norte_proto::methods::SyncFailure {
                rel: norte_proto::methods::RelPath::new(vec![
                    norte_proto::Segment::new(b"a.md".to_vec()).expect("segmento"),
                ]),
                dest_rel: None,
                cause: norte_proto::methods::SyncFailureCause::Denied,
                kind: norte_proto::methods::SyncStepKind::Copy,
            }],
            // With no journal batch: nothing to undo, and the panel will
            // say so.
            batch_id: None,
            dest_trash: norte_proto::methods::DestTrash::Restorable,
        });
    let backend = Arc::new(falso);
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separar_los_paneles(&h, &mut sub).await;
    ejecutar_por_paleta(&h, &mut sub, "pane.sync-dirs").await;
    let mut vista = siguiente_sync(&mut sub).await.expect("opens");
    for _ in 0..20 {
        if vista.can_approve {
            break;
        }
        vista = siguiente_sync(&mut sub).await.expect("still open");
    }
    h.dispatch(tecla("y")).await.expect("host alive");
    anotados(&backend, "the plan applied", 1, |f| {
        f.aplicados.lock().expect("aplicados").clone()
    })
    .await;
    // The daemon finishes the apply's Task.
    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    for _ in 0..40 {
        let v = siguiente_sync(&mut sub).await.expect("still open");
        if !v.failures.is_empty() {
            assert_eq!(v.failures[0].path, "a.md");
            assert!(!v.failures[0].cause.is_empty());
            return;
        }
    }
    panic!("the report never reached the panel");
}

/// Approving an extension's capabilities ASKS, and the question enumerates
/// them.
///
/// "Do you approve org.example.foo?" without saying what it grants is not a
/// decision: it is a button. Each capability goes on its own LINE and with
/// its own flag, because the one painted differently from what it says is
/// exactly the one a hostile manifest writes to sneak in among the real
/// ones.
#[tokio::test]
async fn aprobar_pregunta_y_enumera_las_capabilities() {
    let mut ext = extension("acme.ftp", "FTP de ACME", false);
    ext.approved = false;
    ext.enabled = false;
    ext.capabilities = vec!["fs-read".to_owned(), "net\u{202e}".to_owned()];
    let backend = arbol_con_plugins(vec![ext], &[]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host alive");
    let _ = extensiones_cargadas(&mut sub).await;

    h.dispatch(tecla("a")).await.expect("host alive");
    let dialogos = siguientes_dialogos(&mut sub).await;
    let d = dialogos.last().expect("the question");
    assert_eq!(d.title_key, "modal-extension-approve-title");
    assert_eq!(d.body.len(), 3, "the name and the TWO capabilities: {d:?}");
    assert!(!d.body[1].hostile, "the clean capability is not marked");
    assert!(
        d.body[2].hostile,
        "and the one with the bidi override IS: which one differs is the whole question"
    );
    // And who is asking, by the id the core validates: two extensions can
    // share a name, and the name is written by the manifest.
    assert_eq!(
        d.subject.as_ref().map(|s| s.text.as_str()),
        Some("acme.ftp")
    );
    asentar().await;
    assert!(
        backend.gobierno.lock().expect("gobierno").is_empty(),
        "and nothing has been granted yet"
    );

    // And the affirmative response grants, and the catalogue is
    // RE-REQUESTED: what the screen says about who can read your files is
    // not decided by local optimism.
    h.dispatch(UiAction::Dialog {
        id: d.id,
        choice: "approve".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    for _ in 0..20 {
        let Some(v) = siguiente_extensiones(&mut sub).await else {
            continue;
        };
        if v.rows.first().is_some_and(|r| r.approved) {
            assert_eq!(
                backend.gobierno.lock().expect("gobierno").as_slice(),
                // With the ANCHOR that was shown (#282): what gets granted
                // has to be what the human read, and the core refuses if
                // the manifest changed between the question and the yes.
                ["approval:acme.ftp:true:digest-de-acme.ftp"]
            );
            return;
        }
    }
    panic!("the catalogue never reflected the grant");
}

/// In read-only nothing is granted: it is SAID.
///
/// It is the same switch that decides whether this window deletes. Granting
/// capabilities is the extension system's security decision, and a window
/// mounted with no effects does not make it.
#[tokio::test]
async fn en_solo_lectura_no_se_gobierna_ninguna_extension() {
    let mut ext = extension("acme.ftp", "FTP de ACME", false);
    ext.approved = false;
    let backend = arbol_con_plugins(vec![ext], &[]);
    let (h, _snap) = host_solo_lectura(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host alive");
    let _ = extensiones_cargadas(&mut sub).await;
    for tecla_de in ["a", "e", "d"] {
        let ack = h.dispatch(tecla(tecla_de)).await.expect("host alive");
        assert!(
            matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-read-only"),
            "`{tecla_de}` in read-only: {ack:?}"
        );
    }
    // And the button (bridge 61) goes through the same door as the key.
    let ack = h
        .dispatch(UiAction::ExtensionGovern {
            row: 0,
            id: "acme.ftp".to_owned(),
            change: norte_ui_host::action::ExtensionChange::Uninstall,
        })
        .await
        .expect("host alive");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-read-only"),
        "uninstalling by button in read-only: {ack:?}"
    );
    asentar().await;
    assert!(backend.gobierno.lock().expect("gobierno").is_empty());
}

/// Enabling an extension WITHOUT approving it is refused, and it says why.
///
/// Without approved capabilities the core does not load it: saying
/// "enabled" about something that is not running is the screen lying.
#[tokio::test]
async fn encender_sin_aprobar_se_rehusa() {
    let mut ext = extension("acme.ftp", "FTP de ACME", false);
    ext.approved = false;
    ext.enabled = false;
    let backend = arbol_con_plugins(vec![ext], &[]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host alive");
    let _ = extensiones_cargadas(&mut sub).await;
    let ack = h.dispatch(tecla("e")).await.expect("host alive");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key }
            if reason_key == "host-extension-not-approved"),
        "{ack:?}"
    );
    asentar().await;
    assert!(backend.gobierno.lock().expect("gobierno").is_empty());
}

/// A `bool` CYCLES with `Enter` and gets written; an `int` opens the buffer,
/// and what is typed is validated against the SCHEMA's bounds before
/// leaving.
#[tokio::test]
async fn el_editor_de_config_cicla_teclea_y_valida() {
    let ext = extension("acme.ftp", "FTP de ACME", false);
    let backend = arbol_con_esquema(vec![ext], &[("acme.ftp", esquema_de_prueba())]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host alive");
    let _ = extensiones_cargadas(&mut sub).await;
    h.dispatch(tecla("Enter")).await.expect("host alive");
    let ficha = ficha_abierta(&mut sub).await;
    assert_eq!(ficha.config.len(), 4);
    assert!(
        !ficha.config[3].editable,
        "a `kind` this build does not know is read-only: {:?}",
        ficha.config[3]
    );

    // The first key is the `bool`: `Enter` cycles it and sends it. It WAITS
    // for it to arrive instead of sleeping a fixed span: under load, forty
    // milliseconds are no guarantee, and a test that asserts presence
    // against the clock is intermittently red.
    h.dispatch(tecla("Enter")).await.expect("host alive");
    anotados(&backend, "the `bool`'s write", 1, |f| {
        f.escrituras.lock().expect("escrituras").clone()
    })
    .await;
    assert_eq!(
        backend.escrituras.lock().expect("escrituras").as_slice(),
        [(
            "acme.ftp".to_owned(),
            "verbose".to_owned(),
            "true".to_owned()
        )]
    );
    // And the screen moves with it: the operand and what is painted are two
    // halves of the same row, and updating only one left the cell with the
    // old value — the next `Enter` would send it back to where it was.
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let ficha = siguiente_foto(&mut sub)
        .await
        .extensions
        .expect("still open")
        .detail
        .expect("with a card");
    assert_eq!(ficha.config[0].value, "true");

    // The second one is the `int`: `Enter` opens the buffer and writes
    // NOTHING.
    h.dispatch(tecla("ArrowDown")).await.expect("host alive");
    h.dispatch(tecla("Enter")).await.expect("host alive");
    esperar_buffer(&h, &mut sub).await;
    // A value outside the bounds is refused HERE and does not travel: the
    // daemon validates again, but saying it beforehand saves the trip and
    // states the bound.
    for c in ["Backspace", "Backspace", "9", "9", "9"] {
        h.dispatch(tecla(c)).await.expect("host alive");
    }
    let ack = h.dispatch(tecla("Enter")).await.expect("host alive");
    // The ACK carries a key with no variables — nobody substitutes
    // `{ $min }` on that path — the bounds go in the notice, which does
    // get translated with them.
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key }
            if reason_key == "host-value-rejected"),
        "{ack:?}"
    );
    asentar().await;
    assert_eq!(
        backend.escrituras.lock().expect("escrituras").len(),
        1,
        "the out-of-range value was not sent"
    );
    // And the buffer STAYS open: a rejected commit does not close the
    // field, which is what lets someone correct it without retyping it
    // whole.
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert!(
        siguiente_foto(&mut sub)
            .await
            .extensions
            .expect("still open")
            .detail
            .expect("with a card")
            .editing
            .is_some()
    );

    // And one inside the bounds does.
    h.dispatch(tecla("Enter")).await.expect("host alive");
    esperar_buffer(&h, &mut sub).await;
    for c in ["Backspace", "Backspace", "Backspace", "4", "2"] {
        h.dispatch(tecla(c)).await.expect("host alive");
    }
    h.dispatch(tecla("Enter")).await.expect("host alive");
    let escrituras = anotados(&backend, "the typed key, sent", 2, |f| {
        f.escrituras.lock().expect("escrituras").clone()
    })
    .await;
    assert_eq!(escrituras[1].1, "timeout");
    assert_eq!(escrituras[1].2, "42");
}

/// While a value is being TYPED, `a` is a letter, not a grant.
///
/// It is the same fixed regime as any field in this host: resolving letters
/// as gestures there would turn typing "casa" into two capability grants.
#[tokio::test]
async fn tecleando_un_valor_las_letras_son_letras() {
    let ext = extension("acme.ftp", "FTP de ACME", false);
    let backend = arbol_con_esquema(vec![ext], &[("acme.ftp", esquema_de_prueba())]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host alive");
    let _ = extensiones_cargadas(&mut sub).await;
    h.dispatch(tecla("Enter")).await.expect("host alive");
    let _ = ficha_abierta(&mut sub).await;
    // To the `string` key, which is the THIRD one (`verbose`, `timeout`,
    // `greeting`, and the fourth is the unknown `kind`'s).
    for _ in 0..2 {
        h.dispatch(tecla("ArrowDown")).await.expect("host alive");
    }
    h.dispatch(tecla("Enter")).await.expect("host alive");
    esperar_buffer(&h, &mut sub).await;
    h.dispatch(tecla("a")).await.expect("host alive");
    asentar().await;
    assert!(
        backend.gobierno.lock().expect("gobierno").is_empty(),
        "the typed `a` granted no capabilities"
    );
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let foto = siguiente_foto(&mut sub).await;
    let editando = foto
        .extensions
        .expect("still open")
        .detail
        .expect("with a card")
        .editing
        .expect("editing");
    assert!(editando.ends_with('a'), "the letter went in: {editando:?}");
}

/// A tree with a catalogue AND `[config]` schemas.
pub(super) fn arbol_con_esquema(
    plugins: Vec<norte_proto::methods::PluginInfo>,
    esquemas: &[(&str, Vec<norte_proto::methods::PluginConfigKeyWire>)],
) -> Arc<Falso> {
    let base = arbol();
    let mut f = Falso {
        plugins: plugins.into(),
        esquemas: esquemas
            .iter()
            .map(|(id, keys)| ((*id).to_owned(), keys.clone()))
            .collect(),
        ..Falso::default()
    };
    f.arbol.clone_from(&base.arbol);
    Arc::new(f)
}

/// El esquema de prueba: un `bool`, un `int` acotado y un `kind` que este
/// build no conoce.
pub(super) fn esquema_de_prueba() -> Vec<norte_proto::methods::PluginConfigKeyWire> {
    vec![
        norte_proto::methods::PluginConfigKeyWire {
            key: "verbose".to_owned(),
            kind: "bool".to_owned(),
            default: "false".to_owned(),
            min: None,
            max: None,
            values: Vec::new(),
            description: None,
            value: "false".to_owned(),
        },
        norte_proto::methods::PluginConfigKeyWire {
            key: "timeout".to_owned(),
            kind: "int".to_owned(),
            default: "10".to_owned(),
            min: Some(1),
            max: Some(300),
            values: Vec::new(),
            description: None,
            value: "30".to_owned(),
        },
        norte_proto::methods::PluginConfigKeyWire {
            key: "greeting".to_owned(),
            kind: "string".to_owned(),
            default: "hola".to_owned(),
            min: None,
            max: None,
            values: Vec::new(),
            description: None,
            value: "hola".to_owned(),
        },
        norte_proto::methods::PluginConfigKeyWire {
            key: "future".to_owned(),
            kind: "duration".to_owned(),
            default: "1s".to_owned(),
            min: None,
            max: None,
            values: Vec::new(),
            description: None,
            value: "1s".to_owned(),
        },
    ]
}

/// Waits for the chosen extension's card to be open.
pub(super) async fn ficha_abierta(
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::dto::ExtensionDetailView {
    for _ in 0..20 {
        let Some(v) = siguiente_extensiones(sub).await else {
            continue;
        };
        if let Some(d) = v.detail {
            return d;
        }
    }
    panic!("the card never opened");
}

/// The palette offers extension commands, and running them shows what they
/// printed.
///
/// The rows are composed by the SHARED model: only approved and enabled
/// ones — the same gate `plugin.run_command` enforces on its own — and with
/// the prefix that stops a third party's command from disguising itself as
/// one of the host's own. The output is third-party text: it gets masked,
/// capped, and it says when it was cut off.
#[tokio::test]
async fn la_paleta_ejecuta_un_comando_de_extension_y_ensena_su_salida() {
    let mut ext = extension("acme.ftp", "FTP de ACME", false);
    ext.commands = vec![norte_proto::methods::PluginCommandInfo {
        id: "greet".to_owned(),
        title: "Saludar".to_owned(),
        kind: norte_proto::methods::PluginCommandKind::Command,
    }];
    let backend = arbol_con_plugins(vec![ext], &[]);
    *backend.salida_de_comando.lock().expect("salida") =
        Some(Ok(format!("hola\u{202e}{}", "x".repeat(5_000))));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla_mod("p", true, false))
        .await
        .expect("host alive");
    // Plugin rows are MERGED IN when the daemon answers: the palette is
    // painted first, with the host's own commands.
    let mut llegaron = false;
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let p = siguiente_foto(&mut sub).await.palette.expect("open");
        if p.rows.iter().any(|r| r.text.contains("Saludar")) {
            llegaron = true;
            break;
        }
    }
    assert!(llegaron, "the extension command's row never arrived");
    // It gets narrowed by typing, which is what the palette is for: the
    // command's title is folded by the shared model together with its
    // description.
    for c in "Saludar".chars() {
        h.dispatch(tecla(&c.to_string())).await.expect("host alive");
    }
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let p = siguiente_foto(&mut sub).await.palette.expect("open");
    assert_eq!(p.rows.len(), 1, "the filter leaves a single row: {p:?}");
    h.dispatch(tecla("Enter")).await.expect("host alive");

    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let foto = siguiente_foto(&mut sub).await;
        let Some(salida) = foto.plugin_output else {
            continue;
        };
        assert_eq!(
            backend.ejecutados.lock().expect("ejecutados").as_slice(),
            [("acme.ftp".to_owned(), "greet".to_owned())]
        );
        assert!(salida.text_hostile, "the bidi override is said: {salida:?}");
        assert!(
            !salida.lines.iter().any(|l| l.contains('\u{202e}')),
            "and it is masked"
        );
        assert!(salida.truncated, "and that it was cut off, too: {salida:?}");
        assert_eq!(salida.command.text, "Saludar");
        assert_eq!(salida.plugin_id, "acme.ftp", "and who printed it, by id");

        // And `Escape` closes it without touching anything underneath.
        h.dispatch(tecla("Escape")).await.expect("host alive");
        h.dispatch(UiAction::Resync).await.expect("host alive");
        assert!(siguiente_foto(&mut sub).await.plugin_output.is_none());
        return;
    }
    panic!(
        "the command's output never arrived; ejecutados = {:?}",
        backend.ejecutados.lock().expect("ejecutados")
    );
}

/// C3 (ADR 0095): a RENAMER row in the palette asks the plugin for a plan
/// over what is marked and puts it into the SAME review as the AI's plan —
/// with the core's verdict along for the ride — with no model involved.
#[tokio::test]
async fn la_paleta_pide_el_plan_a_un_renamer_y_lo_revisa_como_el_de_la_ia() {
    let pares = [("ep1.mkv", "2026-09-03_ep1.mkv")];
    let mut ext = extension("org.norte.date-prefix", "Date prefix", false);
    ext.commands = vec![norte_proto::methods::PluginCommandInfo {
        id: "by-date".to_owned(),
        title: "Rename by date".to_owned(),
        kind: norte_proto::methods::PluginCommandKind::Renamer,
    }];
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        pares
            .iter()
            .map(|(from, _)| (from.as_bytes().to_vec(), false))
            .collect::<Vec<_>>(),
    );
    f.plan_renamer = Some(
        pares
            .iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect(),
    );
    f.veredicto = Some(veredicto_ok(&pares));
    *f.plugins.lock().expect("plugins") = vec![ext];
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // Marks the file: the renamer acts on what is marked.
    por_la_paleta(&h, &mut sub, "mark.all").await;
    h.dispatch(tecla_mod("p", true, false))
        .await
        .expect("host alive");
    let mut llego = false;
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let p = siguiente_foto(&mut sub).await.palette.expect("open");
        if p.rows.iter().any(|r| r.text.contains("Rename by date")) {
            llego = true;
            break;
        }
    }
    assert!(llego, "the renamer's row never arrived");
    for c in "Rename by date".chars() {
        h.dispatch(tecla(&c.to_string())).await.expect("host alive");
    }
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let p = siguiente_foto(&mut sub).await.palette.expect("open");
    assert_eq!(p.rows.len(), 1, "{p:?}");
    // The label comes from the process's GLOBAL catalogue (like the
    // extension commands' does), so either one is valid here.
    assert!(
        p.rows[0].text.starts_with("[renombrar]") || p.rows[0].text.starts_with("[rename]"),
        "a different label than a command's: {}",
        p.rows[0].text
    );
    h.dispatch(tecla("Enter")).await.expect("host alive");

    let mut v = siguiente_revision(&mut sub)
        .await
        .expect("the review opens");
    assert_eq!(v.pairs[0].from.text, "ep1.mkv");
    assert_eq!(v.pairs[0].to.text, "2026-09-03_ep1.mkv");
    for _ in 0..40 {
        if v.confirmable {
            break;
        }
        v = siguiente_revision(&mut sub).await.expect("still open");
    }
    assert!(v.confirmable, "the core gave its verdict");
    let pedidos = backend.renamers_pedidos.lock().expect("mutex").clone();
    assert_eq!(
        pedidos,
        vec![(
            "org.norte.date-prefix".to_owned(),
            "by-date".to_owned(),
            vec!["ep1.mkv".to_owned()]
        )]
    );
    assert!(
        backend.instrucciones.lock().expect("mutex").is_empty(),
        "the model was not asked for anything"
    );
}

/// A renamer that REFUSES says why (#332): the phrase reaches the status
/// bar exactly as the daemon capped it, no review opens, and it is not a
/// generic error — "approve my capability" has to be readable.
#[tokio::test]
async fn un_renamer_que_rehusa_dice_por_que_en_la_barra() {
    let mut ext = extension("org.norte.date-prefix", "Date prefix", false);
    ext.commands = vec![norte_proto::methods::PluginCommandInfo {
        id: "by-date".to_owned(),
        title: "Rename by date".to_owned(),
        kind: norte_proto::methods::PluginCommandKind::Renamer,
    }];
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"ep1.mkv".to_vec(), false)]);
    f.renamer_rehusa = Some("needs the location capability".to_owned());
    *f.plugins.lock().expect("plugins") = vec![ext];
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    por_la_paleta(&h, &mut sub, "mark.all").await;
    h.dispatch(tecla_mod("p", true, false))
        .await
        .expect("host alive");
    let mut llego = false;
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let p = siguiente_foto(&mut sub).await.palette.expect("open");
        if p.rows.iter().any(|r| r.text.contains("Rename by date")) {
            llego = true;
            break;
        }
    }
    assert!(llego, "the renamer's row never arrived");
    for c in "Rename by date".chars() {
        h.dispatch(tecla(&c.to_string())).await.expect("host alive");
    }
    h.dispatch(tecla("Enter")).await.expect("host alive");

    // The same snapshot carries the phrase and the absence of a review:
    // waiting for ANOTHER snapshot afterward would hang, because nothing
    // else changes.
    let (msg, revision) = foto_hasta(&h, &mut sub, "the renamer's phrase in the bar", |s| {
        s.status
            .message
            .clone()
            .filter(|m| m.contains("needs the location capability"))
            .map(|m| (m, s.ai_rename.is_some()))
    })
    .await;
    assert!(
        !msg.contains("no soportado") && !msg.contains("not supported"),
        "it is not a generic error: {msg}"
    );
    assert!(!revision, "with no plan there is no review");
}

/// In read-only the palette does NOT offer extension commands.
///
/// What a command does is decided by the PLUGIN: it can write. A window
/// mounted with no effects does not launch it, and therefore does not offer
/// it either — it is the same rule already applied to the host's own
/// commands: offering what is going to be refused is promising something
/// that will not happen.
///
/// What IS requested is the catalogue (phase 3): it carries the declaration
/// of what panels the plugins contribute, and without it a saved layout
/// with a panel leaves a blank box the reader cannot identify. Requesting it
/// is not offering it — what this test pins down is that it is neither
/// offered nor launched.
#[tokio::test]
async fn en_solo_lectura_no_se_ejecuta_un_comando_de_extension() {
    let mut ext = extension("acme.ftp", "FTP de ACME", false);
    ext.commands = vec![norte_proto::methods::PluginCommandInfo {
        id: "greet".to_owned(),
        title: "Saludar".to_owned(),
        kind: norte_proto::methods::PluginCommandKind::Command,
    }];
    let backend = arbol_con_plugins(vec![ext], &[]);
    let (h, _snap) = host_solo_lectura(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla_mod("p", true, false))
        .await
        .expect("host alive");
    for _ in 0..10 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let p = siguiente_foto(&mut sub).await.palette.expect("open");
        assert!(
            !p.rows.iter().any(|r| r.text.contains("Saludar")),
            "a window with no effects does not offer to run third-party code"
        );
        asentar().await;
    }
    // And nothing ran, which is what the rule protects. The catalogue does
    // travel — `pedir_paneles` requests it to declare kinds, even with no
    // effects — so counting round trips stopped saying anything about what
    // is offered.
    assert!(backend.ejecutados.lock().expect("ejecutados").is_empty());
}

/// Waits for the card's editing buffer to be open.
///
/// By RESYNC and not by blindly consuming patches: a loop that reads N
/// updates runs out of them as soon as the test sends a snapshot for
/// another reason, and then fails on a deadline saying something that is
/// not true.
pub(super) async fn esperar_buffer(h: &UiHost, sub: &mut norte_ui_host::UiSubscription) {
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let abierto = siguiente_foto(sub)
            .await
            .extensions
            .and_then(|e| e.detail)
            .is_some_and(|d| d.editing.is_some());
        if abierto {
            return;
        }
    }
    panic!("the editing buffer never opened");
}

/// A governance change that FAILS asks for the catalogue again.
///
/// The failure carries THIS side's deadline, which is not "it did not
/// happen" but "it is not known": the daemon may have granted the
/// capabilities and taken a while to answer. Leaving the row saying "not
/// approved" is the same lie as local optimism, in pessimistic form — and
/// the only thing that resolves an unknown is asking.
#[tokio::test]
async fn un_gobierno_fallido_vuelve_a_preguntar_al_core() {
    let mut ext = extension("acme.ftp", "FTP de ACME", false);
    ext.approved = false;
    ext.enabled = false;
    ext.capabilities = vec!["fs-read".to_owned()];
    let backend = arbol_con_plugins(vec![ext], &[]);
    *backend.error_al_gobernar.lock().expect("gobierno") =
        Some(norte_proto::Error::ProviderUnavailable { retryable: true });
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host alive");
    let _ = extensiones_cargadas(&mut sub).await;
    let pedidos = backend
        .catalogos_pedidos
        .load(std::sync::atomic::Ordering::SeqCst);

    h.dispatch(tecla("a")).await.expect("host alive");
    let dialogos = siguientes_dialogos(&mut sub).await;
    let d = dialogos.last().expect("the question");
    h.dispatch(UiAction::Dialog {
        id: d.id,
        choice: "approve".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");

    hasta(
        &backend,
        "the catalogue re-requested after governance",
        |f| {
            let ahora = f
                .catalogos_pedidos
                .load(std::sync::atomic::Ordering::SeqCst);
            (ahora > pedidos).then_some(())
        },
    )
    .await;
}

/// With a command's output on screen, the keys are ITS OWN.
///
/// It paints full-screen, so a modal that let through a key it does not
/// understand is not a modal: `Enter` on that panel was reaching what was
/// underneath, where a confirmation could be waiting for a yes the reader
/// does not see — and the moment is chosen by the PLUGIN, which decides
/// when it answers.
#[tokio::test]
async fn la_salida_de_un_comando_no_deja_pasar_teclas() {
    let mut ext = extension("acme.ftp", "FTP de ACME", false);
    ext.commands = vec![norte_proto::methods::PluginCommandInfo {
        id: "greet".to_owned(),
        title: "Saludar".to_owned(),
        kind: norte_proto::methods::PluginCommandKind::Command,
    }];
    let backend = arbol_con_plugins(vec![ext], &[]);
    *backend.salida_de_comando.lock().expect("salida") = Some(Ok("hola".to_owned()));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let antes = siguiente_foto_tras_resync(&h, &mut sub).await;
    let cursor_antes = listado(&antes).cursor;

    h.dispatch(tecla_mod("p", true, false))
        .await
        .expect("host alive");
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let p = siguiente_foto(&mut sub).await.palette.expect("open");
        if p.rows.iter().any(|r| r.text.contains("Saludar")) {
            break;
        }
    }
    for c in "Saludar".chars() {
        h.dispatch(tecla(&c.to_string())).await.expect("host alive");
    }
    h.dispatch(tecla("Enter")).await.expect("host alive");
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if siguiente_foto(&mut sub).await.plugin_output.is_some() {
            break;
        }
    }

    // A navigation key with the panel open does NOT move what is
    // underneath.
    h.dispatch(tecla("ArrowDown")).await.expect("host alive");
    let durante = siguiente_foto_tras_resync(&h, &mut sub).await;
    assert!(durante.plugin_output.is_some(), "the panel stays");
    assert_eq!(
        listado(&durante).cursor,
        cursor_antes,
        "the listing's cursor did not move underneath the panel"
    );

    // And `Enter` CLOSES it, which is the reflex of someone who just read
    // it.
    h.dispatch(tecla("Enter")).await.expect("host alive");
    let despues = siguiente_foto_tras_resync(&h, &mut sub).await;
    assert!(despues.plugin_output.is_none());
    assert_eq!(listado(&despues).cursor, cursor_antes);
}

/// Requests a snapshot and waits for it.
pub(super) async fn siguiente_foto_tras_resync(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::ViewSnapshot {
    h.dispatch(UiAction::Resync).await.expect("host alive");
    siguiente_foto(sub).await
}

/// The agents panel lists the sessions THIS window saw ask for permission,
/// and one of them gets undone whole from there (#276).
///
/// The operand is CHOSEN from a list: a session id typed by hand on a
/// governance surface is an id that can be gotten wrong, and undoing the
/// wrong session is undoing someone else's work. And the list says what it
/// is — what this window has seen, not the system's census — because there
/// is no protocol method that enumerates live sessions.
#[tokio::test]
async fn el_panel_de_agentes_deshace_la_sesion_elegida() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.aprobaciones.lock().expect("aprobaciones") = Some(rx);
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // Two sessions, and the one with the hostile id is the LAST seen: the
    // list goes from most recent to oldest.
    for (id, op, aid) in [
        ("agente-2", "copy", 11_u64),
        ("agente\u{202e}1", "delete", 12),
    ] {
        tx.send(norte_proto::methods::PolicyApprovalRequired {
            approval_id: aid,
            session: Some(id.to_owned()),
            op: op.to_owned(),
            paths: vec!["mem:///casa/x".to_owned()],
            paths_total: 1,
            ttl_ms: 30_000,
            detail: norte_proto::methods::ApprovalDetail::default(),
        })
        .expect("the host is listening");
        let dialogos = siguientes_dialogos(&mut sub).await;
        // It is DENIED to get it out of the way: an open dialog keeps the
        // keys, and what is checked here is the panel. Denying does not
        // erase the record — what the session asked for has already been
        // seen — which is exactly the interesting property.
        let d = dialogos.last().expect("the approval");
        h.dispatch(UiAction::Dialog {
            id: d.id,
            choice: "deny".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
        let _ = siguientes_dialogos(&mut sub).await;
    }

    ejecutar_por_paleta(&h, &mut sub, "app.agents").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let panel = siguiente_foto(&mut sub)
        .await
        .agents
        .expect("the panel is open");
    assert_eq!(panel.rows.len(), 2);
    assert_eq!(panel.rows[0].last_op, "delete", "the most recent first");
    assert!(
        panel.rows[0].session_hostile,
        "a session id is an OPAQUE key: if it is painted differently, it is said"
    );
    assert!(
        !panel.rows[0].session.contains('\u{202e}'),
        "and it is masked"
    );
    assert!(!panel.note.is_empty(), "and the list says what it is");

    // `u` ASKS: undoing a session reverts everything it did.
    h.dispatch(tecla("u")).await.expect("host alive");
    let dialogos = siguientes_dialogos(&mut sub).await;
    let d = dialogos.last().expect("the question");
    assert_eq!(d.title_key, "modal-undo-session-title");
    assert!(
        d.choices.iter().any(|c| c.id == "confirm" && c.destructive),
        "undoing writes: the response is marked"
    );
    asentar().await;
    assert!(backend.deshechas.lock().expect("deshechas").is_empty());

    // And confirming sends the RAW id, not the one that is painted.
    h.dispatch(UiAction::Dialog {
        id: d.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let pedidas = anotados(&backend, "the undo requested", 1, |f| {
        f.deshechas.lock().expect("deshechas").clone()
    })
    .await;
    assert_eq!(pedidas, ["agente\u{202e}1".to_owned()]);
}

/// In read-only nothing is undone: it is SAID.
#[tokio::test]
async fn en_solo_lectura_no_se_deshace_una_sesion() {
    // With no approvals: a read-only window also cannot ANSWER them, so an
    // open dialog would keep the keys and this test would be checking
    // something else. The empty list holds just the same: rejection over
    // effects is checked BEFORE whether anything is selected.
    let backend = arbol_como_falso();
    let backend = Arc::new(backend);
    let (h, _snap) = host_solo_lectura(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "app.agents").await;
    let ack = h.dispatch(tecla("u")).await.expect("host alive");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-read-only"),
        "{ack:?}"
    );
    asentar().await;
    assert!(backend.deshechas.lock().expect("deshechas").is_empty());
}

/// A request that arrives with the panel open REPAINTS it, and the
/// selection follows its session even if the list gets reordered.
///
/// The list changes with NO gesture: a new request bumps its session to
/// first place. A renderer that is not told is left painting the previous
/// order — the highlighted row stops being the one the host has chosen —
/// and `u` undoes another session's work. And the selection goes by ID, not
/// by position, which is the rule 6.2 already put in writing.
#[tokio::test]
async fn una_peticion_nueva_repinta_el_panel_y_no_mueve_la_seleccion() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.aprobaciones.lock().expect("aprobaciones") = Some(rx);
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let pedir = |id: &str, aid: u64| norte_proto::methods::PolicyApprovalRequired {
        approval_id: aid,
        session: Some(id.to_owned()),
        op: "copy".to_owned(),
        paths: vec!["mem:///casa/x".to_owned()],
        paths_total: 1,
        ttl_ms: 30_000,
        detail: norte_proto::methods::ApprovalDetail::default(),
    };
    for (id, aid) in [("agente-A", 21_u64), ("agente-B", 22)] {
        tx.send(pedir(id, aid)).expect("the host is listening");
        let dialogos = siguientes_dialogos(&mut sub).await;
        let d = dialogos.last().expect("the approval");
        h.dispatch(UiAction::Dialog {
            id: d.id,
            choice: "deny".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
        let _ = siguientes_dialogos(&mut sub).await;
    }
    ejecutar_por_paleta(&h, &mut sub, "app.agents").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let antes = siguiente_foto(&mut sub).await.agents.expect("open");
    assert_eq!(antes.rows[0].session, "agente-B", "the most recent first");
    // The selection is set on the SECOND one, `agente-A`.
    h.dispatch(tecla("ArrowDown")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let elegida = siguiente_foto(&mut sub).await.agents.expect("open");
    assert_eq!(elegida.cursor, 1);

    // And another request for `agente-B` arrives, which was already first:
    // what changes is its count, and the list has to say it changed.
    tx.send(pedir("agente-B", 23))
        .expect("the host is listening");
    let mut panel = elegida.clone();
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        panel = siguiente_foto(&mut sub).await.agents.expect("open");
        if panel.generation > elegida.generation {
            break;
        }
    }
    assert!(
        panel.generation > elegida.generation,
        "a list that changes on its own has to say it changed: {panel:?}"
    );
    assert_eq!(
        panel.rows[usize::try_from(panel.cursor).expect("fits")].session,
        "agente-A",
        "the selection follows ITS session, not the slot it occupied"
    );

    // The third request left its dialog up front: it is answered before
    // continuing, because the panel is modal for the mouse too.
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let foto = siguiente_foto(&mut sub).await;
    if let Some(d) = foto.dialogs.last() {
        h.dispatch(UiAction::Dialog {
            id: d.id,
            choice: "deny".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
    }

    // A click against the OLD list is refused instead of choosing for the
    // reader.
    let ack = h
        .dispatch(UiAction::AgentSelectRow {
            row: 0,
            generation: elegida.generation,
        })
        .await
        .expect("host alive");
    assert!(
        matches!(
            &ack,
            ActionAck::Stale {
                reason: StaleAction::Generation
            }
        ),
        "{ack:?}"
    );
}

/// In read-only, the empty list does NOT say "no agent has requested
/// anything".
///
/// That window does not even subscribe to the approvals channel: its list
/// is empty for that reason, and asserting the other thing is asserting
/// what it cannot know.
#[tokio::test]
async fn en_solo_lectura_el_panel_dice_que_no_escucha() {
    let backend = Arc::new(arbol_como_falso());
    let (h, _snap) = host_solo_lectura(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "app.agents").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let panel = siguiente_foto(&mut sub).await.agents.expect("open");
    assert!(panel.rows.is_empty());
    let escuchando = norte_i18n::t_in(norte_i18n::Lang::Es, "agents-empty");
    assert_ne!(
        panel.empty, escuchando,
        "a window that is not listening cannot say nobody has requested anything"
    );
}

/// Copying the path puts BYTES on the clipboard, and it does so in both
/// modes.
///
/// Bytes and not text: a file name is bytes, and passing it through a lossy
/// decoding would paste a path that opens something else. And it mutates
/// nothing, so a read-only window also copies — it is as much a look-only
/// action as reading a name.
#[tokio::test]
async fn copiar_la_ruta_manda_bytes_al_escritorio() {
    for solo_lectura in [false, true] {
        let backend = arbol();
        let (h, _snap) = if solo_lectura {
            host_solo_lectura(Arc::clone(&backend)).await
        } else {
            host_arbol(Arc::clone(&backend)).await
        };
        let mut nativos = h.native_effects();
        let mut sub = h.subscribe();
        ejecutar_por_paleta(&h, &mut sub, "pane.copy-path").await;
        let efecto = tokio::time::timeout(std::time::Duration::from_secs(2), nativos.recv())
            .await
            .expect("an effect before the deadline")
            .expect("the channel is still alive");
        match efecto {
            norte_ui_host::dto::NativeEffect::CopyBytes { bytes, count } => {
                assert_eq!(count, 1);
                assert!(
                    bytes.starts_with(b"/") || bytes.starts_with(b"mem:"),
                    "the path, in its native form or the wire's: {bytes:?}"
                );
            }
            otro => panic!("copying the path asks to copy, not {otro:?}"),
        }
    }
}

/// In read-only NOTHING opens and no terminal is launched: it is SAID.
///
/// **A dialog's keys come from the KEYMAP, not from code** (#287).
///
/// It was the drift the shared catalogue exists to not have: the window was
/// handling its modal surfaces with fixed keys, so a preset that rebound
/// `dialog.down` changed the TUI and not the window. Here it is checked
/// against a REAL preset whose dialog keys are different.
#[tokio::test]
async fn las_teclas_de_un_dialogo_las_pone_el_preset() {
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("vim").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("vim").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("vim").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: ajustes_de_prueba(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("starts");
    let mut sub = h.subscribe();
    let antes = selector_columnas(&h, &mut sub).await;

    // The chord THIS preset binds to `dialog.down`, whatever it is.
    let atado = norte_ui_host::keys::keymap_dialogo_de_preset("vim")
        .expect("preset")
        .bindings()
        .into_iter()
        .find(|(_, c)| *c == "dialog.down")
        .map(|(seq, _)| seq)
        .expect("the preset binds down");

    h.dispatch(UiAction::Key(tecla_de_acorde(&atado)))
        .await
        .expect("host alive");
    let despues = siguiente_columnas(&h, &mut sub).await;
    assert_ne!(
        despues.cursor, antes.cursor,
        "the preset's chord moves the cursor: {atado:?}"
    );
}

/// A painted chord, converted back into the key the host receives.
///
/// Only what is needed here: one key with its modifiers, no sequences. A
/// preset that bound `dialog.down` to two chords would fall outside this,
/// and then the test would say so instead of passing by coincidence.
pub(super) fn tecla_de_acorde(acorde: &str) -> norte_ui_host::keys::KeyInput {
    let partes: Vec<&str> = acorde.split('+').collect();
    let (tecla, mods) = partes.split_last().expect("at least one part");
    let tiene = |m: &str| mods.iter().any(|p| p.eq_ignore_ascii_case(m));
    norte_ui_host::keys::KeyInput {
        key: (*tecla).to_owned(),
        ctrl: tiene("ctrl"),
        alt: tiene("alt"),
        shift: tiene("shift"),
        meta: tiene("meta") || tiene("cmd") || tiene("super"),
    }
}

/// **Editing a new one creates the EMPTY file and opens it** (#290).
///
/// The window has no editor and no terminal: what it can do is put the file
/// on disk and hand it to the desktop. And in that order — opening before
/// the outcome would launch an editor over something not there yet.
#[tokio::test]
async fn editar_uno_nuevo_crea_el_fichero_y_lo_abre() {
    let mut falso = Falso::default();
    falso.pon("file:///casa", vec![(b"notas.txt".to_vec(), false)]);
    let backend = Arc::new(falso);
    let (h, _snap) = host_en(Arc::clone(&backend), "file:///casa").await;
    let mut sub = h.subscribe();
    let mut efectos = h.native_effects();

    ejecutar_por_paleta(&h, &mut sub, "pane.edit-new").await;
    let d = siguientes_dialogos(&mut sub).await;
    assert_eq!(d[0].title_key, "modal-new-file-title");
    assert!(d[0].input.is_some(), "a name gets typed here");
    let id = d[0].id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "borrador.md".to_owned(),
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    {
        let creados = anotados(&backend, "the file created", 1, |f| {
            f.creados.lock().expect("creados").clone()
        })
        .await;
        assert_eq!(creados.len(), 1, "{creados:?}");
        assert_eq!(creados[0].to_wire(), "file:///casa/borrador.md");
    }

    let efecto = tokio::time::timeout(ESPERA_MAX, efectos.recv())
        .await
        .expect("the native effect arrives")
        .expect("channel alive");
    match efecto {
        norte_ui_host::dto::NativeEffect::OpenPath { path } => {
            assert_eq!(path.to_wire(), "file:///casa/borrador.md");
        }
        otro => panic!("expected to open the freshly created file: {otro:?}"),
    }
}

/// **And if between creating the name and opening it someone changes it, it
/// does NOT open** (#303).
///
/// norte announces the name by creating it — there is nothing to guess —
/// and whoever can write in that directory sees it appear, unlinks it and
/// leaves a symlink. The human would end up writing into a file nobody
/// showed them, and the `Created` entry's `undo` goes by PATH: undoing
/// would send to the trash whatever is there NOW.
///
/// It narrows the window and does not close it — between the `stat` and the
/// `open` there is a gap — and it is the same decision the TUI makes. The
/// file WAS CREATED: that is not undone here, it is just not opened.
#[tokio::test]
async fn lo_creado_que_dejo_de_ser_un_fichero_no_se_abre() {
    let mut falso = Falso {
        creado_aparece_como: Some(norte_proto::EntryKind::Symlink),
        ..Falso::default()
    };
    falso.pon("file:///casa", vec![(b"notas.txt".to_vec(), false)]);
    let backend = Arc::new(falso);
    let (h, _snap) = host_en(Arc::clone(&backend), "file:///casa").await;
    let mut sub = h.subscribe();
    let mut efectos = h.native_effects();

    ejecutar_por_paleta(&h, &mut sub, "pane.edit-new").await;
    let d = siguientes_dialogos(&mut sub).await;
    let id = d[0].id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "borrador.md".to_owned(),
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    {
        let creados = anotados(&backend, "the file created", 1, |f| {
            f.creados.lock().expect("creados").clone()
        })
        .await;
        assert_eq!(creados.len(), 1, "the file WAS created: {creados:?}");
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), efectos.recv())
            .await
            .is_err(),
        "the desktop is not handed what is no longer the created file"
    );
}

/// Over a REMOTE pane it is not offered, and it is said before the name gets
/// typed.
///
/// What opens afterward is the desktop application, and `xdg-open` cannot
/// be given an `sftp://`. Saying it once the name is already typed arrives
/// too late.
#[tokio::test]
async fn editar_uno_nuevo_no_se_ofrece_en_un_panel_remoto() {
    let mut falso = Falso::default();
    falso.pon("sftp://server/datos", vec![(b"a.txt".to_vec(), false)]);
    let backend = Arc::new(falso);
    let (h, _snap) = host_en(Arc::clone(&backend), "sftp://server/datos").await;
    let mut sub = h.subscribe();

    let ack = ejecutar_por_paleta_ack(&h, &mut sub, "pane.edit-new").await;
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-not-local"),
        "{ack:?}"
    );
    assert!(backend.creados.lock().expect("creados").is_empty());
}

/// A host that starts in a specific directory.
/// Like [`host_en`], but with a custom configuration: openers and the
/// editor are keys the window used to ignore, so the tests bring them in.
pub(super) async fn host_en_con(
    backend: Arc<Falso>,
    inicio: &str,
    cfg: norte_frontend::config::FrontendConfig,
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: VPath::parse(inicio).expect("vpath"),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: cfg,
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca")
}

pub(super) async fn host_en(
    backend: Arc<Falso>,
    inicio: &str,
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: VPath::parse(inicio).expect("vpath"),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: ajustes_de_prueba(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca")
}

/// Waits for a snapshot whose first listing ends in `sufijo`.
pub(super) async fn listado_en(
    sub: &mut norte_ui_host::UiSubscription,
    sufijo: &str,
) -> norte_ui_host::ViewSnapshot {
    for _ in 0..30 {
        let foto = siguiente_foto(sub).await;
        if primer_listado(&foto).path_display.ends_with(sufijo) {
            return foto;
        }
    }
    panic!("the listing never reached `{sufijo}`");
}

/// **Disconnecting sends the pane back to where it was BEFORE connecting**
/// (#140).
///
/// The trail backward, not "home": the pane was somewhere before jumping to
/// the machine, and that place is the answer the reader expects.
#[tokio::test]
async fn desconectar_vuelve_a_donde_estaba_antes() {
    let mut falso = arbol_como_falso();
    falso.pon("sftp://server/datos", vec![(b"a.txt".to_vec(), false)]);
    *falso.conexiones.lock().expect("conexiones") =
        Some(Ok(norte_proto::methods::ConnectionListResult {
            connections: vec![norte_proto::methods::ConnectionEntry {
                name: "trabajo".to_owned(),
                url: "sftp://server/datos".to_owned(),
            }],
            unusable: Vec::new(),
        }));
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // Connecting for real, through the picker: it is the path a reader
    // walks, and it is what leaves the trail that gets undone afterward.
    ejecutar_por_paleta(&h, &mut sub, "pane.connect").await;
    for _ in 0..20 {
        let Some(v) = siguiente_selector(&mut sub).await else {
            continue;
        };
        if !v.rows.is_empty() {
            break;
        }
    }
    h.dispatch(tecla("Enter")).await.expect("host alive");
    let _ = listado_en(&mut sub, "/datos").await;

    ejecutar_por_paleta(&h, &mut sub, "pane.disconnect").await;
    let _ = listado_en(&mut sub, "/casa").await;

    let cerradas = backend.cerradas.lock().expect("cerradas");
    assert_eq!(cerradas.len(), 1, "{cerradas:?}");
    assert_eq!(cerradas[0].to_wire(), "sftp://server/datos");
}

/// And NEVER to another path on the same machine: that would reopen the
/// session that just closed, which is exactly what the gesture asked not to
/// have.
#[tokio::test]
async fn desconectar_no_vuelve_a_la_misma_maquina() {
    let mut falso = Falso::default();
    falso.pon("sftp://server/uno", vec![(b"dos".to_vec(), true)]);
    falso.pon("sftp://server/uno/dos", vec![(b"b.txt".to_vec(), false)]);
    let backend = Arc::new(falso);
    let (h, snap) = host_en(Arc::clone(&backend), "sftp://server/uno").await;
    let mut sub = h.subscribe();

    let b = listado(&snap);
    h.dispatch(UiAction::Activate {
        slot_id: b.slot_id,
        key: b.rows[0].key,
        generation: b.generation,
    })
    .await
    .expect("host alive");
    let _ = listado_en(&mut sub, "/uno/dos").await;

    ejecutar_por_paleta(&h, &mut sub, "pane.disconnect").await;

    let mut visto = None;
    for _ in 0..30 {
        let foto = siguiente_foto(&mut sub).await;
        let p = primer_listado(&foto).path_display.clone();
        if !p.contains("server") {
            visto = Some(p);
            break;
        }
    }
    let after = visto.expect("the pane leaves the closed machine");
    assert!(
        !after.contains("server"),
        "its whole trail was on that machine, so it falls back home: {after}"
    );
}

/// On a LOCAL pane there is nothing to close, and it is said.
///
/// A key that answers "done" about something that did nothing teaches
/// people not to trust the message.
#[tokio::test]
async fn en_un_panel_local_no_hay_conexion_que_cerrar() {
    let mut falso = Falso::default();
    falso.pon("file:///casa", vec![(b"notas.txt".to_vec(), false)]);
    let backend = Arc::new(falso);
    let (h, _snap) = host_en(Arc::clone(&backend), "file:///casa").await;
    let mut sub = h.subscribe();

    let ack = ejecutar_por_paleta_ack(&h, &mut sub, "pane.disconnect").await;
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "msg-disconnect-local"),
        "{ack:?}"
    );
    assert!(
        backend.cerradas.lock().expect("cerradas").is_empty(),
        "and nothing is asked of the daemon"
    );
}

/// Finds the TREE slot in a snapshot.
pub(super) fn arbol_de(snap: &norte_ui_host::ViewSnapshot) -> &norte_ui_host::dto::TreeSlotView {
    snap.slots
        .iter()
        .find_map(|s| match s {
            SlotView::Tree(t) => Some(&**t),
            _ => None,
        })
        .expect("there is a tree slot")
}

/// Waits for a snapshot where the tree already has its branches.
pub(super) async fn arbol_con_ramas(
    sub: &mut norte_ui_host::UiSubscription,
    cuantas: usize,
) -> norte_ui_host::ViewSnapshot {
    for _ in 0..20 {
        let foto = siguiente_foto(sub).await;
        if foto
            .slots
            .iter()
            .any(|s| matches!(s, SlotView::Tree(t) if t.rows.len() >= cuantas))
        {
            return foto;
        }
    }
    panic!("the tree never brought {cuantas} branches");
}

/// **The tree lists ONE branch, and only when it opens** (`pane.tree`).
///
/// Lazy for the same reason the local listing does not carry sizes: one
/// that read itself whole on opening would take minutes on a large `$HOME`
/// and hours against a remote. Opening it requests the ROOT and nothing
/// else — `docs`'s children are not requested until someone expands `docs`.
#[tokio::test]
async fn el_arbol_pide_una_rama_y_solo_al_abrirla() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.tree").await;
    // The root and its DIRECTORY children: `docs` is there, `notas.txt` is
    // not.
    let foto = arbol_con_ramas(&mut sub, 2).await;
    let t = arbol_de(&foto);
    assert_eq!(t.rows.len(), 2, "root + `docs`, no files: {:?}", t.rows);
    assert_eq!(t.rows[0].depth, 0);
    assert!(
        t.rows[0].label.ends_with("/casa"),
        "the root carries its whole path: {:?}",
        t.rows[0]
    );
    assert_eq!(t.rows[1].label, "docs");
    assert_eq!(t.rows[1].depth, 1);
    assert_eq!(
        t.rows[1].children, None,
        "the inside has not been looked at yet, and that is NOT \"it's a leaf\""
    );
}

/// **The tree FOLLOWS the listing that navigates** (ADR 0102).
///
/// A pane anchored on opening and still afterward said where you were when
/// you opened it, and nothing more: entering a folder from the listing left
/// the tree's cursor where it was. It keeps revealing — it expands the
/// ancestors and moves the cursor — not re-anchoring: the root does not
/// change and what is open does not close.
#[tokio::test]
async fn el_arbol_sigue_al_listado_que_navega() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "pane.tree").await;
    let foto = arbol_con_ramas(&mut sub, 2).await;
    assert_eq!(arbol_de(&foto).cursor, 0, "it starts at the root");

    // `docs` is the listing's first row (directories go first), so Enter
    // goes into it.
    ejecutar_por_paleta(&h, &mut sub, "nav.enter").await;

    let mut visto = None;
    for _ in 0..20 {
        let foto = siguiente_foto(&mut sub).await;
        if primer_listado(&foto).path_display.ends_with("/casa/docs") && arbol_de(&foto).cursor == 1
        {
            visto = Some(foto);
            break;
        }
    }
    let foto = visto.expect("the tree ends up pointing at the branch the listing is in");
    let t = arbol_de(&foto);
    assert_eq!(t.rows[1].label, "docs");
    assert!(
        t.rows[0].label.ends_with("/casa"),
        "and the root has not moved: {:?}",
        t.rows[0]
    );
}

/// **`pane.switch` cycles through LISTINGS — all of them — and skips the
/// side panels** (ADR 0102).
///
/// It is "the other pane" of any orthodox manager. It used to share a bind
/// with `layout.focus-next`, and that did two things wrong at once: with
/// the tree open, tab would stop on it, and with three listings on screen
/// there was no way to say "the one next to it" without counting the stops
/// in between.
///
/// `layout.focus-next` remains the whole screen's traversal, and that half
/// is checked right here: they are two rings, not one with two names.
#[tokio::test]
async fn el_tabulador_cicla_los_listados_y_se_salta_los_laterales() {
    let (h, _snap) = host_con_layout(arbol(), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    // The tree next to it, and a third listing: the split pane and the one
    // that is not.
    ejecutar_por_paleta(&h, &mut sub, "pane.tree").await;
    // Drained before going back to the palette: the queue carries the
    // previous opening's envelopes, and the helper would read them as a
    // response to the new one.
    foto_hasta(&h, &mut sub, "the tree is on screen", |foto| {
        foto.palette
            .is_none()
            .then(|| foto.slots.iter().any(|v| matches!(v, SlotView::Tree(_))))
            .filter(|hay| *hay)
    })
    .await;
    ejecutar_por_paleta(&h, &mut sub, "layout.split-v").await;
    let (hueco_del_arbol, partida) =
        foto_hasta(&h, &mut sub, "three listings and a tree", |foto| {
            let listados = foto
                .slots
                .iter()
                .filter(|v| matches!(v, SlotView::Browser(_)))
                .count();
            let arbol = foto.slots.iter().find_map(|v| match v {
                SlotView::Tree(t) => Some(t.slot_id),
                _ => None,
            })?;
            let foco = foto.focus?;
            (listados == 3).then_some((arbol, foco))
        })
        .await;

    let mut vistos = Vec::new();
    let mut anterior = partida;
    for _ in 0..3 {
        h.dispatch(tecla("Tab")).await.expect("host alive");
        // Until focus MOVES: the previous envelope may still be in the
        // queue, and reading it as a response to this key is reading a
        // stale snapshot.
        let ahora = foto_hasta(&h, &mut sub, "focus moved", |foto| {
            foto.focus.filter(|f| *f != anterior)
        })
        .await;
        vistos.push(ahora);
        anterior = ahora;
    }
    assert!(
        !vistos.contains(&hueco_del_arbol),
        "tab does not stop on the tree: {vistos:?}"
    );
    let distintos: std::collections::BTreeSet<u32> = vistos.iter().copied().collect();
    assert_eq!(
        distintos.len(),
        3,
        "all THREE listings are reachable: {vistos:?}"
    );
    assert_eq!(
        vistos[2], partida,
        "and three jumps go all the way around: {vistos:?}"
    );

    // The other half: the screen's traversal DOES stop on the tree.
    let mut paradas = Vec::new();
    for _ in 0..4 {
        h.dispatch(tecla_alt("o")).await.expect("host alive");
        let ahora = foto_hasta(&h, &mut sub, "focus moved", |foto| {
            foto.focus.filter(|f| *f != anterior)
        })
        .await;
        paradas.push(ahora);
        anterior = ahora;
    }
    assert!(
        paradas.contains(&hueco_del_arbol),
        "`layout.focus-next` traverses the whole screen: {paradas:?}"
    );
}

/// Choosing a branch navigates the LISTING, and the tree's ROOT does not
/// move.
///
/// It is what makes keeping it open useful: if the tree re-anchored on
/// every navigation, entering a folder would collapse every open branch.
#[tokio::test]
async fn elegir_una_rama_navega_el_listado_y_el_arbol_no_se_mueve() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "pane.tree").await;
    let foto = arbol_con_ramas(&mut sub, 2).await;
    let generacion = arbol_de(&foto).generation;

    h.dispatch(UiAction::TreeActivateRow {
        row: 1,
        generation: generacion,
    })
    .await
    .expect("host alive");

    let mut visto = None;
    for _ in 0..20 {
        let foto = siguiente_foto(&mut sub).await;
        if primer_listado(&foto).path_display.ends_with("/casa/docs") {
            visto = Some(foto);
            break;
        }
    }
    let foto = visto.expect("the listing goes to the chosen branch");
    let t = arbol_de(&foto);
    assert!(
        t.rows[0].label.ends_with("/casa"),
        "the tree stays anchored where it was: {:?}",
        t.rows[0]
    );
}

/// Regression (2026-09-21): with focus on the LEFT listing, clicking the
/// tree and choosing a branch navigates THAT listing, not the right one.
///
/// Clicking the tree gives the tree slot the active role; `activo()` has to
/// answer with a listing, and it was falling back to the lowest id — which
/// in this layout, a real session's, is the one on the right.
#[tokio::test]
async fn la_rama_del_arbol_va_al_ultimo_listado_enfocado() {
    let disposicion = r#"{"split": {"children": [{"split": {"children": [
        {"slot": {"id": 2, "kind": "browser"}}, {"slot": {"id": 1, "kind": "browser"}}],
        "dir": "horizontal", "sizes": [{"weight": 1}, {"weight": 1}]}},
        {"slot": {"id": 4, "kind": "status"}}], "dir": "vertical",
        "sizes": [{"weight": 1}, {"fixed": 1}]}}"#;
    let tree: norte_frontend::layout::Node = serde_json::from_str(disposicion).expect("tree");
    let (h, _) = super::base::host_con_arbol(arbol(), tree, (160, 50)).await;
    let mut sub = h.subscribe();
    // The one on the left, 2, with focus.
    let _ = h
        .dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    ejecutar_por_paleta(&h, &mut sub, "pane.tree").await;
    let foto = arbol_con_ramas(&mut sub, 2).await;
    let t = arbol_de(&foto);
    // And the click on the tree, which focuses it, before choosing the
    // branch.
    let _ = h
        .dispatch(UiAction::FocusSlot { slot_id: t.slot_id })
        .await
        .expect("host alive");
    h.dispatch(UiAction::TreeActivateRow {
        row: 1,
        generation: t.generation,
    })
    .await
    .expect("host alive");
    let ruta = |s: &norte_ui_host::ViewSnapshot, id: u32| {
        s.slots.iter().find_map(|v| match v {
            SlotView::Browser(b) if b.slot_id == id => Some(b.path_display.clone()),
            _ => None,
        })
    };
    let foto = foto_hasta(&h, &mut sub, "some listing in docs", |s| {
        (ruta(s, 1)?.ends_with("/casa/docs") || ruta(s, 2)?.ends_with("/casa/docs"))
            .then(|| s.clone())
    })
    .await;
    assert!(
        ruta(&foto, 2).is_some_and(|r| r.ends_with("/casa/docs")),
        "it navigates the one on the left, which had focus: {:?}",
        ruta(&foto, 2)
    );
    assert!(
        ruta(&foto, 1).is_some_and(|r| !r.ends_with("/casa/docs")),
        "and the one on the right is not touched"
    );
}

/// A click with ANOTHER painted generation is rejected, it does not
/// navigate.
///
/// A branch's children land IN THE MIDDLE of the list, so between the
/// reader releasing the button and the host attending to it, that row can
/// be a different folder (ADR 0068).
#[tokio::test]
async fn una_rama_de_otra_pintada_no_navega() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "pane.tree").await;
    let foto = arbol_con_ramas(&mut sub, 2).await;
    let generacion = arbol_de(&foto).generation;

    let ack = h
        .dispatch(UiAction::TreeActivateRow {
            row: 1,
            generation: generacion + 99,
        })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, ActionAck::Stale { .. }),
        "a generation that does not match is rejected: {ack:?}"
    );
    let foto = siguiente_foto_tras_resync(&h, &mut sub).await;
    assert!(
        primer_listado(&foto).path_display.ends_with("/casa"),
        "and the listing did not move: {:?}",
        primer_listado(&foto).path_display
    );
}

/// **Dropping files does NOT copy: it asks** (#283).
///
/// A drop is a gesture with no confirmation by nature, and the list is
/// composed by another process. Showing it before writing is the reader's
/// only chance to see that what arrived is not what they dragged.
#[tokio::test]
async fn soltar_pregunta_antes_de_copiar() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    h.dispatch(UiAction::FilesDropped {
        paths: vec!["/tmp/uno.txt".to_owned(), "/tmp/dos.txt".to_owned()],
    })
    .await
    .expect("host alive");

    let dialogos = siguientes_dialogos(&mut sub).await;
    assert_eq!(dialogos.len(), 1);
    assert_eq!(dialogos[0].title_key, "modal-drop-title");
    let cuerpo = &dialogos[0].body;
    assert_eq!(cuerpo.len(), 2, "what arrived, line by line: {cuerpo:?}");
    let destino = dialogos[0]
        .destination
        .as_ref()
        .expect("says where it lands");
    assert!(
        destino.text.ends_with("/casa"),
        "the active pane, in ITS OWN field: {destino:?}"
    );
    assert!(
        backend
            .transferencias
            .lock()
            .expect("transferencias")
            .is_empty(),
        "opening the dialog copies nothing"
    );
}

/// Confirmed, it COPIES — it never moves — and the pane's marks are not
/// touched.
///
/// Moving what another application dragged would delete it from wherever
/// that process has it, and this window has not asked that. And the active
/// pane's marks were put there by the reader for something else: what gets
/// copied did not come from there.
#[tokio::test]
async fn soltar_confirmado_copia_y_respeta_las_marcas() {
    let backend = arbol();
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let b = listado(&snap);
    let hueco = b.slot_id;
    let fila = b.rows.first().expect("there are rows");
    h.dispatch(UiAction::ToggleMark {
        slot_id: hueco,
        key: fila.key,
        generation: b.generation,
    })
    .await
    .expect("host alive");

    h.dispatch(UiAction::FilesDropped {
        paths: vec!["/tmp/uno.txt".to_owned()],
    })
    .await
    .expect("host alive");
    let dialogos = siguientes_dialogos(&mut sub).await;
    let id = dialogos[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    {
        let ts = anotados(&backend, "the drop's copy", 1, |f| {
            f.transferencias.lock().expect("transferencias").clone()
        })
        .await;
        assert_eq!(ts.len(), 1, "{ts:?}");
        let (from, to, mover, _) = &ts[0];
        assert!(!*mover, "a drop COPIES, it never moves: {ts:?}");
        assert_eq!(from.to_wire(), "file:///tmp/uno.txt");
        assert_eq!(
            to.to_wire(),
            "mem:///casa/uno.txt",
            "it lands in the pane's directory, with the name it carried"
        );
    }

    let foto = siguiente_foto_tras_resync(&h, &mut sub).await;
    let b = listado(&foto);
    assert!(
        b.rows.iter().any(|r| r.marked),
        "the reader's mark is still where it was: {:?}",
        b.rows
    );
}

/// What arrives and is not a path on this machine is SAID, not ignored.
///
/// A sender composes the `text/uri-list` by hand if it wants to. Swallowing
/// it in silence would leave the reader looking at a pane that did not
/// change with no idea why.
#[tokio::test]
async fn soltar_lo_que_no_es_ruta_se_dice() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    let ack = h
        .dispatch(UiAction::FilesDropped {
            paths: vec!["relativa/mala".to_owned(), String::new()],
        })
        .await
        .expect("host alive");
    assert!(
        matches!(&ack, norte_ui_host::ActionAck::Unavailable { reason_key } if reason_key == "host-drop-unusable"),
        "{ack:?}"
    );
    // By `Resync` and not waiting for the next snapshot: refusing does not
    // send one, only the bar's patch, and a `siguiente_foto` here would
    // hang instead of turning red.
    let foto = siguiente_foto_tras_resync(&h, &mut sub).await;
    assert!(foto.dialogs.is_empty(), "and no dialog opens");
    assert!(
        foto.status
            .message
            .as_deref()
            .is_some_and(|m| m == norte_i18n::t_in(norte_i18n::Lang::Es, "host-drop-unusable")),
        "and it SAYS so in the bar: {:?}",
        foto.status.message
    );
    assert!(
        backend
            .transferencias
            .lock()
            .expect("transferencias")
            .is_empty()
    );
}

/// **The connections picker is filled by the DAEMON** (#264): the window
/// does not read `connections.toml`, which is what would cost it pulling in
/// the whole network stack.
///
/// And the URL is masked as an AUTHORITY, not as a path: here "which
/// machine am I connecting to?" is the only question the picker answers.
#[tokio::test]
async fn el_selector_de_conexiones_lo_llena_el_daemon() {
    let falso = arbol_como_falso();
    *falso.conexiones.lock().expect("conexiones") =
        Some(Ok(norte_proto::methods::ConnectionListResult {
            connections: vec![
                norte_proto::methods::ConnectionEntry {
                    name: "trabajo".to_owned(),
                    url: "sftp://oscar@server.example/datos".to_owned(),
                },
                norte_proto::methods::ConnectionEntry {
                    name: "archivo".to_owned(),
                    url: "s3://mi-bucket".to_owned(),
                },
            ],
            unusable: Vec::new(),
        }));
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.connect").await;

    let mut con_filas = None;
    for _ in 0..20 {
        let Some(v) = siguiente_selector(&mut sub).await else {
            continue;
        };
        if !v.rows.is_empty() {
            con_filas = Some(v);
            break;
        }
    }
    let v = con_filas.expect("the daemon's list arrives");
    assert_eq!(v.rows.len(), 2);
    assert_eq!(v.rows[0].label, "trabajo");
    assert!(
        v.rows[0].detail.contains("server.example"),
        "the detail is the URL: {:?}",
        v.rows[0]
    );
}

/// **A connection the daemon failed to read is VISIBLE, at the back, and
/// with the reason** (#365).
///
/// Before this, a single bad entry made the whole `connection.list` fail
/// and the picker opened empty: the reader lost the list of ALL their
/// connections over one, with an error that named none of them. Now the
/// good ones are listed and the bad one stays at the bottom, visible, with
/// no destination and saying what is wrong with it — which is the only
/// thing the reader can act on.
#[tokio::test]
async fn una_conexion_ilegible_no_esconde_a_las_demas_en_la_ventana() {
    let falso = arbol_como_falso();
    *falso.conexiones.lock().expect("conexiones") =
        Some(Ok(norte_proto::methods::ConnectionListResult {
            connections: vec![norte_proto::methods::ConnectionEntry {
                name: "buena".to_owned(),
                url: "sftp://server.example/datos".to_owned(),
            }],
            unusable: vec![norte_proto::methods::ConnectionProblem {
                name: "rota".to_owned(),
                reason: "unknown field `password`".to_owned(),
            }],
        }));
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.connect").await;

    let mut con_filas = None;
    for _ in 0..20 {
        let Some(v) = siguiente_selector(&mut sub).await else {
            continue;
        };
        if !v.rows.is_empty() {
            con_filas = Some(v);
            break;
        }
    }
    let v = con_filas.expect("the daemon's list arrives");
    assert_eq!(v.rows.len(), 2, "both are visible: {:?}", v.rows);
    assert_eq!(v.rows[0].label, "buena", "and the usable one goes first");
    assert_eq!(v.rows[1].label, "rota");
    assert!(
        v.rows[1].detail.contains("password"),
        "the unusable one says WHAT is wrong with it where its URL would go: {:?}",
        v.rows[1]
    );
}

/// With none configured, the picker SAYS so. "You have none" and "has not
/// answered yet" are not the same, and an empty list with no phrase always
/// reads as the first one.
#[tokio::test]
async fn sin_conexiones_configuradas_el_selector_lo_dice() {
    let falso = arbol_como_falso();
    *falso.conexiones.lock().expect("conexiones") =
        Some(Ok(norte_proto::methods::ConnectionListResult {
            connections: Vec::new(),
            unusable: Vec::new(),
        }));
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.connect").await;

    let mut visto = None;
    for _ in 0..20 {
        let Some(v) = siguiente_selector(&mut sub).await else {
            continue;
        };
        if v.empty == norte_i18n::t_in(norte_i18n::Lang::Es, "picker-connections-empty") {
            visto = Some(v);
            break;
        }
    }
    let v = visto.expect("the empty-list phrase arrives");
    assert!(v.rows.is_empty());
}

/// **With the window up front, no desktop notice fires** (#285): the bar and
/// the board already say the same thing, and repeating it outside is noise.
#[tokio::test]
async fn con_la_ventana_delante_no_se_avisa_fuera() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut nativos = h.native_effects();
    let mut sub = h.subscribe();
    h.dispatch(tecla("F8")).await.expect("host alive");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let _ = siguientes_tasks(&mut sub).await;

    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    asentar().await;

    assert!(
        nativos.try_recv().is_err(),
        "with focus in place, no notice fires to the desktop"
    );
}

/// And with no focus it DOES, with the name of what was inside (#285).
///
/// The name is MASKED as in the listing: a notification ends up in the
/// desktop's history and can show on the lock screen, so what cannot
/// pretend here cannot pretend there either.
#[tokio::test]
async fn sin_foco_el_aviso_sale_y_lleva_el_nombre() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut nativos = h.native_effects();
    let mut sub = h.subscribe();
    h.dispatch(UiAction::WindowFocus { focused: false })
        .await
        .expect("host alive");
    h.dispatch(tecla("F8")).await.expect("host alive");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let _ = siguientes_tasks(&mut sub).await;

    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| {
        p.state = norte_proto::TaskState::Completed;
        p.current = Some(VPath::parse("mem:///casa/notas.txt").expect("vpath"));
    });

    let mut visto = None;
    for _ in 0..2_000 {
        if let Ok(norte_ui_host::dto::NativeEffect::Notify { titulo, cuerpo }) = nativos.try_recv()
        {
            visto = Some((titulo, cuerpo));
            break;
        }
        asentar().await;
    }
    let (titulo, cuerpo) = visto.expect("with no focus, the notice fires");
    assert!(!titulo.is_empty(), "the notice says WHAT happened");
    assert!(
        cuerpo.contains("notas.txt"),
        "and with which file: {cuerpo:?}"
    );
}

/// **With just ONE pane, copying asks the desktop for the destination**
/// (#284).
///
/// It used to be refused: whoever had not split the window could not copy.
/// What is checked here is the whole chain — the effect fires, the response
/// comes in, and the transfer ends up going where it was chosen — because
/// each half on its own says nothing about the other.
#[tokio::test]
async fn con_un_panel_el_destino_lo_elige_el_escritorio() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut nativos = h.native_effects();
    let mut sub = h.subscribe();

    // F5 with a single listing: instead of refusing, the effect fires.
    h.dispatch(tecla("F5")).await.expect("host alive");
    let efecto = tokio::time::timeout(std::time::Duration::from_secs(2), nativos.recv())
        .await
        .expect("the effect fires before the timeout")
        .expect("channel alive");
    let desde = match efecto {
        norte_ui_host::dto::NativeEffect::PickDirectory { desde } => desde,
        otro => panic!("expected the folder picker: {otro:?}"),
    };
    assert_eq!(
        desde.to_wire(),
        "mem:///casa",
        "the picker opens where the pane is"
    );

    // And the response comes in through the same door as the rest.
    h.dispatch(UiAction::DirectoryPicked {
        path: Some("/tmp".to_owned()),
    })
    .await
    .expect("host alive");

    // What comes out is the usual confirmation, with THAT destination: the
    // reader sees where their files are going before a single byte moves,
    // which is what pins down that the path came from outside.
    let dialogos = siguientes_dialogos(&mut sub).await;
    let confirmacion = dialogos.last().expect("there is a confirmation");
    let destino = confirmacion
        .destination
        .as_ref()
        .expect("the confirmation NAMES the destination");
    assert!(
        destino.text.contains("/tmp"),
        "the chosen destination is shown: {destino:?}"
    );
}

/// Closing the picker without choosing copies nothing: cancelling is a
/// response.
#[tokio::test]
async fn cerrar_el_selector_sin_elegir_no_transfiere() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut nativos = h.native_effects();
    h.dispatch(tecla("F5")).await.expect("host alive");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), nativos.recv())
        .await
        .expect("the effect fires");

    h.dispatch(UiAction::DirectoryPicked { path: None })
        .await
        .expect("host alive");
    asentar().await;
    assert!(
        backend
            .transferencias
            .lock()
            .expect("transferencias")
            .is_empty(),
        "cancelling the picker transfers nothing"
    );
}

/// A picker response NOBODY requested is not interpreted. It is the same
/// rule as a stale dialog: on a surface that moves files, a stray message
/// cannot start an operation.
#[tokio::test]
async fn un_destino_que_nadie_pidio_no_hace_nada() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let ack = h
        .dispatch(UiAction::DirectoryPicked {
            path: Some("/tmp".to_owned()),
        })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, ActionAck::Stale { .. }),
        "with no picker open, the response is stale: {ack:?}"
    );
    asentar().await;
    assert!(
        backend
            .transferencias
            .lock()
            .expect("transferencias")
            .is_empty()
    );
}

/// What an editor or a shell does with the files is not decided by this
/// window, so one mounted with no effects does not launch them.
#[tokio::test]
async fn en_solo_lectura_no_se_lanza_nada_del_escritorio() {
    let backend = arbol();
    let (h, _snap) = host_solo_lectura(Arc::clone(&backend)).await;
    let mut nativos = h.native_effects();
    let mut sub = h.subscribe();
    // They are not even OFFERED: the palette is built with this window's
    // effects, and offering what is going to be refused is promising
    // something that will not happen. It is the same rule that already
    // governs copying and deleting.
    h.dispatch(tecla_mod("p", true, false))
        .await
        .expect("host alive");
    let _ = siguiente_paleta(&mut sub).await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let p = siguiente_foto(&mut sub).await.palette.expect("open");
    for cmd in ["pane.open", "app.terminal"] {
        assert!(
            !p.rows.iter().any(|r| r.text == cmd),
            "{cmd} is not offered in a window with no effects"
        );
    }
    // And copying the path IS, because it launches nothing.
    assert!(p.rows.iter().any(|r| r.text == "pane.copy-path") || p.total > 0);
    assert!(
        matches!(
            nativos.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ),
        "and no effect fired"
    );
}

/// What is not on THIS disk is not handed to the desktop.
///
/// `xdg-open` cannot be given an `sftp://`, and a terminal has nowhere to
/// sit inside one. It is refused saying so, instead of opening something
/// else — `$HOME`, typically — without warning.
#[tokio::test]
async fn una_ruta_que_no_es_local_no_se_abre() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut nativos = h.native_effects();
    let mut sub = h.subscribe();
    for cmd in ["pane.open", "app.terminal"] {
        let ack = ejecutar_por_paleta_ack(&h, &mut sub, cmd).await;
        assert!(
            matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-not-local"),
            "{cmd} over a `mem://`: {ack:?}"
        );
    }
    assert!(
        matches!(
            nativos.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ),
        "and no effect fired"
    );
}

/// With nobody listening to native effects, the gesture is refused: no
/// acknowledgment is given for something that is not going to happen.
#[tokio::test]
async fn sin_escritorio_detras_copiar_se_rehusa() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // NOBODY calls `native_effects()`: it is the case of a frontend that
    // does not know how to do these things.
    let ack = ejecutar_por_paleta_ack(&h, &mut sub, "pane.copy-path").await;
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-no-desktop"),
        "{ack:?}"
    );
}

/// A preset that rebinds `dialog.confirm` changes the window TOO (#287).
///
/// This window's dialogs were handled with fixed keys, so whoever rebound
/// the verb changed the TUI and not the window — which is exactly the drift
/// the shared catalogue exists to not have. With a field open there is
/// still a fixed regime, because there is no `dialog.*` verb for "type a
/// letter"; the test next to this one covers that.
#[tokio::test]
async fn una_tecla_reatada_contesta_el_dialogo() {
    let backend = arbol();
    // A user layer that binds `s` to confirm, on top of the usual preset:
    // it is what a `keymap.toml` would do.
    let capa = norte_frontend::keymap::parse_keymap(
        "[dialog]\nappend_keymap = [{ on = [\"z\"], run = \"dialog.confirm\" }]\n",
    )
    .expect("the layer parses");
    let base = norte_frontend::keymap::parse_keymap(
        norte_frontend::keymap::presets::source("orthodox").expect("preset"),
    )
    .expect("the preset parses");
    let dialogo = norte_frontend::keymap::Effective::build_for(
        &base,
        std::slice::from_ref(&capa),
        norte_ui_host::commands::IMPLEMENTADOS_DIALOGO,
        norte_frontend::keymap::Screen::Dialog,
    )
    .expect("effective");
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: Arc::clone(&backend) as Arc<dyn norte_ui_host::backend::HostBackend>,
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: dialogo,
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: ajustes_de_prueba(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("starts");
    let mut sub = h.subscribe();

    // A delete opens its confirmation, which has NO field to type into.
    ejecutar_por_paleta(&h, &mut sub, "pane.delete").await;
    let dialogos = siguientes_dialogos(&mut sub).await;
    assert_eq!(dialogos.len(), 1, "the confirmation");
    h.dispatch(tecla("z")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert!(
        siguiente_foto(&mut sub).await.dialogs.is_empty(),
        "`z` bound to `dialog.confirm` answers the question"
    );
    hasta(&backend, "the delete queued", |f| {
        (!f.borrados.lock().expect("borrados").is_empty()).then_some(())
    })
    .await;
}

/// With a FIELD open, the dialog's keys are letters.
///
/// There is no `dialog.*` verb for "type a letter", so resolving through
/// the keymap there would turn typing a file name into answering the
/// question. It is the same pair of regimes as the TUI and the 6.4
/// `[config]` editor.
#[tokio::test]
async fn con_un_campo_abierto_las_teclas_no_contestan() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "pane.mkdir").await;
    let dialogos = siguientes_dialogos(&mut sub).await;
    let d = dialogos.last().expect("the name prompt");
    assert!(d.input.is_some(), "this dialog has somewhere to type");
    // Any letter at all: it neither answers nor closes.
    h.dispatch(tecla("y")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert_eq!(
        siguiente_foto(&mut sub).await.dialogs.len(),
        1,
        "the prompt stays open"
    );
}

/// Mark all, invert, and by PATTERN (#289).
///
/// What matches is decided by the shared model (`mark_glob`), which folds
/// the name before comparing: here it is only checked that the gesture
/// arrives and that a glob that fails to compile is SAID instead of doing
/// nothing.
#[tokio::test]
async fn marcar_todo_invertir_y_por_patron() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let marcas = |s: &norte_ui_host::ViewSnapshot| listado(s).marks;

    ejecutar_por_paleta(&h, &mut sub, "mark.all").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let todas = marcas(&siguiente_foto(&mut sub).await);
    assert!(todas > 0, "marking all marks something");

    ejecutar_por_paleta(&h, &mut sub, "mark.invert").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert_eq!(
        marcas(&siguiente_foto(&mut sub).await),
        0,
        "inverting over everything marked leaves none"
    );

    // By pattern: the prompt asks for the glob and `Enter` applies it.
    ejecutar_por_paleta(&h, &mut sub, "mark.pattern-add").await;
    let dialogos = siguientes_dialogos(&mut sub).await;
    let d = dialogos.last().expect("the glob prompt");
    assert!(d.input.is_some());
    h.dispatch(UiAction::DialogInput {
        id: d.id,
        text: "*".to_owned(),
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id: d.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert_eq!(
        marcas(&siguiente_foto(&mut sub).await),
        todas,
        "`*` marks the same as marking all"
    );

    // And a glob that fails to compile is refused, SAYING SO.
    ejecutar_por_paleta(&h, &mut sub, "mark.pattern-remove").await;
    let dialogos = siguientes_dialogos(&mut sub).await;
    let d = dialogos.last().expect("the prompt");
    h.dispatch(UiAction::DialogInput {
        id: d.id,
        text: "[".to_owned(),
    })
    .await
    .expect("host alive");
    let ack = h
        .dispatch(UiAction::Dialog {
            id: d.id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "err-bad-pattern"),
        "{ack:?}"
    );
}

/// The board gets scrolled through and dismissed with the keyboard, without
/// focusing the processes panel (#292).
///
/// And a LIVE task is not dismissed: stopping it is `task.cancel`, and
/// removing from view something still writing to disk is losing sight of
/// exactly what has to be watched.
#[tokio::test]
async fn el_tablero_se_recorre_y_se_descarta() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // With no tasks: all three say so instead of staying silent.
    for cmd in ["task.next", "task.prev", "task.dismiss"] {
        let ack = ejecutar_por_paleta_ack(&h, &mut sub, cmd).await;
        assert!(matches!(ack, ActionAck::Applied { .. }), "{cmd}: {ack:?}");
    }

    // A live task: dismissing it is refused.
    ejecutar_por_paleta(&h, &mut sub, "pane.mkdir").await;
    let dialogos = siguientes_dialogos(&mut sub).await;
    let d = dialogos.last().expect("the prompt");
    h.dispatch(UiAction::DialogInput {
        id: d.id,
        text: "nueva".to_owned(),
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id: d.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let vivas = foto_hasta(&h, &mut sub, "the mkdir's task on the board", |f| {
        (!f.tasks.is_empty()).then(|| f.tasks.clone())
    })
    .await;
    assert!(!vivas.is_empty(), "the mkdir's task reached the board");
    if vivas
        .iter()
        .any(|t| matches!(t.state, norte_ui_host::dto::TaskStateView::Running))
    {
        let ack = ejecutar_por_paleta_ack(&h, &mut sub, "task.dismiss").await;
        assert!(
            matches!(&ack, ActionAck::Unavailable { reason_key }
                if reason_key == "host-task-running"),
            "a live one is not dismissed: {ack:?}"
        );
    }

    // Once it finishes, it is: the row disappears from the board.
    foto_hasta(&h, &mut sub, "no task running", |f| {
        f.tasks
            .iter()
            .all(|t| !matches!(t.state, norte_ui_host::dto::TaskStateView::Running))
            .then_some(())
    })
    .await;
    ejecutar_por_paleta(&h, &mut sub, "task.dismiss").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert!(
        siguiente_foto(&mut sub).await.tasks.is_empty(),
        "the finished row is dismissed"
    );
}

/// Splitting puts another LISTING next to it, in the same directory and
/// with focus (#291).
#[tokio::test]
async fn partir_abre_otro_listado_y_le_da_el_foco() {
    let backend = arbol();
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let antes = snap.slots.len();
    let dir_antes = listado(&snap).path_display.clone();

    ejecutar_por_paleta(&h, &mut sub, "layout.split-v").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(foto.slots.len(), antes + 1, "there is one more slot");
    let listados: Vec<&norte_ui_host::dto::BrowserSlotView> = foto
        .slots
        .iter()
        .filter_map(|s| match s {
            SlotView::Browser(b) => Some(&**b),
            _ => None,
        })
        .collect();
    assert!(listados.len() >= 2, "and it is a listing");
    assert!(
        listados.iter().all(|b| b.path_display == dir_antes),
        "the new one starts where the one that was split was: {listados:?}"
    );
    // Focus to the newborn: splitting is asking for room to work in it.
    let enfocado = foto.focus.expect("there is focus");
    assert!(
        !foto.slots.is_empty() && enfocado != 1,
        "focus moved to the new slot: {enfocado}"
    );
}

/// Splitting a slot that no longer fits two is REFUSED, and it says so.
///
/// The same rule as the TUI and through the same place (ADR 0077): without
/// it the tree was left with a slot the layout hid inside the same frame —
/// the `Split` does not fit, it degrades to tabs, and the screen keeps
/// showing one.
#[tokio::test]
async fn partir_sin_sitio_se_rehusa_y_se_dice() {
    // 24 rows tall for the whole body: enough for one listing and not for
    // two (`browser`'s minimum is 5, and the chrome takes its share).
    let (h, snap) = host_con_layout(arbol(), "orthodox", (100, 9)).await;
    let mut sub = h.subscribe();
    let antes = snap
        .slots
        .iter()
        .filter(|s| matches!(s, SlotView::Browser(_)))
        .count();
    let ack = ejecutar_por_paleta_ack(&h, &mut sub, "layout.split-v").await;
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "msg-layout-split-no-room".to_owned()
        },
        "{ack:?}"
    );
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let foto = siguiente_foto(&mut sub).await;
    let listados = foto
        .slots
        .iter()
        .filter(|s| matches!(s, SlotView::Browser(_)))
        .count();
    assert_eq!(listados, antes, "the tree did not keep an invisible slot");
}

/// Closing the LAST listing is refused, and it says so.
///
/// A screen with no usable listing is not a screen, it is a hang with
/// borders — the same rule the shared layout already applies on its own.
#[tokio::test]
async fn no_se_cierra_el_ultimo_listado() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // `simple` has ONE listing: closing it would leave the screen with none.
    let ack = ejecutar_por_paleta_ack(&h, &mut sub, "layout.close-slot").await;
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key }
            if reason_key == "msg-layout-last-panel"),
        "{ack:?}"
    );

    // With two, closing one does work. By KEY and not by palette: splitting
    // changes the whole screen and sends its snapshot, and the palette
    // helper reads snapshots.
    ejecutar_por_paleta(&h, &mut sub, "layout.split-h").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let _ = siguiente_foto(&mut sub).await;
    ejecutar_por_paleta(&h, &mut sub, "layout.close-slot").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(
        foto.slots
            .iter()
            .filter(|s| matches!(s, SlotView::Browser(_)))
            .count(),
        1,
        "there is one again"
    );
}

/// The three auxiliary slots this window knows how to paint open and close
/// with their command (#291).
#[tokio::test]
async fn los_huecos_auxiliares_se_abren_y_se_cierran() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    for (cmd, presente) in [
        (
            "layout.processes",
            (|s: &norte_ui_host::ViewSnapshot| {
                s.slots
                    .iter()
                    .any(|v| matches!(v, SlotView::Processes { .. }))
            }) as fn(&norte_ui_host::ViewSnapshot) -> bool,
        ),
        ("layout.metadata", |s| {
            s.slots.iter().any(|v| matches!(v, SlotView::Metadata(_)))
        }),
        ("layout.places", |s| {
            s.slots.iter().any(|v| matches!(v, SlotView::Places(_)))
        }),
    ] {
        ejecutar_por_paleta(&h, &mut sub, cmd).await;
        h.dispatch(UiAction::Resync).await.expect("host alive");
        assert!(
            presente(&siguiente_foto(&mut sub).await),
            "{cmd} opens its slot, and this window PAINTS it (not grayed out)"
        );
        ejecutar_por_paleta(&h, &mut sub, cmd).await;
        h.dispatch(UiAction::Resync).await.expect("host alive");
        assert!(
            !presente(&siguiente_foto(&mut sub).await),
            "{cmd} again closes it"
        );
    }
}

/// A host with REAL configuration layers, for profiles.
///
/// Profiles live in `profiles/` of the user layer, and the host receives
/// them already resolved (ADR 0066 D14): without handing them over, there
/// is nowhere to look.
pub(super) async fn host_con_capas(
    dir_usuario: &std::path::Path,
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    host_con_capas_y_favoritos(dir_usuario, Vec::new()).await
}

/// A host with TWO layers: system underneath, user on top.
///
/// "Reset" needs it: removing the key from the user layer does not return
/// the factory value if the one below sets the same one, and that cannot be
/// checked with a single layer. The startup configuration is taken from the
/// theme the user wrote, which is what the window would have set.
pub(super) async fn host_con_capas_apiladas(
    dir_sistema: &std::path::Path,
    dir_usuario: &std::path::Path,
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    use norte_ui_host::settings::{ConfigLayer, HostPath, HostPaths};
    let mut ajustes = ajustes_de_prueba();
    ajustes.common.ui_theme = Some("tokyonight".to_owned());
    UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("orthodox").expect("layout"),
        viewport: (120, 40),
        settings: ajustes,
        paths: HostPaths {
            config_layers: vec![
                (
                    ConfigLayer::System,
                    HostPath {
                        path: dir_sistema.to_path_buf(),
                        missing: false,
                    },
                ),
                (
                    ConfigLayer::User,
                    HostPath {
                        path: dir_usuario.to_path_buf(),
                        missing: false,
                    },
                ),
            ],
            ..HostPaths::default()
        },
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("starts")
}

/// The same one, with favorites ALREADY loaded: the window reads them at
/// startup, so a test that only writes `norte.toml` sets up a host that does
/// not see them.
pub(super) async fn host_con_capas_y_favoritos(
    dir_usuario: &std::path::Path,
    favoritos: Vec<(&str, &str)>,
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    use norte_ui_host::settings::{ConfigLayer, HostPath, HostPaths};
    let mut ajustes = ajustes_de_prueba();
    ajustes.common.hotlist = favoritos
        .into_iter()
        .map(|(nombre, destino)| norte_config::HotlistItem {
            name: nombre.to_owned(),
            target: VPath::parse(destino).map_err(|_| "hotlist-invalid".to_owned()),
        })
        .collect();
    UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("orthodox").expect("layout"),
        viewport: (120, 40),
        settings: ajustes,
        paths: HostPaths {
            config_layers: vec![(
                ConfigLayer::User,
                HostPath {
                    path: dir_usuario.to_path_buf(),
                    missing: false,
                },
            )],
            ..HostPaths::default()
        },
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("starts")
}

/// The profile picker: it shows them, and choosing one APPLIES it.
///
/// A profile is for making the workspace look and behave differently, so
/// what is checked is that the change reaches the screen: here, through the
/// theme, which is what shows.
#[tokio::test]
async fn el_selector_de_perfiles_ensena_y_lo_elegido_se_aplica() {
    use norte_ui_host::dto::NativeEffect;
    let raiz = tempfile::tempdir().expect("temp");
    let fotos = raiz.path().join("profiles").join("fotos");
    std::fs::create_dir_all(&fotos).expect("mkdir");
    std::fs::write(
        fotos.join("norte.toml"),
        "[profile]\ntitle = \"Fotos\"\n\n[ui]\ntheme = \"nord\"\n",
    )
    .expect("write");

    let (h, _snap) = host_con_capas(raiz.path()).await;
    let mut sub = h.subscribe();
    let mut nativos = h.native_effects();

    ejecutar_por_paleta(&h, &mut sub, "profile.pick").await;
    // The list arrives from a background task: the snapshot that carries
    // it is the one to wait for, not just the next one that goes by.
    //
    // A hundred rounds and not six: six is a deadline, not a wait. The
    // background task competes with the rest of the suite for the
    // runtime, and under load — the machine compiling alongside — it
    // used to run past and the test would turn red with nothing broken.
    // A hundred is the same order as this file's neighboring waits.
    let mut selector = None;
    for _ in 0..100 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if let Some(p) = siguiente_foto(&mut sub).await.profiles {
            selector = Some(p);
            break;
        }
    }
    let p = selector.expect("the picker opened with the list");
    assert_eq!(p.rows.len(), 1, "the profile that is there: {:?}", p.rows);
    assert_eq!(p.rows[0].name, "fotos");
    assert_eq!(
        p.rows[0].title.as_deref(),
        Some("Fotos"),
        "its title comes from `[profile] title`"
    );
    assert!(!p.rows[0].active, "not set yet");

    // Choosing it applies it: its `[ui] theme` reaches the host.
    h.dispatch(tecla("Enter")).await.expect("host alive");
    let efecto = tokio::time::timeout(std::time::Duration::from_secs(2), nativos.recv())
        .await
        .expect("the theme notice fires")
        .expect("channel alive");
    let NativeEffect::ThemeChanged { name } = efecto else {
        panic!("the notice is the theme's: {efecto:?}");
    };
    assert_eq!(name, "nord", "the PROFILE's theme, not the previous one");
}

/// And a profile's theme can be a PATH, not just a preset (ADR 0020).
///
/// The window only looked at presets, so a profile with
/// `theme = "…/mio.toml"` was left with no new colors IN SILENCE — with the
/// terminal applying it, which is the divergence. Resolving it reads a
/// file, and this window cannot read inside the actor (rule 2): it steps
/// out and comes back through the mailbox, like saving the theme does.
#[tokio::test]
async fn el_tema_de_un_perfil_puede_ser_una_ruta() {
    use norte_ui_host::dto::NativeEffect;
    let raiz = tempfile::tempdir().expect("temp");
    let mio = raiz.path().join("mio.toml");
    std::fs::write(&mio, "name = \"mio\"\n").expect("write theme");
    let fotos = raiz.path().join("profiles").join("fotos");
    std::fs::create_dir_all(&fotos).expect("mkdir");
    std::fs::write(
        fotos.join("norte.toml"),
        format!(
            "[profile]\ntitle = \"Fotos\"\n\n[ui]\ntheme = \"{}\"\n",
            mio.display()
        ),
    )
    .expect("write");

    let (h, _snap) = host_con_capas(raiz.path()).await;
    let mut sub = h.subscribe();
    let mut nativos = h.native_effects();

    ejecutar_por_paleta(&h, &mut sub, "profile.pick").await;
    let mut abierto = false;
    for _ in 0..100 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if siguiente_foto(&mut sub).await.profiles.is_some() {
            abierto = true;
            break;
        }
    }
    assert!(abierto, "the picker opened with the list");

    h.dispatch(tecla("Enter")).await.expect("host alive");
    let efecto = tokio::time::timeout(std::time::Duration::from_secs(2), nativos.recv())
        .await
        .expect("the theme notice fires")
        .expect("channel alive");
    let NativeEffect::ThemeChanged { name } = efecto else {
        panic!("the notice is the theme's: {efecto:?}");
    };
    assert_eq!(
        name,
        mio.display().to_string(),
        "the profile's theme was a file, and it was applied"
    );
}

/// **The window ADDS a favorite, not just opens the list** (#309).
///
/// And the name comes suggested by the SHARED model: saving REPLACES the
/// favorite already sharing that name, so with the field prefilled the
/// reflex of accepting without reading would overwrite one pointing
/// somewhere else.
#[tokio::test]
async fn la_ventana_guarda_un_favorito_con_el_nombre_sugerido() {
    let raiz = tempfile::tempdir().expect("temp");
    let (h, _snap) = host_con_capas(raiz.path()).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.hotlist").await;
    // `dialog.add` over the favorites list: in the terminal it is the same
    // popup's `a`.
    h.dispatch(tecla("a")).await.expect("host alive");
    let d = siguientes_dialogos(&mut sub).await;
    assert_eq!(d[0].title_key, "modal-hotlist-name-title");
    assert_eq!(
        d[0].input.as_deref(),
        Some("casa"),
        "prefilled with the shared suggestion: {:?}",
        d[0].input
    );

    h.dispatch(UiAction::Dialog {
        id: d[0].id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    // The write comes back through `spawn_blocking` and, on returning, the
    // host RE-SEEDS the favorites list that is open: waiting for the screen
    // to paint it is waiting for the file to be written, without guessing
    // how long it takes.
    // The write comes back through `spawn_blocking`, and on confirming the
    // list closes: nothing is left on screen to say "it's done". The FILE
    // is watched, which is what the test asserts, taking a turn through the
    // actor between glances instead of sleeping a fixed span.
    let escrito = foto_hasta(&h, &mut sub, "the favorite written to norte.toml", |_| {
        std::fs::read_to_string(raiz.path().join("norte.toml"))
            .ok()
            .filter(|s| s.contains("casa"))
    })
    .await;
    assert!(
        escrito.contains("casa"),
        "the favorite ended up in the file: {escrito}"
    );
}

/// And it REMOVES one, which was the other half that was missing (#309).
#[tokio::test]
async fn la_ventana_quita_el_favorito_del_cursor() {
    let raiz = tempfile::tempdir().expect("temp");
    std::fs::write(
        raiz.path().join("norte.toml"),
        "[[hotlist]]\nname = \"casa\"\npath = \"mem:///casa\"\n",
    )
    .expect("escribe");
    let (h, _snap) = host_con_capas_y_favoritos(raiz.path(), vec![("casa", "mem:///casa")]).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.hotlist").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let foto = siguiente_foto(&mut sub).await;
    let filas = foto.picker.as_ref().map_or(0, |p| p.rows.len());
    assert_eq!(filas, 1, "the list carries the favorite: {:?}", foto.picker);
    // `dialog.remove`: the terminal popup's `d`.
    let ack = h.dispatch(tecla("d")).await.expect("host alive");
    assert!(
        matches!(ack, norte_ui_host::ActionAck::Applied { .. }),
        "the picker handles the key: {ack:?}"
    );
    // Same as when adding: the open list re-seeds when the write comes back,
    // so the row leaving is the signal that the file is already updated.
    let despues = foto_hasta(&h, &mut sub, "the list without the favorite", |f| {
        f.picker
            .as_ref()
            .is_some_and(|p| p.rows.is_empty())
            .then(|| f.clone())
    })
    .await;

    let escrito = std::fs::read_to_string(raiz.path().join("norte.toml")).expect("norte.toml");
    assert!(
        !escrito.contains("casa"),
        "the favorite left the file: {escrito}; filas={:?} msg={:?}",
        despues.picker.as_ref().map(|p| p.rows.len()),
        despues.status.message
    );
}

/// A profile that does not exist changes nothing, and it says so.
///
/// "You stay on the one you were on" is what ADR 0079 D7 asks for a change:
/// starting with no profile is recoverable, stopping halfway is not.
#[tokio::test]
async fn un_perfil_que_no_carga_deja_todo_como_estaba() {
    let raiz = tempfile::tempdir().expect("temp");
    std::fs::create_dir_all(raiz.path().join("profiles")).expect("mkdir");
    let (h, _snap) = host_con_capas(raiz.path()).await;
    let mut sub = h.subscribe();

    // With no profiles, cycling has nowhere to go — and it says so instead
    // of pretending.
    ejecutar_por_paleta(&h, &mut sub, "profile.next").await;
    // With no round count: reading `profiles/` is a background task, so
    // the notice does not arrive in the next snapshot but when that task
    // answers. With six resyncs in a row, a loaded machine used to spend
    // them all before the background thread woke up, and the test turned
    // red with nothing broken — which is how a red gets learned to ignore.
    foto_hasta(
        &h,
        &mut sub,
        "the notice that there is no other profile",
        |f| f.status.message.is_some().then_some(()),
    )
    .await;
}

/// With `[ui] parent_entry`, the listing carries its `..` row — and it is
/// not an operand.
///
/// The row anyone coming from any manager in the family expects: the cursor
/// lands on it and Enter goes up. What makes it safe is that nothing is
/// ever marked on it, so a copy or a delete have nothing to act on instead
/// of acting on the parent directory.
#[tokio::test]
async fn con_la_fila_de_subir_el_listado_la_lleva_primera() {
    let mut cfg = norte_ui_host::ajustes_por_defecto();
    cfg.common.ui_parent_entry = Some(true);
    let (_h, snap) = UiHost::start(UiHostOptions {
        backend: arbol(),
        // A SUBdirectory: at a root there is nowhere to go up to and the
        // row does not appear no matter how much the config turns it on.
        initial_dir: norte_proto::VPath::parse("mem:///casa").expect("wire"),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: cfg,
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("starts");

    let filas = snap
        .slots
        .iter()
        .find_map(|v| match v {
            SlotView::Browser(b) => Some(b.rows.clone()),
            _ => None,
        })
        .expect("there is a listing");
    assert_eq!(
        filas.first().map(|r| r.display_name.as_str()),
        Some(".."),
        "the first row is the go-up one, painted `..` and not with the \
         parent's name: {filas:?}"
    );
    assert_eq!(
        filas[0].kind,
        norte_ui_host::dto::RowKind::Dir,
        "and it is a directory: Enter goes up through the same path as any other"
    );
}

/// Dragging the border splits the pair, and what one gains the other loses.
///
/// The renderer sends where the POINTER is, in cells. Which pair splits and
/// how much each one gets is decided by the host, which is the one holding
/// the layout and each kind's minimums.
#[tokio::test]
async fn arrastrar_el_borde_reparte_los_dos_huecos() {
    let (h, snap) = host_con_layout(arbol(), "orthodox", (120, 40)).await;
    let ancho = |s: &norte_ui_host::ViewSnapshot, id: u32| {
        s.layout
            .placements
            .iter()
            .find(|p| p.slot_id == id)
            .map(|p| p.width)
            .expect("the slot is placed")
    };
    let izq = snap.layout.placements[0].slot_id;
    let der = snap.layout.placements[1].slot_id;
    let (a0, b0) = (ancho(&snap, izq), ancho(&snap, der));
    assert_eq!(a0 + b0, 120, "both split the screen");

    let mut sub = h.subscribe();
    // The pointer at a third of the width.
    h.dispatch(UiAction::ResizeSlot {
        slot_id: izq,
        cells: 40,
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let despues = siguiente_foto(&mut sub).await;
    // With ONE cell of margin: the pair renormalizes to weights between 1
    // and 100 and the layout splits again in integers, so a third of 120
    // lands on 39 or 40 depending on which way the rounding falls. Demanding
    // the exact cell would be demanding the drag not go through weights.
    let ancho_izq = ancho(&despues, izq);
    assert!(
        ancho_izq.abs_diff(40) <= 1,
        "the border goes where the pointer says: {ancho_izq}"
    );
    assert_eq!(
        ancho(&despues, izq) + ancho(&despues, der),
        a0 + b0,
        "the pair occupies the same: dragging one border does not touch the rest"
    );
}

/// The theme screen CHOOSES, and what is chosen shows.
///
/// Before, it only showed: whoever hosts this window resolves the theme
/// once on startup, so a chosen theme had no way to reach the screen. With
/// `NativeEffect::ThemeChanged` it does, and this picker is the terminal's —
/// presets, cursor on the one that is set, and LIVE preview.
#[tokio::test]
async fn el_selector_de_tema_elige_y_avisa_a_quien_hospeda() {
    use norte_ui_host::dto::NativeEffect;
    let (h, _snap) = host_con_layout(arbol(), "orthodox", (120, 40)).await;
    let mut nativos = h.native_effects();
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "app.theme").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let abierta = siguiente_foto(&mut sub).await;
    let tema = abierta.theme.expect("the theme screen is open");
    assert!(
        tema.choices.len() > 1,
        "there is something to choose from: {:?}",
        tema.choices
    );

    // Moving down previews: the effect fires BEFORE confirming anything,
    // which is what makes the reader see the theme instead of reading its
    // name.
    h.dispatch(tecla("ArrowDown")).await.expect("host alive");
    let efecto = tokio::time::timeout(std::time::Duration::from_secs(2), nativos.recv())
        .await
        .expect("the theme notice fires")
        .expect("channel alive");
    let NativeEffect::ThemeChanged { name } = efecto else {
        panic!("the notice is the theme's: {efecto:?}");
    };
    assert_eq!(
        name, tema.choices[1],
        "the one that ended up under the cursor, not another"
    );

    // And `Escape` GOES BACK to the one that was set: a picker with live
    // preview that closes leaving the last one the cursor brushed is a way
    // of changing the theme by accident.
    h.dispatch(tecla("Escape")).await.expect("host alive");
    let vuelta = tokio::time::timeout(std::time::Duration::from_secs(2), nativos.recv())
        .await
        .expect("the return notice fires")
        .expect("channel alive");
    let NativeEffect::ThemeChanged { name } = vuelta else {
        panic!("the notice is the theme's: {vuelta:?}");
    };
    assert_eq!(name, tema.name, "it goes back to the one that was set");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert!(
        siguiente_foto(&mut sub).await.theme.is_none(),
        "and the screen closes"
    );
}

/// The menu bar: it drops down, gets navigated, and what is chosen RUNS.
///
/// The menus and their entries are `norte_frontend::menu`, the same model
/// the TUI paints, so what is checked here is not WHAT is inside — that is
/// covered by that crate's tests — but that the window projects it,
/// navigates it and runs it through the same path as a key.
#[tokio::test]
async fn el_menu_se_recorre_y_lo_elegido_corre() {
    let (h, snap) = host_con_layout(arbol(), "orthodox", (120, 40)).await;
    assert!(snap.menu.bar, "the bar is painted by default");
    assert_eq!(snap.menu.open, None, "and it is born closed");
    assert_eq!(
        snap.menu.titles.len(),
        norte_frontend::menu::MENUS.len(),
        "all the shared model's menus"
    );

    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "app.menu").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let abierto = siguiente_foto(&mut sub).await;
    assert_eq!(abierto.menu.open, Some(0), "it drops down at the first one");
    assert!(
        !abierto.menu.items.is_empty(),
        "and it carries its entries: {:?}",
        abierto.menu.items
    );

    // A down arrow moves the cursor INSIDE the menu, not the listing.
    let cursor_del_listado = |s: &norte_ui_host::ViewSnapshot| {
        s.slots.iter().find_map(|v| match v {
            SlotView::Browser(b) => Some(b.cursor),
            _ => None,
        })
    };
    let cursor_antes = cursor_del_listado(&abierto);
    h.dispatch(tecla("ArrowDown")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let movido = siguiente_foto(&mut sub).await;
    assert_eq!(movido.menu.cursor, 1);
    assert_eq!(
        cursor_del_listado(&movido),
        cursor_antes,
        "the listing underneath did not move"
    );

    // And `Escape` closes without running anything.
    h.dispatch(tecla("Escape")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert_eq!(siguiente_foto(&mut sub).await.menu.open, None);
}

/// #324: the panel bar crosses the bridge with what the TUI paints — which
/// panels there are, in what order, which is open and which has the
/// keyboard — and pressing a button opens the panel through the SAME
/// dispatch as its shortcut. The new bar travels as a PATCH in the same
/// send that opens the panel, with `alternar_hueco` never knowing it
/// exists.
#[tokio::test]
async fn la_barra_de_paneles_ensena_los_paneles_y_un_click_los_abre() {
    use norte_ui_host::dto::{PanelButtonState, ViewChange};
    let (h, snap) = host_arbol(arbol()).await;
    let barra = &snap.panel_bar;
    assert!(barra.bar, "the bar is painted by default, like in the TUI");
    let kinds: Vec<&str> = barra.buttons.iter().map(|b| b.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "places",
            "viewer",
            "processes",
            "metadata",
            "tree",
            "log",
            // Phase 4: the disk map. The window does not PAINT it yet (T5),
            // but the kind belongs to the SHARED registry, so its button
            // shows up here from the moment it is declared — which is
            // exactly what this test checks: the same buttons and the same
            // order as the TUI.
            "disk-map",
            // Phase 7: the timeline, likewise — the kind belongs to the
            // shared registry, so its button shows up here from the moment
            // it is declared even though the window does not paint it yet
            // (#359).
            "timeline",
            // #362: the terminal panel, same as its two neighbors — the
            // kind belongs to the shared registry, so its button shows up
            // here from the moment it is declared even though the window
            // does not paint it yet. And in the window the kind will
            // really be needed: the grid is built by `norte-term` once for
            // both frontends (T4).
            "terminal",
        ],
        "the same buttons and the same order as `panelbar::buttons`"
    );
    let sitios = kinds.iter().position(|k| *k == "places").expect("places");
    let boton = &barra.buttons[sitios];
    assert_eq!(
        boton.label, "Sitios",
        "translated into the session's language"
    );
    assert_eq!(boton.letter, "S");
    assert_eq!(boton.state, PanelButtonState::Closed, "{barra:?}");
    assert!(
        barra.buttons.iter().all(|b| !b.attention),
        "with no tasks or notices nothing has anything new: {barra:?}"
    );

    let mut sub = h.subscribe();
    h.dispatch(UiAction::PanelBarActivate {
        button: u32::try_from(sitios).expect("six buttons fit in a u32"),
    })
    .await
    .expect("host alive");
    // What opens the panel CARRIES the new bar: opening a slot changes the
    // layout and goes as a SNAPSHOT, and the snapshot carries the bar; a
    // change that went as a patch would carry it as `ViewChange::PanelBar`.
    // Both forms are accepted, and with a deadline: a host that did not
    // send it would leave this `recv` waiting forever, and a hung test is
    // not a red test.
    let mut barra_nueva = None;
    let plazo = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while barra_nueva.is_none() {
        let siguiente = tokio::time::timeout_at(plazo, sub.recv())
            .await
            .expect("the new bar arrives before five seconds")
            .expect("host alive");
        let Update::Message(m) = siguiente else {
            continue;
        };
        match m.payload {
            UiUpdate::Snapshot(s) => barra_nueva = Some(s.panel_bar),
            UiUpdate::Patch(p) => {
                if let Some(ViewChange::PanelBar { panel_bar }) = p
                    .changes
                    .into_iter()
                    .find(|c| matches!(c, ViewChange::PanelBar { .. }))
                {
                    barra_nueva = Some(panel_bar);
                }
            }
            UiUpdate::Notice(_) => {}
        }
    }
    let barra = barra_nueva.expect("the bar travelled");
    assert_ne!(
        barra.buttons[sitios].state,
        PanelButtonState::Closed,
        "the places panel is open: {barra:?}"
    );

    // Opening the places bar triggers a volumes read that lands as ANOTHER
    // snapshot, later: it waits for the snapshot that shows the requested
    // state, not the next one that happens to be in the queue.
    let con_sitios =
        |s: &norte_ui_host::ViewSnapshot| s.slots.iter().any(|v| matches!(v, SlotView::Places(_)));
    let abierto = foto_hasta(&h, &mut sub, "the places slot placed", |s| {
        con_sitios(s).then(|| s.clone())
    })
    .await;
    assert_ne!(
        abierto.panel_bar.buttons[sitios].state,
        PanelButtonState::Closed
    );

    // The same button again CLOSES it: it is a toggle, like its shortcut.
    let ack = h
        .dispatch(UiAction::PanelBarActivate {
            button: u32::try_from(sitios).expect("six buttons fit in a u32"),
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "was {ack:?}");
    let cerrado = foto_hasta(&h, &mut sub, "the places slot closed", |s| {
        (!con_sitios(s)).then(|| s.clone())
    })
    .await;
    assert_eq!(
        cerrado.panel_bar.buttons[sitios].state,
        PanelButtonState::Closed
    );

    // An index the bar does not have is a stale bar: it should ask for a
    // snapshot.
    let ack = h
        .dispatch(UiAction::PanelBarActivate { button: 99 })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, norte_ui_host::ActionAck::Stale { .. }),
        "was {ack:?}"
    );
}

/// ADR 0134 (phase F): two panels on the same border share a spot as tabs;
/// the group is a PANELS one and its tabs carry the panel's name. Pressing
/// the hidden one brings it to the front, and pressing the visible one
/// closes it and dissolves the group.
#[tokio::test]
async fn los_paneles_de_un_borde_comparten_sitio_en_pestanas() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "layout.timeline").await;
    ejecutar_por_paleta(&h, &mut sub, "layout.metadata").await;
    let foto = foto_hasta(&h, &mut sub, "a panels group", |s| {
        s.layout.tabs.iter().find(|g| g.panels).cloned()
    })
    .await;
    let titulos: Vec<&str> = foto.tabs.iter().map(|t| t.title.as_str()).collect();
    // The panel bar's names, not the kind ids.
    assert_eq!(titulos, ["Historial", "Detalles"], "{foto:?}");
    assert_eq!(foto.active, 1, "the one that arrives stays in front");

    // The timeline is hidden: pressing it SHOWS it.
    ejecutar_por_paleta(&h, &mut sub, "layout.timeline").await;
    let g = foto_hasta(&h, &mut sub, "the timeline in front", |s| {
        s.layout
            .tabs
            .iter()
            .find(|g| g.panels && g.active == 0)
            .cloned()
    })
    .await;
    assert_eq!(g.tabs.len(), 2, "nothing closed");

    // Visible: now it does close it, and the group of one dissolves.
    ejecutar_por_paleta(&h, &mut sub, "layout.timeline").await;
    let () = foto_hasta(&h, &mut sub, "the group dissolved", |s| {
        s.layout.tabs.iter().all(|g| !g.panels).then_some(())
    })
    .await;
}

/// ADR 0133: the snapshot carries the four layout buttons with their name,
/// pressing "split" places one more listing, and an id or a tab that do not
/// exist are a race that asks for a snapshot.
#[tokio::test]
async fn los_botones_de_disposicion_y_de_pestana_se_pulsan() {
    let (h, snap) = host_arbol(arbol()).await;
    let ids: Vec<&str> = snap.layout_buttons.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(ids, ["split-h", "split-v", "equalize", "flip", "pick"]);
    assert_eq!(snap.layout_buttons[0].label, "Partir lado a lado");
    let listados = |s: &norte_ui_host::ViewSnapshot| {
        s.slots
            .iter()
            .filter(|v| matches!(v, SlotView::Browser(_)))
            .count()
    };
    let antes = listados(&snap);

    let mut sub = h.subscribe();
    let ack = h
        .dispatch(UiAction::LayoutButtonActivate {
            id: "split-h".to_owned(),
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "was {ack:?}");
    let () = foto_hasta(&h, &mut sub, "one more listing", |s| {
        (listados(s) > antes).then_some(())
    })
    .await;

    for accion in [
        UiAction::LayoutButtonActivate {
            id: "no-existe".to_owned(),
        },
        UiAction::TabAction {
            slot_id: 9999,
            verb: norte_ui_host::TabVerb::New,
        },
    ] {
        let ack = h.dispatch(accion).await.expect("host alive");
        assert!(matches!(ack, ActionAck::Stale { .. }), "was {ack:?}");
    }
}

/// Regression (2026-09-21 capture): with two listings and details on the
/// right, dragging the border between the SECOND listing and details
/// narrows them. Only that listing was being measured, not the whole body,
/// and the border was not following the pointer.
#[tokio::test]
async fn el_borde_de_los_detalles_se_arrastra_desde_el_segundo_listado() {
    let (h, _) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let _ = h
        .dispatch(UiAction::LayoutButtonActivate {
            id: "split-h".to_owned(),
        })
        .await
        .expect("host alive");
    ejecutar_por_paleta(&h, &mut sub, "layout.metadata").await;
    let detalles = |s: &norte_ui_host::ViewSnapshot| {
        s.slots.iter().find_map(|v| match v {
            SlotView::Metadata(m) => Some(m.slot_id),
            _ => None,
        })
    };
    let snap = foto_hasta(&h, &mut sub, "two listings and the details", |s| {
        let n = s
            .slots
            .iter()
            .filter(|v| matches!(v, SlotView::Browser(_)))
            .count();
        (n == 2 && detalles(s).is_some()).then(|| s.clone())
    })
    .await;
    let meta_id = detalles(&snap).expect("details");
    let sitio = |s: &norte_ui_host::ViewSnapshot, id: u32| {
        *s.layout
            .placements
            .iter()
            .find(|p| p.slot_id == id)
            .expect("placed")
    };
    let meta = sitio(&snap, meta_id);
    // The listing touching the details on the left.
    let listado = snap
        .layout
        .placements
        .iter()
        .find(|p| p.x + p.width == meta.x)
        .expect("neighbor")
        .slot_id;
    // The capture's state: the details MOVED to the right of the listing
    // come in with their weight, and three end up weighted in a layout.
    let _ = h
        .dispatch(UiAction::MoveSlot {
            slot_id: meta_id,
            target: listado,
            zone: norte_frontend::layout::DropZone::Right,
        })
        .await
        .expect("host alive");
    let snap = foto_hasta(&h, &mut sub, "details with weight", |s| {
        let m = sitio(s, meta_id);
        (m.width != meta.width).then(|| s.clone())
    })
    .await;
    let meta = sitio(&snap, meta_id);
    let otro_ancho = snap
        .layout
        .placements
        .iter()
        .find(|p| p.slot_id != listado && p.slot_id != meta_id && p.y == meta.y)
        .map(|p| p.width)
        .expect("the other listing");
    let ack = h
        .dispatch(UiAction::ResizeSlot {
            slot_id: listado,
            cells: meta.x + 10,
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "was {ack:?}");
    let ancho = meta.width;
    let despues = foto_hasta(&h, &mut sub, "details about ten cells narrower", |s| {
        let p = s.layout.placements.iter().find(|p| p.slot_id == meta_id)?;
        (p.width + 9 <= ancho && p.width + 11 >= ancho).then(|| s.clone())
    })
    .await;
    // And the other listing, which nobody grabbed, does not notice.
    let otro = despues
        .layout
        .placements
        .iter()
        .find(|p| p.slot_id != listado && p.slot_id != meta_id && p.y == meta.y)
        .map(|p| p.width)
        .expect("the other listing");
    assert!(
        otro.abs_diff(otro_ancho) <= 1,
        "the third one moved: {otro_ancho} → {otro}"
    );
}

/// Dragged sizes ARE REMEMBERED: what one window leaves in the session is
/// what the next one opens with, until a different layout is chosen.
#[tokio::test]
async fn los_tamanos_arrastrados_vuelven_al_abrir() {
    let falso = super::arbol_como_falso();
    // Owner of an empty session: the only one that writes.
    *falso.sesion.lock().expect("session") = (
        norte_proto::methods::Session {
            version: 0,
            revision: 0,
            body: serde_json::Value::Null,
        },
        true,
    );
    let falso = Arc::new(falso);
    let (h, _) = host_arbol(Arc::clone(&falso)).await;
    let mut sub = h.subscribe();
    let _ = h
        .dispatch(UiAction::LayoutButtonActivate {
            id: "split-h".to_owned(),
        })
        .await
        .expect("host alive");
    let snap = foto_hasta(&h, &mut sub, "two listings", |s| {
        let n = s
            .slots
            .iter()
            .filter(|v| matches!(v, SlotView::Browser(_)))
            .count();
        (n == 2).then(|| s.clone())
    })
    .await;
    let izq = *snap
        .layout
        .placements
        .iter()
        .filter(|p| {
            snap.slots
                .iter()
                .any(|v| matches!(v, SlotView::Browser(b) if b.slot_id == p.slot_id))
        })
        .min_by_key(|p| p.x)
        .expect("listing");
    // The border at a third.
    let _ = h
        .dispatch(UiAction::ResizeSlot {
            slot_id: izq.slot_id,
            cells: (izq.x + izq.width * 2) / 3,
        })
        .await
        .expect("host alive");
    let movido = foto_hasta(&h, &mut sub, "border moved", |s| {
        let p = s
            .layout
            .placements
            .iter()
            .find(|p| p.slot_id == izq.slot_id)?;
        (p.width < izq.width).then(|| s.clone())
    })
    .await;
    let anchos = |s: &norte_ui_host::ViewSnapshot| -> Vec<(u32, u16)> {
        let mut v: Vec<(u32, u16)> = s
            .layout
            .placements
            .iter()
            .map(|p| (p.slot_id, p.width))
            .collect();
        v.sort_unstable();
        v
    };
    // What was written, as soon as it is written.
    let mut escrito = None;
    for _ in 0..50 {
        escrito = falso.escrito.lock().expect("escrito").clone();
        let lleva = escrito
            .as_ref()
            .is_some_and(|b| b.to_string().contains(&format!("\"id\":{}", izq.slot_id)));
        if lleva {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let escrito = escrito.expect("the window wrote its session");
    drop(h);

    // Another window, over what the first one left.
    let segundo = super::arbol_como_falso();
    *segundo.sesion.lock().expect("session") = (
        norte_proto::methods::Session {
            version: norte_frontend::session::SCHEMA_VERSION,
            revision: 9,
            body: escrito,
        },
        true,
    );
    let (_h2, snap2) = host_arbol(Arc::new(segundo)).await;
    assert_eq!(
        anchos(&snap2),
        anchos(&movido),
        "it opens with the same widths"
    );
}

/// Regression (second capture): the HORIZONTAL border between the details
/// and the log below moves the log up and down, grabbed from the details.
#[tokio::test]
async fn el_borde_del_registro_se_arrastra_desde_los_detalles() {
    // The session tree from the capture, exactly as the window saved it.
    let sesion = r#"{"split": {"children": [{"split": {"children": [
        {"slot": {"id": 2, "kind": "browser"}}, {"slot": {"id": 1, "kind": "browser"}},
        {"slot": {"bindings": {"follows": {"role": "active"}}, "id": 5, "kind": "metadata"}}],
        "dir": "horizontal", "sizes": [{"weight": 1}, {"weight": 1}, {"weight": 1}]}},
        {"slot": {"id": 6, "kind": "log"}}, {"slot": {"id": 3, "kind": "tasks"}},
        {"slot": {"id": 4, "kind": "status"}}], "dir": "vertical",
        "sizes": [{"weight": 1}, {"fixed": 11}, "auto", {"fixed": 1}]}}"#;
    let tree: norte_frontend::layout::Node = serde_json::from_str(sesion).expect("tree");
    let (h, _) = super::base::host_con_arbol(arbol(), tree, (160, 50)).await;
    let mut sub = h.subscribe();
    let _ = h.dispatch(UiAction::Resync).await;
    let kind_de = |s: &norte_ui_host::ViewSnapshot, quiere: &str| {
        s.slots.iter().find_map(|v| match (v, quiere) {
            (SlotView::Metadata(m), "metadata") => Some(m.slot_id),
            (SlotView::Log(l), "log") => Some(l.slot_id),
            _ => None,
        })
    };
    let snap = foto_hasta(&h, &mut sub, "detalles y registro", |s| {
        (kind_de(s, "metadata").is_some() && kind_de(s, "log").is_some()).then(|| s.clone())
    })
    .await;
    let sitio = |s: &norte_ui_host::ViewSnapshot, id: u32| {
        *s.layout
            .placements
            .iter()
            .find(|p| p.slot_id == id)
            .expect("colocado")
    };
    let meta_id = kind_de(&snap, "metadata").expect("details");
    let log_id = kind_de(&snap, "log").expect("log");
    let meta = sitio(&snap, meta_id);
    let log = sitio(&snap, log_id);
    assert_eq!(meta.y + meta.height, log.y, "the details touch the log");
    // Move the border up five rows: the log grows by five.
    let ack = h
        .dispatch(UiAction::ResizeSlot {
            slot_id: meta_id,
            cells: log.y - 5,
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "was {ack:?}");
    let alto = log.height;
    let () = foto_hasta(&h, &mut sub, "log five rows taller", |s| {
        (sitio(s, log_id).height == alto + 5).then_some(())
    })
    .await;
}

/// ADR 0138: dropping one listing below the other stacks them;
/// `layout.flip` puts them back side by side.
#[tokio::test]
async fn mover_y_girar_reparten_los_listados() {
    let (h, _) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let listados = |s: &norte_ui_host::ViewSnapshot| -> Vec<u32> {
        s.slots
            .iter()
            .filter_map(|v| match v {
                SlotView::Browser(b) => Some(b.slot_id),
                _ => None,
            })
            .collect()
    };
    let _ = h
        .dispatch(UiAction::LayoutButtonActivate {
            id: "split-h".to_owned(),
        })
        .await
        .expect("host alive");
    let snap = foto_hasta(&h, &mut sub, "two listings", |s| {
        (listados(s).len() == 2).then(|| s.clone())
    })
    .await;
    let [a, b] = listados(&snap)[..] else {
        unreachable!("two, from the wait")
    };
    let sitio = |s: &norte_ui_host::ViewSnapshot, id: u32| {
        s.layout
            .placements
            .iter()
            .find(|p| p.slot_id == id)
            .map(|p| (p.x, p.y))
    };
    let (ax, ay) = sitio(&snap, a).expect("a placed");
    let (bx, by) = sitio(&snap, b).expect("b placed");
    assert!(ay == by && ax < bx, "side by side at the start");

    let ack = h
        .dispatch(UiAction::MoveSlot {
            slot_id: a,
            target: b,
            zone: norte_frontend::layout::DropZone::Bottom,
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "was {ack:?}");
    let () = foto_hasta(&h, &mut sub, "a below b", |s| {
        let (ax, ay) = sitio(s, a)?;
        let (bx, by) = sitio(s, b)?;
        (ax == bx && ay > by).then_some(())
    })
    .await;

    ejecutar_por_paleta(&h, &mut sub, "layout.flip").await;
    let () = foto_hasta(&h, &mut sub, "side by side again", |s| {
        let (ax, ay) = sitio(s, a)?;
        let (bx, by) = sitio(s, b)?;
        (ay == by && ax != bx).then_some(())
    })
    .await;
}

/// ADR 0132: the snapshot carries the status bar's right half, with the
/// default items that have something to say, and pressing one runs its
/// command; an id that is no longer there is a race and asks for a
/// snapshot.
#[tokio::test]
async fn la_barra_de_estado_trae_sus_elementos_y_se_pulsan() {
    let (h, snap) = host_arbol(arbol()).await;
    let ids: Vec<&str> = snap.status_items.iter().map(|i| i.id.as_str()).collect();
    // With no marks, no tasks and no notices, those three stay silent.
    assert_eq!(
        ids,
        ["position", "sort", "encoding"],
        "{:?}",
        snap.status_items
    );
    let orden = snap
        .status_items
        .iter()
        .find(|i| i.id == "sort")
        .expect("sort");
    assert!(orden.clickable);
    assert!(!snap.status_items[0].clickable, "position is not clickable");

    let ack = h
        .dispatch(UiAction::StatusItemActivate {
            id: "sort".to_owned(),
        })
        .await
        .expect("host alive");
    assert!(
        !matches!(ack, ActionAck::Stale { .. }),
        "sort is clickable: {ack:?}"
    );
    // `tasks` is not there (there are no tasks): what the renderer clicked
    // no longer exists.
    let ack = h
        .dispatch(UiAction::StatusItemActivate {
            id: "tasks".to_owned(),
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Stale { .. }), "was {ack:?}");
}

/// #291: the preview slot FOLLOWS the cursor and shows the same viewer as
/// the big one — with the plugin's preview and its fragments — over a
/// directory it says so, and closing it removes it. The last of ADR 0058's
/// seven kinds the window was not painting.
#[tokio::test]
async fn el_hueco_de_preview_sigue_al_cursor_y_ensena_el_visor() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"main.rs".to_vec(), false)],
    );
    f.contenido
        .insert("mem:///casa/main.rs".to_owned(), b"fn main() {}".to_vec());
    f.previews.insert(
        "mem:///casa/main.rs".to_owned(),
        norte_proto::methods::PluginPreviewStyled {
            plugin_id: "acme.syntax".to_owned(),
            plugin_name: "Syntax".to_owned(),
            lines: vec![vec![norte_proto::methods::SpanWire {
                text: "fn main() {}".to_owned(),
                role: Some("title".to_owned()),
                fg: None,
                bg: None,
            }]],
            lossy: false,
        },
    );
    let f = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&f)).await;
    let mut sub = h.subscribe();

    // Opening it is a catalogue command, the same one as in the TUI.
    ejecutar_por_paleta(&h, &mut sub, "layout.preview").await;
    let preview_de = |s: &norte_ui_host::ViewSnapshot| {
        s.slots.iter().find_map(|v| match v {
            SlotView::Preview(p) => Some(p.as_ref().clone()),
            _ => None,
        })
    };
    // The cursor is born on `..` or on `docs`: first the note. Until the
    // listing lands there is no cursor, and THAT note is a different one
    // ("nothing selected"): it waits for the directory's.
    let con_nota = foto_hasta(&h, &mut sub, "the preview slot over a directory", |s| {
        preview_de(s).filter(|p| p.viewer.is_none() && p.note == "directorio")
    })
    .await;
    assert!(con_nota.viewer.is_none(), "{con_nota:?}");

    // Move down to the file: the slot reads it on its own, and what it
    // shows is the plugin's preview, with its fragment and its "via".
    for _ in 0..3 {
        h.dispatch(tecla("ArrowDown")).await.expect("host alive");
    }
    let con_visor = foto_hasta(&h, &mut sub, "the preview slot with the file", |s| {
        preview_de(s).filter(|p| p.viewer.is_some())
    })
    .await;
    let visor = con_visor.viewer.expect("viewer");
    assert!(
        visor.path_display.ends_with("main.rs"),
        "{}",
        visor.path_display
    );
    assert_eq!(visor.styled.len(), 1);
    assert_eq!(visor.styled[0][0].role.as_deref(), Some("title"));
    assert!(visor.preview_by.contains("Syntax"));
    assert!(con_visor.note.is_empty());
    // And the width requested is the SLOT's, not the window's.
    let anchos = f.anchos_de_preview.lock().expect("mutex").clone();
    assert!(
        anchos.iter().all(|a| a.is_some_and(|a| a < 120)),
        "the previewer receives the slot's width: {anchos:?}"
    );

    // The same command closes it, and what it was showing goes with it.
    ejecutar_por_paleta(&h, &mut sub, "layout.preview").await;
    let cerrado = foto_hasta(&h, &mut sub, "no preview slot", |s| {
        preview_de(s).is_none().then(|| s.clone())
    })
    .await;
    assert!(cerrado.viewer.is_none(), "the BIG viewer did not open");
}

/// #291, second half: with FOCUS on the docked slot, the viewer's keys move
/// that viewer; the wheel moves it through the host; `viewer.close` returns
/// focus to the listing without closing the slot (like the TUI); and with
/// no focus, the arrows keep moving the listing.
#[tokio::test]
async fn el_hueco_de_preview_con_el_foco_se_mueve_con_las_teclas_del_visor() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"largo.txt".to_vec(), false)]);
    let texto = (1..=80)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    f.contenido
        .insert("mem:///casa/largo.txt".to_owned(), texto.into_bytes());
    let f = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&f)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "layout.preview").await;
    let preview_de = |s: &norte_ui_host::ViewSnapshot| {
        s.slots.iter().find_map(|v| match v {
            SlotView::Preview(p) => Some(p.as_ref().clone()),
            _ => None,
        })
    };
    // Move down to the file.
    for _ in 0..3 {
        h.dispatch(tecla("ArrowDown")).await.expect("host alive");
    }
    let con_visor = foto_hasta(&h, &mut sub, "the preview slot with the file", |s| {
        preview_de(s).filter(|p| p.viewer.is_some())
    })
    .await;
    let v = con_visor.viewer.as_ref().expect("viewer");
    assert_eq!(v.first_line, 0);
    assert!(
        v.lines.len() < 80 && v.lines.len() <= 38,
        "the WINDOW that fits the slot travels, not the file: {}",
        v.lines.len()
    );
    let slot = con_visor.slot_id;

    // With no focus, an arrow goes to the LISTING, not the viewer. Down and
    // not up: up would change the file under the cursor, and with it what
    // the slot shows — what is measured here is who the key went to.
    h.dispatch(tecla("ArrowDown")).await.expect("host alive");
    let sin_foco = foto_hasta(&h, &mut sub, "the arrow went to the listing", |s| {
        preview_de(s).filter(|p| p.viewer.as_ref().is_some_and(|v| v.first_line == 0))
    })
    .await;
    assert!(sin_foco.viewer.is_some());

    // With focus on the slot: the arrow moves the viewer.
    let ack = h
        .dispatch(UiAction::FocusSlot { slot_id: slot })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "focusing the slot: {ack:?}"
    );
    let enfocado = foto_hasta(&h, &mut sub, "the preview slot with focus", |s| {
        s.layout
            .placements
            .iter()
            .any(|p| p.slot_id == slot && p.role == Some(norte_ui_host::dto::SlotRole::Active))
            .then(|| s.clone())
    })
    .await;
    assert_eq!(enfocado.focus, Some(slot));
    let ack = h.dispatch(tecla("ArrowDown")).await.expect("host alive");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "the arrow in the viewer: {ack:?}"
    );
    let movido = foto_hasta(&h, &mut sub, "the docked viewer moved down a line", |s| {
        preview_de(s).filter(|p| p.viewer.as_ref().is_some_and(|v| v.first_line == 1))
    })
    .await;
    assert_eq!(movido.viewer.expect("viewer").first_line, 1);

    // La rueda, por el host.
    h.dispatch(UiAction::PreviewScroll {
        slot_id: slot,
        delta: 3,
    })
    .await
    .expect("host alive");
    foto_hasta(&h, &mut sub, "the wheel went down three more", |s| {
        preview_de(s).filter(|p| p.viewer.as_ref().is_some_and(|v| v.first_line == 4))
    })
    .await;

    // `viewer.close` (Esc in the viewer's keymap) returns focus to the
    // listing and leaves the slot where it is.
    h.dispatch(tecla("Escape")).await.expect("host alive");
    let devuelto = foto_hasta(&h, &mut sub, "focus returned to the listing", |s| {
        let activo = s
            .layout
            .placements
            .iter()
            .find(|p| p.role == Some(norte_ui_host::dto::SlotRole::Active))
            .map(|p| p.slot_id);
        (activo.is_some() && activo != Some(slot)).then(|| s.clone())
    })
    .await;
    assert!(preview_de(&devuelto).is_some(), "the slot is still open");
}

/// The menu REOPENS where it was, not at the first one.
///
/// Always opening at the first one forces the whole bar to be walked on each
/// gesture, and someone using two entries from the same menu pays for it
/// every time.
#[tokio::test]
async fn el_menu_se_reabre_por_donde_iba() {
    let (h, _snap) = host_con_layout(arbol(), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    // Open, move two menus to the right and close with `Escape`.
    h.dispatch(UiAction::MenuOpen { menu: 2 })
        .await
        .expect("host alive");
    h.dispatch(tecla("Escape")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert_eq!(
        siguiente_foto(&mut sub).await.menu.open,
        None,
        "closed entirely"
    );

    // And reopening it comes out at the same one.
    ejecutar_por_paleta(&h, &mut sub, "app.menu").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert_eq!(siguiente_foto(&mut sub).await.menu.open, Some(2));
}

/// Alt alone (bridge 68) opens the menu like `app.menu`, and again folds it.
#[tokio::test]
async fn alt_solo_abre_y_pliega_el_menu() {
    let (h, _snap) = host_con_layout(arbol(), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::MenuToggle).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert_eq!(siguiente_foto(&mut sub).await.menu.open, Some(0), "open");

    h.dispatch(UiAction::MenuToggle).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert_eq!(siguiente_foto(&mut sub).await.menu.open, None, "folded");
}

/// With a dialog in front, Alt alone opens nothing: F9 there is eaten by the
/// dialog, and a menu on top of a pending question would fight it for the
/// keyboard.
#[tokio::test]
async fn alt_solo_no_abre_el_menu_encima_de_un_dialogo() {
    let (h, _snap) = host_con_layout(arbol(), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "pane.mkdir").await;
    foto_hasta(&h, &mut sub, "the dialog is open", |s| {
        (!s.dialogs.is_empty()).then_some(())
    })
    .await;

    h.dispatch(UiAction::MenuToggle).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(foto.menu.open, None, "the menu does not open");
    assert!(!foto.dialogs.is_empty(), "the dialog remains");
}

/// Choosing in the menu runs the command, and the menu closes BEFORE.
///
/// Order matters: the command can open another screen, and doing so behind
/// the menu would leave it eating the keys of the one that just opened. It
/// is the same rule as the palette.
#[tokio::test]
async fn lo_elegido_en_el_menu_corre_y_el_menu_se_cierra_antes() {
    let (h, _snap) = host_con_layout(arbol(), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    // The "Help" menu and its first entry, which is `app.help`: it opens a
    // screen, so it serves to see that the menu does not stay on top.
    let ayuda = norte_frontend::menu::MENUS.len() - 1;
    h.dispatch(UiAction::MenuOpen {
        menu: u32::try_from(ayuda).expect("fits"),
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::MenuActivateRow { row: 0 })
        .await
        .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(foto.menu.open, None, "the menu closed");
    assert!(foto.help.is_some(), "and what was chosen ran");
}

/// The PROCESSES panel is exited with the same key it was entered with.
///
/// A ring that enters a panel and does not leave it is not a ring: it is a
/// trap, and the reader is left with no way back to the listing without a
/// mouse.
#[tokio::test]
async fn del_panel_de_procesos_se_sale_tabulando() {
    use norte_ui_host::dto::SlotRole;
    let (h, _snap) = host_con_layout(arbol(), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "layout.processes").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let abierto = siguiente_foto(&mut sub).await;
    let procesos = abierto
        .slots
        .iter()
        .find_map(|v| match v {
            SlotView::Processes { slot_id, .. } => Some(*slot_id),
            _ => None,
        })
        .expect("the panel is on screen");

    let activo = |s: &norte_ui_host::ViewSnapshot| {
        s.layout
            .placements
            .iter()
            .find(|p| p.role == Some(SlotRole::Active))
            .map(|p| p.slot_id)
    };
    // Walk the screen until landing on the processes panel...
    let mut dentro = false;
    for _ in 0..6 {
        h.dispatch(tecla_alt("o")).await.expect("host alive");
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if activo(&siguiente_foto(&mut sub).await) == Some(procesos) {
            dentro = true;
            break;
        }
    }
    assert!(dentro, "the ring reaches the processes panel");

    // ...and it leaves the same way it came in.
    h.dispatch(tecla_alt("o")).await.expect("host alive");
    let fuera = foto_hasta(&h, &mut sub, "focus left the panel", |foto| {
        activo(foto).filter(|id| *id != procesos)
    })
    .await;
    assert_ne!(
        fuera, procesos,
        "and it LEAVES it: a ring that enters and does not leave is a trap"
    );

    // And Tab also gets you out, even without entering: it goes back to a
    // LISTING. That is what guarantees no combination leaves the reader
    // stuck inside.
    let mut atras = false;
    for _ in 0..6 {
        h.dispatch(tecla_alt("o")).await.expect("host alive");
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if activo(&siguiente_foto(&mut sub).await) == Some(procesos) {
            atras = true;
            break;
        }
    }
    assert!(atras, "back inside the panel");
    h.dispatch(tecla("Tab")).await.expect("host alive");
    let foto = foto_hasta(&h, &mut sub, "`Tab` gets out of the side panel", |foto| {
        let id = activo(foto).filter(|id| *id != procesos)?;
        foto.slots
            .iter()
            .any(|v| matches!(v, SlotView::Browser(b) if b.slot_id == id))
            .then_some(id)
    })
    .await;
    assert_ne!(foto, procesos);
}

/// The keyboard ring does NOT stop at the attributes sheet.
///
/// The shared traversal (`focus_order`) carries everything FOCUSABLE, and the
/// sheet is: the roster counts it. But it does not take keys — it follows the
/// listing's cursor, and with the keyboard inside it would stop following
/// anything, which is half of #243 — so stopping there is a stop that no key
/// gets you out of: the arrows move nothing and there is nothing on screen
/// that explains it.
///
/// The TUI walks the ring with the same rule (`takes_keys` from the shared
/// registry), and a decision duplicated between frontends diverges silently
/// (ADR 0077).
#[tokio::test]
async fn el_anillo_no_se_para_en_la_hoja_de_atributos() {
    use norte_ui_host::dto::SlotRole;
    // Two listings, so the ring has somewhere to go when it skips the sheet:
    // with only one the correct answer is "there is no other slot".
    let (h, _snap) = host_con_layout(arbol(), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "layout.metadata").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let abierto = siguiente_foto(&mut sub).await;
    let hoja = abierto
        .slots
        .iter()
        .find_map(|v| match v {
            SlotView::Metadata(m) => Some(m.slot_id),
            _ => None,
        })
        .expect("the sheet is on screen");

    // A full lap of the ring: the sheet must not have focus at any point
    // of it.
    for _ in 0..6 {
        ejecutar_por_paleta(&h, &mut sub, "layout.focus-next").await;
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let foto = siguiente_foto(&mut sub).await;
        let activo = foto
            .layout
            .placements
            .iter()
            .find(|p| p.role == Some(SlotRole::Active))
            .map(|p| p.slot_id);
        assert_ne!(
            activo,
            Some(hoja),
            "the ring stopped at the attributes sheet, which does not take keys"
        );
    }
}

/// Tabs: open, walk, move, go to N and close (#288).
///
/// The group is carried by the SHARED model (`add_tab` wraps the slot if
/// needed, `move_tab` does not wrap around): here it is checked that the
/// gesture arrives, that the tab brought to front takes FOCUS with it —
/// working with one that is not visible is what this prevents — and that
/// the view says what is there.
#[tokio::test]
async fn las_pestanas_se_abren_se_recorren_y_se_cierran() {
    let backend = arbol();
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    assert!(snap.layout.tabs.is_empty(), "no group, no bar");

    ejecutar_por_paleta(&h, &mut sub, "pane.tab-new").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let foto = siguiente_foto(&mut sub).await;
    let grupo = foto.layout.tabs.first().expect("there is a group").clone();
    assert_eq!(grupo.tabs.len(), 2, "two tabs");
    assert_eq!(grupo.active, 1, "the new one ends up in front");
    assert_eq!(
        Some(grupo.tabs[1].slot_id),
        foto.focus,
        "and with focus: working on one that is not visible is what this prevents"
    );

    // Walking CYCLES.
    ejecutar_por_paleta(&h, &mut sub, "pane.tab-next").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(
        foto.layout.tabs.first().expect("group").active,
        0,
        "from the last to the first"
    );

    // Going to an N that does not exist is refused: guessing would mean
    // switching tabs on its own.
    let ack = ejecutar_por_paleta_ack(&h, &mut sub, "pane.tab-goto-9").await;
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-no-such-tab"),
        "{ack:?}"
    );

    // Closing the front one leaves one, and the group dissolves.
    ejecutar_por_paleta(&h, &mut sub, "pane.tab-close").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let foto = siguiente_foto(&mut sub).await;
    assert!(
        foto.layout.tabs.is_empty(),
        "a group of one is not a group: {:?}",
        foto.layout.tabs
    );
}

/// ADR 0133 (the review asked for it): a tab's `×` and `+` act on ITS group,
/// not on the one that had focus. Two groups, focus on the second, and a tab
/// from the first is closed: the first dissolves and the second keeps its
/// two.
#[tokio::test]
async fn el_boton_de_una_pestana_actua_sobre_su_grupo_y_no_sobre_el_del_foco() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // Two listings, one group in each.
    ejecutar_por_paleta(&h, &mut sub, "layout.split-h").await;
    ejecutar_por_paleta(&h, &mut sub, "pane.tab-new").await;
    ejecutar_por_paleta(&h, &mut sub, "pane.switch").await;
    ejecutar_por_paleta(&h, &mut sub, "pane.tab-new").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let foto = foto_hasta(&h, &mut sub, "two groups", |s| {
        (s.layout.tabs.len() == 2).then(|| s.clone())
    })
    .await;
    let (a, b) = (foto.layout.tabs[0].clone(), foto.layout.tabs[1].clone());
    let enfocado = foto.focus.expect("there is focus");
    // Focus is on one of the two; the OTHER one is clicked.
    let (pulsado, otro) = if b.tabs.iter().any(|t| t.slot_id == enfocado) {
        (a, b)
    } else {
        (b, a)
    };
    let ack = h
        .dispatch(UiAction::TabAction {
            slot_id: pulsado.tabs[0].slot_id,
            verb: norte_ui_host::TabVerb::Close,
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let foto = foto_hasta(&h, &mut sub, "one group fewer", |s| {
        (s.layout.tabs.len() == 1).then(|| s.clone())
    })
    .await;
    let queda = &foto.layout.tabs[0];
    assert_eq!(
        queda.tabs.iter().map(|t| t.slot_id).collect::<Vec<_>>(),
        otro.tabs.iter().map(|t| t.slot_id).collect::<Vec<_>>(),
        "the focused group stays whole; the clicked one closed"
    );
}

/// With no group, the tab commands SAY SO.
///
/// Closing the whole slot is a different command: doing it here "because
/// there were no tabs" would close what nobody asked to close.
#[tokio::test]
async fn sin_grupo_los_comandos_de_pestana_lo_dicen() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    for cmd in ["pane.tab-close", "pane.tab-next", "pane.tab-move-right"] {
        let ack = ejecutar_por_paleta_ack(&h, &mut sub, cmd).await;
        assert!(
            matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-no-tabs"),
            "{cmd}: {ack:?}"
        );
    }
}

/// A click on a tab brings it to front; against a tree that already changed,
/// it refuses instead of getting it right by chance.
#[tokio::test]
async fn un_clic_en_una_pestana_la_pone_delante() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "pane.tab-new").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let grupo = siguiente_foto(&mut sub)
        .await
        .layout
        .tabs
        .first()
        .expect("group")
        .clone();
    let primera = grupo.tabs[0].slot_id;

    let ack = h
        .dispatch(UiAction::SelectTab { slot_id: primera })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    // Every layout change sends its own snapshot, so the ones piling up in
    // the queue are from BEFORE: it looks for the one that already reflects
    // the click instead of reading the first one that comes out.
    let mut visto = None;
    for _ in 0..10 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let foto = siguiente_foto(&mut sub).await;
        if foto.layout.tabs.first().is_some_and(|g| g.active == 0) {
            visto = Some(foto);
            break;
        }
    }
    let foto = visto.expect("the click brings the first one to front");
    assert_eq!(foto.focus, Some(primera));

    // A slot that is not in any group: stale, not a hit.
    let ack = h
        .dispatch(UiAction::SelectTab { slot_id: 4242 })
        .await
        .expect("host alive");
    assert!(
        matches!(
            &ack,
            ActionAck::Stale {
                reason: StaleAction::Generation
            }
        ),
        "{ack:?}"
    );
}
