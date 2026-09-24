//! The UI session as seen from the frontend (L2): what gets captured, what
//! gets applied, and what does NOT travel.
//!
//! The rule these tests pin is that capturing and applying are the same
//! screen said twice. Everything else is the three things decided on
//! purpose: marks do not travel, the state of a slot the layout does not
//! have is kept, and an unreadable body does not leave a blank screen.

use norte_frontend::layout::{KindId, Node, SlotId};
use norte_proto::VPath;
use norte_tui::app::{App, Pane};

fn vp(wire: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("valid wire")
}

fn app_basica() -> App {
    App::new(
        Pane::new(vp("file:///izq"), Vec::new()),
        Pane::new(vp("file:///der"), Vec::new()),
    )
}

/// Capturing and reapplying leaves the same screen: same tree, same paths,
/// same cursor.
#[test]
fn capture_and_apply_is_the_identity() {
    let mut app = app_basica();
    app.set_layout(norte_frontend::layout::presets::tree("krusader").expect("preset"));
    let before = app.session_body();
    let mut other = app_basica();
    other.apply_session(&before);
    assert_eq!(other.session_body(), before);
}

/// This process looks at ONE profile and the document belongs to all of
/// them: what belongs to the others travels back intact.
///
/// Writing only the active one would erase from the body the place where
/// other profiles left their panels, and the reader would find out by going
/// back to one and finding it blank (ADR 0079, D5).
#[test]
fn other_profiles_layouts_come_back_intact() {
    let foreign = norte_frontend::layout::presets::tree("krusader").expect("preset");
    let mut body = norte_frontend::session::SessionBody::default();
    body.layouts.insert("photos".to_owned(), foreign.clone());

    let mut app = app_basica();
    app.active_profile = Some(std::ffi::OsString::from("work"));
    app.apply_session(&body);
    let return_ = app.session_body();

    assert_eq!(
        return_.layouts.get("photos"),
        Some(&foreign),
        "\"photos\"'s is untouched"
    );
    assert!(
        return_.layouts.contains_key("work"),
        "and this process's goes under ITS name, not under `default`: {:?}",
        return_.layouts.keys().collect::<Vec<_>>()
    );
    assert_eq!(return_.active, "work");
}

/// ADR 0139: the terminal does not overwrite the WINDOW's layout when
/// writing its own — each remembers its own sizes — and on a HANDOFF it
/// hands the window its own too, under the window's key.
#[test]
fn the_windows_layout_is_respected_and_the_handoff_delivers_it() {
    let of_the_window = norte_frontend::layout::presets::tree("krusader").expect("preset");
    let mut body = norte_frontend::session::SessionBody::default();
    body.layouts
        .insert("default@window".to_owned(), of_the_window.clone());
    let mut app = app_basica();
    app.apply_session(&body);
    assert_eq!(
        app.session_body().layouts.get("default@window"),
        Some(&of_the_window),
        "writing its own does not touch the window's"
    );
    let handoff = app.session_body_for_handoff();
    assert_eq!(
        handoff.layouts.get("default@window"),
        handoff.layouts.get("default"),
        "the handoff hands THIS screen to the window"
    );
}

/// The STICKY profile arrives with the session and requests the switch.
///
/// It cannot be applied earlier: it lives in the session, the daemon holds
/// the session, and the daemon is reached with the config that is already
/// loaded. So it is requested and the loop does it through the same path as
/// any other change.
#[test]
fn the_sticky_profile_asks_for_the_change() {
    let body = norte_frontend::session::SessionBody {
        active: "work".to_owned(),
        ..norte_frontend::session::SessionBody::default()
    };
    let mut app = app_basica();
    app.apply_session_value(norte_frontend::session::SCHEMA_VERSION, &body.to_value());
    assert_eq!(
        app.pending_profile.as_deref(),
        Some(std::ffi::OsStr::new("work"))
    );
}

