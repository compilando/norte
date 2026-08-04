+++
id = "panes"
title = "Two panes, one destination"
tags = ["basics"]
see_also = ["selection", "copying"]
commands = ["pane.switch", "nav.enter", "nav.parent", "pane.refresh"]
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

# The destination is a pane, not a disk

The inactive pane can be an SFTP host, an S3 bucket, or the inside of an
archive. Copy does not behave differently because of it; that is what
[[copying]] is about.

Each pane keeps its own directory history and its own sort order, so the
remote side can look nothing like the local one and neither has to compromise.

> 💡 A directory you visit often is worth a favourite: the pane remembers where it has been, and favourites are shared by both panes.
