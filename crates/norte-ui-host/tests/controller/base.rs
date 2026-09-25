use super::*;

// ---------------------------------------------------------------------------
// Waiting without a clock.
//
// These tests talk to an actor that queues every mutation with
// `tokio::spawn` and answers the ack BEFORE the task runs. So a test that
// wants to see what got queued has to wait for something, and for a long
// time that something was a `sleep(30)`: a wall-clock bet that, with 423
// tests in parallel, loses as soon as the machine is loaded. An intermittent
// test is a bug.
//
// There are three questions and each has its own tool:
//
// - "did X already happen?" → [`until`], which waits for the double's
//   NOTIFICATION. It costs zero on the green path and names what it was
//   waiting for when it fails. Its short form, for the most common case, is
//   [`annotated`].
// - "is it certain that NOTHING happened?" → [`settle`], which waits for the
//   executor to have no work left ready. With the clock stopped that is
//   tokio's CONTRACT, not a bet on the scheduler.
// - "and when the double does not see it?" → [`snapshot_until`], which requests
//   snapshots until the screen says so. It is what is left for a write that
//   comes back through `spawn_blocking` or a panel that reseeds itself.
//
// For a REAL deadline (the board's TTL, an extension timeout) neither works:
// that is `#[tokio::test(start_paused = true)]` and `tokio::time::advance`,
// which skips the deadline instead of waiting for it.
// ---------------------------------------------------------------------------

/// Waits for the double to RECORD what the test is looking for. Without a
/// clock.
///
/// `that_expected` is what gets printed if it never arrives: a test that hangs
/// has to say what it was waiting for, not blow up in the assertion
/// afterward.
pub(super) async fn until<T>(
    f: &Fake,
    that_expected: &str,
    that: impl Fn(&Fake) -> Option<T>,
) -> T {
    f.until(that_expected, that).await
}

/// Waits for the double to have at least `n` records in the list it is
/// pointed at, and returns a copy.
///
/// It is the short form of [`until`] for by far the most common case: "what
/// had to be queued is already queued". Returns the cloned `Vec` and not the
/// `MutexGuard` on purpose: a guard cannot cross an `await`.
pub(super) async fn annotated<T: Clone>(
    f: &Fake,
    that_expected: &str,
    n: usize,
    field: impl Fn(&Fake) -> Vec<T>,
) -> Vec<T> {
    until(f, that_expected, |f| {
        let v = field(f);
        (v.len() >= n).then_some(v)
    })
    .await
}

/// Repeats `Resync` until the snapshot satisfies what is asked of it.
///
/// Each round is a round trip to the actor, so the loop advances at the
/// host's pace and not the clock's. It is what is needed when what is being
/// waited for is NOT recorded by the double — a disk write that comes back
/// through `spawn_blocking`, a panel that reseeds itself — and that is why
/// [`until`] does not work.
///
/// The deadline is the FAILURE budget, same as in [`until`]: on the green
/// path the first or second snapshot already brings what is sought.
pub(super) async fn snapshot_until<T>(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    that_expected: &str,
    that: impl Fn(&norte_ui_host::ViewSnapshot) -> Option<T>,
) -> T {
    const RESCUE: std::time::Duration = std::time::Duration::from_secs(15);
    let wait = async {
        loop {
            h.dispatch(UiAction::Resync).await.expect("host alive");
            if let Some(v) = that(&next_snapshot(sub).await) {
                return v;
            }
        }
    };
    let Ok(v) = tokio::time::timeout(RESCUE, wait).await else {
        panic!("the screen never reached: {that_expected}")
    };
    v
}

/// Waits for the executor to have NO work left ready.
///
/// It is the answer to "nothing got queued": there is no event to wait for
/// there, so what has to be guaranteed is that whatever tasks the actor might
/// have launched before answering the ack have already run. The window is
/// narrow and specific: the actor validates, does `tokio::spawn` and ANSWERS
/// the ack; the double records on entering the method, but that method is
/// only called when the launched task gets its first poll.
///
/// **With the clock STOPPED, tokio only advances time when it has nothing
/// left to run.** So sleeping a virtual instant is exactly "wait for the
/// executor to run out of work": when this returns, every task launched
/// before has been polled at least once and is either finished or waiting on
/// something. It costs no real time and guesses nothing.
///
/// It used to be 32 `yield_now()` calls, and that was a bet with another
/// name: tokio's documentation says `yield_now` can poll the same task again
/// immediately, so "32 yields" did not guarantee the others had advanced.
/// Thirty-three of these negative checks depended on just that.
///
/// The deadline is for relief, not for waiting: if the executor never goes
/// still — a spinning task — this says so instead of hanging forever.
pub(super) async fn settle() {
    const RESCUE: std::time::Duration = std::time::Duration::from_secs(15);
    let still = async {
        tokio::time::pause();
        // A VIRTUAL instant: the stopped clock's auto-advance does not
        // happen until the executor is idle, which is exactly what is being
        // waited for.
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        tokio::time::resume();
    };
    assert!(
        tokio::time::timeout(RESCUE, still).await.is_ok(),
        "the executor never ran out of work: there is a spinning task"
    );
}

/// A name that is not UTF-8 crosses the bridge MARKED and with the canonical
/// replacement: the entry is neither rejected nor is the notice lost.
#[tokio::test]
async fn a_non_utf8_name_arrives_marked() {
    let (_h, snap) = host_tree(fake_tree()).await;
    let hostile = listing(&snap)
        .rows
        .iter()
        .find(|r| r.hostile)
        .expect("the hostile row arrives");
    assert!(
        hostile.display_name.contains('\u{FFFD}'),
        "the painted name carries the canonical replacement: {:?}",
        hostile.display_name
    );
}

/// The sort order is the SHARED one: the same one `PaneState` produces for
/// the same entries, not the host's own.
#[tokio::test]
async fn the_sort_order_is_the_shared_layers() {
    let (_h, snap) = host_tree(fake_tree()).await;
    let names: Vec<&str> = listing(&snap)
        .rows
        .iter()
        .map(|r| r.display_name.as_str())
        .collect();
    // Directories first, and within each group by name: it is
    // `norte_frontend::sort`'s rule, and here it is only checked that the
    // host does not reimplement it.
    assert_eq!(names[0], "docs");
    assert!(names.contains(&"notas.txt"));
}

/// Entering a directory navigates and brings its listing.
#[tokio::test]
async fn activating_a_directory_navigates() {
    let (h, snap) = host_tree(fake_tree()).await;
    let docs = listing(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("the directory is there");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs.key,
        generation: listing(&snap).generation,
    })
    .await
    .expect("host alive");

    let after = next_snapshot(&mut sub).await;
    let b = listing(&after);
    assert!(b.path_display.ends_with("/casa/docs"), "{}", b.path_display);
    assert_eq!(b.rows.len(), 2);
    assert_ne!(
        b.generation,
        listing(&snap).generation,
        "another listing, another generation: old keys expire"
    );
}

/// Going up leaves the cursor on the directory being left, which is what
/// makes going down and up reversible.
#[tokio::test]
async fn going_up_returns_the_cursor_to_the_origin_directory() {
    let (h, snap) = host_tree(fake_tree()).await;
    let docs = listing(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("the directory is there");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs.key,
        generation: listing(&snap).generation,
    })
    .await
    .expect("host alive");
    next_snapshot(&mut sub).await;

    h.dispatch(UiAction::Parent { slot_id: 1 })
        .await
        .expect("host alive");
    let up = next_snapshot(&mut sub).await;
    let b = listing(&up);
    let under_cursor = b
        .rows
        .iter()
        .find(|r| Some(r.key) == b.cursor)
        .expect("there is a cursor");
    assert_eq!(
        under_cursor.display_name, "docs",
        "the cursor goes back to the directory it left"
    );
}

/// Back and forward walk the trail, and an exhausted trail SAYS so: a dead
/// key is indistinguishable from a broken one.
#[tokio::test]
async fn the_trail_goes_and_comes_back_and_says_when_it_ends() {
    let (h, snap) = host_tree(fake_tree()).await;
    let docs = listing(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("the directory is there");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs.key,
        generation: listing(&snap).generation,
    })
    .await
    .expect("host alive");
    next_snapshot(&mut sub).await;

    h.dispatch(UiAction::History {
        slot_id: 1,
        back: true,
    })
    .await
    .expect("host alive");
    let back = next_snapshot(&mut sub).await;
    assert!(listing(&back).path_display.ends_with("/casa"));

    h.dispatch(UiAction::History {
        slot_id: 1,
        back: false,
    })
    .await
    .expect("host alive");
    let forward = next_snapshot(&mut sub).await;
    assert!(listing(&forward).path_display.ends_with("/casa/docs"));

    let ack = h
        .dispatch(UiAction::History {
            slot_id: 1,
            back: false,
        })
        .await
        .expect("host alive");
    match ack {
        ActionAck::Unavailable { reason_key } => {
            assert_eq!(reason_key, "msg-nav-no-forward");
        }
        other => panic!("an exhausted trail says so: {other:?}"),
    }
}

/// A response that arrives LATE, once another navigation has already
/// replaced it, is discarded in Rust. Without this, the abandoned
/// directory's listing would show up over the current one.
#[tokio::test]
async fn a_late_response_does_not_overwrite_the_new_navigation() {
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"docs".to_vec(), true)]);
    f.put("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    f.delay_ms = 60;
    let backend = Arc::new(f);
    let (h, snap) = host_tree(Arc::clone(&backend)).await;
    let docs = listing(&snap).rows[0].key;
    let mut sub = h.subscribe();

    // Enter `docs` and, without waiting, go back home: the first response
    // will arrive once the slot is already in another navigation.
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs,
        generation: listing(&snap).generation,
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::History {
        slot_id: 1,
        back: true,
    })
    .await
    .expect("host alive");

    let snap = next_snapshot(&mut sub).await;
    assert!(
        listing(&snap).path_display.ends_with("/casa"),
        "the last navigation is the one that rules: {}",
        listing(&snap).path_display
    );
    // And the late one produces no second snapshot with the abandoned
    // directory. It waits for the THREE responses to have come back — the
    // replaced one included, which is the one that could overwrite — and
    // only then does it look at the channel.
    until(&backend, "no listing in flight", |f| {
        (f.listings() == 3 && f.en_calma()).then_some(())
    })
    .await;
    settle().await;
    let more = tokio::time::timeout(std::time::Duration::ZERO, sub.recv()).await;
    assert!(
        more.is_err(),
        "the replaced response does not reach the screen"
    );
    assert_eq!(backend.listings(), 3, "initial + docs + back");
}

