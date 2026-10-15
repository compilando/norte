# Orders in the box (plan a) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `:sel ext:pdf age<30d | cp >other` typed in the omnibox lights the
rows it will touch while typing and, on Enter, runs through the existing
copy/move/trash flows; `log`/`undo N` over the journal; `ntc run` for scripts.

**Architecture:** A new pure crate `norte-orders` parses and prints the
grammar (no I/O, no matching). `norte-frontend` evaluates a parsed order
against the pane's ALREADY LOADED listing — reusing `mark_glob`'s fold and
matcher so `sel` and the mark-by-pattern modal agree on equality — and
returns a list of steps. Each frontend executes the steps with the launchers
it already has (TUI `submit_transfers`/`submit_deletes`, ui-host
`launch_transfer`). **No wire change, no proto bump** (decided in
conversation 2026-10-14, amending spec §3).

**Tech Stack:** Rust 2024, thiserror, proptest; existing `globset`/`regex`
path in `norte-frontend::pane::marks`; clap in `norte-cli`; Svelte webview
under `crates/norte-gui-tauri/ui`.

**Spec:** `docs/superpowers/specs/2026-10-14-command-box-ai-design.md`
(§1, §2, §6 and `ntc run` from §3; §3's `orders.plan`/`orders.apply` are
REPLACED by the frontend resolution described here — Task 9 amends the spec).

## Global Constraints

- Filenames are bytes: names, globs and paths in the AST are `Vec<u8>`; never `String` except for display with explicit lossy conversion (hard rule 1).
- `norte-orders` depends on no `norte-*` crate at runtime; boundary test enforces it.
- No `unwrap`/`expect` outside tests without an invariant comment (hard rule 6); library errors via `thiserror`.
- Every mutation goes through the existing launchers, so journal + undo come from them; no new mutation path (hard rule 4).
- `redo` is OUT: the journal has no redo operation. The parser rejects `redo` with `ParseErrorKind::NotYet` and a test pins it.
- `sel` is NOT recursive in this plan: it matches the pane's current listing only.
- A key change finishes in all seven presets (`orthodox`, `vim`, `cua`, `krusader`, `far`, `norton`, `total-commander`) or says why not in the preset header.
- User-facing strings through Fluent, both locales (`crates/norte-i18n/i18n/{en,es}/…`).
- Test loop: `just t <crate>`; `cargo test -p <crate> --doc` after touching a public struct; `cargo doc -p <crate> --no-deps` after writing a doc link. `just ci-fast` ONCE after Task 4 and ONCE after Task 8. `just gui-ci` before pushing `main` (Task 5 touches `ui/`). Never `sleep` / `tail -f` to wait.

## Review Focus

1. **`sel` matches nothing, then `cp`.** `marked_paths()` falls back to the cursor row when no marks remain; an order whose `sel` matched 0 rows must run NOTHING, never copy the file under the cursor. Pinned in Task 2 (`zero_matches_never_falls_back_to_the_cursor`).
2. **Esc after a live preview.** The box marks rows live while typing; Esc must restore the exact previous mark set (including "no marks"), not leave the preview's marks behind. Pinned in Task 4 and Task 5.
3. **A name with `|`, a space, a quote or a non-UTF-8 byte typed or printed.** Printer output must parse back to the same bytes; a control byte must never reach the terminal raw. Pinned by the proptest in Task 1 and the hazard test in Task 3.
4. **`cp >other` where the other pane is the same directory, read-only, or an archive.** The order must go through the same validation F5 does (`request_transfer` / the ui-host equivalent), not around it. Pinned in Task 4 (`order_copy_reuses_the_f5_guard`).
5. **`undo 3` while new journal entries arrive.** The count shown must bound what is undone: pass `upto_seq` (the newest seq read). Pinned in Task 6.

---

### Task 1: `norte-orders` — grammar, parser, printer, escape

**Files:**
- Create: `crates/norte-orders/Cargo.toml`, `src/lib.rs`, `src/ast.rs`, `src/lex.rs`, `src/parse.rs`, `src/print.rs`
- Create: `crates/norte-orders/tests/roundtrip.rs`, `tests/parse.rs`, `tests/dependency_boundary.rs`
- Modify: root `Cargo.toml` (workspace member), `justfile` `core_pkgs` if the gate lists crates explicitly

