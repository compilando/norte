//! The theme and the volumes picker.
//!
//! Two small surfaces and a single idea: show what is there without being
//! able to touch it yet.
//!
//! - The **theme** is seen from the inside: what color each ROLE has, which
//!   is what a norte theme really names, and what effects it declares that
//!   this window does not know how to paint. Saying so is half the contract:
//!   a retro theme that does not look different is a theme the user believes
//!   is broken.
//! - **Volumes** are chosen and navigated to, which is reading.
//!
//! The CONNECTIONS picker that task 4.5 names alongside these is not here,
//! and the absence is a decision: reading `connections.toml` would force
//! pulling `norte-connect` — with russh, opendal, suppaftp, age and the
//! keyring — into this window, for a list that still cannot open a single
//! connection. It arrives with phase 5, which needs that crate anyway; until
//! then `pane.connect` answers "not here", which is true.

use norte_i18n::Lang;
use norte_proto::VPath;

use crate::bridge::clamp_display;
use crate::dto::{PickerRowView, PickerView, ThemeRoleView, ThemeView};

/// The `[effects]` keys the window interprets (spec 2026-09-11, V6). Everything
/// else is shown in the theme view as "unsupported".
pub const WINDOW_EFFECTS: &[&str] = &["backdrop"];

/// The MAPPING: CSS variable name, the role that fills it, and whether it
/// takes that role's background (`true`) or foreground (`false`).
///
/// It is a table and not a sequence of calls because two separate questions
/// are needed, and they used to be answered together: **which names exist**
/// (independent of any theme, see [`theme_names`]) and **what colors THIS
/// theme has** (see [`theme_roles`], which omits what the theme leaves
/// unsaid). Mixed together, a theme that did not define a role made its name
/// disappear from the list, and the renderer's orphaned-variable guard read
/// that absence as "nobody feeds that variable".
///
/// A role can appear TWICE, once per side: `Role::Selection` fills
/// `selection-bg` and `selection-fg`, and the terminal uses it as a whole
/// style.
const CSS_VARIABLE_MAP: &[(&str, norte_theme::Role, bool)] = {
    use norte_theme::Role;
    &[
        ("bg", Role::Background, true),
        ("fg", Role::Regular, false),
        ("panel-bg", Role::PaneBackground, true),
        ("panel-focus-bg", Role::PaneFocusBackground, true),
        ("border", Role::BorderUnfocused, false),
        ("border-focus", Role::BorderFocus, false),
        ("selection-bg", Role::Selection, true),
        ("selection-fg", Role::Selection, false),
        // The UNFOCUSED pane's cursor and the dialog buttons (spec
        // 2026-09-10): two new roles, two new pairs.
        ("selection-unfocused-bg", Role::SelectionUnfocused, true),
        ("selection-unfocused-fg", Role::SelectionUnfocused, false),
        ("button-bg", Role::Button, true),
        ("button-fg", Role::Button, false),
        ("mark-bg", Role::Mark, true),
        ("hostile-fg", Role::HostileBadge, false),
        ("status-bg", Role::StatusBar, true),
        // And its foreground. It was missing, and `Role::StatusBar` is a
        // PAIR: the terminal uses it as a whole style. Sending only the
        // background, everything the window paints on top has to guess the
        // text — the viewer's header used to guess `title-fg`, and with a
        // theme whose status bar is light that is light on light: the path,
        // the encoding, the EOL and the losses came out INVISIBLE. A viewer
        // that does not say what it is looking at lies by omission.
        ("status-fg", Role::StatusBar, false),
        ("title-fg", Role::Title, false),
        ("error-fg", Role::Error, false),
        // The two roles a plugin DECORATION can ask for besides `error`.
        // Without them, a `warning` badge fell back to the title's color and
        // was indistinguishable from an `info` one: the role is a closed
        // vocabulary precisely so it means something on screen.
        ("warning-fg", Role::Warning, false),
        ("info-fg", Role::Info, false),
        // The window's chrome (spec 2026-09-11, F2). These ten are NOT in
        // `Role::CORE`, so a theme can leave them unsaid — and the eight
        // long-standing presets do — and then the style sheet DERIVES them
        // from a color the theme does have:
        // `var(--hover, var(--panel-focus-bg))`. That is why its name
        // exists here even when its color does not arrive.
        ("hover", Role::Hover, true),
        ("input-bg", Role::InputBackground, true),
        ("input-border", Role::InputBorder, false),
        ("widget-bg", Role::WidgetBackground, true),
        ("widget-shadow", Role::WidgetShadow, false),
        ("badge-bg", Role::Badge, true),
        ("badge-fg", Role::Badge, false),
        ("scrollbar-slider", Role::ScrollbarSlider, true),
        ("separator", Role::Separator, false),
        ("focus-border", Role::FocusBorder, false),
        ("muted", Role::Muted, false),
        // The listing's striped rows (spec 2026-09-20). BACKGROUND only: the
        // name's color is still set by `[files.ext]`, and a stripe that also
        // recolored the name would hide what CLASS the file is.
        ("stripe-bg", Role::Stripe, true),
    ]
};

