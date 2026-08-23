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

    /// #113: la ayuda F1 lista la sección de diálogos COMPLETA del efectivo
    /// `dialog` — incluidos los verbos que los pies de overlay omiten por
    /// espacio (reordenación del picker de columnas). Única superficie
    /// in-app sin presupuesto de ancho.
    #[test]
    fn la_ayuda_incluye_los_verbos_dialog() {
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
            "sección de diálogos presente: {all}"
        );
        // El caso que parió #113: los verbos de reordenación del picker,
        // filtrados de su pie (101 celdas > 80), aparecen AQUÍ con chord.
        assert!(
            all.contains(&t("dialog-cmd-move-up")),
            "move-up aprendible desde la ayuda: {all}"
        );
        assert!(
            all.contains(&t("dialog-cmd-sort")),
            "sort aprendible desde la ayuda: {all}"
        );
        // La nota de que cada overlay soporta su subconjunto acompaña.
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

    /// Como [`build_effectives_of`], pero fingiendo que el TUI NO implementa
    /// `ausente`.
    ///
    /// Existe porque el test de la fila atenuada necesita que haya al menos
    /// una atadura sin construir, y eso era un accidente: dependía de que
    /// quedara algún comando del catálogo que ningún frontend hiciera. Al
    /// cerrarse el último (`pane.copy-path`, #286) el test se quedó sin
    /// sujeto y se puso rojo afirmando que ya no probaba nada — que es
    /// exactamente lo que su propia aserción decía que pasaría. Lo que se
    /// prueba es el PINTADO de una fila atenuada, así que el hueco se fabrica
    /// en vez de esperarlo.
    fn build_effectives_sin(preset: &str, ausente: &str) -> (Effective, Effective, Effective) {
        let (_, kf) = presets()
            .into_iter()
            .find(|(n, _)| *n == preset)
            .unwrap_or_else(|| panic!("preset {preset}"));
        let known: Vec<&str> = COMMANDS
            .iter()
            .copied()
            .chain(DIALOG_COMMANDS.iter().copied())
            .filter(|c| *c != ausente)
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
    fn un_binding_no_construido_sale_atenuado_y_con_su_razon() {
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
    fn cada_fila_del_sheet_es_exactamente_una_linea_generada() {
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

    /// Facts sin ningún impedimento: un fichero suelto en un directorio
    /// escribible, con el otro pane igual.
    fn facts_normales() -> norte_frontend::availability::Facts {
        norte_frontend::availability::Facts {
            enterable: true,
            viewable: true,
            rename_single: true,
            source_read_only: false,
            dest_read_only: false,
            degraded: false,
            journalled: true,
        }
    }

    fn resolver_con(facts: norte_frontend::availability::Facts) -> TuiChords {
        orthodox_resolver().with_facts(facts)
    }

    /// Antes de congelar nada, el resolver no atenúa: es el mismo fail-OPEN de
    /// la tabla (`norte_frontend::availability::verdict`) aplicado a los hechos
    /// — negar por no haber mirado sería peor que ofrecer y fallar honesto.
    #[test]
    fn sin_hechos_congelados_no_se_atenua_nada() {
        let r = orthodox_resolver();
        assert_eq!(r.availability("pane.copy"), Availability::Available);
        assert_eq!(r.availability("nav.enter"), Availability::Available);
        assert_eq!(r.availability("pane.view"), Availability::Available);
    }

    /// H3d: la fila de un comando que no puede correr AHORA sale atenuada y
    /// con su razón, en vez de prometer algo que la app va a rechazar.
    #[test]
    fn dentro_de_un_zip_copiar_hacia_aqui_esta_vetado() {
        let r = resolver_con(norte_frontend::availability::Facts {
            dest_read_only: true,
            ..facts_normales()
        });
        assert_eq!(
            r.availability("pane.copy").reason(),
            Some(norte_help::Reason::ReadOnlyBackend)
        );
    }

    /// Y el caso que la fase existe para NO romper: un comando que sí puede
    /// correr sigue disponible. Una ayuda que atenúa de más es tan inútil
    /// como una que no atenúa nada.
    #[test]
    fn lo_que_puede_correr_sigue_disponible() {
        let r = resolver_con(facts_normales());
        assert!(r.availability("pane.copy").is_available());
        assert!(r.availability("app.quit").is_available());
    }

    /// Los hechos se CONGELAN al abrir: `with_facts` devuelve otro resolver en
    /// vez de mutar el que la vista está usando, así que una página abierta no
    /// puede cambiar de veredicto bajo el cursor del lector.
    #[test]
    fn congelar_los_hechos_no_toca_el_resolver_de_partida() {
        let before = orthodox_resolver();
        let inside_a_zip = before.with_facts(norte_frontend::availability::Facts {
            source_read_only: true,
            ..facts_normales()
        });
        assert!(!inside_a_zip.availability("pane.delete").is_available());
        assert!(
            before.availability("pane.delete").is_available(),
            "el resolver de partida siguió intacto"
        );
        // Y los chords viajan con la copia: congelar hechos no puede costar la
        // tecla del lector.
        assert_eq!(inside_a_zip.chord("pane.copy"), before.chord("pane.copy"));
    }

    /// H3e: la fila de un comando de un plugin APAGADO sale atenuada, con su
    /// razón. Es el fallo de H3d en su forma nueva — sin el brazo `plugin:`,
    /// la clave cae en el comodín fail-OPEN de la tabla y la fila se enciende
    /// incondicionalmente sobre un `plugin.run_command` que va a rechazarla.
    #[test]
    fn un_comando_de_plugin_apagado_llega_atenuado_a_la_pagina() {
        let r = resolver_con(facts_normales())
            .with_plugins(std::collections::BTreeSet::new(), HashMap::new());
        assert_eq!(
            r.availability("plugin:acme.ftp:sync").reason(),
            Some(norte_help::Reason::PluginInactive)
        );
    }

    #[test]
    fn un_comando_de_plugin_encendido_no_se_atenua() {
        let r = resolver_con(facts_normales()).with_plugins(
            ["acme.ftp".to_owned()].into_iter().collect(),
            HashMap::new(),
        );
        assert!(r.availability("plugin:acme.ftp:sync").is_available());
    }

    /// La foto de plugins viaja con el congelado de hechos, y al revés: los
    /// dos constructores se llaman en secuencia (`main::open_contextual_help`
    /// congela y luego enchufa la foto) y `App::freeze_help_facts` vuelve a
    /// congelar en cada refresco de panes. Si `with_facts` no arrastrara el
    /// conjunto, ese re-congelado apagaría todas las filas de plugin a mitad
    /// de lectura.
    #[test]
    fn congelar_los_hechos_no_pierde_la_foto_de_plugins() {
        let r = orthodox_resolver()
            .with_plugins(
                ["acme.ftp".to_owned()].into_iter().collect(),
                HashMap::new(),
            )
            .with_facts(facts_normales());
        assert!(r.availability("plugin:acme.ftp:sync").is_available());
        // Y al revés: la foto tomada después conserva los hechos.
        let r = resolver_con(norte_frontend::availability::Facts {
            dest_read_only: true,
            ..facts_normales()
        })
        .with_plugins(std::collections::BTreeSet::new(), HashMap::new());
        assert_eq!(
            r.availability("pane.copy").reason(),
            Some(norte_help::Reason::ReadOnlyBackend)
        );
    }

    /// Sin foto (un resolver recién construido, o uno que un hot-reload
    /// rehízo) NINGÚN plugin está activo: ofrecer una fila que
    /// `plugin.run_command` rechazaría es el peor de los dos errores.
    #[test]
    fn sin_foto_de_plugins_ninguna_fila_de_plugin_se_ofrece() {
        let r = orthodox_resolver();
        assert_eq!(
            r.availability("plugin:acme.ftp:sync").reason(),
            Some(norte_help::Reason::PluginInactive)
        );
    }

    /// Un mapa de títulos con la forma que sale de `plugin.list`.
    fn titles(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn resolver_con_titulos(pairs: &[(&str, &str)]) -> TuiChords {
        resolver_con(facts_normales()).with_plugins(
            ["org.norte.demo".to_owned()].into_iter().collect(),
            titles(pairs),
        )
    }

    /// H3e: la fila de un comando de plugin lleva el NOMBRE que le da el
    /// manifiesto, no su clave de despacho.
    ///
    /// El defecto se veía en pantalla: la página de `org.norte.demo` pintaba
    /// `plugin:org.norte.demo:greet` donde el manifiesto dice «Greet the
    /// world», y la paleta —mirando los mismos datos— pintaba el título. La
    /// cadena es `render_command` → sin chord → `label_or_id` → `label` en
    /// blanco → repliegue al id, y para una clave `plugin:` ese repliegue está
    /// GARANTIZADO: el catálogo Fluent de la app no puede tener una entrada
    /// para un comando que declaró un tercero.
    #[test]
    fn una_fila_de_plugin_lleva_el_nombre_del_manifiesto() {
        let r = resolver_con_titulos(&[("plugin:org.norte.demo:greet", "Greet the world")]);
        assert_eq!(r.label("plugin:org.norte.demo:greet"), "Greet the world");
    }

    /// Y la marca `{{cmd:…}}` en línea dice LO MISMO: `render_command` y
    /// `rows_of` comparten `label_or_id` precisamente para que la prosa y la
    /// fila de debajo no puedan nombrar un comando de dos maneras.
    #[test]
    fn la_marca_en_linea_y_la_fila_dicen_lo_mismo() {
        let r = resolver_con_titulos(&[("plugin:org.norte.demo:greet", "Greet the world")]);
        assert_eq!(
            render_command("plugin:org.norte.demo:greet", &r),
            CommandText::Name("Greet the world".to_owned()),
            "sin chord (un comando de plugin no está en el keymap) la marca \
             NOMBRA el comando, y lo nombra como el manifiesto"
        );
    }

    /// Una clave que la foto no nombra conserva el comportamiento de hoy: se
    /// repliega al id, JAMÁS a blanco. Una fila con su clave de despacho es
    /// pobre; una fila sin texto ninguno es peor.
    #[test]
    fn una_clave_sin_titulo_en_la_foto_sigue_pintando_su_id() {
        let r = resolver_con_titulos(&[("plugin:org.norte.demo:greet", "Greet the world")]);
        assert_eq!(
            render_command("plugin:org.norte.demo:otro", &r),
            CommandText::Name("plugin:org.norte.demo:otro".to_owned()),
            "la fila sigue llevando su clave: pobre, pero legible — una fila \
             sin texto ninguno sería peor"
        );
    }

    /// …y esa clave se entrega ENMASCARADA. El repliegue de
    /// `norte_help::label_or_id` pinta el id crudo, y un id de plugin es texto
    /// de tercero: hoy `is_own_command` ya rechaza una clave con controles o
    /// bidi al PARSEAR, pero `Topic` es un struct con campos públicos y
    /// `install_plugin_topic` no lo vuelve a comprobar. El pintor no puede
    /// depender de un filtro que vive tres crates más allá.
    #[test]
    fn una_clave_de_plugin_hostil_no_se_pinta_cruda() {
        let r = resolver_con_titulos(&[]);
        let hostile = "plugin:acme.ftp:\u{202E}x\u{200B}y";
        let painted = r.label(hostile);
        assert!(
            !painted.chars().any(norte_encoding::is_terminal_hazard),
            "sin peligros de terminal: {painted:?}"
        );
        assert!(painted.contains('\u{FFFD}'), "anti-vacuidad: {painted:?}");
        assert_eq!(
            render_command(hostile, &r),
            CommandText::Name(painted),
            "y es lo que la cadena de `norte-help` acaba nombrando"
        );
        // Una clave MALFORMADA (que `plugin_of_command` rechaza) también: la
        // pregunta «¿esto es texto de tercero?» es más laxa que «¿esto
        // identifica un comando?», a propósito.
        let malformed = r.label("plugin:\u{202E}");
        assert!(!malformed.chars().any(norte_encoding::is_terminal_hazard));
    }

    /// Un título hostil llega ENMASCARADO y ACOTADO — el enmascarado ocurre en
    /// el punto de entrada (`crate::app::plugin_label`), no al pintar, porque
    /// este resolver entrega sus cadenas directas al pintor.
    #[test]
    fn un_titulo_hostil_llega_enmascarado_y_acotado() {
        let hostile = format!("Gre\u{202E}et\u{200B}{}", "x".repeat(5_000));
        let r = resolver_con_titulos(&[(
            "plugin:org.norte.demo:greet",
            &crate::app::plugin_label(&hostile),
        )]);
        let label = r.label("plugin:org.norte.demo:greet");
        assert!(
            !label.chars().any(norte_encoding::is_terminal_hazard),
            "sin peligros de terminal: {label:?}"
        );
        assert!(
            label.chars().count() <= crate::app::PLUGIN_NAME_WIRE_CAP + 1,
            "acotado (+1 por la marca de recorte): {} chars",
            label.chars().count()
        );
        assert!(label.ends_with('…'), "y el recorte se MARCA: {label:?}");
        assert!(label.contains('\u{FFFD}'), "anti-vacuidad: {label:?}");
    }

    /// Un comando del binario NO se lee del mapa de plugins: su etiqueta sigue
    /// saliendo del catálogo Fluent, pase lo que pase en el mapa.
    ///
    /// `App::freeze_help_plugins` no puede producir una clave así — pone el
    /// prefijo él mismo — pero `with_plugins` es PÚBLICO y el mapa nace de
    /// datos que cruzaron el wire, así que la guarda es estructural: la
    /// etiqueta de un comando del binario no debe poder sobrescribirse ni en
    /// principio. Este test pina la forma del fallo, no una instancia.
    #[test]
    fn un_comando_del_binario_no_se_lee_del_mapa_de_plugins() {
        let r = resolver_con_titulos(&[("pane.copy", "IMPOSTOR")]);
        assert_eq!(
            r.label("pane.copy"),
            norte_i18n::t_in(norte_i18n::Lang::En, "help-cmd-pane-copy"),
        );
        assert_ne!(r.label("pane.copy"), "IMPOSTOR");
        // Y una clave `plugin:` MALFORMADA tampoco se lee del mapa:
        // `plugin_of_command` la rechaza, igual que el brazo de disponibilidad
        // la atenúa fail-closed. Una sola definición de «esto IDENTIFICA un
        // comando». Cae al repliegue seguro, que devuelve la propia clave
        // enmascarada — jamás el título que el mapa pretendía asociarle.
        let r = resolver_con_titulos(&[("plugin:", "IMPOSTOR"), ("plugin:x", "IMPOSTOR")]);
        assert_eq!(r.label("plugin:"), "plugin:");
        assert_eq!(r.label("plugin:x"), "plugin:x");
    }

    /// Los títulos viajan con el re-congelado de hechos, como el conjunto de
    /// activos: el embudo de refresco (`main::after_panes_refresh`) los pisaría
    /// si no, y una página abierta perdería los nombres de sus filas a mitad de
    /// lectura.
    #[test]
    fn recongelar_los_hechos_no_pierde_los_titulos() {
        let r = resolver_con_titulos(&[("plugin:org.norte.demo:greet", "Greet the world")])
            .with_facts(norte_frontend::availability::Facts {
                dest_read_only: true,
                ..facts_normales()
            });
        assert_eq!(r.label("plugin:org.norte.demo:greet"), "Greet the world");
    }
}
