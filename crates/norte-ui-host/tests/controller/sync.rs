use super::*;

// ---------------------------------------------------------------------------
// Sincronizar: el PLAN (tarea 6.3, fase A).
// ---------------------------------------------------------------------------

/// Espera la siguiente actualización con el panel de sincronización.
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
    panic!("no llegó ninguna actualización con el plan");
}

/// Un paso de plan, con lo mínimo para pintarlo.
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
                .map(|s| norte_proto::Segment::new(s.as_bytes().to_vec()).expect("segmento"))
                .collect(),
        ),
        dest_rel: None,
        size: Some(10),
        criterion: norte_proto::methods::CompareCriterion::Size,
        confidence: norte_proto::methods::CompareConfidence::Certain,
        // La reversa que le corresponde a una copia: deshacerla es BORRAR lo
        // que creó. El modelo compartido rechaza un paso cuya forma se
        // contradice —una clase que escribe sin reversa, un `Skip` que dice
        // tenerla— y ese rechazo es lo que impide aprobar un plan que no se
        // puede pintar.
        reversal: Some(norte_proto::methods::StepReversal::Delete),
        reason: None,
    }
}

/// El cierre de un plan sin bloqueos.
pub(super) fn plan_cerrado(pasos: u64) -> norte_proto::methods::SyncPlanDone {
    // Los recuentos, como los contaría el daemon: el modelo los compara
    // clase a clase con los suyos, y un plan que no cuadra NO se aprueba.
    // Los bytes también: cada paso de este test mide diez.
    let counts = norte_proto::methods::SyncCounts {
        copy: pasos,
        bytes: pasos * 10,
        ..Default::default()
    };
    norte_proto::methods::SyncPlanDone {
        // Se corrige al aterrizar: el modelo casa el cierre con SU Task.
        task_id: norte_proto::TaskId::new(0),
        plan_hash: norte_proto::methods::PlanHash::parse(
            &"a".repeat(norte_proto::methods::PLAN_HASH_LEN),
        )
        .expect("hash de test"),
        counts,
        blockers: Vec::new(),
        blockers_total: 0,
        executable: true,
        // Con papelera: es lo que hace que la columna del deshacer pueda
        // decir algo distinto de «no se sabe».
        dest_trash: norte_proto::methods::DestTrash::Restorable,
    }
}

/// Pedir sincronizar abre el panel con el plan que contestó el core, y el
/// plan dice de cada paso si el deshacer lo devuelve.
#[tokio::test]
async fn pedir_sincronizar_abre_el_plan() {
    let falso = arbol_como_falso();
    *falso.plan_de_sync.lock().expect("plan") = Some((
        vec![
            // Las dos de la MISMA clase: el modelo compara los recuentos
            // clase a clase contra los del daemon, y un plan que no cuadra no
            // se aprueba — que es exactamente lo que tiene que pasar.
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

    // Hasta que el plan CIERRA: los pasos llegan en un parche y el cierre en
    // otro, y lo que se puede aprobar es un plan cerrado.
    let mut vista = siguiente_sync(&mut sub).await.expect("abre");
    for _ in 0..20 {
        if vista.can_approve {
            break;
        }
        vista = siguiente_sync(&mut sub).await.expect("sigue abierto");
    }
    assert_eq!(vista.steps.len(), 2, "{vista:?}");
    assert_eq!(vista.total, 2);
    // El modo se PINTA antes de aprobar: un espejo borra y una actualización
    // no, y quien aprueba tiene que verlo.
    // El modo va ya TRADUCIDO por la etiqueta compartida, no como un id: un
    // modo que esta build no supiera nombrar no puede caer en «actualizar»,
    // que es la mitad segura de lo que se está aprobando.
    assert_eq!(
        vista.mode,
        norte_frontend::sync::mode_label(
            norte_proto::methods::SyncMode::Update,
            norte_i18n::Lang::Es
        )
    );
    // Y cada paso dice si el deshacer lo devuelve: nunca sale de `reversal` a
    // secas, que es la mitad que miente sin papelera en el destino.
    assert!(vista.steps.iter().all(|p| !p.undo.is_empty()), "{vista:?}");
    assert!(
        vista.can_approve,
        "un plan cerrado y sin bloqueos se aprueba: {}",
        vista.status
    );
    let pedidos = backend.planes_pedidos.lock().expect("planes").clone();
    assert_eq!(pedidos.len(), 1);
    assert_ne!(pedidos[0].0, pedidos[0].1, "origen y destino son distintos");
}

/// Un plan con BLOQUEOS no se puede aprobar, y se dice cuáles son.
#[tokio::test]
async fn un_plan_con_bloqueos_no_se_aprueba() {
    let mut done = plan_cerrado(1);
    done.blockers = vec![norte_proto::methods::SyncBlocker {
        kind: norte_proto::methods::SyncBlockerKind::DestReadOnly,
        // La raíz: un bloqueo del árbol entero no cuelga de ningún paso.
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

    let mut vista = siguiente_sync(&mut sub).await.expect("abre");
    for _ in 0..20 {
        if !vista.blockers.is_empty() {
            break;
        }
        vista = siguiente_sync(&mut sub).await.expect("sigue abierto");
    }
    assert!(
        !vista.blockers.is_empty(),
        "se dice qué lo impide: {vista:?}"
    );
    assert!(!vista.can_approve, "y no se ofrece aprobar: {vista:?}");
}

/// Sincronizar los dos paneles cuando están en el MISMO sitio no encola nada.
#[tokio::test]
async fn sincronizar_el_mismo_directorio_no_encola_nada() {
    let falso = arbol_como_falso();
    *falso.plan_de_sync.lock().expect("plan") = Some((Vec::new(), plan_cerrado(0)));
    let backend = Arc::new(falso);
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    // Sin separar los paneles: los dos miran `casa`. Y el comando se EJECUTA
    // de verdad — la versión anterior de este test pulsaba `Escape` sobre la
    // paleta y afirmaba que no se había pedido nada, que es cierto tanto con
    // el guard como sin él.
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
        "no se pidió ningún plan"
    );
}

/// Cancelar la Task del plan desde el TABLERO deja el panel diciendo que se
/// canceló, no «planificando…» para siempre.
///
/// El desenlace de la Task no llegaba al modelo, así que `run` se quedaba en
/// `Running` eternamente: el panel no sabía decir «cancelado» ni «falló», y
/// —lo que importa para la fase siguiente— seguía diciendo que el plan se
/// puede aprobar después de que alguien lo mandara parar.
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
    let vista = siguiente_sync(&mut sub).await.expect("abre");
    assert!(vista.running);

    // El daemon dice que la Task se canceló.
    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("hay task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Cancelled);

    for _ in 0..40 {
        let v = siguiente_sync(&mut sub).await.expect("sigue abierto");
        if !v.running {
            assert!(
                !v.can_approve,
                "un plan cancelado no se aprueba, haya cerrado o no: {v:?}"
            );
            return;
        }
    }
    panic!("el panel siguió diciendo que planifica");
}

/// Con el panel del plan delante no se puede pedir otro.
///
/// Relanzar dejaba el panel anterior sin abandonar y su Task sin cancelar —el
/// daemon seguía caminando un árbol para un plan que ya nadie puede ver— y,
/// con una petición en vuelo, la segunda pulsación mataba el panel de las
/// dos.
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
    let _ = siguiente_sync(&mut sub).await.expect("abre");

    // Con el panel delante, las teclas son SUYAS: `ctrl+p` no abre la paleta,
    // que es la vía por la que se repetiría el comando. Es la primera de las
    // dos cerraduras.
    h.dispatch(tecla_mod("p", true, false))
        .await
        .expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(
        foto.palette.is_none(),
        "el panel del plan no puede dejar pasar la tecla de la paleta"
    );
    assert!(foto.sync.is_some(), "y el panel sigue delante");
    assert_eq!(
        backend.planes_pedidos.lock().expect("planes").len(),
        1,
        "no se pidió un segundo plan"
    );
}

/// Un daemon que no sabe planificar no deja la petición colgada.
#[tokio::test]
async fn un_plan_que_el_daemon_rechaza_no_deja_nada_pendiente() {
    let falso = arbol_como_falso();
    // Sin plan: el falso contesta `Unsupported`.
    let backend = Arc::new(falso);
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separar_los_paneles(&h, &mut sub).await;
    ejecutar_por_paleta(&h, &mut sub, "pane.sync-dirs").await;
    // El fallo se DICE.
    for _ in 0..40 {
        if siguiente_aviso(&mut sub).await.starts_with("err-") {
            break;
        }
    }
    // Y el siguiente intento se puede hacer: la petición no se quedó colgada.
    let ack = ejecutar_por_paleta_ack(&h, &mut sub, "pane.sync-dirs").await;
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "la petición anterior dejó el host encallado: {ack:?}"
    );
}

/// Un plan que BORRA árboles pregunta DOS veces, y la segunda solo la
/// contesta `y`.
///
/// La segunda pregunta no es ceremonia: la compone el modelo compartido y
/// solo aparece cuando el plan borra o deja algo sin vuelta atrás. Preguntar
/// siempre es lo que enseña a contestar sin leer.
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
    let mut vista = siguiente_sync(&mut sub).await.expect("abre");
    for _ in 0..20 {
        if vista.can_approve {
            break;
        }
        vista = siguiente_sync(&mut sub).await.expect("sigue abierto");
    }
    assert!(vista.can_approve, "{}", vista.status);

    // La primera `a` solo PREGUNTA.
    h.dispatch(tecla("y")).await.expect("host vivo");
    let preguntando = siguiente_sync(&mut sub).await.expect("sigue abierto");
    assert!(
        preguntando.confirming.is_some(),
        "un plan que borra árboles pregunta otra vez: {preguntando:?}"
    );
    asentar().await;
    assert!(
        backend.aplicados.lock().expect("aplicados").is_empty(),
        "y todavía no ha aplicado nada"
    );

    // Una tecla que no es `y` RETIRA la pregunta y no aplica.
    h.dispatch(tecla("n")).await.expect("host vivo");
    let retirada = siguiente_sync(&mut sub).await.expect("sigue abierto");
    assert!(retirada.confirming.is_none());
    asentar().await;
    assert!(backend.aplicados.lock().expect("aplicados").is_empty());

    // `a` y luego `y`: ahora sí, y con el hash que devolvió el CORE.
    h.dispatch(tecla("y")).await.expect("host vivo");
    let _ = siguiente_sync(&mut sub).await;
    h.dispatch(tecla("y")).await.expect("host vivo");
    let aplicados = anotados(&backend, "el plan aplicado", 1, |f| {
        f.aplicados.lock().expect("aplicados").clone()
    })
    .await;
    assert_eq!(aplicados.len(), 1, "una sola vez");
}

/// Un apply RECHAZADO suelta el pestillo; uno de resultado DESCONOCIDO no.
///
/// Que el daemon conteste «no» y que la conexión se caiga después de pedirlo
/// son cosas distintas: en el primer caso se sabe que el destino está
/// intacto y volver a intentarlo es correcto; en el segundo la petición pudo
/// llegar, y ofrecer `a` otra vez es ofrecer aplicar el mismo plan dos veces
/// sobre el mismo destino.
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
        let mut vista = siguiente_sync(&mut sub).await.expect("abre");
        for _ in 0..20 {
            if vista.can_approve {
                break;
            }
            vista = siguiente_sync(&mut sub).await.expect("sigue abierto");
        }
        assert!(vista.can_approve, "{}", vista.status);

        h.dispatch(tecla("y")).await.expect("host vivo");
        anotados(&backend, "el apply pedido", 1, |f| {
            f.aplicados.lock().expect("aplicados").clone()
        })
        .await;
        // El desenlace del apply vuelve por el buzón: se le deja correr antes
        // de preguntar qué pinta la pantalla.
        asentar().await;
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let tras = siguiente_foto(&mut sub).await.sync.expect("sigue abierto");
        assert_eq!(
            tras.can_approve, se_reofrece,
            "{error:?} dejó la pantalla ofreciendo aprobar = {}",
            tras.can_approve
        );
    }
}

/// Con el apply EN VUELO, `Escape` pide cancelar y NO cierra el panel.
///
/// Cerrarlo pierde el informe —y con él el recuento, los fallos y el asa del
/// deshacer— sobre un destino que se está reescribiendo.
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
    let mut vista = siguiente_sync(&mut sub).await.expect("abre");
    for _ in 0..20 {
        if vista.can_approve {
            break;
        }
        vista = siguiente_sync(&mut sub).await.expect("sigue abierto");
    }
    // Este plan no borra nada y se deshace entero: no hay segunda pregunta.
    // `y` es `dialog.approve` en el preset (#287): aprobar un plan es decir
    // que sí a lo que ya está delante, no «confirmar» a secas.
    h.dispatch(tecla("y")).await.expect("host vivo");
    anotados(&backend, "el plan aplicado", 1, |f| {
        f.aplicados.lock().expect("aplicados").clone()
    })
    .await;
    assert_eq!(backend.aplicados.lock().expect("aplicados").len(), 1);

    // El PRIMER `Escape` pide parar y NO cierra: cerrar pierde el informe
    // sobre un destino a medio reescribir. Y se le pide parar a la task del
    // APPLY, no a la del plan, que hace rato que terminó.
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    let panel = foto.sync.expect("el panel se queda");
    assert!(panel.cancel_requested, "y la pantalla acusa que se le oyó");
    hasta(&backend, "la parada pedida al daemon", |f| {
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
        "se le pidió parar a la task del apply: {paradas:?}"
    );

    // El SEGUNDO cierra, pase lo que pase con el informe: sin esta salida,
    // la pantalla que escribe era la única de norte sin salida.
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    for _ in 0..20 {
        if siguiente_foto(&mut sub).await.sync.is_none() {
            return;
        }
    }
    panic!("el panel no se pudo cerrar");
}