/// The CSS variable names the window knows, whether or not they exist in a
/// given theme. This is the AGREEMENT with `style.css`, and the renderer's
/// orphaned-variable guard checks it (`tests/variables_de_tema.rs`).
#[must_use]
pub fn theme_names() -> Vec<&'static str> {
    CSS_VARIABLE_MAP.iter().map(|(n, _, _)| *n).collect()
}

/// The theme's roles with their color, in the order they are named. A role
/// the theme does NOT define is omitted: the style sheet derives it (see this
/// module's `CSS_VARIABLE_MAP` table), and sending a made-up color from here
/// would take away that possibility.
///
/// The mapping is EXPLICIT and not automatic: a style sheet variable nobody
/// feeds is still visible (the default value stays), but an automatic dump of
/// `Role` would turn every new role into a variable nobody uses and every
/// rename into a color that disappears without a sound.
///
/// Lives HERE and not in whoever hosts it, even though the names are its CSS
/// variables', for a specific reason: since the theme picker chooses, the
/// host has to resolve by name a theme nobody handed it, and two lists — one
/// to paint and another to show — is exactly what the original comment said
/// could not happen. Whoever hosts it consumes this.
#[must_use]
pub fn theme_roles(theme: &norte_theme::Theme) -> Vec<(String, String)> {
    CSS_VARIABLE_MAP
        .iter()
        .filter_map(|&(name, role, background)| {
            let style = theme.style(role);
            let color = if background { style.bg } else { style.fg };
            color.map(|c| (name.to_owned(), c.to_hex()))
        })
        .collect()
}

/// The theme this window currently has set.
///
/// The role → color mapping is [`theme_roles`], the same one that feeds the
/// CSS variables of whoever hosts it: what is seen on this screen is what it
/// paints.
#[derive(Debug, Clone, Default)]
pub struct HostTheme {
    /// What it is called.
    pub name: String,
    /// Each role with its `#rrggbb` color, in declaration order.
    pub roles: Vec<(String, String)>,
    /// The effects the theme declares. ALL are "unsupported" today: this
    /// renderer is a webview and does not interpret any of them.
    pub effects: Vec<String>,
    /// The WHOLE theme, not just its roles.
    ///
    /// Needed because `[files.kind]` and `[files.ext]` cannot be projected as
    /// CSS variables: roles are a CLOSED set and extensions are OPEN — a
    /// theme can color `.rs`, `.parquet` or whatever it feels like — so there
    /// is no list of names to declare ahead of time. ONE entry's color is
    /// resolved here, against its name's bytes, and travels in its row; which
    /// is what the terminal has always done (`norte_tui::theme`).
    pub resolved: norte_theme::Theme,
    /// The `[ui] theme_light` variant, already resolved, if there is one.
    ///
    /// Variants have existed since V6 and until now only traveled as CSS
    /// VARIABLES, which the renderer plugs in according to
    /// `prefers-color-scheme`. With entries' colors baked into the row
    /// (bridge 66) that stops being enough: the host has to resolve against
    /// the SAME variant the renderer is painting, or half the screen comes
    /// out in the other theme. In a `Box` because `HostTheme` travels INSIDE
    /// the startup future, and two inline `Theme`s crossed
    /// `clippy::large_futures`'s threshold — which is not the lint being
    /// fussy: that future moves whole between `await`s. This is cold data,
    /// read once per row.
    pub variant_clara: Option<Box<norte_theme::Theme>>,
    /// The `[ui] theme_dark` one. See [`HostTheme::variant_clara`].
    pub variant_dark: Option<Box<norte_theme::Theme>>,
}

/// How the THEME paints an entry's name (`[files.ext]`, which wins, or
/// `[files.kind]`).
///
/// All zero = the theme says nothing about it. These are the four attributes
/// a webview knows how to paint; see [`HostTheme::entry_style`] for why
/// `bg` and `reverse` are not here.
// Four INDEPENDENT terminal-style flags, not an enum or packed flags: they
// are a literal subset of `norte_theme::Style`, which carries this same
// `expect` for the same reason. Packing them here would force unpacking them
// at the wire boundary, which is where they become four again.
#[expect(
    clippy::struct_excessive_bools,
    reason = "subset of norte_theme::Style: four independent attributes"
)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryStyle {
    /// `#rrggbb`, or empty. Already validated.
    pub color: String,
    /// Bold (a directory, an executable).
    pub bold: bool,
    /// Dimmed (the compressed files in the retro presets).
    pub dim: bool,
    /// Italic.
    pub italic: bool,
    /// Underline.
    pub underline: bool,
}

