use super::*;

// ---------------------------------------------------------------------------
// Copiar y mover (tarea 5.1 de la fase 5).
//
// El renderer JAMÁS nombra un fichero: manda `pane.copy` y el host deriva el
// origen de las marcas del hueco activo y el destino del hueco con el rol
// `Target`. Ni una ruta cruza desde la webview.
// ---------------------------------------------------------------------------

/// El listado de UN hueco concreto de una foto.
pub(super) fn listado_de(
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

/// El montaje de dos paneles con destino aparte, EN CAJA.
///
/// Doce tests lo esperan, y el futuro de un `async fn` viaja entero en cada
/// `await`: al crecer el snapshot pasó de los 16 KB que clippy tolera y los
/// doce sitios se pusieron rojos a la vez. La caja los arregla de una, y en
/// el sitio correcto —el ayudante— en lugar de repartir doce `Box::pin` por
/// los tests que solo lo llaman.
pub(super) fn dos_paneles_con_destino_aparte(
    backend: Arc<Falso>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = (UiHost, norte_ui_host::ViewSnapshot)>>> {
    Box::pin(dos_paneles_con_destino_aparte_inner(backend))
}

/// Dos paneles, con el DESTINO ya en otro directorio: el escenario real de
/// una copia. Devuelve la foto de después.
async fn dos_paneles_con_destino_aparte_inner(
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

/// El diálogo de copiar dice si NO CABE y si el destino no sabe confinar.
///
/// Las dos líneas son del terminal desde #149 y #164 y la ventana no tenía
/// ninguna: te enterabas por una task fallida, o no te enterabas. Las dos
/// preguntas son I/O, así que el diálogo se abre sin ellas y se rellenan
/// cuando vuelven — el mismo reparto que hace el terminal en su bucle.
///
/// Fallan DISTINTO, y es deliberado: el espacio se traga el fallo («no lo sé»
/// se dice callando) y el confinamiento no, porque ahí el silencio SIGNIFICA
/// «este destino sujeta sus escrituras» y tragárselo sería afirmarlo sin
/// saberlo.
#[tokio::test]
async fn el_dialogo_de_copia_avisa_de_espacio_y_de_confinamiento() {
    let (h, mut sub, _b) = Box::pin(dos_paneles_en_disco(volumen_lleno(), sin_confinar())).await;
    marca_los_ficheros(&h, &mut sub, 1).await;

    h.dispatch(tecla("F5")).await.expect("host vivo");
    let _ = siguientes_dialogos(&mut sub).await;
    asentar().await;

    let avisos = foto_hasta(&h, &mut sub, "el diálogo con sus avisos", |s| {
        // El ÚLTIMO, que es el que el renderer pinta: con uno solo coinciden,
        // y así siguen coincidiendo el día que se apilen.
        match &s.dialogs.last()?.dest_check {
            norte_ui_host::dto::DestCheckView::Done { warnings } if !warnings.is_empty() => {
                Some(warnings.clone())
            }
            _ => None,
        }
    })
    .await;
    assert_eq!(avisos.len(), 2, "las dos líneas: {avisos:?}");
    assert!(
        avisos[0].contains("libres"),
        "la del espacio lleva los dos números: {avisos:?}"
    );
    assert!(
        avisos[1].contains("symlink"),
        "la del confinamiento dice de qué protege: {avisos:?}"
    );
}

/// Y un destino que SÍ cabe y SÍ confina no dice nada.
///
/// La mitad del contrato que se olvida: una línea en cada copia es ruido, y
/// el ruido enseña a saltarse la línea justo el día que dice algo.
#[tokio::test]
async fn un_destino_que_cabe_y_confina_no_dice_nada() {
    let (h, mut sub, backend) =
        Box::pin(dos_paneles_en_disco(volumen_de_sobra(), confinando())).await;
    marca_los_ficheros(&h, &mut sub, 1).await;

    h.dispatch(tecla("F5")).await.expect("host vivo");
    let dialogos = siguientes_dialogos(&mut sub).await;
    assert_eq!(dialogos.len(), 1, "el diálogo se abre igual");
    asentar().await;

    let d = foto_hasta(&h, &mut sub, "el destino ya comprobado", |s| {
        let d = s.dialogs.last()?;
        matches!(d.dest_check, norte_ui_host::dto::DestCheckView::Done { .. }).then(|| d.clone())
    })
    .await;
    assert_eq!(
        d.dest_check,
        norte_ui_host::dto::DestCheckView::Done {
            warnings: Vec::new()
        },
        "nada que avisar, así que nada que decir"
    );
    // Y se PREGUNTÓ. Sin esto el test sigue verde si alguien borra el
    // sondeo: callar por no tener nada que decir y callar por no haber
    // mirado se pintan igual, que es justo lo que este campo separa.
    assert!(
        backend
            .volumenes_pedidos
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0,
        "el silencio es una RESPUESTA, no una omisión"
    );
}

/// Un volumen que no contesta no es un volumen lleno.
///
/// `free_bytes: None` significa «no contestó a tiempo», JAMÁS cero:
/// confundirlos convierte cada montaje lento en una falsa alarma. La línea de
/// confinamiento sí sale, que es la que no depende de esto.
#[tokio::test]
async fn un_volumen_que_no_contesta_no_inventa_una_alarma() {
    let (h, mut sub, _b) = Box::pin(dos_paneles_en_disco(volumen_mudo(), sin_confinar())).await;
    marca_los_ficheros(&h, &mut sub, 1).await;

    h.dispatch(tecla("F5")).await.expect("host vivo");
    let _ = siguientes_dialogos(&mut sub).await;
    asentar().await;

    let avisos = foto_hasta(&h, &mut sub, "el diálogo con su aviso", |s| {
        // El ÚLTIMO, que es el que el renderer pinta: con uno solo coinciden,
        // y así siguen coincidiendo el día que se apilen.
        match &s.dialogs.last()?.dest_check {
            norte_ui_host::dto::DestCheckView::Done { warnings } if !warnings.is_empty() => {
                Some(warnings.clone())
            }
            _ => None,
        }
    })
    .await;
    assert_eq!(
        avisos.len(),
        1,
        "solo la de confinar: del espacio no consta nada ({avisos:?})"
    );
    assert!(avisos[0].contains("symlink"), "{avisos:?}");
}

pub(super) fn volumen_de_disco(free: Option<u64>) -> Vec<norte_proto::methods::Volume> {
    vec![norte_proto::methods::Volume {
        mount: VPath::parse("file:///").expect("wire"),
        label: None,
        fs_type: "ext4".to_owned(),
        kind: norte_proto::methods::VolumeKind::Fixed,
        total_bytes: Some(1_000_000),
        free_bytes: free,
        read_only: false,
    }]
}

/// Un disco sin sitio: los ficheros del doble ocupan un byte cada uno, así
/// que cero libres es «no cabe» sin necesidad de fabricar gigas.
pub(super) fn volumen_lleno() -> Vec<norte_proto::methods::Volume> {
    volumen_de_disco(Some(0))
}

pub(super) fn volumen_de_sobra() -> Vec<norte_proto::methods::Volume> {
    volumen_de_disco(Some(1_000_000))
}

pub(super) fn volumen_mudo() -> Vec<norte_proto::methods::Volume> {
    volumen_de_disco(None)
}

pub(super) fn sin_confinar() -> norte_proto::Capabilities {
    norte_proto::Capabilities {
        flags: norte_proto::CapabilityFlags::CASE_SENSITIVE,
        max_path: None,
    }
}

pub(super) fn confinando() -> norte_proto::Capabilities {
    norte_proto::Capabilities {
        flags: norte_proto::CapabilityFlags::CASE_SENSITIVE
            | norte_proto::CapabilityFlags::CONFINED_WRITES,
        max_path: None,
    }
}

/// Dos listados sobre `file://`, que es el único esquema que cuelga de un
/// volumen de esta máquina: un `mem://` no tiene espacio libre que mirar, así
/// que estos tests no se pueden escribir sobre el árbol de siempre.
pub(super) async fn dos_paneles_en_disco(
    volumenes: Vec<norte_proto::methods::Volume>,
    caps_destino: norte_proto::Capabilities,
) -> (UiHost, norte_ui_host::UiSubscription, Arc<Falso>) {
    let mut f = Falso::default();
    f.pon(
        "file:///casa",
        vec![
            (b"docs".to_vec(), true),
            (b"uno".to_vec(), false),
            (b"dos".to_vec(), false),
        ],
    );
    f.pon("file:///casa/docs", Vec::new());
    f.volumenes = volumenes;
    f.capacidades
        .insert("file:///casa/docs".to_owned(), caps_destino);
    let backend = Arc::new(f);
    let (h, snap) = host_ortodoxo_en(Arc::clone(&backend), "file:///casa").await;
    let mut sub = h.subscribe();
    // El hueco 2 baja a `docs`, que es el destino del rol.
    let b2 = listado_de(&snap, 2);
    let docs = b2
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("el directorio está");
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host vivo");
    h.dispatch(UiAction::Activate {
        slot_id: 2,
        key: docs.key,
        generation: b2.generation,
    })
    .await
    .expect("host vivo");
    esperar_foto(&h, &mut sub, "el destino aterriza en docs", |f| {
        listado_de(f, 2).path_display.ends_with("/casa/docs")
    })
    .await;
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host vivo");
    (h, sub, backend)
}

/// Marca los FICHEROS de un hueco y deja el directorio fuera: con un
/// directorio dentro no hay total que sumar (no dice cuánto ocupa) y la
/// pregunta del espacio no llega a hacerse.
pub(super) async fn marca_los_ficheros(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    slot: u32,
) {
    let foto = foto(h, sub).await;
    let b = listado_de(&foto, slot);
    let claves: Vec<_> = b
        .rows
        .iter()
        .filter(|r| r.display_name == "uno" || r.display_name == "dos")
        .map(|r| r.key)
        .collect();
    assert!(!claves.is_empty(), "hay ficheros que marcar");
    for key in claves {
        h.dispatch(UiAction::ToggleMark {
            slot_id: slot,
            key,
            generation: b.generation,
        })
        .await
        .expect("host vivo");
    }
}

/// Como [`host_con_layout`] con `orthodox`, pero arrancando donde se diga:
/// los volúmenes solo responden por `file://`.
pub(super) async fn host_ortodoxo_en(
    backend: Arc<Falso>,
    inicio: &str,
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: VPath::parse(inicio).expect("vpath"),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("orthodox").expect("layout"),
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

/// Un hueco que espera dice A DÓNDE va.
///
/// El cuerpo sigue enseñando el listado ANTERIOR hasta que llegue el nuevo —a
/// propósito: si la conexión falla, el lector se queda donde estaba—, y sin
/// el destino esa mezcla no se puede leer. La ventana solo ponía
/// `aria-busy="true"`, sin una sola regla que lo pintara: contra un SFTP
/// lento no daba señal ninguna.
#[tokio::test]
async fn un_hueco_que_espera_dice_a_donde_va() {
    let mut f = Falso::default();
    f.arbol.clone_from(&arbol().arbol);
    // Con retraso: sin él, el listado aterriza antes de que se pueda mirar el
    // estado, y el test comprobaría el `Ready` de después.
    f.retraso_ms = 50;
    let (h, snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();
    let b = listado_de(&snap, 1);
    let docs = b
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("el directorio está");

    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs.key,
        generation: b.generation,
    })
    .await
    .expect("host vivo");

    let estado = foto_hasta(&h, &mut sub, "el hueco esperando", |s| {
        match &listado_de(s, 1).state {
            norte_ui_host::dto::SlotState::Loading { target_display, .. }
                if !target_display.is_empty() =>
            {
                Some(target_display.clone())
            }
            _ => None,
        }
    })
    .await;
    assert!(
        estado.ends_with("/casa/docs"),
        "dice a dónde va, no dónde está: {estado}"
    );
}

/// La ventana pinta en SU idioma, no en el del proceso.
///
/// `norte-ui-host` estaba limpio —sus llamadas pasan `self.lang`— y todas las
/// fugas venían de helpers COMPARTIDOS que traducían con el global. La peor
/// era la fecha: cada celda del listado salía en el idioma del proceso bajo
/// una cabecera en el del host, y no se podía esquivar con configuración
/// porque la ventana ignora `time-format`, así que la rama relativa está
/// siempre viva.
///
/// Los ajustes de la ventana salen ENTEROS en el idioma del host.
///
/// Los títulos de sección ya iban con el suyo y el nombre y la descripción de
/// cada opción con el del PROCESO, así que la pantalla salía a medias en dos
/// idiomas. Se comprueba desde fuera —lo que cruza el puente— y no llamando
/// al helper: lo que se arregló es que la ventana le pase su `lang`, y un
/// test sobre el helper seguiría verde si dejara de pasárselo.
#[tokio::test]
async fn los_ajustes_salen_enteros_en_el_idioma_del_host() {
    // El PROCESO en inglés y el host en español: lo que se escape sale en
    // inglés y se ve aquí.
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "app.settings").await;
    let ajustes = foto_hasta(&h, &mut sub, "la pantalla de ajustes", |s| {
        s.settings.clone()
    })
    .await;
    let filas: Vec<norte_ui_host::dto::SettingRowView> = ajustes
        .sections
        .iter()
        .filter_map(|s| match s {
            norte_ui_host::dto::SettingsSectionView::Settings { rows, .. } => Some(rows.clone()),
            norte_ui_host::dto::SettingsSectionView::Paths { .. } => None,
        })
        .flatten()
        .collect();
    assert!(!filas.is_empty(), "hay opciones que enseñar");

    let en_español = norte_i18n::t_in(norte_i18n::Lang::Es, "setting-ui-theme-name");
    let en_ingles = norte_i18n::t_in(norte_i18n::Lang::En, "setting-ui-theme-name");
    assert_ne!(en_español, en_ingles, "la premisa: la clave se traduce");
    let fila = filas
        .iter()
        .find(|r| r.name == en_español || r.name == en_ingles)
        .expect("la opción está en el catálogo");
    assert_eq!(
        fila.name, en_español,
        "la fila salió en el idioma del PROCESO, no en el del host"
    );
}

/// Sin papelera, el borrado lo DICE y se hace permanente.
///
/// «⚠ SIN papelera: esto no se puede deshacer» era solo del terminal. La
/// ventana compensaba con un botón destructivo, que dice que esa respuesta
/// borra — no que no haya vuelta. Son dos cosas distintas, y la segunda es la
/// que decide si alguien pulsa.
///
/// La respuesta sale de la caché de capacidades del hueco, que llega con el
/// listado: sin ella se supone que NO hay papelera, que es la dirección en la
/// que equivocarse solo cuesta un susto.
#[tokio::test]
async fn sin_papelera_el_borrado_avisa_de_que_no_hay_vuelta() {
    // El tercer caso es el que pierde datos: NO CONSTA. Las capacidades
    // llegan detrás del listado y por su cuenta, así que hay una ventana
    // entera —y toda la sesión, si la petición falla— en la que no hay
    // respuesta. Tratar eso como «no hay papelera» borra de verdad en un
    // sitio que sí la tiene.
    for (papelera, avisa) in [(Some(true), false), (Some(false), true), (None, false)] {
        let mut f = Falso::default();
        f.arbol.clone_from(&arbol().arbol);
        if let Some(hay) = papelera {
            let mut flags = norte_proto::CapabilityFlags::CASE_SENSITIVE;
            if hay {
                flags |= norte_proto::CapabilityFlags::TRASH;
            }
            f.capacidades.insert(
                "mem:///casa".to_owned(),
                norte_proto::Capabilities {
                    flags,
                    max_path: None,
                },
            );
        } else {
            // Ni siquiera contesta: el hueco se queda sin capacidades.
            f.error_de_capacidades = true;
        }
        let (h, _snap) = host_arbol(Arc::new(f)).await;
        let mut sub = h.subscribe();
        asentar().await;

        h.dispatch(tecla("F8")).await.expect("host vivo");
        let d = siguientes_dialogos(&mut sub).await;
        let borrado = d.last().expect("el diálogo de borrado");
        let norte_ui_host::dto::DestCheckView::Done { warnings } = &borrado.dest_check else {
            panic!("un borrado no espera a nadie: {:?}", borrado.dest_check)
        };
        assert_eq!(
            !warnings.is_empty(),
            avisa,
            "con papelera={papelera:?} los avisos fueron {warnings:?}"
        );
        if avisa {
            assert!(warnings[0].contains('⚠'), "{warnings:?}");
            assert_eq!(
                borrado.title_key, "modal-delete-permanent-title",
                "y el título lo dice también: sin papelera, esto es permanente"
            );
        } else {
            assert_eq!(
                borrado.title_key, "modal-delete-title",
                "con papelera —o sin saberlo— esto NO es un borrado permanente"
            );
        }
    }
}

/// El diálogo de colisión NO pierde la insignia de un nombre alterado.
///
/// Se enmascaraba sobre `display_lossy()`, que YA había metido los U+FFFD:
/// `display_name` recibía entonces UTF-8 impecable y declaraba el nombre
/// FIEL. O sea que en la única pantalla donde se aprueba SOBRESCRIBIR un
/// fichero, el nombre que difiere de los bytes se pintaba como si no
/// difiriera.
#[tokio::test]
async fn el_dialogo_de_colision_marca_el_nombre_alterado() {
    let backend = arbol();
    let (h, _snap) = Box::pin(dos_paneles_con_destino_aparte(Arc::clone(&backend))).await;
    let mut sub = h.subscribe();
    // El cursor, sobre la entrada cuyo nombre no es UTF-8.
    let foto = foto(&h, &mut sub).await;
    let b = listado_de(&foto, 1);
    let raro = b
        .rows
        .iter()
        .find(|r| r.hostile)
        .expect("el árbol trae un nombre alterado");
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key: raro.key,
        generation: b.generation,
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

    let dialogos = siguientes_dialogos(&mut sub).await;
    let colision = dialogos.last().expect("el diálogo de colisión");
    let destino = colision
        .destination
        .as_ref()
        .expect("dice sobre qué fichero pregunta");
    assert!(
        destino.hostile,
        "lo pintado no son los bytes, y esto es lo que se aprueba: {destino:?}"
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
pub(super) fn hostil(id: &str) -> Vec<u8> {
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
        initial_dir_pedido: false,
        attach: false,
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
// Un LOTE de transferencias: sus topes y su cuenta (#271).
// ---------------------------------------------------------------------------

/// Marca las N primeras filas del hueco activo, una a una.
pub(super) async fn marca_todo(
    h: &UiHost,
    sub: &mut norte_ui_host::controller::UiSubscription,
    slot: u32,
) {
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
pub(super) fn alt_a() -> UiAction {
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
