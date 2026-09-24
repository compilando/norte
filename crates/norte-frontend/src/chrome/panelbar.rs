//! The panel bar: which panels there are, in what order and how they are.
//!
//! The side panels — places, tree, processes, the log — open by shortcut,
//! by the menu or by the palette, and all three paths require KNOWING that
//! the panel exists. There was no surface that showed them, so a new panel
//! was invisible to whoever did not read the changelog.
//!
//! And there is a reason beyond convenience: `layout::kinds` is an OPEN
//! registry — a plugin can contribute a panel kind — and a contributed
//! panel that appears nowhere is discovered by nobody. That is why this is
//! DERIVED from the registry and not from a hand-written list: the day a
//! plugin contributes a kind, it comes out on its own.
//!
//! The decision lives here and not in each frontend (ADR 0077): what goes
//! in the bar and in what order is decided once, and the TUI and the
//! window only paint.

use crate::layout::KindRegistry;

/// Kinds that are NOT panels that open and close.
///
/// `browser` is the listing (there is always one), `tasks` and `status`
/// are strips that are looked at and not focused, and `compare`/`sync` are
/// opened by an operation, not a button. A button that cannot open or
/// close anything is not a button.
const STRUCTURAL: &[&str] = &["browser", "tasks", "status", "compare", "sync"];

/// The command that opens and closes each built-in panel.
///
/// A table and not a convention for these because their names are
/// historical: `viewer` is opened by `layout.preview` and `tree` by
/// `pane.tree`. For what is not here the `layout.<kind>` convention is
/// used, which is what a plugin contributing a panel would have to follow.
const TOGGLES: &[(&str, &str)] = &[
    ("places", "layout.places"),
    ("tree", "pane.tree"),
    ("viewer", "layout.preview"),
    ("processes", "layout.processes"),
    ("metadata", "layout.metadata"),
    ("log", "layout.log"),
    ("disk-map", "layout.disk-map"),
];

/// How a panel is right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelState {
    /// Not even in the layout.
    Closed,
    /// Open, but the keyboard belongs to the listings or another panel.
    Open,
    /// Open AND holding the keyboard.
    Focused,
}

/// A bar button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelButton {
    /// The kind it opens.
    pub kind: String,
    /// The command that opens and closes it.
    pub command: String,
    /// The letter that is painted.
    pub letter: char,
    /// The short name the letter came from, in the language it was built
    /// with (spec 2026-09-10): what `[ui] panel_bar_style = "names"` paints
    /// in full.
    pub name: String,
    /// How it is.
    pub state: PanelState,
    /// How many things it has to count (notices in the log, live tasks);
    /// `0` = nothing. The TUI paints a mark; the window, the figure, like
    /// VS Code's activity bar badge (spec 2026-09-21).
    pub attention: u32,
}

/// What a button takes up and shows in a row of cells (spec 2026-09-10).
///
/// With `names`, ` Places ` with the access letter underlined wherever it
/// appears in the name; without it, ` P `, the usual row. The rightmost
/// cell is ALWAYS the novelty mark's, so the row does not jitter when
/// something happens. Both frontends start from here: the TUI to paint and
/// for the mouse zones (which this way are the same number), the window
/// for the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ButtonCell {
    /// The text WITHOUT the left space or the mark's cell.
    pub text: String,
    /// Index (in `text`'s chars) of the access letter: where it appears in
    /// the name, or `0` if it is not there and is painted in front.
    pub letter_at: usize,
    /// Total width in cells, space and mark included.
    pub width: usize,
}

/// The cell of a button in the requested style.
#[must_use]
pub fn button_cell(b: &PanelButton, names: bool) -> ButtonCell {
    if !names {
        return ButtonCell {
            text: b.letter.to_string(),
            letter_at: 0,
            width: 3,
        };
    }
    let letter_at = b
        .name
        .chars()
        .position(|c| c.to_uppercase().next().is_some_and(|u| u == b.letter));
    let text = if letter_at.is_some() {
        b.name.clone()
    } else {
        // The letter is not in the name (tiebreak by another free one): it
        // is painted in front so it is still known which one it is.
        format!("{} {}", b.letter, b.name)
    };
    let width = crate::display::cells(&text) + 2;
    // With the letter not in the name, it goes in front: index 0.
    let letter_at = letter_at.unwrap_or(0);
    ButtonCell {
        text,
        letter_at,
        width,
    }
}

