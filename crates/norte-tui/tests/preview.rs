//! The DOCKED viewer (L3): what it decides to read, and above all what it
//! decides NOT to read.
//!
//! All of this is tested with no daemon on purpose. `main.rs` is a binary
//! and an integration test cannot reach it, so the decision lives in the
//! lib: if `preview::want` returns no target, there is no request to count.
//! Suspending a hidden slot stops being a rule written in a spec and
//! becomes the only thing the code can do.

use norte_frontend::layout::{LayoutDiagnostic, Resolved};
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, KeyOwner, Pane};
use norte_tui::preview::{Want, want};

const W: u16 = 100;
const H: u16 = 30;

fn vp(wire: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("valid wire")
}

fn entry(dir: &VPath, name: &str, kind: EntryKind) -> Entry {
    Entry {
        attrs: std::collections::BTreeMap::new(),
        path: dir
            .join(Segment::new(name.as_bytes().to_vec()).expect("segment"))
            .clone(),
        kind,
        size: Some(1),
        mtime_ms: None,
    }
}

/// Two listings, each with a DIFFERENT file and a directory.
///
/// `Pane::new` SORTS, and the order puts directories first: the cursor
/// starts on `folder`, not on the file. The tests place it by hand.
fn test_app() -> App {
    let left = vp("file:///izq");
    let right = vp("file:///der");
    App::new(
        Pane::new(
            left.clone(),
            vec![
                entry(&left, "uno.txt", EntryKind::File),
                entry(&left, "carpeta", EntryKind::Dir),
            ],
        ),
        Pane::new(
            right.clone(),
            vec![entry(&right, "dos.txt", EntryKind::File)],
        ),
    )
}

/// Resolves the frame the way the run loop does: `before_frame` reconciles
/// the roles, and without it the `follows: Role(Active)` link points at
/// nobody.
fn resolver(app: &mut App) -> Resolved {
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    norte_tui::ui::before_frame(app, area);
    norte_tui::ui::resolved_for(app, area)
}

/// With the preview open and the cursor on a file, there is a target, and
/// it travels with its SLOT. It is P6 phase C's lesson: by position, an
/// answer in flight applies to whoever occupies that spot when it arrives.
#[test]
fn the_target_carries_the_previews_slot() {
    let mut app = test_app();
    app.toggle_preview();
    app.panes[0].set_cursor(1);
    let res = resolver(&mut app);
    let (slot, w) = want(&app, &res).expect("there is a target");
    assert_eq!(Some(slot), app.preview_slot());
    assert_eq!(w, Want::File(vp("file:///izq/uno.txt")));
}

/// On the `..` row the viewer says "directory," not "nothing selected."
///
/// With the row on — the factory default — the cursor is born right there,
/// so this was the note EVERYONE saw on opening. And it still reads
/// nothing: `..` leads to a folder, and a folder is not read.
#[test]
fn over_the_parent_row_the_viewer_says_directory() {
    let mut app = test_app();
    app.set_parent_row(true);
    app.toggle_preview();
    assert!(
        app.panes[0].is_parent_row(app.panes[0].cursor()),
        "the cursor is born on `..`"
    );
    let res = resolver(&mut app);
    let (_, w) = want(&app, &res).expect("there is a target");
    assert_eq!(w, Want::Note("preview-directory"));
}

/// A preview behind a tab produces no target: there is no request to
/// count. In L1b an identical leak was only ever seen by a test, so here it is.
#[test]
fn un_preview_oculto_no_produce_objetivo() {
    let mut app = test_app();
    app.toggle_preview();
    app.panes[0].set_cursor(1);
    let res = resolver(&mut app);
    assert!(want(&app, &res).is_some());

    // Hidden by putting it in a tab with another panel in front.
    let slot = app.preview_slot().expect("open");
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
    assert!(
        want(&app, &res).is_none(),
        "a slot that does not paint requests nothing"
    );
}

/// A directory under the cursor is not read: what it is gets said instead.
#[test]
fn a_directory_under_the_cursor_is_not_read() {
    let mut app = test_app();
    app.toggle_preview();
    app.panes[0].set_cursor(0);
    let res = resolver(&mut app);
    let (_, w) = want(&app, &res).expect("there is an answer");
    assert_eq!(w, Want::Note("preview-directory"));
}

/// The preview follows the `active` role: switching listings changes what
/// it shows, without touching the layout. It is the first consumer of
/// `follows` that exists.
#[test]
fn changing_listing_changes_the_target() {
    let mut app = test_app();
    app.toggle_preview();
    app.panes[0].set_cursor(1);
    let res = resolver(&mut app);
    let (_, a) = want(&app, &res).expect("target");
    app.set_focus(1);
    let res = resolver(&mut app);
    let (_, b) = want(&app, &res).expect("target");
    assert_eq!(a, Want::File(vp("file:///izq/uno.txt")));
    assert_eq!(b, Want::File(vp("file:///der/dos.txt")));
}

