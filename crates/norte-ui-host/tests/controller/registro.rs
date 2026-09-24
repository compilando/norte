use super::*;

// ---------------------------------------------------------------------------
// The log panel (#326).
// ---------------------------------------------------------------------------

/// Emits a few lines INSIDE the ring, through its real path.
///
/// Through the `tracing` layer and not a direct `push`: the ring exposes no
/// such thing, and it must not — the filter the layer passes through is where
/// the `suppaftp` guard lives, which logs `PASS <password>` at TRACE level. A
/// shortcut for tests that skipped that guard would be testing a path that
/// does not exist.
pub(super) fn con_lineas(anillo: &norte_config::logring::LogRing, f: impl FnOnce()) {
    use tracing_subscriber::layer::SubscriberExt as _;
    let s = tracing_subscriber::registry().with(norte_config::logring::ring_layer(anillo));
    tracing::subscriber::with_default(s, f);
}

/// A host with a log ring mounted and a few lines inside it.
pub(super) async fn host_con_registro() -> (UiHost, norte_config::logring::LogRing) {
    host_con_backend_y_registro(Falso::con(&["a"])).await
}

/// The same, with a double the test has armed: it is what is needed for the
/// remote half (#328), where what is being tested is what the daemon
/// answers.
pub(super) async fn host_con_backend_y_registro(
    backend: Arc<Falso>,
) -> (UiHost, norte_config::logring::LogRing) {
    let anillo = norte_config::logring::LogRing::new(64);
    // At DEBUG so the five fit; the panel shows up to INFO when opened, which
    // is what makes the level-filter test interesting.
    anillo.set_level(norte_config::logline::LogLevel::Debug);
    let h = host_con_backend_y_anillo(backend, Some(anillo.clone())).await;
    (h, anillo)
}

/// And the same with NO ring in this process: nobody mounted the `tracing`
/// layer.
///
/// It is not a lab case — it is what the window sees when the ring is not
/// installed — and it is the one that decides whether "both" can be
/// announced over a list that is entirely the daemon's.
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
    .expect("starts")
    .0;
    // The starting layout carries no log slot: it opens with its key, the
    // way a person would open it. And this way the test ALSO covers that
    // `layout.log` is bound and reaches the effect.
    tecla_registro(&h).await;
    h
}

/// The key that opens the log — and, pressed again, closes it.
pub(super) async fn tecla_registro(h: &UiHost) {
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "l".to_owned(),
        ctrl: false,
        alt: true,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host alive");
}

/// The snapshot's log slot, if there is one.
pub(super) fn registro(snap: &norte_ui_host::ViewSnapshot) -> &norte_ui_host::dto::LogSlotView {
    snap.slots
        .iter()
        .find_map(|s| match s {
            SlotView::Log(l) => Some(&**l),
            _ => None,
        })
        .expect("there is a log slot")
}

/// #326: the window PAINTS the log, with its level, its filter and its
/// source.
///
/// It used to fall to "unsupported kind", grayed out: opening a slot that
/// only paints disabled is not opening it. And the panel says which PROCESS
/// the lines belong to, because the window runs its own daemon and its own
/// lines are not its — staying silent about it would make the panel look
/// broken.
#[tokio::test]
async fn the_window_paints_the_log() {
    let (h, anillo) = host_con_registro().await;
    con_lineas(&anillo, || {
        tracing::info!(target: "norte_prueba", "una linea de prueba");
    });
    let mut sub = h.subscribe();

    let view = foto_hasta(&h, &mut sub, "the log panel", |f| {
        f.slots
            .iter()
            .find_map(|s| match s {
                SlotView::Log(l) => Some((**l).clone()),
                _ => None,
            })
            .filter(|l| !l.lines.is_empty())
    })
    .await;
    assert_eq!(view.level, "info", "opens at INFO, like the ring");
    assert!(view.following, "born pinned to the end");
    assert_eq!(
        view.source,
        norte_i18n::t_in(norte_i18n::Lang::Es, "log-source-window"),
        "says which process the lines belong to"
    );
    assert!(
        view.lines.iter().any(|l| l.message.contains("prueba")),
        "the line just emitted is there: {:?}",
        view.lines
    );
    let _ = anillo;
}

