use super::*;

// ---------------------------------------------------------------------------
// Esperar sin reloj.
//
// Estos tests hablan con un actor que encola cada mutación con `tokio::spawn`
// y contesta el ack ANTES de que la task corra. Así que el test que quiere ver
// lo encolado tiene que esperar a algo, y durante mucho tiempo ese algo fue un
// `sleep(30)`: una apuesta de reloj de pared que, con 423 tests en paralelo,
// pierde en cuanto la máquina va cargada. Un test intermitente es un bug.
//
// Hay tres preguntas y cada una tiene su herramienta:
//
// - «¿ya pasó X?» → [`hasta`], que espera el AVISO del doble. Cuesta cero en
//   el camino verde y nombra lo que esperaba cuando falla. Su forma corta,
//   para el caso más común, es [`anotados`].
// - «¿seguro que NO pasó nada?» → [`asentar`], que espera a que al ejecutor no
//   le quede trabajo listo. Con el reloj parado eso es el CONTRATO de tokio,
//   no una apuesta sobre el planificador.
// - «¿y cuando el doble no lo ve?» → [`foto_hasta`], que pide fotos hasta que
//   la pantalla lo diga. Es lo que queda para una escritura que vuelve por
//   `spawn_blocking` o un panel que se resiembra.
//
// Para un plazo de VERDAD (el TTL del tablero, un timeout de extensión) no
// vale ninguna de las dos: eso es `#[tokio::test(start_paused = true)]` y
// `tokio::time::advance`, que salta el plazo en vez de esperarlo.
// ---------------------------------------------------------------------------

/// Espera a que el doble ANOTE lo que el test busca. Sin reloj.
///
/// `que_esperaba` es lo que se imprime si no llega: un test que se cuelga
/// tiene que decir qué esperaba, no reventar en la aserción de después.
pub(super) async fn hasta<T>(
    f: &Falso,
    que_esperaba: &str,
    que: impl Fn(&Falso) -> Option<T>,
) -> T {
    f.hasta(que_esperaba, que).await
}

/// Espera a que el doble tenga al menos `n` anotaciones en la lista que se le
/// señala, y devuelve una copia.
///
/// Es la forma corta de [`hasta`] para el caso de lejos más común: «ya se
/// encoló lo que tenía que encolarse». Devuelve el `Vec` clonado y no el
/// `MutexGuard` a propósito: un guard no cruza un `await`.
pub(super) async fn anotados<T: Clone>(
    f: &Falso,
    que_esperaba: &str,
    n: usize,
    campo: impl Fn(&Falso) -> Vec<T>,
) -> Vec<T> {
    hasta(f, que_esperaba, |f| {
        let v = campo(f);
        (v.len() >= n).then_some(v)
    })
    .await
}

/// Repite `Resync` hasta que la foto cumpla lo que se le pide.
///
/// Cada vuelta es un viaje de ida y vuelta al actor, así que el bucle avanza
/// al ritmo del host y no al del reloj. Es lo que hace falta cuando lo que se
/// espera NO lo anota el doble —una escritura en disco que vuelve por
/// `spawn_blocking`, un panel que se resiembra— y por eso [`hasta`] no sirve.
///
/// El plazo es el presupuesto de FALLO, igual que en [`hasta`]: en el camino
/// verde la primera o la segunda foto ya trae lo que se busca.
pub(super) async fn foto_hasta<T>(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    que_esperaba: &str,
    que: impl Fn(&norte_ui_host::ViewSnapshot) -> Option<T>,
) -> T {
    const SOCORRO: std::time::Duration = std::time::Duration::from_secs(15);
    let espera = async {
        loop {
            h.dispatch(UiAction::Resync).await.expect("host vivo");
            if let Some(v) = que(&siguiente_foto(sub).await) {
                return v;
            }
        }
    };
    let Ok(v) = tokio::time::timeout(SOCORRO, espera).await else {
        panic!("la pantalla nunca llegó a: {que_esperaba}")
    };
    v
}

/// Espera a que al ejecutor NO le quede trabajo listo.
///
/// Es la respuesta a «no se encoló nada»: ahí no hay evento que esperar, así
/// que lo que hay que garantizar es que las tasks que el actor pudiera haber
/// lanzado antes de contestar el ack ya han corrido. El hueco es estrecho y
/// concreto: el actor valida, hace `tokio::spawn` y CONTESTA el ack; el doble
/// anota al entrar en el método, pero ese método solo se llama cuando la task
/// lanzada recibe su primer poll.
///
/// **Con el reloj PARADO, tokio solo adelanta el tiempo cuando no le queda
/// nada que correr.** O sea que dormir un instante virtual es exactamente
/// «espera a que el ejecutor se quede sin trabajo»: cuando esto vuelve, toda
/// task lanzada antes ha sido sondeada al menos una vez y está terminada o
/// esperando algo. No cuesta tiempo real y no adivina nada.
///
/// Antes eran 32 `yield_now()`, y eso era una apuesta con otro nombre: la
/// documentación de tokio dice que `yield_now` puede volver a sondear la misma
/// task inmediatamente, así que «32 cesiones» no garantizaba que las demás
/// hubieran avanzado. Treinta y tres de estas comprobaciones negativas
/// dependían solo de eso.
///
/// El plazo es de socorro, no de espera: si el ejecutor nunca se queda quieto
/// —una task que gira— esto lo dice en vez de colgarse para siempre.
pub(super) async fn asentar() {
    const SOCORRO: std::time::Duration = std::time::Duration::from_secs(15);
    let quieto = async {
        tokio::time::pause();
        // Un instante VIRTUAL: el auto-avance del reloj parado no ocurre
        // hasta que el ejecutor está ocioso, que es justo lo que se espera.
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        tokio::time::resume();
    };
    assert!(
        tokio::time::timeout(SOCORRO, quieto).await.is_ok(),
        "el ejecutor nunca se quedó sin trabajo: hay una task que gira"
    );
}

/// Un nombre que no es UTF-8 cruza el bridge MARCADO y con el reemplazo
/// canónico: ni se rechaza la entrada ni se pierde el aviso.
#[tokio::test]
async fn un_nombre_no_utf8_llega_marcado() {
    let (_h, snap) = host_arbol(arbol()).await;
    let hostil = listado(&snap)
        .rows
        .iter()
        .find(|r| r.hostile)
        .expect("la fila hostil llega");
    assert!(
        hostil.display_name.contains('\u{FFFD}'),
        "el nombre pintado lleva el reemplazo canónico: {:?}",
        hostil.display_name
    );
}

/// El orden es el COMPARTIDO: el mismo que produce `PaneState` para las
/// mismas entradas, no uno del host.
#[tokio::test]
async fn el_orden_es_el_de_la_capa_compartida() {
    let (_h, snap) = host_arbol(arbol()).await;
    let nombres: Vec<&str> = listado(&snap)
        .rows
        .iter()
        .map(|r| r.display_name.as_str())
        .collect();
    // Directorios primero, y dentro de cada grupo por nombre: es la regla de
    // `norte_frontend::sort`, y aquí solo se comprueba que el host no la
    // reimplementa.
    assert_eq!(nombres[0], "docs");
    assert!(nombres.contains(&"notas.txt"));
}

/// Entrar en un directorio navega y trae su listado.
#[tokio::test]
async fn activar_un_directorio_navega() {
    let (h, snap) = host_arbol(arbol()).await;
    let docs = listado(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("el directorio está");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs.key,
        generation: listado(&snap).generation,
    })
    .await
    .expect("host vivo");

    let despues = siguiente_foto(&mut sub).await;
    let b = listado(&despues);
    assert!(b.path_display.ends_with("/casa/docs"), "{}", b.path_display);
    assert_eq!(b.rows.len(), 2);
    assert_ne!(
        b.generation,
        listado(&snap).generation,
        "otro listado, otra generación: las claves viejas caducan"
    );
}

/// Subir deja el cursor en el directorio del que se sale, que es lo que hace
/// reversible bajar y subir.
#[tokio::test]
async fn subir_devuelve_el_cursor_al_directorio_de_origen() {
    let (h, snap) = host_arbol(arbol()).await;
    let docs = listado(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("el directorio está");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs.key,
        generation: listado(&snap).generation,
    })
    .await
    .expect("host vivo");
    siguiente_foto(&mut sub).await;

    h.dispatch(UiAction::Parent { slot_id: 1 })
        .await
        .expect("host vivo");
    let arriba = siguiente_foto(&mut sub).await;
    let b = listado(&arriba);
    let bajo_cursor = b
        .rows
        .iter()
        .find(|r| Some(r.key) == b.cursor)
        .expect("hay cursor");
    assert_eq!(
        bajo_cursor.display_name, "docs",
        "el cursor vuelve al directorio del que se salió"
    );
}

/// Atrás y adelante recorren el rastro, y el rastro agotado lo DICE: una
/// tecla muda no se distingue de una rota.
#[tokio::test]
async fn el_rastro_va_y_vuelve_y_cuando_se_acaba_lo_dice() {
    let (h, snap) = host_arbol(arbol()).await;
    let docs = listado(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("el directorio está");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs.key,
        generation: listado(&snap).generation,
    })
    .await
    .expect("host vivo");
    siguiente_foto(&mut sub).await;

    h.dispatch(UiAction::History {
        slot_id: 1,
        back: true,
    })
    .await
    .expect("host vivo");
    let atras = siguiente_foto(&mut sub).await;
    assert!(listado(&atras).path_display.ends_with("/casa"));

    h.dispatch(UiAction::History {
        slot_id: 1,
        back: false,
    })
    .await
    .expect("host vivo");
    let adelante = siguiente_foto(&mut sub).await;
    assert!(listado(&adelante).path_display.ends_with("/casa/docs"));

    let ack = h
        .dispatch(UiAction::History {
            slot_id: 1,
            back: false,
        })
        .await
        .expect("host vivo");
    match ack {
        ActionAck::Unavailable { reason_key } => {
            assert_eq!(reason_key, "msg-nav-no-forward");
        }
        otro => panic!("el rastro agotado se dice: {otro:?}"),
    }
}

/// Una respuesta que llega TARDE, cuando otra navegación ya la relevó, se
/// descarta en Rust. Sin esto, el listado del directorio abandonado
/// aparecería encima del actual.
#[tokio::test]
async fn una_respuesta_tardia_no_pisa_la_navegacion_nueva() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"docs".to_vec(), true)]);
    f.pon("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    f.retraso_ms = 60;
    let backend = Arc::new(f);
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let docs = listado(&snap).rows[0].key;
    let mut sub = h.subscribe();

    // Entrar en `docs` y, sin esperar, volver a casa: la primera respuesta
    // llegará cuando el hueco ya esté en otra navegación.
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs,
        generation: listado(&snap).generation,
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::History {
        slot_id: 1,
        back: true,
    })
    .await
    .expect("host vivo");

    let foto = siguiente_foto(&mut sub).await;
    assert!(
        listado(&foto).path_display.ends_with("/casa"),
        "la última navegación es la que manda: {}",
        listado(&foto).path_display
    );
    // Y la tardía no produce una segunda foto con el directorio abandonado.
    // Se espera a que las TRES respuestas hayan vuelto —la relevada incluida,
    // que es la que podría pisar— y solo entonces se mira el canal.
    hasta(&backend, "que no vuele ningún listado", |f| {
        (f.listados() == 3 && f.en_calma()).then_some(())
    })
    .await;
    asentar().await;
    let mas = tokio::time::timeout(std::time::Duration::ZERO, sub.recv()).await;
    assert!(mas.is_err(), "la respuesta relevada no llega a la pantalla");
    assert_eq!(backend.listados(), 3, "inicial + docs + vuelta");
}