/// And `--profile` WINS: the reader named one for this time, so the one
/// that came from the session does not override it.
#[test]
fn an_explicit_profile_wins_over_the_sticky_one() {
    let body = norte_frontend::session::SessionBody {
        active: "photos".to_owned(),
        ..norte_frontend::session::SessionBody::default()
    };
    let mut app = app_basica();
    app.active_profile = Some(std::ffi::OsString::from("work"));
    app.apply_session_value(norte_frontend::session::SCHEMA_VERSION, &body.to_value());
    assert_eq!(app.pending_profile, None, "no change is requested");
    assert_eq!(
        app.active_profile.as_deref(),
        Some(std::ffi::OsStr::new("work"))
    );
}

/// `ntc <DIR>` wins over the saved session on the active pane, and ONLY
/// there: the rest of the screen comes back as it was. It used to be that
/// the session overrode the argument and `ntc ~/project` opened where it
/// was last closed.
#[test]
fn an_explicit_directory_wins_over_the_session_in_the_active_pane() {
    let mut app = app_basica();
    let focus = app.focus();
    let active = app.panes.slot_of(focus);
    let mut spec = app.panes[focus].sort();
    spec.dirs_first = !spec.dirs_first;
    app.panes[focus].set_sort(spec.clone());
    let body = app.session_body();

    let mut other = app_basica();
    let ask = other.apply_session(&body);
    other.pin_start_dir(vp("file:///pedido"));
    let focus = other.focus();
    assert_eq!(other.panes[focus].dir(), &vp("file:///pedido"));
    assert_eq!(
        other.panes[focus].sort(),
        spec,
        "the saved order is kept: only the directory changes"
    );
    assert_eq!(
        other.panes[1 - focus].dir(),
        &vp("file:///der"),
        "the other panel is the session's"
    );
    assert!(ask.contains(&active), "the slot still requests its listing");
}

/// With no profile, the key is still `default`: whoever never picks one
/// reads and writes exactly where they already did.
#[test]
fn without_a_profile_the_key_is_still_default() {
    let app = app_basica();
    let body = app.session_body();
    assert!(body.layouts.contains_key("default"));
    assert_eq!(body.active, "");
}

/// History travels: `nav.back` still works after a restart.
#[test]
fn the_trail_back_survives() {
    let mut app = app_basica();
    let slot = app.panes.slot_of(0);
    app.history.for_slot_mut(slot).record(vp("file:///antes"));
    let body = app.session_body();
    assert_eq!(body.slots[&slot.0].back, vec![vp("file:///antes")]);

    let mut other = app_basica();
    other.apply_session(&body);
    assert_eq!(
        other.history.for_slot(slot).expect("history").trail(),
        [vp("file:///antes")]
    );
}

/// Restoring a session does NOT turn off the `..` row.
///
/// The bug this closes: `apply_session` replaced the whole pane and put
/// back only the order and the hidden flag, so `[ui] parent_entry = true`
/// silently turned into `false` from the first saved session onward. The
/// reader saw it as "the TUI has no parent row and the window does," which
/// is the same setting saying two things.
#[test]
fn restoring_a_session_does_not_turn_off_the_parent_row() {
    let mut app = app_basica();
    app.set_parent_row(true);
    let slot = app.panes.slot_of(0);
    assert!(
        app.panes.browser(slot).expect("listing").is_parent_row(0),
        "it starts with it"
    );

    let body = app.session_body();
    let mut other = app_basica();
    other.set_parent_row(true);
    other.apply_session(&body);
    assert!(
        other.panes.browser(slot).expect("listing").is_parent_row(0),
        "and after applying the session, too"
    );
}

