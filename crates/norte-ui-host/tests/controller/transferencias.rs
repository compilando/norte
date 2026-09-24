use super::*;

// ---------------------------------------------------------------------------
// Copy and move (task 5.1 of phase 5).
//
// The renderer NEVER names a file: it sends `pane.copy` and the host derives
// the source from the active slot's marks and the destination from the slot
// with the `Target` role. Not one path crosses from the webview.
// ---------------------------------------------------------------------------

/// The listing of ONE specific slot in a snapshot.
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
        .unwrap_or_else(|| panic!("slot {slot_id} is a listing"))
}

/// The setup of two panes with a separate destination, BOXED.
///
/// Twelve tests wait on it, and an `async fn`'s future travels whole across
/// every `await`: as the snapshot grew it went over the 16 KB clippy
/// tolerates, and all twelve call sites turned red at once. The box fixes
/// them in one go, and in the right place — the helper — instead of
/// scattering twelve `Box::pin`s across the tests that merely call it.
pub(super) fn dos_paneles_con_destino_aparte(
    backend: Arc<Falso>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = (UiHost, norte_ui_host::ViewSnapshot)>>> {
    Box::pin(dos_paneles_con_destino_aparte_inner(backend))
}

/// Two panes, with the DESTINATION already in another directory: the real
/// scenario for a copy. Returns the snapshot afterward.
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
        .expect("the directory is there");
    let (key, generation) = (docs.key, b2.generation);
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    h.dispatch(UiAction::Activate {
        slot_id: 2,
        key,
        generation,
    })
    .await
    .expect("host alive");
    // The landing snapshot: without waiting for it, F5 would see the
    // destination still in the starting directory and the test would be
    // testing something else.
    //
    // It is ASKED FOR (`esperar_foto` sends `Resync`) instead of staying and
    // listening: with a large listing, the landing travels in PATCHES and
    // the snapshot that would count it may have already gone by, so a
    // looped `siguiente_foto` would sit waiting for one that never comes
    // out again — hung, not red.
    let despues = esperar_foto(&h, &mut sub, "the destination lands in /casa/docs", |f| {
        listado_de(f, 2).path_display.ends_with("/casa/docs")
    })
    .await;
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host alive");
    // The SOURCE's cursor, on a file that is not the destination directory:
    // with the cursor on `docs`, source and destination are written the
    // same, and an assertion on the dialog's text would not tell which of
    // the two it is looking at.
    let b1 = listado_de(&despues, 1);
    let notas = b1
        .rows
        .iter()
        .find(|r| r.display_name == "notas.txt")
        .expect("the file is there");
    let (key, generation) = (notas.key, b1.generation);
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host alive");
    (h, despues)
}

/// The copy dialog says if it does NOT FIT and if the destination fails to
/// confine.
///
/// The two lines have been in the terminal since #149 and #164 and the
/// window had neither: you found out through a failed task, or you did not
/// find out. Both questions are I/O, so the dialog opens without them and
/// they get filled in when they come back — the same split the terminal
/// does in its loop.
///
/// They fail DIFFERENTLY, and it is deliberate: space swallows the failure
/// ("I don't know" is said by staying silent) and confinement does not,
/// because there silence MEANS "this destination holds onto its writes" and
/// swallowing it would be asserting it without
// TODO(translation): review — this doc comment appears interleaved with the
// next function's; the tail "... without knowing so." is the orphaned
// "/// saberlo." above `el_dialogo_de_copia_avisa_de_espacio_y_de_confinamiento`.
/// ADR 0149: the queue switch decides where what gets launched AFTERWARD
/// enters, and that reaches all the way to the backend request.
#[tokio::test]
async fn el_interruptor_de_la_cola_viaja_con_la_transferencia() {
    let (h, mut sub, backend) = Box::pin(dos_paneles_en_disco(Vec::new(), sin_confinar())).await;
    marca_los_ficheros(&h, &mut sub, 1).await;
    // `ctrl+alt+q` in orthodox: the same path as a real keystroke.
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "q".to_owned(),
        ctrl: true,
        alt: true,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host alive");
    h.dispatch(tecla("F5")).await.expect("host alive");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    asentar().await;
    assert!(
        *backend.encoladas.lock().expect("encoladas"),
        "the copy went out asking for the queue"
    );
}

/// knowing so.
#[tokio::test]
async fn el_dialogo_de_copia_avisa_de_espacio_y_de_confinamiento() {
    let (h, mut sub, _b) = Box::pin(dos_paneles_en_disco(volumen_lleno(), sin_confinar())).await;
    marca_los_ficheros(&h, &mut sub, 1).await;

    h.dispatch(tecla("F5")).await.expect("host alive");
    let _ = siguientes_dialogos(&mut sub).await;
    asentar().await;

    let avisos = foto_hasta(&h, &mut sub, "the dialog with its warnings", |s| {
        // The LAST one, which is the one the renderer paints: with only one
        // they coincide, and they keep coinciding the day they stack up.
        match &s.dialogs.last()?.dest_check {
            norte_ui_host::dto::DestCheckView::Done { warnings } if !warnings.is_empty() => {
                Some(warnings.clone())
            }
            _ => None,
        }
    })
    .await;
    assert_eq!(avisos.len(), 2, "both lines: {avisos:?}");
    assert!(
        avisos[0].contains("libres"),
        "the space one carries both numbers: {avisos:?}"
    );
    assert!(
        avisos[1].contains("symlink"),
        "the confinement one says what it protects: {avisos:?}"
    );
}

/// And a destination that DOES fit and DOES confine says nothing.
///
/// The half of the contract that gets forgotten: a line on every copy is
/// noise, and noise teaches people to skip the line exactly the day it says
/// something.
#[tokio::test]
async fn un_destino_que_cabe_y_confina_no_dice_nada() {
    let (h, mut sub, backend) =
        Box::pin(dos_paneles_en_disco(volumen_de_sobra(), confinando())).await;
    marca_los_ficheros(&h, &mut sub, 1).await;

    h.dispatch(tecla("F5")).await.expect("host alive");
    let dialogos = siguientes_dialogos(&mut sub).await;
    assert_eq!(dialogos.len(), 1, "the dialog opens just the same");
    asentar().await;

    let d = foto_hasta(&h, &mut sub, "the destination already checked", |s| {
        let d = s.dialogs.last()?;
        matches!(d.dest_check, norte_ui_host::dto::DestCheckView::Done { .. }).then(|| d.clone())
    })
    .await;
    assert_eq!(
        d.dest_check,
        norte_ui_host::dto::DestCheckView::Done {
            warnings: Vec::new()
        },
        "nothing to warn about, so nothing to say"
    );
    // And it WAS ASKED. Without this the test stays green if someone deletes
    // the probe: staying quiet for having nothing to say and staying quiet
    // for never having checked look the same, which is exactly what this
    // field tells apart.
    assert!(
        backend
            .volumenes_pedidos
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0,
        "silence is a RESPONSE, not an omission"
    );
}

/// A volume that does not answer is not a full volume.
///
/// `free_bytes: None` means "did not answer in time", NEVER zero: confusing
/// them turns every slow mount into a false alarm. The confinement line does
/// come out, since it does not depend on this.
#[tokio::test]
async fn un_volumen_que_no_contesta_no_inventa_una_alarma() {
    let (h, mut sub, _b) = Box::pin(dos_paneles_en_disco(volumen_mudo(), sin_confinar())).await;
    marca_los_ficheros(&h, &mut sub, 1).await;

    h.dispatch(tecla("F5")).await.expect("host alive");
    let _ = siguientes_dialogos(&mut sub).await;
    asentar().await;

    let avisos = foto_hasta(&h, &mut sub, "the dialog with its warning", |s| {
        // The LAST one, which is the one the renderer paints: with only one
        // they coincide, and they keep coinciding the day they stack up.
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
        "only the confinement one: nothing is known about space ({avisos:?})"
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

/// A disk with no room: the double's files each take up one byte, so zero
/// free is "does not fit" with no need to fabricate gigabytes.
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

/// Two listings over `file://`, the only scheme that hangs off a volume on
/// this machine: a `mem://` has no free space to look at, so these tests
/// cannot be written over the usual tree.
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
        .expect("the directory is there");
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    h.dispatch(UiAction::Activate {
        slot_id: 2,
        key: docs.key,
        generation: b2.generation,
    })
    .await
    .expect("host alive");
    esperar_foto(&h, &mut sub, "the destination lands in docs", |f| {
        listado_de(f, 2).path_display.ends_with("/casa/docs")
    })
    .await;
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host alive");
    (h, sub, backend)
}

/// Marks a slot's FILES and leaves the directory out: with a directory
/// inside there is no total to add up (it does not say how much it takes
/// up) and the space question never gets asked.
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
    assert!(!claves.is_empty(), "there are files to mark");
    for key in claves {
        h.dispatch(UiAction::ToggleMark {
            slot_id: slot,
            key,
            generation: b.generation,
        })
        .await
        .expect("host alive");
    }
}

/// Like [`host_con_layout`] with `orthodox`, but starting wherever is said:
/// volumes only answer over `file://`.
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
    .expect("starts")
}

