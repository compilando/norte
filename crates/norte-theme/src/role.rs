//! [`Role`]: the SEMANTIC roles a theme styles (ADR 0020 D2). The
//! frontend asks for a role, never for a loose color. Each role carries a
//! monochrome [`fallback`](Role::fallback) that reproduces the M1 look, so
//! that WITHOUT a theme (or with a partial one) the UI stays coherent.

use serde::{Deserialize, Serialize};

use crate::style::Style;

/// Semantic UI role. Adding a variant is non-breaking: a theme that does not
/// cover it inherits its [`fallback`](Role::fallback).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Role {
    /// BASE background of the whole screen. A light theme sets its light `bg` here;
    /// the frontend paints it first and the rest of the styles (`fg` only)
    /// keep it. Unset = the terminal's background (M1 behavior).
    Background,
    /// Normal text / default file entry.
    Regular,
    /// Selected row in a pane.
    Selection,
    /// Border of the focused pane: says which of the panes the navigation
    /// keys go to. It is not [`Role::FocusBorder`], which is the ring of a
    /// CONTROL inside a dialog; the names look alike and mean
    /// different things.
    BorderFocus,
    /// Border of the unfocused pane.
    BorderUnfocused,
    /// Border of a modal/dialog.
    ModalBorder,
    /// Status bar.
    StatusBar,
    /// Pane/modal title.
    Title,
    /// Hostile-name badge (non-printable bytes, control…).
    HostileBadge,
    /// Error message.
    Error,
    /// Warning message (e.g. permanent delete).
    Warning,
    /// Informational message.
    Info,
    /// Highlighted search match.
    Match,
    /// Pane interior background (GUI chrome; the TUI may adopt it later).
    PaneBackground,
    /// Focused pane interior background.
    PaneFocusBackground,
    /// Marked entry (selection marks, distinct from the cursor's
    /// `Selection`) — two different consumers, so a preset's `mark = { bg =
    /// ... }` reads differently in each: the GUI applies it as the marked
    /// row's BACKGROUND across the whole row; the TUI applies it only as the
    /// style of a one-cell gutter glyph (`*`) at the start of the row, so the
    /// same `bg` shows up as a small tinted cell rather than a full-row
    /// background. Style it with `bg` only (no `fg`) so both readings stay
    /// legible.
    Mark,
    /// Cursor row in an UNFOCUSED pane (spec 2026-09-10). It exists
    /// because `Selection` started being painted with the accent color, and two
    /// equally vivid cursors do not say which one gets the keys: the one in the
    /// unfocused pane stays in the old gray, present but subdued.
    SelectionUnfocused,
    /// A dialog button (`[ Enter  Confirm ]`): each modal paints its line
    /// of keys with this role when `[ui] dialog_buttons` is on.
    Button,

    // --- Window chrome (spec 2026-09-11, F2) ----------------------------
    //
    // The ten that follow name SURFACES, not meanings: they are what
    // makes a window look like a specific editor instead of a
    // form. They share three traits that set them apart from the ones above:
    //
    // 1. They are NOT in [`Role::CORE`], so a preset does not have to
    //    define them (see that constant's rustdoc).
    // 2. Their [`Role::fallback`] carries no color. The sensible value CANNOT
    //    be written here: it depends on the theme's palette, and the window's
    //    stylesheet derives it with `var(--hover, var(--panel-focus-bg))`.
    // 3. They are not [`Role::REQUESTABLE`]: a plugin cannot ask for them for a
    //    badge, because the color of the scrollbar slider means nothing
    //    stuck to a file name.
    //
    /// Row under the POINTER, in a pane or in a list. Distinct from the cursor
    /// (`Selection`): the mouse is over it, the keys do not go there. It loses
    /// against the cursor and against a marked row.
    Hover,
    /// Background of a text field (dialogs, palette, settings).
    InputBackground,
    /// Border of a text field. It is the control's outline, not the pane's
    /// (`BorderUnfocused`) nor the focus ring (`FocusBorder`).
    InputBorder,
    /// Background of a floating WIDGET: the command palette, a dropdown,
    /// the menu, the `which-key`. It sits on top of the base background and so
    /// is usually slightly lighter than `PaneBackground`.
    WidgetBackground,
    /// The SHADOW color of those widgets. It exists because a black hardcoded
    /// in the CSS is a shadow that looks like dirt on a light theme.
    WidgetShadow,
    /// A BADGE with a background: a counter, a label.
    ///
    /// It serves TWO consumers, just like [`Role::Mark`], and the
    /// `fg`/`bg` pair reads differently in each: the window uses it as a
    /// side panel's counter, and the log panel as the chip that
    /// marks a daemon line. Define it with `bg` AND `fg`: a chip without a
    /// foreground inherits the color of the line it marks, which is
    /// exactly what the chip has to distinguish.
    Badge,
    /// The scrollbar SLIDER (the track is
    /// transparent). Window only: a terminal paints no scrollbar.
    ScrollbarSlider,
    /// The line separating two chrome SURFACES — the listing's key bar,
    /// the pane bar from the menu bar, the side panel from the
    /// central one.
    ///
    /// It is what enables the "elevation" look of modern
    /// editors: a theme that sets it almost equal to its background stops having
    /// lines without the stylesheet knowing anything about that theme. It is not
    /// the border of a focused or unfocused pane — those are [`Role::BorderFocus`] and
    /// [`Role::BorderUnfocused`], and they mean where the keys go.
    Separator,
    /// The focus ring of a CONTROL: a field, a button, a checkbox.
    ///
    /// Not to be confused with [`Role::BorderFocus`], which is the border of the PANE that
    /// has focus. The names are dangerously alike and say different
    /// things: this one marks which control receives what you type inside a
    /// dialog; that one, which of the two panes receives the navigation
    /// keys.
    FocusBorder,
    /// DIMMED but legible text: breadcrumbs, a size, a secondary
    /// column, a setting's description. It is a color of its own and not a
    /// `dim` over `Regular` because `dim` in a terminal is an attribute that
    /// many emulators ignore.
    Muted,
    /// The background of ODD rows of a listing when "striped rows" is
    /// on (`[ui] row_stripes`). It is the stripe, not the row: the even row
    /// keeps the pane's background, and that is why this role is defined with `bg`
    /// and never with `fg` — the name's color is still decided by
    /// `[files.ext]`, which is what makes a listing legible.
    ///
    /// It loses against everything that MEANS something: the cursor
    /// ([`Role::Selection`]), the mark ([`Role::Mark`]) and the pointer
    /// ([`Role::Hover`]) are painted on top. A stripe that covered the cursor
    /// would turn a reading aid into a lie about where the
    /// keys go.
    ///
    /// "The mark" reads differently in each frontend, just like
    /// [`Role::Mark`] itself: the window paints the whole marked row and covers the
    /// stripe; the terminal only styles the gutter's `*`, so there what
    /// beats the stripe is that cell and not the row.
    ///
    /// It shares with the chrome the three traits above —outside
    /// [`Role::CORE`], outside [`Role::REQUESTABLE`], `fallback` without color—
    /// and for the same reasons, plus one of its own: striped rows with an
    /// invented color is worse than no striping, because the listing is the
    /// most looked-at surface.
    Stripe,
}

