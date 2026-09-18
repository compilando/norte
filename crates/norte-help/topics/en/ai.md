+++
id = "ai"
title = "AI rename and semantic search"
tags = ["doing"]
see_also = ["finding", "agents", "settings"]
commands = ["pane.ai-rename", "pane.organize", "pane.semantic-search"]
context = ["dialog.ai-rename", "dialog.semantic-search"]
+++
Both of these are OFF until you turn them on, and both are two-step: you ask,
you get something to look at, and only then does anything happen. That shape is
the point — a model is a suggestion engine, and a suggestion you cannot inspect
before it lands is just an action you did not take.

# Renaming a directory's worth of files

{{cmd:pane.ai-rename}} asks for an instruction in your own words — *number these
by date*, *strip the download suffix* — and comes back with a **plan**: the old
name and the proposed name, pair by pair.

Nothing has been renamed at this point. Accepting the plan submits the moves,
each one an ordinary operation with its own confirmation path, journal entry
and undo. Discarding it costs nothing and leaves the directory untouched. A
plan that proposes nothing says so rather than showing an empty list you have
to interpret.

> ⚠ The instruction and the names in that directory are what leave your machine. Not the file contents — but a filename is often the more revealing of the two.

The plan is validated before you ever see it: the names it proposes are single
segments, not paths, so a plan cannot walk out of the directory it was asked
about, and one that tries is rejected whole rather than partly applied.

# Organizing a directory

{{cmd:pane.organize}} is the same deal with one more freedom: the destination
may carry **folders**. The plan comes back as a tree — what will be created,
what was already there, and what ends up inside each thing — because what
changes here is the shape of the directory, and a list of forty moves does not
let you see it.

Above the tree is the count: how many folders it creates and how many files it
moves. And it cannot be approved without reaching the end: if the tree does not
fit, you have to walk it.

Applying it is **one single batch**: the missing folders are created and
everything moves under the same identifier, so undoing it puts the files back
and takes the folders nobody else filled — in one step, not forty.

A plan can also come from an extension of kind `organizer`, which appears in the
palette with its own label. It is reviewed and applied in exactly the same way:
what makes the operation safe is not where the names came from.

# Searching by meaning

{{cmd:pane.semantic-search}} takes a question rather than a pattern and answers
from the local index — what has been indexed and only that. Hits come back
ranked, and choosing one takes you to where it lives.

It complements the ordinary search rather than replacing it: `*.rs` is a job
for a glob, and *the thing about retry backoff* is not. See [[finding]].

# What is on, and what never leaves

Three settings, and they are not the same knob:

| Setting | What it decides |
|-------------------|-------------------------------------------------------|
| enabled | off by default — nothing AI does anything until it is on |
| local only | refuses any provider that is not on this machine |
| denied prefixes | subtrees whose names and contents never reach a provider |

*Local only* is a hard gate in the core rather than a courtesy of the provider:
a provider that claims to be local and is not gets refused there. *Denied
prefixes* compare path segments rather than string prefixes — `~/.ssh` denied
does not accidentally deny `~/.sshfs`, and a subtree under a denied one is
denied too.

All three live in `norte.toml` under `[ai]`; see [[settings]]. Refusals name
which of the three stopped the request, because "AI failed" is not something
you can act on.

> 💡 This is not the same thing as an agent driving norte from outside. That is a different door with its own approvals and its own undo, and it is on [[agents]].