/// Mover el cursor manda el cursor, no el listado entero.
#[tokio::test]
async fn mover_el_cursor_no_reenvia_las_filas() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::MoveCursor {
        slot_id: 1,
        delta: 1,
    })
    .await
    .expect("host vivo");
    let Update::Message(m) = sub.recv().await.expect("llega") else {
        panic!("sin retraso");
    };
    let UiUpdate::Patch(p) = &m.payload else {
        panic!("un parche");
    };
    assert!(
        matches!(p.changes[0], norte_ui_host::dto::ViewChange::Cursor { .. }),
        "solo el cursor viaja: {:?}",
        p.changes[0]
    );
}

/// El TOTAL llega sin pedir una foto: es la altura del scroll del renderer.
///
/// `total_rows` solo viajaba en la foto entera, y el drenaje paginado contesta
/// con parches de filas —también el ÚLTIMO lote—. Así que el renderer se
/// quedaba con el total de la PRIMERA PÁGINA (100) para siempre: pinta el
/// canvas de scroll a `total * alto_de_celda` y publica `aria-rowcount`, o sea
/// que un directorio de cinco mil ficheros quedaba topado en la fila 100 para
/// la rueda, y no había forma de pedir el resto porque el rango visible se
/// calcula del scroll.
///
/// El test de al lado no lo veía porque pide `Resync` en cada vuelta, que es
/// justo lo que el renderer de verdad NO hace: solo resincroniza tras un hueco
/// de secuencia o un `Lagged`.
#[tokio::test]
async fn el_total_de_un_listado_grande_llega_sin_pedir_foto() {
    let mut falso = Falso::default();
    let muchas: Vec<(Vec<u8>, bool)> = (0..5_000u32)
        .map(|i| (format!("f{i:05}").into_bytes(), false))
        .collect();
    falso.pon("mem:///casa", muchas);
    let (host, snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = host.subscribe();
    assert!(
        listado(&snap).total_rows.expect("hay total") <= 100,
        "de partida, solo la primera página"
    );

    // SIN `Resync`: solo lo que el host manda por su cuenta mientras drena.
    let mut ultimo_total = None;
    let espera = async {
        loop {
            match sub.recv().await.expect("host vivo") {
                Update::Message(m) => match m.payload {
                    UiUpdate::Patch(p) => {
                        for c in &p.changes {
                            if let norte_ui_host::dto::ViewChange::Rows { total_rows, .. } = c {
                                ultimo_total = *total_rows;
                            }
                        }
                    }
                    UiUpdate::Snapshot(s) => {
                        ultimo_total = listado(&s).total_rows;
                    }
                    UiUpdate::Notice(_) => {}
                },
                // Quedarse atrás es «pide una foto», y el renderer la pide.
                // No cuenta como que el total llegara solo.
                Update::Lagged => {}
            }
            if ultimo_total == Some(5_000) {
                return;
            }
        }
    };
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(20), espera)
            .await
            .is_ok(),
        "el renderer nunca se entera de que hay 5.000 filas: se queda topado \
         en {ultimo_total:?} y no puede desplazarse más abajo"
    );
}

/// Un listado grande: la primera página se pinta enseguida, el resto llega
/// por detrás, y del total solo cruzan las filas visibles.
#[tokio::test]
async fn un_listado_grande_ni_espera_ni_cruza_entero() {
    let mut falso = Falso::default();
    let muchas: Vec<(Vec<u8>, bool)> = (0..5_000u32)
        .map(|i| (format!("f{i:05}").into_bytes(), false))
        .collect();
    falso.pon("mem:///casa", muchas);
    let (host, snap) = host_arbol(Arc::new(falso)).await;

    // La primera foto NO espera al listado entero.
    let primeras = listado(&snap).total_rows.expect("hay total");
    assert!(
        primeras <= 100,
        "la primera página se pinta sin esperar al resto: {primeras}"
    );

    // El resto llega por detrás. Se sondea, en vez de contar mensajes: los
    // lotes son asíncronos y el número exacto no es el contrato. Lo que NO
    // hace falta es un reloj: cada vuelta es un viaje de ida y vuelta al
    // actor, así que el bucle avanza al ritmo del drenaje, no al del reloj.
    let mut sub = host.subscribe();
    let mut total = primeras;
    for _ in 0..2_000 {
        if total >= 5_000 {
            break;
        }
        host.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        total = listado(&foto).total_rows.expect("hay total");
    }
    assert_eq!(total, 5_000, "acaba entero");

    // Y de las cinco mil, cruzan cuarenta.
    host.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 2_000,
        count: 40,
    })
    .await
    .expect("host vivo");
    host.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(listado(&foto).rows.len(), 40);
}

/// Una tecla CON modificadores.
pub(super) fn tecla_mod(k: &str, ctrl: bool, shift: bool) -> UiAction {
    UiAction::Key(norte_ui_host::keys::KeyInput {
        key: k.to_owned(),
        ctrl,
        alt: false,
        shift,
        meta: false,
    })
}

pub(super) fn tecla(k: &str) -> UiAction {
    UiAction::Key(norte_ui_host::keys::KeyInput {
        key: k.to_owned(),
        ctrl: false,
        alt: false,
        shift: false,
        meta: false,
    })
}

/// `alt+<algo>`: el modificador va en su campo, jamás en el nombre de la
/// tecla — `"Alt+o"` no es un nombre de tecla, `to_chord` lo rechaza y el host
/// contesta `Unavailable` sin mandar nada. Un test escrito así se cumplía o no
/// según qué sobre quedara en la cola.
pub(super) fn tecla_alt(k: &str) -> UiAction {
    UiAction::Key(norte_ui_host::keys::KeyInput {
        key: k.to_owned(),
        ctrl: false,
        alt: true,
        shift: false,
        meta: false,
    })
}

/// Una tecla del preset resuelve al comando del CATÁLOGO compartido y el
/// host solo la ejecuta: no hay un segundo keymap.
#[tokio::test]
async fn una_tecla_del_preset_mueve_el_cursor() {
    let (h, snap) = host_arbol(arbol()).await;
    let antes = listado(&snap).cursor;
    let mut sub = h.subscribe();
    let ack = h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    assert!(matches!(ack, ActionAck::Applied { .. }));
    let Update::Message(m) = sub.recv().await.expect("llega") else {
        panic!("sin retraso");
    };
    let UiUpdate::Patch(p) = &m.payload else {
        panic!("un parche");
    };
    match &p.changes[0] {
        norte_ui_host::dto::ViewChange::Cursor { cursor, .. } => {
            assert_ne!(*cursor, antes, "el cursor se movió");
        }
        otro => panic!("se esperaba el cursor: {otro:?}"),
    }
}

/// El contador lo resuelve Rust, no el renderer: `3` y luego `j` baja tres.
#[tokio::test]
async fn el_contador_lo_resuelve_el_host() {
    let mut falso = Falso::default();
    let nombres: Vec<(Vec<u8>, bool)> = (0..10u32)
        .map(|n| (format!("f{n}").into_bytes(), false))
        .collect();
    falso.pon("mem:///casa", nombres);
    let backend = Arc::new(falso);
    let (host, _snap) = UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        // `vim` es el preset que habilita contadores.
        keymap: norte_ui_host::keys::keymap_de_preset("vim").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("vim").expect("preset"),
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

    // Tecleando el contador, el host lo PINTA: lo que no se ve no se puede
    // cancelar.
    let mut sub = host.subscribe();
    host.dispatch(tecla("3")).await.expect("host vivo");
    let Update::Message(m) = sub.recv().await.expect("llega") else {
        panic!("sin retraso");
    };
    let UiUpdate::Patch(p) = &m.payload else {
        panic!("un parche");
    };
    match &p.changes[0] {
        norte_ui_host::dto::ViewChange::Status(s) => {
            assert_eq!(s.pending.as_ref().and_then(|p| p.count), Some(3));
        }
        otro => panic!("se esperaba el estado: {otro:?}"),
    }

    host.dispatch(tecla("j")).await.expect("host vivo");
    host.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(
        listado(&foto).cursor,
        Some(norte_ui_host::RowKey(3)),
        "tres filas, no una"
    );
}

/// Una tecla ligada a un comando que este host no implementa NO ejecuta
/// nada, y lo dice con la misma frase que el TUI.
#[tokio::test]
async fn un_comando_que_el_host_no_hace_no_dispara_nada() {
    // El ejemplo fue rotando según la ventana se acercaba a la paridad —`F5`
    // hasta copiar, `F4` hasta editar (#290), `alt+t` hasta el árbol, `alt+q`
    // hasta el visor acoplado (#291), `alt+r` hasta el lote (#310), `alt+C`
    // hasta comparar (#312)— y se acabaron: la ventana hace todo lo que el
    // catálogo tiene vivo. Lo que queda es lo que NO APLICA a una ventana
    // (`tests/paridad.rs`), y `ctrl+o` en el preset `norton` está ligado a
    // uno de esos, `app.toggle-panels`: esconder los paneles para ver el
    // terminal de detrás no significa nada en una ventana.
    let (h, snap) = UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("norton").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("norton").expect("preset"),
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
    let antes = listado(&snap).clone();
    let ack = h
        .dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
            key: "o".to_owned(),
            ctrl: true,
            alt: false,
            shift: false,
            meta: false,
        }))
        .await
        .expect("host vivo");
    match ack {
        ActionAck::Unavailable { reason_key } => assert_eq!(reason_key, "cmd-not-here"),
        otro => panic!("se esperaba no disponible: {otro:?}"),
    }
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    let ahora = listado(&foto);
    assert_eq!(ahora.generation, antes.generation, "nada cambió");
    assert!(
        foto.status.message.is_some(),
        "y la barra lo dice en vez de callarse"
    );
}

/// Una tecla sin binding se descarta dejando el estado limpio: ni ejecuta
/// nada ni deja un prefijo colgando.
#[tokio::test]
async fn una_tecla_sin_binding_se_descarta() {
    let (h, _snap) = host_arbol(arbol()).await;
    // `Insert` no está ligada en el preset ortodoxo.
    let ack = h.dispatch(tecla("Insert")).await.expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "una tecla suelta no es un error: {ack:?}"
    );
}

