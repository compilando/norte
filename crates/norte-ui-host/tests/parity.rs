//! Parity: the TERMINAL and the WINDOW do the same thing, and both do what
//! the shared primitives say.
//!
//! No frontend should reimplement a presentation rule. That is easy to say
//! in a comment and hard to maintain: it only takes one homegrown `sort`, a
//! hand-rolled cursor clamp, or a "this is more convenient this way" for two
//! surfaces to start reading differently without anything turning red.
//!
//! Each scenario runs THREE times — against bare `norte_frontend::PaneState`
//! + `nav::History`, against the host through its actions and snapshots, and
//! against `norte-tui` through its own decision functions — and the
//! SEMANTIC state is compared step by step: where the cursor is, what is
//! marked, which directory is shown and with which names. Never pixels.
//!
//! **The third leg is the one that actually catches something, and it was
//! missing** (ADR 0097, D1). The first two measure the host against a
//! harness written with the HOST's OWN rules: its `Enter` step was
//! `selected().filter(|e| e.kind == Dir)`, which is what the window does and
//! not what the terminal does, so the comparison could not fail. The
//! 2026-09-05 parity audit found seventeen decisions already diverged
//! beneath this file.
//!
//! The test tree already brings a `.zip` and a symlink, which was
//! divergence number one in the inventory: `Enter` on either navigated in
//! the terminal and called `xdg-open` in the window. All three legs now ask
//! `norte_frontend::nav::enter_target`, so the question "does this get
//! entered?" has ONE answer and the scenarios can exercise it.
//!
//! What this still does NOT reach: a parity harness catches DIVERGENCE, not
//! a shared bug. If both surfaces get it wrong the same way — because both
//! read the same primitive — this comes out green. When adding a scenario,
//! sabotage just one of the legs and check it turns red; narrowing the
//! primitive narrows all three and proves nothing.

use std::sync::Arc;

use norte_frontend::PaneState;
use norte_frontend::nav::{History, Trail};
use norte_proto::{Entry, VPath};
use norte_ui_host::action::UiAction;
use norte_ui_host::dto::SlotView;
use norte_ui_host::{UiHost, UiHostOptions, UiSubscription, Update, ViewSnapshot, dto::UiUpdate};

mod backend_falso;
use backend_falso::{Falso, arbol_de_prueba};

