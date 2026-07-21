# 0028 - `tar+gz` as an opaque compound archive format

- Status: accepted
- Date: 2026-07-21
- Decision makers: Oscar González
- Related: ADR 0018; issues #55 and #56

## Context

Plain TAR entries can use ranged passthrough because their bytes are contiguous
and offsets refer directly to the container. gzip is sequential and its
compressed offsets do not match decompressed TAR offsets. Yet `.tar.gz` and
`.tgz` are the most common TAR forms.

ADR 0018's whitelisted compound schemes already allow
`tar+gz+file://...`, but the original first-token parser treated it as TAR over
an unknown `gz+file` provider.

## Options considered

1. **Whitelist `tar+gz` as one compound token.** Use longest-prefix matching
   and keep gzip opaque inside `Format::TarGz`. This is a small compatible
   change, though each future combination needs another explicit entry.
2. **Add a general compression-layer grammar.** This is extensible but turns
   `ArchiveRef` into a layer stack and effectively implements deferred nested
   archives from #56 for a single current need.
3. **Spool decompressed TAR to disk.** Later reads become random access, but
   temporary-file lifecycle, disk budgeting, and full upfront decompression
   penalize simple listing. Keep this as a possible hot-read optimization.

## Decision

Choose the explicit `tar+gz` format.

1. Add `tar+gz` to `ARCHIVE_FORMATS` and make `scheme_format_prefix` use the
   longest match. Keep `archive_compose` and `archive_split` shapes unchanged,
   continue rejecting nested compound interiors, and expose
   `scheme_archive_format` so frontends do not duplicate the grammar.
2. Build the index by passing `flate2::read::MultiGzDecoder` to
   `tar::Archive::entries()`. Store `Locator::Gz` offsets in the decompressed
   stream. Detect truncation while indexing or reading and never return silent
   short data.
3. For each read, create a fresh decoder, discard up to the entry offset while
   checking channel closure, and send entry bytes through a bounded channel.
   Reading is O(decompressed bytes before the entry) in v1.
4. Add `Limits.max_decompressed_bytes`, defaulting to 64 GiB, to cap total
   index-pass output and CPU exposure to gzip bombs. Individual reads remain
   bounded by entry size.
5. Make `flate2` a direct archive-provider dependency. It was already present
   transitively and its licenses are already allowed.

## Consequences

Users can browse the most common TAR format through the same interface as ZIP
and plain TAR without a wire migration. Protocol surface growth is limited to
longest matching and one public helper. Reads near the end of a large archive
remain O(n); future spooling or restart points may optimize them. Each new
compression format remains an explicit whitelist entry until #56 justifies a
general layered grammar. The existing generation-keyed LRU cache still applies,
although TAR.GZ does not use ZIP central-directory caching.
