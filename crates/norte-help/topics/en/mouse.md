+++
id = "mouse"
title = "Using the mouse"
tags = ["basics"]
see_also = ["selection", "panes", "copying"]
commands = ["nav.enter", "mark.toggle", "pane.copy", "pane.move"]
+++
norte listens to the mouse in both frontends, and the two behave the same
because the rules live in one place. In the terminal that has a price, and the
price is at the bottom of this page — read it before wondering why you can no
longer select text.

- a left click gives that pane the focus and puts the cursor on the row you clicked
- a double click on a row does exactly what {{cmd:nav.enter}} does: walks into a directory, an archive or a remote, and leaves a file alone
- the wheel scrolls the listing **under the pointer**, focused or not, so you can read one pane while working in the other. It moves that pane's cursor, which is also what a command falls back to there when nothing is marked
- ctrl and a click toggle the mark of one row, the same mark {{cmd:mark.toggle}} sets from the keyboard
- shift and a click mark the range between the cursor and the row you clicked, and add to what was already marked
- dragging across rows marks what it sweeps, and pulling back gives those rows up again
- a right click opens a menu of the operations you already have keys for
- dragging the edge of a column header resizes that column, and the width is saved on its own. In the window the edge is on the right of each header; in the terminal it is the separator that opens each column after the name, since the name is the one that grows. A click on an edge without moving changes nothing

A click never marks on its own. Marking is either a modifier or a drag, so
clicking around a listing to see what is in it cannot change what the next
command will act on.

# Dragging between panes

A drag onto the other pane **copies**. Hold shift and it **moves**. The
decision is read when you let go, not when you press, so a drag you started as
a copy stays a copy until the moment shift is down — and you can change your
mind either way, mid-gesture. Whichever it is, the drop opens the same
confirmation {{cmd:pane.copy}} and {{cmd:pane.move}} open, and is undone the
same way: a drop is an ordinary operation, not a quieter one.

What travels depends on the row you started on, which is how one gesture does
two jobs:

- a drag that starts on a row that is **already marked** takes the marks — all of them, wherever they are in that listing
- a drag that starts on an **unmarked** row takes that one row, and only from the moment the pointer crosses into the other pane. Until then the same gesture is still marking what it sweeps

That second rule is called promotion, and it exists because grabbing one file
and pulling it across is the commonest drag there is. It changes what the
gesture *does*, never what is selected: the row you pressed is not marked by
it, and any rows the sweep marked on the way out are **given back** when the
pointer leaves the pane. Come home and the sweep resumes from the same anchor,
having lost nothing.

Two ways out, both leaving the selection exactly as it was: let go over the
source pane (dropping at home does nothing) or let go anywhere that is not a
row — a border, a header, the status bar. A destination is never guessed.

Because the gesture means one thing over its own pane and another over the far
one, it says which before you let go: how many items, to which directory, and
copy or move. In the terminal that line is the status bar. It is read from the
same state the drop reads, so it cannot promise one thing and the drop do
another — but the terminal only reports the keyboard alongside a mouse report,
so pressing shift without moving updates it on the next row you cross.

# The right-click menu

The graphical frontend opens a menu at the pointer with the operations that
already have keys: open ({{cmd:nav.enter}}), {{cmd:pane.view}},
{{cmd:pane.copy}}, {{cmd:pane.move}}, {{cmd:pane.rename}},
{{cmd:pane.delete}}, and copying the path to the clipboard. Every entry runs
the same command the keyboard runs — there is no second way to copy a file.
Entries that cannot run right now (a read-only listing, a remote without the
capability) are shown dimmed with the reason instead of disappearing.

What the menu acts on is decided by the row you right-clicked:

- if that row is **marked**, the menu acts on the marks, and its header says how many
- if it is **not**, the menu acts on that row alone

Getting there costs something, and it is worth knowing before it surprises
you: right-clicking an unmarked row **drops that pane's marks**. It has to,
because every command prefers the marks when there are any — leaving them
would make the menu say "1" and the copy take eleven. The discarded selection
cannot be brought back, not even by closing the menu with Esc. The trade is
deliberate: the failure it prevents is silent, and this one is visible the
instant the menu opens.

# Giving the terminal its mouse back

While norte is capturing the mouse, your terminal emulator never sees the
button presses it uses for its own text selection. Select-and-paste stops
working the way you have it in your fingers, and that surprises people far
more than the mouse support pleases them.

Two ways out, and neither needs a restart:

- hold **Shift** while you drag. Almost every emulator (xterm, GNOME Terminal, Konsole, Alacritty, kitty, WezTerm, Windows Terminal) reads Shift+drag as "this one is mine" and selects text normally. Shift is doing double duty here: the emulator keeps it for itself, so norte never sees it and neither the range marking nor the move-instead-of-copy above fires. On a terminal that passes Shift+drag through instead, you get norte's gesture and no selection
- set `mouse = false` under `[ui]` in `norte.toml`, or turn *Mouse* off in the settings screen. The capture is released as soon as the file is saved

The same release happens whenever norte hands the terminal to another program
— an editor, a pager — and is taken back when that program exits. A program
launched from norte never inherits a terminal in mouse mode.

> 💡 Everything the mouse does here, the keyboard already did. If a gesture feels missing, the key for it exists — the mouse is a second way in, never the only one.
