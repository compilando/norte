# 0104 — An extension is uninstalled from the manager, and the manager has buttons

- Status: accepted; its two "not done" uninstall gaps are closed by ADR 0113
- Date: 2026-09-10
- Decision makers: Oscar González
- Related: ADR 0022 (plugin consent is manifest + digest), ADR 0066 (the
  window is a renderer over `norte-ui-host`), ADR 0077 (a decision taken
  once, in the shared layer), ADR 0089 (the RPC catalogue and its gates),
  #280 (approval enumerates what it grants)

## Context

Two reports from real use of the window's extension manager (F12):

1. It was a flat, read-only list. The keyboard could approve (`dialog.add`),
   switch on and off (`dialog.toggle-enabled`) and open the settings sheet,
   but nothing on the screen said so, and the mouse could only select a row.
   A reader coming from an editor's extension view expects the row to say
   what it is, a detail pane to say what it does, and buttons to say what can
   be done with it.
2. Uninstalling did not exist outside the command line. `norte plugin
   uninstall <id>` deletes `plugins/<id>/` and leaves the state entry switched
   off and unapproved, but a running daemon **kept listing the deleted plugin
   — and decorating listings with it — until restart**, because its registry
   lives in memory and nothing told it. The manager on either frontend could
   not do it at all: there was no wire method.

A third thing surfaced while looking: the `file-icons` decorator was reported
as "not working". It was working. Its `[config] style` had been left at
`ascii`, so every badge was a piece of punctuation (`{}`, `''`, `#`) glued to
the file name — indistinguishable from noise. That is a configuration fact,
not a defect, and it is recorded here only because the manager is the screen
where a reader would have found it.

## Decision

**1. `plugin.uninstall` is a wire method (protocol 0.71.0).** Params `{id}`,
result `{was_approved}`. It does what the CLI does — `norte_core::plugins::
uninstall`, the same function, which validates the id as reverse-DNS BEFORE
turning it into a path, deletes the directory, and leaves the state entry
switched off and unapproved — and then does what the CLI could not: the
daemon forgets the plugin in its in-memory registry
(`PluginRegistry::forget_in_memory`), so `plugin.list` and `plugin.decorate`
stop serving it at once.

It is gated to `Actor::User`, like `plugin.set_approval` and
`plugin.set_enabled`: withdrawing a consent is the human's as much as giving
it, and deleting files from their configuration, more so. An agent gets
`INVALID_REQUEST`. An id that is not an id, or is not installed, is
`INVALID_PARAMS` — never a path, and never the I/O error text, which would
quote the user's home directory back to whoever asked.

There is deliberately no `plugin.install` on the wire. Installing needs a
source directory on the machine running the CLI; the manager has no file
picker for that and would only be inventing one.

**2. Uninstalling always asks, on both frontends, through the shared
model.** The question names the extension (masked, flagged) and its id, and
its body says the two things that are lost: the files, and the approval — a
plugin installed later under the same id starts unapproved. "Uninstall?"
alone reads as "switch it off for good?", and it is not that.

In the host it is `Pendiente::DesinstalarExtension`, a dialog whose
affirmative choice is `confirm` with the label `dialog-uninstall` and
`destructive: true` — the same answer id as deleting files, because that is
what it is; `approve` stays reserved for granting capabilities. In the
terminal it is `Modal::ConfirmPluginUninstall`, painted as a warning modal
with the name AND the id, and answered through `ALLOW_CONFIRM` — but unlike
a permanent delete, only `dialog.confirm` is a yes there: `dialog.approve`
is the key the reader was just pressing in the list to grant capabilities,
and on this modal it cancels.

**The daemon serializes every write of `plugins-state.toml`.** `persist_state`
merges a snapshot over the file, and the registry's `std::sync::Mutex` cannot
be held across the `spawn_blocking` that writes. Two human connections — a
window and a terminal — could therefore persist in the reverse order of
their snapshots, and with an uninstall in between that resurrected on disk a
consent anchored to a manifest that no longer existed, which a plugin
reinstalled under the same id would inherit. `Shared::plugins_state_io`, a
`tokio::sync::Mutex<()>`, is now taken before the in-memory mutation and
released after the persist in all three governance handlers. It closes the
older lost-update between `set_approval` and `set_enabled` as well.

The key is `dialog.remove` — the verb that removes an entry from the hotlist
list — in the extensions context. It is bound in all seven presets (the four
imported ones take their `[dialog]` block from `orthodox`), so the footer
hint appears everywhere without a new chord.

