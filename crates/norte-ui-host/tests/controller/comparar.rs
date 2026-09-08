use super::*;

// ---------------------------------------------------------------------------
// Comparar directorios (tarea 6.2).
// ---------------------------------------------------------------------------

/// Espera la siguiente actualización con el panel de diferencias.
pub(super) async fn siguiente_comparacion(
    sub: &mut norte_ui_host::controller::UiSubscription,
) -> Option<norte_ui_host::dto::CompareView> {
    for _ in 0..40 {
        match tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("llega")
            .expect("el host sigue vivo")
        {
            Update::Message(m) => {
                if let UiUpdate::Patch(p) = &m.payload {
                    for c in &p.changes {
                        if let norte_ui_host::dto::ViewChange::Compare { compare } = c {
                            return compare.clone();
                        }
                    }
                }
            }
            Update::Lagged => {}
        }
    }
    panic!("no llegó ninguna actualización con comparación");
}

/// Una fila comparada, con lo mínimo para pintarla.
pub(super) fn fila_comparada(
    id: u64,
    izquierda: Option<&str>,
    derecha: Option<&str>,
    verdict: norte_proto::methods::CompareVerdict,
) -> norte_proto::methods::CompareRow {
    let entrada = |wire: &str| norte_proto::Entry {
        path: VPath::parse(wire).expect("vpath"),
        kind: norte_proto::EntryKind::File,
        size: Some(10),
        mtime_ms: Some(1),
        attrs: std::collections::BTreeMap::new(),
    };
    norte_proto::methods::CompareRow {
        id,
        left: izquierda.map(entrada),
        right: derecha.map(entrada),
        verdict,
        criterion: norte_proto::methods::CompareCriterion::Size,
        confidence: norte_proto::methods::CompareConfidence::Certain,
        newer: None,
        reason: None,
        side: None,
        paired_under: None,
    }
}

/// Comparar los dos paneles abre el panel de diferencias con lo que el core
/// contestó, sin volver a emparejar nada aquí.
#[tokio::test]
async fn comparar_los_dos_paneles_abre_el_panel_de_diferencias() {
    let falso = arbol_como_falso();
    *falso.filas_comparadas.lock().expect("filas") = Some(vec![
        fila_comparada(
            1,
            Some("mem:///casa/notas.txt"),
            Some("mem:///casa/docs/notas.txt"),
            norte_proto::methods::CompareVerdict::Same,
        ),
        fila_comparada(
            2,
            Some("mem:///casa/solo.txt"),
            None,
            norte_proto::methods::CompareVerdict::OnlyLeft,
        ),
    ]);
    let backend = Arc::new(falso);
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separar_los_paneles(&h, &mut sub).await;

    ejecutar_por_paleta(&h, &mut sub, "pane.compare-dirs").await;

    let mut vista = siguiente_comparacion(&mut sub).await.expect("abre");
    for _ in 0..20 {
        if !vista.rows.is_empty() {
            break;
        }
        vista = siguiente_comparacion(&mut sub)
            .await
            .expect("sigue abierta");
    }
    assert_eq!(vista.rows.len(), 2, "{vista:?}");
    assert_eq!(vista.total, 2);
    // Los veredictos y las categorías salen del modelo COMPARTIDO, ya
    // traducidos: el renderer no decide qué es «igual».
    assert_eq!(vista.rows[0].category, "same");
    assert_eq!(vista.rows[1].category, "only-left");
    assert!(
        vista.rows[1].right.is_none(),
        "un huérfano no tiene derecha"
    );
    // Y se pidió comparar los dos directorios de verdad.
    let pedidas = backend.comparaciones.lock().expect("comparaciones").clone();
    assert_eq!(pedidas.len(), 1);
    assert_ne!(pedidas[0].0, pedidas[0].1);
}

