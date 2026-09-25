//! The attributes sheet (phase A): who it follows, and above all what it
//! does NOT read.
//!
//! Same as the docked viewer, it is tested with no daemon: the decision
//! lives in the lib, so "a hidden slot requests nothing" stops being a rule
//! written in a spec and becomes the only thing the code can do. And here
//! there is an even stronger rule: the sheet NEVER requests, not even when
//! visible. What it shows is already in the listing.

use norte_frontend::layout::Resolved;
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, KeyOwner, Pane};
use norte_tui::metadata::{Want, want};

const W: u16 = 100;
const H: u16 = 30;

fn vp(wire: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("valid wire")
}

fn entry(dir: &VPath, name: &[u8], kind: EntryKind) -> Entry {
    Entry {
        attrs: std::collections::BTreeMap::new(),
        path: dir
            .join(Segment::new(name.to_vec()).expect("segment"))
            .clone(),
        kind,
        size: Some(7),
        mtime_ms: Some(1_700_000_000_000),
    }
}

fn test_app() -> App {
    let left = vp("file:///izq");
    let right = vp("file:///der");
    App::new(
        Pane::new(
            left.clone(),
            vec![
                entry(&left, b"uno.txt", EntryKind::File),
                entry(&left, b"carpeta", EntryKind::Dir),
            ],
        ),
        Pane::new(
            right.clone(),
            vec![entry(&right, b"dos.txt", EntryKind::File)],
        ),
    )
}

fn resolver(app: &mut App) -> Resolved {
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    norte_tui::ui::before_frame(app, area);
    norte_tui::ui::resolved_for(app, area)
}

/// With the sheet open and the cursor on something, there is a target, and
/// it travels with its SLOT: an answer applied by position lands on
/// whoever occupies that spot when it arrives (P6 phase C's lesson).
#[test]
fn the_target_carries_the_sheets_slot() {
    let mut app = test_app();
    app.toggle_metadata();
    app.panes[0].set_cursor(1);
    let res = resolver(&mut app);
    let (slot, w) = want(&app, &res).expect("there is a target");
    assert_eq!(Some(slot), app.metadata_slot());
    let Want::Entry(e, up) = w else {
        panic!("an entry")
    };
    assert!(!up, "the cursor is on a real entry");
    assert_eq!(e.path, vp("file:///izq/uno.txt"));
}

/// The entry it shows is the one the listing ALREADY has: same size, same
/// date, same bytes. This is what makes the sheet cost no read.
#[test]
fn what_it_shows_comes_from_the_listing_and_not_a_request() {
    let mut app = test_app();
    app.toggle_metadata();
    app.panes[0].set_cursor(1);
    let of_the_listing = app.panes[0].selected().expect("cursor").clone();
    let res = resolver(&mut app);
    let (_, w) = want(&app, &res).expect("target");
    assert_eq!(w, Want::Entry(Box::new(of_the_listing), false));
}

/// With the `..` row on — the factory default — the cursor is born on it,
/// and the sheet DESCRIBES it instead of going empty.
///
/// The same rule as the window's, and for the same reason: `selected()`
/// stays silent about that row because it is not an operand, but the sheet
/// does not operate, it describes. Asking for the operand left the panel
/// empty on every startup and after every `cd`, which is what a broken
/// panel looks like.
#[test]
fn over_the_parent_row_the_sheet_describes_it() {
    let mut app = test_app();
    app.set_parent_row(true);
    app.toggle_metadata();
    assert!(
        app.panes[0].is_parent_row(app.panes[0].cursor()),
        "the cursor is born on `..`"
    );
    let res = resolver(&mut app);
    let (_, w) = want(&app, &res).expect("there is a target");
    let Want::Entry(e, up) = w else {
        panic!("an entry, not the empty note")
    };
    assert!(up, "and it is marked as the parent row");
    assert_eq!(e.path, vp("file:///"), "its path is the parent's");

    // And those rows are the same ones the window paints: the shared crate
    // decides the list, not each renderer.
    let rows = norte_frontend::metadata::sheet(&e, up, None, norte_i18n::Lang::Es);
    assert_eq!(
        rows.iter().map(|f| f.value.as_str()).collect::<Vec<_>>(),
        // `file` with no authority is not announced: it is the default
        // case, and its label did not distinguish anything from anything.
        ["..", "carpeta", "/"]
    );
}

/// The sheet SAYS which listing it follows, and says so also when focus
/// changes.
///
/// The same answer the window gives in `follows_display` (ADR 0077): a
/// bare "Details" does not say whose details they are, and with two
/// listings open the only way to know was to move the cursor and look.
#[test]
fn the_sheet_says_which_listing_it_follows() {
    let mut app = test_app();
    app.toggle_metadata();
    let res = resolver(&mut app);
    let (path, hostile) = norte_tui::metadata::follows(&app, &res).expect("there is a placed slot");
    assert_eq!(path, "/izq");
    assert!(!hostile);

    app.set_focus(1);
    let res = resolver(&mut app);
    let (other, _) = norte_tui::metadata::follows(&app, &res).expect("still placed");
    assert_eq!(other, "/der", "it follows the ACTIVE one, and says so");
}

