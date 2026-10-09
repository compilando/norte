# Terminal instances Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The terminal panel holds several shells, VS Code style — new, close, next/prev, rename, icon/colour, shell profiles — in the TUI and the window.

**Architecture:** One `terminal` slot (unchanged in the layout tree) owns a `Terminals<S>` list from a new pure module in `norte-frontend`. Both frontends replace their `Option<Shell>` with it. `norte-term` learns the exit code and the OSC 0/2 title. Shell profiles come from a new `terminal.toml` (no project layer). The bridge view gains the instance list; `norte-proto` is untouched.

**Tech Stack:** Rust (`norte-term`, `norte-frontend`, `norte-ui-host`, `norte-tui`), `vte`, `portable-pty`, ratatui, TypeScript webview (`ui/src/render/terminal.ts`).

**Spec:** `docs/superpowers/specs/2026-10-09-terminal-instances-design.md`

## Global Constraints

- `norte-proto` NOT touched. `BRIDGE_VERSION` (`crates/norte-ui-host/src/bridge.rs:598`, now 105) bumps ONCE, in Task 4.
- Split groups, persistence across sessions, cwd following, remote shells: out of scope.
- "Shell profile" in code, strings and docs — never bare "profile" (collides with configuration profiles).
- `terminal.toml` read from system, user and configuration-profile layers; **never** `./.norte`. `program` must be absolute.
- Title precedence: `name` > OSC title > profile name. OSC title: C0/C1/DEL and bidi controls stripped, lossy UTF-8, ≤128 chars.
- Exit 0 removes the instance; exit ≠0 (or signal) keeps it with its last screen.
- Every chord taken from inside the panel is in PASS_THROUGH; never `ctrl+<letter>`, `alt+<letter>`, `shift+<char>`.
- Nothing journalled; one `tracing::info!` per started instance naming the shell profile, not the command line.
- Bash cannot write files (hook); use Edit/Write. `rg`, not `grep`. Commits with `git commit -F <file>`.
- Loop per task: `just t <crate>` (+ `just c` if lint surface). `just ci-fast` ONCE after Task 4 and the push of `main` at the end. `just gui-ci` before pushing (Task 7 touches `ui/`). Never `sleep`, never `tail -f /dev/null`; nothing will notify you.
- Struct gained a public field → `cargo test -p <crate> --doc`. Wrote a doc link → `cargo doc -p <crate> --no-deps`.

## Review Focus

1. **Active instance exits while the keyboard is inside the panel.** With others left, focus stays in the panel on the neighbour; with none left, the TUI releases the keyboard (as today) and the window keeps the slot saying "no shell". Test in Task 4 and Task 6.
2. **A hostile title** (`printf '\e]0;‮evil\a'`, 10 KB title, title with `\x1b`) — shown stripped and capped, never obeyed. Test in Task 1.
3. **A noisy inactive shell** (`yes` in instance 2 while 1 is active) — instance 2 is pumped every tick (its screen stays coherent), marked `unseen`, and the bridge publishes nothing for it. Test in Task 4.
4. **`layout.close-slot` with three live shells** kills all three and the tick does not rearm. Test in Task 4.
5. **A shell profile whose program does not exist** — `terminal.new-profile` says `host-shell-failed`, adds no instance, leaves the others untouched. Test in Task 4.

---

### Task 1: `norte-term` — exit code and OSC title

**Files:**
- Modify: `crates/norte-term/src/lib.rs` (`Grid`, `impl vte::Perform for Grid` ~l.542, `Screen` ~l.728)
- Modify: `crates/norte-term/src/pty.rs` (`Shell` ~l.141)

**Interfaces:**
- Produces: `Screen::take_title(&mut self) -> Option<String>` (returns and clears the last OSC 0/2 seen since the previous call); `Shell::take_title(&mut self) -> Option<String>` (delegates); `Shell::exit_code(&mut self) -> Option<i32>` (`None` while alive; `Some(code)` once exited; a signal death is `Some(non-zero)`).

- [ ] **Step 1: Failing tests** in `lib.rs`'s `mod tests`:

