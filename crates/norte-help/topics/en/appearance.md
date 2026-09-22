+++
id = "appearance"
title = "What the screen shows around the listing"
tags = ["basics"]
see_also = ["settings", "dialogs", "panes", "help"]
commands = ["app.settings", "app.menu", "layout.log"]
+++
Around the two listings norte paints a few rows of chrome, and each one is
a setting under `[ui]` in `norte.toml` — or a row on the settings screen
({{cmd:app.settings}}), which is the same thing without remembering the key
names. Turning one off gives its row back.

The key bar
-----------

The last row of the screen names the ten function keys and what each does
on the screen that has the keyboard: the listing, or the viewer. It is read
from your keymap, so rebinding F5 relabels it, and a key that does nothing
here shows only its number. With a dialog in front the row goes blank: no
preset puts a function key on a dialog. Clicking a cell presses the key.
`key_bar = false` removes the row. It is the terminal's alone: the window
has none, and its commands live in the menu and the palette.

The panel bar
-------------

There is one button per side panel — Places, Viewer, Jobs, Details, Tree,
Log — each saying whether it is open, whether it has the keyboard, and
whether it has something to say. In the terminal it is a row under the
menu, with the name and the access letter underlined;
`panel_bar_style = "letters"` shrinks it to the letters alone, and names
fall back to letters on their own when they do not all fit. In the window
it is a column on the left edge, with one icon per panel and a count on the
one with news: how many tasks are running, how many warnings the log holds.

`panel_bar_position` chooses where it goes: `top` (a row), `left` (a
column), or `auto`, which is top in the terminal and left in the window.
`panel_bar = false` removes it. In the terminal's column every panel is an
icon — ★ ⋔ ◉ ∿ ⓘ ≡ ◔ ◷ — with a bar on the one that has the keyboard and a
count on the one with news; `panel_bar_style = "nerd"` uses Nerd Font glyphs
and `"letters"` goes back to letters.

In the window, `titlebar = "custom"` removes the desktop's title bar and the
menu bar does its job, as in VS Code: drag it to move the window,
double-click to maximize, and minimize, maximize and close sit at its right
end. The default is `native`. It applies the next time the window opens.

The pane footer
---------------

The bottom border of every listing counts what is there — directories,
files, bytes — then what is marked, then the free space of the volume the
directory lives on. When the border is too short the free space goes first
and the count second: what you just marked is the last thing to give way.
`pane_footer = false` leaves the border bare.

The status bar
--------------

It has two halves. The left one says the messages, the waits and the
warnings — a listing that is incomplete, names reinterpreted, marks that
were lost, a detached session — and is not configurable: a warning you could
remove would stop being one. The right one shows small facts, and the ones
that do something can be clicked: `position` (where the cursor is), `marks`
(what is marked), `sort` (the order; opens the sort menu), `encoding` (how
names are read; reinterprets them), `tasks` (running tasks; opens the jobs)
and `notices` (unread notices; opens the log). `status_items` says which and
in what order, for example `status_items = ["tasks", "position"]`; an empty
list leaves the right half blank. The ones that do not fit give way by
importance, and always before a warning on the left.

`tasks` is a small progress bar. It does not appear until the work has been
running for a moment — copying a small file finishes sooner, and a bar that
lives half a second is a flicker — ; with several tasks it shows one bar, for
the total; and when they finish it leaves `✓ copied photo.jpg` for a while,
even when it was so quick that the bar never showed. If something failed it
says `✗ 1 failed` and stays longer, as long as the row stays in the jobs
panel, which is where you read why. Short of room it shrinks — losing the
name, then the rate, then the bar — before any other item is dropped.

A columns plugin can speak there too: `status_plugins =
["plugin:git/branch"]` shows that column's value for the entry under the
cursor — the branch, in the example — to the left of the others. Up to
four, only from approved plugins that declare the column; they are the
first to give way and are not clickable.

Striped rows
------------

`row_stripes = true` paints the odd rows of a listing on a band of their
own — the «pyjama» that lets you follow a wide row from its name all the way
to its date. It is off by default, because the band earns its keep on a wide
pane and gets in the way on a narrow one.

The colour is the theme's (`stripe`), not a shade norte picks: a band
computed from the background reads as invisible in one palette and as a
stripe of paint in the next. Every bundled theme defines one. A theme that
does not simply paints no band, and the listing is the one you already know.

The band never covers anything that MEANS something. The cursor, a marked
row and — in the window — the row under the pointer are painted over it, in
that order. A pyjama that hid the cursor would turn a reading aid into a lie
about where the keys are going.

Dates
-----

