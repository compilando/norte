+++
id = "tabs"
title = "Tabs in a panel"
tags = ["basics"]
see_also = ["panes", "selection"]
commands = [
    "pane.tab-new",
    "pane.tab-close",
    "pane.tab-next",
    "pane.tab-prev",
    "pane.tab-move-left",
    "pane.tab-move-right",
    "pane.tab-goto-1",
    "pane.tab-goto-2",
    "pane.tab-goto-3",
    "pane.tab-goto-4",
    "pane.tab-goto-5",
    "pane.tab-goto-6",
    "pane.tab-goto-7",
    "pane.tab-goto-8",
    "pane.tab-goto-9",
]
+++
A panel can hold several tabs, and each one is a whole listing: its own
directory, cursor, marks and history. Switching tabs remembers nothing because
it forgot nothing.

{{cmd:pane.tab-new}} opens a tab beside the one in front of you, in the same
directory and already filled — it is what you were looking at, so there is
nothing to read again. From there the two move independently.

{{cmd:pane.tab-next}} and {{cmd:pane.tab-prev}} walk the group, wrapping around.
{{cmd:pane.tab-goto-1}} through {{cmd:pane.tab-goto-9}} jump straight to one.
{{cmd:pane.tab-move-left}} and {{cmd:pane.tab-move-right}} reorder the current tab and
stop at the edge: a tab that jumps from last to first on one keypress too many
is not what anyone wanted.

{{cmd:pane.tab-close}} closes the current one. When only one is left the group
disappears and the panel is a panel again — a tab bar with a single tab says
nothing. Closing the last panel on a side is **not** this; that is
`layout.close-slot`.

A tab you cannot see costs nothing: it does not watch its directory and asks
for nothing. It catches up when you come back to it.

With the mouse: click a tab to go to it, `[+]` to open another and `[x]` to
close the one you are looking at. Clicking a panel's bar gives it the focus
first — clicking on one side and having the other act would be the opposite of
what the finger said.
