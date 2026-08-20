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

fn dir() -> VPath {
    VPath::parse("mem:///casa").expect("vpath de test")
}

async fn host(nombres: Vec<&'static str>) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend: Falso::con(&nombres),
        initial_dir: dir(),
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
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
    let (h, _snap) = host(vec!["a"]).await;
    let ack = h
        .dispatch(UiAction::SelectRow {
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

/// Lo que el host aún no hace se DICE. Un renderer tiene que poder
/// distinguir «aún no» de «no pasó nada».
#[tokio::test]
async fn lo_no_implementado_se_dice() {
    let (h, _snap) = host(vec!["a"]).await;
    let ack = h
        .dispatch(UiAction::DialogInput {
            id: norte_ui_host::ModalId(1),
            text: "algo".to_owned(),
        })
        .await
        .expect("host vivo");
    assert!(matches!(ack, ActionAck::Unavailable { .. }));
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
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
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
                    return s;
                }
            }
            // Quedarse atrás no rompe la espera: significa «pide una foto»,
            // y una foto es justo lo que se está esperando.
            Update::Lagged => {}
        }
    }
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
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    let mas = tokio::time::timeout(std::time::Duration::from_millis(50), sub.recv()).await;
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
    // lotes son asíncronos y el número exacto no es el contrato.
    let mut sub = host.subscribe();
    let mut total = primeras;
    for _ in 0..100 {
        if total >= 5_000 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
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
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
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
    let (h, snap) = host_arbol(arbol()).await;
    let antes = listado(&snap).clone();
    // `F5` es copiar en el preset ortodoxo: existe, está ligada, y este host
    // todavía no muta nada.
    let ack = h.dispatch(tecla("F5")).await.expect("host vivo");
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
        layout: norte_frontend::layout::presets::tree(layout).expect("layout"),
        viewport,
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
            SlotView::Browser(_) => None,
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
    let despues = siguiente_foto(&mut sub).await;
    assert_eq!(despues.focus, Some(2));
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
                        if let norte_ui_host::dto::ViewChange::Tasks(t) = c {
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
                        if let norte_ui_host::dto::ViewChange::Dialogs(d) = c {
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
        })
        .await
        .expect("host vivo");
    assert!(matches!(primero, ActionAck::Applied { .. }));

    let segundo = h
        .dispatch(UiAction::Dialog {
            id,
            choice: "confirm".to_owned(),
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

    // Y solo se pidió UN borrado.
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
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
