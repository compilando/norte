//! The navigation popup: its items, how they're shown and where the volumes
//! come from.

use super::display_name;
use norte_i18n::t;
use norte_proto::VPath;

/// Which navigation popup is open (spec 2026-07-18).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavPopupKind {
    /// A pane's directory history (the focused one, or one SIDE).
    History,
    /// The session's popular directories (spec 2026-09-15 D6).
    Popular,
    /// Favorites persisted in the USER's `norte.toml`.
    Hotlist,
    /// Host volumes (`pane.select-drive`/`-left`/`-right`, design
    /// 2026-08-10-volumes-design.md §D): a snapshot frozen on open via
    /// `Backend::volumes` — `main.rs` does the async fetch (app.rs doesn't
    /// know about `Backend`) and hands over the already-built items to
    /// [`crate::app::App::open_volumes_popup`].
    Volumes,
}

/// A navigation popup item, FROZEN when built in
/// [`crate::app::App::open_nav_popup`]: display already sanitized,
/// destination already parsed and (for hotlist) the favorite's raw key. The
/// popup is a snapshot on purpose — everything a key needs travels inside
/// the item, nothing gets re-resolved against a state that may have changed
/// underneath.
#[derive(Debug, Clone)]
pub struct NavItem {
    /// Display ALREADY sanitized, ready to paint.
    pub display: String,
    /// Parsed destination; `None` = invalid hotlist entry (shown with its
    /// warning, doesn't navigate).
    pub target: Option<VPath>,
    /// The favorite's RAW `name` — the key `d` deletes by
    /// ([`crate::app::App::nav_popup_selected_hotlist_name`]), frozen on
    /// open: a hot-reload can mutate `App::hotlist` under the popup and the
    /// delete has to land on what was SHOWN, never on whatever now occupies
    /// that index in the new list (review MAJOR T5). `None` in history.
    pub hotlist_name: Option<String>,
    /// A history row's mark (`here`, `forward`), already translated.
    ///
    /// SEPARATE from the display and painted in a different style: glued to
    /// the path text, a directory literally named `x · here` was
    /// indistinguishable from `x` marked as the current one
    /// (encoding-auditor, phase 1). `None` everywhere else.
    pub mark: Option<String>,
}

/// Navigation popup (`Alt+↓` history / `Ctrl+D` hotlist). The `items` are
/// built ALREADY sanitized in [`crate::app::App::open_nav_popup`] (see
/// [`NavItem`]): rendering doesn't re-decide anything and Enter doesn't
/// re-parse anything.
#[derive(Debug, Clone)]
pub struct NavPopup {
    /// History, hotlist or volumes (decides the title, footer and which
    /// extra keys it accepts).
    pub kind: NavPopupKind,
    /// Items frozen on open.
    pub(crate) items: Vec<NavItem>,
    /// Highlighted index.
    pub(crate) cursor: usize,
    /// Open name input (`a` in hotlist): captures printables before anything
    /// else (main.rs); `None` = normal popup navigation.
    pub name_input: Option<String>,
    /// The pane `Confirm` navigates. The focused one for history, hotlist
    /// and `pane.select-drive`; a fixed SIDE for `-left`/`-right` regardless
    /// of where focus is (design §D — that's how Total Commander's
    /// `Alt+F1`/`Alt+F2` behave). Frozen on open, same reason as the rest of
    /// the item: nothing here gets re-resolved against a focus that may have
    /// moved under the popup.
    pub(crate) target_pane: usize,
    /// Volumes only: whether the CURRENT list includes pseudo-filesystems
    /// (design §E's "show all" toggle). Meaningless in history/hotlist,
    /// where it stays `false`.
    pub(crate) include_pseudo: bool,
    /// Only a history opened by SIDE (`pane.history-left/-right`): which
    /// side, so the title can say so. `None` everywhere else.
    pub(crate) side: Option<usize>,
    /// A history or popular list's filter (`dialog.filter`, spec
    /// 2026-09-15 D2): `Some` while typing, and then text keys belong to it.
    /// `None` unfiltered and in the other lists.
    pub filter: Option<String>,
}

impl NavPopup {
    /// Moves the cursor up (clamped at the top).
    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Moves the cursor down (clamped at the last item).
    pub fn down(&mut self) {
        if self.cursor + 1 < self.items.len() {
            self.cursor += 1;
        }
    }

    /// Items frozen for rendering.
    #[must_use]
    pub fn items(&self) -> &[NavItem] {
        &self.items
    }

    /// Highlighted index.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The highlighted item, if any.
    #[must_use]
    pub fn selected(&self) -> Option<&NavItem> {
        self.items.get(self.cursor)
    }

    /// The pane `Confirm` must navigate — see the field.
    #[must_use]
    pub fn target_pane(&self) -> usize {
        self.target_pane
    }

    /// Whether the current volumes list includes pseudo-filesystems — see
    /// the field. Meaningless outside `NavPopupKind::Volumes`.
    #[must_use]
    pub fn include_pseudo(&self) -> bool {
        self.include_pseudo
    }
}

