# H3b — TUI help overlay implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the flat scrolling `F1` key list in the TUI with a navigable
help overlay — sidebar of topics, rendered prose body, incremental filter,
history, and `Enter` running a command row through the existing dispatch —
driven entirely by the `norte-help` corpus shipped in H3a.

**Architecture:** The navigation model (which topics exist, which one is open,
the filter, the history, which pane has focus, which action the cursor is on)
lives in a new `norte_frontend::help::HelpState` so H3f gets the GUI view for
free — same hoist the settings UI (S4) and the palette rows (G3c) already did.
`norte-tui` keeps only three things: a `ChordResolver` implementation joining
the corpus' live `{{cmd:…}}` marks to the effective keymap and the Fluent
catalogue, a ratatui block renderer, and the run-loop wiring. Overlay keys
resolve through the `dialog` keymap context with a new `ALLOW_HELP` allowlist
(H1 precedent — no hardcoded legends), except while the filter editor is
active, which consumes raw characters like the palette and the search dialog.

**Tech Stack:** Rust, `norte-help` (corpus/model/resolver), `norte-frontend`
(shared frontend model), ratatui, `norte-i18n` (Fluent), `unicode-width`,
insta snapshots, nextest.

---

## Scope and deliberate deviations from the spec

Read this before Task 1; three decisions here differ from the spec sketch and
every later task assumes them.

1. **`[?] keys` is a synthetic topic, not a fourth dialog verb.** The spec's
   mock shows a `[?] keys` toggle. Implementing it as a key means a fourth
   `dialog.*` verb for what is really "open the keyboard page". Instead the
   sidebar gets one synthetic entry, `keys`, whose body is exactly today's
   `help::build` output. That keeps #113 (the `dialog.*` verbs are learnable
   from somewhere) and its test alive, costs no vocabulary, and H3h can replace
   the synthetic entry with a real corpus topic without touching the keymap.
2. **`Enter` on a command row closes the overlay, then dispatches.** The
   command acts on the panes underneath; leaving the help covering them would
   hide the confirmation modal the command opens. `Enter` on a *link* does not
   close — it follows the link and pushes history. Same split the palette
   already has between "run" and "filter".
3. **Availability is `Available` for every row in this phase.** Wiring the real
   sources (backend capabilities, plugin state, policy) is H3d by the spec's
   own phase table. The resolver returns `Availability::Available` with a
   comment naming H3d, and no test asserts a reason — a fake "everything is
   fine" assertion would have to be deleted rather than extended.

## File structure

| File | Responsibility |
| --- | --- |
| `crates/norte-frontend/src/help.rs` (new) | `HelpState`: sidebar rows, filter, history, focus, action cursor. No ratatui, no GPUI. |
| `crates/norte-frontend/Cargo.toml` | Adds `norte-help` (dependency direction frontend → help, as `resolve.rs` documents). |
| `crates/norte-tui/src/help.rs` (rewrite) | Keeps `build()` (the keys cheatsheet) and adds `TuiChords`, the `ChordResolver` for this frontend. |
| `crates/norte-tui/src/help_render.rs` (new) | `Block` → ratatui `Line`s, cell-aware wrapping, action line map. |
| `crates/norte-tui/src/app.rs` | `Help` → `HelpView`; `ALLOW_HELP`; `help_action`. |
| `crates/norte-tui/src/hints.rs` | `DialogHints::help`. |
| `crates/norte-tui/src/keymap.rs` | Three new `dialog.*` verbs in `DIALOG_COMMANDS`. |
| `crates/norte-tui/src/ui.rs` | `draw_help` rewritten: sidebar + body + footer. |
| `crates/norte-tui/src/main.rs` | Overlay key handling, dispatch, hot reload. |
| `crates/norte-frontend/presets/keymap/{orthodox,vim,cua}.toml` | Bindings for the three new verbs. |
| `crates/norte-i18n/i18n/{en,es}.ftl` | `dialog-cmd-*`, `help-group-*`, `help-topic-keys`, `help-status`, `help-see-also`. |
| `crates/norte-help/topics/{en,es}/help.md` (new) | The topic that documents the overlay — and pays for the new verbs at the documentation gate. |
| `crates/norte-tui/tests/help_overlay.rs` (new) | Overlay behaviour and render tests. |
| `crates/norte-tui/tests/help_gate.rs` | Allowlist shrinks (never grows). |

---

### Task 1: `HelpState` — the shared navigation model

**Files:**
- Create: `crates/norte-frontend/src/help.rs`
- Modify: `crates/norte-frontend/src/lib.rs` (add `pub mod help;`)
- Modify: `crates/norte-frontend/Cargo.toml` (add `norte-help.workspace = true`)

- [ ] **Step 1: Add the dependency and the module declaration**

In `crates/norte-frontend/Cargo.toml`, under `[dependencies]`, next to the
other `norte-*` entries:

```toml
# H3b: the help corpus and its model. The dependency runs frontend → help and
# never the other way (`norte_help::resolve`'s rustdoc says why): a
# `norte-help` that knew about a frontend could only ever serve one of the
# three. Already in the graph as a workspace crate — nothing new enters.
norte-help.workspace = true
```

In `crates/norte-frontend/src/lib.rs`, with the other `pub mod` lines, in
alphabetical position:

```rust
pub mod help;
```

- [ ] **Step 2: Write the failing tests**

Create `crates/norte-frontend/src/help.rs` containing ONLY this test module
for now (the code above it comes in step 4):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> HelpState {
        HelpState::new(Lang::En)
    }

    #[test]
    fn opens_on_the_index_with_every_topic_in_the_sidebar() {
        let s = state();
        assert_eq!(s.current().as_str(), "index");
        let topics: Vec<&str> = s
            .rows()
            .iter()
            .filter_map(|r| match r {
                SidebarRow::Topic { id, .. } => Some(id.as_str()),
                SidebarRow::Group { .. } => None,
            })
            .collect();
        assert!(topics.contains(&"copying"), "{topics:?}");
        assert!(
            topics.contains(&KEYS_ID),
            "the synthetic keyboard page is a sidebar entry: {topics:?}"
        );
    }

    #[test]
    fn a_group_header_precedes_the_topics_of_its_tag() {
        let s = state();
        let first = s.rows().first().expect("a non-empty sidebar");
        assert!(
            matches!(first, SidebarRow::Group { .. }),
            "the sidebar opens with a group header: {first:?}"
        );
    }

    #[test]
    fn the_cursor_never_lands_on_a_group_header() {
        let mut s = state();
        for _ in 0..s.rows().len() * 2 {
            s.down();
            assert!(
                matches!(s.rows()[s.cursor()], SidebarRow::Topic { .. }),
                "cursor {} landed on a header",
                s.cursor()
            );
        }
        for _ in 0..s.rows().len() * 2 {
            s.up();
            assert!(matches!(s.rows()[s.cursor()], SidebarRow::Topic { .. }));
        }
    }

    #[test]
    fn following_a_link_pushes_history_and_back_pops_it() {
        let mut s = state();
        s.open(&TopicId::new("copying"));
        s.open(&TopicId::new("archives"));
        assert_eq!(s.current().as_str(), "archives");
        assert!(s.back());
        assert_eq!(s.current().as_str(), "copying");
        assert!(s.back());
        assert_eq!(s.current().as_str(), "index");
        assert!(!s.back(), "no history left, and the caller must be able to tell");
        assert_eq!(s.current().as_str(), "index");
    }

    #[test]
    fn opening_an_unknown_topic_changes_nothing() {
        // A `[[link]]` the corpus check would have caught, or a plugin topic
        // that was uninstalled between two frames. Never a panic, never a
        // blank body: the reader stays where they were.
        let mut s = state();
        s.open(&TopicId::new("no-such-topic"));
        assert_eq!(s.current().as_str(), "index");
        assert!(!s.back(), "a refused open must not push history either");
    }

    #[test]
    fn the_filter_keeps_topics_whose_title_id_or_command_matches() {
        let mut s = state();
        s.start_filter();
        for c in "copy".chars() {
            s.push_char(c);
        }
        let ids: Vec<&str> = s
            .rows()
            .iter()
            .filter_map(|r| match r {
                SidebarRow::Topic { id, .. } => Some(id.as_str()),
                SidebarRow::Group { .. } => None,
            })
            .collect();
        assert!(ids.contains(&"copying"), "matched by id and title: {ids:?}");
        assert!(!ids.contains(&"mouse"), "unrelated topic filtered out: {ids:?}");
        assert!(
            s.rows().iter().all(|r| match r {
                SidebarRow::Group { .. } => true,
                SidebarRow::Topic { .. } => true,
            }),
            "rows stay well-formed under a filter"
        );
        s.backspace();
        s.backspace();
        s.backspace();
        s.backspace();
        assert_eq!(s.filter(), "", "backspacing to empty restores everything");
        assert!(s.rows().len() > ids.len());
    }

    #[test]
    fn a_filter_that_matches_nothing_leaves_the_open_topic_alone() {
        let mut s = state();
        s.open(&TopicId::new("copying"));
        s.start_filter();
        for c in "zzzz".chars() {
            s.push_char(c);
        }
        assert!(s.rows().is_empty(), "nothing matched");
        assert_eq!(
            s.current().as_str(),
            "copying",
            "the body keeps showing what the reader was reading"
        );
    }

    #[test]
    fn a_hostile_filter_is_matched_raw_and_displayed_masked() {
        // Same contract as `QuickSearch::query_display` and the palette: the
        // bytes typed are what we match on, and masking happens only where
        // it is painted.
        let mut s = state();
        s.start_filter();
        s.push_char('\u{202E}');
        s.push_char('a');
        assert_eq!(s.filter(), "\u{202E}a", "matching sees the raw bytes");
        assert!(
            !s.filter_display().chars().any(norte_encoding::is_terminal_hazard),
            "what is painted carries no hazard: {:?}",
            s.filter_display()
        );
    }

    #[test]
    fn focus_moves_between_the_panes_and_the_body_cursor_walks_actions() {
        let mut s = state();
        s.open(&TopicId::new("copying"));
        assert_eq!(s.focus(), Focus::Topics);
        s.toggle_focus();
        assert_eq!(s.focus(), Focus::Body);
        // `copying` declares five commands plus its `see_also` links.
        let first = s.action().cloned().expect("a first action");
        assert_eq!(first, Action::Run("pane.copy".to_owned()));
        s.down();
        assert_eq!(s.action(), Some(&Action::Run("pane.move".to_owned())));
        // Walking off the end stops at the end rather than wrapping into the
        // links by surprise.
        for _ in 0..50 {
            s.down();
        }
        assert!(s.action().is_some());
    }

    #[test]
    fn the_keyboard_page_has_lines_and_no_actions() {
        let mut s = state();
        s.open(&TopicId::new(KEYS_ID));
        assert_eq!(s.current().as_str(), KEYS_ID);
        assert!(s.topic().is_none(), "the keyboard page is not a corpus topic");
        s.toggle_focus();
        assert_eq!(s.action(), None, "nothing on it is runnable from here");
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo nextest run -p norte-frontend help::`
Expected: FAIL — `cannot find type HelpState in this scope` (the module has no
code yet).

- [ ] **Step 4: Write the implementation**

Put this ABOVE the test module in `crates/norte-frontend/src/help.rs`:

```rust
//! Navigation model of the help overlay (H3b): which topics the sidebar
//! shows, which one is open, what the filter keeps, where the reader has
//! been, and which action the cursor is on.
//!
//! Lives here and not in `norte-tui` for the reason the settings model (S4)
//! and the palette rows (G3c) live here: the GUI view of H3f is the SAME
//! model with a different painter, and a second copy in `norte-gui` would
//! drift from this one within a phase. Nothing in this module knows about
//! ratatui, GPUI or a terminal cell.

use norte_help::{Lang, Topic, TopicId, topics};

/// Id of the synthetic keyboard page.
///
/// Not a corpus topic: its body is the effective keymap, generated by the
/// frontend (`norte_tui::help::build`), so it cannot be prose in a `.md`
/// file. It gets an id anyway because the sidebar, the history and the
/// filter all address entries by id, and a special case in each of them
/// would be three places to forget. H3h may replace it with a real topic;
/// nothing outside this module has to change if it does.
pub const KEYS_ID: &str = "keys";

/// Tag the keyboard page is grouped under.
const KEYS_TAG: &str = "keys";

/// One row of the sidebar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarRow {
    /// Group header — the tag shared by the topics under it. NOT selectable.
    Group {
        /// The tag itself; the frontend translates it (`help-group-{tag}`).
        tag: String,
    },
    /// A topic the reader can open.
    Topic {
        /// Its id.
        id: TopicId,
        /// Its title, already the corpus' own (masked at parse for plugins).
        title: String,
    },
}

