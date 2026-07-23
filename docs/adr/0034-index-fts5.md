# 0034 - Search index: `norte-index` crate with SQLite FTS5

- Status: accepted
- Date: 2026-07-23
- Decision makers: Oscar González
- Related: specification §9 (subsystem `norte-index`), §337 ("SQLite with FTS5 is
  the first index and storage engine"), §346 ("Live and indexed search"); M4
  "semantic search"; ADR 0020 (journal, sqlx/SQLite); ADR 0001 (VPath wire form).

## Context

M4 calls for semantic search. Semantic (embedding) search needs a persistent
index — embedding every file on every query does not scale. The specification
sequences this: §337 makes **SQLite with FTS5 the first index engine**, with a
semantic embedding layer on top afterwards. `AiProvider::embed()` already exists
in `norte-ai`, so the missing foundation is the index store and a name/metadata
FTS5 search, which this ADR introduces as the first of two sub-projects.

The existing `fs.search` is a *live* walk (glob/regex over `Provider::list`,
cancellable, streaming). It re-walks the filesystem on every query. An index
gives fast repeat queries and is the substrate the semantic layer extends.

## Decision

Add a **`norte-index` crate** (AGPL-3.0-only — a core subsystem, spec §9)
providing an `Index` over a SQLite database, reusing the `SqliteJournal` open
pattern (sqlx 0.8, WAL). FTS5 is confirmed present in the bundled SQLite
(`libsqlite3-sys` 0.30.1 — an empirical probe created an FTS5 virtual table and
ran `MATCH`).

### Schema: raw-bytes authority + lossy-UTF-8 matching (regla 1)

FTS5 tokenizes **UTF-8 text**; filenames are **bytes** and may not be UTF-8. The
index keeps both:

- **Authority**: `files.path` stores the VPath's **lossless `to_wire()` form**
  (percent-encoded, ASCII, ADR 0001) — a query recovers the exact `VPath` (and
  thus exact name bytes) via `VPath::parse`.
- **Matching**: an external-content FTS5 table `files_fts` indexes lossy-UTF-8
  *display* columns (`name_display`/`path_display` = `from_utf8_lossy` /
  `display_lossy`), kept in sync with `files` by AFTER INSERT/DELETE triggers.

A non-UTF-8 name is therefore searchable by its display form (`U+FFFD` where bytes
were invalid) and returned byte-exact. Search addresses the display; the stored
path is the truth. This is the regla-1-correct trade-off: never corrupt a name.

### Manual reindex

`index.build(root)` walks a provider and (re)indexes the subtree: upsert by
`(root_id, path)` tagged with a monotonic `build_id`, then delete rows of that
root not seen this build. A cancelled build skips the delete-sweep, leaving a
coherent superset (never a wrongly-pruned index). Auto-watch (inotify) and
journal-hook live updates are deferred.

### Wire + ownership

Additive proto methods `index.build` (a Task, like `fs.copy`) and `index.query`
(direct), plus `TaskKind::Index` (degrades to `Unknown` on N-1). The `Engine`
holds an `Option<Arc<Index>>`; the daemon installs it at
`~/.local/share/norte/index.db`. Absent index → `index.*` returns `Unsupported`,
fail-closed like AI-off. Build/query are policy-gated by actor. Single-writer =
the daemon (like the journal).

## Consequences

- No new heavy dependency: `sqlx` is already in the tree (journal). FTS5 comes
  from the bundled SQLite at no extra cost.
- The index is a rebuild-on-demand cache: it can be stale between builds
  (external changes are not seen until the next `index.build`). Acceptable for the
  first cut; live updates are tracked debt.
- **Sub-project 2 (semantic)** extends this store: an embedding-vector column (or
  sibling table) populated via `AiProvider::embed`, queried by nearest-neighbor,
  gated by the AI policy. Out of scope here.
- Residual debt: file **content** full-text (not just names), **tags**, and
  **auto-watch** — all deferred (spec §9 lists them under `norte-index`).
- Review debt (M4 reviewers):
  - **Streaming build.** `index.build` materialises the whole `Vec<IndexEntry>`
    from the walk before writing (bounded by `MAX_INDEX_ENTRIES = 5_000_000`).
    A huge tree holds all path bytes resident. `Index::build` already commits in
    512-row batches; the walk should stream into it over a bounded channel so
    peak memory is O(batch). Deferred; the cap plus the `read_gate` (only
    scope-granted roots reach the walk) bound the exposure.
  - **NFC-insensitive match.** The authority preserves bytes exactly, but the
    FTS `*_display` columns and the query are not NFC-folded, so a macOS-NFD name
    searched with an NFC term can miss (a *recall* gap, never corruption). Search
    is display-form best-effort; NFC-fold-for-match is future work.
  - **`root_id` is a 64-bit FNV hash** used only as a scoping key (never
    authority). A collision (astronomically unlikely, and the colliding root must
    also be a valid scope-granted VPath) would at worst merge two roots' results.
    A `roots` intern table would remove the confused-deputy shape; deferred.
  - **`IndexBuildResult` counts** (`indexed`/`removed`) are not forwarded over the
    daemon wire yet (the handler returns `FsTaskResult{task_id}`; task completion
    is the signal). A report-fetch method analogous to `policy.undo_report` is
    future work. The embedded backend surfaces the counts directly.
