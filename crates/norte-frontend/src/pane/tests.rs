use super::*;
use norte_proto::{Entry, EntryKind, VPath};
fn e(w: &str, k: EntryKind) -> Entry {
    Entry {
        attrs: std::collections::BTreeMap::new(),
        path: VPath::parse(w).unwrap(),
        kind: k,
        size: None,
        mtime_ms: None,
    }
}
fn pane(names: &[&str]) -> PaneState {
    let es = names
        .iter()
        .map(|n| e(&format!("mem:///{n}"), EntryKind::File))
        .collect();
    PaneState::new(VPath::parse("mem:///").unwrap(), es)
}

/// **Pointing is not moving the cursor**, and with a live filter that
/// difference is what lets the docked viewer find out.
///
/// In `Mode::Filter` what is pointed at is the quick search's selection and
/// the real cursor is not looked at: `set_cursor` used to move something
/// nobody is following, so the key claimed it worked and the panel stayed
/// the same.
#[test]
fn point_at_moves_the_filters_selection_and_the_cursor_when_there_is_none() {
    // No filter: pointing moves the real cursor.
    let mut p = pane(&["a.png", "b.png", "c.png"]);
    p.senalar(2);
    assert_eq!(p.cursor(), 2);
    assert_eq!(
        p.selected().map(|e| e.path.to_wire()),
        Some("mem:///c.png".to_owned())
    );

    // With a filter: the SELECTION moves, not the cursor.
    //
    // The listing gets SORTED, so the indices are the already-sorted list's:
    // `alfa.png` (0), `alto.png` (1), `zeta.png` (2). And the filter matches
    // by SUBSTRING, which is why the query is `al` and not `a`: with `a`
    // alone `zeta.png` would also match and nothing would be left out to
    // test.
    let mut p = pane(&["alfa.png", "zeta.png", "alto.png"]);
    p.quick_start(crate::nav::Mode::Filter);
    p.quick_char('a');
    p.quick_char('l'); // visible: "alfa.png" (0) and "alto.png" (1)
    let cursor_before = p.cursor();
    assert_eq!(
        p.selected().map(|e| e.path.to_wire()),
        Some("mem:///alfa.png".to_owned()),
        "the filter starts on its first match"
    );
    p.senalar(1);
    assert_eq!(
        p.selected().map(|e| e.path.to_wire()),
        Some("mem:///alto.png".to_owned()),
        "pointing moved the FILTER's selection"
    );
    assert_eq!(p.cursor(), cursor_before, "and not the real cursor");

    // A row the filter does not show cannot be pointed at: nothing is
    // touched.
    p.senalar(2);
    assert_eq!(
        p.selected().map(|e| e.path.to_wire()),
        Some("mem:///alto.png".to_owned()),
        "\"zeta.png\" is filtered out: the selection stays where it was"
    );
}

/// A pane over a subdirectory, which is where the `..` row appears.
fn child_pane(names: &[&str]) -> PaneState {
    let es = names
        .iter()
        .map(|n| e(&format!("mem:///casa/{n}"), EntryKind::File))
        .collect();
    let mut p = PaneState::new(VPath::parse("mem:///casa").unwrap(), es);
    p.set_parent_row(true);
    p
}

/// The organize operand leaves out the `..` row and the directories.
///
/// Both things were seen while piloting phase 8: the tree proposed moving
/// the PARENT DIRECTORY into a new folder —the trap `real_entries` already
/// documents, and that whoever reads `entries()` falls into— and it proposed
/// moving a folder that another move of the same plan used as a
/// destination, so what got applied stopped being what was reviewed.
#[test]
fn organizing_sees_neither_the_parent_nor_the_directories() {
    let mut p = child_pane(&[]);
    // A real folder among the entries.
    p.set_listing(
        VPath::parse("mem:///casa").unwrap(),
        vec![
            e("mem:///casa/a.txt", EntryKind::File),
            e("mem:///casa/fotos", EntryKind::Dir),
            e("mem:///casa/b.txt", EntryKind::File),
        ],
    );
    assert!(p.is_parent_row(0), "the test's pillar: the row is there");
    assert_eq!(
        p.organizable_names(),
        vec!["a.txt".to_owned(), "b.txt".to_owned()],
        "neither the parent nor the folder go into what is about to move"
    );
    // The names that ALREADY occupy the directory do count the folder —they
    // are exactly the ones that tell a new one apart from one that was
    // there— and still do not count the parent.
    // In the listing's order, which puts directories first.
    assert_eq!(
        p.existing_names(),
        vec!["fotos".to_owned(), "a.txt".to_owned(), "b.txt".to_owned()]
    );
}

/// The `..` row is FIRST, and at a root it does not appear: there is nowhere
/// to go up to, and a row that leads nowhere is worse than not having it.
#[test]
fn the_go_up_row_comes_first_and_is_not_at_the_root() {
    let p = child_pane(&["a", "b"]);
    assert_eq!(p.entries().len(), 3, "the two entries and the go-up one");
    assert!(p.is_parent_row(0));
    assert!(!p.is_parent_row(1));
    assert_eq!(p.parent_target(), Some(&VPath::parse("mem:///").unwrap()));

    let mut root = pane(&["a"]);
    root.set_parent_row(true);
    assert_eq!(root.entries().len(), 1, "at the root there is no go-up row");
    assert!(!root.is_parent_row(0));
    assert_eq!(root.parent_target(), None);
}

/// And it is NOT an operand. This is the invariant that makes the row safe:
/// eighty-seven call sites ask "what is pointed at" to copy or delete it,
/// and over it the answer is "nothing".
#[test]
fn the_go_up_row_is_not_an_operand() {
    let mut p = child_pane(&["a"]);
    assert_eq!(p.cursor(), 0, "the cursor is born over it");
    assert!(
        p.selected().is_none(),
        "over `..` nothing is pointed at: if it were, F8 would delete the parent"
    );
    p.cursor_down();
    assert!(p.selected().is_some(), "and over a real entry, it is");
}

/// But it CAN be described: "what does this act on" and "what is pointed
/// at" are two questions, and over `..` they have different answers.
///
/// The panels that follow the cursor —the attribute sheet, the docked
/// viewer— used to ask the first one and say "nothing under the cursor"
/// while a row sat right there. In a freshly opened window the cursor is
/// born over `..`, so the details panel started ALWAYS empty and looked
/// broken.
#[test]
fn the_go_up_row_is_not_an_operand_but_can_be_described() {
    let mut p = child_pane(&["a"]);
    assert!(p.selected().is_none(), "not an operand");
    let under = p
        .cursor_entry()
        .expect("but there is a row under the cursor");
    assert_eq!(
        under.path,
        VPath::parse("mem:///").unwrap(),
        "and it is the one that leads to the parent"
    );
    assert_eq!(under.kind, EntryKind::Dir);

    p.cursor_down();
    assert_eq!(
        p.cursor_entry().map(|e| &e.path),
        p.selected().map(|e| &e.path),
        "over a real entry both questions answer the same"
    );
}

/// And with no rows there is nothing to describe either.
#[test]
fn with_no_rows_there_is_nothing_under_the_cursor() {
    let p = PaneState::new(VPath::parse("mem:///casa").unwrap(), Vec::new());
    assert!(p.cursor_entry().is_none());
}

/// The quick search in Filter mode does NOT turn the `..` row into an
/// operand either.
///
/// `QuickSearch::new` folds `entries` WHOLE, synthetic row included, and
/// with an empty query the selection is born at index 0. The guard used to
/// look at `self.cursor`, which does not move in Filter, so opening the
/// filter was enough for `selected()` to return the PARENT directory: F8
/// over it deletes the parent, which is exactly what this row exists to
/// prevent.
#[test]
fn the_filter_does_not_turn_the_go_up_row_into_an_operand() {
    let mut p = child_pane(&["a", "b"]);
    p.quick_start(crate::nav::Mode::Filter);
    assert!(
        p.selected().is_none(),
        "the freshly opened filter points at row 0, which is `..`: {:?}",
        p.selected().map(|e| e.path.to_wire())
    );
    // And that is how it reached the delete: with no marks,
    // `marked_paths()` falls back to `selected()`, and F8 opens the modal
    // with the first thing on that list.
    assert!(
        p.marked_paths().is_empty(),
        "the parent cannot be F8's operand: {:?}",
        p.marked_paths()
    );
    assert_eq!(
        p.cursor_entry().map(|e| &e.path),
        Some(&VPath::parse("mem:///").unwrap()),
        "but describing it does work"
    );
}

/// And the "this is the go-up row" flag follows what is POINTED AT, not the
/// real cursor.
///
/// With the filter choosing a real entry and the cursor still at 0, the
/// attribute sheet used to ask about the cursor and describe `..` while the
/// listing highlighted a different row — and the hostile name the reader
/// was looking at did not get marked.
#[test]
fn the_go_up_flag_follows_what_is_pointed_at() {
    let mut p = child_pane(&["a", "b"]);
    assert!(p.cursor_is_parent_row(), "with no filter, the cursor rules");

    p.quick_start(crate::nav::Mode::Filter);
    p.quick_char('b');
    assert_eq!(p.cursor(), 0, "in Filter the REAL cursor does not move");
    assert!(
        !p.cursor_is_parent_row(),
        "but what is pointed at is `b`, not `..`"
    );
    assert_eq!(
        p.cursor_entry().and_then(|e| e.path.file_name()),
        Some(&norte_proto::Segment::new(b"b".to_vec()).unwrap()),
    );
}

/// It cannot be marked either, through NONE of the paths that mark.
///
/// Marking it would put the PARENT directory into the list of what gets
/// copied or deleted, which is the worst form of this bug. The test walks
/// every door: the cursor's, the bulk one, the range one, a single row's,
/// inverting, and the pattern.
#[test]
fn the_go_up_row_is_not_marked_through_any_path() {
    let parent = VPath::parse("mem:///").unwrap();
    let mut p = child_pane(&["a", "b"]);

    p.toggle_mark(); // the cursor is over `..`
    p.mark_all();
    p.mark_range(0, 2);
    p.set_mark(0, true);
    p.invert_marks();
    let _ = p.mark_glob("*", true);
    assert!(
        !p.marked_paths().contains(&parent),
        "the parent NEVER goes into what is marked: {:?}",
        p.marked_paths()
    );
    // And everything else does get marked: the guard protects one row, it
    // does not break marking.
    assert_eq!(p.marks_len(), 2, "the two real entries");
}

/// What gets COPIED to a new pane does not carry it.
///
/// `entries()` includes it —it is the list that gets painted, and index 0
/// is what `is_parent_row` answers about— so copying it to another pane
/// turned it into a real entry: the new pane set up ITS OWN on top and the
/// inherited one stayed in the middle of the listing, with the parent's
/// name and markable.
#[test]
fn real_entries_leaves_out_the_go_up_row() {
    let p = child_pane(&["a", "b"]);
    assert_eq!(
        p.entries().len(),
        3,
        "what gets painted carries the go-up one"
    );
    assert_eq!(p.real_entries().len(), 2, "what gets copied does not");
    assert!(
        !p.real_entries()
            .iter()
            .any(|x| x.path == VPath::parse("mem:///").unwrap()),
        "{:?}",
        p.real_entries()
    );

    let mut root = pane(&["a"]);
    root.set_parent_row(true);
    assert_eq!(
        root.real_entries().len(),
        root.entries().len(),
        "at a root there is no row to remove"
    );
}

/// The cursor's target: the folder if it is one, and this directory
/// otherwise. Over `..`, this directory — never the parent.
#[test]
fn target_dir_is_the_folder_under_the_cursor_and_otherwise_its_own() {
    let casa = VPath::parse("mem:///casa").unwrap();
    let mut p = PaneState::new(
        casa.clone(),
        vec![
            e("mem:///casa/dir", EntryKind::Dir),
            e("mem:///casa/f.txt", EntryKind::File),
            e("mem:///casa/enlace", EntryKind::Symlink),
        ],
    );
    p.set_parent_row(true);

    assert!(p.is_parent_row(p.cursor()), "the cursor is born over `..`");
    assert_eq!(
        p.target_dir(),
        &casa,
        "over `..`, this path, not the parent"
    );

    p.cursor_down();
    assert_eq!(
        p.target_dir(),
        &VPath::parse("mem:///casa/dir").unwrap(),
        "over a folder, that folder"
    );

    p.cursor_down();
    assert_eq!(p.target_dir(), &casa, "over a file, this path");

    p.cursor_down();
    assert_eq!(
        p.target_dir(),
        &casa,
        "a link is not followed (M0): this path"
    );
}

/// And if an entry with the parent's path sneaks in anyway, it does not get
/// marked either.
///
/// Defense in depth, and not the main one: the main one is not copying it
/// ([`PaneState::real_entries`]). This is the net that turns the next slip
/// into "nothing happens" instead of deleting the parent. It is safe to look
/// at the PATH here and not in `is_parent_row`: an entry's path in this
/// directory is always `dir/name`, so only the synthetic row —or a copy of
/// it— can be exactly the parent; a link to the parent has its own.
#[test]
fn an_entry_with_the_parents_path_is_not_marked() {
    let parent = VPath::parse("mem:///").unwrap();
    let mut p = PaneState::new(
        VPath::parse("mem:///casa").unwrap(),
        vec![
            e("mem:///", EntryKind::Dir),
            e("mem:///casa/a", EntryKind::File),
        ],
    );
    p.set_parent_row(true);
    p.mark_all();
    p.mark_range(0, p.entries().len().saturating_sub(1));
    p.invert_marks();
    let _ = p.mark_glob("*", true);
    assert!(
        !p.marked_paths().contains(&parent),
        "the parent does not go into what is marked, not even sneaked in as an entry: {:?}",
        p.marked_paths()
    );
}

/// Re-sorting does not move it from its spot: it goes first, it is not
/// sorted with the rest. Sorting by size would send it to the middle of the
/// listing.
#[test]
fn the_go_up_row_stays_first_after_re_sorting() {
    use crate::sort::{SortColumn, SortDir, SortSpec};
    let mut p = child_pane(&["a", "b", "c"]);
    p.set_sort(SortSpec {
        column: SortColumn::Size,
        dir: SortDir::Desc,
        dirs_first: false,
    });
    assert!(p.is_parent_row(0), "still first");
    assert_eq!(p.entries().len(), 4);
}

/// And a paginated fill neither duplicates it nor loses it: it comes out of
/// the merge and comes back afterwards, because the merge pairs by sort key.
#[test]
fn a_fill_does_not_duplicate_the_go_up_row() {
    let mut p = child_pane(&["b"]);
    p.extend(vec![e("mem:///casa/a", EntryKind::File)]);
    p.extend(vec![e("mem:///casa/c", EntryKind::File)]);
    let up = p
        .entries()
        .iter()
        .filter(|x| x.path == VPath::parse("mem:///").unwrap())
        .count();
    assert_eq!(up, 1, "a single go-up row: {:?}", p.entries());
    assert!(p.is_parent_row(0));
    assert_eq!(p.entries().len(), 4);
}

/// Turning it off removes it, and turning it on brings it back, without
/// touching the listing.
#[test]
fn it_can_be_turned_off_and_on() {
    let mut p = child_pane(&["a"]);
    assert_eq!(p.entries().len(), 2);
    p.set_parent_row(false);
    assert_eq!(p.entries().len(), 1, "only the real entry");
    assert!(!p.is_parent_row(0));
    p.set_parent_row(true);
    assert!(p.is_parent_row(0));
}

