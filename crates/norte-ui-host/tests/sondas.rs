//! Un hueco que SIGUE al cursor tiene un camino propio hasta el renderer.
//!
//! F1.3 del plan de paridad, y la decisión 2 de la ADR 0097: la ventana habla
//! por PARCHES, así que lo que no cabe en un parche se queda como estaba hasta
//! que algo provoque una foto entera. Para un panel que describe lo que el
//! cursor señala eso no es un retraso: es un panel que MIENTE, porque enseña
//! los atributos de un fichero mientras el listado resalta otro.
//!
//! Ya pasó dos veces. El visor acoplado (#291) y la hoja de atributos viajaban
//! «de gorra» en la foto que provocaba otro panel, así que una disposición con
//! hoja y sin visor la dejaba congelada en lo que hubiera al arrancar. La
//! reparación fue la misma en los dos casos —una SONDA después de cada mensaje
//! del actor, que es lo más parecido a un frame que tiene un host que solo
//! habla cuando algo cambia—, y este fichero es el guarda para que el tercer
//! panel que siga al cursor no repita el viaje.
//!
//! **Lo que se mide es el producto, no una lista.** Para cada panel que la
//! barra ofrece: se abre, se mueve el cursor del listado, y se mira si la
//! vista de ESE hueco cambió. Si cambió, el cambio tiene que haber llegado
//! solo. Un panel nuevo entra en la comprobación sin tocar este fichero,
//! porque la enumeración sale de la barra de paneles.
//!
//! Los dos tests se reparten las dos averías, y hace falta el par. Con la
//! sonda de la hoja QUITADA del bucle del actor —el contenido se calcula y no
//! se manda— falla el primero: cambia y no viajó. Con la sonda del visor
//! muerta del todo —no se calcula— falla el SEGUNDO, porque entonces no
//! cambia nada y el panel deja de parecer que sigue al cursor. Comprobado
//! saboteando cada una por separado.

use std::sync::Arc;
use std::time::Duration;

use norte_proto::VPath;
use norte_ui_host::action::UiAction;
use norte_ui_host::dto::{SlotView, UiUpdate};
use norte_ui_host::{UiHost, UiHostOptions, UiSubscription, Update, ViewSnapshot};

mod backend_falso;
use backend_falso::arbol_de_prueba;

/// Los paneles que NO siguen al cursor del listado, y por qué no.
///
/// Estar aquí es una AFIRMACIÓN comprobada, no una exención: el test falla
/// también al revés, si uno de éstos resulta que sí cambia al mover el cursor.
/// El motivo importa porque «no sigue» y «sigue y se quedó congelado» se ven
/// igual en pantalla el día que alguien lo rompa.
/// Kinds cuyo botón SÍ está en la barra —viene del registro compartido— pero
/// que esta ventana todavía no pinta, con la issue que lo cierra.
///
/// Estar aquí NO es una exención permanente: el gate de paridad
/// (`paridad.rs`, `APLAZADOS`) lleva la misma issue, así que implementarlo
/// obliga a quitarlo de los dos sitios. Se salta este barrido porque su
/// premisa —pulsar el botón abre un hueco— sólo vale para lo que la ventana
/// sabe pintar; contra un kind que no tiene, el host contesta «no
/// implementado», que es la respuesta correcta y no un fallo.
const SIN_VENTANA: &[(&str, u32)] = &[("timeline", 359)];

/// Kinds que este ARNÉS no puede sondear, y por qué.
///
/// Distinto de [`SIN_VENTANA`] y la diferencia importa: aquéllos son trabajo
/// que falta, con su issue. Éstos la ventana los hace perfectamente — lo que
/// no da el arnés es la precondición.
const SIN_SONDA: &[(&str, &str)] = &[(
    "terminal",
    "un shell se sienta en un directorio del sistema de ficheros, y los \
     paneles de este arnés son `mem:///`. El panel se NIEGA a abrirse ahí, \
     que es la conducta correcta y la misma que `app.terminal`: sondearlo \
     pediría un backend con rutas locales de verdad",
)];

const NO_SIGUEN: &[(&str, &str)] = &[
    (
        "places",
        "enseña volúmenes y favoritos, que son del host y no de la fila",
    ),
    (
        "processes",
        "tiene su PROPIO cursor sobre las tasks; el del listado no le dice nada",
    ),
    ("log", "enseña lo que este proceso registra, no una entrada"),
    (
        "tree",
        "sigue al DIRECTORIO, que sólo cambia con un `cd`, no con una fila",
    ),
    (
        "disk-map",
        "describe el DIRECTORIO que se está mirando, no la fila: mover el \
         cursor no cambia de qué está hecho lo que hay alrededor",
    ),
];

/// A dónde se lleva el cursor: un FICHERO de verdad.
///
/// No vale «una fila más abajo». El cursor nace sobre `..` y debajo hay otro
/// directorio, y el visor acoplado enseña la misma nota para los dos —«esto no
/// se lee»—, así que ese movimiento lo dejaría igual y el panel saldría como
/// que no sigue al cursor. Un verde por no haber preguntado.
const FICHERO: &str = "notas.txt";