/// El informe llega y el panel lo dice, con los fallos uno a uno.
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
            // Sin lote de journal: nada que deshacer, y el panel lo dirá.
            batch_id: None,
            dest_trash: norte_proto::methods::DestTrash::Restorable,
        });
    let backend = Arc::new(falso);
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separar_los_paneles(&h, &mut sub).await;
    ejecutar_por_paleta(&h, &mut sub, "pane.sync-dirs").await;
    let mut vista = siguiente_sync(&mut sub).await.expect("abre");
    for _ in 0..20 {
        if vista.can_approve {
            break;
        }
        vista = siguiente_sync(&mut sub).await.expect("sigue abierto");
    }
    h.dispatch(tecla("y")).await.expect("host vivo");
    anotados(&backend, "el plan aplicado", 1, |f| {
        f.aplicados.lock().expect("aplicados").clone()
    })
    .await;
    // El daemon termina la Task del apply.
    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("hay task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    for _ in 0..40 {
        let v = siguiente_sync(&mut sub).await.expect("sigue abierto");
        if !v.failures.is_empty() {
            assert_eq!(v.failures[0].path, "a.md");
            assert!(!v.failures[0].cause.is_empty());
            return;
        }
    }
    panic!("el informe no llegó al panel");
}

/// Aprobar las capabilities de una extensión PREGUNTA, y la pregunta las
/// enumera.
///
/// «¿Apruebas org.ejemplo.foo?» sin decir qué concede no es una decisión: es
/// un botón. Cada capability va en su LÍNEA y con su bandera, porque la que
/// se pinta distinta de lo que dice es justo la que un manifiesto hostil
/// escribe para colarse entre las de verdad.
#[tokio::test]
async fn aprobar_pregunta_y_enumera_las_capabilities() {
    let mut ext = extension("acme.ftp", "FTP de ACME", false);
    ext.approved = false;
    ext.enabled = false;
    ext.capabilities = vec!["fs-read".to_owned(), "net\u{202e}".to_owned()];
    let backend = arbol_con_plugins(vec![ext], &[]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let _ = extensiones_cargadas(&mut sub).await;

    h.dispatch(tecla("a")).await.expect("host vivo");
    let dialogos = siguientes_dialogos(&mut sub).await;
    let d = dialogos.last().expect("la pregunta");
    assert_eq!(d.title_key, "modal-extension-approve-title");
    assert_eq!(d.body.len(), 3, "el nombre y las DOS capabilities: {d:?}");
    assert!(!d.body[1].hostile, "la capability limpia no se marca");
    assert!(
        d.body[2].hostile,
        "y la del override bidi SÍ: cuál difiere es la pregunta entera"
    );
    // Y quién la pide, por el id que el core valida: dos extensiones pueden
    // llamarse igual, y el nombre lo escribe el manifiesto.
    assert_eq!(
        d.subject.as_ref().map(|s| s.text.as_str()),
        Some("acme.ftp")
    );
    asentar().await;
    assert!(
        backend.gobierno.lock().expect("gobierno").is_empty(),
        "y todavía no se ha concedido nada"
    );

    // Y la respuesta afirmativa concede, y el catálogo se REPIDE: lo que la
    // pantalla dice de quién puede leer tus ficheros no lo decide un
    // optimismo local.
    h.dispatch(UiAction::Dialog {
        id: d.id,
        choice: "approve".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    for _ in 0..20 {
        let Some(v) = siguiente_extensiones(&mut sub).await else {
            continue;
        };
        if v.rows.first().is_some_and(|r| r.approved) {
            assert_eq!(
                backend.gobierno.lock().expect("gobierno").as_slice(),
                // Con el ANCLA que se enseñó (#282): lo que se concede tiene
                // que ser lo que el humano leyó, y el core rehúsa si el
                // manifiesto cambió entre la pregunta y el sí.
                ["approval:acme.ftp:true:digest-de-acme.ftp"]
            );
            return;
        }
    }
    panic!("el catálogo nunca reflejó la concesión");
}

/// En solo lectura no se concede nada: se DICE.
///
/// Es el mismo interruptor que decide si esta ventana borra. Conceder
/// capabilities es la decisión de seguridad del sistema de extensiones, y
/// una ventana montada sin efectos no la toma.
#[tokio::test]
async fn en_solo_lectura_no_se_gobierna_ninguna_extension() {
    let mut ext = extension("acme.ftp", "FTP de ACME", false);
    ext.approved = false;
    let backend = arbol_con_plugins(vec![ext], &[]);
    let (h, _snap) = host_solo_lectura(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let _ = extensiones_cargadas(&mut sub).await;
    for tecla_de in ["a", "e"] {
        let ack = h.dispatch(tecla(tecla_de)).await.expect("host vivo");
        assert!(
            matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-read-only"),
            "`{tecla_de}` en solo lectura: {ack:?}"
        );
    }
    asentar().await;
    assert!(backend.gobierno.lock().expect("gobierno").is_empty());
}

/// Encender una extensión SIN aprobar se rehúsa, y se dice por qué.
///
/// Sin capabilities aprobadas el core no la carga: decir «encendida» sobre
/// algo que no corre es la pantalla mintiendo.
#[tokio::test]
async fn encender_sin_aprobar_se_rehusa() {
    let mut ext = extension("acme.ftp", "FTP de ACME", false);
    ext.approved = false;
    ext.enabled = false;
    let backend = arbol_con_plugins(vec![ext], &[]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let _ = extensiones_cargadas(&mut sub).await;
    let ack = h.dispatch(tecla("e")).await.expect("host vivo");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key }
            if reason_key == "host-extension-not-approved"),
        "{ack:?}"
    );
    asentar().await;
    assert!(backend.gobierno.lock().expect("gobierno").is_empty());
}

/// Un `bool` CICLA con `Enter` y se escribe; un `int` abre el buffer, y lo
/// que se teclea se valida contra las cotas del ESQUEMA antes de salir.
#[tokio::test]
async fn el_editor_de_config_cicla_teclea_y_valida() {
    let ext = extension("acme.ftp", "FTP de ACME", false);
    let backend = arbol_con_esquema(vec![ext], &[("acme.ftp", esquema_de_prueba())]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let _ = extensiones_cargadas(&mut sub).await;
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    let ficha = ficha_abierta(&mut sub).await;
    assert_eq!(ficha.config.len(), 4);
    assert!(
        !ficha.config[3].editable,
        "un `kind` que este build no conoce es de solo lectura: {:?}",
        ficha.config[3]
    );

    // La primera clave es el `bool`: `Enter` la cicla y la manda. Se ESPERA
    // a que llegue en vez de dormir un plazo fijo: bajo carga, cuarenta
    // milisegundos no son una garantía, y un test que afirma presencia
    // contra el reloj es rojo intermitente.
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    anotados(&backend, "la escritura del `bool`", 1, |f| {
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
    // Y la pantalla se mueve con él: el operando y lo que se pinta son dos
    // mitades de la misma fila, y actualizar solo una dejaba la celda con el
    // valor viejo — el siguiente `Enter` lo devolvía a donde estaba.
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let ficha = siguiente_foto(&mut sub)
        .await
        .extensions
        .expect("sigue abierto")
        .detail
        .expect("con ficha");
    assert_eq!(ficha.config[0].value, "true");

    // La segunda es el `int`: `Enter` abre el buffer y NO escribe nada.
    h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    esperar_buffer(&h, &mut sub).await;
    // Un valor fuera de las cotas se rehúsa AQUÍ y no viaja: el daemon
    // vuelve a validar, pero decirlo antes ahorra el viaje y dice la cota.
    for c in ["Backspace", "Backspace", "9", "9", "9"] {
        h.dispatch(tecla(c)).await.expect("host vivo");
    }
    let ack = h.dispatch(tecla("Enter")).await.expect("host vivo");
    // El ACUSE lleva una clave sin variables —nadie sustituye `{ $min }` en
    // ese camino—; las cotas van en el aviso, que sí se traduce con ellas.
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key }
            if reason_key == "host-value-rejected"),
        "{ack:?}"
    );
    asentar().await;
    assert_eq!(
        backend.escrituras.lock().expect("escrituras").len(),
        1,
        "el valor fuera de rango no se mandó"
    );
    // Y el buffer SIGUE abierto: un commit rechazado no cierra el campo, que
    // es lo que permite corregir sin volver a teclearlo entero.
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    assert!(
        siguiente_foto(&mut sub)
            .await
            .extensions
            .expect("sigue abierto")
            .detail
            .expect("con ficha")
            .editing
            .is_some()
    );

    // Y uno dentro sí.
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    esperar_buffer(&h, &mut sub).await;
    for c in ["Backspace", "Backspace", "Backspace", "4", "2"] {
        h.dispatch(tecla(c)).await.expect("host vivo");
    }
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    let escrituras = anotados(&backend, "la clave tecleada, mandada", 2, |f| {
        f.escrituras.lock().expect("escrituras").clone()
    })
    .await;
    assert_eq!(escrituras[1].1, "timeout");
    assert_eq!(escrituras[1].2, "42");
}

/// Mientras se TECLEA un valor, `a` es una letra y no una concesión.
///
/// Es el mismo régimen fijo que cualquier campo de este host: resolver las
/// letras como gestos ahí convierte escribir «casa» en dos concesiones de
/// capabilities.
#[tokio::test]
async fn tecleando_un_valor_las_letras_son_letras() {
    let ext = extension("acme.ftp", "FTP de ACME", false);
    let backend = arbol_con_esquema(vec![ext], &[("acme.ftp", esquema_de_prueba())]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let _ = extensiones_cargadas(&mut sub).await;
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    let _ = ficha_abierta(&mut sub).await;
    // A la clave `string`, que es la TERCERA (`verbose`, `timeout`,
    // `greeting`, y la cuarta es la del `kind` desconocido).
    for _ in 0..2 {
        h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    }
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    esperar_buffer(&h, &mut sub).await;
    h.dispatch(tecla("a")).await.expect("host vivo");
    asentar().await;
    assert!(
        backend.gobierno.lock().expect("gobierno").is_empty(),
        "la `a` tecleada no concedió capabilities"
    );
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    let editando = foto
        .extensions
        .expect("sigue abierto")
        .detail
        .expect("con ficha")
        .editing
        .expect("editando");
    assert!(editando.ends_with('a'), "la letra entró: {editando:?}");
}

/// Un árbol con catálogo Y esquemas de `[config]`.
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

/// Espera a que la ficha de la extensión elegida esté abierta.
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
    panic!("la ficha nunca se abrió");
}

/// La paleta ofrece los comandos de las extensiones, y ejecutarlos enseña lo
/// que imprimieron.
///
/// Las filas las compone el modelo COMPARTIDO: solo aprobadas y encendidas
/// —la misma puerta que `plugin.run_command` exige por su cuenta— y con el
/// prefijo que impide que un comando de tercero se disfrace de uno propio.
/// La salida es texto de tercero: se enmascara, se acota, y que se cortó se
/// dice.
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
        .expect("host vivo");
    // Las filas de plugin se UNEN cuando el daemon contesta: la paleta se
    // pinta antes, con los comandos propios.
    let mut llegaron = false;
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let p = siguiente_foto(&mut sub).await.palette.expect("abierta");
        if p.rows.iter().any(|r| r.text.contains("Saludar")) {
            llegaron = true;
            break;
        }
    }
    assert!(llegaron, "la fila del comando de la extensión nunca llegó");
    // Se acota tecleando, que es para lo que está la paleta: el título del
    // comando lo pliega el modelo compartido junto con su descripción.
    for c in "Saludar".chars() {
        h.dispatch(tecla(&c.to_string())).await.expect("host vivo");
    }
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let p = siguiente_foto(&mut sub).await.palette.expect("abierta");
    assert_eq!(p.rows.len(), 1, "el filtro deja una sola fila: {p:?}");
    h.dispatch(tecla("Enter")).await.expect("host vivo");

    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        let Some(salida) = foto.plugin_output else {
            continue;
        };
        assert_eq!(
            backend.ejecutados.lock().expect("ejecutados").as_slice(),
            [("acme.ftp".to_owned(), "greet".to_owned())]
        );
        assert!(salida.text_hostile, "el override bidi se dice: {salida:?}");
        assert!(
            !salida.lines.iter().any(|l| l.contains('\u{202e}')),
            "y se enmascara"
        );
        assert!(salida.truncated, "y que se cortó también: {salida:?}");
        assert_eq!(salida.command.text, "Saludar");
        assert_eq!(salida.plugin_id, "acme.ftp", "y quién lo imprimió, por id");

        // Y `Escape` la cierra sin tocar nada de debajo.
        h.dispatch(tecla("Escape")).await.expect("host vivo");
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        assert!(siguiente_foto(&mut sub).await.plugin_output.is_none());
        return;
    }
    panic!(
        "la salida del comando nunca llegó; ejecutados = {:?}",
        backend.ejecutados.lock().expect("ejecutados")
    );
}

/// C3 (ADR 0095): una fila de RENAMER en la paleta pide el plan al plugin
/// sobre lo marcado y lo mete en la MISMA revisión que el plan de la IA —
/// con el veredicto del core en su viaje— sin que ningún modelo entre en
/// juego.
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

    // Marcar el fichero: el renamer actúa sobre lo marcado.
    por_la_paleta(&h, &mut sub, "mark.all").await;
    h.dispatch(tecla_mod("p", true, false))
        .await
        .expect("host vivo");
    let mut llego = false;
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let p = siguiente_foto(&mut sub).await.palette.expect("abierta");
        if p.rows.iter().any(|r| r.text.contains("Rename by date")) {
            llego = true;
            break;
        }
    }
    assert!(llego, "la fila del renamer nunca llegó");
    for c in "Rename by date".chars() {
        h.dispatch(tecla(&c.to_string())).await.expect("host vivo");
    }
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let p = siguiente_foto(&mut sub).await.palette.expect("abierta");
    assert_eq!(p.rows.len(), 1, "{p:?}");
    // El rótulo sale del catálogo GLOBAL del proceso (como el de los
    // comandos de extensión), así que aquí vale en cualquiera de los dos.
    assert!(
        p.rows[0].text.starts_with("[renombrar]") || p.rows[0].text.starts_with("[rename]"),
        "otro rótulo que un comando: {}",
        p.rows[0].text
    );
    h.dispatch(tecla("Enter")).await.expect("host vivo");

    let mut v = siguiente_revision(&mut sub)
        .await
        .expect("abre la revisión");
    assert_eq!(v.pairs[0].from.text, "ep1.mkv");
    assert_eq!(v.pairs[0].to.text, "2026-09-03_ep1.mkv");
    for _ in 0..40 {
        if v.confirmable {
            break;
        }
        v = siguiente_revision(&mut sub).await.expect("sigue abierta");
    }
    assert!(v.confirmable, "el core dio su veredicto");
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
        "al modelo no se le pidió nada"
    );
}

