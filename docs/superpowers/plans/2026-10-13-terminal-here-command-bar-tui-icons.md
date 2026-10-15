# Terminal here, command bar, TUI context menu and TUI icons — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** "Terminal here" that works on every OS and from the right-click menu in both frontends, a Krusader-style command bar, a context menu in the TUI, and the TUI's kitty/sixel icons on the chrome that only the window draws today.

**Architecture:** Every decision lives in `norte-frontend` (candidates, desktop predicate, menu projection, command-bar model, icon source); the TUI and `norte-ui-host` only start processes and paint (ADR 0077). The command bar never types into a shell (ADR 0153): each order is a new terminal-panel entry running `$SHELL -c`.

**Tech Stack:** Rust (ratatui TUI, norte-ui-host actor), TypeScript webview (vitest), Fluent i18n, resvg/tiny-skia rasteriser already in `rail_icons.rs`.

**Spec:** `docs/superpowers/specs/2026-10-13-terminal-here-command-bar-tui-icons-design.md`. One correction to it: the argv template uses `openers.toml`'s field codes (`%d` = the pane's directory), not `{dir}` — same as `[ui] diff`.

## Global Constraints

- Filenames are bytes (rule 1): no `to_str().unwrap()`, `OsString::push` for argv building, names inserted into the bar go through `quote_for`.
- No `std::fs` outside `norte-vfs-local` except where the crate already does it (state files in `norte-frontend` follow the existing hotlist/session pattern).
- Every user-visible string in `crates/norte-i18n/i18n/{en,es}.ftl`, both locales; grep the key before adding it (Fluent keeps the FIRST definition).
- A new command binds in all seven presets or says why in the preset header (CLAUDE.md "A key change is not done until EVERY preset is done"); catalogue entry, `help-cmd-*` in both locales, help topic, `NORTE_UPDATE_GOLDEN=1` for the `norte-cli` golden.
- Tests: `just t <crate>`; `cargo test -p <crate> --doc` after touching a documented public item; `cargo doc -p <crate> --no-deps` after writing doc links; `just gui-ci` after touching `crates/norte-gui-tauri/**`.
- Never `sleep`/`tail -f /dev/null` in agents; nothing will notify you.
- Commit messages via `git commit -F <file>` (the hook reads `<…>` as redirection).
- Branches: `feat/terminal-here` (A), `feat/tui-context-menu` (B), `feat/command-bar` (C), `feat/tui-chrome-icons` (D); merged to `main` together, ONE push.

## Review Focus

1. A directory whose name is not UTF-8 reaches the emulator's argv byte-exact, in the candidate list AND through `[ui] terminal`'s `%d`.
2. Over SSH on macOS (`SSH_CONNECTION` set) the TUI must NOT run `open -a Terminal` (it would open on the remote screen); it falls back to the suspended shell.
3. A context-menu entry run after the listing changed under it must not act on the new cursor row (wrong-file operation) — the TUI menu closes on a stale fingerprint like the host's.
4. A name with `'`, a newline, or invalid UTF-8 inserted into the bar runs as ONE argument in bash, zsh and fish.
5. A command-bar order that exits 0 keeps its terminal entry (the reader typed it to see the output); a plain shell that exits 0 still closes.

Each line has its test in the owning task (A1, A2, B2, C1, C2).

---

## Part A — `feat/terminal-here`

### Task A1: Candidates for every OS, and `[ui] terminal`

