# Panels usability — plan

Source: the usability review of 2026-10-07 (TUI in tmux, window under Xvfb;
screenshots in the session scratchpad `shots/`). Two lines in parallel, on
disjoint crates; one merge to `main` at the end.

- **Line T (controller):** `norte-tui`, `norte-frontend`, `norte-i18n`,
  presets, `norte-config`. Branch `fix/panels-usability`.
- **Line W (agent, own worktree):** `norte-ui-host`, `norte-gui-tauri`
  (Rust + `ui/`). Branch `fix/panels-usability-window`. Never edits Line T's
  crates; if it needs a shared change, it says so in its report.

## Line T

- **T1 Keys never swallowed by a side panel.** Every side panel of the TUI
  (places, tree, processes, log, disk map, timeline, metadata, terminal)
  lets through: every panel toggle (`layout.places`, `layout.preview`,
  `layout.processes`, `layout.metadata`, `layout.log`, `layout.disk-map`,
  `layout.timeline`, `layout.terminal`, `pane.tree`), `layout.close-slot`
  (closes THAT panel), `layout.focus-next/prev`. One shared list, not one
  allowlist each.
- **T2 One name per panel**, the same in bar, tab, title and menu, both
  locales: Places, Tree, Viewer, Processes, Details, Log, Disk map,
  Timeline, Terminal (es: Sitios, Árbol, Visor, Procesos, Detalles,
  Registro, Mapa de disco, Historial, Terminal).
- **T3 Visible state.** The View menu marks open panels. `PanelState` gains
  "open but behind a tab / not placed" (shared `panelbar`), painted in the
  TUI column; Line W paints it in the window.
- **T4 Space.** Bottom docks no taller than a third of the body when the
  terminal is short (shared layout); listing title cut from the LEFT.
- **T5 Keys for Details and Timeline** in the seven presets (new-command
  checklist: catalog, help, goldens).
- **T6 Small texts:** "1 files" plural, key bar spacing at 80 columns,
  places names, viewer on a folder says what is in it.
- **T7 Times:** log and timeline in local time like the listing.

## Line W

- **W1** Terminal panel: `alt+o` / focus-next always leaves it; Esc in any
  side panel returns focus to the listing; visible focus = where keys go.
- **W2** Layout buttons in the menu bar: split acts on the active LISTING,
  never inside a dock; every button has a tooltip.
- **W3** Disk map moved to another dock stays alive (it went blank).
- **W4** `alt+x` on a side panel: the right notice (not "Alt+h splits
  again").
- **W5** Horizontal splitter above the bottom dock drags; no text
  selection on the app's chrome.
- **W6** Paths in path bar, viewer and timeline headers cut from the LEFT.
- **W7** Right-click on a tab: our own small menu (close, move); tabs get a
  close "×"; middle-click closes.
- **W8** Paint the "open behind a tab" state in the activity bar (after
  T3 lands: the DTO already carries the state).
- **W9** Empty states: map with nothing to show says so in every dock;
  favorites hint; layout picker preview with a legend.
- **W10** Socket path too long: the error says so.

## Rules for both

TDD; `just t <crate>`; no gate runs except one `ci-fast` by the controller
at the end; reviews per line before merging; commits with the attribution
trailer via `git commit -F`.
