//! `org.norte.rename-log`: a hook that says how many files a rename touched
//! (demo of the `hook` category, ADR 0100).
//!
//! The decision lives in a pure function with its own tests; the WIT glue
//! only exists when compiled as a component.

/// The sentence for `renamed` entries seen in one call, or `None` when there
/// is nothing to say. Plural handled by hand: a hook's text is its own.
#[must_use]
pub fn summary(renamed: usize) -> Option<String> {
    match renamed {
        0 => None,
        1 => Some("renamed 1 file".to_owned()),
        n => Some(format!("renamed {n} files")),
    }
}

#[cfg(target_arch = "wasm32")]
mod guest {
    wit_bindgen::generate!({
        world: "norte:hook/norte-hook",
        path: "wit",
        generate_all,
    });

    use exports::norte::hook::hook::{Effect, Event, Guest, Op};

    struct RenameLog;

    impl Guest for RenameLog {
        fn on_events(events: Vec<Event>) -> Result<Vec<Effect>, String> {
            // The host already filtered by the manifest's `on`; counting by
            // op anyway is what keeps this correct if the manifest grows.
            let renamed = events.iter().filter(|e| e.op == Op::Renamed).count();
            Ok(crate::summary(renamed)
                .map(Effect::Notify)
                .into_iter()
                .collect())
        }
    }

    export!(RenameLog);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_in_plain_words() {
        assert_eq!(summary(0), None);
        assert_eq!(summary(1).as_deref(), Some("renamed 1 file"));
        assert_eq!(summary(3).as_deref(), Some("renamed 3 files"));
    }
}