impl HostTheme {
    /// The theme with this name, resolved.
    ///
    /// The name that gets stored is the REQUESTED one, and the colors are
    /// from the theme that was actually resolved: with the factory presets
    /// they are always the same, and whoever calls this has already checked
    /// it exists.
    #[must_use]
    pub fn de(name: &str, theme: &norte_theme::Theme) -> Self {
        Self {
            name: name.to_owned(),
            roles: theme_roles(theme),
            // Effects the window DOES interpret are not shown as
            // "unsupported": `backdrop` (spec 2026-09-11, V6) is translated
            // by the window's catalog into a CSS variable.
            effects: theme
                .effect_names()
                .unwrap_or_default()
                .into_iter()
                .filter(|e| !WINDOW_EFFECTS.contains(&e.as_str()))
                .collect(),
            resolved: theme.clone(),
            // Set by whoever starts up, who is the only one that reads the
            // configuration; `de` builds the BASE theme.
            variant_clara: None,
            variant_dark: None,
        }
    }

    /// The theme to paint with, according to the scheme the desktop asks for.
    ///
    /// **The same rule as the renderer's `themeFor`** (`ui/src/main.ts`), and
    /// it is written twice for a specific reason: the renderer needs the CSS
    /// variables SYNCHRONOUSLY at startup — going through the host would cost
    /// it a flash of the wrong theme — and the host needs the whole `Theme`
    /// to resolve `[files.ext]`, which does not fit in variables. What
    /// prevents them from drifting apart is
    /// `the_variant_rule_is_the_renderers`, which pins them against the
    /// same three cases.
    #[must_use]
    pub fn for_scheme(&self, dark: bool) -> &norte_theme::Theme {
        let variant = if dark {
            self.variant_dark.as_ref()
        } else {
            self.variant_clara.as_ref()
        };
        variant.map_or(&self.resolved, Box::as_ref)
    }

    /// The color and weight an entry's NAME is painted with, according to
    /// `[files.ext]` (wins) and `[files.kind]` of the theme.
    ///
    /// `name` is the name's BYTES (rule 1): the extension is matched against
    /// bytes, never against a string, because a name does not have to be
    /// UTF-8 and the masking done for painting is not injective — two
    /// different names can paint the same and do not share an extension
    /// because of it.
    ///
    /// `dark` is the scheme the desktop asks for: it is resolved against
    /// the VARIANT the renderer is painting (see [`Self::for_scheme`]) and
    /// not against `[ui] theme` alone. With `theme_light`/`theme_dark` set,
    /// resolving against the base left names with the OTHER theme's colors —
    /// and a `dir` that is blue in a dark theme over the light theme's white
    /// gives 2.6:1.
    ///
    /// All zero = the theme says nothing about this entry and the renderer
    /// uses the listing's normal color. The resolved `regular` is not
    /// returned on purpose: sending it on every row would be six bytes per
    /// entry to repeat what the style sheet already knows.
    ///
    /// All FOUR attributes a webview knows how to paint travel, not just the
    /// color: `retro-crt` and `retro-crt-amber` dim `zip`/`tar`/`gz` with
    /// `dim = true`, so carrying only `fg` left those files dimmed in the
    /// terminal and at full brightness in the window — the kind of silent
    /// divergence ADR 0077 exists to prevent. `bg` and `reverse` are left
    /// out and that IS a decision: a row's background is already contested
    /// by the cursor, the hover and the mark, and adding a fifth owner would
    /// let the theme hide where the cursor is.
    #[must_use]
    pub fn entry_style(&self, name: &[u8], kind: norte_theme::FileKind, dark: bool) -> EntryStyle {
        self.for_scheme(dark)
            .files
            .style_for(name, kind)
            .map_or_else(EntryStyle::default, |s| EntryStyle {
                // Through `valid_color` like any other color that ends up in
                // a CSS property: today `to_hex` is total and cannot produce
                // anything else, but that invariant was held up by the
                // callers and not the type, and this is the third one.
                color: s
                    .fg
                    .map(norte_theme::Color::to_hex)
                    .map(|c| valid_color(&c))
                    .unwrap_or_default(),
                bold: s.bold,
                dim: s.dim,
                italic: s.italic,
                underline: s.underline,
            })
    }

    /// The projection.
    #[must_use]
    pub(crate) fn vista(&self) -> ThemeView {
        ThemeView {
            name: clamp_display(self.name.clone()),
            // The list and the cursor are set by whoever holds the SELECTOR:
            // this type is the theme that is set, not the choice in
            // progress.
            choices: Vec::new(),
            cursor: 0,
            roles: self
                .roles
                .iter()
                .map(|(role, color)| ThemeRoleView {
                    role: clamp_display(role.clone()),
                    color: valid_color(color),
                })
                .collect(),
            unsupported_effects: self
                .effects
                .iter()
                .map(|e| {
                    // The key comes from the theme file: it gets masked, and
                    // it is SAID that it was masked (#266).
                    let (paintable, hostile) = norte_frontend::display_name(e.as_bytes());
                    crate::dto::ThemeEffectView {
                        key: clamp_display(paintable),
                        hostile,
                    }
                })
                .collect(),
        }
    }
}

