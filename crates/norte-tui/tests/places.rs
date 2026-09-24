//! The places sidebar inside `App` (L3): opening it, focusing it, closing
//! it and, above all, NOT touching the listings while doing so.
//!
//! The rule these tests protect is spec rule 7: `app.panes[i]` keeps
//! meaning "the i-th LISTING". A sidebar is not a side, and the day it
//! were, a copy could end up targeting a drive list.

use norte_proto::methods::{Volume, VolumeKind};
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, KeyOwner, Pane};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn vp(wire: &str) -> VPath {
    // Fixed language: the snapshot freezes localized text.
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("valid wire")
}

fn entries(dir: &VPath) -> Vec<Entry> {
    (0..3)
        .map(|i| Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir
                .join(Segment::new(format!("f{i:02}").into_bytes()).expect("segment"))
                .clone(),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        })
        .collect()
}

fn test_app() -> App {
    let dir = vp("file:///casa");
    App::new(
        Pane::new(dir.clone(), entries(&dir)),
        Pane::new(dir.clone(), entries(&dir)),
    )
}

/// Opening the sidebar does not change how many LISTINGS there are, which
/// one has focus, or where its cursor is. It is spec rule 7: the sidebar is
/// not a side.
#[test]
fn opening_the_sidebar_does_not_touch_the_sides() {
    let mut app = test_app();
    let before = (app.panes.len(), app.focus(), app.focused().dir().clone());
    app.toggle_places();
    assert_eq!(app.panes.len(), before.0, "still two listings");
    assert_eq!(app.focus(), before.1);
    assert_eq!(*app.focused().dir(), before.2);
    assert!(app.places_slot().is_some());
    assert_eq!(app.key_owner(), KeyOwner::Places);
}

/// And closing it leaves the tree EXACTLY as it was: no degenerate `Split`
/// piling up every time someone opens and closes the sidebar.
#[test]
fn closing_the_sidebar_returns_the_previous_tree() {
    let mut app = test_app();
    let before = app.layout.clone();
    app.toggle_places();
    assert_ne!(app.layout, before, "opening it does change the tree");
    app.toggle_places();
    assert_eq!(app.layout, before);
    assert!(app.places_slot().is_none());
    assert_eq!(app.key_owner(), KeyOwner::Panes);
}

/// A second press with the keyboard on the listings: FOCUSES, does not
/// close. Closing something the reader just glanced at is the wrong
/// answer.
#[test]
fn with_the_sidebar_open_and_the_keyboard_outside_the_key_focuses_it() {
    let mut app = test_app();
    app.toggle_places();
    app.return_keys_to_panes();
    app.toggle_places();
    assert!(app.places_slot().is_some(), "still open");
    assert_eq!(app.key_owner(), KeyOwner::Places);
}

/// The sidebar's slot exists in the tree but is NOT a `browser`: iterating
/// the panes still gives only listings, which is what half the run loop
/// lives on.
#[test]
fn the_sidebar_slot_does_not_appear_as_a_listing() {
    let mut app = test_app();
    app.toggle_places();
    let sidebar = app.places_slot().expect("open");
    assert!(app.layout.slot_ids().contains(&sidebar));
    assert_eq!(app.panes.iter().count(), 2);
    assert!(app.panes.browser(sidebar).is_none());
    assert!(app.panes.places(sidebar).is_some());
}

/// An extra `Split` does not appear from opening the sidebar twice: the
/// second press does not mint another slot.
#[test]
fn opening_it_twice_does_not_nest_two_slots() {
    let mut app = test_app();
    app.toggle_places();
    let first = app.places_slot().expect("open");
    app.return_keys_to_panes();
    app.toggle_places();
    assert_eq!(app.places_slot(), Some(first));
    assert_eq!(
        app.layout
            .slot_ids()
            .iter()
            .filter(|id| app
                .layout
                .kind_of(**id)
                .is_some_and(|k| k.as_str() == "places"))
            .count(),
        1
    );
}

fn volume(mount: &str, free: u64, total: u64) -> Volume {
    Volume {
        mount: vp(mount),
        label: None,
        fs_type: "ext4".to_owned(),
        kind: VolumeKind::Fixed,
        total_bytes: Some(total),
        free_bytes: Some(free),
        read_only: false,
    }
}

