+++
id = "compare"
title = "Comparing"
tags = ["doing"]
see_also = ["panes", "sync", "copying"]
commands = ["pane.compare-dirs", "pane.compare-files"]
+++
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
