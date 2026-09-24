//! The menu bar: the same commands as the keyboard, sorted by topic.
//!
//! It adds no capabilities. It adds a way to FIND them: the palette expects
//! you to know the name of what you are looking for and `F1` expects you
//! to read, while a menu is browsed. It is the path for whoever comes from
//! a manager with menus and for whoever uses the mouse, and its content is
//! ids from the shared catalogue — a command that does not exist here
//! cannot appear in a menu.
//!
//! Inside a menu, commands go in SECTIONS (ADR 0125): seventeen entries in
//! a row read as a list that has to be browsed whole, and five groups of
//! three read at a glance. The cursor does not see the sections — it
//! browses the commands as a single list — whoever paints does.

/// A group of commands inside a menu.
#[derive(Debug, Clone, Copy)]
pub struct Section {
    /// Fluent key of the label (`menu-section-*`), or `None` for an
    /// unnamed separation: when the group is self-explanatory, a label is
    /// noise.
    pub title: Option<&'static str>,
    /// Command ids, in the order they are painted.
    pub items: &'static [&'static str],
}

/// A menu: its title and its sections.
#[derive(Debug, Clone, Copy)]
pub struct Menu {
    /// Fluent key of the title (`menu-*`).
    pub title: &'static str,
    /// The sections, top to bottom.
    pub sections: &'static [Section],
}

impl Menu {
    /// How many commands it has, all sections together.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sections.iter().map(|s| s.items.len()).sum()
    }

    /// Does it have no command at all?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The commands in the order they are painted, without sections: this
    /// is what the cursor browses.
    pub fn items(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.sections.iter().flat_map(|s| s.items.iter().copied())
    }

    /// Command `i` of the flat list.
    #[must_use]
    pub fn item(&self, i: usize) -> Option<&'static str> {
        self.items().nth(i)
    }

    /// If a section STARTS at command `i` — and it is not the first one —
    /// its label: `Some(None)` is an unnamed separation, `Some(Some(k))`
    /// one with a label. `None`: `i` continues in the previous one's
    /// section.
    ///
    /// The first section carries no separator: the menu's top rule already
    /// does that. A label on the first one IS painted, and that is why it
    /// is returned.
    #[must_use]
    pub fn section_at(&self, i: usize) -> Option<Option<&'static str>> {
        let mut start = 0;
        for (k, s) in self.sections.iter().enumerate() {
            if start == i && !s.items.is_empty() && (k > 0 || s.title.is_some()) {
                return Some(s.title);
            }
            start += s.items.len();
        }
        None
    }
}

/// What kind of command it is, so it is painted as what it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemRole {
    /// Any regular command.
    Normal,
    /// Deletes, or cannot be undone: painted in the danger color, so the
    /// hand moving down the menu sees it BEFORE clicking it.
    Destructive,
    /// An AI model does it: it carries the `✦` mark, because what it
    /// proposes was not decided by norte and is worth reading before
    /// accepting it.
    Ai,
}

impl ItemRole {
    /// The stable name that crosses the window's bridge.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Destructive => "destructive",
            Self::Ai => "ai",
        }
    }
}

/// A menu command's role, DERIVED from the effect the catalogue declares
/// for it (ADR 0126, which replaces the standalone list here from ADR
/// 0125): deleting is painted as danger and sending data to a model
/// carries `✦`. It was the same fact the read-only window consulted on its
/// own side, in another list.
#[must_use]
pub fn role(id: &str) -> ItemRole {
    match crate::keymap::catalogue::effect(id) {
        Some(crate::keymap::Effect::Destroys) => ItemRole::Destructive,
        Some(crate::keymap::Effect::SendsOut) => ItemRole::Ai,
        _ => ItemRole::Normal,
    }
}

