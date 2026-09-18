use super::*;

// ---------------------------------------------------------------------------
// El RELEVO a la terminal (fase 9 del programa WOW).
// ---------------------------------------------------------------------------

/// El camino feliz: escribe la pantalla CON las marcas, la suelta, y sólo
/// entonces pide abrir la terminal.
///
/// El orden es lo que se comprueba, y no es cosmético: soltar antes de
/// escribir dejaría a la terminal leyendo la pantalla de hace un segundo, y
/// lanzarla antes de soltar la dejaría abriendo sobre una sesión con dueño.
#[tokio::test]
async fn el_relevo_escribe_suelta_y_entonces_abre_la_terminal() {
    use norte_ui_host::dto::NativeEffect;

    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b"a.txt".to_vec(), false), (b"b.txt".to_vec(), false)],
    );
    f.suelta_la_sesion = true;
    let backend = Arc::new(f);
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let mut nativos = h.native_effects();

    // Marcar una fila: es lo único de la pantalla que no viajaba antes.
    let b = listado(&snap);
    let fila = b
        .rows
        .iter()
        .find(|r| r.display_name == "a.txt")
        .expect("está");
    h.dispatch(UiAction::ToggleMark {
        slot_id: 1,
        key: fila.key,
        generation: b.generation,
    })
    .await
    .expect("host vivo");

    por_la_paleta(&h, &mut sub, "handoff").await;

    let efecto = tokio::time::timeout(std::time::Duration::from_secs(5), nativos.recv())
        .await
        .expect("sale el efecto del relevo")
        .expect("canal vivo");
    assert!(
        matches!(efecto, NativeEffect::HandoffToTerminal { .. }),
        "el efecto es el del relevo: {efecto:?}"
    );

    // Y la pantalla que se escribió lleva la marca, por RUTA.
    let puestas = backend.puestas.lock().expect("puestas").clone();
    let ultima = puestas.last().expect("se escribió algo");
    let marcas = ultima
        .get("slots")
        .and_then(|s| s.get("1"))
        .and_then(|s| s.get("marks"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    assert_eq!(
        marcas,
        serde_json::json!(["mem:///casa/a.txt"]),
        "lo marcado viaja con el relevo, por ruta: {ultima}"
    );
    assert_eq!(
        backend.sueltas.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "se soltó la sesión, una vez"
    );
}

/// Si el daemon dice que esta conexión NO era la dueña, no se lanza nada.
///
/// `released: false` no es un error, es un hecho: la sesión sigue teniendo
/// dueño. Lanzar la terminal entonces la abriría sobre el listado de otro, y
/// esta ventana se habría cerrado por nada.
#[tokio::test]
async fn si_no_se_pudo_soltar_no_se_abre_nada() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    f.suelta_la_sesion = false;
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let mut nativos = h.native_effects();

    por_la_paleta(&h, &mut sub, "handoff").await;
    hasta(&backend, "el intento de soltar", |f| {
        (f.sueltas.load(std::sync::atomic::Ordering::SeqCst) > 0).then_some(())
    })
    .await;
    asentar().await;

    // Nada cruza el canal de efectos nativos.
    assert!(
        nativos.try_recv().is_err(),
        "un relevo que no soltó no abre ninguna terminal"
    );
}