/// And a listing born FROM OUTSIDE — the one startup brings for each
/// session slot — gets it through the same door.
///
/// That was the same hole's other half: `restore_slots` replaced the pane
/// with a freshly listed one and gave it back the order and the hidden
/// flag, never the row. A single door to adopt a foreign listing, and both
/// were covered by it.
#[test]
fn an_adopted_listing_arrives_with_the_parent_row() {
    let mut app = app_basica();
    app.set_parent_row(true);
    let slot = app.panes.slot_of(0);
    app.adoptar_pane(slot, Pane::new(vp("file:///izq"), Vec::new()), None, None);
    assert!(app.panes.browser(slot).expect("listing").is_parent_row(0));

    app.set_parent_row(false);
    app.adoptar_pane(slot, Pane::new(vp("file:///izq"), Vec::new()), None, None);
    assert!(
        !app.panes.browser(slot).expect("listing").is_parent_row(0),
        "and off it does not sneak in either"
    );
}

/// Marks do NOT travel: they are the state of an operation, not of a
/// session.
#[test]
fn marks_do_not_travel() {
    let mut app = app_basica();
    app.focused_mut().mark_all();
    let v = serde_json::to_string(&app.session_body().to_value()).expect("json");
    assert!(!v.contains("mark"), "{v}");
}

/// A session that mentions a slot this layout does not have breaks
/// nothing: its state is saved and what the layout says gets painted.
#[test]
fn a_slot_the_layout_does_not_have_does_not_break_the_app() {
    let mut app = app_basica();
    let mut body = app.session_body();
    let one = body.slots.values().next().expect("one").clone();
    body.slots.insert(99, one);
    app.apply_session(&body);
    assert!(app.session_body().slots.contains_key(&99), "it is kept");
}

/// An ORPHANED slot — one the store still has and the layout no longer
/// does — is kept as is, not readopted.
///
/// The question "does this layout have the slot?" was asked of the panes
/// store, and orphans stay there: it answered yes, so the slot went through
/// the adoption door — losing the state the session had saved for it —
/// instead of being written back intact. Returning to yesterday's layout
/// has to give the panel back where it was.
#[test]
fn an_orphan_slot_from_the_store_is_preserved_instead_of_being_readopted() {
    let mut app = app_basica();
    // A single-slot layout: the second one is orphaned in the store.
    let solo = norte_frontend::layout::presets::tree("simple").expect("preset");
    app.set_layout(solo);
    let outside = SlotId(2);
    assert!(
        app.panes.browser(outside).is_some(),
        "the store keeps the orphan"
    );
    assert!(
        !app.layout.slot_ids().contains(&outside),
        "and the layout no longer places it"
    );

    let mut body = app.session_body();
    let mut state = body.slots.values().next().expect("one").clone();
    state.path = vp("file:///de-ayer");
    body.slots.insert(outside.0, state);
    app.apply_session(&body);

    assert_eq!(
        app.session_body().slots.get(&outside.0).map(|s| &s.path),
        Some(&vp("file:///de-ayer")),
        "it is written back as is, without going through the pane"
    );
}

/// A corrupt body does not leave a blank screen: it is ignored and the
/// config's layout stays, with a warning. And it KEEPS getting written: what
/// was unreadable is already lost, and never saving again would be worse.
#[test]
fn a_corrupt_body_leaves_the_config_screen() {
    let mut app = app_basica();
    let before = app.layout.clone();
    // Truly malformed: a slot with no `path`, the only field with no
    // default.
    app.apply_session_value(
        norte_frontend::session::SCHEMA_VERSION,
        &serde_json::json!({ "slots": { "1": { "cursor": 3 } } }),
    );
    assert_eq!(app.layout, before);
    assert!(app.message.is_some(), "and it says so");
    assert!(!app.session.detached, "and this window keeps writing");
}

