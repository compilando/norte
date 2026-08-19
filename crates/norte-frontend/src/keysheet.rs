//! The keyboard reference sheet: every bound key of every screen, INCLUDING
//! the ones this build cannot run.
//!
//! One row builder, shared, because the sheet was generated three times — the
//! TUI's three screens, the GUI's (which had no dialog section at all) and the
//! CLI's page — from three walks over the same effective keymaps. Padding and
//! styling stay each frontend's; the data stops being theirs.
//!
//! Two rules make this different from a list of shortcuts, and both are the
//! point of the feature:
//!
//! - **Unavailable rows are rows.** K2b ships four presets transcribed from
//!   other programs, and about thirty of their bindings name a command norte
//!   has not built (issues #131..#140 — #131, the drive family, is now built,
//!   `2026-08-10-volumes.md`). Built on
//!   [`Effective::bindings_all`] rather than
//!   [`Effective::bindings`](crate::keymap::Effective::bindings), which drops
//!   them: the sheet answers "what does this key do", and "nothing, and here
//!   is why" is an answer. Hiding the row leaves a Total Commander migrant
//!   pressing `alt+f5` and learning nothing.
//! - **They are INTERLEAVED, in key order, not gathered into an unavailable
//!   section at the bottom.** A reader scanning the F-keys has to find
//!   `alt+f5` where it belongs, greyed and explained, not discover much later
//!   that the sheet was two lists.

use crate::keymap::{Availability, Chord, Effective, Screen, paint_chord, render_seq};

/// One row of the keyboard reference sheet: what the key IS, not what the
/// preset wished it were.
///
/// Owns its strings. A row outlives the borrow of the [`Effective`] it came
/// from in every caller — the CLI renders after `CliChords` has moved, the TUI
/// keeps its sheet across frames — and two allocations on a page built once
/// per keypress-into-help are not the cost worth borrowing for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetRow {
    /// Which screen's map this binding belongs to. The renderer groups by it;
    /// the row carries it so a flat list is still self-describing (the
    /// `--json` dump of `norte help keys` is exactly that).
    pub screen: Screen,
    /// The sequence PAINTED: spelled for a reader (`f5` → `F5`, `g g` for a
    /// two-chord sequence) and masked, because a project `./.norte/keymap.toml`
    /// can bind any lone codepoint and this string reaches a terminal — see
    /// [`paint_chord`].
    pub chord: String,
    /// The same sequence in CHORDS, unpainted: what the shortcut editor (K3c)
    /// hands to [`rebind_check`](crate::keymap::rebind_check) and, spelled
    /// with `Display`, to `norte_config::persist_keymap_unbind`.
    ///
    /// Carried rather than parsed back out of [`Self::chord`], which is
    /// painted — masking is not reversible, so the display string is a
    /// dead end for anything that needs to ACT on the row. Renderers ignore
    /// it.
    pub seq: Vec<Chord>,
    /// The command name, raw. The LABEL is the renderer's:
    /// [`command_label`](crate::whichkey::command_label) is the shared router
    /// (`help-cmd-*` / `dialog-cmd-*`, falling back to this name), and a
    /// surface with its own label column is free to use its own.
    pub command: String,
    /// Whether this build can run it — the keymap question (`Status::Planned`
    /// in the catalogue, or a command this frontend does not implement), NOT
    /// `norte_help::Availability`, which is about the state of the app right
    /// now. Render anything other than [`Availability::Here`] dimmed and with
    /// [`short_unavailable_message`](crate::keymap::short_unavailable_message).
    ///
    /// It is only as good as the command set the [`Effective`] was built with,
    /// and the two halves are not equally solid.
    /// [`Availability::NotBuilt`] comes from the shared catalogue and is a
    /// fact about norte; [`Availability::NotHere`] means no more than "absent
    /// from the set this caller passed". A frontend passes its own commands,
    /// so for the TUI and the GUI it does mean "not implemented here" — but
    /// `norte help keys`, which has no frontend and passes the bundled
    /// presets' vocabulary, must not report it as one (it prints only the
    /// `NotBuilt` half).
    pub avail: Availability,
    /// Whether this binding lives in `[global]` rather than this screen's own
    /// section ([`Effective::is_global`]). The reference sheet has no use for
    /// it — every row is read-only there — but the shortcut editor (K3c #141)
    /// builds its rows from the same walk and needs to know, per row, whether
    /// writing to it would touch this screen alone or all three.
    pub global: bool,
}