/// Which half of the overlay has the cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    /// The sidebar: up/down change the open topic.
    Topics,
    /// The body: up/down walk the runnable rows and links, Enter acts.
    Body,
}

/// What `Enter` does on the focused body row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Dispatch a command — the same id the palette sends.
    Run(String),
    /// Open another topic.
    Open(TopicId),
}

/// The overlay's whole state.
#[derive(Clone, Debug)]
pub struct HelpState {
    lang: Lang,
    current: TopicId,
    history: Vec<TopicId>,
    rows: Vec<SidebarRow>,
    cursor: usize,
    filter: String,
    filtering: bool,
    focus: Focus,
    actions: Vec<Action>,
    action_cursor: usize,
    body_scroll: usize,
}

impl HelpState {
    /// Opens the overlay on the index of `lang`'s corpus.
    ///
    /// ```
    /// use norte_frontend::help::HelpState;
    /// use norte_help::Lang;
    ///
    /// let s = HelpState::new(Lang::En);
    /// assert_eq!(s.current().as_str(), "index");
    /// ```
    #[must_use]
    pub fn new(lang: Lang) -> Self {
        let mut s = Self {
            lang,
            current: TopicId::new("index"),
            history: Vec::new(),
            rows: Vec::new(),
            cursor: 0,
            filter: String::new(),
            filtering: false,
            focus: Focus::Topics,
            actions: Vec::new(),
            action_cursor: 0,
            body_scroll: 0,
        };
        s.rebuild_rows();
        s.rebuild_actions();
        s
    }

    /// The topic currently shown in the body.
    #[must_use]
    pub fn current(&self) -> &TopicId {
        &self.current
    }

