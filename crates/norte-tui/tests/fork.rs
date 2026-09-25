//! Splitting a pane and opening a tab INHERIT the listing of the pane next
//! to it, and that must not bring along the `..` row.
//!
//! What was being copied was `entries()`, which carries it inside: the new
//! pane put ITS OWN on top and the inherited one ended up in the middle of
//! the listing — with the parent directory's name, sorted among the
//! directories, and markable. Every split added one more, and `Ctrl+A`
//! stuffed the PARENT into what gets copied or deleted.

use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, Pane};

fn vp(wire: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("valid wire")
}

fn entries(dir: &VPath, n: usize) -> Vec<Entry> {
    (0..n)
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

/// Two panes over `file:///home/child` —a directory with a parent, which is
/// where the `..` row exists— and the row turned on.
fn app_with_parent_row() -> App {
    let dir = vp("file:///home/child");
    let mut app = App::new(
        Pane::new(dir.clone(), entries(&dir, 3)),
        Pane::new(dir.clone(), entries(&dir, 3)),
    );
    app.set_parent_row(true);
    app
}

/// How many entries each listing has, and how many of them point at the parent.
fn snapshot(app: &App) -> (Vec<usize>, usize) {
    let parent = vp("file:///home");
    let counts = (0..app.panes.len())
        .map(|i| app.panes[i].entries().len())
        .collect();
    let parents = (0..app.panes.len())
        .map(|i| {
            app.panes[i]
                .entries()
                .iter()
                .filter(|e| e.path == parent)
                .count()
        })
        .sum();
    (counts, parents)
}

/// Splitting three times leaves all four panes with the SAME listing, and a
/// single parent row in each.
#[test]
fn splitting_several_times_does_not_accumulate_parent_rows() {
    let mut app = app_with_parent_row();
    let (before, _) = snapshot(&app);
    assert_eq!(before[0], 4, "three entries plus the parent row");

    for _ in 0..3 {
        app.layout_split(norte_frontend::layout::Dir::Horizontal);
    }
    let (counts, parents) = snapshot(&app);
    assert_eq!(counts.len(), 5, "the original two plus the three new ones");
    assert!(
        counts.iter().all(|n| *n == before[0]),
        "every pane lists the same thing: {counts:?}"
    );
    assert_eq!(
        parents,
        counts.len(),
        "one parent row per pane, not one more"
    );
}

/// And a new tab, the same: it inherits the listing through the same path.
#[test]
fn a_new_tab_does_not_inherit_the_parent_row_as_an_entry() {
    let mut app = app_with_parent_row();
    let (before, _) = snapshot(&app);
    app.tab_new();
    app.tab_new();
    let (counts, parents) = snapshot(&app);
    assert!(
        counts.iter().all(|n| *n == before[0]),
        "every tab lists the same thing: {counts:?}"
    );
    assert_eq!(parents, counts.len());
}

/// The consequence that matters: after splitting, marking EVERYTHING marks
/// the same as before splitting — and the parent is not among what is
/// marked. If it were, F8 would delete the directory above.
#[test]
fn marking_all_in_a_split_pane_does_not_mark_the_parent() {
    let mut app = app_with_parent_row();
    app.panes[0].mark_all();
    let expected = app.panes[0].marks_len();
    assert_eq!(expected, 3, "the three real entries, not the parent row");
    app.panes[0].clear_marks();

    app.layout_split(norte_frontend::layout::Dir::Horizontal);
    app.layout_split(norte_frontend::layout::Dir::Horizontal);
    let new_pane = app.focus();
    app.panes[new_pane].mark_all();
    assert_eq!(
        app.panes[new_pane].marks_len(),
        expected,
        "the split pane marks the same as the original"
    );
    assert!(
        !app.panes[new_pane]
            .marked_paths()
            .contains(&vp("file:///home")),
        "the PARENT never enters what is marked: {:?}",
        app.panes[new_pane].marked_paths()
    );
}