/// Every binding of every given screen, in one flat list.
///
/// Order is the caller's screen order first — each frontend names its
/// sections in the order a reader learns them, which is not the enum's order —
/// then, within a screen, the effective map's own precedence order, unchanged
/// and unsorted. Precedence order IS what the key does: a user layer that
/// overrides a preset binding sits where the resolver would find it.
///
/// ```
/// use norte_frontend::keymap::{Availability, Effective, Screen, parse_keymap};
/// use norte_frontend::keysheet::sheet;
///
/// let src = r#"
/// [pane]
/// keymap = [
///     { on = ["f5"], run = "pane.copy" },
///     { on = ["alt+f5"], run = "pane.pack" },
///     { on = ["f6"], run = "pane.move" },
/// ]
/// "#;
/// let preset = parse_keymap(src).unwrap();
/// // `pane.pack` is NOT in this frontend's command list, so the catalogue
/// // answers for it: live, but not here.
/// let known = ["pane.copy", "pane.move"];
/// let eff = Effective::build_for(&preset, &[], &known, Screen::Browse).unwrap();
///
/// let rows = sheet(&[(Screen::Browse, eff)]);
/// let chords: Vec<&str> = rows.iter().map(|r| r.chord.as_str()).collect();
/// // Interleaved, in the map's order: the unavailable key keeps its place.
/// assert_eq!(chords, ["F5", "Alt+F5", "F6"]);
/// assert!(matches!(rows[1].avail, Availability::NotHere));
/// ```
#[must_use]
pub fn sheet(effectives: &[(Screen, Effective)]) -> Vec<SheetRow> {
    effectives
        .iter()
        .flat_map(|(screen, eff)| sheet_of(*screen, eff))
        .collect()
}

