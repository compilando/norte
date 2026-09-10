+++
id = "settings"
title = "Settings and themes"
tags = ["basics"]
see_also = ["appearance", "dialogs", "mouse", "help"]
commands = ["app.settings", "app.theme"]
+++
{{cmd:app.settings}} opens the settings screen: every option norte has, with
its current value and a line saying what it does. Changing one writes it to
`norte.toml`.

That file is the real interface — the screen is a way to edit it without
remembering the key names. Editing it in a text editor is equally supported,
and a save is picked up without a restart.

# Where the file lives

Three layers, each overriding the one before it:

| Layer | Where | When it applies |
|---------|------------------------------|--------------------------------|
| system | `/etc/norte` | always |
| user | `~/.config/norte` | always |
| project | `./.norte` next to your work | only after you trust it |

A project layer is a file in a directory you may have just cloned, so it does
nothing until you say so. The same caution applies to a project `init.lua`,
which is a script and not a setting.

The settings screen writes to your USER layer. A value a trusted project layer
overrides goes on being overridden after you change it: the screen wrote what
you asked, and the more specific file still wins.

> 💡 A few options only take effect on a restart — a font, a language. Their row says so instead of pretending the change landed.

# Themes

{{cmd:app.theme}} lists the themes and previews the highlighted one as you
move: what you are looking at while you choose is what you are choosing.
Confirming keeps it and writes it; cancelling puts back the one you had.

The picker lists the themes that ship with norte. A theme of your own is a TOML
file, and `theme` under `[ui]` takes a path to one as readily as a name — it
just will not be in the list, since the list is what is built in.

Every surface takes its colours from the theme in force: the panes, the
dialogs, this help. A page that ignored your theme would be the one screen that
does not look like the program.

# The two settings people look for first

- **Mouse.** `mouse = false` under `[ui]` gives the terminal its own selection back. See [[mouse]] for what capture costs and for the Shift trick that needs no setting at all.
- **Confirm on quit.** `auto` asks only when work is pending, `always` asks every time, `never` closes straight away. See [[dialogs]].