#[test]
fn set_sort_resorts_re_anchors_and_extends_under_the_spec() {
    use crate::sort::{SortColumn, SortDir, SortSpec};
    let mk = |n: &str, size: Option<u64>| {
        let mut e = e(&format!("mem:///{n}"), EntryKind::File);
        e.size = size;
        e
    };
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![mk("a", Some(30)), mk("b", Some(10)), mk("c", Some(20))],
    );
    p.cursor_down(); // "b"
    p.toggle_mark(); // marks "b"
    let spec = SortSpec {
        column: SortColumn::Size,
        dir: SortDir::Asc,
        dirs_first: true,
    };
    p.set_sort(spec);
    let order: Vec<_> = p.entries().iter().map(|e| e.path.clone()).collect();
    assert_eq!(
        order,
        vec![
            VPath::parse("mem:///b").unwrap(),
            VPath::parse("mem:///c").unwrap(),
            VPath::parse("mem:///a").unwrap()
        ]
    );
    assert_eq!(
        p.selected().map(|e| e.path.clone()),
        Some(VPath::parse("mem:///b").unwrap()),
        "cursor re-anchored by path"
    );
    assert_eq!(p.marks_len(), 1, "marks go by identity");

    // A fill that arrives AFTERWARDS merges under the active spec.
    p.set_loading(true);
    p.extend(vec![mk("d", Some(15))]);
    let order: Vec<_> = p.entries().iter().map(|e| e.path.clone()).collect();
    assert_eq!(
        order,
        vec![
            VPath::parse("mem:///b").unwrap(),
            VPath::parse("mem:///d").unwrap(),
            VPath::parse("mem:///c").unwrap(),
            VPath::parse("mem:///a").unwrap()
        ],
        "the batch enters its position under size/asc"
    );
}

/// #107: hiding is PRESENTATION — the provider lists everything, the pane
/// sets the leading-dot ones aside into a stash and returns them when shown,
/// merged in order (reuses `extend`). Only the LAST segment decides:
/// `a.txt` is not hidden.
#[test]
fn hiding_sets_dotfiles_aside_and_showing_returns_them_in_order() {
    let mut p = pane(&[".git", "a.txt", ".hidden", "b"]);
    assert_eq!(p.entries().len(), 4);
    assert!(p.show_hidden(), "default: everything is shown");
    p.set_show_hidden(false);
    let names: Vec<_> = p.entries().iter().map(|e| e.path.clone()).collect();
    assert_eq!(
        names,
        vec![
            VPath::parse("mem:///a.txt").unwrap(),
            VPath::parse("mem:///b").unwrap()
        ],
        "only the last segment starting with '.' is hidden"
    );
    assert_eq!(p.hidden_count(), 2);
    p.set_show_hidden(true);
    assert_eq!(p.entries().len(), 4, "showing restores ALL of them");
    assert_eq!(p.hidden_count(), 0);
    // And the order is canonical again (merge, not append).
    let first = p.entries().first().map(|e| e.path.clone());
    assert_eq!(first, Some(VPath::parse("mem:///.git").unwrap()));
}

#[test]
fn a_new_listing_under_hiding_filters_on_entry() {
    let mut p = pane(&["x"]);
    p.set_show_hidden(false);
    p.set_listing(
        VPath::parse("mem:///sub").unwrap(),
        vec![
            e("mem:///sub/.env", EntryKind::File),
            e("mem:///sub/main.rs", EntryKind::File),
        ],
    );
    assert_eq!(p.entries().len(), 1);
    assert_eq!(p.hidden_count(), 1);
    p.set_show_hidden(true);
    assert_eq!(p.entries().len(), 2);
}

#[test]
fn a_paginated_fill_under_hiding_sets_the_batch_aside() {
    let mut p = pane(&["a"]);
    p.set_show_hidden(false);
    p.set_loading(true);
    p.extend(vec![
        e("mem:///.b", EntryKind::File),
        e("mem:///c", EntryKind::File),
    ]);
    assert_eq!(p.entries().len(), 2, "a + c");
    assert_eq!(p.hidden_count(), 1);
    // A batch of hidden entries ONLY breaks nothing.
    p.extend(vec![e("mem:///.d", EntryKind::File)]);
    assert_eq!(p.entries().len(), 2);
    assert_eq!(p.hidden_count(), 2);
}

/// Hiding PRUNES the marks of entries that disappear from view (same
/// discipline as `refill`, #103): an invisible selection feeding the next
/// F8 is exactly the hazard the `pruned_marks` counter exists to make
/// noisy.
#[test]
fn hiding_prunes_dotfile_marks_and_reports_it() {
    let mut p = pane(&[".secret", "a"]);
    p.mark_all();
    assert_eq!(p.marks_len(), 2);
    p.set_show_hidden(false);
    assert_eq!(p.marks_len(), 1, "the .secret mark drops");
    assert_eq!(p.pruned_marks(), 1, "and NEVER silently");
    assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///a").unwrap()]);
}

#[test]
fn hiding_re_anchors_the_cursor_by_path() {
    let mut p = pane(&[".a", ".b", "c"]);
    p.cursor_down();
    p.cursor_down(); // "c"
    p.set_show_hidden(false);
    assert_eq!(p.entries().len(), 1);
    assert_eq!(p.cursor(), 0);
    assert_eq!(
        p.selected().map(|e| e.path.clone()),
        Some(VPath::parse("mem:///c").unwrap()),
        "the cursor stays on the SAME visible entry"
    );
}

#[test]
fn refill_under_hiding_replaces_the_stash_without_duplicating() {
    let mut p = pane(&[".a", "b"]);
    p.set_show_hidden(false);
    assert_eq!(p.hidden_count(), 1);
    p.refill(vec![
        e("mem:///.a", EntryKind::File),
        e("mem:///.z", EntryKind::File),
        e("mem:///b", EntryKind::File),
    ]);
    assert_eq!(p.entries().len(), 1);
    assert_eq!(p.hidden_count(), 2, "a FRESH refill stash, no dup");
    p.set_show_hidden(true);
    assert_eq!(p.entries().len(), 3, "no duplicates after showing");
}

/// Rule 1: the decision is by BYTES of the last segment — a non-UTF8 name
/// starting with `.` (0x2E) is hidden all the same; a hostile one that does
/// not stays visible.
#[test]
fn hiding_decides_by_bytes_not_by_text() {
    let dir = VPath::parse("mem:///").unwrap();
    let dot_hostile = dir
        .clone()
        .join(norte_proto::Segment::new(b".\xff\xfe".to_vec()).unwrap());
    let plain_hostile = dir
        .clone()
        .join(norte_proto::Segment::new(b"\xff\xfe".to_vec()).unwrap());
    let mk = |p: &VPath| Entry {
        attrs: std::collections::BTreeMap::new(),
        path: p.clone(),
        kind: EntryKind::File,
        size: None,
        mtime_ms: None,
    };
    let mut p = PaneState::new(dir, vec![mk(&dot_hostile), mk(&plain_hostile)]);
    p.set_show_hidden(false);
    assert_eq!(p.entries().len(), 1);
    assert_eq!(p.entries()[0].path, plain_hostile);
    p.set_show_hidden(true);
    assert_eq!(p.entries().len(), 2, "the bytes come back intact");
}

#[test]
fn the_cursor_moves_with_a_clamp() {
    let mut p = pane(&["a", "b", "c"]);
    assert_eq!(p.cursor(), 0);
    p.cursor_up(); // clamps at 0
    assert_eq!(p.cursor(), 0);
    p.cursor_down();
    p.cursor_down();
    assert_eq!(p.cursor(), 2);
    p.cursor_down(); // clamps at len-1
    assert_eq!(p.cursor(), 2);
    p.home();
    assert_eq!(p.cursor(), 0);
    p.end();
    assert_eq!(p.cursor(), 2);
}

#[test]
fn selected_honours_the_quick_filter() {
    let mut p = pane(&["alfa", "beta", "alto"]);
    p.quick_start(crate::nav::Mode::Filter);
    p.quick_char('a');
    // "alfa" and "alto" match; selected is the first filtered one.
    assert_eq!(
        p.selected().unwrap().path,
        VPath::parse("mem:///alfa").unwrap()
    );
    p.quick_cancel();
    assert_eq!(
        p.selected().unwrap().path,
        VPath::parse("mem:///alfa").unwrap()
    );
}

#[test]
fn set_listing_resets_the_cursor_and_closes_the_quick_search() {
    let mut p = pane(&["a", "b"]);
    p.cursor_down();
    p.quick_start(crate::nav::Mode::Filter);
    p.set_listing(
        VPath::parse("mem:///otro").unwrap(),
        vec![e("mem:///otro/x", EntryKind::File)],
    );
    assert_eq!(p.cursor(), 0);
    assert!(p.quick_visible().is_none());
    assert_eq!(p.dir(), &VPath::parse("mem:///otro").unwrap());
}

#[test]
fn the_page_moves_with_a_clamp() {
    let mut p = pane(&["a", "b", "c"]);
    p.page_down(100); // clamps at len-1
    assert_eq!(p.cursor(), 2);
    p.page_up(100); // clamps at 0
    assert_eq!(p.cursor(), 0);

    let mut empty = pane(&[]);
    empty.page_down(100); // no-op, no panic
    assert_eq!(empty.cursor(), 0);
    empty.page_up(100);
    assert_eq!(empty.cursor(), 0);
}

#[test]
fn begin_loading_leaves_transitory_state() {
    let mut p = pane(&["a", "b", "c"]);
    p.cursor_down();
    p.begin_loading(VPath::parse("mem:///nuevo").unwrap());
    assert!(p.entries().is_empty());
    assert!(p.loading());
    assert!(p.selected().is_none());
    assert_eq!(p.dir(), &VPath::parse("mem:///nuevo").unwrap());

    p.set_listing(
        VPath::parse("mem:///nuevo").unwrap(),
        vec![e("mem:///nuevo/x", EntryKind::File)],
    );
    assert!(!p.loading());
    assert_eq!(p.cursor(), 0);
    assert!(p.selected().is_some());
}

#[test]
fn end_on_an_empty_list_does_not_panic() {
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), vec![]);
    p.home();
    p.end();
    p.cursor_down();
    p.cursor_up();
    assert_eq!(p.cursor(), 0);
    assert!(p.selected().is_none());
}

#[test]
fn toggle_marks_and_unmarks_the_entry_under_the_cursor() {
    let mut p = pane(&["a", "b", "c"]);
    p.cursor_down(); // cursor on "b"
    assert_eq!(p.marks_len(), 0);
    p.toggle_mark();
    assert_eq!(p.marks_len(), 1);
    assert!(p.is_marked(&e("mem:///b", EntryKind::File)));
    assert!(!p.is_marked(&e("mem:///a", EntryKind::File)));
    p.toggle_mark(); // unmarks
    assert_eq!(p.marks_len(), 0);
    assert!(!p.is_marked(&e("mem:///b", EntryKind::File)));
}

#[test]
fn marked_paths_with_no_marks_returns_the_cursors_target() {
    let mut p = pane(&["a", "b", "c"]);
    p.cursor_down(); // "b"
    assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///b").unwrap()]);
}

#[test]
fn marked_paths_with_marks_in_entries_order() {
    let mut p = pane(&["a", "b", "c"]);
    p.cursor_down();
    p.cursor_down();
    p.toggle_mark(); // marks "c"
    p.home();
    p.toggle_mark(); // marks "a"
    // Order = `entries`' (deterministic), not insertion order.
    assert_eq!(
        p.marked_paths(),
        vec![
            VPath::parse("mem:///a").unwrap(),
            VPath::parse("mem:///c").unwrap(),
        ]
    );
}

#[test]
fn set_listing_clears_the_marks() {
    let mut p = pane(&["a", "b"]);
    p.toggle_mark();
    assert_eq!(p.marks_len(), 1);
    p.set_listing(
        VPath::parse("mem:///otro").unwrap(),
        vec![e("mem:///otro/x", EntryKind::File)],
    );
    assert_eq!(p.marks_len(), 0);
}

#[test]
fn begin_loading_clears_the_marks() {
    let mut p = pane(&["a", "b"]);
    p.toggle_mark();
    p.begin_loading(VPath::parse("mem:///nuevo").unwrap());
    assert_eq!(p.marks_len(), 0);
}

#[test]
fn toggle_under_a_filter_marks_the_visible_selection() {
    let mut p = pane(&["alfa", "beta", "alto"]);
    p.quick_start(crate::nav::Mode::Filter);
    p.quick_char('a'); // "alfa" and "alto" visible; selection = "alfa"
    p.toggle_mark();
    assert!(p.is_marked(&e("mem:///alfa", EntryKind::File)));
    assert!(!p.is_marked(&e("mem:///beta", EntryKind::File)));
}

#[test]
fn mark_identity_is_by_the_paths_bytes_hostile_name() {
    // A name with NON-UTF8 bytes (0xFF): the mark tells it apart by its
    // exact VPath, without degrading to lossy (rule 1).
    let hostile = VPath::parse("mem:///")
        .unwrap()
        .join(norte_proto::Segment::new(vec![0xFF, 0xFE]).unwrap());
    let benign = VPath::parse("mem:///a").unwrap();
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: hostile.clone(),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            },
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: benign.clone(),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            },
        ],
    );
    // #54: `new` normalises — sorting by raw bytes puts "a" (0x61) before
    // 0xFF, so the hostile one ends up at index 1, not at cursor 0.
    p.cursor_down();
    p.toggle_mark(); // marks the hostile one
    assert!(p.marks.contains(&hostile));
    assert!(!p.marks.contains(&benign));
    assert_eq!(p.marked_paths(), vec![hostile]);
}

#[test]
fn clear_marks_empties_the_set() {
    let mut p = pane(&["a", "b"]);
    p.toggle_mark();
    p.cursor_down();
    p.toggle_mark();
    assert_eq!(p.marks_len(), 2);
    p.clear_marks();
    assert_eq!(p.marks_len(), 0);
}

#[test]
fn marked_paths_with_no_entries_and_no_marks_is_empty() {
    let p = PaneState::new(VPath::parse("mem:///").unwrap(), vec![]);
    assert!(p.marked_paths().is_empty());
}

#[test]
fn marks_tell_nfc_and_nfd_twins_apart_without_folding() {
    // é in NFC (0xC3 0xA9) vs NFD (0x65 0xCC 0x81): different bytes, same
    // visual shape. The mark must NOT fold them (macOS trap: preserve bytes).
    let nfc = VPath::parse("mem:///")
        .unwrap()
        .join(norte_proto::Segment::new(vec![0xC3, 0xA9]).unwrap());
    let nfd = VPath::parse("mem:///")
        .unwrap()
        .join(norte_proto::Segment::new(vec![0x65, 0xCC, 0x81]).unwrap());
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: nfc.clone(),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            },
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: nfd.clone(),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            },
        ],
    );
    p.toggle_mark();
    p.cursor_down();
    p.toggle_mark();
    assert_eq!(p.marks_len(), 2, "nfc and nfd are TWO different marks");
    assert!(p.marks.contains(&nfc));
    assert!(p.marks.contains(&nfd));
}

// --- S1: per-directory cursor memory + pending focus (spec 2026-07-24
// §S1) ---------------------------------------------------

/// Basic round trip: leave a dir with the cursor moved, navigate to
/// another, come back — the cursor is restored where it was left (not at
/// 0).
#[test]
fn cursor_memory_basic_round_trip() {
    let mut p = pane(&["a", "b", "c"]);
    p.set_cursor(2); // "c"
    p.remember_cursor(); // simulates begin_loading's capture point
    p.set_listing(
        VPath::parse("mem:///otro").unwrap(),
        vec![e("mem:///otro/x", EntryKind::File)],
    );
    assert_eq!(p.cursor(), 0, "new dir, no memory: the usual 0");

    // Back to the original dir: begin_loading (here simulated with
    // remember_cursor + set_listing, same as the real GUI) must restore
    // the remembered cursor.
    p.remember_cursor();
    p.set_listing(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///a", EntryKind::File),
            e("mem:///b", EntryKind::File),
            e("mem:///c", EntryKind::File),
        ],
    );
    assert_eq!(p.cursor(), 2, "restores the cursor remembered for mem:///");
}

