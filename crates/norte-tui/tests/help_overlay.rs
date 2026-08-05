//! The help overlay's public seams (H3b).
//!
//! The KEY ROUTING lives in `main.rs` (`on_help_key`, next to the other
//! `on_*_key` handlers) and is tested there, alongside the run loop it serves.
//! What is testable from outside the binary is the invariant the hot-reload
//! rebuild exists for: the corpus is rendered through
//! [`norte_tui::help::TuiChords`], and a rebind that does not reach it leaves a
//! page teaching a key the user no longer has.

use norte_help::{ChordResolver, CommandText, render_command};
use norte_tui::help::TuiChords;
use norte_tui::keymap::{COMMANDS, DIALOG_COMMANDS, Effective, Screen, parse_keymap, presets};

/// Every command the TUI knows: the `dialog` effective merges `[global]` too,
/// so `DIALOG_COMMANDS` alone is not a sufficient vocabulary for it.
fn all_commands() -> Vec<&'static str> {
    COMMANDS
        .iter()
        .copied()
        .chain(DIALOG_COMMANDS.iter().copied())
        .collect()
}

/// The orthodox preset plus `layers`, resolved into a `TuiChords` exactly as
/// `main.rs` does at startup and on every hot reload.
fn chords(layers: &[norte_tui::keymap::KeymapFile]) -> TuiChords {
    let (_, preset) = presets()
        .into_iter()
        .find(|(n, _)| *n == "orthodox")
        .expect("preset orthodox");
    let known = all_commands();
    let build = |screen| {
        Effective::build_for(&preset, layers, &known, screen)
            .unwrap_or_else(|e| panic!("efectivo {screen:?}: {e}"))
    };
    TuiChords::new(
        &build(Screen::Browse),
        &build(Screen::Viewer),
        &build(Screen::Dialog),
        norte_i18n::Lang::En,
    )
}

/// A rebind REACHES the page. This is the whole reason `main.rs` rebuilds
/// `App::help_chords` next to `help_lines` in the hot-reload arm: the prose in
/// the corpus carries `{{cmd:pane.copy}}` marks and never a literal key, so
/// what the reader is taught is whatever this resolver answers. A rebuild that
/// reached the generated keyboard page but not this one would leave every
/// topic in the corpus teaching the OLD key — and a help page that teaches a
/// key the user does not have is worse than no help page.
#[test]
fn a_rebind_reaches_the_rendered_page() {
    // Baseline: the preset's own key.
    assert_eq!(
        render_command("pane.copy", &chords(&[])),
        CommandText::Chord("f5".to_owned()),
        "el preset orthodox ata `pane.copy` a f5"
    );

    // A user layer that prepends its own binding — `prepend` is what wins, the
    // same precedence `Effective::bindings` reports and the run loop obeys.
    let layer =
        parse_keymap("[pane]\nprepend_keymap = [{ on = [\"ctrl+alt+k\"], run = \"pane.copy\" }]\n")
            .expect("la capa parsea");
    let rebound = chords(std::slice::from_ref(&layer));
    assert_eq!(
        render_command("pane.copy", &rebound),
        CommandText::Chord("ctrl+alt+k".to_owned()),
        "la página tiene que enseñar la tecla NUEVA, no la del preset"
    );
    assert_eq!(
        rebound.chord("pane.copy").as_deref(),
        Some("ctrl+alt+k"),
        "y el resolver mismo, que es de donde sale"
    );

    // Un comando sin tecla se NOMBRA en vez de inventarse una — el otro
    // extremo del contrato, para que el test de arriba no pase por casualidad
    // con un resolver que responda cualquier cosa.
    assert_eq!(rebound.chord("no.such.command"), None);
}