/// Moving the cursor sends the cursor, not the whole listing.
#[tokio::test]
async fn moving_the_cursor_does_not_resend_the_rows() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::MoveCursor {
        slot_id: 1,
        delta: 1,
    })
    .await
    .expect("host alive");
    let Update::Message(m) = sub.recv().await.expect("arrives") else {
        panic!("no lag");
    };
    let UiUpdate::Patch(p) = &m.payload else {
        panic!("a patch");
    };
    assert!(
        matches!(p.changes[0], norte_ui_host::dto::ViewChange::Cursor { .. }),
        "only the cursor travels: {:?}",
        p.changes[0]
    );
}

/// The TOTAL arrives without requesting a snapshot: it is the renderer's
/// scroll height.
///
/// `total_rows` only travelled in the whole snapshot, and the paginated
/// drain answers with row patches — the LAST batch too. So the renderer
/// stayed stuck on the FIRST PAGE's total (100) forever: it paints the
/// scroll canvas at `total * cell_height` and publishes `aria-rowcount`, so a
/// directory of five thousand files stayed capped at row 100 for the wheel,
/// and there was no way to request the rest because the visible range is
/// computed from the scroll.
///
/// The test next to this one did not catch it because it requests `Resync`
/// on every round, which is exactly what a real renderer does NOT do: it
/// only resyncs after a sequence gap or a `Lagged`.
#[tokio::test]
async fn a_large_listings_total_arrives_with_no_snapshot_requested() {
    let mut fake = Fake::default();
    let many: Vec<(Vec<u8>, bool)> = (0..5_000u32)
        .map(|i| (format!("f{i:05}").into_bytes(), false))
        .collect();
    fake.put("mem:///casa", many);
    let (host, snap) = host_tree(Arc::new(fake)).await;
    let mut sub = host.subscribe();
    assert!(
        listing(&snap).total_rows.expect("there is a total") <= 100,
        "to start with, only the first page"
    );

    // With NO `Resync`: only what the host sends on its own while draining.
    let mut last_total = None;
    let wait = async {
        loop {
            match sub.recv().await.expect("host alive") {
                Update::Message(m) => match m.payload {
                    UiUpdate::Patch(p) => {
                        for c in &p.changes {
                            if let norte_ui_host::dto::ViewChange::Rows { total_rows, .. } = c {
                                last_total = *total_rows;
                            }
                        }
                    }
                    UiUpdate::Snapshot(s) => {
                        last_total = listing(&s).total_rows;
                    }
                    UiUpdate::Notice(_) => {}
                },
                // Falling behind is "request a snapshot", and the renderer
                // requests it. It does not count as the total arriving on
                // its own.
                Update::Lagged => {}
            }
            if last_total == Some(5_000) {
                return;
            }
        }
    };
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(20), wait)
            .await
            .is_ok(),
        "the renderer never finds out there are 5,000 rows: it stays capped \
         at {last_total:?} and cannot scroll further down"
    );
}

/// A large listing: the first page paints right away, the rest arrives
/// behind it, and of the total only the visible rows cross over.
#[tokio::test]
async fn a_large_listing_neither_waits_nor_crosses_whole() {
    let mut fake = Fake::default();
    let many: Vec<(Vec<u8>, bool)> = (0..5_000u32)
        .map(|i| (format!("f{i:05}").into_bytes(), false))
        .collect();
    fake.put("mem:///casa", many);
    let (host, snap) = host_tree(Arc::new(fake)).await;

    // The first snapshot does NOT wait for the whole listing.
    let first = listing(&snap).total_rows.expect("there is a total");
    assert!(
        first <= 100,
        "the first page paints without waiting for the rest: {first}"
    );

    // The rest arrives behind it. It is polled, instead of counting
    // messages: batches are asynchronous and the exact number is not the
    // contract. What is NOT needed is a clock: each round is a round trip to
    // the actor, so the loop advances at the drain's pace, not the clock's.
    let mut sub = host.subscribe();
    let mut total = first;
    for _ in 0..2_000 {
        if total >= 5_000 {
            break;
        }
        host.dispatch(UiAction::Resync).await.expect("host alive");
        let snap = next_snapshot(&mut sub).await;
        total = listing(&snap).total_rows.expect("there is a total");
    }
    assert_eq!(total, 5_000, "it ends up whole");

    // And of the five thousand, forty cross over.
    host.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 2_000,
        count: 40,
    })
    .await
    .expect("host alive");
    host.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    assert_eq!(listing(&snap).rows.len(), 40);
}

/// A key WITH modifiers.
pub(super) fn key_mod(k: &str, ctrl: bool, shift: bool) -> UiAction {
    UiAction::Key(norte_ui_host::keys::KeyInput {
        key: k.to_owned(),
        ctrl,
        alt: false,
        shift,
        meta: false,
    })
}

pub(super) fn press(k: &str) -> UiAction {
    UiAction::Key(norte_ui_host::keys::KeyInput {
        key: k.to_owned(),
        ctrl: false,
        alt: false,
        shift: false,
        meta: false,
    })
}

/// `alt+<something>`: the modifier goes in its own field, never in the key's
/// name — `"Alt+o"` is not a key name, `to_chord` rejects it and the host
/// answers `Unavailable` without sending anything. A test written that way
/// passed or not depending on what envelope was left in the queue.
pub(super) fn key_alt(k: &str) -> UiAction {
    UiAction::Key(norte_ui_host::keys::KeyInput {
        key: k.to_owned(),
        ctrl: false,
        alt: true,
        shift: false,
        meta: false,
    })
}

/// A preset key resolves to the shared CATALOGUE's command and the host
/// only runs it: there is no second keymap.
#[tokio::test]
async fn a_preset_key_moves_the_cursor() {
    let (h, snap) = host_tree(fake_tree()).await;
    let before = listing(&snap).cursor;
    let mut sub = h.subscribe();
    let ack = h.dispatch(press("ArrowDown")).await.expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }));
    let Update::Message(m) = sub.recv().await.expect("arrives") else {
        panic!("no lag");
    };
    let UiUpdate::Patch(p) = &m.payload else {
        panic!("a patch");
    };
    match &p.changes[0] {
        norte_ui_host::dto::ViewChange::Cursor { cursor, .. } => {
            assert_ne!(*cursor, before, "the cursor moved");
        }
        other => panic!("expected the cursor: {other:?}"),
    }
}

/// The counter is resolved by Rust, not the renderer: `3` and then `j` goes
/// down three.
#[tokio::test]
async fn the_counter_is_resolved_by_the_host() {
    let mut fake = Fake::default();
    let names: Vec<(Vec<u8>, bool)> = (0..10u32)
        .map(|n| (format!("f{n}").into_bytes(), false))
        .collect();
    fake.put("mem:///casa", names);
    let backend = Arc::new(fake);
    let (host, _snap) = UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        // `vim` is the preset that enables counters.
        keymap: norte_ui_host::keys::keymap_de_preset("vim").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("vim").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
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
    .expect("starts");

    // Typing the counter, the host PAINTS it: what is not visible cannot be
    // cancelled.
    let mut sub = host.subscribe();
    host.dispatch(press("3")).await.expect("host alive");
    let Update::Message(m) = sub.recv().await.expect("arrives") else {
        panic!("no lag");
    };
    let UiUpdate::Patch(p) = &m.payload else {
        panic!("a patch");
    };
    match &p.changes[0] {
        norte_ui_host::dto::ViewChange::Status(s) => {
            assert_eq!(s.pending.as_ref().and_then(|p| p.count), Some(3));
        }
        other => panic!("expected the status: {other:?}"),
    }

    host.dispatch(press("j")).await.expect("host alive");
    host.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    assert_eq!(
        listing(&snap).cursor,
        Some(norte_ui_host::RowKey(3)),
        "three rows, not one"
    );
}

/// A key bound to a command this host does not implement runs NOTHING, and
/// says so with the same phrase as the TUI.
#[tokio::test]
async fn a_command_the_host_does_not_do_triggers_nothing() {
    // The example kept rotating as the window got closer to parity — `F5`
    // until copy, `F4` until edit (#290), `alt+t` until the tree, `alt+q`
    // until the docked viewer (#291), `alt+r` until the batch (#310), `alt+C`
    // until compare (#312) — and they ran out: the window does everything
    // the catalogue has live. What is left is what does NOT APPLY to a
    // window (`tests/parity_matrix.rs`), and `ctrl+o` in the `norton` preset is
    // bound to one of those, `app.toggle-panels`: hiding the panels to see
    // the terminal behind them means nothing in a window.
    let (h, snap) = UiHost::start(UiHostOptions {
        backend: fake_tree(),
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("norton").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("norton").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
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
    .expect("starts");
    let before = listing(&snap).clone();
    let ack = h
        .dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
            key: "o".to_owned(),
            ctrl: true,
            alt: false,
            shift: false,
            meta: false,
        }))
        .await
        .expect("host alive");
    match ack {
        ActionAck::Unavailable { reason_key } => assert_eq!(reason_key, "cmd-not-here"),
        other => panic!("expected unavailable: {other:?}"),
    }
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    let now = listing(&snap);
    assert_eq!(now.generation, before.generation, "nothing changed");
    assert!(
        snap.status.message.is_some(),
        "and the bar says so instead of staying silent"
    );
}

