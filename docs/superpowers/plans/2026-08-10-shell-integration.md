# Shell integration implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make norte something you stay in: cd-on-quit for bash/zsh/fish, a
picker other tools can pipe, and the three #135 keys (`F9`, `Ctrl+O`, command
line) built by suspending the TUI.

**Architecture:** All decidable logic is pure and lives in one new module,
`norte-frontend::shell` — which bytes go in the cd-file, which bytes come out
of the picker, the text of each shell wrapper, which terminal emulator to
probe. The TUI's output moves from stdout to the controlling terminal
(`crates/norte-tui/src/tty.rs`) so stdout can carry data. The three keys reuse
`run_opener` (`crates/norte-tui/src/main.rs:7841`), which already releases the
mouse, leaves raw mode and the alternate screen, and restores on every path.

**Tech stack:** Rust, ratatui/crossterm, `norte_vfs_local::vpath_to_native`,
Fluent (`crates/norte-i18n/i18n/{en,es}.ftl`), no new dependencies.

**Spec:** `docs/superpowers/specs/2026-08-10-shell-integration-design.md`

---

## File structure

| file | responsibility |
| --- | --- |
| `crates/norte-frontend/src/shell.rs` (new) | Everything pure: `pick_bytes`, `cd_bytes`, `Shell` + wrapper text, `login_shell`, `terminal_argv`. No I/O, no terminal. |
| `crates/norte-frontend/src/lib.rs` | `pub mod shell;` |
| `crates/norte-tui/src/tty.rs` (new) | The controlling-terminal handle: open, `init`/`restore`, panic hook, the `Tui` type alias. |
| `crates/norte-tui/src/lib.rs` | `pub mod tty;` |
| `crates/norte-tui/src/main.rs` | Flags, the picker exit path, the cd-file write, three new dispatch arms, `run_suspended`. |
| `crates/norte-tui/src/keymap.rs` | Four new `Command` variants. |
| `crates/norte-frontend/src/keymap/catalogue.rs` | `app.pick-accept` added; three `planned` entries flip to `live`. |
| `crates/norte-cli/src/main.rs` | `norte shell-init <SHELL>` subcommand. |
| `crates/norte-cli/src/doctor.rs` | One line: is the wrapper installed. |
| `crates/norte-cli/tests/shell_init_e2e.rs` (new) | Runs bash, zsh and fish for real against the emitted wrapper. |
| `crates/norte-gui/src/main.rs` | `app.terminal` via the system terminal emulator. |
| `crates/norte-i18n/i18n/{en,es}.ftl` | Every new user-facing string. |

---

## Task 1: the controlling terminal

**Why first:** stdout must be free before the picker can write to it, and this
task alone fixes `ntc > log` writing escape sequences into `log`.

