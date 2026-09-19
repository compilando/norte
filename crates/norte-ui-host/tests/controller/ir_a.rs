use super::*;

// ---------------------------------------------------------------------------
// «Ir a cualquier sitio» en la ventana (#357, fase 6).
// ---------------------------------------------------------------------------

/// Espera la siguiente actualización que traiga «ir a».
async fn siguiente_ir_a(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::GotoView> {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("una actualización antes del plazo")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Goto { goto } = c {
                    return goto.clone();
                }
            }
        }
    }
    panic!("no llegó ninguna actualización con «ir a»");
}

fn ctrl_g() -> UiAction {
    UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "g".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    })
}

/// Las filas que se pintan, sin las cabeceras.
fn filas(v: &norte_ui_host::dto::GotoView) -> Vec<&str> {
    v.lines
        .iter()
        .filter_map(|l| match l {
            norte_ui_host::dto::GotoLineView::Row { text, .. } => Some(text.as_str()),
            norte_ui_host::dto::GotoLineView::Header { .. } => None,
        })
        .collect()
}

/// `ctrl+g` abre «ir a», y teclear una RUTA la ofrece arriba; `Enter` va ahí.
///
/// La ruta tecleada es la única fila que no sale de una lista, y la decisión
/// de qué cuenta como ruta y a dónde lleva es la del modelo compartido: la
/// misma que en la TUI.
#[tokio::test]
async fn una_ruta_tecleada_se_ofrece_y_enter_va_ahi() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    h.dispatch(ctrl_g()).await.expect("host vivo");
    let abierta = siguiente_ir_a(&mut sub).await.expect("«ir a» abre");
    assert!(abierta.query.is_empty(), "arranca sin consulta");

    for c in "mem:///casa/docs".chars() {
        h.dispatch(tecla(&c.to_string())).await.expect("host vivo");
    }
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    let v = foto.goto.expect("sigue abierta");
    assert_eq!(
        filas(&v).first().copied(),
        Some("mem:///casa/docs"),
        "la ruta tecleada va la primera: {:?}",
        v.lines
    );
    assert!(
        matches!(
            v.lines
                .get(usize::try_from(v.cursor.expect("cursor")).expect("índice")),
            Some(norte_ui_host::dto::GotoLineView::Row { .. })
        ),
        "el cursor está en una fila, nunca en una cabecera"
    );

    h.dispatch(tecla("Enter")).await.expect("host vivo");
    for _ in 0..30 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        assert!(foto.goto.is_none(), "«ir a» se cierra al confirmar");
        if listado(&foto).path_display.ends_with("docs") {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("Enter no llevó el panel a la ruta tecleada");
}

/// Un COMANDO se corre desde «ir a» por el mismo camino que una tecla: sus
/// filas son las de la paleta de esta ventana.
#[tokio::test]
async fn un_comando_se_corre_como_su_tecla() {
    let (h, snap) = host_arbol(arbol()).await;
    let cursor_antes = listado(&snap).cursor;
    let mut sub = h.subscribe();
    h.dispatch(ctrl_g()).await.expect("host vivo");
    let _ = siguiente_ir_a(&mut sub).await;
    for c in "cursor.bottom".chars() {
        h.dispatch(tecla(&c.to_string())).await.expect("host vivo");
    }
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let v = siguiente_foto(&mut sub).await.goto.expect("abierta");
    assert!(
        v.lines.iter().any(|l| matches!(
            l,
            norte_ui_host::dto::GotoLineView::Header { title } if title == "Comandos"
        )),
        "los comandos salen con su cabecera: {:?}",
        v.lines
    );
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.goto.is_none(), "se cierra");
    assert_ne!(listado(&foto).cursor, cursor_antes, "y el comando corrió");
}

/// Una RUTA tecleada no se le pregunta al índice semántico, y una palabra sí.
///
/// Mandar `/home/u/secreto` a un proveedor de embeddings —quizá remoto— es
/// mandarle el nombre de un directorio del lector, y una ruta no es una
/// consulta de significado.
#[tokio::test]
async fn una_ruta_tecleada_no_va_al_indice() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(ctrl_g()).await.expect("host vivo");
    let _ = siguiente_ir_a(&mut sub).await;
    for c in "/casa/secreto".chars() {
        h.dispatch(tecla(&c.to_string())).await.expect("host vivo");
    }
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    h.dispatch(ctrl_g()).await.expect("host vivo");
    for c in "facturas".chars() {
        h.dispatch(tecla(&c.to_string())).await.expect("host vivo");
    }
    let pedidas = backend
        .hasta("una pregunta al índice", |f| {
            let p = f.semanticas_pedidas.lock().expect("semánticas").clone();
            (!p.is_empty()).then_some(p)
        })
        .await;
    assert!(
        pedidas.iter().all(|(q, _)| !q.starts_with('/')),
        "ninguna ruta fue al índice: {pedidas:?}"
    );
}

/// `Escape` cierra sin ir a ninguna parte.
#[tokio::test]
async fn escape_cierra_sin_ir_a_ninguna_parte() {
    let (h, snap) = host_arbol(arbol()).await;
    let antes = listado(&snap).path_display.clone();
    let mut sub = h.subscribe();
    h.dispatch(ctrl_g()).await.expect("host vivo");
    let _ = siguiente_ir_a(&mut sub).await;
    h.dispatch(tecla("/")).await.expect("host vivo");
    let _ = siguiente_ir_a(&mut sub).await;
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    // Una foto y no el siguiente parche: las conexiones contestan en
    // segundo plano y su parche puede llegar entre medias.
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.goto.is_none(), "se cierra");
    assert_eq!(listado(&foto).path_display, antes, "y no navegó");
}
