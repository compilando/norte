//! How much WEIGHT crosses the bridge. Task 3.6's budget.
//!
//! These numbers are half of the question the Tauri spike exists to answer:
//! a vertical slice can look perfect and still be a no-go if moving the
//! cursor in a directory of a hundred thousand entries sends a hundred
//! thousand rows in JSON. They are measured here, in the host, because that
//! is where they are generated — and that way the measurement does not
//! depend on there being a screen.

use std::sync::Arc;

use norte_proto::VPath;
use norte_ui_host::action::UiAction;
use norte_ui_host::controller::{UiHost, UiHostOptions, Update};
use norte_ui_host::dto::{UiUpdate, ViewChange};

mod backend_falso;
use backend_falso::Falso;

/// The ceiling for a cursor patch (task 3.6).
const CURSOR_CEILING: usize = 16 * 1024;

/// Entries in the large directory.
const LARGE: usize = 100_000;

/// The large directory's host, with its future on the HEAP.
///
/// `Estado` is large — it is the window's whole state — and the future that
/// builds it carries it inside, so as soon as one more field grows, the
/// future goes over `clippy::large_futures`'s ceiling and the gate turns red
/// in a file nobody touched. Boxing it once, here, means the three callers
/// no longer have to know about it.
fn large_host() -> std::pin::Pin<Box<dyn Future<Output = (UiHost, norte_ui_host::ViewSnapshot)>>> {
    Box::pin(large_host_inner())
}

async fn large_host_inner() -> (UiHost, norte_ui_host::ViewSnapshot) {
    let mut f = Falso::default();
    let names: Vec<(Vec<u8>, bool)> = (0..LARGE)
        .map(|i| (format!("fichero-{i:06}.txt").into_bytes(), false))
        .collect();
    f.pon("mem:///casa", names);
    host_with(Arc::new(f)).await
}

async fn host_with(backend: Arc<Falso>) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: VPath::parse("mem:///casa").expect("vpath"),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("orthodox").expect("layout"),
        viewport: (200, 60),
        // The `..` row turned off: these tests reason about listing indices,
        // and one more row at the start would shift them all without saying
        // anything about what they test.
        settings: {
            let mut cfg = norte_ui_host::ajustes_por_defecto();
            cfg.common.ui_parent_entry = Some(false);
            cfg
        },
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("starts")
}

fn bytes(u: &norte_ui_host::BridgeEnvelope<UiUpdate>) -> usize {
    serde_json::to_vec(u)
        .expect("the envelope serializes")
        .len()
}

/// Waits for the first patch matching `pred`, skipping the listing's filler
/// (the host keeps draining a hundred thousand entries behind the scenes)
/// and the lag notices.
///
/// Since #252 the filler no longer publishes a patch per batch — only when
/// the visible window actually changes, plus the last one — so what needs
/// skipping is two or three patches, not two hundred. The helper stays
/// because those two can still slip in between the action and its response;
/// what disappeared is the burst that forced a `Lagged` and a whole
/// snapshot.
async fn next_patch(
    sub: &mut norte_ui_host::UiSubscription,
    pred: impl Fn(&norte_ui_host::dto::ViewPatch) -> bool,
) -> norte_ui_host::BridgeEnvelope<UiUpdate> {
    for _ in 0..2000 {
        let next = tokio::time::timeout(std::time::Duration::from_secs(10), sub.recv())
            .await
            .expect("an update before the deadline")
            .expect("the host is still alive");
        if let Update::Message(m) = next
            && let UiUpdate::Patch(p) = &m.payload
            && pred(p)
        {
            return *m;
        }
    }
    panic!("the expected patch never arrived");
}

/// Moving the cursor sends the cursor, and the cursor fits well within the
/// ceiling.
#[tokio::test]
async fn a_cursor_patch_does_not_even_reach_a_kilobyte() {
    let (h, _snap) = large_host().await;
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 0,
        count: 60,
    })
    .await
    .expect("host alive");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::MoveCursor {
        slot_id: 1,
        delta: 1,
    })
    .await
    .expect("host alive");

    let m = next_patch(&mut sub, |p| {
        p.changes
            .iter()
            .all(|c| matches!(c, ViewChange::Cursor { .. }))
    })
    .await;
    let n = bytes(&m);
    assert!(
        n <= CURSOR_CEILING,
        "a cursor patch is {n} bytes, and the ceiling is {CURSOR_CEILING}"
    );
    // And above all: it carries NO rows.
    let UiUpdate::Patch(p) = &m.payload else {
        panic!("moving the cursor is a patch, not a snapshot");
    };
    assert!(
        p.changes
            .iter()
            .all(|c| matches!(c, ViewChange::Cursor { .. })),
        "and the only thing it carries is the cursor: {:?}",
        p.changes
    );
    println!("cursor patch over {LARGE} entries: {n} bytes");
}

