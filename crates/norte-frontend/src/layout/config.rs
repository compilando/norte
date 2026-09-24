//! Loading a pane layout from a file.
//!
//! A layout's name **is a file name**, and that is why it travels as
//! [`OsStr`] and not as `String` (#246): `--layout` and the picker both end
//! up at `<dir>/layouts/<name>.toml`, so passing the value through
//! `to_string_lossy` changed which file opens — `$'\xff'` and `$'\xfe'` both
//! landed on `layouts/\xEF\xBF\xBD.toml`, silently.
//!
//! And the file is resolved **byte for byte against the directory** (#245):
//! on APFS or NTFS, `load(dir, "orthodox")` with a saved `Orthodox.toml`
//! opened the user's file while the row said "factory" and the preview
//! showed the preset. Here the entries are listed and the EXACT name is
//! required, so the result is the same on all three systems.

use std::ffi::{OsStr, OsString};
use std::path::Path;

use super::{LayoutError, Node};

/// The subdirectory of the config directory they live in.
pub const LAYOUTS_DIR: &str = "layouts";

/// The extension, without the dot.
const EXT: &str = "toml";

/// Win32 device names, resolved BEFORE looking at the disk.
///
/// Rejected on ALL systems, not only Windows: a `layouts/CON.toml` created
/// on Linux and synced to a Windows machine would open the console from a
/// TUI that has the terminal in raw mode, and `NUL` would give an empty
/// read. A reserved name is no more valid on one side than the other, so it
/// is rejected where it is written and where it is read.
/// Can this name be a `layouts/` file and nothing else?
///
/// `Path::components().count() == 1` is NOT enough and that was the
/// previous check (#246): on Windows `Path::new("C:")` is exactly one
/// component — a `Prefix` — and `Path::join` with a prefix REPLACES the
/// whole base, so the `format!` ended up reading `C:.toml` relative to
/// drive C's current directory. Here the name is looked at, not its path
/// shape.
/// Delegates to [`norte_config::valid_profile_name`], which is the SAME
/// question — "can this be a loose entry of one of our directories?" — and
/// was answered twice. The canonical one lives in `norte-config` because it
/// is downstream: profiles need it so a name cannot point the config layer
/// anywhere else on disk, and two copies of a security rule diverge.
fn name_usable(name: &OsStr) -> bool {
    norte_config::valid_profile_name(name)
}

/// `<name>.toml`, without going through `String`.
fn con_extension(name: &OsStr) -> OsString {
    let mut f = name.to_os_string();
    f.push(".");
    f.push(EXT);
    f
}

/// Reads `<dir>/layouts/<name>.toml` and validates what it carries.
///
/// The format is the SAME one [`to_toml`] serializes and the same one L2's
/// session blob carries: one for the file, the session, and whatever a
/// future layout editor spits out (ADR 0058).
///
/// The file is looked up in the directory's LISTING and its name is
/// required to match byte for byte with what was asked: on a system that
/// is not case-sensitive, letting the OS resolve it opened the user's file
/// when the factory one was asked for (#245).
///
/// # Errors
///
/// [`LayoutError::BadName`] if the name cannot be a `layouts/` file,
/// [`LayoutError::NotFound`] if there is none with that exact name,
/// [`LayoutError::Parse`] if it is not valid TOML or does not describe a
/// tree, and whatever [`super::validate`] returns if the tree is
/// inconsistent.
pub fn load(dir: &Path, name: &OsStr) -> Result<Node, LayoutError> {
    if !name_usable(name) {
        return Err(LayoutError::BadName(name.to_string_lossy().into_owned()));
    }
    let folder = dir.join(LAYOUTS_DIR);
    let sought = con_extension(name);
    let exists = std::fs::read_dir(&folder)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| e.file_name() == sought);
    let path = folder.join(&sought);
    if !exists {
        return Err(LayoutError::NotFound(path.display().to_string()));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|_| LayoutError::NotFound(path.display().to_string()))?;
    let tree: Node = toml::from_str(&text).map_err(|e| LayoutError::Parse(e.to_string()))?;
    super::validate(&tree)?;
    Ok(tree)
}

