//! `org.norte.rename-log`: a hook that says how many files a rename touched
//! (demo of the `hook` category, ADR 0100).
//!
//! The decision lives in a pure function with its own tests; the WIT glue
//! only exists when compiled as a component.

/// The sentence for `renamed` entries seen in one call, or `None` when there
/// is nothing to say. `dropped` is how many events the host lost since the
/// last call: when it is not zero the count is a floor, and the sentence
/// says so rather than state it as fact. Plural handled by hand: a hook's
/// text is its own.
#[must_use]
pub fn summary(renamed: usize, dropped: u64) -> Option<String> {
    let base = match renamed {
        0 if dropped == 0 => return None,
        0 => "renamed some files".to_owned(),
        1 => "renamed 1 file".to_owned(),
        n => format!("renamed {n} files"),
    };
    if dropped > 0 {
        Some(format!("{base} (at least: {dropped} events were missed)"))
    } else {
        Some(base)
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
        fn on_events(events: Vec<Event>, dropped: u64) -> Result<Vec<Effect>, String> {
            // The host already filtered by the manifest's `on`; counting by
            // op anyway is what keeps this correct if the manifest grows.
            let renamed = events.iter().filter(|e| e.op == Op::Renamed).count();
            Ok(crate::summary(renamed, dropped)
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
        assert_eq!(summary(0, 0), None);
        assert_eq!(summary(1, 0).as_deref(), Some("renamed 1 file"));
        assert_eq!(summary(3, 0).as_deref(), Some("renamed 3 files"));
    }

    #[test]
    fn a_lossy_stream_is_not_stated_as_fact() {
        assert_eq!(
            summary(3, 2).as_deref(),
            Some("renamed 3 files (at least: 2 events were missed)")
        );
        assert_eq!(
            summary(0, 5).as_deref(),
            Some("renamed some files (at least: 5 events were missed)")
        );
    }
}
