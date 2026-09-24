use super::*;

// ---------------------------------------------------------------------------
// The splash screen (spec 2026-09-15, ADR 0115): what puts it up, what takes
// it down, and what its footer promises.
// ---------------------------------------------------------------------------

/// A host with whichever splash mode is requested.
///
/// The other test constructors fix `test_settings()` internally, and
/// here what is being tested IS the configuration key: `brief` takes itself
/// down, `home` stays until someone touches it, and `off` puts up nothing.
async fn host_con_splash(
    backend: Arc<Fake>,
    mode: norte_config::load::SplashMode,
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    let mut settings = norte_ui_host::default_settings();
    settings.common.ui_parent_entry = Some(false);
    settings.common.ui_chrome.splash = Some(mode);
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
    .expect("starts")
}

/// `off` puts up nothing: the key is honored, not negotiated.
#[tokio::test]
async fn off_puts_up_no_screen() {
    let (h, _snap) = host_con_splash(fake_tree(), norte_config::load::SplashMode::Off).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SplashOpen).await.expect("host alive");
    let snap = crate::sync::next_snapshot_after_resync(&h, &mut sub).await;
    assert!(snap.splash.is_none(), "with `splash = off` nothing goes up");
}

/// `brief` carries its own DEADLINE, because the renderer is the one who
/// honors it.
///
/// On the window's side there is no event loop that wakes the host up —
/// that belongs to the terminal — so a screen that promises to be brief and
/// does not say how much time it has left would stay up until someone
/// pressed a key.
#[tokio::test]
async fn brief_says_how_much_time_is_left() {
    let (h, _snap) = host_con_splash(fake_tree(), norte_config::load::SplashMode::Brief).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SplashOpen).await.expect("host alive");
    let v = crate::sync::next_snapshot_after_resync(&h, &mut sub)
        .await
        .splash
        .expect("went up");
    let left = v.close_after_ms.expect("brief carries a deadline");
    assert!(
        left > 0 && i64::from(left) <= norte_frontend::splash::BRIEF_MS,
        "the deadline is what is LEFT, not an instant from another clock: {left}"
    );
    // And it carries no sections: it takes itself down, so a list of places
    // would be an offer withdrawn before it could be accepted.
    assert!(v.sections.is_empty(), "brief offers no places");
}

/// Any key takes it down, and means nothing else.
///
/// What is in front rules: typing that key into the listing behind it would
/// be acting on something the reader is not looking at.
#[tokio::test]
async fn any_key_takes_it_down_and_never_reaches_the_listing() {
    let (h, snap) = host_con_splash(fake_tree(), norte_config::load::SplashMode::Home).await;
    let before = listing(&snap).path_display.clone();
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SplashOpen).await.expect("host alive");
    assert!(
        crate::sync::next_snapshot_after_resync(&h, &mut sub)
            .await
            .splash
            .is_some(),
        "went up"
    );

    h.dispatch(press("j")).await.expect("host alive");
    let snap2 = crate::sync::next_snapshot_after_resync(&h, &mut sub).await;
    assert!(snap2.splash.is_none(), "the key took it down");
    assert_eq!(
        listing(&snap2).cursor,
        listing(&snap).cursor,
        "and the key did NOT move the listing's cursor behind it"
    );
    assert_eq!(
        listing(&snap2).path_display,
        before,
        "nor navigated anywhere"
    );
}

/// `home` offers places by number, and the number OPENS it.
///
/// The screen paints them numbered and its footer promises it; without this
/// the rows would be an offer that cannot be accepted.
#[tokio::test]
async fn at_home_a_digit_opens_its_row() {
    let (h, snap) = host_con_splash(fake_tree(), norte_config::load::SplashMode::Home).await;
    // A visit first: the list comes from where you USUALLY go, and a
    // freshly started host has gone nowhere.
    let b = listing(&snap);
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
    asentar().await;

    // The subscription goes AFTER navigating: the navigation's envelopes
    // stay queued, and `next_snapshot` would return a snapshot from before
    // the screen went up — green or red depending on what the landing left
    // behind, which is a test that proves nothing.
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SplashOpen).await.expect("host alive");
    let v = crate::sync::next_snapshot_after_resync(&h, &mut sub)
        .await
        .splash
        .expect("went up");
    let row = v
        .sections
        .iter()
        .flat_map(|s| s.rows.iter())
        .find(|f| f.number == 1)
        .expect("there is a numbered row");
    assert!(!row.label.is_empty(), "the row says where it goes");

    h.dispatch(press("1")).await.expect("host alive");
    let snap2 = crate::sync::next_snapshot_after_resync(&h, &mut sub).await;
    assert!(snap2.splash.is_none(), "opening a row also takes it down");
}