/// The names of the layouts the user has in `<dir>/layouts/`, sorted.
///
/// Does not validate or parse: the picker SHOWS them, and whoever picks a
/// broken one finds out when picking it, with the loader's error.
///
/// A directory that does not exist is not an error: it is a user who has
/// not saved any.
///
/// Returns [`OsString`] and not `String`: a name that is not UTF-8 is a
/// file like any other and used to disappear from the picker with no word
/// (#246 m2). The extension is compared case-insensitively, because
/// `MIO.TOML` is the same file to the OS that saved it that way.
#[must_use]
pub fn list(dir: &Path) -> Vec<OsString> {
    let Ok(entries) = std::fs::read_dir(dir.join(LAYOUTS_DIR)) else {
        return Vec::new();
    };
    let mut names: Vec<OsString> = entries
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x.to_string_lossy().eq_ignore_ascii_case(EXT))
        })
        .filter_map(|e| e.path().file_stem().map(OsStr::to_os_string))
        // A name `load` would not accept is not offered: the row would be
        // there just to fail when clicked.
        .filter(|n| name_usable(n))
        .collect();
    names.sort();
    names
}

/// The tree as TOML, to write it out.
///
/// # Errors
///
/// [`LayoutError::Parse`] if the tree cannot be serialized.
pub fn to_toml(tree: &Node) -> Result<String, LayoutError> {
    toml::to_string_pretty(tree).map_err(|e| LayoutError::Parse(e.to_string()))
}