/// A `#rrggbb` color, or empty.
///
/// The renderer puts it into
/// `style.setProperty("background-color", …)`. Today it always comes from
/// `Theme::to_hex()`, so it is safe — but the invariant was held up by ONE
/// caller and nothing said so in the type. The CSSOM drops a value that does
/// not parse instead of splitting it on `;`, so this is not an injection
/// hole; it is that the guarantee was not written down anywhere.
///
/// One that does not match is sent EMPTY: the unpainted swatch says the theme
/// has an invalid color, and an arbitrary string in a CSS property says
/// nothing.
fn valid_color(color: &str) -> String {
    let valid = color.len() == 7
        && color.starts_with('#')
        && color[1..].bytes().all(|b| b.is_ascii_hexdigit());
    if valid {
        color.to_owned()
    } else {
        String::new()
    }
}

/// An open picker.
pub(crate) struct Selector {
    rows: Vec<Row>,
    cursor: usize,
    /// The list is empty and this is the Fluent key that explains it.
    empty_key: &'static str,
    /// What it is called, as a Fluent key. It was PINNED to the volumes one,
    /// which used to be the only one; with three, a fixed title lies about
    /// two of them.
    title: &'static str,
    /// Which slot the chosen entry navigates to.
    ///
    /// Explicit and not "the active one": `pane.select-drive-left` names a
    /// SIDE of the screen, and the side is resolved when OPENING. Reading it
    /// at choice time would let moving focus while the list is up change
    /// which pane ends up mounting the volume.
    slot: u32,
    /// Which list this is, for the verbs that only mean something on some of
    /// them.
    ///
    /// It used to be a `bool` for "is it the favorites one" while there was
    /// ONE editable list (#309). With history and the popular ones (spec
    /// 2026-09-15 D2) there are three, and three bools would be three fields
    /// that can contradict each other.
    kind: SelectorKind,
    /// A history list's filter while it is being typed (spec 2026-09-15 D2).
    /// `None` when unfiltered and in the other lists.
    filter: Option<String>,
}

/// Which list a picker is, as far as its verbs are concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectorKind {
    /// Volumes, connections: they are chosen and nothing more.
    Other,
    /// Favorites: they are added and removed (#309).
    Hotlist,
    /// A slot's history: it is removed and cleared.
    History,
    /// The session's popular ones: same as history.
    Popular,
}

/// A row with what is needed to ACT, besides what is needed to paint.
struct Row {
    view: PickerRowView,
    /// Where it navigates to.
    destination: Option<VPath>,
    /// The RAW name, when the row can be edited (#309): it is the key a
    /// favorite is removed from `norte.toml` with, and it cannot come from
    /// the label, which is sanitized and clamped for painting.
    name: Option<String>,
}

impl Selector {
    /// The volumes picker, still without its list: it is requested and
    /// arrives later.
    pub(crate) fn volumes(slot: u32) -> Self {
        Self::volumes_with_title(slot, "picker-volumes-title")
    }

    /// The volumes picker for one SIDE of the screen.
    ///
    /// The title says so, because nothing else can: the slot does not cross
    /// the bridge and both sides open the same list. In Total Commander the
    /// window's position says so; here, with focus on the other pane, without
    /// the title there is no way to know where it is going to mount until it
    /// mounts (ADR 0058 D9, #293).
    pub(crate) fn side_volumes(slot: u32, right: bool) -> Self {
        Self::volumes_with_title(
            slot,
            if right {
                "picker-volumes-title-right"
            } else {
                "picker-volumes-title-left"
            },
        )
    }

    fn volumes_with_title(slot: u32, title: &'static str) -> Self {
        Self {
            rows: Vec::new(),
            cursor: 0,
            empty_key: "picker-volumes-loading",
            title,
            slot,
            kind: SelectorKind::Other,
            filter: None,
        }
    }

    /// The CONNECTIONS picker, still without its list: it is requested from
    /// the daemon and arrives later (#264).
    ///
    /// Empty on open, like the volumes one and with the same race: the list
    /// comes from a response, so its `generation` is what stops a click
    /// painted over one list from being applied to another.
    pub(crate) fn connections(slot: u32) -> Self {
        Self {
            rows: Vec::new(),
            cursor: 0,
            empty_key: "picker-connections-loading",
            title: "picker-connections-title",
            slot,
            kind: SelectorKind::Other,
            filter: None,
        }
    }

