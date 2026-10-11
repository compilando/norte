# Search, phase B: "Open as listing": implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn search results into a listing you can work in. You open it with **Open as listing** in the search panel (both frontends) or with the TUI's Alt+F7 dialog. It becomes a new tab in the focused pane, titled `🔍 <query> (N)`, with a Folder column. Marks and file operations work there as in any folder. The tab survives navigation elsewhere, `pane.refresh` re-runs it, and session restore re-runs it.

**Architecture:** A shared "result set" lives in `norte-frontend` (`results.rs`). It holds the query that produced the results (`SavedSearch`, serde, for refresh and the session), the run's `Outcome`, the per-hit match context, and the paths a mutation is expected to remove. `PaneState` carries it as `results: Option<ResultSet>`, and its entries stay the ordinary `Vec<Entry>`. Any real listing (`set_listing`, `begin_loading`, `refill`) drops it. A new builtin column `Folder` and sort column `Folder` show and order each hit's parent relative to the search root. The TUI drops its pane takeover (`virtual_search` and friends). Its runs become per-slot (`BySlot<SearchRun>`). The host gains per-slot results runs fed by the same forwarder pattern as its dialog search. The webview gets a button and one new action.

**Tech Stack:** Rust 2024 workspace (`norte-frontend`, `norte-tui` on ratatui, `norte-ui-host`), TypeScript webview (`crates/norte-gui-tauri/ui`, vitest + jsdom), Fluent (`crates/norte-i18n/i18n/{en,es}.ftl`).

**Spec:** `/tmp/claude-1000/-home-oscar-work-wot-projects-high-getnorte-web/46f24dcf-65a3-4fef-97e6-717f92cd1b04/scratchpad/2026-10-10-search-b-listing-design.md`. Task 1's commit copies it to `docs/superpowers/specs/2026-10-10-search-b-listing-design.md` and copies this plan to `docs/superpowers/plans/2026-10-10-search-b-listing.md`, so both travel with the branch. Read both before any task.

**Depends on:** phase A (`feat/search-panel`, plan `docs/superpowers/plans/2026-10-09-search-panel-a.md`) merged into `main`. This plan uses its names: `norte_frontend::search_panel::{SearchPanel, DEBOUNCE, MAX_FILES, ASKED_PER_FILE}`, `SearchForm::regex_unreadable`, the TUI's `crate::searchpanel::{PanelRun, on_key, tick, drain, cancel}` and `work.search_panel`, and the host's `controller/searchpanel.rs` (`PanelState`, `PanelRunHost`, `Background::SearchPanelHits/SearchPanelLive/SearchPanelBroken`, `SlotView::Search`, `SearchPanelSlotView`). Before Task 1, check each name exists on `main` with `rg`. If phase A renamed one, use the merged name everywhere it appears here.

**Repository:** `/home/oscar/work/wot/projects/high/norte`. Work on branch `feat/search-listing` from `main` after phase A is merged, in its own worktree (`scripts/wt.sh search-listing`).

## Global Constraints

- Toolchain loop: `just t <crate>` (nextest; never bare `cargo test` for the suite). Run `just c` when lint surface was touched. Use `cargo check -p <crate> --all-targets` for "does it compile". `just t` does NOT run doctests: after adding a field to any public struct, run `cargo test --workspace --exclude norte-gui-tauri --doc`. After writing an intra-doc link, run `cargo doc -p <crate> --no-deps`.
- Gate budget: run `just ci-fast` ONCE after Task 4 and ONCE after Task 8. Run `just gui-ci` once after Task 9, because the webview was touched. Pushing `main` IS the gate. Never use the gate as a debugger.
- A hook blocks editing through Bash: use Edit/Write. Use `rg`, not `grep`. Commit messages contain `<…>` (the trailer), so always use `git commit -F <file>`. Read `git diff --cached --stat` before every commit.
- Every commit uses Conventional Commits in English and ends with the line `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Hard rules:
  - Filenames are bytes: use `VPath` and `Segment`, and `display_name`/`display_name_with`/`path_display` only for display.
  - No blocking I/O in async code.
  - Every long operation is a cancellable task.
  - No `unwrap()`/`expect()` outside tests without an invariant comment.
  - Errors are typed.
  - Every user-facing string goes through Fluent, in BOTH `en.ftl` and `es.ftl`.
- Protocol (`norte-proto`): untouched. `PROTOCOL_VERSION` does not change.
- Bridge: ONE bump for the whole plan, in Task 7. Set `BRIDGE_VERSION` in `crates/norte-ui-host/src/bridge.rs` to **the next free number at execution time**: at least 111 once phase A has merged; read the constant. Add its `/// - **N**:` doc entry, and mirror the number ONLY in `crates/norte-gui-tauri/ui/src/types.ts` (the `BRIDGE_VERSION` constant near the top). Re-bless the host goldens with `NORTE_BLESS=1 just t norte-ui-host`.
- Numbers, fixed by this plan:
  - `results::MAX_HITS` = **10 000**, the TUI's existing `SEARCH_MAX_HITS`, which it replaces.
  - Tab label: at most **32** chars before `…`.
  - The panel's own cap stays `search_panel::MAX_FILES` (2000). A tab opened from the panel inherits that cap until its first refresh, which uses `MAX_HITS`.
- Session: `SlotState.results` is additive: `#[serde(default, skip_serializing_if = "Option::is_none")]`. The session envelope version does not change.
- "One explanation, one home": the decisions go in the rustdoc of `norte_frontend::results`, the user-visible change in `CHANGELOG.md` `[Unreleased]`, and each test's doc says what it pins.
- Keys: the only new key is **F9 inside the search panel**, a fixed key like the panel's F2–F8, so no preset changes. It is documented in the help topic `finding` in both locales (Task 10).

## Contradictions between the spec and the code (flagged, with the adjustment this plan takes)

- **C1: the spec's `Source::Results { query_label, entries: Vec<VPath> }` would duplicate the listing.** Hits arrive as `Entry` (size, mtime, kind) and `PaneState` already holds them, sorts them and marks them. A second `Vec<VPath>` is a second list to keep in step. **Adjustment:** `ResultSet` holds the query (`SavedSearch`), the outcome, the match context and the expected removals. The entries are the pane's.
- **C2: "`F5`-refresh re-runs the same search".** `F5` is `pane.copy` in the orthodox presets. Refreshing is `pane.refresh` (`Command::PaneRefresh` in the TUI, `Effect::Refresh` in the host). **Adjustment:** `pane.refresh` re-runs the search in a results tab.
- **C3: "name collisions across folders go through the normal collision policy" vs the host's #268 refusal.** `transfer_operands` refuses a WHOLE batch when two marks fold to one name on a folding destination (`two_marks_fold_the_same`, `host-batch-folds-to-one`). It ignores exact duplicates on a non-folding destination, which go through as two jobs. **Adjustment:** both behaviours are kept and pinned:
  - exact duplicates (`a/x.txt`, `b/x.txt`) go through: the second job meets the first's file and gets the normal policy (TUI: the collision dialog queue; window: `CollisionPolicy::Fail`, a visible failed row);
  - fold pairs on a folding destination keep #268's refusal.