/// Y una tecla que el adaptador no entiende tampoco se adivina.
#[tokio::test]
async fn una_tecla_que_no_se_entiende_no_se_inventa() {
    let (h, _snap) = host_arbol(arbol()).await;
    let ack = h.dispatch(tecla("Compose")).await.expect("host vivo");
    match ack {
        ActionAck::Unavailable { reason_key } => assert_eq!(reason_key, "host-key-unmapped"),
        otro => panic!("se esperaba no disponible: {otro:?}"),
    }
}

/// Arranca con una disposición concreta y un tamaño concreto.
pub(super) async fn host_con_layout(
    backend: Arc<Falso>,
    layout: &str,
    viewport: (u16, u16),
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree(layout).expect("layout"),
        viewport,
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

/// Arranca con un ÁRBOL dado: el de una sesión real, para reproducir lo que
/// alguien vio.
pub(super) async fn host_con_arbol(
    backend: Arc<Falso>,
    layout: norte_frontend::layout::Node,
    viewport: (u16, u16),
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout,
        viewport,
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

/// Las cinco disposiciones de fábrica resuelven a tamaños razonables, y
/// ninguna deja una pantalla sin listado.
#[tokio::test]
async fn los_cinco_presets_de_disposicion_resuelven() {
    for nombre in norte_frontend::layout::presets::NAMES {
        for viewport in [(80u16, 24u16), (120, 40), (200, 60)] {
            let (_h, snap) = host_con_layout(arbol(), nombre, viewport).await;
            let listados = snap
                .slots
                .iter()
                .filter(|s| matches!(s, SlotView::Browser(_)))
                .count();
            assert!(
                listados >= 1,
                "{nombre} a {viewport:?} se quedó sin listado usable"
            );
        }
    }
}

/// Redimensionar reparte otra vez y NO reescribe la disposición: un layout
/// guardado es la intención del usuario, no una función del tamaño de su
/// ventana.
#[tokio::test]
async fn redimensionar_no_reescribe_la_disposicion() {
    let (h, grande) = host_con_layout(arbol(), "orthodox", (200, 60)).await;
    let listados_antes = grande
        .slots
        .iter()
        .filter(|s| matches!(s, SlotView::Browser(_)))
        .count();
    assert_eq!(listados_antes, 2, "ortodoxo tiene dos listados");

    let mut sub = h.subscribe();
    h.dispatch(UiAction::SetViewport {
        width: 30,
        height: 10,
    })
    .await
    .expect("host vivo");
    let apretado = siguiente_foto(&mut sub).await;
    assert!(
        apretado
            .slots
            .iter()
            .any(|s| matches!(s, SlotView::Browser(_))),
        "aunque no quepan los dos, queda un listado"
    );

    // Y al volver al tamaño de antes, vuelven los dos: el árbol no se tocó.
    h.dispatch(UiAction::SetViewport {
        width: 200,
        height: 60,
    })
    .await
    .expect("host vivo");
    let otra_vez = siguiente_foto(&mut sub).await;
    let listados = otra_vez
        .slots
        .iter()
        .filter(|s| matches!(s, SlotView::Browser(_)))
        .count();
    assert_eq!(listados, 2, "la disposición sobrevivió al apretón");
}

/// Un hueco de un kind que este host aún no proyecta viaja en gris y con su
/// nombre: preservar lo que no se entiende es la regla, y desaparecer sería
/// peor que estar apagado.
#[tokio::test]
async fn un_kind_desconocido_viaja_apagado_y_con_nombre() {
    let (_h, snap) = host_con_layout(arbol(), "simple", (120, 40)).await;
    let nombres: Vec<&str> = snap
        .slots
        .iter()
        .filter_map(|s| match s {
            SlotView::Unsupported { kind_name, .. } => Some(kind_name.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        nombres.contains(&"tasks") && nombres.contains(&"status"),
        "los kinds que el host no proyecta siguen ahí: {nombres:?}"
    );
}

/// El foco cambia de hueco, y con él el destino: el destino es SIEMPRE otro
/// listado visible, jamás el mismo que tiene el foco.
#[tokio::test]
async fn el_foco_cambia_y_el_destino_lo_sigue() {
    let (h, snap) = host_con_layout(arbol(), "orthodox", (200, 60)).await;
    assert_eq!(snap.focus, Some(1));
    let mut sub = h.subscribe();
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host vivo");
    // Cambiar de foco NO reenvía la pantalla: viaja el reparto con los
    // papeles nuevos, que es lo único que cambió.
    let despues = siguiente_disposicion(&mut sub).await;
    let rol = |id: u32| {
        despues
            .placements
            .iter()
            .find(|p| p.slot_id == id)
            .and_then(|p| p.role)
    };
    assert_eq!(rol(2), Some(norte_ui_host::dto::SlotRole::Active));
    assert_eq!(rol(1), Some(norte_ui_host::dto::SlotRole::Target));
}

/// Enfocar un hueco que no se ve es una carrera con un reparto anterior, no
/// una orden.
#[tokio::test]
async fn no_se_puede_enfocar_lo_que_no_se_ve() {
    let (h, _snap) = host_con_layout(arbol(), "orthodox", (200, 60)).await;
    let ack = h
        .dispatch(UiAction::FocusSlot { slot_id: 99 })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Generation
        }
    );
}

/// Lo que no se ve no se trae: un reparto que oculta un listado no le pide
/// su directorio al daemon.
#[tokio::test]
async fn un_hueco_oculto_no_pide_listado() {
    let backend = arbol();
    // A lo ancho caben los dos listados de `orthodox`; a 30 columnas, no.
    let (_h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (30, 10)).await;
    assert_eq!(
        backend.listados(),
        1,
        "solo el listado que se ve pide su directorio"
    );
}

/// Construye una sesión guardada con un hueco en `dir`.
pub(super) fn sesion_guardada(
    version: u32,
    revision: u64,
    slot: u32,
    wire: &str,
) -> norte_proto::methods::Session {
    let mut body = norte_frontend::session::SessionBody::default();
    body.slots.insert(
        slot,
        norte_frontend::session::SlotState {
            path: VPath::parse(wire).expect("vpath"),
            cursor: 0,
            back: Vec::new(),
            forward: Vec::new(),
            jump: None,
            sort: norte_frontend::SortSpec::default(),
            columns: Vec::new(),
            show_hidden: false,
            touched_ms: 0,
            marks: Vec::new(),
        },
    );
    norte_proto::methods::Session {
        version,
        revision,
        body: serde_json::to_value(&body).expect("json"),
    }
}

/// La sesión dice dónde estaba cada hueco, y el host arranca ahí.
#[tokio::test]
async fn la_sesion_coloca_los_huecos() {
    let mut falso = Falso::default();
    falso.pon("mem:///casa", vec![(b"a".to_vec(), false)]);
    falso.pon("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    *falso.sesion.lock().expect("sesión") = (sesion_guardada(1, 7, 1, "mem:///casa/docs"), true);
    let (_h, snap) = host_arbol(Arc::new(falso)).await;
    assert!(
        listado(&snap).path_display.ends_with("/casa/docs"),
        "arrancó donde lo dejó la sesión: {}",
        listado(&snap).path_display
    );
}

/// La ventana devuelve el CURSOR que guardó la sesión, como la terminal.
///
/// Se vio en el primer relevo con una persona delante: la terminal tenía el
/// cursor en `c.txt` y la ventana abrió en `/..`. Guardaba el cursor y nunca lo
/// leía —sitio, orden, ocultos e historia sí—, así que «sigue donde estabas»
/// se cumplía a medias. Mismo índice que usa la terminal (`restore_cursor`),
/// para que un relevo caiga en la misma fila en los dos sentidos.
#[tokio::test]
async fn la_sesion_devuelve_el_cursor() {
    let mut falso = Falso::default();
    falso.pon(
        "mem:///casa",
        vec![
            (b"a.txt".to_vec(), false),
            (b"b.txt".to_vec(), false),
            (b"c.txt".to_vec(), false),
        ],
    );
    let mut sesion = sesion_guardada(1, 7, 1, "mem:///casa");
    let mut body: norte_frontend::session::SessionBody =
        serde_json::from_value(sesion.body.clone()).expect("cuerpo");
    body.slots.get_mut(&1).expect("hueco").cursor = 2;
    sesion.body = serde_json::to_value(&body).expect("json");
    *falso.sesion.lock().expect("sesión") = (sesion, true);
    let (h, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let (tercera, bajo_el_cursor) = foto_hasta(&h, &mut sub, "el listado con sus filas", |s| {
        let b = listado(s);
        let tercera = b.rows.get(2)?.display_name.clone();
        let cursor = b.cursor?;
        let bajo = b
            .rows
            .iter()
            .find(|r| r.key == cursor)?
            .display_name
            .clone();
        Some((tercera, bajo))
    })
    .await;
    assert_eq!(
        bajo_el_cursor, tercera,
        "el cursor vuelve a la fila que guardó"
    );
}

/// Un directorio tecleado gana a la sesión, y entonces el cursor guardado NO
/// vale: era una fila de OTRO directorio. La misma regla que `pin_start_dir`
/// en la terminal.
#[tokio::test]
async fn con_dir_tecleado_el_cursor_guardado_no_vale() {
    let mut falso = Falso::default();
    falso.pon(
        "mem:///casa",
        vec![(b"a".to_vec(), false), (b"b".to_vec(), false)],
    );
    falso.pon(
        "mem:///casa/docs",
        vec![
            (b"x.md".to_vec(), false),
            (b"y.md".to_vec(), false),
            (b"z.md".to_vec(), false),
        ],
    );
    let mut sesion = sesion_guardada(1, 7, 1, "mem:///casa/docs");
    let mut body: norte_frontend::session::SessionBody =
        serde_json::from_value(sesion.body.clone()).expect("cuerpo");
    body.slots.get_mut(&1).expect("hueco").cursor = 2;
    sesion.body = serde_json::to_value(&body).expect("json");
    *falso.sesion.lock().expect("sesión") = (sesion, true);
    let (h, _snap) = Box::pin(UiHost::start(UiHostOptions {
        backend: Arc::new(falso),
        initial_dir: VPath::parse("mem:///casa").expect("vpath"),
        initial_dir_pedido: true,
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
    }))
    .await
    .expect("arranca");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let (primera, bajo_el_cursor) = foto_hasta(&h, &mut sub, "el listado tecleado", |s| {
        let b = listado(s);
        if !b.path_display.ends_with("/casa") {
            return None;
        }
        let primera = b.rows.first()?.display_name.clone();
        let cursor = b.cursor?;
        let bajo = b
            .rows
            .iter()
            .find(|r| r.key == cursor)?
            .display_name
            .clone();
        Some((primera, bajo))
    })
    .await;
    assert_eq!(
        bajo_el_cursor, primera,
        "el cursor de `docs` no se aplica sobre `casa`"
    );
}

/// Arranca como [`host_arbol`], con `[profile.start]` puesto.
pub(super) async fn host_con_start(
    backend: Arc<Falso>,
    start: &[(u32, &str)],
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    let mut settings = ajustes_de_prueba();
    settings.common.profile_start = start
        .iter()
        .map(|(id, wire)| (*id, VPath::parse(wire).expect("vpath")))
        .collect();
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings,
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

/// `[profile.start]` abre el hueco del que la sesión no sabe nada.
///
/// Es lo que hace útil un perfil recién creado, o uno que llega de otra
/// máquina: la clave la escribían los dos frontends y no la leía NINGUNO, así
/// que entrar en un perfil dejaba los paneles donde estaban y el perfil solo
/// cambiaba los colores. Dos ficheros prometían que sí.
#[tokio::test]
async fn profile_start_siembra_un_hueco_sin_sesion() {
    let mut falso = Falso::default();
    falso.pon("mem:///casa", vec![(b"a".to_vec(), false)]);
    falso.pon("mem:///casa/fotos", vec![(b"gato.png".to_vec(), false)]);
    // Sesión legible y VACÍA: nadie ha guardado el hueco 1 todavía.
    *falso.sesion.lock().expect("sesión") = (
        norte_proto::methods::Session {
            version: 1,
            revision: 7,
            body: serde_json::to_value(norte_frontend::session::SessionBody::default())
                .expect("json"),
        },
        true,
    );
    let (_h, snap) = host_con_start(Arc::new(falso), &[(1, "mem:///casa/fotos")]).await;
    assert!(
        listado(&snap).path_display.ends_with("/casa/fotos"),
        "abrió donde dice el perfil: {}",
        listado(&snap).path_display
    );
}

/// Y la SESIÓN gana: `[profile.start]` dice dónde abre un hueco la primera
/// vez, no cada vez.
///
/// Un perfil es un espacio de trabajo, no un marcador que te devuelve al
/// principio: si cada entrada al perfil te sacara de donde estabas, el perfil
/// sería inservible justo para quien lo usa a diario.
#[tokio::test]
async fn la_sesion_gana_a_profile_start() {
    let mut falso = Falso::default();
    falso.pon("mem:///casa", vec![(b"a".to_vec(), false)]);
    falso.pon("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    falso.pon("mem:///casa/fotos", vec![(b"gato.png".to_vec(), false)]);
    *falso.sesion.lock().expect("sesión") = (sesion_guardada(1, 7, 1, "mem:///casa/docs"), true);
    let (_h, snap) = host_con_start(Arc::new(falso), &[(1, "mem:///casa/fotos")]).await;
    assert!(
        listado(&snap).path_display.ends_with("/casa/docs"),
        "manda dónde lo dejaste, no dónde nace el perfil: {}",
        listado(&snap).path_display
    );
}

/// Un directorio ESCRITO en la línea de órdenes gana a la sesión.
///
/// `norte-gui /usr/bin` con una sesión guardada abría donde estuvieras ayer y
/// se comía el argumento sin decir nada: `aplicar_sesion` escribe el dir de
/// TODOS los huecos, y no había nada que dijera «éste lo acaba de teclear un
/// humano». El terminal cerró lo mismo en `eb237c61` con `pin_start_dir`, y a
/// la ventana no llegó.
///
/// Gana en el panel ACTIVO y solo ahí: el otro sigue donde la sesión lo dejó,
/// que es media pantalla de memoria que nadie pidió tirar.
#[tokio::test]
async fn el_dir_de_la_linea_de_ordenes_gana_a_la_sesion() {
    let mut falso = Falso::default();
    falso.pon("mem:///casa", vec![(b"a".to_vec(), false)]);
    falso.pon("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    *falso.sesion.lock().expect("sesión") = (sesion_guardada(1, 7, 1, "mem:///casa/docs"), true);
    let (_h, snap) = UiHost::start(UiHostOptions {
        backend: Arc::new(falso),
        // Lo que el humano tecleó, que NO es donde lo dejó la sesión.
        initial_dir: VPath::parse("mem:///casa").expect("vpath"),
        initial_dir_pedido: true,
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
    .expect("arranca");
    assert!(
        listado(&snap).path_display.ends_with("/casa"),
        "manda lo que se tecleó, no lo que guardó la sesión: {}",
        listado(&snap).path_display
    );
}

/// Y sin argumento, la sesión sigue mandando: es lo de siempre.
#[tokio::test]
async fn sin_argumento_la_sesion_sigue_mandando() {
    let mut falso = Falso::default();
    falso.pon("mem:///casa", vec![(b"a".to_vec(), false)]);
    falso.pon("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    *falso.sesion.lock().expect("sesión") = (sesion_guardada(1, 7, 1, "mem:///casa/docs"), true);
    let (_h, snap) = host_arbol(Arc::new(falso)).await;
    assert!(
        listado(&snap).path_display.ends_with("/casa/docs"),
        "sin nada tecleado, donde lo dejaste: {}",
        listado(&snap).path_display
    );
}

/// Una sesión de un esquema MÁS NUEVO no se aplica y —sobre todo— no se
/// sobrescribe: arrancar sin sesión es recuperable, machacar la de una
/// versión futura no.
#[tokio::test]
async fn una_sesion_del_futuro_ni_se_aplica_ni_se_pisa() {
    let mut falso = Falso::default();
    falso.pon("mem:///casa", vec![(b"a".to_vec(), false)]);
    *falso.sesion.lock().expect("sesión") = (
        sesion_guardada(
            norte_frontend::session::SCHEMA_VERSION + 1,
            7,
            1,
            "mem:///casa/docs",
        ),
        true,
    );
    let backend = Arc::new(falso);
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    assert!(
        listado(&snap).path_display.ends_with("/casa"),
        "se arranca de la configuración, no de lo que no se entiende"
    );
    h.shutdown().await.expect("apaga");
    assert!(
        backend.escrito.lock().expect("escrito").is_none(),
        "y no se escribe encima"
    );
}

/// Una ventana SUELTA no escribe: la sesión es un documento con un solo
/// escritor.
#[tokio::test]
async fn una_ventana_suelta_no_escribe() {
    let mut falso = Falso::default();
    falso.pon("mem:///casa", vec![(b"a".to_vec(), false)]);
    *falso.sesion.lock().expect("sesión") = (sesion_guardada(1, 7, 1, "mem:///casa"), false);
    let backend = Arc::new(falso);
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    // Y lo DICE desde el primer frame, con el mismo indicador que el
    // terminal: hasta aquí una ventana suelta cerraba y perdía dónde estaba
    // cada panel sin una palabra.
    let indicador = norte_i18n::t_in(norte_i18n::Lang::Es, "status-session-detached");
    assert!(
        snap.status.banners.iter().any(|b| b.text == indicador),
        "la barra lleva el indicador de sesión suelta: {:?}",
        snap.status.banners
    );
    let informe = h.shutdown().await.expect("apaga");
    assert!(!informe.incomplete, "no escribir no es dejar algo a medias");
    assert!(backend.escrito.lock().expect("escrito").is_none());
}

/// Y la dueña no lleva el indicador: no es un adorno, es un estado.
#[tokio::test]
async fn la_duena_no_lleva_el_indicador_de_sesion() {
    let mut falso = Falso::default();
    falso.pon("mem:///casa", vec![(b"a".to_vec(), false)]);
    *falso.sesion.lock().expect("sesión") = (sesion_guardada(1, 7, 1, "mem:///casa"), true);
    let (_h, snap) = host_arbol(Arc::new(falso)).await;
    let indicador = norte_i18n::t_in(norte_i18n::Lang::Es, "status-session-detached");
    assert!(
        !snap.status.banners.iter().any(|b| b.text == indicador),
        "la dueña no avisa de nada: {:?}",
        snap.status.banners
    );
}

/// La dueña vuelca al cerrar —cerrar justo después de navegar guarda el
/// directorio nuevo— y las MARCAS no entran en la sesión.
#[tokio::test]
async fn la_duena_vuelca_al_cerrar_y_sin_marcas() {
    let mut falso = Falso::default();
    falso.pon(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"a".to_vec(), false)],
    );
    falso.pon("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    *falso.sesion.lock().expect("sesión") = (sesion_guardada(1, 7, 1, "mem:///casa"), true);
    let backend = Arc::new(falso);
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;

    // Marcar algo y navegar.
    let fila = listado(&snap).rows[0].key;
    h.dispatch(UiAction::ToggleMark {
        slot_id: 1,
        key: fila,
        generation: listado(&snap).generation,
    })
    .await
    .expect("host vivo");
    let mut sub = h.subscribe();
    let docs = listado(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("el directorio está")
        .key;
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs,
        generation: listado(&snap).generation,
    })
    .await
    .expect("host vivo");
    siguiente_foto(&mut sub).await;

    h.shutdown().await.expect("apaga");
    let escrito = backend
        .escrito
        .lock()
        .expect("escrito")
        .clone()
        .expect("escribió");
    let texto = escrito.to_string();
    assert!(
        texto.contains("casa/docs"),
        "guarda dónde acabó, no dónde empezó: {texto}"
    );
    assert!(
        !texto.contains("marks") && !texto.contains("marcas"),
        "las marcas no entran en la sesión: {texto}"
    );
}

/// La ventana gráfica NO le cambia la disposición al TUI, ni le barre sus
/// huecos.
///
/// `capturar_sesion` escribía `self.arbol` en `layouts["default"]`, y hasta
/// esta fase `self.arbol` era constante —o sea que escribía lo que había
/// leído—. Cambiarlo con `layout.pick` o con dos `Ctrl+→` lo convirtió en una
/// escritura de verdad, y `norte-tui` ADOPTA `layouts["default"]` al
/// arrancar: curiosear un minuto en el selector le cambiaba el arranque al
/// TUI. El rustdoc del campo lo prohíbe por su nombre (ADR 0058 D5) y
/// `aplicar_disposicion_elegida` promete «se aplica para ESTA ventana», que
/// era verdad para la configuración y falso para la sesión.
///
/// Y de paso: se partía de un `SessionBody::default()`, así que los huecos de
/// cualquier OTRO frontend se tiraban en vez de conservarse.
#[tokio::test]
async fn cerrar_la_ventana_no_le_toca_la_disposicion_ni_los_huecos_al_tui() {
    use norte_frontend::layout::{KindId, Node, SlotId};

    // Lo que había en la sesión: la disposición del TUI y un hueco suyo que
    // esta ventana no tiene.
    let del_tui = Node::Split {
        dir: norte_frontend::layout::Dir::Vertical,
        children: vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(42), KindId::new("tasks")),
        ],
        sizes: vec![
            norte_frontend::layout::Size::Weight(1),
            norte_frontend::layout::Size::Fixed(3),
        ],
    };
    let mut body = norte_frontend::session::SessionBody::default();
    body.layouts.insert("default".to_owned(), del_tui.clone());
    body.slots.insert(
        99,
        norte_frontend::session::SlotState {
            path: VPath::parse("mem:///ajeno").expect("vpath"),
            cursor: 0,
            back: Vec::new(),
            forward: Vec::new(),
            jump: None,
            sort: norte_frontend::SortSpec::default(),
            columns: Vec::new(),
            show_hidden: false,
            // Recién tocado por el otro frontend: no es un huérfano.
            touched_ms: u64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis()),
            )
            .unwrap_or(0),
            marks: Vec::new(),
        },
    );

    let mut falso = Falso::default();
    falso.pon("mem:///casa", vec![(b"a".to_vec(), false)]);
    *falso.sesion.lock().expect("sesión") = (
        norte_proto::methods::Session {
            version: norte_frontend::session::SCHEMA_VERSION,
            revision: 7,
            body: serde_json::to_value(&body).expect("json"),
        },
        true,
    );
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // La ventana cambia SU disposición: dos veces de ancho.
    for _ in 0..2 {
        h.dispatch(tecla("ctrl+Right")).await.expect("host vivo");
    }
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let _ = siguiente_foto(&mut sub).await;

    h.shutdown().await.expect("apaga");
    let escrito = backend
        .escrito
        .lock()
        .expect("escrito")
        .clone()
        .expect("escribió");
    let guardado: norte_frontend::session::SessionBody =
        serde_json::from_value(escrito).expect("el cuerpo parsea");

    assert_eq!(
        guardado.layouts.get("default"),
        Some(&del_tui),
        "la disposición del TUI se queda como estaba"
    );
    assert!(
        guardado.slots.contains_key(&99),
        "y su hueco también: partir de `default()` lo tiraba — {:?}",
        guardado.slots.keys().collect::<Vec<_>>()
    );
    // Y los huecos VIVOS se sellan con un reloj de verdad: un cero los dejaba
    // con treinta días de edad para el siguiente escritor, que se los llevaba
    // en su primera barrida.
    let vivo = guardado.slots.get(&1).expect("el hueco propio está");
    assert!(
        vivo.touched_ms > 0,
        "el hueco vivo se sella con la hora, no con cero: {vivo:?}"
    );
}

/// Un conflicto al escribir NO pisa lo de la otra ventana, y se DICE.
#[tokio::test]
async fn un_conflicto_no_pisa_a_nadie_y_se_dice() {
    let mut falso = Falso::default();
    falso.pon("mem:///casa", vec![(b"a".to_vec(), false)]);
    *falso.sesion.lock().expect("sesión") = (sesion_guardada(1, 7, 1, "mem:///otro"), true);
    falso.conflicto = true;
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let informe = h.shutdown().await.expect("apaga");
    assert!(
        informe.incomplete,
        "lo nuestro no llegó, y apagar en silencio sería mentir"
    );
    assert!(backend.escrito.lock().expect("escrito").is_none());
}

/// **Un cuerpo que se pasa de tamaño se DEGRADA y se reintenta** (#316).
///
/// El core rehúsa el `put` entero y deja almacenado lo que hubiera, o sea
/// dónde estaba el lector hace días. La TUI ya tiraba el historial y volvía a
/// intentarlo; esta ventana trataba cualquier error igual —«no llegó»— y esa
/// es la divergencia silenciosa del ADR 0077.
///
/// Lo que se comprueba es que el SEGUNDO intento manda algo distinto: sin
/// degradar, reintentar es pedir el mismo error otra vez.
#[tokio::test]
async fn un_cuerpo_que_no_cabe_se_degrada_y_se_reintenta() {
    let mut falso = Falso::default();
    falso.pon("mem:///casa", vec![(b"a".to_vec(), false)]);
    // Con historial, que es lo único que la degradación tira.
    let mut guardada = sesion_guardada(1, 7, 1, "mem:///casa");
    let mut cuerpo: norte_frontend::session::SessionBody =
        serde_json::from_value(guardada.body.clone()).expect("cuerpo");
    for s in cuerpo.slots.values_mut() {
        s.back = vec![VPath::parse("mem:///casa/atras").expect("vpath")];
    }
    guardada.body = serde_json::to_value(&cuerpo).expect("json");
    *falso.sesion.lock().expect("sesión") = (guardada, true);
    // El primero no cabe; el segundo sí.
    *falso.rechazos_por_tamano.lock().expect("rechazos") = 1;
    let backend = Arc::new(falso);

    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let informe = h.shutdown().await.expect("apaga");

    let puestas = backend.puestas.lock().expect("puestas");
    assert_eq!(puestas.len(), 2, "se reintenta UNA vez: {puestas:?}");
    let ultimo: norte_frontend::session::SessionBody =
        serde_json::from_value(puestas[1].clone()).expect("cuerpo");
    assert!(
        ultimo
            .slots
            .values()
            .all(|s| s.back.is_empty() && s.forward.is_empty()),
        "el reintento va sin historial, que es lo que se degrada"
    );
    assert!(
        !ultimo.slots.is_empty(),
        "y CON los huecos: lo que había que salvar es dónde está el lector"
    );
    assert!(
        !informe.incomplete,
        "el segundo `put` entró, así que no queda nada sin escribir"
    );
}

/// Y si ni sin historial cabe, se dice: reintentar otra vez sería pedir el
/// mismo error, y apagar en silencio sería mentir.
#[tokio::test]
async fn un_cuerpo_que_no_cabe_ni_degradado_se_dice() {
    let mut falso = Falso::default();
    falso.pon("mem:///casa", vec![(b"a".to_vec(), false)]);
    // CON historial: sin él `degrade_for_size` no tiene nada que tirar,
    // contesta `false`, y el reintento ni se intenta — el test pasaría sin
    // ejercitar el camino que dice ejercitar.
    let mut guardada = sesion_guardada(1, 7, 1, "mem:///casa");
    let mut cuerpo: norte_frontend::session::SessionBody =
        serde_json::from_value(guardada.body.clone()).expect("cuerpo");
    for s in cuerpo.slots.values_mut() {
        s.back = vec![VPath::parse("mem:///casa/atras").expect("vpath")];
    }
    guardada.body = serde_json::to_value(&cuerpo).expect("json");
    *falso.sesion.lock().expect("sesión") = (guardada, true);
    *falso.rechazos_por_tamano.lock().expect("rechazos") = 5;
    let backend = Arc::new(falso);

    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let informe = h.shutdown().await.expect("apaga");

    assert!(informe.incomplete);
    assert_eq!(
        backend.puestas.lock().expect("puestas").len(),
        2,
        "un reintento, y solo uno: sin nada más que degradar, insistir es pedir el mismo error"
    );
    assert!(backend.escrito.lock().expect("escrito").is_none());
}

/// Un doble con una sesión guardada de la que esta ventana es dueña.
fn falso_con_sesion(guardada: norte_proto::methods::Session, duena: bool) -> Falso {
    let mut falso = Falso::default();
    falso.pon(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"a".to_vec(), false)],
    );
    *falso.sesion.lock().expect("sesión") = (guardada, duena);
    falso
}

/// El último cuerpo escrito, ya leído.
fn cuerpo_escrito(f: &Falso) -> Option<norte_frontend::session::SessionBody> {
    let v = f.escrito.lock().ok()?.clone()?;
    serde_json::from_value(v).ok()
}

/// Alternar un panel lateral escribe la disposición en la sesión AL MOMENTO,
/// bajo la clave PROPIA de la ventana (ADR 0139, que sustituye aquí a la D8
/// de la ADR 0058): la terminal y la ventana recuerdan cada una la suya, y
/// la de la terminal no se toca.
#[tokio::test]
async fn alternar_un_panel_escribe_la_disposicion_al_momento() {
    let backend = Arc::new(falso_con_sesion(
        sesion_guardada(1, 7, 1, "mem:///casa"),
        true,
    ));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    por_la_paleta(&h, &mut sub, "layout.places").await;
    let cuerpo = hasta(&backend, "la disposición escrita", cuerpo_escrito).await;
    let arbol = cuerpo
        .layouts
        .get("default@window")
        .expect("bajo la clave de la ventana sin perfil");
    assert!(
        !cuerpo.layouts.contains_key("default"),
        "la de la terminal no se escribe"
    );
    let texto = serde_json::to_string(arbol).expect("json");
    assert!(
        texto.contains("places"),
        "con la barra lateral dentro: {texto}"
    );
    assert!(
        cuerpo.slots.contains_key(&1),
        "y los huecos siguen ahí: {:?}",
        cuerpo.slots.keys().collect::<Vec<_>>()
    );

    // Cerrarla escribe OTRA vez, sin ella: cada cambio del árbol se guarda.
    let antes = backend.puestas.lock().expect("puestas").len();
    por_la_paleta(&h, &mut sub, "layout.places").await;
    let cuerpo = hasta(&backend, "la segunda escritura", |f| {
        (f.puestas.lock().ok()?.len() > antes)
            .then(|| cuerpo_escrito(f))
            .flatten()
    })
    .await;
    let texto =
        serde_json::to_string(cuerpo.layouts.get("default@window").expect("sigue")).expect("json");
    assert!(!texto.contains("places"), "ya sin la barra: {texto}");
}

/// Y al arrancar se APLICA la que la sesión guardó, por encima de la de la
/// configuración: cierra la ventana con dos listados, vuelve con dos.
#[tokio::test]
async fn la_disposicion_guardada_se_aplica_al_arrancar() {
    let mut guardada = sesion_guardada(1, 7, 1, "mem:///casa");
    let mut cuerpo: norte_frontend::session::SessionBody =
        serde_json::from_value(guardada.body.clone()).expect("cuerpo");
    cuerpo.layouts.insert(
        "default".to_owned(),
        norte_frontend::layout::presets::tree("orthodox").expect("preset"),
    );
    guardada.body = serde_json::to_value(&cuerpo).expect("json");
    let backend = Arc::new(falso_con_sesion(guardada, true));
    // El host arranca con `simple`, un listado; la sesión dice `orthodox`,
    // dos. Lo que se ve al arrancar es lo que la sesión dice.
    let (_h, snap) = host_arbol(Arc::clone(&backend)).await;
    let listados = snap
        .slots
        .iter()
        .filter(|s| matches!(s, norte_ui_host::dto::SlotView::Browser(_)))
        .count();
    assert_eq!(
        listados, 2,
        "los dos listados de la disposición guardada, no el uno de `simple`"
    );
}

/// ADR 0139: con la suya guardada, la ventana arranca con LA SUYA, aunque la
/// terminal haya dejado otra después.
#[tokio::test]
async fn la_ventana_arranca_con_su_disposicion_y_no_con_la_de_la_terminal() {
    let mut guardada = sesion_guardada(1, 7, 1, "mem:///casa");
    let mut cuerpo: norte_frontend::session::SessionBody =
        serde_json::from_value(guardada.body.clone()).expect("cuerpo");
    // La terminal: un listado. La ventana: dos.
    cuerpo.layouts.insert(
        "default".to_owned(),
        norte_frontend::layout::presets::tree("simple").expect("preset"),
    );
    cuerpo.layouts.insert(
        "default@window".to_owned(),
        norte_frontend::layout::presets::tree("orthodox").expect("preset"),
    );
    guardada.body = serde_json::to_value(&cuerpo).expect("json");
    let backend = Arc::new(falso_con_sesion(guardada, true));
    let (_h, snap) = host_arbol(Arc::clone(&backend)).await;
    let listados = snap
        .slots
        .iter()
        .filter(|s| matches!(s, norte_ui_host::dto::SlotView::Browser(_)))
        .count();
    assert_eq!(listados, 2, "la de la ventana, no la de la terminal");
}

/// El tic de la sesión escribe lo que cambió y NO repite lo mismo.
///
/// Con el reloj parado: un segundo virtual dispara el tic sin esperar un
/// segundo de verdad. La primera vuelta escribe —la disposición de esta
/// ventana aún no estaba en la sesión— y la segunda, sin cambios, no manda
/// nada: comparar con lo último escrito es todo lo que hace un tic quieto.
#[tokio::test(start_paused = true)]
async fn el_tic_escribe_lo_que_cambio_y_no_repite_lo_mismo() {
    let backend = Arc::new(falso_con_sesion(
        sesion_guardada(1, 7, 1, "mem:///casa"),
        true,
    ));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    assert!(
        backend.puestas.lock().expect("puestas").is_empty(),
        "arrancar no escribe: el tic aún no ha sonado"
    );
    tokio::time::advance(std::time::Duration::from_millis(1100)).await;
    let n = hasta(&backend, "la primera escritura del tic", |f| {
        let n = f.puestas.lock().ok()?.len();
        (n > 0).then_some(n)
    })
    .await;
    assert_eq!(n, 1, "una escritura, la de la disposición nueva");

    tokio::time::advance(std::time::Duration::from_millis(2100)).await;
    // Una vuelta al actor: los tics que sonaron ya se han atendido.
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    assert_eq!(
        backend.puestas.lock().expect("puestas").len(),
        1,
        "sin cambios, el tic no repite lo mismo"
    );
}

/// Una ventana SUELTA no escribe tampoco al alternar un panel.
#[tokio::test]
async fn una_ventana_suelta_no_escribe_al_alternar_un_panel() {
    let backend = Arc::new(falso_con_sesion(
        sesion_guardada(1, 7, 1, "mem:///casa"),
        false,
    ));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    por_la_paleta(&h, &mut sub, "layout.places").await;
    asentar().await;
    assert!(
        backend.puestas.lock().expect("puestas").is_empty(),
        "suelta no escribe: la sesión es un documento con un solo escritor"
    );
}

/// Espera la siguiente actualización que traiga tasks.
pub(super) async fn siguientes_tasks(
    sub: &mut norte_ui_host::UiSubscription,
) -> Vec<norte_ui_host::dto::TaskView> {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("una actualización con tasks, no un cuelgue")
            .expect("el host sigue vivo");
        match siguiente {
            Update::Message(m) => {
                if let UiUpdate::Patch(p) = &m.payload {
                    for c in &p.changes {
                        if let norte_ui_host::dto::ViewChange::Tasks { tasks: t, .. } = c {
                            return t.clone();
                        }
                    }
                }
                if let UiUpdate::Snapshot(s) = &m.payload
                    && !s.tasks.is_empty()
                {
                    return s.tasks.clone();
                }
            }
            Update::Lagged => panic!("sin retraso en este test"),
        }
    }
    panic!("no llegó ninguna actualización con tasks");
}

/// Espera la siguiente actualización que traiga diálogos.
pub(super) async fn siguientes_dialogos(
    sub: &mut norte_ui_host::UiSubscription,
) -> Vec<norte_ui_host::dto::DialogView> {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("una actualización con diálogos, no un cuelgue")
            .expect("el host sigue vivo");
        match siguiente {
            Update::Message(m) => {
                if let UiUpdate::Patch(p) = &m.payload {
                    for c in &p.changes {
                        if let norte_ui_host::dto::ViewChange::Dialogs { dialogs: d } = c {
                            return d.clone();
                        }
                    }
                }
            }
            Update::Lagged => panic!("sin retraso en este test"),
        }
    }
    panic!("no llegó ninguna actualización con diálogos");
}