/// The panel shows the rows the RENDERER says fit, not one.
///
/// The host starts with one — never zero, so a page moves something — and
/// waits to be told the height. While nobody told it, a twelve-row panel
/// painted ONE clipped line and the wheel skipped two per notch: the same bug
/// the TUI fixed by no longer guessing the viewport.
#[tokio::test]
async fn the_log_shows_the_rows_it_is_told_fit() {
    let (h, anillo) = host_con_registro().await;
    con_lineas(&anillo, || {
        for i in 0..8 {
            tracing::info!(target: "norte_prueba", n = i, "linea");
        }
    });
    let mut sub = h.subscribe();

    h.dispatch(UiAction::LogSetVisibleRange { rows: 6 })
        .await
        .expect("host alive");
    let view = foto_hasta(&h, &mut sub, "six rows", |f| {
        let l = registro(f).clone();
        (l.lines.len() == 6).then_some(l)
    })
    .await;
    assert_eq!(view.lines.len(), 6);
    assert_eq!(view.total, 8, "all eight pass the filter; six are shown");
}

/// A wild `rows` gets CLAMPED: the webview does not decide how heavy a
/// snapshot is.
///
/// With no ceiling, a `rows` of four billion makes every snapshot carry the
/// whole ring — two thousand lines per action, which is exactly what
/// decision D7 exists to prevent. The listing's path was already clamped the
/// same way.
#[tokio::test]
async fn a_wild_height_does_not_send_the_whole_ring() {
    let (h, anillo) = host_con_registro().await;
    con_lineas(&anillo, || {
        for i in 0..40 {
            tracing::info!(target: "norte_prueba", n = i, "linea");
        }
    });
    let mut sub = h.subscribe();

    h.dispatch(UiAction::LogSetVisibleRange { rows: u32::MAX })
        .await
        .expect("host alive");
    asentar().await;
    let view = foto_hasta(&h, &mut sub, "the clamped log", |f| {
        Some(registro(f).clone())
    })
    .await;
    assert!(
        view.lines.len() <= 512,
        "{} lines travelled: the ceiling was not applied",
        view.lines.len()
    );
}

/// Closing the panel LOWERS what the process captures.
///
/// The ring's level is raised live to show more, and it only ever rises.
/// Without this, a single press of "trace" left the process keeping TRACE in
/// memory for the rest of the session — with the `suppaftp` guard as the only
/// barrier — while the interface said "info", with no panel to see it in.
#[tokio::test]
async fn closing_the_panel_lowers_what_gets_captured() {
    let (h, anillo) = host_con_registro().await;
    h.dispatch(UiAction::LogSetLevel {
        level: "trace".to_owned(),
    })
    .await
    .expect("host alive");
    asentar().await;
    assert_eq!(anillo.level(), norte_config::logline::LogLevel::Trace);

    // And while it is open, the panel SAYS more is being captured than it
    // shows: a screenshot saying "info" over a process saving TRACE would be
    // a false answer.
    h.dispatch(UiAction::LogSetLevel {
        level: "info".to_owned(),
    })
    .await
    .expect("host alive");
    let mut sub = h.subscribe();
    let view = foto_hasta(&h, &mut sub, "the capture notice", |f| {
        let l = registro(f).clone();
        (!l.capturing.is_empty()).then_some(l)
    })
    .await;
    assert!(view.capturing.contains("trace"), "{}", view.capturing);

    // Close it with the same key that opened it.
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "l".to_owned(),
        ctrl: false,
        alt: true,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host alive");
    asentar().await;
    assert_eq!(
        anillo.level(),
        norte_config::logline::LogLevel::Info,
        "closing the panel stops capturing what no longer shows"
    );
}