/// If the remembered dir's listing shrank, the restore clamps.
#[test]
fn cursor_memory_restores_with_a_clamp_if_it_shrank() {
    let mut p = pane(&["a", "b", "c"]);
    p.set_cursor(2); // "c"
    p.remember_cursor();
    p.set_listing(VPath::parse("mem:///otro").unwrap(), vec![]);
    p.remember_cursor();
    // We go back to "mem:///" but now with only 1 entry.
    p.set_listing(
        VPath::parse("mem:///").unwrap(),
        vec![e("mem:///a", EntryKind::File)],
    );
    assert_eq!(p.cursor(), 0, "clamp: only index 0 is available");
}

/// LRU: past the cap (64), the oldest entry is dropped.
#[test]
fn cursor_memory_lru_evicts_past_the_cap() {
    let mut p = PaneState::new(VPath::parse("mem:///d0").unwrap(), vec![]);
    // 65 different dirs, each with cursor=7 (arbitrary, the clamp does not
    // matter here: each listing has a single entry, but what is remembered
    // is the raw value before set_cursor's clamp).
    for i in 0..65 {
        p.set_cursor(7); // internal clamp does not matter: empty listing -> 0
        p.remember_cursor();
        p.set_listing(VPath::parse(&format!("mem:///d{}", i + 1)).unwrap(), vec![]);
    }
    // The entry for "mem:///d0" (the very first one, before the loop) must
    // have been evicted: if it was not, going back to "mem:///d0" with a
    // 65-entry listing would restore the cursor to an index != 0.
    let mut entries = Vec::new();
    for i in 0..65 {
        entries.push(e(&format!("mem:///d0/x{i:02}"), EntryKind::File));
    }
    p.remember_cursor();
    p.set_listing(VPath::parse("mem:///d0").unwrap(), entries);
    assert_eq!(
        p.cursor(),
        0,
        "d0 was evicted from the LRU memory (cap 64), there is nothing to restore"
    );
}

/// The anchor is BYTE for BYTE, and with the two normalization twins in
/// front, it is clear why it matters (#122).
///
/// `é` in NFC (`c3a9`) and `é` in NFD (`65cc81`) paint the same and are two
/// different files. A semantic hit —or any other `pending_focus`— over the
/// NFD one has to land on the NFD one. The day someone adds a "helpful"
/// `nfc()` to this comparison, the cursor will land on the other file:
/// on macOS and on SMB, where the two really do coexist, that means
/// opening, copying, or deleting the wrong one.
#[test]
fn the_anchor_tells_normalization_twins_apart() {
    let nfc = "mem:///caf\u{e9}.txt";
    let nfd = "mem:///cafe\u{301}.txt";
    assert_ne!(
        VPath::parse(nfc).unwrap().to_wire(),
        VPath::parse(nfd).unwrap().to_wire(),
        "the twins are TWO paths: if this fails, the test below proves nothing"
    );
    for wanted in [nfd, nfc] {
        let mut p = pane(&["otro"]);
        p.set_pending_focus(VPath::parse(wanted).unwrap());
        p.set_listing(
            VPath::parse("mem:///").unwrap(),
            vec![e(nfc, EntryKind::File), e(nfd, EntryKind::File)],
        );
        // By the row's PATH and not by an index: `set_listing` re-sorts,
        // and a hand-written index would test the sort's order, not the
        // anchor.
        assert_eq!(
            p.entries()[p.cursor()].path.to_wire(),
            VPath::parse(wanted).unwrap().to_wire(),
            "the anchor for {wanted} landed on the wrong twin"
        );
    }
}

/// The marked names, in order, to read an assertion at a glance.
fn marked(p: &PaneState) -> Vec<String> {
    p.marked_entries()
        .iter()
        .map(|e| String::from_utf8_lossy(e.path.file_name().unwrap().as_bytes()).into_owned())
        .collect()
}

/// `shift+↑`: the exact mirror of `space`. Marks the cursor's row and moves
/// UP, without wrapping at the first one.
#[test]
fn toggle_mark_and_retreat_is_the_mirror_of_advance() {
    let mut p = pane(&["a", "b", "c", "d"]);
    p.set_cursor(2); // "c"
    p.toggle_mark_and_retreat();
    p.toggle_mark_and_retreat();
    assert_eq!(marked(&p), ["b", "c"]);
    assert_eq!(p.cursor(), 0);
    // At the first row it does not wrap: it marks and stays.
    p.toggle_mark_and_retreat();
    assert_eq!(marked(&p), ["a", "b", "c"]);
    assert_eq!(p.cursor(), 0);
}

/// **The stretch is decided by the CURSOR's row, not each row.**
///
/// This is what makes the gesture reversible —repeating it undoes what it
/// did— and what gives Far's phrase its meaning: "to deselect, hold Shift
/// and move in the opposite direction". If each row toggled on its own, a
/// half-marked stretch would end up alternating.
#[test]
fn a_page_marks_or_unmarks_based_on_the_cursors_row() {
    let mut p = pane(&["a", "b", "c", "d", "e"]);
    p.toggle_mark_page(3, true);
    assert_eq!(marked(&p), ["a", "b", "c", "d"]);
    assert_eq!(p.cursor(), 3);

    // The cursor is now on "d", which IS marked: the same gesture upward
    // unmarks instead of marking.
    p.toggle_mark_page(3, false);
    assert_eq!(marked(&p), Vec::<String>::new());
    assert_eq!(p.cursor(), 0);
}

/// The stretch reaches as far as the CURSOR gets, not as far as requested:
/// against the edge, `n` rows are fewer than `n`.
#[test]
fn a_page_against_the_edge_marks_only_what_it_travels() {
    let mut p = pane(&["a", "b", "c"]);
    p.set_cursor(1);
    p.toggle_mark_page(10, true);
    assert_eq!(marked(&p), ["b", "c"], "never wraps at the start");
    assert_eq!(p.cursor(), 2);
}

/// **Krusader `Shift+Home`/`Shift+End`: mark one side and CLEAR the other.**
///
/// Literally from its documentation ("selects everything above the cursor
/// and deselects everything below the cursor, if selected"), and it is what
/// tells them apart from "add a stretch": whoever uses them to bound a
/// selection counts on what is outside going away.
#[test]
fn the_edge_gestures_clear_the_other_side() {
    let mut p = pane(&["a", "b", "c", "d", "e"]);
    p.set_cursor(4);
    p.toggle_mark(); // "e" marked by hand, on the other side of the cut
    p.set_cursor(1);

    p.mark_to_top();
    assert_eq!(marked(&p), ["a", "b"], "\"e\" had to go");
    assert_eq!(p.cursor(), 1, "the edge gesture does NOT move the cursor");

    p.mark_to_bottom();
    assert_eq!(marked(&p), ["b", "c", "d", "e"], "and now \"a\" goes");
}

/// The `..` row is not marked through any of the new paths either: it is
/// the same door (`markable_indices`/`mark`) that already keeps it out.
#[test]
fn the_parent_does_not_get_in_through_the_new_paths() {
    let mut p = child_pane(&["a", "b"]);
    p.set_parent_row(true);
    p.set_cursor(0); // the `..` row
    p.mark_to_bottom();
    assert!(
        !marked(&p).iter().any(|n| n == ".."),
        "the parent is never an operand: {:?}",
        marked(&p)
    );
    p.set_cursor(2);
    p.mark_to_top();
    assert!(
        p.marked_entries()
            .iter()
            .all(|e| Some(&e.path) != p.parent_target()),
        "not even marking upward from below"
    );
}

/// `set_pending_focus` wins over the memory and is consumed only once.
#[test]
fn pending_focus_wins_over_memory_and_is_consumed_once() {
    let mut p = pane(&["a", "b", "c"]);
    p.set_cursor(2); // "c" — this will stay in memory for "mem:///"
    p.remember_cursor();
    p.set_listing(
        VPath::parse("mem:///a").unwrap(),
        vec![e("mem:///a/x", EntryKind::File)],
    );
    // Pending focus toward "b" on returning to "mem:///".
    p.set_pending_focus(VPath::parse("mem:///b").unwrap());
    p.remember_cursor();
    p.set_listing(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///a", EntryKind::File),
            e("mem:///b", EntryKind::File),
            e("mem:///c", EntryKind::File),
        ],
    );
    assert_eq!(
        p.selected().unwrap().path,
        VPath::parse("mem:///b").unwrap(),
        "pending_focus wins over the memory (which pointed at \"c\")"
    );

    // Second round, WITHOUT setting pending_focus again: if the hint had
    // not been consumed, it would still win and we would land on "b" again
    // no matter what. We move the cursor to "a" (index 0) before leaving so
    // the memory predicts a result DIFFERENT from "b" — only the memory
    // (not a ghost pending_focus) explains the result.
    p.set_cursor(0); // "a"
    p.remember_cursor(); // overwrites "mem:///"'s memory to 0
    p.set_listing(
        VPath::parse("mem:///a").unwrap(),
        vec![e("mem:///a/x", EntryKind::File)],
    );
    p.remember_cursor();
    p.set_listing(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///a", EntryKind::File),
            e("mem:///b", EntryKind::File),
            e("mem:///c", EntryKind::File),
        ],
    );
    assert_eq!(
        p.selected().unwrap().path,
        VPath::parse("mem:///a").unwrap(),
        "consumed: the second round uses the memory (a), not the old pending_focus (b)"
    );
}

/// Review S, M2: a `cd` that FAILS must not leave a ghost `pending_focus`
/// alive for a future, unrelated `set_listing` — `clear_pending_focus`
/// (called by the caller on the `cd`'s error branch) discards it WITHOUT
/// consuming it against any listing.
#[test]
fn clear_pending_focus_discards_the_hint_without_a_listing() {
    let mut p = pane(&["a", "b", "c"]);
    p.set_pending_focus(VPath::parse("mem:///b").unwrap());
    p.clear_pending_focus();
    // A LATER `set_listing` (the `cd`'s retry, or a completely different
    // one) does NOT land on "b": there is no memory for "mem:///" in this
    // fresh pane, so the cursor falls to the usual 0 — if the hint had
    // survived, "b" would still win.
    p.set_listing(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///a", EntryKind::File),
            e("mem:///b", EntryKind::File),
            e("mem:///c", EntryKind::File),
        ],
    );
    assert_eq!(p.cursor(), 0, "the discarded hint must not win");
}

/// Hostile bytes (segment 0xFF/0xFE, the corpus's wire form): the memory
/// and `pending_focus` identify by EXACT byte PATH, not normalised (rule 1
/// — hostile twins are never folded).
#[test]
fn cursor_memory_and_pending_focus_are_byte_exact_with_a_hostile_path() {
    let root = VPath::parse("mem:///").unwrap();
    let hostile = root
        .clone()
        .join(norte_proto::Segment::new(vec![0xFF, 0xFE]).unwrap());
    let benign = root
        .clone()
        .join(norte_proto::Segment::new(b"a".to_vec()).unwrap());
    let mut p = PaneState::new(
        root.clone(),
        vec![
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: hostile.clone(),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            },
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: benign.clone(),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            },
        ],
    );
    // "new" normalises: 0xFF > 0x61 => hostile ends up at index 1.
    p.set_cursor(1);
    assert_eq!(p.selected().unwrap().path, hostile);

    // Simulates entering the hostile dir (cd) and leaving again
    // (parent-nav): the hint is set AFTER entering, right before going back
    // to the parent — same as real `nav.parent` (spec §S1 point 3).
    p.remember_cursor();
    p.set_listing(hostile.clone(), vec![]);
    p.set_pending_focus(hostile.clone());
    p.remember_cursor();
    p.set_listing(
        root,
        vec![
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: hostile.clone(),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            },
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: benign,
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            },
        ],
    );
    assert_eq!(
        p.selected().unwrap().path,
        hostile,
        "pending_focus matches by exact bytes, without folding the hostile form"
    );
}

/// `begin_loading` captures the OLD dir (and its cursor) before
/// overwriting the state with the new destination — it is the real
/// capture point for the GUI (`PaneState::begin_loading` is called BEFORE
/// the async fetch).
#[test]
fn begin_loading_records_the_old_dir_before_overwriting_it() {
    let mut p = pane(&["a", "b", "c"]);
    p.set_cursor(2); // "c" in "mem:///"
    p.begin_loading(VPath::parse("mem:///nuevo").unwrap());
    assert_eq!(p.cursor(), 0, "the destination starts at 0 while loading");
    // set_listing of the SAME new dir must not alter what was recorded for
    // the old dir: going back to "mem:///" restores the cursor
    // begin_loading recorded, not a corrupted value.
    p.set_listing(
        VPath::parse("mem:///nuevo").unwrap(),
        vec![e("mem:///nuevo/x", EntryKind::File)],
    );
    p.remember_cursor();
    p.set_listing(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///a", EntryKind::File),
            e("mem:///b", EntryKind::File),
            e("mem:///c", EntryKind::File),
        ],
    );
    assert_eq!(
        p.cursor(),
        2,
        "begin_loading recorded (mem:///, 2) before overwriting the dir"
    );
}

#[test]
fn set_cursor_sets_with_a_clamp() {
    let mut p = pane(&["a", "b", "c"]);
    p.set_cursor(2);
    assert_eq!(p.cursor(), 2);
    p.set_cursor(99); // clamps at len-1
    assert_eq!(p.cursor(), 2);
    p.set_cursor(0);
    assert_eq!(p.cursor(), 0);

    let mut empty = pane(&[]);
    empty.set_cursor(5); // no-op, no panic
    assert_eq!(empty.cursor(), 0);
}

#[test]
fn set_loading_toggles_the_flag() {
    let mut p = pane(&["a"]);
    assert!(!p.loading());
    p.set_loading(true);
    assert!(p.loading());
    p.set_loading(false);
    assert!(!p.loading());
}

#[test]
fn quick_next_moves_the_real_cursor_with_wrap() {
    // #54: `new` normalises (dirs first, alphabetical within the group).
    // "aa"(dir) and "ac"(file) match 'a'; "bb"(dir) stays in the middle (no
    // match) to keep testing that Tab SKIPS the non-matching one in between.
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///aa", EntryKind::Dir),
            e("mem:///bb", EntryKind::Dir),
            e("mem:///ac", EntryKind::File),
        ],
    );
    p.quick_start(crate::nav::Mode::Jump);
    p.quick_char('a');
    assert_eq!(p.cursor(), 0, "jumps to the first match");
    assert!(
        p.quick_visible().is_none(),
        "in jump mode the whole listing shows"
    );
    p.quick_next();
    assert_eq!(
        p.cursor(),
        2,
        "Tab: next match, skips the non-matching one in between"
    );
    p.quick_next();
    assert_eq!(p.cursor(), 0, "wrap");
}

#[test]
fn the_quick_getter_exposes_the_live_query() {
    let mut p = pane(&["a"]);
    assert!(p.quick().is_none());
    p.quick_start(crate::nav::Mode::Filter);
    p.quick_char('a');
    assert_eq!(p.quick().unwrap().mode(), crate::nav::Mode::Filter);
    p.quick_cancel();
    assert!(p.quick().is_none());
}

