+++
id = "org.norte.file-icons"
title = "File icons"
+++
Puts an icon in front of each entry saying what it is: a folder, a link, or
a kind of file — code, script, document, spreadsheet, slides, image, audio,
video, archive, configuration — and a few names that mean something on
their own (`Makefile`, `Cargo.toml`, `.gitignore`, `Dockerfile`, `LICENSE`,
`README`, a `.git` folder).

It decides from the name and from what the listing says the entry is. It
never opens the file and never learns where you are.

The `style` setting picks the glyph set: `emoji` (the default), `ascii` for
terminals whose font has no emoji, `nerd` for one-cell Nerd Font glyphs, or
`seti` for the Seti icons Visual Studio Code shows — one per language, so a
Python file and a Go file look different. The window carries the glyphs of
both; a terminal needs a Nerd-patched font or it paints a box.

Two more settings shape the column: `dir-icon` is your own glyph for plain
folders (folders whose name means more, like `.git`, keep theirs), and
`unknown-icon` is the glyph for a file the table does not know — set it and
every row gets an icon, so the column reads as a column. Both are empty by
default.
