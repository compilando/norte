# #261: the window's test matrix, automated where it can be

A draft comment for issue #261 (2026-10-15). Not posted.

## The X11 finding, corrected

The issue says two things that no longer hold:

- **"Nothing here can be scripted."** `just gui-e2e` (`scripts/gui-e2e/run.sh`)
  drives the real debug `norte-gui` on Xvfb, inside bwrap with a throwaway
  home, using xdotool. It checks two things: what crossed the bridge
  (`NORTE_GUI_TRACE`, debug builds only, secrets redacted) and what the
  renderer reports it shows (the `dom` probes in
  `crates/norte-gui-tauri/ui/src/probes.ts`).
- **"`GDK_BACKEND=x11` renders a blank webview."** This reproduces on Xvfb,
  and the cause is WebKitGTK's DMA-BUF renderer on an X server without DRI3
  (`MESA-EGL: DRI3 error` in the log). The page lays out completely: every
  probe read a full DOM, menus and dialogs included. But nothing is painted,
  and every screenshot was a flat background.
  `WEBKIT_DISABLE_DMABUF_RENDERER=1` makes it paint. The harness now always
  sets it.
  Until now the harness's screenshots were blank and nobody had looked at
  them. Its oracles were never pixels, so it still passed.

  On the reference machine under Xwayland this is the probable cause, not a
  confirmed one. The product question is still open: should the window set
  that variable itself when it runs on X11, the usual Tauri workaround? Or
  should X11 without DRI3 be recorded as a limitation? Either way,
  "X11 cannot be tested at all" is false.

## What is automated, and where

All scenarios are in `scripts/gui-e2e/scenarios/`. All pass as of
2026-10-15.

| matrix item | scenario | what it checks |
| --- | --- | --- |
| keyboard-only complete navigation | `matrix-keyboard-only.scn` | Both panes: walk, enter, go up, Tab. Menu: F9, the arrows along the bar, alt+m, Alt alone, Escape. The mkdir dialog: the field takes the focus, Enter creates the folder on disk, Escape cancels. The box: open, type, close. A side panel: open it, move the focus ring onto it (alt+o), close it. Help (F1), settings (F11) and the theme selector (alt+9), each opened and closed. |
| light and dark themes | `matrix-theme-light-dark.scn` | Dark at start. Moving through the selector previews a light theme live, and Escape restores dark. Enter keeps the theme and saves it, and it survives a restart. Switching back works too. `tone` is read from the body's painted background luminance, after the CSS variables land. |
| context menu | `matrix-context-menu.scn` | Opens by keyboard (shift+F10) on the row under the cursor, and by right click on the row under the pointer. A click on an item runs it and closes the menu. |
| clipboard, out of the window | `matrix-context-menu.scn` | "Copy the path" puts `/home/ada/data/a/notas.txt` on the X clipboard. The window does this through xclip, inside the sandbox. |
| clipboard, into the window | `matrix-clipboard-paste.scn` | An order pasted into the box (ctrl+v) arrives whole, spaces included, and runs (one row marked). |
| drag selection | `matrix-mouse-selection.scn` | The listing has no rubber band. The mouse selection it does have: click, then shift+click for a range (marked by the host), and ctrl+click to toggle one row. Dragging the border between the panes resizes them (`resize_slot`). |
| daemon handover with the window open | `matrix-daemon-handover.scn` | `norte daemon stop --handover` runs from a second sandbox on the same runtime dir. The window sees the drop (`reconnecting`), starts the replacement itself, comes back `connected` on the same folder and cursor, and keeps working. **It found a bug, see below.** |
| window restore after a restart | `matrix-session-restore.scn` | A clean quit (`q`), then a restart with no folder on the command line. Each pane's folder and the open side panel come back. |
| 100/200 % scaling | `matrix-scale-200.scn` | `GDK_SCALE=2` on X11 gives `devicePixelRatio` 2. At that scale the listing, the box and a dialog still work. |

New harness steps: `restart`, `norte <args>`, `rclick`, `click … <mod>`,
`drag`, `clip-set`, `env`, `expect-dom[-has|-lacks] <probe>`, `expect-clip`,
`expect-jq`, and `expect-[no-]file`. Every oracle now polls for up to 10 s
instead of checking once after a fixed sleep. A pass returns at the first
match, so the cap costs nothing when the check passes. At `GDK_SCALE=2` the
probe once trailed the bridge by more than 3 s under load. Probes: `goto active cursor
slots menu context dialog open focus tone dpr`.

## Bugs found

1. **After a daemon handover the session is never saved again.** The window
   stops being the session's owner, and nothing re-claims ownership after
   reconnecting. Ownership is taken only by the `session_get` at startup;
   `session_placed` then marks the window loose. So the status bar shows
   "session not saved" (`status-session-detached`) and the banner stays up.
   `session.json` keeps the pre-handover folders at revision 2 while the
   screen has moved on. The next restart therefore reopens the old folders.
   It reproduces on every run. The oracle is written into
   `matrix-daemon-handover.scn`, commented out until the fix lands; it fails
   on today's trace. The TUI may share the pattern; this was not checked.
2. **The theme selector does not scroll to its cursor.** `.theme-choices`
   has `max-height: 30vh`, and `paintTheme` rebuilds the list on every move
   without revealing the selected row. At 1200×800 only the first four themes
   are visible: gruvbox-light, catppuccin-latte, the vscode pair and both
   retro themes are previewed and chosen while their row is off-screen. The
   title still names them. The settings list already solves this
   (`revealTarget`). Seen in `target/gui-e2e/matrix-theme-light-dark-preview.png`.
3. **Harness flake, fixed:** a run started right after another one on the
   same display could find the display free while the old Xvfb still held
   its lock. Its own Xvfb then died and GTK failed to initialise. Cleanup now
   waits for Xvfb, and start-up checks that it is alive.

Observations, not bugs:

- The focused pane is not part of the session (`SessionBody` has no field
  for it), so after a restart the focus is on the left pane.
- Opening a side panel by keyboard (alt+j) leaves the focus where it was, so
  an immediate alt+x closes the file pane, not the new panel.

## Human-only, and why

| item | why it cannot be scripted here |
| --- | --- |
| IME and dead keys with a real input method | xdotool types keysyms directly. Nothing goes through an IBus/Fcitx pre-edit, so the renderer's `isComposing` guard never sees a composition. |
| fractional scaling (150 %) | Needs a compositor that scales by a fraction. Xvfb only offers integer `GDK_SCALE`, and that is automated. |
| Orca / AT-SPI traversal | Needs a running accessibility bus and a screen reader. What Orca announces is the claim, not the roles in the DOM. |
| compositor restart on Wayland | Needs a real Wayland session to kill and bring back. The harness is X11 only. |
| the window under a real window manager and on Wayland | Xvfb has no WM, so geometry, decorations and focus stealing go untested. |
