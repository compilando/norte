+++
id = "user-commands"
title = "Your own commands on a key"
tags = ["doing"]
see_also = ["shell", "settings", "panes"]
commands = ["app.palette"]
context = ["dialog.user-command"]
+++
Sooner or later there is a program you run on a file every day: optimize an
image, convert a document, open a folder in your editor. A user command is
that program, written down once in `commands.toml` and run from a key, from
the search box, or from either frontend, on the files you marked or on the
file under the cursor.

## Declare one

Put this in `commands.toml`, next to your `norte.toml`:

```toml
[[command]]
id = "optimize-png"
label = "Optimize PNGs"
argv = ["oxipng", "-o", "4", "%F"]
```

Every command is one `[[command]]` table. The keys:

- `id` (required): lowercase letters, digits and dashes, 64 at most, starting
  with a letter or a digit. This is what you bind.
- `label` (required): the text of its row in the search box.
- `argv` (required): the program, then its arguments.
- `description`: one line that `F1` shows over its row in the search box.
- `cwd`: `"pane"` (the default, the folder of the focused pane) or an
  absolute path.
- `run_in`: `"background"` (the default) or `"terminal"`.
- `confirm`: `false` by default. With `true`, norte asks before it runs.
- `refresh`: `true` by default. When a background command ends, the panes
  list their folders again.

## What goes in `argv`

`argv` is a program and its arguments, and norte runs exactly that. There is
no shell: no pipes, no globs, no `~`, no `$VARIABLES`. The first element is
the program, looked up on your `PATH` when the command runs, and it is never
expanded. A program that is not there is reported when you run the command,
not when the file loads, so one file can travel between machines.

Three field codes are filled in, and only as a whole element of `argv`:

- `%f` is the file under the cursor, or the first marked one.
- `%F` is every marked file, or the file under the cursor when nothing is
  marked. It becomes one argument per file, so names with spaces or odd bytes
  arrive intact.
- `%d` is the folder of the focused pane.

Any other `%x` as a whole element is an error when the file loads. A command
that uses `%f` or `%F` and has nothing to work on is refused with a reason;
one that uses only `%d` runs with no selection.

When you do want a shell, write it yourself and hand the files over as
arguments, never spliced into the script:

```toml
[[command]]
id = "name-of-first"
label = "Print the first name"
argv = ["sh", "-c", 'echo "$(basename "$1")"', "sh", "%F"]
```

Here `$1` is the first file and `"$@"` is all of them. Very many files can
overflow what the system accepts for one command line; norte then refuses
with the count and runs nothing. From `sh -c`, `xargs` is the way around it.

## Put it on a key

Bind it in `keymap.toml`:

```toml
{ on = ["ctrl+alt+o"], run = "user:optimize-png" }
```

Or find it without a key: open the search box with {{cmd:app.palette}}, or
type `>` in it, and look under the User category. `F1` over a row shows its
`description`. A binding to an `id` that `commands.toml` does not define is
not fatal: the file loads, the problem is reported when the configuration
loads or reloads, `norte doctor` names the key, and pressing it says there is
no such command.

## Where it runs and where its output goes

In the background (the default) the program is detached. Its output goes to
the log panel, and how it ended goes to the status bar. Quitting norte does
not stop a background command, and whatever it prints after that is lost; a
command that never exits is yours to stop.

With `run_in = "terminal"`, norte opens a new shell in the terminal panel and
runs the program there, so you can watch it and answer it.

User commands run only on local folders. In a remote pane they are refused
with a reason. A window opened read-only does not run them either.

## Which files are read

`commands.toml` is read from the user and the system layers. A project's
`.norte/commands.toml`, and any `user:` binding in a project's keymap, are
ignored: a folder you downloaded must not choose what runs with your rights.
A file that does not parse, or has an invalid command, is a configuration
error, and norte keeps the configuration it already had.

The terminal reloads `commands.toml` when you save it. The window reads it on
a profile switch, a settings change or a restart; it does not watch the file.

## Agents never see them

A user command runs an arbitrary program with your rights, so it is not a
tool of the agent bridge and an agent session cannot call one. See the
agents page for what an agent can do.
