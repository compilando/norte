//! The right-click menu's shared model: WHICH entries each surface offers.
//!
//! Pure, with no I/O and no host types: the host measures the surface under
//! the pointer (a row, the empty area, a column header, a place, a tree
//! branch), reduces it to a [`Surface`] and asks [`entries`] for the list.
//!
//! What this module does NOT decide:
//!
//! - Whether an entry can run right now: that is
//!   [`crate::availability::verdict`] (plus "not in the command registry"),
//!   asked by the host per entry. The list here never changes with the
//!   verdict, so the core stays identical on every row and the hand learns it
//!   once.
//! - What an entry says: labels are Fluent keys, resolved by the renderer.
//! - What it does: a [`Action::Command`] goes through the one command
//!   dispatcher; a [`Action::Verb`] is a menu-local act the frontend handles.

/// What the pointer was over when the menu was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// One or several file rows (the marks, or the row under the pointer).
    Row(RowTarget),
    /// The empty area of a pane; `remote` when the folder is on a connection.
    Empty {
        /// The folder is on a connection (adds Disconnect).
        remote: bool,
    },
    /// A column header; `hideable` is for the host's availability, the list
    /// does not change with it.
    Header {
        /// The column can be hidden.
        hideable: bool,
    },
    /// A drive, a favorite or a section header in the places sidebar.
    Place(PlaceTarget),
    /// A branch of the folder tree.
    Branch,
}

/// The facts about a row target the model needs, all computed by the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowTarget {
    /// How many entries the menu acts on.
    pub count: usize,
    /// The entry's kind; `None` when `count > 1`.
    pub kind: Option<norte_proto::EntryKind>,
    /// Single entry, `nav::archive_root_for(e).is_some()`.
    pub archive: bool,
    /// Every target entry is `EntryKind::File`.
    pub all_files: bool,
}

/// What kind of place was clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceTarget {
    /// A drive.
    Drive,
    /// A favorite.
    Favorite,
    /// A section header (fold / unfold only).
    SectionHeader,
}

/// A menu-local act that is not a catalogue command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// Open the place or branch in the active pane.
    OpenHere,
    /// Open it in the other pane.
    OpenInOther,
    /// Open it in a new tab.
    OpenInNewTab,
    /// Copy its path.
    CopyPath,
    /// Add it to the favorites.
    AddFavorite,
    /// Remove it from the favorites.
    RemoveFavorite,
    /// Fold or unfold the section / branch.
    ToggleFold,
    /// Sort by the clicked column.
    SortByColumn,
    /// Hide the clicked column (window-only).
    HideColumn,
}

/// What choosing an entry does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// A catalogue command id, run through the one dispatcher.
    Command(&'static str),
    /// A menu-local verb.
    Verb(Verb),
}

/// One menu entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// What it does.
    pub action: Action,
    /// The Fluent key of its label.
    pub label_key: &'static str,
    /// Starts a section: `Some(key)` titled, `Some("")` a bare rule, `None`
    /// continues the previous one.
    pub section: Option<&'static str>,
}

/// Longest target name, in chars, shown in the menu header.
pub const TARGET_MAX_CHARS: usize = 40;

/// `s` unchanged, or its first [`TARGET_MAX_CHARS`] chars plus `…` when
/// longer. Counts chars, never bytes, so a multibyte name is not cut mid-char.
#[must_use]
pub fn elide(s: &str) -> String {
    match s.char_indices().nth(TARGET_MAX_CHARS) {
        Some((cut, _)) => format!("{}…", &s[..cut]),
        None => s.to_owned(),
    }
}

fn cmd(id: &'static str, label_key: &'static str, section: Option<&'static str>) -> Entry {
    Entry {
        action: Action::Command(id),
        label_key,
        section,
    }
}

fn verb(v: Verb, label_key: &'static str, section: Option<&'static str>) -> Entry {
    Entry {
        action: Action::Verb(v),
        label_key,
        section,
    }
}

