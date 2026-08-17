+++
id = "help"
title = "Reading this help"
tags = ["basics"]
see_also = ["index", "panes"]
commands = [
    "app.help",
    "app.palette",
    "dialog.filter",
    "dialog.pane",
    "dialog.back",
    "app.menu",
]
+++
{{cmd:app.help}} opens this overlay from anywhere. The list on the left is
every page; the panel on the right is the page you are on.

{{cmd:dialog.pane}} moves between the two. On the left, up and down change the
page, and the cursor OPENS what it lands on — arrowing down the list is
reading it, not choosing what to read. On the right they walk the runnable
rows and the links at the bottom. Enter on a runnable row closes the help and
runs the command exactly as its own key would: the same confirmation, the same
policy gate, the same journal entry. Enter on a link follows it, and
{{cmd:dialog.back}} returns to the page you came from.

Rows for the overlay's own verbs — the three on this page — are the exception.
They are listed so their keys can be looked up, but they mean something only
inside an overlay, so a pane has nothing to run: Enter on one says so and
leaves the help open.

{{cmd:dialog.filter}} starts filtering the list on the left. It matches page
titles, page ids and the commands each page documents — a command from the
start of the id or of any of its dotted parts — so `copy` brings up every page
that documents `pane.copy`, not only the one named after it. Leaving the search
box keeps what you typed; emptying it is what backspace is for.

# The keys here are yours

No key in these pages is written into the text. Each one is looked up in your
effective keymap as the page is drawn, so a rebind changes the prose. A command
with no key at all shows its name instead of claiming one.

The last entry in the list, *keys*, is the other direction: the whole effective
keymap, generated, including the `dialog.*` verbs that overlay footers leave out
for want of width.

> 💡 {{cmd:app.palette}} is the fast version of the same model: type, Enter, gone. This help is the version that explains.

{{cmd:app.menu}} opens a menu bar with the same commands arranged by topic. It
adds nothing the keyboard cannot do; it adds a way to FIND it — the palette
asks you to know the name of what you want and this help asks you to read,
while a menu can be walked. Arrows move, `Enter` runs, `Esc` closes, and with
the mouse you just click.