/// Marking re-sends the WINDOW, not the directory: the cost is the screen's
/// size, not the directory's size.
#[tokio::test]
async fn a_row_patch_weighs_what_the_window_does() {
    let (h, snap) = large_host().await;
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 0,
        count: 60,
    })
    .await
    .expect("host alive");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::ToggleMark {
        slot_id: 1,
        key: norte_ui_host::RowKey(3),
        generation: generation_of(&snap.slots[0]),
    })
    .await
    .expect("host alive");
    // From slot 1, the one the window was declared on: the layout has two
    // listings and the other one sends its own rows with ITS OWN size.
    let m = next_patch(&mut sub, |p| {
        p.changes
            .iter()
            .any(|c| matches!(c, ViewChange::Rows { slot_id: 1, .. }))
    })
    .await;
    let n = bytes(&m);
    let UiUpdate::Patch(p) = &m.payload else {
        unreachable!("filtered above")
    };
    let rows: usize = p
        .changes
        .iter()
        .map(|c| match c {
            ViewChange::Rows {
                slot_id: 1, rows, ..
            } => rows.len(),
            _ => 0,
        })
        .sum();
    assert!(
        rows <= 60,
        "the visible ones travel ({rows}), not the {LARGE}"
    );
    assert!(
        n < 32 * 1024,
        "a window of 60 rows is {n} bytes, which cannot depend on the directory"
    );
    println!("patch of {rows} rows over {LARGE} entries: {n} bytes");
}

/// The slot's generation, which in this layout is always a listing.
///
/// A slot that is not one here would be a test that stopped testing what it
/// claims to, so it STOPS instead of sending a made-up generation the host
/// would reject as stale.
fn generation_of(s: &norte_ui_host::dto::SlotView) -> u64 {
    match s {
        norte_ui_host::dto::SlotView::Browser(b) => b.generation,
        other => panic!("slot 0 of this layout is a listing: {other:?}"),
    }
}

/// How many ROWS a slot carries in the snapshot.
///
/// An exhaustive `match` with NO wildcard, on purpose. The size guard
/// measures that the initial snapshot does not carry the whole directory,
/// and a `_ => 0` would silently disarm it: places and processes slots also
/// send rows and counted as zero, so the count was measuring a fifth of the
/// message. And along the way it would lose the canary that forces this to
/// be looked at whenever `SlotView` grows.
fn rows_of(s: &norte_ui_host::dto::SlotView) -> usize {
    use norte_ui_host::dto::SlotView;
    match s {
        SlotView::Browser(b) => b.rows.len(),
        SlotView::Places(p) => p.rows.len(),
        SlotView::Tree(t) => t.rows.len(),
        SlotView::Metadata(m) => m.fields.len(),
        // The log DOES carry its own, and that is why it counts: it sends a
        // WINDOW of the ring, not the ring — two thousand lines per patch is
        // exactly what this count exists to keep anyone from slipping in
        // unnoticed.
        SlotView::Log(l) => l.lines.len(),
        // The docked viewer (#291) carries the file's lines, IN FULL up to
        // the bridge's ceiling: they count, and the ceiling above bounds
        // them.
        SlotView::Preview(p) => p.viewer.as_ref().map_or(0, |v| v.lines.len()),
        // A PLUGIN panel carries the lines its guest described, and they
        // count like any other: the line ceiling belongs to the protocol
        // (`PANEL_MAX_LINES`), but this message is what pays for them.
        SlotView::Panel(p) => p.lines.len(),
        // The disk map carries the treemap already laid out by the host, and
        // its lines count like a plugin panel's: the ceiling is the frame's
        // (`PANEL_MAX_LINES`), but this message is what pays for them. A
        // zero here would disarm the guard for exactly the slot that can
        // grow most easily — a map is as many lines as it is tall.
        SlotView::DiskMap(m) => m.lines.len(),
        // The timeline carries its LOADED rows, bounded by the bridge's row
        // ceiling: they count, and one more page arriving at the bottom adds
        // up.
        SlotView::Timeline(t) => t.rows.len(),
        // The terminal sends its WHOLE grid on every repaint — it does not
        // scroll like a list, it repaints — so its rows are the ones that
        // cross over the most times of all. Counting them is exactly the
        // canary: a tall panel inside a `make` can send fifty rows several
        // times a second, and that cost has to show up in this count.
        SlotView::Terminal(t) => t.rows.len(),
        // The processes panel does not carry its rows in the slot: they are
        // carried by `ViewSnapshot::tasks`, which is a single list for the
        // whole screen.
        SlotView::Processes { .. } | SlotView::Unsupported { .. } => 0,
    }
}