/// F5 does not copy: it opens the confirmation, and that confirmation SAYS
/// where it is going.
///
/// In a window with two listings the destination is not obvious — there is
/// no "the other pane" when there are three — so the dialog is the only
/// place it can be read before accepting.
#[tokio::test]
async fn copiar_pide_confirmacion_y_dice_a_donde() {
    let backend = arbol();
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F5")).await.expect("host alive");
    let dialogos = siguientes_dialogos(&mut sub).await;
    assert_eq!(dialogos.len(), 1, "ONE dialog opens");
    assert_eq!(dialogos[0].title_key, "modal-copy-title");
    let cuerpo = &dialogos[0].body;
    assert!(
        cuerpo.iter().any(|l| l.text.ends_with("/casa/notas.txt")),
        "the body is what gets transferred: {cuerpo:?}"
    );
    let destino = dialogos[0]
        .destination
        .as_ref()
        .expect("a transfer says where it is going");
    assert!(
        destino.text.ends_with("/casa/docs"),
        "and the destination goes in ITS OWN field: {destino:?}"
    );
    assert!(
        cuerpo.iter().all(|l| !l.text.contains("/casa/docs")),
        "not repeated among the body's lines: {cuerpo:?}"
    );
    assert!(
        backend
            .transferencias
            .lock()
            .expect("transferencias")
            .is_empty(),
        "opening the dialog copies nothing"
    );
}

/// **Splitting reads the size in BINARY** (#132, #290): `10M` is 10 MiB,
/// which is what it means in a file manager, not ten million.
#[tokio::test]
async fn partir_lee_el_tamano_en_binario() {
    let backend = Arc::new(arbol_como_falso());
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separar_los_paneles(&h, &mut sub).await;
    // The cursor, on a FILE: it starts in `docs/`, which is also the
    // destination directory, and there the check below would tell nothing
    // apart.
    h.dispatch(tecla("Down")).await.expect("host alive");

    ejecutar_por_paleta(&h, &mut sub, "pane.split-file").await;
    let id = siguientes_dialogos(&mut sub)
        .await
        .last()
        .expect("there is one")
        .id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "10M".to_owned(),
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let ps = anotados(&backend, "the split queued", 1, |f| {
        f.partidos.lock().expect("partidos").clone()
    })
    .await;
    assert_eq!(ps.len(), 1);
    assert_eq!(ps[0].part_bytes, 10 * 1024 * 1024, "MiB, not millions");
    // Whatever entry is under the cursor does not matter — the listing
    // sorts and the corpus throws in a hostile name — what this test pins
    // down is WHICH pane each thing comes out of.
    assert_eq!(
        ps[0].path.parent().map(|p| p.to_wire()).as_deref(),
        Some("mem:///casa"),
        "the file comes out of the ACTIVE pane"
    );
    assert_eq!(
        ps[0].dest_dir.to_wire(),
        "mem:///casa/docs",
        "and the parts go to the DESTINATION pane: splitting a huge one where \
         it already is usually does not fit"
    );
}

/// A size that makes no sense is refused and splits nothing. Zero belongs
/// here: zero-byte parts never finish.
#[tokio::test]
async fn partir_rehusa_un_tamano_que_no_vale() {
    let backend = Arc::new(arbol_como_falso());
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separar_los_paneles(&h, &mut sub).await;

    ejecutar_por_paleta(&h, &mut sub, "pane.split-file").await;
    let id = siguientes_dialogos(&mut sub)
        .await
        .last()
        .expect("there is one")
        .id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "0".to_owned(),
    })
    .await
    .expect("host alive");
    let ack = h
        .dispatch(UiAction::Dialog {
            id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, ActionAck::Unavailable { ref reason_key } if reason_key == "msg-split-bad-size"),
        "{ack:?}"
    );
    asentar().await;
    assert!(backend.partidos.lock().expect("partidos").is_empty());
}

/// **Joining only from the FIRST part** (#132, #290): starting from `.007`
/// would join half a thing, and the core only looks forward.
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

    // The cursor starts on the first row: the `.001`.
    ejecutar_por_paleta(&h, &mut sub, "pane.combine-files").await;
    {
        let js = anotados(&backend, "the join queued", 1, |f| {
            f.juntados.lock().expect("juntados").clone()
        })
        .await;
        assert_eq!(js.len(), 1, "from the .001 it does");
        assert_eq!(
            js[0].dest.to_wire(),
            "mem:///casa/pelicula.mkv",
            "the destination is the name WITHOUT the part suffix"
        );
    }

    // Move down to `.007` and ask for it again: not from there.
    h.dispatch(tecla("Down")).await.expect("host alive");
    let ack = ejecutar_por_paleta_ack(&h, &mut sub, "pane.combine-files").await;
    assert!(
        matches!(ack, ActionAck::Unavailable { ref reason_key } if reason_key == "msg-combine-needs-first"),
        "not from another part: {ack:?}"
    );
    assert_eq!(
        backend.juntados.lock().expect("juntados").len(),
        1,
        "and nothing new is requested"
    );
}

