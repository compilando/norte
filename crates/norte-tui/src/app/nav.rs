//! The navigation popup as seen from `App`: history, hotlist and volumes,
//! its key input, and saving and deleting a hotlist entry.

use super::nav_popup::{NavItem, NavPopup, NavPopupKind, nav_item_display};
use super::{App, PickerAction, display_name};
use norte_i18n::t;
use norte_proto::VPath;

impl App {
    /// Opens the navigation popup (spec 2026-07-18): the focused pane's
    /// history (most recent first) or the hotlist copy. Items are built
    /// ALREADY sanitized here (`nav_item_display`); an invalid hotlist entry
    /// shows with its warning and a `None` destination.
    ///
    /// # Panics
    /// With `NavPopupKind::Volumes`: those items need an ASYNC fetch against
    /// `Backend::volumes` that this method (synchronous, no `Backend`)
    /// can't do — `main.rs` opens that kind via
    /// [`Self::open_volumes_popup`], never here.
    pub fn open_nav_popup(&mut self, kind: NavPopupKind) {
        let pane = self.focus;
        self.open_nav_popup_for(kind, pane, None, None);
    }

    /// A SIDE of the screen's history (`pane.history-left/-right`, spec
    /// 2026-09-15 D7): what's chosen navigates THAT panel even if focus is
    /// on the other one, and the side is frozen on open, like volumes.
    pub fn open_side_history(&mut self, side: usize) {
        self.open_nav_popup_for(NavPopupKind::History, side, Some(side), None);
    }

    /// Opens the `kind` popup over pane `pane`. `side` is only carried by a
    /// history opened per side, for the title.
    fn open_nav_popup_for(
        &mut self,
        kind: NavPopupKind,
        pane: usize,
        side: Option<usize>,
        filter: Option<String>,
    ) {
        let enc = self.panes[pane].name_encoding();
        let mut cursor = 0;
        let items: Vec<NavItem> = match kind {
            NavPopupKind::Volumes => unreachable!(
                "Volumes opens via `open_volumes_popup` (design §D), never `open_nav_popup`"
            ),
            NavPopupKind::History | NavPopupKind::Popular => {
                // The rows are the SHARED ones: what comes out, in what
                // order and with what mark is decided by
                // `norte_frontend::history`, same as in the window
                // (ADR 0077).
                let current = self.panes[pane].dir().clone();
                let filter_str = filter.as_deref().unwrap_or("");
                let rows = if kind == NavPopupKind::History {
                    norte_frontend::history::history_rows(
                        &self.history[pane],
                        &current,
                        filter_str,
                        enc,
                    )
                } else {
                    norte_frontend::history::popular_rows(&self.popular, &current, filter_str)
                };
                cursor = norte_frontend::history::start_cursor(&rows);
                // Popular entries span the whole session: ONE panel's
                // reinterpretation applied to another's paths would invent
                // mojibake (encoding-auditor, phase 1).
                let enc = if kind == NavPopupKind::History {
                    enc
                } else {
                    None
                };
                super::nav_popup::history_items(rows, enc)
            }
            NavPopupKind::Hotlist => self
                .hotlist
                .iter()
                .map(|h| {
                    let (display, target) = if let Ok(p) = &h.target {
                        (nav_item_display(Some(&h.name), p, enc), Some(p.clone()))
                    } else {
                        // review MINOR T5: the name's hostile flag is NOT
                        // discarded — an invalid one with a bidi name also
                        // carries the badge (same criterion as everything
                        // else).
                        let (name, hostile) = display_name(h.name.as_bytes());
                        let notice = t("hotlist-invalid");
                        let display = if hostile {
                            format!("{} {name} {notice}", crate::ui::HOSTILE_BADGE)
                        } else {
                            format!("{name} {notice}")
                        };
                        (display, None)
                    };
                    NavItem {
                        display,
                        target,
                        hotlist_name: Some(h.name.clone()),
                        mark: None,
                    }
                })
                .collect(),
        };
        self.nav_popup = Some(NavPopup {
            kind,
            items,
            cursor,
            name_input: None,
            target_pane: pane,
            include_pseudo: false,
            side,
            filter,
        });
    }

