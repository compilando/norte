//! The command palette: its model, the filtering, and the cursor.
//!
//! Hoisted from `norte-tui` (same operation as `History`/`Trail` in task 2.3
//! of the multi-frontend plan): filtering a list of commands by what was
//! typed, moving the cursor among what matches, and knowing what is selected
//! are PRESENTATION RULES, and two frontends with two copies are two palettes
//! that behave differently without anyone noticing (ADR 0066, decision D14).
//!
//! The ROWS are built by each frontend with its own list of implemented
//! commands; what is shared is what the palette does with them.

/// The command palette (`Ctrl+P`, vim `:`): free-text filter over the rows
/// the frontend gives it.
///
/// Its keys do NOT resolve against the `dialog` context: it is a free text
/// editor, like the incremental search. There is no `dialog.*` vocabulary for
/// "type a character" or "run the selected one", so whoever has it open
/// treats those keys as fixed.
///
/// The `rows` arrive ALREADY built (each frontend's `build_rows`,
/// precomputed like `help_lines`/`dialog_hints` — same criterion: rebuilt on
/// startup and on every hot-reload OK, BEFORE the effectives move to the
/// `Resolver`); `Palette::new` only folds each row's haystack. Same cache
/// pattern as [`crate::nav::QuickSearch`] (#77): the per-row fold is computed
/// ONCE here, not per keystroke — keystrokes only fold the query.
#[derive(Debug, Clone)]
pub struct Palette {
    /// `(command, description, chord-or-dash)` — snapshot frozen on open.
    rows: Vec<crate::palette::Row>,
    /// Folded haystack per row (name + description, [`crate::nav::fold`]),
    /// index-parallel to `rows`.
    folds: Vec<String>,
    /// Bytes typed as-is (matching WITHOUT sanitizing; sanitizing happens
    /// only when painting, [`Self::query_display`] — same contract as
    /// [`crate::nav::QuickSearch::query_display`]).
    query: Vec<u8>,
    /// REAL indices into `rows` that match (empty query = all).
    visible: Vec<usize>,
    /// Position of the selection WITHIN `visible`.
    cursor: usize,
    /// Recently launched keys, most recent first (spec 2026-09-10): with an
    /// empty query they go on top, in that order. They come from the session
    /// ([`crate::session::SessionBody::palette_recent`]).
    recent: Vec<String>,
}

/// Is `needle` a subsequence of `hay`? (`cpf` matches `copy path` because
/// `c`, `p`, `f`... — no wait, `f` does not: it matches `cop` and `pat`; what
/// matters is that each byte appears in order). Empty matches everything.
/// Folded bytes only.
pub(crate) fn is_subsequence(needle: &str, hay: &str) -> bool {
    // By CHARS, not by bytes: a query must not be able to match a
    // continuation byte in the middle of a character (review m12).
    let mut it = hay.chars();
    needle.chars().all(|c| it.any(|h| h == c))
}

impl Palette {
    /// [`Self::new`] with the recent commands: with an empty query, rows
    /// whose key is in `recent` go first, in `recent`'s order. A key that no
    /// longer has a row (an uninstalled plugin, a command this frontend does
    /// not implement) paints nothing.
    #[must_use]
    pub fn with_recent(rows: Vec<crate::palette::Row>, recent: &[String]) -> Self {
        let mut p = Self::new(rows);
        p.recent = recent.to_vec();
        p.recompute();
        p
    }
    /// Opens the palette over `rows` (`App`'s precomputed snapshot): folds
    /// each row's haystack and starts with an empty query (everything
    /// visible). The fold is over `text`+`desc` (what is PAINTED, already
    /// masked for a plugin row) — never over `key` (P1: it could carry the
    /// manifest's raw `command_id`, with no charset validated).
    #[must_use]
    pub fn new(rows: Vec<crate::palette::Row>) -> Self {
        let folds = rows
            .iter()
            .map(|row| crate::nav::fold(format!("{} {}", row.text, row.desc).as_bytes()))
            .collect();
        let mut p = Self {
            rows,
            folds,
            query: Vec::new(),
            visible: Vec::new(),
            cursor: 0,
            recent: Vec::new(),
        };
        p.recompute();
        p
    }

    /// Is the `i`-th row of [`Self::rows`] one of the recent ones? So
    /// whoever paints can say so (a separator, a tone).
    #[must_use]
    pub fn is_recent(&self, i: usize) -> bool {
        self.rows
            .get(i)
            .is_some_and(|r| self.recent.contains(&r.key))
    }