Use the `new-crate` skill for scaffolding (lints, license, `#![warn(missing_docs)]`).

**Interfaces:**
- Produces:

```rust
// ast.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order { pub stages: Vec<Stage> }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    Sel(Query),
    Cp(Dest),
    Mv(Dest),
    Trash,
    Mkdir(Vec<u8>),          // one name, relative to the focused pane
    Cd(Vec<u8>),             // a path as typed; resolved by the frontend
    Log(Option<u32>),
    Undo(Option<u32>),
    Catalogue(String),       // any id containing '.', e.g. "pane.swap"
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dest { Other, Here, Path(Vec<u8>) }

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query { pub terms: Vec<Term> }   // implicit AND

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term { pub negated: bool, pub pred: Pred }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pred {
    Ext(Vec<Vec<u8>>),       // ext:pdf,PDF — compared folded by the frontend
    Name(Vec<u8>),           // name:GLOB, or a bare GLOB term
    Size(Cmp, u64),          // bytes; K/M/G are 1024-based
    Age(Cmp, u64),           // seconds; units s m h d w (m = minutes)
    Kind(KindSel),           // type:file|dir|link
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp { Lt, Le, Gt, Ge, Eq }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KindSel { File, Dir, Link }

// parse.rs
pub fn parse(line: &str) -> Result<Order, ParseError>;
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{kind} at byte {offset}")]
pub struct ParseError { pub offset: usize, pub kind: ParseErrorKind }
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseErrorKind {
    #[error("empty order")] Empty,
    #[error("unknown verb")] UnknownVerb,
    #[error("not yet")] NotYet,               // `redo`
    #[error("stands alone")] StandsAlone,     // log/undo with a pipe
    #[error("bad term")] BadTerm,
    #[error("bad number")] BadNumber,
    #[error("bad escape")] BadEscape,
    #[error("missing destination")] MissingDest,
    #[error("unterminated quote")] Unterminated,
}

// print.rs
pub fn print(order: &Order) -> String;

// lib.rs
pub const VERBS: &[&str] = &["sel", "cp", "mv", "trash", "mkdir", "cd", "log", "undo"];
/// Token spans for colouring: (byte range in the line, class).
pub fn tokens(line: &str) -> Vec<(std::ops::Range<usize>, TokenClass)>;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenClass { Verb, Key, Value, Pipe, Dest, Error }
```

Escaping rules (lex.rs), one home for them — document them on `print`:
`\xHH` any byte; `\\`; `\ ` space; `\|`; `\'`. A `'…'` quoted word takes the
same escapes. The printer writes printable UTF-8 as-is and escapes:
non-UTF-8 bytes, ASCII controls and `norte_encoding`-style hazards (C0, DEL,
C1, bidi controls U+202A–U+202E, U+2066–U+2069) as `\xHH` of their UTF-8
bytes, plus space `|` `'` `\`. `parse` must accept `print`'s output for every
`Order`.

- [ ] **Step 1: Write the failing tests** — `tests/parse.rs`:

```rust
use norte_orders::*;

fn q(line: &str) -> Order { parse(line).expect(line) }

#[test]
fn the_flagship_line() {
    assert_eq!(q("sel ext:pdf age<30d | cp >other"), Order { stages: vec![
        Stage::Sel(Query { terms: vec![
            Term { negated: false, pred: Pred::Ext(vec![b"pdf".to_vec()]) },
            Term { negated: false, pred: Pred::Age(Cmp::Lt, 30 * 86_400) },
        ]}),
        Stage::Cp(Dest::Other),
    ]});
}

#[test]
fn units_negation_bare_glob_and_kinds() {
    let o = q("sel size>=1.5M !name:*sample* *.mkv type:file");
    let Stage::Sel(Query { terms }) = &o.stages[0] else { panic!() };
    assert_eq!(terms[0].pred, Pred::Size(Cmp::Ge, 1_572_864));
    assert!(terms[1].negated);
    assert_eq!(terms[2].pred, Pred::Name(b"*.mkv".to_vec()));
    assert_eq!(terms[3].pred, Pred::Kind(KindSel::File));
}

