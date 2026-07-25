# First-class selection implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give both frontends a real selection — toggle, all, invert, clear, select/deselect by glob — with marks that survive a refresh, and make copy/move/delete operate on the whole selection.

**Architecture:** Every decision lives in `norte-frontend::pane::PaneState` (hard rule 7); the frontends only bind keys and paint. Marks stay a `HashSet<VPath>` keyed by byte-exact absolute path, so re-sorting is already immune. `cd` clears marks (existing `set_listing` / `begin_loading` behaviour); the refresh path (`refill`) preserves them and prunes vanished entries. Bulk operations submit one engine call per item with a conflict backlog, the shape the GUI already uses.

**Tech stack:** Rust, `globset` (already a workspace dependency, used by `norte-core` for `fs.search`), `ratatui` (TUI), GPUI (GUI), Fluent (`crates/norte-i18n/i18n/{en,es}.ftl`), `cargo nextest`.

**Spec:** `docs/superpowers/specs/2026-07-25-first-class-selection-design.md`
**Issue:** #103

**Conventions for this plan:** new rustdoc and new test names are written in **English** — `norte-frontend` still carries Spanish comments from M1, but newer additions across the repo (`role.rs`'s `Mark`, `entry.rs`'s `attrs`, every doc since commit `0d63f83`) are English, and that is the direction. Do not translate surrounding code you are not otherwise touching.

---

## File structure

| File | Responsibility | Tasks |
| --- | --- | --- |
| `crates/norte-frontend/src/pane.rs` | mark model: prune, all/invert/clear, glob, byte total | 1, 2, 3 |
| `crates/norte-frontend/src/nav.rs` | fold pipeline: `fold_with` becomes `pub(crate)` | 3 |
| `crates/norte-frontend/src/format.rs` (new) | human byte-size formatting, reused by columns (#108) | 8 |
| `crates/norte-frontend/src/keymap.rs` | `plus` chord token + `Display` symmetry | 4 |
| `crates/norte-frontend/presets/keymap/{orthodox,vim,cua}.toml` | the six chords | 5 |
| `crates/norte-i18n/i18n/{en,es}.ftl` | `help-cmd-mark-*`, dialog and status strings | 5, 8, 9 |
| `crates/norte-tui/src/keymap.rs` | `COMMANDS` gains the mark commands | 6 |
| `crates/norte-tui/src/app.rs` | `Pane` delegators, `Modal::MarkPattern`, bulk modal shapes | 6, 9, 10 |
| `crates/norte-tui/src/main.rs` | command dispatch, bulk submission, conflict backlog | 6, 9, 10 |
| `crates/norte-tui/src/ui.rs` | mark gutter, status-bar counter, pattern dialog render | 7, 8, 9 |
| `crates/norte-gui/src/keymap.rs` | catalogue gains the commands; supplement drops `insert` | 5, 11 |
| `crates/norte-gui/src/main.rs` | dispatch for all/invert/clear | 11 |

---

### Task 1: Marks are pruned when the listing changes

**Files:**
- Modify: `crates/norte-frontend/src/pane.rs` (`refill` ~line 592, `set_loading` ~line 531)
- Test: `crates/norte-frontend/src/pane.rs` (inline `mod tests`)

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` in `pane.rs`. `e(...)` and the `PaneState` helpers already exist there — read the neighbouring tests first and reuse them verbatim.

```rust
#[test]
fn refill_keeps_marks_of_entries_that_survive() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![e("mem:///a", EntryKind::File), e("mem:///b", EntryKind::File)],
    );
    p.cursor_to(1); // "b"
    p.toggle_mark();
    p.refill(vec![e("mem:///a", EntryKind::File), e("mem:///b", EntryKind::File)]);
    assert_eq!(p.marks_len(), 1);
    assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///b").unwrap()]);
}

#[test]
fn refill_prunes_a_mark_whose_entry_vanished() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![e("mem:///a", EntryKind::File), e("mem:///b", EntryKind::File)],
    );
    p.cursor_to(1);
    p.toggle_mark();
    p.refill(vec![e("mem:///a", EntryKind::File)]);
    assert_eq!(p.marks_len(), 0, "a mark is a claim about an entry that exists");
}

/// A fill only ADDS entries (`extend`), so a mark placed while it runs always
/// points at something present. Pinned so a future `extend` that starts
/// dropping entries fails here instead of silently widening a bulk operation.
#[test]
fn a_mark_placed_mid_fill_survives_the_rest_of_the_fill() {
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), vec![e("mem:///b", EntryKind::File)]);
    p.set_loading(true);
    p.toggle_mark();
    p.extend(vec![e("mem:///a", EntryKind::File)]);
    p.set_loading(false);
    assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///b").unwrap()]);
}

#[test]
fn cd_clears_marks() {
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), vec![e("mem:///a", EntryKind::File)]);
    p.toggle_mark();
    assert_eq!(p.marks_len(), 1);
    p.set_listing(VPath::parse("mem:///sub").unwrap(), vec![e("mem:///sub/a", EntryKind::File)]);
    assert_eq!(p.marks_len(), 0);
}
```

If `cursor_to` is not the existing cursor-setter, use whatever the neighbouring tests use (`cursor_down()` in a loop is fine) — do not invent an API.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p norte-frontend -E 'test(marks)'`
Expected: `refill_prunes_a_mark_whose_entry_vanished` FAILS (the mark survives); the other three already pass. The `test(marks)` filter does not match that test's name — widen it to `-E 'test(prune) or test(marks) or test(mid_fill)'`.

- [ ] **Step 3: Implement pruning**

Add the private helper next to the other mark methods in `pane.rs`:

```rust
    /// Drops marks whose entry is no longer listed. A mark is a claim about
    /// an entry that EXISTS: a stale path would silently widen the next bulk
    /// operation. Called from [`Self::refill`], the same-dir refresh: the only
    /// path that can drop an entry without a `cd`. A paginated fill
    /// ([`Self::extend`], ADR 0017) only ADDS entries, so a mark placed
    /// mid-fill always points at something present and needs no pruning there.
    fn prune_marks(&mut self) {
        if self.marks.is_empty() {
            return;
        }
        let present: HashSet<&VPath> = self.entries.iter().map(|e| &e.path).collect();
        self.marks.retain(|p| present.contains(p));
    }
```

In `refill`, after `self.sort_keys = sort_keys;`, add `self.prune_marks();`. Leave `set_loading` alone — see the rustdoc above for why the fill needs no pruning.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p norte-frontend`
Expected: PASS, no regressions.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-frontend/src/pane.rs
git commit -m "feat(frontend): marks survive a refresh and prune vanished entries (#103)"
```

---

### Task 2: Mark all, invert, and the marked byte total