/// One screen's rows, from a BORROWED map — [`sheet`] over a single entry.
///
/// [`sheet`] takes its maps by value because every help surface owns a clone
/// of the three effectives; the shortcut editor (K3c) does not — the TUI's
/// live maps sit inside its resolvers and are handed out as `&Effective`.
/// Cloning three keymaps to ask them what they contain would be a strange
/// price to pay, so the row builder is the borrowing one and [`sheet`] is the
/// convenience over it.
#[must_use]
pub fn sheet_of(screen: Screen, eff: &Effective) -> Vec<SheetRow> {
    eff.bindings_all_seq()
        .into_iter()
        .map(|(seq, command, avail)| SheetRow {
            screen,
            chord: paint_chord(&render_seq(seq)),
            global: eff.is_global(seq),
            seq: seq.to_vec(),
            command: command.to_owned(),
            avail,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{SheetRow, sheet};
    use crate::keymap::{Availability, Effective, Screen, parse_keymap, presets};

    /// Builds the three screens the way a frontend does: the command
    /// vocabulary is what the catalogue calls Live, so a `Planned` binding
    /// comes back marked instead of pretending to work.
    fn effectives(preset: &str) -> Vec<(Screen, Effective)> {
        let src = presets::source(preset).expect("bundled preset");
        let kf = parse_keymap(src).expect("preset parses");
        [Screen::Browse, Screen::Viewer, Screen::Dialog]
            .into_iter()
            .filter_map(|screen| {
                let known = crate::keymap::preset_commands(screen);
                let known: Vec<&str> = known
                    .iter()
                    .map(String::as_str)
                    .filter(|n| {
                        crate::keymap::catalogue::lookup(n)
                            .is_none_or(|d| d.status == crate::keymap::Status::Live)
                    })
                    // `pane.pack` se deja FUERA a propósito: la hoja tiene que
                    // tener alguna fila no ejecutable para que estos tests
                    // digan algo, y desde #132 el catálogo no tiene ni un
                    // `Planned` — así que la que queda es la otra clase, un
                    // comando vivo que ESTE frontend no implementa. Es
                    // exactamente lo que le pasa a la GUI con la mitad de la
                    // lista, no un caso inventado.
                    .filter(|n| *n != "pane.pack")
                    .collect();
                Effective::build_for(&kf, &[], &known, screen)
                    .ok()
                    .map(|eff| (screen, eff))
            })
            .collect()
    }

    /// The row K2b made necessary: Total Commander's `Alt+F5` packs, this
    /// build does not run that command, and the sheet says both things at
    /// once — the key and why it will do nothing.
    #[test]
    fn a_planned_binding_is_a_row_with_its_issue() {
        let rows = sheet(&effectives("total-commander"));
        let pack = rows
            .iter()
            .find(|r| r.command == "pane.pack")
            .expect("total-commander binds pane.pack");
        assert_eq!(pack.chord, "Alt+F5");
        assert_eq!(pack.avail, Availability::NotHere);
    }

    /// The whole reason the sheet stopped calling `bindings()`: nothing is
    /// dropped, and the count proves it against the filtered view.
    #[test]
    fn nothing_is_dropped_and_the_unavailable_ones_are_the_difference() {
        let effs = effectives("total-commander");
        let rows = sheet(&effs);
        let all: usize = effs.iter().map(|(_, e)| e.bindings_all().len()).sum();
        let runnable: usize = effs.iter().map(|(_, e)| e.bindings().len()).sum();
        assert_eq!(rows.len(), all, "a row per binding, available or not");
        assert!(
            runnable < all,
            "this preset must carry unavailable bindings for the test to mean anything"
        );
        assert_eq!(
            rows.iter()
                .filter(|r| r.avail != Availability::Here)
                .count(),
            all - runnable
        );
    }

    /// Interleaved, not segregated: the unavailable rows sit where the keymap
    /// puts them, so a reader scanning the F-keys finds `Alt+F5` between its
    /// neighbours instead of in a section at the bottom.
    #[test]
    fn unavailable_rows_keep_their_place_in_key_order() {
        let effs = effectives("total-commander");
        let rows = sheet(&effs);
        let browse: Vec<&SheetRow> = rows.iter().filter(|r| r.screen == Screen::Browse).collect();
        let pos = |cmd: &str| {
            browse
                .iter()
                .position(|r| r.command == cmd)
                .unwrap_or_else(|| panic!("{cmd} is bound in browse"))
        };
        // Total Commander's Alt+F5/Alt+F6 sit between Alt+F4 (edit-new, also
        // planned) and the rest: what matters is that an available row follows
        // an unavailable one, which a segregated sheet could not produce.
        let pack = pos("pane.pack");
        assert!(
            browse[..pack].iter().any(|r| r.avail == Availability::Here),
            "available rows come before it"
        );
        assert!(
            browse[pack + 1..]
                .iter()
                .any(|r| r.avail == Availability::Here),
            "and after it: the sheet is one list, not two"
        );
        // The order within a screen IS the effective map's, unchanged.
        let (_, eff) = effs
            .iter()
            .find(|(s, _)| *s == Screen::Browse)
            .expect("browse");
        let want: Vec<String> = eff
            .bindings_all()
            .into_iter()
            .map(|(_, cmd, _)| cmd.to_owned())
            .collect();
        let got: Vec<String> = browse.iter().map(|r| r.command.clone()).collect();
        assert_eq!(got, want);
    }

    /// Screen order is the caller's, and every row says which screen it is
    /// from — a flat list that could not be grouped again would be useless to
    /// a renderer with sections.
    #[test]
    fn screens_come_in_the_order_they_were_given() {
        let mut effs = effectives("orthodox");
        effs.reverse();
        let rows = sheet(&effs);
        let mut seen: Vec<Screen> = Vec::new();
        for row in &rows {
            if seen.last() != Some(&row.screen) {
                assert!(
                    !seen.contains(&row.screen),
                    "a screen's rows are contiguous"
                );
                seen.push(row.screen);
            }
        }
        assert_eq!(seen, vec![Screen::Dialog, Screen::Viewer, Screen::Browse]);
    }

    /// The sheet is painted, so it inherits `paint_chord`'s masking: a project
    /// layer carries no trust and this string reaches a terminal.
    #[test]
    fn chords_are_painted_and_masked() {
        // U+202E RIGHT-TO-LEFT OVERRIDE bound as a lone codepoint, which is a
        // legal chord.
        let src = "[pane]\nkeymap = [{ on = [\"\u{202e}\"], run = \"pane.copy\" }]\n";
        let kf = parse_keymap(src).expect("fixture parses");
        let eff =
            Effective::build_for(&kf, &[], &["pane.copy"], Screen::Browse).expect("fixture builds");
        let rows = sheet(&[(Screen::Browse, eff)]);
        let row = rows.first().expect("one row");
        assert!(
            !row.chord.chars().any(norte_encoding::is_terminal_hazard),
            "{:?}",
            row.chord
        );
    }
}
