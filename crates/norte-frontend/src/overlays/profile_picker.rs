//! The PROFILES picker: which rows there are and what is said about each.
//!
//! Sibling of [`crate::layout_picker`] on purpose, and with its same
//! discipline: it lives here and not in a frontend because of rule 7, the
//! rows arrive ALREADY READ because reading as the cursor passes over would
//! be I/O in the event loop (#244), and a row that cannot be used is SHOWN
//! with its reason instead of disappearing — hiding a directory the reader
//! created is worse than showing it broken.
//!
//! What this picker adds over that one are two warnings the spec asks for by
//! name (`docs/superpowers/specs/2026-08-26-config-profiles-design.md`): a
//! profile whose name is not UTF-8 cannot carry state (D4), and a name that
//! matches a layout's or a keyboard preset's is a trap if it goes unsaid.

use std::ffi::{OsStr, OsString};

/// A profile from disk, already read.
///
/// `title` and `problem` are resolved by whoever has the disk in front of
/// them: this crate does not open directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserProfile {
    /// The directory's name, with its raw bytes (rule 1, D4).
    pub name: OsString,
    /// Its `[profile] title`, if it declares one.
    pub title: Option<String>,
    /// Why its `norte.toml` could not be read, when it could not.
    pub problem: Option<String>,
}

/// What else in norte is named the same as a profile.
///
/// It is WARNED about for the same reason the layout picker warns about it:
/// they are different settings that share a name, and without the line the
/// coincidence is a trap instead of a convenience. Choosing the `far` profile
/// does not bind a single key of the `far` preset.
///
/// An enum and not two `bool`s because it is ONE question —"does this name
/// mean something else somewhere?"— and whoever paints it has to say which,
/// not two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NameClash {
    /// It is only a profile.
    #[default]
    None,
    /// There is also a built-in layout by that name.
    Layout,
    /// There is also a keyboard preset by that name.
    Keymap,
    /// Both things.
    Both,
}

/// One row of the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The name it activates under. The profile's IDENTITY.
    ///
    /// [`OsString`] and not `String`: it is a DIRECTORY name and ends up as
    /// `profiles/<name>/`, so passing it through text changes which one gets
    /// opened (#245, #246).
    pub name: OsString,
    /// Its `[profile] title`, to show next to the name. Never INSTEAD OF the
    /// name: two profiles can share a title and still be two.
    pub title: Option<String>,
    /// Whether it is the currently active profile.
    ///
    /// Without this the picker is a list of names where you cannot tell
    /// where you are.
    pub active: bool,
    /// What OTHER thing is named the same as this profile.
    pub clash: NameClash,
    /// Whether this profile can save where you left each panel.
    ///
    /// `false` when its name is not UTF-8: the key for saved layouts is a
    /// JSON object, so a name like that is good for configuration and cannot
    /// carry state or be sticky (D4). It is said BEFORE choosing it, not
    /// after losing it.
    pub carries_state: bool,
    /// Why this row cannot be loaded, when it cannot.
    pub problem: Option<String>,
}

/// The profiles picker.
#[derive(Debug)]
pub struct ProfilePicker {
    rows: Vec<Row>,
    cursor: usize,
}

/// The profile that follows (or precedes) the active one, wrapping at the
/// end.
///
/// `None` when there is nowhere to go: no profiles, or only the one that is
/// already active — cycling over a single one is a change that changes
/// nothing, and running it through the whole sequence would tear down and
/// reload the screen to leave it the same.
///
/// With no active profile, `next` is the first one and `prev` the last:
/// entering from either end is what whoever has not chosen one yet expects.
///
/// Shared on purpose: `profile.next` has to mean the same thing in the
/// window and in the terminal, and two copies of a modulo cycle are two
/// different orders waiting to diverge (ADR 0077).
#[must_use]
pub fn next_profile(
    profiles: &[UserProfile],
    active: Option<&OsStr>,
    forward: bool,
) -> Option<OsString> {
    if profiles.is_empty() {
        return None;
    }
    let Some(active) = active else {
        let i = if forward { 0 } else { profiles.len() - 1 };
        return Some(profiles[i].name.clone());
    };
    let current = profiles.iter().position(|p| p.name == active)?;
    if profiles.len() == 1 {
        return None;
    }
    let n = profiles.len();
    let i = if forward {
        (current + 1) % n
    } else {
        (current + n - 1) % n
    };
    Some(profiles[i].name.clone())
}

