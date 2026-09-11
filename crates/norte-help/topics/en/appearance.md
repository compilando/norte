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
`key_bar = false` removes the row.

The panel bar
-------------

Under the menu sits a row with one button per side panel — Places, Viewer,
Jobs, Details, Tree, Log — each showing its name with the access letter
underlined, whether it is open, whether it has the keyboard, and whether it
has something to say. `panel_bar_style = "letters"` shrinks it to the
letters alone, and names fall back to letters on their own when they do not
all fit. `panel_bar = false` removes the row.

The pane footer
---------------

The bottom border of every listing counts what is there — directories,
files, bytes — then what is marked, then the free space of the volume the
directory lives on. When the border is too short the free space goes first
and the count second: what you just marked is the last thing to give way.
`pane_footer = false` leaves the border bare.

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

Every row shows a mark checkbox on hover; clicking it toggles the mark
without Ctrl. The `file-icons` extension can paint one-cell Nerd Font
glyphs (`style = "nerd"`): the window bundles the glyphs it needs, a
terminal needs a Nerd-patched font.

> 💡 Every row on the settings screen says what it does and applies at once.
> The file is the real interface; the screen is a way to edit it.