/// `extend` re-sorts EVERYTHING and re-anchors the cursor to the selected
/// PATH.
#[test]
fn extend_re_sorts_and_re_anchors_by_path() {
    let mut first = vec![
        e("mem:///m", EntryKind::File),
        e("mem:///z", EntryKind::File),
    ];
    crate::sort_entries(&mut first);
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), first);
    p.set_cursor(1); // "z"
    p.extend(vec![
        e("mem:///a", EntryKind::File),
        e("mem:///b", EntryKind::File),
    ]);
    let order: Vec<_> = p
        .entries()
        .iter()
        .map(|e| e.path.file_name().unwrap().as_bytes().to_vec())
        .collect();
    assert_eq!(
        order,
        vec![b"a".to_vec(), b"b".to_vec(), b"m".to_vec(), b"z".to_vec()]
    );
    assert_eq!(
        p.selected().unwrap().path,
        VPath::parse("mem:///z").unwrap(),
        "the selection follows the path despite the re-sort"
    );
}

/// #124: the viewport's height is reported back by the frontend after
/// painting, and from it come the page jump (one screen minus one row of
/// context) and the stat probe's radius. With no frame painted, the
/// fallbacks rule.
#[test]
fn the_painted_viewport_rules_the_page_and_the_probe() {
    let lazy = |n: &str| {
        let mut x = e(&format!("mem:///{n}"), EntryKind::File);
        x.size = None;
        x
    };
    let entries: Vec<Entry> = (0..100).map(|i| lazy(&format!("f{i:03}"))).collect();
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), entries);
    assert_eq!(p.viewport_rows(), None, "not painted yet");
    assert_eq!(
        p.page_step(),
        DEFAULT_PAGE,
        "no frame: the historical fallback"
    );
    assert_eq!(
        p.needs_stat_window(3).len(),
        4,
        "with no frame the caller's fallback rules: cursor 0 ± 3"
    );

    p.set_viewport_rows(30);
    assert_eq!(p.viewport_rows(), Some(30));
    assert_eq!(p.page_step(), 29, "one screen minus one row of context");
    assert_eq!(
        p.needs_stat_window(3).len(),
        31,
        "the radius becomes the REAL height, not the fallback"
    );

    // A one-row pane still advances (never a jump of 0).
    p.set_viewport_rows(1);
    assert_eq!(p.page_step(), 1);
    // Covered pane (viewer open): the fallback rules again.
    p.set_viewport_rows(0);
    assert_eq!(p.viewport_rows(), None);
    assert_eq!(p.page_step(), DEFAULT_PAGE);
}

/// #123: `needs_stat_at` filters an explicit ABSOLUTE range (the one the
/// GUI gets from `uniform_list`), with the same criterion as the
/// radius-based window: only `File`s with no `size`, and indices outside
/// the listing are ignored instead of blowing up.
#[test]
fn needs_stat_at_filters_the_explicit_range() {
    let lazy = |n: &str| {
        let mut x = e(&format!("mem:///{n}"), EntryKind::File);
        x.size = None;
        x
    };
    let mut already = lazy("b");
    already.size = Some(7);
    let mut dir = lazy("c");
    dir.kind = EntryKind::Dir;
    // `PaneState::new` sorts (dirs first): [c, a, b, d].
    let p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![lazy("a"), already, dir, lazy("d")],
    );
    let paths = p.needs_stat_at(0..99);
    let names: Vec<String> = paths
        .iter()
        .map(norte_proto::VPath::display_lossy)
        .collect();
    assert!(
        names.iter().any(|n| n.ends_with("/a")) && names.iter().any(|n| n.ends_with("/d")),
        "the lazy Files of the range: {names:?}"
    );
    assert_eq!(
        paths.len(),
        2,
        "neither the Dir nor the already-hydrated one: {names:?}"
    );
    assert!(
        p.needs_stat_at(50..99).is_empty(),
        "a range outside the listing contributes nothing"
    );
}

/// The cursor AT THE TOP stays at the top while the listing fills in: a
/// paginated dir's first page arrives in `readdir` order (FS hash), so its
/// SORTED first element is arbitrary — anchoring by path there pinned the
/// cursor in the middle of the final listing (in a 5000-file dir, the pane
/// opened showing the TAIL instead of the start). Anchoring by path still
/// applies as soon as the user moves the cursor.
#[test]
fn extend_with_the_cursor_at_the_top_leaves_it_at_the_top() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![e("mem:///m", EntryKind::File)],
    );
    assert_eq!(p.cursor(), 0);
    p.extend(vec![
        e("mem:///a", EntryKind::File),
        e("mem:///b", EntryKind::File),
    ]);
    assert_eq!(p.cursor(), 0, "the cursor stays on the first row");
    assert_eq!(
        p.selected().unwrap().path,
        VPath::parse("mem:///a").unwrap(),
        "and that row is the REAL start of the already-merged listing"
    );
}

/// #54: the incremental merge produces EXACTLY the same order as
/// `sort_entries` over the whole thing (dirs first, NFC, tie-break by
/// bytes) — including mixed NFD/NFC and non-UTF8.
#[test]
fn extend_merge_is_equivalent_to_a_full_sort() {
    let batches: Vec<Vec<Entry>> = vec![
        vec![
            e("mem:///zeta", EntryKind::File),
            e("mem:///Adir", EntryKind::Dir),
        ],
        vec![e("mem:///an%CC%83o", EntryKind::File)], // NFD
        vec![
            e("mem:///a%C3%B1o2", EntryKind::File), // NFC
            e("mem:///%FF%FE", EntryKind::File),    // non-UTF8
        ],
        vec![e("mem:///Bdir", EntryKind::Dir)],
        // Overlong (>255 bytes): the order has no special path by length,
        // but let it be pinned into the equivalence.
        vec![e(&format!("mem:///{}", "x".repeat(300)), EntryKind::File)],
    ];
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), Vec::new());
    for batch in batches.clone() {
        p.extend(batch);
    }
    let mut flat: Vec<Entry> = batches.into_iter().flatten().collect();
    crate::sort_entries(&mut flat);
    assert_eq!(p.entries(), flat.as_slice(), "merge ≡ full sort");
}

/// The name histogram `extend` SUMS per batch is the same as measuring the
/// whole listing at once (ADR 0124): summing is the optimisation, not a
/// different result.
#[test]
fn extend_measures_names_the_same_as_all_at_once() {
    let batches: Vec<Vec<Entry>> = vec![
        vec![e("mem:///corto", EntryKind::File)],
        vec![
            e("mem:///un-nombre-bastante-largo.png", EntryKind::File),
            e("mem:///%FF%FE", EntryKind::File),
        ],
        vec![e("mem:///otro-nombre-largo-ya.pdf", EntryKind::File)],
    ];
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), Vec::new());
    for batch in batches.clone() {
        p.extend(batch);
    }
    let all_at_once = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        batches.into_iter().flatten().collect(),
    );
    assert_eq!(p.name_width_p80(), all_at_once.name_width_p80());
    assert!(p.name_width_p80() >= 20, "the long one counts");
}

/// The "sort them first" contract stops being a footgun: `set_listing`/`new`
/// normalise internally (keys + order) — an unsorted caller no longer
/// breaks the merge's invariant.
#[test]
fn set_listing_normalises_even_when_it_arrives_unsorted() {
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), Vec::new());
    p.set_listing(
        VPath::parse("mem:///d").unwrap(),
        vec![
            e("mem:///d/z", EntryKind::File),
            e("mem:///d/a", EntryKind::File),
        ],
    );
    assert_eq!(p.entries()[0].path, VPath::parse("mem:///d/a").unwrap());
    // And the later extend still merges fine over that base.
    p.extend(vec![e("mem:///d/m", EntryKind::File)]);
    let names: Vec<_> = p.entries().iter().map(|x| x.path.clone()).collect();
    assert_eq!(
        names,
        vec![
            VPath::parse("mem:///d/a").unwrap(),
            VPath::parse("mem:///d/m").unwrap(),
            VPath::parse("mem:///d/z").unwrap(),
        ]
    );
}

/// NFC key tie between batches (same normalised form, different raw bytes:
/// NFD in batch 1 vs NFC in batch 2) — the tie-break is decided by raw
/// `name_bytes`, same as `sort_entries`, NOT the merge's arrival order
/// (which only breaks a LEFT=exact key tie, and here the NFC keys match but
/// the bytes do not).
#[test]
fn extend_breaks_ties_by_raw_bytes_same_as_a_full_sort() {
    let nfd = e("mem:///an%CC%83o", EntryKind::File); // "año" NFD
    let nfc = e("mem:///a%C3%B1o", EntryKind::File); // "año" NFC
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), Vec::new());
    p.extend(vec![nfd.clone()]);
    p.extend(vec![nfc.clone()]);
    let mut flat = vec![nfd, nfc];
    crate::sort_entries(&mut flat);
    assert_eq!(
        p.entries(),
        flat.as_slice(),
        "the NFC key tie between batches resolves the same as sort_entries"
    );
}

/// ADVERSARIAL A (mutation, encoding review #54): the NFC one arrives
/// BEFORE the NFD one — arrival order CONTRADICTS the raw-byte tie-break
/// (NFD `61 6E CC 83` < NFC `61 C3 B1`). A merge with no
/// `.then_with(bytes)` would pass the twin test above (there, arrival and
/// bytes agree) but dies here.
#[test]
fn adversarial_nfc_arrives_before_nfd() {
    let nfd = e("mem:///an%CC%83o", EntryKind::File);
    let nfc = e("mem:///a%C3%B1o", EntryKind::File);
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), Vec::new());
    p.extend(vec![nfc.clone()]);
    p.extend(vec![nfd.clone()]);
    let mut flat = vec![nfc, nfd];
    crate::sort_entries(&mut flat);
    assert_eq!(p.entries(), flat.as_slice());
    assert_eq!(
        p.entries()[0].path,
        VPath::parse("mem:///an%CC%83o").unwrap(),
        "NFD first by raw bytes, not by arrival order"
    );
}

/// ADVERSARIAL B (mutation, encoding review #54): NFC↔bytes inversion.
/// NFD "ñu" = `6E CC 83 75`, "o" = `6F`: by NFC key (`C3 B1 75`) ñu > o,
/// but by raw bytes ñu < o. A `cmp_keyed` that uses the bytes as the
/// PRIMARY key (ignoring the persisted NFC) inverts the order — spec §6.1
/// breaks on macOS/NFD without the rest of the suite noticing. Crosses a
/// batch boundary on purpose.
#[test]
fn adversarial_nfc_vs_bytes_inversion_across_batches() {
    let nfd_enye = e("mem:///n%CC%83u", EntryKind::File);
    let o = e("mem:///o", EntryKind::File);
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), Vec::new());
    p.extend(vec![nfd_enye.clone()]);
    p.extend(vec![o.clone()]);
    let mut flat = vec![nfd_enye, o];
    crate::sort_entries(&mut flat);
    assert_eq!(p.entries(), flat.as_slice());
    assert_eq!(
        p.entries()[0].path,
        VPath::parse("mem:///o").unwrap(),
        "'o' first: the primary key is NFC, not the raw bytes"
    );
}

#[test]
fn extend_with_an_empty_batch_is_a_noop() {
    let mut p = pane(&["a"]);
    p.extend(vec![]);
    assert_eq!(p.entries().len(), 1);
    assert_eq!(p.cursor(), 0);
}

/// `extend` re-applies the live filter over the new listing.
#[test]
fn extend_re_applies_the_filter() {
    let mut p = pane(&["a1"]);
    p.quick_start(crate::nav::Mode::Filter);
    p.quick_char('a');
    p.extend(vec![
        e("mem:///a2", EntryKind::File),
        e("mem:///zz", EntryKind::File),
    ]);
    assert_eq!(
        p.quick_visible().unwrap().len(),
        2,
        "a2 gets in, zz does not"
    );
}

/// `refill` keeps the cursor by clamped index and re-applies the filter.
#[test]
fn refill_keeps_the_cursor_by_clamped_index() {
    let mut p = pane(&["a", "b", "c"]);
    p.set_cursor(2); // "c"
    p.refill(vec![
        e("mem:///a", EntryKind::File),
        e("mem:///b", EntryKind::File),
    ]);
    assert_eq!(p.cursor(), 1, "clamps to the new listing's last entry");
    assert_eq!(p.entries().len(), 2);
}

#[test]
fn refill_keeps_marks_of_entries_that_survive() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///a", EntryKind::File),
            e("mem:///b", EntryKind::File),
        ],
    );
    p.cursor_down(); // "b"
    p.toggle_mark();
    p.refill(vec![
        e("mem:///a", EntryKind::File),
        e("mem:///b", EntryKind::File),
    ]);
    assert_eq!(p.marks_len(), 1);
    assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///b").unwrap()]);
}

#[test]
fn refill_prunes_a_mark_whose_entry_vanished() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///a", EntryKind::File),
            e("mem:///b", EntryKind::File),
        ],
    );
    p.cursor_down();
    p.toggle_mark();
    p.refill(vec![e("mem:///a", EntryKind::File)]);
    assert_eq!(
        p.marks_len(),
        0,
        "a mark is a claim about an entry that exists"
    );
}

#[test]
fn refill_prunes_only_the_marks_whose_entry_vanished() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///a", EntryKind::File),
            e("mem:///b", EntryKind::File),
            e("mem:///c", EntryKind::File),
        ],
    );
    p.toggle_mark(); // "a"
    p.cursor_down();
    p.cursor_down();
    p.toggle_mark(); // "c"
    p.refill(vec![
        e("mem:///a", EntryKind::File),
        e("mem:///b", EntryKind::File),
    ]);
    assert_eq!(p.marks_len(), 1);
    assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///a").unwrap()]);
}

#[test]
fn refill_reports_how_many_marks_it_pruned() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///a", EntryKind::File),
            e("mem:///b", EntryKind::File),
        ],
    );
    p.toggle_mark(); // "a"
    p.cursor_down();
    p.toggle_mark(); // "b"
    p.refill(vec![e("mem:///a", EntryKind::File)]);
    assert_eq!(p.pruned_marks(), 1);
    assert_eq!(p.marks_len(), 1);
}

#[test]
fn a_fully_pruned_selection_falls_back_to_the_cursor_entry() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///a", EntryKind::File),
            e("mem:///b", EntryKind::File),
        ],
    );
    p.cursor_down(); // "b"
    p.toggle_mark();
    p.refill(vec![e("mem:///a", EntryKind::File)]);
    assert_eq!(p.pruned_marks(), 1);
    // Documented consequence, NOT an endorsement: with the set empty the
    // fallback takes over, so the caller must check `pruned_marks()`
    // before treating `marked_paths()` as "what the user selected".
    assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///a").unwrap()]);
}

#[test]
fn a_mark_placed_mid_fill_survives_the_rest_of_the_fill() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![e("mem:///b", EntryKind::File)],
    );
    p.set_loading(true);
    p.toggle_mark();
    p.extend(vec![e("mem:///a", EntryKind::File)]);
    p.set_loading(false);
    assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///b").unwrap()]);
}

#[test]
fn set_listing_to_the_same_dir_still_clears_marks() {
    // Deliberate, not a bug: `set_listing` means "a listing arrived for a
    // directory I navigated to" — even a `cd` that lands back on the SAME
    // dir clears marks. Only `refill` means "refresh" and preserves what
    // survives; this is the case neither `set_listing_clears_the_marks`
    // (different dir) nor the `refill` tests (same dir, but via `refill`)
    // cover.
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![e("mem:///a", EntryKind::File)],
    );
    p.toggle_mark();
    assert_eq!(p.marks_len(), 1);
    p.set_listing(
        VPath::parse("mem:///").unwrap(),
        vec![e("mem:///a", EntryKind::File)],
    );
    assert_eq!(p.marks_len(), 0);
}

