//! A search's results as a listing you can work in ("Open as listing").
//!
//! A [`ResultSet`] is what a results TAB carries next to its entries: the
//! query that produced them ([`SavedSearch`]), how the run ended, the match
//! context per hit and the paths a mutation is still expected to remove.
//! - **C1**: the entries are NOT here. They are the pane's ordinary
//!   `Vec<Entry>`, which already sorts, marks and filters them; a second
//!   list would be a second thing to keep in step.
//! - **C10/C11**: a hit is a file in some folder below the search root, so
//!   the tab has no directory of its own. A real listing (`set_listing`,
//!   `begin_loading`, `refill`) ends the result set; a move, delete or
//!   rename submitted from the tab records its sources ([`ResultSet::expect`])
//!   and the tab stats only those after the mutation finished
//!   ([`ResultSet::settle`], `PaneState::forget`).
//! - Refresh and the session do not keep a stale list: they re-run the
//!   [`SavedSearch`] ([`SavedSearch::params`] reads the clock at re-run time).
//!
//! It is PURE, like [`crate::search`]: nothing is sent or painted here.

use std::collections::{BTreeSet, HashMap};

use norte_encoding::NameEncoding;
use norte_i18n::Lang;
use norte_proto::methods::{FsSearchParams, MatchInfo, SearchHits};
use norte_proto::{Entry, VPath};
use serde::{Deserialize, Serialize};

use crate::search::{SearchForm, SearchKinds};
use crate::search_status::{Outcome, status_key};

/// The most hits a results run asks for: the terminal's old search cap.
pub const MAX_HITS: u32 = 10_000;

/// The most chars of the query shown in a tab title before `…`.
pub const LABEL_MAX_CHARS: usize = 32;

const fn yes() -> bool {
    true
}

/// [`SearchKinds`], serializable: what a saved search keeps of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedKinds {
    /// Anything.
    #[default]
    All,
    /// Files only.
    Files,
    /// Directories only.
    Folders,
}

impl From<SearchKinds> for SavedKinds {
    fn from(k: SearchKinds) -> Self {
        match k {
            SearchKinds::All => Self::All,
            SearchKinds::Files => Self::Files,
            SearchKinds::Folders => Self::Folders,
        }
    }
}

impl From<SavedKinds> for SearchKinds {
    fn from(k: SavedKinds) -> Self {
        match k {
            SavedKinds::All => Self::All,
            SavedKinds::Files => Self::Files,
            SavedKinds::Folders => Self::Folders,
        }
    }
}

/// The query a results tab shows: what refresh and the session re-run.
///
/// Every field but `root` has a default, so an older or hand-edited session
/// reads as "everything, recursive" instead of failing whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the form's switches, mirrored one to one"
)]
pub struct SavedSearch {
    /// Where the search started.
    pub root: VPath,
    /// Name pattern.
    #[serde(default)]
    pub name: String,
    /// Content text.
    #[serde(default)]
    pub content: String,
    /// Directory names not descended into.
    #[serde(default)]
    pub exclude: String,
    /// Minimum size, as typed.
    #[serde(default)]
    pub min_size: String,
    /// Maximum size, as typed.
    #[serde(default)]
    pub max_size: String,
    /// Modified in the last N days, as typed.
    #[serde(default)]
    pub days: String,
    /// Encoding to read the content with.
    #[serde(default)]
    pub encoding: String,
    /// Patterns are regex.
    #[serde(default)]
    pub regex: bool,
    /// Case-sensitive.
    #[serde(default)]
    pub case: bool,
    /// Whole-word content match.
    #[serde(default)]
    pub whole_word: bool,
    /// Descend into subdirectories.
    #[serde(default = "yes")]
    pub recursive: bool,
    /// Which entry kind counts.
    #[serde(default)]
    pub kinds: SavedKinds,
}

impl SavedSearch {
    /// What `form` asked, rooted at `root`.
    #[must_use]
    pub fn from_form(form: &SearchForm, root: VPath) -> Self {
        Self {
            root,
            name: form.name.clone(),
            content: form.content.clone(),
            exclude: form.exclude.clone(),
            min_size: form.min_size.clone(),
            max_size: form.max_size.clone(),
            days: form.days.clone(),
            encoding: form.encoding.clone(),
            regex: form.regex,
            case: form.case,
            whole_word: form.whole_word,
            recursive: form.recursive,
            kinds: form.kinds.into(),
        }
    }

