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

async fn host(nombres: Vec<&'static str>) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend: Falso::con(&nombres),
        initial_dir: dir(),
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: norte_ui_host::ajustes_por_defecto(),
        paths: norte_ui_host::settings::HostPaths::default(),
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
        settings: norte_ui_host::ajustes_por_defecto(),
        paths: norte_ui_host::settings::HostPaths::default(),
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
        settings: norte_ui_host::ajustes_por_defecto(),
        paths: norte_ui_host::settings::HostPaths::default(),
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
        settings: norte_ui_host::ajustes_por_defecto(),
        paths: norte_ui_host::settings::HostPaths::default(),
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
        settings: norte_ui_host::ajustes_por_defecto(),
        paths: norte_ui_host::settings::HostPaths::default(),
        columns: columnas_de(&["name", "attr:posix.mode"]),
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
        settings: norte_ui_host::ajustes_por_defecto(),
        paths: norte_ui_host::settings::HostPaths::default(),
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
        settings: norte_ui_host::ajustes_por_defecto(),
        paths: norte_ui_host::settings::HostPaths::default(),
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
        settings: norte_ui_host::ajustes_por_defecto(),
        paths: norte_ui_host::settings::HostPaths::default(),
        columns: columnas_de(&["name", "plugin:acme.\u{202e}ftp/x"]),
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
        settings: norte_ui_host::ajustes_por_defecto(),
        paths: norte_ui_host::settings::HostPaths::default(),
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
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: norte_ui_host::ajustes_por_defecto(),
        paths: norte_ui_host::settings::HostPaths::default(),
        columns: norte_frontend::columns::ColumnsSettings::resolve(&cfg),
        effects: norte_ui_host::commands::Efectos::Completo,
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
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: norte_ui_host::ajustes_por_defecto(),
        paths: norte_ui_host::settings::HostPaths::default(),
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
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
    }
}

/// Un árbol con catálogo de extensiones.
fn arbol_con_plugins(
    plugins: Vec<norte_proto::methods::PluginInfo>,
    paginas: &[(&str, &str)],
) -> Arc<Falso> {
    let base = arbol();
    let mut f = Falso {
        plugins,
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
    let corribles: Vec<&norte_ui_host::dto::HelpActionView> =
        pagina.actions.iter().filter(|a| !a.opens_topic).collect();
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
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: norte_ui_host::ajustes_por_defecto(),
        paths,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
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

/// La sección de ubicaciones dice dónde vive cada cosa, marca lo que falta y
/// no enseña ni un valor.
#[tokio::test]
async fn las_ubicaciones_se_dicen_y_lo_que_falta_se_marca() {
    let tmp = tempfile::tempdir().expect("tmp");
    let existe = tmp.path().join("config");
    std::fs::create_dir(&existe).expect("mkdir");
    let no_existe = tmp.path().join("no-esta");
    let h = host_con_rutas(norte_ui_host::settings::HostPaths {
        config_layers: vec![
            (norte_ui_host::settings::ConfigLayer::User, existe.clone()),
            (norte_ui_host::settings::ConfigLayer::Project, no_existe),
        ],
        state_dir: None,
        logs_dir: None,
        socket: Some(tmp.path().join("daemon.sock")),
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
        config_layers: vec![(norte_ui_host::settings::ConfigLayer::User, hostil)],
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