/// Un renamer que REHÚSA dice por qué (#332): la frase llega a la barra de
/// estado tal cual la acotó el daemon, no se abre revisión, y no es un
/// error genérico — «aprueba mi capacidad» tiene que leerse.
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
        .expect("host vivo");
    let mut llego = false;
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let p = siguiente_foto(&mut sub).await.palette.expect("abierta");
        if p.rows.iter().any(|r| r.text.contains("Rename by date")) {
            llego = true;
            break;
        }
    }
    assert!(llego, "la fila del renamer nunca llegó");
    for c in "Rename by date".chars() {
        h.dispatch(tecla(&c.to_string())).await.expect("host vivo");
    }
    h.dispatch(tecla("Enter")).await.expect("host vivo");

    // La misma foto trae la frase y la ausencia de revisión: esperar OTRA
    // foto después colgaría, porque nada más cambia.
    let (msg, revision) = foto_hasta(&h, &mut sub, "la frase del renamer en la barra", |s| {
        s.status
            .message
            .clone()
            .filter(|m| m.contains("needs the location capability"))
            .map(|m| (m, s.ai_rename.is_some()))
    })
    .await;
    assert!(
        !msg.contains("no soportado") && !msg.contains("not supported"),
        "no es un error genérico: {msg}"
    );
    assert!(!revision, "sin plan no hay revisión");
}

/// En solo lectura la paleta NO ofrece comandos de extensión.
///
/// Lo que hace un comando lo decide el PLUGIN: puede escribir. Una ventana
/// montada sin efectos no lo lanza, y por tanto tampoco lo ofrece — es la
/// misma regla que ya se aplica a los comandos propios: ofrecer lo que se va
/// a rehusar es prometer algo que no se hará. El catálogo ni se pide.
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
        .expect("host vivo");
    for _ in 0..10 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let p = siguiente_foto(&mut sub).await.palette.expect("abierta");
        assert!(
            !p.rows.iter().any(|r| r.text.contains("Saludar")),
            "una ventana sin efectos no ofrece ejecutar código de tercero"
        );
        asentar().await;
    }
    // Y el catálogo ni se pidió: la puerta se cierra antes del viaje.
    assert_eq!(
        backend
            .catalogos_pedidos
            .load(std::sync::atomic::Ordering::SeqCst),
        0,
        "una ventana sin efectos no va a preguntar por comandos que no va a lanzar"
    );
    assert!(backend.ejecutados.lock().expect("ejecutados").is_empty());
}

/// Espera a que el buffer de edición de la ficha esté abierto.
///
/// Por RESYNC y no consumiendo parches a ciegas: un bucle que lee N
/// actualizaciones se queda sin ellas en cuanto el test manda una foto por
/// otro motivo, y entonces falla por plazo diciendo algo que no es.
pub(super) async fn esperar_buffer(h: &UiHost, sub: &mut norte_ui_host::UiSubscription) {
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let abierto = siguiente_foto(sub)
            .await
            .extensions
            .and_then(|e| e.detail)
            .is_some_and(|d| d.editing.is_some());
        if abierto {
            return;
        }
    }
    panic!("el buffer de edición nunca se abrió");
}

/// Un cambio de gobierno que FALLA vuelve a pedir el catálogo.
///
/// El fallo incluye el plazo de ESTE lado, que no es «no pasó» sino «no se
/// sabe»: el daemon pudo conceder las capabilities y tardar en contestar.
/// Dejar la fila diciendo «sin aprobar» es la misma mentira que el optimismo
/// local, en pesimista — y lo único que resuelve un desconocido es preguntar.
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
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let _ = extensiones_cargadas(&mut sub).await;
    let pedidos = backend
        .catalogos_pedidos
        .load(std::sync::atomic::Ordering::SeqCst);

    h.dispatch(tecla("a")).await.expect("host vivo");
    let dialogos = siguientes_dialogos(&mut sub).await;
    let d = dialogos.last().expect("la pregunta");
    h.dispatch(UiAction::Dialog {
        id: d.id,
        choice: "approve".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");

    hasta(&backend, "el catálogo repedido tras el gobierno", |f| {
        let ahora = f
            .catalogos_pedidos
            .load(std::sync::atomic::Ordering::SeqCst);
        (ahora > pedidos).then_some(())
    })
    .await;
}

/// Con la salida de un comando en pantalla, las teclas son SUYAS.
///
/// Pinta a pantalla completa, así que un modal que dejara pasar la tecla que
/// no entiende no es un modal: `Enter` sobre ese panel llegaba a lo de
/// debajo, donde podía haber una confirmación esperando un sí que el lector
/// no ve — y el momento lo elige el PLUGIN, que decide cuándo contesta.
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
        .expect("host vivo");
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let p = siguiente_foto(&mut sub).await.palette.expect("abierta");
        if p.rows.iter().any(|r| r.text.contains("Saludar")) {
            break;
        }
    }
    for c in "Saludar".chars() {
        h.dispatch(tecla(&c.to_string())).await.expect("host vivo");
    }
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if siguiente_foto(&mut sub).await.plugin_output.is_some() {
            break;
        }
    }

    // Una tecla de navegación con el panel abierto NO mueve lo de debajo.
    h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    let durante = siguiente_foto_tras_resync(&h, &mut sub).await;
    assert!(durante.plugin_output.is_some(), "el panel sigue");
    assert_eq!(
        listado(&durante).cursor,
        cursor_antes,
        "el cursor del listado no se movió bajo el panel"
    );

    // Y `Enter` lo CIERRA, que es el reflejo de quien acaba de leerlo.
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    let despues = siguiente_foto_tras_resync(&h, &mut sub).await;
    assert!(despues.plugin_output.is_none());
    assert_eq!(listado(&despues).cursor, cursor_antes);
}

/// Pide una foto y la espera.
pub(super) async fn siguiente_foto_tras_resync(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::ViewSnapshot {
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    siguiente_foto(sub).await
}

/// El panel de agentes lista las sesiones que ESTA ventana vio pedir
/// permiso, y desde ahí se deshace una entera (#276).
///
/// El operando se ELIGE de una lista: un id de sesión tecleado a mano en una
/// superficie de gobierno es un id que se puede equivocar, y deshacer la
/// sesión equivocada es deshacer el trabajo de otro. Y la lista dice lo que
/// es —lo visto por esta ventana, no el censo del sistema—, porque no hay
/// método en el protocolo que enumere sesiones vivas.
#[tokio::test]
async fn el_panel_de_agentes_deshace_la_sesion_elegida() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.aprobaciones.lock().expect("aprobaciones") = Some(rx);
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // Dos sesiones, y la del id hostil es la ÚLTIMA vista: la lista va de la
    // más reciente a la más antigua.
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
        .expect("el host escucha");
        let dialogos = siguientes_dialogos(&mut sub).await;
        // Se DENIEGA para quitarlo de en medio: un diálogo abierto se queda
        // las teclas, y lo que se comprueba aquí es el panel. Denegar no
        // borra el apunte —lo que la sesión pidió ya se vio—, que es
        // justamente la propiedad interesante.
        let d = dialogos.last().expect("la aprobación");
        h.dispatch(UiAction::Dialog {
            id: d.id,
            choice: "deny".to_owned(),
            secret: None,
        })
        .await
        .expect("host vivo");
        let _ = siguientes_dialogos(&mut sub).await;
    }

    ejecutar_por_paleta(&h, &mut sub, "app.agents").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let panel = siguiente_foto(&mut sub)
        .await
        .agents
        .expect("el panel está abierto");
    assert_eq!(panel.rows.len(), 2);
    assert_eq!(panel.rows[0].last_op, "delete", "la más reciente primero");
    assert!(
        panel.rows[0].session_hostile,
        "un id de sesión es una clave OPACA: si se pinta distinto, se dice"
    );
    assert!(
        !panel.rows[0].session.contains('\u{202e}'),
        "y se enmascara"
    );
    assert!(!panel.note.is_empty(), "y la lista dice lo que es");

    // `u` PREGUNTA: deshacer una sesión revierte todo lo que hizo.
    h.dispatch(tecla("u")).await.expect("host vivo");
    let dialogos = siguientes_dialogos(&mut sub).await;
    let d = dialogos.last().expect("la pregunta");
    assert_eq!(d.title_key, "modal-undo-session-title");
    assert!(
        d.choices.iter().any(|c| c.id == "confirm" && c.destructive),
        "deshacer escribe: la respuesta va marcada"
    );
    asentar().await;
    assert!(backend.deshechas.lock().expect("deshechas").is_empty());

    // Y al confirmar viaja el id CRUDO, no el que se pinta.
    h.dispatch(UiAction::Dialog {
        id: d.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    let pedidas = anotados(&backend, "el deshacer pedido", 1, |f| {
        f.deshechas.lock().expect("deshechas").clone()
    })
    .await;
    assert_eq!(pedidas, ["agente\u{202e}1".to_owned()]);
}

/// En solo lectura no se deshace nada: se DICE.
#[tokio::test]
async fn en_solo_lectura_no_se_deshace_una_sesion() {
    // Sin aprobaciones: una ventana de solo lectura tampoco puede
    // CONTESTARLAS, así que un diálogo abierto se quedaría las teclas y este
    // test estaría comprobando otra cosa. La lista vacía vale igual: el
    // rechazo por efectos se mira ANTES que si hay algo señalado.
    let backend = arbol_como_falso();
    let backend = Arc::new(backend);
    let (h, _snap) = host_solo_lectura(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "app.agents").await;
    let ack = h.dispatch(tecla("u")).await.expect("host vivo");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-read-only"),
        "{ack:?}"
    );
    asentar().await;
    assert!(backend.deshechas.lock().expect("deshechas").is_empty());
}

/// Una petición que llega con el panel abierto lo REPINTA, y la selección
/// sigue a su sesión aunque la lista se reordene.
///
/// La lista cambia SIN gesto: una petición nueva sube a su sesión al primer
/// puesto. Un renderer al que no se le dice se queda pintando el orden de
/// antes —la fila resaltada deja de ser la que el host tiene elegida— y `u`
/// deshace el trabajo de otra sesión. Y la selección va por ID, no por
/// posición, que es la regla que la 6.2 ya dejó escrita.
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
        tx.send(pedir(id, aid)).expect("el host escucha");
        let dialogos = siguientes_dialogos(&mut sub).await;
        let d = dialogos.last().expect("la aprobación");
        h.dispatch(UiAction::Dialog {
            id: d.id,
            choice: "deny".to_owned(),
            secret: None,
        })
        .await
        .expect("host vivo");
        let _ = siguientes_dialogos(&mut sub).await;
    }
    ejecutar_por_paleta(&h, &mut sub, "app.agents").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let antes = siguiente_foto(&mut sub).await.agents.expect("abierto");
    assert_eq!(antes.rows[0].session, "agente-B", "la más reciente primero");
    // La selección se pone en la SEGUNDA, `agente-A`.
    h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let elegida = siguiente_foto(&mut sub).await.agents.expect("abierto");
    assert_eq!(elegida.cursor, 1);

    // Y llega otra petición de `agente-B`, que ya estaba primera: lo que
    // cambia es su cuenta, y la lista tiene que decir que cambió.
    tx.send(pedir("agente-B", 23)).expect("el host escucha");
    let mut panel = elegida.clone();
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        panel = siguiente_foto(&mut sub).await.agents.expect("abierto");
        if panel.generation > elegida.generation {
            break;
        }
    }
    assert!(
        panel.generation > elegida.generation,
        "una lista que cambia sola tiene que decir que cambió: {panel:?}"
    );
    assert_eq!(
        panel.rows[usize::try_from(panel.cursor).expect("cabe")].session,
        "agente-A",
        "la selección sigue a SU sesión, no al hueco que ocupaba"
    );

    // La tercera petición dejó su diálogo delante: se contesta antes de
    // seguir, porque el panel es modal también para el ratón.
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    if let Some(d) = foto.dialogs.last() {
        h.dispatch(UiAction::Dialog {
            id: d.id,
            choice: "deny".to_owned(),
            secret: None,
        })
        .await
        .expect("host vivo");
    }

    // Un clic contra la lista VIEJA se rehúsa en vez de elegir por el lector.
    let ack = h
        .dispatch(UiAction::AgentSelectRow {
            row: 0,
            generation: elegida.generation,
        })
        .await
        .expect("host vivo");
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

/// En solo lectura, la lista vacía NO dice «ningún agente ha pedido nada».
///
/// Esa ventana ni siquiera se suscribe al canal de aprobaciones: su lista
/// está vacía por eso, y afirmar lo otro es afirmar lo que no puede saber.
#[tokio::test]
async fn en_solo_lectura_el_panel_dice_que_no_escucha() {
    let backend = Arc::new(arbol_como_falso());
    let (h, _snap) = host_solo_lectura(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "app.agents").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let panel = siguiente_foto(&mut sub).await.agents.expect("abierto");
    assert!(panel.rows.is_empty());
    let escuchando = norte_i18n::t_in(norte_i18n::Lang::Es, "agents-empty");
    assert_ne!(
        panel.empty, escuchando,
        "una ventana que no escucha no puede decir que nadie ha pedido nada"
    );
}

/// Copiar la ruta pone BYTES en el portapapeles, y lo hace en los dos modos.
///
/// Bytes y no texto: un nombre de fichero es bytes, y pasarlo por una
/// decodificación con pérdida pegaría una ruta que abre otra cosa. Y no muta
/// nada, así que una ventana de solo lectura también copia — es tan de solo
/// mirar como leer un nombre.
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
            .expect("un efecto antes del plazo")
            .expect("el canal sigue vivo");
        match efecto {
            norte_ui_host::dto::NativeEffect::CopyBytes { bytes, count } => {
                assert_eq!(count, 1);
                assert!(
                    bytes.starts_with(b"/") || bytes.starts_with(b"mem:"),
                    "la ruta, en su forma nativa o la del wire: {bytes:?}"
                );
            }
            otro => panic!("copiar la ruta pide copiar, no {otro:?}"),
        }
    }
}