/// The ENVELOPE's version leaves the window detached the same as the
/// inside one does (#247).
///
/// The body carried an undocumented copy of the version, and it was the
/// only one read: a foreign client doing what the contract says — `version`
/// in the envelope, v2 body — reached a reader that saw it absent, took it
/// for 0, swallowed the fields it did not understand and rewrote them lost.
#[test]
fn the_envelope_version_also_leaves_the_window_detached() {
    let mut app = app_basica();
    let before = app.layout.clone();
    // A body with NO copy inside, which is what a client following the
    // documented contract writes.
    app.apply_session_value(
        norte_frontend::session::SCHEMA_VERSION + 1,
        &serde_json::json!({ "layouts": {}, "slots": {} }),
    );
    assert_eq!(app.layout, before, "what cannot be read is not applied");
    assert!(app.session.detached, "and above all it is not overwritten");
    assert!(app.message.is_some(), "and it says so");
}

/// A body from a NEWER version leaves this window DETACHED: it is not
/// read, and above all, it is not written back over. Without this the
/// warning showed and a second later the dump published the config's
/// screen over the new binary's session.
#[test]
fn a_body_from_the_future_leaves_the_window_detached() {
    let mut app = app_basica();
    let before = app.layout.clone();
    app.apply_session_value(
        norte_frontend::session::SCHEMA_VERSION,
        &serde_json::json!({ "version": 999 }),
    );
    assert_eq!(app.layout, before);
    assert!(
        app.session.detached,
        "what cannot be read is not overwritten"
    );
    assert!(app.message.is_some(), "and it says so");
}

/// The two schema versions — the core's, which decides whether a file can
/// be overwritten, and the frontend's, which decides whether a body can be
/// read — go hand in hand.
///
/// They live in different crates because the core cannot depend on the
/// frontend, and nothing ties them at compile time: bumping ONLY the
/// frontend's makes the core refuse its own file on every startup, forever
/// and behind a `warn!`. This is the only crate that sees both.
#[test]
fn the_two_schema_versions_go_arm_in_arm() {
    assert_eq!(
        norte_core::ui_session::disk::SCHEMA_VERSION,
        norte_frontend::session::SCHEMA_VERSION,
        "if you bump one, bump the other"
    );
}

/// The cursor is placed when the listing arrives, not before: on an empty
/// pane row 12 is row 0, and applying it there would lose it.
#[test]
fn the_cursor_waits_for_its_listing() {
    let mut app = app_basica();
    let slot = app.panes.slot_of(0);
    let mut body = app.session_body();
    body.slots.get_mut(&slot.0).expect("slot").cursor = 2;
    app.apply_session(&body);
    assert_eq!(
        app.panes[0].cursor(),
        0,
        "with no listing there is nowhere to put it"
    );

    let entries: Vec<norte_proto::Entry> = ["a", "b", "c", "d"]
        .iter()
        .map(|n| norte_proto::Entry {
            path: vp(&format!("file:///izq/{n}")),
            kind: norte_proto::EntryKind::File,
            size: Some(0),
            mtime_ms: None,
            attrs: std::collections::BTreeMap::new(),
        })
        .collect();
    app.panes[0].begin_listing(vp("file:///izq"), entries, false, None);
    app.restore_cursor(slot);
    assert_eq!(app.panes[0].cursor(), 2);
}

