# 0080 - A digest is a read that nobody can take back

- Status: accepted
- Date: 2026-08-29
- Decision makers: Oscar González
- Related: ADR 0009 (long operations are Tasks), ADR 0011 (daemon), ADR 0042
  (a reviewable batch plan), ADR 0071 (`expected_digest`), issue #311.

## Context and problem statement

Checking a file against the sum somebody published is the only way to know that
what arrived is what was offered. A copy that ends without an error says the
bytes went out and came in; it does not say they are the same bytes. Krusader
puts checksums in its File menu, Total Commander has them, and norte had
nothing: no method, no command, no surface.

The obvious shape — "hash this file, give me the string" — is wrong on three
counts here, and each one is a decision that outlives the feature.

## Decision

**Add `fs.checksum` and `fs.checksum_report`, protocol 0.59.0.** The first
computes the sha256 of the CONTENT of a batch of paths as a cancellable Task;
the second hands back the digests.

### It is a read, and it is gated by the NARROW door

Hashing writes nothing: no journal entry, no undo, not a byte. Hard rule 4 asks
for a journal entry per *mutation*, and this is not one — classifying it as
`Irreversible` would be a lie in the other direction.

What it does need is a gate, and the first version took the read gate, the same
one `fs.dir_size` uses. That was wrong, and `protocol-guardian` caught it before
0.59.0 shipped. Counting a tree tells an out-of-scope actor how big something
is. A digest is a *fingerprint of the content*: with it, an actor that cannot
read a file can still confirm that the file is the one it suspects — a document
it already holds, a key, a photo.

There is already a narrow door for exactly that, `content_gate`, and
`fs.compare` with the hash rung goes through it. `fs.checksum` **subsumes that
oracle and exceeds it**: compare answers "are these two the same?" and makes the
caller PLACE the candidate somewhere first; checksum hands back the sha256,
which is then matched offline against a dictionary with nothing placed at all.
An agent denied `fs.compare` with `criteria.hash` only had to call here.

So both gates apply, per path. The honest footnote is that the delta against the
status quo is zero — `fs.read` is gated by the read door alone, and with the
bytes anyone computes the digest themselves; `covers_content`'s own rustdoc
calls that "THE INCONSISTENCY, said out loud". The rule that settles it is
written in `policy.rs`: prefer the narrow door in what is NEW, because loosening
it later is additive and tightening it is not.

The cap is checked BEFORE the gates, and that leaks nothing: it is a public
constant and the caller knows how many paths it sent. The other order cost a
lock on the scope registry per path — up to a million of them — before anyone
looked at the cap.

### A partial report says so, and nobody reads it as a verdict

`pending` is how many paths are still unresolved. It reaches zero when the task
completes — and it does NOT reach zero on a task that was cancelled or failed.
That is deliberate, and the first version of the rustdoc got it wrong by
promising "zero once the task is terminal", which would have had a client
polling forever.

The frontend consequence is the one that matters. A cancelled batch leaves a
report with half the entries, and comparing that against a sums file produced
"N do not match or are missing" about files nobody ever opened — an accusation
manufactured by the tool whose only job is to check. So the surface requires
BOTH: the task `Completed` and `pending == 0`. Anything else is reported as
partial and compared against nothing.

The report also carries `algo`. It is not only in the params because the report
can be fetched without having sent the request — `task.list` shows other
people's tasks — and a reader that assumed sha256 by omission would paint
digests of something else the day a second algorithm exists.

### The digests come back in a second method, not in the Task

N digests fit neither in a Task's outcome nor in its progress, which only
counts. So the result lives in a bounded ring in the engine, keyed by task id
and owning actor, and `fs.checksum_report` retrieves it — the same split, for
the same reason, as `fs.rename_batch_report` (ADR 0042) and
`archive.pack_report`.

A report that is gone is `NotFound`, and so is a task that never existed and
one of another kind. The three answers are deliberately indistinguishable: the
difference is exactly the information an actor probing for someone else's task
would want.

### Above the cap it is REJECTED, never truncated

