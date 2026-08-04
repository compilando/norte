+++
id = "remote"
title = "SFTP, FTP and object storage"
tags = ["remote"]
see_also = ["copying", "archives"]
commands = ["pane.hotlist", "pane.history", "pane.refresh"]
+++
A pane holds a remote location the same way it holds a directory. The address
is a URL, and its scheme says who answers:

| Scheme | What it reaches                            |
|--------|--------------------------------------------|
| file   | this machine                               |
| sftp   | file transfer over SSH                     |
| ftp    | plain FTP                                  |
| s3     | S3-compatible object storage               |

# Getting there

A remote lives in your favourites. Put it in `norte.toml`:

```toml
[[hotlist]]
name = "work"
path = "sftp://you@host/srv/data"
```

Then {{cmd:pane.hotlist}} opens the list and Enter takes that pane there.
{{cmd:pane.history}} brings you back to somewhere the pane has already been
this session. From that point on every command in this help works unchanged —
{{cmd:pane.refresh}} especially, since a remote directory is not watched and
will not notice a change on its own.

The first time you reach an unknown SSH host, norte shows you its fingerprint
and asks. Compare it out of band before you trust it: that dialog is the only
moment anyone gets to notice a machine-in-the-middle, and Enter deliberately
does not answer it.

# Secrets

A password or an access key is kept in the **system keyring**. The
configuration holds a reference to it and never the secret itself, so a config
directory can be copied, backed up or committed without leaking anything.

That is also why a password does not go in the URL: an address of the form
`sftp://user:pass@host` is rejected rather than quietly accepted, because a URL
ends up in history, in logs and on screen.

> ⚠ Plain FTP encrypts nothing — not the password, not the files. On anything that leaves your own network, prefer `sftp://`.

> ⚠ Object storage has **no directories**. A folder there is a common prefix of the keys under it, so an empty folder exists only if something wrote a marker object for it, and removing the last key under a prefix makes the folder itself disappear. Renaming one is a copy of every key followed by a delete of every key, not an instant operation.
