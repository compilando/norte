//! The function-key bar (spec 2026-09-10): ten cells, `F1`-`F10`, with what
//! each one does on the current screen.
//!
//! DERIVED from the effective keymap, never hand-written: rebinding `F5`
//! changes its label, and a screen that does not bind `F7` leaves the cell
//! blank. It is the orthodox file manager's signature and the cheapest
//! discovery help there is; both frontends paint it from here.

use crate::keymap::Effective;
use norte_i18n::{Lang, t_in};

/// One cell of the bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyCell {
    /// `1`..=`10`.
    pub key: u8,
    /// The short label, in the requested language. Empty = the key does not
    /// bind anything on this screen.
    pub label: String,
    /// The command that runs, if any.
    pub command: Option<String>,
}

/// How many cells the bar has.
pub const CELLS: u8 = 10;

/// The ten cells of a screen, in the given language.
///
/// The label is the bar's own (`keybar-<cmd>`, short: every F-key of the
/// bundled presets has one), then the MENU's (`menu-item-<cmd>`), then the
/// help description (`help-cmd-<cmd>`), and if there is not one either, the
/// last segment of the id: a plugin command or one nobody translated still
/// says something.
///
/// ```
/// use norte_frontend::keybar::cells_in;
/// use norte_frontend::keymap::{Effective, Screen, parse_keymap};
/// use norte_i18n::Lang;
///
/// let preset = parse_keymap(
///     "[pane]\nkeymap = [{ on = [\"f5\"], run = \"pane.copy\" }]\n",
/// )
/// .unwrap();
/// let eff = Effective::build_for(&preset, &[], &["pane.copy"], Screen::Browse).unwrap();
/// let cells = cells_in(&eff, Lang::En);
/// assert_eq!(cells.len(), 10);
/// assert_eq!(cells[4].command.as_deref(), Some("pane.copy"));
/// assert!(!cells[4].label.is_empty());
/// assert!(cells[0].label.is_empty() && cells[0].command.is_none());
/// ```
#[must_use]
pub fn cells_in(eff: &Effective, lang: Lang) -> Vec<KeyCell> {
    let bindings = eff.bindings();
    (1..=CELLS)
        .map(|n| {
            let chord = format!("f{n}");
            let command = bindings
                .iter()
                .find(|(seq, _)| *seq == chord)
                .map(|(_, cmd)| (*cmd).to_owned());
            let label = command
                .as_deref()
                .map(|c| label_in(c, lang))
                .unwrap_or_default();
            KeyCell {
                key: n,
                label,
                command,
            }
        })
        .collect()
}

/// The short label of a command, in the given language: the bar's own
/// (`keybar-<cmd>`, written to fit a cell at 80 columns), then the menu's,
/// then the help's — a key a user or a plugin bound still says something.
fn label_in(command: &str, lang: Lang) -> String {
    let dashed = command.replace('.', "-");
    for prefix in ["keybar-", "menu-item-", "help-cmd-"] {
        let key = format!("{prefix}{dashed}");
        let text = t_in(lang, &key);
        // `t_in`'s contract is to return the key when it is missing.
        if text != key {
            return text;
        }
    }
    command.rsplit('.').next().unwrap_or(command).to_owned()
}

/// Where each cell falls in a row of `width` cells: `(x0, width)` per key,
/// in order. The row is split into equal parts and the remainder goes to
/// the last ones, so that all ten exist whenever there are ten cells; with
/// fewer, whichever fit. Painting and the mouse zones come from here, so
/// they measure the same.
#[must_use]
pub fn layout(width: usize) -> Vec<(usize, usize)> {
    let n = usize::from(CELLS);
    if width < n {
        return (0..width).map(|x| (x, 1)).collect();
    }
    let base = width / n;
    let extra = width % n;
    let mut x = 0;
    (0..n)
        .map(|i| {
            let w = base + usize::from(i >= n - extra);
            let cell = (x, w);
            x += w;
            cell
        })
        .collect()
}

