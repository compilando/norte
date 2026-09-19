+++
id = "sync"
title = "Synchronising two folders"
tags = ["doing"]
see_also = ["panes", "compare", "copying"]
commands = ["pane.sync-dirs"]
+++
# Synchronising the two panes

{{cmd:pane.sync-dirs}} is the half that writes. It plans a one-way
synchronisation — this pane onto the other one — shows you every step it would
take, and does nothing at all until you approve it. Inside the diff pane from
[[compare]] the same thing is `s`, and `m` plans a **mirror**, which also
deletes from the destination anything the source does not have. There the
direction is the diff pane's own active side, the one `Tab` flips and the
footer names — not the focused pane. Either way the plan's title spells it out
with an arrow before you approve anything. Plain letters on purpose: a
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