impl Role {
    /// The roles a preset is REQUIRED to color: the eighteen that
    /// existed before the window chrome (spec 2026-09-11, F2).
    ///
    /// It is what preset completeness iterates, and not [`Self::ALL`], for
    /// a concrete reason: the ten CHROME roles are derived in the window's
    /// stylesheet from colors the theme already has, so
    /// requiring them from every preset would mean eighty invented values — and the
    /// monochrome [`Self::fallback`] is a bad default for them (a
    /// colorless `hover` is not a cautious hover, it is an invisible one).
    // TODO(translation): review — "eighty" assumes eight presets; there are ten.
    pub const CORE: &'static [Role] = &[
        Role::Background,
        Role::Regular,
        Role::Selection,
        Role::BorderFocus,
        Role::BorderUnfocused,
        Role::ModalBorder,
        Role::StatusBar,
        Role::Title,
        Role::HostileBadge,
        Role::Error,
        Role::Warning,
        Role::Info,
        Role::Match,
        Role::PaneBackground,
        Role::PaneFocusBackground,
        Role::Mark,
        Role::SelectionUnfocused,
        Role::Button,
    ];

    /// The roles a PLUGIN can name in a span or a decoration
    /// (ADR 0037, and the amendment in spec 2026-09-11).
    ///
    /// There is a single criterion: **a plugin describes CONTENT**, so it
    /// can name what a piece of content MEANS —that it is an
    /// error, a warning, a title, a match— and it cannot name
    /// anything the window uses to say what STATE it is in. Two
    /// families are therefore left out:
    ///
    /// - The **chrome** (`hover`, `scrollbar-slider`, `widget-*`, `input-*`,
    ///   `separator`, `focus-border`, `widget-shadow`): a badge painted
    ///   with the scrollbar slider's color does not
    ///   mean anything.
    /// - The **state** (`selection`, `selection-unfocused`, `status-bar`,
    ///   `mark`, `background`, `pane-*`, `border-*`, `modal-border`,
    ///   `button`): where the cursor is, what is marked and which is the focused
    ///   pane are things the plugin does not know and that, painted by it,
    ///   would lie.
    ///
    /// This NARROWS the vocabulary that ADR 0037 left open to all of
    /// [`Self::ALL`]. A non-requestable name degrades to `None` through
    /// [`Self::from_kebab_requestable`] — the same degradation an
    /// unknown name already had, and for the same reason: a newer guest
    /// cannot break the rendering of an older norte.
    pub const REQUESTABLE: &'static [Role] = &[
        Role::Regular,
        Role::Title,
        Role::HostileBadge,
        Role::Error,
        Role::Warning,
        Role::Info,
        Role::Match,
        Role::Badge,
        Role::Muted,
    ];

    /// All roles, for iterating (e.g. checking that kebab names
    /// round-trip). It is [`Self::CORE`] plus the ten chrome ones.
    pub const ALL: &'static [Role] = &[
        Role::Background,
        Role::Regular,
        Role::Selection,
        Role::BorderFocus,
        Role::BorderUnfocused,
        Role::ModalBorder,
        Role::StatusBar,
        Role::Title,
        Role::HostileBadge,
        Role::Error,
        Role::Warning,
        Role::Info,
        Role::Match,
        Role::PaneBackground,
        Role::PaneFocusBackground,
        Role::Mark,
        Role::SelectionUnfocused,
        Role::Button,
        // Window chrome (spec 2026-09-11, F2).
        Role::Hover,
        Role::InputBackground,
        Role::InputBorder,
        Role::WidgetBackground,
        Role::WidgetShadow,
        Role::Badge,
        Role::ScrollbarSlider,
        Role::Separator,
        Role::FocusBorder,
        Role::Muted,
        // The listing's striped rows (spec 2026-09-20).
        Role::Stripe,
    ];

    /// The role's MONOCHROME default style: reproduces the M1 look
    /// (`BOLD`/`REVERSED`/`DIM` where they are today) without color. It is what is used
    /// when the theme does not define the role, so that a user without a theme sees
    /// exactly the usual UI.
    #[must_use]
    pub const fn fallback(self) -> Style {
        match self {
            Role::Selection | Role::StatusBar | Role::Button => Style::new().reverse(),
            // The unfocused cursor: visible without color, but not the same as the
            // one receiving the keys.
            Role::SelectionUnfocused => Style::new().reverse().dim(),
            Role::BorderFocus | Role::ModalBorder | Role::HostileBadge | Role::Title => {
                Style::new().bold()
            }
            // BorderUnfocused/Mark: Mark, distinct from Selection (reverse) but
            // visible without color, shares the dimming of the unfocused border —
            // "present but not active".
            Role::BorderUnfocused | Role::Mark => Style::new().dim(),
            // Background/Regular/Error/Warning/Info/Match: no color by default
            // (the M1 UI did not distinguish them; unset Background = the
            // terminal's background). A colored theme tells them apart.
            // PaneBackground/PaneFocusBackground: new GUI chrome, with no
            // equivalent in the M1 TUI; same treatment as Background
            // (no color = background inherited from the backend).
            //
            // The ten CHROME ones carry no color either, and for a different
            // reason worth stating: their sensible default CANNOT BE
            // WRITTEN HERE. A correct `hover` is "the focused pane's
            // background of THIS theme", and a literal cannot follow eight
            // palettes. The derivation lives in the window's
            // stylesheet (`var(--hover, var(--panel-focus-bg))`), which is the
            // only place where both colors are present at once.
            Role::Background
            | Role::Regular
            | Role::Error
            | Role::Warning
            | Role::Info
            | Role::Match
            | Role::PaneBackground
            | Role::PaneFocusBackground
            | Role::Hover
            | Role::InputBackground
            | Role::InputBorder
            | Role::WidgetBackground
            | Role::WidgetShadow
            | Role::Badge
            | Role::ScrollbarSlider
            | Role::Separator
            | Role::FocusBorder
            | Role::Muted
            // Colorless striped rows are the usual listing: the stripe only
            // exists if a theme paints it. An alternating `dim` would be worse than
            // nothing — it dims the NAME, which is what one comes to read.
            | Role::Stripe => Style::new(),
        }
    }

    /// Parses a kebab-case name (the SAME one this type's serde
    /// serialization produces/consumes, `#[serde(rename_all =
    /// "kebab-case")]`) into the corresponding [`Role`]. `None` if `s` is not a
    /// recognized name from the CLOSED set (ADR 0037, decision 3 and its
    /// responsibility-boundary amendment): validation of a `role`
    /// arriving in plugin data (`SpanWire::role`/`DecorationWire::
    /// role`, `norte-proto`) lives in the FRONTEND that owns the theme
    /// (`norte-core` does not depend on `norte-theme`), and this is the single
    /// entry point — it reuses the existing serde derive as the source of
    /// truth for the name instead of duplicating a match table that could
    /// drift from `#[serde(rename_all = "kebab-case")]`. An unknown
    /// name (from a newer plugin, or from a fork with its own roles)
    /// degrades to `None` — never an error — so that a guest from a future
    /// norte (or another fork) does not break the rendering of an older one.
    ///
    /// ```
    /// use norte_theme::Role;
    /// assert_eq!(Role::from_kebab("hostile-badge"), Some(Role::HostileBadge));
    /// assert_eq!(Role::from_kebab("pane-background"), Some(Role::PaneBackground));
    /// assert_eq!(Role::from_kebab("not-a-role"), None);
    /// assert_eq!(Role::from_kebab(""), None);
    /// ```
    #[must_use]
    pub fn from_kebab(s: &str) -> Option<Role> {
        serde_json::from_value(serde_json::Value::String(s.to_owned())).ok()
    }

    /// [`Self::from_kebab`] restricted to [`Self::REQUESTABLE`]: the SINGLE
    /// entry point for a role name coming from a plugin.
    ///
    /// It exists so that the restriction lives where the list lives, and is not
    /// repeated in every frontend that validates a guest's data.
    ///
    /// ```
    /// use norte_theme::Role;
    /// // A meaning: passes.
    /// assert_eq!(Role::from_kebab_requestable("error"), Some(Role::Error));
    /// // Window chrome: degrades, it is not an error.
    /// assert_eq!(Role::from_kebab_requestable("scrollbar-slider"), None);
    /// // And it still exists for whoever asks without the filter.
    /// assert!(Role::from_kebab("scrollbar-slider").is_some());
    /// ```
    #[must_use]
    pub fn from_kebab_requestable(s: &str) -> Option<Role> {
        Self::from_kebab(s).filter(|r| Self::REQUESTABLE.contains(r))
    }

    /// The kebab name of a role: the exact inverse of [`Self::from_kebab`].
    ///
    /// Needed by frontends that do not share memory with the host —the
    /// graphical renderer receives a STRING, not an enum— and so that a role that
    /// crosses over and back is the same role.
    ///
    /// ```
    /// use norte_theme::Role;
    /// assert_eq!(Role::HostileBadge.as_kebab(), "hostile-badge");
    /// assert_eq!(Role::from_kebab(Role::PaneBackground.as_kebab()), Some(Role::PaneBackground));
    /// ```
    ///
    /// The `match` is exhaustive with no wildcard, so a new role stops
    /// compiling here; and `as_kebab_is_the_serde_name` checks, role by role
    /// over [`Self::ALL`], that it says the same as the serialization — which is
    /// what keeps the two tables from drifting apart.
    #[must_use]
    pub const fn as_kebab(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Regular => "regular",
            Self::Selection => "selection",
            Self::BorderFocus => "border-focus",
            Self::BorderUnfocused => "border-unfocused",
            Self::ModalBorder => "modal-border",
            Self::StatusBar => "status-bar",
            Self::Title => "title",
            Self::HostileBadge => "hostile-badge",
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
            Self::Match => "match",
            Self::Mark => "mark",
            Self::PaneBackground => "pane-background",
            Self::PaneFocusBackground => "pane-focus-background",
            Self::SelectionUnfocused => "selection-unfocused",
            Self::Button => "button",
            Self::Hover => "hover",
            Self::InputBackground => "input-background",
            Self::InputBorder => "input-border",
            Self::WidgetBackground => "widget-background",
            Self::WidgetShadow => "widget-shadow",
            Self::Badge => "badge",
            Self::ScrollbarSlider => "scrollbar-slider",
            Self::Separator => "separator",
            Self::FocusBorder => "focus-border",
            Self::Muted => "muted",
            Self::Stripe => "stripe",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Role;

    /// `as_kebab` says the SAME as serde, role by role.
    ///
    /// Without this they are two tables that drift apart on the first new role: the
    /// `match` stops compiling, yes, but nothing forces the name written
    /// there to be the one that goes over the wire.
    #[test]
    fn as_kebab_is_the_serde_name() {
        for &role in Role::ALL {
            let via_serde = serde_json::to_value(role).expect("serializes");
            assert_eq!(
                via_serde.as_str(),
                Some(role.as_kebab()),
                "{role:?} has a different name depending on who asks"
            );
            assert_eq!(Role::from_kebab(role.as_kebab()), Some(role));
        }
    }

    #[test]
    fn from_kebab_every_role_round_trips() {
        // Single source: if `Role::ALL` gains a variant and its kebab name
        // changes shape, this test exercises it WITHOUT listing the
        // names by hand (avoids the duplication that `from_kebab`'s rustdoc
        // explicitly wants to avoid).
        for &role in Role::ALL {
            let kebab = serde_json::to_value(role)
                .expect("Role serializes")
                .as_str()
                .expect("Role serializes to a string")
                .to_owned();
            assert_eq!(
                Role::from_kebab(&kebab),
                Some(role),
                "kebab roundtrip of {role:?}"
            );
        }
    }

    #[test]
    fn from_kebab_unknown_is_none() {
        // Names a plugin unaware of the theme could send (ADR 0037): they must
        // not panic nor sneak through as a valid Role.
        assert_eq!(Role::from_kebab("number"), None);
        assert_eq!(Role::from_kebab("keyword"), None);
        assert_eq!(Role::from_kebab("HostileBadge"), None); // not kebab-case
    }

    /// `CORE` is a SUBSET of `ALL`, and `ALL` loses nobody.
    ///
    /// The two sets exist because they measure different things: `ALL` is the
    /// whole vocabulary, `CORE` is what a preset is REQUIRED to
    /// color. Without this check, a new role can fall outside both
    /// and exist for nobody.
    #[test]
    fn core_is_a_subset_of_all_and_all_has_everyone() {
        for &r in Role::CORE {
            assert!(Role::ALL.contains(&r), "{r:?} is in CORE and not in ALL");
        }
        assert_eq!(Role::CORE.len(), 18, "CORE is the usual eighteen");
        assert_eq!(
            Role::ALL.len(),
            29,
            "ALL is those plus the ten chrome ones and the stripe"
        );
    }

    /// What a plugin can REQUEST is a subset of what exists, and leaves
    /// out both the chrome and the window's STATE surfaces
    /// (ADR 0037 + spec 2026-09-11, F2).
    #[test]
    fn requestable_leaves_out_chrome_and_state() {
        for &r in Role::REQUESTABLE {
            assert!(
                Role::ALL.contains(&r),
                "{r:?} is requestable and does not exist"
            );
        }
        // Chrome: the slider's color means nothing on a badge.
        for r in [
            Role::ScrollbarSlider,
            Role::WidgetShadow,
            Role::InputBorder,
            Role::Separator,
            Role::Hover,
            // Striped rows are a READING aid for the listing: it depends on which
            // row the entry falls on, which is exactly what a plugin does not know.
            Role::Stripe,
        ] {
            assert!(!Role::REQUESTABLE.contains(&r), "{r:?} is chrome");
        }
        // Window state: where the cursor is, what is marked, which
        // is the focused pane. A plugin describes CONTENT, and knows nothing
        // about that.
        for r in [
            Role::Selection,
            Role::SelectionUnfocused,
            Role::StatusBar,
            Role::Mark,
            Role::Background,
            Role::Button,
        ] {
            assert!(
                !Role::REQUESTABLE.contains(&r),
                "{r:?} is state, not meaning"
            );
        }
        // And the signals a plugin DOES need to say something.
        for r in [
            Role::Error,
            Role::Warning,
            Role::Info,
            Role::Title,
            Role::Match,
            Role::Regular,
            Role::HostileBadge,
            Role::Badge,
            Role::Muted,
        ] {
            assert!(
                Role::REQUESTABLE.contains(&r),
                "{r:?} has to be requestable"
            );
        }
    }

    /// The entry point for a name coming from a plugin: a non-requestable
    /// role degrades to `None`, just like an unknown name. It is not an
    /// error — a guest from a newer norte cannot break the rendering of
    /// an older one (ADR 0037).
    #[test]
    fn from_kebab_requestable_degrades_the_non_requestable_to_none() {
        assert_eq!(Role::from_kebab_requestable("warning"), Some(Role::Warning));
        assert_eq!(Role::from_kebab_requestable("muted"), Some(Role::Muted));
        assert_eq!(Role::from_kebab_requestable("scrollbar-slider"), None);
        assert_eq!(Role::from_kebab_requestable("selection"), None);
        assert_eq!(Role::from_kebab_requestable("does-not-exist"), None);
        // And they are still real roles for whoever asks without the filter.
        assert_eq!(
            Role::from_kebab("scrollbar-slider"),
            Some(Role::ScrollbarSlider)
        );
        assert_eq!(Role::from_kebab("selection"), Some(Role::Selection));
    }

    /// The ten chrome roles are NOT in CORE: they are DERIVED in the window's
    /// stylesheet from colors the theme already has (spec
    /// 2026-09-11, F2), and that is why a preset does not have to define them. Requiring them
    /// would mean eighty invented values spread across the eight presets that
    /// already exist.
    // TODO(translation): review — there are ten presets now, not eight.
    #[test]
    fn chrome_roles_stay_out_of_core() {
        for r in [
            Role::Hover,
            Role::InputBackground,
            Role::InputBorder,
            Role::WidgetBackground,
            Role::WidgetShadow,
            Role::Badge,
            Role::ScrollbarSlider,
            Role::Separator,
            Role::FocusBorder,
            Role::Muted,
        ] {
            assert!(
                !Role::CORE.contains(&r),
                "{r:?} should not be required of every preset"
            );
            assert!(Role::ALL.contains(&r), "{r:?} has to exist");
        }
    }
}
