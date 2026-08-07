//! The provenance line of a plugin help page (H3e), shared by the three
//! frontends.
//!
//! It lives here and not in [`crate::help`] for a reason that module states in
//! its own header: nothing there produces user-facing text, it deals in ids and
//! tags and lets each frontend name them. This line IS text — it is assembled
//! from Fluent — and it was, until H3h, three byte-identical copies in
//! `norte-tui`, `norte-gui` and `norte-cli`. Three copies of a security-shaped
//! string is how one of them drifts.

use norte_i18n::Lang;

use crate::display::cells;
use crate::display::middle_ellipsis;

/// Display budget for the whole provenance line, in terminal cells.
///
/// The publisher is THIRD-PARTY and the flags that follow it are the HOST's.
/// `norte_help::parse` caps a header string at 280 CHARACTERS, which in CJK is
/// 560 columns; the GUI paints this line unwrapped inside a 720 px panel, so an
/// unclamped publisher pushes `cut short` and `some bytes did not decode` off
/// the right edge. A plugin must not be able to decide whether a warning about
/// itself is on screen, which is what makes this a bound and not a nicety.
///
/// 96 cells is wide enough for a real publisher (the longest in the sample
/// registry is 31) and narrow enough to fit the panel at the default font.
pub const MAX_BADGE_CELLS: usize = 96;

/// The joiner between segments, and the reason the publisher is labelled.
const JOINER: &str = " · ";