**3. The window's manager becomes two panes with buttons (bridge 61).** The
left pane lists rows: name, version, a state pill that says both facts
(approved, enabled), publisher · category, the description, and the
capabilities as chips — still in the row, because they are the decision. The
right pane is the selected extension: name, meta, description, and a button
row — **Approve** or **Revoke**, **Enable** or **Disable**, **Help** when it
ships a page, **Uninstall** — followed by the settings sheet when it has been
requested, or a hint saying how to request it. A header counts what is
installed and what is on, and a close button sends the same `Escape` the
keyboard would.

The buttons carry no logic. `extension_govern {row, change}` selects the row
and calls the same controller path as the key: `gobernar_elegida`. So
approving from a button opens the same consent dialog that enumerates the
capabilities (#280); enabling an unapproved extension is refused with the
same `host-extension-not-approved`; uninstalling asks the same question. A
label is resolved from state (`Revoke` on an approved row, never "toggle"),
and the one thing a button decides on its own is to be disabled — with the
host's own refusal as its `title` — when the host would refuse anyway.
`extension_help {row}` closes the manager and opens the help at that
extension's page, which is what `F1` over the row does in the terminal.

## Consequences

- Protocol 0.70 → 0.71, additive. A 0.70 client loses nothing: it does not
  know the method and does not call it. Catalogue, goldens and schema
  regenerated; the daemon test covers the agent gate, the deletion, the
  in-memory forget, the withdrawn consent on reinstall, and the two
  `INVALID_PARAMS` shapes including `../fuera`.
- Bridge 60 → 61, two new actions, no DTO change. The window's test suite
  pins the button labels per state, the actions they send, that a button
  click does not re-select the row, and that a disabled button says why.
- The host test suite pins that the button opens the same question as the
  key, that cancel sends nothing, that the catalogue is re-fetched after the
  confirmation and no longer lists the extension, and that a row index past
  the end is `Stale`, not an action on whatever is now at that index.
- The `plugins` help page now says an extension leaves from either side, and
  that both ask.
- **The terminal's manager was levelled the same day** (2026-09-10, later):
  two columns from 64 useful cells, the same detail pane — state as two
  facts, description, capabilities, counts, the settings table inside the
  pane, the commands — painted from the same `PluginInfo` the window reads,
  with the compact list on the left. The keys did not change; the ADR 0077
  rule holds because the two managers never had two rules, only two screens.
  And any governance or settings change now makes every open listing forget
  its decorations and ask again (a batch in flight is dropped by
  generation), on both frontends — switching a decorator off used to leave
  its glyphs on the rows until the next `cd`.
- **Not journalled, on purpose.** Hard rule 4 asks that a mutation without an
  undo path be classified. Uninstalling mutates the user's configuration,
  not their files; it has no undo by wire (there is no `plugin.install`);
  and its siblings — approving, enabling, writing a plugin's settings — are
  not journalled either. The question the human answered is the record, and
  the daemon logs the id and whether consent was withdrawn.
- `PluginUninstallResult::was_approved` is informational today: the CLI
  prints it; the manager on both frontends already said in the question that
  the approval goes, and re-fetches the catalogue instead of composing a
  second message from the answer.
- The terminal's footer on the manager reads `[d] remove`, the catalogue's
  generic label for `dialog.remove`; the modal it opens says "Uninstall". A
  per-context label is a larger change than this one. The terminal's
  APPROVE modal also does not print the id — a pre-existing gap the uninstall
  modal does not share.
- `norte plugin uninstall` still writes the disk behind a running daemon's
  back: the CLI calls `plugins::uninstall` directly and the daemon does not
  watch the directory, so on THAT path the stale registry survives until the
  daemon restarts (it does, on idle). Routing the CLI through the daemon when
  one is listening is the fix, and is out of this change's scope. **Closed by
  ADR 0113: `--daemon plugin uninstall` goes through the daemon, and without
  it the command warns.**
- Not done, on purpose: a search box (the host has no filter model for this
  list), per-row inline buttons (the pane is where the decision is read), an
  icon per extension (manifests carry none), and uninstalling a plugin that
  failed to load from the window — the handler supports it (a broken plugin
  is in `errors`, not in the catalogue; `uninstall` deletes it and
  `forget_in_memory` drops it from `errors` too), the button does not yet
  exist. **The last one is closed by ADR 0113: a broken extension is a row of
  the manager, in both frontends.**
