# 0175 — The disk map tells touching rectangles of one class apart by tone

- Status: accepted
- Date: 2026-10-08
- Decision makers: Oscar González
- Protocol: unchanged. Bridge 105 (`DiskTileView.shade`).
- Related: ADR 0117 (the disk map), 0172 (the window's chrome), the
  treemap of 2026-10-08 (`treemap::tiles`)

## Context and problem statement

The landing shots open the disk map on a home directory. A home is all
folders, so every rectangle got the folder colour, and with only a thin
gap between them the map read as one blue block: the size of each folder,
which is the whole point, was hard to see.

The map colours by class (`ChildClass`: folder, code, archive, image,
media, document, other), and for a child that is a folder the class is
"folder" and nothing more.

## The study

**Colouring a folder by what it holds** — Photos as images, projects as
code — is the more informative option, and it is what some disk analysers
do. It costs:

- a wire change: `DirUsageChild` carries `kind`, `bytes` and `entries`
  only. It would need the bytes per class under each child, or at least
  the dominant class. That is a proto bump, golden fixtures and a review
  of the wire.
- the classification in the core: today `class_of` lives in
  `norte-frontend` and classifies by name. The core walking the tree would
  have to classify every file it counts, so that table would move to a
  crate both can use.
- a judgement it can get wrong: most folders are mixed. "Dominant by
  bytes" paints a code project blue-grey because of one video in it, and
  a reader then trusts the colour for a claim it does not make.

**Tones of the class colour** fix what the shots showed — telling
rectangles apart — with no wire change and no new claim about content.

## Decision

1. Tones. `treemap::tiles` gives each rectangle a `shade` in `0..3`,
   largest first, greedily: the lowest tone that no TOUCHING rectangle of
   the same class already has. Two neighbouring folders get different
   tones whenever three allow it; a rectangle whose same-class neighbours
   already use all three takes the base one. Rectangles of different
   classes need no tone of their own, because their colours already
   differ.
2. Both frontends paint it from that one field. The window mixes the
   class colour into the panel at 45 % for tone 0, 30 % for tone 1 and
   62 % for tone 2 (bridge 105). The terminal, which fills rectangles with
   the reversed colour, darkens it by 30 % for tone 1 and lightens it by
   25 % for tone 2. A palette colour, on a terminal without true colour,
   cannot be mixed: tone 1 is the dimmed fill there.
3. Colouring by content is not taken now, for the costs above. If it is
   ever done, it is a protocol ADR of its own and keeps the tones for
   ties.

## Consequences

- A map of one class still reads as one class (the legend is unchanged)
  with its rectangles separated.
- On a 16- or 256-colour terminal there are two tones, the colour and its
  dimmed fill, where the terminal honours dim under reverse.
- The terminal computes the tones on every draw: `tiles` is quadratic in
  the rectangles shown, which a panel bounds to a few hundred in practice.
