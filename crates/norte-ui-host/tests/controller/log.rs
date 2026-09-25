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
pub(super) fn with_lines(ring: &norte_config::logring::LogRing, f: impl FnOnce()) {
    use tracing_subscriber::layer::SubscriberExt as _;
    let s = tracing_subscriber::registry().with(norte_config::logring::ring_layer(ring));
    tracing::subscriber::with_default(s, f);
}

/// A host with a log ring mounted and a few lines inside it.
pub(super) async fn host_with_log() -> (UiHost, norte_config::logring::LogRing) {
    host_with_backend_and_log(Fake::con(&["a"])).await
}

/// The same, with a double the test has armed: it is what is needed for the
/// remote half (#328), where what is being tested is what the daemon
/// answers.
pub(super) async fn host_with_backend_and_log(
    backend: Arc<Fake>,
) -> (UiHost, norte_config::logring::LogRing) {
    let ring = norte_config::logring::LogRing::new(64);
    // At DEBUG so the five fit; the panel shows up to INFO when opened, which
    // is what makes the level-filter test interesting.
    ring.set_level(norte_config::logline::LogLevel::Debug);
    let h = host_with_backend_and_ring(backend, Some(ring.clone())).await;
    (h, ring)
}

/// And the same with NO ring in this process: nobody mounted the `tracing`
/// layer.
///
/// It is not a lab case — it is what the window sees when the ring is not
/// installed — and it is the one that decides whether "both" can be
/// announced over a list that is entirely the daemon's.
pub(super) async fn host_with_backend_and_ring(
    backend: Arc<Fake>,
    ring: Option<norte_config::logring::LogRing>,
) -> UiHost {
    let h = UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("orthodox").expect("layout"),
        viewport: (120, 40),
        settings: norte_ui_host::default_settings(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::default_columns(),
        effects: norte_ui_host::commands::Effects::Full,
        log_ring: ring,
    })
    .await
    .expect("starts")
    .0;
    // The starting layout carries no log slot: it opens with its key, the
    // way a person would open it. And this way the test ALSO covers that
    // `layout.log` is bound and reaches the effect.
    key_log(&h).await;
    h
}