    /// Fills the connections picker with what the daemon answered.
    ///
    /// **The URL is masked as an authority and not as a path**: a host can be
    /// named `banco.example@malo.example` without carrying a single
    /// character that gets masked, and that reads as the userinfo of a
    /// legitimate host. It is the same care as the degraded-session notice,
    /// and for the same reason: here "what machine am I connecting to?" is
    /// the only question.
    ///
    /// What gets navigated is the URL: going there ESTABLISHES the session
    /// the usual way. One that does not parse as a `VPath` is shown with no
    /// destination — it is visible that it is configured and cannot be
    /// opened, which is more honest than hiding it.
    ///
    /// The ones the daemon could not READ (#365) enter through the same door
    /// and behind the good ones: with no destination, and with the reason
    /// where the URL would go. Before 0.84.0 none of them arrived, because a
    /// single bad entry made the call fail and the picker opened empty with
    /// an error.
    pub(crate) fn with_connections(
        &mut self,
        connections: Vec<norte_proto::methods::ConnectionEntry>,
        unusable: Vec<norte_proto::methods::ConnectionProblem>,
    ) {
        self.rows = connections
            .into_iter()
            .map(|c| {
                let (name, name_hostile) = norte_frontend::display_name(c.name.as_bytes());
                let (url, url_hostile) = norte_frontend::display_name(c.url.as_bytes());
                Row {
                    view: PickerRowView {
                        label: clamp_display(name),
                        hostile: name_hostile || url_hostile,
                        detail: clamp_display(url),
                    },
                    destination: VPath::parse(&c.url).ok(),
                    name: None,
                }
            })
            .chain(unusable.into_iter().map(|p| {
                let (name, name_hostile) = norte_frontend::display_name(p.name.as_bytes());
                // The reason was written by a parser over a user file, so it
                // gets masked just like a name: it is outside text, not one
                // of our own strings.
                let (reason, reason_hostile) = norte_frontend::display_name(p.reason.as_bytes());
                Row {
                    view: PickerRowView {
                        label: clamp_display(name),
                        hostile: name_hostile || reason_hostile,
                        detail: clamp_display(reason),
                    },
                    destination: None,
                    name: None,
                }
            }))
            .collect();
        self.cursor = 0;
        self.empty_key = if self.rows.is_empty() {
            "picker-connections-empty"
        } else {
            ""
        };
    }

    /// A history list — a slot's or the session's popular ones — from the
    /// SHARED rows ([`norte_frontend::history::history_rows`]).
    ///
    /// Which rows come out, in what order and with what mark cannot depend on
    /// who paints it: it is decided by the shared crate, same as in the
    /// terminal. The mark ("here", "forward") goes in the row's detail, and
    /// the cursor starts on the one right after the current one.
    ///
    /// `paint` puts the path on screen — with the pane's reinterpretation,
    /// or with none for the popular ones — and it is decided by whoever knows
    /// which pane the list belongs to.
    pub(crate) fn history(
        slot: u32,
        rows: &[norte_frontend::history::HistoryRow],
        paint: impl Fn(&VPath) -> (String, bool),
        lang: Lang,
        title: &'static str,
        popular: bool,
        filter: Option<String>,
    ) -> Self {
        let projected = rows
            .iter()
            .map(|r| {
                let (paintable, hostile) = paint(&r.path);
                let detail = norte_frontend::history::mark_key(r.mark)
                    .map_or_else(String::new, |k| norte_i18n::t_in(lang, k));
                Row {
                    view: PickerRowView {
                        label: clamp_display(paintable),
                        hostile,
                        detail: clamp_display(detail),
                    },
                    destination: Some(r.path.clone()),
                    name: None,
                }
            })
            .collect();
        Self {
            rows: projected,
            cursor: norte_frontend::history::start_cursor(rows),
            empty_key: if popular {
                "picker-popular-empty"
            } else {
                "picker-history-empty"
            },
            title,
            slot,
            kind: if popular {
                SelectorKind::Popular
            } else {
                SelectorKind::History
            },
            filter,
        }
    }

    /// The configuration's favorites.
    ///
    /// A favorite whose path does not parse STAYS, with its notice and no
    /// destination: the hotlist is user data, and one that disappears
    /// silently is a failure nobody can see (same criterion as the side
    /// panel).
    pub(crate) fn hotlist(
        slot: u32,
        favorites: &[(String, Result<VPath, String>)],
        lang: Lang,
    ) -> Self {
        let rows = favorites
            .iter()
            .map(|(name, destination)| {
                // A favorite's name is BYTES just as much as a path: a person
                // wrote it into a file and it can carry bidi.
                let (name_paintable, name_hostile) = norte_frontend::display_name(name.as_bytes());
                let (detail, detail_hostile, resolved_destination) = match destination {
                    Ok(p) => {
                        let (paintable, hostile) = norte_frontend::display::path_display(p);
                        (paintable, hostile, Some(p.clone()))
                    }
                    Err(_) => (norte_i18n::t_in(lang, "hotlist-invalid"), false, None),
                };
                Row {
                    view: PickerRowView {
                        label: clamp_display(name_paintable),
                        hostile: name_hostile || detail_hostile,
                        detail: clamp_display(detail),
                    },
                    destination: resolved_destination,
                    // The RAW name travels with the row: it is what the
                    // favorite gets removed from `norte.toml` with (#309).
                    name: Some(name.clone()),
                }
            })
            .collect();
        Self {
            rows,
            cursor: 0,
            empty_key: "picker-hotlist-empty",
            title: "picker-hotlist-title",
            slot,
            kind: SelectorKind::Hotlist,
            filter: None,
        }
    }

    /// Which slot what gets chosen here navigates to.
    pub(crate) fn slot(&self) -> u32 {
        self.slot
    }

    /// Is this the FAVORITES picker? (#309)
    ///
    /// Asked by whoever handles `dialog.add`/`dialog.remove`: favorites are
    /// the only list in this window that is edited — volumes are mounted by
    /// the system and layouts are saved through another path — so those two
    /// verbs only mean something here.
    pub(crate) fn es_hotlist(&self) -> bool {
        self.kind == SelectorKind::Hotlist
    }

