+++
id = "mouse"
title = "Using the mouse"
tags = ["basics"]
see_also = ["selection", "panes"]
commands = ["nav.enter", "mark.toggle", "pane.copy", "pane.move"]
+++
norte listens to the mouse in both frontends. In the terminal that has a
price, and the price is at the bottom of this page — read it before wondering
why you can no longer select text.

- a left click gives that pane the focus and puts the cursor on the row you clicked
- a double click on a row does exactly what {{cmd:nav.enter}} does: walks into a directory, an archive or a remote, and leaves a file alone
- the wheel scrolls the listing **under the pointer**, focused or not, so you can read one pane while working in the other. It moves that pane's cursor, which is also what a command falls back to there when nothing is marked
- ctrl and a click toggle the mark of one row, the same mark {{cmd:mark.toggle}} sets from the keyboard
- shift and a click mark the range between the cursor and the row you clicked, and add to what was already marked
- dragging across rows marks what it sweeps, and pulling back gives those rows up again

A click never marks on its own. Marking is either a modifier or a drag, so
clicking around a listing to see what is in it cannot change what the next
command will act on.

# Dragging between panes

A drag that starts on a row that is **already marked** means "take these
somewhere", not "mark some more" — one gesture, two jobs, told apart by the
state of the row you started on.

In this terminal frontend that gesture has nowhere to land yet: dropping onto
the other pane does nothing, and says so in the status bar rather than failing
in silence. Mark what you want and press {{cmd:pane.copy}} or
{{cmd:pane.move}}; the destination is the other pane either way.

# Giving the terminal its mouse back

While norte is capturing the mouse, your terminal emulator never sees the
button presses it uses for its own text selection. Select-and-paste stops
working the way you have it in your fingers, and that surprises people far
more than the mouse support pleases them.

Two ways out, and neither needs a restart:

- hold **Shift** while you drag. Almost every emulator (xterm, GNOME Terminal, Konsole, Alacritty, kitty, WezTerm, Windows Terminal) reads Shift+drag as "this one is mine" and selects text normally. Shift is doing double duty here: the emulator keeps it for itself, so norte never sees it and the range marking above does not fire. On a terminal that passes Shift+drag through instead, you get the range mark and no selection
- set `mouse = false` under `[ui]` in `norte.toml`, or turn *Mouse* off in the settings screen. The capture is released as soon as the file is saved

The same release happens whenever norte hands the terminal to another program
— an editor, a pager — and is taken back when that program exits. A program
launched from norte never inherits a terminal in mouse mode.

> 💡 Everything the mouse does here, the keyboard already did. If a gesture feels missing, the key for it exists — the mouse is a second way in, never the only one.
