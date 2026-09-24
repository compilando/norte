//! A mirror to the other pane must not strangle the fill of the mirrored
//! pane: they are two different slots and each has its own `Fill` channel.

use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, Pane};
use norte_tui::fill::{Fill, FillMsg, apply_fill_msg};
use norte_tui::jobs::SearchRun;
use norte_tui::navigate::{Cd, apply_cd};
use norte_tui::probes::{DecorateFetch, Probed};

fn vp(w: &str) -> VPath {
    VPath::parse(w).expect("test wire")
}

fn file(dir: &VPath, name: &str) -> Entry {
    Entry {
        attrs: std::collections::BTreeMap::new(),
        path: dir.join(Segment::new(name.as_bytes().to_vec()).unwrap()),
        kind: EntryKind::File,
        size: Some(1),
        mtime_ms: None,
    }
}

/// A fill paginated PER PANE, not a global one: `pane.mirror` sends the
/// OTHER pane somewhere WITHOUT moving the focus, so with a single slot one
/// keypress would be enough to leave the pane the reader is looking at —
/// theirs, the focused one, still paginating a large dir — half-done.
///
/// Dropping its `rx` kills the drainer without `finish_listing`, and
/// `loading` is only turned off by `finish_listing`/`Failed`/a new listing:
/// the pane would be left with a truncated listing under a permanent
/// "loading…".
#[test]
fn a_mirror_to_the_other_pane_does_not_strangle_the_mirrored_panes_fill() {
    let dir = vp("file:///d");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir.clone(), Vec::new()),
    );
    // Pane 0 —the focused one, the one the reader is looking at— is paginating.
    app.panes[0].begin_listing(dir.clone(), vec![file(&dir, "a")], true, None);
    let (tx0, rx0) = tokio::sync::mpsc::channel::<FillMsg>(1);
    let mut fill: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
    fill.insert(norte_tui::panel::SLOT_LEFT, Fill { rx: rx0 });
    let mut df: norte_frontend::layout::BySlot<DecorateFetch> =
        norte_frontend::layout::BySlot::new();
    let mut lp = Probed::new();

    // `pane.mirror`: pane 1 travels, and its listing is also paginated.
    let (_tx1, rx1) = tokio::sync::mpsc::channel::<FillMsg>(1);
    app.panes[1].begin_listing(dir.clone(), Vec::new(), true, None);
    let mut sr: Option<SearchRun> = None;
    apply_cd(
        &app.panes,
        &mut fill,
        &mut df,
        &mut lp,
        &mut sr,
        Cd::Filling {
            pane: 1,
            fill: Fill { rx: rx1 },
        },
    );

    // Pane 0's drainer still has someone to send to: nobody dropped its
    // `rx` from under it.
    tx0.try_send(FillMsg::Batch(vec![file(&dir, "b")]))
        .expect("pane 0's drainer was not abandoned");
    let msg = fill
        .get_mut(norte_tui::panel::SLOT_LEFT)
        .expect("pane 0's fill is still in its slot")
        .rx
        .try_recv()
        .ok();
    apply_fill_msg(&mut app, &mut fill, norte_tui::panel::SLOT_LEFT, msg);

    assert_eq!(
        app.panes[0].entries().len(),
        2,
        "the later batch enters pane 0's listing"
    );
    assert!(
        app.panes[0].loading(),
        "and the \"loading…\" is still alive: nobody finished the listing for it"
    );
    assert!(
        fill.get(norte_tui::panel::SLOT_RIGHT).is_some(),
        "the mirror kept ITS OWN slot"
    );
}
