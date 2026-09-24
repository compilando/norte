//! TC navigation (spec 2026-07-18): the PURE quick search (`Mode`, `matches`,
//! `QuickSearch`) now lives in [`norte_frontend::nav`] — shared with the GUI —
//! and is re-exported here so the TUI's call sites need not change. The
//! per-pane directory history ([`History`]) is TUI-specific and stays.

pub use norte_frontend::nav::{
    History, Mode, QuickSearch, Trail, TrailStep, archive_root_for, fold, matches,
};

#[cfg(test)]
mod archive_nav_tests {
    use super::*;
    use norte_proto::{Entry, EntryKind, VPath};

    fn entry(wire: &str, kind: EntryKind) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse(wire).expect("test wire"),
            kind,
            size: None,
            mtime_ms: None,
        }
    }

    #[test]
    fn archive_root_for_decides_by_extension_and_kind() {
        let e = entry("file:///d/A.ZIP", EntryKind::File);
        assert_eq!(
            archive_root_for(&e)
                .expect("uppercase is accepted")
                .to_wire(),
            "zip+file:///d/A.ZIP/!"
        );
        assert!(archive_root_for(&entry("file:///d/a.tar", EntryKind::File)).is_some());
        assert!(archive_root_for(&entry("file:///d/a.txt", EntryKind::File)).is_none());
        // A dir named x.zip is NOT a container; neither is a symlink (v1).
        assert!(archive_root_for(&entry("file:///d/x.zip", EntryKind::Dir)).is_none());
        assert!(archive_root_for(&entry("file:///d/x.zip", EntryKind::Symlink)).is_none());
        // #56 (previously v1 = no-op): Enter on a zip INSIDE a tar composes
        // one more layer — navigable nesting.
        assert_eq!(
            archive_root_for(&entry("tar+file:///a.tar/!/i.zip", EntryKind::File))
                .expect("navigable nesting")
                .to_wire(),
            "zip+tar+file:///a.tar/!/i.zip/!"
        );
    }

    /// #55: `.tgz`/`.tar.gz` do not match the `tar+gz` token via the generic
    /// `.{format}` (the `+` is not in the file extension) — `EXT_ALIASES`
    /// maps them explicitly, case-insensitive, before the generic path.
    /// Plain `.tar`/`.zip` keep working without going through the alias
    /// (`.tar.gz` must NOT match `.tar`: it ends in `.gz`).
    #[test]
    fn archive_root_for_targz_extensions() {
        for wire in [
            "file:///d/a.tgz",
            "file:///d/a.tar.gz",
            "file:///d/A.TAR.GZ",
        ] {
            let root = archive_root_for(&entry(wire, EntryKind::File))
                .unwrap_or_else(|| panic!("{wire} should be navigable"));
            assert_eq!(root.scheme(), "tar+gz+file", "wire={wire}");
        }
        // Plain extensions keep working (not captured by the alias).
        assert_eq!(
            archive_root_for(&entry("file:///d/a.tar", EntryKind::File))
                .expect("plain tar still works")
                .scheme(),
            "tar+file"
        );
        assert_eq!(
            archive_root_for(&entry("file:///d/a.zip", EntryKind::File))
                .expect("plain zip still works")
                .scheme(),
            "zip+file"
        );
    }

    /// Encoding lock (#55, review): `ends_ci` operates on BYTES and the
    /// compose does not go through String — a NON-UTF8 name ending in `.tgz`
    /// composes fine and its raw bytes survive the wire (rule 1). If someone
    /// "simplifies" this tomorrow with `to_str()`/lossy, this turns red.
    #[test]
    fn archive_root_for_targz_non_utf8_name() {
        for wire in [
            "file:///d/%FF%FE.tgz",
            "file:///d/a%F1o.TGZ",
            "file:///d/%FF.tar.gz",
        ] {
            let root = archive_root_for(&entry(wire, EntryKind::File))
                .unwrap_or_else(|| panic!("{wire} should be navigable"));
            assert_eq!(root.scheme(), "tar+gz+file", "wire={wire}");
        }
        assert_eq!(
            archive_root_for(&entry("file:///d/%FF%FE.tgz", EntryKind::File))
                .expect("non-UTF8 is navigable")
                .to_wire(),
            "tar+gz+file:///d/%FF%FE.tgz/!",
            "the raw bytes survive the compose"
        );
    }
}