#[test]
fn m_is_minutes_not_months() {
    assert_eq!(q("sel age<90m").stages[0],
        Stage::Sel(Query { terms: vec![Term { negated: false, pred: Pred::Age(Cmp::Lt, 5400) }] }));
}

#[test]
fn escapes_reach_the_bytes() {
    assert_eq!(q(r"cp >/mnt/a\ b\|c\xff").stages[0],
        Stage::Cp(Dest::Path(b"/mnt/a b|c\xff".to_vec())));
    assert_eq!(q("mkdir 'two words'").stages[0], Stage::Mkdir(b"two words".to_vec()));
}

#[test]
fn errors_carry_their_offset() {
    assert_eq!(parse("sel ext:pdf | frobnicate").unwrap_err(),
        ParseError { offset: 14, kind: ParseErrorKind::UnknownVerb });
    assert_eq!(parse("sel size>12Q").unwrap_err().kind, ParseErrorKind::BadNumber);
    assert_eq!(parse("cp").unwrap_err().kind, ParseErrorKind::MissingDest);
    assert_eq!(parse("  ").unwrap_err().kind, ParseErrorKind::Empty);
    assert_eq!(parse(r"cp >a\x4").unwrap_err().kind, ParseErrorKind::BadEscape);
}

#[test]
fn redo_is_refused_until_the_journal_has_one() {
    assert_eq!(parse("redo").unwrap_err().kind, ParseErrorKind::NotYet);
}

#[test]
fn log_and_undo_stand_alone() {
    assert_eq!(q("undo 3").stages, vec![Stage::Undo(Some(3))]);
    assert_eq!(q("log").stages, vec![Stage::Log(None)]);
    assert_eq!(parse("sel *.rs | undo").unwrap_err().kind, ParseErrorKind::StandsAlone);
}

#[test]
fn a_dotted_word_is_a_catalogue_command_without_args() {
    assert_eq!(q("pane.swap").stages, vec![Stage::Catalogue("pane.swap".into())]);
}

#[test]
fn a_first_stage_that_is_not_sel_consumes_the_marks() {
    assert_eq!(q("trash").stages, vec![Stage::Trash]);
}
```

`tests/roundtrip.rs` — proptest over hostile names:

```rust
use norte_orders::*;
use proptest::prelude::*;

fn arb_order() -> impl Strategy<Value = Order> {
    let name = norte_testkit::strategies::arb_hostile_filename();
    let dest = prop_oneof![Just(Dest::Other), Just(Dest::Here), name.clone().prop_map(Dest::Path)];
    let pred = prop_oneof![
        name.clone().prop_map(Pred::Name),
        prop::collection::vec(name.clone(), 1..3).prop_map(Pred::Ext),
        (0u64..1 << 40).prop_map(|n| Pred::Size(Cmp::Gt, n)),
        (0u64..1 << 30).prop_map(|n| Pred::Age(Cmp::Lt, n)),
    ];
    let term = (any::<bool>(), pred).prop_map(|(negated, pred)| Term { negated, pred });
    let sel = prop::collection::vec(term, 1..4).prop_map(|terms| Stage::Sel(Query { terms }));
    let act = prop_oneof![dest.clone().prop_map(Stage::Cp), dest.prop_map(Stage::Mv),
        Just(Stage::Trash), name.prop_map(Stage::Mkdir)];
    (sel, act).prop_map(|(s, a)| Order { stages: vec![s, a] })
}

