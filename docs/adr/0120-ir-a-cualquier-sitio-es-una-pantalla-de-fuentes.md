# 0120 — "Go anywhere" is one screen of sources, and a source is a trait

- Status: accepted
- Date: 2026-09-18
- Decision makers: Oscar González
- Related: ADR 0077 (the same command means the same thing in both
  frontends), ADR 0089 (the RPC catalogue), ADR 0114 (navigation history,
  phase 1 of this programme), spec
  `docs/superpowers/specs/2026-09-15-historia-y-wow-design.md` (phase 6),
  memory `historia-navegacion-0114`, `posicion-no-nombra-una-fila`

## Context and problem statement

Six lists already answered "where do I want to be": this panel's history
(`pane.history`), the places you return to (`pane.popular`), bookmarks
(`pane.hotlist`), connections (`pane.connect`), the command palette
(`app.palette`) and — for anyone who has built one — the semantic index. Each
has its own key and its own screen, and each is the right screen when you
know which one holds your answer.

The phase-6 entry in the spec is one line: a reader who does *not* know which
of the six has it has to open them one at a time. That is not a missing
feature in any of them; it is a missing screen over all of them.

## Decision

**1. `app.goto` does not replace any of the six.** Every list keeps its key,
its screen and the things only its screen can do (delete an entry, clear the
history). The new screen is the one for when you cannot remember where you
saw it. Replacing them would have traded six good screens for one crowded
one.

**2. A source is a trait (`norte_frontend::goto::GotoSource`), and the
model owns everything else.** A source answers one question — what rows do
you contribute for this query — and the model owns the section order, the
subsequence filter, the headers, the per-section cap and the cursor. The
alternative, each source filtering and sorting itself, means the seventh
source gets one of those five wrong, and the wrong one is invisible: a
section that quietly filters differently just looks like it has fewer
results.

**3. Section order is fixed and not configurable** (`goto::ORDEN`): what you
just typed, where you have been, where you go often, what you saved, what you
can connect to, what you can do, and last what the index found. A list that
reorders itself is a list where you cannot learn where anything is.

**4. The rows are a SNAPSHOT taken when the screen opens**, as the palette
already does with its own, except the typed path (which *is* the query) and
the index (which arrives when the core answers). A list that changes under
the cursor while it is being read is how Enter lands somewhere nobody chose.

**5. The index is asynchronous and lands through
`Goto::reemplazar_seccion`, which does not move the cursor.** It is asked
from three characters on — one or two letters cannot be a semantic query, and
each question costs a provider call — replaces rather than accumulates (two
answers to different queries describe neither), is abandoned when the screen
closes or a row is confirmed, and is ignored when the answer comes back for a
query that is no longer typed. Its section is last for the same reason: a
section that appears mid-typing must not push down what the reader is already
looking at. It passes through the same `validate_semantic_hits` belt as
`ai.search`, because the cap on a daemon's answer is the client's to enforce.

**6. Its own in-flight slot, beside the existing `semantic` one.** The two
ask the same question and do different things with the answer — one opens a
modal, one fills a section of an open screen — and they can be alive at the
same time. Sharing a slot would have one abort the other with nobody having
asked for that.

**7. The typed path is the first section, and only three shapes count**
(`/…`, `~`/`~/…`, `scheme://…`). A relative path deliberately does not:
"where am I going" cannot depend on which panel you were in, or the same
keystroke leads to two places. `~` expands against the process HOME for the
same reason. An unknown scheme DOES parse and is navigated — `VPath` has no
list of backends and must not grow one; the core says what it cannot serve,
exactly as it does for any other URL.

**8. Only the focused panel's HISTORY carries that panel's name
reinterpretation; every other section is built with `enc = None`.** The rule
already exists in `popular_rows`' own comment — the popular list is the whole
session's, and a panel's reinterpretation applied to another panel's paths
invents mojibake — and it applies identically to bookmarks, connections and
index hits. This is why the row builder takes the encoding as a parameter
instead of reading it off `App`: a function that fetches it itself would
apply it to all six.

**9. A connection with an unparseable URL is still offered, and complains on
Enter** — the same behaviour, and the same message, as the `pane.connect`
picker it shares data with. "My connection is missing from go-anywhere" is
worse than an error on Enter, because it gives the reader nowhere to look. A
bookmark whose target does not parse is dropped instead, because the places
sidebar already shows it with its error.

**10. With nothing typed, the commands section is not shown**
(`GotoSource::solo_con_consulta`). Hundreds of verbs bury the four
destination lists above them, and a reader who opens this screen without
typing is asking where they can go, not what verbs exist. One character
brings them back, and the whole catalogue is still in the palette.

**11. `ctrl+g` in `orthodox`, `cua` and `vim`; unbound in the four
transcriptions, with the reason in each header.** `ctrl+g` is free in all
seven, so this is not a collision — it is that none of the four managers
being transcribed has a key for a screen that does not exist in them, and
inventing one is what those files exist to avoid. They reach it from the Go
menu, where it sits first, which is preset-independent.

## Consequences

- The window does not have this screen yet. The model, the sections, the
  filter and the typed-path rule are in `norte-frontend` precisely so that
  adding it there is a renderer plus its sources, not a second design — and
  so that the second one cannot quietly disagree with the first about what
  counts as a path (memory `funcion-compartida-no-basta` is about the
  opposite case, where sharing the wiring did not share the rule; here the
  rule itself is what is shared).
- A seventh source is `impl GotoSource` plus one line in `ORDEN` and its
  Fluent title. Nothing else.
- The per-section cap (12) makes the screen a shortcut and not a browser.
  Someone who wants to page through a whole history still has
  `pane.history`, which is the screen that can also delete from it.
