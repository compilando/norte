# English catalog for norte. Full parity with es.ftl (tested).

# --- TUI modals ---
modal-trash-title = To trash
modal-trash-note = recoverable from the system trash
modal-delete-permanent-title = PERMANENT delete
modal-delete-permanent-warning = ⚠ NO trash here: this cannot be undone
modal-copy-title = Copy
modal-move-title = Move
modal-collision-title = Collision
modal-collision-body = destination already exists:
modal-approval-title = Agent approval
modal-approval-body = agent "{ $session }" requests { $op }:
modal-approval-path = { $badge }path { $n }: { $path }
modal-trust-host-title = Unknown host key
modal-trust-host-host = { $badge }host: { $host }
modal-trust-host-algo = { $badge }algorithm: { $algo }
modal-trust-host-fp = { $badge }fingerprint: { $fingerprint }
modal-trust-host-note = compare it out of band before trusting.
modal-lua-trust-title = Run project init.lua?
modal-lua-trust-body = { $path } (sha256 { $hash }) will run WITH YOUR PERMISSIONS. A cloned repo's script can do anything you can. y = trust and run · n/Esc = deny (remembered until the file changes)

# --- Status bar messages ---
msg-done = done
msg-cancelled = cancelled
msg-cancelling = cancelling…
msg-no-tasks = no running tasks
msg-error = error: { $error }
msg-refresh-error = refresh: { $error }
# Errores por CATEGORÍA (spec §17.7): localizados, jamás el string del OS.
err-not-found = not found
err-permission-denied = permission denied
err-conflict-exists = destination already exists
err-conflict-case = name collides by case with an existing entry
err-conflict-normalization = name collides after Unicode normalization
err-conflict-type = destination is a different type
err-conflict = conflict at the destination
err-provider-unavailable = the location is unavailable (retryable)
err-no-space = no space left on destination
err-io = I/O error
err-cancelled = cancelled
err-policy-denied = denied by policy
err-encoding-loss = the operation would lose data in transcoding
err-unsupported = not supported here
err-invalid-path = invalid path
err-internal = internal error
err-loop = symlink loop
err-corrupt = not a valid archive/container
err-limit-exceeded = container exceeds local safety limits (not opened)
err-host-key-unknown = unknown host key (first contact)
err-host-key-mismatch = host key MISMATCH — possible MITM
err-cursor-expired = the listing expired; refresh
err-unknown = unknown error
err-config-io = cannot read { $path }: { $error }
err-config-parse = invalid TOML in { $path }: { $detail }
err-keymap-preset-unknown = unknown preset { $name }; available: { $available }
err-keymap-invalid = invalid keymap: { $detail }
# --- Lua scripting (M4, ADR 0026) — detalles SIEMPRE por detail_for_bar ---
err-lua-load = init.lua ({ $layer }): { $detail }
err-lua-unknown = unknown Lua command: { $name }
err-lua-command = Lua command failed: { $detail }
err-lua-cancelled = Lua command cancelled
err-lua-timeout = Lua command timed out
err-lua-statusbar = Lua statusbar disabled: { $detail }
err-lua-no-state-dir = project init.lua not loaded: no state directory (XDG_STATE_HOME/HOME)
# Ya en la cima: no hay directorio padre (raíz `/` o raíz de unidad Windows).
msg-nav-at-top = already at the top
msg-view-error = view: { $error }
msg-config-reloaded = config reloaded
msg-daemon-lost = daemon connection lost; reconnecting…
msg-daemon-restored = reconnected to daemon
msg-config-not-applied = config NOT applied: { $error }
msg-config-polling = config: watching degraded to polling
msg-no-trash-here = no trash here: F8 again for permanent
msg-list-incomplete = incomplete listing: cut off while filling
msg-lua-busy = a Lua command is already running (queued)
msg-lua-queue-full = Lua command discarded: queue full
msg-lua-denied-changed = project init.lua previously denied; it has changed (not loaded)
msg-lua-symlink = project init.lua is a symlink; not loaded
msg-lua-keymap-project = project keymap.toml: { $n } lua: binding(s) ignored (not trusted)
pane-loading = loading… ({ $n })
quicksearch-partial = (partial)

# --- Task panel ---
task-cancelled = cancelled

# --- Viewer ---
viewer-forced = (forced)
viewer-lossy = lossy (�)
viewer-truncated = [head]
viewer-binary = binary
viewer-plugin-preview = via { $plugin }
eol-mixed = mixed EOL
eol-none = no EOL