/// A key with no binding is discarded leaving the state clean: it neither
/// runs anything nor leaves a dangling prefix.
#[tokio::test]
async fn an_unbound_key_is_discarded() {
    let (h, _snap) = host_tree(fake_tree()).await;
    // `Insert` is not bound in the orthodox preset.
    let ack = h.dispatch(press("Insert")).await.expect("host alive");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "a loose key is not an error: {ack:?}"
    );
}

/// And a key the adapter does not understand is not guessed either.
#[tokio::test]
async fn a_key_that_is_not_understood_is_not_invented() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let ack = h.dispatch(press("Compose")).await.expect("host alive");
    match ack {
        ActionAck::Unavailable { reason_key } => assert_eq!(reason_key, "host-key-unmapped"),
        other => panic!("expected unavailable: {other:?}"),
    }
}

/// Starts with a specific layout and a specific size.
pub(super) async fn host_con_layout(
    backend: Arc<Fake>,
    layout: &str,
    viewport: (u16, u16),
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree(layout).expect("layout"),
        viewport,
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
    .expect("arranca")
}

/// Starts with a given TREE: a real session's, to reproduce what someone
/// saw.
pub(super) async fn host_with_tree(
    backend: Arc<Fake>,
    layout: norte_frontend::layout::Node,
    viewport: (u16, u16),
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout,
        viewport,
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
    .expect("arranca")
}

/// The five factory layouts resolve to reasonable sizes, and none leaves a
/// screen with no listing.
#[tokio::test]
async fn the_five_factory_layout_presets_resolve() {
    for name in norte_frontend::layout::presets::NAMES {
        for viewport in [(80u16, 24u16), (120, 40), (200, 60)] {
            let (_h, snap) = host_con_layout(fake_tree(), name, viewport).await;
            let listings = snap
                .slots
                .iter()
                .filter(|s| matches!(s, SlotView::Browser(_)))
                .count();
            assert!(
                listings >= 1,
                "{name} at {viewport:?} ended up with no usable listing"
            );
        }
    }
}

/// Resizing splits again and does NOT rewrite the layout: a saved layout is
/// the user's intent, not a function of their window's size.
#[tokio::test]
async fn resizing_does_not_rewrite_the_layout() {
    let (h, big) = host_con_layout(fake_tree(), "orthodox", (200, 60)).await;
    let listings_before = big
        .slots
        .iter()
        .filter(|s| matches!(s, SlotView::Browser(_)))
        .count();
    assert_eq!(listings_before, 2, "orthodox has two listings");

    let mut sub = h.subscribe();
    h.dispatch(UiAction::SetViewport {
        width: 30,
        height: 10,
    })
    .await
    .expect("host alive");
    let tight = next_snapshot(&mut sub).await;
    assert!(
        tight
            .slots
            .iter()
            .any(|s| matches!(s, SlotView::Browser(_))),
        "even if both do not fit, one listing is left"
    );

    // And going back to the previous size, both come back: the tree was
    // never touched.
    h.dispatch(UiAction::SetViewport {
        width: 200,
        height: 60,
    })
    .await
    .expect("host alive");
    let again = next_snapshot(&mut sub).await;
    let listings = again
        .slots
        .iter()
        .filter(|s| matches!(s, SlotView::Browser(_)))
        .count();
    assert_eq!(listings, 2, "the layout survived the squeeze");
}

/// A slot of a kind this host does not yet project travels grayed out and
/// with its name: preserving what is not understood is the rule, and
/// disappearing would be worse than being disabled.
#[tokio::test]
async fn an_unknown_kind_travels_disabled_and_named() {
    let (_h, snap) = host_con_layout(fake_tree(), "simple", (120, 40)).await;
    let names: Vec<&str> = snap
        .slots
        .iter()
        .filter_map(|s| match s {
            SlotView::Unsupported { kind_name, .. } => Some(kind_name.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        names.contains(&"tasks") && names.contains(&"status"),
        "the kinds the host does not project are still there: {names:?}"
    );
}

/// Focus changes slot, and the target follows it: the target is ALWAYS
/// another visible listing, never the same one that has focus.
#[tokio::test]
async fn focus_changes_and_the_target_follows_it() {
    let (h, snap) = host_con_layout(fake_tree(), "orthodox", (200, 60)).await;
    assert_eq!(snap.focus, Some(1));
    let mut sub = h.subscribe();
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    // Changing focus does NOT resend the screen: the split travels with the
    // new roles, which is the only thing that changed.
    let after = next_layout(&mut sub).await;
    let role = |id: u32| {
        after
            .placements
            .iter()
            .find(|p| p.slot_id == id)
            .and_then(|p| p.role)
    };
    assert_eq!(role(2), Some(norte_ui_host::dto::SlotRole::Active));
    assert_eq!(role(1), Some(norte_ui_host::dto::SlotRole::Target));
}

/// Focusing a slot that is not visible is a race with a previous split, not
/// an order.
#[tokio::test]
async fn what_is_not_visible_cannot_be_focused() {
    let (h, _snap) = host_con_layout(fake_tree(), "orthodox", (200, 60)).await;
    let ack = h
        .dispatch(UiAction::FocusSlot { slot_id: 99 })
        .await
        .expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Generation
        }
    );
}

/// What is not visible is not fetched: a split that hides a listing does not
/// ask the daemon for its directory.
#[tokio::test]
async fn a_hidden_slot_does_not_request_a_listing() {
    let backend = fake_tree();
    // Widthwise, both `orthodox` listings fit; at 30 columns, they do not.
    let (_h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (30, 10)).await;
    assert_eq!(
        backend.listings(),
        1,
        "only the visible listing requests its directory"
    );
}

/// Builds a saved session with a slot on `dir`.
pub(super) fn session_saved(
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
            jump: None,
            sort: norte_frontend::SortSpec::default(),
            columns: Vec::new(),
            show_hidden: false,
            touched_ms: 0,
            marks: Vec::new(),
        },
    );
    norte_proto::methods::Session {
        version,
        revision,
        body: serde_json::to_value(&body).expect("json"),
    }
}

/// The session says where each slot was, and the host starts there.
#[tokio::test]
async fn the_session_places_the_slots() {
    let mut fake = Fake::default();
    fake.put("mem:///casa", vec![(b"a".to_vec(), false)]);
    fake.put("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    *fake.session.lock().expect("session") = (session_saved(1, 7, 1, "mem:///casa/docs"), true);
    let (_h, snap) = host_tree(Arc::new(fake)).await;
    assert!(
        listing(&snap).path_display.ends_with("/casa/docs"),
        "it started where the session left it: {}",
        listing(&snap).path_display
    );
}

/// The window returns the CURSOR the session saved, like the terminal.
///
/// It was seen at the first handoff with a person in front: the terminal had
/// the cursor on `c.txt` and the window opened on `/..`. It saved the cursor
/// and never read it back — place, order, hidden state and history it did —
/// so "stay where you were" held only halfway. Same index the terminal uses
/// (`restore_cursor`), so a handoff lands on the same row in both directions.
#[tokio::test]
async fn the_session_returns_the_cursor() {
    let mut fake = Fake::default();
    fake.put(
        "mem:///casa",
        vec![
            (b"a.txt".to_vec(), false),
            (b"b.txt".to_vec(), false),
            (b"c.txt".to_vec(), false),
        ],
    );
    let mut session = session_saved(1, 7, 1, "mem:///casa");
    let mut body: norte_frontend::session::SessionBody =
        serde_json::from_value(session.body.clone()).expect("body");
    body.slots.get_mut(&1).expect("slot").cursor = 2;
    session.body = serde_json::to_value(&body).expect("json");
    *fake.session.lock().expect("session") = (session, true);
    let (h, _snap) = host_tree(Arc::new(fake)).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let (third, under_cursor) = snapshot_until(&h, &mut sub, "the listing with its rows", |s| {
        let b = listing(s);
        let third = b.rows.get(2)?.display_name.clone();
        let cursor = b.cursor?;
        let under = b
            .rows
            .iter()
            .find(|r| r.key == cursor)?
            .display_name
            .clone();
        Some((third, under))
    })
    .await;
    assert_eq!(
        under_cursor, third,
        "the cursor returns to the row it saved"
    );
}

/// A typed directory beats the session, and then the saved cursor does NOT
/// apply: it was a row from ANOTHER directory. The same rule as
/// `pin_start_dir` in the terminal.
#[tokio::test]
async fn with_a_typed_dir_the_saved_cursor_does_not_apply() {
    let mut fake = Fake::default();
    fake.put(
        "mem:///casa",
        vec![(b"a".to_vec(), false), (b"b".to_vec(), false)],
    );
    fake.put(
        "mem:///casa/docs",
        vec![
            (b"x.md".to_vec(), false),
            (b"y.md".to_vec(), false),
            (b"z.md".to_vec(), false),
        ],
    );
    let mut session = session_saved(1, 7, 1, "mem:///casa/docs");
    let mut body: norte_frontend::session::SessionBody =
        serde_json::from_value(session.body.clone()).expect("body");
    body.slots.get_mut(&1).expect("slot").cursor = 2;
    session.body = serde_json::to_value(&body).expect("json");
    *fake.session.lock().expect("session") = (session, true);
    let (h, _snap) = Box::pin(UiHost::start(UiHostOptions {
        backend: Arc::new(fake),
        initial_dir: VPath::parse("mem:///casa").expect("vpath"),
        initial_dir_requested: true,
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
        effects: norte_ui_host::commands::Effects::Full,
        log_ring: None,
    }))
    .await
    .expect("starts");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let (first, under_cursor) = snapshot_until(&h, &mut sub, "the typed listing", |s| {
        let b = listing(s);
        if !b.path_display.ends_with("/casa") {
            return None;
        }
        let first = b.rows.first()?.display_name.clone();
        let cursor = b.cursor?;
        let under = b
            .rows
            .iter()
            .find(|r| r.key == cursor)?
            .display_name
            .clone();
        Some((first, under))
    })
    .await;
    assert_eq!(
        under_cursor, first,
        "`docs`'s cursor does not apply over `casa`"
    );
}

/// Starts like [`host_tree`], with `[profile.start]` set.
pub(super) async fn host_con_start(
    backend: Arc<Fake>,
    start: &[(u32, &str)],
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    let mut settings = test_settings();
    settings.common.profile_start = start
        .iter()
        .map(|(id, wire)| (*id, VPath::parse(wire).expect("vpath")))
        .collect();
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings,
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::default_columns(),
        effects: norte_ui_host::commands::Effects::Full,
        log_ring: None,
    })
    .await
    .expect("arranca")
}

