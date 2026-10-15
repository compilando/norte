# Search panel, phase B: "Open as listing" — design

- Date: 2026-10-10
- Status: decided by the controller under the user's delegation; pending review
- Depends on: phase A (search panel, protocol 0.86) merged.

## Goal

Results you can act on. Today only the TUI can turn results into a listing,
by taking over the focused pane (`Pane.virtual_search`, `jobs/search.rs`
`launch_search`/`drain_search`); any `cd` loses them and the window has no
equivalent at all. Success: the search panel has **Open as listing**; it opens
a NEW TAB in the focused pane titled `🔍 <query> (N)`, listing every result
file with a Folder column; marks, copy/move/delete/rename/view/edit work as in
any folder; the tab survives navigation in other tabs and can be closed like
any tab. The TUI's Alt+F7-style takeover is replaced by the same tab.

## Decisions

| question | answer |
| --- | --- |
| model | a shared "result set" listing kind in `norte-frontend` (`PaneState` already supports `set_listing`/`extend`): a tab whose source is `Source::Results { query_label, entries: Vec<VPath> }` instead of a directory |
| addressing | no new VFS scheme: entries are absolute `VPath`s from many folders; the tab has no single cwd (`pane.mkdir`, AI organize, "pane location" gestures refuse with a reason, as the TUI's virtual mode does today) |
| columns | Name, Folder (relative to the search root), Size, Mtime; sort works; Folder sortable |
| live updates | snapshot at open, plus the rest of the run if it is still streaming; afterwards no watching; `F5`-refresh re-runs the same search into the tab |
| file ops | per-entry absolute paths (host `transfer.rs` already joins `dest.join(basename)`); name collisions across folders go through the normal collision policy; moved/deleted entries leave the tab |
| session | the tab is restored as its query (re-run on restore), not as a stale list |
| TUI Alt+F7 dialog | its results now open the same tab instead of hijacking the pane (`virtual_search` removed) |
| wire | bridge bump (tab kind + Folder column); protocol untouched |

## Testing

- Model: result-set tab from hits, refuse location-only gestures, refresh re-runs, entries removed after delete/move.
- TUI + host: open-as-listing creates a tab, marks + copy across folders, collision policy, session restore re-runs.
- Webview: tab title with count, Folder column.

## Out of scope

- Editing the list by hand (add/remove arbitrary files).
- Saving a result set to disk.
