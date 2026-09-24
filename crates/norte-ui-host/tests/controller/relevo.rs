use super::*;

/// Starts the host over a saved session that brings `a.txt` MARKED, with or
/// without `--attach`, and returns the first snapshot in which the listing
/// has already arrived.
async fn arrancar_con_marca(attach: bool) -> norte_ui_host::ViewSnapshot {
    let mut fake = Falso::default();
    fake.pon(
        "mem:///casa",
        vec![(b"a.txt".to_vec(), false), (b"b.txt".to_vec(), false)],
    );
    let mut session = crate::base::sesion_guardada(1, 7, 1, "mem:///casa");
    let mut body: norte_frontend::session::SessionBody =
        serde_json::from_value(session.body.clone()).expect("body");
    body.slots.get_mut(&1).expect("slot").marks =
        vec![VPath::parse("mem:///casa/a.txt").expect("vpath")];
    session.body = serde_json::to_value(&body).expect("json");
    *fake.sesion.lock().expect("sesión") = (session, true);
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: Arc::new(fake),
        initial_dir: VPath::parse("mem:///casa").expect("vpath"),
        initial_dir_pedido: false,
        attach,
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
    .expect("starts");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Resync).await.expect("host alive");
    foto_hasta(&h, &mut sub, "casa's listing with its two rows", |s| {
        (listado(s).rows.len() == 2).then(|| s.clone())
    })
    .await
}

/// The window RETURNS a handoff's marks with `--attach` (phase 9) — and only
/// then.
///
/// It used to write them to the session and never read them back: a handoff
/// from the terminal opened in the right place and with nothing marked,
/// which is exactly the half a `cd` does not redo. And they are seeded when
/// the LISTING arrives, not when the session is read: the listing that lands
/// clears the marks, the same trap that bit the terminal.
#[tokio::test]
async fn with_attach_the_window_returns_the_handoffs_marks() {
    // On the heap: the future carries the whole `Estado`, and on the stack
    // it goes over `large_futures`'s threshold (see `host_grande` in
    // `payload.rs`).
    let snap = Box::pin(arrancar_con_marca(true)).await;
    let rows = &listado(&snap).rows;
    let a = rows.iter().find(|r| r.display_name == "a.txt").expect("a");
    let b = rows.iter().find(|r| r.display_name == "b.txt").expect("b");
    assert!(a.marked, "the handoff's mark comes back");
    assert!(!b.marked, "and only that one");
}

/// Without `--attach` a startup is a startup: marks from a handoff that was
/// left half-done do not come back to life the next day.
#[tokio::test]
async fn without_attach_the_saved_marks_do_not_come_back() {
    let snap = Box::pin(arrancar_con_marca(false)).await;
    assert!(
        listado(&snap).rows.iter().all(|r| !r.marked),
        "an ordinary startup returns nothing marked"
    );
}

// ---------------------------------------------------------------------------
// The HANDOFF to the terminal (phase 9 of the WOW program).
// ---------------------------------------------------------------------------

