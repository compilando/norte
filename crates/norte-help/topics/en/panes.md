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
    "pane.select-drive",
    "pane.select-drive-left",
    "pane.select-drive-right",
    "pane.compare-dirs",
    "pane.sync-dirs",
    "layout.focus-next",
    "layout.focus-prev",
    "layout.close-slot",
    "layout.grow",
    "layout.shrink",
    "layout.equalize",
    "layout.set-target",
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

# Picking a drive

{{cmd:pane.select-drive}} opens a picker of the host's volumes for the
**focused** pane; {{cmd:pane.select-drive-left}} and
{{cmd:pane.select-drive-right}} open the same picker for a **side** of the
screen instead — whichever pane is drawn there, regardless of which one has
focus. Total Commander's `Alt+F1`/`Alt+F2` have worked that way since Norton
Commander, and both presets that import them keep the same split. Enter sends
that pane to the highlighted mount.

Each row shows the label when the filesystem has one, the mount point, the
filesystem type, and free space of total — a mount the host could not query in
time shows as unknown rather than as zero, which would read as full instead of
unanswered. The list is a snapshot taken when the picker opens: it does not
grow, shrink or re-check free space while you are looking at it, the same
contract {{cmd:pane.history}} and {{cmd:pane.hotlist}} already keep. A key
inside the picker toggles between the everyday list and every mount the host
has, system filesystems included, and the footer says which one you are
looking at.

# Comparing the two panes

{{cmd:pane.compare-dirs}} answers the question an orthodox file manager exists
to answer: **are these two trees the same?** It walks both panes at once and
opens a diff pane where every row is one name, seen from both sides.

Nothing is written. This key produces an answer and only an answer — no copy,
no delete, no plan. It is also the honest way to check a transfer you have just
finished, which is the question people actually ask after every copy.

Each row carries two marks, and the second is the one worth learning. The first
says WHAT was decided: `=` same, `#` different, `<` only on the left, `>` only
on the right, `T` two different kinds under one name, `A` an ambiguous pairing,
`E` a row that could not be read at all. The second says HOW MUCH that verdict
is worth: `!` proved it, `~` suggests it, `?` means the location could not say.

That second mark is not decoration. A row marked `= ~` was called *the same*
because the two dates match, and two files with the same date can still hold
different bytes; a row marked `= !` was proved by a hash, or by a size that
settled it. An archive has no date you should trust, and it answers `?` rather
than having something invented for it — which is a real answer, not a failure.

Comparison never reads file contents unless you ask it to. Names, kinds, sizes
and dates are enough for almost every question, and hashing a terabyte over
SFTP because you pressed a key would not be.

`Tab` swaps which side you are looking from, and the footer says which one that
is. Nothing is ever inferred from the row: a row that exists only on the left,
seen from the right, has nothing to go to and says so rather than quietly
taking you to the other side. Today the side governs where `Enter` lands;
acting on a row without leaving the diff — viewing it, copying it, deleting it
— is the next spec's work, and until then the way to do any of those is to
press `Enter` and use the keys you already know once you are there. Digits `1`
to `5` hide and show whole categories — same, different, only left, only right, and everything that went
wrong — and hiding a category never moves what is selected. `Enter` leaves the
diff and takes you to where the selected row really lives, which is how you
open a directory that exists on one side only: the walk reports it as one row
rather than enumerating a subtree it already knows the answer for. `Esc`
cancels a comparison that is still running, and closes the pane once it is not.

# Synchronising the two panes

{{cmd:pane.sync-dirs}} is the half that writes. It plans a one-way
synchronisation — this pane onto the other one — shows you every step it would
take, and does nothing at all until you approve it. Inside the diff pane the
same thing is `s`, and `m` plans a **mirror**, which also deletes from the
destination anything the source does not have. There the direction is the diff
pane's own active side, the one `Tab` flips and the footer names — not the
focused pane. Either way the plan's title spells it out with an arrow before
you approve anything. Plain letters on purpose: a
function key with a modifier does not survive a `tmux` session, and a
documented shortcut that never arrives is worse than none.

Nothing is planned twice and nothing is executed from the screen. What you
approve is a plan the daemon is holding, named by its own digest, so the thing
that runs is byte for byte the thing you read.

The plan leads with what the undo could give back, and that is a fact about the
DESTINATION and not about the steps. The same list of copies reverts entirely
against a destination whose trash records where it buried things, and reverts
nothing against one with no trash at all — so the summary says which of those
you are looking at before it says anything else. A `mirror` that deletes trees,
or any plan the undo does not cover, asks a second question with the number in
it.

Each step carries three marks: what it does, how sure the comparison behind it
was, and whether the undo brings it back. The third is the one that needed the
daemon to say something new, and it is never read off the step alone.

Mark rows with `Ins` in the diff pane to synchronise only those; a marked
directory takes its whole subtree with it. With nothing marked the plan covers
both trees.

This needs norte running against the daemon. Synchronising deletes and
overwrites, so it has to be journalled and undoable, and the in-process engine
has no journal — the key says so rather than failing halfway.

> 💡 A directory you visit often is worth a favourite: the pane remembers where it has been, and favourites are shared by both panes.

> 💡 When there is nothing further back, the key says so. A key that goes quiet is indistinguishable from a broken one.

Panels can be resized and closed. {{cmd:layout.grow}} and
{{cmd:layout.shrink}} give the focused panel room or take it away, and
{{cmd:layout.equalize}} returns them all to the same size.
{{cmd:layout.close-slot}} closes the focused one and **refuses to close the
last**: a screen with no listing at all is not a layout, it is a hang with
borders.

{{cmd:layout.focus-next}} and {{cmd:layout.focus-prev}} walk the panels. With
two they do what {{cmd:pane.switch}} does; they exist for when there are more.
{{cmd:layout.set-target}} sets which panel a copy goes to. With two panels the
destination is already the other one and nothing changes — it is for the day
there are more than two and the tie cannot be broken on its own.
