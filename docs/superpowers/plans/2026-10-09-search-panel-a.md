# Search panel, phase A — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A **Search** panel in the rail of both frontends that searches as you type (300 ms debounce, previous run cancelled), shows every matching line of every file as a folder → file → line tree with the match highlighted, and opens norte's viewer at the line with the match highlighted.

**Architecture:** The engine learns to report every matching LINE of a file (up to a cap, the rest counted) with the line's byte offset and the match ranges inside a preview centred on the match (protocol 0.86, additive). A pure model in `norte-frontend` (`search_panel.rs`) owns the form, the debounce, the epoch, the grouping into a tree and the activation; the TUI paints it in cells, the host projects it as `SlotView::Search` for the webview. The shared `Viewer` learns a `ViewerTarget` (scroll to a line, highlight ranges, or load a window past its 256 KiB cap from the line's offset).

**Tech Stack:** Rust 2024 workspace (`norte-proto`, `norte-core`, `norte-frontend`, `norte-tui` on ratatui, `norte-ui-host`), TypeScript webview (`crates/norte-gui-tauri/ui`, vitest + jsdom), Fluent (`crates/norte-i18n/i18n/{en,es}.ftl`).

**Spec:** `/tmp/claude-1000/-home-oscar-work-wot-projects-high-getnorte-web/46f24dcf-65a3-4fef-97e6-717f92cd1b04/scratchpad/2026-10-09-search-panel-a-design.md` (copy it to `docs/superpowers/specs/2026-10-09-search-panel-a-design.md` in Task 1's commit so the plan and the spec travel with the branch). Read both before any task.

**User rulings after planning (binding, 2026-10-09):** (1) the panel's key is `alt+F` (Alt+Shift+F) in all seven presets, as the plan's C1 proposes; (2) this plan runs AFTER the omnibox plan (`feat/omnibox`), which takes bridge 108 — so wherever this plan says bridge 108, use the next free number at execution time (expected **109**); rebase on `main` with the omnibox merged before starting.

**Repository:** `/home/oscar/work/wot/projects/high/norte`, branch `feat/search-panel` from `main`, in its own worktree (`scripts/wt.sh search-panel`): another session is editing the main tree.

## Global Constraints

- Toolchain loop: `just t <crate>` (nextest; never bare `cargo test` for the suite), `just c` when lint surface was touched, `cargo check -p <crate> --all-targets` for "does it compile". Doctests are NOT run by `just t`: after adding a field to any public struct run `cargo test --workspace --exclude norte-gui-tauri --doc`. After writing an intra-doc link run `cargo doc -p <crate> --no-deps`.
- Gate budget: `just ci-fast` ONCE every ~3 tasks (after Tasks 4, 8 and 10), never as a debugger. `just gui-ci` once after Task 11 (the webview was touched). Pushing `main` IS the gate.
- Editing through Bash is blocked by a hook: use Edit/Write. Use `rg`, not `grep`. Commit messages contain `<…>` (the trailer): always `git commit -F <file>`. Read `git diff --cached --stat` before every commit.
- Every commit: Conventional Commits in English, ending with the line `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Hard rules: filenames are bytes (`VPath`, `display_lossy` only for display); no blocking I/O in async; every long operation is a cancellable task with a cancel test; no `unwrap()`/`expect()` outside tests without an invariant comment; typed errors; user-facing strings through Fluent in BOTH `en.ftl` and `es.ftl`.
- Protocol: `PROTOCOL_VERSION = "0.86.0"` (from 0.85.0) with a `# 0.86.0` rustdoc section above it in `crates/norte-proto/src/methods.rs`; every change additive and optional (ADR 0004); goldens `crates/norte-proto/tests/golden_types.rs` + `crates/norte-proto/tests/golden/types/methods.json` (hand-edited), schema regenerated with `NORTE_UPDATE_SCHEMA=1 just t norte-proto` (`docs/schema/proto.schema.json`, ADR 0038). `protocol-guardian` review is mandatory for Tasks 2–3.
- Bridge: `BRIDGE_VERSION` 107 → **108** in `crates/norte-ui-host/src/bridge.rs` with a `- **108**:` doc entry, mirrored ONLY in `crates/norte-gui-tauri/ui/src/types.ts:13` (`catalog.rs` and `wire_catalogue.rs` read the constant). One bump for the whole plan (Tasks 9–11 share it). Host goldens re-blessed with `NORTE_BLESS=1 just t norte-ui-host`.
- Numbers from the spec, verbatim: debounce **300 ms**; **50** matching lines shown per file, the request asks for **51**; daemon cap on `max_matches_per_file` **1000**; preview **~160 chars** centred on the first range; viewer cap **256 KiB** (`VISOR_CAP`, `VIEW_CAP`); result cap **2000** files (the host's `MAX_RESULTS`).
- Key binding (see Contradiction C1): `layout.search` on **`alt+F`** in `[global]` of all seven presets (`orthodox`, `vim`, `cua`, `krusader`, `far`, `norton`, `total-commander`), next to `alt+T` (timeline).
- "One explanation, one home": the wire reasoning lives in the `# 0.86.0` rustdoc; the user-facing change in `CHANGELOG.md` `[Unreleased]`; tests' docs say what they pin; no history in comments.
- Alt+F7 / `pane.search` (the dialog) and the TUI's pane takeover stay untouched (phase B).

## Contradictions between the spec and the code (flagged, with the adjustment this plan takes)

- **C1 — Ctrl+Shift+F cannot be bound.** `parse_chord("ctrl+shift+f")` is an error (`KeymapError::ShiftWithChar`, test `shift_with_char_is_a_diagnosable_error` in `crates/norte-frontend/src/keymap/mod.rs`), and `ctrl+F` is rejected by `no_preset_binds_a_chord_the_terminal_cannot_deliver` (`keymap/presets.rs`): a terminal sends the same byte for Ctrl+F and Ctrl+Shift+F. **Adjustment:** `alt+F` (free in all seven presets, the same shifted-alt family as `alt+T`/`alt+I`), bound in ALL seven, Krusader included (Krusader does not bind Alt+F7 at all — it opens the dialog with `ctrl+s` — so "Krusader keeps Alt+F7" has nothing to keep).
- **C2 — "An older daemon ignores the field and the client degrades" cannot happen over a negotiated connection.** `version_compatible(server, client)` (methods.rs ~1578) accepts a client whose minor is equal to or ONE BEHIND the daemon's, never ahead: a 0.86 client against a 0.85 daemon gets `VERSION_MISMATCH` at `initialize`. Only "0.85 client against 0.86 daemon" exists, and that client never sends `max_matches_per_file`. **Adjustment:** no SDK gate; the model still tolerates absent `ranges`/`offset`/`all_matches`/`more` (they are `Option`), pinned by a unit test that feeds it a 0.85-shaped `SearchHits`. The `# 0.86.0` rustdoc says this plainly. The spec's "0.86 client against an 0.85 fixture (degrades)" test becomes "a 0.85-shaped batch deserialises into 0.86 types and the panel shows it unhighlighted".
- **C3 — `field_unreadable` does not validate regexes.** The spec's "regex invalid while typing: the field shows the error (`field_unreadable`)" relies on something that does not exist (it checks sizes, days, exclusions and the encoding only). **Adjustment:** a new `SearchForm::regex_unreadable() -> Option<SearchField>` in `norte-frontend/src/search.rs` (compiles name/content with `regex::Regex` when `form.regex`; `regex` is already a dependency of the crate); the panel checks both before launching. `field_unreadable` and the dialog are untouched.
- **C4 — There is no "re-point" of a board entry in either frontend.** The host board is `BTreeMap<u64 /*task id*/, TaskViva>` (`controller/mod.rs` ~3072); entries are keyed by task id. **Adjustment:** in the window, "one entry" is achieved by REMOVING the previous run's row when the next run goes live (Task 10), so the board holds at most the panel's live run. The TUI puts no search on its board today (the Alt+F7 run never did) and its board has no remove-by-id, so the panel follows suit (D3). Progress for a removed id is already ignored (`tasks.rs` `progress`: `let Some(viva) = self.tasks.get_mut(..) else { return Vec::new() }`).
- **C5 — The host has no tick.** Everything is an epoch-tagged mailbox message. **Adjustment:** each edit spawns `sleep(DEBOUNCE)` → `Message::SearchPanelTic` (the `LogTic` pattern, `logpanel.rs` `probe_log`); the handler calls `due(Instant::now())`, which fires only once the LAST edit's deadline has passed.
- **C6 — The viewer paints no line numbers.** "line numbers continue from the known line" has no gutter to continue. **Adjustment:** `Viewer::first_line()` (1 normally, N for a window loaded past the cap) feeds the title ("lines from N", `viewer-lines-from` Fluent key) in both frontends.
- **C7 — Preview ranges cannot be re-found by "the matched text" alone** when the same text occurs twice on the line. **Adjustment:** `ViewerTarget` also carries the `preview` string; the viewer locates the preview (minus its `…`) inside the masked decoded line and shifts the ranges by that position; only if the preview is not found does it fall back to searching each matched substring.
- **C8 — Matches are counted in LINES.** The spec says "matches per file" and `MatchLine` is per line; a line with three occurrences is ONE `MatchInfo` with three ranges, and `more` counts further matching LINES. "+9950 more" for 10 000 matches therefore holds when each match is on its own line.
- **C9 — The decode path's `offset`** (regex searches and UTF-16 literals) is exact for UTF-8, UTF-16 and single-byte encodings and `None` for multi-byte legacy encodings (Shift_JIS, GBK…); an invalid UTF-8 byte before the match shifts it (each becomes a 3-byte U+FFFD after decoding). Documented on `MatchInfo::offset`. Without an offset the viewer past its cap falls back to opening at the top with a status note.
- **C10 — The spec's "empty name AND empty content runs nothing"** differs from `SearchForm::has_criteria`, which counts filters (size, days, kinds) as criteria. The panel follows the spec (typing in the filters alone does not search the whole tree as you type); the dialog keeps `has_criteria`.

## Review Focus

1. **A needle shorter than the overlap repeated across a chunk seam** (`"aa"` in `"aaaa…"` cut anywhere): the byte path must neither count a match twice nor miss one; the same lines/offsets whatever the cuts. → proptest in Task 2.
2. **Typing fast then stopping**: exactly one launch after the last keystroke, the previous task cancelled, the board showing one row, late batches of the old run dropped. → model test in Task 4, TUI test in Task 7, host test in Task 10.
3. **A match inside a 1 MiB minified line, or at column 5000**: the preview shows the match (centred, `…` at both ends), the range indexes chars of what is painted, the viewer scrolls to the line and highlights the right occurrence even after tab expansion and horizontal scroll. → engine test in Task 2, viewer test in Task 6.
4. **Hostile text in a matching line** (RLO, ESC, ZWSP before the match): masked to U+FFFD char-for-char so ranges still point at the needle; nothing raw reaches either renderer. → engine test in Task 2, webview tests in Tasks 9 and 11 (`<mark>` built with `textContent`, never `innerHTML`).
5. **Closing the panel, or switching its root, with a run live**: the task is cancelled, no batch repaints a closed panel, the board row goes. → TUI test in Task 7, host test in Task 10.

---

## File Structure

| file | responsibility | task |
| --- | --- | --- |
| `crates/norte-ui-host/src/controller/search.rs` | dialog search: relaunch cancels the previous run | 1 |
| `crates/norte-proto/src/methods.rs` | `MatchInfo.offset/ranges`, `FsSearchParams.max_matches_per_file`, `SearchHits.all_matches/more`, `SEARCH_MATCHES_PER_FILE_MAX`, 0.86.0 | 2, 3 |
| `crates/norte-proto/tests/{golden_types.rs,types.rs,golden/types/methods.json}`, `docs/schema/proto.schema.json` | goldens and schema | 2, 3 |
| `crates/norte-core/src/search.rs` | every matching line, ranges, offset, centred preview, per-file cap | 2, 3 |
| `crates/norte-frontend/src/search.rs` | `SearchForm::regex_unreadable` | 4 |
| `crates/norte-frontend/src/search_panel.rs` (new) | the panel model: form, debounce, epoch, tree, rows, activation, status | 4 |
| `crates/norte-frontend/src/layout/{kinds.rs,docks.rs}`, `chrome/{panelbar.rs,menu.rs}`, `keymap/catalogue.rs`, `presets/keymap/*.toml`, `assets/panel-icons/search.svg` | the `search` kind, its rail button, `layout.search` | 5 |
| `crates/norte-frontend/src/view/viewer.rs` | `ViewerTarget`, highlight, first line, window past the cap | 6 |
| `crates/norte-tui/src/searchpanel.rs` (new), `app.rs`, `app/layout.rs`, `app/focus.rs`, `keys.rs`, `keymap.rs`, `dispatch.rs`, `panel.rs`, `ui.rs`, `ui/panels.rs`, `ui/chrome.rs`, `mouse.rs`, `rail_icons.rs`, event loop | TUI panel | 5 (stub), 7 |
| `crates/norte-tui/src/viewer_open.rs`, viewer paint in `ui/panels.rs` | open at a target, highlight | 8 |
| `crates/norte-ui-host/src/controller/viewer.rs`, `dto.rs` (`ViewerView.highlight`, `lines_from`), `bridge.rs`; webview viewer renderer | viewer at a target in the window, bridge 108 | 9 |
| `crates/norte-ui-host/src/controller/searchpanel.rs` (new), `controller/{mod.rs,views.rs,tabs.rs,listing.rs,extensions.rs}`, `dto.rs`, `action.rs`, `commands.rs` | host panel, `SlotView::Search`, actions, debounce tic, one board row | 5 (command), 10 |
| `crates/norte-gui-tauri/ui/src/render/searchpanel.ts` (new), `render.ts`, `render/icons.ts`, `types.ts`, `style.css`, `tests/render.test.ts` | webview panel | 11 |
| `crates/norte-help/topics/{en,es}/finding.md`, `crates/norte-help/tests/corpus.rs`, `CHANGELOG.md`, `crates/norte-cli/tests/goldens/help-en.json` | docs | 5 (gate minimum), 12 |

Task order: 1 (host bug, independent) → 2–3 (wire + engine, independently useful) → 4 (model) → 5 (kind, rail, command, both frontends' stubs) → 6 (shared viewer) → 7–8 (TUI) → 9–10 (host) → 11 (webview) → 12 (docs).

---

### Task 1: The window's dialog search cancels the run it replaces

The bug: `launch_search` (`crates/norte-ui-host/src/controller/search.rs` ~302) overwrites `self.search` without stopping the previous walk; its batches are dropped by epoch but the daemon keeps walking. Independent of the panel; the dialog benefits.

**Files:**
- Modify: `crates/norte-ui-host/src/controller/search.rs` (`launch_search`)
- Test: `crates/norte-ui-host/tests/controller/` — the module that already tests the dialog search (find it with `rg -l "search_activate_row|launch_search|Pending::Search" crates/norte-ui-host/tests`); fake backend in `crates/norte-ui-host/tests/backend_fake/mod.rs` (~1068 sends `SearchHits`).
- Also: copy the spec to `docs/superpowers/specs/2026-10-09-search-panel-a-design.md` and this plan to `docs/superpowers/plans/2026-10-09-search-panel-a.md`.

**Interfaces:**
- Produces: `State::abandon_search(&mut self)` — marks the current `Search` abandoned and cancels its task if the id is known. Task 9 reuses the same shape for the panel.

- [ ] **Step 1: Write the failing test.** In the dialog-search test module, launch a search twice through the dialog (the existing helper that confirms the search dialog; if none, drive `UiAction::DialogField` + `UiAction::Dialog { choice: "confirm" }` as the existing search tests do). The fake backend must record cancellations: if `backend_fake` has no counter for search cancels, add `pub search_cancels: Arc<AtomicUsize>` to the fake and increment it inside the `cancel` closure of the `HostTask` it returns from `search`.

```rust
/// A second search replaces the first: the first walk is CANCELLED in the
/// daemon, not just ignored by epoch. Before the fix only the epoch
/// changed, and a search-as-you-type would leave one live walk per
/// keystroke.
#[tokio::test]
async fn relaunching_a_search_cancels_the_previous_task() {
    let (mut h, fake) = harness_with_fake().await; // the module's existing constructor
    h.confirm_search("uno").await;
    h.settle().await; // the first task's id is known (SearchViva handled)
    h.confirm_search("dos").await;
    h.settle().await;
    assert_eq!(fake.search_cancels.load(Ordering::SeqCst), 1);
}

/// And when the second launch arrives before the first task's id is known,
/// the forwarder cancels it as soon as it lands (`abandoned`).
#[tokio::test]
async fn relaunching_before_the_id_is_known_still_cancels() {
    let (mut h, fake) = harness_with_fake().await;
    fake.delay_search_ack(); // hold the first `search` future until released
    h.confirm_search("uno").await;
    h.confirm_search("dos").await;
    fake.release_search_ack();
    h.settle().await;
    assert_eq!(fake.search_cancels.load(Ordering::SeqCst), 1);
}
```

Name the helpers after what the module already has; `delay_search_ack`/`release_search_ack` are new on the fake only if the second case cannot be reached otherwise (a `tokio::sync::Notify` the fake's `search` awaits).

- [ ] **Step 2: Run** `just t norte-ui-host` — expected FAIL: `search_cancels` is 0.
- [ ] **Step 3: Implement** in `search.rs`:

```rust
/// Lets go of the current file search, if any: the forwarder stops
/// (`abandoned`) and the task is cancelled in the daemon once its id is
/// known — before that, the forwarder cancels it as soon as it lands.
pub(super) fn abandon_search(&mut self) {
    let Some(b) = self.search.take() else { return };
    b.abandoned.store(true, std::sync::atomic::Ordering::SeqCst);
    if b.task.get() != 0 {
        let _ = self.cancel(b.task.get());
    }
}
```

Call `self.abandon_search();` as the FIRST line of `launch_search`, and replace the hand-written copies in the Escape branch of `key_in_search` and in `go_to_result` with it (keep `semantics_in_flight.take()` where it is).

- [ ] **Step 4: Run** `just t norte-ui-host` — PASS.
- [ ] **Step 5: Commit** (`fix(ui-host): a new search cancels the walk it replaces`; body: what broke — the old walk kept running in the daemon, its batches dropped by epoch; not covered: semantic queries, which are aborted already).

---

### Task 2: The engine reports every matching line with its offset and match ranges (protocol 0.86.0)

**Files:**
- Modify: `crates/norte-proto/src/methods.rs` (`MatchInfo` ~3273, `PROTOCOL_VERSION` 1428 + `# 0.86.0` section above it)
- Modify: `crates/norte-core/src/search.rs` (`ContentRegex`, `SearchMatchers`, `Batch::push`, `run_walk`'s `settle!`/`hit!`, `search_content` 1030, `scan_bytes` 1100, `scan_decode_lines` 1198, `trim_preview` 1313, `extract_line_preview` 1324; tests module)
- Modify: `crates/norte-proto/tests/golden_types.rs` (`match_info`, `search_hits` cases ~3758-3782; version assert ~4971), `crates/norte-proto/tests/golden/types/methods.json` (`match_info` ~900, `search_hits` ~1477), `crates/norte-proto/tests/types.rs` (~1436-1442 window), `docs/schema/proto.schema.json`
- Modify (literal sites that break): `crates/norte-tui/tests/app.rs:206`, `crates/norte-tui/tests/render.rs:958` → add `..Default::default()`.

**Interfaces:**
- Produces (proto): `MatchInfo { line: Option<u64>, preview: Option<String>, offset: Option<u64>, ranges: Option<Vec<(u32, u32)>> }`, now `#[derive(Default)]`.
- Produces (core, private): `struct FileMatches { lines: Vec<MatchInfo>, more: u32 }`, `struct Want { keep: usize, count_more: bool }`, `SearchMatchers::want(&self) -> Want` (returns `{ keep: 1, count_more: false }` in this task; Task 3 makes it follow the request), `search_content(..) -> Result<Option<FileMatches>, Error>`.

- [ ] **Step 1: Proto fields.** In `MatchInfo` add `Default` to the derive and, after `preview`:

```rust
    /// Byte offset in the file of the START of the matching line (0.86.0):
    /// what a viewer needs to load the window holding a line past what it
    /// reads from the top.
    ///
    /// Exact on the literal path. On the decoded path (regex, UTF-16) it is
    /// counted over the decoded text: exact for UTF-8, UTF-16 and
    /// single-byte encodings, absent for multi-byte legacy ones (Shift_JIS,
    /// GBK…), and shifted by two bytes per invalid UTF-8 byte before the
    /// match (each decodes to a three-byte `U+FFFD`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    /// Where the match is inside [`Self::preview`] (0.86.0): `[start, end)`
    /// in CHARS of the preview as sent — after masking and after the
    /// centring that may have put a `…` in front. Every occurrence on the
    /// line that falls inside the preview; empty when the line matched but
    /// the text could not be located in it (a literal found as bytes of
    /// another encoding).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranges: Option<Vec<(u32, u32)>>,
```

Fix `MatchInfo { line, preview }` literal sites (`rg -n "MatchInfo \{" crates`) with `..MatchInfo::default()` or the new fields.

- [ ] **Step 2: Version.** `PROTOCOL_VERSION = "0.86.0"`; above it add the section (Task 3 extends it):

```rust
/// # 0.86.0 — a search says WHERE it matched
///
/// [`MatchInfo::offset`] and [`MatchInfo::ranges`], optional, and the
/// preview is now a window CENTRED on the first match (`…` at a cut end)
/// instead of the line's first 160 chars, so a match at column 200 is
/// visible. Old clients read the same field with better content.
///
/// A **0.85 client against a 0.86 daemon** ignores the two fields (ADR
/// 0004). A **0.86 client against a 0.85 daemon** does not negotiate
/// ([`version_compatible`] never accepts a client ahead of the daemon), so
/// no client has to degrade on the wire; a frontend that gets a batch
/// without them shows the lines unhighlighted.
```

Update `golden_types.rs` (~4971) to `"0.86.0"` with a `// 0.86.0: no new method. fs.search says where it matched.` line, and `types.rs` (~1436) to `version_compatible(PROTOCOL_VERSION, "0.86.9")` "N", `"0.85.0"` "N-1", `!"0.84.9"` "N-2".

- [ ] **Step 3: Goldens.** In `golden_types.rs` the `match_info` and the `search_hits` cases get `offset: Some(41), ranges: Some(vec![(7, 10)])` (`"hay un año aquí"`: `año` is chars 7..10); add the same keys to `methods.json` (`"offset": 41, "ranges": [[7, 10]]`). Add to `types.rs`:

```rust
/// A 0.85 batch — no offset, no ranges — still reads, with both absent:
/// that is the shape a panel shows unhighlighted.
#[test]
fn a_085_match_info_reads_without_offset_or_ranges() {
    let m: MatchInfo = serde_json::from_str(r#"{"line":3,"preview":"hay"}"#).expect("0.85 shape");
    assert_eq!((m.offset, m.ranges), (None, None));
}
```

Run `just t norte-proto` — the golden must fail first if fixtures were not updated; then `NORTE_UPDATE_SCHEMA=1 just t norte-proto`, review `git diff docs/schema/proto.schema.json` (only the two fields), `just t norte-proto` PASS.

- [ ] **Step 4: Engine failing tests.** Add to the tests module of `crates/norte-core/src/search.rs`. First the helpers (replace the existing `scan_cut` with a wrapper over `scan_cut_with` so the existing proptest keeps compiling):

```rust
    const ONE: Want = Want { keep: 1, count_more: false };
    const ALL: Want = Want { keep: 1000, count_more: true };

    fn spans_for(needle: &str, cs: bool) -> ContentRegex {
        ContentRegex::new(&regex::escape(needle), cs).expect("literal regex")
    }

    /// `scan_bytes` over `content` cut at `cuts`, keeping what `want` asks.
    fn scan_cut_with(
        needle: &ContentNeedle,
        content: &[u8],
        cuts: &[usize],
        spans: Option<&ContentRegex>,
        want: Want,
    ) -> FileMatches {
        let mut bounds: Vec<usize> = cuts.iter().map(|c| c % (content.len() + 1)).collect();
        bounds.push(0);
        bounds.push(content.len());
        bounds.sort_unstable();
        let mut chunks: Vec<Vec<u8>> = bounds.windows(2).map(|w| content[w[0]..w[1]].to_vec()).collect();
        let first = if chunks.is_empty() { Vec::new() } else { chunks.remove(0) };
        let rest: Vec<Result<bytes::Bytes, Error>> =
            chunks.into_iter().map(|c| Ok(bytes::Bytes::from(c))).collect();
        let stream: ByteStream = futures::stream::iter(rest).boxed();
        let rt = tokio::runtime::Builder::new_current_thread().build().expect("runtime");
        rt.block_on(scan_bytes(needle, norte_encoding::UTF_8, first, stream, &CancellationToken::new(), spans, want))
            .expect("scan")
    }

    fn scan_cut(needle: &ContentNeedle, content: &[u8], cuts: &[usize]) -> Option<MatchInfo> {
        scan_cut_with(needle, content, cuts, None, ONE).lines.into_iter().next()
    }

    /// `scan_decode_lines` over `content` whole, with a regex.
    fn decode_with(enc: &'static Encoding, content: &[u8], re: &ContentRegex, want: Want) -> FileMatches {
        let rt = tokio::runtime::Builder::new_current_thread().build().expect("runtime");
        let re = re.clone();
        rt.block_on(scan_decode_lines(
            enc,
            content.to_vec(),
            futures::stream::empty::<Result<bytes::Bytes, Error>>().boxed(),
            &CancellationToken::new(),
            want,
            move |l| re.is_match(l).then(|| re.spans(l)),
        ))
        .expect("scan")
    }
```

Then the tests:

```rust
    /// A match past column 160 used to be cut out of its own preview.
    #[test]
    fn a_match_far_into_the_line_is_in_the_preview_centred() {
        let line = format!("{}aguja{}\n", "x".repeat(300), "y".repeat(300));
        let n = ContentNeedle::literal("aguja", true);
        let m = scan_cut_with(&n, line.as_bytes(), &[], Some(&spans_for("aguja", true)), ONE)
            .lines.remove(0);
        let p: Vec<char> = m.preview.expect("preview").chars().collect();
        assert!(p.len() <= 160, "{}", p.len());
        assert_eq!((p[0], p[p.len() - 1]), ('…', '…'));
        let (s, e) = m.ranges.expect("ranges")[0];
        assert_eq!(p[s as usize..e as usize].iter().collect::<String>(), "aguja");
        let centre = (s + e) / 2;
        assert!(centre.abs_diff(p.len() as u32 / 2) <= 1, "centred: {centre} of {}", p.len());
    }

    /// Ranges count CHARS of the preview, not bytes.
    #[test]
    fn ranges_index_chars_not_bytes() {
        let n = ContentNeedle::literal("año", true);
        let m = scan_cut_with(&n, "ñandú año\n".as_bytes(), &[], Some(&spans_for("año", true)), ONE)
            .lines.remove(0);
        assert_eq!(m.ranges, Some(vec![(6, 9)]));
    }

    /// Masking is char-for-char, so a hazard before the match does not move
    /// the range off the needle.
    #[test]
    fn ranges_point_at_the_needle_after_masking() {
        let n = ContentNeedle::literal("año", true);
        let m = scan_cut_with(&n, "\u{202E}\u{1b}x año\n".as_bytes(), &[], Some(&spans_for("año", true)), ONE)
            .lines.remove(0);
        assert_eq!(m.preview.as_deref(), Some("\u{FFFD}\u{FFFD}x año"));
        assert_eq!(m.ranges, Some(vec![(4, 7)]));
    }

    /// The literal path: the offset is the start of the matching LINE.
    #[test]
    fn the_byte_path_gives_the_line_start_offset() {
        let n = ContentNeedle::literal("año", true);
        let m = scan_cut(&n, "uno\ndos año\n".as_bytes(), &[]).expect("hit");
        assert_eq!((m.line, m.offset), (Some(2), Some(4)));
    }

    /// Several lines are kept, several occurrences on one line are ONE line
    /// with several ranges, and what is past the cap is counted.
    #[test]
    fn lines_are_kept_up_to_the_cap_and_the_rest_counted() {
        let n = ContentNeedle::literal("año", true);
        let body = "año\nx\naño y año\naño\naño\n";
        let f = scan_cut_with(&n, body.as_bytes(), &[], Some(&spans_for("año", true)), Want { keep: 2, count_more: true });
        let lines: Vec<_> = f.lines.iter().map(|m| (m.line, m.offset)).collect();
        // `año\n` is 5 bytes (ñ is two), `x\n` two: line 3 starts at 7.
        assert_eq!(lines, vec![(Some(1), Some(0)), (Some(3), Some(7))]);
        assert_eq!(f.lines[1].ranges, Some(vec![(0, 3), (6, 9)]));
        assert_eq!(f.more, 2);
    }

    /// Without a request for more, the scan still stops at the first hit:
    /// 0.85's cost for 0.85's question.
    #[test]
    fn one_line_asked_is_one_line_and_nothing_counted() {
        let n = ContentNeedle::literal("año", true);
        let f = scan_cut_with(&n, "año\naño\n".as_bytes(), &[], None, ONE);
        assert_eq!((f.lines.len(), f.more), (1, 0));
    }

    /// The regex path: every occurrence ranged, offsets counted in bytes.
    #[test]
    fn the_regex_path_ranges_every_occurrence() {
        let re = ContentRegex::new("a.o", true).expect("regex");
        let f = decode_with(norte_encoding::UTF_8, "uno\naño y aro\n".as_bytes(), &re, ALL);
        assert_eq!(f.lines[0].line, Some(2));
        assert_eq!(f.lines[0].offset, Some(4));
        assert_eq!(f.lines[0].ranges, Some(vec![(0, 3), (6, 9)]));
    }

    /// UTF-16 with a BOM: the offset counts the BOM and two bytes per unit.
    #[test]
    fn utf16_offsets_count_the_bom_and_code_units() {
        let mut bytes = vec![0xFF, 0xFE];
        for u in "uno\nhay año\n".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        let enc = Encoding::for_label(b"utf-16le").expect("utf-16le");
        let re = spans_for("año", true);
        let f = decode_with(enc, &bytes, &re, ALL);
        assert_eq!((f.lines[0].line, f.lines[0].offset), (Some(2), Some(2 + 8)));
        assert_eq!(f.lines[0].ranges, Some(vec![(4, 7)]));
    }

    /// Whole word: only the standalone occurrence is ranged.
    #[test]
    fn whole_word_ranges_skip_partial_words() {
        let re = ContentRegex::new(&whole_word_pattern("set"), true).expect("regex");
        let f = decode_with(norte_encoding::UTF_8, b"offset set settings\n", &re, ALL);
        assert_eq!(f.lines[0].ranges, Some(vec![(7, 10)]));
    }

    /// A line that matched as bytes of another encoding but whose text does
    /// not contain the needle: a hit with no range, preview as before.
    #[test]
    fn without_a_range_the_preview_is_the_head_of_the_line() {
        let line = "z".repeat(200);
        let (p, r) = preview_with_ranges(&line, &[]);
        assert_eq!((p.chars().count(), r), (160, vec![]));
        assert!(!p.contains('…'));
    }
```

And the seam property (Review Focus 1):

```rust
    proptest::proptest! {
        /// Every matching line, its offset and its ranges are the same
        /// whatever the chunk cuts — a short needle repeated across a seam
        /// is neither counted twice nor lost.
        #[test]
        fn all_matches_do_not_depend_on_where_chunks_are_cut(
            parts in proptest::collection::vec(
                proptest::prop_oneof![
                    proptest::strategy::Just(b"a".to_vec()),
                    proptest::strategy::Just(b"aa".to_vec()),
                    proptest::strategy::Just(b"\n".to_vec()),
                    proptest::strategy::Just("añ".as_bytes().to_vec()),
                    proptest::strategy::Just("AÑO".as_bytes().to_vec()),
                    proptest::strategy::Just(b"o".to_vec()),
                ],
                0..60,
            ),
            cuts in proptest::collection::vec(0usize..600, 0..16),
        ) {
            let content: Vec<u8> = parts.concat();
            for needle in ["aa", "año"] {
                let n = ContentNeedle::literal(needle, false);
                let re = spans_for(needle, false);
                let key = |f: FileMatches| (f.more, f.lines.into_iter().map(|m| (m.line, m.offset)).collect::<Vec<_>>());
                let whole = key(scan_cut_with(&n, &content, &[], Some(&re), ALL));
                let cut = key(scan_cut_with(&n, &content, &cuts, Some(&re), ALL));
                proptest::prop_assert_eq!(cut, whole);
            }
        }
    }
```

(Ranges are not compared across cuts: a line longer than the retained tail gets a left-trimmed preview depending on the cut, as today.)

Also extend the existing cancellation coverage: a scan with `ALL` over a long stream must return `Err(Error::Cancelled)` when the token is cancelled before the second chunk:

```rust
    #[test]
    fn counting_the_rest_of_a_file_still_honours_cancellation() {
        let n = ContentNeedle::literal("a", true);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let rt = tokio::runtime::Builder::new_current_thread().build().expect("runtime");
        let rest: ByteStream = futures::stream::iter(vec![Ok(bytes::Bytes::from_static(b"a\na\n"))]).boxed();
        let r = rt.block_on(scan_bytes(&n, norte_encoding::UTF_8, b"a\n".to_vec(), rest, &cancel, None, ALL));
        assert!(matches!(r, Err(Error::Cancelled)));
    }
```

- [ ] **Step 5: Run** `just t norte-core` — expected: compile errors (`Want`, `FileMatches`, `spans`, new `scan_*` signatures).

- [ ] **Step 6: Implement.** In `search.rs`:

1. `ContentRegex::spans`:
```rust
    /// The non-empty match spans in `line`, as BYTE ranges (0.86.0). An
    /// empty match (`a*`) still makes the line match through
    /// [`Self::is_match`]; it just paints nothing.
    #[must_use]
    pub fn spans(&self, line: &str) -> Vec<std::ops::Range<usize>> {
        self.re.find_iter(line).map(|m| m.range()).filter(|r| !r.is_empty()).collect()
    }
```
2. `SearchMatchers` gains `spans: Option<ContentRegex>` — what locates the match inside a matching line. In `compile`: for `ContentSpec::Literal(c)` → `ContentRegex::new(&regex::escape(c), cs).ok()`; for `ContentSpec::Regex(re)` → `Some(re.clone())`; `None` otherwise. Plus `fn want(&self) -> Want { Want { keep: 1, count_more: false } }`.
3. The private types and helpers:
```rust
/// What one file's content scan found (0.86.0).
#[derive(Debug, Default)]
struct FileMatches {
    /// The matching LINES kept, in file order.
    lines: Vec<MatchInfo>,
    /// Further matching lines past the cap, counted and not kept.
    more: u32,
}

/// How many matching lines a scan keeps, and whether it reads on to count
/// the rest. `{ keep: 1, count_more: false }` stops at the first hit.
#[derive(Debug, Clone, Copy)]
struct Want {
    keep: usize,
    count_more: bool,
}

impl FileMatches {
    /// One more matching line: kept while there is room (`make` builds it
    /// only then), counted after. `true` = nothing more is wanted.
    fn add(&mut self, want: Want, make: impl FnOnce() -> MatchInfo) -> bool {
        if self.lines.len() < want.keep {
            self.lines.push(make());
        } else {
            self.more = self.more.saturating_add(1);
        }
        self.lines.len() >= want.keep && !want.count_more
    }
}

/// A matching line as the wire carries it.
fn matched(line: u64, offset: Option<u64>, text: &str, spans: &[std::ops::Range<usize>]) -> MatchInfo {
    let (preview, ranges) = preview_with_ranges(text, spans);
    MatchInfo { line: Some(line), offset, preview: Some(preview), ranges: Some(ranges) }
}
```
4. `preview_with_ranges(line: &str, spans: &[Range<usize>]) -> (String, Vec<(u32, u32)>)` replaces `trim_preview` (keep its doc paragraph on mask-first; add: masking is char-for-char, so char indices of `line` index the masked text). Algorithm: convert the byte spans to char spans in ONE pass over `line.char_indices()` (spans are sorted); `masked: Vec<char>`; if `masked.len() <= PREVIEW_MAX_CHARS` return it whole with the char spans; if there is no span return the first `PREVIEW_MAX_CHARS` chars and no ranges (0.85's preview); else `room = PREVIEW_MAX_CHARS - 2`, `len = (e - s).min(room)`, `start = s.saturating_sub((room - len) / 2).min(masked.len() - room)`, `end = start + room`; prefix `…` if `start > 0`, suffix `…` if `end < masked.len()`; ranges clipped to `[start, end)`, shifted by `-start + usize::from(start > 0)`, empty ones dropped; `u32::try_from(..).unwrap_or(u32::MAX)` for the cast (a preview is ≤160 chars, comment the invariant).
5. `line_preview(raw: &[u8], enc, spans: Option<&ContentRegex>) -> (String, Vec<(u32,u32)>)` replaces `extract_line_preview`: `decode(raw, enc, true).text`, strip a trailing `\r`, `spans.map(|re| re.spans(line)).unwrap_or_default()`, `preview_with_ranges`.
6. `scan_bytes` — new signature and body (the fast path keeps #391's no-copy commit):
```rust
/// Where a byte scan is: the absolute offset of the first byte still held
/// (`tail[0]`), how many `\n` lie before it, and where the line holding it
/// began.
#[derive(Debug, Default, Clone, Copy)]
struct Cursor {
    abs: u64,
    nl: u64,
    line_start: u64,
}

impl Cursor {
    /// `seen` leaves the window: count its newlines, remember the last line start.
    fn advance(&mut self, seen: &[u8]) {
        self.nl += count_nl(seen);
        if let Some(i) = memchr::memrchr(b'\n', seen) {
            self.line_start = self.abs + i as u64 + 1;
        }
        self.abs += seen.len() as u64;
    }
}

async fn scan_bytes(
    needle: &ContentNeedle,
    enc: &'static Encoding,
    first: Vec<u8>,
    mut stream: ByteStream,
    cancel: &CancellationToken,
    spans: Option<&ContentRegex>,
    want: Want,
) -> Result<FileMatches, Error> {
    let mut found = FileMatches::default();
    let mut tail: Vec<u8> = Vec::new();
    let mut at = Cursor::default();
    // A hit starting before `resume` is on a line already filed.
    let mut resume: u64 = 0;
    // The last filed line has not ended in what was read yet.
    let mut open_line = false;
    // Absolute end of what was already searched: a hit wholly before it
    // was seen in an earlier pass (a short needle can fit in `tail`).
    let mut searched_to: u64 = 0;
    let mut chunk = Some(bytes::Bytes::from(first));
    let finders: Vec<memchr::memmem::Finder<'_>> = needle
        .needles()
        .iter()
        .map(|(_, n)| memchr::memmem::Finder::new(n))
        .collect();
    let keep = needle.max_len().saturating_sub(1);
    loop {
        let c = match chunk.take() {
            Some(c) => c,
            None => match stream.next().await {
                Some(Ok(c)) => c,
                Some(Err(e)) => return Err(e),
                None => break,
            },
        };
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let base = at.abs;
        let end = base + (tail.len() + c.len()) as u64;
        if open_line || first_hit(&finders, &tail, &c, keep).is_some() {
            let mut buf = std::mem::take(&mut tail);
            buf.extend_from_slice(&c);
            if open_line {
                // The filed line ends at the first `\n` in the NEW bytes.
                let from = usize::try_from(searched_to - base).map_or(buf.len(), |f| f.min(buf.len()));
                match memchr::memchr(b'\n', &buf[from..]) {
                    Some(i) => {
                        resume = base + (from + i + 1) as u64;
                        open_line = false;
                    }
                    None => resume = end,
                }
            }
            let mut hits: Vec<(usize, usize)> = finders
                .iter()
                .flat_map(|f| {
                    let n = f.needle().len();
                    f.find_iter(&buf).map(move |p| (p, n))
                })
                .collect();
            hits.sort_unstable();
            for (p, n) in hits {
                let abs = base + p as u64;
                if abs < resume || abs + n as u64 <= searched_to {
                    continue;
                }
                let le = memchr::memchr(b'\n', &buf[p..]).map(|i| p + i);
                let done = found.add(want, || {
                    let ls = memchr::memrchr(b'\n', &buf[..p]).map(|i| i + 1);
                    let line = at.nl + count_nl(&buf[..p]) + 1;
                    let offset = ls.map_or(at.line_start, |i| base + i as u64);
                    let (preview, ranges) =
                        line_preview(&buf[ls.unwrap_or(0)..le.unwrap_or(buf.len())], enc, spans);
                    MatchInfo { line: Some(line), offset: Some(offset), preview: Some(preview), ranges: Some(ranges) }
                });
                if done {
                    return Ok(found);
                }
                match le {
                    Some(i) => resume = base + i as u64 + 1,
                    None => {
                        resume = end;
                        open_line = true;
                    }
                }
            }
            let split = buf.len() - keep.min(buf.len());
            at.advance(&buf[..split]);
            tail = buf[split..].to_vec();
        } else {
            let total = tail.len() + c.len();
            let split = total - keep.min(total);
            if split >= tail.len() {
                at.advance(&tail);
                at.advance(&c[..split - tail.len()]);
                tail = c[split - tail.len()..].to_vec();
            } else {
                at.advance(&tail[..split]);
                tail.drain(..split);
                tail.extend_from_slice(&c);
            }
        }
        searched_to = end;
    }
    Ok(found)
}
```
Update `first_hit`'s doc: `tail` may now hold a match already filed; a hit inside it only sends the chunk down the slow path, where `searched_to` discards it.
7. `scan_decode_lines(enc, first, stream, cancel, want: Want, mut find: impl FnMut(&str) -> Option<Vec<Range<usize>>>) -> Result<FileMatches, Error>`: same loop, plus a byte offset of the line being read:
```rust
/// Bytes `text` took in `enc`: exact for UTF-8, UTF-16 and single-byte
/// encodings, `None` for the rest (C9 of the plan, `MatchInfo::offset`).
fn encoded_len(text: &str, enc: &'static Encoding) -> Option<u64> {
    if enc == norte_encoding::UTF_8 {
        Some(text.len() as u64)
    } else if is_utf16(enc) {
        Some(2 * text.encode_utf16().count() as u64)
    } else if enc.is_single_byte() {
        Some(text.chars().count() as u64)
    } else {
        None
    }
}
```
Start with `let bom = Encoding::for_bom(&first); let enc_for_offsets = bom.map_or(enc, |(e, _)| e); let mut offset = Some(bom.map_or(0, |(_, n)| n as u64));` (the decoder consumes the BOM and follows it). For every complete line: `let here = offset; offset = offset.and_then(|o| encoded_len(line, enc_for_offsets).map(|n| o + n));` BEFORE the `skipping` check; a matching line files `found.add(want, || matched(line_no, here, l, &spans))` and returns `Ok(found)` when `add` says done. The over-cap truncation branch files with `offset` (the truncated line's start) and then advances `offset` by `pending` before clearing it; the `skipping` clear advances by `pending` too. The final line without `\n` files the same way; the end returns `Ok(found)`.
8. `search_content` passes `matchers.want()` and `matchers.spans.as_ref()`; the UTF-16 literal closure is `move |line| line_contains(line, &needle, cs).then(|| spans.as_ref().map(|re| re.spans(line)).unwrap_or_default())` (with `let spans = matchers.spans.clone();`), the regex one `move |line| re.is_match(line).then(|| re.spans(line))`; the result maps `FileMatches` with no lines to `None`.
9. `Batch::push(entry, found: Option<FileMatches>)`: `self.matches.push(found.and_then(|f| f.lines.into_iter().next()).unwrap_or_default())`; `settle!`'s tuple type becomes `(Entry, Result<Option<FileMatches>, Error>)`.
10. Update the `PREVIEW_MAX_CHARS` and `LINE_MATCH_CAP` docs and `find_in`'s "stops at the first hit" sentence to say "stops at the first hit unless more are asked (0.86.0)".

- [ ] **Step 7: Run** `just t norte-core`, `just t norte-proto`, `cargo test -p norte-proto --doc`, `cargo test -p norte-core --doc`, `cargo check --workspace --all-targets` — all PASS. `just c`.
- [ ] **Step 8: Dispatch reviewers before committing:** `protocol-guardian` (the `# 0.86.0` text, C2, additive-only) and `encoding-auditor` (char/byte ranges, masking, UTF-16 offsets, the BOM). Questions to ask: can `searched_to`/`resume` drop a match that starts in `tail` and ends in `c`? is `encoded_len` exact for every single-byte WHATWG encoding?
- [ ] **Step 9: Commit** (`feat(core,proto): a search says where in the line it matched (0.86.0)`).

---

### Task 3: A search can ask for every matching line of each file (`max_matches_per_file`)

**Files:**
- Modify: `crates/norte-proto/src/methods.rs` (`FsSearchParams` + `new`, `SearchHits`, new `SEARCH_MATCHES_PER_FILE_MAX`, the `# 0.86.0` section, `FS_SEARCH` doc ~1705)
- Modify: `crates/norte-core/src/search.rs` (`SearchMatchers.per_file`, `want()`, `Batch`)
- Modify: goldens (`fs_search_params` full case, `search_hits`), `methods.json`, schema; `crates/norte-ui-host/tests/backend_fake/mod.rs:1068` (`SearchHits` literal) and any other `SearchHits {` literal (`rg -n "SearchHits \{" crates`).

**Interfaces:**
- Consumes: Task 2's `Want`, `FileMatches`.
- Produces: `FsSearchParams.max_matches_per_file: Option<u32>`; `SearchHits.all_matches: Option<Vec<Vec<MatchInfo>>>`; `SearchHits.more: Option<Vec<u32>>`; `norte_proto::methods::SEARCH_MATCHES_PER_FILE_MAX: u32 = 1000`.

- [ ] **Step 1: Failing tests** in `search.rs` (walk level, over `MemProvider`):

```rust
    /// A tree of named files with bodies, walked with `p`; every batch.
    async fn batches(files: &[(&str, &[u8])], p: FsSearchParams) -> Vec<SearchHits> {
        let mem = Arc::new(MemProvider::new());
        mem.mkdir(&vp("mem:///t")).await.expect("mkdir");
        for (name, body) in files {
            let mut sink = mem.write(&vp(&format!("mem:///t/{name}"))).await.expect("write");
            sink.write(bytes::Bytes::copy_from_slice(body)).await.expect("chunk");
            sink.commit().await.expect("commit");
        }
        let matchers = SearchMatchers::compile(&p).expect("criteria");
        let (tx, mut rx) = mpsc::channel::<SearchHits>(64);
        let ctx = test_ctx();
        let provider: Arc<dyn Provider> = mem;
        let h = tokio::spawn(async move {
            let mut out = Vec::new();
            while let Some(b) = rx.recv().await {
                out.push(b);
            }
            out
        });
        run_walk(provider, vp("mem:///t"), matchers, Vec::new(), tx, &ctx).await.expect("walk");
        drop(ctx);
        h.await.expect("collector")
    }

    fn content_in_t(needle: &str, per_file: Option<u32>) -> FsSearchParams {
        FsSearchParams {
            content: Some(needle.to_owned()),
            max_matches_per_file: per_file,
            ..FsSearchParams::new(vp("mem:///t"))
        }
    }

    /// Asked for two lines per file: two kept, the third counted, and the
    /// 0.85 `matches` still carries the FIRST so an old reader is unchanged.
    #[tokio::test]
    async fn per_file_cap_keeps_and_counts() {
        let b = batches(&[("a.txt", b"x\naguja 1\naguja 2\naguja 3\n")], content_in_t("aguja", Some(2))).await;
        let b = &b[0];
        let all = b.all_matches.as_ref().expect("asked for");
        assert_eq!(all[0].iter().map(|m| m.line).collect::<Vec<_>>(), vec![Some(2), Some(3)]);
        assert_eq!(b.more.as_deref(), Some(&[1][..]));
        assert_eq!(b.matches.as_ref().expect("content")[0].line, Some(2));
    }

    /// Not asked: the batch is 0.85's shape — no `all_matches`, no `more`.
    #[tokio::test]
    async fn without_the_field_the_batch_keeps_its_old_shape() {
        let b = batches(&[("a.txt", b"aguja\naguja\n")], content_in_t("aguja", None)).await;
        assert_eq!((b[0].all_matches.is_none(), b[0].more.is_none()), (true, true));
    }

    /// The daemon clamps what it is asked: 0 means one, and nobody gets
    /// more than the protocol's ceiling.
    #[test]
    fn the_per_file_request_is_clamped() {
        let at = |n| SearchMatchers::compile(&content_in_t("x", Some(n))).expect("criteria").want().keep;
        assert_eq!(at(0), 1);
        assert_eq!(at(5000), norte_proto::methods::SEARCH_MATCHES_PER_FILE_MAX as usize);
    }
```

Golden: the full `fs_search_params` case gains `max_matches_per_file: Some(51)` and `methods.json` `"max_matches_per_file": 51`; `search_hits` gains `all_matches: Some(vec![vec![<the same MatchInfo>]]), more: Some(vec![0])` and the fixture the matching keys. `types.rs`: a 0.85 `SearchHits` JSON (no new keys) deserialises with both `None`.

- [ ] **Step 2: Run** `just t norte-core` — FAIL (no field).
- [ ] **Step 3: Implement.** Proto:

```rust
/// The most matching lines of ONE file a search reports, whatever
/// [`FsSearchParams::max_matches_per_file`] asks (0.86.0). A protocol
/// bound, like [`SEARCH_EXCLUDES_MAX`]: a reader past a few hundred lines
/// of one file wants the viewer, not the list.
pub const SEARCH_MATCHES_PER_FILE_MAX: u32 = 1000;
```

`FsSearchParams` (after `encoding`, and `max_matches_per_file: None` in `new`):

```rust
    /// How many matching LINES of each file to report (0.86.0). Absent = the
    /// first one only, and the daemon stops reading the file there — 0.85's
    /// search. With it the daemon reads every content file to the end: it
    /// reports up to this many lines in [`SearchHits::all_matches`] and
    /// COUNTS the rest in [`SearchHits::more`]. Clamped to
    /// `1..=`[`SEARCH_MATCHES_PER_FILE_MAX`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_matches_per_file: Option<u32>,
```

`SearchHits` (after `matches`):

```rust
    /// Every matching line kept per file, aligned with `entries` (0.86.0).
    /// Only when the search asked for [`FsSearchParams::max_matches_per_file`];
    /// `matches` keeps carrying the first line so a reader that ignores
    /// this field sees 0.85's batch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub all_matches: Option<Vec<Vec<MatchInfo>>>,
    /// Matching lines past the cap per file, counted and not sent; aligned
    /// with `entries`, `0` = none (0.86.0). Present with `all_matches`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub more: Option<Vec<u32>>,
```

Append to the `# 0.86.0` rustdoc: the request field, the two batch fields, that a 0.85 daemon would ignore the request (and that it cannot be reached, C2), and the cost (asking reads every content file whole). Add to the `FS_SEARCH` doc one line: "`max_matches_per_file` (0.86.0) turns stop-at-first into read-to-the-end".

Core: `SearchMatchers.per_file: Option<usize>` = `params.max_matches_per_file.map(|n| n.clamp(1, SEARCH_MATCHES_PER_FILE_MAX) as usize)`; `want()` = `per_file.map_or(Want { keep: 1, count_more: false }, |keep| Want { keep, count_more: true })`. `Batch` gains `per_file: bool`, `all: Vec<Vec<MatchInfo>>`, `more: Vec<u32>`; `push` fills `all`/`more` only when `per_file` (with `content`), `take` returns them under `per_file.then(..)`. `Batch::new(task_id, content, per_file)`.

- [ ] **Step 4: Run** `just t norte-core`, `just t norte-proto` (with fixtures updated), `NORTE_UPDATE_SCHEMA=1 just t norte-proto`, `cargo test --workspace --exclude norte-gui-tauri --doc`, `cargo check --workspace --all-targets` — PASS. `just c`.
- [ ] **Step 5: Reviewer:** `protocol-guardian` on the combined Tasks 2–3 wire diff.
- [ ] **Step 6: Commit** (`feat(core,proto): a search can report every matching line of a file`).

---

### Task 4: The panel's model in the shared crate

**Files:**
- Create: `crates/norte-frontend/src/search_panel.rs` (+ `pub mod search_panel;` in `crates/norte-frontend/src/lib.rs`, alphabetical, next to `pub mod search;`)
- Modify: `crates/norte-frontend/src/search.rs` (`SearchForm::regex_unreadable`, contradiction C3)
- Modify: `crates/norte-frontend/src/view/viewer.rs` (only the `ViewerTarget` struct; its behaviour is Task 6)
- Modify: `crates/norte-i18n/i18n/en.ftl`, `es.ftl` (the keys listed in Step 4)

**Interfaces:**
- Consumes: Task 3's `FsSearchParams.max_matches_per_file`, `SearchHits.all_matches/more`, `MatchInfo.offset/ranges`; `norte_frontend::search::{SearchForm, SearchField, params, ID_*}`; `search_status::{Outcome, status_key}`.
- Produces (every later task uses these names):

```rust
// crates/norte-frontend/src/view/viewer.rs
/// Where to open a file and what to light up (spec 2026-10-09 §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewerTarget {
    /// 1-based line.
    pub line: u64,
    /// Byte offset of the line's start, when the daemon knew it.
    pub offset: Option<u64>,
    /// The preview the ranges index (C7: how the viewer finds them in the line).
    pub preview: String,
    /// `[start, end)` in chars of `preview`.
    pub ranges: Vec<(u32, u32)>,
}

// crates/norte-frontend/src/search_panel.rs
pub const DEBOUNCE: std::time::Duration;      // 300 ms
pub const SHOWN_PER_FILE: usize;              // 50
pub const ASKED_PER_FILE: u32;                // 51
pub const MAX_FILES: u32;                     // 2000
pub struct MatchLine { pub line: u64, pub offset: Option<u64>, pub preview: String, pub ranges: Vec<(u32, u32)> }
pub struct FileNode { pub entry: Entry, pub matches: Vec<MatchLine>, pub more: u32 /* + private `next: Option<MatchLine>` */ }
pub struct FolderNode { pub dir: VPath, pub files: Vec<FileNode> }
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Row { Folder { folder: usize }, File { folder: usize, file: usize }, Line { folder: usize, file: usize, line: usize }, More { folder: usize, file: usize } }
pub enum Activation { Toggled, GoTo { dir: VPath, entry: VPath }, View { path: VPath, target: ViewerTarget } }
pub enum Due { Launch { epoch: u64, params: Box<FsSearchParams> }, Clear, Unreadable(SearchField) }
pub enum PanelStatus { Idle, Unreadable(SearchField), Run { key: &'static str, files: usize, folders: usize, error: Option<String> } }
impl SearchPanel {
    pub fn new(root: VPath) -> Self;
    pub fn form(&self) -> &SearchForm;
    pub fn root(&self) -> &VPath;
    pub fn epoch(&self) -> u64;
    pub fn edit(&mut self, field: SearchField, text: String, now: Instant);
    pub fn push_char(&mut self, field: SearchField, c: char, now: Instant);   // the TUI's keystrokes
    pub fn backspace(&mut self, field: SearchField, now: Instant);
    pub fn toggle(&mut self, id: &str, now: Instant) -> bool;                 // ID_REGEX/ID_CASE/ID_WHOLE_WORD/ID_RECURSIVE/ID_KINDS
    pub fn set_root(&mut self, root: VPath, now: Instant);
    pub fn force(&mut self, now: Instant);                                    // Enter
    pub fn due(&mut self, now: Instant, now_ms: i64) -> Option<Due>;
    pub fn apply_hits(&mut self, epoch: u64, hits: &SearchHits) -> bool;
    pub fn finished(&mut self, epoch: u64, outcome: Outcome);
    pub fn is_running(&self) -> bool;
    pub fn is_dimmed(&self) -> bool;
    pub fn filters_open(&self) -> bool;
    pub fn toggle_filters(&mut self);
    pub fn filters_set(&self) -> usize;                                       // the `n` of "filters (n)"
    pub fn tree(&self) -> &[FolderNode];
    pub fn rows(&self) -> Vec<Row>;
    pub fn cursor(&self) -> usize;
    pub fn set_cursor(&mut self, i: usize);
    pub fn up(&mut self, n: usize);
    pub fn down(&mut self, n: usize);
    pub fn home(&mut self);
    pub fn end(&mut self);
    pub fn fold(&mut self, open: bool);                                       // ←/→
    pub fn activate(&mut self) -> Option<Activation>;
    pub fn status(&self) -> PanelStatus;
}
```

- [ ] **Step 1: `regex_unreadable` — test, then code** in `search.rs`:

```rust
    /// A regex that does not compile is named before launching (C3 of the
    /// search-panel plan): the panel searches as you type, and a half-typed
    /// `(` must flag the field, not send a request the daemon refuses.
    #[test]
    fn an_unfinished_regex_is_flagged_only_in_regex_mode() {
        let mut f = SearchForm::new();
        f.content = "(año".to_owned();
        assert_eq!(f.regex_unreadable(), None, "a literal parenthesis is fine");
        f.regex = true;
        assert_eq!(f.regex_unreadable(), Some(SearchField::Content));
        f.content = "a.o".to_owned();
        f.name = "[".to_owned();
        assert_eq!(f.regex_unreadable(), Some(SearchField::Name));
    }
```

```rust
    /// The pattern that does not compile as a regex, if the regex switch is
    /// on. Separate from [`Self::field_unreadable`] so the dialog keeps its
    /// behaviour; the daemon still has the last word (it adds a size limit).
    #[must_use]
    pub fn regex_unreadable(&self) -> Option<SearchField> {
        if !self.regex {
            return None;
        }
        [(SearchField::Name, &self.name), (SearchField::Content, &self.content)]
            .into_iter()
            .find(|(_, p)| !p.is_empty() && regex::Regex::new(p).is_err())
            .map(|(f, _)| f)
    }
```

- [ ] **Step 2: Model tests** at the bottom of `search_panel.rs` (RED is a compile error until Step 4):

```rust
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
        Entry { path: vp(p), kind: EntryKind::File, size: None, mtime_ms: None, attrs: Default::default() }
    }
    fn mi(line: u64) -> MatchInfo {
        MatchInfo { line: Some(line), offset: Some(line * 10), preview: Some(format!("l{line} aguja")), ranges: Some(vec![(3, 8)]) }
    }
    /// A 0.86 batch: per file, its lines and its `more`.
    fn batch(files: &[(&str, Vec<MatchInfo>, u32)]) -> SearchHits {
        SearchHits {
            task_id: TaskId::new(1),
            entries: files.iter().map(|(p, _, _)| file(p)).collect(),
            matches: Some(files.iter().map(|(_, l, _)| l.first().cloned().unwrap_or_default()).collect()),
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
        let Some(Due::Launch { params, .. }) = p.due(t0 + DEBOUNCE, 0) else { panic!("launch") };
        assert_eq!((params.max_matches_per_file, params.max_hits), (Some(51), Some(2000)));
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
        assert!(!p.apply_hits(e, &batch(&[("mem:///r/b.txt", vec![mi(1)], 0)])), "the old run is gone");
    }

    #[test]
    fn a_bad_regex_does_not_launch_and_dims_what_is_there() {
        let (mut p, e, t0) = searching("ag");
        p.apply_hits(e, &batch(&[("mem:///r/a.txt", vec![mi(1)], 0)]));
        p.toggle(crate::search::ID_REGEX, t0);
        typed(&mut p, "(ag", t0);
        assert!(matches!(p.due(t0 + DEBOUNCE, 0), Some(Due::Unreadable(SearchField::Content))));
        assert!(p.is_dimmed());
        assert_eq!(p.tree().len(), 1, "the results stay");
        assert!(p.apply_hits(e, &batch(&[("mem:///r/b.txt", vec![mi(1)], 0)])), "the running query still lands");
        assert_eq!(p.tree().len(), 2, "and does not wipe what was there");
        assert!(matches!(p.status(), PanelStatus::Unreadable(SearchField::Content)));
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
        p.apply_hits(e, &batch(&[("mem:///r/z/b.txt", vec![mi(1)], 0), ("mem:///r/a/x.txt", vec![mi(1)], 0)]));
        p.apply_hits(e, &batch(&[("mem:///r/z/a.txt", vec![mi(1)], 0)]));
        let dirs: Vec<_> = p.tree().iter().map(|f| f.dir.clone()).collect();
        assert_eq!(dirs, vec![vp("mem:///r/z"), vp("mem:///r/a")]);
        let names: Vec<_> = p.tree()[0].files.iter().map(|f| f.entry.path.clone()).collect();
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
        let Some(Activation::View { target, .. }) = p.activate() else { panic!("view") };
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
        let Some(Activation::View { path, target }) = p.activate() else { panic!("view") };
        assert_eq!(path, vp("mem:///r/a/x.txt"));
        assert_eq!(target, ViewerTarget { line: 7, offset: Some(70), preview: "l7 aguja".to_owned(), ranges: vec![(3, 8)] });
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
        assert!(matches!(SearchPanel::new(vp("mem:///r")).status(), PanelStatus::Idle));
        let (mut p, e, _) = searching("ag");
        p.apply_hits(e, &batch(&[("mem:///r/a/x.txt", vec![mi(1)], 0), ("mem:///r/b/y.txt", vec![mi(1)], 0)]));
        assert!(matches!(p.status(), PanelStatus::Run { key: "search-panel-running", files: 2, folders: 2, .. }));
        p.finished(e, Outcome::Cancelled);
        assert!(matches!(p.status(), PanelStatus::Run { key: "search-panel-cancelled", .. }));
    }
}
```

- [ ] **Step 3: Run** `just t norte-frontend` — FAIL (compile).
- [ ] **Step 4: Implement** `search_panel.rs`. Module doc: one paragraph — the panel's model, pure, the clock is told; points at the spec. The parts with a trap:
  - Two flags, not one: `dimmed` (painting: what is shown belongs to an older query) and `replace_on_arrival` (the next batch, or the end, of the new run clears the tree first). A launch sets both; `Due::Unreadable` sets only `dimmed` — otherwise the running query's next batch would wipe the tree.
  - `due`: `None` until `now >= deadline`; it clears the deadline before deciding. Empty `name.trim()` AND empty `content` → bump the epoch, drop `run`, clear tree/cursor/flags → `Due::Clear`. Then `field_unreadable().or_else(|| regex_unreadable())` → remember it, `dimmed = true` → `Due::Unreadable`. Else clear the unreadable mark, bump the epoch, `run = Some(Run { epoch, outcome: Outcome::Running, files: 0 })`, params = `crate::search::params(&self.form, self.root.clone(), now_ms, MAX_FILES)` with `max_matches_per_file = Some(ASKED_PER_FILE)`.
  - `apply_hits`: `false` unless `run.epoch == epoch`. Per entry, lines from `all_matches[i]` when present, else `matches[i]` (one line), else none; drop `MatchInfo`s without `line` or `preview`; `ranges.unwrap_or_default()`. Lines past `SHOWN_PER_FILE`: the first is kept as `next` (the `More` row's target), `more = wire_more + lines_past_shown`. Folder = `entry.path.parent()` (else the root); a private `BTreeMap<VPath, usize>` indexes folders; files inserted at `binary_search_by` on `file_name().as_bytes()`. Stop adding at `MAX_FILES` (the daemon's `max_hits` already caps; this is the belt).
  - `rows()` honours `collapsed: BTreeSet<VPath>`; `More` only when `more > 0` AND `next` is some. The cursor is clamped to `rows().len() - 1` after every mutation.
  - `activate` on `More` → `View` with `next`; on a `File` (directories found by name included) → `GoTo { dir: folder, entry }`; on a `Folder` → toggle `collapsed`, `Toggled`.
  - `status`: `Idle` without a query; `Unreadable(f)`; otherwise `search_status::status_key(&run.outcome, files >= MAX_FILES as usize)` mapped `"search-status-running" => "search-panel-running"`, `done`, `truncated`, `cancelled`, `failed` (error = the `Outcome::Failed` text). `filters_set` counts the non-empty Exclude/MinSize/MaxSize/Days/Encoding fields plus `kinds != All` and `!recursive`.
  - Fluent keys, each written once in `en.ftl` (first text) and once in `es.ftl` (second), next to `search-status-*`:
    - `search-panel-title` — `Search` / `Buscar`
    - `search-panel-idle` — `Type a name or some text to search` / `Escribe un nombre o un texto para buscar`
    - `search-panel-unreadable` — `{ $field } cannot be read` / `{ $field } no se puede leer`
    - `search-panel-running` — `{ $n } files in { $folders } folders · searching…` / `{ $n } ficheros en { $folders } carpetas · buscando…`
    - `search-panel-done` — `{ $n } files in { $folders } folders` / `{ $n } ficheros en { $folders } carpetas`
    - `search-panel-truncated` — `stopped at { $n } results` / `detenida en { $n } resultados`
    - `search-panel-cancelled` — `{ $n } files · stopped` / `{ $n } ficheros · detenida`
    - `search-panel-failed` — `search failed: { $error }` / `búsqueda fallida: { $error }`
    - `search-panel-more` — `+{ $n } more` / `+{ $n } más`
    - `search-panel-filters` — `filters ({ $n })` / `filtros ({ $n })`
    - `search-panel-root` — `in { $root }` / `en { $root }`
    - `search-panel-reroot` — `Search here` / `Buscar aquí`
    - `viewer-lines-from` — `lines from { $n }` / `líneas desde { $n }`
    - `viewer-target-past-cap` — `the line is past what the viewer reads` / `la línea está más allá de lo que lee el visor`
- [ ] **Step 5: Run** `just t norte-frontend`, `just t norte-i18n`, `cargo test -p norte-frontend --doc` — PASS; `just c`. (`every_literal_key_in_the_code_exists_in_the_catalogue` checks the keys used in code.)
- [ ] **Step 6: Commit** (`feat(frontend): the search panel's model`).
- [ ] **Step 7: Gate** — `just ci-fast` once (Tasks 1–4).

---

### Task 5: The `search` kind, its rail button and `layout.search` in both frontends

Opening, closing and focusing only: the TUI paints the frame with the status line; the window shows the generic box until Task 10. Everything the registry, the catalogue and the help gate force together lands together.

**Files:**
- Create: `crates/norte-frontend/assets/panel-icons/search.svg` (24×24, `fill="none" stroke="currentColor"`, stroke-width as `timeline.svg`; a magnifier: `<circle cx="10.5" cy="10.5" r="6"/>` and `<path d="M15 15l5 5"/>`)
- Modify: `crates/norte-frontend/src/layout/kinds.rs` (after `terminal`: `decl("search", (24, 6), true, true, false, SIN_ROLES)` — comment: focusable, takes keys (it has inputs), one, holds no role; 24×6 = an input, the status and three rows)
- Modify: `crates/norte-frontend/src/layout/docks.rs` (`"search" => (Edge::Left, Size::Fixed(36)),` — what you navigate WITH, VS Code's place for it; extend the docks test)
- Modify: `crates/norte-frontend/src/chrome/panelbar.rs` (`icon`: `"search" => ("⌕", "\u{f002}")`; `icon_svg`: `"search" => include_str!("../../assets/panel-icons/search.svg")`; tests: `"search"` in `every_builtin_button_has_a_one_cell_icon`'s list and last in `the_panels_are_there_and_the_structural_ones_are_not`'s order; `assert_eq!(toggled_kind(&reg, "layout.search"), Some("search"))` next to the timeline's ~822)
- Modify: `crates/norte-frontend/src/keymap/catalogue.rs` (`live("layout.search", false, Inert)` after `layout.timeline`; comment: a keyboard panel, `alt+F` in all seven because Ctrl+Shift+F cannot reach a terminal (C1)). While there, fix the stale `layout.timeline` comment that says no preset binds it (all seven bind `alt+T`), and the same stale claim in `chrome/menu.rs`'s View-menu comment.
- Modify: `crates/norte-frontend/presets/keymap/{orthodox,vim,cua,krusader,far,norton,total-commander}.toml` (`[global]`, right after the `alt+T` timeline line: `{ on = ["alt+F"], run = "layout.search" },`; orthodox's comment above it says why shifted; in krusader's header divergence block one sentence: Krusader's own search stays on `ctrl+s` (`pane.search`), the panel takes `alt+F`)
- Modify: `crates/norte-frontend/src/keymap/presets.rs` (`keyboard_panels_open_and_are_browsed_in_all_seven_presets`: add `layout.search`)
- Modify: `crates/norte-frontend/src/chrome/menu.rs` (Find menu, first section: `["pane.quick-search", "pane.search", "layout.search", "pane.semantic-search"]`; NOT also in View — `no_command_is_in_two_menus`)
- Modify: `crates/norte-i18n/i18n/{en,es}.ftl`: `panelbar-search` = `Search` / `Buscar`; `menu-item-layout-search` = `Search panel` / `Panel de búsqueda`; `help-cmd-layout-search` = `show or hide the search panel` / `muestra u oculta el panel de búsqueda`
- Modify: `crates/norte-help/topics/{en,es}/finding.md` (front matter `commands` gains `"layout.search"`; one sentence `{{cmd:layout.search}} opens the search panel…`, Task 12 writes the section), `crates/norte-help/tests/corpus.rs` (`DOCUMENTED` gains `"layout.search"`, array length +1)
- Modify (host): `crates/norte-ui-host/src/commands.rs` (`IMPLEMENTED` gains `"layout.search"`; `effect_of`: `"layout.search" => Effect::ToggleSlot { kind: "search" },`)
- Modify (TUI): `crates/norte-tui/src/searchpanel.rs` (new: `pub const KIND: &str = "search";`), `lib.rs` (`pub mod searchpanel;`), `app.rs` (`KeyOwner::Search`, doc like `Timeline`'s: no payload, `multi: false`), `app/layout.rs` (`settle_key_owner` arm; the layout-restore insert ~66; the session `sync.open` list ~754 `"search" => self.open_search_panel()`; `search_panel_slot`, `toggle_search_panel`, `open_search_panel`, `close_search_panel`; `resize_target` ~1629), `app/focus.rs` (`crate::searchpanel::KIND => Some(FocusStop::Side(KeyOwner::Search))`), `panel.rs` (`TuiPanel::Search(Box<norte_frontend::search_panel::SearchPanel>)`; the four `None` lists of `as_browser`/`as_browser_mut`/`as_places`/`as_places_mut`; `search_panel`/`search_panel_mut`/`insert_search_panel`), `keymap.rs` (`"layout.search" => LayoutSearch,`; `PANEL_SHARED` gains it), `dispatch.rs` (`Command::LayoutSearch => app.toggle_search_panel(),`), `app/dialogs.rs` (`ALLOW_LOG`, `ALLOW_DISK_MAP`, `ALLOW_PANEL` gain it) and the matching arms in `app/layout.rs` `log_command`/`disk_map_command`/`panel_command` and `screens/side_nav.rs` (~118-125, ~427) (`"layout.search" => app.toggle_search_panel()`), `ui/chrome.rs` `kind_with_keyboard` (`KeyOwner::Search => Some(crate::searchpanel::KIND)`), `ui.rs` (draw dispatch → `draw_search_panel`, here a bordered block titled `search-panel-title` with the status line), `crates/norte-tui/tests/mouse.rs` `every_bar_button_falls_into_its_place` (the button after `layout.terminal` now presses `layout.search`; the past-the-end column moves one button right)
- Regenerate: `crates/norte-cli/tests/goldens/help-en.json`

**Interfaces:**
- Consumes: Task 4's `SearchPanel::new(root)`.
- Produces (TUI): `App::search_panel_slot(&self) -> Option<SlotId>`, `App::toggle_search_panel(&mut self)`, `App::open_search_panel(&mut self)` (root = `self.focused().dir().clone()`), `App::close_search_panel(&mut self)`, `PaneSlots::{search_panel, search_panel_mut, insert_search_panel}`, `KeyOwner::Search`. Task 7 adds the run and cancels it in `close_search_panel`.
- Produces (host): the `search` kind toggles through the generic `toggle_slot`; until Task 10 its slot projects as `SlotView::Unsupported`.

- [ ] **Step 1: Failing tests.** Shared: the panelbar tests above. TUI, new `crates/norte-tui/tests/searchpanel.rs` (helpers copied from `tests/places.rs`):

```rust
/// The command opens the panel at the focused listing's folder with the
/// keyboard in it; again closes it and gives the keys back.
#[test]
fn layout_search_opens_rooted_at_the_focused_folder_and_closes() {
    let mut app = test_app();
    let dir = app.focused().dir().clone();
    app.toggle_search_panel();
    let slot = app.search_panel_slot().expect("open");
    assert_eq!(app.panes.search_panel(slot).expect("model").root(), &dir);
    assert_eq!(app.key_owner(), norte_tui::app::KeyOwner::Search);
    app.toggle_search_panel();
    assert!(app.search_panel_slot().is_none());
    assert_eq!(app.key_owner(), norte_tui::app::KeyOwner::Panes);
}

/// Docked on the left by default (ADR 0173).
#[test]
fn snapshot_search_panel_opens_on_the_left() {
    let mut app = test_app();
    app.render_now_ms = Some(0);
    app.toggle_search_panel();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    terminal.draw(|f| norte_tui::ui::draw(f, &app)).expect("draw");
    insta::assert_snapshot!(terminal.backend().to_string());
}
```

Host: copy the nearest `layout.timeline` toggle test (`rg -n "layout.timeline" crates/norte-ui-host/tests`) for `layout.search` placing a slot of kind `search`; `tests/parity_matrix.rs` goes green only with `IMPLEMENTED`.
- [ ] **Step 2: Run** `just t norte-frontend`, `just t norte-tui`, `just t norte-ui-host` — FAIL.
- [ ] **Step 3: Implement** the list. `open_search_panel` mirrors `open_timeline` (`mint_slot`, `insert_search_panel(id, SearchPanel::new(root))`, `dock_for(KIND)`, `dock_grouped`, `refresh_visible`, `key_owner = KeyOwner::Search`); `toggle_search_panel` mirrors `toggle_timeline`'s three states; `close_search_panel` mirrors `close_timeline`; the layout-restore arm inserts `SearchPanel::new(focused dir)`.
- [ ] **Step 4: Run** `just t` on `norte-frontend`, `norte-tui`, `norte-ui-host`, `norte-help`, `norte-i18n`, `norte-cli`; `NORTE_UPDATE_GOLDEN=1 just t norte-cli` and review (`layout.search`, chord `Alt+Shift+F`); `cargo insta review` for every TUI snapshot whose rail gained a button, then `cargo insta accept` (never edit `.snap` by hand); `just c`.
- [ ] **Step 5: Commit** (`feat(layout): a search panel kind, its rail button and layout.search`).

---

### Task 6: The shared viewer opens at a line, highlights it, and reads a window past its cap

**Files:**
- Modify: `crates/norte-frontend/src/view/viewer.rs` (`Viewer` gains `first_line: u64` (1) and `highlight: Option<(usize, Vec<(usize, usize)>)>`, initialised in `base`; the functions below)

**Interfaces:**
- Consumes: Task 4's `ViewerTarget`.
- Produces:

```rust
/// Bytes of context read before a target line past the cap.
pub const CONTEXT_BEFORE: u64 = 4096;
/// The read that shows `t` in a viewer that reads `cap` bytes: from the top
/// when the line starts inside the cap (or nobody knows where it starts),
/// else a window from `CONTEXT_BEFORE` bytes before it. `len = Some(cap + 1)`.
pub fn read_for(t: &ViewerTarget, cap: u64) -> norte_proto::ByteRange;
impl Viewer {
    /// A viewer over a window read at `start` (from `read_for`): drops the
    /// partial first line, numbers the rest from `t`'s line, aims at `t`.
    pub fn window(path: VPath, bytes: Vec<u8>, truncated: bool, start: u64, t: &ViewerTarget) -> Self;
    /// Scroll to `t`'s line and light up its ranges (text mode only; hex and
    /// plugin previews keep their scroll and light nothing).
    pub fn aim(&mut self, t: &ViewerTarget);
    /// The first line's number: 1, or N for a window past the cap.
    pub fn first_line(&self) -> u64;
    /// Which line (0-based in this viewer) is lit, if any.
    pub fn highlight_line(&self) -> Option<usize>;
    /// The lit spans of line `line`, as CHAR ranges of the string `rows()`
    /// returns for it — after tab expansion and horizontal scroll. Empty for
    /// any other line.
    pub fn highlight_chars(&self, line: usize) -> Vec<(usize, usize)>;
}
```

- [ ] **Step 1: Failing tests** in viewer.rs's tests module:

```rust
    fn target(line: u64, offset: Option<u64>, preview: &str, ranges: &[(u32, u32)]) -> ViewerTarget {
        ViewerTarget { line, offset, preview: preview.to_owned(), ranges: ranges.to_vec() }
    }

    #[test]
    fn aim_scrolls_to_the_line_and_lights_the_match() {
        let mut v = Viewer::new(vp("mem:///a.txt"), b"uno\ndos aguja\ntres\n".to_vec(), false);
        v.aim(&target(2, Some(4), "dos aguja", &[(4, 9)]));
        assert_eq!(v.scroll, 1);
        assert_eq!(v.highlight_line(), Some(1));
        assert_eq!(v.highlight_chars(1), vec![(4, 9)]);
        assert!(v.highlight_chars(0).is_empty());
    }

    /// C7: the same text twice on the line — the preview's position picks
    /// the right occurrence, not the first.
    #[test]
    fn the_preview_picks_which_occurrence() {
        let line = format!("aguja {} aguja fin", "x".repeat(300));
        let mut v = Viewer::new(vp("mem:///a.txt"), format!("{line}\n").into_bytes(), false);
        let tail: String = line.chars().skip(200).collect(); // a preview cut on the left
        let preview = format!("…{tail}");
        let at = u32::try_from(preview.chars().position(|c| c == 'a').expect("a")).expect("small");
        v.aim(&target(1, Some(0), &preview, &[(at, at + 5)]));
        // "aguja " (6) + 300 x + " " → the second `aguja` is chars 307..312.
        assert_eq!(v.highlight_chars(0), vec![(307, 312)]);
    }

    /// A tab before the match moves it in the PAINTED row; so does scrolling sideways.
    #[test]
    fn tabs_and_horizontal_scroll_move_the_highlight() {
        let mut v = Viewer::new(vp("mem:///a.txt"), b"\taguja\n".to_vec(), false);
        v.aim(&target(1, Some(0), "\taguja", &[(1, 6)]));
        assert_eq!(v.highlight_chars(0), vec![(TAB_WIDTH, TAB_WIDTH + 5)]);
        v.scroll_right(2);
        assert_eq!(v.highlight_chars(0), vec![(TAB_WIDTH - 2, TAB_WIDTH + 3)]);
    }

    /// Past the cap: the window starts mid-line, that partial line goes, and
    /// the lines are numbered from the target's.
    #[test]
    fn a_window_past_the_cap_numbers_from_the_known_line() {
        let near = target(1000, Some(10_000), "aguja", &[(0, 5)]);
        assert_eq!(read_for(&near, 256 * 1024).offset, 0, "inside the cap reads from the top");
        let far = target(90_000, Some(1_000_000), "aguja", &[(0, 5)]);
        assert_eq!(read_for(&far, 256 * 1024).offset, 1_000_000 - CONTEXT_BEFORE);
        // The window: half a line, two whole lines, then the target.
        let bytes = b"tial\nuno\ndos\naguja\n".to_vec();
        let start = 1_000_000 - 13; // `aguja` starts at byte 1_000_000
        let v = Viewer::window(vp("mem:///a.txt"), bytes, true, start, &far);
        assert_eq!(v.first_line(), 89_998);
        assert_eq!(v.highlight_line(), Some(2));
    }

    #[test]
    fn hex_lights_nothing() {
        let mut v = Viewer::new(vp("mem:///a.bin"), b"\0\0aguja".to_vec(), false);
        assert!(v.hex);
        v.aim(&target(1, Some(0), "aguja", &[(0, 5)]));
        assert_eq!(v.highlight_line(), None);
    }
```

Plus one for a plugin preview (build it with the existing `with_plugin_preview` test helper): `aim` leaves `highlight_line() == None`. And a wide-char case: `"日本 aguja"` lights `(3, 8)` in the row string (chars, not cells).
- [ ] **Step 2: Run** `just t norte-frontend` — FAIL.
- [ ] **Step 3: Implement.** `aim`: no-op on `hex` or `plugin_preview`; `idx = t.line.saturating_sub(first_line)` clamped into `0..lines`; `scroll = idx` (the spec's `line - 1`); locate: `masked = mask_terminal_hazards(line)`, `core` = preview without a leading/trailing `…`; if `masked.find(core)` gives a byte position, its char index `k` shifts the ranges by `k - lead` (`lead` = 1 when the preview starts with `…`); otherwise each range's substring of the preview is searched in `masked` and every occurrence lit. `highlight_chars`: walk the source line once computing each char's rendered cell span with `render_line`'s rules (tab to the next `TAB_WIDTH` stop, hazard = 1 cell, else `UnicodeWidthChar::width`), map a range to cells, subtract `hscroll` (clip at 0), and map cells back to char indices of the row string `rows()` returns (walk its chars with their widths). `read_for`: `Some(o) if o >= cap` → `offset: o.saturating_sub(CONTEXT_BEFORE)`, else `0`. `window`: when `start > 0` drop through the first `\n`; `first_line = t.line - (count of '\n' between that point and offset - start)`; `new` on the rest; `aim`.
- [ ] **Step 4: Run** `just t norte-frontend`, `cargo test -p norte-frontend --doc` — PASS; `just c`.
- [ ] **Step 5: Reviewer** `encoding-auditor` (masking vs. the preview, the cell math). **Commit** (`feat(viewer): open at a line and light up the match`).

---

### Task 7: The TUI paints the panel, types into it, and runs the search

**Files:**
- Modify: `crates/norte-tui/src/searchpanel.rs` (keys, run, drain, paint helpers), `app.rs` (`search_panel_focus: crate::searchpanel::SearchFocus`), `keys.rs` (the shared-chord branch ~224 adds `&& !crate::searchpanel::typing(app)` like `log_filter_input.is_none()`, and `KeyOwner::Search` to its `matches!`; a branch `KeyOwner::Search && !modal_wins(app)` → `crate::searchpanel::on_key(..).await` next to the timeline's ~337), `jobs/inflight.rs` (`search_panel: Option<searchpanel::PanelRun>`), `event_loop.rs` (a `select!` arm draining `work.search_panel`, like the `work.search` arm ~1152), `turn.rs` (`crate::searchpanel::tick(app, backend, work).await` where `timeline_stale` is consumed ~163), `ui.rs` + `ui/panels.rs` (`draw_search_panel`, `search_panel_window`), `mouse.rs` (`side_panel_click` tries the search panel's rows)
- Test: `crates/norte-tui/tests/searchpanel.rs`, `crates/norte-tui/src/ui/panels.rs` (a `mod draw_search_panel_tests` like `draw_log_tests` ~1645)

**Interfaces:**
- Consumes: Task 4 model; Task 5 slot fns.
- Produces:

```rust
// crates/norte-tui/src/searchpanel.rs
/// Where the panel's keys go in the terminal (the window's inputs own their caret).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchFocus { Field(norte_frontend::search::SearchField), Tree }   // Default: Field(Name)
pub struct PanelRun { pub epoch: u64, pub task: TaskRef, pub rx: tokio::sync::mpsc::Receiver<SearchHits> }
pub fn typing(app: &App) -> bool;                                                            // focus is an input
pub async fn on_key(app: &mut App, backend: &Backend, work: &mut InFlight, resolver: &mut Resolver, mods: KeyModifiers, code: KeyCode);
pub async fn tick(app: &mut App, backend: &Backend, work: &mut InFlight);                    // debounce → cancel + launch
pub fn drain(app: &mut App, run: &mut Option<PanelRun>, hits: Option<SearchHits>);
pub fn cancel(run: &mut Option<PanelRun>);                                                   // close and re-root
```

(Use the real paths of `TaskRef`, `Backend`, `Resolver`, `InFlight` as `jobs/search.rs` and `screens/side_nav.rs` import them.)

Keys with an input focused (fixed keys, like the Alt+F7 dialog `jobs/search.rs` ~55): printable (no ctrl/alt) → `push_char`; Backspace; F2 regex, F3 case, F4 whole word, F5 subdirectories, F6 kinds (the dialog's keys); F7 search here (`set_root(focused dir)` + `cancel`); F8 fold the filters; Tab → next input (`Name → Content → [filters open: Exclude … Encoding] → Tree → Name`); Down on the last input → Tree; Enter → `force`; Esc → `app.return_keys_to_panes()`; Ctrl+C quits as everywhere. Tree focus resolves through the `dialog` keymap like `on_timeline_key` (`dialog.up/down/page-up/page-down/top/bottom` move; Left/Right fold — check which ids `[dialog]` gives them in `presets/keymap/orthodox.toml`, else match the raw `KeyCode::Left/Right` first; `dialog.confirm` activates — `Toggled` here, GoTo/View in Task 8; `dialog.pane`/Tab → Name; `dialog.cancel` → panes).

Paint (`draw_search_panel(frame, area, panel, focus, app, with_keyboard)`), inside the border titled `search-panel-title`, top to bottom: `search-panel-root` dim with the root's `display_lossy`; the name and content inputs (label, masked text, `▏` caret on the focused one), `[.*] [Aa] [ab]` lit when on after the content input; `▸/▾ search-panel-filters` and, open, the other five fields; the status line (translated `status()`, dim when `is_dimmed()`); the tree windowed around the cursor (like `timeline_window`): `▸/▾ dir (n)`, files indented 2 with the name, lines indented 4 with the line number dim and the preview with `Role::Mark` over its `ranges`, `search-panel-more` rows dim. Everything from disk through `display_name`/`mask_terminal_hazards`.

- [ ] **Step 1: Failing tests** in `tests/searchpanel.rs`; the async ones use the embedded backend over a tempdir the way the existing search tests do (`rg -ln "launch_search|begin_search" crates/norte-tui/tests`):

```rust
/// Typing goes to the focused input and arms the pause; F2 toggles regex.
#[tokio::test]
async fn typing_writes_the_focused_input_and_f2_toggles_regex() {
    let (mut app, backend, mut work, mut resolver) = harness().await; // this file's helper
    app.toggle_search_panel();
    for code in [KeyCode::Char('a'), KeyCode::Char('g'), KeyCode::F(2)] {
        norte_tui::searchpanel::on_key(&mut app, &backend, &mut work, &mut resolver, KeyModifiers::NONE, code).await;
    }
    let p = app.panes.search_panel(app.search_panel_slot().expect("open")).expect("model");
    assert_eq!((p.form().name.as_str(), p.form().regex), ("ag", true));
}

/// Review Focus 2: a second query after the pause cancels the first run.
#[tokio::test]
async fn a_second_query_cancels_the_first_run() {
    let (mut app, backend, mut work, mut resolver) = harness_with_tree(&[("a.txt", "aguja\n")]).await;
    app.toggle_search_panel();
    type_in(&mut app, &backend, &mut work, &mut resolver, "ag").await;
    tokio::time::sleep(norte_frontend::search_panel::DEBOUNCE).await;
    norte_tui::searchpanel::tick(&mut app, &backend, &mut work).await;
    let first = work.search_panel.as_ref().expect("launched").task.clone();
    type_in(&mut app, &backend, &mut work, &mut resolver, "u").await;
    tokio::time::sleep(norte_frontend::search_panel::DEBOUNCE).await;
    norte_tui::searchpanel::tick(&mut app, &backend, &mut work).await;
    assert_eq!(wait_terminal(&first).await, norte_proto::TaskState::Cancelled);
}

/// Review Focus 5: closing the panel cancels the run.
#[tokio::test]
async fn closing_the_panel_cancels_its_run() {
    let (mut app, backend, mut work, mut resolver) = harness_with_tree(&[("a.txt", "aguja\n")]).await;
    app.toggle_search_panel();
    type_in(&mut app, &backend, &mut work, &mut resolver, "ag").await;
    tokio::time::sleep(norte_frontend::search_panel::DEBOUNCE).await;
    norte_tui::searchpanel::tick(&mut app, &backend, &mut work).await;
    let task = work.search_panel.as_ref().expect("launched").task.clone();
    norte_tui::searchpanel::close(&mut app, &mut work); // close_search_panel + cancel
    assert!(work.search_panel.is_none());
    assert_eq!(wait_terminal(&task).await, norte_proto::TaskState::Cancelled);
}

/// The panel with results: a highlighted line, folded filters, a `+N` row.
#[test]
fn snapshot_search_panel_with_results() {
    let mut app = test_app();
    app.render_now_ms = Some(0);
    app.toggle_search_panel();
    let slot = app.search_panel_slot().expect("open");
    let p = app.panes.search_panel_mut(slot).expect("model");
    let t0 = std::time::Instant::now();
    p.edit(norte_frontend::search::SearchField::Content, "aguja".to_owned(), t0);
    let Some(norte_frontend::search_panel::Due::Launch { epoch, .. }) =
        p.due(t0 + norte_frontend::search_panel::DEBOUNCE, 0) else { panic!("launch") };
    p.apply_hits(epoch, &hits_with_51_lines()); // this file's helper: one file, 51 lines, more 3
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    terminal.draw(|f| norte_tui::ui::draw(f, &app)).expect("draw");
    insta::assert_snapshot!(terminal.backend().to_string());
}
```

(`harness`, `harness_with_tree`, `type_in`, `wait_terminal`, `hits_with_51_lines` are small helpers in this test file; if the TUI's `TaskRef` is not `Clone`, keep its progress `watch` instead. If tasks race the 300 ms sleep, use `tokio::time::pause()` and pass `tokio::time::Instant::now().into_std()` the same way the host does in Task 10.) And in `ui/panels.rs`, a paint test that the cells of a range carry `theme.role(Role::Mark)`'s style on the `TestBackend` buffer and the cells next to them do not.
- [ ] **Step 2: Run** `just t norte-tui` — FAIL.
- [ ] **Step 3: Implement.** `tick`: for the open panel, `due(Instant::now(), now_ms)`; `Launch` → `cancel(&mut work.search_panel)` then `backend.search(*params).await` → `work.search_panel = Some(PanelRun { epoch, task, rx })`, an error → `finished(epoch, Outcome::Failed(category))` (the category as `jobs/search.rs` builds it); `Clear` → `cancel`; `Unreadable` → nothing more. Set `app.needs_frame = true` on every change (`quiet_for_a_tick` skips the frame otherwise). `drain`: `Some(batch)` → `apply_hits(run.epoch, &batch)`; `None` → `finished(run.epoch, outcome_of(task state))` (`search_status::outcome_of`) and drop the run. The panel's runs do NOT go on the TUI board (D3). `close_search_panel` and F7 call `cancel`.
- [ ] **Step 4: Run** `just t norte-tui`, `cargo insta review` → `accept`, `just c`.
- [ ] **Step 5: Commit** (`feat(tui): the search panel searches as you type`).

---

### Task 8: In the TUI, a result opens where it matched

**Files:**
- Modify: `crates/norte-tui/src/searchpanel.rs` (`Activation::GoTo` and `View`), `crates/norte-tui/src/viewer_open.rs` (`open_viewer_at`), `crates/norte-tui/src/ui/panels.rs` (`draw_viewer` ~212 and `draw_preview` ~335: rows with `highlight_chars` become `Line::from(spans)` with `Role::Mark` on the ranges instead of `Line::raw`; the title adds `viewer-lines-from` when `first_line() > 1`)
- Test: `crates/norte-tui/tests/searchpanel.rs`

**Interfaces:**
- Consumes: Task 6 (`read_for`, `Viewer::window`, `aim`, `highlight_chars`, `highlight_line`, `first_line`), Task 4 `Activation`.
- Produces: `pub async fn open_viewer_at(app: &mut App, backend: &Backend, events: &mut crate::console::Console<'_>, path: VPath, target: ViewerTarget)` — reads `read_for(&target, VIEW_CAP)`; at offset 0 builds the viewer like `viewer_for` then `aim`s (plugin previews are not aimed); past the cap `Viewer::window`; a target without `offset` whose line is beyond what was read opens at the top and says `viewer-target-past-cap` (C9).

GoTo: the focused listing navigates to `dir` with the cursor on `entry` (the way the Alt+F7 results land on a file: `rg -n "pending_focus|set_pending" crates/norte-tui/src`), and the keys go back to the panes.

- [ ] **Step 1: Failing tests:** activating a `Line` row opens the viewer with `highlight_line() == Some(line - 1)` and `scroll == line - 1`; activating a `File` row moves the focused pane to its folder with the cursor on it; a 300 KiB file in a tempdir whose match is past 256 KiB opens with `first_line() > 1` and `highlight_line()` on the target (target built by hand from the known byte offset); a viewer snapshot with the highlighted span.
- [ ] **Step 2: Run** `just t norte-tui` — FAIL. **Step 3: Implement.** **Step 4: Run** `just t norte-tui`, snapshot review, `just c`.
- [ ] **Step 5: Commit** (`feat(tui): a search result opens the viewer at its line`).
- [ ] **Step 6: Gate** — `just ci-fast` once (Tasks 5–8).

---

### Task 9: The window's viewer opens at a target and paints the highlight (bridge 108)

**Files:**
- Modify: `crates/norte-ui-host/src/controller/viewer.rs` (`request_viewer_of` gains `target: Option<ViewerTarget>`, existing callers pass `None`; with a target the read is `read_for(&t, VISOR_CAP)` inside the same `DEADLINE_VISOR` timeout; the target rides `Message::Content` — widen its tuple with `Option<(u64 /*start*/, ViewerTarget)>`; `open_visor` builds `Viewer::window` when start > 0, else `new` + `aim`; plugin styled previews are not aimed)
- Modify: `crates/norte-ui-host/src/dto.rs` `ViewerView` (+ `#[serde(default, skip_serializing_if = "Option::is_none")] pub highlight: Option<HighlightView>`, + `#[serde(default = "one")] pub lines_from: u64`), new `pub struct HighlightView { pub row: u64 /* line, 0-based */, pub ranges: Vec<(u32, u32)> /* chars of lines[row - first_line] */ }`; the projection fills them from `highlight_line()`/`highlight_chars()`/`first_line()`
- Modify: `crates/norte-ui-host/src/bridge.rs` (`BRIDGE_VERSION = 108`, doc `- **108**: \`ViewerView.highlight\` and \`lines_from\`; \`SlotView::Search\` and the \`search_panel_*\` actions: the search panel (spec 2026-10-09).`), `crates/norte-gui-tauri/ui/src/types.ts` (`BRIDGE_VERSION = 108`, the two fields)
- Modify: the webview viewer renderer (`rg -ln "first_line" crates/norte-gui-tauri/ui/src/render`): a highlighted row is split by code points (`Array.from(line)`: the ranges are chars, not UTF-16 units) into text nodes and `<mark>` elements built with `textContent`; the title shows `viewer-lines-from` when `lines_from > 1`
- Goldens: the reference viewer in `crates/norte-ui-host/tests/golden.rs` gains a highlight; `NORTE_BLESS=1 just t norte-ui-host`
- Test: the host's viewer controller tests, `crates/norte-gui-tauri/ui/tests/viewer.test.ts`

**Interfaces:**
- Consumes: Task 6.
- Produces: `State::request_viewer_of(&mut self, path: VPath, target: Option<norte_frontend::viewer::ViewerTarget>, backend, mailbox)`; `HighlightView`. Task 10 calls it with `Some(target)`.

- [ ] **Step 1: Failing tests:** host — a target inside the cap publishes a viewer with `first_line == line - 1` and `highlight == Some(HighlightView { row: line - 1, ranges })`; a target past `VISOR_CAP` makes the fake backend's `read` see `offset == target.offset - CONTEXT_BEFORE` and the view carries `lines_from > 1`. Webview:

```ts
it("a highlighted range is a <mark> over the right code points", () => {
  const { screen } = mount();
  const v = view({});
  v.viewer = viewerView({ lines: ["añ aguja fin"], first_line: 0, highlight: { row: 0, ranges: [[3, 8]] } });
  screen.paint(v);
  expect(document.querySelector(".viewer mark")?.textContent).toBe("aguja");
});
```

(`viewerView`: the fixture builder the viewer tests use; add one if there is none.)
- [ ] **Step 2: Run** `just t norte-ui-host`, `just gui-test-ui` — FAIL. **Step 3: Implement.** **Step 4: Run** both, `NORTE_BLESS=1 just t norte-ui-host` + review the golden diff, `just gui-lint-ui`, `just c`.
- [ ] **Step 5: Commit** (`feat(gui): the viewer opens at a line and lights up the match`).

---

### Task 10: The host runs the panel: `SlotView::Search`, its actions, the debounce, one board row

**Files:**
- Create: `crates/norte-ui-host/src/controller/searchpanel.rs` (`pub(super) const KIND: &str = "search";`, the state, the projection, the actions, the tic)
- Modify: `controller/mod.rs` (`search_panel: Option<(u32 /*slot*/, searchpanel::PanelState)>`; `Message::SearchPanelTic`; `Background::SearchPanelHits(u64, Box<SearchHits>)`, `SearchPanelLive(u64, TaskId)`, `SearchPanelBroken(u64, Box<Error>)` handled in `controller/extensions.rs`; the actions in `apply`/`row_by_index`), `controller/views.rs` (`Some(searchpanel::KIND) => slots.push(SlotView::Search(Box::new(self.search_panel_view(id))))`), `controller/tabs.rs` (`close_slot_of_kind` for `search`: abandon the run, forget its board row, drop the state; opening creates `PanelState::new(root = the active pane's dir)`), `controller/listing.rs` `effect_on_focused_pane` (`searchpanel::KIND` → `self.search_panel_effect(effect)`: `Cursor`/`Page`/`End` move, `Enter` activates, `Left`/`Right` fold), `dto.rs`, `action.rs`, `tests/golden.rs` (`action_tag` arms, `overlay_actions` cases, `reference_slots` case)
- Modify: `crates/norte-gui-tauri/ui/src/types.ts` (the DTOs and the actions; same bridge 108)
- Test: `crates/norte-ui-host/tests/controller/search_panel.rs` (+ `mod search_panel;` in `tests/controller/main.rs`); Task 1's cancel counter on the fake, plus `searches_launched()`

**Interfaces:**
- Consumes: Task 4 model, Task 1's `abandon_search` shape, Task 9's `request_viewer_of(path, Some(target), ..)`.
- Produces (wire, bridge 108):

```rust
// dto.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchPanelSlotView {
    pub slot_id: u32,
    pub title: String,
    pub root: String,
    pub root_hostile: bool,
    /// The form: the SAME fields the dialog shows (`search_fields`).
    pub fields: Vec<DialogFieldView>,
    pub filters_open: bool,
    pub filters_set: u32,
    /// The field that cannot be read (`SearchField::id`), if any.
    pub unreadable: Option<String>,
    pub status: String,
    pub running: bool,
    pub dimmed: bool,
    /// A window of the rows: `rows[i]` is row `first_row + i` of `total_rows`.
    pub first_row: u64,
    pub total_rows: u64,
    pub rows: Vec<SearchPanelRowView>,
    pub cursor: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "row")]
pub enum SearchPanelRowView {
    Folder { path: String, hostile: bool, count: u32, open: bool },
    File { name: String, hostile: bool, count: u32 },
    Line { line: u64, preview: String, ranges: Vec<(u32, u32)> },
    More { label: String },
}
// action.rs (UiAction)
SearchPanelEdit { slot_id: u32, field: String, value: DialogFieldValue },   // the ids of `search_fields`
SearchPanelGo { slot_id: u32 },                                              // Enter in an input
SearchPanelReroot { slot_id: u32 },                                          // "Search here"
SearchPanelFilters { slot_id: u32 },                                         // fold the filters
SearchPanelActivate { slot_id: u32, row: u64 },                              // click on a row
SearchPanelFold { slot_id: u32, row: u64, open: bool },                      // click on ▸/▾
SearchPanelVisibleRange { slot_id: u32, first: u64, count: u32 },            // virtual scroll
```

The projected window is `[first, first + count)`, clamped, 200 rows around the cursor by default, never more than 500.

- [ ] **Step 1: Failing tests** (`tests/controller/search_panel.rs`, with the module's existing harness and `tokio::time::pause`):

```rust
/// Review Focus 2: edits within the pause launch ONE search; a later edit
/// cancels it, and the board keeps one row for the panel.
#[tokio::test(start_paused = true)]
async fn typing_launches_once_and_relaunching_keeps_one_board_row() {
    let (mut h, fake) = harness_with_fake().await;
    let slot = h.open_slot("layout.search").await;
    for t in ["a", "ag", "agu"] {
        h.act(UiAction::SearchPanelEdit { slot_id: slot, field: "content".into(), value: DialogFieldValue::Text { text: t.into() } }).await;
        tokio::time::advance(Duration::from_millis(100)).await;
    }
    tokio::time::advance(DEBOUNCE).await;
    h.settle().await;
    assert_eq!(fake.searches_launched(), 1);
    h.act(UiAction::SearchPanelEdit { slot_id: slot, field: "content".into(), value: DialogFieldValue::Text { text: "aguj".into() } }).await;
    tokio::time::advance(DEBOUNCE).await;
    h.settle().await;
    assert_eq!((fake.searches_launched(), fake.search_cancels.load(Ordering::SeqCst)), (2, 1));
    assert_eq!(h.snapshot().tasks.iter().filter(|t| t.kind == "search").count(), 1);
}

/// A batch becomes rows: folder, file, the line with its ranges.
#[tokio::test(start_paused = true)]
async fn hits_become_rows_with_ranges() {
    let (mut h, fake) = harness_with_fake().await;
    let slot = h.open_slot("layout.search").await;
    fake.answer_search_with(one_file_one_line("mem:///casa/a.txt", 7, "x aguja", (2, 7)));
    h.act(UiAction::SearchPanelEdit { slot_id: slot, field: "content".into(), value: DialogFieldValue::Text { text: "aguja".into() } }).await;
    h.act(UiAction::SearchPanelGo { slot_id: slot }).await;
    h.settle().await;
    let SlotView::Search(s) = h.slot(slot) else { panic!("search slot") };
    assert!(matches!(&s.rows[..], [
        SearchPanelRowView::Folder { .. },
        SearchPanelRowView::File { .. },
        SearchPanelRowView::Line { line: 7, ranges, .. },
    ] if ranges == &vec![(2, 7)]));
}

/// Clicking a line opens the viewer at it, highlighted.
#[tokio::test(start_paused = true)]
async fn activating_a_line_opens_the_viewer_at_it() {
    // as above, then:
    // h.act(UiAction::SearchPanelActivate { slot_id: slot, row: 2 }).await; h.settle().await;
    // the published viewer has highlight.row == 6 and first_line == 6.
}

/// Review Focus 5: closing the panel cancels the run and forgets its row.
#[tokio::test(start_paused = true)]
async fn closing_the_panel_cancels_its_run() {
    // launch as in the first test, then toggle `layout.search` off:
    // search_cancels == 1 and no task row of kind "search" remains.
}
```

(`answer_search_with`, `one_file_one_line`, `searches_launched`, `open_slot`, `slot` are helpers on the fake/harness — add the ones that do not exist. Write the two commented bodies out in full in the same style as the first two.) The model's `Instant` is std's: the host passes `tokio::time::Instant::now().into_std()` so the paused clock drives the debounce. Golden: `reference_slots()` gains a `SlotView::Search` with one row of each variant; `overlay_actions()` one case per new action; `action_tag()` its arms.
- [ ] **Step 2: Run** `just t norte-ui-host` — FAIL.
- [ ] **Step 3: Implement.** `PanelState { model: SearchPanel, run: Option<PanelRunHost>, window: (u64, u32) }`, `PanelRunHost { epoch: u64, task: TaskId /* 0 until live */, abandoned: Arc<AtomicBool> }`, plus `previous_row: Option<u64>`. An edit/toggle/re-root → the model with `now`, then `tokio::spawn(sleep(DEBOUNCE) → Message::SearchPanelTic)` (C5); the tic's handler calls `due` and acts: `Launch` → abandon the current run (Task 1's shape) and spawn the forwarder `launch_search` uses, sending `SearchPanelHits`/`SearchPanelLive`/`SearchPanelBroken` and the usual `Message::TaskNew`; `Clear` → abandon. `SearchPanelLive(epoch, id)` → if current, remember `id`, and remove the previous run's board row (C4: `self.tasks.remove(&old)` + republish `ViewChange::Tasks`, the way `expire_task` does). Every change republishes ONLY the slot (`ViewChange::Slot { slot: Box::new(SlotView::Search(..)) }`, the log's pattern `logpanel.rs` ~608). `Activation::GoTo` → `set_pending_focus(entry)` + `navigate(dir)` (as `go_to_result`); `View` → `request_viewer_of(path, Some(target), ..)`. The status line is translated with `t_in(self.lang, key)` and `n`/`folders`/`error`/`field` args.
- [ ] **Step 4: Run** `just t norte-ui-host`, `NORTE_BLESS=1 just t norte-ui-host` + review, `cargo test -p norte-ui-host --doc`, `just c`.
- [ ] **Step 5: Reviewer** `rust-reviewer` (the abandon/cancel race before the id is known, the epoch check on `SearchPanelLive`, removing a board row of a task still being cancelled). **Commit** (`feat(ui-host): the search panel runs in the window's host`).
- [ ] **Step 6: Gate** — `just ci-fast` once (Tasks 9–10).

---

### Task 11: The webview paints the panel

**Files:**
- Create: `crates/norte-gui-tauri/ui/src/render/searchpanel.ts`
- Modify: `render.ts` (`import * as searchpanel from "./render/searchpanel";`, `readonly paintSearchPanel = searchpanel.paintSearchPanel;`, dispatch `if (slot.kind === "search") { this.paintSearchPanel(dom, slot); return; }` next to the timeline's ~1067), `render/icons.ts` (`import search from "../../../../norte-frontend/assets/panel-icons/search.svg?raw";` and `search` in `PANEL_SVG`), `style.css` (`.searchpanel-*`; colours only from variables every theme already feeds — `gui-ci` fails otherwise)
- Test: `crates/norte-gui-tauri/ui/tests/render.test.ts`

**Interfaces:**
- Consumes: Task 10's DTOs and actions (`types.ts`).

Structure (inputs as `render/log.ts` ~151-185 does them; the cursor as `render/menus.ts` does since commit a80cb896):
- Controls built ONCE per slot and kept across paints (a module-level `WeakMap<SlotDom, Controls>`): root line with a "Search here" button (`search_panel_reroot`); name and content `<input data-field>` (`input` → `search_panel_edit` with `{ set: "text", text }`; `Enter` → `search_panel_go`); toggle buttons for the fields of kind toggle/cycle (`{ set: "toggled" }` / `{ set: "cycled" }`); a fold for the other five inputs (`search_panel_filters`). An input's `value` is written only when it differs AND the input is not focused — the caret is the user's.
- Status `p[role=status][aria-live=polite]` with `data-dimmed`; `unreadable` marks its input `aria-invalid`.
- The tree `ul[role=tree]` of `li.searchpanel-row[role=treeitem]`; a line row = a dim line number and the preview, each range a `<mark>` — code-point slicing (`Array.from`), `textContent` only (Review Focus 4). Click on a row → `search_panel_activate`; click on a folder's chevron → `search_panel_fold`.
- Repaint: a signature of everything but `cursor` via `unchanged()` (`render/dom.ts`); same signature → move `aria-selected`/`aria-activedescendant` on the existing nodes and return; otherwise rebuild the tree only, never the inputs.
- Virtual scroll: spacers before and after the window (`first_row`, `total_rows`, a fixed row height in CSS); on scroll send `search_panel_visible_range` only when first/count change.

- [ ] **Step 1: Failing tests** (`tests/render.test.ts`, existing `mount()`/`view()` helpers):

```ts
const panelSlot = (over: Partial<SearchPanelSlotView> = {}): SearchPanelSlotView => ({
  kind: "search", slot_id: 9, title: "Search", root: "/casa", root_hostile: false,
  fields: [
    { id: "name", label_key: "search-name", value: "", hostile: false, kind: { kind: "text" } },
    { id: "content", label_key: "search-content", value: "aguja", hostile: false, kind: { kind: "text" } },
  ],
  filters_open: false, filters_set: 0, unreadable: null, status: "1 files in 1 folders",
  running: false, dimmed: false, first_row: 0, total_rows: 3, cursor: 2,
  rows: [
    { row: "folder", path: "/casa", hostile: false, count: 1, open: true },
    { row: "file", name: "a.txt", hostile: false, count: 1 },
    { row: "line", line: 7, preview: "x <b>aguja</b>", ranges: [[2, 7]] },
  ],
  ...over,
});
function withPanel(slot: SearchPanelSlotView) {
  const v = view({});
  v.slots = [...v.slots, slot];
  v.layout.placements = [...v.layout.placements, { slot_id: 9, x: 0, y: 0, width: 40, height: 20, role: null, focus_index: 2 }];
  return v;
}

it("a match is a <mark> of text, never markup", () => {
  const { screen } = mount();
  screen.paint(withPanel(panelSlot()));
  expect(document.querySelector(".searchpanel-line mark")?.textContent).toBe("<b>ag");
  expect(document.querySelector(".searchpanel-line b")).toBeNull();
});

it("typing sends an edit and a stale echo does not move the caret", () => {
  const { screen, sent } = mount();
  screen.paint(withPanel(panelSlot()));
  const input = document.querySelector<HTMLInputElement>(".searchpanel-input[data-field='content']")!;
  input.focus();
  input.value = "agu";
  input.dispatchEvent(new Event("input"));
  expect(sent.at(-1)).toEqual({ action: "search_panel_edit", slot_id: 9, field: "content", value: { set: "text", text: "agu" } });
  screen.paint(withPanel(panelSlot({ fields: panelSlot().fields.map((f) => (f.id === "content" ? { ...f, value: "ag" } : f)) })));
  expect(input.value).toBe("agu");
  expect(document.activeElement).toBe(input);
});

it("a cursor-only change moves the mark on the SAME nodes", () => {
  const { screen } = mount();
  screen.paint(withPanel(panelSlot()));
  const before = [...document.querySelectorAll(".searchpanel-row")];
  screen.paint(withPanel(panelSlot({ cursor: 1 })));
  const after = [...document.querySelectorAll(".searchpanel-row")];
  expect(after).toEqual(before);
  expect(after[1]?.getAttribute("aria-selected")).toBe("true");
});

it("a click on a line activates it", () => {
  const { screen, sent } = mount();
  screen.paint(withPanel(panelSlot()));
  (document.querySelectorAll(".searchpanel-row")[2] as HTMLElement).click();
  expect(sent.at(-1)).toEqual({ action: "search_panel_activate", slot_id: 9, row: 2 });
});
```

(The literal `"x <b>aguja</b>"` with range `[2, 7]` covers `<b>ag`: the point is that markup arrives as text. Match `DialogFieldView`'s real TS shape for `kind` from `types.ts`.)
- [ ] **Step 2: Run** `just gui-test-ui` — FAIL. **Step 3: Implement.** **Step 4: Run** `just gui-test-ui`, `just gui-lint-ui`.
- [ ] **Step 5: Commit** (`feat(gui): the search panel in the window`).
- [ ] **Step 6: Gate** — `just gui-ci` once.

---

### Task 12: Help topic and CHANGELOG

**Files:**
- Modify: `crates/norte-help/topics/en/finding.md`, `crates/norte-help/topics/es/finding.md`: a section "The search panel" / "El panel de búsqueda" — opening it (`{{cmd:layout.search}}`, the rail, Find → Search panel), searching after a pause as you type, Enter to search now, the toggles (and F2–F6, F7 "search here", F8 filters in the terminal), the tree and `+N more`, Enter or a click on a line opens the viewer at it with the match lit, on a file goes to its folder; the root is the folder the panel opened in; the Alt+F7 dialog is still there. The ES text is a translation of the EN, not a summary.
- Modify: `CHANGELOG.md` `[Unreleased]`: under `### Added` — "**Search panel** (protocol 0.86, bridge 108). `Alt+Shift+F`, its rail button or Find → Search panel opens it: it searches as you type, shows every matching line with the match highlighted, and opens the viewer at the line. The `Alt+F7` dialog is unchanged." Under `### Fixed` (create it if absent) — "In the window, starting a search stops the one it replaces; before, the old one kept walking the disk."
- Regenerate: `NORTE_UPDATE_GOLDEN=1 just t norte-cli`

- [ ] **Step 1:** Write both topics. **Step 2:** `just t norte-help`, `just t norte-tui` (help gate), `NORTE_UPDATE_GOLDEN=1 just t norte-cli` + review. **Step 3: Commit** (`docs(help): the search panel`).
- [ ] **Step 4:** Whole-branch review (wire + both frontends + ADR 0077 parity: the same decisions in the terminal and the window), then merge `feat/search-panel` into `main` once; pushing `main` is the gate.

---

## Decisions this plan took that the spec did not

- **D1** `alt+F` in all seven presets (C1).
- **D2** `ViewerTarget` carries `preview` (C7); `Viewer::first_line` feeds the title, not a gutter (C6).
- **D3** The TUI does not put panel runs on its task board — its Alt+F7 search never did, and the TUI board has no remove-by-id; the panel's status line is the progress. The window keeps ONE row by removing the previous run's (C4). To add a TUI row later: `app.board.push(&task, None)` in `tick` plus a removal API on `TaskBoard`.
- **D4** Matches counted per LINE (C8); the request asks 51 so the 51st is the `+N more` target; N = hidden lines kept + lines the daemon counted.
- **D5** Every 0.86 search carries `offset`/`ranges` and the centred preview, not only the panel's: cheap, and the dialog, the Alt+F7 pane and MCP readers get a preview that shows the match.
- **D6** Old results stay dimmed until the new run's first batch instead of clearing on each keystroke.
- **D7** Regex validation in the panel only (`regex_unreadable`, C3).
- **D8** TUI panel keys: inputs use fixed keys (the dialog's F2–F6, plus F7 "search here", F8 filters); the tree uses the `dialog` keymap like the timeline. Docked left, 36 columns.
- **D9** Debounce: the host spawns a 300 ms sleep per edit (`SearchPanelTic`) checked against the model's deadline; the TUI polls the deadline every loop turn (≤100 ms), so it fires 300–400 ms after the last key.
- **D10** No ADR: the wire reasoning lives in the `# 0.86.0` rustdoc; write one only if the protocol reviewer asks.
- **D11** One bridge bump (108) for Tasks 9–11.

## Self-review

- Spec coverage: §1 → Tasks 2–3; §2 → Task 4; §3 → Task 5; §4 → Tasks 1, 7, 10; §5 → Tasks 6, 8, 9; §6 → Task 7; §7 → Tasks 10–11. Edge cases: old daemon → C2 + Task 4 test; binary files → Task 2 (no hit) and Task 4 (`apply_hits` drops line-less matches, the file row stays); 10 000 matches → Task 4 test; long lines → Task 2 (the cap stays; ranges only inside the evaluated part); bad regex → C3 + Task 4; root gone → `Failed` status, tree kept (Task 4 `finished`); panel closed → Tasks 7, 10; two windows → each host/TUI owns its run. Testing list → each task's Step 1. Help/CHANGELOG → Tasks 5, 12.
- Names checked across tasks: `SearchPanel`, `Due`, `Activation`, `Row`, `PanelStatus`, `ViewerTarget`, `read_for`, `CONTEXT_BEFORE`, `Viewer::{window, aim, first_line, highlight_line, highlight_chars}`, `KeyOwner::Search`, `searchpanel::KIND`, `SearchFocus`, `PanelRun`, `open_viewer_at`, `SlotView::Search`, `SearchPanelSlotView`, `SearchPanelRowView`, `HighlightView`, `search_panel_*` actions, `BRIDGE_VERSION = 108`, `PROTOCOL_VERSION = "0.86.0"`, `SEARCH_MATCHES_PER_FILE_MAX`, `Want`, `FileMatches`.
- Placeholders: the async test bodies that depend on harness helpers name the helpers and their assertions exactly; the implementer writes them against the existing harness.

