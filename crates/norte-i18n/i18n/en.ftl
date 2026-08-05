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
# H3c: the footer of a modal a help page is covering. While that help is open
# it owns the keys, so the modal's verbs do nothing — a footer that kept
# offering them would lie. The box and the question stay visible (the modal is
# painted last); only the verbs are replaced by what is true.
modal-hint-help-open = close the help to answer this
modal-trust-host-title = Unknown host key
modal-trust-host-host = { $badge }host: { $host }
modal-trust-host-algo = { $badge }algorithm: { $algo }
modal-trust-host-fp = { $badge }fingerprint: { $fingerprint }
modal-trust-host-note = compare it out of band before trusting.
modal-lua-trust-title = Run project init.lua?
modal-lua-trust-body = { $path } (sha256 { $hash }) will run WITH YOUR PERMISSIONS. A cloned repo's script can do anything you can. y = trust and run · n/Esc = deny (remembered until the file changes)
modal-confirm-quit-title = Quit norte?
modal-confirm-quit-body = Close the application.
modal-mark-pattern-add = Mark by pattern
modal-mark-pattern-remove = Unmark by pattern
modal-mark-pattern-hint = glob, for example *.rs
modal-mark-pattern-keys = [enter] confirm · [esc] cancel
modal-mkdir = Create directory
modal-mkdir-hint = name of the new directory
modal-transfer-name-copy = Copy to
modal-transfer-name-move = Move to
modal-transfer-name-hint = destination name (edit to rename)
modal-ai-rename = AI rename — instruction
modal-ai-rename-hint = Enter: request plan · Esc: cancel
modal-ai-rename-empty-instruction = type an instruction first
modal-ai-rename-plan = AI rename — proposed plan
modal-ai-rename-dir = in: { $dir }
modal-ai-rename-pair-from = { $n }. { $from }
modal-ai-rename-pair-to = → { $to }
modal-ai-rename-more = … { $shown }/{ $total } (scroll: ↓/↑)
modal-ai-rename-plan-hint = y/Enter: apply · n/Esc: discard
modal-semantic = Semantic search
modal-semantic-hint = Enter searches · Esc cancels
modal-semantic-empty-query = Type a query first
modal-semantic-hits = Semantic hits
modal-semantic-hit = { $n }. { $path } · { $score }
modal-semantic-more = … { $shown }/{ $total } (scroll: ↓/↑)
modal-semantic-hits-hint = y/Enter: open location · n/Esc: close
msg-transfer-name-fffd = the name still contains the replacement character — retype it cleanly
msg-transfer-name-same = same name and place: nothing to do
msg-transfer-name-failed = could not enqueue — the name is kept

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
# El rastro de atrás/adelante se acabó: la tecla lo DICE, porque una tecla
# que calla es indistinguible de una rota.
msg-nav-no-back = no further back
msg-nav-no-forward = nothing to go forward to
# Un pane virtual de búsqueda no es una ubicación: una lista de hits no se
# puede mandar al otro pane ni traer de él.
msg-pane-not-a-location = search results are not a location: nothing to send
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
viewer-plugin-preview-lossy = [lossy decode]
eol-mixed = mixed EOL
eol-none = no EOL