    /// Is this a HISTORY list, a slot's or the popular ones? Asked by
    /// whoever handles `dialog.remove`/`dialog.clear` (spec 2026-09-15 D2).
    pub(crate) fn es_history(&self) -> bool {
        matches!(self.kind, SelectorKind::History | SelectorKind::Popular)
    }

    /// Is it the popular-ones one?
    pub(crate) fn es_popular(&self) -> bool {
        self.kind == SelectorKind::Popular
    }

    /// The cursor's row, to rebuild the list without losing the spot.
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    /// The title's Fluent key, to rebuild the list with the same one.
    pub(crate) fn title(&self) -> &'static str {
        self.title
    }

    /// A history list's filter, if it is being filtered.
    pub(crate) fn filter(&self) -> Option<&str> {
        self.filter.as_deref()
    }

    /// The cursor row's RAW NAME, unpainted.
    ///
    /// Raw and not the view's label: what gets painted is sanitized and
    /// clamped, and removing a favorite by its label would delete the wrong
    /// one — or none — as soon as the name carried bidi or measured too long.
    pub(crate) fn name_raw(&self) -> Option<&str> {
        self.rows.get(self.cursor)?.name.as_deref()
    }

    /// Feeds in the volumes the host answered with.
    pub(crate) fn set_volumes(&mut self, vols: &[norte_proto::methods::Volume], lang: Lang) {
        self.empty_key = "picker-volumes-empty";
        self.rows = vols
            .iter()
            .map(|v| {
                // A mount point is a `VPath`, i.e. BYTES: it is painted the
                // shared way and travels with its mark.
                let (paintable, hostile) = norte_frontend::display::path_display(&v.mount);
                let (detail, detail_hostile) = detail_of(v, lang);
                Row {
                    view: PickerRowView {
                        label: clamp_display(paintable),
                        // The mount point OR the LABEL. The label is
                        // `Option<Vec<u8>>` and on Windows crosses as WTF-8:
                        // a lone surrogate a FAT/NTFS label can legally carry
                        // survives instead of turning into U+FFFD. It used to
                        // be masked and the mark got THROWN AWAY, while the
                        // SAME label in the side panel was marked: two
                        // surfaces, two answers, the same bytes.
                        hostile: hostile || detail_hostile,
                        detail: clamp_display(detail),
                    },
                    destination: Some(v.mount.clone()),
                    name: None,
                }
            })
            .collect();
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
    }

    /// Moves the cursor without going out of bounds.
    pub(crate) fn mover(&mut self, delta: i64) {
        if self.rows.is_empty() {
            return;
        }
        let target = i64::try_from(self.cursor)
            .unwrap_or(0)
            .saturating_add(delta);
        self.cursor = usize::try_from(target.max(0))
            .unwrap_or(0)
            .min(self.rows.len() - 1);
    }

    /// Puts the cursor on a row (a click). Out of range does nothing.
    pub(crate) fn point_at(&mut self, row: usize) {
        if row < self.rows.len() {
            self.cursor = row;
        }
    }

    /// There is a row under the cursor, whether it has a destination or not.
    ///
    /// Distinguishes "the list is empty" from "this row does not lead
    /// anywhere" — a favorite whose path does not parse — which are two
    /// different answers and without this were both answered the same way:
    /// with silence.
    pub(crate) fn hay_row(&self) -> bool {
        self.rows.get(self.cursor).is_some()
    }

    /// Where the cursor's row navigates to, if there is one.
    pub(crate) fn choose(&self) -> Option<VPath> {
        self.rows.get(self.cursor)?.destination.clone()
    }

    /// The projection.
    pub(crate) fn vista(&self, lang: Lang) -> PickerView {
        PickerView {
            // A history's filter is SAID in the title (spec 2026-09-15 D2):
            // without seeing it, the list shrinks for no apparent reason.
            // Masked: the reader types it, but a paste can smuggle in bidi.
            title: clamp_display(match &self.filter {
                Some(f) => format!(
                    "{} — /{}",
                    norte_i18n::t_in(lang, self.title),
                    norte_frontend::display_name(f.as_bytes()).0
                ),
                None => norte_i18n::t_in(lang, self.title),
            }),
            rows: self.rows.iter().map(|f| f.view.clone()).collect(),
            cursor: (!self.rows.is_empty()).then_some(self.cursor as u64),
            empty: if self.rows.is_empty() {
                clamp_display(norte_i18n::t_in(lang, self.empty_key))
            } else {
                String::new()
            },
            // Set by the controller, which is the one that knows how many
            // times the set has changed: the picker does not learn about its
            // own reopenings.
            generation: 0,
        }
    }
}