/// **Packing takes the FORMAT from the typed name** (#132, #290), and the
/// base is the pane's directory: whoever unpacks expects to see what was on
/// screen, not absolute paths.
#[tokio::test]
async fn empaquetar_saca_el_formato_del_nombre() {
    let backend = Arc::new(arbol_como_falso());
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.pack").await;
    let id = siguientes_dialogos(&mut sub)
        .await
        .last()
        .expect("there is one")
        .id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "cosas.tar.gz".to_owned(),
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let ps = anotados(&backend, "the archiving queued", 1, |f| {
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

/// A name whose format is NOT known how to write is refused, instead of
/// archiving into something else. `.rar` is the real case: it is read by
/// delegation and not written.
#[tokio::test]
async fn empaquetar_rehusa_un_formato_que_no_se_escribe() {
    let backend = Arc::new(arbol_como_falso());
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.pack").await;
    let id = siguientes_dialogos(&mut sub)
        .await
        .last()
        .expect("there is one")
        .id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "cosas.rar".to_owned(),
    })
    .await
    .expect("host alive");
    let ack = h
        .dispatch(UiAction::Dialog {
            id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
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
        "and nothing gets archived"
    );
}

/// Testing only applies to a CONTAINER, and it is decided by the same
/// function `Enter` uses to enter one: two extension tables would be two
/// places where one gets forgotten.
#[tokio::test]
async fn comprobar_un_archivo_exige_que_lo_sea() {
    let backend = Arc::new(arbol_como_falso());
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // The cursor starts on `docs/`, which is a directory.
    let ack = ejecutar_por_paleta_ack(&h, &mut sub, "pane.test-archive").await;
    assert!(
        matches!(ack, ActionAck::Unavailable { ref reason_key } if reason_key == "msg-unpack-not-archive"),
        "a directory is not a container: {ack:?}"
    );
    assert!(
        backend.comprobados.lock().expect("comprobados").is_empty(),
        "and nothing gets asked to be tested"
    );
}

/// A slot that is waiting says WHERE it is going.
///
/// The body keeps showing the PREVIOUS listing until the new one arrives —on
/// purpose: if the connection fails, the reader stays where they were — and
/// without the destination that mix cannot be read. The window was only
/// setting `aria-busy="true"`, with not one rule painting it: against a slow
/// SFTP it gave no signal at all.
#[tokio::test]
async fn un_hueco_que_espera_dice_a_donde_va() {
    let mut f = Falso::default();
    f.arbol.clone_from(&arbol().arbol);
    // With a delay: without it, the listing lands before the state can be
    // looked at, and the test would be checking the later `Ready`.
    f.retraso_ms = 50;
    let (h, snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();
    let b = listado_de(&snap, 1);
    let docs = b
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("the directory is there");

    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs.key,
        generation: b.generation,
    })
    .await
    .expect("host alive");

    let estado = foto_hasta(&h, &mut sub, "the waiting slot", |s| {
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
        "it says where it is going, not where it is: {estado}"
    );
}

/// The window paints in ITS OWN language, not the process's.
///
/// `norte-ui-host` was clean — its calls pass `self.lang` — and all the
/// leaks came from SHARED helpers that translated using the global. The
/// worst one was the date: every listing cell came out in the process's
/// language under a header in the host's, and it could not be dodged with
/// configuration because the window ignores `time-format`, so the relative
/// branch is always alive.
///
/// The window's settings come out WHOLE in the host's language.
///
/// Section titles already carried their own, and each option's name and
/// description carried the PROCESS's, so the screen came out split between
/// two languages. Checked from the outside — what crosses the bridge — and
/// not by calling the helper: what got fixed is that the window passes it
/// its `lang`, and a test on the helper would still stay green if it
/// stopped passing it.
#[tokio::test]
async fn los_ajustes_salen_enteros_en_el_idioma_del_host() {
    // The PROCESS in English and the host in Spanish: whatever leaks comes
    // out in English and shows up here.
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "app.settings").await;
    let ajustes = foto_hasta(&h, &mut sub, "the settings screen", |s| s.settings.clone()).await;
    let filas: Vec<norte_ui_host::dto::SettingRowView> = ajustes
        .sections
        .iter()
        .filter_map(|s| match s {
            norte_ui_host::dto::SettingsSectionView::Settings { rows, .. } => Some(rows.clone()),
            norte_ui_host::dto::SettingsSectionView::Paths { .. } => None,
        })
        .flatten()
        .collect();
    assert!(!filas.is_empty(), "there are options to show");

    let in_spanish = norte_i18n::t_in(norte_i18n::Lang::Es, "setting-ui-theme-name");
    let in_english = norte_i18n::t_in(norte_i18n::Lang::En, "setting-ui-theme-name");
    assert_ne!(
        in_spanish, in_english,
        "the premise: the key gets translated"
    );
    let fila = filas
        .iter()
        .find(|r| r.name == in_spanish || r.name == in_english)
        .expect("the option is in the catalogue");
    assert_eq!(
        fila.name, in_spanish,
        "the row came out in the PROCESS's language, not the host's"
    );
}

/// With no trash, the delete SAYS so and becomes permanent.
///
/// "⚠ NO trash: this cannot be undone" was terminal-only. The window
/// compensated with a destructive button, which says that response deletes —
/// not that there is no going back. They are two different things, and the
/// second one is what decides whether anyone presses it.
///
/// The answer comes from the slot's capabilities cache, which arrives with
/// the listing: without it, it is assumed there is NO trash, which is the
/// direction where being wrong only costs a scare.
#[tokio::test]
async fn sin_papelera_el_borrado_avisa_de_que_no_hay_vuelta() {
    // The third case is the one that loses data: NOT KNOWN. Capabilities
    // arrive behind the listing and on their own schedule, so there is a
    // whole window — and the whole session, if the request fails — with no
    // answer. Treating that as "there is no trash" really deletes in a
    // place that does have one.
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
            // It does not even answer: the slot is left with no
            // capabilities.
            f.error_de_capacidades = true;
        }
        let (h, _snap) = host_arbol(Arc::new(f)).await;
        let mut sub = h.subscribe();
        asentar().await;

        h.dispatch(tecla("F8")).await.expect("host alive");
        let d = siguientes_dialogos(&mut sub).await;
        let borrado = d.last().expect("the delete dialog");
        let norte_ui_host::dto::DestCheckView::Done { warnings } = &borrado.dest_check else {
            panic!("a delete waits on nobody: {:?}", borrado.dest_check)
        };
        assert_eq!(
            !warnings.is_empty(),
            avisa,
            "with papelera={papelera:?} the warnings were {warnings:?}"
        );
        if avisa {
            assert!(warnings[0].contains('⚠'), "{warnings:?}");
            assert_eq!(
                borrado.title_key, "modal-delete-permanent-title",
                "and the title says so too: with no trash, this is permanent"
            );
        } else {
            assert_eq!(
                borrado.title_key, "modal-delete-title",
                "with trash — or not knowing — this is NOT a permanent delete"
            );
        }
    }
}

/// The collision dialog does NOT lose the badge from an altered name.
///
/// It was masking over `display_lossy()`, which had ALREADY put in the
/// U+FFFD: `display_name` then received flawless UTF-8 and declared the name
/// FAITHFUL. Meaning that on the only screen where OVERWRITING a file gets
/// approved, the name that differs from the bytes was painted as if it did
/// not differ.
#[tokio::test]
async fn el_dialogo_de_colision_marca_el_nombre_alterado() {
    let backend = arbol();
    let (h, _snap) = Box::pin(dos_paneles_con_destino_aparte(Arc::clone(&backend))).await;
    let mut sub = h.subscribe();
    // The cursor, on the entry whose name is not UTF-8.
    let foto = foto(&h, &mut sub).await;
    let b = listado_de(&foto, 1);
    let raro = b
        .rows
        .iter()
        .find(|r| r.hostile)
        .expect("the tree carries an altered name");
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key: raro.key,
        generation: b.generation,
    })
    .await
    .expect("host alive");

    h.dispatch(tecla("F5")).await.expect("host alive");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let _ = siguientes_tasks(&mut sub).await;
    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| {
        p.state = norte_proto::TaskState::Failed {
            error: norte_proto::Error::Conflict {
                conflict: norte_proto::ConflictKind::Exists,
            },
        };
    });

    let dialogos = siguientes_dialogos(&mut sub).await;
    let colision = dialogos.last().expect("the collision dialog");
    let destino = colision
        .destination
        .as_ref()
        .expect("says which file it is asking about");
    assert!(
        destino.hostile,
        "what is painted is not the bytes, and this is what gets approved: {destino:?}"
    );
}

/// **A transfer that COLLIDES has a way out** (#274).
///
/// The window always sends `CollisionPolicy::Fail`, the safe default, but
/// had nowhere to make the decision: a failed task was left on the board
/// with no way forward, while the TUI does offer the four. What is checked
/// is what really matters: that the second transfer GOES OUT, with the
/// chosen policy and the same verb.
#[tokio::test]
async fn una_copia_que_choca_se_puede_reintentar_con_otra_politica() {
    let backend = arbol();
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F5")).await.expect("host alive");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let _ = siguientes_tasks(&mut sub).await;

    // The daemon says the destination already exists.
    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| {
        p.state = norte_proto::TaskState::Failed {
            error: norte_proto::Error::Conflict {
                conflict: norte_proto::ConflictKind::Exists,
            },
        };
    });

    // And now there IS a question to answer.
    let dialogos = siguientes_dialogos(&mut sub).await;
    let colision = dialogos.last().expect("the collision dialog");
    let opciones: Vec<&str> = colision.choices.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        opciones,
        vec!["overwrite", "newer", "rename", "skip", "cancel"],
        "the TUI's four ways out, plus cancel"
    );
    assert!(
        colision
            .choices
            .iter()
            .any(|c| c.id == "overwrite" && c.destructive),
        "overwrite is marked destructive: it destroys what is there"
    );

    // It opened on its own, so the first response only acknowledges it.
    let cid = colision.id;
    h.dispatch(UiAction::Dialog {
        id: cid,
        choice: "overwrite".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id: cid,
        choice: "overwrite".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let ts = anotados(&backend, "the original and the retry", 2, |f| {
        f.transferencias.lock().expect("transferencias").clone()
    })
    .await;
    assert_eq!(ts.len(), 2, "the original and the retry: {ts:?}");
    let (from, to, mover, colision) = &ts[1];
    assert_eq!(
        *colision,
        norte_proto::CollisionPolicy::Overwrite,
        "the retry goes with the chosen policy"
    );
    assert!(!mover, "and with the SAME verb: a copy retry does not move");
    assert_eq!(from.to_wire(), ts[0].0.to_wire(), "same source");
    assert_eq!(to.to_wire(), ts[0].1.to_wire(), "and same destination");
}

/// Cancelling the collision relaunches nothing: not choosing is a response,
/// and the failed task stays as it was.
#[tokio::test]
async fn cancelar_una_colision_no_reintenta() {
    let backend = arbol();
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F5")).await.expect("host alive");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let _ = siguientes_tasks(&mut sub).await;
    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| {
        p.state = norte_proto::TaskState::Failed {
            error: norte_proto::Error::Conflict {
                conflict: norte_proto::ConflictKind::Exists,
            },
        };
    });
    let cid = siguientes_dialogos(&mut sub)
        .await
        .last()
        .expect("there is one")
        .id;

    // Cancel is EXEMPT from acknowledgment: shrugging off something one
    // never asked for goes through on the first try.
    h.dispatch(UiAction::Dialog {
        id: cid,
        choice: "cancel".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    anotados(&backend, "the original transfer", 1, |f| {
        f.transferencias.lock().expect("transferencias").clone()
    })
    .await;
    asentar().await;
    assert_eq!(
        backend.transferencias.lock().expect("transferencias").len(),
        1,
        "cancel does not relaunch"
    );
}

/// Confirmed, the copy goes out with the destination COMPOSED in Rust: the
/// destination slot's directory plus the entry's name, byte for byte.
#[tokio::test]
async fn copiar_compone_el_destino_en_rust() {
    let backend = arbol();
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F5")).await.expect("host alive");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let ts = anotados(&backend, "the transfer queued", 1, |f| {
        f.transferencias.lock().expect("transferencias").clone()
    })
    .await;
    assert_eq!(ts.len(), 1, "one entry under the cursor, one task");
    let (from, to, mover, colision) = &ts[0];
    assert_eq!(
        *colision,
        norte_proto::CollisionPolicy::Fail,
        "the wire's SAFE default: a busy destination fails, it is not overwritten"
    );
    assert!(!mover, "F5 copies");
    assert_eq!(from.to_wire(), "mem:///casa/notas.txt");
    assert_eq!(
        to.to_wire(),
        "mem:///casa/docs/notas.txt",
        "the destination is the other slot's DIRECTORY plus the source's name"
    );
}