# --- CLI ---
cli-runtime-error = norte: could not start the runtime: { $error }
cli-enqueue-copy = could not enqueue the copy
cli-enqueue-move = could not enqueue the move
cli-enqueue-delete = could not enqueue the delete
cli-list-failed = list failed
cli-entry-unreadable = unreadable entry
cli-serialize-failed = could not serialize
cli-cancelling = cancelling…
cli-cancelled-clean = cancelled (destination clean)
cli-final-error = error: { $error }
cli-unexpected-state = unexpected final state: { $state }
cli-daemon-listening = daemon listening on { $socket }
cli-mcp-serving = MCP over stdio (session { $session }); Ctrl-D to stop
cli-scope-granted = scope granted
cli-undo-done = session { $session } undone
cli-undo-failed = undo incomplete: { $error }
cli-undo-report = { $undone } reverted, { $skipped_irreversible } irreversible skipped
cli-undo-left-in-place = { $count } created item(s) left in place (destination has no trash); delete explicitly if you want them gone
cli-undo-blocked = undo stopped at journal entry { $seq } ({ $error }): earlier entries were NOT undone
cli-undo-report-unavailable = undo finished but its report could not be fetched ({ $error }): outcome unverified
cli-audit-open-failed = could not open the journal (is the daemon running? audit needs it stopped)
cli-audit-empty = journal is empty: nothing to anchor
cli-audit-anchored = anchored journal head at seq { $seq }
cli-audit-chain-ok = hash chain intact ({ $entries } entries)
cli-audit-chain-broken = hash chain BROKEN at journal entry { $seq }
cli-audit-no-anchors = no anchors file: run `norte audit anchor` to pin the current head (absence is a FAILURE unless --allow-no-anchors: an attacker can simply delete the file)
cli-audit-anchor-bad = anchor at line { $line } FAILED: { $detail }
cli-audit-coverage = anchor coverage: up to seq { $anchored } of chain head { $head }
cli-audit-verdict-bad-line = unreadable anchor line
cli-audit-verdict-bad-mac = invalid MAC (forged anchor, or key rotated without re-anchoring?)
cli-audit-verdict-missing = anchored seq { $seq } NO LONGER EXISTS (tail truncation/rollback)
cli-audit-verdict-mismatch = seq { $seq } exists with a DIFFERENT hash (history rewritten)
cli-audit-anchors-ok = { $count } anchor(s) verified against the chain
cli-plugin-run-failed = plugin run failed: { $error }
cli-daemon-stopped = shutdown requested
cli-daemon-hard-shutdown = second signal: cancelling tasks…
cli-hostkey-unknown = first connection to { $host }:{ $port } — unregistered host key
cli-hostkey-fingerprint = fingerprint { $algo }: { $fingerprint }
cli-hostkey-prompt = Trust this key and record it in known_hosts? [y/N]
cli-hostkey-refused = connection aborted: host key not confirmed
cli-hostkey-noninteractive = non-interactive input: confirm the host key with `norte connect <url>` in a terminal, or pre-populate known_hosts (env NORTE_KNOWN_HOSTS)
cli-hostkey-trusted = host key recorded
cli-connect-ok = connection established: { $target }
cli-connect-failed = could not connect
cli-connect-daemon-unsupported = `norte connect` does not work with --daemon yet (use embedded mode)
cli-invalid-url = invalid remote URL: { $url }
cli-confirm-read = confirmation read
cli-inline-password = the URL must not carry an inline password (user:pass@…); secrets go through the keyring/env/secrets.age
cli-connection-degraded = ⚠ { $scheme }://{ $host }: UNENCRYPTED session (server rejected AUTH TLS, tls="allow"). Data and credentials travel in cleartext.
status-connection-degraded = ⚠ { $scheme }://{ $host } — plaintext
status-archive-skipped = ⚠ { $n } entries omitted (hostile names/limits)
status-names-encoding = names: { $enc }
msg-names-encoding = names shown as { $enc } (display only; bytes unchanged)
msg-names-encoding-off = names shown as-is (reinterpretation off)
msg-open-no-opener = no opener configured for { $mime } (see openers.toml)
msg-open-missing-program = opener needs `{ $program }` — not installed
msg-open-remote = openers only work on local files
msg-open-launched = opened with { $program }
msg-open-failed = could not launch { $program }: { $error }
cli-ls-skipped = warning: { $n } container entries omitted from the index (hostile names/limits)