/// `[profile.start]` opens the slot the session knows nothing about.
///
/// It is what makes a freshly created profile useful, or one arriving from
/// another machine: the key used to be written by both frontends and read by
/// NEITHER, so entering a profile left the panels where they were and the
/// profile only changed the colors. Two files promised otherwise.
#[tokio::test]
async fn profile_start_seeds_a_slot_with_no_session() {
    let mut fake = Fake::default();
    fake.put("mem:///casa", vec![(b"a".to_vec(), false)]);
    fake.put("mem:///casa/fotos", vec![(b"gato.png".to_vec(), false)]);
    // A readable and EMPTY session: nobody has saved slot 1 yet.
    *fake.session.lock().expect("session") = (
        norte_proto::methods::Session {
            version: 1,
            revision: 7,
            body: serde_json::to_value(norte_frontend::session::SessionBody::default())
                .expect("json"),
        },
        true,
    );
    let (_h, snap) = host_con_start(Arc::new(fake), &[(1, "mem:///casa/fotos")]).await;
    assert!(
        listing(&snap).path_display.ends_with("/casa/fotos"),
        "it opened where the profile says: {}",
        listing(&snap).path_display
    );
}

/// And the SESSION wins: `[profile.start]` says where a slot opens the first
/// time, not every time.
///
/// A profile is a workspace, not a bookmark that sends you back to the
/// start: if every entry into the profile pulled you out of where you were,
/// the profile would be useless for exactly whoever uses it daily.
#[tokio::test]
async fn the_session_beats_profile_start() {
    let mut fake = Fake::default();
    fake.put("mem:///casa", vec![(b"a".to_vec(), false)]);
    fake.put("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    fake.put("mem:///casa/fotos", vec![(b"gato.png".to_vec(), false)]);
    *fake.session.lock().expect("session") = (session_saved(1, 7, 1, "mem:///casa/docs"), true);
    let (_h, snap) = host_con_start(Arc::new(fake), &[(1, "mem:///casa/fotos")]).await;
    assert!(
        listing(&snap).path_display.ends_with("/casa/docs"),
        "it rules where you left it, not where the profile is born: {}",
        listing(&snap).path_display
    );
}

/// A directory TYPED on the command line beats the session.
///
/// `norte-gui /usr/bin` with a saved session used to open wherever you were
/// yesterday and swallow the argument without a word: `apply_session` writes
/// ALL slots' dir, and nothing said "a human just typed this one". The
/// terminal closed the same gap in `eb237c61` with `pin_start_dir`, and it
/// never reached the window.
///
/// It wins on the ACTIVE panel and only there: the other one stays where the
/// session left it, which is half a screen of memory nobody asked to throw
/// away.
#[tokio::test]
async fn the_command_lines_dir_beats_the_session() {
    let mut fake = Fake::default();
    fake.put("mem:///casa", vec![(b"a".to_vec(), false)]);
    fake.put("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    *fake.session.lock().expect("session") = (session_saved(1, 7, 1, "mem:///casa/docs"), true);
    let (_h, snap) = UiHost::start(UiHostOptions {
        backend: Arc::new(fake),
        // What the human typed, which is NOT where the session left it.
        initial_dir: VPath::parse("mem:///casa").expect("vpath"),
        initial_dir_requested: true,
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
        effects: norte_ui_host::commands::Effects::Full,
        log_ring: None,
    })
    .await
    .expect("starts");
    assert!(
        listing(&snap).path_display.ends_with("/casa"),
        "it rules what was typed, not what the session saved: {}",
        listing(&snap).path_display
    );
}

/// And with no argument, the session still rules: it is the usual behavior.
#[tokio::test]
async fn with_no_argument_the_session_still_rules() {
    let mut fake = Fake::default();
    fake.put("mem:///casa", vec![(b"a".to_vec(), false)]);
    fake.put("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    *fake.session.lock().expect("session") = (session_saved(1, 7, 1, "mem:///casa/docs"), true);
    let (_h, snap) = host_tree(Arc::new(fake)).await;
    assert!(
        listing(&snap).path_display.ends_with("/casa/docs"),
        "with nothing typed, where you left it: {}",
        listing(&snap).path_display
    );
}

/// A session with a NEWER schema is neither applied nor — above all —
/// overwritten: starting with no session is recoverable, clobbering a future
/// version's is not.
#[tokio::test]
async fn a_session_from_the_future_is_neither_applied_nor_overwritten() {
    let mut fake = Fake::default();
    fake.put("mem:///casa", vec![(b"a".to_vec(), false)]);
    *fake.session.lock().expect("session") = (
        session_saved(
            norte_frontend::session::SCHEMA_VERSION + 1,
            7,
            1,
            "mem:///casa/docs",
        ),
        true,
    );
    let backend = Arc::new(fake);
    let (h, snap) = host_tree(Arc::clone(&backend)).await;
    assert!(
        listing(&snap).path_display.ends_with("/casa"),
        "it starts from the configuration, not from what it does not understand"
    );
    h.shutdown().await.expect("shuts down");
    assert!(
        backend.written.lock().expect("escrito").is_none(),
        "and it does not write over it"
    );
}

/// A DETACHED window does not write: the session is a document with a
/// single writer.
#[tokio::test]
async fn a_detached_window_does_not_write() {
    let mut fake = Fake::default();
    fake.put("mem:///casa", vec![(b"a".to_vec(), false)]);
    *fake.session.lock().expect("session") = (session_saved(1, 7, 1, "mem:///casa"), false);
    let backend = Arc::new(fake);
    let (h, snap) = host_tree(Arc::clone(&backend)).await;
    // And it SAYS so from the first frame, with the same indicator as the
    // terminal: until now a detached window closed and lost where every
    // panel was without a word.
    let indicator = norte_i18n::t_in(norte_i18n::Lang::Es, "status-session-detached");
    assert!(
        snap.status.banners.iter().any(|b| b.text == indicator),
        "the bar carries the detached-session indicator: {:?}",
        snap.status.banners
    );
    let report = h.shutdown().await.expect("shuts down");
    assert!(
        !report.incomplete,
        "not writing is not leaving something unfinished"
    );
    assert!(backend.written.lock().expect("escrito").is_none());
}

/// And the owner carries no indicator: it is not decoration, it is a state.
#[tokio::test]
async fn the_owner_carries_no_session_indicator() {
    let mut fake = Fake::default();
    fake.put("mem:///casa", vec![(b"a".to_vec(), false)]);
    *fake.session.lock().expect("session") = (session_saved(1, 7, 1, "mem:///casa"), true);
    let (_h, snap) = host_tree(Arc::new(fake)).await;
    let indicator = norte_i18n::t_in(norte_i18n::Lang::Es, "status-session-detached");
    assert!(
        !snap.status.banners.iter().any(|b| b.text == indicator),
        "the owner warns about nothing: {:?}",
        snap.status.banners
    );
}

/// The owner dumps on close — closing right after navigating saves the new
/// directory — and MARKS do not enter the session.
#[tokio::test]
async fn the_owner_dumps_on_close_and_with_no_marks() {
    let mut fake = Fake::default();
    fake.put(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"a".to_vec(), false)],
    );
    fake.put("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    *fake.session.lock().expect("session") = (session_saved(1, 7, 1, "mem:///casa"), true);
    let backend = Arc::new(fake);
    let (h, snap) = host_tree(Arc::clone(&backend)).await;

    // Mark something and navigate.
    let row = listing(&snap).rows[0].key;
    h.dispatch(UiAction::ToggleMark {
        slot_id: 1,
        key: row,
        generation: listing(&snap).generation,
    })
    .await
    .expect("host alive");
    let mut sub = h.subscribe();
    let docs = listing(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("the directory is there")
        .key;
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs,
        generation: listing(&snap).generation,
    })
    .await
    .expect("host alive");
    next_snapshot(&mut sub).await;

    h.shutdown().await.expect("shuts down");
    let written = backend
        .written
        .lock()
        .expect("escrito")
        .clone()
        .expect("wrote");
    let text = written.to_string();
    assert!(
        text.contains("casa/docs"),
        "it saves where it ended up, not where it started: {text}"
    );
    assert!(
        !text.contains("marks") && !text.contains("marcas"),
        "marks do not enter the session: {text}"
    );
}

/// The graphical window does NOT change the TUI's layout, nor sweep its
/// slots.
///
/// `capture_session` wrote `self.tree` into `layouts["default"]`, and until
/// this phase `self.tree` was constant — i.e. it wrote back what it had
/// read. Changing it with `layout.pick` or with two `Ctrl+→` turned it into a
/// real write, and `norte-tui` ADOPTS `layouts["default"]` on startup:
/// browsing the picker for a minute changed the TUI's startup. The field's
/// rustdoc forbids this by its very name (ADR 0058 D5) and
/// `apply_layout_chosen` promises "it applies to THIS window", which
/// was true for the configuration and false for the session.
///
/// And along the way: it started from a `SessionBody::default()`, so any
/// OTHER frontend's slots got thrown away instead of preserved.
#[tokio::test]
async fn closing_the_window_does_not_touch_the_tuis_layout_or_slots() {
    use norte_frontend::layout::{KindId, Node, SlotId};

    // What was in the session: the TUI's layout and one of its own slots
    // this window does not have.
    let tui_layout = Node::Split {
        dir: norte_frontend::layout::Dir::Vertical,
        children: vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(42), KindId::new("tasks")),
        ],
        sizes: vec![
            norte_frontend::layout::Size::Weight(1),
            norte_frontend::layout::Size::Fixed(3),
        ],
    };
    let mut body = norte_frontend::session::SessionBody::default();
    body.layouts
        .insert("default".to_owned(), tui_layout.clone());
    body.slots.insert(
        99,
        norte_frontend::session::SlotState {
            path: VPath::parse("mem:///ajeno").expect("vpath"),
            cursor: 0,
            back: Vec::new(),
            forward: Vec::new(),
            jump: None,
            sort: norte_frontend::SortSpec::default(),
            columns: Vec::new(),
            show_hidden: false,
            // Just touched by the other frontend: it is not an orphan.
            touched_ms: u64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis()),
            )
            .unwrap_or(0),
            marks: Vec::new(),
        },
    );

    let mut fake = Fake::default();
    fake.put("mem:///casa", vec![(b"a".to_vec(), false)]);
    *fake.session.lock().expect("session") = (
        norte_proto::methods::Session {
            version: norte_frontend::session::SCHEMA_VERSION,
            revision: 7,
            body: serde_json::to_value(&body).expect("json"),
        },
        true,
    );
    let backend = Arc::new(fake);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // The window changes ITS OWN layout: widened twice.
    for _ in 0..2 {
        h.dispatch(press("ctrl+Right")).await.expect("host alive");
    }
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let _ = next_snapshot(&mut sub).await;

    h.shutdown().await.expect("shuts down");
    let written = backend
        .written
        .lock()
        .expect("escrito")
        .clone()
        .expect("wrote");
    let saved: norte_frontend::session::SessionBody =
        serde_json::from_value(written).expect("the body parses");

    assert_eq!(
        saved.layouts.get("default"),
        Some(&tui_layout),
        "the TUI's layout stays as it was"
    );
    assert!(
        saved.slots.contains_key(&99),
        "and so does its slot: starting from `default()` threw it away — {:?}",
        saved.slots.keys().collect::<Vec<_>>()
    );
    // And LIVE slots are sealed with a real clock: a zero left them thirty
    // days old for the next writer, which swept them away on its first pass.
    let live = saved.slots.get(&1).expect("its own slot is there");
    assert!(
        live.touched_ms > 0,
        "the live slot is sealed with the time, not with zero: {live:?}"
    );
}