**Files:**
- Modify: `crates/norte-frontend/src/shell.rs` (`UNIX_TERMINALS` ~:446, `cwd_args` ~:470, `terminal_candidates[_from]` ~:900-990, `terminal_argv[_from]`, tests ~:1300-1380)
- Modify: `crates/norte-config/src/schema.rs` (`UiSection`, next to `diff` ~:375), `crates/norte-config/src/load.rs` (`ui_terminal` next to `ui_diff` ~:2069, ~:2919, ~:3082, ~:3113), `crates/norte-frontend/src/settings.rs` (row `ui.terminal`, kind `Args`, section `OpenWith`, next to `ui.diff`), `crates/norte-ui-host/tests/config_coverage.rs` (destructure `ui_terminal: _`)
- Modify callers: `crates/norte-gui-tauri/src/native.rs:361` (`terminal()`), `crates/norte-tui/tests/suspend.rs:92`
- Modify: `crates/norte-i18n/i18n/{en,es}.ftl` (settings row label/description for `ui.terminal`, same key shape as `ui.diff`'s)
- Schema golden: `NORTE_UPDATE_SCHEMA=1 just t norte-tui` regenerates `docs/schema/norte.schema.json`

**Interfaces:**
- Produces: `pub fn terminal_candidates_from(configured: Option<&[String]>, env_terminal: Option<&OsStr>, dir: &Path) -> Vec<Vec<OsString>>` and `pub fn terminal_candidates(configured: Option<&[String]>, dir: &Path)` (reads `$TERMINAL`). `terminal_argv[_from]` gain the same first parameter.
- Produces: `CommonConfig::ui_terminal: Option<Vec<String>>` (field name mirrors `ui_diff`). Never honoured from the Project layer (same rule as `diff`).

- [ ] **Step 1: Failing tests** in `shell.rs`'s test module:

```rust
#[test]
fn a_configured_terminal_is_the_only_candidate_and_expands_the_dir() {
    let dir = std::path::Path::new("/home/u/src");
    let cfg = vec!["wezterm".to_owned(), "start".to_owned(), "--cwd".to_owned(), "%d".to_owned()];
    let all = terminal_candidates_from(Some(&cfg), Some("kitty".as_ref()), dir);
    assert_eq!(all.len(), 1, "configured wins over $TERMINAL and every probe: {all:?}");
    assert_eq!(all[0], ["wezterm", "start", "--cwd", "/home/u/src"].map(OsString::from).to_vec());
}

#[cfg(unix)]
#[test]
fn a_non_utf8_dir_survives_the_configured_template() {
    use std::os::unix::ffi::OsStrExt;
    let dir = std::path::Path::new(OsStr::from_bytes(b"/tmp/caf\xE9"));
    let cfg = vec!["foot".to_owned(), "-D".to_owned(), "%d".to_owned()];
    let all = terminal_candidates_from(Some(&cfg), None, dir);
    assert_eq!(all[0][2].as_bytes(), b"/tmp/caf\xE9");
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn the_probe_list_knows_the_current_desktops_terminals() {
    let dir = std::path::Path::new("/w");
    let all = terminal_candidates_from(None, None, dir);
    let programs: Vec<_> = all.iter().map(|a| a[0].to_string_lossy().into_owned()).collect();
    for p in ["ptyxis", "kgx", "foot", "xfce4-terminal", "x-terminal-emulator"] {
        assert!(programs.contains(&p.to_owned()), "{p} missing: {programs:?}");
    }
    // x-terminal-emulator is an ALTERNATIVE (any program): no flag guessed.
    let x = all.iter().find(|a| a[0] == "x-terminal-emulator").unwrap();
    assert_eq!(x.len(), 1);
    let ptyxis = all.iter().find(|a| a[0] == "ptyxis").unwrap();
    assert_eq!(ptyxis[1], "--working-directory=/w");
}
```

Update the existing tests' calls to pass `None` as the new first argument.

- [ ] **Step 2:** `just t norte-frontend` — FAIL (signature, missing emulators).
- [ ] **Step 3: Implement.** `configured` non-empty → `vec![openers::expand_argv(cfg, &[], dir)]` and return (exclusive). Add to `UNIX_TERMINALS` after `xdg-terminal-exec`'s probe order: `ptyxis`, `kgx`, `foot`, `xfce4-terminal` before `xterm`; `x-terminal-emulator` LAST before `xterm` (Debian's answer when nothing better is installed). Flags in `cwd_args` — verify each against the emulator's `--help` before committing and quote it in the commit message: `ptyxis` `--working-directory=`, `kgx` `--working-directory=`, `foot` `--working-directory=`, `xfce4-terminal` `--working-directory=`; `x-terminal-emulator` none. Also extend `exec_flag` if any of them does not take `-e` (`kgx` takes `-e`; `ptyxis` takes `-x`; verify).
- [ ] **Step 4:** Config plumbing (copy `diff`'s three sites in `load.rs` and its doc wording; Project layer ignored), settings row, i18n. Callers: `native.rs::terminal` reads `self`-less config — pass the host's `ui_terminal` through `NativeEffect::OpenTerminal { dir, configured: Option<Vec<String>> }` (`crates/norte-ui-host/src/dto.rs`; size it first: `rg -c "OpenTerminal \{" --glob '*.rs'`). The renderer never sees `NativeEffect`, so no bridge bump.
- [ ] **Step 5:** `just t norte-frontend`, `just t norte-config`, `NORTE_UPDATE_SCHEMA=1 just t norte-tui`, `just t norte-ui-host`, `cargo test -p norte-frontend --doc`, `cargo test -p norte-config --doc` — PASS. Commit `feat(frontend): terminal candidates for current desktops and [ui] terminal`.

### Task A2: The TUI opens an emulator when there is a desktop (ADR)

**Files:**
- Modify: `crates/norte-frontend/src/shell.rs` (new predicate), `crates/norte-tui/src/main.rs:225` (use it), `crates/norte-tui/src/dispatch.rs:781` (`Command::AppTerminal`), `crates/norte-tui/src/app.rs:123` (`PendingShell` unchanged) and a new `crates/norte-tui/src/launch.rs` (detached spawn)
- Create: `docs/adr/0180-terminal-here-opens-an-emulator-when-there-is-a-desktop.md` (use the `/adr` skill)
- Modify: `crates/norte-i18n/i18n/{en,es}.ftl` (`msg-terminal-opened = Opened { $program } in { $path }`, `msg-terminal-fallback = No terminal emulator started; opening a shell here instead`)

**Interfaces:**
- Produces: `pub fn has_desktop_from(var: impl Fn(&str) -> Option<OsString>) -> bool` and `pub fn has_desktop() -> bool` in `norte_frontend::shell`.
- Produces: `norte_tui::launch::spawn_detached(program: &Path, args: &[OsString], cwd: &Path) -> std::io::Result<()>` — own process group (`process_group(0)` on unix, `CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS` on Windows), stdin/stdout/stderr null, child not waited on (reap with a detached `std::thread` that `wait()`s, as `native.rs::launch` does — read it and share the helper if it is not tauri-specific).

- [ ] **Step 1: Failing tests** (`shell.rs`):

```rust
fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
    move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v))
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn a_desktop_on_linux_is_a_display_even_over_ssh_x() {
    assert!(!has_desktop_from(env(&[])));
    assert!(!has_desktop_from(env(&[("DISPLAY", "")])));
    assert!(has_desktop_from(env(&[("WAYLAND_DISPLAY", "wayland-0")])));
    assert!(has_desktop_from(env(&[("DISPLAY", "localhost:10.0"), ("SSH_CONNECTION", "x")])));
}

#[cfg(any(target_os = "macos", windows))]
#[test]
fn over_ssh_a_mac_or_windows_screen_is_not_the_readers() {
    assert!(has_desktop_from(env(&[])));
    assert!(!has_desktop_from(env(&[("SSH_CONNECTION", "1.2.3.4 5 6.7.8.9 22")])));
    assert!(!has_desktop_from(env(&[("SSH_TTY", "/dev/ttys001")])));
}
```

- [ ] **Step 2:** `just t norte-frontend` — FAIL.
- [ ] **Step 3:** Implement the predicate; `main.rs` sets `app.has_desktop = norte_frontend::shell::has_desktop()` (the handoff's `windowed` fact now also gets the macOS/Windows SSH rule — note it in the ADR).
- [ ] **Step 4:** `Command::AppTerminal`: refusals first (`shell_cwd`, plus the archive key from A3). With `app.has_desktop`, resolve `terminal_candidates(app.ui_terminal.as_deref(), &dir)` with `openers::resolve_program` (off the loop turn, like `user_cmd::outcome_of` does with `spawn_blocking`) and `spawn_detached` the first that starts → `msg-terminal-opened`. None started, or no desktop → today's `PendingShell` and, when it was a fallback, `msg-terminal-fallback`. `app.ui_terminal` comes from `cfg.common.ui_terminal` in `main.rs` next to `app.diff`.
- [ ] **Step 5:** Test in `crates/norte-tui/tests/` (pattern of `suspend.rs`): with `has_desktop = false`, `AppTerminal` sets `pending_shell`; with `has_desktop = true` and `ui_terminal = Some(["/nonexistent-term"])`, it falls back to `pending_shell` AND the message is the fallback one. `just t norte-tui` — PASS.
- [ ] **Step 6:** ADR 0180 (decision: TUI follows the window when there is a desktop; the macOS/Windows SSH rule; fallback to the suspended shell; why not always external). Commit `feat(tui): terminal here opens an emulator when there is a desktop`.

### Task A3: Disabled with a reason, and in the window's context menu

**Files:**
- Modify: `crates/norte-frontend/src/availability.rs` (`Facts`, `Reason`, `reason_key`, `verdict`, the `reason_keys_exist_in_both_locales` list), every `Facts {` literal (`rg -c "Facts \{" --glob '*.rs'` ≈ 34, most in `availability.rs` tests via `one_file()`), `crates/norte-tui/src/app/caps.rs:153`, `crates/norte-ui-host/src/controller/context_menu.rs` (facts frozen at opening) and its other `Facts` builders
- Modify: `crates/norte-frontend/src/context_menu.rs` (`entries`), `crates/norte-ui-host/tests/controller/context_menu.rs` (expected lists)
- Modify: `crates/norte-ui-host/src/controller/effects.rs:610` (`open_terminal`: archive reason), `crates/norte-i18n/i18n/{en,es}.ftl`

**Interfaces:**
- Produces: `Facts::pane_on_disk: bool` (the acting pane's directory is a native local path) and `Facts::pane_in_archive: bool`; `Reason::InsideArchive` → `reason-inside-archive` ("inside an archive: go up to its folder"); `Reason::NotOnThisDisk` → `reason-not-on-this-disk` (reuse the wording of `host-not-local`).
- `verdict("app.terminal")` and `verdict("pane.command-line")` = `first_failure(&[(facts.pane_in_archive, InsideArchive), (!facts.pane_on_disk, NotOnThisDisk)])`.

- [ ] **Step 1: Failing tests:** in `availability.rs`:

```rust
#[test]
fn terminal_here_says_why_not_inside_an_archive_or_remote() {
    let mut f = one_file();
    f.pane_on_disk = true;
    assert_eq!(verdict("app.terminal", &f), Availability::Available);
    f.pane_on_disk = false;
    f.pane_in_archive = true;
    assert_eq!(verdict("app.terminal", &f).reason(), Some(Reason::InsideArchive));
    f.pane_in_archive = false;
    assert_eq!(verdict("app.terminal", &f).reason(), Some(Reason::NotOnThisDisk));
}
```

and in `context_menu.rs`: `commands(&Surface::Empty { remote: false })` and `commands(&Surface::Row(..))` contain `"app.terminal"` in section `ctx-section-open` (grep the existing section keys first; reuse one if it fits).
- [ ] **Step 2:** `just t norte-frontend` — FAIL.
- [ ] **Step 3:** Implement; fill the facts where `Facts` is built (`pane_on_disk = shell::is_local(dir)`, `pane_in_archive = dir.archive_split() is Ok(Some(_))`). `open_terminal` refuses an archive pane with `reason-inside-archive`.
- [ ] **Step 4:** `just t norte-frontend`, `just t norte-ui-host`, `just t norte-tui`, `cargo test --workspace --exclude norte-gui-tauri --doc` (Facts is public: doctests build literals) — PASS. `just gui-ci` once for the branch. Commit `feat: terminal here in the context menu, disabled with its reason`.

---

## Part B — `feat/tui-context-menu`

### Task B1: One projection of a menu, shared

**Files:**
- Modify: `crates/norte-frontend/src/context_menu.rs` (new `project`), `crates/norte-ui-host/src/controller/context_menu.rs:449-529` (use it)

**Interfaces:**
- Produces:

```rust
pub struct Row { pub label: String, pub chord: String, pub enabled: bool,
                 pub reason: String, pub section: Option<String>, pub role: &'static str,
                 pub action: Action }
pub fn project(entries: &[Entry], facts: &availability::Facts, implemented: &[&str],
               keymap: &keymap::Effective, lang: Lang,
               verb_reason: impl Fn(Verb) -> Option<Cow<'static, str>>) -> Vec<Row>
```

`implemented` is the frontend's command set (host: `commands::all_with(effects)`; TUI: its dispatcher's list — find the TUI equivalent of `IMPLEMENTED` used by its palette). `verb_reason` keeps the host's two verb vetoes where they are (they read the host's `Subject`).
- [ ] **Step 1:** Move the body of `vista_context_menu`/`reason_against` into `project` without behaviour change; the host's existing context-menu tests are the safety net. Add one unit test in `norte-frontend`: an entry whose command is not in `implemented` projects `enabled: false, reason == t("reason-unavailable")`.
- [ ] **Step 2:** `just t norte-frontend`, `just t norte-ui-host` — PASS. Commit `refactor(frontend): the context menu projection is shared`.

### Task B2: The TUI opens, paints and runs a context menu

**Files:**
- Create: `crates/norte-tui/src/context_menu.rs` (state + keys + run), `crates/norte-tui/src/ui/context_menu.rs` (paint)
- Modify: `crates/norte-tui/src/mouse.rs:1627` (right button), `crates/norte-tui/src/dispatch.rs` (`pane.context-menu` stops being "not here"; `Esc`/keys routed while open), `crates/norte-tui/src/app.rs` (`context_menu: Option<ContextMenu>`), the TUI's implemented-command list, `crates/norte-tui/src/ui.rs` (paint last, above panes)
- Test: `crates/norte-tui/tests/context_menu.rs`

**Interfaces:**
- Consumes: `norte_frontend::context_menu::{Surface, entries, project, Row}`.
- Produces: `ContextMenu { surface: Surface, rows: Vec<Row>, cursor: usize, anchor: (u16, u16), fingerprint: Fingerprint }` where `Fingerprint { dir: VPath, entry: Option<VPath>, marks: usize }` mirrors the host's `fingerprint_of` (`context_menu.rs` in ui-host) — lift that struct into `norte-frontend` in this task rather than writing a second one.

- [ ] **Step 1: Failing tests** (`tests/context_menu.rs`, App-level like `tests/mouse.rs`):
  - right click on a file row opens a menu whose labels equal `project(entries(&Surface::Row(..)), …)`'s for the same facts (the two frontends build from one list);
  - `Enter` on "Copy" dispatches `pane.copy` (assert the same pending state a `F5` press leaves);
  - **stale**: open on row 2, then refresh the listing so a different entry sits at row 2, press `Enter` → nothing dispatched and the menu is closed (Review Focus 3);
  - a disabled entry (`app.terminal` on a `mem://` pane) does not dispatch on `Enter` nor on click;
  - click outside closes it (ADR 0178); `Esc` closes it.
- [ ] **Step 2:** `just t norte-tui` — FAIL.
- [ ] **Step 3:** Implement. Surface measured from the hit (`spot(hit)` in `mouse.rs`: a row → `Surface::Row`, below the last row → `Empty`, header row → `Header`); the cursor's row for the `pane.context-menu` key. Paint: bordered `Block` at the anchor clamped into the frame, section rule between sections, disabled rows with `Role::Disabled` (or the dimmed role the TUI menus use) and their reason right-aligned, chords right-aligned when enabled. Keys: ↑/↓ skip nothing (disabled rows are selectable, they say why), `Enter`, `Esc`, mouse click on a row.
- [ ] **Step 4:** `just t norte-tui` — PASS; pilot it in tmux (memory `tui-harness-tmux`) — right click via `tmux send-keys` SGR mouse sequence — and capture the pane. Commit `feat(tui): a context menu on right click and the Menu key`.

### Task B3: Places, tree branches and column headers in the TUI menu

**Files:** `crates/norte-tui/src/context_menu.rs` (verb runner), `crates/norte-tui/src/mouse.rs` (places/tree/header hits)
- [ ] **Step 1: Failing tests:** right click on a favourite in the places panel → menu has "Remove favourite" and running it removes it (same effect as the existing TUI key for it); on a tree branch → "Toggle fold" folds it; on a hideable header → "Hide column" hides it, on `name` it is disabled with `reason-wrong-target`.
- [ ] **Step 2–3:** Map each `Verb` to the TUI's existing handler for that surface (grep the places/tree/columns modules for open/favourite/fold/sort/hide). No new behaviour: a verb with no TUI handler is a bug to report, not to invent.
- [ ] **Step 4:** `just t norte-tui`, `just c` — PASS. Update the context-menu design doc's "Out of scope" line and the parity matrix (`parity_matrix.rs`) if it lists `pane.context-menu` as window-only. Commit `feat(tui): context menu on places, tree and headers`.

---

## Part C — `feat/command-bar`

### Task C1: The bar's model, shared

**Files:**
- Create: `crates/norte-frontend/src/command_bar.rs` (+ `pub mod` in `lib.rs`)

**Interfaces:**
- Produces:

```rust
pub struct CommandBar { /* buffer: String, cursor: usize (char index), history: Vec<String>, browsing: Option<usize> */ }
impl CommandBar {
    pub fn new(history: Vec<String>) -> Self;
    pub fn text(&self) -> &str; pub fn cursor(&self) -> usize;
    pub fn insert(&mut self, s: &str); pub fn backspace(&mut self); pub fn delete(&mut self);
    pub fn left(&mut self); pub fn right(&mut self); pub fn home(&mut self); pub fn end(&mut self);
    pub fn history_prev(&mut self); pub fn history_next(&mut self);
    pub fn complete(&mut self, names: &[&[u8]], shell: Shell) -> bool; // longest common prefix of the word before the cursor
    pub fn take(&mut self) -> Option<String>; // trimmed order, pushed to history (dedup consecutive, cap HISTORY_MAX = 200); None if blank
    pub fn history(&self) -> &[String];
}
pub fn quote_for(shell: Shell, name: &[u8]) -> String;      // ASCII-only result, one word for that shell
pub fn argv(shell_path: &Path, order: &str) -> Vec<OsString>; // = shell::shell_command_argv
pub fn load_history(path: &Path) -> Vec<String>;           // one order per line, missing file = empty
pub fn save_history(path: &Path, h: &[String]) -> std::io::Result<()>; // temp + rename, mode 0600
```

`Shell` is `norte_frontend::subshell::Shell` (`of_program`); unknown shells quote as POSIX `sh`.
- [ ] **Step 1: Failing tests**:

```rust
#[test]
fn a_hostile_name_is_one_ascii_word_in_every_shell() {
    let name = b"it's\na caf\xE9";
    for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
        let q = quote_for(shell, name);
        assert!(q.is_ascii() && !q.contains('\n'), "{shell:?}: {q}");
    }
}

/// Runs the quoted word through the REAL shell and gets the bytes back.
#[cfg(unix)]
#[test]
fn the_shells_read_back_the_exact_bytes() {
    use std::os::unix::ffi::OsStrExt;
    let name = b"it's\na caf\xE9 $HOME `x`";
    for (shell, bin) in [(Shell::Bash, "bash"), (Shell::Zsh, "zsh"), (Shell::Fish, "fish")] {
        let Some(path) = crate::openers::resolve_program(std::ffi::OsStr::new(bin)) else { continue };
        let order = format!("printf %s {}", quote_for(shell, name));
        let out = std::process::Command::new(&path).args(argv(&path, &order).into_iter().skip(1)).output().unwrap();
        assert_eq!(out.stdout, name, "{bin}: {order}");
    }
}

#[test]
fn history_skips_blanks_and_consecutive_repeats() {
    let mut b = CommandBar::new(Vec::new());
    for o in ["ls", "ls", "  ", "make"] { b.insert(o); let _ = b.take(); }
    assert_eq!(b.history(), ["ls", "make"]);
    b.history_prev(); assert_eq!(b.text(), "make");
    b.history_prev(); assert_eq!(b.text(), "ls");
    b.history_next(); b.history_next(); assert_eq!(b.text(), "");
}
```

plus `complete` (two names `report-a.txt`, `report-b.txt`, buffer `cat rep` → `cat report-`; a single match appends the quoted name and a space) and a `save_history`/`load_history` round trip in a tempdir.
- [ ] **Step 2:** `just t norte-frontend` — FAIL.
- [ ] **Step 3:** Implement. Quoting: bash/zsh → `$'…'` with `\NNN` octal for every byte outside printable ASCII and for `'` and `\`; fish → unquoted word with `\xHH` for those bytes and backslash-escaping fish's specials (`[]{}()*?$&;|<>~#"'\ ` and space); sh → `'…'` with `'\''`, and a non-UTF-8 or control byte makes it `"$(printf '\NNN…')"` (document that trailing newlines are lost there — POSIX limitation, sh only).
- [ ] **Step 4:** PASS. `cargo test -p norte-frontend --doc`. Commit `feat(frontend): the command bar's model`.

### Task C2: A terminal entry that stays after a clean exit

**Files:** `crates/norte-frontend/src/terminals.rs` (`Instance`, `on_exit` ~:352), callers that build instances (`rg -n "exited: None" crates`)
**Interfaces:** Produces `Instance::keep_on_success: bool` (default `false`); `Terminals::open_kept(...)` or a parameter on the existing open — follow whichever the file already uses for `title`.
- [ ] **Step 1: Failing test** in `terminals.rs`: an instance with `keep_on_success = true` and `on_exit(id, 0)` is still listed with `exited == Some(0)`; one without is removed (Review Focus 5).
- [ ] **Step 2–4:** Implement; `just t norte-frontend`, `just t norte-tui`, `just t norte-ui-host` — PASS. Commit `feat(frontend): a terminal entry can stay after a clean exit`.

### Task C3: The bar in the TUI

**Files:**
- Create: `crates/norte-tui/src/ui/command_bar.rs`, `crates/norte-tui/src/command_bar.rs` (keys + run)
- Modify: `crates/norte-config/src/{schema,load}.rs` (`[ui] command_bar: Option<bool>`, all layers — presentation), `crates/norte-frontend/src/settings.rs` (row `ui.command-bar`, `Bool`), keymap catalogue (`app.command-bar`, effect like other layout toggles) + the seven presets in `crates/norte-frontend/assets/keymaps/*.toml` (or wherever `rg -l "app.terminal" --glob '*.toml'` finds them), `crates/norte-tui/src/dispatch.rs` (`pane.command-line` focuses the bar; `Command::AppCommandBar` toggles), `crates/norte-tui/src/app/prompts.rs:788` + `app/modal.rs:555` + `gestures.rs:232` (remove `Modal::CommandLine` and `submit_command_line`), the TUI layout (one row above the key bar when shown or focused), `crates/norte-i18n/i18n/{en,es}.ftl`, help topic (`crates/norte-help/topics/{en,es}/shell.md`: the bar, `-c` loads no rc file), `norte-cli` golden (`NORTE_UPDATE_GOLDEN=1`)
- Test: `crates/norte-tui/tests/command_bar.rs`

**Interfaces:** Consumes `CommandBar`, `quote_for`, `argv`, `load/save_history` (C1), `keep_on_success` (C2), `termpanel::start_instance_in` (`crates/norte-tui/src/termpanel.rs:201`). History file: `<state dir>/command-history` (the directory the hotlist/session use; grep `state_dir`).
- [ ] **Step 1: Failing tests:**
  - `pane.command-line` with the bar hidden shows and focuses it; `Esc` returns focus to the pane and hides it again if it was hidden;
  - typing `echo hi` + `Enter` opens the terminal panel if closed and adds ONE instance whose argv is `argv(login_shell, "echo hi")`, cwd = focused pane dir, `keep_on_success = true`, title `echo hi`; nothing is written to any existing instance's pty (assert the other instances' input queues are empty);
  - the insert-name chord on a cursor entry named `a b` inserts `quote_for(shell, b"a b")`;
  - on a `mem://` pane `Enter` does nothing and the bar shows `reason-not-on-this-disk`;
  - every preset binds `app.command-bar` or its header explains (existing preset-coverage test should catch it — run it).