# --- CLI ---
cli-runtime-error = norte: could not start the runtime: { $error }
cli-enqueue-copy = could not enqueue the copy
cli-enqueue-move = could not enqueue the move
cli-enqueue-delete = could not enqueue the delete
cli-enqueue-mkdir = could not enqueue the mkdir
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
status-hidden = { $n } hidden
# --- Column cells (#108) ---
col-time-now = now
col-header-name = Name
col-header-size = Size
col-header-mtime = Modified
col-header-kind = Kind
col-kind-dir = dir
col-kind-file = file
col-kind-symlink = symlink
col-kind-other = other
col-time-min = { $n }m ago
col-time-hour = { $n }h ago
col-time-day = { $n }d ago
col-time-year = { $n }y ago
col-cell-yes = yes
col-cell-no = no
col-attr-posix-mode = Mode
col-attr-posix-uid = UID
col-attr-posix-gid = GID
col-attr-posix-nlink = Links
col-attr-posix-ctime-ms = Changed
col-attr-win-attributes = Attributes
col-attr-s3-etag = ETag
col-attr-s3-content-type = Content type
col-attr-archive-method = Method
col-attr-archive-packed-size = Packed
col-attr-archive-crc32 = CRC-32
status-marked = { $n } marked, { $size }
status-marked-with-dirs = { $n } marked, { $size } + { $dirs } dirs
status-marks-pruned = { $n } marks dropped, their entries are gone
status-watch-degraded = directory watching degraded to polling (inotify limit?) — creates/deletes/renames show up within seconds; edits to existing files are not detected
msg-names-encoding = names shown as { $enc } (display only; bytes unchanged)
msg-names-encoding-off = names shown as-is (reinterpretation off)
msg-hidden-shown = hidden entries shown
msg-mkdir-in-search = search results have no destination directory — leave the search first
msg-ai-rename-running = AI rename: thinking… (Esc cancels)
# GUI variant: the GUI has no path to abort the in-flight request, so it must
# not promise "Esc cancels" (never a false affordance).
gui-msg-ai-rename-running = AI rename: thinking…
# A retained plan (waiting behind an open modal) was replaced by a newer
# request before the human could review it — the loss is said, never silent.
gui-msg-ai-rename-superseded = AI rename: previous plan discarded (new request)
msg-ai-rename-empty = AI rename: the model proposed no changes
msg-ai-rename-failed = AI rename failed: { $error }
msg-ai-rename-applied = AI rename: { $n } move(s) submitted
msg-ai-rename-invalid-plan = AI rename: invalid plan from the daemon — nothing applied
msg-ai-rename-in-search = AI rename is not available in a search pane
msg-semantic-running = Semantic search: thinking… (Esc cancels)
# GUI variant: the GUI has no path to abort the in-flight request, so it must
# not promise "Esc cancels" (never a false affordance).
gui-msg-semantic-running = Semantic search: thinking…
# Retained hits (waiting behind an open modal) were replaced by a newer
# result before the human could review them — the loss is said, never silent.
gui-msg-semantic-superseded = Semantic search: previous hits discarded (new result)
msg-semantic-empty = Semantic search: no hits
msg-semantic-failed = Semantic search failed: { $error }
msg-semantic-invalid = Semantic search: invalid response from the daemon — nothing shown
msg-semantic-in-search = Semantic search is not available in a search pane
msg-hidden-hidden = hidden entries hidden
# H3b: Enter on a help row that documents an OVERLAY verb (`dialog.*`). Those
# are not dispatchable from a pane, so nothing runs — said out loud, because a
# swallowed Enter reads as a command that ran.
msg-help-not-runnable = that row documents an overlay key, not a command a pane can run
msg-help-modal-waiting = answer the dialog first: a command run from here would push it off the screen
msg-marked-by-pattern = { $n } marks changed
msg-mouse-capture-failed = the terminal did not accept mouse capture: norte stays keyboard-only
# Drag in flight, BOTH frontends: what a release would do right now. Never
# GUI-only — the TUI status bar says the same thing from the same source
# (`Drag::pending`), so the two cannot promise different drops.
drag-copy = Drop to COPY { $n } item(s) → { $to }   (hold shift to move)
drag-move = Drop to MOVE { $n } item(s) → { $to }   (release shift to copy)
msg-open-missing-program = opener needs `{ $program }` — not installed
msg-open-remote = openers only work on local files
msg-open-launched = opened with { $program }
msg-open-failed = could not launch { $program }: { $error }
cli-ls-skipped = warning: { $n } container entries omitted from the index (hostile names/limits)