    /// The OTHER panel relative to the one the open popup navigates: the
    /// focus's destination if the popup is the focus's, and the focus if
    /// the popup is a side's that doesn't have it (`dialog.confirm-other`,
    /// spec 2026-09-15 D2).
    #[must_use]
    pub fn nav_popup_other_pane(&self) -> Option<usize> {
        let pane = self.nav_popup.as_ref()?.target_pane;
        if pane == self.focus {
            self.target_index()
        } else {
            Some(self.focus)
        }
    }

    /// Removes the cursor's row from history or from popular entries
    /// (`dialog.remove`) and rebuilds the list keeping the cursor.
    pub fn nav_popup_remove_selected(&mut self) {
        let Some(p) = &self.nav_popup else {
            return;
        };
        let (kind, pane) = (p.kind, p.target_pane);
        let Some(path) = p.selected().and_then(|it| it.target.clone()) else {
            return;
        };
        match kind {
            // The "here" row doesn't get removed: the list always puts it
            // there, and `History::remove` would prune the current
            // directory and its jump point from the trail without it
            // showing (rust-reviewer, phase 1).
            NavPopupKind::History if path == *self.panes[pane].dir() => return,
            NavPopupKind::History => self.history[pane].remove(&path),
            NavPopupKind::Popular => self.popular.remove(&path),
            NavPopupKind::Hotlist | NavPopupKind::Volumes => return,
        }
        self.reopen_history_popup();
    }

    /// Clears the panel's history or the popular entries (`dialog.clear`).
    /// No confirmation: it's navigation memory, not files, and the notice
    /// says so.
    pub fn nav_popup_clear(&mut self) {
        let Some(p) = &self.nav_popup else {
            return;
        };
        let (kind, pane) = (p.kind, p.target_pane);
        let key = match kind {
            NavPopupKind::History => {
                self.history[pane].clear();
                "msg-history-cleared"
            }
            NavPopupKind::Popular => {
                self.popular.clear();
                "msg-popular-cleared"
            }
            NavPopupKind::Hotlist | NavPopupKind::Volumes => return,
        };
        self.message = Some(t(key));
        self.reopen_history_popup();
    }

    /// Rebuilds a history or popular list after changing it, with the
    /// cursor where it was (clamped).
    fn reopen_history_popup(&mut self) {
        let Some(p) = &self.nav_popup else {
            return;
        };
        let (kind, pane, side, cursor) = (p.kind, p.target_pane, p.side, p.cursor);
        let filter = p.filter.clone();
        self.open_nav_popup_for(kind, pane, side, filter);
        if let Some(p) = &mut self.nav_popup {
            p.cursor = cursor.min(p.items.len().saturating_sub(1));
        }
    }

    /// Opens the volumes popup (`pane.select-drive`/`-left`/`-right`, design
    /// §D) with `items` ALREADY built by
    /// [`super::nav_popup::volume_items`] — `main.rs` does the async fetch
    /// against `Backend::volumes` and calls here, the same split as the
    /// rest of this popup: main.rs is I/O, app.rs is state and
    /// presentation.
    ///
    /// `pane` is the SIDE `Confirm` is going to navigate: the focus for
    /// `pane.select-drive`, a fixed side for `-left`/`-right` regardless of
    /// current focus. `include_pseudo` is the mode THIS list was requested
    /// with — the toggle inside the popup calls back here with the value
    /// flipped, so this is literally a re-open, not a special case.
    pub fn open_volumes_popup(&mut self, pane: usize, include_pseudo: bool, items: Vec<NavItem>) {
        self.nav_popup = Some(NavPopup {
            kind: NavPopupKind::Volumes,
            items,
            cursor: 0,
            name_input: None,
            target_pane: pane,
            include_pseudo,
            side: None,
            filter: None,
        });
    }

    /// Processes an action on the navigation popup. `Confirm` with a VALID
    /// item closes the popup and returns its destination (the caller
    /// navigates through the normal cd flow); over an invalid item (or no
    /// items) it's a no-op — the popup stays open. `Cancel` closes the
    /// `name_input` if active, and if not, the popup. The caller must not
    /// call `Confirm` with `name_input` active (Enter there confirms the
    /// ADD, main.rs).
    pub fn nav_popup_input(&mut self, action: PickerAction) -> Option<VPath> {
        match action {
            PickerAction::Up => {
                if let Some(p) = &mut self.nav_popup {
                    p.up();
                }
                None
            }
            PickerAction::Down => {
                if let Some(p) = &mut self.nav_popup {
                    p.down();
                }
                None
            }
            PickerAction::Confirm => {
                let target = self
                    .nav_popup
                    .as_ref()
                    .and_then(NavPopup::selected)
                    .and_then(|it| it.target.clone());
                if target.is_some() {
                    self.nav_popup = None;
                }
                target
            }
            PickerAction::Cancel => {
                if let Some(p) = &mut self.nav_popup {
                    if p.name_input.is_some() {
                        p.name_input = None;
                    } else {
                        self.nav_popup = None;
                    }
                }
                None
            }
        }
    }

