# 0155 — Typing a name jumps to it, in the presets that reserve letters

- Status: accepted and implemented
- Date: 2026-09-25
- Decision makers: Oscar González
- Protocol: unchanged. `keymap.toml` gains one preset-only key,
  `type_to_search`; `docs/schema/keymap.schema.json` publishes it.
- Related: ADR 0006 (keymap layers), ADR 0044 (counts), ADR 0045 (the keymap
  schema is the documentation)

## Context and problem statement

Krusader moves through a directory by typing the start of a name: press `d`
and the cursor is on the first name beginning with `d`. norte's Krusader
preset already reserved the bare letter for it — its header says no bare
letter is ever bound, so as not to shadow that search — but nothing
implemented it. A letter did nothing, and the quick search was only reachable
through `ctrl+f`, matched anywhere in the name, and filtered the listing by
default.

## Considered options

1. **A preset-level key, `type_to_search`, set by the presets whose source
   gives the bare letter to that search.** A printable key that no binding
   takes, with nothing half-typed, opens a quick search in jump mode that
   matches by PREFIX.
   - Good: the preset that reserved the letter is the one that uses it; a
     bound key still wins.
   - Bad: one more preset-only key, refused in user layers like `counts`.
2. **Any unbound letter, in every preset.** In orthodox, vim and cua some
   letters run commands and the rest would search, with nothing on screen to
   say which is which.
3. **A `[ui]` setting.** Moves the decision away from the preset whose key
   layout makes it safe, and lets a reader turn it on in vim, where it would
   break the same way as option 2.

## Decision

Option 1, and only Krusader sets it. Far, Norton and Total Commander send a
bare letter to their command line; their letter search is Alt+letter (Far's
Fast Find, NC's speed search) or Ctrl+Alt+letter (TC), a family of chords the
keymap grammar cannot bind to one command. Turning the key on there would
give the bare letter a meaning those programs never gave it.

The search opened this way is the ordinary quick search — up/down walk the
matches, Enter confirms, Esc leaves the cursor where it jumped — with
`Match::Prefix` instead of `Match::Contains`. `/` and `ctrl+f` keep matching
anywhere and keep following `[ui] quick_search`. A letter no name starts with
opens nothing: an empty search would hold the arrows until Esc.

Both frontends start it from the resolver's miss through one shared rule
(`Effective::typed_search_char`, `PaneState::type_to_search`), and each adds
its own "a listing holds the focus" check, because their focus models differ.

## Consequences

- A test pins which presets set the key and that none of them binds a bare
  letter or digit in the panels, which would make that letter unreachable.
- A user who wants it in another preset cannot turn it on from a layer; that
  is the same line ADR 0044 draws for `counts`.
- Far's and Norton's Alt+letter search is still missing. It needs a way to
  bind a chord family, which is its own decision.
