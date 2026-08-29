+++
id = "columns"
title = "What the listing shows"
tags = ["doing"]
see_also = ["finding", "panes", "remote"]
context = ["dialog.properties"]
commands = [
    "pane.columns",
    "pane.sort-name",
    "pane.sort-ext",
    "pane.sort-size",
    "pane.sort-time",
    "pane.sort-menu",
    "pane.properties",
    "pane.chmod",
    "pane.dir-size",
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

# Sorting without opening anything

Four keys sort the focused pane without going through the dialog:
{{cmd:pane.sort-name}}, {{cmd:pane.sort-ext}}, {{cmd:pane.sort-size}} and
{{cmd:pane.sort-time}}. Pressing the one already in use reverses the direction,
exactly like clicking a header twice. {{cmd:pane.sort-menu}} opens the columns
dialog, which is where the direction and "directories first" live: no sort key
touches those, because they are your preferences rather than a property of a
column.

Sorting by extension looks at what follows the LAST dot. `.TXT` and `.txt` land
together — grouping them is what sorting by extension is for — even though they
remain different names for everything else. A name that starts with a dot has no
extension: `.bashrc` is a whole name. Anything with no extension sorts last, in
both directions, like a size the backend cannot tell you.

# What this entry is, and how much it takes

{{cmd:pane.properties}} opens the properties of whatever is under the cursor:
kind, size, date, path and whatever attributes the backend reported. All of that
is already in the listing, so opening it asks for nothing.

Except one thing, and it is exactly the one a listing cannot know: **how much a
folder takes**. A listing knows the size of a file; a folder's means walking it
whole, and doing that per row would turn going down one level into a storm of
requests. So it is counted when you ask: opening the properties of a folder
starts the count and says so while it runs.

{{cmd:pane.dir-size}} counts without opening anything, over what is MARKED — or
what is under the cursor if you marked nothing: the question it answers is "how
much does all of this take?", which is the one you ask before copying.

Counting is a task like any other: it shows in the task panel and it can be
cancelled. What cannot be read does not sink it — one forbidden folder in the
middle of a three-hour tree cannot cost you the whole count — so the number is
for what could be read.

# Changing the permissions

{{cmd:pane.chmod}} is the other half: what properties SHOW, this changes. It
asks for the mode in octal — `755`, `0644`, `4755` — with the field prefilled
with the one the entry under the cursor already has, and it acts on what is
marked, or on that same entry when nothing is. The title says how many it will
change, because typing a mode believing it goes to one and having it go to fifty
is the mistake this dialog has to make hard.

Octal rather than checkboxes because it is what someone who knows what they want
types, and it is the form the listing itself shows. Digits are 0 to 7 and four
at most: the bits above that say what CLASS the node is, and that is not
changed, it is what it is.

It is a mutation like copying or deleting, with everything that drags along: it
goes through the policy, it lands in the journal, and it **can be undone** — the
reversal is the permissions it had, read before the new ones were written. When
those cannot be read the change is made anyway and the journal records it for
what it is: something with no way back.

Only where POSIX permissions exist: a local directory or an SSH host, yes; an
object bucket or the inside of a `.zip` has nothing to change, and it says so.
It is not recursive: it changes exactly the entries you give it, and a folder
changes its own, not that of what is inside it.