/// F6 uses the same path, but it is a different verb: on the wire it is two
/// methods, on the board two task classes, and in the journal two entries.
#[tokio::test]
async fn mover_es_otro_verbo_y_lo_dice() {
    let backend = arbol();
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F6")).await.expect("host alive");
    let dialogos = siguientes_dialogos(&mut sub).await;
    assert_eq!(dialogos[0].title_key, "modal-move-title");
    let id = dialogos[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let ts = anotados(&backend, "the move queued", 1, |f| {
        f.transferencias.lock().expect("transferencias").clone()
    })
    .await;
    assert!(ts[0].2, "F6 moves");
}

/// With a single listing there is nowhere to copy to **inside the window**,
/// so it is asked outside (#284) — and until the answer arrives nothing
/// gets transferred. What this test asserts is that a destination is not
/// MADE UP: not the directory itself, not the last one used.
///
/// Before #284 this was refused with `host-no-other-slot`. The full chain —
/// effect, response and confirmation — is covered by
/// `con_un_panel_el_destino_lo_elige_el_escritorio`.
#[tokio::test]
async fn sin_otro_hueco_el_destino_se_pregunta_fuera() {
    let backend = arbol();
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "simple", (120, 40)).await;
    let mut nativos = h.native_effects();
    let ack = h.dispatch(tecla("F5")).await.expect("host alive");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "the gesture is accepted and it asks: {ack:?}"
    );
    let efecto = tokio::time::timeout(std::time::Duration::from_secs(2), nativos.recv())
        .await
        .expect("the picker comes out")
        .expect("channel alive");
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
        "and nothing moves until there is a destination"
    );
}

/// Both listings in the SAME directory: copying there is copying onto
/// itself, and no dialog opens suggesting it.
#[tokio::test]
async fn copiar_sobre_el_propio_directorio_se_rechaza() {
    let backend = arbol();
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (120, 40)).await;
    let ack = h.dispatch(tecla("F5")).await.expect("host alive");
    assert!(
        matches!(ack, ActionAck::Unavailable { .. }),
        "the destination is the source directory: {ack:?}"
    );
    assert!(
        backend
            .transferencias
            .lock()
            .expect("transferencias")
            .is_empty()
    );
}

/// In read-only, F5 opens nothing: the key existing in the preset is not
/// permission.
#[tokio::test]
async fn en_solo_lectura_copiar_no_abre_nada() {
    let backend = arbol();
    let (h, _snap) = host_solo_lectura(Arc::clone(&backend)).await;
    let ack = h.dispatch(tecla("F5")).await.expect("host alive");
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

/// The marks are CONSUMED by sending, like in the TUI: a half-consumed
/// selection would mean different things depending on which task finished.
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
        .expect("host alive");
    }
    h.dispatch(tecla("F5")).await.expect("host alive");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    anotados(&backend, "the two tasks from the two marks", 2, |f| {
        f.transferencias.lock().expect("transferencias").clone()
    })
    .await;
    assert_eq!(
        backend.transferencias.lock().expect("transferencias").len(),
        2,
        "two marks, two tasks"
    );
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(listado_de(&foto, 1).marks, 0, "sending consumed the marks");
}

/// When the copy finishes, the DESTINATION slot is re-listed: the new entry
/// is there and a screen that does not show it lies.
#[tokio::test]
async fn al_terminar_una_copia_se_relista_el_destino() {
    let backend = arbol();
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F5")).await.expect("host alive");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
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

/// A collision is not an exception from the host: it is the task's TYPED
/// outcome, and it reaches the board as such.
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
    h.dispatch(tecla("F5")).await.expect("host alive");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let tasks = siguientes_tasks(&mut sub).await;
    assert_eq!(
        tasks[0].state,
        norte_ui_host::dto::TaskStateView::Failed,
        "the destination already existed, and the board says so"
    );
}

/// A task that IS BORN terminal — the daemon completed it before the call
/// returned — also re-lists the destination.
///
/// It is the real race: the progress channel never changes, so nobody ever
/// looks at it, and without checking the state ON REGISTERING the copy was
/// left done on disk and absent on screen forever.
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
    h.dispatch(tecla("F5")).await.expect("host alive");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    let antes = backend.listados();
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");

    hasta(&backend, "the refresh from an already-finished copy", |f| {
        (f.listados() > antes).then_some(())
    })
    .await;
}

/// A destination that does not accept writes refuses ON QUEUEING, before
/// there is a task: there is no row on the board to look at, so the bar
/// says it — with the error's typed phrase, not with a "something failed".
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
    h.dispatch(tecla("F5")).await.expect("host alive");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");

    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let foto = siguiente_foto(&mut sub).await;
        if let Some(m) = &foto.status.message {
            assert!(
                !m.starts_with("err-"),
                "the bar says the TRANSLATED error, not its key: {m}"
            );
            assert!(foto.tasks.is_empty(), "no task ever came to be");
            return;
        }
    }
    panic!("a queueing rejection got lost in silence");
}

