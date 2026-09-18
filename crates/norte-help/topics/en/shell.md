+++
id = "shell"
title = "Dropping to a shell"
tags = ["doing"]
see_also = ["panes", "settings"]
commands = ["app.terminal", "app.toggle-panels", "pane.command-line", "pane.copy-path", "app.handoff"]
context = ["dialog.command-line"]
+++
A file manager you cannot leave is a file manager you stop using. Three
commands hand the terminal back to you and take it again afterwards. The
default keymaps bind none of them — the imported Krusader, Norton and Far
presets do, and otherwise you reach them from the command palette or bind them
yourself. All three start the same way: norte steps out of the way — alternate screen, raw mode and
mouse capture all released — and the program you asked for gets the whole
terminal. What differs is how you come back: two of them wait for the program
to finish; the third leaves a LIVE shell you return to with the same key.

{{cmd:app.terminal}} opens your shell (`$SHELL`, or `/bin/sh` if that says
nothing) in the **active pane's directory**. Quit the shell and you are back in
the listing, refreshed — anything you changed down there is already on screen.

{{cmd:pane.command-line}} asks for one command and runs it in that same
directory. The line is handed to the shell whole, so pipes, quoting and globs
mean what they always mean; norte does not parse it. When the command
finishes, its output stays on screen until you press a key, because output that
vanishes under a redrawn listing may as well not have been printed.

{{cmd:app.toggle-panels}} hides the panels and hands the terminal to a shell
that **stays alive** behind them. Press it again and you are back in the
listing; press it a third time and you are back in the same shell, with the
history, the variables and the half-typed line you left there. It is the one of
the three you can leave a `make` running in.

The panel and that shell follow each other. Going in, the shell is sent to the
active pane's directory; coming back, if you moved with `cd`, the panel goes
where you ended up. The shell announces where it is by printing a marker in its
prompt, which norte installs by typing it into the shell — no file of yours is
touched, and the arrangement disappears with the shell. bash, zsh and fish are
the three it knows how to set up; under any other shell the key still gives you
the shell, but nothing follows anything.

The shell is only sent somewhere when it is **idle at its prompt**. Leave a
half-typed line, or a `make` running, or `vim` open, and norte types nothing —
your line is still yours. That is why the shell sometimes does not follow the
panel, and it is the safe direction: the alternative is norte appending a `cd`
to a command you had decided not to run.

It starts on the first press, not at launch: never press the key and no shell
is ever forked. Type `exit` and the next press starts a fresh one. It dies when
norte does.

The key that brings the panels back is **the same one that gave them away**,
which is why a preset has to bind {{cmd:app.toggle-panels}} to a single key. A
key sequence cannot serve: its first chord belongs to the shell you are typing
in. Bound to a sequence, the command says so and hands over nothing, rather
than handing over the terminal with no way back.

# What it is not

The shell is a shell, not a norte pane. It knows nothing about marks, and
{{cmd:app.terminal}} is still the one to reach for when you want a shell that
ends when you leave it.

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

All three need a real directory on
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

## Carrying on in the other frontend

{{cmd:app.handoff}} hands the screen to the OTHER frontend: the terminal passes
it to the window, and the window to the terminal. What travels is what you were
looking at — the tabs, the directories, the cursor, where you have been — and
also what you had **marked**, which is the one part a `cd` does not rebuild.

It only works **with the daemon**, for a reason that fits in a sentence: the
daemon is what holds the screen. Without it there is nothing to hand over, and
over SSH there is no window to put it in; in both cases the command is
announced as unavailable with that reason rather than failing afterwards.

When you ask for it, the one leaving writes the screen, **releases** it and
launches the other. If the other does not start, nothing bad happens: the
screen is saved and the one that was leaving is still where it was. The worst
case is that it tells you.

## Copying the path

{{cmd:pane.copy-path}} puts the path of everything marked — or of the entry
under the cursor when nothing is marked — on the clipboard, one per line and in
its **native** form: `/home/notes.txt`, not `file:///home/notes.txt`. Anything
not on this filesystem has no native form, so it travels as its full location.

There are two routes and norte tells you which one it took. If a desktop
helper is installed (`wl-copy`, `xclip`) it uses that, because that one
ANSWERS. If none is — the usual case over SSH — it emits the **OSC 52**
sequence, which is read by the terminal emulator you are looking at rather than
by the machine norte runs on. That second route cannot be confirmed: a terminal
that does not support it ignores it silently, which is why the message asks you
to check by pasting.
