//! The search panel's model (spec 2026-10-09-search-panel): the form, the
//! search-as-you-type pause, the results grouped by folder and file with
//! their matching lines, the cursor over them and the status sentence. It is
//! PURE, like [`crate::search`]: the clock is TOLD (`now`, `now_ms`), nothing
//! is sent or painted here — [`SearchPanel::due`] says what to launch, the
//! frontend launches it and feeds the batches back to
//! [`SearchPanel::apply_hits`], and each frontend paints [`SearchPanel::rows`]
//! with the Fluent keys this hands it.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use norte_proto::methods::{FsSearchParams, MatchInfo, SearchHits};
use norte_proto::{Entry, VPath};

use crate::search::{
    ID_CASE, ID_KINDS, ID_RECURSIVE, ID_REGEX, ID_WHOLE_WORD, SearchField, SearchForm, SearchKinds,
};
use crate::search_status::{Outcome, status_key};
use crate::viewer::ViewerTarget;

/// The pause after the last keystroke before the search launches.
pub const DEBOUNCE: Duration = Duration::from_millis(300);
/// Matching lines shown per file; the rest go behind `+N more`.
pub const SHOWN_PER_FILE: usize = 50;
/// Lines asked per file: one past what is shown, so the `+N more` row has a
/// line to open.
pub const ASKED_PER_FILE: u32 = 51;
/// Files a run collects (the request's `max_hits`).
pub const MAX_FILES: u32 = 2000;

/// One matching line of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchLine {
    /// 1-based line.
    pub line: u64,
    /// Byte offset of the line's start, when the daemon knew it.
    pub offset: Option<u64>,
    /// The line as the daemon sent it (already sanitized at the source).
    pub preview: String,
    /// `[start, end)` in chars of `preview`; empty = nothing to highlight.
    pub ranges: Vec<(u32, u32)>,
}

impl MatchLine {
    /// A line from the wire; `None` without a line number or a preview, which
    /// gives nothing to show nor anywhere to open.
    fn from_wire(m: &MatchInfo) -> Option<Self> {
        Some(Self {
            line: m.line?,
            offset: m.offset,
            preview: m.preview.clone()?,
            ranges: m.ranges.clone().unwrap_or_default(),
        })
    }

    /// Where the viewer opens for this line.
    #[must_use]
    pub fn target(&self) -> ViewerTarget {
        ViewerTarget {
            line: self.line,
            offset: self.offset,
            preview: self.preview.clone(),
            ranges: self.ranges.clone(),
        }
    }
}

/// A file found, with its shown lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileNode {
    /// The entry the daemon sent.
    pub entry: Entry,
    /// Up to [`SHOWN_PER_FILE`] lines.
    pub matches: Vec<MatchLine>,
    /// Matching lines not shown: the ones received past the shown ones plus
    /// the ones the daemon counted and did not send.
    pub more: u32,
    /// The first line past the shown ones: what `+N more` opens.
    next: Option<MatchLine>,
}

/// A folder and the files found directly in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderNode {
    /// The folder.
    pub dir: VPath,
    /// Its files, sorted by name.
    pub files: Vec<FileNode>,
}

/// One painted row, as indices into [`SearchPanel::tree`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Row {
    /// A folder's header.
    Folder {
        /// Index of the folder.
        folder: usize,
    },
    /// A file.
    File {
        /// Index of the folder.
        folder: usize,
        /// Index of the file in the folder.
        file: usize,
    },
    /// A matching line.
    Line {
        /// Index of the folder.
        folder: usize,
        /// Index of the file in the folder.
        file: usize,
        /// Index of the line in the file.
        line: usize,
    },
    /// The `+N more` row of a file.
    More {
        /// Index of the folder.
        folder: usize,
        /// Index of the file in the folder.
        file: usize,
    },
}

/// What activating the cursor's row asks the frontend to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activation {
    /// A folder was folded or unfolded; only a repaint.
    Toggled,
    /// Go to the entry in the file panel: open `dir`, select `entry`.
    GoTo {
        /// The folder to open.
        dir: VPath,
        /// The entry to select in it.
        entry: VPath,
    },
    /// Open the file in the viewer at a line.
    View {
        /// The file.
        path: VPath,
        /// Where, and what to light up.
        target: ViewerTarget,
    },
}