/// A volume's detail: its file system, the space, and whether it is
/// read-only.
///
/// Space the system did not answer is SAID, a `0` is never painted: zero free
/// reads as "full", which is the opposite of "unknown".
///
/// ALSO returns whether what is painted differs from the real thing: the
/// label is given by the system and is bytes, so the flag is produced by this
/// function and whoever calls it has to carry it to the row. It used to be
/// computed and thrown away.
fn detail_of(v: &norte_proto::methods::Volume, lang: Lang) -> (String, bool) {
    let mut parts: Vec<String> = Vec::new();
    if !v.fs_type.is_empty() {
        parts.push(norte_frontend::display_name(v.fs_type.as_bytes()).0);
    }
    // Space and read-only, through the SHARED crate: here and in the side
    // panel they used to be written separately and already differed.
    parts.push(norte_frontend::places::PlacesState::volume_detail(
        v.free_bytes,
        v.total_bytes,
        v.read_only,
        false,
        lang,
    ));
    // The label the system gives is BYTES — no platform promises UTF-8 — so
    // it enters through the same path as a file name.
    let mut hostile = false;
    if let Some(label) = &v.label {
        let (paintable, h) = norte_frontend::display_name(label);
        hostile = h;
        parts.push(paintable);
    }
    (parts.join(" · "), hostile)
}

#[cfg(test)]
mod tests {
    use super::{EntryStyle, HostTheme, theme_names, theme_roles};

    /// **A PAIRED role crosses with both its halves.**
    ///
    /// `Role::StatusBar` is background AND text: the terminal applies it as a
    /// whole style. Only the background used to travel here, so everything
    /// the window painted on top had to GUESS the text color — the viewer's
    /// header used to guess `title-fg`, and with a theme whose status bar is
    /// light that is light on light: the path, the encoding, the EOL and the
    /// losses came out invisible. This was seen painting the real window, not
    /// in a test.
    ///
    /// The list is checked whole and by hand, per what `theme_roles`'s
    /// rustdoc says: it is an agreement with a style sheet that shares no
    /// types, so removing a key has to go red here instead of being
    /// discovered by looking at the screen.
    #[test]
    fn the_theme_crosses_both_halves_of_the_status_bar() {
        let theme = norte_theme::Theme::preset_default();
        let roles = theme_roles(&theme);
        let names: Vec<&str> = roles.iter().map(|(n, _)| n.as_str()).collect();

        for half in ["status-bg", "status-fg"] {
            assert!(
                names.contains(&half),
                "missing `{half}`: without both, whoever paints on top guesses \
                 — and guessed light on light ({names:?})"
            );
        }

        assert_eq!(
            names,
            [
                "bg",
                "fg",
                "panel-bg",
                "panel-focus-bg",
                "border",
                "border-focus",
                "selection-bg",
                "selection-fg",
                "selection-unfocused-bg",
                "selection-unfocused-fg",
                "button-bg",
                "button-fg",
                "mark-bg",
                "hostile-fg",
                "status-bg",
                "status-fg",
                "title-fg",
                "error-fg",
                "warning-fg",
                "info-fg",
                // The striped rows ARE defined by every preset (spec
                // 2026-09-20), and that is why they travel: the stripe
                // cannot be derived in the style sheet like the rest of the
                // chrome — it is a step over the pane's background that each
                // palette gives differently, and one computed in CSS ends up
                // invisible in one theme and garish in the next.
                "stripe-bg",
            ],
            "what the default preset PROJECTS: leaves unsaid the ten chrome \
             ones, which the sheet derives, and states the striped rows, \
             which are not derived"
        );

        // The agreement with `style.css` is `theme_names`, not the list
        // above: the names exist even when the theme does not fill them, and
        // confusing the two is what made the renderer's orphan guard read
        // "nobody feeds this" where it actually said "this theme does not
        // say so".
        let all_names = theme_names();
        for n in &names {
            assert!(
                all_names.contains(n),
                "`{n}` is projected and is not in the agreement"
            );
        }
        for chrome in [
            "hover",
            "input-bg",
            "input-border",
            "widget-bg",
            "widget-shadow",
            "badge-bg",
            "badge-fg",
            "scrollbar-slider",
            "separator",
            "focus-border",
            "muted",
        ] {
            assert!(
                all_names.contains(&chrome),
                "missing `{chrome}` in the agreement with the sheet"
            );
            assert!(
                !names.contains(&chrome),
                "`{chrome}` should not be projected: the default preset does \
                 not define it, and the sheet derives it"
            );
        }

        // And each one carries a real color, not an empty string the sheet
        // would silently accept.
        for (name, color) in &roles {
            assert!(
                color.starts_with('#') && color.len() == 7,
                "`{name}` is not a color: {color:?}"
            );
        }
    }

    /// A test theme with one extension rule and one kind rule.
    fn theme_with_files() -> HostTheme {
        let t = norte_theme::Theme::from_toml(
            "name = \"t\"\n\
             [files.kind]\n\
             dir = { fg = \"#5fafd7\", bold = true }\n\
             [files.ext]\n\
             rs = { fg = \"#d7875f\" }\n\
             zip = { fg = \"#d75f5f\", dim = true }\n",
        )
        .expect("parses");
        HostTheme::de("t", &t)
    }

