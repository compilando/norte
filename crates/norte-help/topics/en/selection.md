+++
id = "selection"
title = "Marking what to act on"
tags = ["basics"]
see_also = ["panes", "copying"]
commands = [
    "mark.toggle",
    "mark.all",
    "mark.invert",
    "mark.clear",
    "mark.pattern-add",
    "mark.pattern-remove",
    "app.pick-accept",
]
context = ["dialog.mark-pattern"]
+++
A command acts on the marked entries, or on the entry under the cursor when
nothing is marked. There is no third case, which is why a batch of one needs
no special key.

- {{cmd:mark.toggle}} flips the mark on the entry under the cursor and steps down a row, so holding it sweeps a range — and sweeping back over it unmarks it again
- {{cmd:mark.all}} marks everything the listing is showing
- {{cmd:mark.invert}} flips the marks of what is showing, and leaves the rest alone
- {{cmd:mark.clear}} drops them all
- {{cmd:mark.pattern-add}} marks by glob, and {{cmd:mark.pattern-remove}} unmarks by glob

# What the listing is showing

A bulk mark reaches what you can actually see. With a quick filter active it
marks the filtered subset and not the whole directory; while a long listing is
still arriving it reaches the part that has arrived so far, and the pane says
so while that is happening.

Marking by glob is the same rule from the other end: the pattern is matched
against the names in front of you, so `*.log` twice in a row marks the same
set twice rather than toggling it off. Case is folded and the names are
normalised first, so `*README*` finds `readme` and a name written in NFD by
macOS still matches the pattern you typed.

When a refresh finds that a marked entry has vanished, the mark goes with it
and the status bar says how many were dropped. A silent prune would quietly
change what the next command acts on.

> ⚠ Marks belong to **one** listing. Changing directory clears them, and an operation consumes them the moment the batch is sent — so a selection is never left half-spent, waiting on which task happened to finish.

# Handing the selection to another program

Started as `ntc --pick`, norte answers instead of acting: {{cmd:app.pick-accept}}
writes the marked entries — or the entry under the cursor, same fallback as
every other command here — to standard output, each path terminated by a NUL
rather than a newline, and exits. A shell pipes that straight into another tool, for
example `ntc --pick | xargs -0 vim`.

Enter runs it whenever the cursor is not on something that would otherwise be
entered, so browsing into a directory still works; Ctrl+Enter accepts no
matter what is under the cursor. Quitting any other way exits with nothing
written — a script tells "nothing chosen" apart from "norte failed" by the
exit code, not by parsing output.