/// Requesting DEBUG RAISES the ring's level, and dropping to ERROR does not
/// stop capturing.
///
/// Both halves matter and both belong to `LogPanel`: filtering on screen
/// what was never logged is impossible, so requesting DEBUG has to make the
/// ring start capturing it; and if dropping stopped capturing, going back up
/// would show a hole the size of however long it stayed down.
#[tokio::test]
async fn the_panels_level_raises_the_rings_and_does_not_lower_it() {
    let (h, anillo) = host_con_registro().await;
    anillo.set_level(norte_config::logline::LogLevel::Info);

    h.dispatch(UiAction::LogSetLevel {
        level: "debug".to_owned(),
    })
    .await
    .expect("host alive");
    asentar().await;
    assert_eq!(
        anillo.level(),
        norte_config::logline::LogLevel::Debug,
        "requesting DEBUG makes the ring capture it"
    );

    h.dispatch(UiAction::LogSetLevel {
        level: "error".to_owned(),
    })
    .await
    .expect("host alive");
    asentar().await;
    assert_eq!(
        anillo.level(),
        norte_config::logline::LogLevel::Debug,
        "lowering what is SHOWN does not stop capturing"
    );
}

/// A level that does not exist is SAID; it does not fall back to `info`.
#[tokio::test]
async fn an_unknown_log_level_does_not_fall_back() {
    let (h, _anillo) = host_con_registro().await;
    let ack = h
        .dispatch(UiAction::LogSetLevel {
            level: "verboso".to_owned(),
        })
        .await
        .expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "host-log-level-unknown".to_owned()
        }
    );
}

/// The filter trims, and detaching from the end is SAID.
///
/// "Nothing is happening" and "you have detached and this is history" are
/// indistinguishable without saying so, and that is half of what the panel is
/// for: one that always jumps to the end cannot be read while something is
/// writing.
#[tokio::test]
async fn the_filter_trims_and_detaching_is_said() {
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
    .expect("host alive");
    let view = foto_hasta(&h, &mut sub, "the filtered log", |f| {
        let l = registro(f).clone();
        (l.filter == "aguja").then_some(l)
    })
    .await;
    assert_eq!(view.total, 1, "only the one that matches: {:?}", view.lines);

    // And detaching: scrolling up through the log stops following the end.
    h.dispatch(UiAction::LogSetFilter {
        filter: String::new(),
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::LogScroll { delta: -1 })
        .await
        .expect("host alive");
    let view = foto_hasta(&h, &mut sub, "the detached log", |f| {
        let l = registro(f).clone();
        (!l.following).then_some(l)
    })
    .await;
    assert!(!view.following);

    h.dispatch(UiAction::LogFollow).await.expect("host alive");
    let view = foto_hasta(&h, &mut sub, "the log back to following", |f| {
        let l = registro(f).clone();
        l.following.then_some(l)
    })
    .await;
    assert!(view.following, "going back to the end can be requested");
}

// ---------------------------------------------------------------------------
// The log panel ALSO reads the daemon (#328).
// ---------------------------------------------------------------------------

/// A line just as it comes over the wire.
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

/// The log panel's snapshot, with anything that was in flight already
/// landed.
///
/// `asentar` first: the daemon's response comes back to the actor through
/// the SAME mailbox as actions, so once the executor goes still the message
/// is already queued and `foto_hasta`'s `Resync` goes behind it. No clock and
/// no guessing.
pub(super) async fn foto_registro(h: &UiHost) -> norte_ui_host::dto::LogSlotView {
    // With a real height: the host starts with ONE row — never zero, so a
    // page moves something — and with one row the visible window is the last
    // line, so a mixed list would look like half of what there is.
    h.dispatch(UiAction::LogSetVisibleRange { rows: 20 })
        .await
        .expect("host alive");
    let mut sub = h.subscribe();
    asentar().await;
    foto_hasta(h, &mut sub, "the log panel", |f| Some(registro(f).clone())).await
}

/// Triggers ONE more round of the 500 ms poll.
///
/// Advancing the clock and not sleeping it: the deadline is REAL — the timer
/// the panel rearms on its own — and that is exactly the tool this file's
/// note on deterministic waits points to for a deadline.
pub(super) async fn sondear(h: &UiHost) {
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_millis(600)).await;
    tokio::time::resume();
    asentar().await;
    let _ = h;
}