proptest! {
    #[test]
    fn print_then_parse_is_identity(o in arb_order()) {
        let line = print(&o);
        prop_assert_eq!(parse(&line), Ok(o));
    }

    #[test]
    fn printed_lines_carry_no_terminal_hazard(o in arb_order()) {
        prop_assert!(print(&o).chars().all(|c| !c.is_control()
            && !('\u{202A}'..='\u{202E}').contains(&c)
            && !('\u{2066}'..='\u{2069}').contains(&c)));
    }
}
```

Note for the implementer: `Pred::Ext` with an empty or comma-bearing
element and `Pred::Size`/`Age` values not representable in the printer's
chosen unit must still round-trip — print sizes and ages in base units
(`size>1572864`, `age<5400s`) unless exactly divisible. If the strategy
produces an `Ext` element containing `,`, escape it as `\x2c`.

`tests/dependency_boundary.rs`: copy `crates/norte-client/tests/dependency_boundary.rs`
with `FORBIDDEN` = every workspace crate name starting with `norte-` other
than `norte-orders` itself (read them from the same `cargo metadata`), and
the test name `the_grammar_reaches_no_other_norte_crate`.

- [ ] **Step 2:** `just t norte-orders` → FAIL (crate empty).
- [ ] **Step 3:** Implement lexer (words, quotes, escapes, `|`, `!`), parser (one stage per pipe segment; `sel` terms; `>` dest), printer, `tokens`. Doctests on every public item.
- [ ] **Step 4:** `just t norte-orders` → PASS; `cargo test -p norte-orders --doc`; `just c`.
- [ ] **Step 5: Commit** `feat(orders): the order grammar, a pure crate`

---

### Task 2: `norte-frontend::orders` — evaluate against the pane

**Files:**
- Create: `crates/norte-frontend/src/orders.rs`
- Modify: `crates/norte-frontend/src/pane/marks.rs:944-1003` (extract the matcher build), `crates/norte-frontend/src/lib.rs`, `crates/norte-frontend/Cargo.toml` (add `norte-orders`)
- Test: inline `#[cfg(test)] mod tests` in `orders.rs`, following `marks.rs`

**Interfaces:**
- Consumes: `norte_orders::{Order, Stage, Query, Pred, Dest}` (Task 1).
- Produces:

```rust
// pane/marks.rs — extracted, mark_glob now calls it
pub(crate) struct NameMatcher { /* regex over the folded name */ }
impl NameMatcher {
    pub(crate) fn new(pattern: &[u8]) -> Result<Self, PatternError>;
    pub(crate) fn is_match(&self, name: &[u8], enc: Option<norte_encoding::NameEncoding>) -> bool;
}

// pane/marks.rs — new
impl Pane {
    /// Replaces the marks with the rows matching `query`; returns how many.
    /// Snapshots first, like `mark_glob`, so `restore_previous_marks` undoes it.
    pub fn select_query(&mut self, query: &norte_orders::Query, now_ms: i64)
        -> Result<usize, PatternError>;
}

// orders.rs
pub fn matches(entry: &norte_proto::Entry, q: &CompiledQuery, now_ms: i64) -> bool;
pub struct CompiledQuery { /* terms with compiled NameMatchers */ }
impl CompiledQuery { pub fn new(q: &norte_orders::Query, enc: Option<NameEncoding>) -> Result<Self, PatternError>; }

/// What the frontend must do, in order. Paths are resolved here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Select(norte_orders::Query),
    Copy { to: VPath },
    Move { to: VPath },
    Trash,
    Mkdir { path: VPath },
    Cd { to: VPath },
    Log(u32),
    Undo(u32),
    Command(String),
}
pub struct Context<'a> { pub here: &'a VPath, pub other: Option<&'a VPath> }
pub fn steps(order: &norte_orders::Order, cx: &Context<'_>) -> Result<Vec<Step>, OrderError>;
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OrderError {
    #[error("no other pane")] NoOtherPane,
    #[error("bad path")] BadPath,
    #[error("unknown command")] UnknownCommand { nearest: Option<String> },
}
pub const LOG_DEFAULT: u32 = 20;
```

`steps` resolves `Dest::Path` relative to `here` when not absolute, via
`VPath::join_all` over `Segment`s built from the bytes split on `/`, or
`VPath::parse` for an absolute/URL form (no `to_str`). `Stage::Catalogue(id)` is
checked against `keymap::catalogue` and `nearest` comes from
`crate::fuzzy`. `Log(None)` → `Log(LOG_DEFAULT)`, `Undo(None)` → `Undo(1)`.