/// The refresh must NOT be able to empty the columns. A fresh listing of
/// the SAME dir arrives WITHOUT size/mtime (lazy stat, #52), so installing
/// it as-is leaves the size and date cells blank until the probe fills
/// them: with the watcher (#106) refreshing on every event, that is
/// constant flicker. `refill` inherits by path what was already known.
#[test]
fn refill_inherits_already_known_size_and_mtime() {
    let mut entries = vec![
        e("mem:///a", EntryKind::File),
        e("mem:///b", EntryKind::File),
    ];
    entries[0].size = Some(42);
    entries[0].mtime_ms = Some(1000);
    crate::sort_entries(&mut entries);
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), entries);

    // What an `fs.list` of the same dir returns: bare.
    p.refill(vec![
        e("mem:///a", EntryKind::File),
        e("mem:///b", EntryKind::File),
    ]);

    let a = p
        .entries()
        .iter()
        .find(|x| x.path == VPath::parse("mem:///a").unwrap())
        .expect("a is still in the listing");
    assert_eq!(a.size, Some(42), "the known size survives the refresh");
    assert_eq!(a.mtime_ms, Some(1000), "and so does the date");
    let b = p
        .entries()
        .iter()
        .find(|x| x.path == VPath::parse("mem:///b").unwrap())
        .expect("b is still in the listing");
    assert_eq!(b.size, None, "what was never known stays unknown");
}

/// The inheritance above must not be able to freeze a stale value: the
/// fresh listing wins when it DOES bring the data (a provider that knows
/// it), and the later probe always wins — otherwise a growing file would
/// forever show the size it was listed with the first time.
#[test]
fn fresh_data_beats_inheritance_and_the_probe_beats_both() {
    let mut entries = vec![e("mem:///a", EntryKind::File)];
    entries[0].size = Some(42);
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), entries);

    let mut fresh = e("mem:///a", EntryKind::File);
    fresh.size = Some(100);
    p.refill(vec![fresh]);
    assert_eq!(p.entries()[0].size, Some(100), "the fresh listing wins");

    p.hydrate(&VPath::parse("mem:///a").unwrap(), Some(7), Some(7));
    assert_eq!(p.entries()[0].size, Some(7), "the probe is authoritative");
    p.hydrate(&VPath::parse("mem:///a").unwrap(), None, None);
    assert_eq!(
        p.entries()[0].size,
        Some(7),
        "a probe that knows nothing never erases what IS known"
    );
}

/// #52: hydrate by path fills in the live entry's size/mtime; an unknown
/// path is a no-op; the order does not change (size/mtime do not sort).
#[test]
fn hydrate_fills_in_without_re_sorting_and_is_a_noop_if_absent() {
    let mut entries = vec![
        e("mem:///a", EntryKind::File),
        e("mem:///b", EntryKind::File),
        e("mem:///c", EntryKind::File),
    ];
    entries[1].size = Some(999); // "b" was already hydrated
    crate::sort_entries(&mut entries);
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), entries);

    p.hydrate(&VPath::parse("mem:///a").unwrap(), Some(5), Some(1000));
    // The probe OVERWRITES whatever there was: it just looked at the file.
    // It used to keep the previous value, and that froze a size inherited
    // from before the refresh (see `inherit_known_metadata`).
    p.hydrate(&VPath::parse("mem:///b").unwrap(), Some(1), Some(2));
    p.hydrate(&VPath::parse("mem:///no-existe").unwrap(), Some(7), Some(7)); // no-op

    let order: Vec<_> = p
        .entries()
        .iter()
        .map(|e| e.path.file_name().unwrap().as_bytes().to_vec())
        .collect();
    assert_eq!(
        order,
        vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()],
        "hydrate does not re-sort"
    );

    let a = p
        .entries()
        .iter()
        .find(|e| e.path == VPath::parse("mem:///a").unwrap())
        .unwrap();
    assert_eq!(a.size, Some(5));
    assert_eq!(a.mtime_ms, Some(1000));

    let b = p
        .entries()
        .iter()
        .find(|e| e.path == VPath::parse("mem:///b").unwrap())
        .unwrap();
    assert_eq!(
        b.size,
        Some(1),
        "the freshly measured data beats what was already there"
    );

    let c = p
        .entries()
        .iter()
        .find(|e| e.path == VPath::parse("mem:///c").unwrap())
        .unwrap();
    assert_eq!(c.size, None, "with no hydrate for c, it stays None");
}

/// `refresh_quick` re-applies the filter without touching entries or the
/// real cursor.
#[test]
fn refresh_quick_does_not_touch_entries_or_the_cursor() {
    let mut p = pane(&["a1", "a2"]);
    p.quick_start(crate::nav::Mode::Filter);
    p.quick_char('a');
    let before: Vec<_> = p.entries().to_vec();
    let cur = p.cursor();
    p.refresh_quick();
    assert_eq!(p.entries(), before.as_slice());
    assert_eq!(p.cursor(), cur);
    assert_eq!(p.quick_visible().unwrap().len(), 2);
}

/// #98/F1 (corpus fixture `cp866_papka`): the quick search matches against
/// the text the user SEES. With IBM866 reinterpretation active, typing "п"
/// finds the entry painted "Папка" — and the fold cache is invalidated on
/// BOTH paths: a live quick search when cycling (`set_name_encoding`) and
/// a quick search started afterwards (`new` with enc).
#[test]
fn the_quick_search_matches_against_the_reinterpreted_text() {
    let papka = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "cp866_papka")
        .expect("corpus fixture")
        .bytes;
    let dir = VPath::parse("mem:///").unwrap();
    let seg = norte_proto::Segment::new(papka).unwrap();
    let entries = vec![
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(seg),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
        },
        e("mem:///otro.txt", EntryKind::File),
    ];

    // Path 1: LIVE quick search, then cycle — the fold re-folds.
    let mut p = PaneState::new(dir.clone(), entries.clone());
    p.quick_start(Mode::Filter);
    p.quick_char('\u{043f}'); // п
    assert_eq!(
        p.quick_visible().map(<[usize]>::len),
        Some(0),
        "without reinterpreting, п does not match the lossy form"
    );
    // Cycles to IBM866 (the suggestion for these samples).
    assert_eq!(p.cycle_name_encoding(), Some("IBM866"));
    assert_eq!(
        p.quick_visible().map(<[usize]>::len),
        Some(1),
        "with IBM866 the filter matches \"Папка\""
    );

    // Path 2: cycle first, quick search afterwards (folds are born with enc).
    let mut p = PaneState::new(dir, entries);
    assert_eq!(p.cycle_name_encoding(), Some("IBM866"));
    p.quick_start(Mode::Filter);
    p.quick_char('\u{043f}');
    assert_eq!(p.quick_visible().map(<[usize]>::len), Some(1));
}

// --- #103 task 2: mark all, invert, and the marked byte total -------

/// Asymmetric starting state (#103 T9 review debt): pre-mark "a" before
/// calling `mark_all`. `mark_all` only ADDS, so "a" stays marked and "b"
/// gets added — total 2. A body accidentally rewritten to call
/// `invert_marks` instead would FLIP "a" back off (it was already
/// marked) while still marking "b" — total 1 — and this assertion would
/// catch it; starting from an empty set cannot tell the two apart.
#[test]
fn mark_all_marks_every_entry() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///a", EntryKind::File),
            e("mem:///b", EntryKind::File),
        ],
    );
    p.toggle_mark(); // pre-marks "a" (asymmetric start)
    p.mark_all();
    assert_eq!(p.marks_len(), 2);
}

#[test]
fn invert_marks_flips_every_entry() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///a", EntryKind::File),
            e("mem:///b", EntryKind::File),
        ],
    );
    p.toggle_mark(); // marks "a"
    p.invert_marks();
    assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///b").unwrap()]);
}

/// Asymmetric starting state (#103 T9 review debt): pre-mark "alfa"
/// (the ONLY entry the filter leaves visible) before filtering and
/// calling `mark_all`. `mark_all` is idempotent on an already-marked
/// visible entry, so it stays marked — total 1. A body accidentally
/// rewritten to call `invert_marks` instead would FLIP it back off —
/// total 0 — and this assertion would catch it; starting from an empty
/// set cannot tell the two apart (both give 1).
#[test]
fn mark_all_under_a_filter_only_marks_the_visible() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///alfa", EntryKind::File),
            e("mem:///beta", EntryKind::File),
        ],
    );
    p.toggle_mark(); // pre-marks "alfa" (asymmetric start)
    p.quick_start(Mode::Filter);
    p.quick_char('a');
    p.quick_char('l'); // matches "alfa" only
    p.mark_all();
    assert_eq!(
        p.marks_len(),
        1,
        "marked_paths falls back to the cursor: pin the SET"
    );
    assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///alfa").unwrap()]);
}

/// `invert` under a filter only flips the visible entries — the
/// counterpart of `mark_all_under_a_filter_only_marks_the_visible`
/// (nothing else pinned this direction).
#[test]
fn invert_under_a_filter_only_flips_the_visible() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///alfa", EntryKind::File),
            e("mem:///beta", EntryKind::File),
        ],
    );
    p.quick_start(Mode::Filter);
    p.quick_char('a');
    p.quick_char('l'); // only "alfa" visible
    p.invert_marks();
    assert_eq!(p.marks_len(), 1);
    let entries: Vec<_> = p.entries().to_vec();
    assert!(p.is_marked(&entries[0]), "alfa was visible: flipped");
    assert!(!p.is_marked(&entries[1]), "beta was hidden: untouched");
}

/// Marks OUTSIDE the visible set survive an invert untouched: invert is
/// "flip what you see", not "replace the selection with its complement".
#[test]
fn invert_under_a_filter_leaves_a_hidden_mark_alone() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///alfa", EntryKind::File),
            e("mem:///beta", EntryKind::File),
        ],
    );
    p.cursor_down(); // "beta"
    p.toggle_mark(); // marks "beta"
    p.quick_start(Mode::Filter);
    p.quick_char('a');
    p.quick_char('l'); // only "alfa" visible now
    p.invert_marks();
    assert_eq!(p.marks_len(), 2, "beta survives, alfa gets flipped on");
    assert!(p.is_marked(&e("mem:///alfa", EntryKind::File)));
    assert!(p.is_marked(&e("mem:///beta", EntryKind::File)));
}

/// `Mode::Jump` marks the WHOLE listing, not just the jump target: unlike
/// `Mode::Filter`, `quick_visible()` returns `None` in Jump, so
/// `markable_indices` falls through to the full range. This is intended
/// (a narrower `markable_indices` under Jump would also pass every other
/// test in this file), so it needs its own pin.
///
/// Asymmetric starting state (#103 T9 review debt): pre-mark "alfa"
/// before jumping and calling `mark_all`. Correct behavior keeps BOTH
/// entries marked (the pre-mark stays, "beta" gets added) — total ==
/// `entries().len()`. A body accidentally rewritten to call
/// `invert_marks` instead would flip "alfa" back off while still
/// marking "beta" — total 1, short of `entries().len()` — and this
/// assertion would catch it; starting from an empty set cannot (both
/// give the full length).
#[test]
fn mark_all_under_jump_marks_the_whole_listing() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///alfa", EntryKind::File),
            e("mem:///beta", EntryKind::File),
        ],
    );
    p.toggle_mark(); // pre-marks "alfa" (asymmetric start)
    p.quick_start(Mode::Jump);
    p.quick_char('a');
    p.mark_all();
    assert_eq!(p.marks_len(), p.entries().len());
}

/// #103 review BLOCKER/MAJOR fix: `toggle_mark_and_advance` (mc/Total
/// Commander sweep, `insert`) marks the FILTERED selection and advances
/// WITHIN the filter — the real cursor (invisible to the user) must
/// never move under a `Mode::Filter` quick search. Two presses under the
/// filter `a` (visible: `aa`, `ab`) mark exactly those two and never
/// touch the hidden `zz`.
#[test]
fn toggle_mark_and_advance_stays_inside_an_active_filter() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///aa", EntryKind::File),
            e("mem:///ab", EntryKind::File),
            e("mem:///zz", EntryKind::File),
        ],
    );
    p.quick_start(Mode::Filter);
    p.quick_char('a'); // visible: aa, ab

    p.toggle_mark_and_advance();
    p.toggle_mark_and_advance();

    assert_eq!(p.marks_len(), 2);
    assert!(p.is_marked(&e("mem:///aa", EntryKind::File)));
    assert!(p.is_marked(&e("mem:///ab", EntryKind::File)));
    assert!(
        !p.is_marked(&e("mem:///zz", EntryKind::File)),
        "zz is hidden by the filter: never marked"
    );
    assert_eq!(
        p.quick_visible().map(<[usize]>::len),
        Some(2),
        "the filter itself is untouched"
    );

    // The filter only has 2 visible rows, so the second press already
    // clamped at the last one ("ab") without wrapping. A third press
    // toggles "ab" back OFF — it never wraps onto the hidden "zz".
    p.toggle_mark_and_advance();
    assert!(
        p.is_marked(&e("mem:///aa", EntryKind::File)),
        "aa stays marked"
    );
    assert!(
        !p.is_marked(&e("mem:///ab", EntryKind::File)),
        "ab toggled back off, clamped at the last visible row"
    );
    assert!(
        !p.is_marked(&e("mem:///zz", EntryKind::File)),
        "sweeping never wraps onto a hidden entry"
    );
}

#[test]
fn marked_bytes_sums_files_and_ignores_dirs() {
    let mut a = e("mem:///a", EntryKind::File);
    a.size = Some(10);
    let mut d = e("mem:///d", EntryKind::Dir);
    d.size = Some(4096); // a provider may report a dir size; it must not count
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), vec![a, d]);
    p.mark_all();
    assert_eq!(p.marked_bytes(), 10);
}

/// A file that is NOT marked must not contribute to the total: both of
/// the tests above mark every entry, so neither would catch
/// `marked_bytes` silently dropping the `self.marks.contains(...)` guard
/// and summing the whole directory.
#[test]
fn marked_bytes_counts_only_what_is_marked() {
    let mut a = e("mem:///a", EntryKind::File);
    a.size = Some(10);
    let mut b = e("mem:///b", EntryKind::File);
    b.size = Some(32);
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), vec![a, b]);
    p.toggle_mark(); // "a" only
    assert_eq!(
        p.marked_bytes(),
        10,
        "an unmarked file must not be in the total"
    );
}

#[test]
fn marked_bytes_saturates_instead_of_overflowing() {
    let mut a = e("mem:///a", EntryKind::File);
    a.size = Some(u64::MAX);
    let mut b = e("mem:///b", EntryKind::File);
    b.size = Some(1);
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), vec![a, b]);
    p.mark_all();
    assert_eq!(
        p.marked_bytes(),
        u64::MAX,
        "a hostile listing must not panic in debug"
    );
}

#[test]
fn marked_dirs_counts_only_marked_directories() {
    let mut a = e("mem:///a", EntryKind::File);
    a.size = Some(10);
    let d = e("mem:///d", EntryKind::Dir);
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), vec![a, d]);
    assert_eq!(p.marked_dirs(), 0, "nothing marked yet");
    p.mark_all();
    assert_eq!(p.marked_dirs(), 1, "one of the two marked entries is a dir");
}

/// A directory that is NOT marked must not contribute: mirrors
/// `marked_bytes_counts_only_what_is_marked` for the dir counter.
#[test]
fn marked_dirs_ignores_unmarked_directories() {
    let f = e("mem:///a", EntryKind::File);
    let d = e("mem:///d", EntryKind::Dir);
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), vec![f, d]);
    // `sort_entries` puts directories first, so the file is at index 1
    // regardless of construction order.
    let file_idx = p
        .entries()
        .iter()
        .position(|entry| entry.kind == EntryKind::File)
        .expect("the file is in the listing");
    p.set_cursor(file_idx);
    p.toggle_mark(); // marks the file only, not the directory
    assert_eq!(p.marked_dirs(), 0);
}

