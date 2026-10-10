# User commands on a key — design

- Date: 2026-10-10
- Status: decided by the controller under the user's delegation ("hazlo tú todo"); pending review
- Frontends: both. Shared model in `norte-frontend`; no protocol change.

## Goal

Krusader's "user actions": run a program of mine on the selection with a key,
without writing a plugin. norte has the pieces but not the door:

- the keymap accepts catalogue commands and `lua:<name>` only; anything else
  is rejected (`keymap/effective.rs:456-470`), and Lua is TUI-only and frozen
  (ADR 0110 §1-4);
- `openers.toml` already expands argv templates with field codes byte-safely
  (`openers.rs` `expand_argv` :186) and probes `PATH` (`resolve_program`).

Success: a `commands.toml` entry like

```toml
[[command]]
id = "optimize-png"
label = "Optimize PNGs"
argv = ["oxipng", "-o", "4", "%F"]
```

is bound with `{ on = ["ctrl+alt+o"], run = "user:optimize-png" }`, appears in
the search box under `>` (category "User"), and runs in the focused pane's
folder on the marked files (or the cursor file), in both frontends.

## Decisions

| question | answer |
| --- | --- |
| where they live | `~/.config/norte/commands.toml` (user layer only; a project's `.norte/commands.toml` is IGNORED, as ADR 0176 does for `terminal.toml`) |
| shape | `[[command]]` with `id`, `label`, `argv`, optional `cwd`, `run_in`, `confirm`, `refresh`, `description` |
| templates | `openers.toml`'s field codes, same expander: `%f` cursor/first file, `%F` all targets (one argv element each), `%d` pane folder, `%n` file name without folder, `%%` |
| shell | never implicit. `argv[0]` is a program; a user who wants a shell writes `["sh", "-c", "…", "sh", "%F"]` themselves (field codes still separate argv elements, never spliced into the script) |
| `run_in` | `"background"` (default: detached, stdout/stderr to the log panel, exit code in the status bar) or `"terminal"` (a new shell in the terminal panel, ADR 0176, running the argv; stays open on failure) |
| `cwd` | `"pane"` (default, the focused pane's folder) or an absolute path |
| `confirm` | `false` default; `true` asks "Run <label> on N items?" first |
| `refresh` | `true` default for background: re-list the focused pane when it exits |
| keymap | `run = "user:<id>"`; id charset `[a-z0-9][a-z0-9-]{0,63}`; unknown id at load → the binding is reported like an unknown command (same diagnostics path as `lua:`) |
| remote panes | refused with a reason ("user commands run on local folders"): `Availability::NotHere`-style dim in the box, message on the key |
| agents / MCP | NEVER exposed: user commands are not in the MCP tool surface nor callable by an agent session (they run arbitrary programs with the user's rights) |
| hot reload | `commands.toml` added to the config watcher's `CONFIG_FILES` |
| box | rows under `>` with category "User"/"Usuario", label as text, `user:<id>` as desc; F1 shows the entry's `description` (no help topic) |
| ADR | new ADR "user commands" — reopens ADR 0110 §4 for argv-only programs, records the no-shell, no-agent, user-layer-only rules |

## Architecture

1. `norte-frontend/src/user_commands.rs` (new): `UserCommand` (serde), `load_user_commands(layers) -> (Vec<UserCommand>, Vec<Diagnostic>)` skipping `Layer::Project` (pattern: `config.rs` `load_shell_profiles` :145-170), validation (id charset, non-empty argv, absolute cwd, no duplicate ids), `plan(cmd, ctx) -> Result<Launch, Refusal>` where `ctx` = focused pane folder (local `PathBuf` or remote), targets (marks or cursor), and `Launch { program, args: Vec<OsString>, cwd, run_in, refresh }` built with `openers::expand_argv`.
2. Keymap: `effective.rs` accepts `user:<id>` beside `lua:`; `USER_HOST = "user:*"` availability; both frontends resolve to a new dispatch arm.
3. TUI: dispatch `user:<id>` → `plan` → background spawn (tokio `Command`, output lines to the log panel, exit status message, optional refresh) or terminal panel spawn (ADR 0176 API). Confirm dialog reuses the existing yes/no modal.
4. Host (window): the same through its effect layer; background spawn on the host side (the window host is local), terminal mode through the host's terminal panel. Bridge bump only if a view changes (confirm uses the existing dialog) — expected none.
5. Box rows: a `user_command_rows(&[UserCommand]) -> Vec<GotoRow>` source in `goto.rs` (category `cmd-ns-user`), activated via `Action::Command("user:<id>")`; recents recorded like any command.
6. Docs: help topic `user-commands` (EN+ES), `commands.toml` example in docs, CHANGELOG, ADR.

## Edge cases

- Program not found in `PATH`: refused at run with "oxipng not found", never at load (the file may be shared across machines).
- No targets and the template uses `%f`/`%F`: refused with a reason; `%d`-only commands run with no selection.
- Names with invalid UTF-8: bytes preserved through `OsString` (expander is byte-safe).
- A command that never exits in background mode: listed in the processes panel as a child; quitting norte does not kill it (detached), documented.
- Very many targets: argv length limit → refused with a reason (count shown), suggest `xargs` in docs.

## Testing

- Model: load/skip-project/validate/diagnostics; `plan` for each field code, marks vs cursor, remote refusal, missing targets, cwd modes.
- Keymap: `user:` accepted, bad charset rejected, unknown id diagnosed.
- TUI: a binding runs a fake program (test helper script) and refreshes; terminal mode opens a shell entry; confirm path.
- Host: same via controller tests.
- Box: rows appear under `>` with the User category; never in places mode.
- MCP: a test that the tool list and agent dispatch never see `user:` commands.

## Out of scope

- Per-command keybinding inside `commands.toml` (keys stay in the keymap).
- Interactive prompts/placeholders (`%{ask:…}`), Krusader's full placeholder language.
- Remote execution over sftp.