    /// The form this was made from, focused on the name field.
    #[must_use]
    pub fn form(&self) -> SearchForm {
        SearchForm {
            name: self.name.clone(),
            content: self.content.clone(),
            exclude: self.exclude.clone(),
            min_size: self.min_size.clone(),
            max_size: self.max_size.clone(),
            days: self.days.clone(),
            encoding: self.encoding.clone(),
            regex: self.regex,
            case: self.case,
            whole_word: self.whole_word,
            recursive: self.recursive,
            kinds: self.kinds.into(),
            ..SearchForm::new()
        }
    }

    /// The wire request, with "N days ago" read from `now_ms` NOW.
    #[must_use]
    pub fn params(&self, now_ms: i64) -> FsSearchParams {
        crate::search::params(&self.form(), self.root.clone(), now_ms, MAX_HITS)
    }

    /// The query as a tab shows it, masked and clamped to
    /// [`LABEL_MAX_CHARS`], and whether masking changed anything.
    ///
    /// The content wins over the name when only one is shown; with both it
    /// reads `name · content`; with neither, `*`.
    #[must_use]
    pub fn label(&self) -> (String, bool) {
        let (name, content) = (self.name.trim(), self.content.trim());
        let raw = match (name.is_empty(), content.is_empty()) {
            (false, false) => format!("{name} · {content}"),
            (true, false) => content.to_owned(),
            (false, true) => name.to_owned(),
            (true, true) => "*".to_owned(),
        };
        let (shown, hostile) = crate::display_name(raw.as_bytes());
        (clamp_chars(&shown, LABEL_MAX_CHARS), hostile)
    }
}

/// `max` chars at most, the last one `…` when cut.
fn clamp_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// What a results tab carries besides its entries. See the module docs.
#[derive(Debug, Clone, PartialEq)]
pub struct ResultSet {
    search: SavedSearch,
    outcome: Outcome,
    cap: u32,
    received: u32,
    matches: HashMap<VPath, MatchInfo>,
    expected: BTreeSet<VPath>,
}

impl ResultSet {
    /// A run that has not delivered anything yet, capped at `cap` hits.
    #[must_use]
    pub fn new(search: SavedSearch, cap: u32) -> Self {
        Self {
            search,
            outcome: Outcome::Running,
            cap,
            received: 0,
            matches: HashMap::new(),
            expected: BTreeSet::new(),
        }
    }

    /// The query this tab shows.
    #[must_use]
    pub const fn search(&self) -> &SavedSearch {
        &self.search
    }

    /// Where the search started.
    #[must_use]
    pub const fn root(&self) -> &VPath {
        &self.search.root
    }

    /// How the run is going or ended.
    #[must_use]
    pub const fn outcome(&self) -> &Outcome {
        &self.outcome
    }

    /// Records how the run ended (or that it is going).
    pub fn set_outcome(&mut self, outcome: Outcome) {
        self.outcome = outcome;
    }

    /// Is the run still going?
    #[must_use]
    pub const fn is_running(&self) -> bool {
        matches!(self.outcome, Outcome::Running)
    }

    /// Starts over for a re-run capped at `cap`: running, nothing received,
    /// nothing expected.
    pub fn restart(&mut self, cap: u32) {
        self.outcome = Outcome::Running;
        self.cap = cap;
        self.received = 0;
        self.matches.clear();
        self.expected.clear();
    }

    /// Counts a batch and keeps the FIRST match context per path.
    pub fn note_hits(&mut self, hits: &SearchHits) {
        let added = u32::try_from(hits.entries.len()).unwrap_or(u32::MAX);
        self.received = self.received.saturating_add(added);
        if let Some(infos) = &hits.matches {
            for (entry, info) in hits.entries.iter().zip(infos) {
                self.matches
                    .entry(entry.path.clone())
                    .or_insert_with(|| info.clone());
            }
        }
    }

    /// Match context that arrived before the tab existed (the panel's run).
    pub fn seed_matches(&mut self, matches: Vec<(VPath, MatchInfo)>) {
        for (path, info) in matches {
            self.matches.entry(path).or_insert(info);
        }
    }

    /// The match context of one hit, if the search was by content.
    #[must_use]
    pub fn match_of(&self, path: &VPath) -> Option<&MatchInfo> {
        self.matches.get(path)
    }

    /// Did the run receive as many hits as it may?
    #[must_use]
    pub const fn at_cap(&self) -> bool {
        self.received >= self.cap
    }

    /// Records the sources of a mutation just submitted from this tab.
    pub fn expect(&mut self, paths: impl IntoIterator<Item = VPath>) {
        self.expected.extend(paths);
    }

    /// The paths still to check once the mutation finishes.
    #[must_use]
    pub fn expected(&self) -> Vec<VPath> {
        self.expected.iter().cloned().collect()
    }

    /// Stops expecting `path`.
    pub fn settle(&mut self, path: &VPath) {
        self.expected.remove(path);
    }
}