Semantics of a mutation step after a `Select` that matched zero rows: the
FRONTEND must not run it. `select_query` returns the count; executors stop
when it is 0 and report `msg-order-nothing-matched`.

- [ ] **Step 1: Failing tests** (in `orders.rs`, build panes the way `marks.rs` tests do):

```rust
#[test]
fn sel_marks_by_extension_size_and_age() { /* 3 entries: a.pdf 2MB new, b.pdf 10B old, c.txt; `sel ext:pdf size>1M` marks only a.pdf */ }

#[test]
fn ext_compares_folded_like_mark_glob() { /* `ext:pdf` marks `X.PDF` and the NFD form of `é.pdf` */ }

#[test]
fn sel_and_the_glob_modal_agree_on_every_hostile_name() {
    // For every norte_testkit::corpus::hostile_names(): mark_glob("*") and
    // select_query(sel *) mark the SAME set.
}

#[test]
fn zero_matches_never_falls_back_to_the_cursor() {
    // cursor on a.txt; select_query(sel ext:pdf) == 0; marks_len()==0 and the
    // caller contract: steps after it are not run — assert via a helper
    // `runnable(&steps, matched)` that returns the empty slice.
}

#[test]
fn a_directory_never_matches_size_without_type_dir() {}

#[test]
fn steps_resolve_other_here_and_relative_paths() {}

#[test]
fn other_without_a_second_pane_is_an_error() {}

#[test]
fn an_unknown_catalogue_id_suggests_the_nearest() { /* pane.swp → pane.swap */ }

#[test]
fn select_query_is_undone_by_restore_previous_marks() {}
```

Add the helper to the public API — executors in Tasks 4/5/8 call it:

```rust
/// The steps that may run once `Select` reported `matched` rows: none of the
/// mutations when it matched zero (the cursor fallback must not fire).
pub fn runnable(steps: &[Step], matched: Option<usize>) -> &[Step];
```

- [ ] **Step 2:** `just t norte-frontend` → FAIL.
- [ ] **Step 3:** Extract `NameMatcher` (mark_glob keeps its exact behaviour — its existing tests are the proof), implement `CompiledQuery`, `select_query`, `steps`, `runnable`.
- [ ] **Step 4:** `just t norte-frontend` → PASS; `cargo test -p norte-frontend --doc`; `just c`.
- [ ] **Step 5: Commit** `feat(frontend): evaluate an order against the pane`

---

### Task 3: the `:` mode in the omnibox model

**Files:**
- Modify: `crates/norte-frontend/src/navigation/goto.rs` (Mode, prefix, `needle`, `Action`, `confirm`, `refresh`)
- Modify: `crates/norte-i18n/i18n/{en,es}/*.ftl` (the file that holds `goto-*` keys)

**Interfaces:**
- Consumes: `norte_orders::{parse, tokens, ParseError}`, `crate::orders::{steps, Step}`.
- Produces:

```rust
pub const PREFIX_ORDER: &str = ":";
pub enum Mode { Places, Commands, Help, Order }      // as_str: "order"
pub enum Action { /* existing … */ Order(norte_orders::Order) }
impl Goto {
    /// The parsed order in Order mode, or the error with its offset.
    pub fn order(&self) -> Option<Result<norte_orders::Order, norte_orders::ParseError>>;
    /// Colour spans over `query_display()` (offsets shifted past the prefix).
    pub fn order_tokens(&self) -> Vec<(std::ops::Range<usize>, norte_orders::TokenClass)>;
}
```

In Order mode the row list is the COMPLETION list: when the cursor word is
the first word of a stage, rows are `VERBS` + catalogue ids matching it
(`cmd:` keys select-and-type via `Action::SetQuery`); when it is a `sel`
term, rows are the keys `ext: name: size> size< age> age< type:`. Enter with
no row chosen, or with a valid order, returns `Action::Order`. A parse error
returns `Action::Nothing("msg-order-parse")` and the box stays open (the
frontends already keep it open on `Nothing`? — check `keys.rs:605-726`; if
not, Task 4 adds it).

The `?` help list gains a prefix row for `:` (`goto-prefix-order`).