/// A transfer in progress is cancelled through the same path as any other
/// task: there is only one board.
#[tokio::test]
async fn una_copia_en_marcha_se_cancela() {
    let backend = arbol();
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F5")).await.expect("host alive");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let tasks = siguientes_tasks(&mut sub).await;
    assert_eq!(tasks.len(), 1);
    let ack = h
        .dispatch(UiAction::CancelTask {
            task_id: tasks[0].task_id,
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    assert_eq!(backend.cancelaciones.load(Ordering::SeqCst), 1);
}

/// A refresh NEVER steps on a navigation in flight.
///
/// The refresh reserves a new token, so the navigation's response would
/// arrive with an old one and be discarded: the pane would be left in the
/// directory the reader had just left, without saying anything. A slightly
/// stale screen is acceptable; the application moving on its own is not.
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
        .expect("is there")
        .push((b"hondo".to_vec(), true));
    // The listing's response TAKES A WHILE: that is what opens the window
    // where the refresh could sneak in.
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
        .expect("the subdirectory is there");
    let (key, generation) = (hondo.key, b2.generation);

    // A copy toward `casa/docs`, which finishes as soon as it is queued.
    h.dispatch(tecla("F5")).await.expect("host alive");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    // And, BEFORE confirming, the destination navigates elsewhere: the
    // navigation is left in flight during the fake's 120 ms.
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    let antes = backend.listados();
    h.dispatch(UiAction::Activate {
        slot_id: 2,
        key,
        generation,
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");

    // The navigation reaches its destination and NOBODY sends it back to
    // `casa/docs`. It waits for `hondo`'s listing to have been REQUESTED and
    // for no response to be left in flight — including the refresh the
    // finished copy triggers, which is the one that could step on it —
    // only then is the screen looked at, ONCE.
    hasta(&backend, "hondo's listing, already served", |f| {
        (f.listados() > antes && f.en_calma()).then_some(())
    })
    .await;
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let foto = siguiente_foto(&mut sub).await;
    assert!(
        listado_de(&foto, 2).path_display.ends_with("/docs/hondo"),
        "the navigation survived the refresh: {}",
        listado_de(&foto, 2).path_display
    );
}

// ---------------------------------------------------------------------------
// What the three 5.1 reviews found.
// ---------------------------------------------------------------------------

/// A corpus name, by its id.
pub(super) fn hostil(id: &str) -> Vec<u8> {
    norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == id)
        .unwrap_or_else(|| panic!("the corpus has {id}"))
        .bytes
}

/// A destination directory named `a → mem_b.txt` CANNOT simulate two paths
/// in the confirmation.
///
/// The arrow is legitimate (U+2192), it is not a terminal hazard and is
/// therefore neither masked nor marked. With the destination as the body's
/// first line and an arrow as its label, whoever reads `→ …/a → mem_b.txt`
/// could understand their files are going to `mem_b.txt`. It is labeled OUT
/// OF BAND: the destination has its own field. Fixture `arrow_join_spoof`
/// from the canonical corpus.
#[tokio::test]
async fn un_destino_con_una_flecha_no_simula_dos_rutas() {
    let trampa = hostil("arrow_join_spoof");
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(trampa.clone(), true), (b"notas.txt".to_vec(), false)],
    );
    let vp = norte_proto::VPath::parse("mem:///casa")
        .expect("root")
        .join(norte_proto::Segment::new(trampa.clone()).expect("segment"));
    f.pon(vp.to_wire().as_str(), vec![(b"a.md".to_vec(), false)]);
    let backend = Arc::new(f);
    let (h, snap) = host_con_layout(Arc::clone(&backend), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();

    // The destination slot enters the trap directory.
    let b2 = listado_de(&snap, 2);
    let fila = b2
        .rows
        .iter()
        .find(|r| r.display_name.contains('→'))
        .expect("the trap is painted");
    let (key, generation) = (fila.key, b2.generation);
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    h.dispatch(UiAction::Activate {
        slot_id: 2,
        key,
        generation,
    })
    .await
    .expect("host alive");
    let mut despues = siguiente_foto(&mut sub).await;
    while listado_de(&despues, 2).path_display == listado_de(&snap, 2).path_display {
        despues = siguiente_foto(&mut sub).await;
    }
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host alive");
    // The SOURCE's cursor, on the file: the trap directory is also listed
    // here, and what is checked is the destination.
    let b1 = listado_de(&despues, 1);
    let notas = b1
        .rows
        .iter()
        .find(|r| r.display_name == "notas.txt")
        .expect("the file is there");
    let (key, generation) = (notas.key, b1.generation);
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host alive");

    h.dispatch(tecla("F5")).await.expect("host alive");
    let d = siguientes_dialogos(&mut sub).await[0].clone();
    let destino = d.destination.expect("says where it is going");
    assert!(
        destino.text.contains('\u{2192}'),
        "the real name carries the arrow: {destino:?}"
    );
    assert_eq!(d.body.len(), 1, "one entry, one line: {:?}", d.body);
    assert!(
        d.body[0].text.ends_with("/casa/notas.txt") && !d.body[0].text.contains('\u{2192}'),
        "the body is ONLY the source; the destination does not appear there: {:?}",
        d.body
    );
}

/// A batch bigger than what fits in the dialog SAYS so.
///
/// Marking forty, seeing sixteen and confirming is approving something
/// else: this is the last screen where it can still be said no.
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
        "there are more than what fits: {}",
        claves.len()
    );
    for key in &claves {
        h.dispatch(UiAction::ToggleMark {
            slot_id: 1,
            key: *key,
            generation,
        })
        .await
        .expect("host alive");
    }
    // Listening starts AFTER marking: forty marks are forty patches, and the
    // helper waiting for a dialog only looks at the first updates that
    // reach it.
    let mut sub = h.subscribe();
    h.dispatch(tecla("F5")).await.expect("host alive");
    let d = siguientes_dialogos(&mut sub).await[0].clone();
    assert!(
        d.body.len() < claves.len(),
        "the body is capped: {}",
        d.body.len()
    );
    assert!(
        !d.overflow_note.is_empty(),
        "and it SAYS so, in its own field: {d:?}"
    );
    assert!(
        !d.overflow_note.starts_with("dialog-"),
        "translated, not the Fluent key: {}",
        d.overflow_note
    );
}

/// A confirmation's body SAYS which line is painted differently from what
/// it is. It is the only surface where a foreign name gets approved.
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
            .expect("is there");
        let (key, generation) = (fila.key, b1.generation);
        h.dispatch(UiAction::SelectRow {
            slot_id: 1,
            key,
            generation,
        })
        .await
        .expect("host alive");
        // F8 is enough: the delete's body and the transfer's are built with
        // the SAME function.
        h.dispatch(tecla("F8")).await.expect("host alive");
        let d = siguientes_dialogos(&mut sub).await[0].clone();
        for l in &d.body {
            sin_peligro(&l.text, id, "a line from a dialog's body");
        }
        assert_eq!(
            d.body.iter().any(|l| l.hostile),
            altera,
            "[{id}] the body's mark says exactly what `display_name` says: {:?}",
            d.body
        );
    }
}

/// The destination is composed from the source's BYTES, even when they are
/// not UTF-8. The path that crosses the wire never went through the screen.
#[tokio::test]
async fn el_destino_se_compone_byte_a_byte() {
    let backend = arbol();
    let (h, snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // `caf\xC3(`: not UTF-8, and on screen it carries a U+FFFD.
    let b1 = listado_de(&snap, 1);
    let fila = b1
        .rows
        .iter()
        .find(|r| r.hostile)
        .expect("the tree carries a name that is not UTF-8");
    let (key, generation) = (fila.key, b1.generation);
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host alive");
    h.dispatch(tecla("F5")).await.expect("host alive");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let ts = anotados(&backend, "the transfer queued", 1, |f| {
        f.transferencias.lock().expect("transferencias").clone()
    })
    .await;
    let (from, to, _, _) = &ts[0];
    assert!(
        from.to_wire().starts_with("mem:///casa/caf"),
        "the source is the entry that is not UTF-8: {}",
        from.to_wire()
    );
    let nombre = from
        .to_wire()
        .strip_prefix("mem:///casa/")
        .expect("hangs off casa")
        .to_owned();
    assert_eq!(
        to.to_wire(),
        format!("mem:///casa/docs/{nombre}"),
        "the name's bytes arrive intact"
    );
    assert!(
        !to.to_wire().contains("%EF%BF%BD"),
        "and without the U+FFFD the screen paints: {}",
        to.to_wire()
    );
}

/// Moving ALSO re-lists the source pane: entries disappear from it.
#[tokio::test]
async fn mover_relista_tambien_el_origen() {
    let backend = arbol();
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F6")).await.expect("host alive");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    siguientes_tasks(&mut sub).await;
    let antes = backend.listados();
    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    // BOTH: the source (`casa`, where it leaves from) and the destination
    // (`casa/docs`, where it arrives).
    hasta(&backend, "both panes re-listed", |f| {
        (f.listados() >= antes + 2).then_some(())
    })
    .await;
}

/// A refresh keeps the cursor BY PATH, not by index.
///
/// Per-directory memory stores an index, and an index does not survive the
/// operation removing an entry: whoever was looking at one file would find
/// the cursor on another without having touched a key, and the next key
/// could be F8.
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
    // Delete REMOVES the entry: without that the listing that arrives is
    // identical and the cursor's index keeps naming the same file by
    // accident — a green test proving nothing.
    f.borrar_de_verdad = true;
    let backend = Arc::new(f);
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let b = listado(&snap);
    // Neither the first nor the LAST one: on the last, deleting an entry
    // ahead of it leaves the old, trimmed index landing right on the same
    // file, and the test would pass with no anchor by pure coincidence.
    let medio = &b.rows[2];
    let (key, generation, nombre) = (medio.key, b.generation, medio.display_name.clone());
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host alive");

    // The FIRST one is marked and deleted, not the cursor's: the listing
    // arrives with one entry fewer AHEAD of it, so the old index points at a
    // different file while the path stays the same.
    let primera = b.rows.first().expect("there are rows").key;
    h.dispatch(UiAction::ToggleMark {
        slot_id: 1,
        key: primera,
        generation,
    })
    .await
    .expect("host alive");
    h.dispatch(tecla("F8")).await.expect("host alive");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    siguientes_tasks(&mut sub).await;
    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
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
            "the cursor is still on the SAME file after the refresh"
        );
        return;
    }
    panic!("the refresh never arrived");
}

