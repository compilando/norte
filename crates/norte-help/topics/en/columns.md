+++
id = "columns"
title = "What the listing shows"
tags = ["doing"]
see_also = ["finding", "panes", "remote"]
commands = [
    "pane.columns",
    "dialog.toggle-enabled",
    "dialog.move-up",
    "dialog.move-down",
    "dialog.sort",
    "dialog.cycle-format",
]
+++
{{cmd:pane.columns}} opens the picker: every column norte can show, the ones
that are on, and the order they appear in.

- {{cmd:dialog.toggle-enabled}} turns the highlighted column on or off
- {{cmd:dialog.move-up}} and {{cmd:dialog.move-down}} move it in the row
- {{cmd:dialog.sort}} sorts the listing by it
- {{cmd:dialog.cycle-format}} changes how its values are written

Confirming applies the choice and saves it; cancelling leaves the listing as it
was. This is one of the few dialogs where `⏎` is safe by construction — it
writes your own configuration and touches no file.

# Not every column comes from norte

Three kinds of column share the picker, and they behave differently for good
reasons:

| Kind | Where the value comes from | Example |
|---------|-----------------------------------------|--------------------|
| built-in| the entry itself | name, size, mtime |
| attr | an attribute the provider reports | `attr:posix.mode` |
| plugin | an extension that computes it per entry | a `git` status |

A column a backend cannot answer is left **empty**. That is the honest
outcome: object storage has no owner and no mode, and painting a plausible `-`
would be inventing an answer. See [[remote]].

> 💡 A plugin column is filled asynchronously, after the listing is already on screen. It appears a moment later on a slow provider, and a listing that changes underneath it drops what it had rather than showing a value that belongs to the previous directory.

# Formats

Size and time can be written more than one way — bytes or IEC units, absolute
or relative dates — and {{cmd:dialog.cycle-format}} walks the choices for the
column under the cursor. An attribute column offers the cycle its provider's
hint allows, and offers none when the value is opaque.

One case is deliberately inert: a column whose format is pinned by a per-scheme
rule in your configuration shows that format and refuses to cycle. The picker
only writes the GLOBAL setting, and letting you cycle a value that the more
specific rule would go on overriding is a dialog that lies to you about what it
just did.

# Sorting

{{cmd:dialog.sort}} sorts by the highlighted column; pressing it again reverses
the direction. The default is by name, ascending, directories first. A column
with no order to it — the kind of an entry, say — does nothing when you press
it, rather than inventing a ranking.

Sorting is per pane and is remembered while the pane lives, so the two panes
can be sorted differently — which is the point when one of them is a listing
you are reading and the other a destination you are filling.
