//! `org.norte.git-status`: the official git-status column.
//!
//! The host opens the repository's root —the ancestor containing `.git`,
//! which is what the manifest declares as `location-root-marker`— and hands
//! this guest an opaque token and the prefix of the directory the user is
//! looking at. From there, all this plugin does is read: `.git/index`, the
//! applicable `.gitignore` files, and —only when `stat` is not enough— the
//! file in question.
//!
//! What it does NOT do: write, run `git`, or know where anything is. There
//! are no paths in this code; there is a token and relative paths.
//!
//! # What this column CANNOT say, and why (#225, ADR 0057)
//!
//! It compares the WORKING TREE against the index, and nothing more. The
//! three boundaries, stated here so nobody has to deduce them from the
//! code:
//!
//! - **The "staged" state (index against HEAD).** `M` means "different from
//!   the index". `git status`'s short form has two columns because a file
//!   can be added, or staged and modified again. Telling them apart requires
//!   reading HEAD's tree, i.e. an object-database reader inside a `no_std`
//!   guest: loose objects are zlib streams and packed ones need the pack
//!   index. That is a lot of code, and the first version does not attempt
//!   it.
//! - **Submodules.** Their entry is a gitlink and is recognized as such, so
//!   they are no longer reported as deleted; but knowing whether they have
//!   changes requires opening the repository inside them. The cell stays
//!   EMPTY, which is staying silent instead of asserting.
//! - **A location that is not `file://`.** The location capability mints no
//!   token for sftp, s3, mem or the inside of a compressed archive: the
//!   confined opener needs a real directory descriptor. There the column
//!   comes out empty, which is correct and worth having written down — the
//!   same plugin LOOKS broken to whoever is looking at a remote checkout.
#![cfg_attr(target_arch = "wasm32", no_std)]

extern crate alloc;

// The WASM layer only exists when compiled AS a component: the host's tests
// compile the same crate without it, which is what allows testing the
// decisions without a wasm runtime in between.
#[cfg(target_arch = "wasm32")]
mod guest;
pub mod ignore;
pub mod index;
pub mod sha1;
pub mod status;

/// Id of the column this plugin contributes; the same one from the
/// manifest.
pub const COLUMN_ID: &str = "git-status";

/// Gathers the ignore files that apply to `prefix`: the repository root's,
/// each directory's along the path, and `.git/info/exclude`.
///
/// In that order on purpose: in gitignore the last matching rule wins, and
/// the one closest to the file is the one that rules.
pub fn load_ignores(loc: &dyn status::Location, prefix: &[u8]) -> ignore::Ignores {
    let mut ign = ignore::Ignores::default();
    if let Ok(content) = loc.read(b".git/info/exclude") {
        ign.add_file(b"", &content);
    }
    if let Ok(content) = loc.read(b".gitignore") {
        ign.add_file(b"", &content);
    }
    let mut base: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    for comp in prefix.split(|b| *b == b'/').filter(|c| !c.is_empty()) {
        if !base.is_empty() {
            base.push(b'/');
        }
        base.extend_from_slice(comp);
        let mut file = base.clone();
        file.extend_from_slice(b"/.gitignore");
        if let Ok(content) = loc.read(&file) {
            ign.add_file(&base, &content);
        }
    }
    ign
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    struct Fake(BTreeMap<Vec<u8>, Vec<u8>>);

    impl status::Location for Fake {
        fn read(&self, rel: &[u8]) -> Result<Vec<u8>, String> {
            self.0
                .get(rel)
                .cloned()
                .ok_or_else(|| "does not exist".to_string())
        }

        fn stat(&self, _rel: &[u8]) -> Result<status::Meta, String> {
            Err("not needed".to_string())
        }
    }

    /// The closest `.gitignore` wins, and `.git/info/exclude` counts as one
    /// from the root: all three sources are there, in the order that
    /// decides.
    #[test]
    fn ignores_stack_from_the_root_inward() {
        let mut files = BTreeMap::new();
        files.insert(b".git/info/exclude".to_vec(), b"*.bak\n".to_vec());
        files.insert(b".gitignore".to_vec(), b"*.log\n".to_vec());
        files.insert(b"src/.gitignore".to_vec(), b"!kept.log\n".to_vec());
        let fake = Fake(files);

        let ign = load_ignores(&fake, b"src/deep");
        assert!(ign.is_ignored(b"whatever.bak", false), "the exclude counts");
        assert!(ign.is_ignored(b"root.log", false));
        assert!(
            !ign.is_ignored(b"src/kept.log", false),
            "`src`'s .gitignore beats the root's"
        );
    }

    #[test]
    fn without_ignore_files_there_are_no_rules() {
        let ign = load_ignores(&Fake(BTreeMap::new()), b"a/b");
        assert!(ign.is_empty());
    }
}