**Files:**
- Create: `crates/norte-tui/src/tty.rs`
- Modify: `crates/norte-tui/src/lib.rs` (add `pub mod tty;`)
- Modify: `crates/norte-tui/src/main.rs:1239` (`ratatui::init()`), `:1263-1264`
  (`capture.set` + `ratatui::restore()`), `:1280-1296` (`arm_mouse`'s panic
  hook and `capture.set`), `:1523`, `:7770`, `:7841` (the `DefaultTerminal`
  signatures), `:7859-7876` (`run_opener`'s stdout sites)

- [ ] **Step 1: Write the failing test**

In `crates/norte-tui/src/tty.rs`, at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// No controlling terminal is an ERROR with a message, never a crash and
    /// never a screenful of escapes into somebody's pipe. Under nextest the
    /// test process has no tty, which is exactly the case being pinned.
    #[test]
    fn opening_without_a_controlling_terminal_is_an_error() {
        // Only meaningful where the harness really has no tty; when it does
        // (a developer running under a terminal that leaks it), the open
        // succeeds and there is nothing to assert.
        if let Err(e) = open_controlling_terminal() {
            let m = e.to_string();
            assert!(
                m.contains("terminal"),
                "the error must name what is missing: {m}"
            );
        }
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `just t norte-tui`
Expected: FAIL, `cannot find function open_controlling_terminal in this scope`.

- [ ] **Step 3: Write the module**

`crates/norte-tui/src/tty.rs`, with a module doc explaining WHY (stdout carries
data now — `--pick`; a TUI that paints there cannot be piped). Public surface,
exactly these names, used unchanged by later tasks:

```rust
/// The terminal this process is attached to, as a writer.
pub type TtyOut = std::fs::File;

/// The TUI terminal. Replaces `ratatui::DefaultTerminal`, which is hard-wired
/// to stdout.
pub type Tui = ratatui::Terminal<ratatui::backend::CrosstermBackend<TtyOut>>;

/// Opens the CONTROLLING terminal for writing: `/dev/tty` on unix, `CONOUT$`
/// on Windows. `Err` when there is none (cron, both ends piped).
pub fn open_controlling_terminal() -> std::io::Result<TtyOut>;

/// Raw mode + alternate screen on the given handle, plus a panic hook that
/// undoes both. Mirrors what `ratatui::init` did for stdout.
pub fn init(out: TtyOut) -> std::io::Result<Tui>;

/// Undoes [`init`]. Called once, on the way out.
pub fn restore(term: &mut Tui) -> std::io::Result<()>;
```

Implementation notes the engineer needs:

- unix: `OpenOptions::new().read(true).write(true).open("/dev/tty")`. Windows:
  the same on `"CONOUT$"`. `read(true)` matters on unix — some terminals refuse
  a write-only open.
- The panic hook wraps the hook already in place (the pattern is right there in
  `arm_mouse`, `main.rs:1280`): take it, install one that disables raw mode,
  leaves the alternate screen and disables mouse capture **on a freshly opened
  handle** (the hook cannot borrow `TtyOut`), then calls the previous hook.
- `init` must return the error rather than panicking; `main` turns it into the
  exit-code-2 message.

- [ ] **Step 4: Run the test**

Run: `just t norte-tui`
Expected: PASS.

- [ ] **Step 5: Rewire `main.rs`**

Mechanical, in this order:
1. `let mut terminal = ratatui::init();` (`:1239`) becomes an
   `open_controlling_terminal()` + `tty::init(out)`, with the `Err` arm
   printing `ntc: no controlling terminal` to stderr and returning exit code 2.
   Keep the handle: several later sites need a second one, obtained with
   `try_clone()`.
2. `ratatui::restore()` (`:1264`) becomes `tty::restore(&mut terminal)`.
3. Every `std::io::stdout()` that drives the terminal — `:1263`, `:1289`,
   `:1293`, `:2123`, `:7859`, `:7863`, `:7871`, `:7876` — takes the tty handle
   instead. `mouse::Capture::set` already accepts `&mut impl Write`
   (`mouse.rs:611`), so nothing there changes.
4. `ratatui::DefaultTerminal` in the three signatures (`:1523`, `:7770`,
   `:7841`) becomes `norte_tui::tty::Tui`.

- [ ] **Step 6: Verify nothing else paints to stdout**

Run: `rg -n 'std::io::stdout' crates/norte-tui/src/`
Expected: no hits that write terminal control sequences. A hit inside a test is
fine; a hit in the run loop is a bug this step exists to catch.

- [ ] **Step 7: Test and lint**

Run: `just t norte-tui` then `just c`
Expected: both green.

- [ ] **Step 8: Commit**

```bash
git add crates/norte-tui/src/tty.rs crates/norte-tui/src/lib.rs crates/norte-tui/src/main.rs
git commit -m "feat(tui): the interface paints on the terminal, not on stdout"
```

---

## Task 2: `ntc --pick`

**Files:**
- Create: `crates/norte-frontend/src/shell.rs`
- Modify: `crates/norte-frontend/src/lib.rs`
- Modify: `crates/norte-frontend/src/keymap/catalogue.rs` (add `app.pick-accept`)
- Modify: `crates/norte-tui/src/keymap.rs` (`"app.pick-accept" => AppPickAccept`)
- Modify: `crates/norte-tui/src/main.rs` (`BOOL_FLAGS`, `USAGE`, dispatch arm, exit path)
- Modify: `crates/norte-i18n/i18n/{en,es}.ftl`

- [ ] **Step 1: Write the failing test**

In `crates/norte-frontend/src/shell.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::VPath;

    /// NUL-TERMINATED, not NUL-separated: one result is unambiguous and
    /// `xargs -0` is happy either way. The bytes are the path's, untouched —
    /// a name is bytes (rule 1) and a picker that lossily decodes is a picker
    /// that opens the wrong file.
    #[test]
    fn pick_bytes_terminates_every_path_with_nul() {
        let a = VPath::parse("file:///tmp/a").unwrap();
        let b = VPath::parse("file:///tmp/b").unwrap();
        assert_eq!(pick_bytes(&[a, b]), b"/tmp/a\0/tmp/b\0".to_vec());
    }

    /// A remote path has no native form, so what comes out is the wire form —
    /// which is what the tool asked norte to pick.
    #[test]
    fn pick_bytes_of_a_remote_path_is_its_wire_form() {
        let p = VPath::parse("sftp://host/x").unwrap();
        assert_eq!(pick_bytes(&[p]), b"sftp://host/x\0".to_vec());
    }

    /// Nothing selected is an empty output, never a stray NUL: a consumer
    /// that reads one empty record would open the current directory.
    #[test]
    fn pick_bytes_of_nothing_is_nothing() {
        assert!(pick_bytes(&[]).is_empty());
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `just t norte-frontend`
Expected: FAIL, `cannot find function pick_bytes`.

- [ ] **Step 3: Write `pick_bytes`**

`crates/norte-frontend/src/shell.rs` starts here. Module doc: this is the pure
half of shell integration; nothing in it touches a terminal or spawns anything,
so all of it is unit-testable.

```rust
/// The picker's output: every path's bytes, each followed by a NUL.
///
/// A local path comes out in NATIVE form (`/tmp/a`), because that is what the
/// tool on the other side of the pipe will open. Anything else comes out as
/// its wire form (`sftp://host/x`), which is the only lossless thing to say
/// about it.
#[must_use]
pub fn pick_bytes(paths: &[norte_proto::VPath]) -> Vec<u8>;
```

The native conversion is `norte_vfs_local::vpath_to_native`; on `Err`, fall
back to `VPath::to_wire()`. On unix take the bytes with
`std::os::unix::ffi::OsStrExt::as_bytes`; on Windows use
`to_string_lossy().into_owned().into_bytes()` and say so in the rustdoc — a
Windows picker is not part of this item.

If `norte-frontend` does not already depend on `norte-vfs-local`, add it to
`crates/norte-frontend/Cargo.toml` and justify it in the commit body: the
conversion is one function and the alternative is duplicating scheme logic in
two frontends.

- [ ] **Step 4: Run the test**

Run: `just t norte-frontend`
Expected: PASS, three tests.

- [ ] **Step 5: Add the command to the catalogue**

In `crates/norte-frontend/src/keymap/catalogue.rs`, in the `app` block:

```rust
    live("app.pick-accept", false),
```

Then in `crates/norte-tui/src/keymap.rs`, inside `commands!`:

```rust
    "app.pick-accept" => AppPickAccept,
```

The suite forces a Fluent description for every catalogue command
(`help_id`, `keymap.rs:207`), so add to **both** locales:

```ftl
# crates/norte-i18n/i18n/en.ftl
help-cmd-app-pick-accept = accept the selection and exit (picker mode)
# crates/norte-i18n/i18n/es.ftl
help-cmd-app-pick-accept = aceptar la selección y salir (modo picker)
```

- [ ] **Step 6: Run the keymap suites**

Run: `just t norte-frontend` then `just t norte-tui`
Expected: PASS. A missing locale string fails here, loudly — that is the gate
doing its job, not a surprise.

- [ ] **Step 7: Wire the flag and the exit**

In `crates/norte-tui/src/main.rs`:

```rust
const BOOL_FLAGS: &[&str] = &["--daemon", "--pick"];
```

Add to `USAGE` (English, no Fluent — it prints before the language is
negotiated):

```
      --pick             print the selection, NUL-terminated, and exit
```

Then:
- Thread `pick: bool` into `App` (or into `run`'s arguments alongside
  `confirm_quit`; follow whichever the surrounding code already does for
  start-up settings).
- Dispatch arm: `Command::AppPickAccept` sets `app.quit = true` and records
  `app.picked = Some(app.focused().marked_paths())`. `marked_paths` already
  falls back to the cursor entry (`pane.rs:840` and the test at
  `app.rs:5919`), so the "selection if any, else cursor" rule needs no new
  code. Outside picker mode the arm is a no-op.
- Bindings: `Enter` maps to `app.pick-accept` only when picker mode is on AND
  the cursor is not a directory; otherwise `Enter` stays `nav.enter`. Resolve
  this where the key is turned into a command, not in the keymap file — a
  preset must not have to know about `--pick`. `Ctrl+Enter` maps to
  `app.pick-accept` unconditionally in picker mode.
- After the run loop returns, before `tty::restore`: if `app.picked` is
  `Some`, write `pick_bytes(&paths)` to stdout, flush, exit 0. If picker mode
  was on and `app.picked` is `None`, exit 1. Anything else, exit as today.

- [ ] **Step 8: Write the boundary test**

`crates/norte-tui/tests/pick.rs` (new file):

```rust
//! What `--pick` puts on stdout, decided without a terminal: the App state
//! goes in, the bytes come out.

use norte_frontend::shell::pick_bytes;
use norte_proto::VPath;

#[test]
fn a_cancelled_pick_writes_nothing() {
    let picked: Option<Vec<VPath>> = None;
    let out = picked.map(|p| pick_bytes(&p)).unwrap_or_default();
    assert!(out.is_empty(), "cancelling must not name a file");
}

#[test]
fn an_accepted_pick_is_nul_terminated() {
    let picked = vec![VPath::parse("file:///tmp/x").unwrap()];
    assert_eq!(pick_bytes(&picked), b"/tmp/x\0".to_vec());
}
```

- [ ] **Step 9: Run tests and lint**

Run: `just t norte-tui` then `just c`
Expected: both green.

- [ ] **Step 10: Commit**

```bash
git add crates/norte-frontend/src/shell.rs crates/norte-frontend/src/lib.rs \
        crates/norte-frontend/src/keymap/catalogue.rs crates/norte-tui/src/keymap.rs \
        crates/norte-tui/src/main.rs crates/norte-tui/tests/pick.rs \
        crates/norte-i18n/i18n/en.ftl crates/norte-i18n/i18n/es.ftl
git commit -m "feat(tui): a picker other tools can pipe"
```

---

## Task 3: cd-on-quit

**Files:**
- Modify: `crates/norte-frontend/src/shell.rs` (`cd_bytes`, `Shell`, wrappers)
- Modify: `crates/norte-tui/src/main.rs` (`VALUE_FLAGS`, `USAGE`, the write on quit)
- Modify: `crates/norte-cli/src/main.rs` (`shell-init` subcommand)
- Modify: `crates/norte-cli/src/doctor.rs`
- Create: `crates/norte-cli/tests/shell_init_e2e.rs`
- Modify: `crates/norte-i18n/i18n/{en,es}.ftl`

- [ ] **Step 1: Write the failing tests**

Append to `crates/norte-frontend/src/shell.rs`'s test module:

```rust
    /// A directory is bytes and the file is NUL-delimited, so a name ending
    /// in a newline survives — `$(...)` would eat it, which is why the
    /// wrapper reads a file instead.
    #[test]
    fn cd_bytes_are_the_raw_directory_plus_a_nul() {
        let p = VPath::parse("file:///tmp/we%0Aird").unwrap();
        assert_eq!(cd_bytes(&p), Some(b"/tmp/we\nird\0".to_vec()));
    }

    /// Not `file://` writes NOTHING: there is no local cwd that corresponds
    /// to `sftp://host/x`, and an empty file is the wrapper's signal to leave
    /// the shell where it is.
    #[test]
    fn cd_bytes_of_a_remote_pane_are_nothing() {
        assert_eq!(cd_bytes(&VPath::parse("sftp://host/x").unwrap()), None);
        assert_eq!(cd_bytes(&VPath::parse("s3://b/k").unwrap()), None);
    }

    /// The wrapper must call the BINARY, not itself. A function named `ntc`
    /// that runs `ntc` is infinite recursion, and it is the one way this can
    /// fail catastrophically — so it is pinned for all three shells.
    #[test]
    fn every_wrapper_calls_the_binary_not_the_function() {
        for sh in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let w = sh.wrapper();
            assert!(w.contains("command ntc"), "{sh:?} recurses: {w}");
        }
    }

    /// No wrapper may read the cd-file with command substitution: it cannot
    /// carry a NUL and it strips trailing newlines.
    #[test]
    fn no_wrapper_uses_command_substitution_on_the_cd_file() {
        for sh in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let w = sh.wrapper();
            assert!(!w.contains("$(cat"), "{sh:?} uses $(cat …)");
            assert!(!w.contains("(cat "), "{sh:?} uses (cat …)");
        }
    }