    /// Opens the hotlist popup's name input (`a`), pre-filled with what
    /// [`norte_frontend::places::suggested_hotlist_name`] proposes for the
    /// focused pane's dir — the same one that's about to be saved — already
    /// clear of the names the hotlist already has taken. In the history
    /// popup it's a no-op (nothing to name).
    ///
    /// Pre-filled and EDITABLE, the same mold as a copy's destination name:
    /// a blank field asked for typing by hand what the path already
    /// implied. Empty still means cancel (`main.rs`), so deleting it whole
    /// is still the way out.
    pub fn nav_popup_open_name_input(&mut self) {
        let Some(target) = self.nav_popup_add_target() else {
            return;
        };
        let taken: Vec<&str> = self.hotlist.iter().map(|h| h.name.as_str()).collect();
        let suggested = norte_frontend::places::suggested_hotlist_name(&target, &taken);
        if let Some(p) = &mut self.nav_popup {
            p.name_input = Some(suggested);
        }
    }

    /// What the favorite `dialog.add` creates from the popup points at: the
    /// focused pane's directory in the favorites list, and the cursor's ROW
    /// in a history or popular list (spec 2026-09-15 D2). `None` in volumes
    /// or with no row.
    #[must_use]
    pub fn nav_popup_add_target(&self) -> Option<VPath> {
        let p = self.nav_popup.as_ref()?;
        match p.kind {
            NavPopupKind::Hotlist => Some(self.focused().dir().clone()),
            NavPopupKind::History | NavPopupKind::Popular => p.selected()?.target.clone(),
            NavPopupKind::Volumes => None,
        }
    }

    /// Changes a history or popular list's filter and rebuilds it (spec
    /// 2026-09-15 D2). The cursor goes back to the start: it's a different
    /// list. `None` removes the filter.
    pub fn nav_popup_set_filter(&mut self, filter: Option<String>) {
        let Some(p) = &self.nav_popup else {
            return;
        };
        if !matches!(p.kind, NavPopupKind::History | NavPopupKind::Popular) {
            return;
        }
        let (kind, pane, side) = (p.kind, p.target_pane, p.side);
        self.open_nav_popup_for(kind, pane, side, filter);
    }

    /// The selected favorite's RAW `name` (the key `persist_hotlist_remove`
    /// needs — the item's display is sanitized and does NOT serve as a
    /// key). Comes from the key FROZEN in the item itself
    /// ([`NavItem::hotlist_name`]): `App::hotlist` is never indexed, which a
    /// hot-reload could have mutated under the popup (review MAJOR T5 — it
    /// would delete another favorite). `None` in history or with no items.
    #[must_use]
    pub fn nav_popup_selected_hotlist_name(&self) -> Option<String> {
        self.nav_popup.as_ref()?.selected()?.hotlist_name.clone()
    }

    /// Replaces the current favorites list and carries it to the two
    /// surfaces that show it.
    ///
    /// Used by startup, `norte.toml`'s hot-reload and switching profiles.
    /// Before, each one wrote `App::hotlist` bare, and the sidebar was left
    /// with the old list with nothing ever touching it again.
    ///
    /// An OPEN popup does NOT get rebuilt, and that's the opposite of what
    /// adding and removing do: its items are a snapshot frozen on opening
    /// (see [`NavPopup`]) because `dialog.remove` deletes by the row's name,
    /// and a list moving under the cursor because of a file edited
    /// elsewhere would delete something else.
    pub fn set_hotlist(&mut self, items: Vec<crate::config::HotlistItem>) {
        self.hotlist = items;
        self.sync_places_favorites();
    }