- **C4: `transfer_operands` refuses `source_dir == dest` (`host-same-directory`).** In a results slot, `source_dir` is the search ROOT. Copying hits from subfolders into the root, which is the most common case, would be refused. **Adjustment:** a shared `results::transfer_refusal(paths, dest)` refuses only when SOME marked path already lives in `dest` (its parent equals `dest`, byte for byte, the same leniency as today's check). Both frontends use it for a results-tab source, and the old check stays for folder tabs.
- **C5: copying INTO a results tab copies into the search root today.** The TUI's `open_transfer` uses `panes[to].dir()`, and the host's `directory_dest` uses `pane.dir()`. Results are not a location. **Adjustment:** a results tab as DESTINATION refuses with `msg-pane-not-a-location` (TUI) or `host-results-not-a-location` (window).
- **C6: "pane location gestures refuse … as the TUI's virtual mode does today".** The window has none of those refusals. **Adjustment:** Task 8 adds them to the host for every effect that writes at `pane.dir()`, listed there.
- **C7: "the tab is restored as its query (re-run on restore)" vs "what is not seen is not fetched".** Both frontends list only VISIBLE slots at start-up. A results tab has no other content: left cold, it would show `🔍 x (0)` and claim the search found nothing. **Adjustment:** every restored results tab re-runs at start-up, visible or not.
- **C8: the window's Alt+F7 dialog** (`controller/search.rs`, its own result overlay) is not mentioned by the spec. **Adjustment:** unchanged. Only the TUI's dialog stops taking over the pane, because only the TUI had a takeover.
- **C9: "the rest of the run if it is still streaming" when the panel's query changes afterwards.** Phase A's panel cancels its run on the next query. **Adjustment:** opening the listing HANDS the live run to the tab. The panel watches along while it still shows that query; its next query launches a new run and no longer cancels the tab's.
- **C10: what Enter does in a results tab** is not in the spec. Today's takeover `cd`s the pane to the hit's parent and loses the results. **Adjustment:** Enter on a hit opens its FOLDER in a NEW tab with the cursor on it. The results tab stays. Esc on a running results tab cancels the run. Any other navigation inside the tab (typed path, history, places) turns it into an ordinary folder tab, as a `cd` ended the takeover before.
- **C11: "moved/deleted entries leave the tab".** No frontend re-lists a results tab (it has no directory). **Adjustment:** when a move, delete or rename is SUBMITTED from a results tab, its source paths are recorded (`ResultSet::expect`). When the frontend's normal "a mutation finished" refresh fires, the tab stats only those paths: `NotFound` removes the hit. A renamed hit leaves too, because it may no longer match the query.

## Review Focus

1. **A results tab whose run is still streaming when the tab is closed, or navigated to a folder.** The run must be cancelled in the daemon, no batch may land in the folder listing that replaced it, and the panel that handed the run over must not resurrect it. Pinned by tests in Task 4 (TUI) and Task 7 (host).
2. **Copying hits into the search root, and copying with a results tab as the destination.** The first must go through, with only the hits already in the root refused. The second must refuse instead of writing into the root. Pinned by tests in Task 4 (TUI) and Task 8 (host).
3. **Two hits with the same name copied to one folder.** The first lands, and the second goes through the collision policy without silently overwriting. Pinned by tests in Task 4 (TUI) and Task 8 (host).
4. **A hostile query or folder name** (ESC, RLO, invalid UTF-8 in a folder segment). The tab title, the location line and the Folder cell are masked and marked hostile, and the 🔍 title fits the tab strip's measured width (it is two cells wide) so clicks hit the right tab. Pinned by tests in Task 1 (masking), Task 4 (TUI strip zones) and Task 7 (host).
5. **Session restore with a results tab behind another tab and a root that no longer exists.** It re-runs, fails with a visible `search-status-failed`, and does not turn into a listing of something else. Pinned by tests in Task 5 (TUI) and Task 8 (host).

---

## File Structure

| file | responsibility | task |
| --- | --- | --- |
| `crates/norte-frontend/src/results.rs` (new) | `SavedSearch`, `ResultSet`, `MAX_HITS`, `tab_title`, `location`, `status`, `folder_text`, `transfer_refusal` | 1 |
| `crates/norte-frontend/src/pane/mod.rs`, `pane/tests.rs` | `PaneState.results`, `begin_results`, `forget`, `folder_text`; real listings drop results; hidden flag restored | 1 |
| `crates/norte-frontend/src/view/columns.rs`, `sort.rs` | `Builtin::Folder`, `SortColumn::Folder`, `folder: bool` in `fitted_columns`/`column_widths` | 2 |
| `crates/norte-frontend/src/session.rs` | `SlotState.results: Option<SavedSearch>` | 2 |
| `crates/norte-frontend/src/search_panel.rs` | `SearchPanel::listing() -> Option<PanelListing>` | 6 |
| `crates/norte-tui/src/app/pane.rs` (+ the ~40 `virtual_search` sites) | the TUI pane over the shared result set; `virtual_search`/`search_state`/`search_error`/`search_matches`/`show_hidden_pref` removed | 3 |
| `crates/norte-tui/src/jobs/search.rs`, `jobs/inflight.rs`, `event_loop.rs`, `navigate.rs`, `refresh.rs`, `keys.rs`, `mouse.rs`, `turn.rs`, `jobs/compare.rs`, `app/focus.rs`, `app/ops.rs`, `ui/geometry.rs`, `ui/pane.rs`, `ui/status.rs` | the results TAB, per-slot runs, Enter/Esc, transfer refusals | 4 |
| `crates/norte-tui/src/dispatch.rs`, `refresh.rs`, `mutations.rs`, `app/session.rs`, `event_loop.rs` | refresh re-runs, the recheck after mutations, session | 5 |
| `crates/norte-tui/src/searchpanel.rs` | F9: open as listing, run hand-over | 6 |
| `crates/norte-ui-host/src/controller/results.rs` (new), `controller/{mod.rs,extensions.rs,searchpanel.rs,panel.rs,patches.rs,layout.rs,nav.rs,tabs.rs,tasks.rs}`, `dto.rs`, `action.rs`, `bridge.rs`, `tests/golden.rs`; `ui/src/types.ts` | results slots in the window, the action, bridge bump | 7 |
| `crates/norte-ui-host/src/controller/{effects.rs,fileops.rs,transfer.rs,gestures.rs,listing.rs,session.rs,mod.rs}` | refusals, transfers, refresh, recheck, session | 8 |
| `crates/norte-gui-tauri/ui/src/render/searchpanel.ts`, `render.ts`, `session.ts`, `tests/render.test.ts` | button, results note, Folder column | 9 |
| `crates/norte-help/topics/{en,es}/finding.md`, `crates/norte-help/tests/corpus.rs`, `CHANGELOG.md`, `crates/norte-cli/tests/goldens/help-en.json` | docs | 10 |

Task order: 1 → 2 (shared model, both frontends compile) → 3 → 4 → 5 → 6 (TUI) → 7 → 8 → 9 (window) → 10 (docs).

---

### Task 1: The shared result set

**Files:**
- Create: `crates/norte-frontend/src/results.rs` (+ `pub mod results;` in `crates/norte-frontend/src/lib.rs`, alphabetical, next to `pub mod search;`)
- Modify: `crates/norte-frontend/src/pane/mod.rs` (field `results`, the methods below, the drop in `set_listing`/`begin_loading`/`refill`)
- Test: `crates/norte-frontend/src/results.rs` (`mod tests`), `crates/norte-frontend/src/pane/tests.rs`
- Modify: `crates/norte-i18n/i18n/en.ftl`, `es.ftl` (the keys in Step 3)
- Also: copy the spec to `docs/superpowers/specs/2026-10-10-search-b-listing-design.md` and this plan to `docs/superpowers/plans/2026-10-10-search-b-listing.md`

**Interfaces:**
- Consumes: `norte_frontend::search::{SearchForm, SearchKinds, params}`, `norte_frontend::search_status::{Outcome, status_key}`, `norte_frontend::{display_name, display_name_with, path_display}`, `norte_proto::methods::{FsSearchParams, MatchInfo, SearchHits}`.
- Produces (every later task uses these exact names):

```rust
// crates/norte-frontend/src/results.rs
pub const MAX_HITS: u32 = 10_000;
pub const LABEL_MAX_CHARS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedKinds { #[default] All, Files, Folders }

/// The query a results tab shows: what refresh and the session re-run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSearch {
    pub root: VPath,
    #[serde(default)] pub name: String,
    #[serde(default)] pub content: String,
    #[serde(default)] pub exclude: String,
    #[serde(default)] pub min_size: String,
    #[serde(default)] pub max_size: String,
    #[serde(default)] pub days: String,
    #[serde(default)] pub encoding: String,
    #[serde(default)] pub regex: bool,
    #[serde(default)] pub case: bool,
    #[serde(default)] pub whole_word: bool,
    #[serde(default = "yes")] pub recursive: bool,
    #[serde(default)] pub kinds: SavedKinds,
}
impl SavedSearch {
    pub fn from_form(form: &SearchForm, root: VPath) -> Self;
    pub fn form(&self) -> SearchForm;                      // field = Name
    pub fn params(&self, now_ms: i64) -> FsSearchParams;  // search::params(.., MAX_HITS)
    pub fn label(&self) -> (String, bool);                 // masked + clamped, hostile flag
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResultSet { /* search, outcome, cap, received, matches, expected */ }
impl ResultSet {
    pub fn new(search: SavedSearch, cap: u32) -> Self;     // Outcome::Running
    pub fn search(&self) -> &SavedSearch;
    pub fn root(&self) -> &VPath;
    pub fn outcome(&self) -> &Outcome;
    pub fn set_outcome(&mut self, outcome: Outcome);
    pub fn is_running(&self) -> bool;
    pub fn restart(&mut self, cap: u32);                   // Running, counters/matches/expected cleared
    pub fn note_hits(&mut self, hits: &SearchHits);        // counts, keeps the FIRST MatchInfo per path
    pub fn seed_matches(&mut self, matches: Vec<(VPath, MatchInfo)>);
    pub fn match_of(&self, path: &VPath) -> Option<&MatchInfo>;
    pub fn at_cap(&self) -> bool;                          // received >= cap
    pub fn expect(&mut self, paths: impl IntoIterator<Item = VPath>);
    pub fn expected(&self) -> Vec<VPath>;
    pub fn settle(&mut self, path: &VPath);                // stop expecting it
}
pub fn tab_title(set: &ResultSet, n: usize, lang: Lang) -> (String, bool);   // "🔍 <label> (n)"
pub fn location(set: &ResultSet, lang: Lang) -> (String, bool);              // "search “<label>” in <root>"
pub fn status(set: &ResultSet, n: usize, lang: Lang) -> String;              // search-status-*
pub fn folder_text(entry: &Entry, root: &VPath, enc: Option<NameEncoding>) -> String;
pub fn transfer_refusal(paths: &[VPath], dest: &VPath) -> Option<&'static str>; // "results-already-there"

// crates/norte-frontend/src/pane/mod.rs (impl PaneState)
pub fn begin_results(&mut self, set: ResultSet, entries: Vec<Entry>);
pub fn results(&self) -> Option<&ResultSet>;
pub fn results_mut(&mut self) -> Option<&mut ResultSet>;
pub fn is_results(&self) -> bool;
pub fn forget(&mut self, paths: &[VPath]) -> usize;    // how many hits left the tab
pub fn folder_text(&self, entry: &Entry) -> Option<String>;  // None outside a results tab
```

- [ ] **Step 1: Write the failing tests** at the bottom of `results.rs`. RED is a compile error until Step 3.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::{Entry, EntryKind, VPath};

    fn vp(s: &str) -> VPath {
        VPath::parse(s).expect("test wire")
    }
    fn file(p: &str) -> Entry {
        Entry { path: vp(p), kind: EntryKind::File, size: Some(1), mtime_ms: None, attrs: Default::default() }
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
        let back: SavedSearch = serde_json::from_value(serde_json::to_value(&s).expect("ser")).expect("de");
        assert_eq!(back, s);
        let now = 30 * 86_400_000;
        let p = back.params(now);
        assert_eq!(p, crate::search::params(&s.form(), vp("mem:///casa"), now, MAX_HITS));
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

    /// The label: content wins over the name; both are masked; long ones
    /// are cut at LABEL_MAX_CHARS with `…`.
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
        set.note_hits(&SearchHits { entries: vec![file("mem:///casa/a"), file("mem:///casa/b")], ..Default::default() });
        assert!(set.at_cap());
        set.set_outcome(Outcome::Cancelled);
        assert!(status(&set, 2, lang).contains("cancelled"));
    }

    /// Folder relative to the root; a hit IN the root says `.`; a hostile
    /// segment is masked.
    #[test]
    fn folder_text_is_relative_and_masked() {
        let root = vp("mem:///casa");
        assert_eq!(folder_text(&file("mem:///casa/src/x/a.rs"), &root, None), "src/x");
        assert_eq!(folder_text(&file("mem:///casa/a.rs"), &root, None), ".");
        let bad = Entry { path: root.join(norte_proto::Segment::new(b"d\x1b".to_vec()).expect("seg"))
            .join(norte_proto::Segment::new(b"a".to_vec()).expect("seg")), ..file("mem:///casa/a") };
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
        assert_eq!(transfer_refusal(&mixed, &root), Some("results-already-there"));
    }
}
```

`SearchHits` may not be `Default`. If it is not, build it with its full literal; phase A added `all_matches`/`more`.

In `pane/tests.rs`:

```rust
/// A results tab: hidden entries shown (a hit is explicit), no `..` row,
/// and the first real listing drops the result set and gives back the
/// hidden preference.
#[test]
fn a_real_listing_ends_a_results_tab_and_restores_hidden() {
    let mut p = PaneState::new(vp("mem:///casa"), vec![]);
    p.set_show_hidden(false);
    p.set_parent_row(true);
    let set = ResultSet::new(saved_search("mem:///casa"), MAX_HITS);
    p.begin_results(set, vec![file("mem:///casa/sub/.env")]);
    assert!(p.is_results() && p.show_hidden());
    assert!(!(0..p.entries().len()).any(|i| p.is_parent_row(i)), "no `..` in results");
    assert_eq!(p.entries().len(), 1);
    p.set_listing(vp("mem:///casa"), vec![]);
    assert!(!p.is_results());
    assert!(!p.show_hidden(), "the preference comes back");
}

/// `forget` takes hits and their marks out and keeps the cursor on the
/// next row (orthodox), without ending the result set.
#[test]
fn forget_removes_hits_and_marks_but_keeps_results() {
    let mut p = PaneState::new(vp("mem:///casa"), vec![]);
    p.begin_results(ResultSet::new(saved_search("mem:///casa"), MAX_HITS),
        vec![file("mem:///casa/a/1"), file("mem:///casa/b/2"), file("mem:///casa/c/3")]);
    p.set_cursor(1);
    p.toggle_mark_at_cursor();
    assert_eq!(p.forget(&[vp("mem:///casa/b/2"), vp("mem:///casa/zz")]), 1);
    assert!(p.is_results());
    assert_eq!(p.marks_len(), 0);
    assert_eq!(p.selected().map(|e| e.path.clone()), Some(vp("mem:///casa/c/3")));
}

