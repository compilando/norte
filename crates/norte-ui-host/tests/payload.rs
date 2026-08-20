//! Cuánto PESA lo que cruza el bridge. El presupuesto de la tarea 3.6.
//!
//! Estas cifras son la mitad de la pregunta que el spike de Tauri existe para
//! contestar: una rebanada vertical puede verse perfecta y ser un no-go si
//! mover el cursor en un directorio de cien mil entradas manda cien mil filas
//! en JSON. Se miden aquí, en el host, porque es donde se generan — y así la
//! medida no depende de que haya pantalla.

use std::sync::Arc;

use norte_proto::VPath;
use norte_ui_host::action::UiAction;
use norte_ui_host::controller::{UiHost, UiHostOptions, Update};
use norte_ui_host::dto::{UiUpdate, ViewChange};

mod backend_falso;
use backend_falso::Falso;

/// El tope de un parche de cursor (tarea 3.6).
const TOPE_CURSOR: usize = 16 * 1024;

/// Entradas del directorio grande.
const GRANDE: usize = 100_000;

async fn host_grande() -> (UiHost, norte_ui_host::ViewSnapshot) {
    let mut f = Falso::default();
    let nombres: Vec<(Vec<u8>, bool)> = (0..GRANDE)
        .map(|i| (format!("fichero-{i:06}.txt").into_bytes(), false))
        .collect();
    f.pon("mem:///casa", nombres);
    UiHost::start(UiHostOptions {
        backend: Arc::new(f),
        initial_dir: VPath::parse("mem:///casa").expect("vpath"),
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("orthodox").expect("layout"),
        viewport: (200, 60),
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
    })
    .await
    .expect("arranca")
}

fn bytes(u: &norte_ui_host::BridgeEnvelope<UiUpdate>) -> usize {
    serde_json::to_vec(u).expect("el sobre serializa").len()
}

/// Espera al primer parche que cumpla `pred`, saltándose el relleno del
/// listado (el host sigue drenando cien mil entradas por detrás) y los avisos
/// de retraso, que en un directorio así son parte del paisaje.
async fn siguiente_parche(
    sub: &mut norte_ui_host::UiSubscription,
    pred: impl Fn(&norte_ui_host::dto::ViewPatch) -> bool,
) -> norte_ui_host::BridgeEnvelope<UiUpdate> {
    for _ in 0..2000 {
        let siguiente = tokio::time::timeout(std::time::Duration::from_secs(10), sub.recv())
            .await
            .expect("una actualización antes del plazo")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
            && pred(p)
        {
            return *m;
        }
    }
    panic!("no llegó el parche que se esperaba");
}

/// Mover el cursor manda el cursor, y el cursor cabe de sobra en el tope.
#[tokio::test]
async fn un_parche_de_cursor_no_llega_ni_a_un_kilobyte() {
    let (h, _snap) = host_grande().await;
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 0,
        count: 60,
    })
    .await
    .expect("host vivo");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::MoveCursor {
        slot_id: 1,
        delta: 1,
    })
    .await
    .expect("host vivo");

    let m = siguiente_parche(&mut sub, |p| {
        p.changes
            .iter()
            .all(|c| matches!(c, ViewChange::Cursor { .. }))
    })
    .await;
    let n = bytes(&m);
    assert!(
        n <= TOPE_CURSOR,
        "un parche de cursor son {n} bytes, y el tope es {TOPE_CURSOR}"
    );
    // Y sobre todo: NO lleva filas.
    let UiUpdate::Patch(p) = &m.payload else {
        panic!("mover el cursor es un parche, no una foto");
    };
    assert!(
        p.changes
            .iter()
            .all(|c| matches!(c, ViewChange::Cursor { .. })),
        "y lo único que lleva es el cursor: {:?}",
        p.changes
    );
    println!("parche de cursor sobre {GRANDE} entradas: {n} bytes");
}

/// Marcar re-manda la VENTANA, no el directorio: el coste es del tamaño de la
/// pantalla, no del tamaño del directorio.
#[tokio::test]
async fn un_parche_de_filas_pesa_lo_que_la_ventana() {
    let (h, snap) = host_grande().await;
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 0,
        count: 60,
    })
    .await
    .expect("host vivo");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::ToggleMark {
        slot_id: 1,
        key: norte_ui_host::RowKey(3),
        generation: match &snap.slots[0] {
            norte_ui_host::dto::SlotView::Browser(b) => b.generation,
            norte_ui_host::dto::SlotView::Unsupported { .. } => 0,
        },
    })
    .await
    .expect("host vivo");
    // Del hueco 1, que es al que se le declaró la ventana: la disposición
    // tiene dos listados y el otro manda las suyas con SU tamaño.
    let m = siguiente_parche(&mut sub, |p| {
        p.changes
            .iter()
            .any(|c| matches!(c, ViewChange::Rows { slot_id: 1, .. }))
    })
    .await;
    let n = bytes(&m);
    let UiUpdate::Patch(p) = &m.payload else {
        unreachable!("filtrado arriba")
    };
    let filas: usize = p
        .changes
        .iter()
        .map(|c| match c {
            ViewChange::Rows {
                slot_id: 1, rows, ..
            } => rows.len(),
            _ => 0,
        })
        .sum();
    assert!(
        filas <= 60,
        "viajan las visibles ({filas}), no las {GRANDE}"
    );
    assert!(
        n < 32 * 1024,
        "una ventana de 60 filas son {n} bytes, que no puede depender del directorio"
    );
    println!("parche de {filas} filas sobre {GRANDE} entradas: {n} bytes");
}

/// La foto de arranque tampoco lleva el directorio entero.
#[tokio::test]
async fn la_foto_inicial_no_lleva_cien_mil_filas() {
    let (_h, snap) = host_grande().await;
    let n = serde_json::to_vec(&snap).expect("serializa").len();
    let filas: usize = snap
        .slots
        .iter()
        .map(|s| match s {
            norte_ui_host::dto::SlotView::Browser(b) => b.rows.len(),
            norte_ui_host::dto::SlotView::Unsupported { .. } => 0,
        })
        .sum();
    assert!(
        filas <= 2 * 64,
        "los dos listados mandan su ventana ({filas} filas), no {GRANDE}"
    );
    assert!(n < 64 * 1024, "la foto inicial son {n} bytes");
    println!("foto inicial con {GRANDE} entradas: {n} bytes, {filas} filas");
}