/// An `App` with the sidebar open and populated, ready to paint.
fn app_con_sidebar() -> App {
    let mut app = test_app();
    app.render_now_ms = Some(0);
    app.toggle_places();
    let id = app.places_slot().expect("open");
    let sidebar = app.panes.places_mut(id).expect("it is a sidebar");
    sidebar.set_drives(&[
        volume("file:///", 41_000_000_000, 120_000_000_000),
        volume("file:///boot", 402_000_000, 1_000_000_000),
    ]);
    sidebar.set_favorites(&[
        ("trabajo".to_owned(), Ok(vp("file:///trabajo"))),
        ("roto".to_owned(), Err("hotlist-invalid".to_owned())),
    ]);
    app
}

/// Presses the left button on a cell, through the same path as the run
/// loop.
fn pulsar_en(app: &mut App, col: u16, row: u16) -> norte_tui::mouse::After {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    norte_tui::mouse::handle(
        app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        },
    )
}

fn buffer_de(app: &App, w: u16, h: u16) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, app))
        .expect("draw");
    terminal.backend().buffer().clone()
}

/// The buffer's rows, cell by cell.
///
/// By hand and NOT with `TestBackend::to_string()`: that wraps each row in
/// quotes, so any per-column clip comes out shifted by one cell — and a
/// `contains()` hides it. A geometry test with `contains` does not check
/// geometry.
fn rows(buf: &ratatui::buffer::Buffer) -> Vec<String> {
    (buf.area.top()..buf.area.bottom())
        .map(|y| {
            (buf.area.left()..buf.area.right())
                .map(|x| buf[(x, y)].symbol())
                .collect()
        })
        .collect()
}

/// The sidebar measures EXACTLY 16 cells and the first listing starts right
/// after. `Fixed` wins over the kind's minimum, so this number is the real
/// width and not a suggestion.
#[test]
fn the_sidebar_occupies_sixteen_cells_and_the_listing_starts_at_seventeen() {
    let app = app_con_sidebar();
    let buf = buffer_de(&app, 100, 30);
    let f = rows(&buf);
    // Row 3: inside both blocks, already past the top border. THREE since
    // there are two chrome rows pinned: 0 is the menu bar, 1 the panel bar
    // (#324) and 2 the blocks' top border.
    let row = &f[3];
    let cell = |x: usize| row.chars().nth(x).expect("the cell is painted");
    assert_eq!(cell(0), '│', "sidebar's left border");
    assert_eq!(cell(15), '│', "sidebar's right border, at cell 15");
    assert_eq!(cell(16), '│', "first listing's left border, at 16");
}

/// The whole screen with the sidebar open.
#[test]
fn snapshot_sidebar_open() {
    let app = app_con_sidebar();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    insta::assert_snapshot!(terminal.backend().to_string());
}

/// A broken favorite PAINTS, marked and dimmed. Hiding it would be a
/// config error the reader cannot see; and the whole reason does not fit in
/// fourteen cells, so the status bar says it (see `places_activate`).
#[test]
fn the_broken_favorite_is_painted_marked_and_dimmed() {
    let app = app_con_sidebar();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    let buf = terminal.backend().buffer().clone();
    let f = rows(&buf);
    let y = f
        .iter()
        .position(|row| row.chars().take(16).collect::<String>().contains("roto"))
        .expect("the broken favorite's row is painted");
    let sidebar: String = f[y].chars().take(16).collect();
    assert!(sidebar.contains('!'), "it is marked: {sidebar:?}");
    // And DIMMED: the text dump carries no styles, so this goes cell by cell.
    let x = sidebar.find("roto").expect("the name is there");
    let style = buf[(
        u16::try_from(x).expect("it fits"),
        u16::try_from(y).expect("it fits"),
    )]
        .style();
    assert_eq!(
        style.fg,
        app.theme.role(norte_theme::Role::Info).fg,
        "a broken favorite's row paints with the dimmed foreground"
    );
}

