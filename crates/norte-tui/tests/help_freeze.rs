//! Opening help freezes the context facts: the overlay paints what was
//! true when it opened, not whatever is true while it stays open.

use norte_help::ChordResolver as _;
use norte_proto::VPath;
use norte_tui::app::{App, Pane};
use norte_tui::overlays::open_contextual_help;

/// Opening help FREEZES the context facts (H3d).
///
/// The rest of the chain —the shared table, the resolver, the reason
/// painter— has its own tests and would stay GREEN with this call
/// removed: the overlay would paint against the permissive startup
/// resolver and no row would ever be dimmed. This test is the only one
/// that looks at this link.
///
/// Opened from inside a zip (`READ_ONLY` via the scheme, ADR 0018) with
/// both panes there: with no writable destination, `pane.copy` cannot run.
#[test]
fn opening_help_freezes_the_context_facts() {
    let inside = VPath::parse("zip+file:///a.zip/!").expect("test wire");
    let mut app = App::new(
        Pane::new(inside.clone(), Vec::new()),
        Pane::new(inside, Vec::new()),
    );
    assert!(
        app.help_chords.availability("pane.copy").is_available(),
        "before opening, the startup resolver dims nothing"
    );

    open_contextual_help(&mut app, norte_help::Lang::En, &[], None);

    assert!(app.help.is_some(), "the overlay opened");
    assert_eq!(
        app.help_chords.availability("pane.copy").reason(),
        Some(norte_help::Reason::ReadOnlyBackend),
        "help must know it is inside an archive"
    );
}