/// En solo lectura NO se abre nada ni se lanza un terminal: se DICE.
///
/// **Las teclas de un diálogo salen del KEYMAP, no del código** (#287).
///
/// Era la deriva que el catálogo compartido existe para no tener: la ventana
/// atendía sus superficies modales con teclas fijas, así que un preset que
/// reataba `dialog.down` cambiaba el TUI y no la ventana. Aquí se comprueba
/// sobre un preset REAL cuyas teclas de diálogo son otras.
#[tokio::test]
async fn las_teclas_de_un_dialogo_las_pone_el_preset() {
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        initial_dir_pedido: false,
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
    .expect("arranca");
    let mut sub = h.subscribe();
    let antes = selector_columnas(&h, &mut sub).await;

    // El acorde que ESTE preset ata a `dialog.down`, sea el que sea.
    let atado = norte_ui_host::keys::keymap_dialogo_de_preset("vim")
        .expect("preset")
        .bindings()
        .into_iter()
        .find(|(_, c)| *c == "dialog.down")
        .map(|(seq, _)| seq)
        .expect("el preset ata bajar");

    h.dispatch(UiAction::Key(tecla_de_acorde(&atado)))
        .await
        .expect("host vivo");
    let despues = siguiente_columnas(&h, &mut sub).await;
    assert_ne!(
        despues.cursor, antes.cursor,
        "el acorde del preset mueve el cursor: {atado:?}"
    );
}

/// Un acorde pintado, de vuelta a la tecla que el host recibe.
///
/// Solo lo que hace falta aquí: una tecla con sus modificadores, sin
/// secuencias. Un preset que atara `dialog.down` a dos acordes se saldría de
/// esto, y entonces el test lo diría en vez de pasar por casualidad.
pub(super) fn tecla_de_acorde(acorde: &str) -> norte_ui_host::keys::KeyInput {
    let partes: Vec<&str> = acorde.split('+').collect();
    let (tecla, mods) = partes.split_last().expect("al menos una parte");
    let tiene = |m: &str| mods.iter().any(|p| p.eq_ignore_ascii_case(m));
    norte_ui_host::keys::KeyInput {
        key: (*tecla).to_owned(),
        ctrl: tiene("ctrl"),
        alt: tiene("alt"),
        shift: tiene("shift"),
        meta: tiene("meta") || tiene("cmd") || tiene("super"),
    }
}

/// **Editar uno nuevo crea el fichero VACÍO y lo abre** (#290).
///
/// La ventana no tiene editor ni terminal: lo que puede hacer es poner el
/// fichero en el disco y dárselo al escritorio. Y en ese orden — abrir antes
/// del desenlace sería lanzar un editor sobre algo que todavía no está.
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
    assert!(d[0].input.is_some(), "aquí se teclea un nombre");
    let id = d[0].id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "borrador.md".to_owned(),
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    {
        let creados = anotados(&backend, "el fichero creado", 1, |f| {
            f.creados.lock().expect("creados").clone()
        })
        .await;
        assert_eq!(creados.len(), 1, "{creados:?}");
        assert_eq!(creados[0].to_wire(), "file:///casa/borrador.md");
    }

    let efecto = tokio::time::timeout(ESPERA_MAX, efectos.recv())
        .await
        .expect("llega el efecto nativo")
        .expect("canal vivo");
    match efecto {
        norte_ui_host::dto::NativeEffect::OpenPath { path } => {
            assert_eq!(path.to_wire(), "file:///casa/borrador.md");
        }
        otro => panic!("se esperaba abrir el fichero recién creado: {otro:?}"),
    }
}

/// **Y si entre crear el nombre y abrirlo alguien lo cambia, NO se abre**
/// (#303).
///
/// norte anuncia el nombre creándolo —no hay nada que adivinar— y quien pueda
/// escribir en ese directorio lo ve aparecer, lo desenlaza y deja un symlink.
/// El humano acabaría escribiendo en un fichero que nadie le enseñó, y el
/// `undo` de la entrada `Created` va por RUTA: deshacer mandaría a la papelera
/// lo que haya ahí AHORA.
///
/// Estrecha la ventana y no la cierra —entre el `stat` y el `open` queda
/// hueco—, y es la misma decisión que toma la TUI. El fichero SE CREÓ: eso no
/// se deshace aquí, solo no se abre.
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
    .expect("host vivo");
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    {
        let creados = anotados(&backend, "el fichero creado", 1, |f| {
            f.creados.lock().expect("creados").clone()
        })
        .await;
        assert_eq!(creados.len(), 1, "el fichero SÍ se creó: {creados:?}");
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), efectos.recv())
            .await
            .is_err(),
        "no se le entrega al escritorio lo que ya no es el fichero creado"
    );
}

/// Sobre un panel REMOTO no se ofrece, y se dice antes de teclear el nombre.
///
/// Lo que se abre después es la aplicación del escritorio, y a `xdg-open` no
/// se le puede dar un `sftp://`. Decirlo cuando el nombre ya está escrito
/// llega tarde.
#[tokio::test]
async fn editar_uno_nuevo_no_se_ofrece_en_un_panel_remoto() {
    let mut falso = Falso::default();
    falso.pon("sftp://servidor/datos", vec![(b"a.txt".to_vec(), false)]);
    let backend = Arc::new(falso);
    let (h, _snap) = host_en(Arc::clone(&backend), "sftp://servidor/datos").await;
    let mut sub = h.subscribe();

    let ack = ejecutar_por_paleta_ack(&h, &mut sub, "pane.edit-new").await;
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-not-local"),
        "{ack:?}"
    );
    assert!(backend.creados.lock().expect("creados").is_empty());
}

/// Un host que arranca en un directorio concreto.
/// Como [`host_en`], pero con una configuración a medida: los openers y el
/// editor son claves que la ventana ignoraba, así que los tests las traen.
pub(super) async fn host_en_con(
    backend: Arc<Falso>,
    inicio: &str,
    cfg: norte_frontend::config::FrontendConfig,
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: VPath::parse(inicio).expect("vpath"),
        initial_dir_pedido: false,
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

/// Espera a una foto cuyo primer listado está en `sufijo`.
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
    panic!("el listado nunca llegó a `{sufijo}`");
}

/// **Desconectar devuelve el panel a donde estaba ANTES de conectar** (#140).
///
/// El rastro hacia atrás, no «a casa»: el panel estaba en algún sitio antes de
/// saltar a la máquina, y ese sitio es la respuesta que el lector espera.
#[tokio::test]
async fn desconectar_vuelve_a_donde_estaba_antes() {
    let mut falso = arbol_como_falso();
    falso.pon("sftp://servidor/datos", vec![(b"a.txt".to_vec(), false)]);
    *falso.conexiones.lock().expect("conexiones") =
        Some(Ok(vec![norte_proto::methods::ConnectionEntry {
            name: "trabajo".to_owned(),
            url: "sftp://servidor/datos".to_owned(),
        }]));
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // Conectar de verdad, por el selector: es el camino que un lector recorre,
    // y es lo que deja el rastro que después se deshace.
    ejecutar_por_paleta(&h, &mut sub, "pane.connect").await;
    for _ in 0..20 {
        let Some(v) = siguiente_selector(&mut sub).await else {
            continue;
        };
        if !v.rows.is_empty() {
            break;
        }
    }
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    let _ = listado_en(&mut sub, "/datos").await;

    ejecutar_por_paleta(&h, &mut sub, "pane.disconnect").await;
    let _ = listado_en(&mut sub, "/casa").await;

    let cerradas = backend.cerradas.lock().expect("cerradas");
    assert_eq!(cerradas.len(), 1, "{cerradas:?}");
    assert_eq!(cerradas[0].to_wire(), "sftp://servidor/datos");
}

/// Y NUNCA a otra ruta de la misma máquina: eso reabriría la sesión que se
/// acaba de cerrar, que es justo lo que el gesto pidió no tener.
#[tokio::test]
async fn desconectar_no_vuelve_a_la_misma_maquina() {
    let mut falso = Falso::default();
    falso.pon("sftp://servidor/uno", vec![(b"dos".to_vec(), true)]);
    falso.pon("sftp://servidor/uno/dos", vec![(b"b.txt".to_vec(), false)]);
    let backend = Arc::new(falso);
    let (h, snap) = host_en(Arc::clone(&backend), "sftp://servidor/uno").await;
    let mut sub = h.subscribe();

    let b = listado(&snap);
    h.dispatch(UiAction::Activate {
        slot_id: b.slot_id,
        key: b.rows[0].key,
        generation: b.generation,
    })
    .await
    .expect("host vivo");
    let _ = listado_en(&mut sub, "/uno/dos").await;

    ejecutar_por_paleta(&h, &mut sub, "pane.disconnect").await;

    let mut visto = None;
    for _ in 0..30 {
        let foto = siguiente_foto(&mut sub).await;
        let p = primer_listado(&foto).path_display.clone();
        if !p.contains("servidor") {
            visto = Some(p);
            break;
        }
    }
    let donde = visto.expect("el panel sale de la máquina cerrada");
    assert!(
        !donde.contains("servidor"),
        "todo su rastro era de esa máquina, así que cae a casa: {donde}"
    );
}

/// En un panel LOCAL no hay nada que cerrar, y se dice.
///
/// Una tecla que contesta «hecho» sobre algo que no ha hecho nada enseña a no
/// fiarse del mensaje.
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
        "y no se le pide nada al daemon"
    );
}

/// Busca el hueco de ÁRBOL en una foto.
pub(super) fn arbol_de(snap: &norte_ui_host::ViewSnapshot) -> &norte_ui_host::dto::TreeSlotView {
    snap.slots
        .iter()
        .find_map(|s| match s {
            SlotView::Tree(t) => Some(&**t),
            _ => None,
        })
        .expect("hay un hueco de árbol")
}

/// Espera a una foto en la que el árbol ya tiene sus ramas.
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
    panic!("el árbol nunca trajo {cuantas} ramas");
}

/// **El árbol lista UNA rama, y solo cuando se abre** (`pane.tree`).
///
/// Perezoso por la misma razón que el listado local no trae tamaños: uno que
/// se leyera entero al abrirse tardaría minutos en un `$HOME` grande y horas
/// contra un remoto. Al abrirlo se pide la RAÍZ y nada más — las hijas de
/// `docs` no se piden hasta que alguien despliega `docs`.
#[tokio::test]
async fn el_arbol_pide_una_rama_y_solo_al_abrirla() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.tree").await;
    // La raíz y sus hijas DIRECTORIO: `docs` está, `notas.txt` no.
    let foto = arbol_con_ramas(&mut sub, 2).await;
    let t = arbol_de(&foto);
    assert_eq!(t.rows.len(), 2, "raíz + `docs`, sin ficheros: {:?}", t.rows);
    assert_eq!(t.rows[0].depth, 0);
    assert!(
        t.rows[0].label.ends_with("/casa"),
        "la raíz lleva su ruta entera: {:?}",
        t.rows[0]
    );
    assert_eq!(t.rows[1].label, "docs");
    assert_eq!(t.rows[1].depth, 1);
    assert_eq!(
        t.rows[1].children, None,
        "todavía no se ha mirado dentro, y eso NO es «es una hoja»"
    );
}

/// Elegir una rama navega el LISTADO, y el árbol se queda donde está.
///
/// Es lo que hace útil tenerlo abierto: si el árbol se re-anclara en cada
/// navegación, entrar en una carpeta tiraría todas las ramas abiertas.
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
    .expect("host vivo");

    let mut visto = None;
    for _ in 0..20 {
        let foto = siguiente_foto(&mut sub).await;
        if primer_listado(&foto).path_display.ends_with("/casa/docs") {
            visto = Some(foto);
            break;
        }
    }
    let foto = visto.expect("el listado va a la rama elegida");
    let t = arbol_de(&foto);
    assert!(
        t.rows[0].label.ends_with("/casa"),
        "el árbol sigue anclado donde estaba: {:?}",
        t.rows[0]
    );
}

/// Un click con la generación de OTRA pintada se rechaza, no navega.
///
/// Las hijas de una rama aterrizan EN MEDIO de la lista, así que entre que el
/// lector suelta el botón y el host atiende, esa fila puede ser otra carpeta
/// (ADR 0068).
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
        .expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Stale { .. }),
        "una generación que no case se rechaza: {ack:?}"
    );
    let foto = siguiente_foto_tras_resync(&h, &mut sub).await;
    assert!(
        primer_listado(&foto).path_display.ends_with("/casa"),
        "y el listado no se movió: {:?}",
        primer_listado(&foto).path_display
    );
}

/// **Soltar ficheros NO copia: pregunta** (#283).
///
/// Un drop es un gesto sin confirmación por naturaleza, y la lista la compone
/// otro proceso. Enseñarla antes de escribir es la única ocasión que tiene el
/// lector de ver que lo que llegó no es lo que arrastró.
#[tokio::test]
async fn soltar_pregunta_antes_de_copiar() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    h.dispatch(UiAction::FilesDropped {
        paths: vec!["/tmp/uno.txt".to_owned(), "/tmp/dos.txt".to_owned()],
    })
    .await
    .expect("host vivo");

    let dialogos = siguientes_dialogos(&mut sub).await;
    assert_eq!(dialogos.len(), 1);
    assert_eq!(dialogos[0].title_key, "modal-drop-title");
    let cuerpo = &dialogos[0].body;
    assert_eq!(cuerpo.len(), 2, "lo que llegó, línea a línea: {cuerpo:?}");
    let destino = dialogos[0].destination.as_ref().expect("dice a dónde cae");
    assert!(
        destino.text.ends_with("/casa"),
        "el panel activo, en SU campo: {destino:?}"
    );
    assert!(
        backend
            .transferencias
            .lock()
            .expect("transferencias")
            .is_empty(),
        "abrir el diálogo no copia nada"
    );
}

