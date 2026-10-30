# 0189 - An order suggestion is taken with Tab, and Enter runs a line that parses

- Status: accepted
- Date: 2026-10-30
- Decision makers: Oscar González (go-ahead on #429; the rule below was the
  implementer's call and is open to his review)
- Related: ADR 0185 (the order grammar), ADR 0077 (one rule for both
  frontends), issue #429.

## Context and problem statement

The `:` box offered completions only for verbs and `sel` keys, and the first
one was always chosen: Enter took it. #429 asks for what can come next —
`>other`, `>here`, folders, `ext:`/`type:` values, the pane's names. Those
are VALUES, and a value under the cursor changes what Enter means: with
`cp >/tmp/new` typed and `/tmp/newer` suggested, Enter would rewrite the
destination, and a second Enter, out of habit, would copy there.

## Considered options

1. **Keep "the first row is chosen, Enter takes it".** Simple and what the
   box did. With values it makes Enter a completion key on almost every
   line, and a reader who types a whole new folder name gets a different,
   existing one.
2. **Nothing is ever chosen; only Tab completes.** Enter always runs. But
   `:tr` Enter then says "unknown verb" instead of becoming `:trash`, which
   the box did before and which costs nothing: a verb cannot be a wrong
   destination.
3. **Choose for the reader only what is syntax, and only while the line
   does not parse.** A line that parses runs on Enter. A line that does not
   gets its first row chosen if that row is a verb or a key; a destination
   or a name is never chosen for the reader. Tab takes the chosen row, or
   the first. An arrow, Home/End or a click chooses; a hover does not.

## Decision outcome

Option 3, in `norte-frontend::goto` (`Goto::accept`, `choose`, `point`), so
both frontends share it.

- What a suggestion types is built by the grammar's printer from bytes the
  reader did not type (`escape_name`, `escape_dest`, `name_term`), and the
  line is split with the grammar's own lexer (`norte_orders::spans`).
- A name is offered as a bare glob only when nothing else in the listing
  folds to it and it has no glob character or `\`; otherwise as `name=`,
  which compares bytes. `name=` still pairs NFC twins (ADR 0185).
- Folders are this pane's own (never the `..` row, never in a results tab)
  and the known places on the same backend, as absolute paths.

### Positive consequences

- Enter on a line that already says something does what it says.
- `:cp` Enter Enter no longer copies to whatever came first; it says the
  destination is missing.

### Negative consequences

- `:sel ex` Enter now runs `sel ex` (a valid glob) instead of becoming
  `sel ext:`; Tab does that. The live preview shows it matched nothing.
- A legacy-encoded name is offered in its escaped form (`\x8f…`) and ranked
  by it, not by what the pane paints.
