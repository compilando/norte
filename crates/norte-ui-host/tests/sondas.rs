//! A slot that FOLLOWS the cursor has its own path to the renderer.
//!
//! F1.3 of the parity plan, and decision 2 of ADR 0097: the window speaks in
//! PATCHES, so whatever does not fit in a patch stays as it was until
//! something triggers a whole snapshot. For a panel that describes what the
//! cursor points at, that is not a delay: it is a panel that LIES, because it
//! shows one file's attributes while the listing highlights another.
//!
//! It has already happened twice. The docked viewer (#291) and the
//! attributes sheet used to travel "for free" in the snapshot another panel
//! triggered, so a layout with the sheet and no viewer left it frozen on
//! whatever was there at startup. The fix was the same in both cases — a
//! PROBE after every message from the actor, which is the closest thing to a
//! frame a host that only speaks when something changes has — and this file
//! is the guard so a third panel that follows the cursor does not repeat the
//! trip.
//!
//! **What is measured is the product, not a list.** For every panel the bar
//! offers: it is opened, the listing's cursor is moved, and it is checked
//! whether THAT slot's view changed. If it changed, the change had to have
//! arrived on its own. A new panel joins the check without touching this
//! file, because the enumeration comes from the panel bar.
//!
//! The two tests split the two failures, and the pair is needed. With the
//! sheet's probe REMOVED from the actor's loop — the content is computed but
//! not sent — the first one fails: it changes and did not travel. With the
//! viewer's probe dead altogether — it is not computed — the SECOND one
//! fails, because then nothing changes and the panel stops looking like it
//! follows the cursor. Verified by sabotaging each one separately.

use std::sync::Arc;
use std::time::Duration;

use norte_proto::VPath;
use norte_ui_host::action::UiAction;
use norte_ui_host::dto::{SlotView, UiUpdate};
use norte_ui_host::{UiHost, UiHostOptions, UiSubscription, Update, ViewSnapshot};

mod backend_falso;
use backend_falso::arbol_de_prueba;

/// The panels that do NOT follow the listing's cursor, and why not.
///
/// Being here is a CHECKED ASSERTION, not an exemption: the test also fails
/// the other way, if one of these turns out to change when the cursor moves.
/// The reason matters because "does not follow" and "follows and got frozen"
/// look the same on screen the day someone breaks it.
/// Kinds whose button IS in the bar — it comes from the shared registry —
/// but that this window does not yet paint, with the issue that closes it.
///
/// Being here is NOT a permanent exemption: the parity gate (`paridad.rs`,
/// `APLAZADOS`) carries the same issue, so implementing it means removing it
/// from both places. It is skipped by this sweep because its premise —
/// pressing the button opens a slot — only holds for what the window knows
/// how to paint; against a kind it does not have, the host answers "not
/// implemented", which is the correct response and not a failure.
const NO_WINDOW: &[(&str, u32)] = &[("timeline", 359)];

/// Kinds this HARNESS cannot probe, and why.
///
/// Different from [`NO_WINDOW`], and the difference matters: those are
/// missing work, with their issue. These the window handles perfectly — what
/// the harness lacks is the precondition.
const NO_PROBE: &[(&str, &str)] = &[(
    "terminal",
    "a shell sits in a filesystem directory, and this harness's panels are \
     `mem:///`. The panel REFUSES to open there, which is the correct \
     behavior and the same as `app.terminal`: probing it would require a \
     backend with real local paths",
)];

const DO_NOT_FOLLOW: &[(&str, &str)] = &[
    (
        "places",
        "shows volumes and favorites, which belong to the host and not the row",
    ),
    (
        "processes",
        "has its OWN cursor over the tasks; the listing's says nothing to it",
    ),
    ("log", "shows what this process logs, not an entry"),
    (
        "tree",
        "follows the DIRECTORY, which only changes with a `cd`, not a row",
    ),
    (
        "disk-map",
        "describes the DIRECTORY being looked at, not the row: moving the \
         cursor does not change what is around it",
    ),
];

/// Where the cursor is moved to: a REAL file.
///
/// "One row down" does not work. The cursor is born on `..` and below it is
/// another directory, and the docked viewer shows the same note for both —
/// "this cannot be read" — so that move would leave it the same and the
/// panel would come out as not following the cursor. A green from not
/// having asked.
const FILE: &str = "notas.txt";