The modified column prints the time if the file changed today, the day and
time if it changed this year, and the date otherwise (`date_format =
"smart"`, the default). `"relative"` prints how long ago instead, and
`"iso"` the full date and time. All three are your local time. A column
setting (`[ui.columns]`) still wins for that column.

Notices
-------

A message on the status line stays `notice_seconds` seconds (eight by
default), then moves to the log and leaves a `!n` badge at the right of the
line until you open the log panel ({{cmd:layout.log}}) — clicking the badge
opens it. `0` keeps a message until the next key, as before. Persistent
warnings — a degraded connection, a journal that cannot open — never expire:
they are state, not notices.

The start screen
----------------

Before the first listing, norte shows which build is running and which core
it talks to, and the first key — or four seconds on its own — takes it away
(`splash = "brief"`, the default). `splash_ms` changes those four seconds:
anything from 200 to 60000. Below a few hundred it is a flash nobody can
read; if you want it to stay until you touch it, that is `"home"` rather than
a very large number.

`"home"` turns it into a start screen that stays until you touch it, listing
the directories you go to most and your bookmarks, each opened by its number
(1-9); clicking the row does the same. `"off"` shows nothing at all. The
first-start wizard wins: with no `norte.toml` yet, it asks first and the
screen stays away.

The jobs panel
--------------

With `processes_panel = "auto"` (the default) the panel opens itself once
work — a copy, a move, a delete — has been running for a couple of seconds:
whatever finishes sooner is told by the status bar, without taking a third of
the screen from the listing. It closes itself a few
seconds after the last row finishes, without taking the keyboard: you stay in
your listing. Those seconds are how long a finished row stays on the board, and
they are deliberate: a panel that vanished at the very moment of the outcome
would take with it the one place that says something failed. It
only closes what it opened; one you opened yourself stays. Searching,
comparing and checksumming do not open it: each of those has a screen of its
own, and covering it would say the same thing twice. `"manual"` leaves the
panel as it was, opened and closed by you.

Directories
-----------

A directory is told apart by its icon and its colour, and also carries a
slash after the name when there are no icons to look at (`dir_indicator =
"auto"`, the default). `"slash"` always adds it, `"none"` never does.

Dialog buttons
--------------

The key line of a dialog is painted as buttons you can click, each showing
its key. `dialog_buttons = false` paints the plain line of keys instead.

The cursor
----------

The cursor row of the pane with the keyboard takes the theme's accent colour;
the other pane's cursor stays grey, so two cursors never compete to say where
the keys go. Both are roles of the theme (`selection` and
`selection-unfocused`), and a theme of your own can set them.

The first start
---------------

With no `norte.toml` of your own yet, norte asks three things once: which
file manager you have in your fingers (the keys follow it), which theme, and
whether your terminal font shows icons. Esc keeps the defaults and never asks
again. `ntc --setup` asks again; `NORTE_NO_WIZARD=1` keeps it closed. F9
opens the menu in every preset but Krusader's, where it stays the terminal.

The window
----------

The window carries its own typography: JetBrains Mono for everything that
lines up in cells and Inter for menus and dialogs, 14 px on 22 px rows,
bundled so it looks the same on every machine. `font`, `mono_font` and
`font_size` under `[ui]` still win when set.

Column headers paint in small caps, and dragging the right edge of a header
sets that column's width: it is written to `[ui.columns]` as a `width`,
which the terminal reads too. When the fixed columns would leave the name
fewer than ten cells, the window drops columns from the right until they
fit, as the terminal does.

The pane title is a row of breadcrumbs — each segment is a button that goes
there — and the footer carries a two-pixel gauge of the volume's usage,
warning past 75 % and error past 90 %. A notice shows as a toast at the
bottom right for `notice_seconds`; persistent warnings are pills.

`theme_light` and `theme_dark` name the theme the window paints when the
desktop prefers a light or a dark scheme; `theme` covers whichever is not
set. A theme can ask for `backdrop = "blur"` under `[effects]` to blur what
lies behind a dialog; the terminal ignores that block.

A theme of your own is a file in `themes/` under the configuration
directory, offered by name wherever a theme is chosen. `norte theme import`
makes one from a Visual Studio Code colour theme.

Every row shows a mark checkbox on hover; clicking it toggles the mark
without Ctrl. The `file-icons` extension can paint one-cell Nerd Font
glyphs (`style = "nerd"`), or Visual Studio Code's Seti icons, one per
language (`style = "seti"`): the window bundles the glyphs it needs, a
terminal needs a Nerd-patched font.

When the viewer has no picture of its own — a photo too big for its cap, a
format the window cannot decode — a `thumbnail` extension can give it one:
`image-thumb` does for image files, and the viewer says «via» whose it is.

> 💡 Every row on the settings screen says what it does and applies at once.
> The file is the real interface; the screen is a way to edit it.