/// Leaves the panel's SOURCE at the one requested.
///
/// Built on the real control, which is ONE single button that cycles through
/// the three (`Both` → `Window` → `Daemon` → `Both`): there is no "set this
/// one" action, and manufacturing one just for the tests would test a path
/// nobody uses.
pub(super) async fn poner_fuente(h: &UiHost, fuente: &str) {
    // First the daemon's response is let to land: the control does NOT cycle
    // while it is not known there is a second source — moving the preference
    // behind a reader's back who cannot see it move is what got fixed — so
    // pressing it before the first `log.tail` would do nothing.
    asentar().await;
    let rounds = match fuente {
        "window" => 1,
        "daemon" => 2,
        "both" => 3,
        other => panic!("unknown source: {other}"),
    };
    for _ in 0..rounds {
        h.dispatch(UiAction::LogCycleSource)
            .await
            .expect("host alive");
    }
    asentar().await;
}

/// With a daemon that knows nothing about logging there are no two rings, so
/// there is no picker to show: the panel stays exactly as in #326.
#[tokio::test]
async fn embedded_offers_no_source_picker() {
    let (host, _anillo) = host_con_registro().await;
    let v = foto_registro(&host).await;
    assert!(!v.sources_available);
    assert_eq!(v.source_mode, "window");
}

/// With a daemon, the panel brings the lines of BOTH and each one says
/// where it is from.
#[tokio::test]
async fn with_a_daemon_the_two_sources_mix() {
    let backend = Falso::con(&["a"]);
    backend.responde_log_tail(vec![linea_wire(20, "info", "norte_core", "del daemon")], 1);
    let (host, anillo) = host_con_backend_y_registro(Arc::clone(&backend)).await;
    con_lineas(&anillo, || {
        tracing::info!(target: "norte_prueba", "de la ventana");
    });
    let v = foto_registro(&host).await;
    assert!(v.sources_available);
    assert_eq!(v.source_mode, "both");
    let texts: Vec<_> = v.lines.iter().map(|l| l.message.as_str()).collect();
    assert!(
        texts.iter().any(|t| t.contains("del daemon")),
        "the daemon's are missing: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("de la ventana")),
        "the window's are missing: {texts:?}"
    );
    // And each one says where it came from: in a mixed list, "the daemon
    // wrote this" is half the information.
    let from_daemon = v
        .lines
        .iter()
        .find(|l| l.message.contains("del daemon"))
        .expect("is there");
    assert_eq!(from_daemon.source, "daemon");
    let from_window = v
        .lines
        .iter()
        .find(|l| l.message.contains("de la ventana"))
        .expect("is there");
    assert_eq!(from_window.source, "window");
    // And in "both", which is how the panel is born, it ALREADY says whose
    // level it is: it is the common path, and through it pressing "trace"
    // raises a global daemon ring that never comes back down and that
    // closing this panel does not lower. Saying so only with the daemon as
    // the sole source left exactly the most common time unannounced.
    assert_eq!(
        v.source_note,
        norte_i18n::t_in(norte_i18n::Lang::Es, "log-source-daemon-level"),
        "the common path also warns which level is being touched"
    );
}

/// With no ring in THIS window, the source falls back to the DAEMON.
///
/// The mirror of the embedded case: there, the other side's ring is missing
/// and everything falls to `Window`; here, this one's is missing. Without
/// this, a `Both` over a process that never mounted the layer would be
/// announced as "the window's and the daemon's" while being the daemon's
/// whole list.
#[tokio::test]
async fn with_no_local_ring_the_source_falls_to_the_daemon() {
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
        "and its own are shown: {:?}",
        v.lines
    );
}

/// A daemon that does not know how to serve its log does NOT leave the
/// panel mute: it falls back to the local ring and SAYS so. It is the half
/// #326 already solved, applied to the only reachable case: a daemon of the
/// SAME version compiled without the `logging` feature. An older one never
/// gets here — it dies at `initialize`.
#[tokio::test]
async fn a_daemon_with_no_log_says_so_in_the_panel() {
    let backend = Falso::con(&["a"]);
    backend.log_tail_no_soportado();
    let (host, _anillo) = host_con_backend_y_registro(Arc::clone(&backend)).await;
    let v = foto_registro(&host).await;
    let asked = backend.cursores_pedidos().len();
    assert!(asked > 0, "it did get asked");
    assert_eq!(v.source_mode, "window");
    assert!(!v.source_note.is_empty(), "it has to say why");

    // And it is not asked again. That refusal cannot change while that
    // daemon lives — it comes from a compile-time feature or a mount that
    // failed at startup — so continuing to poll would be two RPCs per
    // second, forever, for an answer that cannot be any different.
    sondear(&host).await;
    sondear(&host).await;
    assert_eq!(
        backend.cursores_pedidos().len(),
        asked,
        "a daemon with no log is not asked again"
    );
}

