//! The shared startup screen (spec 2026-09-15, phase 2).
//!
//! What the splash says — what build is running, against which daemon, and
//! where a number can take you — is the same question in the terminal and
//! in the window, so it is answered once. Each frontend supplies the
//! pixels.
//!
//! The sections come from a REGISTRY and not a hand-written list: a new
//! source (recents, favorites, profiles… and tomorrow whatever a plugin
//! contributes) is added by implementing [`SplashSource`], without
//! touching whoever paints it. A source with nothing to say takes up NO
//! room.

/// What this frontend is talking to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Daemon {
    /// The core runs inside the process.
    Embedded,
    /// There is a daemon and it is connected.
    Connected,
    /// It is connecting, or reconnecting.
    Connecting,
}

impl Daemon {
    /// The Fluent key that says it.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Embedded => "splash-daemon-embedded",
            Self::Connected => "splash-daemon-connected",
            Self::Connecting => "splash-daemon-connecting",
        }
    }
}

/// A row of the splash: what is read, and the command that runs if it is
/// chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplashRow {
    /// What is read, already sanitized by whoever builds it.
    pub label: String,
    /// The detail on the right (a path, a visit count). Can be empty.
    pub detail: String,
    /// The catalogue command the row runs.
    pub command: String,
    /// Its argument, if it carries one (a directory, a profile name).
    pub arg: Option<String>,
}

/// A group of rows with its title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplashSection {
    /// Fluent key of the title: the splash carries no translated prose
    /// inline.
    pub title_key: &'static str,
    /// The rows, in the order they are painted.
    pub rows: Vec<SplashRow>,
}

/// Everything the splash shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplashView {
    /// The art, one row per line ([`ART`]).
    pub art: &'static [&'static str],
    /// The binary's version.
    pub version: String,
    /// The git revision it was built from.
    pub revision: String,
    /// Which core it talks to.
    pub daemon: Daemon,
    /// The sections, already filtered: none of them come empty.
    pub sections: Vec<SplashSection>,
}

/// Where a splash section comes from.
///
/// `None` = this source has nothing to say today (no favorites, no
/// profiles), and then it takes up no room on screen.
pub trait SplashSource {
    /// This source's section, if it has rows.
    fn section(&self) -> Option<SplashSection>;
}

/// The sections of the given sources, in their order, skipping the empty
/// ones.
///
/// ```
/// use norte_frontend::splash::{SplashRow, SplashSection, SplashSource, sections};
///
/// struct Empty;
/// impl SplashSource for Empty {
///     fn section(&self) -> Option<SplashSection> { None }
/// }
/// struct One;
/// impl SplashSource for One {
///     fn section(&self) -> Option<SplashSection> {
///         Some(SplashSection {
///             title_key: "splash-recent",
///             rows: vec![SplashRow {
///                 label: "home".to_owned(),
///                 detail: String::new(),
///                 command: "nav.enter".to_owned(),
///                 arg: None,
///             }],
///         })
///     }
/// }
/// let sources: [&dyn SplashSource; 3] = [&Empty, &One, &Empty];
/// let s = sections(&sources);
/// assert_eq!(s.len(), 1, "a source with no rows takes up no room");
/// assert_eq!(s[0].title_key, "splash-recent");
/// ```
#[must_use]
pub fn sections(sources: &[&dyn SplashSource]) -> Vec<SplashSection> {
    sources
        .iter()
        .filter_map(|f| f.section())
        .filter(|s| !s.rows.is_empty())
        .collect()
}

/// How long the `brief` splash covers the screen AT MOST, in milliseconds.
///
/// Shared because it is part of what the screen PROMISES: "it shows, and
/// it goes away on its own". Two different deadlines would be two
/// different startups, and whichever took longer would read as that
/// surface being slower.
///
/// It is not one of the timers ADR 0006 forbids — that one is about
/// resolving KEYS — here no key waits on the clock, because any of them
/// dismisses it first.
pub const BRIEF_MS: i64 = norte_config::load::UiChrome::DEFAULT_SPLASH_MS as i64;

/// How many splash rows can be picked by number.
///
/// Nine, not ten: `0` is not the tenth of anything, and a list that starts
/// at `1` and ends at `0` has to be read twice.
pub const NUMBERED: usize = 9;

/// The numbered rows, in the order they are painted: `(number, row)`.
///
/// Numbers across the sections, not within each one: what the reader sees
/// is a list with numbers, and two rows with the same number would be two
/// keys that do different things.
///
/// ```
/// use norte_frontend::splash::{SplashRow, SplashSection, numbered};
/// let row = |l: &str| SplashRow {
///     label: l.to_owned(),
///     detail: String::new(),
///     command: "nav.enter".to_owned(),
///     arg: None,
/// };
/// let sections = vec![
///     SplashSection { title_key: "a", rows: vec![row("one"), row("two")] },
///     SplashSection { title_key: "b", rows: vec![row("three")] },
/// ];
/// let n = numbered(&sections);
/// assert_eq!(n[2].0, 3, "numbering crosses the sections");
/// assert_eq!(n[2].1.label, "three");
/// ```
#[must_use]
pub fn numbered(sections: &[SplashSection]) -> Vec<(u8, &SplashRow)> {
    sections
        .iter()
        .flat_map(|s| s.rows.iter())
        .take(NUMBERED)
        .enumerate()
        .map(|(i, row)| (u8::try_from(i + 1).unwrap_or(u8::MAX), row))
        .collect()
}

/// The compass: the splash's art, one row per line.
///
/// All rows measure the SAME in cells — a test pins it — because both
/// surfaces center it, and a row wider than the rest comes out crooked as
/// soon as centering is done per line.
pub const ART: &[&str] = &[
    "▛▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▜",
    "▌                                 ▐",
    "▌  ░▒▓█  N O R T E  █▓▒░          ▐",
    "▌                                 ▐",
    "▙▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▟",
];

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(&'static str, usize);

    impl SplashSource for Fixed {
        fn section(&self) -> Option<SplashSection> {
            Some(SplashSection {
                title_key: self.0,
                rows: (0..self.1)
                    .map(|i| SplashRow {
                        label: format!("row {i}"),
                        detail: String::new(),
                        command: "nav.enter".to_owned(),
                        arg: None,
                    })
                    .collect(),
            })
        }
    }

    #[test]
    fn the_registry_keeps_order_and_skips_empty_ones() {
        let (a, empty, b) = (Fixed("a", 2), Fixed("empty", 0), Fixed("b", 1));
        let sources: [&dyn SplashSource; 3] = [&a, &empty, &b];
        let s = sections(&sources);
        assert_eq!(
            s.iter().map(|x| x.title_key).collect::<Vec<_>>(),
            ["a", "b"]
        );
    }

    /// The art is CENTERED, so a row of a different width comes out
    /// crooked.
    #[test]
    fn the_art_measures_the_same_in_all_its_rows() {
        let widths: Vec<usize> = ART.iter().map(|l| crate::display::cells(l)).collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "rows of different widths: {widths:?}"
        );
    }

    /// Nine at most: the tenth row is painted, but with no number to call
    /// it — a `0` after the `9` is read twice.
    #[test]
    fn numbering_stops_at_nine() {
        let many = Fixed("many", 12);
        let sources: [&dyn SplashSource; 1] = [&many];
        let s = sections(&sources);
        let n = numbered(&s);
        assert_eq!(n.len(), NUMBERED);
        assert_eq!(n.last().expect("there are rows").0, 9);
    }
}
