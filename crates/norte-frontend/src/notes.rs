//! The sentences that say a listing is NOT complete, or that it is not what
//! it looks like.
//!
//! All of them obey the same rule, written back in the day for the terminal's
//! bar: **a listing that shows less than there is is never silent.** What is
//! missing is not there, so there is no row where the reader could stumble
//! on it — if nobody says so, the screen asserts that is all there is.
//!
//! They live here because both frontends need them and each one used to word
//! them on its own, which is how a decision drifts without anyone noticing
//! (ADR 0077). And they already had: the window said "N entries were
//! skipped" without the ⚠, and **also with N equal to zero**, announcing an
//! incomplete listing that was complete; and the name-reinterpretation mark,
//! which in the terminal is permanent because the painted names are not the
//! bytes, did not exist there at all.
//!
//! Each one returns the EMPTY string when there is nothing to say. That is
//! the other half of the contract and not a detail: a sentence that always
//! shows up is noise, and noise teaches people to skip the line on the exact
//! day it says something.
//!
//! # The spacing contract
//!
//! **A sentence carries no separator and no spaces at its ends.** How they
//! are separated is up to whoever lines them up: the terminal joins them with
//! two spaces in a text bar, the window sends them in separate nodes and
//! separates them with CSS. Without this rule written down, a `.ftl` with a
//! leading space gives the terminal three and the window a double gap, and no
//! test notices — so `no_sentence_carries_its_own_spacing`, below, pins it
//! down.

use norte_i18n::{Lang, t_in, ta_in};

/// What the provider SKIPPED from the listing: names it could not stat,
/// entries past a limit of its own (#93, #96).
///
/// With zero it stays quiet, and that is the bug this function exists never
/// to commit again: "0 entries were skipped" describes a complete listing as
/// if it were incomplete, wasting the only signal there is for when
/// something is truly missing.
///
/// ```
/// use norte_frontend::notes::skipped;
/// use norte_i18n::Lang;
///
/// assert!(skipped(Some(3), Lang::En).contains('3'));
/// assert_eq!(skipped(Some(0), Lang::En), "", "zero skipped is not a warning");
/// assert_eq!(skipped(None, Lang::En), "", "the provider does not keep count");
/// ```
#[must_use]
pub fn skipped(n: Option<u64>, lang: Lang) -> String {
    match n {
        Some(n) if n > 0 => ta_in(lang, "status-archive-skipped", &[("n", &n.to_string())]),
        _ => String::new(),
    }
}

/// Names are being REINTERPRETED with a different encoding (#57).
///
/// Permanent for as long as it lasts, not only in the toggle's message: what
/// is painted is not the bytes on disk, and the reader has to be able to know
/// it at the moment they decide to copy or delete something, not half a
/// minute earlier.
///
/// ```
/// use norte_frontend::notes::names_encoding;
/// use norte_i18n::Lang;
///
/// assert_eq!(names_encoding(None, Lang::En), "", "no reinterpretation, nothing");
/// let cp437 = norte_encoding::NameEncoding::Cp437;
/// assert!(names_encoding(Some(cp437), Lang::En).contains("cp437"));
/// ```
#[must_use]
pub fn names_encoding(enc: Option<norte_encoding::NameEncoding>, lang: Lang) -> String {
    enc.map_or_else(String::new, |e| {
        ta_in(lang, "status-names-encoding", &[("enc", e.label())])
    })
}

/// Marks the last refresh dropped because their entry is no longer there.
///
/// Never silent, and for a harder reason than the rest: with the selection
/// empty the operand funnel falls back to the CURSOR, so staying quiet about
/// the selection having emptied redirects the next bulk operation onto
/// something nobody marked.
///
/// ```
/// use norte_frontend::notes::pruned_marks;
/// use norte_i18n::Lang;
///
/// assert!(pruned_marks(2, Lang::En).contains('2'));
/// assert_eq!(pruned_marks(0, Lang::En), "");
/// ```
#[must_use]
pub fn pruned_marks(n: usize, lang: Lang) -> String {
    if n == 0 {
        return String::new();
    }
    ta_in(lang, "status-marks-pruned", &[("n", &n.to_string())])
}

/// How many entries are marked and how much they weigh.
///
/// The only one in this module that is not a warning but a counter, and
/// that is why it goes AFTER the rest where space is shared out: a trimmed
/// warning stops warning, and a trimmed counter only stops counting.
///
/// Directories are named apart because they add no bytes: a "3 marked, 12 KB"
/// over two folders and a file poorly describes what is about to be moved.
///
/// ```
/// use norte_frontend::notes::marked;
/// use norte_i18n::Lang;
///
/// assert_eq!(marked(0, 0, 0, Lang::En), "", "marking nothing wins no noise");
/// assert!(marked(2, 2048, 0, Lang::En).contains('2'));
/// // With directories involved, the sentence counts them apart.
/// assert!(marked(3, 2048, 2, Lang::En).contains('3'));
/// ```
#[must_use]
pub fn marked(n: usize, bytes: u64, dirs: usize, lang: Lang) -> String {
    if n == 0 {
        return String::new();
    }
    let n = n.to_string();
    let size = crate::human_bytes(bytes);
    if dirs == 0 {
        return ta_in(lang, "status-marked", &[("n", &n), ("size", &size)]);
    }
    ta_in(
        lang,
        "status-marked-with-dirs",
        &[("n", &n), ("size", &size), ("dirs", &dirs.to_string())],
    )
}