/// Assembles the provenance line: `from an extension`, the publisher when there
/// is one, then the host's flags. `None` is for callers that have a built-in
/// topic — a plugin page always gets a line.
///
/// # The first segment is unconditional, and that is the whole point
///
/// It used to be assembled from three OPTIONAL facts — publisher, cut short,
/// decoded lossily — so a plugin declaring `publisher = ""` and shipping a
/// clean `help.md` got no line at all, and its page then had the exact shape of
/// a built-in one. That is not cosmetic: the reader meets a plugin page from
/// the extension manager, at the moment they are deciding whether to approve
/// it. A line that is always there is one a reader can learn to trust; one that
/// shows up only sometimes teaches the opposite of the truth when it is absent.
///
/// The plugin's ID is deliberately not painted. An id is a LOOKUP KEY and has
/// never been through a mask, and the reader already knows which extension they
/// opened — they arrived on its row, under its name.
///
/// # The `·` joiner, and what the label does NOT close
///
/// The segments are joined in band, so a publisher containing `·` can make one
/// segment look like two: `publisher = "ACME · cut short"` paints a page that
/// appears to admit it was truncated when it was not. The publisher is
/// therefore wrapped in a LABELLED segment (`help-plugin-by`, "published by X")
/// so a fabricated `·` reads inside a run that already announced whose name it
/// is.
///
/// That is MITIGATION, not a fix, and the distinction matters to whoever reads
/// this next: `"ACME · cut short"` still renders as two visually separate
/// segments and the label only makes the first one say `published by ACME`.
/// The only thing that closes it is one segment per line, and that is not worth
/// the rows on a 40-cell overlay — because the direction of the lie is benign.
/// A fabricated segment can add a warning the page does not deserve; it cannot
/// HIDE one, since the real flags are appended after the publisher and come
/// from the host. The flags are what a reader acts on, and they cannot be
/// suppressed from inside the file.
///
/// # Why the clamp is on the publisher and not on the result
///
/// Truncating the joined line would eat the flags, which is the failure being
/// guarded against. The publisher gets whatever the budget has left once the
/// host's own segments are reserved, so the flags are never the part that goes.
///
/// `norte_help::is_blank_id` and not `str::trim`: `"\u{3164}"` (HANGUL FILLER)
/// is not whitespace, so a trim-based check calls it a publisher and paints
/// `published by ` with nothing after it.
///
/// ```
/// use norte_frontend::help_badge::plugin_badge;
/// use norte_i18n::Lang;
///
/// let line = plugin_badge(Some("ACME"), false, false, Lang::En)
///     .expect("a plugin topic always has a badge");
/// assert!(line.contains("ACME"));
/// ```
#[must_use]
pub fn plugin_badge(
    publisher: Option<&str>,
    truncated: bool,
    lossy: bool,
    lang: Lang,
) -> Option<String> {
    let origin = norte_i18n::t_in(lang, "help-plugin-origin");
    let cut = truncated.then(|| norte_i18n::t_in(lang, "help-plugin-truncated"));
    let bad = lossy.then(|| norte_i18n::t_in(lang, "help-plugin-lossy"));

    let mut parts: Vec<String> = vec![origin];
    if let Some(p) = publisher.filter(|p| !norte_help::is_blank_id(p)) {
        // Everything the host contributes is reserved FIRST; the publisher gets
        // what is left. `saturating_sub` and not arithmetic: a catalogue whose
        // flags are longer than the whole budget leaves zero room, and
        // `middle_ellipsis` answers a zero budget with an empty string, which
        // still leaves the labelled segment saying whose name is missing.
        let joiners = cells(JOINER) * (1 + usize::from(cut.is_some()) + usize::from(bad.is_some()));
        let reserved = cells(&parts[0])
            + cut.as_deref().map_or(0, cells)
            + bad.as_deref().map_or(0, cells)
            + joiners
            // The label itself ("published by ") is part of the segment, and
            // measuring it with an empty argument is what makes the budget
            // account for a translation that is longer in one locale.
            + cells(&norte_i18n::ta_in(lang, "help-plugin-by", &[("who", "")]));
        let room = MAX_BADGE_CELLS.saturating_sub(reserved);
        let clamped = middle_ellipsis(p, room);
        parts.push(norte_i18n::ta_in(
            lang,
            "help-plugin-by",
            &[("who", clamped.as_str())],
        ));
    }
    parts.extend(cut);
    parts.extend(bad);
    Some(parts.join(JOINER))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_publisher_cannot_push_the_host_flags_off_the_line() {
        // 280 CJK characters is what the parser's cap allows, and it is 560
        // columns: the flags are appended AFTER the publisher, so an unclamped
        // segment is a third-party string deciding whether a host warning is
        // visible.
        let publisher = "字".repeat(280);
        for lang in [Lang::En, Lang::Es] {
            let badge = plugin_badge(Some(&publisher), true, true, lang)
                .expect("a plugin topic always has a badge");
            assert!(
                badge.contains(&norte_i18n::t_in(lang, "help-plugin-truncated")),
                "[{lang:?}] the cut-short flag must survive a 560-column publisher: {badge}"
            );
            assert!(
                badge.contains(&norte_i18n::t_in(lang, "help-plugin-lossy")),
                "[{lang:?}] the lossy flag must survive too: {badge}"
            );
            assert!(
                cells(&badge) <= MAX_BADGE_CELLS,
                "[{lang:?}] the badge is {} cells wide, over the {MAX_BADGE_CELLS} budget",
                cells(&badge)
            );
        }
    }

    #[test]
    fn a_publisher_that_fits_is_painted_whole() {
        let badge = plugin_badge(Some("ACME Tools"), false, false, Lang::En)
            .expect("a plugin topic always has a badge");
        assert!(
            badge.contains("ACME Tools"),
            "an ordinary publisher is not touched: {badge}"
        );
        assert!(
            !badge.contains('…'),
            "nothing was cut, so nothing says it was: {badge}"
        );
    }

    #[test]
    fn the_first_segment_is_there_with_nothing_else_to_say() {
        let badge =
            plugin_badge(None, false, false, Lang::En).expect("a plugin topic always has a badge");
        assert_eq!(badge, norte_i18n::t_in(Lang::En, "help-plugin-origin"));
    }

    #[test]
    fn a_publisher_blank_after_masking_is_not_a_publisher() {
        // HANGUL FILLER: not whitespace, blank on screen. A `trim`-based check
        // paints `published by ` with nothing after it.
        let badge = plugin_badge(Some("\u{3164}"), false, false, Lang::En)
            .expect("a plugin topic always has a badge");
        assert_eq!(badge, norte_i18n::t_in(Lang::En, "help-plugin-origin"));
    }

    #[test]
    fn the_flags_keep_their_order_behind_the_publisher() {
        let badge = plugin_badge(Some("ACME"), true, true, Lang::En)
            .expect("a plugin topic always has a badge");
        let by = badge
            .find(&norte_i18n::ta_in(
                Lang::En,
                "help-plugin-by",
                &[("who", "ACME")],
            ))
            .expect("the publisher segment");
        let cut = badge
            .find(&norte_i18n::t_in(Lang::En, "help-plugin-truncated"))
            .expect("the cut-short flag");
        let bad = badge
            .find(&norte_i18n::t_in(Lang::En, "help-plugin-lossy"))
            .expect("the lossy flag");
        assert!(
            by < cut && cut < bad,
            "host flags come last, in a fixed order: {badge}"
        );
    }
}
