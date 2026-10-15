use super::*;

// ---------------------------------------------------------------------------
// Results tabs in the window (spec 2026-10-10, phase B): "Open as listing"
// turns the search panel's files into a tab of the listing, with a Folder
// column, fed by the same run while it streams (C9).
//
// On a STOPPED clock (`start_paused`), like the panel's tests.
// ---------------------------------------------------------------------------

use std::time::Duration;

use norte_frontend::search_panel::DEBOUNCE;
use norte_ui_host::action::{DialogFieldValue, TabVerb};
use norte_ui_host::dto::BrowserSlotView;

/// A host and what it is driven with.
struct Harness {
    h: UiHost,
}

impl Harness {
    async fn act(&mut self, action: UiAction) -> ActionAck {
        self.h.dispatch(action).await.expect("host alive")
    }

    /// Opens the search panel by its button in the panel bar; its slot.
    async fn open_panel(&mut self) -> u32 {
        let snap = self.snapshot().await;
        let button = snap
            .panel_bar
            .buttons
            .iter()
            .position(|b| b.kind == "search")
            .expect("search is in the panel bar");
        self.act(UiAction::PanelBarActivate {
            button: u32::try_from(button).expect("fits"),
        })
        .await;
        self.settle().await;
        self.snapshot()
            .await
            .slots
            .iter()
            .find_map(|s| match s {
                SlotView::Search(p) => Some(p.slot_id),
                _ => None,
            })
            .expect("the panel opened")
    }

