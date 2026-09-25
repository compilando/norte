//! Dialog overlays' footer hints (H1 T3, issue #24 — CLOSES): same pattern as
//! F1 help (`help.rs`), but per command of the `dialog` context. A hint is the
//! JOIN of the commands SUPPORTED by a given overlay × the EFFECTIVE `dialog`
//! keymap × the `dialog-cmd-*` Fluent labels — never a hand-kept static
//! string: a rebind can no longer desync the footer from what the key really
//! does.

use std::collections::HashSet;

use norte_i18n::t;

use crate::keymap::{Effective, dialog_hint_id};

/// Commands considered self-evident navigation (MAJOR-1, H1 close): arrows
/// and paging are universal — every terminal user already knows what they
/// do — so they cost footer width without paying for it in clarity. Excluded
/// ONLY from the generated HINT text via [`without_navigation`]; the
/// dispatch allowlists in `app.rs` (`ALLOW_PICKER`/`ALLOW_EXTENSIONS`/
/// `ALLOW_NAV_HOTLIST`) are untouched — the keys still work, they are just
/// not spelled out in the footer.
const NAVIGATION_HINT_EXCLUDED: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.page-up",
    "dialog.page-down",
    "dialog.top",
    "dialog.bottom",
    "dialog.section-prev",
    "dialog.section-next",
];

