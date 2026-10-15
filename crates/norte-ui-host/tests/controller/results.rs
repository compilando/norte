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

// ---------------------------------------------------------------------------
// Task 8: what a results tab refuses, how it transfers, refreshes, follows
// mutations and comes back with the session. Two listings side by side
// (`orthodox`): the results tab joins the left one's group, and the right
// one, at `mem:///casa`, is the target.
// ---------------------------------------------------------------------------

/// The held tree, its search answering `hits`, with the hits' folders
/// listed so `stat` finds them.
fn fake_answering(hits: &[&str]) -> Fake {
    let mut f = held_fake();
    f.answer_search_with(files(hits));
    f.put("mem:///casa/a", vec![(b"x.txt".to_vec(), false)]);
    f.put("mem:///casa/b", vec![(b"y.txt".to_vec(), false)]);
    f
}

/// Two listings and a results tab over `fake`'s answer, focused: `(harness,
/// fake, tab)`.
async fn two_panes_with_results(fake: Fake) -> (Harness, Arc<Fake>, u32) {
    let fake = Arc::new(fake);
    let (h, _snap) = Box::pin(host_con_layout(Arc::clone(&fake), "orthodox", (160, 40))).await;
    let mut h = Harness { h };
    let panel = h.open_panel().await;
    h.search(panel, "aguja").await;
    let tab = h.open_listing(panel).await;
    (h, fake, tab)
}

impl Harness {
    /// Runs `command` through the palette, as a reader would; its ACK.
    async fn command(&mut self, command: &str) -> ActionAck {
        let mut sub = self.h.subscribe();
        let ack = execute_via_palette_ack(&self.h, &mut sub, command).await;
        self.settle().await;
        ack
    }

    /// Answers the dialog on top with `choice`.
    async fn answer(&mut self, choice: &str) {
        let id = self.snapshot().await.dialogs.last().expect("a dialog").id;
        self.act(UiAction::Dialog {
            id,
            choice: choice.to_owned(),
            secret: None,
        })
        .await;
        self.settle().await;
    }
}

fn refused_as(ack: &ActionAck, key: &str) -> bool {
    matches!(ack, ActionAck::Unavailable { reason_key } if reason_key == key)
}

/// C6: mkdir, organize and mirror refuse in a results tab with the
/// reason; the folder pane next to it still can.
#[tokio::test(start_paused = true)]
async fn location_gestures_refuse_in_a_results_tab() {
    let (mut h, _fake, _tab) =
        two_panes_with_results(fake_answering(&["mem:///casa/a/x.txt"])).await;
    for command in ["pane.mkdir", "pane.organize", "pane.mirror"] {
        let ack = h.command(command).await;
        assert!(
            refused_as(&ack, "host-results-not-a-location"),
            "{command}: {ack:?}"
        );
    }
    assert!(h.snapshot().await.dialogs.is_empty());
    h.act(UiAction::FocusSlot { slot_id: 2 }).await;
    let ack = h.command("pane.mkdir").await;
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    assert_eq!(h.snapshot().await.dialogs.len(), 1, "its dialog opened");
}

/// Review Focus 2 (C4/C5): copying hits into the root goes to the
/// confirmation; a hit already in the root refuses; a results tab as the
/// destination refuses.
#[tokio::test(start_paused = true)]
async fn transfers_from_and_into_results_tabs() {
    let (mut h, _fake, tab) = two_panes_with_results(fake_answering(&[
        "mem:///casa/notas.txt",
        "mem:///casa/a/x.txt",
    ]))
    .await;
    // Sorted by name: `notas.txt` (in the root, the target's folder) first.
    let ack = h.command("pane.copy").await;
    assert!(refused_as(&ack, "results-already-there"), "{ack:?}");
    h.act(UiAction::MoveCursor {
        slot_id: tab,
        delta: 1,
    })
    .await;
    let ack = h.command("pane.copy").await;
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    assert_eq!(h.snapshot().await.dialogs.len(), 1, "the confirmation");
    h.answer("cancel").await;
    // The folder pane copies TOWARD the results tab: refused.
    h.act(UiAction::FocusSlot { slot_id: 2 }).await;
    let ack = h.command("pane.copy").await;
    assert!(refused_as(&ack, "results-not-a-location"), "{ack:?}");
}