/// The key that opens the log — and, pressed again, closes it.
pub(super) async fn key_log(h: &UiHost) {
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
pub(super) fn log(snap: &norte_ui_host::ViewSnapshot) -> &norte_ui_host::dto::LogSlotView {
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
    let (h, ring) = host_with_log().await;
    with_lines(&ring, || {
        tracing::info!(target: "norte_prueba", "una linea de prueba");
    });
    let mut sub = h.subscribe();

    let view = snapshot_until(&h, &mut sub, "the log panel", |f| {
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
    let _ = ring;
}

/// The panel shows the rows the RENDERER says fit, not one.
///
/// The host starts with one — never zero, so a page moves something — and
/// waits to be told the height. While nobody told it, a twelve-row panel
/// painted ONE clipped line and the wheel skipped two per notch: the same bug
/// the TUI fixed by no longer guessing the viewport.
#[tokio::test]
async fn the_log_shows_the_rows_it_is_told_fit() {
    let (h, ring) = host_with_log().await;
    with_lines(&ring, || {
        for i in 0..8 {
            tracing::info!(target: "norte_prueba", n = i, "linea");
        }
    });
    let mut sub = h.subscribe();

    h.dispatch(UiAction::LogSetVisibleRange { rows: 6 })
        .await
        .expect("host alive");
    let view = snapshot_until(&h, &mut sub, "six rows", |f| {
        let l = log(f).clone();
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
    let (h, ring) = host_with_log().await;
    with_lines(&ring, || {
        for i in 0..40 {
            tracing::info!(target: "norte_prueba", n = i, "linea");
        }
    });
    let mut sub = h.subscribe();

    h.dispatch(UiAction::LogSetVisibleRange { rows: u32::MAX })
        .await
        .expect("host alive");
    settle().await;
    let view = snapshot_until(&h, &mut sub, "the clamped log", |f| Some(log(f).clone())).await;
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
    let (h, ring) = host_with_log().await;
    h.dispatch(UiAction::LogSetLevel {
        level: "trace".to_owned(),
    })
    .await
    .expect("host alive");
    settle().await;
    assert_eq!(ring.level(), norte_config::logline::LogLevel::Trace);

    // And while it is open, the panel SAYS more is being captured than it
    // shows: a screenshot saying "info" over a process saving TRACE would be
    // a false answer.
    h.dispatch(UiAction::LogSetLevel {
        level: "info".to_owned(),
    })
    .await
    .expect("host alive");
    let mut sub = h.subscribe();
    let view = snapshot_until(&h, &mut sub, "the capture notice", |f| {
        let l = log(f).clone();
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
    settle().await;
    assert_eq!(
        ring.level(),
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
    let (h, ring) = host_with_log().await;
    ring.set_level(norte_config::logline::LogLevel::Info);

    h.dispatch(UiAction::LogSetLevel {
        level: "debug".to_owned(),
    })
    .await
    .expect("host alive");
    settle().await;
    assert_eq!(
        ring.level(),
        norte_config::logline::LogLevel::Debug,
        "requesting DEBUG makes the ring capture it"
    );

    h.dispatch(UiAction::LogSetLevel {
        level: "error".to_owned(),
    })
    .await
    .expect("host alive");
    settle().await;
    assert_eq!(
        ring.level(),
        norte_config::logline::LogLevel::Debug,
        "lowering what is SHOWN does not stop capturing"
    );
}

/// A level that does not exist is SAID; it does not fall back to `info`.
#[tokio::test]
async fn an_unknown_log_level_does_not_fall_back() {
    let (h, _ring) = host_with_log().await;
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
    let (h, ring) = host_with_log().await;
    with_lines(&ring, || {
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
    let view = snapshot_until(&h, &mut sub, "the filtered log", |f| {
        let l = log(f).clone();
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
    let view = snapshot_until(&h, &mut sub, "the detached log", |f| {
        let l = log(f).clone();
        (!l.following).then_some(l)
    })
    .await;
    assert!(!view.following);

    h.dispatch(UiAction::LogFollow).await.expect("host alive");
    let view = snapshot_until(&h, &mut sub, "the log back to following", |f| {
        let l = log(f).clone();
        l.following.then_some(l)
    })
    .await;
    assert!(view.following, "going back to the end can be requested");
}

// ---------------------------------------------------------------------------
// The log panel ALSO reads the daemon (#328).
// ---------------------------------------------------------------------------

/// A line just as it comes over the wire.
pub(super) fn line_wire(
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
/// `settle` first: the daemon's response comes back to the actor through
/// the SAME mailbox as actions, so once the executor goes still the message
/// is already queued and `snapshot_until`'s `Resync` goes behind it. No clock and
/// no guessing.
pub(super) async fn snapshot_log(h: &UiHost) -> norte_ui_host::dto::LogSlotView {
    // With a real height: the host starts with ONE row — never zero, so a
    // page moves something — and with one row the visible window is the last
    // line, so a mixed list would look like half of what there is.
    h.dispatch(UiAction::LogSetVisibleRange { rows: 20 })
        .await
        .expect("host alive");
    let mut sub = h.subscribe();
    settle().await;
    snapshot_until(h, &mut sub, "the log panel", |f| Some(log(f).clone())).await
}

/// Triggers ONE more round of the 500 ms poll.
///
/// Advancing the clock and not sleeping it: the deadline is REAL — the timer
/// the panel rearms on its own — and that is exactly the tool this file's
/// note on deterministic waits points to for a deadline.
pub(super) async fn probe(h: &UiHost) {
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_millis(600)).await;
    tokio::time::resume();
    settle().await;
    let _ = h;
}

/// Leaves the panel's SOURCE at the one requested.
///
/// Built on the real control, which is ONE single button that cycles through
/// the three (`Both` → `Window` → `Daemon` → `Both`): there is no "set this
/// one" action, and manufacturing one just for the tests would test a path
/// nobody uses.
pub(super) async fn set_source(h: &UiHost, source: &str) {
    // First the daemon's response is let to land: the control does NOT cycle
    // while it is not known there is a second source — moving the preference
    // behind a reader's back who cannot see it move is what got fixed — so
    // pressing it before the first `log.tail` would do nothing.
    settle().await;
    let rounds = match source {
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
    settle().await;
}

/// With a daemon that knows nothing about logging there are no two rings, so
/// there is no picker to show: the panel stays exactly as in #326.
#[tokio::test]
async fn embedded_offers_no_source_picker() {
    let (host, _ring) = host_with_log().await;
    let v = snapshot_log(&host).await;
    assert!(!v.sources_available);
    assert_eq!(v.source_mode, "window");
}

/// With a daemon, the panel brings the lines of BOTH and each one says
/// where it is from.
#[tokio::test]
async fn with_a_daemon_the_two_sources_mix() {
    let backend = Fake::con(&["a"]);
    backend.answers_log_tail(vec![line_wire(20, "info", "norte_core", "del daemon")], 1);
    let (host, ring) = host_with_backend_and_log(Arc::clone(&backend)).await;
    with_lines(&ring, || {
        tracing::info!(target: "norte_prueba", "de la ventana");
    });
    let v = snapshot_log(&host).await;
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
    let backend = Fake::con(&["a"]);
    backend.answers_log_tail(vec![line_wire(20, "info", "norte_core", "del daemon")], 1);
    let host = host_with_backend_and_ring(Arc::clone(&backend), None).await;
    let v = snapshot_log(&host).await;
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
    let backend = Fake::con(&["a"]);
    backend.log_tail_no_supported();
    let (host, _ring) = host_with_backend_and_log(Arc::clone(&backend)).await;
    let v = snapshot_log(&host).await;
    let asked = backend.cursors_requests().len();
    assert!(asked > 0, "it did get asked");
    assert_eq!(v.source_mode, "window");
    assert!(!v.source_note.is_empty(), "it has to say why");

    // And it is not asked again. That refusal cannot change while that
    // daemon lives — it comes from a compile-time feature or a mount that
    // failed at startup — so continuing to poll would be two RPCs per
    // second, forever, for an answer that cannot be any different.
    probe(&host).await;
    probe(&host).await;
    assert_eq!(
        backend.cursors_requests().len(),
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
    let backend = Fake::con(&["a"]);
    backend.log_tail_no_supported();
    let (host, _ring) = host_with_backend_and_log(Arc::clone(&backend)).await;
    // The premise: it already answered it has no ring to serve.
    let v = snapshot_log(&host).await;
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
    settle().await;
    assert!(
        backend.log_level_requests().is_empty(),
        "a dead RPC per keystroke: {:?}",
        backend.log_level_requests()
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
    let mut f = Fake::default();
    f.put("mem:///casa", [(b"a".to_vec(), false)]);
    let gate = Arc::new(backend_fake::Gate::default());
    f.gate_log = Some(Arc::clone(&gate));
    let backend = Arc::new(f);
    backend.answers_log_tail(vec![line_wire(10, "info", "norte_core", "del daemon")], 1);
    let (host, _ring) = host_with_backend_and_log(Arc::clone(&backend)).await;

    // With the response held back, it is not yet known whether there is a
    // second source.
    let v = snapshot_log(&host).await;
    assert!(!v.sources_available, "nobody has answered yet");

    // Two turns of the control: with no guard these would leave the
    // preference on "daemon".
    for _ in 0..2 {
        host.dispatch(UiAction::LogCycleSource)
            .await
            .expect("host alive");
    }
    settle().await;

    // Now it does answer, and the picker shows up: the preference has to
    // still be the opening one.
    gate.open();
    let v = snapshot_log(&host).await;
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
    let backend = Fake::con(&["a"]);
    // Another client already raised the daemon's ring to `trace`. It is
    // global and only ever rises, so requesting `info` does not lower it: it
    // answers whatever it has.
    backend.answers_log_tail(Vec::new(), 0);
    backend.log_level_answers("trace");
    let (host, _ring) = host_with_backend_and_log(Arc::clone(&backend)).await;
    set_source(&host, "daemon").await;
    host.dispatch(UiAction::LogSetLevel {
        level: "info".to_owned(),
    })
    .await
    .expect("host alive");
    let requested = until(&backend, "the level requested from the daemon", |f| {
        let v = f.log_level_requests();
        (!v.is_empty()).then_some(v)
    })
    .await;
    assert_eq!(
        requested,
        vec!["info".to_owned()],
        "it IS REQUESTED from the daemon"
    );

    let v = snapshot_log(&host).await;
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
    let backend = Fake::con(&["a"]);
    backend.answers_log_tail(vec![line_wire(10, "info", "norte_core", "primera")], 1);
    let (host, _ring) = host_with_backend_and_log(Arc::clone(&backend)).await;
    let v = snapshot_log(&host).await;
    assert!(v.lines.iter().any(|l| l.message.contains("primera")));

    backend.answers_log_tail(vec![line_wire(20, "info", "norte_core", "segunda")], 2);
    probe(&host).await;
    let cursors = backend.cursors_requests();
    assert_eq!(
        cursors[0], None,
        "the first round requests \"whatever there is\""
    );
    assert!(
        cursors[1..].iter().all(Option::is_some),
        "no later round asks for \"whatever there is\" again: {cursors:?}"
    );
    assert_eq!(cursors[1], Some(1), "the second one chains where it ended");
    let v = snapshot_log(&host).await;
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
    let mut f = Fake::default();
    f.put("mem:///casa", [(b"a".to_vec(), false)]);
    let gate = Arc::new(backend_fake::Gate::default());
    f.gate_log = Some(Arc::clone(&gate));
    let backend = Arc::new(f);
    backend.answers_log_tail(
        vec![line_wire(10, "info", "norte_core", "de la apertura vieja")],
        1,
    );
    let (host, _ring) = host_with_backend_and_log(Arc::clone(&backend)).await;
    // The first opening's request is still held back: it is closed and
    // reopened underneath it.
    key_log(&host).await;
    key_log(&host).await;
    gate.open();

    let v = snapshot_log(&host).await;
    assert!(
        !v.lines
            .iter()
            .any(|l| l.message.contains("de la apertura vieja")),
        "the previous opening's response does not enter: {:?}",
        v.lines
    );
}

/// Enters `docs`, which is the navigation that triggers the remote listing.
pub(super) async fn enter_docs(h: &UiHost, snap: &norte_ui_host::ViewSnapshot) {
    let docs = listing(snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("the directory is there");
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs.key,
        generation: listing(snap).generation,
    })
    .await
    .expect("host alive");
}

/// Arms the double so the NEXT listing asks for the password (#327).
///
/// After starting the host, not before: the startup listing would carry off
/// the request and the window would be born with the pane in error, which is
/// another case.
pub(super) fn will_ask_for_secret(f: &Fake) {
    *f.asks_secret.lock().expect("pide_secreto") = Some(norte_proto::Error::SecretNeeded {
        conn: "rosetta".to_owned(),
        endpoint: "s3://cubo.example".to_owned(),
    });
}

/// A slot that starts up asking for the password does NOT ask on its own, but
/// SAYS which one, and it can be retried — and the retry does ask.
///
/// This is the case of reopening norte: the previous daemon shut down from
/// inactivity and took the session secret with it, so the pane saved over
/// `s3://…` comes back with `SecretNeeded`. Startup does not open the dialog
/// on purpose — restoring a session is not asking to connect, and a password
/// asked for before the screen exists is the shape ADR 0015 calls phishing —
/// but it also cannot leave a stalled pane without saying what happened to
/// it.
#[tokio::test]
async fn a_slot_that_asks_for_a_secret_says_which_one_and_the_retry_asks() {
    let backend = Arc::new(tree_as_fake());
    will_ask_for_secret(&backend);
    // The STARTUP listing is the one that runs into the error, so the
    // breakdown is armed before building the host.
    let (h, snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    let SlotView::Browser(b) = &snap.slots[0] else {
        panic!("the first slot is a listing");
    };
    let norte_ui_host::dto::SlotState::Error { reason_key, detail } = &b.state else {
        panic!("the slot stays in error, not pretending to be an empty directory");
    };
    assert_eq!(reason_key, "err-secret-needed");
    assert_eq!(
        detail.as_deref(),
        Some("rosetta"),
        "and WHICH ONE: with two remote panes, \"a secret is needed\" is not \
         answerable"
    );
    // No dialog: startup does not ask on its own.
    assert!(
        snap.dialogs.is_empty(),
        "startup does not open the question: the first gesture does"
    );

    // The retry DOES open it, because it is a gesture. The breakdown is
    // rearmed: the secret is still missing — nobody has handed it over — and
    // the double consumes it one at a time.
    will_ask_for_secret(&backend);
    h.dispatch(UiAction::RefreshSlot { slot_id: 1 })
        .await
        .expect("host alive");
    let dialogs = next_dialogs(&mut sub).await;
    assert_eq!(
        dialogs.last().map(|d| d.title_key.as_str()),
        Some("modal-ask-secret-title"),
        "retrying is the gesture that turns the stalled pane into the question"
    );
}

/// #327: the window ASKS for the password instead of painting the error.
///
/// Until now, a `norte-gui` user on a `secret = "prompt"` connection saw the
/// text of `err-secret-needed` — which names an environment variable — and
/// that is where the road ended. The TUI opened a dialog since #325: the same
/// parity gap ADR 0077 exists to not leave open.
#[tokio::test]
async fn the_window_asks_for_the_secret_and_retries_the_navigation() {
    let backend = Arc::new(tree_as_fake());
    let (h, snap) = host_tree(Arc::clone(&backend)).await;
    will_ask_for_secret(&backend);
    let mut sub = h.subscribe();
    // Entering the directory triggers the listing that asks for the secret.
    enter_docs(&h, &snap).await;

    let dialogs = next_dialogs(&mut sub).await;
    let d = dialogs.last().expect("the dialog opened");
    assert_eq!(d.title_key, "modal-ask-secret-title");
    // The question says WHERE the password is going, not just what the entry
    // is called: the name was chosen by a file, and a file gets edited.
    assert_eq!(
        d.destination.as_ref().map(|l| l.text.as_str()),
        Some("s3://cubo.example"),
        "without the destination the question is not answerable"
    );
    assert_eq!(d.subject.as_ref().map(|l| l.text.as_str()), Some("rosetta"));
    assert!(d.input_secret, "the field is a password");
    assert_eq!(d.input.as_deref(), Some(""), "born empty");

    // Typing through the NAME path does nothing to this dialog: the host
    // does not store passwords, and a renderer that sent them that way would
    // be smuggling secret material through a file name.
    let ack = h
        .dispatch(UiAction::DialogInput {
            id: d.id,
            text: "s3cr3t".to_owned(),
        })
        .await
        .expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Modal
        },
        "a password field is not typed through `dialog_input`"
    );

    // Nor through the FORM path (bridge 91), the other place where the host
    // DOES store what gets typed: a password dialog carries no fields, and
    // `touch_dialog_field` checks that before touching anything. Without
    // this test, #327's invariant stayed enforced in two places and tested
    // in one — the old one.
    assert!(d.fields.is_empty(), "a password is not a form");
    let ack = h
        .dispatch(UiAction::DialogField {
            id: d.id,
            field: "name".to_owned(),
            value: norte_ui_host::action::DialogFieldValue::Text {
                text: "s3cr3t".to_owned(),
            },
        })
        .await
        .expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Modal
        },
        "a password field is not typed through `dialog_field` either"
    );

    // Confirming hands over the secret AS IS and retries THAT navigation. It
    // travels WITH the response: it crosses once, at the instant it is
    // decided.
    h.dispatch(UiAction::Dialog {
        id: d.id,
        choice: "confirm".to_owned(),
        secret: Some("s3cr3t".to_owned()),
    })
    .await
    .expect("host alive");

    let dados = annotated(&backend, "the secret handed over", 1, |f| {
        f.secrets_dados.lock().expect("secretos_dados").clone()
    })
    .await;
    assert_eq!(
        dados[0],
        ("rosetta".to_owned(), "s3cr3t".to_owned()),
        "arrives whole, to the connection that asked for it"
    );

    // And the pane ends up WHERE it was going: handing over the password
    // without resuming the navigation would leave the reader with the
    // secret given and the pane standing still.
    let dir = snapshot_until(&h, &mut sub, "the pane entered", |f| {
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

/// Confirming with an EMPTY field is inert: it neither hands anything over
/// nor closes.
///
/// Handing over the empty string reproduces #320 — an empty secret makes the
/// connection authenticate with the ambient string, that is, with an
/// identity nobody asked for — and closing would turn a finger getting ahead
/// of itself into an abandoned navigation.
#[tokio::test]
async fn confirming_without_typing_anything_neither_hands_over_nor_closes() {
    let backend = Arc::new(tree_as_fake());
    let (h, snap) = host_tree(Arc::clone(&backend)).await;
    will_ask_for_secret(&backend);
    let mut sub = h.subscribe();
    enter_docs(&h, &snap).await;
    let dialogs = next_dialogs(&mut sub).await;
    let id = dialogs.last().expect("the dialog opened").id;

    // No prior ack: the reader's navigation opened it, so the first response
    // is already a response. And with the field empty, it does nothing.
    let ack = h
        .dispatch(UiAction::Dialog {
            id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "host-secret-empty".to_owned()
        },
        "confirming an empty password field is inert"
    );

    settle().await;
    assert!(
        backend
            .secrets_dados
            .lock()
            .expect("secretos_dados")
            .is_empty(),
        "NOTHING was handed over: the empty string is #320"
    );
    // And the dialog stays up front: answering with a `Stale` would mean it
    // closed.
    let ack = h
        .dispatch(UiAction::Dialog {
            id,
            choice: "cancel".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "the dialog was still open: {ack:?}"
    );
}

/// A password that does not fit is REJECTED, not truncated.
///
/// Truncating was worse than the cap: handing over the first 256 characters
/// of a longer passphrase fails authentication without saying why, and the
/// reader cannot suspect it because the field is masked.
#[tokio::test]
async fn a_password_that_does_not_fit_is_rejected() {
    let backend = Arc::new(tree_as_fake());
    let (h, snap) = host_tree(Arc::clone(&backend)).await;
    will_ask_for_secret(&backend);
    let mut sub = h.subscribe();
    enter_docs(&h, &snap).await;
    let dialogs = next_dialogs(&mut sub).await;
    let id = dialogs.last().expect("the dialog opened").id;

    let ack = h
        .dispatch(UiAction::Dialog {
            id,
            choice: "confirm".to_owned(),
            secret: Some("x".repeat(257)),
        })
        .await
        .expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "host-secret-too-long".to_owned()
        }
    );
    settle().await;
    assert!(
        backend
            .secrets_dados
            .lock()
            .expect("secretos_dados")
            .is_empty(),
        "no password was handed over halfway"
    );
}

/// Two panes on the SAME connection do not stack two identical questions.
///
/// Each one carried its own empty field, and under enough of them the
/// cap-driven eviction of the stack sweeps away unrecognized agent
/// approvals, which is the first thing it sacrifices.
#[tokio::test]
async fn two_listings_of_the_same_connection_do_not_stack_two_questions() {
    let backend = Arc::new(tree_as_fake());
    let (h, snap) = host_tree(Arc::clone(&backend)).await;
    will_ask_for_secret(&backend);
    let mut sub = h.subscribe();
    enter_docs(&h, &snap).await;
    let dialogs = next_dialogs(&mut sub).await;
    assert_eq!(dialogs.len(), 1);

    // Another navigation to the same place, and again without a secret.
    will_ask_for_secret(&backend);
    h.dispatch(UiAction::Parent { slot_id: 1 })
        .await
        .expect("host alive");
    settle().await;
    let snapshot = snapshot_until(&h, &mut sub, "the stack is stable", |f| {
        Some(f.dialogs.len())
    })
    .await;
    assert_eq!(
        snapshot, 1,
        "one question per connection, not one per listing"
    );
}

/// Closing the dialog abandons the navigation, like TOFU: nothing is handed
/// over and the slot stays with the error it already knew how to explain.
#[tokio::test]
async fn canceling_the_secret_abandons_the_navigation() {
    let backend = Arc::new(tree_as_fake());
    let (h, snap) = host_tree(Arc::clone(&backend)).await;
    will_ask_for_secret(&backend);
    let mut sub = h.subscribe();
    enter_docs(&h, &snap).await;
    let dialogs = next_dialogs(&mut sub).await;
    let id = dialogs.last().expect("the dialog opened").id;

    h.dispatch(UiAction::Dialog {
        id,
        choice: "cancel".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    settle().await;
    assert!(
        backend
            .secrets_dados
            .lock()
            .expect("secretos_dados")
            .is_empty(),
        "canceling hands nothing over"
    );
    let motivo = snapshot_until(&h, &mut sub, "the slot in error", |f| {
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
        "behind the dialog stays the screen that already knew how to explain itself"
    );
}

/// #322: a connection that does NOT open says WHY, with the concrete phrase.
///
/// Without this, the failure arrived as the error's category —
/// `PermissionDenied` — which does not distinguish an empty secret from a
/// wrong key or from a bucket with no permissions. The exact phrase stayed
/// in the daemon's log.
///
/// And it arrives as an EPHEMERAL notice, not a banner: degradation
/// describes a session that stays open while it is being watched; this, an
/// attempt that ended.
#[tokio::test]
async fn a_connection_that_fails_says_why() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.failed.lock().expect("fallidas") = Some(rx);
    let (h, _snap) = host_tree(Arc::new(fake)).await;
    let mut sub = h.subscribe();
    tx.send(norte_proto::methods::ConnectionFailed {
        conn: Some("rosetta".to_owned()),
        scheme: "s3".to_owned(),
        host: "cubo.example".to_owned(),
        reason: "secret-empty".to_owned(),
        detail: Some("the secret for «rosetta» is defined but EMPTY".to_owned()),
    })
    .expect("the host is listening");

    // On the SCREEN, not on the bridge's envelope. The renderer only
    // attends to `Notice`s of class `fatal`, and its status text comes from
    // `status.message`: a test that asserted on the notice went green with
    // the window painting nothing, which is exactly what happened.
    let detail = snapshot_until(&h, &mut sub, "the failure in the bar", |f| {
        f.status
            .message
            .clone()
            .filter(|m| m.contains("cubo.example"))
    })
    .await;
    assert!(
        detail.contains("cubo.example"),
        "the notice names the machine that was not reached: {detail}"
    );
    assert!(
        detail.contains("rosetta"),
        "and the name from connections.toml, the one the human wrote: {detail}"
    );
    assert!(
        detail.contains(&norte_i18n::t_in(
            norte_i18n::Lang::Es,
            "failed-reason-secret-empty"
        )),
        "and the translated REASON, which is what #322 exists to make cross over: {detail}"
    );
    assert!(
        !detail.contains("s3://"),
        "the authority is labeled, never as a URL: {detail}"
    );

    // And the notice travels ALSO, with the same line: a frontend that does
    // attend to `Notice`s does not depend on having read the snapshot.
    let mut sub2 = h.subscribe();
    tx.send(norte_proto::methods::ConnectionFailed {
        conn: None,
        scheme: "s3".to_owned(),
        host: "otro.example".to_owned(),
        reason: "auth-rejected".to_owned(),
        detail: None,
    })
    .expect("the host is listening");
    let notice = snapshot_until_notice(&mut sub2, "status-connection-failed").await;
    assert!(notice.contains("otro.example"), "{notice}");
}

/// The vocabulary of failures can also GROW, and an unknown one cannot
/// inherit the phrase from the one next to it: it leans on `detail`, as the
/// proto requires.
#[tokio::test]
async fn a_failure_with_an_unknown_reason_leans_on_the_detail() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.failed.lock().expect("fallidas") = Some(rx);
    let (h, _snap) = host_tree(Arc::new(fake)).await;
    let mut sub = h.subscribe();
    tx.send(norte_proto::methods::ConnectionFailed {
        conn: None,
        scheme: "sftp".to_owned(),
        host: "maquina.example".to_owned(),
        reason: "something-that-did-not-exist".to_owned(),
        detail: Some("the server asked for a method norte does not have".to_owned()),
    })
    .expect("the host is listening");

    let detail = snapshot_until(&h, &mut sub, "the unknown failure in the bar", |f| {
        f.status
            .message
            .clone()
            .filter(|m| m.contains("maquina.example"))
    })
    .await;
    assert!(
        detail.contains("the server asked for a method norte does not have"),
        "with no known reason, the detail is the only thing that orients: {detail}"
    );
    assert!(
        !detail.contains(&norte_i18n::t_in(
            norte_i18n::Lang::Es,
            "failed-reason-auth-rejected"
        )),
        "an unknown reason does not inherit another one's phrase: {detail}"
    );
}

/// Waits for the next notice with this key and returns its detail.
pub(super) async fn snapshot_until_notice(
    sub: &mut norte_ui_host::controller::UiSubscription,
    map_key: &str,
) -> String {
    for _ in 0..40 {
        match tokio::time::timeout(WAIT_MAX, sub.recv())
            .await
            .expect("arrives")
            .expect("the host is still alive")
        {
            Update::Message(m) => {
                if let UiUpdate::Notice(UiNotice::Message { key, detail }) = &m.payload
                    && key == map_key
                {
                    return detail.clone().unwrap_or_default();
                }
            }
            Update::Lagged => {}
        }
    }
    panic!("the notice {map_key} never arrived");
}

/// **A reason this binary does not know is not read as «FTP in the clear»**
/// (#279). The wire's vocabulary can grow, and before this a newer daemon
/// reporting a NEW degradation produced exactly the same phrase: a security
/// notice asserting a cause nobody had said.
#[tokio::test]
async fn an_unknown_reason_is_not_painted_as_the_known_one() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.degraded.lock().expect("degradadas") = Some(rx);
    let (h, _snap) = host_tree(Arc::new(fake)).await;
    let mut sub = h.subscribe();
    tx.send(norte_proto::methods::ConnectionDegraded {
        scheme: "sftp".to_owned(),
        host: "archivo.example".to_owned(),
        reason: "something-that-did-not-exist".to_owned(),
        detail: Some("el servidor negoció un perfil antiguo".to_owned()),
    })
    .expect("the host is listening");

    let banners = next_banners(&mut sub).await;
    let subject = banners
        .iter()
        .find_map(|b| b.subject.as_ref())
        .expect("the notice names the connection");
    assert_eq!(
        subject.reason,
        norte_i18n::t_in(norte_i18n::Lang::Es, "degraded-reason-unknown"),
        "an unknown reason says so: {subject:?}"
    );
    assert_ne!(
        subject.reason,
        norte_i18n::t_in(norte_i18n::Lang::Es, "degraded-reason-ftp-plaintext"),
    );
    assert_eq!(
        subject.detail.as_deref(),
        Some("el servidor negoció un perfil antiguo"),
        "and it leans on `detail`, which is what the proto asks for"
    );
}

/// The daemon that warns it is STOPPING says so, and says so persistently:
/// «reconnecting…» over a daemon that does not come back is a false wait.
#[tokio::test]
async fn a_daemon_that_stops_says_so() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.eventos.lock().expect("eventos") = Some(rx);
    let (h, _snap) = host_tree(Arc::new(fake)).await;
    let mut sub = h.subscribe();
    tx.send(norte_client::ConnEvent::GoingAway { reconnect: false })
        .expect("the host is listening");

    let banners = next_banners(&mut sub).await;
    assert_eq!(
        banners.iter().map(|b| b.text.clone()).collect::<Vec<_>>(),
        vec![norte_i18n::t_in(
            norte_i18n::Lang::Es,
            "msg-daemon-stopping"
        )],
    );
}

/// A handoff is NOT a stop, and it is said differently: one comes back and
/// the other does not.
#[tokio::test]
async fn a_handoff_is_not_read_as_a_stop() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.eventos.lock().expect("eventos") = Some(rx);
    let (h, _snap) = host_tree(Arc::new(fake)).await;
    let mut sub = h.subscribe();
    tx.send(norte_client::ConnEvent::GoingAway { reconnect: true })
        .expect("the host is listening");
    let banners = next_banners(&mut sub).await;
    assert_eq!(
        banners.iter().map(|b| b.text.clone()).collect::<Vec<_>>(),
        vec![norte_i18n::t_in(
            norte_i18n::Lang::Es,
            "msg-daemon-handover"
        )],
    );

    // And when it comes back, the notice turns off: a notice that does not
    // know how to become "it's back now" lies as soon as the daemon
    // reappears.
    tx.send(norte_client::ConnEvent::Restored)
        .expect("the host is listening");
    for _ in 0..40 {
        if let Ok(Some(Update::Message(m))) = tokio::time::timeout(WAIT_MAX, sub.recv()).await
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
    panic!("the daemon's notice did not turn off when it came back");
}

/// A mutation the daemon REJECTS for failing to open the journal leaves a
/// persistent notice: "it is not being logged" is a fact about the whole
/// session, and hard rule 4 says no journal means no mutation.
#[tokio::test]
async fn a_mutation_without_a_journal_leaves_a_notice() {
    let fake = tree_as_fake();
    *fake.error_on_delete.lock().expect("error") = Some(norte_proto::Error::JournalUnavailable);
    let backend = Arc::new(fake);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F8")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");

    let banners = next_banners(&mut sub).await;
    assert_eq!(
        banners.iter().map(|b| b.text.clone()).collect::<Vec<_>>(),
        vec![norte_i18n::t_in(
            norte_i18n::Lang::Es,
            "status-journal-refused"
        )],
    );
}

/// An approval that arrives TWICE does not open two dialogs.
///
/// It is not hypothetical: the SDK resyncs `policy.pending` on every
/// reconnection, so an approval that is still alive comes back over the
/// channel. Two dialogs for the same decision are two responses, and the
/// second one lands on an `approval_id` the daemon already closed.
#[tokio::test]
async fn a_repeated_approval_does_not_open_two_dialogs() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.approvals.lock().expect("aprobaciones") = Some(rx);
    let (host, _snap) = host_tree(Arc::new(fake)).await;
    let mut sub = host.subscribe();

    let request = |ttl: u64| norte_proto::methods::PolicyApprovalRequired {
        approval_id: 5,
        session: Some("agente-1".to_owned()),
        op: "delete".to_owned(),
        paths: vec!["mem:///casa/x".to_owned()],
        paths_total: 1,
        ttl_ms: ttl,
        detail: norte_proto::methods::ApprovalDetail::default(),
    };
    tx.send(request(30_000)).expect("the host is listening");
    assert_eq!(next_dialogs(&mut sub).await.len(), 1);
    // The same one, rebuilt by the resync: no TTL, because `policy.pending`
    // does not carry it.
    tx.send(request(0)).expect("the host is listening");
    settle().await;
    assert!(!was_dialogs(&mut sub).await, "the repeat opens nothing new");
}

/// An approval EXPIRES: the daemon stops accepting it, so its dialog closes
/// on its own and says so.
///
/// A dialog that stays up front after the TTL invites approving into the
/// void: approve gets pressed, the daemon answers that the id no longer
/// exists, and the agent has been denied for a while. Worse still if
/// meanwhile the human believed they had authorized it.
#[tokio::test]
async fn an_approval_expires_and_its_dialog_closes() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.approvals.lock().expect("aprobaciones") = Some(rx);
    let (host, _snap) = host_tree(Arc::new(fake)).await;
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
    .expect("the host is listening");
    let open_ones = next_dialogs(&mut sub).await;
    assert_eq!(open_ones.len(), 1);

    let empty = next_dialogs(&mut sub).await;
    assert!(empty.is_empty(), "the dialog closed on its own: {empty:?}");
}

/// An undo that finishes ASKS FOR its report and shows it.
///
/// The Task's outcome says whether the undo ran; what did NOT come back is
/// said only by the report, and an undo that stopped halfway leaves the tree
/// in a state nobody else is going to account for.
#[tokio::test]
async fn a_finished_undo_asks_for_its_report_and_says_what_did_not_come_back() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.foreign.lock().expect("ajenas") = Some(rx);
    *fake.report_undo.lock().expect("informe undo") =
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
    let backend = Arc::new(fake);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let p = inject_task_for(&tx, 51, norte_proto::TaskKind::Undo);
    next_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    let dialogs = next_dialogs(&mut sub).await;
    assert_eq!(
        *backend.informes_undo_requests.lock().expect("pedidos"),
        vec![51]
    );
    let body: String = dialogs[0]
        .body
        .iter()
        .map(|l| l.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        body.contains("42"),
        "cites the entry where it stopped: {body}"
    );
    assert_eq!(dialogs[0].title_key, "modal-undo-report-title");
}

/// #250 — an archiving that COMPLETES asks for its report and says what it
/// saved that means something else outside.
#[tokio::test]
async fn a_finished_archiving_says_the_names_that_mean_something_else() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.foreign.lock().expect("ajenas") = Some(rx);
    *fake.report_pack.lock().expect("informe pack") =
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
    let backend = Arc::new(fake);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let p = inject_task_for(&tx, 77, norte_proto::TaskKind::Pack);
    next_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    for _ in 0..40 {
        if let Ok(Some(Update::Message(m))) = tokio::time::timeout(WAIT_MAX, sub.recv()).await
            && let UiUpdate::Patch(p) = &m.payload
            && let Some(norte_ui_host::dto::ViewChange::Status(s)) = p
                .changes
                .iter()
                .find(|c| matches!(c, norte_ui_host::dto::ViewChange::Status(_)))
            && s.message.as_deref().is_some_and(|t| t.contains('1'))
        {
            assert_eq!(
                *backend.informes_pack_requests.lock().expect("pedidos"),
                vec![77]
            );
            return;
        }
    }
    panic!("an archiving with a hostile name inside said nothing");
}

/// And a CANCELLED archiving says nothing, because there is no archive to
/// speak of (a `protocol-guardian` finding).
///
/// The report exists just the same — it is computed before the first byte is
/// written — and cancellation leaves the destination CLEAN. Painting it
/// would say "archived, but…" about something nobody archived, and it would
/// also make the window say something the TUI does not say (ADR 0077).
#[tokio::test]
async fn a_cancelled_archiving_does_not_warn_about_a_file_that_does_not_exist() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.foreign.lock().expect("ajenas") = Some(rx);
    *fake.report_pack.lock().expect("informe pack") =
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
    let backend = Arc::new(fake);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let p = inject_task_for(&tx, 78, norte_proto::TaskKind::Pack);
    next_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Cancelled);

    // What is asserted is that it does NOT ask for it: everything the task's
    // outcome might have queued is left to run, and is checked afterward.
    settle().await;
    assert!(
        backend
            .informes_pack_requests
            .lock()
            .expect("pedidos")
            .is_empty(),
        "a cancelled archiving has no archive to warn about"
    );
}

/// A clean undo does not interrupt: the board says so and that is it.
#[tokio::test]
async fn a_clean_undo_opens_nothing() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.foreign.lock().expect("ajenas") = Some(rx);
    *fake.report_undo.lock().expect("informe undo") =
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
    let (h, _snap) = host_tree(Arc::new(fake)).await;
    let mut sub = h.subscribe();
    let p = inject_task_for(&tx, 52, norte_proto::TaskKind::Undo);
    next_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    let detail = task_detail(&mut sub).await;
    assert!(detail.contains('4'), "the board says how many: {detail}");
    assert!(!was_dialogs(&mut sub).await);
}

/// The "no journal" notice TURNS OFF when the daemon goes back to accepting
/// a mutation.
///
/// An indicator that does not know how to become "it's fine now" lies about
/// the one thing it describes for the whole session, and it is the same
/// lesson the TUI learned in #179: the ownership window over `journal.db`
/// reopens on its own once the passing occupant releases it. There is no
/// notification announcing it here, so the test there is to have is a
/// mutation the daemon ACCEPTS.
#[tokio::test]
async fn the_journal_notice_turns_off_once_a_mutation_is_accepted_again() {
    let fake = tree_as_fake();
    *fake.error_on_delete.lock().expect("error") = Some(norte_proto::Error::JournalUnavailable);
    let backend = Arc::new(fake);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // A delete rejected by the journal turns the notice on.
    h.dispatch(press("F8")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    assert!(!next_banners(&mut sub).await.is_empty());

    // The journal gets fixed: the next mutation goes through.
    *backend.error_on_delete.lock().expect("error") = None;
    h.dispatch(press("F8")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");

    for _ in 0..40 {
        if let Ok(Some(Update::Message(m))) = tokio::time::timeout(WAIT_MAX, sub.recv()).await
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
    panic!("the journal notice did not turn off once a mutation was accepted");
}

/// The report of a batch with a HOSTILE name inside does not paint it raw,
/// and it says it masked it.
///
/// The name shown now is the only actionable thing in the report, so it is
/// exactly where a name with bidi overrides would make the reader go
/// looking for a different file.
#[tokio::test]
async fn a_batchs_report_masks_the_name_and_says_so() {
    let hostile_bytes = hostile("rtl_override");
    let name = String::from_utf8(hostile_bytes).expect("the fixture is UTF-8");
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.foreign.lock().expect("ajenas") = Some(rx);
    *fake.report.lock().expect("informe") = Some(norte_proto::methods::FsRenameBatchReportResult {
        applied: 1,
        rolled_back: 0,
        failed_pair: Some(0),
        stuck: Some(norte_proto::methods::RenameStuckStep {
            from: VPath::parse("mem:///casa/antes.txt").expect("vpath"),
            to: VPath::parse(&format!("mem:///casa/{name}")).expect("vpath"),
            pair_index: 0,
            error: norte_proto::Error::Io { retryable: false },
            journalled: false,
            still_applied: 1,
        }),
        uncertain: None,
        compensations_lost: 0,
    });
    let (h, _snap) = host_tree(Arc::new(fake)).await;
    let mut sub = h.subscribe();
    let p = inject_task_for(&tx, 61, norte_proto::TaskKind::RenameBatch);
    next_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    let dialogs = next_dialogs(&mut sub).await;
    assert!(
        dialogs[0].body.iter().all(|l| !l.text.contains('\u{202E}')),
        "it is not painted raw: {:?}",
        dialogs[0].body
    );
    assert!(
        dialogs[0].body.iter().any(|l| l.hostile),
        "and it SAYS that what is painted is not what there is: {:?}",
        dialogs[0].body
    );
}

/// A reconnection that RE-ANNOUNCES an already-finished batch does not erase
/// its report.
///
/// The SDK re-announces tasks on reconnecting, and the log projects the view
/// again from the progress — which knows nothing about the report. Without
/// this, the only signal that the directory was left halfway would
/// disappear from the board exactly when the connection recovers, which is
/// when the reader looks at it again.
#[tokio::test]
async fn a_reannouncement_does_not_erase_the_batchs_report() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.foreign.lock().expect("ajenas") = Some(rx);
    *fake.report.lock().expect("informe") = Some(report_clean(2));
    let backend = Arc::new(fake);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let p = inject_task_for(&tx, 71, norte_proto::TaskKind::RenameBatch);
    next_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    let detail = task_detail(&mut sub).await;

    // The same task, re-announced over the "others'" channel the way a
    // reconnection would: already terminal.
    let p2 = inject_task_for(&tx, 71, norte_proto::TaskKind::RenameBatch);
    p2.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    let tasks = next_tasks(&mut sub).await;
    let t = tasks.iter().find(|t| t.task_id == 71).expect("still there");
    assert_eq!(t.detail.as_deref(), Some(detail.as_str()), "{t:?}");
    assert_eq!(
        backend.informes_requests.lock().expect("pedidos").len(),
        1,
        "and it is not asked for again"
    );
}

/// An approval that does NOT reach the daemon is said.
///
/// `policy.decide` is sent and forgotten, so if the daemon went down between
/// the question and the yes, the window would take an operation for
/// authorized that is going to end up denied by silence. On a security
/// surface, "I said it" and "it arrived" are not the same thing.
#[tokio::test]
async fn an_approval_that_does_not_reach_the_daemon_is_said() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.approvals.lock().expect("aprobaciones") = Some(rx);
    *fake.error_on_decide.lock().expect("error") =
        Some(norte_proto::Error::ProviderUnavailable { retryable: true });
    let (host, _snap) = host_tree(Arc::new(fake)).await;
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
    .expect("the host is listening");
    let id = next_dialogs(&mut sub).await[0].id;
    // Twice: the first only acknowledges the surface, which opened on its
    // own.
    for _ in 0..2 {
        host.dispatch(UiAction::Dialog {
            id,
            choice: "approve".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
    }

    for _ in 0..40 {
        if next_notice(&mut sub).await == "msg-approval-not-delivered" {
            return;
        }
    }
    panic!("nobody said the approval did not arrive");
}

/// With the board CAPPED, the row that is visible gets cancelled.
///
/// The board crosses the bridge capped at `MAX_TASKS`, and the cursor is an
/// index. While the cap and the cursor counted over different lists, with
/// more than 256 tasks — mark three thousand files and press F5, and the
/// eviction only takes the FINISHED ones — the highlighted row and the task
/// that was stopped were two different tasks.
#[tokio::test]
async fn with_the_board_capped_the_visible_row_is_the_one_cancelled() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.foreign.lock().expect("ajenas") = Some(rx);
    let (h, _snap) = host_con_layout(Arc::new(fake), "full", (200, 60)).await;
    let canceled = Arc::new(std::sync::Mutex::new(Vec::new()));
    let max = norte_ui_host::bridge::MAX_TASKS;
    let total = max + 5;
    let mut vivas = Vec::new();
    for i in 0..total {
        vivas.push(inject_task(&tx, 1000 + i as u64, &canceled));
    }
    // Subscribes AFTER inserting them: two hundred sixty-one insertions
    // produce more patches than fit to read, and falling behind is not what
    // this test measures. The snapshot the resync asks for brings the whole
    // board.
    settle().await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    assert_eq!(snapshot.tasks.len(), max, "the board is capped");
    let first_painted = snapshot.tasks[0].task_id;

    h.dispatch(UiAction::FocusSlot { slot_id: 7 })
        .await
        .expect("host alive");
    // Cursor on the first PAINTED row (all the way up).
    h.dispatch(press("Home")).await.expect("host alive");
    h.dispatch(key_mod("k", true, false))
        .await
        .expect("host alive");
    assert_eq!(
        *canceled.lock().expect("canceladas"),
        vec![first_painted],
        "the one on the highlighted row is cancelled, not one off screen"
    );
    drop(vivas);
}

/// A batch that IS BORN terminal asks for its report just the same.
///
/// The daemon can complete it before the call returns; then the watch is
/// already resolved, `progress` is never called even once, and the only
/// signal that the directory was left halfway was never asked for — right
/// in the fast batches, which is where the outcome most looks like
/// everything went fine.
#[tokio::test]
async fn a_batch_born_terminal_asks_for_its_report() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.foreign.lock().expect("ajenas") = Some(rx);
    *fake.report.lock().expect("informe") = Some(report_clean(2));
    let backend = Arc::new(fake);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // Born COMPLETED: the sender is dropped right after, as the SDK does
    // with a task that already arrived terminal.
    let progress = norte_proto::TaskProgress {
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
    let (ptx, prx) = tokio::sync::watch::channel(progress);
    drop(ptx);
    tx.send(norte_ui_host::backend::HostTask {
        id: norte_proto::TaskId::new(81),
        progress: prx,
        cancel: Arc::new(|| {}),
        pause: None,
        cola: None,
        foreign: false,
    })
    .expect("the host is listening");

    let detail = task_detail(&mut sub).await;
    assert!(
        detail.contains('2'),
        "the report reached the board: {detail}"
    );
    assert_eq!(
        *backend.informes_requests.lock().expect("pedidos"),
        vec![81]
    );
}

/// A CLICK on a just-opened approval does not approve it.
///
/// The dialog is painted in the same place as the previous one and with the
/// same first option, so a click already in motion toward "Confirm" would
/// land on the "Approve" of an agent approval that had just arrived. The
/// rule "it opens on its own, the first response only acknowledges" was
/// keyboard-only, and the mouse is this surface's primary input.
#[tokio::test]
async fn a_click_on_a_just_opened_approval_does_not_approve_it() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.approvals.lock().expect("aprobaciones") = Some(rx);
    let backend = Arc::new(fake);
    let (host, _snap) = host_tree(Arc::clone(&backend)).await;
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
    .expect("the host is listening");
    let id = next_dialogs(&mut sub).await[0].id;

    host.dispatch(UiAction::Dialog {
        id,
        choice: "approve".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    settle().await;
    assert!(
        backend.decisiones.lock().expect("decisiones").is_empty(),
        "the first click only acknowledges"
    );

    // The second one does approve: the question has already been seen.
    host.dispatch(UiAction::Dialog {
        id,
        choice: "approve".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    until(&backend, "the decision sent", |f| {
        (!f.decisiones.lock().expect("decisiones").is_empty()).then_some(())
    })
    .await;
    assert_eq!(
        backend.decisiones.lock().expect("decisiones").clone(),
        vec![(21, true)]
    );
}

/// A path the daemon ALREADY redacted is marked.
///
/// An approval's paths arrive as text passed through the daemon's lossy
/// conversion: the controls, the bidi overrides and the invalid bytes are
/// already U+FFFD. Computing the mark by comparing against that text gave
/// `false` in exactly the most dangerous class, and inconsistently on top of
/// it — a `zwsp`, which the lossy conversion does not touch, DID turn it on.
#[tokio::test]
async fn a_path_already_redacted_by_the_daemon_is_marked() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.approvals.lock().expect("aprobaciones") = Some(rx);
    let (host, _snap) = host_tree(Arc::new(fake)).await;
    let mut sub = host.subscribe();

    // Exactly as the daemon sends it: `display_lossy` has already replaced
    // the override.
    tx.send(norte_proto::methods::PolicyApprovalRequired {
        approval_id: 22,
        session: None,
        op: "delete".to_owned(),
        paths: vec!["mem:///casa/factura\u{FFFD}.pdf".to_owned()],
        paths_total: 1,
        ttl_ms: 30_000,
        detail: norte_proto::methods::ApprovalDetail::default(),
    })
    .expect("the host is listening");

    let d = next_dialogs(&mut sub).await;
    assert!(
        d[0].body.iter().any(|l| l.hostile),
        "what is read is not what there is, and it is said: {:?}",
        d[0].body
    );
}

/// A CLEAN path that is longer than the bridge's cap is marked by the
/// truncation.
///
/// The truncation attaches an ellipsis AFTER `path_display`'s verdict, and
/// `…` is a legal character in a name: without a mark, the reader cannot
/// tell "that is its name" from "this got cut". And in a batch's report,
/// that name is the only actionable thing there is.
#[tokio::test]
async fn a_clean_but_truncated_path_is_marked() {
    let long: String = std::iter::repeat_n("segmento_larguisimo_pero_limpio", 200)
        .collect::<Vec<_>>()
        .join("/");
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.foreign.lock().expect("ajenas") = Some(rx);
    *fake.report.lock().expect("informe") = Some(norte_proto::methods::FsRenameBatchReportResult {
        applied: 1,
        rolled_back: 0,
        failed_pair: Some(0),
        stuck: Some(norte_proto::methods::RenameStuckStep {
            from: VPath::parse("mem:///casa/antes.txt").expect("vpath"),
            to: VPath::parse(&format!("mem:///casa/{long}")).expect("vpath"),
            pair_index: 0,
            error: norte_proto::Error::Io { retryable: false },
            journalled: true,
            still_applied: 1,
        }),
        uncertain: None,
        compensations_lost: 0,
    });
    let (h, _snap) = host_tree(Arc::new(fake)).await;
    let mut sub = h.subscribe();
    let p = inject_task_for(&tx, 91, norte_proto::TaskKind::RenameBatch);
    next_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    let d = next_dialogs(&mut sub).await;
    assert!(
        d[0].body.iter().any(|l| l.hostile),
        "the truncation also alters what is painted: {:?}",
        d[0].body
    );
}

/// After a daemon HANDOFF, a task with the same id inherits nothing from the
/// previous one.
///
/// Ids are handed out by one process's scheduler and start at 1 on every
/// startup, so the new daemon hands out the SAME numbers. New task 3 was
/// inheriting from the old one that its report had already been asked for —
/// and then it was never asked for, which is losing the only signal that a
/// directory was left halfway.
#[tokio::test]
async fn after_a_handoff_a_repeated_id_inherits_nothing() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.foreign.lock().expect("ajenas") = Some(rx);
    let (evtx, evrx) = tokio::sync::mpsc::unbounded_channel();
    *fake.eventos.lock().expect("eventos") = Some(evrx);
    *fake.report.lock().expect("informe") = Some(report_clean(1));
    let backend = Arc::new(fake);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    let p = inject_task_for(&tx, 3, norte_proto::TaskKind::RenameBatch);
    next_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    task_detail(&mut sub).await;
    assert_eq!(backend.informes_requests.lock().expect("pedidos").len(), 1);

    // Handoff: it leaves and comes back. On the other end, a different
    // daemon.
    evtx.send(norte_client::ConnEvent::GoingAway { reconnect: true })
        .expect("the host is listening");
    evtx.send(norte_client::ConnEvent::Restored)
        .expect("the host is listening");
    // The handoff is processed by the actor: it is let run before injecting
    // the new daemon's task, or the race would be with the reconnected one.
    settle().await;

    // Its first task is also 3, and also a batch.
    let p2 = inject_task_for(&tx, 3, norte_proto::TaskKind::RenameBatch);
    p2.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    annotated(&backend, "the NEW task's report", 2, |f| {
        f.informes_requests.lock().expect("pedidos").clone()
    })
    .await;
}

/// The dialog stack has a ceiling, and losing one is SAID.
///
/// Since the wire feeds it — one report per foreign batch left halfway — a
/// ceiling-less stack is a channel for free-growing memory, and every dialog
/// patch clones the whole stack.
#[tokio::test]
async fn the_dialog_stack_has_a_ceiling() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.foreign.lock().expect("ajenas") = Some(rx);
    *fake.report.lock().expect("informe") = Some(norte_proto::methods::FsRenameBatchReportResult {
        applied: 1,
        rolled_back: 1,
        failed_pair: Some(0),
        stuck: None,
        uncertain: None,
        compensations_lost: 0,
    });
    let (h, _snap) = host_tree(Arc::new(fake)).await;
    let mut sub = h.subscribe();

    let cap = norte_ui_host::bridge::MAX_DIALOGS;
    let mut vivas = Vec::new();
    for i in 0..(cap + 3) {
        let p = inject_task_for(&tx, 400 + i as u64, norte_proto::TaskKind::RenameBatch);
        p.send_modify(|p| p.state = norte_proto::TaskState::Completed);
        vivas.push(p);
    }

    let mut last = Vec::new();
    for _ in 0..60 {
        let d = next_dialogs(&mut sub).await;
        last = d;
        if last.len() >= cap {
            break;
        }
    }
    assert!(
        last.len() <= cap,
        "the stack does not go past the ceiling: {}",
        last.len()
    );
    drop(vivas);
}

/// A window WITHOUT effects does not abort another client's task.
///
/// Cancelling a copy leaves the destination clean or a `.norte-partial`: it
/// touches disk. Its own tasks are a different matter — launching them
/// already needed the switch.
#[tokio::test]
async fn read_only_does_not_stop_someone_elses_task() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.foreign.lock().expect("ajenas") = Some(rx);
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: Arc::new(fake),
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: test_settings(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::default_columns(),
        effects: norte_ui_host::commands::Effects::SoloRead,
        log_ring: None,
    })
    .await
    .expect("starts");
    let mut sub = h.subscribe();
    let canceled = Arc::new(std::sync::Mutex::new(Vec::new()));
    let _p = inject_task(&tx, 55, &canceled);
    next_tasks(&mut sub).await;

    let ack = h
        .dispatch(key_mod("k", true, false))
        .await
        .expect("host alive");
    assert!(
        matches!(ack, ActionAck::Unavailable { .. }),
        "a window without effects does not stop it: {ack:?}"
    );
    assert!(canceled.lock().expect("canceladas").is_empty());
}

/// An approval says WHAT is being asked and WHO is asking, and both stay
/// outside the list of paths.
///
/// Mixed in with the paths they were one more line: a file named `delete` —
/// or named like an agent session — was indistinguishable from the line
/// saying what is being approved. And the deadline, the same: with
/// `ttl_ms == 0` no deadline line was painted at all, so a file named
/// «expires in 3600 s» was the only one that looked like one.
#[tokio::test]
async fn an_approval_says_what_who_and_until_when() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.approvals.lock().expect("aprobaciones") = Some(rx);
    let (host, _snap) = host_tree(Arc::new(fake)).await;
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
    .expect("the host is listening");

    let d = &next_dialogs(&mut sub).await[0];
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
    assert_eq!(d.body.len(), 2, "the body is ONLY the paths: {:?}", d.body);
}

/// Without a TTL — a pending one rebuilt by the resync — it is said that the
/// deadline is NOT known, instead of staying silent.
///
/// Staying silent leaves the dialog up front inviting approval on an id the
/// daemon may have reaped a while ago.
#[tokio::test]
async fn an_approval_without_a_ttl_says_it_does_not_know_the_deadline() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.approvals.lock().expect("aprobaciones") = Some(rx);
    let (host, _snap) = host_tree(Arc::new(fake)).await;
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
    .expect("the host is listening");

    let d = &next_dialogs(&mut sub).await[0];
    assert_eq!(
        d.deadline.as_deref(),
        Some(norte_i18n::t_in(norte_i18n::Lang::Es, "modal-approval-ttl-unknown").as_str())
    );
    assert!(d.asker.is_none(), "with no session, one is not invented");
}

/// The plaintext-session notice carries the connection in its OWN field and
/// with its mark.
///
/// Inside the sentence, a host named `banco.example@malo.example` — which
/// carries not one character to mask — reads as the userinfo of a
/// legitimate host. And masking without saying so, on the indicator that
/// something is traveling unencrypted, is where it costs the most.
#[tokio::test]
async fn a_plaintext_notice_carries_the_connection_separately_and_marked() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.degraded.lock().expect("degradadas") = Some(rx);
    let (h, _snap) = host_tree(Arc::new(fake)).await;
    let mut sub = h.subscribe();
    tx.send(norte_proto::methods::ConnectionDegraded {
        scheme: "ftp".to_owned(),
        host: "ma\u{202E}lo.example".to_owned(),
        reason: "ftp-plaintext".to_owned(),
        detail: None,
    })
    .expect("the host is listening");

    let banners = next_banners(&mut sub).await;
    let subject = banners
        .iter()
        .find_map(|b| b.subject.clone())
        .expect("the notice carries its connection");
    assert!(!subject.host.contains('\u{202E}'), "{subject:?}");
    assert!(subject.hostile, "and says it masked it: {subject:?}");
    assert!(
        banners.iter().all(|b| !b.text.contains("://")),
        "the connection is not assembled inside the sentence: {banners:?}"
    );
}

/// A kind this host does not project and whose name arrives altered is
/// MARKED.
///
/// It comes from the user's layout disposition file: it was being masked and
/// the flag was being dropped, so it read as faithful (#266).
#[tokio::test]
async fn an_unknown_kind_with_an_altered_name_is_marked() {
    use norte_frontend::layout::{KindId, Node, SlotId};
    let layout = Node::Split {
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
        backend: fake_tree(),
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout,
        viewport: (120, 40),
        settings: test_settings(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::default_columns(),
        effects: norte_ui_host::commands::Effects::Full,
        log_ring: None,
    })
    .await
    .expect("arranca");

    let marked = snap.slots.iter().any(|s| match s {
        SlotView::Unsupported {
            kind_name,
            kind_name_hostile,
            ..
        } => *kind_name_hostile && !kind_name.contains('\u{202E}'),
        _ => false,
    });
    assert!(marked, "el kind alterado se dice: {:?}", snap.slots);
}
