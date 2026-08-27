# English catalog for norte. Full parity with es.ftl (tested).

# --- TUI modals ---
modal-trash-title = To trash
modal-trash-note = recoverable from the system trash
modal-delete-permanent-title = PERMANENT delete
modal-delete-permanent-warning = ⚠ NO trash here: this cannot be undone
modal-copy-title = Copy
modal-move-title = Move
modal-drop-title = Copy what you dropped
modal-collision-title = Collision
modal-collision-body = destination already exists:
modal-approval-title = Agent approval
modal-semantic-title = Search by meaning
modal-semantic-scope = across the whole index (built with `norte index build`)
msg-semantic-no-index = nothing indexed yet: run `norte index build` and then `norte index embed`
msg-semantic-unsupported = this daemon cannot search by meaning
msg-semantic-bad-hits = the answer did not look like a result set and was discarded
search-title-semantic = By meaning
modal-approval-ttl = expires in { $s } s
modal-approval-ttl-unknown = deadline unknown (rebuilt after a reconnect): it may already have expired
dialog-subject = asks to:
dialog-asker = agent:
msg-approval-not-delivered = approval { $id } did not reach the daemon: the operation is still denied
msg-approval-expired = approval { $id } expired before your answer: the operation was denied
msg-approval-already-decided = approval { $id } had already been decided by someone
msg-approval-unknown = approval { $id } is not from this daemon (did it restart?)
msg-dialog-dropped = too many open dialogs: the oldest one was dropped
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
modal-new-file = Create file
modal-new-file-hint = name of the new file
modal-transfer-name-copy = Copy to
modal-transfer-name-move = Move to
modal-transfer-name-hint = destination name (edit to rename)
# The typed destination: F5/F6 when there is no other panel to copy to (the
# `simple` layout). The address is the wire form, the same one `[[hotlist]]`
# takes, prefilled with this panel's own.
modal-transfer-dest-copy = Copy to address
modal-transfer-dest-move = Move to address
modal-transfer-dest-hint = address, for example file:///home/you/work
# `pane.command-line` (#135): the free-text prompt whose Enter runs
# `$SHELL -c CMD` in the pane's directory, with the TUI suspended. Same mould
# as the mkdir/AI-rename prompts.
modal-command-line = Run a command
modal-command-line-hint = runs in the active pane's directory
modal-command-line-empty = type a command first
modal-command-line-too-long = the command line is full ({ $max } characters); the rest was not typed
modal-ai-rename = AI rename — instruction
modal-ai-rename-hint = Enter: request plan · Esc: cancel
modal-ai-rename-empty-instruction = type an instruction first
modal-ai-rename-plan = AI rename — proposed plan
modal-ai-rename-dir = in: { $dir }
modal-ai-rename-pair-from = { $n }. { $from }
modal-ai-rename-pair-to = → { $to }
modal-ai-rename-more = … { $shown }/{ $total } (scroll: ↓/↑)
modal-ai-rename-real-steps = { $n } will actually be renamed
modal-ai-rename-hidden-hostile = ⚠ a name you cannot see is painted differently from what it is
modal-ai-rename-apply = Apply
modal-ai-rename-discard = Discard
gui-modal-ai-rename-plan-hint = y or Apply: apply · n/Esc or Discard: discard
modal-ai-rename-plan-hint = y/Enter: apply · n/Esc: discard
# The plan cannot be applied (collisions, or still being checked): the footer
# must not offer a key that does nothing.
modal-rename-batch-plan-hint-blocked = n/Esc: discard
# State of the transactional batch plan (fs.rename_batch_plan), shown under
# the pairs. "pending" = the core has not answered yet.
modal-rename-batch-pending = batch: checking…
modal-rename-batch-applicable = batch: applicable — one task, one undo
modal-rename-batch-not-applicable = batch: NOT applicable — nothing will be renamed
# The check itself did not happen (or failed). Distinct from "checking…": that
# one resolves by itself, this one does not, and a spinner that never advances
# is a lie. The reason went to the bar.
modal-rename-batch-unchecked = batch: NOT checked — nothing will be renamed
# Planner machinery: renames through a temporary name to break a cycle. Only
# the COUNT is shown; the temporary names are never presented as proposals.
modal-rename-batch-temp = via { $n } internal step(s) (a cycle needs a detour)
# One collision per line. The offending name goes LAST so a truncation can
# never swallow the verdict.
modal-rename-batch-collision = ✗ { $n }. { $kind }: { $name }
# Same line for a verdict whose pair_index does not point at any row of the
# request: the index is dropped rather than pointing at a row that is not there.
modal-rename-batch-collision-unindexed = ✗ { $kind }: { $name }
modal-rename-batch-collision-prefix = ✗ { $n }. { $kind }:
modal-rename-batch-collision-prefix-unindexed = ✗ { $kind }:
modal-rename-batch-temp-part = { $n } planner temp step(s)
modal-rename-batch-collision-internal = another pair took it
modal-rename-batch-collision-external = already exists
modal-rename-batch-collision-absent-source = source not there
modal-rename-batch-collision-ambiguous-source = ambiguous source
modal-rename-batch-collision-unknown = unknown verdict
modal-rename-batch-collision-more = … { $shown }/{ $total } collisions
modal-semantic = Semantic search
modal-semantic-hint = Enter searches · Esc cancels
modal-semantic-empty-query = Type a query first
modal-semantic-hits = Semantic hits
modal-semantic-hit = { $n }. { $path } · { $score }
modal-semantic-more = … { $shown }/{ $total } (scroll: ↓/↑)
modal-semantic-hits-hint = y/Enter: open location · n/Esc: close
modal-volumes-more = … { $shown }/{ $total } (scroll: ↓/↑)
msg-transfer-name-fffd = the name still contains the replacement character — retype it cleanly
msg-transfer-name-same = same name and place: nothing to do
msg-transfer-name-failed = could not enqueue — the name is kept

# --- Status bar messages ---
msg-done = done
msg-pack-warnings = packed, but { $risky } name(s) mean something else on another system
msg-pack-warnings-partial = packed, but at least { $risky } name(s) mean something else on another system
msg-cancelled = cancelled
msg-cancelling = cancelling…
msg-no-tasks = no running tasks
msg-task-finished = that task already finished
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
err-plan-stale = the folder changed; review the rename plan again
err-plan-not-executable = the rename plan has name collisions
err-journal-unavailable = this session's journal cannot be opened, so nothing was changed: nothing would be recorded and nothing could be undone. The reason reported alongside names what to repair — the state directory, or journal.db inside it
err-unknown = unknown error
err-config-io = cannot read { $path }: { $error }
err-config-parse = invalid TOML in { $path }: { $detail }
err-keymap-preset-unknown = unknown preset { $name }; available: { $available }
err-keymap-invalid = invalid keymap: { $detail }
# --- Lua scripting (M4, ADR 0026) — detalles SIEMPRE por detail_for_bar ---
err-lua-load = init.lua ({ $layer }): { $detail }
err-lua-profile-ignored = the profile's init.lua was ignored: a profile declares, it does not run code
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
# A pasted newline must never submit a field (#143): only the first line is
# inserted, and this says how many more were dropped.
msg-paste-truncated = pasted the first line; { $lines } more discarded
msg-view-error = view: { $error }
msg-config-reloaded = config reloaded
msg-profile-switched = profile: { $profile }
msg-profile-switched-partial = profile: { $profile } — needs a restart to apply: { $keys }
msg-profile-not-applied = the profile { $profile } could not be applied
msg-daemon-lost = daemon connection lost; reconnecting…
msg-daemon-restored = reconnected to daemon
msg-daemon-handover = the daemon is handing over; it will be back
msg-daemon-stopping = the daemon is stopping
msg-config-not-applied = config NOT applied: { $error }
msg-config-polling = config: watching degraded to polling
msg-no-trash-here = no trash here: F8 again for permanent
msg-list-incomplete = incomplete listing: cut off while filling
msg-lua-busy = a Lua command is already running (queued)
msg-lua-queue-full = Lua command discarded: queue full
msg-lua-denied-changed = project init.lua previously denied; it has changed (not loaded)
msg-lua-symlink = project init.lua is a symlink; not loaded
msg-lua-keymap-project = project keymap.toml: { $n } lua: binding(s) ignored (not trusted)
# `--cd-file` (S3, shell.rs `cd_bytes`): the active pane was not `file://`, so
# nothing was written to the cd-file and the wrapper leaves the shell where it
# is. `$path` is already run through `path_display`'s hostile-name masking,
# with a leading `!` in place of the badge colour a plain stderr line cannot
# carry.
msg-cd-not-local = the active pane was { $path }; the shell stays put
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
# The header declares a size this window will not decode. A 64 KB PNG can
# claim 60000x60000 and cost the decoder gigabytes; reading the header and
# refusing is the only cheap defence.
viewer-image-too-large = image too large to preview
# The header does not say anything we understand. Treating that as "go
# ahead" is the door the budget exists to close.
viewer-image-unreadable = image header not understood
viewer-image-loading = loading image…
viewer-plugin-preview-lossy = [lossy decode]
eol-mixed = mixed EOL
eol-none = no EOL