/// Review Focus 3 (C3): two same-named hits copied into one folder go as
/// two jobs, each with `CollisionPolicy::Fail` (the second fails visibly
/// on the first's file; nothing is overwritten); on a destination that
/// folds case, `a/N.txt` + `b/n.txt` still refuse with
/// `host-batch-folds-to-one`.
#[tokio::test(start_paused = true)]
async fn same_named_hits_each_get_their_job() {
    let (mut h, fake, tab) = two_panes_with_results(fake_answering(&[
        "mem:///casa/a/n.txt",
        "mem:///casa/b/n.txt",
    ]))
    .await;
    for row in 0..2 {
        let generation = h.browser(tab).await.generation;
        h.act(UiAction::ToggleMark {
            slot_id: tab,
            key: RowKey(row),
            generation,
        })
        .await;
    }
    let ack = h.command("pane.copy").await;
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    h.answer("confirm").await;
    let jobs = fake.transfers.lock().expect("transfers").clone();
    assert_eq!(jobs.len(), 2, "{jobs:?}");
    for (_, to, mover, policy) in &jobs {
        assert_eq!(to.to_wire(), "mem:///casa/n.txt");
        assert!(!mover);
        assert_eq!(*policy, norte_proto::CollisionPolicy::Fail);
    }

    let mut folding = fake_answering(&["mem:///casa/a/N.txt", "mem:///casa/b/n.txt"]);
    folding.capabilities.insert(
        "mem:///casa".to_owned(),
        norte_proto::Capabilities {
            flags: norte_proto::CapabilityFlags::empty(),
            max_path: None,
        },
    );
    let (mut h, _fake, tab) = two_panes_with_results(folding).await;
    for row in 0..2 {
        let generation = h.browser(tab).await.generation;
        h.act(UiAction::ToggleMark {
            slot_id: tab,
            key: RowKey(row),
            generation,
        })
        .await;
    }
    let ack = h.command("pane.copy").await;
    assert!(refused_as(&ack, "host-batch-folds-to-one"), "{ack:?}");
}

/// C11: a deleted hit leaves the tab once the delete finishes; a hit
/// whose stat still answers stays.
#[tokio::test(start_paused = true)]
async fn deleted_hits_leave_the_tab() {
    let mut f = fake_answering(&["mem:///casa/a/x.txt", "mem:///casa/b/y.txt"]);
    f.delete_for_real = true;
    let (mut h, fake, tab) = two_panes_with_results(f).await;
    let ack = h.command("pane.delete").await;
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    h.answer("confirm").await;
    assert_eq!(fake.deleted.lock().expect("deleted").len(), 1);
    let tx = fake
        .progress
        .lock()
        .expect("progress")
        .clone()
        .expect("the delete's progress");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    h.settle().await;
    h.settle().await;
    let b = h.browser(tab).await;
    assert_eq!(b.total_rows, Some(1));
    assert_eq!(b.rows[0].display_name, "y.txt");
    assert!(b.results_note.contains('1'), "{}", b.results_note);
}

/// Completes the delete the fake saw last.
async fn complete_last_delete(h: &mut Harness, fake: &Fake) {
    let tx = fake
        .progress
        .lock()
        .expect("progress")
        .clone()
        .expect("the delete's progress");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    h.settle().await;
    h.settle().await;
}

