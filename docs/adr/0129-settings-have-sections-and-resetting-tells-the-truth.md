# 0129 — Settings have sections, and resetting tells the truth

- Status: accepted
- Date: 2026-09-20
- Decision makers: Oscar González
- Protocol: unchanged — none of this crosses the socket. Bridge: **81** —
  `SettingsView.index`/`.query`/`.shown`/`.total`, `SettingRowView.modified`,
  and the actions `settings_query`, `settings_jump_section`,
  `settings_reset`.
- Related: the S1–S4 design of 2026-07-24 (which shipped the flat list this
  amends), ADR 0079 (the active profile decides the writing layer), ADR 0026
  (the project layer is "I opened this repository", not "I vouch for this"),
  ADR 0077 (parity between the terminal and the window)

## Context and problem statement

The settings screen shipped as a curated flat list with a free-text filter —
deliberately, as v1. Two years of entries later it holds 33 of them, and the
reader who opened it saw one heading, `General`, followed by everything.

Three things were wrong at once, and only the first looks like a bug:

1. **The heading never came back.** Both frontends anchored the view to the
   selected ROW, and the first row of a section sits one line below its
   heading. Scroll to the bottom, come back up, and `General` stayed above
   the edge for good — the list lost the only label saying where you were.
2. **Every entry was in `General`.** The `Section` enum had two variants and
   the other one (`Plugins`) did not appear in the catalog at all. The
   reader's mental model — "settings are grouped" — had nothing behind it.
3. **Nothing said what you had changed.** A value differing from the factory
   default looked exactly like one that did not, and there was no way to put
   one back: `norte-config` could write a key and had no way to remove one.

## Decision

**Seven sections, an index, a pinned heading, a search that narrows, and a
reset that says what it actually did.** In both frontends, because a setting
that is findable in one surface and not in the other is a setting most
readers do not have.

### The reparto lives in one table, not in 33 fields

`SettingDef` had a `section` field written next to each entry. It is now
`section_of(id)`, a single match. Written 33 times beside each `id`, the
grouping cannot be read at a glance or audited in one pass — which is
exactly how all 33 came to say `General`. An id with no arm lands in
`Behavior`, visible rather than hidden, and a coverage test catches it; the
alternative, a wildcard arm handing out some section, misfiles in silence.

`build_rows` sorts by section (stably, so the catalog order survives inside
each one). This was not optional: the catalog is grouped by `norte.toml`
section, which is a different grouping — `ui.lang` is Behaviour and
`ui.quick-search` is Keyboard, two rows apart.

### "Modified" is measured against the factory value

Not against "there is a key in your file". A key written with the value it
already had is not a change, and marking it as one would send the reader to
reset something that does nothing. It is computed with `current_value` — the
same function that paints the value — against a config loaded from zero
layers, so it cannot drift from the schema.

### Resetting removes the key, and the dot stays honest

`persist_unset` removes the key from the writing layer — the profile's if one
is active, the user's otherwise (ADR 0079). It writes no default: freezing
today's default into the file is the opposite of what the reader asked for.

**And it does not always restore the factory value.** If the system, the
profile or the project sets the same key, the value changes and still is not
the default. We do not build layer provenance to explain this. The row is
rebuilt after the write — both frontends already do that — and the dot is
read back: still modified means another layer sets it, and the status bar
says so (`settings-still-set-elsewhere`). What has happened, said with what
is already known.

Two consequences of measuring against the default rather than "is the key in
your file", and both are deliberate: a key written by hand with the value it
already had shows **no** dot and cannot be removed from this screen (edit
the file for that), and a key set only by the system layer shows a dot the
reader never caused — which is why the label reads "not the factory value"
and not "you changed this".

### The new keys are local to the overlay

`[`, `]` and `ctrl+r` are handled where the settings screen reads its keys,
not in the command catalogue. A catalogue command must be bound in all seven
presets, needs two `help-cmd-*` strings and a CLI golden, and this overlay
consumes every printable character for its filter anyway. The price is that
a bracket cannot be searched for, and no setting's name contains one. This is
a deliberate exception to "a key change is not done until every preset is
done", and it is confined to keys that only exist inside one modal.

### Two things the design asked for and this does not do

Written down rather than forgotten, because silence and an oversight read
the same:

- ~~**`tab` does not move between the index and the list.**~~ **Reversed the
  same day** (bridge 82). The argument above — "the index is a map, not a
  focus target" — was written from the code and not from using it: the first
  reader to open the screen said the index "is not very navigable" and asked
  for exactly this. `tab` now moves the keyboard between the two halves in
  both frontends, and with the keyboard on the index the arrows walk
  SECTIONS while the list follows, the way the help's sidebar opens a topic
  as you move through it. There is no second cursor to keep in sync: the
  index simply points at the cursor's section.

  Both cursors are drawn at all times and the one without the keyboard is
  dimmed — ADR 0128's rule, which is why `SettingsView` had to grow `focus`
  and each section its stable `key` (pairing a section with its index row by
  translated title would break the day two read alike).
- **The modified dot is always `•`, never `*`.** The design asked for an
  ASCII fallback on terminals without reliable Unicode. There is no
  capability probe in this repository to hang that on, and inventing one for
  a single glyph is the wrong place to start: the panel bar's indicators and
  the tree's branches already assume the same character set.

## Consequences

- `Section::General` is gone. Its Fluent key stays one cycle as an unused
  alias so a half-translated locale does not paint an empty heading.
- The window's settings cursor counts VISIBLE rows. It was flat over
  `rows ++ paths`, with a `debug_assert` saying that only held because the
  window did not filter. It filters now.
- A section this surface does not have is absent from the index; one the
  filter emptied stays, dimmed. An index that changes length while you type
  cannot be used as a map, but one that promises a section that will never
  hold anything is a promise broken.
- The terminal's index disappears below 60 columns, the same degradation the
  pane columns already do.
- In the window the pinned heading is `position: sticky` with an opaque
  background, and the list keeps its `scrollTop` across repaints — it was
  losing the wheel position on every patch from the host.