/// Do ALL named buttons fit in `width` cells? If not, the row falls back
/// alone to letters: half a word is not a button, and a bar that hides
/// buttons says less than one of letters that shows them all.
#[must_use]
pub fn names_fit(buttons: &[PanelButton], width: usize) -> bool {
    buttons
        .iter()
        .map(|b| button_cell(b, true).width)
        .sum::<usize>()
        <= width
}

/// Which icon set the terminal paints its panel column with (ADR 0140).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconSet {
    /// One-cell Unicode symbols in any terminal font, with no emoji
    /// presentation (an emoji measures two in many terminals and throws
    /// off the column).
    Unicode,
    /// Nerd Fonts glyphs (Font Awesome, in the private area): closer to VS
    /// Code's icons, for whoever has one of those fonts.
    Nerd,
}

/// The icon of a built-in panel, or `None` for one that has none (a
/// plugin's): then its LETTER is painted, which is already known about it.
///
/// The same subjects as the window's icons (`render/icons.ts`): star,
/// branches, eye, pulse, "i", lines, wheel, clock.
///
/// ```
/// use norte_frontend::panelbar::{IconSet, icon};
/// assert_eq!(icon("places", IconSet::Unicode), Some("★"));
/// assert_eq!(icon("plugin:x:y", IconSet::Nerd), None);
/// ```
#[must_use]
pub fn icon(kind: &str, set: IconSet) -> Option<&'static str> {
    let (unicode, nerd) = match kind {
        "places" => ("★", "\u{f005}"),
        "tree" => ("⋔", "\u{f0e8}"),
        "viewer" => ("◉", "\u{f06e}"),
        "processes" => ("∿", "\u{f21e}"),
        "metadata" => ("ⓘ", "\u{f05a}"),
        "log" => ("≡", "\u{f03a}"),
        "disk-map" => ("◔", "\u{f200}"),
        "timeline" => ("◷", "\u{f017}"),
        _ => return None,
    };
    Some(match set {
        IconSet::Unicode => unicode,
        IconSet::Nerd => nerd,
    })
}

/// A count for a button's badge: saturates instead of truncating, because
/// a figure that wraps around would say "nothing" with a full log.
///
/// ```
/// use norte_frontend::panelbar::figure;
/// assert_eq!(figure(3), 3);
/// assert_eq!(figure(usize::MAX), u32::MAX);
/// ```
#[must_use]
pub fn figure(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// What the bar needs to know about the moment.
#[derive(Debug, Default, Clone, Copy)]
pub struct PanelBarInput<'a> {
    /// Kinds placed in the layout, **in screen order**: top to bottom and,
    /// at equal height, left to right.
    ///
    /// The order matters because it is the buttons' (see [`buttons`]). The
    /// caller orders it, since it is the one with the rectangles; here it
    /// is only respected.
    pub open: &'a [&'a str],
    /// The kind holding the keyboard, if it is a panel.
    pub focused: Option<&'a str>,
    /// Kinds with novelty, with how many. A `0` figure is the same as not
    /// being there.
    pub attention: &'a [(&'a str, u32)],
}

/// The bar's buttons: the OPEN ones in the order they are on screen, and
/// the closed ones behind them in registry order.
///
/// The row following the screen is what makes the bar readable at a
/// glance: the button of the pane on the left goes on the left, the one
/// below goes last. With registry order you had to mentally translate
/// between two lists every time.
///
/// The closed ones go after because they have no position: inventing one
/// would mean saying where they are when they are nowhere. Among them the
/// registry rules — the built-in ones before whatever a plugin
/// contributes — so their relative position does not jitter.
///
/// **The LETTER does not depend on the order**, and that is half the
/// decision: it is resolved by walking the registry, before sorting. If it
/// did depend on it, opening a panel could change another one's letter —
/// the tiebreak looks at the ones already given out — and the bar would
/// stop being learnable.
#[must_use]
pub fn buttons(reg: &KindRegistry, input: PanelBarInput<'_>) -> Vec<PanelButton> {
    buttons_with(reg, input, norte_i18n::t)
}

/// [`buttons`] in a GIVEN language: the letter comes from the short name,
/// so a window translating with its session's language has to derive it
/// from the SAME name it shows, or "Sitios" would carry the `P` from
/// "Places".
#[must_use]
pub fn buttons_in(
    reg: &KindRegistry,
    input: PanelBarInput<'_>,
    lang: norte_i18n::Lang,
) -> Vec<PanelButton> {
    buttons_with(reg, input, |key| norte_i18n::t_in(lang, key))
}

