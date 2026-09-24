//! The startup screen in the TUI (spec 2026-09-15, phase 2): when it goes up,
//! with what rows, and what a key does while it is up.
//!
//! The model is the shared one (`norte_frontend::splash`); what is here is
//! what only the terminal knows: where this process's sources come from, and
//! that the `brief`'s deadline is measured against the paint clock.

use norte_frontend::splash::{Daemon, SplashRow, SplashSection, SplashSource, SplashView};

use crate::app::App;
use crate::config;

/// Should the splash go up?
///
/// `off`, `--no-splash`, and `NORTE_NO_SPLASH` turn it off; so does `--pick`,
/// because there the output is for another program. The WIZARD wins: if both
/// wanted to show, the one that asks something wins, and that startup's
/// splash is superfluous.
#[must_use]
pub fn should_open(
    mode: norte_config::load::SplashMode,
    no_splash: bool,
    pick: bool,
    wizard: bool,
) -> bool {
    use norte_config::load::SplashMode;
    if pick || wizard || no_splash || std::env::var_os("NORTE_NO_SPLASH").is_some() {
        return false;
    }
    mode != SplashMode::Off
}

/// This process's sections: where you usually go, and what you have saved.
///
/// `home` shows them; `brief` does not, because a cover that dismisses itself
/// is no place to choose anything.
struct PopularSource<'a>(&'a norte_frontend::history::Popular);

impl SplashSource for PopularSource<'_> {
    fn section(&self) -> Option<SplashSection> {
        let rows: Vec<SplashRow> = self
            .0
            .ranked()
            .into_iter()
            .take(5)
            .map(|e| {
                let (text, _) = norte_frontend::display::path_display(&e.path);
                SplashRow {
                    label: text,
                    detail: e.visits.to_string(),
                    command: "nav.enter".to_owned(),
                    arg: Some(e.path.to_wire()),
                }
            })
            .collect();
        (!rows.is_empty()).then_some(SplashSection {
            title_key: "splash-popular",
            rows,
        })
    }
}

struct FavoritesSource<'a>(&'a [norte_config::HotlistItem]);

impl SplashSource for FavoritesSource<'_> {
    fn section(&self) -> Option<SplashSection> {
        let rows: Vec<SplashRow> = self
            .0
            .iter()
            .take(5)
            .filter_map(|h| {
                let target = h.target.as_ref().ok()?;
                // The name was written by a person into a file: it is masked
                // like any other third-party text.
                let (name, _) = norte_frontend::display_name(h.name.as_bytes());
                let (path, _) = norte_frontend::display::path_display(target);
                Some(SplashRow {
                    label: name,
                    detail: path,
                    command: "nav.enter".to_owned(),
                    arg: Some(target.to_wire()),
                })
            })
            .collect();
        (!rows.is_empty()).then_some(SplashSection {
            title_key: "splash-bookmarks",
            rows,
        })
    }
}

/// Puts up the splash, with the rows matching the mode.
///
/// `brief` goes up WITHOUT sections on purpose: it dismisses itself, so a
/// list of places there would be an offer withdrawn before it could be
/// accepted.
pub fn open(app: &mut App, mode: norte_config::load::SplashMode, cfg: &config::LoadedConfig) {
    use norte_config::load::SplashMode;
    let sections = if mode == SplashMode::Home {
        let popular = PopularSource(&app.popular);
        let favorites = FavoritesSource(&cfg.common.hotlist);
        let sources: [&dyn SplashSource; 2] = [&popular, &favorites];
        norte_frontend::splash::sections(&sources)
    } else {
        Vec::new()
    };
    // The line `--version` already prints: version AND git revision, written
    // once. Two ways of saying which build is running end up saying different
    // things the day one of them falls behind.
    let version = norte_frontend::version::VERSION.to_owned();
    let revision = norte_frontend::version::VERSION_LINE
        .split_once(' ')
        .map_or_else(String::new, |(_, rest)| rest.to_owned());
    app.splash = Some(SplashView {
        art: norte_frontend::splash::ART,
        version,
        revision,
        // What this process has in front of it: the embedded core or a
        // daemon. Decided outside and arrives already resolved, so as not to
        // ask the config something only the backend knows.
        daemon: if app.backend_journalled {
            Daemon::Connected
        } else {
            Daemon::Embedded
        },
        sections,
    });
    // The deadline comes from `[ui] splash_ms`, not a constant: a cover that
    // gives no time to be read is only in the way, and how much counts as
    // "time" depends on who is looking.
    app.splash_until_ms = (mode == SplashMode::Brief)
        .then(|| app.now_ms() + i64::from(cfg.common.ui_chrome.splash_ms()));
}

/// A key with the splash up: dismisses it, and with `home` a number runs its
/// row.
///
/// Returns the `(command, argument)` that must be dispatched, if the number
/// named a row. Any other key only dismisses the layer: it does not swallow
/// anything else, because a cover that eats the first useful keystroke reads
/// as norte not responding.
pub fn on_key(app: &mut App, code: crossterm::event::KeyCode) -> Option<(String, Option<String>)> {
    use crossterm::event::KeyCode;
    let chosen = match (code, app.splash.as_ref()) {
        (KeyCode::Char(c @ '1'..='9'), Some(view)) => {
            let n = c.to_digit(10).unwrap_or(0) as usize;
            norte_frontend::splash::numbered(&view.sections)
                .into_iter()
                .find(|(i, _)| usize::from(*i) == n)
                .map(|(_, row)| (row.command.clone(), row.arg.clone()))
        }
        _ => None,
    };
    app.splash = None;
    app.splash_until_ms = None;
    chosen
}

/// The `brief`'s deadline is over (or there never was a splash): dismisses
/// it.
///
/// Called by the loop after painting, with the same clock that paints: a
/// deadline measured with another clock is a deadline the tests cannot pin.
pub fn tick(app: &mut App) {
    let Some(until) = app.splash_until_ms else {
        return;
    };
    if app.now_ms() >= until {
        app.splash = None;
        app.splash_until_ms = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_config::load::SplashMode;

    /// The gate: `off` puts up nothing, the wizard wins, and `--no-splash`
    /// and `--pick` turn it off no matter what the config says.
    #[test]
    fn the_splash_gate_yields_to_the_wizard_and_to_the_flags() {
        assert!(should_open(SplashMode::Brief, false, false, false));
        assert!(should_open(SplashMode::Home, false, false, false));
        assert!(!should_open(SplashMode::Off, false, false, false));
        assert!(
            !should_open(SplashMode::Home, false, false, true),
            "the wizard asks something: that startup's splash is superfluous"
        );
        assert!(
            !should_open(SplashMode::Brief, true, false, false),
            "--no-splash"
        );
        assert!(
            !should_open(SplashMode::Brief, false, true, false),
            "--pick: the output is for another program"
        );
    }
}
