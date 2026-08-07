+++
id = "viewer"
title = "Reading a file without leaving"
tags = ["doing"]
see_also = ["panes", "archives", "mouse"]
commands = [
    "pane.view",
    "viewer.close",
    "viewer.up",
    "viewer.down",
    "viewer.page-up",
    "viewer.page-down",
    "viewer.top",
    "viewer.bottom",
    "viewer.encoding",
    "viewer.encoding-auto",
    "viewer.hex",
    "pane.open",
]
context = ["viewer"]
+++
{{cmd:pane.view}} opens the entry under the cursor and {{cmd:viewer.close}}
puts it away. Nothing is written, nothing is unpacked, and the file is never
opened for writing — the viewer reads.

What it reads is the **first 256 KiB**, not the file. That is why a 40 GB log
opens as fast as a note, and why the status line says so when there is more:
what you are looking at is the head, and it is not pretending otherwise.

{{cmd:viewer.up}} and {{cmd:viewer.down}} move a line,
{{cmd:viewer.page-up}} and {{cmd:viewer.page-down}} a screen, and
{{cmd:viewer.top}} and {{cmd:viewer.bottom}} go to the ends of what was read.

The same keys work on a file inside a `.zip` or over SFTP. The pane holds a
location, the viewer reads whatever that location gives it, and neither of them
has a special case for a backend; see [[archives]].

# Text is a decision, not a fact

A file is bytes. Whether those bytes are text, and in which encoding, is
DETECTED — by looking at the bytes, never at the extension — and the answer is
in the status line.

{{cmd:viewer.encoding}} disagrees with the detector: it cycles the plausible
candidates, reloading the same bytes as UTF-8, as one of the legacy 8-bit
encodings, as UTF-16. {{cmd:viewer.encoding-auto}} hands the decision back.

> 💡 Reloading changes nothing on disk. Decoding is a reading of bytes that never move, so a wrong guess costs you a screen of mojibake and one keystroke.

Two things are always visible without being asked for: bytes that did not
decode arrive as `�` rather than being dropped, and control characters, bidi
overrides and invisible separators are masked before they are painted. A
terminal that executes what it displays is a terminal a file can drive, so
nothing here is painted raw.

# When it is not text at all

{{cmd:viewer.hex}} switches to a hex dump: offset, sixteen bytes, and the
printable column beside them. It is the honest view for anything that was never
text, and it is where you land when the detector says binary.

Images are recognised by their magic bytes — PNG, JPEG, GIF, BMP, WebP — again
by content and not by name. The graphical frontend paints them; the terminal
shows the hex, because that is what a terminal can honestly show.

An extension can also supply a preview: a plugin that knows a format turns it
into text or into styled lines, and its output is bounded and masked like any
other third-party text. A plugin that fails, is disabled, or takes too long
does not block the file — you get the raw view, which is what you would have
had anyway.

# Handing the file to another program

{{cmd:pane.open}} does not use the viewer at all: it launches an external
program on the entry under the cursor, chosen by `openers.toml` from the file's
mimetype and this OS. `bat` for source, `xdg-open` for a PDF, whatever you put
there.

That is the one command on this page that leaves norte. Openers are
CONFIGURATION and not plugins, deliberately: a WebAssembly plugin has no way to
execute anything, so this is the only door out, and it is one you open by
writing a file yourself.

> ⚠ Whatever you launch runs with **your** permissions, outside every policy norte enforces. Norte hands over the path and steps back; the program decides what it does with it.

While an external program has the terminal, norte does not — it takes it back
when the program exits, and a program launched from here never inherits a
terminal left in mouse mode. See [[mouse]].