# --- Help (F1) — built from the effective keymap ---
help-title = Help
help-section-browse = Browsing (panes)
help-section-viewer = Viewer
help-section-dialog = Dialogs and overlays
help-dialog-note = each dialog supports its own subset of these keys
# --- Help overlay (H3b) — sidebar group headers and the synthetic keyboard
# page. `help-group-{tag}` is looked up from the corpus' OWN first tag (see
# the front matter of `norte-help/topics/*/*.md`): a new tag there needs its
# entry here, in both locales, or the sidebar paints the lookup key.
help-group-basics = Basics
help-group-doing = Doing things
help-group-remote = Remote & archives
# The synthetic `keys` entry gets NO header: it is a group of one by
# construction and its header would be the same word as its only row, so the
# sidebar does not paint it (`ui::draw_help`). Hence no `help-group-keys`.
help-topic-keys = Keyboard
help-cmd-app-quit = quit norte
help-cmd-app-help = this help
help-cmd-app-theme = choose theme
help-cmd-app-extensions = extension manager
help-cmd-app-palette = command palette
help-cmd-app-settings = settings
# --- Command palette (H1 T4) — a free-text filter editor like the search
# dialog (decision 8): its keys are hardcoded, NOT resolved through the
# `dialog` context, so this hint is a static string like `search-hint`.
#
# Arrows and paging are NOT listed, for the reason `without_navigation` gives
# (H1 MAJOR-1): they are self-evident and the box is 60 cells wide, so spelling
# them out cut the rest of the footer mid-word instead.
palette-title = Command palette
palette-hint = [enter] run · [esc] close
# H3c, and a SEPARATE key on purpose: `palette-hint` is painted by both
# frontends, and only the TUI has a help overlay for F1 to open (the GUI's is
# phase H3f). Folded into the string above, the GUI's footer would advertise a
# key that does nothing there. When H3f lands, the GUI joins this group too.
palette-hint-help = [f1] help
# H3c: F1 on a row opens the page that documents that command. When no page
# does, the palette stays open and says so — opening the index instead would
# leave the reader working out what it had to do with what they asked.
msg-palette-no-help = no help page documents this command yet
# --- Settings overlay (S3) — same free-text-filter idiom as the palette
# above (decision 8): search is always active, Enter toggles/cycles/edits.
settings-title = Settings
settings-hint = [↑/↓/pgup/pgdn] navigate · [enter] edit · [esc] close
settings-edit-hint = [enter] save · [esc] cancel
settings-section-general = General
settings-section-plugins = Plugins
settings-plugins-name = Plugin settings
settings-plugins-note = No installed plugin declares configurable settings.
settings-plugins-open-hint = [enter] open this plugin's settings
settings-plugins-key-count = {$count} settings
# --- GUI settings view (S4) — mouse-driven full-view swap over the same
# catalog/state machine as the overlay above.
settings-restart-badge = restart required
settings-hint-gui = [↑/↓/pgup/pgdn/click] navigate · [enter/click] edit · [esc] close
# P1: prefix on a plugin-contributed row (`palette::plugin_rows`) — no
# built-in row ever carries it, so a plugin cannot spoof a built-in command
# by copying its exact display text.
palette-plugin-prefix = extension
theme-picker-title = Theme
columns-picker-title = Columns — { $target }
columns-picker-target-default = all schemes
columns-picker-hint-gui = Space toggle · Shift+↑/↓ move · S sort · F format · Enter apply · Esc close
msg-columns-saved = columns saved
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
# G3c: `dialog.confirm` on a plugin with an empty `[config]` schema.
msg-plugin-config-empty = this plugin declares no configurable settings
# G3c: plugin.set_config succeeded — { $key } is the manifest-declared,
# charset-safe config key (never plugin free text); { $value } is the new
# value already validated client-side.
msg-plugin-config-saved = { $key } saved: { $value }
msg-settings-saved = { $name } saved: { $value }
msg-settings-save-failed = not saved: { $error }
# S review I1: the background write task itself panicked or was cancelled
# (never observed in practice — the one known panic source, an unexpected
# `[section]` shape, is now a clean `Err` from `persist_set` — this is the
# defense-in-depth arm for anything else that could still crash that task).
# No `{ $error }` placeholder on purpose: a join failure carries no clean,
# localizable category the way an `io::Error` kind does.
msg-settings-save-crashed = internal error while saving — the value was not written
msg-settings-invalid-int = not a number
msg-settings-invalid-range = value must be between { $min } and { $max }
msg-settings-no-config-dir = no user config directory (env not set)
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
help-cmd-nav-back = back to the previous directory
help-cmd-nav-forward = forward again
help-cmd-pane-mirror = send this location to the other pane
help-cmd-pane-pull = go where the other pane is
help-cmd-pane-swap = swap the two panes
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
# G3c: GUI-only commands (no TUI equivalent — its multi-select/task strip
# use different bindings) surfaced now that the GUI's command palette lists
# `app.palette`/`app.extensions` and needs help text for every GUI command.
# `mark.toggle` moved into the shared presets (#103) — kept here since the
# id predates that move and both frontends still reference it.
help-cmd-mark-toggle = toggle mark on the entry under the cursor
help-cmd-mark-all = mark every visible entry
help-cmd-mark-invert = invert the marks
help-cmd-mark-clear = clear every mark
help-cmd-mark-pattern-add = mark by pattern
help-cmd-mark-pattern-remove = unmark by pattern
help-cmd-task-next = highlight the next task
help-cmd-task-prev = highlight the previous task
help-cmd-task-dismiss = dismiss finished tasks from the strip
help-cmd-viewer-close = close the viewer
help-cmd-viewer-up = one line up
help-cmd-viewer-down = one line down
help-cmd-viewer-page-up = page up
help-cmd-viewer-page-down = page down
help-cmd-viewer-top = go to top
help-cmd-viewer-bottom = go to bottom
help-cmd-viewer-encoding = reload as… (next encoding)
help-cmd-pane-names-encoding = show names as… (cp437/cp866/Shift-JIS/GBK/…; display only)
help-cmd-pane-toggle-hidden = show or hide hidden entries
help-cmd-pane-columns = column picker
help-cmd-pane-mkdir = create a directory (F7)
help-cmd-pane-ai-rename = AI rename of the current directory (reviewable plan)
help-cmd-pane-semantic-search = Semantic search over the index (AI)
help-cmd-pane-rename = rename in place (Shift+F6)
help-cmd-pane-refresh = reload both panes (Ctrl+R)
help-cmd-viewer-encoding-auto = back to auto-detection
help-cmd-viewer-hex = toggle hex view
help-cmd-pane-search = search by name/content (Alt+F7)
help-cmd-pane-copy-path = copy the path of the selection to the clipboard
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
gui-menu-acts-on = acts on { $target }
gui-menu-target-marks = { $n } marked items
gui-menu-entry-disabled = { $label } — { $reason }
gui-menu-hint = ↑/↓ move   Enter run   Esc close
gui-menu-open = Open
gui-menu-view = View
gui-menu-copy = Copy to the other pane
gui-menu-move = Move to the other pane
gui-menu-rename-ai = Rename with AI (whole folder)…
gui-menu-delete = Delete
gui-menu-copy-path = Copy path
gui-menu-reason-read-only = read-only backend
gui-menu-reason-wrong-target = does not apply to this selection
gui-menu-reason-unavailable = unavailable right now
gui-menu-copied = { $n } path(s) copied to the clipboard
gui-modal-rename-title = Rename
gui-modal-rename-from = current: { $name }
gui-modal-rename-to = new: { $name }
gui-modal-rename-footer = Enter rename   Esc cancel
gui-menu-rename = Rename…
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
gui-task-kind-mkdir = mkdir
gui-task-kind-undo = undo
gui-task-kind-search = search
gui-task-kind-index = index
gui-task-kind-embed = embed
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
gui-modal-quit-title-empty = Quit norte?
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
dialog-cmd-move-up = move up
dialog-cmd-move-down = move down
dialog-cmd-sort = sort by
dialog-cmd-cycle-format = format
dialog-cmd-pane = other pane
dialog-cmd-back = back
dialog-cmd-filter = filter

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

