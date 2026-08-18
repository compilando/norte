+++
id = "remote"
title = "SFTP, FTP and object storage"
tags = ["remote"]
see_also = ["copying", "archives"]
commands = [
    "pane.hotlist",
    "pane.history",
    "pane.refresh",
    "dialog.add",
    "dialog.remove",

    "pane.connect",
    "pane.disconnect",]
context = ["dialog.trust-host"]
+++
A pane holds a remote location the same way it holds a directory. The address
is a URL, and its scheme says who answers:

| Scheme | What it reaches                            | Served by          |
|--------|--------------------------------------------|--------------------|
| file   | this machine                               | built in           |
| sftp   | file transfer over SSH                     | built in           |
| ftp    | plain FTP                                  | a sandboxed plugin |
| s3     | S3-compatible object storage               | built in           |

FTP is the odd one out: it is served by a WebAssembly plugin that ships inside
norte and touches the network only through a socket the host opens for it, at
an address the host resolved and vetted first. That is a deliberate place to
put the oldest and least trustworthy of the four.

# Getting there

A remote lives in your favourites. Put it in `norte.toml`:

```toml
[[hotlist]]
name = "work"
path = "sftp://you@host/srv/data"
```

Then {{cmd:pane.hotlist}} opens the list and confirming takes that pane there.
{{cmd:pane.history}} brings you back to somewhere the pane has already been
this session. From that point on every command in this help works unchanged —
{{cmd:pane.refresh}} especially, since a remote directory is not watched and
will not notice a change on its own.

You do not have to edit the file to keep that list: inside the favourites popup
{{cmd:dialog.add}} adds the pane's current location under a name you type, and
{{cmd:dialog.remove}} drops the highlighted one. Both write `norte.toml`, which
is the same list you would have edited by hand. The history popup takes
neither — there is nothing to name in a place you have simply been, and nothing
to delete from a record of this session.

A favourite is only an address. How a connection authenticates, and what it is
allowed to do, lives separately in `connections.toml` — so a favourite gives
away nothing but a path.

The first time you reach an unknown SSH host, norte shows you its fingerprint
and asks. Compare it against the host through some other channel before you
accept: that dialog is the only moment anyone gets to notice a machine sitting
in the middle, and it deliberately has no default answer.

# Secrets

`connections.toml` holds references, never secrets. When a connection needs
one, it is looked for in three places, in order: the `NORTE_SECRET_`
environment variable for that connection, then the **system keyring**, then an
encrypted `secrets.age` file. The first hit wins, so a machine with no keyring
— a server, a container — still works through the other two.

An S3 access key id is not on that list, because it is not a secret: it is an
identifier, and it sits in `connections.toml` in the clear. The secret access
key beside it does go through the resolver.

It is also why a password never goes in the URL: an address of the form
`sftp://user:pass@host` is rejected rather than quietly accepted, because a URL
ends up in history, in logs and on screen.

> ⚠ FTP is plaintext. Not "unless you turn on TLS" — there is no FTPS yet, so the setting means nothing and the password and every byte of every file cross the network in the clear. Each FTP connection says so. Off your own network, use `sftp://`.

> ⚠ Object storage has **no directories**. A folder there is a common prefix of the keys under it, so an empty folder exists only if something wrote a marker object for it, and removing the last key under a prefix makes the folder itself disappear. Renaming one is a copy of every key followed by a delete of every key, not an instant operation.

# Opening and closing a connection

{{cmd:pane.connect}} shows the connections in your `connections.toml` and takes
the panel to the one you pick. The list comes from that file, so what you see
here is what you wrote there — name and address, never a password: credentials
are referenced rather than stored, which is what the keyring is for.

{{cmd:pane.disconnect}} does both things its name promises: it **releases the
session** — the socket closes now, not when it eventually times out — and sends
the panel back to your home directory. On a local panel there is nothing to
close and it says so, rather than answering "done" to something it did not do.

Closing does not forbid: next time you navigate there, norte connects again the
usual way.