/// The listing is still FILLING IN (pagination, ADR 0017): this is what there
/// is SO FAR.
///
/// ```
/// use norte_frontend::notes::filling;
/// use norte_i18n::Lang;
///
/// assert!(filling(true, 120, Lang::En).contains("120"));
/// assert_eq!(filling(false, 120, Lang::En), "", "already complete");
/// ```
#[must_use]
pub fn filling(loading: bool, so_far: usize, lang: Lang) -> String {
    if !loading {
        return String::new();
    }
    ta_in(lang, "pane-loading", &[("n", &so_far.to_string())])
}

/// This slot COULD NOT be listed (#235).
///
/// Without this the screen asserts the directory is empty, which is exactly
/// what is not known.
///
/// ```
/// use norte_frontend::notes::unlisted;
/// use norte_i18n::Lang;
///
/// assert!(!unlisted(true, Lang::En).is_empty());
/// assert_eq!(unlisted(false, Lang::En), "");
/// ```
#[must_use]
pub fn unlisted(yes: bool, lang: Lang) -> String {
    if yes {
        return t_in(lang, "pane-unlisted");
    }
    String::new()
}

/// How many entries the active HIDING sets aside (#107).
///
/// ```
/// use norte_frontend::notes::hidden;
/// use norte_i18n::Lang;
///
/// assert!(hidden(4, Lang::En).contains('4'));
/// assert_eq!(hidden(0, Lang::En), "", "a dir with nothing hidden says nothing");
/// ```
#[must_use]
pub fn hidden(n: usize, lang: Lang) -> String {
    if n == 0 {
        return String::new();
    }
    ta_in(lang, "status-hidden", &[("n", &n.to_string())])
}

/// "pane closed · alt+h splits again": what is said when a slot is closed,
/// with the chord the ACTIVE preset ties to `layout.split-h`.
///
/// Closing a pane is easy to do by accident and hard to undo if you do not
/// know with what — whoever does it is left staring at half a screen — and
/// the shortcut is not hardcoded: it comes from the effective keymap, like
/// the rest of the chrome (ADR 0106). With none bound, the menu is named,
/// which is always there.
///
/// ```
/// use norte_frontend::notes::slot_closed;
/// use norte_i18n::Lang;
///
/// assert!(slot_closed(Some("alt+h"), Lang::Es).contains("alt+h"));
/// assert!(!slot_closed(None, Lang::Es).is_empty(), "no key, the menu");
/// ```
#[must_use]
pub fn slot_closed(chord: Option<&str>, lang: Lang) -> String {
    match chord {
        Some(c) => ta_in(lang, "msg-layout-slot-closed", &[("chord", c)]),
        None => t_in(lang, "msg-layout-slot-closed-nokey"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No key is left untranslated in either language.
    ///
    /// `t_in` answers with the key itself when it does not have it, so an
    /// entry missing from a `.ftl` would be painted as `status-archive-skipped`
    /// to the reader — the echo the rest of the crate takes care not to
    /// produce.
    #[test]
    fn the_sentences_exist_in_both_languages() {
        let cp437 = norte_encoding::NameEncoding::Cp437;
        for lang in [Lang::Es, Lang::En] {
            for sentence in [
                skipped(Some(2), lang),
                names_encoding(Some(cp437), lang),
                pruned_marks(1, lang),
                marked(2, 10, 0, lang),
                marked(2, 10, 1, lang),
                filling(true, 3, lang),
                unlisted(true, lang),
                hidden(5, lang),
                slot_closed(Some("alt+h"), lang),
                slot_closed(None, lang),
            ] {
                assert!(!sentence.is_empty(), "no sentence in {lang:?}");
                assert!(
                    !sentence.starts_with("status-")
                        && !sentence.starts_with("pane-")
                        && !sentence.contains('{'),
                    "raw key or unsubstituted placeholder in {lang:?}: {sentence}"
                );
            }
        }
    }

    /// None of them carries a separator or spaces at its ends: that belongs
    /// to whoever lines them up, and each frontend does it differently. A
    /// `.ftl` with a leading space gives the terminal three and the window a
    /// double gap.
    #[test]
    fn no_sentence_carries_its_own_spacing() {
        let cp437 = norte_encoding::NameEncoding::Cp437;
        for lang in [Lang::Es, Lang::En] {
            for sentence in [
                skipped(Some(2), lang),
                names_encoding(Some(cp437), lang),
                pruned_marks(1, lang),
                marked(2, 10, 1, lang),
                filling(true, 3, lang),
                unlisted(true, lang),
                hidden(5, lang),
                slot_closed(Some("alt+h"), lang),
                slot_closed(None, lang),
            ] {
                assert_eq!(
                    sentence.trim(),
                    sentence,
                    "carries its own spacing: {sentence:?}"
                );
            }
        }
    }

    /// The skipped-entries warning carries the MARK: that is what tells it
    /// apart from a counter a meter away from the screen, and it is half the
    /// reason the window's warning did not read as a warning.
    #[test]
    fn the_skipped_warning_is_marked() {
        for lang in [Lang::Es, Lang::En] {
            assert!(
                skipped(Some(9), lang).contains('⚠'),
                "without the mark it does not read as a warning in {lang:?}"
            );
        }
    }

    /// And all of them stay quiet when there is nothing to say. That is what
    /// makes the one that speaks mean something.
    #[test]
    fn silence_is_the_normal_case() {
        let l = Lang::Es;
        assert_eq!(skipped(None, l), "");
        assert_eq!(skipped(Some(0), l), "");
        assert_eq!(names_encoding(None, l), "");
        assert_eq!(pruned_marks(0, l), "");
        assert_eq!(marked(0, 999, 3, l), "", "no marks, nothing to add up");
        assert_eq!(filling(false, 9, l), "");
        assert_eq!(unlisted(false, l), "");
        assert_eq!(hidden(0, l), "");
    }
}
