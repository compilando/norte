# User Commands Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user bind a key to `run = "user:<id>"` that runs a program declared in `~/.config/norte/commands.toml` on the selection (marks, or the cursor file), in the TUI and in the window, without writing a plugin.

**Architecture:** All the deciding is shared code in `norte-frontend` (ADR 0077): a new `user_commands` module parses `commands.toml`, turns "id + pane folder + targets" into a `Launch` (or a `Refusal` with a reason), resolves the program on `PATH`, and runs background commands (output to `tracing`, which feeds the log panel). The keymap accepts `user:<id>` beside `lua:` and uses a `USER_HOST` marker. Each frontend adds three things: a dispatch arm, a confirm dialog, and a terminal-panel launch. Background runs go through a `JoinSet` in the TUI, and through a new `NativeEffect` that the Tauri shell executes in the window. The search box gets a "User" row source. No protocol change.

**Tech Stack:** Rust 2024, serde/toml, tokio (`process`, `io-util` added to `norte-frontend`), Fluent (`crates/norte-i18n/i18n/{en,es}.ftl`), nextest via `just t`.

**Spec:** `/tmp/claude-1000/-home-oscar-work-wot-projects-high-getnorte-web/46f24dcf-65a3-4fef-97e6-717f92cd1b04/scratchpad/2026-10-10-user-commands-design.md`

**Repo / branch:** `/home/oscar/work/wot/projects/high/norte`, the main tree. Before Task 1 run `git switch -c feat/user-commands` (from `main`). Another worktree (`norte-wt-omnibox`) runs a different plan, so do not touch it.

## Global Constraints

- Branch `feat/user-commands` in `/home/oscar/work/wot/projects/high/norte`.
- Tests:
  - Use `just t <crate>`, never bare `cargo test` or `cargo nextest`.
  - `just c` if you touched lint surface.
  - `cargo test -p <crate> --doc` after adding or changing a public item or a field of a public struct.
  - `cargo doc -p <crate> --no-deps` after writing an intra-doc link.
- Gate budget:
  - `just ci-fast` exactly once after Task 5 and once after Task 9.
  - The final push of `main` is the last gate, and the hook runs it.
  - Run `just gui-ci` before pushing if Task 9 touched `crates/norte-gui-tauri/**`.
- Never `sleep` or `tail -f /dev/null`. Nothing will notify you; run commands in the foreground.
- Editing files through Bash is blocked: use Edit/Write. Use `rg`, not `grep`.
- Commits:
  - Conventional Commits, written to a file and committed with `git commit -F <file>`.
  - Every message ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
  - Run `git diff --cached --stat` before every commit.
- Hard rule 1: file names are bytes.
  - Paths travel as `PathBuf`/`OsString`.
  - `String` only for display, through `to_string_lossy`.
  - No `to_str().unwrap()`.
- Hard rule 2: no blocking I/O in async code. The `PATH` probe (`resolve_program`) runs inside `spawn_blocking` in the TUI. The window host may call it synchronously, as `argv_resolved` already does (`controller/effects.rs:530`).
- Hard rule 6: no `unwrap`/`expect` outside tests unless a comment states the invariant.
- No shell is ever implicit. `argv[0]` is a program and field codes are whole argv elements (`openers::expand_argv`).
- `commands.toml` is read from every layer EXCEPT `Layer::Project`, the same as `load_shell_profiles`. A higher layer's entry overrides a lower one with the same `id`.
- Id charset `[a-z0-9][a-z0-9-]{0,63}`.
- User commands are NEVER in the MCP tool surface and never reachable from an agent connection.
- User-facing text goes through Fluent, EN **and** ES, and lowercase terse status lines match the existing `msg-*` style.
- ADR: the next free number is **0177** (`docs/adr/0177-user-commands-run-programs-on-a-key.md`), and it gets a row in `docs/adr/README.md`.
- One explanation, one home:
  - The ADR holds the why.
  - The help topic holds the how-to.
  - The CHANGELOG holds what changed for the reader.
  - Rustdoc points at `(ADR 0177)`.

## Review Focus

1. **Cursor on the `..` row with no marks.** `marked_paths()` must not hand the parent folder to `%f`. Expected: "nothing selected" refusal. The test is in Task 6, `parent_row_is_not_a_target`.
2. **A background command that prints more than a pipe buffer.** For example `yes | head -c 300000`. Expected: it finishes; norte drains both pipes concurrently and never deadlocks. The test is in Task 4, `large_output_does_not_block`.
3. **A file named `-rf` or with non-UTF-8 bytes in the selection.** Expected: it arrives as one absolute-path argv element, bytes intact, never as a flag. The test is in Task 2, `hostile_names_are_absolute_whole_elements`.
4. **A keymap binding `user:x` whose id `commands.toml` does not define (deleted, typo).** Expected: the keymap still loads (not fatal), the key says "no user command x", and `norte doctor` warns. The tests are in Task 3: `unknown_user_id_loads_and_is_listed` and `doctor_warns_on_undefined_user_command`.
5. **A project `./.norte/keymap.toml` binding `j` to `user:<an id the user has>`.** Expected: discarded like a project `lua:` binding, and counted in the warning. The test is in Task 3, `project_user_binding_is_discarded`.

## Spec contradictions found (and the adjustment this plan makes)

- **C1, field codes `%n` and `%%` do not exist.**
  - `openers::expand_argv` (openers.rs:186) handles only `%f`, `%F` and `%d`.
  - `%n` (a bare file name) would also break the expander's security contract: "every field code is a single argv element AND never starts with `-`". A file named `-rf` would become a flag.
  - **Adjustment:** ship `%f`, `%F` and `%d` only. Any token that is exactly `%` plus one other character is a LOAD error ("unknown field code"), so nobody gets a silent literal `%n`. The help topic shows the `sh -c '… "$(basename "$1")"' sh %f` idiom.
- **C2, the processes panel lists the daemon task board, not OS children** (`norte_frontend::processes`, TUI `app.board`).
  - **Adjustment:** background commands are not listed there.
  - Their start ("… started") and exit go to the status bar, and their output goes to the log panel (via `tracing`, `target: "norte::user_command"`).
  - The help topic documents that quitting norte does not kill them, and that their output after norte quits is lost.
- **C3, an unknown id cannot be "reported like an unknown command".**
  - `KeymapError::UnknownCommand` is FATAL in `Effective::build_for`: the whole reload is refused.
  - `lua:` itself only validates the charset at load and warns at press.
  - **Adjustment:**
    - A malformed `user:` id is a load error, Structural, same as `lua:`.
    - A well-formed id that `commands.toml` does not define LOADS.
    - It is reported in three places: a status-bar warning at load and reload (both frontends), a `norte doctor` Warn finding (`keymap-unknown-user-cmd`), and the refusal "no user command <id>" when pressed.
    - This also keeps a broken or edited `commands.toml` from taking the keymap down.
- **C4, "user layer only" versus the pattern it cites.** `load_shell_profiles` (ADR 0176) skips only `Layer::Project`; System and Profile are allowed. **Adjustment:** follow the cited pattern: every layer but Project, with a higher layer overriding by id. A configuration profile carrying its own commands is the expected use.
- **C5, `load_user_commands(layers) -> (Vec<UserCommand>, Vec<Diagnostic>)`.** Every other per-layer loader returns `Result<Option<_>, ConfigError>` naming the culprit file, and reload is all-or-nothing (ADR 0007). **Adjustment:** same shape as `load_shell_profiles`. A broken `commands.toml` is a config error naming the file, and the session keeps its previous config.
- **C6, the window has no config file watcher.** It reloads only on a profile switch or a settings write (`controller/profiles.rs:177 apply_config`). **Adjustment:**
  - `commands.toml` joins `CONFIG_FILES` (TUI hot reload) as specified.
  - The window picks it up at the next `apply_config` or restart. The help topic says so.
- **C7, bridge bump.** `UiAction::UserCommandFinished` is native-only, like `UiAction::ProgramFinished`, which never went through `types.ts`. **Adjustment:** no `BRIDGE_VERSION` bump expected. Task 9 checks the precedent with `git log -S ProgramFinished -- crates/norte-ui-host/src/bridge.rs`, and bumps to 109 only if the golden/contract tests require it.
- **C8, F1 "shows the entry's description".** `goto::help_topic` returns `None` for non-catalogue keys and the frontends then say `msg-palette-no-help`. **Adjustment:** a shared `goto::user_command_f1` returns the description, or "no description", as a status message, checked before `help_topic` in both frontends.
- **Not a contradiction, but noted:** `plugin.run_command` (`norte-core/src/daemon/server.rs:3115`) is open to agent connections. That is out of scope here, because user commands get no daemon method at all.

## File Structure

| file | responsibility |
| --- | --- |
| `crates/norte-frontend/src/user_commands.rs` (new) | `commands.toml` model and validation; `request` (id + dir + targets → `Launch`/`Refusal`); `resolve`; `run_background` + `Finished`; `undefined_bindings` |
| `crates/norte-frontend/src/config.rs` | `load_user_commands`, `FrontendConfig.user_commands` |
| `crates/norte-frontend/src/keymap/{effective,layer,mod}.rs` | `USER_HOST`, `user:` validation, project discard |
| `crates/norte-frontend/src/navigation/goto.rs` | `SECTION_USER`, `user_command_rows`, `user_command_chords`, `user_command_f1` |
| `crates/norte-frontend/Cargo.toml` | tokio features `process`, `io-util` |
| `crates/norte-config/src/watch.rs` | `commands.toml` in `CONFIG_FILES` |
| `crates/norte-cli/src/doctor.rs` | `keymap-unknown-user-cmd` finding |
| `crates/norte-tui/src/user_cmd.rs` (new) | TUI glue: `request`, `start_pending`, `outcome_of`, `harvest` |
| `crates/norte-tui/src/{keys,event_loop,main,config_reload,shortcuts_editor,termpanel,goto,overlays}.rs`, `app.rs`, `app/{modal,dialogs}.rs`, `mutations.rs`, `ui/modals.rs`, `help_context.rs`, `hints.rs`, `jobs/inflight.rs` | wiring |
| `crates/norte-ui-host/src/controller/user_commands.rs` (new) | host glue: `run_user_command`, `launch_user_command`, `user_command_finished` |
| `crates/norte-ui-host/src/{keys,dto,action}.rs`, `controller/{input,palette,dialogs,mod,termpanel,goto}.rs` | wiring |
| `crates/norte-gui-tauri/src/native.rs` | executes `NativeEffect::RunUserCommand` |
| `crates/norte-mcp/src/bridge.rs` | never-exposed test |
| `crates/norte-i18n/i18n/{en,es}.ftl` | strings |
| `crates/norte-help/topics/{en,es}/user-commands.md`, `crates/norte-help/src/corpus.rs` | help topic |
| `docs/adr/0177-…md`, `docs/adr/README.md`, `CHANGELOG.md` | docs |

---

### Task 1: `commands.toml` model, loader and hot reload

**Files:**
- Create: `crates/norte-frontend/src/user_commands.rs`
- Modify: `crates/norte-frontend/src/lib.rs` (add `pub mod user_commands;` in the alphabetical `pub mod` list, after `pub mod timeline;`)
- Modify: `crates/norte-frontend/src/config.rs`:
  - `FrontendConfig` gets the field.
  - Add `load_user_commands` after `load_shell_profiles` (~:170).
  - `load` (~:175) calls it.
- Modify: `crates/norte-config/src/watch.rs:174` (`CONFIG_FILES`)
- Modify: `crates/norte-config/tests/watch.rs` (polling test)
- Modify: `crates/norte-ui-host/tests/config_coverage.rs:169` (destructure guard)

**Interfaces:**
- Produces:
  - `norte_frontend::user_commands::{UserCommands, UserCommand, UserCommandError, RunIn, Cwd, valid_user_id}`
  - `UserCommands::{empty, parse, get, iter, is_empty, overlay}`
  - `UserCommand::uses_targets(&self) -> bool`
  - `norte_frontend::config::load_user_commands(dir: &Path, kind: Layer, sources: &mut Vec<PathBuf>) -> Result<Option<UserCommands>, ConfigError>`
  - `FrontendConfig.user_commands: UserCommands`

