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
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
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
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
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
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("vim").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
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
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree(layout).expect("layout"),
        viewport,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
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
    })
    .await
    .expect("host vivo");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let creados = backend.creados.lock().expect("creados").clone();
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
    })
    .await
    .expect("host vivo");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
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
        d.body.iter().all(|l| !l.contains('\u{202E}')),
        "las rutas van enmascaradas: {:?}",
        d.body
    );
    assert!(
        d.body.len() >= 3,
        "y se dice que la lista viene recortada: {:?}",
        d.body
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
    })
    .expect("el host escucha");
    let id = siguientes_dialogos(&mut sub).await[0].id;

    host.dispatch(UiAction::Dialog {
        id,
        choice: "deny".to_owned(),
    })
    .await
    .expect("host vivo");
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
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
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        columns: vec![
            norte_frontend::columns::ColumnId::Builtin(norte_frontend::columns::Builtin::Name),
            norte_frontend::columns::ColumnId::Attr("posix.mode".to_owned()),
        ],
        effects: norte_ui_host::commands::Efectos::Completo,
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
    })
    .await
    .expect("host vivo");
    let vivas = siguientes_tasks(&mut sub).await;
    assert!(!vivas.is_empty(), "hay una task en el tablero");

    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(foto.tasks, vivas, "el snapshot lleva el tablero entero");
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
    // El sondeo sale solo, en cuanto el listado aterriza. Se le da tiempo al
    // viaje de ida y vuelta y se pide una foto: lo que importa es que la
    // pantalla acabe con las celdas llenas, no por qué mensaje llegó.
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
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
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
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
        layout: norte_frontend::layout::presets::tree("orthodox").expect("layout"),
        viewport: (200, 60),
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
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
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::SoloLectura,
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
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
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
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
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

    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
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

    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        if backend.sondeos.lock().expect("sondeos").len() >= 500 {
            return;
        }
    }
    let n = backend.sondeos.lock().expect("sondeos").len();
    panic!("el sondeo se paró en {n} de 500: la tanda no se re-armó");
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
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

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
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
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
    })
    .await
    .expect("host vivo");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let creados = backend.creados.lock().expect("creados").clone();
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

/// El id de una columna hostil llega acotado y enmascarado, y viaja así en
/// cada fila.
#[tokio::test]
async fn el_id_de_una_columna_hostil_no_cruza_crudo() {
    let backend = arbol();
    let (h, snap) = UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        columns: vec![
            norte_frontend::columns::ColumnId::Builtin(norte_frontend::columns::Builtin::Name),
            norte_frontend::columns::ColumnId::Plugin {
                plugin: "acme.\u{202e}ftp".to_owned(),
                column: "x".to_owned(),
            },
        ],
        effects: norte_ui_host::commands::Efectos::Completo,
    })
    .await
    .expect("arranca");
    drop(h);
    let b = listado(&snap);
    for c in &b.columns {
        assert!(
            !c.id.contains('\u{202e}'),
            "el id de la cabecera va crudo: {:?}",
            c.id
        );
    }
    for fila in &b.rows {
        for celda in &fila.cells {
            assert!(
                !celda.column.contains('\u{202e}'),
                "el id de la celda va crudo: {:?}",
                celda.column
            );
        }
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
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    // Antes de que llegue el contenido, se cierra.
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

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
        layout: arbol_sin_listado,
        viewport: (120, 40),
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
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