/// Borrar NO borra: abre la confirmación, y la respuesta destructiva viene
/// marcada como tal para que el renderer no tenga que adivinar cuál es.
#[tokio::test]
async fn borrar_pide_confirmacion_antes_de_tocar_nada() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F8")).await.expect("host vivo");
    let dialogos = siguientes_dialogos(&mut sub).await;
    assert_eq!(dialogos.len(), 1, "se abre UN diálogo");
    assert!(
        dialogos[0].choices.iter().any(|c| c.destructive),
        "y dice cuál de las respuestas destruye"
    );
    assert!(
        backend.borrados.lock().expect("borrados").is_empty(),
        "abrir el diálogo no borra nada"
    );
}

/// Confirmar dos veces con el MISMO id no borra dos veces: el segundo es una
/// carrera del renderer, no una segunda orden.
#[tokio::test]
async fn confirmar_dos_veces_no_borra_dos_veces() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F8")).await.expect("host vivo");
    let dialogos = siguientes_dialogos(&mut sub).await;
    let id = dialogos[0].id;

    let primero = h
        .dispatch(UiAction::Dialog {
            id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host vivo");
    assert!(matches!(primero, ActionAck::Applied { .. }));

    let segundo = h
        .dispatch(UiAction::Dialog {
            id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host vivo");
    assert_eq!(
        segundo,
        ActionAck::Stale {
            reason: StaleAction::Modal
        },
        "el segundo confirm es una carrera, no una orden"
    );

    // Y solo se pidió UN borrado: se espera al primero, y se deja correr lo
    // que hubiera detrás antes de contar.
    hasta(&backend, "el borrado encolado", |f| {
        (!f.borrados.lock().expect("borrados").is_empty()).then_some(())
    })
    .await;
    asentar().await;
    assert_eq!(backend.borrados.lock().expect("borrados").len(), 1);
}

/// Una respuesta que el diálogo no ofreció no se interpreta: en una
/// superficie de decisión no hay respuestas implícitas.
#[tokio::test]
async fn una_respuesta_que_no_existe_no_se_interpreta() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F8")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    let ack = h
        .dispatch(UiAction::Dialog {
            id,
            choice: "borra-y-no-preguntes".to_owned(),
            secret: None,
        })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Modal
        }
    );
    assert!(backend.borrados.lock().expect("borrados").is_empty());
}