/// Closed — the default — the screen carries no sidebar at all: L3's
/// acceptance criterion is that the user notices nothing until they open
/// it, and the orthodox snapshots that already exist check it cell by
/// cell.
#[test]
fn closed_paints_nothing() {
    let mut app = test_app();
    app.render_now_ms = Some(0);
    let buf = buffer_de(&app, 100, 30);
    let f = rows(&buf);
    // Row 1 is the panel bar, which since spec 2026-09-10 names the panel
    // ("Sitios") precisely so its existence is known: it is skipped.
    assert!(
        !f.iter()
            .enumerate()
            .filter(|(i, _)| *i != 1)
            .any(|(_, row)| row.contains(&norte_i18n::t_in(norte_i18n::Lang::Es, "places-title"))),
        "without opening it, the sidebar's title does not appear"
    );
}

/// Enter on a favorite takes the FOCUSED LISTING to that location, and
/// returns the keyboard. The sidebar is a REMOTE, not a panel with its own
/// directory.
#[test]
fn enter_on_a_favorite_gives_the_destination_and_releases_the_keyboard() {
    let mut app = app_con_sidebar();
    // Units header, two disks, Favorites header, trabajo.
    for _ in 0..4 {
        app.places_down();
    }
    let dest = app
        .places_activate()
        .expect("a favorite gives a destination");
    assert_eq!(dest, vp("file:///trabajo"));
    assert_eq!(app.key_owner(), KeyOwner::Panes);
}

/// Enter on a header does nothing, and the keyboard stays put.
#[test]
fn enter_on_a_header_does_nothing() {
    let mut app = app_con_sidebar();
    assert!(app.places_activate().is_none());
    assert_eq!(app.key_owner(), KeyOwner::Places);
}

/// Enter on a BROKEN favorite does not navigate and the bar says why: it is
/// the other half of painting it marked, because fourteen cells fit the
/// warning but not the explanation.
#[test]
fn enter_on_a_broken_favorite_explains_in_the_bar() {
    let mut app = app_con_sidebar();
    for _ in 0..5 {
        app.places_down();
    }
    assert!(app.places_activate().is_none());
    assert_eq!(
        app.key_owner(),
        KeyOwner::Places,
        "it does not release the keyboard"
    );
    assert_eq!(
        app.message.as_deref(),
        Some(norte_i18n::t_in(norte_i18n::Lang::Es, "hotlist-invalid").as_str()),
        "the bar says the reason"
    );
}

/// Enter on a disk goes to its mount point.
#[test]
fn enter_on_a_disk_gives_its_mount() {
    let mut app = app_con_sidebar();
    app.places_down();
    assert_eq!(app.places_activate(), Some(vp("file:///")));
}

/// Folding a section hides its rows without closing anything.
#[test]
fn folding_from_the_app_hides_the_rows() {
    let mut app = app_con_sidebar();
    let id = app.places_slot().expect("open");
    let before = app.panes.places(id).expect("sidebar").rows().len();
    app.places_toggle_fold();
    let after = app.panes.places(id).expect("sidebar").rows().len();
    assert!(after < before);
    assert!(
        app.places_slot().is_some(),
        "folding does not close the sidebar"
    );
}

/// A layout that no longer has the panel CANNOT leave the keyboard inside
/// it.
///
/// `set_layout` did not touch `key_owner`, so with the sidebar focused and
/// a new layout with no sidebar — switching profile, applying a preset,
/// restoring a session — the keyboard kept pointing at a panel that was no
/// longer there. Every key went to `on_places_key`, `places_slot()`
/// returned `None`, and every arm was a no-op: the whole manager stopped
/// responding with nothing on screen to explain why.
#[test]
fn a_layout_without_the_panel_returns_the_keyboard() {
    use norte_frontend::layout::{KindId, Node, SlotId};

    // `app_con_sidebar` already opens it, and opening it ALREADY gives the
    // keyboard: the real cycle is open-with-keyboard → close, not the three
    // presses some code comment describes.
    let mut app = app_con_sidebar();
    assert_eq!(app.key_owner(), KeyOwner::Places, "the keyboard is inside");

    // A single-listing layout: no sidebar.
    app.set_layout(Node::slot(SlotId(1), KindId::browser()));

    assert!(app.places_slot().is_none(), "the panel is no longer there");
    assert_eq!(
        app.key_owner(),
        KeyOwner::Panes,
        "and the keyboard has returned to the listings"
    );
}