- [ ] **Step 1: Failing tests** in `goto.rs`'s test module:

```rust
#[test] fn colon_selects_order_mode() { assert_eq!(Mode::of(":sel *"), Mode::Order); }
#[test] fn needle_strips_the_colon() { assert_eq!(needle(": sel"), "sel"); }
#[test] fn a_valid_order_confirms_as_order() {}
#[test] fn a_parse_error_keeps_the_box_open_with_its_offset() {}
#[test] fn the_first_word_completes_verbs_and_catalogue_ids() {}
#[test] fn a_sel_term_completes_query_keys() {}
#[test] fn tokens_are_offset_past_the_prefix() {}
#[test]
fn an_order_line_with_a_control_byte_paints_masked() {
    // query ":mkdir a\u{1b}b" → query_display has U+FFFD, tokens still align
}
```

- [ ] **Step 2–4:** red, implement, `just t norte-frontend` green, `cargo test -p norte-frontend --doc`.
- [ ] **Step 5: Commit** `feat(frontend): the omnibox takes an order after ':'`

---

### Task 4: TUI — live preview and execution

**Files:**
- Modify: `crates/norte-tui/src/keys.rs:595-730` (goto key handling), `crates/norte-tui/src/jobs/goto.rs` (per-keystroke hook), the TUI's omnibox painter (find with `rg -n "fn .*goto" crates/norte-tui/src/ui*`), `crates/norte-tui/src/mutations.rs` (call sites only)
- Create: `crates/norte-tui/src/orders.rs` (the executor, kept out of `keys.rs`)
- Test: `crates/norte-tui/tests/orders.rs` (use the harness the existing `tests/*.rs` use for key-driven scenarios)

**Interfaces:**
- Consumes: `Goto::order`, `Goto::order_tokens`, `orders::{steps, runnable, Step, Context}`, `Pane::select_query`, `Pane::restore_previous_marks`, `mutations::{submit_transfers, submit_deletes}`, `backend.journal_list`, `backend.undo_after`.
- Produces: `pub async fn run(app: &mut App, backend: &Backend, events: …, order: Order)`.

