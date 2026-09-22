use super::*;

// ---------------------------------------------------------------------------
// El panel de registro (#326).
// ---------------------------------------------------------------------------

/// Emite unas líneas DENTRO del anillo, por su camino de verdad.
///
/// Por la capa de `tracing` y no por un `push` directo: el anillo no expone
/// uno, y no debe — el filtro por el que pasa la capa es donde vive la cota de
/// `suppaftp`, que loguea `PASS <contraseña>` a nivel TRACE. Un atajo para los
/// tests que se saltara esa cota probaría un camino que no existe.
pub(super) fn con_lineas(anillo: &norte_config::logring::LogRing, f: impl FnOnce()) {
    use tracing_subscriber::layer::SubscriberExt as _;
    let s = tracing_subscriber::registry().with(norte_config::logring::ring_layer(anillo));
    tracing::subscriber::with_default(s, f);
}

/// Un host con un anillo de registro montado y unas cuantas líneas dentro.
pub(super) async fn host_con_registro() -> (UiHost, norte_config::logring::LogRing) {
    host_con_backend_y_registro(Falso::con(&["a"])).await
}

/// Lo mismo, con un doble que el test ha armado: es lo que hace falta para
/// la mitad remota (#328), donde lo que se prueba es qué contesta el daemon.
pub(super) async fn host_con_backend_y_registro(
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
pub(super) async fn host_con_backend_y_anillo(
    backend: Arc<Falso>,
    anillo: Option<norte_config::logring::LogRing>,
) -> UiHost {
    let h = UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
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
pub(super) async fn tecla_registro(h: &UiHost) {
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
pub(super) fn registro(snap: &norte_ui_host::ViewSnapshot) -> &norte_ui_host::dto::LogSlotView {
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
pub(super) fn linea_wire(
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
pub(super) async fn foto_registro(h: &UiHost) -> norte_ui_host::dto::LogSlotView {
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
pub(super) async fn sondear(h: &UiHost) {
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
pub(super) async fn poner_fuente(h: &UiHost, fuente: &str) {
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
pub(super) async fn entrar_en_docs(h: &UiHost, snap: &norte_ui_host::ViewSnapshot) {
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
pub(super) fn pedira_el_secreto(f: &Falso) {
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

    // Ni por el camino de un FORMULARIO (puente 91), que es el otro sitio
    // donde el host SÍ guarda lo que se escribe: un diálogo de contraseña no
    // lleva campos, y `tocar_campo_de_dialogo` lo comprueba antes de tocar
    // nada. Sin este test, la invariante de #327 quedaba enforzada en dos
    // sitios y probada en uno — el viejo.
    assert!(d.fields.is_empty(), "una contraseña no es un formulario");
    let ack = h
        .dispatch(UiAction::DialogField {
            id: d.id,
            field: "name".to_owned(),
            value: norte_ui_host::action::DialogFieldValue::Text {
                text: "s3cr3t".to_owned(),
            },
        })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Modal
        },
        "un campo de contraseña tampoco se teclea por `dialog_field`"
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
pub(super) async fn foto_hasta_notice(
    sub: &mut norte_ui_host::controller::UiSubscription,
    clave: &str,
) -> String {
    for _ in 0..40 {
        match tokio::time::timeout(ESPERA_MAX, sub.recv())
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
        if let Ok(Some(Update::Message(m))) = tokio::time::timeout(ESPERA_MAX, sub.recv()).await
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
        if let Ok(Some(Update::Message(m))) = tokio::time::timeout(ESPERA_MAX, sub.recv()).await
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
        if let Ok(Some(Update::Message(m))) = tokio::time::timeout(ESPERA_MAX, sub.recv()).await
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
        pause: None,
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
        initial_dir_pedido: false,
        attach: false,
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
