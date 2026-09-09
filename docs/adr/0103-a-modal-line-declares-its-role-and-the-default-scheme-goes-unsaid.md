# 0103 — A modal line declares its ROLE, and the default scheme goes unsaid

- Status: accepted
- Date: 2026-09-09
- Decision makers: Oscar González
- Related: ADR 0001 (`display_lossy` is deliberately non-parseable), ADR 0020
  (a theme with partial data degrades), ADR 0070 (a mutation names its own
  operands), ADR 0077 (a decision taken once, in `norte-frontend`), spec §6
  (nothing is altered silently)

## Context

A report from real use: the copy dialog is hard to read. Six defects in five
lines — the editable field looked like every other line, its label sat BELOW
it, the file name appeared twice, `⟨file⟩` ate eight columns on every path,
the key hints weighed as much as the data, and the destination was marked with
an in-band `→`.

Only the last one is a bug in the usual sense. The rest come from one place: a
modal's body was a single `String`, and `draw_modal` painted it as a flat
`Paragraph`. **There was no way to paint any part of a modal differently from
any other part** — not in this one, not in the other 25.

The comparison that settled it: the graphical host already models this with
FIELDS. `DialogView` carries `destination`, `subject`, `asker`, `body`,
`input`, `input_hostile`, `dest_check` and `choices`, and the window paints
them as such — the destination in its own region with a rule under it, the
field as an `<input>` with a border. The terminal flattened all of that into
text. That is a divergence of the kind ADR 0077 exists to prevent, with the
TUI on the poor side of it.

## Options

### A — Reorder the text of this one modal

- Advantage: an hour, no risk, fixes the order, the redundancy and the arrow.
- Drawback: does not fix what makes it hard to read. The field still looks
  like prose; the hints still weigh as much as the data. The next modal
  someone improves starts from zero again.

### B — Redesign this one modal with a framed field

- Advantage: the best-looking result for this dialog alone.
- Drawback: one modal painted unlike the other 25, and the next improvement
  is another special case. It moves the divergence inside the TUI.

### C — Give a modal line a ROLE, and migrate one modal at a time

- `modal_title_body` returns `Vec<ModalLine>`; each line declares a
  `LineKind` — data, label-or-hint, destination, field, warning, error — and
  the theme decides how it is painted.
- A body still composed as a `String` converts to plain lines, so the
  unmigrated modals paint exactly as before. Declaring roles is a change PER
  MODAL, not a precondition for compiling.

## Decision

**C**, plus two things that fell out of it and are decisions in their own
right.

### A line's role lives in the STYLE, because a name cannot forge a style

This is the part that turned out not to be cosmetic. `TransferName` marked its
destination with `→` at the head of the line, and `ConfirmTransfer` still did
the same directly above a list of somebody else's file names. `→` (U+2192) is
legitimate inside a name, is not a terminal hazard, so it is neither masked nor
badged: a file called `docs → /home/BURN` manufactures a line that reads as two
paths. The corpus has said so since `arrow_join_spoof`, and the host's own
rustdoc for `DialogView::destination` argues it at length.

The usual answer — label out of band, in its own column — is better but not
enough on its own: a name can contain the label text too. What a name cannot
produce is the STYLE of a line, because it does not write it. So the
destination is `Strong` and the rows of the list are `Plain`, and that is the
distinction that holds.

Removing `⟨file⟩` (below) made this worse before it made it better: a local
path now paints as `/srv/publico`, and slash homoglyphs (U+2215, U+2044,
U+FF0F) are legal on ext4, APFS and NTFS. `→ ∕srv∕publico` is a legal file
name that renders a complete, credible destination line. Both fixtures are now
in the corpus.

### `file` without an authority goes unsaid

It is the default case — this machine, this disk — so the label distinguished
nothing, on every path of every listing, header and modal. Every other scheme
still says so, and a `file` WITH an authority does too: that one is another
machine.

**This means `path_display` no longer traces `VPath::display_lossy`, and that
is deliberate.** ADR 0001 records the `⟨scheme authority⟩/` form as
deliberately non-parseable, and it still holds where it was written: for a
LOG and for an error, where no screen says which machine is meant.
`path_display` is the form of a row on a screen, which is a different job. The
two still agree for every scheme that is announced.

What remains as the role marker for a local path is the leading `/`, and it is
a real one: a segment cannot contain `/` on any supported OS. It is thinner
than a label, which is why the two tests that pin the marker now run over
`file` as well — they were written to catch exactly this change and they did
not, because both only exercised `mem`.

## Consequences

### Positive

- Every modal can have a hierarchy, and the ones that need it most get it
  first. The remaining 24 paint identically until someone migrates them.
- Two spoofs that the corpus described and the code allowed are closed, and
  closed by a mechanism a file name cannot reach.
- `modal_height` finally has a test tying it to the body it is supposed to
  cover. The module's rustdoc had warned from the start that the two halves
  drift and the modal gets clipped; nothing checked it.
- `tail_window` now budgets in CELLS. It counted chars, so fifty chars of CJK
  were a hundred cells: a Japanese name overflowed its box and the overflow
  ate the cursor. That bug was in all six free-text fields, not just the new
  one.
- One duplicated rule dies: `mount_name` carried its own copy of "do not
  announce the local scheme", and the copy had already drifted (it used
  `display_name` where `path_display_with` uses `display_name_with`, so under
  a reinterpretation the two painted the same mount differently).

### Negative

- Two sources of truth for a modal's geometry still exist: `modal_height`
  declares the height and the composer decides the body. The new test ties
  them for the migrated modals only; the other 24 keep the old arrangement.
- A local path is now spelled the way the OS spells it, and that is exactly
  the form `TransferDest` refuses on purpose — it reads wire form, because
  guessing a scheme is how a copy lands on a backend the reader did not mean.
  So the product displays a spelling its own entry point rejects. It did
  before too (`⟨file⟩/casa` was not typeable either), but now the spelling
  looks acceptable and is not. **Left open deliberately**: making
  `TransferDest` accept a leading `/` as `file://` is a separate decision
  about a mutation's entry point, not a display change.
- Three spellings of the same path now coexist: the frontend's
  (`/home/o/x`), the semantic index's stored `path_display`
  (`⟨file⟩/home/o/x`, written by `norte-index`) and the approval modal's wire
  form (`file:///home/o/x`). Two of those are decision or record surfaces.
  Worth unifying; not unified here.
- `LineKind::Dim` uses `Role::Info` rather than `Role::BorderUnfocused`. The
  border role looked like the natural fit and is not: it is meant for CHROME,
  the light themes make it pale and it carries `dim`, so measured against its
  own theme's background it gives 2.30:1 on `catppuccin-latte` and 2.45:1 on
  `gruvbox-light` where WCAG AA asks 4.5 for text. `Info` gives 4.34 and 5.82.
  The contrast gate in `norte-theme` still covers only the three semantic
  signals, so this one is measured by hand and written down here.