fn buttons_with(
    reg: &KindRegistry,
    input: PanelBarInput<'_>,
    t: impl Fn(&str) -> String,
) -> Vec<PanelButton> {
    let mut out: Vec<PanelButton> = Vec::new();
    for decl in reg.decls() {
        let id = decl.id.as_str();
        if !es_button(decl) {
            continue;
        }
        let command = TOGGLES
            .iter()
            .find(|(k, _)| *k == id)
            .map_or_else(|| format!("layout.{id}"), |(_, c)| (*c).to_string());
        let name = name_with(id, &command, &t);
        let letter = letter_of(&name, id, &out);
        let open = input.open.contains(&id);
        let state = if !open {
            PanelState::Closed
        } else if input.focused == Some(id) {
            PanelState::Focused
        } else {
            PanelState::Open
        };
        out.push(PanelButton {
            kind: id.to_string(),
            command,
            letter,
            name,
            state,
            attention: input
                .attention
                .iter()
                .find(|(k, _)| *k == id)
                .map_or(0, |(_, n)| *n),
        });
    }
    // The order is the REGISTRY's, always, open or not (Oscar's decision
    // 2026-09-11). Before, open ones jumped ahead "in screen order", and
    // that made clicking a button move the others: a row that rearranges
    // itself when clicked is not learned by the finger. The state — open,
    // holding the keyboard, with novelty — already says it with each
    // button's color; the position does not have to repeat it.
    out
}

/// Does this kind deserve a button?
///
/// One single copy of the criterion, and that matters: the tests that
/// check that every panel has a short name in both languages use it too,
/// so adding a kind to the registry and forgetting its translation turns a
/// test red instead of painting a Spanish-speaking reader the id's English
/// initial.
#[must_use]
pub fn es_button(decl: &crate::layout::KindDecl) -> bool {
    // A bar panel is one that gets focused: the ones that are only looked
    // at have no business here.
    //
    // And not the ones CONTRIBUTED by a plugin (phase 3): a button's
    // command is `layout.<kind>`, which for a contributed one would be
    // `layout.plugin:git:status` and does not exist in any catalogue. The
    // TUI silently dropped it and the window answered "cmd-not-here" — the
    // same decision with two answers, which is exactly what ADR 0077
    // forbids. They enter the bar once the command that opens and closes
    // them exists.
    !STRUCTURAL.contains(&decl.id.as_str())
        && decl.focusable
        && !decl.id.as_str().starts_with("plugin:")
}

/// The panel's SHORT name, in a GIVEN language.
///
/// Its own key (`panelbar-<kind>`) and not the menu label, which is a
/// phrase: "Places panel", "Metadata panel" and "Processes panel" all
/// three start with `P`, so their initials distinguish nothing. A short
/// name is data distinct from a menu entry, and this treats it as such.
///
/// With no key — a panel contributed by a plugin — it falls back to the
/// menu label, and with none of that either, to the kind's id: never to
/// nothing, because a button with no letter is not a button.
///
/// In a given language and not the global one because the window
/// translates with its session's (`t_in`), and a label pulled from the
/// global one would speak a different language than the rest of its
/// chrome. The TUI goes through [`buttons`], which uses the global one.
#[must_use]
pub fn label_in(lang: norte_i18n::Lang, kind: &str, command: &str) -> String {
    name_with(kind, command, |key| norte_i18n::t_in(lang, key))
}

fn name_with(kind: &str, command: &str, t: impl Fn(&str) -> String) -> String {
    // Compared against the KEY, not `starts_with`: `t`'s contract is to
    // return the id when the message is missing, and `starts_with("panelbar-")`
    // would also fire on a present translation whose text happened to
    // start with that literal.
    let key = format!("panelbar-{kind}");
    let own = t(&key);
    if own != key {
        return own;
    }
    // The menu label, for a kind that has it and not the other. Nobody
    // reaches here today: a plugin's messages are not merged into the
    // translation bundle, so a contributed kind always falls back to the
    // id. It stays as a RESERVED step for when a plugin can register
    // messages.
    let menu_key = format!("menu-item-{}", command.replace('.', "-"));
    let from_menu = t(&menu_key);
    if from_menu == menu_key {
        kind.to_string()
    } else {
        from_menu
    }
}

