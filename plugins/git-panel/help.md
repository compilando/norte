+++
id = "org.norte.git-panel"
title = "Git panel"
+++
Paints a panel with the state of the repository you are looking at: the
current branch, the commit it points to, and the latest reflog moves —
where you came from, which is what is hard to remember after a while of
jumping between branches.

It is placed like any other slot: the layout picker lists it as one more
panel, and it can sit beside the listing, below it, or in a tab.

It reads the repository root the host confines it to — the ancestor that
holds `.git` — and nothing else: `.git/HEAD` for the branch, `.git/logs/HEAD`
for the commit and the moves. It never writes, never runs `git`, and does not
know where it is: the host hands it a token and it asks for relative paths.

Outside a repository the panel says so in one line instead of staying
blank: an empty slot cannot be told apart from a broken one.

The panel's clickable areas run norte commands, and only the ones any panel
may run: moving between panels, opening another one, resizing. A plugin
chooses the label and the command, so norte does not let it name anything
you could not do with a key while the panel has the keyboard.