# --- Help (F1) — built from the effective keymap ---
help-title = Help — active keys
help-hint = [esc/q/f1] close   [↑/↓/pgup/pgdn] scroll
help-section-browse = Browsing (panes)
help-section-viewer = Viewer
help-cmd-app-quit = quit norte
help-cmd-app-help = this help
help-cmd-app-theme = choose theme
help-cmd-app-extensions = extension manager
help-cmd-app-palette = command palette
# --- Command palette (H1 T4) — a free-text filter editor like the search
# dialog (decision 8): its keys are hardcoded, NOT resolved through the
# `dialog` context, so this hint is a static string like `search-hint`.
palette-title = Command palette
palette-hint = [↑/↓/pgup/pgdn] navigate · [enter] run · [esc] close
# P1: prefix on a plugin-contributed row (`palette::plugin_rows`) — no
# built-in row ever carries it, so a plugin cannot spoof a built-in command
# by copying its exact display text.
palette-plugin-prefix = extension
theme-picker-title = Theme
ext-title = Extensions
ext-empty = no extensions installed
ext-unapproved = not approved
history-title = History
history-empty = no history yet
hotlist-title = Favorites
hotlist-empty = empty — add the current directory from the popup
hotlist-name-prompt = name:
hotlist-invalid = invalid path
msg-theme-applied = theme applied: { $name }
msg-theme-reverted = theme unchanged
msg-theme-saved = theme saved: { $name } → { $path }
msg-theme-save-failed = theme applied (not saved): { $error }
msg-hotlist-saved = favorite saved: { $name }
msg-hotlist-removed = favorite removed: { $name }
# P1: result of Enter on a plugin-command palette row — { $output } is
# untrusted plugin output, already masked+capped by `detail_for_bar` before
# reaching here (#73 pattern). The "extension:" prefix marks it as
# third-party text, same vocabulary as `palette-plugin-prefix`.
msg-plugin-run-ok = extension: { $output }
msg-hotlist-persist-failed = favorites not saved: { $error }
help-cmd-pane-switch = switch pane
help-cmd-cursor-up = move cursor up
help-cmd-cursor-down = move cursor down
help-cmd-cursor-page-up = page up
help-cmd-cursor-page-down = page down
help-cmd-cursor-top = go to top
help-cmd-cursor-bottom = go to bottom
help-cmd-nav-enter = enter the selected directory
help-cmd-nav-parent = go to parent directory
help-cmd-pane-copy = copy selection to the other pane
help-cmd-pane-move = move selection to the other pane
help-cmd-pane-delete = delete (trash when available)
help-cmd-pane-delete-permanent = PERMANENT delete
help-cmd-pane-view = view the selected file
help-cmd-pane-open = open the selected file with an external program (openers.toml)
help-cmd-pane-quick-search = quick search in pane (filter/jump)
help-cmd-pane-history = directory history
help-cmd-pane-hotlist = favorite directories
help-cmd-task-cancel = cancel the most recent task
help-cmd-viewer-close = close the viewer
help-cmd-viewer-up = one line up
help-cmd-viewer-down = one line down
help-cmd-viewer-page-up = page up
help-cmd-viewer-page-down = page down
help-cmd-viewer-top = go to top
help-cmd-viewer-bottom = go to bottom
help-cmd-viewer-encoding = reload as… (next encoding)
help-cmd-pane-names-encoding = show names as… (cp437/cp866/Shift-JIS/GBK/…; display only)
help-cmd-viewer-encoding-auto = back to auto-detection
help-cmd-viewer-hex = toggle hex view
help-cmd-pane-search = search by name/content (Alt+F7)
search-title = Search
search-name = name (glob):
search-content = content:
search-regex = [F2] regex: { $on }
search-case = [F3] case: { $on }
search-hint = [tab] field · [enter] search · [esc] cancel
search-empty = enter a name or content criterion
search-status-running = search: { $n } hits (searching…)
search-status-done = search: { $n } hits
search-status-truncated = search: { $n } hits (truncated)
search-status-cancelled = search: { $n } hits (cancelled)
search-status-failed = search failed: { $error }
on-yes = on
on-no = off