```

- [ ] **Step 2: Run and watch them fail**

Run: `just t norte-frontend`
Expected: FAIL, `cannot find function cd_bytes` / `cannot find type Shell`.

- [ ] **Step 3: Implement**

```rust
/// What to write into the `--cd-file`, or `None` when the pane is not local.
///
/// `Some` is the directory's native bytes with a trailing NUL. `None` means
/// write nothing at all — an empty file tells the wrapper to leave the shell
/// where it is, and a norte that died mid-write can therefore never move a
/// shell to half a path.
#[must_use]
pub fn cd_bytes(dir: &norte_proto::VPath) -> Option<Vec<u8>>;

/// A shell we can emit a cd-on-quit wrapper for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    /// bash.
    Bash,
    /// zsh.
    Zsh,
    /// fish.
    Fish,
}

impl Shell {
    /// `"bash"`/`"zsh"`/`"fish"`, else `None`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self>;

    /// The wrapper's source, for `eval`.
    #[must_use]
    pub fn wrapper(self) -> &'static str;
}
```

The wrapper text, verbatim. bash and zsh share it:

```sh
ntc() {
    local f
    f="$(mktemp "${TMPDIR:-/tmp}/ntc-cd.XXXXXX")" || return 1
    command ntc --cd-file "$f" "$@"
    local status=$?
    local dir
    IFS= read -r -d '' dir < "$f"
    rm -f -- "$f"
    if [ -n "$dir" ]; then
        cd -- "$dir" || return $?
    fi
    return $status
}
```

fish:

```fish
function ntc
    set -l f (mktemp (test -n "$TMPDIR"; and echo $TMPDIR; or echo /tmp)/ntc-cd.XXXXXX)
    or return 1
    command ntc --cd-file $f $argv
    set -l status_code $status
    set -l dir (string split0 < $f)
    rm -f -- $f
    if test -n "$dir[1]"
        cd -- $dir[1]
    end
    return $status_code