- [ ] **Step 1: Write the failing tests** at the bottom of the new `user_commands.rs`. Write the module header and the types' skeleton first, so it compiles.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = r#"
[[command]]
id = "optimize-png"
label = "Optimize PNGs"
argv = ["oxipng", "-o", "4", "%F"]

[[command]]
id = "here"
label = "Open a terminal here"
description = "kitty in the pane's folder"
argv = ["kitty", "--directory", "%d"]
run_in = "terminal"
cwd = "/tmp"
confirm = true
refresh = false
"#;

    #[test]
    fn parses_defaults_and_overrides() {
        let c = UserCommands::parse(OK).expect("valid");
        let png = c.get("optimize-png").expect("defined");
        assert_eq!(png.label, "Optimize PNGs");
        assert_eq!(png.run_in, RunIn::Background);
        assert_eq!(png.cwd, Cwd::Pane);
        assert!(!png.confirm);
        assert!(png.refresh, "refresh defaults to true");
        assert!(png.uses_targets());
        let here = c.get("here").expect("defined");
        assert_eq!(here.run_in, RunIn::Terminal);
        assert_eq!(here.cwd, Cwd::Fixed(std::path::PathBuf::from("/tmp")));
        assert!(here.confirm && !here.refresh);
        assert_eq!(here.description.as_deref(), Some("kitty in the pane's folder"));
        assert!(!here.uses_targets(), "%d alone needs no selection");
    }

    #[test]
    fn id_charset() {
        for ok in ["a", "0", "optimize-png", &"a".repeat(64)] {
            assert!(valid_user_id(ok), "{ok}");
        }
        for bad in ["", "-a", "A", "a_b", "a.b", "a b", "ñ", &"a".repeat(65)] {
            assert!(!valid_user_id(bad), "{bad}");
        }
    }

    #[test]
    fn rejects_each_defect_by_name() {
        let cases: &[(&str, UserCommandError)] = &[
            ("[[command]]\nid = \"Bad\"\nlabel = \"x\"\nargv = [\"a\"]\n",
             UserCommandError::BadId { id: "Bad".into() }),
            ("[[command]]\nid = \"a\"\nlabel = \"x\"\nargv = []\n",
             UserCommandError::EmptyArgv { id: "a".into() }),
            ("[[command]]\nid = \"a\"\nlabel = \"x\"\nargv = [\"\"]\n",
             UserCommandError::EmptyArgv { id: "a".into() }),
            ("[[command]]\nid = \"a\"\nlabel = \"\"\nargv = [\"a\"]\n",
             UserCommandError::EmptyLabel { id: "a".into() }),
            ("[[command]]\nid = \"a\"\nlabel = \"x\"\nargv = [\"a\"]\ncwd = \"rel/dir\"\n",
             UserCommandError::BadCwd { id: "a".into() }),
            ("[[command]]\nid = \"a\"\nlabel = \"x\"\nargv = [\"a\", \"%n\"]\n",
             UserCommandError::BadFieldCode { id: "a".into(), code: "%n".into() }),
            ("[[command]]\nid = \"a\"\nlabel = \"x\"\nargv = [\"a\", \"%%\"]\n",
             UserCommandError::BadFieldCode { id: "a".into(), code: "%%".into() }),
            ("[[command]]\nid = \"a\"\nlabel = \"x\"\nargv = [\"a\"]\n[[command]]\nid = \"a\"\nlabel = \"y\"\nargv = [\"b\"]\n",
             UserCommandError::DuplicateId { id: "a".into() }),
        ];
        for (src, want) in cases {
            assert_eq!(UserCommands::parse(src).unwrap_err(), *want, "{src}");
        }
        assert!(matches!(
            UserCommands::parse("[[command]]\nid = \"a\"\nlabel = \"x\"\nargv = [\"a\"]\nshell = true\n"),
            Err(UserCommandError::Toml(_))
        ), "unknown keys are refused");
        assert!(matches!(
            UserCommands::parse("[[command]]\nid = \"a\"\nlabel = \"x\"\nargv = [\"a\"]\nrun_in = \"popup\"\n"),
            Err(UserCommandError::Toml(_))
        ));
    }

    #[test]
    fn a_literal_with_a_percent_inside_is_not_a_field_code() {
        // Only a WHOLE token `%x` is a code (openers.rs contract); `--q=%f`
        // and `100%` are literals and stay legal.
        let src = "[[command]]\nid = \"a\"\nlabel = \"x\"\nargv = [\"a\", \"--q=%f\", \"100%\"]\n";
        assert!(UserCommands::parse(src).is_ok());
    }

    #[test]
    fn a_higher_layer_overrides_by_id() {
        let mut low = UserCommands::parse(OK).unwrap();
        let high = UserCommands::parse(
            "[[command]]\nid = \"here\"\nlabel = \"Mine\"\nargv = [\"foot\"]\n",
        )
        .unwrap();
        low.overlay(high);
        assert_eq!(low.get("here").unwrap().label, "Mine");
        assert!(low.get("optimize-png").is_some(), "the rest survives");
        assert_eq!(low.iter().count(), 2);
    }
}
```

Also add these to `crates/norte-frontend/src/config.rs`'s test module (create `#[cfg(test)] mod tests` at the bottom if there is none):

```rust
#[test]
fn commands_toml_is_read_from_the_user_layer_and_never_from_the_project() {
    let user = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let body = "[[command]]\nid = \"a\"\nlabel = \"A\"\nargv = [\"true\"]\n";
    std::fs::write(user.path().join("commands.toml"), body).unwrap();
    std::fs::write(
        project.path().join("commands.toml"),
        "[[command]]\nid = \"pwn\"\nlabel = \"P\"\nargv = [\"rm\"]\n",
    )
    .unwrap();
    let cfg = load(&Layers {
        dirs: vec![
            (user.path().to_path_buf(), Layer::User),
            (project.path().to_path_buf(), Layer::Project),
        ],
    })
    .unwrap();
    assert!(cfg.user_commands.get("a").is_some());
    assert!(cfg.user_commands.get("pwn").is_none(), "a project may not choose programs");
    assert!(cfg.common.sources.iter().any(|p| p.ends_with("commands.toml")));
}

#[test]
fn a_broken_commands_toml_names_the_file() {
    let user = tempfile::tempdir().unwrap();
    std::fs::write(user.path().join("commands.toml"), "[[command]]\nid = \"X\"\n").unwrap();
    let err = load(&Layers { dirs: vec![(user.path().to_path_buf(), Layer::User)] }).unwrap_err();
    assert!(err.to_string().contains("commands.toml"), "{err}");
}
```

(If `tempfile` is not yet a dev-dependency of `norte-frontend`, add `tempfile.workspace = true` under `[dev-dependencies]`. Check with `rg -n tempfile crates/norte-frontend/Cargo.toml`.)

In `crates/norte-config/tests/watch.rs`, copy `polling_detects_changes_in_openers_toml` (:71) into `polling_detects_changes_in_commands_toml`, replacing every `"openers.toml"` with `"commands.toml"`.

- [ ] **Step 2: Run to verify they fail**

Run: `just t norte-frontend` and then `just t norte-config`
Expected: FAIL. The frontend tests fail because `parse` returns the stub; the watch test times out with "the poll sees the … change".

- [ ] **Step 3: Implement.** In `user_commands.rs`, write the module rustdoc: a short what-it-is, the TOML example from the spec, and a pointer to `(ADR 0177)`. Then:

```rust
use std::path::PathBuf;
use serde::Deserialize;

/// What is wrong with a `commands.toml`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UserCommandError {
    /// It does not parse, or has a key nobody reads.
    #[error("{0}")]
    Toml(String),
    /// `id` outside `[a-z0-9][a-z0-9-]{0,63}`.
    #[error("user command {id:?}: the id must be lowercase letters, digits and dashes, 64 at most")]
    BadId { /// The id as written.
        id: String },
    /// No program to run.
    #[error("user command {id:?}: `argv` needs a program")]
    EmptyArgv { /// The command.
        id: String },
    /// An empty `label`: the box would show a blank row.
    #[error("user command {id:?}: `label` cannot be empty")]
    EmptyLabel { /// The command.
        id: String },
    /// `cwd` neither `"pane"` nor absolute.
    #[error("user command {id:?}: `cwd` must be \"pane\" or an absolute path")]
    BadCwd { /// The command.
        id: String },
    /// A whole `%x` token norte does not expand (only `%f`, `%F`, `%d`).
    #[error("user command {id:?}: unknown field code {code:?} (norte expands %f, %F and %d)")]
    BadFieldCode { /// The command.
        id: String, /// The token.
        code: String },
    /// The same id twice in ONE file.
    #[error("user command {id:?} is defined twice in the same file")]
    DuplicateId { /// The id.
        id: String },
}

/// Where a command runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunIn {
    /// Detached; output to the log panel, exit in the status bar.
    #[default]
    Background,
    /// A new entry in the terminal panel (ADR 0176).
    Terminal,
}

/// The working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cwd {
    /// The focused pane's folder.
    Pane,
    /// This absolute path.
    Fixed(PathBuf),
}

/// One validated `[[command]]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserCommand {
    /// `user:<id>` in the keymap.
    pub id: String,
    /// The box's row text.
    pub label: String,
    /// What F1 on its row says.
    pub description: Option<String>,
    /// Program and argument template (`%f`, `%F`, `%d`).
    pub argv: Vec<String>,
    /// Working directory.
    pub cwd: Cwd,
    /// Background or terminal.
    pub run_in: RunIn,
    /// Ask first.
    pub confirm: bool,
    /// Re-list the panes when a background run exits.
    pub refresh: bool,
}