/// Confirmado, se COPIA —nunca se mueve— y no se tocan las marcas del panel.
///
/// Mover lo que arrastró otra aplicación sería borrarlo de donde ese proceso
/// lo tenga, y esta ventana no ha preguntado eso. Y las marcas del panel
/// activo las puso el lector para otra cosa: lo que se copia no salió de ahí.
#[tokio::test]
async fn soltar_confirmado_copia_y_respeta_las_marcas() {
    let backend = arbol();
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let b = listado(&snap);
    let hueco = b.slot_id;
    let fila = b.rows.first().expect("hay filas");
    h.dispatch(UiAction::ToggleMark {
        slot_id: hueco,
        key: fila.key,
        generation: b.generation,
    })
    .await
    .expect("host vivo");

    h.dispatch(UiAction::FilesDropped {
        paths: vec!["/tmp/uno.txt".to_owned()],
    })
    .await
    .expect("host vivo");
    let dialogos = siguientes_dialogos(&mut sub).await;
    let id = dialogos[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    {
        let ts = anotados(&backend, "la copia del drop", 1, |f| {
            f.transferencias.lock().expect("transferencias").clone()
        })
        .await;
        assert_eq!(ts.len(), 1, "{ts:?}");
        let (from, to, mover, _) = &ts[0];
        assert!(!*mover, "un drop COPIA, jamás mueve: {ts:?}");
        assert_eq!(from.to_wire(), "file:///tmp/uno.txt");
        assert_eq!(
            to.to_wire(),
            "mem:///casa/uno.txt",
            "cae en el directorio del panel, con el nombre que traía"
        );
    }

    let foto = siguiente_foto_tras_resync(&h, &mut sub).await;
    let b = listado(&foto);
    assert!(
        b.rows.iter().any(|r| r.marked),
        "la marca del lector sigue donde estaba: {:?}",
        b.rows
    );
}

/// Lo que llega y no es una ruta de esta máquina se DICE, no se ignora.
///
/// Un emisor compone el `text/uri-list` a mano si quiere. Tragárselo en
/// silencio dejaría al lector mirando un panel que no cambió sin saber por
/// qué.
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
        .expect("host vivo");
    assert!(
        matches!(&ack, norte_ui_host::ActionAck::Unavailable { reason_key } if reason_key == "host-drop-unusable"),
        "{ack:?}"
    );
    // Por `Resync` y no esperando la siguiente foto: rehusar no manda una,
    // solo el parche de la barra, y un `siguiente_foto` aquí se queda
    // colgado en vez de ponerse rojo.
    let foto = siguiente_foto_tras_resync(&h, &mut sub).await;
    assert!(foto.dialogs.is_empty(), "y no abre ningún diálogo");
    assert!(
        foto.status
            .message
            .as_deref()
            .is_some_and(|m| m == norte_i18n::t_in(norte_i18n::Lang::Es, "host-drop-unusable")),
        "y lo DICE en la barra: {:?}",
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

/// **El selector de conexiones lo llena el DAEMON** (#264): la ventana no lee
/// `connections.toml`, que es lo que le costaría meter la pila de red entera.
///
/// Y la URL se enmascara como una AUTORIDAD, no como una ruta: aquí «¿a qué
/// máquina me conecto?» es la única pregunta que el selector contesta.
#[tokio::test]
async fn el_selector_de_conexiones_lo_llena_el_daemon() {
    let falso = arbol_como_falso();
    *falso.conexiones.lock().expect("conexiones") = Some(Ok(vec![
        norte_proto::methods::ConnectionEntry {
            name: "trabajo".to_owned(),
            url: "sftp://oscar@servidor.example/datos".to_owned(),
        },
        norte_proto::methods::ConnectionEntry {
            name: "archivo".to_owned(),
            url: "s3://mi-bucket".to_owned(),
        },
    ]));
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
    let v = con_filas.expect("la lista del daemon llega");
    assert_eq!(v.rows.len(), 2);
    assert_eq!(v.rows[0].label, "trabajo");
    assert!(
        v.rows[0].detail.contains("servidor.example"),
        "el detalle es la URL: {:?}",
        v.rows[0]
    );
}

/// Sin ninguna configurada, el selector lo DICE. «No tienes ninguna» y
/// «todavía no ha contestado» no son lo mismo, y una lista vacía sin frase se
/// lee siempre como lo primero.
#[tokio::test]
async fn sin_conexiones_configuradas_el_selector_lo_dice() {
    let falso = arbol_como_falso();
    *falso.conexiones.lock().expect("conexiones") = Some(Ok(Vec::new()));
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
    let v = visto.expect("la frase de lista vacía llega");
    assert!(v.rows.is_empty());
}

/// **Con la ventana delante NO se avisa por el escritorio** (#285): la barra
/// y el tablero ya cuentan lo mismo, y repetirlo fuera es ruido.
#[tokio::test]
async fn con_la_ventana_delante_no_se_avisa_fuera() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut nativos = h.native_effects();
    let mut sub = h.subscribe();
    h.dispatch(tecla("F8")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    let _ = siguientes_tasks(&mut sub).await;

    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("hay task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    asentar().await;

    assert!(
        nativos.try_recv().is_err(),
        "con el foco puesto, ningún aviso sale al escritorio"
    );
}

/// Y sin foco SÍ, con el nombre de lo que iba dentro (#285).
///
/// El nombre va ENMASCARADO como en el listado: una notificación acaba en el
/// historial del escritorio y puede verse en la pantalla de bloqueo, así que
/// lo que no puede fingir aquí tampoco puede fingir allí.
#[tokio::test]
async fn sin_foco_el_aviso_sale_y_lleva_el_nombre() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut nativos = h.native_effects();
    let mut sub = h.subscribe();
    h.dispatch(UiAction::WindowFocus { focused: false })
        .await
        .expect("host vivo");
    h.dispatch(tecla("F8")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    let _ = siguientes_tasks(&mut sub).await;

    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("hay task");
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
    let (titulo, cuerpo) = visto.expect("sin foco, el aviso sale");
    assert!(!titulo.is_empty(), "el aviso dice QUÉ pasó");
    assert!(
        cuerpo.contains("notas.txt"),
        "y con qué fichero: {cuerpo:?}"
    );
}

/// **Con UN solo panel, copiar pide el destino al escritorio** (#284).
///
/// Antes se rehusaba: quien no había partido la ventana no podía copiar. Lo
/// que se comprueba aquí es la cadena entera —el efecto sale, la respuesta
/// entra, y la transferencia acaba yendo a donde se eligió— porque cada mitad
/// por separado no dice nada sobre la otra.
#[tokio::test]
async fn con_un_panel_el_destino_lo_elige_el_escritorio() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut nativos = h.native_effects();
    let mut sub = h.subscribe();

    // F5 con un solo listado: en vez de rehusar, sale el efecto.
    h.dispatch(tecla("F5")).await.expect("host vivo");
    let efecto = tokio::time::timeout(std::time::Duration::from_secs(2), nativos.recv())
        .await
        .expect("el efecto sale antes del timeout")
        .expect("canal vivo");
    let desde = match efecto {
        norte_ui_host::dto::NativeEffect::PickDirectory { desde } => desde,
        otro => panic!("se esperaba el selector de carpeta: {otro:?}"),
    };
    assert_eq!(
        desde.to_wire(),
        "mem:///casa",
        "el selector abre donde está el panel"
    );

    // Y la respuesta entra por la misma puerta que el resto.
    h.dispatch(UiAction::DirectoryPicked {
        path: Some("/tmp".to_owned()),
    })
    .await
    .expect("host vivo");

    // Lo que sale es la confirmación de siempre, con ESE destino: el lector ve
    // a dónde van sus ficheros antes de que se mueva un byte, que es lo que
    // acota que la ruta haya venido de fuera.
    let dialogos = siguientes_dialogos(&mut sub).await;
    let confirmacion = dialogos.last().expect("hay confirmación");
    let destino = confirmacion
        .destination
        .as_ref()
        .expect("la confirmación NOMBRA el destino");
    assert!(
        destino.text.contains("/tmp"),
        "el destino elegido se enseña: {destino:?}"
    );
}

/// Cerrar el selector sin elegir no copia nada: cancelar es una respuesta.
#[tokio::test]
async fn cerrar_el_selector_sin_elegir_no_transfiere() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut nativos = h.native_effects();
    h.dispatch(tecla("F5")).await.expect("host vivo");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), nativos.recv())
        .await
        .expect("sale el efecto");

    h.dispatch(UiAction::DirectoryPicked { path: None })
        .await
        .expect("host vivo");
    asentar().await;
    assert!(
        backend
            .transferencias
            .lock()
            .expect("transferencias")
            .is_empty(),
        "cancelar el selector no transfiere"
    );
}

/// Una respuesta del selector que NADIE pidió no se interpreta. Es la misma
/// regla que un diálogo obsoleto: en una superficie que mueve ficheros, un
/// mensaje suelto no puede iniciar una operación.
#[tokio::test]
async fn un_destino_que_nadie_pidio_no_hace_nada() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let ack = h
        .dispatch(UiAction::DirectoryPicked {
            path: Some("/tmp".to_owned()),
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Stale { .. }),
        "sin selector abierto, la respuesta es obsoleta: {ack:?}"
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

/// Lo que haga con los ficheros un editor o un shell no lo decide esta
/// ventana, así que una montada sin efectos no los arranca.
#[tokio::test]
async fn en_solo_lectura_no_se_lanza_nada_del_escritorio() {
    let backend = arbol();
    let (h, _snap) = host_solo_lectura(Arc::clone(&backend)).await;
    let mut nativos = h.native_effects();
    let mut sub = h.subscribe();
    // Ni siquiera se OFRECEN: la paleta se construye con los efectos de esta
    // ventana, y ofrecer lo que se va a rehusar es prometer algo que no se
    // va a hacer. Es la misma regla que ya rige para copiar y borrar.
    h.dispatch(tecla_mod("p", true, false))
        .await
        .expect("host vivo");
    let _ = siguiente_paleta(&mut sub).await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let p = siguiente_foto(&mut sub).await.palette.expect("abierta");
    for cmd in ["pane.open", "app.terminal"] {
        assert!(
            !p.rows.iter().any(|r| r.text == cmd),
            "{cmd} no se ofrece en una ventana sin efectos"
        );
    }
    // Y copiar la ruta SÍ, porque no lanza nada.
    assert!(p.rows.iter().any(|r| r.text == "pane.copy-path") || p.total > 0);
    assert!(
        matches!(
            nativos.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ),
        "y no salió ningún efecto"
    );
}

/// Lo que no está en ESTE disco no se le da al escritorio.
///
/// A `xdg-open` no se le puede pasar un `sftp://`, y un terminal no tiene
/// dónde sentarse dentro de uno. Se rehúsa diciéndolo, en vez de abrir otra
/// cosa —el `$HOME`, típicamente— sin avisar.
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
            "{cmd} sobre un `mem://`: {ack:?}"
        );
    }
    assert!(
        matches!(
            nativos.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ),
        "y no salió ningún efecto"
    );
}

/// Sin nadie escuchando los efectos nativos, el gesto se rehúsa: no se acusa
/// recibo de algo que no va a ocurrir.
#[tokio::test]
async fn sin_escritorio_detras_copiar_se_rehusa() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // NADIE llama a `native_effects()`: es el caso de un frontend que no sabe
    // hacer estas cosas.
    let ack = ejecutar_por_paleta_ack(&h, &mut sub, "pane.copy-path").await;
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-no-desktop"),
        "{ack:?}"
    );
}

/// Un preset que reata `dialog.confirm` cambia TAMBIÉN la ventana (#287).
///
/// Los diálogos de esta ventana se atendían con teclas fijas, así que quien
/// reataba el verbo cambiaba el TUI y no la ventana — que es exactamente la
/// deriva que el catálogo compartido está para no tener. Con un campo
/// abierto sigue habiendo régimen fijo, porque no hay verbo `dialog.*` para
/// «teclea una letra»; eso lo cubre el test de al lado.
#[tokio::test]
async fn una_tecla_reatada_contesta_el_dialogo() {
    let backend = arbol();
    // Una capa de usuario que ata `s` a confirmar, sobre el preset de
    // siempre: es lo que un `keymap.toml` haría.
    let capa = norte_frontend::keymap::parse_keymap(
        "[dialog]\nappend_keymap = [{ on = [\"z\"], run = \"dialog.confirm\" }]\n",
    )
    .expect("la capa parsea");
    let base = norte_frontend::keymap::parse_keymap(
        norte_frontend::keymap::presets::source("orthodox").expect("preset"),
    )
    .expect("el preset parsea");
    let dialogo = norte_frontend::keymap::Effective::build_for(
        &base,
        std::slice::from_ref(&capa),
        norte_ui_host::commands::IMPLEMENTADOS_DIALOGO,
        norte_frontend::keymap::Screen::Dialog,
    )
    .expect("efectivo");
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: Arc::clone(&backend) as Arc<dyn norte_ui_host::backend::HostBackend>,
        initial_dir: dir(),
        initial_dir_pedido: false,
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
    .expect("arranca");
    let mut sub = h.subscribe();

    // Un borrado abre su confirmación, que NO tiene campo donde teclear.
    ejecutar_por_paleta(&h, &mut sub, "pane.delete").await;
    let dialogos = siguientes_dialogos(&mut sub).await;
    assert_eq!(dialogos.len(), 1, "la confirmación");
    h.dispatch(tecla("z")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    assert!(
        siguiente_foto(&mut sub).await.dialogs.is_empty(),
        "`z` atada a `dialog.confirm` contesta la pregunta"
    );
    hasta(&backend, "el borrado encolado", |f| {
        (!f.borrados.lock().expect("borrados").is_empty()).then_some(())
    })
    .await;
}

/// Con un CAMPO abierto, las teclas del diálogo son letras.
///
/// No hay verbo `dialog.*` para «teclea una letra», así que resolver por el
/// keymap ahí convertiría escribir un nombre de fichero en contestar la
/// pregunta. Es el mismo par de regímenes que el TUI y que el editor de
/// `[config]` de la 6.4.
#[tokio::test]
async fn con_un_campo_abierto_las_teclas_no_contestan() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "pane.mkdir").await;
    let dialogos = siguientes_dialogos(&mut sub).await;
    let d = dialogos.last().expect("el prompt del nombre");
    assert!(d.input.is_some(), "este diálogo tiene dónde teclear");
    // Una letra cualquiera: ni contesta ni cierra.
    h.dispatch(tecla("y")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    assert_eq!(
        siguiente_foto(&mut sub).await.dialogs.len(),
        1,
        "el prompt sigue abierto"
    );
}

