use super::*;

// ---------------------------------------------------------------------------
// The search panel (spec 2026-10-09): the window runs the shared model —
// the pause, the runs, one board row — and projects it as `SlotView::Search`.
//
// On a STOPPED clock (`start_paused`): the pause is 300 ms of tokio's time,
// and `advance` skips it instead of waiting for it.
// ---------------------------------------------------------------------------

use std::time::Duration;

use norte_frontend::search_panel::DEBOUNCE;
use norte_ui_host::action::DialogFieldValue;
use norte_ui_host::dto::{SearchPanelRowView, SearchPanelSlotView};

/// A host and what it is driven with.
struct Harness {
    h: UiHost,
}

impl Harness {
    /// One action, answered.
    async fn act(&mut self, action: UiAction) -> ActionAck {
        self.h.dispatch(action).await.expect("host alive")
    }

    /// Opens (or, pressed again, closes) the panel of `command`
    /// (`layout.<kind>`) by its button in the panel bar, the way a click
    /// does; returns the slot of that kind, or 0 if there is none now.
    async fn open_slot(&mut self, command: &str) -> u32 {
        let kind = command.strip_prefix("layout.").expect("a layout command");
        let snap = self.snapshot().await;
        let button = snap
            .panel_bar
            .buttons
            .iter()
            .position(|b| b.kind == kind)
            .unwrap_or_else(|| panic!("`{kind}` is not in the panel bar"));
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
            .unwrap_or(0)
    }

