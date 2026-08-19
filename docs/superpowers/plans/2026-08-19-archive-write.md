# Plan — writing archives (#132)

Design: `docs/superpowers/specs/2026-08-19-archive-write-design.md`.
Branch: `feat/archive-write`. Proto **0.50.0** (additive: three methods).

Gate budget: `just t <crate>` in the loop; ONE `just ci-fast` around task 6;
ONE `just ci` before the merge.

---

## T1 — the format writers, pure and provider-free

`crates/norte-vfs-archive/src/write/` — new module, no `Provider` in sight: an
iterator of `(name bytes, kind, mode, mtime, size, reader)` in, bytes out.

- `zip.rs`: local header + data descriptor + central directory + EOCD.
  Deflate through `flate2`, CRC-32 through the `crc32fast` already re-exported.
  **Bit 11 set iff the name is valid UTF-8**; otherwise raw bytes, bit clear.
  Zip64 when any entry or the total crosses 4 GiB — a silently truncated
  offset is a corrupt archive that opens.
- `tar.rs`: `tar::Builder` over a writer, GNU long names so a 200-byte name is
  not truncated to 100.
- `targz.rs`: the tar writer wrapped in `flate2::write::GzEncoder`.

Tests here, where there is no I/O: every hostile-corpus name round-trips
through the writer and back through `zip_cd.rs` / the tar reader, byte-exact,
and bit 11 is asserted in both directions.

## T2 — proto 0.50.0

`ARCHIVE_PACK`, `ARCHIVE_TEST`, `FILE_SPLIT`, `FILE_COMBINE`; params
`ArchivePackParams`, `ArchiveTestParams`, `FileSplitParams`,
`FileCombineParams`; results `ArchiveTestResult` + `ArchiveTestFailure`
(everything else is `FsTaskResult`). `TaskKind::Pack`, `TaskKind::TestArchive`,
`TaskKind::Split`, `TaskKind::Combine`. Golden tests for each new type,
rustdoc + doctest on every public item, `PROTOCOL_VERSION` to 0.50.0.

`protocol-guardian` is mandatory on this task.

## T3 — the pack op

`norte-core`: `Engine::pack` → Task (`TaskKind::Pack`), cancellation checked
per entry AND inside the copy of a big entry; writes to
`<dest>.norte-partial`, renames at the end; journals ONE `created` with
`Reversal::Delete`, registered after the rename.

Test: cancel mid-pack leaves nothing unmarked; undo removes the archive.

## T4 — the test op

`Engine::test_archive` → Task, streams every entry to its end, per-format
`checked`, bounded failure list with `truncated`. No journal. Read gate.

Test: a zip with one flipped CRC byte fails and names the entry; a plain tar
reports what it could not check.

## T5 — split and join

`Engine::split_file` / `Engine::combine_files` → Tasks, `.001…` convention,
refuse >999 parts BEFORE writing, refuse a gap and a short middle part on
join, journal per node created.

Test: exact-multiple split writes no empty trailing part; join round-trips
byte-exact; gap and short-part refusals.

## T6 — daemon handlers, backend arms, dispatch

Four handlers in `daemon/server.rs`, four arms in `backend.rs` (embedded +
remote), the dispatch table. `just ci-fast` once, here.

## T7 — the TUI

Five commands `Planned` → `live` in `norte-frontend`'s catalogue; the keys the
four presets already declare; `pane.unpack` issues an `fs.copy` from the
container's interior root (no new method — see the design); pack/split
dialogs; the test result dialog; refusals with a reason on a read-only or
non-container target. i18n in both locales, help topics in both locales.

## T8 — close out

`encoding-auditor` (names inside archives) and `rust-reviewer` before
committing; `protocol-guardian` already ran in T2. ADR only if T1–T7 forced a
decision the design did not already make. CHANGELOG, memory, `just ci`.