/// A handoff's MARKS wait for their listing, like the cursor — and only
/// come back with `--attach` (phase 9).
///
/// Both halves came out of piloting it, and the first is a bug a green
/// test would not have seen: seeding them on session apply looked like it
/// worked — the mark set is by path, not by index — but `begin_listing`
/// clears them when the listing arrives, which is correct for a cd and
/// fatal for a seed made too early. The handoff returned the screen with
/// nothing marked, silently.
#[test]
fn a_handoffs_marks_wait_for_their_listing_and_only_with_attach() {
    fn listing(app: &mut norte_tui::app::App) {
        let entries: Vec<norte_proto::Entry> = ["a", "b", "c"]
            .iter()
            .map(|n| norte_proto::Entry {
                path: vp(&format!("file:///izq/{n}")),
                kind: norte_proto::EntryKind::File,
                size: Some(0),
                mtime_ms: None,
                attrs: std::collections::BTreeMap::new(),
            })
            .collect();
        app.panes[0].begin_listing(vp("file:///izq"), entries, false, None);
    }

    // WITHOUT `--attach`: an ordinary startup is not a handoff, and
    // yesterday's selection is not revived.
    let mut app = app_basica();
    let slot = app.panes.slot_of(0);
    let mut body = app.session_body();
    body.slots.get_mut(&slot.0).expect("slot").path = vp("file:///izq");
    body.slots.get_mut(&slot.0).expect("slot").marks = vec![vp("file:///izq/a")];
    app.apply_session(&body);
    listing(&mut app);
    app.restore_cursor(slot);
    assert_eq!(
        app.panes[0].marks_len(),
        0,
        "without --attach a startup does not return what was marked"
    );

    // WITH `--attach`: it comes back, and after the listing.
    let mut app = app_basica();
    app.session.attach = true;
    app.apply_session(&body);
    assert_eq!(
        app.panes[0].marks_len(),
        0,
        "not yet: the pane is empty and the listing would clear them"
    );
    listing(&mut app);
    app.restore_cursor(slot);
    assert_eq!(
        app.panes[0].marks_len(),
        1,
        "and now it does, with its listing"
    );
}

/// A slot whose layout DOES exist gets back its directory and its order.
#[test]
fn the_directory_and_the_order_come_back() {
    let mut app = app_basica();
    let slot = app.panes.slot_of(0);
    let mut spec = app.panes[0].sort();
    spec.dirs_first = !spec.dirs_first;
    app.panes[0].set_sort(spec.clone());
    app.panes[0].set_show_hidden(false);
    let body = app.session_body();

    let mut other = app_basica();
    let ask = other.apply_session(&body);
    assert!(ask.contains(&slot), "it needs listing");
    assert_eq!(other.panes[0].dir(), &vp("file:///izq"));
    assert_eq!(other.panes[0].sort(), spec);
    assert!(!other.panes[0].show_hidden());
}

/// `[profile.start]` opens the slot the session knows nothing about.
///
/// It is what makes a freshly created profile useful, or one arriving from
/// another machine. Both frontends wrote the key and NEITHER read it, with
/// two files promising they did.
#[test]
fn profile_start_seeds_a_slot_without_a_session() {
    let mut app = app_basica();
    let body = norte_frontend::session::SessionBody::default();
    // Through `apply_session_value`, the real path: it is where the
    // terminal notes which slots it KNOWS were saved, and that note is the
    // veto. Calling the seeder with a hand-built body would only test the
    // harness.
    app.apply_session_value(norte_frontend::session::SCHEMA_VERSION, &body.to_value());
    let start = std::collections::BTreeMap::from([(1, vp("file:///fotos"))]);

    let seeded = app.seed_profile_start(&start);

    assert_eq!(seeded, vec![SlotId(1)]);
    assert_eq!(
        app.panes
            .browser(SlotId(1))
            .expect("there is a listing")
            .dir(),
        &vp("file:///fotos")
    );
}

/// And only the FIRST time. With no saved session — a fresh install — the
/// session never knows anything about any slot, so without keeping count of
/// what was seeded, entering and leaving the profile pulled the reader away
/// from where they were on every round: ADR 0098's decision 2, upside down.
#[test]
fn an_already_seeded_slot_is_not_seeded_again() {
    let mut app = app_basica();
    let body = norte_frontend::session::SessionBody::default();
    app.apply_session_value(norte_frontend::session::SCHEMA_VERSION, &body.to_value());
    let start = std::collections::BTreeMap::from([(1, vp("file:///fotos"))]);
    assert_eq!(app.seed_profile_start(&start), vec![SlotId(1)]);

    // The reader goes somewhere else and re-enters the profile.
    app.adoptar_pane(
        SlotId(1),
        Pane::new(vp("file:///trabajo"), Vec::new()),
        None,
        None,
    );

    assert!(app.seed_profile_start(&start).is_empty());
    assert_eq!(
        app.panes
            .browser(SlotId(1))
            .expect("there is a listing")
            .dir(),
        &vp("file:///trabajo"),
        "seeding is a first-time-only thing"
    );
}