/// The entries of `surface`, in display order.
#[must_use]
pub fn entries(surface: &Surface) -> Vec<Entry> {
    match surface {
        Surface::Row(t) => row_entries(t),
        Surface::Empty { remote } => {
            let mut v = vec![
                cmd(
                    "pane.mkdir",
                    "menu-item-pane-mkdir",
                    Some("ctx-section-folder"),
                ),
                cmd("pane.edit-new", "menu-item-pane-edit-new", None),
                cmd(
                    "pane.refresh",
                    "menu-item-pane-refresh",
                    Some("ctx-section-view"),
                ),
                cmd("pane.toggle-hidden", "menu-item-pane-toggle-hidden", None),
                cmd("pane.sort-menu", "menu-item-pane-sort-menu", None),
                cmd("pane.columns", "menu-item-pane-columns", None),
                cmd("mark.all", "menu-item-mark-all", Some("ctx-section-marks")),
                cmd("mark.invert", "menu-item-mark-invert", None),
                cmd(
                    "pane.ai-rename",
                    "menu-item-pane-ai-rename",
                    Some("ctx-section-ai"),
                ),
                cmd("pane.organize", "menu-item-pane-organize", None),
            ];
            if *remote {
                v.push(cmd(
                    "pane.disconnect",
                    "menu-item-pane-disconnect",
                    Some("ctx-section-connection"),
                ));
            }
            v
        }
        // `hideable` is the host's to dim; the list stays the same.
        Surface::Header { .. } => vec![
            verb(Verb::SortByColumn, "ctx-sort-by-column", None),
            verb(Verb::HideColumn, "ctx-hide-column", None),
            cmd("pane.columns", "menu-item-pane-columns", Some("")),
        ],
        Surface::Place(PlaceTarget::SectionHeader) => {
            vec![verb(Verb::ToggleFold, "ctx-toggle-fold", None)]
        }
        Surface::Place(p) => {
            let mut v = open_verbs();
            v.push(match p {
                PlaceTarget::Favorite => {
                    verb(Verb::RemoveFavorite, "ctx-remove-favorite", Some(""))
                }
                _ => verb(Verb::AddFavorite, "ctx-add-favorite", Some("")),
            });
            v
        }
        Surface::Branch => {
            let mut v = open_verbs();
            v.push(verb(Verb::AddFavorite, "ctx-add-favorite", Some("")));
            v.push(verb(Verb::ToggleFold, "ctx-toggle-fold", None));
            v
        }
    }
}

fn open_verbs() -> Vec<Entry> {
    vec![
        verb(Verb::OpenHere, "ctx-open-here", None),
        verb(Verb::OpenInOther, "ctx-open-in-other", None),
        verb(Verb::OpenInNewTab, "ctx-open-in-new-tab", None),
        verb(Verb::CopyPath, "ctx-copy-path", None),
    ]
}

