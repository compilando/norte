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
    host_de(Arc::new(f)).await
}

async fn host_de(backend: Arc<Falso>) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: VPath::parse("mem:///casa").expect("vpath"),
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("orthodox").expect("layout"),
        viewport: (200, 60),
        // La fila `..` apagada: estos tests razonan sobre índices de
        // listado, y una fila más al principio los desplazaría todos sin
        // decir nada de lo que prueban.
        settings: {
            let mut cfg = norte_ui_host::ajustes_por_defecto();
            cfg.common.ui_parent_entry = Some(false);
            cfg
        },
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

fn bytes(u: &norte_ui_host::BridgeEnvelope<UiUpdate>) -> usize {
    serde_json::to_vec(u).expect("el sobre serializa").len()
}

/// Espera al primer parche que cumpla `pred`, saltándose el relleno del
/// listado (el host sigue drenando cien mil entradas por detrás) y los avisos
/// de retraso.
///
/// Desde #252 el relleno ya no publica un parche por lote —solo cuando la
/// ventana visible cambia de verdad, más el último—, así que lo que hay que
/// saltarse son dos o tres parches y no doscientos. El ayudante se queda
/// porque esos dos siguen pudiendo colarse entre la acción y su respuesta;
/// lo que desapareció es la ráfaga que forzaba un `Lagged` y una foto
/// entera.
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
        generation: generacion_de(&snap.slots[0]),
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

/// La generación del hueco, que en esta disposición es siempre un listado.
///
/// Un hueco que no lo sea aquí sería un test que dejó de probar lo que dice,
/// así que se PARA en vez de mandar una generación inventada que el host
/// rechazaría como obsoleta.
fn generacion_de(s: &norte_ui_host::dto::SlotView) -> u64 {
    match s {
        norte_ui_host::dto::SlotView::Browser(b) => b.generation,
        otro => panic!("el hueco 0 de esta disposición es un listado: {otro:?}"),
    }
}

/// Cuántas FILAS lleva un hueco en la foto.
///
/// `match` exhaustivo y SIN comodín a propósito. El guardia de tamaño mide
/// que la foto inicial no lleve el directorio entero, y un `_ => 0` lo
/// desarmaba en silencio: los huecos de sitios y de procesos también mandan
/// filas y contaban cero, así que la cuenta medía un quinto del mensaje. Y de
/// paso perdía el canario que obliga a mirar esto al crecer `SlotView`.
fn filas_de(s: &norte_ui_host::dto::SlotView) -> usize {
    use norte_ui_host::dto::SlotView;
    match s {
        SlotView::Browser(b) => b.rows.len(),
        SlotView::Places(p) => p.rows.len(),
        SlotView::Tree(t) => t.rows.len(),
        SlotView::Metadata(m) => m.fields.len(),
        // El registro sí lleva las suyas, y por eso cuenta: manda una VENTANA
        // del anillo, no el anillo — dos mil líneas por parche es justo lo que
        // esta cuenta existe para que nadie pueda meter sin enterarse.
        SlotView::Log(l) => l.lines.len(),
        // El visor acoplado (#291) lleva las líneas del fichero, ENTERAS
        // hasta el tope del puente: cuentan, y la cota de arriba las acota.
        SlotView::Preview(p) => p.viewer.as_ref().map_or(0, |v| v.lines.len()),
        // El panel de procesos no lleva sus filas en el hueco: las lleva
        // `ViewSnapshot::tasks`, que es una sola lista para toda la pantalla.
        SlotView::Processes { .. } | SlotView::Unsupported { .. } => 0,
    }
}

/// La foto de arranque tampoco lleva el directorio entero.
#[tokio::test]
async fn la_foto_inicial_no_lleva_cien_mil_filas() {
    let (_h, snap) = host_grande().await;
    let n = serde_json::to_vec(&snap).expect("serializa").len();
    let filas: usize = snap.slots.iter().map(filas_de).sum();
    assert!(
        filas <= 2 * 64,
        "los dos listados mandan su ventana ({filas} filas), no {GRANDE}"
    );
    assert!(n < 64 * 1024, "la foto inicial son {n} bytes");
    println!("foto inicial con {GRANDE} entradas: {n} bytes, {filas} filas");
}

/// Rellenar un listado grande NO publica un parche por lote (#252).
///
/// Se drena en lotes de 500 y cada uno publicaba su parche de filas: en un
/// directorio de cien mil entradas eso son doscientos parches en ráfaga
/// contra un canal de 64, así que cualquier suscriptor que no drene a esa
/// velocidad recibe `Lagged` y tiene que pedir una foto entera. Y casi todos
/// llevaban las MISMAS filas, porque lo que se mezclaba caía muy por debajo
/// de la ventana visible: el renderer repintaba doscientas veces lo mismo.
///
/// El tope es holgado a propósito. Lo que se afirma no es un número fino
/// —cuántas veces cambia la ventana depende del orden en que el provider
/// entregue— sino que ya no hay UNO POR LOTE.
#[tokio::test]
async fn el_relleno_no_publica_un_parche_por_lote() {
    let cuantas = GRANDE;
    // La puerta del doble detiene el stream justo tras la primera página, así
    // que al suscribirse el relleno NO ha empezado: sin ella, el falso drena
    // cien mil entradas en memoria antes de que este test mire.
    let puerta = Arc::new(backend_falso::Puerta::default());
    let mut f = Falso::default();
    let nombres: Vec<(Vec<u8>, bool)> = (0..cuantas)
        .map(|i| (format!("fichero-{i:06}.txt").into_bytes(), false))
        .collect();
    f.pon("mem:///casa", nombres);
    f.puerta_drenaje = Some(Arc::clone(&puerta));
    let (h, _snap) = host_de(Arc::new(f)).await;
    let mut sub = h.subscribe();
    puerta.abrir();

    let mut parches_de_filas = 0usize;
    let mut visto_el_final = false;
    for _ in 0..2000 {
        // Los plazos NO son un ritmo: son «el host se ha callado». Treinta
        // segundos para que el relleno ARRANQUE —cien mil entradas se mezclan
        // en doscientos lotes, y con los otros tests de este fichero a la vez
        // eso tarda lo que tarda—, y cinco para la cola, que es lo que evita
        // que el test se pase medio minuto esperando a nada.
        let plazo = if parches_de_filas == 0 { 30 } else { 5 };
        let Ok(Some(siguiente)) =
            tokio::time::timeout(std::time::Duration::from_secs(plazo), sub.recv()).await
        else {
            break;
        };
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let ViewChange::Rows { rows, .. } = c {
                    parches_de_filas += 1;
                    // El último parche del relleno trae la ventana completa.
                    visto_el_final = !rows.is_empty();
                }
            }
        }
    }

    let lotes = cuantas / 500;
    assert!(
        visto_el_final,
        "el relleno tiene que publicar al menos un parche de filas"
    );
    assert!(
        parches_de_filas < lotes / 4,
        "{parches_de_filas} parches de filas para {lotes} lotes: el relleno \
         sigue publicando uno por lote"
    );
}
