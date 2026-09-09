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
    "pane.mirror-target",
    "pane.pull",
    "pane.swap",
    "nav.back",
    "nav.forward",
    "pane.select-drive",
    "pane.select-drive-left",
    "pane.select-drive-right",
    "pane.compare-dirs",
    "pane.compare-files",
    "pane.sync-dirs",
    "layout.split-h",
    "layout.split-v",
    "layout.focus-next",
    "layout.focus-prev",
    "layout.close-slot",
    "layout.grow",
    "layout.shrink",
    "layout.equalize",
    "layout.set-target",
    "layout.places",
    "layout.preview",
    "layout.processes",
    "layout.metadata",
    "layout.log",
    "layout.pick",

    "profile.pick",
    "profile.next",
    "profile.prev",
    "profile.save-as",

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

# Comparing two FILES

{{cmd:pane.compare-files}} is the other question: **how do these two files
differ?** It acts on two marked in the focused pane, or on the one under the
cursor here and the one under the cursor over there. Two, and it is not
guessed: with three marked, with one, or with a folder among them, it SAYS so
rather than comparing something you did not choose.

Another program shows the difference — the one you name in `[ui] diff`
(`meld %F`, `vimdiff %F`, whatever you use). With nothing configured it is
`diff -u`, and its output stays on screen until you press a key. Both files
must be on this system: an external program cannot be handed an `sftp://`, and
that is said out loud, as it is for opening and editing.

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

# Profiles

A layout arranges the screen. A **profile** is the whole workspace: its layout,
its keymap, its theme, its columns, its favourites, and where every panel was
standing when you left it. `photos` and `servers` and the tree you are working
in want different answers to all of those, and a profile is how you keep them
apart instead of rearranging the same screen by hand every time you change
task.

A profile is a directory inside `profiles/` in your config directory, with the
same shape as your configuration itself — a `norte.toml`, and optionally its
own `keymap.toml`, `openers.toml` and `layouts/`. Copying a profile between
machines is copying a directory.

{{cmd:profile.pick}} lists them and marks the one you are in.
{{cmd:profile.next}} and {{cmd:profile.prev}} cycle without opening anything,
which is what you want when you keep two. `--profile <name>` starts in one for
a single run. Otherwise norte remembers the one you were last in.

{{cmd:profile.save-as}} saves **what you are looking at** as a profile: the
arrangement as it stands and each panel's directory, so that its first start
puts you back where you left off. If you were in a profile, the new one takes
its `keymap.toml` too — save-as produces something that behaves like what you
had. The name becomes a directory, so it is checked before anything is written,
and saving over an existing profile rewrites those pieces and leaves its other
files alone.

What a profile sets overrides your own configuration — that is what choosing it
is for — and a project's `.norte` still overrides the profile. What a profile
**cannot** do is change where the daemon listens, turn the AI on, decide where
logs are written, raise the archive limits, or run an `init.lua`: a profile
declares, it does not execute. Anything of that kind inside one is ignored and
said out loud rather than quietly honoured.

If a profile you named does not load, norte says which file and refuses to
start — you asked for that one. If it was merely the profile you were last in,
it starts without it and tells you, so you are never locked out of the program
by a typo in a directory you were only trying out.

# The session: where every panel was

On closing, norte saves the screen — the layout, which panels are open, each
one's directory and history — and puts you back there the next time. That is
the **session**, and **one window** keeps it: the first to connect to the
daemon takes it, and any later one starts with the same screen and goes its own
way from there, writing nothing. Two windows writing the same session would
overwrite each other in turns, and neither would put you back where you left
off.

A window that is not saving says so with a discreet indicator in the status
bar: `session not saved`. Clicking it opens this page. It means that **when
this window closes its screen will not be remembered**; your files have nothing
to do with it and are at no risk. It happens in three cases. Usually another
norte window was already open — the terminal or the graphical one, it does not
matter — and that one is saving; once you close it, the next window to ask
takes the session over. It also happens while the daemon is being handed over
(an update): the session is free for a moment and this window asks for it
again on its own. And it happens when the saved session was written by a NEWER
norte than this one: it is left alone so it is not damaged, and this window
starts from its configuration.

The indicator goes away by itself as soon as the window is the one saving
again. What a profile says about where each panel opens is a seed for the
panels the session does not know about; what the session remembers wins.

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