    async fn settle(&mut self) {
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    async fn snapshot(&mut self) -> norte_ui_host::ViewSnapshot {
        let mut sub = self.h.subscribe();
        self.act(UiAction::Resync).await;
        next_snapshot(&mut sub).await
    }

    /// The listing in slot `id`, now.
    async fn browser(&mut self, id: u32) -> BrowserSlotView {
        self.snapshot()
            .await
            .slots
            .into_iter()
            .find_map(|s| match s {
                SlotView::Browser(b) if b.slot_id == id => Some(*b),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no listing in slot {id}"))
    }

    /// The search panel's view, if it is open.
    async fn panel(&mut self) -> Option<norte_ui_host::dto::SearchPanelSlotView> {
        self.snapshot()
            .await
            .slots
            .into_iter()
            .find_map(|s| match s {
                SlotView::Search(p) => Some(*p),
                _ => None,
            })
    }

    /// The 🔍 tab, if there is one: `(slot, title, hostile)`.
    async fn results_tab(&mut self) -> Option<(u32, String, bool)> {
        self.snapshot()
            .await
            .layout
            .tabs
            .iter()
            .flat_map(|g| g.tabs.clone())
            .find(|t| t.title.contains('🔍'))
            .map(|t| (t.slot_id, t.title, t.title_hostile))
    }

    /// Types `content` in the panel and searches now.
    async fn search(&mut self, panel: u32, content: &str) {
        self.act(UiAction::SearchPanelEdit {
            slot_id: panel,
            field: "content".into(),
            value: DialogFieldValue::Text {
                text: content.into(),
            },
        })
        .await;
        self.act(UiAction::SearchPanelGo { slot_id: panel }).await;
        self.settle().await;
    }

    /// Open as listing; the new tab's slot.
    async fn open_listing(&mut self, panel: u32) -> u32 {
        let ack = self
            .act(UiAction::SearchPanelOpenListing { slot_id: panel })
            .await;
        assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
        self.settle().await;
        self.results_tab().await.expect("a 🔍 tab").0
    }
}

async fn harness_over(fake: Fake) -> (Harness, Arc<Fake>) {
    let fake = Arc::new(fake);
    let (h, _snap) = Box::pin(host_tree(Arc::clone(&fake))).await;
    (Harness { h }, fake)
}

fn file(path: &str) -> norte_proto::Entry {
    norte_proto::Entry {
        path: VPath::parse(path).expect("vpath"),
        kind: norte_proto::EntryKind::File,
        size: Some(1),
        mtime_ms: Some(0),
        attrs: std::collections::BTreeMap::new(),
    }
}

/// One batch with these files, each with one matching line.
fn files(paths: &[&str]) -> norte_proto::methods::SearchHits {
    let m = norte_proto::methods::MatchInfo {
        line: Some(1),
        preview: Some("x aguja".to_owned()),
        offset: Some(0),
        ranges: Some(vec![(2, 7)]),
    };
    norte_proto::methods::SearchHits {
        task_id: norte_proto::TaskId::new(1),
        entries: paths.iter().map(|p| file(p)).collect(),
        matches: Some(vec![m.clone(); paths.len()]),
        all_matches: Some(paths.iter().map(|_| vec![m.clone()]).collect()),
        more: Some(vec![0; paths.len()]),
    }
}

/// The tree, its searches answering `a/x.txt` and `b/y.txt` and kept open
/// until the test closes them.
fn held_fake() -> Fake {
    let mut f = tree_as_fake();
    // The root's parent, for a results tab navigated up.
    f.put("mem:///", vec![(b"casa".to_vec(), true)]);
    f.answer_search_with(files(&["mem:///casa/a/x.txt", "mem:///casa/b/y.txt"]));
    f.hold_search_stream();
    f
}

/// A host with a results tab over `held_fake`'s two hits: `(harness, fake,
/// panel, tab)`.
async fn with_results_tab() -> (Harness, Arc<Fake>, u32, u32) {
    let (mut h, fake) = harness_over(held_fake()).await;
    let panel = h.open_panel().await;
    h.search(panel, "aguja").await;
    let tab = h.open_listing(panel).await;
    (h, fake, panel, tab)
}

/// Open as listing: a new tab next to the active listing, titled with
/// the query and count, the Folder column right after the name, the
/// location line instead of breadcrumbs, the status note.
#[tokio::test(start_paused = true)]
async fn open_as_listing_makes_a_results_tab() {
    let (mut h, _fake, _panel, tab) = with_results_tab().await;
    let (_, title, hostile) = h.results_tab().await.expect("a 🔍 tab");
    assert_eq!(title, "🔍 aguja (2)");
    assert!(!hostile);
    let b = h.browser(tab).await;
    assert_eq!(b.columns[1].id, "folder");
    assert!(b.path_segments.is_empty());
    assert!(b.path_display.contains("aguja"), "{}", b.path_display);
    assert!(!b.results_note.is_empty());
    assert_eq!(
        b.rows
            .iter()
            .map(|r| r.cells[0].text.clone().unwrap_or_default())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
}

/// With nothing shown, Open as listing says so and opens nothing.
#[tokio::test(start_paused = true)]
async fn open_as_listing_with_nothing_shown_refuses() {
    let (mut h, _fake) = harness_over(tree_as_fake()).await;
    let panel = h.open_panel().await;
    let ack = h
        .act(UiAction::SearchPanelOpenListing { slot_id: panel })
        .await;
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "search-panel-nothing-to-list"),
        "{ack:?}"
    );
    assert!(h.results_tab().await.is_none());
}

/// C9: a live run moves to the tab; the panel's next query launches a
/// new run and does NOT cancel the tab's; the tab keeps receiving.
#[tokio::test(start_paused = true)]
async fn the_live_run_moves_to_the_tab() {
    let (mut h, fake, panel, tab) = with_results_tab().await;
    assert_eq!(fake.searches_launched(), 1);
    h.act(UiAction::SearchPanelEdit {
        slot_id: panel,
        field: "content".into(),
        value: DialogFieldValue::Text {
            text: "otra".into(),
        },
    })
    .await;
    tokio::time::advance(DEBOUNCE).await;
    h.settle().await;
    assert_eq!(fake.searches_launched(), 2);
    assert_eq!(fake.cancellations.load(Ordering::SeqCst), 0);
    fake.push_batch(0, files(&["mem:///casa/c/z.txt"])).await;
    h.settle().await;
    let b = h.browser(tab).await;
    assert_eq!(b.total_rows, Some(3));
    let running = b.results_note.clone();
    fake.close_search(0);
    h.settle().await;
    let b = h.browser(tab).await;
    assert_ne!(b.results_note, running, "the note says the run ended");
    assert!(!b.results_note.contains('…'), "{}", b.results_note);
    assert_eq!(
        h.results_tab().await.map(|t| t.1).as_deref(),
        Some("🔍 aguja (3)")
    );
}

/// C9: the panel watches the run it handed over while it still shows that
/// query: a batch grows its tree too, and the end stops its spinner.
#[tokio::test(start_paused = true)]
async fn the_panel_watches_the_handed_over_run() {
    let (mut h, fake, _panel, _tab) = with_results_tab().await;
    let before = h.panel().await.expect("panel").total_rows;
    assert!(h.panel().await.expect("panel").running);
    fake.push_batch(0, files(&["mem:///casa/c/z.txt"])).await;
    h.settle().await;
    assert!(h.panel().await.expect("panel").total_rows > before);
    fake.close_search(0);
    h.settle().await;
    assert!(!h.panel().await.expect("panel").running);
}

/// Review of Task 7: closing the tab that took the run tells the panel
/// still watching it, which would otherwise say "searching" forever.
#[tokio::test(start_paused = true)]
async fn closing_the_tab_stops_the_watching_panel() {
    let (mut h, _fake, _panel, tab) = with_results_tab().await;
    h.act(UiAction::TabAction {
        slot_id: tab,
        verb: TabVerb::Close,
    })
    .await;
    h.settle().await;
    assert!(!h.panel().await.expect("panel").running);
}

/// Review of Task 7: a reopened panel's epochs start over, so it must not
/// pick up batches of the run its predecessor handed to a tab.
#[tokio::test(start_paused = true)]
async fn a_reopened_panel_does_not_watch_an_old_hand_over() {
    let (mut h, fake, _panel, _tab) = with_results_tab().await;
    // The panel bar's button closes the panel, and opens a new one.
    let snap = h.snapshot().await;
    let button = snap
        .panel_bar
        .buttons
        .iter()
        .position(|b| b.kind == "search")
        .expect("search is in the panel bar");
    h.act(UiAction::PanelBarActivate {
        button: u32::try_from(button).expect("fits"),
    })
    .await;
    h.settle().await;
    assert!(h.panel().await.is_none(), "closed");
    let panel = h.open_panel().await;
    h.search(panel, "aguja").await;
    let before = h.panel().await.expect("panel").total_rows;
    fake.push_batch(0, files(&["mem:///casa/c/z.txt"])).await;
    h.settle().await;
    assert_eq!(h.panel().await.expect("panel").total_rows, before);
}

/// Review Focus 1: closing the results tab, or navigating it to a
/// folder, cancels its run; a batch pushed afterwards (stopped by the
/// forwarder, which was let go) does not land in the folder.
#[tokio::test(start_paused = true)]
async fn closing_or_navigating_cancels_the_run() {
    let (mut h, fake, panel, tab) = with_results_tab().await;
    let ack = h
        .act(UiAction::TabAction {
            slot_id: tab,
            verb: TabVerb::Close,
        })
        .await;
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    h.settle().await;
    assert!(h.results_tab().await.is_none());
    assert_eq!(fake.cancellations.load(Ordering::SeqCst), 1);
    // A second run, listed again, then navigated to its parent.
    h.search(panel, "aguja2").await;
    let tab = h.open_listing(panel).await;
    let ack = h.act(UiAction::Parent { slot_id: tab }).await;
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    h.settle().await;
    assert_eq!(fake.cancellations.load(Ordering::SeqCst), 2);
    fake.push_batch(1, files(&["mem:///casa/tarde.txt"])).await;
    h.settle().await;
    let b = h.browser(tab).await;
    assert!(b.results_note.is_empty(), "a folder now");
    assert!(
        b.rows.iter().all(|r| r.display_name != "tarde.txt"),
        "a late batch does not land in the folder"
    );
}

/// A header click on Folder sorts by folder.
#[tokio::test(start_paused = true)]
async fn the_folder_header_sorts() {
    let (mut h, _fake, _panel, tab) = with_results_tab().await;
    h.act(UiAction::SortBy {
        slot_id: tab,
        column: "folder".into(),
    })
    .await;
    h.settle().await;
    assert_eq!(h.browser(tab).await.columns[1].sort.as_deref(), Some("asc"));
}

/// Review Focus 4: a hostile query is masked in the tab title and the
/// location, and flagged.
#[tokio::test(start_paused = true)]
async fn a_hostile_query_is_masked() {
    let (mut h, _fake) = harness_over(held_fake()).await;
    let panel = h.open_panel().await;
    h.search(panel, "a\u{1b}[2Jb").await;
    let tab = h.open_listing(panel).await;
    let (_, title, hostile) = h.results_tab().await.expect("a 🔍 tab");
    assert!(hostile && !title.contains('\u{1b}'), "{title:?}");
    let b = h.browser(tab).await;
    assert!(b.path_hostile && !b.path_display.contains('\u{1b}'));
}
