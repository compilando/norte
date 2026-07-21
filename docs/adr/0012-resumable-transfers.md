# 0012 - Resumable transfers and `.norte-partial` lifecycle

- Status: accepted
- Date: 2026-07-12
- Decision makers: Oscar González
- Related: specification section 5; ADRs 0004 and 0005

## Context

M1 wrote to uniquely named staging files and atomically committed them. Failure
or cancellation removed the staging file. Remote multi-gigabyte transfers also
need opt-in resume without coupling the engine to POSIX offsets: S3 resumes a
multipart upload rather than appending to a normal file.

## Options considered

- Let the engine seek and append directly. This assumes file semantics and does
  not fit S3 multipart uploads.
- Let a provider open a resumable sink and report the number of durable bytes.
  The engine can then range-read the source independently of the provider's
  staging mechanism.
- Always keep partial files. This changes M1's clean-cancellation contract and
  creates unexpected garbage. An explicit resume option preserves the default.
- Keep process-specific staging names, which cannot be rediscovered, or derive
  a stable name from the exact destination bytes.
- Trust only the partial length, or optionally verify the source and destination
  prefixes by hash.

## Decision

- Add
  `open_resumable(&self, path) -> Result<(Box<dyn ByteSink>, u64), Error>`.
  The provider reports already-durable bytes and returns a sink that continues
  from there. The default opens a normal write and reports zero.
- Add `ByteSink::keep`. It makes staging durable without publishing or deleting
  it. The default implementation aborts, so a provider without resume support
  still degrades to a clean destination.
- Make resume opt-in. `resume=Off` retains M1 behaviour: abort on failure or
  cancellation. `resume=On` keeps staging after cancellation or a transient
  failure.
- Name local staging files `.norte-partial.<sha256-128>`, using 32 hex digits
  derived from the exact final filename bytes. Recognize only that exact form
  during garbage collection so similarly prefixed user files are untouched.
- `LocalProvider::open_resumable` opens the staging file for append, reports its
  length, and `keep` calls `sync_all`. It advertises `APPEND`. Listing and GC
  APIs remove recognized partials older than a threshold.
- When resuming, restart from zero if the partial is longer than the source. In
  `VerifyPolicy::Hash`, also compare SHA-256 of both prefixes and restart on a
  mismatch. `VerifyPolicy::Length` is the inexpensive default.
- Progress begins at the durable byte count. A resumed progress bar does not
  move backward.
- Protocol 0.6.0 adds optional
  `resume: ResumePolicy { Off, On }` and
  `verify: VerifyPolicy { Length, Hash }` fields to copy/move options, preserving
  N-1 defaults.
- S3 can implement the same contract through multipart `ListParts`, leaving the
  upload open on `keep` and completing it on `commit`; the engine remains
  unchanged.

`Provider::partial_digest(path, len)` later implemented hash verification.
Local and SFTP providers support it. Object storage and archives return `None`
and safely fall back to length-only verification.

## Consequences

Resume works across provider-specific staging models without changing default
cancellation behaviour. Concurrent resume-enabled copies to the same
destination are unsupported because they share staging; destination conflict
handling still prevents publication. Abandoned partials remain until age-based
GC or future journal-directed cleanup.

Hash verification rereads the transferred prefix and is therefore opt-in. A
stable name derived from raw bytes may miss an existing partial when a
case-insensitive or normalization-folding filesystem treats a different byte
spelling as the same destination. That failure is safe: it recopies rather than
sharing the wrong staging file.