/// A write conflict does NOT overwrite the other window's, and it is SAID.
#[tokio::test]
async fn a_conflict_overwrites_nobody_and_says_so() {
    let mut fake = Fake::default();
    fake.put("mem:///casa", vec![(b"a".to_vec(), false)]);
    *fake.session.lock().expect("session") = (session_saved(1, 7, 1, "mem:///otro"), true);
    fake.conflict = true;
    let backend = Arc::new(fake);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let report = h.shutdown().await.expect("shuts down");
    assert!(
        report.incomplete,
        "our own did not go through, and shutting down silently would lie"
    );
    assert!(backend.written.lock().expect("escrito").is_none());
}

/// **A body that goes over size gets DEGRADED and retried** (#316).
///
/// The core refuses the whole `put` and leaves stored whatever there was,
/// i.e. where the reader was days ago. The TUI already dropped history and
/// tried again; this window treated any error the same — "did not
/// arrive" — and that is ADR 0077's silent divergence.
///
/// What is checked is that the SECOND attempt sends something different:
/// without degrading, retrying is asking for the same error again.
#[tokio::test]
async fn a_body_that_does_not_fit_gets_degraded_and_retried() {
    let mut fake = Fake::default();
    fake.put("mem:///casa", vec![(b"a".to_vec(), false)]);
    // With history, which is the only thing degrading throws away.
    let mut saved = session_saved(1, 7, 1, "mem:///casa");
    let mut body: norte_frontend::session::SessionBody =
        serde_json::from_value(saved.body.clone()).expect("body");
    for s in body.slots.values_mut() {
        s.back = vec![VPath::parse("mem:///casa/atras").expect("vpath")];
    }
    saved.body = serde_json::to_value(&body).expect("json");
    *fake.session.lock().expect("session") = (saved, true);
    // The first one does not fit; the second does.
    *fake.rejections_by_size.lock().expect("rechazos") = 1;
    let backend = Arc::new(fake);

    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let report = h.shutdown().await.expect("shuts down");

    let puts = backend.placed.lock().expect("puestas");
    assert_eq!(puts.len(), 2, "it is retried ONCE: {puts:?}");
    let last: norte_frontend::session::SessionBody =
        serde_json::from_value(puts[1].clone()).expect("body");
    assert!(
        last.slots
            .values()
            .all(|s| s.back.is_empty() && s.forward.is_empty()),
        "the retry goes with no history, which is what gets degraded"
    );
    assert!(
        !last.slots.is_empty(),
        "and WITH the slots: what had to be saved is where the reader is"
    );
    assert!(
        !report.incomplete,
        "the second `put` went through, so nothing is left unwritten"
    );
}

/// And if it does not fit even degraded, it is said: retrying again would be
/// asking for the same error, and shutting down silently would lie.
#[tokio::test]
async fn a_body_that_does_not_fit_even_degraded_says_so() {
    let mut fake = Fake::default();
    fake.put("mem:///casa", vec![(b"a".to_vec(), false)]);
    // WITH history: without it `degrade_for_size` has nothing to throw away,
    // answers `false`, and the retry is not even attempted — the test would
    // pass without exercising the path it claims to exercise.
    let mut saved = session_saved(1, 7, 1, "mem:///casa");
    let mut body: norte_frontend::session::SessionBody =
        serde_json::from_value(saved.body.clone()).expect("body");
    for s in body.slots.values_mut() {
        s.back = vec![VPath::parse("mem:///casa/atras").expect("vpath")];
    }
    saved.body = serde_json::to_value(&body).expect("json");
    *fake.session.lock().expect("session") = (saved, true);
    *fake.rejections_by_size.lock().expect("rechazos") = 5;
    let backend = Arc::new(fake);

    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let report = h.shutdown().await.expect("shuts down");

    assert!(report.incomplete);
    assert_eq!(
        backend.placed.lock().expect("puestas").len(),
        2,
        "one retry, and only one: with nothing else to degrade, insisting is asking for the same error"
    );
    assert!(backend.written.lock().expect("escrito").is_none());
}

/// A double with a saved session this window owns.
fn fake_with_session(saved: norte_proto::methods::Session, owner: bool) -> Fake {
    let mut fake = Fake::default();
    fake.put(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"a".to_vec(), false)],
    );
    *fake.session.lock().expect("session") = (saved, owner);
    fake
}

/// The last body written, already read.
fn body_written(f: &Fake) -> Option<norte_frontend::session::SessionBody> {
    let v = f.written.lock().ok()?.clone()?;
    serde_json::from_value(v).ok()
}

/// Toggling a side panel writes the layout to the session AT ONCE, under the
/// window's OWN key (ADR 0139, which replaces D8 of ADR 0058 here): the
/// terminal and the window each remember their own, and the terminal's is
/// never touched.
#[tokio::test]
async fn toggling_a_panel_writes_the_layout_immediately() {
    let backend = Arc::new(fake_with_session(
        session_saved(1, 7, 1, "mem:///casa"),
        true,
    ));
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    by_palette(&h, &mut sub, "layout.places").await;
    let body = until(&backend, "the layout written", body_written).await;
    let tree = body
        .layouts
        .get("default@window")
        .expect("under the window's key with no profile");
    assert!(
        !body.layouts.contains_key("default"),
        "the terminal's is not written"
    );
    let text = serde_json::to_string(tree).expect("json");
    assert!(text.contains("places"), "with the side bar inside: {text}");
    assert!(
        body.slots.contains_key(&1),
        "and the slots are still there: {:?}",
        body.slots.keys().collect::<Vec<_>>()
    );

    // Closing it writes AGAIN, without it: every tree change gets saved.
    let before = backend.placed.lock().expect("puestas").len();
    by_palette(&h, &mut sub, "layout.places").await;
    let body = until(&backend, "the second write", |f| {
        (f.placed.lock().ok()?.len() > before)
            .then(|| body_written(f))
            .flatten()
    })
    .await;
    let text = serde_json::to_string(body.layouts.get("default@window").expect("still there"))
        .expect("json");
    assert!(!text.contains("places"), "already without the bar: {text}");
}

/// And on startup, what the session saved gets APPLIED, over the
/// configuration's: close the window with two listings, come back with two.
#[tokio::test]
async fn the_saved_layout_applies_on_startup() {
    let mut saved = session_saved(1, 7, 1, "mem:///casa");
    let mut body: norte_frontend::session::SessionBody =
        serde_json::from_value(saved.body.clone()).expect("body");
    body.layouts.insert(
        "default".to_owned(),
        norte_frontend::layout::presets::tree("orthodox").expect("preset"),
    );
    saved.body = serde_json::to_value(&body).expect("json");
    let backend = Arc::new(fake_with_session(saved, true));
    // The host starts with `simple`, one listing; the session says
    // `orthodox`, two. What is seen on startup is what the session says.
    let (_h, snap) = host_tree(Arc::clone(&backend)).await;
    let listings = snap
        .slots
        .iter()
        .filter(|s| matches!(s, norte_ui_host::dto::SlotView::Browser(_)))
        .count();
    assert_eq!(
        listings, 2,
        "the saved layout's two listings, not `simple`'s one"
    );
}

/// ADR 0139: with its own saved, the window starts with ITS OWN, even if the
/// terminal left another one afterward.
#[tokio::test]
async fn the_window_starts_with_its_own_layout_and_not_the_terminals() {
    let mut saved = session_saved(1, 7, 1, "mem:///casa");
    let mut body: norte_frontend::session::SessionBody =
        serde_json::from_value(saved.body.clone()).expect("body");
    // The terminal: one listing. The window: two.
    body.layouts.insert(
        "default".to_owned(),
        norte_frontend::layout::presets::tree("simple").expect("preset"),
    );
    body.layouts.insert(
        "default@window".to_owned(),
        norte_frontend::layout::presets::tree("orthodox").expect("preset"),
    );
    saved.body = serde_json::to_value(&body).expect("json");
    let backend = Arc::new(fake_with_session(saved, true));
    let (_h, snap) = host_tree(Arc::clone(&backend)).await;
    let listings = snap
        .slots
        .iter()
        .filter(|s| matches!(s, norte_ui_host::dto::SlotView::Browser(_)))
        .count();
    assert_eq!(listings, 2, "the window's, not the terminal's");
}