Behaviour:
- **Live preview:** after every keystroke in Order mode, if the order parses and its first stage is `Sel`, restore the marks snapshot taken when the box opened and apply `select_query`; paint `N files · SIZE` (`msg-order-preview`, Fluent plural) under the line. Parse error: underline from the offset (`TokenClass::Error`) and the translated kind.
- **Esc:** restore the snapshot taken when the box OPENED (not the last preview's) — Review Focus 2.
- **Enter:** run `steps`, through `runnable`; `Copy`/`Move` go through the SAME function F5/F6 call before `submit_transfers` (the one that validates destination: same dir, read-only, archive) — find it by reading what `pane.copy`'s dispatch arm calls. `Trash` → the F8 path with `permanent = false`. `Mkdir` → the F7 path. `Cd` → `navigate::cd`. `Command(id)` → the existing `Action::Command` arm. `Log`/`Undo` → Task 6.

- [ ] **Step 1: Failing tests:**

```rust
#[tokio::test] async fn typing_sel_lights_the_rows_before_enter() {}
#[tokio::test] async fn esc_restores_the_marks_from_before_the_box() {}   // incl. empty set
#[tokio::test] async fn enter_copies_the_matched_rows_to_the_other_pane() {}
#[tokio::test] async fn order_copy_reuses_the_f5_guard() {}             // other pane == here → same refusal F5 gives
#[tokio::test] async fn zero_matches_copies_nothing() {}
#[tokio::test] async fn a_catalogue_stage_runs_like_its_key() {}
```

- [ ] **Step 2–4:** red → implement → `just t norte-tui` green.
- [ ] **Step 5:** `just ci-fast` (first of two gate runs). Fix any red with `just t <crate>`, not by re-running the gate.
- [ ] **Step 6: Commit** `feat(tui): run an order from the box`

---

### Task 5: window — ui-host and webview

**Files:**
- Modify: `crates/norte-ui-host/src/controller/goto.rs:400-500`, `crates/norte-ui-host/src/controller/transfer.rs` (call site only)
- Create: `crates/norte-ui-host/src/controller/orders.rs`
- Modify: the bridge snapshot of the omnibox (find with `rg -n "goto" crates/norte-ui-host/src/bridge*`) to carry `mode: "order"`, `tokens`, `preview`, `error`
- Modify: the webview omnibox component under `crates/norte-gui-tauri/ui/src` (find with `rg -l "goto" crates/norte-gui-tauri/ui/src`): token colours as CSS classes fed by the theme (no new CSS variables without a theme value — gui-ci checks it)
- Test: `crates/norte-ui-host/tests/orders.rs` with `backend_fake`

Same behaviour as Task 4, same order of guards: live preview, Esc restores
the box-open snapshot, Enter runs `runnable(steps)`, copy/move go through the
function that `pane.copy` uses before `launch_transfer`.

- [ ] **Step 1: Failing tests** — the four from Task 4 that are frontend-agnostic, plus parity:

```rust
#[tokio::test] async fn same_order_same_marks_as_the_tui() {
    // Same MemProvider tree, same line; the set of marked paths equals what
    // norte_frontend::orders evaluation gives (the shared function is the oracle).
}
```

- [ ] **Step 2–4:** red → implement → `just t norte-ui-host` green; `just gui-ci`.
- [ ] **Step 5: Commit** `feat(gui): run an order from the box`

---

### Task 6: `log` and `undo N`

**Files:**
- Modify: `crates/norte-frontend/src/orders.rs` (pure helpers), `crates/norte-tui/src/orders.rs`, `crates/norte-ui-host/src/controller/orders.rs`
- Modify: i18n both locales

**Interfaces:**
- Produces (pure, in `norte-frontend::orders`):

```rust
/// From the newest-first human entries just listed, the `journal.undo_after`
/// arguments that undo exactly the newest `n`: `seq` of the (n+1)-th entry
/// (or 0 when fewer exist), and `upto_seq` = the newest seq — so entries
/// that land after the listing are NOT undone (Review Focus 5).
pub fn undo_bounds(newest_first: &[norte_proto::methods::JournalRow], n: u32) -> Option<(i64, i64)>;
/// One painted row per entry: `● copy 14 → /mnt/b  10:42`.
pub fn log_rows(entries: &[norte_proto::methods::JournalRow], lang: norte_i18n::Lang) -> Vec<crate::goto::GotoRow>;
```

`log` keeps the box open listing `log_rows`; Enter on a row opens the
timeline panel on it (the existing timeline command). `undo N` calls
`journal_list(None, N+1, Some("user"))`, then `undo_after(seq, Some(upto))`,
and confirms through the SAME modal the timeline uses for "undo after"
(`Modal::ConfirmUndoAfter` in the TUI; its ui-host twin) so the count shown
is the count undone.

- [ ] **Step 1: Failing tests** (pure, in `orders.rs`):

```rust
#[test] fn undo_one_points_at_the_second_newest() {}
#[test] fn undo_more_than_exist_points_at_zero() {}
#[test] fn the_ceiling_is_the_newest_listed_seq() {}
#[test] fn undo_zero_is_none() {}
```

Plus one TUI test: `undo 2` opens the confirm modal with count 2.

- [ ] **Step 2–4:** red → implement → `just t norte-frontend`, `just t norte-tui`, `just t norte-ui-host`.
- [ ] **Step 5: Commit** `feat(frontend): log and undo as orders`

---

### Task 7: the `app.orders` command and the `:` key, all seven presets

Use the `new-command` skill — it carries the catalogue / dispatch / presets /
i18n / help / golden checklist.

**Files:** `crates/norte-frontend/src/keymap/catalogue.rs`, `crates/norte-frontend/presets/keymap/*.toml` (7), both dispatchers (`crates/norte-tui/src/dispatch.rs` next to `AppGoto` at ~996; ui-host twin), i18n `help-cmd-app-orders` in both locales, the help topic for the omnibox, `norte-cli` golden (`NORTE_UPDATE_GOLDEN=1`).

- `app.orders` opens the omnibox with `:` typed (`open_omnibox(app, backend, PREFIX_ORDER)`).
- Bind `:` in browse in `vim` (`:` is its native command line). For each other preset: bind `:` if it is free in `browse`, else bind `app.orders` on a free, deliverable chord and write the reason `:` is not used in the header's omissions block. The four imported presets are transcriptions: if the source manager has no such feature, say so in the header and bind only where a free chord exists without inventing a clash.
- Check `:` is not swallowed by quick-search/type-initial in `krusader` (ADR 0155).

- [ ] **Step 1:** the keymap guard test that every catalogue command is bound or excused in each preset fails for `app.orders`.
- [ ] **Step 2:** bind/excuse in all seven; golden update.
- [ ] **Step 3:** `just t norte-frontend`, `just t norte-cli`, `just t norte-tui`.
- [ ] **Step 4: Commit** `feat(keymap): ':' opens the order box`

---

### Task 8: `ntc run`

**Files:**
- Create: `crates/norte-cli/src/cmd/run.rs`
- Modify: `crates/norte-cli/src/main.rs` (`Cmd::Run`), `crates/norte-cli/src/cmd/mod.rs`
- Test: `crates/norte-cli/tests/run.rs` (golden style of `tests/smoke.rs`)

```rust
Run {
    /// The order, e.g. 'sel ext:pdf | cp >/mnt/backup'.
    order: String,
    /// Folder that `sel` and relative paths resolve against.
    #[arg(long, default_value = ".")]
    cwd: PathBuf,
    /// Print what would run, run nothing.
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    json: bool,
},
```

Executor: `norte_orders::parse` → `orders::steps` with `other: None` (so
`>other` is `OrderError::NoOtherPane`, exit code 2 with the message) → for
`Select`, `backend.list(cwd)` and filter with `orders::matches` (the same
`CompiledQuery` the panes use) → `Copy`/`Move`/`Trash`/`Mkdir` call the SAME
helpers `Cmd::Cp`/`Mv`/`Rm`/`Mkdir` call, one task per item, waiting each as
they do. `Log`/`Undo`: `Log` prints; `Undo` refuses without `--yes`
(non-interactive). `Command(_)` → error "catalogue commands need a pane".
Zero matches → print `0 files`, exit 0, run nothing.

- [ ] **Step 1: Failing tests:**

```rust
#[test] fn dry_run_lists_the_matches_and_touches_nothing() {}
#[test] fn run_copies_the_matches() {}
#[test] fn other_is_refused_without_a_pane() {}
#[test] fn json_output_is_stable() {}      // golden
#[test] fn a_non_utf8_name_survives_sel_and_cp() {}  // corpus fixture
```

- [ ] **Step 2–4:** red → implement → `just t norte-cli` green.
- [ ] **Step 5:** `just ci-fast` (second and last mid-plan gate run).
- [ ] **Step 6: Commit** `feat(cli): ntc run executes an order`

---

### Task 9: ADR, spec amendment, changelog, reviews

- ADR via `/adr`: "An order is a small grammar in a pure crate, resolved against the loaded pane" — why not shell, why not per-operation tool calls, why the pane's listing and not a new `orders.plan` wire (cost, F5 guards reused, live preview without a socket), what it gives up (non-recursive `sel`, no stale-tree hash).
- Spec: amend §3 to point at the ADR; note `redo` dropped until the journal has a redo.
- `CHANGELOG.md`: the `:` box, `log`/`undo N`, `ntc run`.
- Reviewers before the final commit, in parallel, with the commit range and these questions:
  - `encoding-auditor`: escape/print round trip, fold agreement between `sel` and `mark_glob`, hazards in painted order lines.
  - `rust-reviewer`: the extracted `NameMatcher` keeps `mark_glob`'s behaviour; executor error paths; no `to_str().unwrap()`.
  - `security-reviewer`: is there any path where an order mutates without the F5/F6/F8 guard, or `undo` exceeds what was shown?
- Apply BLOCKER/MAJOR in one pass, `just t` the touched crates, commit `docs: ADR and changelog for the order box`.
- Merge: one branch `feat/orders-box`, merged once; push `main` (the hook is the gate run). Then `just link` + `just link-gui`.