/// The text of a cell that measures `width`: the number flush to the left
/// and the label behind it, capitalized and CUT at the end if it does not
/// fit — `7New dire` reads, `7New …ctory` does not: in eight cells what
/// says something is the beginning. `F` is not painted: ten cells of `F`
/// say nothing and cost ten columns.
#[must_use]
pub fn cell_text(cell: &KeyCell, width: usize) -> String {
    let num = cell.key.to_string();
    // A cell that cannot even fit its own number goes blank: painting `10`
    // in a one-column cell would shift every cell to its right relative to
    // their zones (review m11).
    if width < num.len() {
        return " ".repeat(width);
    }
    // A space between the number and the label when the cell has room for
    // it and for something to read (spec 2026-09-15): `1 Help` reads at a
    // glance and `1Help` needs the eye to separate it. In narrow cells the
    // space gives way before a letter, which is what actually says what the
    // key does.
    //
    // And a blank last column whenever there is room for one and a letter:
    // it is what separates this cell from the next one's number, and it
    // gives way AFTER that space — `8 Delete9 Menu` at 80 columns read as
    // one word, `8Delete 9 Menu` reads as two keys.
    let gap = usize::from(width >= num.len() + 2);
    // Capitalize BEFORE measuring, and what is painted is what is measured:
    // `ß` becomes `SS` and takes up two (review m7).
    let mut chars = cell.label.chars();
    let capitalized: String = chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>())
        .unwrap_or_default()
        + chars.as_str();
    let after_num = width - num.len() - gap;
    let separator = usize::from(
        width >= num.len() + 4
            && (after_num > crate::display::cells(&capitalized) || after_num >= 8),
    );
    let num = format!("{num}{}", " ".repeat(separator));
    let room = after_num - separator;
    let mut label = String::new();
    let mut used = 0;
    for c in capitalized.chars() {
        let w = crate::display::cells(&c.to_string());
        if used + w > room {
            break;
        }
        label.push(c);
        used += w;
    }
    // Cut, and with a WHOLE word before the cut: it stops there. `7 Crear
    // di` or `9 Barra de` glued to the next cell's number used to read as
    // broken words; `7 Crear` leaves the gap that separates the two cells.
    // With no whole word to keep, the usual cut (`7 Renomb`): the start of
    // the word says more than nothing.
    if label.len() < capitalized.len()
        && let Some(space) = label.rfind(' ')
    {
        label.truncate(space);
        used = crate::display::cells(&label);
    }
    format!("{num}{label}{}", " ".repeat(room - used + gap))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The row is split whole and in order; with fewer than ten cells,
    /// whichever fit; and a cell's text measures exactly its width. A label
    /// that does not fit is cut at the last WHOLE word if there is one:
    /// `7 Crear di` glued to `8 Delete` used to read as a single broken
    /// word; `7 Crear` leaves the gap that separates the two cells.
    #[test]
    fn the_cut_respects_the_last_whole_word() {
        let c = |label: &str| KeyCell {
            key: 7,
            label: label.into(),
            command: Some("pane.mkdir".into()),
        };
        assert_eq!(cell_text(&c("create directory"), 10), "7 Create  ");
        assert_eq!(cell_text(&c("key bar"), 10), "7 Key bar ");
        // A label that fills the cell drops the space after the number,
        // never the gap at the end: `8 Delete9 Menu` ran together at 80
        // columns (review of 2026-10-07).
        assert_eq!(cell_text(&c("rename"), 8), "7Rename ");
        // With no whole word to keep, the usual cut: the start of the word
        // says more than nothing.
        assert_eq!(cell_text(&c("renaming"), 8), "7Renami ");
        // And what fits, fits whole.
        assert_eq!(cell_text(&c("view"), 8), "7 View  ");
    }

    #[test]
    fn the_split_covers_the_row_and_the_text_measures_its_cell() {
        let l = layout(83);
        assert_eq!(l.len(), 10);
        assert_eq!(l[0], (0, 8));
        assert_eq!(l[9].0 + l[9].1, 83, "the last one ends at the edge");
        assert!(l.windows(2).all(|w| w[0].0 + w[0].1 == w[1].0));
        assert_eq!(layout(4).len(), 4);
        let c = KeyCell {
            key: 10,
            label: "Quit norte now".into(),
            command: Some("app.quit".into()),
        };
        assert_eq!(crate::display::cells(&cell_text(&c, 8)), 8);
        assert!(cell_text(&c, 8).starts_with("10"));
        let empty = KeyCell {
            key: 7,
            label: String::new(),
            command: None,
        };
        assert_eq!(cell_text(&empty, 6), "7     ");
    }

    /// Every F-key of every preset, on every screen, in both languages,
    /// reads WHOLE at 80 and at 132 columns: a label written for the bar,
    /// never a command's long name cut mid-phrase ("Close the", "Fit the",
    /// "Go on to", landing shots 2026-10-08).
    #[test]
    fn every_bundled_f_key_label_fits_whole() {
        use crate::keymap::{Effective, Screen, parse_keymap, preset_commands, presets};
        // The narrowest cell `layout` gives at 80 and at 132 columns.
        let widths = [80, 132].map(|cols| layout(cols).iter().map(|(_, w)| *w).min().unwrap_or(0));
        for name in presets::NAMES {
            let preset = parse_keymap(presets::source(name).expect("bundled")).expect("parses");
            for screen in [Screen::Browse, Screen::Viewer, Screen::Dialog] {
                let known = preset_commands(screen);
                let known: Vec<&str> = known.iter().map(String::as_str).collect();
                let eff = Effective::build_for(&preset, &[], &known, screen).expect("builds");
                for lang in [Lang::En, Lang::Es] {
                    for cell in cells_in(&eff, lang) {
                        let Some(cmd) = &cell.command else { continue };
                        let key = format!("keybar-{}", cmd.replace('.', "-"));
                        assert_ne!(
                            t_in(lang, &key),
                            key,
                            "{name} {screen:?} {lang:?}: no {key}"
                        );
                        let mut chars = cell.label.chars();
                        let cap: String = chars
                            .next()
                            .map(|c| c.to_uppercase().collect::<String>())
                            .unwrap_or_default()
                            + chars.as_str();
                        for w in widths {
                            let text = cell_text(&cell, w);
                            assert!(
                                text.contains(&cap),
                                "{name} {screen:?} {lang:?} F{}: {cap:?} cut to {text:?} at {w}",
                                cell.key
                            );
                        }
                    }
                }
            }
        }
    }

    /// With no menu or help translation, the last segment of the id.
    #[test]
    fn the_label_falls_back_to_the_id_when_nobody_translated_it() {
        assert_eq!(
            label_in("plugin:acme:frobnicate", Lang::En),
            "plugin:acme:frobnicate"
        );
        assert_eq!(label_in("dialog.no-existe", Lang::Es), "no-existe");
    }
}