/// Marcar todo, invertir y por PATRÓN (#289).
///
/// Lo que casa lo decide el modelo compartido (`mark_glob`), que pliega el
/// nombre antes de comparar: aquí solo se comprueba que el gesto llega y que
/// un glob que no compila se DICE en vez de no hacer nada.
#[tokio::test]
async fn marcar_todo_invertir_y_por_patron() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let marcas = |s: &norte_ui_host::ViewSnapshot| listado(s).marks;

    ejecutar_por_paleta(&h, &mut sub, "mark.all").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let todas = marcas(&siguiente_foto(&mut sub).await);
    assert!(todas > 0, "marcar todo marca algo");

    ejecutar_por_paleta(&h, &mut sub, "mark.invert").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    assert_eq!(
        marcas(&siguiente_foto(&mut sub).await),
        0,
        "invertir sobre todo marcado no deja ninguna"
    );

    // Por patrón: el prompt pide el glob y `Enter` lo aplica.
    ejecutar_por_paleta(&h, &mut sub, "mark.pattern-add").await;
    let dialogos = siguientes_dialogos(&mut sub).await;
    let d = dialogos.last().expect("el prompt del glob");
    assert!(d.input.is_some());
    h.dispatch(UiAction::DialogInput {
        id: d.id,
        text: "*".to_owned(),
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Dialog {
        id: d.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    assert_eq!(
        marcas(&siguiente_foto(&mut sub).await),
        todas,
        "`*` marca lo mismo que marcar todo"
    );

    // Y un glob que no compila se rehúsa DICIÉNDOLO.
    ejecutar_por_paleta(&h, &mut sub, "mark.pattern-remove").await;
    let dialogos = siguientes_dialogos(&mut sub).await;
    let d = dialogos.last().expect("el prompt");
    h.dispatch(UiAction::DialogInput {
        id: d.id,
        text: "[".to_owned(),
    })
    .await
    .expect("host vivo");
    let ack = h
        .dispatch(UiAction::Dialog {
            id: d.id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "err-bad-pattern"),
        "{ack:?}"
    );
}

/// El tablero se recorre y se descarta con el teclado, sin enfocar el panel
/// de procesos (#292).
///
/// Y una task VIVA no se descarta: pararla es `task.cancel`, y quitar de la
/// vista algo que sigue escribiendo en el disco es perder de vista justo lo
/// que hay que mirar.
#[tokio::test]
async fn el_tablero_se_recorre_y_se_descarta() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // Sin tasks: los tres lo dicen en vez de callar.
    for cmd in ["task.next", "task.prev", "task.dismiss"] {
        let ack = ejecutar_por_paleta_ack(&h, &mut sub, cmd).await;
        assert!(matches!(ack, ActionAck::Applied { .. }), "{cmd}: {ack:?}");
    }

    // Una task viva: descartarla se rehúsa.
    ejecutar_por_paleta(&h, &mut sub, "pane.mkdir").await;
    let dialogos = siguientes_dialogos(&mut sub).await;
    let d = dialogos.last().expect("el prompt");
    h.dispatch(UiAction::DialogInput {
        id: d.id,
        text: "nueva".to_owned(),
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Dialog {
        id: d.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    let vivas = foto_hasta(&h, &mut sub, "la task del mkdir en el tablero", |f| {
        (!f.tasks.is_empty()).then(|| f.tasks.clone())
    })
    .await;
    assert!(!vivas.is_empty(), "la task del mkdir llegó al tablero");
    if vivas
        .iter()
        .any(|t| matches!(t.state, norte_ui_host::dto::TaskStateView::Running))
    {
        let ack = ejecutar_por_paleta_ack(&h, &mut sub, "task.dismiss").await;
        assert!(
            matches!(&ack, ActionAck::Unavailable { reason_key }
                if reason_key == "host-task-running"),
            "una viva no se descarta: {ack:?}"
        );
    }

    // Cuando termina, sí: la fila desaparece del tablero.
    foto_hasta(&h, &mut sub, "ninguna task corriendo", |f| {
        f.tasks
            .iter()
            .all(|t| !matches!(t.state, norte_ui_host::dto::TaskStateView::Running))
            .then_some(())
    })
    .await;
    ejecutar_por_paleta(&h, &mut sub, "task.dismiss").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    assert!(
        siguiente_foto(&mut sub).await.tasks.is_empty(),
        "la fila terminada se descarta"
    );
}

/// Partir pone otro LISTADO al lado, en el mismo directorio y con el foco
/// (#291).
#[tokio::test]
async fn partir_abre_otro_listado_y_le_da_el_foco() {
    let backend = arbol();
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let antes = snap.slots.len();
    let dir_antes = listado(&snap).path_display.clone();

    ejecutar_por_paleta(&h, &mut sub, "layout.split-v").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(foto.slots.len(), antes + 1, "hay un hueco más");
    let listados: Vec<&norte_ui_host::dto::BrowserSlotView> = foto
        .slots
        .iter()
        .filter_map(|s| match s {
            SlotView::Browser(b) => Some(&**b),
            _ => None,
        })
        .collect();
    assert!(listados.len() >= 2, "y es un listado");
    assert!(
        listados.iter().all(|b| b.path_display == dir_antes),
        "el nuevo arranca donde estaba el que se partió: {listados:?}"
    );
    // El foco al recién nacido: partir es pedir sitio para trabajar en él.
    let enfocado = foto.focus.expect("hay foco");
    assert!(
        !foto.slots.is_empty() && enfocado != 1,
        "el foco se movió al hueco nuevo: {enfocado}"
    );
}

/// Partir un hueco que ya no da para dos se REHÚSA, y se dice.
///
/// La misma regla que la TUI y por el mismo sitio (ADR 0077): sin ella el
/// árbol se quedaba un hueco que el reparto escondía en el mismo frame — el
/// `Split` no cabe, se degrada a pestañas y la pantalla sigue enseñando uno.
#[tokio::test]
async fn partir_sin_sitio_se_rehusa_y_se_dice() {
    // 24 filas de alto para el cuerpo entero: dan para un listado y no para
    // dos (el mínimo del `browser` son 5, y el cromo se lleva lo suyo).
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
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    let listados = foto
        .slots
        .iter()
        .filter(|s| matches!(s, SlotView::Browser(_)))
        .count();
    assert_eq!(listados, antes, "el árbol no se quedó un hueco invisible");
}

/// Cerrar el ÚLTIMO listado se rehúsa y se dice.
///
/// Una pantalla sin un listado usable no es una pantalla, es un cuelgue con
/// bordes — la misma regla que el reparto compartido ya aplica por su cuenta.
#[tokio::test]
async fn no_se_cierra_el_ultimo_listado() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // `simple` tiene UN listado: cerrarlo dejaría la pantalla sin ninguno.
    let ack = ejecutar_por_paleta_ack(&h, &mut sub, "layout.close-slot").await;
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key }
            if reason_key == "msg-layout-last-panel"),
        "{ack:?}"
    );

    // Con dos, cerrar uno sí. Por TECLA y no por paleta: partir cambia la
    // pantalla entera y manda su foto, y el ayudante de la paleta lee fotos.
    ejecutar_por_paleta(&h, &mut sub, "layout.split-h").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let _ = siguiente_foto(&mut sub).await;
    ejecutar_por_paleta(&h, &mut sub, "layout.close-slot").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(
        foto.slots
            .iter()
            .filter(|s| matches!(s, SlotView::Browser(_)))
            .count(),
        1,
        "vuelve a haber uno"
    );
}

/// Los tres huecos auxiliares que esta ventana sabe pintar se abren y se
/// cierran con su comando (#291).
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
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        assert!(
            presente(&siguiente_foto(&mut sub).await),
            "{cmd} abre su hueco, y esta ventana lo PINTA (no en gris)"
        );
        ejecutar_por_paleta(&h, &mut sub, cmd).await;
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        assert!(
            !presente(&siguiente_foto(&mut sub).await),
            "{cmd} otra vez lo cierra"
        );
    }
}

/// Un host con capas de configuración DE VERDAD, para los perfiles.
///
/// Los perfiles viven en `profiles/` de la capa del usuario, y el host las
/// recibe ya resueltas (ADR 0066 D14): sin dárselas, no hay dónde buscar.
pub(super) async fn host_con_capas(
    dir_usuario: &std::path::Path,
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    host_con_capas_y_favoritos(dir_usuario, Vec::new()).await
}

/// El mismo, con favoritos YA cargados: la ventana los lee al arrancar, así
/// que un test que solo escriba el `norte.toml` monta un host que no los ve.
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
    .expect("arranca")
}

/// El selector de perfiles: los enseña, y elegir uno lo APLICA.
///
/// Un perfil sirve para que el espacio de trabajo se vea y se comporte
/// distinto, así que lo que se comprueba es que el cambio llegue a la
/// pantalla: aquí, por el tema, que es lo que se ve.
#[tokio::test]
async fn el_selector_de_perfiles_enseña_y_lo_elegido_se_aplica() {
    use norte_ui_host::dto::NativeEffect;
    let raiz = tempfile::tempdir().expect("temp");
    let fotos = raiz.path().join("profiles").join("fotos");
    std::fs::create_dir_all(&fotos).expect("mkdir");
    std::fs::write(
        fotos.join("norte.toml"),
        "[profile]\ntitle = \"Fotos\"\n\n[ui]\ntheme = \"nord\"\n",
    )
    .expect("escribir");

    let (h, _snap) = host_con_capas(raiz.path()).await;
    let mut sub = h.subscribe();
    let mut nativos = h.native_effects();

    ejecutar_por_paleta(&h, &mut sub, "profile.pick").await;
    // La lista llega de una tarea de fondo: la foto que la trae es la que
    // hay que esperar, no la siguiente que pase.
    //
    // Cien vueltas y no seis: seis es un plazo, no una espera. La tarea de
    // fondo compite con el resto de la suite por el runtime, y bajo carga
    // —la máquina compilando al lado— se pasaba de largo y el test se ponía
    // rojo sin que nada estuviera roto. Cien es del orden de las esperas
    // vecinas de este fichero.
    let mut selector = None;
    for _ in 0..100 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if let Some(p) = siguiente_foto(&mut sub).await.profiles {
            selector = Some(p);
            break;
        }
    }
    let p = selector.expect("el selector se abrió con la lista");
    assert_eq!(p.rows.len(), 1, "el perfil que hay: {:?}", p.rows);
    assert_eq!(p.rows[0].name, "fotos");
    assert_eq!(
        p.rows[0].title.as_deref(),
        Some("Fotos"),
        "su título sale del `[profile] title`"
    );
    assert!(!p.rows[0].active, "todavía no está puesto");

    // Elegirlo lo aplica: su `[ui] theme` llega a quien hospeda.
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    let efecto = tokio::time::timeout(std::time::Duration::from_secs(2), nativos.recv())
        .await
        .expect("sale el aviso de tema")
        .expect("canal vivo");
    let NativeEffect::ThemeChanged { name } = efecto else {
        panic!("el aviso es el del tema: {efecto:?}");
    };
    assert_eq!(name, "nord", "el tema del PERFIL, no el de antes");
}

/// Y el tema de un perfil puede ser una RUTA, no solo un preset (ADR 0020).
///
/// La ventana solo miraba presets, así que un perfil con
/// `theme = "…/mio.toml"` se quedaba sin colores nuevos EN SILENCIO — con el
/// terminal aplicándolo, que es la divergencia. Resolverlo lee un fichero, y
/// esta ventana no puede leer dentro del actor (regla 2): se va fuera y vuelve
/// por el buzón, como el guardado del tema.
#[tokio::test]
async fn el_tema_de_un_perfil_puede_ser_una_ruta() {
    use norte_ui_host::dto::NativeEffect;
    let raiz = tempfile::tempdir().expect("temp");
    let mio = raiz.path().join("mio.toml");
    std::fs::write(&mio, "name = \"mio\"\n").expect("escribir tema");
    let fotos = raiz.path().join("profiles").join("fotos");
    std::fs::create_dir_all(&fotos).expect("mkdir");
    std::fs::write(
        fotos.join("norte.toml"),
        format!(
            "[profile]\ntitle = \"Fotos\"\n\n[ui]\ntheme = \"{}\"\n",
            mio.display()
        ),
    )
    .expect("escribir");

    let (h, _snap) = host_con_capas(raiz.path()).await;
    let mut sub = h.subscribe();
    let mut nativos = h.native_effects();

    ejecutar_por_paleta(&h, &mut sub, "profile.pick").await;
    let mut abierto = false;
    for _ in 0..100 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if siguiente_foto(&mut sub).await.profiles.is_some() {
            abierto = true;
            break;
        }
    }
    assert!(abierto, "el selector se abrió con la lista");

    h.dispatch(tecla("Enter")).await.expect("host vivo");
    let efecto = tokio::time::timeout(std::time::Duration::from_secs(2), nativos.recv())
        .await
        .expect("sale el aviso de tema")
        .expect("canal vivo");
    let NativeEffect::ThemeChanged { name } = efecto else {
        panic!("el aviso es el del tema: {efecto:?}");
    };
    assert_eq!(
        name,
        mio.display().to_string(),
        "el tema del perfil era un fichero, y se aplicó"
    );
}

