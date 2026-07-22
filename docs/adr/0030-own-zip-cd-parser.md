# Own zip central-directory parser (supersedes the `zip` crate dependency)

- Status: accepted
- Date: 2026-07-22
- Issues: #59
- Supersedes: the "prefer the `zip` crate over hand-written parsers" clause of ADR 0018.

## Context and problem statement

ADR 0018 chose the `zip` crate over a hand-written parser as the safer option.
Field experience reversed that assessment: the crate indexes the central
directory by the DECODED name (bit 11 → lossy / cp437) in an IndexMap, so two
distinct raw names that decode equally collapse silently before norte sees
them (H1, encoding audit 8e); a valid Info-ZIP 0x7075 extra field silently
substitutes `name_raw()`, and a malformed one aborts the whole archive (H3).
These break hard rule 1 (names are bytes) below our boundary and upstream has
not moved.

## Decision

`norte-vfs-archive` parses the zip central directory itself (`zip_cd.rs`) and
reads entries via local headers with flate2; the `zip` dependency is removed.

Safety argument replacing "battle-tested crate":

- **Raw bytes end to end.** Names go from CD to index verbatim; kind by raw
  trailing `/`; 0x7075 is ignored by design; a malformed extra skips only the
  entry.
- **Streaming, bounded.** The CD is never materialized (parse under `Take`);
  per-entry allocations bounded by u16 fields; EOCD window ≤ 64 KiB + 22;
  cancel per entry; count limits enforced before paying the CD (zip64
  included — closes the old u16 preflight gap).
- **Fail-loud reads.** Stored entries must satisfy `comp == uncomp`
  (APPNOTE); short reads are `Corrupt`, never silent; CRC verified on full
  reads (mismatch = final `Err(Corrupt)`); ranged reads are CRC-unverified
  (documented — verifying would require decoding the whole entry). Ranged
  deflate discard shares the forward-decode semaphore and aborts when the
  consumer drops (regla 3).
- **Adversarial validation.** Hostile corpus (lossy-collision pair, 0x7075
  forgeries, zip64 forgeries, lying EOCD/CRC/sizes, prepended data) plus the
  existing proptest fuzz run against the parser; the encoding audit ran
  mutation testing over the acceptance logic.

Deliberate strictness (documented, pinned by tests):

- Archives with prepended data (self-extractors) are rejected: acceptance
  requires exact EOCD self-consistency, which is what makes fake-signature
  rejection sound (H9). Info-ZIP "offset fudge" support is future work if
  real demand appears.
- zip64 extras are read Go-style strict: values only for marked fields, in
  APPNOTE order; the first 0x0001 record wins; a marked field with no value
  is hostile (entry skipped).

## Consequences

- The dependency tree loses `zip`, `zopfli`, `derive_arbitrary`; no new deps
  (flate2 was already direct; `flate2::Crc` covers CRC).
- `Limits::max_cd_bytes` is obsolete (nothing is retained); kept deprecated
  for API compatibility.
- Maintenance of a security-sensitive parser moves in-tree: changes to
  `zip_cd.rs` should re-run the encoding audit with mutation testing.