/// The cursor starts on a HEADER, which is why `⏎` has to answer there.
///
/// `places_activate` returns `None` on a header, so Enter was inert
/// exactly on the panel's first row: you opened it, pressed the key that
/// gets tried first on something that opens, and nothing happened. Folding
/// was Space and only Space.
#[test]
fn the_cursor_starts_on_a_header() {
    let app = app_con_sidebar();
    assert!(
        app.places_cursor_on_header(),
        "the first row is a section header"
    );
}

/// And scrolling down to a drive stops being one: there `⏎` navigates,
/// which is what Enter means on a leaf.
#[test]
fn over_a_drive_the_cursor_is_no_longer_on_a_header() {
    let mut app = app_con_sidebar();
    app.places_down();
    assert!(!app.places_cursor_on_header());
}

/// `layout.places` is bound in all SEVEN presets, and in `[global]`.
///
/// The first, because a core command bound in some and not others is the
/// hole L1b introduced with `pane.tab-next`: you could open a tab and not
/// get back to it in five of the seven.
///
/// The second was exposed by piloting the TUI in tmux with the suite green:
/// bound only in `[pane]`, the key did not exist for the `dialog` screen,
/// which is the one in force while the keyboard is INSIDE the sidebar. So
/// you opened the panel and the key to close it stopped working. That is
/// why BOTH screens are checked: the one that fires is the one that
/// matters.
#[test]
fn layout_places_is_bound_in_all_seven_presets_and_both_screens() {
    use norte_frontend::keymap::{CATALOGUE, Effective, Screen, parse_keymap, presets};
    let conocidos: Vec<&str> = CATALOGUE.iter().map(|d| d.name).collect();
    for name in presets::NAMES {
        let src = presets::source(name).expect("the preset exists");
        let kf = parse_keymap(src).expect("the preset parses");
        for screen in [Screen::Browse, Screen::Dialog] {
            let eff =
                Effective::build_for(&kf, &[], &conocidos, screen).expect("the preset merges");
            assert!(
                eff.bindings()
                    .iter()
                    .any(|(_, cmd)| *cmd == "layout.places"),
                "{name} does not bind layout.places in {screen:?}"
            );
        }
    }
}

/// The mouse: clicking a row selects it and brings the keyboard; clicking
/// it again ACTIVATES it, which is the same as `Enter` (#226).
///
/// The sidebar shipped with keyboard support and nothing more: its cells
/// belong to no listing, so a click there landed on "outside the panes"
/// and did nothing — a panel that paints and cannot be touched.
#[test]
fn pressing_a_sidebar_row_selects_it_and_pressing_again_activates_it() {
    let mut app = app_con_sidebar();
    let area = ratatui::layout::Rect::new(0, 0, 100, 30);
    let _ = buffer_de(&app, 100, 30);
    let (geo, tabs, menus, sitios) = (
        norte_tui::ui::pane_geometry(&app, area),
        norte_tui::ui::tab_zones(&app, area),
        norte_tui::ui::menu_zones(&app, area),
        norte_tui::ui::places_zones(&app, area),
    );
    let huecos = norte_tui::ui::panel_slots(&app, area);
    norte_tui::mouse::after_frame(
        &mut app,
        geo,
        norte_tui::mouse::FrameZones {
            tabs,
            menus,
            places: sitios,
            slots: huecos,
            ..Default::default()
        },
    );
    let zones = norte_tui::ui::places_zones(&app, area);
    assert!(!zones.is_empty(), "the sidebar has clickable rows");
    // The first drive: row 0 is the section's header.
    let unit = zones
        .iter()
        .find(|z| z.index == 1)
        .copied()
        .expect("the first drive is visible");

    app.return_keys_to_panes();
    let after = pulsar_en(&mut app, unit.x0 + 1, unit.row);
    assert_eq!(after, norte_tui::mouse::After::Nothing, "only selects");
    assert_eq!(app.key_owner(), KeyOwner::Places, "and brings the keyboard");
    let cursor = app
        .places_slot()
        .and_then(|id| app.panes.places(id))
        .map(norte_frontend::places::PlacesState::cursor);
    assert_eq!(cursor, Some(1));

    // The same row again: that is activating, and activating it is
    // resolved by the run loop through the usual `cd` flow.
    let after = pulsar_en(&mut app, unit.x0 + 1, unit.row);
    assert_eq!(after, norte_tui::mouse::After::PlacesActivate);
    assert!(
        app.places_activate().is_some(),
        "and there is somewhere to take the listing"
    );
}