/// And the SESSION wins: `[profile.start]` says where a slot opens the
/// first time, not every time.
///
/// A profile is a workspace, not a bookmark that pulls you back to the
/// start: if every entry pulled you away from where you were, it would be
/// useless for exactly the people who use it daily.
#[test]
fn the_session_wins_over_profile_start() {
    let mut app = app_basica();
    let mut body = norte_frontend::session::SessionBody::default();
    body.slots.insert(
        1,
        norte_frontend::session::SlotState {
            path: vp("file:///donde/lo/dejaste"),
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
    app.apply_session_value(norte_frontend::session::SCHEMA_VERSION, &body.to_value());
    let start = std::collections::BTreeMap::from([(1, vp("file:///fotos"))]);

    assert!(
        app.seed_profile_start(&start).is_empty(),
        "a slot the session already places is not seeded"
    );
    assert_eq!(
        app.panes
            .browser(SlotId(1))
            .expect("there is a listing")
            .dir(),
        &vp("file:///donde/lo/dejaste")
    );
}

/// An id the profile names and this layout does not place has nowhere to
/// open: it falls silently instead of minting a slot nobody asked for.
#[test]
fn profile_start_does_not_invent_a_slot_the_layout_does_not_have() {
    let mut app = app_basica();
    let start = std::collections::BTreeMap::from([(4242, vp("file:///fotos"))]);
    assert!(app.seed_profile_start(&start).is_empty());
    assert!(app.panes.browser(SlotId(4242)).is_none());
}

/// And it does not overwrite an ORPHANED slot either: one the store keeps
/// and this layout does not place.
///
/// The store cannot be asked "does it exist?": it keeps orphans for when
/// its layout comes back, and `insert` REVIVES them. Seeding there, the
/// reader returns to that layout and finds the panel in the profile's
/// startup directory instead of where they left it — which is the promise
/// the store has written down. The LAYOUT is asked instead, the same fix
/// `apply_session` documents thirty lines up.
#[test]
fn profile_start_does_not_revive_an_orphan_slot() {
    let mut app = app_basica();
    // Slot 3 exists in the store and NOT in the layout.
    app.set_layout(Node::split(
        norte_frontend::layout::Dir::Vertical,
        vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(3), KindId::browser()),
        ],
    ));
    app.adoptar_pane(
        SlotId(3),
        Pane::new(vp("file:///lo/de/ayer"), Vec::new()),
        None,
        None,
    );
    app.set_layout(Node::slot(SlotId(1), KindId::browser()));

    let start = std::collections::BTreeMap::from([(3, vp("file:///fotos"))]);
    assert!(app.seed_profile_start(&start).is_empty());
    assert_eq!(
        app.panes.browser(SlotId(3)).map(Pane::dir),
        Some(&vp("file:///lo/de/ayer")),
        "the orphan stays where it was, for when its layout comes back"
    );
}

/// A layout this binary does not know how to paint still travels: the
/// session saves the tree, it does not interpret it.
#[test]
fn an_unknown_kind_travels_in_the_layout() {
    let mut app = app_basica();
    app.set_layout(Node::split(
        norte_frontend::layout::Dir::Vertical,
        vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(2), KindId::new("kind-de-otro-binario")),
        ],
    ));
    let body = app.session_body();
    let return_ = norte_frontend::session::SessionBody::from_value(
        norte_frontend::session::SCHEMA_VERSION,
        &body.to_value(),
    )
    .expect("parses");
    assert_eq!(return_.layouts["default"], app.layout);
}