    /// The open topic, or `None` on the synthetic keyboard page.
    #[must_use]
    pub fn topic(&self) -> Option<&'static Topic> {
        norte_help::topic(self.lang, self.current.as_str())
    }

    /// The sidebar rows after the filter.
    #[must_use]
    pub fn rows(&self) -> &[SidebarRow] {
        &self.rows
    }

    /// Index into [`Self::rows`] of the selected entry. Always a
    /// `SidebarRow::Topic` while any topic is visible.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Which half has the cursor.
    #[must_use]
    pub fn focus(&self) -> Focus {
        self.focus
    }

    /// Actions of the open topic, in body order: its declared commands first,
    /// then its `see_also` links.
    #[must_use]
    pub fn actions(&self) -> &[Action] {
        &self.actions
    }

    /// The focused action, if the body has focus and has any.
    #[must_use]
    pub fn action(&self) -> Option<&Action> {
        (self.focus == Focus::Body)
            .then(|| self.actions.get(self.action_cursor))
            .flatten()
    }

    /// Index of the focused action (renderers highlight it).
    #[must_use]
    pub fn action_cursor(&self) -> usize {
        self.action_cursor
    }

    /// First body line to paint.
    #[must_use]
    pub fn body_scroll(&self) -> usize {
        self.body_scroll
    }

    /// Scrolls the body so that `line` is visible in a window of `height`
    /// lines. Called by the renderer, which is the only side that knows how
    /// tall the body is.
    pub fn reveal(&mut self, line: usize, height: usize) {
        let height = height.max(1);
        if line < self.body_scroll {
            self.body_scroll = line;
        } else if line >= self.body_scroll + height {
            self.body_scroll = line + 1 - height;
        }
    }

    /// Whether the filter editor is taking keystrokes.
    #[must_use]
    pub fn filtering(&self) -> bool {
        self.filtering
    }

    /// The filter as TYPED — raw, for matching.
    #[must_use]
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// The filter as PAINTED: hazards masked, same contract as
    /// `QuickSearch::query_display` (a pasted bidi override must not reach a
    /// terminal).
    #[must_use]
    pub fn filter_display(&self) -> String {
        norte_encoding::mask_terminal_hazards(&self.filter)
    }

    /// Starts the filter editor (`/`). Idempotent.
    pub fn start_filter(&mut self) {
        self.filtering = true;
        self.focus = Focus::Topics;
    }

    /// Leaves the filter editor, KEEPING what was typed: the reader narrowed
    /// the sidebar on purpose, and throwing it away on `Esc` would make the
    /// key that stops typing also undo the search.
    pub fn end_filter(&mut self) {
        self.filtering = false;
    }

    /// Appends a character to the filter.
    pub fn push_char(&mut self, c: char) {
        self.filter.push(c);
        self.rebuild_rows();
    }

    /// Removes the last character of the filter.
    pub fn backspace(&mut self) {
        self.filter.pop();
        self.rebuild_rows();
    }

    /// Opens `id` and pushes the previous topic onto the history.
    ///
    /// An id no corpus topic answers to (a dangling link, a plugin removed
    /// between frames) is IGNORED: no history entry, no change of body. The
    /// alternative — opening a blank page — loses the reader's place to
    /// punish them for someone else's broken link.
    pub fn open(&mut self, id: &TopicId) {
        if id == &self.current {
            return;
        }
        if !self.exists(id) {
            return;
        }
        self.history.push(self.current.clone());
        self.set_current(id.clone());
    }

    /// Goes back one step. `false` when there is nowhere to go.
    pub fn back(&mut self) -> bool {
        let Some(prev) = self.history.pop() else {
            return false;
        };
        self.set_current(prev);
        true
    }

    /// Moves the cursor down: the next topic in the sidebar, or the next
    /// action in the body.
    pub fn down(&mut self) {
        match self.focus {
            Focus::Topics => self.step_sidebar(1),
            Focus::Body => {
                self.action_cursor = (self.action_cursor + 1)
                    .min(self.actions.len().saturating_sub(1));
            }
        }
    }

    /// Moves the cursor up.
    pub fn up(&mut self) {
        match self.focus {
            Focus::Topics => self.step_sidebar(-1),
            Focus::Body => self.action_cursor = self.action_cursor.saturating_sub(1),
        }
    }

    /// Pages down (`n` rows), same targets as [`Self::down`].
    pub fn page_down(&mut self, n: usize) {
        match self.focus {
            Focus::Topics => {
                for _ in 0..n {
                    self.step_sidebar(1);
                }
            }
            Focus::Body => self.body_scroll = self.body_scroll.saturating_add(n),
        }
    }

    /// Pages up (`n` rows).
    pub fn page_up(&mut self, n: usize) {
        match self.focus {
            Focus::Topics => {
                for _ in 0..n {
                    self.step_sidebar(-1);
                }
            }
            Focus::Body => self.body_scroll = self.body_scroll.saturating_sub(n),
        }
    }

    /// Switches focus between sidebar and body. Focusing an empty body is a
    /// no-op — a focus with nothing to move through looks like a freeze.
    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Topics if !self.actions.is_empty() => Focus::Body,
            Focus::Topics => Focus::Topics,
            Focus::Body => Focus::Topics,
        };
    }

    /// The topic under the sidebar cursor, if the cursor is on one.
    #[must_use]
    pub fn selected_topic(&self) -> Option<&TopicId> {
        match self.rows.get(self.cursor) {
            Some(SidebarRow::Topic { id, .. }) => Some(id),
            _ => None,
        }
    }

    /// Opens whatever the sidebar cursor is on (the sidebar's `Enter`).
    pub fn open_selected(&mut self) {
        if let Some(id) = self.selected_topic().cloned() {
            self.open(&id);
        }
    }

    fn exists(&self, id: &TopicId) -> bool {
        id.as_str() == KEYS_ID || norte_help::topic(self.lang, id.as_str()).is_some()
    }

    fn set_current(&mut self, id: TopicId) {
        self.current = id;
        self.focus = Focus::Topics;
        self.body_scroll = 0;
        self.action_cursor = 0;
        self.rebuild_actions();
        self.sync_cursor_to_current();
    }

    /// Moves the sidebar cursor by `delta` rows, SKIPPING group headers, and
    /// opens what it lands on: the sidebar is a preview, exactly like the
    /// theme picker (moving the cursor already shows the theme).
    fn step_sidebar(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let mut i = self.cursor;
        for _ in 0..self.rows.len() {
            let next = i as isize + delta;
            if next < 0 || next as usize >= self.rows.len() {
                return; // stop at the ends; no wrap
            }
            i = next as usize;
            if matches!(self.rows[i], SidebarRow::Topic { .. }) {
                self.cursor = i;
                let id = match &self.rows[i] {
                    SidebarRow::Topic { id, .. } => id.clone(),
                    SidebarRow::Group { .. } => unreachable!("just matched"),
                };
                if id != self.current {
                    self.history.push(self.current.clone());
                    self.set_current(id);
                }
                return;
            }
        }
    }

    fn sync_cursor_to_current(&mut self) {
        if let Some(i) = self.rows.iter().position(|r| {
            matches!(r, SidebarRow::Topic { id, .. } if id == &self.current)
        }) {
            self.cursor = i;
        } else {
            self.cursor = self
                .rows
                .iter()
                .position(|r| matches!(r, SidebarRow::Topic { .. }))
                .unwrap_or(0);
        }
    }

    fn rebuild_actions(&mut self) {
        self.actions = match self.topic() {
            None => Vec::new(),
            Some(t) => t
                .commands
                .iter()
                .map(|c| Action::Run(c.clone()))
                .chain(t.see_also.iter().cloned().map(Action::Open))
                .collect(),
        };
        self.action_cursor = 0;
        if self.actions.is_empty() {
            self.focus = Focus::Topics;
        }
    }

    /// Rebuilds the sidebar: every topic of the corpus grouped by its first
    /// tag, in corpus order, plus the synthetic keyboard page, filtered by
    /// the folded query.
    fn rebuild_rows(&mut self) {
        let needle = crate::nav::fold(self.filter.as_bytes());
        let mut rows = Vec::new();
        let mut current_tag: Option<String> = None;
        for t in topics(self.lang) {
            if !Self::matches(t, &needle) {
                continue;
            }
            let tag = t.tags.first().cloned().unwrap_or_default();
            if current_tag.as_deref() != Some(tag.as_str()) {
                rows.push(SidebarRow::Group { tag: tag.clone() });
                current_tag = Some(tag);
            }
            rows.push(SidebarRow::Topic {
                id: t.id.clone(),
                title: t.title.clone(),
            });
        }
        if needle.is_empty() || crate::nav::fold(KEYS_ID.as_bytes()).contains(&needle) {
            rows.push(SidebarRow::Group {
                tag: KEYS_TAG.to_owned(),
            });
            rows.push(SidebarRow::Topic {
                id: TopicId::new(KEYS_ID),
                title: KEYS_ID.to_owned(),
            });
        }
        self.rows = rows;
        self.sync_cursor_to_current();
    }

    /// A topic matches when its id, its title or any command it documents
    /// contains the folded needle. Folded on BOTH sides (`nav::fold`), so a
    /// search for `arch` finds `Árchivos` — the same fold the quick search
    /// and the palette use.
    fn matches(t: &Topic, needle: &str) -> bool {
        if needle.is_empty() {
            return true;
        }
        crate::nav::fold(t.id.as_str().as_bytes()).contains(needle)
            || crate::nav::fold(t.title.as_bytes()).contains(needle)
            || t.commands
                .iter()
                .any(|c| crate::nav::fold(c.as_bytes()).contains(needle))
    }
}
```

Add the imports the tests need at the top of the test module:

```rust
    use norte_help::{Lang, TopicId};
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo nextest run -p norte-frontend help::`
Expected: PASS, 9 tests.

If `the_cursor_never_lands_on_a_group_header` fails at the very first `down()`,
the cause is `new()` leaving the cursor at row 0 (a header) — `rebuild_rows`
calls `sync_cursor_to_current`, which lands on `index`; check that the `index`
topic is the first topic of the corpus order.

- [ ] **Step 6: Lint and document**

Run: `cargo clippy -p norte-frontend --all-targets -- -D warnings`
Expected: clean. `norte-frontend` does not enable `missing_docs`, but every
public item above already carries rustdoc — keep it that way.

- [ ] **Step 7: Commit**

```bash
git add crates/norte-frontend/src/help.rs crates/norte-frontend/src/lib.rs crates/norte-frontend/Cargo.toml
git commit -m "feat(frontend): HelpState — the shared help navigation model (H3b)"
```

---

### Task 2: `TuiChords` — the TUI's `ChordResolver`

**Files:**
- Modify: `crates/norte-tui/src/help.rs`
- Modify: `crates/norte-tui/Cargo.toml` (promote `norte-help` from dev to a real dependency)

- [ ] **Step 1: Promote the dependency**

In `crates/norte-tui/Cargo.toml`, MOVE the `norte-help.workspace = true` line
out of `[dev-dependencies]` into `[dependencies]`, and delete the stale comment
above it ("El binario NO enlaza el corpus todavía — lo hará H3c"). Replace it
with:

```toml
# H3b: the binary now links the corpus — F1 paints it.
norte-help.workspace = true
```

- [ ] **Step 2: Write the failing tests**

Append to the `mod tests` block of `crates/norte-tui/src/help.rs`:

```rust
    use norte_help::{Availability, ChordResolver, render_command, CommandText};

    fn orthodox_resolver() -> TuiChords {
        let (_, preset) = presets()
            .into_iter()
            .find(|(n, _)| *n == "orthodox")
            .expect("preset orthodox");
        let known: Vec<&str> = COMMANDS
            .iter()
            .copied()
            .chain(DIALOG_COMMANDS.iter().copied())
            .collect();
        TuiChords::new(
            Effective::build_for(&preset, &[], &known, Screen::Browse).unwrap(),
            Effective::build_for(&preset, &[], &known, Screen::Viewer).unwrap(),
            Effective::build_for(&preset, &[], &known, Screen::Dialog).unwrap(),
            norte_i18n::Lang::En,
        )
    }

    #[test]
    fn resolves_a_browse_command_to_its_effective_chord() {
        let r = orthodox_resolver();
        assert_eq!(r.chord("pane.copy").as_deref(), Some("f5"));
    }

    #[test]
    fn resolves_commands_that_live_in_the_other_screens() {
        // The rule `ChordResolver::chord` documents: resolve a command in the
        // screen THAT COMMAND lives in. `viewer.*` is not in browse, and
        // `dialog.*` is in neither — a resolver that only asked browse would
        // report "no key" for two thirds of the vocabulary.
        let r = orthodox_resolver();
        assert!(r.chord("viewer.hex").is_some(), "viewer command");
        assert_eq!(r.chord("dialog.approve").as_deref(), Some("y"));
    }

    #[test]
    fn an_unbound_command_has_no_chord_and_none_is_invented() {
        let r = orthodox_resolver();
        assert_eq!(r.chord("no.such.command"), None);
        assert_eq!(
            render_command("no.such.command", &r),
            CommandText::Name("no.such.command".to_owned()),
            "with no chord and no catalogue entry the chain names the command"
        );
    }

    #[test]
    fn a_missing_catalogue_entry_returns_blank_not_the_lookup_key() {
        // The trap `ChordResolver::label` spells out: `t` echoes the id when
        // the catalogue misses, so the naive one-liner would paint
        // `help-cmd-no-such-command` at the reader and defeat the fallback
        // chain by never being blank.
        let r = orthodox_resolver();
        assert_eq!(r.label("no.such.command"), "");
        assert!(!r.label("pane.copy").is_empty(), "a real command has a label");
        assert!(
            !r.label("dialog.approve").is_empty(),
            "dialog verbs read from `dialog-cmd-*`, not `help-cmd-*`"
        );
    }

    #[test]
    fn a_hostile_chord_is_masked_before_it_reaches_the_page() {
        // Same duty as `dialog_hints` and the palette's chord column: a
        // project keymap layer can bind any codepoint, and this string is
        // painted.
        let (_, preset) = presets()
            .into_iter()
            .find(|(n, _)| *n == "orthodox")
            .expect("preset orthodox");
        for hazard in norte_testkit::corpus::hostile_chords() {
            let token = format!("\\u{:04X}", hazard.token as u32);
            let layer = crate::keymap::parse_keymap(&format!(
                "[pane]\nprepend_keymap = [{{ on = [\"{token}\"], run = \"pane.copy\" }}]\n"
            ))
            .unwrap();
            let browse =
                Effective::build_for(&preset, &[layer], COMMANDS, Screen::Browse).unwrap();
            let viewer = Effective::build_for(&preset, &[], COMMANDS, Screen::Viewer).unwrap();
            let dialog =
                Effective::build_for(&preset, &[], DIALOG_COMMANDS, Screen::Dialog).unwrap();
            let r = TuiChords::new(browse, viewer, dialog, norte_i18n::Lang::En);
            let chord = r.chord("pane.copy").expect("bound");
            assert!(
                !chord.chars().any(norte_encoding::is_terminal_hazard),
                "[{}] raw hazard in a painted chord: {chord:?}",
                hazard.id
            );
        }
    }

    #[test]
    fn every_command_is_available_in_this_phase() {
        // H3d wires the real sources. Pinned so the day it changes, this
        // assertion is the one that says where the claim used to live.
        let r = orthodox_resolver();
        assert_eq!(r.availability("pane.copy"), Availability::Available);
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo nextest run -p norte-tui help::`
Expected: FAIL — `cannot find type TuiChords in this scope`.

- [ ] **Step 4: Write the implementation**

Append to `crates/norte-tui/src/help.rs` (keep `build` exactly as it is — the
keyboard page still renders from it):

```rust
use std::collections::HashSet;

use norte_help::{Availability, ChordResolver};

use crate::keymap::{DIALOG_COMMANDS, dialog_hint_id};
use crate::palette::first_chord;

/// The TUI's answer to the three questions `norte-help` asks a frontend
/// (H3b): the user's effective chord, a short label, and whether the command
/// can run now.
///
/// Built once per help session and rebuilt on hot reload, like `help_lines`
/// and `DialogHints` — a rebind must change the page, and it does because the
/// page is drawn through this.
pub struct TuiChords {
    browse: Effective,
    viewer: Effective,
    dialog: Effective,
    /// Message ids the catalogue actually has, so `label` can return blank on
    /// a miss instead of echoing its lookup key (see [`ChordResolver::label`]
    /// — `t` returns the id when the message is absent). A set built once
    /// rather than a `message_ids` scan per row.
    catalogue: HashSet<String>,
}

impl TuiChords {
    /// Builds the resolver from the three effective keymaps and the locale.
    #[must_use]
    pub fn new(
        browse: Effective,
        viewer: Effective,
        dialog: Effective,
        lang: norte_i18n::Lang,
    ) -> Self {
        Self {
            browse,
            viewer,
            dialog,
            catalogue: norte_i18n::message_ids(lang).into_iter().collect(),
        }
    }

    /// Fluent id of a command's short label: `dialog.*` verbs live in
    /// `dialog-cmd-*` and everything else in `help-cmd-*` — the two
    /// catalogues the app already keeps (`#113`).
    fn label_id(command: &str) -> String {
        if command.starts_with("dialog.") {
            dialog_hint_id(command)
        } else {
            help_id(command)
        }
    }
}

impl ChordResolver for TuiChords {
    /// The command's chord in the screen it belongs to, masked.
    ///
    /// The or-chain IS the rule `ChordResolver::chord` states: a `viewer.*`
    /// command is not in the browse keymap and a `dialog.*` verb is in
    /// neither, so asking one screen would report "no key bound" for most of
    /// the vocabulary. `first_chord` masks (encoding audit H1) — a project
    /// keymap layer can bind any codepoint and this string is painted.
    fn chord(&self, command: &str) -> Option<String> {
        first_chord(command, &self.browse)
            .or_else(|| first_chord(command, &self.viewer))
            .or_else(|| first_chord(command, &self.dialog))
    }

    fn label(&self, command: &str) -> String {
        let id = Self::label_id(command);
        if self.catalogue.contains(&id) {
            t(&id)
        } else {
            // Blank ON PURPOSE: `norte_help::render_command`'s chain then
            // names the command, which is searchable, instead of painting
            // `help-cmd-…` at the reader.
            String::new()
        }
    }

    fn availability(&self, _command: &str) -> Availability {
        // H3d wires the real sources (backend capabilities, plugin state,
        // policy scope). Until then every row is offered, which is what the
        // app does today anyway — this phase changes how help is NAVIGATED,
        // not what it claims about the world.
        Availability::Available
    }
}
```

Extend the existing `use` line at the top of the file to
`use crate::keymap::{Effective, dialog_hint_id, help_id};` if `dialog_hint_id`
is not already imported there (it is, from `build`).

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo nextest run -p norte-tui help::`
Expected: PASS — the six new tests plus the pre-existing
`la_ayuda_incluye_los_verbos_dialog`.

If `resolves_a_browse_command_to_its_effective_chord` reports `Some("F5")`
instead of `Some("f5")`, adjust the expectation to whatever `Chord`'s
`Display` produces — do not change `first_chord`.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-tui/src/help.rs crates/norte-tui/Cargo.toml
git commit -m "feat(tui): ChordResolver over the effective keymap and Fluent (H3b)"
```

---

### Task 3: three new `dialog.*` verbs, presets, labels and allowlist

**Files:**
- Modify: `crates/norte-tui/src/keymap.rs:157-177` (`DIALOG_COMMANDS`)
- Modify: `crates/norte-frontend/presets/keymap/{orthodox,vim,cua}.toml`
- Modify: `crates/norte-i18n/i18n/en.ftl`, `crates/norte-i18n/i18n/es.ftl`
- Modify: `crates/norte-tui/src/app.rs` (add `ALLOW_HELP` next to the other allowlists, ~line 2880)
- Modify: `crates/norte-tui/src/hints.rs` (`DialogHints::help`)

- [ ] **Step 1: Write the failing test**

Add to `crates/norte-tui/src/hints.rs`'s `mod tests`:

```rust
    /// H3b: the help overlay's footer is GENERATED like every other
    /// overlay's — the three verbs it adds must reach it with their chords.
    #[test]
    fn el_hint_de_la_ayuda_lista_sus_verbos_propios() {
        let (_, preset) = crate::keymap::presets()
            .into_iter()
            .find(|(n, _)| *n == "orthodox")
            .expect("preset orthodox");
        let eff = crate::keymap::Effective::build_for(
            &preset,
            &[],
            crate::keymap::DIALOG_COMMANDS,
            Screen::Dialog,
        )
        .unwrap();
        let hints = DialogHints::build(&eff);
        for cmd in ["dialog.filter", "dialog.back", "dialog.pane"] {
            assert!(
                hints.help.contains(&t(&crate::keymap::dialog_hint_id(cmd))),
                "{cmd} debe aparecer en el pie de la ayuda: {}",
                hints.help
            );
        }
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo nextest run -p norte-tui hints::`
Expected: FAIL — `no field help on type DialogHints`.

- [ ] **Step 3: Extend the vocabulary**

In `crates/norte-tui/src/keymap.rs`, append to `DIALOG_COMMANDS` (after
`"dialog.cycle-format"`):

```rust
    // H3b — verbs the help overlay adds. They are `dialog.*` and not
    // `app.*` because they only mean anything inside an overlay: "the other
    // pane of this overlay", "the page I came from", "start filtering this
    // list". Documented in the `help` topic, so the documentation gate is
    // paid in the same change that introduces them.
    "dialog.pane",
    "dialog.back",
    "dialog.filter",
```

- [ ] **Step 4: Bind them in the three presets**

In `crates/norte-frontend/presets/keymap/orthodox.toml`, inside `[dialog]`,
before the closing `]`:

```toml
    { on = ["tab"], run = "dialog.pane" },
    { on = ["backspace"], run = "dialog.back" },
    { on = ["/"], run = "dialog.filter" },
```

In `crates/norte-frontend/presets/keymap/cua.toml`, the same three lines inside
its `[dialog]` section.

In `crates/norte-frontend/presets/keymap/vim.toml`, the same three lines plus
the vim-idiomatic alias:

```toml
    { on = ["tab"], run = "dialog.pane" },
    { on = ["backspace"], run = "dialog.back" },
    { on = ["/"], run = "dialog.filter" },
    { on = ["ctrl+o"], run = "dialog.back" },
```

- [ ] **Step 5: Add the Fluent labels**

`crates/norte-i18n/i18n/en.ftl`, next to the other `dialog-cmd-*` entries:

```
dialog-cmd-pane = other pane
dialog-cmd-back = back
dialog-cmd-filter = filter
```

`crates/norte-i18n/i18n/es.ftl`:

```
dialog-cmd-pane = otro panel
dialog-cmd-back = atrás
dialog-cmd-filter = filtrar
```

- [ ] **Step 6: Add the allowlist and the hint**

In `crates/norte-tui/src/app.rs`, after `ALLOW_NAV_HOTLIST`:

```rust
/// Verbs the help overlay dispatches (H3b). Navigation, confirm (run the
/// focused row or follow the focused link), cancel (close), plus its own
/// three. Nothing that mutates: the overlay itself changes no files — a
/// command it RUNS goes through the normal dispatch, with its own
/// confirmation, gate and journal entry.
pub const ALLOW_HELP: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.page-up",
    "dialog.page-down",
    "dialog.confirm",
    "dialog.cancel",
    "dialog.pane",
    "dialog.back",
    "dialog.filter",
];
```

In `crates/norte-tui/src/hints.rs`, add the field to `DialogHints`:

```rust
    /// Overlay de ayuda (`App::help`, H3b).
    pub help: String,
