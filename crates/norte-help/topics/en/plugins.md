+++
id = "plugins"
title = "Extensions"
tags = ["extensions"]
see_also = ["settings", "remote", "agents"]
commands = ["app.extensions"]
context = ["dialog.trust-lua", "dialog.plugin-approval"]
+++
{{cmd:app.extensions}} lists what is installed, and for each one two separate
facts: whether you have APPROVED it, and whether it is ENABLED. Nothing runs
until you approve it, and approving is not the same as turning it on — you can
approve an extension and leave it off, or turn one off without withdrawing your
approval.

An extension is WebAssembly. It cannot see your filesystem, open a socket or
run a program on its own: it gets exactly the capabilities its manifest asks
for, and those are what you are approving when you approve it. There is no way
for a plugin to execute anything at all, which is why handing a file to an
external program is configuration instead — see [[viewer]].

That sandbox is load-bearing rather than decorative: the FTP backend on
[[remote]] is a plugin, reaching the network only through a socket the host
opens for it. A whole protocol lives inside the same walls a one-line
extension does.

What a manifest can declare, and what approving it therefore grants:

| Kind | What it adds |
|------------|-----------------------------------------------------|
| provider | a backend, addressed by its own URL scheme |
| previewer | a rendering of a file in the viewer |
| command | a verb in the palette |
| decorator | a badge on the rows of a listing |
| column | a value per entry in the listing |

# Their pages, and how to read them

An extension can ship its own help page, and it appears in this group, next to
this one. Every one of them says on its face that a plugin wrote it, and that
line is there whether or not the plugin declared anything — a page that could
pass for norte's own prose is a page that could tell you approving it is safe.

The text is third-party throughout and is treated that way: bounded, decoded,
and masked before it reaches your screen, so a bidi override in a heading
cannot rearrange what you read. A row for an extension that is not approved and
enabled is dimmed and says which of the two it is — which is the answer you
came for, if you are reading that page to decide whether to turn it on.

A provider is the one kind that answers to a URL: install one that declares
`webdav`, approve it and turn it on, and `webdav://host` opens through it. The
scheme it claims shows among its capabilities as `provider:webdav`, because
that is what approving grants. The schemes norte serves itself — `file`,
`sftp`, `ftp`, `s3` — cannot be claimed by an extension, so approving one never
puts it in front of a built-in backend.

Extensions come in from the command line: `norte plugin install <dir>` brings
one in unapproved, and `norte plugin list` shows the same two facts as this
screen. They leave from either side. The remove key (and, in the window, the
button) on this screen asks first, because uninstalling deletes the
extension's files **and its approval** — a plugin installed later under the
same id starts from nothing. `norte plugin uninstall <id>` does the same
without asking, and a daemon already running does not notice until it
restarts.

> 💡 `norte doctor` reports what is wrong with an installed extension: a manifest that does not parse, a digest that no longer matches, a help page over the size limit.

# A project that brings its own script

A directory can carry an `init.lua` — a script, not a setting — and norte will
not run it until you say so. The question comes up the first time you land
there, and answering it is one key.

The decision is remembered for **that file's contents**, not for its path. Edit
the script and you are asked again, because approving a script is not a blank
cheque for whatever that name holds later. What gets evaluated is the bytes
that were read when you were asked — never a fresh read afterwards, which would
be a window for a different script to slip in between your answer and the run.

Configuration from a project directory follows the same rule and is on
[[settings]].

## Approving an extension

Approving is THE security decision of this system: an approved extension acts
on your behalf with the capabilities it declares — reading under a location,
reaching the network. So norte **asks**, and the question lists them
one per line, each flagged separately when its text is not what it looks like.
`Enter` does not grant: it takes the approve key, the same as an agent's
operation.

**Revoking does not ask**, and enabling something unapproved is refused.
Disabling is always allowed, even if the approval was revoked meanwhile:
disabling goes in the safe direction.

After granting or revoking, the list is asked of the core again. What you see
is what the core believes, not what this screen expected to happen.
