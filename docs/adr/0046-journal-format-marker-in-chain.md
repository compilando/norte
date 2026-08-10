# 0046 - The journal declares its format inside the hash chain

- Status: accepted
- Date: 2026-08-10
- Decision makers: Oscar González
- Related: ADR 0023 (SQLite journal and its hash chain), ADR 0025 (HMAC
  journal-head anchors and audit export), ADR 0042 (batch rename wire, which
  added `batch_id` to the chain); hard rule 4 (every mutation goes through the
  journal); spec §4 and §10; issues #127, #63 and #146 (anchoring the marker).

## Context

ADR 0042 added `batch_id` to the journal's rows and therefore to the preimage
of `chain_hash`. The forward direction was handled with care: `None` feeds
nothing at all, not even a presence byte, so every entry written before batches
existed hashes exactly as it did then. That is pinned by a frozen digest vector
and by a test that builds a real on-disk database with the old schema and chains
onto it.

The reverse direction was not handled, and could not be. A binary older than
that migration, opening a journal that already holds batched entries,
recomputes their hashes without the field, finds different digests, and reports
`ChainStatus::Broken`. The file is intact. Nobody touched it. The user is told
their audit trail was tampered with.

That is worse than a cosmetic bug. `Broken` is the single signal the journal
exists to produce, and a user who has seen it fire on a healthy file — after a
downgrade, a rollback of a release, a second machine on an older version — has
learned to dismiss it. The next time it fires it will mean something.

So the journal needs to say which format wrote it, in a way that satisfies three
constraints at once:

1. **Authenticated.** A format marker that anyone can flip is a way to make a
   valid journal unreadable, and the tamper evidence would never see it. A
   `PRAGMA user_version` stamp was in fact written and reverted during the batch
   work for exactly this reason.
2. **Readable by a binary that does not know the format it announces.** This is
   circular unless it is designed against: the whole point of the marker is to
   be understood by a build that cannot understand the entries after it.
3. **Optional.** Journals already on disk have no marker and must stay valid.
   Their absence is information — "written before markers existed" — not a
   fault.

## Options considered

### `PRAGMA user_version` in the SQLite header

Cheap, standard, and one line of SQL. It is also outside the chain and outside
what ADR 0025's anchors sign, which makes it the wrong shape twice over.

A four-byte write no hash can see would stop the audit from opening the journal
at all, and would word the refusal as *"this journal is newer than your norte,
upgrade"*. An attacker gets a denial of service against the audit that the
tamper evidence is structurally unable to report, and gets it framed as the
victim's own fault for running an old build. Rejected; the earlier attempt was
already reverted on these grounds.

### A field in the anchor file

Signed, so constraint 1 holds, and ADR 0025 already defines the file's line
format. But an anchor is written by `norte audit anchor`, on purpose, by a human
who has set up a keyring key. The journal of a user who has configured nothing
has no anchor file — and that user is precisely the one who will hit the false
accusation, because they have no second signal to weigh it against. A marker
that is absent for everyone who needs it most is not a marker. Rejected.

### An entry inside the chain, adjacent to the genesis

The journal already has a structure that is authenticated, mandatory, and
present from the first byte: the chain itself. A row at the reserved `seq 0`,
written when the journal is created, carrying the format version, hashed and
chained like any other row.

Altering it breaks the chain, exactly like altering a mutation, because it *is*
an entry. Any anchor over a later head covers it transitively, because every
subsequent digest depends on it. It exists on every journal from creation,
whether or not the user ever configured anything.

Its one hard part is constraint 2, and it is met by freezing the marker's
preimage — see the decision below.

### Accept the false `Broken` and word it better

`verify_chain` could try to detect "probably a newer format" heuristically —
for instance by noticing that the digests fail from the very first entry. It
cannot tell that case from a rewritten chain, so the wording would be a guess,
and a guess that exculpates is worse than one that accuses. Rejected.

## Decision

**The journal's format version is an entry inside the hash chain, adjacent to
the genesis.**

### 1. The marker row

At `seq 0`, written by `Journal::open` when — and only when — the table has no
rows at all:

| column | value |
| --- | --- |
| `seq` | `0`, reserved for journal metadata |
| `op` | `journal_format` |
| `actor_kind` | `system` |
| `path` | the version as ASCII decimal (`1` today) |
| `reversal` | `irreversible` |
| everything else | `NULL` |

`seq 0` is the discriminator, and it is free: `record_entry` assigns
`last_seq + 1` starting from an initial 0, so a mutation is never row 0. The
version reuses the `path` column rather than adding one, because a new column
would have to be migrated onto journals that already exist and then fed to the
chain for every row — the cost the marker is here to avoid, paid to introduce
it.

`actor_kind = system` is outside `Actor`'s vocabulary, so no actor can claim it
and no query filtered by actor can return it.

### 2. The marker is metadata, not history