/// And the LEVEL is not requested from it either: it is the other half of
/// the same rule.
///
/// The window used to request it based on the SOURCE alone, so against a
/// daemon that had already answered `Unsupported`, every level press sent a
/// `log.level` whose answer was already known — one RPC per keystroke,
/// forever. The TUI already required both conditions and said why; now it is
/// the same rule in both.
#[tokio::test]
async fn a_daemon_with_no_log_is_not_asked_for_the_level() {
    let backend = Falso::con(&["a"]);
    backend.log_tail_no_soportado();
    let (host, _anillo) = host_con_backend_y_registro(Arc::clone(&backend)).await;
    // The premise: it already answered it has no ring to serve.
    let v = foto_registro(&host).await;
    assert!(
        !v.sources_available,
        "the daemon already said it has no ring"
    );

    // And the panel's preference is still the opening one ("both"), which is
    // what made the source condition hold on its own.
    for level in ["debug", "trace", "warn"] {
        host.dispatch(UiAction::LogSetLevel {
            level: (*level).to_owned(),
        })
        .await
        .expect("host alive");
    }
    asentar().await;
    assert!(
        backend.log_level_pedidos().is_empty(),
        "a dead RPC per keystroke: {:?}",
        backend.log_level_pedidos()
    );
}

/// With no daemon serving, the source control does not move the
/// PREFERENCE.
///
/// It cannot be seen today — the effective source collapses to "this window"
/// anyway, and the renderer does not even paint the picker — and that is
/// exactly why it slips through: the preference moved behind a reader's back
/// who could not see it move, and reappeared set to something else the first
/// time there was a daemon serving. It is checked through that path: it is
/// pressed with the response held back and released afterward.
#[tokio::test]
async fn with_no_second_source_the_control_does_not_move_the_preference() {
    let mut f = Falso::default();
    f.pon("mem:///casa", [(b"a".to_vec(), false)]);
    let gate = Arc::new(backend_falso::Puerta::default());
    f.puerta_registro = Some(Arc::clone(&gate));
    let backend = Arc::new(f);
    backend.responde_log_tail(vec![linea_wire(10, "info", "norte_core", "del daemon")], 1);
    let (host, _anillo) = host_con_backend_y_registro(Arc::clone(&backend)).await;

    // With the response held back, it is not yet known whether there is a
    // second source.
    let v = foto_registro(&host).await;
    assert!(!v.sources_available, "nobody has answered yet");

    // Two turns of the control: with no guard these would leave the
    // preference on "daemon".
    for _ in 0..2 {
        host.dispatch(UiAction::LogCycleSource)
            .await
            .expect("host alive");
    }
    asentar().await;

    // Now it does answer, and the picker shows up: the preference has to
    // still be the opening one.
    gate.abrir();
    let v = foto_registro(&host).await;
    assert!(v.sources_available, "now it serves its log");
    assert_eq!(
        v.source_mode, "both",
        "the control moved the preference without anyone being able to see it"
    );
}