```

and to `build`, in the non-modal group:

```rust
            help: dialog_hints(&without_navigation(ALLOW_HELP), eff),
```

adding `ALLOW_HELP` to the `use crate::app::{…}` import list in `build`.

- [ ] **Step 7: Run the tests**

Run: `cargo nextest run -p norte-tui hints:: keymap::`
Expected: PASS. If a preset test fails with "comando desconocido", the verb is
missing from `DIALOG_COMMANDS` — presets are validated against it.

Run: `cargo nextest run -p norte-i18n`
Expected: PASS — the parity suite requires every id in both locales.

- [ ] **Step 8: Commit**

```bash
git add crates/norte-tui/src/keymap.rs crates/norte-tui/src/app.rs crates/norte-tui/src/hints.rs crates/norte-frontend/presets/keymap crates/norte-i18n/i18n
git commit -m "feat(keymap,i18n): dialog.pane/back/filter for the help overlay (H3b)"
```

---

### Task 4: the `help` topic — paying for the new verbs at the gate

**Files:**
- Create: `crates/norte-help/topics/en/help.md`
- Create: `crates/norte-help/topics/es/help.md`
- Modify: `crates/norte-help/src/corpus.rs` (embed list, if it is explicit)
- Modify: `crates/norte-tui/tests/help_gate.rs:59-122` (`PENDIENTES` shrinks)

- [ ] **Step 1: Check how topics are embedded**

Run: `grep -n "include_str\|EN_TOPICS\|\.md" crates/norte-help/src/corpus.rs`
Expected: an explicit list of `include_str!` entries per locale. Add
`help.md` to BOTH locale lists in the same order the file appears below (the
sidebar order is the corpus order).

- [ ] **Step 2: Write the failing test**

The gate is the test. Add `"dialog.pane"`, `"dialog.back"`, `"dialog.filter"`
to NOTHING — do not touch `PENDIENTES` yet — and run:

Run: `cargo nextest run -p norte-tui --test help_gate`
Expected: FAIL —
`comandos sin tema y sin entrada en PENDIENTES … dialog.pane, dialog.back, dialog.filter`.
This is the gate doing its job: a new command costs a paragraph.

- [ ] **Step 3: Write the English topic**

Create `crates/norte-help/topics/en/help.md`:

```markdown
+++
id = "help"
title = "Reading this help"
tags = ["basics"]
see_also = ["index", "panes"]
commands = [
    "app.help",
    "app.palette",
    "dialog.filter",
    "dialog.pane",
    "dialog.back",
]
+++
{{cmd:app.help}} opens this overlay from anywhere. The list on the left is
every page; the panel on the right is the page you are on.