/// `🔍 <query> (n)`: the tab's title, and whether the query was masked.
#[must_use]
pub fn tab_title(set: &ResultSet, n: usize, lang: Lang) -> (String, bool) {
    let (label, hostile) = set.search.label();
    let title = norte_i18n::ta_in(
        lang,
        "results-tab-title",
        &[("query", &label), ("n", &n.to_string())],
    );
    (title, hostile)
}

/// `search “<query>” in <root>`: the location line, and whether anything in
/// it was masked.
#[must_use]
pub fn location(set: &ResultSet, lang: Lang) -> (String, bool) {
    let (label, hostile) = set.search.label();
    let (root, root_hostile) = crate::path_display(set.root());
    let text = norte_i18n::ta_in(
        lang,
        "results-location",
        &[("query", &label), ("root", &root)],
    );
    (text, hostile || root_hostile)
}

/// The status sentence, with the shared precedence of
/// [`crate::search_status::status_key`].
#[must_use]
pub fn status(set: &ResultSet, n: usize, lang: Lang) -> String {
    let key = status_key(&set.outcome, set.at_cap());
    match &set.outcome {
        Outcome::Failed(category) => norte_i18n::ta_in(lang, key, &[("error", category)]),
        _ => norte_i18n::ta_in(lang, key, &[("n", &n.to_string())]),
    }
}

/// The hit's folder relative to the search root, masked; `.` for a hit in
/// the root itself.
#[must_use]
pub fn folder_text(entry: &Entry, root: &VPath, enc: Option<NameEncoding>) -> String {
    let Some(parent) = entry.path.parent() else {
        return ".".to_owned();
    };
    // Not under the root (other scheme, authority or prefix): the relative
    // form would be wrong, so say where the hit really is.
    let under = parent.scheme() == root.scheme()
        && parent.authority() == root.authority()
        && parent.segments().count() >= root.segments().count()
        && root.segments().zip(parent.segments()).all(|(r, p)| r == p);
    if !under {
        return crate::path_display_with(&parent, enc).0;
    }
    let skip = root.segments().count();
    let parts: Vec<String> = parent
        .segments()
        .skip(skip)
        .map(|s| crate::display_name_with(s, enc).0)
        .collect();
    if parts.is_empty() {
        ".".to_owned()
    } else {
        parts.join("/")
    }
}

