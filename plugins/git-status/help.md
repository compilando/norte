+++
id = "org.norte.git-status"
title = "Git status"
+++
Adds a column that marks which files changed against git's index: `M`
modified, `D` deleted, `?` untracked, `!` ignored. A clean file carries no
mark, and a directory carries the strongest mark of what is inside it.

It reads the index and the `.gitignore` files under the repository root the
host confines it to; it never writes.

Two settings. `glyphs` swaps the letters for one-cell symbols (`●`
modified, `✖` deleted, `+` new, `·` ignored). `ignored`, turned off, leaves
ignored files unmarked: with large `.gitignore` files the mark repeats over
half the screen and stops saying anything.