{{cmd:dialog.pane}} moves between the two: on the left, up and down change the
page; on the right, they walk the runnable rows and the links at the bottom.
Enter on a runnable row closes the help and runs the command exactly as its own
key would — the same confirmation, the same policy gate, the same journal
entry. Enter on a link follows it, and {{cmd:dialog.back}} returns to the page
you came from.

{{cmd:dialog.filter}} starts filtering the list on the left. It matches page
titles, page names and the commands each page documents, so typing `copy`
finds the copying page whether or not the word appears in its title.

# The keys here are yours

No key in these pages is written into the text. Each one is looked up in your
effective keymap as the page is drawn, so a rebind changes the prose. A command
with no key at all names itself instead of claiming one.

The last entry in the list, *keys*, is the other direction: the whole effective
keymap, generated, including the dialog verbs that overlay footers leave out
for want of width.

> 💡 {{cmd:app.palette}} is the fast version of the same model: type, Enter,
> gone. This help is the version that explains.
```

- [ ] **Step 4: Write the Spanish topic**

Create `crates/norte-help/topics/es/help.md` with the SAME `id`, `commands`
and `see_also` (locale parity is structural — the prose may differ, the
structure may not):

```markdown
+++
id = "help"
title = "Cómo leer esta ayuda"
tags = ["basics"]
see_also = ["index", "panes"]
commands = [
    "app.help",
    "app.palette",
    "dialog.filter",
    "dialog.pane",
    "dialog.back",
]
+++
{{cmd:app.help}} abre esta ventana desde cualquier sitio. La lista de la
izquierda son todas las páginas; el panel de la derecha es la página en la que
estás.

{{cmd:dialog.pane}} cambia de una a otra: a la izquierda, arriba y abajo
cambian de página; a la derecha, recorren las filas ejecutables y los enlaces
del final. Enter sobre una fila ejecutable cierra la ayuda y lanza el comando
igual que lo haría su tecla — la misma confirmación, la misma política, la
misma entrada en el diario. Enter sobre un enlace lo sigue, y
{{cmd:dialog.back}} vuelve a la página anterior.

{{cmd:dialog.filter}} filtra la lista de la izquierda. Busca en el título, en
el nombre de la página y en los comandos que cada página documenta, así que
escribir `copy` encuentra la página de copiar aunque la palabra no salga en su
título.

# Las teclas de estas páginas son las tuyas

Ninguna tecla está escrita en el texto. Cada una se consulta en tu keymap
efectivo mientras se pinta la página, así que un rebind cambia la prosa. Un
comando sin tecla se nombra a sí mismo en vez de inventarse una.

La última entrada de la lista, *keys*, es la dirección contraria: el keymap
efectivo entero, generado, incluidos los verbos de diálogo que los pies de los
overlays se dejan por falta de ancho.

> 💡 {{cmd:app.palette}} es la versión rápida del mismo modelo: teclear, Enter,
> fuera. Esta ayuda es la versión que explica.
```

- [ ] **Step 5: Shrink the allowlist**

In `crates/norte-tui/tests/help_gate.rs`, DELETE these two lines from
`PENDIENTES` (the new topic documents them, so they are now `StaleAllowEntry`):

```rust
    "app.help",
    "app.palette",
```

and lower the ceiling in the same diff:

```rust
const _: () = assert!(
    PENDIENTES.len() <= 47,
    "la allowlist de la puerta de documentación solo puede MENGUAR: \
     documenta el comando en vez de añadirlo aquí"
);
```

- [ ] **Step 6: Run the gate and the corpus suite**

Run: `cargo nextest run -p norte-help -p norte-tui --test help_gate`
Expected: PASS — corpus integrity (locale parity, links, ids), no unknown
commands, no undocumented commands, no stale allowlist entries.

If it reports `StaleAllowEntry` for something else, that entry is now covered
too: delete it and lower the ceiling by the same amount.

- [ ] **Step 7: Commit**

```bash
git add crates/norte-help/topics crates/norte-help/src/corpus.rs crates/norte-tui/tests/help_gate.rs
git commit -m "docs(help): the topic that documents the help overlay (H3b)"
```

---

### Task 5: the block renderer

**Files:**
- Create: `crates/norte-tui/src/help_render.rs`
- Modify: `crates/norte-tui/src/lib.rs` (add `pub mod help_render;`)

- [ ] **Step 1: Write the failing tests**

Create `crates/norte-tui/src/help_render.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use norte_help::{Availability, ChordResolver, Lang, topic};

    struct Fake;

    impl ChordResolver for Fake {
        fn chord(&self, command: &str) -> Option<String> {
            (command == "pane.copy").then(|| "f5".to_owned())
        }
        fn label(&self, command: &str) -> String {
            format!("do {command}")
        }
        fn availability(&self, _command: &str) -> Availability {
            Availability::Available
        }
    }

    fn text_of(lines: &[ratatui::text::Line<'_>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_command_mark_becomes_the_users_chord_in_the_prose() {
        let t = topic(Lang::En, "copying").expect("copying");
        let out = render_topic(t, &Fake, 60, &norte_theme::Theme::default().into());
        let text = text_of(&out.lines);
        assert!(text.contains("f5"), "the mark resolved to the chord: {text}");
        assert!(
            !text.contains("{{cmd:"),
            "no raw mark survives to the screen: {text}"
        );
    }

    #[test]
    fn wrapping_respects_terminal_cells_not_chars() {
        // A CJK paragraph is two cells per char: budgeting by chars overflows
        // the width and ratatui trims the tail (#79's lesson, applied here).
        let wide = "漢字".repeat(40);
        let block = norte_help::Block::Paragraph(vec![norte_help::Span::Text(wide)]);
        let lines = render_block(&block, &Fake, 20, &norte_theme::Theme::default().into());
        for l in &lines {
            let w: usize = l
                .spans
                .iter()
                .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
                .sum();
            assert!(w <= 20, "line of {w} cells in a 20-cell body");
        }
    }

    #[test]
    fn command_rows_map_to_the_lines_they_are_painted_on() {
        // The action map is what lets the body scroll follow the cursor: an
        // action nobody can locate cannot be revealed.
        let t = topic(Lang::En, "copying").expect("copying");
        let out = render_topic(t, &Fake, 60, &norte_theme::Theme::default().into());
        assert_eq!(
            out.action_lines.len(),
            t.commands.len() + t.see_also.len(),
            "one line index per action, commands then links"
        );
        assert!(
            out.action_lines.windows(2).all(|w| w[0] < w[1]),
            "in painting order: {:?}",
            out.action_lines
        );
        assert!(
            *out.action_lines.last().expect("some actions") < out.lines.len(),
            "every action points at a line that exists"
        );
    }

    #[test]
    fn a_table_row_shorter_than_its_header_does_not_panic() {
        // The parser normalises rows to the header length; this pins that the
        // renderer relies on it rather than re-checking, and that a corpus
        // change breaking it is caught here and not in a user's terminal.
        let block = norte_help::Block::Table {
            header: vec!["a".to_owned(), "b".to_owned()],
            rows: vec![vec!["1".to_owned(), "2".to_owned()]],
        };
        let lines = render_block(&block, &Fake, 20, &norte_theme::Theme::default().into());
        assert_eq!(lines.len(), 2, "header plus one row");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p norte-tui help_render::`
Expected: FAIL — `cannot find function render_topic in this scope`.

- [ ] **Step 3: Write the implementation**

Above the tests in `crates/norte-tui/src/help_render.rs`:

```rust
//! Painting a `norte-help` topic with ratatui (H3b).
//!
//! The corpus hands over a closed vocabulary of blocks and spans (ADR 0040)
//! and no layout at all; everything about width, colour and emphasis is
//! decided here, which is what lets the same topic render in the GUI (H3f)
//! and as plain text (H3g) without a second corpus.
//!
//! Masking: a built-in topic is TRUSTED text (it ships in the binary and the
//! documentation gate cross-checks its command ids), a plugin topic arrives
//! ALREADY masked and bounded from `parse_untrusted`, and chords are masked
//! by the resolver (`first_chord`). This module therefore masks nothing —
//! and that is a claim about its inputs, so a renderer fed a corpus that has
//! not been through the gate must mask before calling in.

use norte_help::{Block, Callout, ChordResolver, Span, Topic, render_command, rows_of};
use norte_theme::Role;
use ratatui::style::Style;
use ratatui::text::{Line, Span as TSpan};
use unicode_width::UnicodeWidthStr;

use crate::theme::TuiTheme;

/// A rendered topic: the lines to paint, and where each action landed.
pub struct Rendered<'a> {
    /// Body lines, in order.
    pub lines: Vec<Line<'a>>,
    /// Line index of each action of `HelpState::actions`, in the same order
    /// (commands first, then `see_also` links). Lets the caller scroll the
    /// body so the focused action is visible.
    pub action_lines: Vec<usize>,
}

/// Renders a whole topic into `width` cells.
#[must_use]
pub fn render_topic<'a>(
    topic: &'a Topic,
    r: &(impl ChordResolver + ?Sized),
    width: usize,
    theme: &TuiTheme,
) -> Rendered<'a> {
    let mut lines: Vec<Line<'a>> = Vec::new();
    lines.push(Line::from(TSpan::styled(
        topic.title.clone(),
        theme.role(Role::Title),
    )));
    lines.push(Line::raw("─".repeat(width.min(80))));
    for block in &topic.blocks {
        lines.extend(render_block(block, r, width, theme));
        lines.push(Line::raw(""));
    }

    let mut action_lines = Vec::new();
    if !topic.commands.is_empty() {
        for row in rows_of(topic, r) {
            action_lines.push(lines.len());
            let chord = row.chord.unwrap_or_else(|| "—".to_owned());
            let style = if row.row.avail.is_available() {
                theme.role(Role::Regular)
            } else {
                theme.role(Role::Info)
            };
            lines.push(Line::from(vec![
                TSpan::styled(format!("  {chord:<10}"), theme.role(Role::Mark)),
                TSpan::styled(row.label, style),
            ]));
        }
        lines.push(Line::raw(""));
    }
    for id in &topic.see_also {
        action_lines.push(lines.len());
        lines.push(Line::from(TSpan::styled(
            format!("  [[{id}]]"),
            theme.role(Role::Info),
        )));
    }
    Rendered {
        lines,
        action_lines,
    }
}

