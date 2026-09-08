//! `org.norte.rename-log`: a hook that says how many files a rename touched
//! (demo of the `hook` category, ADR 0100) and keeps a `.norte-renames.log`
//! next to them (the sidecar effect, ADR 0101).
//!
//! The decisions live in pure functions with their own tests; the WIT glue
//! only exists when compiled as a component.

/// The file this hook keeps next to what was renamed. The same name the
/// manifest declares: norte refuses any other.
pub const LOG_NAME: &str = ".norte-renames.log";

/// One line per rename, `old -> new`, on the wire form of the paths. The
/// previous log comes first so the file is append-only from the reader's
/// side even though norte replaces it whole (the old one goes to the trash).
/// Capped at 64 KiB by keeping the newest lines: a log is not an archive.
#[must_use]
pub fn log_content(previous: &[u8], lines: &[(String, String)]) -> Vec<u8> {
    const CAP: usize = 64 * 1024;
    let mut out = previous.to_vec();
    if !out.is_empty() && out.last() != Some(&b'\n') {
        out.push(b'\n');
    }
    for (from, to) in lines {
        out.extend_from_slice(from.as_bytes());
        out.extend_from_slice(b" -> ");
        out.extend_from_slice(to.as_bytes());
        out.push(b'\n');
    }
    if out.len() > CAP {
        // Drop whole lines from the front until it fits.
        let cut = out.len() - CAP;
        let start = out[cut..]
            .iter()
            .position(|b| *b == b'\n')
            .map_or(out.len(), |i| cut + i + 1);
        out.drain(..start);
    }
    out
}

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

    use exports::norte::hook::hook::{Effect, Event, Guest, OnExists, Op, Sidecar};
    use norte::location::location;

    struct RenameLog;

    impl Guest for RenameLog {
        fn on_events(events: Vec<Event>, dropped: u64) -> Result<Vec<Effect>, String> {
            // The host already filtered by the manifest's `on`; counting by
            // op anyway is what keeps this correct if the manifest grows.
            let renamed: Vec<&Event> = events.iter().filter(|e| e.op == Op::Renamed).collect();
            let mut out: Vec<Effect> = crate::summary(renamed.len(), dropped)
                .map(Effect::Notify)
                .into_iter()
                .collect();
            // One log per distinct parent directory, keyed by the location
            // token norte minted for it: the guest never sees the path, only
            // that two events share a token.
            let mut groups: Vec<(String, Vec<u8>, u64, Vec<(String, String)>)> = Vec::new();
            for e in &renamed {
                let Some(loc) = &e.location else { continue };
                // A rename without its origin is a row this build cannot
                // explain; a line starting with " -> " would be a lie.
                let Some(from) = e.path_to.clone() else { continue };
                let line = (from, e.path.clone());
                match groups.iter_mut().find(|g| g.0 == loc.token) {
                    Some(g) => g.3.push(line),
                    None => groups.push((loc.token.clone(), loc.prefix.clone(), e.seq, vec![line])),
                }
            }
            for (token, prefix, seq, lines) in groups.into_iter().take(4) {
                let mut rel = prefix;
                if !rel.is_empty() && rel.last() != Some(&b'/') {
                    rel.push(b'/');
                }
                rel.extend_from_slice(crate::LOG_NAME.as_bytes());
                let previous = location::read(&token, &rel).unwrap_or_default();
                out.push(Effect::WriteSidecar(Sidecar {
                    seq,
                    name: crate::LOG_NAME.as_bytes().to_vec(),
                    content: crate::log_content(&previous, &lines),
                    if_exists: OnExists::Replace,
                }));
            }
            Ok(out)
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
    fn the_log_carries_the_previous_content_and_stays_under_the_cap() {
        let lines = vec![("file:///d/a".to_owned(), "file:///d/b".to_owned())];
        assert_eq!(log_content(b"", &lines), b"file:///d/a -> file:///d/b\n".to_vec());
        assert_eq!(
            log_content(b"old", &lines),
            b"old\nfile:///d/a -> file:///d/b\n".to_vec()
        );
        let big = vec![b'x'; 70 * 1024];
        let out = log_content(&big, &lines);
        assert!(out.len() <= 64 * 1024, "{}", out.len());
        assert!(out.ends_with(b"file:///d/a -> file:///d/b\n"));
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