/// Confirmado el borrado, la task aparece en el tablero y su estado TERMINAL
/// llega: un desenlace que se pierde deja al usuario mirando un progreso que
/// no avanza.
#[tokio::test]
async fn la_task_aparece_y_su_desenlace_no_se_pierde() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
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

    let tasks = siguientes_tasks(&mut sub).await;
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].task_id, 7);

    // El daemon termina la task.
    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("hay task");
    tx.send_modify(|p| {
        p.state = norte_proto::TaskState::Completed;
        p.bytes_done = 10;
    });
    let tasks = siguientes_tasks(&mut sub).await;
    assert_eq!(
        tasks[0].state,
        norte_ui_host::dto::TaskStateView::Done,
        "el estado terminal llega al tablero"
    );
    assert_eq!(tasks[0].percent, Some(100));
}

/// Una task TERMINADA se va sola del tablero a los diez segundos.
///
/// Antes se quedaba hasta que otra la empujaba fuera por el tope de filas, así
/// que el panel enseñaba el historial de la sesión en vez de lo que está
/// pasando. Es el mismo plazo que el TUI: dos frontends que caducan distinto
/// son dos respuestas a «¿sigue esto en marcha?».
///
/// Reloj VIRTUAL (`start_paused`): el test no espera diez segundos, los salta
/// — cuando nadie tiene trabajo, tokio adelanta al siguiente temporizador.
#[tokio::test(start_paused = true)]
async fn una_task_terminada_se_va_del_tablero_sola() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
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
    assert_eq!(siguientes_tasks(&mut sub).await.len(), 1);

    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("hay task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    let tasks = siguientes_tasks(&mut sub).await;
    assert_eq!(
        tasks[0].state,
        norte_ui_host::dto::TaskStateView::Done,
        "primero se VE terminada: el ✓ no puede pasar de largo"
    );

    // Se adelanta el reloj A MANO en vez de dejar que tokio salte solo: los
    // helpers de este fichero esperan con un plazo de 500 ms, y el salto
    // automático va al temporizador MÁS CERCANO — o sea a ese plazo, no al
    // TTL, y el test moriría por «cuelgue» sin que nada estuviera mal.
    tokio::time::advance(std::time::Duration::from_secs(11)).await;
    // El `spawn` del TTL despierta y manda su mensaje; el ceder deja que lo
    // haga ANTES de que el `Resync` entre en el mismo buzón, que se atiende
    // en orden.
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    // Hasta que el tablero quede vacío: entre el desenlace y la caducidad hay
    // otros cambios de tablero (el relistado del directorio que el borrado
    // dejó viejo publica el suyo), y afirmar sobre «el siguiente» sería
    // afirmar sobre el que pase primero.
    let mut vacio = false;
    for _ in 0..5 {
        if siguientes_tasks(&mut sub).await.is_empty() {
            vacio = true;
            break;
        }
    }
    assert!(vacio, "la terminada caducó y se fue del tablero");
}

