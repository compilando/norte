+++
id = "panes"
title = "Two panes, one destination"
tags = ["basics"]
see_also = ["selection", "copying", "history", "compare", "sync", "profiles"]
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
    "pane.mirror-target",
    "pane.pull",
    "pane.swap",
    "pane.select-drive",
    "pane.select-drive-left",
    "pane.select-drive-right",
    "layout.split-h",
    "layout.split-v",
    "layout.focus-next",
    "layout.focus-prev",
    "layout.close-slot",
    "layout.grow",
    "layout.shrink",
    "layout.equalize",
    "layout.flip",
    "layout.set-target",
    "layout.places",
    "layout.preview",
    "layout.processes",
    "layout.metadata",
    "layout.log",
    "layout.disk-map",
    "layout.timeline",
    "layout.pick",

    "pane.tree",]
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

{{cmd:pane.mirror-target}} sends what is **under the cursor**: a folder, and
the other pane opens it; anything else, and it sends this pane's location,
which is what {{cmd:pane.mirror}} does. It is for looking inside a directory
without leaving where you are, and it is what a Krusader reader expects from
the arrows with Ctrl. On the `..` row it sends this location, not the parent's:
that row is the operand of nothing.

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

# Arranging the screen

Panels can be resized and closed. {{cmd:layout.grow}} and
{{cmd:layout.shrink}} give the focused panel room or take it away, and
{{cmd:layout.equalize}} returns them all to the same size, and
{{cmd:layout.flip}} flips the focused one's split: side by side becomes
stacked, and back. With the mouse a panel moves by dragging it by its title
and dropping it on another: on one of its sides, or in the middle to join
it as a tab; while dragging, where it would land is marked. Sizes and
places are remembered on closing — the window and the terminal each keep
their own — until another layout is chosen with {{cmd:layout.pick}}.
{{cmd:layout.close-slot}} closes the focused one and **refuses to close the
last**: a screen with no listing at all is not a layout, it is a hang with
borders.

{{cmd:layout.focus-next}} and {{cmd:layout.focus-prev}} walk the panels. With
two they do what {{cmd:pane.switch}} does; they exist for when there are more.
{{cmd:layout.set-target}} sets which panel a copy goes to. With two panels the
destination is already the other one and nothing changes — it is for the day
there are more than two and the tie cannot be broken on its own.

{{cmd:layout.split-h}} splits the focused panel side by side and
{{cmd:layout.split-v}} splits it top and bottom. The new panel starts in the
same directory, already filled, and takes the focus: splitting is asking for
room to work in. From three panels on, which one a copy goes to stops being
obvious — that is what {{cmd:layout.set-target}} is for, and the panel you
designate is marked on its border.

{{cmd:layout.places}} opens a panel on the left with your drives and your
favourites, and `Enter` on a row sends the **focused listing** there: it is a
control, not a panel with a directory of its own. A second press moves the
keyboard into it; a third closes it. Drives are asked for when it opens and
when you unfold their section, never on a clock: asking every filesystem how
much room it has left every few seconds is felt on a network mount.

A favourite whose path no longer parses is marked `!` and dimmed rather than
dropped — a favourite that hides itself is a configuration bug you cannot see.
The status bar says why when you press it.

{{cmd:layout.preview}} opens a viewer on the right that **follows the cursor**
of the active listing: moving the cursor changes what it shows, with nothing
else pressed. It is the same viewer {{cmd:pane.view}} opens — same keys, same
encodings, same hex — placed in a slot instead of over the screen.

A directory is not read: the box says that is what it is. A file that cannot be
read asks nothing either — the reason is painted inside, because a panel that
follows the cursor cannot raise a dialog for every key you press going down a
listing. And a docked viewer you cannot see — behind a tab, or with no room —
reads NOTHING.

{{cmd:layout.processes}} opens a panel with one row per running task: its
progress bar, how far along it is, and cancel on the row under the cursor. The
task strip at the foot of the screen does not go away — the panel is what you
open when you want to **act** on a task rather than watch it. It takes the
keyboard on opening, and a second press closes it: the opposite of the docked
viewer, and deliberately so, because you opened it to press something in it.

There is no pause. The protocol has cancel and nothing else, and a control that
does not do what it says is worse than a control that is missing.

{{cmd:layout.log}} opens this session's log: what norte is noting down while you
work, right there in the terminal. It is what answers "and why did that fail?"
without going off to find a file — a connection that dies leaves a "permission
denied" on the bar that says nothing, and the exact reason is right here.

`e`, `w`, `i`, `d` and `t` pick how much is shown, from errors to everything; `/`
filters by text, and searches the module name too, which is half of what you
actually look for. Arrows and pages detach from the tail so you can read while
lines keep arriving, and `End` re-attaches. `Esc` hands the keyboard back without
closing the panel.

{{cmd:layout.disk-map}} opens the disk map: what the directory you are looking at
is made of, one rectangle per child, sized by what it takes up. It answers "where
did my space go?", which a listing sorted by size does not — there a directory
weighs what its own node weighs, not what is inside it.