# --- CLI ---
cli-runtime-error = norte: could not start the runtime: { $error }
cli-enqueue-copy = could not enqueue the copy
cli-enqueue-move = could not enqueue the move
cli-enqueue-delete = could not enqueue the delete
cli-enqueue-mkdir = could not enqueue the mkdir
# S3 (shell-integration): `Shell::parse`'s vocabulary is exactly these three
# tokens — named here instead of echoed back the way `cli-help-unknown-topic`
# does, because this one is a closed set, not a corpus the user could plausibly
# have misspelled a new page against.
cli-shell-init-unknown = unknown shell "{ $shell }" — supported: bash, zsh, fish
cli-list-failed = list failed
cli-entry-unreadable = unreadable entry
cli-serialize-failed = could not serialize
cli-compare-failed = compare failed
cli-compare-incomplete = compare did not finish cleanly: { $state }
cli-sync-failed = sync failed
cli-sync-incomplete = sync plan did not finish cleanly: { $state }
cli-sync-empty = nothing to synchronize
cli-sync-plan = Sync plan:
cli-sync-unjournalled = without a journal this sync cannot be applied — norte refuses to write a tree it could not undo. Another process holds it (a running `ntc` or daemon owns journal.db exclusively): use --daemon to go through it
cli-sync-journal-unreadable = without a journal this sync cannot be applied — norte refuses to write a tree it could not undo. This session's journal could NOT BE OPENED (it is not merely held by another process), so `--daemon` is no remedy: the daemon refuses to start on that same file. The reason above names what to repair
cli-ai-rename-refused = these renames were NOT made: this session's journal could not be opened, so nothing would be recorded and nothing could be undone
cli-sync-noninteractive = there is no terminal to ask, and nothing was applied — use --yes to apply without a question
cli-sync-blocked = the plan cannot run, so nothing was applied
# A blocker is two fields on two lines, not one joined by `: ` — the same
# reason as the failure rows below (corpus `cause_join_spoof`).
cli-sync-blocker = { $rel }
cli-sync-blocker-why = { $why }
cli-sync-blockers-more = … and { $n } more
cli-sync-integrity = the plan above is not all of the plan that would be applied, so it will not be: { $detail }
cli-sync-nothing-to-apply = every step is a skip: there is nothing to apply
cli-sync-confirm = Apply this plan? [y/N]
cli-sync-abort = aborted; nothing was applied
cli-sync-done = applied: { $done } done, { $failed } failed, { $skipped } skipped
cli-sync-cancelled = cancelled — what was applied before the cut stays, journalled; the rest was not applied
# One failure is THREE fields on THREE lines, never one joined by `: ` and
# ` → `: both joiners are ordinary printable characters that the name masker
# leaves alone, so a filename can forge a whole fabricated row in band (corpus
# `cause_join_spoof`). A newline is Cc, so it is masked — a name cannot forge
# a line break, which makes it the pipe's structural separator.
cli-sync-step-dest = to the destination: { $dest }
cli-sync-step-reason = why: { $reason }
cli-sync-failure = { $rel }
cli-sync-failure-dest = on the destination: { $dest }
cli-sync-failure-cause = failed: { $cause }
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
cli-audit-anchored-marker = the format marker (seq 0) is anchored too: a re-declared format can no longer pass as "I cannot read this"
cli-audit-marker-ok = the format marker (seq 0) is anchored and matches
cli-audit-marker-unanchored = the journal declares a format marker and NOTHING anchors it: a re-declared or injected marker would look identical from here. Run `norte audit anchor`, and if you already had, its anchor file was removed
cli-audit-only-marker = no mutations yet: the format marker is anchored and there is nothing else to anchor
cli-audit-chain-ok = hash chain intact ({ $entries } entries)
cli-audit-chain-broken = hash chain BROKEN at journal entry { $seq }
cli-audit-chain-unknown-format = journal declares format { $declared }, this build knows { $known }: it CANNOT be verified here — upgrade norte and verify again. This is not a clean bill of health, and not an accusation either: a re-declared marker looks the same from here, so read the anchor report below before believing either reading.
cli-audit-chain-unverifiable-from = first entry this build could not recompute: { $seq }
cli-audit-chain-not-certified = the chain was NOT certified by this build (verdict unknown to this version): treat the journal as unverified
cli-audit-format-unreadable = unreadable
cli-audit-no-anchors = no anchors file: run `norte audit anchor` to pin the current head (absence is a FAILURE unless --allow-no-anchors: an attacker can simply delete the file)
cli-audit-anchor-bad = anchor at line { $line } FAILED: { $detail }
cli-audit-coverage = anchor coverage: up to seq { $anchored } of chain head { $head }
cli-audit-verdict-bad-line = unreadable anchor line
cli-audit-verdict-bad-mac = invalid MAC (forged anchor, or key rotated without re-anchoring?)
cli-audit-verdict-missing = anchored seq { $seq } NO LONGER EXISTS (tail truncation/rollback)
cli-audit-verdict-mismatch = seq { $seq } exists with a DIFFERENT hash (history rewritten)
cli-audit-anchors-ok = { $count } anchor(s) verified against the chain
cli-audit-anchors-ok-unverified-chain = { $count } anchor(s) match the STORED hashes — which is not a verified chain: this build could not recompute the entries they cover
cli-plugin-run-failed = plugin run failed: { $error }
cli-daemon-stopped = shutdown requested
# A handover asked of a daemon too old to know what one is. It performed an
# ordinary shutdown instead, so the frontends were told nothing and will not
# come back on their own — which is worth saying, because the CLI would
# otherwise report success for something that did not happen. This is the
# FIRST upgrade to 0.46 by definition, so it is the common case, not a corner.
cli-daemon-handover-unsupported = this daemon speaks protocol { $version } and does not know about handovers: it was stopped instead, so open windows will not reconnect on their own
cli-daemon-handover-requested = handover requested: the windows will come back when the replacement is up
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
status-connection-degraded = ⚠ UNENCRYPTED session
status-connections-degraded = ⚠ UNENCRYPTED session (and { $n } more)
status-degraded-subject = { $banner } — scheme { $scheme }, host { $host } ({ $reason })
degraded-reason-ftp-plaintext = FTP without encryption
degraded-reason-tls-auth-rejected = the server rejected TLS
degraded-reason-unknown = unknown reason
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
msg-project-config-skipped = { $n } project config did not load and was ignored
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
msg-ai-rename-invalid-plan = AI rename: invalid plan from the daemon — nothing applied
msg-ai-rename-in-search = AI rename is not available in a search pane
# The AI plan is applied through the transactional batch executor (spec §17):
# ONE task, ONE undoable journal unit, rollback on failure.
msg-rename-batch-plan-failed = batch rename: could not check the plan: { $error }
msg-rename-batch-no-plan = batch rename: no checked plan — nothing applied
msg-rename-batch-collisions = batch rename: the plan collides — nothing applied
msg-rename-batch-applied = batch rename: { $n } rename(s) submitted as one batch
msg-rename-batch-failed = batch rename failed: { $error }
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
# GUI-only (2026-08-10-volumes.md task V4): same doctrine as the semantic
# search trio above — "running" cannot promise "Esc cancels" (no abort path),
# a superseded fetch says so instead of dropping it silently, and "failed"
# wraps the daemon's already-flattened error text.
gui-msg-volumes-running = Drives: loading…
gui-msg-volumes-superseded = Drives: previous list discarded (new result)
gui-msg-volumes-failed = Could not list drives: { $error }
msg-hidden-hidden = hidden entries hidden
# H3b: Enter on a help row that documents an OVERLAY verb (`dialog.*`). Those
# are not dispatchable from a pane, so nothing runs — said out loud, because a
# swallowed Enter reads as a command that ran.
msg-help-not-runnable = that row documents an overlay key, not a command a pane can run
msg-help-modal-waiting = answer the dialog first: a command run from here would push it off the screen
# H3c (review MAJOR-2): F1 over a dialog no page documents yet. The index is
# NOT opened over it — a page covering a live question freezes its keys and
# explains something else — so the reader is told instead, and the dialog stays
# answerable.
msg-help-no-dialog-page = no help page explains this dialog yet: answer it, and press F1 for the index
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
# #135 (S4) — suspension. A pane that is not `file://` has no directory a
# local shell could sit in, so the shell is declined rather than opened
# somewhere else. `$path` arrives already sanitised (`path_display`): the line
# lands in the user's own terminal, after norte has released it.
msg-shell-remote = the active pane is { $path }; a shell there would not be where you are looking
msg-shell-failed = could not run { $program }: { $error }
# The pane IS local, but its native form is one only the `\\?\` verbatim
# namespace can express (a reserved device name, a trailing dot or space, or
# over 260 characters) — and `CreateProcessW` does not accept that namespace.
# Stripping it would silently open the child somewhere else.
msg-shell-cwd-unsupported = { $path } cannot be a program's working directory on this system
# Printed on the HOST terminal after a suspension that waits (`Ctrl+O`, and
# after a command line runs): the panels are gone and this is the only thing
# telling the reader norte is still there.
msg-shell-press-key = [norte] press any key to return
# The GUI's `app.terminal` (§E): nothing on this desktop answered, so say what
# was tried instead of doing nothing.
# `$configured` is the user's own $TERMINAL and `$tried` is norte's own
# closed list. They are SEPARATE arguments on purpose: joining an untrusted
# value into a `", "`-separated report lets one setting read as two entries
# (`TERMINAL='kitty, konsole'`), which is the arrow-join spoof wearing a
# comma. Nothing untrusted ever shares a joiner with anything.
msg-terminal-none = $TERMINAL is { $configured } and no terminal emulator was found; norte also tried its own list ({ $tried })
msg-terminal-none-unset = $TERMINAL is not set and no terminal emulator was found; norte tried { $tried }
help-cmd-app-terminal = open a shell in the active pane's directory
help-cmd-app-toggle-panels = hide the panels and show the terminal
help-cmd-pane-command-line = run a command in the active pane's directory
cli-ls-skipped = warning: { $n } container entries omitted from the index (hostile names/limits)