**Files:**
- Modify: `crates/norte-frontend/src/pane.rs`
- Test: `crates/norte-frontend/src/pane.rs` (inline `mod tests`)

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn mark_all_marks_every_entry() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![e("mem:///a", EntryKind::File), e("mem:///b", EntryKind::File)],
    );
    p.mark_all();
    assert_eq!(p.marks_len(), 2);
}

#[test]
fn invert_marks_flips_every_entry() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![e("mem:///a", EntryKind::File), e("mem:///b", EntryKind::File)],
    );
    p.toggle_mark(); // marks "a"
    p.invert_marks();
    assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///b").unwrap()]);
}

#[test]
fn mark_all_under_a_filter_only_marks_the_visible() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![e("mem:///alfa", EntryKind::File), e("mem:///beta", EntryKind::File)],
    );
    p.quick_start(Mode::Filter);
    p.quick_push('a');
    p.quick_push('l'); // matches "alfa" only
    p.mark_all();
    assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///alfa").unwrap()]);
}

#[test]
fn marked_bytes_sums_files_and_ignores_dirs() {
    let mut a = e("mem:///a", EntryKind::File);
    a.size = Some(10);
    let mut d = e("mem:///d", EntryKind::Dir);
    d.size = Some(4096); // a provider may report a dir size; it must not count
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), vec![a, d]);
    p.mark_all();
    assert_eq!(p.marked_bytes(), 10);
}

#[test]
fn marked_bytes_saturates_instead_of_overflowing() {
    let mut a = e("mem:///a", EntryKind::File);
    a.size = Some(u64::MAX);
    let mut b = e("mem:///b", EntryKind::File);
    b.size = Some(1);
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), vec![a, b]);
    p.mark_all();
    assert_eq!(p.marked_bytes(), u64::MAX, "a hostile listing must not panic in debug");
}
```

Use the quick-search entry points the neighbouring tests already use (`quick_start`/`quick_push` or their real names — read them first; `Mode` is `crate::nav::Mode`).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p norte-frontend -E 'test(mark)'`
Expected: FAIL — `mark_all`, `invert_marks`, `marked_bytes` do not exist.

- [ ] **Step 3: Implement**

```rust
    /// Índices sobre los que actúan las marcas EN BLOQUE: el subconjunto
    /// VISIBLE bajo un filtro quick activo, el listado entero si no lo hay
    /// — lo que ves es lo que marcas. Con un fill en curso
    /// ([`Self::loading`]) solo alcanza lo ya drenado; el pie del pane ya
    /// avisa de que el listado es parcial.
    fn markable_indices(&self) -> Vec<usize> {
        match self.quick_visible() {
            Some(vis) => vis.to_vec(),
            None => (0..self.entries.len()).collect(),
        }
    }

    /// Marks every entry of the visible set (see [`Self::markable_indices`]).
    pub fn mark_all(&mut self) {
        for i in self.markable_indices() {
            if let Some(path) = self.entries.get(i).map(|e| e.path.clone()) {
                self.marks.insert(path);
            }
        }
    }

    /// Flips the mark of every entry of the visible set.
    pub fn invert_marks(&mut self) {
        for i in self.markable_indices() {
            let Some(path) = self.entries.get(i).map(|e| e.path.clone()) else {
                continue;
            };
            if !self.marks.remove(&path) {
                self.marks.insert(path);
            }
        }
    }

    /// Total size of the marked FILES, saturating. Directories contribute 0:
    /// nothing here walks a tree, and a status bar that added a directory's
    /// own inode size would be claiming a total it never computed.
    #[must_use]
    pub fn marked_bytes(&self) -> u64 {
        self.entries
            .iter()
            .filter(|e| e.kind != EntryKind::Dir && self.marks.contains(&e.path))
            .fold(0u64, |acc, e| acc.saturating_add(e.size.unwrap_or(0)))
    }
```

`EntryKind` is already imported in `pane.rs` via `norte_proto::{Entry, VPath}` — add it to that `use` if it is missing.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p norte-frontend`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-frontend/src/pane.rs
git commit -m "feat(frontend): mark all, invert, and marked byte total (#103)"
```

---

### Task 3: Mark and unmark by glob

**Files:**
- Modify: `crates/norte-frontend/src/nav.rs` (make `fold_with` `pub(crate)`)
- Modify: `crates/norte-frontend/src/pane.rs`
- Modify: `crates/norte-frontend/Cargo.toml`
- Test: `crates/norte-frontend/src/pane.rs` (inline `mod tests`)

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn mark_glob_marks_the_matching_names() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![
            e("mem:///a.rs", EntryKind::File),
            e("mem:///b.rs", EntryKind::File),
            e("mem:///c.txt", EntryKind::File),
        ],
    );
    assert_eq!(p.mark_glob("*.rs", true).unwrap(), 2);
    assert_eq!(
        p.marked_paths(),
        vec![VPath::parse("mem:///a.rs").unwrap(), VPath::parse("mem:///b.rs").unwrap()]
    );
}

#[test]
fn mark_glob_is_case_insensitive() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![e("mem:///PHOTO.JPG", EntryKind::File)],
    );
    assert_eq!(p.mark_glob("*.jpg", true).unwrap(), 1);
}

#[test]
fn mark_glob_with_mark_false_unmarks_only_the_matches() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![e("mem:///a.rs", EntryKind::File), e("mem:///c.txt", EntryKind::File)],
    );
    p.mark_all();
    assert_eq!(p.mark_glob("*.rs", false).unwrap(), 1);
    assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///c.txt").unwrap()]);
}

#[test]
fn mark_glob_counts_only_the_marks_it_changed() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![e("mem:///a.rs", EntryKind::File), e("mem:///b.rs", EntryKind::File)],
    );
    p.mark_glob("a.rs", true).unwrap();
    assert_eq!(p.mark_glob("*.rs", true).unwrap(), 1, "a.rs was already marked");
}

#[test]
fn an_invalid_glob_errors_and_marks_nothing() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![e("mem:///a.rs", EntryKind::File)],
    );
    assert!(p.mark_glob("[", true).is_err());
    assert_eq!(p.marks_len(), 0);
}

#[test]
fn mark_glob_under_a_filter_only_reaches_the_visible() {
    let mut p = PaneState::new(
        VPath::parse("mem:///").unwrap(),
        vec![e("mem:///alfa.rs", EntryKind::File), e("mem:///beta.rs", EntryKind::File)],
    );
    p.quick_start(Mode::Filter);
    p.quick_push('a');
    p.quick_push('l');
    assert_eq!(p.mark_glob("*.rs", true).unwrap(), 1);
    assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///alfa.rs").unwrap()]);
}