    /// Until the executor has nothing left ready. On the stopped clock a
    /// sleep only ends once every task is waiting on something.
    async fn settle(&mut self) {
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    /// The whole screen, now.
    async fn snapshot(&mut self) -> norte_ui_host::ViewSnapshot {
        let mut sub = self.h.subscribe();
        self.act(UiAction::Resync).await;
        next_snapshot(&mut sub).await
    }

    /// Slot `id`'s view, now.
    async fn slot(&mut self, id: u32) -> SlotView {
        let wanted = id;
        self.snapshot()
            .await
            .slots
            .into_iter()
            .find(|s| matches!(s, SlotView::Search(p) if p.slot_id == wanted))
            .unwrap_or_else(|| panic!("no search slot {id}"))
    }

    /// The search panel's view, now.
    async fn panel(&mut self, id: u32) -> SearchPanelSlotView {
        let SlotView::Search(s) = self.slot(id).await else {
            panic!("search slot")
        };
        *s
    }
}

/// The double the panel tests run on: the usual tree, and searches that
/// walk until they are cancelled.
fn panel_fake() -> Fake {
    let mut f = tree_as_fake();
    f.search_runs_until_cancelled = true;
    f
}

async fn harness_over(fake: Fake) -> (Harness, Arc<Fake>) {
    let fake = Arc::new(fake);
    // Boxed: starting a host is a large future, and every test awaits this.
    let (h, _snap) = Box::pin(host_tree(Arc::clone(&fake))).await;
    (Harness { h }, fake)
}

async fn harness_with_fake() -> (Harness, Arc<Fake>) {
    harness_over(panel_fake()).await
}

/// A batch with one file and one matching line of it.
fn one_file_one_line(
    path: &str,
    line: u64,
    offset: Option<u64>,
    preview: &str,
    range: (u32, u32),
) -> norte_proto::methods::SearchHits {
    let m = norte_proto::methods::MatchInfo {
        line: Some(line),
        preview: Some(preview.to_owned()),
        offset,
        ranges: Some(vec![range]),
    };
    norte_proto::methods::SearchHits {
        task_id: norte_proto::TaskId::new(1),
        entries: vec![norte_proto::Entry {
            path: VPath::parse(path).expect("vpath"),
            kind: norte_proto::EntryKind::File,
            size: Some(1),
            mtime_ms: Some(0),
            attrs: std::collections::BTreeMap::new(),
        }],
        matches: Some(vec![m.clone()]),
        all_matches: Some(vec![vec![m]]),
        more: Some(vec![0]),
    }
}

fn text(t: &str) -> DialogFieldValue {
    DialogFieldValue::Text { text: t.into() }
}

fn search_rows(snap: &norte_ui_host::ViewSnapshot) -> usize {
    snap.tasks.iter().filter(|t| t.kind == "search").count()
}

/// Opening the panel gives it the keyboard, as in the terminal: it opens
/// to be typed into, and the renderer focuses its input when it is the
/// active slot.
#[tokio::test(start_paused = true)]
async fn opening_the_panel_makes_it_the_active_slot() {
    let (mut h, _fake) = harness_with_fake().await;
    let slot = h.open_slot("layout.search").await;
    let snap = h.snapshot().await;
    let role = snap
        .layout
        .placements
        .iter()
        .find(|p| p.slot_id == slot)
        .and_then(|p| p.role);
    assert_eq!(role, Some(norte_ui_host::dto::SlotRole::Active));
}

/// Review Focus 2: edits within the pause launch ONE search; a later edit
/// cancels it, and the board keeps one row for the panel.
#[tokio::test(start_paused = true)]
async fn typing_launches_once_and_relaunching_keeps_one_board_row() {
    let (mut h, fake) = harness_with_fake().await;
    let slot = h.open_slot("layout.search").await;
    for t in ["a", "ag", "agu"] {
        h.act(UiAction::SearchPanelEdit {
            slot_id: slot,
            field: "content".into(),
            value: text(t),
        })
        .await;
        tokio::time::advance(Duration::from_millis(100)).await;
    }
    tokio::time::advance(DEBOUNCE).await;
    h.settle().await;
    assert_eq!(fake.searches_launched(), 1);
    h.act(UiAction::SearchPanelEdit {
        slot_id: slot,
        field: "content".into(),
        value: text("aguj"),
    })
    .await;
    tokio::time::advance(DEBOUNCE).await;
    h.settle().await;
    assert_eq!(
        (
            fake.searches_launched(),
            fake.cancellations.load(Ordering::SeqCst)
        ),
        (2, 1)
    );
    assert_eq!(search_rows(&h.snapshot().await), 1);
}

/// A batch becomes rows: folder, file, the line with its ranges.
#[tokio::test(start_paused = true)]
async fn hits_become_rows_with_ranges() {
    let (mut h, fake) = harness_with_fake().await;
    let slot = h.open_slot("layout.search").await;
    fake.answer_search_with(one_file_one_line(
        "mem:///casa/a.txt",
        7,
        Some(30),
        "x aguja",
        (2, 7),
    ));
    h.act(UiAction::SearchPanelEdit {
        slot_id: slot,
        field: "content".into(),
        value: text("aguja"),
    })
    .await;
    h.act(UiAction::SearchPanelGo { slot_id: slot }).await;
    h.settle().await;
    let s = h.panel(slot).await;
    assert!(
        matches!(&s.rows[..], [
            SearchPanelRowView::Folder { .. },
            SearchPanelRowView::File { .. },
            SearchPanelRowView::Line { line: 7, ranges, .. },
        ] if ranges == &vec![(2, 7)]),
        "{:?}",
        s.rows
    );
    assert_eq!((s.first_row, s.total_rows, s.cursor), (0, 3, Some(0)));
    assert!(s.running, "the walk goes on until it is cancelled");
}

/// A fake whose `mem:///casa/a.txt` is `lines` lines, `x aguja` on line
/// `at`; returns it and the byte offset of that line.
fn fake_with_file(lines: usize, at: usize) -> (Fake, u64) {
    let mut f = panel_fake();
    let mut body = Vec::new();
    let mut offset = 0;
    for i in 1..=lines {
        if i == at {
            offset = body.len() as u64;
            body.extend_from_slice(b"x aguja\n");
        } else {
            body.extend_from_slice(format!("line {i:08} of the file\n").as_bytes());
        }
    }
    f.content.insert("mem:///casa/a.txt".to_owned(), body);
    // A previewer that WOULD match: an aimed open must not ask it (the lit
    // line is the file's own bytes, and a styled view has no such line).
    f.previews.insert(
        "mem:///casa/a.txt".to_owned(),
        super::viewer::preview_de("Texto", &["x aguja"], false),
    );
    (f, offset)
}

/// The viewer opened at a line, lit, and no styled preview was requested.
async fn assert_aimed_open_asks_no_previewer(h: &mut Harness, fake: &Fake) {
    // A few rounds so a late request, had one been made, would have landed.
    for _ in 0..3 {
        h.settle().await;
    }
    let v = h.snapshot().await.viewer.expect("the viewer opened");
    assert!(v.highlight.is_some(), "the line is lit");
    assert!(
        fake.preview_widths.lock().expect("mutex").is_empty(),
        "an aimed open does not request a styled preview"
    );
}

/// Searches `aguja` with `hits` answering, then activates row 2 (the line).
async fn activate_the_line(h: &mut Harness, fake: &Fake, slot: u32, line: u64, offset: u64) {
    fake.answer_search_with(one_file_one_line(
        "mem:///casa/a.txt",
        line,
        Some(offset),
        "x aguja",
        (2, 7),
    ));
    h.act(UiAction::SearchPanelEdit {
        slot_id: slot,
        field: "content".into(),
        value: text("aguja"),
    })
    .await;
    h.act(UiAction::SearchPanelGo { slot_id: slot }).await;
    h.settle().await;
    activate_row(h, slot, 2).await;
}

/// Activates row `row` as a click on the panel's current paint does.
async fn activate_row(h: &mut Harness, slot: u32, row: u64) {
    let generation = h.panel(slot).await.generation;
    let ack = h
        .act(UiAction::SearchPanelActivate {
            slot_id: slot,
            row,
            generation,
        })
        .await;
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    h.settle().await;
}

/// Clicking a line opens the viewer at it, highlighted.
#[tokio::test(start_paused = true)]
async fn activating_a_line_opens_the_viewer_at_it() {
    let (f, offset) = fake_with_file(200, 7);
    let (mut h, fake) = harness_over(f).await;
    let slot = h.open_slot("layout.search").await;
    activate_the_line(&mut h, &fake, slot, 7, offset).await;
    let v = h.snapshot().await.viewer.expect("the viewer opened");
    let lit = v.highlight.expect("the line is lit");
    assert_eq!((lit.row, v.first_line), (6, 6));
    assert_eq!(lit.ranges, vec![(2, 7)]);
    assert_eq!(v.lines_from, 1, "inside the cap: the file from its head");
    assert_aimed_open_asks_no_previewer(&mut h, &fake).await;
}

/// A line past the viewer's cap: the read starts `CONTEXT_BEFORE` bytes
/// before the line, and the viewer says which line its window starts at.
#[tokio::test(start_paused = true)]
async fn a_line_past_the_cap_reads_a_window_around_it() {
    // 25-byte lines: 20 000 of them is ~488 KiB, the match on line 15 000
    // starts ~366 KiB in, past the 256 KiB cap.
    let (f, offset) = fake_with_file(20_000, 15_000);
    assert!(offset > 256 * 1024);
    let (mut h, fake) = harness_over(f).await;
    let slot = h.open_slot("layout.search").await;
    activate_the_line(&mut h, &fake, slot, 15_000, offset).await;
    let reads = fake.reads.lock().expect("reads mutex").clone();
    let (_, range) = reads
        .iter()
        .rev()
        .find(|(p, _)| p.to_wire() == "mem:///casa/a.txt")
        .expect("the file was read");
    assert_eq!(
        range.map(|r| r.offset),
        Some(offset - norte_frontend::viewer::CONTEXT_BEFORE)
    );
    let v = h.snapshot().await.viewer.expect("the viewer opened");
    assert!(v.lines_from > 1, "a window, not the head: {}", v.lines_from);
    let lit = v.highlight.expect("the line is lit");
    assert_eq!(lit.ranges, vec![(2, 7)]);
    assert_aimed_open_asks_no_previewer(&mut h, &fake).await;
}

/// A hit with no byte offset (UTF-16, some legacy encodings) on a line the
/// head does not hold: the viewer opens at the top and SAYS so, as the
/// terminal does — not near the end of the read, which looks like an answer.
#[tokio::test(start_paused = true)]
async fn a_line_not_held_without_an_offset_opens_at_the_top_and_says_so() {
    let (f, _) = fake_with_file(200, 7);
    let (mut h, fake) = harness_over(f).await;
    let slot = h.open_slot("layout.search").await;
    search_answered(
        &mut h,
        &fake,
        slot,
        one_file_one_line("mem:///casa/a.txt", 90_000, None, "x aguja", (2, 7)),
    )
    .await;
    activate_row(&mut h, slot, 2).await;
    h.settle().await;
    let snap = h.snapshot().await;
    let v = snap.viewer.expect("the viewer opened");
    assert_eq!((v.first_line, v.highlight), (0, None));
    assert_eq!(
        snap.status.message.as_deref(),
        Some(norte_i18n::t_in(norte_i18n::Lang::Es, "viewer-target-past-cap").as_str())
    );
}

/// Activating a FILE row takes the listing to its folder with the cursor
/// on it, byte for byte, and the keys back to the listing.
#[tokio::test(start_paused = true)]
async fn activating_a_file_goes_to_it_in_the_listing() {
    let (mut h, fake) = harness_with_fake().await;
    let slot = h.open_slot("layout.search").await;
    search_answered(
        &mut h,
        &fake,
        slot,
        one_file_one_line("mem:///casa/docs/b.md", 1, Some(0), "aguja", (0, 5)),
    )
    .await;
    // Folder `docs`, then its file.
    activate_row(&mut h, slot, 1).await;
    for _ in 0..3 {
        h.settle().await;
    }
    let snap = h.snapshot().await;
    // The panel docks on the left: the listing is not the first slot.
    let l = snap
        .slots
        .iter()
        .find_map(|s| match s {
            SlotView::Browser(b) => Some(b),
            _ => None,
        })
        .expect("a listing");
    assert!(l.path_display.contains("docs"), "{}", l.path_display);
    let under_cursor = l
        .rows
        .iter()
        .find(|r| Some(r.key) == l.cursor)
        .map(|r| r.display_name.clone());
    assert_eq!(under_cursor.as_deref(), Some("b.md"));
    let active = snap
        .layout
        .placements
        .iter()
        .find(|p| p.role == Some(norte_ui_host::dto::SlotRole::Active))
        .map(|p| p.slot_id);
    assert_eq!(active, Some(l.slot_id), "the keys are the listing's");
    assert!(snap.viewer.is_none());
}

/// "Search here" stops the run at once; if the form then cannot be read
/// nothing relaunches, and the panel must not keep saying it is searching.
#[tokio::test(start_paused = true)]
async fn rerooting_with_an_unreadable_form_ends_the_run() {
    let (mut h, _fake) = harness_with_fake().await;
    let slot = h.open_slot("layout.search").await;
    h.act(UiAction::SearchPanelEdit {
        slot_id: slot,
        field: "content".into(),
        value: text("aguja"),
    })
    .await;
    h.act(UiAction::SearchPanelGo { slot_id: slot }).await;
    h.settle().await;
    assert!(h.panel(slot).await.running);
    h.act(UiAction::SearchPanelEdit {
        slot_id: slot,
        field: "regex".into(),
        value: DialogFieldValue::Toggled,
    })
    .await;
    h.act(UiAction::SearchPanelEdit {
        slot_id: slot,
        field: "content".into(),
        value: text("(agu"),
    })
    .await;
    h.act(UiAction::SearchPanelReroot { slot_id: slot }).await;
    tokio::time::advance(DEBOUNCE).await;
    h.settle().await;
    let s = h.panel(slot).await;
    assert_eq!(s.unreadable.as_deref(), Some("content"));
    assert!(!s.running, "no walk is left, so nothing is running");
}

/// Review Focus 5: closing the panel cancels the run and forgets its row.
#[tokio::test(start_paused = true)]
async fn closing_the_panel_cancels_its_run() {
    let (mut h, fake) = harness_with_fake().await;
    let slot = h.open_slot("layout.search").await;
    h.act(UiAction::SearchPanelEdit {
        slot_id: slot,
        field: "content".into(),
        value: text("aguja"),
    })
    .await;
    tokio::time::advance(DEBOUNCE).await;
    h.settle().await;
    assert_eq!(fake.searches_launched(), 1);
    assert_eq!(
        search_rows(&h.snapshot().await),
        1,
        "the run is on the board"
    );
    assert_eq!(h.open_slot("layout.search").await, 0, "the panel closed");
    assert_eq!(fake.cancellations.load(Ordering::SeqCst), 1);
    assert_eq!(search_rows(&h.snapshot().await), 0, "and its row went");
}

/// The model's epoch starts over with every panel, so the run of a closed
/// panel and the first run of the reopened one are both "epoch 1": the old
/// walk's last words, arriving late, must not end the new run — or the next
/// relaunch would skip cancelling it and its walk would leak.
#[tokio::test(start_paused = true)]
async fn a_closed_panels_late_ending_does_not_touch_the_reopened_one() {
    let (mut h, fake) = harness_with_fake().await;
    let end = Arc::new(tokio::sync::Notify::new());
    *fake.search_end_gate.lock().expect("gate mutex") = Some(Arc::clone(&end));
    let slot = h.open_slot("layout.search").await;
    h.act(UiAction::SearchPanelEdit {
        slot_id: slot,
        field: "content".into(),
        value: text("aguja"),
    })
    .await;
    h.act(UiAction::SearchPanelGo { slot_id: slot }).await;
    h.settle().await;
    assert_eq!(fake.searches_launched(), 1);
    // Closed: the walk is cancelled, but it is held before it ends.
    assert_eq!(h.open_slot("layout.search").await, 0);
    h.settle().await;
    assert_eq!(fake.cancellations.load(Ordering::SeqCst), 1);

    let slot = h.open_slot("layout.search").await;
    assert_ne!(slot, 0, "the panel reopened");
    h.act(UiAction::SearchPanelEdit {
        slot_id: slot,
        field: "content".into(),
        value: text("pajar"),
    })
    .await;
    h.act(UiAction::SearchPanelGo { slot_id: slot }).await;
    h.settle().await;
    assert_eq!(fake.searches_launched(), 2);

    // The old walk ends now, late.
    end.notify_one();
    h.settle().await;
    assert!(
        h.panel(slot).await.running,
        "the old run's ending is not the new run's"
    );

    // So relaunching still cancels the new walk.
    h.act(UiAction::SearchPanelEdit {
        slot_id: slot,
        field: "content".into(),
        value: text("pajares"),
    })
    .await;
    h.act(UiAction::SearchPanelGo { slot_id: slot }).await;
    h.settle().await;
    assert_eq!(fake.searches_launched(), 3);
    assert_eq!(fake.cancellations.load(Ordering::SeqCst), 2);
}

/// "Search here" lets go of what runs under the old root at once, not at
/// the tic.
#[tokio::test(start_paused = true)]
async fn rerooting_cancels_the_run_now() {
    let (mut h, fake) = harness_with_fake().await;
    let slot = h.open_slot("layout.search").await;
    h.act(UiAction::SearchPanelEdit {
        slot_id: slot,
        field: "content".into(),
        value: text("aguja"),
    })
    .await;
    h.act(UiAction::SearchPanelGo { slot_id: slot }).await;
    h.settle().await;
    assert_eq!(fake.cancellations.load(Ordering::SeqCst), 0);
    let ack = h.act(UiAction::SearchPanelReroot { slot_id: slot }).await;
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    assert_eq!(
        fake.cancellations.load(Ordering::SeqCst),
        1,
        "cancelled before the pause is over"
    );
}

/// A run relaunched before the daemon acknowledged the previous one: the
/// old run never reaches the board, and it is cancelled once it lands.
#[tokio::test(start_paused = true)]
async fn a_run_replaced_before_its_id_is_known_is_cancelled_when_it_lands() {
    let (mut h, fake) = harness_with_fake().await;
    let gate = Arc::new(tokio::sync::Notify::new());
    *fake.search_ack_gate.lock().expect("gate mutex") = Some(Arc::clone(&gate));
    let slot = h.open_slot("layout.search").await;
    for t in ["aguja", "pajar"] {
        h.act(UiAction::SearchPanelEdit {
            slot_id: slot,
            field: "content".into(),
            value: text(t),
        })
        .await;
        h.act(UiAction::SearchPanelGo { slot_id: slot }).await;
        h.settle().await;
    }
    assert_eq!(fake.searches_launched(), 2);
    assert_eq!(fake.cancellations.load(Ordering::SeqCst), 0);
    gate.notify_one();
    h.settle().await;
    assert_eq!(fake.cancellations.load(Ordering::SeqCst), 1);
    assert_eq!(search_rows(&h.snapshot().await), 1, "only the live run");
}

/// A batch of `n` files in one folder, one matching line each.
fn many_files(n: usize) -> norte_proto::methods::SearchHits {
    let m = norte_proto::methods::MatchInfo {
        line: Some(1),
        preview: Some("aguja".to_owned()),
        offset: Some(0),
        ranges: Some(vec![(0, 5)]),
    };
    norte_proto::methods::SearchHits {
        task_id: norte_proto::TaskId::new(1),
        entries: (0..n)
            .map(|i| norte_proto::Entry {
                path: VPath::parse(&format!("mem:///casa/f{i:05}.txt")).expect("vpath"),
                kind: norte_proto::EntryKind::File,
                size: Some(1),
                mtime_ms: Some(0),
                attrs: std::collections::BTreeMap::new(),
            })
            .collect(),
        matches: Some(vec![m.clone(); n]),
        all_matches: Some(vec![vec![m]; n]),
        more: Some(vec![0; n]),
    }
}

/// Searches `aguja` with `hits` answering, and lets them land.
async fn search_answered(
    h: &mut Harness,
    fake: &Fake,
    slot: u32,
    hits: norte_proto::methods::SearchHits,
) {
    fake.answer_search_with(hits);
    h.act(UiAction::SearchPanelEdit {
        slot_id: slot,
        field: "content".into(),
        value: text("aguja"),
    })
    .await;
    h.act(UiAction::SearchPanelGo { slot_id: slot }).await;
    h.settle().await;
}

/// The wheel scrolled the results far from the cursor: the rows sent are
/// the ones scrolled to, not the cursor's — or the renderer paints blanks.
/// A keyboard move brings the view back around the cursor.
#[tokio::test(start_paused = true)]
async fn the_view_carries_the_rows_scrolled_to_and_a_key_brings_the_cursor_back() {
    let (mut h, fake) = harness_with_fake().await;
    let slot = h.open_slot("layout.search").await;
    // One folder, 2000 files, a line each: 4001 rows.
    search_answered(&mut h, &fake, slot, many_files(2000)).await;
    let ack = h
        .act(UiAction::SearchPanelVisibleRange {
            slot_id: slot,
            first: 3000,
            count: 60,
        })
        .await;
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    let s = h.panel(slot).await;
    assert_eq!(s.cursor, Some(0));
    assert_eq!((s.first_row, s.rows.len()), (3000, 60));

    h.act(UiAction::FocusSlot { slot_id: slot }).await;
    h.act(super::base::press("ArrowDown")).await;
    let s = h.panel(slot).await;
    assert_eq!(s.cursor, Some(1));
    assert_eq!(s.first_row, 0, "around the cursor again");
}

/// Enter in an empty form clears: nothing launches, and the status says
/// what to type.
#[tokio::test(start_paused = true)]
async fn an_empty_form_launches_nothing() {
    let (mut h, fake) = harness_with_fake().await;
    let slot = h.open_slot("layout.search").await;
    h.act(UiAction::SearchPanelGo { slot_id: slot }).await;
    h.settle().await;
    assert_eq!(fake.searches_launched(), 0);
    let s = h.panel(slot).await;
    assert_eq!(
        s.status,
        norte_i18n::t_in(norte_i18n::Lang::Es, "search-panel-idle")
    );
    assert!(s.rows.is_empty() && s.cursor.is_none());
}

/// A click painted before a batch renumbered the rows names a row by an
/// index that now holds another: refused as stale, and nothing opens.
#[tokio::test(start_paused = true)]
async fn a_click_from_an_older_numbering_is_refused() {
    let (mut h, fake) = harness_with_fake().await;
    let slot = h.open_slot("layout.search").await;
    search_answered(&mut h, &fake, slot, many_files(3)).await;
    let s = h.panel(slot).await;
    let ack = h
        .act(UiAction::SearchPanelActivate {
            slot_id: slot,
            row: 2,
            generation: s.generation.wrapping_sub(1),
        })
        .await;
    assert!(
        matches!(
            ack,
            ActionAck::Stale {
                reason: StaleAction::Generation,
                ..
            }
        ),
        "{ack:?}"
    );
    h.settle().await;
    assert!(h.snapshot().await.viewer.is_none(), "nothing opened");
    let ack = h
        .act(UiAction::SearchPanelFold {
            slot_id: slot,
            row: 0,
            open: false,
            generation: s.generation.wrapping_add(1),
        })
        .await;
    assert!(matches!(ack, ActionAck::Stale { .. }), "{ack:?}");
    assert_eq!(
        h.panel(slot).await.total_rows,
        s.total_rows,
        "nothing folded"
    );
}

/// An action naming another slot, or a row that is not there, is a race:
/// stale, and nothing happens.
#[tokio::test(start_paused = true)]
async fn a_stale_slot_or_row_is_refused() {
    let (mut h, _fake) = harness_with_fake().await;
    let slot = h.open_slot("layout.search").await;
    let ack = h
        .act(UiAction::SearchPanelGo {
            slot_id: slot + 100,
        })
        .await;
    assert!(matches!(ack, ActionAck::Stale { .. }), "{ack:?}");
    let generation = h.panel(slot).await.generation;
    let ack = h
        .act(UiAction::SearchPanelActivate {
            slot_id: slot,
            row: 9,
            generation,
        })
        .await;
    assert!(matches!(ack, ActionAck::Stale { .. }), "{ack:?}");
    let ack = h
        .act(UiAction::SearchPanelEdit {
            slot_id: slot,
            field: "nope".into(),
            value: text("x"),
        })
        .await;
    assert!(matches!(ack, ActionAck::Unavailable { .. }), "{ack:?}");
}