/// Clicking a HEADER folds its section with a single click, and says so, so
/// the run loop asks for the drives again — the same path as the key, not
/// a fourth refresh trigger (#226).
#[test]
fn pressing_a_header_folds_its_section() {
    let mut app = app_con_sidebar();
    let area = ratatui::layout::Rect::new(0, 0, 100, 30);
    let _ = buffer_de(&app, 100, 30);
    let (geo, tabs, menus, sitios) = (
        norte_tui::ui::pane_geometry(&app, area),
        norte_tui::ui::tab_zones(&app, area),
        norte_tui::ui::menu_zones(&app, area),
        norte_tui::ui::places_zones(&app, area),
    );
    let huecos = norte_tui::ui::panel_slots(&app, area);
    norte_tui::mouse::after_frame(
        &mut app,
        geo,
        norte_tui::mouse::FrameZones {
            tabs,
            menus,
            places: sitios,
            slots: huecos,
            ..Default::default()
        },
    );
    let zones = norte_tui::ui::places_zones(&app, area);
    let header = zones
        .iter()
        .find(|z| z.index == 0)
        .copied()
        .expect("the header is visible");
    let rows_before = app
        .places_slot()
        .and_then(|id| app.panes.places(id))
        .map(|s| s.rows().len())
        .expect("sidebar");

    let after = pulsar_en(&mut app, header.x0 + 1, header.row);
    assert_eq!(after, norte_tui::mouse::After::PlacesFolded);
    let rows_now = app
        .places_slot()
        .and_then(|id| app.panes.places(id))
        .map(|s| s.rows().len())
        .expect("sidebar");
    assert!(
        rows_now < rows_before,
        "folding hides its rows: {rows_before} → {rows_now}"
    );
    assert!(!app.places_drives_visible(), "and the drives stay folded");
}

/// With the keyboard INSIDE the sidebar, `layout.grow` changes THE
/// SIDEBAR'S width.
///
/// It used to change nothing: `layout_resize` always passed
/// `focused_slot()`, which is a visible listing, so no production path
/// ever reached `Node::resize`'s `Size::Fixed` branch — the sidebar kept
/// the width it opened with while the CHANGELOG announced the opposite
/// (#244 M1). `resize`'s tests passed because they handed it the sidebar's
/// id by hand.
#[test]
fn with_the_keyboard_inside_the_sidebar_changes_width() {
    use norte_frontend::layout::Size;

    let mut app = test_app();
    app.toggle_places();
    assert_eq!(app.key_owner(), KeyOwner::Places, "the keyboard is inside");
    let id = app.places_slot().expect("open");
    let width = |app: &App| {
        app.layout
            .sizes_of(id)
            .and_then(|(sizes, pos)| sizes.get(pos).copied())
    };
    let before = width(&app).expect("the sidebar has a size");
    assert!(
        matches!(before, Size::Fixed(_)),
        "and it is FIXED: {before:?}"
    );

    app.layout_resize(1);
    assert_ne!(width(&app), Some(before), "it grew");

    // And with the keyboard OUTSIDE the focused listing rules again: the
    // sidebar does not move on its own.
    app.return_keys_to_panes();
    let now = width(&app);
    app.layout_resize(1);
    assert_eq!(
        width(&app),
        now,
        "the sidebar is not touched from the listings"
    );
}

/// And the sidebar DISPATCHES its own key: without this the key arrives and
/// falls into the allowlist, which is the same dead screen with a
/// different culprit.
#[test]
fn the_sidebar_dispatches_its_own_key() {
    assert!(norte_tui::app::ALLOW_PLACES.contains(&"layout.places"));
}

