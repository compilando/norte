//! Help content (F1), in the two shapes the app needs it.
//!
//! Both are built from the EFFECTIVE keymap (preset plus the user's and the
//! project's layers) and the Fluent catalogue (`help-cmd-*`, `dialog-cmd-*`),
//! never from a hand-kept list. Extending the app is therefore a binding in
//! the preset plus a catalogue entry — the i18n suite enforces the second.
//!
//! - [`build`] renders the flat F1 cheatsheet: every binding of every screen,
//!   in real precedence order (what the key DOES, not what the preset says).
//! - [`TuiChords`] is this frontend's [`norte_help::ChordResolver`], the seam
//!   through which `norte-help`'s corpus resolves its live `{{cmd:…}}` marks
//!   against the reader's own keymap and language.
//!
//! Every chord either shape paints goes through
//! [`norte_frontend::keymap::paint_chord`]. `Chord`'s `Display` is raw and
//! lower case ON PURPOSE (logs and debug output want the real chord), and a
//! project `./.norte/keymap.toml` carries no trust, so masking — and then the
//! conventional spelling — is the painter's duty, in ONE shared home rather
//! than one per call site.

use norte_frontend::keysheet::{SheetRow, sheet};
use norte_i18n::t;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use unicode_width::UnicodeWidthStr;

use crate::keymap::{Effective, Screen};

/// This frontend's [`ChordResolver`](norte_help::ChordResolver), which since
/// task 4.4 of the multi-frontend transition is the SHARED one: the graphical
/// host asks `norte-help` the same three questions, and two copies would be two
/// help pages teaching different keys for the same command (ADR 0066, D14).
///
/// Re-exported under its old name so no call site of this crate moved. Its
/// tests stayed here too, unlike the palette's: they are written against the
/// TUI's real presets and its whole command vocabulary, which is where they
/// describe something the model alone cannot.
pub use norte_frontend::help_chords::Chords as TuiChords;

/// Width in CELLS of the chord column of the cheatsheet.
const CHORD_COLUMN: usize = 14;

/// Padding that takes `seq` up to `col` CELLS, or nothing when it is already
/// wider.
///
/// Not `{seq:<14}`: `std::fmt`'s width counts CHARS, so a chord bound to a
/// wide codepoint (CJK, an emoji — `parse_chord` accepts any lone codepoint
/// as a `KeyCode::Char`, and a project `./.norte/keymap.toml` carries no
/// trust) padded to 14 chars occupies more than 14 columns and shoves the
/// label out of its column. H3b promotes these lines into the help overlay
/// body, where they sit beside prose that IS cell-correct
/// (`crate::help_render`), so the drift is now visible side by side.
fn pad_to(seq: &str, col: usize) -> String {
    " ".repeat(col.saturating_sub(seq.width()))
}

/// Builds the help lines from the effective keymaps of the three screens:
/// EVERY binding of every screen — built or not (K3b) — with its catalogue
/// label, in real precedence order (what the key DOES, not what the preset
/// says).
///
/// A renderer over [`keysheet::sheet`](norte_frontend::keysheet::sheet)'s
/// rows: padding and dimming are this function's job, the DATA — which keys
/// exist and which of them this build can run — is the sheet's. An
/// unavailable row is dimmed with the theme's bare `Modifier::DIM` (the same
/// convention the which-key panel uses, K3a) and carries the short reason
/// `keysheet::sheet`'s doc names — interleaved where the sheet puts it, not
/// gathered into a section of leftovers, so a reader scanning the F-keys
/// finds `alt+f5` where it belongs.
#[must_use]
pub fn build(browse: &Effective, viewer: &Effective, dialog: &Effective) -> Vec<Line<'static>> {
    let lang = norte_i18n::active();
    let rows = sheet(&[
        (Screen::Browse, browse.clone()),
        (Screen::Viewer, viewer.clone()),
        (Screen::Dialog, dialog.clone()),
    ]);
    let mut out = Vec::new();
    for (screen, title) in [
        (Screen::Browse, t("help-section-browse")),
        (Screen::Viewer, t("help-section-viewer")),
    ] {
        out.push(Line::default());
        out.push(Line::raw(format!("── {title} ──")));
        for row in rows.iter().filter(|r| r.screen == screen) {
            out.push(sheet_row_line(row, lang));
        }
    }
    // #113: the `dialog.*` verbs were invisible in the app (overlay footers
    // FILTER by width — reordering in the column picker, for one, could only
    // be learnt from the docs). Help has no such budget: the whole `dialog`
    // effective, with a note that each overlay supports its own SUBSET
    // (allowlists).
    out.push(Line::default());
    out.push(Line::raw(format!("── {} ──", t("help-section-dialog"))));
    out.push(Line::raw(format!("  {}", t("help-dialog-note"))));
    for row in rows.iter().filter(|r| r.screen == Screen::Dialog) {
        out.push(sheet_row_line(row, lang));
    }
    out
}

