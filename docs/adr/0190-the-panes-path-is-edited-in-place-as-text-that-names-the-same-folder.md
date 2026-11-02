# 0190 - The pane's path is edited in place, as text that names the same folder

- Status: accepted
- Date: 2026-11-01
- Decision makers: Oscar González (asked for Krusader's Ctrl+L and approved
  the behaviour; the placement and the rules below were the implementer's
  call, which he delegated)
- Related: ADR 0077 (one rule for both frontends), ADR 0181 (the command
  bar's line editor), ADR 0006 (one command name for every surface).

## Context and problem statement

Krusader's Ctrl+L turns the panel's location bar into a text field with the
path selected. Its readers use it for two things: to copy the path, and to
change it — usually a small change (go up two levels, swap one folder in the
middle). norte had go-anywhere (`app.goto`), which accepts a typed path but
starts EMPTY and only appends, so neither use was covered.

Showing a path as editable text has a trap that displaying it does not: the
text is fed back as a destination. A masked or lossily-decoded path, shown
and confirmed unchanged, would lead to a different folder (rule 1).

## Considered options

1. **Open go-anywhere prefilled with the path.** No new surface. But its
   query has no caret — editing the middle needs one — and its lists
   (history, favourites, commands) are noise for "change one folder".
2. **Edit the path in place, where the pane shows it.** What Krusader and
   every desktop file manager do. A new editable surface in both frontends.
3. **A separate dialog.** Neither in place nor shorter than option 1.

## Decision outcome

Option 2, `pane.edit-path`, with these rules, all in
`norte_frontend::path_edit` so both frontends answer the same:

- **The text names the same folder.** A local folder whose native path is
  UTF-8 and parses back to the same `VPath` is shown natively (`/home/x`).
  Anything else — another scheme, another machine, a name that is not text —
  is shown as its wire (`sftp://h/x`, `file:///caf%E9`), which escapes the
  bytes text cannot carry. What even the wire shows raw (a bidi override, a
  C1 control) refuses to open the line: painting it reorders the line,
  masking it edits another path.
- **Enter resolves like go-anywhere** (`goto::action("path:…")`): one text
  never means two places depending on the box. Relative text is refused, as
  there.
- **Tab completes only against the pane's listing**, and only where the
  text up to the last separator IS the pane's folder. It is the only listing
  at hand; offering its names under another folder would invent them.
- **A navigation that fails gives the typed text back**, unselected, so the
  reader fixes a letter instead of retyping (TUI: on `Cd::Failed`; window:
  when that navigation's listing lands as an error).
- **Copying**: the window's `<input>` copies natively (Ctrl+C). The terminal
  cannot — Ctrl+C quits norte (decision #23) — so the line copies on
  Ctrl+Insert, the CUA copy chord, through the same clipboard path as
  `pane.copy-path`.

Keys: `ctrl+l` in `krusader` (attested), `cua` (the location bar's key in
desktop managers and browsers) and `vim` (its redraw meaning is unused in
norte); `alt+L` in `orthodox`, where `ctrl+l` and `alt+c` are taken.
`total-commander` has the feature (`cm_EditPath`) with no default key,
`far` and `norton` do not have it: unbound there, reached from the Go menu
and the palette.

### Positive consequences

- Both of the reader's uses are covered, in the place they look.
- Showing and confirming never changes which folder: the wire form keeps
  every byte.

### Negative consequences

- A remote or non-UTF-8 path is edited in its wire spelling: `%25` for a
  `%`, and a typed `%41` is a byte, not three characters.
- Tab does not complete under folders other than the pane's own; that
  would need a listing request per keystroke.
- In the terminal any mouse press closes the line; clicking inside it does
  not place the caret.