/// Drives are REQUESTED by a flag, and what turns it on are the three paths
/// through which the section appears.
///
/// `host.volumes` is I/O and `App` has no backend, so each place had to
/// request it on its own — and it was missing exactly where nobody
/// remembered: starting with a layout that brings the sidebar, and
/// switching profile, which mounts a new screen and with it an empty
/// panel.
#[test]
fn the_three_paths_leave_the_requested_units() {
    use norte_frontend::layout::{Edge, KindId, Node, Size, SlotId};
    let mut app = test_app();
    assert!(
        !app.places_wants_drives,
        "with no sidebar nothing is requested"
    );

    // 1 — open it with its key.
    app.toggle_places();
    assert!(app.places_wants_drives);
    app.places_wants_drives = false;

    // 2 — folding does NOT request them (they are not visible); unfolding does.
    app.places_toggle_fold();
    assert!(!app.places_wants_drives, "folded, they are not requested");
    app.places_toggle_fold();
    assert!(app.places_wants_drives);
    app.places_wants_drives = false;

    // 3 — a layout that already brings it, without going through the key.
    let tree = Node::slot(SlotId(0), KindId::browser()).dock(
        SlotId(0),
        Edge::Left,
        Size::Fixed(16),
        &Node::slot(SlotId(9), KindId::new("places")),
    );
    app.set_layout(tree);
    assert!(app.places_wants_drives);
}

/// A valid favorite, as the already-loaded config leaves it.
fn favorite(name: &str, wire: &str) -> norte_tui::config::HotlistItem {
    norte_tui::config::HotlistItem {
        name: name.to_owned(),
        target: Ok(vp(wire)),
    }
}

/// The names of the favorites the sidebar paints right now.
fn sidebar_favorites(app: &App) -> Vec<String> {
    let id = app.places_slot().expect("the sidebar is open");
    app.panes
        .places(id)
        .expect("it is a sidebar")
        .rows()
        .iter()
        .filter_map(|r| match r {
            norte_frontend::places::PlaceRow::Favorite { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

/// A layout that BRINGS the sidebar — `full`, `explorer`, yesterday's
/// session — opens it without going through its key, and it was the key
/// that copied the favorites: the panel came out empty and nothing inside
/// the program ever filled it.
#[test]
fn a_layout_that_brings_the_sidebar_starts_it_with_the_favorites() {
    use norte_frontend::layout::{Edge, KindId, Node, Size, SlotId};
    let mut app = test_app();
    app.set_hotlist(vec![favorite("descargas", "file:///casa/descargas")]);
    // The sidebar comes in through the TREE, not through `toggle_places`.
    let tree = Node::slot(SlotId(0), KindId::browser()).dock(
        SlotId(0),
        Edge::Left,
        Size::Fixed(16),
        &Node::slot(SlotId(9), KindId::new("places")),
    );
    app.set_layout(tree);
    assert_eq!(sidebar_favorites(&app), vec!["descargas".to_owned()]);
}

/// And a favorite added with the sidebar ALREADY open shows up in it. The
/// popup got rebuilt and the sidebar did not, so the two surfaces for the
/// same data said different things — the complaint was exactly that.
#[test]
fn adding_a_favorite_also_paints_it_in_the_sidebar() {
    let mut app = test_app();
    app.toggle_places();
    assert!(sidebar_favorites(&app).is_empty(), "it starts with none");

    app.hotlist_apply_saved("descargas", vp("file:///casa/descargas"));
    assert_eq!(sidebar_favorites(&app), vec!["descargas".to_owned()]);

    app.hotlist_apply_removed("descargas");
    assert!(
        sidebar_favorites(&app).is_empty(),
        "and removing it removes it from both places"
    );
}

/// A hot reload of `norte.toml` — or a profile switch, which goes through
/// the same place — replaces the whole list, and the sidebar follows it.
#[test]
fn reloading_the_config_replaces_the_sidebars_favorites() {
    let mut app = test_app();
    app.toggle_places();
    app.set_hotlist(vec![favorite("viejo", "file:///viejo")]);
    assert_eq!(sidebar_favorites(&app), vec!["viejo".to_owned()]);

    app.set_hotlist(vec![favorite("nuevo", "file:///nuevo")]);
    assert_eq!(sidebar_favorites(&app), vec!["nuevo".to_owned()]);
}
