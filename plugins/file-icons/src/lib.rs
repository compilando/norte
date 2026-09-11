//! `org.norte.file-icons`: a badge per row from the name alone.
//!
//! Implements the `norte-decorator` world: the host hands over the basenames
//! of the visible page and gets one `Decoration` per entry, in the same
//! order. The whole decision lives in [`icons`], a pure table with its own
//! tests; the WIT glue below only exists when compiled AS a component, so
//! the host-side tests build the same crate without it.

pub mod icons;

#[cfg(target_arch = "wasm32")]
mod guest {
    wit_bindgen::generate!({
        world: "norte-decorator",
        path: "wit",
        generate_all,
    });

    use exports::norte::plugin::decorator::{
        Decoration, Entry, EntryKind, Guest as DecoratorGuest,
    };
    use norte::host::host_config;

    use crate::icons::{Class, Style, icon_with};

    struct FileIcons;

    impl DecoratorGuest for FileIcons {
        fn decorate(entries: Vec<Entry>) -> Vec<Decoration> {
            // The host resolves `[config]` defaults before it calls us, so an
            // absent key would mean the manifest changed under us; `emoji` is
            // then the least surprising answer, not a second default.
            let style = match host_config::get("style").as_deref() {
                Some("ascii") => Style::Ascii,
                Some("nerd") => Style::Nerd,
                _ => Style::Emoji,
            };
            // The two user overrides; empty (the default) means "none".
            let dir_icon = host_config::get("dir-icon").unwrap_or_default();
            let unknown = host_config::get("unknown-icon").unwrap_or_default();
            entries
                .iter()
                .map(|e| {
                    let class = match e.kind {
                        EntryKind::Dir => Class::Dir,
                        EntryKind::Symlink => Class::Symlink,
                        EntryKind::File | EntryKind::Other => Class::File,
                    };
                    Decoration {
                        badge: icon_with(&e.name, class, style, &dir_icon, &unknown)
                            .map(str::to_owned),
                        role: None,
                    }
                })
                .collect()
        }
    }

    export!(FileIcons);
}