    /// The extension is matched against BYTES, and that is why a name that
    /// is not valid UTF-8 keeps its color.
    ///
    /// This is the invariant a comment and nothing else used to uphold. The
    /// fixture is `lossy_collapse_ff` from the canonical corpus (`\xFF.rs`):
    /// whoever refactors this into decoding the WHOLE name — which is the
    /// more convenient call, because `text` is already built right there —
    /// will see every test pass, because every test's names are ASCII, and
    /// will silently break every file whose name is not.
    ///
    /// `norte_theme::FileColors::style_for` validates with `from_utf8` ONLY
    /// the extension's chunk, and the separator byte (`.`, 0x2E) cannot
    /// appear inside a multibyte UTF-8 sequence: that is why the cut is safe
    /// and why this works.
    #[test]
    fn the_extension_matches_against_bytes_and_survives_a_non_utf8_name() {
        let theme = theme_with_files();
        let valid = theme.entry_style(b"main.rs", norte_theme::FileKind::Regular, false);
        assert_eq!(valid.color, "#d7875f");

        // `\xFF.rs`: a lone invalid byte. The extension is still `rs` and the
        // color has to be the SAME.
        let hostile = theme.entry_style(b"\xff.rs", norte_theme::FileKind::Regular, false);
        assert_eq!(
            hostile.color, valid.color,
            "a non-UTF8 name lost its extension's color: someone is decoding \
             the whole name"
        );
    }

    /// The FOUR attributes the window knows how to paint travel, not just the
    /// color: `retro-crt` dims the compressed ones with `dim`, and carrying
    /// only `fg` made them come out dimmed in the terminal and at full
    /// brightness in the window.
    #[test]
    fn the_style_attributes_cross_and_not_just_the_color() {
        let theme = theme_with_files();
        let zip = theme.entry_style(b"backup.zip", norte_theme::FileKind::Regular, false);
        assert_eq!(zip.color, "#d75f5f");
        assert!(zip.dim, "the theme's `dim = true` did not reach the row");

        let dir = theme.entry_style(b"src", norte_theme::FileKind::Dir, false);
        assert!(dir.bold, "a directory is bold");
    }

    /// An entry's color comes from the VARIANT the desktop asks for, not
    /// from `[ui] theme` alone.
    ///
    /// With `theme_dark`/`theme_light` set, the renderer plugs in the
    /// variant's variables and the host used to resolve against the base:
    /// the chrome came from one theme and the NAMES from the other. With the
    /// `vscode-*` pair that left directories blue from the dark one over the
    /// light one's white, at 2.6:1 — below the floor those same presets
    /// promise in their header.
    #[test]
    fn the_color_of_an_entry_comes_from_the_desktops_variant() {
        let light =
            norte_theme::Theme::from_toml("name = \"c\"\n[files.ext]\nrs = { fg = \"#895503\" }\n")
                .expect("parses");
        let dark =
            norte_theme::Theme::from_toml("name = \"o\"\n[files.ext]\nrs = { fg = \"#e2c08d\" }\n")
                .expect("parses");
        let mut theme = theme_with_files();
        theme.variant_clara = Some(Box::new(light));
        theme.variant_dark = Some(Box::new(dark));

        let kind = norte_theme::FileKind::Regular;
        assert_eq!(
            theme.entry_style(b"main.rs", kind, true).color,
            "#e2c08d",
            "the desktop asks for dark"
        );
        assert_eq!(
            theme.entry_style(b"main.rs", kind, false).color,
            "#895503",
            "the desktop asks for light"
        );
    }

    /// The variant rule is THE SAME as the renderer's `themeFor`
    /// (`ui/src/main.ts`): that side's variant if there is one, and `theme`
    /// if not.
    ///
    /// It is written twice — the renderer needs the CSS variables
    /// synchronously so it does not flash, the host needs the whole `Theme`
    /// for `[files.ext]` — so what keeps them from drifting apart is this:
    /// the three cases, pinned. If someone changes one of the two, this test
    /// has to change, and changing it reveals the other.
    #[test]
    fn the_variant_rule_is_the_renderers() {
        let base = theme_with_files();
        // No variants: the base rules on both sides.
        assert_eq!(base.for_scheme(true).name.as_deref(), Some("t"));
        assert_eq!(base.for_scheme(false).name.as_deref(), Some("t"));

        // Only the dark one: the light side stays with the base.
        let mut dark_only = theme_with_files();
        dark_only.variant_dark = Some(Box::new(
            norte_theme::Theme::from_toml("name = \"o\"\n").expect("parses"),
        ));
        assert_eq!(dark_only.for_scheme(true).name.as_deref(), Some("o"));
        assert_eq!(dark_only.for_scheme(false).name.as_deref(), Some("t"));
    }

    /// A theme that says nothing about an entry does not invent a color: the
    /// renderer uses the listing's normal one, which the style sheet already
    /// knows.
    #[test]
    fn without_a_rule_there_is_no_color() {
        let theme = theme_with_files();
        let nothing = theme.entry_style(b"notas.txt", norte_theme::FileKind::Regular, false);
        assert_eq!(nothing, EntryStyle::default());
    }
}