/// Hostile corpus (hard rule 1): a pattern addresses the DISPLAYED text, so
/// it cannot name the invalid bytes — but it does match a name whose valid
/// suffix satisfies it, and `marked_paths` gives the ORIGINAL bytes back.
#[test]
fn mark_glob_matches_the_lossy_form_and_returns_raw_bytes() {
    let hostile = VPath::from_bytes(b"mem:///\xFF\xFE.rs").unwrap();
    let mut entry = Entry {
        path: hostile.clone(),
        kind: EntryKind::File,
        size: None,
        mtime_ms: None,
        ..Default::default()
    };
    entry.size = Some(1);
    let mut p = PaneState::new(VPath::parse("mem:///").unwrap(), vec![entry]);
    assert_eq!(p.mark_glob("*.rs", true).unwrap(), 1);
    assert_eq!(p.marked_paths(), vec![hostile], "raw bytes, never the lossy form");

    // The invalid bytes themselves are unaddressable: they fold to U+FFFD.
    p.clear_marks();
    assert_eq!(p.mark_glob("\u{FFFD}*", true).unwrap(), 1);
}
```

Build the hostile `VPath` exactly the way the existing hostile tests in this file do (there is already one around `marked_paths_con_marcas_...`); reuse their constructor rather than `from_bytes` if the name differs.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p norte-frontend -E 'test(glob)'`
Expected: FAIL — `mark_glob` does not exist.

- [ ] **Step 3: Add the dependency**

In `crates/norte-frontend/Cargo.toml`, under `[dependencies]`:

```toml
# Glob de nombre para marcar por patrón (#103): la MISMA lib que usa
# norte-core en fs.search, así que el patrón que el usuario aprende en la
# búsqueda vale aquí. Ya está en el grafo (workspace dep), no entra nada nuevo.
globset.workspace = true
```

- [ ] **Step 4: Widen the fold pipeline**

In `crates/norte-frontend/src/nav.rs`, change `fn fold_with(` to `pub(crate) fn fold_with(` and append to its rustdoc:

```
/// `pub(crate)` para que el marcado por patrón (#103) pliegue EXACTAMENTE
/// igual que el quick search — un solo pipeline, jamás una copia divergente.
```

- [ ] **Step 5: Implement `mark_glob`**

At the top of `pane.rs`, add `use globset::{Glob, GlobBuilder};`. Define the error next to `PaneState`:

```rust
/// Why a mark-by-pattern was rejected (hard rule 6: typed library errors).
#[derive(Debug, thiserror::Error)]
pub enum PatternError {
    /// The glob does not compile. Carries the `globset` diagnostic so the
    /// dialog can show WHY, the same treatment `fs.search` gives an invalid
    /// glob — the pattern is the user's own input, never a secret.
    #[error("invalid pattern: {0}")]
    Glob(#[from] globset::Error),
}
```

`thiserror` is already a dependency of `norte-frontend`; if it is not, add `thiserror.workspace = true` in the same commit.

```rust
    /// Marks (`mark = true`) or unmarks (`false`) the visible entries whose
    /// name matches `pattern`, a glob. Returns how many marks CHANGED, so
    /// the UI can report "12 marked" without recounting.
    ///
    /// Matching reuses the quick-search fold ([`crate::nav::fold_with`]:
    /// lossy UTF-8 → NFC → lowercase → NFC, honouring the pane's name
    /// reinterpretation) with a case-insensitive glob. A pattern therefore
    /// addresses the text the pane DISPLAYS, never the raw bytes: the
    /// invalid bytes of a non-UTF-8 name fold to U+FFFD and cannot be named,
    /// though such a name still matches a pattern its valid part satisfies.
    /// [`Self::toggle_mark`] always reaches it by hand, and
    /// [`Self::marked_paths`] returns its original bytes (hard rule 1).
    ///
    /// # Errors
    /// [`PatternError::Glob`] if the pattern does not compile. Nothing is
    /// marked in that case.
    pub fn mark_glob(&mut self, pattern: &str, mark: bool) -> Result<usize, PatternError> {
        let matcher = GlobBuilder::new(pattern)
            .case_insensitive(true)
            .literal_separator(false)
            .build()?
            .compile_matcher();
        let enc = self.name_encoding;
        let mut changed = 0usize;
        for i in self.markable_indices() {
            let Some(entry) = self.entries.get(i) else {
                continue;
            };
            let name = entry.path.file_name().map_or(&b""[..], |n| n.as_bytes());
            if !matcher.is_match(crate::nav::fold_with(name, enc).as_str()) {
                continue;
            }
            let path = entry.path.clone();
            let hit = if mark {
                self.marks.insert(path)
            } else {
                self.marks.remove(&path)
            };
            if hit {
                changed += 1;
            }
        }
        Ok(changed)
    }
```

Note `Glob` is imported for the `?` on `build()`; if clippy flags it as unused, drop it from the `use` and keep `GlobBuilder`.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo nextest run -p norte-frontend`
Expected: PASS.

- [ ] **Step 7: Lint**

Run: `cargo clippy -p norte-frontend --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 8: Commit**

```bash
git add crates/norte-frontend/Cargo.toml crates/norte-frontend/src/nav.rs crates/norte-frontend/src/pane.rs Cargo.lock
git commit -m "feat(frontend): mark and unmark by glob, folded like the quick search (#103)"
```

---

### Task 4: The `plus` chord token

**Files:**
- Modify: `crates/norte-frontend/src/keymap.rs` (`parse_chord` ~line 214, the `KeyCode` `Display` ~line 118)
- Test: `crates/norte-frontend/src/keymap.rs` (inline `mod tests`)

- [ ] **Step 1: Write the failing tests**

```rust
/// `+` is the modifier separator, so a bare "+" is unparseable and `plus`
/// is the only spelling. Pinned because a future refactor that "simplifies"
/// the token table would silently make the mark.pattern-add chord
/// unreachable (#103).
#[test]
fn plus_token_is_the_only_spelling_of_the_plus_key() {
    assert_eq!(
        parse_chord("plus"),
        Ok(Chord::new(Mods::default(), KeyCode::Char('+')))
    );
    assert!(matches!(parse_chord("+"), Err(KeymapError::BadChord { .. })));
    assert_eq!(
        parse_chord("ctrl+plus"),
        Ok(Chord::new(Mods { ctrl: true, ..Default::default() }, KeyCode::Char('+')))
    );
}

#[test]
fn plus_chord_round_trips_through_display() {
    let c = Chord::new(Mods::default(), KeyCode::Char('+'));
    assert_eq!(c.to_string(), "plus");
    assert_eq!(parse_chord(&c.to_string()), Ok(c));
}
```