/// What the pause decided, once it is over.
#[derive(Debug)]
pub enum Due {
    /// Launch this search; its batches carry `epoch` back.
    Launch {
        /// The run's epoch, for [`SearchPanel::apply_hits`]/[`SearchPanel::finished`].
        epoch: u64,
        /// The request.
        params: Box<FsSearchParams>,
    },
    /// Nothing to search for: cancel what runs; the results are cleared.
    Clear,
    /// A field cannot be read: nothing launches, what is shown is dimmed.
    Unreadable(SearchField),
}

/// The status key of a failed run: the one whose argument is the failure's
/// text, which each frontend masks before it is said.
pub const KEY_FAILED: &str = "search-panel-failed";

/// The status line, as a Fluent key and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelStatus {
    /// No query (`search-panel-idle`).
    Idle,
    /// A field cannot be read (`search-panel-unreadable`).
    Unreadable(SearchField),
    /// A run, live or finished.
    Run {
        /// `search-panel-{running,done,truncated,cancelled,failed}`.
        key: &'static str,
        /// Files found by the run.
        files: usize,
        /// Folders they are in.
        folders: usize,
        /// The failure's text, for `search-panel-failed`.
        error: Option<String>,
    },
}

/// The run whose batches are accepted.
#[derive(Debug)]
struct Run {
    epoch: u64,
    outcome: Outcome,
    files: usize,
}

/// The search panel: form, pause, results and cursor.
#[derive(Debug)]
pub struct SearchPanel {
    form: SearchForm,
    root: VPath,
    epoch: u64,
    /// When the pause ends; `None` = nothing pending.
    deadline: Option<Instant>,
    run: Option<Run>,
    unreadable: Option<SearchField>,
    /// What is shown belongs to an older query (painting).
    dimmed: bool,
    /// The run's next batch, or its end, clears the tree first.
    replace_on_arrival: bool,
    tree: Vec<FolderNode>,
    /// Folder → its index in `tree`.
    folders: BTreeMap<VPath, usize>,
    collapsed: BTreeSet<VPath>,
    cursor: usize,
    filters_open: bool,
}

impl SearchPanel {
    /// An empty panel searching under `root`.
    #[must_use]
    pub fn new(root: VPath) -> Self {
        Self {
            form: SearchForm::new(),
            root,
            epoch: 0,
            deadline: None,
            run: None,
            unreadable: None,
            dimmed: false,
            replace_on_arrival: false,
            tree: Vec::new(),
            folders: BTreeMap::new(),
            collapsed: BTreeSet::new(),
            cursor: 0,
            filters_open: false,
        }
    }

    /// The form, to paint it.
    #[must_use]
    pub fn form(&self) -> &SearchForm {
        &self.form
    }

    /// Where it searches.
    #[must_use]
    pub fn root(&self) -> &VPath {
        &self.root
    }