/// The level is requested FROM THE DAEMON, but the one the header marks is
/// the one being SHOWN — and the daemon's is said separately, as extra
/// capture.
///
/// Both halves are the same trap seen from its two faces. The client does
/// not apply levels: the guard that keeps a password from showing up in
/// there lives in the process that has the ring, so requesting is all that
/// can be done. And what the header marks has to keep being what is shown,
/// because that is what FILTERS the list and what the buttons control:
/// marking the daemon's level there — which is global to its clients, which
/// another one could have raised, and which never lowers — left `trace` on
/// while the panel silently threw away every `debug` line arriving over the
/// wire, and pressing `info` did not move the mark. A control that does not
/// move what it marks reads as broken.
#[tokio::test]
async fn the_daemons_level_is_requested_and_said_separately() {
    let backend = Falso::con(&["a"]);
    // Another client already raised the daemon's ring to `trace`. It is
    // global and only ever rises, so requesting `info` does not lower it: it
    // answers whatever it has.
    backend.responde_log_tail(Vec::new(), 0);
    backend.log_level_contesta("trace");
    let (host, _anillo) = host_con_backend_y_registro(Arc::clone(&backend)).await;
    poner_fuente(&host, "daemon").await;
    host.dispatch(UiAction::LogSetLevel {
        level: "info".to_owned(),
    })
    .await
    .expect("host alive");
    let requested = hasta(&backend, "the level requested from the daemon", |f| {
        let v = f.log_level_pedidos();
        (!v.is_empty()).then_some(v)
    })
    .await;
    assert_eq!(
        requested,
        vec!["info".to_owned()],
        "it IS REQUESTED from the daemon"
    );

    let v = foto_registro(&host).await;
    assert_eq!(v.source_mode, "daemon");
    assert_eq!(
        v.level, "info",
        "the header marks what is SHOWN, which is what filters the list"
    );
    // And the daemon's is not kept silent: it comes out where "more is
    // captured than shown" already lives, and there it DOES say whose ring
    // it is.
    assert_eq!(
        v.capturing,
        norte_i18n::ta_in(
            norte_i18n::Lang::Es,
            "log-capturing-daemon",
            &[("level", "trace")]
        ),
        "the daemon's ring keeps more than this panel shows"
    );
    assert!(
        !v.source_note.is_empty(),
        "and it says WHOSE level that is: it is global to the daemon"
    );
}

/// Polling chains the cursor: the second round requests from where the
/// first ended and does not repeat lines.
#[tokio::test]
async fn polling_chains_the_cursor() {
    let backend = Falso::con(&["a"]);
    backend.responde_log_tail(vec![linea_wire(10, "info", "norte_core", "primera")], 1);
    let (host, _anillo) = host_con_backend_y_registro(Arc::clone(&backend)).await;
    let v = foto_registro(&host).await;
    assert!(v.lines.iter().any(|l| l.message.contains("primera")));

    backend.responde_log_tail(vec![linea_wire(20, "info", "norte_core", "segunda")], 2);
    sondear(&host).await;
    let cursors = backend.cursores_pedidos();
    assert_eq!(
        cursors[0], None,
        "the first round requests \"whatever there is\""
    );
    assert!(
        cursors[1..].iter().all(Option::is_some),
        "no later round asks for \"whatever there is\" again: {cursors:?}"
    );
    assert_eq!(cursors[1], Some(1), "the second one chains where it ended");
    let v = foto_registro(&host).await;
    let texts: Vec<_> = v.lines.iter().map(|l| l.message.as_str()).collect();
    assert_eq!(
        texts.iter().filter(|t| t.contains("primera")).count(),
        1,
        "the first line is not repeated: {texts:?}"
    );
    assert!(texts.iter().any(|t| t.contains("segunda")), "{texts:?}");
}

/// A response still in flight when the panel closes does NOT enter the
/// panel that reopens.
///
/// It is the question that asks itself as soon as the request is
/// asynchronous: between requesting and answering there is room for a close
/// and an open, and a few lines from the previous session landing in the new
/// panel would be history nobody asked for, ahead of the one that was. The
/// opening's EPOCH travels with the request and is what lets it die — the
/// same mechanism that already retires the timer.
#[tokio::test]
async fn a_response_in_flight_does_not_enter_the_reopened_panel() {
    let mut f = Falso::default();
    f.pon("mem:///casa", [(b"a".to_vec(), false)]);
    let gate = Arc::new(backend_falso::Puerta::default());
    f.puerta_registro = Some(Arc::clone(&gate));
    let backend = Arc::new(f);
    backend.responde_log_tail(
        vec![linea_wire(10, "info", "norte_core", "de la apertura vieja")],
        1,
    );
    let (host, _anillo) = host_con_backend_y_registro(Arc::clone(&backend)).await;
    // The first opening's request is still held back: it is closed and
    // reopened underneath it.
    tecla_registro(&host).await;
    tecla_registro(&host).await;
    gate.abrir();

    let v = foto_registro(&host).await;
    assert!(
        !v.lines
            .iter()
            .any(|l| l.message.contains("de la apertura vieja")),
        "the previous opening's response does not enter: {:?}",
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
            skipped_not_ours: 0,
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
            skipped_not_ours: 0,
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
        cola: None,
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
