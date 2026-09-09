+++
id = "org.norte.rename-log"
title = "Rename log"
+++
A hook: every time a rename is recorded in the journal — yours, an agent's, a
batch rename, an undo — this plugin puts a line in the status bar saying how
many files it touched. It changes nothing and it cannot stop anything: it
sees the entry after it is durable.

It listens to `after-renamed` only. The message is attributed to the plugin,
and if the plugin fails three times in a row norte switches its hooks off and
says so.