Arrows move from rectangle to rectangle and {{cmd:nav.enter}} goes into the
selected one, which is how you walk down to whatever is eating the disk. A click
does the same on whichever rectangle you press. `Esc` hands the keyboard back
without closing the panel.

Measuring a big tree takes a while, so the map paints as it measures and says
when it has finished. Anything that could not be read in full is marked with `≈`
rather than counted as zero: it is a lower bound and it says so, because a small
rectangle that is really enormous is worse than one admitting it does not know.
And when a directory has more children than fit, the ones that travel are the
**largest** — the ones a map exists to show.

Asking for more detail really does raise the level, not just the filter: debug
messages do not exist until you ask for them, so they appear from then on and not
backwards. Lowering it again does **not** stop recording them, so going there and
back does not erase the very stretch you were looking at; the title says what is
being recorded whenever that is more than what is shown, and closing the panel
puts it back. The panel keeps the last two thousand lines and says how many it
dropped.

With `ntc --socket` the daemon is **another process**: the providers, the
journal, the policy and the reason a connection never opened are all on the far
side of the socket, and this terminal's log only has this terminal's lines. So
the panel asks for its log too and merges them by time, with a rule down the
margin on the lines that came from it. `s` cycles the three views — this
terminal, the daemon, both — and is only offered when there is a daemon serving
its log; one built without it says so, rather than letting you believe the
interesting half never happens. Raising the level asks it too, and there is a
difference the status bar warns you about: its ring belongs to **all** its
clients, never lowers, and closing this panel does not lower it either. The
missed-line counts stay apart — this side's and its own do not mean the same
thing and are never summed.

That extra detail is **norte's only**, though. The libraries norte uses to talk
to a server write, at that level, the contents of what they send — including your
password before it is encrypted. So their messages stay at warnings and errors,
which is what explains a failure, and no key in this panel can raise them. The
file `norte paths` points at holds everyone's at that level, and the same cap
holds on the far side: the daemon's ring applies it in the process that owns it,
which is where it has to be.

{{cmd:layout.timeline}} opens the timeline: what has been done on this machine,
newest first, with the time, who did it — you, an agent or an extension, and the
dot's colour says which — the verb and what it was done to. A batch shows as
**one** row and says how many entries it carries, because it is undone whole or
not at all. Reaching the bottom asks for more history; it is not all loaded when
it opens.

Pointing at a row and pressing {{cmd:dialog.confirm}} asks whether to undo **what
you did after it**. The row you point at stays: it is the state you want to get
back to, not the first casualty. The question carries the count before you
answer, in three numbers that do not add up to one: what will be undone, what
will be skipped — what has no way back, what you already undid — and what is not
yours, which this undo never touches (an agent's work is undone from its own
screen). If there is nothing of yours above that row, no dialog opens and it
says so: asking about something that will not happen teaches you to say yes
without reading.

Undoing runs as a task, with its progress and its cancel, in reverse order, and
it stops the moment something does not line up — if a file is no longer where it
was, it stops there and tells you, instead of guessing onward. What has no way
back is not invented: it is counted and skipped.

It has no keyboard shortcut in any preset, deliberately: the `alt+letter` space
for panels is taken and none is free across all seven, so binding it in some and
not others would be a feature half the readers do not have. It is in the panel
bar — which carries them all — and in the View menu.

{{cmd:layout.metadata}} opens a details panel on the right that also follows
the cursor: name, kind, size, when it was last modified, and whatever the
provider already said about the entry. It reads **nothing** to do it —
everything it shows arrived with the listing — so walking down a directory with
it open costs no requests at all.

{{cmd:layout.pick}} lists the layouts: the five norte ships with — **orthodox**
(the two listings you already know), **simple** (one listing), **krusader**
(two listings and the places sidebar), **explorer** (one listing, sidebar,
docked viewer and processes) and **full** (everything at once) — plus whatever
you have saved in `layouts/` inside your config directory. Each row draws what
the screen would look like, worked out from the layout itself rather than from
a picture stored beside it, so the drawing cannot go stale.

A layout name and a keymap preset name are two different settings. `krusader`
is both, and choosing the **layout** moves panels around without rebinding a
single key; the keys are `[keymap] preset`. The dialog says so at the foot, so
that the coincidence is a convenience and not a trap.

A file of yours wins over the factory layout of the same name: `layouts/simple.toml`
is what `simple` loads. Delete the file to get the original back. `--layout <name>`
picks one for a single run without touching your config.

# The directory tree

{{cmd:pane.tree}} opens a column on the left with the tree hanging from the
directory you are looking at. `⏎` on a branch expands it and sends the listing
there: seeing what is inside and being inside are the same answer.

It is read **branch by branch**: opening one lists THAT directory and nothing
else. A tree that read itself whole would take minutes on a big folder and far
longer on a remote one, and what is inside does not change by collapsing it — so
collapsing and reopening costs no second trip.

Only directories show. A tree with files in it would be a worse copy of the
listing you already have next to it; what this panel answers is how the place is
organised.

Three presses, like the places panel: the first opens it and takes the keyboard,
the second takes the keyboard back if you dropped it, the third closes it.