end
```

- [ ] **Step 4: Run the tests**

Run: `just t norte-frontend`
Expected: PASS.

- [ ] **Step 5: Add `--cd-file` to the TUI**

```rust
const VALUE_FLAGS: &[&str] = &["--preset", "--socket", "--cd-file"];
```

`USAGE` gains:

```
      --cd-file PATH     write the final directory here, NUL-terminated
                         (used by the `norte shell-init` wrapper)
```

On a clean quit, after the run loop and before restoring the terminal: if
`--cd-file` was given, `cd_bytes(app.focused().dir())`; `Some(bytes)` is
written with `File::create` (truncating); `None` writes nothing and prints one
line to stderr from Fluent:

```ftl
# en.ftl
msg-cd-not-local = the active pane was { $path }; the shell stays put
# es.ftl
msg-cd-not-local = el pane activo era { $path }; el shell se queda donde está
```

Pass the path through the frontend's existing hostile-name sanitiser
(`path_display`) before interpolating it — a directory name can carry control
bytes, and this line lands in the user's shell after norte has released the
terminal.

- [ ] **Step 6: Add `norte shell-init`**

In `crates/norte-cli/src/main.rs`, a new `Cmd` variant:

```rust
    /// Prints the cd-on-quit wrapper for a shell, to be `eval`ed
    ShellInit {
        /// bash, zsh or fish
        shell: String,
    },