# --- GUI (GUI-e T1) — banners, states, modals, task strip ---
gui-banner-keymap-error = keymap: { $error }
gui-banner-config-invalid = invalid configuration: { $error }
gui-banner-op-rejected = operation rejected: { $error }
gui-banner-viewer-error = viewer { $name }: { $error }
gui-banner-error = error: { $error }
gui-loading = loading…
gui-dir-empty = (empty directory)
gui-tasks-empty = (no tasks)
gui-viewer-opening = opening viewer…
gui-viewer-image-unreadable = unreadable image
gui-a11y-pane-left = left pane
gui-a11y-pane-right = right pane
gui-a11y-tasks = tasks in progress
gui-modal-copy-title = Copy { $n } item(s) → { $to }
gui-modal-move-title = Move { $n } item(s) → { $to }
gui-modal-delete-title = Delete { $n } item(s)
gui-modal-mode-trash = TRASH
gui-modal-mode-permanent = PERMANENT
gui-modal-conflict-title = Conflict: { $conflict }
gui-modal-more = … and { $n } more
gui-modal-footer-transfer = y confirm   n/Esc cancel
gui-modal-footer-delete = y confirm   p toggle permanent   n/Esc cancel
gui-modal-footer-conflict = o overwrite   s skip   c/Esc cancel
gui-task-kind-copy = copy
gui-task-kind-move = move
gui-task-kind-delete = delete
gui-task-kind-undo = undo
gui-task-kind-search = search
gui-task-kind-index = index
gui-task-kind-unknown = task
gui-task-state-pending = pending
gui-task-state-running = running
gui-task-state-paused = paused
gui-task-state-done = done
gui-task-state-cancelled = cancelled
gui-task-state-failed = failed
gui-task-state-unknown = ?
cli-gc-result = swept { $n } orphaned partials under { $dir }
cli-gc-remote-unsupported = `norte gc` does not work with --daemon yet (use embedded mode)
cli-ai-rename-plan = Proposed renames (review before applying):
cli-ai-rename-empty = the model proposed no renames
cli-ai-rename-confirm = Apply these renames? [y/N]
cli-ai-rename-abort = aborted; nothing was renamed
cli-ai-rename-done = renamed { $n } file(s)
gui-modal-quit-title = Quit with { $tasks } task(s) running and { $marks } mark(s)?
gui-modal-footer-quit = y confirm   n/Esc cancel
gui-banner-theme-io = theme { $spec }: { $error }
gui-banner-theme-parse = theme { $spec }: { $detail }
gui-banner-config-io = configuration { $path }: { $error }
gui-banner-config-parse = configuration { $path }: { $detail }
gui-banner-effects-key-skipped = theme effects: skipped { $key }
gui-banner-font-unknown = font { $family } not found; using the default

# --- Dialog footer hints (H1 T3, closes #24) — generated: supported
# dialog.* commands × the effective dialog keymap × these labels. NEVER a
# hand-written footer string again: a rebind can't desync it.
dialog-cmd-confirm = confirm
dialog-cmd-cancel = cancel
dialog-cmd-approve = approve
dialog-cmd-deny = deny
dialog-cmd-overwrite = overwrite
dialog-cmd-skip = skip
dialog-cmd-rename = rename
dialog-cmd-newer = keep newer
dialog-cmd-up = up
dialog-cmd-down = down
dialog-cmd-page-up = pg up
dialog-cmd-page-down = pg dn
dialog-cmd-add = add
dialog-cmd-toggle-enabled = toggle
dialog-cmd-remove = remove

# --- norte doctor (H2): read-only diagnostics over config layers,
# keymaps, plugins and connections.
cli-doctor-title = norte doctor — read-only diagnostics
cli-doctor-section-config = -- config --
cli-doctor-section-keymap = -- keymap --
cli-doctor-section-plugins = -- plugins --
cli-doctor-section-connections = -- connections --
cli-doctor-ok = OK
cli-doctor-warn = WARN
cli-doctor-error = ERROR
cli-doctor-footer-keymap-approx = note: an "unknown command" warning is an approximation against the three bundled presets' own bindings for that screen — a frontend-specific command with no default binding anywhere is invisible to this check.
cli-doctor-footer-connections-not-probed = note: keyring/age are not probed (side-effect-free diagnostics only) — only the presence of the env-var fallback is checked; use `norte connect` to test a connection for real.
cli-doctor-detail-connections-parse = connections.toml does not parse (fix or remove it)
cli-doctor-detail-connections-none = no connections.toml, or no connections configured
cli-doctor-detail-plugin-digest-stale = { $id }: manifest capabilities changed since approval; re-approval required