// --- mouse T1: range and mark by index -------------------------------

/// The range marks in BOTH DIRECTIONS: a shift+click's or a sweep's anchor
/// can sit above or below the pointer, and the frontend must not have to
/// sort them before calling. Returns the marks it CHANGED (`mark_glob`'s
/// convention), not the total.
#[test]
fn mark_range_marks_in_both_directions() {
    let mut p = pane(&["a", "b", "c", "d"]);
    assert_eq!(p.mark_range(1, 2), 2, "b and c");
    p.clear_marks();
    assert_eq!(p.mark_range(2, 1), 2, "reversed, the SAME range");
    assert_eq!(p.marked_paths().len(), 2);
    assert!(p.is_marked(&e("mem:///b", EntryKind::File)));
    assert!(p.is_marked(&e("mem:///c", EntryKind::File)));
    assert!(!p.is_marked(&e("mem:///a", EntryKind::File)));
    assert!(!p.is_marked(&e("mem:///d", EntryKind::File)));
    // Only ADDS: re-marking what is already marked changes 0 even though
    // the selection is still full — the caller reads the counter, it does
    // not confuse it with the total.
    assert_eq!(p.mark_range(1, 2), 0);
    assert_eq!(p.marks_len(), 2);
}

/// Under a live filter the range reaches ONLY what is visible, same as
/// `mark_all`: what the user does not see does not get marked. Without
/// this rule, a range whose ends straddle an entry hidden by the filter
/// would mark it blindly, and the next bulk operation (copy, DELETE) would
/// widen onto a file nobody chose.
#[test]
fn mark_range_under_a_filter_only_marks_the_visible() {
    let mut p = pane(&["alfa", "beta", "alga"]);
    p.quick_start(Mode::Filter);
    p.quick_char('a');
    p.quick_char('l'); // leaves "alfa" and "alga" visible, hides "beta"
    let visible = p.quick_visible().expect("active filter").to_vec();
    assert_eq!(visible.len(), 2, "the filter leaves two");
    // Range over the WHOLE listing: the ends straddle the hidden one.
    assert_eq!(p.mark_range(0, p.entries().len() - 1), 2);
    assert!(p.is_marked(&e("mem:///alfa", EntryKind::File)));
    assert!(p.is_marked(&e("mem:///alga", EntryKind::File)));
    assert!(
        !p.is_marked(&e("mem:///beta", EntryKind::File)),
        "beta was hidden by the filter: never marked"
    );
    assert_eq!(
        p.marks_len(),
        2,
        "marked_paths falls back to the cursor: pin the SET"
    );
}

/// An empty listing (or indices out of range) = no-op, no panic: the
/// frontend resolves the index from the pointer's position and can arrive
/// late at a pane that just emptied out.
#[test]
fn mark_range_on_an_empty_listing_does_nothing() {
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), Vec::new());
    assert_eq!(p.mark_range(0, 0), 0);
    assert_eq!(p.mark_range(3, 9), 0);
    assert_eq!(p.marks_len(), 0);
    let mut q = pane(&["a"]);
    assert_eq!(
        q.mark_range(5, 7),
        0,
        "the whole range is outside the listing"
    );
    assert_eq!(q.marks_len(), 0);
}

/// A range that runs off the listing gets CLAMPED, not rejected: it is
/// exactly what a hit test in the blank area below the last row produces,
/// and rejecting it whole would turn a sweep to the end of the pane into a
/// no-op.
#[test]
fn mark_range_clamps_to_the_listing() {
    let mut p = pane(&["a", "b", "c"]);
    assert_eq!(p.mark_range(1, 999), 2, "marks up to the last entry");
    assert_eq!(p.marks_len(), 2);
    assert!(!p.is_marked(&e("mem:///a", EntryKind::File)));
}

/// `set_mark` names ONE row by index (what ctrl+click needs), unlike
/// `toggle_mark`, which only ever reaches the cursor. Out of range: no-op.
#[test]
fn set_mark_sets_and_clears_a_single_entry() {
    let mut p = pane(&["a", "b"]);
    p.set_mark(1, true);
    assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///b").unwrap()]);
    p.set_mark(1, false);
    assert_eq!(p.marks_len(), 0);
    p.set_mark(9, true);
    assert_eq!(p.marks_len(), 0, "index outside the listing: no-op");
}

/// Marking honours the filter and UNMARKING does not. The index comes from
/// an already-painted frame against a listing that is not stable (a fill
/// inserts, a refill prunes, a re-sort moves): it can name a different
/// entry, and under a filter one the user does not see. Marking too much
/// WIDENS the next bulk operation onto a file nobody chose; unmarking too
/// much only SHRINKS it. Only the first destroys data, so only the first
/// is rejected.
#[test]
fn set_mark_marks_only_the_visible_but_always_unmarks() {
    // Sorted: alfa(0), alga(1), beta(2), zeta(3).
    let mut p = pane(&["alfa", "beta", "alga", "zeta"]);
    p.set_mark(2, true); // "beta" marked BEFORE filtering
    p.quick_start(Mode::Filter);
    p.quick_char('a');
    p.quick_char('l'); // visible: alfa(0) and alga(1); hidden: beta, zeta
    // Asymmetric starting state (lesson from the #103 tests): the row
    // being marked must NOT already be marked, or marking it extra would
    // not change the total and the test would see nothing.
    p.set_mark(3, true);
    assert_eq!(
        p.marks_len(),
        1,
        "marking a hidden row ('zeta'): rejected — only 'beta' is left"
    );
    assert!(!p.is_marked(&e("mem:///zeta", EntryKind::File)));
    p.set_mark(2, false);
    assert_eq!(
        p.marks_len(),
        0,
        "unmarking a hidden row: always allowed (it only shrinks)"
    );
    p.set_mark(0, true);
    assert_eq!(p.marks_len(), 1, "the visible one does get marked");
}

// --- mouse T1 (fix): the sweep rubber-bands against its baseline -------------

/// The sweep GIVES BACK what the pointer overshot. An additive sweep
/// leaves marked everything it ever touched: overshooting by twenty rows
/// and coming back left seventeen files marked, and since the overshoot
/// happens at the viewport's edge under autoscroll, those rows are
/// precisely the ones that just scrolled out of sight — the next bulk
/// operation would act on invisible files the user pulled back from, with
/// no repair beyond one ctrl+click per row.
#[test]
fn the_sweep_gives_back_the_overshot_rows_on_retreat() {
    let names: Vec<String> = (0..10).map(|i| format!("f{i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut p = pane(&refs);
    p.begin_sweep();
    p.apply_sweep(2, 8); // the pointer overshoots to 8
    assert_eq!(p.marks_len(), 7);
    p.apply_sweep(2, 4); // and the user comes back
    assert_eq!(p.marks_len(), 3, "only 2, 3 and 4 stay marked");
    assert!(!p.is_marked(&e("mem:///f5", EntryKind::File)));
    assert!(!p.is_marked(&e("mem:///f8", EntryKind::File)));
}

/// The baseline is a snapshot of the mark SET: what was marked by hand
/// before the gesture survives every retreat. Without this, rubber-banding
/// the sweep would erase a selection the user built with ctrl+click.
#[test]
fn the_sweep_keeps_what_was_marked_before_the_gesture() {
    let mut p = pane(&["a", "b", "c", "d", "e"]);
    p.set_mark(0, true); // marked by hand, outside the sweep's range
    p.begin_sweep();
    p.apply_sweep(2, 4);
    assert_eq!(p.marks_len(), 4);
    p.apply_sweep(2, 2); // retreats all the way
    assert_eq!(
        p.marks_len(),
        2,
        "'a' (by hand) and 'c' (the anchor) remain"
    );
    assert!(p.is_marked(&e("mem:///a", EntryKind::File)));
    assert!(p.is_marked(&e("mem:///c", EntryKind::File)));
}

/// `begin_sweep` drops the previous gesture's baseline: without that, a
/// new sweep would restore the previous one's snapshot and silently erase
/// everything marked in between.
#[test]
fn begin_sweep_does_not_restore_the_previous_gestures_baseline() {
    let mut p = pane(&["a", "b", "c", "d"]);
    p.begin_sweep();
    p.apply_sweep(0, 1); // gesture 1: marks a, b
    p.set_mark(3, true); // ctrl+click between gestures
    p.begin_sweep(); // gesture 2
    p.apply_sweep(2, 2);
    assert_eq!(
        p.marks_len(),
        4,
        "a, b and d survive; c belongs to the sweep"
    );
    assert!(p.is_marked(&e("mem:///d", EntryKind::File)));
}

/// A listing change drops the baseline: restoring it over entries that
/// moved would resurrect marks the prune had already dropped.
#[test]
fn a_refill_drops_the_sweeps_baseline() {
    let mut p = pane(&["a", "b", "c"]);
    p.begin_sweep();
    p.apply_sweep(0, 2);
    assert_eq!(p.marks_len(), 3);
    // "c" disappears from the dir; the refill prunes its mark.
    p.refill(vec![
        e("mem:///a", EntryKind::File),
        e("mem:///b", EntryKind::File),
    ]);
    assert_eq!(p.pruned_marks(), 1);
    p.apply_sweep(0, 0); // the sweep is still armed and re-snapshots
    assert_eq!(p.marks_len(), 2, "a and b: 'c' does NOT come back");
    assert_eq!(p.entries().len(), 2);
}

/// The sweep still honours the filter (it goes through `mark_range`), and
/// the baseline keeps the hidden marks it cannot touch.
#[test]
fn the_sweep_under_a_filter_only_reaches_the_visible() {
    // The listing gets sorted: alfa(0), alga(1), beta(2).
    let mut p = pane(&["alfa", "beta", "alga"]);
    p.set_mark(2, true); // "beta", marked before filtering
    p.quick_start(Mode::Filter);
    p.quick_char('a');
    p.quick_char('l'); // visible: "alfa" (0) and "alga" (1)
    p.begin_sweep();
    p.apply_sweep(0, 2);
    assert_eq!(
        p.marks_len(),
        3,
        "the two visible ones + the earlier hidden one"
    );
    p.apply_sweep(0, 0);
    assert_eq!(
        p.marks_len(),
        2,
        "releases 'alga'; the hidden 'beta' survives"
    );
    assert!(p.is_marked(&e("mem:///beta", EntryKind::File)));
}

/// The sweep re-marks what comes BACK into the range. Marking and
/// unmarking by delta (only what enters and what leaves) is what makes a
/// motion cost one row and not a pass over the listing, but a badly closed
/// delta would leave unmarked gaps in the middle of the range — invisible
/// until the bulk operation skipped a file.
#[test]
fn the_sweep_re_marks_what_comes_back_into_the_range() {
    let names: Vec<String> = (0..10).map(|i| format!("f{i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut p = pane(&refs);
    p.begin_sweep();
    p.apply_sweep(2, 8);
    p.apply_sweep(2, 4); // retreats: releases 5..8
    p.apply_sweep(2, 6); // and advances again
    assert_eq!(p.marks_len(), 5, "2..6 with no gaps");
    for i in 2..=6 {
        assert!(
            p.is_marked(&e(&format!("mem:///f{i}"), EntryKind::File)),
            "f{i} must still be marked"
        );
    }
    assert!(!p.is_marked(&e("mem:///f7", EntryKind::File)));
}

/// `quick_visible`'s indices come in ASCENDING order. This is not a
/// detail: `is_markable` looks them up with BINARY search, so a day when
/// `nav` returned a different order would stop the filter applying to
/// single rows — silently marking what the user does not see.
#[test]
fn quick_visible_comes_in_ascending_order() {
    let mut p = pane(&["alfa", "beta", "alga", "zeta", "algo"]);
    p.quick_start(Mode::Filter);
    p.quick_char('a');
    p.quick_char('l');
    let vis = p.quick_visible().expect("active filter");
    assert!(vis.len() > 1, "several are needed to see the order");
    assert!(
        vis.windows(2).all(|w| w[0] < w[1]),
        "ascending indices with no repeats: {vis:?}"
    );
}

/// `apply_sweep` with no `begin_sweep` self-snapshots on the first call: a
/// frontend that skips arming it still rubber-bands correctly instead of
/// accumulating.
#[test]
fn apply_sweep_with_no_begin_snapshots_on_the_first_call() {
    let mut p = pane(&["a", "b", "c", "d"]);
    p.set_mark(3, true);
    p.apply_sweep(0, 2);
    p.apply_sweep(0, 0);
    assert_eq!(p.marks_len(), 2, "'a' (sweep) and 'd' (previous) remain");
}

// --- #313: extension, class, and restore ------------------------------

/// The cursor entry's extension marks its equals, and the twin unmarks
/// them. It is Total Commander's `Alt+Gray+`/`Alt+Gray-`.
#[test]
fn the_cursors_extension_marks_its_equals() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///a.rs", EntryKind::File),
            e("mem:///b.rs", EntryKind::File),
            e("mem:///c.txt", EntryKind::File),
        ],
    );
    assert_eq!(p.mark_same_extension(true), 2, "the two `.rs` ones");
    assert_eq!(p.marks_len(), 2);
    assert_eq!(
        p.mark_same_extension(false),
        2,
        "and the twin releases them"
    );
    assert_eq!(p.marks_len(), 0);
}

/// A hidden file has NO extension, it has a name: `.bashrc` does not mark
/// every `bashrc` in the world, nor the other hidden ones. Same rule as
/// template renaming, and on purpose — two definitions of "the extension"
/// would mark one set and rename another.
#[test]
fn a_name_starting_with_a_dot_has_no_extension() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///.bashrc", EntryKind::File),
            e("mem:///.vimrc", EntryKind::File),
        ],
    );
    assert_eq!(p.mark_same_extension(true), 0);
    assert_eq!(p.marks_len(), 0);
}

/// Names are BYTES: two that would collapse to the same replacement
/// character when passed through `String` still have different
/// extensions.
#[test]
fn the_extension_is_compared_in_bytes() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///a.%FF", EntryKind::File),
            e("mem:///b.%FE", EntryKind::File),
            e("mem:///c.%FF", EntryKind::File),
        ],
    );
    assert_eq!(
        p.mark_same_extension(true),
        2,
        "only the two that share the SAME extension bytes"
    );
}

/// Files or directories, and a link counts as a file — that is what any
/// operation of this panel does with it.
#[test]
fn marking_only_files_or_only_folders() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///a.txt", EntryKind::File),
            e("mem:///sub", EntryKind::Dir),
            e("mem:///enlace", EntryKind::Symlink),
        ],
    );
    assert_eq!(p.mark_kind(false), 2, "the file and the link");
    p.clear_marks();
    assert_eq!(p.mark_kind(true), 1, "only the directory");
}

/// The net for whoever pressed "unmark all" by accident, and for whoever
/// pressed "restore" by accident: it goes and it comes back.
#[test]
fn restoring_returns_the_previous_selection_and_undoes_itself() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///a", EntryKind::File),
            e("mem:///b", EntryKind::File),
        ],
    );
    assert_eq!(p.restore_previous_marks(), None, "no snapshot, nothing");
    p.mark_all();
    p.clear_marks();
    assert_eq!(p.restore_previous_marks(), Some(2));
    assert_eq!(
        p.restore_previous_marks(),
        Some(0),
        "and it undoes itself again"
    );
}