If `parse_chord` returns `Result<Chord, KeymapError>` where `KeymapError` is not `PartialEq`, use `assert!(matches!(...))` plus an `unwrap()` comparison instead of `assert_eq!` on the `Result`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p norte-frontend -E 'test(plus)'`
Expected: FAIL — `parse_chord("plus")` is `BadChord`, and `Display` renders `+`.

- [ ] **Step 3: Implement**

In `parse_chord`'s `match key_txt`, next to `"space" => KeyCode::Char(' '),`:

```rust
        // `+` es el SEPARADOR de modificadores, así que un token "+" da key
        // vacía y muere en BadChord: `plus` es la única forma de expresar la
        // tecla (#103, mark.pattern-add). Aditivo: ningún keymap de usuario
        // podía contener "+" como tecla, porque hoy no parsea.
        "plus" => KeyCode::Char('+'),
```

In the `KeyCode` `Display` impl, before the generic `Char(c)` arm:

```rust
            KeyCode::Char('+') => f.write_str("plus"),
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p norte-frontend`
Expected: PASS. If an existing chord round-trip property test covers all `KeyCode`s, it must still pass — the `Display` arm is what keeps it holding.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-frontend/src/keymap.rs
git commit -m "feat(frontend): 'plus' chord token, the only spelling of the + key (#103)"
```

---

### Task 5: Commands in the shared presets, help text in both locales

**Files:**
- Modify: `crates/norte-frontend/presets/keymap/orthodox.toml`, `vim.toml`, `cua.toml`
- Modify: `crates/norte-tui/src/keymap.rs` (`COMMANDS`)
- Modify: `crates/norte-gui/src/keymap.rs` (`COMMANDS`, `gui_supplement`)
- Modify: `crates/norte-i18n/i18n/en.ftl`, `crates/norte-i18n/i18n/es.ftl`

- [ ] **Step 1: Write the failing test**

In `crates/norte-tui/src/keymap.rs` `mod tests`:

```rust
/// The six mark commands resolve in the three factory presets (#103). A
/// preset that loses one leaves the selection unreachable by keyboard,
/// which is the regression class the GUI already hit once.
#[test]
fn mark_commands_resolve_in_the_three_presets() {
    let expected = [
        (Chord::new(Mods::default(), KeyCode::Insert), "mark.toggle"),
        (
            Chord::new(Mods { ctrl: true, ..Default::default() }, KeyCode::Char('a')),
            "mark.all",
        ),
        (Chord::new(Mods::default(), KeyCode::Char('*')), "mark.invert"),
        (
            Chord::new(Mods { ctrl: true, ..Default::default() }, KeyCode::Char('A')),
            "mark.clear",
        ),
        (Chord::new(Mods::default(), KeyCode::Char('+')), "mark.pattern-add"),
        (Chord::new(Mods::default(), KeyCode::Char('-')), "mark.pattern-remove"),
    ];
    for (name, preset) in presets() {
        let eff = Effective::build_for(&preset, &[], COMMANDS, Screen::Browse)
            .unwrap_or_else(|e| panic!("preset {name}: {e}"));
        for (chord, command) in &expected {
            let mut r = Resolver::new(eff.clone());
            assert_eq!(
                r.push(*chord),
                Resolution::Run((*command).to_owned()),
                "preset {name}: {command}"
            );
        }
    }
}
```

If `Effective` is not `Clone`, rebuild it inside the inner loop instead.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p norte-tui -E 'test(mark_commands)'`
Expected: FAIL — `UnknownCommand` or an unbound chord.

- [ ] **Step 3: Add the commands to the TUI catalogue**

In `crates/norte-tui/src/keymap.rs`, extend `COMMANDS` after `"pane.names-encoding",`:

```rust
    "mark.toggle",
    "mark.all",
    "mark.invert",
    "mark.clear",
    "mark.pattern-add",
    "mark.pattern-remove",
```

- [ ] **Step 4: Bind them in the three presets**

In `crates/norte-frontend/presets/keymap/orthodox.toml`, inside `[pane] keymap`:

```toml
    { on = ["insert"], run = "mark.toggle" },
    { on = ["ctrl+a"], run = "mark.all" },
    { on = ["ctrl+A"], run = "mark.clear" },
    { on = ["*"], run = "mark.invert" },
    { on = ["plus"], run = "mark.pattern-add" },
    { on = ["-"], run = "mark.pattern-remove" },
```

`ctrl+A`, not `ctrl+shift+a`: the parser rejects `shift+<char>` outright
(`keymap.rs:151`, "usa la mayúscula") because a char already encodes shift,
and `Chord::new` drops the shift bit for `Char`. Writing it the other way
fails to build the preset.

Add the same block to `vim.toml` and `cua.toml`. Keep each preset's own idiom where it has one — in `vim`, additionally bind `{ on = ["v"], run = "mark.toggle" }` (visual-select muscle memory) and in `cua` `{ on = ["ctrl+a"], run = "mark.all" }` is already the CUA-correct chord. Do not remove any existing binding.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo nextest run -p norte-tui -E 'test(mark_commands)'`
Expected: PASS.

- [ ] **Step 6: Add the help strings**

`crates/norte-i18n/i18n/en.ftl`, next to the other `help-cmd-*`:

```
help-cmd-mark-toggle = mark the entry and move down
help-cmd-mark-all = mark every visible entry
help-cmd-mark-invert = invert the marks
help-cmd-mark-clear = clear every mark
help-cmd-mark-pattern-add = mark by pattern
help-cmd-mark-pattern-remove = unmark by pattern
```

`crates/norte-i18n/i18n/es.ftl`:

```
help-cmd-mark-toggle = marcar la entrada y bajar
help-cmd-mark-all = marcar todas las entradas visibles
help-cmd-mark-invert = invertir las marcas
help-cmd-mark-clear = quitar todas las marcas
help-cmd-mark-pattern-add = marcar por patrón
help-cmd-mark-pattern-remove = desmarcar por patrón
```

- [ ] **Step 7: Update the GUI catalogue and shrink its supplement**

In `crates/norte-gui/src/keymap.rs`, add `"mark.all"`, `"mark.invert"`, and `"mark.clear"` to `COMMANDS` (leave `mark.pattern-*` out: the GUI has no text input, and `build_for_subset` drops the preset binding silently — see the spec). Then delete this line from `gui_supplement`:

```toml
    { on = ["insert"], run = "mark.toggle" },
```

and extend that function's rustdoc: `insert→mark.toggle salió de aquí al entrar en los presets compartidos (#103); el resto sigue siendo GUI-only.`

- [ ] **Step 8: Run the full suites**

Run: `cargo nextest run -p norte-frontend -p norte-tui -p norte-i18n`
Expected: PASS, including the locale-completeness test that demands a `help-cmd-*` per command in both locales.