/// One row of the generated keys page: the painted chord in its padded
/// column, then the catalogue label — dimmed, with the short reason, when
/// `row.avail` is not [`norte_frontend::keymap::Availability::Here`].
///
/// `command_label` (not the raw Fluent id and [`t`]) is deliberate: it is the SAME
/// router the which-key panel uses, and it falls back to the command NAME
/// rather than echoing a Fluent id — the common case for a `NotBuilt`
/// command, since nothing has written help text for one that does not exist.
fn sheet_row_line(row: &SheetRow, lang: norte_i18n::Lang) -> Line<'static> {
    let pad = pad_to(&row.chord, CHORD_COLUMN);
    let label = norte_frontend::whichkey::command_label(&row.command, lang);
    let reason = norte_frontend::keymap::short_unavailable_message(row.avail, lang);
    let text = if reason.is_empty() {
        format!("  {}{pad} {label}", row.chord)
    } else {
        format!("  {}{pad} {label} — {reason}", row.chord)
    };
    let style = if row.avail == norte_frontend::keymap::Availability::Here {
        Style::default()
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    Line::styled(text, style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_help::{Availability, ChordResolver, CommandText, render_command};
    use std::collections::HashMap;

    use crate::keymap::{COMMANDS, DIALOG_COMMANDS, parse_keymap, presets};
    use crate::palette::first_chord;

    /// Flattens painted lines into their text, for a `.contains()` check —
    /// this module's twin of `help_render`'s own test helper of the same
    /// name (K3b: `build` now returns styled `Line`s, not `String`s).
    fn flatten(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The orthodox preset, the only one the fixtures need.
    fn orthodox() -> crate::keymap::KeymapFile {
        presets()
            .into_iter()
            .find(|(n, _)| *n == "orthodox")
            .expect("preset orthodox")
            .1
    }

    /// Every command the TUI knows, browse and dialog alike.
    ///
    /// The `dialog` effective merges `[global]` too, so `DIALOG_COMMANDS`
    /// alone is not a sufficient vocabulary for it (`app.quit` lives in
    /// `COMMANDS`) — `build_for` would reject the preset outright.
    fn all_commands() -> Vec<&'static str> {
        COMMANDS
            .iter()
            .copied()
            .chain(DIALOG_COMMANDS.iter().copied())
            .collect()
    }

    /// The three effectives of the orthodox preset plus `layers`.
    fn effectives(layers: &[crate::keymap::KeymapFile]) -> (Effective, Effective, Effective) {
        let preset = orthodox();
        let known = all_commands();
        let build = |screen| {
            Effective::build_for(&preset, layers, &known, screen)
                .unwrap_or_else(|e| panic!("effective {screen:?}: {e}"))
        };
        (
            build(Screen::Browse),
            build(Screen::Viewer),
            build(Screen::Dialog),
        )
    }

    fn orthodox_resolver() -> TuiChords {
        let (browse, viewer, dialog) = effectives(&[]);
        TuiChords::new(&browse, &viewer, &dialog, norte_i18n::Lang::En)
    }

    /// A hostile chord as an untrusted PROJECT layer, bound in all three
    /// sections at once. TOML `\uXXXX` escape (spec v1.0.0): a raw C0 control
    /// such as BEL is invalid syntax inside a basic string, so the token is
    /// ALWAYS escaped.
    fn hostile_project_layer(token: char) -> crate::keymap::KeymapFile {
        let esc = format!("\\u{:04X}", token as u32);
        let mut layer = parse_keymap(&format!(
            "[pane]\nprepend_keymap = [{{ on = [\"{esc}\"], run = \"pane.copy\" }}]\n\
             [viewer]\nprepend_keymap = [{{ on = [\"{esc}\"], run = \"viewer.hex\" }}]\n\
             [dialog]\nprepend_keymap = [{{ on = [\"{esc}\"], run = \"dialog.approve\" }}]\n"
        ))
        .expect("the layer parses: any lone codepoint is a valid chord");
        // The exact threat `paint_chord`'s rustdoc names: a `./.norte`
        // keymap in a cloned repository, which carries no trust.
        layer.mark_project();
        layer
    }

    /// #113: the F1 help lists the WHOLE dialog section of the `dialog`
    /// effective — including the verbs the overlay footers omit for space
    /// (the column picker's reordering). The only in-app surface with no
    /// width budget.
    #[test]
    fn help_includes_the_dialog_verbs() {
        let (_, preset) = presets()
            .into_iter()
            .find(|(n, _)| *n == "orthodox")
            .expect("preset orthodox");
        let known: Vec<&str> = COMMANDS
            .iter()
            .copied()
            .chain(DIALOG_COMMANDS.iter().copied())
            .collect();
        let browse = Effective::build_for(&preset, &[], &known, Screen::Browse).unwrap();
        let viewer = Effective::build_for(&preset, &[], &known, Screen::Viewer).unwrap();
        let dialog = Effective::build_for(&preset, &[], &known, Screen::Dialog).unwrap();
        let lines = build(&browse, &viewer, &dialog);
        let all = flatten(&lines);
        assert!(
            all.contains(&t("help-section-dialog")),
            "dialog section present: {all}"
        );
        // The case that gave birth to #113: the picker's reordering verbs,
        // filtered from its footer (101 cells > 80), appear HERE with a
        // chord.
        assert!(
            all.contains(&t("dialog-cmd-move-up")),
            "move-up is learnable from help: {all}"
        );
        assert!(
            all.contains(&t("dialog-cmd-sort")),
            "sort is learnable from help: {all}"
        );
        // The note that each overlay supports its own subset comes along.
        assert!(all.contains(&t("help-dialog-note")));
    }

    /// The cheatsheet never paints a Fluent id at the reader.
    ///
    /// It did, and for a year: the dialog section asked `dialog-cmd-*` for
    /// EVERY binding of the dialog effective, which merges the preset's
    /// `[global]` section, so eight rows read `dialog-cmd-app-quit` and the
    /// like. `t` answers a missing message with its id, which is exactly why
    /// [`label_id`] routes by command prefix rather than by which list the
    /// caller happens to be walking. H3b ships these lines as the keyboard
    /// page of the new overlay, so the defect would have been promoted, not
    /// retired.
    #[test]
    fn the_cheatsheet_never_paints_a_fluent_id() {
        let (_, preset) = presets()
            .into_iter()
            .find(|(n, _)| *n == "orthodox")
            .expect("preset orthodox");
        let known: Vec<&str> = COMMANDS
            .iter()
            .copied()
            .chain(DIALOG_COMMANDS.iter().copied())
            .collect();
        let browse = Effective::build_for(&preset, &[], &known, Screen::Browse).unwrap();
        let viewer = Effective::build_for(&preset, &[], &known, Screen::Viewer).unwrap();
        let dialog = Effective::build_for(&preset, &[], &known, Screen::Dialog).unwrap();
        for line in build(&browse, &viewer, &dialog) {
            let text = flatten(std::slice::from_ref(&line));
            assert!(
                !text.contains("help-cmd-") && !text.contains("dialog-cmd-"),
                "a catalogue miss echoed its lookup key at the reader: {text:?}"
            );
        }
        // And the global commands that exposed it are still LISTED in the
        // dialog section — the fix is a better label, not a filtered row.
        let lines = build(&browse, &viewer, &dialog);
        let dialog_section = lines
            .iter()
            .skip_while(|l| !flatten(std::slice::from_ref(l)).contains(&t("help-section-dialog")))
            .fold(String::new(), |acc, l| {
                acc + &flatten(std::slice::from_ref(l)) + "\n"
            });
        assert!(
            dialog_section.contains(&t("help-cmd-app-quit")),
            "the global verbs reachable from a dialog stay visible: \
             {dialog_section}"
        );
    }

    /// The three effectives of `preset`, built against the TUI's REAL
    /// command vocabulary (`COMMANDS`/`DIALOG_COMMANDS`) rather than the
    /// preset's own — the same set `main.rs` builds production effectives
    /// with, and the reason a Total Commander preset's `pane.pack` comes out
    /// `NotBuilt` here instead of quietly resolving.
    fn build_effectives_of(preset: &str) -> (Effective, Effective, Effective) {
        build_effectives_sin(preset, "")
    }

    /// Like [`build_effectives_of`], but pretending the TUI does NOT
    /// implement `missing`.
    ///
    /// Exists because the dimmed-row test needs at least one unbuilt binding,
    /// and that used to be an accident: it depended on some catalogue command
    /// being left that no frontend implemented. Once the last one closed
    /// (`pane.copy-path`, #286) the test lost its subject and went red
    /// asserting it no longer tested anything — exactly what its own
    /// assertion said would happen. What is tested is the PAINTING of a
    /// dimmed row, so the gap is manufactured instead of waited for.
    fn build_effectives_sin(preset: &str, missing: &str) -> (Effective, Effective, Effective) {
        let (_, kf) = presets()
            .into_iter()
            .find(|(n, _)| *n == preset)
            .unwrap_or_else(|| panic!("preset {preset}"));
        let known: Vec<&str> = COMMANDS
            .iter()
            .copied()
            .chain(DIALOG_COMMANDS.iter().copied())
            .filter(|c| *c != missing)
            .collect();
        let browse = Effective::build_for(&kf, &[], &known, Screen::Browse).unwrap();
        let viewer = Effective::build_for(&kf, &[], &known, Screen::Viewer).unwrap();
        let dialog = Effective::build_for(&kf, &[], &known, Screen::Dialog).unwrap();
        (browse, viewer, dialog)
    }

    /// K3b: a preset with a `Planned` binding — Total Commander's `Alt+F5`
    /// packs (K2b, #131..#140) — is a DIMMED row carrying its reason, never a
    /// silently dropped one. `sheet_row_line` dims with the bare
    /// `Modifier::DIM` the which-key panel (K3a, `ui.rs::draw_which_key`)
    /// already uses, checked here at the LINE'S OWN style
    /// (`Line::styled` sets it there, not per-span).
    #[test]
    fn an_unbuilt_binding_comes_out_dimmed_with_its_reason() {
        let (browse, viewer, dialog) = build_effectives_sin("total-commander", "pane.pack");
        let lines = build(&browse, &viewer, &dialog);
        let dimmed: Vec<&Line<'_>> = lines
            .iter()
            .filter(|l| l.style.add_modifier.contains(Modifier::DIM))
            .collect();
        assert!(
            !dimmed.is_empty(),
            "total-commander must carry at least one unavailable row for \
             this test to mean anything"
        );
        for line in &dimmed {
            let text = flatten(std::slice::from_ref(line));
            assert!(
                text.contains('—'),
                "a dimmed row must carry its reason: {text:?}"
            );
        }
    }

    /// `build`'s row count can never drift from `keysheet::sheet`'s: it is a
    /// renderer over the sheet's rows plus a FIXED amount of scaffolding (a
    /// blank separator and a `── … ──` header per screen — three screens,
    /// six lines — plus the dialog note). A `sheet_row_line` call dropped or
    /// doubled anywhere in `build`'s three loops fails here.
    #[test]
    fn every_sheet_row_is_exactly_one_generated_line() {
        for preset in ["orthodox", "total-commander", "vim", "krusader"] {
            let (browse, viewer, dialog) = build_effectives_of(preset);
            let rows = sheet(&[
                (Screen::Browse, browse.clone()),
                (Screen::Viewer, viewer.clone()),
                (Screen::Dialog, dialog.clone()),
            ]);
            let lines = build(&browse, &viewer, &dialog);
            // Three screens × (blank separator + header) = 6, plus the
            // dialog note = 7.
            let scaffolding = 3 * 2 + 1;
            assert_eq!(
                lines.len() - scaffolding,
                rows.len(),
                "[{preset}] a generated line per sheet row, no more, no \
                 fewer: {} lines, {scaffolding} of them scaffolding, {} \
                 sheet rows",
                lines.len(),
                rows.len()
            );
        }
    }

    #[test]
    fn resolves_a_browse_command_to_its_effective_chord() {
        let r = orthodox_resolver();
        assert_eq!(r.chord("pane.copy").as_deref(), Some("F5"));
    }

    #[test]
    fn resolves_every_bound_command_whichever_screen_it_lives_in() {
        // The rule `ChordResolver::chord` documents: resolve a command in the
        // screen THAT COMMAND lives in. Swept over the WHOLE vocabulary
        // rather than sampled, so a resolver that dropped a screen — or one
        // built with its constructor arguments transposed — fails here
        // instead of silently reporting "no key" for a third of the app.
        let (browse, viewer, dialog) = effectives(&[]);
        assert!(
            first_chord("viewer.hex", &browse).is_none(),
            "the premise: a `viewer.*` command is NOT in the browse keymap, \
             which is why asking one screen is not enough"
        );
        let bound = |eff: &Effective, cmd: &str| eff.bindings().iter().any(|(_, c)| *c == cmd);
        let r = TuiChords::new(&browse, &viewer, &dialog, norte_i18n::Lang::En);
        let mut swept = 0_usize;
        for cmd in all_commands() {
            if bound(&browse, cmd) || bound(&viewer, cmd) || bound(&dialog, cmd) {
                swept += 1;
                assert!(
                    r.chord(cmd).is_some(),
                    "{cmd} is bound in some screen but the resolver has no key for it"
                );
            }
        }
        assert!(swept > 40, "the sweep must actually cover the app: {swept}");
        // Named samples, one per screen, so a regression says WHICH arm.
        assert_eq!(r.chord("pane.copy").as_deref(), Some("F5"));
        assert_eq!(r.chord("viewer.hex").as_deref(), Some("x"));
        assert_eq!(r.chord("dialog.approve").as_deref(), Some("y"));
    }

    #[test]
    fn an_unbound_command_has_no_chord_and_none_is_invented() {
        let r = orthodox_resolver();
        assert_eq!(r.chord("no.such.command"), None);
        assert_eq!(
            render_command("no.such.command", &r),
            CommandText::Name("no.such.command".to_owned()),
            "with no chord and no catalogue entry the chain names the command"
        );
    }

    #[test]
    fn a_missing_catalogue_entry_returns_blank_not_the_lookup_key() {
        let r = orthodox_resolver();
        assert_eq!(r.label("no.such.command"), "");
        // The regression this guards is a REAL command whose catalogue entry
        // is missing: `t_in` answers with the id, and a resolver that handed
        // that back would paint `help-cmd-pane-copy` at the reader AND stop
        // `render_command`'s chain from ever naming the command. Asserted by
        // NAME over the whole vocabulary, so the failure mode is what fails.
        for cmd in all_commands() {
            let label = r.label(cmd);
            assert!(
                !label.starts_with("help-cmd-") && !label.starts_with("dialog-cmd-"),
                "{cmd}: the lookup key leaked into the label: {label:?}"
            );
            assert!(!label.is_empty(), "{cmd} has no catalogue entry");
        }
        assert!(
            !r.label("dialog.approve").is_empty(),
            "dialog verbs read from `dialog-cmd-*`, not `help-cmd-*`"
        );
    }

    #[test]
    fn the_label_language_is_the_one_the_resolver_was_built_with() {
        // `lang` names the language of the TEXT, not just of a membership
        // check — a resolver built for `Es` must not answer in whatever the
        // process-global locale happens to be.
        let (browse, viewer, dialog) = effectives(&[]);
        let es = TuiChords::new(&browse, &viewer, &dialog, norte_i18n::Lang::Es);
        assert_eq!(
            es.label("pane.copy"),
            norte_i18n::t_in(norte_i18n::Lang::Es, "help-cmd-pane-copy")
        );
        assert_ne!(
            es.label("pane.copy"),
            orthodox_resolver().label("pane.copy"),
            "the two locales say different things, or this pins nothing"
        );
    }

    #[test]
    fn a_hostile_chord_is_masked_before_it_reaches_the_page() {
        // Encoding audit H1, resolver side. Every arm of the fill is
        // exercised: a project layer binds the hazard in `[pane]`, `[viewer]`
        // AND `[dialog]`, so a mask applied to only one screen fails here.
        for hazard in norte_testkit::corpus::hostile_chords() {
            let layer = hostile_project_layer(hazard.token);
            let (browse, viewer, dialog) = effectives(std::slice::from_ref(&layer));
            let r = TuiChords::new(&browse, &viewer, &dialog, norte_i18n::Lang::En);
            for cmd in ["pane.copy", "viewer.hex", "dialog.approve"] {
                let chord = r.chord(cmd).expect("bound by the layer");
                assert!(
                    !chord.chars().any(norte_encoding::is_terminal_hazard),
                    "[{}] {cmd}: raw hazard in a painted chord: {chord:?}",
                    hazard.id
                );
                // Guard against a vacuous pass: `prepend_keymap` must win
                // precedence over the preset's own key, or this would only be
                // asserting that `f5` is not hostile.
                assert_eq!(
                    chord, "\u{FFFD}",
                    "[{}] {cmd}: the prepended layer is what the resolver reports",
                    hazard.id
                );
            }
        }
    }

    #[test]
    fn the_f1_cheatsheet_masks_hostile_chords_too() {
        // `build`'s lines are painted with `Line::raw` (`ui.rs`, the F1
        // overlay), so an unmasked chord reaches the terminal verbatim: BEL
        // rings it, RLO reorders the line around it. A project keymap in a
        // cloned repository is enough to do it, which is why `build` goes
        // through `paint_chord` in EVERY loop — browse, viewer and dialog.
        for hazard in norte_testkit::corpus::hostile_chords() {
            let layer = hostile_project_layer(hazard.token);
            let (browse, viewer, dialog) = effectives(std::slice::from_ref(&layer));
            let lines = build(&browse, &viewer, &dialog);
            // Line by line, not over a join: `\n` is itself a C0 control, so
            // a joined haystack would flag the joiner. One line is exactly
            // what `Line::raw` receives.
            for line in &lines {
                let text = flatten(std::slice::from_ref(line));
                assert!(
                    !text.chars().any(norte_encoding::is_terminal_hazard),
                    "[{}] raw hazard on the F1 page: {text:?}",
                    hazard.id
                );
            }
            // Anti-vacuity: the hazard must actually have reached the page,
            // masked — three times over, one per section, so a `paint_chord`
            // dropped from any single loop fails here.
            assert_eq!(
                lines
                    .iter()
                    .map(|l| flatten(std::slice::from_ref(l)).matches('\u{FFFD}').count())
                    .sum::<usize>(),
                3,
                "[{}] one masked chord per screen section",
                hazard.id
            );
        }
    }

    /// Facts with no impediment at all: a lone file in a writable directory,
    /// same for the other pane.
    fn normal_facts() -> norte_frontend::availability::Facts {
        norte_frontend::availability::Facts {
            enterable: true,
            viewable: true,
            rename_single: true,
            source_read_only: false,
            dest_read_only: false,
            degraded: false,
            journalled: true,
            daemon: true,
            windowed: true,
        }
    }

    fn resolver_with(facts: norte_frontend::availability::Facts) -> TuiChords {
        orthodox_resolver().with_facts(facts)
    }

    /// Before freezing anything, the resolver dims nothing: it is the same
    /// fail-OPEN from the table (`norte_frontend::availability::verdict`)
    /// applied to the facts — denying for not having looked would be worse
    /// than offering and failing honestly.
    #[test]
    fn with_no_frozen_facts_nothing_is_dimmed() {
        let r = orthodox_resolver();
        assert_eq!(r.availability("pane.copy"), Availability::Available);
        assert_eq!(r.availability("nav.enter"), Availability::Available);
        assert_eq!(r.availability("pane.view"), Availability::Available);
    }

    /// H3d: the row of a command that cannot run RIGHT NOW comes out dimmed
    /// with its reason, instead of promising something the app is going to
    /// reject.
    #[test]
    fn inside_a_zip_copying_here_is_forbidden() {
        let r = resolver_with(norte_frontend::availability::Facts {
            dest_read_only: true,
            ..normal_facts()
        });
        assert_eq!(
            r.availability("pane.copy").reason(),
            Some(norte_help::Reason::ReadOnlyBackend)
        );
    }

    /// And the case this phase exists to NOT break: a command that can run
    /// stays available. Help that over-dims is as useless as one that dims
    /// nothing.
    #[test]
    fn what_can_run_stays_available() {
        let r = resolver_with(normal_facts());
        assert!(r.availability("pane.copy").is_available());
        assert!(r.availability("app.quit").is_available());
    }

    /// Facts are FROZEN on open: `with_facts` returns another resolver
    /// instead of mutating the one the view is using, so an open page cannot
    /// change verdict under the reader's cursor.
    #[test]
    fn freezing_facts_does_not_touch_the_original_resolver() {
        let before = orthodox_resolver();
        let inside_a_zip = before.with_facts(norte_frontend::availability::Facts {
            source_read_only: true,
            ..normal_facts()
        });
        assert!(!inside_a_zip.availability("pane.delete").is_available());
        assert!(
            before.availability("pane.delete").is_available(),
            "the original resolver stayed intact"
        );
        // And the chords travel with the copy: freezing facts must not cost
        // the reader's key.
        assert_eq!(inside_a_zip.chord("pane.copy"), before.chord("pane.copy"));
    }

    /// H3e: the row of a command from a DISABLED plugin comes out dimmed,
    /// with its reason. It is H3d's bug in its new form — with no `plugin:`
    /// arm, the key falls into the table's fail-OPEN wildcard and the row
    /// lights up unconditionally over a `plugin.run_command` that is going to
    /// reject it.
    #[test]
    fn a_disabled_plugins_command_reaches_the_page_dimmed() {
        let r = resolver_with(normal_facts())
            .with_plugins(std::collections::BTreeSet::new(), HashMap::new());
        assert_eq!(
            r.availability("plugin:acme.ftp:sync").reason(),
            Some(norte_help::Reason::PluginInactive)
        );
    }

    #[test]
    fn an_enabled_plugins_command_is_not_dimmed() {
        let r = resolver_with(normal_facts()).with_plugins(
            ["acme.ftp".to_owned()].into_iter().collect(),
            HashMap::new(),
        );
        assert!(r.availability("plugin:acme.ftp:sync").is_available());
    }

    /// The plugin snapshot travels with the facts freeze, and vice versa: the
    /// two constructors are called in sequence (`main::open_contextual_help`
    /// freezes and then plugs in the snapshot) and `App::freeze_help_facts`
    /// freezes again on every pane refresh. If `with_facts` did not carry the
    /// set along, that re-freeze would turn off every plugin row mid-read.
    #[test]
    fn freezing_facts_does_not_lose_the_plugin_snapshot() {
        let r = orthodox_resolver()
            .with_plugins(
                ["acme.ftp".to_owned()].into_iter().collect(),
                HashMap::new(),
            )
            .with_facts(normal_facts());
        assert!(r.availability("plugin:acme.ftp:sync").is_available());
        // And vice versa: a snapshot taken afterward keeps the facts.
        let r = resolver_with(norte_frontend::availability::Facts {
            dest_read_only: true,
            ..normal_facts()
        })
        .with_plugins(std::collections::BTreeSet::new(), HashMap::new());
        assert_eq!(
            r.availability("pane.copy").reason(),
            Some(norte_help::Reason::ReadOnlyBackend)
        );
    }

    /// With no snapshot (a freshly built resolver, or one a hot-reload
    /// rebuilt) NO plugin is active: offering a row that
    /// `plugin.run_command` would reject is the worse of the two errors.
    #[test]
    fn with_no_plugin_snapshot_no_plugin_row_is_offered() {
        let r = orthodox_resolver();
        assert_eq!(
            r.availability("plugin:acme.ftp:sync").reason(),
            Some(norte_help::Reason::PluginInactive)
        );
    }

    /// A title map shaped like `plugin.list`'s output.
    fn titles(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn resolver_with_titles(pairs: &[(&str, &str)]) -> TuiChords {
        resolver_with(normal_facts()).with_plugins(
            ["org.norte.demo".to_owned()].into_iter().collect(),
            titles(pairs),
        )
    }

    /// H3e: a plugin command's row carries the NAME the manifest gives it,
    /// not its dispatch key.
    ///
    /// The defect showed on screen: `org.norte.demo`'s page painted
    /// `plugin:org.norte.demo:greet` where the manifest says "Greet the
    /// world", and the palette — looking at the same data — painted the
    /// title. The chain is `render_command` → no chord → `label_or_id` →
    /// `label` blank → fallback to the id, and for a `plugin:` key that
    /// fallback is GUARANTEED: the app's Fluent catalogue cannot have an
    /// entry for a command a third party declared.
    #[test]
    fn a_plugin_row_carries_the_manifests_name() {
        let r = resolver_with_titles(&[("plugin:org.norte.demo:greet", "Greet the world")]);
        assert_eq!(r.label("plugin:org.norte.demo:greet"), "Greet the world");
    }

    /// And the inline `{{cmd:…}}` mark says the SAME thing: `render_command`
    /// and `rows_of` share `label_or_id` precisely so the prose and the row
    /// below cannot name a command two different ways.
    #[test]
    fn the_inline_mark_and_the_row_say_the_same_thing() {
        let r = resolver_with_titles(&[("plugin:org.norte.demo:greet", "Greet the world")]);
        assert_eq!(
            render_command("plugin:org.norte.demo:greet", &r),
            CommandText::Name("Greet the world".to_owned()),
            "with no chord (a plugin command is not in the keymap) the mark \
             NAMES the command, and names it as the manifest does"
        );
    }

    /// A key the snapshot does not name keeps today's behavior: it falls back
    /// to the id, NEVER to blank. A row with its dispatch key is poor; a row
    /// with no text at all is worse.
    #[test]
    fn a_key_with_no_title_in_the_snapshot_still_paints_its_id() {
        let r = resolver_with_titles(&[("plugin:org.norte.demo:greet", "Greet the world")]);
        assert_eq!(
            render_command("plugin:org.norte.demo:otro", &r),
            CommandText::Name("plugin:org.norte.demo:otro".to_owned()),
            "the row still carries its key: poor, but readable — a row with \
             no text at all would be worse"
        );
    }

    /// …and that key is delivered MASKED. `norte_help::label_or_id`'s
    /// fallback paints the raw id, and a plugin id is third-party text: today
    /// `is_own_command` already rejects a key with controls or bidi at PARSE
    /// time, but `Topic` is a struct with public fields and
    /// `install_plugin_topic` does not check it again. The painter cannot
    /// depend on a filter that lives three crates away.
    #[test]
    fn a_hostile_plugin_key_is_not_painted_raw() {
        let r = resolver_with_titles(&[]);
        let hostile = "plugin:acme.ftp:\u{202E}x\u{200B}y";
        let painted = r.label(hostile);
        assert!(
            !painted.chars().any(norte_encoding::is_terminal_hazard),
            "no terminal hazards: {painted:?}"
        );
        assert!(painted.contains('\u{FFFD}'), "anti-vacuity: {painted:?}");
        assert_eq!(
            render_command(hostile, &r),
            CommandText::Name(painted),
            "and it is what norte-help's chain ends up naming"
        );
        // A MALFORMED key (which `plugin_of_command` rejects) too: the
        // question "is this third-party text?" is deliberately looser than
        // "does this identify a command?".
        let malformed = r.label("plugin:\u{202E}");
        assert!(!malformed.chars().any(norte_encoding::is_terminal_hazard));
    }

    /// A hostile title arrives MASKED and BOUNDED — the masking happens at
    /// the entry point (`crate::app::plugin_label`), not while painting,
    /// because this resolver hands its strings straight to the painter.
    #[test]
    fn a_hostile_title_arrives_masked_and_bounded() {
        let hostile = format!("Gre\u{202E}et\u{200B}{}", "x".repeat(5_000));
        let r = resolver_with_titles(&[(
            "plugin:org.norte.demo:greet",
            &crate::app::plugin_label(&hostile),
        )]);
        let label = r.label("plugin:org.norte.demo:greet");
        assert!(
            !label.chars().any(norte_encoding::is_terminal_hazard),
            "no terminal hazards: {label:?}"
        );
        assert!(
            label.chars().count() <= crate::app::PLUGIN_NAME_WIRE_CAP + 1,
            "bounded (+1 for the truncation mark): {} chars",
            label.chars().count()
        );
        assert!(
            label.ends_with('…'),
            "and the truncation is MARKED: {label:?}"
        );
        assert!(label.contains('\u{FFFD}'), "anti-vacuity: {label:?}");
    }

    /// A binary's own command is NOT read from the plugin map: its label
    /// still comes from the Fluent catalogue, whatever the map says.
    ///
    /// `App::freeze_help_plugins` cannot produce such a key — it sets the
    /// prefix itself — but `with_plugins` is PUBLIC and the map is born from
    /// data that crossed the wire, so the guard is structural: a binary
    /// command's label must not be overridable even in principle. This test
    /// pins the shape of the failure, not one instance of it.
    #[test]
    fn a_binary_command_is_not_read_from_the_plugin_map() {
        let r = resolver_with_titles(&[("pane.copy", "IMPOSTOR")]);
        assert_eq!(
            r.label("pane.copy"),
            norte_i18n::t_in(norte_i18n::Lang::En, "help-cmd-pane-copy"),
        );
        assert_ne!(r.label("pane.copy"), "IMPOSTOR");
        // And a MALFORMED `plugin:` key is not read from the map either:
        // `plugin_of_command` rejects it, same as the availability arm dims
        // it fail-closed. One single definition of "this IDENTIFIES a
        // command". It falls to the safe fallback, which returns the key
        // itself, masked — never the title the map tried to associate with
        // it.
        let r = resolver_with_titles(&[("plugin:", "IMPOSTOR"), ("plugin:x", "IMPOSTOR")]);
        assert_eq!(r.label("plugin:"), "plugin:");
        assert_eq!(r.label("plugin:x"), "plugin:x");
    }

    /// Titles travel with the facts re-freeze, like the active set: the
    /// refresh funnel (`main::after_panes_refresh`) would otherwise override
    /// them, and an open page would lose its rows' names mid-read.
    #[test]
    fn refreezing_facts_does_not_lose_the_titles() {
        let r = resolver_with_titles(&[("plugin:org.norte.demo:greet", "Greet the world")])
            .with_facts(norte_frontend::availability::Facts {
                dest_read_only: true,
                ..normal_facts()
            });
        assert_eq!(r.label("plugin:org.norte.demo:greet"), "Greet the world");
    }
}