/// ¿Hay un panel de procesos colocado en esta foto?
fn hay_procesos(snap: &norte_ui_host::ViewSnapshot) -> bool {
    snap.slots
        .iter()
        .any(|s| matches!(s, SlotView::Processes { .. }))
}

/// El panel de procesos se abre solo cuando el trabajo DURA (ADR 0146) y se
/// va cuando la fila caduca.
///
/// Las dos mitades del gesto, y la segunda es la que faltaba: el host solo
/// reevaluaba desde `progreso`, y cuando la última fila caduca ya no llega
/// ningún progreso más — así que el panel que se abrió solo se quedaba puesto
/// el resto de la sesión. El terminal no tenía el fallo porque su bucle
/// reevalúa en cada vuelta; era la clase de divergencia que el ADR 0077
/// persigue, y ningún test la veía porque todos miraban el PRIMER evento.
///
/// Reloj virtual, como el del TTL de aquí arriba y por lo mismo.
#[tokio::test(start_paused = true)]
async fn el_panel_de_procesos_se_abre_solo_y_se_cierra_al_caducar_la_fila() {
    let backend = arbol();
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    assert!(
        !hay_procesos(&snap),
        "sin nada encolado, el panel no ocupa sitio"
    );
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
    assert_eq!(siguientes_tasks(&mut sub).await.len(), 1);

    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("hay task");
    // Un tic de progreso: es donde el host reevalúa el panel. Abrirlo ya en el
    // REGISTRO se probó y se revirtió —abrir un panel republica la foto
    // entera, y hacerlo al encolar la mete en medio de cada operación que el
    // lector acaba de pedir—; está escrito en el ADR 0115.
    tx.send_modify(|p| p.bytes_done = 1);
    // Antes de que la ráfaga DURE, el panel no se abre (ADR 0146): una
    // copia que acaba en un segundo la cuenta la barra de estado.
    assert!(
        !hay_procesos(&crate::sync::siguiente_foto_tras_resync(&h, &mut sub).await),
        "una ráfaga recién empezada no abre el panel"
    );
    tokio::time::advance(std::time::Duration::from_millis(
        u64::try_from(norte_frontend::task_strip::PANEL_MS).expect("positivo") + 100,
    ))
    .await;
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    // HASTA que aparezca, no en la primera foto: el progreso viaja por el
    // buzón del actor y el `Resync` entra en ese mismo buzón, así que
    // afirmar sobre «la siguiente» sería afirmar sobre la que llegue antes.
    let mut abierto = false;
    for _ in 0..6 {
        if hay_procesos(&crate::sync::siguiente_foto_tras_resync(&h, &mut sub).await) {
            abierto = true;
            break;
        }
    }
    assert!(abierto, "se abrió solo cuando el trabajo ya duraba");

    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    // Otra vez HASTA, no «la siguiente»: el bucle de arriba dejó un `Resync`
    // en camino, y su foto llega entre el desenlace y quien lo espera.
    let mut terminada = false;
    for _ in 0..6 {
        if siguientes_tasks(&mut sub)
            .await
            .first()
            .is_some_and(|t| t.state == norte_ui_host::dto::TaskStateView::Done)
        {
            terminada = true;
            break;
        }
    }
    assert!(
        terminada,
        "primero se VE terminada, con el panel todavía puesto"
    );

    // El mismo salto a mano que el test del TTL, y por el mismo motivo.
    tokio::time::advance(std::time::Duration::from_secs(11)).await;
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    // Hasta que se vaya: entre el desenlace y la caducidad pasan otras cosas
    // (el relistado del directorio que el borrado dejó viejo), y afirmar
    // sobre «la siguiente foto» sería afirmar sobre la que pase primero.
    let mut cerrado = false;
    for _ in 0..6 {
        if !hay_procesos(&crate::sync::siguiente_foto_tras_resync(&h, &mut sub).await) {
            cerrado = true;
            break;
        }
    }
    assert!(
        cerrado,
        "y se cerró solo cuando la última fila caducó: un panel que se abre \
         solo y no se cierra nunca ocupa un tercio de la pantalla para decir \
         que no pasa nada"
    );
}