```rust
/// OSC 0 and OSC 2 set the title; the last one wins and is taken once.
#[test]
fn osc_0_and_2_set_the_title() {
    let mut p = Screen::new(10, 1);
    p.alimentar(b"\x1b]0;one\x07\x1b]2;two\x1b\\");
    assert_eq!(p.take_title().as_deref(), Some("two"));
    assert_eq!(p.take_title(), None, "taken, not peeked");
}

/// The title is FOREIGN text: a `cat` of a file can set it. Control bytes
/// and bidi overrides never survive, and it is capped.
#[test]
fn a_hostile_title_is_stripped_and_capped() {
    let mut p = Screen::new(10, 1);
    p.alimentar("\x1b]0;a\u{202e}b\u{2066}c\u{9b}d\x07".as_bytes());
    assert_eq!(p.take_title().as_deref(), Some("abcd"));
    let long = format!("\x1b]2;{}\x07", "x".repeat(10_000));
    p.alimentar(long.as_bytes());
    assert_eq!(p.take_title().map(|t| t.chars().count()), Some(128));
}

/// Other OSCs (norte's 777 marker, OSC 7) do not touch the title.
#[test]
fn other_oscs_are_not_titles() {
    let mut p = Screen::new(10, 1);
    p.alimentar(b"\x1b]7;file:///tmp\x07\x1b]777;norte-cwd;x\x07");
    assert_eq!(p.take_title(), None);
}

/// A title in invalid UTF-8 is shown lossily, not dropped (rule 1: display).
#[test]
fn a_non_utf8_title_is_lossy() {
    let mut p = Screen::new(10, 1);
    p.alimentar(b"\x1b]0;a\xffb\x07");
    assert_eq!(p.take_title().as_deref(), Some("a\u{fffd}b"));
}
```

In `pty.rs` tests (`#[cfg(unix)]`, they already spawn real shells — follow the existing ones there):

```rust
#[test]
fn exit_code_reports_zero_and_non_zero() {
    for (cmd, want) in [("exit 0", 0), ("exit 3", 3)] {
        let mut s = open_sh(); // the existing test helper that opens /bin/sh
        s.write(format!("{cmd}\n").as_bytes());
        let code = wait_until(|| s.exit_code()); // existing polling helper, deadline-bounded
        assert_eq!(code, Some(want), "{cmd}");
    }
}

#[test]
fn a_signal_death_is_non_zero() {
    let mut s = open_sh();
    s.write(b"kill -9 $$\n");
    let code = wait_until(|| s.exit_code());
    assert!(matches!(code, Some(c) if c != 0));
}
```

If `pty.rs` has no `open_sh`/`wait_until` helpers, write them in the test module: `wait_until` loops `pump()` + the probe against a 5 s `Instant` deadline with `std::thread::yield_now()` — NO `sleep`.

- [ ] **Step 2:** `just t norte-term` → the new tests fail to compile.
- [ ] **Step 3: Implement.** In `Grid` add `title: Option<String>`. Implement `osc_dispatch(&mut self, params: &[&[u8]], _bell: bool)`: when `params[0]` is `b"0"` or `b"2"`, join `params[1..]` with `;` (a title may contain `;`), `String::from_utf8_lossy`, filter out `char::is_control` and `'\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'`, take 128 chars, store. Do NOT change any other OSC handling (`an_osc_paints_nothing` must stay green). `Screen::take_title` = `self.grid.title.take()`. `Shell::exit_code`: `self.child.try_wait()` → `Ok(Some(st))` → `Some(i32::try_from(st.exit_code()).unwrap_or(i32::MAX))`; `portable_pty::ExitStatus::exit_code()` already folds a signal into a non-zero `u32` — verify in the test, do not assume. Keep `dead()`.
- [ ] **Step 4:** `just t norte-term` green; `cargo test -p norte-term --doc`.
- [ ] **Step 5:** Commit `feat(term): exit code and OSC 0/2 title`.

---

### Task 2: `norte-frontend::terminals` — the model

**Files:**
- Create: `crates/norte-frontend/src/terminals.rs`
- Modify: `crates/norte-frontend/src/lib.rs` (add `pub mod terminals;`)

**Interfaces:**
- Produces (all `pub`, documented — this crate warns on missing docs):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InstanceId(pub u32);

/// 1..=6, the ANSI index the THEME resolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnsiColor(u8);
impl AnsiColor { pub fn new(i: u8) -> Option<Self>; pub fn index(self) -> u8; }