/// An entry that is no longer there does not come back, and a `cd` drops
/// the snapshot: its paths belong to a different directory.
#[test]
fn the_snapshot_does_not_survive_a_cd_nor_resurrect_what_was_deleted() {
    let dir = VPath::parse("mem:///d").unwrap();
    let mut p = PaneState::new(
        dir.clone(),
        vec![
            e("mem:///d/a", EntryKind::File),
            e("mem:///d/b", EntryKind::File),
        ],
    );
    p.mark_all();
    p.clear_marks();
    // `b` disappears from the listing without changing directory.
    p.refill(vec![e("mem:///d/a", EntryKind::File)]);
    assert_eq!(
        p.restore_previous_marks(),
        Some(1),
        "only what is still there comes back"
    );

    p.mark_all();
    p.clear_marks();
    p.set_listing(
        VPath::parse("mem:///otro").unwrap(),
        vec![e("mem:///otro/a", EntryKind::File)],
    );
    assert_eq!(
        p.restore_previous_marks(),
        None,
        "a cd drops the snapshot: its paths name nothing here"
    );
}

// --- #103: mark/unmark by glob ---------------------------------------

#[test]
fn mark_glob_marks_the_matching_names() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///a.rs", EntryKind::File),
            e("mem:///b.rs", EntryKind::File),
            e("mem:///c.txt", EntryKind::File),
        ],
    );
    assert_eq!(p.mark_glob("*.rs", true).unwrap(), 2);
    assert_eq!(
        p.marked_paths(),
        vec![
            VPath::parse("mem:///a.rs").unwrap(),
            VPath::parse("mem:///b.rs").unwrap()
        ]
    );
}

/// review: the direction that actually needs a fold is an UPPERCASE
/// pattern against a lowercase name — the reverse always worked because
/// the fold already lowercases the haystack regardless of `globset`'s
/// own `case_insensitive` knob. Also pins the non-ASCII case: `globset`'s
/// knob is ASCII-only (`(?-u)` byte mode), so `É*` only reaches
/// `étude.txt` because `mark_glob` folds the PATTERN too (BLOCKER C).
#[test]
fn mark_glob_is_case_insensitive() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![e("mem:///photo.jpg", EntryKind::File)],
    );
    assert_eq!(
        p.mark_glob("*.JPG", true).unwrap(),
        1,
        "uppercase ASCII pattern, lowercase name"
    );

    let mut p2 = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![e("mem:///étude.txt", EntryKind::File)],
    );
    assert_eq!(
        p2.mark_glob("É*", true).unwrap(),
        1,
        "uppercase non-ASCII pattern, lowercase name — globset's own \
             case_insensitive can't do this, only the pattern fold can"
    );
}

#[test]
fn mark_glob_with_mark_false_unmarks_only_the_matches() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///a.rs", EntryKind::File),
            e("mem:///c.txt", EntryKind::File),
        ],
    );
    p.mark_all();
    assert_eq!(p.mark_glob("*.rs", false).unwrap(), 1);
    // An empty mark set makes `marked_paths` fall back to the cursor
    // entry (see its rustdoc) — a one-element vector could then be
    // satisfied by ZERO marks. Pin the count first.
    assert_eq!(p.marks_len(), 1);
    assert_eq!(
        p.marked_paths(),
        vec![VPath::parse("mem:///c.txt").unwrap()]
    );
}

#[test]
fn mark_glob_counts_only_the_marks_it_changed() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///a.rs", EntryKind::File),
            e("mem:///b.rs", EntryKind::File),
        ],
    );
    p.mark_glob("a.rs", true).unwrap();
    assert_eq!(
        p.mark_glob("*.rs", true).unwrap(),
        1,
        "a.rs was already marked"
    );
}

#[test]
fn an_invalid_glob_errors_and_marks_nothing() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![e("mem:///a.rs", EntryKind::File)],
    );
    assert!(p.mark_glob("[", true).is_err());
    assert_eq!(p.marks_len(), 0);
}

#[test]
fn mark_glob_under_a_filter_only_reaches_the_visible() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///alfa.rs", EntryKind::File),
            e("mem:///beta.rs", EntryKind::File),
        ],
    );
    p.quick_start(crate::nav::Mode::Filter);
    p.quick_char('a');
    p.quick_char('l');
    assert_eq!(p.mark_glob("*.rs", true).unwrap(), 1);
    assert_eq!(p.marks_len(), 1);
    assert!(p.is_marked(&p.entries()[0].clone()));
}

/// Hostile corpus (hard rule 1): a pattern addresses the FOLDED text
/// (lossy → NFC → lowercase → NFC), never the raw bytes — the invalid
/// bytes of a non-UTF-8 name fold to U+FFFD and cannot be named
/// INDIVIDUALLY, though typing U+FFFD names ALL of them at once (a name
/// whose valid suffix satisfies the rest of the pattern still matches).
/// `marked_paths` gives the ORIGINAL bytes back regardless.
#[test]
fn mark_glob_matches_the_lossy_form_and_returns_raw_bytes() {
    // mem:///<0xFF><0xFE>.rs — same hostile construction as the other
    // hostile tests in this file (see
    // `marca_identidad_por_bytes_del_path_nombre_hostil`).
    let hostile = VPath::parse("mem:///")
        .unwrap()
        .join(norte_proto::Segment::new(vec![0xFF, 0xFE, b'.', b'r', b's']).unwrap());
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![Entry {
            attrs: std::collections::BTreeMap::new(),
            path: hostile.clone(),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
        }],
    );
    assert_eq!(p.mark_glob("*.rs", true).unwrap(), 1);
    assert_eq!(
        p.marked_paths(),
        vec![hostile],
        "raw bytes, never the lossy form"
    );

    // The invalid bytes themselves are unaddressable INDIVIDUALLY: they
    // fold to U+FFFD, and typing U+FFFD names all of them at once.
    p.clear_marks();
    assert_eq!(p.mark_glob("\u{FFFD}*", true).unwrap(), 1);
}

/// Symmetry pin (review requirement, the one that guards the fold
/// forever): for EVERY name in the canonical hostile corpus, a pattern
/// built from that name's own RAW (unfolded) text — escaped only for
/// the glob metacharacters it happens to contain — must still mark
/// exactly that entry, and `marked_paths` must return its exact bytes.
///
/// This is what actually exercises `mark_glob` folding the PATTERN
/// (BLOCKER C): before that fix, `GlobBuilder` compiled the pattern
/// AS-IS while the haystack was already folded (NFC + lowercase) — an
/// NFD name's raw (decomposed) text then never matched its own
/// (composed) haystack. Fails today for `nfd_e_acute`,
/// `nfd_uppercase_composed_only_lowercase`, and `name_max_nfd_overflow`.
#[test]
fn mark_glob_pattern_from_each_names_own_text_marks_exactly_that_entry() {
    fn escape_glob(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            if matches!(c, '*' | '?' | '[' | ']' | '{' | '}' | '\\') {
                out.push('\\');
            }
            out.push(c);
        }
        out
    }

    for name in norte_testkit::corpus::hostile_names() {
        let dir = VPath::parse("mem:///").unwrap();
        let seg = norte_proto::Segment::new(name.bytes.clone()).unwrap();
        let path = dir.clone().join(seg);
        let mut p = PaneState::new(
            dir,
            vec![Entry {
                attrs: std::collections::BTreeMap::new(),
                path: path.clone(),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            }],
        );
        let raw = String::from_utf8_lossy(&name.bytes).into_owned();
        let pattern = escape_glob(&raw);
        let changed = p
            .mark_glob(&pattern, true)
            .unwrap_or_else(|err| panic!("[{}] pattern {pattern:?} must compile: {err}", name.id));
        assert_eq!(
            changed, 1,
            "[{}] a pattern from its own text must mark exactly this \
                 entry (pattern {pattern:?})",
            name.id
        );
        assert_eq!(p.marks_len(), 1, "[{}] exactly one mark", name.id);
        assert_eq!(
            p.marked_paths(),
            vec![path],
            "[{}] raw bytes back, never the lossy/folded form",
            name.id
        );
    }
}

/// A pattern typed exactly as an NFD name is painted (macOS trap,
/// CLAUDE.md) matches its NFD twin only because `mark_glob` folds the
/// pattern to NFC before compiling (BLOCKER C). Also pins the
/// name-reinterpretation branch (#57): `П*` reaches `cp866_papka` only
/// under an active IBM866 reinterpretation — a mutant that folds the
/// haystack with `fold_with(name, None)` instead of
/// `fold_with(name, self.name_encoding)` would make this fail, since
/// the raw bytes aren't valid UTF-8 and their plain lossy fold is
/// unrelated Unicode replacement text, not Cyrillic.
#[test]
fn mark_glob_matches_nfd_typed_pattern_and_reinterpreted_uppercase() {
    let nfd_name = "an\u{0303}o.txt";
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![e(&format!("mem:///{nfd_name}"), EntryKind::File)],
    );
    assert_eq!(
        p.mark_glob(nfd_name, true).unwrap(),
        1,
        "NFD-typed pattern must find its NFD twin"
    );

    let papka = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "cp866_papka")
        .expect("fixture del corpus")
        .bytes;
    let dir = VPath::parse("mem:///").unwrap();
    let seg = norte_proto::Segment::new(papka).unwrap();
    let mut p2 = PaneState::new(
        dir.clone(),
        vec![Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(seg),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
        }],
    );
    assert_eq!(p2.cycle_name_encoding(), Some("IBM866"));
    assert_eq!(
        p2.mark_glob("П*", true).unwrap(),
        1,
        "uppercase Cyrillic pattern must reach the reinterpreted name"
    );
}

/// Collapse pin (review requirement): `lossy_collapse_ff`/
/// `lossy_collapse_fe` are two DISTINCT byte sequences (`\xFF.rs` vs
/// `\xFE.rs`) whose lossy fold collapses to the SAME `"\u{FFFD}.rs"` —
/// without this pair the collapse can't be pinned at all: with a single
/// hostile entry, "matches this one" and "matches every invalid name"
/// are indistinguishable.
#[test]
fn mark_glob_collapse_pin() {
    let names = norte_testkit::corpus::hostile_names();
    let ff = names
        .iter()
        .find(|n| n.id == "lossy_collapse_ff")
        .expect("fixture del corpus")
        .bytes
        .clone();
    let fe = names
        .iter()
        .find(|n| n.id == "lossy_collapse_fe")
        .expect("fixture del corpus")
        .bytes
        .clone();
    let dir = VPath::parse("mem:///").unwrap();
    let path_ff = dir.clone().join(norte_proto::Segment::new(ff).unwrap());
    let path_fe = dir.clone().join(norte_proto::Segment::new(fe).unwrap());
    let clean = dir
        .clone()
        .join(norte_proto::Segment::new(b"clean.rs".to_vec()).unwrap());
    let mut p = PaneState::new(
        dir,
        vec![
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: path_ff.clone(),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            },
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: path_fe.clone(),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            },
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: clean,
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            },
        ],
    );
    // ONE pattern (U+FFFD, unaddressable individually) reaches BOTH
    // distinct byte sequences, never the clean name.
    assert_eq!(p.mark_glob("\u{FFFD}*", true).unwrap(), 2);
    assert_eq!(p.marks_len(), 2);
    let mut marked = p.marked_paths();
    marked.sort();
    let mut expected = vec![path_ff, path_fe];
    expected.sort();
    assert_eq!(marked, expected, "both original byte sequences, untouched");

    // Separately: a many-to-one selector over byte-exact identities —
    // the counterpart of `marcas_distinguen_gemelos_nfc_y_nfd_sin_plegar`
    // (which pins that a TOGGLE never folds gemelos). Here, deliberately,
    // ONE NFC pattern marks BOTH the NFC and NFD é twins: `mark_glob`
    // matches by folded TEXT, an intentional many-to-one selector, while
    // each mark's IDENTITY (its `VPath`) stays byte-exact — this is the
    // opposite property from toggle's byte-exact SELECTION, not a
    // regression of it.
    let nfc = names
        .iter()
        .find(|n| n.id == "nfc_e_acute")
        .expect("fixture del corpus")
        .bytes
        .clone();
    let nfd = names
        .iter()
        .find(|n| n.id == "nfd_e_acute")
        .expect("fixture del corpus")
        .bytes
        .clone();
    let dir2 = VPath::parse("mem:///").unwrap();
    let path_nfc = dir2.clone().join(norte_proto::Segment::new(nfc).unwrap());
    let path_nfd = dir2.clone().join(norte_proto::Segment::new(nfd).unwrap());
    let mut p2 = PaneState::new(
        dir2,
        vec![
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: path_nfc.clone(),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            },
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: path_nfd.clone(),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            },
        ],
    );
    assert_eq!(
        p2.mark_glob("é", true).unwrap(),
        2,
        "a many-to-one selector over byte-exact identities: ONE NFC \
             pattern deliberately marks BOTH the NFC and NFD é twins — the \
             counterpart of toggle's byte-exact selection, not a regression \
             of it"
    );
    assert_eq!(p2.marks_len(), 2);
}

/// Guard for the #110 translation ([`unicode_glob_regex`]): pins the
/// SHAPE of globset's output that the byte-run decoding relies on —
/// the `(?-u)` prefix, and non-ASCII pattern bytes emitted as
/// consecutive `\xNN` escapes (also across a class range's `-`). A
/// globset upgrade that changes either fails HERE, loudly, instead of
/// letting patterns silently stop matching non-ASCII names.
#[test]
fn globset_regex_shape_is_the_one_this_translation_expects() {
    // Same builder config as `mark_glob` (no `case_insensitive` — the
    // fold owns case, see the rustdoc there).
    let build = |p: &str| GlobBuilder::new(p).backslash_escape(true).build().unwrap();
    assert!(build("a").regex().starts_with("(?-u)"));
    let lit = build("a\u{f1}o").regex().to_owned();
    assert!(lit.contains(r"\xc3\xb1"), "ñ as a byte-escape run: {lit}");
    let class = build("[\u{f1}x]").regex().to_owned();
    assert!(class.contains(r"[\xc3\xb1x]"), "class run: {class}");
    let range = build("[\u{f1}-\u{fc}]").regex().to_owned();
    assert!(
        range.contains(r"\xc3\xb1-\xc3\xbc"),
        "range endpoints as runs split by ASCII '-': {range}"
    );

    // And the translation of those shapes, end to end:
    assert_eq!(
        unicode_glob_regex(&build("a\u{f1}o")).unwrap(),
        "^a\u{f1}o$"
    );
    assert_eq!(
        unicode_glob_regex(&build("[\u{f1}-\u{fc}]")).unwrap(),
        "^[\u{f1}-\u{fc}]$"
    );
    // Astral endpoints (4-byte UTF-8 runs): 𝄞..𝄢 stays a CHAR range.
    assert_eq!(
        unicode_glob_regex(&build("[\u{1D11E}-\u{1D122}]")).unwrap(),
        "^[\u{1D11E}-\u{1D122}]$"
    );
}

/// The #110 recompile must PRESERVE globset's `dot_matches_new_line`
/// (audit MAJOR-1): `\n` is a legal name byte on unix (corpus
/// `control_newline`) and `*`/`?` translate to `.`-derived tokens —
/// losing the flag makes `*` silently stop matching those names, the
/// INVERSE of the byte/char bug. `mark_glob("*")` must equal mark-all
/// over the whole hostile corpus.
#[test]
fn mark_glob_star_still_reaches_names_with_newlines() {
    let dir = VPath::parse("mem:///").unwrap();
    let entries: Vec<Entry> = norte_testkit::corpus::hostile_names()
        .into_iter()
        .map(|n| Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir
                .clone()
                .join(norte_proto::Segment::new(n.bytes.clone()).unwrap()),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
        })
        .collect();
    let total = entries.len();
    let mut p = PaneState::new(dir.clone(), entries);
    assert_eq!(
        p.mark_glob("*", true).unwrap(),
        total,
        "'*' IS mark-all — a name the wildcard cannot reach would feed \
             the next bulk op a survivor set the user never chose"
    );
    p.clear_marks();

    // Direct pin on the `?` token crossing `\n`, like globset's does.
    let nl = dir.join(norte_proto::Segment::new(b"a\nb".to_vec()).unwrap());
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![Entry {
            attrs: std::collections::BTreeMap::new(),
            path: nl,
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
        }],
    );
    assert_eq!(p.mark_glob("a?b", true).unwrap(), 1, "? crosses \\n");
}