/// And if the FOLLOWED slot dies, the engine degrades to the `active` role
/// and SAYS SO. The diagnostic has existed since L1a and until L3 nobody
/// exercised it.
#[test]
fn if_the_followed_slot_dies_it_degrades_to_active_and_says_so() {
    use norte_frontend::layout::{Bindings, Edge, Follow, KindId, Node, Size, SlotId};
    let mut app = test_app();
    app.toggle_preview();
    let slot = app.preview_slot().expect("open");
    // The SAME slot is redocked bound to a SPECIFIC one that does not
    // exist: it is the state a `follows: Slot(id)` is left in when its
    // panel closed.
    let focus = app.focused_slot();
    app.layout = app.layout.close_slot(slot).expect("it can be closed").dock(
        focus,
        Edge::Right,
        Size::Weight(1),
        &Node::slot_bound(
            slot,
            KindId::new("viewer"),
            Bindings {
                follows: Some(Follow::Slot(SlotId(777))),
            },
        ),
    );
    let res = resolver(&mut app);
    assert!(
        res.diagnostics.is_empty(),
        "the layout itself has nothing to fix"
    );
    let mut diags = Vec::new();
    let dest = norte_frontend::layout::resolve_follow(&app.layout, slot, &app.roles, &mut diags);
    assert_eq!(dest, app.roles.get(norte_frontend::layout::RoleId::Active));
    assert!(
        diags
            .iter()
            .any(|d| matches!(d, LayoutDiagnostic::FollowRetargeted { .. })),
        "and it says so instead of staring into the void"
    );
    assert!(
        want(&app, &res).is_some(),
        "the preview still shows something"
    );
}

/// Opening the preview does not change how many LISTINGS there are, which
/// one has focus, or — and this is what matters — who has the keyboard.
///
/// A preview that takes the arrows turns off the only thing it does:
/// following a cursor that can no longer move. tmux exposed it on the
/// first press.
#[test]
fn opening_the_preview_does_not_take_the_keyboard() {
    let mut app = test_app();
    let before = (app.panes.len(), app.focus());
    app.toggle_preview();
    assert_eq!((app.panes.len(), app.focus()), before);
    assert_eq!(app.key_owner(), KeyOwner::Panes);
    assert!(app.preview_slot().is_some());
}

/// The second press DOES take it: it is how `viewer.hex` and the encodings
/// are reached without inventing new keys.
#[test]
fn the_second_press_focuses_the_preview() {
    let mut app = test_app();
    app.toggle_preview();
    app.toggle_preview();
    assert_eq!(app.key_owner(), KeyOwner::Preview);
    assert!(app.preview_slot().is_some(), "focusing does not close it");
}

/// And the third one closes it, leaving the tree as it was.
#[test]
fn the_third_press_closes_and_returns_the_previous_tree() {
    let mut app = test_app();
    let before = app.layout.clone();
    app.toggle_preview();
    app.toggle_preview();
    app.toggle_preview();
    assert_eq!(app.layout, before);
    assert!(app.preview_slot().is_none());
    assert_eq!(app.key_owner(), KeyOwner::Panes);
}

/// A denied read PAINTS in the slot, and opens no modal.
///
/// The preview follows the cursor: a dialog per keystroke would turn
/// scrolling down a directory into a burst of modals nobody asked for.
#[test]
fn a_denied_read_paints_the_reason_and_does_not_open_a_modal() {
    let mut app = test_app();
    app.toggle_preview();
    let slot = app.preview_slot().expect("open");
    app.preview_failed(slot, "err-permission-denied");
    assert!(app.modal.is_none(), "nothing is asked");

    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(W, H)).expect("terminal");
    app.render_now_ms = Some(0);
    norte_tui::ui::before_frame(&mut app, ratatui::layout::Rect::new(0, 0, W, H));
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    let text = terminal.backend().to_string();
    let reason = norte_i18n::t_in(norte_i18n::Lang::Es, "err-permission-denied");
    assert!(
        text.contains(&reason),
        "the reason reads inside the slot:\n{text}"
    );
}

/// `layout.preview` is bound in all seven presets and on both screens that
/// need it: the navigation one and the VIEWER's, which is the one that
/// resolves while the keyboard is inside the preview. Same trap `alt+b`
/// exposed in tmux, closed here before it bites.
#[test]
fn layout_preview_is_bound_in_all_seven_presets_and_both_screens() {
    use norte_frontend::keymap::{CATALOGUE, Effective, Screen, parse_keymap, presets};
    let conocidos: Vec<&str> = CATALOGUE.iter().map(|d| d.name).collect();
    for name in presets::NAMES {
        let src = presets::source(name).expect("the preset exists");
        let kf = parse_keymap(src).expect("the preset parses");
        for screen in [Screen::Browse, Screen::Viewer] {
            let eff =
                Effective::build_for(&kf, &[], &conocidos, screen).expect("the preset merges");
            assert!(
                eff.bindings()
                    .iter()
                    .any(|(_, cmd)| *cmd == "layout.preview"),
                "{name} does not bind layout.preview in {screen:?}"
            );
        }
    }
}