/// Every command norte knows, in declaration order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserCommands {
    cmds: Vec<UserCommand>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    command: Vec<RawCommand>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCommand {
    id: String,
    label: String,
    argv: Vec<String>,
    description: Option<String>,
    cwd: Option<String>,
    run_in: Option<RawRunIn>,
    confirm: Option<bool>,
    refresh: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawRunIn {
    Background,
    Terminal,
}

/// The `user:<id>` charset, `[a-z0-9][a-z0-9-]{0,63}`. SINGLE SOURCE: the
/// keymap validates `user:` bindings with it (`keymap::effective`).
#[must_use]
pub fn valid_user_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

impl UserCommands {
    /// No commands (no `commands.toml` anywhere).
    #[must_use]
    pub fn empty() -> Self { Self::default() }

    /// Parses and validates one layer's `commands.toml`.
    ///
    /// # Errors
    /// [`UserCommandError`] naming the command at fault.
    pub fn parse(s: &str) -> Result<Self, UserCommandError> {
        let raw: RawFile =
            toml::from_str(s).map_err(|e| UserCommandError::Toml(e.message().to_owned()))?;
        let mut cmds: Vec<UserCommand> = Vec::with_capacity(raw.command.len());
        for r in raw.command {
            let id = r.id;
            if !valid_user_id(&id) { return Err(UserCommandError::BadId { id }); }
            if cmds.iter().any(|c| c.id == id) { return Err(UserCommandError::DuplicateId { id }); }
            if r.label.trim().is_empty() { return Err(UserCommandError::EmptyLabel { id }); }
            if r.argv.first().is_none_or(String::is_empty) { return Err(UserCommandError::EmptyArgv { id }); }
            if let Some(code) = r.argv.iter().skip(1).find(|t| {
                t.len() == 2 && t.starts_with('%') && !matches!(t.as_str(), "%f" | "%F" | "%d")
            }) {
                return Err(UserCommandError::BadFieldCode { id, code: code.clone() });
            }
            let cwd = match r.cwd.as_deref() {
                None | Some("pane") => Cwd::Pane,
                Some(p) if std::path::Path::new(p).is_absolute() => Cwd::Fixed(PathBuf::from(p)),
                Some(_) => return Err(UserCommandError::BadCwd { id }),
            };
            cmds.push(UserCommand {
                id,
                label: r.label,
                description: r.description,
                argv: r.argv,
                cwd,
                run_in: match r.run_in { Some(RawRunIn::Terminal) => RunIn::Terminal, _ => RunIn::Background },
                confirm: r.confirm.unwrap_or(false),
                refresh: r.refresh.unwrap_or(true),
            });
        }
        Ok(Self { cmds })
    }

    /// The command with this id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&UserCommand> { self.cmds.iter().find(|c| c.id == id) }

    /// All of them, in order.
    pub fn iter(&self) -> impl Iterator<Item = &UserCommand> { self.cmds.iter() }

    /// No commands.
    #[must_use]
    pub fn is_empty(&self) -> bool { self.cmds.is_empty() }

    /// Lays a HIGHER layer on top: same id → the higher one replaces it in place;
    /// new ids are appended.
    pub fn overlay(&mut self, higher: UserCommands) {
        for c in higher.cmds {
            match self.cmds.iter_mut().find(|m| m.id == c.id) {
                Some(slot) => *slot = c,
                None => self.cmds.push(c),
            }
        }
    }
}

impl UserCommand {
    /// Does the template need a selection (`%f`/`%F`)?
    #[must_use]
    pub fn uses_targets(&self) -> bool {
        self.argv.iter().skip(1).any(|t| t == "%f" || t == "%F")
    }
}
```

Add a doctest on `UserCommands::parse` (parse the spec's example and `assert!(c.get("optimize-png").is_some())`).

In `config.rs`, add the field after `shell_profiles`, rustdoc'd: "The user's commands (`commands.toml`, ADR 0177): every layer but the project one, which may not choose programs norte runs." Add the loader:

```rust
/// Loads `commands.toml` for a layer; `None` if absent or the layer is
/// PROJECT — a user command names a program norte runs (ADR 0177, same rule
/// as [`load_openers`]).
///
/// # Errors
/// [`ConfigError::Toml`] naming the culprit file if it is invalid.
pub fn load_user_commands(
    dir: &Path,
    kind: Layer,
    sources: &mut Vec<PathBuf>,
) -> Result<Option<UserCommands>, ConfigError> {
    if kind == Layer::Project {
        return Ok(None);
    }
    let path = dir.join("commands.toml");
    let Some(raw) = read_optional(&path)? else {
        return Ok(None);
    };
    let parsed = UserCommands::parse(&raw).map_err(|e| ConfigError::Toml {
        path: path.clone(),
        message: e.to_string(),
    })?;
    sources.push(path);
    Ok(Some(parsed))
}
```

In `load`, declare `let mut user_commands = UserCommands::empty();`. Inside the layer loop add `if let Some(parsed) = load_user_commands(dir, *kind, &mut common.sources)? { user_commands.overlay(parsed); }`, and set the field in the struct literal.

In `watch.rs`:
- Change `CONFIG_FILES` to `[&str; 4] = ["norte.toml", "keymap.toml", "openers.toml", "commands.toml"]`.
- Fix `snapshot`'s comment, which mentions openers only, so it says "every file in `CONFIG_FILES`".

In `config_coverage.rs`, add to the destructure:

```rust
        // ─── The host reads it: `user:<id>` keys and the box's User rows
        //     run these (ADR 0177), same as the terminal.
        user_commands: _,
```

- [ ] **Step 4: Run tests**

Run: `just t norte-frontend`, `just t norte-config`, `cargo test -p norte-frontend --doc`, `cargo check -p norte-ui-host --all-targets`
Expected: PASS / compiles.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-frontend crates/norte-config crates/norte-ui-host/tests/config_coverage.rs
git diff --cached --stat
git commit -F /tmp/msg   # feat(frontend): read commands.toml, never from a project
```

---

### Task 2: `request` and `resolve`, a command turned into a launch or a reason

**Files:**
- Modify: `crates/norte-frontend/src/user_commands.rs`
- Modify: `crates/norte-i18n/i18n/en.ftl`, `crates/norte-i18n/i18n/es.ftl` (new `user-cmd-*` block, placed after the `cmd-ns-*` block, ~:847)

**Interfaces:**
- Consumes: Task 1 types.
- Produces:
  - `pub const ARGV_BUDGET: usize`
  - `pub struct Launch { pub id: String, pub label: String, pub program: OsString, pub args: Vec<OsString>, pub cwd: PathBuf, pub run_in: RunIn, pub refresh: bool, pub confirm: bool, pub count: usize }` (derive `Debug, Clone, PartialEq, Eq`)
  - `pub enum Refusal { Unknown { id: String }, Remote, NoTargets { label: String }, TooLong { label: String, count: usize }, Missing { program: String } }` (same derives)
  - `Refusal::key(&self) -> &'static str`, `Refusal::args(&self) -> Vec<(&'static str, String)>`, `Refusal::message(&self, lang: norte_i18n::Lang) -> String`
  - `pub fn request(cmds: &UserCommands, id: &str, dir: &VPath, targets: &[VPath]) -> Result<Launch, Refusal>`
  - `pub fn resolve(launch: &Launch) -> Result<PathBuf, Refusal>`, `pub fn resolve_in(launch: &Launch, path_var: Option<&OsStr>) -> Result<PathBuf, Refusal>`
  - `pub fn confirm_text(launch: &Launch, lang: Lang) -> String`

- [ ] **Step 1: Failing tests** (append to `user_commands::tests`):

```rust
    use norte_proto::VPath;

    fn vp(s: &str) -> VPath { VPath::parse(s).expect("vpath") }
    fn cmds(src: &str) -> UserCommands { UserCommands::parse(src).expect("valid") }
    const PNG: &str = "[[command]]\nid = \"png\"\nlabel = \"Optimize\"\nargv = [\"oxipng\", \"-o\", \"4\", \"%F\"]\n";

    #[test]
    fn every_field_code_expands_as_whole_elements() {
        let c = cmds("[[command]]\nid = \"x\"\nlabel = \"X\"\nargv = [\"p\", \"%f\", \"--\", \"%F\", \"%d\"]\n");
        let l = request(&c, "x", &vp("file:///d"), &[vp("file:///d/a"), vp("file:///d/b")]).unwrap();
        assert_eq!(l.program, std::ffi::OsString::from("p"));
        let args: Vec<_> = l.args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["/d/a", "--", "/d/a", "/d/b", "/d"]);
        assert_eq!(l.cwd, std::path::PathBuf::from("/d"));
        assert_eq!(l.count, 2);
        assert!(l.refresh && !l.confirm);
    }

    #[test]
    fn fixed_cwd_wins_but_percent_d_is_still_the_pane() {
        let c = cmds("[[command]]\nid = \"x\"\nlabel = \"X\"\nargv = [\"p\", \"%d\"]\ncwd = \"/srv\"\n");
        let l = request(&c, "x", &vp("file:///d"), &[]).unwrap();
        assert_eq!(l.cwd, std::path::PathBuf::from("/srv"));
        assert_eq!(l.args, [std::ffi::OsString::from("/d")]);
        assert_eq!(l.count, 0, "%d-only runs with no selection");
    }

    #[test]
    fn refusals() {
        let c = cmds(PNG);
        assert_eq!(request(&c, "nope", &vp("file:///d"), &[]), Err(Refusal::Unknown { id: "nope".into() }));
        assert_eq!(request(&c, "png", &vp("sftp://h/d"), &[vp("sftp://h/d/a")]), Err(Refusal::Remote));
        assert_eq!(request(&c, "png", &vp("file:///d"), &[]), Err(Refusal::NoTargets { label: "Optimize".into() }));
    }

    #[test]
    fn too_many_targets_are_refused_with_the_count() {
        let c = cmds(PNG);
        let name = "n".repeat(200);
        let many: Vec<VPath> = (0..4000).map(|i| vp(&format!("file:///d/{name}{i}"))).collect();
        assert_eq!(
            request(&c, "png", &vp("file:///d"), &many),
            Err(Refusal::TooLong { label: "Optimize".into(), count: 4000 })
        );
    }

    #[cfg(unix)]
    #[test]
    fn hostile_names_are_absolute_whole_elements() {
        use std::os::unix::ffi::OsStrExt;
        let c = cmds(PNG);
        let dir = vp("file:///d");
        let dash = norte_vfs::native::vpath_from_native(std::path::Path::new("/d/-rf")).unwrap();
        let bytes = norte_vfs::native::vpath_from_native(std::path::Path::new(
            std::ffi::OsStr::from_bytes(b"/d/caf\xe9 name"),
        ))
        .unwrap();
        let l = request(&c, "png", &dir, &[dash, bytes]).unwrap();
        assert_eq!(l.args[2].as_bytes(), b"/d/-rf", "a path, never a flag");
        assert_eq!(l.args[3].as_bytes(), b"/d/caf\xe9 name", "bytes intact, spaces kept in ONE element");
    }

    #[cfg(unix)]
    #[test]
    fn resolve_finds_on_path_or_says_missing() {
        let c = cmds("[[command]]\nid = \"x\"\nlabel = \"X\"\nargv = [\"sh\"]\n");
        let l = request(&c, "x", &vp("file:///"), &[]).unwrap();
        let p = resolve_in(&l, Some(std::ffi::OsStr::new("/bin:/usr/bin"))).expect("sh exists");
        assert!(p.is_absolute());
        assert_eq!(
            resolve_in(&l, Some(std::ffi::OsStr::new("/nonexistent"))),
            Err(Refusal::Missing { program: "sh".into() })
        );
    }

    #[test]
    fn every_message_key_exists_in_both_languages() {
        for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
            let ids = norte_i18n::message_ids(lang);
            for k in [
                "user-cmd-unknown", "user-cmd-remote", "user-cmd-no-targets",
                "user-cmd-too-long", "user-cmd-missing", "user-cmd-confirm",
                "user-cmd-confirm-title", "user-cmd-started", "user-cmd-ok",
                "user-cmd-failed", "user-cmd-signalled", "user-cmd-not-started",
                "user-cmd-no-description", "msg-user-cmd-undefined", "cmd-ns-user",
            ] {
                assert!(ids.iter().any(|i| i == k), "{k} missing in {lang:?}");
            }
        }
    }
```

(Check `vpath_from_native`'s exact path/name with `rg -n "pub fn vpath_from_native" crates/norte-vfs/src`. The TUI exploration found it as the reverse of `vpath_to_native`.)

- [ ] **Step 2: Run, verify FAIL.** `just t norte-frontend` → compile errors on `request`/`Launch`.

- [ ] **Step 3: Implement.**

```rust
/// Bytes an expanded argv may take before norte refuses it. Half of macOS's
/// `ARG_MAX` (1 MiB, which includes the environment); Linux allows more.
pub const ARGV_BUDGET: usize = 512 * 1024;

/// A command ready to start: program as written (resolution is
/// [`resolve`]'s, off the async thread), arguments expanded byte-safely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch { /* fields as in Interfaces, each with a one-line doc */ }

/// Why a command did not start. `key`/`args` feed Fluent, and every
/// interpolated value is untrusted text, masked by [`Refusal::args`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal { /* as in Interfaces, one-line docs */ }

impl Refusal {
    /// Its Fluent key.
    #[must_use]
    pub fn key(&self) -> &'static str {
        match self {
            Self::Unknown { .. } => "user-cmd-unknown",
            Self::Remote => "user-cmd-remote",
            Self::NoTargets { .. } => "user-cmd-no-targets",
            Self::TooLong { .. } => "user-cmd-too-long",
            Self::Missing { .. } => "user-cmd-missing",
        }
    }
    /// Its Fluent arguments, masked for a terminal.
    #[must_use]
    pub fn args(&self) -> Vec<(&'static str, String)> {
        let m = |s: &str| norte_encoding::mask_terminal_hazards(s);
        match self {
            Self::Unknown { id } => vec![("id", m(id))],
            Self::Remote => vec![],
            Self::NoTargets { label } => vec![("label", m(label))],
            Self::TooLong { label, count } => vec![("label", m(label)), ("count", count.to_string())],
            Self::Missing { program } => vec![("program", m(program))],
        }
    }
    /// The status-bar line.
    #[must_use]
    pub fn message(&self, lang: norte_i18n::Lang) -> String {
        let args = self.args();
        let refs: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
        norte_i18n::ta_in(lang, self.key(), &refs)
    }
}

/// `id` + the focused pane + its targets (marks, or the cursor) → a launch,
/// or why not. Pure: no disk.
///
/// # Errors
/// [`Refusal`] — unknown id, a remote pane, no selection for a `%f`/`%F`
/// template, or an argv over [`ARGV_BUDGET`].
pub fn request(cmds: &UserCommands, id: &str, dir: &VPath, targets: &[VPath]) -> Result<Launch, Refusal> {
    use norte_vfs::native::vpath_to_native;
    let cmd = cmds.get(id).ok_or_else(|| Refusal::Unknown { id: id.to_owned() })?;
    let dir_native = vpath_to_native(dir).map_err(|_| Refusal::Remote)?;
    let files: Vec<PathBuf> = targets
        .iter()
        .map(vpath_to_native)
        .collect::<Result<_, _>>()
        .map_err(|_| Refusal::Remote)?;
    if cmd.uses_targets() && files.is_empty() {
        return Err(Refusal::NoTargets { label: cmd.label.clone() });
    }
    let refs: Vec<&std::path::Path> = files.iter().map(PathBuf::as_path).collect();
    let mut argv = crate::openers::expand_argv(&cmd.argv, &refs, &dir_native);
    if argv.iter().map(|a| a.len() + 1).sum::<usize>() > ARGV_BUDGET {
        return Err(Refusal::TooLong { label: cmd.label.clone(), count: files.len() });
    }
    // `parse` refused an empty argv, so `expand_argv` always returns the program.
    let program = argv.remove(0);
    Ok(Launch {
        id: cmd.id.clone(),
        label: cmd.label.clone(),
        program,
        args: argv,
        cwd: match &cmd.cwd { Cwd::Pane => dir_native, Cwd::Fixed(p) => p.clone() },
        run_in: cmd.run_in,
        refresh: cmd.refresh,
        confirm: cmd.confirm,
        count: if cmd.uses_targets() { files.len() } else { 0 },
    })
}
```

`argv.remove(0)` panics on an empty Vec. The invariant comment above covers rule 6; or use `if argv.is_empty() { return Err(Refusal::Missing{..}) }` before it. Pick the explicit guard.

`resolve_in` wraps `crate::openers::resolve_program_in(&launch.program, path_var)` and turns `None` into `Refusal::Missing { program: launch.program.to_string_lossy().into_owned() }`. `resolve` passes `std::env::var_os("PATH").as_deref()`, and its rustdoc says "blocking: stats the disk — call it from `spawn_blocking` in async code".

`confirm_text`: `ta_in(lang, "user-cmd-confirm", &[("label", masked label), ("count", count)])`.

Fluent, EN (lowercase house style):

```ftl
# User commands (commands.toml, ADR 0177)
cmd-ns-user = User
user-cmd-unknown = no user command named { $id } in commands.toml
user-cmd-remote = user commands run on local folders
user-cmd-no-targets = { $label }: nothing selected
user-cmd-too-long = { $label }: { $count } items do not fit on one command line
user-cmd-missing = { $program } not found
user-cmd-confirm-title = Run command
user-cmd-confirm = { $count ->
    [0] Run { $label }?
    [one] Run { $label } on 1 item?
   *[other] Run { $label } on { $count } items?
}
user-cmd-started = { $label } started
user-cmd-ok = { $label } finished
user-cmd-failed = { $label } exited with code { $code }
user-cmd-signalled = { $label } was ended by a signal
user-cmd-not-started = { $label } could not start: { $error }
user-cmd-no-description = { $label } has no description
msg-user-cmd-undefined = { $n } key(s) run user commands commands.toml does not define ({ $first })
```

ES:

```ftl
# Comandos de usuario (commands.toml, ADR 0177)
cmd-ns-user = Usuario
user-cmd-unknown = no hay ningún comando de usuario { $id } en commands.toml
user-cmd-remote = los comandos de usuario se ejecutan en carpetas locales
user-cmd-no-targets = { $label }: no hay nada seleccionado
user-cmd-too-long = { $label }: { $count } elementos no caben en una línea de órdenes
user-cmd-missing = no se encuentra { $program }
user-cmd-confirm-title = Ejecutar comando
user-cmd-confirm = { $count ->
    [0] ¿Ejecutar { $label }?
    [one] ¿Ejecutar { $label } sobre 1 elemento?
   *[other] ¿Ejecutar { $label } sobre { $count } elementos?
}
user-cmd-started = { $label } en marcha
user-cmd-ok = { $label } terminó
user-cmd-failed = { $label } salió con código { $code }
user-cmd-signalled = { $label } terminó por una señal
user-cmd-not-started = { $label } no pudo arrancar: { $error }
user-cmd-no-description = { $label } no tiene descripción
msg-user-cmd-undefined = { $n } tecla(s) ejecutan comandos de usuario que commands.toml no define ({ $first })
```

- [ ] **Step 4: Run.** `just t norte-frontend`, `just t norte-i18n` (parity guards), `cargo test -p norte-frontend --doc`. Expected: PASS.

- [ ] **Step 5: Commit.** `feat(frontend): turn a user command into a launch or a reason`.

---

### Task 3: the keymap accepts `user:<id>`; doctor warns on undefined ids

**Files:**
- Modify: `crates/norte-frontend/src/keymap/effective.rs`:
  - `USER_HOST` goes beside `LUA_HOST` (~:126).
  - `check_binding` (~:181).
  - The `build_diagnostics` guard (~:456).
- Modify: `crates/norte-frontend/src/keymap/layer.rs:397` (project discard)
- Modify: `crates/norte-frontend/src/keymap/mod.rs:28` (re-export), plus tests
- Modify: `crates/norte-frontend/src/user_commands.rs` (`undefined_bindings`)
- Modify: `crates/norte-i18n/i18n/{en,es}.ftl` (`msg-lua-keymap-project` now covers both)
- Modify: `crates/norte-cli/src/doctor.rs` (`check_keymaps`, ~:295-345)

**Interfaces:**
- Produces:
  - `norte_frontend::keymap::USER_HOST: &str = "user:*"`
  - `norte_frontend::user_commands::undefined_bindings(eff: &Effective, cmds: &UserCommands) -> Vec<String>` (sorted, deduped ids)
  - doctor code `keymap-unknown-user-cmd` (Warn)

- [ ] **Step 1: Failing tests** in `keymap/mod.rs` tests, next to `prefixed_lua_passes_command_validation` (~:890):

```rust
    #[test]
    fn user_binding_resolves_with_a_host_and_not_without() {
        let preset = r#"
            [pane]
            keymap = [{ on = ["x"], run = "user:optimize-png" }]
        "#;
        let kf = parse_keymap(preset).unwrap();
        let eff = Effective::build(&kf, None, &[USER_HOST]).expect("valid id loads");
        let mut r = Resolver::new(eff);
        assert_eq!(r.push(parse_chord("x").unwrap()), run("user:optimize-png"));

        let without = eff(preset, None).expect("loads without a host too");
        let mut r = Resolver::new(without);
        assert_eq!(
            r.push(parse_chord("x").unwrap()),
            Resolution::Unavailable { command: "user:optimize-png".to_owned(), why: Availability::NotHere },
        );

        let long = format!("user:{}", "a".repeat(65));
        for bad in ["user:", "user:Bad", "user:a_b", "user:-a", long.as_str()] {
            let src = format!("[pane]\nkeymap = [{{ on = [\"x\"], run = \"{bad}\" }}]\n");
            let kf = parse_keymap(&src).unwrap();
            assert!(Effective::build(&kf, None, &[USER_HOST]).is_err(), "{bad} is a load error");
            let d = Effective::build_diagnostics(&kf, &[], &[USER_HOST], Screen::Browse);
            assert!(matches!(d.as_slice(), [KeymapDiagnostic::Structural { .. }]), "{bad}: {d:?}");
        }
    }

    #[test]
    fn project_user_binding_is_discarded() {
        const COMMANDS: &[&str] = &["cursor.down", USER_HOST];
        let preset = parse_keymap("[pane]\nkeymap = [{ on = [\"j\"], run = \"cursor.down\" }]\n").unwrap();
        let mut project = parse_keymap("[pane]\nprepend_keymap = [{ on = [\"j\"], run = \"user:wipe\" }]\n").unwrap();
        project.mark_project();
        let eff = Effective::build_layered(&preset, std::slice::from_ref(&project), COMMANDS).unwrap();
        assert_eq!(eff.discarded_lua_bindings(), 1, "counted with the lua: ones");
        let mut r = Resolver::new(eff);
        assert_eq!(r.push(parse_chord("j").unwrap()), run("cursor.down"));
    }
```

In `user_commands::tests`:

```rust
    #[test]
    fn unknown_user_id_loads_and_is_listed() {
        let kf = crate::keymap::parse_keymap(
            "[pane]\nkeymap = [{ on = [\"x\"], run = \"user:png\" }, { on = [\"y\"], run = \"user:gone\" }, { on = [\"z\"], run = \"user:gone\" }]\n",
        ).unwrap();
        let eff = crate::keymap::Effective::build(&kf, None, &[crate::keymap::USER_HOST]).expect("not fatal");
        assert_eq!(undefined_bindings(&eff, &cmds(PNG)), ["gone"]);
    }
```

In `doctor.rs` tests (find the module that tests `check_keymaps`; `rg -n "fn .*keymap" crates/norte-cli/src/doctor.rs`), add `doctor_warns_on_undefined_user_command`:
1. Write a user layer dir with `keymap.toml` = `[pane]\nprepend_keymap = [{ on = ["ctrl+alt+o"], run = "user:gone" }]` and no `commands.toml`.
2. Call `check_keymaps(&layers)`.
3. Assert that one finding has `code == "keymap-unknown-user-cmd"`, `severity == Severity::Warn`, and `detail` containing `gone`.
4. Assert that no finding has `severity == Severity::Error`.

- [ ] **Step 2: Run, FAIL.** `just t norte-frontend`, `just t norte-cli`.

- [ ] **Step 3: Implement.**

In `effective.rs`:

```rust
/// The entry a frontend adds to its `known_commands` to say it RUNS user
/// commands (ADR 0177). Same contract as [`LUA_HOST`]: without it a
/// well-formed `user:<id>` binding loads but is [`Availability::NotHere`].
/// Not a valid id (`*`), so no binding collides with it.
pub const USER_HOST: &str = "user:*";
```

In `check_binding`, add an arm before the `else if known_commands.contains(..)`:

```rust
    } else if let Some(id) = raw.run.strip_prefix("user:") {
        // The id is validated, never looked up: commands.toml is loaded
        // apart and may change under a running keymap (ADR 0177). An id it
        // does not define loads, and says so when pressed and in doctor.
        if !crate::user_commands::valid_user_id(id) {
            return Err(KeymapError::UnknownCommand { run: raw.run.clone() });
        }
        if known_commands.contains(&USER_HOST) { Availability::Here } else { Availability::NotHere }
```

In `build_diagnostics`, the recoverable guard becomes:

```rust
                Err(KeymapError::UnknownCommand { run })
                    if run.strip_prefix("lua:").is_none_or(valid_lua_name)
                        && run.strip_prefix("user:").is_none_or(crate::user_commands::valid_user_id) =>
```

Update the comment under it so it says that a malformed `user:` id is structural for the same reason as `lua:`.

In `layer.rs` `merge_ctx`, use `if layer_project && (b.run.starts_with("lua:") || b.run.starts_with("user:"))`, and extend the rustdoc sentence to "`lua:` and `user:` bindings". Re-export `USER_HOST` in `keymap/mod.rs:28`.

Fluent: change `msg-lua-keymap-project`:
- EN: `project keymap.toml: { $n } lua:/user: binding(s) ignored (not trusted)`
- ES: `keymap.toml del proyecto: { $n } binding(s) lua:/user: ignorados (sin trust)`

`undefined_bindings`:

```rust
/// The ids of `user:` bindings in `eff` that `cmds` does not define —
/// sorted, once each. Reported at load (status bar) and by `norte doctor`.
#[must_use]
pub fn undefined_bindings(eff: &crate::keymap::Effective, cmds: &UserCommands) -> Vec<String> {
    let mut out: Vec<String> = eff
        .bindings_all()
        .into_iter()
        .filter_map(|(_, run, _)| run.strip_prefix("user:").map(str::to_owned))
        .filter(|id| cmds.get(id).is_none())
        .collect();
    out.sort();
    out.dedup();
    out
}
```

Doctor: in `check_keymaps`, after the per-screen loop, do the following:
1. Load commands with `norte_frontend::config::load_user_commands` over `layers.dirs`, overlaying as `config::load` does. On `Err`, push an Error finding `user-commands-parse` with the message masked by `norte_encoding::mask_terminal_hazards`.
2. Build the browse map with `Effective::build_for(&preset_kf, &layer_kfs, &[USER_HOST], Screen::Browse)`.
3. If that returns `Ok(eff)`, push one `Severity::Warn` finding per id from `undefined_bindings`: code `keymap-unknown-user-cmd`, detail `sanitize_detail(&format!("user:{id}"))`. The structural errors are already reported, so an `Err` adds nothing here.

Only the browse map is checked, because `[global]` merges into it and user commands make no sense in the viewer or dialogs.

- [ ] **Step 4: Run.** `just t norte-frontend`, `just t norte-cli`, `cargo test -p norte-frontend --doc`. If `norte-cli`'s golden changes, regenerate with `NORTE_UPDATE_GOLDEN=1 just t norte-cli` and read the diff.

- [ ] **Step 5: Commit.** `feat(keymap): bind keys to user:<id>; doctor names the undefined ones`.

---

### Task 4: run a background command, drain its output, report how it ended

**Files:**
- Modify: `crates/norte-frontend/Cargo.toml`: tokio features gain `"process", "io-util"`. Add a comment saying both frontends run user commands through one runner (ADR 0077/0177); `process` is std's spawn behind tokio; nothing new enters the graph.
- Modify: `crates/norte-frontend/src/user_commands.rs`

**Interfaces:**
- Produces:
  - `#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)] #[serde(tag = "kind", rename_all = "snake_case")] pub enum Finished { Ok, Failed { code: i32 }, Signalled, Missing { program: String }, NotStarted { error: String } }`
  - `Finished::key(&self) -> &'static str`, `Finished::args(&self, label: &str) -> Vec<(&'static str, String)>`, `Finished::message(&self, label: &str, lang) -> String`, `Finished::ran(&self) -> bool` (true for `Ok`/`Failed`/`Signalled`)
  - `pub async fn run_background(launch: Launch) -> Finished`
  - `pub const LOG_TARGET: &str = "norte::user_command"`

- [ ] **Step 1: Failing tests** (unix-only, real processes):

```rust
    #[cfg(unix)]
    fn sh(script: &str, dir: &std::path::Path) -> Launch {
        Launch {
            id: "t".into(), label: "T".into(),
            program: "sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: dir.to_path_buf(),
            run_in: RunIn::Background, refresh: true, confirm: false, count: 0,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exit_codes_are_reported() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(run_background(sh("exit 0", d.path())).await, Finished::Ok);
        assert_eq!(run_background(sh("exit 3", d.path())).await, Finished::Failed { code: 3 });
        assert_eq!(run_background(sh("kill -9 $$", d.path())).await, Finished::Signalled);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_in_its_cwd() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(run_background(sh("touch made-here", d.path())).await, Finished::Ok);
        assert!(d.path().join("made-here").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn large_output_does_not_block() {
        let d = tempfile::tempdir().unwrap();
        let f = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            run_background(sh("yes out | head -c 300000; yes err | head -c 300000 >&2", d.path())),
        )
        .await
        .expect("both pipes are drained");
        assert_eq!(f, Finished::Ok);
    }

    #[tokio::test]
    async fn a_missing_program_is_named() {
        let mut l = sh("", std::path::Path::new("/"));
        l.program = "norte-no-such-program-xyz".into();
        assert_eq!(run_background(l).await, Finished::Missing { program: "norte-no-such-program-xyz".into() });
    }

    #[test]
    fn finished_keys_and_ran() {
        assert_eq!(Finished::Failed { code: 2 }.key(), "user-cmd-failed");
        assert!(Finished::Signalled.ran() && !Finished::Missing { program: "x".into() }.ran());
    }
```

(`tokio` with `macros`/`rt` is already in the crate; if `#[tokio::test]` needs `rt` for dev, it is in the main dependency features.)

- [ ] **Step 2: Run, FAIL.** `just t norte-frontend`.

- [ ] **Step 3: Implement.**

```rust
/// Where background output goes: the log panel shows `tracing` events.
pub const LOG_TARGET: &str = "norte::user_command";

/// Runs a [`RunIn::Background`] launch to the end: resolves the program off
/// the async thread, spawns it with stdin closed, forwards each stdout line
/// as an INFO and each stderr line as a WARN event under [`LOG_TARGET`],
/// and reports how it ended. Not killed on drop: quitting norte leaves it
/// running (its output is then lost) — ADR 0177.
pub async fn run_background(launch: Launch) -> Finished {
    let probe = launch.clone();
    let program = match tokio::task::spawn_blocking(move || resolve(&probe)).await {
        Ok(Ok(p)) => p,
        Ok(Err(Refusal::Missing { program })) => return Finished::Missing { program },
        Ok(Err(other)) => return Finished::NotStarted { error: other.key().to_owned() },
        Err(e) => return Finished::NotStarted { error: e.to_string() },
    };
    let spawned = tokio::process::Command::new(&program)
        .args(&launch.args)
        .current_dir(&launch.cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => return Finished::NotStarted { error: e.to_string() },
    };
    tracing::info!(target: LOG_TARGET, command = %launch.id, "started");
    let out = forward(child.stdout.take(), launch.id.clone(), false);
    let err = forward(child.stderr.take(), launch.id.clone(), true);
    // Both pipes drain WHILE waiting: waiting first deadlocks a program
    // whose output fills the pipe.
    let (status, (), ()) = tokio::join!(child.wait(), out, err);
    match status {
        Ok(s) => match s.code() {
            Some(0) => Finished::Ok,
            Some(code) => Finished::Failed { code },
            None => Finished::Signalled,
        },
        Err(e) => Finished::NotStarted { error: e.to_string() },
    }
}

async fn forward(pipe: Option<impl tokio::io::AsyncRead + Unpin>, id: String, stderr: bool) {
    use tokio::io::AsyncBufReadExt;
    let Some(pipe) = pipe else { return };
    let mut lines = tokio::io::BufReader::new(pipe).split(b'\n');
    while let Ok(Some(line)) = lines.next_segment().await {
        let text = String::from_utf8_lossy(&line);
        if stderr {
            tracing::warn!(target: LOG_TARGET, command = %id, "{text}");
        } else {
            tracing::info!(target: LOG_TARGET, command = %id, "{text}");
        }
    }
}
```

`Finished::key`:
- `Ok` → `user-cmd-ok`
- `Failed` → `user-cmd-failed`
- `Signalled` → `user-cmd-signalled`
- `Missing` → `user-cmd-missing`
- `NotStarted` → `user-cmd-not-started`

`args` masks `label`, `program` and `error` with `norte_encoding::mask_terminal_hazards`, and adds `code` as a string. `message` mirrors `Refusal::message`.

- [ ] **Step 4: Run.** `just t norte-frontend`, `just c`. Expected: PASS, with no clippy warnings.

- [ ] **Step 5: Commit.** `feat(frontend): run a user command in the background and say how it ended`.

---

### Task 5: the search box's User rows and F1

**Files:**
- Modify: `crates/norte-frontend/src/navigation/goto.rs`:
  - `SECTION_USER` near `SECTION_PLUGINS` (:132).
  - The `refresh_flat` call (:417).
  - The new fns after `plugin_command_rows` (:923).
  - Tests at the bottom, including extending `every_catalogue_namespace_has_a_category_in_both_languages` (:1731) or adding a sibling.

**Interfaces:**
- Produces:
  - `pub const SECTION_USER: GotoSection`
  - `pub fn user_command_chords(cmds: &UserCommands, eff: &Effective) -> HashMap<String, String>` (key `user:<id>`)
  - `pub fn user_command_rows(cmds: &UserCommands, chords: &HashMap<String, String>, local: bool, lang: Lang) -> Vec<GotoRow>`
  - `pub fn user_command_f1(key: &str, cmds: &UserCommands, lang: Lang) -> Option<String>`

- [ ] **Step 1: Failing tests:**

```rust
    fn user_cmds() -> crate::user_commands::UserCommands {
        crate::user_commands::UserCommands::parse(
            "[[command]]\nid = \"png\"\nlabel = \"Optimize PNGs\"\ndescription = \"oxipng -o 4\"\nargv = [\"oxipng\", \"%F\"]\n\
             [[command]]\nid = \"bare\"\nlabel = \"Bare\"\nargv = [\"true\"]\n",
        ).unwrap()
    }

    #[test]
    fn user_rows_carry_label_id_category_and_key() {
        let mut chords = std::collections::HashMap::new();
        chords.insert("user:png".to_owned(), "ctrl+alt+o".to_owned());
        let rows = user_command_rows(&user_cmds(), &chords, true, norte_i18n::Lang::En);
        let png = &rows[0];
        assert_eq!(png.section, SECTION_USER.id);
        assert_eq!(png.key, "cmd:user:png");
        assert_eq!(png.text, "Optimize PNGs");
        assert_eq!(png.desc, "user:png");
        assert_eq!(png.category.as_deref(), Some("User"));
        assert_eq!(png.chord.as_deref(), Some("ctrl+alt+o"));
        assert!(png.unavailable.is_none());
        assert_eq!(action(&png.key), Action::Command("user:png".into()));
        let remote = user_command_rows(&user_cmds(), &chords, false, norte_i18n::Lang::Es);
        assert_eq!(remote[0].category.as_deref(), Some("Usuario"));
        assert!(remote[0].unavailable.is_some(), "dimmed on a remote pane, with the reason");
    }

    #[test]
    fn user_rows_show_under_the_command_prefix_only() {
        // Build a Goto with ONE FixedSource(SECTION_USER, user rows), the way
        // the existing `refresh_flat` tests build theirs (copy the nearest
        // test that constructs `Goto` with a `FixedSource`), then:
        //   query ">optim" → exactly one row, key "cmd:user:png";
        //   query "optim"  (places mode) → no row from SECTION_USER.
    }

    #[test]
    fn f1_on_a_user_row_says_its_description() {
        let c = user_cmds();
        assert_eq!(user_command_f1("cmd:user:png", &c, norte_i18n::Lang::En).as_deref(), Some("oxipng -o 4"));
        assert_eq!(user_command_f1("cmd:user:bare", &c, norte_i18n::Lang::En).as_deref(), Some("Bare has no description"));
        assert!(user_command_f1("cmd:pane.copy", &c, norte_i18n::Lang::En).is_none());
        assert_eq!(
            user_command_f1("cmd:user:gone", &c, norte_i18n::Lang::En).as_deref(),
            Some("no user command named gone in commands.toml"),
        );
    }
```

Fill the second test's body with real code modeled on the nearest existing `Goto` + `FixedSource` test in the same file. Find it with `rg -n "FixedSource::new" crates/norte-frontend/src/navigation/goto.rs`. The comment in it only states the two assertions; the body must contain them.

- [ ] **Step 2: Run, FAIL.** `just t norte-frontend`.

- [ ] **Step 3: Implement.**

```rust
/// The user's own commands (`commands.toml`, ADR 0177): under `>` only.
pub const SECTION_USER: GotoSection = GotoSection { id: "user", title_key: "goto-section-commands" };
```

Add `SECTION_USER.id` to the `Mode::Commands` slice at :417.

`user_command_rows`: for each command, build a `GotoRow` with:
- `section: SECTION_USER.id`
- `key: format!("{K_CMD}user:{}", c.id)`
- `text: c.label.clone()`
- `desc: format!("user:{}", c.id)`
- `category: Some(norte_i18n::t_in(lang, "cmd-ns-user"))`
- `chord: chords.get(&format!("user:{}", c.id)).cloned()`
- `unavailable: (!local).then(|| norte_i18n::t_in(lang, "user-cmd-remote"))`
- `hostile: true` (the label is file text, and masking clean text changes nothing)
- `..GotoRow::default()`

`user_command_chords` maps each `user:<id>` to `crate::palette::first_chord(&format!("user:{}", c.id), eff)`, keeping only the `Some` results.

`user_command_f1`:
1. Return `None` unless `key` starts with `cmd:user:`.
2. If the id is unknown, return `Refusal::Unknown{..}.message(lang)`.
3. Otherwise return the description masked by `norte_encoding::mask_terminal_hazards`, or `ta_in(lang, "user-cmd-no-description", label)`.

In the category test, add an explicit `cmd-ns-user` check for both langs (the catalogue loop does not see it).

- [ ] **Step 4: Run.** `just t norte-frontend`, then this plan's FIRST `just ci-fast` (Tasks 1-5). Expected: green. If it is red, reproduce the one failure with `just t <crate>`, fix it, and do not re-run the gate until the end of Task 9.

- [ ] **Step 5: Commit.** `feat(goto): a User category for the user's commands, F1 says what they do`.

---

### Task 6: TUI, running a user command (key, box, confirm, background, terminal)

**Files:**
- Create: `crates/norte-tui/src/user_cmd.rs` (add `mod user_cmd;` / `pub mod` in `crates/norte-tui/src/lib.rs` next to its siblings)
- Modify:
  - `crates/norte-tui/src/app.rs`: fields `user_commands: UserCommands`, `pending_user: Option<Launch>`, `user_chords: HashMap<String, String>`, near `openers` (:1145), initialised empty in `App::new`.
  - `crates/norte-tui/src/main.rs:340` and `crates/norte-tui/src/config_reload.rs:107`: copy `cfg.user_commands`.
  - `crates/norte-tui/src/shortcuts_editor.rs:67`: `known_commands` chains `USER_HOST` like `LUA_HOST`.
  - `crates/norte-tui/src/keys.rs`: the `Resolution::Run` arm (~:1337) and the box `Action::Command` (~:553).
  - `crates/norte-tui/src/jobs/inflight.rs:193`: `pub user_commands: tokio::task::JoinSet<crate::user_cmd::Outcome>`.
  - `crates/norte-tui/src/event_loop.rs`: top of the loop body in `run`, plus one `select!` arm near :734.
  - `crates/norte-tui/src/termpanel.rs:163`: `start_instance_in` and `start_user_command`.
  - `crates/norte-tui/src/app/modal.rs:100`: `Modal::ConfirmUserCommand { launch: Launch }`.
  - `crates/norte-tui/src/app/dialogs.rs:591`: allowlist arm.
  - `crates/norte-tui/src/mutations.rs:304`: `confirm_modal` arm.
  - `crates/norte-tui/src/ui/modals.rs:649`, `help_context.rs:74`, `hints.rs:191`: follow `ConfirmQuit`.

**Interfaces:**
- Consumes:
  - `user_commands::{request, resolve, run_background, Launch, Refusal, Finished, RunIn, confirm_text}` and `keymap::USER_HOST`.
  - `Pane::marked_paths()` (`app/pane.rs:700`) and `Pane::dir()`.
  - `refresh_panes` (`refresh.rs:416`), `after_panes_refresh` (`refresh.rs:533`), `overlays::watch_refresh_allowed` (`overlays.rs:456`).
- Produces:
  - `pub fn request(app: &mut App, id: &str)`
  - `pub fn start_pending(app: &mut App, work: &mut InFlight)`
  - `pub async fn outcome_of(launch: Launch) -> Outcome`
  - `pub enum Outcome { Finished { launch: Launch, finished: Finished }, Ready { launch: Launch, program: Result<PathBuf, Refusal> } }`
  - `pub fn harvest(app: &mut App, out: Outcome) -> bool` (true means re-list the panes)
  - `termpanel::start_user_command(app: &mut App, launch: &Launch, program: PathBuf) -> Result<(), String>`

- [ ] **Step 1: Failing tests** in `user_cmd.rs`. Use a real tempdir and a `file://` pane; see `gestures.rs:1997` for `tempfile` + `vpath_from_native`, and `app/testutil.rs` for `Pane::new`/`App::new`/`e(...)`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, Pane};
    use norte_frontend::user_commands::UserCommands;

    fn local_app(dir: &std::path::Path, names: &[&str]) -> App {
        // Pane::new(vpath_from_native(dir), entries for `names` as files) on
        // both sides — see gestures.rs `app_local` (:2196) for the entry
        // builder; mark nothing.
        todo_replace_with_the_gestures_rs_builder()
    }

    const CMDS: &str = "[[command]]\nid = \"touch\"\nlabel = \"Touch\"\nargv = [\"touch\", \"%F\"]\n\
        [[command]]\nid = \"ask\"\nlabel = \"Ask\"\nargv = [\"true\", \"%f\"]\nconfirm = true\n";

    #[test]
    fn a_key_queues_a_launch_on_the_cursor_file() {
        let d = tempfile::tempdir().unwrap();
        let mut app = local_app(d.path(), &["a.txt"]);
        app.user_commands = UserCommands::parse(CMDS).unwrap();
        request(&mut app, "touch");
        let l = app.pending_user.as_ref().expect("queued");
        assert_eq!(l.args, [d.path().join("a.txt").into_os_string()]);
        assert!(app.modal.is_none());
    }

    #[test]
    fn confirm_true_opens_the_question_instead() {
        let d = tempfile::tempdir().unwrap();
        let mut app = local_app(d.path(), &["a.txt"]);
        app.user_commands = UserCommands::parse(CMDS).unwrap();
        request(&mut app, "ask");
        assert!(app.pending_user.is_none());
        assert!(matches!(app.modal, Some(crate::app::Modal::ConfirmUserCommand { .. })));
    }

    #[test]
    fn refusals_go_to_the_bar() {
        let d = tempfile::tempdir().unwrap();
        let mut app = local_app(d.path(), &["a.txt"]);
        request(&mut app, "nope");
        assert!(app.message.as_deref().unwrap_or("").contains("nope"));
        assert!(app.pending_user.is_none());
    }

    #[test]
    fn parent_row_is_not_a_target() {
        // [ui] parent_entry on, cursor on `..`, no marks: `touch %F` must be
        // refused as "nothing selected", never run on the parent folder.
        let d = tempfile::tempdir().unwrap();
        let mut app = local_app(d.path(), &["a.txt"]);
        app.set_parent_row(true);
        // move the focused pane's cursor to row 0 (the `..` row) with the
        // pane's own cursor API (rg -n "fn set_cursor\|fn cursor_to" crates/norte-tui/src/app/pane.rs)
        app.user_commands = UserCommands::parse(CMDS).unwrap();
        request(&mut app, "touch");
        assert!(app.pending_user.is_none());
        assert!(app.message.is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_background_run_comes_back_and_asks_for_a_refresh() {
        let d = tempfile::tempdir().unwrap();
        let mut app = local_app(d.path(), &["a.txt"]);
        app.user_commands = UserCommands::parse(
            "[[command]]\nid = \"mk\"\nlabel = \"Make\"\nargv = [\"touch\", \"made\"]\n",
        ).unwrap();
        request(&mut app, "mk");
        let launch = app.pending_user.take().unwrap();
        let out = outcome_of(launch).await;
        assert!(harvest(&mut app, out), "refresh = true by default");
        assert!(d.path().join("made").exists());
        assert!(app.message.as_deref().unwrap_or("").contains("Make"));
    }
}
```

Write `local_app` with the real builder from `gestures.rs` (`app_local`, :2196, adapted to a tempdir VPath) before running. The test must not ship with a placeholder call. If `parent_row_is_not_a_target` turns out to pass only because `marked_paths` already skips `..`, keep it as the regression pin. If it fails, filter the parent row out of the targets in `request` (by comparing each target with `app.focused().dir().parent()`, or with the pane's parent-row predicate if `rg -n "parent_row\|is_parent" crates/norte-frontend/src/pane` finds one) and keep the test.

- [ ] **Step 2: Run, FAIL.** `just t norte-tui`.

- [ ] **Step 3: Implement.**

```rust
//! The TUI's half of user commands (ADR 0177): the decision is
//! `norte_frontend::user_commands`'; this only routes it.

use std::path::PathBuf;

use norte_frontend::user_commands::{self as uc, Finished, Launch, Refusal, RunIn};

use crate::app::{App, Modal};
use crate::jobs::inflight::InFlight;

/// What a spawned user-command job hands back to the loop.
#[derive(Debug)]
pub enum Outcome {
    /// A background run ended.
    Finished { launch: Launch, finished: Finished },
    /// A terminal-mode launch, its program resolved (or not) off the loop.
    Ready { launch: Launch, program: Result<PathBuf, Refusal> },
}

/// `user:<id>` pressed or picked in the box.
pub fn request(app: &mut App, id: &str) {
    let lang = norte_i18n::active();
    let pane = app.focused();
    let targets = pane.marked_paths();
    match uc::request(&app.user_commands, id, pane.dir(), &targets) {
        Err(r) => app.message = Some(r.message(lang)),
        Ok(launch) if launch.confirm => app.modal = Some(Modal::ConfirmUserCommand { launch }),
        Ok(launch) => app.pending_user = Some(launch),
    }
}

/// Starts what `request` (or the confirm) queued. Called once per loop turn.
pub fn start_pending(app: &mut App, work: &mut InFlight) {
    let Some(launch) = app.pending_user.take() else { return };
    if launch.run_in == RunIn::Background {
        app.message = Some(norte_i18n::ta(
            "user-cmd-started",
            &[("label", &norte_encoding::mask_terminal_hazards(&launch.label))],
        ));
    }
    work.user_commands.spawn(outcome_of(launch));
}

/// The job itself: run to the end, or resolve for the terminal panel.
pub async fn outcome_of(launch: Launch) -> Outcome {
    match launch.run_in {
        RunIn::Background => {
            let finished = uc::run_background(launch.clone()).await;
            Outcome::Finished { launch, finished }
        }
        RunIn::Terminal => {
            let probe = launch.clone();
            let program = tokio::task::spawn_blocking(move || uc::resolve(&probe))
                .await
                .unwrap_or_else(|_| Err(Refusal::Missing { program: launch.program.to_string_lossy().into_owned() }));
            Outcome::Ready { launch, program }
        }
    }
}

/// Applies a finished job. `true` = re-list the panes.
pub fn harvest(app: &mut App, out: Outcome) -> bool {
    let lang = norte_i18n::active();
    match out {
        Outcome::Finished { launch, finished } => {
            app.message = Some(finished.message(&launch.label, lang));
            launch.refresh && finished.ran()
        }
        Outcome::Ready { program: Err(r), .. } => {
            app.message = Some(r.message(lang));
            false
        }
        Outcome::Ready { launch, program: Ok(p) } => {
            if let Err(msg) = crate::termpanel::start_user_command(app, &launch, p) {
                app.message = Some(msg);
            }
            false
        }
    }
}
```

Wiring:
- `keys.rs`, in the `Resolution::Run` arm, before the `lua:` check: `if let Some(id) = cmd.strip_prefix("user:") { crate::user_cmd::request(app, id); return; }`.
- `keys.rs:553` (box), after `note_palette_recent` and before `parse_plugin_key`: the same, returning whatever that arm's early-return shape is.
- `event_loop.rs` `run`, as the first statement inside the main `loop`: `crate::user_cmd::start_pending(&mut app, &mut work);` (use the loop's actual binding names).
- Add a select arm next to the checksum harvest:

```rust
Some(res) = work.user_commands.join_next(), if !work.user_commands.is_empty() => {
    let refresh = match res {
        Ok(out) => crate::user_cmd::harvest(&mut app, out),
        Err(e) => { tracing::error!(error = %e, "a user command job did not finish"); false }
    };
    if refresh && crate::overlays::watch_refresh_allowed(&app) {
        let refreshed = crate::refresh::refresh_panes(&mut app, &backend, &mut console).await;
        crate::refresh::after_panes_refresh(&mut app, &backend, refreshed, &mut work.fill, &mut work.decorate, &mut work.probed, &mut work.search);
    }
}
```

`termpanel.rs`:
1. Split `start_instance` into `start_instance_in(app, profile, dir: &Path)`. The old one calls it with `shell_cwd(app)?`.
2. Add `start_user_command`. It does the following:
   - If `app.terminal_slot().is_none()`, call `app.toggle_terminal()`.
   - If the slot is still `None`, return `Err(not_here message)`.
   - Build `ShellProfile { name: launch.label.clone(), program, args: launch.args.clone(), icon: None, color: None }`.
   - Call `start_instance_in(app, &profile, &launch.cwd)`.

An entry whose program exits 0 disappears and a failing one stays (`Terminals::on_exit`, `terminals.rs:337`). That is the spec's "stays open on failure", so add no code for it.

Modal:
- `Modal::ConfirmUserCommand { launch: Launch }`, rustdoc'd "(ADR 0177) — `confirm = true` in commands.toml".
- `dialog_action`: add it to the `ConfirmDelete | ConfirmTransfer | ConfirmQuit | SemanticHits` arm.
- `confirm_modal`: `Modal::ConfirmUserCommand { launch } => app.pending_user = Some(launch),`.
- `ui/modals.rs`: title `t("user-cmd-confirm-title")`, body `uc::confirm_text(&launch, lang)`, same hints as ConfirmQuit.
- `help_context.rs`: map it to the topic id `"user-commands"` (Task 10 creates it). Until then map it to the same id ConfirmQuit uses, and change it in Task 10.
- `hints.rs`: like ConfirmQuit.

Config copy:
- `main.rs:340`: `app.user_commands = cfg.user_commands.clone();`.
- `config_reload.rs`: the same, next to `app.openers`.
- `known_commands`: chain `std::iter::once(norte_frontend::keymap::USER_HOST)`, and update its rustdoc paragraph to name both markers.

- [ ] **Step 4: Run.** `just t norte-tui`, `just c`. Expected: PASS.

- [ ] **Step 5: Commit.** `feat(tui): run user commands from a key or the box, in the background or the terminal panel`.

---

### Task 7: TUI, User rows in the box, F1, and the undefined-id warning

**Files:**
- Modify: `crates/norte-tui/src/goto.rs:93-105` (`sources`)
- Modify: `crates/norte-tui/src/overlays.rs:417-450` (`goto_help_target` / `goto_help`)
- Modify: `crates/norte-tui/src/main.rs` (startup) and `crates/norte-tui/src/config_reload.rs` (reload): compute `app.user_chords` and the warning

**Interfaces:**
- Consumes: `goto::{SECTION_USER, user_command_rows, user_command_chords, user_command_f1}`, `user_commands::undefined_bindings`.

- [ ] **Step 1: Failing tests** in `crates/norte-tui/src/goto.rs` tests (or create a module):

```rust
    #[test]
    fn the_box_lists_user_commands_under_the_prefix() {
        let mut app = crate::app::testutil::app_en(/* a file:// pane, see gestures.rs app_local */);
        app.user_commands = norte_frontend::user_commands::UserCommands::parse(
            "[[command]]\nid = \"png\"\nlabel = \"Optimize PNGs\"\nargv = [\"oxipng\", \"%F\"]\n",
        ).unwrap();
        let srcs = sources(&app, &[], &[]);
        // Build the Goto with `srcs` the way `goto::open` does and query ">optim":
        // exactly one row, key "cmd:user:png".
    }
```

Write the real body by copying how `goto::open` (TUI `goto.rs`) wraps `sources(...)` into a `Goto` and refreshes it.

In `config_reload.rs` tests (:200+, `a_reload_closes_the_help_and_the_box` shows the harness), add `a_reload_warns_about_undefined_user_commands`:
1. Write a user layer with `keymap.toml` = `[pane]\nprepend_keymap = [{ on = ["ctrl+alt+o"], run = "user:gone" }]`.
2. Reload.
3. Assert `app.message` contains `gone`.

- [ ] **Step 2: Run, FAIL.** `just t norte-tui`.

- [ ] **Step 3: Implement.**
- `sources`: add `FixedSource::new(SECTION_USER, user_command_rows(&app.user_commands, &app.user_chords, norte_frontend::shell::is_local(app.focused().dir()), norte_i18n::active()))`.
- `goto_help`: at its start, if the chosen row's key gives `Some(msg)` from `user_command_f1(key, &app.user_commands, lang)`, set `app.message = Some(msg)` and return without opening help.
- Startup and reload, both while `browse` is still owned (before it moves into the resolver, next to `palette_rows`):
  - `app.user_chords = norte_frontend::goto::user_command_chords(&cfg.user_commands, &browse);`
  - `let undefined = norte_frontend::user_commands::undefined_bindings(&browse, &cfg.user_commands);`
  - If `undefined` is non-empty: `app.message = Some(ta("msg-user-cmd-undefined", &[("n", &undefined.len().to_string()), ("first", &crate::app::detail_for_bar(&undefined[0]))]))`.
  - Place this BEFORE the `discarded_lua` warning so the security warning still wins.

- [ ] **Step 4: Run.** `just t norte-tui`. PASS.

- [ ] **Step 5: Commit.** `feat(tui): the box lists user commands; F1 and load say what is missing`.

---

### Task 8: window host, run user commands (key, box, confirm, terminal, effect)

**Files:**
- Create: `crates/norte-ui-host/src/controller/user_commands.rs` (declare it in `controller/mod.rs` with the sibling `mod` lines)
- Modify:
  - `crates/norte-ui-host/src/keys.rs:284-292` (`effective_with`): with `Effects::Full`, the known set also carries `norte_frontend::keymap::USER_HOST`. Clone `all_with(effects)` into a `Vec` and push; never add it to `IMPLEMENTED`, which feeds the palette rows.
  - `crates/norte-ui-host/src/controller/input.rs:444-470`: `Resolution::Run`, before `effect_of`: `if let Some(id) = command.strip_prefix("user:") { return self.run_user_command(id, backend, mailbox); }` (match the arm's actual return shape).
  - `crates/norte-ui-host/src/controller/palette.rs:19-40` (`run_command_key`): an arm `None if cmd.starts_with("user:") =>` that calls `self.run_user_command(&cmd["user:".len()..], backend, mailbox)`.
  - `crates/norte-ui-host/src/controller/mod.rs:2253` `enum Pending`: `UserCommand { launch: norte_frontend::user_commands::Launch }`. `run_pending` (`dialogs.rs:322`) gets an arm that calls `launch_user_command`.
  - `crates/norte-ui-host/src/dto.rs:4251` `NativeEffect`: a new variant (below).
  - `crates/norte-ui-host/src/controller/termpanel.rs:204-260,738`: give `spawn_instance` a `dir: &VPath` parameter (callers pass `self.slot().pane.dir()`), and add `start_user_terminal`.
  - `crates/norte-ui-host/src/controller/goto.rs:90-140` (`go_to_sources`): add the User source. `:294` (F1) checks `user_command_f1` first.
  - `crates/norte-ui-host/src/controller/profiles.rs:177` (`apply_config`): after rebuilding `self.effective`, say `msg-user-cmd-undefined` if `undefined_bindings` is non-empty. Do the same once at boot: find where the startup notice is composed (`startup.rs:940 startup_notice`) and add it there as a second line, or in the host's first status if `startup_notice` takes only the lua count. Pick the smaller change and say which in the commit.

**Interfaces:**
- Consumes: Tasks 1-5.
- Produces:
  - `NativeEffect::RunUserCommand { launch: norte_frontend::user_commands::Launch }`
  - `pub(super) fn run_user_command(&mut self, id: &str, backend: &Arc<dyn HostBackend>, mailbox: &mpsc::Sender<Message>) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>)`
  - `pub(super) fn launch_user_command(&mut self, launch: Launch, mailbox) -> (ActionAck, Vec<..>)`

- [ ] **Step 1: Failing controller tests**, new file `crates/norte-ui-host/tests/controller/user_commands.rs` (add `mod user_commands;` in `tests/controller/main.rs`). Model them on `rename.rs:977 a_declared_opener_governs_in_the_window`:

```rust
use super::*;

fn cfg_with(src: &str) -> norte_frontend::config::FrontendConfig {
    let mut cfg = test_settings();
    cfg.user_commands = norte_frontend::user_commands::UserCommands::parse(src).expect("valid");
    cfg
}

#[tokio::test]
async fn the_box_runs_a_background_user_command_as_a_native_effect() {
    let mut fake = Fake::default();
    fake.put("file:///casa", vec![(b"notas.txt".to_vec(), false)]);
    let cfg = cfg_with("[[command]]\nid = \"png\"\nlabel = \"Optimize\"\nargv = [\"oxipng\", \"%F\"]\n");
    let (h, _snap) = host_en_con(Arc::new(fake), "file:///casa", cfg).await;
    let mut sub = h.subscribe();
    let mut effects = h.native_effects();
    run_by_palette(&h, &mut sub, "user:png").await;
    let effect = tokio::time::timeout(Duration::from_secs(2), effects.recv()).await.expect("in time").expect("an effect");
    let NativeEffect::RunUserCommand { launch } = effect else { panic!("{effect:?}") };
    assert_eq!(launch.id, "png");
    assert_eq!(launch.args, [std::ffi::OsString::from("/casa/notas.txt")]);
    assert_eq!(launch.cwd, std::path::PathBuf::from("/casa"));
}

#[tokio::test]
async fn confirm_true_asks_before_the_effect() {
    // Same setup with `confirm = true`; after run_by_palette, `next_dialogs(&mut sub)`
    // shows one dialog whose title_key is "user-cmd-confirm-title"; no
    // NativeEffect yet (try_recv is Empty). Answer it with
    // h.dispatch(UiAction::Dialog { id, choice: "confirm".into(), secret: None })
    // and the RunUserCommand effect arrives.
}

#[tokio::test]
async fn a_remote_pane_refuses_with_the_reason() {
    // fake.put("sftp://h/d", …); host_en_con(…, "sftp://h/d", cfg);
    // run_by_palette(…, "user:png") → status message equals the EN text of
    // "user-cmd-remote"; no NativeEffect.
}

#[tokio::test]
async fn finished_says_how_it_ended() {
    // h.dispatch(UiAction::UserCommandFinished { label: "Optimize".into(),
    //   finished: Finished::Failed { code: 2 }, refresh: false }) → the status
    // message is "Optimize exited with code 2".
}
```

Write the three commented bodies in full before running, by copying the dialog and status helpers from `base.rs` (`next_dialogs` :1743, the status pattern near :596). Comment-only test bodies must not be committed.

The `UiAction::UserCommandFinished` used by the last test is added in THIS task (host side). Task 9 only adds the Tauri sender.

- [ ] **Step 2: Run, FAIL.** `just t norte-ui-host`.

- [ ] **Step 3: Implement.** `controller/user_commands.rs`:

```rust
//! User commands in the window (ADR 0177): the decision is
//! `norte_frontend::user_commands`'; background runs are the shell's
//! (`NativeEffect::RunUserCommand`), terminal runs are the panel's.

impl State {
    pub(super) fn run_user_command(&mut self, id: &str, backend: &Arc<dyn HostBackend>, mailbox: &mpsc::Sender<Message>)
        -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>)
    {
        if self.effects == crate::commands::Effects::SoloRead {
            return Self::no_mutates();
        }
        let pane = &self.slot().pane;
        let targets = pane.marked_paths();
        match norte_frontend::user_commands::request(&self.config.user_commands, id, pane.dir(), &targets) {
            Err(r) => {
                let args = r.args();
                let refs: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
                (ActionAck::Unavailable { reason_key: r.key() }, self.say_with(r.key(), &refs))
            }
            Ok(launch) if launch.confirm => self.ask_user_command(launch),
            Ok(launch) => self.launch_user_command(launch, mailbox),
        }
    }
}
```

Use the real names of the controller state type, the `effects` field and `ActionAck`. Check them with `rg -n "fn no_mutates|fn say_with|reason_key" crates/norte-ui-host/src/controller`. `reason_key` may need a `&'static str`; `Refusal::key()` returns one.