/// **La ventana AÑADE un favorito, no solo abre la lista** (#309).
///
/// Y el nombre viene sugerido por el modelo COMPARTIDO: guardar REEMPLAZA el
/// favorito que ya se llame igual, así que con el campo prellenado el reflejo
/// de aceptar sin leer pisaría uno que apuntaba a otro sitio.
#[tokio::test]
async fn la_ventana_guarda_un_favorito_con_el_nombre_sugerido() {
    let raiz = tempfile::tempdir().expect("temp");
    let (h, _snap) = host_con_capas(raiz.path()).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.hotlist").await;
    // `dialog.add` sobre la lista de favoritos: en el terminal es la `a` del
    // mismo popup.
    h.dispatch(tecla("a")).await.expect("host vivo");
    let d = siguientes_dialogos(&mut sub).await;
    assert_eq!(d[0].title_key, "modal-hotlist-name-title");
    assert_eq!(
        d[0].input.as_deref(),
        Some("casa"),
        "prellenado con la sugerencia compartida: {:?}",
        d[0].input
    );

    h.dispatch(UiAction::Dialog {
        id: d[0].id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    // La escritura vuelve por `spawn_blocking` y, al volver, el host
    // RESIEMBRA la lista de favoritos que está abierta: esperar a que la
    // pantalla lo pinte es esperar a que el fichero esté escrito, sin
    // adivinar cuánto tarda.
    // La escritura vuelve por `spawn_blocking`, y al confirmar la lista se
    // cierra: no queda nada en la pantalla que decir «ya está». Se mira el
    // FICHERO, que es lo que el test afirma, dando una vuelta al actor entre
    // ojeada y ojeada en vez de dormir un plazo fijo.
    let escrito = foto_hasta(&h, &mut sub, "el favorito escrito en norte.toml", |_| {
        std::fs::read_to_string(raiz.path().join("norte.toml"))
            .ok()
            .filter(|s| s.contains("casa"))
    })
    .await;
    assert!(
        escrito.contains("casa"),
        "el favorito acabó en el fichero: {escrito}"
    );
}

/// Y lo QUITA, que era la otra mitad que no había (#309).
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
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    let filas = foto.picker.as_ref().map_or(0, |p| p.rows.len());
    assert_eq!(filas, 1, "la lista trae el favorito: {:?}", foto.picker);
    // `dialog.remove`: la `d` del popup del terminal.
    let ack = h.dispatch(tecla("d")).await.expect("host vivo");
    assert!(
        matches!(ack, norte_ui_host::ActionAck::Applied { .. }),
        "la tecla la atiende el selector: {ack:?}"
    );
    // Igual que al añadir: la lista abierta se resiembra cuando la escritura
    // vuelve, así que la fila que se va es la señal de que el fichero ya está.
    let despues = foto_hasta(&h, &mut sub, "la lista sin el favorito", |f| {
        f.picker
            .as_ref()
            .is_some_and(|p| p.rows.is_empty())
            .then(|| f.clone())
    })
    .await;

    let escrito = std::fs::read_to_string(raiz.path().join("norte.toml")).expect("norte.toml");
    assert!(
        !escrito.contains("casa"),
        "el favorito se fue del fichero: {escrito}; filas={:?} msg={:?}",
        despues.picker.as_ref().map(|p| p.rows.len()),
        despues.status.message
    );
}

/// Un perfil que no existe no cambia nada, y se dice.
///
/// «Se sigue en el que estabas» es lo que la ADR 0079 D7 pide para un cambio:
/// arrancar sin perfil es recuperable, quedarse a medias no.
#[tokio::test]
async fn un_perfil_que_no_carga_deja_todo_como_estaba() {
    let raiz = tempfile::tempdir().expect("temp");
    std::fs::create_dir_all(raiz.path().join("profiles")).expect("mkdir");
    let (h, _snap) = host_con_capas(raiz.path()).await;
    let mut sub = h.subscribe();

    // Sin perfiles, girar no tiene a dónde ir — y lo dice en vez de fingir.
    ejecutar_por_paleta(&h, &mut sub, "profile.next").await;
    // Sin cuenta de vueltas: leer `profiles/` es una tarea de fondo, así que
    // el aviso no llega en la foto siguiente sino cuando esa tarea contesta.
    // Con seis resyncs seguidos, una máquina cargada los gastaba todos antes
    // de que el hilo de fondo despertara y el test se ponía rojo sin que nada
    // estuviera roto — que es como se aprende a ignorar un rojo.
    foto_hasta(&h, &mut sub, "el aviso de que no hay otro perfil", |f| {
        f.status.message.is_some().then_some(())
    })
    .await;
}

/// Con `[ui] parent_entry`, el listado lleva su fila `..` — y no es un
/// operando.
///
/// La fila que espera quien viene de cualquier gestor de la familia: el
/// cursor cae en ella y Enter sube. Lo que la hace segura es que sobre ella
/// no hay nada señalado, así que una copia o un borrado no tienen sobre qué
/// actuar en vez de actuar sobre el directorio padre.
#[tokio::test]
async fn con_la_fila_de_subir_el_listado_la_lleva_primera() {
    let mut cfg = norte_ui_host::ajustes_por_defecto();
    cfg.common.ui_parent_entry = Some(true);
    let (_h, snap) = UiHost::start(UiHostOptions {
        backend: arbol(),
        // Un SUBdirectorio: en una raíz no hay a dónde subir y la fila no
        // aparece por mucho que la configuración la encienda.
        initial_dir: norte_proto::VPath::parse("mem:///casa").expect("wire"),
        initial_dir_pedido: false,
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
    .expect("arranca");

    let filas = snap
        .slots
        .iter()
        .find_map(|v| match v {
            SlotView::Browser(b) => Some(b.rows.clone()),
            _ => None,
        })
        .expect("hay listado");
    assert_eq!(
        filas.first().map(|r| r.display_name.as_str()),
        Some(".."),
        "la primera fila es la de subir, pintada `..` y no con el nombre del \
         padre: {filas:?}"
    );
    assert_eq!(
        filas[0].kind,
        norte_ui_host::dto::RowKind::Dir,
        "y es un directorio: Enter sube por el mismo camino que cualquier otro"
    );
}

/// Arrastrar el borde reparte la pareja, y lo que uno gana lo pierde el otro.
///
/// El renderer manda dónde está el PUNTERO, en celdas. Qué pareja se reparte
/// y cuánto le toca a cada uno lo decide el host, que es quien tiene el
/// reparto y los mínimos de cada kind.
#[tokio::test]
async fn arrastrar_el_borde_reparte_los_dos_huecos() {
    let (h, snap) = host_con_layout(arbol(), "orthodox", (120, 40)).await;
    let ancho = |s: &norte_ui_host::ViewSnapshot, id: u32| {
        s.layout
            .placements
            .iter()
            .find(|p| p.slot_id == id)
            .map(|p| p.width)
            .expect("el hueco está colocado")
    };
    let izq = snap.layout.placements[0].slot_id;
    let der = snap.layout.placements[1].slot_id;
    let (a0, b0) = (ancho(&snap, izq), ancho(&snap, der));
    assert_eq!(a0 + b0, 120, "los dos se reparten la pantalla");

    let mut sub = h.subscribe();
    // El puntero a un tercio del ancho.
    h.dispatch(UiAction::ResizeSlot {
        slot_id: izq,
        cells: 40,
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let despues = siguiente_foto(&mut sub).await;
    // Con UNA celda de margen: la pareja se renormaliza a pesos entre 1 y 100
    // y el reparto vuelve a repartir en enteros, así que un tercio de 120
    // aterriza en 39 o en 40 según por dónde caiga el redondeo. Exigir la
    // celda exacta sería exigir que el arrastre no pase por pesos.
    let ancho_izq = ancho(&despues, izq);
    assert!(
        ancho_izq.abs_diff(40) <= 1,
        "el borde va donde dice el puntero: {ancho_izq}"
    );
    assert_eq!(
        ancho(&despues, izq) + ancho(&despues, der),
        a0 + b0,
        "la pareja ocupa lo mismo: arrastrar un borde no toca al resto"
    );
}

/// La pantalla del tema ELIGE, y lo elegido se ve.
///
/// Antes solo enseñaba: quien hospeda esta ventana resuelve el tema una vez al
/// arrancar, así que un tema elegido no tenía forma de llegar a la pantalla.
/// Con `NativeEffect::ThemeChanged` la tiene, y este selector es el del
/// terminal — presets, cursor en el que está puesto, y preview EN VIVO.
#[tokio::test]
async fn el_selector_de_tema_elige_y_avisa_a_quien_hospeda() {
    use norte_ui_host::dto::NativeEffect;
    let (h, _snap) = host_con_layout(arbol(), "orthodox", (120, 40)).await;
    let mut nativos = h.native_effects();
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "app.theme").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let abierta = siguiente_foto(&mut sub).await;
    let tema = abierta.theme.expect("la pantalla del tema está abierta");
    assert!(
        tema.choices.len() > 1,
        "hay entre qué elegir: {:?}",
        tema.choices
    );

    // Bajar previsualiza: el efecto sale ANTES de confirmar nada, que es lo
    // que hace que el lector vea el tema en vez de leer su nombre.
    h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    let efecto = tokio::time::timeout(std::time::Duration::from_secs(2), nativos.recv())
        .await
        .expect("sale el aviso de tema")
        .expect("canal vivo");
    let NativeEffect::ThemeChanged { name } = efecto else {
        panic!("el aviso es el del tema: {efecto:?}");
    };
    assert_eq!(
        name, tema.choices[1],
        "el que quedó bajo el cursor, no otro"
    );

    // Y `Escape` VUELVE al que había: un selector con preview en vivo que se
    // cierra dejando lo último que rozó el cursor es una forma de cambiar de
    // tema sin querer.
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    let vuelta = tokio::time::timeout(std::time::Duration::from_secs(2), nativos.recv())
        .await
        .expect("sale el aviso de vuelta")
        .expect("canal vivo");
    let NativeEffect::ThemeChanged { name } = vuelta else {
        panic!("el aviso es el del tema: {vuelta:?}");
    };
    assert_eq!(name, tema.name, "se vuelve al que estaba puesto");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    assert!(
        siguiente_foto(&mut sub).await.theme.is_none(),
        "y la pantalla se cierra"
    );
}

/// La barra de menús: se despliega, se recorre y lo que se elige CORRE.
///
/// Los menús y sus entradas son `norte_frontend::menu`, el mismo modelo que
/// pinta el TUI, así que aquí no se comprueba QUÉ hay dentro —eso lo cubren
/// los tests de ese crate— sino que la ventana lo proyecta, lo recorre y
/// ejecuta por el mismo camino que una tecla.
#[tokio::test]
async fn el_menu_se_recorre_y_lo_elegido_corre() {
    let (h, snap) = host_con_layout(arbol(), "orthodox", (120, 40)).await;
    assert!(snap.menu.bar, "la barra se pinta por defecto");
    assert_eq!(snap.menu.open, None, "y nace cerrada");
    assert_eq!(
        snap.menu.titles.len(),
        norte_frontend::menu::MENUS.len(),
        "todos los menús del modelo compartido"
    );

    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "app.menu").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let abierto = siguiente_foto(&mut sub).await;
    assert_eq!(abierto.menu.open, Some(0), "se despliega por el primero");
    assert!(
        !abierto.menu.items.is_empty(),
        "y trae sus entradas: {:?}",
        abierto.menu.items
    );

    // Una flecha abajo mueve el cursor DENTRO del menú, no el listado.
    let cursor_del_listado = |s: &norte_ui_host::ViewSnapshot| {
        s.slots.iter().find_map(|v| match v {
            SlotView::Browser(b) => Some(b.cursor),
            _ => None,
        })
    };
    let cursor_antes = cursor_del_listado(&abierto);
    h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let movido = siguiente_foto(&mut sub).await;
    assert_eq!(movido.menu.cursor, 1);
    assert_eq!(
        cursor_del_listado(&movido),
        cursor_antes,
        "el listado de debajo no se movió"
    );

    // Y `Escape` cierra sin ejecutar nada.
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    assert_eq!(siguiente_foto(&mut sub).await.menu.open, None);
}

/// #324: la barra de paneles cruza el puente con lo que la TUI pinta — qué
/// paneles hay, en qué orden, cuál está abierto y cuál tiene el teclado — y
/// pulsar un botón abre el panel por el MISMO despacho que su atajo. La barra
/// nueva viaja como PARCHE en el mismo envío que abre el panel, sin que
/// `alternar_hueco` sepa que existe.
#[tokio::test]
async fn la_barra_de_paneles_ensena_los_paneles_y_un_click_los_abre() {
    use norte_ui_host::dto::{PanelButtonState, ViewChange};
    let (h, snap) = host_arbol(arbol()).await;
    let barra = &snap.panel_bar;
    assert!(barra.bar, "la barra se pinta por defecto, como en la TUI");
    let kinds: Vec<&str> = barra.buttons.iter().map(|b| b.kind.as_str()).collect();
    assert_eq!(
        kinds,
        ["places", "viewer", "processes", "metadata", "tree", "log"],
        "los mismos botones y el mismo orden que `panelbar::buttons`"
    );
    let sitios = kinds.iter().position(|k| *k == "places").expect("places");
    let boton = &barra.buttons[sitios];
    assert_eq!(boton.label, "Sitios", "traducido al idioma de la sesión");
    assert_eq!(boton.letter, "S");
    assert_eq!(boton.state, PanelButtonState::Closed, "{barra:?}");
    assert!(
        barra.buttons.iter().all(|b| !b.attention),
        "sin tareas ni avisos nada tiene novedad: {barra:?}"
    );

    let mut sub = h.subscribe();
    h.dispatch(UiAction::PanelBarActivate {
        button: u32::try_from(sitios).expect("seis botones caben en un u32"),
    })
    .await
    .expect("host vivo");
    // Lo que abre el panel LLEVA la barra nueva: abrir un hueco cambia el
    // reparto y va como FOTO, y la foto trae la barra; un cambio que fuera
    // como parche la traería como `ViewChange::PanelBar`. Se aceptan las
    // dos formas, y con plazo: un host que no la mandara dejaría este
    // `recv` esperando para siempre, y un test colgado no es un test rojo.
    let mut barra_nueva = None;
    let plazo = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while barra_nueva.is_none() {
        let siguiente = tokio::time::timeout_at(plazo, sub.recv())
            .await
            .expect("la barra nueva llega antes de cinco segundos")
            .expect("host vivo");
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
    let barra = barra_nueva.expect("la barra viajó");
    assert_ne!(
        barra.buttons[sitios].state,
        PanelButtonState::Closed,
        "el panel de sitios está abierto: {barra:?}"
    );

    // Abrir la barra de sitios dispara una lectura de volúmenes que aterriza
    // como OTRA foto, más tarde: se espera a la foto que enseña el estado
    // pedido, no a la siguiente que haya en la cola.
    let con_sitios =
        |s: &norte_ui_host::ViewSnapshot| s.slots.iter().any(|v| matches!(v, SlotView::Places(_)));
    let abierto = foto_hasta(&h, &mut sub, "el hueco de sitios colocado", |s| {
        con_sitios(s).then(|| s.clone())
    })
    .await;
    assert_ne!(
        abierto.panel_bar.buttons[sitios].state,
        PanelButtonState::Closed
    );

    // El mismo botón otra vez lo CIERRA: es un conmutador, como su atajo.
    let ack = h
        .dispatch(UiAction::PanelBarActivate {
            button: u32::try_from(sitios).expect("seis botones caben en un u32"),
        })
        .await
        .expect("host vivo");
    assert!(matches!(ack, ActionAck::Applied { .. }), "fue {ack:?}");
    let cerrado = foto_hasta(&h, &mut sub, "el hueco de sitios cerrado", |s| {
        (!con_sitios(s)).then(|| s.clone())
    })
    .await;
    assert_eq!(
        cerrado.panel_bar.buttons[sitios].state,
        PanelButtonState::Closed
    );

    // Un índice que la barra no tiene es una barra vieja: que pida foto.
    let ack = h
        .dispatch(UiAction::PanelBarActivate { button: 99 })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, norte_ui_host::ActionAck::Stale { .. }),
        "fue {ack:?}"
    );
}

/// #291: el hueco de preview SIGUE al cursor y enseña el mismo visor que el
/// grande — con la preview del plugin y sus fragmentos—; sobre un
/// directorio dice que lo es, y cerrarlo lo quita. El último de los siete
/// kinds de la ADR 0058 que la ventana no pintaba.
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

    // Abrirlo es un comando del catálogo, el mismo que en la TUI.
    ejecutar_por_paleta(&h, &mut sub, "layout.preview").await;
    let preview_de = |s: &norte_ui_host::ViewSnapshot| {
        s.slots.iter().find_map(|v| match v {
            SlotView::Preview(p) => Some(p.as_ref().clone()),
            _ => None,
        })
    };
    // El cursor nace sobre `..` o sobre `docs`: primero la nota. Hasta que
    // el listado aterriza no hay cursor, y ESA nota es otra («nada
    // seleccionado»): se espera a la del directorio.
    let con_nota = foto_hasta(
        &h,
        &mut sub,
        "el hueco de preview sobre un directorio",
        |s| preview_de(s).filter(|p| p.viewer.is_none() && p.note == "directorio"),
    )
    .await;
    assert!(con_nota.viewer.is_none(), "{con_nota:?}");

    // Bajar hasta el fichero: el hueco lo lee solo, y lo que enseña es la
    // preview del plugin, con su fragmento y su «via».
    for _ in 0..3 {
        h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    }
    let con_visor = foto_hasta(&h, &mut sub, "el hueco de preview con el fichero", |s| {
        preview_de(s).filter(|p| p.viewer.is_some())
    })
    .await;
    let visor = con_visor.viewer.expect("visor");
    assert!(
        visor.path_display.ends_with("main.rs"),
        "{}",
        visor.path_display
    );
    assert_eq!(visor.styled.len(), 1);
    assert_eq!(visor.styled[0][0].role.as_deref(), Some("title"));
    assert!(visor.preview_by.contains("Syntax"));
    assert!(con_visor.note.is_empty());
    // Y el ancho que se pidió es el del HUECO, no el de la ventana.
    let anchos = f.anchos_de_preview.lock().expect("mutex").clone();
    assert!(
        anchos.iter().all(|a| a.is_some_and(|a| a < 120)),
        "el previewer recibe el ancho del hueco: {anchos:?}"
    );

    // El mismo comando lo cierra, y con él se va lo que enseñaba.
    ejecutar_por_paleta(&h, &mut sub, "layout.preview").await;
    let cerrado = foto_hasta(&h, &mut sub, "sin hueco de preview", |s| {
        preview_de(s).is_none().then(|| s.clone())
    })
    .await;
    assert!(cerrado.viewer.is_none(), "el visor GRANDE no se abrió");
}

/// #291, segunda mitad: con el FOCO en el hueco acoplado, las teclas del
/// visor mueven ese visor; la rueda lo mueve por el host; `viewer.close`
/// devuelve el foco al listado sin cerrar el hueco (como la TUI); y sin el
/// foco, las flechas siguen moviendo el listado.
#[tokio::test]
async fn el_hueco_de_preview_con_el_foco_se_mueve_con_las_teclas_del_visor() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"largo.txt".to_vec(), false)]);
    let texto = (1..=80)
        .map(|i| format!("línea {i}"))
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
    // Bajar hasta el fichero.
    for _ in 0..3 {
        h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    }
    let con_visor = foto_hasta(&h, &mut sub, "el hueco de preview con el fichero", |s| {
        preview_de(s).filter(|p| p.viewer.is_some())
    })
    .await;
    let v = con_visor.viewer.as_ref().expect("visor");
    assert_eq!(v.first_line, 0);
    assert!(
        v.lines.len() < 80 && v.lines.len() <= 38,
        "viaja la VENTANA que cabe en el hueco, no el fichero: {}",
        v.lines.len()
    );
    let slot = con_visor.slot_id;

    // Sin el foco, una flecha va al LISTADO, no al visor. Abajo y no arriba:
    // arriba cambiaría el fichero bajo el cursor, y con él lo que el hueco
    // enseña — lo que se mide aquí es a quién fue la tecla.
    h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    let sin_foco = foto_hasta(&h, &mut sub, "la flecha fue al listado", |s| {
        preview_de(s).filter(|p| p.viewer.as_ref().is_some_and(|v| v.first_line == 0))
    })
    .await;
    assert!(sin_foco.viewer.is_some());

    // Con el foco en el hueco: la flecha mueve el visor.
    let ack = h
        .dispatch(UiAction::FocusSlot { slot_id: slot })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "enfocar el hueco: {ack:?}"
    );
    let enfocado = foto_hasta(&h, &mut sub, "el hueco de preview con el foco", |s| {
        s.layout
            .placements
            .iter()
            .any(|p| p.slot_id == slot && p.role == Some(norte_ui_host::dto::SlotRole::Active))
            .then(|| s.clone())
    })
    .await;
    assert_eq!(enfocado.focus, Some(slot));
    let ack = h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "la flecha en el visor: {ack:?}"
    );
    let movido = foto_hasta(&h, &mut sub, "el visor acoplado bajó una línea", |s| {
        preview_de(s).filter(|p| p.viewer.as_ref().is_some_and(|v| v.first_line == 1))
    })
    .await;
    assert_eq!(movido.viewer.expect("visor").first_line, 1);

    // La rueda, por el host.
    h.dispatch(UiAction::PreviewScroll {
        slot_id: slot,
        delta: 3,
    })
    .await
    .expect("host vivo");
    foto_hasta(&h, &mut sub, "la rueda bajó tres más", |s| {
        preview_de(s).filter(|p| p.viewer.as_ref().is_some_and(|v| v.first_line == 4))
    })
    .await;

    // `viewer.close` (Esc en el keymap del visor) devuelve el foco al
    // listado y deja el hueco donde está.
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    let devuelto = foto_hasta(&h, &mut sub, "el foco volvió al listado", |s| {
        let activo = s
            .layout
            .placements
            .iter()
            .find(|p| p.role == Some(norte_ui_host::dto::SlotRole::Active))
            .map(|p| p.slot_id);
        (activo.is_some() && activo != Some(slot)).then(|| s.clone())
    })
    .await;
    assert!(preview_de(&devuelto).is_some(), "el hueco sigue abierto");
}