/// C4: copying hits into `dest` is refused only when SOME of them already
/// lives there (its parent is `dest`, byte for byte). The Fluent key.
#[must_use]
pub fn transfer_refusal(paths: &[VPath], dest: &VPath) -> Option<&'static str> {
    paths
        .iter()
        .any(|p| p.parent().as_ref() == Some(dest))
        .then_some("results-already-there")
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::{Entry, EntryKind, TaskId, VPath};

    fn vp(s: &str) -> VPath {
        VPath::parse(s).expect("test wire")
    }
    fn file(p: &str) -> Entry {
        Entry {
            path: vp(p),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        }
    }
    fn hits(entries: Vec<Entry>) -> SearchHits {
        SearchHits {
            task_id: TaskId::new(1),
            entries,
            matches: None,
            all_matches: None,
            more: None,
        }
    }
    fn saved(name: &str, content: &str) -> SavedSearch {
        let mut f = SearchForm::new();
        f.name = name.to_owned();
        f.content = content.to_owned();
        f.days = "7".to_owned();
        f.kinds = crate::search::SearchKinds::Files;
        SavedSearch::from_form(&f, vp("mem:///casa"))
    }

    /// The saved query round-trips through JSON and rebuilds the SAME
    /// params as the form it came from, with the clock read at re-run time
    /// (seven days before `now_ms`, not before the original launch).
    #[test]
    fn a_saved_search_round_trips_and_re_runs_from_now() {
        let s = saved("*.rs", "aguja");
        let back: SavedSearch =
            serde_json::from_value(serde_json::to_value(&s).expect("ser")).expect("de");
        assert_eq!(back, s);
        let now = 30 * 86_400_000;
        let p = back.params(now);
        assert_eq!(
            p,
            crate::search::params(&s.form(), vp("mem:///casa"), now, MAX_HITS)
        );
        assert_eq!(p.mtime_after, Some(23 * 86_400_000));
    }

    /// An older session's object with only `root` reads as "everything,
    /// recursive" instead of failing the whole session.
    #[test]
    fn a_minimal_saved_search_reads_with_defaults() {
        let s: SavedSearch = serde_json::from_str(r#"{"root":"mem:///casa"}"#).expect("de");
        assert!(s.recursive);
        assert_eq!(s.kinds, SavedKinds::All);
    }

    /// Whitespace-only content must not give a blank label.
    #[test]
    fn a_blank_content_does_not_blank_the_label() {
        assert_eq!(saved("*.rs", "  \t").label().0, "*.rs");
        assert_eq!(saved("", "  ").label().0, "*");
        assert_eq!(saved("*.rs", " aguja ").label().0, "*.rs · aguja");
    }

    /// A hit not under the root shows its full parent path, not a wrong
    /// relative folder (a sibling with the same depth, say).
    #[test]
    fn folder_text_outside_the_root_is_the_full_parent() {
        let root = vp("file:///casa/uno");
        assert_eq!(
            folder_text(&file("file:///otra/dos/a.rs"), &root, None),
            "/otra/dos"
        );
        assert_eq!(
            folder_text(&file("file:///casa/dos/a.rs"), &root, None),
            "/casa/dos"
        );
        assert_eq!(
            folder_text(&file("file:///casa/uno/x/a.rs"), &root, None),
            "x"
        );
    }

    /// The label: content wins over the name; both are masked; long ones
    /// are cut at `LABEL_MAX_CHARS` with `…`.
    #[test]
    fn the_label_is_masked_and_clamped() {
        assert_eq!(saved("*.rs", "").label(), ("*.rs".to_owned(), false));
        assert_eq!(saved("*.rs", "aguja").label().0, "*.rs · aguja");
        let (hostile, flag) = saved("a\u{1b}[31mb", "").label();
        assert!(flag && !hostile.contains('\u{1b}'));
        let long = saved(&"x".repeat(80), "").label().0;
        assert_eq!(long.chars().count(), LABEL_MAX_CHARS);
        assert!(long.ends_with('…'));
    }

    /// The tab title carries the count; the status uses the shared
    /// precedence (cancelled beats the cap).
    #[test]
    fn title_and_status() {
        let mut set = ResultSet::new(saved("", "aguja"), 2);
        let lang = norte_i18n::Lang::En;
        assert_eq!(tab_title(&set, 3, lang).0, "🔍 aguja (3)");
        set.note_hits(&hits(vec![file("mem:///casa/a"), file("mem:///casa/b")]));
        assert!(set.at_cap());
        set.set_outcome(Outcome::Cancelled);
        assert!(status(&set, 2, lang).contains("cancelled"));
    }

    /// Folder relative to the root; a hit IN the root says `.`; a hostile
    /// segment is masked.
    #[test]
    fn folder_text_is_relative_and_masked() {
        let root = vp("mem:///casa");
        assert_eq!(
            folder_text(&file("mem:///casa/src/x/a.rs"), &root, None),
            "src/x"
        );
        assert_eq!(folder_text(&file("mem:///casa/a.rs"), &root, None), ".");
        let bad = Entry {
            path: root
                .join(norte_proto::Segment::new(b"d\x1b".to_vec()).expect("seg"))
                .join(norte_proto::Segment::new(b"a".to_vec()).expect("seg")),
            ..file("mem:///casa/a")
        };
        assert!(!folder_text(&bad, &root, None).contains('\u{1b}'));
    }

    /// C4: copying into the root is fine unless SOME hit already lives
    /// there.
    #[test]
    fn transfer_refusal_only_for_hits_already_there() {
        let root = vp("mem:///casa");
        let deep = [vp("mem:///casa/src/a.rs"), vp("mem:///casa/doc/b.md")];
        assert_eq!(transfer_refusal(&deep, &root), None);
        let mixed = [vp("mem:///casa/src/a.rs"), vp("mem:///casa/c.txt")];
        assert_eq!(
            transfer_refusal(&mixed, &root),
            Some("results-already-there")
        );
    }

    /// The first `MatchInfo` per path wins; the expected set settles.
    #[test]
    fn hits_keep_the_first_match_and_expectations_settle() {
        let mut set = ResultSet::new(saved("", "x"), 10);
        let m = |l| norte_proto::methods::MatchInfo {
            line: Some(l),
            ..Default::default()
        };
        let mut h = hits(vec![file("mem:///casa/a")]);
        h.matches = Some(vec![m(1)]);
        set.note_hits(&h);
        h.matches = Some(vec![m(2)]);
        set.note_hits(&h);
        assert_eq!(
            set.match_of(&vp("mem:///casa/a")).and_then(|i| i.line),
            Some(1)
        );
        set.expect([vp("mem:///casa/a"), vp("mem:///casa/b")]);
        set.settle(&vp("mem:///casa/a"));
        assert_eq!(set.expected(), vec![vp("mem:///casa/b")]);
        set.restart(5);
        assert!(
            set.is_running()
                && set.expected().is_empty()
                && set.match_of(&vp("mem:///casa/a")).is_none()
        );
    }
}
