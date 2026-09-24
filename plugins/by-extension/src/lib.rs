//! `org.norte.by-extension`: an organizer that files each name into a folder
//! named after its extension (demo of the `organizer` category, phase 8).
//!
//! It needs no capabilities at all: the answer is in the name. That is on
//! purpose — it makes this plugin the smallest possible end-to-end exercise
//! of the `norte:organizer` ABI, so a failure here is the ABI and not the
//! plugin's own cleverness.
//!
//! The decisions live in pure functions with their own tests; the WIT glue
//! only exists when compiled as a component.

/// The folder a name belongs in: its extension, lowercased, or `sin-extension`
/// when it has none.
///
/// A leading dot is NOT an extension (`.bashrc` has none), which is the rule
/// every file manager uses and the one a human expects to see.
#[must_use]
pub fn folder_for(name: &str) -> String {
    match name.rsplit_once('.') {
        // `rsplit_once` on `.bashrc` gives `("", "bashrc")`: an empty stem
        // means the dot was leading, so there is no extension.
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => ext.to_ascii_lowercase(),
        _ => "sin-extension".to_owned(),
    }
}

/// Where `name` should go, or `None` when there is nothing to propose.
///
/// `None` for a name that has no folder to go to and for one that would move
/// onto itself. Returning a no-op would make the plan look like it does
/// something it does not.
#[must_use]
pub fn destination(name: &str) -> Option<String> {
    if name.is_empty() || name.contains('/') {
        return None;
    }
    Some(format!("{}/{name}", folder_for(name)))
}

#[cfg(target_family = "wasm")]
mod guest {
    wit_bindgen::generate!({
        world: "norte:organizer/norte-organizer",
        path: "wit",
        generate_all,
    });

    use exports::norte::organizer::organizer::{Guest, LocationRef, Proposal};

    struct ByExtension;

    impl Guest for ByExtension {
        fn plan(
            id: String,
            _location: Option<LocationRef>,
            names: Vec<String>,
        ) -> Result<Vec<Proposal>, String> {
            if id != "by-extension" {
                return Err(format!("unknown organizer `{id}`"));
            }
            Ok(names
                .into_iter()
                .filter_map(|n| {
                    super::destination(&n).map(|proposed_rel| Proposal {
                        current: n,
                        proposed_rel,
                    })
                })
                .collect())
        }
    }

    export!(ByExtension);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_is_the_lowercased_extension() {
        assert_eq!(folder_for("photo.JPG"), "jpg");
        assert_eq!(folder_for("a.tar.gz"), "gz");
    }

    /// A leading dot is NOT an extension: `.bashrc` does not go to a folder
    /// called `bashrc`.
    #[test]
    fn a_leading_dot_is_not_an_extension() {
        assert_eq!(folder_for(".bashrc"), "sin-extension");
        assert_eq!(folder_for("README"), "sin-extension");
        assert_eq!(folder_for("ends.in.dot."), "sin-extension");
    }

    #[test]
    fn the_destination_hangs_off_the_folder() {
        assert_eq!(
            destination("invoice.pdf").as_deref(),
            Some("pdf/invoice.pdf")
        );
        assert_eq!(
            destination("README").as_deref(),
            Some("sin-extension/README")
        );
    }

    /// A name with `/` is not a name, and nothing is proposed for it: the
    /// host would reject it, and proposing it would dirty the plan with
    /// something the reader will see disappear.
    #[test]
    fn a_name_with_a_slash_proposes_nothing() {
        assert!(destination("a/b").is_none());
        assert!(destination("").is_none());
    }
}