/// Renders one block into `width` cells.
#[must_use]
pub fn render_block<'a>(
    block: &'a Block,
    r: &(impl ChordResolver + ?Sized),
    width: usize,
    theme: &TuiTheme,
) -> Vec<Line<'a>> {
    match block {
        Block::Heading { level, text } => vec![Line::from(TSpan::styled(
            format!("{} {text}", "#".repeat(usize::from(*level))),
            theme.role(Role::Title),
        ))],
        Block::Paragraph(spans) => wrap(&styled(spans, r, theme), width),
        Block::Bullets(items) => items
            .iter()
            .flat_map(|spans| {
                let mut out = wrap(&styled(spans, r, theme), width.saturating_sub(2));
                if let Some(first) = out.first_mut() {
                    first.spans.insert(0, TSpan::raw("• "));
                }
                for rest in out.iter_mut().skip(1) {
                    rest.spans.insert(0, TSpan::raw("  "));
                }
                out
            })
            .collect(),
        Block::Code { text, .. } => text
            .lines()
            .map(|l| Line::from(TSpan::styled(format!("  {l}"), theme.role(Role::Mark))))
            .collect(),
        Block::Table { header, rows } => {
            let mut out = vec![Line::from(TSpan::styled(
                header.join("  "),
                theme.role(Role::Title),
            ))];
            // The parser guarantees every row has `header.len()` cells
            // (`Block::Table`'s contract), so this joins without checking.
            out.extend(rows.iter().map(|row| Line::raw(row.join("  "))));
            out
        }
        Block::Callout { kind, spans } => {
            let (mark, role) = match kind {
                Callout::Note => ("ℹ", Role::Info),
                Callout::Warn => ("⚠", Role::Warning),
                Callout::Tip => ("💡", Role::Info),
            };
            let mut out = wrap(&styled(spans, r, theme), width.saturating_sub(2));
            if let Some(first) = out.first_mut() {
                first.spans.insert(0, TSpan::styled(format!("{mark} "), theme.role(role)));
            }
            out
        }
    }
}

/// Turns spans into styled ratatui spans, resolving the live marks.
fn styled<'a>(
    spans: &'a [Span],
    r: &(impl ChordResolver + ?Sized),
    theme: &TuiTheme,
) -> Vec<TSpan<'a>> {
    spans
        .iter()
        .map(|s| match s {
            Span::Text(t) => TSpan::raw(t.clone()),
            Span::Strong(t) => TSpan::styled(t.clone(), theme.role(Role::Title)),
            Span::Emph(t) => TSpan::styled(t.clone(), theme.role(Role::Info)),
            Span::Code(t) => TSpan::styled(t.clone(), theme.role(Role::Mark)),
            Span::CommandRef(c) => {
                // A key is painted as a key and a name as prose — the
                // distinction `CommandText` exists to preserve. Flattening
                // them would make every mark look like a chord, including
                // the ones that are not.
                let text = render_command(c, r);
                let role = if text.is_chord() { Role::Mark } else { Role::Info };
                TSpan::styled(text.into_text(), theme.role(role))
            }
            Span::TopicLink(id) => {
                TSpan::styled(format!("[[{id}]]"), theme.role(Role::Info))
            }
        })
        .collect()
}

/// Wraps styled spans into lines of at most `width` CELLS.
///
/// Cells and not chars: CJK and emoji are two columns wide, and budgeting by
/// chars overflows the body, after which ratatui trims the tail (the defect
/// #79 fixed in the middle ellipsis, in a different place).
fn wrap<'a>(spans: &[TSpan<'a>], width: usize) -> Vec<Line<'a>> {
    let width = width.max(1);
    let mut lines: Vec<Line<'a>> = Vec::new();
    let mut current: Vec<TSpan<'a>> = Vec::new();
    let mut used = 0usize;
    for span in spans {
        for word in split_keeping_spaces(span.content.as_ref()) {
            let w = UnicodeWidthStr::width(word.as_str());
            if used + w > width && used > 0 {
                lines.push(Line::from(std::mem::take(&mut current)));
                used = 0;
                if word.trim().is_empty() {
                    continue; // a space never starts a line
                }
            }
            used += w;
            current.push(TSpan::styled(word, span.style));
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(Line::from(current));
    }
    lines
}

/// Splits into words WITH their trailing spaces, so wrapping never joins two
/// words that were separate.
fn split_keeping_spaces(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        cur.push(c);
        if c.is_whitespace() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}
```

Add to `crates/norte-tui/src/lib.rs`:

```rust
pub mod help_render;
```

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p norte-tui help_render::`
Expected: PASS, 4 tests.

If `wrapping_respects_terminal_cells_not_chars` fails on a CJK run with no
spaces, `split_keeping_spaces` returns one enormous word — split words longer
than `width` at a cell boundary inside `wrap` before pushing them, and add a
test line asserting the split.

- [ ] **Step 5: Lint**

Run: `cargo clippy -p norte-tui --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-tui/src/help_render.rs crates/norte-tui/src/lib.rs
git commit -m "feat(tui): ratatui renderer for help blocks, cell-aware (H3b)"
```

---

### Task 6: `HelpView` in `App`, and the new `draw_help`

**Files:**
- Modify: `crates/norte-tui/src/app.rs:2504-2521` (replace `Help`)
- Modify: `crates/norte-tui/src/ui.rs:889-914` (rewrite `draw_help`)

- [ ] **Step 1: Write the failing test**

Add to `crates/norte-tui/src/app.rs`'s `mod tests`:

```rust
    /// H3b: the overlay's state is the shared model plus what only the TUI
    /// needs (the generated keyboard page). Opening it lands on the index.
    #[test]
    fn el_overlay_de_ayuda_abre_en_el_indice() {
        let view = HelpView::new(norte_help::Lang::En, vec!["── keys ──".to_owned()]);
        assert_eq!(view.state.current().as_str(), "index");
        assert_eq!(view.keys_lines.len(), 1);
    }

    /// The dialog verbs the overlay accepts, and no others: `dialog.remove`
    /// (the extension manager's "uninstall") must not resolve to anything
    /// here.
    #[test]
    fn help_action_solo_acepta_su_allowlist() {
        assert!(help_action("dialog.cancel").is_some());
        assert!(help_action("dialog.filter").is_some());
        assert_eq!(help_action("dialog.remove"), None);
        for cmd in crate::keymap::DIALOG_COMMANDS {
            if !ALLOW_HELP.contains(cmd) {
                assert_eq!(help_action(cmd), None, "{cmd} fuera del allowlist");
            }
        }
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p norte-tui app::tests::el_overlay`
Expected: FAIL — `cannot find type HelpView in this scope`.

- [ ] **Step 3: Replace `Help` with `HelpView`**

In `crates/norte-tui/src/app.rs`, replace the whole `pub struct Help` block and
its `impl` (lines 2504-2521) with:

```rust
/// State of the help overlay (H3b): the shared navigation model plus the one
/// thing only this frontend can produce — the generated keyboard page.
#[derive(Debug, Clone)]
pub struct HelpView {
    /// Sidebar, body, filter, history and focus.
    pub state: norte_frontend::help::HelpState,
    /// The effective-keymap cheatsheet (`help::build`), painted as the body
    /// of the synthetic `keys` entry. Rebuilt on hot reload with everything
    /// else derived from the keymap.
    pub keys_lines: Vec<String>,
}

impl HelpView {
    /// Opens the overlay on the index.
    #[must_use]
    pub fn new(lang: norte_help::Lang, keys_lines: Vec<String>) -> Self {
        Self {
            state: norte_frontend::help::HelpState::new(lang),
            keys_lines,
        }
    }

    /// `true` while the body shows the generated keyboard page rather than a
    /// corpus topic.
    #[must_use]
    pub fn on_keys_page(&self) -> bool {
        self.state.current().as_str() == norte_frontend::help::KEYS_ID
    }
}

/// What a `dialog.*` verb means inside the help overlay. `None` for a verb
/// the overlay does not support — the SAME allowlist that generates its
/// footer hint ([`ALLOW_HELP`]), never a second copy.
#[must_use]
pub fn help_action(cmd: &str) -> Option<HelpOutcome> {
    if !ALLOW_HELP.contains(&cmd) {
        return None;
    }
    Some(match cmd {
        "dialog.up" => HelpOutcome::Up,
        "dialog.down" => HelpOutcome::Down,
        "dialog.page-up" => HelpOutcome::PageUp,
        "dialog.page-down" => HelpOutcome::PageDown,
        "dialog.confirm" => HelpOutcome::Activate,
        "dialog.cancel" => HelpOutcome::Close,
        "dialog.pane" => HelpOutcome::TogglePane,
        "dialog.back" => HelpOutcome::Back,
        "dialog.filter" => HelpOutcome::StartFilter,
        _ => return None,
    })
}

/// The overlay's verbs, resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpOutcome {
    /// Previous row (sidebar) or action (body).
    Up,
    /// Next row or action.
    Down,
    /// A page up.
    PageUp,
    /// A page down.
    PageDown,
    /// Run the focused command row, or follow the focused link.
    Activate,
    /// Close the overlay.
    Close,
    /// Switch focus between sidebar and body.
    TogglePane,
    /// Back to the previous page.
    Back,
    /// Start the filter editor.
    StartFilter,
}
```

