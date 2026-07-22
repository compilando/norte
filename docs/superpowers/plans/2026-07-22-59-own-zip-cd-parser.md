# Own zip central-directory parser (#59) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the `zip` crate in `norte-vfs-archive` with our own raw central-directory parser (index keyed by raw bytes — fixes the H1 lossy-collapse and H3 0x7075 substitution/abort) and local-header-based reads (stored/deflate via flate2), with zip64 covered in the EOCD preflight.

**Architecture:** New `src/zip_cd.rs` owns EOCD/EOCD64 location and streaming CD parsing (never materializes the CD; raw names verbatim; 0x7075 ignored by design; malformed extras skip the ENTRY, never kill the archive). `Locator::Zip` becomes self-contained (`header_offset/method/crc32/comp_size/uncomp_size`) so reads need no retained archive object: `zip_format::read_entry` resolves the data offset from the LOCAL header and streams stored bytes or a `flate2::read::DeflateDecoder`. `CachedContainer.zip` and `open_archive` disappear; the `zip` dependency is dropped from norte-vfs-archive.

**Tech stack:** flate2 (already a dep, zlib-rs), crc32fast (add if not already transitive-direct-usable; tiny, justified in commit), no proto changes.

**Key invariants (do not regress):**
- Regla 1: names are raw bytes end-to-end; kind decided by trailing `b'/'` on RAW bytes.
- #95.4 fail-loud: `Ok(0)` mid-skip or mid-take → `Error::Corrupt`, never silent short data.
- #95.3: limits report `Error::LimitExceeded{limit}` (`LIMIT_ENTRIES`), Corrupt = structural only.
- #58: io errors from the inner provider propagate verbatim via `crate::blocking::inner_proto_error` (keep `corrupt_io`).
- Cancel checked per CD entry (regla 3).
- Encrypted (flag bit 0) or method ∉ {0 stored, 8 deflate} → listable, `locator: None` (read → Unsupported).

---

### Task 1: `zip_cd.rs` — EOCD/EOCD64 + streaming CD parse

Create `crates/norte-vfs-archive/src/zip_cd.rs`; register `mod zip_cd;` in lib.rs (private).

```rust
pub(crate) struct Eocd { pub count: u64, pub cd_offset: u64, pub cd_size: u64 }

pub(crate) struct CdEntry {
    pub name_raw: Vec<u8>,
    pub flags: u16,
    pub method: u16,
    pub crc32: u32,
    pub comp_size: u64,
    pub uncomp_size: u64,
    pub header_offset: u64,
    pub mtime_ms: Option<i64>,
}
```

- `locate_eocd<R: Read+Seek>(reader, container_len) -> Result<Eocd, Error>`:
  port the existing backward-scan from `zip_format::eocd_preflight` INCLUDING the
  self-consistency rule (a comment can contain the signature: candidate valid only if
  `cd_off + cd_size == candidate_pos`, keep searching backwards otherwise). New: when
  any of count/cd_size/cd_off is at its `u16::MAX`/`u32::MAX` marker, look for the
  zip64 EOCD locator 20 bytes BEFORE the EOCD (sig `PK\x06\x07` = 0x07064b50), read
  the u64 offset of the zip64 EOCD (sig `PK\x06\x06` = 0x06064b50, ≥56 bytes), take
  count(@32)/cd_size(@40)/cd_offset(@48) from it, and keep a consistency check
  (`cd_offset + cd_size <= eocd64_pos`). No EOCD anywhere → `Error::Corrupt`.