/// Shorthand for declaring a section.
const fn sec(title: Option<&'static str>, items: &'static [&'static str]) -> Section {
    Section { title, items }
}

/// The menus, left to right.
///
/// Not one invented id: a test checks that all of them exist in the
/// catalogue and that none is declared `Planned`, because a menu that
/// offers something not yet built is worse than not having a menu.
pub const MENUS: &[Menu] = &[
    // Ten groups by what the reader WANTS TO DO, not by where the command
    // lives: reading a file, changing it, choosing what to act on, going
    // somewhere else, moving the panes, the tabs, searching, what is
    // shown, the tools and help. Everything built is in one of them;
    // nothing in two.
    Menu {
        title: "menu-file",
        sections: &[
            sec(
                None,
                &["pane.view", "pane.edit", "pane.edit-new", "pane.open"],
            ),
            // #139: properties belong to the FILE, so they go with what is
            // done to a file, not with what changes on the screen.
            sec(
                None,
                &["pane.properties", "pane.dir-size", "pane.copy-path"],
            ),
            sec(None, &["app.quit"]),
        ],
    },
    // What WRITES: apart from what only reads, because it is what goes
    // through the journal and what a reader wants to find together.
    // Deleting has its own section: it is the only thing here that does
    // not undo with a gesture.
    Menu {
        title: "menu-operate",
        sections: &[
            sec(
                None,
                &[
                    "pane.copy",
                    "pane.move",
                    "pane.rename",
                    "pane.rename-batch",
                    "pane.ai-rename",
                    "pane.organize",
                ],
            ),
            sec(None, &["pane.mkdir", "pane.chmod"]),
            sec(None, &["pane.delete", "pane.delete-permanent"]),
            sec(
                Some("menu-section-archives"),
                &["pane.pack", "pane.unpack", "pane.test-archive"],
            ),
            sec(
                Some("menu-section-pieces"),
                &["pane.split-file", "pane.combine-files"],
            ),
            sec(
                Some("menu-section-integrity"),
                &["pane.checksum", "pane.checksum-verify"],
            ),
        ],
    },
    Menu {
        title: "menu-mark",
        sections: &[
            sec(
                None,
                &[
                    "mark.toggle",
                    "mark.all",
                    "mark.invert",
                    "mark.clear",
                    "mark.restore",
                ],
            ),
            sec(
                Some("menu-section-by-pattern"),
                &[
                    "mark.pattern-add",
                    "mark.pattern-remove",
                    "mark.extension-add",
                    "mark.extension-remove",
                ],
            ),
            sec(Some("menu-section-by-kind"), &["mark.files", "mark.dirs"]),
        ],
    },
    // WHERE a pane looks: up, back, favorites, volumes, connect. #140 put
    // them in Panes for that same reason; with a navigation menu of its
    // own, this is where they are looked for.
    Menu {
        title: "menu-go",
        sections: &[
            // First in the menu because it is the one that helps when you
            // do not know which of the others you want, and because the
            // four imported presets do not bind it to any key: this is
            // where they find it.
            sec(None, &["app.goto"]),
            sec(
                None,
                &["nav.parent", "nav.back", "nav.forward", "pane.refresh"],
            ),
            sec(
                Some("menu-section-history"),
                &[
                    "nav.jump-back",
                    "nav.set-jump-point",
                    "pane.history",
                    "pane.history-left",
                    "pane.history-right",
                    "pane.popular",
                ],
            ),
            sec(
                Some("menu-section-places"),
                &[
                    "pane.hotlist",
                    "pane.select-drive",
                    "pane.connect",
                    "pane.disconnect",
                ],
            ),
            sec(
                Some("menu-section-shell"),
                &["pane.command-line", "app.terminal", "app.handoff"],
            ),
        ],
    },
    Menu {
        title: "menu-panels",
        sections: &[
            sec(
                None,
                &["pane.switch", "layout.focus-next", "layout.focus-prev"],
            ),
            sec(
                Some("menu-section-contents"),
                &[
                    "pane.mirror",
                    "pane.mirror-target",
                    "pane.pull",
                    "pane.swap",
                ],
            ),
            sec(
                Some("menu-section-split"),
                &[
                    "layout.split-h",
                    "layout.split-v",
                    "layout.close-slot",
                    "layout.grow",
                    "layout.shrink",
                    "layout.equalize",
                    "layout.flip",
                ],
            ),
            sec(None, &["layout.set-target", "app.toggle-panels"]),
        ],
    },
    Menu {
        title: "menu-tabs",
        sections: &[
            sec(None, &["pane.tab-new", "pane.tab-close"]),
            sec(None, &["pane.tab-next", "pane.tab-prev"]),
            sec(None, &["pane.tab-move-left", "pane.tab-move-right"]),
        ],
    },
    Menu {
        title: "menu-find",
        sections: &[
            sec(
                None,
                &["pane.quick-search", "pane.search", "pane.semantic-search"],
            ),
            sec(
                Some("menu-section-compare"),
                &["pane.compare-files", "pane.compare-dirs", "pane.sync-dirs"],
            ),
        ],
    },
    Menu {
        title: "menu-view",
        sections: &[
            sec(
                None,
                &[
                    "pane.toggle-hidden",
                    "pane.columns",
                    // #138: the sort belongs to the VIEW, and this is
                    // where what the view shows gets changed.
                    "pane.sort-menu",
                    "pane.names-encoding",
                ],
            ),
            // What opens BESIDE the listing. #136: the tree is another
            // navigation column, like the sidebar. #323: the log goes
            // next to processes — both answer "what is this doing?" — and
            // the disk map with them (phase 4). The timeline (phase 7)
            // has no shortcut in any preset: this is its only keyboard
            // path.
            sec(
                Some("menu-section-side-panels"),
                &[
                    "pane.tree",
                    "layout.places",
                    "layout.preview",
                    "layout.processes",
                    "layout.metadata",
                    "layout.log",
                    "layout.disk-map",
                    "layout.timeline",
                    // #362: the embedded terminal. It goes with the side
                    // panes and not with `app.terminal` in the commands
                    // menu, because what it opens is a PANE: what this menu
                    // manages is what shows beside the listing, and this is
                    // one more thing shown beside it.
                    "layout.terminal",
                ],
            ),
            sec(None, &["layout.pick", "app.theme"]),
        ],
    },
    // What is managed: extensions, agents, settings, profiles. The palette
    // goes here and not in Help, because it is used to DO things.
    Menu {
        title: "menu-tools",
        sections: &[
            sec(None, &["app.extensions", "app.agents", "app.settings"]),
            sec(
                Some("menu-section-profiles"),
                &["profile.pick", "profile.save-as"],
            ),
            sec(None, &["app.palette"]),
        ],
    },
    Menu {
        title: "menu-help",
        sections: &[sec(None, &["app.help"])],
    },
];

/// Which menu is open and where the cursor is within it.
///
/// Pure and render-free: the TUI paints it and the GUI will paint it
/// differently, but browsing a menu is not decided twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuState {
    menu: usize,
    item: usize,
}

