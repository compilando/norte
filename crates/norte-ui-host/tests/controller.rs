//! El controlador: un solo escritor, y lo que eso garantiza.
//!
//! Estos tests no necesitan daemon. El backend es una tabla determinista, que
//! es exactamente lo que el plan pedía: el host tiene que ser útil a un test
//! headless antes de que exista renderer alguno.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use norte_proto::VPath;
use norte_ui_host::action::UiAction;
use norte_ui_host::bridge::{ActionAck, RowKey, StaleAction};
use norte_ui_host::controller::{UiHost, UiHostOptions, Update};
use norte_ui_host::dto::{SlotView, UiNotice, UiUpdate};

mod backend_falso;
use backend_falso::Falso;

/// Unos ajustes de columnas con estos ids, para todos los esquemas.
fn columnas_de(ids: &[&str]) -> norte_frontend::columns::ColumnsSettings {
    let cfg = norte_config::ColumnsConfig {
        default_columns: Some(ids.iter().map(|s| (*s).to_owned()).collect()),
        ..norte_config::ColumnsConfig::default()
    };
    norte_frontend::columns::ColumnsSettings::resolve(&cfg)
}

fn dir() -> VPath {
    VPath::parse("mem:///casa").expect("vpath de test")
}

/// La configuración de un host de prueba: la de fábrica, con la fila `..`
/// APAGADA.
///
/// Apagada a propósito y no por descuido. Estos tests razonan sobre índices
/// de listado —la fila 0 es la primera entrada— y una fila más al principio
/// los desplazaría todos sin decir nada de lo que cada uno prueba. La fila
/// tiene sus propios tests, y son los que la encienden.
fn ajustes_de_prueba() -> norte_frontend::config::FrontendConfig {
    let mut cfg = norte_ui_host::ajustes_por_defecto();
    cfg.common.ui_parent_entry = Some(false);
    cfg
}

async fn host(nombres: Vec<&'static str>) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend: Falso::con(&nombres),
        initial_dir: dir(),
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

/// Arrancar produce EXACTAMENTE una foto, y describe una pantalla que ya
/// existe: el listado se pidió antes de publicarla.
#[tokio::test]
async fn arrancar_da_un_snapshot_con_el_listado_dentro() {
    let (_h, snap) = host(vec!["b.txt", "a.txt"]).await;
    let SlotView::Browser(b) = &snap.slots[0] else {
        panic!("el primer hueco es un listado");
    };
    assert_eq!(b.rows.len(), 2);
    // Ordenado con el comparador COMPARTIDO, no con el del host.
    assert_eq!(b.rows[0].display_name, "a.txt");
    assert_eq!(b.cursor, Some(RowKey(0)));
}

/// Dos asas del host siguen siendo UN escritor: las acciones se aplican en
/// orden y la secuencia no salta.
#[tokio::test]
async fn dos_asas_un_escritor_y_las_secuencias_no_saltan() {
    let (h, _snap) = host(vec!["a", "b", "c", "d"]).await;
    let h2 = h.clone();
    let mut sub = h.subscribe();

    for _ in 0..3 {
        let ack = h2
            .dispatch(UiAction::MoveCursor {
                slot_id: 1,
                delta: 1,
            })
            .await
            .expect("host vivo");
        assert!(matches!(ack, ActionAck::Applied { .. }));
    }

    let mut vistas = Vec::new();
    for _ in 0..3 {
        match sub.recv().await.expect("hay actualización") {
            Update::Message(m) => vistas.push(m.sequence),
            Update::Lagged => panic!("no debería haber retraso con tres mensajes"),
        }
    }
    assert_eq!(
        vistas,
        vec![1, 2, 3],
        "una secuencia por acción, sin saltos"
    );
}

/// Un click sobre una fila que ya no existe no muta nada, y lo dice.
#[tokio::test]
async fn una_fila_que_no_existe_es_una_carrera_no_un_error() {
    let (h, snap) = host(vec!["a"]).await;
    let ack = h
        .dispatch(UiAction::SelectRow {
            generation: listado(&snap).generation,
            slot_id: 1,
            key: RowKey(99),
        })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Generation
        }
    );
}

/// Un suscriptor lento NO hace crecer la memoria del host: se entera de que
/// se quedó atrás y pide una foto.
#[tokio::test]
async fn un_suscriptor_lento_se_entera_y_pide_foto() {
    let (h, _snap) = host(vec!["a", "b", "c"]).await;
    let mut sub = h.subscribe();
    // Muchas más actualizaciones que huecos tiene el buffer.
    for _ in 0..200 {
        let _ = h
            .dispatch(UiAction::MoveCursor {
                slot_id: 1,
                delta: 1,
            })
            .await;
    }
    match sub.recv().await.expect("algo llega") {
        Update::Lagged => {}
        Update::Message(m) => panic!("debería avisar del retraso, no dar {:?}", m.sequence),
    }
    // Y la recuperación es una foto completa.
    let ack = h.dispatch(UiAction::Resync).await.expect("host vivo");
    assert!(matches!(ack, ActionAck::Applied { .. }));
}

/// Que un suscriptor se vaya no para el host.
#[tokio::test]
async fn si_el_suscriptor_se_va_el_host_sigue() {
    let (h, _snap) = host(vec!["a"]).await;
    drop(h.subscribe());
    let ack = h
        .dispatch(UiAction::MoveCursor {
            slot_id: 1,
            delta: 1,
        })
        .await
        .expect("el host sigue vivo sin nadie escuchando");
    assert!(matches!(ack, ActionAck::Applied { .. }));
}

/// Apagar dice si quedó algo a medias, y después el host ya no acepta nada.
#[tokio::test]
async fn apagar_informa_y_cierra() {
    let (h, _snap) = host(vec!["a"]).await;
    let mut sub = h.subscribe();
    let informe = h.shutdown().await.expect("apaga");
    assert!(!informe.incomplete);
    let ultimo = sub.recv().await.expect("el último mensaje llega");
    match ultimo {
        Update::Message(m) => assert!(matches!(
            m.payload,
            UiUpdate::Notice(UiNotice::Shutdown { .. })
        )),
        Update::Lagged => panic!("sin retraso aquí"),
    }
    assert!(
        h.dispatch(UiAction::Resync).await.is_err(),
        "un host apagado no acepta más acciones"
    );
}

/// La ventana visible acota lo que viaja: pedir cuarenta filas de un listado
/// grande manda cuarenta, no el listado.
#[tokio::test]
async fn solo_viaja_la_ventana_visible() {
    let nombres: Vec<&'static str> = vec!["f"; 500];
    let (h, _snap) = host(nombres).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 10,
        count: 40,
    })
    .await
    .expect("host vivo");
    let Update::Message(m) = sub.recv().await.expect("llega") else {
        panic!("sin retraso");
    };
    let UiUpdate::Patch(p) = &m.payload else {
        panic!("un parche");
    };
    match &p.changes[0] {
        norte_ui_host::dto::ViewChange::Rows {
            rows,
            first_visible,
            ..
        } => {
            assert_eq!(rows.len(), 40, "solo la ventana");
            assert_eq!(*first_visible, 10);
        }
        otro => panic!("se esperaban filas: {otro:?}"),
    }
}

/// El mismo árbol, sin envolver: para los tests que necesitan tocar sus
/// canales antes de arrancar el host.
fn arbol_como_falso() -> Falso {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![
            (b"docs".to_vec(), true),
            (b"notas.txt".to_vec(), false),
            (vec![0x63, 0x61, 0x66, 0xC3, 0x28], false),
        ],
    );
    f.pon(
        "mem:///casa/docs",
        vec![(b"a.md".to_vec(), false), (b"b.md".to_vec(), false)],
    );
    f
}

/// Un árbol de dos niveles para navegar de verdad.
fn arbol() -> Arc<Falso> {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![
            (b"docs".to_vec(), true),
            (b"notas.txt".to_vec(), false),
            // Un nombre que NO es UTF-8: tiene que sobrevivir como bytes y
            // llegar al renderer marcado, jamás rechazado ni silenciado.
            (vec![0x63, 0x61, 0x66, 0xC3, 0x28], false),
        ],
    );
    f.pon(
        "mem:///casa/docs",
        vec![(b"a.md".to_vec(), false), (b"b.md".to_vec(), false)],
    );
    Arc::new(f)
}

async fn host_arbol(backend: Arc<Falso>) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
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

fn listado(snap: &norte_ui_host::ViewSnapshot) -> &norte_ui_host::dto::BrowserSlotView {
    let SlotView::Browser(b) = &snap.slots[0] else {
        panic!("el primer hueco es un listado");
    };
    b
}

/// Espera la siguiente foto (una navegación manda una).
async fn siguiente_foto(sub: &mut norte_ui_host::UiSubscription) -> norte_ui_host::ViewSnapshot {
    loop {
        match sub.recv().await.expect("el host sigue vivo") {
            Update::Message(m) => {
                if let UiUpdate::Snapshot(s) = m.payload {
                    return *s;
                }
            }
            // Quedarse atrás no rompe la espera: significa «pide una foto»,
            // y una foto es justo lo que se está esperando.
            Update::Lagged => {}
        }
    }
}

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
async fn hasta<T>(f: &Falso, que_esperaba: &str, que: impl Fn(&Falso) -> Option<T>) -> T {
    f.hasta(que_esperaba, que).await
}

/// Espera a que el doble tenga al menos `n` anotaciones en la lista que se le
/// señala, y devuelve una copia.
///
/// Es la forma corta de [`hasta`] para el caso de lejos más común: «ya se
/// encoló lo que tenía que encolarse». Devuelve el `Vec` clonado y no el
/// `MutexGuard` a propósito: un guard no cruza un `await`.
async fn anotados<T: Clone>(
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
async fn foto_hasta<T>(
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
async fn asentar() {
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
fn tecla_mod(k: &str, ctrl: bool, shift: bool) -> UiAction {
    UiAction::Key(norte_ui_host::keys::KeyInput {
        key: k.to_owned(),
        ctrl,
        alt: false,
        shift,
        meta: false,
    })
}

fn tecla(k: &str) -> UiAction {
    UiAction::Key(norte_ui_host::keys::KeyInput {
        key: k.to_owned(),
        ctrl: false,
        alt: false,
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
async fn host_con_layout(
    backend: Arc<Falso>,
    layout: &str,
    viewport: (u16, u16),
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
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
fn sesion_guardada(
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
            sort: norte_frontend::SortSpec::default(),
            columns: Vec::new(),
            show_hidden: false,
            touched_ms: 0,
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
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let informe = h.shutdown().await.expect("apaga");
    assert!(!informe.incomplete, "no escribir no es dejar algo a medias");
    assert!(backend.escrito.lock().expect("escrito").is_none());
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

/// Espera la siguiente actualización que traiga tasks.
async fn siguientes_tasks(
    sub: &mut norte_ui_host::UiSubscription,
) -> Vec<norte_ui_host::dto::TaskView> {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv())
            .await
            .expect("una actualización con tasks, no un cuelgue")
            .expect("el host sigue vivo");
        match siguiente {
            Update::Message(m) => {
                if let UiUpdate::Patch(p) = &m.payload {
                    for c in &p.changes {
                        if let norte_ui_host::dto::ViewChange::Tasks { tasks: t } = c {
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
async fn siguientes_dialogos(
    sub: &mut norte_ui_host::UiSubscription,
) -> Vec<norte_ui_host::dto::DialogView> {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv())
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
        match tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv())
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

// ---------------------------------------------------------------------------
// La disposición proyectada, y un snapshot que de verdad reemplaza (fase 3).
// ---------------------------------------------------------------------------

/// Espera la siguiente actualización que traiga el VISOR.
///
/// Viaja como parche desde la versión 6: una foto entera por cada línea de
/// scroll mandaba las filas de todos los listados de debajo.
async fn siguiente_visor(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::ViewerView> {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv())
            .await
            .expect("una actualización con visor, no un cuelgue")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente {
            match &m.payload {
                UiUpdate::Patch(p) => {
                    for c in &p.changes {
                        if let norte_ui_host::dto::ViewChange::Viewer { viewer } = c {
                            return viewer.clone();
                        }
                    }
                }
                UiUpdate::Snapshot(s) => return s.viewer.clone(),
                UiUpdate::Notice(_) => {}
            }
        }
    }
    panic!("no llegó ninguna actualización con visor");
}

/// Espera la siguiente actualización que traiga disposición.
async fn siguiente_disposicion(
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::dto::LayoutView {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv())
            .await
            .expect("una actualización con disposición, no un cuelgue")
            .expect("el host sigue vivo");
        match siguiente {
            Update::Message(m) => {
                if let UiUpdate::Patch(p) = &m.payload {
                    for c in &p.changes {
                        if let norte_ui_host::dto::ViewChange::Layout(l) = c {
                            return l.clone();
                        }
                    }
                }
                if let UiUpdate::Snapshot(s) = &m.payload {
                    return s.layout.clone();
                }
            }
            Update::Lagged => panic!("sin retraso en este test"),
        }
    }
    panic!("no llegó ninguna actualización con disposición");
}

/// El renderer no reparte la pantalla: la recibe repartida.
///
/// Sin esto, colocar dos paneles sería una regla de presentación escrita en
/// TypeScript — exactamente lo que la decisión D14 prohíbe—, y encima una
/// distinta de la del TUI.
#[tokio::test]
async fn el_snapshot_reparte_la_pantalla_por_el_renderer() {
    let (_h, snap) = host_con_layout(arbol(), "orthodox", (120, 40)).await;
    let l = &snap.layout;
    assert_eq!(l.cells, (120, 40), "el reparto es del tamaño que se le dio");
    let listados: Vec<&norte_ui_host::dto::SlotPlacement> = l
        .placements
        .iter()
        .filter(|p| [1, 2].contains(&p.slot_id))
        .collect();
    assert_eq!(
        listados.len(),
        2,
        "la disposición de siempre son dos listados"
    );
    let izq = listados[0];
    let der = listados[1];
    assert!(
        izq.width > 0 && izq.height > 0,
        "un hueco pintable tiene área"
    );
    assert!(
        izq.x + izq.width <= der.x,
        "y los dos listados no se solapan: {izq:?} vs {der:?}"
    );
}

/// Activo y destino los resuelve el host, con la MISMA regla que el TUI: el
/// destino es el otro listado visible. El renderer solo los pinta.
#[tokio::test]
async fn los_roles_los_resuelve_el_host_no_el_renderer() {
    use norte_ui_host::dto::SlotRole;
    let (h, snap) = host_con_layout(arbol(), "orthodox", (120, 40)).await;
    let rol = |l: &norte_ui_host::dto::LayoutView, id: u32| {
        l.placements
            .iter()
            .find(|p| p.slot_id == id)
            .and_then(|p| p.role)
    };
    assert_eq!(rol(&snap.layout, 1), Some(SlotRole::Active));
    assert_eq!(rol(&snap.layout, 2), Some(SlotRole::Target));

    let mut sub = h.subscribe();
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host vivo");
    let despues = siguiente_disposicion(&mut sub).await;
    assert_eq!(rol(&despues, 2), Some(SlotRole::Active), "el foco cambió");
    assert_eq!(
        rol(&despues, 1),
        Some(SlotRole::Target),
        "y el destino también"
    );
}

/// Cambiar el tamaño de la ventana reparte otra vez, y el renderer se entera
/// por el mismo canal ordenado que todo lo demás.
#[tokio::test]
async fn un_resize_reparte_otra_vez_y_lo_dice() {
    let (h, snap) = host_con_layout(arbol(), "orthodox", (120, 40)).await;
    let ancho_antes = snap
        .layout
        .placements
        .iter()
        .find(|p| p.slot_id == 1)
        .expect("el listado izquierdo se pinta")
        .width;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SetViewport {
        width: 200,
        height: 60,
    })
    .await
    .expect("host vivo");
    let despues = siguiente_disposicion(&mut sub).await;
    assert_eq!(despues.cells, (200, 60));
    let ancho_despues = despues
        .placements
        .iter()
        .find(|p| p.slot_id == 1)
        .expect("sigue pintándose")
        .width;
    assert!(
        ancho_despues > ancho_antes,
        "una ventana más ancha da listados más anchos: {ancho_antes} -> {ancho_despues}"
    );
}

/// Un snapshot REEMPLAZA el estado del renderer, así que tiene que llevarlo
/// entero. Si un resync se comiera el diálogo abierto, el renderer se quedaría
/// pintando una pantalla sin la pregunta que está esperando respuesta — y la
/// operación destructiva seguiría ahí, viva y sin confirmar.
#[tokio::test]
async fn un_resync_no_se_come_el_dialogo_abierto() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F8")).await.expect("host vivo");
    let abierto = siguientes_dialogos(&mut sub).await;
    assert_eq!(abierto.len(), 1);

    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(
        foto.dialogs, abierto,
        "el snapshot lleva el diálogo que hay abierto"
    );
}

/// Lo mismo para el tablero: una copia en marcha no puede desaparecer porque
/// el renderer haya pedido una foto nueva.
#[tokio::test]
async fn un_resync_no_se_come_las_tasks_vivas() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F7")).await.expect("host vivo");
    let dialogos = siguientes_dialogos(&mut sub).await;
    let id = dialogos[0].id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "nueva".to_owned(),
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
    let vivas = siguientes_tasks(&mut sub).await;
    assert!(!vivas.is_empty(), "hay una task en el tablero");

    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(foto.tasks, vivas, "el snapshot lleva el tablero entero");
}

/// Espera a que el falso reciba un lote de `dir_size` (el brazo lo lanza en
/// una tarea aparte, así que no está listo al volver del dispatch).
async fn siguiente_recuento(falso: &Falso) -> Vec<VPath> {
    for _ in 0..200 {
        if let Some(lote) = falso.recuentos.lock().expect("recuentos").first() {
            return lote.clone();
        }
        tokio::task::yield_now().await;
    }
    panic!("nadie pidió contar");
}

/// El mensaje de la barra tras un desenlace, reintentando: el progreso viaja
/// por su propio canal y el parche puede tardar un tick en salir.
async fn siguiente_mensaje_de_estado(
    h: &UiHost,
    sub: &mut norte_ui_host::controller::UiSubscription,
) -> String {
    // Cada vuelta es un viaje de ida y vuelta al actor: el bucle avanza al
    // ritmo del host, no al del reloj.
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(sub).await;
        if let Some(m) = foto.status.message.clone() {
            return m;
        }
    }
    panic!("la barra no dijo nada");
}

/// `pane.dir-size` cuenta lo MARCADO, y en UNA sola Task (#139, #290).
///
/// Una Task por marca obligaría a quien pregunta a sumar los bytes y los
/// ilegibles por su cuenta, y esos dos no se suman igual.
#[tokio::test]
async fn contar_el_tamano_manda_las_marcas_en_un_solo_lote() {
    let backend = Arc::new(arbol_como_falso());
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let epoca = listado(&snap).generation;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::MarkRange {
        slot_id: 1,
        from: RowKey(1),
        to: RowKey(2),
        generation: epoca,
    })
    .await
    .expect("host vivo");
    let _ = sub.recv().await.expect("host vivo");

    ejecutar_por_paleta(&h, &mut sub, "pane.dir-size").await;

    let lote = siguiente_recuento(&backend).await;
    assert_eq!(lote.len(), 2, "las dos marcas, en un solo lote: {lote:?}");
    assert_eq!(
        backend.recuentos.lock().expect("recuentos").len(),
        1,
        "y una sola Task"
    );
}

/// **El total de un recuento se DICE.** `fs.dir_size` no publica nada: su
/// resultado es su progreso terminal, así que sin esto la ventana lanzaría la
/// cuenta, la vería terminar y no diría jamás cuánto ocupaba.
#[tokio::test]
async fn el_total_de_un_recuento_llega_a_la_barra() {
    let backend = Arc::new(arbol_como_falso());
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "pane.dir-size").await;
    let _ = siguiente_recuento(&backend).await;

    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("hay task");
    tx.send_modify(|p| {
        p.state = norte_proto::TaskState::Completed;
        p.bytes_done = 2048;
        p.entries_done = 3;
    });

    let mensaje = siguiente_mensaje_de_estado(&h, &mut sub).await;
    assert!(
        mensaje.contains('3'),
        "el total dice cuántas entradas: {mensaje}"
    );
}

/// Y lo que NO se pudo leer cambia la frase: un recuento sirve para decidir si
/// algo CABE, así que un total redondo sin haber podido contarlo entero es una
/// respuesta equivocada, no una incompleta.
#[tokio::test]
async fn un_recuento_con_ilegibles_no_da_el_total_a_secas() {
    let backend = Arc::new(arbol_como_falso());
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "pane.dir-size").await;
    let _ = siguiente_recuento(&backend).await;

    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("hay task");
    tx.send_modify(|p| {
        p.state = norte_proto::TaskState::Completed;
        p.bytes_done = 2048;
        p.entries_done = 3;
        p.unreadable = Some(2);
    });

    let mensaje = siguiente_mensaje_de_estado(&h, &mut sub).await;
    assert!(
        mensaje.contains('2'),
        "tiene que decir cuántas no pudo leer: {mensaje}"
    );
    assert_ne!(
        mensaje,
        norte_i18n::ta_in(
            norte_i18n::Lang::Es,
            "msg-dir-size",
            &[("size", "2,0 KB"), ("count", "3")]
        ),
        "y no puede ser la frase del total redondo"
    );
}

/// Sin marcas se cuenta lo que hay bajo el CURSOR: es la misma fuente de
/// «sobre qué opera esto» que usa una transferencia, y no un segundo respaldo
/// que se pueda separar del primero.
#[tokio::test]
async fn contar_el_tamano_sin_marcas_usa_el_cursor() {
    let backend = Arc::new(arbol_como_falso());
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.dir-size").await;

    let lote = siguiente_recuento(&backend).await;
    assert_eq!(lote.len(), 1, "solo el del cursor: {lote:?}");
}

/// Un barrido con el ratón marca el rango entero de UNA vez, con la regla
/// compartida: qué entra en el rango no lo decide quien pinta.
#[tokio::test]
async fn un_rango_se_marca_de_una_vez() {
    let (h, snap) = host(vec!["a", "b", "c", "d", "e"]).await;
    assert_eq!(listado(&snap).marks, 0);
    let epoca = listado(&snap).generation;
    let mut sub = h.subscribe();
    let ack = h
        .dispatch(UiAction::MarkRange {
            slot_id: 1,
            from: RowKey(3),
            to: RowKey(1),
            generation: epoca,
        })
        .await
        .expect("host vivo");
    assert!(matches!(ack, ActionAck::Applied { .. }));
    let _ = sub.recv().await.expect("el host sigue vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(
        listado(&foto).marks,
        3,
        "los extremos entran, y el orden en que se dan da igual"
    );
}

/// Un extremo de una generación anterior no marca A MEDIAS: marcar hasta un
/// sitio que ya no es el que el usuario señaló es peor que no marcar nada.
#[tokio::test]
async fn un_rango_con_un_extremo_viejo_no_marca_nada() {
    let (h, snap) = host(vec!["a", "b"]).await;
    let epoca = listado(&snap).generation;
    let ack = h
        .dispatch(UiAction::MarkRange {
            slot_id: 1,
            from: RowKey(0),
            to: RowKey(99),
            generation: epoca,
        })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Generation
        }
    );
}

/// Un listado PEREZOSO —el del provider local (#52): sin tamaño ni fecha—
/// no deja las columnas en blanco: el host sonda lo que se ve.
///
/// El backend de tabla daba tamaño en el propio listado, así que esta
/// diferencia solo se vio cuando el spike de Tauri pintó un directorio real
/// y enseñó dos columnas vacías.
#[tokio::test]
async fn un_listado_perezoso_se_sondea_y_las_celdas_se_llenan() {
    let mut f = Falso {
        lazy: true,
        ..Falso::default()
    };
    f.pon(
        "mem:///casa",
        vec![(b"a.txt".to_vec(), false), (b"b.txt".to_vec(), false)],
    );
    let backend = Arc::new(f);
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    assert!(
        listado(&snap).rows[0]
            .cells
            .iter()
            .all(|c| c.text.is_none()),
        "el listado llega sin tamaño, como el de verdad: {:?}",
        listado(&snap).rows[0].cells
    );

    let mut sub = h.subscribe();
    // El sondeo sale solo, en cuanto el listado aterriza. Se pide foto tras
    // foto: lo que importa es que la pantalla acabe con las celdas llenas, no
    // por qué mensaje llegó. Cada vuelta es un viaje al actor, no una espera.
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        let lleno = listado(&foto)
            .rows
            .iter()
            .any(|r| r.cells.iter().any(|c| c.text.is_some()));
        if lleno {
            assert!(
                !backend.sondeos.lock().expect("sondeos").is_empty(),
                "y se llenaron sondeando, no inventando"
            );
            return;
        }
    }
    panic!("las celdas siguen en blanco: el sondeo no llegó");
}

/// Lo ya sondeado no se vuelve a sondear: un `stat` por repintado sería un
/// bucle contra el daemon, y uno que falla lo sería para siempre.
#[tokio::test]
async fn lo_sondeado_no_se_vuelve_a_pedir() {
    let mut f = Falso {
        lazy: true,
        ..Falso::default()
    };
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    for _ in 0..5 {
        h.dispatch(UiAction::SetVisibleRange {
            slot_id: 1,
            first: 0,
            count: 40,
        })
        .await
        .expect("host vivo");
    }
    // Se espera al PRIMER sondeo y se deja correr lo demás: si los cinco
    // repintados sondearan, los otros cuatro ya estarían encolados.
    hasta(&backend, "el primer sondeo", |f| {
        (!f.sondeos.lock().expect("sondeos").is_empty()).then_some(())
    })
    .await;
    asentar().await;
    let sondeos = backend.sondeos.lock().expect("sondeos").clone();
    assert_eq!(
        sondeos.len(),
        1,
        "una entrada se sondea UNA vez, no una por repintado: {sondeos:?}"
    );
}

// ---------------------------------------------------------------------------
// Dos paneles son DOS paneles (fase 4, tarea 4.1).
// ---------------------------------------------------------------------------

/// La rueda sobre el panel que NO tiene el foco mueve ESE panel, y no le
/// roba el foco al otro.
///
/// Declarar qué filas se ven no es actuar sobre el listado: es decir dónde
/// está mirando el usuario. Tratarlo como una acción del panel activo dejaba
/// el segundo panel de una disposición de dos sin poder desplazarse.
#[tokio::test]
async fn el_panel_inactivo_se_desplaza_sin_robar_el_foco() {
    let (h, snap) = host_con_layout(arbol(), "orthodox", (200, 60)).await;
    assert_eq!(snap.focus, Some(1));
    let mut sub = h.subscribe();
    let ack = h
        .dispatch(UiAction::SetVisibleRange {
            slot_id: 2,
            first: 1,
            count: 10,
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "el panel de al lado también se desplaza: {ack:?}"
    );

    let mut visto = None;
    for _ in 0..10 {
        let siguiente = tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv())
            .await
            .expect("una actualización antes del plazo")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Rows {
                    slot_id,
                    first_visible,
                    ..
                } = c
                {
                    visto = Some((*slot_id, *first_visible));
                }
            }
        }
        if visto.is_some() {
            break;
        }
    }
    assert_eq!(
        visto,
        Some((2, 1)),
        "las filas que viajan son las del hueco que se desplazó"
    );

    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(foto.focus, Some(1), "y el foco no se movió");
}

/// El relleno de un listado pinta en SU panel.
///
/// Los lotes que drenan por detrás se anunciaban siempre como filas del panel
/// activo, así que en una disposición de dos el segundo se quedaba con lo que
/// cupo en la primera página hasta que algo lo tocara.
#[tokio::test]
async fn el_relleno_de_un_panel_no_se_anuncia_en_el_otro() {
    let mut f = Falso::default();
    // Más entradas que la primera página (100), para que haya relleno.
    let muchas: Vec<(Vec<u8>, bool)> = (0..300)
        .map(|i| (format!("f{i:04}.txt").into_bytes(), false))
        .collect();
    f.pon("mem:///casa", muchas);
    let (h, _snap) = host_con_layout(Arc::new(f), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 2,
        first: 0,
        count: 20,
    })
    .await
    .expect("host vivo");

    let mut slots = std::collections::BTreeSet::new();
    for _ in 0..40 {
        let Ok(Some(siguiente)) =
            tokio::time::timeout(std::time::Duration::from_millis(300), sub.recv()).await
        else {
            break;
        };
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Rows { slot_id, .. } = c {
                    slots.insert(*slot_id);
                }
            }
        }
    }
    assert!(
        slots.contains(&2),
        "el segundo panel también recibe sus filas: {slots:?}"
    );
}

/// El tabulador cambia de panel, con el MISMO recorrido que el TUI: solo lo
/// que se ve y solo lo que se puede enfocar.
///
/// Sin esto, en una ventana de dos paneles el teclado no podía cambiar de
/// panel: había que usar el ratón, que es exactamente la clase de diferencia
/// entre frontends que la capa compartida existe para no tener.
#[tokio::test]
async fn el_tabulador_cambia_de_panel() {
    use norte_ui_host::dto::SlotRole;
    let (h, snap) = host_con_layout(arbol(), "orthodox", (200, 60)).await;
    assert_eq!(snap.focus, Some(1));
    let mut sub = h.subscribe();

    h.dispatch(tecla("Tab")).await.expect("host vivo");
    let l = siguiente_disposicion(&mut sub).await;
    let rol = |l: &norte_ui_host::dto::LayoutView, id: u32| {
        l.placements
            .iter()
            .find(|p| p.slot_id == id)
            .and_then(|p| p.role)
    };
    assert_eq!(rol(&l, 2), Some(SlotRole::Active), "el foco pasó al otro");

    // Y vuelve: el recorrido CICLA, no se queda en el último.
    h.dispatch(tecla("Tab")).await.expect("host vivo");
    let vuelta = siguiente_disposicion(&mut sub).await;
    assert_eq!(rol(&vuelta, 1), Some(SlotRole::Active));
}

/// El foco jamás aterriza en un hueco que no se enfoca (la barra de estado,
/// la franja de tareas): el recorrido es el de la capa compartida.
#[tokio::test]
async fn el_tabulador_no_enfoca_la_barra_de_estado() {
    let (h, _snap) = host_con_layout(arbol(), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    for _ in 0..6 {
        h.dispatch(tecla("Tab")).await.expect("host vivo");
        let l = siguiente_disposicion(&mut sub).await;
        let activo = l
            .placements
            .iter()
            .find(|p| p.role == Some(norte_ui_host::dto::SlotRole::Active))
            .map(|p| p.slot_id);
        assert!(
            matches!(activo, Some(1 | 2)),
            "el foco solo pasa por los listados, no por {activo:?}"
        );
    }
}

/// Designar destino nunca se señala a uno mismo: un destino igual al panel
/// con el foco sería pedirle a una copia que se copie encima.
///
/// De paso, esto prueba el camino de una CAPA de usuario: el binding no está
/// en ningún preset (#228), así que la tecla la ata el keymap efectivo que
/// recibe el host — el mismo que construye un arranque de verdad.
#[tokio::test]
async fn el_destino_nunca_es_el_panel_enfocado() {
    use norte_frontend::keymap::{Effective, Screen, parse_keymap, parse_keymap_layer};
    use norte_ui_host::dto::SlotRole;

    let preset = parse_keymap(
        norte_frontend::keymap::presets::source("orthodox").expect("preset de fábrica"),
    )
    .expect("preset parsea");
    let capa = parse_keymap_layer(
        r#"
[pane]
prepend_keymap = [{ on = ["ctrl+t"], run = "layout.set-target" }]
"#,
    )
    .expect("capa parsea");
    let keymap = Effective::build_for(
        &preset,
        &[capa],
        norte_ui_host::commands::IMPLEMENTADOS,
        Screen::Browse,
    )
    .expect("efectivo");

    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        locale: "es".to_owned(),
        keymap,
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("orthodox").expect("layout"),
        viewport: (200, 60),
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
    let ack = h
        .dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
            key: "t".to_owned(),
            ctrl: true,
            alt: false,
            shift: false,
            meta: false,
        }))
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "la capa del usuario ata la tecla: {ack:?}"
    );
    let l = siguiente_disposicion(&mut sub).await;
    let rol_de = |r: SlotRole| {
        l.placements
            .iter()
            .find(|p| p.role == Some(r))
            .map(|p| p.slot_id)
    };
    assert_eq!(rol_de(SlotRole::Active), Some(1));
    assert_eq!(
        rol_de(SlotRole::Target),
        Some(2),
        "el destino es SIEMPRE otro hueco"
    );
}

// ---------------------------------------------------------------------------
// Cabeceras y orden (fase 4, tarea 4.2).
// ---------------------------------------------------------------------------

/// El listado viaja con sus CABECERAS: etiqueta ya traducida, alineación y
/// cuál manda el orden. El renderer las pinta; no las inventa ni las traduce.
#[tokio::test]
async fn el_listado_lleva_sus_cabeceras() {
    let (_h, snap) = host_arbol(arbol()).await;
    let b = listado(&snap);
    let ids: Vec<&str> = b.columns.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["name", "size", "mtime"],
        "las columnas configuradas, en orden y con el nombre delante"
    );
    for c in &b.columns {
        assert!(!c.label.is_empty(), "cada cabecera trae su etiqueta: {c:?}");
    }
    let nombre = &b.columns[0];
    assert_eq!(
        nombre.sort.as_deref(),
        Some("asc"),
        "y dice cuál ordena y en qué sentido"
    );
    assert!(b.columns[1].sort.is_none(), "las demás, no");
}

/// Ordenar por una columna es del host: misma regla que el TUI —la misma
/// columna invierte, otra columna empieza ascendente— y el cursor se queda
/// en la MISMA entrada, no en la misma fila.
#[tokio::test]
async fn ordenar_por_columna_usa_la_regla_compartida() {
    let (h, snap) = host_arbol(arbol()).await;
    // Los FICHEROS, sin el directorio: `dirs_first` los agrupa aparte y ese
    // grupo va siempre ascendente — invertir el orden no lo toca.
    let ficheros = |s: &norte_ui_host::ViewSnapshot| -> Vec<String> {
        listado(s)
            .rows
            .iter()
            .filter(|r| r.kind != norte_ui_host::dto::RowKind::Dir)
            .map(|r| r.display_name.clone())
            .collect()
    };
    let antes = ficheros(&snap);
    assert!(antes.len() >= 2, "hay ficheros que ordenar: {antes:?}");
    let mut sub = h.subscribe();

    h.dispatch(UiAction::SortBy {
        slot_id: 1,
        column: "name".to_owned(),
    })
    .await
    .expect("host vivo");
    let _ = sub.recv().await.expect("el host sigue vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let invertido = siguiente_foto(&mut sub).await;
    let despues = ficheros(&invertido);
    let mut al_reves = antes.clone();
    al_reves.reverse();
    assert_eq!(
        despues, al_reves,
        "la misma columna dos veces invierte el sentido"
    );
    assert_eq!(
        listado(&invertido).rows[0].kind,
        norte_ui_host::dto::RowKind::Dir,
        "y los directorios siguen primero: invertir no toca su grupo"
    );
    assert_eq!(
        listado(&invertido).columns[0].sort.as_deref(),
        Some("desc"),
        "y la cabecera lo dice"
    );
}

/// Una columna que no ordena —o que no está— no altera el listado, y se
/// responde en vez de callarse.
#[tokio::test]
async fn ordenar_por_una_columna_que_no_ordena_se_dice() {
    let (h, _snap) = host_arbol(arbol()).await;
    let ack = h
        .dispatch(UiAction::SortBy {
            slot_id: 1,
            column: "no-existe".to_owned(),
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Unavailable { .. }),
        "se dice que esa columna no ordena: {ack:?}"
    );
}

// ---------------------------------------------------------------------------
// El visor (fase 4, tarea 4.3).
// ---------------------------------------------------------------------------

/// F3 sobre un fichero lo ABRE: se lee una cabecera acotada, se decodifica
/// con la detección compartida y lo que viaja son líneas ya saneadas.
#[tokio::test]
async fn ver_un_fichero_lo_decodifica_y_lo_pinta() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
    f.contenido.insert(
        "mem:///casa/notas.txt".to_owned(),
        b"primera\nsegunda\ntercera\n".to_vec(),
    );
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    let v = siguiente_visor(&mut sub)
        .await
        .expect("el visor está abierto");
    assert!(v.path_display.ends_with("notas.txt"));
    assert_eq!(v.total_rows, 3, "tres líneas");
    assert!(
        v.lines.iter().any(|l| l == "primera"),
        "y el texto llega decodificado: {:?}",
        v.lines
    );
    assert!(!v.hex, "un texto no se enseña en hexadecimal");
    assert_eq!(v.encoding.to_ascii_uppercase(), "UTF-8");
}

/// Con el visor abierto, las teclas son SUYAS: el mismo mapa de la pantalla
/// `viewer` que usa el TUI, no un segundo keymap escrito aquí.
#[tokio::test]
async fn con_el_visor_abierto_las_teclas_son_del_visor() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
    let mut cuerpo = String::new();
    for i in 0..200 {
        use std::fmt::Write as _;
        let _ = writeln!(cuerpo, "linea {i}");
    }
    let cuerpo = cuerpo.into_bytes();
    f.contenido
        .insert("mem:///casa/notas.txt".to_owned(), cuerpo);
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F3")).await.expect("host vivo");
    let abierto = siguiente_visor(&mut sub).await.expect("visor abierto");
    assert_eq!(abierto.first_line, 0);

    // `down` en el visor DESPLAZA el visor; no mueve el cursor del listado.
    h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    let bajado = siguiente_visor(&mut sub).await.expect("visor abierto");
    assert_eq!(bajado.first_line, 1);

    // Y `esc` lo cierra.
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    assert!(
        siguiente_visor(&mut sub).await.is_none(),
        "el visor se cierra"
    );

    // El listado de debajo no se movió, y para verlo hace falta una foto:
    // el visor viaja en parches justo para no mandarla en cada tecla.
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(listado(&foto).cursor, Some(RowKey(0)));
}

/// Un binario no se pinta como si fuera texto: se enseña en hexadecimal, y
/// lo decide la capa compartida por el CONTENIDO, no por la extensión.
#[tokio::test]
async fn un_binario_se_enseña_en_hexadecimal() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"raro.txt".to_vec(), false)]);
    f.contenido.insert(
        "mem:///casa/raro.txt".to_owned(),
        vec![0x00, 0x01, 0x02, 0xff, 0xfe, 0x00, 0x03],
    );
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F3")).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    let v = foto.viewer.as_ref().expect("visor");
    assert!(
        v.hex,
        "un binario entra en hexadecimal aunque se llame .txt"
    );
}

// ---------------------------------------------------------------------------
// Solo lectura: la ventana todavía no muta (revisión de seguridad de la
// tarea 3.3; el gate de salida de la fase 4 lo exige literalmente).
// ---------------------------------------------------------------------------

async fn host_solo_lectura(backend: Arc<Falso>) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
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
    .expect("arranca")
}

/// En solo lectura, F8 no abre la confirmación de borrado: lo DICE.
///
/// Un frontend que no tiene todavía el camino seguro de la fase 5 no puede
/// tener la tecla viva y el diálogo detrás; que la tecla exista en el preset
/// no es permiso.
#[tokio::test]
async fn en_solo_lectura_borrar_no_abre_nada() {
    let backend = arbol();
    let (h, _snap) = host_solo_lectura(Arc::clone(&backend)).await;
    let ack = h.dispatch(tecla("F8")).await.expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Unavailable { .. }),
        "F8 se responde, no se ejecuta: {ack:?}"
    );
    asentar().await;
    assert!(
        backend.borrados.lock().expect("borrados").is_empty(),
        "y no borra nada"
    );
}

/// Lo mismo con crear directorio.
#[tokio::test]
async fn en_solo_lectura_crear_no_crea() {
    let backend = arbol();
    let (h, _snap) = host_solo_lectura(Arc::clone(&backend)).await;
    let ack = h.dispatch(tecla("F7")).await.expect("host vivo");
    assert!(matches!(ack, ActionAck::Unavailable { .. }), "{ack:?}");
    asentar().await;
    assert!(backend.creados.lock().expect("creados").is_empty());
}

/// Y una aprobación de policy no llega siquiera a plantearse: un renderer que
/// no puede mutar tampoco puede aprobar que mute un agente.
#[tokio::test]
async fn en_solo_lectura_no_hay_aprobaciones_que_responder() {
    let backend = arbol();
    let (h, _snap) = host_solo_lectura(Arc::clone(&backend)).await;
    let ack = h
        .dispatch(UiAction::Dialog {
            id: norte_ui_host::ModalId(1),
            choice: "approve".to_owned(),
            secret: None,
        })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Modal
        },
        "no hay diálogo que responder"
    );
    assert!(
        backend.decisiones.lock().expect("decisiones").is_empty(),
        "y ninguna decisión llegó al daemon"
    );
}

/// En modo completo, la misma tecla SÍ abre la confirmación: el gate es una
/// decisión de arranque, no una amputación del host.
#[tokio::test]
async fn en_modo_completo_borrar_sigue_pidiendo_confirmacion() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F8")).await.expect("host vivo");
    let dialogos = siguientes_dialogos(&mut sub).await;
    assert_eq!(dialogos.len(), 1);
}

// ---------------------------------------------------------------------------
// Los dos BLOCKER de la revisión.
// ---------------------------------------------------------------------------

/// Navegar a un directorio grande trae el directorio ENTERO, no la primera
/// página.
///
/// `aterriza_en` limpia el testigo al aterrizar la primera página, y la task
/// de drenaje seguía mandando los lotes con ese mismo testigo: `aplicar_lote`
/// los rechazaba todos. El arranque no lo veía porque `listar_inicial`
/// restituye el testigo a mano.
#[tokio::test]
async fn navegar_a_un_directorio_grande_lo_trae_entero() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"docs".to_vec(), true)]);
    let muchas: Vec<(Vec<u8>, bool)> = (0..300)
        .map(|i| (format!("f{i:04}.txt").into_bytes(), false))
        .collect();
    f.pon("mem:///casa/docs", muchas);
    let (h, snap) = host_arbol(Arc::new(f)).await;
    let docs = listado(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("docs está")
        .key;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs,
        generation: listado(&snap).generation,
    })
    .await
    .expect("host vivo");

    // Cada vuelta es un viaje al actor: el relleno avanza entre foto y foto.
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        if listado(&foto).total_rows == Some(300) {
            return;
        }
    }
    panic!("el listado se quedó en la primera página: el relleno no aterriza");
}

/// Una fila de una generación anterior NO se toca.
///
/// El contrato del bridge lo promete desde el principio («un doble click
/// tardío no actúa sobre el fichero que ocupó esa fila DESPUÉS») y no había
/// nada que lo implementara: las acciones no llevaban generación.
#[tokio::test]
async fn una_fila_de_otra_generacion_no_se_marca() {
    let (h, snap) = host(vec!["a", "b", "c"]).await;
    let vieja = listado(&snap).generation;
    // Reordenar mueve TODAS las filas y sube la generación.
    h.dispatch(UiAction::SortBy {
        slot_id: 1,
        column: "name".to_owned(),
    })
    .await
    .expect("host vivo");

    let ack = h
        .dispatch(UiAction::ToggleMark {
            slot_id: 1,
            key: RowKey(0),
            generation: vieja,
        })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Generation
        },
        "la clave era de la pantalla anterior: {ack:?}"
    );
}

/// Y un rango con un extremo fuera de la ventana no se recorta: se rechaza.
///
/// `PaneState::mark_range` recorta a propósito (su contrato), así que un
/// `to: u64::MAX` marcaba el listado ENTERO — incluidas filas que el renderer
/// nunca recibió— y lo marcado es la entrada de un borrado.
#[tokio::test]
async fn un_rango_desbordado_no_marca_el_listado_entero() {
    let (h, snap) = host(vec!["a", "b", "c", "d", "e"]).await;
    let epoca = listado(&snap).generation;
    let ack = h
        .dispatch(UiAction::MarkRange {
            slot_id: 1,
            from: RowKey(0),
            to: RowKey(u64::MAX),
            generation: epoca,
        })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Generation
        },
        "un extremo que no existe invalida el rango entero"
    );
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(listado(&foto).marks, 0, "y no marcó nada");
}

// ---------------------------------------------------------------------------
// El sondeo (revisión: rust M2/M3/m1, encoding M4).
// ---------------------------------------------------------------------------

/// Una ventana más alta que una tanda de sondeo se rellena ENTERA.
///
/// `MAX_SONDEOS` acota cada tanda, y no había nada que pidiera la siguiente:
/// 200 filas con tamaño y el resto en blanco hasta que el usuario moviera
/// algo. Un tope que no se re-arma es un tope silencioso.
#[tokio::test]
async fn una_ventana_grande_se_sondea_en_tandas_hasta_el_final() {
    let mut f = Falso {
        lazy: true,
        ..Falso::default()
    };
    let muchas: Vec<(Vec<u8>, bool)> = (0..500)
        .map(|i| (format!("f{i:04}.txt").into_bytes(), false))
        .collect();
    f.pon("mem:///casa", muchas);
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 0,
        count: 500,
    })
    .await
    .expect("host vivo");

    hasta(&backend, "las 500 entradas sondeadas", |f| {
        (f.sondeos.lock().expect("sondeos").len() >= 500).then_some(())
    })
    .await;
}

/// Un sondeo que aterriza cuando el listado YA es otro no pega nada.
///
/// El guard miraba el testigo de la petición en vuelo, que tras aterrizar es
/// `None` — así que valía cero y la comparación era siempre falsa. Lo que
/// distingue un listado de otro es su ÉPOCA, que está definida siempre.
#[tokio::test]
async fn un_sondeo_de_otro_listado_no_hidrata() {
    let mut f = Falso {
        lazy: true,
        // El stat tarda: da tiempo a navegar por debajo.
        retraso_ms: 120,
        ..Falso::default()
    };
    f.pon(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"a.txt".to_vec(), false)],
    );
    f.pon("mem:///casa/docs", vec![(b"a.txt".to_vec(), false)]);
    let backend = Arc::new(f);
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let docs = listado(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("docs")
        .key;
    // Navegar mientras el sondeo del listado anterior vuela.
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs,
        generation: listado(&snap).generation,
    })
    .await
    .expect("host vivo");
    // Lo que este caso pone en vuelo: el listado de casa, el sondeo del
    // `a.txt` de FUERA —el que llega tarde y no debe hidratar— y el listado
    // de docs. Se espera a que no quede ninguna volando.
    hasta(&backend, "el sondeo tardío ya servido", |f| {
        (f.listados() >= 2 && f.en_calma()).then_some(())
    })
    .await;
    asentar().await;

    let mut sub = h.subscribe();
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    // El `a.txt` de DENTRO es otro fichero que el `a.txt` de fuera; lo que se
    // comprueba es que la pantalla es coherente, no que tenga o no tamaño.
    assert!(
        listado(&foto).path_display.ends_with("/docs"),
        "se navegó: {}",
        listado(&foto).path_display
    );
}

/// La hidratación casa por la ruta que se PIDIÓ, no por la que devuelve el
/// provider.
///
/// Un HFS+ que devuelve NFD, un SMB que devuelve otra caja o un `stat` que
/// sigue un enlace producen una respuesta cuya ruta no está en el listado.
/// Como el path pedido ya quedó marcado como sondeado, la celda se quedaba en
/// blanco para siempre.
#[tokio::test]
async fn un_provider_que_devuelve_otra_ortografia_no_deja_la_celda_en_blanco() {
    let mut f = Falso {
        lazy: true,
        // El stat contesta con el nombre en MAYÚSCULAS: otra ortografía de lo
        // mismo, como haría un servidor sin distinción de caja.
        stat_grita: true,
        ..Falso::default()
    };
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        let lleno = listado(&foto)
            .rows
            .iter()
            .any(|r| r.cells.iter().any(|c| c.text.is_some()));
        if lleno {
            return;
        }
    }
    panic!("la celda sigue en blanco: se casó por la ruta devuelta");
}

// ---------------------------------------------------------------------------
// Nombres y texto (revisión de encoding).
// ---------------------------------------------------------------------------

/// Lo que se teclea en el diálogo es lo que se crea, byte a byte.
///
/// El nombre viajaba por `clamp_display`, que recorta a 4 KiB y AÑADE `…`, y
/// `Segment::new` acepta la elipsis: se creaba un directorio con un nombre
/// que nadie tecleó. Es ADR 0061 en miniatura — un texto de pantalla que
/// acaba siendo un nombre de fichero.
#[tokio::test]
async fn el_nombre_que_se_teclea_es_el_que_se_crea() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F7")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;

    // Un nombre con un carácter de control dentro: legal en Unix, y lo que
    // se cree tiene que ser EXACTAMENTE eso.
    let crudo = "caf\u{202e}e.txt";
    h.dispatch(UiAction::DialogInput {
        id,
        text: crudo.to_owned(),
    })
    .await
    .expect("host vivo");

    // Lo que se PINTA está enmascarado y se dice que lo está.
    let pintado = siguientes_dialogos(&mut sub).await;
    assert!(
        pintado[0].input_hostile,
        "un nombre con una marca de dirección se DICE: {:?}",
        pintado[0].input
    );
    assert!(
        !pintado[0]
            .input
            .as_deref()
            .unwrap_or_default()
            .contains('\u{202e}'),
        "y no se pinta crudo"
    );

    h.dispatch(UiAction::Dialog {
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
    let nombre = creados[0]
        .file_name()
        .expect("tiene nombre")
        .as_bytes()
        .to_vec();
    assert_eq!(
        nombre,
        crudo.as_bytes(),
        "lo creado son los bytes tecleados, no su proyección"
    );
}

/// Un nombre imposible se RECHAZA en vez de recortarse.
#[tokio::test]
async fn un_nombre_desmesurado_no_se_recorta() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F7")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    let ack = h
        .dispatch(UiAction::DialogInput {
            id,
            text: "a".repeat(5000),
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Unavailable { .. }),
        "se dice que no cabe: {ack:?}"
    );
}

/// El id de una columna es una IDENTIDAD: viaja entero, y lo que se enmascara
/// es la ETIQUETA.
///
/// Enmascarar el id no es inyectivo. Dos columnas configuradas que solo se
/// diferencien en un carácter invisible daban el MISMO id enmascarado, y la
/// resolución del click hace `find`: pulsar la segunda ordenaba por la
/// primera. Es la regla del ADR 0061 sobre una superficie que el ADR no
/// cubría. Lo que el renderer PINTA es `label`; el id solo va en un `data-`.
#[tokio::test]
async fn dos_columnas_que_se_enmascaran_igual_siguen_siendo_dos() {
    let backend = arbol();
    let (h, snap) = UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
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
        // Los dos ids se enmascaran a lo MISMO: U+200B y U+202E son los dos
        // peligros de terminal y `display_name` los sustituye por U+FFFD.
        // Van por `plugin:` y no por `attr:`: los `attr:` ya los filtra
        // `is_valid_attr_id` —un id que no es legal en el wire tumbaría el
        // listado entero— y los de plugin no los filtra nadie.
        columns: columnas_de(&[
            "name",
            "plugin:acme.a\u{200b}b/x",
            "plugin:acme.a\u{202e}b/x",
        ]),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca");
    drop(h);
    let b = listado(&snap);
    let ids: Vec<&str> = b.columns.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids.len(), 3, "las tres columnas se pintan: {ids:?}");
    assert_ne!(
        ids[1], ids[2],
        "y siguen siendo DOS: enmascarar el id las fundía en una, y el `find` \
         de la resolución habría ordenado siempre por la primera"
    );
    assert!(
        ids[1].contains('\u{200b}') && ids[2].contains('\u{202e}'),
        "el id viaja ENTERO, que es lo que hace que case consigo mismo: {ids:?}"
    );
    // Lo que se PINTA sí va enmascarado.
    for c in &b.columns {
        assert!(
            !c.label.contains('\u{202e}') && !c.label.contains('\u{200b}'),
            "la etiqueta va cruda: {:?}",
            c.label
        );
    }
    // Y la celda nombra su columna con la misma identidad.
    let columnas_de_celdas: std::collections::BTreeSet<&str> = b
        .rows
        .iter()
        .flat_map(|f| f.cells.iter().map(|c| c.column.as_str()))
        .collect();
    for c in &columnas_de_celdas {
        assert!(
            ids.contains(c),
            "una celda nombra una columna que no está en la cabecera: {c:?}"
        );
    }
}

/// Una lectura del visor que llega tarde no abre nada.
///
/// F3 sobre un fichero en un montaje lento, `esc`, y segundos después el
/// visor aparecía solo — y como las teclas se enrutan por «hay visor», la
/// siguiente tecla la interpretaba otro mapa sin que nadie lo pidiera.
#[tokio::test]
async fn un_visor_que_llega_tarde_no_se_abre_solo() {
    let mut f = Falso {
        // La lectura tarda; da tiempo a cerrar.
        retraso_ms: 150,
        ..Falso::default()
    };
    f.pon("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
    f.contenido
        .insert("mem:///casa/notas.txt".to_owned(), b"hola\n".to_vec());
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    // Antes de que llegue el contenido, se cierra.
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    // La lectura tardía ya volvió: no queda ninguna en vuelo.
    hasta(&backend, "la lectura tardía servida", |f| {
        (f.servidos() >= 2 && f.en_calma()).then_some(())
    })
    .await;
    asentar().await;

    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(
        foto.viewer.is_none(),
        "el visor no se abre por su cuenta después de cerrarlo"
    );
}

/// Una disposición sin ningún listado se rechaza al ARRANCAR.
///
/// Es #242 en esta superficie: no panicaba al arrancar sino en la primera
/// tecla, dentro de la task del actor —sin log, sin caída visible— y la
/// ventana se quedaba muerta contestando `Down` para siempre.
#[tokio::test]
async fn una_disposicion_sin_listado_no_arranca() {
    let arbol_sin_listado = norte_frontend::layout::Node::slot(
        norte_frontend::layout::SlotId(1),
        norte_frontend::layout::KindId::new("status"),
    );
    let salida = UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: arbol_sin_listado,
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
    .await;
    assert!(
        matches!(
            salida,
            Err(norte_ui_host::controller::UiError::NoBrowserSlot)
        ),
        "una pantalla sin listado no es una pantalla"
    );
}

/// Las columnas se resuelven POR ESQUEMA, no una vez al arrancar.
///
/// Con una lista resuelta en el arranque, `[ui.columns.schemes.sftp]` quedaba
/// muerta: sus columnas no se pintaban y sus atributos no se pedían nunca,
/// porque los que viajan en cada listado se habían congelado con los del
/// esquema inicial.
#[tokio::test]
async fn las_columnas_de_otro_esquema_no_estan_muertas() {
    let cfg = norte_config::ColumnsConfig {
        default_columns: Some(vec!["name".to_owned(), "size".to_owned()]),
        schemes: [(
            "mem".to_owned(),
            norte_config::SchemeColumns {
                columns: Some(vec!["name".to_owned(), "attr:mem.mode".to_owned()]),
                ..norte_config::SchemeColumns::default()
            },
        )]
        .into_iter()
        .collect(),
        ..norte_config::ColumnsConfig::default()
    };
    let backend = arbol();
    let (h, snap) = UiHost::start(UiHostOptions {
        backend: Arc::clone(&backend) as Arc<dyn norte_ui_host::HostBackend>,
        initial_dir: dir(),
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
        columns: norte_frontend::columns::ColumnsSettings::resolve(&cfg),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca");
    drop(h);

    let ids: Vec<&str> = listado(&snap)
        .columns
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["name", "attr:mem.mode"],
        "manda la configuración del esquema `mem`, no la de por defecto"
    );
    assert!(
        backend
            .attrs_pedidos
            .lock()
            .expect("attrs")
            .iter()
            .any(|a| a.iter().any(|id| id == "mem.mode")),
        "y su atributo se PIDE en el listado"
    );
}

// ---------------------------------------------------------------------------
// Which-key: qué continúa un prefijo a medias (fase 4, tarea 4.4).
// ---------------------------------------------------------------------------

/// Un prefijo a medias enseña QUÉ puede seguir, con la etiqueta de cada
/// tecla en el idioma del usuario y diciendo cuáles no se pueden hacer aquí.
///
/// Lo construye `norte_frontend::whichkey`, el mismo modelo que pinta el TUI:
/// el renderer no sabe resolver un prefijo, solo pintar lo que continúa.
#[tokio::test]
async fn un_prefijo_a_medias_ensena_lo_que_sigue() {
    use norte_frontend::keymap::{Effective, Screen, parse_keymap, parse_keymap_layer};

    let preset = parse_keymap(
        norte_frontend::keymap::presets::source("orthodox").expect("preset de fábrica"),
    )
    .expect("preset parsea");
    // Una secuencia de dos teclas, que es lo que which-key existe para
    // enseñar. Ningún preset de fábrica las usa en `pane`.
    let capa = parse_keymap_layer(
        r#"
[pane]
prepend_keymap = [
    { on = ["ctrl+x", "g"], run = "cursor.top" },
    { on = ["ctrl+x", "b"], run = "cursor.bottom" },
]
"#,
    )
    .expect("capa parsea");
    let keymap = Effective::build_for(
        &preset,
        &[capa],
        &norte_ui_host::commands::todos(),
        Screen::Browse,
    )
    .expect("efectivo");

    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        locale: "es".to_owned(),
        keymap,
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

    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "x".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");

    let panel = siguiente_whichkey(&mut sub).await.expect("hay panel");
    assert!(
        !panel.title.is_empty(),
        "el panel dice qué prefijo describe"
    );
    let teclas: Vec<&str> = panel.rows.iter().map(|r| r.chord.as_str()).collect();
    assert!(
        teclas.contains(&"g") && teclas.contains(&"b"),
        "enseña las dos continuaciones: {teclas:?}"
    );
    for fila in &panel.rows {
        assert!(!fila.label.is_empty(), "cada tecla dice qué hace: {fila:?}");
    }

    // Y al completar la secuencia, el panel se va: describía teclas que ya no
    // están vivas.
    h.dispatch(tecla("g")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.whichkey.is_none(), "la secuencia se cerró");
}

/// Espera la siguiente actualización que traiga el panel de continuaciones.
async fn siguiente_whichkey(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::WhichKeyView> {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv())
            .await
            .expect("una actualización antes del plazo")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::WhichKey { whichkey } = c {
                    return whichkey.clone();
                }
            }
        }
    }
    panic!("no llegó ninguna actualización con panel");
}

// ---------------------------------------------------------------------------
// La paleta de comandos (fase 4, tarea 4.4).
// ---------------------------------------------------------------------------

/// Espera la siguiente actualización que traiga la paleta.
async fn siguiente_paleta(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::PaletteView> {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv())
            .await
            .expect("una actualización antes del plazo")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Palette { palette } = c {
                    return palette.clone();
                }
            }
        }
    }
    panic!("no llegó ninguna actualización con paleta");
}

fn tecla_de(k: &str) -> UiAction {
    tecla(k)
}

/// `ctrl+p` abre la paleta con TODO lo que el host implementa, cada fila con
/// su descripción y su atajo real.
#[tokio::test]
async fn la_paleta_ofrece_lo_que_el_host_implementa() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");

    let p = siguiente_paleta(&mut sub).await.expect("la paleta abre");
    assert!(p.query.is_empty(), "arranca sin filtro");
    assert_eq!(
        usize::try_from(p.total).unwrap_or(usize::MAX),
        norte_ui_host::commands::todos().len(),
        "ofrece todo lo implementado, ni más ni menos"
    );
    assert_eq!(p.rows.len() as u64, p.total, "sin filtro se ven todas");
    let entrar = p
        .rows
        .iter()
        .find(|r| r.text == "nav.enter")
        .expect("nav.enter está");
    assert!(!entrar.desc.is_empty(), "cada fila dice qué hace");
    assert_ne!(
        entrar.chord, "—",
        "y el atajo sale del preset, no de una lista a mano"
    );
}

/// Teclear ACOTA, y lo que se corre es lo seleccionado — por el mismo camino
/// que una tecla.
#[tokio::test]
async fn teclear_en_la_paleta_acota_y_enter_ejecuta() {
    let (h, snap) = host_arbol(arbol()).await;
    let cursor_antes = listado(&snap).cursor;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");
    let _ = siguiente_paleta(&mut sub).await;

    for c in ["c", "u", "r", "s", "o", "r"] {
        h.dispatch(tecla_de(c)).await.expect("host vivo");
    }
    // Una foto, no el siguiente parche: hay seis en la cola y el primero
    // describe la paleta tras la PRIMERA letra.
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let filtrada = siguiente_foto(&mut sub)
        .await
        .palette
        .expect("sigue abierta");
    assert!(
        !filtrada.rows.is_empty()
            && filtrada.rows.len() < usize::try_from(filtrada.total).unwrap_or(usize::MAX),
        "teclear acota: {} de {}",
        filtrada.rows.len(),
        filtrada.total
    );
    assert!(
        filtrada.rows.iter().all(|r| {
            // El filtro casa sobre lo PINTADO —nombre y descripción—, que es
            // lo que el modelo compartido pliega: una fila cuya descripción
            // habla del cursor casa igual, y eso es lo correcto.
            let heno = format!("{} {}", r.text, r.desc).to_lowercase();
            heno.contains("cursor")
        }),
        "y lo que queda casa con lo tecleado: {:?}",
        filtrada.rows
    );

    // Bajar y ejecutar: la paleta se cierra y el comando corre.
    h.dispatch(tecla_de("ArrowDown")).await.expect("host vivo");
    h.dispatch(tecla_de("Enter")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.palette.is_none(), "la paleta se cierra al ejecutar");
    assert_ne!(
        listado(&foto).cursor,
        cursor_antes,
        "y el comando de cursor se ejecutó"
    );
}

/// `esc` la cierra sin ejecutar nada.
#[tokio::test]
async fn escape_cierra_la_paleta_sin_ejecutar() {
    let (h, snap) = host_arbol(arbol()).await;
    let antes = listado(&snap).cursor;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");
    let _ = siguiente_paleta(&mut sub).await;
    h.dispatch(tecla_de("Escape")).await.expect("host vivo");
    assert!(
        siguiente_paleta(&mut sub).await.is_none(),
        "la paleta se cierra"
    );
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(listado(&foto).cursor, antes, "y no ejecutó nada");
}

/// Ejecutar desde la paleta CIERRA la paleta en el flujo de parches, no solo
/// en la siguiente foto.
///
/// Un renderer que aplica parches —que es lo que hace el de referencia, y
/// para lo que existe la secuencia— no puede enterarse de que la paleta se
/// cerró solo si pide un `Resync`. Antes de este test, `enter` mandaba el
/// parche del COMANDO y ninguno de la paleta: la lista se quedaba pintada
/// encima del listado hasta que algo, por otro motivo, provocaba una foto.
#[tokio::test]
async fn ejecutar_en_la_paleta_manda_su_cierre_en_un_parche() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");
    let _ = siguiente_paleta(&mut sub).await.expect("la paleta abre");

    h.dispatch(tecla_de("Enter")).await.expect("host vivo");
    assert!(
        siguiente_paleta(&mut sub).await.is_none(),
        "el cierre viaja como parche, sin esperar a una foto"
    );
}

// ---------------------------------------------------------------------------
// La ayuda (fase 4, tarea 4.4).
// ---------------------------------------------------------------------------

/// Espera la siguiente actualización que traiga la ayuda.
async fn siguiente_ayuda(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::HelpView> {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv())
            .await
            .expect("una actualización antes del plazo")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Help { help } = c {
                    return help.clone();
                }
            }
        }
    }
    panic!("no llegó ninguna actualización con ayuda");
}

/// Abre la ayuda y devuelve lo que se pintaría.
async fn abrir_ayuda(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::dto::HelpView {
    h.dispatch(tecla("F1")).await.expect("host vivo");
    siguiente_ayuda(sub).await.expect("la ayuda abre")
}

/// `F1` abre la ayuda sobre la página del CONTEXTO donde está el lector, con
/// su prosa ya en bloques y sin una sola marca sin resolver.
#[tokio::test]
async fn f1_abre_la_ayuda_del_contexto_y_su_prosa_llega_en_bloques() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let ayuda = abrir_ayuda(&h, &mut sub).await;

    assert_eq!(
        ayuda.topic_id, "panes",
        "abre la página del CONTEXTO (el listado), no el índice"
    );
    assert!(!ayuda.title.is_empty(), "la página tiene título");
    assert!(!ayuda.blocks.is_empty(), "y cuerpo");
    assert!(!ayuda.sidebar.is_empty(), "y la lateral enumera lo que hay");
    assert!(
        !ayuda.can_back,
        "la página del contexto es la RAÍZ: `⌫` cierra, no vuelve a un índice \
         donde el lector no estuvo"
    );
    // Ni una marca viva sin resolver, ni una clave Fluent cruda: las dos
    // cosas son texto que el lector no debería ver jamás.
    let texto = format!("{:?}", ayuda.blocks);
    assert!(!texto.contains("{{cmd:"), "una marca sin resolver: {texto}");
    assert!(!texto.contains("[["), "un enlace sin resolver: {texto}");
    assert!(!texto.contains("help-cmd-"), "una clave Fluent cruda");
    // Una cabecera de grupo llega TRADUCIDA, no como su tag.
    let grupos: Vec<&norte_ui_host::dto::HelpSidebarRowView> = ayuda
        .sidebar
        .iter()
        .filter(|r| matches!(r, norte_ui_host::dto::HelpSidebarRowView::Group { .. }))
        .collect();
    assert!(!grupos.is_empty(), "hay cabeceras de grupo");
    for g in grupos {
        let norte_ui_host::dto::HelpSidebarRowView::Group { label } = g else {
            unreachable!("filtrado arriba")
        };
        assert!(!label.starts_with("help-group-"), "sin traducir: {label}");
    }
}

/// La hoja de teclado se GENERA del mapa efectivo: un rebind la cambia, y una
/// tecla que este frontend no ejecuta sale apagada y con su motivo.
#[tokio::test]
async fn la_hoja_de_teclado_sale_del_keymap_efectivo() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let ayuda = abrir_ayuda(&h, &mut sub).await;

    // La página de teclado es la última de la lateral (grupo de una sola
    // fila): se llega con el cursor, como llegaría el lector.
    let ultima = ayuda.sidebar.len() - 1;
    h.dispatch(UiAction::HelpSelectTopic {
        row: u32::try_from(ultima).expect("cabe"),
    })
    .await
    .expect("host vivo");
    let teclas = siguiente_ayuda(&mut sub).await.expect("sigue abierta");
    assert_eq!(teclas.topic_id, "keys", "se abrió la página de teclado");

    let filas: Vec<&norte_ui_host::dto::HelpKeyRowView> = teclas
        .blocks
        .iter()
        .filter_map(|b| match b {
            norte_ui_host::dto::HelpBlockView::Keys { rows } => Some(rows),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(!filas.is_empty(), "la hoja tiene filas");
    assert!(
        filas.iter().any(|r| r.chord == "F5"),
        "y las escribe como las escribe la documentación: {:?}",
        filas.iter().map(|r| &r.chord).collect::<Vec<_>>()
    );
    assert!(
        filas.iter().all(|r| !r.label.starts_with("help-cmd-")),
        "ninguna fila pinta una clave Fluent"
    );
    let apagadas: Vec<&&norte_ui_host::dto::HelpKeyRowView> =
        filas.iter().filter(|r| !r.enabled).collect();
    assert!(
        !apagadas.is_empty(),
        "el preset ata comandos que esta ventana no hace"
    );
    assert!(
        apagadas.iter().all(|r| !r.reason.is_empty()),
        "y cada una dice POR QUÉ: atenuar sin decirlo deja al lector \
         adivinando si la app está rota"
    );
}

/// Una fila ejecutable de un comando que este frontend NO implementa se
/// ofrece apagada y con su motivo, en vez de prometer un `enter` que
/// contestaría «aquí no».
#[tokio::test]
async fn una_fila_que_esta_ventana_no_ejecuta_llega_apagada() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let mut ayuda = abrir_ayuda(&h, &mut sub).await;

    // Se recorre la lateral hasta dar con una página que documente comandos.
    for row in 0..ayuda.sidebar.len() {
        if ayuda.actions.iter().any(|a| !a.opens_topic) {
            break;
        }
        h.dispatch(UiAction::HelpSelectTopic {
            row: u32::try_from(row).expect("cabe"),
        })
        .await
        .expect("host vivo");
        ayuda = siguiente_ayuda(&mut sub).await.expect("sigue abierta");
    }
    let corribles: Vec<&norte_ui_host::dto::HelpActionView> =
        ayuda.actions.iter().filter(|a| !a.opens_topic).collect();
    assert!(
        !corribles.is_empty(),
        "alguna página del corpus documenta comandos"
    );
    for a in corribles {
        assert!(!a.label.is_empty(), "toda fila se llama de algo");
        assert_eq!(
            a.enabled,
            a.reason.is_empty(),
            "una fila apagada dice por qué, y una viva no inventa motivo: {a:?}"
        );
    }
}

/// Activar una fila ejecutable cierra la ayuda Y corre el comando — por el
/// MISMO camino que una tecla, que es lo que hace que la ayuda sea otra
/// puerta al catálogo y no un segundo despachador.
#[tokio::test]
async fn activar_en_la_ayuda_cierra_y_ejecuta_por_el_mismo_camino() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let ayuda = abrir_ayuda(&h, &mut sub).await;

    // La página de marcado documenta `mark.toggle`, que esta ventana SÍ
    // ejecuta: se llega a ella por la lateral, como llegaría el lector.
    let mut pagina = ayuda;
    for row in 0..40 {
        if pagina.topic_id == "selection" {
            break;
        }
        h.dispatch(UiAction::HelpSelectTopic { row })
            .await
            .expect("host vivo");
        pagina = siguiente_ayuda(&mut sub).await.expect("sigue abierta");
    }
    assert_eq!(pagina.topic_id, "selection", "la página de marcado existe");
    let i = pagina
        .actions
        .iter()
        .position(|a| !a.opens_topic && a.enabled)
        .expect("alguna de sus filas la ejecuta esta ventana");

    h.dispatch(UiAction::HelpActivate {
        index: u32::try_from(i).expect("cabe"),
    })
    .await
    .expect("host vivo");
    assert!(
        siguiente_ayuda(&mut sub).await.is_none(),
        "correr cierra la ayuda, y el cierre viaja como parche"
    );

    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.help.is_none(), "y sigue cerrada en la foto");
    assert!(
        listado(&foto).rows.iter().any(|r| r.marked),
        "y el comando de marcado se ejecutó de verdad"
    );
}

/// Seguir un enlace de «ver también» abre la otra página y DEJA la ayuda
/// abierta: es navegación, no una acción sobre el listado.
#[tokio::test]
async fn seguir_un_enlace_abre_la_otra_pagina_y_deja_volver() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let mut pagina = abrir_ayuda(&h, &mut sub).await;

    for row in 0..40 {
        if pagina.actions.iter().any(|a| a.opens_topic) {
            break;
        }
        h.dispatch(UiAction::HelpSelectTopic { row })
            .await
            .expect("host vivo");
        pagina = siguiente_ayuda(&mut sub).await.expect("sigue abierta");
    }
    let i = pagina
        .actions
        .iter()
        .position(|a| a.opens_topic)
        .expect("alguna página enlaza a otra");
    let antes = pagina.topic_id.clone();

    h.dispatch(UiAction::HelpActivate {
        index: u32::try_from(i).expect("cabe"),
    })
    .await
    .expect("host vivo");
    let seguida = siguiente_ayuda(&mut sub).await.expect("sigue abierta");
    assert_ne!(seguida.topic_id, antes, "cambió de página");
    assert!(seguida.can_back, "y hay a dónde volver");

    h.dispatch(tecla("Backspace")).await.expect("host vivo");
    let vuelta = siguiente_ayuda(&mut sub).await.expect("sigue abierta");
    assert_eq!(vuelta.topic_id, antes, "`⌫` vuelve por donde vino");
}

/// `esc` la cierra; `/` abre el filtro y entonces las teclas de texto son
/// suyas.
#[tokio::test]
async fn la_barra_filtra_y_escape_cierra() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let ayuda = abrir_ayuda(&h, &mut sub).await;
    assert!(!ayuda.filtering, "arranca sin filtro");

    h.dispatch(tecla("/")).await.expect("host vivo");
    let filtrando = siguiente_ayuda(&mut sub).await.expect("sigue abierta");
    assert!(filtrando.filtering, "`/` abre el filtro");

    h.dispatch(tecla("c")).await.expect("host vivo");
    let tecleada = siguiente_ayuda(&mut sub).await.expect("sigue abierta");
    assert_eq!(tecleada.filter, "c", "y la letra la escribe el filtro");

    // El primer `esc` deja de filtrar; el segundo cierra.
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    let sin_filtro = siguiente_ayuda(&mut sub).await.expect("sigue abierta");
    assert!(!sin_filtro.filtering, "el primer esc abandona el filtro");
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    assert!(
        siguiente_ayuda(&mut sub).await.is_none(),
        "el segundo cierra la ayuda"
    );
}

/// Con la ayuda abierta, una tecla del listado NO se cuela: la pantalla es
/// suya, como la del visor.
#[tokio::test]
async fn con_la_ayuda_abierta_el_listado_no_se_mueve() {
    let (h, snap) = host_arbol(arbol()).await;
    let antes = listado(&snap).cursor;
    let mut sub = h.subscribe();
    let _ = abrir_ayuda(&h, &mut sub).await;

    // `j` en el preset baja el cursor; con la ayuda abierta no es del
    // listado, y sin filtro abierto tampoco teclea nada.
    h.dispatch(tecla("j")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(listado(&foto).cursor, antes, "el listado no se movió");
    assert!(foto.help.is_some(), "y la ayuda sigue abierta en la foto");
}

/// `Ctrl+P` sale de la ayuda a la paleta, y los DOS cambios viajan en el
/// mismo parche: un renderer que solo recibiera el de la paleta seguiría
/// pintando la ayuda debajo.
#[tokio::test]
async fn ctrl_p_cambia_la_ayuda_por_la_paleta_en_un_solo_parche() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let _ = abrir_ayuda(&h, &mut sub).await;

    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");

    let mut vio_cierre = false;
    let mut vio_paleta = false;
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv())
            .await
            .expect("una actualización antes del plazo")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                match c {
                    norte_ui_host::dto::ViewChange::Help { help } => vio_cierre = help.is_none(),
                    norte_ui_host::dto::ViewChange::Palette { palette } => {
                        vio_paleta = palette.is_some();
                    }
                    _ => {}
                }
            }
            if vio_cierre && vio_paleta {
                return;
            }
        }
    }
    panic!("el relevo no viajó entero: cierre={vio_cierre}, paleta={vio_paleta}");
}

// ---------------------------------------------------------------------------
// Las páginas de extensión de la ayuda (H3e sobre el host gráfico).
// ---------------------------------------------------------------------------

/// Un plugin del catálogo, con lo mínimo que la ayuda mira.
fn extension(id: &str, name: &str, has_help: bool) -> norte_proto::methods::PluginInfo {
    norte_proto::methods::PluginInfo {
        id: id.to_owned(),
        name: name.to_owned(),
        publisher: "ACME".to_owned(),
        version: "1.0.0".to_owned(),
        category: "previewer".to_owned(),
        capabilities: Vec::new(),
        approved: true,
        enabled: true,
        description: None,
        commands: Vec::new(),
        columns: Vec::new(),
        has_help,
        // El ancla que el core manda (#282): la ventana la devuelve al
        // confirmar, y sin ella en el doble el hilo entero no se ejercitaría.
        manifest_digest: Some(format!("digest-de-{id}")),
    }
}

/// Un árbol con catálogo de extensiones.
fn arbol_con_plugins(
    plugins: Vec<norte_proto::methods::PluginInfo>,
    paginas: &[(&str, &str)],
) -> Arc<Falso> {
    let base = arbol();
    let mut f = Falso {
        plugins: plugins.into(),
        paginas: paginas
            .iter()
            .map(|(id, md)| ((*id).to_owned(), (*md).to_owned()))
            .collect(),
        ..Falso::default()
    };
    f.arbol.clone_from(&base.arbol);
    Arc::new(f)
}

/// Espera a que la lateral tenga una fila cuyo título contenga `aguja`.
async fn ayuda_con_fila(
    sub: &mut norte_ui_host::UiSubscription,
    aguja: &str,
) -> norte_ui_host::dto::HelpView {
    for _ in 0..20 {
        let Some(v) = siguiente_ayuda(sub).await else {
            continue;
        };
        if v.sidebar.iter().any(|r| match r {
            norte_ui_host::dto::HelpSidebarRowView::Topic { title, .. } => title.contains(aguja),
            norte_ui_host::dto::HelpSidebarRowView::Group { .. } => false,
        }) {
            return v;
        }
    }
    panic!("la lateral nunca trajo una fila con {aguja:?}");
}

/// Una extensión con página aparece en la lateral, y abrirla PIDE su página y
/// la instala con su línea de procedencia.
#[tokio::test]
async fn una_extension_con_pagina_se_lee_desde_la_ayuda() {
    let backend = arbol_con_plugins(
        vec![extension("acme.ftp", "FTP de ACME", true)],
        &[("acme.ftp", "Conecta con un servidor FTP.")],
    );
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F1")).await.expect("host vivo");
    let ayuda = ayuda_con_fila(&mut sub, "FTP de ACME").await;

    let fila = ayuda
        .sidebar
        .iter()
        .position(|r| {
            matches!(r, norte_ui_host::dto::HelpSidebarRowView::Topic { title, .. }
                if title.contains("FTP de ACME"))
        })
        .expect("la fila está");
    h.dispatch(UiAction::HelpSelectTopic {
        row: u32::try_from(fila).expect("cabe"),
    })
    .await
    .expect("host vivo");

    // La página llega ASÍNCRONA: primero la página vacía con su nombre, y
    // luego el cuerpo cuando el daemon contesta.
    let mut pagina = None;
    for _ in 0..20 {
        let Some(v) = siguiente_ayuda(&mut sub).await else {
            continue;
        };
        if v.topic_id == "acme.ftp" && !v.blocks.is_empty() {
            pagina = Some(v);
            break;
        }
    }
    let pagina = pagina.expect("la página del plugin se instala");
    let texto = format!("{:?}", pagina.blocks);
    assert!(texto.contains("Conecta con un servidor FTP"), "{texto}");
    // Y lleva su procedencia: una página de tercero SIEMPRE la lleva, o
    // tendría la misma forma que una del binario.
    let badge = pagina.badge.expect("una página de plugin lleva insignia");
    assert!(badge.contains("ACME"), "dice quién la publica: {badge}");

    assert_eq!(
        backend
            .paginas_pedidas
            .lock()
            .expect("mutex")
            .as_slice()
            .iter()
            .filter(|i| i.as_str() == "acme.ftp")
            .count(),
        1,
        "la página se pide UNA vez por apertura"
    );
}

/// Un id que no es reverse-DNS válido se DESCARTA en la entrada: ni fila, ni
/// petición al wire. Enmascararlo no valdría — no es inyectivo, así que dos
/// plugins distintos caerían en la misma fila.
#[tokio::test]
async fn un_id_de_extension_invalido_ni_se_pinta_ni_llega_al_wire() {
    let backend = arbol_con_plugins(
        vec![
            extension("acme.\u{202e}ftp", "Malicioso", true),
            extension("sinpunto", "Tampoco", true),
        ],
        &[],
    );
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let ayuda = abrir_ayuda(&h, &mut sub).await;

    // Se le dan varias vueltas al bucle: si llegara una fila, llegaría aquí.
    for _ in 0..4 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        let lateral = foto.help.map_or_else(Vec::new, |v| v.sidebar);
        for r in &lateral {
            if let norte_ui_host::dto::HelpSidebarRowView::Topic { title, .. } = r {
                assert!(!title.contains("Malicioso"), "entró un id inválido");
                assert!(!title.contains("Tampoco"), "entró un id sin punto");
            }
        }
    }
    assert!(
        backend.paginas_pedidas.lock().expect("mutex").is_empty(),
        "un id inválido jamás se manda al wire"
    );
    let _ = ayuda;
}

/// Un `help.md` hostil se PARSEA antes de pintarse: lo que cruza son bloques,
/// y ni un peligro de terminal viaja dentro de ellos.
#[tokio::test]
async fn una_pagina_hostil_cruza_ya_parseada_y_enmascarada() {
    let backend = arbol_con_plugins(
        vec![extension("acme.ftp", "FTP de ACME", true)],
        &[(
            "acme.ftp",
            "Texto \u{202e}con override\u{7} y un pitido.\n\n{{cmd:pane.copy}}\n",
        )],
    );
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F1")).await.expect("host vivo");
    let ayuda = ayuda_con_fila(&mut sub, "FTP de ACME").await;
    let fila = ayuda
        .sidebar
        .iter()
        .position(|r| {
            matches!(r, norte_ui_host::dto::HelpSidebarRowView::Topic { title, .. }
                if title.contains("FTP de ACME"))
        })
        .expect("la fila está");
    h.dispatch(UiAction::HelpSelectTopic {
        row: u32::try_from(fila).expect("cabe"),
    })
    .await
    .expect("host vivo");

    let mut pagina = None;
    for _ in 0..20 {
        let Some(v) = siguiente_ayuda(&mut sub).await else {
            continue;
        };
        if v.topic_id == "acme.ftp" && !v.blocks.is_empty() {
            pagina = Some(v);
            break;
        }
    }
    let pagina = pagina.expect("la página se instala");
    let texto = format!("{:?}", pagina.blocks);
    assert!(
        !texto.contains('\u{202e}'),
        "un override bidi cruzó: {texto}"
    );
    assert!(!texto.contains('\u{7}'), "un control cruzó: {texto}");
    // Y la marca de un comando de OTRO —el binario— no se resuelve en una
    // página de plugin: un tercero no toma prestado el aviso del host.
    assert!(
        !texto.contains("F5"),
        "una página de plugin no resuelve marcas ajenas: {texto}"
    );
}

// ---------------------------------------------------------------------------
// Lo que encontraron las revisiones de la 4.4.
// ---------------------------------------------------------------------------

/// `F1` con el VISOR abierto abre la ayuda Y se queda las teclas.
///
/// Antes no: el visor iba primero en el enrutado, así que la ayuda se
/// construía, viajaba, y ninguna tecla llegaba a ella — ni la que la cierra.
/// Encima, en el DOM la ayuda estaba ANTES del visor, cuyo fondo es opaco, o
/// sea que ni se veía. Una ventana con un overlay abierto que no responde a
/// nada es lo más parecido a estar colgada.
#[tokio::test]
async fn con_el_visor_abierto_la_ayuda_se_queda_las_teclas() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
    f.contenido
        .insert("mem:///casa/notas.txt".to_owned(), b"hola".to_vec());
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if siguiente_foto(&mut sub).await.viewer.is_some() {
            break;
        }
    }

    h.dispatch(tecla("F1")).await.expect("host vivo");
    let ayuda = siguiente_ayuda(&mut sub).await.expect("la ayuda abre");
    assert_eq!(
        ayuda.topic_id, "viewer",
        "y sobre la página del visor, que es donde está el lector"
    );

    // `esc` es de la AYUDA, no del visor: la ayuda está encima.
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    assert!(
        siguiente_ayuda(&mut sub).await.is_none(),
        "esc cierra la ayuda"
    );
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.help.is_none(), "la ayuda se fue");
    assert!(foto.viewer.is_some(), "y el visor sigue donde estaba");
}

/// Sobre un diálogo que se está TECLEANDO, `F1` no abre nada.
///
/// La ayuda se queda el teclado, así que abrirla encima de un campo de texto
/// convierte el `⌫` que corrige una errata en un paso atrás de la ayuda.
#[tokio::test]
async fn la_ayuda_no_se_abre_encima_de_un_campo_de_texto() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    // F7 abre el prompt de crear directorio.
    h.dispatch(tecla("F7")).await.expect("host vivo");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if siguiente_foto(&mut sub)
            .await
            .dialogs
            .iter()
            .any(|d| d.input.is_some())
        {
            break;
        }
    }

    h.dispatch(tecla("F1")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.help.is_none(), "la ayuda no se abrió encima del campo");
    assert!(
        foto.dialogs.iter().any(|d| d.input.is_some()),
        "y el diálogo sigue esperando el nombre"
    );
}

/// Una fila de OTRA pantalla no se ofrece encendida, y su motivo lo dice.
///
/// La lista de comandos del host es plana —listado y visor juntos—, así que
/// preguntarle a secas encendía `viewer.close` con el visor cerrado, para
/// luego negarse al pulsarla. Y un verbo `dialog.*` no lo hace esta ventana
/// ni tiene por qué: lo contesta el propio diálogo con sus botones.
#[tokio::test]
async fn una_fila_de_otra_pantalla_no_se_ofrece_encendida() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let mut pagina = abrir_ayuda(&h, &mut sub).await;

    for row in 0..40 {
        if pagina.topic_id == "viewer" {
            break;
        }
        h.dispatch(UiAction::HelpSelectTopic { row })
            .await
            .expect("host vivo");
        pagina = siguiente_ayuda(&mut sub).await.expect("sigue abierta");
    }
    assert_eq!(pagina.topic_id, "viewer", "la página del visor existe");
    // `pane.open`, `pane.edit` y `pane.edit-new` salen en esta página y NO son
    // del visor: los tres actúan sobre el LISTADO con la aplicación del
    // escritorio, así que estar vivos aquí es lo correcto (#290 hizo que
    // editar fuera lo segundo, y que crear-y-editar fuera lo tercero). Las
    // demás filas sí necesitan el visor.
    let del_listado = [
        norte_frontend::keymap::paint_chord("alt+f4"),
        norte_frontend::keymap::paint_chord("f4"),
        norte_frontend::keymap::paint_chord("shift+f4"),
    ];
    let corribles: Vec<&norte_ui_host::dto::HelpActionView> = pagina
        .actions
        .iter()
        .filter(|a| !a.opens_topic && !del_listado.contains(&a.chord))
        .collect();
    assert!(!corribles.is_empty(), "documenta comandos");
    for a in corribles {
        assert!(
            !a.enabled,
            "sin visor abierto, ninguna fila suya se puede correr: {a:?}"
        );
        assert!(!a.reason.is_empty(), "y cada una dice por qué: {a:?}");
    }
}

/// `enter` sobre una fila apagada NO la corre, y la página sigue abierta.
///
/// El renderer no le pone escuchador a una fila apagada, pero el teclado no
/// pasa por el renderer: la comprobación tiene que estar en el host o hay una
/// puerta sin cerrojo.
#[tokio::test]
async fn enter_sobre_una_fila_apagada_no_la_corre() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let mut pagina = abrir_ayuda(&h, &mut sub).await;
    for row in 0..40 {
        if pagina.actions.iter().any(|a| !a.opens_topic && !a.enabled) {
            break;
        }
        h.dispatch(UiAction::HelpSelectTopic { row })
            .await
            .expect("host vivo");
        pagina = siguiente_ayuda(&mut sub).await.expect("sigue abierta");
    }
    let i = pagina
        .actions
        .iter()
        .position(|a| !a.opens_topic && !a.enabled)
        .expect("alguna página documenta un comando que esta ventana no hace");

    let ack = h
        .dispatch(UiAction::HelpActivate {
            index: u32::try_from(i).expect("cabe"),
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, norte_ui_host::ActionAck::Unavailable { .. }),
        "se dice que no se puede: {ack:?}"
    );
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(
        foto.help.is_some(),
        "y la ayuda SIGUE abierta: la explicación está en la página"
    );
}

/// Todo contexto que este host declara tiene una página que lo reclama, en
/// los DOS idiomas.
///
/// Sin esto, renombrar una portada del corpus deja a `F1` abriendo el índice
/// en silencio y ningún test se pone rojo.
#[test]
fn todo_contexto_declarado_tiene_pagina() {
    for lang in [norte_help::Lang::En, norte_help::Lang::Es] {
        for c in norte_ui_host::controller::CONTEXTOS {
            assert!(
                norte_help::topic_for_context(lang, c).is_some(),
                "ninguna página reclama {c} en {lang:?}"
            );
        }
    }
}

/// NINGUNA cadena de la ayuda proyectada lleva un peligro de terminal, en
/// ninguna página del corpus y en los dos idiomas.
///
/// Es la invariante sobre la que descansa todo el diseño —el renderer pinta
/// lo que llega y no lo interpreta—, y no la afirmaba nada. Se barre la
/// proyección ENTERA (título, insignia, lateral, bloques, filas y motivos):
/// una cadena nueva que se olvide de sanear se cae aquí sin que nadie tenga
/// que acordarse de añadirle su aserción.
#[tokio::test]
async fn ninguna_cadena_de_la_ayuda_lleva_un_peligro_de_terminal() {
    /// Todo lo pintable de una proyección, en una sola cadena.
    fn todo(v: &norte_ui_host::dto::HelpView) -> String {
        use std::fmt::Write as _;

        let mut s = format!("{} {}", v.title, v.filter);
        if let Some(b) = &v.badge {
            s.push_str(b);
        }
        for r in &v.sidebar {
            match r {
                norte_ui_host::dto::HelpSidebarRowView::Group { label } => s.push_str(label),
                norte_ui_host::dto::HelpSidebarRowView::Topic { title, .. } => s.push_str(title),
            }
        }
        // Los bloques y las filas se barren por su `Debug`, que incluye
        // TODOS sus campos: es justamente lo que hace que una cadena nueva
        // entre en el barrido sin tocar este test.
        let _ = write!(s, "{:?}{:?}", v.blocks, v.actions);
        s
    }

    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let ayuda = abrir_ayuda(&h, &mut sub).await;
    let filas = ayuda.sidebar.len();
    let mut vistas = vec![ayuda];
    for row in 0..filas {
        h.dispatch(UiAction::HelpSelectTopic {
            row: u32::try_from(row).expect("cabe"),
        })
        .await
        .expect("host vivo");
        if let Some(v) = siguiente_ayuda(&mut sub).await {
            vistas.push(v);
        }
    }
    assert!(vistas.len() > 3, "se recorrieron varias páginas");
    for v in &vistas {
        let texto = todo(v);
        // El `Debug` de un `&str` escapa los controles como `\u{...}`, así
        // que se busca sobre el texto DESESCAPADO de los campos planos y,
        // para los anidados, sobre la forma escapada — que delata igual.
        assert!(
            !texto.chars().any(norte_encoding::is_terminal_hazard),
            "peligro de terminal en la página {}: {texto:?}",
            v.topic_id
        );
        assert!(
            !texto.contains("\\u{202e}") && !texto.contains("\\u{7}"),
            "peligro escapado en la página {}",
            v.topic_id
        );
    }
}

// ---------------------------------------------------------------------------
// Los ajustes en solo lectura (tarea 4.5).
// ---------------------------------------------------------------------------

/// Espera la siguiente actualización que traiga los ajustes.
async fn siguiente_ajustes(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::SettingsView> {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv())
            .await
            .expect("una actualización antes del plazo")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Settings { settings } = c {
                    return settings.clone();
                }
            }
        }
    }
    panic!("no llegó ninguna actualización con ajustes");
}

/// Un host con unas rutas dichas, para la sección de diagnóstico.
async fn host_con_rutas(paths: norte_ui_host::settings::HostPaths) -> UiHost {
    UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: ajustes_de_prueba(),
        paths,
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca")
    .0
}

/// `F11` abre los ajustes con el registro COMPARTIDO y su valor efectivo, y
/// dice que esta ventana todavía no los escribe.
#[tokio::test]
async fn los_ajustes_ensenan_el_registro_compartido_con_su_valor() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F11")).await.expect("host vivo");
    let a = siguiente_ajustes(&mut sub).await.expect("abren");

    assert!(
        a.read_only,
        "y lo DICE, en vez de ofrecer un enter que no va"
    );
    let general = a
        .sections
        .iter()
        .find_map(|s| match s {
            norte_ui_host::dto::SettingsSectionView::Settings { rows, .. } => Some(rows),
            norte_ui_host::dto::SettingsSectionView::Paths { .. } => None,
        })
        .expect("hay sección general");
    assert_eq!(
        general.len(),
        norte_frontend::settings::catalog().len(),
        "ni una entrada del catálogo compartido se queda fuera"
    );
    for r in general {
        assert!(!r.id.is_empty(), "cada fila lleva su id estable");
        assert!(!r.name.is_empty(), "y su nombre traducido: {r:?}");
        assert!(
            !r.name.starts_with("setting-"),
            "ninguna pinta una clave Fluent: {r:?}"
        );
        assert!(
            r.restart_required,
            "esta ventana resuelve tema y keymap al arrancar: TODO pide \
             reiniciar, y decir lo contrario manda a buscar un bug que no hay"
        );
    }
}

/// Una ubicación con su existencia resuelta, como la resuelve el arranque.
fn sitio(p: std::path::PathBuf) -> norte_ui_host::settings::HostPath {
    norte_ui_host::settings::HostPath {
        missing: !p.exists(),
        path: p,
    }
}

/// La sección de ubicaciones dice dónde vive cada cosa, marca lo que falta y
/// no enseña ni un valor.
#[tokio::test]
async fn las_ubicaciones_se_dicen_y_lo_que_falta_se_marca() {
    let tmp = tempfile::tempdir().expect("tmp");
    let existe = tmp.path().join("config");
    std::fs::create_dir(&existe).expect("mkdir");
    let no_existe = tmp.path().join("no-esta");
    let h = host_con_rutas(norte_ui_host::settings::HostPaths {
        // El `missing` lo trae YA resuelto quien arranca: el host no hace
        // I/O al proyectar, y el test lo dice porque es el contrato.
        config_layers: vec![
            (
                norte_ui_host::settings::ConfigLayer::User,
                sitio(existe.clone()),
            ),
            (
                norte_ui_host::settings::ConfigLayer::Project,
                sitio(no_existe),
            ),
        ],
        state_dir: None,
        logs_dir: None,
        socket: Some(sitio(tmp.path().join("daemon.sock"))),
    })
    .await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F11")).await.expect("host vivo");
    let a = siguiente_ajustes(&mut sub).await.expect("abren");

    let rutas = a
        .sections
        .iter()
        .find_map(|s| match s {
            norte_ui_host::dto::SettingsSectionView::Paths { rows, .. } => Some(rows),
            norte_ui_host::dto::SettingsSectionView::Settings { .. } => None,
        })
        .expect("hay sección de rutas");
    assert_eq!(rutas.len(), 3, "dos capas y el socket");
    assert!(!rutas[0].missing, "la capa que existe no se marca");
    assert!(
        rutas[1].missing,
        "la que no existe SÍ: no se pinta como si estuviera"
    );
    for r in rutas {
        assert!(!r.label.is_empty(), "cada una dice QUÉ es: {r:?}");
        assert!(!r.display.is_empty(), "y dónde: {r:?}");
    }
}

/// Un directorio de configuración con bytes hostiles llega ENMASCARADO y
/// marcado, por el mismo camino que un nombre del listado.
#[tokio::test]
async fn una_ruta_hostil_llega_enmascarada_y_marcada() {
    let tmp = tempfile::tempdir().expect("tmp");
    // Un nombre con un override bidi: legal como fichero, y una mentira en
    // pantalla si se pinta crudo.
    let hostil = tmp.path().join("conf\u{202e}gif");
    std::fs::create_dir(&hostil).expect("mkdir");
    let h = host_con_rutas(norte_ui_host::settings::HostPaths {
        config_layers: vec![(norte_ui_host::settings::ConfigLayer::User, sitio(hostil))],
        state_dir: None,
        logs_dir: None,
        socket: None,
    })
    .await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F11")).await.expect("host vivo");
    let a = siguiente_ajustes(&mut sub).await.expect("abren");
    let rutas = a
        .sections
        .iter()
        .find_map(|s| match s {
            norte_ui_host::dto::SettingsSectionView::Paths { rows, .. } => Some(rows),
            norte_ui_host::dto::SettingsSectionView::Settings { .. } => None,
        })
        .expect("hay sección de rutas");
    assert!(
        !rutas[0].display.contains('\u{202e}'),
        "un override bidi cruzó crudo: {:?}",
        rutas[0].display
    );
    assert!(rutas[0].hostile, "y se MARCA que difiere del nombre real");
}

/// El cursor se mueve y no se sale, y `enter` dice que aquí no se edita.
#[tokio::test]
async fn el_cursor_no_se_sale_y_enter_lo_dice() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F11")).await.expect("host vivo");
    let a = siguiente_ajustes(&mut sub).await.expect("abren");
    assert_eq!(a.cursor, 0);

    h.dispatch(tecla("ArrowUp")).await.expect("host vivo");
    let arriba = siguiente_ajustes(&mut sub).await.expect("sigue abierto");
    assert_eq!(arriba.cursor, 0, "arriba del todo no se sale por arriba");

    h.dispatch(tecla("End")).await.expect("host vivo");
    let final_ = siguiente_ajustes(&mut sub).await.expect("sigue abierto");
    let total: usize = final_
        .sections
        .iter()
        .map(|s| match s {
            norte_ui_host::dto::SettingsSectionView::Settings { rows, .. } => rows.len(),
            norte_ui_host::dto::SettingsSectionView::Paths { rows, .. } => rows.len(),
        })
        .sum();
    assert_eq!(
        usize::try_from(final_.cursor).expect("cabe"),
        total - 1,
        "y por abajo tampoco"
    );

    let ack = h.dispatch(tecla("Enter")).await.expect("host vivo");
    assert!(
        matches!(ack, norte_ui_host::ActionAck::Unavailable { .. }),
        "enter no edita, y lo dice en vez de no hacer nada: {ack:?}"
    );

    h.dispatch(tecla("Escape")).await.expect("host vivo");
    assert!(
        siguiente_ajustes(&mut sub).await.is_none(),
        "esc los cierra"
    );
}

/// Con los ajustes abiertos, una tecla del listado no se cuela.
#[tokio::test]
async fn con_los_ajustes_abiertos_el_listado_no_se_mueve() {
    let (h, snap) = host_arbol(arbol()).await;
    let antes = listado(&snap).cursor;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F11")).await.expect("host vivo");
    let _ = siguiente_ajustes(&mut sub).await.expect("abren");

    h.dispatch(tecla("j")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(listado(&foto).cursor, antes, "el listado no se movió");
    assert!(foto.settings.is_some(), "y los ajustes siguen abiertos");
}

// ---------------------------------------------------------------------------
// El gestor de extensiones (tarea 4.5).
// ---------------------------------------------------------------------------

/// Espera la siguiente actualización que traiga el gestor.
async fn siguiente_extensiones(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::ExtensionsView> {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv())
            .await
            .expect("una actualización antes del plazo")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Extensions { extensions } = c {
                    return extensions.clone();
                }
            }
        }
    }
    panic!("no llegó ninguna actualización con extensiones");
}

/// Espera a que el catálogo haya llegado (deje de estar cargando).
async fn extensiones_cargadas(
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::dto::ExtensionsView {
    for _ in 0..20 {
        let Some(v) = siguiente_extensiones(sub).await else {
            continue;
        };
        if !v.loading {
            return v;
        }
    }
    panic!("el catálogo nunca llegó");
}

/// `F12` abre el gestor: primero diciendo que carga, luego con el catálogo
/// saneado y su estado de aprobación.
#[tokio::test]
async fn el_gestor_ensena_lo_instalado_y_su_estado() {
    let mut backend = arbol_con_plugins(
        vec![extension("acme.ftp", "FTP de ACME", true), {
            let mut p = extension("org.norte.demo", "Demo", false);
            p.approved = false;
            p.enabled = false;
            p.capabilities = vec!["fs-read".to_owned()];
            p
        }],
        &[],
    );
    // Un directorio que no cargó: se enseña, porque una extensión que
    // desaparece en silencio es una que el usuario cree tener.
    std::sync::Arc::get_mut(&mut backend)
        .expect("única referencia")
        .errores_de_carga = vec![(
        "/plugins/roto".to_owned(),
        "el manifiesto no parsea".to_owned(),
    )];
    let (h, _snap) = host_arbol(std::sync::Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F12")).await.expect("host vivo");
    let primera = siguiente_extensiones(&mut sub).await.expect("abre");
    assert!(
        primera.loading,
        "se abre DICIENDO que carga: una lista vacía sin ese aviso se lee \
         como «no tienes ninguna»"
    );

    let v = extensiones_cargadas(&mut sub).await;
    assert_eq!(v.rows.len(), 2);
    assert_eq!(v.rows[0].id, "acme.ftp");
    assert!(v.rows[0].approved && v.rows[0].enabled);
    assert!(!v.rows[1].approved, "y la que no está aprobada se ve");
    assert_eq!(
        v.rows[1].capabilities,
        vec!["fs-read".to_owned()],
        "las capabilities van en la FILA: son la decisión que se aprueba"
    );
    assert_eq!(v.errors.len(), 1, "y lo que no cargó se dice");
}

/// `enter` sobre una extensión pide su esquema `[config]` y lo enseña con el
/// valor efectivo.
#[tokio::test]
async fn la_ficha_ensena_el_esquema_con_su_valor_efectivo() {
    let mut backend = arbol_con_plugins(vec![extension("acme.ftp", "FTP de ACME", false)], &[]);
    std::sync::Arc::get_mut(&mut backend)
        .expect("única referencia")
        .esquemas
        .insert(
            "acme.ftp".to_owned(),
            vec![norte_proto::methods::PluginConfigKeyWire {
                key: "timeout".to_owned(),
                kind: "int".to_owned(),
                default: "10".to_owned(),
                min: Some(1),
                max: Some(300),
                values: Vec::new(),
                description: Some("Segundos antes de rendirse".to_owned()),
                value: "30".to_owned(),
            }],
        );
    let (h, _snap) = host_arbol(std::sync::Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let _ = extensiones_cargadas(&mut sub).await;

    h.dispatch(tecla("Enter")).await.expect("host vivo");
    let mut ficha = None;
    for _ in 0..20 {
        let Some(v) = siguiente_extensiones(&mut sub).await else {
            continue;
        };
        if v.detail.is_some() {
            ficha = v.detail;
            break;
        }
    }
    let d = ficha.expect("la ficha llega");
    assert_eq!(d.id, "acme.ftp");
    assert_eq!(d.config.len(), 1);
    let k = &d.config[0];
    assert_eq!(k.key, "timeout");
    assert_eq!(k.value, "30", "el valor EFECTIVO, no el del esquema");
    assert_eq!(k.default, "10", "y el del esquema, para ver qué se cambió");
    assert!(!k.domain.is_empty(), "y qué lo acota: {k:?}");
    assert!(
        !k.domain.contains("ext-config-"),
        "sin pintar una clave Fluent: {k:?}"
    );
}

/// Moverse tira la ficha: describe otra extensión.
#[tokio::test]
async fn moverse_tira_la_ficha() {
    let backend = arbol_con_plugins(
        vec![
            extension("acme.ftp", "FTP de ACME", false),
            extension("org.norte.demo", "Demo", false),
        ],
        &[],
    );
    let (h, _snap) = host_arbol(backend).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let _ = extensiones_cargadas(&mut sub).await;
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    for _ in 0..20 {
        let Some(v) = siguiente_extensiones(&mut sub).await else {
            continue;
        };
        if v.detail.is_some() {
            break;
        }
    }
    // Con la ficha abierta, las flechas son SUYAS: recorren sus claves. Este
    // catálogo no declara ninguna, y entonces no se las queda —una ficha sin
    // nada que andar dejaría al lector sin poder moverse sin cerrarla—, así
    // que esta baja el catálogo y tira la ficha.
    h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    let v = siguiente_extensiones(&mut sub)
        .await
        .expect("sigue abierto");
    assert_eq!(v.cursor, 1);
    assert!(
        v.detail.is_none(),
        "la ficha de la anterior no puede quedarse describiendo a otra"
    );
}

/// El primer `esc` cierra la FICHA; el segundo, el gestor.
#[tokio::test]
async fn el_primer_esc_cierra_la_ficha_y_el_segundo_el_gestor() {
    let backend = arbol_con_plugins(vec![extension("acme.ftp", "FTP de ACME", false)], &[]);
    let (h, _snap) = host_arbol(backend).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let _ = extensiones_cargadas(&mut sub).await;
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    for _ in 0..20 {
        let Some(v) = siguiente_extensiones(&mut sub).await else {
            continue;
        };
        if v.detail.is_some() {
            break;
        }
    }

    h.dispatch(tecla("Escape")).await.expect("host vivo");
    let sin_ficha = siguiente_extensiones(&mut sub)
        .await
        .expect("sigue abierto");
    assert!(sin_ficha.detail.is_none(), "el primer esc cierra la ficha");
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    assert!(
        siguiente_extensiones(&mut sub).await.is_none(),
        "el segundo cierra el gestor"
    );
}

/// Un nombre, un publicador y una descripción hostiles llegan enmascarados;
/// un id inválido no llega en absoluto.
#[tokio::test]
async fn el_texto_de_una_extension_llega_enmascarado() {
    let mut malo = extension("acme.\u{202e}ftp", "Invisible", false);
    malo.description = Some("desc".to_owned());
    let mut hostil = extension("acme.ftp", "FTP\u{202e}de ACME", false);
    hostil.publisher = "ACME\u{7}".to_owned();
    hostil.description = Some("Sirve\u{202e}ficheros".to_owned());
    hostil.version = "1.0\u{7}".to_owned();
    let backend = arbol_con_plugins(vec![hostil, malo], &[]);
    let (h, _snap) = host_arbol(backend).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let v = extensiones_cargadas(&mut sub).await;

    assert_eq!(v.rows.len(), 1, "el id inválido se DESCARTA en la entrada");
    let texto = format!("{:?}", v.rows[0]);
    assert!(
        !texto.contains('\u{202e}') && !texto.contains('\u{7}'),
        "texto de tercero sin enmascarar: {texto}"
    );
    assert!(
        !texto.contains("\\u{202e}") && !texto.contains("\\u{7}"),
        "texto de tercero sin enmascarar: {texto}"
    );
}

/// El VALOR de una clave de configuración, su defecto y los valores de un
/// `enum` los escribe el PLUGIN, y llegan enmascarados y marcados.
///
/// El manifiesto solo les acota la LONGITUD —`CONFIG_STRING_MAX_CHARS`,
/// `CONFIG_ENUM_MAX_VALUES`— y no comprueba charset ninguno, así que un
/// `plugin.toml` podía meter un override bidi en un valor de `enum` y verlo
/// llegar crudo a un nodo de texto del DOM. Tres rustdocs decían que esos
/// campos eran «vocabulario de norte, nunca texto libre del plugin».
///
/// Y el `·` que une el dominio se compone AQUÍ: si el valor no se enmascarara,
/// un plugin podría fabricar uno y fingir un dominio que no tiene.
#[tokio::test]
async fn el_valor_de_una_clave_de_plugin_llega_enmascarado_y_marcado() {
    let ext = extension("acme.ftp", "FTP", true);
    let mut f = Falso {
        plugins: vec![ext].into(),
        ..Falso::default()
    };
    f.arbol.clone_from(&arbol().arbol);
    f.esquemas.insert(
        "acme.ftp".to_owned(),
        vec![norte_proto::methods::PluginConfigKeyWire {
            key: "mode".to_owned(),
            kind: "enum".to_owned(),
            default: "safe\u{202e}".to_owned(),
            min: None,
            max: None,
            values: vec!["safe".to_owned(), "fast\u{202e} · read-only".to_owned()],
            description: None,
            value: "fast\u{7}".to_owned(),
        }],
    );
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let _ = extensiones_cargadas(&mut sub).await;
    h.dispatch(tecla("Enter")).await.expect("host vivo");

    let mut ficha = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if let Some(d) = siguiente_foto(&mut sub)
            .await
            .extensions
            .and_then(|e| e.detail)
        {
            ficha = Some(d);
            break;
        }
    }
    let ficha = ficha.expect("la ficha llega");
    let fila = ficha.config.first().expect("la clave está");
    let texto = format!("{fila:?}");
    assert!(
        !texto.contains('\u{202e}') && !texto.contains('\u{7}'),
        "texto del plugin sin enmascarar: {texto}"
    );
    assert!(
        !texto.contains("\\u{202e}") && !texto.contains("\\u{7}"),
        "texto del plugin sin enmascarar: {texto}"
    );
    assert!(
        fila.hostile,
        "y se DICE que lo pintado difiere de lo que es: {fila:?}"
    );
}

/// Con el gestor abierto, el listado no se mueve.
#[tokio::test]
async fn con_el_gestor_abierto_el_listado_no_se_mueve() {
    let (h, snap) = host_arbol(arbol()).await;
    let antes = listado(&snap).cursor;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let _ = siguiente_extensiones(&mut sub).await.expect("abre");

    h.dispatch(tecla("j")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(listado(&foto).cursor, antes);
    assert!(foto.extensions.is_some(), "y el gestor sigue abierto");
}

// ---------------------------------------------------------------------------
// El tema y el selector de volúmenes (tarea 4.5).
// ---------------------------------------------------------------------------

/// Espera la siguiente actualización con el tema.
async fn siguiente_tema(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::ThemeView> {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv())
            .await
            .expect("una actualización antes del plazo")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Theme { theme } = c {
                    return theme.clone();
                }
            }
        }
    }
    panic!("no llegó ninguna actualización con tema");
}

/// Espera la siguiente actualización con el selector.
async fn siguiente_selector(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::PickerView> {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv())
            .await
            .expect("una actualización antes del plazo")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Picker { picker } = c {
                    return picker.clone();
                }
            }
        }
    }
    panic!("no llegó ninguna actualización con selector");
}

/// Un host con un tema dicho.
async fn host_con_tema(theme: norte_ui_host::pickers::HostTheme) -> UiHost {
    UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: ajustes_de_prueba(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme,
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca")
    .0
}

/// `F9` enseña el tema rol a rol, y NOMBRA los efectos que esta ventana no
/// sabe pintar: un tema retro que se ve idéntico se lee como roto.
#[tokio::test]
async fn el_tema_se_ve_por_dentro_y_dice_lo_que_no_pinta() {
    let h = host_con_tema(norte_ui_host::pickers::HostTheme {
        name: "retro".to_owned(),
        roles: vec![
            ("selection-bg".to_owned(), "#2d4f8a".to_owned()),
            ("error-fg".to_owned(), "#f7768e".to_owned()),
        ],
        effects: vec!["crt".to_owned(), "scanlines".to_owned()],
    })
    .await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F9")).await.expect("host vivo");
    let t = siguiente_tema(&mut sub).await.expect("abre");

    assert_eq!(t.name, "retro");
    assert_eq!(t.roles.len(), 2);
    assert_eq!(t.roles[0].color, "#2d4f8a", "el color va como muestra");
    assert_eq!(
        t.unsupported_effects
            .iter()
            .map(|e| e.key.clone())
            .collect::<Vec<_>>(),
        vec!["crt".to_owned(), "scanlines".to_owned()],
        "los efectos se NOMBRAN, no se ignoran"
    );

    h.dispatch(tecla("Escape")).await.expect("host vivo");
    assert!(siguiente_tema(&mut sub).await.is_none(), "esc lo cierra");
}

/// Un tema sin efectos no inventa ninguno.
#[tokio::test]
async fn un_tema_sin_efectos_no_dice_nada_de_ellos() {
    let h = host_con_tema(norte_ui_host::pickers::HostTheme {
        name: "default".to_owned(),
        roles: vec![("fg".to_owned(), "#d4d8de".to_owned())],
        effects: Vec::new(),
    })
    .await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F9")).await.expect("host vivo");
    let t = siguiente_tema(&mut sub).await.expect("abre");
    assert!(t.unsupported_effects.is_empty());
}

/// Un volumen del host con lo que la vista mira.
fn volumen(mount: &str, fs: &str, ro: bool) -> norte_proto::methods::Volume {
    norte_proto::methods::Volume {
        mount: norte_proto::VPath::parse(mount).expect("vpath"),
        label: None,
        fs_type: fs.to_owned(),
        kind: norte_proto::methods::VolumeKind::Fixed,
        total_bytes: Some(100 * 1024 * 1024 * 1024),
        free_bytes: Some(12 * 1024 * 1024 * 1024),
        read_only: ro,
    }
}

/// El selector de volúmenes se abre PREGUNTANDO, y elegir uno navega el
/// panel a su punto de montaje — que es lectura, y por eso sí se hace.
#[tokio::test]
async fn elegir_un_volumen_navega_el_panel() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
    f.pon("mem:///otro", vec![(b"raiz.txt".to_vec(), false)]);
    f.volumenes = vec![volumen("mem:///otro", "ext4", false)];
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    // `pane.select-drive` no lo ata el preset orthodox: se corre por la
    // paleta, que es otra puerta al MISMO catálogo.
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");
    let _ = siguiente_paleta(&mut sub).await.expect("la paleta abre");
    for c in "select-drive".chars() {
        h.dispatch(tecla(&c.to_string())).await.expect("host vivo");
    }
    h.dispatch(tecla("Enter")).await.expect("host vivo");

    let primero = siguiente_selector(&mut sub).await.expect("abre");
    assert!(
        !primero.empty.is_empty() || !primero.rows.is_empty(),
        "o pregunta o trae filas, pero nunca se queda mudo"
    );

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
    let v = con_filas.expect("la tabla de montaje llega");
    assert_eq!(v.rows.len(), 1);
    assert!(v.rows[0].detail.contains("ext4"), "{:?}", v.rows[0]);
    assert!(
        v.rows[0].detail.contains("12"),
        "y cuánto queda: {:?}",
        v.rows[0]
    );

    h.dispatch(tecla("Enter")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.picker.is_none(), "el selector se cierra");
    assert!(
        listado(&foto).path_display.contains("otro"),
        "y el panel navegó al volumen: {}",
        listado(&foto).path_display
    );
}

/// Un espacio que el sistema no contestó se DICE; jamás se pinta un `0`, que
/// se lee como «lleno» — lo contrario de «no lo sé».
#[tokio::test]
async fn un_volumen_sin_tamano_lo_dice() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    let mut v = volumen("mem:///otro", "nfs4", true);
    v.total_bytes = None;
    v.free_bytes = None;
    f.volumenes = vec![v];
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");
    let _ = siguiente_paleta(&mut sub).await.expect("la paleta abre");
    for c in "select-drive".chars() {
        h.dispatch(tecla(&c.to_string())).await.expect("host vivo");
    }
    h.dispatch(tecla("Enter")).await.expect("host vivo");

    for _ in 0..20 {
        let Some(view) = siguiente_selector(&mut sub).await else {
            continue;
        };
        if let Some(fila) = view.rows.first() {
            assert!(!fila.detail.contains(" 0 "), "un cero se lee como lleno");
            assert!(
                fila.detail.contains("nfs4"),
                "y sigue diciendo lo que sí sabe: {fila:?}"
            );
            return;
        }
    }
    panic!("la tabla de montaje nunca llegó");
}

// ---------------------------------------------------------------------------
// La hoja de atributos y el panel de procesos (huecos de la fase 4).
// ---------------------------------------------------------------------------

/// Un host con la disposición `full`, que trae hoja de atributos y panel de
/// procesos además de los dos listados.
async fn host_full(backend: Arc<Falso>) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("full").expect("layout"),
        viewport: (200, 60),
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

/// El PRIMER listado de una foto, sea cual sea su posición.
///
/// `listado` mira el hueco 0, que en `simple` es el listado; en `full` el
/// hueco 0 es la barra lateral de sitios.
fn primer_listado(snap: &norte_ui_host::ViewSnapshot) -> &norte_ui_host::dto::BrowserSlotView {
    snap.slots
        .iter()
        .find_map(|s| match s {
            SlotView::Browser(b) => Some(b.as_ref()),
            _ => None,
        })
        .expect("la disposición tiene algún listado")
}

/// La hoja de atributos de una foto, si está colocada.
fn hoja(snap: &norte_ui_host::ViewSnapshot) -> Option<&norte_ui_host::dto::MetadataSlotView> {
    snap.slots.iter().find_map(|s| match s {
        SlotView::Metadata(m) => Some(m.as_ref()),
        _ => None,
    })
}

/// La hoja de atributos enseña la entrada bajo el cursor del listado al que
/// SIGUE, y se mueve con él.
#[tokio::test]
async fn la_hoja_de_atributos_sigue_al_cursor() {
    let (h, snap) = host_full(arbol()).await;
    let mut sub = h.subscribe();
    let primera = hoja(&snap).expect("la disposición `full` coloca la hoja");
    assert!(
        primera.note.is_empty() && !primera.fields.is_empty(),
        "con un listado con entradas, la hoja enseña la primera: {primera:?}"
    );
    let nombre_de = |m: &norte_ui_host::dto::MetadataSlotView| {
        m.fields
            .first()
            .map(|f| f.value.clone())
            .unwrap_or_default()
    };
    let antes = nombre_de(primera);
    assert!(!antes.is_empty(), "el primer campo es el nombre");

    h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    let despues = nombre_de(hoja(&foto).expect("sigue colocada"));
    assert_ne!(
        antes, despues,
        "la hoja siguió al cursor sin que nadie pidiera nada"
    );
}

/// Con el FOCO en la propia hoja sigue enseñando la entrada del listado
/// activo: seguir al rol activo cuando el activo es ella misma era seguir a
/// nadie, y la hoja se vaciaba al pulsarla (mismo fallo que el visor
/// acoplado, #291).
#[tokio::test]
async fn la_hoja_de_atributos_enfocada_no_se_vacia() {
    let (h, snap) = host_full(arbol()).await;
    let mut sub = h.subscribe();
    let primera = hoja(&snap).expect("la disposición `full` coloca la hoja");
    assert!(!primera.fields.is_empty());
    let slot = primera.slot_id;
    let ack = h
        .dispatch(UiAction::FocusSlot { slot_id: slot })
        .await
        .expect("host vivo");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    let enfocada = foto_hasta(&h, &mut sub, "la hoja con el foco", |s| {
        (s.focus == Some(slot)).then(|| s.clone())
    })
    .await;
    let h2 = hoja(&enfocada).expect("sigue colocada");
    assert!(
        h2.note.is_empty() && h2.fields == primera.fields,
        "la hoja enfocada sigue enseñando la entrada del listado: {h2:?}"
    );
}

/// Un nombre hostil llega a la hoja enmascarado y MARCADO, igual que a una
/// fila del listado.
#[tokio::test]
async fn un_nombre_hostil_en_la_hoja_va_marcado() {
    let (h, snap) = host_full(arbol()).await;
    let mut sub = h.subscribe();
    // El árbol de pruebas tiene una entrada cuyo nombre no es UTF-8.
    let mut vista = hoja(&snap).expect("colocada").clone();
    for _ in 0..6 {
        if vista.fields.first().is_some_and(|f| f.hostile) {
            break;
        }
        h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        vista = hoja(&foto).expect("colocada").clone();
    }
    let nombre = vista.fields.first().expect("hay nombre");
    assert!(nombre.hostile, "la entrada no-UTF-8 se marca: {nombre:?}");
    assert!(
        !nombre.value.contains('\u{fffd}') || nombre.hostile,
        "y su texto ya viene saneado"
    );
}

/// Con el foco en el panel de PROCESOS, bajar baja por él y no por el
/// listado de al lado.
#[tokio::test]
async fn el_panel_de_procesos_toma_sus_teclas() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b"a.txt".to_vec(), false), (b"b.txt".to_vec(), false)],
    );
    let backend = Arc::new(f);
    let (h, snap) = host_full(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let cursor_antes = primer_listado(&snap).cursor;

    // Se lanzan dos borrados para que el tablero tenga filas. El id del
    // diálogo se LEE, y se espera al NUEVO: fijarlo a mano dejaba el segundo
    // sin contestar, y un diálogo abierto se queda el teclado —que es justo
    // lo que debe hacer—, así que el tabulador ya no llegaba a ningún lado.
    // `Enter` no vale para confirmarlo: en un borrado `confirm` es
    // destructivo y el teclado elige la primera respuesta que no lo es.
    let mut contestado = norte_ui_host::ModalId(0);
    for _ in 0..2 {
        h.dispatch(tecla("F8")).await.expect("host vivo");
        let id = loop {
            h.dispatch(UiAction::Resync).await.expect("host vivo");
            if let Some(d) = siguiente_foto(&mut sub).await.dialogs.last()
                && d.id != contestado
            {
                break d.id;
            }
        };
        contestado = id;
        h.dispatch(UiAction::Dialog {
            id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host vivo");
    }

    // Se rota el foco hasta el panel de procesos.
    let mut en_procesos = false;
    for _ in 0..8 {
        h.dispatch(tecla("Tab")).await.expect("host vivo");
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        let activo = foto
            .layout
            .placements
            .iter()
            .find(|p| p.role == Some(norte_ui_host::dto::SlotRole::Active))
            .map(|p| p.slot_id);
        if let Some(id) = activo
            && foto
                .slots
                .iter()
                .any(|s| matches!(s, SlotView::Processes { slot_id, .. } if *slot_id == id))
        {
            en_procesos = true;
            break;
        }
    }
    assert!(en_procesos, "el tabulador llega al panel de procesos");

    h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(
        primer_listado(&foto).cursor,
        cursor_antes,
        "bajar con el foco en procesos NO mueve el listado: el rol lo pintaba \
         enfocado y las teclas se iban al panel de al lado"
    );
}

/// La barra lateral de sitios de una foto, si está colocada.
fn sitios(snap: &norte_ui_host::ViewSnapshot) -> Option<&norte_ui_host::dto::PlacesSlotView> {
    snap.slots.iter().find_map(|s| match s {
        SlotView::Places(p) => Some(p.as_ref()),
        _ => None,
    })
}

/// La barra lateral llega con sus DOS cabeceras desde el primer frame, y los
/// volúmenes se le añaden cuando el host contesta.
#[tokio::test]
async fn la_barra_de_sitios_no_da_un_brinco_cuando_llegan_los_discos() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    f.volumenes = vec![volumen("mem:///otro", "ext4", false)];
    let (h, snap) = host_full(Arc::new(f)).await;
    let mut sub = h.subscribe();

    let primera = sitios(&snap).expect("la disposición `full` coloca la barra");
    let cabeceras = primera
        .rows
        .iter()
        .filter(|r| matches!(r, norte_ui_host::dto::PlaceRowView::Header { .. }))
        .count();
    assert_eq!(
        cabeceras, 2,
        "las dos cabeceras están desde el principio, aunque no haya nada debajo"
    );

    // Los volúmenes llegan después: la barra no espera a ellos para pintarse.
    let mut con_discos = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        let v = sitios(&foto).expect("sigue colocada").clone();
        if v.rows
            .iter()
            .any(|r| matches!(r, norte_ui_host::dto::PlaceRowView::Drive { .. }))
        {
            con_discos = Some(v);
            break;
        }
    }
    let v = con_discos.expect("los volúmenes llegan a la barra");
    let disco = v
        .rows
        .iter()
        .find_map(|r| match r {
            norte_ui_host::dto::PlaceRowView::Drive { detail, .. } => Some(detail.clone()),
            _ => None,
        })
        .expect("hay un disco");
    assert!(!disco.is_empty(), "y dice cuánto espacio tiene");
}

/// Con el foco en la barra lateral, bajar baja por ELLA, y entrar navega el
/// LISTADO — que es lo que hace que tenerla abierta no cambie a dónde van las
/// operaciones.
#[tokio::test]
async fn la_barra_de_sitios_navega_el_listado_y_no_se_lo_queda() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    f.pon("mem:///otro", vec![(b"raiz.txt".to_vec(), false)]);
    f.volumenes = vec![volumen("mem:///otro", "ext4", false)];
    let (h, _snap) = host_full(Arc::new(f)).await;
    let mut sub = h.subscribe();

    // Se espera a que los discos estén, y se busca su fila.
    let mut fila_del_disco = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        let v = sitios(&foto).expect("colocada");
        if let Some(i) = v
            .rows
            .iter()
            .position(|r| matches!(r, norte_ui_host::dto::PlaceRowView::Drive { .. }))
        {
            fila_del_disco = Some((i, v.generation));
            break;
        }
    }
    let (i, generacion) = fila_del_disco.expect("los discos llegan");

    // Un click en el disco: elige Y activa, porque una barra lateral existe
    // para ir a sitios.
    h.dispatch(UiAction::PlaceActivateRow {
        row: u32::try_from(i).expect("cabe"),
        generation: generacion,
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let mut llego = false;
    for _ in 0..20 {
        let foto = siguiente_foto(&mut sub).await;
        if primer_listado(&foto).path_display.contains("otro") {
            llego = true;
            break;
        }
        h.dispatch(UiAction::Resync).await.expect("host vivo");
    }
    assert!(llego, "el LISTADO navegó al volumen, no la barra");
}

/// Un click en la barra lateral no puede navegar a un sitio que no se pulsó.
///
/// Los volúmenes llegan de una tarea de fondo y se insertan EN MEDIO —las
/// unidades van antes que los favoritos—, así que entre que el usuario suelta
/// el botón sobre un favorito y el host atiende la acción, esa fila es otra.
/// Sin generación el host la aceptaba, y `set_cursor` recorta en vez de
/// rechazar, así que el peor caso era navegar al ÚLTIMO sitio de la lista con
/// un acuse `Applied`. Es la carrera que el ADR 0068 existe para cerrar.
#[tokio::test]
async fn un_click_en_la_barra_no_navega_a_otro_sitio_si_la_lista_cambio() {
    let mut cfg = ajustes_de_prueba();
    cfg.common.hotlist = vec![norte_config::HotlistItem {
        name: "proyectos".to_owned(),
        target: norte_proto::VPath::parse("mem:///proyectos").map_err(|_| "err".to_owned()),
    }];
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    f.pon("mem:///proyectos", vec![(b"p.txt".to_vec(), false)]);
    f.pon("mem:///boot", vec![(b"vmlinuz".to_vec(), false)]);
    f.pon("mem:///datos", vec![(b"d.txt".to_vec(), false)]);
    // DOS discos: desplazan el favorito lo justo para que su índice caiga
    // sobre un disco y no sobre una cabecera. Con uno el click habría plegado
    // una sección, que también está mal pero se nota menos.
    f.volumenes = vec![
        volumen("mem:///boot", "ext4", false),
        volumen("mem:///datos", "ext4", false),
    ];
    let (h, snap) = UiHost::start(UiHostOptions {
        backend: Arc::new(f),
        initial_dir: dir(),
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("full").expect("layout"),
        viewport: (200, 60),
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
    let mut sub = h.subscribe();

    // La foto que el usuario TIENE DELANTE, antes de que lleguen los discos.
    let antes = sitios(&snap).expect("colocada").clone();
    let fila_pulsada = antes
        .rows
        .iter()
        .position(|r| matches!(r, norte_ui_host::dto::PlaceRowView::Favorite { .. }))
        .expect("el favorito está desde el principio");

    // Los discos aterrizan y la lista es OTRA.
    let mut despues = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let v = sitios(&siguiente_foto(&mut sub).await)
            .expect("colocada")
            .clone();
        if v.generation != antes.generation {
            despues = Some(v);
            break;
        }
    }
    let despues = despues.expect("los volúmenes llegan y suben la generación");
    assert!(
        matches!(
            despues.rows.get(fila_pulsada),
            Some(norte_ui_host::dto::PlaceRowView::Drive { .. })
        ),
        "la fila que se pulsó es ahora un DISCO, que es lo que hace peligroso \
         el índice desnudo: {:?}",
        despues.rows.get(fila_pulsada)
    );

    // El click en vuelo, con la generación de la pantalla que se vio.
    let ack = h
        .dispatch(UiAction::PlaceActivateRow {
            row: u32::try_from(fila_pulsada).expect("cabe"),
            generation: antes.generation,
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, norte_ui_host::ActionAck::Stale { .. }),
        "se rechaza en vez de navegar a otro sitio: {ack:?}"
    );
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    for _ in 0..8 {
        let foto = siguiente_foto(&mut sub).await;
        assert!(
            !primer_listado(&foto).path_display.contains("boot"),
            "y el panel NO se fue al disco que nadie pulsó"
        );
        h.dispatch(UiAction::Resync).await.expect("host vivo");
    }

    // Con la generación buena, el mismo click sí va.
    let ack = h
        .dispatch(UiAction::PlaceActivateRow {
            row: u32::try_from(fila_pulsada).expect("cabe"),
            generation: despues.generation,
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, norte_ui_host::ActionAck::Applied { .. }),
        "y la generación buena sí vale: {ack:?}"
    );
}

/// Un favorito cuya ruta no parsea se PINTA con su motivo: uno que
/// desaparece en silencio es un fallo de configuración que nadie puede ver.
#[tokio::test]
async fn un_favorito_roto_se_ve_y_dice_por_que() {
    let mut cfg = ajustes_de_prueba();
    cfg.common.hotlist = vec![
        norte_config::HotlistItem {
            name: "casa".to_owned(),
            target: norte_proto::VPath::parse("mem:///casa").map_err(|_| "err".to_owned()),
        },
        norte_config::HotlistItem {
            name: "roto".to_owned(),
            target: Err("err-invalid-path".to_owned()),
        },
    ];
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    let (h, snap) = UiHost::start(UiHostOptions {
        backend: Arc::new(f),
        initial_dir: dir(),
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("full").expect("layout"),
        viewport: (200, 60),
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
    let _ = &h;

    let v = sitios(&snap).expect("colocada");
    let favoritos: Vec<(String, String)> = v
        .rows
        .iter()
        .filter_map(|r| match r {
            norte_ui_host::dto::PlaceRowView::Favorite { name, broken, .. } => {
                Some((name.clone(), broken.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        favoritos.len(),
        2,
        "los dos favoritos se ven: {favoritos:?}"
    );
    let roto = favoritos
        .iter()
        .find(|(n, _)| n == "roto")
        .expect("el roto está");
    assert!(!roto.1.is_empty(), "y dice por qué está roto");
    assert!(
        !roto.1.starts_with("err-"),
        "traducido, no la clave: {roto:?}"
    );
}

// ---------------------------------------------------------------------------
// Disposiciones: redimensionar, igualar y elegir.
// ---------------------------------------------------------------------------

/// Corre un comando por la PALETA, que es otra puerta al mismo catálogo.
///
/// Ninguno de los comandos de disposición lo ata un preset de fábrica, así
/// que este es el camino por el que llegan hoy.
async fn por_la_paleta(h: &UiHost, sub: &mut norte_ui_host::UiSubscription, cmd: &str) {
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");
    let _ = siguiente_paleta(sub).await.expect("la paleta abre");
    for c in cmd.chars() {
        h.dispatch(tecla(&c.to_string())).await.expect("host vivo");
    }
    h.dispatch(tecla("Enter")).await.expect("host vivo");
}

/// El ancho de un hueco en la foto.
fn ancho_de(snap: &norte_ui_host::ViewSnapshot, slot: u32) -> u16 {
    snap.layout
        .placements
        .iter()
        .find(|p| p.slot_id == slot)
        .map_or(0, |p| p.width)
}

/// Crecer ensancha el hueco con el FOCO, y encoger lo devuelve.
#[tokio::test]
async fn crecer_y_encoger_mueven_el_hueco_enfocado() {
    // Una disposición con DOS listados: redimensionar reparte entre
    // hermanos, y con un hueco solo no hay a quién quitarle.
    let (h, snap) = host_con_layout(arbol(), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    let activo = snap
        .layout
        .placements
        .iter()
        .find(|p| p.role == Some(norte_ui_host::dto::SlotRole::Active))
        .map(|p| p.slot_id)
        .expect("hay un hueco activo");
    let antes = ancho_de(&snap, activo);

    por_la_paleta(&h, &mut sub, "layout.grow").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let crecido = siguiente_foto(&mut sub).await;
    assert!(
        ancho_de(&crecido, activo) > antes,
        "creció: {} → {}",
        antes,
        ancho_de(&crecido, activo)
    );

    por_la_paleta(&h, &mut sub, "layout.shrink").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let vuelto = siguiente_foto(&mut sub).await;
    assert_eq!(ancho_de(&vuelto, activo), antes, "y encoger lo devuelve");
}

/// Igualar deja a los hermanos con el mismo peso.
#[tokio::test]
async fn igualar_reparte_a_partes_iguales() {
    let (h, snap) = host_con_layout(arbol(), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    let activo = snap
        .layout
        .placements
        .iter()
        .find(|p| p.role == Some(norte_ui_host::dto::SlotRole::Active))
        .map(|p| p.slot_id)
        .expect("hay un hueco activo");

    // Se desequilibra y se vuelve a igualar.
    for _ in 0..3 {
        por_la_paleta(&h, &mut sub, "layout.grow").await;
    }
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let torcido = ancho_de(&siguiente_foto(&mut sub).await, activo);

    por_la_paleta(&h, &mut sub, "layout.equalize").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let igualado = ancho_de(&siguiente_foto(&mut sub).await, activo);
    assert!(igualado < torcido, "igualar deshace el desequilibrio");
}

/// El selector ofrece las cinco de fábrica con su vista previa, y elegir una
/// CAMBIA la pantalla.
#[tokio::test]
async fn elegir_una_disposicion_cambia_la_pantalla() {
    let (h, snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let huecos_antes = snap.slots.len();

    por_la_paleta(&h, &mut sub, "layout.pick").await;
    let mut v = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        if let Some(l) = foto.layouts.clone() {
            v = Some(l);
            break;
        }
    }
    let picker = v.expect("el selector abre");
    assert_eq!(
        picker.rows.len(),
        norte_frontend::layout::presets::NAMES.len(),
        "las cinco de fábrica, y ninguna del usuario en este host"
    );
    assert!(picker.rows.iter().all(|r| r.factory));
    assert!(
        !picker.preview.is_empty(),
        "y la elegida enseña su FORMA, pintada por el mismo motor que reparte"
    );
    let anchos: std::collections::BTreeSet<usize> =
        picker.preview.iter().map(|l| l.chars().count()).collect();
    assert_eq!(anchos.len(), 1, "la miniatura es un rectángulo: {anchos:?}");

    // La disposición `full` tiene más huecos que `simple`.
    let i = picker
        .rows
        .iter()
        .position(|r| r.name == "full")
        .expect("`full` está");
    h.dispatch(UiAction::LayoutActivateRow {
        row: u32::try_from(i).expect("cabe"),
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let despues = siguiente_foto(&mut sub).await;
    assert!(despues.layouts.is_none(), "el selector se cierra al elegir");
    assert!(
        despues.slots.len() > huecos_antes,
        "y la pantalla es otra: {} → {}",
        huecos_antes,
        despues.slots.len()
    );
    assert!(
        despues
            .slots
            .iter()
            .any(|s| matches!(s, SlotView::Places(_))),
        "con la barra lateral que `full` coloca"
    );
}

/// Una disposición del usuario que no parsea se OFRECE, sin vista previa y
/// diciendo por qué, y elegirla no cambia la pantalla.
#[tokio::test]
async fn una_disposicion_rota_se_ve_y_no_se_aplica() {
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: ajustes_de_prueba(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: vec![norte_frontend::layout_picker::UserLayout {
            name: std::ffi::OsString::from("mia"),
            tree: Err("no parsea".to_owned()),
        }],
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        profile: None,
        log_ring: None,
    })
    .await
    .expect("arranca");
    let mut sub = h.subscribe();

    por_la_paleta(&h, &mut sub, "layout.pick").await;
    let mut picker = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if let Some(l) = siguiente_foto(&mut sub).await.layouts.clone() {
            picker = Some(l);
            break;
        }
    }
    let picker = picker.expect("el selector abre");
    let i = picker
        .rows
        .iter()
        .position(|r| r.name == "mia")
        .expect("la del usuario se OFRECE aunque no parsee");
    assert!(picker.rows[i].broken, "y se dice que está rota");
    assert!(!picker.rows[i].factory);

    let ack = h
        .dispatch(UiAction::LayoutActivateRow {
            row: u32::try_from(i).expect("cabe"),
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, norte_ui_host::ActionAck::Unavailable { .. }),
        "elegirla NO cambia la pantalla por un fichero roto: {ack:?}"
    );
}

/// Una disposición cuyo REPARTO no coloca ningún listado no puede dejar al
/// host sin huecos.
///
/// `validate` garantiza que el árbol TENGA un `browser`, no que el reparto lo
/// COLOQUE: un `Tabs` cuyo activo es otro kind manda el listado a `hidden`, y
/// un split todo-ponderado que no quepa hace lo mismo con todos menos el hijo
/// 0. Sembrar los huecos desde `placements` en vez de desde el árbol vaciaba
/// el mapa, y la siguiente tecla moría en el `expect` de `hueco()` —dentro de
/// la task del actor, sin log y sin caída visible, dejando la ventana muerta
/// contestando `Down` para siempre. Es la forma de #242 en esta superficie.
#[tokio::test]
async fn una_disposicion_que_esconde_el_listado_deja_el_hueco_vivo() {
    use norte_frontend::layout::{KindId, Node, SlotId};
    let escondida = Node::Tabs {
        children: vec![
            Node::slot(SlotId(9), KindId::new("metadata")),
            Node::slot(SlotId(1), KindId::browser()),
        ],
        active: 0,
    };
    norte_frontend::layout::validate(&escondida)
        .expect("el árbol es VÁLIDO: tiene un browser, aunque el reparto no lo coloque");

    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: ajustes_de_prueba(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: vec![norte_frontend::layout_picker::UserLayout {
            name: std::ffi::OsString::from("escondida"),
            tree: Ok(escondida),
        }],
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        profile: None,
        log_ring: None,
    })
    .await
    .expect("arranca");
    let mut sub = h.subscribe();

    por_la_paleta(&h, &mut sub, "layout.pick").await;
    let mut picker = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if let Some(l) = siguiente_foto(&mut sub).await.layouts.clone() {
            picker = Some(l);
            break;
        }
    }
    let picker = picker.expect("el selector abre");
    let i = picker
        .rows
        .iter()
        .position(|r| r.name == "escondida")
        .expect("la del usuario está");
    h.dispatch(UiAction::LayoutActivateRow {
        row: u32::try_from(i).expect("cabe"),
    })
    .await
    .expect("host vivo");

    // La tecla que mataba: cualquiera que toque el hueco activo.
    h.dispatch(tecla("Down"))
        .await
        .expect("el host sigue VIVO tras elegir una disposición que esconde el listado");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let despues = siguiente_foto(&mut sub).await;
    assert!(
        despues
            .slots
            .iter()
            .any(|s| matches!(s, SlotView::Metadata(_))),
        "y la pantalla es la que se pidió: la pestaña activa es la ficha"
    );
}

// ---------------------------------------------------------------------------
// Buscar por el subárbol (tarea 6.1).
// ---------------------------------------------------------------------------

/// Espera la siguiente actualización con la búsqueda.
async fn siguiente_busqueda(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::SearchView> {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv())
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
fn arbol_con_hallazgos(patron: &str, rutas: &[&str]) -> Arc<Falso> {
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

/// Lanza la búsqueda `patron` por el prompt y devuelve su primera vista.
async fn buscar(
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
// El corpus canónico contra las superficies nuevas.
// ---------------------------------------------------------------------------

/// Ninguna superficie deja pasar un peligro de terminal, y la que enmascara
/// lo DICE.
///
/// Una tabla sobre el corpus de `norte-testkit`, que es lo que faltaba: las
/// superficies de esta fase se escribieron sin que ninguna lo tocara, y todas
/// las banderas que se calculaban y se tiraban habrían salido de aquí. La
/// propiedad es un PAR: lo pintado no lleva peligro Y la marca está puesta.
/// Comprobar solo lo primero es lo que deja pasar una superficie que enmascara
/// en silencio.
///
/// TRES superficies, y se dice cuáles porque el doc de antes prometía nueve y
/// ejercitaba dos (#277): el nombre de un FAVORITO (lo escribe el usuario en
/// su `norte.toml`), la etiqueta de un VOLUMEN (la da el sistema y son bytes)
/// y el valor de un ATRIBUTO (el nombre de la entrada bajo el cursor). El
/// diálogo de APROBACIÓN tiene su propia tabla, porque sus rutas llegan del
/// daemon ya redactadas y hay que pasarlas antes por el mismo lossy.
#[tokio::test]
// Larga por TABLA, no por lógica: cada superficie es un bloque con su
// aserción y su frase, y partirla escondería cuáles se cubren.
#[allow(clippy::too_many_lines)]
async fn ninguna_superficie_enmascara_en_silencio() {
    let corpus = norte_testkit::corpus::hostile_names();
    assert!(
        corpus.len() >= 48,
        "el corpus canónico está: {}",
        corpus.len()
    );

    // Los que de verdad ALTERAN la pantalla. Un nombre largo o con NFD no se
    // enmascara —ni debe—, así que exigirle marca sería exigir una mentira.
    let alteran: Vec<&norte_testkit::corpus::HostileName> = corpus
        .iter()
        .filter(|n| norte_frontend::display_name(&n.bytes).1)
        .collect();
    assert!(
        alteran.len() >= 8,
        "el corpus trae peligros de verdad: {}",
        alteran.len()
    );

    for n in alteran {
        // El nombre de un favorito vive en un `String` del `norte.toml`, así
        // que solo puede llevar lo que sea UTF-8 válido. Convertir el resto
        // con `from_utf8_lossy` sería hacer aquí la conversión que el host
        // tiene que marcar, y el test diría que el host no la marca cuando
        // quien la hizo fue el test: es la trampa del doble lossy, que la
        // bandera ya no puede recuperar porque U+FFFD no es un peligro.
        let texto = match std::str::from_utf8(&n.bytes) {
            Ok(t) => t.to_owned(),
            Err(_) => String::new(),
        };

        // 1. El nombre de un FAVORITO: lo escribe el usuario, y la capa de
        //    proyecto es «he abierto este repo», no «doy fe de esta cadena».
        let mut cfg = ajustes_de_prueba();
        cfg.common.hotlist = vec![norte_config::HotlistItem {
            name: texto.clone(),
            target: norte_proto::VPath::parse("mem:///casa").map_err(|_| "err".to_owned()),
        }];
        let mut f = Falso::default();
        // La entrada bajo el cursor es la hostil: su nombre es el primer campo
        // de la HOJA DE ATRIBUTOS, que es la tercera superficie.
        f.pon("mem:///casa", vec![(n.bytes.clone(), false)]);
        // 2. La etiqueta de un VOLUMEN: la da el sistema y son bytes.
        f.volumenes = vec![norte_proto::methods::Volume {
            label: Some(n.bytes.clone()),
            ..volumen("mem:///casa", "ext4", false)
        }];
        let (h, snap) = UiHost::start(UiHostOptions {
            backend: Arc::new(f),
            initial_dir: dir(),
            locale: "es".to_owned(),
            keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
            keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
            keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox")
                .expect("preset"),
            layout: norte_frontend::layout::presets::tree("full").expect("layout"),
            viewport: (200, 60),
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
        let mut sub = h.subscribe();

        let barra = sitios(&snap).expect("`full` coloca la barra").clone();
        if !texto.is_empty() {
            let favorito = barra
                .rows
                .iter()
                .find_map(|r| match r {
                    norte_ui_host::dto::PlaceRowView::Favorite { name, hostile, .. } => {
                        Some((name.clone(), *hostile))
                    }
                    _ => None,
                })
                .expect("el favorito está");
            sin_peligro(&favorito.0, &n.id, "el nombre de un favorito");
            assert!(
                favorito.1,
                "[{}] el favorito se enmascara y NO lo dice: {:?}",
                n.id, favorito.0
            );
        }

        // La barra lateral, cuando lleguen los volúmenes.
        for _ in 0..20 {
            h.dispatch(UiAction::Resync).await.expect("host vivo");
            let foto = siguiente_foto(&mut sub).await;
            let v = sitios(&foto).expect("colocada");
            let disco = v.rows.iter().find_map(|r| match r {
                norte_ui_host::dto::PlaceRowView::Drive { label, hostile, .. } => {
                    Some((label.clone(), *hostile))
                }
                _ => None,
            });
            if let Some((label, hostile)) = disco {
                sin_peligro(&label, &n.id, "la etiqueta de un volumen en la barra");
                assert!(
                    hostile,
                    "[{}] la etiqueta del volumen se enmascara y NO lo dice: {label:?}",
                    n.id
                );
                break;
            }
        }

        // 3. El valor de un ATRIBUTO: el primer campo de la hoja es el nombre
        //    de la entrada bajo el cursor, o sea bytes del provider (#277).
        //    El doc de este test la nombraba desde el principio y nadie la
        //    ejercitaba.
        let foto = esperar_foto(&h, &mut sub, "la hoja tiene el nombre", |f| {
            hoja(f).is_some_and(|m| !m.fields.is_empty())
        })
        .await;
        let campo = hoja(&foto)
            .expect("la disposición `full` coloca la hoja")
            .fields
            .first()
            .expect("el primer campo es el nombre")
            .clone();
        sin_peligro(&campo.value, &n.id, "el valor de un atributo");
        assert!(
            campo.hostile,
            "[{}] el valor del atributo se enmascara y NO lo dice: {:?}",
            n.id, campo.value
        );
    }
}

/// Ninguna cadena pintable lleva un peligro de terminal.
fn sin_peligro(pintado: &str, id: &str, donde: &str) {
    for c in pintado.chars() {
        assert!(
            !norte_encoding::is_terminal_hazard(c),
            "[{id}] {donde} lleva {c:?} sin enmascarar: {pintado:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Decoraciones y columnas de plugin (tarea 4.2).
// ---------------------------------------------------------------------------

/// Un backend con `n` entradas, una insignia en la primera y una columna de
/// plugin con valor para todas.
fn arbol_grande_con_plugins(n: usize) -> Arc<Falso> {
    let nombres: Vec<(Vec<u8>, bool)> = (0..n)
        .map(|i| (format!("f{i:05}.txt").into_bytes(), false))
        .collect();
    let mut f = Falso::default();
    f.arbol.insert("mem:///casa".to_owned(), nombres);
    f.decoraciones
        .insert("mem:///casa/f00000.txt".to_owned(), "M".to_owned());
    *f.plugins.lock().expect("plugins") = vec![{
        let mut p = extension("acme.git", "Git", true);
        // DECLARADA en el catálogo: `validated_plugin_requests` no pide una
        // columna que su plugin no dice tener, para no atribuirla a quien no
        // es.
        p.columns = vec![norte_proto::methods::PluginColumnInfo {
            id: "status".to_owned(),
            header: "Estado".to_owned(),
        }];
        p
    }];
    for i in 0..n {
        f.valores_de_columna.insert(
            ("status".to_owned(), format!("mem:///casa/f{i:05}.txt")),
            "limpio".to_owned(),
        );
    }
    Arc::new(f)
}

/// Solo se le pregunta a los plugins por lo que se VE.
///
/// Cada llamada levanta una instancia de wasm por plugin: #224 midió 167 ms
/// por página de 20 sobre 2000 entradas. Preguntar por el directorio entero
/// multiplica ese precio por el tamaño del directorio, y para nada — el
/// renderer solo puede pintar su ventana. Es donde esto se separa del TUI,
/// que decora todo lo cargado porque su pane no declara ventana.
#[tokio::test]
async fn a_los_plugins_solo_se_les_pregunta_por_la_ventana() {
    const TOTAL: usize = 2000;
    let backend = arbol_grande_con_plugins(TOTAL);
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: Arc::clone(&backend) as Arc<dyn norte_ui_host::HostBackend>,
        initial_dir: dir(),
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
        columns: columnas_de(&["name", "size", "plugin:acme.git/status"]),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca");
    let mut sub = h.subscribe();

    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 0,
        count: 20,
    })
    .await
    .expect("host vivo");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let _ = siguiente_foto(&mut sub).await;
    }

    let lotes = backend.decorados.lock().expect("mutex").clone();
    assert!(!lotes.is_empty(), "se pregunta a los plugins");
    for lote in &lotes {
        assert!(
            lote.len() <= 20,
            "un lote de {} rutas sobre {TOTAL} entradas: se está pidiendo más \
             que la ventana",
            lote.len()
        );
    }
    let pedidas: usize = lotes.iter().map(Vec::len).sum();
    assert!(
        pedidas <= 40,
        "en total se pidieron {pedidas} de {TOTAL}: la ventana es 20"
    );

    // Y la columna de plugin viaja por el mismo lote, no por otro barrido.
    let cols = backend.columnas_pedidas.lock().expect("mutex").clone();
    assert!(!cols.is_empty(), "la columna configurada se pide");
    for (plugin, columna, paths) in &cols {
        assert_eq!(plugin, "acme.git");
        assert_eq!(columna, "status");
        assert!(
            paths.len() <= 20,
            "la columna se pide para {} rutas, no para la ventana",
            paths.len()
        );
    }
}

/// La insignia y el valor de columna llegan a la fila, marcados como lo que
/// son: texto de un TERCERO.
#[tokio::test]
async fn la_insignia_de_un_plugin_llega_a_la_fila() {
    let backend = arbol_grande_con_plugins(3);
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: Arc::clone(&backend) as Arc<dyn norte_ui_host::HostBackend>,
        initial_dir: dir(),
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
        columns: columnas_de(&["name", "plugin:acme.git/status"]),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca");
    let mut sub = h.subscribe();

    // Lo que hace el renderer nada más montarse. El arranque NO adorna ni
    // sondea: espera a que se declare la ventana, igual que con los tamaños
    // —pedir por una ventana inventada es pedir de más—.
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 0,
        count: 10,
    })
    .await
    .expect("host vivo");

    let mut adornada = None;
    for _ in 0..30 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        let b = listado(&foto);
        if let Some(f) = b.rows.iter().find(|r| !r.badge.is_empty()) {
            adornada = Some(f.clone());
            break;
        }
    }
    let fila = adornada.expect("la insignia llega a la fila");
    assert_eq!(fila.display_name, "f00000.txt");
    assert_eq!(fila.badge, "M");
    assert_eq!(
        fila.badge_role, "warning",
        "el rol viene del vocabulario CERRADO del tema, no de una cadena \
         libre que el plugin elija"
    );

    // Y la celda de la columna del plugin.
    let celda = fila
        .cells
        .iter()
        .find(|c| c.column == "plugin:acme.git/status")
        .expect("la columna configurada tiene su celda");
    assert_eq!(celda.text.as_deref(), Some("limpio"));
}

/// Lo que el provider se SALTÓ al listar se dice, y traducido.
///
/// Es la clase de fallo que no se puede descubrir mirando: lo que falta no
/// está, así que no hay ninguna fila donde el lector pueda tropezarse con
/// ello. Un listado incompleto que se calla miente por omisión. La cuenta la
/// da el provider —`FsListResult::skipped`— y `HostBackend::list` la TIRABA.
#[tokio::test]
async fn lo_que_el_provider_se_salto_se_dice() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    f.omitidas = Some(3);
    let (h, snap) = host_arbol(Arc::new(f)).await;
    let _ = &h;

    let b = listado(&snap);
    assert_eq!(b.rows.len(), 1, "se pinta lo que sí vino");
    assert!(
        b.skipped_note.contains('3'),
        "y se dice cuántas faltan: {:?}",
        b.skipped_note
    );
    assert!(
        !b.skipped_note.starts_with("listing-"),
        "traducido, no la clave: {:?}",
        b.skipped_note
    );
}

/// Un provider que no lleva la cuenta NO dice que no se saltó ninguna.
///
/// `None` y `Some(0)` no son lo mismo, y afirmar «no falta nada» cuando
/// nadie lo ha comprobado es peor que callarse.
#[tokio::test]
async fn un_provider_sin_cuenta_no_afirma_nada() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    f.omitidas = None;
    let (h, snap) = host_arbol(Arc::new(f)).await;
    let _ = &h;
    assert!(listado(&snap).skipped_note.is_empty());
}

/// Y la cuenta es de ESTE listado: no se arrastra al siguiente directorio.
#[tokio::test]
async fn la_cuenta_de_omitidas_no_sobrevive_a_un_cd() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"docs".to_vec(), true)]);
    f.pon("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    f.omitidas = Some(2);
    let backend = Arc::new(f);
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    assert!(!listado(&snap).skipped_note.is_empty(), "el primero sí");

    // El segundo directorio también las salta —el doble contesta lo mismo—,
    // pero lo que importa es que la cuenta se VUELVA a poner y no se herede:
    // `set_listing` la limpia, así que sin `set_skipped` después quedaría
    // vacía. Se comprueba que sigue diciéndose.
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
    for _ in 0..20 {
        let foto = siguiente_foto(&mut sub).await;
        if listado(&foto).path_display.contains("docs") {
            assert!(
                !listado(&foto).skipped_note.is_empty(),
                "la cuenta se vuelve a poner tras el `cd`"
            );
            return;
        }
        h.dispatch(UiAction::Resync).await.expect("host vivo");
    }
    panic!("nunca llegó el listado de docs");
}

// ---------------------------------------------------------------------------
// El selector de columnas (tarea 4.2).
// ---------------------------------------------------------------------------

/// La primera vista del selector con el cursor donde `aguja` diga.
async fn selector_columnas(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::dto::ColumnsPickerView {
    por_la_paleta(h, sub, "pane.columns").await;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if let Some(c) = siguiente_foto(sub).await.columns.clone() {
            return c;
        }
    }
    panic!("el selector de columnas no abre");
}

/// El selector enseña lo configurado, dice su ALCANCE y avisa de que lo
/// elegido no se guarda.
///
/// Lo último importa: esta fase no escribe configuración, y un selector que
/// se calla deja al usuario creyendo que acaba de configurar norte.
#[tokio::test]
async fn el_selector_de_columnas_dice_su_alcance_y_que_no_guarda() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let c = selector_columnas(&h, &mut sub).await;

    assert!(!c.rows.is_empty(), "hay columnas que enseñar");
    assert!(
        c.rows[0].fixed,
        "la primera es el NOMBRE, y no se apaga ni se mueve: {:?}",
        c.rows[0]
    );
    assert!(
        !c.title.starts_with("columns-picker"),
        "el título viene traducido: {:?}",
        c.title
    );
    assert!(
        !c.note.is_empty() && !c.note.starts_with("columns-picker"),
        "y dice que no guarda, traducido: {:?}",
        c.note
    );
    for r in &c.rows {
        assert!(!r.label.is_empty(), "cada fila dice cómo se llama: {r:?}");
    }
}

/// Encender una columna `attr:` RE-LISTA el hueco.
///
/// Los valores de un atributo solo llegan si se piden en `fs.list`, así que
/// una columna nueva sobre el listado viejo se quedaría en blanco — y en
/// blanco significa «este fichero no tiene ese atributo», que es otra cosa.
/// La huella que decide si hace falta es la COMPARTIDA (`pane_fingerprint`),
/// la misma que usa el TUI.
#[tokio::test]
async fn encender_una_columna_attr_vuelve_a_listar() {
    let backend = arbol();
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: Arc::clone(&backend) as Arc<dyn norte_ui_host::HostBackend>,
        initial_dir: dir(),
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
        // SIN la columna de modo: encenderla es lo que cambia la huella.
        columns: columnas_de(&["name", "size", "attr:posix.mode"]),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca");
    let mut sub = h.subscribe();
    let c = selector_columnas(&h, &mut sub).await;

    // Se baja hasta la fila del atributo y se APAGA: quitarla también cambia
    // la huella, y es el caso que no pide un viaje de más al daemon... pero
    // sí un re-listado, porque `attrs_de` deja de pedirla.
    let fila = c
        .rows
        .iter()
        .position(|r| r.id == "attr:posix.mode")
        .expect("la columna de modo está");
    for _ in 0..fila {
        h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    }
    let antes = backend.listados.load(Ordering::SeqCst);
    h.dispatch(tecla(" ")).await.expect("host vivo");
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    // La foto puede venir atrasada, así que se drena hasta ver el efecto.
    let mut cerrado = false;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if siguiente_foto(&mut sub).await.columns.is_none() {
            cerrado = true;
            break;
        }
    }
    assert!(cerrado, "el selector se cierra al aplicar");

    for _ in 0..20 {
        if backend.listados.load(Ordering::SeqCst) > antes {
            return;
        }
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let _ = siguiente_foto(&mut sub).await;
    }
    panic!(
        "cambiar el conjunto de columnas `attr:` tiene que RE-LISTAR: los \
         valores de un atributo solo llegan pidiéndolos en `fs.list`, y sin \
         volver a pedirlo la columna se queda en blanco — que significa otra \
         cosa"
    );
}

/// Y cambiar solo el ORDEN no re-lista: no cambia qué se pide al provider.
#[tokio::test]
async fn cambiar_el_orden_de_las_columnas_no_vuelve_a_listar() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let _ = selector_columnas(&h, &mut sub).await;

    let antes = backend.listados.load(Ordering::SeqCst);
    h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    h.dispatch(tecla("s")).await.expect("host vivo");
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    for _ in 0..10 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let _ = siguiente_foto(&mut sub).await;
    }
    assert_eq!(
        backend.listados.load(Ordering::SeqCst),
        antes,
        "ordenar es cosa del pane: no hay nada nuevo que pedirle al provider"
    );
}

/// El PIE del selector anuncia teclas, y esas teclas hacen lo que dice.
///
/// El pie es una cadena del catálogo y las teclas son un `match` del host:
/// dos sitios, ninguna atadura. La primera versión de esto escuchaba `J`/`K`
/// y `→` mientras el pie prometía `Shift+↑/↓` y `F` — una mentira que solo
/// se descubre probando, y que ningún test verde decía.
#[tokio::test]
async fn las_teclas_del_selector_de_columnas_son_las_que_anuncia_su_pie() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let antes = selector_columnas(&h, &mut sub).await;

    // El pie viene del HOST y sale del KEYMAP (#287): no es una cadena que
    // nombre teclas y pueda quedarse rancia cuando alguien las reata.
    let pie = antes.hint.clone();
    assert!(!pie.is_empty(), "el pie llega pintado: {pie:?}");
    assert!(
        pie.contains("activa") && pie.contains("aplica"),
        "y dice qué hace cada acorde: {pie:?}"
    );
    // El cursor abre sobre el NOMBRE, que es fijo: espacio ahí no hace nada,
    // y eso es el contrato —la primera columna ES el nombre por contrato del
    // render— no un fallo.
    assert_eq!(antes.cursor, 0);
    assert!(antes.rows[0].fixed);
    h.dispatch(tecla(" ")).await.expect("host vivo");
    let quieta = siguiente_columnas(&h, &mut sub).await;
    assert!(
        quieta.rows[0].enabled,
        "el nombre no se puede apagar: {:?}",
        quieta.rows[0]
    );

    // Una fila que SÍ se puede tocar: espacio la apaga y la enciende.
    h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    let sobre_otra = siguiente_columnas(&h, &mut sub).await;
    let fila = usize::try_from(sobre_otra.cursor).expect("cabe");
    assert!(fila > 0 && !sobre_otra.rows[fila].fixed);
    let encendida = sobre_otra.rows[fila].enabled;
    h.dispatch(tecla(" ")).await.expect("host vivo");
    let despues = siguiente_columnas(&h, &mut sub).await;
    assert_ne!(
        despues.rows[fila].enabled, encendida,
        "espacio activa y desactiva"
    );

    // SHIFT+FLECHA mueve la FILA, no el cursor.
    assert!(pie.contains("Shift+"), "{pie:?}");
    let orden_antes: Vec<String> = despues.rows.iter().map(|r| r.id.clone()).collect();
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "ArrowDown".to_owned(),
        ctrl: false,
        alt: false,
        shift: true,
        meta: false,
    }))
    .await
    .expect("host vivo");
    let movido = siguiente_columnas(&h, &mut sub).await;
    let orden_despues: Vec<String> = movido.rows.iter().map(|r| r.id.clone()).collect();
    assert_ne!(
        orden_antes, orden_despues,
        "shift+↓ mueve la fila: {orden_antes:?} → {orden_despues:?}"
    );

    // F cicla el formato de la fila del cursor, si lo admite. Se recorre
    // como lo haría una persona —bajando y mirando dónde está— en vez de
    // apuntar a un índice calculado sobre una lista que el paso anterior
    // acaba de reordenar. Con tope: un bucle sobre una condición que puede
    // no llegar es un test que se CUELGA en vez de fallar, y uno colgado no
    // dice nada.
    assert!(
        pie.to_lowercase().contains('f'),
        "y el acorde de formato: {pie:?}"
    );
    let mut ciclado = false;
    for _ in 0..movido.rows.len() + 2 {
        let v = siguiente_columnas(&h, &mut sub).await;
        let aqui = usize::try_from(v.cursor).expect("cabe");
        let Some(fila) = v.rows.get(aqui) else { break };
        if !fila.format.is_empty() && !fila.format_locked {
            let antes = fila.format.clone();
            h.dispatch(tecla("f")).await.expect("host vivo");
            let luego = siguiente_columnas(&h, &mut sub).await;
            assert_ne!(
                luego.rows[aqui].format, antes,
                "F cicla el formato de la fila del cursor"
            );
            ciclado = true;
            break;
        }
        h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    }
    assert!(ciclado, "alguna columna admite formato y se pudo ciclar");
}

/// La vista del selector tras la última tecla.
async fn siguiente_columnas(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::dto::ColumnsPickerView {
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if let Some(c) = siguiente_foto(sub).await.columns.clone() {
            return c;
        }
    }
    panic!("el selector sigue abierto");
}

// ---------------------------------------------------------------------------
// La preview de un plugin en el visor (tarea 4.3).
// ---------------------------------------------------------------------------

/// Una preview con estilo, como la devolvería un previewer.
fn preview_de(
    plugin: &str,
    lineas: &[&str],
    lossy: bool,
) -> norte_proto::methods::PluginPreviewStyled {
    norte_proto::methods::PluginPreviewStyled {
        plugin_id: "acme.pdf".to_owned(),
        plugin_name: plugin.to_owned(),
        lines: lineas
            .iter()
            .map(|l| {
                vec![norte_proto::methods::SpanWire {
                    text: (*l).to_owned(),
                    role: Some("info".to_owned()),
                    fg: None,
                    bg: None,
                }]
            })
            .collect(),
        lossy,
    }
}

/// Cuando un previewer aplica, el visor enseña LO SUYO y dice de quién es.
///
/// Un plugin puede enseñar cualquier cosa —ese es su trabajo: un PDF como
/// texto, un JSON formateado— así que quien mira tiene derecho a saber que no
/// está viendo los bytes del fichero.
#[tokio::test]
async fn el_visor_ensena_la_preview_de_un_plugin_y_dice_de_quien_es() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"informe.pdf".to_vec(), false)]);
    f.contenido.insert(
        "mem:///casa/informe.pdf".to_owned(),
        b"%PDF-1.7 crudo".to_vec(),
    );
    f.previews.insert(
        "mem:///casa/informe.pdf".to_owned(),
        preview_de("PDF de ACME", &["Informe anual", "Página 1 de 12"], true),
    );
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    let mut visor = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if let Some(v) = siguiente_foto(&mut sub).await.viewer.clone() {
            visor = Some(v);
            break;
        }
    }
    let v = visor.expect("el visor abre");

    assert!(
        v.lines.iter().any(|l| l.contains("Informe anual")),
        "enseña lo del previewer: {:?}",
        v.lines
    );
    assert!(
        !v.lines.iter().any(|l| l.contains("%PDF")),
        "y NO los bytes crudos: enseñar las dos cosas sería el mismo fichero \
         dos veces — {:?}",
        v.lines
    );
    assert!(
        v.preview_by.contains("PDF de ACME"),
        "y dice de quién es lo que enseña: {:?}",
        v.preview_by
    );
    assert!(
        !v.preview_by.starts_with("viewer-plugin"),
        "traducido, no la clave: {:?}",
        v.preview_by
    );
    assert!(
        v.preview_lossy,
        "y que la decodificación que se le dio fue con pérdida: los `?` de su \
         salida vienen de ahí y no del fichero"
    );
}

/// La preview de un plugin llega a la ventana CON sus fragmentos (puente
/// 49): rol del tema en kebab, color propio en `#rrggbb`, texto ya
/// enmascarado. Hasta aquí `ViewerView` la aplanaba a `lines`, y la ventana
/// pintaba en gris lo que la TUI pintaba en color.
#[tokio::test]
async fn el_visor_lleva_los_fragmentos_de_la_preview() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"main.rs".to_vec(), false)]);
    f.contenido
        .insert("mem:///casa/main.rs".to_owned(), b"fn main() {}".to_vec());
    f.previews.insert(
        "mem:///casa/main.rs".to_owned(),
        norte_proto::methods::PluginPreviewStyled {
            plugin_id: "acme.syntax".to_owned(),
            plugin_name: "Syntax".to_owned(),
            lines: vec![
                vec![
                    norte_proto::methods::SpanWire {
                        text: "fn".to_owned(),
                        role: Some("title".to_owned()),
                        fg: Some([255, 0, 0]),
                        bg: None,
                    },
                    norte_proto::methods::SpanWire {
                        text: " main".to_owned(),
                        role: None,
                        fg: Some([0, 128, 255]),
                        bg: Some([0, 0, 64]),
                    },
                    norte_proto::methods::SpanWire {
                        // Un rol que el tema no conoce degrada a plano, y un
                        // override bidi del plugin llega enmascarado.
                        text: "()\u{202e}{}".to_owned(),
                        role: Some("no-es-un-rol".to_owned()),
                        fg: None,
                        bg: None,
                    },
                ],
                vec![norte_proto::methods::SpanWire {
                    text: "plano".to_owned(),
                    role: None,
                    fg: None,
                    bg: None,
                }],
            ],
            lossy: false,
        },
    );
    let f = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&f)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    let mut visor = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if let Some(v) = siguiente_foto(&mut sub).await.viewer.clone() {
            visor = Some(v);
            break;
        }
    }
    let v = visor.expect("el visor abre");

    // El ancho del visor viaja con la petición (0.66.0): es el viewport
    // con el que arrancó el host, no un `None` que deja elegir al guest.
    assert_eq!(
        f.anchos_de_preview.lock().expect("mutex").as_slice(),
        &[Some(120)],
        "una petición, con el ancho del viewport"
    );

    assert_eq!(v.styled.len(), 2, "una entrada por fila: {:?}", v.styled);
    assert_eq!(
        v.styled.len(),
        v.lines.len(),
        "las mismas filas que `lines`"
    );
    let primera = &v.styled[0];
    assert_eq!(primera.len(), 3);
    assert_eq!(primera[0].text, "fn");
    assert_eq!(primera[0].role.as_deref(), Some("title"));
    assert_eq!(primera[0].fg.as_deref(), Some("#ff0000"));
    assert_eq!(primera[1].role, None);
    assert_eq!(primera[1].fg.as_deref(), Some("#0080ff"));
    assert_eq!(
        primera[1].bg.as_deref(),
        Some("#000040"),
        "el fondo cruza (puente 50)"
    );
    assert_eq!(primera[0].bg, None);
    assert_eq!(primera[2].role, None, "un rol desconocido degrada a plano");
    assert!(
        !primera[2].text.contains('\u{202e}'),
        "el override bidi no cruza crudo: {:?}",
        primera[2].text
    );
    assert_eq!(v.styled[1][0].text, "plano");
    assert_eq!(
        v.lines[0],
        primera.iter().map(|s| s.text.as_str()).collect::<String>(),
        "`lines` es el mismo texto, aplanado"
    );
}

/// Lo que el renderer MIDIÓ del cuerpo del visor (puente 53) manda sobre el
/// viewport la próxima vez que se abre: el viewport cuenta el cromo, y una
/// imagen encogida a él se salía por la derecha.
#[tokio::test]
async fn el_visor_pide_el_ancho_que_midio_el_renderer() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"main.rs".to_vec(), false)]);
    f.contenido
        .insert("mem:///casa/main.rs".to_owned(), b"fn main() {}".to_vec());
    let f = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&f)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F3")).await.expect("host vivo");
    assert_eq!(
        anchos_de_preview_tras(&h, &mut sub, &f, 1).await.as_slice(),
        &[Some(120)]
    );

    h.dispatch(UiAction::SetViewerCols { cols: 77 })
        .await
        .expect("host vivo");
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    h.dispatch(tecla("F3")).await.expect("host vivo");
    assert_eq!(
        anchos_de_preview_tras(&h, &mut sub, &f, 2).await.as_slice(),
        &[Some(120), Some(77)],
        "la segunda apertura pide el ancho medido"
    );
}

/// Pide fotos hasta que el backend falso haya visto `n` peticiones de
/// preview con estilo, y devuelve los anchos que llevaban.
async fn anchos_de_preview_tras(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    f: &Falso,
    n: usize,
) -> Vec<Option<u32>> {
    let mut anchos = Vec::new();
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let _ = siguiente_foto(sub).await;
        anchos.clone_from(&f.anchos_de_preview.lock().expect("mutex"));
        if anchos.len() >= n {
            break;
        }
    }
    anchos
}

/// Un previewer que no aplica NO estorba: el visor enseña el fichero.
#[tokio::test]
async fn sin_previewer_el_visor_ensena_el_fichero() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
    f.contenido.insert(
        "mem:///casa/notas.txt".to_owned(),
        b"hola\nmundo\n".to_vec(),
    );
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if let Some(v) = siguiente_foto(&mut sub).await.viewer.clone() {
            assert!(v.lines.iter().any(|l| l.contains("hola")));
            assert!(
                v.preview_by.is_empty(),
                "sin plugin no se atribuye a nadie: {:?}",
                v.preview_by
            );
            return;
        }
    }
    panic!("el visor abre igual sin previewer");
}

/// El nombre de un previewer es texto de TERCERO y llega enmascarado.
#[tokio::test]
async fn el_nombre_del_previewer_llega_enmascarado() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"x.bin".to_vec(), false)]);
    f.contenido
        .insert("mem:///casa/x.bin".to_owned(), b"\x00\x01".to_vec());
    f.previews.insert(
        "mem:///casa/x.bin".to_owned(),
        preview_de("ACME\u{202e}gpj", &["contenido"], false),
    );
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if let Some(v) = siguiente_foto(&mut sub).await.viewer.clone() {
            assert!(
                !v.preview_by.contains('\u{202e}'),
                "el nombre del plugin va crudo: {:?}",
                v.preview_by
            );
            return;
        }
    }
    panic!("el visor abre");
}

// ---------------------------------------------------------------------------
// La imagen del visor (tarea 4.3, ADR 0069).
// ---------------------------------------------------------------------------

/// Un PNG cuya CABECERA declara `w`x`h`, con relleno hasta `bytes`.
fn png_de(w: u32, h: u32, bytes: usize) -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend_from_slice(&[0, 0, 0, 13]);
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&w.to_be_bytes());
    v.extend_from_slice(&h.to_be_bytes());
    v.resize(bytes.max(v.len()), 0);
    v
}

/// El visor abre la imagen: dice su formato y su tamaño DECLARADO, y sus
/// bytes NO viajan en la foto.
#[tokio::test]
async fn una_imagen_se_acepta_y_sus_bytes_van_aparte() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"foto.png".to_vec(), false)]);
    f.contenido
        .insert("mem:///casa/foto.png".to_owned(), png_de(1920, 1080, 4096));
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    let mut v = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if let Some(x) = siguiente_foto(&mut sub).await.viewer.clone() {
            v = Some(x);
            break;
        }
    }
    let v = v.expect("el visor abre");
    let img = v.image.clone().expect("se reconoce la imagen");
    assert_eq!(img.format, "PNG", "por bytes MÁGICOS, no por la extensión");
    assert_eq!((img.width, img.height), (1920, 1080));
    assert!(v.image_refused.is_empty());

    // Los bytes NO están en la foto: ocho megas en el flujo de parches es un
    // mensaje que se reenvía entero en cada `Resync`.
    let foto = serde_json::to_string(&v).expect("serializa");
    assert!(
        foto.len() < 4096,
        "la vista del visor pesa {} bytes: los de la imagen se han colado",
        foto.len()
    );
    // Y se sirven por su propio camino.
    let bytes = h
        .image_bytes()
        .await
        .expect("host vivo")
        .expect("hay bytes");
    assert_eq!(bytes.len(), 4096);
}

/// Una cabecera que declara una BOMBA se rechaza, y se dice.
///
/// Un PNG de cuatro kilobytes puede declarar 60000×60000 —36 gigapíxeles— y
/// costarle gigabytes al decodificador. La cabecera se lee y se niega ANTES
/// de que nadie decodifique, que es la única defensa barata (ADR 0069).
#[tokio::test]
async fn una_cabecera_que_declara_una_bomba_se_rechaza() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"bomba.png".to_vec(), false)]);
    f.contenido.insert(
        "mem:///casa/bomba.png".to_owned(),
        png_de(60000, 60000, 4096),
    );
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let Some(v) = siguiente_foto(&mut sub).await.viewer.clone() else {
            continue;
        };
        assert!(v.image.is_none(), "no se pinta");
        assert!(
            !v.image_refused.is_empty(),
            "y se DICE: caer al hexview en silencio parece norte roto, no \
             norte prudente"
        );
        assert!(
            !v.image_refused.starts_with("viewer-image"),
            "traducido, no la clave: {:?}",
            v.image_refused
        );
        assert!(
            h.image_bytes().await.expect("host vivo").is_none(),
            "y sus bytes no se sirven a nadie"
        );
        return;
    }
    panic!("el visor abre igual");
}

/// Una cabecera que no se entiende también se rechaza.
///
/// «No sé» tratado como «adelante» es la puerta que el presupuesto existe
/// para cerrar.
#[tokio::test]
async fn una_cabecera_que_no_se_entiende_se_rechaza() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"raro.png".to_vec(), false)]);
    // Firma PNG válida, pero el primer chunk NO es IHDR.
    let mut roto = b"\x89PNG\r\n\x1a\n".to_vec();
    roto.extend_from_slice(&[0, 0, 0, 13]);
    roto.extend_from_slice(b"iTXt");
    roto.resize(64, 0);
    f.contenido.insert("mem:///casa/raro.png".to_owned(), roto);
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let Some(v) = siguiente_foto(&mut sub).await.viewer.clone() else {
            continue;
        };
        assert!(v.image.is_none());
        assert!(!v.image_refused.is_empty(), "se dice que no se entiende");
        return;
    }
    panic!("el visor abre igual");
}

/// Cerrar el visor SUELTA los bytes: son megas.
#[tokio::test]
async fn cerrar_el_visor_suelta_la_imagen() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"foto.png".to_vec(), false)]);
    f.contenido
        .insert("mem:///casa/foto.png".to_owned(), png_de(64, 64, 2048));
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if siguiente_foto(&mut sub).await.viewer.is_some() {
            break;
        }
    }
    assert!(h.image_bytes().await.expect("host vivo").is_some());

    h.dispatch(tecla("Escape")).await.expect("host vivo");
    assert!(
        h.image_bytes().await.expect("host vivo").is_none(),
        "un visor cerrado no retiene megas de imagen"
    );
}

// ---------------------------------------------------------------------------
// Copiar y mover (tarea 5.1 de la fase 5).
//
// El renderer JAMÁS nombra un fichero: manda `pane.copy` y el host deriva el
// origen de las marcas del hueco activo y el destino del hueco con el rol
// `Target`. Ni una ruta cruza desde la webview.
// ---------------------------------------------------------------------------

/// El listado de UN hueco concreto de una foto.
fn listado_de(
    snap: &norte_ui_host::ViewSnapshot,
    slot_id: u32,
) -> &norte_ui_host::dto::BrowserSlotView {
    snap.slots
        .iter()
        .find_map(|s| match s {
            SlotView::Browser(b) if b.slot_id == slot_id => Some(b),
            _ => None,
        })
        .unwrap_or_else(|| panic!("el hueco {slot_id} es un listado"))
}

/// Dos paneles, con el DESTINO ya en otro directorio: el escenario real de
/// una copia. Devuelve la foto de después.
async fn dos_paneles_con_destino_aparte(
    backend: Arc<Falso>,
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    let (h, snap) = host_con_layout(backend, "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    let b2 = listado_de(&snap, 2);
    let docs = b2
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("el directorio está");
    let (key, generation) = (docs.key, b2.generation);
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host vivo");
    h.dispatch(UiAction::Activate {
        slot_id: 2,
        key,
        generation,
    })
    .await
    .expect("host vivo");
    // La foto del aterrizaje: sin esperarla, F5 vería el destino todavía en
    // el directorio de partida y el test probaría otra cosa.
    //
    // Se PIDE (`esperar_foto` manda `Resync`) en vez de quedarse escuchando:
    // con un listado grande el aterrizaje viaja en PARCHES y la foto que lo
    // contaría puede haber pasado ya, así que un `siguiente_foto` en bucle se
    // quedaba esperando una que no vuelve a salir — colgado, no rojo.
    let despues = esperar_foto(&h, &mut sub, "el destino aterriza en /casa/docs", |f| {
        listado_de(f, 2).path_display.ends_with("/casa/docs")
    })
    .await;
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host vivo");
    // El cursor del ORIGEN, sobre un fichero que no es el directorio destino:
    // con el cursor en `docs` el origen y el destino se escriben igual, y una
    // aserción sobre el texto del diálogo no distinguiría cuál de los dos
    // está mirando.
    let b1 = listado_de(&despues, 1);
    let notas = b1
        .rows
        .iter()
        .find(|r| r.display_name == "notas.txt")
        .expect("el fichero está");
    let (key, generation) = (notas.key, b1.generation);
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host vivo");
    (h, despues)
}

/// F5 no copia: abre la confirmación, y esa confirmación DICE a dónde va.
///
/// En una ventana con dos listados el destino no es evidente —no hay «el
/// otro panel» cuando hay tres—, así que el diálogo es el único sitio donde
/// se puede leer antes de aceptar.
#[tokio::test]
async fn copiar_pide_confirmacion_y_dice_a_donde() {
    let backend = arbol();
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F5")).await.expect("host vivo");
    let dialogos = siguientes_dialogos(&mut sub).await;
    assert_eq!(dialogos.len(), 1, "se abre UN diálogo");
    assert_eq!(dialogos[0].title_key, "modal-copy-title");
    let cuerpo = &dialogos[0].body;
    assert!(
        cuerpo.iter().any(|l| l.text.ends_with("/casa/notas.txt")),
        "el cuerpo es lo que se transfiere: {cuerpo:?}"
    );
    let destino = dialogos[0]
        .destination
        .as_ref()
        .expect("una transferencia dice a dónde va");
    assert!(
        destino.text.ends_with("/casa/docs"),
        "y el destino va en SU campo: {destino:?}"
    );
    assert!(
        cuerpo.iter().all(|l| !l.text.contains("/casa/docs")),
        "no repetido entre las líneas del cuerpo: {cuerpo:?}"
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

/// **Partir lee el tamaño en BINARIO** (#132, #290): `10M` son 10 MiB, que es
/// lo que significa en un gestor de ficheros, y no diez millones.
#[tokio::test]
async fn partir_lee_el_tamano_en_binario() {
    let backend = Arc::new(arbol_como_falso());
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separar_los_paneles(&h, &mut sub).await;
    // El cursor, sobre un FICHERO: arranca en `docs/`, que además es el
    // directorio destino, y ahí la comprobación de abajo no distinguiría nada.
    h.dispatch(tecla("Down")).await.expect("host vivo");

    ejecutar_por_paleta(&h, &mut sub, "pane.split-file").await;
    let id = siguientes_dialogos(&mut sub).await.last().expect("hay").id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "10M".to_owned(),
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
    let ps = anotados(&backend, "el troceado encolado", 1, |f| {
        f.partidos.lock().expect("partidos").clone()
    })
    .await;
    assert_eq!(ps.len(), 1);
    assert_eq!(ps[0].part_bytes, 10 * 1024 * 1024, "MiB, no millones");
    // Cuál sea la entrada bajo el cursor da igual —el listado ordena y el
    // corpus mete un nombre hostil por medio—; lo que este test fija es de
    // QUÉ panel sale cada cosa.
    assert_eq!(
        ps[0].path.parent().map(|p| p.to_wire()).as_deref(),
        Some("mem:///casa"),
        "el fichero sale del panel ACTIVO"
    );
    assert_eq!(
        ps[0].dest_dir.to_wire(),
        "mem:///casa/docs",
        "y los trozos van al panel DESTINO: partir uno enorme donde ya está \
         suele no caber"
    );
}

/// Un tamaño que no se entiende se rehúsa y no parte nada. El cero entra ahí:
/// trozos de cero bytes no terminan nunca.
#[tokio::test]
async fn partir_rehusa_un_tamano_que_no_vale() {
    let backend = Arc::new(arbol_como_falso());
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separar_los_paneles(&h, &mut sub).await;

    ejecutar_por_paleta(&h, &mut sub, "pane.split-file").await;
    let id = siguientes_dialogos(&mut sub).await.last().expect("hay").id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "0".to_owned(),
    })
    .await
    .expect("host vivo");
    let ack = h
        .dispatch(UiAction::Dialog {
            id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Unavailable { ref reason_key } if reason_key == "msg-split-bad-size"),
        "{ack:?}"
    );
    asentar().await;
    assert!(backend.partidos.lock().expect("partidos").is_empty());
}

/// **Juntar solo desde el PRIMER trozo** (#132, #290): empezar por el `.007`
/// uniría media cosa, y el core solo busca hacia delante.
#[tokio::test]
async fn juntar_exige_empezar_por_el_primer_trozo() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![
            (b"pelicula.mkv.001".to_vec(), false),
            (b"pelicula.mkv.007".to_vec(), false),
        ],
    );
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // El cursor arranca en la primera fila: el `.001`.
    ejecutar_por_paleta(&h, &mut sub, "pane.combine-files").await;
    {
        let js = anotados(&backend, "la unión encolada", 1, |f| {
            f.juntados.lock().expect("juntados").clone()
        })
        .await;
        assert_eq!(js.len(), 1, "desde el .001 sí");
        assert_eq!(
            js[0].dest.to_wire(),
            "mem:///casa/pelicula.mkv",
            "el destino es el nombre SIN el sufijo de trozo"
        );
    }

    // Bajar al `.007` y volver a pedirlo: ahí no.
    h.dispatch(tecla("Down")).await.expect("host vivo");
    let ack = ejecutar_por_paleta_ack(&h, &mut sub, "pane.combine-files").await;
    assert!(
        matches!(ack, ActionAck::Unavailable { ref reason_key } if reason_key == "msg-combine-needs-first"),
        "desde otro trozo no: {ack:?}"
    );
    assert_eq!(
        backend.juntados.lock().expect("juntados").len(),
        1,
        "y no se pide nada nuevo"
    );
}

/// **Empaquetar saca el FORMATO del nombre tecleado** (#132, #290), y la base
/// es el directorio del panel: quien desempaquete espera ver lo que se veía en
/// pantalla, no rutas absolutas.
#[tokio::test]
async fn empaquetar_saca_el_formato_del_nombre() {
    let backend = Arc::new(arbol_como_falso());
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.pack").await;
    let id = siguientes_dialogos(&mut sub).await.last().expect("hay").id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "cosas.tar.gz".to_owned(),
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
    let ps = anotados(&backend, "el empaquetado encolado", 1, |f| {
        f.empaquetados.lock().expect("empaquetados").clone()
    })
    .await;
    assert_eq!(ps.len(), 1, "un gesto, una task");
    assert_eq!(
        ps[0].format,
        norte_proto::methods::ArchiveFormat::TarGz,
        "`.tar.gz` no es `.tar`: el sufijo compuesto se mira ANTES"
    );
    assert_eq!(ps[0].dest.to_wire(), "mem:///casa/cosas.tar.gz");
    assert_eq!(
        ps[0].base.to_wire(),
        "mem:///casa",
        "la base es el directorio del panel"
    );
}

/// Un nombre cuyo formato NO se sabe escribir se rehúsa, en vez de empaquetar
/// en otra cosa. `.rar` es el caso real: se lee por delegación y no se escribe.
#[tokio::test]
async fn empaquetar_rehusa_un_formato_que_no_se_escribe() {
    let backend = Arc::new(arbol_como_falso());
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.pack").await;
    let id = siguientes_dialogos(&mut sub).await.last().expect("hay").id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "cosas.rar".to_owned(),
    })
    .await
    .expect("host vivo");
    let ack = h
        .dispatch(UiAction::Dialog {
            id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Unavailable { ref reason_key } if reason_key == "msg-pack-unknown-format"),
        "{ack:?}"
    );
    asentar().await;
    assert!(
        backend
            .empaquetados
            .lock()
            .expect("empaquetados")
            .is_empty(),
        "y no se empaqueta nada"
    );
}

/// Comprobar solo vale sobre un CONTENEDOR, y lo decide la misma función que
/// usa `Enter` para entrar en uno: dos tablas de extensiones serían dos sitios
/// donde una se olvida.
#[tokio::test]
async fn comprobar_un_archivo_exige_que_lo_sea() {
    let backend = Arc::new(arbol_como_falso());
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // El cursor arranca sobre `docs/`, que es un directorio.
    let ack = ejecutar_por_paleta_ack(&h, &mut sub, "pane.test-archive").await;
    assert!(
        matches!(ack, ActionAck::Unavailable { ref reason_key } if reason_key == "msg-unpack-not-archive"),
        "un directorio no es un contenedor: {ack:?}"
    );
    assert!(
        backend.comprobados.lock().expect("comprobados").is_empty(),
        "y no se pide comprobar nada"
    );
}

/// **Una transferencia que CHOCA tiene salida** (#274).
///
/// La ventana manda siempre `CollisionPolicy::Fail`, que es el default seguro,
/// pero no tenía dónde tomar la decisión: quedaba una task fallida en el
/// tablero y ningún camino hacia delante, mientras el TUI sí ofrece las
/// cuatro. Se comprueba lo que de verdad importa: que la segunda transferencia
/// SALE, con la política elegida y el mismo verbo.
#[tokio::test]
async fn una_copia_que_choca_se_puede_reintentar_con_otra_politica() {
    let backend = arbol();
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F5")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    let _ = siguientes_tasks(&mut sub).await;

    // El daemon dice que el destino ya existe.
    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("hay task");
    tx.send_modify(|p| {
        p.state = norte_proto::TaskState::Failed {
            error: norte_proto::Error::Conflict {
                conflict: norte_proto::ConflictKind::Exists,
            },
        };
    });

    // Y ahora SÍ hay una pregunta que contestar.
    let dialogos = siguientes_dialogos(&mut sub).await;
    let colision = dialogos.last().expect("el diálogo de colisión");
    let opciones: Vec<&str> = colision.choices.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        opciones,
        vec!["overwrite", "newer", "rename", "skip", "cancel"],
        "las cuatro salidas del TUI, más cancelar"
    );
    assert!(
        colision
            .choices
            .iter()
            .any(|c| c.id == "overwrite" && c.destructive),
        "sobrescribir se marca como destructivo: destruye lo que hay"
    );

    // Se abrió SOLO, así que la primera respuesta solo lo reconoce.
    let cid = colision.id;
    h.dispatch(UiAction::Dialog {
        id: cid,
        choice: "overwrite".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Dialog {
        id: cid,
        choice: "overwrite".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    let ts = anotados(&backend, "la original y el reintento", 2, |f| {
        f.transferencias.lock().expect("transferencias").clone()
    })
    .await;
    assert_eq!(ts.len(), 2, "la original y el reintento: {ts:?}");
    let (from, to, mover, colision) = &ts[1];
    assert_eq!(
        *colision,
        norte_proto::CollisionPolicy::Overwrite,
        "el reintento va con la política que se eligió"
    );
    assert!(
        !mover,
        "y con el MISMO verbo: un reintento de copia no mueve"
    );
    assert_eq!(from.to_wire(), ts[0].0.to_wire(), "mismo origen");
    assert_eq!(to.to_wire(), ts[0].1.to_wire(), "y mismo destino");
}

/// Cancelar la colisión no relanza nada: no elegir es una respuesta, y la task
/// fallida se queda como estaba.
#[tokio::test]
async fn cancelar_una_colision_no_reintenta() {
    let backend = arbol();
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F5")).await.expect("host vivo");
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
        p.state = norte_proto::TaskState::Failed {
            error: norte_proto::Error::Conflict {
                conflict: norte_proto::ConflictKind::Exists,
            },
        };
    });
    let cid = siguientes_dialogos(&mut sub).await.last().expect("hay").id;

    // Cancelar está EXENTO del reconocimiento: quitarse de encima algo que uno
    // no ha pedido sale a la primera.
    h.dispatch(UiAction::Dialog {
        id: cid,
        choice: "cancel".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    anotados(&backend, "la transferencia original", 1, |f| {
        f.transferencias.lock().expect("transferencias").clone()
    })
    .await;
    asentar().await;
    assert_eq!(
        backend.transferencias.lock().expect("transferencias").len(),
        1,
        "cancelar no relanza"
    );
}

/// Confirmada, la copia sale con el destino COMPUESTO en Rust: el directorio
/// del hueco destino más el nombre de la entrada, byte a byte.
#[tokio::test]
async fn copiar_compone_el_destino_en_rust() {
    let backend = arbol();
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F5")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    let ts = anotados(&backend, "la transferencia encolada", 1, |f| {
        f.transferencias.lock().expect("transferencias").clone()
    })
    .await;
    assert_eq!(ts.len(), 1, "una entrada bajo el cursor, una task");
    let (from, to, mover, colision) = &ts[0];
    assert_eq!(
        *colision,
        norte_proto::CollisionPolicy::Fail,
        "el default SEGURO del wire: un destino ocupado falla, no se pisa"
    );
    assert!(!mover, "F5 copia");
    assert_eq!(from.to_wire(), "mem:///casa/notas.txt");
    assert_eq!(
        to.to_wire(),
        "mem:///casa/docs/notas.txt",
        "el destino es el DIRECTORIO del otro hueco más el nombre del origen"
    );
}

/// F6 usa el mismo camino, pero es otro verbo: en el wire son dos métodos,
/// en el tablero dos clases de task y en el journal dos entradas.
#[tokio::test]
async fn mover_es_otro_verbo_y_lo_dice() {
    let backend = arbol();
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F6")).await.expect("host vivo");
    let dialogos = siguientes_dialogos(&mut sub).await;
    assert_eq!(dialogos[0].title_key, "modal-move-title");
    let id = dialogos[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    let ts = anotados(&backend, "el movimiento encolado", 1, |f| {
        f.transferencias.lock().expect("transferencias").clone()
    })
    .await;
    assert!(ts[0].2, "F6 mueve");
}

/// Con un solo listado no hay a dónde copiar **dentro de la ventana**, así que
/// se pregunta fuera (#284) — y hasta que llegue la respuesta no se transfiere
/// nada. Lo que este test sostiene es que no se INVENTA un destino: ni el
/// propio directorio, ni el último que se usó.
///
/// Antes de #284 esto se rehusaba con `host-no-other-slot`. La cadena completa
/// —efecto, respuesta y confirmación— la cubre
/// `con_un_panel_el_destino_lo_elige_el_escritorio`.
#[tokio::test]
async fn sin_otro_hueco_el_destino_se_pregunta_fuera() {
    let backend = arbol();
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "simple", (120, 40)).await;
    let mut nativos = h.native_effects();
    let ack = h.dispatch(tecla("F5")).await.expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "se acepta el gesto y se pregunta: {ack:?}"
    );
    let efecto = tokio::time::timeout(std::time::Duration::from_secs(2), nativos.recv())
        .await
        .expect("sale el selector")
        .expect("canal vivo");
    assert!(matches!(
        efecto,
        norte_ui_host::dto::NativeEffect::PickDirectory { .. }
    ));
    assert!(
        backend
            .transferencias
            .lock()
            .expect("transferencias")
            .is_empty(),
        "y nada se mueve hasta que haya destino"
    );
}

/// Los dos listados en el MISMO directorio: copiar ahí es copiar encima de
/// uno mismo, y no se abre ningún diálogo que lo sugiera.
#[tokio::test]
async fn copiar_sobre_el_propio_directorio_se_rechaza() {
    let backend = arbol();
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (120, 40)).await;
    let ack = h.dispatch(tecla("F5")).await.expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Unavailable { .. }),
        "el destino es el directorio de origen: {ack:?}"
    );
    assert!(
        backend
            .transferencias
            .lock()
            .expect("transferencias")
            .is_empty()
    );
}

/// En solo lectura, F5 no abre nada: que la tecla exista en el preset no es
/// permiso.
#[tokio::test]
async fn en_solo_lectura_copiar_no_abre_nada() {
    let backend = arbol();
    let (h, _snap) = host_solo_lectura(Arc::clone(&backend)).await;
    let ack = h.dispatch(tecla("F5")).await.expect("host vivo");
    assert!(matches!(ack, ActionAck::Unavailable { .. }), "{ack:?}");
    asentar().await;
    assert!(
        backend
            .transferencias
            .lock()
            .expect("transferencias")
            .is_empty()
    );
}

/// Las marcas las CONSUME el envío, como en el TUI: una selección a medio
/// consumir significaría cosas distintas según qué task terminó.
#[tokio::test]
async fn las_marcas_se_consumen_al_enviar() {
    let backend = arbol();
    let (h, snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let b1 = listado_de(&snap, 1);
    let (generation, claves): (u64, Vec<_>) = (
        b1.generation,
        b1.rows
            .iter()
            .filter(|r| r.display_name != "docs")
            .map(|r| r.key)
            .collect(),
    );
    let mut sub = h.subscribe();
    for key in claves.iter().take(2) {
        h.dispatch(UiAction::ToggleMark {
            slot_id: 1,
            key: *key,
            generation,
        })
        .await
        .expect("host vivo");
    }
    h.dispatch(tecla("F5")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    anotados(&backend, "las dos tasks de las dos marcas", 2, |f| {
        f.transferencias.lock().expect("transferencias").clone()
    })
    .await;
    assert_eq!(
        backend.transferencias.lock().expect("transferencias").len(),
        2,
        "dos marcas, dos tasks"
    );
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(
        listado_de(&foto, 1).marks,
        0,
        "las marcas las consumió el envío"
    );
}

/// Al terminar la copia, el hueco DESTINO se vuelve a listar: la entrada
/// nueva está ahí y una pantalla que no la enseña miente.
#[tokio::test]
async fn al_terminar_una_copia_se_relista_el_destino() {
    let backend = arbol();
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F5")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    siguientes_tasks(&mut sub).await;
    let antes = backend.listados();

    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("hay task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    hasta(&backend, "el relistado del destino tras la copia", |f| {
        (f.listados() > antes).then_some(())
    })
    .await;
}

/// Una colisión no es una excepción del host: es el desenlace TIPADO de la
/// task, y llega al tablero como tal.
#[tokio::test]
async fn una_colision_llega_al_tablero_como_fallo() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)],
    );
    f.pon("mem:///casa/docs", vec![(b"notas.txt".to_vec(), false)]);
    f.estado_transferencia = Some(norte_proto::TaskState::Failed {
        error: norte_proto::Error::Conflict {
            conflict: norte_proto::ConflictKind::Exists,
        },
    });
    let backend = Arc::new(f);
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F5")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    let tasks = siguientes_tasks(&mut sub).await;
    assert_eq!(
        tasks[0].state,
        norte_ui_host::dto::TaskStateView::Failed,
        "el destino ya existía, y el tablero lo dice"
    );
}

/// Una task que NACE terminal —el daemon la completó antes de que la llamada
/// volviera— también relista el destino.
///
/// Es la carrera de verdad: el canal de progreso no cambia nunca, así que
/// nadie llega a mirarlo, y sin comprobar el estado AL REGISTRAR la copia
/// quedaba hecha en el disco y ausente en la pantalla para siempre.
#[tokio::test]
async fn una_copia_que_nace_terminal_tambien_relista() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)],
    );
    f.pon("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    f.estado_transferencia = Some(norte_proto::TaskState::Completed);
    let backend = Arc::new(f);
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F5")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    let antes = backend.listados();
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");

    hasta(&backend, "el refresco de una copia ya terminada", |f| {
        (f.listados() > antes).then_some(())
    })
    .await;
}

/// Un destino que no acepta escrituras rechaza al ENCOLAR, antes de que haya
/// task: no hay fila en el tablero que mirar, así que lo dice la barra —con
/// la frase tipada del error, no con un «algo falló»—.
#[tokio::test]
async fn un_destino_de_solo_lectura_lo_dice_al_encolar() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)],
    );
    f.pon("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    f.transferencia_rechazada = Some(norte_proto::Error::Unsupported);
    let backend = Arc::new(f);
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F5")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");

    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        if let Some(m) = &foto.status.message {
            assert!(
                !m.starts_with("err-"),
                "la barra dice el error TRADUCIDO, no su clave: {m}"
            );
            assert!(foto.tasks.is_empty(), "no llegó a haber task");
            return;
        }
    }
    panic!("un rechazo al encolar se perdió en silencio");
}

/// Una transferencia en marcha se cancela por el mismo camino que cualquier
/// otra task: el tablero es uno solo.
#[tokio::test]
async fn una_copia_en_marcha_se_cancela() {
    let backend = arbol();
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F5")).await.expect("host vivo");
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
    let ack = h
        .dispatch(UiAction::CancelTask {
            task_id: tasks[0].task_id,
        })
        .await
        .expect("host vivo");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    assert_eq!(backend.cancelaciones.load(Ordering::SeqCst), 1);
}

/// Un refresco JAMÁS pisa una navegación en vuelo.
///
/// El refresco reserva un testigo nuevo, así que la respuesta de la
/// navegación llegaría con uno viejo y se descartaría: el panel se quedaría
/// en el directorio del que el lector acababa de salir, sin decir nada. Una
/// pantalla un poco vieja es aceptable; la aplicación moviéndose sola, no.
#[tokio::test]
async fn un_refresco_no_pisa_una_navegacion_en_vuelo() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)],
    );
    f.pon("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    f.pon("mem:///casa/docs/hondo", vec![(b"z.md".to_vec(), false)]);
    f.arbol
        .get_mut("mem:///casa/docs")
        .expect("está")
        .push((b"hondo".to_vec(), true));
    // La respuesta del listado TARDA: es lo que abre la ventana en la que el
    // refresco podría colarse.
    f.retraso_ms = 120;
    f.estado_transferencia = Some(norte_proto::TaskState::Completed);
    let backend = Arc::new(f);
    let (h, snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let b2 = listado_de(&snap, 2);
    let hondo = b2
        .rows
        .iter()
        .find(|r| r.display_name == "hondo")
        .expect("el subdirectorio está");
    let (key, generation) = (hondo.key, b2.generation);

    // Una copia hacia `casa/docs`, que termina nada más encolarse.
    h.dispatch(tecla("F5")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    // Y, ANTES de confirmar, el destino se va a otro sitio: la navegación
    // queda volando durante los 120 ms del falso.
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host vivo");
    let antes = backend.listados();
    h.dispatch(UiAction::Activate {
        slot_id: 2,
        key,
        generation,
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host vivo");
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");

    // La navegación llega a su destino y NADIE la devuelve a `casa/docs`. Se
    // espera a que el listado de `hondo` se haya PEDIDO y a que no quede
    // ninguna respuesta volando —incluido el refresco que dispara la copia
    // terminada, que es el que podría pisarla—; solo entonces se mira la
    // pantalla, UNA vez.
    hasta(&backend, "el listado de hondo, ya servido", |f| {
        (f.listados() > antes && f.en_calma()).then_some(())
    })
    .await;
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(
        listado_de(&foto, 2).path_display.ends_with("/docs/hondo"),
        "la navegación sobrevivió al refresco: {}",
        listado_de(&foto, 2).path_display
    );
}

// ---------------------------------------------------------------------------
// Lo que las tres revisiones de la 5.1 encontraron.
// ---------------------------------------------------------------------------

/// Un nombre del corpus, por su id.
fn hostil(id: &str) -> Vec<u8> {
    norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == id)
        .unwrap_or_else(|| panic!("el corpus tiene {id}"))
        .bytes
}

/// Un directorio destino que se llama `a → mem_b.txt` NO puede simular dos
/// rutas en la confirmación.
///
/// La flecha es legítima (U+2192), no es un peligro de terminal y por tanto
/// no se enmascara ni se marca. Con el destino como primera línea del cuerpo
/// y una flecha por etiqueta, quien lee `→ …/a → mem_b.txt` puede entender
/// que sus ficheros van a `mem_b.txt`. Se etiqueta FUERA de banda: el destino
/// tiene su propio campo. Fixture `arrow_join_spoof` del corpus canónico.
#[tokio::test]
async fn un_destino_con_una_flecha_no_simula_dos_rutas() {
    let trampa = hostil("arrow_join_spoof");
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(trampa.clone(), true), (b"notas.txt".to_vec(), false)],
    );
    let vp = norte_proto::VPath::parse("mem:///casa")
        .expect("raíz")
        .join(norte_proto::Segment::new(trampa.clone()).expect("segmento"));
    f.pon(vp.to_wire().as_str(), vec![(b"a.md".to_vec(), false)]);
    let backend = Arc::new(f);
    let (h, snap) = host_con_layout(Arc::clone(&backend), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();

    // El hueco destino entra en el directorio trampa.
    let b2 = listado_de(&snap, 2);
    let fila = b2
        .rows
        .iter()
        .find(|r| r.display_name.contains('→'))
        .expect("la trampa se pinta");
    let (key, generation) = (fila.key, b2.generation);
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host vivo");
    h.dispatch(UiAction::Activate {
        slot_id: 2,
        key,
        generation,
    })
    .await
    .expect("host vivo");
    let mut despues = siguiente_foto(&mut sub).await;
    while listado_de(&despues, 2).path_display == listado_de(&snap, 2).path_display {
        despues = siguiente_foto(&mut sub).await;
    }
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host vivo");
    // El cursor del ORIGEN, sobre el fichero: el directorio trampa también
    // está listado aquí, y lo que se comprueba es el destino.
    let b1 = listado_de(&despues, 1);
    let notas = b1
        .rows
        .iter()
        .find(|r| r.display_name == "notas.txt")
        .expect("el fichero está");
    let (key, generation) = (notas.key, b1.generation);
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host vivo");

    h.dispatch(tecla("F5")).await.expect("host vivo");
    let d = siguientes_dialogos(&mut sub).await[0].clone();
    let destino = d.destination.expect("dice a dónde va");
    assert!(
        destino.text.contains('\u{2192}'),
        "el nombre real lleva la flecha: {destino:?}"
    );
    assert_eq!(d.body.len(), 1, "una entrada, una línea: {:?}", d.body);
    assert!(
        d.body[0].text.ends_with("/casa/notas.txt") && !d.body[0].text.contains('\u{2192}'),
        "el cuerpo es SOLO el origen; el destino no aparece ahí: {:?}",
        d.body
    );
}

/// Un lote más grande de lo que cabe en el diálogo lo DICE.
///
/// Marcar cuarenta, ver dieciséis y confirmar es aprobar otra cosa: esta es
/// la última pantalla donde todavía se puede decir que no.
#[tokio::test]
async fn un_lote_recortado_lo_dice() {
    let mut nombres: Vec<(Vec<u8>, bool)> =
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)];
    nombres.extend((0..40).map(|i| (format!("f{i:03}.txt").into_bytes(), false)));
    let mut f = Falso::default();
    f.pon("mem:///casa", nombres);
    f.pon("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    let backend = Arc::new(f);
    let (h, snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;

    let b1 = listado_de(&snap, 1);
    let generation = b1.generation;
    let claves: Vec<_> = b1
        .rows
        .iter()
        .filter(|r| r.display_name != "docs")
        .map(|r| r.key)
        .collect();
    assert!(
        claves.len() > 16,
        "hay más de lo que cabe: {}",
        claves.len()
    );
    for key in &claves {
        h.dispatch(UiAction::ToggleMark {
            slot_id: 1,
            key: *key,
            generation,
        })
        .await
        .expect("host vivo");
    }
    // Se escucha DESPUÉS de marcar: cuarenta marcas son cuarenta parches, y
    // el ayudante que espera un diálogo mira solo las primeras
    // actualizaciones que le llegan.
    let mut sub = h.subscribe();
    h.dispatch(tecla("F5")).await.expect("host vivo");
    let d = siguientes_dialogos(&mut sub).await[0].clone();
    assert!(
        d.body.len() < claves.len(),
        "el cuerpo está acotado: {}",
        d.body.len()
    );
    assert!(
        !d.overflow_note.is_empty(),
        "y lo DICE, en su propio campo: {d:?}"
    );
    assert!(
        !d.overflow_note.starts_with("dialog-"),
        "traducido, no la clave Fluent: {}",
        d.overflow_note
    );
}

/// El cuerpo de una confirmación DICE qué línea se pinta distinta de lo que
/// es. Es la única superficie donde se aprueba un nombre ajeno.
#[tokio::test]
async fn el_cuerpo_de_una_confirmacion_marca_lo_que_enmascara() {
    for id in ["control_newline", "control_escape", "arrow_join_spoof"] {
        let bytes = hostil(id);
        let altera = norte_frontend::display_name(&bytes).1;
        let mut f = Falso::default();
        f.pon(
            "mem:///casa",
            vec![(b"docs".to_vec(), true), (bytes.clone(), false)],
        );
        f.pon("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
        let backend = Arc::new(f);
        let (h, snap) = host_con_layout(Arc::clone(&backend), "orthodox", (120, 40)).await;
        let mut sub = h.subscribe();
        // El cursor, sobre la entrada hostil.
        let b1 = listado_de(&snap, 1);
        let fila = b1
            .rows
            .iter()
            .find(|r| r.display_name != "docs")
            .expect("está");
        let (key, generation) = (fila.key, b1.generation);
        h.dispatch(UiAction::SelectRow {
            slot_id: 1,
            key,
            generation,
        })
        .await
        .expect("host vivo");
        // F8 basta: el cuerpo del borrado y el de la transferencia se
        // construyen con la MISMA función.
        h.dispatch(tecla("F8")).await.expect("host vivo");
        let d = siguientes_dialogos(&mut sub).await[0].clone();
        for l in &d.body {
            sin_peligro(&l.text, id, "una línea del cuerpo de un diálogo");
        }
        assert_eq!(
            d.body.iter().any(|l| l.hostile),
            altera,
            "[{id}] la marca del cuerpo dice exactamente lo que `display_name` dice: {:?}",
            d.body
        );
    }
}

/// El destino se compone con los BYTES del origen, también cuando no son
/// UTF-8. La ruta que cruza el wire no ha pasado por pantalla.
#[tokio::test]
async fn el_destino_se_compone_byte_a_byte() {
    let backend = arbol();
    let (h, snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // `caf\xC3(`: no es UTF-8, y en pantalla lleva un U+FFFD.
    let b1 = listado_de(&snap, 1);
    let fila = b1
        .rows
        .iter()
        .find(|r| r.hostile)
        .expect("el árbol trae un nombre que no es UTF-8");
    let (key, generation) = (fila.key, b1.generation);
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host vivo");
    h.dispatch(tecla("F5")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    let ts = anotados(&backend, "la transferencia encolada", 1, |f| {
        f.transferencias.lock().expect("transferencias").clone()
    })
    .await;
    let (from, to, _, _) = &ts[0];
    assert!(
        from.to_wire().starts_with("mem:///casa/caf"),
        "el origen es la entrada que no es UTF-8: {}",
        from.to_wire()
    );
    let nombre = from
        .to_wire()
        .strip_prefix("mem:///casa/")
        .expect("cuelga de casa")
        .to_owned();
    assert_eq!(
        to.to_wire(),
        format!("mem:///casa/docs/{nombre}"),
        "los bytes del nombre llegan intactos"
    );
    assert!(
        !to.to_wire().contains("%EF%BF%BD"),
        "y sin el U+FFFD que la pantalla pinta: {}",
        to.to_wire()
    );
}

/// Mover relista TAMBIÉN el panel de origen: de ahí desaparecen entradas.
#[tokio::test]
async fn mover_relista_tambien_el_origen() {
    let backend = arbol();
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F6")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    siguientes_tasks(&mut sub).await;
    let antes = backend.listados();
    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("hay task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    // Los DOS: el origen (`casa`, de donde sale) y el destino (`casa/docs`, a
    // donde llega).
    hasta(&backend, "el relistado de los dos paneles", |f| {
        (f.listados() >= antes + 2).then_some(())
    })
    .await;
}

/// Un refresco conserva el cursor POR RUTA, no por índice.
///
/// La memoria por directorio guarda un índice, y un índice no sobrevive a que
/// la operación quite una entrada: quien miraba un fichero se encontraba el
/// cursor en otro sin haber tocado una tecla, y la siguiente tecla podía ser
/// F8.
#[tokio::test]
async fn el_cursor_sobrevive_a_un_refresco() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![
            (b"a.txt".to_vec(), false),
            (b"b.txt".to_vec(), false),
            (b"c.txt".to_vec(), false),
            (b"d.txt".to_vec(), false),
        ],
    );
    // El borrado QUITA la entrada: sin eso el listado que llega es idéntico
    // y el índice del cursor sigue nombrando el mismo fichero por accidente
    // — un test verde que no prueba nada.
    f.borrar_de_verdad = true;
    let backend = Arc::new(f);
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let b = listado(&snap);
    // Ni la primera ni la ÚLTIMA: sobre la última, borrar una entrada por
    // delante deja el índice viejo recortado justo sobre el mismo fichero, y
    // el test pasaría sin ancla por pura coincidencia.
    let medio = &b.rows[2];
    let (key, generation, nombre) = (medio.key, b.generation, medio.display_name.clone());
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host vivo");

    // Se marca y se borra la PRIMERA, no la del cursor: el listado llega con
    // una entrada menos por DELANTE, así que el índice viejo apunta a otro
    // fichero mientras que la ruta sigue siendo la misma.
    let primera = b.rows.first().expect("hay filas").key;
    h.dispatch(UiAction::ToggleMark {
        slot_id: 1,
        key: primera,
        generation,
    })
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
    siguientes_tasks(&mut sub).await;
    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("hay task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        let b = listado(&foto);
        if b.generation == generation {
            continue;
        }
        let bajo = b
            .cursor
            .and_then(|k| b.rows.iter().find(|r| r.key == k))
            .map(|r| r.display_name.clone());
        assert_eq!(
            bajo.as_deref(),
            Some(nombre.as_str()),
            "el cursor sigue sobre el MISMO fichero tras el refresco"
        );
        return;
    }
    panic!("el refresco no llegó");
}

/// Con TRES listados, un destino designado a mano SOBREVIVE a un cambio de
/// foco, y sin designar no se adivina ninguno.
///
/// El host reasignaba el rol en cada `FocusSlot` con su propia regla —«el
/// primero que no sea el activo»— pisando lo que una persona había elegido y
/// desempatando solo cuando había varios candidatos. Mientras el destino era
/// decoración eso se veía raro; desde que copiar y mover lo leen, es mandar
/// ficheros a un sitio que nadie eligió. La regla es la compartida (ADR 0058
/// D7), y con varios candidatos y ninguno elegido el rol se queda SIN FIJAR.
#[tokio::test]
async fn con_tres_listados_el_destino_no_se_adivina() {
    use norte_ui_host::dto::SlotRole;
    const TRES: &str = r#"
[split]
dir = "vertical"
sizes = [{ weight = 1 }, { fixed = 1 }]

[[split.children]]
[split.children.split]
dir = "horizontal"
sizes = [{ weight = 1 }, { weight = 1 }, { weight = 1 }]

[[split.children.split.children]]
[split.children.split.children.slot]
id = 1
kind = "browser"

[[split.children.split.children]]
[split.children.split.children.slot]
id = 2
kind = "browser"

[[split.children.split.children]]
[split.children.split.children.slot]
id = 3
kind = "browser"

[[split.children]]
[split.children.slot]
id = 4
kind = "status"
"#;
    let arbol_layout: norte_frontend::layout::Node =
        toml::from_str(TRES).expect("la disposición parsea");
    let (h, snap) = UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: arbol_layout,
        viewport: (200, 60),
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
    let rol = |l: &norte_ui_host::dto::LayoutView, id: u32| {
        l.placements
            .iter()
            .find(|p| p.slot_id == id)
            .and_then(|p| p.role)
    };
    let _ = &snap;
    let mut sub = h.subscribe();

    // Sin designar: F5 no adivina, PIDE que se elija.
    let ack = h.dispatch(tecla("F5")).await.expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "host-no-target-designated".to_owned()
        },
        "con tres paneles el destino se elige, no se desempata: {ack:?}"
    );

    // `layout.set-target` no lo ata ningún preset, así que se corre por la
    // PALETA — que es la otra puerta del catálogo, y sirve igual.
    let mut puesto = false;
    for _ in 0..4 {
        h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
            key: "p".to_owned(),
            ctrl: true,
            alt: false,
            shift: false,
            meta: false,
        }))
        .await
        .expect("host vivo");
        for c in ["s", "e", "t", "-", "t", "a", "r", "g", "e", "t"] {
            h.dispatch(tecla_de(c)).await.expect("host vivo");
        }
        h.dispatch(tecla("Enter")).await.expect("host vivo");
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        if rol(&foto.layout, 3) == Some(SlotRole::Target) {
            puesto = true;
            break;
        }
    }
    assert!(puesto, "se puede designar el tercero");

    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let despues = siguiente_foto(&mut sub).await;
    assert_eq!(
        rol(&despues.layout, 3),
        Some(SlotRole::Target),
        "el destino ELEGIDO sobrevive al cambio de foco"
    );
}

/// Las marcas que consume el envío son las del hueco de ORIGEN, aunque el
/// foco se haya ido a otro entre la pregunta y la respuesta.
///
/// `FocusSlot` no está vedada mientras hay un diálogo abierto: solo lo están
/// las teclas. Un clic en el otro panel borraba las marcas del panel ajeno y
/// dejaba intactas las que se acababan de enviar.
#[tokio::test]
async fn las_marcas_que_se_consumen_son_las_del_origen() {
    let backend = arbol();
    let (h, snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let b1 = listado_de(&snap, 1);
    let generation = b1.generation;
    let claves: Vec<_> = b1
        .rows
        .iter()
        .filter(|r| r.display_name != "docs")
        .map(|r| r.key)
        .collect();
    for key in &claves {
        h.dispatch(UiAction::ToggleMark {
            slot_id: 1,
            key: *key,
            generation,
        })
        .await
        .expect("host vivo");
    }
    let mut sub = h.subscribe();
    h.dispatch(tecla("F5")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;

    // Y AHORA el foco se va al otro panel, sin cerrar el diálogo.
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host vivo");
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    anotados(
        &backend,
        "la transferencia que consume las marcas",
        1,
        |f| f.transferencias.lock().expect("transferencias").clone(),
    )
    .await;

    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(
        listado_de(&foto, 1).marks,
        0,
        "las marcas consumidas son las del hueco que las mandó"
    );
}

/// Un hueco OCULTO sobre el directorio afectado no se lista —lo que no se ve
/// no se trae— pero queda marcado para recargar en cuanto vuelva.
///
/// Sin esto, una pestaña de atrás sobre el directorio de destino enseñaba un
/// listado anterior a la operación hasta que alguien navegara a mano, y una
/// tecla sobre una de sus filas actuaba contra ese listado viejo.
#[tokio::test]
async fn un_hueco_oculto_afectado_queda_para_recargar() {
    let backend = arbol();
    // Nace ANCHA, para que el segundo listado se liste de verdad y quede
    // `Ready`. Si naciera escondido estaría `Loading` desde el principio y se
    // recargaría al volver por ese motivo, no por este.
    let (h, snap) = host_con_layout(Arc::clone(&backend), "orthodox", (160, 40)).await;
    assert!(
        snap.slots
            .iter()
            .filter(|s| matches!(s, SlotView::Browser(_)))
            .count()
            >= 2,
        "los dos listados se ven"
    );
    let mut sub = h.subscribe();
    // Y ahora se estrecha hasta que solo cabe uno.
    h.dispatch(UiAction::SetViewport {
        width: 30,
        height: 10,
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let estrecha = siguiente_foto(&mut sub).await;
    assert_eq!(
        estrecha
            .slots
            .iter()
            .filter(|s| matches!(s, SlotView::Browser(_)))
            .count(),
        1,
        "con 30 columnas solo cabe un listado"
    );
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
    let listados_antes = backend.listados();
    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("hay task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    // El desenlace de la task llega por su propio canal: se deja correr antes
    // de ensanchar, para que el hueco escondido ya esté marcado.
    asentar().await;

    // Se ensancha la ventana: el hueco que estaba escondido vuelve, y como
    // quedó marcado CARGANDO, se lista.
    h.dispatch(UiAction::SetViewport {
        width: 160,
        height: 40,
    })
    .await
    .expect("host vivo");
    hasta(&backend, "el relistado del hueco que volvió", |f| {
        (f.listados() > listados_antes + 1).then_some(())
    })
    .await;
}

/// En solo lectura, la PALETA tampoco ofrece lo que muta.
///
/// Era la única puerta que no pasaba por el keymap efectivo: ofrecía copiar,
/// mover y borrar, y la guarda de ejecución los rechazaba. Ofrecer lo que se
/// va a rehusar es prometer algo que no se va a hacer.
#[tokio::test]
async fn en_solo_lectura_la_paleta_no_ofrece_lo_que_muta() {
    let (h, _snap) = host_solo_lectura(arbol()).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");
    let p = siguiente_paleta(&mut sub).await.expect("la paleta abre");
    for cmd in norte_ui_host::commands::MUTAN {
        assert!(
            !p.rows.iter().any(|r| r.text == *cmd),
            "la paleta de una ventana de solo lectura ofrece {cmd}"
        );
    }
}

/// Y la tecla lo dice con SU motivo, no con uno cualquiera.
#[tokio::test]
async fn en_solo_lectura_copiar_dice_por_que() {
    let (h, _snap) = host_solo_lectura(arbol()).await;
    let ack = h.dispatch(tecla("F5")).await.expect("host vivo");
    let ActionAck::Unavailable { reason_key } = ack else {
        panic!("se esperaba no disponible: {ack:?}");
    };
    assert!(
        reason_key == "cmd-not-here" || reason_key == "host-read-only",
        "y con un motivo del vocabulario, no uno inventado: {reason_key}"
    );
}

/// Un refresco conserva las MARCAS, por ruta.
///
/// `set_listing` las limpia porque las filas son otras — correcto para un
/// `cd`, y un castigo para quien no se movió: el panel de DESTINO de una
/// copia se relista cuando la copia termina, y se llevaba por delante una
/// selección que su dueño había hecho a mano y que nadie había enviado.
#[tokio::test]
async fn las_marcas_sobreviven_a_un_refresco() {
    let backend = arbol();
    let (h, snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // Marcas en el panel de DESTINO: no son las que la copia consume, así que
    // lo único que puede quitarlas es el relistado.
    let b2 = listado_de(&snap, 2);
    let generation2 = b2.generation;
    let claves: Vec<_> = b2.rows.iter().map(|r| r.key).collect();
    assert!(!claves.is_empty(), "el destino tiene filas");
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host vivo");
    for key in &claves {
        h.dispatch(UiAction::ToggleMark {
            slot_id: 2,
            key: *key,
            generation: generation2,
        })
        .await
        .expect("host vivo");
    }
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host vivo");

    h.dispatch(tecla("F5")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    siguientes_tasks(&mut sub).await;
    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("hay task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        let b = listado_de(&foto, 2);
        if b.generation == generation2 {
            continue;
        }
        assert_eq!(
            b.marks,
            claves.len() as u64,
            "el relistado del destino no se lleva por delante lo que su dueño \
             había marcado"
        );
        return;
    }
    panic!("el refresco del destino no llegó");
}

/// Un movimiento relista el panel de ORIGEN aunque el provider escriba el
/// padre de sus entradas con OTRA ortografía del mismo directorio.
///
/// El padre de una entrada lo escribe el provider; el directorio del panel
/// puede venir de la config, de la sesión o de un favorito. En macOS (NFD
/// contra NFC) y contra un servidor sin distinción de caja son dos cadenas
/// para el mismo sitio, y la comparación byte a byte no las junta (ADR 0061).
#[tokio::test]
async fn mover_relista_el_origen_aunque_el_provider_lo_escriba_distinto() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)],
    );
    f.pon("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    // El provider cuelga sus entradas de `⟨mem⟩/CASA`, no de `⟨mem⟩/casa`.
    f.padre_distinto = true;
    let backend = Arc::new(f);
    let (h, snap) = host_con_layout(Arc::clone(&backend), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();

    // El destino, en `docs`.
    let b2 = listado_de(&snap, 2);
    let docs = b2
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("está");
    let (key, generation) = (docs.key, b2.generation);
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host vivo");
    h.dispatch(UiAction::Activate {
        slot_id: 2,
        key,
        generation,
    })
    .await
    .expect("host vivo");
    let mut despues = siguiente_foto(&mut sub).await;
    while listado_de(&despues, 2).path_display == listado_de(&snap, 2).path_display {
        despues = siguiente_foto(&mut sub).await;
    }
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host vivo");

    h.dispatch(tecla("F6")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    siguientes_tasks(&mut sub).await;
    let antes = backend.listados();
    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("hay task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    // Los DOS paneles. Sin apuntar el directorio del HUECO de origen, el
    // padre de la entrada (`⟨mem⟩/CASA`) no casaría con lo que el panel
    // enseña (`⟨mem⟩/casa`) y el origen se quedaría sin relistar.
    hasta(&backend, "el relistado de los dos paneles", |f| {
        (f.listados() >= antes + 2).then_some(())
    })
    .await;
}

// ---------------------------------------------------------------------------
// Renombrar UNA entrada (tarea 5.2).
// ---------------------------------------------------------------------------

/// `shift+F6` abre el nombre EDITABLE, sembrado con lo que la fila pinta.
#[tokio::test]
async fn renombrar_abre_el_nombre_para_editarlo() {
    let backend = arbol();
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // El cursor, sobre `notas.txt`.
    let b = listado(&snap);
    let notas = b
        .rows
        .iter()
        .find(|r| r.display_name == "notas.txt")
        .expect("está");
    let (key, generation) = (notas.key, b.generation);
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host vivo");

    h.dispatch(tecla_mod("F6", false, true))
        .await
        .expect("host vivo");
    let d = siguientes_dialogos(&mut sub).await[0].clone();
    assert_eq!(d.title_key, "modal-rename-title");
    assert_eq!(
        d.input.as_deref(),
        Some("notas.txt"),
        "el campo nace con el nombre de ahora"
    );
    assert!(d.destination.is_none(), "un rename no va a otro sitio");
    assert!(
        backend
            .transferencias
            .lock()
            .expect("transferencias")
            .is_empty(),
        "abrir el diálogo no renombra"
    );
}

/// Sin tocar el campo NO se renombra nada, y esa es la protección.
///
/// Es la regla 1 en la costura: el campo se siembra con lo que la fila PINTA,
/// y para un nombre que no es UTF-8 eso lleva un U+FFFD. Sin tocar se
/// reconstruyen los bytes ORIGINALES — que son los de ahora, o sea «mismo
/// nombre, mismo sitio»—, así que la siembra nunca puede convertirse en el
/// operando. Mandar el texto sin más escribiría mojibake de verdad.
#[tokio::test]
async fn un_nombre_sin_tocar_no_renombra_nada() {
    let backend = arbol();
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let b = listado(&snap);
    let hostil = b.rows.iter().find(|r| r.hostile).expect("hay uno");
    let (key, generation) = (hostil.key, b.generation);
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host vivo");
    h.dispatch(tecla_mod("F6", false, true))
        .await
        .expect("host vivo");
    let d = siguientes_dialogos(&mut sub).await[0].clone();
    assert!(d.input_hostile, "el campo dice que lo sembrado no es fiel");

    // Se confirma SIN escribir nada. No hay renombrado posible —el destino
    // sería el mismo— y eso se DICE en el acuse, no solo en la barra.
    let ack = h
        .dispatch(UiAction::Dialog {
            id: d.id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "msg-transfer-name-same".to_owned()
        },
        "{ack:?}"
    );
    asentar().await;
    assert!(
        backend
            .transferencias
            .lock()
            .expect("transferencias")
            .is_empty(),
        "el mismo nombre en el mismo sitio no es una operación"
    );
}

/// Un nombre TOCADO que aún lleva el carácter de sustitución se RECHAZA:
/// confirmarlo escribiría el mojibake que la pantalla inventó.
#[tokio::test]
async fn un_nombre_tocado_con_fffd_se_rechaza() {
    let backend = arbol();
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let b = listado(&snap);
    let hostil = b.rows.iter().find(|r| r.hostile).expect("hay uno");
    let (key, generation, pintado) = (hostil.key, b.generation, hostil.display_name.clone());
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host vivo");
    h.dispatch(tecla_mod("F6", false, true))
        .await
        .expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;

    // Se edita: el renderer devuelve lo que había MÁS una letra, y lo que
    // había lleva el U+FFFD que puso la pantalla.
    h.dispatch(UiAction::DialogInput {
        id,
        text: format!("{pintado}x"),
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
    asentar().await;
    assert!(
        backend
            .transferencias
            .lock()
            .expect("transferencias")
            .is_empty(),
        "no se escribe un nombre que la pantalla se inventó"
    );
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(
        foto.status.message.is_some_and(|m| !m.starts_with("msg-")),
        "y se dice, traducido"
    );
}

/// Un nombre nuevo sale como un `fs.move` dentro del MISMO directorio.
#[tokio::test]
async fn un_nombre_nuevo_sale_como_movimiento_al_mismo_sitio() {
    let backend = arbol();
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let b = listado(&snap);
    let notas = b
        .rows
        .iter()
        .find(|r| r.display_name == "notas.txt")
        .expect("está");
    let (key, generation) = (notas.key, b.generation);
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host vivo");
    h.dispatch(tecla_mod("F6", false, true))
        .await
        .expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "apuntes.md".to_owned(),
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
    let ts = anotados(&backend, "el renombrado encolado", 1, |f| {
        f.transferencias.lock().expect("transferencias").clone()
    })
    .await;
    assert_eq!(ts.len(), 1);
    let (from, to, mover, colision) = &ts[0];
    assert!(mover, "renombrar es mover");
    assert_eq!(from.to_wire(), "mem:///casa/notas.txt");
    assert_eq!(
        to.to_wire(),
        "mem:///casa/apuntes.md",
        "al MISMO directorio"
    );
    assert_eq!(*colision, norte_proto::CollisionPolicy::Fail);
}

/// Con VARIAS marcas, esta ventana se niega: cuál renombrar no lo dice
/// nadie. Es la asimetría que `Facts::rename_single` documenta, y este host
/// ya la declaraba en `hechos()`.
#[tokio::test]
async fn renombrar_con_varias_marcas_se_niega() {
    let backend = arbol();
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let b = listado(&snap);
    let generation = b.generation;
    for r in b.rows.iter().take(2) {
        h.dispatch(UiAction::ToggleMark {
            slot_id: 1,
            key: r.key,
            generation,
        })
        .await
        .expect("host vivo");
    }
    let ack = h
        .dispatch(tecla_mod("F6", false, true))
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "reason-wrong-target".to_owned()
        },
        "{ack:?}"
    );
}

/// Y en solo lectura, ni se abre.
#[tokio::test]
async fn en_solo_lectura_renombrar_no_abre_nada() {
    let backend = arbol();
    let (h, _snap) = host_solo_lectura(Arc::clone(&backend)).await;
    let ack = h
        .dispatch(tecla_mod("F6", false, true))
        .await
        .expect("host vivo");
    assert!(matches!(ack, ActionAck::Unavailable { .. }), "{ack:?}");
    asentar().await;
    assert!(
        backend
            .transferencias
            .lock()
            .expect("transferencias")
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// El plan de renombrado que propone un modelo (tarea 5.2).
// ---------------------------------------------------------------------------

fn hash_de_prueba() -> norte_proto::methods::PlanHash {
    norte_proto::methods::PlanHash::parse(&"a".repeat(norte_proto::methods::PLAN_HASH_LEN))
        .expect("hex válido")
}

/// Un veredicto del core: aplicable, con `n` pasos reales.
fn veredicto_ok(pares: &[(&str, &str)]) -> norte_proto::methods::FsRenameBatchPlanResult {
    norte_proto::methods::FsRenameBatchPlanResult {
        steps: pares
            .iter()
            .map(|(f, t)| norte_proto::methods::RenameStep {
                from: norte_proto::Segment::new(f.as_bytes().to_vec()).expect("seg"),
                to: norte_proto::Segment::new(t.as_bytes().to_vec()).expect("seg"),
                temp: false,
            })
            .collect(),
        collisions: Vec::new(),
        executable: true,
        plan_hash: hash_de_prueba(),
    }
}

/// Un backend con un plan de IA y su veredicto.
fn falso_con_plan(
    pares: &[(&str, &str)],
    veredicto: Option<norte_proto::methods::FsRenameBatchPlanResult>,
) -> Arc<Falso> {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        pares
            .iter()
            .map(|(from, _)| (from.as_bytes().to_vec(), false))
            .collect::<Vec<_>>(),
    );
    f.plan_ia = Some(
        pares
            .iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect(),
    );
    f.veredicto = veredicto;
    Arc::new(f)
}

/// Espera la siguiente actualización que traiga la revisión del plan.
async fn siguiente_revision(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::AiRenameView> {
    for _ in 0..40 {
        let siguiente = tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv())
            .await
            .expect("una actualización, no un cuelgue")
            .expect("el host sigue vivo");
        match siguiente {
            Update::Message(m) => {
                if let UiUpdate::Patch(p) = &m.payload {
                    for c in &p.changes {
                        if let norte_ui_host::dto::ViewChange::AiRename { ai_rename } = c {
                            return ai_rename.clone();
                        }
                    }
                }
                if let UiUpdate::Snapshot(s) = &m.payload
                    && s.ai_rename.is_some()
                {
                    return s.ai_rename.clone();
                }
            }
            Update::Lagged => panic!("sin retraso en este test"),
        }
    }
    panic!("la revisión no llegó");
}

/// Pide un plan: abre el prompt de la instrucción y lo contesta.
///
/// `pane.ai-rename` no lo ata ningún preset de fábrica, así que llega por la
/// paleta, que es la otra puerta del catálogo.
async fn pedir_plan(h: &UiHost, sub: &mut norte_ui_host::UiSubscription) {
    por_la_paleta(h, sub, "ai-rename").await;
    let id = siguientes_dialogos(sub).await[0].id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "numera los episodios".to_owned(),
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
}

/// #310: el renombrado en lote por PLANTILLA en la ventana. El prompt fija
/// el operando (lo marcado), la plantilla se valida con el humano delante y
/// el prompt vuelve con lo tecleado y el diagnóstico en la barra, y el plan
/// —determinista, sin modelo— entra por la MISMA revisión que el de la IA,
/// con el veredicto del core en su viaje.
#[tokio::test]
async fn el_lote_por_plantilla_se_revisa_como_el_de_la_ia() {
    let pares = [("ep1.mkv", "ep01.mkv"), ("ep2.mkv", "ep02.mkv")];
    let backend = falso_con_plan(&pares, Some(veredicto_ok(&pares)));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // El lote actúa sobre lo MARCADO: los dos.
    por_la_paleta(&h, &mut sub, "mark.all").await;
    por_la_paleta(&h, &mut sub, "rename-batch").await;
    let prompt = foto_hasta(&h, &mut sub, "el prompt de la plantilla", |s| {
        s.dialogs
            .iter()
            .find(|d| d.title_key == "modal-rename-batch")
            .cloned()
    })
    .await;
    assert_eq!(
        prompt.input.as_deref(),
        Some("[N].[E]"),
        "prellenado con la identidad, como la TUI"
    );

    // Una plantilla que dejaría un `/` dentro se explica y el prompt VUELVE
    // con lo tecleado, en vez de tirarlo.
    h.dispatch(UiAction::DialogInput {
        id: prompt.id,
        text: "a/[N]".to_owned(),
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Dialog {
        id: prompt.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    let reabierto = foto_hasta(
        &h,
        &mut sub,
        "el prompt reabierto con el diagnóstico",
        |s| {
            s.dialogs
                .iter()
                .find(|d| d.title_key == "modal-rename-batch" && d.id != prompt.id)
                .cloned()
                .filter(|_| s.status.message.as_deref().is_some_and(|m| m.contains('/')))
        },
    )
    .await;
    assert_eq!(reabierto.input.as_deref(), Some("a/[N]"));
    assert!(
        backend.instrucciones.lock().expect("mutex").is_empty(),
        "al modelo no se le pidió nada"
    );

    // La buena: el plan se genera aquí y se revisa como el de la IA.
    h.dispatch(UiAction::DialogInput {
        id: reabierto.id,
        text: "ep0[C].[E]".to_owned(),
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Dialog {
        id: reabierto.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    let mut v = siguiente_revision(&mut sub)
        .await
        .expect("abre la revisión");
    assert_eq!(v.total, 2, "{v:?}");
    assert_eq!(v.pairs[0].from.text, "ep1.mkv");
    assert_eq!(v.pairs[0].to.text, "ep01.mkv");
    assert_eq!(v.pairs[1].to.text, "ep02.mkv");
    for _ in 0..40 {
        if v.confirmable {
            break;
        }
        v = siguiente_revision(&mut sub).await.expect("sigue abierta");
    }
    assert!(
        v.confirmable,
        "el core dio su veredicto sobre el plan de la plantilla"
    );
    assert!(
        backend.instrucciones.lock().expect("mutex").is_empty(),
        "sigue sin haber modelo de por medio"
    );
    assert_eq!(
        backend.veredictos_pedidos.lock().expect("mutex").len(),
        1,
        "un veredicto pedido, para el plan de la plantilla"
    );
    assert!(
        backend.lotes.lock().expect("lotes").is_empty(),
        "revisar no aplica nada"
    );
}

/// #312: comparar dos ficheros desde la ventana. El operando y el programa
/// son las decisiones compartidas con la TUI (`diffpair`, `[ui] diff`,
/// `diff -u` por defecto); lo que cambia es que quien hospeda corre el
/// programa ESPERÁNDOLO —el argv sale resuelto e interpolado, en bytes— y lo
/// que imprimió vuelve como acción y se enseña en su panel hasta que se
/// cierra.
#[tokio::test]
async fn comparar_dos_ficheros_corre_el_comparador_y_ensena_su_salida() {
    let mut falso = Falso::default();
    falso.pon(
        "file:///casa",
        vec![(b"a.txt".to_vec(), false), (b"b.txt".to_vec(), false)],
    );
    let backend = Arc::new(falso);
    let (h, _snap) = host_en(Arc::clone(&backend), "file:///casa").await;
    let mut sub = h.subscribe();
    let mut efectos = h.native_effects();

    // Con UNO solo bajo el cursor y nada enfrente, el comando lo DICE.
    // `alt+C` es su atajo en el preset ortodoxo.
    let ack = h
        .dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
            key: "C".to_owned(),
            ctrl: false,
            alt: true,
            shift: false,
            meta: false,
        }))
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Unavailable { ref reason_key } if reason_key == "msg-compare-files-need-two"),
        "fue {ack:?}"
    );

    // Marcados los dos: sale el efecto con el argv por defecto, resuelto.
    por_la_paleta(&h, &mut sub, "mark.all").await;
    ejecutar_por_paleta(&h, &mut sub, "pane.compare-files").await;
    let efecto = tokio::time::timeout(std::time::Duration::from_secs(2), efectos.recv())
        .await
        .expect("sale el efecto")
        .expect("canal vivo");
    let norte_ui_host::dto::NativeEffect::RunProgram {
        title_key,
        argv,
        cwd,
        detached,
    } = efecto
    else {
        panic!("se esperaba correr un programa: {efecto:?}");
    };
    assert_eq!(title_key, "program-output-compare");
    assert!(
        !detached,
        "`diff -u` se espera: su salida es lo que se enseña"
    );
    let como_texto: Vec<String> = argv
        .iter()
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    assert!(
        como_texto[0].ends_with("/diff"),
        "el programa va resuelto a ruta absoluta (ADR 0082): {como_texto:?}"
    );
    assert_eq!(
        &como_texto[1..],
        ["-u", "/casa/a.txt", "/casa/b.txt"],
        "las DOS rutas nativas, interpoladas por `%F`"
    );
    assert_eq!(cwd.as_deref(), Some(b"/casa".as_slice()));

    // Lo que imprimió vuelve como acción y se enseña, por líneas y
    // enmascarado; Esc lo cierra.
    h.dispatch(UiAction::ProgramFinished {
        title_key,
        command: como_texto.join(" "),
        output: b"--- a.txt\n+++ b.txt\n-hola\x1b[31m\n+adios\n".to_vec(),
        truncated: false,
        failed: false,
    })
    .await
    .expect("host vivo");
    let con_salida = foto_hasta(&h, &mut sub, "la salida del programa", |s| {
        s.program_output.clone()
    })
    .await;
    assert_eq!(con_salida.title_key, "program-output-compare");
    assert_eq!(con_salida.lines.len(), 4, "{con_salida:?}");
    assert_eq!(con_salida.lines[0], "--- a.txt");
    assert!(
        con_salida.text_hostile,
        "el escape de la tercera línea se marcó"
    );
    assert!(!con_salida.lines[2].contains('\x1b'));
    assert!(!con_salida.failed);
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    foto_hasta(&h, &mut sub, "el panel cerrado", |s| {
        s.program_output.is_none().then_some(())
    })
    .await;
}

/// El plan se REVISA antes de nada: llega, se pinta pareja a pareja, y el
/// veredicto del core llega DESPUÉS, en su propio viaje.
#[tokio::test]
async fn un_plan_se_revisa_antes_de_aplicarse() {
    let pares = [("ep1.mkv", "ep01.mkv"), ("ep2.mkv", "ep02.mkv")];
    let backend = falso_con_plan(&pares, Some(veredicto_ok(&pares)));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;

    let primera = siguiente_revision(&mut sub).await.expect("abre");
    assert_eq!(primera.total, 2, "las dos parejas");
    assert_eq!(primera.pairs[0].from.text, "ep1.mkv");
    assert_eq!(primera.pairs[0].to.text, "ep01.mkv");
    assert!(
        backend.lotes.lock().expect("lotes").is_empty(),
        "revisar no aplica nada"
    );

    // El veredicto llega en su propio viaje, y hasta entonces no se puede
    // aprobar.
    let mut v = primera;
    for _ in 0..40 {
        if v.confirmable {
            break;
        }
        v = siguiente_revision(&mut sub).await.expect("sigue abierta");
    }
    assert!(v.confirmable, "el core dijo que es aplicable");
    assert!(
        v.real_steps_note.contains('2'),
        "y cuántos renombra DE VERDAD, dicho y traducido: {}",
        v.real_steps_note
    );
    assert!(!v.status.is_empty() && !v.status.starts_with("modal-"));
}

/// Aprobar manda UNA task para el lote, con el `plan_hash` que devolvió el
/// core: se ejecuta EXACTAMENTE lo que se enseñó.
#[tokio::test]
async fn aprobar_manda_el_lote_con_el_hash_del_core() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = falso_con_plan(&pares, Some(veredicto_ok(&pares)));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    let mut v = siguiente_revision(&mut sub).await.expect("abre");
    for _ in 0..40 {
        if v.confirmable {
            break;
        }
        v = siguiente_revision(&mut sub).await.expect("sigue abierta");
    }

    // La PRIMERA tecla solo reconoce la pantalla: se abrió sola y se quedó
    // el teclado, así que la tecla que venía en camino no puede ser una
    // respuesta. La segunda ya aprueba.
    h.dispatch(tecla("y")).await.expect("host vivo");
    assert!(
        backend.lotes.lock().expect("lotes").is_empty(),
        "la primera tecla no aprueba nada"
    );
    h.dispatch(tecla("y")).await.expect("host vivo");
    let lotes = anotados(&backend, "el lote aprobado", 1, |f| {
        f.lotes.lock().expect("lotes").clone()
    })
    .await;
    assert_eq!(lotes.len(), 1, "UNA task para el lote entero");
    let (dir, parejas, hash) = &lotes[0];
    assert_eq!(dir.to_wire(), "mem:///casa");
    assert_eq!(parejas.len(), 1);
    assert_eq!(parejas[0].from.as_bytes(), b"ep1.mkv");
    assert_eq!(parejas[0].to.as_bytes(), b"ep01.mkv");
    assert_eq!(hash, &hash_de_prueba(), "el hash es el que dio el core");
}

/// Un plan que el core NO acepta no se puede aprobar, y se dice.
#[tokio::test]
async fn un_plan_no_aplicable_no_se_aprueba() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let mut v = veredicto_ok(&pares);
    v.executable = false;
    v.steps.clear();
    v.collisions = vec![norte_proto::methods::RenameCollision {
        pair_index: 0,
        kind: norte_proto::methods::RenameCollisionKind::External,
        name: norte_proto::Segment::new(b"ep01.mkv".to_vec()).expect("seg"),
    }];
    let backend = falso_con_plan(&pares, Some(v));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    let mut r = siguiente_revision(&mut sub).await.expect("abre");
    for _ in 0..40 {
        if !r.detail.is_empty() {
            break;
        }
        r = siguiente_revision(&mut sub).await.expect("sigue abierta");
    }
    assert!(!r.confirmable, "el core dijo que no");
    assert!(!r.detail.is_empty(), "y la colisión se LEE: {:?}", r.detail);

    // La primera tecla reconoce la pantalla; la segunda intenta aprobar.
    h.dispatch(tecla("y")).await.expect("host vivo");
    let ack = h.dispatch(tecla("y")).await.expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "host-plan-not-applicable".to_owned()
        },
        "{ack:?}"
    );
    assert!(backend.lotes.lock().expect("lotes").is_empty());
}

/// Una pareja que no es un nombre legal tumba el plan ENTERO: aplicar «lo que
/// valga» de un plan adulterado es lo que este cinturón existe para impedir.
#[tokio::test]
async fn una_pareja_invalida_tumba_el_plan_entero() {
    let pares = [("ep1.mkv", "ep01.mkv"), ("ep2.mkv", "../fuera")];
    let backend = falso_con_plan(&pares, Some(veredicto_ok(&[("ep1.mkv", "ep01.mkv")])));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;

    // El plan del modelo ya volvió: lo que se comprueba es lo que el host
    // hace CON él, no que todavía no haya llegado.
    hasta(&backend, "el plan del modelo, ya servido", |f| {
        f.en_calma().then_some(())
    })
    .await;
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(
        foto.ai_rename.is_none(),
        "ni se abre la revisión: {:?}",
        foto.ai_rename
    );
    assert!(
        backend
            .veredictos_pedidos
            .lock()
            .expect("veredictos")
            .is_empty(),
        "ni se le pide veredicto al core a un plan adulterado"
    );
    assert!(foto.status.message.is_some(), "y se dice");
}

/// Un plan que llega TARDE, después de que el lector cerrara la revisión, no
/// la reabre.
///
/// El modelo tarda, y en esa ventana el lector puede descartar. Sin subir la
/// época al cerrar, el plan aterrizaba encima de una pantalla que su dueño ya
/// había quitado — y con las teclas puestas sobre él.
#[tokio::test]
async fn un_plan_que_llega_tarde_no_reabre_lo_cerrado() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"ep1.mkv".to_vec(), false)]);
    f.plan_ia = Some(vec![("ep1.mkv".to_owned(), "ep01.mkv".to_owned())]);
    f.veredicto = Some(veredicto_ok(&pares));
    f.retraso_ia_ms = 150;
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;

    // Antes de que el modelo conteste, se descarta. Y se espera a que el
    // plan tardío HAYA llegado: sin eso, el test pasaría por no haber
    // esperado bastante, que es la forma más silenciosa de no probar nada.
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    hasta(&backend, "el plan tardío, ya servido", |f| {
        f.en_calma().then_some(())
    })
    .await;
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(
        foto.ai_rename.is_none(),
        "el plan tardío no reabre lo cerrado: {:?}",
        foto.ai_rename
    );
    assert!(backend.lotes.lock().expect("lotes").is_empty());
}

/// Descartar no aplica nada, y la revisión se queda cerrada.
#[tokio::test]
async fn descartar_cierra_y_no_aplica_nada() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = falso_con_plan(&pares, Some(veredicto_ok(&pares)));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    siguiente_revision(&mut sub).await.expect("abre");

    h.dispatch(tecla("Escape")).await.expect("host vivo");
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.ai_rename.is_none(), "se cerró y sigue cerrada");
    assert!(backend.lotes.lock().expect("lotes").is_empty());
}

/// Los nombres del plan los propone un MODELO sobre nombres que escribió
/// cualquiera: se enmascaran y se DICE.
#[tokio::test]
async fn un_nombre_hostil_del_plan_va_marcado() {
    // Del corpus canónico, no escrito a mano: un nombre inventado en el test
    // prueba lo que el test cree, y el corpus prueba lo que de verdad hay.
    let bytes = hostil("rtl_override");
    let alterado = String::from_utf8(bytes).expect("el del corpus es UTF-8");
    let pares = [("ep1.mkv", alterado.as_str())];
    let backend = falso_con_plan(&pares, None);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    let r = siguiente_revision(&mut sub).await.expect("abre");
    assert!(
        !r.pairs[0].to.text.contains('\u{202E}'),
        "enmascarado: {:?}",
        r.pairs[0].to
    );
    assert!(r.pairs[0].to.hostile, "y marcado: {:?}", r.pairs[0].to);
    assert!(!r.pairs[0].from.hostile, "el de origen no lo es");
}

/// Un plan más largo que la ventana se recorre entero.
#[tokio::test]
async fn un_plan_largo_se_recorre() {
    let pares: Vec<(String, String)> = (0..12)
        .map(|i| (format!("a{i:02}.mkv"), format!("b{i:02}.mkv")))
        .collect();
    let refs: Vec<(&str, &str)> = pares
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let backend = falso_con_plan(&refs, None);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    let r = siguiente_revision(&mut sub).await.expect("abre");
    assert_eq!(r.total, 12);
    assert!(
        r.pairs.len() < 12,
        "solo la ventana viaja: {}",
        r.pairs.len()
    );
    assert_eq!(r.first_visible, 0);

    // La primera tecla reconoce la pantalla; a partir de ahí se recorre.
    h.dispatch(tecla("PageDown")).await.expect("host vivo");
    h.dispatch(tecla("PageDown")).await.expect("host vivo");
    // El veredicto del core viaja por el MISMO canal ordenado, así que puede
    // haber una actualización suya por delante de la del recorrido.
    let mut bajado = siguiente_revision(&mut sub).await.expect("sigue abierta");
    for _ in 0..10 {
        if bajado.first_visible > 0 {
            break;
        }
        bajado = siguiente_revision(&mut sub).await.expect("sigue abierta");
    }
    assert!(bajado.first_visible > 0, "se recorrió: {bajado:?}");

    for _ in 0..10 {
        h.dispatch(tecla("PageDown")).await.expect("host vivo");
    }
    let tope = siguiente_revision(&mut sub).await.expect("sigue abierta");
    assert!(
        tope.first_visible + tope.pairs.len() as u64 <= tope.total,
        "la ventana no se sale del plan: {tope:?}"
    );
}

/// En solo lectura no se le pide un plan a nadie.
#[tokio::test]
async fn en_solo_lectura_no_se_pide_plan() {
    let backend = falso_con_plan(&[("a", "b")], None);
    let (h, _snap) = host_solo_lectura(Arc::clone(&backend)).await;
    // Ni la paleta lo ofrece: una ventana de solo lectura no lista lo que
    // muta. Y aunque llegara por otra puerta, la guarda lo rehúsa.
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");
    let p = siguiente_paleta(&mut sub).await.expect("la paleta abre");
    assert!(
        !p.rows.iter().any(|r| r.text == "pane.ai-rename"),
        "una ventana de solo lectura no ofrece pedir un plan"
    );
    asentar().await;
    assert!(
        backend
            .instrucciones
            .lock()
            .expect("instrucciones")
            .is_empty()
    );
}

/// Un plan se abre sobre el directorio para el que se PIDIÓ, aunque el lector
/// haya navegado mientras el modelo pensaba.
///
/// Leer el directorio del hueco al aterrizar prometía renombrar lo que se ve
/// —que ya es otra cosa— y habría renombrado lo de antes.
#[tokio::test]
async fn un_plan_se_abre_sobre_el_directorio_que_se_planeo() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"ep1.mkv".to_vec(), false)],
    );
    f.pon("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    f.plan_ia = Some(vec![("ep1.mkv".to_owned(), "ep01.mkv".to_owned())]);
    f.retraso_ia_ms = 150;
    let backend = Arc::new(f);
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let b = listado(&snap);
    let docs = b
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("está");
    let (key, generation) = (docs.key, b.generation);
    pedir_plan(&h, &mut sub).await;

    // Y mientras el modelo piensa, el lector se va a otro directorio.
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host vivo");

    let r = siguiente_revision(&mut sub).await.expect("abre igual");
    assert!(
        r.dir.text.ends_with("/casa"),
        "el plan es del directorio que se planeó, no del que se ve ahora: {:?}",
        r.dir
    );
}

/// DOS peticiones vivas a la vez: la respuesta de la primera no puede matar a
/// la segunda.
///
/// `Option::take` vacía el hueco ANTES de que el filtro mire, así que una
/// respuesta vieja se llevaba por delante la petición viva y se quedaban las
/// DOS sin abrir — sin decir nada, y sin poder distinguirse de un daemon
/// muerto. Y la secuencia es la normal: pedir, no ver nada, volver a pedir.
#[tokio::test]
async fn dos_peticiones_a_la_vez_y_la_segunda_sigue_abriendo() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"ep1.mkv".to_vec(), false)]);
    f.plan_ia = Some(vec![("ep1.mkv".to_owned(), "ep01.mkv".to_owned())]);
    f.veredicto = Some(veredicto_ok(&pares));
    f.retraso_ia_ms = 120;
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // Dos peticiones seguidas, sin esperar a la primera.
    pedir_plan(&h, &mut sub).await;
    pedir_plan(&h, &mut sub).await;

    let r = siguiente_revision(&mut sub).await.expect("la segunda abre");
    assert_eq!(r.total, 1);
    assert_eq!(
        backend.instrucciones.lock().expect("instrucciones").len(),
        2,
        "se pidieron las dos"
    );
}

/// Con un plan en vuelo, `Escape` cierra la PALETA y no mata el plan.
///
/// La rama que abandona el plan estaba por encima del reparto de overlays, así
/// que una sola tecla hacía dos cosas mal: dejaba la paleta abierta y se
/// llevaba por delante el plan que el lector sí quería.
#[tokio::test]
async fn con_un_plan_en_vuelo_escape_cierra_la_paleta() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"ep1.mkv".to_vec(), false)]);
    f.plan_ia = Some(vec![("ep1.mkv".to_owned(), "ep01.mkv".to_owned())]);
    f.veredicto = Some(veredicto_ok(&pares));
    f.retraso_ia_ms = 120;
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;

    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");
    let _ = siguiente_paleta(&mut sub).await.expect("abre");
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.palette.is_none(), "el Escape cerró la paleta");

    // Y el plan sigue vivo: llega y abre.
    let r = siguiente_revision(&mut sub)
        .await
        .expect("el plan sobrevivió");
    assert_eq!(r.total, 1);
}

/// Lo mismo con el filtro rápido: `Escape` lo cancela, y el plan sigue.
#[tokio::test]
async fn con_un_plan_en_vuelo_escape_cancela_el_filtro() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"ep1.mkv".to_vec(), false)]);
    f.plan_ia = Some(vec![("ep1.mkv".to_owned(), "ep01.mkv".to_owned())]);
    f.veredicto = Some(veredicto_ok(&pares));
    f.retraso_ia_ms = 120;
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;

    // `pane.quick-search` es `ctrl+s` en el preset ortodoxo; se llega por la
    // paleta para no depender de la tecla.
    por_la_paleta(&h, &mut sub, "quick-search").await;
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(
        listado(&foto).quick.is_none(),
        "el Escape canceló el filtro"
    );
    let r = siguiente_revision(&mut sub)
        .await
        .expect("el plan sobrevivió");
    assert_eq!(r.total, 1);
}

/// Descartar una revisión no mata una petición POSTERIOR.
///
/// Con una revisión abierta, el teclado es suyo — así que la única forma de
/// tener dos peticiones y una revisión a la vez es la real: se pide la
/// primera, se abre el prompt de la segunda mientras el modelo piensa, la
/// primera revisión aterriza DEBAJO de ese diálogo, se confirma la segunda
/// petición, y solo entonces se descarta la revisión que quedó a la vista.
/// Soltar ahí la petición en vuelo la mataba en silencio.
#[tokio::test]
async fn descartar_una_revision_no_mata_la_peticion_siguiente() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"ep1.mkv".to_vec(), false)]);
    f.plan_ia = Some(vec![("ep1.mkv".to_owned(), "ep01.mkv".to_owned())]);
    f.veredicto = Some(veredicto_ok(&pares));
    f.retraso_ia_ms = 120;
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // Petición 1, y el prompt de la 2 abierto mientras el modelo piensa.
    pedir_plan(&h, &mut sub).await;
    por_la_paleta(&h, &mut sub, "ai-rename").await;
    let id2 = siguientes_dialogos(&mut sub).await[0].id;

    // La revisión 1 aterriza DEBAJO del diálogo.
    let r1 = siguiente_revision(&mut sub).await.expect("la primera abre");
    assert_eq!(r1.total, 1);

    // Se confirma la petición 2 y se descarta la revisión 1.
    h.dispatch(UiAction::DialogInput {
        id: id2,
        text: "otra cosa".to_owned(),
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Dialog {
        id: id2,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    h.dispatch(tecla("Escape")).await.expect("host vivo");

    // Y la petición 2 sigue viva. La señal que NO se puede confundir con un
    // parche rezagado de la revisión 1 es que el core reciba un SEGUNDO
    // veredicto: solo lo pide un plan que llegó y se abrió.
    anotados(&backend, "el segundo veredicto", 2, |f| {
        f.veredictos_pedidos.lock().expect("veredictos").clone()
    })
    .await;
    assert_eq!(
        backend.instrucciones.lock().expect("instrucciones").len(),
        2
    );
}

/// La primera tecla que llega a la revisión solo la RECONOCE.
///
/// La pantalla se abre sola, decenas de segundos después del gesto que la
/// pidió, y se queda el teclado. Sin este paso, la `y` de quien estaba
/// tecleando `yes.txt` en el filtro rápido aprobaba el renombrado del
/// directorio entero.
#[tokio::test]
async fn la_primera_tecla_solo_reconoce_la_revision() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = falso_con_plan(&pares, Some(veredicto_ok(&pares)));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    let mut v = siguiente_revision(&mut sub).await.expect("abre");
    for _ in 0..40 {
        if v.confirmable {
            break;
        }
        v = siguiente_revision(&mut sub).await.expect("sigue abierta");
    }

    h.dispatch(tecla("y")).await.expect("host vivo");
    asentar().await;
    assert!(
        backend.lotes.lock().expect("lotes").is_empty(),
        "la tecla que venía en camino no aprueba nada"
    );
    h.dispatch(tecla("y")).await.expect("host vivo");
    anotados(&backend, "el lote que aprueba la segunda tecla", 1, |f| {
        f.lotes.lock().expect("lotes").clone()
    })
    .await;
    assert_eq!(
        backend.lotes.lock().expect("lotes").len(),
        1,
        "la segunda sí: ya es una respuesta"
    );
}

/// `Escape` NO necesita reconocimiento: descartar es seguro en los dos
/// estados, y quien no quiere esto tiene que poder quitárselo de encima a la
/// primera.
#[tokio::test]
async fn escape_descarta_la_revision_a_la_primera() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = falso_con_plan(&pares, Some(veredicto_ok(&pares)));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    siguiente_revision(&mut sub).await.expect("abre");

    h.dispatch(tecla("Escape")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.ai_rename.is_none(), "se fue a la primera");
}

/// Un acorde CON modificador no es una respuesta a esta pantalla.
#[tokio::test]
async fn un_acorde_con_modificador_no_aprueba_el_plan() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = falso_con_plan(&pares, Some(veredicto_ok(&pares)));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    siguiente_revision(&mut sub).await.expect("abre");
    // Reconocida, para que lo único que quede en pie sea el modificador.
    h.dispatch(tecla("j")).await.expect("host vivo");

    for (ctrl, shift) in [(true, false), (false, false)] {
        let ack = h
            .dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
                key: "y".to_owned(),
                ctrl,
                alt: false,
                shift,
                meta: true,
            }))
            .await
            .expect("host vivo");
        assert_eq!(
            ack,
            ActionAck::Unavailable {
                reason_key: "host-key-unmapped".to_owned()
            },
            "ctrl={ctrl}: {ack:?}"
        );
    }
    asentar().await;
    assert!(backend.lotes.lock().expect("lotes").is_empty());
}

/// No se aprueba un plan que no se ha recorrido ENTERO.
///
/// La ventana son cinco parejas de hasta doscientas cincuenta y seis: sin
/// esto, la pareja doscientos se ejecutaba sin que nadie la hubiera pintado
/// jamás, y la revisión es toda la defensa que hay contra un plan que un
/// modelo escribió a partir de nombres que controla quien escribe en el
/// directorio.
#[tokio::test]
async fn no_se_aprueba_un_plan_sin_recorrerlo_entero() {
    let pares: Vec<(String, String)> = (0..12)
        .map(|i| (format!("a{i:02}.mkv"), format!("b{i:02}.mkv")))
        .collect();
    let refs: Vec<(&str, &str)> = pares
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let backend = falso_con_plan(&refs, Some(veredicto_ok(&refs)));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    let mut v = siguiente_revision(&mut sub).await.expect("abre");
    for _ in 0..40 {
        if !v.status.is_empty() && v.total == 12 && v.more_note.contains("12") {
            break;
        }
        v = siguiente_revision(&mut sub).await.expect("sigue abierta");
    }
    assert!(!v.seen_all, "todavía no se ha visto entero");
    assert!(!v.confirmable, "y por eso no se puede aprobar");

    // Reconocer, e intentar aprobar sin haber bajado.
    h.dispatch(tecla("y")).await.expect("host vivo");
    let ack = h.dispatch(tecla("y")).await.expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "host-plan-unseen".to_owned()
        },
        "{ack:?}"
    );

    // Se recorre hasta el final y ya sí.
    for _ in 0..6 {
        h.dispatch(tecla("PageDown")).await.expect("host vivo");
    }
    asentar().await;
    h.dispatch(tecla("y")).await.expect("host vivo");
    anotados(&backend, "el lote aprobado", 1, |f| {
        f.lotes.lock().expect("lotes").clone()
    })
    .await;
    assert_eq!(backend.lotes.lock().expect("lotes").len(), 1);
}

/// Un nombre alterado FUERA de la ventana también se dice.
#[tokio::test]
async fn un_nombre_alterado_que_no_se_ve_tambien_se_dice() {
    let hostil =
        String::from_utf8(hostil("control_escape")).unwrap_or_else(|_| "\u{1b}x".to_owned());
    let mut pares: Vec<(String, String)> = (0..12)
        .map(|i| (format!("a{i:02}.mkv"), format!("b{i:02}.mkv")))
        .collect();
    // En la posición ONCE: fuera de la primera ventana de cinco.
    pares[11].1 = hostil;
    let refs: Vec<(&str, &str)> = pares
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let backend = falso_con_plan(&refs, None);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    let r = siguiente_revision(&mut sub).await.expect("abre");
    assert!(
        r.pairs.iter().all(|p| !p.to.hostile),
        "ninguna de las visibles está alterada"
    );
    assert!(
        r.hidden_hostile,
        "y aun así se dice que hay una que no se ve: {r:?}"
    );
}

/// La línea de «cuánto se ve» va TRADUCIDA, no como un patrón sin sustituir.
#[tokio::test]
async fn la_linea_de_cuanto_se_ve_va_traducida() {
    let pares: Vec<(String, String)> = (0..12)
        .map(|i| (format!("a{i:02}.mkv"), format!("b{i:02}.mkv")))
        .collect();
    let refs: Vec<(&str, &str)> = pares
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let backend = falso_con_plan(&refs, None);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    let r = siguiente_revision(&mut sub).await.expect("abre");
    assert!(
        !r.more_note.contains('$') && !r.more_note.contains('{'),
        "sin patrones sin sustituir: {}",
        r.more_note
    );
    assert!(
        r.more_note.contains("12"),
        "y con el total: {}",
        r.more_note
    );
}

/// Crear un directorio tampoco escribe el carácter que puso la pantalla.
#[tokio::test]
async fn crear_un_directorio_con_fffd_se_rechaza() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F7")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "caf\u{FFFD}".to_owned(),
    })
    .await
    .expect("host vivo");
    let ack = h
        .dispatch(UiAction::Dialog {
            id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "msg-transfer-name-fffd".to_owned()
        },
        "{ack:?}"
    );
    asentar().await;
    assert!(
        backend.creados.lock().expect("creados").is_empty(),
        "no se crea un directorio con el U+FFFD que inventó la pantalla"
    );
}

/// `Enter` NO aprueba el plan.
///
/// Rompe la paridad con el TUI a propósito: allí el plan lo abre una tecla del
/// lector y la siguiente es una respuesta. Aquí la pantalla se abre sola
/// decenas de segundos después, y `Enter` es justo la tecla con la que se
/// estaba recorriendo el árbol mientras el modelo pensaba — dos seguidos
/// entrando en directorios anidados son normales.
#[tokio::test]
async fn enter_no_aprueba_el_plan() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = falso_con_plan(&pares, Some(veredicto_ok(&pares)));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    let mut v = siguiente_revision(&mut sub).await.expect("abre");
    for _ in 0..40 {
        if v.confirmable {
            break;
        }
        v = siguiente_revision(&mut sub).await.expect("sigue abierta");
    }
    for _ in 0..3 {
        h.dispatch(tecla("Enter")).await.expect("host vivo");
    }
    asentar().await;
    assert!(
        backend.lotes.lock().expect("lotes").is_empty(),
        "ningún Enter aprueba un lote"
    );
}

/// Un clic en el botón SÍ contesta a la primera: es un gesto dirigido a esta
/// pantalla, no una tecla que iba a otro sitio.
#[tokio::test]
async fn el_boton_aprueba_sin_reconocimiento_previo() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = falso_con_plan(&pares, Some(veredicto_ok(&pares)));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    let mut v = siguiente_revision(&mut sub).await.expect("abre");
    for _ in 0..40 {
        if v.confirmable {
            break;
        }
        v = siguiente_revision(&mut sub).await.expect("sigue abierta");
    }
    let ack = h
        .dispatch(UiAction::AiRenameDecide { approve: true })
        .await
        .expect("host vivo");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    anotados(&backend, "el lote que aprueba el botón", 1, |f| {
        f.lotes.lock().expect("lotes").clone()
    })
    .await;
    assert_eq!(backend.lotes.lock().expect("lotes").len(), 1);
}

/// Y descartar con el botón cierra sin aplicar nada.
#[tokio::test]
async fn el_boton_de_descartar_cierra_sin_aplicar() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = falso_con_plan(&pares, Some(veredicto_ok(&pares)));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    siguiente_revision(&mut sub).await.expect("abre");
    h.dispatch(UiAction::AiRenameDecide { approve: false })
        .await
        .expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.ai_rename.is_none());
    assert!(backend.lotes.lock().expect("lotes").is_empty());
}

/// Un nombre de plan que EMPIEZA por la flecha no puede fingir ser el destino
/// de otra pareja.
///
/// Fixture `arrow_leading_row_spoof` del corpus: `arrow_join_spoof` pone la
/// flecha en medio y falsifica UNA pareja; esta la pone al principio y
/// falsifica el PAPEL de la fila. El papel lo lleva el campo —`from` o `to`—,
/// no el texto, así que el host manda los dos por separado y el renderer los
/// pone en elementos distintos.
#[tokio::test]
async fn un_nombre_que_empieza_por_la_flecha_no_finge_ser_un_destino() {
    let bytes = hostil("arrow_leading_row_spoof");
    let trampa = String::from_utf8(bytes).expect("el del corpus es UTF-8");
    let pares = [(trampa.as_str(), "ep02.mkv")];
    let backend = falso_con_plan(&pares, None);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    let r = siguiente_revision(&mut sub).await.expect("abre");
    // El nombre viaja ENTERO y en el campo que le toca: la flecha que lleva
    // dentro no lo convierte en un destino, porque el papel no está en el
    // texto.
    assert!(
        r.pairs[0].from.text.starts_with('\u{2192}'),
        "el nombre real empieza por la flecha: {:?}",
        r.pairs[0].from
    );
    assert_eq!(r.pairs[0].to.text, "ep02.mkv");
    assert_eq!(r.pairs.len(), 1, "una pareja, no dos: {:?}", r.pairs);
}

/// Un nombre cuya proyección NO cabe en pantalla no se puede editar aquí, y
/// se dice.
///
/// El recorte le pega una elipsis, y `…` es un carácter legal en un nombre:
/// ni se enmascara ni se marca. Editar el campo y confirmar escribiría el
/// recorte en el disco como parte del nombre. Fixture
/// `display_expansion_over_clamp`.
#[tokio::test]
async fn un_nombre_que_no_cabe_en_pantalla_no_se_edita() {
    let bytes = hostil("display_expansion_over_clamp");
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(bytes, false)]);
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let ack = h
        .dispatch(tecla_mod("F6", false, true))
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "host-name-not-editable".to_owned()
        },
        "{ack:?}"
    );
}

/// Inyecta una task AJENA con su propio cancelador, y devuelve por dónde
/// mandarle progreso.
///
/// Cada una lleva un cancelador que apunta SU id: un contador compartido dice
/// que se canceló algo, no CUÁL, y «cuál» es justo lo que un tablero con
/// cursor tiene que acertar.
fn inyectar_task(
    tx: &tokio::sync::mpsc::UnboundedSender<norte_ui_host::backend::HostTask>,
    id: u64,
    canceladas: &Arc<std::sync::Mutex<Vec<u64>>>,
) -> tokio::sync::watch::Sender<norte_proto::TaskProgress> {
    let progreso = norte_proto::TaskProgress {
        task_id: norte_proto::TaskId::new(id),
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
    let (ptx, prx) = tokio::sync::watch::channel(progreso);
    let canceladas = Arc::clone(canceladas);
    tx.send(norte_ui_host::backend::HostTask {
        id: norte_proto::TaskId::new(id),
        progress: prx,
        cancel: Arc::new(move || canceladas.lock().expect("canceladas").push(id)),
        foreign: true,
    })
    .expect("el host escucha");
    ptx
}

/// Espera el siguiente aviso con clave, sea cual sea.
async fn siguiente_aviso(sub: &mut norte_ui_host::controller::UiSubscription) -> String {
    for _ in 0..40 {
        match tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv())
            .await
            .expect("llega")
            .expect("el host sigue vivo")
        {
            Update::Message(m) => {
                if let UiUpdate::Notice(UiNotice::Message { key, .. }) = &m.payload {
                    return key.clone();
                }
            }
            Update::Lagged => {}
        }
    }
    panic!("no llegó ningún aviso");
}

/// `Ctrl+K` para la task viva: hasta ahora el catálogo ataba la tecla y el
/// host respondía `NotHere`, así que una copia lanzada desde la ventana solo
/// se podía parar matando la ventana.
#[tokio::test]
async fn la_tecla_de_cancelar_para_la_task_viva() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    let (h, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = h.subscribe();
    let canceladas = Arc::new(std::sync::Mutex::new(Vec::new()));
    let _p = inyectar_task(&tx, 11, &canceladas);
    siguientes_tasks(&mut sub).await;

    let ack = h
        .dispatch(tecla_mod("k", true, false))
        .await
        .expect("host vivo");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    assert_eq!(
        *canceladas.lock().expect("canceladas"),
        vec![11],
        "se le pidió parar a la task viva"
    );
    assert_eq!(siguiente_aviso(&mut sub).await, "msg-cancelling");
}

/// Sin nada en marcha, cancelar no es un error ni un silencio: se dice.
#[tokio::test]
async fn cancelar_sin_tasks_lo_dice() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let ack = h
        .dispatch(tecla_mod("k", true, false))
        .await
        .expect("host vivo");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    assert_eq!(siguiente_aviso(&mut sub).await, "msg-no-tasks");
}

/// Una task ya TERMINADA sigue en el tablero, y cancelarla no es cancelar
/// nada: se busca una viva, y si no la hay se dice.
#[tokio::test]
async fn una_task_terminada_no_es_la_que_se_cancela() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    let (h, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = h.subscribe();
    let canceladas = Arc::new(std::sync::Mutex::new(Vec::new()));
    let p = inyectar_task(&tx, 11, &canceladas);
    siguientes_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    siguientes_tasks(&mut sub).await;

    h.dispatch(tecla_mod("k", true, false))
        .await
        .expect("host vivo");
    assert!(
        canceladas.lock().expect("canceladas").is_empty(),
        "a una task terminada no se le pide parar"
    );
    assert_eq!(siguiente_aviso(&mut sub).await, "msg-no-tasks");
}

/// Con el panel de procesos enfocado se cancela la del CURSOR, no la última.
///
/// Es la misma regla que el panel ya tenía para moverse: si la lista que se
/// ve tiene cursor y la tecla cancela otra cosa, el tablero pinta una
/// selección que no manda.
#[tokio::test]
async fn con_el_panel_enfocado_se_cancela_la_del_cursor() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    let (h, _snap) = host_con_layout(Arc::new(falso), "full", (200, 60)).await;
    let mut sub = h.subscribe();
    let canceladas = Arc::new(std::sync::Mutex::new(Vec::new()));
    let _a = inyectar_task(&tx, 11, &canceladas);
    let _b = inyectar_task(&tx, 12, &canceladas);
    // Las dos en el tablero antes de tocar el cursor.
    for _ in 0..2 {
        if siguientes_tasks(&mut sub).await.len() == 2 {
            break;
        }
    }

    h.dispatch(UiAction::FocusSlot { slot_id: 7 })
        .await
        .expect("host vivo");
    // El tablero va por id, así que la segunda fila es la 12.
    h.dispatch(tecla("Down")).await.expect("host vivo");
    h.dispatch(tecla_mod("k", true, false))
        .await
        .expect("host vivo");
    assert_eq!(
        *canceladas.lock().expect("canceladas"),
        vec![12],
        "la del cursor, no la última"
    );
}

/// Igual que [`inyectar_task`], pero eligiendo la CLASE: el informe de un
/// lote solo se pide para un lote.
fn inyectar_task_de(
    tx: &tokio::sync::mpsc::UnboundedSender<norte_ui_host::backend::HostTask>,
    id: u64,
    kind: norte_proto::TaskKind,
) -> tokio::sync::watch::Sender<norte_proto::TaskProgress> {
    let progreso = norte_proto::TaskProgress {
        task_id: norte_proto::TaskId::new(id),
        kind,
        state: norte_proto::TaskState::Running,
        bytes_done: 0,
        bytes_total: None,
        entries_done: 0,
        entries_total: None,
        current: None,
        unreadable: None,
        unvisited: None,
    };
    let (ptx, prx) = tokio::sync::watch::channel(progreso);
    tx.send(norte_ui_host::backend::HostTask {
        id: norte_proto::TaskId::new(id),
        progress: prx,
        cancel: Arc::new(|| {}),
        foreign: false,
    })
    .expect("el host escucha");
    ptx
}

/// Un informe limpio: N aplicados y nada más.
fn informe_limpio(n: u64) -> norte_proto::methods::FsRenameBatchReportResult {
    norte_proto::methods::FsRenameBatchReportResult {
        applied: n,
        rolled_back: 0,
        failed_pair: None,
        stuck: None,
        uncertain: None,
        compensations_lost: 0,
    }
}

/// Espera a que el tablero traiga una task con `detail` puesto.
async fn detalle_de_task(sub: &mut norte_ui_host::controller::UiSubscription) -> String {
    for _ in 0..40 {
        let tasks = siguientes_tasks(sub).await;
        if let Some(d) = tasks.first().and_then(|t| t.detail.clone()) {
            return d;
        }
    }
    panic!("ninguna task trajo detalle");
}

/// Un lote que termina PIDE su informe: es la única señal de que el
/// directorio se quedó a medias, y hasta ahora no lo pedía nadie (#272).
#[tokio::test]
async fn un_lote_terminado_pide_su_informe() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    *falso.informe.lock().expect("informe") = Some(informe_limpio(3));
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let p = inyectar_task_de(&tx, 31, norte_proto::TaskKind::RenameBatch);
    siguientes_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    let detalle = detalle_de_task(&mut sub).await;
    assert_eq!(
        *backend.informes_pedidos.lock().expect("informes"),
        vec![31],
        "se pidió el informe del lote"
    );
    assert!(detalle.contains('3'), "el tablero dice cuántos: {detalle}");
    // Un lote limpio no interrumpe: no hay nada que decidir ni que buscar.
    assert!(
        !hubo_dialogos(&mut sub).await,
        "un lote limpio no abre nada"
    );
}

/// `true` si en lo que queda por leer llega algún diálogo.
async fn hubo_dialogos(sub: &mut norte_ui_host::controller::UiSubscription) -> bool {
    while let Ok(Some(u)) =
        tokio::time::timeout(std::time::Duration::from_millis(150), sub.recv()).await
    {
        if let Update::Message(m) = u
            && let UiUpdate::Patch(p) = &m.payload
            && p.changes
                .iter()
                .any(|c| matches!(c, norte_ui_host::dto::ViewChange::Dialogs { .. }))
        {
            return true;
        }
    }
    false
}

/// Una copia que termina NO pide informe de lote: el informe es de los lotes.
#[tokio::test]
async fn una_copia_no_pide_informe_de_lote() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let p = inyectar_task_de(&tx, 32, norte_proto::TaskKind::Copy);
    siguientes_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    siguientes_tasks(&mut sub).await;
    asentar().await;
    assert!(
        backend
            .informes_pedidos
            .lock()
            .expect("informes")
            .is_empty()
    );
}

/// Un lote ATASCADO abre una superficie que lo dice, y dice CÓMO SE LLAMA
/// AHORA el fichero: sin ese nombre, «se quedó a medias» no se puede actuar.
#[tokio::test]
async fn un_lote_atascado_lo_dice_y_da_el_nombre_de_ahora() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    *falso.informe.lock().expect("informe") =
        Some(norte_proto::methods::FsRenameBatchReportResult {
            applied: 4,
            rolled_back: 2,
            failed_pair: Some(2),
            stuck: Some(norte_proto::methods::RenameStuckStep {
                from: VPath::parse("mem:///casa/viejo.txt").expect("vpath"),
                to: VPath::parse("mem:///casa/nuevo.txt").expect("vpath"),
                pair_index: 2,
                error: norte_proto::Error::Io { retryable: false },
                journalled: true,
                still_applied: 2,
            }),
            uncertain: None,
            compensations_lost: 1,
        });
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let p = inyectar_task_de(&tx, 33, norte_proto::TaskKind::RenameBatch);
    siguientes_tasks(&mut sub).await;
    p.send_modify(|p| {
        p.state = norte_proto::TaskState::Failed {
            error: norte_proto::Error::Io { retryable: false },
        };
    });

    let dialogos = siguientes_dialogos(&mut sub).await;
    let cuerpo: String = dialogos[0]
        .body
        .iter()
        .map(|l| l.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        cuerpo.contains("nuevo.txt"),
        "dice cómo se llama AHORA: {cuerpo}"
    );
    // Y la marca de compensaciones perdidas no se calla: un undo de sesión se
    // va a parar justo ahí.
    assert!(cuerpo.contains('1'), "{cuerpo}");
}

/// Un daemon que NO sabe informar de un lote fallido no se degrada en
/// silencio: se dice que el desenlace se quedó sin comprobar.
#[tokio::test]
async fn un_informe_que_no_se_puede_pedir_se_dice() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    // Sin informe: el falso contesta `Unsupported`.
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let p = inyectar_task_de(&tx, 34, norte_proto::TaskKind::RenameBatch);
    siguientes_tasks(&mut sub).await;
    p.send_modify(|p| {
        p.state = norte_proto::TaskState::Failed {
            error: norte_proto::Error::Io { retryable: false },
        };
    });

    let dialogos = siguientes_dialogos(&mut sub).await;
    assert_eq!(dialogos[0].title_key, "modal-batch-report-title");
    // Y dice EXACTAMENTE que el daemon no sabe informar: «no se pudo pedir»
    // y «este daemon no sabe» son dos cosas distintas, y confundirlas es
    // degradar en silencio con más palabras.
    assert_eq!(
        dialogos[0].body[0].text,
        norte_i18n::t_in(norte_i18n::Lang::Es, "modal-batch-unsupported"),
        "{:?}",
        dialogos[0].body
    );
}

/// Espera el siguiente estado de la barra que traiga avisos persistentes.
async fn siguientes_banners(
    sub: &mut norte_ui_host::controller::UiSubscription,
) -> Vec<norte_ui_host::dto::BannerView> {
    for _ in 0..40 {
        match tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv())
            .await
            .expect("llega")
            .expect("el host sigue vivo")
        {
            Update::Message(m) => {
                if let UiUpdate::Patch(p) = &m.payload {
                    for c in &p.changes {
                        if let norte_ui_host::dto::ViewChange::Status(s) = c
                            && !s.banners.is_empty()
                        {
                            return s.banners.clone();
                        }
                    }
                }
            }
            Update::Lagged => {}
        }
    }
    panic!("no llegó ningún aviso persistente");
}

/// Una sesión que viaja SIN cifrar deja un aviso persistente que la NOMBRA.
///
/// Un mensaje efímero no vale: lo borra la siguiente tecla, y esto es un
/// hecho de toda la sesión. La ventana lo pintaba de ninguna manera —el
/// canal existía en el SDK y el host no lo tomaba— así que un FTP en claro
/// se leía igual que un SFTP.
#[tokio::test]
async fn una_sesion_en_claro_deja_aviso_persistente() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.degradadas.lock().expect("degradadas") = Some(rx);
    let (h, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = h.subscribe();
    tx.send(norte_proto::methods::ConnectionDegraded {
        scheme: "ftp".to_owned(),
        host: "archivo.example".to_owned(),
        reason: "ftp-plaintext".to_owned(),
        detail: None,
    })
    .expect("el host escucha");

    let banners = siguientes_banners(&mut sub).await;
    assert!(
        banners.iter().any(|b| b
            .subject
            .as_ref()
            .is_some_and(|s| s.host == "archivo.example")),
        "el aviso nombra la conexión, en su propio campo: {banners:?}"
    );
}

// ---------------------------------------------------------------------------
// El panel de registro (#326).
// ---------------------------------------------------------------------------

/// Emite unas líneas DENTRO del anillo, por su camino de verdad.
///
/// Por la capa de `tracing` y no por un `push` directo: el anillo no expone
/// uno, y no debe — el filtro por el que pasa la capa es donde vive la cota de
/// `suppaftp`, que loguea `PASS <contraseña>` a nivel TRACE. Un atajo para los
/// tests que se saltara esa cota probaría un camino que no existe.
fn con_lineas(anillo: &norte_config::logring::LogRing, f: impl FnOnce()) {
    use tracing_subscriber::layer::SubscriberExt as _;
    let s = tracing_subscriber::registry().with(norte_config::logring::ring_layer(anillo));
    tracing::subscriber::with_default(s, f);
}

/// Un host con un anillo de registro montado y unas cuantas líneas dentro.
async fn host_con_registro() -> (UiHost, norte_config::logring::LogRing) {
    host_con_backend_y_registro(Falso::con(&["a"])).await
}

/// Lo mismo, con un doble que el test ha armado: es lo que hace falta para
/// la mitad remota (#328), donde lo que se prueba es qué contesta el daemon.
async fn host_con_backend_y_registro(
    backend: Arc<Falso>,
) -> (UiHost, norte_config::logring::LogRing) {
    let anillo = norte_config::logring::LogRing::new(64);
    // A DEBUG para que las cinco quepan; el panel enseña hasta INFO al abrirse,
    // que es lo que hace interesante el test del filtro por nivel.
    anillo.set_level(norte_config::logline::LogLevel::Debug);
    let h = host_con_backend_y_anillo(backend, Some(anillo.clone())).await;
    (h, anillo)
}

/// Y lo mismo SIN anillo en este proceso: nadie montó la capa de `tracing`.
///
/// No es un caso de laboratorio —es lo que ve la ventana cuando el anillo no
/// se instala— y es el que decide si «los dos» puede anunciarse sobre una
/// lista que es entera del daemon.
async fn host_con_backend_y_anillo(
    backend: Arc<Falso>,
    anillo: Option<norte_config::logring::LogRing>,
) -> UiHost {
    let h = UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("orthodox").expect("layout"),
        viewport: (120, 40),
        settings: norte_ui_host::ajustes_por_defecto(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: anillo,
    })
    .await
    .expect("arranca")
    .0;
    // La disposición de arranque no lleva registro: se abre con su tecla, que
    // es como lo abre una persona. Y así el test cubre TAMBIÉN que
    // `layout.log` esté atado y llegue al efecto.
    tecla_registro(&h).await;
    h
}

/// La tecla que abre el registro — y, pulsada otra vez, lo cierra.
async fn tecla_registro(h: &UiHost) {
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "l".to_owned(),
        ctrl: false,
        alt: true,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");
}

/// El hueco de registro de la foto, si está.
fn registro(snap: &norte_ui_host::ViewSnapshot) -> &norte_ui_host::dto::LogSlotView {
    snap.slots
        .iter()
        .find_map(|s| match s {
            SlotView::Log(l) => Some(&**l),
            _ => None,
        })
        .expect("hay un hueco de registro")
}

/// #326: la ventana PINTA el registro, con su nivel, su filtro y su origen.
///
/// Antes caía a «kind no soportado», en gris: abrir un hueco que solo se pinta
/// apagado no es abrirlo. Y el panel dice de qué PROCESO son las líneas,
/// porque la ventana arranca su propio daemon y las suyas no son las de él —
/// callarlo haría que el panel pareciera roto.
#[tokio::test]
async fn la_ventana_pinta_el_registro() {
    let (h, anillo) = host_con_registro().await;
    con_lineas(&anillo, || {
        tracing::info!(target: "norte_prueba", "una linea de prueba");
    });
    let mut sub = h.subscribe();

    let vista = foto_hasta(&h, &mut sub, "el panel de registro", |f| {
        f.slots
            .iter()
            .find_map(|s| match s {
                SlotView::Log(l) => Some((**l).clone()),
                _ => None,
            })
            .filter(|l| !l.lines.is_empty())
    })
    .await;
    assert_eq!(vista.level, "info", "abre en INFO, como el anillo");
    assert!(vista.following, "nace pegado al final");
    assert_eq!(
        vista.source,
        norte_i18n::t_in(norte_i18n::Lang::Es, "log-source-window"),
        "dice de qué proceso son las líneas"
    );
    assert!(
        vista.lines.iter().any(|l| l.message.contains("prueba")),
        "la línea que se acaba de emitir está: {:?}",
        vista.lines
    );
    let _ = anillo;
}

/// El panel enseña las filas que el RENDERER dice que caben, no una.
///
/// El host arranca con una —nunca cero, para que una página mueva algo— y
/// espera a que le digan el alto. Mientras nadie se lo decía, un panel de doce
/// filas pintaba UNA línea recortada y la rueda se saltaba dos por muesca: el
/// mismo defecto que en la TUI se arregló dejando de adivinar el viewport.
#[tokio::test]
async fn el_registro_ensena_las_filas_que_le_dicen_que_caben() {
    let (h, anillo) = host_con_registro().await;
    con_lineas(&anillo, || {
        for i in 0..8 {
            tracing::info!(target: "norte_prueba", n = i, "linea");
        }
    });
    let mut sub = h.subscribe();

    h.dispatch(UiAction::LogSetVisibleRange { rows: 6 })
        .await
        .expect("host vivo");
    let vista = foto_hasta(&h, &mut sub, "seis filas", |f| {
        let l = registro(f).clone();
        (l.lines.len() == 6).then_some(l)
    })
    .await;
    assert_eq!(vista.lines.len(), 6);
    assert_eq!(vista.total, 8, "las ocho pasan el filtro; se ven seis");
}

/// Un `rows` disparatado se ACOTA: la webview no decide cuánto pesa una foto.
///
/// Sin techo, un `rows` de cuatro mil millones hace que cada foto lleve el
/// anillo entero — dos mil líneas por acción, que es justo lo que la decisión
/// D7 existe para impedir. El camino del listado ya se acotaba igual.
#[tokio::test]
async fn un_alto_disparatado_no_manda_el_anillo_entero() {
    let (h, anillo) = host_con_registro().await;
    con_lineas(&anillo, || {
        for i in 0..40 {
            tracing::info!(target: "norte_prueba", n = i, "linea");
        }
    });
    let mut sub = h.subscribe();

    h.dispatch(UiAction::LogSetVisibleRange { rows: u32::MAX })
        .await
        .expect("host vivo");
    asentar().await;
    let vista = foto_hasta(&h, &mut sub, "el registro acotado", |f| {
        Some(registro(f).clone())
    })
    .await;
    assert!(
        vista.lines.len() <= 512,
        "viajaron {} líneas: el techo no se aplicó",
        vista.lines.len()
    );
}

/// Cerrar el panel BAJA lo que el proceso captura.
///
/// El nivel del anillo se sube en caliente para poder enseñar más, y solo
/// sube. Sin esto, una sola pulsación de «traza» dejaba el proceso guardando
/// TRACE en memoria el resto de la sesión —con la cota de `suppaftp` como
/// única barrera— y la interfaz diciendo «info», sin ningún panel donde verlo.
#[tokio::test]
async fn cerrar_el_panel_baja_lo_que_se_captura() {
    let (h, anillo) = host_con_registro().await;
    h.dispatch(UiAction::LogSetLevel {
        level: "trace".to_owned(),
    })
    .await
    .expect("host vivo");
    asentar().await;
    assert_eq!(anillo.level(), norte_config::logline::LogLevel::Trace);

    // Y mientras esté abierto, el panel DICE que se captura más de lo que
    // enseña: una captura de pantalla que dijera «info» sobre un proceso
    // guardando TRACE sería una respuesta falsa.
    h.dispatch(UiAction::LogSetLevel {
        level: "info".to_owned(),
    })
    .await
    .expect("host vivo");
    let mut sub = h.subscribe();
    let vista = foto_hasta(&h, &mut sub, "el aviso de captura", |f| {
        let l = registro(f).clone();
        (!l.capturing.is_empty()).then_some(l)
    })
    .await;
    assert!(vista.capturing.contains("trace"), "{}", vista.capturing);

    // Cerrarlo con la misma tecla que lo abrió.
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "l".to_owned(),
        ctrl: false,
        alt: true,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");
    asentar().await;
    assert_eq!(
        anillo.level(),
        norte_config::logline::LogLevel::Info,
        "cerrar el panel deja de capturar lo que ya no se enseña"
    );
}

/// Pedir DEBUG SUBE el nivel del anillo, y bajar a ERROR no deja de capturar.
///
/// Las dos mitades importan y las dos son de `LogPanel`: filtrar en la
/// pantalla lo que nunca se registró es imposible, así que pedir DEBUG tiene
/// que hacer que el anillo empiece a capturarlo; y si bajar dejara de
/// capturar, volver a subir enseñaría un agujero del tamaño del rato que se
/// estuvo abajo.
#[tokio::test]
async fn el_nivel_del_panel_sube_el_del_anillo_y_no_lo_baja() {
    let (h, anillo) = host_con_registro().await;
    anillo.set_level(norte_config::logline::LogLevel::Info);

    h.dispatch(UiAction::LogSetLevel {
        level: "debug".to_owned(),
    })
    .await
    .expect("host vivo");
    asentar().await;
    assert_eq!(
        anillo.level(),
        norte_config::logline::LogLevel::Debug,
        "pedir DEBUG hace que el anillo lo capture"
    );

    h.dispatch(UiAction::LogSetLevel {
        level: "error".to_owned(),
    })
    .await
    .expect("host vivo");
    asentar().await;
    assert_eq!(
        anillo.level(),
        norte_config::logline::LogLevel::Debug,
        "bajar lo que se ENSEÑA no deja de capturar"
    );
}

/// Un nivel que no existe se DICE; no cae en `info`.
#[tokio::test]
async fn un_nivel_de_registro_desconocido_no_cae_en_otro() {
    let (h, _anillo) = host_con_registro().await;
    let ack = h
        .dispatch(UiAction::LogSetLevel {
            level: "verboso".to_owned(),
        })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "host-log-level-unknown".to_owned()
        }
    );
}

/// El filtro recorta, y despegarse del final se DICE.
///
/// «No pasa nada» y «te has despegado y esto es historia» son indistinguibles
/// sin decirlo, y eso es la mitad de para qué sirve el panel: uno que salta
/// siempre al final no se puede leer mientras algo escribe.
#[tokio::test]
async fn el_filtro_recorta_y_despegarse_se_dice() {
    let (h, anillo) = host_con_registro().await;
    con_lineas(&anillo, || {
        tracing::info!(target: "norte_prueba", "aguja");
        tracing::info!(target: "norte_prueba", "pajar uno");
        tracing::info!(target: "norte_prueba", "pajar dos");
    });
    let mut sub = h.subscribe();

    h.dispatch(UiAction::LogSetFilter {
        filter: "aguja".to_owned(),
    })
    .await
    .expect("host vivo");
    let vista = foto_hasta(&h, &mut sub, "el registro filtrado", |f| {
        let l = registro(f).clone();
        (l.filter == "aguja").then_some(l)
    })
    .await;
    assert_eq!(vista.total, 1, "solo la que casa: {:?}", vista.lines);

    // Y despegarse: subir por el registro deja de seguir el final.
    h.dispatch(UiAction::LogSetFilter {
        filter: String::new(),
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::LogScroll { delta: -1 })
        .await
        .expect("host vivo");
    let vista = foto_hasta(&h, &mut sub, "el registro despegado", |f| {
        let l = registro(f).clone();
        (!l.following).then_some(l)
    })
    .await;
    assert!(!vista.following);

    h.dispatch(UiAction::LogFollow).await.expect("host vivo");
    let vista = foto_hasta(&h, &mut sub, "el registro pegado", |f| {
        let l = registro(f).clone();
        l.following.then_some(l)
    })
    .await;
    assert!(vista.following, "volver al final se puede pedir");
}

// ---------------------------------------------------------------------------
// El panel de registro lee TAMBIÉN el daemon (#328).
// ---------------------------------------------------------------------------

/// Una línea tal y como viene por el cable.
fn linea_wire(
    epoch_ms: i64,
    level: &str,
    target: &str,
    message: &str,
) -> norte_proto::methods::LogLine {
    norte_proto::methods::LogLine {
        epoch_ms,
        level: level.to_owned(),
        target: target.to_owned(),
        message: message.to_owned(),
    }
}

/// La foto del panel de registro, con todo lo que estuviera en vuelo ya
/// aterrizado.
///
/// `asentar` primero: la respuesta del daemon vuelve al actor por el MISMO
/// buzón que las acciones, así que cuando el ejecutor se queda quieto el
/// mensaje ya está encolado y el `Resync` de `foto_hasta` va detrás. Sin
/// reloj y sin adivinar.
async fn foto_registro(h: &UiHost) -> norte_ui_host::dto::LogSlotView {
    // Con alto de verdad: el host arranca con UNA fila —nunca cero, para que
    // una página mueva algo— y con una fila la ventana visible es la última
    // línea, así que una lista mezclada se vería como la mitad de la que hay.
    h.dispatch(UiAction::LogSetVisibleRange { rows: 20 })
        .await
        .expect("host vivo");
    let mut sub = h.subscribe();
    asentar().await;
    foto_hasta(h, &mut sub, "el panel de registro", |f| {
        Some(registro(f).clone())
    })
    .await
}

/// Dispara UNA vuelta más del sondeo de 500 ms.
///
/// Adelantar el reloj y no dormirlo: el plazo es de VERDAD —el temporizador
/// que el panel se rearma solo— y ésa es exactamente la herramienta que la
/// nota de las esperas deterministas de este fichero señala para un plazo.
async fn sondear(h: &UiHost) {
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_millis(600)).await;
    tokio::time::resume();
    asentar().await;
    let _ = h;
}

/// Deja la FUENTE del panel en la que se pide.
///
/// A base del mando de verdad, que es UN solo botón que recorre las tres
/// (`Both` → `Window` → `Daemon` → `Both`): no hay una acción «pon ésta», y
/// fabricar una solo para los tests probaría un camino que nadie usa.
async fn poner_fuente(h: &UiHost, fuente: &str) {
    // Primero se deja aterrizar la respuesta del daemon: el mando NO recorre
    // mientras no se sepa que hay una segunda fuente —mover la preferencia por
    // debajo de un lector que no puede verla moverse es lo que se arregló—, así
    // que pulsarlo antes del primer `log.tail` no haría nada.
    asentar().await;
    let vueltas = match fuente {
        "window" => 1,
        "daemon" => 2,
        "both" => 3,
        otra => panic!("fuente desconocida: {otra}"),
    };
    for _ in 0..vueltas {
        h.dispatch(UiAction::LogCycleSource)
            .await
            .expect("host vivo");
    }
    asentar().await;
}

/// Con un daemon que no sabe de registro no hay dos anillos, así que no hay
/// selector que enseñar: el panel se queda exactamente como en #326.
#[tokio::test]
async fn embebido_no_ofrece_selector_de_fuente() {
    let (host, _anillo) = host_con_registro().await;
    let v = foto_registro(&host).await;
    assert!(!v.sources_available);
    assert_eq!(v.source_mode, "window");
}

/// Con daemon, el panel trae las líneas de los DOS y cada una dice de dónde es.
#[tokio::test]
async fn con_daemon_se_mezclan_las_dos_fuentes() {
    let backend = Falso::con(&["a"]);
    backend.responde_log_tail(vec![linea_wire(20, "info", "norte_core", "del daemon")], 1);
    let (host, anillo) = host_con_backend_y_registro(Arc::clone(&backend)).await;
    con_lineas(&anillo, || {
        tracing::info!(target: "norte_prueba", "de la ventana");
    });
    let v = foto_registro(&host).await;
    assert!(v.sources_available);
    assert_eq!(v.source_mode, "both");
    let textos: Vec<_> = v.lines.iter().map(|l| l.message.as_str()).collect();
    assert!(
        textos.iter().any(|t| t.contains("del daemon")),
        "faltan las del daemon: {textos:?}"
    );
    assert!(
        textos.iter().any(|t| t.contains("de la ventana")),
        "faltan las de la ventana: {textos:?}"
    );
    // Y cada una dice de dónde salió: en una lista mezclada, «esto lo escribió
    // el daemon» es la mitad de la información.
    let del_daemon = v
        .lines
        .iter()
        .find(|l| l.message.contains("del daemon"))
        .expect("está");
    assert_eq!(del_daemon.source, "daemon");
    let de_la_ventana = v
        .lines
        .iter()
        .find(|l| l.message.contains("de la ventana"))
        .expect("está");
    assert_eq!(de_la_ventana.source, "window");
    // Y en «los dos», que es como nace el panel, YA se dice de quién es el
    // nivel: es el camino corriente, y por él pulsar «traza» sube un anillo
    // global al daemon que no vuelve a bajar y que cerrar este panel no baja.
    // Decirlo solo con el daemon como única fuente dejaba sin anunciar
    // justamente la vez que más pasa.
    assert_eq!(
        v.source_note,
        norte_i18n::t_in(norte_i18n::Lang::Es, "log-source-daemon-level"),
        "el camino corriente también avisa de qué nivel se está tocando"
    );
}

/// Sin anillo en ESTA ventana, la fuente cae al DAEMON.
///
/// El espejo del caso embebido: allí falta el anillo de enfrente y todo cae a
/// `Window`; aquí falta el de aquí. Sin esto, un `Both` sobre un proceso que
/// nunca montó la capa se anunciaba como «de la ventana y del daemon» siendo
/// la lista entera del daemon.
#[tokio::test]
async fn sin_anillo_local_la_fuente_cae_al_daemon() {
    let backend = Falso::con(&["a"]);
    backend.responde_log_tail(vec![linea_wire(20, "info", "norte_core", "del daemon")], 1);
    let host = host_con_backend_y_anillo(Arc::clone(&backend), None).await;
    let v = foto_registro(&host).await;
    assert_eq!(v.source_mode, "daemon");
    assert_eq!(
        v.source,
        norte_i18n::t_in(norte_i18n::Lang::Es, "log-source-daemon")
    );
    assert!(
        v.lines.iter().any(|l| l.message.contains("del daemon")),
        "y se enseñan las suyas: {:?}",
        v.lines
    );
}

/// Un daemon que no sabe servir su registro NO deja el panel mudo: vuelve al
/// anillo local y lo DICE. Es la mitad que #326 ya resolvió, aplicada al único
/// caso alcanzable: un daemon de la MISMA versión compilado sin la feature
/// `logging`. Uno más viejo no llega aquí — muere en el `initialize`.
#[tokio::test]
async fn un_daemon_sin_registro_se_dice_en_el_panel() {
    let backend = Falso::con(&["a"]);
    backend.log_tail_no_soportado();
    let (host, _anillo) = host_con_backend_y_registro(Arc::clone(&backend)).await;
    let v = foto_registro(&host).await;
    let preguntas = backend.cursores_pedidos().len();
    assert!(preguntas > 0, "se llegó a preguntar");
    assert_eq!(v.source_mode, "window");
    assert!(!v.source_note.is_empty(), "tiene que decir por qué");

    // Y no se le vuelve a preguntar. Esa negativa no puede cambiar mientras
    // ese daemon viva —sale de una feature de compilación o de un montaje que
    // falló al arrancar—, así que seguir sondeando eran dos RPC por segundo,
    // para siempre, por una respuesta que no puede ser otra.
    sondear(&host).await;
    sondear(&host).await;
    assert_eq!(
        backend.cursores_pedidos().len(),
        preguntas,
        "a un daemon sin registro no se le repregunta"
    );
}

/// Y tampoco se le pide el NIVEL: es la otra mitad de la misma regla.
///
/// La ventana lo pedía por la FUENTE sola, así que contra un daemon que ya
/// había contestado `Unsupported` cada pulsación de nivel mandaba un `log.level`
/// cuya respuesta ya se conocía — un RPC por tecla, para siempre. La TUI ya
/// exigía las dos condiciones y decía por qué; ahora es la misma regla en las
/// dos.
#[tokio::test]
async fn a_un_daemon_sin_registro_no_se_le_pide_el_nivel() {
    let backend = Falso::con(&["a"]);
    backend.log_tail_no_soportado();
    let (host, _anillo) = host_con_backend_y_registro(Arc::clone(&backend)).await;
    // La premisa: ya contestó que no tiene anillo que servir.
    let v = foto_registro(&host).await;
    assert!(
        !v.sources_available,
        "el daemon ya dijo que no tiene anillo"
    );

    // Y la preferencia del panel sigue siendo la de la apertura («los dos»),
    // que es lo que hacía que la condición de la fuente se cumpliera sola.
    for nivel in ["debug", "trace", "warn"] {
        host.dispatch(UiAction::LogSetLevel {
            level: (*nivel).to_owned(),
        })
        .await
        .expect("host vivo");
    }
    asentar().await;
    assert!(
        backend.log_level_pedidos().is_empty(),
        "un RPC muerto por pulsación: {:?}",
        backend.log_level_pedidos()
    );
}

/// Sin daemon que sirva, el mando de fuente no mueve la PREFERENCIA.
///
/// Hoy no se ve —la fuente efectiva colapsa a «esta ventana» de todos modos, y
/// el renderer ni pinta el selector—, y por eso es justo el que se cuela: la
/// preferencia se movía a espaldas de un lector que no podía verla moverse, y
/// reaparecía puesta en otra cosa la primera vez que sí hubiera daemon
/// sirviendo. Se comprueba por ese camino: se pulsa con la respuesta retenida
/// y se suelta después.
#[tokio::test]
async fn sin_segunda_fuente_el_mando_no_mueve_la_preferencia() {
    let mut f = Falso::default();
    f.pon("mem:///casa", [(b"a".to_vec(), false)]);
    let puerta = Arc::new(backend_falso::Puerta::default());
    f.puerta_registro = Some(Arc::clone(&puerta));
    let backend = Arc::new(f);
    backend.responde_log_tail(vec![linea_wire(10, "info", "norte_core", "del daemon")], 1);
    let (host, _anillo) = host_con_backend_y_registro(Arc::clone(&backend)).await;

    // Con la respuesta retenida no se sabe todavía si hay una segunda fuente.
    let v = foto_registro(&host).await;
    assert!(!v.sources_available, "aún no ha contestado nadie");

    // Dos vueltas del mando: sin guarda dejarían la preferencia en «daemon».
    for _ in 0..2 {
        host.dispatch(UiAction::LogCycleSource)
            .await
            .expect("host vivo");
    }
    asentar().await;

    // Ahora sí contesta, y aparece el selector: la preferencia tiene que
    // seguir siendo la de la apertura.
    puerta.abrir();
    let v = foto_registro(&host).await;
    assert!(v.sources_available, "ahora sirve su registro");
    assert_eq!(
        v.source_mode, "both",
        "el mando movió la preferencia sin que nadie pudiera verlo"
    );
}

/// El nivel se le pide AL DAEMON, pero el que la cabecera marca es el que se
/// ENSEÑA — y el del daemon se dice aparte, como captura de más.
///
/// Las dos mitades son la misma trampa vista por sus dos caras. El cliente no
/// aplica niveles: la cota que impide que ahí dentro aparezca una contraseña
/// vive en el proceso que tiene el anillo, así que pedir es todo lo que se
/// puede hacer. Y lo que la cabecera marca tiene que seguir siendo lo que se
/// enseña, porque es lo que FILTRA la lista y lo que los botones controlan:
/// marcar ahí el nivel del daemon —que es global a sus clientes, que otro pudo
/// subir y que nunca baja— dejaba `trace` encendido mientras el panel tiraba en
/// silencio cada línea `debug` que llegaba por el cable, y pulsar `info` no
/// movía la marca. Un mando que no mueve lo que marca se lee como roto.
#[tokio::test]
async fn el_nivel_del_daemon_se_pide_y_se_dice_aparte() {
    let backend = Falso::con(&["a"]);
    // Otro cliente ya subió el anillo del daemon a `trace`. Es global y solo
    // sube, así que pedirle `info` no lo baja: contesta el que tiene.
    backend.responde_log_tail(Vec::new(), 0);
    backend.log_level_contesta("trace");
    let (host, _anillo) = host_con_backend_y_registro(Arc::clone(&backend)).await;
    poner_fuente(&host, "daemon").await;
    host.dispatch(UiAction::LogSetLevel {
        level: "info".to_owned(),
    })
    .await
    .expect("host vivo");
    let pedidos = hasta(&backend, "el nivel pedido al daemon", |f| {
        let v = f.log_level_pedidos();
        (!v.is_empty()).then_some(v)
    })
    .await;
    assert_eq!(pedidos, vec!["info".to_owned()], "se le PIDE al daemon");

    let v = foto_registro(&host).await;
    assert_eq!(v.source_mode, "daemon");
    assert_eq!(
        v.level, "info",
        "la cabecera marca lo que se ENSEÑA, que es lo que filtra la lista"
    );
    // Y el del daemon no se calla: sale donde ya vive «se recoge más de lo que
    // se ve», y ahí SÍ dice de quién es el anillo.
    assert_eq!(
        v.capturing,
        norte_i18n::ta_in(
            norte_i18n::Lang::Es,
            "log-capturing-daemon",
            &[("level", "trace")]
        ),
        "el anillo del daemon guarda más de lo que este panel enseña"
    );
    assert!(
        !v.source_note.is_empty(),
        "y dice de QUIÉN es ese nivel: es global al daemon"
    );
}

/// El sondeo encadena el cursor: la segunda vuelta pide desde donde acabó la
/// primera y no repite líneas.
#[tokio::test]
async fn el_sondeo_encadena_el_cursor() {
    let backend = Falso::con(&["a"]);
    backend.responde_log_tail(vec![linea_wire(10, "info", "norte_core", "primera")], 1);
    let (host, _anillo) = host_con_backend_y_registro(Arc::clone(&backend)).await;
    let v = foto_registro(&host).await;
    assert!(v.lines.iter().any(|l| l.message.contains("primera")));

    backend.responde_log_tail(vec![linea_wire(20, "info", "norte_core", "segunda")], 2);
    sondear(&host).await;
    let cursores = backend.cursores_pedidos();
    assert_eq!(cursores[0], None, "la primera vuelta pide «lo que haya»");
    assert!(
        cursores[1..].iter().all(Option::is_some),
        "ninguna vuelta posterior vuelve a pedir «lo que haya»: {cursores:?}"
    );
    assert_eq!(cursores[1], Some(1), "la segunda encadena donde acabó");
    let v = foto_registro(&host).await;
    let textos: Vec<_> = v.lines.iter().map(|l| l.message.as_str()).collect();
    assert_eq!(
        textos.iter().filter(|t| t.contains("primera")).count(),
        1,
        "la primera línea no se repite: {textos:?}"
    );
    assert!(textos.iter().any(|t| t.contains("segunda")), "{textos:?}");
}

/// Una respuesta que sigue volando cuando el panel se cierra NO entra en el
/// panel que se vuelve a abrir.
///
/// Es la pregunta que se hace sola en cuanto la petición es asíncrona: entre
/// pedir y contestar caben un cierre y una apertura, y unas líneas de la
/// sesión anterior aterrizando en el panel nuevo serían historia que nadie
/// pidió, delante de la que sí. La ÉPOCA de la apertura viaja con la petición
/// y es lo que la deja morir — el mismo mecanismo que ya apaga el
/// temporizador.
#[tokio::test]
async fn una_respuesta_en_vuelo_no_entra_en_el_panel_reabierto() {
    let mut f = Falso::default();
    f.pon("mem:///casa", [(b"a".to_vec(), false)]);
    let puerta = Arc::new(backend_falso::Puerta::default());
    f.puerta_registro = Some(Arc::clone(&puerta));
    let backend = Arc::new(f);
    backend.responde_log_tail(
        vec![linea_wire(10, "info", "norte_core", "de la apertura vieja")],
        1,
    );
    let (host, _anillo) = host_con_backend_y_registro(Arc::clone(&backend)).await;
    // La petición de la primera apertura sigue retenida: se cierra y se
    // vuelve a abrir por debajo de ella.
    tecla_registro(&host).await;
    tecla_registro(&host).await;
    puerta.abrir();

    let v = foto_registro(&host).await;
    assert!(
        !v.lines
            .iter()
            .any(|l| l.message.contains("de la apertura vieja")),
        "la respuesta de la apertura anterior no entra: {:?}",
        v.lines
    );
}

/// Entra en `docs`, que es la navegación que dispara el listado remoto.
async fn entrar_en_docs(h: &UiHost, snap: &norte_ui_host::ViewSnapshot) {
    let docs = listado(snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("el directorio está");
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs.key,
        generation: listado(snap).generation,
    })
    .await
    .expect("host vivo");
}

/// Arma el doble para que el SIGUIENTE listado pida la contraseña (#327).
///
/// Después de arrancar el host, no antes: el listado del arranque se llevaría
/// la petición y la ventana nacería con el panel en error, que es otro caso.
fn pedira_el_secreto(f: &Falso) {
    *f.pide_secreto.lock().expect("pide_secreto") = Some(norte_proto::Error::SecretNeeded {
        conn: "rosetta".to_owned(),
        endpoint: "s3://cubo.example".to_owned(),
    });
}

/// Un hueco que arranca pidiendo la contraseña NO pregunta solo, pero DICE
/// cuál y se puede reintentar — y el reintento sí pregunta.
///
/// Es el caso de reabrir norte: el daemon anterior se apagó por inactividad y
/// se llevó el secreto de sesión, así que el panel guardado sobre `s3://…`
/// vuelve con `SecretNeeded`. El arranque no abre el diálogo a propósito
/// —restaurar una sesión no es pedir conectarse, y una contraseña pedida antes
/// de que la pantalla exista es la forma que el ADR 0015 llama phishing— pero
/// tampoco puede dejar un panel parado sin decir qué le pasa.
#[tokio::test]
async fn un_hueco_que_pide_secreto_dice_cual_y_el_reintento_pregunta() {
    let backend = Arc::new(arbol_como_falso());
    pedira_el_secreto(&backend);
    // El listado del ARRANQUE es el que se topa con el error, así que la
    // avería se arma antes de construir el host.
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    let SlotView::Browser(b) = &snap.slots[0] else {
        panic!("el primer hueco es un listado");
    };
    let norte_ui_host::dto::SlotState::Error { reason_key, detail } = &b.state else {
        panic!("el hueco se queda en error, no fingiendo un directorio vacío");
    };
    assert_eq!(reason_key, "err-secret-needed");
    assert_eq!(
        detail.as_deref(),
        Some("rosetta"),
        "y CUÁL: con dos paneles remotos, «hace falta un secreto» no es \
         contestable"
    );
    // Sin diálogo: el arranque no pregunta solo.
    assert!(
        snap.dialogs.is_empty(),
        "el arranque no abre la pregunta: la abre el primer gesto"
    );

    // El reintento SÍ la abre, porque es un gesto. Se rearma la avería: el
    // secreto sigue faltando —nadie lo ha entregado— y el doble la consume de
    // una en una.
    pedira_el_secreto(&backend);
    h.dispatch(UiAction::RefreshSlot { slot_id: 1 })
        .await
        .expect("host vivo");
    let dialogos = siguientes_dialogos(&mut sub).await;
    assert_eq!(
        dialogos.last().map(|d| d.title_key.as_str()),
        Some("modal-ask-secret-title"),
        "reintentar es el gesto que convierte el panel parado en la pregunta"
    );
}

/// #327: la ventana PREGUNTA la contraseña en vez de pintar el error.
///
/// Hasta ahora un usuario de `norte-gui` sobre una conexión `secret = "prompt"`
/// veía el texto de `err-secret-needed` —que nombra una variable de entorno— y
/// ahí se acababa el camino. La TUI abría un diálogo desde #325: el mismo
/// hueco de paridad que ADR 0077 existe para no dejar abierto.
#[tokio::test]
async fn la_ventana_pide_el_secreto_y_reintenta_la_navegacion() {
    let backend = Arc::new(arbol_como_falso());
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    pedira_el_secreto(&backend);
    let mut sub = h.subscribe();
    // Entrar en el directorio dispara el listado que pide el secreto.
    entrar_en_docs(&h, &snap).await;

    let dialogos = siguientes_dialogos(&mut sub).await;
    let d = dialogos.last().expect("el diálogo se abrió");
    assert_eq!(d.title_key, "modal-ask-secret-title");
    // La pregunta dice A DÓNDE va la contraseña, y no solo cómo se llama la
    // entrada: el nombre lo eligió un fichero, y un fichero se edita.
    assert_eq!(
        d.destination.as_ref().map(|l| l.text.as_str()),
        Some("s3://cubo.example"),
        "sin el destino la pregunta no es contestable"
    );
    assert_eq!(d.subject.as_ref().map(|l| l.text.as_str()), Some("rosetta"));
    assert!(d.input_secret, "el campo es una contraseña");
    assert_eq!(d.input.as_deref(), Some(""), "nace vacío");

    // Teclear por el camino de un NOMBRE no hace nada sobre este diálogo: el
    // host no guarda contraseñas, y un renderer que las mandara por ahí
    // estaría metiendo material secreto por la vía de un nombre de fichero.
    let ack = h
        .dispatch(UiAction::DialogInput {
            id: d.id,
            text: "s3cr3t".to_owned(),
        })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Modal
        },
        "un campo de contraseña no se teclea por `dialog_input`"
    );

    // Confirmar entrega el secreto TAL CUAL y reintenta ESA navegación. Va
    // CON la respuesta: cruza una vez, en el instante en que se decide.
    h.dispatch(UiAction::Dialog {
        id: d.id,
        choice: "confirm".to_owned(),
        secret: Some("s3cr3t".to_owned()),
    })
    .await
    .expect("host vivo");

    let dados = anotados(&backend, "el secreto entregado", 1, |f| {
        f.secretos_dados.lock().expect("secretos_dados").clone()
    })
    .await;
    assert_eq!(
        dados[0],
        ("rosetta".to_owned(), "s3cr3t".to_owned()),
        "llega entero y a la conexión que lo pidió"
    );

    // Y el panel acaba DONDE iba: entregar la contraseña sin reanudar la
    // navegación dejaría al lector con el secreto dado y el panel quieto.
    let dir = foto_hasta(&h, &mut sub, "el panel entró", |f| {
        let SlotView::Browser(b) = f.slots.first()? else {
            return None;
        };
        b.path_display
            .ends_with("/casa/docs")
            .then(|| b.path_display.clone())
    })
    .await;
    assert!(dir.ends_with("/casa/docs"), "{dir}");
}

/// Confirmar con el campo VACÍO es inerte: ni entrega, ni cierra.
///
/// Entregar la cadena vacía reproduce #320 —un secreto vacío hace que la
/// conexión autentique con la cadena ambiente, o sea con una identidad que
/// nadie pidió— y cerrar convertiría un dedo que se adelanta en una navegación
/// abandonada.
#[tokio::test]
async fn confirmar_sin_teclear_nada_no_entrega_ni_cierra() {
    let backend = Arc::new(arbol_como_falso());
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    pedira_el_secreto(&backend);
    let mut sub = h.subscribe();
    entrar_en_docs(&h, &snap).await;
    let dialogos = siguientes_dialogos(&mut sub).await;
    let id = dialogos.last().expect("el diálogo se abrió").id;

    // Sin acuse previo: lo abrió la navegación del lector, así que la primera
    // respuesta ya es una respuesta. Y con el campo vacío, no hace nada.
    let ack = h
        .dispatch(UiAction::Dialog {
            id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "host-secret-empty".to_owned()
        },
        "el confirmar de un campo de contraseña vacío es inerte"
    );

    asentar().await;
    assert!(
        backend
            .secretos_dados
            .lock()
            .expect("secretos_dados")
            .is_empty(),
        "no se entregó NADA: la cadena vacía es #320"
    );
    // Y el diálogo sigue delante: responder con un `Stale` querría decir que
    // se cerró.
    let ack = h
        .dispatch(UiAction::Dialog {
            id,
            choice: "cancel".to_owned(),
            secret: None,
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "el diálogo seguía abierto: {ack:?}"
    );
}

/// Una contraseña que no cabe se RECHAZA, no se recorta.
///
/// Recortar era peor que el tope: entregar los primeros 256 caracteres de una
/// frase de paso más larga falla la autenticación sin decir por qué, y el
/// lector no puede sospecharlo porque el campo va enmascarado.
#[tokio::test]
async fn una_contrasena_que_no_cabe_se_rechaza() {
    let backend = Arc::new(arbol_como_falso());
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    pedira_el_secreto(&backend);
    let mut sub = h.subscribe();
    entrar_en_docs(&h, &snap).await;
    let dialogos = siguientes_dialogos(&mut sub).await;
    let id = dialogos.last().expect("el diálogo se abrió").id;

    let ack = h
        .dispatch(UiAction::Dialog {
            id,
            choice: "confirm".to_owned(),
            secret: Some("x".repeat(257)),
        })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "host-secret-too-long".to_owned()
        }
    );
    asentar().await;
    assert!(
        backend
            .secretos_dados
            .lock()
            .expect("secretos_dados")
            .is_empty(),
        "no se entregó una contraseña a medias"
    );
}

/// Dos paneles sobre la misma conexión NO apilan dos preguntas iguales.
///
/// Cada una traía su propio campo vacío, y bajo suficientes de ellas el
/// desalojo por tope de la pila se lleva por delante las aprobaciones de
/// agente sin reconocer, que es lo primero que sacrifica.
#[tokio::test]
async fn dos_listados_de_la_misma_conexion_no_apilan_dos_preguntas() {
    let backend = Arc::new(arbol_como_falso());
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    pedira_el_secreto(&backend);
    let mut sub = h.subscribe();
    entrar_en_docs(&h, &snap).await;
    let dialogos = siguientes_dialogos(&mut sub).await;
    assert_eq!(dialogos.len(), 1);

    // Otra navegación al mismo sitio, y otra vez sin secreto.
    pedira_el_secreto(&backend);
    h.dispatch(UiAction::Parent { slot_id: 1 })
        .await
        .expect("host vivo");
    asentar().await;
    let foto = foto_hasta(&h, &mut sub, "la pila estable", |f| Some(f.dialogs.len())).await;
    assert_eq!(foto, 1, "una pregunta por conexión, no una por listado");
}

/// Cerrar el diálogo abandona la navegación, como el TOFU: no se entrega nada
/// y el hueco se queda con el error que ya sabía explicarse.
#[tokio::test]
async fn cancelar_el_secreto_abandona_la_navegacion() {
    let backend = Arc::new(arbol_como_falso());
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    pedira_el_secreto(&backend);
    let mut sub = h.subscribe();
    entrar_en_docs(&h, &snap).await;
    let dialogos = siguientes_dialogos(&mut sub).await;
    let id = dialogos.last().expect("el diálogo se abrió").id;

    h.dispatch(UiAction::Dialog {
        id,
        choice: "cancel".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    asentar().await;
    assert!(
        backend
            .secretos_dados
            .lock()
            .expect("secretos_dados")
            .is_empty(),
        "cancelar no entrega nada"
    );
    let motivo = foto_hasta(&h, &mut sub, "el hueco en error", |f| {
        let SlotView::Browser(b) = f.slots.first()? else {
            return None;
        };
        match &b.state {
            norte_ui_host::dto::SlotState::Error { reason_key, .. } => Some(reason_key.clone()),
            _ => None,
        }
    })
    .await;
    assert_eq!(
        motivo, "err-secret-needed",
        "detrás del diálogo queda la pantalla que ya sabía explicarse"
    );
}

/// #322: una conexión que NO se abre dice POR QUÉ, y con la frase concreta.
///
/// Sin esto el fallo llegaba como la categoría del error —`PermissionDenied`—
/// que no distingue un secreto vacío de una clave equivocada ni de un bucket
/// sin permisos. La frase exacta se quedaba en el log del daemon.
///
/// Y llega como aviso EFÍMERO, no como banner: la degradación describe una
/// sesión que sigue abierta mientras se mira; esto, un intento que terminó.
#[tokio::test]
async fn una_conexion_que_falla_dice_por_que() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.fallidas.lock().expect("fallidas") = Some(rx);
    let (h, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = h.subscribe();
    tx.send(norte_proto::methods::ConnectionFailed {
        conn: Some("rosetta".to_owned()),
        scheme: "s3".to_owned(),
        host: "cubo.example".to_owned(),
        reason: "secret-empty".to_owned(),
        detail: Some("el secreto de «rosetta» está definido pero VACÍO".to_owned()),
    })
    .expect("el host escucha");

    // Sobre la PANTALLA, no sobre el sobre del puente. El renderer solo
    // atiende los `Notice` de clase `fatal` y su texto de estado sale de
    // `status.message`: un test que afirmara sobre el aviso se ponía verde con
    // la ventana sin pintar nada, que es justo lo que pasó.
    let detalle = foto_hasta(&h, &mut sub, "el fallo en la barra", |f| {
        f.status
            .message
            .clone()
            .filter(|m| m.contains("cubo.example"))
    })
    .await;
    assert!(
        detalle.contains("cubo.example"),
        "el aviso nombra la máquina a la que no se entró: {detalle}"
    );
    assert!(
        detalle.contains("rosetta"),
        "y el nombre de connections.toml, que es el que el humano escribió: {detalle}"
    );
    assert!(
        detalle.contains(&norte_i18n::t_in(
            norte_i18n::Lang::Es,
            "failed-reason-secret-empty"
        )),
        "y el MOTIVO traducido, que es lo que #322 existe para que cruce: {detalle}"
    );
    assert!(
        !detalle.contains("s3://"),
        "la autoridad va etiquetada, jamás como URL: {detalle}"
    );

    // Y el aviso viaja TAMBIÉN, con la misma línea: un frontend que sí atienda
    // los `Notice` no depende de haber leído la foto.
    let mut sub2 = h.subscribe();
    tx.send(norte_proto::methods::ConnectionFailed {
        conn: None,
        scheme: "s3".to_owned(),
        host: "otro.example".to_owned(),
        reason: "auth-rejected".to_owned(),
        detail: None,
    })
    .expect("el host escucha");
    let aviso = foto_hasta_notice(&mut sub2, "status-connection-failed").await;
    assert!(aviso.contains("otro.example"), "{aviso}");
}

/// El vocabulario de fallos también puede CRECER, y uno desconocido no puede
/// heredar la frase del de al lado: se apoya en `detail`, como pide el proto.
#[tokio::test]
async fn un_fallo_de_motivo_desconocido_se_apoya_en_el_detalle() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.fallidas.lock().expect("fallidas") = Some(rx);
    let (h, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = h.subscribe();
    tx.send(norte_proto::methods::ConnectionFailed {
        conn: None,
        scheme: "sftp".to_owned(),
        host: "maquina.example".to_owned(),
        reason: "algo-que-no-existia".to_owned(),
        detail: Some("el servidor pidió un método que norte no tiene".to_owned()),
    })
    .expect("el host escucha");

    let detalle = foto_hasta(&h, &mut sub, "el fallo desconocido en la barra", |f| {
        f.status
            .message
            .clone()
            .filter(|m| m.contains("maquina.example"))
    })
    .await;
    assert!(
        detalle.contains("el servidor pidió un método que norte no tiene"),
        "sin motivo conocido, el detalle es lo único que orienta: {detalle}"
    );
    assert!(
        !detalle.contains(&norte_i18n::t_in(
            norte_i18n::Lang::Es,
            "failed-reason-auth-rejected"
        )),
        "un motivo desconocido no hereda la frase de otro: {detalle}"
    );
}

/// Espera el siguiente aviso con esta clave y devuelve su detalle.
async fn foto_hasta_notice(
    sub: &mut norte_ui_host::controller::UiSubscription,
    clave: &str,
) -> String {
    for _ in 0..40 {
        match tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv())
            .await
            .expect("llega")
            .expect("el host sigue vivo")
        {
            Update::Message(m) => {
                if let UiUpdate::Notice(UiNotice::Message { key, detail }) = &m.payload
                    && key == clave
                {
                    return detail.clone().unwrap_or_default();
                }
            }
            Update::Lagged => {}
        }
    }
    panic!("no llegó el aviso {clave}");
}

/// **Un motivo que este binario no conoce no se lee como «FTP en claro»**
/// (#279). El vocabulario del wire puede crecer, y antes de esto un daemon más
/// nuevo informando de una degradación NUEVA producía exactamente la misma
/// frase: un aviso de seguridad afirmando una causa que nadie había dicho.
#[tokio::test]
async fn un_motivo_desconocido_no_se_pinta_como_el_conocido() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.degradadas.lock().expect("degradadas") = Some(rx);
    let (h, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = h.subscribe();
    tx.send(norte_proto::methods::ConnectionDegraded {
        scheme: "sftp".to_owned(),
        host: "archivo.example".to_owned(),
        reason: "algo-que-no-existia".to_owned(),
        detail: Some("el servidor negoció un perfil antiguo".to_owned()),
    })
    .expect("el host escucha");

    let banners = siguientes_banners(&mut sub).await;
    let subject = banners
        .iter()
        .find_map(|b| b.subject.as_ref())
        .expect("el aviso nombra la conexión");
    assert_eq!(
        subject.reason,
        norte_i18n::t_in(norte_i18n::Lang::Es, "degraded-reason-unknown"),
        "un motivo desconocido lo dice: {subject:?}"
    );
    assert_ne!(
        subject.reason,
        norte_i18n::t_in(norte_i18n::Lang::Es, "degraded-reason-ftp-plaintext"),
    );
    assert_eq!(
        subject.detail.as_deref(),
        Some("el servidor negoció un perfil antiguo"),
        "y se apoya en `detail`, que es lo que el proto pide"
    );
}

/// El daemon que avisa de que se PARA lo dice, y lo dice de forma persistente:
/// «reconectando…» sobre un daemon que no vuelve es una espera falsa.
#[tokio::test]
async fn un_daemon_que_se_para_lo_dice() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.eventos.lock().expect("eventos") = Some(rx);
    let (h, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = h.subscribe();
    tx.send(norte_client::ConnEvent::GoingAway { reconnect: false })
        .expect("el host escucha");

    let banners = siguientes_banners(&mut sub).await;
    assert_eq!(
        banners.iter().map(|b| b.text.clone()).collect::<Vec<_>>(),
        vec![norte_i18n::t_in(
            norte_i18n::Lang::Es,
            "msg-daemon-stopping"
        )],
    );
}

/// Un relevo NO es una parada, y se dice distinto: uno vuelve y el otro no.
#[tokio::test]
async fn un_relevo_no_se_lee_como_una_parada() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.eventos.lock().expect("eventos") = Some(rx);
    let (h, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = h.subscribe();
    tx.send(norte_client::ConnEvent::GoingAway { reconnect: true })
        .expect("el host escucha");
    let banners = siguientes_banners(&mut sub).await;
    assert_eq!(
        banners.iter().map(|b| b.text.clone()).collect::<Vec<_>>(),
        vec![norte_i18n::t_in(
            norte_i18n::Lang::Es,
            "msg-daemon-handover"
        )],
    );

    // Y cuando vuelve, el aviso se apaga: un aviso que no sabe volverse
    // «ya está» miente en cuanto el daemon reaparece.
    tx.send(norte_client::ConnEvent::Restored)
        .expect("el host escucha");
    for _ in 0..40 {
        if let Ok(Some(Update::Message(m))) =
            tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv()).await
            && let UiUpdate::Patch(p) = &m.payload
            && let Some(norte_ui_host::dto::ViewChange::Status(s)) = p
                .changes
                .iter()
                .find(|c| matches!(c, norte_ui_host::dto::ViewChange::Status(_)))
            && s.banners.is_empty()
        {
            return;
        }
    }
    panic!("el aviso del daemon no se apagó al volver");
}

/// Una mutación que el daemon RECHAZA por no poder abrir el journal deja
/// aviso persistente: «no se registra» es un hecho de toda la sesión, y la
/// regla dura 4 dice que sin registro no se muta.
#[tokio::test]
async fn una_mutacion_sin_journal_deja_aviso() {
    let falso = arbol_como_falso();
    *falso.error_al_borrar.lock().expect("error") = Some(norte_proto::Error::JournalUnavailable);
    let backend = Arc::new(falso);
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

    let banners = siguientes_banners(&mut sub).await;
    assert_eq!(
        banners.iter().map(|b| b.text.clone()).collect::<Vec<_>>(),
        vec![norte_i18n::t_in(
            norte_i18n::Lang::Es,
            "status-journal-refused"
        )],
    );
}

/// Una aprobación que llega DOS veces no abre dos diálogos.
///
/// No es hipotético: el SDK resincroniza `policy.pending` en cada
/// reconexión, así que una aprobación que sigue viva vuelve por el canal.
/// Dos diálogos para la misma decisión son dos respuestas, y la segunda cae
/// sobre un `approval_id` que el daemon ya cerró.
#[tokio::test]
async fn una_aprobacion_repetida_no_abre_dos_dialogos() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.aprobaciones.lock().expect("aprobaciones") = Some(rx);
    let (host, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = host.subscribe();

    let peticion = |ttl: u64| norte_proto::methods::PolicyApprovalRequired {
        approval_id: 5,
        session: Some("agente-1".to_owned()),
        op: "delete".to_owned(),
        paths: vec!["mem:///casa/x".to_owned()],
        paths_total: 1,
        ttl_ms: ttl,
        detail: norte_proto::methods::ApprovalDetail::default(),
    };
    tx.send(peticion(30_000)).expect("el host escucha");
    assert_eq!(siguientes_dialogos(&mut sub).await.len(), 1);
    // La misma, reconstruida por el resync: sin TTL, porque `policy.pending`
    // no lo transporta.
    tx.send(peticion(0)).expect("el host escucha");
    asentar().await;
    assert!(
        !hubo_dialogos(&mut sub).await,
        "la repetida no abre nada nuevo"
    );
}

/// Una aprobación CADUCA: el daemon deja de aceptarla, así que su diálogo se
/// cierra solo y se dice.
///
/// Un diálogo que sigue delante después del TTL invita a aprobar en el vacío:
/// se pulsa aprobar, el daemon contesta que ese id ya no existe, y el agente
/// lleva rato denegado. Peor todavía si mientras tanto el humano se creyó que
/// lo había autorizado.
#[tokio::test]
async fn una_aprobacion_caduca_y_su_dialogo_se_cierra() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.aprobaciones.lock().expect("aprobaciones") = Some(rx);
    let (host, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = host.subscribe();

    tx.send(norte_proto::methods::PolicyApprovalRequired {
        approval_id: 7,
        session: None,
        op: "delete".to_owned(),
        paths: vec!["mem:///casa/x".to_owned()],
        paths_total: 1,
        ttl_ms: 60,
        detail: norte_proto::methods::ApprovalDetail::default(),
    })
    .expect("el host escucha");
    let abiertos = siguientes_dialogos(&mut sub).await;
    assert_eq!(abiertos.len(), 1);

    let vacios = siguientes_dialogos(&mut sub).await;
    assert!(vacios.is_empty(), "el diálogo se cerró solo: {vacios:?}");
}

/// Un undo que termina PIDE su informe y lo enseña.
///
/// El desenlace de la Task dice si el undo corrió; lo que NO volvió lo dice
/// solo el informe, y un undo que paró a mitad deja el árbol en un estado
/// que nadie más va a contar.
#[tokio::test]
async fn un_undo_terminado_pide_su_informe_y_dice_lo_que_no_volvio() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    *falso.informe_undo.lock().expect("informe undo") =
        Some(norte_proto::methods::PolicyUndoReportResult {
            undone: 3,
            skipped_irreversible: 1,
            skipped_created_no_trash: 0,
            blocked: Some(norte_proto::methods::UndoBlocked {
                seq: 42,
                error: norte_proto::Error::Conflict {
                    conflict: norte_proto::ConflictKind::Exists,
                },
            }),
            batch_stuck: None,
            compensations_lost: 0,
            denied: Vec::new(),
            denied_total: 0,
        });
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let p = inyectar_task_de(&tx, 51, norte_proto::TaskKind::Undo);
    siguientes_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    let dialogos = siguientes_dialogos(&mut sub).await;
    assert_eq!(
        *backend.informes_undo_pedidos.lock().expect("pedidos"),
        vec![51]
    );
    let cuerpo: String = dialogos[0]
        .body
        .iter()
        .map(|l| l.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        cuerpo.contains("42"),
        "cita la entrada donde paró: {cuerpo}"
    );
    assert_eq!(dialogos[0].title_key, "modal-undo-report-title");
}

/// #250 — un empaquetado que COMPLETA pide su informe y dice lo que guardó que
/// significa otra cosa fuera.
#[tokio::test]
async fn un_empaquetado_terminado_dice_los_nombres_que_significan_otra_cosa() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    *falso.informe_pack.lock().expect("informe pack") =
        Some(norte_proto::methods::ArchivePackReportResult {
            entries: 9,
            checked: vec!["separator".to_owned()],
            risky: vec![norte_proto::methods::PackRiskyName {
                path: "a%5Cb.txt".to_owned(),
                name: "a\\b.txt".to_owned(),
                risk: "separator".to_owned(),
            }],
            truncated: false,
        });
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let p = inyectar_task_de(&tx, 77, norte_proto::TaskKind::Pack);
    siguientes_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    for _ in 0..40 {
        if let Ok(Some(Update::Message(m))) =
            tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv()).await
            && let UiUpdate::Patch(p) = &m.payload
            && let Some(norte_ui_host::dto::ViewChange::Status(s)) = p
                .changes
                .iter()
                .find(|c| matches!(c, norte_ui_host::dto::ViewChange::Status(_)))
            && s.message.as_deref().is_some_and(|t| t.contains('1'))
        {
            assert_eq!(
                *backend.informes_pack_pedidos.lock().expect("pedidos"),
                vec![77]
            );
            return;
        }
    }
    panic!("un empaquetado con un nombre hostil dentro no dijo nada");
}

/// Y un empaquetado CANCELADO no dice nada, porque no hay archivo del que
/// hablar (hallazgo del `protocol-guardian`).
///
/// El informe existe igual —se calcula antes de escribir el primer byte—, y la
/// cancelación deja el destino LIMPIO. Pintarlo diría «empaquetado, pero…»
/// sobre algo que nadie empaquetó, y además haría a la ventana decir una cosa
/// que la TUI no dice (ADR 0077).
#[tokio::test]
async fn un_empaquetado_cancelado_no_avisa_de_un_archivo_que_no_existe() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    *falso.informe_pack.lock().expect("informe pack") =
        Some(norte_proto::methods::ArchivePackReportResult {
            entries: 9,
            checked: vec!["separator".to_owned()],
            risky: vec![norte_proto::methods::PackRiskyName {
                path: "a%5Cb.txt".to_owned(),
                name: "a\\b.txt".to_owned(),
                risk: "separator".to_owned(),
            }],
            truncated: false,
        });
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let p = inyectar_task_de(&tx, 78, norte_proto::TaskKind::Pack);
    siguientes_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Cancelled);

    // Lo que se afirma es que NO lo pide: se deja correr todo lo que el
    // desenlace de la task pudiera haber encolado, y se mira después.
    asentar().await;
    assert!(
        backend
            .informes_pack_pedidos
            .lock()
            .expect("pedidos")
            .is_empty(),
        "de un empaquetado cancelado no hay archivo del que avisar"
    );
}

/// Un undo limpio no interrumpe: el tablero lo dice y ya.
#[tokio::test]
async fn un_undo_limpio_no_abre_nada() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    *falso.informe_undo.lock().expect("informe undo") =
        Some(norte_proto::methods::PolicyUndoReportResult {
            undone: 4,
            skipped_irreversible: 0,
            skipped_created_no_trash: 0,
            blocked: None,
            batch_stuck: None,
            compensations_lost: 0,
            denied: Vec::new(),
            denied_total: 0,
        });
    let (h, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = h.subscribe();
    let p = inyectar_task_de(&tx, 52, norte_proto::TaskKind::Undo);
    siguientes_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    let detalle = detalle_de_task(&mut sub).await;
    assert!(detalle.contains('4'), "el tablero dice cuántas: {detalle}");
    assert!(!hubo_dialogos(&mut sub).await);
}

/// El aviso de «sin journal» se APAGA cuando el daemon vuelve a aceptar una
/// mutación.
///
/// Un indicador que no sabe volverse «ya sí» miente sobre lo único que
/// describe de toda la sesión, y es la misma lección que el TUI aprendió en
/// el #179: la ventana de propiedad de `journal.db` se reabre sola cuando el
/// ocupante de paso lo suelta. Aquí no hay una notificación que lo anuncie,
/// así que la prueba es la que hay: una mutación que el daemon ACEPTA.
#[tokio::test]
async fn el_aviso_de_journal_se_apaga_cuando_vuelve_a_aceptarse_una_mutacion() {
    let falso = arbol_como_falso();
    *falso.error_al_borrar.lock().expect("error") = Some(norte_proto::Error::JournalUnavailable);
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // Un borrado rechazado por el journal enciende el aviso.
    h.dispatch(tecla("F8")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    assert!(!siguientes_banners(&mut sub).await.is_empty());

    // El journal se arregla: la siguiente mutación entra.
    *backend.error_al_borrar.lock().expect("error") = None;
    h.dispatch(tecla("F8")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");

    for _ in 0..40 {
        if let Ok(Some(Update::Message(m))) =
            tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv()).await
            && let UiUpdate::Patch(p) = &m.payload
            && let Some(norte_ui_host::dto::ViewChange::Status(s)) = p
                .changes
                .iter()
                .find(|c| matches!(c, norte_ui_host::dto::ViewChange::Status(_)))
            && s.banners.is_empty()
        {
            return;
        }
    }
    panic!("el aviso de journal no se apagó al aceptarse una mutación");
}

/// El informe de un lote con un nombre HOSTIL dentro no lo pinta crudo, y
/// dice que lo enmascaró.
///
/// El nombre de ahora es lo único accionable del informe, así que es
/// exactamente donde un nombre con anulaciones bidi haría que quien lo lee
/// busque otro fichero.
#[tokio::test]
async fn el_informe_de_un_lote_enmascara_el_nombre_y_lo_dice() {
    let hostil_bytes = hostil("rtl_override");
    let nombre = String::from_utf8(hostil_bytes).expect("la fixture es UTF-8");
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    *falso.informe.lock().expect("informe") =
        Some(norte_proto::methods::FsRenameBatchReportResult {
            applied: 1,
            rolled_back: 0,
            failed_pair: Some(0),
            stuck: Some(norte_proto::methods::RenameStuckStep {
                from: VPath::parse("mem:///casa/antes.txt").expect("vpath"),
                to: VPath::parse(&format!("mem:///casa/{nombre}")).expect("vpath"),
                pair_index: 0,
                error: norte_proto::Error::Io { retryable: false },
                journalled: false,
                still_applied: 1,
            }),
            uncertain: None,
            compensations_lost: 0,
        });
    let (h, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = h.subscribe();
    let p = inyectar_task_de(&tx, 61, norte_proto::TaskKind::RenameBatch);
    siguientes_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    let dialogos = siguientes_dialogos(&mut sub).await;
    assert!(
        dialogos[0]
            .body
            .iter()
            .all(|l| !l.text.contains('\u{202E}')),
        "no se pinta crudo: {:?}",
        dialogos[0].body
    );
    assert!(
        dialogos[0].body.iter().any(|l| l.hostile),
        "y se DICE que lo pintado no es lo que hay: {:?}",
        dialogos[0].body
    );
}

/// Una reconexión que REANUNCIA un lote ya terminado no borra su informe.
///
/// El SDK vuelve a anunciar las tasks al reconectar, y el registro proyecta
/// la vista otra vez desde el progreso — que no sabe nada del informe. Sin
/// esto, la única señal de que el directorio se quedó a medias desaparecía
/// del tablero justo cuando la conexión se recupera, que es cuando el lector
/// vuelve a mirarlo.
#[tokio::test]
async fn un_reanuncio_no_borra_el_informe_del_lote() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    *falso.informe.lock().expect("informe") = Some(informe_limpio(2));
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let p = inyectar_task_de(&tx, 71, norte_proto::TaskKind::RenameBatch);
    siguientes_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    let detalle = detalle_de_task(&mut sub).await;

    // La misma task, reanunciada por el canal de ajenas como haría una
    // reconexión: ya terminal.
    let p2 = inyectar_task_de(&tx, 71, norte_proto::TaskKind::RenameBatch);
    p2.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    let tasks = siguientes_tasks(&mut sub).await;
    let t = tasks.iter().find(|t| t.task_id == 71).expect("sigue ahí");
    assert_eq!(t.detail.as_deref(), Some(detalle.as_str()), "{t:?}");
    assert_eq!(
        backend.informes_pedidos.lock().expect("pedidos").len(),
        1,
        "y no se vuelve a pedir"
    );
}

/// Una aprobación que NO llega al daemon se dice.
///
/// `policy.decide` se manda y se olvida, así que si el daemon se cayó entre
/// la pregunta y el sí, la ventana daba por autorizada una operación que va a
/// quedar denegada por silencio. En una superficie de seguridad, «lo dije» y
/// «llegó» no son lo mismo.
#[tokio::test]
async fn una_aprobacion_que_no_llega_al_daemon_se_dice() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.aprobaciones.lock().expect("aprobaciones") = Some(rx);
    *falso.error_al_decidir.lock().expect("error") =
        Some(norte_proto::Error::ProviderUnavailable { retryable: true });
    let (host, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = host.subscribe();

    tx.send(norte_proto::methods::PolicyApprovalRequired {
        approval_id: 12,
        session: Some("agente-1".to_owned()),
        op: "delete".to_owned(),
        paths: vec!["mem:///casa/x".to_owned()],
        paths_total: 1,
        ttl_ms: 30_000,
        detail: norte_proto::methods::ApprovalDetail::default(),
    })
    .expect("el host escucha");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    // Dos veces: la primera solo reconoce la superficie, que se abrió sola.
    for _ in 0..2 {
        host.dispatch(UiAction::Dialog {
            id,
            choice: "approve".to_owned(),
            secret: None,
        })
        .await
        .expect("host vivo");
    }

    for _ in 0..40 {
        if siguiente_aviso(&mut sub).await == "msg-approval-not-delivered" {
            return;
        }
    }
    panic!("nadie dijo que la aprobación no llegó");
}

/// Con el tablero RECORTADO, se cancela la fila que se ve.
///
/// El tablero cruza el puente acotado a `MAX_TASKS` y el cursor es un
/// índice. Mientras el recorte y el cursor contaban sobre listas distintas,
/// con más de 256 tasks —marcar tres mil ficheros y pulsar F5, y el desalojo
/// solo se lleva las TERMINADAS— la fila resaltada y la task que paraba eran
/// dos tasks distintas.
#[tokio::test]
async fn con_el_tablero_recortado_se_cancela_la_fila_que_se_ve() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    let (h, _snap) = host_con_layout(Arc::new(falso), "full", (200, 60)).await;
    let canceladas = Arc::new(std::sync::Mutex::new(Vec::new()));
    let max = norte_ui_host::bridge::MAX_TASKS;
    let total = max + 5;
    let mut vivas = Vec::new();
    for i in 0..total {
        vivas.push(inyectar_task(&tx, 1000 + i as u64, &canceladas));
    }
    // Se suscribe DESPUÉS de meterlas: doscientas sesenta y una altas
    // producen más parches de los que cabe leer, y quedarse atrás no es lo
    // que este test mide. La foto que pide el resync trae el tablero entero.
    asentar().await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(foto.tasks.len(), max, "el tablero va acotado");
    let primera_pintada = foto.tasks[0].task_id;

    h.dispatch(UiAction::FocusSlot { slot_id: 7 })
        .await
        .expect("host vivo");
    // Cursor en la primera fila PINTADA (arriba del todo).
    h.dispatch(tecla("Home")).await.expect("host vivo");
    h.dispatch(tecla_mod("k", true, false))
        .await
        .expect("host vivo");
    assert_eq!(
        *canceladas.lock().expect("canceladas"),
        vec![primera_pintada],
        "se cancela la de la fila resaltada, no una que no está en pantalla"
    );
    drop(vivas);
}

/// Un lote que NACE terminal pide su informe igual.
///
/// El daemon puede completarlo antes de que vuelva la llamada; entonces el
/// watch ya está resuelto, `progreso` no se llama ni una vez, y la única
/// señal de que el directorio quedó a medias no se pedía nunca — justo en
/// los lotes rápidos, que es donde el desenlace más parece que todo fue bien.
#[tokio::test]
async fn un_lote_que_nace_terminal_pide_su_informe() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    *falso.informe.lock().expect("informe") = Some(informe_limpio(2));
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // Nace COMPLETADA: el emisor se suelta acto seguido, como hace el SDK
    // con una task que ya llegó terminal.
    let progreso = norte_proto::TaskProgress {
        task_id: norte_proto::TaskId::new(81),
        kind: norte_proto::TaskKind::RenameBatch,
        state: norte_proto::TaskState::Completed,
        bytes_done: 0,
        bytes_total: None,
        entries_done: 1,
        entries_total: Some(1),
        current: None,
        unreadable: None,
        unvisited: None,
    };
    let (ptx, prx) = tokio::sync::watch::channel(progreso);
    drop(ptx);
    tx.send(norte_ui_host::backend::HostTask {
        id: norte_proto::TaskId::new(81),
        progress: prx,
        cancel: Arc::new(|| {}),
        foreign: false,
    })
    .expect("el host escucha");

    let detalle = detalle_de_task(&mut sub).await;
    assert!(
        detalle.contains('2'),
        "el informe llegó al tablero: {detalle}"
    );
    assert_eq!(*backend.informes_pedidos.lock().expect("pedidos"), vec![81]);
}

/// Un CLIC sobre una aprobación recién abierta no la aprueba.
///
/// El diálogo se pinta en el mismo sitio que el anterior y con la misma
/// primera opción, así que un clic ya en marcha sobre «Confirmar» aterrizaba
/// sobre el «Aprobar» de una aprobación de agente que acababa de llegar. La
/// regla de «se abre solo, la primera respuesta solo reconoce» era solo del
/// teclado, y el ratón es la entrada primaria de esta superficie.
#[tokio::test]
async fn un_clic_sobre_una_aprobacion_recien_abierta_no_la_aprueba() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.aprobaciones.lock().expect("aprobaciones") = Some(rx);
    let backend = Arc::new(falso);
    let (host, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = host.subscribe();

    tx.send(norte_proto::methods::PolicyApprovalRequired {
        approval_id: 21,
        session: Some("agente-1".to_owned()),
        op: "delete".to_owned(),
        paths: vec!["mem:///casa/x".to_owned()],
        paths_total: 1,
        ttl_ms: 30_000,
        detail: norte_proto::methods::ApprovalDetail::default(),
    })
    .expect("el host escucha");
    let id = siguientes_dialogos(&mut sub).await[0].id;

    host.dispatch(UiAction::Dialog {
        id,
        choice: "approve".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    asentar().await;
    assert!(
        backend.decisiones.lock().expect("decisiones").is_empty(),
        "el primer clic solo reconoce"
    );

    // El segundo sí aprueba: la pregunta ya se ha visto.
    host.dispatch(UiAction::Dialog {
        id,
        choice: "approve".to_owned(),
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
        vec![(21, true)]
    );
}

/// Una ruta que el daemon YA redactó va marcada.
///
/// Las rutas de una aprobación llegan como texto pasado por el lossy del
/// daemon: los controles, los overrides bidi y los bytes inválidos ya son
/// U+FFFD. Calcular la marca comparando contra ese texto daba `false`
/// exactamente en la clase más peligrosa, y encima de forma inconsistente
/// —un `zwsp`, que el lossy no toca, sí la encendía—.
#[tokio::test]
async fn una_ruta_ya_redactada_por_el_daemon_va_marcada() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.aprobaciones.lock().expect("aprobaciones") = Some(rx);
    let (host, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = host.subscribe();

    // Tal cual lo manda el daemon: `display_lossy` ya sustituyó el override.
    tx.send(norte_proto::methods::PolicyApprovalRequired {
        approval_id: 22,
        session: None,
        op: "delete".to_owned(),
        paths: vec!["mem:///casa/factura\u{FFFD}.pdf".to_owned()],
        paths_total: 1,
        ttl_ms: 30_000,
        detail: norte_proto::methods::ApprovalDetail::default(),
    })
    .expect("el host escucha");

    let d = siguientes_dialogos(&mut sub).await;
    assert!(
        d[0].body.iter().any(|l| l.hostile),
        "lo que se lee no es lo que hay, y se dice: {:?}",
        d[0].body
    );
}

/// Una ruta LIMPIA pero más larga que el tope del puente se marca por el
/// recorte.
///
/// El recorte le pega una elipsis DESPUÉS del veredicto de `path_display`, y
/// `…` es un carácter legal en un nombre: sin marca, quien lee no distingue
/// «se llama así» de «esto está cortado». Y en el informe de un lote ese
/// nombre es lo único accionable que hay.
#[tokio::test]
async fn una_ruta_limpia_pero_recortada_se_marca() {
    let largo: String = std::iter::repeat_n("segmento_larguisimo_pero_limpio", 200)
        .collect::<Vec<_>>()
        .join("/");
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    *falso.informe.lock().expect("informe") =
        Some(norte_proto::methods::FsRenameBatchReportResult {
            applied: 1,
            rolled_back: 0,
            failed_pair: Some(0),
            stuck: Some(norte_proto::methods::RenameStuckStep {
                from: VPath::parse("mem:///casa/antes.txt").expect("vpath"),
                to: VPath::parse(&format!("mem:///casa/{largo}")).expect("vpath"),
                pair_index: 0,
                error: norte_proto::Error::Io { retryable: false },
                journalled: true,
                still_applied: 1,
            }),
            uncertain: None,
            compensations_lost: 0,
        });
    let (h, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = h.subscribe();
    let p = inyectar_task_de(&tx, 91, norte_proto::TaskKind::RenameBatch);
    siguientes_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    let d = siguientes_dialogos(&mut sub).await;
    assert!(
        d[0].body.iter().any(|l| l.hostile),
        "el recorte también altera lo pintado: {:?}",
        d[0].body
    );
}

/// Tras un RELEVO del daemon, una task con el mismo id no hereda nada de la
/// anterior.
///
/// Los ids los reparte el scheduler de un proceso y empiezan en 1 en cada
/// arranque, así que el daemon nuevo reparte los MISMOS números. La task 3
/// nueva heredaba de la vieja que su informe ya se había pedido — y entonces
/// no se pedía nunca, que es perder la única señal de un directorio a medias.
#[tokio::test]
async fn tras_un_relevo_un_id_repetido_no_hereda_nada() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    let (evtx, evrx) = tokio::sync::mpsc::unbounded_channel();
    *falso.eventos.lock().expect("eventos") = Some(evrx);
    *falso.informe.lock().expect("informe") = Some(informe_limpio(1));
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    let p = inyectar_task_de(&tx, 3, norte_proto::TaskKind::RenameBatch);
    siguientes_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    detalle_de_task(&mut sub).await;
    assert_eq!(backend.informes_pedidos.lock().expect("pedidos").len(), 1);

    // Relevo: se va y vuelve. Al otro lado, otro daemon.
    evtx.send(norte_client::ConnEvent::GoingAway { reconnect: true })
        .expect("el host escucha");
    evtx.send(norte_client::ConnEvent::Restored)
        .expect("el host escucha");
    // El relevo lo procesa el actor: se le deja correr antes de inyectar la
    // task del daemon nuevo, o la carrera sería con el reconectado.
    asentar().await;

    // Su primera task también es la 3, y también es un lote.
    let p2 = inyectar_task_de(&tx, 3, norte_proto::TaskKind::RenameBatch);
    p2.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    anotados(&backend, "el informe de la task NUEVA", 2, |f| {
        f.informes_pedidos.lock().expect("pedidos").clone()
    })
    .await;
}

/// La pila de diálogos tiene techo, y que se cayó uno se DICE.
///
/// Desde que la alimenta el wire —un informe por cada lote ajeno que quedó a
/// medias— una pila sin techo es un canal de memoria de crecimiento libre, y
/// cada parche de diálogos clona la pila entera.
#[tokio::test]
async fn la_pila_de_dialogos_tiene_techo() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    *falso.informe.lock().expect("informe") =
        Some(norte_proto::methods::FsRenameBatchReportResult {
            applied: 1,
            rolled_back: 1,
            failed_pair: Some(0),
            stuck: None,
            uncertain: None,
            compensations_lost: 0,
        });
    let (h, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = h.subscribe();

    let tope = norte_ui_host::bridge::MAX_DIALOGS;
    let mut vivas = Vec::new();
    for i in 0..(tope + 3) {
        let p = inyectar_task_de(&tx, 400 + i as u64, norte_proto::TaskKind::RenameBatch);
        p.send_modify(|p| p.state = norte_proto::TaskState::Completed);
        vivas.push(p);
    }

    let mut ultimos = Vec::new();
    for _ in 0..60 {
        let d = siguientes_dialogos(&mut sub).await;
        ultimos = d;
        if ultimos.len() >= tope {
            break;
        }
    }
    assert!(
        ultimos.len() <= tope,
        "la pila no pasa del techo: {}",
        ultimos.len()
    );
    drop(vivas);
}

/// Una ventana SIN efectos no aborta la task de otro cliente.
///
/// Cancelar una copia deja el destino limpio o un `.norte-partial`: toca el
/// disco. Sus propias tasks son otra cosa — para lanzarlas ya hacía falta el
/// interruptor.
#[tokio::test]
async fn en_solo_lectura_no_se_para_la_task_de_otro() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: Arc::new(falso),
        initial_dir: dir(),
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
        effects: norte_ui_host::commands::Efectos::SoloLectura,
        log_ring: None,
    })
    .await
    .expect("arranca");
    let mut sub = h.subscribe();
    let canceladas = Arc::new(std::sync::Mutex::new(Vec::new()));
    let _p = inyectar_task(&tx, 55, &canceladas);
    siguientes_tasks(&mut sub).await;

    let ack = h
        .dispatch(tecla_mod("k", true, false))
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Unavailable { .. }),
        "una ventana sin efectos no la para: {ack:?}"
    );
    assert!(canceladas.lock().expect("canceladas").is_empty());
}

/// Una aprobación dice QUÉ se pide y QUIÉN lo pide, y los dos van fuera de
/// la lista de rutas.
///
/// Mezclados con las rutas eran una línea más: un fichero llamado `delete`
/// —o llamado como una sesión de agente— era indistinguible de la línea que
/// dice qué se está aprobando. Y el plazo, lo mismo: con `ttl_ms == 0` no se
/// pintaba ninguna línea de plazo, así que un fichero llamado «caduca en
/// 3600 s» era la única con pinta de serlo.
#[tokio::test]
async fn una_aprobacion_dice_que_pide_quien_y_hasta_cuando() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.aprobaciones.lock().expect("aprobaciones") = Some(rx);
    let (host, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = host.subscribe();

    tx.send(norte_proto::methods::PolicyApprovalRequired {
        approval_id: 31,
        session: Some("agente-7".to_owned()),
        op: "delete".to_owned(),
        paths: vec!["mem:///casa/x".to_owned(), "mem:///casa/y".to_owned()],
        paths_total: 2,
        ttl_ms: 30_000,
        detail: norte_proto::methods::ApprovalDetail::default(),
    })
    .expect("el host escucha");

    let d = &siguientes_dialogos(&mut sub).await[0];
    assert_eq!(
        d.subject.as_ref().map(|l| l.text.clone()).as_deref(),
        Some("delete")
    );
    assert_eq!(
        d.asker.as_ref().map(|l| l.text.clone()).as_deref(),
        Some("agente-7")
    );
    assert_eq!(
        d.deadline.as_deref(),
        Some(
            norte_i18n::ta_in(norte_i18n::Lang::Es, "modal-approval-ttl", &[("s", "30")]).as_str()
        )
    );
    assert_eq!(
        d.body.len(),
        2,
        "el cuerpo son SOLO las rutas: {:?}",
        d.body
    );
}

/// Sin TTL —una pendiente reconstruida por el resync— se dice que el plazo
/// NO se sabe, en vez de callar.
///
/// Callar deja el diálogo delante invitando a aprobar sobre un id que el
/// daemon puede haber reapado hace rato.
#[tokio::test]
async fn una_aprobacion_sin_ttl_dice_que_no_sabe_el_plazo() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.aprobaciones.lock().expect("aprobaciones") = Some(rx);
    let (host, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = host.subscribe();

    tx.send(norte_proto::methods::PolicyApprovalRequired {
        approval_id: 32,
        session: None,
        op: "copy".to_owned(),
        paths: vec!["mem:///casa/x".to_owned()],
        paths_total: 1,
        ttl_ms: 0,
        detail: norte_proto::methods::ApprovalDetail::default(),
    })
    .expect("el host escucha");

    let d = &siguientes_dialogos(&mut sub).await[0];
    assert_eq!(
        d.deadline.as_deref(),
        Some(norte_i18n::t_in(norte_i18n::Lang::Es, "modal-approval-ttl-unknown").as_str())
    );
    assert!(d.asker.is_none(), "sin sesión, no se inventa una");
}

/// El aviso de sesión en claro lleva la conexión en su PROPIO campo y con su
/// marca.
///
/// Dentro de la frase, un host llamado `banco.example@malo.example` —que no
/// lleva ni un carácter que se enmascare— se lee como userinfo de un host
/// legítimo. Y enmascarar sin decirlo, en el indicador de que algo viaja sin
/// cifrar, es donde más caro sale.
#[tokio::test]
async fn el_aviso_en_claro_lleva_la_conexion_aparte_y_marcada() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.degradadas.lock().expect("degradadas") = Some(rx);
    let (h, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = h.subscribe();
    tx.send(norte_proto::methods::ConnectionDegraded {
        scheme: "ftp".to_owned(),
        host: "ma\u{202E}lo.example".to_owned(),
        reason: "ftp-plaintext".to_owned(),
        detail: None,
    })
    .expect("el host escucha");

    let banners = siguientes_banners(&mut sub).await;
    let sujeto = banners
        .iter()
        .find_map(|b| b.subject.clone())
        .expect("el aviso lleva su conexión");
    assert!(!sujeto.host.contains('\u{202E}'), "{sujeto:?}");
    assert!(sujeto.hostile, "y dice que la enmascaró: {sujeto:?}");
    assert!(
        banners.iter().all(|b| !b.text.contains("://")),
        "la conexión no se monta dentro de la frase: {banners:?}"
    );
}

/// Un kind que este host no proyecta y cuyo nombre viene alterado va MARCADO.
///
/// Sale del fichero de disposición del usuario: se enmascaraba y se tiraba la
/// bandera, así que se leía como fiel (#266).
#[tokio::test]
async fn un_kind_desconocido_con_nombre_alterado_va_marcado() {
    use norte_frontend::layout::{KindId, Node, SlotId};
    let disposicion = Node::Split {
        dir: norte_frontend::layout::Dir::Vertical,
        children: vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(9), KindId::new("com\u{202E}pare")),
        ],
        sizes: vec![
            norte_frontend::layout::Size::Weight(1),
            norte_frontend::layout::Size::Fixed(3),
        ],
    };
    let (_h, snap) = UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: disposicion,
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

    let marcado = snap.slots.iter().any(|s| match s {
        SlotView::Unsupported {
            kind_name,
            kind_name_hostile,
            ..
        } => *kind_name_hostile && !kind_name.contains('\u{202E}'),
        _ => false,
    });
    assert!(marcado, "el kind alterado se dice: {:?}", snap.slots);
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

/// En SOLO LECTURA no se pregunta: la consulta sale del proceso hacia el
/// proveedor de IA, igual que el plan de renombrado.
#[tokio::test]
async fn en_solo_lectura_no_hay_busqueda_semantica() {
    let falso = arbol_como_falso();
    let backend = Arc::new(falso);
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: Arc::clone(&backend) as Arc<dyn norte_ui_host::backend::HostBackend>,
        initial_dir: dir(),
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
async fn ejecutar_por_paleta(
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
async fn ejecutar_por_paleta_ack(
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

// ---------------------------------------------------------------------------
// Comparar directorios (tarea 6.2).
// ---------------------------------------------------------------------------

/// Espera la siguiente actualización con el panel de diferencias.
async fn siguiente_comparacion(
    sub: &mut norte_ui_host::controller::UiSubscription,
) -> Option<norte_ui_host::dto::CompareView> {
    for _ in 0..40 {
        match tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv())
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
fn fila_comparada(
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
async fn separar_los_paneles(h: &UiHost, sub: &mut norte_ui_host::controller::UiSubscription) {
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

// ---------------------------------------------------------------------------
// Sincronizar: el PLAN (tarea 6.3, fase A).
// ---------------------------------------------------------------------------

/// Espera la siguiente actualización con el panel de sincronización.
async fn siguiente_sync(
    sub: &mut norte_ui_host::controller::UiSubscription,
) -> Option<norte_ui_host::dto::SyncView> {
    for _ in 0..40 {
        let Ok(Some(u)) =
            tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv()).await
        else {
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
fn paso_de_plan(
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
fn plan_cerrado(pasos: u64) -> norte_proto::methods::SyncPlanDone {
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
fn arbol_con_esquema(
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
fn esquema_de_prueba() -> Vec<norte_proto::methods::PluginConfigKeyWire> {
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
async fn ficha_abierta(
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
async fn esperar_buffer(h: &UiHost, sub: &mut norte_ui_host::UiSubscription) {
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
async fn siguiente_foto_tras_resync(
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
fn tecla_de_acorde(acorde: &str) -> norte_ui_host::keys::KeyInput {
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

    let efecto = tokio::time::timeout(std::time::Duration::from_millis(500), efectos.recv())
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
async fn host_en(backend: Arc<Falso>, inicio: &str) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: VPath::parse(inicio).expect("vpath"),
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
async fn listado_en(
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
fn arbol_de(snap: &norte_ui_host::ViewSnapshot) -> &norte_ui_host::dto::TreeSlotView {
    snap.slots
        .iter()
        .find_map(|s| match s {
            SlotView::Tree(t) => Some(&**t),
            _ => None,
        })
        .expect("hay un hueco de árbol")
}

/// Espera a una foto en la que el árbol ya tiene sus ramas.
async fn arbol_con_ramas(
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
async fn host_con_capas(dir_usuario: &std::path::Path) -> (UiHost, norte_ui_host::ViewSnapshot) {
    host_con_capas_y_favoritos(dir_usuario, Vec::new()).await
}

/// El mismo, con favoritos YA cargados: la ventana los lee al arrancar, así
/// que un test que solo escriba el `norte.toml` monta un host que no los ve.
async fn host_con_capas_y_favoritos(
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

// ---------------------------------------------------------------------------
// #290 fase A: los gestos de panel que el TUI tenía y la ventana no.
//
// Ninguno inventa modelo. Lo que estos tests comprueban es que la ventana usa
// el COMPARTIDO —`SortSpec::after_click`, `PaneState::toggle_hidden`,
// `History::entries`, el rol `Target` de la ADR 0058— y que lo que no puede
// hacer lo DICE, en vez de quedarse muda.
// ---------------------------------------------------------------------------

/// La columna por la que se ordena, y en qué sentido, leídas de las cabeceras
/// que cruzan el puente: es lo único que el renderer sabe del orden.
fn orden_de(b: &norte_ui_host::dto::BrowserSlotView) -> (String, String) {
    let marcadas: Vec<&norte_ui_host::dto::ColumnHeader> =
        b.columns.iter().filter(|c| c.sort.is_some()).collect();
    assert_eq!(
        marcadas.len(),
        1,
        "una sola columna lleva la marca de orden: {:?}",
        b.columns
    );
    (
        marcadas[0].id.clone(),
        marcadas[0].sort.clone().expect("marcada"),
    )
}

/// Una foto de ahora mismo.
async fn foto(h: &UiHost, sub: &mut norte_ui_host::UiSubscription) -> norte_ui_host::ViewSnapshot {
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    siguiente_foto(sub).await
}

/// Espera a que una FOTO cumpla `cond`, sin relojes de intervalo.
///
/// Hay que PREGUNTAR: ni `total_rows` ni `path_display` viajan en un parche
/// —solo en una foto, y la foto la pide `Resync`—, así que quedarse
/// escuchando no basta. Lo que este ayudante no hace es dormir 25 ms entre
/// intento e intento: se BLOQUEA en el siguiente mensaje del host, o sea
/// que va al ritmo del host y no al del planificador. El plazo total está
/// para que una condición que no se cumple se lea como un fallo con su
/// frase, y no como un test colgado.
async fn esperar_foto(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    que: &str,
    cond: impl Fn(&norte_ui_host::ViewSnapshot) -> bool,
) -> norte_ui_host::ViewSnapshot {
    let espera = async {
        loop {
            let f = foto(h, sub).await;
            if cond(&f) {
                return f;
            }
            // CUALQUIER mensaje del host, no la siguiente FOTO: las fotos las
            // pide el renderer, y un drenaje viaja entero en PARCHES. Esperar
            // otra foto era esperar a que el host mandara una por su cuenta,
            // que es justo lo que no hace: el bucle se colgaba en vez de
            // agotar el plazo, y un test colgado no dice qué falló.
            let _ = sub.recv().await.expect("el host sigue vivo");
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), espera)
        .await
        .unwrap_or_else(|_| panic!("plazo agotado esperando a que {que}"))
}

/// `pane.sort-size` ordena por tamaño y repetirlo INVIERTE.
///
/// La misma semántica que un click en la cabecera porque es el MISMO camino:
/// quien decide es `SortSpec::after_click`, no una tabla por superficie.
#[tokio::test]
async fn el_comando_de_orden_es_el_click_en_la_cabecera() {
    let (h, snap) = host_arbol(arbol()).await;
    let (columna_inicial, _) = orden_de(listado(&snap));
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.sort-size").await;
    let despues = foto(&h, &mut sub).await;
    let (columna, sentido) = orden_de(listado(&despues));
    assert_ne!(columna, columna_inicial, "ordena por OTRA columna");
    assert_eq!(sentido, "asc", "una columna nueva empieza ascendente");

    ejecutar_por_paleta(&h, &mut sub, "pane.sort-size").await;
    let otra_vez = foto(&h, &mut sub).await;
    let (misma, sentido) = orden_de(listado(&otra_vez));
    assert_eq!(misma, columna, "sigue siendo la misma columna");
    assert_eq!(sentido, "desc", "la columna activa INVIERTE");
}

/// `pane.sort-menu` no estrena pantalla: abre el selector de COLUMNAS, donde
/// están la columna, la dirección y `dirs_first`. Es la decisión del TUI, y
/// dos pantallas para lo mismo serían otra que mantener y otra que aprender.
#[tokio::test]
async fn el_menu_de_orden_es_el_selector_de_columnas() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "pane.sort-menu").await;
    let despues = foto(&h, &mut sub).await;
    assert!(
        despues.columns.is_some(),
        "el menú de orden es el selector de columnas"
    );
}

/// `pane.toggle-hidden` aparta los dotfiles del panel y lo ANUNCIA.
///
/// Presentación-solo (#107): el provider no vuelve a listar, así que el
/// backend no ve una petición más.
#[tokio::test]
async fn ocultar_es_presentacion_y_se_dice() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![
            (b"docs".to_vec(), true),
            (b".oculto".to_vec(), false),
            (b"notas.txt".to_vec(), false),
        ],
    );
    let backend = Arc::new(f);
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let antes = listado(&snap).rows.len();
    assert!(
        listado(&snap)
            .rows
            .iter()
            .any(|r| r.display_name == ".oculto"),
        "sin `[ui] show_hidden` en la config se enseña todo"
    );
    let listados = backend.listados();
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.toggle-hidden").await;
    let despues = foto(&h, &mut sub).await;
    assert!(
        listado(&despues)
            .rows
            .iter()
            .all(|r| r.display_name != ".oculto"),
        "los ocultos se apartaron"
    );
    assert_eq!(
        backend.listados(),
        listados,
        "y se apartaron SIN volver a pedir el directorio"
    );
    assert!(
        despues.status.message.is_some(),
        "un listado que encoge sin decir por qué se lee como un fallo"
    );

    ejecutar_por_paleta(&h, &mut sub, "pane.toggle-hidden").await;
    let otra_vez = foto(&h, &mut sub).await;
    assert_eq!(
        listado(&otra_vez).rows.len(),
        antes,
        "y vuelve a devolverlos, sin re-listar"
    );
}

/// `pane.names-encoding` cambia cómo se PINTA un nombre que no es UTF-8, y no
/// los bytes: la fila sigue marcada como hostil y su clave sigue valiendo.
#[tokio::test]
async fn ciclar_el_encoding_repinta_sin_tocar_los_bytes() {
    let (h, snap) = host_arbol(arbol()).await;
    let hostil = listado(&snap)
        .rows
        .iter()
        .find(|r| r.hostile)
        .expect("el árbol trae un nombre que no es UTF-8");
    let (clave, pintado) = (hostil.key, hostil.display_name.clone());
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.names-encoding").await;
    let despues = foto(&h, &mut sub).await;
    let misma = listado(&despues)
        .rows
        .iter()
        .find(|r| r.key == clave)
        .expect("la clave sigue valiendo: los bytes no cambiaron");
    assert_ne!(misma.display_name, pintado, "se pinta de otra forma");
    assert!(
        despues.status.message.is_some(),
        "y se dice con qué se está reinterpretando"
    );
}

/// `pane.refresh` vuelve a pedir TODOS los listados que se ven, no solo el
/// enfocado: lo que cambia un directorio por debajo es un cambio en el DISCO,
/// y un cambio en el disco no respeta el foco.
#[tokio::test]
async fn refrescar_relista_los_dos_paneles() {
    let backend = arbol();
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let antes = backend.listados();
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.refresh").await;
    esperar_foto(&h, &mut sub, "los dos paneles se relisten", |_| {
        backend.listados() >= antes + 2
    })
    .await;
}

/// `pane.mirror` manda la ubicación del panel ACTIVO al panel destino, y el
/// destino sale del rol compartido — nunca de «el de al lado».
#[tokio::test]
async fn el_espejo_manda_la_ubicacion_al_destino() {
    let (h, snap) = crate::dos_paneles_con_destino_aparte(arbol()).await;
    // El escenario deja el 2 en `/casa/docs` y el foco en el 1, que sigue en
    // `/casa`: espejar tiene que llevarse el 2 de vuelta.
    assert!(listado_de(&snap, 2).path_display.ends_with("/casa/docs"));
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.mirror").await;
    let f = esperar_foto(&h, &mut sub, "el destino siga al activo", |f| {
        listado_de(f, 2).path_display.ends_with("/casa")
            && listado_de(f, 1).path_display.ends_with("/casa")
    })
    .await;
    assert_eq!(f.focus, Some(1), "el espejo no mueve el foco");
}

/// `pane.mirror-target` manda la CARPETA BAJO EL CURSOR, no la ubicación:
/// `Ctrl+←`/`Ctrl+→` de Krusader, y la misma respuesta que da la TUI porque
/// quien la decide es `PaneState::target_dir` (ADR 0077).
#[tokio::test]
async fn el_espejo_del_objetivo_manda_la_carpeta_del_cursor() {
    let (h, _snap) = crate::dos_paneles_con_destino_aparte(arbol()).await;
    let mut sub = h.subscribe();

    // Los dos en `/casa` para empezar: así lo que se mide después es el
    // OBJETIVO del cursor y no el arrastre del escenario.
    ejecutar_por_paleta(&h, &mut sub, "pane.mirror").await;
    esperar_foto(&h, &mut sub, "los dos en /casa", |f| {
        listado_de(f, 2).path_display.ends_with("/casa")
    })
    .await;

    // El cursor a la fila 0, que aquí es el directorio `docs`: estos ajustes
    // traen la fila `..` APAGADA (`ui_parent_entry = false`), y el escenario
    // deja el cursor donde lo dejó su propia navegación.
    ejecutar_por_paleta(&h, &mut sub, "cursor.top").await;
    ejecutar_por_paleta(&h, &mut sub, "pane.mirror-target").await;
    let f = esperar_foto(&h, &mut sub, "el destino entre en la carpeta", |f| {
        listado_de(f, 2).path_display.ends_with("/casa/docs")
    })
    .await;
    assert!(
        listado_de(&f, 1).path_display.ends_with("/casa"),
        "el panel del foco no se mueve: {}",
        listado_de(&f, 1).path_display
    );
    assert_eq!(f.focus, Some(1), "ni el foco");
}

/// `pane.pull` es el mismo gesto al revés: la ubicación sale del destino y
/// viaja el panel con el foco.
#[tokio::test]
async fn traer_mueve_el_panel_del_foco() {
    let (h, _snap) = crate::dos_paneles_con_destino_aparte(arbol()).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.pull").await;
    let f = esperar_foto(
        &h,
        &mut sub,
        "el panel del foco se traiga la otra ubicación",
        |f| listado_de(f, 1).path_display.ends_with("/casa/docs"),
    )
    .await;
    assert!(
        listado_de(&f, 2).path_display.ends_with("/casa/docs"),
        "el otro se queda donde estaba"
    );
}

/// `pane.swap` cambia los dos listados de sitio SIN tocar disco: nadie
/// vuelve a pedir un directorio, y el foco se queda donde estaba.
#[tokio::test]
async fn intercambiar_no_pide_nada_al_backend() {
    let backend = arbol();
    let (h, snap) = crate::dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    assert!(listado_de(&snap, 1).path_display.ends_with("/casa"));
    assert!(listado_de(&snap, 2).path_display.ends_with("/casa/docs"));
    let listados = backend.listados();
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.swap").await;
    let despues = foto(&h, &mut sub).await;
    assert!(
        listado_de(&despues, 1).path_display.ends_with("/casa/docs"),
        "el panel del foco enseña lo otro: {}",
        listado_de(&despues, 1).path_display
    );
    assert!(listado_de(&despues, 2).path_display.ends_with("/casa"));
    assert_eq!(despues.focus, Some(1), "el foco no se mueve con el gesto");
    assert_eq!(
        backend.listados(),
        listados,
        "los dos listados ya existían: intercambiarlos no toca disco"
    );
}

/// La ventana de PINTADO no viaja con el intercambio.
///
/// `primera_visible`/`visibles` los pone el renderer por slot con
/// `set_visible_range`, y su `scrollTop` es suyo: un swap no lo mueve ni
/// dispara un evento de scroll. Si la ventana viajara con el hueco, cada
/// panel pintaría filas de una banda que el lector no tiene delante y los DOS
/// se verían VACÍOS. Con listados cortos y los dos arriba del todo no se nota
/// nada, que es por lo que hace falta este test y no el de al lado.
#[tokio::test]
async fn el_intercambio_no_mueve_la_ventana_de_pintado() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        (0..300).map(|i| (format!("f{i:03}").into_bytes(), false)),
    );
    let (h, _snap) = host_con_layout(Arc::new(f), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 200,
        count: 30,
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 2,
        first: 0,
        count: 30,
    })
    .await
    .expect("host vivo");
    let antes = foto(&h, &mut sub).await;
    assert_eq!(listado_de(&antes, 1).first_visible, 200);
    assert_eq!(listado_de(&antes, 2).first_visible, 0);

    h.dispatch(tecla_mod("u", true, false))
        .await
        .expect("host vivo");
    let despues = foto(&h, &mut sub).await;

    assert_eq!(
        listado_de(&despues, 1).first_visible,
        200,
        "el slot que estaba por la fila 200 sigue pintando por la 200: el \
         scroll del renderer no se ha movido"
    );
    assert_eq!(
        listado_de(&despues, 2).first_visible,
        0,
        "y el que estaba arriba sigue arriba"
    );
}

/// Un intercambio durante una NAVEGACIÓN la repide, apuntando a donde iba.
///
/// La respuesta en vuelo viaja etiquetada con su slot: tras el cambio llega
/// al hueco equivocado y se descarta por testigo. Sin repedirla, el panel se
/// queda `Loading` para siempre.
#[tokio::test]
async fn el_intercambio_repide_la_navegacion_en_vuelo() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)],
    );
    f.pon("mem:///casa/docs", vec![(b"informe.pdf".to_vec(), false)]);
    // Lo bastante lento para que el swap caiga DENTRO de la navegación.
    f.retraso_ms = 400;
    let (h, snap) = host_con_layout(Arc::new(f), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    let b1 = listado_de(&snap, 1);
    let docs = b1
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("el directorio está");
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs.key,
        generation: b1.generation,
    })
    .await
    .expect("host vivo");
    h.dispatch(tecla_mod("u", true, false))
        .await
        .expect("host vivo");

    let f = esperar_foto(
        &h,
        &mut sub,
        "la navegación interrumpida llegue a su destino",
        |f| {
            listado_de(f, 2).path_display.ends_with("/casa/docs")
                && listado_de(f, 1).path_display.ends_with("/casa")
        },
    )
    .await;
    let (izq, der) = (listado_de(&f, 1), listado_de(&f, 2));
    assert!(
        !matches!(izq.state, norte_ui_host::dto::SlotState::Loading)
            && !matches!(der.state, norte_ui_host::dto::SlotState::Loading),
        "ningún panel se queda cargando: {:?} {:?}",
        izq.state,
        der.state
    );
}

/// Un intercambio durante el DRENAJE también lo repide.
///
/// `en_vuelo` muere con la primera página y `drenando` sigue vivo: en un
/// directorio de más de cien entradas —o sea casi cualquiera— hay una ventana
/// en la que solo vive el drenaje. Mirar solo `en_vuelo` dejaba el listado
/// congelado en cien entradas, en `Ready` y sin decir nada, y marcar todo
/// actuaba sobre ese trozo.
#[tokio::test]
async fn el_intercambio_repide_el_drenaje() {
    let puerta = Arc::new(backend_falso::Puerta::default());
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        (0..250).map(|i| (format!("f{i:03}").into_bytes(), false)),
    );
    f.puerta_drenaje = Some(Arc::clone(&puerta));
    let (h, _snap) = host_con_layout(Arc::new(f), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();

    // La primera página ya está en pantalla y el resto sigue detenido en la
    // puerta: ESTE es el estado que el bug necesitaba.
    esperar_foto(&h, &mut sub, "aterrice la primera página", |f| {
        listado_de(f, 1).total_rows == Some(100)
    })
    .await;

    h.dispatch(tecla_mod("u", true, false))
        .await
        .expect("host vivo");
    puerta.abrir();

    esperar_foto(&h, &mut sub, "el drenaje se repida y llegue entero", |f| {
        listado_de(f, 1).total_rows == Some(250) && listado_de(f, 2).total_rows == Some(250)
    })
    .await;
}

/// `pane.toggle-hidden` PODA las marcas de lo que aparta, y lo dice.
///
/// El contrato de `PaneState::pruned_marks` es que una selección que alimenta
/// una op en masa jamás encoge en silencio: callarlo copiaría menos ficheros
/// de los que el lector marcó, creyendo él que van todos.
#[tokio::test]
async fn apartar_los_ocultos_dice_las_marcas_que_se_lleva() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b".env".to_vec(), false), (b"notas.txt".to_vec(), false)],
    );
    let (h, snap) = host_arbol(Arc::new(f)).await;
    let oculta = listado(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == ".env")
        .expect("los ocultos se ven de fábrica");
    h.dispatch(UiAction::ToggleMark {
        slot_id: 1,
        key: oculta.key,
        generation: listado(&snap).generation,
    })
    .await
    .expect("host vivo");
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.toggle-hidden").await;
    let despues = foto(&h, &mut sub).await;

    assert_eq!(
        listado_de(&despues, 1).marks,
        0,
        "la marca se fue con la fila"
    );
    let dicho = despues.status.message.clone().unwrap_or_default();
    assert!(
        dicho.contains('1') && dicho.contains("caída") || dicho.contains("caídas"),
        "y se DICE cuántas se cayeron: {dicho:?}"
    );
}

/// Un espejo REDUNDANTE no toca el otro panel.
///
/// Los dos ya enseñan lo mismo, así que un cd sería re-listar: `set_listing`
/// le borra las marcas —una navegación, a diferencia de un refresco, no las
/// restaura— y le desliza el listado bajo el cursor, a cambio de nada. El TUI
/// lo rehúsa por lo mismo (`gestures::mirror_plan`).
#[tokio::test]
async fn un_espejo_redundante_no_borra_las_marcas_del_otro() {
    let backend = arbol();
    let (h, snap) = host_con_layout(Arc::clone(&backend), "orthodox", (120, 40)).await;
    let b2 = listado_de(&snap, 2);
    // `fila_de` RECHAZA un slot que no sea el activo, así que el foco va
    // primero al 2 y vuelve al 1. Lo que se prueba aquí es el espejo.
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host vivo");
    h.dispatch(UiAction::ToggleMark {
        slot_id: 2,
        key: b2.rows[0].key,
        generation: b2.generation,
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host vivo");
    let mut sub = h.subscribe();
    let antes = foto(&h, &mut sub).await;
    assert_eq!(listado_de(&antes, 2).marks, 1);
    let listados = backend.listados();

    ejecutar_por_paleta(&h, &mut sub, "pane.mirror").await;
    let despues = foto(&h, &mut sub).await;

    assert_eq!(
        listado_de(&despues, 2).marks,
        1,
        "los dos ya estaban en el mismo sitio: la marca sigue puesta"
    );
    assert_eq!(
        backend.listados(),
        listados,
        "y no se ha vuelto a pedir nada"
    );
}

/// Un gesto de panel sin otro panel se DICE, y con la frase que distingue
/// «no hay otro» de «hay varios, designa uno».
#[tokio::test]
async fn un_gesto_sin_otro_panel_se_dice() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    for cmd in ["pane.mirror", "pane.pull", "pane.swap"] {
        let ack = ejecutar_por_paleta_ack(&h, &mut sub, cmd).await;
        match ack {
            ActionAck::Unavailable { reason_key } => {
                assert_eq!(reason_key, "host-no-other-slot", "{cmd}");
            }
            otro => panic!("{cmd} con un solo panel: {otro:?}"),
        }
    }
}

/// `pane.history` enseña el rastro del panel, y elegir una fila navega.
///
/// Las filas son las de `History::entries` —el MRU compartido—: qué recuerda
/// un panel y en qué orden no puede depender de quién lo pinta.
#[tokio::test]
async fn el_historial_es_el_rastro_compartido() {
    let (h, snap) = host_arbol(arbol()).await;
    let docs = listado(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("el directorio está");
    let (key, generation) = (docs.key, listado(&snap).generation);
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host vivo");
    let mut despues = siguiente_foto(&mut sub).await;
    while !listado(&despues).path_display.ends_with("/casa/docs") {
        despues = siguiente_foto(&mut sub).await;
    }

    ejecutar_por_paleta(&h, &mut sub, "pane.history").await;
    let abierto = foto(&h, &mut sub).await;
    let picker = abierto.picker.expect("el historial está abierto");
    assert!(
        picker.rows.iter().any(|r| r.label.ends_with("/casa")),
        "el sitio del que se salió está en el rastro: {:?}",
        picker.rows
    );

    h.dispatch(tecla("Enter")).await.expect("host vivo");
    esperar_foto(&h, &mut sub, "elegir en el historial navegue", |f| {
        f.picker.is_none() && listado(f).path_display.ends_with("/casa")
    })
    .await;
}

/// `pane.hotlist` enseña los favoritos de la configuración, y uno cuya ruta
/// no parsea SE QUEDA con su aviso: la hotlist es data del usuario, y un
/// favorito que desaparece en silencio es un fallo que nadie puede ver.
#[tokio::test]
async fn un_favorito_invalido_se_queda_y_se_dice() {
    let mut ajustes = ajustes_de_prueba();
    ajustes.common.hotlist = vec![
        norte_config::HotlistItem {
            name: "casa".to_owned(),
            target: Ok(dir()),
        },
        norte_config::HotlistItem {
            name: "roto".to_owned(),
            target: Err("err-invalid-path".to_owned()),
        },
    ];
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: ajustes,
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

    ejecutar_por_paleta(&h, &mut sub, "pane.hotlist").await;
    let abierto = foto(&h, &mut sub).await;
    let picker = abierto.picker.expect("los favoritos están abiertos");
    assert_eq!(picker.rows.len(), 2, "el inválido NO se cae de la lista");
    assert_eq!(picker.rows[1].label, "roto");
    assert!(
        !picker.rows[1].detail.is_empty(),
        "y su detalle dice que la ruta no vale"
    );

    // El cursor sobre el inválido: elegirlo no puede navegar a ninguna parte,
    // y quedarse mudo no se distingue de una tecla rota.
    h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    let ack = h.dispatch(tecla("Enter")).await.expect("host vivo");
    match ack {
        ActionAck::Unavailable { reason_key } => assert_eq!(reason_key, "hotlist-invalid"),
        otro => panic!("un favorito inválido: {otro:?}"),
    }
}

/// `pane.select-drive-left` nombra un LADO de la pantalla, no el foco: con el
/// foco en el panel derecho, el selector sigue siendo el del izquierdo.
#[tokio::test]
async fn los_volumenes_por_lado_no_siguen_al_foco() {
    // Los dos sentidos, porque una implementación que leyera el foco pasaría
    // cualquiera de los dos por separado: lo que hay que fijar es que el
    // panel que se MUEVE es el del lado nombrado y el otro no se toca.
    for (comando, montado, quieto) in [
        ("pane.select-drive-left", 1_u32, 2_u32),
        ("pane.select-drive-right", 2, 1),
    ] {
        let mut f = Falso::default();
        f.pon("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
        f.pon("mem:///otro", vec![(b"raiz.txt".to_vec(), false)]);
        f.volumenes = vec![volumen("mem:///otro", "ext4", false)];
        let (h, _snap) = host_con_layout(Arc::new(f), "orthodox", (200, 60)).await;
        // El foco se pone en el panel CONTRARIO al que el comando nombra.
        h.dispatch(UiAction::FocusSlot { slot_id: quieto })
            .await
            .expect("host vivo");
        let mut sub = h.subscribe();

        ejecutar_por_paleta(&h, &mut sub, comando).await;
        let con_filas = foto_hasta(&h, &mut sub, "la tabla de montaje", |f| {
            f.picker
                .as_ref()
                .is_some_and(|p| !p.rows.is_empty())
                .then(|| f.picker.clone())
        })
        .await;
        assert!(
            con_filas.is_some(),
            "{comando}: la tabla de montaje llega al selector"
        );

        h.dispatch(tecla("Enter")).await.expect("host vivo");
        let montada = foto_hasta(
            &h,
            &mut sub,
            &format!("{comando}: el volumen montado en el hueco del lado {montado}"),
            |f| {
                (f.picker.is_none() && listado_de(f, montado).path_display.contains("otro"))
                    .then(|| f.clone())
            },
        )
        .await;
        assert!(
            listado_de(&montada, quieto).path_display.ends_with("/casa"),
            "{comando}: el panel del foco NO se ha movido: {}",
            listado_de(&montada, quieto).path_display
        );
    }
}

/// Las propiedades de esta ventana son el hueco `metadata`, que ya enseña
/// nombre, clase, tamaño y fecha de lo señalado. La ventana lo hace de otra
/// forma, igual que ordena pulsando la cabecera.
#[tokio::test]
async fn las_propiedades_abren_la_hoja_de_atributos() {
    let (h, snap) = host_arbol(arbol()).await;
    assert!(
        !snap
            .slots
            .iter()
            .any(|s| matches!(s, SlotView::Metadata(_))),
        "de fábrica `simple` no trae hoja de atributos"
    );
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.properties").await;
    let despues = foto(&h, &mut sub).await;
    assert!(
        despues
            .slots
            .iter()
            .any(|s| matches!(s, SlotView::Metadata(_))),
        "las propiedades abren la hoja"
    );
}

/// El orden y la ocultación VUELVEN de la sesión.
///
/// Se escribían y no los leía nadie: la ventana se acordaba de dónde estabas
/// y olvidaba cómo lo estabas mirando, así que ordenar por tamaño o apartar
/// los dotfiles duraba hasta cerrar.
#[tokio::test]
async fn la_sesion_devuelve_el_orden_y_los_ocultos() {
    let mut falso = Falso::default();
    falso.pon(
        "mem:///casa",
        vec![
            (b"docs".to_vec(), true),
            (b".oculto".to_vec(), false),
            (b"notas.txt".to_vec(), false),
        ],
    );
    let mut body = norte_frontend::session::SessionBody::default();
    body.slots.insert(
        1,
        norte_frontend::session::SlotState {
            path: dir(),
            cursor: 0,
            back: Vec::new(),
            forward: Vec::new(),
            sort: norte_frontend::SortSpec {
                column: norte_frontend::SortColumn::Size,
                dir: norte_frontend::SortDir::Desc,
                dirs_first: true,
            },
            columns: Vec::new(),
            show_hidden: false,
            touched_ms: 0,
        },
    );
    *falso.sesion.lock().expect("sesión") = (
        norte_proto::methods::Session {
            version: norte_frontend::session::SCHEMA_VERSION,
            revision: 7,
            body: serde_json::to_value(&body).expect("json"),
        },
        true,
    );

    let (_h, snap) = host_arbol(Arc::new(falso)).await;
    let b = listado(&snap);
    assert!(
        b.rows.iter().all(|r| r.display_name != ".oculto"),
        "la sesión decía que estaban apartados"
    );
    let (_, sentido) = orden_de(b);
    assert_eq!(sentido, "desc", "y que se ordenaba al revés");
}

/// `[ui] show_hidden = false` siembra el estado inicial de los paneles, igual
/// que en el TUI (#107). Sin esto, la clave estaba muerta en esta ventana.
#[tokio::test]
async fn la_config_siembra_la_ocultacion() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b".oculto".to_vec(), false), (b"notas.txt".to_vec(), false)],
    );
    let mut ajustes = ajustes_de_prueba();
    ajustes.common.ui_show_hidden = Some(false);
    let (_h, snap) = UiHost::start(UiHostOptions {
        backend: Arc::new(f),
        initial_dir: dir(),
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: ajustes,
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
        listado(&snap)
            .rows
            .iter()
            .all(|r| r.display_name != ".oculto"),
        "la configuración decía que no se enseñan"
    );
}

// ---------------------------------------------------------------------------
// Un LOTE de transferencias: sus topes y su cuenta (#271).
// ---------------------------------------------------------------------------

/// Marca las N primeras filas del hueco activo, una a una.
async fn marca_todo(h: &UiHost, sub: &mut norte_ui_host::controller::UiSubscription, slot: u32) {
    let foto = foto(h, sub).await;
    let b = listado_de(&foto, slot);
    let (generation, claves): (u64, Vec<_>) =
        (b.generation, b.rows.iter().map(|r| r.key).collect());
    for key in claves {
        h.dispatch(UiAction::ToggleMark {
            slot_id: slot,
            key,
            generation,
        })
        .await
        .expect("host vivo");
    }
}

/// Un lote cuyos rechazos al encolar son TODOS: la barra dice UNA frase con la
/// cuenta, no N frases de las que sobrevive la última (#271, punto 3).
#[tokio::test]
async fn los_rechazos_de_un_lote_se_dicen_una_sola_vez() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![
            (b"docs".to_vec(), true),
            (b"notas.txt".to_vec(), false),
            (b"a.txt".to_vec(), false),
            (b"b.txt".to_vec(), false),
        ],
    );
    f.pon("mem:///casa/docs", vec![(b"x.md".to_vec(), false)]);
    f.transferencia_rechazada = Some(norte_proto::Error::Unsupported);
    let backend = Arc::new(f);
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    marca_todo(&h, &mut sub, 1).await;
    h.dispatch(tecla("F5")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");

    // El resumen dice CUÁNTAS, o sea que la cuenta existió: sin ella la barra
    // llevaría la frase del último error y nada más.
    let foto = esperar_foto(&h, &mut sub, "el lote se resume", |f| {
        f.status.message.as_deref().is_some_and(|m| m.contains('4'))
    })
    .await;
    let msg = foto.status.message.clone().expect("hay resumen");
    assert!(msg.contains('4'), "el resumen no cuenta el lote: {msg}");
    assert!(foto.tasks.is_empty(), "ninguna llegó a ser task");
}

/// Y con las tasks encoladas: el resumen cuenta los DESENLACES, y solo cuando
/// el lote entero está resuelto (#271, punto 2).
#[tokio::test]
async fn el_lote_dice_cuantas_terminaron_bien_y_cuantas_no() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![
            (b"docs".to_vec(), true),
            (b"notas.txt".to_vec(), false),
            (b"a.txt".to_vec(), false),
            (b"b.txt".to_vec(), false),
        ],
    );
    f.pon("mem:///casa/docs", vec![(b"x.md".to_vec(), false)]);
    // Nacen TERMINALES y bien: el camino donde `progreso` no se llama nunca.
    f.estado_transferencia = Some(norte_proto::TaskState::Completed);
    let backend = Arc::new(f);
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    marca_todo(&h, &mut sub, 1).await;
    h.dispatch(tecla("F5")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");

    let foto = esperar_foto(&h, &mut sub, "el lote se resume", |f| {
        f.status.message.is_some()
    })
    .await;
    let msg = foto.status.message.clone().expect("hay resumen");
    assert!(
        msg.contains('4') && msg.contains('0'),
        "el resumen dice 4 pedidas y 0 mal: {msg}"
    );
}

/// El tope de lote (#271, punto 4): `pane.copy` opera sobre las marcas y
/// marcar no tiene techo. Sin este tope el lote se encolaba entero y el límite
/// se descubría a mitad, cuando el daemon empezaba a rechazar por
/// `MAX_LIVE_TASKS`: con la mitad hecha y nada que dijera dónde se cortó.
#[tokio::test]
async fn un_lote_por_encima_del_tope_se_rechaza_entero() {
    const CUANTAS: usize = norte_ui_host::MAX_TRANSFER_BATCH + 8;
    let mut f = Falso::default();
    let mut entradas: Vec<(Vec<u8>, bool)> =
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)];
    for i in 0..CUANTAS {
        entradas.push((format!("f{i:04}.txt").into_bytes(), false));
    }
    f.pon("mem:///casa", entradas);
    f.pon("mem:///casa/docs", vec![(b"x.md".to_vec(), false)]);
    let backend = Arc::new(f);
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    // El destino, a mano y no con el ayudante de dos paneles: con un listado
    // de este tamaño el drenaje MUEVE la generación, y un `Activate` con la
    // del arranque llega rancio. Se espera a que el listado esté entero y se
    // lee la generación de ESA foto.
    let asentado = esperar_foto(&h, &mut sub, "el drenaje termina", |f| {
        listado_de(f, 2).total_rows.unwrap_or(0) >= CUANTAS as u64 + 2
    })
    .await;
    let b2 = listado_de(&asentado, 2);
    let docs = b2
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("el directorio está");
    let (key, generation) = (docs.key, b2.generation);
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host vivo");
    h.dispatch(UiAction::Activate {
        slot_id: 2,
        key,
        generation,
    })
    .await
    .expect("host vivo");
    esperar_foto(&h, &mut sub, "el destino aterriza en /casa/docs", |f| {
        listado_de(f, 2).path_display.ends_with("/casa/docs")
    })
    .await;
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host vivo");
    // Y el origen entero cargado: `invert_marks` marca lo CARGADO, y con el
    // drenaje a medias marcaría cien y el tope no se rozaría.
    esperar_foto(&h, &mut sub, "el origen está entero", |f| {
        listado_de(f, 1).total_rows.unwrap_or(0) >= CUANTAS as u64 + 2
    })
    .await;
    ejecutar_por_paleta(&h, &mut sub, "mark.invert").await;
    let ack = h.dispatch(tecla("F5")).await.expect("host vivo");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-batch-too-large"),
        "{ack:?}"
    );
    let foto = foto(&h, &mut sub).await;
    assert!(
        foto.dialogs.is_empty(),
        "no se abre un diálogo que promete algo que no se va a hacer"
    );
}

/// El corpus canónico contra el diálogo de APROBACIÓN (#277).
///
/// Es la superficie donde más caro sale mentir: lo que se lee ahí es lo único
/// que un humano tiene para decidir si un agente borra sus ficheros.
///
/// Las rutas llegan del daemon como TEXTO ya redactado, no como `VPath`, así
/// que el test las pasa antes por `display_lossy` —que es lo que hace
/// `norte_core::engine::span_path`, y no se puede llamar desde aquí porque el
/// host no depende del core (ADR 0066)—. Ese paso ES lo que hace que el test
/// signifique algo: alimentar bytes crudos encendería la bandera por un camino
/// que en producción no ocurre.
#[tokio::test]
async fn el_corpus_hostil_cruza_el_dialogo_de_aprobacion() {
    // Las cuatro que el lossy del daemon ALTERA, y `zwsp_twin` como CONTRASTE:
    // a ésa el lossy no la toca —es UTF-8 válido— y su bandera se tiene que
    // encender por el otro camino, el del enmascarado.
    let casos = [
        "lossy_collapse_ff",
        "lossy_collapse_fe",
        "rtl_override",
        "control_escape",
        "zwsp_twin",
    ];
    let corpus = norte_testkit::corpus::hostile_names();
    for id in casos {
        let n = corpus
            .iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("la fixture {id} está en el corpus"));
        let p = norte_proto::VPath::parse("mem:///casa")
            .expect("raíz")
            .join(norte_proto::Segment::new(n.bytes.clone()).expect("segmento"));
        // El paso del daemon: `span_path` es esto para cualquier autoridad sin
        // userinfo, que es el caso de un `mem://`.
        let redactada = p.display_lossy().clone();

        let falso = arbol_como_falso();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        *falso.aprobaciones.lock().expect("aprobaciones") = Some(rx);
        let (host, _snap) = host_arbol(Arc::new(falso)).await;
        let mut sub = host.subscribe();
        tx.send(norte_proto::methods::PolicyApprovalRequired {
            approval_id: 7,
            session: Some("agente-1".to_owned()),
            op: "delete".to_owned(),
            paths: vec![redactada.clone()],
            paths_total: 1,
            ttl_ms: 30_000,
            detail: norte_proto::methods::ApprovalDetail::default(),
        })
        .expect("el host escucha");

        let dialogos = siguientes_dialogos(&mut sub).await;
        let d = &dialogos[0];
        let linea = d.body.first().expect("la ruta está");
        sin_peligro(&linea.text, id, "una ruta del diálogo de aprobación");
        assert!(
            linea.hostile,
            "[{id}] la línea se pinta distinta de lo que hay y NO lo dice: {:?}",
            linea.text
        );
        // Y el plazo va en SU campo, nunca entre las rutas: entre ellas lo
        // podría suplantar un nombre de fichero (`approval_ttl_line_spoof`).
        assert!(
            d.deadline.is_some(),
            "[{id}] el plazo tiene que tener campo propio"
        );
        assert!(
            d.body.iter().all(|l| Some(&l.text) != d.deadline.as_ref()),
            "[{id}] el plazo se coló entre las rutas: {:?}",
            d.body
        );
    }
}

/// El directorio de un plugin roto es BYTES, y llegaba ya convertido (#265).
///
/// `PluginLoadError.dir` es un `String` que el core producía con un
/// `to_string_lossy` SIN marcar, así que un directorio llamado `caf\xff`
/// —fixture `lossy_collapse_ff` del corpus— cruzaba el wire ya con su
/// `U+FFFD`. Y `display_name` no lo recupera: pone `lossy` solo cuando
/// `from_utf8` falla y `masked` solo ante un peligro de terminal, y `U+FFFD`
/// no es ninguna de las dos cosas —es Specials—. La fila se declaraba fiel.
///
/// El test es un PAR, porque una sola fila no distingue el arreglo de la
/// heurística que había antes:
///
/// - `caf\xff` (bytes de verdad no-UTF-8) → la fila se MARCA. La heurística
///   vieja también lo marcaba, así que esta mitad sola no prueba nada.
/// - `caf\u{FFFD}` (un directorio que se llama ASÍ, en UTF-8 válido) → la fila
///   NO se marca. Es el falso positivo de la heurística —«la cadena lleva un
///   reemplazo, luego alguien convirtió»— y es la mitad que solo pasa con los
///   bytes delante.
///
/// Lo que este arreglo NO hace: distinguir `caf\xff` de `caf\xfe` al pintar.
/// `display_name` mapea todo byte inválido al mismo `U+FFFD`, así que las dos
/// siguen pintándose igual. Lo que se recupera es la MARCA, no la ortografía.
#[tokio::test]
async fn un_directorio_de_plugin_no_utf8_llega_marcado_y_sin_falso_positivo() {
    let crudos = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "lossy_collapse_ff")
        .expect("la fixture está")
        .bytes;
    assert!(
        std::str::from_utf8(&crudos).is_err(),
        "la premisa: son bytes que NO son UTF-8"
    );
    // Y el gemelo legítimo: un nombre que ES `U+FFFD` en disco, en UTF-8
    // válido. Nadie lo convirtió, así que marcarlo sería mentir.
    let honesto = "caf\u{FFFD}".as_bytes().to_vec();
    assert!(std::str::from_utf8(&honesto).is_ok());

    let mut backend = arbol_con_plugins(Vec::new(), &[]);
    {
        let f = std::sync::Arc::get_mut(&mut backend).expect("única referencia");
        for bytes in [&crudos, &honesto] {
            // Lo que el core manda: la cadena YA convertida, y los bytes al
            // lado. Las dos filas se distinguen por su texto; lo que NO se
            // puede distinguir por el texto es cuál de las dos se convirtió,
            // que es justo la pregunta.
            let convertida = String::from_utf8_lossy(bytes).into_owned();
            f.errores_de_carga
                .push((convertida.clone(), "el manifiesto no parsea".to_owned()));
            f.bytes_de_carga.insert(convertida, bytes.clone());
        }
    }
    let (h, _snap) = host_arbol(std::sync::Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let _ = siguiente_extensiones(&mut sub).await.expect("abre");
    let v = extensiones_cargadas(&mut sub).await;

    // Las dos cadenas colapsan a una sola clave, así que el falso llega a
    // mandar UNA fila: lo que se afirma es su bandera, que con los bytes del
    // nombre honesto tiene que ser FALSA.
    assert_eq!(v.errors.len(), 2, "las dos filas llegan: {:?}", v.errors);

    // La de bytes crudos: se marca, y con los bytes delante se marca por el
    // motivo correcto —`display_name` vio que no eran UTF-8— y no por la
    // heurística.
    let convertida = v
        .errors
        .iter()
        .find(|e| e.dir == String::from_utf8_lossy(&crudos))
        .expect("la fila de bytes crudos está");
    assert!(
        convertida.hostile,
        "lo pintado difiere de lo que hay y NO lo dice: {:?}",
        convertida.dir
    );

    // Y la honesta: NO se marca. Ésta es la mitad que solo pasa con los bytes
    // delante; con la heurística de la cadena salía marcada de más.
    let fila_limpia = v
        .errors
        .iter()
        .find(|e| e.dir == "caf\u{FFFD}")
        .expect("la fila honesta está");
    assert!(
        !fila_limpia.hostile,
        "un directorio que SE LLAMA `caf\u{FFFD}` no se convirtió: marcarlo es \
         el falso positivo que los bytes existen para quitar"
    );
    for c in fila_limpia.dir.chars() {
        assert!(
            !norte_encoding::is_terminal_hazard(c),
            "un peligro cruzó sin enmascarar: {:?}",
            fila_limpia.dir
        );
    }
}

/// Y la otra mitad, aislada: SIN bytes —un peer 0.52— la heurística marca esa
/// misma fila honesta, y ése es el falso positivo que #265 quita.
#[tokio::test]
async fn sin_los_bytes_un_nombre_honesto_con_reemplazo_sale_marcado_de_mas() {
    let honesto = "caf\u{FFFD}".to_owned();
    let mut backend = arbol_con_plugins(Vec::new(), &[]);
    std::sync::Arc::get_mut(&mut backend)
        .expect("única referencia")
        .errores_de_carga = vec![(honesto, "el manifiesto no parsea".to_owned())];
    // Deliberadamente SIN `bytes_de_carga`: es un daemon 0.52.
    let (h, _snap) = host_arbol(std::sync::Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let _ = siguiente_extensiones(&mut sub).await.expect("abre");
    let v = extensiones_cargadas(&mut sub).await;
    assert!(
        v.errors[0].hostile,
        "contra un peer viejo la heurística es lo único que hay, y marca de \
         más antes que de menos"
    );
}

/// #268 — dos marcas que son UN nombre en el destino se rechazan enteras.
///
/// En un ext4 `README.txt` y `readme.txt` son dos ficheros; en NTFS o APFS son
/// uno. Encolar las dos deja que una gane —cuál, no es determinista— y que la
/// otra falle sin explicación sobre un miembro arbitrario de la pareja.
///
/// El test corre las TRES parejas del corpus canónico, que son tres pliegues
/// distintos: caja ASCII, normalización NFC/NFD, y el pliegue completo de un
/// ext4 `+F`. Un arreglo que solo mirase la caja pasaría el primero y fallaría
/// los otros dos.
#[tokio::test]
async fn dos_marcas_que_pliegan_al_mismo_nombre_no_se_encolan() {
    let corpus = norte_testkit::corpus::hostile_names();
    let bytes_de = |id: &str| {
        corpus
            .iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("la fixture {id} está"))
            .bytes
            .clone()
    };
    let parejas = [
        ("ascii_case_twin_upper", "ascii_case_twin_lower"),
        ("nfc_e_acute", "nfd_e_acute"),
        ("ext4_full_fold_ss", "ext4_full_fold_es_zett"),
    ];
    for (a, b) in parejas {
        let (uno, otro) = (bytes_de(a), bytes_de(b));
        assert_ne!(uno, otro, "[{a}/{b}] la premisa: son bytes distintos");

        let mut f = Falso::default();
        f.pon(
            "mem:///casa",
            vec![
                (b"docs".to_vec(), true),
                (b"notas.txt".to_vec(), false),
                (uno.clone(), false),
                (otro.clone(), false),
            ],
        );
        f.pon("mem:///casa/docs", vec![(b"x.md".to_vec(), false)]);
        // El DESTINO pliega: un APFS, un NTFS o un ext4 `+F`. Sin este mando
        // el caso no se podía escribir, que es lo que la issue decía.
        f.capacidades.insert(
            "mem:///casa/docs".to_owned(),
            norte_proto::Capabilities {
                flags: norte_proto::CapabilityFlags::FULL_FOLD,
                max_path: None,
            },
        );
        let backend = Arc::new(f);
        let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
        let mut sub = h.subscribe();
        // Que el pliegue del destino haya llegado: se pide al aterrizar, no
        // delante del diálogo, así que hay que esperarlo.
        esperar_foto(&h, &mut sub, "el destino dice cómo pliega", |_| true).await;
        marca_todo(&h, &mut sub, 1).await;
        let ack = h.dispatch(tecla("F5")).await.expect("host vivo");

        assert!(
            matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-batch-folds-to-one"),
            "[{a}/{b}] {ack:?}"
        );
        assert!(
            backend
                .transferencias
                .lock()
                .expect("transferencias")
                .is_empty(),
            "[{a}/{b}] no se encoló ni una: el lote se rechaza ENTERO"
        );
    }
}

/// Y en un destino que NO pliega, las mismas dos marcas son dos ficheros y el
/// lote sale. La comprobación no puede costar la operación legítima.
#[tokio::test]
async fn dos_gemelos_de_caja_hacia_un_destino_sensible_si_se_encolan() {
    let corpus = norte_testkit::corpus::hostile_names();
    let bytes_de = |id: &str| {
        corpus
            .iter()
            .find(|n| n.id == id)
            .expect("la fixture está")
            .bytes
            .clone()
    };
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![
            (b"docs".to_vec(), true),
            (b"notas.txt".to_vec(), false),
            (bytes_de("ascii_case_twin_upper"), false),
            (bytes_de("ascii_case_twin_lower"), false),
        ],
    );
    f.pon("mem:///casa/docs", vec![(b"x.md".to_vec(), false)]);
    // Sin mando = ext4 corriente, que distingue la caja.
    let backend = Arc::new(f);
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    esperar_foto(&h, &mut sub, "el destino dice cómo pliega", |_| true).await;
    marca_todo(&h, &mut sub, 1).await;
    let ack = h.dispatch(tecla("F5")).await.expect("host vivo");
    assert!(
        matches!(&ack, ActionAck::Applied { .. }),
        "un ext4 distingue la caja: son dos ficheros y el lote es legítimo: {ack:?}"
    );
}

/// #311: calcular sumas en la ventana. La Task se encola con lo marcado, y
/// cuando su INFORME llega se abre un diálogo con una fila por fichero y la
/// opción de copiar la lista.
#[tokio::test]
async fn calcular_sumas_abre_el_dialogo_con_sus_filas() {
    let backend = arbol();
    // El informe que el falso daemon devolverá: un digest para `notas.txt`.
    *backend.sumas_informe.lock().expect("informe") =
        norte_proto::methods::FsChecksumReportResult {
            entries: vec![norte_proto::methods::ChecksumEntry {
                path: norte_proto::VPath::parse("mem:///casa/notas.txt").expect("wire"),
                digest: Some("ab".repeat(32)),
                miss: None,
            }],
            algo: norte_proto::methods::ChecksumAlgo::Sha256,
            pending: 0,
        };
    let (host, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = host.subscribe();

    host.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "k".to_owned(),
        ctrl: false,
        alt: true,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");

    let dialogos = siguientes_dialogos(&mut sub).await;
    assert_eq!(dialogos.len(), 1, "se abre UN diálogo con las sumas");
    assert_eq!(dialogos[0].body.len(), 1, "una fila por fichero");
    assert!(
        dialogos[0].body[0].text.contains("ababab"),
        "con su digest recortado: {:?}",
        dialogos[0].body[0].text
    );
    assert!(
        dialogos[0].choices.iter().any(|c| c.id == "confirm"),
        "y con la opción de COPIAR, que es lo único que se hace con una lista de digests"
    );
    assert_eq!(
        backend.sumas_pedidas.lock().expect("sumas").len(),
        1,
        "se pidió UN lote"
    );
}

/// Un informe PARCIAL —una Task cancelada deja `pending` por encima de cero—
/// no se compara con nada: acusar a ficheros que nadie llegó a leer es el peor
/// error posible en la herramienta que existe para comprobar.
#[tokio::test]
async fn un_informe_a_medias_no_abre_veredicto() {
    let backend = arbol();
    *backend.sumas_informe.lock().expect("informe") =
        norte_proto::methods::FsChecksumReportResult {
            entries: Vec::new(),
            algo: norte_proto::methods::ChecksumAlgo::Sha256,
            // Lo que deja una cancelación: la Task terminó y queda trabajo.
            pending: 3,
        };
    let (host, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = host.subscribe();

    host.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "k".to_owned(),
        ctrl: false,
        alt: true,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");
    // Se espera a que el informe HAYA vuelto: lo que se afirma es que con él
    // en la mano no se abre nada, no que todavía no hubiera llegado.
    hasta(&backend, "el informe de sumas pedido", |f| {
        (!f.sumas_informes_pedidos
            .lock()
            .expect("informes")
            .is_empty())
        .then_some(())
    })
    .await;
    asentar().await;
    let f = foto(&host, &mut sub).await;
    assert!(
        f.dialogs.is_empty(),
        "un informe a medias no abre ningún veredicto: {:?}",
        f.dialogs
    );
}

/// `alt+A`, el acorde que los tres presets nativos dan a `pane.chmod`.
fn alt_a() -> UiAction {
    UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "A".to_owned(),
        ctrl: false,
        alt: true,
        shift: false,
        meta: false,
    })
}

/// #314: la ventana cambia permisos. El diálogo lleva campo de texto —el modo
/// en octal—, dice sobre cuántas entradas va, y confirmar encola la Task con
/// el modo que se tecleó.
///
/// La regla de qué es un modo válido es la COMPARTIDA
/// (`norte_frontend::chmod::parse_mode`), la misma que usa la terminal: dos
/// lecturas distintas de `755` en dos frontends serían dos permisos distintos.
#[tokio::test]
async fn cambiar_permisos_teclea_y_encola() {
    let backend = arbol();
    let (host, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = host.subscribe();

    host.dispatch(alt_a()).await.expect("host vivo");
    let dialogos = siguientes_dialogos(&mut sub).await;
    let id = dialogos[0].id;
    assert!(
        dialogos[0].input.is_some(),
        "el diálogo de permisos dice que aquí se teclea"
    );

    host.dispatch(UiAction::DialogInput {
        id,
        text: "0750".to_owned(),
    })
    .await
    .expect("host vivo");
    let _ = siguientes_dialogos(&mut sub).await;
    host.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    let lotes = anotados(&backend, "el lote de permisos encolado", 1, |f| {
        f.permisos.lock().expect("permisos").clone()
    })
    .await;
    assert_eq!(lotes.len(), 1, "se encoló UN lote");
    assert_eq!(lotes[0].1, 0o750, "en OCTAL: 750, no 750 decimal");
    assert_eq!(lotes[0].0.len(), 1, "sobre lo que hay bajo el cursor");
}

/// Un modo que no vale no encola nada, y se dice.
#[tokio::test]
async fn un_modo_invalido_no_cambia_nada() {
    let backend = arbol();
    let (host, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = host.subscribe();

    host.dispatch(alt_a()).await.expect("host vivo");
    let dialogos = siguientes_dialogos(&mut sub).await;
    let id = dialogos[0].id;
    host.dispatch(UiAction::DialogInput {
        id,
        text: "899".to_owned(),
    })
    .await
    .expect("host vivo");
    let _ = siguientes_dialogos(&mut sub).await;
    let ack = host
        .dispatch(UiAction::Dialog {
            id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(&ack, ActionAck::Unavailable { .. }),
        "un 899 no es octal y se dice: {ack:?}"
    );
    asentar().await;
    assert!(
        backend.permisos.lock().expect("permisos").is_empty(),
        "y no se encoló nada"
    );
}