/// Filters a SUPPORTED allowlist down to the commands worth spelling out in
/// a footer hint (MAJOR-1): drops [`NAVIGATION_HINT_EXCLUDED`] entries. Used
/// only by [`DialogHints::build`] for the NON-MODAL overlays (theme picker,
/// extensions, hotlist popup) — 80-column footers were being cut mid-word
/// (`snapshots_ui__snapshot_popup_hotlist.snap` showed `[d] borr┘`) because
/// universally-known arrow keys were eating the scarce width. Modal dialogs
/// (confirm/collision/approval/trust-host) never include navigation commands
/// in their allowlists to begin with, so this is a no-op for them.
#[must_use]
pub(crate) fn without_navigation<'a>(supported: &'a [&'a str]) -> Vec<&'a str> {
    supported
        .iter()
        .copied()
        .filter(|c| !NAVIGATION_HINT_EXCLUDED.contains(c))
        .collect()
}

/// The HELP overlay's printable verbs, in the PRIORITY order its footer
/// offers them.
///
/// The help overlay is full-screen and cannot grow, so it is the one footer
/// whose hint has to be CUT — `ui::fit_hint_groups` drops whole
/// `[chord] label` groups from the TAIL and marks the loss with a `…`. That
/// mechanism is what decides here: this list is offered WHOLE and the width
/// takes what it takes, so a 113-column terminal shows all five groups and an
/// 80-column one keeps the three at the head.
///
/// The order is therefore a ranking of what a reader cannot guess:
///
/// 1. `dialog.filter` — nothing else on screen suggests the page is
///    searchable;
/// 2. `dialog.back` — the only way out of a link, and the overlay's history is
///    invisible;
/// 3. `dialog.pane` — the only way INTO the body, where `Enter` runs commands
///    that touch the filesystem;
/// 4. `dialog.confirm`, 5. `dialog.cancel` — the UNIVERSAL overlay keys, which
///    the `help` topic also spells out in prose. Last because they are the
///    ones a reader already knows, not because they are unimportant.
///
/// It was a fixed EXCLUSION of the last two before, which honoured the
/// 80-column frame by hiding `[enter]`/`[esc]` on every frame, wide ones
/// included. `ALLOW_HELP` and the dispatch in `app.rs` are untouched either
/// way — this is the printed hint only.
///
/// **Paging IS included**, unlike in the other overlays. In an options list
/// the arrows are taken for granted and the footer is narrow; here the body
/// is two hundred lines of PROSE in a twenty-line window, and there was
/// nothing on screen saying how to scroll down through it. `dialog.up`/`down`
/// stay out: on this screen they move the cursor between runnable rows, and
/// that is discoverable on its own — scrolling through text is not.
///
/// `help_priority_covers_every_printable_verb` pins that this list stays a
/// complete projection of `ALLOW_HELP`: a verb added there has to be ranked
/// here, not left mute forever.
const HELP_HINT_PRIORITY: &[&str] = &[
    "dialog.pane",
    "dialog.page-down",
    "dialog.page-up",
    "dialog.filter",
    "dialog.back",
    "dialog.confirm",
    "dialog.cancel",
];

/// A verb's label IN HELP, which is not always the same verb's label in a
/// dialog.
///
/// `dialog.pane` is the motivating case: in a modal it means "the other
/// pane", and here it means "index ↔ content" — painting "other pane" over an
/// overlay that has no panes tells the reader something they cannot do, and
/// hides the one thing they need to reach the text. Paging is also worded
/// differently: here it does not page a list, it scrolls the page.
fn help_hint_id(cmd: &str) -> String {
    match cmd {
        "dialog.pane" => "help-cmd-pane".to_owned(),
        "dialog.page-up" => "help-cmd-page-up".to_owned(),
        "dialog.page-down" => "help-cmd-page-down".to_owned(),
        other => dialog_hint_id(other),
    }
}

/// Footer hint for an overlay: the join of its SUPPORTED dialog commands ×
/// the effective dialog keymap × Fluent labels — same invariant as F1 help
/// (#24: a rebind can never desync the hint again).
///
/// The ORDER comes from the EFFECTIVE keymap (`eff.bindings()`, in real
/// precedence), not from the `supported` array: a command with NO binding in
/// the effective (rebound to nothing, or simply never bound in an exotic
/// layer) is left out — honest, no phantom key. The FIRST chord of each
/// command in that order is the one shown (e.g. in `vim`, `up`/`down` beat
/// the `k`/`j` added later in the preset).
#[must_use]
pub fn dialog_hints(supported: &[&str], eff: &Effective) -> String {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    for (chord, cmd) in eff.bindings() {
        if supported.contains(&cmd) && seen.insert(cmd) {
            // RENDER-side duty (encoding audit H1): `chord` comes from a
            // potentially hostile keymap (`./.norte/keymap.toml`, a PROJECT
            // layer with no trust — `parse_chord` accepts ANY lone codepoint
            // as a `KeyCode::Char`). `Chord`'s `Display` writes it raw and
            // lowercase ON PURPOSE (logs/debug want the real chord); this
            // hint DOES get painted on security modals' footers, so
            // `paint_chord` — the ONLY presentation home for a chord, shared
            // with the palette and F1 help — masks FIRST and only then
            // writes the key the way the documentation spells it (`F5`, not
            // `f5`).
            let chord = crate::keymap::paint_chord(&chord);
            out.push(format!("[{chord}] {}", t(&dialog_hint_id(cmd))));
        }
    }
    out.join(" ")
}

/// Same hint, but in the order the CALLER gives instead of the effective
/// keymap's.
///
/// [`dialog_hints`] follows the effective on purpose (a footer that lists keys
/// in the order they resolve), and every overlay that can GROW to fit its hint
/// wants exactly that. The help overlay cannot grow: its footer is cut by
/// `ui::fit_hint_groups`, which drops groups from the TAIL — so the order is
/// what decides which verbs survive a narrow frame, and that is a
/// presentation ranking (`HELP_HINT_PRIORITY`, private to this module), not a
/// keymap fact.
///
/// A command with no binding in the effective is skipped, same as
/// [`dialog_hints`] — no phantom key. `order` is expected to be duplicate-free
/// (a constant ranking); a repeat would simply print its group twice.
#[must_use]
pub fn dialog_hints_in_order(order: &[&str], eff: &Effective) -> String {
    hints_in_order_with(order, eff, dialog_hint_id)
}

/// Like [`dialog_hints_in_order`], with whichever label `label` decides.
fn hints_in_order_with(order: &[&str], eff: &Effective, label: impl Fn(&str) -> String) -> String {
    order
        .iter()
        .filter_map(|cmd| {
            // `first_chord` is the same join `dialog_hints` performs (first
            // chord of the command in the effective's precedence order),
            // through the same `paint_chord` presentation home.
            norte_frontend::palette::first_chord(cmd, eff)
                .map(|chord| format!("[{chord}] {}", t(&label(cmd))))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Precomputed hints for EVERY dialog overlay, one per field. Rebuilt at
/// startup and on every successful hot-reload (`main.rs`), same as
/// `help_lines` (`help::build`), from the SAME `dialog` effective the shared
/// `Resolver` consumes — BEFORE that effective moves into the `Resolver`
/// (`Effective` is `Clone`, but `DialogHints::build` only borrows: no clone
/// needed). `ui::draw_*` reads them instead of a static Fluent key. The only
/// coupling point with SECURITY semantics (which commands each overlay
/// accepts) is `app.rs`'s ALLOWLISTs — the SAME list that filters dispatch,
/// never a copy.
#[derive(Debug, Clone, Default)]
pub struct DialogHints {
    /// `Modal::ConfirmDelete`/`Modal::ConfirmTransfer`/`Modal::ConfirmQuit`
    /// (S2, `[ui] confirm_quit`).
    pub confirm: String,
    /// `Modal::Collision`.
    pub collision: String,
    /// `Modal::ApproveAgentOp`.
    pub approval: String,
    /// `Modal::ConfirmPluginUninstall` (ADR 0104): confirm with no `approve`.
    pub uninstall: String,
    /// `Modal::TrustHostKey`.
    pub trust_host: String,
    /// `Modal::AskSecret` (#325).
    pub ask_secret: String,
    /// Theme selector (`App::theme_picker`).
    pub picker: String,
    /// Columns picker (`App::columns_picker`, #108 7a).
    pub columns: String,
    /// Extension manager (`App::extensions`).
    pub extensions: String,
    /// A plugin's `[config]` panel inside the extension manager
    /// (`App::extensions`'s `config`, G3c).
    pub plugin_config: String,
    /// Navigation popup in hotlist mode (`App::nav_popup`,
    /// `NavPopupKind::Hotlist`) — history paints no footer, same as before
    /// H1.
    pub nav_list: String,
    /// Navigation popup in volumes mode (`App::nav_popup`,
    /// `NavPopupKind::Volumes`, design §D) — its own hint because
    /// `nav_list`'s `add`/`remove` mean nothing here and the "show all"
    /// toggle does.
    pub nav_volumes: String,
    /// Navigation popup in history or popular mode (spec 2026-09-15 D2).
    pub nav_history: String,
    /// Help overlay (`App::help`, H3b).
    pub help: String,
    /// `true` when a help page is covering a modal, so the modal's own keys
    /// are inert until it closes (H3c).
    ///
    /// The generated footers already say so — [`Self::with_modals_inert`]
    /// replaces them. This flag is for the modals whose key hint is not
    /// generated but baked into Fluent PROSE, so their text can say the same
    /// true thing instead of advertising `y`/`n` at a reader for whom both do
    /// nothing.
    ///
    /// Today that is the AI rename plan and the semantic hits — the two whose
    /// hint is PROSE, which is not the same set as "the modals a help can
    /// cover". That set is every modal
    /// `norte_tui::help_context::help_over_modal_allowed` admits, the agent
    /// approval and the host-key TOFU included; those simply have a generated
    /// footer, which the function above replaces. A new prose-hinted modal needs
    /// an arm here too.
    pub modals_inert: bool,
    /// `[ui] dialog_buttons` (spec 2026-09-10): a modal's key line is painted
    /// as clickable BUTTONS instead of as text. Set by whoever builds the
    /// hints from the config; `build` leaves it off because an `Effective`
    /// knows nothing about settings.
    pub buttons: bool,
}

/// A button on a modal's key line: the painted chord and its verb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HintButton {
    /// The chord exactly as `paint_chord` writes it (`Enter`, `Esc`, `F5`).
    pub chord: String,
    /// The verb, in the reader's language.
    pub label: String,
}

/// The buttons of a key line — the one [`dialog_hints`] generates (`[Enter]
/// confirm [Esc] cancel`) or one written in Fluent (`[enter] confirm · [esc]
/// cancel`) — or `None` if the line does not have that shape.
///
/// Each group starts with `[`, the chord ends at `] `, and the verb runs to
/// the next ` [` or ` · [`. A verb may carry spaces; a chord carries neither
/// `]` nor spaces, and is painted the way the documentation spells it
/// (`Enter`, not `enter`), which is also what the mouse synthesizes.
#[must_use]
pub fn hint_buttons(line: &str) -> Option<Vec<HintButton>> {
    if !line.starts_with('[') {
        return None;
    }
    let mut out = Vec::new();
    for group in line.split(" [") {
        let group = group.strip_prefix('[').unwrap_or(group);
        let (chord, label) = group.split_once("] ")?;
        let label = label.trim_end_matches(" ·").trim();
        if chord.is_empty() || chord.contains(' ') || chord.len() > 16 || label.is_empty() {
            return None;
        }
        out.push(HintButton {
            chord: crate::keymap::paint_chord(chord),
            label: label.to_owned(),
        });
    }
    (!out.is_empty()).then_some(out)
}

impl DialogHints {
    /// Rebuilds every hint from the current `dialog` effective.
    #[must_use]
    pub fn build(eff: &Effective) -> Self {
        use crate::app::{
            ALLOW_APPROVAL, ALLOW_ASK_SECRET, ALLOW_COLLISION, ALLOW_COLUMNS, ALLOW_CONFIRM,
            ALLOW_EXTENSIONS, ALLOW_NAV_HISTORY, ALLOW_NAV_HOTLIST, ALLOW_NAV_VOLUMES,
            ALLOW_PICKER, ALLOW_PLUGIN_CONFIG, ALLOW_TRUST_HOST, ALLOW_UNINSTALL,
        };
        Self {
            confirm: dialog_hints(ALLOW_CONFIRM, eff),
            collision: dialog_hints(ALLOW_COLLISION, eff),
            approval: dialog_hints(ALLOW_APPROVAL, eff),
            uninstall: dialog_hints(ALLOW_UNINSTALL, eff),
            trust_host: dialog_hints(ALLOW_TRUST_HOST, eff),
            ask_secret: dialog_hints(ALLOW_ASK_SECRET, eff),
            // Non-modal overlays (MAJOR-1): arrows are self-evident, so they
            // are dropped from the PRINTED hint (never from dispatch — see
            // `without_navigation`).
            picker: dialog_hints(&without_navigation(ALLOW_PICKER), eff),
            // #108 7a: besides navigation, the columns picker's footer omits
            // the REORDERING verbs — shift+↑/↓ are the shifted arrows,
            // self-evident next to up/down, and with them the hint (101
            // cells in es) does not fit an 80-column frame (the same
            // MAJOR-1 that motivated `without_navigation`). Only the PRINTED
            // hint: `ALLOW_COLUMNS` and the dispatch do not change.
            columns: dialog_hints(
                &without_navigation(ALLOW_COLUMNS)
                    .into_iter()
                    .filter(|c| !matches!(*c, "dialog.move-up" | "dialog.move-down"))
                    .collect::<Vec<_>>(),
                eff,
            ),
            // The manager binds `dialog.pane` — `tab` moves focus between the
            // list and the sheet's buttons — but does NOT print it, and it is
            // the only one of these footers with an exclusion of its own. Its
            // footer already ran full at 80 columns: with the usual five
            // verbs three cells were left free, and the sixth did not grow
            // the box but split the fifth in half (`[Tab] othe┘`) — exactly
            // the MAJOR-1 that gave birth to `without_navigation`, whose
            // criterion applies the same here: the key still does its job, it
            // is just not spelled out. `dialog.pane` does not join the SHARED
            // list because six other allowlists bind it with the meaning "the
            // other pane", and there it does fit and is needed. What
            // accounts for it instead: the manager's own help topic, and the
            // button itself, which lights up on receiving focus.
            extensions: dialog_hints(
                &without_navigation(ALLOW_EXTENSIONS)
                    .into_iter()
                    .filter(|c| *c != "dialog.pane")
                    .collect::<Vec<_>>(),
                eff,
            ),
            // `dialog.pane` out of the footer for the same reason as above:
            // it exits as `Esc`, which is already spelled out.
            plugin_config: dialog_hints(
                &without_navigation(ALLOW_PLUGIN_CONFIG)
                    .into_iter()
                    .filter(|c| *c != "dialog.pane")
                    .collect::<Vec<_>>(),
                eff,
            ),
            nav_list: dialog_hints(&without_navigation(ALLOW_NAV_HOTLIST), eff),
            nav_volumes: dialog_hints(&without_navigation(ALLOW_NAV_VOLUMES), eff),
            // Only the list's OWN verbs: with confirm and cancel in front,
            // the Spanish footer went past 80 cells and the frame cut the
            // last group in half (`[Alt+Enter]┘` with no label), the same
            // MAJOR-1 that motivated `without_navigation`. Enter and Esc are
            // keys any list already teaches; the dispatch does not change.
            nav_history: dialog_hints(
                &without_navigation(ALLOW_NAV_HISTORY)
                    .into_iter()
                    // `add` too: with it, it does not fit 80 cells, and
                    // history's help topic already says so.
                    .filter(|c| !matches!(*c, "dialog.confirm" | "dialog.cancel" | "dialog.add"))
                    .collect::<Vec<_>>(),
                eff,
            ),
            // H3b: offered WHOLE, in priority order — the width decides how
            // much of it is printed (`ui::fit_hint_groups`), not a fixed
            // exclusion. See [`HELP_HINT_PRIORITY`].
            help: hints_in_order_with(HELP_HINT_PRIORITY, eff, help_hint_id),
            // Hints JUST built describe keys that DO respond; only
            // `with_modals_inert` raises the flag, and only while a help page
            // covers the modal.
            modals_inert: false,
            buttons: false,
        }
    }

    /// The same hints, with every MODAL footer replaced by "close the help to
    /// answer" (H3c).
    ///
    /// For the state where a help page is open OVER a modal: the help owns the
    /// keys then (`HelpView::over_modal`), so the modal's verbs are INERT — an
    /// approval prompt advertising `[y] approve [n] deny` while both keys do
    /// nothing is the same defect this module exists to prevent, only reached
    /// through key OWNERSHIP instead of through a rebind. A footer here can
    /// never desync from what the key does; that has to include the case where
    /// the key does nothing.
    ///
    /// The four modal fields and no others. The rest belong to overlays that
    /// are not modals, and a modal being on screen at all already took their
    /// keys away long before this (`modal_wins`) — a state H1 decided
    /// deliberately, not one this function is about.
    ///
    /// Only the footer changes: the box, the title and the question keep being
    /// painted, on top of the page ([`crate::ui::draw`] paints the modal last).
    /// Hiding the question is the defect that ordering fixed, and replacing a
    /// footer must not undo it.
    #[must_use]
    pub fn with_modals_inert(&self) -> Self {
        let notice = t("modal-hint-help-open");
        Self {
            confirm: notice.clone(),
            collision: notice.clone(),
            approval: notice.clone(),
            trust_host: notice,
            modals_inert: true,
            ..self.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Screen, parse_keymap};

    /// The factory preset's `dialog` effective, the one real footers paint.
    /// Vocabulary = `COMMANDS` ∪ `DIALOG_COMMANDS`: the `dialog` effective
    /// ALSO merges the preset's `[global]` section, so `DIALOG_COMMANDS`
    /// alone is not enough (`build_for` would fail with
    /// `UnknownCommand { run: "app.quit" }`).
    fn orthodox_dialog() -> Effective {
        let (_, preset) = crate::keymap::presets()
            .into_iter()
            .find(|(n, _)| *n == "orthodox")
            .expect("preset orthodox");
        let known: Vec<&str> = crate::keymap::COMMANDS
            .iter()
            .copied()
            .chain(crate::keymap::DIALOG_COMMANDS.iter().copied())
            .collect();
        Effective::build_for(&preset, &[], &known, Screen::Dialog).expect("dialog effective")
    }

    /// Encoding audit H1: a PROJECT `./.norte/keymap.toml` (no trust) can
    /// bind a hostile chord (RLO/ZWSP/LRM/BEL, corpus
    /// `norte_testkit::corpus::hostile_chords`) to a `dialog.*` command
    /// supported via `prepend_keymap` — a user layer, which beats the preset.
    /// The generated hint (`dialog_hints`) is what gets painted on SECURITY
    /// modals' footers (`ApproveAgentOp`/`TrustHostKey`/permanent
    /// `ConfirmDelete`): no hazard can survive raw.
    #[test]
    fn dialog_hints_masks_hostile_chords_from_a_layer() {
        let preset = parse_keymap(
            r#"
            [dialog]
            keymap = [{ on = ["y"], run = "dialog.approve" }]
        "#,
        )
        .unwrap();
        let known = ["dialog.approve", "dialog.deny"];
        for hazard in norte_testkit::corpus::hostile_chords() {
            // TOML `\uXXXX` escape (spec v1.0.0): a raw C0 control such as
            // BEL (U+0007) is invalid syntax inside a basic TOML string, so
            // the token is ALWAYS escaped, never raw.
            let token_esc = format!("\\u{:04X}", hazard.token as u32);
            let layer_src = format!(
                r#"
                [dialog]
                prepend_keymap = [{{ on = ["{token_esc}"], run = "dialog.approve" }}]
                "#,
            );
            let layer = parse_keymap(&layer_src).unwrap();
            let eff = Effective::build_for(&preset, &[layer], &known, Screen::Dialog)
                .unwrap_or_else(|e| panic!("[{}] effective keymap: {e}", hazard.id));
            let hint = dialog_hints(&["dialog.approve", "dialog.deny"], &eff);
            assert!(
                !hint.chars().any(norte_encoding::is_terminal_hazard),
                "[{}] raw hazard in the hint: {hint:?}",
                hazard.id
            );
            assert!(
                hint.contains('\u{FFFD}'),
                "[{}] the hazard must be masked to U+FFFD: {hint:?}",
                hazard.id
            );
        }
    }

    #[test]
    fn dialog_hints_skips_commands_with_no_binding() {
        // This test asserts the ENGLISH corpus strings. Without pinning the
        // language it resolved by environment (`LANG`), so it was green in CI
        // and red on any machine with `LANG=es_*` — the same line the rest of
        // this crate's render tests already carried.
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let preset = parse_keymap(
            r#"
            [dialog]
            keymap = [{ on = ["y"], run = "dialog.approve" }]
        "#,
        )
        .unwrap();
        let known = ["dialog.approve", "dialog.deny"];
        let eff = Effective::build_for(&preset, &[], &known, Screen::Dialog).unwrap();
        let hint = dialog_hints(&["dialog.approve", "dialog.deny"], &eff);
        assert_eq!(hint, "[y] approve");
    }

    #[test]
    fn dialog_hints_follows_the_effectives_order_not_the_allowlists() {
        // This test asserts the ENGLISH corpus strings. Without pinning the
        // language it resolved by environment (`LANG`), so it was green in CI
        // and red on any machine with `LANG=es_*` — the same line the rest of
        // this crate's render tests already carried.
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let preset = parse_keymap(
            r#"
            [dialog]
            keymap = [
                { on = ["esc"], run = "dialog.cancel" },
                { on = ["enter"], run = "dialog.confirm" },
            ]
        "#,
        )
        .unwrap();
        let known = ["dialog.confirm", "dialog.cancel"];
        let eff = Effective::build_for(&preset, &[], &known, Screen::Dialog).unwrap();
        // The allowlist asks for confirm-before-cancel; the effective
        // declares cancel first — the hint follows the effective.
        // PAINTED chords (`paint_chord`): `Esc`/`Enter`, not `esc`/`enter` —
        // `Chord`'s `Display` is raw and lowercase only for logs.
        let hint = dialog_hints(&["dialog.confirm", "dialog.cancel"], &eff);
        assert_eq!(hint, "[Esc] cancel [Enter] confirm");
    }

    #[test]
    fn dialog_hints_uses_the_first_chord_on_a_duplicate() {
        // This test asserts the ENGLISH corpus strings. Without pinning the
        // language it resolved by environment (`LANG`), so it was green in CI
        // and red on any machine with `LANG=es_*` — the same line the rest of
        // this crate's render tests already carried.
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let preset = parse_keymap(
            r#"
            [dialog]
            keymap = [
                { on = ["up"], run = "dialog.up" },
                { on = ["k"], run = "dialog.up" },
            ]
        "#,
        )
        .unwrap();
        let known = ["dialog.up"];
        let eff = Effective::build_for(&preset, &[], &known, Screen::Dialog).unwrap();
        let hint = dialog_hints(&["dialog.up"], &eff);
        assert_eq!(hint, "[Up] up");
    }

    #[test]
    fn dialog_hints_is_an_empty_string_with_no_supported_bound() {
        let preset = parse_keymap(
            r#"
            [dialog]
            keymap = [{ on = ["y"], run = "dialog.approve" }]
        "#,
        )
        .unwrap();
        let known = ["dialog.approve"];
        let eff = Effective::build_for(&preset, &[], &known, Screen::Dialog).unwrap();
        let hint = dialog_hints(&["dialog.rename"], &eff);
        assert_eq!(hint, "");
    }

    /// MAJOR-1 (H1 close): navigation keys do NOT appear in the three
    /// NON-modal overlay hints (picker/extensions/hotlist) — they would paint
    /// as self-evident and truncate the footer at 80 columns
    /// (`snapshots_ui__snapshot_popup_hotlist.snap` before this fix). Modals
    /// DO carry their commands whole (none of them support navigation) —
    /// nothing to filter, so their behavior does not change.
    #[test]
    fn non_modal_overlays_omit_navigation_from_the_hint() {
        let hints = DialogHints::build(&orthodox_dialog());
        for hint in [
            &hints.picker,
            &hints.extensions,
            &hints.plugin_config,
            &hints.nav_list,
        ] {
            assert!(
                !hint.contains("[Up]") && !hint.contains("[Down]"),
                "arrows should not appear in a non-modal hint: {hint:?}"
            );
        }
        // The picker DOES keep confirm/cancel (they are not navigation).
        assert!(hints.picker.contains("[Enter]"));
        assert!(hints.picker.contains("[Esc]"));
    }

    /// H3b: the help overlay's footer is GENERATED like every other
    /// overlay's — the three verbs it adds must reach it with their chords.
    ///
    /// With HELP's own label, which is not the same verb's label in a modal:
    /// `dialog.pane` here is "index ↔ text" and not "other pane", which over
    /// an overlay with no panes names something the reader cannot do.
    #[test]
    fn helps_hint_lists_its_own_verbs() {
        let hints = DialogHints::build(&orthodox_dialog());
        for cmd in ["dialog.filter", "dialog.back", "dialog.pane"] {
            assert!(
                hints.help.contains(&t(&help_hint_id(cmd))),
                "{cmd} must appear in help's footer: {}",
                hints.help
            );
        }
        assert!(
            !hints.help.contains(&t("dialog-cmd-pane")),
            "and never with the modal's label: {}",
            hints.help
        );
    }

    /// H3b, adaptive footer: the help hint is OFFERED whole — all five
    /// printable verbs, `Enter` and `Esc` included — and in the priority order
    /// a narrow frame will cut from the tail. Nothing is excluded up front any
    /// more: a 113-column terminal has room for the lot and used to paint half
    /// an empty footer while hiding them.
    #[test]
    fn helps_footer_offers_all_its_verbs_in_priority_order() {
        let hints = DialogHints::build(&orthodox_dialog());
        let position = |cmd: &str| {
            hints
                .help
                .find(&t(&help_hint_id(cmd)))
                .unwrap_or_else(|| panic!("{cmd} must be in help's footer: {}", hints.help))
        };
        let order: Vec<usize> = HELP_HINT_PRIORITY.iter().map(|c| position(c)).collect();
        assert!(
            order.windows(2).all(|w| w[0] < w[1]),
            "verbs come out in priority order, which is what decides \
             what survives a narrow frame: {}",
            hints.help
        );
        // ARROWS stay out — moving the cursor between runnable rows is
        // discoverable on its own — but paging IS included: the body is long
        // prose in a short window, and nothing else on screen says how to
        // scroll down through it. That is the difference between this screen
        // and an options list.
        for cmd in ["dialog.up", "dialog.down"] {
            assert!(
                !hints
                    .help
                    .contains(&format!("] {}", t(&dialog_hint_id(cmd)))),
                "{cmd} is self-evident and does not spend width: {}",
                hints.help
            );
        }
        for cmd in ["dialog.page-up", "dialog.page-down"] {
            assert!(
                hints.help.contains(&t(&help_hint_id(cmd))),
                "{cmd} is what nobody guesses on a page of prose: {}",
                hints.help
            );
        }
    }

    /// [`HELP_HINT_PRIORITY`] is a COMPLETE projection of `ALLOW_HELP`: a
    /// verb new to the allowlist has to be ranked here, not left invisible in
    /// the footer forever (which is what the fixed exclusion used to do).
    /// And the other way around: nothing is advertised that the dispatch does
    /// not accept.
    #[test]
    fn help_priority_covers_every_printable_verb() {
        use crate::app::{ALLOW_HELP, help_action};
        // Paging IS printed on this screen (see `HELP_HINT_PRIORITY`): the
        // only things that do not spend width here are the arrows and the
        // extremes (Home, End), which are discoverable on their own — and
        // with them the footer did not fit whole even at 124 columns.
        let printable: Vec<&str> = ALLOW_HELP
            .iter()
            .copied()
            .filter(|c| {
                !matches!(
                    *c,
                    "dialog.up"
                        | "dialog.down"
                        | "dialog.top"
                        | "dialog.bottom"
                        | "dialog.section-prev"
                        | "dialog.section-next"
                )
            })
            .collect();
        for cmd in &printable {
            assert!(
                HELP_HINT_PRIORITY.contains(cmd),
                "{cmd} is printable but is not ranked in HELP_HINT_PRIORITY"
            );
        }
        for cmd in HELP_HINT_PRIORITY {
            assert!(
                printable.contains(cmd),
                "{cmd} would be advertised without the dispatch accepting it"
            );
            assert!(
                help_action(cmd).is_some(),
                "…and the key has to be alive: {cmd}"
            );
        }
        assert_eq!(HELP_HINT_PRIORITY.len(), printable.len());
    }

    /// `dialog_hints_in_order` follows the caller's ORDER (unlike
    /// [`dialog_hints`], which follows the effective) and skips what is not
    /// bound.
    #[test]
    fn dialog_hints_in_order_follows_the_caller_not_the_effective() {
        // This test asserts the ENGLISH corpus strings. Without pinning the
        // language it resolved by environment (`LANG`), so it was green in CI
        // and red on any machine with `LANG=es_*` — the same line the rest of
        // this crate's render tests already carried.
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let preset = parse_keymap(
            r#"
            [dialog]
            keymap = [
                { on = ["esc"], run = "dialog.cancel" },
                { on = ["enter"], run = "dialog.confirm" },
            ]
        "#,
        )
        .unwrap();
        let known = ["dialog.confirm", "dialog.cancel", "dialog.pane"];
        let eff = Effective::build_for(&preset, &[], &known, Screen::Dialog).unwrap();
        assert_eq!(
            dialog_hints_in_order(&["dialog.confirm", "dialog.pane", "dialog.cancel"], &eff),
            "[Enter] confirm [Esc] cancel",
            "the order is the one requested, and `dialog.pane` (no binding) invents no key"
        );
    }

    /// H3c: with a help page ON TOP, the FOUR modal footers say it must be
    /// closed and offer no verb at all — not even `confirm`/`cancel`, which
    /// the render test cannot isolate on its own (help's own footer lists
    /// them, and there they DO respond).
    ///
    /// Footers that are NOT a modal's stay untouched: a modal on screen had
    /// already taken their key away long before (`modal_wins`, H1), and that
    /// is a decision from back then, not what this function fixes.
    #[test]
    fn modal_footers_stop_offering_verbs_under_help() {
        let alive = DialogHints::build(&orthodox_dialog());
        let inert = alive.with_modals_inert();
        let notice = t("modal-hint-help-open");
        for footer in [
            &inert.confirm,
            &inert.collision,
            &inert.approval,
            &inert.trust_host,
        ] {
            assert_eq!(footer, &notice);
        }
        // No verb of the `dialog.*` vocabulary survives in them.
        for cmd in crate::keymap::DIALOG_COMMANDS {
            let label = t(&dialog_hint_id(cmd));
            for footer in [
                &inert.confirm,
                &inert.collision,
                &inert.approval,
                &inert.trust_host,
            ] {
                assert!(
                    !footer.contains(&label),
                    "{cmd} is still advertised in an inert footer: {footer:?}"
                );
            }
        }
        // And whatever is not a modal is not touched.
        assert_eq!(inert.picker, alive.picker);
        assert_eq!(inert.columns, alive.columns);
        assert_eq!(inert.extensions, alive.extensions);
        assert_eq!(inert.plugin_config, alive.plugin_config);
        assert_eq!(inert.nav_list, alive.nav_list);
        assert_eq!(inert.help, alive.help, "help DOES have its keys");
    }

    /// [`without_navigation`] filters ONLY the four navigation entries,
    /// preserving the rest intact along with their relative order.
    #[test]
    fn without_navigation_filters_only_navigation() {
        let supported = [
            "dialog.up",
            "dialog.approve",
            "dialog.down",
            "dialog.cancel",
        ];
        assert_eq!(
            without_navigation(&supported),
            vec!["dialog.approve", "dialog.cancel"]
        );
    }
}