`entries`, `revertible_for`, `count` and `head` filter `seq >= 1`. The marker is
part of the chain and not part of what happened: an audit export must not gain a
row describing no mutation, the undo's LIFO stack must not gain a step it cannot
take, and the anchor must not point at a `seq` the export does not contain.

`verify_chain` walks everything, marker included. `Intact { entries }` counts
mutations, so the number a user sees does not move because of this ADR.

### 3. The marker's preimage is frozen forever

The marker is hashed by the ordinary `chain_hash` over the values above, and
**that preimage may never change**, in this format or any future one.

This is what makes constraint 2 hold in both directions:

- Every field it feeds existed in format 1, and `batch_id: None` feeds nothing,
  so a binary built before the marker existed — including one built before
  `batch_id` — recomputes this row's digest byte for byte and reports it
  intact. Introducing the marker therefore makes no downgrade worse than it
  already is.
- A binary that meets a journal from the future can verify the row that tells it
  the journal is from the future. Without the freeze, reading the version would
  require knowing the format the version exists to announce.

A future format that adds a field to `chain_hash` adds it for mutations and
keeps it out of the marker. The frozen digest vector in `journal.rs` is the
mechanical guard.

### 4. Three verdicts, and the order of the checks

`verify_chain` gains `ChainStatus::UnknownFormat { declared, known,
first_unverifiable_seq }`, and `ChainStatus` becomes `#[non_exhaustive]` so the
next verdict cannot be silently absorbed into an old arm.

The walk **verifies the marker's own hash before it believes a word of it**:

- marker absent, or declaring a version this build knows → `Intact` or `Broken`,
  exactly as before;
- marker intact and declaring a version above `JOURNAL_FORMAT`, or one this
  build cannot read → `UnknownFormat`, whether or not later digests recompute.
  Certifying a format nobody here can read would be a claim about rules this
  binary does not have;
- marker altered → `Broken` at `seq 0`. Its columns are **compared** against the
  canonical marker record and only then hashed, so only the version is the row's
  to choose: a marker wearing a different `actor_kind` or `reversal` does not
  verify however carefully its hash was refreshed. Comparing rather than
  substituting is load-bearing — a verifier that hashed a substituted canonical
  record would leave the row's real `op` outside the digest while still reading
  it to decide whether the journal is readable, and one column write would make
  a pristine journal report "upgrade norte". A version must also be spelled
  canonically (`1`, never `+1` or `0001`), so one version has exactly one
  preimage.
- a row that does not **decode** → the same as a digest that does not match: it
  is evidence, and it is reported as `Broken` at that `seq` rather than as an
  error. SQLite's typing is dynamic, so a blob in a TEXT column is one write
  away, and answering "the database errored" there would let that one write
  replace a located accusation with a shrug.

A broken **link** — a `prev_hash` that is not the previous `entry_hash` — is
`Broken` even under an unknown format, and the walk keeps checking links *past*
a digest it could not recompute, carrying the stored hash forward for exactly
that purpose. The link needs no preimage, so it is the one property a build can
assert about a journal it cannot read, and it is what keeps an insertion, a
deletion or a reordering visible in a journal from the future. Rows below
`seq 0` are `Broken` too: the mutation readers filter `seq >= 1`, so a negative
`seq` would otherwise be chained and certified while being invisible to
`entries`, `count` and the export. That check is also a **forward constraint,
frozen like the preimage**: the negative `seq` range is forbidden for every
future format, because a build that predates the format reserving it reports
`Broken` on an untouched file — #127, third verse.

`UnknownFormat` is not `is_intact()`. `norte audit anchor` refuses to anchor —
an anchor over a chain this build could not verify would pin unknown history as
good — and `norte audit verify` prints the refusal without accusing anyone,
**then goes on to check the anchors** and exits with failure regardless. That
last part is not politeness: under `UnknownFormat` the anchors are the only
second opinion the operator has, they need no recomputation to check (they are
compared against stored digests), and the file is already on disk.

### 5. The marker is read at open, and does not gate it

`Journal::open` logs loudly when the journal declares a format it does not know,
and opens it anyway. Refusing would turn one column write — the declared version
— into a daemon that will not start, on a file that is otherwise perfectly well
formed. (One write can already stop the open today: a head `entry_hash` that is
not 32 bytes is `Corrupt`. The difference is that a malformed head really is a
file this build cannot safely append to, while an unknown format is a file it
can read perfectly well and merely cannot certify.) What must not happen is the
write that follows; see below. The open-time read does not verify the marker's
hash, which is fine for a log line and is exactly why it must never become a
gate.

### 6. It is fixed forward only, and existing journals stay unmarked

No released binary writes or checks any marker, so #127 cannot be repaired for
the downgrades that exist today. What this buys is that the *next* format change
does not repeat it.

