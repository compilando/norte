+++
id = "copying"
title = "Copying, moving and deleting"
tags = ["doing"]
see_also = ["selection", "remote", "archives"]
commands = [
    "pane.copy",
    "pane.move",
    "pane.delete",
    "pane.delete-permanent",
    "task.cancel",
]
+++
Mark what you want in the focused pane and press {{cmd:pane.copy}}. The other
pane is the destination, whatever it is holding — a local directory, an SSH
host, a bucket. An archive is the one thing that can only ever be a source; see
[[archives]].

{{cmd:pane.move}} moves. Within one backend that is a rename: no bytes are
read, no bytes are written, and the size of what you are moving does not matter.
Only when the two ends are different backends — or different filesystems on
this machine — does it degrade into a copy followed by a delete of the source.

{{cmd:pane.delete}} uses the trash when the backend offers one and deletes
permanently when it does not. {{cmd:pane.delete-permanent}} never uses the
trash. Both confirm first, and the confirmation says which of the two is about
to happen.

# Every transfer is a task

A copy is not a frozen screen. It reports progress, it keeps going while you
navigate elsewhere, and {{cmd:task.cancel}} stops the most recent one.

What cancelling promises is per **file**: bytes go to a staging file and the
destination name does not appear until that file is complete, so no half-written
file ever wears the name you were expecting.

A cancelled **tree** is a different matter. The files already copied stay where
they landed — each one whole, none of them half-written, but the directory is
there and it is partial. Nothing sweeps it up for you.

> ⚠ Cancelling a copy INTO object storage is the one case that stays ambiguous: the server may finish a copy it had already begun, so the object can appear after you cancelled, and abandoned multipart parts can go on costing money until a lifecycle rule clears them.

Resumable copies are asked for explicitly, from the command line. There a
cancelled copy leaves a file whose name carries `.norte-partial` —
unmistakable at a glance, and where the next attempt picks up:

```sh
norte cp --resume sftp://host/big.iso ./big.iso
```

# When the name is already taken

The first collision stops the transfer and asks. Your answer is not applied to
that one entry: the whole operation is sent again with that policy in force, so
one answer governs every collision the rest of the way.

| Answer     | What happens                                                 |
|------------|--------------------------------------------------------------|
| overwrite  | what is at the destination is replaced                       |
| skip       | the colliding entries are left alone                         |
| rename     | the copy lands beside it: report.txt becomes report (1).txt  |
| keep newer | replaced only where the source is more recent, else left     |

Answer with the key the dialog shows, not with a default: there is none, because
a dialog that can destroy data should not be answerable by leaning on a key.

> ⚠ **keep newer** does not guess. When either side has no usable timestamp, that entry stops and asks again rather than being replaced or skipped on a hunch.

> ⚠ Case collisions are judged against the **destination**, not the source: `README` and `readme` live together happily on Linux and land on the same file on macOS or Windows, and it is the destination that decides.
