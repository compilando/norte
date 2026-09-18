use super::*;

/// Arranca el host sobre una sesión guardada que trae `a.txt` MARCADO, con o
/// sin `--attach`, y devuelve la primera foto en la que el listado ya llegó.
async fn arrancar_con_marca(attach: bool) -> norte_ui_host::ViewSnapshot {
    let mut falso = Falso::default();
    falso.pon(
        "mem:///casa",
        vec![(b"a.txt".to_vec(), false), (b"b.txt".to_vec(), false)],
    );
    let mut sesion = crate::base::sesion_guardada(1, 7, 1, "mem:///casa");
    let mut body: norte_frontend::session::SessionBody =
        serde_json::from_value(sesion.body.clone()).expect("cuerpo");
    body.slots.get_mut(&1).expect("hueco").marks =
        vec![VPath::parse("mem:///casa/a.txt").expect("vpath")];
    sesion.body = serde_json::to_value(&body).expect("json");
    *falso.sesion.lock().expect("sesión") = (sesion, true);
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: Arc::new(falso),
        initial_dir: VPath::parse("mem:///casa").expect("vpath"),
        initial_dir_pedido: false,
        attach,
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
    .expect("arranca");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    foto_hasta(&h, &mut sub, "el listado de casa con sus dos filas", |s| {
        (listado(s).rows.len() == 2).then(|| s.clone())
    })
    .await
}

/// La ventana DEVUELVE las marcas de un relevo con `--attach` (fase 9) — y
/// sólo entonces.
///
/// Las escribía en la sesión y no las leía nunca: el relevo desde la terminal
/// abría en el sitio correcto y sin lo señalado, que es justo la mitad que no
/// se rehace con un cd. Y se siembran cuando el LISTADO llega, no al leer la
/// sesión: el listado que aterriza limpia las marcas, la misma trampa que
/// mordió a la terminal.
#[tokio::test]
async fn con_attach_la_ventana_devuelve_las_marcas_del_relevo() {
    // En el montón: el future lleva el `Estado` entero, y en pila pasa del
    // tope de `large_futures` (ver `host_grande` en `payload.rs`).
    let foto = Box::pin(arrancar_con_marca(true)).await;
    let filas = &listado(&foto).rows;
    let a = filas.iter().find(|r| r.display_name == "a.txt").expect("a");
    let b = filas.iter().find(|r| r.display_name == "b.txt").expect("b");
    assert!(a.marked, "la marca del relevo vuelve");
    assert!(!b.marked, "y sólo ésa");
}

/// Sin `--attach` un arranque es un arranque: unas marcas de un relevo que se
/// quedó a medias no resucitan al día siguiente.
#[tokio::test]
async fn sin_attach_las_marcas_guardadas_no_vuelven() {
    let foto = Box::pin(arrancar_con_marca(false)).await;
    assert!(
        listado(&foto).rows.iter().all(|r| !r.marked),
        "un arranque cualquiera no devuelve lo marcado"
    );
}

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
