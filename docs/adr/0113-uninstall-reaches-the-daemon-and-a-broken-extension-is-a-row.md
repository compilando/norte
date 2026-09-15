# 0113 — Uninstall reaches the daemon, and a broken extension is a row

- Status: accepted
- Date: 2026-09-15
- Decision makers: Oscar González
- Related: ADR 0104 (uninstall from the manager, whose two "not done" items
  this closes), ADR 0066 (the SDK and the host bridge)

## Context and problem statement

ADR 0104 made uninstalling an extension a wire method, `plugin.uninstall`,
and left two gaps written down:

1. `norte plugin uninstall` called `plugins::uninstall` directly. The daemon
   discovers its catalogue once at start-up and does not watch the plugins
   directory, so after the CLI deleted a plugin a running daemon kept listing
   it — and decorating rows with it — until it restarted.
2. An extension that failed to load sits in `PluginListResult.errors`, not in
   the catalogue. The handler already deleted and forgot such a directory, but
   neither manager could ask for it: the cursor only walked the catalogue. The
   only way to remove a broken extension was deleting its directory by hand.

## Decision

**The CLI uninstalls through the daemon only when asked to, with
`--daemon`.** Then it checks the id against the DAEMON's catalogue and calls
`plugin.uninstall`, which deletes in the daemon's config directory and forgets
the plugin in memory. Without `--daemon` it deletes in its own config directory
as before, and if a daemon accepts on the socket it warns that the daemon will
keep listing the plugin until it restarts.

The first version of this change routed automatically to any daemon listening
on the socket, and review found that unsafe. The default socket is derived
from the user, not from `NORTE_CONFIG_DIR`, and nothing in `initialize` says
which directory a daemon serves. A CLI run with another config directory — a
sandbox, or any test that isolates with that variable — would have deleted the
extension and withdrawn its approval in the running daemon's directory, with
no journal entry to undo it. Asking the daemon for its directory would be a
protocol change, and an explicit flag already means "the daemon's world" for
every other `plugin` subcommand.

The "is it installed" check happens before the call because the handler
answers both refusals as `INVALID_PARAMS` with no taxonomy in `data`, which
the SDK turns into `Internal`.

**A broken extension is a row of the manager, in both frontends.** Broken
rows come after the loaded ones, in the order they crossed: row
`rows.len() + j` is `errors[j]`. The cursor, the keys and the mouse walk
them, and the window restores a selected broken row by what it shows when the
catalogue is fetched again, not by position. On a broken row the only verb is
uninstall, with the same key (`dialog.remove`), the same button and the same
question as a loaded one. Approving, enabling or opening settings is refused
with `ext-broken-only-uninstall`.

**A broken directory is offered for uninstall only if its name is an
extension id that no loaded extension uses.** `plugin.uninstall` deletes
`plugins/<id>/` and validates the id before turning it into a path, so
`caf\xff` or `a b` has nothing to send. And discovery does not require a
directory to be named like its manifest's `id`: `plugins/org.a/` can load as
`org.b` next to a broken `plugins/org.b/`, and uninstalling `org.b` would
withdraw the loaded extension's approval as well. The rule lives once, in
`norte_frontend::broken_plugin::uninstallable_id`, and reads the directory's
BYTES when the peer sends them (#265). Without an id there is no button, and
the manager says `ext-broken-not-id`.

Bridge 69 carries it: `ExtensionErrorView.id: Option<String>`, and the
`ExtensionsView.cursor` that continues past `rows` into `errors`.
`extension_select_row` and `extension_govern` name those rows by the same
count, and `extension_govern` keeps checking the row's id, so a click on a
row that moved is refused as stale.

## Consequences

- Protocol unchanged. Bridge 68 → 69; the renderer's constant moves with it
  and the corpus shape pin was updated in the same change.
- Without `--daemon`, the warning is all a CLI user gets: the running daemon
  still lists the plugin until it restarts. Frontends start their daemon with
  a two-second idle timeout, and both managers can now uninstall directly.
- With `--daemon`, a daemon older than protocol 0.71 has no
  `plugin.uninstall`; the CLI reports the failure and does not fall back to
  deleting on disk, because the user asked for the daemon's directory.
- A loaded extension whose directory is not named like its id is still
  uninstalled by id, which deletes `plugins/<id>/` — not its own directory.
  That predates this ADR. The robust fix is discovery refusing a manifest
  whose id differs from its directory name, which would turn such plugins into
  broken rows; it is left for its own change.
- `norte plugin install` still writes behind a running daemon: there is no
  wire method to install, and a newly installed plugin arrives unapproved, so
  the stale view only hides something the human still has to approve.