/// The session's tick writes what changed and does NOT repeat the same
/// thing.
///
/// With the clock stopped: a virtual second triggers the tick without
/// waiting a real second. The first round writes — this window's layout was
/// not yet in the session — and the second, with no changes, sends nothing:
/// comparing against the last thing written is all a quiet tick does.
#[tokio::test(start_paused = true)]
async fn the_tick_writes_what_changed_and_does_not_repeat_it() {
    let backend = Arc::new(fake_with_session(
        session_saved(1, 7, 1, "mem:///casa"),
        true,
    ));
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    assert!(
        backend.placed.lock().expect("puestas").is_empty(),
        "starting does not write: the tick has not fired yet"
    );
    tokio::time::advance(std::time::Duration::from_millis(1100)).await;
    let n = until(&backend, "the tick's first write", |f| {
        let n = f.placed.lock().ok()?.len();
        (n > 0).then_some(n)
    })
    .await;
    assert_eq!(n, 1, "one write, the new layout's");

    tokio::time::advance(std::time::Duration::from_millis(2100)).await;
    // One round to the actor: the ticks that fired have already been
    // handled.
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert_eq!(
        backend.placed.lock().expect("puestas").len(),
        1,
        "with no changes, the tick does not repeat the same thing"
    );
}

/// A DETACHED window does not write when toggling a panel either.
#[tokio::test]
async fn a_detached_window_does_not_write_when_toggling_a_panel() {
    let backend = Arc::new(fake_with_session(
        session_saved(1, 7, 1, "mem:///casa"),
        false,
    ));
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    by_palette(&h, &mut sub, "layout.places").await;
    settle().await;
    assert!(
        backend.placed.lock().expect("puestas").is_empty(),
        "detached does not write: the session is a document with a single writer"
    );
}

/// Waits for the next update that carries tasks.
pub(super) async fn next_tasks(
    sub: &mut norte_ui_host::UiSubscription,
) -> Vec<norte_ui_host::dto::TaskView> {
    for _ in 0..20 {
        let next = tokio::time::timeout(WAIT_MAX, sub.recv())
            .await
            .expect("an update with tasks, not a hang")
            .expect("the host is still alive");
        match next {
            Update::Message(m) => {
                if let UiUpdate::Patch(p) = &m.payload {
                    for c in &p.changes {
                        if let norte_ui_host::dto::ViewChange::Tasks { tasks: t, .. } = c {
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
            Update::Lagged => panic!("no lag in this test"),
        }
    }
    panic!("no update with tasks ever arrived");
}

/// Waits for the next update that carries dialogs.
pub(super) async fn next_dialogs(
    sub: &mut norte_ui_host::UiSubscription,
) -> Vec<norte_ui_host::dto::DialogView> {
    for _ in 0..20 {
        let next = tokio::time::timeout(WAIT_MAX, sub.recv())
            .await
            .expect("an update with dialogs, not a hang")
            .expect("the host is still alive");
        match next {
            Update::Message(m) => {
                if let UiUpdate::Patch(p) = &m.payload {
                    for c in &p.changes {
                        if let norte_ui_host::dto::ViewChange::Dialogs { dialogs: d } = c {
                            return d.clone();
                        }
                    }
                }
            }
            Update::Lagged => panic!("no lag in this test"),
        }
    }
    panic!("no update with dialogs ever arrived");
}

/// Deleting does NOT delete: it opens the confirmation, and the destructive
/// answer comes marked as such so the renderer does not have to guess which
/// one it is.
#[tokio::test]
async fn deleting_asks_for_confirmation_before_touching_anything() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F8")).await.expect("host alive");
    let dialogs = next_dialogs(&mut sub).await;
    assert_eq!(dialogs.len(), 1, "ONE dialog opens");
    assert!(
        dialogs[0].choices.iter().any(|c| c.destructive),
        "and it says which of the answers destroys"
    );
    assert!(
        backend.deleted.lock().expect("borrados").is_empty(),
        "opening the dialog deletes nothing"
    );
}

/// Confirming twice with the SAME id does not delete twice: the second one
/// is a race in the renderer, not a second order.
#[tokio::test]
async fn confirming_twice_does_not_delete_twice() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F8")).await.expect("host alive");
    let dialogs = next_dialogs(&mut sub).await;
    let id = dialogs[0].id;

    let first = h
        .dispatch(UiAction::Dialog {
            id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
    assert!(matches!(first, ActionAck::Applied { .. }));

    let second = h
        .dispatch(UiAction::Dialog {
            id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
    assert_eq!(
        second,
        ActionAck::Stale {
            reason: StaleAction::Modal
        },
        "the second confirm is a race, not an order"
    );

    // And only ONE delete was requested: it waits for the first, and lets
    // whatever was behind it run before counting.
    until(&backend, "the queued delete", |f| {
        (!f.deleted.lock().expect("borrados").is_empty()).then_some(())
    })
    .await;
    settle().await;
    assert_eq!(backend.deleted.lock().expect("borrados").len(), 1);
}

/// An answer the dialog did not offer is not interpreted: on a decision
/// surface there are no implicit answers.
#[tokio::test]
async fn an_answer_that_does_not_exist_is_not_interpreted() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F8")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    let ack = h
        .dispatch(UiAction::Dialog {
            id,
            choice: "borra-y-no-preguntes".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Modal
        }
    );
    assert!(backend.deleted.lock().expect("borrados").is_empty());
}

/// With the delete confirmed, the task shows up on the board and its
/// TERMINAL state arrives: an outcome that gets lost leaves the user staring
/// at progress that does not advance.
#[tokio::test]
async fn the_task_shows_up_and_its_outcome_is_not_lost() {
    let backend = fake_tree();
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

    let tasks = next_tasks(&mut sub).await;
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].task_id, 7);

    // The daemon finishes the task.
    let tx = backend
        .progress
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| {
        p.state = norte_proto::TaskState::Completed;
        p.bytes_done = 10;
    });
    let tasks = next_tasks(&mut sub).await;
    assert_eq!(
        tasks[0].state,
        norte_ui_host::dto::TaskStateView::Done,
        "the terminal state reaches the board"
    );
    assert_eq!(tasks[0].percent, Some(100));
}

/// A FINISHED task leaves the board on its own after ten seconds.
///
/// It used to stay until another one pushed it out via the row cap, so the
/// panel showed the session's history instead of what is happening now. It
/// is the same deadline as the TUI: two frontends that expire differently are
/// two answers to "is this still running?".
///
/// VIRTUAL clock (`start_paused`): the test does not wait ten seconds, it
/// skips them — when nobody has work, tokio advances to the next timer.
#[tokio::test(start_paused = true)]
async fn a_finished_task_leaves_the_board_on_its_own() {
    let backend = fake_tree();
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
    assert_eq!(next_tasks(&mut sub).await.len(), 1);

    let tx = backend
        .progress
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    let tasks = next_tasks(&mut sub).await;
    assert_eq!(
        tasks[0].state,
        norte_ui_host::dto::TaskStateView::Done,
        "it is first SEEN finished: the ✓ cannot slip by"
    );

    // The clock is advanced BY HAND instead of letting tokio jump on its
    // own: this file's helpers wait with a 500 ms deadline, and the
    // automatic jump goes to the CLOSEST timer — i.e. that deadline, not the
    // TTL, and the test would die of "hang" with nothing actually wrong.
    tokio::time::advance(std::time::Duration::from_secs(11)).await;
    // The TTL's `spawn` wakes up and sends its message; yielding lets it do
    // so BEFORE the `Resync` enters the same mailbox, which is served in
    // order.
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    // Until the board is empty: between the outcome and the expiry, other
    // board changes happen (the re-listing of the directory the delete left
    // stale publishes its own), and asserting on "the next one" would be
    // asserting on whichever comes first.
    let mut empty = false;
    for _ in 0..5 {
        if next_tasks(&mut sub).await.is_empty() {
            empty = true;
            break;
        }
    }
    assert!(empty, "the finished one expired and left the board");
}

/// Is there a processes panel placed in this snapshot?
fn hay_processes(snap: &norte_ui_host::ViewSnapshot) -> bool {
    snap.slots
        .iter()
        .any(|s| matches!(s, SlotView::Processes { .. }))
}

/// The processes panel opens only when work LASTS (ADR 0146) and leaves when
/// the row expires.
///
/// The gesture's two halves, and the second is the one that was missing: the
/// host only re-evaluated from `progress`, and once the last row expires no
/// more progress ever arrives — so a panel that opened on its own stayed put
/// for the rest of the session. The terminal did not have the bug because its
/// loop re-evaluates on every round; it was the kind of divergence ADR 0077
/// chases, and no test caught it because all of them looked at the FIRST
/// event.
///
/// Virtual clock, like the TTL one above and for the same reason.
#[tokio::test(start_paused = true)]
async fn the_processes_panel_opens_on_its_own_and_closes_when_the_row_expires() {
    let backend = fake_tree();
    let (h, snap) = host_tree(Arc::clone(&backend)).await;
    assert!(
        !hay_processes(&snap),
        "with nothing queued, the panel takes no room"
    );
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
    assert_eq!(next_tasks(&mut sub).await.len(), 1);

    let tx = backend
        .progress
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    // A progress tick: it is where the host re-evaluates the panel. Opening
    // it right at the LOG was tried and reverted — opening a panel
    // republishes the whole snapshot, and doing it on queuing would put it
    // in the middle of every operation the reader just requested; it is
    // written down in ADR 0115.
    tx.send_modify(|p| p.bytes_done = 1);
    // Before the burst LASTS, the panel does not open (ADR 0146): a copy
    // that finishes in a second is counted by the status bar.
    assert!(
        !hay_processes(&crate::sync::next_snapshot_after_resync(&h, &mut sub).await),
        "a burst that just started does not open the panel"
    );
    tokio::time::advance(std::time::Duration::from_millis(
        u64::try_from(norte_frontend::task_strip::PANEL_MS).expect("positive") + 100,
    ))
    .await;
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    // UNTIL it appears, not on the first snapshot: progress travels through
    // the actor's mailbox and `Resync` enters that same mailbox, so
    // asserting on "the next one" would be asserting on whichever arrives
    // first.
    let mut open = false;
    for _ in 0..6 {
        if hay_processes(&crate::sync::next_snapshot_after_resync(&h, &mut sub).await) {
            open = true;
            break;
        }
    }
    assert!(open, "it opened only once the work had already lasted");

    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    // UNTIL again, not "the next one": the loop above left a `Resync` in
    // flight, and its snapshot arrives between the outcome and whoever waits
    // for it.
    let mut finished = false;
    for _ in 0..6 {
        if next_tasks(&mut sub)
            .await
            .first()
            .is_some_and(|t| t.state == norte_ui_host::dto::TaskStateView::Done)
        {
            finished = true;
            break;
        }
    }
    assert!(
        finished,
        "it is first SEEN finished, with the panel still up"
    );

    // The same manual jump as the TTL test, and for the same reason.
    tokio::time::advance(std::time::Duration::from_secs(11)).await;
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    // Until it leaves: between the outcome and the expiry other things
    // happen (the re-listing of the directory the delete left stale), and
    // asserting on "the next snapshot" would be asserting on whichever
    // happens first.
    let mut closed = false;
    for _ in 0..6 {
        if !hay_processes(&crate::sync::next_snapshot_after_resync(&h, &mut sub).await) {
            closed = true;
            break;
        }
    }
    assert!(
        closed,
        "and it closed only once the last row expired: a panel that opens \
         on its own and never closes takes up a third of the screen to say \
         nothing is happening"
    );
}

/// ADR 0146: a copy that finishes before the threshold opens no panel NOR
/// paints the bar, but leaves the "✓" on the tasks item; and the "✓" leaves
/// on its own.
#[tokio::test(start_paused = true)]
async fn a_quick_task_leaves_the_checkmark_and_opens_no_panel() {
    let backend = fake_tree();
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
    assert_eq!(next_tasks(&mut sub).await.len(), 1);
    let tx = backend
        .progress
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    let tasks_item =
        |s: &norte_ui_host::ViewSnapshot| s.status_items.iter().find(|i| i.id == "tasks").cloned();
    let mut done = None;
    for _ in 0..6 {
        let snap = crate::sync::next_snapshot_after_resync(&h, &mut sub).await;
        assert!(!hay_processes(&snap), "a quick copy opens no panel");
        if let Some(t) = tasks_item(&snap) {
            done = Some(t);
            break;
        }
    }
    let done = done.expect("the tasks item says it finished");
    assert!(done.text.starts_with('✓'), "{:?}", done.text);
    assert!(done.progress.is_none(), "a ✓ carries no bar");

    tokio::time::advance(std::time::Duration::from_millis(
        u64::try_from(norte_frontend::task_strip::DONE_MS).expect("positive") + 100,
    ))
    .await;
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    let mut gone = false;
    for _ in 0..6 {
        if tasks_item(&crate::sync::next_snapshot_after_resync(&h, &mut sub).await).is_none() {
            gone = true;
            break;
        }
    }
    assert!(gone, "and the ✓ leaves on its own");
}

/// ADR 0146: after a daemon handoff, the PREVIOUS one's work leaves neither
/// the bar running nor the automatic panel open forever: those tasks are
/// never going to finish, because there is nobody left to finish them.
#[tokio::test(start_paused = true)]
async fn a_handoff_does_not_leave_the_bar_or_the_panel_hanging() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.foreign.lock().expect("ajenas") = Some(rx);
    let (evtx, evrx) = tokio::sync::mpsc::unbounded_channel();
    *fake.eventos.lock().expect("eventos") = Some(evrx);
    let (h, _snap) = host_tree(Arc::new(fake)).await;
    let mut sub = h.subscribe();
    let _p = inject_task_for(&tx, 7, norte_proto::TaskKind::Copy);
    next_tasks(&mut sub).await;
    tokio::time::advance(std::time::Duration::from_millis(
        u64::try_from(norte_frontend::task_strip::PANEL_MS).expect("positive") + 100,
    ))
    .await;
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    let mut open = false;
    for _ in 0..6 {
        if hay_processes(&crate::sync::next_snapshot_after_resync(&h, &mut sub).await) {
            open = true;
            break;
        }
    }
    assert!(open, "work that lasts opens the panel");

    evtx.send(norte_client::ConnEvent::GoingAway { reconnect: true })
        .expect("the host is listening");
    evtx.send(norte_client::ConnEvent::Restored)
        .expect("the host is listening");
    let mut clean = false;
    for _ in 0..8 {
        let snap = crate::sync::next_snapshot_after_resync(&h, &mut sub).await;
        if !hay_processes(&snap) && snap.status_items.iter().all(|i| i.id != "tasks") {
            clean = true;
            break;
        }
    }
    assert!(
        clean,
        "the previous daemon's task keeps neither the panel nor the bar alive"
    );
}

/// Cancelling is idempotent: asking twice is not an error.
#[tokio::test]
async fn cancelling_is_idempotent() {
    let backend = fake_tree();
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
    next_tasks(&mut sub).await;

    for _ in 0..2 {
        let ack = h
            .dispatch(UiAction::CancelTask { task_id: 7 })
            .await
            .expect("host alive");
        assert!(matches!(ack, ActionAck::Applied { .. }));
    }
    assert_eq!(
        backend.cancellations.load(Ordering::SeqCst),
        2,
        "both requests arrive; the idempotency contract belongs to the daemon"
    );
}

/// Cancelling a task the board does not know about is a race, not an error.
#[tokio::test]
async fn cancelling_what_does_not_exist_is_a_race() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let ack = h
        .dispatch(UiAction::CancelTask { task_id: 999 })
        .await
        .expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Generation
        }
    );
}