Then fix the field declaration at `app.rs:739`:

```rust
    /// Help overlay (F1), H3b.
    pub help: Option<HelpView>,
```

Run: `cargo check -p norte-tui 2>&1 | head -40` and fix every call site the
compiler names (they are the ones Task 7 rewrites anyway; a temporary
`app.help = None` is acceptable ONLY inside this task, and Task 7 removes it).

- [ ] **Step 4: Rewrite `draw_help`**

Replace `crates/norte-tui/src/ui.rs:889-914` with:

```rust
/// Help overlay (H3b): sidebar of topics on the left, the open page on the
/// right, a generated footer, and the filter editor at the bottom when it is
/// active. Nothing here is masked, and that is a claim about the inputs: the
/// corpus is trusted text, a plugin topic was masked by `parse_untrusted`,
/// the chords were masked by `first_chord`, and the filter is painted through
/// `HelpState::filter_display`.
fn draw_help(frame: &mut Frame<'_>, help: &crate::app::HelpView, theme: &TuiTheme, hint: &str) {
    use norte_frontend::help::{Focus, SidebarRow};

    let area = centered(
        frame.area(),
        frame.area().width.saturating_sub(4).max(20),
        frame.area().height.saturating_sub(2).max(6),
    );
    clear_themed(frame, area, theme);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", t("help-title")))
            .title_style(theme.role(Role::Title))
            .border_style(theme.role(Role::ModalBorder)),
        area,
    );
    let inner = area.inner(ratatui::layout::Margin::new(1, 1));
    let cols = Layout::horizontal([Constraint::Length(24), Constraint::Min(20)]).split(inner);
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(cols[1]);
    let body_area = rows[0];

    // Sidebar.
    let side: Vec<Line<'_>> = help
        .state
        .rows()
        .iter()
        .enumerate()
        .map(|(i, row)| match row {
            SidebarRow::Group { tag } => Line::from(TSpan::styled(
                t(&format!("help-group-{tag}")),
                theme.role(Role::Title),
            )),
            SidebarRow::Topic { title, .. } => {
                let selected = i == help.state.cursor();
                let style = if selected {
                    theme.role(Role::Selection)
                } else {
                    theme.role(Role::Regular)
                };
                Line::from(TSpan::styled(format!("  {title}"), style))
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(side), cols[0]);

    // Body: the keyboard page is the generated cheatsheet; everything else is
    // a corpus topic through the renderer.
    let width = body_area.width as usize;
    let height = body_area.height as usize;
    let lines: Vec<Line<'_>> = if help.on_keys_page() {
        help.keys_lines.iter().map(|l| Line::raw(l.clone())).collect()
    } else if let Some(topic) = help.state.topic() {
        let resolver = HELP_RESOLVER.with(Clone::clone);
        let rendered = crate::help_render::render_topic(topic, resolver.as_ref(), width, theme);
        let mut lines = rendered.lines;
        if help.state.focus() == Focus::Body
            && let Some(&line) = rendered.action_lines.get(help.state.action_cursor())
            && let Some(l) = lines.get_mut(line)
        {
            *l = l.clone().style(theme.role(Role::Selection));
        }
        lines
    } else {
        Vec::new()
    };
    frame.render_widget(
        Paragraph::new(
            lines
                .into_iter()
                .skip(help.state.body_scroll())
                .take(height)
                .collect::<Vec<_>>(),
        ),
        body_area,
    );

    // Footer: the filter editor when it is open, the generated hint otherwise.
    let footer = if help.state.filtering() {
        format!("/{}", display_name(help.state.filter_display().as_bytes()))
    } else {
        hint.to_owned()
    };
    frame.render_widget(
        Paragraph::new(Line::from(TSpan::styled(footer, theme.role(Role::StatusBar)))),
        rows[1],
    );
}
```

The resolver cannot be rebuilt per frame (it allocates a catalogue set), and
`ui.rs` has no access to the keymaps. Rather than a thread local, pass it in:
change the signature to

```rust
fn draw_help(
    frame: &mut Frame<'_>,
    help: &crate::app::HelpView,
    chords: &crate::help::TuiChords,
    theme: &TuiTheme,
    hint: &str,
)
```

drop the `HELP_RESOLVER` line, use `chords` directly, and update the call site
at `ui.rs:262`:

```rust
    if let Some(help) = &app.help {
        draw_help(frame, help, &app.help_chords, &app.theme, &app.dialog_hints.help);
    }
```

Add the field to `App` next to `dialog_hints` in `app.rs`:

```rust
    /// Resolver of the help's live marks (H3b): rebuilt with the effectives
    /// on every hot reload, like `dialog_hints`.
    pub help_chords: std::sync::Arc<crate::help::TuiChords>,
```

initialising it in `App::new` with the same effectives the hints use.

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p norte-tui app:: ui::`
Expected: PASS. `cargo check -p norte-tui` must be clean before moving on.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-tui/src/app.rs crates/norte-tui/src/ui.rs
git commit -m "feat(tui): HelpView state and the two-pane help draw (H3b)"
```

---

### Task 7: run-loop wiring

**Files:**
- Modify: `crates/norte-tui/src/main.rs:1714-1730` (the help key branch)
- Modify: `crates/norte-tui/src/main.rs:4875` (where `app.help` is opened)
- Modify: `crates/norte-tui/src/main.rs:3165` (hot reload)

- [ ] **Step 1: Write the failing test**

Create `crates/norte-tui/tests/help_overlay.rs`:

```rust
//! The help overlay end to end (H3b): opening, navigating, filtering, and
//! what `Enter` does on each kind of row.

use norte_frontend::help::{Action, Focus};
use norte_tui::app::{HelpOutcome, HelpView, help_action};

fn view() -> HelpView {
    HelpView::new(norte_help::Lang::En, vec!["── keys ──".to_owned()])
}

#[test]
fn the_filter_verb_opens_the_editor_and_esc_leaves_the_text() {
    let mut v = view();
    assert_eq!(help_action("dialog.filter"), Some(HelpOutcome::StartFilter));
    v.state.start_filter();
    v.state.push_char('c');
    v.state.push_char('o');
    assert!(v.state.filtering());
    v.state.end_filter();
    assert!(!v.state.filtering());
    assert_eq!(v.state.filter(), "co", "ending the edit keeps the search");
}

#[test]
fn enter_on_a_command_row_yields_a_command_to_dispatch() {
    let mut v = view();
    v.state.open(&norte_help::TopicId::new("copying"));
    v.state.toggle_focus();
    assert_eq!(v.state.focus(), Focus::Body);
    match v.state.action() {
        Some(Action::Run(cmd)) => assert_eq!(cmd, "pane.copy"),
        other => panic!("expected a runnable row, got {other:?}"),
    }
}

#[test]
fn enter_on_a_link_follows_it_without_dispatching() {
    let mut v = view();
    v.state.open(&norte_help::TopicId::new("copying"));
    v.state.toggle_focus();
    // Walk past the commands into the `see_also` links.
    for _ in 0..20 {
        v.state.down();
    }
    let Some(Action::Open(id)) = v.state.action().cloned() else {
        panic!("the last actions of a topic are its links");
    };
    v.state.open(&id);
    assert_eq!(v.state.current(), &id);
    assert!(v.state.back(), "following a link is undoable");
}

#[test]
fn the_keyboard_page_is_reachable_and_shows_the_generated_lines() {
    let mut v = view();
    v.state.open(&norte_help::TopicId::new(
        norte_frontend::help::KEYS_ID,
    ));
    assert!(v.on_keys_page());
    assert_eq!(v.keys_lines, vec!["── keys ──".to_owned()]);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p norte-tui --test help_overlay`
Expected: FAIL to compile — `HelpView`/`help_action` not exported, or the
run-loop branch still builds the old `Help`.

- [ ] **Step 3: Rewrite the run-loop branch**

Replace `crates/norte-tui/src/main.rs:1714-1730` with:

