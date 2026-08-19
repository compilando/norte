//! The which-key panel end to end (K3a): it appears while a chord sequence is
//! pending, it lists what can follow — unavailable keys included — and it goes
//! away the moment something else owns the keyboard or the reader abandons the
//! sequence by any route other than a key of that sequence.
//!
//! Driven through `App` and the resolver, never through a terminal: the run
//! loop's arms are one call each to `show_pending`/`clear_pending`, and what
//! matters here is that the panel and the surface that owns the keys cannot
//! disagree.

use norte_frontend::keymap::{Effective, Resolution, Resolver, Screen, parse_chord, parse_keymap};
use norte_i18n::Lang;
use norte_proto::VPath;
use norte_tui::app::{App, Modal, Pane};
use norte_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn app() -> App {
    let _ = norte_i18n::force(Lang::En);
    let d = VPath::parse("file:///x").expect("wire");
    App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
}

/// `g` opens a branch with an available key, an unavailable one (`pane.pack`
/// is `Planned`, #132) and a deeper sequence.
fn resolver() -> Resolver {
    let src = r#"
[pane]
keymap = [
    { on = ["g", "g"], run = "cursor.top" },
    { on = ["g", "p"], run = "pane.pack" },
]
"#;
    let preset = parse_keymap(src).expect("fixture parses");
    let eff = Effective::build_for(&preset, &[], &["cursor.top"], Screen::Browse)
        .expect("fixture builds");
    Resolver::new(eff)
}

fn pending(app: &mut App, r: &mut Resolver) {
    assert!(matches!(
        r.push(parse_chord("g").expect("chord")),
        Resolution::Pending(1)
    ));
    app.show_pending(r, Lang::En);
    assert!(app.which_key.is_some(), "the panel is open");
}

fn painted(app: &App) -> String {
    let mut t = Terminal::new(TestBackend::new(70, 18)).expect("term");
    t.draw(|f| ui::draw(f, app)).expect("draw");
    t.backend().to_string()
}

/// The whole frame shows it: title, the available row, and the unavailable one
/// with the reason it does nothing. It is painted OVER the panes, above the
/// status bar, which keeps its own `[g …]` segment.
#[test]
fn the_panel_reaches_the_frame_with_its_unavailable_row() {
    let (mut app, mut r) = (app(), resolver());
    pending(&mut app, &mut r);
    let text = painted(&app);
    assert!(text.contains("go to top"), "the available row: {text}");
    // The unavailable row used to be a `Planned` command with its issue
    // number. #132 built the last of those, so the row that is dimmed now is a
    // command this build does not implement — same row, same explanation.
    assert!(
        text.contains("pack into an archive"),
        "the unavailable row: {text}"
    );
    assert!(
        text.contains(&norte_i18n::t("keymap-short-not-here")),
        "and the reason it does nothing: {text}"
    );
    assert!(text.contains("[g …]"), "the bar keeps its segment: {text}");
}

/// A modal can open with NO key pressed — a policy approval off the bus, a
/// collision when a copy lands — and from then on the keys go to the dialog.
/// A panel still offering `g` beside it would be the pixels lying about who is
/// in charge, which is exactly what the modal's own draw order exists to stop.
#[test]
fn a_modal_hides_it_although_the_sequence_is_still_pending() {
    let (mut app, mut r) = (app(), resolver());
    pending(&mut app, &mut r);
    app.modal = Some(Modal::ConfirmQuit);
    let text = painted(&app);
    assert!(
        !text.contains("go to top"),
        "the panel outlived the keyboard: {text}"
    );
    assert!(text.contains("Quit norte?"), "the modal is up: {text}");

    // The sequence itself is NOT cancelled — the modal did not touch the pane
    // resolver, exactly as it does not touch the bar's segment — so closing
    // the dialog brings the panel back.
    app.modal = None;
    assert!(painted(&app).contains("go to top"), "back after the dialog");
}

/// A key SWALLOWED before the resolver (the `Esc` that cancels a Lua command
/// in flight) and a mouse gesture both end the sequence: the resolver is reset
/// with the panel, so the next key cannot complete a sequence the reader had
/// already abandoned.
#[test]
fn abandoning_the_sequence_resets_the_resolver_too() {
    let (mut app, mut r) = (app(), resolver());
    pending(&mut app, &mut r);
    app.abandon_pending(&mut r);
    assert!(app.which_key.is_none(), "the panel went with it");
    assert!(app.pending.is_empty(), "and so did the bar segment");
    assert!(r.pending().is_empty(), "and the resolver is not armed");
    // The proof that matters: `g` is a fresh first chord again, not the
    // second half of the sequence that was abandoned.
    assert!(matches!(
        r.push(parse_chord("g").expect("chord")),
        Resolution::Pending(1)
    ));
}