/// The happy path: it writes the screen WITH the marks, releases it, and
/// only then asks to open the terminal.
///
/// The order is what is checked, and it is not cosmetic: releasing before
/// writing would leave the terminal reading last second's screen, and
/// launching it before releasing would leave it opening on a session that
/// still has an owner.
#[tokio::test]
async fn the_handoff_writes_releases_and_then_opens_the_terminal() {
    use norte_ui_host::dto::NativeEffect;

    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b"a.txt".to_vec(), false), (b"b.txt".to_vec(), false)],
    );
    f.suelta_la_sesion = true;
    let backend = Arc::new(f);
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let mut native = h.native_effects();

    // Mark a row: it is the only thing on the screen that did not travel
    // before.
    let b = listado(&snap);
    let row = b
        .rows
        .iter()
        .find(|r| r.display_name == "a.txt")
        .expect("is there");
    h.dispatch(UiAction::ToggleMark {
        slot_id: 1,
        key: row.key,
        generation: b.generation,
    })
    .await
    .expect("host alive");

    por_la_paleta(&h, &mut sub, "handoff").await;

    let effect = tokio::time::timeout(std::time::Duration::from_secs(5), native.recv())
        .await
        .expect("the handoff's effect comes out")
        .expect("channel alive");
    assert!(
        matches!(effect, NativeEffect::HandoffToTerminal { .. }),
        "the effect is the handoff's: {effect:?}"
    );

    // And the screen that got written carries the mark, by PATH.
    let puts = backend.puestas.lock().expect("puestas").clone();
    let last = puts.last().expect("something was written");
    // ADR 0139: handing over the screen means the terminal opens with THIS
    // one, so the handoff also writes the shared key, not just its own.
    let layouts = last.get("layouts").expect("layouts");
    assert_eq!(
        layouts.get("default"),
        layouts.get("default@window"),
        "the terminal receives the window's layout"
    );
    assert!(layouts.get("default").is_some());
    let marks = last
        .get("slots")
        .and_then(|s| s.get("1"))
        .and_then(|s| s.get("marks"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    assert_eq!(
        marks,
        serde_json::json!(["mem:///casa/a.txt"]),
        "what is marked travels with the handoff, by path: {last}"
    );
    assert_eq!(
        backend.sueltas.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the session was released, once"
    );
}

/// If the handoff's terminal does NOT open, the window stays and says so
/// (ADR 0123 amendment).
///
/// The first version launched the emulator and forgot about it: it stayed
/// saying "handing over the screen…" with the session already released. Now
/// whoever hosts it warns with `HandoffFailed`, and the host tells which of
/// the two failures it was.
#[tokio::test]
async fn if_the_terminal_does_not_open_the_window_stays_and_says_so() {
    use norte_ui_host::dto::NativeEffect;

    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    f.suelta_la_sesion = true;
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let mut native = h.native_effects();
    por_la_paleta(&h, &mut sub, "handoff").await;
    let effect = tokio::time::timeout(std::time::Duration::from_secs(5), native.recv())
        .await
        .expect("the handoff's effect comes out")
        .expect("channel alive");
    assert!(matches!(effect, NativeEffect::HandoffToTerminal { .. }));

    // Whoever hosts it found no emulator.
    let ack = h
        .dispatch(UiAction::HandoffFailed { no_terminal: true })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, norte_ui_host::ActionAck::Applied { .. }),
        "with a handoff in progress, it is handled: {ack:?}"
    );
    let mut said = String::new();
    for _ in 0..10 {
        said = siguiente_aviso(&mut sub).await;
        if said.starts_with("msg-handoff-no") {
            break;
        }
    }
    assert_eq!(
        said, "msg-handoff-no-terminal",
        "it says which of the two failures it was"
    );
}

/// A `HandoffFailed` with NO handoff in progress does nothing.
///
/// The action can be sent by anyone talking to the host: without this guard,
/// it would be enough to send it to paint "the terminal did not start" over
/// a window that had asked for nothing, and to make it request the session
/// again.
#[tokio::test]
async fn a_handoff_failed_with_no_handoff_is_stale() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let ack = h
        .dispatch(UiAction::HandoffFailed { no_terminal: false })
        .await
        .expect("host alive");
    assert!(
        !matches!(ack, norte_ui_host::ActionAck::Applied { .. }),
        "with no handoff there is nothing to handle: {ack:?}"
    );
}

/// If the daemon says this connection did NOT own it, nothing gets launched.
///
/// `released: false` is not an error, it is a fact: the session still has an
/// owner. Launching the terminal then would open it on someone else's
/// listing, and this window would have closed for nothing.
#[tokio::test]
async fn if_it_could_not_be_released_nothing_opens() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    f.suelta_la_sesion = false;
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let mut native = h.native_effects();

    por_la_paleta(&h, &mut sub, "handoff").await;
    hasta(&backend, "the attempt to release", |f| {
        (f.sueltas.load(std::sync::atomic::Ordering::SeqCst) > 0).then_some(())
    })
    .await;
    asentar().await;

    // Nothing crosses the native effects channel.
    assert!(
        native.try_recv().is_err(),
        "a handoff that did not release opens no terminal"
    );
}
