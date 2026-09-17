+++
id = "history"
title = "Navigation history"
tags = ["basics"]
see_also = ["panes", "tabs"]
commands = [
    "nav.back",
    "nav.forward",
    "pane.history",
    "pane.history-left",
    "pane.history-right",
    "dialog.confirm",
    "dialog.confirm-other",
    "dialog.remove",
    "dialog.clear",
    "dialog.add",
    "dialog.filter",
    "pane.popular",
    "nav.set-jump-point",
    "nav.jump-back",
    "pane.hotlist",
    "app.goto",
]
+++

# Navigation history

Every panel remembers where it has been, and it remembers it in three ways
because they answer three different questions: where did I come from, where
have I been, and where do I always go.

## Back and forward

{{cmd:nav.back}} returns the panel to the directory it came from, and
{{cmd:nav.forward}} undoes that step. It is a trail like a browser's: go back
and then somewhere else, and the branch ahead is gone.

## The list

{{cmd:pane.history}} opens the list of places the panel has been, newest
first. The first row is where you are now, marked "here", and the cursor
starts on the next one. Rows you can still reach with {{cmd:nav.forward}} are
marked "forward".

{{cmd:pane.history-left}} and {{cmd:pane.history-right}} open the list of one
side of the screen: what you pick moves that panel even when the focus is on
the other one.

Inside the list:

- {{cmd:dialog.confirm}} goes to the directory;
- {{cmd:dialog.confirm-other}} opens it in the OTHER panel, focus stays put;
- {{cmd:dialog.remove}} drops it from the list and from the trail;
- {{cmd:dialog.clear}} clears that panel's history;
- {{cmd:dialog.add}} saves the row as a bookmark, with a name already proposed;
- {{cmd:dialog.filter}} filters the list as you type, and `Esc` drops the
  filter.

## Popular

{{cmd:pane.popular}} lists the directories you visit most, by number of
visits. It is ONE list for the whole session, not one per panel: the question
is where you usually go, and it does not change with the side you ask from.
The list keys work here too.

## Jump point

{{cmd:nav.set-jump-point}} marks the panel's current directory and
{{cmd:nav.jump-back}} returns there from wherever you are. Returning is an
ordinary navigation, so {{cmd:nav.back}} takes you back to where you were
before the jump.

## What is kept

History, jump point and popular directories live in the session: they are
still there when you open norte again. How many directories each panel keeps
is `[ui] history_size`, between 5 and 64; 30 when unset.

Bookmarks ({{cmd:pane.hotlist}}) are something else: you choose them and they
live in your configuration.

In the window, the mouse's side buttons are back and forward. A terminal does
not receive those buttons.

# Go anywhere

{{cmd:app.goto}} opens one screen holding everything that can be a
destination, in sections: the path you are typing, this panel's history, the
places you come back to most, your bookmarks, your connections, the
commands, and — if you have a semantic index — whatever the index finds.
Type and what does not match drops away; the arrows move row by row,
skipping the titles, and Enter takes you there.

It replaces none of the lists above: each keeps its own key and its own
screen, where you see them whole and can delete entries. This is the one for
when you cannot remember which of the five held the thing you want.

Three things worth knowing. Something counts as a path if it starts with
`/`, with `~`, or carries a scheme (`sftp://…`); `~` is your home directory,
not the panel's. A relative path deliberately does not count: where you are
going cannot depend on where you were. And the semantic index is asked from
three letters on, answers when it can — it is not instant — and its section
appears at the bottom without moving what you were looking at; if you do not
have it switched on, it simply never appears.

In the `orthodox`, `cua` and `vim` presets it is `ctrl+g`. The four imported
ones do not bind it, because none of the managers they transcribe has an
equivalent key: there, you reach it from the Go menu, where it sits first.
