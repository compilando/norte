# 0143 — A dialog can carry a form

- Status: accepted
- Date: 2026-09-22
- Decision makers: Oscar González
- Protocol: unchanged. Bridge: 90 → 91.
- Related: ADR 0066 (the host decides, the renderer paints), ADR 0077 (one
  decision, one place), `docs/batida-de-funcionalidades-2026-09-20.md` §4.3

## Context and problem statement

The terminal asks for a search with seven fields and four switches: name,
content, folders to skip, minimum and maximum size, days since change, forced
encoding, plus regex, case, whole word and subfolders, and a cycle for which
kinds count. Every one of them has been on the wire since protocol 0.81.0.

The window asked with **one text box**. Its dialog could not hold more,
because `DialogView` carries a single `input: Option<String>`, and no dialog
in the window has ever had two inputs. So the same question, asked from the
other frontend, silently dropped nine controls — and a filter that is not
applied does not look like a missing feature: it looks like a search that
found more things.

Two separate problems hid behind that, and conflating them is what would have
made this a one-off:

1. **The bridge had no form.** Not for search, not for anything.
2. **The form's model lived in the terminal**, together with its parsers, its
   validation and its mapping to `FsSearchParams`. Copying it into the window
   would have been the divergence ADR 0077 already paid for once: the two
   frontends used to decide how a search ENDED on their own, with different
   precedences, and one said "cancelled" where the other said "there is more".

## Decision

**`DialogView` gains a generic list of fields, and the search form moves to
the shared crate.**

The bridge grows `DialogView.fields: Vec<DialogFieldView>`, where a field is
an id, a Fluent label key, a masked value, a hostile flag and a kind: `text`,
`toggle { on }` or `cycle { value_key }`. Tocarlo travels back as
`dialog_field { id, field, value }`, with `value` one of `text { text }`,
`toggled` or `cycled`.

Four things are deliberate:

- **Generic, not search-shaped.** The next dialog with a form — and the sweep
  lists several — gets it for free. A `SearchDialogView` would have had to be
  rewritten the first time anything else needed two fields.
- **Fields are named by ID, never by position.** A field inserted in the
  middle renumbers everything under it, and what comes back would name another
  one. It is the same rule as the rows of a listing.
- **A toggle and a cycle carry no value.** The renderer says the control was
  TOUCHED; which state it goes to is decided in Rust. Sending the destination
  would let two quick clicks overwrite each other, the second one born from an
  older snapshot.
- **`input` stays.** It is the path of the one-field dialog and of the
  PASSWORD, which deliberately does not travel through the form: a form keeps
  what was typed in the host so it can project it, and that is exactly what a
  secret must not do (#327).

The model moves to `norte_frontend::search`: `SearchForm`, `SearchField` (with
its order and its stable ids), `SearchKinds`, `parse_size`, `parse_days`, the
"which field is unreadable" check, and `params()`, which builds the
`FsSearchParams`. The clock is a PARAMETER of `params()` and not a reading
inside it: "changed in the last seven days" counts from the instant Enter is
pressed, the caller knows that instant, and a mapping that asked the clock
itself could not be tested without waiting.

The terminal keeps its painting and its keys and re-exports the names it used.

## Consequences

**Good.** The window asks the same search as the terminal, filters included,
and the two can no longer drift: there is one model and one mapping. A field
that is unreadable is named before the search is launched in both frontends —
launching while ignoring it returns the whole tree, and that reads exactly
like a result. Any future dialog that needs a form has one.

**Bad.** A dialog can now be two different shapes, and a renderer has to
handle both. The window's form paints every field in one column with no
grouping; the terminal's has a layout the window does not copy. Keeping the
focus and the caret across repaints is the renderer's job — every keystroke
produces a patch that rebuilds the dialog box, so the focused field's id and
caret are captured and restored by hand.

**Left out.** `max_hits` is not in the form (the terminal does not expose it
either), and neither are several roots, following links, or searching inside
archives: those are in the sweep and none of them is a form problem.

## Alternatives considered

- **A search-specific dialog view.** Smaller today, rewritten at the second
  form.
- **Reusing `input` with a separator.** A single string holding seven values
  needs an escape rule, and the values are file names and globs — the place
  where a separator inside the text becomes another value. The same reason
  `destination` is its own field and not the first line of the body.
- **Copying the model into the window.** Explicitly rejected: it is ADR 0077
  again, with the same shape.