# --- Settings registry (S2): curated GENERAL settings shown by the TUI
# overlay (S3) and the GUI view (S4). One name/desc pair per entry in
# `norte_frontend::settings::catalog()`.
setting-ui-theme-name = Theme
setting-ui-theme-desc = Color preset for the UI (or a path to a custom theme file, ADR 0020).
setting-ui-lang-name = Language
setting-ui-lang-desc = Interface language. Leave unset to negotiate from the environment.
setting-ui-font-name = UI font
setting-ui-font-desc = Font family for GUI chrome (window text, not the listing). Ignored by the TUI.
setting-ui-mono-font-name = Monospace font
setting-ui-mono-font-desc = Font family for listings and the file viewer. Ignored by the TUI.
setting-ui-font-size-name = Font size
setting-ui-font-size-desc = Base UI font size in pixels (8-32). Ignored by the TUI.
setting-ui-quick-search-name = Quick search mode
setting-ui-quick-search-desc = What `/` does: narrow the listing (filter) or move the cursor without changing it (jump).
setting-ui-reduce-motion-name = Reduce motion
setting-ui-reduce-motion-desc = Turn off animated effects (CRT flicker, cursor blink) for accessibility. GUI only.
setting-ui-mouse-name = Mouse
setting-ui-mouse-desc = Whether the TUI captures the mouse (click, wheel, drag to mark). While it is captured the terminal cannot select text with the mouse; hold Shift to select anyway, or turn this off. Ignored by the GUI.
setting-ui-confirm-quit-name = Confirm before quitting
setting-ui-confirm-quit-desc = When quitting asks for confirmation: only with pending work (auto), always, or never. An emergency-exit shortcut, where bound (e.g. the TUI's Ctrl+C), always bypasses this.
setting-keymap-preset-name = Keymap preset
setting-keymap-preset-desc = Base key-binding preset (orthodox, vim, or cua). User/project layers can still rebind on top.