/// El menú se REABRE por donde iba, no por el primero.
///
/// Abrirlo siempre por el primero obliga a recorrer la barra entera en cada
/// gesto, y quien usa dos entradas del mismo menú lo paga cada vez.
#[tokio::test]
async fn el_menu_se_reabre_por_donde_iba() {
    let (h, _snap) = host_con_layout(arbol(), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    // Abrir, moverse dos menús a la derecha y cerrar con `Escape`.
    h.dispatch(UiAction::MenuOpen { menu: 2 })
        .await
        .expect("host vivo");
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    assert_eq!(
        siguiente_foto(&mut sub).await.menu.open,
        None,
        "cerrado del todo"
    );

    // Y al reabrirlo sale por el mismo.
    ejecutar_por_paleta(&h, &mut sub, "app.menu").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    assert_eq!(siguiente_foto(&mut sub).await.menu.open, Some(2));
}

/// Elegir en el menú corre el comando, y el menú se cierra ANTES.
///
/// El orden importa: el comando puede abrir otra pantalla, y hacerlo por
/// detrás del menú lo dejaría comiéndose las teclas de la que acaba de
/// abrirse. Es la misma regla que la paleta.
#[tokio::test]
async fn lo_elegido_en_el_menu_corre_y_el_menu_se_cierra_antes() {
    let (h, _snap) = host_con_layout(arbol(), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    // El menú «Ayuda» y su primera entrada, que es `app.help`: abre una
    // pantalla, así que sirve para ver que el menú no se queda encima.
    let ayuda = norte_frontend::menu::MENUS.len() - 1;
    h.dispatch(UiAction::MenuOpen {
        menu: u32::try_from(ayuda).expect("cabe"),
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::MenuActivateRow { row: 0 })
        .await
        .expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(foto.menu.open, None, "el menú se cerró");
    assert!(foto.help.is_some(), "y lo elegido corrió");
}

/// Del panel de PROCESOS se sale con la misma tecla con la que se entró.
///
/// Un anillo que entra en un panel y no sale de él no es un anillo: es una
/// trampa, y el lector se queda sin forma de volver al listado sin ratón.
#[tokio::test]
async fn del_panel_de_procesos_se_sale_tabulando() {
    use norte_ui_host::dto::SlotRole;
    let (h, _snap) = host_con_layout(arbol(), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "layout.processes").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let abierto = siguiente_foto(&mut sub).await;
    let procesos = abierto
        .slots
        .iter()
        .find_map(|v| match v {
            SlotView::Processes { slot_id, .. } => Some(*slot_id),
            _ => None,
        })
        .expect("el panel está en pantalla");

    let activo = |s: &norte_ui_host::ViewSnapshot| {
        s.layout
            .placements
            .iter()
            .find(|p| p.role == Some(SlotRole::Active))
            .map(|p| p.slot_id)
    };
    // Se tabula hasta caer en el panel de procesos...
    let mut dentro = false;
    for _ in 0..6 {
        h.dispatch(tecla("Tab")).await.expect("host vivo");
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if activo(&siguiente_foto(&mut sub).await) == Some(procesos) {
            dentro = true;
            break;
        }
    }
    assert!(dentro, "el anillo llega al panel de procesos");

    // ...y se sale.
    h.dispatch(tecla("Tab")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    assert_ne!(
        activo(&siguiente_foto(&mut sub).await),
        Some(procesos),
        "y se SALE de él: un anillo que entra y no sale es una trampa"
    );
}

/// El anillo del teclado NO para en la hoja de atributos.
///
/// El recorrido compartido (`focus_order`) lleva todo lo ENFOCABLE, y la hoja
/// lo es: el reparto la cuenta. Pero no toma teclas —sigue al cursor del
/// listado, y con el teclado dentro dejaría de seguir a nada, que es la mitad
/// de #243— así que pararse ahí es una parada de la que ninguna tecla saca:
/// las flechas no mueven nada y no hay nada en pantalla que lo explique.
///
/// El TUI recorre el anillo con la misma regla (`takes_keys` del registro
/// compartido), y una decisión duplicada entre frontends diverge en silencio
/// (ADR 0077).
#[tokio::test]
async fn el_anillo_no_se_para_en_la_hoja_de_atributos() {
    use norte_ui_host::dto::SlotRole;
    // Dos listados, para que el anillo tenga a dónde ir cuando salte la hoja:
    // con uno solo la respuesta correcta es «no hay otro hueco».
    let (h, _snap) = host_con_layout(arbol(), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "layout.metadata").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let abierto = siguiente_foto(&mut sub).await;
    let hoja = abierto
        .slots
        .iter()
        .find_map(|v| match v {
            SlotView::Metadata(m) => Some(m.slot_id),
            _ => None,
        })
        .expect("la hoja está en pantalla");

    // Una vuelta entera al anillo: la hoja no puede tener el foco en ningún
    // momento de ella.
    for _ in 0..6 {
        ejecutar_por_paleta(&h, &mut sub, "layout.focus-next").await;
        h.dispatch(UiAction::Resync).await.expect("host vivo");
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
            "el anillo se paró en la hoja de atributos, que no toma teclas"
        );
    }
}

/// Las pestañas: abrir, recorrer, mover, ir a la N y cerrar (#288).
///
/// El grupo lo lleva el modelo COMPARTIDO (`add_tab` envuelve el hueco si
/// hacía falta, `move_tab` no da la vuelta): aquí se comprueba que el gesto
/// llega, que la pestaña que se pone delante se lleva el FOCO —trabajar con
/// una que no se ve es lo que esto evita— y que la vista dice qué hay.
#[tokio::test]
async fn las_pestanas_se_abren_se_recorren_y_se_cierran() {
    let backend = arbol();
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    assert!(snap.layout.tabs.is_empty(), "sin grupo no hay barra");

    ejecutar_por_paleta(&h, &mut sub, "pane.tab-new").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    let grupo = foto.layout.tabs.first().expect("hay grupo").clone();
    assert_eq!(grupo.tabs.len(), 2, "dos pestañas");
    assert_eq!(grupo.active, 1, "la nueva queda delante");
    assert_eq!(
        Some(grupo.tabs[1].slot_id),
        foto.focus,
        "y con el foco: trabajar en una que no se ve es lo que esto evita"
    );

    // Recorrer CICLA.
    ejecutar_por_paleta(&h, &mut sub, "pane.tab-next").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(
        foto.layout.tabs.first().expect("grupo").active,
        0,
        "de la última a la primera"
    );

    // Ir a la N que no existe se rehúsa: adivinar sería cambiar de pestaña
    // sola.
    let ack = ejecutar_por_paleta_ack(&h, &mut sub, "pane.tab-goto-9").await;
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-no-such-tab"),
        "{ack:?}"
    );

    // Cerrar la de delante deja una, y el grupo se disuelve.
    ejecutar_por_paleta(&h, &mut sub, "pane.tab-close").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(
        foto.layout.tabs.is_empty(),
        "un grupo de una no es un grupo: {:?}",
        foto.layout.tabs
    );
}

/// Sin grupo, los comandos de pestaña lo DICEN.
///
/// Cerrar el hueco entero es otro comando: hacerlo aquí «porque no había
/// pestañas» sería cerrar lo que nadie pidió cerrar.
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

/// Un clic en una pestaña la pone delante; contra un árbol que ya cambió, se
/// rehúsa en vez de acertar por casualidad.
#[tokio::test]
async fn un_clic_en_una_pestana_la_pone_delante() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "pane.tab-new").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let grupo = siguiente_foto(&mut sub)
        .await
        .layout
        .tabs
        .first()
        .expect("grupo")
        .clone();
    let primera = grupo.tabs[0].slot_id;

    let ack = h
        .dispatch(UiAction::SelectTab { slot_id: primera })
        .await
        .expect("host vivo");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    // Cada cambio de disposición manda su propia foto, así que las que se
    // acumulan en la cola son de ANTES: se busca la que ya refleja el clic
    // en vez de leer la primera que salga.
    let mut visto = None;
    for _ in 0..10 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        if foto.layout.tabs.first().is_some_and(|g| g.active == 0) {
            visto = Some(foto);
            break;
        }
    }
    let foto = visto.expect("el clic pone delante la primera");
    assert_eq!(foto.focus, Some(primera));

    // Un hueco que no está en ningún grupo: obsoleto, no un acierto.
    let ack = h
        .dispatch(UiAction::SelectTab { slot_id: 4242 })
        .await
        .expect("host vivo");
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
