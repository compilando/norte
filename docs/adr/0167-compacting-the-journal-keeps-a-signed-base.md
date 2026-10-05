# 0167 — Compacting the journal keeps a signed base

- Status: accepted
- Date: 2026-10-05
- Decision makers: Oscar González
- Related: ADR 0023 (hash chain), ADR 0025 (HMAC anchors), ADR 0046
  (format marker), #146 (marker anchors), #396

## Context

The journal is one row per mutated file, forever. #411 gave the undo and
timeline queries indexes, so a large journal is no longer slow to query;
it is still a file that only grows, and `verify_chain` and `audit export`
walk all of it.

Deleting old rows is not free here. The journal is a hash chain
(`prev_hash[i] == entry_hash[i-1]`) whose point is that a missing row is
VISIBLE: deleting a prefix makes the first surviving row link to a hash
that is no longer there, and `verify_chain` reports `Broken`. Head anchors
(ADR 0025) at a deleted `seq` report `MissingSeq` — "truncation". A
compaction that does nothing else turns into a false accusation of
tampering, and anything that silences that accusation silences it for an
attacker too.

## Decision

**Compaction is explicit and offline: `norte journal compact --before
<date>`.** No automatic retention: what is dropped is the user's audit
trail, and the user decides when. The daemon must not be running (the
journal's exclusive lock makes that a plain "database is locked").

**What it drops: the longest PREFIX of mutations older than the date**
(`ts_ms` before `<date>` 00:00 UTC), with three limits:

1. never the chain's head — at least one mutation survives, so head
   anchors and the writer's in-memory chain state stay valid;
2. never half a batch — batches interleave and a group undo reuses its
   batch id much later, so the cut moves back until no batch has rows on
   both sides of it;
3. never on a journal that does not pass `audit verify` — chain, base
   signature, head anchors and marker anchors, with an anchors file
   required — and never past what the anchors cover: every dropped row
   must sit at or below an anchor that verifies NOW. An intact chain is not
   enough: it is keyless, and once rows are dropped an anchor below the
   base is never compared again, so a signature minted over a forged base,
   a rewrite, or a trimmed anchors file would make it pass for good.
   `audit anchor` refuses an unsigned base for the same reason.

   What this cannot close is ADR 0025's own limit: an attacker who rewrites,
   trims the local anchors that would accuse it, and waits for the user to
   anchor the forged head. Only the external copy of the anchors catches
   that, and **its power ends at the base**: check it with `audit verify`
   before compacting.

The format marker (`seq 0`) is never dropped.

**What it keeps: a base.** A one-row table `journal_base` records
`(through_seq, through_hash)` — the last dropped row and its digest — and
`verify_chain` starts the link check there: the first surviving row must
be `through_seq + 1` and carry `through_hash` as `prev_hash`.

**The base is signed, in its own file.** On its own the base is keyless, so
a truncation could fake it. Compacting therefore needs the anchor key and
appends `HMAC(key, "norte-compact-v1" ‖ through_seq ‖ through_hash ‖
marker)` to `journal-compactions.jsonl`. The marker's digest is in it
because after a compaction no row links to the marker any more: deleting
or replacing it would otherwise go unnoticed. `audit verify` fails unless the current base
matches a valid line there. Its own context string and its own file,
for the reasons #146 gave the marker: a head anchor at the same `seq` must
not vouch for a base, and an older binary must not read these lines as
broken head anchors.

**The line is written BEFORE the rows are deleted**, synced, and so is
its directory when the file is new. A crash between the two leaves a signed
compaction that never happened, and never a base without a signature,
which `verify` would report as tampering forever. That stray line is not
quite nothing: someone with write access could later perform exactly that
cut and `verify` would accept it. It is a cut the user authorised, so it is
accepted as such.

**Head anchors below the base** verify their MAC and are counted as
compacted, not missing: the signed base is the evidence that their rows
were dropped on purpose. An anchor AT `through_seq` is still checked
against `through_hash`.

## Consequences

- A compacted journal verifies with this binary and later ones. **An older
  binary reports it `Broken` at the first surviving row**: it does not
  know the base. Bumping the format marker would turn that into the gentler
  `UnknownFormat`, but rewriting the marker changes its digest and every
  marker anchor (#146) would then accuse it. Compaction is an explicit
  command; the CHANGELOG says not to go back to an older binary after
  running it.
- Rows compacted away can no longer be undone, and their trash items are
  not purged: the trash keeps its own lifecycle.
- `VACUUM` runs after the delete, so the file actually shrinks; if it
  fails, the compaction still happened and the command says so.
- The batch counter restarts from the surviving rows, so an id whose rows
  were all compacted can be handed out again. Cosmetic: ids only group rows
  that coexist.
- Not covered: a size- or age-based automatic policy. If one is wanted
  later, it is this command run by a schedule, not a different mechanism.