/// How many rows there are from the cursor to the row with this name.
fn rows_to(snap: &ViewSnapshot, name: &str) -> i64 {
    let SlotView::Browser(b) = snap
        .slots
        .iter()
        .find(|s| matches!(s, SlotView::Browser(_)))
        .expect("there is a listing")
    else {
        unreachable!("filtered above")
    };
    let cursor = b.cursor.map_or(0, |k| usize::try_from(k.0).unwrap_or(0));
    let target = b
        .rows
        .iter()
        .position(|r| r.display_name == name)
        .unwrap_or_else(|| panic!("`{name}` is not in the listing"));
    i64::try_from(target).unwrap_or(0) - i64::try_from(cursor).unwrap_or(0)
}

/// What was observed of a panel when the cursor moved under it.
#[derive(Debug)]
struct Observation {
    /// Is its view different after moving the cursor?
    changes: bool,
    /// Did that change arrive ON ITS OWN, without the renderer requesting
    /// anything?
    travels: bool,
}

/// A `kind` slot's view in a snapshot, if present.
///
/// The kind → variant map is the only hand-written part, and it cannot be
/// derived: the wire calls `preview` what the layout calls `viewer`, which is
/// exactly the kind of synonym an exhaustive `match` cannot see.
fn view_of(snap: &ViewSnapshot, kind: &str) -> Option<SlotView> {
    snap.slots
        .iter()
        .find(|s| match (kind, s) {
            ("metadata", SlotView::Metadata(_))
            | ("places", SlotView::Places(_))
            | ("tree", SlotView::Tree(_))
            | ("processes", SlotView::Processes { .. })
            | ("log", SlotView::Log(_))
            | ("viewer", SlotView::Preview(_))
            | ("disk-map", SlotView::DiskMap(_))
            | ("timeline", SlotView::Timeline(_))
            | ("terminal", SlotView::Terminal(_)) => true,
            (_, SlotView::Unsupported { kind_name, .. }) => kind_name == kind,
            _ => false,
        })
        .cloned()
}

/// Drains whatever is pending, without waiting on anything.
///
/// Only to catch up after opening a panel or moving focus: nothing is
/// decided here, so a message that arrives late does not break the test —
/// the wait afterward will see it.
async fn drain(sub: &mut UiSubscription) {
    while let Ok(received) = tokio::time::timeout(Duration::from_millis(50), sub.recv()).await {
        let _ = received.expect("the host is still alive");
    }
}

/// Waits for a snapshot to arrive ON ITS OWN in which `kind`'s view is no
/// longer `before`, or `None` if none arrives within the whole deadline.
///
/// A silence deadline does not work for this. "Nothing arrived in 150 ms"
/// and "this panel does not follow the cursor" look the same, and under the
/// gate's load a probe that reads a file takes longer than that: the test
/// would turn red saying "frozen panel" over a lost race, which is exactly
/// the kind of intermittent red this repository treats as a bug.
///
/// This way the green path is immediate — the snapshot is already waiting —
/// and the long deadline is only spent on the panels that really do not
/// change, where their response is the correct one.
async fn wait_for_change(
    sub: &mut UiSubscription,
    kind: &str,
    before: &SlotView,
) -> Option<SlotView> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return None;
        }
        let Ok(received) = tokio::time::timeout(left, sub.recv()).await else {
            return None;
        };
        if let Update::Message(m) = received.expect("the host is still alive")
            && let UiUpdate::Snapshot(s) = m.payload
            && let Some(now) = view_of(&s, kind)
            && &now != before
        {
            return Some(now);
        }
    }
}

/// Requests a whole snapshot and waits for it to arrive.
async fn request_snapshot(host: &UiHost, sub: &mut UiSubscription) -> ViewSnapshot {
    host.dispatch(UiAction::Resync).await.expect("host alive");
    for _ in 0..20 {
        let next = tokio::time::timeout(Duration::from_millis(500), sub.recv())
            .await
            .expect("a snapshot, not a hang")
            .expect("the host is still alive");
        if let Update::Message(m) = next
            && let UiUpdate::Snapshot(s) = m.payload
        {
            return *s;
        }
    }
    panic!("no snapshot ever arrived");
}

/// Starts a host with just the listing, over the test tree.
async fn start() -> (UiHost, ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend: Arc::new(arbol_de_prueba()),
        initial_dir: VPath::parse("mem:///casa").expect("vpath"),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: norte_ui_host::ajustes_por_defecto(),
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

/// A kind's observation, BOXED.
///
/// An `async fn`'s future travels whole across every `await`, and this one
/// builds a host and keeps two snapshots: as the snapshot grew it went past
/// the 16 KB clippy tolerates. The box goes here, at the root, and not in the
/// two loops that call it.
fn observe(kind: &str) -> std::pin::Pin<Box<dyn std::future::Future<Output = Observation> + '_>> {
    Box::pin(observe_inner(kind))
}