/// ADR 0146: una copia que acaba antes del umbral no abre el panel NI pinta
/// la barra, pero deja el «✓» en el item de tareas; y el «✓» se va solo.
#[tokio::test(start_paused = true)]
async fn una_tarea_rapida_deja_el_hecho_y_no_abre_el_panel() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
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
    assert_eq!(siguientes_tasks(&mut sub).await.len(), 1);
    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("hay task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    let tareas =
        |s: &norte_ui_host::ViewSnapshot| s.status_items.iter().find(|i| i.id == "tasks").cloned();
    let mut hecho = None;
    for _ in 0..6 {
        let foto = crate::sync::siguiente_foto_tras_resync(&h, &mut sub).await;
        assert!(!hay_procesos(&foto), "una copia rápida no abre el panel");
        if let Some(t) = tareas(&foto) {
            hecho = Some(t);
            break;
        }
    }
    let hecho = hecho.expect("el item de tareas dice que acabó");
    assert!(hecho.text.starts_with('✓'), "{:?}", hecho.text);
    assert!(hecho.progress.is_none(), "un ✓ no lleva barra");

    tokio::time::advance(std::time::Duration::from_millis(
        u64::try_from(norte_frontend::task_strip::HECHO_MS).expect("positivo") + 100,
    ))
    .await;
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    let mut ido = false;
    for _ in 0..6 {
        if tareas(&crate::sync::siguiente_foto_tras_resync(&h, &mut sub).await).is_none() {
            ido = true;
            break;
        }
    }
    assert!(ido, "y el ✓ se va solo");
}

/// ADR 0146: tras un relevo del daemon, el trabajo del ANTERIOR no deja la
/// barra en marcha ni el panel automático abierto para siempre: esas tasks
/// no van a terminar nunca, porque ya no hay nadie que las termine.
#[tokio::test(start_paused = true)]
async fn un_relevo_no_deja_la_barra_ni_el_panel_colgados() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    let (evtx, evrx) = tokio::sync::mpsc::unbounded_channel();
    *falso.eventos.lock().expect("eventos") = Some(evrx);
    let (h, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = h.subscribe();
    let _p = inyectar_task_de(&tx, 7, norte_proto::TaskKind::Copy);
    siguientes_tasks(&mut sub).await;
    tokio::time::advance(std::time::Duration::from_millis(
        u64::try_from(norte_frontend::task_strip::PANEL_MS).expect("positivo") + 100,
    ))
    .await;
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    let mut abierto = false;
    for _ in 0..6 {
        if hay_procesos(&crate::sync::siguiente_foto_tras_resync(&h, &mut sub).await) {
            abierto = true;
            break;
        }
    }
    assert!(abierto, "el trabajo que dura abre el panel");

    evtx.send(norte_client::ConnEvent::GoingAway { reconnect: true })
        .expect("el host escucha");
    evtx.send(norte_client::ConnEvent::Restored)
        .expect("el host escucha");
    let mut limpio = false;
    for _ in 0..8 {
        let foto = crate::sync::siguiente_foto_tras_resync(&h, &mut sub).await;
        if !hay_procesos(&foto) && foto.status_items.iter().all(|i| i.id != "tasks") {
            limpio = true;
            break;
        }
    }
    assert!(
        limpio,
        "la task del daemon anterior no mantiene ni el panel ni la barra"
    );
}

/// Cancelar es idempotente: pedirlo dos veces no es un error.
#[tokio::test]
async fn cancelar_es_idempotente() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
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
    siguientes_tasks(&mut sub).await;

    for _ in 0..2 {
        let ack = h
            .dispatch(UiAction::CancelTask { task_id: 7 })
            .await
            .expect("host vivo");
        assert!(matches!(ack, ActionAck::Applied { .. }));
    }
    assert_eq!(
        backend.cancelaciones.load(Ordering::SeqCst),
        2,
        "las dos peticiones llegan; el contrato de idempotencia es del daemon"
    );
}

/// Cancelar una task que el tablero no conoce es una carrera, no un error.
#[tokio::test]
async fn cancelar_lo_que_no_existe_es_una_carrera() {
    let (h, _snap) = host_arbol(arbol()).await;
    let ack = h
        .dispatch(UiAction::CancelTask { task_id: 999 })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Generation
        }
    );
}

/// El buscador incremental se abre por su comando, se queda las teclas de
/// TEXTO y filtra el listado. Es el contexto de entrada del listado: dejar
/// que el resolver se quedara la «d» convertiría teclear en borrar.
#[tokio::test]
async fn el_buscador_se_queda_el_texto_y_filtra() {
    let (host, _snap) = host_arbol(arbol()).await;
    let mut sub = host.subscribe();

    host.dispatch(tecla("/")).await.expect("host vivo");
    host.dispatch(UiAction::Resync).await.expect("host vivo");
    let abierto = siguiente_foto(&mut sub).await;
    assert!(
        listado(&abierto).quick.is_some(),
        "el buscador está abierto"
    );

    // Teclear NO ejecuta comandos: filtra.
    for c in ["n", "o"] {
        host.dispatch(tecla(c)).await.expect("host vivo");
    }
    host.dispatch(UiAction::Resync).await.expect("host vivo");
    let filtrado = siguiente_foto(&mut sub).await;
    let quick = listado(&filtrado).quick.clone().expect("sigue abierto");
    assert_eq!(quick.query, "no");
    assert_eq!(quick.matches, 1, "solo `notas.txt` casa");

    // Y Esc lo cierra sin tocar el listado.
    host.dispatch(tecla("Escape")).await.expect("host vivo");
    host.dispatch(UiAction::Resync).await.expect("host vivo");
    let cerrado = siguiente_foto(&mut sub).await;
    assert!(listado(&cerrado).quick.is_none());
    assert_eq!(listado(&cerrado).rows.len(), 3, "el listado sigue entero");
}

/// Perder el daemon se pinta Y se dice: notarlo solo en un icono no basta
/// cuando pasa a mitad de una operación.
#[tokio::test]
async fn la_conexion_perdida_se_pinta_y_se_dice() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.eventos.lock().expect("eventos") = Some(rx);
    let (host, snap) = host_arbol(Arc::new(falso)).await;
    assert_eq!(
        snap.connection,
        norte_ui_host::dto::ConnectionView::Connected
    );

    let mut sub = host.subscribe();
    tx.send(norte_client::ConnEvent::Lost)
        .expect("el host escucha");

    let mut vista = None;
    let mut dicho = false;
    for _ in 0..10 {
        match tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("llega")
            .expect("el host sigue vivo")
        {
            Update::Message(m) => match &m.payload {
                UiUpdate::Patch(p) => {
                    for c in &p.changes {
                        if let norte_ui_host::dto::ViewChange::Connection(v) = c {
                            vista = Some(v.clone());
                        }
                    }
                }
                UiUpdate::Notice(norte_ui_host::dto::UiNotice::Message { key, .. }) => {
                    if key == "msg-daemon-lost" {
                        dicho = true;
                    }
                }
                UiUpdate::Snapshot(_) | UiUpdate::Notice(_) => {}
            },
            Update::Lagged => {}
        }
        if vista.is_some() && dicho {
            break;
        }
    }
    assert_eq!(
        vista,
        Some(norte_ui_host::dto::ConnectionView::Reconnecting),
        "se pinta reconectando"
    );
    assert!(dicho, "y se dice");
}

/// Una task que lanzó OTRO cliente de la misma sesión aparece en el tablero,
/// y el tablero dice que es ajena: una operación que uno no ha pedido y no se
/// distingue de las suyas es una sorpresa.
#[tokio::test]
async fn una_task_ajena_se_ve_y_se_dice_ajena() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    let (host, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = host.subscribe();

    let progreso = norte_proto::TaskProgress {
        task_id: norte_proto::TaskId::new(11),
        kind: norte_proto::TaskKind::Copy,
        state: norte_proto::TaskState::Running,
        bytes_done: 0,
        bytes_total: None,
        entries_done: 0,
        entries_total: None,
        current: None,
        unreadable: None,
        unvisited: None,
    };
    let (_ptx, prx) = tokio::sync::watch::channel(progreso);
    tx.send(norte_ui_host::backend::HostTask {
        id: norte_proto::TaskId::new(11),
        progress: prx,
        cancel: Arc::new(|| {}),
        pause: None,
        foreign: true,
    })
    .expect("el host escucha");

    let tasks = siguientes_tasks(&mut sub).await;
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].task_id, 11);
    assert!(tasks[0].foreign, "el tablero dice que es ajena");
}