`FS_CHECKSUM_MAX_PATHS` is 4096, and a bigger batch is a params error. A
silently truncated report reads as "all checked" over files nobody looked at,
and checking is the entire reason the method exists. The same rule already
governs `fs.rename_batch`.

By the same logic a file that cannot be read does not kill the batch — it comes
back with its REASON and the rest are computed — and a directory is FLAGGED
rather than walked. Hashing a tree is a different question, with its own format
and its own order; answering it halfway would produce a number that checks
nothing.

### A sums file speaks about what sits beside it

On the frontend side, verifying resolves each name in the sums file against the
directory of **that file**, not against the panel's. A `SHA256SUMS` names its
siblings. Resolving against the pane would check different files that happen to
share a name and report "ok" about them, which is the one wrong answer this
feature must never give.

The names in a sums file are bytes from outside (hard rule 1): they are parsed
as bytes, matched as bytes, written back as bytes, and painted through the same
hostile-name masking as any other name. Going through `String` in either
direction is a silent false result — a lossy name checks a file that is not
there, and two different names that collapse to the same replacement character
become one line checked twice.

### What was not understood is counted, never swallowed

A sums file is text from outside, so a line that does not fit is skipped rather
than killing the file. But skipping is only safe if it is *counted*: "37
checked, all ok" over a 40-line file is a false green, and the three that fell
out are the ones with the strange names — the ones an attacker would control.
So the parser returns how many lines looked like sums and were not understood,
and any number above zero forbids saying "all ok".

Three of those lines were not the reader's fault but ours, and are now read
properly: coreutils' own `\`-escaped form for names containing a backslash or a
newline, the BSD/`--tag` form, and a leading UTF-8 BOM. A UTF-16 file (what
PowerShell writes by default) is named as such instead of "this does not look
like a sums file".

Reading the sums file is capped, and above the cap it is REFUSED rather than
truncated — same rule as the batch cap, and for a sharper reason: the cut lands
on an arbitrary byte, so a truncated last line checks a *prefix* of a name and
reports "missing" about a file that is there.

### The parser is shared, the surface is not

`norte_frontend::checksums` — parse, verify, render back to `sha256sum` format
— sits in the shared frontend crate. The terminal has the surface today; the
window inherits the logic the day it grows one, and the two cannot drift into
two different ideas of what "ok" means (ADR 0077).

## Consequences

- Protocol **0.59.0**: `FS_CHECKSUM`, `FS_CHECKSUM_REPORT`, `ChecksumAlgo`,
  `ChecksumEntry`, `ChecksumMiss`, `TaskKind::Checksum`. Additive: a 0.58 peer
  does not call the methods and loses the check entirely — there is no partial
  degradation to describe. All it sees of the bump is somebody else's task of
  an unknown kind, which its `serde(other)` already handles.
- Only sha256 exists. `ChecksumAlgo` is an enum rather than a string so that
  adding blake3 later is additive and a typo is not a silent "no algorithm".
- The TUI gains `pane.checksum` and `pane.checksum-verify` at `alt+k`/`alt+K`
  in all seven presets. The chords are CHOSEN, not transcribed: none of the four
  imported managers gives checksums a key, and the preset files say so where a
  reader will look.
- A task whose answer IS its report does not take the tick's generic "done" on
  top of its verdict (`refresh::habla_por_su_informe`). Checksums are the first;
  the rule is written where the next one will need it.
- The window is classified as deferred against #311 in the parity test rather
  than left unclassified, so the gap is a row in a list and not a discovery.
- Four verdicts, not one. "Missing" used to mean four different things — not
  there, not allowed, it is a directory, or this system cannot spell that name
  — and they are fixed in four different ways.
- One thing was reviewed and deliberately NOT done: annotating a "missing" with
  "it exists under another Unicode normalisation" (the macOS NFD trap) or
  another case. `norte-compare`'s `key_for` is the right tool, but the answer
  needs a LISTING of the directory, which verification does not do — it asks
  only about the names the file names. It stays a follow-up on #311; the
  verdict as it stands is true, just less helpful than it could be.