- `ask_user_command` copies `request_exit` (`dialogs.rs:233-305`):
  - `title_key: "user-cmd-confirm-title".into()` (a literal, for the host catalogue sweep).
  - `body: vec![DialogLine { text: norte_frontend::user_commands::confirm_text(&launch, self.lang), hostile: true }]`.
  - Choices `confirm` (label key `dialog-run` if one exists, otherwise the one `request_exit` uses; `destructive: false`) and `cancel`.
  - `on_confirm: Some(Pending::UserCommand { launch })`.
- `launch_user_command`:
  - `Background`: `if !self.native(NativeEffect::RunUserCommand { launch: launch.clone() }) { return Self::without_desktop(); }`, then `say_with("user-cmd-started", &[("label", masked label)])`.
  - `Terminal`: `match norte_frontend::user_commands::resolve(&launch)`. On `Err(r)`, say it as above. On `Ok(program)`, call `self.start_user_terminal(&launch, program, mailbox)`.
- `start_user_terminal`:
  1. If `slot_of_kind(KIND)` is `None`, open the panel the way `open_terminal_panel` (:81) does.
  2. Build a `ShellProfile { name: launch.label.clone(), program, args: launch.args.clone(), icon: None, color: None }`.
  3. `cwd_vpath = norte_vfs::native::vpath_from_native(&launch.cwd)`. On error say `user-cmd-remote`.
  4. Call `self.spawn_instance(&profile, &cwd_vpath, mailbox)`.