# --- Help (F1) — built from the effective keymap ---
help-title = Help
help-section-browse = Browsing (panes)
help-section-viewer = Viewer
help-section-dialog = Dialogs and overlays
help-dialog-note = each dialog supports its own subset of these keys
# The caveat of the KEYBOARD sheet (K3b), printed once under its title. Two
# claims, and the second is the one worth the line: a row without the mark is
# a command norte has built, but this command has no frontend, so it cannot
# say which of the two implements it — asking would need a running app.
keys-page-note = Keys norte has not built yet are listed too, marked with the issue that tracks them. A key without that mark is built; which frontend implements it is not knowable from here.
# --- Help overlay (H3b) — sidebar group headers and the synthetic keyboard
# page. `help-group-{tag}` is looked up from the corpus' OWN first tag (see
# the front matter of `norte-help/topics/*/*.md`): a new tag there needs its
# entry here, in both locales, or the sidebar paints the lookup key.
help-group-basics = Basics
help-group-doing = Doing things
help-group-remote = Remote & archives
# H3e: the group the plugin pages sit under, after everything the host wrote.
# Not a corpus tag — `norte_frontend::help` emits it for the plugin rows.
help-group-agents = Agents & policy
help-group-extensions = Extensions
# H3e: the provenance line under a plugin page's title. `help-plugin-origin` is
# UNCONDITIONAL on any plugin page and the others are appended when true — a
# plugin that declares no publisher and ships a clean file must not be able to
# make the line vanish and have its page read as a built-in one.
# `truncated`/`lossy` come from the HOST, not from re-parsing the text (it
# arrives already short and already decoded), so this is the only place a reader
# learns a page was cut.
help-plugin-origin = from an extension
help-plugin-by = published by { $who }
help-plugin-truncated = cut short
help-plugin-lossy = some bytes did not decode
# H3g: `norte help` writes to a stream that may not be a terminal, so its
# callouts are ASCII WORDS where the TUI paints `ℹ`/`⚠`/`💡` and the see-also
# row is spelled out instead of implied by a style.
help-see-also = See also
help-callout-note = note
help-callout-warn = warning
help-callout-tip = tip
cli-help-unknown-topic = no help page is called { $id } — `norte help --list` names them all
cli-help-no-matches = nothing matches { $query }
# The synthetic `keys` entry gets NO header: it is a group of one by
# construction and its header would be the same word as its only row, so the
# sidebar does not paint it (`ui::draw_help`). Hence no `help-group-keys`.
help-topic-keys = Keyboard
# H3f: footer of the GUI's help overlay. A FIXED string, unlike the TUI's
# footer, which is generated from the `dialog` keymap — the GUI has no such
# context and routes these keys by hardcoded GPUI names (`help_view::on_key`),
# so a generated hint would have nothing to generate from. Rebinding does not
# change these keys, and this string must change if `on_key` does.
help-hint-gui = ⇥ pane · ⏎ run · / filter · ⌫ back · Ctrl+P palette · Esc close
help-cmd-app-quit = quit norte
help-cmd-app-help = this help
help-cmd-app-theme = choose theme
help-cmd-app-extensions = extension manager
help-cmd-app-agents = agent sessions
agents-note = only the agent sessions THIS window has seen ask for permission; it is not the system’s roster of agents
agents-empty = no agent has asked for permission since this window opened
agents-not-listening = this window is mounted without effects: it does not receive permission requests, so this list is empty for that reason and not because nobody asked
agents-forgotten = { $n } sessions were forgotten to the cap: this list is not complete
host-undo-already-running = that session already has an undo running
agents-title = Agent sessions
agents-counts = asked { $seen }, approved from here { $approved }
host-no-session = no session is selected
modal-undo-session-title = undo everything this session did?
modal-undo-session-scope = reverts ALL of its operations, newest first; whatever cannot be reverted is named in the report
help-cmd-app-palette = command palette
help-cmd-app-settings = settings
help-cmd-app-pick-accept = accept the selection and exit (picker mode)
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
# Task 4.5: the read-only settings view of the graphical window. The paths
# section is DIAGNOSTIC, not configuration: it answers "where does what I am
# looking at come from". Locations only — never a value, so nothing secret.
settings-section-paths = Where things live
settings-path-config-system = System config
settings-path-config-user = Your config
settings-path-config-profile = Active profile
settings-path-config-project = Project config
settings-path-state = State (session, history)
settings-path-logs = Logs
settings-path-socket = Daemon socket
settings-path-missing = not there
settings-read-only = This window shows settings but does not write them yet.
settings-restart-badge = restart required
settings-hint-gui = [↑/↓/pgup/pgdn/click] navigate · [enter/click] edit · [ctrl+k] shortcuts · [esc] close
# P1: prefix on a plugin-contributed row (`palette::plugin_rows`) — no
# built-in row ever carries it, so a plugin cannot spoof a built-in command
# by copying its exact display text.
palette-plugin-prefix = extension
theme-picker-title = Theme
columns-picker-title = Columns — { $target }
columns-picker-target-default = all schemes
columns-picker-hint-toggle = toggles
columns-picker-hint-move = moves
columns-picker-hint-sort = sorts
columns-picker-hint-format = format
columns-picker-hint-apply = applies
columns-picker-hint-close = closes
msg-columns-saved = columns saved
ext-title = Extensions
ext-empty = no extensions installed
ext-unapproved = not approved
# Task 4.5: the graphical window's read-only extension manager reuses the
# three above and adds these. The state is TWO independent facts —
# approved, and switched on — because an extension approved and then
# switched off is not the same as one nobody has looked at yet, and
# "loading" is not "none" either. `ext-config-*` say what bounds a
# plugin's `[config]` key; an `enum` lists its values instead.
ext-loading = asking the daemon what is installed…
ext-state-on = approved · on
ext-state-off = approved · off
ext-config-title = Its settings
ext-commands-title = Commands
plugin-output-title = Extension output
plugin-output-empty = (it printed nothing)
plugin-output-truncated = the output was cut: it was longer than fits
ext-config-none = This extension declares no settings.
ext-config-range = between {$min} and {$max}
ext-config-min = at least {$min}
ext-config-max = at most {$max}
# Task 4.5: the two list pickers and the theme view of the graphical
# window. The connections picker is NOT here: reading connections.toml
# means pulling the connection crate — russh, opendal, suppaftp, age, the
# keyring — into this window for a list it cannot act on yet. A theme's
# `effects` are interpreted per renderer: the ones this one cannot paint
# are NAMED, because a retro theme that looks identical reads as broken.
picker-volumes-title = Volumes
picker-volumes-title-left = Volumes (left pane)
picker-volumes-title-right = Volumes (right pane)
picker-volumes-loading = asking the host for its mount table…
picker-connections-title = Connections
picker-connections-loading = asking the daemon which connections exist…
picker-connections-empty = no connections configured (connections.toml)
picker-volumes-empty = the host reported no volumes
picker-history-title = History
picker-history-empty = this panel has not been anywhere else yet
picker-hotlist-title = Favorites
picker-hotlist-empty = no favorites configured
picker-volume-space = {$free} free of {$total}
picker-volume-read-only = read-only
theme-title = Theme
theme-roles = What each role is painted with
theme-effects-unsupported = This theme declares effects that this window does not paint:
history-title = History
history-empty = no history yet
hotlist-title = Favorites
hotlist-empty = empty — add the current directory from the popup
hotlist-name-prompt = name:
hotlist-invalid = invalid path
# 2026-08-10-volumes.md §D: the drive picker (`pane.select-drive*`), a third
# `NavPopupKind` beside history and hotlist.
volumes-title = Drives
volumes-empty = no volumes found
# The in-popup unfiltered toggle's two modes (design §E) — the footer says
# which one is showing, so the toggle is never silent about what it did.
volumes-mode-filtered = system filesystems hidden
volumes-mode-all = showing everything
# A size the filesystem did not answer in time (design §A): never a bare `0`,
# which would read as "full" — the opposite of "unknown".
volumes-size-unknown = unknown
# L3: the places sidebar (`layout.places`) — a panel, not a popup, so its
# labels are LABELS: 14 cells of room and no sentence fits in them.
places-title = Places
places-section-drives = Drives
places-section-favorites = Favorites
places-empty = nothing here yet

