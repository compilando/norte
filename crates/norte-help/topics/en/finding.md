+++
id = "finding"
title = "Finding things"
tags = ["doing"]
see_also = ["selection", "panes", "viewer"]
commands = ["pane.quick-search", "pane.search", "pane.toggle-hidden"]
+++
Two different questions, two different keys. *Which of the rows in front of me
is it?* is {{cmd:pane.quick-search}}. *Is it anywhere under here?* is
{{cmd:pane.search}}.

# Narrowing what is already listed

{{cmd:pane.quick-search}} types into the pane. Every keystroke filters the rows
to the ones whose name contains what you typed, the cursor lands on the first,
and leaving the search puts the listing back exactly as it was.

Nothing is read from disk and nothing is asked of a daemon: the entries are
already here, and this only decides which of them you are looking at. That is
why it is instant on a directory of a hundred thousand entries, and why it
works identically over SFTP, in a bucket, or inside a `.zip`.

Matching ignores case and compares names in a normalised form, so `café` finds
a name macOS stored decomposed. The bytes on disk are never touched by any of
that — it is the comparison that is lenient, not the name.

# Searching a whole subtree

{{cmd:pane.search}} walks everything under the pane's directory. It matches on
the name, by glob (`*.rs`) or by regular expression, and it can also search
CONTENT — a literal string or a regular expression over the files the detector
reads as text.

Results arrive in the pane as they are found, so the first hits are usable
while the walk is still going. A hit that lives somewhere else is still a real
entry: put the cursor on it and every command on this page's neighbours works
on it, including opening it in the viewer.

A search is a **task**, exactly like a copy. It reports progress, it does not
freeze the screen, you can navigate the other pane while it runs, and it can be
cancelled — see [[copying]].

> ⚠ A content search over a remote backend reads the candidate files over the network. On SFTP or on object storage that is a request per file, and a deep tree is slow in a way the same search on a local disk is not. Narrow it by name first.

The needle is transcoded rather than the haystack decoded: a content search
does not decode whole files into text to look inside them, which is what lets
it run over a directory of unknown encodings without inventing content that is
not there.

# Hidden entries

{{cmd:pane.toggle-hidden}} shows or hides the entries whose name starts with a
dot. It is per pane and it is a decision about the LISTING: the provider always
lists everything, and hiding only sets aside what is already here — which is
why toggling it back is instant and never re-reads the directory.

The consequence is worth stating plainly, because it is the one that surprises:
a row you cannot see is a row you cannot mark, so a command acts on the visible
selection. Copying a DIRECTORY still copies everything inside it, hidden
entries included — the filter is on what you are shown, not on what a recursive
operation walks.

> 💡 A search suspends the filter. Asking for `.env` by name and being told nothing was found, because the answer was hidden, is worse than an extra row: an explicit request wins over a display preference.