```

The handler writes `Shell::parse(&shell)`'s `wrapper()` to stdout, or exits
with a message naming the three supported shells. Add usage lines to
`crates/norte-cli/src/help.rs` where the other subcommands document themselves.

- [ ] **Step 7: The wrappers run for real**

`crates/norte-cli/tests/shell_init_e2e.rs`:

```rust
//! The wrappers are shell code, so they are tested by running the shell.
//! A stub `ntc` on PATH stands in for the binary: it writes hostile bytes to
//! the cd-file, which is exactly the contract the wrapper has to survive.

use std::io::Write;
use std::process::Command;

fn shell_available(sh: &str) -> bool {
    Command::new(sh)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Builds a temp dir holding: a stub `ntc` that writes `<target>\0` to the
/// path given by `--cd-file`, and the target directory itself.
fn stub_env(tmp: &std::path::Path, target: &str) {
    let bin = tmp.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let stub = bin.join("ntc");
    let mut f = std::fs::File::create(&stub).unwrap();
    // `$2` is the cd-file: the wrapper always passes `--cd-file <path>` first.
    writeln!(f, "#!/bin/sh\nprintf '%s\\0' \"{target}\" > \"$2\"").unwrap();
    drop(f);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::create_dir_all(target).unwrap();
}

#[test]
fn bash_wrapper_lands_in_a_directory_whose_name_is_hostile() {
    if !shell_available("bash") {
        eprintln!("skip: no bash on this machine");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    // A space and a newline: `$(...)` would destroy the second one.
    let target = tmp.path().join("we ird\ndir");
    stub_env(tmp.path(), target.to_str().unwrap());
    let script = format!(
        "{}\nntc\npwd",
        norte_frontend::shell::Shell::Bash.wrapper()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("PATH", format!("{}/bin:{}", tmp.path().display(), std::env::var("PATH").unwrap()))
        .output()
        .unwrap();
    let pwd = String::from_utf8_lossy(&out.stdout);
    assert!(
        pwd.trim_end().ends_with("dir"),
        "the wrapper did not cd: {pwd:?} / {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
```

Repeat the same test for `zsh` (identical body, `Shell::Zsh`, `zsh -c`) and for
`fish` (`Shell::Fish`, `fish -c`). Write them out — do not loop over the three
inside one test, because a loop reports the first failure and hides the rest,
and these three fail for different reasons.

Add `tempfile` to `crates/norte-cli`'s dev-dependencies if it is not there.

- [ ] **Step 8: Doctor reports it**

In `crates/norte-cli/src/doctor.rs`, one check: is `ntc` a shell function in
the caller's environment? It cannot be detected from a child process, so report
honestly what CAN be known — whether `shell-init` output exists in the user's
rc file is not knowable either. Print the instruction instead:

```ftl
# en.ftl
doctor-shell-init = cd-on-quit: run `eval "$(norte shell-init bash)"` (or zsh/fish) from your shell's rc file
# es.ftl
doctor-shell-init = cd-on-quit: añade `eval "$(norte shell-init bash)"` (o zsh/fish) al rc de tu shell
```

- [ ] **Step 9: Run tests and lint**

Run: `just t norte-frontend`, `just t norte-cli`, `just c`
Expected: all green. The zsh/fish tests print a skip line if the shell is
absent — read the output and confirm at least bash actually ran.

- [ ] **Step 10: Commit**

```bash
git add crates/norte-frontend/src/shell.rs crates/norte-tui/src/main.rs \
        crates/norte-cli/src/main.rs crates/norte-cli/src/help.rs \
        crates/norte-cli/src/doctor.rs crates/norte-cli/tests/shell_init_e2e.rs \
        crates/norte-cli/Cargo.toml crates/norte-i18n/i18n/en.ftl crates/norte-i18n/i18n/es.ftl
git commit -m "feat(cli,tui): the shell follows you out"
```

- [ ] **Step 11: Gate checkpoint**

Three tasks done. Run `just ci-fast` — ONE run, per the plan budget in
CLAUDE.md. Fix everything it finds in one pass; do not re-run it per finding.

---

## Task 4: the three keys, and the GUI's terminal

**Files:**
- Modify: `crates/norte-frontend/src/shell.rs` (`login_shell`, `terminal_argv`)
- Modify: `crates/norte-frontend/src/keymap/catalogue.rs:190-192` (three `planned` → `live`)
- Modify: `crates/norte-tui/src/keymap.rs` (three `Command` variants)
- Modify: `crates/norte-tui/src/main.rs:7841` (`run_opener` → `run_suspended`) and the dispatch table
- Modify: `crates/norte-gui/src/main.rs` (its command set + the launcher)
- Modify: `crates/norte-i18n/i18n/{en,es}.ftl`

- [ ] **Step 1: Write the failing tests**

Append to `crates/norte-frontend/src/shell.rs`'s test module:

```rust
    /// `$SHELL` wins; without it, something that certainly exists. Never an
    /// empty argv — `Command::new("")` is a confusing spawn error much later.
    #[test]
    fn login_shell_falls_back_to_a_real_shell() {
        // SAFETY-free: this only reads the value the caller passes.
        assert_eq!(login_shell_from(Some("/bin/zsh".as_ref())), std::path::PathBuf::from("/bin/zsh"));
        assert!(!login_shell_from(None).as_os_str().is_empty());
    }

    /// The GUI has no host terminal to suspend, so it launches one. `$TERMINAL`
    /// is the user's explicit answer and beats every probe.
    #[test]
    fn terminal_argv_prefers_the_configured_terminal() {
        let dir = std::path::Path::new("/tmp/x");
        let argv = terminal_argv_from(Some("kitty".as_ref()), dir).expect("configured");
        assert_eq!(argv[0], std::ffi::OsString::from("kitty"));
    }
```

Note the `_from` suffixes: the pure functions take the environment as an
argument so they are testable, and thin `login_shell()` / `terminal_argv(dir)`
wrappers read `std::env::var_os` and call them. Do not read the environment
inside the tested function — a test that sets a process-wide env var races
every other test in the binary.

- [ ] **Step 2: Run and watch them fail**

Run: `just t norte-frontend`
Expected: FAIL, `cannot find function login_shell_from`.

- [ ] **Step 3: Implement**

```rust
/// The user's shell: `$SHELL`, else `/bin/sh` (`%COMSPEC%`, else `cmd.exe`, on
/// Windows).
#[must_use]
pub fn login_shell() -> std::path::PathBuf;

/// Testable core of [`login_shell`].
#[must_use]
pub fn login_shell_from(env_shell: Option<&std::ffi::OsStr>) -> std::path::PathBuf;

/// argv that opens a terminal emulator with `dir` as its working directory, for
/// a frontend that cannot suspend. `None` when nothing plausible was found.
#[must_use]
pub fn terminal_argv(dir: &std::path::Path) -> Option<Vec<std::ffi::OsString>>;

/// Testable core of [`terminal_argv`].
#[must_use]
pub fn terminal_argv_from(
    env_terminal: Option<&std::ffi::OsStr>,
    dir: &std::path::Path,
) -> Option<Vec<std::ffi::OsString>>;
```

`terminal_argv_from`'s order on unix: `$TERMINAL`, then `xdg-terminal-exec`,
then the short probe list `["ghostty", "kitty", "alacritty", "wezterm",
"konsole", "gnome-terminal", "xterm"]`. macOS: `open -a Terminal <dir>`.
Windows: `wt` then `cmd`. Return the argv; the caller probes the PATH with
`norte_frontend::openers::program_available` in `spawn_blocking` (rule 2) —
this function does no I/O.

- [ ] **Step 4: Run the tests**

Run: `just t norte-frontend`
Expected: PASS.

- [ ] **Step 5: Generalise `run_opener`**

`crates/norte-tui/src/main.rs:7841`. Keep every line of the existing
restore-always structure; change only the shape:

```rust
/// Suspends the TUI, runs `argv` with inherited stdio in `cwd`, and restores
/// the terminal on EVERY path. `wait_for_key` holds the host terminal visible
/// until the user presses something, which is what makes the output of a
/// command readable before the panels come back. An empty `argv` shows the
/// host terminal and waits — that is `app.toggle-panels`.
async fn run_suspended(
    terminal: &mut norte_tui::tty::Tui,
    capture: &mut mouse::Capture,
    argv: Vec<std::ffi::OsString>,
    cwd: Option<std::path::PathBuf>,
    wait_for_key: bool,
) -> std::io::Result<Option<std::process::ExitStatus>>;
```

`run_opener`'s three existing callers keep working by calling
`run_suspended(term, cap, argv, Some(dir), false)`. The child gets
`.current_dir(cwd)` when `cwd` is `Some`, and `.env("NORTE_LEVEL", n + 1)`
where `n` is the current value parsed as `u32`, defaulting to 0.

- [ ] **Step 6: Three dispatch arms**

`crates/norte-tui/src/keymap.rs`, inside `commands!`:

```rust
    "app.terminal" => AppTerminal,
    "app.toggle-panels" => AppTogglePanels,
    "pane.command-line" => PaneCommandLine,
```

In `dispatch` (`main.rs:8219`), following the `Command::PaneOpen` pattern
(`:8469`) — resolve here, launch in the run loop, because the run loop owns the
terminal:

- `Command::AppTerminal`: `vpath_to_native(app.focused().dir())`; on `Err`,
  `app.message = Some(t("msg-shell-remote"))` and stop. On `Ok(dir)`, set
  `app.pending_shell = Some(PendingShell { argv: vec![login_shell().into()],
  cwd: Some(dir), wait_for_key: false })`.
- `Command::AppTogglePanels`: `app.pending_shell = Some(PendingShell { argv:
  vec![], cwd: None, wait_for_key: true })`. Works on a remote pane — it shows
  the host terminal and touches nothing.
- `Command::PaneCommandLine`: opens the bottom-row prompt. Only on `file://`;
  otherwise the same `msg-shell-remote`. On `Enter`, `PendingShell { argv:
  vec![login_shell().into(), "-c".into(), cmd.into()], cwd: Some(dir),
  wait_for_key: true }`.

`PendingShell` is a new struct next to `PendingOpen` (`app.rs:691`), with the
same rustdoc reasoning. After `run_suspended` returns, the run loop refreshes
the active pane through the same path `pane.refresh` uses (`main.rs:8387`).

Strings:

```ftl
# en.ftl
msg-shell-remote = the active pane is { $path }; a shell there would not be where you are looking
help-cmd-app-terminal = open a shell in the active pane's directory
help-cmd-app-toggle-panels = hide the panels and show the terminal
help-cmd-pane-command-line = run a command in the active pane's directory
# es.ftl
msg-shell-remote = el pane activo es { $path }; un shell ahí no estaría donde estás mirando
help-cmd-app-terminal = abrir un shell en el directorio del pane activo
help-cmd-app-toggle-panels = ocultar los paneles y enseñar la terminal
help-cmd-pane-command-line = ejecutar un comando en el directorio del pane activo
```

- [ ] **Step 7: Flip the catalogue**

`crates/norte-frontend/src/keymap/catalogue.rs:190-192`:

```rust
    live("app.terminal", false),
    live("app.toggle-panels", false),
    live("pane.command-line", false),
```

The reason strings `keymap-reason-shell` may now be unused; remove them from
both `.ftl` files only if no other command references them (`rg
keymap-reason-shell`).

- [ ] **Step 8: Test the suspension**

`crates/norte-tui/tests/suspend.rs` (new). The terminal cannot be exercised
under nextest, so what is tested is the DECISION and the child handling:

```rust
//! Suspension restores the terminal on every path, including the paths that
//! are easy to forget: a child that fails, and a child killed by a signal.

#[test]
fn a_remote_pane_refuses_to_open_a_shell() {
    let p = norte_proto::VPath::parse("sftp://host/x").unwrap();
    assert!(
        norte_vfs_local::vpath_to_native(&p).is_err(),
        "the refusal is this conversion failing; if it ever succeeds, the \
         dispatch arm silently opens a shell in the wrong place"
    );
}

#[test]
fn a_local_pane_yields_the_cwd_the_shell_gets() {
    let p = norte_proto::VPath::parse("file:///tmp").unwrap();
    assert_eq!(
        norte_vfs_local::vpath_to_native(&p).unwrap(),
        std::path::PathBuf::from("/tmp")
    );
}
```

For the restore-always behaviour, add a `#[cfg(test)]` unit test next to
`run_suspended` that calls it with `argv = ["false"]` and asserts it returns
`Ok(Some(status))` with `!status.success()` — the point is that a failing child
does not short-circuit the restore, and the function returning at all proves
it. Guard it with the same tty availability check the module already needs; if
there is no terminal, print a skip line rather than failing.

- [ ] **Step 9: The GUI's terminal**

`crates/norte-gui/src/main.rs`: add `app.terminal` to the GUI's implemented
command set and handle it by `terminal_argv(dir)` + a detached spawn (the GUI
never suspends). `None` from `terminal_argv`, or a binary missing from PATH, is
a message naming what was tried — the same shape as
`msg-open-missing-program`. Do NOT add `app.toggle-panels` or
`pane.command-line`: leaving them out is what makes them resolve to
`Availability::NotHere` (`keymap/effective.rs:38`), which is the truth and is
already rendered greyed by the reference sheet.

Run the GUI's own gate: `just gui-ci`.

- [ ] **Step 10: Run tests and lint**

Run: `just t norte-frontend`, `just t norte-tui`, `just c`
Expected: green.

- [ ] **Step 11: Reviewers, before committing**

Dispatch, per the review table in CLAUDE.md — this diff spawns processes with
inherited stdio and writes user-controlled bytes to a file the shell will
`cd` into:

- `security-reviewer`: the spawn surface (inherited stdio, `NORTE_LEVEL`, the
  cd-file's permissions and truncation, what a hostile directory name can do to
  a wrapper), and specifically: can anything other than norte write the
  cd-file between `mktemp` and the read?
- `encoding-auditor`: `pick_bytes` and `cd_bytes` against the hostile corpus —
  a name with a newline, with `0xFF`, with a NUL-adjacent lookalike.
- `rust-reviewer`: the whole diff.

Tell each what you chose and what you are unsure about, not "review this".
Apply BLOCKER and MAJOR; say which MINORs you skipped.

- [ ] **Step 12: Commit**

```bash
git add crates/norte-frontend/src/shell.rs crates/norte-frontend/src/keymap/catalogue.rs \
        crates/norte-tui/src/keymap.rs crates/norte-tui/src/main.rs crates/norte-tui/src/app.rs \
        crates/norte-tui/tests/suspend.rs crates/norte-gui/src/main.rs \
        crates/norte-i18n/i18n/en.ftl crates/norte-i18n/i18n/es.ftl
git commit -m "feat(tui,gui): three keys that stopped being dark (#135)"
```

- [ ] **Step 13: Close the branch**

Run `just ci` — ONE run. Then update
`docs/superpowers/specs/2026-08-07-post-alpha-roadmap.md` to mark item 4 built,
in the shape items 2 and 5 already use, and close #135 with a note saying what
was built by suspension and what was deliberately not (the persistent
subshell). File the subshell issue and link it from the `app.toggle-panels`
rustdoc.

---

## Notes for whoever executes this

- **The gate is billed per plan.** `just t <crate>` freely; `just ci-fast` once
  at the Task 3 checkpoint; `just ci` once at the end. Never re-run the gate to
  see whether a fix worked — reproduce the single failure with `just t`.
- **`NORTE_LEVEL` is not decoration.** A user who opens a shell from norte and
  then runs `ntc` in it has two of them; without the marker the second quit
  looks like the first one failing.
- **The wrapper is the dangerous file in this plan.** It runs in the user's
  interactive shell, before norte exists in the process tree. The two pinned
  tests (no recursion, no command substitution) are the ones to keep if
  anything ever has to be dropped.