/// A button's letter: the initial of its NAME, in the reader's language.
///
/// And not the shortcut's, which was the first version and was dropped
/// once painted: the letters really came from the keymap — `B` from
/// `alt+b`, `Q` from `alt+q` — so they were unambiguous about what to
/// press and mute about what each one opened. A bar that exists so you
/// discover the panels are there was only understood by whoever already
/// knew them. The shortcut is shown by the menu, which lists every panel
/// with its chord next to it; this row shows that they EXIST.
///
/// It is deduplicated: two buttons with the same letter are not
/// distinguishable, so the second one moves to the next free letter of its
/// own name and, if those run out, of the alphabet.
fn letter_of(name: &str, kind: &str, already: &[PanelButton]) -> char {
    let candidates = name
        .chars()
        .chain(kind.chars())
        .chain('a'..='z')
        // And digits BEFORE the question mark: a `7` does not say which
        // panel it is, but at least distinguishes two buttons, and `?`
        // distinguishes nothing.
        .chain('0'..='9')
        // Alphanumeric, which includes accents and eñes: the initial of
        // "Árbol" is an `Á` and painting it is correct.
        .filter(|c| c.is_alphanumeric())
        // `to_uppercase` can give several (the `ß`); the first is taken.
        .filter_map(|c| c.to_uppercase().next())
        // Of ONE cell: the button measures three and the clickable zones
        // are computed with that number. A wide character — a contributed
        // kind's id could carry one, and `KindId::new` does not validate —
        // would paint four and shift every button to its right by one
        // column relative to their zones.
        .filter(|c| unicode_width::UnicodeWidthChar::width(*c) == Some(1));
    for c in candidates {
        if !already.iter().any(|b| b.letter == c) {
            return c;
        }
    }
    // Last resort, and deduplicated too: two `?` buttons are not
    // distinguishable from each other, which is worse than a single one
    // that says nothing.
    if already.iter().any(|b| b.letter == '?') {
        '·'
    } else {
        '?'
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{KindDecl, KindId};

    fn registry() -> KindRegistry {
        KindRegistry::builtin()
    }

    /// Every built-in panel that is a button has an icon in both sets, and
    /// every icon measures ONE cell: the terminal's column is three, and a
    /// two-wide one would push the badge out.
    #[test]
    fn every_builtin_button_has_a_one_cell_icon() {
        use unicode_width::UnicodeWidthStr;
        let reg = registry();
        let kinds = [
            "places",
            "tree",
            "viewer",
            "processes",
            "metadata",
            "log",
            "disk-map",
            "timeline",
        ];
        for k in kinds {
            assert!(reg.get(&KindId::new(k)).is_some(), "{k} is a built-in kind");
            for set in [IconSet::Unicode, IconSet::Nerd] {
                let i = icon(k, set).unwrap_or_else(|| panic!("{k} {set:?} has no icon"));
                assert_eq!(i.width(), 1, "{k} {set:?}: {i:?}");
                assert_eq!(i.chars().count(), 1, "no variation selectors");
            }
        }
    }

    /// The bar shows EVERY panel that opens and closes, and none of the
    /// ones that don't. That is its reason to exist: a panel that does not
    /// appear here is only found by whoever already knew it was there.
    #[test]
    fn the_panels_are_there_and_the_structural_ones_are_not() {
        let b = buttons(&registry(), PanelBarInput::default());
        let kinds: Vec<&str> = b.iter().map(|x| x.kind.as_str()).collect();
        // The exact ORDER, not just membership: the position is what the
        // reader learns with their finger, so reordering `builtin()` for
        // an unrelated reason has to fail HERE — with this message — and
        // not in forty render snapshots that do not explain themselves.
        assert_eq!(
            kinds,
            [
                "places",
                "viewer",
                "processes",
                "metadata",
                "tree",
                "log",
                // Phase 4: the disk map enters at the END, which is where
                // its registration order in `builtin()` puts it. The
                // usual ones do not move: the position is what the finger
                // learns.
                "disk-map",
                // Phase 7: the timeline, behind the map for the same
                // reason — the last one registered goes last, and the
                // usual ones do not move.
                "timeline",
                // #362: the terminal panel, the last one registered and
                // therefore the last button. It is the only one that does
                // not close with its own button: a second tap gives it
                // back the focus and leaves the shell alive.
                "terminal",
            ],
            "the built-in buttons' order changed"
        );
        for outside in ["browser", "tasks", "status", "compare", "sync"] {
            assert!(
                !kinds.contains(&outside),
                "\"{outside}\" is not a panel that opens: {kinds:?}"
            );
        }
    }

    /// The order does NOT change when a panel opens: it is the
    /// registry's, open or not (2026-09-11). Before, open ones jumped
    /// ahead and clicking a button moved the others; a row that rearranges
    /// itself when clicked is not learned by the finger. The state is
    /// said by the color.
    #[test]
    fn opening_a_panel_does_not_move_the_buttons() {
        let closed = buttons(&registry(), PanelBarInput::default());
        let before: Vec<&str> = closed.iter().map(|x| x.kind.as_str()).collect();
        // `log` at the very bottom and `places` on the left: in screen
        // order they would go first and reversed; here nothing moves.
        let open = ["log", "places"];
        let b = buttons(
            &registry(),
            PanelBarInput {
                open: &open,
                ..PanelBarInput::default()
            },
        );
        let after: Vec<&str> = b.iter().map(|x| x.kind.as_str()).collect();
        assert_eq!(before, after, "opening does not reorder: {after:?}");
        assert!(
            b.iter()
                .any(|x| x.kind == "log" && x.state == PanelState::Open)
        );
    }

    /// And the LETTER does not depend on the order.
    ///
    /// It is handed out by walking the registry, BEFORE sorting. If it
    /// depended on it, opening a panel could change another one's letter
    /// — the tiebreak looks at the ones already given out — and the bar
    /// would stop being learnable by the finger, which is exactly what it
    /// exists for.
    #[test]
    fn the_letter_does_not_change_when_reordered() {
        let letter_of = |open: &[&str]| -> Vec<(String, char)> {
            let mut v: Vec<(String, char)> = buttons(
                &registry(),
                PanelBarInput {
                    open,
                    ..PanelBarInput::default()
                },
            )
            .into_iter()
            .map(|b| (b.kind, b.letter))
            .collect();
            v.sort();
            v
        };
        assert_eq!(
            letter_of(&[]),
            letter_of(&["log", "places"]),
            "opening panels changed someone's letter"
        );
    }

    /// A kind contributed LATER — what a plugin would do — comes out on
    /// its own, and last: the built-in ones' position cannot jitter
    /// because someone installs something.
    #[test]
    fn a_contributed_kind_appears_last() {
        let mut reg = registry();
        let before = buttons(&reg, PanelBarInput::default());
        reg.insert(KindDecl {
            id: KindId::new("gitlog"),
            min: (20, 4),
            focusable: true,
            takes_keys: true,
            multi: false,
            roles: &[],
        });
        let after = buttons(&reg, PanelBarInput::default());
        assert_eq!(
            after.len(),
            before.len() + 1,
            "the contributed kind did not come out: {after:?}"
        );
        let last = after.last().expect("there are buttons");
        assert_eq!(last.kind, "gitlog");
        // And by convention it is opened by `layout.<kind>`, which is what
        // the plugin would have to declare.
        assert_eq!(last.command, "layout.gitlog");
        // The built-in ones are still where they were.
        assert_eq!(
            after[..before.len()]
                .iter()
                .map(|b| &b.kind)
                .collect::<Vec<_>>(),
            before.iter().map(|b| &b.kind).collect::<Vec<_>>()
        );
    }

    /// The letter is the initial of the panel's NAME, in the reader's
    /// language, and comes from the same place as the menu label.
    ///
    /// The first version pulled it from the shortcut, and was dropped once
    /// painted: `B Q J M T L` was unambiguous about what to press and mute
    /// about what each key opened. A bar that exists to discover the
    /// panels cannot require already knowing them.
    /// The letter is the name's initial, and changes with the language
    /// because the name changes: "Registro" gives `R` and "Log" gives `L`.
    ///
    /// On the PURE function and not on `buttons`, which reads the global
    /// language: that global is set once per process, so a test that
    /// forced it would depend on who forced it earlier — and under `cargo
    /// test`, which shares the process, that is a race.
    #[test]
    fn the_letter_is_the_names_initial() {
        assert_eq!(letter_of("Sitios", "places", &[]), 'S');
        assert_eq!(letter_of("Procesos", "processes", &[]), 'P');
        assert_eq!(letter_of("Registro", "log", &[]), 'R');
        assert_eq!(letter_of("Log", "log", &[]), 'L');
        // With no translatable name, the kind's id; and if not that
        // either, the alphabet: a button with no letter is not a button.
        assert_eq!(letter_of("", "gitlog", &[]), 'G');
    }

    /// A button's cell (spec 2026-09-10): with names, the whole name and
    /// the localized letter inside it; if the letter is not in the name,
    /// it goes in front; without names, three cells as always. And
    /// `names_fit` says when the row falls back alone to letters.
    #[test]
    fn a_buttons_cell_carries_the_name_and_knows_where_its_letter_is() {
        let b = |name: &str, letter: char| PanelButton {
            kind: "x".into(),
            command: "layout.x".into(),
            letter,
            name: name.into(),
            state: PanelState::Closed,
            attention: 0,
        };
        let places = button_cell(&b("Sitios", 'S'), true);
        assert_eq!(
            (places.text.as_str(), places.letter_at, places.width),
            ("Sitios", 0, 8)
        );
        let tree = button_cell(&b("Árbol", 'R'), true);
        assert_eq!((tree.text.as_str(), tree.letter_at), ("Árbol", 1));
        let foreign = button_cell(&b("Log", 'Q'), true);
        assert_eq!(
            (foreign.text.as_str(), foreign.letter_at, foreign.width),
            ("Q Log", 0, 7)
        );
        let letter_only = button_cell(&b("Sitios", 'S'), false);
        assert_eq!((letter_only.text.as_str(), letter_only.width), ("S", 3));
        let row = [b("Sitios", 'S'), b("Visor", 'V')];
        assert!(names_fit(&row, 15) && !names_fit(&row, 14));
    }

    /// Every panel has a short name in BOTH languages.
    ///
    /// Without it, the letter falls back to the menu label, which is a
    /// phrase: "Places panel", "Metadata panel" and "Processes panel" all
    /// three start with `P` and the bar would stop distinguishing
    /// anything.
    #[test]
    fn every_panel_has_a_short_name_in_both_languages() {
        // From the REGISTRY and not a list written here: with the list,
        // adding a kind and forgetting its translation left this test
        // green and painted a Spanish-speaking reader the id's English
        // initial.
        let reg = registry();
        let panels: Vec<&str> = reg
            .decls()
            .iter()
            .filter(|d| es_button(d))
            .map(|d| d.id.as_str())
            .collect();
        assert!(panels.len() >= 6, "the registry lost panels: {panels:?}");
        for kind in panels {
            for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
                let key = format!("panelbar-{kind}");
                let name = norte_i18n::t_in(lang, &key);
                assert_ne!(name, key, "{lang:?}: missing \"{key}\"");
            }
        }
    }

    /// Two buttons cannot share a letter: they would be indistinguishable.
    #[test]
    fn letters_are_not_repeated() {
        let b = buttons(&registry(), PanelBarInput::default());
        let mut seen = Vec::new();
        for x in &b {
            assert!(
                !seen.contains(&x.letter),
                "\"{}\" repeats the letter {}: {b:?}",
                x.kind,
                x.letter
            );
            seen.push(x.letter);
        }
    }

    /// Three distinct states, and focus beats being open: a button that
    /// only said "open" would not say where the keyboard is, which is
    /// half of what is asked when looking at the bar.
    #[test]
    fn closed_open_and_holding_the_keyboard_are_distinguished() {
        let open = ["places", "log"];
        let b = buttons(
            &registry(),
            PanelBarInput {
                open: &open,
                focused: Some("log"),
                attention: &[("processes", 3), ("tree", 0)],
            },
        );
        let of = |k: &str| b.iter().find(|x| x.kind == k).expect("is there").clone();
        assert_eq!(of("places").state, PanelState::Open);
        assert_eq!(of("log").state, PanelState::Focused);
        assert_eq!(of("tree").state, PanelState::Closed);
        // And novelty is independent of being open: a closed panel with
        // something to count is exactly the case that makes you look at
        // the bar.
        // The FIGURE travels as is, and a zero is having nothing to count.
        assert_eq!(of("processes").attention, 3);
        assert_eq!(of("processes").state, PanelState::Closed);
        assert_eq!(of("log").attention, 0);
        assert_eq!(of("tree").attention, 0);
    }
}