The GUI is excluded from the workspace (see the memory note on `norte-gui`), so build it explicitly:
Run: `cargo nextest run --manifest-path crates/norte-gui/Cargo.toml`
Expected: PASS, including `todo_comando_gui_es_alcanzable_desde_el_preset_default`.

- [ ] **Step 9: Commit**

```bash
git add crates/norte-frontend/presets crates/norte-tui/src/keymap.rs crates/norte-gui/src/keymap.rs crates/norte-i18n/i18n
git commit -m "feat(frontend,tui,gui): mark commands in the shared presets (#103)"
```

---

### Task 6: The TUI can mark

**Files:**
- Modify: `crates/norte-tui/src/app.rs` (`Pane` delegators, near the read-only delegators ~line 100)
- Modify: `crates/norte-tui/src/main.rs` (command dispatch, near `"pane.copy" | "pane.move"` ~line 3282)
- Test: `crates/norte-tui/src/app.rs` (inline `mod tests`)

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn pane_delegates_the_mark_api() {
    let mut p = Pane::new(
        VPath::parse("mem:///").unwrap(),
        vec![e("mem:///a", EntryKind::File), e("mem:///b", EntryKind::File)],
    );
    p.mark_all();
    assert_eq!(p.marks_len(), 2);
    p.invert_marks();
    assert_eq!(p.marks_len(), 0);
    p.toggle_mark();
    assert_eq!(p.marks_len(), 1);
    p.clear_marks();
    assert_eq!(p.marks_len(), 0);
}
```

Use the `Pane` constructor the neighbouring tests use.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p norte-tui -E 'test(delegates_the_mark_api)'`
Expected: FAIL — no such methods on `Pane`.

- [ ] **Step 3: Add the delegators**

In `crates/norte-tui/src/app.rs`, with the other delegators:

```rust
    /// Togglea la marca de la entrada seleccionada. Delegado puro (#103).
    pub fn toggle_mark(&mut self) {
        self.state.toggle_mark();
    }

    /// Marca todas las entradas visibles. Delegado puro (#103).
    pub fn mark_all(&mut self) {
        self.state.mark_all();
    }

    /// Invierte las marcas de las entradas visibles. Delegado puro (#103).
    pub fn invert_marks(&mut self) {
        self.state.invert_marks();
    }

    /// Quita todas las marcas. Delegado puro (#103).
    pub fn clear_marks(&mut self) {
        self.state.clear_marks();
    }

    /// Marca/desmarca por glob; devuelve cuántas marcas cambió (#103).
    ///
    /// # Errors
    /// Si el patrón no compila.
    pub fn mark_glob(
        &mut self,
        pattern: &str,
        mark: bool,
    ) -> Result<usize, norte_frontend::PatternError> {
        self.state.mark_glob(pattern, mark)
    }

    /// Cuántas entradas marcadas. Delegado puro (#103).
    #[must_use]
    pub fn marks_len(&self) -> usize {
        self.state.marks_len()
    }

    /// Tamaño total de los FICHEROS marcados. Delegado puro (#103).
    #[must_use]
    pub fn marked_bytes(&self) -> u64 {
        self.state.marked_bytes()
    }

    /// ¿Marcada? Delegado puro (#103).
    #[must_use]
    pub fn is_marked(&self, entry: &Entry) -> bool {
        self.state.is_marked(entry)
    }

    /// Sobre qué opera la acción: marcas, o cursor si no hay. Delegado puro (#103).
    #[must_use]
    pub fn marked_paths(&self) -> Vec<VPath> {
        self.state.marked_paths()
    }
```

Re-export `PatternError` from `norte-frontend`'s `lib.rs` if it is not already public there (`pub use pane::{PaneState, PatternError};`).

- [ ] **Step 4: Dispatch the commands**

In `crates/norte-tui/src/main.rs`, in the same `match cmd` that holds `"pane.copy" | "pane.move"`:

```rust
        "mark.toggle" => {
            app.focused_mut().toggle_mark();
            app.focused_mut().cursor_down();
        }
        "mark.all" => app.focused_mut().mark_all(),
        "mark.invert" => app.focused_mut().invert_marks(),
        "mark.clear" => app.focused_mut().clear_marks(),
```

Use the crate's real mutable-focus accessor and cursor-down method — read the neighbouring `"cursor.down"` arm and copy its exact call. `mark.pattern-add` / `mark.pattern-remove` land in Task 9.

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p norte-tui`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-tui/src/app.rs crates/norte-tui/src/main.rs crates/norte-frontend/src/lib.rs
git commit -m "feat(tui): mark, mark all, invert, clear (#103)"
```

---

### Task 7: Marked rows are visible

**Files:**
- Modify: `crates/norte-tui/src/ui.rs` (`entry_item` ~line 1246, its call sites ~line 1220-1236)
- Test: `crates/norte-tui/src/ui.rs` (inline `mod tests`)

- [ ] **Step 1: Write the failing test**

```rust
/// A marked row carries a TEXTUAL cue, never colour alone: `Role::Mark`'s
/// monochrome fallback is `dim`, which on its own reads as "inactive"
/// rather than "selected" (#103).
#[test]
fn a_marked_row_starts_with_the_mark_gutter() {
    let entry = e("mem:///a", EntryKind::File);
    let theme = TuiTheme::default();
    let marked = entry_item(&entry, &theme, None, None, true);
    let plain = entry_item(&entry, &theme, None, None, false);
    assert_eq!(first_span_text(&marked), "*");
    assert_eq!(first_span_text(&plain), " ");
}

/// The gutter goes BEFORE the hostile badge, so the badge column and the
/// decorator badge keep the positions they have today.
#[test]
fn the_gutter_precedes_the_hostile_badge() {
    let entry = e_hostile();
    let theme = TuiTheme::default();
    let item = entry_item(&entry, &theme, None, None, true);
    let texts = span_texts(&item);
    assert_eq!(texts[0], "*");
    assert_eq!(texts[1], HOSTILE_BADGE);
}
```

`first_span_text` / `span_texts` / `e_hostile` may not exist — write the tiny helpers next to the test using ratatui's `ListItem`/`Line` accessors, following whatever the existing render tests in this file do to inspect spans.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p norte-tui -E 'test(gutter)'`
Expected: FAIL — `entry_item` takes four arguments.

- [ ] **Step 3: Implement**

Change the signature and body of `entry_item`:

```rust
fn entry_item<'a>(
    entry: &'a norte_proto::Entry,
    theme: &TuiTheme,
    reinterpret: Option<norte_encoding::NameEncoding>,
    decoration: Option<&norte_frontend::Decoration>,
    marked: bool,
) -> ListItem<'a> {
```

and, where `let mut spans = vec![badge, body];` is built, put the gutter first:

```rust
    // Canalón de marca (#103): señal TEXTUAL, jamás solo color — el fallback
    // monocromo de `Role::Mark` es `dim`, que por sí solo se lee «inactivo»,
    // no «seleccionado». Va ANTES del badge hostil para que ni el badge ni la
    // decoración cambien de columna respecto a como se pintaban.
    let gutter = Span::styled(if marked { "*" } else { " " }, theme.role(Role::Mark));
    let mut spans = vec![gutter, badge, body];
```

At both call sites in `draw_pane`, pass `pane.is_marked(e)`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p norte-tui`
Expected: PASS. Snapshot-style render tests elsewhere in the file may need their expected strings widened by one column — update them; do not weaken the assertions.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-tui/src/ui.rs
git commit -m "feat(tui): mark gutter on the listing rows (#103)"
```

---

### Task 8: Size formatting and the status-bar counter

**Files:**
- Create: `crates/norte-frontend/src/format.rs`
- Modify: `crates/norte-frontend/src/lib.rs` (`mod format;` + re-export)
- Modify: `crates/norte-tui/src/ui.rs` (`draw_status` ~line 1290)
- Modify: `crates/norte-i18n/i18n/{en,es}.ftl`
- Test: `crates/norte-frontend/src/format.rs`, `crates/norte-tui/src/ui.rs`

- [ ] **Step 1: Write the failing tests**

`crates/norte-frontend/src/format.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_under_a_kilobyte_are_exact() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(999), "999 B");
    }

    #[test]
    fn larger_sizes_get_one_decimal_and_a_binary_unit() {
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1.0 MiB");
    }

    #[test]
    fn the_largest_u64_does_not_panic_or_overflow() {
        assert_eq!(human_bytes(u64::MAX), "16.0 EiB");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p norte-frontend -E 'test(human_bytes)'`
Expected: FAIL — the module does not exist.

- [ ] **Step 3: Implement the formatter**

`crates/norte-frontend/src/format.rs`:

```rust
//! Presentation formatting shared by the frontends. Lives here, not in a
//! frontend, so the TUI, the GUI, and the columns work (#108) format a size
//! the same way instead of growing three formatters.

/// Human-readable byte size: exact below 1 KiB, one decimal and a binary
/// unit above. Never panics and never overflows — `u64::MAX` is `16.0 EiB`.
///
/// The unit is NOT localised: `KiB`/`MiB` are the same token in every locale
/// norte ships, and a translated unit would make sizes incomparable between
/// screenshots and bug reports. The surrounding sentence IS localised.
///
/// ```
/// use norte_frontend::human_bytes;
/// assert_eq!(human_bytes(1536), "1.5 KiB");
/// ```
#[must_use]
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 7] = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB", "ZiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1024.0;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}
```

In `crates/norte-frontend/src/lib.rs`, add `mod format;` and `pub use format::human_bytes;`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p norte-frontend -E 'test(human_bytes)'`
Expected: PASS.

- [ ] **Step 5: Add the status strings**

`en.ftl`:

```
status-marked = { $n } marked, { $size }
```

`es.ftl`:

```
status-marked = { $n } marcadas, { $size }
```

- [ ] **Step 6: Show the counter**

In `draw_status` in `crates/norte-tui/src/ui.rs`, where `pos_total` is built, add the marked segment and append it to the status line next to `pos_total`:

```rust
    // Marcas (#103): cuántas y cuánto pesan. Se calla con 0 marcas — la
    // barra no gana ruido para quien no marca nada.
    let marked = if pane.marks_len() == 0 {
        String::new()
    } else {
        format!(
            "  {}",
            ta(
                "status-marked",
                &[
                    ("n", &pane.marks_len().to_string()),
                    ("size", &norte_frontend::human_bytes(pane.marked_bytes())),
                ],
            )
        )
    };
```

Then include `{marked}` in the same `format!` that already renders `{pos_total}`.

- [ ] **Step 7: Write and run the status test**

```rust
#[test]
fn the_status_bar_reports_marks_only_when_there_are_any() {
    let mut app = app_with_sized_entries(vec![("a", 10), ("b", 20)]);
    assert!(!status_text(&app).contains("marked"));
    app.focused_mut().mark_all();
    let text = status_text(&app);
    assert!(text.contains('2'), "count: {text}");
    assert!(text.contains("30 B"), "size: {text}");
}
```

Write `app_with_sized_entries` (entries with an explicit `size`) and `status_text` next to the test; if the file has no render helper yet, draw `draw_status` into a `ratatui::backend::TestBackend` buffer the way other render tests here do. Keep the name distinct from Task 9's `app_with_entries(&[&str])` — two helpers, two signatures, no shadowing.

Run: `cargo nextest run -p norte-tui -p norte-frontend`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/norte-frontend/src/format.rs crates/norte-frontend/src/lib.rs crates/norte-tui/src/ui.rs crates/norte-i18n/i18n
git commit -m "feat(frontend,tui): shared byte formatting and the marked counter (#103)"
```

---

### Task 9: The pattern dialog

**Files:**
- Modify: `crates/norte-tui/src/app.rs` (`Modal`, `dialog_action` allowlists)
- Modify: `crates/norte-tui/src/main.rs` (dispatch + key handling)
- Modify: `crates/norte-tui/src/ui.rs` (render)
- Modify: `crates/norte-i18n/i18n/{en,es}.ftl`
- Test: `crates/norte-tui/src/app.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_pattern_modal_marks_and_reports_how_many() {
    let mut app = app_with_entries(&["a.rs", "b.rs", "c.txt"]);
    app.open_mark_pattern(true);
    assert!(matches!(app.modal, Some(Modal::MarkPattern { mark: true, .. })));
    app.mark_pattern_push('*');
    app.mark_pattern_push('.');
    app.mark_pattern_push('r');
    app.mark_pattern_push('s');
    let changed = app.mark_pattern_confirm().expect("valid glob");
    assert_eq!(changed, 2);
    assert!(app.modal.is_none());
    assert_eq!(app.focused().marks_len(), 2);
}

#[test]
fn an_invalid_pattern_keeps_the_modal_open_and_marks_nothing() {
    let mut app = app_with_entries(&["a.rs"]);
    app.open_mark_pattern(true);
    app.mark_pattern_push('[');
    assert!(app.mark_pattern_confirm().is_err());
    assert!(app.modal.is_some(), "the user keeps their text to fix it");
    assert_eq!(app.focused().marks_len(), 0);
}

#[test]
fn the_pattern_modal_cancels_without_marking() {
    let mut app = app_with_entries(&["a.rs"]);
    app.open_mark_pattern(true);
    app.mark_pattern_push('*');
    app.close_modal();
    assert_eq!(app.focused().marks_len(), 0);
}
```

Reuse the `app_with_entries` helper the existing modal tests use, and its real `close_modal` equivalent.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p norte-tui -E 'test(pattern_modal)'`
Expected: FAIL — no `Modal::MarkPattern`.

