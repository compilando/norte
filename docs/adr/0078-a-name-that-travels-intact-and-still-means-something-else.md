# 0078 - A name that travels intact and still means something else

- Status: accepted
- Date: 2026-08-26
- Decision makers: Oscar González
- Related: ADR 0060 (writing an archive is not writing into one), ADR 0051/0054
  (the fold key), ADR 0005 (the direction not to take), issue #250.

## Context and problem statement

`archive.pack` already refuses one class of hostile name pair. Two entries whose
fold keys are equal — `café` in NFD and in NFC, `Makefile` and `makefile`,
`straße` and `strasse` — are rejected with `Conflict { Exists }` before a byte is
written, folding with the *widest* mode on purpose: an archive's destination is
unknown by definition, so the question is not "do these collide here" but "do
they collide anywhere". Extracted on the machine where they do, **one of the two
files disappears**.

That leaves a second class, which #250 listed and nothing checked:

| stored name | what it becomes elsewhere |
| --- | --- |
| `a\b.txt` | a file `b.txt` inside a folder `a`, in 7-Zip and Explorer |
| `f:ads` | an NTFS alternate data stream on `f` |
| `CON`, `NUL`, `COM1` | cannot be extracted on Windows at all |
| `nombre.`, `otro ` | Windows silently eats the trailing dot or space |

Our own reader round-trips all of them exactly — `zip_format.rs` treats `\` as an
ordinary byte — which is precisely why `write_roundtrip.rs` cannot see any of it.
The interop test added for #250 shells out to `unzip -t` and `tar -tvf`, and
those do not see it either: the archive *is* well-formed. Nothing is corrupt.
The names simply mean something different on the other side.

So: is this the same problem as the fold collisions, and does it get the same
answer?

## Considered options

### 1. Refuse, like the fold collisions

Symmetrical and simple to explain, and wrong. The two cases differ in the thing
that matters: **a fold collision loses a file, and this does not.** `a\b.txt`
extracted on Linux is still `a\b.txt`; on Windows it is a file in a folder. Both
files exist, in both places. What changed is where they sit.

Refusing would mean norte cannot pack an ordinary Unix tree that happens to
contain a backslash in a name — a legitimate filename on every Unix filesystem —
in order to prevent something that is not a loss. That trade is the wrong way
round, and it is not what ADR 0005 asks for: ADR 0005 is about not taking the
direction where data disappears quietly.

### 2. Warn in the log

Free, and worthless: `tracing::warn!` reaches whoever is reading the daemon's
log, which is nobody at the moment they packed the archive.

### 3. Warn on the wire, in a report the caller asks for

`task.progress` has no channel for it — `unreadable` counts something else, and
a counter that meant two different things depending on the task kind is a
counter nobody can read. What exists already is the shape: `fs.rename_batch_report`
and `archive.test_report`, both of which say the thing that does not fit in a
Task's outcome.

## Decision

**Option 3: `archive.pack_report`, protocol 0.58.0.** Fourth of a family, and
the first whose subject is a Task that succeeded — the other three report what
went wrong; this one reports something true about an archive that was written
correctly, entirely, and is about to be sent somewhere else.

### The vocabulary is open, and one fixture per value

`separator`, `stream`, `reserved`, `trailing` — comparable by equality, growable
(another platform mangles names in ways this set does not have), and each with
its own golden fixture, because a single fixture lets the other three be renamed
with nothing noticing.

### The report has no place for collisions

Deliberately. They cannot occur in an archive that exists: packing fails first.
A list that can never carry anything reads as "there were none", which is a
different and false statement.

The first draft of this ADR had that list, with a `fold` vocabulary
(`unicode`/`case`/`full`) and detection code and tests behind it. The premise was
that rejecting was not an option because `Makefile` and `makefile` coexist on
Linux — written without checking whether the code already rejected. It did, with
its reasoning next to it. The dead half was removed before commit; it is recorded
here because the mistake was not the code, it was arguing from the issue instead
of from the source.

### The report is computed before the first byte

Over the enumerated entry list, right after the walk and before the sink opens.
So it is already final while the archive is still being written, and what it
says stays true of an archive that was cancelled **during writing**: the names
that mean something else do so whether all of them made it in or only the first
few. (Cancelled during *enumeration* is different — the ring entry exists from
submit time and holds a default report, which reads as "nothing found". Nothing
renders it, because of the rule below.)

**Only a pack that COMPLETED is reported to the reader.** Cancelling leaves the
destination clean, so there is no archive to warn about, and "packed, but…" over
something nobody packed is a sentence whose first word is false. The window had
this bug and the guardian caught it — which would also have made the two
frontends disagree, which is what ADR 0077 exists to stop.

### Empty is an assertion, and `checked` is what makes it one

`entries` travels even when nothing was found, so an empty report says "twelve
entries were checked" rather than saying nothing. That is not enough on its own:
`<`, `>`, `"`, `|`, `?` and `*` are illegal on Windows too and are **not** among
the classes looked for, so an empty report with no further qualification would
be claiming the archive travels intact anywhere. It carries `checked` — the same
device, for the same reason, as `ArchiveTestResult::checked`, whose own doctest
says "without saying what was checked, nothing is asserted".

The compatibility story is the one the handshake actually permits, and this ADR
had it wrong at first: a 0.58 client against a 0.57 daemon is not a degraded
scenario, it is a connection that never opens — `version_compatible` refuses a
client from the future outright at `initialize`. The only reachable N-1 pairing
is a **0.57 client against a 0.58 daemon**, which packs the same archive with
the same bytes and simply never asks for the report. The defensive
`Unsupported` handling in the SDK and both frontends stays, but it is
belt-and-braces, not the compatibility story.

## Consequences

- Protocol **0.58.0**: `ARCHIVE_PACK_REPORT`, `ArchivePackReportParams`,
  `ArchivePackReportResult`, `PackRiskyName`. Additive — a 0.57 client does not
  call the method. What it loses against a 0.57 daemon is the warning, not the
  archive.
- Both frontends read it, and both say the same sentence: the TUI on the status
  bar when a pack task completes, the window through the same report plumbing
  that already fetches the batch and undo reports. A warning in one frontend
  only would be the divergence ADR 0077 exists to stop.
- `Engine` grows a fourth report ring, with the same bound and the same eviction
  rule as its three siblings: what says nothing is sacrificed before what does.
- A pack cancelled while writing still has its report; it is simply not shown,
  because there is no archive it would be describing.
- `PackRiskyName::path` is percent-encoded over raw bytes and is **not** a
  `VPath` — unlike `ArchiveTestFailure::path`, which is. Two report types, the
  same field name, different meanings; the rustdoc says so explicitly because a
  third-party client has only that to go on, and `norte-proto` exports no public
  decoder for the relative form.
- What is *not* covered stays uncovered and is worth naming: names hostile in
  ways no class on this list catches (`<`, `>`, `"`, `|`, `?`, `*` — hence
  `checked`), and the interop oracle for the risky names themselves — nothing
  here verifies that Windows does what this table says it does, because there is
  no Windows in CI (#261 territory).