fn row_entries(t: &RowTarget) -> Vec<Entry> {
    use norte_proto::EntryKind;
    let single = t.count == 1;
    let enters =
        single && (matches!(t.kind, Some(EntryKind::Dir | EntryKind::Symlink)) || t.archive);
    let mut v = vec![
        cmd(
            if enters { "nav.enter" } else { "pane.open" },
            "ctx-open",
            None,
        ),
        cmd("pane.view", "menu-item-pane-view", None),
        cmd("pane.edit", "menu-item-pane-edit", None),
        cmd("pane.copy", "menu-item-pane-copy", Some("")),
        cmd("pane.move", "menu-item-pane-move", None),
        cmd("pane.rename", "menu-item-pane-rename", None),
        cmd("pane.delete", "menu-item-pane-delete", None),
        cmd("pane.copy-path", "menu-item-pane-copy-path", Some("")),
        cmd("pane.properties", "menu-item-pane-properties", None),
    ];
    if single && t.archive {
        v.push(cmd(
            "pane.unpack",
            "menu-item-pane-unpack",
            Some("ctx-section-archive"),
        ));
        v.push(cmd(
            "pane.test-archive",
            "menu-item-pane-test-archive",
            None,
        ));
    }
    if single && t.kind == Some(EntryKind::Dir) {
        v.push(cmd(
            "pane.dir-size",
            "menu-item-pane-dir-size",
            Some("ctx-section-directory"),
        ));
        v.push(cmd(
            "pane.compare-dirs",
            "menu-item-pane-compare-dirs",
            None,
        ));
        v.push(cmd("pane.sync-dirs", "menu-item-pane-sync-dirs", None));
    }
    if t.count >= 2 {
        v.push(cmd(
            "pane.rename-batch",
            "menu-item-pane-rename-batch",
            Some("ctx-section-selection"),
        ));
        if t.count == 2 && t.all_files {
            v.push(cmd(
                "pane.compare-files",
                "menu-item-pane-compare-files",
                None,
            ));
        }
    }
    if !t.archive {
        v.push(cmd("pane.pack", "menu-item-pane-pack", Some("")));
    }
    v.push(cmd(
        "pane.checksum",
        "menu-item-pane-checksum",
        Some("ctx-section-more"),
    ));
    v.push(cmd("pane.chmod", "menu-item-pane-chmod", None));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::EntryKind;

    fn row(count: usize, kind: Option<EntryKind>, archive: bool, all_files: bool) -> Surface {
        Surface::Row(RowTarget {
            count,
            kind,
            archive,
            all_files,
        })
    }
    fn commands(s: &Surface) -> Vec<&'static str> {
        entries(s)
            .iter()
            .filter_map(|e| match e.action {
                Action::Command(c) => Some(c),
                Action::Verb(_) => None,
            })
            .collect()
    }
    const CORE_TAIL: [&str; 8] = [
        "pane.view",
        "pane.edit",
        "pane.copy",
        "pane.move",
        "pane.rename",
        "pane.delete",
        "pane.copy-path",
        "pane.properties",
    ];

    /// The core is the SAME nine entries, in the same order, on every row: the
    /// hand learns it once.
    #[test]
    fn the_core_is_identical_on_every_row() {
        for s in [
            row(1, Some(EntryKind::File), false, true),
            row(1, Some(EntryKind::Dir), false, false),
            row(1, Some(EntryKind::File), true, true),
            row(5, None, false, false),
        ] {
            let c = commands(&s);
            assert!(matches!(c[0], "nav.enter" | "pane.open"), "{c:?}");
            assert_eq!(&c[1..9], &CORE_TAIL, "{c:?}");
        }
    }

    #[test]
    fn open_enters_a_directory_or_an_archive_and_opens_a_file() {
        assert_eq!(
            commands(&row(1, Some(EntryKind::Dir), false, false))[0],
            "nav.enter"
        );
        assert_eq!(
            commands(&row(1, Some(EntryKind::Symlink), false, false))[0],
            "nav.enter"
        );
        assert_eq!(
            commands(&row(1, Some(EntryKind::File), true, true))[0],
            "nav.enter"
        );
        assert_eq!(
            commands(&row(1, Some(EntryKind::File), false, true))[0],
            "pane.open"
        );
        // Several: `pane.open` (the verdict dims nothing; the dispatch refuses
        // a multiple target with its own message).
        assert_eq!(commands(&row(3, None, false, true))[0], "pane.open");
    }

    #[test]
    fn contextual_sections_appear_only_where_they_apply() {
        let file = commands(&row(1, Some(EntryKind::File), false, true));
        assert!(!file.contains(&"pane.unpack") && !file.contains(&"pane.dir-size"));
        assert!(file.contains(&"pane.pack") && file.contains(&"pane.checksum"));

        let zip = commands(&row(1, Some(EntryKind::File), true, true));
        assert!(zip.contains(&"pane.unpack") && zip.contains(&"pane.test-archive"));
        assert!(
            !zip.contains(&"pane.pack"),
            "an archive is not packed again"
        );

        let dir = commands(&row(1, Some(EntryKind::Dir), false, false));
        for c in ["pane.dir-size", "pane.compare-dirs", "pane.sync-dirs"] {
            assert!(dir.contains(&c), "{c}");
        }

        let two_files = commands(&row(2, None, false, true));
        assert!(
            two_files.contains(&"pane.compare-files") && two_files.contains(&"pane.rename-batch")
        );
        let two_mixed = commands(&row(2, None, false, false));
        assert!(!two_mixed.contains(&"pane.compare-files"));
        let three = commands(&row(3, None, false, true));
        assert!(!three.contains(&"pane.compare-files") && three.contains(&"pane.rename-batch"));
    }

    /// AI rename, organize and disconnect act on the whole folder: never on a row.
    #[test]
    fn folder_wide_commands_are_not_on_a_row() {
        for s in [
            row(1, Some(EntryKind::File), false, true),
            row(4, None, false, false),
        ] {
            let c = commands(&s);
            for no in ["pane.ai-rename", "pane.organize", "pane.disconnect"] {
                assert!(!c.contains(&no), "{no} on a row");
            }
        }
    }

    #[test]
    fn the_empty_area_has_ai_always_and_disconnect_only_when_remote() {
        let local = commands(&Surface::Empty { remote: false });
        for c in [
            "pane.mkdir",
            "pane.edit-new",
            "pane.refresh",
            "pane.toggle-hidden",
            "pane.sort-menu",
            "pane.columns",
            "mark.all",
            "mark.invert",
            "pane.ai-rename",
            "pane.organize",
        ] {
            assert!(local.contains(&c), "{c}");
        }
        assert!(!local.contains(&"pane.disconnect"));
        assert!(commands(&Surface::Empty { remote: true }).contains(&"pane.disconnect"));
    }

    #[test]
    fn header_places_and_branch_are_verbs() {
        let verbs = |s: &Surface| -> Vec<Verb> {
            entries(s)
                .iter()
                .filter_map(|e| match e.action {
                    Action::Verb(v) => Some(v),
                    Action::Command(_) => None,
                })
                .collect()
        };
        assert_eq!(
            verbs(&Surface::Header { hideable: true }),
            [Verb::SortByColumn, Verb::HideColumn]
        );
        assert_eq!(
            commands(&Surface::Header { hideable: true }),
            ["pane.columns"]
        );
        let base = [
            Verb::OpenHere,
            Verb::OpenInOther,
            Verb::OpenInNewTab,
            Verb::CopyPath,
        ];
        assert_eq!(
            verbs(&Surface::Place(PlaceTarget::Drive)),
            [&base[..], &[Verb::AddFavorite]].concat()
        );
        assert_eq!(
            verbs(&Surface::Place(PlaceTarget::Favorite)),
            [&base[..], &[Verb::RemoveFavorite]].concat()
        );
        assert_eq!(
            verbs(&Surface::Place(PlaceTarget::SectionHeader)),
            [Verb::ToggleFold]
        );
        assert_eq!(
            verbs(&Surface::Branch),
            [&base[..], &[Verb::AddFavorite, Verb::ToggleFold]].concat()
        );
    }

    /// Every label and section key exists in BOTH locales.
    #[test]
    fn every_key_is_translated() {
        let surfaces = [
            row(1, Some(EntryKind::File), false, true),
            row(1, Some(EntryKind::Dir), false, false),
            row(1, Some(EntryKind::File), true, true),
            row(2, None, false, true),
            Surface::Empty { remote: true },
            Surface::Header { hideable: true },
            Surface::Place(PlaceTarget::Drive),
            Surface::Place(PlaceTarget::Favorite),
            Surface::Place(PlaceTarget::SectionHeader),
            Surface::Branch,
        ];
        for s in &surfaces {
            for e in entries(s) {
                for key in std::iter::once(e.label_key).chain(e.section.filter(|k| !k.is_empty())) {
                    for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
                        let t = norte_i18n::t_in(lang, key);
                        assert_ne!(t, key, "{key} missing in {lang:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn elide_counts_chars_not_bytes() {
        assert_eq!(elide("corto"), "corto");
        let long = "ñ".repeat(50);
        let e = elide(&long);
        assert_eq!(e.chars().count(), 41);
        assert!(e.ends_with('…'));
    }
}