# Fase A: el panel de procesos y la hoja de atributos. Dos paneles nuevos que
# se abren a mano; la franja de tareas de siempre no se toca.
processes-title = Processes
processes-empty = nothing running
processes-has-keyboard = this panel has the keyboard · Esc returns it
task-failed = failed
metadata-title = Details
metadata-empty = nothing under the cursor
metadata-name = Name
metadata-kind = Kind
metadata-size = Size
metadata-mtime = Modified
metadata-kind-dir = folder
metadata-kind-file = file
metadata-kind-symlink = link
metadata-kind-other = other
# L3: what the docked viewer says INSTEAD of a file. A directory is never
# read: a preview follows the cursor, so reading whatever the cursor lands
# on is how one turns into opening a block device by accident.
preview-title = Preview
preview-directory = directory
preview-empty = nothing selected
preview-not-a-file = not a regular file
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
msg-plugin-not-approved = this extension is not approved: approve it before enabling it
modal-plugin-approval-title = Grant these capabilities?
modal-plugin-approval-note = an approved extension acts on your behalf with everything listed here
msg-extensions-no-help = this extension ships no help page
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
help-cmd-app-menu = menu bar
menu-item-pane-view = View
menu-item-pane-properties = Properties
menu-item-pane-dir-size = Space used
menu-item-pane-open = Open with…
menu-item-pane-copy = Copy
menu-item-pane-move = Move
menu-item-pane-rename = Rename
menu-item-pane-mkdir = New directory
menu-item-pane-delete = Delete
menu-item-pane-delete-permanent = Delete permanently
menu-item-app-quit = Quit
menu-item-mark-toggle = Toggle mark
menu-item-mark-all = Mark all
menu-item-mark-invert = Invert marks
menu-item-mark-clear = Clear marks
menu-item-mark-pattern-add = Mark by pattern
menu-item-mark-pattern-remove = Unmark by pattern
menu-item-pane-switch = Switch panel
menu-item-pane-mirror = Mirror to the other
menu-item-pane-pull = Pull from the other
menu-item-pane-swap = Swap panels
menu-item-layout-split-h = Split side by side
menu-item-layout-split-v = Split top and bottom
menu-item-layout-close-slot = Close panel
menu-item-layout-grow = Grow
menu-item-layout-shrink = Shrink
menu-item-layout-equalize = Equalise
menu-item-layout-set-target = Set destination
menu-item-layout-places = Places sidebar
menu-item-layout-preview = Docked viewer
menu-item-layout-processes = Processes panel
menu-item-layout-metadata = Details panel
menu-item-layout-pick = Layout...
menu-item-profile-pick = Profile...
menu-item-pane-tab-new = New tab
menu-item-pane-tab-close = Close tab
menu-item-pane-tab-next = Next tab
menu-item-pane-tab-prev = Previous tab
menu-item-pane-tab-move-left = Move left
menu-item-pane-tab-move-right = Move right
menu-item-pane-quick-search = Quick filter
menu-item-pane-search = Search the subtree
menu-item-pane-semantic-search = Search by meaning
menu-item-pane-compare-dirs = Compare directories
menu-item-pane-sync-dirs = Synchronise
menu-item-pane-toggle-hidden = Hidden entries
menu-item-pane-columns = Columns
menu-item-pane-sort-menu = Sort by…
menu-item-pane-names-encoding = Name encoding
menu-item-app-theme = Theme
menu-item-app-settings = Settings
menu-item-app-extensions = Extensions
menu-item-app-help = Help
menu-item-app-palette = Command palette
menu-file = File
menu-mark = Mark
menu-panels = Panels
menu-tabs = Tabs
menu-find = Find
menu-view = View
menu-help = Help
help-cmd-pane-switch = switch pane
help-cmd-pane-tab-new = new tab
help-cmd-pane-tab-close = close tab
help-cmd-pane-tab-next = next tab
help-cmd-pane-tab-prev = previous tab
help-cmd-pane-tab-move-left = move tab left
help-cmd-pane-tab-move-right = move tab right
help-cmd-pane-tab-goto-1 = go to tab 1
help-cmd-pane-tab-goto-2 = go to tab 2
help-cmd-pane-tab-goto-3 = go to tab 3
help-cmd-pane-tab-goto-4 = go to tab 4
help-cmd-pane-tab-goto-5 = go to tab 5
help-cmd-pane-tab-goto-6 = go to tab 6
help-cmd-pane-tab-goto-7 = go to tab 7
help-cmd-pane-tab-goto-8 = go to tab 8
help-cmd-pane-tab-goto-9 = go to tab 9
help-cmd-layout-split-h = split side by side
help-cmd-layout-split-v = split top and bottom
help-cmd-layout-focus-next = next panel
help-cmd-layout-focus-prev = previous panel
help-cmd-layout-close-slot = close panel
help-cmd-layout-grow = grow panel
help-cmd-layout-shrink = shrink panel
help-cmd-layout-equalize = equalise panels
help-cmd-layout-set-target = set destination
help-cmd-layout-places = show or hide the places sidebar
help-cmd-layout-preview = show or hide the docked viewer
help-cmd-layout-processes = show or hide the processes panel
help-cmd-layout-metadata = show or hide the details panel
help-cmd-layout-pick = choose a layout
help-cmd-profile-pick = choose a profile
help-cmd-profile-next = next profile
help-cmd-profile-prev = previous profile
help-cmd-profile-save-as = save this workspace as a profile
cmd-planned-profile-save-as = writing a profile from what is on screen comes with the rest of config editing
msg-layout-last-panel = cannot close the last panel
msg-transfer-dest-invalid = that is not an address: {$err}
msg-transfer-dest-same = that is where they already are: type another destination
msg-layout-load-failed = could not load layout "{$name}": {$err}
msg-session-detached = another window owns the session; this one runs on its own
msg-session-slots-timeout = { $n } panels did not list in time at startup: enter them again to fill them
pane-unlisted = not listed
modal-pack = Pack into
modal-pack-title = Pack
modal-pack-hint = Enter packs · Esc cancels
modal-pack-hint-zip = zip · Enter packs · Esc cancels
modal-pack-hint-tar = tar · Enter packs · Esc cancels
modal-pack-hint-targz = tar.gz · Enter packs · Esc cancels
modal-pack-hint-unknown = unknown extension — use .zip, .tar, .tar.gz or .tgz
modal-split = Split into pieces of
modal-split-title = Split
modal-split-hint = 4096, 10M, 700M · pieces land in the other panel · Enter splits · Esc cancels
msg-pack-read-only = that panel is read-only: nothing can be written there
msg-pack-nothing = nothing marked and nothing under the cursor
msg-pack-unknown-format = norte writes .zip, .tar and .tar.gz; it reads .rar but cannot write it
msg-pack-bad-name = that name cannot be a filename
msg-split-needs-file = split works on a file, not on a folder
msg-split-bad-size = a size like 4096, 10M or 700M
msg-unpack-not-archive = that is not an archive norte knows how to open
msg-unpack-started = unpacking into the other panel
msg-test-archive-started = checking the archive
msg-combine-needs-first = start from the first piece (.001)
msg-combine-started = joining the pieces
msg-session-owned = this window now keeps the session
msg-session-too-large = the session is too big to store; history dropped
msg-session-unreadable = the stored session could not be read; starting from the configured layout
msg-layout-applied = layout applied: { $name }
layout-picker-title = Layout
# This window does not write configuration yet: what is chosen applies to
# THIS window and is lost when it closes. Saying nothing would leave the
# user believing they had just configured norte.
columns-picker-session-only = applies to this window; not saved
# Task 6.1 in the graphical window: search over a subtree. "Running" and
# "nothing matched" are said apart, because a short list that stopped
# growing and one still growing read the same.
modal-search-title = Search in this tree
err-empty-pattern = type a pattern: an empty one matches the whole tree
modal-mark-pattern-title = mark by pattern (glob)
modal-unmark-pattern-title = unmark by pattern (glob)
err-bad-pattern = that pattern is not a valid glob
# Task 4.1 in the graphical window: what each row of the layout picker says
# about itself. The keymap-name warning is not decoration — the two settings
# share a namespace, and without the line the coincidence is a trap.
layout-picker-factory = factory
layout-picker-shares-keymap = also a keyboard preset — no keys change
layout-picker-mine = yours
layout-picker-keymap-note = the layout does not change your keys ([keymap] preset does)
profile-picker-title = Profile
profile-picker-empty = you have no profiles yet — a profile is a directory in profiles/
profile-picker-broken = will not load
profile-picker-no-state = this name cannot be saved: it will not remember your panels
profile-picker-clash-layout = a layout is also called this; picking the profile is not picking it
profile-picker-clash-keymap = a keymap preset is also called this; picking the profile binds no keys
profile-picker-clash-both = a layout and a keymap preset are also called this; picking the profile is neither
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
# 2026-08-10-volumes.md (closes #131): the focused pane, and the two SIDES
# Total Commander's Alt+F1/Alt+F2 name — not the focus, see the design doc §D.
help-cmd-pane-select-drive = pick a drive for the focused pane
help-cmd-pane-select-drive-left = pick a drive for the LEFT pane
help-cmd-pane-select-drive-right = pick a drive for the RIGHT pane
help-cmd-task-cancel = cancel the selected task, or the most recent one
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
help-cmd-pane-sort-name = sort by name
help-cmd-pane-sort-ext = sort by extension
help-cmd-pane-sort-size = sort by size
help-cmd-pane-sort-time = sort by date
help-cmd-pane-sort-menu = choose the sort order (opens the columns dialog)
help-cmd-pane-properties = properties of the entry
help-cmd-pane-dir-size = count how much space it takes
help-cmd-pane-pack = pack into an archive
help-cmd-pane-unpack = unpack into the other panel
help-cmd-pane-test-archive = check an archive
help-cmd-pane-split-file = split a file into pieces
help-cmd-pane-combine-files = join the pieces back
help-cmd-pane-edit = edit with your editor
help-cmd-pane-edit-new = edit a new file
help-cmd-pane-connect = open a saved connection
help-cmd-pane-tree = directory tree
menu-item-pane-tree = Tree
tree-title = Tree
tree-loading = reading…
help-cmd-pane-disconnect = close this panel's connection
menu-item-pane-connect = Connect…
menu-item-pane-disconnect = Disconnect
connections-picker-title = Connections
connections-picker-empty = no connections in connections.toml
msg-connect-bad-url = that connection has an address norte cannot read: { $url }
msg-disconnect-local = this panel is local: there is no connection to close
msg-disconnect-done = connection closed
msg-disconnect-none = there was no open connection
menu-item-pane-edit = Edit
menu-item-pane-edit-new = Edit new
msg-edit-nothing = there is nothing under the cursor to edit
msg-edit-not-a-file = that is a folder: press ⏎ to enter it, it is not edited
props-kind = kind
props-kind-dir = folder
props-kind-file = file
props-kind-symlink = link
props-kind-other = other
props-size = size
props-size-unknown = the backend does not know
props-mtime-unknown = the backend does not know
props-modified = modified
props-path = path
props-entries = { $count } entries
props-counting = counting…
props-count-hint = not counted (close this and use "count how much space it takes")
props-hint = [Esc] close
msg-dir-size-counting = counting how much space it takes…
msg-dir-size = { $size } in { $count } entries
msg-dir-size-partial = at least { $size } in { $count } entries ({ $skipped } unreadable)
help-cmd-pane-mkdir = create a directory (F7)
help-cmd-pane-ai-rename = AI rename of the current directory (reviewable plan)
help-cmd-pane-semantic-search = Semantic search over the index (AI)
help-cmd-pane-rename = rename in place (Shift+F6)
help-cmd-pane-refresh = reload both panes (Ctrl+R)
help-cmd-viewer-encoding-auto = back to auto-detection
help-cmd-viewer-hex = toggle hex view
help-cmd-pane-search = search by name/content (Alt+F7)
help-cmd-pane-compare-dirs = compare the two panes and open the diff pane
help-cmd-pane-sync-dirs = plan a one-way synchronisation from this pane to the other
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

