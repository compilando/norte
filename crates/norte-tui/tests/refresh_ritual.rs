//! The refresh ritual: an Esc halfway through keeps the `Fill` of the pane
//! that was not refreshed, and a refresh under open help re-freezes its
//! facts.

use norte_proto::VPath;
use norte_tui::app::{App, Pane};
use norte_tui::fill::{Fill, FillMsg};
use norte_tui::jobs::SearchRun;
use norte_tui::probes::Probed;
use norte_tui::refresh::after_panes_refresh;

fn fill() -> Fill {
    let (_tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
    Fill { rx }
}

fn app() -> App {
    let d = VPath::parse("file:///d").expect("test wire");
    App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
}

/// #118 (regression requested in the issue): an Esc halfway through the
/// refresh re-listed pane 0 but ABANDONED pane 1 — the ritual can only
/// release the drainer of the pane that was actually re-listed; the other
/// one's keeps draining a listing that is still its own (#78).
#[test]
fn an_esc_halfway_through_keeps_the_fill_of_the_unrefreshed_pane() {
    let mut app = app();
    let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
    f.insert(norte_tui::panel::SLOT_RIGHT, fill());
    let mut lp = Probed::from([(1, VPath::parse("file:///d/x").unwrap())]);
    let mut sr: Option<SearchRun> = None;
    after_panes_refresh(&mut app, [true, false], &mut f, &mut lp, &mut sr);
    assert!(
        f.get(norte_tui::panel::SLOT_RIGHT).is_some(),
        "pane 1's fill (not re-listed) survives the Esc halfway through"
    );
    assert!(
        lp.is_empty(),
        "the #52 probe's dedup still expires either way"
    );
}

/// MAJOR-2: freezing stops a verdict from changing because the reader
/// MOVES, and that is fine. What it cannot stop is it changing because the
/// WORLD changes: the `tick` arm carries no overlay guard (unlike
/// `dir_watch`'s, gated by `watch_refresh_allowed`), so a copy or a delete
/// that finishes with help open re-lists both panes and the entry the facts
/// described may be gone. The row said "does not apply to this selection"
/// about a selection that no longer existed.
#[test]
fn a_refresh_under_open_help_refreezes_the_facts() {
    use norte_help::ChordResolver as _;

    let d = VPath::parse("file:///d").expect("test wire");
    let file = norte_proto::Entry {
        attrs: std::collections::BTreeMap::new(),
        path: d.join(norte_proto::Segment::new(b"readme.txt".to_vec()).expect("segment")),
        kind: norte_proto::EntryKind::File,
        size: Some(3),
        mtime_ms: None,
    };
    let mut app = App::new(Pane::new(d.clone(), vec![file]), Pane::new(d, Vec::new()));
    norte_tui::overlays::open_contextual_help(&mut app, norte_help::Lang::En, &[], None);
    assert!(
        app.help_chords.availability("pane.view").is_available(),
        "with a file under the cursor, F3 can be pressed"
    );

    // The task finishes, the refresh comes in under the overlay and sweeps
    // away the entry the facts were talking about.
    app.panes[0].refresh_listing(Vec::new());
    let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
    let mut lp = Probed::new();
    let mut sr: Option<SearchRun> = None;
    after_panes_refresh(&mut app, [true, false], &mut f, &mut lp, &mut sr);

    assert_eq!(
        app.help_chords.availability("pane.view").reason(),
        Some(norte_help::Reason::WrongTarget),
        "the listing changed: the facts must freeze again"
    );
}

/// #311: a task whose result IS a report must not let the tick's generic
/// `done` clobber its message. Checksums are the case: the verdict
/// ("1 does not match") is set by the report's harvest, and the bar was
/// losing it — seen in tmux, where the footer said `done` over a modal with
/// a MISMATCH inside.
#[test]
fn a_task_that_speaks_through_its_report_does_not_say_done() {
    use norte_proto::TaskKind;
    use norte_tui::refresh::habla_por_su_informe;

    assert!(
        habla_por_su_informe(TaskKind::Checksum),
        "checksums answer with their report, not with their state"
    );
    for other in [
        TaskKind::Copy,
        TaskKind::Move,
        TaskKind::Delete,
        TaskKind::Pack,
    ] {
        assert!(
            !habla_por_su_informe(other),
            "a mutation does finish with a `done`: {other:?}"
        );
    }
}
