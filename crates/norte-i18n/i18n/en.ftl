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
