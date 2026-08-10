+++
id = "shell"
title = "Dropping to a shell"
tags = ["doing"]
see_also = ["panes", "settings"]
commands = ["app.terminal", "app.toggle-panels", "pane.command-line"]
context = ["dialog.command-line"]
+++
A file manager you cannot leave is a file manager you stop using. Three
commands hand the terminal back to you and take it again afterwards. The
default keymaps bind none of them — the imported Krusader, Norton and Far
presets do, and otherwise you reach them from the command palette or bind them
yourself. All three work the same way: norte steps out of the way — alternate screen, raw mode and
mouse capture all released — the program you asked for gets the whole
terminal, and the panels come back when it is done.

{{cmd:app.terminal}} opens your shell (`$SHELL`, or `/bin/sh` if that says
nothing) in the **active pane's directory**. Quit the shell and you are back in
the listing, refreshed — anything you changed down there is already on screen.

{{cmd:pane.command-line}} asks for one command and runs it in that same
directory. The line is handed to the shell whole, so pipes, quoting and globs
mean what they always mean; norte does not parse it. When the command
finishes, its output stays on screen until you press a key, because output that
vanishes under a redrawn listing may as well not have been printed.

{{cmd:app.toggle-panels}} hides the panels and shows the terminal underneath
until you press a key. It launches nothing, so it is the one of the three that
works on a remote pane too.

# What it is not

{{cmd:app.toggle-panels}} shows the terminal's **scrollback**, not a live
shell. mc keeps a subshell alive behind its panels and types into it; norte
does not. What you see is what was already there. Issue #142 tracks the real
thing.

Suspension also hands over the whole terminal, and norte can only put back
what it took: the alternate screen, raw mode and the mouse capture. A program
that dies leaving the terminal in some state of its own is beyond what can be
undone from here.

While a program has the terminal, norte is not running: no directory watch, no
task tick, and no answer to anything a background agent asks. An approval
request that arrives during a long shell session times out and is refused,
which is the safe direction but is worth knowing before you leave a shell open
for an hour.

Pasting into the command line behaves like pasting into a terminal that has no
bracketed-paste support: the first newline in what you paste **submits**, and
the rest is discarded rather than run. Type the command, or paste it and check
what is in the field before pressing Enter.

`Ctrl+C` reaches the program, not norte. `Ctrl+Z` is not handled: suspending
norte while it has handed the terminal over is not something it recovers from
cleanly, so avoid it.

# Not every pane has a shell

{{cmd:app.terminal}} and {{cmd:pane.command-line}} need a real directory on
this machine, so they decline on an SFTP host, an S3 bucket or the inside of an
archive, and say which pane they are talking about. A shell opened "there"
would silently be somewhere else — your home directory, most likely — and that
is worse than a refusal.

A shell started this way is **you**, acting with your own permissions. norte
records nothing about it: there is no actor to attribute it to and no way to
undo it, and pretending otherwise would put entries in the journal that cannot
be reversed. See [[agents]] for the other half of that rule — what is written
down is what norte itself did.

# Which norte you are quitting

A program the TUI suspends for inherits `NORTE_LEVEL`, one higher than the
one norte itself was started with. Run `ntc` inside a shell you opened from
norte and you have two of them; the variable is there so your prompt can say
so, the same way `SHLVL` does.

The graphical version cannot always promise this. A terminal window served by
an already-running instance — GNOME Terminal and Konsole both do this, and so
does macOS — is actually started by that server, not by norte, so it inherits
the server's environment and not ours.