impl ProfilePicker {
    /// Opens the picker with the profiles it is given, already read, and the
    /// name of the one that is active.
    ///
    /// There are no "built-in" profiles: unlike layouts, a profile is always
    /// a directory the reader created. An empty list is an empty list, and
    /// whoever paints it says so.
    #[must_use]
    pub fn open(profiles: Vec<UserProfile>, active: Option<&OsStr>) -> Self {
        let rows = profiles
            .into_iter()
            .map(|p| {
                let text = p.name.to_str();
                let as_layout = text.is_some_and(|n| crate::layout::presets::NAMES.contains(&n));
                let as_keymap = text.is_some_and(|n| crate::keymap::presets::NAMES.contains(&n));
                Row {
                    active: active == Some(p.name.as_os_str()),
                    clash: match (as_layout, as_keymap) {
                        (true, true) => NameClash::Both,
                        (true, false) => NameClash::Layout,
                        (false, true) => NameClash::Keymap,
                        (false, false) => NameClash::None,
                    },
                    carries_state: text.is_some(),
                    name: p.name,
                    title: p.title,
                    problem: p.problem,
                }
            })
            .collect();
        Self { rows, cursor: 0 }
    }

    /// The rows, in order.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Where the cursor is, clamped to the rows there are.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor.min(self.rows.len().saturating_sub(1))
    }

    /// Moves up.
    pub const fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Moves down.
    pub fn down(&mut self) {
        self.cursor = (self.cursor + 1).min(self.rows.len().saturating_sub(1));
    }

    /// The name of the highlighted row.
    #[must_use]
    pub fn chosen(&self) -> Option<&OsStr> {
        self.rows.get(self.cursor()).map(|r| r.name.as_os_str())
    }

    /// The whole highlighted row.
    #[must_use]
    pub fn current(&self) -> Option<&Row> {
        self.rows.get(self.cursor())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str) -> UserProfile {
        UserProfile {
            name: OsString::from(name),
            title: None,
            problem: None,
        }
    }

    fn profiles(names: &[&str]) -> Vec<UserProfile> {
        names.iter().map(|n| profile(n)).collect()
    }

    #[test]
    fn it_wraps_at_the_end_in_both_directions() {
        let p = profiles(&["a", "b", "c"]);
        assert_eq!(
            next_profile(&p, Some(OsStr::new("c")), true).as_deref(),
            Some(OsStr::new("a")),
            "from the last one to the first"
        );
        assert_eq!(
            next_profile(&p, Some(OsStr::new("a")), false).as_deref(),
            Some(OsStr::new("c")),
            "and from the first one to the last"
        );
    }

    /// Cycling over a SINGLE profile is not a change: running it through the
    /// whole sequence would tear down and reload the screen to leave it the
    /// same.
    #[test]
    fn with_a_single_active_profile_there_is_nowhere_to_go() {
        let p = profiles(&["a"]);
        assert_eq!(next_profile(&p, Some(OsStr::new("a")), true), None);
        assert_eq!(next_profile(&p, Some(OsStr::new("a")), false), None);
    }

    /// With no active profile, entry is from whichever end matches the
    /// direction.
    #[test]
    fn with_none_active_it_enters_from_one_end() {
        let p = profiles(&["a", "b", "c"]);
        assert_eq!(
            next_profile(&p, None, true).as_deref(),
            Some(OsStr::new("a"))
        );
        assert_eq!(
            next_profile(&p, None, false).as_deref(),
            Some(OsStr::new("c"))
        );
    }

    /// An active one that is no longer in the list —deleted while the
    /// program was open— does not choose blindly: cycling from a place that
    /// does not exist has no good answer, and jumping to the first one would
    /// move the reader to a profile they did not ask for.
    #[test]
    fn an_active_one_that_no_longer_exists_does_not_choose_blindly() {
        let p = profiles(&["a", "b"]);
        assert_eq!(next_profile(&p, Some(OsStr::new("ghost")), true), None);
    }

    #[test]
    fn with_no_profiles_there_is_nothing() {
        assert_eq!(next_profile(&[], None, true), None);
    }

    /// The ACTIVE profile's row is marked. Without that, the picker is a
    /// list of names where you cannot tell where you are.
    #[test]
    fn the_active_one_is_marked() {
        let p = ProfilePicker::open(
            vec![profile("work"), profile("photos")],
            Some(OsStr::new("photos")),
        );
        assert!(!p.rows()[0].active);
        assert!(p.rows()[1].active, "photos is the active one");
    }

    /// With no active profile none is marked: "none" is a legitimate state
    /// and does not disguise itself as the first row.
    #[test]
    fn with_none_active_none_is_marked() {
        let p = ProfilePicker::open(vec![profile("work")], None);
        assert!(p.rows().iter().all(|r| !r.active));
    }

    /// D4: a name that is not UTF-8 is good for configuration and CANNOT
    /// carry state, not even sticky state. The row says so BEFORE it is
    /// chosen, not after it is lost. And the bytes travel intact.
    #[test]
    #[cfg(unix)]
    fn a_non_utf8_name_is_listed_and_warns_it_carries_no_state() {
        use std::os::unix::ffi::OsStringExt;

        let hostile = OsString::from_vec(vec![b'w', 0xFF, b'k']);
        let p = ProfilePicker::open(
            vec![UserProfile {
                name: hostile.clone(),
                title: None,
                problem: None,
            }],
            None,
        );
        let row = &p.rows()[0];
        assert_eq!(row.name, hostile, "the bytes stay intact");
        assert!(!row.carries_state);
        assert_eq!(p.chosen(), Some(hostile.as_os_str()));
    }

    /// A profile that does not parse IS LISTED, with its reason: hiding a
    /// directory the reader created is worse than showing it broken, and
    /// that is what the layout picker does with an unreadable layout.
    #[test]
    fn a_broken_profile_is_listed_with_its_reason() {
        let p = ProfilePicker::open(
            vec![UserProfile {
                name: OsString::from("work"),
                title: None,
                problem: Some("line 3: unknown field `them`".to_owned()),
            }],
            None,
        );
        assert_eq!(p.rows().len(), 1, "the row does not disappear");
        assert!(p.rows()[0].problem.is_some());
    }

    /// Sharing a name with a layout or a keyboard preset is WARNED about:
    /// they are three different settings, and the coincidence is a trap if
    /// it goes unsaid.
    #[test]
    fn a_name_clash_is_warned_about() {
        let p = ProfilePicker::open(
            vec![profile("orthodox"), profile("far"), profile("mine")],
            None,
        );
        assert_eq!(
            p.rows()[0].clash,
            NameClash::Both,
            "orthodox is both things"
        );
        assert_eq!(
            p.rows()[1].clash,
            NameClash::Keymap,
            "far is a keyboard preset and not a layout"
        );
        assert_eq!(p.rows()[2].clash, NameClash::None);
    }

    /// The cursor is clamped to the rows there are, and an empty list does
    /// not panic.
    #[test]
    fn the_cursor_is_clamped() {
        let mut p = ProfilePicker::open(vec![profile("a"), profile("b")], None);
        p.down();
        p.down();
        p.down();
        assert_eq!(p.cursor(), 1, "it does not run off the bottom");
        p.up();
        p.up();
        assert_eq!(p.cursor(), 0, "nor the top");

        let empty = ProfilePicker::open(Vec::new(), None);
        assert_eq!(empty.cursor(), 0);
        assert_eq!(empty.chosen(), None);
        assert!(empty.current().is_none());
    }
}