/// The fixed icon set. `as_str`/`parse` are the TOML and bridge spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalIcon { Terminal, Code, Server, Debug, Package, Star }
impl TerminalIcon { pub const ALL: [Self; 6]; pub fn as_str(self) -> &'static str; pub fn parse(s: &str) -> Option<Self>; }

pub struct Instance<S> {
    pub id: InstanceId,
    pub profile: String,
    pub name: Option<String>,
    pub title: Option<String>,
    pub icon: Option<TerminalIcon>,
    pub color: Option<AnsiColor>,
    pub exited: Option<i32>,
    pub unseen: bool,
    pub shell: Option<S>,
}

pub struct Terminals<S> { /* instances, active: Option<InstanceId>, next_id: u32 */ }

impl<S> Terminals<S> {
    pub fn new() -> Self;
    pub fn is_empty(&self) -> bool;
    pub fn len(&self) -> usize;
    pub fn iter(&self) -> impl Iterator<Item = &Instance<S>>;
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Instance<S>>;
    pub fn active(&self) -> Option<&Instance<S>>;
    pub fn active_mut(&mut self) -> Option<&mut Instance<S>>;
    pub fn active_id(&self) -> Option<InstanceId>;
    /// Appends at the end and makes it active.
    pub fn push(&mut self, profile: String, icon: Option<TerminalIcon>, color: Option<AnsiColor>, shell: S) -> InstanceId;
    /// Removes it (dropping the shell kills it). Active moves right, else left.
    pub fn close(&mut self, id: InstanceId) -> Option<Instance<S>>;
    pub fn select(&mut self, id: InstanceId) -> bool;
    pub fn next(&mut self);
    pub fn prev(&mut self);
    /// Empty after trim clears the name.
    pub fn rename(&mut self, id: InstanceId, name: &str);
    pub fn decorate(&mut self, id: InstanceId, icon: Option<TerminalIcon>, color: Option<AnsiColor>);
    /// 0 removes; anything else keeps it with `exited` and drops the shell.
    pub fn on_exit(&mut self, id: InstanceId, code: i32);
    /// Marks `unseen` unless it is the active one.
    pub fn on_output(&mut self, id: InstanceId);
    pub fn set_title(&mut self, id: InstanceId, title: String);
    pub fn display_title(&self, id: InstanceId) -> Option<&str>;
    /// Drops every shell (closing the panel).
    pub fn clear(&mut self);
}
```

- [ ] **Step 1: Failing tests** (in the module, `S = &'static str` stands in for a shell):

```rust
fn three() -> (Terminals<&'static str>, [InstanceId; 3]) {
    let mut t = Terminals::new();
    let a = t.push("fish".into(), None, None, "a");
    let b = t.push("bash".into(), None, None, "b");
    let c = t.push("zsh".into(), None, None, "c");
    (t, [a, b, c])
}

#[test]
fn push_appends_and_activates() {
    let (t, [_, _, c]) = three();
    assert_eq!(t.active_id(), Some(c));
    assert_eq!(t.len(), 3);
}

#[test]
fn closing_the_active_one_moves_right_then_left() {
    let (mut t, [a, b, c]) = three();
    t.select(b);
    t.close(b);
    assert_eq!(t.active_id(), Some(c), "right neighbour first");
    t.close(c);
    assert_eq!(t.active_id(), Some(a), "then left");
    t.close(a);
    assert_eq!(t.active_id(), None);
    assert!(t.is_empty());
}

#[test]
fn closing_an_inactive_one_keeps_the_active() {
    let (mut t, [a, _, c]) = three();
    t.close(a);
    assert_eq!(t.active_id(), Some(c));
}

#[test]
fn next_and_prev_wrap() {
    let (mut t, [a, _, c]) = three();
    t.next();
    assert_eq!(t.active_id(), Some(a));
    t.prev();
    assert_eq!(t.active_id(), Some(c));
}

#[test]
fn exit_zero_removes_and_non_zero_stays_without_shell() {
    let (mut t, [a, b, _]) = three();
    t.on_exit(a, 0);
    assert!(t.iter().all(|i| i.id != a));
    t.on_exit(b, 3);
    let i = t.iter().find(|i| i.id == b).expect("stays");
    assert_eq!(i.exited, Some(3));
    assert!(i.shell.is_none());
}

#[test]
fn unseen_only_off_screen_and_select_clears_it() {
    let (mut t, [a, _, c]) = three();
    t.on_output(c);
    t.on_output(a);
    assert!(!t.active().expect("c").unseen, "the active one is being seen");
    assert!(t.iter().find(|i| i.id == a).expect("a").unseen);
    t.select(a);
    assert!(!t.active().expect("a").unseen);
}

#[test]
fn title_precedence_name_then_osc_then_profile() {
    let (mut t, [a, ..]) = three();
    assert_eq!(t.display_title(a), Some("fish"));
    t.set_title(a, "vim".into());
    assert_eq!(t.display_title(a), Some("vim"));
    t.rename(a, "build");
    assert_eq!(t.display_title(a), Some("build"));
    t.rename(a, "   ");
    assert_eq!(t.display_title(a), Some("vim"), "blank name clears it");
}

#[test]
fn ids_are_never_reused() {
    let (mut t, [_, _, c]) = three();
    t.close(c);
    let d = t.push("x".into(), None, None, "d");
    assert_ne!(d, c);
}

#[test]
fn colour_is_one_to_six_and_icons_round_trip() {
    assert!(AnsiColor::new(0).is_none() && AnsiColor::new(7).is_none());
    assert_eq!(AnsiColor::new(4).map(AnsiColor::index), Some(4));
    for i in TerminalIcon::ALL {
        assert_eq!(TerminalIcon::parse(i.as_str()), Some(i));
    }
}
```

- [ ] **Step 2:** `just t norte-frontend` → fails to compile.
- [ ] **Step 3:** Implement. `instances: Vec<Instance<S>>`, position lookups by id. `close` of the active: `let pos = …; remove(pos); active = instances.get(pos).or(instances.get(pos.wrapping_sub(1)))…`. One-line docs on private helpers.
- [ ] **Step 4:** `just t norte-frontend`; `cargo test -p norte-frontend --doc`; `just c`.
- [ ] **Step 5:** Commit `feat(frontend): terminal instance list`.

---

### Task 3: Shell profiles — `terminal.toml`

**Files:**
- Create: `crates/norte-frontend/src/shell_profiles.rs`
- Modify: `crates/norte-frontend/src/lib.rs`, `crates/norte-frontend/src/config.rs` (`FrontendConfig` gains `pub shell_profiles: ShellProfiles`, loaded like `openers` — read how `openers` is loaded per layer in `load` and copy that layer filter, adding the configuration-profile layer, which `openers` may not include: check `Layer` variants)

**Interfaces:**
- Consumes: `TerminalIcon`, `AnsiColor` (Task 2); `norte_frontend::shell::login_shell`.
- Produces:

```rust
pub struct ShellProfile { pub name: String, pub program: PathBuf, pub args: Vec<OsString>, pub icon: Option<TerminalIcon>, pub color: Option<AnsiColor> }
pub struct ShellProfiles { /* ordered, default index */ }
impl ShellProfiles {
    /// The implicit single profile from `login_shell()`, named after its file name (lossy).
    pub fn implicit() -> Self;
    pub fn parse(s: &str) -> Result<ShellProfilesFile, ShellProfileError>;
    /// Higher layer wins on equal `name`; `default` from the highest layer that sets it.
    pub fn merge(files: Vec<ShellProfilesFile>) -> Result<Self, ShellProfileError>;
    pub fn default_profile(&self) -> &ShellProfile;
    pub fn get(&self, name: &str) -> Option<&ShellProfile>;
    pub fn iter(&self) -> impl Iterator<Item = &ShellProfile>;
}
#[derive(Debug, thiserror::Error)]
pub enum ShellProfileError { Toml(String), RelativeProgram { name: String }, UnknownDefault { name: String }, BadColor { name: String, value: u8 }, BadIcon { name: String, value: String } }
```

TOML shape (`deny_unknown_fields`; `program` and `args` as strings — on unix convert with `OsString::from`, it is config text, the bytes rule concerns filenames read from disk):

```toml
default = "fish"
[[shell]]
name = "fish"
program = "/usr/bin/fish"
args = ["-l"]
icon = "terminal"
color = 4
```

- [ ] **Step 1: Failing tests:**

```rust
#[test]
fn a_relative_program_is_refused() {
    let e = ShellProfiles::parse("[[shell]]\nname = \"x\"\nprogram = \"fish\"\n").unwrap_err();
    assert!(matches!(e, ShellProfileError::RelativeProgram { .. }));
}

#[test]
fn an_unknown_default_is_refused_not_ignored() {
    let f = ShellProfiles::parse("default = \"nope\"\n[[shell]]\nname = \"a\"\nprogram = \"/bin/sh\"\n").expect("parses");
    assert!(matches!(ShellProfiles::merge(vec![f]), Err(ShellProfileError::UnknownDefault { .. })));
}

#[test]
fn colour_and_icon_are_checked() {
    assert!(ShellProfiles::parse("[[shell]]\nname = \"a\"\nprogram = \"/bin/sh\"\ncolor = 9\n").is_err());
    assert!(ShellProfiles::parse("[[shell]]\nname = \"a\"\nprogram = \"/bin/sh\"\nicon = \"rocket\"\n").is_err());
}

#[test]
fn the_higher_layer_wins_on_equal_name() {
    let low = ShellProfiles::parse("[[shell]]\nname = \"a\"\nprogram = \"/bin/sh\"\n").expect("low");
    let high = ShellProfiles::parse("[[shell]]\nname = \"a\"\nprogram = \"/bin/bash\"\n").expect("high");
    let m = ShellProfiles::merge(vec![low, high]).expect("merges"); // ascending precedence
    assert_eq!(m.get("a").expect("a").program, PathBuf::from("/bin/bash"));
}

#[test]
fn no_file_means_the_login_shell() {
    let m = ShellProfiles::implicit();
    assert_eq!(m.iter().count(), 1);
    assert!(m.default_profile().program.is_absolute());
}
```

And in `config.rs` tests (use the existing tempdir-based layer tests there as the model):

```rust
/// A repository must not choose programs norte runs (same rule as openers).
#[test]
fn a_project_terminal_toml_is_ignored() {
    // project layer dir with a terminal.toml naming /tmp/evil → not in the merged profiles
}
```

Write that last body by copying the existing `openers` project-layer test in this file and swapping the file name and assertion — it exists precisely for this rule; find it with `rg -n "project" crates/norte-frontend/src/config.rs`.

- [ ] **Step 2:** `just t norte-frontend` → fails.
- [ ] **Step 3:** Implement. A load error is surfaced the way an `openers.toml` error is (same `ConfigError` path) — not swallowed.
- [ ] **Step 4:** `just t norte-frontend`; `cargo test -p norte-frontend --doc`; `rg -c "FrontendConfig \{" --glob '*.rs'` and fix the literal sites; `cargo test --workspace --exclude norte-gui-tauri --doc`.
- [ ] **Step 5:** Dispatch `security-reviewer` on this commit range (question: "can any layer other than system/user/config-profile reach `program`? is a relative or empty program possible after merge?"). Do not wait idle: start Task 4. Commit `feat(frontend): shell profiles in terminal.toml` only after its findings are applied.

---

### Task 4: Window host — N shells, tick, bridge view

**Files:**
- Modify: `crates/norte-ui-host/src/controller/termpanel.rs` (whole module)
- Modify: `crates/norte-ui-host/src/controller/mod.rs:2986-2994,3463-3467` (field), `controller/tabs.rs:216,424`, `controller/views.rs:102`
- Modify: `crates/norte-ui-host/src/dto.rs` (`TerminalSlotView`), `crates/norte-ui-host/src/action.rs` (new `UiAction`s), `crates/norte-ui-host/src/bridge.rs:598` (`BRIDGE_VERSION` 105→106)
- Modify: `crates/norte-ui-host/tests/golden.rs` + `tests/golden/*.json`, `crates/norte-gui-tauri/ui/src/types.ts:612` (mirror the DTO only; painting is Task 7)
- Test: `crates/norte-ui-host/tests/controller/` (new file `terminals.rs`, registered like its siblings)

**Interfaces:**
- Consumes: Tasks 1–3.
- Produces:

```rust
// dto.rs
pub struct TerminalInstanceView { pub id: u32, pub title: String, pub icon: Option<String>, pub color: Option<u8>, pub exited: Option<i32>, pub unseen: bool }
// TerminalSlotView gains:
pub instances: Vec<TerminalInstanceView>,
pub active: Option<u32>,
pub exited: Option<i32>, // the ACTIVE one's; rows/cursor are its last grid
pub profiles: Vec<String>, // shell profile names, default first, for the `▾` menu

// action.rs
TerminalSelect { id: u32 },
TerminalClose { id: u32 },
TerminalNew { profile: Option<String> },
TerminalRename { id: u32, name: String },
TerminalDecorate { id: u32, icon: Option<String>, color: Option<u8> },

// State (controller)
terminals: norte_frontend::terminals::Terminals<norte_term::pty::Shell>,
pub(super) fn new_terminal(&mut self, profile: Option<&str>, mailbox: &mpsc::Sender<Message>) -> Vec<BridgeEnvelope<UiUpdate>>;
```

Behaviour to implement (spec §4):
- `start_si_missing` → starts the DEFAULT shell profile only when `terminals.is_empty()`.
- `new_terminal`: local-dir gate (`host-not-local`) as in `open_terminal_panel`; resolves the profile (`None` → default; unknown name → `ActionAck::Unavailable`); on spawn error says `host-shell-failed` and pushes nothing. Starts the tick if it was not running.
- `terminal_tic`: for EVERY instance: resize to `terminal_size()`, `pump()` → if bytes and not active, `on_output`; `take_title()` → `set_title`; `exit_code()` → `on_exit`. Republish if the active pumped bytes OR the list changed (titles, unseen, exits, removals). If the list becomes empty the tick still rearms only while the slot exists (no change in that rule).
- `release_terminal` → `terminals.clear()` + epoch bump.
- `key_in_terminal`/`terminal_write` → the ACTIVE instance; an exited active instance swallows keys except the pass-through ones.
- `PASS_THROUGH` gains `terminal.new`, `terminal.close`, `terminal.next`, `terminal.prev`, `terminal.new-profile`, `terminal.rename`, `terminal.decorate`.
- The `UiAction`s go through the same functions the commands will call in Task 5. Unknown ids → `ActionAck::Unavailable`, never a panic. Rename/decorate validate with `AnsiColor::new` / `TerminalIcon::parse`.

- [ ] **Step 1: Failing tests** in `tests/controller/terminals.rs`. Copy the harness the existing terminal-panel tests use (`rg -n "layout.terminal" crates/norte-ui-host/tests/controller` to find them; they run a real `/bin/sh` under `#[cfg(unix)]`). Each waits for state via the harness' existing deterministic waits (ADR on ui-host deterministic tests: no `sleep`). Tests:

```rust
#[tokio::test] async fn new_adds_a_second_instance_and_activates_it()      // view.instances.len()==2, active == second id
#[tokio::test] async fn an_inactive_noisy_shell_is_pumped_and_marked_unseen() // instance 1 runs `yes | head -c 400000`; select 2; instance 1 unseen==true; no ViewChange::Terminal patch carries instance 1's rows
#[tokio::test] async fn exit_zero_removes_exit_three_stays()             // `exit` in one → gone; `exit 3` in other → exited==Some(3), rows kept
#[tokio::test] async fn the_last_exit_leaves_no_shell()                  // no_shell==true, slot still in the tree
#[tokio::test] async fn close_slot_kills_every_shell_and_the_tick_stops() // 3 instances, layout.close-slot, then a stale TerminalTic(old epoch) produces no updates
#[tokio::test] async fn a_missing_program_adds_nothing()                  // config with [[shell]] program="/nonexistent/sh"; TerminalNew{profile:Some}; len unchanged; message key host-shell-failed
#[tokio::test] async fn a_pass_through_chord_does_not_reach_the_shell()   // focus inside, press terminal.next's chord, shell input unchanged (write a marker command after and check the grid)
#[tokio::test] async fn an_unknown_id_is_refused_not_panicked()           // TerminalSelect{id:999} → Unavailable
```

Plus golden: add `TerminalSelect`, `TerminalClose`, `TerminalNew`, `TerminalRename`, `TerminalDecorate` to the action name table (`tests/golden.rs:200-230`) and cases near l.461; extend the `TerminalSlotView` fixture with two instances.

- [ ] **Step 2:** `just t norte-ui-host` → fails.
- [ ] **Step 3:** Implement. Keep `termpanel.rs`'s existing comments that still hold; delete those about "the one shell" that no longer do.
- [ ] **Step 4:** `NORTE_UPDATE_GOLDEN=1 just t norte-ui-host`, read the golden diff (only terminal fields and the five actions), then `just t norte-ui-host`; `cargo test -p norte-ui-host --doc`; `just c`.
- [ ] **Step 5:** Dispatch `rust-reviewer` (question: "can the tick spin with an empty list? can an exited instance's grid leak into another's patch? is `Drop` of every `Shell` guaranteed on every close path?"). Start Task 5 meanwhile. Apply findings, commit `feat(ui-host): several shells in the terminal panel`.
- [ ] **Step 6:** `just ci-fast` — the plan's ONE mid-plan gate run (Tasks 1–4).

---

### Task 5: Commands, presets, strings, help (catalogue + window dispatch)

Follow `.claude/commands/new-command.md` steps 2–10 for each command; this task gives the decisions it asks for.

**Files:**
- Modify: `crates/norte-frontend/src/keymap/catalogue.rs` (rows), `crates/norte-frontend/src/availability.rs` (only-with-panel-open verdict), `crates/norte-frontend/src/chrome/menu.rs` (a "Terminal" section in the menu where `layout.terminal` lives)
- Modify: `crates/norte-frontend/presets/keymap/{orthodox,vim,cua,krusader,far,norton,total-commander}.toml`
- Modify: `crates/norte-ui-host/src/commands.rs` (`IMPLEMENTADOS`, `efecto_de`), `controller/termpanel.rs` (effects call Task 4's functions; rename and decorate open the existing input / picker modal — find with `rg -n "ViewChange::Picker" crates/norte-ui-host/src/controller/selectors.rs`)
- Modify: `crates/norte-i18n/i18n/{en,es}.ftl`, `crates/norte-help/topics/{en,es}/panes.md`
- Golden: `norte-cli` help golden (`NORTE_UPDATE_GOLDEN=1`)

**Interfaces:**
- Consumes: Task 4's `new_terminal`, select/close/rename/decorate paths.
- Produces: catalogue names used by Task 6 (TUI dispatch) and Task 7 (window buttons).

Catalogue rows (all `counts = false`):

| command | effect | why |
| --- | --- | --- |
| `terminal.new` | `Launches` | starts a program |
| `terminal.new-profile` | `Launches` | same, after a picker |
| `terminal.close` | `Destroys` | kills a running shell |
| `terminal.next`, `terminal.prev`, `terminal.rename`, `terminal.decorate` | `Inert` | |

`Launches`/`Destroys` ones go into `el_conjunto_que_no_es_inerte_es_exactamente_este`.

Availability: all seven are `NotHere` unless the terminal slot is open; `terminal.new`/`new-profile` additionally `host-not-local` over a remote pane (the check lives in the effect, as for `layout.terminal`).

Chords (screen `browse`, the one `layout.terminal` uses — check the `[…]` table its `ctrl+alt+s` line sits in for each preset):

| command | orthodox, vim, cua, far, norton, total-commander | krusader |
| --- | --- | --- |
| `terminal.new` | `ctrl+alt+n` | unbound — `ctrl+alt+n` is `pane.tab-new` there (attested); header says so |
| `terminal.close` | `ctrl+alt+w` | `ctrl+alt+w` |
| `terminal.next` | `ctrl+pagedown` | `ctrl+pagedown` |
| `terminal.prev` | `ctrl+pageup` | `ctrl+pageup` |
| `terminal.new-profile`, `terminal.rename`, `terminal.decorate` | unbound | unbound |

Before writing any of them: `rg -n '"ctrl\+alt\+w"|"ctrl\+page(up|down)"' crates/norte-frontend/presets/keymap/` must return nothing in `browse` (it returned nothing on 2026-10-09). For the imported four (krusader, far, norton, total-commander) check the manager's own documentation that these chords are not attested for something else; if one is, leave it unbound in that preset and say why. Every preset's header divergences block gets one line: "`terminal.new-profile`, `terminal.rename`, `terminal.decorate`: no chord — inside the panel every chord is taken from the shell; reached by menu, palette and mouse." `ctrl+pageup/pagedown` is VS Code's own next/prev and is the one accepted cost: a `vim` inside the panel loses its tab switch.

Strings: `help-cmd-terminal-*` in both locales; `terminal-exited = exited with code { $code }` / `terminada con código { $code }`; `terminal-new`, `terminal-close`, `terminal-shell-profiles` (button tooltips, Task 7); `terminal-rename-prompt`; `terminal-decorate-title`. Help: a "Several terminals" section in `panes.md` (both languages): the four chords, exit rule, `terminal.toml` example, and that a repository's `.norte/terminal.toml` is ignored.

- [ ] **Step 1:** Catalogue rows + availability + the guard tests the catalogue already has → `just t norte-frontend` red until the sets are updated, then green.
- [ ] **Step 2:** Presets + headers. `just t norte-frontend` (preset guards) and `just t norte-tui` (`panel_keys_pass_through_any_side_panel` style tests live there).
- [ ] **Step 3:** Window dispatch + effects; strings; help. `just t norte-ui-host`, `just t norte-help`, `NORTE_UPDATE_GOLDEN=1 just t norte-cli` then read the diff — only these seven commands.
- [ ] **Step 4:** `just c`; `cargo test -p norte-frontend --doc`.
- [ ] **Step 5:** Commit `feat(keymap): terminal instance commands in all seven presets`.

---

### Task 6: TUI — instances, tab strip, dispatch, mouse

**Files:**
- Modify: `crates/norte-tui/src/app.rs:861` (`terminal` → `terminals: Terminals<TermPanel>`), `src/termpanel.rs` (`open` takes a `&ShellProfile`), `src/dispatch.rs:215-260` (+ new arms), `src/keymap.rs` (`commands!` table, `PANEL_SHARED` gains the four bound `terminal.*`), `src/keys.rs:330-360`, `src/ui/geometry.rs:83-104`, `src/ui/panels.rs:1190-1232`, `src/app/layout.rs:1099-1100`, `src/mouse.rs` (tab clicks)
- Create: `crates/norte-tui/src/ui/terminal_tabs.rs` (strip layout, pure)
- Test: `crates/norte-tui/tests/render.rs` (snapshot), `crates/norte-tui/tests/mouse.rs`

**Interfaces:**
- Consumes: Tasks 2, 3, 5.
- Produces:

```rust
/// What the top border shows: one entry per instance, cut around the active.
pub(crate) struct Strip { pub items: Vec<StripItem>, pub cut_left: bool, pub cut_right: bool }
pub(crate) struct StripItem { pub id: InstanceId, pub label: String, pub active: bool, pub x: u16, pub width: u16 }
/// `labels` are "N title" with "● " before the title when unseen; `width` is the border's free width.
pub(crate) fn layout_strip(labels: &[(InstanceId, String, bool)], active: InstanceId, width: u16) -> Strip;
```

Behaviour:
- `LayoutTerminal` with an empty list starts the default shell profile (today's path). `terminal.new`/`new-profile` push another (new-profile opens the TUI picker the theme picker uses, listing `config.shell_profiles`).
- `geometry.rs`: for every instance `resize` + `pump`, `take_title`, `exit_code` → model calls. If the list becomes empty and the keyboard is in the panel → `release_keyboard()` (today's behaviour, now on "empty", not "dead").
- `keys.rs`: bytes to the ACTIVE instance; `shared_panel_command` already lets `PANEL_SHARED` chords through.
- `panels.rs`: border title = strip when `len() >= 2`, else today's title. Separator `│`, active one in `Role::Title` + bold, others `Role::Muted`, an exited one shows `title (N)`. Cut side gets `…`. Icon glyph only when the icon column is on (ADR 0140; find the flag with `rg -n "icons" crates/norte-tui/src/app.rs`). Colour: `Role` for ANSI index via the theme's terminal palette — the same mapping `termpanel::color_of` uses for `ColorTerm::Indexed`. An exited active instance paints its last grid plus a `Role::Muted` bottom line `terminal-exited`.
- Mouse: store the strip's `(x, width, id)` at paint time like other border buttons; a left click selects.

- [ ] **Step 1: Failing tests:**

```rust
// terminal_tabs.rs
#[test] fn everything_fits() { /* 3 short labels, width 40 → no cuts, x increasing, separators accounted */ }
#[test] fn cut_around_the_active() { /* 10 labels, width 20, active = 7th → item 7 present, cut_left && cut_right */ }
#[test] fn the_active_is_kept_even_if_alone_too_wide() { /* 1 label wider than width → truncated with …, still present */ }
#[test] fn widths_count_chars_not_bytes() { /* label "ñandú" → width 5+number */ }
```

```rust
// tests/render.rs — insta snapshot, like the existing terminal ones there
#[test] fn terminal_strip_with_three_instances() // expect "┌ 1 sh │ 2 ● sh │ 3 sh ─…"
#[test] fn terminal_single_instance_shows_no_strip()
// tests/mouse.rs
#[test] fn a_click_on_a_tab_selects_it()
```

- [ ] **Step 2:** `just t norte-tui` → fails.
- [ ] **Step 3:** Implement. `dispatch.rs` arms stay 1–3 lines calling `crate::termpanel::*`.
- [ ] **Step 4:** `just t norte-tui` (accept snapshots only after reading them); `just c`.
- [ ] **Step 5:** Drive it for real with the tmux harness (memory "Harness tmux para la TUI"): open the panel, `ctrl+alt+n` twice, `exit 3` in one, `ctrl+pageup`, click a tab. `tmux kill-session` afterwards. Commit `feat(tui): several shells in the terminal panel`.

---

### Task 7: Window UI — list, buttons, context menu

**Files:**
- Modify: `crates/norte-gui-tauri/ui/src/render/terminal.ts`, its CSS (find with `rg -n "terminal-row" crates/norte-gui-tauri/ui/src --glob '*.css'`), the action sender used by other slot buttons (`rg -n "select_tab" crates/norte-gui-tauri/ui/src`)
- Test: the webview's existing unit tests for `render/` (`rg -l "paintTerminal" crates/norte-gui-tauri/ui`)

Behaviour (spec §5): title bar actions `+` (`terminal_new`, profile null), `▾` (menu from `TerminalSlotView.profiles` → `terminal_new` with that name), trash (`terminal_close` active). Right-hand list only with `instances.length >= 2`: icon, `var(--term-N)` colour swatch, title via `textContent`, `●` when `unseen`, `t("terminal-exited", {code})` when exited. Click → `terminal_select`; context menu → rename (inline input → `terminal_rename`), colour (6 swatches), icon (six), close. An exited active instance: grid with `opacity` dimmed and the exited line.

- [ ] **Step 1:** Failing renderer tests: list hidden with one instance; shown with two; `unseen` dot; exited label; clicking a row sends `terminal_select` with that id; a title containing `<b>` renders as text.
- [ ] **Step 2–3:** Implement; any new CSS variable must be fed by every theme (the `gui-ci` theme guard).
- [ ] **Step 4:** `just gui-ci`. Look at pixels with the vite + headless Chrome harness (memory "Diseño VS Code") — two instances, one exited.
- [ ] **Step 5:** Commit `feat(gui): terminal instance list and buttons`.

---

### Task 8: ADR, CHANGELOG, memory, merge

- [ ] **Step 1:** `/adr` "The terminal panel holds several shells": one slot vs. one slot per instance (ADR 0170), OSC title vs. foreground process, shell profiles' layers (and the name, vs. configuration profiles), the `ctrl+pageup/pagedown` cost. Spec stays the design record; the ADR states decisions and points at it.
- [ ] **Step 2:** CHANGELOG: what the reader can now do, the four chords, `terminal.toml`, krusader without `terminal.new` chord.
- [ ] **Step 3:** Whole-branch review is NOT needed (one agent wrote it; per-task reviews ran). Merge `feat/terminal-instances` into `main` once, push `main` — the hook is the gate run. Then `just link` + `just link-gui`.
- [ ] **Step 4:** Memory: a new file for this feature (what landed, the chord cost, the Krusader gap), pointer in `MEMORY.md`; update "Dónde está el proyecto" only if a release is cut.