impl Default for MenuState {
    fn default() -> Self {
        Self::new()
    }
}

impl MenuState {
    /// The first menu, first item.
    #[must_use]
    pub const fn new() -> Self {
        Self { menu: 0, item: 0 }
    }

    /// Reopens on whichever menu was open last time.
    ///
    /// A menu that always opens on the first one forces browsing the
    /// whole bar every time, and whoever uses two entries of the same menu
    /// pays for it on every gesture. An index that no longer exists — the
    /// bar changed between one opening and the next — falls back to the
    /// first instead of opening nothing.
    ///
    /// The CURSOR does go back to the start: inside a menu the list is
    /// short and read whole, and also remembering the row would make the
    /// same key run different things depending on what was last touched.
    ///
    /// Lives here because it is a presentation decision and both frontends
    /// have to make it the same way: a menu that remembers in the window
    /// and does not in the terminal are not one program.
    #[must_use]
    pub fn reopen_at(menu: usize) -> Self {
        let mut state = Self::new();
        state.open(menu);
        state
    }

    /// Which menu is open.
    #[must_use]
    pub const fn menu(&self) -> usize {
        self.menu
    }

    /// Which item is highlighted.
    #[must_use]
    pub const fn item(&self) -> usize {
        self.item
    }

    /// The id of the highlighted command.
    #[must_use]
    pub fn selected(&self) -> Option<&'static str> {
        MENUS.get(self.menu)?.item(self.item)
    }

    /// Switches menu, cycling. The cursor goes back to the first one:
    /// keeping it where it was would leave it on an item the new menu does
    /// not have.
    pub fn cycle_menu(&mut self, delta: isize) {
        let n = MENUS.len();
        if n == 0 {
            return;
        }
        let i = isize::try_from(self.menu).unwrap_or(0);
        self.menu =
            usize::try_from((i + delta).rem_euclid(isize::try_from(n).unwrap_or(1))).unwrap_or(0);
        self.item = 0;
    }

    /// Moves the cursor within the open menu, cycling.
    pub fn cycle_item(&mut self, delta: isize) {
        let Some(n) = MENUS.get(self.menu).map(Menu::len) else {
            return;
        };
        if n == 0 {
            return;
        }
        let i = isize::try_from(self.item).unwrap_or(0);
        self.item =
            usize::try_from((i + delta).rem_euclid(isize::try_from(n).unwrap_or(1))).unwrap_or(0);
    }

    /// Opens a menu by index and puts the cursor at the start.
    pub fn open(&mut self, menu: usize) {
        if menu < MENUS.len() {
            self.menu = menu;
            self.item = 0;
        }
    }

    /// Puts the cursor on an item of the open menu.
    pub fn point_at(&mut self, item: usize) {
        if MENUS.get(self.menu).is_some_and(|m| item < m.len()) {
            self.item = item;
        }
    }
}

