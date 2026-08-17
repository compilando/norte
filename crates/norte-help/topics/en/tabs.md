+++
id = "tabs"
title = "Tabs in a panel"
tags = ["basics"]
see_also = ["panes", "selection"]
commands = [
    "tab.new",
    "tab.close",
    "tab.next",
    "tab.prev",
    "tab.move-left",
    "tab.move-right",
    "tab.goto-1",
    "tab.goto-2",
    "tab.goto-3",
    "tab.goto-4",
    "tab.goto-5",
    "tab.goto-6",
    "tab.goto-7",
    "tab.goto-8",
    "tab.goto-9",
]
+++
A panel can hold several tabs, and each one is a whole listing: its own
directory, cursor, marks and history. Switching tabs remembers nothing because
it forgot nothing.

{{cmd:tab.new}} opens a tab beside the one in front of you, in the same
directory and already filled — it is what you were looking at, so there is
nothing to read again. From there the two move independently.

{{cmd:tab.next}} and {{cmd:tab.prev}} walk the group, wrapping around.
{{cmd:tab.goto-1}} through {{cmd:tab.goto-9}} jump straight to one.
{{cmd:tab.move-left}} and {{cmd:tab.move-right}} reorder the current tab and
stop at the edge: a tab that jumps from last to first on one keypress too many
is not what anyone wanted.

{{cmd:tab.close}} closes the current one. When only one is left the group
disappears and the panel is a panel again — a tab bar with a single tab says
nothing. Closing the last panel on a side is **not** this; that is
`layout.close-slot`.

A tab you cannot see costs nothing: it does not watch its directory and asks
for nothing. It catches up when you come back to it.