- [ ] **Step 2–3:** Implement. The prompt is the pane dir through `norte_frontend::path_display` (masked), cut from the start (`cutStart` equivalent in the TUI: reuse the breadcrumb's helper). The insert-name chord: `ctrl+enter` is not deliverable in most terminals — use `alt+enter` in all presets (check with the "chord is deliverable" rule; `ctrl+j` is Enter).
- [ ] **Step 4:** `just t norte-tui`, `just t norte-frontend`, `just t norte-cli` (golden), `NORTE_UPDATE_SCHEMA=1 just t norte-tui`, `just c` — PASS. Pilot in tmux. Commit `feat(tui): a command bar that runs each order in the terminal panel`.

### Task C4: The bar in the window

**Files:** `crates/norte-ui-host/src/dto.rs` (`CommandBarView { prompt: String, prompt_hostile: bool, text: String, cursor: u64, focused: bool, reason: String }` in the snapshot), bridge version bump (grep `BRIDGE_VERSION`), the `actions.json` golden + `UiAction::CommandBar{Input,Key,Submit}` (follow how the omnibox/search input travel — `rg -n "Omnibox" crates/norte-ui-host/src/dto.rs`), `crates/norte-ui-host/src/controller/` (new `command_bar.rs`, runs through `start_user_terminal`'s path with `keep_on_success`), `crates/norte-gui-tauri/ui/src/types.ts`, new `ui/src/render/commandbar.ts`, `style.css`, `crates/norte-gui-tauri/tests/parity_matrix.rs` (move `pane.command-line` out of `DOES_NOT_APPLY`), `ui/tests/render.test.ts`
- [ ] **Step 1: Failing tests:** host test (pattern of `tests/controller/context_menu.rs`): submitting `echo hi` emits the terminal-instance start with the C3 argv/cwd/keep; a `sftp://` pane refuses with `reason-not-on-this-disk`. Renderer test: the bar paints the prompt and text with `textContent`, never `innerHTML`; typing sends `command_bar_input`.
- [ ] **Step 2–4:** Implement; `just t norte-ui-host`, `just gui-ci` — PASS. Commit `feat(window): the command bar`.

### Task C5: ADR and changelog for the bar
- [ ] ADR 0181 "the command bar runs each order as a terminal-panel entry" (why not typing: ADR 0153; why `$SHELL -c` and not `-ic`; why entries stay after exit 0). CHANGELOG "Added". Commit `docs: ADR 0181 and changelog for the command bar`.

---

## Part D — `feat/tui-chrome-icons`

### Task D1: One drawing per chrome icon, shared

**Files:**
- Create: `crates/norte-frontend/assets/ui-icons/{layout-split-h,layout-split-v,layout-equalize,layout-flip,layout-pick,fs-folder,fs-folder-open,fs-file,fs-drive,fs-removable,fs-network,fs-home,fs-favorite,ui-remote,ui-bell,ui-search}.svg` — each the exact `paths`/`circles` of `crates/norte-gui-tauri/ui/src/render/icons.ts` `SHAPES` on a 24 grid, stroked with `currentColor`, same attributes as `assets/panel-icons/*.svg`
- Modify: `crates/norte-frontend/src/chrome/panelbar.rs:196` → generalise to `pub fn icon_svg(id: &str) -> Option<&'static str>` covering panel kinds AND `layout:*`/`fs:*`/`ui:*` ids (keep the name; it already is "the shared icon")
- Modify: `crates/norte-gui-tauri/ui/src/render/icons.ts` (import those files `?raw` into `PANEL_SVG`-style map; delete the moved `SHAPES`)
- Test: `crates/norte-gui-tauri/tests/` new `shared_icons.rs`: every `?raw` import in `icons.ts` names a file that exists and every id `icon_svg` answers has a file the window imports (one drawing, two frontends)
- [ ] Steps: failing test → move → `just t norte-frontend`, `just gui-ci` — PASS; visually compare the window (vite harness, see the help work of 2026-10-13) before/after: identical. Commit `refactor: chrome icons are shared SVG files`.

### Task D2: Icons on the TUI's layout buttons

**Files:** `crates/norte-tui/src/rail_icons.rs` (`raster(kind, rgb, canvas)` already takes a canvas: add a cell box `(cols, rows)` to the placement type, today fixed 2×2), `crates/norte-tui/src/ui/chrome.rs:300-345` (`draw_layout_buttons`, `layout_zones`), `crates/norte-tui/src/ui.rs:719` (`rail_icons_to_place` → also collect chrome icons)
- [ ] **Step 1: Failing tests:** with graphics on, `draw_layout_buttons` paints no ASCII glyph and requests one placement per button of 2×1 cells at the button's x; with graphics off, today's `[|]` text (snapshot unchanged); `layout_zones` cells are identical in both modes (a click still lands).
- [ ] **Step 2–4:** Implement; `just t norte-tui` — PASS. Verify pixels on kitty and on `xterm -ti vt340` sixel under Xvfb (memory `columna-de-paneles-grande-0168-0169`). Commit `feat(tui): the layout buttons draw their icons`.

### Task D3: Places/tree rows and status bar indicators
- [ ] Same pattern as D2 for the places and tree rows (`fs:*`, 2×1 before the name, text fallback unchanged) and the status bar's remote/bell/search indicators (`ui:*`). Tests: placement per visible row only (scrolling does not leave stale images — assert the placement list after a scroll), fallback text when graphics are off. `just t norte-tui`, pixels verified. Commit `feat(tui): places, tree and status icons`.

---

## Finish

- [ ] `just ci-fast` once mid-plan (after Part B) and fix in one batch.
- [ ] Reviewers before each part's last commit: `rust-reviewer` for A/B/C/D Rust diffs; `security-reviewer` for C (it launches programs from typed text) and A2 (detached spawn); `encoding-auditor` for A1 and C1 (bytes in argv and quoting).
- [ ] `just gui-ci` before merging (A3, C4, D1 touch the window).
- [ ] Merge the four branches into `main`, push `main` once (the hook is the gate), `just link && just link-gui`, update memory (`donde-esta-el-proyecto`, a new memory for the command bar decision) and CHANGELOG.