/// The incremental search opens through its command, keeps TEXT keys, and
/// filters the listing. It is the listing's input context: letting the
/// resolver keep the "d" would turn typing into deleting.
#[tokio::test]
async fn the_quick_search_keeps_the_text_and_filters() {
    let (host, _snap) = host_tree(fake_tree()).await;
    let mut sub = host.subscribe();

    host.dispatch(press("/")).await.expect("host alive");
    host.dispatch(UiAction::Resync).await.expect("host alive");
    let open = next_snapshot(&mut sub).await;
    assert!(listing(&open).quick.is_some(), "the quick search is open");

    // Typing does NOT run commands: it filters.
    for c in ["n", "o"] {
        host.dispatch(press(c)).await.expect("host alive");
    }
    host.dispatch(UiAction::Resync).await.expect("host alive");
    let filtered = next_snapshot(&mut sub).await;
    let quick = listing(&filtered).quick.clone().expect("still open");
    assert_eq!(quick.query, "no");
    assert_eq!(quick.matches, 1, "only `notas.txt` matches");

    // And Esc closes it without touching the listing.
    host.dispatch(press("Escape")).await.expect("host alive");
    host.dispatch(UiAction::Resync).await.expect("host alive");
    let closed = next_snapshot(&mut sub).await;
    assert!(listing(&closed).quick.is_none());
    assert_eq!(listing(&closed).rows.len(), 3, "the listing is still whole");
}

/// Losing the daemon is painted AND said: noticing it only through an icon
/// is not enough when it happens mid-operation.
#[tokio::test]
async fn the_lost_connection_is_painted_and_said() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.eventos.lock().expect("eventos") = Some(rx);
    let (host, snap) = host_tree(Arc::new(fake)).await;
    assert_eq!(
        snap.connection,
        norte_ui_host::dto::ConnectionView::Connected
    );

    let mut sub = host.subscribe();
    tx.send(norte_client::ConnEvent::Lost)
        .expect("the host is listening");

    let mut view = None;
    let mut said = false;
    for _ in 0..10 {
        match tokio::time::timeout(WAIT_MAX, sub.recv())
            .await
            .expect("arrives")
            .expect("the host is still alive")
        {
            Update::Message(m) => match &m.payload {
                UiUpdate::Patch(p) => {
                    for c in &p.changes {
                        if let norte_ui_host::dto::ViewChange::Connection(v) = c {
                            view = Some(v.clone());
                        }
                    }
                }
                UiUpdate::Notice(norte_ui_host::dto::UiNotice::Message { key, .. }) => {
                    if key == "msg-daemon-lost" {
                        said = true;
                    }
                }
                UiUpdate::Snapshot(_) | UiUpdate::Notice(_) => {}
            },
            Update::Lagged => {}
        }
        if view.is_some() && said {
            break;
        }
    }
    assert_eq!(
        view,
        Some(norte_ui_host::dto::ConnectionView::Reconnecting),
        "it paints reconnecting"
    );
    assert!(said, "and it is said");
}

/// A task launched by ANOTHER client of the same session shows up on the
/// board, and the board says it is foreign: an operation nobody asked for
/// and that is indistinguishable from one's own is a surprise.
#[tokio::test]
async fn a_foreign_task_is_visible_and_said_to_be_foreign() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.foreign.lock().expect("ajenas") = Some(rx);
    let (host, _snap) = host_tree(Arc::new(fake)).await;
    let mut sub = host.subscribe();

    let progress = norte_proto::TaskProgress {
        task_id: norte_proto::TaskId::new(11),
        kind: norte_proto::TaskKind::Copy,
        state: norte_proto::TaskState::Running,
        bytes_done: 0,
        bytes_total: None,
        entries_done: 0,
        entries_total: None,
        current: None,
        unreadable: None,
        unvisited: None,
    };
    let (_ptx, prx) = tokio::sync::watch::channel(progress);
    tx.send(norte_ui_host::backend::HostTask {
        id: norte_proto::TaskId::new(11),
        progress: prx,
        cancel: Arc::new(|| {}),
        pause: None,
        cola: None,
        foreign: true,
    })
    .expect("the host is listening");

    let tasks = next_tasks(&mut sub).await;
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].task_id, 11);
    assert!(tasks[0].foreign, "the board says it is foreign");
}