/// With THREE listings, a hand-designated destination SURVIVES a focus
/// change, and with none designated, none is guessed.
///
/// The host was reassigning the role on every `FocusSlot` with its own rule
/// — "the first one that is not active" — overriding what a person had
/// chosen and only tie-breaking when there were several candidates. While
/// the destination was decoration that looked odd; since copy and move read
/// it, it means sending files to a place nobody chose. The rule is the
/// shared one (ADR 0058 D7), and with several candidates and none chosen
/// the role is left UNSET.
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
        toml::from_str(TRES).expect("the layout parses");
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
    .expect("starts");
    let rol = |l: &norte_ui_host::dto::LayoutView, id: u32| {
        l.placements
            .iter()
            .find(|p| p.slot_id == id)
            .and_then(|p| p.role)
    };
    let _ = &snap;
    let mut sub = h.subscribe();

    // With none designated: F5 does not guess, it ASKS that one be chosen.
    let ack = h.dispatch(tecla("F5")).await.expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "host-no-target-designated".to_owned()
        },
        "with three panes the destination is chosen, not tie-broken: {ack:?}"
    );

    // No preset binds `layout.set-target`, so it is run through the
    // PALETTE — the catalogue's other door, and it works just as well.
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
        .expect("host alive");
        for c in ["s", "e", "t", "-", "t", "a", "r", "g", "e", "t"] {
            h.dispatch(tecla_de(c)).await.expect("host alive");
        }
        h.dispatch(tecla("Enter")).await.expect("host alive");
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let foto = siguiente_foto(&mut sub).await;
        if rol(&foto.layout, 3) == Some(SlotRole::Target) {
            puesto = true;
            break;
        }
    }
    assert!(puesto, "se puede designar el tercero");

    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let despues = siguiente_foto(&mut sub).await;
    assert_eq!(
        rol(&despues.layout, 3),
        Some(SlotRole::Target),
        "el destino ELEGIDO sobrevive al cambio de foco"
    );
}

/// The marks sending consumes are the SOURCE slot's, even if focus moved to
/// another one between the question and the answer.
///
/// `FocusSlot` is not barred while a dialog is open: only keys are. A click
/// on the other pane was erasing the foreign pane's marks and leaving the
/// ones that had just been sent intact.
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
        .expect("host alive");
    }
    let mut sub = h.subscribe();
    h.dispatch(tecla("F5")).await.expect("host alive");
    let id = siguientes_dialogos(&mut sub).await[0].id;

    // And NOW focus moves to the other pane, without closing the dialog.
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    anotados(&backend, "the transfer that consumes the marks", 1, |f| {
        f.transferencias.lock().expect("transferencias").clone()
    })
    .await;

    h.dispatch(UiAction::Resync).await.expect("host alive");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(
        listado_de(&foto, 1).marks,
        0,
        "the consumed marks are the slot's that sent them"
    );
}

/// A HIDDEN slot on the affected directory is not listed — what is not seen
/// is not fetched — but it is marked to reload as soon as it comes back.
///
/// Without this, a background tab on the destination directory showed a
/// listing from before the operation until someone navigated by hand, and a
/// key on one of its rows acted against that stale listing.
#[tokio::test]
async fn un_hueco_oculto_afectado_queda_para_recargar() {
    let backend = arbol();
    // Born WIDE, so the second listing really gets listed and ends up
    // `Ready`. If it were born hidden it would be `Loading` from the start
    // and would reload on return for that reason, not this one.
    let (h, snap) = host_con_layout(Arc::clone(&backend), "orthodox", (160, 40)).await;
    assert!(
        snap.slots
            .iter()
            .filter(|s| matches!(s, SlotView::Browser(_)))
            .count()
            >= 2,
        "both listings are visible"
    );
    let mut sub = h.subscribe();
    // And now it narrows until only one fits.
    h.dispatch(UiAction::SetViewport {
        width: 30,
        height: 10,
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let estrecha = siguiente_foto(&mut sub).await;
    assert_eq!(
        estrecha
            .slots
            .iter()
            .filter(|s| matches!(s, SlotView::Browser(_)))
            .count(),
        1,
        "with 30 columns only one listing fits"
    );
    h.dispatch(tecla("F8")).await.expect("host alive");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    siguientes_tasks(&mut sub).await;
    let listados_antes = backend.listados();
    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    // The task's outcome arrives over its own channel: it is let run before
    // widening, so the hidden slot is already marked.
    asentar().await;

    // The window widens: the slot that was hidden comes back, and since it
    // was left marked as LOADING, it gets listed.
    h.dispatch(UiAction::SetViewport {
        width: 160,
        height: 40,
    })
    .await
    .expect("host alive");
    hasta(&backend, "the re-listing of the slot that came back", |f| {
        (f.listados() > listados_antes + 1).then_some(())
    })
    .await;
}

/// In read-only, the PALETTE also does not offer what mutates.
///
/// It was the only door that did not go through the effective keymap: it
/// was offering copy, move and delete, and the execution guard was
/// rejecting them. Offering what is going to be refused is promising
/// something that will not happen.
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
    .expect("host alive");
    let p = siguiente_paleta(&mut sub).await.expect("la paleta abre");
    let solo_lectura =
        norte_ui_host::commands::implementados(norte_ui_host::commands::Efectos::SoloLectura);
    let no_inertes = norte_ui_host::commands::IMPLEMENTADOS
        .iter()
        .filter(|c| !solo_lectura.contains(c));
    for cmd in no_inertes {
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
    let ack = h.dispatch(tecla("F5")).await.expect("host alive");
    let ActionAck::Unavailable { reason_key } = ack else {
        panic!("se esperaba no disponible: {ack:?}");
    };
    assert!(
        reason_key == "cmd-not-here" || reason_key == "host-read-only",
        "y con un motivo del vocabulario, no uno inventado: {reason_key}"
    );
}

/// A refresh keeps the MARKS, by path.
///
/// `set_listing` clears them because the rows are different — correct for a
/// `cd`, and a punishment for whoever did not move: a copy's DESTINATION
/// pane gets re-listed when the copy finishes, and it was sweeping away a
/// selection its owner had made by hand and that nobody had sent.
#[tokio::test]
async fn las_marcas_sobreviven_a_un_refresco() {
    let backend = arbol();
    let (h, snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // Marks on the DESTINATION pane: they are not the ones the copy
    // consumes, so the only thing that can remove them is the re-listing.
    let b2 = listado_de(&snap, 2);
    let generation2 = b2.generation;
    let claves: Vec<_> = b2.rows.iter().map(|r| r.key).collect();
    assert!(!claves.is_empty(), "the destination has rows");
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    for key in &claves {
        h.dispatch(UiAction::ToggleMark {
            slot_id: 2,
            key: *key,
            generation: generation2,
        })
        .await
        .expect("host alive");
    }
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host alive");

    h.dispatch(tecla("F5")).await.expect("host alive");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    siguientes_tasks(&mut sub).await;
    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let foto = siguiente_foto(&mut sub).await;
        let b = listado_de(&foto, 2);
        if b.generation == generation2 {
            continue;
        }
        assert_eq!(
            b.marks,
            claves.len() as u64,
            "re-listing the destination does not sweep away what its owner \
             had marked"
        );
        return;
    }
    panic!("the destination's refresh never arrived");
}

/// A move re-lists the SOURCE pane even if the provider writes its entries'
/// parent with a DIFFERENT spelling of the same directory.
///
/// An entry's parent is written by the provider; the pane's directory can
/// come from config, from the session or from a favorite. On macOS (NFD
/// against NFC) and against a case-insensitive server they are two strings
/// for the same place, and byte-for-byte comparison does not join them (ADR
/// 0061).
#[tokio::test]
async fn mover_relista_el_origen_aunque_el_provider_lo_escriba_distinto() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)],
    );
    f.pon("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    // The provider hangs its entries off `⟨mem⟩/CASA`, not `⟨mem⟩/casa`.
    f.padre_distinto = true;
    let backend = Arc::new(f);
    let (h, snap) = host_con_layout(Arc::clone(&backend), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();

    // The destination, in `docs`.
    let b2 = listado_de(&snap, 2);
    let docs = b2
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("is there");
    let (key, generation) = (docs.key, b2.generation);
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    h.dispatch(UiAction::Activate {
        slot_id: 2,
        key,
        generation,
    })
    .await
    .expect("host alive");
    let mut despues = siguiente_foto(&mut sub).await;
    while listado_de(&despues, 2).path_display == listado_de(&snap, 2).path_display {
        despues = siguiente_foto(&mut sub).await;
    }
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host alive");

    h.dispatch(tecla("F6")).await.expect("host alive");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    siguientes_tasks(&mut sub).await;
    let antes = backend.listados();
    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    // BOTH panes. Without pinning the SOURCE slot's directory, the entry's
    // parent (`⟨mem⟩/CASA`) would not match what the pane shows
    // (`⟨mem⟩/casa`) and the source would be left without a re-listing.
    hasta(&backend, "both panes re-listed", |f| {
        (f.listados() >= antes + 2).then_some(())
    })
    .await;
}

// ---------------------------------------------------------------------------
// A BATCH of transfers: its caps and its count (#271).
// ---------------------------------------------------------------------------

/// Marks the active slot's first N rows, one by one.
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
        .expect("host alive");
    }
}

