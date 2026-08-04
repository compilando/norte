+++
id = "archives"
title = "Inside an archive"
tags = ["remote"]
see_also = ["copying", "remote"]
commands = ["nav.enter", "nav.parent", "pane.view", "pane.names-encoding"]
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

> ⚠ An archive is **read-only**. Copying out of one is an ordinary copy and works with any destination; copying into one is refused.

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