#[cfg(test)]
mod tests {
    /// Every menu item has a LABEL in both languages.
    ///
    /// Without this, a new command comes out in the menu with its raw key
    /// — `menu-item-pane-properties` in the middle of the list — which is
    /// exactly what happened when adding #138's and #139's: the whole
    /// suite green and the screen showing the identifier. The frontend
    /// paints the menu, so the gate lives here. Same for section labels.
    #[test]
    fn every_menu_item_has_a_label_in_both_languages() {
        for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
            let _ = norte_i18n::force(lang);
            for menu in MENUS {
                let title = norte_i18n::t(menu.title);
                assert!(
                    !title.is_empty() && title != menu.title,
                    "{lang:?}: menu {} has no title",
                    menu.title
                );
                for id in menu.items() {
                    let key = format!("menu-item-{}", id.replace('.', "-"));
                    let label = norte_i18n::t(&key);
                    assert!(
                        !label.is_empty() && label != key,
                        "{lang:?}: {id} appears in the menu with no label ({key})"
                    );
                }
                for key in menu.sections.iter().filter_map(|s| s.title) {
                    let label = norte_i18n::t(key);
                    assert!(
                        !label.is_empty() && label != key,
                        "{lang:?}: section {key} has no label"
                    );
                }
            }
        }
    }

    use super::*;
    use crate::keymap::catalogue::{Status, lookup};

    /// Not one invented id, and none `Planned`: a menu that offers
    /// something not yet built is worse than not having a menu — the
    /// reader clicks it and nothing happens, with no explanation.
    #[test]
    fn everything_a_menu_offers_exists_and_is_built() {
        for m in MENUS {
            for id in m.items() {
                let def = lookup(id).unwrap_or_else(|| panic!("{id} is not in the catalogue"));
                assert_eq!(def.status, Status::Live, "{id} is declared Planned");
            }
        }
    }

    /// No command in two menus: two places for the same thing is a menu
    /// that does not show where things are.
    #[test]
    fn no_command_is_in_two_menus() {
        let mut seen = std::collections::BTreeSet::new();
        for m in MENUS {
            for id in m.items() {
                assert!(seen.insert(id), "{id} appears in two menus");
            }
        }
    }

    /// An empty section would paint a rule with nothing under it.
    #[test]
    fn no_section_is_empty() {
        for m in MENUS {
            for s in m.sections {
                assert!(!s.items.is_empty(), "{}: empty section", m.title);
            }
        }
    }

    /// Sections are announced where they start, the first one with no rule.
    #[test]
    fn section_at_marks_the_start_of_each_section() {
        let operate = MENUS
            .iter()
            .find(|m| m.title == "menu-operate")
            .expect("Operate");
        assert_eq!(operate.section_at(0), None, "the first one carries no rule");
        assert_eq!(operate.section_at(1), None, "Move stays in Copy's section");
        let delete = operate
            .items()
            .position(|id| id == "pane.delete")
            .expect("Delete");
        assert_eq!(operate.section_at(delete), Some(None), "rule with no label");
        let pack = operate
            .items()
            .position(|id| id == "pane.pack")
            .expect("Pack");
        assert_eq!(
            operate.section_at(pack),
            Some(Some("menu-section-archives"))
        );
        assert_eq!(operate.item(delete), Some("pane.delete"));
    }

    #[test]
    fn delete_is_destructive_and_ai_is_marked() {
        assert_eq!(role("pane.delete-permanent"), ItemRole::Destructive);
        assert_eq!(role("pane.ai-rename"), ItemRole::Ai);
        assert_eq!(role("pane.copy"), ItemRole::Normal);
    }

    #[test]
    fn switching_menu_returns_the_cursor_to_the_start() {
        let mut s = MenuState::new();
        s.cycle_item(2);
        assert_eq!(s.item(), 2);
        s.cycle_menu(1);
        assert_eq!(s.menu(), 1);
        assert_eq!(s.item(), 0, "item 2 might not exist here");
    }

    #[test]
    fn both_traversals_cycle() {
        let mut s = MenuState::new();
        s.cycle_menu(-1);
        assert_eq!(s.menu(), MENUS.len() - 1);
        s.cycle_item(-1);
        assert_eq!(s.item(), MENUS[MENUS.len() - 1].len() - 1);
    }

    /// Pointing out of range does NOT move the cursor: the emitter of
    /// indices is the mouse, and an impossible index is a bug of ours, not
    /// something that should leave the cursor on an item that does not
    /// exist.
    #[test]
    fn pointing_out_of_range_moves_nothing() {
        let mut s = MenuState::new();
        s.point_at(999);
        assert_eq!(s.item(), 0);
    }
}