/// A scenario step, in SEMANTIC vocabulary: neither keys nor bridge actions,
/// so the comparison does not depend on how each surface gets there.
#[derive(Debug, Clone, Copy)]
enum Step {
    /// Moves the cursor this many rows.
    Cursor(i64),
    /// Puts the cursor on the row with this name, coming down from the top.
    ///
    /// By NAME and not by index because `compare` runs each scenario with
    /// the `..` row both on and off, and the same entry's index is not the
    /// same in both. Each surface does it with its own cursor keys: what is
    /// compared is still where it ends up.
    CursorTo(&'static str),
    /// Marks or unmarks the cursor's row.
    Mark,
    /// Enters the directory under the cursor.
    Enter,
    /// Goes up to the parent.
    Up,
    /// Back in the trail.
    Back,
    /// Forward in the trail.
    Forward,
}

/// What is compared: the state a user could describe out loud.
#[derive(Debug, PartialEq, Eq)]
struct Semantic {
    dir: String,
    cursor: usize,
    marks: usize,
    names: Vec<String>,
}

/// The scenario run against the bare shared primitives.
fn via_primitives(steps: &[Step], parent_row: bool) -> Vec<Semantic> {
    let tree = arbol_de_prueba();
    let start = VPath::parse("mem:///casa").expect("vpath");
    let mut pane = PaneState::new(start.clone(), entries_of(&tree, &start));
    pane.set_parent_row(parent_row);
    let mut history = History::default();
    let mut out = vec![primitives_snapshot(&pane)];

    for step in steps {
        match step {
            Step::Cursor(delta) => {
                for _ in 0..delta.unsigned_abs() {
                    if *delta < 0 {
                        pane.cursor_up();
                    } else {
                        pane.cursor_down();
                    }
                }
            }
            Step::CursorTo(name) => {
                for _ in 0..pane.entries().len() {
                    pane.cursor_up();
                }
                for _ in 0..pane.entries().len() {
                    if primitives_snapshot(&pane).names.get(pane.cursor())
                        == Some(&(*name).to_owned())
                    {
                        break;
                    }
                    pane.cursor_down();
                }
            }
            Step::Mark => pane.toggle_mark(),
            Step::Enter => {
                // On `..`, Enter GOES UP: it is the only thing that row
                // knows how to do, and both surfaces do it — the TUI in
                // `trail::nav_enter_target` and the host in
                // `UiAction::Activate`, which sees the synthetic `Entry` and
                // navigates to its path. Modeling it as "no operand, nothing
                // happens" would measure the harness and not the product.
                //
                // And what is "enterable" is answered by `nav::enter_target`,
                // the SHARED function both surfaces use. There used to be a
                // hand-written `selected().filter(kind == Dir)` here — i.e.
                // the window's rule — and that is why this comparison could
                // not fail on a `.zip` or a link: it measured the harness
                // and not the product. It is the failure this file's header
                // describes, and it can now be removed.
                let target = if pane.is_parent_row(pane.cursor()) {
                    pane.parent_target().cloned()
                } else {
                    pane.selected().and_then(norte_frontend::nav::enter_target)
                };
                let Some(target) = target else {
                    out.push(primitives_snapshot(&pane));
                    continue;
                };
                navigate(&mut pane, &mut history, &target, Trail::Record, &tree);
            }
            Step::Up => {
                let current = pane.dir().clone();
                let Some(parent) = current.parent() else {
                    out.push(primitives_snapshot(&pane));
                    continue;
                };
                pane.set_pending_focus(current);
                navigate(&mut pane, &mut history, &parent, Trail::Record, &tree);
            }
            Step::Back | Step::Forward => {
                let current = pane.dir().clone();
                let target = if matches!(step, Step::Back) {
                    history.step_back(current)
                } else {
                    history.step_forward(current)
                };
                let Some(target) = target else {
                    out.push(primitives_snapshot(&pane));
                    continue;
                };
                navigate(
                    &mut pane,
                    &mut history,
                    &target,
                    Trail::Replay(if matches!(step, Step::Back) {
                        norte_frontend::nav::TrailStep::Back
                    } else {
                        norte_frontend::nav::TrailStep::Forward
                    }),
                    &tree,
                );
            }
        }
        out.push(primitives_snapshot(&pane));
    }
    out
}

/// The same scenario, against the TERMINAL, by its own decisions.
///
/// This is the leg that was missing (ADR 0097, D1). The other two compare
/// the host against the primitives, and the primitives' harness is written
/// with the host's rules — its `Enter` was
/// `selected().filter(|e| e.kind == Dir)`, which is what the window does and
/// not what the terminal does — so that comparison could not fail by
/// construction.
///
/// Here the steps go through the TUI's decision functions: `enter_action`
/// for Enter and `nav_enter_target` underneath, which is where the terminal
/// decides that a `.zip` is navigated and a symlink is followed.
fn via_tui(steps: &[Step], parent_row: bool) -> Vec<Semantic> {
    use norte_tui::app::{App, Pane};

    let tree = arbol_de_prueba();
    let start = VPath::parse("mem:///casa").expect("vpath");
    let mut app = App::new(
        Pane::new(start.clone(), entries_of(&tree, &start)),
        Pane::new(start.clone(), Vec::new()),
    );
    app.set_parent_row(parent_row);
    app.set_focus(0);
    let mut out = vec![tui_snapshot(&app)];

    for step in steps {
        match step {
            Step::Cursor(delta) => {
                for _ in 0..delta.unsigned_abs() {
                    if *delta < 0 {
                        app.focused_mut().move_up(1);
                    } else {
                        app.focused_mut().move_down(1);
                    }
                }
            }
            Step::CursorTo(name) => {
                let rows = app.focused().entries().len();
                app.focused_mut().move_up(rows);
                for _ in 0..rows {
                    if tui_snapshot(&app).names.get(app.focused().cursor())
                        == Some(&(*name).to_owned())
                    {
                        break;
                    }
                    app.focused_mut().move_down(1);
                }
            }
            Step::Mark => app.focused_mut().toggle_mark(),
            Step::Enter => {
                // The TERMINAL's decision, not a copy of it.
                match norte_tui::gestures::enter_action(&app) {
                    norte_tui::gestures::EnterAction::Cd(dir) => cd_tui(&mut app, &dir, &tree),
                    norte_tui::gestures::EnterAction::Up(parent) => {
                        let child = app.focused().dir().clone();
                        app.focused_mut().set_pending_focus(child);
                        cd_tui(&mut app, &parent, &tree);
                    }
                    // Opening externally or viewing does not move the
                    // listing: the scenario observes the listing, so this is
                    // a no-op step.
                    _ => {}
                }
            }
            Step::Up => {
                let current = app.focused().dir().clone();
                let Some(parent) = current.parent() else {
                    out.push(tui_snapshot(&app));
                    continue;
                };
                app.focused_mut().set_pending_focus(current);
                cd_tui(&mut app, &parent, &tree);
            }
            Step::Back | Step::Forward => {
                let current = app.focused().dir().clone();
                let slot = app.panes.slot_of(app.focus());
                let target = {
                    let h = app.history.for_slot_mut(slot);
                    if matches!(step, Step::Back) {
                        h.step_back(current)
                    } else {
                        h.step_forward(current)
                    }
                };
                let Some(target) = target else {
                    out.push(tui_snapshot(&app));
                    continue;
                };
                let rows = entries_of(&tree, &target);
                // `begin_listing` is the terminal's REAL cd: it records the
                // old dir's cursor before replacing the listing.
                app.focused_mut().begin_listing(target, rows, false, None);
            }
        }
        out.push(tui_snapshot(&app));
    }
    out
}

/// A terminal `cd`: record the step, remember the cursor, list.
fn cd_tui(app: &mut norte_tui::app::App, target: &VPath, tree: &Falso) {
    let previous = app.focused().dir().clone();
    let slot = app.panes.slot_of(app.focus());
    if previous != *target {
        app.history.for_slot_mut(slot).record(previous);
    }
    let rows = entries_of(tree, target);
    app.focused_mut()
        .begin_listing(target.clone(), rows, false, None);
}

/// The same semantic snapshot, read from the terminal.
fn tui_snapshot(app: &norte_tui::app::App) -> Semantic {
    let pane = app.focused();
    Semantic {
        dir: norte_frontend::path_display(pane.dir()).0,
        cursor: pane.cursor(),
        marks: pane.marks_len(),
        names: pane
            .entries()
            .iter()
            .enumerate()
            .map(|(i, e)| {
                if pane.is_parent_row(i) {
                    return "..".to_owned();
                }
                norte_frontend::display_name(
                    e.path
                        .file_name()
                        .map_or(&[][..], norte_proto::Segment::as_bytes),
                )
                .0
            })
            .collect(),
    }
}

/// The same navigation ritual as the host: record the step unless the trail
/// is replaying, remember the cursor, and list.
fn navigate(
    pane: &mut PaneState,
    history: &mut History,
    target: &VPath,
    trail: Trail,
    tree: &Falso,
) {
    let previous = pane.dir().clone();
    if previous != *target && trail == Trail::Record {
        history.record(previous);
    }
    pane.remember_cursor();
    pane.set_listing(target.clone(), entries_of(tree, target));
}

fn entries_of(tree: &Falso, dir: &VPath) -> Vec<Entry> {
    tree.entradas_de(dir)
}

fn primitives_snapshot(pane: &PaneState) -> Semantic {
    Semantic {
        dir: norte_frontend::path_display(pane.dir()).0,
        cursor: pane.cursor(),
        marks: pane.marks_len(),
        names: pane
            .entries()
            .iter()
            .enumerate()
            .map(|(i, e)| {
                // The `..` row paints as `..` and not with the parent's
                // name — its own is the parent's path, whose `file_name` at
                // the root does not even exist. That is what both renderers
                // do, and this path has to paint the same or the comparison
                // measures the harness.
                if pane.is_parent_row(i) {
                    return "..".to_owned();
                }
                norte_frontend::display_name(
                    e.path
                        .file_name()
                        .map_or(&[][..], norte_proto::Segment::as_bytes),
                )
                .0
            })
            .collect(),
    }
}

/// The same scenario, against the host, through its actions and snapshots.
async fn via_host(steps: &[Step], parent_row: bool) -> Vec<Semantic> {
    let backend = Arc::new(arbol_de_prueba());
    let (host, first) = UiHost::start(UiHostOptions {
        backend,
        initial_dir: VPath::parse("mem:///casa").expect("vpath"),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        // Each scenario runs TWICE, with the `..` row off and on, and both
        // paths have to agree on both. Running it only with it off would
        // test the one state nobody starts in: out of the box the row is
        // there, and the cursor is born right on top of it.
        settings: {
            let mut cfg = norte_ui_host::ajustes_por_defecto();
            cfg.common.ui_parent_entry = Some(parent_row);
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
    .expect("starts");
    let mut sub = host.subscribe();
    let mut out = vec![host_snapshot(&first)];
    // The listing's LIVE generation. A row action carries it because
    // without it the key is an index, and this harness navigates between
    // steps: the previous screen's index would name a different file.
    let mut generation = listing_of(&first).generation;

    for step in steps {
        let (action, navigates) = match step {
            Step::Cursor(delta) => (
                UiAction::MoveCursor {
                    slot_id: 1,
                    delta: *delta,
                },
                false,
            ),
            Step::CursorTo(name) => {
                // As a single action: this surface moves the cursor by
                // DELTA, so the sought row becomes one. What is compared is
                // where it ends up, not how many keystrokes it cost.
                let current = out.last().expect("there is a snapshot");
                let target = current
                    .names
                    .iter()
                    .position(|n| n == name)
                    .unwrap_or(current.cursor);
                let delta =
                    i64::try_from(target).unwrap_or(0) - i64::try_from(current.cursor).unwrap_or(0);
                (UiAction::MoveCursor { slot_id: 1, delta }, false)
            }
            Step::Mark => {
                let current = out.last().expect("there is a snapshot");
                (
                    UiAction::ToggleMark {
                        slot_id: 1,
                        key: norte_ui_host::RowKey(u64::try_from(current.cursor).unwrap_or(0)),
                        generation,
                    },
                    false,
                )
            }
            Step::Enter => {
                let current = out.last().expect("there is a snapshot");
                (
                    UiAction::Activate {
                        slot_id: 1,
                        key: norte_ui_host::RowKey(u64::try_from(current.cursor).unwrap_or(0)),
                        generation,
                    },
                    true,
                )
            }
            Step::Up => (UiAction::Parent { slot_id: 1 }, true),
            Step::Back => (
                UiAction::History {
                    slot_id: 1,
                    back: true,
                },
                true,
            ),
            Step::Forward => (
                UiAction::History {
                    slot_id: 1,
                    back: false,
                },
                true,
            ),
        };
        let ack = host.dispatch(action).await.expect("host alive");
        // A navigation that cannot happen (root, exhausted trail, a row that
        // is not a directory) leaves the screen as it was: it is the SAME
        // outcome as in the primitives.
        // Without guessing whether a snapshot will arrive: it waits a moment
        // in case the screen moves on its own — a `cd` moves it — and, if it
        // does not move, it is requested.
        //
        // It used to be inferred from the ack (`navigates && Applied`), and
        // that was the harness encoding a rule of the product: `Activate` on
        // a file answers `Applied` because IT OPENS EXTERNALLY, so the
        // scenario would sit waiting for a listing that was never going to
        // exist. Inferring it is also exactly what this file must not do: if
        // it knew what navigates, it would not be measuring whether the two
        // surfaces agree on what navigates.
        let _ = (navigates, &ack);
        let snap = match optional_next_snapshot(&mut sub).await {
            Some(f) => f,
            None => request_snapshot(&host, &mut sub).await,
        };
        generation = listing_of(&snap).generation;
        out.push(host_snapshot(&snap));
    }
    out
}

/// A snapshot that arrives ON ITS OWN, or `None` if the screen did not move.
///
/// The deadline is the FAILURE budget: on the green path — a `cd` — the
/// snapshot is already waiting, and on the one where nothing moves this
/// whole deadline is spent once per step.
async fn optional_next_snapshot(sub: &mut UiSubscription) -> Option<ViewSnapshot> {
    for _ in 0..20 {
        let next = tokio::time::timeout(std::time::Duration::from_millis(100), sub.recv()).await;
        let Ok(received) = next else {
            return None;
        };
        if let Update::Message(m) = received.expect("the host is still alive")
            && let UiUpdate::Snapshot(s) = m.payload
        {
            return Some(*s);
        }
    }
    None
}

async fn wait_for_snapshot(sub: &mut UiSubscription) -> ViewSnapshot {
    for _ in 0..20 {
        let next = tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv())
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

async fn request_snapshot(host: &UiHost, sub: &mut UiSubscription) -> ViewSnapshot {
    host.dispatch(UiAction::Resync).await.expect("host alive");
    wait_for_snapshot(sub).await
}

/// A snapshot's listing.
fn listing_of(snap: &ViewSnapshot) -> &norte_ui_host::dto::BrowserSlotView {
    let SlotView::Browser(b) = snap
        .slots
        .iter()
        .find(|s| matches!(s, SlotView::Browser(_)))
        .expect("there is a listing")
    else {
        unreachable!("filtered above")
    };
    b
}

fn host_snapshot(snap: &ViewSnapshot) -> Semantic {
    let SlotView::Browser(b) = snap
        .slots
        .iter()
        .find(|s| matches!(s, SlotView::Browser(_)))
        .expect("there is a listing")
    else {
        unreachable!("filtered above")
    };
    Semantic {
        dir: b.path_display.clone(),
        cursor: b.cursor.map_or(0, |k| usize::try_from(k.0).unwrap_or(0)),
        marks: usize::try_from(b.marks).unwrap_or(0),
        names: b.rows.iter().map(|r| r.display_name.clone()).collect(),
    }
}

/// Runs a scenario through both paths and compares step by step.
async fn compare(name: &str, steps: &[Step]) {
    // Both configurations of the `..` row. On is the factory default and
    // what anyone opening norte sees; off is still run because it is a real
    // option and its listing has different indices.
    for parent_row in [false, true] {
        let label = if parent_row {
            "with `..` row"
        } else {
            "without `..` row"
        };
        let expected = via_primitives(steps, parent_row);
        // Boxed: the future that builds the whole host goes over
        // `clippy::large_futures`'s threshold as soon as the controller
        // gains one more field, and copying it on every test's stack
        // measures nothing.
        let got = Box::pin(via_host(steps, parent_row)).await;
        assert_eq!(
            expected.len(),
            got.len(),
            "[{name}, {label}] different number of steps observed"
        );
        for (i, (a, b)) in expected.iter().zip(got.iter()).enumerate() {
            assert_eq!(
                a, b,
                "[{name}, {label}] step {i}: the host and the primitives diverge"
            );
        }

        // And the comparison that actually catches something: the TWO
        // frontends, against each other. The ones above measure the host
        // against a harness written with the host's rules.
        let terminal = via_tui(steps, parent_row);
        assert_eq!(
            terminal.len(),
            got.len(),
            "[{name}, {label}] the terminal and the window observe a different number of steps"
        );
        for (i, (a, b)) in terminal.iter().zip(got.iter()).enumerate() {
            assert_eq!(
                a, b,
                "[{name}, {label}] step {i}: the TERMINAL and the WINDOW diverge"
            );
        }
    }
}

/// Listing, moving and marking.
#[tokio::test]
async fn listing_moving_marking() {
    compare(
        "list → move → mark",
        &[Step::Cursor(1), Step::Mark, Step::Cursor(1), Step::Mark],
    )
    .await;
}

/// The cursor stops at both ends the same way on both surfaces.
#[tokio::test]
async fn the_cursor_stops_the_same_way() {
    compare(
        "cursor to the ends",
        &[Step::Cursor(-5), Step::Cursor(99), Step::Cursor(1)],
    )
    .await;
}

/// Enter, go back, go forward and go up: the trail and the cursor's memory
/// behave the same.
#[tokio::test]
async fn enter_back_forward_up() {
    compare(
        "open dir → back → forward → up",
        &[Step::Enter, Step::Back, Step::Forward, Step::Up, Step::Back],
    )
    .await;
}

/// `Enter` on a COMPRESSED file enters it, on both surfaces.
///
/// Divergence number one in the inventory, and the one this harness could
/// not touch: its tree only had directories and files, so the `Enter` step
/// never ran into anything the two could answer differently about. The
/// terminal navigated to the `zip+…!/` and the window handed it to
/// `xdg-open`, and this comparison passed green underneath.
#[tokio::test]
async fn entering_a_compressed_file_is_the_same_on_both() {
    compare(
        "cursor to the zip → enter → back",
        &[Step::CursorTo("cosas.zip"), Step::Enter, Step::Back],
    )
    .await;
}

/// And on a SYMLINK, the same: it is still followed without resolving where
/// it points.
///
/// Whether the provider lists or fails is its own business; what is compared
/// is that both surfaces ask the SAME question. Here the link leads to a
/// directory that does get listed, which is the case where a disagreement
/// shows.
#[tokio::test]
async fn entering_a_symlink_is_the_same_on_both() {
    compare(
        "cursor to the link → enter → up",
        &[Step::CursorTo("atajo"), Step::Enter, Step::Up],
    )
    .await;
}

/// And on a plain FILE, `Enter` navigates on neither.
///
/// The other half of the contract: if `enter_target` became permissive, the
/// two tests above would stay green and this one would turn red.
#[tokio::test]
async fn entering_a_file_navigates_on_neither() {
    compare(
        "cursor to a file → enter",
        &[Step::CursorTo("notas.txt"), Step::Enter],
    )
    .await;
}

/// A scenario mixing marks and navigation: marks do NOT survive a new
/// listing, and that also has to match.
#[tokio::test]
async fn marks_do_not_survive_a_cd() {
    compare(
        "mark → enter → go back",
        &[Step::Mark, Step::Enter, Step::Back],
    )
    .await;
}

/// `[profile.start]` opens the same directory on both surfaces, and only the
/// first time on both (ADR 0098).
///
/// Outside the `Step` harness because it is not a navigation: it is each
/// frontend's STARTUP with the same configuration and no session. And it
/// has to be one comparison and not two separate tests, because the two
/// bugs the review found were exactly this shape — the terminal never
/// seeded and the window seeded too much — and each frontend looked green on
/// its own: both called the shared function correctly and called it in the
/// wrong place.
#[tokio::test]
async fn profile_start_opens_the_same_thing_on_both() {
    let start =
        std::collections::BTreeMap::from([(1, VPath::parse("mem:///casa/fotos").expect("vpath"))]);

    // The WINDOW: starts with the configuration and no session to read.
    let backend = Arc::new(arbol_de_prueba());
    let (host, first) = UiHost::start(UiHostOptions {
        backend,
        initial_dir: VPath::parse("mem:///casa").expect("vpath"),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: {
            let mut cfg = norte_ui_host::ajustes_por_defecto();
            cfg.common.profile_start = start.clone();
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
    .expect("starts");
    let window = listing_of(&first).path_display.clone();
    drop(host);

    // The TERMINAL: the same startup, by its own path.
    let tree = arbol_de_prueba();
    let start_dir = VPath::parse("mem:///casa").expect("vpath");
    let mut app = norte_tui::app::App::new(
        norte_tui::app::Pane::new(start_dir.clone(), entries_of(&tree, &start_dir)),
        norte_tui::app::Pane::new(start_dir, Vec::new()),
    );
    app.apply_session_value(
        norte_frontend::session::SCHEMA_VERSION,
        &norte_frontend::session::SessionBody::default().to_value(),
    );
    app.seed_profile_start(&start);
    let terminal = norte_frontend::path_display(
        app.panes
            .browser(norte_frontend::layout::SlotId(1))
            .expect("there is a listing")
            .dir(),
    )
    .0;

    assert!(
        window.ends_with("/casa/fotos"),
        "the window opens where the profile says: {window}"
    );
    assert_eq!(terminal, window, "and the terminal opens the same thing");

    // And both only the FIRST time: re-entering the profile does not pull
    // the reader out of where they were.
    app.adoptar_pane(
        norte_frontend::layout::SlotId(1),
        norte_tui::app::Pane::new(VPath::parse("mem:///casa/docs").expect("vpath"), Vec::new()),
        None,
        None,
    );
    assert!(
        app.seed_profile_start(&start).is_empty(),
        "seeding is a first-time thing, in the terminal too"
    );
}