    /// Reflects in the local copy a favorite ALREADY persisted successfully
    /// (replaces by `name` keeping position, or appends at the end — the
    /// SAME semantics as `config::persist_hotlist_add`/`load`) and refreshes
    /// the two surfaces that show it: the open popup and the sidebar.
    pub fn hotlist_apply_saved(&mut self, name: &str, target: VPath) {
        if let Some(item) = self.hotlist.iter_mut().find(|h| h.name == name) {
            item.target = Ok(target);
        } else {
            self.hotlist.push(crate::config::HotlistItem {
                name: name.to_owned(),
                target: Ok(target),
            });
        }
        self.sync_places_favorites();
        self.rebuild_hotlist_popup();
    }

    /// Reflects in the local copy a favorite ALREADY deleted from disk and
    /// refreshes the two surfaces that showed it.
    pub fn hotlist_apply_removed(&mut self, name: &str) {
        self.hotlist.retain(|h| h.name != name);
        self.sync_places_favorites();
        self.rebuild_hotlist_popup();
    }

    /// Rebuilds the hotlist popup's items after an add/remove, keeping the
    /// cursor (clamped): the painted list never drifts out of sync with the
    /// copy in `App` (the 1:1 index invariant).
    fn rebuild_hotlist_popup(&mut self) {
        if let Some(p) = &self.nav_popup
            && p.kind == NavPopupKind::Hotlist
        {
            let cursor = p.cursor;
            self.open_nav_popup(NavPopupKind::Hotlist);
            if let Some(p) = &mut self.nav_popup {
                p.cursor = cursor.min(p.items.len().saturating_sub(1));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::nav_popup::volume_items;
    use crate::app::testutil::*;

    /// History popup (spec 2026-07-18): navigation with `PickerAction`,
    /// Confirm returns the destination and closes, Cancel closes.
    #[test]
    fn nav_popup_history_navigates_confirms_and_cancels() {
        let mut app = app_two_panes();
        app.history[0].push(vp("mem:///one"));
        app.history[0].push(vp("mem:///two"));
        app.open_nav_popup(NavPopupKind::History);
        assert_eq!(
            app.nav_popup.as_ref().unwrap().items().len(),
            3,
            "the current directory goes first (spec 2026-09-15 D3)"
        );
        assert_eq!(
            app.nav_popup.as_ref().unwrap().selected().unwrap().target,
            Some(vp("mem:///two")),
            "most recent first"
        );
        assert_eq!(app.nav_popup_input(PickerAction::Down), None);
        assert_eq!(
            app.nav_popup_input(PickerAction::Confirm),
            Some(vp("mem:///one")),
            "Confirm returns the highlighted item's destination"
        );
        assert!(app.nav_popup.is_none(), "Confirm closes the popup");

        app.open_nav_popup(NavPopupKind::History);
        assert_eq!(app.nav_popup_input(PickerAction::Cancel), None);
        assert!(app.nav_popup.is_none(), "Cancel closes the popup");
    }

    /// An INVALID favorite (a path that doesn't parse) shows with its
    /// warning and a `None` destination: Confirm over it is a no-op (the
    /// popup stays open).
    #[test]
    fn nav_popup_hotlist_invalid_item_does_not_confirm() {
        let _ = norte_i18n::force(norte_i18n::Lang::Es);
        let mut app = app_two_panes();
        app.hotlist = vec![crate::config::HotlistItem {
            name: "broken".into(),
            target: Err("err-invalid-path".into()),
        }];
        app.open_nav_popup(NavPopupKind::Hotlist);
        let item = app.nav_popup.as_ref().unwrap().selected().unwrap().clone();
        assert!(item.target.is_none(), "invalid doesn't navigate");
        assert!(
            item.display.contains(&norte_i18n::t("hotlist-invalid")),
            "the invalid warning gets painted: {}",
            item.display
        );
        assert_eq!(app.nav_popup_input(PickerAction::Confirm), None);
        assert!(app.nav_popup.is_some(), "the popup does NOT close");
    }

    /// `a` opens the name input ONLY in hotlist; Cancel with the input
    /// active closes the input (not the popup). `d`: the selected RAW name
    /// serves as key and the local delete refreshes the items.
    #[test]
    fn nav_popup_hotlist_input_and_deletion() {
        let mut app = app_two_panes();
        app.hotlist = vec![
            crate::config::HotlistItem {
                name: "one".into(),
                target: Ok(vp("mem:///one")),
            },
            crate::config::HotlistItem {
                name: "two".into(),
                target: Ok(vp("mem:///two")),
            },
        ];
        app.open_nav_popup(NavPopupKind::Hotlist);
        app.nav_popup_open_name_input();
        assert_eq!(
            app.nav_popup.as_ref().unwrap().name_input.as_deref(),
            Some("/"),
            "`a` opens the input pre-filled with the suggested name"
        );
        app.nav_popup_input(PickerAction::Cancel);
        let p = app.nav_popup.as_ref().unwrap();
        assert!(p.name_input.is_none(), "Cancel closes the input");
        assert!(app.nav_popup.is_some(), "...not the popup");

        assert_eq!(
            app.nav_popup_selected_hotlist_name().as_deref(),
            Some("one"),
            "the selected one's RAW name (the persist's key)"
        );
        app.hotlist_apply_removed("one");
        assert_eq!(app.hotlist.len(), 1);
        let p = app.nav_popup.as_ref().unwrap();
        assert_eq!(p.items().len(), 1, "the popup refreshes after deleting");
        assert_eq!(p.selected().unwrap().target, Some(vp("mem:///two")));
    }

    /// review MAJOR T5: a hot-reload with the popup open mutates
    /// `App.hotlist` while the user sees the OLD snapshot (items frozen on
    /// purpose) — `d` must delete what's SHOWN (the key frozen in the
    /// item), never whatever now occupies that index in the new list (it
    /// would delete ANOTHER favorite: config loss).
    #[test]
    fn d_with_desynced_popup_clears_the_shown_one() {
        let mut app = app_two_panes();
        app.hotlist = vec![
            crate::config::HotlistItem {
                name: "one".into(),
                target: Ok(vp("mem:///one")),
            },
            crate::config::HotlistItem {
                name: "two".into(),
                target: Ok(vp("mem:///two")),
            },
        ];
        app.open_nav_popup(NavPopupKind::Hotlist);
        // Cursor on 0: the user SEES "one". Simulates the hot-reload that
        // removed "one" from the config (the copy in App changes, the
        // popup doesn't).
        app.hotlist.remove(0);
        assert_eq!(
            app.nav_popup_selected_hotlist_name().as_deref(),
            Some("one"),
            "the key is the popup's FROZEN one, not App.hotlist[cursor]"
        );
    }

    /// `a` doesn't open a blank field: the path already implies it from the
    /// panel, so does the name. And the suggestion dodges the names the
    /// hotlist already has taken — `persist_hotlist_add` REPLACES by name,
    /// and accepting without checking would overwrite a favorite pointing
    /// somewhere else.
    #[test]
    fn the_name_input_prefills_with_the_panels_dir() {
        let mut app = crate::app::testutil::app_en("mem:///home/o/norte/src", "mem:///other");
        app.open_nav_popup(NavPopupKind::Hotlist);
        app.nav_popup_open_name_input();
        assert_eq!(
            app.nav_popup.as_ref().unwrap().name_input.as_deref(),
            Some("src")
        );

        app.nav_popup_input(PickerAction::Cancel);
        app.hotlist = vec![crate::config::HotlistItem {
            name: "src".into(),
            target: Ok(vp("mem:///other/src")),
        }];
        app.nav_popup_open_name_input();
        assert_eq!(
            app.nav_popup.as_ref().unwrap().name_input.as_deref(),
            Some("norte/src"),
            "taken: it qualifies with the parent instead of overwriting"
        );
    }

    /// Spec 2026-09-15 D3: the first row is where the panel is, marked, and
    /// the cursor starts on the next one; removing and clearing rebuild the
    /// list.
    #[test]
    fn history_marks_the_current_one_and_remove_or_clear_redo_it() {
        let _ = norte_i18n::force(norte_i18n::Lang::Es);
        let mut app = app_two_panes();
        let here = app.panes[0].dir().clone();
        app.history[0].push(vp("mem:///one"));
        app.history[0].push(vp("mem:///two"));
        app.open_nav_popup(NavPopupKind::History);
        let p = app.nav_popup.as_ref().unwrap();
        assert_eq!(p.items()[0].target, Some(here));
        assert_eq!(
            p.items()[0].mark.as_deref(),
            Some(norte_i18n::t("history-mark-current").as_str()),
            "the mark goes APART from the path: glued to the text it \
             mimicked a directory named `x · here`"
        );
        assert!(
            !p.items()[0]
                .display
                .contains(&norte_i18n::t("history-mark-current"))
        );
        // Removing the "here" row does nothing: neither the list nor the
        // trail change.
        app.nav_popup.as_mut().unwrap().cursor = 0;
        app.nav_popup_remove_selected();
        assert_eq!(app.nav_popup.as_ref().unwrap().items().len(), 3);
        app.nav_popup.as_mut().unwrap().cursor = 1;
        let p = app.nav_popup.as_ref().unwrap();
        assert_eq!(p.selected().unwrap().target, Some(vp("mem:///two")));

        app.nav_popup_remove_selected();
        assert!(!app.history[0].entries().contains(&vp("mem:///two")));
        let p = app.nav_popup.as_ref().unwrap();
        assert_eq!(p.items().len(), 2);
        assert_eq!(p.selected().unwrap().target, Some(vp("mem:///one")));

        app.nav_popup_clear();
        assert!(app.history[0].entries().is_empty());
        assert_eq!(
            app.nav_popup.as_ref().unwrap().items().len(),
            1,
            "only the current one"
        );
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-history-cleared").as_str())
        );
    }

    /// D7: a SIDE's history navigates that side even if focus is on the
    /// other one, and opening on the other panel points at the focus.
    #[test]
    fn one_sides_history_freezes_the_side() {
        let mut app = app_two_panes();
        app.history[1].push(vp("mem:///right"));
        app.open_side_history(1);
        let p = app.nav_popup.as_ref().unwrap();
        assert_eq!((p.target_pane(), p.side), (1, Some(1)));
        assert_eq!(p.selected().unwrap().target, Some(vp("mem:///right")));
        assert_eq!(app.nav_popup_other_pane(), Some(app.focus()));
    }

    /// D6: popular entries are listed by visits.
    #[test]
    fn popular_ones_are_ranked_by_visits() {
        let mut app = app_two_panes();
        app.popular.visit(&vp("mem:///little"));
        app.popular.visit(&vp("mem:///lots"));
        app.popular.visit(&vp("mem:///lots"));
        app.open_nav_popup(NavPopupKind::Popular);
        let p = app.nav_popup.as_ref().unwrap();
        assert_eq!(p.items()[0].target, Some(vp("mem:///lots")));
        assert_eq!(p.items()[1].target, Some(vp("mem:///little")));
    }

    /// In the HISTORY popup, `a` opens the favorite name for the cursor's
    /// ROW (spec 2026-09-15 D2) — it used to do nothing — and there's no
    /// hotlist name to delete.
    #[test]
    fn nav_popup_history_adds_the_row_as_a_favorite() {
        let mut app = app_two_panes();
        app.history[0].push(vp("mem:///one"));
        app.open_nav_popup(NavPopupKind::History);
        assert_eq!(app.nav_popup_add_target(), Some(vp("mem:///one")));
        app.nav_popup_open_name_input();
        assert_eq!(
            app.nav_popup.as_ref().unwrap().name_input.as_deref(),
            Some("one")
        );
        assert_eq!(app.nav_popup_selected_hotlist_name(), None);
    }

    /// D2: the filter rebuilds the list, the cursor goes back to the start
    /// and removing it returns it whole.
    #[test]
    fn the_history_filter_rebuilds_the_list() {
        let mut app = app_two_panes();
        app.history[0].push(vp("mem:///photos/2024"));
        app.history[0].push(vp("mem:///invoices"));
        app.history[0].push(vp("mem:///music"));
        app.open_nav_popup(NavPopupKind::History);
        let whole = app.nav_popup.as_ref().unwrap().items().len();
        app.nav_popup_set_filter(Some("inv".to_owned()));
        let p = app.nav_popup.as_ref().unwrap();
        assert_eq!(p.filter.as_deref(), Some("inv"));
        assert_eq!(p.items().len(), 1);
        assert_eq!(p.selected().unwrap().target, Some(vp("mem:///invoices")));
        app.nav_popup_set_filter(None);
        assert_eq!(app.nav_popup.as_ref().unwrap().items().len(), whole);
    }

    /// `hotlist_apply_saved` replaces by name keeping position or appends at
    /// the end (same semantics as persist/load) and refreshes the popup.
    #[test]
    fn hotlist_apply_saved_replaces_or_adds() {
        let mut app = app_two_panes();
        app.hotlist = vec![crate::config::HotlistItem {
            name: "one".into(),
            target: Ok(vp("mem:///old")),
        }];
        app.open_nav_popup(NavPopupKind::Hotlist);
        app.hotlist_apply_saved("one", vp("mem:///new"));
        assert_eq!(app.hotlist.len(), 1, "replaces, doesn't duplicate");
        assert_eq!(app.hotlist[0].target.as_ref().unwrap(), &vp("mem:///new"));
        app.hotlist_apply_saved("two", vp("mem:///two"));
        assert_eq!(app.hotlist.len(), 2, "a new name gets appended at the end");
        assert_eq!(
            app.nav_popup.as_ref().unwrap().items().len(),
            2,
            "the open popup reflects the add"
        );
    }

    /// A HOSTILE path in the history comes out masked and with the badge as
    /// a prefix — never raw bidi/controls in the popup (spec §6).
    #[test]
    fn nav_popup_sanitizes_hostile_paths() {
        let mut app = app_two_panes();
        app.history[0].push(vp("mem:///evil%E2%80%AEdir"));
        app.open_nav_popup(NavPopupKind::History);
        let display = app
            .nav_popup
            .as_ref()
            .unwrap()
            .selected()
            .unwrap()
            .display
            .clone();
        assert!(!display.contains('\u{202E}'), "no raw bidi: {display:?}");
        assert!(display.starts_with('!'), "badge prefix: {display}");
    }

    /// encoding-auditor MAJOR: `fs_type` looked like a closed, ASCII-only
    /// vocabulary (`ext4`, `nfs4`…) but a FUSE mount's `fuse.<subtype>` is
    /// the `-o subtype=` value an UNPRIVILEGED user picks (`sshfs`, `rclone
    /// mount`…) — exactly as untrusted as a filename. An earlier draft of
    /// `volume_item_display` spliced it in with `{}` and skipped
    /// `display_name` entirely, so a hostile `fs_type` reached the row raw.
    #[test]
    fn volume_row_sanitizes_hostile_fs_type() {
        let vol = norte_proto::methods::Volume {
            mount: vp("mem:///media/usb"),
            label: None,
            fs_type: "fuse.evil\u{202E}type".to_owned(),
            kind: norte_proto::methods::VolumeKind::Fixed,
            total_bytes: None,
            free_bytes: None,
            read_only: false,
        };
        let items = volume_items(std::slice::from_ref(&vol), None);
        let display = items[0].display.clone();
        assert!(!display.contains('\u{202E}'), "no raw bidi: {display:?}");
        assert!(display.starts_with('!'), "badge prefix: {display}");
    }

    /// V3.5 (encoding-auditor MAJOR deferred from V3): `label` is
    /// `Option<Vec<u8>>` end to end now, so a non-UTF-8 label reaches this
    /// row as the ORIGINAL bytes — not a lossy `String` some earlier layer
    /// already mangled — and goes through the exact same masking `fs_type`
    /// gets above. Bytes `\xFF\xFE` are not valid UTF-8 in any position, so
    /// `display_name` must fall back to lossy rendering AND mark it hostile.
    #[test]
    fn volume_row_sanitizes_non_utf8_label() {
        let vol = norte_proto::methods::Volume {
            mount: vp("mem:///media/usb"),
            label: Some(vec![0xFF, 0xFE, b'X']),
            fs_type: "vfat".to_owned(),
            kind: norte_proto::methods::VolumeKind::Removable,
            total_bytes: None,
            free_bytes: None,
            read_only: false,
        };
        let items = volume_items(std::slice::from_ref(&vol), None);
        let display = items[0].display.clone();
        assert!(display.starts_with('!'), "badge prefix: {display}");
        assert!(
            display.contains('\u{FFFD}'),
            "the non-UTF8 label paints lossy: {display}"
        );
        assert_eq!(
            items[0].target,
            Some(vp("mem:///media/usb")),
            "the target is still the real mount, unrelated to the label"
        );
    }

    /// V3.5 (encoding-auditor MINOR: the hand-picked byte string above is
    /// not the canonical corpus): every hostile name in
    /// `norte_testkit::corpus::hostile_names()`, used as a LABEL, must reach
    /// the row without panicking, badged EXACTLY when `display_name` alone
    /// says that name comes out altered — the same function
    /// `volume_item_display` calls, so this pins agreement rather than
    /// reimplementing the masking rule a second time. `target` stays the
    /// clean mount throughout: a hostile label must never leak into
    /// Enter-to-navigate.
    ///
    /// #169's `archive_marker_literal` (a label whose own CLEAN text is
    /// `"!"`, the same glyph as [`crate::ui::HOSTILE_BADGE`]) caught this
    /// assertion checking `display.starts_with('!')` — true for that
    /// fixture even with `label_hostil == false`, because the UN-badged
    /// label prefix (`"{label} — "`) itself starts with `!`. A leading `!`
    /// is not proof of a badge, and even `"! "` is not enough: that fixture's
    /// clean prefix is `"! — "`, which also starts with `"! "`. Nothing
    /// short of the FULL string settles it, so the expected display is
    /// rebuilt here from the same primitives `volume_item_display` calls
    /// (`display_name`, `path_display_with`, `t`) — not the masking rule
    /// itself, only the template it is spliced into — and compared for
    /// EXACT equality.
    #[test]
    fn volume_label_hostile_corpus_sweep() {
        let mount = vp("mem:///media/usb");
        let (path_text, path_hostile) = norte_frontend::path_display_with(&mount, None);
        assert!(
            !path_hostile,
            "control: the test's fixed mount isn't hostile"
        );
        let (fs_text, fs_hostile) = display_name(b"vfat");
        assert!(!fs_hostile, "control: \"vfat\" isn't hostile");
        let sizes = format!("{u} / {u}", u = t("volumes-size-unknown"));
        for fixture in norte_testkit::corpus::hostile_names() {
            let vol = norte_proto::methods::Volume {
                mount: mount.clone(),
                label: Some(fixture.bytes.clone()),
                fs_type: "vfat".to_owned(),
                kind: norte_proto::methods::VolumeKind::Removable,
                total_bytes: None,
                free_bytes: None,
                read_only: false,
            };
            let items = volume_items(std::slice::from_ref(&vol), None);
            let display = &items[0].display;
            let (label_text, label_hostile) = display_name(&fixture.bytes);
            let body = format!("{label_text} — {path_text}  {fs_text}  {sizes}");
            let expected = if label_hostile {
                format!("{} {body}", crate::ui::HOSTILE_BADGE)
            } else {
                body
            };
            assert_eq!(
                display, &expected,
                "{}: badge must match display_name({:?})",
                fixture.id, fixture.bytes
            );
            assert_eq!(
                items[0].target,
                Some(mount.clone()),
                "{}: the target is still the mount, unrelated to the label",
                fixture.id
            );
        }
    }

    /// The everyday case: an ordinary `fs_type` and absent sizes (the
    /// filesystem never answered `statvfs` in time, design §A) render with
    /// NO badge and say `volumes-size-unknown` rather than a bare zero — a
    /// zero here would read as "full", the opposite of "unknown".
    #[test]
    fn volume_row_missing_size_is_not_zero() {
        let vol = norte_proto::methods::Volume {
            mount: vp("mem:///media/usb"),
            label: Some(b"USB".to_vec()),
            fs_type: "vfat".to_owned(),
            kind: norte_proto::methods::VolumeKind::Removable,
            total_bytes: None,
            free_bytes: None,
            read_only: false,
        };
        let items = volume_items(std::slice::from_ref(&vol), None);
        let display = items[0].display.clone();
        assert!(!display.starts_with('!'), "nothing hostile here: {display}");
        assert!(!display.contains('0'), "absent isn't zero: {display}");
        assert_eq!(items[0].target, Some(vp("mem:///media/usb")));
    }

    /// review MINOR T5: an INVALID entry with a hostile name also carries
    /// the badge (before, `display_name`'s flag was discarded in that arm).
    #[test]
    fn invalid_hotlist_with_hostile_name_carries_a_badge() {
        let mut app = app_two_panes();
        app.hotlist = vec![crate::config::HotlistItem {
            name: "evil\u{202E}name".into(),
            target: Err("err-invalid-path".into()),
        }];
        app.open_nav_popup(NavPopupKind::Hotlist);
        let display = app
            .nav_popup
            .as_ref()
            .unwrap()
            .selected()
            .unwrap()
            .display
            .clone();
        assert!(!display.contains('\u{202E}'), "no raw bidi: {display:?}");
        assert!(display.starts_with('!'), "badge prefix: {display}");
    }
}