/// A batch whose queueing rejections are ALL of it: the bar says ONE phrase
/// with the count, not N phrases of which only the last survives (#271,
/// point 3).
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
    h.dispatch(tecla("F5")).await.expect("host alive");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");

    // The summary says HOW MANY, meaning the count existed: without it the
    // bar would carry the last error's phrase and nothing else.
    let foto = esperar_foto(&h, &mut sub, "the batch gets summarized", |f| {
        f.status.message.as_deref().is_some_and(|m| m.contains('4'))
    })
    .await;
    let msg = foto.status.message.clone().expect("there is a summary");
    assert!(
        msg.contains('4'),
        "the summary does not count the batch: {msg}"
    );
    assert!(foto.tasks.is_empty(), "none ever became a task");
}

/// And with the tasks queued: the summary counts the OUTCOMES, and only once
/// the whole batch is resolved (#271, point 2).
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
    h.dispatch(tecla("F5")).await.expect("host alive");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");

    let foto = esperar_foto(&h, &mut sub, "el lote se resume", |f| {
        f.status.message.is_some()
    })
    .await;
    let msg = foto.status.message.clone().expect("there is a summary");
    assert!(
        msg.contains('4') && msg.contains('0'),
        "the summary says 4 requested and 0 failed: {msg}"
    );
}

/// The batch cap (#271, point 4): `pane.copy` operates on the marks, and
/// marking has no ceiling. Without this cap the batch was queued whole and
/// the limit was discovered halfway, when the daemon started rejecting over
/// `MAX_LIVE_TASKS`: with half of it done and nothing saying where it cut
/// off.
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
    // The destination, by hand and not with the two-pane helper: with a
    // listing this size the drain MOVES the generation, and an `Activate`
    // with the startup one arrives stale. It waits for the listing to be
    // whole and reads THAT snapshot's generation.
    let asentado = esperar_foto(&h, &mut sub, "the drain finishes", |f| {
        listado_de(f, 2).total_rows.unwrap_or(0) >= CUANTAS as u64 + 2
    })
    .await;
    let b2 = listado_de(&asentado, 2);
    let docs = b2
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("the directory is there");
    let (key, generation) = (docs.key, b2.generation);
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    h.dispatch(UiAction::Activate {
        slot_id: 2,
        key,
        generation,
    })
    .await
    .expect("host alive");
    esperar_foto(&h, &mut sub, "the destination lands in /casa/docs", |f| {
        listado_de(f, 2).path_display.ends_with("/casa/docs")
    })
    .await;
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host alive");
    // And the source fully loaded: `invert_marks` marks what is LOADED, and
    // with the drain halfway it would mark a hundred and the cap would not
    // be touched.
    esperar_foto(&h, &mut sub, "the source is whole", |f| {
        listado_de(f, 1).total_rows.unwrap_or(0) >= CUANTAS as u64 + 2
    })
    .await;
    ejecutar_por_paleta(&h, &mut sub, "mark.invert").await;
    let ack = h.dispatch(tecla("F5")).await.expect("host alive");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-batch-too-large"),
        "{ack:?}"
    );
    let foto = foto(&h, &mut sub).await;
    assert!(
        foto.dialogs.is_empty(),
        "no dialog opens promising something that will not happen"
    );
}

/// The canonical corpus against the APPROVAL dialog (#277).
///
/// It is the surface where lying costs the most: what is read there is the
/// only thing a human has to decide whether an agent deletes their files.
///
/// The paths arrive from the daemon as text already redacted, not as
/// `VPath`, so the test passes them first through `display_lossy` — which is
/// what `norte_core::engine::span_path` does, and cannot be called from here
/// because the host does not depend on the core (ADR 0066). That step IS
/// what makes the test mean something: feeding it raw bytes would light the
/// flag through a path that does not happen in production.
#[tokio::test]
async fn el_corpus_hostil_cruza_el_dialogo_de_aprobacion() {
    // The four the daemon's lossy conversion ALTERS, and `zwsp_twin` as
    // CONTRAST: the lossy conversion does not touch that one — it is valid
    // UTF-8 — and its flag has to turn on through the other path, the
    // masking one.
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
            .unwrap_or_else(|| panic!("fixture {id} is in the corpus"));
        let p = norte_proto::VPath::parse("mem:///casa")
            .expect("root")
            .join(norte_proto::Segment::new(n.bytes.clone()).expect("segment"));
        // The daemon's step: `span_path` is this for any authority with no
        // userinfo, which is the case for a `mem://`.
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
        .expect("the host is listening");

        let dialogos = siguientes_dialogos(&mut sub).await;
        let d = &dialogos[0];
        let linea = d.body.first().expect("the path is there");
        sin_peligro(&linea.text, id, "a path from the approval dialog");
        assert!(
            linea.hostile,
            "[{id}] the line is painted differently from what there is and does NOT say so: {:?}",
            linea.text
        );
        // And the deadline goes in ITS OWN field, never among the paths:
        // among them a file name could impersonate it (`approval_ttl_line_spoof`).
        assert!(
            d.deadline.is_some(),
            "[{id}] the deadline has to have its own field"
        );
        assert!(
            d.body.iter().all(|l| Some(&l.text) != d.deadline.as_ref()),
            "[{id}] the deadline snuck in among the paths: {:?}",
            d.body
        );
    }
}

/// A broken plugin's directory is BYTES, and it was arriving already
/// converted (#265).
///
/// `PluginLoadError.dir` is a `String` the core was producing with a
/// `to_string_lossy` with NO mark, so a directory named `caf\xff` — fixture
/// `lossy_collapse_ff` from the corpus — crossed the wire already carrying
/// its `U+FFFD`. And `display_name` cannot recover it: it puts `lossy` only
/// when `from_utf8` fails and `masked` only in the face of a terminal
/// hazard, and `U+FFFD` is neither of those two things — it is Specials.
/// The row declared itself faithful.
///
/// The test is a PAIR, because a single row does not tell the fix apart
/// from the heuristic that came before:
///
/// - `caf\xff` (real non-UTF-8 bytes) → the row gets MARKED. The old
///   heuristic also marked it, so this half alone proves nothing.
/// - `caf\u{FFFD}` (a directory actually NAMED that, in valid UTF-8) → the
///   row is NOT marked. It is the heuristic's false positive — "the string
///   carries a replacement, so someone converted it" — and it is the half
///   that only passes with the bytes on hand.
///
/// What this fix does NOT do: tell `caf\xff` apart from `caf\xfe` when
/// painting. `display_name` maps every invalid byte to the same `U+FFFD`,
/// so the two keep being painted the same. What is recovered is the MARK,
/// not the spelling.
#[tokio::test]
async fn un_directorio_de_plugin_no_utf8_llega_marcado_y_sin_falso_positivo() {
    let crudos = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "lossy_collapse_ff")
        .expect("the fixture is there")
        .bytes;
    assert!(
        std::str::from_utf8(&crudos).is_err(),
        "the premise: they are bytes that are NOT UTF-8"
    );
    // And the legitimate twin: a name that IS `U+FFFD` on disk, in valid
    // UTF-8. Nobody converted it, so marking it would be lying.
    let honesto = "caf\u{FFFD}".as_bytes().to_vec();
    assert!(std::str::from_utf8(&honesto).is_ok());

    let mut backend = arbol_con_plugins(Vec::new(), &[]);
    {
        let f = std::sync::Arc::get_mut(&mut backend).expect("single reference");
        for bytes in [&crudos, &honesto] {
            // What the core sends: the string ALREADY converted, with the
            // bytes alongside. The two rows are told apart by their text;
            // what CANNOT be told apart by the text is which of the two got
            // converted, which is exactly the question.
            let convertida = String::from_utf8_lossy(bytes).into_owned();
            f.errores_de_carga
                .push((convertida.clone(), "el manifiesto no parsea".to_owned()));
            f.bytes_de_carga.insert(convertida, bytes.clone());
        }
    }
    let (h, _snap) = host_arbol(std::sync::Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host alive");
    let _ = siguiente_extensiones(&mut sub).await.expect("opens");
    let v = extensiones_cargadas(&mut sub).await;

    // The two strings collapse into a single key, so the fake ends up
    // sending ONE row: what is asserted is its flag, which with the honest
    // name's bytes has to be FALSE.
    assert_eq!(v.errors.len(), 2, "both rows arrive: {:?}", v.errors);

    // The one with raw bytes: it gets marked, and with the bytes on hand it
    // gets marked for the right reason — `display_name` saw they were not
    // UTF-8 — and not by the heuristic.
    let convertida = v
        .errors
        .iter()
        .find(|e| e.dir == String::from_utf8_lossy(&crudos))
        .expect("the raw-bytes row is there");
    assert!(
        convertida.hostile,
        "what is painted differs from what there is and does NOT say so: {:?}",
        convertida.dir
    );

    // And the honest one: NOT marked. This is the half that only passes
    // with the bytes on hand; with the string's heuristic it came out
    // over-marked.
    let fila_limpia = v
        .errors
        .iter()
        .find(|e| e.dir == "caf\u{FFFD}")
        .expect("the honest row is there");
    assert!(
        !fila_limpia.hostile,
        "a directory that IS NAMED `caf\u{FFFD}` was not converted: marking it \
         is the false positive the bytes exist to remove"
    );
    for c in fila_limpia.dir.chars() {
        assert!(
            !norte_encoding::is_terminal_hazard(c),
            "a hazard crossed unmasked: {:?}",
            fila_limpia.dir
        );
    }
}

