//! The splash screen, PAINTED.
//!
//! It exists because there was nothing: `src/splash.rs` tests the model
//! —that it goes up, that it comes down, which rows it carries— and
//! `ui/overlays.rs`'s painter did not show up in a single assert. Turning
//! the splash into a cover put nothing in red, which is exactly the sign
//! that nobody was checking what got painted.
//!
//! What is pinned here are the two shapes and the boundary between them:
//! `brief` —with no sections— fills the screen and does not draw a
//! dialog's chrome; `home` —with numbered rows— is still a box, because a
//! list that gets read and pressed needs the frame that bounds it.

use norte_frontend::splash::{Daemon, SplashRow, SplashSection, SplashView};
use norte_proto::VPath;
use norte_tui::app::{App, Pane};
use norte_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn vp(wire: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("valid wire")
}

fn app_with_splash(sections: Vec<SplashSection>) -> App {
    let dir = vp("mem:///home");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    app.splash = Some(SplashView {
        art: norte_frontend::splash::ART,
        version: "0.3.0-alpha.4".to_owned(),
        revision: "abc1234".to_owned(),
        daemon: Daemon::Embedded,
        sections,
    });
    app
}

fn one_section() -> Vec<SplashSection> {
    vec![SplashSection {
        title_key: "splash-popular",
        rows: vec![SplashRow {
            label: "home".to_owned(),
            detail: "12".to_owned(),
            command: "nav.goto".to_owned(),
            arg: Some("mem:///home".to_owned()),
        }],
    }]
}

fn screen(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("backend");
    terminal.draw(|f| ui::draw(f, app)).expect("draw");
    (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| terminal.backend().buffer()[(x, y)].symbol().to_owned())
        .collect()
}

/// With no sections it is a COVER: the logo, and no dialog chrome.
///
/// The dialog's title is the signal, not the border characters: in box
/// mode the splash paints OVER the listing, which has its own borders, so
/// looking for borders would not tell one shape from the other.
#[test]
fn with_no_sections_the_splash_screen_is_a_cover() {
    let app = app_with_splash(Vec::new());
    let seen = screen(&app, 80, 24);

    assert!(seen.contains("N O R T E"), "the logo is painted: {seen:?}");
    assert!(
        seen.contains("0.3.0-alpha.4"),
        "and below it says which build is running"
    );
    assert!(
        !seen.contains(&norte_i18n::t("splash-title")),
        "a cover does not carry the title of something that needs closing"
    );
}

/// With numbered rows it is still a BOX, with its title and its list.
#[test]
fn with_sections_the_splash_screen_is_still_a_box() {
    let app = app_with_splash(one_section());
    let seen = screen(&app, 80, 24);

    assert!(
        seen.contains(&norte_i18n::t("splash-title")),
        "the box announces itself: {seen:?}"
    );
    assert!(seen.contains("home"), "and shows the row");
    assert!(
        seen.contains('1'),
        "with its number, which is what opens it"
    );
}

/// The logo fits in a narrow terminal without breaking in half.
///
/// Eighty columns is the width it was drawn at; at 40 the art measures 35
/// and still fits. What this test protects is the day someone makes the
/// logo wider without checking: it would show up cut off, and on a cover
/// that is the only thing there is.
#[test]
fn the_logo_fits_in_a_narrow_terminal() {
    let app = app_with_splash(Vec::new());
    let seen = screen(&app, 40, 20);
    assert!(seen.contains("N O R T E"), "fits in 40 columns: {seen:?}");
}