- `parse_cd<R: Read+Seek>(reader, eocd: &Eocd, cancel, mut per_entry: impl FnMut(CdEntry) -> Result<(), Error>) -> Result<ParseStats, Error>` where `ParseStats { parsed: u64, hostile_skipped: u64 }`:
  seek `cd_offset`, wrap in `reader.take(cd_size)` (the CD is NEVER materialized whole),
  loop until the take is exhausted: fixed 46-byte header (sig 0x02014b50 else `Corrupt`),
  LE fields: flags@8, method@10, dos_time@12, dos_date@14, crc@16, comp@20, uncomp@24,
  name_len@28, extra_len@30, comment_len@32, header_off@42. Read `name_len` raw bytes.
  Walk the extra blob (id:u16, size:u16 records, bounds-checked):
  - id 0x0001 (zip64): consume u64 values IN APPNOTE ORDER for each field that was
    0xFFFFFFFF — uncomp, comp, header_offset (disk# u32 last, ignored). A marker field
    WITHOUT its zip64 value → the entry is hostile: report it via per-entry skip (see
    Task 2), never `Corrupt` for the whole archive.
  - id 0x7075 (Info-ZIP unicode path): IGNORED BY DESIGN — never substitutes
    `name_raw` (H3 fix; the crate's substitution/abort is the bug we are removing).
  - truncated/overflowing extra records: stop parsing the blob, keep CD values
    (conservative), entry survives with its raw CD name.
  Skip `comment_len`. `cancel.load(Relaxed)` per entry → `Error::Cancelled`.
- `dos_pair_to_ms(time: u16, date: u16) -> Option<i64>`: port `days_from_civil` +
  `dos_to_ms` math from zip_format.rs, operating on the raw u16 pair
  (date: y=1980+(d>>9), m=(d>>5)&0xF, day=d&0x1F; time: h=t>>11, min=(t>>5)&0x3F,
  s=(t&0x1F)*2). Reject month 0/>12, day 0 → None (defensive).
- `data_offset<R: Read+Seek>(reader, header_offset, container_len) -> Result<u64, Error>`:
  seek `header_offset`, read 30-byte local header (sig 0x04034b50 else `Corrupt`),
  name_len@26 + extra_len@28 (the LOCAL values — they can differ from the CD copies),
  result `header_offset + 30 + name_len + extra_len`; `> container_len` → `Corrupt`.
- Unit tests in-module: EOCD in comment (self-consistency), zip64 roundtrip on a
  hand-forged minimal buffer, dos date math pins (epoch, leap day 2024-02-29, month 0
  → None), extra-walk bounds (record size overflowing the blob).

### Task 2: rewire `zip_format::build_index` over the new parser

- `Locator::Zip` in index.rs becomes
  `Zip { header_offset: u64, method: u16, crc32: u32, comp_size: u64, uncomp_size: u64 }`.
- `build_index` signature loses the `ZipArchive` return: `-> Result<ArchiveIndex, Error>`.
  Flow: `locate_eocd` → `eocd.count > max_entries` → `LimitExceeded{LIMIT_ENTRIES}`
  (this now COVERS zip64 counts — the old u16 preflight gap is closed). `parse_cd`
  with a closure that: decides kind by raw trailing `b'/'`; readable =
  `flags & 1 == 0 && (method == 0 || method == 8)`; builds `Node` (locator only when
  readable File); `index.insert_entry(&name_raw, node, limits)?`; enforces the
  running skipped budget exactly as today (`LimitExceeded` past `max_entries`).
  Also count parsed entries against `max_entries` DURING the walk (EOCD lied low).
  After parse: `eocd.count != parsed` → `tracing::warn!` + `index.skipped +=
  claimed.saturating_sub(parsed)` (an EOCD lying HIGH is stale metadata, not fatal —
  same posture as today, but note the H1 collapse itself can no longer happen).
- `Limits.max_cd_bytes` rustdoc: obsolete since #59 (the CD is stream-parsed, never
  materialized or retained); field kept for API compat, no longer consulted.
- Delete `eocd_preflight`, `dos_to_ms`, `days_from_civil` from zip_format.rs (they
  move/port into zip_cd.rs).

### Task 3: read path without the crate

- `zip_format::read_entry<R: Read+Seek>(mut reader: R, loc: /* the Zip locator fields */, container_len: u64, skip: u64, take: u64, tx)`:
  `data_offset(...)` first. Then:
  - stored (method 0): available = `uncomp_size.saturating_sub(skip).min(take)`;
    seek `data + skip`, stream chunks of 64 KiB; `Ok(0)` before delivering
    `available` bytes → `Corrupt` (container truncated).
  - deflate (method 8): seek `data`, `DeflateDecoder::new(reader.take(comp_size))`,
    skip-loop then take-loop, both `Ok(0)` → `Corrupt` (#95.4 semantics preserved
    verbatim from the current code).
  - CRC32: when `skip == 0 && take == uncomp_size` (full read — the common copy path),
    hash streamed bytes with `crc32fast::Hasher`; final mismatch → send
    `Err(Error::Corrupt)` as the last stream item (mid-stream Err = failed read for
    all consumers). Ranged reads skip CRC (document: partial data cannot be verified
    without decoding the whole entry; the crate had the same hole via seek-less
    read-through only for full reads).
- provider.rs: `CachedContainer` loses the `zip` field (struct may reduce to the
  index Arc — keep the name to minimize churn), drop the `assert_clone` for
  `ZipArchive`, drop `open_archive` and the cached-archive branch in the
  `Locator::Zip` read arm: always spawn_blocking with a fresh `ProviderReader` +
  `read_entry`. `build_blocking` returns the index for ALL formats now.
- Cargo.toml (norte-vfs-archive): remove `zip`; add `crc32fast` if flate2 does not
  re-export it (one-line justification in the commit body: replaces `zip`'s own use,
  smaller tree). Run `cargo deny check` mentally-clean (crc32fast is MIT/Apache).

### Task 4: tests

- FLIP the pin: `pin_h1_colapso_lossy_del_crate_zip` (hostile_zip.rs) asserted the
  upstream collapse via skipped-count; rewrite as
  `h1_nombres_que_colapsan_en_lossy_ya_no_colapsan`: two raw names that decode-equal
  (the existing fixture pair) now BOTH list, byte-exact, skipped == Some(0).
- New `hostile_zip.rs` cases:
  - `extra_7075_valido_jamas_sustituye_el_nombre` (H3): forge entry with a valid
    0x7075 extra whose unicode name differs → listing shows the RAW CD name.
  - `extra_7075_invalido_no_mata_el_archivo` (H3): malformed 0x7075 → archive lists
    fine (crate zip aborted the whole archive here).
  - `zip64_eocd_cuenta_y_lee`: minimal hand-forged zip64 (markers + EOCD64 +
    locator): entry lists and reads byte-exact; and a zip64 EOCD64 claiming
    > max_entries hits `LimitExceeded` BEFORE parsing the CD.
  - `crc_mentiroso_en_lectura_completa_es_corrupt`: stored entry, CRC field lying →
    full read ends in `Err(Corrupt)`; ranged read of the same entry succeeds
    (documented no-CRC).
- ZipSmith: if forging needs helpers (zip64, extra fields), extend
  `norte-testkit/src/smith.rs` (`file_with_extra(name, data, extra: &[u8])`,
  `zip64: bool` on build) — fixtures=código, same style as existing.
- Whole existing suite green: `cargo nextest run -p norte-vfs-archive -p norte-testkit`
  plus the engine archive tests (`-p norte-core -E 'test(archive)'`).

### Task 5: gate

- `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all`,
  `just ci` at the repo root. Known clippy traps: doc_markdown backticks,
  items_after_statements in tests, unnecessary_literal_bound, Duration::from_hours.