- [ ] **Step 3: Add the modal**

In `crates/norte-tui/src/app.rs`, in `enum Modal`:

```rust
    /// Marcar (`mark = true`) o desmarcar por patrón (`+`/`-`, #103). El
    /// texto es la query CRUDA del usuario; se enmascara al pintarla, igual
    /// que el quick search (un patrón puede llegar por PASTE con bidi o
    /// invisibles).
    MarkPattern {
        /// Marcar, o desmarcar.
        mark: bool,
        /// Lo tecleado hasta ahora.
        pattern: String,
        /// Diagnóstico del último intento fallido, para pintarlo bajo el
        /// campo. `None` = aún no se ha confirmado nada.
        error: Option<String>,
    },
```

Add the `App` methods next to the other modal openers:

```rust
    /// Abre el modal de marcado por patrón (#103).
    pub fn open_mark_pattern(&mut self, mark: bool) {
        self.modal = Some(Modal::MarkPattern {
            mark,
            pattern: String::new(),
            error: None,
        });
    }

    /// Añade un carácter al patrón en curso. No-op sin modal de patrón.
    pub fn mark_pattern_push(&mut self, c: char) {
        if let Some(Modal::MarkPattern { pattern, error, .. }) = &mut self.modal {
            pattern.push(c);
            *error = None;
        }
    }

    /// Borra el último carácter del patrón. No-op sin modal de patrón.
    pub fn mark_pattern_pop(&mut self) {
        if let Some(Modal::MarkPattern { pattern, error, .. }) = &mut self.modal {
            pattern.pop();
            *error = None;
        }
    }

    /// Aplica el patrón: cierra el modal y devuelve cuántas marcas cambió.
    /// Un patrón inválido DEJA el modal abierto con el diagnóstico — el
    /// usuario conserva lo tecleado para corregirlo.
    ///
    /// # Errors
    /// Si el glob no compila.
    pub fn mark_pattern_confirm(&mut self) -> Result<usize, norte_frontend::PatternError> {
        let Some(Modal::MarkPattern { mark, pattern, .. }) = &self.modal else {
            return Ok(0);
        };
        let (mark, pattern) = (*mark, pattern.clone());
        match self.focused_mut().mark_glob(&pattern, mark) {
            Ok(changed) => {
                self.modal = None;
                Ok(changed)
            }
            Err(e) => {
                if let Some(Modal::MarkPattern { error, .. }) = &mut self.modal {
                    *error = Some(e.to_string());
                }
                Err(e)
            }
        }
    }
```

`PatternError` is not `Clone`; if returning it after storing `e.to_string()` fights the borrow checker, format the string first and return the error afterwards.

- [ ] **Step 4: Wire the keys**

`Modal::MarkPattern` is a text-entry modal, so it must NOT go through `dialog_action`'s command allowlists — it consumes raw characters, exactly the way the `pane.search` dialog already does. In `main.rs`, intercept it in the same place the search dialog is intercepted, before the keymap `dialog` context: printable chars → `mark_pattern_push`, Backspace → `mark_pattern_pop`, Enter → `mark_pattern_confirm` (on `Ok(n)` set `app.message` from `msg-marked-by-pattern`; on `Err` leave the modal open), Esc → close.

Add the dispatch arms next to the other mark commands:

```rust
        "mark.pattern-add" => app.open_mark_pattern(true),
        "mark.pattern-remove" => app.open_mark_pattern(false),
```

- [ ] **Step 5: Render it**

In `ui.rs`, render `Modal::MarkPattern` with the same modal frame the other dialogs use: a title (`modal-mark-pattern-add` / `modal-mark-pattern-remove`), the pattern line passed through the SAME mask the quick-search input uses (`display_name(pattern.as_bytes())`), and the error line under it when present.

Strings — `en.ftl`:

```
modal-mark-pattern-add = Mark by pattern
modal-mark-pattern-remove = Unmark by pattern
modal-mark-pattern-hint = glob, for example *.rs
msg-marked-by-pattern = { $n } marks changed
```

`es.ftl`:

```
modal-mark-pattern-add = Marcar por patrón
modal-mark-pattern-remove = Desmarcar por patrón
modal-mark-pattern-hint = glob, por ejemplo *.rs
msg-marked-by-pattern = { $n } marcas cambiadas
```

- [ ] **Step 6: Run the tests**

Run: `cargo nextest run -p norte-tui`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/norte-tui/src crates/norte-i18n/i18n
git commit -m "feat(tui): mark by pattern dialog (#103)"
```

---

### Task 10: Bulk copy, move, and delete in the TUI

**Files:**
- Modify: `crates/norte-frontend/src/lib.rs` or a small new module for the item-list truncation policy
- Modify: `crates/norte-gui/src/main.rs` (use the shared truncation helper)
- Modify: `crates/norte-tui/src/app.rs` (`Modal::ConfirmTransfer`, `Modal::ConfirmDelete`)
- Modify: `crates/norte-tui/src/main.rs` (submission + conflict backlog)
- Modify: `crates/norte-tui/src/ui.rs` (modal render)
- Test: `crates/norte-tui/src/app.rs`, `crates/norte-tui/src/main.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn copy_builds_the_modal_from_every_mark() {
    let mut app = app_with_two_panes(&["a", "b", "c"], "mem:///dst");
    app.focused_mut().mark_all();
    app.open_transfer_modal(TransferKind::Copy);
    let Some(Modal::ConfirmTransfer { items, to, .. }) = &app.modal else {
        panic!("no transfer modal");
    };
    assert_eq!(items.len(), 3);
    assert_eq!(to, &VPath::parse("mem:///dst").unwrap());
}

#[test]
fn copy_without_marks_still_uses_the_cursor_entry() {
    let mut app = app_with_two_panes(&["a", "b"], "mem:///dst");
    app.open_transfer_modal(TransferKind::Copy);
    let Some(Modal::ConfirmTransfer { items, .. }) = &app.modal else {
        panic!("no transfer modal");
    };
    assert_eq!(items.len(), 1, "marked_paths falls back to the cursor");
}

#[test]
fn submitting_a_bulk_operation_consumes_the_marks() {
    let mut app = app_with_two_panes(&["a", "b"], "mem:///dst");
    app.focused_mut().mark_all();
    app.open_transfer_modal(TransferKind::Copy);
    app.consume_marks();
    assert_eq!(app.focused().marks_len(), 0);
}

