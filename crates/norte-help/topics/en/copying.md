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
host, a bucket, the inside of an archive. {{cmd:pane.move}} is the same
operation and removes the source once the copy has landed.

{{cmd:pane.delete}} uses the trash when the backend offers one and deletes
permanently when it does not. {{cmd:pane.delete-permanent}} never uses the
trash. Both confirm first, and the confirmation says which of the two is about
to happen.

# Every transfer is a task

A copy is not a frozen screen. It reports progress, it keeps going while you
navigate elsewhere, and {{cmd:task.cancel}} stops the most recent one.

Cancelling is the interesting case, because a half-written file wearing the
name you expected is worse than no file at all. Bytes go to a staging file
first and the destination name does not exist until the copy has finished, so
cancelling from here leaves the destination **clean**: nothing half-written,
nothing to tidy up.

The exception is resumable copies, which you ask for explicitly from the
command line. There, what a cancelled copy leaves is a file whose name carries
`.norte-partial` — unmistakable at a glance, and picked up by the next attempt:

```sh
norte cp --resume sftp://host/big.iso ./big.iso
```

# When the name is already taken

Each collision is asked about, one entry at a time, and the answer is yours:

| Answer    | What happens                                        |
|-----------|-----------------------------------------------------|
| Overwrite | the entry at the destination is replaced            |
| Skip      | this one is left alone and the rest carry on        |
| Rename    | the copy lands beside it, with a (2) suffix         |
| Newer     | replaced only if the source is more recent, else skipped |

There is no "apply to all", and Enter is not an answer: a dialog about
destroying data has no innocuous default to lean on.

> ⚠ Case collisions are judged against the **destination**, not the source: `README` and `readme` live together happily on Linux and land on the same file on macOS or Windows, and it is the destination that decides.