    /// Adds rows to an ALREADY open palette, keeping what was typed.
    ///
    /// Exists because plugin rows cannot be available on open: they come
    /// from a `plugin.list` that has to be requested, and waiting for it to
    /// answer before painting the palette would freeze the window for rows
    /// that might not even exist. The alternative —rebuilding it with
    /// `new`— loses the query, which is exactly what the reader just typed.
    ///
    /// The fold is computed the same way as in [`Self::new`]: over what is
    /// PAINTED (`text`+`desc`), never over `key`.
    pub fn extend_rows(&mut self, rows: Vec<crate::palette::Row>) {
        self.folds.extend(
            rows.iter()
                .map(|row| crate::nav::fold(format!("{} {}", row.text, row.desc).as_bytes())),
        );
        self.rows.extend(rows);
        self.recompute();
    }

    /// Recomputes `visible` from the current query over `self.folds` (the
    /// ALREADY current cache) and clamps the cursor.
    fn recompute(&mut self) {
        self.visible = if self.query.is_empty() {
            // Recent ones first, in their order; then the rest in row order.
            let mut out: Vec<usize> = self
                .recent
                .iter()
                .filter_map(|k| self.rows.iter().position(|r| r.key == *k))
                .collect();
            let rest: Vec<usize> = (0..self.rows.len()).filter(|i| !out.contains(i)).collect();
            out.extend(rest);
            out
        } else {
            let q = crate::nav::fold(&self.query);
            let exact: Vec<usize> = self
                .folds
                .iter()
                .enumerate()
                .filter(|(_, f)| f.contains(&q))
                .map(|(i, _)| i)
                .collect();
            if exact.is_empty() {
                // No substring, subsequence: `cpf` reaches "copy path".
                // Only as a FALLBACK, so typing what you see keeps giving
                // what you see, and nothing more, as long as something
                // matches.
                self.folds
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| is_subsequence(&q, f))
                    .map(|(i, _)| i)
                    .collect()
            } else {
                exact
            }
        };
        self.clamp_cursor();
    }

    fn clamp_cursor(&mut self) {
        if self.visible.is_empty() {
            self.cursor = 0;
        } else if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len() - 1;
        }
    }

    /// Adds a typed character to the query and recomputes (same contract as
    /// [`crate::nav::QuickSearch::push_char`]).
    pub fn push_char(&mut self, c: char) {
        let mut buf = [0u8; 4];
        self.query
            .extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        self.recompute();
    }

    /// Removes the last complete UTF-8 char typed and recomputes.
    pub fn backspace(&mut self) {
        if self.query.is_empty() {
            return;
        }
        let mut cut = self.query.len() - 1;
        while cut > 0 && (self.query[cut] & 0b1100_0000) == 0b1000_0000 {
            cut -= 1;
        }
        self.query.truncate(cut);
        self.recompute();
    }

    /// Moves the selection up (stops at the top).
    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Moves the selection down (stops at the end).
    pub fn down(&mut self) {
        if self.cursor + 1 < self.visible.len() {
            self.cursor += 1;
        }
    }

    /// Moves up `n` positions (pgup).
    pub fn page_up(&mut self, n: usize) {
        self.cursor = self.cursor.saturating_sub(n);
    }

    /// Moves down `n` positions, stops at the end (pgdn).
    pub fn page_down(&mut self, n: usize) {
        self.cursor = (self.cursor + n).min(self.visible.len().saturating_sub(1));
    }

    /// REAL indices into `rows()` visible with the current query.
    #[must_use]
    pub fn visible(&self) -> &[usize] {
        &self.visible
    }

    /// All the rows ([`crate::palette::Row`]) — `rows()[visible()[i]]` to
    /// paint the `i`-th row of the filtered list. Only `text`/`desc`/`chord`
    /// get painted; `key` is for internal dispatch (see
    /// [`crate::palette::Row`]'s doc).
    #[must_use]
    pub fn rows(&self) -> &[crate::palette::Row] {
        &self.rows
    }

    /// Position of the selection WITHIN `visible()` (for `ListState`).
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The dispatch KEY under the cursor, if one is visible (P1: it is no
    /// longer `&'static str` — a plugin row carries a `key` built at
    /// runtime, `plugin:{id}:{command}`; it is cloned because
    /// `main::dispatch` uses it AFTER closing the palette, `app.palette =
    /// None`, which drops `rows`).
    #[must_use]
    pub fn selected(&self) -> Option<String> {
        self.visible
            .get(self.cursor)
            .map(|&i| self.rows[i].key.clone())
    }

    /// Query to paint (lossy, masked — same contract as
    /// [`crate::nav::QuickSearch::query_display`]: without bracketed paste a
    /// hostile paste arrives as a stream of `push_char` and would paint raw
    /// bidi/invisible characters at the edge).
    #[must_use]
    pub fn query_display(&self) -> String {
        String::from_utf8_lossy(&self.query)
            .chars()
            .map(|c| {
                if norte_encoding::is_terminal_hazard(c) {
                    '\u{FFFD}'
                } else {
                    c
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod palette_tests {
    use super::Palette;

    fn row(key: &str, desc: &str, chord: &str) -> crate::palette::Row {
        crate::palette::Row {
            key: key.to_owned(),
            text: key.to_owned(),
            desc: desc.to_owned(),
            chord: chord.to_owned(),
            hostile: false,
        }
    }

    fn rows() -> Vec<crate::palette::Row> {
        vec![
            row("app.quit", "quit norte", "q"),
            row("app.help", "this help", "f1"),
        ]
    }

    /// Recent ones on top with an empty query, in their order; a key with no
    /// row paints nothing; and a query returns them to the normal order.
    #[test]
    fn recent_rows_go_first_only_with_an_empty_query() {
        let recent = vec!["app.help".to_owned(), "plugin:no-longer:exists".to_owned()];
        let mut p = Palette::with_recent(rows(), &recent);
        let visible: Vec<&str> = p
            .visible()
            .iter()
            .map(|&i| p.rows()[i].key.as_str())
            .collect();
        assert_eq!(visible, ["app.help", "app.quit"]);
        assert!(p.is_recent(p.visible()[0]) && !p.is_recent(p.visible()[1]));
        p.push_char('q');
        assert_eq!(p.selected().as_deref(), Some("app.quit"));
    }

    /// No substring, subsequence: `qn` matches "quit norte". And as long as
    /// there is a substring, the subsequence adds no noise.
    #[test]
    fn palette_falls_back_to_subsequence_when_nothing_matches_whole() {
        let mut p = Palette::new(rows());
        for c in "qn".chars() {
            p.push_char(c);
        }
        assert_eq!(p.selected().as_deref(), Some("app.quit"));
        let mut p = Palette::new(rows());
        for c in "help".chars() {
            p.push_char(c);
        }
        assert_eq!(p.visible().len(), 1, "exact substring: only app.help");
        assert!(super::is_subsequence("", "x") && !super::is_subsequence("ba", "ab"));
        assert!(
            !super::is_subsequence("\u{a9}", "é"),
            "by chars, not by bytes"
        );
    }

    #[test]
    fn palette_filters_and_selects() {
        let mut p = Palette::new(rows());
        for c in "quit".chars() {
            p.push_char(c);
        }
        assert_eq!(p.visible().len(), 1, "only app.quit matches 'quit'");
        assert_eq!(p.selected().as_deref(), Some("app.quit"));
    }

    #[test]
    fn palette_hostile_query_is_masked() {
        let mut p = Palette::new(rows());
        for c in "a\u{202E}b".chars() {
            p.push_char(c);
        }
        let display = p.query_display();
        assert!(
            !display.chars().any(norte_encoding::is_terminal_hazard),
            "query_display left a raw hazard: {display:?}"
        );
    }

    #[test]
    fn palette_empty_filter_shows_everything() {
        let p = Palette::new(rows());
        assert_eq!(p.visible().len(), 2, "empty query = all rows");
        assert_eq!(
            p.selected().as_deref(),
            Some("app.quit"),
            "the cursor starts on the first one"
        );
    }

    /// A filter that does NOT match any row: `selected()` returns `None`
    /// (never a phantom index) and `up`/`down`/pages do not panic over an
    /// empty `visible`.
    #[test]
    fn palette_no_matches_selected_is_none_and_does_not_panic() {
        let mut p = Palette::new(rows());
        for c in "zzz".chars() {
            p.push_char(c);
        }
        assert!(p.visible().is_empty());
        assert_eq!(p.selected(), None);
        p.up();
        p.down();
        p.page_up(3);
        p.page_down(3);
        assert_eq!(p.selected(), None);
    }

    /// (P1) Plugin rows ([`crate::palette::plugin_rows`]) mixed with the
    /// built-ins: the free-text filter matches against the ALREADY masked
    /// TITLE (`text`), and Enter (`selected()`) returns the dispatch `key`
    /// `plugin:{id}:{command}` — never the painted text.
    #[test]
    fn palette_plugin_rows_are_filtered_by_title_and_dispatch_by_key() {
        let plugin = norte_proto::methods::PluginInfo {
            id: "org.norte.demo".into(),
            name: "Demo".into(),
            publisher: "norte".into(),
            version: "0.1.0".into(),
            category: "command".into(),
            capabilities: Vec::new(),
            approved: true,
            enabled: true,
            description: None,
            commands: vec![norte_proto::methods::PluginCommandInfo {
                id: "greet".into(),
                title: "Greet loudly".into(),
                kind: norte_proto::methods::PluginCommandKind::Command,
            }],
            columns: Vec::new(),
            panels: Vec::new(),
            has_help: false,
            manifest_digest: None,
        };
        let mut all = rows();
        all.extend(crate::palette::plugin_rows(&[plugin]));
        let mut p = Palette::new(all);
        for c in "loudly".chars() {
            p.push_char(c);
        }
        assert_eq!(
            p.visible().len(),
            1,
            "only the plugin row matches 'loudly' (the title)"
        );
        assert_eq!(p.selected().as_deref(), Some("plugin:org.norte.demo:greet"));
    }
}