#[test]
fn the_modal_lists_the_first_items_and_summarises_the_rest() {
    let many: Vec<VPath> = (0..20)
        .map(|i| VPath::parse(&format!("mem:///f{i}")).unwrap())
        .collect();
    let lines = norte_frontend::item_lines(&many);
    assert_eq!(lines.len(), norte_frontend::MODAL_ITEM_LIMIT + 1);
    assert!(lines.last().unwrap().contains(&(20 - norte_frontend::MODAL_ITEM_LIMIT).to_string()));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p norte-tui -E 'test(bulk) or test(modal)'`
Expected: FAIL — `Modal::ConfirmTransfer` has `from`/`to`, and `item_lines` is GUI-private.

- [ ] **Step 3: Move the truncation policy into `norte-frontend`**

Cut `MODAL_ITEM_LIMIT` and `item_lines` out of `crates/norte-gui/src/main.rs:3398-3420` into `norte-frontend` (a `modal.rs` module, re-exported from `lib.rs`), keeping their behaviour byte-for-byte, including the masking each line already applies. Make the GUI call the shared ones. Its existing tests for those helpers move with them.

- [ ] **Step 4: Change the TUI modal shapes**

```rust
    /// Confirmación de copia/movimiento sobre las MARCAS (#103). `to` es el
    /// DIRECTORIO destino (el del otro pane): con varios ítems no hay un
    /// nombre único que editar. El destino editable de un solo ítem, y el
    /// rename que trae, viven en #105.
    ConfirmTransfer {
        /// Copy o Move.
        kind: TransferKind,
        /// Los orígenes, en orden de listado.
        items: Vec<VPath>,
        /// Directorio destino.
        to: VPath,
    },
    /// Confirmación de borrado (F8) sobre las MARCAS. `permanent = false` →
    /// papelera.
    ConfirmDelete {
        /// Los ítems a borrar, en orden de listado.
        items: Vec<VPath>,
        /// Permanente (shift+F8, o sin papelera en el provider).
        permanent: bool,
    },
```

Replace the `"pane.copy" | "pane.move"` arm in `main.rs`:

```rust
        "pane.copy" | "pane.move" => {
            let kind = if cmd == "pane.copy" {
                TransferKind::Copy
            } else {
                TransferKind::Move
            };
            // Destino ortodoxo: el DIRECTORIO del otro pane. Los orígenes son
            // las marcas, o el cursor si no hay ninguna (#103).
            let items = app.focused().marked_paths();
            if !items.is_empty() {
                let to = app.panes[1 - app.focus()].dir().clone();
                app.modal = Some(Modal::ConfirmTransfer { kind, items, to });
            }
        }
```

and the delete arm equivalently, keeping its existing `TRASH` capability probe — probe it once for the batch, using the first item's path.

Add the helper the submission path and the test both use:

```rust
    /// Las marcas las CONSUME la operación (mc/Total Commander): se limpian
    /// al ENVIAR el lote, no al completarse, para que jamás exista una
    /// selección a medio consumir cuyo significado dependa de qué task
    /// terminó (#103).
    pub fn consume_marks(&mut self) {
        self.focused_mut().clear_marks();
    }
```

- [ ] **Step 5: Submit one task per item, queue the conflicts**

Where the confirmed modal is turned into engine calls, iterate `items`, joining each item's file name onto `to` for the destination, and submit one call per item. On a collision, queue rather than replace the open modal — mirror `queue_conflict` / `open_next_conflict` from `crates/norte-gui/src/main.rs:975-990`, carrying the full `RetrySpec` the existing `Modal::Collision` already expects. Call `app.consume_marks()` once the batch is submitted.

- [ ] **Step 6: Render the item list**

In `ui.rs`, render `ConfirmTransfer` / `ConfirmDelete` with `norte_frontend::item_lines(items)` — one path per line, already masked and truncated, never a joiner in the same line (the encoding audit rule the GUI modal already follows).

- [ ] **Step 7: Run everything**

Run: `cargo nextest run -p norte-tui -p norte-frontend`
Run: `cargo nextest run --manifest-path crates/norte-gui/Cargo.toml`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/norte-frontend/src crates/norte-tui/src crates/norte-gui/src/main.rs
git commit -m "feat(tui,frontend): bulk copy, move, and delete over the selection (#103)"
```

---

### Task 11: The GUI gets all, invert, and clear

**Files:**
- Modify: `crates/norte-gui/src/main.rs` (dispatch ~line 1175)
- Test: `crates/norte-gui/src/main.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn the_gui_dispatches_the_bulk_mark_commands() {
    let mut app = test_app_with_entries(&["a", "b"]);
    app.run_command("mark.all");
    assert_eq!(app.panes[app.focus].marks_len(), 2);
    app.run_command("mark.invert");
    assert_eq!(app.panes[app.focus].marks_len(), 0);
    app.run_command("mark.all");
    app.run_command("mark.clear");
    assert_eq!(app.panes[app.focus].marks_len(), 0);
}
```

Use the harness the neighbouring GUI command tests use to build an app and feed it a command name.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run --manifest-path crates/norte-gui/Cargo.toml -E 'test(bulk_mark)'`
Expected: FAIL — the commands are no-ops.

- [ ] **Step 3: Implement**

Next to `"mark.toggle" => self.panes[f].toggle_mark(),`:

```rust
            "mark.all" => self.panes[f].mark_all(),
            "mark.invert" => self.panes[f].invert_marks(),
            "mark.clear" => self.panes[f].clear_marks(),
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo nextest run --manifest-path crates/norte-gui/Cargo.toml`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-gui/src/main.rs
git commit -m "feat(gui): mark all, invert, and clear (#103)"
```

---

### Task 12: Close out

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `docs/superpowers/specs/2026-07-25-first-class-selection-design.md` (only if the implementation diverged)

- [ ] **Step 1: Update the changelog**

Add under the unreleased section:

```markdown
### Added
- First-class selection (#103): mark, mark all, invert, clear, and mark or
  unmark by glob, in both frontends; marks survive a refresh and copy, move,
  and delete operate on the whole selection. The pattern dialog is TUI-only
  for now — the GUI has no text input yet.
```

- [ ] **Step 2: Run the full local gate**

Run: `just ci`
Expected: EXIT=0. This is the once-per-change full run — do not loop it. If coverage on `norte-frontend` regressed, add the missing unit test rather than lowering a threshold.

- [ ] **Step 3: Reconcile the spec**

Re-read the design doc against what was built. If anything diverged, edit the spec to say what is true and note why; do not leave the spec describing a design nobody implemented.

- [ ] **Step 4: Commit**

```bash
git add CHANGELOG.md docs/superpowers/specs
git commit -m "docs: changelog and spec reconciliation for first-class selection (#103)"
```

- [ ] **Step 5: Review**

Request a `rust-reviewer` pass over the whole diff, and an `encoding-auditor` pass specifically over Task 3 (glob folding), Task 7 (gutter and hostile badge order), and Task 10 (item lists in modals). Apply what they find before merging.