/// Cuántas filas hay del cursor a la fila que se llama así.
fn hasta(snap: &ViewSnapshot, nombre: &str) -> i64 {
    let SlotView::Browser(b) = snap
        .slots
        .iter()
        .find(|s| matches!(s, SlotView::Browser(_)))
        .expect("hay listado")
    else {
        unreachable!("filtrado arriba")
    };
    let cursor = b.cursor.map_or(0, |k| usize::try_from(k.0).unwrap_or(0));
    let destino = b
        .rows
        .iter()
        .position(|r| r.display_name == nombre)
        .unwrap_or_else(|| panic!("`{nombre}` no está en el listado"));
    i64::try_from(destino).unwrap_or(0) - i64::try_from(cursor).unwrap_or(0)
}

/// Lo que se observó de un panel al mover el cursor debajo.
#[derive(Debug)]
struct Observacion {
    /// ¿Su vista es distinta después de mover el cursor?
    cambia: bool,
    /// ¿Llegó ese cambio SOLO, sin que el renderer pidiera nada?
    viaja: bool,
}

/// La vista del hueco de `kind` en una foto, si está.
///
/// El mapa kind → variante es la única parte escrita a mano, y no se puede
/// derivar: el wire llama `preview` a lo que la disposición llama `viewer`,
/// que es justo el tipo de sinónimo que un `match` exhaustivo no ve.
fn vista_de(snap: &ViewSnapshot, kind: &str) -> Option<SlotView> {
    snap.slots
        .iter()
        .find(|s| match (kind, s) {
            ("metadata", SlotView::Metadata(_))
            | ("places", SlotView::Places(_))
            | ("tree", SlotView::Tree(_))
            | ("processes", SlotView::Processes { .. })
            | ("log", SlotView::Log(_))
            | ("viewer", SlotView::Preview(_))
            | ("disk-map", SlotView::DiskMap(_))
            | ("timeline", SlotView::Timeline(_))
            | ("terminal", SlotView::Terminal(_)) => true,
            (_, SlotView::Unsupported { kind_name, .. }) => kind_name == kind,
            _ => false,
        })
        .cloned()
}

/// Vacía la cola de lo que haya pendiente, sin esperar a nada.
///
/// Solo para ponerse al día tras abrir un panel o mover el foco: aquí no se
/// decide nada, así que un mensaje que llegue tarde no rompe el test — lo verá
/// la espera de después.
async fn vacia(sub: &mut UiSubscription) {
    while let Ok(recibido) = tokio::time::timeout(Duration::from_millis(50), sub.recv()).await {
        let _ = recibido.expect("el host sigue vivo");
    }
}

/// Espera a que llegue SOLA una foto en la que la vista de `kind` ya no es
/// `antes`, o `None` si en todo el plazo no llega ninguna.
///
/// Un plazo de silencio no vale para esto. «No ha llegado nada en 150 ms» y
/// «este panel no sigue al cursor» se ven igual, y bajo la carga del gate una
/// sonda que lee un fichero tarda más que eso: el test se pondría rojo
/// diciendo «panel congelado» por una carrera perdida, que es exactamente la
/// clase de rojo intermitente que este repositorio trata como un bug.
///
/// Así el camino verde es inmediato —la foto ya está esperando— y el plazo
/// largo solo se gasta en los paneles que de verdad no cambian, donde su
/// respuesta es la correcta.
async fn espera_cambio(sub: &mut UiSubscription, kind: &str, antes: &SlotView) -> Option<SlotView> {
    let hasta = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let queda = hasta.saturating_duration_since(tokio::time::Instant::now());
        if queda.is_zero() {
            return None;
        }
        let Ok(recibido) = tokio::time::timeout(queda, sub.recv()).await else {
            return None;
        };
        if let Update::Message(m) = recibido.expect("el host sigue vivo")
            && let UiUpdate::Snapshot(s) = m.payload
            && let Some(ahora) = vista_de(&s, kind)
            && &ahora != antes
        {
            return Some(ahora);
        }
    }
}

/// Pide una foto entera y espera a que llegue.
async fn pide_foto(host: &UiHost, sub: &mut UiSubscription) -> ViewSnapshot {
    host.dispatch(UiAction::Resync).await.expect("host vivo");
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(Duration::from_millis(500), sub.recv())
            .await
            .expect("una foto, no un cuelgue")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Snapshot(s) = m.payload
        {
            return *s;
        }
    }
    panic!("no llegó ninguna foto");
}

/// Arranca un host con el listado solo, sobre el árbol de prueba.
async fn arranca() -> (UiHost, ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend: Arc::new(arbol_de_prueba()),
        initial_dir: VPath::parse("mem:///casa").expect("vpath"),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: norte_ui_host::ajustes_por_defecto(),
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

/// La observación de un kind, EN CAJA.
///
/// El futuro de un `async fn` viaja entero en cada `await`, y este monta un
/// host y guarda dos fotos: al crecer el snapshot pasó de los 16 KB que
/// clippy tolera. La caja va aquí, en la raíz, y no en los dos bucles que lo
/// llaman.
fn observa(kind: &str) -> std::pin::Pin<Box<dyn std::future::Future<Output = Observacion> + '_>> {
    Box::pin(observa_inner(kind))
}

