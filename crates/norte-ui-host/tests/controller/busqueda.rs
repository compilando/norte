use super::*;

// ---------------------------------------------------------------------------
// Buscar por el subárbol (tarea 6.1).
// ---------------------------------------------------------------------------

/// Espera la siguiente actualización con la búsqueda.
pub(super) async fn siguiente_busqueda(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::SearchView> {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("una actualización antes del plazo")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Search { search } = c {
                    return search.clone();
                }
            }
        }
    }
    panic!("no llegó ninguna actualización con búsqueda");
}

/// Un árbol con hallazgos preparados para un patrón.
pub(super) fn arbol_con_hallazgos(patron: &str, rutas: &[&str]) -> Arc<Falso> {
    let base = arbol();
    let mut f = Falso {
        hallazgos: [(
            patron.to_owned(),
            rutas
                .iter()
                .map(|r| norte_proto::VPath::parse(r).expect("vpath"))
                .collect(),
        )]
        .into_iter()
        .collect(),
        ..Falso::default()
    };
    f.arbol.clone_from(&base.arbol);
    Arc::new(f)
}

/// Buscar abre su prompt, lanza la Task y los hallazgos llegan en lotes: la
/// vista se abre YA, diciendo que corre, y se llena después.
#[tokio::test]
async fn buscar_abre_su_vista_y_los_hallazgos_llegan_en_lotes() {
    let backend = arbol_con_hallazgos("*.txt", &["mem:///casa/notas.txt", "mem:///casa/docs"]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    por_la_paleta(&h, &mut sub, "pane.search").await;
    // El prompt pide el patrón.
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    let dialogo = foto
        .dialogs
        .iter()
        .find(|d| d.input.is_some())
        .expect("el prompt de buscar pide un patrón");
    h.dispatch(UiAction::DialogInput {
        id: dialogo.id,
        text: "*.txt".to_owned(),
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Dialog {
        id: dialogo.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");

    let primera = siguiente_busqueda(&mut sub).await.expect("la vista abre");
    assert_eq!(primera.query, "*.txt");
    assert!(
        primera.running,
        "se abre YA y diciendo que corre: esperar al primer lote es una \
         ventana que no reacciona a una tecla que sí hizo algo"
    );

    let mut con_filas = None;
    for _ in 0..20 {
        let Some(v) = siguiente_busqueda(&mut sub).await else {
            continue;
        };
        if !v.rows.is_empty() {
            con_filas = Some(v);
            break;
        }
    }
    let v = con_filas.expect("los hallazgos llegan");
    assert_eq!(v.rows.len(), 2);
    assert_eq!(v.rows[0].name, "notas.txt");
    assert!(
        !v.rows[0].parent.is_empty(),
        "y dónde está: {:?}",
        v.rows[0]
    );
    assert!(
        !v.status.is_empty() && !v.status.starts_with("search-status"),
        "la frase de estado viene traducida: {:?}",
        v.status
    );
    assert_eq!(
        backend.busquedas.lock().expect("mutex").as_slice(),
        &["*.txt".to_owned()],
        "y el patrón llegó al wire tal cual"
    );
}

/// Cero omitidas NO es un aviso, y las que hay se leen como aviso.
///
/// La ventana usaba una clave propia —«se saltaron N entradas», sin la marca
/// que la hace leerse como aviso— y la pintaba TAMBIÉN con N igual a cero: o
/// sea que anunciaba un listado incompleto que estaba completo, gastando la
/// única señal que hay para cuando de verdad falta algo.
#[tokio::test]
async fn el_aviso_de_omitidas_calla_con_cero_y_va_marcado_con_mas() {
    for (omitidas, espera_aviso) in [(None, false), (Some(0), false), (Some(2), true)] {
        let mut f = Falso::default();
        f.arbol.clone_from(&arbol().arbol);
        f.omitidas = omitidas;
        let (_h, snap) = host_arbol(Arc::new(f)).await;
        let nota = &listado_de(&snap, 1).skipped_note;

        assert_eq!(
            !nota.is_empty(),
            espera_aviso,
            "con {omitidas:?} omitidas la nota fue {nota:?}"
        );
        if espera_aviso {
            assert!(
                nota.contains('⚠'),
                "un aviso sin marca se lee como un contador: {nota:?}"
            );
            assert!(nota.contains('2'), "{nota:?}");
        }
    }
}

/// Y la cabecera dice las otras tres cosas que solo decía el terminal.
///
/// Las tres bajo la misma regla: un listado que enseña menos de lo que hay
/// —o que no enseña lo que hay— jamás es silencioso. La de los NOMBRES es la
/// que más costaba: la ventana transcribía con otra codificación y no lo
/// decía en ninguna parte salvo el mensaje del toggle, que se lleva la
/// siguiente tecla.
#[tokio::test]
async fn la_cabecera_dice_que_los_nombres_se_reinterpretan_y_cuanto_hay_marcado() {
    let (h, snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let antes = listado_de(&snap, 1);
    assert_eq!(antes.names_note, "", "sin reinterpretar no dice nada");
    assert_eq!(antes.marked_note, "", "quien no marca no gana ruido");

    ejecutar_por_paleta(&h, &mut sub, "pane.names-encoding").await;
    let con_nombres = foto_hasta(&h, &mut sub, "la cabecera con la codificación", |s| {
        let b = listado_de(s, 1);
        (!b.names_note.is_empty()).then(|| b.names_note.clone())
    })
    .await;
    assert!(
        !con_nombres.contains("status-names"),
        "traducida, no la clave: {con_nombres}"
    );

    marca_todo(&h, &mut sub, 1).await;
    let marcado = foto_hasta(&h, &mut sub, "la cabecera con lo marcado", |s| {
        let b = listado_de(s, 1);
        (!b.marked_note.is_empty()).then(|| b.marked_note.clone())
    })
    .await;
    assert!(
        !marcado.contains("status-marked"),
        "traducida, no la clave: {marcado}"
    );
}

/// Una búsqueda que FALLÓ no se lee como una que terminó sin hallazgos.
///
/// El host marcaba cualquier estado terminal como «ya no está viva» y pintaba
/// `search-status-done`, así que una búsqueda que se rompió al segundo
/// directorio y otra que recorrió el árbol entero decían lo mismo: «0
/// hallazgos». Eso no es una imprecisión de la interfaz — es una afirmación
/// falsa sobre el disco, y quien la lee deja de buscar.
#[tokio::test]
async fn una_busqueda_que_fallo_lo_dice_y_no_finge_cero_hallazgos() {
    let mut f = Falso::default();
    f.arbol.clone_from(&arbol().arbol);
    f.desenlace_de_busqueda = Some(norte_proto::TaskState::Failed {
        error: norte_proto::Error::PermissionDenied,
    });
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    let _ = buscar(&h, &mut sub, "*.txt").await;
    let vista = foto_hasta(&h, &mut sub, "la búsqueda con su desenlace", |s| {
        s.search.clone().filter(|b| !b.running)
    })
    .await;

    let cero_hallazgos =
        norte_i18n::ta_in(norte_i18n::Lang::Es, "search-status-done", &[("n", "0")]);
    assert_ne!(
        vista.status, cero_hallazgos,
        "una búsqueda rota NO es una búsqueda sin resultados"
    );
    assert!(
        vista
            .status
            .contains(&norte_frontend::error::error_category_in(
                norte_i18n::Lang::Es,
                &norte_proto::Error::PermissionDenied
            )),
        "y dice POR QUÉ se rompió: {}",
        vista.status
    );
}

/// Y una que ni llegó a ENCOLARSE deja de decir que busca.
///
/// El otro camino, y el que no tenía test: ahí no hay Task, así que no hay
/// progreso que traiga el desenlace. La vista se quedaba en «buscando…» para
/// siempre mientras el error pasaba por la barra y se lo llevaba la siguiente
/// tecla.
#[tokio::test]
async fn una_busqueda_que_ni_se_encola_deja_de_decir_que_busca() {
    let mut f = Falso::default();
    f.arbol.clone_from(&arbol().arbol);
    f.error_de_busqueda = Some(norte_proto::Error::ProviderUnavailable { retryable: false });
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    let _ = buscar(&h, &mut sub, "*.txt").await;
    let vista = foto_hasta(&h, &mut sub, "la búsqueda que no arrancó", |s| {
        s.search.clone().filter(|b| !b.running)
    })
    .await;

    assert!(
        vista
            .status
            .contains(&norte_frontend::error::error_category_in(
                norte_i18n::Lang::Es,
                &norte_proto::Error::ProviderUnavailable { retryable: false }
            )),
        "dice por qué no arrancó, y de forma persistente: {}",
        vista.status
    );
}

/// Y una que CANCELÓ el lector tampoco: lo encontrado vale, lo que falta no
/// se llegó a mirar.
#[tokio::test]
async fn una_busqueda_cancelada_no_se_lee_como_terminada() {
    let mut f = Falso::default();
    f.arbol.clone_from(&arbol().arbol);
    f.desenlace_de_busqueda = Some(norte_proto::TaskState::Cancelled);
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    let _ = buscar(&h, &mut sub, "*.txt").await;
    let vista = foto_hasta(&h, &mut sub, "la búsqueda cancelada", |s| {
        s.search.clone().filter(|b| !b.running)
    })
    .await;

    assert_eq!(
        vista.status,
        norte_i18n::ta_in(
            norte_i18n::Lang::Es,
            "search-status-cancelled",
            &[("n", &vista.rows.len().to_string())]
        ),
        "cancelada tiene su propia frase, y la del terminal"
    );
}

/// Lanza la búsqueda `patron` por el prompt y devuelve su primera vista.
pub(super) async fn buscar(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    patron: &str,
) -> norte_ui_host::dto::SearchView {
    por_la_paleta(h, sub, "pane.search").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(sub).await;
    let dialogo = foto
        .dialogs
        .iter()
        .find(|d| d.input.is_some())
        .expect("el prompt de buscar pide un patrón");
    h.dispatch(UiAction::DialogInput {
        id: dialogo.id,
        text: patron.to_owned(),
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Dialog {
        id: dialogo.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    siguiente_busqueda(sub).await.expect("la vista abre")
}

/// Cerrar una búsqueda SIN hallazgos cancela igual.
///
/// La búsqueda se nombraba con el primer lote, y el core NO manda lotes
/// vacíos (`norte-core/src/search.rs`: `if batch.is_empty() { return
/// FlushOutcome::Continue }`). Así que sobre un árbol sin coincidencias el id
/// no llegaba nunca, `esc` no tenía a quién cancelar y el daemon seguía
/// caminando el subárbol entero para una superficie ya cerrada. La
/// cancelación existía y era inalcanzable: la regla 3 rota por el lado de la
/// UI.
#[tokio::test]
async fn cerrar_una_busqueda_sin_hallazgos_la_cancela() {
    let backend = arbol_con_hallazgos("*.txt", &["mem:///casa/notas.txt"]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    let v = buscar(&h, &mut sub, "*.zzz").await;
    assert_eq!(v.query, "*.zzz");
    assert!(v.rows.is_empty(), "no hay nada que encontrar");

    h.dispatch(tecla("Escape")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    assert!(
        siguiente_foto(&mut sub).await.search.is_none(),
        "la vista se cierra"
    );
    assert_eq!(
        backend.cancelaciones.load(Ordering::SeqCst),
        1,
        "y la Task se cancela AUNQUE no haya llegado ni un lote: es lo único \
         que para al daemon"
    );
}

/// Un lote rezagado de la búsqueda ANTERIOR no llena la lista de la nueva.
///
/// El reenviador de la búsqueda vieja no se aborta —su `tokio::spawn` no
/// guarda handle— así que puede seguir escupiendo lotes después del `esc`.
/// Con la búsqueda nombrándose por el primer lote, el primero que llegara la
/// bautizaba: los hallazgos de la ANTERIOR llenaban la lista rotulada con la
/// consulta NUEVA, y `enter` navegaba a un fichero que casaba el patrón viejo.
#[tokio::test]
async fn un_lote_de_la_busqueda_anterior_no_llena_la_nueva() {
    let backend = arbol_con_hallazgos("*.txt", &["mem:///casa/notas.txt"]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // La primera encuentra algo; se cierra antes de mirarlo.
    let _ = buscar(&h, &mut sub, "*.txt").await;
    h.dispatch(tecla("Escape")).await.expect("host vivo");

    // La segunda no encuentra nada.
    let v = buscar(&h, &mut sub, "*.zzz").await;
    assert_eq!(v.query, "*.zzz");

    // Y sigue sin encontrar nada por mucho que se drene el buzón: lo que
    // quede de la primera no es suyo.
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        let Some(s) = foto.search else { continue };
        assert!(
            s.rows.is_empty(),
            "un lote de `*.txt` no puede aparecer bajo `*.zzz`: {:?}",
            s.rows
        );
    }
}

/// Un diálogo modal se queda el teclado.
///
/// `tecla_de_un_overlay` enrutaba nueve superficies y NO el diálogo, que es
/// la única con `aria-modal` de verdad, así que las teclas caían al listado
/// de DEBAJO: con el prompt de un nombre abierto, `Backspace` navegaba al
/// padre y `Enter` entraba en el directorio bajo el cursor en vez de
/// confirmar. Es la superficie donde se aprueban los bytes de un nombre de
/// fichero, y la que en fase 5 preguntará antes de borrar.
#[tokio::test]
async fn un_dialogo_se_queda_el_teclado() {
    let backend = arbol_con_hallazgos("*.txt", &["mem:///casa/notas.txt"]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // El cursor se pone sobre un DIRECTORIO, que es lo que `Enter` abriría.
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let antes = siguiente_foto(&mut sub).await;
    let SlotView::Browser(b0) = &antes.slots[0] else {
        panic!("el primer hueco es un listado");
    };
    let donde = b0.path_display.clone();

    por_la_paleta(&h, &mut sub, "pane.search").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    let dialogo = foto
        .dialogs
        .iter()
        .find(|d| d.input.is_some())
        .expect("el prompt pide un patrón")
        .clone();

    // Las teclas de navegación NO llegan al listado de debajo.
    for k in ["Backspace", "ArrowDown", "Home"] {
        h.dispatch(tecla(k)).await.expect("host vivo");
    }
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let durante = siguiente_foto(&mut sub).await;
    let SlotView::Browser(b1) = &durante.slots[0] else {
        panic!("el primer hueco es un listado");
    };
    assert_eq!(
        b1.path_display, donde,
        "el panel de debajo no se ha movido: el modal se queda las teclas"
    );
    assert!(
        durante.dialogs.iter().any(|d| d.id == dialogo.id),
        "y el diálogo sigue abierto"
    );

    // `Enter` CONFIRMA el diálogo, no abre el directorio bajo el cursor.
    h.dispatch(UiAction::DialogInput {
        id: dialogo.id,
        text: "*.txt".to_owned(),
    })
    .await
    .expect("host vivo");
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    let v = siguiente_busqueda(&mut sub)
        .await
        .expect("confirmar con el teclado lanza la búsqueda");
    assert_eq!(v.query, "*.txt");
}

/// `Escape` cancela el diálogo, y solo el diálogo.
#[tokio::test]
async fn escape_cancela_el_dialogo_de_arriba() {
    let backend = arbol_con_hallazgos("*.txt", &[]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    por_la_paleta(&h, &mut sub, "pane.search").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    assert!(
        siguiente_foto(&mut sub)
            .await
            .dialogs
            .iter()
            .any(|d| d.input.is_some()),
        "el prompt abre"
    );
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.dialogs.is_empty(), "y `esc` lo cierra");
    assert!(
        foto.search.is_none(),
        "sin lanzar nada: cancelar es cancelar"
    );
}

/// Ir a un resultado navega a su DIRECTORIO y deja el cursor encima, sin
/// reconstruir ninguna ruta.
#[tokio::test]
async fn ir_a_un_resultado_navega_y_deja_el_cursor_encima() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"docs".to_vec(), true)]);
    f.pon(
        "mem:///casa/docs",
        vec![(b"a.md".to_vec(), false), (b"hallado.md".to_vec(), false)],
    );
    f.hallazgos = [(
        "hallado*".to_owned(),
        vec![norte_proto::VPath::parse("mem:///casa/docs/hallado.md").expect("vpath")],
    )]
    .into_iter()
    .collect();
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    por_la_paleta(&h, &mut sub, "pane.search").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    let dialogo = foto
        .dialogs
        .iter()
        .find(|d| d.input.is_some())
        .expect("el prompt está");
    h.dispatch(UiAction::DialogInput {
        id: dialogo.id,
        text: "hallado*".to_owned(),
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Dialog {
        id: dialogo.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    for _ in 0..20 {
        let Some(v) = siguiente_busqueda(&mut sub).await else {
            continue;
        };
        if !v.rows.is_empty() {
            break;
        }
    }

    h.dispatch(UiAction::SearchActivateRow { row: 0 })
        .await
        .expect("host vivo");
    let mut llego = false;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        if listado(&foto).path_display.contains("docs") {
            assert!(foto.search.is_none(), "la búsqueda se cierra al ir");
            let bajo_cursor = listado(&foto)
                .rows
                .iter()
                .find(|r| Some(r.key) == listado(&foto).cursor)
                .map(|r| r.display_name.clone());
            assert_eq!(
                bajo_cursor.as_deref(),
                Some("hallado.md"),
                "y el cursor queda ENCIMA del hallazgo, casado byte a byte"
            );
            llego = true;
            break;
        }
    }
    assert!(llego, "el panel navegó al directorio del hallazgo");
}

/// Un patrón vacío no lanza nada y lo dice: casaría el árbol entero.
#[tokio::test]
async fn un_patron_vacio_no_lanza_nada() {
    let backend = arbol_con_hallazgos("*", &[]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    por_la_paleta(&h, &mut sub, "pane.search").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    let dialogo = foto
        .dialogs
        .iter()
        .find(|d| d.input.is_some())
        .expect("el prompt está");
    h.dispatch(UiAction::Dialog {
        id: dialogo.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let despues = siguiente_foto(&mut sub).await;
    assert!(despues.search.is_none(), "no se abrió ninguna búsqueda");
    assert!(
        backend.busquedas.lock().expect("mutex").is_empty(),
        "y nada llegó al wire"
    );
}

/// Un nombre hostil llega a los resultados enmascarado y MARCADO.
#[tokio::test]
async fn un_hallazgo_hostil_va_marcado() {
    let hostil = "mem:///casa/ca%CC%81f%C3%A9%E2%80%AE.txt";
    let backend = arbol_con_hallazgos("*", &[hostil]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    por_la_paleta(&h, &mut sub, "pane.search").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    let dialogo = foto
        .dialogs
        .iter()
        .find(|d| d.input.is_some())
        .expect("el prompt está");
    h.dispatch(UiAction::DialogInput {
        id: dialogo.id,
        text: "*".to_owned(),
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Dialog {
        id: dialogo.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    for _ in 0..20 {
        let Some(v) = siguiente_busqueda(&mut sub).await else {
            continue;
        };
        if let Some(fila) = v.rows.first() {
            assert!(
                !fila.name.contains('\u{202e}'),
                "un override bidi cruzó crudo: {:?}",
                fila.name
            );
            assert!(fila.hostile, "y se MARCA: {fila:?}");
            return;
        }
    }
    panic!("los hallazgos nunca llegaron");
}

// ---------------------------------------------------------------------------
// Búsqueda semántica (tarea 6.1).
// ---------------------------------------------------------------------------

/// La ventana pregunta al índice por SIGNIFICADO, y lo que vuelve se navega
/// como cualquier otro hallazgo.
///
/// El catálogo ataba `pane.semantic-search` desde el keymap y el host
/// contestaba `NotHere`: la capacidad existía en el daemon y en el TUI, y
/// aquí no había por dónde pedirla.
#[tokio::test]
async fn la_ventana_busca_por_significado() {
    let falso = arbol_como_falso();
    *falso.semanticos.lock().expect("semánticos") = Some(vec![
        norte_proto::methods::SemanticHit {
            path: VPath::parse("mem:///casa/docs/a.md").expect("vpath"),
            score: 0.91,
        },
        norte_proto::methods::SemanticHit {
            path: VPath::parse("mem:///casa/notas.txt").expect("vpath"),
            score: 0.42,
        },
    ]);
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.semantic-search").await;
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "facturas del año pasado".to_owned(),
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

    // La vista se abre YA, vacía y corriendo; los hallazgos llegan después.
    let mut vista = siguiente_busqueda(&mut sub)
        .await
        .expect("la búsqueda abre");
    for _ in 0..20 {
        if !vista.rows.is_empty() {
            break;
        }
        vista = siguiente_busqueda(&mut sub).await.expect("sigue abierta");
    }
    assert!(vista.semantic, "la vista dice que esto es semántico");
    assert_eq!(vista.rows.len(), 2);
    assert_eq!(vista.rows[0].name, "a.md");
    // El parecido se ENSEÑA: sin él, dos hallazgos con 0,91 y 0,42 se leen
    // igual de buenos y el orden parece arbitrario.
    assert!(vista.rows[0].score.is_some_and(|s| s > 0.9));
    let pedidas = backend.semanticas_pedidas.lock().expect("pedidas").clone();
    assert_eq!(pedidas.len(), 1);
    assert_eq!(pedidas[0].0, "facturas del año pasado");
    assert!(
        pedidas[0].1 <= norte_proto::methods::INDEX_SEMANTIC_MAX_K,
        "la k va acotada a lo que el daemon acepta: {}",
        pedidas[0].1
    );
}

/// Sin índice, se DICE qué falta y cómo se arregla.
///
/// `NotFound` aquí no es «no hay resultados»: es «este root no tiene filas en
/// el índice», y confundirlo con una búsqueda vacía deja al lector creyendo
/// que no hay nada parecido a lo que buscó.
#[tokio::test]
async fn una_busqueda_semantica_sin_indice_dice_que_falta_construirlo() {
    let falso = arbol_como_falso();
    // Sin `semanticos`: el falso contesta `NotFound`.
    let (h, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.semantic-search").await;
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "lo que sea".to_owned(),
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

    for _ in 0..40 {
        if siguiente_aviso(&mut sub).await == "msg-semantic-no-index" {
            return;
        }
    }
    panic!("nadie dijo que falta construir el índice");
}

/// Una consulta VACÍA no sale del proceso.
#[tokio::test]
async fn una_consulta_semantica_vacia_no_se_manda() {
    let falso = arbol_como_falso();
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "pane.semantic-search").await;
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    asentar().await;
    assert!(
        backend
            .semanticas_pedidas
            .lock()
            .expect("pedidas")
            .is_empty()
    );
}

/// Una instrucción de IA vacía deja el campo DELANTE, como su gemela.
///
/// El terminal deja el modal abierto con el error debajo. La ventana ya se
/// había comido el diálogo y ponía el mensaje en la barra: un «escribe una
/// instrucción» sobre una pantalla sin dónde escribirla no es una negativa,
/// es un callejón. La consulta semántica —el mismo caso, tres ficheros más
/// allá— ya se había arreglado así.
#[tokio::test]
async fn una_instruccion_de_ia_vacia_devuelve_el_campo() {
    let backend = Arc::new(arbol_como_falso());
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "pane.ai-rename").await;
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    asentar().await;

    let foto = foto(&h, &mut sub).await;
    assert!(
        foto.dialogs.iter().any(|d| d.input.is_some()),
        "el campo vuelve: {:?}",
        foto.dialogs
    );
    assert!(
        backend.instrucciones.lock().expect("pedidas").is_empty(),
        "y nada sale hacia el proveedor de IA"
    );
}

/// En SOLO LECTURA no se pregunta: la consulta sale del proceso hacia el
/// proveedor de IA, igual que el plan de renombrado.
#[tokio::test]
async fn en_solo_lectura_no_hay_busqueda_semantica() {
    let falso = arbol_como_falso();
    let backend = Arc::new(falso);
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: Arc::clone(&backend) as Arc<dyn norte_ui_host::backend::HostBackend>,
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset_con(
            "orthodox",
            norte_ui_host::commands::Efectos::SoloLectura,
        )
        .expect("preset"),
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
        effects: norte_ui_host::commands::Efectos::SoloLectura,
        log_ring: None,
    })
    .await
    .expect("arranca");

    let mut sub = h.subscribe();
    h.dispatch(tecla_mod("p", true, false))
        .await
        .expect("host vivo");
    let paleta = siguiente_paleta(&mut sub).await.expect("la paleta abre");
    // La paleta no lleva la clave de despacho —se elige por índice— así que
    // se busca por la etiqueta, que es lo que el lector ve.
    let etiqueta =
        norte_frontend::whichkey::command_label("pane.semantic-search", norte_i18n::Lang::Es);
    assert!(
        !paleta.rows.iter().any(|r| r.text == etiqueta),
        "una ventana sin efectos no ofrece preguntarle a un modelo: {:?}",
        paleta
            .rows
            .iter()
            .map(|r| r.text.clone())
            .collect::<Vec<_>>()
    );
    assert!(
        backend
            .semanticas_pedidas
            .lock()
            .expect("pedidas")
            .is_empty()
    );
}

/// Ejecuta un comando por la PALETA, que es por donde se llega a lo que
/// ningún preset ata (la búsqueda semántica es uno).
pub(super) async fn ejecutar_por_paleta(
    h: &UiHost,
    sub: &mut norte_ui_host::controller::UiSubscription,
    comando: &str,
) {
    let ack = ejecutar_por_paleta_ack(h, sub, comando).await;
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "la paleta no pudo ejecutar `{comando}`: {ack:?}"
    );
}

/// Como [`ejecutar_por_paleta`], pero devolviendo el ACUSE: lo que se
/// comprueba a veces es el rechazo.
pub(super) async fn ejecutar_por_paleta_ack(
    h: &UiHost,
    sub: &mut norte_ui_host::controller::UiSubscription,
    comando: &str,
) -> ActionAck {
    let etiqueta = comando.to_owned();
    h.dispatch(tecla_mod("p", true, false))
        .await
        .expect("host vivo");
    let _ = siguiente_paleta(sub).await;
    for c in etiqueta.chars().skip(5).take(6) {
        h.dispatch(tecla(&c.to_string())).await.expect("host vivo");
    }
    for _ in 0..40 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let p = siguiente_foto(sub)
            .await
            .palette
            .expect("la paleta sigue abierta");
        assert!(
            !p.rows.is_empty(),
            "`{comando}` no sale en la paleta con la consulta `{}`",
            p.query
        );
        let i = p
            .cursor
            .and_then(|c| usize::try_from(c).ok())
            .unwrap_or(0)
            .min(p.rows.len() - 1);
        if p.rows[i].text == etiqueta {
            return h.dispatch(tecla("Enter")).await.expect("host vivo");
        }
        // `ArrowDown`, no `Down`: la paleta acepta el nombre del navegador o
        // el del proyecto en minúscula, y `Down` no es ninguno de los dos —
        // este ayudante llevaba desde la fase 2 funcionando solo cuando el
        // comando buscado caía el PRIMERO.
        h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    }
    panic!("`{comando}` no aparece entre lo que el filtro deja");
}