/// Case decision pin (audit MINOR-2): equality is the FOLD's, and only
/// the fold's. Regex-crate `(?i)` would additionally fold `ſ` (U+017F)
/// to `s` — wider than `nav::fold`'s `to_lowercase`, so a pattern `s.*`
/// would mark a file the quick search filter treats as distinct. The
/// glob therefore compiles WITHOUT `case_insensitive`; flipping it back
/// on fails here.
#[test]
fn mark_glob_case_equality_is_the_folds_not_the_regex_crates() {
    let dir = VPath::parse("mem:///").unwrap();
    let long_s = dir
        .clone()
        .join(norte_proto::Segment::new("\u{17f}.txt".as_bytes().to_vec()).unwrap());
    let mut p = PaneState::new(
        dir,
        vec![Entry {
            attrs: std::collections::BTreeMap::new(),
            path: long_s,
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
        }],
    );
    assert_eq!(
        p.mark_glob("s.txt", true).unwrap(),
        0,
        "ſ folds to itself; only regex-crate case folding equates it \
             with s, and that is NOT the pane's definition of equality"
    );
    assert_eq!(p.mark_glob("\u{17f}.txt", true).unwrap(), 1);
}

/// Escaped-backslash adjacency (audit hole 4): in `a\\xc3o` the `\\`
/// pair is one token, so `xc3` is LITERAL text — the translation must
/// not re-scan it as a byte escape. Kills any future
/// scan-and-replace-`\xNN` rewrite of the tokenizer.
#[test]
fn mark_glob_escaped_backslash_before_hex_text_stays_literal() {
    let dir = VPath::parse("mem:///").unwrap();
    let lit = dir
        .clone()
        .join(norte_proto::Segment::new(b"a\\xc3o".to_vec()).unwrap());
    let anio = dir
        .clone()
        .join(norte_proto::Segment::new("a\u{c3}o".as_bytes().to_vec()).unwrap());
    let mk = |path: &VPath| Entry {
        attrs: std::collections::BTreeMap::new(),
        path: path.clone(),
        kind: EntryKind::File,
        size: None,
        mtime_ms: None,
    };
    let mut p = PaneState::new(dir, vec![mk(&lit), mk(&anio)]);
    assert_eq!(
        p.mark_glob("a\\\\xc3o", true).unwrap(),
        1,
        "the pattern names the literal-backslash file, nothing else"
    );
    assert_eq!(p.marked_paths(), vec![lit]);
}

/// Character-granularity (#110, fixed): `?` consumes one CHARACTER and
/// a class matches char-wise — `a?o.txt` covers `año.txt`, and
/// `a[ñx]o.txt` covers both twins. globset alone compiles `(?-u)` byte
/// mode, where `ñ` is 2 bytes and both patterns silently marked a
/// DIFFERENT file than the one named. Astral chars (4-byte UTF-8) are
/// one character too.
#[test]
fn mark_glob_matches_characters_not_utf8_bytes() {
    let dir = VPath::parse("mem:///").unwrap();
    let anio = dir
        .clone()
        .join(norte_proto::Segment::new("a\u{f1}o.txt".as_bytes().to_vec()).unwrap());
    let axo = dir
        .clone()
        .join(norte_proto::Segment::new(b"axo.txt".to_vec()).unwrap());
    let mut p = PaneState::new(
        dir,
        vec![
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: anio.clone(),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            },
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: axo.clone(),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            },
        ],
    );
    assert_eq!(
        p.mark_glob("a?o.txt", true).unwrap(),
        2,
        "one char-wildcard covers año.txt AND axo.txt"
    );
    assert_eq!(p.marks_len(), 2);
    p.clear_marks();

    assert_eq!(
        p.mark_glob("a??o.txt", true).unwrap(),
        0,
        "two wildcards are two CHARACTERS — neither 3-char name matches"
    );
    p.clear_marks();

    assert_eq!(
        p.mark_glob("a[\u{f1}x]o.txt", true).unwrap(),
        2,
        "a character class reaches a multi-byte char"
    );
    assert_eq!(p.marks_len(), 2);
    p.clear_marks();

    // A class RANGE spanning non-ASCII endpoints is char-wise too.
    assert_eq!(
        p.mark_glob("a[\u{f0}-\u{f2}]o.txt", true).unwrap(),
        1,
        "ñ (U+00F1) sits inside the U+00F0..U+00F2 range"
    );
    assert_eq!(p.marked_paths(), vec![anio]);
    p.clear_marks();

    // A NEGATED class over chars: `axo` has no ñ, `año` does.
    assert_eq!(
        p.mark_glob("a[!\u{f1}]o.txt", true).unwrap(),
        1,
        "negated char class excludes año.txt only"
    );
    assert_eq!(p.marked_paths(), vec![axo]);

    // Astral: 𝄞 is FOUR UTF-8 bytes and exactly ONE `?`.
    let dir = VPath::parse("mem:///").unwrap();
    let clef = dir
        .clone()
        .join(norte_proto::Segment::new("\u{1D11E}.txt".as_bytes().to_vec()).unwrap());
    let mut p = PaneState::new(
        dir,
        vec![Entry {
            attrs: std::collections::BTreeMap::new(),
            path: clef,
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
        }],
    );
    assert_eq!(p.mark_glob("?.txt", true).unwrap(), 1, "𝄞 = ONE char");
}

/// Masking-divergence pin: rows are painted through
/// [`crate::display_name_with`], which MASKS bidi overrides to U+FFFD;
/// `mark_glob`'s fold does NOT mask them. Typing exactly what the pane
/// PAINTED therefore does not name what the fold preserves.
#[test]
fn mark_glob_pattern_diverges_from_the_painted_text_for_bidi_hazards() {
    let rtl = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "rtl_override")
        .expect("fixture del corpus")
        .bytes;
    let (painted, hostile) = crate::display_name_with(&rtl, None);
    assert!(hostile, "rtl_override is flagged hostile");
    assert!(
        painted.contains('\u{FFFD}'),
        "display_name_with masks the RLO override to U+FFFD: {painted:?}"
    );

    let dir = VPath::parse("mem:///").unwrap();
    let seg = norte_proto::Segment::new(rtl.clone()).unwrap();
    let mut p = PaneState::new(
        dir.clone(),
        vec![Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(seg),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
        }],
    );
    assert_eq!(
        p.mark_glob(&painted, true).unwrap(),
        0,
        "the painted (U+FFFD-masked) text is not what the fold matches \
             against — the fold preserves the raw RLO, unmasked"
    );
}

/// Backslash pin: `backslash_escape(true)` (BLOCKER C) makes `\`
/// consistently an escape character regardless of host OS — `globset`'s
/// own default depends on `is_separator('\\')` (true on unix, false on
/// windows; it also rewrites `\` to `/` in the haystack there), so the
/// SAME pattern would otherwise answer differently per platform. `\` is
/// a legal Linux filename byte (corpus `win_backslash`).
#[test]
fn mark_glob_backslash_matches_only_when_escaped() {
    let name = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "win_backslash")
        .expect("fixture del corpus")
        .bytes; // a\b: 3 bytes, a literal backslash in the middle.
    let dir = VPath::parse("mem:///").unwrap();
    let seg = norte_proto::Segment::new(name).unwrap();
    let mut p = PaneState::new(
        dir.clone(),
        vec![Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(seg),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
        }],
    );
    // An escaped backslash (`\\` in the pattern) matches the literal byte.
    assert_eq!(
        p.mark_glob("a\\\\b", true).unwrap(),
        1,
        "escaped backslash matches the literal byte"
    );
    p.clear_marks();
    // A lone backslash is the escape character itself: `\b` escapes `b`
    // to a literal `b`, so the compiled pattern means "ab", not "a\b".
    assert_eq!(
        p.mark_glob("a\\b", true).unwrap(),
        0,
        "unescaped backslash is consumed as an escape, not a literal match"
    );
}

/// #117-follow-up: `plugin:` column values live in a side-map of the pane
/// (a mirror of `decorations` — keyed by the CURRENT listing's `VPath`):
/// `plugin_cell` serves them defensively RE-masked (P1 doctrine: consumers
/// do not trust the ingest), and a new listing invalidates them just like
/// the decorations.
#[test]
fn the_plugin_columns_side_map_re_masks_and_clears_itself() {
    let mut p = pane(&["a", "b"]);
    let path = VPath::parse("mem:///a").unwrap();
    let mut per_path = std::collections::HashMap::new();
    // A value with a raw RLO: the render never paints it without U+FFFD.
    per_path.insert(path.clone(), "main\u{202E}evil".to_owned());
    let mut cols = std::collections::HashMap::new();
    cols.insert("plugin:git/branch".to_owned(), per_path);
    p.set_plugin_columns(cols);
    let cell = p
        .plugin_cell("plugin:git/branch", &path)
        .expect("value present");
    assert!(
        !cell.contains('\u{202E}'),
        "raw hazard in the cell: {cell:?}"
    );
    assert!(cell.contains('\u{FFFD}'), "the hazard is masked: {cell:?}");
    assert!(cell.starts_with("main"));
    // Unknown column or path with no value → None (blank).
    assert_eq!(p.plugin_cell("plugin:git/otro", &path), None);
    assert_eq!(
        p.plugin_cell("plugin:git/branch", &VPath::parse("mem:///b").unwrap()),
        None
    );
    // A new listing invalidates the side-map (keys of ANOTHER listing).
    p.set_listing(VPath::parse("mem:///d").unwrap(), Vec::new());
    assert_eq!(p.plugin_cell("plugin:git/branch", &path), None);
}

/// Audit F4 (#117-follow-up): the side-map's keys are BYTE-exact `VPath`s
/// — two different non-UTF8 names whose lossy display COLLAPSES to the
/// same `�` (corpus `lossy_collapse_ff`/`_fe`) keep separate cells. If
/// someone "simplifies" tomorrow by keying on display, the values would
/// mix between different files and this goes red.
#[test]
fn plugin_columns_pins_by_bytes_not_by_display() {
    let mut p = pane(&[]);
    let ff = VPath::parse("mem:///%FF").unwrap();
    let fe = VPath::parse("mem:///%FE").unwrap();
    let mut per_path = std::collections::HashMap::new();
    per_path.insert(ff.clone(), "uno".to_owned());
    per_path.insert(fe.clone(), "dos".to_owned());
    let mut cols = std::collections::HashMap::new();
    cols.insert("plugin:git/branch".to_owned(), per_path);
    p.set_plugin_columns(cols);
    assert_eq!(
        p.plugin_cell("plugin:git/branch", &ff).as_deref(),
        Some("uno")
    );
    assert_eq!(
        p.plugin_cell("plugin:git/branch", &fe).as_deref(),
        Some("dos")
    );
}

/// ADR 0137: el elemento de estado de un plugin es el valor de su columna
/// para la entrada bajo el CURSOR, enmascarado y acotado; sin valor, no
/// sale; y no se pulsa.
#[test]
fn plugin_items_report_the_column_under_the_cursor() {
    use crate::statusbar::{PLUGIN_ITEM_MAX_CELLS, plugin_items};
    let mut p = pane(&["a", "b", "c"]);
    let mut per_path = std::collections::HashMap::new();
    per_path.insert(VPath::parse("mem:///a").unwrap(), "main".to_owned());
    per_path.insert(
        VPath::parse("mem:///b").unwrap(),
        format!("x\u{202E}{}", "rama".repeat(20)),
    );
    let mut cols = std::collections::HashMap::new();
    cols.insert("plugin:git/branch".to_owned(), per_path);
    p.set_plugin_columns(cols);
    let pairs = [
        ("git".to_owned(), "branch".to_owned()),
        ("git".to_owned(), "nada".to_owned()),
    ];

    p.set_cursor(0);
    let v = plugin_items(&p, &pairs, norte_i18n::Lang::Es);
    assert_eq!(v.len(), 1, "the column with no value does not show: {v:?}");
    assert_eq!(v[0].id, "plugin:git/branch");
    assert_eq!(v[0].text, "main");
    assert_eq!(v[0].command, None, "a columns plugin does not navigate");
    assert!(v[0].tooltip.contains("git") && v[0].tooltip.contains("branch"));

    p.set_cursor(1);
    let long = &plugin_items(&p, &pairs, norte_i18n::Lang::Es)[0].text;
    assert!(!long.contains('\u{202E}'), "masked: {long:?}");
    assert!(long.ends_with('…'), "bounded: {long:?}");
    assert!(unicode_width::UnicodeWidthStr::width(long.as_str()) <= PLUGIN_ITEM_MAX_CELLS);

    p.set_cursor(2);
    assert!(
        plugin_items(&p, &pairs, norte_i18n::Lang::Es).is_empty(),
        "entry with no data"
    );
}

/// ADR 0137: what is requested from the plugin is what is painted PLUS
/// what the bar needs, without repeats.
#[test]
fn the_requested_columns_add_up_the_bars_ones() {
    let st = crate::columns::ColumnsSettings::default();
    let pairs = [
        ("git".to_owned(), "branch".to_owned()),
        ("git".to_owned(), "branch".to_owned()),
    ];
    assert_eq!(
        crate::columns::plugin_requests(&st, &pairs, "file"),
        vec![("git".to_owned(), "branch".to_owned())]
    );
}

#[test]
fn the_mark_ruler_says_which_segments_carry_any() {
    let names: Vec<String> = (0..100).map(|i| format!("f{i:03}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut p = pane(&refs);
    assert!(p.mark_ruler(10).is_empty(), "no marks, no ruler");
    // Rows 5, 7 (same segment) and 99 (the last one).
    for i in [5, 7, 99] {
        p.set_cursor(i);
        p.toggle_mark();
    }
    assert_eq!(p.mark_ruler(10), vec![0, 9]);
    assert_eq!(p.mark_ruler(100), vec![5, 7, 99]);
    assert!(p.mark_ruler(0).is_empty(), "zero segments, nothing");
    // More segments than rows: each row falls into its own, without
    // overshooting.
    assert!(p.mark_ruler(u16::MAX).iter().all(|t| *t < u16::MAX));
}

/// The header's single pass says exactly the same as the three functions
/// it replaces: if they diverge, the window and the TUI count different
/// marks for the same listing.
#[test]
fn the_marks_summary_matches_the_three_separate_passes() {
    let mut entries = Vec::new();
    for i in 0..40u64 {
        let kind = if i % 4 == 0 {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        let mut x = e(&format!("mem:///n{i:02}"), kind);
        x.size = Some(i * 100);
        entries.push(x);
    }
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), entries);
    assert_eq!(p.marks_summary(10), crate::MarksSummary::default());
    for i in [0, 3, 4, 5, 17, 39] {
        p.set_cursor(i);
        p.toggle_mark();
    }
    for segments in [0, 1, 7, 10, 40, 100] {
        let r = p.marks_summary(segments);
        assert_eq!(r.bytes, p.marked_bytes(), "bytes with {segments} segments");
        assert_eq!(r.dirs, p.marked_dirs(), "dirs with {segments} segments");
        assert_eq!(
            r.ruler,
            p.mark_ruler(segments),
            "ruler with {segments} segments"
        );
    }
    assert!(p.marks_summary(10).dirs > 0 && p.marks_summary(10).bytes > 0);
}
