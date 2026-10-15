+++
id = "finding"
title = "Finding things"
tags = ["doing"]
see_also = ["selection", "panes", "viewer"]
commands = ["pane.quick-search", "pane.search", "layout.search", "pane.toggle-hidden"]
+++
Two different questions, two different keys. *Which of the rows in front of me
is it?* is {{cmd:pane.quick-search}}. *Is it anywhere under here?* is
{{cmd:pane.search}}.

{{cmd:layout.search}} opens the search panel beside the listing.

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

## Narrowing it down

A name and some content are rarely enough in a big tree, so the dialog also
has seven fields and four switches. All optional, and all pulling the same
way: getting out of sight what you are not looking for.

They are the same ones in the terminal and in the window, and they ask the
same thing: the form lives in a single place so the two cannot drift apart.
The function keys named below are the TERMINAL's; in the window the switches
are checkboxes and the cycle is a button, reached with the mouse or with tab.

**Skip folders** is the one you feel most. Comma-separated names — `target,
node_modules, .git` — skipped at ANY level, which is how they turn up. Names
and not paths for exactly that reason: the folder in the way is in a hundred
places you cannot list in advance. It stops the DESCENT and nothing else, so
the folder itself can still come back as a hit if its name matches.

**At least** and **at most** bound the size, written the way people say it:
`1M`, `500k`, `2.5G`, or plain bytes. **Changed in last** takes a number of
days. And **looking for** (`F6`) cycles anything / files / folders.

A filter ON ITS OWN is already a search: "everything over a gigabyte" needs no
name at all, and it is one of the questions people ask most.

**Whole word** (`F4`) is for content: without it, searching `set` in code
returns `offset`, `settings` and `subset`. It costs something — it forces a
decode instead of a byte comparison — which is why it is off by default.

**Subfolders** (`F5`) can be turned off when the question is what is HERE
rather than what is under here.

**Read text as** forces an encoding for the content. Empty is the normal case
and the one that is right nearly always; this is for when it is not, the same
way the viewer lets you force its own. A name that is not recognised is
reported, rather than falling back to detection and handing you believable
results read in the wrong alphabet.

> ⚠ A field that cannot be read STOPS the search and takes you to it. That is not fussiness: running it while ignoring a mistyped `1 gigabyte` returns the whole tree, and a whole tree reads exactly like a result.

In the terminal the results open as a new tab of the pane (see *Results as a
tab* below); the window shows them in its own results view. Either way they
arrive as they are found, so the first hits are usable while the walk is still
going. A hit that lives somewhere else is still a real entry: put the cursor on
it and every command on this page's neighbours works on it, including opening
it in the viewer.

A search is a **task**, exactly like a copy. It reports progress, it does not
freeze the screen, you can navigate the other pane while it runs, and it can be
cancelled — see [[copying]].

> ⚠ A content search over a remote backend reads the candidate files over the network. On SFTP or on object storage that is a request per file, and a deep tree is slow in a way the same search on a local disk is not. Narrow it by name first.

The needle is transcoded rather than the haystack decoded: a content search
does not decode whole files into text to look inside them, which is what lets
it run over a directory of unknown encodings without inventing content that is
not there.

# The search panel

{{cmd:layout.search}} (`Alt+Shift+F`), the Search button in the rail, or Find →
Search panel opens a panel that asks the same question as {{cmd:pane.search}}
but keeps the answer in view. The panel searches the folder the listing was in
when it opened; that folder is its root. It opens with the keyboard in its name
field: Tab and Down go on to the text field and then to the results, and Escape
gives the keys back to the listing.

It searches as you type: a moment after you stop (about a third of a second) it
starts, and if a search was still running it is cancelled first, so two never
race over the same tree. Enter searches at once. Until the new results begin to
arrive the old ones stay on screen, dimmed.

Next to the text there are three switches: match case, whole word and regular
expression. A fold below them holds the rest — skip folders, size, age,
subfolders, kind of entry, the encoding to read text as. The "Search here"
button moves the root to the folder the listing is in now; in the terminal that
is `F7`, the switches are `F2` to `F6` as in the dialog, and `F8` opens or folds
the filters. A regular expression that cannot be read is said so in the panel
instead of searching.

Results are a tree: folder, then file, then the lines that matched, each with
the match highlighted and the line centred on it. A file shows up to 50 lines;
the rest are summed up as "+N more". Left and Right fold and unfold, and the
status line says how many files and folders were reached while the walk is
still going.

Enter on a line opens norte's viewer on that line with the match lit, scrolling
sideways when the line is long. In the window a click does the same; in the
terminal the first click selects the row and a second click on it — or a double
click — opens it. A very large file (over 256 KiB) opens on a window around the
line, and the title says "lines from N". When norte cannot tell where that line
starts — UTF-16 and some other encodings — it opens at the top instead and says
so. In the terminal, closing the viewer puts you back in the panel. Enter on a
file takes the pane to its folder with the cursor on it.

The {{cmd:pane.search}} dialog is still there for a one-off question.

# Results as a tab

**Open as listing** turns what the panel found into a tab of the listing you
can work in: `F9` in the panel, or the button above the results in the
window. In the terminal, {{cmd:pane.search}} opens such a tab too, instead of
taking over the pane. The tab is called `🔍 <query> (N)` and sits next to the
folder you were in; the line where the path usually goes says what was searched
and where.

The files keep their names and gain a **Folder** column: where each one lives,
relative to the folder the search started from (`.` for that folder itself).
Click its header to sort by it. Mark, copy, move, delete and rename work as in
any folder, and each file goes as its own job, so two hits with the same name
copied to one folder go through the usual collision handling for the second
one — the terminal asks, the window shows it failed — instead of one silently
replacing the other. Copying hits into the folder the search
started from works; only a file that is already there is refused.

If the search was still running when you opened the tab, it goes on in the
tab, and the panel is free for the next question. A moved, deleted or renamed
file leaves the tab once the operation ends. {{cmd:pane.refresh}} runs the
same search again, from now, and a tab saved with the session comes back by
running its search again, not as an old list.

In the terminal, Enter on a hit opens its folder in a new tab with the cursor
on it, and the results tab stays; Esc stops a search that is still running.
Going anywhere else from the tab turns it into an ordinary folder tab.

A results tab is not a folder, so what needs one is refused there, with the
reason: creating a folder or a file, organizing, packing, a terminal, sync or
compare "here", and sending this location to the other pane. Copying INTO a
results tab is refused too — choose a folder as the destination.

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