```rust
                    } else if !modal_wins(app) && app.help.is_some() {
                        // Help overlay (H3b). Two regimes, the same split the
                        // palette and the search dialog already have: while
                        // the filter editor is open the keys are FIXED (there
                        // is no `dialog.*` verb for "type a character"), and
                        // otherwise every key resolves through the `dialog`
                        // context against `ALLOW_HELP` — no hardcoded legend,
                        // and a rebind moves the footer with it.
                        //
                        // `ctrl+c` keeps its global meaning (quit) and
                        // `ctrl+p` hands the current filter to the palette,
                        // both BEFORE any resolution, like every other
                        // overlay (H1 T2).
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && key.code == KeyCode::Char('c')
                        {
                            app.quit = true;
                            continue;
                        }
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && key.code == KeyCode::Char('p')
                        {
                            let filter = app
                                .help
                                .as_ref()
                                .map(|h| h.state.filter().to_owned())
                                .unwrap_or_default();
                            app.help = None;
                            let mut palette = Palette::new(rows_for_context(
                                &app.palette_rows,
                                app.viewer.is_some(),
                            ));
                            for c in filter.chars() {
                                palette.push_char(c);
                            }
                            app.palette = Some(palette);
                            continue;
                        }
                        let filtering =
                            app.help.as_ref().is_some_and(|h| h.state.filtering());
                        let plain = key.modifiers.is_empty()
                            || key.modifiers == KeyModifiers::SHIFT;
                        if filtering {
                            let Some(help) = &mut app.help else {
                                continue;
                            };
                            match key.code {
                                KeyCode::Char(c) if plain => help.state.push_char(c),
                                KeyCode::Backspace if plain => help.state.backspace(),
                                // Esc ends the EDIT and keeps the text; a
                                // second Esc closes the overlay through the
                                // normal path below.
                                KeyCode::Esc if plain => help.state.end_filter(),
                                KeyCode::Enter if plain => help.state.end_filter(),
                                KeyCode::Up if plain => help.state.up(),
                                KeyCode::Down if plain => help.state.down(),
                                _ => {}
                            }
                            continue;
                        }
                        let Resolution::Run(cmd) =
                            dialog_resolver.resolve(key.modifiers, key.code)
                        else {
                            continue;
                        };
                        let Some(outcome) = norte_tui::app::help_action(&cmd) else {
                            continue;
                        };
                        let mut run: Option<String> = None;
                        if let Some(help) = &mut app.help {
                            use norte_frontend::help::Action;
                            use norte_tui::app::HelpOutcome as O;
                            match outcome {
                                O::Up => help.state.up(),
                                O::Down => help.state.down(),
                                O::PageUp => help.state.page_up(PAGE),
                                O::PageDown => help.state.page_down(PAGE),
                                O::TogglePane => help.state.toggle_focus(),
                                O::StartFilter => help.state.start_filter(),
                                O::Back => {
                                    if !help.state.back() {
                                        app.help = None;
                                    }
                                }
                                O::Close => app.help = None,
                                O::Activate => match help.state.action().cloned() {
                                    // A link stays inside the overlay.
                                    Some(Action::Open(id)) => help.state.open(&id),
                                    // A command acts on the panes UNDERNEATH,
                                    // so the overlay gets out of the way
                                    // first — otherwise it would cover the
                                    // confirmation the command opens.
                                    Some(Action::Run(c)) => run = Some(c),
                                    None => help.state.open_selected(),
                                },
                            }
                        }
                        if let Some(cmd) = run {
                            app.help = None;
                            // The SAME dispatch the palette and the key
                            // resolver use: a command run from the help is
                            // not a second path into the engine, so it cannot
                            // skip a confirmation, a policy gate or a journal
                            // entry.
                            let Some(parsed) = Command::parse(&cmd) else {
                                debug_assert!(false, "help fuera de COMMANDS: {cmd}");
                                continue;
                            };
                            let outcome = dispatch(
                                app,
                                backend,
                                &mut events,
                                help_lines,
                                quick_mode,
                                confirm_quit,
                                &cfg,
                                parsed,
                            )
                            .await;
                            apply_cd(&mut fill, &mut last_probed, outcome);
                        }
                    }
```

The `dialog_resolver` name must match the resolver the modal branch below
already builds from the `dialog` effective — reuse that binding rather than
constructing a second one. If it is created inside the modal branch, hoist it
above both.

- [ ] **Step 4: Open the overlay with the new type**

At `crates/norte-tui/src/main.rs:4875`, replace the construction:

```rust
            app.help = Some(HelpView::new(help_lang, help_lines.to_vec()));
```

where `help_lang` is the `lang` computed at line 588 — thread it into
`dispatch` as a parameter if it is not already in scope, next to `cfg`.

- [ ] **Step 5: Rebuild on hot reload**

At `crates/norte-tui/src/main.rs:3165`, next to
`*help_lines = norte_tui::help::build(&browse, &viewer, &dialog);`, also
rebuild the resolver so a rebind changes the prose:

```rust
                app.help_chords = std::sync::Arc::new(norte_tui::help::TuiChords::new(
                    browse.clone(),
                    viewer.clone(),
                    dialog.clone(),
                    help_lang,
                ));
                app.help = None;
```

(the `app.help = None` line already exists — keep it: the open page's rendered
lines are stale after a rebind, and rebuilding them mid-frame would be the same
"rows that expired" case the palette already closes for.)

- [ ] **Step 6: Run the tests**

Run: `cargo nextest run -p norte-tui`
Expected: PASS. Snapshot tests of the old help will FAIL with a diff — that is
Task 8; do not accept them here.

- [ ] **Step 7: Commit**

```bash
git add crates/norte-tui/src/main.rs
git commit -m "feat(tui): help overlay keys, dispatch and hot reload (H3b)"
```

---

### Task 8: snapshots, hostile render, and the changelog

**Files:**
- Modify: `crates/norte-tui/tests/snapshots_ui.rs`
- Modify/accept: `crates/norte-tui/tests/snapshots/*help*.snap`
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Write the failing snapshot test**

Add to `crates/norte-tui/tests/snapshots_ui.rs`, following the shape of the
existing overlay snapshots in that file:

```rust
/// H3b: the help overlay in its default state — sidebar, index page, footer.
#[test]
fn snapshot_help_overlay() {
    let mut app = app_para_snapshot();
    app.help = Some(norte_tui::app::HelpView::new(
        norte_help::Lang::En,
        norte_tui::help::build(&browse_eff(), &viewer_eff(), &dialog_eff()),
    ));
    insta::assert_snapshot!(render_to_string(&app, 100, 30));
}

/// The same overlay with a hostile filter typed into it: what is painted
/// carries no terminal hazard (the corpus fixture arrives by paste as easily
/// as by hand).
#[test]
fn snapshot_help_overlay_filtro_hostil() {
    let mut app = app_para_snapshot();
    let mut view = norte_tui::app::HelpView::new(norte_help::Lang::En, Vec::new());
    view.state.start_filter();
    for c in "\u{202E}co".chars() {
        view.state.push_char(c);
    }
    app.help = Some(view);
    let out = render_to_string(&app, 100, 30);
    assert!(
        !out.chars().any(norte_encoding::is_terminal_hazard),
        "hazard painted in the help footer: {out:?}"
    );
    insta::assert_snapshot!(out);
}
```

Use whatever the file's existing helpers are actually called
(`app_para_snapshot`/`render_to_string` are placeholders for the ones already
in that file — check the top of it and match them).

- [ ] **Step 2: Run and review the snapshots**

Run: `cargo nextest run -p norte-tui --test snapshots_ui`
Expected: FAIL with pending snapshots.

Run: `cargo insta review`
Read every diff before accepting: the old flat-list snapshot must DISAPPEAR
(its test is gone) and the two new ones must show a sidebar with group headers,
a body with resolved chords (`f5`, not `{{cmd:pane.copy}}`), and a footer of
generated hints.

- [ ] **Step 3: Delete the obsolete snapshot files**

Run: `git status --short crates/norte-tui/tests/snapshots`
Delete any `*.snap` left over from the old help overlay, if `insta` did not.

- [ ] **Step 4: Write the changelog entry**

In `CHANGELOG.md`, under `## [Unreleased]` → `### Added`, above the mouse
entry:

```markdown
- **Help you can navigate, and that knows your keys:** `F1` opens a page, not
  a key dump. A sidebar of topics on the left, the page on the right, `/` to
  filter it by title or by the commands a page documents, `Tab` to move into
  the page, `Enter` to run the command a row describes — through the same
  dispatch its own key uses, with the same confirmation, policy gate and
  journal entry — and `Backspace` to go back where you came from. No key in
  any page is written into the text: each one is looked up in your effective
  keymap as the page is drawn, so a rebind changes the prose. The old flat
  cheatsheet is still there, as the last entry in the list, generated from the
  same keymap.
```

- [ ] **Step 5: Commit**

```bash
git add crates/norte-tui/tests CHANGELOG.md
git commit -m "test(tui): help overlay snapshots and hostile filter render (H3b)"
```

---

### Task 9: full gate

**Files:** none (verification only)

- [ ] **Step 1: Run the workspace tests**

Run: `cargo nextest run --workspace 2>&1 | tail -20; echo "EXIT=$?"`
Expected: `EXIT=0`, zero failures.

- [ ] **Step 2: Run clippy over everything**

Run: `cargo clippy --workspace --all-targets --features norte-config/watch -- -D warnings 2>&1 | tail -20; echo "EXIT=$?"`
Expected: `EXIT=0`. Capture the exit code explicitly — a piped `| tail`
swallows the real status, which shipped two red commits during H1.

- [ ] **Step 3: Check the GUI still builds**

Run: `just check-gui 2>&1 | tail -5; echo "EXIT=$?"`
Expected: `EXIT=0`. `norte-gui` is out of the workspace and consumes
`norte-frontend`; the new `help` module must not break it.

- [ ] **Step 4: Full local CI**

Run: `just ci 2>&1 | tail -30; echo "EXIT=$?"`
Expected: `EXIT=0`, coverage at or above 85% for proto/vfs/core.

- [ ] **Step 5: Dispatch the reviewers**

Per the phase table in the spec, H3b needs `rust-reviewer` and
`encoding-auditor`. Give each the diff range of this branch. Apply BLOCKER and
MAJOR findings before merging; record MINORs either as fixes or as issues.

- [ ] **Step 6: Commit any review fixes and close the phase**

```bash
git add -A
git commit -m "fix(tui): review findings on the help overlay (H3b)"
```

---

## Self-review notes

- **Spec coverage.** Sidebar/body/filter/history/Enter-runs: Tasks 1, 5, 6, 7.
  Keys through the `dialog` context with no hardcoded legend: Task 3 + Task 7.
  `Ctrl+P` handoff to the palette: Task 7. Execution through the same dispatch:
  Task 7 (the `dispatch` call), asserted in Task 8's snapshot review and Task
  7's test. Live marks resolving against the effective keymap: Task 2, pinned
  in Task 5's first test.
- **Deliberately NOT in this phase**, and each has a later one in the spec's
  own table: contextual F1 per screen (H3c), real `Availability` (H3d), plugin
  topics and the `plugin.help` wire (H3e), the GUI view (H3f), `norte help` on
  the CLI (H3g), the full corpus and the zero allowlist (H3h). The `keys`
  entry is a synthetic stand-in until H3h writes a real page.
- **Known rough edge to raise in review:** `HelpState::step_sidebar` opens the
  topic it lands on (preview semantics, like the theme picker), which means
  arrowing through the sidebar fills the history. If review dislikes it, the
  fix is one line — do not push history from `step_sidebar` — plus its test.
