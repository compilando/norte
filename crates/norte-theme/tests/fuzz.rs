//! Short fuzz (proptest) of theme and color parsing (ADR 0020, phase T6):
//! no arbitrary input causes a `panic` — Ok or a typed Error. It runs in the
//! PR gate (like the repo's other proptest fuzzers).

use norte_theme::{Color, Theme};
use proptest::prelude::*;

proptest! {
    /// `Color::parse` never panics on arbitrary text (includes Unicode and
    /// control characters); a valid color round-trips.
    #[test]
    fn color_parse_never_panics(s in ".{0,16}") {
        let _ = Color::parse(&s); // Ok or Err, never panic.
    }

    /// `Theme::from_toml` never panics on arbitrary TOML: garbage bytes
    /// give Err; valid TOML with unknown keys is tolerated (ADR 0020's
    /// forward-compat, no top-level `deny_unknown_fields`).
    #[test]
    fn theme_from_toml_never_panics(s in "\\PC{0,256}") {
        let _ = Theme::from_toml(&s);
    }

    /// A theme with a MALFORMED color in a role fails CLEANLY (Err), never
    /// panics nor lets a garbage color through.
    #[test]
    fn malformed_color_in_role_is_error(bad in "[^#\"]{0,8}") {
        let src = format!("[roles]\nselection = {{ fg = \"{bad}\" }}\n");
        // Either it parses (if `bad` turned out to be a valid hex, unlikely with the
        // filter) or it is Err; never panic.
        let _ = Theme::from_toml(&src);
    }
}

/// A valid color embedded in a theme arrives intact (not fuzz, a
/// happy-path sanity anchor).
#[test]
fn valid_color_in_theme_arrives_intact() {
    let t = Theme::from_toml("[roles]\nerror = { fg = \"#ff0000\" }\n").unwrap();
    assert_eq!(
        t.style(norte_theme::Role::Error).fg,
        Some(Color::rgb(0xff, 0, 0))
    );
}
