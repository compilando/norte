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
    "app.orders",
    "layout.assistant",
]
+++

# Navigation history

Every panel remembers where it has been, and it remembers it in three ways
because they answer three different questions: where did I come from, where
have I been, and where do I always go.

## Back and forward

{{cmd:nav.back}} returns the panel to the directory it came from, and
{{cmd:nav.forward}} undoes that. Each pane walks its own trail, and neither key
moves the focus.

It is a TRAIL, not a list. From one directory to a second and then a third,
back twice reaches the first. A most-recently-used list walked as if it were a
trail would bounce between the two most recent directories forever, which is
why "where was I a moment ago" and "where has this pane been" are two different
questions here: the second one is the popup behind {{cmd:pane.history}}, and
stepping back never adds to it.

Navigating somewhere new from the middle of the trail forgets the branch you
stepped off, exactly as a browser does. A way forward into a history you have
already abandoned is the bug everyone has met.

A step that does not arrive is rewound — you never left, so the trail is put
back as it was. That covers the step that **fails** and the one you **abandon**
with Esc while it is still listing: either way the pane is showing what
it was showing, and a trail that counted the step would send you "forward" into
the directory already on screen. When the reason is that the directory is
**gone**, it also leaves the trail, the forward branch and the history popup, so
the key can never trap you on a directory that has proved not to be there. Any
other failure keeps it: a host that was down and a directory you may not read
are both still places, and either may answer next time.

A step that stops to ask about an unknown host key is the one case that waits:
it is neither taken nor put back until you answer, because trusting the key
resumes that very navigation. Trust it and the step finishes; deny it, or let
the resumed step fail, and the step is rewound like any other that never
arrived.

> 💡 When there is nothing further back, the key says so. A key that goes quiet is indistinguishable from a broken one.

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

> 💡 A directory you visit often is worth a favourite: the pane remembers where it has been, and favourites are shared by both panes.

In the window, the mouse's side buttons are back and forward. A terminal does
not receive those buttons.

# Go anywhere

{{cmd:app.goto}} opens one search box. Empty, it lists everything that can be
a destination, in sections: the path you are typing, this panel's history, the
places you come back to most, your bookmarks, your connections and — if you
have a semantic index — whatever the index finds. Type and the best matches
rise, with the letters that matched highlighted; the arrows move row by row,
skipping the titles, and Enter takes you there.

The first character changes what the box lists. `>` lists the commands — it is
what {{cmd:app.palette}} opens — with their category, their key and, dimmed
with the reason, the ones that cannot run here; with nothing after the `>`,
the ones you ran last come first. `?` lists the help pages. Delete the prefix
and you are back to places, without closing. `F1` on a command opens its page.

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
equivalent key: there, you reach it from the Go menu, where it sits first. In those four,
{{cmd:app.palette}} opens the box with `>`; delete it to search places.

## Orders: `:`

{{cmd:app.orders}} opens the same box with `:` typed, and the line becomes an
order: `sel ext:pdf age<30d | cp >other`. While you type, the rows the `sel`
matches are marked; Esc puts back the marks you had. Enter runs the last
stage through the same dialog its key opens — a copy still asks.

- `sel` takes terms, all of which must hold: `ext:pdf,jpg`, `name:*draft*`
  or a bare glob, `name=NAME` for exactly one name (`name=*` is the file
  called `*`), `size>10M`, `age<7d` (`s m h d w`; `m` is minutes),
  `type:file|dir|link`, and `!` in front negates one. Rows the listing has
  not measured yet are measured first; Esc stops that.
- Then one action: `cp >other`, `mv >here`, `cp >/a/path`, `trash`,
  `mkdir NAME`, `cd PATH`, or any command by its name (`pane.swap`).
- `log` opens the timeline; `undo 3` undoes your last three operations,
  after asking.

A name that is not text, or holds a space or `|`, is written with escapes:
`\xff`, `\ `, `\|`; any space, also a no-break one, separates words. A
name you type finds the entry it names even when stored decomposed, or in
the encoding the pane reads names with. A `sel` that matches nothing runs
nothing — it never falls back to the row under the cursor.

## The assistant: `@`

{{cmd:layout.assistant}} opens a panel where you ask in plain words — "copy
this month's PDFs to the other pane", "what takes the most space here?". Typing
`@` in the box does the same with what follows. Each reply is either an order
in the grammar above, which you review and apply with `a` (it opens the same
dialog its key would), or an answer.

It is off until you turn it on in `norte.toml` (`[ai] enabled = true` and a
provider). It never changes anything itself. To answer, it reads names, sizes,
dates and your recent operations — never the contents of a file — and only
under the folders of your two panes, minus the paths you deny in `[ai]`.