/// Which tree is valid for name `name`, given what the user's file
/// returned.
///
/// **The user's file WINS**, as in every other configuration layer, and a
/// preset is recovered by deleting the file. A file that is not there is
/// NORMAL for a factory name and nothing is warned about; a BROKEN one
/// falls back to the preset just the same — a layout that does not parse
/// cannot leave norte with no screen — but it IS warned about, and that
/// warning is the tuple's `Some`.
///
/// Exists here, and not inside each frontend, because it is a rule and
/// duplicated rules diverge: the TUI had it and the window did not, so
/// `norte-gui --layout mine` could not open a user layout while `ntc
/// --layout mine` could — and the SAME window offered it in its picker.
///
/// Reading the file is the caller's job: each frontend knows which thread
/// can do I/O (rule 2), and this function does none.
///
/// # Errors
///
/// The user file's own error if there is no preset with that name either,
/// and [`LayoutError::NotFound`] if the name is not even text — a factory
/// preset is called by its ASCII name.
///
/// ```
/// use norte_frontend::layout::{LayoutError, config::or_preset};
/// use std::ffi::OsStr;
///
/// // With no user file the preset is left, and no warning.
/// let (tree, notice) =
///     or_preset(OsStr::new("simple"), Err(LayoutError::NotFound(String::new())))
///         .expect("`simple` is a factory one");
/// assert_eq!(tree.slot_ids().len(), 3);
/// assert!(notice.is_none());
/// ```
pub fn or_preset(
    name: &OsStr,
    loaded: Result<Node, LayoutError>,
) -> Result<(Node, Option<LayoutError>), LayoutError> {
    let broken = match loaded {
        Ok(tree) => return Ok((tree, None)),
        // No file at all is NORMAL for a factory one: not warned about.
        Err(LayoutError::NotFound(_)) => None,
        Err(e) => Some(e),
    };
    // A factory preset is called by its ASCII name: a name that is not
    // text cannot be one of them.
    // The name goes into a MESSAGE, so it is masked: a file name can carry
    // an ESC, and `valid_profile_name` does not filter controls. The bytes
    // that open the file are `name`'s, untouched.
    let factory = name.to_str().map_or_else(
        || {
            Err(LayoutError::NotFound(
                norte_encoding::mask_terminal_hazards(&crate::display::display_os_name(name).0),
            ))
        },
        super::presets::tree,
    );
    match factory {
        Ok(tree) => Ok((tree, broken)),
        Err(e) => Err(broken.unwrap_or(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Dir, KindId, SlotId};

    fn tree() -> Node {
        Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        )
    }

    fn write(dir: &Path, file: &OsStr, text: &str) {
        let layouts = dir.join(LAYOUTS_DIR);
        std::fs::create_dir_all(&layouts).expect("mkdir");
        std::fs::write(layouts.join(file), text).expect("write");
    }

    #[test]
    fn a_written_layout_is_read_back() {
        let dir = tempfile::tempdir().expect("tmp");
        write(
            dir.path(),
            OsStr::new("mio.toml"),
            &to_toml(&tree()).expect("toml"),
        );
        assert_eq!(load(dir.path(), OsStr::new("mio")).expect("loads"), tree());
    }

    /// The user's file WINS over a preset of the same name.
    #[test]
    fn the_user_file_wins_over_the_preset() {
        let (placed_tree, notice) =
            or_preset(OsStr::new("simple"), Ok(tree())).expect("there is a tree");
        assert_eq!(placed_tree, tree(), "the user's, not the factory one");
        assert!(notice.is_none());
    }

    /// A BROKEN one falls back to the preset AND warns: nobody is left with
    /// no screen, but nobody is hidden the fact that their file is no good
    /// either.
    #[test]
    fn a_broken_file_falls_back_to_the_preset_with_a_warning() {
        let (placed_tree, notice) = or_preset(
            OsStr::new("simple"),
            Err(LayoutError::Parse("line 3".to_owned())),
        )
        .expect("the preset is left");
        assert_eq!(
            placed_tree,
            crate::layout::presets::tree("simple").expect("preset")
        );
        assert!(
            matches!(notice, Some(LayoutError::Parse(_))),
            "the reason arrives to paint it: {notice:?}"
        );
    }

    /// And a name that is neither a factory one nor has a file returns the
    /// FILE's error, which is what the reader can fix.
    #[test]
    fn with_neither_file_nor_preset_the_files_error_wins() {
        let e = or_preset(
            OsStr::new("mio"),
            Err(LayoutError::Parse("line 3".to_owned())),
        )
        .expect_err("there is nowhere to get it from");
        assert!(matches!(e, LayoutError::Parse(_)), "{e:?}");
    }

    #[test]
    fn listing_returns_the_tomls_sorted_and_without_extension() {
        let dir = tempfile::tempdir().expect("tmp");
        assert!(list(dir.path()).is_empty(), "no directory, no names");
        for n in ["zeta.toml", "alfa.toml", "notas.txt"] {
            write(dir.path(), OsStr::new(n), "");
        }
        assert_eq!(
            list(dir.path()),
            vec![OsString::from("alfa"), OsString::from("zeta")]
        );
    }

    /// `MIO.TOML` is the same file to the system that saved it that way, and
    /// `--layout MIO` loaded it: not appearing in the picker was a missing
    /// row, not a protection (#246 m2).
    #[test]
    fn the_extension_is_case_insensitive() {
        let dir = tempfile::tempdir().expect("tmp");
        write(dir.path(), OsStr::new("MIO.TOML"), "");
        assert_eq!(list(dir.path()), vec![OsString::from("MIO")]);
    }

    /// A name with separators does NOT build a path: it comes from the
    /// user's config — from ANY layer, the project's included — and
    /// `../something` would leave the layouts directory.
    #[test]
    fn a_name_with_a_path_inside_it_is_rejected() {
        let dir = tempfile::tempdir().expect("tmp");
        for bad in ["../secreto", "", "sub/mio", "sub\\mio", ".", ".."] {
            assert!(
                matches!(
                    load(dir.path(), OsStr::new(bad)),
                    Err(LayoutError::BadName(_))
                ),
                "{bad:?} should be rejected"
            );
        }
    }

    /// `Path::new("C:")` is ONE component on Windows — a `Prefix` — and
    /// `join` with it replaces the whole base: the "single component" check
    /// admitted it and the read went to drive C's current directory (#246
    /// M2).
    #[test]
    fn a_drive_prefix_is_not_a_name() {
        let dir = tempfile::tempdir().expect("tmp");
        assert!(matches!(
            load(dir.path(), OsStr::new("C:")),
            Err(LayoutError::BadName(_))
        ));
        assert!(
            matches!(
                load(dir.path(), OsStr::new("notas:secreto")),
                Err(LayoutError::BadName(_))
            ),
            "an NTFS alternate stream either"
        );
    }

    /// `--layout CON` read `layouts\\CON.toml`, which Win32 resolves to the
    /// CONSOLE, from a TUI with the terminal in raw mode (#246 M2). Rejected
    /// on every system: the file syncs, the reserved name travels with it.
    #[test]
    fn windows_device_names_are_rejected_everywhere() {
        let dir = tempfile::tempdir().expect("tmp");
        for bad in ["CON", "con", "NUL", "com1", "LPT9", "CON.toml"] {
            assert!(
                matches!(
                    load(dir.path(), OsStr::new(bad)),
                    Err(LayoutError::BadName(_))
                ),
                "{bad} should be rejected"
            );
        }
        // And not offered in the picker, which is where they come from
        // without typing them.
        write(dir.path(), OsStr::new("CON.toml"), "");
        assert!(list(dir.path()).is_empty());
    }

    /// Windows eats trailing dots and spaces: the file that opens would not
    /// be the one named.
    #[test]
    fn a_trailing_dot_or_space_is_not_a_name() {
        let dir = tempfile::tempdir().expect("tmp");
        for bad in ["mio.", "mio "] {
            assert!(
                matches!(
                    load(dir.path(), OsStr::new(bad)),
                    Err(LayoutError::BadName(_))
                ),
                "{bad:?} should be rejected"
            );
        }
    }

    /// The name is resolved against the LISTING, byte for byte. On APFS or
    /// NTFS `load(dir, "orthodox")` with a saved `Orthodox.toml` opened the
    /// user's file while the row said "factory" (#245); here there is no
    /// file with that name, period — the same answer on all three systems.
    #[test]
    fn a_name_that_only_differs_in_case_is_not_the_same_file() {
        let dir = tempfile::tempdir().expect("tmp");
        write(
            dir.path(),
            OsStr::new("Orthodox.toml"),
            &to_toml(&tree()).expect("toml"),
        );
        assert!(matches!(
            load(dir.path(), OsStr::new("orthodox")),
            Err(LayoutError::NotFound(_))
        ));
        assert_eq!(
            load(dir.path(), OsStr::new("Orthodox")).expect("theirs does load"),
            tree()
        );
    }

    #[test]
    fn a_missing_layout_is_reported_by_its_name() {
        let dir = tempfile::tempdir().expect("tmp");
        assert!(matches!(
            load(dir.path(), OsStr::new("nada")),
            Err(LayoutError::NotFound(_))
        ));
    }

    /// An INCONSISTENT tree is rejected at load time, not at paint time: the
    /// place where a user can do something about it is startup.
    #[test]
    fn an_incoherent_layout_never_gets_painted() {
        let dir = tempfile::tempdir().expect("tmp");
        let repeated = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(1), KindId::browser()),
            ],
        );
        write(
            dir.path(),
            OsStr::new("roto.toml"),
            &to_toml(&repeated).expect("toml"),
        );
        assert!(matches!(
            load(dir.path(), OsStr::new("roto")),
            Err(LayoutError::DuplicateSlotId(_))
        ));
    }

    /// With no listing at all there is no layout: rejected at load time,
    /// which is where the previous screen still remains (#242).
    #[test]
    fn a_layout_without_a_listing_never_gets_applied() {
        let dir = tempfile::tempdir().expect("tmp");
        let none = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::new("places")),
                Node::slot(SlotId(4), KindId::new("status")),
            ],
        );
        write(
            dir.path(),
            OsStr::new("sin.toml"),
            &to_toml(&none).expect("toml"),
        );
        assert!(matches!(
            load(dir.path(), OsStr::new("sin")),
            Err(LayoutError::NoBrowser)
        ));
    }

    /// The WHOLE hostile corpus goes through the loader and NO name gets
    /// the read out of `<dir>/layouts/`: not the separators, not `C:`, not
    /// a Win32 reserved one, not a name that is not text. The previous
    /// check was `components().count() == 1`, which admits `C:` (#246 M2).
    ///
    /// Checked against the ERROR and the effect: what is not rejected has
    /// to give `NotFound` for a file INSIDE the directory — the directory
    /// does not exist, so none opens — and never `BadName` for something
    /// that was valid.
    #[cfg(unix)]
    #[test]
    fn no_corpus_name_escapes_the_layouts_directory() {
        use std::os::unix::ffi::OsStrExt as _;

        let dir = tempfile::tempdir().expect("tmp");
        let inside = dir.path().join(LAYOUTS_DIR);
        for n in norte_testkit::corpus::hostile_names() {
            let name = OsStr::from_bytes(&n.bytes);
            match load(dir.path(), name) {
                Err(LayoutError::BadName(_)) => {}
                Err(LayoutError::NotFound(path)) => assert!(
                    path.starts_with(&inside.display().to_string()),
                    "{} resolved outside: {path}",
                    n.id
                ),
                other => panic!("{}: {other:?}", n.id),
            }
        }
    }

    /// A name that is not UTF-8 is a file like any other: it shows up in
    /// the listing and loads by its bytes. It used to disappear from the
    /// picker, and through `--layout` it turned into `\u{FFFD}` — i.e.
    /// ANOTHER file, or the same one for two different byte sequences (#246
    /// M1).
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_name_is_neither_lost_nor_confused() {
        use std::os::unix::ffi::OsStrExt as _;

        let dir = tempfile::tempdir().expect("tmp");
        let raw = OsStr::from_bytes(b"\xff");
        write(
            dir.path(),
            OsStr::from_bytes(b"\xff.toml"),
            &to_toml(&tree()).expect("toml"),
        );
        assert_eq!(list(dir.path()), vec![raw.to_os_string()]);
        assert_eq!(load(dir.path(), raw).expect("loads"), tree());
        // The lossy replacement is ANOTHER name, and if it existed it would
        // open instead of the one asked for.
        assert!(matches!(
            load(dir.path(), OsStr::from_bytes("\u{FFFD}".as_bytes())),
            Err(LayoutError::NotFound(_))
        ));
    }
}