/// And the other half, isolated: WITHOUT bytes — a 0.52 peer — the heuristic
/// marks that same honest row, and that is the false positive #265 removes.
#[tokio::test]
async fn sin_los_bytes_un_nombre_honesto_con_reemplazo_sale_marcado_de_mas() {
    let honesto = "caf\u{FFFD}".to_owned();
    let mut backend = arbol_con_plugins(Vec::new(), &[]);
    std::sync::Arc::get_mut(&mut backend)
        .expect("single reference")
        .errores_de_carga = vec![(honesto, "el manifiesto no parsea".to_owned())];
    // Deliberately WITHOUT `bytes_de_carga`: it is a 0.52 daemon.
    let (h, _snap) = host_arbol(std::sync::Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host alive");
    let _ = siguiente_extensiones(&mut sub).await.expect("opens");
    let v = extensiones_cargadas(&mut sub).await;
    assert!(
        v.errors[0].hostile,
        "against an old peer the heuristic is all there is, and it marks \
         too much rather than too little"
    );
}

/// #268 — two marks that are ONE name at the destination are rejected as a
/// whole.
///
/// On an ext4, `README.txt` and `readme.txt` are two files; on NTFS or APFS
/// they are one. Queueing both lets one win — which one is not
/// deterministic — and the other fail with no explanation, on an arbitrary
/// member of the pair.
///
/// The test runs the THREE pairs from the canonical corpus, which are three
/// different folds: ASCII case, NFC/NFD normalization, and an ext4 `+F`'s
/// full fold. A fix that only looked at case would pass the first and fail
/// the other two.
#[tokio::test]
async fn dos_marcas_que_pliegan_al_mismo_nombre_no_se_encolan() {
    let corpus = norte_testkit::corpus::hostile_names();
    let bytes_de = |id: &str| {
        corpus
            .iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("fixture {id} is there"))
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
        assert_ne!(uno, otro, "[{a}/{b}] the premise: they are different bytes");

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
        // The DESTINATION folds: an APFS, an NTFS or an ext4 `+F`. Without
        // this flag the case could not be written, which is what the issue
        // said.
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
        // That the destination's fold has arrived: it is requested on
        // landing, not in front of the dialog, so it has to be waited for.
        esperar_foto(&h, &mut sub, "the destination says how it folds", |_| true).await;
        marca_todo(&h, &mut sub, 1).await;
        let ack = h.dispatch(tecla("F5")).await.expect("host alive");

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
            "[{a}/{b}] not one was queued: the batch is rejected AS A WHOLE"
        );
    }
}

/// And at a destination that does NOT fold, the same two marks are two files
/// and the batch goes out. The check must not cost the legitimate
/// operation.
#[tokio::test]
async fn dos_gemelos_de_caja_hacia_un_destino_sensible_si_se_encolan() {
    let corpus = norte_testkit::corpus::hostile_names();
    let bytes_de = |id: &str| {
        corpus
            .iter()
            .find(|n| n.id == id)
            .expect("the fixture is there")
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
    // No flag = a regular ext4, which distinguishes case.
    let backend = Arc::new(f);
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    esperar_foto(&h, &mut sub, "the destination says how it folds", |_| true).await;
    marca_todo(&h, &mut sub, 1).await;
    let ack = h.dispatch(tecla("F5")).await.expect("host alive");
    assert!(
        matches!(&ack, ActionAck::Applied { .. }),
        "an ext4 distinguishes case: they are two files and the batch is legitimate: {ack:?}"
    );
}

/// #311: computing checksums in the window. The Task is queued with what is
/// marked, and when its REPORT arrives a dialog opens with one row per file
/// and the option to copy the list.
#[tokio::test]
async fn calcular_sumas_abre_el_dialogo_con_sus_filas() {
    let backend = arbol();
    // The report the fake daemon will return: one digest for `notas.txt`.
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
    .expect("host alive");

    let dialogos = siguientes_dialogos(&mut sub).await;
    assert_eq!(dialogos.len(), 1, "ONE dialog opens with the checksums");
    assert_eq!(dialogos[0].body.len(), 1, "one row per file");
    assert!(
        dialogos[0].body[0].text.contains("ababab"),
        "with its digest truncated: {:?}",
        dialogos[0].body[0].text
    );
    assert!(
        dialogos[0].choices.iter().any(|c| c.id == "confirm"),
        "and with the COPY option, which is the only thing done with a list of digests"
    );
    assert_eq!(
        backend.sumas_pedidas.lock().expect("sumas").len(),
        1,
        "ONE batch was requested"
    );
}

/// A PARTIAL report — a cancelled Task leaves `pending` above zero — is
/// compared to nothing: accusing files nobody ever read is the worst
/// possible mistake in the tool that exists to check.
#[tokio::test]
async fn un_informe_a_medias_no_abre_veredicto() {
    let backend = arbol();
    *backend.sumas_informe.lock().expect("informe") =
        norte_proto::methods::FsChecksumReportResult {
            entries: Vec::new(),
            algo: norte_proto::methods::ChecksumAlgo::Sha256,
            // What a cancellation leaves: the Task ended and work remains.
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
    .expect("host alive");
    // It waits for the report to HAVE come back: what is asserted is that
    // with it in hand nothing opens, not that it had not arrived yet.
    hasta(&backend, "the checksum report requested", |f| {
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
        "a partial report opens no verdict: {:?}",
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

/// #314: the window changes permissions. The dialog carries a text field —
/// the mode, in octal — says how many entries it applies to, and confirming
/// queues the Task with the mode that was typed.
///
/// The rule for what a valid mode is is the SHARED one
/// (`norte_frontend::chmod::parse_mode`), the same one the terminal uses:
/// two different readings of `755` in two frontends would be two different
/// permissions.
#[tokio::test]
async fn cambiar_permisos_teclea_y_encola() {
    let backend = arbol();
    let (host, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = host.subscribe();

    host.dispatch(alt_a()).await.expect("host alive");
    let dialogos = siguientes_dialogos(&mut sub).await;
    let id = dialogos[0].id;
    assert!(
        dialogos[0].input.is_some(),
        "the permissions dialog says typing happens here"
    );

    host.dispatch(UiAction::DialogInput {
        id,
        text: "0750".to_owned(),
    })
    .await
    .expect("host alive");
    let _ = siguientes_dialogos(&mut sub).await;
    host.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let lotes = anotados(&backend, "the permissions batch queued", 1, |f| {
        f.permisos.lock().expect("permisos").clone()
    })
    .await;
    assert_eq!(lotes.len(), 1, "ONE batch was queued");
    assert_eq!(lotes[0].1, 0o750, "in OCTAL: 750, not decimal 750");
    assert_eq!(lotes[0].0.len(), 1, "on what is under the cursor");
}

/// A mode that is not valid queues nothing, and it is said.
#[tokio::test]
async fn un_modo_invalido_no_cambia_nada() {
    let backend = arbol();
    let (host, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = host.subscribe();

    host.dispatch(alt_a()).await.expect("host alive");
    let dialogos = siguientes_dialogos(&mut sub).await;
    let id = dialogos[0].id;
    host.dispatch(UiAction::DialogInput {
        id,
        text: "899".to_owned(),
    })
    .await
    .expect("host alive");
    let _ = siguientes_dialogos(&mut sub).await;
    let ack = host
        .dispatch(UiAction::Dialog {
            id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
    assert!(
        matches!(&ack, ActionAck::Unavailable { .. }),
        "899 is not octal and it is said: {ack:?}"
    );
    asentar().await;
    assert!(
        backend.permisos.lock().expect("permisos").is_empty(),
        "and nothing was queued"
    );
}