/// A slot behind a tab produces no target. It is the same invariant as the
/// preview's, and is tested the same way because only a test sees a leak
/// like that.
#[test]
fn a_hidden_sheet_produces_no_target() {
    let mut app = test_app();
    app.toggle_metadata();
    app.panes[0].set_cursor(1);
    let res = resolver(&mut app);
    assert!(want(&app, &res).is_some());

    let slot = app.metadata_slot().expect("open");
    app.layout = app.layout.wrap_in_tabs(slot);
    app.layout = app.layout.add_tab(
        slot,
        &norte_frontend::layout::Node::slot(
            norte_frontend::layout::SlotId(900),
            norte_frontend::layout::KindId::new("tasks"),
        ),
    );
    let res = resolver(&mut app);
    assert!(
        !res.placements.iter().any(|(id, _)| *id == slot),
        "the slot really is hidden"
    );
    assert!(want(&app, &res).is_none(), "what is not seen shows nothing");
}

/// With no cursor, what was there before does not stick: it says there is
/// nothing underneath.
#[test]
fn an_empty_listing_says_there_is_nothing() {
    let dir = vp("file:///vacio");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    app.toggle_metadata();
    let res = resolver(&mut app);
    let (_, w) = want(&app, &res).expect("there is an answer");
    assert_eq!(w, Want::Note("metadata-empty"));
}

/// It follows the `active` role: switching listings changes what it shows,
/// without touching the layout.
#[test]
fn changing_listing_changes_what_it_shows() {
    let mut app = test_app();
    app.toggle_metadata();
    app.panes[0].set_cursor(1);
    let res = resolver(&mut app);
    let (_, a) = want(&app, &res).expect("target");
    app.set_focus(1);
    let res = resolver(&mut app);
    let (_, b) = want(&app, &res).expect("target");
    let (Want::Entry(a, _), Want::Entry(b, _)) = (a, b) else {
        panic!("two entries")
    };
    assert_eq!(a.path, vp("file:///izq/uno.txt"));
    assert_eq!(b.path, vp("file:///der/dos.txt"));
}

/// A name that is not UTF-8 arrives byte for byte: the sheet neither
/// reinterprets it nor loses it along the way.
#[test]
fn a_non_utf8_name_arrives_whole() {
    let dir = vp("file:///izq");
    let hostile = entry(&dir, b"m\xffl.txt", EntryKind::File);
    let mut app = App::new(
        Pane::new(dir.clone(), vec![hostile.clone()]),
        Pane::new(vp("file:///der"), Vec::new()),
    );
    app.toggle_metadata();
    let res = resolver(&mut app);
    let (_, w) = want(&app, &res).expect("target");
    assert_eq!(w, Want::Entry(Box::new(hostile), false));
}

/// The sheet NEVER takes the keyboard: it follows the cursor, and with the
/// arrows inside it would stop following anything. Two states, open and
/// close.
///
/// It used to have three, and the middle one was fake: it set a `KeyOwner`
/// nobody consumed, so the sheet took the focus border while the arrows
/// kept moving the neighboring listing, and a third press was needed to
/// close what the second had not focused (#243).
#[test]
fn the_sheet_never_takes_the_keyboard() {
    let mut app = test_app();
    app.toggle_metadata();
    assert!(app.metadata_slot().is_some(), "open");
    assert_eq!(
        app.key_owner(),
        KeyOwner::Panes,
        "the cursor is still yours"
    );

    app.toggle_metadata();
    assert!(app.metadata_slot().is_none(), "the second one closes it");
    assert_eq!(app.key_owner(), KeyOwner::Panes);
}

/// The processes panel does the opposite, on purpose: it opens to act on a
/// task, so it takes the keyboard on entry and the second press closes it.
/// The tasks strip is not touched at any point.
#[test]
fn the_processes_pane_takes_the_keyboard_on_open() {
    let mut app = test_app();
    let tasks_before = app
        .layout
        .slot_ids()
        .into_iter()
        .filter(|id| {
            app.layout
                .kind_of(*id)
                .is_some_and(|k| k.as_str() == "tasks")
        })
        .count();

    app.toggle_processes();
    assert!(app.processes_slot().is_some(), "open");
    assert_eq!(app.key_owner(), KeyOwner::Processes);

    app.toggle_processes();
    assert!(app.processes_slot().is_none(), "closed");
    assert_eq!(app.key_owner(), KeyOwner::Panes);

    let tasks_after = app
        .layout
        .slot_ids()
        .into_iter()
        .filter(|id| {
            app.layout
                .kind_of(*id)
                .is_some_and(|k| k.as_str() == "tasks")
        })
        .count();
    assert_eq!(tasks_before, tasks_after, "the strip stays where it was");
}

/// Closing either of the two returns the earlier tree, with no degenerate
/// `Split` left behind.
#[test]
fn closing_returns_the_previous_tree() {
    let mut app = test_app();
    let before = app.layout.clone();

    app.toggle_metadata();
    app.toggle_metadata();
    assert_eq!(app.layout, before, "the sheet left no trace");

    app.toggle_processes();
    app.toggle_processes();
    assert_eq!(app.layout, before, "neither did the processes panel");
}
