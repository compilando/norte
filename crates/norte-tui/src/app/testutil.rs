//! The constructors `app`'s tests share: paths, entries, panes and
//! already-assembled `App`s. Lives outside any `mod tests` because seven
//! sibling modules use it, and duplicating it in each one was the
//! alternative.

use super::pane::Pane;
use super::*;
use norte_proto::{Entry, EntryKind, Scheme};

pub fn root() -> VPath {
    VPath::root(Scheme::new("mem").unwrap(), None)
}

pub fn file(name: &str) -> Entry {
    Entry {
        attrs: std::collections::BTreeMap::new(),
        path: root().join(norte_proto::Segment::new(name.as_bytes().to_vec()).unwrap()),
        kind: EntryKind::File,
        size: Some(1),
        mtime_ms: None,
    }
}

pub fn names(p: &Pane) -> Vec<String> {
    p.entries()
        .iter()
        .map(|e| String::from_utf8_lossy(e.path.file_name().unwrap().as_bytes()).into_owned())
        .collect()
}

pub fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("test wire")
}

/// Pane over `mem://` with files named as requested: #54, `Pane` (via
/// `PaneState::new`) normalizes the order internally (dirs first, NFC, ties
/// by bytes) — the quick search tests reason over the real, ALREADY SORTED
/// index, not over `names`'s arrival order.
pub fn pane_con(names: &[&str]) -> Pane {
    Pane::new(root(), names.iter().map(|n| file(n)).collect())
}

pub fn app_dos_panes() -> App {
    App::new(pane_con(&["a"]), pane_con(&["b"]))
}

/// Some caps or other: what's tested is the per-scheme CACHE, not which
/// flags the provider carries.
pub fn test_caps() -> norte_proto::Capabilities {
    norte_proto::Capabilities {
        flags: norte_proto::CapabilityFlags::RENAME_ATOMIC,
        max_path: None,
    }
}

/// `App` with each pane on ITS OWN dir (the `app_dos_panes` above puts both
/// on `root()`, which doesn't distinguish sides).
pub fn app_en(left: &str, right: &str) -> App {
    App::new(
        Pane::new(vp(left), Vec::new()),
        Pane::new(vp(right), Vec::new()),
    )
}

pub fn e(wire: &str, k: EntryKind) -> Entry {
    Entry {
        attrs: std::collections::BTreeMap::new(),
        path: VPath::parse(wire).unwrap(),
        kind: k,
        size: None,
        mtime_ms: None,
    }
}

/// App with a single listing of names, over `mem://` (task 9, #103): via
/// `pane_con` — the same real order the UI paints — with focus on the full
/// pane; the other one empty. Different name from `app_with_sized_entries`
/// (`tests/status_marks.rs`): that one carries an explicit size, this one
/// only names.
pub fn app_with_entries(names: &[&str]) -> App {
    App::new(pane_con(names), Pane::new(root(), Vec::new()))
}

/// Like [`app_with_entries`], with the INACTIVE pane planted on `dst`
/// (empty): the orthodox destination for F5/F6 is the OTHER pane's
/// DIRECTORY (#103 T10), so the batch tests need a destination different
/// from the source root.
pub fn app_with_two_panes(names: &[&str], dst: &str) -> App {
    App::new(
        pane_con(names),
        Pane::new(VPath::parse(dst).unwrap(), Vec::new()),
    )
}

/// A `connection.degraded` notice like the wire's (#44).
pub fn test_degraded(scheme: &str, host: &str) -> norte_proto::methods::ConnectionDegraded {
    norte_proto::methods::ConnectionDegraded {
        scheme: scheme.to_owned(),
        host: host.to_owned(),
        reason: "ftp-plaintext".to_owned(),
        detail: None,
    }
}

/// A one-sided (orphan) comparison row with the requested kind and size, for
/// the `compare_size_probe_targets` tests (#157).
pub fn orphan_row(id: u64, kind: EntryKind, size: Option<u64>) -> norte_proto::methods::CompareRow {
    use norte_proto::methods::{CompareConfidence, CompareCriterion, CompareVerdict};
    norte_proto::methods::CompareRow {
        id,
        left: Some(Entry {
            attrs: std::collections::BTreeMap::new(),
            path: root().join(norte_proto::Segment::new(format!("f{id}").into_bytes()).unwrap()),
            kind,
            size,
            mtime_ms: None,
        }),
        right: None,
        verdict: CompareVerdict::OnlyLeft,
        criterion: CompareCriterion::Presence,
        confidence: CompareConfidence::Certain,
        newer: None,
        reason: None,
        side: None,
        paired_under: None,
    }
}
