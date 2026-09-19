+++
id = "dialogs"
title = "Answering a dialog"
tags = ["basics"]
see_also = ["copying", "help", "panes"]
commands = [
    "dialog.confirm",
    "dialog.cancel",
    "dialog.up",
    "dialog.down",
    "dialog.page-up",
    "dialog.page-down",
    "dialog.top",
    "dialog.bottom",
    "dialog.section-prev",
    "dialog.section-next",
    "app.quit",
]
context = ["dialog.quit"]
+++
Every overlay in norte — a confirmation, a picker, a list, this help — speaks
the same six verbs:

- {{cmd:dialog.confirm}} accepts what the dialog is showing
- {{cmd:dialog.cancel}} closes it, changing nothing
- {{cmd:dialog.up}} and {{cmd:dialog.down}} move through it
- {{cmd:dialog.page-up}} and {{cmd:dialog.page-down}} move a screen at a time

In this help, {{cmd:dialog.top}} and {{cmd:dialog.bottom}} also go to the start
and the end of the index or the page, {{cmd:dialog.section-prev}} and
{{cmd:dialog.section-next}} jump to the previous or next section of the page,
and in the text the arrows scroll it a line at a time until an action is in
view.

Each dialog supports the subset that means something in it, and the footer is
GENERATED from that subset rather than written by hand. What the footer offers
is what the dialog accepts — there is no key it takes quietly and none it
advertises and ignores.

Rebind any of them and every overlay follows, including this page. The keys
in this help are looked up in your keymap as the page is drawn; see [[help]].

# The rule that matters

**A dialog that can destroy data has no default answer.** Confirming is not
bound to a key you would press by reflex to get past something, and a
collision does not resolve itself towards `overwrite` because you leaned on
`⏎`. You answer with the key the dialog shows, having read what it says.

That rule shapes the ones you will meet most:

| Dialog | What it is asking |
|-------------------|-----------------------------------------------------|
| confirmation | shall I touch these files, and how many |
| collision         | the name at the destination is already taken        |
| approval | an agent wants to act; approving is never the default|
| host key | this host is new, or its key changed |

An overlay that only changes your own configuration is the exception, and it
says so by behaving differently: the column picker and the theme picker apply
on `⏎`, because the worst outcome is a listing you did not want and one more
keystroke to undo it.

# Quitting

{{cmd:app.quit}} is the one confirmation that mutates nothing, which makes it
a good place to learn the grammar: it asks, `⏎` accepts and cancel goes back to
where you were.

By default it only asks when something is pending — a running task, marks you
have not used — and closes straight away when there is nothing to lose. You can
make it always ask, or never ask, from the settings. See [[settings]].

> ⚠ Quitting cancels the tasks that are still running. A cancelled copy leaves each file it finished intact and the directory it was filling partial; nothing sweeps it up for you.

An emergency exit, where your keymap binds one, bypasses the question entirely.
That is deliberate: a shortcut whose purpose is getting out of a wedged screen
cannot be the shortcut that opens another dialog.