/// Las columnas configuradas llegan como celdas, con el MISMO formato que
/// pinta el TUI, y la ausencia viaja como ausencia: un directorio sin tamaño
/// no lleva un `0` fabricado.
#[tokio::test]
async fn las_columnas_configuradas_llegan_como_celdas() {
    let (_h, snap) = host_arbol(arbol()).await;
    let filas = &listado(&snap).rows;
    let dir = filas
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("el directorio está");
    let fichero = filas
        .iter()
        .find(|r| r.display_name == "notas.txt")
        .expect("el fichero está");

    let columnas: Vec<&str> = fichero.cells.iter().map(|c| c.column.as_str()).collect();
    assert_eq!(
        columnas,
        vec!["size", "mtime"],
        "nombre aparte, el resto aquí"
    );

    let size_dir = dir
        .cells
        .iter()
        .find(|c| c.column == "size")
        .expect("la celda existe");
    assert_eq!(size_dir.text, None, "un dir sin tamaño no inventa un cero");

    let size_fichero = fichero
        .cells
        .iter()
        .find(|c| c.column == "size")
        .expect("la celda existe");
    assert!(
        size_fichero.text.is_some(),
        "y un fichero con tamaño lo trae formateado"
    );
}

/// Crear directorio: el diálogo lleva CAMPO DE TEXTO, lo tecleado viaja, y
/// confirmar encola la task.
#[tokio::test]
async fn crear_directorio_teclea_y_encola() {
    let backend = arbol();
    let (host, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = host.subscribe();

    host.dispatch(tecla("F7")).await.expect("host vivo");
    let dialogos = siguientes_dialogos(&mut sub).await;
    let id = dialogos[0].id;
    assert_eq!(
        dialogos[0].input.as_deref(),
        Some(""),
        "el diálogo dice que aquí se teclea"
    );

    host.dispatch(UiAction::DialogInput {
        id,
        text: "carpeta nueva".to_owned(),
    })
    .await
    .expect("host vivo");
    let tecleado = siguientes_dialogos(&mut sub).await;
    assert_eq!(tecleado[0].input.as_deref(), Some("carpeta nueva"));

    host.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    let creados = hasta(&backend, "la creación encolada", |f| {
        let c = f.creados.lock().expect("creados").clone();
        (!c.is_empty()).then_some(c)
    })
    .await;
    assert_eq!(creados.len(), 1, "se encoló una creación");
    assert!(
        creados[0].to_wire().ends_with("carpeta nueva"),
        "con el nombre tecleado: {}",
        creados[0].to_wire()
    );
}

/// Un nombre que no vale no encola nada y se dice.
#[tokio::test]
async fn un_nombre_invalido_no_crea_nada() {
    let backend = arbol();
    let (host, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = host.subscribe();
    host.dispatch(tecla("F7")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;

    host.dispatch(UiAction::DialogInput {
        id,
        text: "..".to_owned(),
    })
    .await
    .expect("host vivo");
    host.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    asentar().await;
    assert!(
        backend.creados.lock().expect("creados").is_empty(),
        "`..` no es un nombre de directorio"
    );
}

/// Escribir en un diálogo de DECISIÓN no se interpreta: no tiene dónde.
#[tokio::test]
async fn no_se_teclea_en_un_dialogo_de_decision() {
    let (host, _snap) = host_arbol(arbol()).await;
    let mut sub = host.subscribe();
    host.dispatch(tecla("F8")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    let ack = host
        .dispatch(UiAction::DialogInput {
            id,
            text: "lo que sea".to_owned(),
        })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Modal
        }
    );
}

/// Una aprobación de policy abre su diálogo, con las rutas SANEADAS y
/// diciendo si la lista viene recortada. Aprobar es una decisión de
/// seguridad: viene marcada como destructiva y no tiene respuesta por
/// defecto.
#[tokio::test]
async fn una_aprobacion_abre_su_dialogo() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.aprobaciones.lock().expect("aprobaciones") = Some(rx);
    let backend = Arc::new(falso);
    let (host, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = host.subscribe();

    tx.send(norte_proto::methods::PolicyApprovalRequired {
        approval_id: 5,
        session: Some("agente-1".to_owned()),
        op: "delete".to_owned(),
        // Con un control dentro: el diálogo lo enmascara, jamás lo pinta.
        paths: vec!["mem:///casa/borra\u{202E}me".to_owned()],
        paths_total: 40,
        ttl_ms: 30_000,
        detail: norte_proto::methods::ApprovalDetail::default(),
    })
    .expect("el host escucha");

    let dialogos = siguientes_dialogos(&mut sub).await;
    assert_eq!(dialogos.len(), 1);
    let d = &dialogos[0];
    assert!(
        d.choices.iter().any(|c| c.id == "approve" && c.destructive),
        "aprobar una op de agente es destructivo y se dice"
    );
    assert!(
        d.body.iter().all(|l| !l.text.contains('\u{202E}')),
        "las rutas van enmascaradas: {:?}",
        d.body
    );
    assert!(
        d.body.iter().any(|l| l.hostile),
        "y se DICE cuál se pinta distinta de lo que es: {:?}",
        d.body
    );
    assert!(
        !d.overflow_note.is_empty(),
        "y que la lista viene recortada, en su propio campo: {d:?}"
    );
    // La única ruta que llega SE ENSEÑA, así que el resumen no marca nada: el
    // badge del recorte habla de lo que NO se puede mirar, y aquí lo recortado
    // lo recortó el server y no llegó.
    assert!(
        !d.overflow_hostile,
        "sin rutas ocultas que mirar, el resumen no marca: {d:?}"
    );
    // Y dice cuánto le queda, en su propio campo: una decisión con fecha de
    // caducidad que no la enseña se lee como una que espera para siempre, y
    // entre las rutas la podría suplantar un nombre de fichero.
    assert_eq!(
        d.deadline.as_deref(),
        Some(
            norte_i18n::ta_in(norte_i18n::Lang::Es, "modal-approval-ttl", &[("s", "30")]).as_str()
        )
    );
}

/// Y una ruta hostil que se queda FUERA de lo que se enseña se dice.
///
/// El badge de una ruta visible dice «lo que lees no son los bytes que hay».
/// Sobre lo recortado no se puede decir eso —no está delante para mirarlo—
/// pero sí que ahí fuera hay algo así, y eso es lo que decide si merece la
/// pena ampliar antes de aprobar. El terminal lo decía en su resumen desde
/// siempre y esta ventana no, sobre las mismas rutas (plan de paridad, 14).
#[tokio::test]
async fn el_resumen_de_una_aprobacion_delata_una_ruta_hostil_escondida() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.aprobaciones.lock().expect("aprobaciones") = Some(rx);
    let backend = Arc::new(falso);
    let (host, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = host.subscribe();

    // Más rutas de las que el diálogo enseña, y la hostil en la COLA. El tope
    // es del host y no se exporta; treinta pasa de largo de cualquier valor
    // razonable, que es lo que hace falta.
    let mut paths: Vec<String> = (0..30).map(|i| format!("mem:///casa/f{i}")).collect();
    let ultima = paths.len() - 1;
    paths[ultima] = "mem:///casa/x\u{202E}y".to_owned();
    let total = paths.len() as u64;

    tx.send(norte_proto::methods::PolicyApprovalRequired {
        approval_id: 7,
        session: Some("agente-1".to_owned()),
        op: "delete".to_owned(),
        paths,
        paths_total: total,
        ttl_ms: 30_000,
        detail: norte_proto::methods::ApprovalDetail::default(),
    })
    .expect("el host escucha");

    let dialogos = siguientes_dialogos(&mut sub).await;
    let d = &dialogos[0];
    assert!(
        !d.overflow_note.is_empty(),
        "la lista viene recortada: {d:?}"
    );
    assert!(
        d.body.iter().all(|l| !l.hostile),
        "las que SE ENSEÑAN son todas limpias, así que el badge no viene de ahí: {:?}",
        d.body
    );
    assert!(
        d.overflow_hostile,
        "y el resumen delata la que no se ve: {d:?}"
    );
}

/// Denegar es lo que pasa por defecto: cualquier respuesta que no sea
/// aprobar deniega, y cerrar el diálogo también. Dejar al agente esperando
/// sería peor que decirle que no.
#[tokio::test]
async fn cualquier_respuesta_que_no_sea_aprobar_deniega() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.aprobaciones.lock().expect("aprobaciones") = Some(rx);
    let backend = Arc::new(falso);
    let (host, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = host.subscribe();

    tx.send(norte_proto::methods::PolicyApprovalRequired {
        approval_id: 9,
        session: None,
        op: "copy".to_owned(),
        paths: vec!["mem:///casa/x".to_owned()],
        paths_total: 1,
        ttl_ms: 30_000,
        detail: norte_proto::methods::ApprovalDetail::default(),
    })
    .expect("el host escucha");
    let id = siguientes_dialogos(&mut sub).await[0].id;

    host.dispatch(UiAction::Dialog {
        id,
        choice: "deny".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    hasta(&backend, "la decisión mandada", |f| {
        (!f.decisiones.lock().expect("decisiones").is_empty()).then_some(())
    })
    .await;
    assert_eq!(
        backend.decisiones.lock().expect("decisiones").clone(),
        vec![(9, false)],
        "se deniega, y se dice al daemon"
    );
}

/// Con catálogo, un `attr:` numérico se pinta como lo que ES: un modo se lee
/// `rwx`, no `33188`.
#[tokio::test]
async fn el_catalogo_da_sentido_a_un_attr() {
    let falso = arbol_como_falso();
    *falso.catalogo.lock().expect("catálogo") =
        norte_proto::AttrCatalog::new(vec![norte_proto::attrs::AttrInfo {
            id: "posix.mode".to_owned(),
            label: "modo".to_owned(),
            ty: norte_proto::attrs::AttrType::Uint,
            hint: norte_proto::attrs::AttrHint::Mode,
        }]);
    let backend = Arc::new(falso);
    let (host, _snap) = UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
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
        columns: columnas_de(&["name", "attr:posix.mode"]),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca");

    // El catálogo llega después del primer listado y trae su propia foto.
    let mut sub = host.subscribe();
    let foto = siguiente_foto(&mut sub).await;
    let fila = listado(&foto)
        .rows
        .iter()
        .find(|r| r.display_name == "notas.txt")
        .expect("el fichero está");
    let celda = fila
        .cells
        .iter()
        .find(|c| c.column == "attr:posix.mode")
        .expect("la celda existe");
    assert_eq!(
        celda.text.as_deref(),
        Some("-rw-r--r--"),
        "el catálogo convierte el número en un modo legible"
    );
}