# --- Directory comparison (2026-08-11-directory-comparison.md, `Shift+F2`) ---
# The diff pane. Every row says WHAT was decided, WHICH criterion decided it
# and HOW MUCH that criterion is worth, and each of the three is a word here
# rather than a colour: spec §17 wants a textual cue, and `same/probable`
# versus `same/certain` is precisely the pair a colour-blind reader must not
# lose.
compare-title = Compare
sync-title = synchronise
sync-mode-update = update
sync-mode-mirror = mirror
gui-sync-mode-update = UPDATE: nothing is deleted at the destination
gui-sync-mode-mirror = MIRROR: what is not in the source is DELETED at the destination
host-sync-read-only = this window can only READ the plan for now · ↑↓ move · Esc close
host-sync-already = there is already a plan open or on its way
sync-hint-applying = Esc asks to cancel · Esc again closes and gives up the report
msg-sync-cancelled-late = the sync had already started: it was asked to stop
msg-sync-closed-midway = closed while writing: the destination may be half done and its report is gone
msg-sync-apply-unknown = unknown whether it started: the connection failed AFTER the request, so the destination may be being written. Check the task board before applying again
compare-header-left = left
compare-header-right = right
compare-empty = no rows yet
# Rows DID arrive, every category is switched off. Saying "no rows yet" there
# is a lie the filter line's own counts contradict — and the reader's next
# move (wait, or press 1-5) depends on which of the two it is.
compare-all-filtered = every category is hidden — 1-5 brings them back
compare-hint = tab side · 1-5 filter · ins mark · s sync · m mirror · enter go · esc close
# The GUI's OWN key line, and the only compare id that is not shared. It is
# shorter because the GUI's diff pane is smaller: marking rows exists to seed a
# synchronisation plan, and that surface (#161) is not built here yet. A key
# line that promised `s sync` in a frontend that cannot sync would be a
# documented dead shortcut — this repository has shipped one already.
gui-compare-hint = tab side · 1-5 filter · ins mark · s sync · m mirror · enter go · esc close
gui-compare-marked = marked
compare-active-side = acting on: { $side }
compare-status-running = compare: { $n } rows (comparing…)
compare-status-done = compare: { $n } rows
compare-status-unknown = compare: { $n } rows arrived, and nothing said whether that was all of them
# The row stream ended with FEWER rows than the task counted: a notification
# batch was dropped between the daemon and here. Saying "done" would be a lie
# about completeness, which on a comparison is the whole answer.
compare-status-incomplete = compare: { $n } of { $total } rows (some were lost in transit)
compare-status-cancelled = compare: { $n } rows (cancelled)
compare-status-failed = compare failed: { $error }
# Refused locally, before a round trip: the daemon answers `-32602` for it too,
# but the reader deserves the sentence without the wait.
compare-same-path = both panes are on the same directory — there is nothing to compare
compare-no-target = that row has nothing on the { $side } side
compare-side-left = left
compare-side-right = right
compare-side-unknown = an unknown side
compare-verdict-same = same
compare-paired-under = the two names are spelled differently and match anyway
compare-paired-under-singleton = WARNING: these two names may be different files that Unicode calls equal
compare-paired-under-full-fold = WARNING: these two names may be different files that this filesystem folds into one
compare-verdict-different = different
compare-verdict-only-left = only on the left
compare-verdict-only-right = only on the right
compare-verdict-type-mismatch = different kind
compare-verdict-ambiguous = ambiguous
compare-verdict-error = error
compare-verdict-unknown = verdict this version does not know
compare-confidence-certain = certain
compare-confidence-probable = probable
# NOT a failure: an archive with no trustworthy date, an object store whose
# ETag is a hash only sometimes. An honest provider, and the reason the
# confidence vocabulary exists.
compare-confidence-unknown = cannot be sure
compare-confidence-unrecognised = confidence this version does not know
compare-criterion-presence = presence
compare-criterion-kind = kind
compare-criterion-link-target = link target
compare-criterion-size = size
compare-criterion-mtime = date
compare-criterion-hash = content hash
compare-criterion-unknown = criterion this version does not know
compare-reason-case-fold = two names on one side differ only in case
compare-reason-normalization = two names on one side differ only in Unicode normalization
compare-reason-unreadable = the directory could not be listed, or an entry could not be inspected
compare-reason-dir-too-large = the directory is over the entry limit
compare-reason-read-failed = a read failed while hashing
compare-reason-unknown = reason this version does not know
compare-filter-same = same
compare-filter-different = different
compare-filter-only-left = only left
compare-filter-only-right = only right
compare-filter-problems = problems

