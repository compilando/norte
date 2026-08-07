+++
id = "panes"
title = "Two panes, one destination"
tags = ["basics"]
see_also = ["selection", "copying"]
commands = [
    "pane.switch",
    "cursor.up",
    "cursor.down",
    "cursor.page-up",
    "cursor.page-down",
    "cursor.top",
    "cursor.bottom",
    "nav.enter",
    "nav.parent",
    "pane.refresh",
    "pane.mirror",
    "pane.pull",
    "pane.swap",
    "nav.back",
    "nav.forward",
]
context = ["browse"]
+++
Two panes are on screen at once. One has focus: it is the one the cursor moves
in, and the one every command reads from. The other one is the destination.

{{cmd:pane.switch}} hands the focus to the other pane. Everything else follows
from which pane holds it — {{cmd:nav.enter}} walks into the entry under the
cursor and {{cmd:nav.parent}} goes back up, landing the cursor on the
directory you just left.

{{cmd:pane.refresh}} re-reads the listing. It refreshes **both** panes, not
just the focused one, because a change made outside norte rarely respects
which pane you happened to be looking at. Local directories are watched and
refresh themselves; a remote or an archive is not, so that is the key that
tells you the truth about them.

No command ever asks *where to*. That is why an orthodox manager needs so few
keys, and why the second pane is not a layout preference you can talk it out
of.

# Moving the cursor

{{cmd:cursor.up}} and {{cmd:cursor.down}} step a row,
{{cmd:cursor.page-up}} and {{cmd:cursor.page-down}} a screen, and
{{cmd:cursor.top}} and {{cmd:cursor.bottom}} go to the ends of the listing.

The cursor belongs to the pane, not to the screen: each one keeps its own, and
a pane you come back to is where you left it, on the row you left it on. That
is also what makes the second pane usable as a destination while you work in
the first.

The cursor is what a command falls back to when nothing is marked — one row is
a batch of one, and needs no special key. See [[selection]].

# The destination is a pane, not a disk

The inactive pane can be an SFTP host, an S3 bucket, or the inside of an
archive. Copy does not behave differently because of it; that is what
[[copying]] is about.

Each pane keeps its own directory history and its own sort order, so the
remote side can look nothing like the local one and neither has to compromise.

# Moving a location across

{{cmd:pane.mirror}} sends the **other** pane where this one is, and the focus
stays where it was. It is the fastest way to line a copy up: {{cmd:pane.copy}}
never asks where to, so preparing a transfer *is* pointing the other pane
somewhere — and this points it without you having to leave the source.
{{cmd:pane.pull}} is the same gesture the other way round: the focused pane
goes where the other one is.

Neither of them says anything when both panes are already in the same place.
Nothing was asked for that failed, and re-listing a pane for no reason would
slide its listing out from under the cursor sitting in it.

{{cmd:pane.swap}} exchanges the two, which is how you reverse the direction of
a copy without navigating anywhere. It touches no disk: no listing is re-read,
nothing can fail, and the marks, the filter, the sort order, the cursor and the
pane's own history all travel with their pane, because the whole pane moves
instead of being rebuilt. The focus stays on the same physical **side** of the
screen on purpose — carrying it along with the content would leave you looking
at the very same listing and calling it a swap.

Mirroring onto a host you have not visited yet connects and asks about its key
exactly as walking there would. The question belongs to the pane that is
travelling, which under a mirror is not the pane you are sitting in. If the
destination cannot be reached, the pane stays where it was and the reason goes
to the status bar.

A pane showing the hits of a live search has no location to hand over: the
directory behind it is the root the search walked, not the list you are reading,
so the gesture is refused and says why rather than guessing. Only the pane the
location comes **from** is vetoed. Sending a location onto a results pane is
fine — the listing that arrives is a real one, and it ends the search.

# Going back

{{cmd:nav.back}} returns the focused pane to where it was, and
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

> 💡 A directory you visit often is worth a favourite: the pane remembers where it has been, and favourites are shared by both panes.

> 💡 When there is nothing further back, the key says so. A key that goes quiet is indistinguishable from a broken one.
