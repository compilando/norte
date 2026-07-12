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
modal-collision-keys = [o]verwrite  [s]kip  [r]ename  [n]ewer  [esc]cancel
modal-confirm-keys = [y/enter] go ahead   [n/esc] cancel

# --- Status bar messages ---
msg-done = done
msg-cancelled = cancelled
msg-cancelling = cancelling…
msg-no-tasks = no running tasks
msg-error = error: { $error }
msg-refresh-error = refresh: { $error }
msg-view-error = view: { $error }
msg-config-reloaded = config reloaded
msg-config-not-applied = config NOT applied: { $error }
msg-config-polling = config: watching degraded to polling
msg-no-trash-here = no trash here: F8 again for permanent

# --- Task panel ---
task-cancelled = cancelled

# --- Viewer ---
viewer-forced = (forced)
viewer-lossy = lossy (�)
viewer-truncated = [head]
viewer-binary = binary
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
cli-daemon-stopped = shutdown requested
cli-daemon-hard-shutdown = second signal: cancelling tasks…

# --- Help (F1) — built from the effective keymap ---
help-title = Help — active keys
help-hint = [esc/q/f1] close   [↑/↓/pgup/pgdn] scroll
help-section-browse = Browsing (panes)
help-section-viewer = Viewer
help-cmd-app-quit = quit norte
help-cmd-app-help = this help
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
help-cmd-task-cancel = cancel the most recent task
help-cmd-viewer-close = close the viewer
help-cmd-viewer-up = one line up
help-cmd-viewer-down = one line down
help-cmd-viewer-page-up = page up
help-cmd-viewer-page-down = page down
help-cmd-viewer-top = go to top
help-cmd-viewer-bottom = go to bottom
help-cmd-viewer-encoding = reload as… (next encoding)
help-cmd-viewer-encoding-auto = back to auto-detection
help-cmd-viewer-hex = toggle hex view
