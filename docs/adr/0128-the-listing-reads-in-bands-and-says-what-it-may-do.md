# 0128 — The listing reads in bands, and says what it may do

- Status: accepted
- Date: 2026-09-20
- Decision makers: Oscar González
- Protocol: unchanged. Bridge: **80** — `View::row_stripes`, a boolean. The
  band's colour already crossed, as the theme role `stripe` in `--stripe-bg`.
- Related: ADR 0124 (the ladder this amends), ADR 0108 (how a chrome role is
  added), ADR 0039 (attributes travel in `Entry::attrs`), ADR 0081 (`chmod`,
  the other half of permissions), #108 (the shared column layout)

## Context and problem statement

Three requests about the listing arrived together, and they turned out to be
one problem seen from three sides: **a listing that is wide is hard to read
across, and a listing that is honest about Unix is missing a column.**

1. Krusader, Total Commander and every file manager with wide rows stripe
   alternate rows. norte had nothing: no role, no setting, no code, in either
   frontend.
2. Krusader on Linux shows `rwx` permissions by default. norte *could* —
   `attr:posix.mode` with `format_mode_rwx` has rendered `drwxr-xr-x` since
   #108 — but only if the reader knew to write it into `[ui.columns]`. A
   capability nobody can find is not a capability.
3. The system help has two panes and moves focus between them, and the screen
   did not say which one had the keys: the sidebar highlighted its row with
   `Role::Selection` unconditionally, and the body highlighted nothing at all
   unless it had focus. Two live cursors, or none.

## Decision

### 1. The pyjama is a theme role, a config key, and nothing else

`[ui] row_stripes` (default **off**) paints the odd rows of a listing on
`Role::Stripe`. Parity is the position of the PAINTED row, so a listing
narrowed by quick-search still alternates.

`Stripe` joins the vocabulary the way ADR 0108's chrome roles did — outside
`CORE`, outside `REQUESTABLE`, colourless `fallback` — with **one
difference: it is not derived.** The window derives `hover` from
`pane-focus-background` because a plausible hover is "the focused pane's
background, a bit more so". A plausible band is not: it is a small step away
from the pane background, and how small is a property of the palette. A fixed
step in CSS is invisible on `gruvbox-light` and a stripe of paint on
`retro-crt`. So each of the ten bundled themes states its own band, and a
theme that says nothing gets no band — which is the listing as it was.

The band loses to everything that MEANS something: the cursor, a marked row,
and in the window the row under the pointer. Off by default because the band
earns its keep on a wide pane and gets in the way on a narrow one, and
because a reading aid should be the reader's choice.

### 2. The permissions column comes switched on where permissions exist

On `file` and `sftp`, `attr:posix.mode` is part of the **default** column
set. No new column class, no `Entry` field, no wire change: the formatter,
the attribute and the request funnel all existed.

Two guards make an automatic column honest, and both are new:

**It only appears once the backend says it has POSIX permissions.** The
decision reads the provider's `AttrCatalog`, not the scheme, and `None` — not
answered yet — means no. `file` on Windows never announces `posix.mode`, and
a "Mode" header over twelve blank cells is name width spent saying nothing.
Appearing one frame late is cheap; being there unable to speak is not.

**It gives way first.** This AMENDS ADR 0124, which exempts `attr:` columns
from the ladder "because someone asked for them on purpose". That reasoning
is exactly right and exactly why the exemption must not reach this one:
nobody asked for it. So the ladder grows a rung above hiding Type, and it
fires only while the listing is painting the default set. Write the column
into `[ui.columns]` yourself and the exemption applies again — because then
someone did ask.

Sorting by it stays impossible (`SortColumn` is still `Name`/`Size`/`Mtime`),
which is the pre-existing rule for every attribute column and out of scope
here.

### 3. Focus in the help is said the way the panes say it

The sidebar's cursor and the body's cursor are BOTH always drawn, one with
`Role::Selection` and the other with `Role::SelectionUnfocused`, swapping on
`Focus`. That is verbatim the rule the two file panes have followed since the
2026-09-10 spec, and the window gets the matching CSS: the body gains the
accent inset the cursor row has, and each side's unfocused cursor dims.

