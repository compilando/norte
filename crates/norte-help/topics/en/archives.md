+++
id = "archives"
title = "Inside an archive"
tags = ["remote"]
see_also = ["copying", "remote"]
commands = [
    "nav.enter",
    "nav.parent",
    "pane.view",
    "pane.names-encoding",
    "pane.pack",
    "pane.unpack",
    "pane.test-archive",
    "pane.split-file",
    "pane.combine-files",
]
+++
{{cmd:nav.enter}} on an archive walks into it. The pane lists what is inside,
the cursor moves as usual, {{cmd:pane.view}} opens an entry in the viewer, and
{{cmd:nav.parent}} walks back out. Nothing is unpacked into a folder you have
to find and delete afterwards.

Three containers are addressable, recognised by extension:

| Format | Extension       | Read by                             |
|--------|-----------------|-------------------------------------|
| zip    | .zip            | seeking straight to the entry       |
| tar    | .tar            | seeking straight to the entry       |
| tar+gz | .tar.gz, .tgz   | decompressing; see below            |

A `.tar.gz` cannot be seeked: reaching the last entry means decompressing
everything before it. Browsing one is therefore fine, and reading several
entries out of one would mean starting over each time — so from the second
read of the same archive, norte decompresses it once into a scratch file and
serves the rest from there. That file is anonymous and unlinked, so it never
appears in a listing and the space returns by itself when norte lets go of it,
but while it exists it is as large as the decompressed archive. Above a
gigabyte it is not built at all, and reads fall back to the slow path.

# The ! in the path

An archive path names the container and the entry inside it at once, with a
`!` segment between them:

```
zip+file:///home/you/photos.zip/!/2019/spain.jpg
```

The scheme grows the format, and the `!` marks the boundary. Everything to its
left is an ordinary file on the outer backend — which may itself be remote, so
`zip+sftp://` is a real address and reading a zip on an SSH host needs no
download first.

> ⚠ An archive is **read-only** from the inside: copying out of one is an ordinary copy and works with any destination, copying *into* one is refused. Making a **new** archive is a different thing, and it is below.

# Making one

{{cmd:pane.pack}} writes a new archive from what you marked, or from the entry
under the cursor. It asks for the name, and the name decides the format —
`.zip`, `.tar`, `.tar.gz` or `.tgz`; the dialog says which one it is going to
write before you press Enter. `.rar` is not on that list: norte reads rar by
handing it to an external program, and that program is not asked to write.

The names stored inside are the ones you see on screen, relative to the panel
you packed from. Cancelling leaves nothing behind — no half-written file that
looks like an archive.

{{cmd:pane.unpack}} is the reverse and needs no dialog: it copies the archive's
contents into the other panel, which is an ordinary copy with the ordinary
questions about collisions, and an ordinary undo.

{{cmd:pane.test-archive}} reads every entry to the end and reports what failed.
What "passed" means depends on the format, and the report says so: a zip
carries a CRC per entry, a `.tar.gz` one checksum for the whole stream, and a
plain tar carries none at all — there, all that can be verified is that every
declared size is reachable.

# Splitting a big file

{{cmd:pane.split-file}} cuts a file into numbered pieces — `name.001`,
`name.002` — in the other panel, with the size you ask for (`10M`, `700M`,
`4096`). More than 999 pieces is refused before anything is written, because a
set that runs out of numbers halfway is a set nobody can put back together.

{{cmd:pane.combine-files}} puts them back, starting from the `.001`. A gap in
the numbering, or a middle piece shorter than the first, stops it: a badly
joined file is a corrupt file that looks fine.

# Names that are not UTF-8

A zip entry only promises UTF-8 when bit 11 of its header says so. Older
archives are routinely CP437, CP866 or whatever codepage the machine that
wrote them used, and nothing in the file says which.

norte keeps the raw bytes and refuses to guess in silence. When the names look
wrong, {{cmd:pane.names-encoding}} re-reads them as another encoding for
*display only* — cp437, cp866, Shift-JIS, GBK, windows-1252 — and the bytes in
the archive are untouched, as is what a copy writes at the far end.

An entry whose name cannot be a path at all, such as one containing `..` or
carrying an absolute path, is left out of the listing rather than placed
somewhere it was never meant to go. The pane says how many it dropped.