/// Opens `kind`'s panel, moves the listing's cursor, and looks at what
/// happened.
async fn observe_inner(kind: &str) -> Observation {
    let (host, first) = start().await;
    let mut sub = host.subscribe();

    // The panel bar is the enumeration: a click comes back as the INDEX in
    // its list, which is the only thing the renderer can name.
    let button = first
        .panel_bar
        .buttons
        .iter()
        .position(|b| b.kind == kind)
        .unwrap_or_else(|| panic!("`{kind}` is not in the panel bar"));
    host.dispatch(UiAction::PanelBarActivate {
        button: u32::try_from(button).expect("fits"),
    })
    .await
    .expect("host alive");
    drain(&mut sub).await;

    // Focus returns to the listing: opening a panel that takes focus grabs
    // it, and `MoveCursor` on a slot that is not active gets answered
    // `Stale` — so without this the cursor would not move and ALL panels
    // would come out "does not follow", which is a green that proves
    // nothing.
    host.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host alive");
    drain(&mut sub).await;

    let snap = request_snapshot(&host, &mut sub).await;
    let delta = rows_to(&snap, FILE);
    let before = view_of(&snap, kind).unwrap_or_else(|| panic!("`{kind}` did not open"));

    host.dispatch(UiAction::MoveCursor { slot_id: 1, delta })
        .await
        .expect("host alive");

    // First, the question that actually matters: did a change from THIS
    // panel arrive ON ITS OWN? If it did, everything is already answered and
    // there is no deadline to spend.
    if wait_for_change(&mut sub, kind, &before).await.is_some() {
        return Observation {
            changes: true,
            travels: true,
        };
    }

    // Nothing arrived. Now the panel that does not follow the cursor — the
    // correct behavior — is told apart from the frozen panel: the snapshot
    // is REQUESTED and it is checked whether its view was different all
    // along.
    let after = request_snapshot(&host, &mut sub).await;
    let after = view_of(&after, kind).unwrap_or_else(|| panic!("`{kind}` is no longer open"));
    Observation {
        changes: before != after,
        travels: false,
    }
}

/// The guard: if a panel's view depends on the cursor, the change arrives on
/// its own.
///
/// What fails here is a frozen panel, and it reads as such: "changes when
/// the cursor moves and the change did not arrive on its own".
#[tokio::test(flavor = "multi_thread")]
async fn every_slot_that_follows_the_cursor_has_a_probe() {
    let (host, first) = start().await;
    let kinds: Vec<String> = first
        .panel_bar
        .buttons
        .iter()
        .map(|b| b.kind.clone())
        .collect();
    drop(host);
    assert!(!kinds.is_empty(), "the panel bar offers nothing");

    for kind in &kinds {
        if NO_WINDOW.iter().any(|(k, _)| k == kind) || NO_PROBE.iter().any(|(k, _)| k == kind) {
            continue;
        }
        let o = observe(kind).await;
        if o.changes {
            assert!(
                o.travels,
                "`{kind}` changes when the cursor moves and the change did NOT \
                 arrive on its own: it stays frozen until something else \
                 triggers a whole snapshot"
            );
        }
    }
}

/// And the other way around: the list of the ones that do not follow is an
/// assertion, not an exemption. A panel that starts following the cursor
/// without saying so here fails, even with a probe — because then the
/// written reason is false.
#[tokio::test(flavor = "multi_thread")]
async fn the_list_of_non_followers_is_up_to_date() {
    let (host, first) = start().await;
    let kinds: Vec<String> = first
        .panel_bar
        .buttons
        .iter()
        .map(|b| b.kind.clone())
        .collect();
    drop(host);

    for kind in &kinds {
        if NO_WINDOW.iter().any(|(k, _)| k == kind) || NO_PROBE.iter().any(|(k, _)| k == kind) {
            continue;
        }
        let declared = DO_NOT_FOLLOW.iter().find(|(k, _)| k == kind);
        let o = observe(kind).await;
        match declared {
            Some((_, reason)) => assert!(
                !o.changes,
                "`{kind}` is declared as not following the cursor ({reason}), \
                 but its view changed when it moved"
            ),
            None => assert!(
                o.changes,
                "`{kind}` is not in DO_NOT_FOLLOW, so it should follow the \
                 cursor, and its view did not move"
            ),
        }
    }
}