/// Un filtro esconde una categoría entera, y NO renumera: la selección sigue
/// nombrando la misma fila.
#[tokio::test]
async fn un_filtro_esconde_una_categoria_y_no_renumera() {
    let falso = arbol_como_falso();
    *falso.filas_comparadas.lock().expect("filas") = Some(vec![
        fila_comparada(
            1,
            Some("mem:///casa/notas.txt"),
            Some("mem:///casa/docs/notas.txt"),
            norte_proto::methods::CompareVerdict::Same,
        ),
        fila_comparada(
            2,
            Some("mem:///casa/solo.txt"),
            None,
            norte_proto::methods::CompareVerdict::OnlyLeft,
        ),
    ]);
    let (h, _snap) = host_con_layout(Arc::new(falso), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separar_los_paneles(&h, &mut sub).await;
    ejecutar_por_paleta(&h, &mut sub, "pane.compare-dirs").await;
    let mut vista = siguiente_comparacion(&mut sub).await.expect("abre");
    for _ in 0..20 {
        if vista.rows.len() == 2 {
            break;
        }
        vista = siguiente_comparacion(&mut sub)
            .await
            .expect("sigue abierta");
    }

    h.dispatch(UiAction::CompareSelectRow { id: 2 })
        .await
        .expect("host vivo");
    h.dispatch(UiAction::CompareToggleFilter {
        category: "same".to_owned(),
    })
    .await
    .expect("host vivo");
    // Hay parches en cola (la selección produjo el suyo): se lee hasta el que
    // ya trae el filtro puesto.
    let mut filtrada = siguiente_comparacion(&mut sub)
        .await
        .expect("sigue abierta");
    for _ in 0..20 {
        if filtrada.rows.len() == 1 {
            break;
        }
        filtrada = siguiente_comparacion(&mut sub)
            .await
            .expect("sigue abierta");
    }
    assert_eq!(filtrada.rows.len(), 1, "la categoría escondida no viaja");
    assert_eq!(
        filtrada.selected,
        Some(2),
        "y la selección sigue siendo suya"
    );
    assert!(
        filtrada
            .filters
            .iter()
            .any(|f| f.id == "same" && f.hidden && f.count == 1),
        "el filtro dice cuántas esconde: {:?}",
        filtrada.filters
    );
}

/// Abrir una fila navega al lado ACTIVO, y una fila cuyo lado activo está
/// vacío no cae al otro lado.
#[tokio::test]
async fn abrir_un_huerfano_por_el_lado_vacio_no_cae_al_otro() {
    let falso = arbol_como_falso();
    *falso.filas_comparadas.lock().expect("filas") = Some(vec![fila_comparada(
        1,
        None,
        Some("mem:///casa/docs/a.md"),
        norte_proto::methods::CompareVerdict::OnlyRight,
    )]);
    let (h, _snap) = host_con_layout(Arc::new(falso), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separar_los_paneles(&h, &mut sub).await;
    ejecutar_por_paleta(&h, &mut sub, "pane.compare-dirs").await;
    let mut vista = siguiente_comparacion(&mut sub).await.expect("abre");
    for _ in 0..20 {
        if !vista.rows.is_empty() {
            break;
        }
        vista = siguiente_comparacion(&mut sub)
            .await
            .expect("sigue abierta");
    }

    // El lado activo es el IZQUIERDO, y esta fila no tiene izquierda.
    let ack = h
        .dispatch(UiAction::CompareActivateRow { id: 1 })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "compare-no-target".to_owned()
        },
        "{ack:?}"
    );
}

/// Deja los dos paneles en directorios DISTINTOS: comparar dos veces el mismo
/// no es una comparación, y el host lo rehúsa antes de encolar nada.
pub(super) async fn separar_los_paneles(
    h: &UiHost,
    sub: &mut norte_ui_host::controller::UiSubscription,
) {
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host vivo");
    // La primera fila es el directorio `docs`: los directorios van primero.
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    for _ in 0..20 {
        let foto = {
            h.dispatch(UiAction::Resync).await.expect("host vivo");
            siguiente_foto(sub).await
        };
        let en_docs = foto.slots.iter().any(|s| match s {
            SlotView::Browser(b) => b.path_display.ends_with("/casa/docs"),
            _ => false,
        });
        if en_docs {
            break;
        }
    }
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host vivo");
}