A journal that already holds entries is never stamped. The marker lives ahead of
`seq 1`, and inserting a row there would leave the first mutation chained onto
the genesis hash with a row now in front of it — `verify_chain` would report
`Broken` on a file nobody touched, which is the accusation this ADR exists to
prevent, delivered by the fix. So journals created before this change stay
unmarked for life.

For the same reason a marker can never be *updated*: re-declaring it changes its
digest and breaks every link behind it. A journal's declared format is therefore
fixed at **creation**, not at write time — which puts a condition on the next
bump, recorded here because the code cannot enforce it alone:

> **Bumping `JOURNAL_FORMAT` is not enough.** A build of format N that opens a
> journal declaring M &lt; N must either keep hashing that file with M's rules, or
> append a marker at the point of change ("from `seq k` on, format N").
> Appending N-shaped entries onto an M-declaring journal reproduces #127
> exactly, for every journal in the field at bump time.

## Consequences

Positive:

- A downgrade across the next format change reads as *"this build cannot verify
  this journal"* instead of *"your audit trail was tampered with"*, and the
  signal that matters keeps meaning what it says.
- The marker is covered by the chain, and by every HMAC anchor over a later head
  **for a consistent rewrite** (ADR 0025): re-declaring the format and
  recomputing the entries after it changes every stored digest, and no anchor
  written before the change matches. That is the attack ADR 0025 was written
  for, and it now covers the format declaration too, with no new key material
  and no new file. Two qualifications, because the unqualified sentence is
  false: an *inconsistent* re-declaration is not covered, since it changes no
  stored digest at `seq >= 1` (spelled out below); and the transitive cover is
  realized by recomputing the chain forward from the marker, which is exactly
  what a verifier reporting `UnknownFormat` has just said it cannot do. The
  anchor protects the marker for every verifier except the one that needed it.
- `norte audit verify` and `norte audit anchor` fail closed on anything they
  cannot certify, including verdicts that do not exist yet, because
  `#[non_exhaustive]` forces the wildcard arm to be written and the wildcard
  says "not certified". (`norte audit export` still verifies nothing at all and
  exits successfully; that is older behaviour this ADR does not change.)

Negative, and stated rather than hidden:

- **Journals that exist today never gain a marker.** If the format changes
  again, those journals will produce the same false `Broken` on an older binary
  that #127 describes. Closing that needs a marker appended at the point of
  change — the same mechanism, at the tail instead of at the genesis, declaring
  "entries from `seq k` onward are format N" — which is a bigger design (it
  makes the format a property of a range, not of a file) and is deliberately not
  built here.
- **An attacker who can write the database can re-declare the format, and the
  anchors do not catch that one.** Three column writes, no key: set the version,
  refresh the marker's digest (keyless, publicly computable), relink `seq 1`.
  A verdict that was `Broken { first_bad_seq: k }` becomes `UnknownFormat`, and
  a version of `u32::MAX` guarantees no build will ever say otherwise. The
  anchors do **not** rule it out — the edit leaves every stored digest at
  `seq >= 1` untouched, so each anchor still verifies. This attack falls in the
  gap between the two mechanisms: `verify_chain` catches inconsistent edits,
  anchors catch consistent ones, and here an inconsistent edit had its verdict
  rerouted. What survives is the alarm, not the blame: both verdicts refuse to
  certify and both exit with failure, and the operator is told in the message
  itself that a re-declared marker looks identical from here. The capability is
  not new — the same write access already allowed a full, consistent rewrite,
  which is strictly stronger — but the cost of a *narrative* dropped, and that
  is worth stating. What would close it is anchoring the marker itself: one
  MAC'd line at `seq 0`, after which an inconsistent re-declaration is a
  `HashMismatch` for anyone who has ever anchored. Tracked in #146.
- **A "how many entries failed to recompute" heuristic was considered and
  rejected** as the discriminator between a genuine newer format (where nearly
  every entry fails) and a laundered one (where two do). It breaks on the shape
  the next bump is most likely to have. `batch_id` is the precedent: a field
  that is *sparse and absent-safe*, so an honest older build reading a journal
  full of it fails on the batched entries only — perhaps two of five hundred,
  the forgery profile exactly. A number that is wrong in the likeliest honest
  case is worse than no number.
- `ChainStatus` grew a variant and became `#[non_exhaustive]`: a Rust API break
  for anything outside the workspace that matches on it. `norte-core` is not in
  the `cargo-semver-checks` list, and the two call sites in the tree are in
  `norte-cli`.
- One extra row per journal, and `count()` no longer equals `SELECT COUNT(*)`.
  Anything reading the table by hand must filter `seq >= 1` or explain why not.
  One query deliberately does not: the `last_seq`/`last_hash` read at open, which
  must see the marker or a new journal's first mutation would chain onto the
  genesis hash instead of onto it.
- `verify_chain` no longer returns at the first bad entry; it walks to the end so
  the link checks continue. For a session-sized journal already read with
  `fetch_all`, that is not a cost worth naming, but it is a change.