/// The startup snapshot does not carry the whole directory either.
#[tokio::test]
async fn the_initial_snapshot_does_not_carry_a_hundred_thousand_rows() {
    let (_h, snap) = large_host().await;
    let n = serde_json::to_vec(&snap).expect("serializes").len();
    let rows: usize = snap.slots.iter().map(rows_of).sum();
    assert!(
        rows <= 2 * 64,
        "both listings send their window ({rows} rows), not {LARGE}"
    );
    assert!(n < 64 * 1024, "the initial snapshot is {n} bytes");
    println!("initial snapshot with {LARGE} entries: {n} bytes, {rows} rows");
}

/// Filling a large listing does NOT publish a patch per batch (#252).
///
/// It is drained in batches of 500 and each one used to publish its own row
/// patch: in a directory of a hundred thousand entries that is two hundred
/// patches in a burst against a channel of 64, so any subscriber that does
/// not drain at that speed gets `Lagged` and has to request a whole
/// snapshot. And almost all of them carried the SAME rows, because what got
/// mixed in fell well below the visible window: the renderer repainted the
/// same thing two hundred times.
///
/// The ceiling is generous on purpose. What is being asserted is not a fine
/// number — how many times the window changes depends on the order the
/// provider delivers in — but that there is no longer ONE PER BATCH.
#[tokio::test]
async fn filling_does_not_publish_a_patch_per_batch() {
    let how_many = LARGE;
    // The double's gate stops the stream right after the first page, so at
    // subscription time the fill has NOT started: without it, the fake
    // drains a hundred thousand entries into memory before this test even
    // looks.
    let gate = Arc::new(backend_falso::Puerta::default());
    let mut f = Falso::default();
    let names: Vec<(Vec<u8>, bool)> = (0..how_many)
        .map(|i| (format!("fichero-{i:06}.txt").into_bytes(), false))
        .collect();
    f.pon("mem:///casa", names);
    f.puerta_drenaje = Some(Arc::clone(&gate));
    let (h, _snap) = host_with(Arc::new(f)).await;
    let mut sub = h.subscribe();
    gate.abrir();

    let mut row_patches = 0usize;
    let mut seen_the_end = false;
    for _ in 0..2000 {
        // The deadlines are NOT a pace: they mean "the host has gone quiet".
        // Thirty seconds for the fill to START — a hundred thousand entries
        // get mixed in across two hundred batches, and with this file's
        // other tests running at the same time that takes as long as it
        // takes — and five for the tail, which is what keeps the test from
        // spending half a minute waiting on nothing.
        let deadline = if row_patches == 0 { 30 } else { 5 };
        let Ok(Some(next)) =
            tokio::time::timeout(std::time::Duration::from_secs(deadline), sub.recv()).await
        else {
            break;
        };
        if let Update::Message(m) = next
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let ViewChange::Rows { rows, .. } = c {
                    row_patches += 1;
                    // The fill's last patch brings the whole window.
                    seen_the_end = !rows.is_empty();
                }
            }
        }
    }

    let batches = how_many / 500;
    assert!(
        seen_the_end,
        "the fill has to publish at least one row patch"
    );
    assert!(
        row_patches < batches / 4,
        "{row_patches} row patches for {batches} batches: the fill is still \
         publishing one per batch"
    );
}