/// Configured columns arrive as cells, with the SAME format the TUI paints,
/// and absence travels as absence: a directory with no size carries no
/// manufactured `0`.
#[tokio::test]
async fn configured_columns_arrive_as_cells() {
    let (_h, snap) = host_tree(fake_tree()).await;
    let rows = &listing(&snap).rows;
    let dir = rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("the directory is there");
    let file = rows
        .iter()
        .find(|r| r.display_name == "notas.txt")
        .expect("the file is there");

    let columns: Vec<&str> = file.cells.iter().map(|c| c.column.as_str()).collect();
    assert_eq!(columns, vec!["size", "mtime"], "name aside, the rest here");

    let size_dir = dir
        .cells
        .iter()
        .find(|c| c.column == "size")
        .expect("the cell exists");
    assert_eq!(
        size_dir.text, None,
        "a dir with no size does not invent a zero"
    );

    let size_file = file
        .cells
        .iter()
        .find(|c| c.column == "size")
        .expect("the cell exists");
    assert!(
        size_file.text.is_some(),
        "and a file with a size brings it formatted"
    );
}

/// Create directory: the dialog carries a TEXT FIELD, what is typed travels,
/// and confirming queues the task.
#[tokio::test]
async fn creating_a_directory_types_and_queues() {
    let backend = fake_tree();
    let (host, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = host.subscribe();

    host.dispatch(press("F7")).await.expect("host alive");
    let dialogs = next_dialogs(&mut sub).await;
    let id = dialogs[0].id;
    assert_eq!(
        dialogs[0].input.as_deref(),
        Some(""),
        "the dialog says this is where you type"
    );

    host.dispatch(UiAction::DialogInput {
        id,
        text: "carpeta nueva".to_owned(),
    })
    .await
    .expect("host alive");
    let typed = next_dialogs(&mut sub).await;
    assert_eq!(typed[0].input.as_deref(), Some("carpeta nueva"));

    host.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let created = until(&backend, "the queued creation", |f| {
        let c = f.created.lock().expect("creados").clone();
        (!c.is_empty()).then_some(c)
    })
    .await;
    assert_eq!(created.len(), 1, "one creation got queued");
    assert!(
        created[0].to_wire().ends_with("carpeta nueva"),
        "with the typed name: {}",
        created[0].to_wire()
    );
}

/// A name that is not valid queues nothing and says so.
#[tokio::test]
async fn an_invalid_name_creates_nothing() {
    let backend = fake_tree();
    let (host, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = host.subscribe();
    host.dispatch(press("F7")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;

    host.dispatch(UiAction::DialogInput {
        id,
        text: "..".to_owned(),
    })
    .await
    .expect("host alive");
    host.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    settle().await;
    assert!(
        backend.created.lock().expect("creados").is_empty(),
        "`..` is not a directory name"
    );
}

/// Typing into a DECISION dialog is not interpreted: it has nowhere to go.
#[tokio::test]
async fn typing_is_not_interpreted_in_a_decision_dialog() {
    let (host, _snap) = host_tree(fake_tree()).await;
    let mut sub = host.subscribe();
    host.dispatch(press("F8")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    let ack = host
        .dispatch(UiAction::DialogInput {
            id,
            text: "lo que sea".to_owned(),
        })
        .await
        .expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Modal
        }
    );
}

/// A policy approval opens its dialog, with the paths SANITIZED and saying
/// whether the list comes trimmed. Approving is a security decision: it
/// comes marked as destructive and has no default answer.
#[tokio::test]
async fn an_approval_opens_its_dialog() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.approvals.lock().expect("aprobaciones") = Some(rx);
    let backend = Arc::new(fake);
    let (host, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = host.subscribe();

    tx.send(norte_proto::methods::PolicyApprovalRequired {
        approval_id: 5,
        session: Some("agente-1".to_owned()),
        op: "delete".to_owned(),
        // With a control character inside: the dialog masks it, never
        // paints it.
        paths: vec!["mem:///casa/borra\u{202E}me".to_owned()],
        paths_total: 40,
        ttl_ms: 30_000,
        detail: norte_proto::methods::ApprovalDetail::default(),
    })
    .expect("the host is listening");

    let dialogs = next_dialogs(&mut sub).await;
    assert_eq!(dialogs.len(), 1);
    let d = &dialogs[0];
    assert!(
        d.choices.iter().any(|c| c.id == "approve" && c.destructive),
        "approving an agent's op is destructive and it is said"
    );
    assert!(
        d.body.iter().all(|l| !l.text.contains('\u{202E}')),
        "paths go masked: {:?}",
        d.body
    );
    assert!(
        d.body.iter().any(|l| l.hostile),
        "and it SAYS which one paints differently from what it is: {:?}",
        d.body
    );
    assert!(
        !d.overflow_note.is_empty(),
        "and that the list comes trimmed, in its own field: {d:?}"
    );
    // The one path that does arrive IS SHOWN, so the summary marks nothing:
    // the trim's badge speaks of what CANNOT be looked at, and here what got
    // trimmed was trimmed by the server and never arrived.
    assert!(
        !d.overflow_hostile,
        "with no hidden paths to look at, the summary marks nothing: {d:?}"
    );
    // And it says how much time is left, in its own field: a decision with
    // an expiry that does not show it reads as one that waits forever, and
    // among the paths a file name could impersonate it.
    assert_eq!(
        d.deadline.as_deref(),
        Some(
            norte_i18n::ta_in(norte_i18n::Lang::Es, "modal-approval-ttl", &[("s", "30")]).as_str()
        )
    );
}

/// And a hostile path left OUTSIDE what is shown is said.
///
/// A visible path's badge says "what you read is not the bytes there are".
/// About the trimmed ones that cannot be said — it is not there to look
/// at — but it can be said that there is something like that out there, and
/// that is what decides whether expanding before approving is worth it. The
/// terminal has always said so in its summary and this window did not, over
/// the same paths (parity plan, 14).
#[tokio::test]
async fn an_approvals_summary_gives_away_a_hidden_hostile_path() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.approvals.lock().expect("aprobaciones") = Some(rx);
    let backend = Arc::new(fake);
    let (host, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = host.subscribe();

    // More paths than the dialog shows, with the hostile one at the TAIL.
    // The ceiling belongs to the host and is not exported; thirty goes well
    // past any reasonable value, which is what is needed.
    let mut paths: Vec<String> = (0..30).map(|i| format!("mem:///casa/f{i}")).collect();
    let last = paths.len() - 1;
    paths[last] = "mem:///casa/x\u{202E}y".to_owned();
    let total = paths.len() as u64;

    tx.send(norte_proto::methods::PolicyApprovalRequired {
        approval_id: 7,
        session: Some("agente-1".to_owned()),
        op: "delete".to_owned(),
        paths,
        paths_total: total,
        ttl_ms: 30_000,
        detail: norte_proto::methods::ApprovalDetail::default(),
    })
    .expect("the host is listening");

    let dialogs = next_dialogs(&mut sub).await;
    let d = &dialogs[0];
    assert!(!d.overflow_note.is_empty(), "the list comes trimmed: {d:?}");
    assert!(
        d.body.iter().all(|l| !l.hostile),
        "the ones that ARE SHOWN are all clean, so the badge does not come from there: {:?}",
        d.body
    );
    assert!(
        d.overflow_hostile,
        "and the summary gives away the one that is not visible: {d:?}"
    );
}

/// Denying is the default: any answer that is not approving denies, and so
/// does closing the dialog. Leaving the agent waiting would be worse than
/// telling it no.
#[tokio::test]
async fn any_answer_other_than_approve_denies() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.approvals.lock().expect("aprobaciones") = Some(rx);
    let backend = Arc::new(fake);
    let (host, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = host.subscribe();

    tx.send(norte_proto::methods::PolicyApprovalRequired {
        approval_id: 9,
        session: None,
        op: "copy".to_owned(),
        paths: vec!["mem:///casa/x".to_owned()],
        paths_total: 1,
        ttl_ms: 30_000,
        detail: norte_proto::methods::ApprovalDetail::default(),
    })
    .expect("the host is listening");
    let id = next_dialogs(&mut sub).await[0].id;

    host.dispatch(UiAction::Dialog {
        id,
        choice: "deny".to_owned(),
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
        vec![(9, false)],
        "it is denied, and the daemon is told"
    );
}

/// With a catalogue, a numeric `attr:` is painted as what it IS: a mode
/// reads `rwx`, not `33188`.
#[tokio::test]
async fn the_catalogue_gives_meaning_to_an_attr() {
    let fake = tree_as_fake();
    *fake.catalog.lock().expect("catalog") =
        norte_proto::AttrCatalog::new(vec![norte_proto::attrs::AttrInfo {
            id: "posix.mode".to_owned(),
            label: "modo".to_owned(),
            ty: norte_proto::attrs::AttrType::Uint,
            hint: norte_proto::attrs::AttrHint::Mode,
        }]);
    let backend = Arc::new(fake);
    let (host, _snap) = UiHost::start(UiHostOptions {
        backend,
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
        columns: columns_of(&["name", "attr:posix.mode"]),
        effects: norte_ui_host::commands::Effects::Full,
        log_ring: None,
    })
    .await
    .expect("starts");

    // The catalogue arrives after the first listing and carries its own
    // snapshot.
    let mut sub = host.subscribe();
    let snap = next_snapshot(&mut sub).await;
    let row = listing(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "notas.txt")
        .expect("the file is there");
    let cell = row
        .cells
        .iter()
        .find(|c| c.column == "attr:posix.mode")
        .expect("the cell exists");
    assert_eq!(
        cell.text.as_deref(),
        Some("-rw-r--r--"),
        "the catalogue converts the number into a readable mode"
    );
}