# --- Directory synchronisation (2026-08-11-directory-sync.md) ---
# The approval dialog's vocabulary. The rule every string here obeys: a step's
# `reversal` column says how it WOULD come back, and only the destination's
# trash says whether it will. A copy onto a destination with no trash is sent
# as `delete` and the undo skips it — so nothing here promises a step comes
# back without saying which destination that is true of.
sync-step-create-dir = create directory
sync-step-copy = copy
sync-step-overwrite = overwrite
sync-step-delete-tree = delete
sync-step-skip = leave alone
sync-step-unknown = step this version does not know
# Neutral on PURPOSE: on an overwrite the undo brings the old file back, on a
# copy it removes the new one. "Puts it back" reads as the first in both.
sync-undo-reverts = the undo reverses it
sync-undo-left-behind = undo leaves it there
sync-undo-irreversible = cannot be undone
sync-undo-nothing = nothing to undo
sync-undo-unclear = this version cannot say
sync-reason-ambiguous-source = two names on the source collapse into one — neither is copied
sync-reason-unknown-confidence = the provider could not tell the two sides apart
sync-reason-unreadable = it could not be read, so the plan leaves it alone
# Covers BOTH "there is no trash" and "there is one that cannot say where it
# put things" (macOS, Windows): the wire token does not separate them, so this
# sentence must not claim either.
sync-reason-no-trash-on-target = the destination cannot give this back: it has no trash, or one that does not say where it puts things
sync-reason-non-injective-pairing = the two names may be different files that Unicode calls equal — the plan does not touch this pair
sync-reason-unknown = reason this version does not know
sync-blocker-ambiguous-dest = two names on the destination collapse into one — writing there could hit the wrong file
sync-blocker-overlap-detected = the two roots are the same tree
sync-blocker-dest-read-only = the destination does not take writes
sync-blocker-dir-too-large = a directory on the destination is over the entry limit
sync-blocker-type-mismatch-dir = a directory on one side is a file on the other
sync-blocker-unknown = blocker this version does not know
# The headline: what the undo would give back if this plan ran. "Can be
# undone" is a statement about the PLAN, never a guarantee per entry — an
# entry the trash cannot name, or a path that changed in the meantime, is
# named in the undo report instead of being touched.
space-warning = { $size } to write and { $free } free at the destination
# #164: this destination cannot open a confined root, so a write reaches its
# place by resolving a path. Someone who can drop a symlink inside the
# destination between this prompt and the write can send it elsewhere. Said,
# never refused: refusing would strand every destination that cannot offer the
# defence, which costs far more than the race it avoids.
confine-warning = this destination cannot confine writes: a symlink placed inside it could send this somewhere else
sync-outlook-full = you can undo all of this afterwards, except anything that changes in the meantime
sync-outlook-partial = some of this can be undone afterwards, and some cannot
sync-outlook-nothing = nothing here can be undone by norte
sync-outlook-unclear = this version cannot tell whether any of this can be undone
# Which destination this is. Same outlook, very different news: in the first
# the file is sitting in the system trash, in the second it is gone.
sync-trash-restorable = the destination's trash names what it buries, so the undo can find it again
sync-trash-opaque = the destination's trash does not say where it puts things: what this replaces is recoverable by hand from the system trash, but not by norte
sync-trash-absent = the destination has no trash: what this replaces or deletes is not kept anywhere
sync-trash-unknown = this version does not know what kind of trash the destination has
sync-summary-irreversible = { $n } steps are irreversible: nothing brings them back
sync-summary-actions = { $copy } to copy · { $overwrite } to overwrite · { $createdir } directories to create · { $deletetree } to delete · { $skip } left alone
sync-summary-bytes = { $bytes } to write
# `bytes` is a LOWER BOUND, never a total: a listing on file:// gives no sizes
# at all, so a confident number is the normal way to lie here.
sync-summary-bytes-partial = { $bytes } to write, plus { $n } files whose size the provider did not give
# A directory that would not list is ONE entry here, and everything inside it
# is unseen — hence "and whatever is inside them".
sync-summary-unreadable = { $n } entries could not be read: neither they nor anything inside them is covered by this plan
# The steps that arrived do not add up to what the plan closed with.
sync-summary-mismatch = { $received } steps arrived and the plan says { $n }: this plan cannot be approved
sync-summary-unnameable = { $n } steps are of a kind this version cannot show: this plan cannot be approved
sync-summary-malformed = { $n } steps contradict themselves: this plan cannot be approved
sync-summary-duplicate-ids = { $n } steps carry an id that does not advance past an earlier one: this plan cannot be approved
sync-summary-list-truncated = the list shows the first { $shown } steps; { $hidden } more are counted but not listed
# The classes add up and a number the dialog leads with does not.
sync-summary-contradictory = the plan's own totals do not match the steps it sent: this plan cannot be approved
# Blockers are never filtered by the selection, so this is about the whole
# comparison and not about what is on screen.
sync-summary-blocked = { $n } blockers stop this plan — anywhere in the two trees, not only in what you selected
# One question per outlook, saying exactly what the summary said: a headline
# that reads "some of this can be undone" over a confirmation that reads "none
# of it can" teaches the reader to skip both.
sync-confirm-delete = { $n } trees will be deleted from the destination. They go to the trash and can be restored. A tree is re-checked at the directory itself, not inside it: something added deeper since you approved will not stop the deletion. Continue?
sync-confirm-delete-final = { $n } trees will be deleted from the destination and CANNOT be restored afterwards. A tree is re-checked at the directory itself, not inside it: something added deeper since you approved will not stop the deletion. Continue?
sync-confirm-delete-partial = { $n } trees will be deleted from the destination, and { $steps } steps of this plan cannot be undone afterwards. A tree is re-checked at the directory itself, not inside it: something added deeper since you approved will not stop the deletion. Continue?
sync-confirm-delete-unclear = { $n } trees will be deleted from the destination, and this version cannot tell whether they could be restored. A tree is re-checked at the directory itself, not inside it: something added deeper since you approved will not stop the deletion. Continue?
sync-confirm-no-way-back = { $n } steps will change the destination and none of them can be undone afterwards. Continue?
sync-confirm-partial = { $n } steps of this plan cannot be undone afterwards. Continue?
sync-confirm-unclear = this version cannot tell whether these { $n } changes can be undone afterwards. Continue?
# Heading of the list of failures, and the SAME string names that list to a
# screen reader: one sentence, both surfaces. Its own name and never the step
# list's — the two sit one above the other, and a reader who lands in the
# wrong one reads "applied" where it says "failed".
sync-failures-title = steps that failed
# `sync.report` lists at most 256 failures and counts them all, so a run with
# more says so rather than letting the list read as the total.
sync-failures-more = … and { $n } more
# Why ONE step of an applied plan did not happen (`sync.report`). Shared by
# every frontend: the CLI prints them after the run, the GUI lists them under
# the plan.
sync-cause-conflict = the destination changed since the plan was made
sync-cause-denied = permission denied
sync-cause-illegal-name = the name is not legal on the destination
sync-cause-io = read or write failed
sync-cause-unknown = unrecognised failure
# The sync PANE (Ctrl+Y, or `s`/`m` inside the diff pane). Its keys are fixed,
# like the diff pane's, so the hint line is the only place they are written.
# A mode this build cannot name. It must NOT fall back to "update": saying "this
# does not delete" about a mode we cannot name asserts the SAFE half of what a
# human is approving.
sync-mode-unknown = mode this version does not know
sync-header-step = step
sync-header-path = path
sync-header-size = size
sync-planning = planning… { $n } steps so far
sync-empty = this plan has no steps: the two trees already agree
sync-status-cancelled = cancelled — { $n } steps had arrived, and there is no plan to approve
sync-status-failed = the plan failed: { $error }
sync-status-ready = { $n } steps · press a to approve
sync-status-not-approvable = { $n } steps · this plan cannot be approved
sync-status-applying = applying…
sync-status-applied = { $done } steps applied, { $failed } failed
sync-status-applied-undoable = { $done } steps applied, { $failed } failed · undo it with the undo command
sync-status-applied-not-undoable = { $done } steps applied, { $failed } failed · nothing was journalled, so there is nothing to undo
# The same report, when the run did NOT finish on its own. The counts alone
# would read as a completed sync, and the word that says otherwise must not be
# left to colour.
sync-orphan-report = the synchronisation finished with no panel open: { $done } steps applied, { $failed } failed · undo it with the undo command
sync-status-applied-cut-undoable = cancelled after applying { $done } steps ({ $failed } failed); the rest was not applied · undo it with the undo command
sync-status-applied-cut-not-undoable = cancelled after applying { $done } steps ({ $failed } failed); the rest was not applied · nothing was journalled, so there is nothing to undo
# And when it died: the error AND the counts, because a mirror that deleted
# forty trees and then failed is the last place to hide how much it wrote.
sync-status-applied-failed-undoable = it failed after applying { $done } steps ({ $failed } failed): { $error } · undo it with the undo command
sync-status-applied-failed-not-undoable = it failed after applying { $done } steps ({ $failed } failed): { $error } · nothing was journalled, so there is nothing to undo
sync-hint = ↑↓ move · a approve · Esc close
sync-hint-done = ↑↓ move · Esc close
sync-hint-confirm = y confirm · any other key cancels
# The destination spells this entry differently from the source (#152): the
# write lands on the destination's spelling, and both are shown so nobody
# reads a normalisation difference as a second file.
sync-dest-spelling = destination: { $path }
# A step's path hangs off the destination root, not the source one.
sync-anchor-dest = (destination)
sync-anchor-either = (which side is not recorded)
# A whole-tree blocker (a read-only destination) names the ROOT, which
# `rel_display` alone paints as an empty string — this is what a pane says
# there instead, so it does not read as an empty row (#193).
sync-rel-root = the whole tree
# An NFC/NFD pair (or any other byte-different, glyph-identical spelling) is
# valid UTF-8 on both sides, so neither half is masked as hostile — this is
# what explains the arrow instead (#192).
sync-dest-twin = (same on screen, different bytes)
compare-marked = { $n } marked
msg-sync-needs-daemon = synchronising needs the daemon (--daemon): it has to be journalled
msg-journal-squatted = your journal has been held by another process for minutes and no daemon is listening: nothing is being recorded
status-journal-squatted = ⛔ someone is holding your journal — nothing recorded
status-no-journal = NOT journalled: this session cannot be undone
status-journal-refused = journal UNREADABLE: this session refuses to change your files
status-session-detached = detached window: your screen is not being saved
msg-journal-busy = this session is NOT recorded in the journal: another norte process holds it (a daemon, or another window)
msg-journal-refused = this session REFUSES to change anything: its journal could not be opened ({ $motivo }). Nothing would be recorded and nothing could be undone. Repair what the reason names — the directory or the file — and try again
msg-journal-recovered = journalling resumes from your NEXT operation; one already running keeps the answer it started with
msg-sync-too-many-marks = { $n } marks is over the { $max } this request takes: mark fewer, or mark a directory that holds them
msg-sync-cannot-approve = this plan cannot be approved as it stands
msg-sync-mark-outside-roots = a marked row is in neither of the two directories: re-run the comparison
msg-sync-mark-is-the-root = a marked row is one of the two directories itself, which would mean the whole tree: mark what is inside it
err-overlapping-roots = the two directories overlap
err-overlapping-roots-same = source and destination are the same directory
err-overlapping-roots-source-inside = the source is inside the destination
err-overlapping-roots-dest-inside = the destination is inside the source
reason-needs-daemon = needs the daemon (--daemon)

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
# Name of the diff pane's row list (#158). It goes on a WRAPPER around the
# virtualised list, never on the list itself: its id feeds scrolling and
# measurement.
gui-a11y-compare-rows = comparison rows
# Name of the sync pane's step list (#161). Same wrapper rule as the diff
# pane's, and a name of its own: the pane's frame is already announced as
# "synchronise", and giving the list the same words says nothing about which
# of the two the reader is inside.
gui-a11y-sync-steps = plan steps
# Name of the sync pane's second question, the one that is answered before
# anything is written. It names the question so a reader who lands on it knows
# what the `y` answers.
gui-a11y-sync-confirm = confirm the plan
# Spoken prefix for a name the sanitiser had to alter (spec §6). A WORD and
# not the `⚠` badge: at the default symbol verbosity of NVDA and Orca a lone
# U+26A0 is not spoken at all, and under an active name reinterpretation (#57)
# the masked form carries no U+FFFD either — it is clean, legible text that
# differs from the bytes on disk, so the badge is its only marker.
gui-a11y-hostile-name = altered name
# The visible badge next to a name that does not paint as it really is
# (bidi overrides, controls, undecodable bytes). It is TEXT and not a bare
# symbol on purpose: the symbol alone is a mystery the first time, and this
# marker is the only warning the reader gets before acting on that name.
hostile-name = ⚠ altered name
# What the graphical window paints where a list has nothing in it. They are
# separate messages because they answer different questions: a directory
# with no entries, and a filter that matched none.
listing-empty = empty
# El provider no pudo con todas: sin permiso para statearlas, o por
# encima de un tope suyo. Se DICE, porque lo que falta no está y no hay
# ninguna fila donde el lector pueda tropezarse con ello.
listing-skipped = { $n } entries were skipped
palette-empty = nothing matches what you typed
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
gui-menu-copied = { $n } path(s) copied to the clipboard