/// Abre el panel de `kind`, mueve el cursor del listado, y mira qué pasó.
async fn observa_inner(kind: &str) -> Observacion {
    let (host, primera) = arranca().await;
    let mut sub = host.subscribe();

    // La barra de paneles es la enumeración: un click vuelve como el ÍNDICE
    // en su lista, que es lo único que el renderer puede nombrar.
    let boton = primera
        .panel_bar
        .buttons
        .iter()
        .position(|b| b.kind == kind)
        .unwrap_or_else(|| panic!("`{kind}` no está en la barra de paneles"));
    host.dispatch(UiAction::PanelBarActivate {
        button: u32::try_from(boton).expect("cabe"),
    })
    .await
    .expect("host vivo");
    vacia(&mut sub).await;

    // El foco vuelve al listado: abrir un panel que se enfoca se lo lleva, y
    // `MoveCursor` sobre un hueco que no es el activo se contesta `Stale` —
    // o sea que sin esto el cursor no se movería y TODOS los paneles saldrían
    // «no sigue», que es un verde que no prueba nada.
    host.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host vivo");
    vacia(&mut sub).await;

    let foto = pide_foto(&host, &mut sub).await;
    let delta = hasta(&foto, FICHERO);
    let antes = vista_de(&foto, kind).unwrap_or_else(|| panic!("`{kind}` no se abrió"));

    host.dispatch(UiAction::MoveCursor { slot_id: 1, delta })
        .await
        .expect("host vivo");

    // Primero, la pregunta que de verdad se hace: ¿llegó SOLO un cambio de
    // este panel? Si llegó, ya está contestado todo y no hay plazo que gastar.
    if espera_cambio(&mut sub, kind, &antes).await.is_some() {
        return Observacion {
            cambia: true,
            viaja: true,
        };
    }

    // No llegó nada. Ahora se distingue el panel que no sigue al cursor —lo
    // correcto— del panel congelado: se PIDE la foto y se mira si su vista era
    // otra todo este rato.
    let despues = pide_foto(&host, &mut sub).await;
    let despues = vista_de(&despues, kind).unwrap_or_else(|| panic!("`{kind}` sigue abierto"));
    Observacion {
        cambia: antes != despues,
        viaja: false,
    }
}

/// El guarda: si la vista de un panel depende del cursor, el cambio llega solo.
///
/// Lo que falla aquí es un panel congelado, y se lee tal cual: «cambia al
/// mover el cursor y el cambio no llegó solo».
#[tokio::test(flavor = "multi_thread")]
async fn cada_hueco_que_sigue_al_cursor_tiene_sonda() {
    let (host, primera) = arranca().await;
    let kinds: Vec<String> = primera
        .panel_bar
        .buttons
        .iter()
        .map(|b| b.kind.clone())
        .collect();
    drop(host);
    assert!(!kinds.is_empty(), "la barra de paneles no ofrece nada");

    for kind in &kinds {
        if SIN_VENTANA.iter().any(|(k, _)| k == kind) || SIN_SONDA.iter().any(|(k, _)| k == kind) {
            continue;
        }
        let o = observa(kind).await;
        if o.cambia {
            assert!(
                o.viaja,
                "`{kind}` cambia al mover el cursor y el cambio NO llegó solo: \
                 se queda congelado hasta que otra cosa provoque una foto entera"
            );
        }
    }
}

/// Y al revés: la lista de los que no siguen es una afirmación, no una
/// exención. Un panel que empiece a seguir al cursor sin decirlo aquí falla,
/// aunque tenga sonda — porque entonces el motivo escrito es falso.
#[tokio::test(flavor = "multi_thread")]
async fn la_lista_de_los_que_no_siguen_esta_al_dia() {
    let (host, primera) = arranca().await;
    let kinds: Vec<String> = primera
        .panel_bar
        .buttons
        .iter()
        .map(|b| b.kind.clone())
        .collect();
    drop(host);

    for kind in &kinds {
        if SIN_VENTANA.iter().any(|(k, _)| k == kind) || SIN_SONDA.iter().any(|(k, _)| k == kind) {
            continue;
        }
        let declarado = NO_SIGUEN.iter().find(|(k, _)| k == kind);
        let o = observa(kind).await;
        match declarado {
            Some((_, motivo)) => assert!(
                !o.cambia,
                "`{kind}` está declarado como que no sigue al cursor ({motivo}), \
                 pero su vista cambió al moverlo"
            ),
            None => assert!(
                o.cambia,
                "`{kind}` no está en NO_SIGUEN, así que debería seguir al cursor, \
                 y su vista no se movió"
            ),
        }
    }
}