    /// The current epoch: the last launch's, or later after a clear.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    fn schedule(&mut self, now: Instant) {
        self.deadline = Some(now + DEBOUNCE);
    }

    /// A field's whole new text (the window's inputs).
    pub fn edit(&mut self, field: SearchField, text: String, now: Instant) {
        self.form.set_text(field, text);
        self.schedule(now);
    }

    /// A keystroke into a field (the terminal).
    pub fn push_char(&mut self, field: SearchField, c: char, now: Instant) {
        self.form.field = field;
        self.form.push_char(c);
        self.schedule(now);
    }

    /// Backspace in a field (the terminal).
    pub fn backspace(&mut self, field: SearchField, now: Instant) {
        self.form.field = field;
        self.form.backspace();
        self.schedule(now);
    }

    /// Flips a switch by its stable id; `false` for an unknown id.
    pub fn toggle(&mut self, id: &str, now: Instant) -> bool {
        match id {
            ID_REGEX => self.form.toggle_regex(),
            ID_CASE => self.form.toggle_case(),
            ID_WHOLE_WORD => self.form.toggle_whole_word(),
            ID_RECURSIVE => self.form.toggle_recursive(),
            ID_KINDS => self.form.cycle_kinds(),
            _ => return false,
        }
        self.schedule(now);
        true
    }

    /// Searches somewhere else.
    pub fn set_root(&mut self, root: VPath, now: Instant) {
        self.root = root;
        self.schedule(now);
    }

    /// Enter: the pause ends now.
    pub fn force(&mut self, now: Instant) {
        self.deadline = Some(now);
    }

    fn clear_tree(&mut self) {
        self.tree.clear();
        self.folders.clear();
        self.cursor = 0;
    }

    /// What to do once the pause is over; `None` before, and after it was
    /// answered once. `now_ms` is the instant "modified in N days" counts
    /// from.
    pub fn due(&mut self, now: Instant, now_ms: i64) -> Option<Due> {
        if now < self.deadline? {
            return None;
        }
        self.deadline = None;
        // C10: filters alone do not search as you type.
        if self.form.name.trim().is_empty() && self.form.content.is_empty() {
            self.epoch += 1;
            self.run = None;
            self.unreadable = None;
            self.dimmed = false;
            self.replace_on_arrival = false;
            self.clear_tree();
            return Some(Due::Clear);
        }
        if let Some(f) = self
            .form
            .field_unreadable()
            .or_else(|| self.form.regex_unreadable())
        {
            // Only dimmed: the running query keeps landing on what is shown.
            self.unreadable = Some(f);
            self.dimmed = true;
            return Some(Due::Unreadable(f));
        }
        self.unreadable = None;
        self.epoch += 1;
        self.run = Some(Run {
            epoch: self.epoch,
            outcome: Outcome::Running,
            files: 0,
        });
        self.dimmed = true;
        self.replace_on_arrival = true;
        let mut params = crate::search::params(&self.form, self.root.clone(), now_ms, MAX_FILES);
        params.max_matches_per_file = Some(ASKED_PER_FILE);
        Some(Due::Launch {
            epoch: self.epoch,
            params: Box::new(params),
        })
    }

    fn is_current(&self, epoch: u64) -> bool {
        self.run.as_ref().is_some_and(|r| r.epoch == epoch)
    }

    /// The first answer of a new run replaces what an older one left.
    fn replace_if_due(&mut self) {
        if self.replace_on_arrival {
            self.replace_on_arrival = false;
            self.clear_tree();
            self.dimmed = self.unreadable.is_some();
        }
    }

    /// A batch of the run `epoch`; `false` (and ignored) if it is not the
    /// current run's.
    pub fn apply_hits(&mut self, epoch: u64, hits: &SearchHits) -> bool {
        if !self.is_current(epoch) {
            return false;
        }
        self.replace_if_due();
        let mut files = self.run.as_ref().map_or(0, |r| r.files);
        for (i, entry) in hits.entries.iter().enumerate() {
            if files >= MAX_FILES as usize {
                break;
            }
            let wire: Vec<&MatchInfo> = match hits.all_matches.as_ref().and_then(|all| all.get(i)) {
                Some(lines) => lines.iter().collect(),
                None => hits
                    .matches
                    .as_ref()
                    .and_then(|m| m.get(i))
                    .into_iter()
                    .collect(),
            };
            let mut matches: Vec<MatchLine> =
                wire.into_iter().filter_map(MatchLine::from_wire).collect();
            let wire_more = hits
                .more
                .as_ref()
                .and_then(|m| m.get(i))
                .copied()
                .unwrap_or(0);
            let past = if matches.len() > SHOWN_PER_FILE {
                matches.split_off(SHOWN_PER_FILE)
            } else {
                Vec::new()
            };
            let more = wire_more.saturating_add(u32::try_from(past.len()).unwrap_or(u32::MAX));
            self.insert(FileNode {
                entry: entry.clone(),
                matches,
                more,
                next: past.into_iter().next(),
            });
            files += 1;
        }
        if let Some(run) = self.run.as_mut() {
            run.files = files;
        }
        self.clamp();
        true
    }

    /// Into its folder (made on first sight, in arrival order), sorted by name.
    fn insert(&mut self, node: FileNode) {
        let dir = node
            .entry
            .path
            .parent()
            .unwrap_or_else(|| self.root.clone());
        let fi = if let Some(&fi) = self.folders.get(&dir) {
            fi
        } else {
            self.tree.push(FolderNode {
                dir: dir.clone(),
                files: Vec::new(),
            });
            self.folders.insert(dir, self.tree.len() - 1);
            self.tree.len() - 1
        };
        let name = |p: &VPath| {
            p.file_name()
                .map(|s| s.as_bytes().to_vec())
                .unwrap_or_default()
        };
        let key = name(&node.entry.path);
        let files = &mut self.tree[fi].files;
        let (Ok(at) | Err(at)) = files.binary_search_by(|f| name(&f.entry.path).cmp(&key));
        files.insert(at, node);
    }

    /// The run `epoch` ended.
    pub fn finished(&mut self, epoch: u64, outcome: Outcome) {
        if !self.is_current(epoch) {
            return;
        }
        self.replace_if_due();
        if let Some(run) = self.run.as_mut() {
            run.outcome = outcome;
        }
        self.clamp();
    }

    /// Is a run still going?
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.run
            .as_ref()
            .is_some_and(|r| r.outcome == Outcome::Running)
    }

    /// Does what is shown belong to an older query?
    #[must_use]
    pub fn is_dimmed(&self) -> bool {
        self.dimmed
    }

    /// Is the filters section unfolded?
    #[must_use]
    pub fn filters_open(&self) -> bool {
        self.filters_open
    }

    /// Unfolds or folds the filters section.
    pub fn toggle_filters(&mut self) {
        self.filters_open = !self.filters_open;
    }

    /// How many filters are set: the `n` of "filters (n)".
    #[must_use]
    pub fn filters_set(&self) -> usize {
        let f = &self.form;
        [&f.exclude, &f.min_size, &f.max_size, &f.days, &f.encoding]
            .into_iter()
            .filter(|s| !s.trim().is_empty())
            .count()
            + usize::from(f.kinds != SearchKinds::All)
            + usize::from(!f.recursive)
    }

    /// The results, folders in arrival order.
    #[must_use]
    pub fn tree(&self) -> &[FolderNode] {
        &self.tree
    }

    /// The rows to paint, folded folders' contents left out.
    #[must_use]
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (folder, node) in self.tree.iter().enumerate() {
            rows.push(Row::Folder { folder });
            if self.collapsed.contains(&node.dir) {
                continue;
            }
            for (file, f) in node.files.iter().enumerate() {
                rows.push(Row::File { folder, file });
                rows.extend((0..f.matches.len()).map(|line| Row::Line { folder, file, line }));
                if f.more > 0 && f.next.is_some() {
                    rows.push(Row::More { folder, file });
                }
            }
        }
        rows
    }

    fn clamp(&mut self) {
        self.cursor = self.cursor.min(self.rows().len().saturating_sub(1));
    }

    /// The cursor's row index.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Puts the cursor on a row (clamped).
    pub fn set_cursor(&mut self, i: usize) {
        self.cursor = i;
        self.clamp();
    }

    /// `n` rows up.
    pub fn up(&mut self, n: usize) {
        self.cursor = self.cursor.saturating_sub(n);
    }

    /// `n` rows down (clamped).
    pub fn down(&mut self, n: usize) {
        self.set_cursor(self.cursor.saturating_add(n));
    }

    /// To the first row.
    pub fn home(&mut self) {
        self.cursor = 0;
    }

    /// To the last row.
    pub fn end(&mut self) {
        self.set_cursor(usize::MAX);
    }

    /// → (`open`) unfolds the cursor's folder; ← folds it, or from inside a
    /// folder climbs to its header.
    pub fn fold(&mut self, open: bool) {
        let rows = self.rows();
        match rows.get(self.cursor) {
            Some(Row::Folder { folder }) => {
                let dir = self.tree[*folder].dir.clone();
                if open {
                    self.collapsed.remove(&dir);
                } else {
                    self.collapsed.insert(dir);
                }
                self.clamp();
            }
            Some(
                Row::File { folder, .. } | Row::Line { folder, .. } | Row::More { folder, .. },
            ) if !open => {
                let header = Row::Folder { folder: *folder };
                if let Some(i) = rows.iter().position(|r| *r == header) {
                    self.cursor = i;
                }
            }
            _ => {}
        }
    }

    /// Activates the cursor's row.
    pub fn activate(&mut self) -> Option<Activation> {
        match *self.rows().get(self.cursor)? {
            Row::Folder { folder } => {
                let dir = &self.tree[folder].dir;
                if !self.collapsed.remove(dir) {
                    self.collapsed.insert(dir.clone());
                }
                self.clamp();
                Some(Activation::Toggled)
            }
            Row::File { folder, file } => Some(Activation::GoTo {
                dir: self.tree[folder].dir.clone(),
                entry: self.tree[folder].files[file].entry.path.clone(),
            }),
            Row::Line { folder, file, line } => {
                let f = &self.tree[folder].files[file];
                Some(Activation::View {
                    path: f.entry.path.clone(),
                    target: f.matches[line].target(),
                })
            }
            Row::More { folder, file } => {
                let f = &self.tree[folder].files[file];
                Some(Activation::View {
                    path: f.entry.path.clone(),
                    target: f.next.as_ref()?.target(),
                })
            }
        }
    }

    /// The status line. The precedence between outcomes is
    /// [`crate::search_status`]'s, so both frontends and the old dialog agree.
    #[must_use]
    pub fn status(&self) -> PanelStatus {
        if let Some(f) = self.unreadable {
            return PanelStatus::Unreadable(f);
        }
        let Some(run) = &self.run else {
            return PanelStatus::Idle;
        };
        let key = match status_key(&run.outcome, run.files >= MAX_FILES as usize) {
            "search-status-running" => "search-panel-running",
            "search-status-done" => "search-panel-done",
            "search-status-truncated" => "search-panel-truncated",
            "search-status-cancelled" => "search-panel-cancelled",
            // `search-status-failed`, the only one left.
            _ => KEY_FAILED,
        };
        let error = match &run.outcome {
            Outcome::Failed(e) => Some(e.clone()),
            _ => None,
        };
        PanelStatus::Run {
            key,
            files: run.files,
            // While the old results wait to be replaced, none are this run's.
            folders: if self.replace_on_arrival {
                0
            } else {
                self.tree.len()
            },
            error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::methods::{MatchInfo, SearchHits};
    use norte_proto::{Entry, EntryKind, TaskId, VPath};
    use std::time::{Duration, Instant};

    fn vp(s: &str) -> VPath {
        VPath::parse(s).expect("test wire")
    }
    fn file(p: &str) -> Entry {
        Entry {
            path: vp(p),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        }
    }
    fn mi(line: u64) -> MatchInfo {
        MatchInfo {
            line: Some(line),
            offset: Some(line * 10),
            preview: Some(format!("l{line} aguja")),
            ranges: Some(vec![(3, 8)]),
        }
    }
    /// A 0.86 batch: per file, its lines and its `more`.
    fn batch(files: &[(&str, Vec<MatchInfo>, u32)]) -> SearchHits {
        SearchHits {
            task_id: TaskId::new(1),
            entries: files.iter().map(|(p, _, _)| file(p)).collect(),
            matches: Some(
                files
                    .iter()
                    .map(|(_, l, _)| l.first().cloned().unwrap_or_default())
                    .collect(),
            ),
            all_matches: Some(files.iter().map(|(_, l, _)| l.clone()).collect()),
            more: Some(files.iter().map(|(_, _, m)| *m).collect()),
        }
    }
    fn typed(p: &mut SearchPanel, text: &str, at: Instant) {
        p.edit(SearchField::Content, text.to_owned(), at);
    }
    fn launched(p: &mut SearchPanel, at: Instant) -> u64 {
        match p.due(at, 0) {
            Some(Due::Launch { epoch, .. }) => epoch,
            other => panic!("expected a launch, got {other:?}"),
        }
    }
    fn searching(text: &str) -> (SearchPanel, u64, Instant) {
        let t0 = Instant::now();
        let mut p = SearchPanel::new(vp("mem:///r"));
        typed(&mut p, text, t0);
        let e = launched(&mut p, t0 + DEBOUNCE);
        (p, e, t0)
    }

    #[test]
    fn the_pause_fires_once() {
        let t0 = Instant::now();
        let mut p = SearchPanel::new(vp("mem:///r"));
        typed(&mut p, "ag", t0);
        assert!(p.due(t0 + Duration::from_millis(299), 0).is_none());
        launched(&mut p, t0 + DEBOUNCE);
        assert!(p.due(t0 + Duration::from_secs(5), 0).is_none(), "once");
    }

    #[test]
    fn typing_during_the_pause_postpones_it() {
        let t0 = Instant::now();
        let mut p = SearchPanel::new(vp("mem:///r"));
        typed(&mut p, "a", t0);
        typed(&mut p, "ag", t0 + Duration::from_millis(200));
        assert!(p.due(t0 + Duration::from_millis(400), 0).is_none());
        launched(&mut p, t0 + Duration::from_millis(500));
    }

    #[test]
    fn enter_does_not_wait() {
        let t0 = Instant::now();
        let mut p = SearchPanel::new(vp("mem:///r"));
        typed(&mut p, "ag", t0);
        p.force(t0);
        launched(&mut p, t0);
    }

    #[test]
    fn the_request_asks_for_51_lines_and_2000_files() {
        let t0 = Instant::now();
        let mut p = SearchPanel::new(vp("mem:///r"));
        typed(&mut p, "ag", t0);
        let Some(Due::Launch { params, .. }) = p.due(t0 + DEBOUNCE, 0) else {
            panic!("launch")
        };
        assert_eq!(
            (params.max_matches_per_file, params.max_hits),
            (Some(51), Some(2000))
        );
        assert_eq!(params.content.as_deref(), Some("ag"));
        assert_eq!(params.root, vp("mem:///r"));
    }

    /// C10: filters alone do not search as you type.
    #[test]
    fn an_empty_name_and_content_clears_and_launches_nothing() {
        let (mut p, e, t0) = searching("ag");
        assert!(p.apply_hits(e, &batch(&[("mem:///r/a.txt", vec![mi(1)], 0)])));
        p.edit(SearchField::MinSize, "1M".to_owned(), t0);
        typed(&mut p, "", t0);
        assert!(matches!(p.due(t0 + DEBOUNCE, 0), Some(Due::Clear)));
        assert!(p.tree().is_empty());
        assert!(
            !p.apply_hits(e, &batch(&[("mem:///r/b.txt", vec![mi(1)], 0)])),
            "the old run is gone"
        );
    }

    #[test]
    fn a_bad_regex_does_not_launch_and_dims_what_is_there() {
        let (mut p, e, t0) = searching("ag");
        p.apply_hits(e, &batch(&[("mem:///r/a.txt", vec![mi(1)], 0)]));
        p.toggle(crate::search::ID_REGEX, t0);
        typed(&mut p, "(ag", t0);
        assert!(matches!(
            p.due(t0 + DEBOUNCE, 0),
            Some(Due::Unreadable(SearchField::Content))
        ));
        assert!(p.is_dimmed());
        assert_eq!(p.tree()[0].files.len(), 1, "the results stay");
        assert!(
            p.apply_hits(e, &batch(&[("mem:///r/b.txt", vec![mi(1)], 0)])),
            "the running query still lands"
        );
        assert_eq!(
            p.tree()[0].files.len(),
            2,
            "and does not wipe what was there"
        );
        assert!(matches!(
            p.status(),
            PanelStatus::Unreadable(SearchField::Content)
        ));
    }

    /// The previous results stay, dimmed, until the new run answers; then
    /// they are replaced, and a late batch of the old run is dropped.
    #[test]
    fn a_relaunch_replaces_on_first_answer_and_drops_stale_batches() {
        let (mut p, old, t0) = searching("ag");
        p.apply_hits(old, &batch(&[("mem:///r/a.txt", vec![mi(1)], 0)]));
        typed(&mut p, "agu", t0 + DEBOUNCE);
        let new = launched(&mut p, t0 + DEBOUNCE * 2);
        assert!(new > old);
        assert!(p.is_dimmed() && p.tree().len() == 1);
        assert!(!p.apply_hits(old, &batch(&[("mem:///r/z.txt", vec![mi(1)], 0)])));
        assert!(p.apply_hits(new, &batch(&[("mem:///r/b.txt", vec![mi(2)], 0)])));
        assert!(!p.is_dimmed());
        assert_eq!(p.tree()[0].files[0].entry.path, vp("mem:///r/b.txt"));
    }

    /// Folders in the order they first arrive; files sorted by name inside.
    #[test]
    fn grouping_keeps_folder_arrival_and_sorts_files() {
        let (mut p, e, _) = searching("ag");
        p.apply_hits(
            e,
            &batch(&[
                ("mem:///r/z/b.txt", vec![mi(1)], 0),
                ("mem:///r/a/x.txt", vec![mi(1)], 0),
            ]),
        );
        p.apply_hits(e, &batch(&[("mem:///r/z/a.txt", vec![mi(1)], 0)]));
        let dirs: Vec<_> = p.tree().iter().map(|f| f.dir.clone()).collect();
        assert_eq!(dirs, vec![vp("mem:///r/z"), vp("mem:///r/a")]);
        let names: Vec<_> = p.tree()[0]
            .files
            .iter()
            .map(|f| f.entry.path.clone())
            .collect();
        assert_eq!(names, vec![vp("mem:///r/z/a.txt"), vp("mem:///r/z/b.txt")]);
    }

    /// 51 lines arrive: 50 shown, the 51st is behind `+N more`, N counts it
    /// and what the daemon counted, and activating it views the 51st.
    #[test]
    fn fifty_shown_and_the_rest_behind_more() {
        let (mut p, e, _) = searching("ag");
        let lines: Vec<MatchInfo> = (1..=51).map(mi).collect();
        p.apply_hits(e, &batch(&[("mem:///r/a.txt", lines, 9949)]));
        let f = &p.tree()[0].files[0];
        assert_eq!((f.matches.len(), f.more), (50, 9950));
        let rows = p.rows();
        assert_eq!(rows.len(), 1 + 1 + 50 + 1);
        assert!(matches!(rows.last(), Some(Row::More { .. })));
        p.end();
        let Some(Activation::View { target, .. }) = p.activate() else {
            panic!("view")
        };
        assert_eq!(target.line, 51);
    }

    #[test]
    fn folding_hides_a_folder_and_left_climbs_to_it() {
        let (mut p, e, _) = searching("ag");
        p.apply_hits(e, &batch(&[("mem:///r/a/x.txt", vec![mi(1), mi(2)], 0)]));
        p.set_cursor(3); // Folder, File, Line 1, Line 2
        p.fold(false);
        assert_eq!(p.cursor(), 0, "← on a line goes to its folder");
        p.fold(false);
        assert_eq!(p.rows().len(), 1, "← on the folder folds it");
        p.fold(true);
        assert_eq!(p.rows().len(), 4);
    }

    #[test]
    fn activation_by_row_kind() {
        let (mut p, e, _) = searching("ag");
        p.apply_hits(e, &batch(&[("mem:///r/a/x.txt", vec![mi(7)], 0)]));
        p.set_cursor(0);
        assert!(matches!(p.activate(), Some(Activation::Toggled)));
        p.activate(); // unfold again
        p.set_cursor(1);
        assert!(matches!(p.activate(), Some(Activation::GoTo { dir, entry })
            if dir == vp("mem:///r/a") && entry == vp("mem:///r/a/x.txt")));
        p.set_cursor(2);
        let Some(Activation::View { path, target }) = p.activate() else {
            panic!("view")
        };
        assert_eq!(path, vp("mem:///r/a/x.txt"));
        assert_eq!(
            target,
            ViewerTarget {
                line: 7,
                offset: Some(70),
                preview: "l7 aguja".to_owned(),
                ranges: vec![(3, 8)]
            }
        );
    }

    /// C2: a batch in 0.85's shape — `matches` only, no ranges — is shown,
    /// one line per file, unhighlighted, with no `+N` row.
    #[test]
    fn a_085_shaped_batch_shows_one_unhighlighted_line() {
        let (mut p, e, _) = searching("ag");
        let old: SearchHits = serde_json::from_str(
            r#"{"task_id":1,"entries":[{"path":"mem:///r/a.txt","kind":"file"}],"matches":[{"line":3,"preview":"x aguja"}]}"#,
        )
        .expect("0.85 batch");
        p.apply_hits(e, &old);
        let f = &p.tree()[0].files[0];
        assert_eq!((f.matches.len(), f.more), (1, 0));
        assert!(f.matches[0].ranges.is_empty());
        assert_eq!(p.rows().len(), 3, "no +N row");
    }

    /// The precedence is `search_status`'s: cancelled beats the cap.
    #[test]
    fn status_follows_the_shared_precedence() {
        assert!(matches!(
            SearchPanel::new(vp("mem:///r")).status(),
            PanelStatus::Idle
        ));
        let (mut p, e, _) = searching("ag");
        p.apply_hits(
            e,
            &batch(&[
                ("mem:///r/a/x.txt", vec![mi(1)], 0),
                ("mem:///r/b/y.txt", vec![mi(1)], 0),
            ]),
        );
        assert!(matches!(
            p.status(),
            PanelStatus::Run {
                key: "search-panel-running",
                files: 2,
                folders: 2,
                ..
            }
        ));
        p.finished(e, Outcome::Cancelled);
        assert!(matches!(
            p.status(),
            PanelStatus::Run {
                key: "search-panel-cancelled",
                ..
            }
        ));
    }
}