# --- Why a command cannot run (H3d) — SHARED by both frontends: the GUI
# dims a context-menu entry and the TUI dims a help row with the same
# wording, from `norte_frontend::availability::reason_key`. No `gui-` prefix
# on purpose. `reason-unavailable` is the fallback for a `Reason` variant
# added after this catalogue (the enum is `#[non_exhaustive]`).
reason-read-only = read-only backend
reason-wrong-target = does not apply to this selection
reason-answered-by-overlay = the open overlay answers this key itself
# H3f/4.4: a row of the VIEWER screen, read from a help page with no viewer
# open. It is not "this frontend does not do it" — it does — and it is not
# about the selection either: the key belongs to another screen.
reason-viewer-only = only while the viewer is open
reason-unsupported = the backend does not support it
reason-plugin-inactive = the plugin is disabled or unapproved
reason-policy-denied = the policy denies it
reason-connection-degraded = degraded connection
reason-unavailable = unavailable right now
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
gui-task-kind-create = create file
gui-task-kind-undo = undo
gui-task-kind-search = search
gui-task-kind-index = index
gui-task-kind-embed = embed
gui-task-kind-rename-batch = rename
gui-task-kind-unknown = task
gui-task-kind-compare = compare
gui-task-kind-dir-size = size
gui-task-kind-pack = pack
gui-task-kind-test-archive = test
gui-task-kind-split = split
gui-task-kind-combine = combine
gui-task-kind-sync-plan = plan
gui-task-kind-sync = sync
# La lanzó OTRO cliente de la misma sesión. Se pinta igual y se cancela igual
# —es la misma sesión—, pero el tablero lo dice: una operación que uno no ha
# pedido y no se distingue de las suyas es una sorpresa.
gui-task-foreign = not yours
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
cli-ai-rename-unjournalled = these renames will NOT be recorded in the journal, so they cannot be undone
cli-ai-rename-confirm = Apply these renames? [y/N]
cli-ai-rename-abort = aborted; nothing was renamed
cli-ai-rename-done = renamed { $n } file(s)
gui-modal-quit-title = Quit with { $tasks } task(s) running and { $marks } mark(s)?
gui-modal-quit-title-empty = Quit norte?
gui-modal-footer-quit = y confirm   n/Esc cancel
gui-modal-footer-volumes = y/Enter open   n/Esc cancel   ↓↑ move   Tab toggle all
gui-banner-theme-io = theme { $spec }: { $error }
gui-banner-theme-parse = theme { $spec }: { $detail }
gui-banner-config-io = configuration { $path }: { $error }
gui-banner-config-parse = configuration { $path }: { $detail }
gui-banner-effects-key-skipped = theme effects: skipped { $key }
gui-banner-font-unknown = font { $family } not found; using the default