`UiAction::UserCommandFinished { label: String, finished: norte_frontend::user_commands::Finished, refresh: bool }` goes in `action.rs` right after `ProgramFinished`, rustdoc'd "native-only, like ProgramFinished". It needs:
- its `golden.rs` case (`:188` and `:313`) and the fixture. Regenerate per the file's header (`rg -n "NORTE_UPDATE" crates/norte-ui-host/tests/golden.rs`);
- routing in `controller/mod.rs` next to `ProgramFinished` (:4169) into `user_command_finished`.

`user_command_finished` says `finished.key()` with `finished.args(&label)`, and then, if `refresh && finished.ran()`, appends `self.refresh_visible(backend, mailbox)`'s changes.

`NativeEffect::RunUserCommand { launch }` is rustdoc'd: "Runs a user command in the BACKGROUND (ADR 0177) with `norte_frontend::user_commands::run_background` and answers with `UiAction::UserCommandFinished`. The launch is already expanded; whoever hosts it resolves nothing itself."

The read-only guard (`effects.rs:182-204`) does not apply here because `run_user_command` checks it first.

Box: in `go_to_sources` add a `FixedSource` for `SECTION_USER`. For the rows use `user_command_rows(&self.config.user_commands, &user_command_chords(&self.config.user_commands, &self.effective), norte_frontend::shell::is_local(self.slot().pane.dir()), self.lang)`. In the F1 handler (:294), check `user_command_f1(&r.key, &self.config.user_commands, self.lang)` first and `say` the text. Through which door: if `say` only takes keys, add the one-line `say_text(&mut self, text: String)` beside `say_with`, which sets `self.status.message` and emits the same `ViewChange::Status`.