/// Sorting by Folder survives inside results and resets to the default
/// order when the tab becomes a folder (Folder means nothing there).
#[test]
fn leaving_results_resets_a_folder_sort() { /* written in Task 2, after SortColumn::Folder exists */ }
```

Use this file's existing helpers (`vp`, `file`), and name the mark toggle after what `PaneState` exposes (`rg -n "pub fn (toggle_mark|mark)" crates/norte-frontend/src/pane`). `saved_search(root)` is a three-line helper in the test file: `SavedSearch::from_form(&SearchForm::new(), vp(root))`. Leave `leaving_results_resets_a_folder_sort` OUT of this task; Task 2 writes it.

- [ ] **Step 2: Run** `just t norte-frontend`. Expected: FAIL (compile errors: module `results` missing).
- [ ] **Step 3: Implement.**

`results.rs`: start the module rustdoc with the design in five lines: what a result set is, C1, C10, C11, and that refresh and the session re-run the `SavedSearch`. The constructors and accessors are mechanical. The bodies that matter:

```rust
impl SavedSearch {
    pub fn label(&self) -> (String, bool) {
        let raw = match (self.name.trim().is_empty(), self.content.is_empty()) {
            (false, false) => format!("{} · {}", self.name.trim(), self.content),
            (true, false) => self.content.clone(),
            (false, true) => self.name.trim().to_owned(),
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

pub fn folder_text(entry: &Entry, root: &VPath, enc: Option<NameEncoding>) -> String {
    let Some(parent) = entry.path.parent() else { return ".".to_owned() };
    let skip = root.segments().count();
    let parts: Vec<String> = parent
        .segments()
        .skip(skip)
        .map(|s| crate::display_name_with(s, enc).0)
        .collect();
    if parts.is_empty() { ".".to_owned() } else { parts.join("/") }
}

pub fn transfer_refusal(paths: &[VPath], dest: &VPath) -> Option<&'static str> {
    paths
        .iter()
        .any(|p| p.parent().as_ref() == Some(dest))
        .then_some("results-already-there")
}
```

Use `status_key(&self.outcome, self.at_cap())` in `status`. `Failed(category)` takes `("error", category)` and the rest take `("n", n)`, exactly as `controller/search.rs` `search_state` does today. `note_hits` keeps the FIRST `MatchInfo` per path (`entry().or_insert`) and adds `entries.len()` to `received`.

`PaneState`:
- add `results: Option<ResultSet>` and `hidden_before_results: Option<bool>`;
- `begin_results(set, entries)`:
  - save `self.show_hidden()` into `hidden_before_results`, but only if it is `None`, so a refresh of a results tab does not overwrite the saved preference;
  - `set_show_hidden(true)`;
  - call the internal listing replacement with `set.root().clone()` and `entries`;
  - then `self.results = Some(set)`.
- The `..` row: while `results.is_some()`, nothing inserts it. Find where `ParentRow` inserts the synthetic entry (`rg -n "ParentRow" crates/norte-frontend/src/pane/mod.rs`) and guard it with `self.results.is_none()`.
- `set_listing`, `begin_loading` and `refill` call a private `end_results()`. It takes `results`, restores `hidden_before_results` with `set_show_hidden`, and resets the sort to `SortSpec::default()` if its column is `Folder` (that part lands in Task 2).
- `forget(paths)`: take `results` out, rebuild with the remaining REAL entries through `refill`'s machinery, which bumps the epoch, prunes marks, re-applies quick search and keeps the cursor by index. Put `results` back without touching the hidden flag. Return how many left. Implement it as a private `replace_entries_keeping(&mut self, entries)` shared by `refill` and `forget`, so the "drop results" part of `refill` stays in the public method only.
- `folder_text(entry)`: `self.results.as_ref().map(|r| results::folder_text(entry, r.root(), self.name_encoding))`.

Fluent, both locales. Do NOT add `col-header-folder` here; Task 2 adds it.

```ftl
# en.ftl
results-tab-title = 🔍 { $query } ({ $n })
results-location = search “{ $query }” in { $root }
results-already-there = some of these files are already in that folder
results-not-a-location = search results are not a folder: choose a folder as the destination
```
```ftl
# es.ftl
results-tab-title = 🔍 { $query } ({ $n })
results-location = búsqueda «{ $query }» en { $root }
results-already-there = algunos de estos ficheros ya están en esa carpeta
results-not-a-location = los resultados de una búsqueda no son una carpeta: elige una carpeta como destino
```

- [ ] **Step 4: Run** `just t norte-frontend`, then `cargo test -p norte-frontend --doc` (public items have doc examples where the crate requires them), then `just c`. Expected: PASS.
- [ ] **Step 5: Commit** with message `feat(frontend): a result set a listing can carry` and body "the shared model of phase B; C1 and C11 of the plan". Include the spec and plan copies.

---

### Task 2: The Folder column, the Folder sort, and the session field

**Files:**
- Modify: `crates/norte-frontend/src/view/columns.rs`:
  - `Builtin::Folder` and its `FromStr`/`Display` (`"folder"`);
  - its arms in `builtin_layout_item`, `sort_column`, `styled_cell_in` (returns `None`, like `Name`), `header_label_in` (`col-header-folder`), `format_name`/`next_format`/`format_fits` (no format, like `Name`/`Kind`), and the `columns_picker` builtin list (not offered);
  - `layout_items_for` (a configured `"folder"` is skipped: not configurable);
  - `paintable_items`/`column_widths`/`fitted_columns` gain `folder: bool`.
- Modify: `crates/norte-frontend/src/sort.rs` (`SortColumn::Folder` and its arm in `cmp_keyed_with`)
- Modify: `crates/norte-frontend/src/pane/mod.rs` (`end_results` resets a Folder sort)
- Modify: `crates/norte-frontend/src/session.rs` (`SlotState.results`)
- Modify, the callers so the workspace stays green: `crates/norte-tui/src/ui/pane.rs` `pane_columns` (pass `pane.state().is_results()`), `crates/norte-tui/src/mouse.rs` (`header_under`/`column_border_at`'s `column_widths` call, same flag), `crates/norte-tui/src/ui/pickers.rs` (`Builtin::Folder` arm), `crates/norte-ui-host/src/controller/panel.rs` `setting_of` (both `fitted_columns` calls get `target_slot.pane.is_results()`). The `SlotState { .. }` literals get `results: None`: count them first with `rg -c "SlotState \{" --glob '*.rs' crates`. Expect about 12, in `norte-frontend/src/session.rs`, `norte-tui/src/{app/session.rs,session_push.rs}`, `norte-tui/tests/session.rs`, `norte-ui-host/src/controller/{session.rs,nav.rs}` (check: `nav.rs` may construct the dto `SlotState`, which is a different type) and `norte-ui-host/tests/controller/{base.rs,gestures.rs}`.
- Modify: `en.ftl`/`es.ftl` (`col-header-folder = Folder` / `col-header-folder = Carpeta`)
- Test: `columns.rs` and `sort.rs` `mod tests`, `pane/tests.rs`, `session.rs` `mod tests`

**Interfaces:**
- Consumes: Task 1.
- Produces:

```rust
pub enum Builtin { Name, Size, Mtime, Kind, Folder }
pub enum SortColumn { Name, Size, Mtime, Extension, Attr(String), Folder }   // serde "folder"
pub fn column_widths(settings: &ColumnsSettings, scheme: &str, folder: bool, inner_width: u16,
                     catalog: Option<&norte_proto::AttrCatalog>) -> Vec<(ColumnId, u16)>;
pub fn fitted_columns(settings: &ColumnsSettings, scheme: &str, folder: bool, inner_width: u16,
                      name_wanted: u16, catalog: Option<&norte_proto::AttrCatalog>) -> Vec<Fitted>;
// session.rs
pub struct SlotState { /* … */ #[serde(default, skip_serializing_if = "Option::is_none")] pub results: Option<crate::results::SavedSearch> }
```

- [ ] **Step 1: Failing tests.**

```rust
// columns.rs
/// In a results tab the Folder column goes right after the name, and the
/// ladder never hides it: it is the one column that says where a hit is.
#[test]
fn folder_goes_after_the_name_and_never_yields() {
    let s = ColumnsSettings::default();
    let wide = fitted_columns(&s, "file", true, 120, 20, None);
    assert_eq!(wide[1].id, ColumnId::Builtin(Builtin::Folder));
    let narrow = fitted_columns(&s, "file", true, 40, 30, None);
    assert!(narrow.iter().any(|f| f.id == ColumnId::Builtin(Builtin::Folder)));
    let folder_tab = fitted_columns(&s, "file", false, 120, 20, None);
    assert!(folder_tab.iter().all(|f| f.id != ColumnId::Builtin(Builtin::Folder)));
}

/// `"folder"` round-trips as an id (it crosses the bridge as one) but a
/// configured `"folder"` paints nothing in a folder tab.
#[test]
fn folder_is_an_id_but_not_configurable() {
    assert_eq!("folder".parse::<ColumnId>(), Ok(ColumnId::Builtin(Builtin::Folder)));
    assert_eq!(ColumnId::Builtin(Builtin::Folder).to_string(), "folder");
    let cfg = norte_config::ColumnsConfig { default_columns: Some(vec!["name".into(), "folder".into()]), ..Default::default() };
    let s = ColumnsSettings::resolve(&cfg);
    assert!(s.layout_items_for("file").iter().all(|(id, _)| *id != ColumnId::Builtin(Builtin::Folder)));
    assert_eq!(sort_column(Builtin::Folder), Some(crate::sort::SortColumn::Folder));
}

// sort.rs
/// By folder: parents compared segment by segment (bytes); ties by name;
/// `Desc` reverses the folders, never the name tie-break.
#[test]
fn sorting_by_folder_groups_by_parent() {
    let mut v = vec![file("mem:///r/b/z"), file("mem:///r/a/y"), file("mem:///r/b/a"), file("mem:///r/a.d/x")];
    sort_entries_with(&mut v, &SortSpec { column: SortColumn::Folder, dir: SortDir::Asc, dirs_first: false });
    let names: Vec<_> = v.iter().map(|e| e.path.to_wire()).collect();
    assert_eq!(names, ["mem:///r/a/y", "mem:///r/a.d/x", "mem:///r/b/a", "mem:///r/b/z"]);
}

// pane/tests.rs (the test Task 1 left out)
#[test]
fn leaving_results_resets_a_folder_sort() {
    let mut p = PaneState::new(vp("mem:///casa"), vec![]);
    p.begin_results(ResultSet::new(saved_search("mem:///casa"), MAX_HITS), vec![]);
    p.set_sort(SortSpec { column: SortColumn::Folder, ..SortSpec::default() });
    p.set_listing(vp("mem:///casa"), vec![]);
    assert_eq!(p.sort(), SortSpec::default());
}

// session.rs
/// A results slot saves its query; an old body without the field still
/// reads.
#[test]
fn a_slot_carries_its_saved_search() {
    let mut s = slot("mem:///casa");
    s.results = Some(crate::results::SavedSearch::from_form(&crate::search::SearchForm::new(), vp("mem:///casa")));
    let v = serde_json::to_value(&s).expect("ser");
    assert_eq!(serde_json::from_value::<SlotState>(v).expect("de"), s);
    let old: SlotState = serde_json::from_str(r#"{"path":"mem:///casa"}"#).expect("old body");
    assert!(old.results.is_none());
}
```

Check the order expectation in `sorting_by_folder_groups_by_parent` against a byte comparison of segment lists: `[a] < [a.d] < [b]`, because segment `a` is a prefix of `a.d`. If `dirs_first` or NFC keys change it, adjust the fixture, not the rule.

- [ ] **Step 2: Run** `just t norte-frontend`. Expected: FAIL.
- [ ] **Step 3: Implement.**
  - In `paintable_items`: when `folder` is true, insert `(ColumnId::Builtin(Builtin::Folder), builtin_layout_item(Builtin::Folder))` right after the name. `Folder`'s item is `Flex { min: 8, weight: 1 }`, `is_name: false`.
  - `LADDER` is untouched, because Folder is not in it.
  - `cmp_keyed_with`'s arm: `SortColumn::Folder => cmp_folder(a.1, b.1, spec.dir)`, where a missing parent (a root) sorts last in both directions, like the other columns. `cmp_folder` compares `parent.segments()` with `Iterator::cmp` and reverses on `Desc`.
  - Every exhaustive `match` on `Builtin`/`SortColumn` that the compiler names gets its arm: `Folder` goes with `Name` wherever "no format/no style" applies.
  - In the host's `sort_by_column`, a header click on `"folder"` parses through `sort_column_id`. Nothing to add there; the test in Task 7 pins it.
  - The callers pass the flag, and the TUI cell painter (`ui/pane.rs` ~852) gets an arm:

```rust
norte_frontend::columns::ColumnId::Builtin(norte_frontend::columns::Builtin::Folder) => pane
    .state()
    .folder_text(entry)
    .unwrap_or_default(),
```

  The host's `cells()` in `controller/patches.rs` gets the same arm: `target_slot.pane.folder_text(e)`. Both stay invisible until a results tab exists (Tasks 4 and 7).
- [ ] **Step 4: Run** `just t norte-frontend`, `just t norte-tui`, `just t norte-ui-host` (snapshots must not change: no folder tab paints Folder), `cargo test --workspace --exclude norte-gui-tauri --doc` (a public struct gained a field), and `just c`.
- [ ] **Step 5: Commit** with message `feat(frontend): a Folder column and sort for result sets` and body "not configurable: only a results tab paints it; the session slot can carry its query".

---

### Task 3: The TUI pane rides on the shared result set

A behaviour-preserving refactor: the Alt+F7 takeover still happens, but its state lives in `PaneState.results`. After it, `virtual_search`, `search_state`, `search_error`, `search_matches` and `show_hidden_pref` no longer exist on `Pane`, and `SearchState` is replaced by `Outcome`.

**Files:**
- Modify: `crates/norte-tui/src/app/pane.rs`:
  - `begin_search(root)` becomes `begin_results(set: ResultSet)`, a delegate;
  - `set_listing`/`begin_listing`/`refresh_listing` lose their manual resets, because `PaneState` does them;
  - `toggle_hidden` loses the `virtual_search` branch, because the preference now lives in `PaneState`;
  - new delegates: `is_results()`, `results()`, `results_mut()`, `forget()`.
- Modify: every site listed by `rg -n "virtual_search|search_matches|search_state|search_error|SearchState" crates/norte-tui/src crates/norte-tui/tests`. Each becomes `is_results()`, `results().and_then(|r| r.match_of(path))`, or `results().map(ResultSet::outcome)`:
  - `navigate.rs`, `fill.rs`, `refresh.rs`, `event_loop.rs` (`watch_targets`), `gestures.rs`, `dispatch.rs` (the four refusals), `keys.rs`, `jobs/ai.rs`, `jobs/search.rs`, `app/compare.rs`, `app.rs` tests, `ui/status.rs`;
  - tests in `tests/{app.rs,render.rs,swap.rs,snapshots_ui.rs}`.
- Modify: `crates/norte-tui/src/ui/status.rs`: the results branch paints `norte_frontend::results::status(set, n, lang)` plus today's `[:line preview]` from `set.match_of(&entry.path)`.
- Modify: `crates/norte-tui/src/jobs/search.rs`:
  - `SearchRun.state: SearchState` becomes `outcome: Outcome`;
  - `finalize_search_state` becomes `finalize_outcome(&SearchRun) -> Outcome`, using `search_status::outcome_of` with `error_category`;
  - `drain_search` feeds `results_mut().note_hits(&batch)` and then `extend_listing(batch.entries)`;
  - `SEARCH_MAX_HITS` is replaced by `norte_frontend::results::MAX_HITS`.
- Test: the existing tests, rewritten to the new names. No new behaviour.

**Interfaces:**
- Consumes: Tasks 1–2.
- Produces: `Pane::begin_results(&mut self, set: norte_frontend::results::ResultSet)`, `Pane::is_results(&self) -> bool`, `Pane::results(&self) -> Option<&ResultSet>`, `Pane::results_mut(&mut self) -> Option<&mut ResultSet>`, `Pane::forget(&mut self, paths: &[VPath]) -> usize`. `jobs::search::SearchRun { task, rx, pane, prev_dir, hits, outcome }`; Task 4 rekeys it by slot.

- [ ] **Step 1: Rewrite the tests that name the old fields.** `begin_search_clears_previous_previews` (`tests/app.rs` ~200) becomes:

```rust
/// #81 (review MAJOR-4): re-launching the search with NO cd in between
/// does not drag previews from the earlier one.
#[test]
fn a_new_result_set_starts_without_previews() {
    let mut p = pane_with(&[(b"a.rs", EntryKind::File)]);
    let root = vp("file:///casa");
    let saved = norte_frontend::results::SavedSearch::from_form(&norte_frontend::search::SearchForm::new(), root.clone());
    p.begin_results(norte_frontend::results::ResultSet::new(saved.clone(), norte_frontend::results::MAX_HITS));
    p.results_mut().expect("results").seed_matches(vec![(root.join(seg(b"a.rs")), match_info(7, "vieja"))]);
    p.begin_results(norte_frontend::results::ResultSet::new(saved, norte_frontend::results::MAX_HITS));
    assert!(p.results().expect("results").match_of(&root.join(seg(b"a.rs"))).is_none());
}
```

Replace `swap.rs`'s `SearchState::Running` with `outcome: Outcome::Running`. The snapshot `snapshot_search_pane_virtual` keeps its name and its EXPECTED output, which must not change in this task. It now calls `begin_results`.
- [ ] **Step 2: Run** `just t norte-tui`. Expected: FAIL (compile errors in tests).
- [ ] **Step 3: Implement** the site-by-site replacement. `Pane::begin_results` is a pure delegate to `self.state.begin_results(set, Vec::new())`. The hidden-flag dance is the shared crate's now: delete `show_hidden_pref` and keep `set_show_hidden` as the delegate that `[ui] show_hidden` seeds. Status bar: `ui/status.rs` ~318 becomes a branch on `pane.results()`.
- [ ] **Step 4: Run** `just t norte-tui`, then `just c`. Expected: PASS with NO snapshot changes. A changed snapshot means behaviour moved: stop and diff before accepting anything.
- [ ] **Step 5: Commit** with message `refactor(tui): the search pane rides on the shared result set` and body "no behaviour change; virtual_search, search_state, search_error, search_matches and show_hidden_pref removed".

---

### Task 4: In the TUI, Alt+F7 opens a results tab

**Files:**
- Modify: `crates/norte-tui/src/app/focus.rs`: `open_results_tab`, next to `tab_new`.
- Modify: `crates/norte-tui/src/jobs/search.rs`:
  - `SearchRun` becomes keyed by slot; drop `pane` and `prev_dir`, add `panel_epoch`;
  - `launch_search` becomes `launch_results`;
  - `drain_search` takes the slot;
  - delete `on_search_escape`'s `cd` back (Esc only cancels);
  - `on_search_enter` becomes `open_hit_folder`.
- Modify: `crates/norte-tui/src/jobs/inflight.rs` (`search: BySlot<SearchRun>` and `search_cursor: usize`) and `crates/norte-tui/src/event_loop.rs` (the hits arm polls every run, like the `work.fill` arm ~1124).
- Modify: `crates/norte-tui/src/refresh.rs`: `reap_search_run` becomes `reap_search_runs(app, &mut BySlot<SearchRun>)`. It cancels and removes runs whose slot has no browser, or whose pane is no longer results.
- Modify: `crates/norte-tui/src/navigate.rs`: drop the swap bookkeeping for the run (~371-390), because the run follows its slot. `settle_cd`'s parameter type changes; follow the compiler through `keys.rs`, `mouse.rs`, `turn.rs`, `jobs/compare.rs` and `event_loop.rs`.
- Modify: `crates/norte-tui/src/keys.rs` ~1126: the Esc/Enter interception reads `work.search.get(app.focused_slot())`.
- Modify: `crates/norte-tui/src/app/ops.rs` `open_transfer`, and `mutations.rs`/`app/prompts.rs` where the destination comes from a pane (C4, C5).
- Modify: `crates/norte-tui/src/ui/geometry.rs` `tab_strip_for` (results title) and `crates/norte-tui/src/ui/pane.rs` `pane_title` (results location).
- Test: `crates/norte-tui/tests/results_tab.rs` (new) and `crates/norte-tui/tests/swap.rs` (adapted). Snapshot `snapshot_search_pane_virtual` becomes `snapshot_results_tab` (re-accepted: tab strip, Folder column).

**Interfaces:**
- Consumes: Tasks 1–3.
- Produces:

```rust
// app/focus.rs
impl App {
    /// A new tab next to the focused listing showing `set`, focused.
    pub fn open_results_tab(&mut self, set: ResultSet, entries: Vec<Entry>) -> SlotId;
}
// jobs/search.rs
pub struct SearchRun {
    pub task: TaskRef,
    pub rx: tokio::sync::mpsc::Receiver<SearchHits>,
    pub hits: usize,
    /// `Running` while the channel is open (the event loop polls only
    /// those); terminal once it closed (`finalize_outcome`, Task 3).
    pub outcome: Outcome,
    /// The search panel's run this one came from (Task 6): its batches
    /// also go to the panel while the panel shows that epoch.
    pub panel_epoch: Option<u64>,
}
pub async fn launch_results(app: &mut App, backend: &Backend, runs: &mut BySlot<SearchRun>,
                            search: SavedSearch, into: Option<SlotId>) -> Option<SlotId>;
pub fn drain_search(app: &mut App, runs: &mut BySlot<SearchRun>, slot: SlotId, hits: Option<SearchHits>);
pub async fn open_hit_folder(app: &mut App, backend: &Backend, events: &mut crate::console::Console<'_>,
                             fill: &mut BySlot<Fill>, decorate: &mut BySlot<DecorateFetch>, probed: &mut Probed,
                             runs: &mut BySlot<SearchRun>);
// refresh.rs
pub fn reap_search_runs(app: &App, runs: &mut BySlot<SearchRun>);
```

`launch_results` with `into: None` opens a new tab. With `Some(slot)`, it restarts that tab: `set.restart(MAX_HITS)` plus an empty listing; Task 5 uses that for refresh and restore. It cancels any run already registered for that slot. On error it says `search-status-failed` in the bar. With `into: None` it opens no tab; with `Some` it sets that tab's outcome to `Failed`.

- [ ] **Step 1: Failing tests** in `tests/results_tab.rs`. Use the embedded backend over a tempdir, as `tests/searchpanel.rs` from phase A does. Its helpers are `harness_with_tree` and `wait_terminal`; copy them into this file if they are private there.

```rust
/// Alt+F7's Enter opens a TAB: the pane that was focused keeps its folder,
/// a new tab shows the hits with the 🔍 title, and the run is filed under
/// the NEW slot.
#[tokio::test]
async fn the_dialog_opens_a_results_tab() {
    let (mut app, backend, mut work, _r) = harness_with_tree(&[("a/x.txt", "aguja\n"), ("b/y.txt", "aguja\n")]).await;
    let before = app.focused().dir().clone();
    let old_slot = app.focused_slot();
    let search = saved_content(&before, "aguja");
    let slot = launch_results(&mut app, &backend, &mut work.search, search, None).await.expect("tab");
    assert_ne!(slot, old_slot);
    assert_eq!(app.panes.browser(old_slot).expect("old").dir(), &before, "the folder tab is untouched");
    drain_until_closed(&mut app, &mut work, slot).await;
    let tab = app.panes.browser(slot).expect("tab");
    assert!(tab.is_results());
    assert_eq!(tab.entries().len(), 2);
    let strip = norte_tui::ui::tab_strip_for(&app, app.focus()).expect("a group");
    assert!(strip.titles.iter().any(|t| t.starts_with("🔍 aguja (2)")));
}

/// Review Focus 1: closing the tab cancels its run; navigating it to a
/// folder cancels its run and no late batch lands in the folder listing.
#[tokio::test]
async fn closing_or_leaving_the_tab_cancels_its_run() {
    let (mut app, backend, mut work, _r) = harness_with_big_tree(2_000).await; // enough to still be running
    let root = app.focused().dir().clone();
    let slot = launch_results(&mut app, &backend, &mut work.search, saved_name(&root, "*"), None).await.expect("tab");
    let task = work.search.get(slot).expect("run").task.clone();
    app.tab_close();
    reap_search_runs(&app, &mut work.search);
    assert!(work.search.get(slot).is_none());
    assert_eq!(wait_terminal(&task).await, norte_proto::TaskState::Cancelled);

    let slot = launch_results(&mut app, &backend, &mut work.search, saved_name(&root, "*"), None).await.expect("tab");
    let task = work.search.get(slot).expect("run").task.clone();
    app.panes.browser_mut(slot).expect("tab").set_listing(root.clone(), vec![]);
    reap_search_runs(&app, &mut work.search);
    assert_eq!(wait_terminal(&task).await, norte_proto::TaskState::Cancelled);
    assert!(!app.panes.browser(slot).expect("tab").is_results());
}

/// C10: Enter on a hit opens its folder in a NEW tab with the cursor on
/// it; the results tab is still there.
#[tokio::test]
async fn enter_opens_the_hit_folder_in_a_new_tab() { /* launch, drain, put the cursor on b/y.txt,
    call open_hit_folder; assert: a third browser slot exists with dir == root/b, selected() == root/b/y.txt,
    and the results slot still is_results() */ }

/// Review Focus 2 / C4 / C5: from a results tab into the ROOT is allowed
/// unless a marked hit already lives there; INTO a results tab refuses.
#[tokio::test]
async fn transfer_rules_for_results_tabs() { /* source = results tab over root, target = the original
    folder tab (also at root): open_transfer with hits a/x.txt, b/y.txt → a ConfirmTransfer modal to root;
    mark a hit that lives directly in root → app.message == t("results-already-there"), no modal;
    swap roles (results tab as destination) → message == t("msg-pane-not-a-location"), no modal */ }

/// Review Focus 3: two hits with the same name copied to one folder —
/// the second job meets the first's file and goes through the collision
/// queue; nothing is overwritten.
#[tokio::test]
async fn same_named_hits_meet_the_collision_policy() { /* tree a/n.txt ("A"), b/n.txt ("B"), dest folder
    empty; submit_transfers(Copy) with both; drive on_tick until both tasks are terminal; assert dest/n.txt
    reads "A" and app.pending_collisions (or the collision modal) holds the second */ }

/// Review Focus 4: the 🔍 title is two cells wide; the click zones of the
/// tab strip are measured with the painted width, so a click on the
/// third tab selects the third tab.
#[test]
fn the_results_tab_title_measures_its_emoji() { /* build an app with three tabs, the middle one a
    results tab with a hostile query; render to TestBackend 100x30; find the third title's first cell
    in the buffer; feed a mouse click there through crate::mouse; assert tabs_of(...).1 == 2 and the
    painted title contains no ESC */ }
```

Write the four bodies sketched in comments out in full, in the style of the first two, before running them. `saved_content`, `saved_name` and `drain_until_closed` are helpers in this file: `drain_until_closed` loops `work.search.get_mut(slot).rx.recv().await` into `drain_search` until `None`. In `swap.rs`, delete `the_swap_flips_the_pane_with_the_live_search` and `a_swap_does_not_harvest_the_live_search`. Replace them with ONE test: after `app.swap_panes()`, `work.search.get(slot)` still names the same tab, and that tab is still results.
- [ ] **Step 2: Run** `just t norte-tui`. Expected: FAIL.
- [ ] **Step 3: Implement.**
  - `open_results_tab` mirrors `tab_new`:
    - mint a slot;
    - build `self.new_pane(set.root().clone(), vec![])`, call `begin_results(set, entries)` on it, and `insert_browser`;
    - call `layout.add_tab(focused_slot, …browser())`;
    - make the new slot the active tab (`set_active_for` with its index from `tabs_of`), then `refresh_visible`, `prune_by_tree` and `settle_key_owner`.
  - The Alt+F7 call site (`keys.rs` ~405) builds `SavedSearch::from_form(dialog, app.focused().dir().clone())`. If the focused pane is itself a results tab, use its `results().search().root`.
  - Esc on a results tab with a live run calls `task.cancel()` and does nothing more. With no live run it falls through to the resolver.
  - Enter calls `open_hit_folder`: `open_results_tab`'s sibling `tab_new_at(dir)`, which mints a slot, inserts `new_pane(dir, vec![])` and lists it through the usual `cd` of that pane. Then it sets `set_pending_focus(hit)`, so the cursor lands on the hit when the listing arrives. Find how `go_to_result`-style focus is done in `trail.rs`/`navigate.rs` (`set_pending_focus`), and use that, not an index set after the fact.
  - Transfers. In `open_transfer`:
    - if `self.panes[to].is_results()`, set `self.message = Some(t("msg-pane-not-a-location"))` and return;
    - if `self.panes[from].is_results()` and `results::transfer_refusal(&items, &to_dir)` is `Some(key)`, set `self.message = Some(t(key))` and return.
    - The same two checks go where `open_transfer_dest` resolves a typed destination, and where a drop resolves one (`rg -n "open_transfer_to_dir\(" crates/norte-tui/src`).
  - Titles:
    - `tab_strip_for` uses `results::tab_title(set, p.real_entries().len(), lang)` for a results pane;
    - `pane_title` uses `results::location` (hostile flag → the same marking a hostile dir gets).
    - The tab strip's zone measurement (`ui/chrome.rs` `tab_pieces`/`tab_zones`) must already use display width. If it counts `chars()`, switch it to `UnicodeWidthStr::width` in both the painter and the zones.
  - `watch_targets`: a results pane watches nothing. Already true through `is_results()` from Task 3.
- [ ] **Step 4: Run** `just t norte-tui`. Run `cargo insta review` and accept only `snapshot_results_tab` (tab strip + Folder column). Run `just c`.
- [ ] **Step 5: Reviewer.** Dispatch `rust-reviewer` (the BySlot polling fairness, the reap points, a run whose slot is swapped into a hidden tab). Dispatch `encoding-auditor` (title, location, Folder cell). Apply BLOCKER/MAJOR findings. Commit with message `feat(tui): Alt+F7 opens a results tab instead of taking over the pane` and a body that names C4, C5 and C10.
- [ ] **Step 6: Gate:** `just ci-fast` once (Tasks 1–4).

---

### Task 5: In the TUI, results tabs refresh, follow mutations, and come back with the session

**Files:**
- Modify: `crates/norte-tui/src/dispatch.rs` `Command::PaneRefresh` (~574): on a results tab, `launch_results(.., into: Some(slot))`. This needs the runs map: route it like the other run-loop intents, through a pending flag (`app.pending_results_rerun = Some(slot)`, consumed in `turn.rs` where `pending_organize` is). Find that pattern with `rg -n "pending_organize" crates/norte-tui/src`.
- Modify: `crates/norte-tui/src/mutations.rs`:
  - `submit_transfers` (Move only), `submit_deletes`, and the rename submit (`rg -n "TransferName" crates/norte-tui/src/mutations.rs`) call `results_mut().expect(items)` on the SOURCE pane when it is a results tab;
  - a rename also expects the source path only, because the renamed hit leaves (C11).
- Modify: `crates/norte-tui/src/refresh.rs` `refresh_panes_where`: in place of the `continue` for a results pane, call `recheck_results(app, backend, events, i).await`.
- Modify: `crates/norte-tui/src/app/session.rs`:
  - `session_body_with_marks` writes `results: pane.results().map(|r| r.search().clone())`;
  - `apply_session` seeds a results slot with `Pane::new(root)` and pushes it to `self.session.rerun: Vec<SlotId>` instead of `ask`;
  - add a `rerun` field to the session struct that holds `orphans`/`cursors`.
- Modify: `crates/norte-tui/src/event_loop.rs` ~1668 (after `apply_session`): drain `app.session.rerun` through `launch_results(.., Some(slot))`.
- Test: `crates/norte-tui/tests/results_tab.rs`, `crates/norte-tui/tests/session.rs`

**Interfaces:**
- Consumes: Task 4's `launch_results`.
- Produces: `pub async fn recheck_results(app: &mut App, backend: &Backend, events: &mut crate::console::Console<'_>, side: usize)` in `refresh.rs`. It stats each `expected()` path through `crate::console::wait_painting`, the same cancellable wait listings use. `Err(Error::NotFound)` → `forget([p])` + `settle(p)`. `Ok(_)` → `settle(p)` only if the board has no live mutation task (`app.board` has a live-task query; use it, or add `Board::any_live_mutation()` if there is none). Any other error → `settle(p)`, keeping the hit. Plus `App::pending_results_rerun: Option<SlotId>` and `SessionUi::rerun: Vec<SlotId>`.

- [ ] **Step 1: Failing tests.**

```rust
/// C2: `pane.refresh` on a results tab re-runs the SAME query and picks
/// up a file created since.
#[tokio::test]
async fn refresh_re_runs_the_query() { /* launch over a tree with one hit, drain; write a second
    matching file into the tempdir; dispatch Command::PaneRefresh with the tab focused; run the turn that
    consumes pending_results_rerun; drain; assert 2 entries and outcome Done */ }

/// C11: deleting a hit from a results tab removes it from the tab when
/// the task finishes; the other hits stay; nothing is re-listed.
#[tokio::test]
async fn a_deleted_hit_leaves_the_tab() { /* two hits; mark one; submit_deletes(permanent=true);
    run on_tick until the board reports the task finished, then refresh_panes; assert one entry left,
    the tab still is_results(), and expected() is empty */ }

/// C11: a moved hit leaves; a hit whose move FAILED stays.
#[tokio::test]
async fn a_moved_hit_leaves_and_a_failed_move_stays() { /* two hits; move one into a folder that
    already has a same-named file with the collision dialog answered "skip"; assert the skipped one stays
    in the tab and expected() is empty after the board settles */ }
```

```rust
// tests/session.rs
/// Review Focus 5: a results tab behind another tab, whose root is gone,
/// is restored as its query: re-run, failed visibly, still a results tab.
#[tokio::test]
async fn a_restored_results_tab_re_runs_even_when_hidden() { /* body with two tabs in one group, the
    hidden one carrying results: Some(SavedSearch{root: a path that does not exist}); apply_session +
    the start-up rerun; drain; assert the slot is_results(), outcome is Failed(_), and its status line
    contains the failed sentence; and a body without `results` restores folders exactly as before */ }
```

Write each sketched body in full before running it.
- [ ] **Step 2: Run** `just t norte-tui`. Expected: FAIL.
- [ ] **Step 3: Implement** as listed under Files. `refresh_panes_where` iterates SIDES (`0..app.panes.len()`), so a hidden results tab is rechecked the next time it is visible and a mutation refresh fires. That is acceptable: its `expected` set waits. Say so in `recheck_results`' doc.
- [ ] **Step 4: Run** `just t norte-tui`, then `just c`.
- [ ] **Step 5: Commit** with message `feat(tui): results tabs refresh, follow mutations and survive the session` and a body that names C2, C7 and C11.

---

### Task 6: The search panel opens its results as a tab (TUI, and the shared snapshot)

**Files:**
- Modify: `crates/norte-frontend/src/search_panel.rs` (`PanelListing`, `SearchPanel::listing`, the private `pending_query`/`tree_query` fields)
- Modify: `crates/norte-tui/src/searchpanel.rs` (F9 → `open_listing`; `drain` forwards to a tab that adopted the run)
- Modify: `crates/norte-tui/src/jobs/search.rs` `drain_search` (forward to the panel while `panel_epoch` matches; on close, `finished` on the panel too)
- Modify: `en.ftl`/`es.ftl` (`search-panel-nothing-to-list`, `search-panel-open-listing`)
- Test: `search_panel.rs` `mod tests`, `crates/norte-tui/tests/searchpanel.rs`

**Interfaces:**
- Consumes: phase A's model and TUI panel; Task 4's `open_results_tab`, `SearchRun.panel_epoch`.
- Produces:

```rust
// crates/norte-frontend/src/search_panel.rs
#[derive(Debug, Clone, PartialEq)]
pub struct PanelListing {
    pub search: crate::results::SavedSearch,
    pub entries: Vec<Entry>,
    /// The first shown line of each file, as the status bar paints it.
    pub matches: Vec<(VPath, MatchInfo)>,
    /// `Some(epoch)` when the run that produced the tree is still live.
    pub live: Option<u64>,
    pub outcome: Outcome,
}
impl SearchPanel {
    /// What "Open as listing" takes: the files of the tree with the query
    /// THAT produced them. `None` with an empty tree, or while a new run
    /// has launched and its first answer has not replaced the tree yet.
    pub fn listing(&self) -> Option<PanelListing>;
}
// crates/norte-tui/src/searchpanel.rs
pub fn open_listing(app: &mut App, work: &mut InFlight);
```

- [ ] **Step 1: Failing model tests** (`search_panel.rs`):

```rust
/// The listing pairs the tree with the query that produced it, not with
/// what has been typed since.
#[test]
fn the_listing_carries_the_query_of_the_tree() {
    let t0 = Instant::now();
    let mut p = SearchPanel::new(vp("mem:///casa"));
    p.edit(SearchField::Content, "aguja".to_owned(), t0);
    let Some(Due::Launch { epoch, .. }) = p.due(t0 + DEBOUNCE, 0) else { panic!("launch") };
    p.apply_hits(epoch, &hits(&[("mem:///casa/a/x.txt", 3), ("mem:///casa/b/y.txt", 9)]));
    p.edit(SearchField::Content, "agujas".to_owned(), t0 + DEBOUNCE); // typing, pause pending
    let l = p.listing().expect("tree shown");
    assert_eq!(l.search.content, "aguja");
    assert_eq!(l.entries.len(), 2);
    assert_eq!(l.live, Some(epoch));
    assert_eq!(l.matches.iter().find(|(p, _)| p.to_wire().ends_with("x.txt")).and_then(|(_, m)| m.line), Some(3));
}

/// After the next launch and before its first batch, the tree is stale:
/// no listing.
#[test]
fn no_listing_between_a_launch_and_its_first_answer() {
    /* as above, then due(t0 + 2*DEBOUNCE, 0) → Launch; assert p.listing().is_none();
       apply_hits(new_epoch, ..) → listing().search.content == "agujas" */
}

/// An empty tree has nothing to list.
#[test]
fn an_empty_tree_has_no_listing() {
    assert!(SearchPanel::new(vp("mem:///casa")).listing().is_none());
}
```

`hits` is a helper in this test module building one `SearchHits` with one `MatchInfo` per entry (`line`, `preview: Some("…aguja…")`, `ranges: Some(vec![(1, 6)])`). Write `no_listing_between_a_launch_and_its_first_answer` in full.

TUI tests (`tests/searchpanel.rs`):

```rust
/// F9 in the panel opens a results tab with the panel's files; a live run
/// MOVES to the tab: the panel's next query does not cancel it, and the
/// tab keeps receiving.
#[tokio::test]
async fn f9_opens_the_listing_and_hands_over_the_live_run() { /* panel over a big tree (still
    streaming after the first batch); type, tick past DEBOUNCE, drain one batch; press F9 (input focus);
    assert: a results slot exists, focus returned to panes, work.search_panel is None,
    work.search.get(slot).panel_epoch == Some(epoch); type another query and tick → a NEW panel run;
    the tab's task is NOT cancelled (its progress is Running or Completed); drain the tab to the end →
    its entries count == the run's total */ }

/// F9 works from the tree focus as well as from an input.
#[tokio::test]
async fn f9_works_from_the_tree() { /* Tab to the tree; F9; a results tab exists */ }

/// With nothing shown, F9 says so and opens nothing.
#[tokio::test]
async fn f9_with_nothing_to_list_says_so() { /* app.message == t("search-panel-nothing-to-list") */ }
```

Write these bodies in full. If F9 from the tree focus is consumed by `keys.rs`'s shared-chord branch before `searchpanel::on_key`, `f9_works_from_the_tree` goes RED. The fix is in that branch's condition: `&& !(app.key_owner() == KeyOwner::Search && key.code == KeyCode::F(9))`. Do not change the presets.
- [ ] **Step 2: Run** `just t norte-frontend` and `just t norte-tui`. Expected: FAIL.
- [ ] **Step 3: Implement.**
  - Model:
    - `due()`'s Launch branch stores `pending_query = Some(SavedSearch::from_form(&self.form, self.root.clone()))`;
    - `replace_if_due` (the first answer or the end of the new run) moves it into `tree_query`;
    - `listing()` returns `None` if `replace_on_arrival` or the tree is empty;
    - `entries` = every `FileNode.entry` in tree order;
    - `matches` = each file's first `MatchLine` converted back to `MatchInfo { line: Some, offset, preview: Some, ranges: Some }`, filling any other `MatchInfo` field phase A added with its "absent" value;
    - `live` = `Some(run.epoch)` when the current run's outcome is `Running`;
    - `outcome` = that run's outcome.
  - TUI `open_listing`:
    - build `ResultSet::new(l.search, search_panel::MAX_FILES)`, then `seed_matches(l.matches)`, then `set_outcome(l.outcome)` when not live;
    - `let n = l.entries.len(); let slot = app.open_results_tab(set, l.entries)`;
    - if `l.live == Some(e)` and `work.search_panel.as_ref().is_some_and(|r| r.epoch == e)`, take the `PanelRun` and insert `SearchRun { task, rx, hits: n, outcome: Outcome::Running, panel_epoch: Some(e) }` under `slot`;
    - finally `app.return_keys_to_panes()`.
  - `drain_search`:
    - a batch → `if let Some(e) = run.panel_epoch` and the panel model exists, `panel.apply_hits(e, &batch)` (the model ignores it once its epoch moved on);
    - the end → `panel.finished(e, outcome)`.
- [ ] **Step 4: Run** `just t norte-frontend`, `just t norte-tui`, `just c`.
- [ ] **Step 5: Commit** with message `feat(tui): the search panel opens its results as a tab (F9)` and a body that names C9.

---

### Task 7: The window's results slots (bridge bump)

**Files:**
- Create: `crates/norte-ui-host/src/controller/results.rs` (state, launch, forwarder, apply, finish, abandon)
- Modify: `crates/norte-ui-host/src/controller/mod.rs`:
  - `results_runs: BTreeMap<u32, results::ResultsRun>`;
  - `Background::ResultsHits(u32, u64, Box<SearchHits>)`, `ResultsLive(u32, u64, TaskId)`, `ResultsBroken(u32, u64, Box<Error>)`, handled in `controller/extensions.rs` next to phase A's `SearchPanel*` arms.
- Modify: `crates/norte-ui-host/src/controller/searchpanel.rs`:
  - `UiAction::SearchPanelOpenListing { slot_id }` → `open_listing`;
  - the `SearchPanelHits` handler also extends every results slot whose run is `Feed::Panel(epoch)`;
  - wherever phase A finishes the panel run (`model.finished(epoch, outcome)`), also call `self.results_finished_by_panel(epoch, outcome)`.
- Modify: `crates/norte-ui-host/src/controller/tasks.rs`: where a TaskProgress turns the dialog `Search` or the panel's run terminal (`rg -n "outcome_of" crates/norte-ui-host/src/controller`), also finish a `results_runs` entry whose `task` equals the progress's id.
- Modify: `crates/norte-ui-host/src/controller/panel.rs` `header_of`:
  - `path_display`/`path_hostile` from `results::location`;
  - `path_segments` empty for results (in `crumbs_of`);
  - new `results_note` from `results::status`.
- Modify: `crates/norte-ui-host/src/controller/layout.rs` `tab()`: the title from `results::tab_title`.
- Modify: `crates/norte-ui-host/src/controller/nav.rs` `navigate_slot` and `controller/tabs.rs` (closing a slot or tab): `self.abandon_results(slot)`.
- Modify: `crates/norte-ui-host/src/dto.rs`: `BrowserSlotView.results_note: String` and `ViewChange::BrowserHeader.results_note: String`, both `#[serde(default)]`. Also `SearchPanelSlotView.open_listing_label: String` (`#[serde(default)]`), translated with `t_in(self.lang, "search-panel-open-listing")` in phase A's `search_panel_view`. Task 9's button paints it, because a renderer does not translate.
- Modify: `crates/norte-ui-host/src/action.rs` (`UiAction::SearchPanelOpenListing`), `crates/norte-ui-host/src/bridge.rs` (`BRIDGE_VERSION`), `crates/norte-ui-host/tests/golden.rs` (`action_tag`, `overlay_actions`, `reference_slots`: a results browser slot with a Folder header and a non-empty `results_note`).
- Modify: `crates/norte-gui-tauri/ui/src/types.ts` (`BRIDGE_VERSION`, `results_note?: string` on the browser slot and the header change, the action's type)
- Test: `crates/norte-ui-host/tests/controller/results.rs` (new; `mod results;` in `tests/controller/main.rs`), with phase A's fake helpers (`answer_search_with`, `searches_launched`, `search_cancels`, `open_slot`, `slot`)

**Interfaces:**
- Consumes: Tasks 1–2 and 6 (`SearchPanel::listing`); phase A's host panel.
- Produces:

```rust
// controller/results.rs
pub(super) enum Feed { Own(u64), Panel(u64) }
pub(super) struct ResultsRun {
    pub(super) feed: Feed,
    /// Zero until `ResultsLive`/`SearchPanelLive` names it.
    pub(super) task: norte_proto::TaskId,
    pub(super) abandoned: Arc<std::sync::atomic::AtomicBool>,
}
impl State {
    /// A new tab next to the active listing showing `set`; returns its id.
    pub(super) fn open_results_slot(&mut self, set: ResultSet, entries: Vec<Entry>,
        backend: &Arc<dyn HostBackend>, mailbox: &mpsc::Sender<Message>) -> (u32, Vec<BridgeEnvelope<UiUpdate>>);
    /// (Re)runs `slot`'s query into it (refresh, session restore).
    pub(super) fn launch_results(&mut self, slot: u32, backend: &Arc<dyn HostBackend>, mailbox: &mpsc::Sender<Message>);
    pub(super) fn apply_results_hits(&mut self, slot: u32, epoch: u64, hits: &SearchHits) -> Vec<ViewChange>;
    pub(super) fn results_finished(&mut self, slot: u32, outcome: Outcome) -> Vec<ViewChange>;
    pub(super) fn results_finished_by_panel(&mut self, epoch: u64, outcome: Outcome) -> Vec<ViewChange>;
    /// Cancels and forgets `slot`'s run, if any (closing, navigating away).
    pub(super) fn abandon_results(&mut self, slot: u32);
}
// action.rs
UiAction::SearchPanelOpenListing { slot_id: u32 }   // the PANEL's slot
```

The forwarder is `launch_search`'s (`controller/search.rs`). It sends `ResultsLive(slot, epoch, id)` after `TaskNew`, then a `ResultsHits` per batch, and `ResultsBroken` on a launch error. Every handler checks that `results_runs[slot].feed == Feed::Own(epoch)`; a mismatch is a late answer from a previous run and is ignored. Use `self.epoch_search += 1` for `Own` epochs: one counter for all the window's searches, so no epoch is reused across the dialog and the tabs.

Hand-over (C9): `open_listing`:
- calls `model.listing()`; `None` → `Unavailable { reason_key: "search-panel-nothing-to-list" }`;
- `open_results_slot(..)`;
- if `live == Some(e)` and the panel's `run` is that epoch, MOVE its `PanelRunHost` into `results_runs[slot]` as `Feed::Panel(e)` and set the panel's `run = None`. From then on, the panel's next launch abandons nothing of it, and the panel's own board-row bookkeeping (phase A's C4) no longer owns that task id.

- [ ] **Step 1: Failing tests** (`tests/controller/results.rs`, `tokio::time::pause` as phase A's panel tests):

```rust
/// Open as listing: a new tab next to the active listing, titled with
/// the query and count, the Folder column right after the name, the
/// location line instead of breadcrumbs, the status note.
#[tokio::test(start_paused = true)]
async fn open_as_listing_makes_a_results_tab() {
    let (mut h, fake) = harness_with_fake().await;
    let panel = h.open_slot("layout.search").await;
    fake.answer_search_with(two_files("mem:///casa/a/x.txt", "mem:///casa/b/y.txt"));
    h.act(UiAction::SearchPanelEdit { slot_id: panel, field: "content".into(), value: DialogFieldValue::Text { text: "aguja".into() } }).await;
    h.act(UiAction::SearchPanelGo { slot_id: panel }).await;
    h.settle().await;
    h.act(UiAction::SearchPanelOpenListing { slot_id: panel }).await;
    h.settle().await;
    let snap = h.snapshot();
    let tab = snap.layout.tabs.iter().flat_map(|g| &g.tabs).find(|t| t.title.starts_with("🔍")).expect("a 🔍 tab");
    assert_eq!(tab.title, "🔍 aguja (2)");
    let SlotView::Browser(b) = h.slot(tab.slot_id) else { panic!("browser") };
    assert_eq!(b.columns[1].id, "folder");
    assert!(b.path_segments.is_empty());
    assert!(b.path_display.contains("aguja"));
    assert!(!b.results_note.is_empty());
    assert_eq!(b.rows.iter().map(|r| r.cells[0].text.clone().unwrap_or_default()).collect::<Vec<_>>(), ["a", "b"]);
}

/// C9: a live run moves to the tab; the panel's next query launches a
/// new run and does NOT cancel the tab's; the tab keeps receiving.
#[tokio::test(start_paused = true)]
async fn the_live_run_moves_to_the_tab() { /* fake.hold_search_stream() so the first run stays open;
    open listing; edit + advance DEBOUNCE → searches_launched()==2, search_cancels==0; fake.push_batch(first
    run, one more file) → the tab's total_rows grows by one; fake.close_search(first) → the tab's
    results_note says done */ }

/// Review Focus 1: closing the results tab, or navigating it to a
/// folder, cancels its run; a late batch does not land in the folder.
#[tokio::test(start_paused = true)]
async fn closing_or_navigating_cancels_the_run() { /* open listing with a held stream; close the
    tab (Effect::CloseTab with it focused) → search_cancels==1; reopen; navigate it (UiAction for a path
    typed / breadcrumb is gone, use the goto action or nav to parent) → search_cancels==2; push a late batch
    → the folder listing's rows are the folder's only */ }

/// A header click on Folder sorts by folder.
#[tokio::test(start_paused = true)]
async fn the_folder_header_sorts() { /* open listing; UiAction for a header click on "folder"
    (rg the action the header sends: `SortBy`/`HeaderClick`) → columns[1].sort == Some("asc") */ }

/// Review Focus 4: a hostile query is masked in the tab title and the
/// location, and flagged.
#[tokio::test(start_paused = true)]
async fn a_hostile_query_is_masked() { /* content "a\u{1b}[2Jb"; the 🔍 tab has title_hostile and
    no ESC; b.path_hostile is true */ }
```

`two_files`, `hold_search_stream`, `push_batch` and `close_search` are fake helpers; add the ones that do not exist on `tests/backend_fake/mod.rs`. Write every sketched body in full. In `golden.rs`, add one `overlay_actions` case for `SearchPanelOpenListing`, its `action_tag` arm, and a `reference_slots` browser slot with `results_note`.
- [ ] **Step 2: Run** `just t norte-ui-host`. Expected: FAIL.
- [ ] **Step 3: Implement** as listed under Files.
  - `open_results_slot`:
    - `let id = self.new_slot();` then insert a `Slot::empty(set.root().clone(), …)` whose pane gets `begin_results(set, entries)`;
    - mark it settled (not loading: `state` = the at-rest variant `land_listing` sets);
    - `self.tree.add_tab(SlotId(self.focused_listing()), &Node::slot(SlotId(id), KindId::browser()))`, where `focused_listing()` is the active BROWSER slot, not the panel;
    - `apply_layout_with(updated, Some(SlotId(id)), ..)`, the same as `tab_new`.
  - Every change to a results slot republishes its header (`header_of`) and rows, the way `apply_batch` does for a filling listing (`rg -n "fn apply_batch" crates/norte-ui-host/src/controller`).
  - `abandon_results` sets `abandoned`, then calls `self.cancel(task)` if `task != 0`. That is phase A's `abandon_search` shape.
  - Bridge: run `rg -n "BRIDGE_VERSION: u32" crates/norte-ui-host/src/bridge.rs`, take N+1, and write the doc line: `- **N+1**: results tabs (spec 2026-10-10): BrowserSlotView/BrowserHeader.results_note, the column id "folder", the action search_panel_open_listing`.
- [ ] **Step 4: Run** `just t norte-ui-host`. Then `NORTE_BLESS=1 just t norte-ui-host` and review the golden diff: it must show ONLY the new action, the new field and the reference slot. Then run `cargo test -p norte-ui-host --doc` and `just c`.
- [ ] **Step 5: Reviewer.** Dispatch `rust-reviewer` with these questions: the hand-over of a `PanelRunHost` whose id is still 0; a `ResultsLive` arriving after the tab closed (the forwarder must cancel through `abandoned`); the epoch check on every `Results*` message. Apply the findings, then commit with message `feat(ui-host): results tabs in the window (bridge N+1)` and a body that names C9.

---

### Task 8: The window's results slots refuse what has no location, transfer per hit, refresh, follow mutations, restore

**Files:**
- Modify: `crates/norte-ui-host/src/controller/effects.rs`, plus the handlers it reaches. A results slot refuses with `host-results-not-a-location` (C6). The effects:
  - `CreateDirectory`, `CreateFile`, `Organize`, `RenameIa`, `RenameBatch`;
  - `Pack`, `Join` (both compose at `pane.dir()`);
  - `Sync`, `Compare` (directory compare), `Terminal`, `OpenTerminal` (the shell's cwd);
  - `Mirror`/`MirrorTarget`/`Bring`/`Swap`/`Send` when the ORIGIN is a results slot;
  - `SearchSemantic` (the TUI refuses it too).

  Put the check in ONE place: `fn refuses_in_results(&self, effect: &Effect) -> Option<&'static str>`, called at the top of the dispatch for the active slot. Also run `rg -n "slot\(\)\.pane\.dir\(\)" crates/norte-ui-host/src/controller` and classify every hit:
  - (a) a location to write at → covered by the list above, or add it;
  - (b) refresh bookkeeping → in a results slot, use the entries' parents.
- Modify: `crates/norte-ui-host/src/controller/transfer.rs`:
  - `directory_dest` returns `Err("host-results-not-a-location")` when the target slot is results (C5);
  - `transfer_operands` uses `results::transfer_refusal(&paths, &dest)` for a results source in place of the `source_dir == dest` check (C4);
  - `launch_transfer`'s `affected` for a results move pushes each path's parent; it already does, so verify it and keep it;
  - for a results source, a confirmed move/delete/rename calls `slot.pane.results_mut().expect(sources)` in the actor before the spawn (`Pending::Transferir` confirm, `launch_deleted`, `confirm_rename`).
- Modify: `crates/norte-ui-host/src/controller/listing.rs`:
  - `refresh(slot)`: a results slot does NOT re-list. `refresh_visible` (`pane.refresh`) calls `launch_results(slot)` for it (C2);
  - `refresh_affected`:
    - a results slot is excluded from the re-list filter;
    - every results slot with a non-empty `expected()` gets ONE spawned stat batch → `Message::Background(Background::ResultsRechecked(slot, Vec<(VPath, bool /*gone*/)>, bool /*settle*/))`, where `settle = !remains_work`;
    - the handler calls `forget` on the gone ones and `settle` on the gone ones plus, when `settle`, the rest.
  - `wake_visible`: a results slot coming into view does nothing; it has its rows.
- Modify: `crates/norte-ui-host/src/controller/session.rs`:
  - `capture_session_with_marks` writes `results: slot.pane.results().map(|r| r.search().clone())`;
  - `apply_session` seeds a results slot with `begin_results(ResultSet::new(saved, MAX_HITS), vec![])`, not `begin_loading`.
- Modify: `crates/norte-ui-host/src/controller/mod.rs` `list_initial`: a results slot is skipped for listing, and `launch_results(id)` runs for EVERY results slot, hidden or not (C7).
- Modify: `en.ftl`/`es.ftl` (`host-results-not-a-location`, the same text as `results-not-a-location`)
- Test: `crates/norte-ui-host/tests/controller/results.rs`

**Interfaces:**
- Consumes: Task 7.
- Produces: `Background::ResultsRechecked(u32, Vec<(VPath, bool)>, bool)`; `State::refuses_in_results`.

- [ ] **Step 1: Failing tests.**

```rust
/// C6: mkdir, organize and mirror refuse in a results tab with the
/// reason; the folder tab next to it still can.
#[tokio::test(start_paused = true)]
async fn location_gestures_refuse_in_a_results_tab() { /* open a results tab (helper from Task 7);
    for each of pane.mkdir, pane.organize, pane.mirror: h.command(..) → ActionAck::Unavailable { reason_key:
    "host-results-not-a-location" }; focus the folder tab; pane.mkdir opens its dialog */ }

/// Review Focus 2 (C4/C5): copying hits into the root goes to the
/// confirmation; a hit already in the root refuses; a results tab as the
/// destination refuses.
#[tokio::test(start_paused = true)]
async fn transfers_from_and_into_results_tabs() { /* results over mem:///casa with a/x.txt and b/y.txt;
    target = the folder tab at mem:///casa: pane.copy → a dialog opens; mark also mem:///casa/c.txt (a hit in
    the root) → Unavailable "results-already-there"; make the results tab the target and copy from the
    folder tab → Unavailable "host-results-not-a-location" */ }

/// Review Focus 3 (C3): two same-named hits copied into one folder: the
/// second job fails visibly on the collision; nothing is overwritten.
#[tokio::test(start_paused = true)]
async fn same_named_hits_each_get_their_job() { /* hits a/n.txt and b/n.txt; copy to an empty folder;
    confirm; fake records two copy jobs, the second sees Exists → the board has one Completed and one
    Failed row; and on a fake destination that folds case, `a/N.txt` + `b/n.txt` still refuses with
    host-batch-folds-to-one */ }

/// C11: a deleted hit leaves the tab once the delete finishes; a hit
/// whose stat still answers stays.
#[tokio::test(start_paused = true)]
async fn deleted_hits_leave_the_tab() { /* delete one marked hit; fake completes the task and then
    answers stat NotFound for it; settle → total_rows down by one; the other hit still there */ }

/// C2: pane.refresh re-runs the query into the same tab.
#[tokio::test(start_paused = true)]
async fn refresh_re_runs_into_the_same_tab() { /* searches_launched() goes up by one; the slot id is
    the same; rows replaced by the new answer */ }

/// Review Focus 5 / C7: a session with a results slot behind another
/// tab, whose root is gone: re-run at start-up, failed visibly, still a
/// results tab — and a body without `results` restores exactly as before.
#[tokio::test(start_paused = true)]
async fn a_restored_results_slot_re_runs_even_hidden() { /* harness_with_session(body) with the
    fake answering search with Err(NotFound) for that root; settle; the slot's results_note contains the
    failed sentence; path_segments empty; no listing request for that root */ }
```

Write every body in full in the style of Task 7's tests.
- [ ] **Step 2: Run** `just t norte-ui-host`. Expected: FAIL.
- [ ] **Step 3: Implement** as listed under Files.
- [ ] **Step 4: Run** `just t norte-ui-host`, then `NORTE_BLESS=1 just t norte-ui-host` only if a golden moved (none should), then `just c`.
- [ ] **Step 5: Reviewer.** Dispatch `rust-reviewer`, and `security-reviewer` because this touches which paths a mutation writes. Questions: the recheck racing a refresh re-run that replaced the entries (`forget` must be a no-op for paths no longer present); `transfer_refusal`'s byte comparison against a folding destination (it errs permissive, like the check it replaces; is the core's authority enough?). Apply the findings, then commit with message `feat(ui-host): results tabs refuse locations, transfer per hit, refresh and restore` and a body that names C2–C7 and C11.
- [ ] **Step 6: Gate:** `just ci-fast` once (Tasks 5–8).

---

### Task 9: The webview: the button, the results note, the Folder column

**Files:**
- Modify: `crates/norte-gui-tauri/ui/src/render/searchpanel.ts` (phase A's panel). Add an **Open as listing** button whose text is `open_listing_label` (Task 7). It is enabled when `total_rows > 0` and sends `{ action: "search_panel_open_listing", slot_id }`.
- Modify: `crates/norte-gui-tauri/ui/src/render.ts` (~1259, ~1367): paint `results_note` where `filling_note` goes (`slot-results` class), and `session.ts` ~142, which copies header changes into the slot.
- Modify: `crates/norte-gui-tauri/ui/src/style.css`: `.slot-results` uses an existing theme variable. `gui-ci` fails on a CSS variable no theme feeds.
- Test: `crates/norte-gui-tauri/ui/tests/render.test.ts`

**Interfaces:**
- Consumes: Task 7's wire.
- Produces: nothing new on the wire beyond Task 7's bump.

- [ ] **Step 1: Failing vitest cases.**

```ts
it("paints the results tab: 🔍 title with count, Folder column, results note", () => {
  const view = referenceView();               // this file's fixture builder
  const slot = browserSlot({
    slot_id: 7,
    path_display: "search “aguja” in /casa",
    path_segments: [],
    results_note: "search: 2 hits",
    columns: [header("name", "Name"), header("folder", "Folder"), header("size", "Size")],
    rows: [row("x.txt", ["a", "1 B"]), row("y.txt", ["b", "1 B"])],
  });
  const root = render({ ...view, slots: [slot], layout: withTabs(7, ["🔍 aguja (2)", "casa"]) });
  expect(root.querySelector(".tab.active")?.textContent).toBe("🔍 aguja (2)");
  expect([...root.querySelectorAll(".col-header")].map((h) => h.textContent)).toEqual(["Name", "Folder", "Size"]);
  expect(root.querySelector(".slot-results")?.textContent).toBe("search: 2 hits");
  expect(root.querySelector(".breadcrumbs")).toBeNull();
});

it("the search panel's Open as listing sends its action, disabled with no rows", () => {
  const sent: unknown[] = [];
  const root = renderPanel(searchPanelSlot({ slot_id: 3, total_rows: 0 }), (a) => sent.push(a));
  const btn = root.querySelector<HTMLButtonElement>("button.search-open-listing");
  expect(btn?.disabled).toBe(true);
  const root2 = renderPanel(searchPanelSlot({ slot_id: 3, total_rows: 4 }), (a) => sent.push(a));
  root2.querySelector<HTMLButtonElement>("button.search-open-listing")?.click();
  expect(sent).toEqual([{ action: "search_panel_open_listing", slot_id: 3 }]);
});
```

Name the fixture builders and selectors after the ones `render.test.ts` and phase A's panel tests already use. Check them with `rg -n "function (browserSlot|referenceView|searchPanelSlot)" crates/norte-gui-tauri/ui/tests`. The class names above are the ones this task introduces.
- [ ] **Step 2: Run** `cd crates/norte-gui-tauri/ui && npx vitest run tests/render.test.ts`. Expected: FAIL.
- [ ] **Step 3: Implement** as listed under Files. Text is set with `textContent`, never `innerHTML`.
- [ ] **Step 4: Run** vitest again, then `just gui-ci` (once).
- [ ] **Step 5: Commit** with message `feat(gui): open search results as a tab from the panel`.

---

### Task 10: Help topic and CHANGELOG

**Files:**
- Modify: `crates/norte-help/topics/en/finding.md` and `crates/norte-help/topics/es/finding.md`. Add a section "Results as a tab" / "Resultados en una pestaña" covering:
  - F9 in the panel and the window's button;
  - Alt+F7 now opens a tab;
  - the Folder column;
  - Enter opens the hit's folder in a new tab, and Esc stops a running search;
  - `pane.refresh` re-runs;
  - what refuses (create a folder here, organize, send this location) and why;
  - copying into the search root.
- Modify: `crates/norte-help/tests/corpus.rs`, if it counts sections or keys per topic (it did for phase A).
- Modify: `CHANGELOG.md` `[Unreleased]`:
  - **Changed**: Alt+F7 in the terminal opens a results tab instead of taking over the pane;
  - **Added**: Open as listing (F9 in the panel, button in the window), the Folder column, results tabs restored as their query.
- Modify: `crates/norte-cli/tests/goldens/help-en.json`, re-blessed with `NORTE_UPDATE_GOLDEN=1 just t norte-cli`.

- [ ] **Step 1: Run** `just t norte-help`. Expected: FAIL only if the corpus test pins the topic's anchors. Otherwise write the text and go to Step 3.
- [ ] **Step 2: Write** both topics. ES is not a translation of EN word for word; it uses the Spanish UI's own labels from `es.ftl`.
- [ ] **Step 3: Run** `just t norte-help`, then `NORTE_UPDATE_GOLDEN=1 just t norte-cli` and review the diff.
- [ ] **Step 4: Commit** with message `docs: results tabs (help and changelog)`.

---

## Self-review notes

- Spec coverage:
  - model → Tasks 1–2;
  - addressing and refusals → Task 4 (TUI, plus Task 3's carried-over refusals) and Task 8 (window);
  - columns and sort → Task 2, painted in Tasks 4/7/9;
  - live updates and refresh → Tasks 4–7, 8;
  - file ops, collisions, entries leaving → Tasks 4, 5, 8;
  - session → Tasks 2, 5, 8;
  - TUI Alt+F7 dialog → Task 4;
  - wire → Task 7;
  - testing bullets → Tasks 1, 4–9.
- Out of scope, as in the spec: editing the list by hand, saving a result set to disk. Also out (C8): the window's Alt+F7 dialog overlay.