# --- Dialog footer hints (H1 T3, closes #24) — generated: supported
# dialog.* commands × the effective dialog keymap × these labels. NEVER a
# hand-written footer string again: a rebind can't desync it.
help-cmd-pane = index ↔ text
help-cmd-page-up = scroll up
help-cmd-page-down = scroll down
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
# S3 (shell-integration): whether the wrapper is EVAL'd in the caller's rc
# file cannot be seen from a child process — same honesty limit as the two
# footers above — so this names the instruction instead of a pass/fail.
cli-doctor-footer-shell-init = cd-on-quit: run `eval "$(norte shell-init bash)"` (or zsh/fish) from your shell's rc file
cli-doctor-detail-connections-parse = connections.toml does not parse (fix or remove it)
cli-doctor-detail-connections-none = no connections.toml, or no connections configured
cli-doctor-detail-plugin-digest-stale = { $id }: manifest capabilities changed since approval; re-approval required
cli-doctor-detail-plugin-help-truncated = { $id }: its help.md is over the size limit and is served cut short
cli-doctor-detail-plugin-help-lossy = { $id }: its help.md has bytes that do not decode; they render as replacement characters
cli-doctor-detail-plugin-help-empty = { $id }: it announces a help.md that serves nothing: empty, unreadable, or a symlink pointing outside the plugin's own directory
cli-doctor-detail-plugin-help-bad-header = { $id }: its help.md opens a +++ header that does not parse; the whole header is ignored and its text is read as prose
cli-doctor-detail-plugin-help-foreign-command = { $detail } — its help.md declares a command it does not own; that row is dropped
cli-doctor-detail-plugin-help-shadows-topic = { $id }: its id is also a built-in help page; the plugin page is not shown

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
keymap-unavailable-not-built = { $command }: not built yet ({ $reason }, issue #{ $issue })
keymap-unavailable-not-here = { $command }: not available here
keymap-count-ignored = { $command } does not take a count ({ $count } ignored)
keymap-reason-archive-write = writing archives
keymap-reason-editor = built-in editor
keymap-reason-tree = directory tree panel
keymap-reason-tabs = panel tabs
keymap-reason-sort = sort commands
keymap-reason-task-walk = walking the task board
keymap-reason-properties = properties and directory size
keymap-reason-connections = connection management
# The SHORT form, for a surface whose ROW already names the command: the
# which-key panel (K3a) and the reference sheet (K3b, `norte help keys`
# included). Unlike `keymap-unavailable-*` — the sentence the status bar
# prints on a key that was pressed — it drops the command, but it keeps
# "not built yet": the CLI sheet writes to a pipe and cannot dim a row, and
# the bare reason would read as a description of what the key does.
# `#132` and not `issue #132`, unlike the long form: the which-key panel is
# the surface with no room, and the six extra characters pushed the NUMBER —
# the only actionable part — off the right edge of a 60-column panel. The
# clause already says "not built yet", so `#` needs no introduction.
keymap-short-not-built = not built yet ({ $reason }, #{ $issue })
keymap-short-not-here = not available here

# which-key overlay (K3a): the panel that lists what can follow a pending
# prefix.
whichkey-more-keys = more keys
# A row the panel had no room for is COUNTED, never silently dropped:
# a box that just ends implies the list ended with it.
whichkey-truncated = … { $shown }/{ $total }

# Shortcut editor (K3c): the Shortcuts screen, opened from Settings. It lists
# every bound key AND every runnable command nothing presses — the reference
# sheet above answers "what does this key do", an editor must also answer "how
# do I press X".
shortcuts-title = Shortcuts
shortcuts-hint = [↑/↓/pgup/pgdn] navigate · [enter] rebind · [ctrl+u] unbind · [esc] close
# Capture mode. Esc is what cancels it, so Esc is the one chord the editor
# cannot capture, and the reader is told rather than left pressing it. `mod+`
# is the other honesty: crossterm never delivers Cmd without the Kitty
# keyboard protocol, which norte does not enable, so a chord captured here is
# a Ctrl one on every platform (ADR 0043 decision 9).
shortcuts-capture-hint = press the new key · [esc] cancel
shortcuts-capture-note = esc cancels (so it cannot be bound here) · mod+ is ctrl in the terminal
shortcuts-confirm-hint = [enter] save · [backspace] another key · [esc] cancel
# A row for a command no key presses.
shortcuts-no-key = (no key)
# The verdict, shown BEFORE the confirmation. Only the first two can be
# confirmed at all; the rest are what the loader would reject, and refusing
# here is what stops a keymap.toml that reverts the whole map on reload.
shortcuts-verdict-free = free — nothing else uses it
shortcuts-verdict-replaces = replaces { $command }
shortcuts-verdict-replaces-unavailable = replaces { $command } ({ $reason })
shortcuts-verdict-prefix-clash = refused: { $chord } already uses it ({ $command })
shortcuts-verdict-sacred = refused: reserved for { $command }
shortcuts-verdict-digit = refused: this preset reads digits as repeat counts
shortcuts-verdict-empty = refused: no key captured
shortcuts-verdict-esc = refused: esc always cancels a pending sequence
shortcuts-verdict-unwritable = refused: { $chord } cannot be written to keymap.toml
# The door (`rebind_dry_run`) refused after the confirm. The load diagnostic
# is NOT quoted: it may embed text from a project layer that arrived with a
# cloned repository, and this goes to the status bar (#73).
shortcuts-refused-load = not saved: that binding would leave a keymap that does not load
shortcuts-refused-shadowed = not saved: { $command } keeps that key (a project keymap outranks yours)
shortcuts-refused-shadowed-unavailable = not saved: { $command } keeps that key ({ $reason }, a project keymap outranks yours)
# Unreachable while the editor is open (the same preset lookup built the maps
# it is showing), and worded rather than asserted away: rule 6 does not make
# exceptions for unreachable.
shortcuts-refused-preset = not saved: the active keymap preset is unknown
msg-shortcut-bound = { $chord } now runs { $command }
# The GUI's own unbind still matches byte-exactly and speaks about the FILE,
# never about what the key does now (#141: a `[global]` binding, a twin
# spelling, or another layer still binding the key each make "removed" true
# and useless). The TUI's unbind is worded from the rebuilt map instead —
# `msg-shortcut-unbound-cleared` below, or `msg-shortcut-bound` reused when the
# key now runs something else — and keeps this one only for its own no-op.
msg-shortcut-unbound = removed from your keymap.toml: { $chord } → { $command }
msg-shortcut-unbound-cleared = { $chord } does nothing now
msg-shortcut-nothing-to-unbind = nothing removed: nothing in that section of your keymap.toml matched that key
msg-shortcut-not-bindable = that key cannot be captured here
# K3c #141: a binding read from `[global]` merges into every screen, so
# `Screen::section` never names it and this editor must not write there from
# a row that names one screen — that write would change all three. The row
# says where it actually lives instead of silently doing nothing.
shortcuts-row-global = bound in [global]; edit keymap.toml to change it

# The GUI's own wording for the same screen (K3c c4). Two things differ from
# the terminal's and neither is cosmetic. It has a mouse, so the hint names
# click. And it CAN see Cmd/Super — gpui reports `platform`, crossterm never
# delivers it without the Kitty keyboard protocol norte does not enable — so
# a chord captured here may be one the terminal frontend can never press.
# That is not a refusal (it works in this window), so it is said on the row
# at capture time and again on the confirmation, rather than discovered
# months later in the TUI.
gui-shortcuts-hint = [↑/↓/pgup/pgdn/click] navigate · [enter] rebind · [ctrl+u] unbind · [esc] close
gui-shortcuts-capture-note = esc cancels (so it cannot be bound here) · ⌘ works in this window only
gui-shortcuts-cmd-note = ⌘ is not reachable in the terminal frontend
# The write landed but the keymap did not: this frontend watches no files, so
# a rebind reaches the keyboard only through the rebuild that follows the
# write, and that rebuild is all-or-nothing. Saying "saved" alone would
# describe a key that did not change.
gui-msg-shortcut-saved-not-applied = saved, but this window kept the previous keymap

# ---------------------------------------------------------------------------
# The graphical host: what its dialogs answer, and why an action could not be
# done. These keys are chosen in RUST and painted by the renderer through
# `t(key)`, so they appear in no `t("…")` literal on the TypeScript side and
# the renderer's own catalogue test cannot see them. Twenty-one of them were
# missing and painted as their own identifiers — including both buttons of
# the dialog where a human approves an agent's mutation.
dialog-body-truncated = … showing { $shown } of { $total }
dialog-destination = Destination:
dialog-confirm = Confirm
dialog-cancel = Cancel
dialog-approve = Approve
dialog-deny = Deny
dialog-overwrite = Overwrite
dialog-newer = Only if newer
dialog-rename = Rename automatically
dialog-skip = Skip
modal-mkdir-title = New directory
modal-new-file-title = New file
modal-rename-title = Rename
modal-delete-title = Move to trash?
# The three verdicts of the shared resolver, for a key that IS bound.
cmd-here = that is already what this does here
cmd-not-built = not built yet
cmd-not-here = not available on this screen
host-key-unmapped = that key is not bound
host-cannot-view-dir = a directory has no viewer
host-nothing-to-view = nothing to view
host-open-file-not-implemented = opening files is not built yet
host-column-not-sortable = that column does not sort
host-help-over-input = help does not open over a text field
host-layout-broken = that layout file does not parse
host-name-too-long = that name is too long
host-name-not-editable = that name does not fit on screen: it cannot be edited here without truncating it
host-cannot-transfer-root = a root cannot be copied or moved
host-no-other-slot = there is no other panel
notify-approval-title = norte: an agent is asking
notify-approval-body = { $op } — { $who }
notify-task-done = norte: finished
notify-task-failed = norte: something failed
notify-task-body = { $kind }: { $what }
host-pick-destination = choose the destination folder
host-bad-destination = that folder cannot be named here
host-drop-empty = nothing arrived
host-drop-unusable = what arrived are not paths on this machine
err-bad-name = that is not a legal name here
host-no-target-designated = there is more than one panel: designate a destination first
host-plan-not-applicable = the core did not accept this plan: nothing would be renamed
host-plan-abandoned = the rename plan was abandoned
host-plan-asking = asking the model for a rename plan…
host-plan-acknowledge = press a key again to answer: this window opened on its own
host-dialog-acknowledge = press a key again to answer: this dialog opened on its own
dialog-ok = Got it
modal-batch-report-title = batch rename report
modal-undo-report-title = undo report
modal-undo-summary = { $undone } reverted, { $skipped } irreversible skipped
modal-undo-left-in-place = { $n } creation(s) stay where they are: the destination has no trash, and an undo never destroys beyond recovery
modal-undo-blocked = the undo stopped at journal entry { $seq } ({ $error }): everything older than it was NOT undone
modal-undo-batch-stuck = a rename batch did not come back; it is now called:
modal-undo-denied = { $n } unit(s) the policy denied: they were left untouched
modal-undo-unsupported = this daemon cannot report on an undo, so what came back is unverified
modal-undo-report-failed = the undo report could not be fetched, so what came back is unverified
task-undo-done = { $n } reverted
task-undo-unverified = finished, report unavailable
modal-batch-summary = { $applied } applied, { $back } rolled back
modal-batch-stuck = could not be put back; it is now called:
modal-batch-stuck-journalled = the journal describes it: an undo can finish the job
modal-batch-stuck-unjournalled = the journal never recorded it: only a person can undo this
modal-batch-uncertain = unknown whether this step took effect; look here:
modal-batch-compensations-lost = { $n } lost compensations: a session undo will stop there
modal-batch-unsupported = this daemon cannot report on a batch, so the outcome is unverified
modal-batch-report-failed = the report could not be fetched, so the outcome is unverified
task-batch-applied = { $n } renamed
task-batch-half = batch left half done: { $applied } applied, { $back } rolled back
task-batch-unverified = finished, report unavailable
host-plan-unseen = scroll through the whole plan before applying it
host-same-directory = source and destination are the same directory
host-batch-too-large = too many entries for a single transfer
host-batch-folds-to-one = two of the marks are one name on the destination
host-task-board-full = the task board is full
host-read-only = this window is mounted without effects: it does not write
host-no-tabs = this panel is not in a tab group
host-no-such-tab = there are not that many tabs
host-task-running = that task is still running: stopping it is another key
host-nothing-selected = nothing is selected
host-not-local = that is not on this disk: there is no native path to hand the desktop
host-no-desktop = this window has no desktop behind it: it cannot copy to the clipboard or launch anything
msg-paths-copied = { $n } path(s) on the clipboard
msg-paths-copied-osc52 = { $n } path(s) sent to the terminal clipboard (OSC 52); check by pasting
msg-clipboard-failed = the clipboard refused the paths
msg-opening-external = opening with the desktop’s application…
msg-opening-terminal = opening a terminal here…
host-plugin-running = running the extension command…
host-not-an-int = that is not a whole number
host-out-of-range = the schema bounds it between { $min } and { $max }
host-value-rejected = the value does not fit that key’s schema
host-no-extension = no extension is selected
host-extension-not-approved = approve its capabilities first: without them the core will not load it
host-extension-changed = the capabilities it declares changed since the question was asked: asking again
host-extension-too-many-caps = it declares more capabilities than fit on one screen; not granted from here
host-extension-updated = done; the catalogue is being fetched again to confirm it
modal-extension-approve-title = grant these capabilities?
host-settings-read-only = settings are read-only here
msg-nav-at-root = already at the root
msg-nothing-selected = nothing selected
msg-batch-summary = { $total } transfers: { $ok } ok, { $fail } failed