/// Review of Task 8 (M1): a hit whose stat still answers when a mutation in
/// its folder ends STAYS expected — its own task may not have reached the
/// window yet — so a later mutation's recheck still takes it out once gone.
#[tokio::test(start_paused = true)]
async fn a_hit_still_there_stays_expected() {
    let mut f = fake_answering(&["mem:///casa/a/x.txt", "mem:///casa/a/z.txt"]);
    f.put(
        "mem:///casa/a",
        vec![(b"x.txt".to_vec(), false), (b"z.txt".to_vec(), false)],
    );
    let (mut h, fake, tab) = two_panes_with_results(f).await;
    // `x.txt` is deleted, but the fake keeps answering its stat: it stays.
    h.command("pane.delete").await;
    h.answer("confirm").await;
    complete_last_delete(&mut h, &fake).await;
    assert_eq!(h.browser(tab).await.total_rows, Some(2));
    // Now both are gone, and `z.txt`'s delete ends: both leave.
    for p in ["mem:///casa/a/x.txt", "mem:///casa/a/z.txt"] {
        fake.disappeared.lock().expect("gone").insert(p.to_owned());
    }
    h.act(UiAction::MoveCursor {
        slot_id: tab,
        delta: 1,
    })
    .await;
    h.command("pane.delete").await;
    h.answer("confirm").await;
    complete_last_delete(&mut h, &fake).await;
    assert_eq!(h.browser(tab).await.total_rows, Some(0));
}

/// C2: pane.refresh re-runs the query into the same tab.
#[tokio::test(start_paused = true)]
async fn refresh_re_runs_into_the_same_tab() {
    let (mut h, fake, tab) = two_panes_with_results(fake_answering(&["mem:///casa/a/x.txt"])).await;
    assert_eq!(fake.searches_launched(), 1);
    fake.answer_search_with(files(&["mem:///casa/b/y.txt"]));
    let ack = h.command("pane.refresh").await;
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    assert_eq!(fake.searches_launched(), 2);
    assert_eq!(h.results_tab().await.map(|t| t.0), Some(tab));
    let b = h.browser(tab).await;
    assert_eq!(
        b.rows
            .iter()
            .map(|r| r.display_name.clone())
            .collect::<Vec<_>>(),
        ["y.txt"]
    );
    assert!(!b.results_note.is_empty(), "still a results tab");
}

/// Review Focus 5 / C7: a session with a results slot behind another
/// tab, whose root is gone: re-run at start-up, failed visibly (the root
/// is stat'ed first: the walk would end "completed, nothing found"), still
/// a results tab, and its root never listed.
#[tokio::test(start_paused = true)]
async fn a_restored_results_slot_re_runs_even_hidden() {
    let (mut h, fake, tab) = two_panes_with_results(fake_answering(&["mem:///casa/a/x.txt"])).await;
    // The folder tab in front: the results tab is behind it.
    h.act(UiAction::SelectTab { slot_id: 1 }).await;
    h.settle().await;
    let mut body = fake
        .written
        .lock()
        .expect("written")
        .clone()
        .expect("a session");
    let slot = body["slots"][tab.to_string()]
        .as_object_mut()
        .expect("the results slot was saved");
    assert!(slot["results"].is_object(), "saved as its query: {slot:?}");
    slot.insert("path".to_owned(), "mem:///gone".into());
    slot.get_mut("results")
        .and_then(serde_json::Value::as_object_mut)
        .expect("results")
        .insert("root".to_owned(), "mem:///gone".into());

    let restored = fake_answering(&[]);
    *restored.session.lock().expect("session") = (
        norte_proto::methods::Session {
            version: 1,
            revision: 7,
            body,
        },
        true,
    );
    let restored = Arc::new(restored);
    let (h2, _snap) = Box::pin(host_con_layout(
        Arc::clone(&restored),
        "orthodox",
        (160, 40),
    ))
    .await;
    let mut h2 = Harness { h: h2 };
    h2.settle().await;
    assert_eq!(restored.searches_launched(), 0, "the root failed its stat");
    h2.act(UiAction::SelectTab { slot_id: tab }).await;
    h2.settle().await;
    let b = h2.browser(tab).await;
    assert!(b.results_note.contains("fallida"), "{}", b.results_note);
    assert!(b.path_segments.is_empty());
    assert!(
        !restored
            .listed_dirs
            .lock()
            .expect("listed")
            .iter()
            .any(|d| d.to_wire() == "mem:///gone"),
        "the results tab's root is never listed"
    );
}