/// Display for a navigation popup item: `[name — ]path` with the hostile
/// badge as a PREFIX if any part would show up altered (same criterion as
/// the panes: lossy and MARKED, spec §6).
pub(crate) fn nav_item_display(
    name: Option<&str>,
    path: &VPath,
    enc: Option<norte_encoding::NameEncoding>,
) -> String {
    // #98/F4: popups are a DECISION surface (choosing a jump destination) —
    // they follow the focused pane's reinterpretation, like the status bar.
    let (text, path_hostile) = norte_frontend::path_display_with(path, enc);
    let (prefix, name_hostile) = match name {
        Some(n) => {
            let (nt, nh) = display_name(n.as_bytes());
            (format!("{nt} — "), nh)
        }
        None => (String::new(), false),
    };
    if path_hostile || name_hostile {
        format!("{} {prefix}{text}", crate::ui::HOSTILE_BADGE)
    } else {
        format!("{prefix}{text}")
    }
}

/// Items for a history or popular list, from the SHARED rows
/// ([`norte_frontend::history::history_rows`]): the path sanitized like any
/// other, and the row's mark (`here`, `forward`) in its own field, which the
/// painter puts after it and in a different style.
pub(crate) fn history_items(
    rows: Vec<norte_frontend::history::HistoryRow>,
    enc: Option<norte_encoding::NameEncoding>,
) -> Vec<NavItem> {
    rows.into_iter()
        .map(|r| NavItem {
            display: nav_item_display(None, &r.path, enc),
            mark: norte_frontend::history::mark_key(r.mark).map(t),
            target: Some(r.path),
            hotlist_name: None,
        })
        .collect()
}

/// Rows for the volumes popup (design §D): `main.rs` calls this right after
/// `Backend::volumes` answers and hands the result to
/// [`crate::app::App::open_volumes_popup`] — this function owns none of the I/O, only the
/// presentation, same split as the rest of the popup family.
#[must_use]
pub fn volume_items(
    volumes: &[norte_proto::methods::Volume],
    enc: Option<norte_encoding::NameEncoding>,
) -> Vec<NavItem> {
    volumes
        .iter()
        .map(|v| NavItem {
            display: volume_item_display(v, enc),
            target: Some(v.mount.clone()),
            hotlist_name: None,
            mark: None,
        })
        .collect()
}

/// One volume row: `[label — ]mount  fs_type  free / total`. Every text
/// field the platform hands us — label, mount AND `fs_type` — goes through
/// the same masking [`nav_item_display`] uses (`display_name`/
/// `path_display_with`, both backed by `norte_encoding::is_terminal_hazard`)
/// before it reaches the screen. `fs_type` is not the closed, ASCII-only
/// vocabulary it looks like: a FUSE mount's `fuse.<subtype>` component is the
/// `-o subtype=` value an UNPRIVILEGED user picks (`sshfs`, `rclone mount`,
/// `encfs`…), so it is exactly as untrusted as a filename — encoding-auditor
/// review caught it reaching the row unmasked in an earlier draft of this
/// function, the same class of bug `control_escape` in the canonical corpus
/// exists to catch. `free`/`total` print `volumes-size-unknown` instead of a
/// number when the filesystem did not answer in time — design §A is explicit
/// that a bare `0` here would read as "full", the opposite of what an absent
/// size means.
///
/// `label` is `Option<Vec<u8>>` (V3.5, a second encoding-auditor finding on
/// the same review pass that caught `fs_type` above): it reaches
/// [`display_name`] as the raw bytes the wire carried, with NO `String`
/// upstream to have already thrown away or lossily rewritten a non-UTF-8
/// label before the masking ever saw it — otherwise the badge below would
/// be protecting evidence that was already gone.
fn volume_item_display(
    v: &norte_proto::methods::Volume,
    enc: Option<norte_encoding::NameEncoding>,
) -> String {
    // #98/F4 (same reasoning `nav_item_display` carries): a popup is a
    // decision surface, so it follows the focused pane's reinterpretation.
    let (path_text, path_hostile) = norte_frontend::path_display_with(&v.mount, enc);
    let (label_prefix, label_hostile) = match v.label.as_deref() {
        Some(l) => {
            let (nt, nh) = display_name(l);
            (format!("{nt} — "), nh)
        }
        None => (String::new(), false),
    };
    let (fs_type_text, fs_type_hostile) = display_name(v.fs_type.as_bytes());
    let free = v
        .free_bytes
        .map_or_else(|| t("volumes-size-unknown"), norte_frontend::human_bytes);
    let total = v
        .total_bytes
        .map_or_else(|| t("volumes-size-unknown"), norte_frontend::human_bytes);
    let body = format!("{label_prefix}{path_text}  {fs_type_text}  {free} / {total}");
    if path_hostile || label_hostile || fs_type_hostile {
        format!("{} {body}", crate::ui::HOSTILE_BADGE)
    } else {
        body
    }
}