- [ ] **Step 4: Run.** `just t norte-ui-host`, `cargo check -p norte-gui-tauri` (it must still compile: `execute`'s match over `NativeEffect` is exhaustive, so add a temporary-free arm there NOW: `NativeEffect::RunUserCommand { .. } => RunOutcome::NoProgram,` with the comment "`pump` runs it (Task 9); reached only by a hand call"), `just c`.

- [ ] **Step 5: Commit.** `feat(ui-host): run user commands from a key, the box or a dialog`.

---

### Task 9: Tauri shell, executing `RunUserCommand`

**Files:**
- Modify: `crates/norte-gui-tauri/src/native.rs` (`pump`, near the `RunProgram { detached: false }` arm, ~:123)

- [ ] **Step 1: Failing test.** In `native.rs`'s test module (or `crates/norte-gui-tauri/tests/` if the module has none; check `rg -n "#\[cfg\(test\)\]" crates/norte-gui-tauri/src/native.rs`), add a unix-only `#[tokio::test]` named `pump_runs_a_user_command_and_reports_back`. It builds a real `UiHost` the way the existing native tests do. If none exist, test the extracted helper instead:

```rust
/// What `pump` does with a RunUserCommand, minus the host: run, then build the action.
pub(crate) async fn user_command_action(launch: norte_frontend::user_commands::Launch) -> norte_ui_host::UiAction {
    let label = launch.label.clone();
    let refresh = launch.refresh;
    let finished = norte_frontend::user_commands::run_background(launch).await;
    norte_ui_host::UiAction::UserCommandFinished { label, finished, refresh }
}

#[cfg(unix)]
#[tokio::test]
async fn a_user_command_runs_and_becomes_an_action() {
    let d = tempfile::tempdir().unwrap();
    let launch = norte_frontend::user_commands::Launch {
        id: "t".into(), label: "T".into(), program: "sh".into(),
        args: vec!["-c".into(), "exit 4".into()], cwd: d.path().to_path_buf(),
        run_in: norte_frontend::user_commands::RunIn::Background,
        refresh: true, confirm: false, count: 0,
    };
    let a = user_command_action(launch).await;
    assert!(matches!(a, norte_ui_host::UiAction::UserCommandFinished {
        finished: norte_frontend::user_commands::Finished::Failed { code: 4 }, refresh: true, ..
    }));
}
```

- [ ] **Step 2: Run, FAIL.** `just gui-ci` (or the crate's test recipe; see `rg -n "gui" justfile`).

- [ ] **Step 3: Implement** the pump arm:

```rust
            // A user command (ADR 0177): run to the end on the runtime, its
            // output to the log ring, and how it ended back through `dispatch`.
            Ok(NativeEffect::RunUserCommand { launch }) => {
                let host = std::sync::Arc::clone(&host);
                tokio::task::spawn(async move {
                    let _ = host.dispatch(user_command_action(launch).await).await;
                });
            }
```

Check the bridge: `git log -S ProgramFinished --oneline -- crates/norte-ui-host/src/bridge.rs`. If adding `ProgramFinished` did not bump `BRIDGE_VERSION`, do not bump now. If the golden or TS contract tests (`crates/norte-gui-tauri/ui/tests/contract.test.ts`) fail on the new `UiAction`, bump to 109 in `bridge.rs:612` and `ui/src/types.ts:13`, with the doc entry "**109**: `UiAction::UserCommandFinished` (native-only)".

- [ ] **Step 4: Run.** `just gui-ci`, then this plan's SECOND `just ci-fast`. Expected: green.

- [ ] **Step 5: Commit.** `feat(gui): the window runs user commands in the background`.

---

### Task 10: never exposed to agents; ADR, help topic, CHANGELOG

**Files:**
- Modify: `crates/norte-mcp/src/bridge.rs` (test next to `the_tools_catalog_does_not_have_two_conventions`, :1937)
- Create: `docs/adr/0177-user-commands-run-programs-on-a-key.md`; Modify: `docs/adr/README.md` (row)
- Create: `crates/norte-help/topics/en/user-commands.md`, `crates/norte-help/topics/es/user-commands.md`; Modify: `crates/norte-help/src/corpus.rs` (both locale tables), `crates/norte-tui/src/help_context.rs` (point `ConfirmUserCommand` at `"user-commands"`)
- Modify: `CHANGELOG.md` (`## [Unreleased]` → `### Added`)

- [ ] **Step 1: Failing tests** in `bridge.rs`:

```rust
    /// User commands run arbitrary programs with the user's rights (ADR
    /// 0177): no tool may name them, and calling one by name is refused.
    #[test]
    fn user_commands_are_never_a_tool() {
        let Value::Array(defs) = tool_defs() else { panic!("tool_defs is an array") };
        for d in &defs {
            let text = d.to_string();
            assert!(!text.contains("user:"), "{text}");
            assert!(!text.contains("commands.toml"), "{text}");
        }
    }
```

Add an async sibling `calling_a_user_command_by_name_is_an_unknown_tool`. Build a `Bridge` the way the nearest `call_tool` test does (`rg -n "call_tool\|tools_call" crates/norte-mcp/src/bridge.rs` tests or `crates/norte-mcp/tests/bridge.rs:117`), call the tool `user:png`, and assert the error contains `unknown tool`.

Also pin the dependency boundary in `crates/norte-mcp/tests/` with a test `mcp_does_not_depend_on_the_frontend`. It reads `env!("CARGO_MANIFEST_DIR")/Cargo.toml` with `std::fs::read_to_string` (tests may) and asserts it does not contain `norte-frontend`.

Help topic `en/user-commands.md`. The front matter:

```toml
+++
id = "user-commands"
title = "Your own commands on a key"
tags = ["doing"]
see_also = ["shell", "settings", "panes"]
+++
```

The body is the how-to; this is the ONE home for it. It covers:
- The `commands.toml` example from the spec.
- Every key (`id`, `label`, `argv`, `description`, `cwd`, `run_in`, `confirm`, `refresh`) with defaults.
- Field codes `%f`/`%F`/`%d` only, whole elements, and the `sh -c '…' sh %F` idiom (including `"$(basename "$1")"` for a bare name).
- Binding: `{ on = ["ctrl+alt+o"], run = "user:optimize-png" }` in `keymap.toml`.
- The box: `>` and the User category; F1 shows the description.
- Background versus terminal; output in the log panel; quitting norte does not stop a background command, and what it prints after that is lost.
- Remote panes refuse.
- Too many files: use `xargs` from `sh -c`.
- A project's `.norte/commands.toml` and its `user:` bindings are ignored.
- The window reloads `commands.toml` on a profile switch, a settings change or a restart; the terminal reloads it on save.
- `norte doctor` names keys bound to undefined ids.
- Agents never see these commands.

The ES topic is a faithful translation with the same front matter (title "Tus propios comandos en una tecla"). Add `("user-commands", include_str!("../topics/en/user-commands.md"))` and the ES equivalent to `corpus.rs`.

ADR 0177 uses the 0110/0176 structure (Status accepted, Date 2026-10-10, Decision makers Oscar González, Related 0010 openers, 0026 project trust, 0077, 0082, 0110, 0176, CLAUDE.md hard rules 1 and 9). It covers:
- **Context:** Krusader's user actions; Lua is TUI-only and frozen (0110 §4).
- **Options:** A, reopen Lua; B, WASM command plugins; C, argv-only `commands.toml`.
- **Decision C**, with the rules:
  - argv only, no implicit shell, field codes as whole elements;
  - every layer but Project; project `user:` bindings discarded;
  - an unknown id loads and is reported, not fatal;
  - never in MCP or agent sessions, and no daemon method;
  - background output to the log, not the processes panel (C2);
  - one runner shared by both frontends.
- It states that this does NOT reopen 0110 §4: no Lua surface grows.
- **Consequences:** a detached child survives quit; the window reloads late (C6).

The CHANGELOG entry under `### Added`, in the house style:

"**Your own commands on a key** (ADR 0177). `~/.config/norte/commands.toml` declares programs to run on the marked files or the cursor file, with `%f`, `%F` and `%d`. Bind one with `run = "user:<id>"`, or find it in the search box under `>` in the User category. They run in the background, with their output in the log panel and the exit in the status bar, or in a new terminal-panel shell. They run only on local folders, ask first with `confirm = true`, and are never offered to agents. A project's `.norte/commands.toml` is ignored. In both frontends."

- [ ] **Step 2: Run, FAIL** (the MCP tests should PASS immediately because nothing is exposed; that is expected for a guard. Run them and confirm they are green and that they FAIL if you temporarily add `"user:"` to one tool description; revert that). Run `just t norte-help`: it fails until both topics exist and are listed.

- [ ] **Step 3: Write the docs** described above.

- [ ] **Step 4: Run.** `just t norte-mcp`, `just t norte-help`, `just t norte-tui` (help_context), `cargo test -p norte-help --doc`.

- [ ] **Step 5: Commit** in two commits:
  - `test(mcp): user commands are never a tool`
  - `docs: user commands — ADR 0177, help topic, changelog`

Then merge: `git switch main && git merge --no-ff feat/user-commands` (commit message via `-F`), and `git push origin main`. The hook is the gate run; also run `just gui-ci` first, because Task 9 touched the window.

---

## Self-review

- **Spec coverage:**
  - model, load and validate: T1
  - templates: T2 (C1)
  - shell never implicit: T2/T4
  - `run_in` background: T4/T6/T8/T9
  - `run_in` terminal: T6/T8
  - `cwd`: T2
  - `confirm`: T6/T8
  - `refresh`: T6/T8
  - keymap `user:` and diagnostics: T3 (C3)
  - remote refusal: T2/T5/T8
  - agents and MCP: T10
  - hot reload: T1 (C6)
  - box and F1: T5/T7/T8 (C8)
  - ADR, help, CHANGELOG: T10
  - edge cases:
    - missing program: T2/T4
    - no targets: T2
    - non-UTF-8: T2
    - never exits: C2 + help
    - argv limit: T2
- **Placeholders:**
  - Tasks 5, 6, 7 and 8 contain test bodies that are pointers into existing harness code (`local_app`, the `Goto` construction, the dialog and status helpers). Each says explicitly that the body must be written from the named helper before running, because the harness signatures were read through excerpts, not whole files.
  - Everything else is literal.
- **Type consistency:**
  - `Launch` fields are identical in T2, T4, T6, T8 and T9.
  - `Finished` is serde-derived in T4 and used in `UiAction` in T8.
  - `USER_HOST` is defined in T3 and used in T6/T8.
  - `undefined_bindings` is defined in T3 and used in T7/T8/doctor.
  - `user_command_rows(cmds, chords, local, lang)` has the same signature in T5, T7 and T8.