Not a second border. The help is ONE frame, and splitting it would cost a
column of the topic titles — the column that makes an index readable. The
role carries it, and without a theme the monochrome fallback still separates
them, because `SelectionUnfocused` falls back to `reverse().dim()`.

## Consequences

- One more role (29 in `ALL`, still 18 in `CORE`), one more `[ui]` key, one
  more settings row, one bridge version.
- `fitted_columns` and `column_widths` both take the catalogue now. They had
  to take the same one: two answers to "which columns are there" would leave
  the border the mouse drags grabbing the column next door.
- A default `file` listing is one column wider than it was. On a narrow pane
  it is not, because that column is the first to go.
- **A local listing now stats every entry it streams.** Asking for any
  advertised attribute takes `norte-vfs-local` off #52's lazy path and onto
  one `lstat` per entry — which is also where the real `size` and `mtime`
  come from, so the listing gains as much as it pays. It streams, and the
  host paints after the first page, so what stands between the keypress and
  the first rows is ~100 stats and not the whole directory. On local storage
  that is not measurable. On a `file://` path that is really NFS, SMB or
  SSHFS it is round trips, and a reader there can put `[ui.columns]` in their
  config without the permissions column and get the old path back. Windows
  keeps the fast path either way, because it never announces `posix.mode`.
- **The attribute catalogue is cached per SCHEME**, in both frontends. It only
  fed hints and labels before; now it also decides whether a column exists, so
  two SFTP panes on hosts that answer differently share one answer and the
  last one navigated to wins. Pre-existing, out of scope here, and worth an
  issue.
- **The permissions column made two pre-existing gaps load-bearing**, both
  found by the encoding audit and both fixed here rather than inherited:
  `format_mode_rwx` painted `-` for fifos, sockets and device nodes — so a
  listing of `/dev` read as ordinary files — and painted `-` for a mode with
  no type bits at all, which is what an SFTP server that reports only
  permissions sends and what `MemProvider` emits. The second was the worse
  one: a directory read as a regular file while the icon on the same row said
  otherwise. Both now render the seven `S_IFMT` classes and `?` for anything
  else. The canonical corpus grew `posix_modes()` to hold the line.
- **A cut cell now says it was cut.** The listing truncated an over-wide cell
  silently, which only mattered while every column was opt-in. Narrow the
  permissions column by dragging its border and `-rw-r--r--` and `-rw-r-----`
  both became `-rw-r--`: two different answers, one string, no sign anything
  was missing. The renderer spends one cell on `…`, the same rule the corpus
  already called `truncation_twins` for titles.
- Found while doing this: the window's palette test helper walked the row
  list across snapshots, so any extra snapshot — and asking for an attribute
  catalogue is one — made it count a step twice and run the neighbouring
  command. `pane.edit` became `pane.edit-new`, a dialog instead of an effect.
  Fixed by draining what is already published before asking for the current
  state. It was a real race, not a flake: deterministic, and it would have
  bitten the next feature that publishes an extra snapshot.

## Alternatives considered

- **A `Builtin::Perms` column** instead of reusing `attr:posix.mode`. It
  would be sortable and on the ladder for free, but it invents a hybrid: a
  builtin whose value comes from an attribute. The three column classes are a
  good boundary and worth more than the shortcut.
- **Show the permissions column everywhere and let it be empty.** The help
  already says an unanswerable column stays empty, and that is right — for a
  column the reader CHOSE. norte choosing one that is always empty is norte's
  bug, not honesty.
- **Derive the band in the stylesheet**, like the ten chrome roles. Tried on
  paper against the ten bundled palettes and it fails at both ends; and the
  terminal cannot derive at all, so the TUI would have no pyjama.
- **A second border in the help** to mark focus. Costs a column of the titles
  and changes the text layout, which would churn four golden files to say
  what a role already says.
