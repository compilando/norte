# Indexed search — sub-project 1: `norte-index` + FTS5 name/metadata index — design

**Date:** 2026-07-23
**Status:** approved
**Related:** spec §9 (subsystems: `norte-index` = "SQLite metadata, search, tags, and embeddings"), §337 ("SQLite with FTS5 is the first index and storage engine"), §346 ("Live and indexed search"); M4 "semantic search". ADR (new crate + FTS5 build dependency). Sibling: the future sub-project 2 (semantic embeddings over stored vectors).

## Scope

The first of two sub-projects for M4 indexed/semantic search. This one builds the
**foundation**: a `norte-index` crate with a SQLite **FTS5 name/metadata index**,
a manual `index.build` that walks a provider and (re)indexes a subtree, and an
`index.query` FTS5 lookup — **no AI, no content, no tags, no auto-watch**. It is
independently useful (fast indexed name search that does not re-walk the FS per
query) and is the store the semantic layer (sub-project 2) later extends with
embedding vectors.

Confirmed decisions (brainstorming): index **names + metadata only**; **manual
reindex** by command (auto-watch / journal-hook deferred).

## Architecture

New crate **`norte-index`** (AGPL-3.0-only — a core subsystem, spec §9), depending
on `norte-proto`, `norte-vfs`, and `sqlx` (SQLite, already a workspace dep via the
journal). FTS5 is confirmed available in the bundled SQLite (empirical probe:
`CREATE VIRTUAL TABLE … USING fts5` + `MATCH` work under `libsqlite3-sys` 0.30.1).

The crate exposes an `Index`:

- `Index::open(path) -> Result<Index, IndexError>` — opens/creates the SQLite DB
  (WAL, `0600`, idempotent schema migration), like `SqliteJournal::open`.
- `Index::open_memory()` — in-memory, for tests.
- `Index::build(root, entries, token) -> Result<BuildReport, IndexError>` — replaces
  the index for one `root`: upsert each entry by `(root_id, path)`, then sweep rows
  of that `root_id` not seen this build (deletion of vanished paths). Cancelable via
  the `CancellationToken`; on cancel the seen rows persist but the delete-sweep does
  NOT run (a cancelled build leaves a coherent superset, never a wrongly-pruned
  index).
- `Index::query(root, text, limit) -> Result<Vec<IndexHit>, IndexError>` — FTS5
  `MATCH` over the name (and path) display columns, scoped to `root`, ranked by
  `bm25`, capped at `limit`. Returns `IndexHit { path: VPath, kind, size, mtime_ms }`.

`root_id` is a stable hash (or interned id) of the root's `scheme://authority` +
base path. The index is provider-agnostic: any `Provider` can be walked.

## Schema

```sql
-- Autoridad: los BYTES crudos del path/nombre viven aquí.
CREATE TABLE IF NOT EXISTS files (
    root_id   INTEGER NOT NULL,
    path      BLOB    NOT NULL,   -- VPath serializado (bytes crudos, regla 1)
    name      BLOB    NOT NULL,   -- último segmento, bytes crudos
    kind      INTEGER NOT NULL,   -- EntryKind discriminante
    size      INTEGER,            -- NULL para dirs
    mtime_ms  INTEGER,            -- NULL si desconocido
    PRIMARY KEY (root_id, path)
) WITHOUT ROWID;

-- Índice de MATCHING: tokeniza la vista UTF-8 (display) de name/path. content=''
-- (external content) para no duplicar los bytes; se sincroniza con `files` a mano.
CREATE VIRTUAL TABLE IF NOT EXISTS files_fts USING fts5(
    name_display, path_display
);
-- Una fila de files_fts por fila de files, con el mismo orden de inserción; se
-- correlacionan por el rowid de files_fts == un id monótono guardado en files.
```

Refinement locked at implementation time: correlating `files` ↔ `files_fts`. The
simplest robust approach is to give `files` an `INTEGER PRIMARY KEY fts_rowid`
(a plain rowid table, not WITHOUT ROWID) and insert into `files_fts` with the same
explicit `rowid`, so a query joins `files_fts` MATCH → rowid → `files`. The plan
uses that shape (drop WITHOUT ROWID; `files` gets an alias rowid).

## Regla 1 — the load-bearing encoding decision

FTS5 tokenizes **UTF-8 TEXT**; filenames are **bytes** and may not be UTF-8. The
index therefore keeps two representations:

- **Authority = raw bytes**: `files.path` / `files.name` are `BLOB`s holding the
  exact `VPath` bytes. Query results reconstruct the `VPath` from these — byte-exact.
- **Matching = lossy UTF-8 view**: `files_fts.name_display` / `path_display` hold
  `String::from_utf8_lossy(name)` for tokenization only. A non-UTF-8 name is
  searchable by its display form (with `U+FFFD` where bytes were invalid) and still
  returned byte-exact.

This means: search matches the *display* form (what the user sees and types); the
*bytes* are authoritative and never lost. A query can't address the exact invalid
bytes, only their display — an accepted limitation for a search UI, explicitly the
regla-1-correct trade-off (never silently corrupt a name; the BLOB is truth).
Documented; encoding-auditor reviews.

## Wire surface (proto — additive minor bump)

- `index.build` — params `{ root: VPath }` → a Task (progress + cancellation, like
  `fs.copy`/`fs.search`). Result `IndexBuildResult { indexed: u64, removed: u64 }`.
- `index.query` — params `{ root: VPath, text: String, limit: u32 }` → result
  `IndexQueryResult { hits: Vec<IndexHit> }`, `IndexHit { path, kind, size, mtime_ms }`.

Additive: new methods + types, N-1 window shifts, goldens added. protocol-guardian
reviews. `Error` reuses the existing taxonomy (`Io`, `InvalidPath`, a new
`IndexUnavailable`/reuse `ProviderUnavailable` if the DB can't open — decided in the
plan; prefer reusing existing variants over a new one unless necessary).

## Engine wiring

- `Engine` holds an `Option<Arc<Index>>` (like the journal observer), installed by
  the daemon (`norte daemon run`) at a fixed `~/.local/share/norte/index.db`
  (XDG data). Absent index → `index.*` returns `Unsupported` (fail-closed, like AI
  off).
- `index_build_as(root, actor)` — a `Task` (`TaskKind::Index`, new) whose inner loop
  walks the provider recursively via `Provider::list` (bounded depth/entries, checks
  the `CancellationToken` every N entries, regla 3), streaming entries into
  `Index::build`. Gated by the policy engine by `actor` (same `*_with_as` pattern).
- `index_query(root, text, limit)` — reads the index, no walk. Gated by policy read.
- The recursive walk reuses the existing bounded-walk shape from `search::run_walk`
  where possible (or a sibling); it must not follow symlinks into cycles (visited
  set or depth cap).

## Daemon / Backend / CLI

- Daemon handlers `index.build` / `index.query` (dispatch by actor, like fs tasks).
- `Backend` methods `index_build` / `index_query` (embedded + remote).
- CLI `norte index build <path>` (Task with progress) and `norte index query <path>
  <text>` (prints hits). TUI integration = debt (a later, thin addition).

## Error handling / cancellation

- `Index::build` is a Task with a `CancellationToken`; a cancellation test asserts a
  clean partial index (indexed rows persist; no erroneous deletion; re-running
  completes). Regla 3 + a clean-cancellation test (mandatory for new tasks).
- SQLite errors map to the proto taxonomy (`Io { retryable:false }` for a corrupt/
  locked DB; the open failure surfaces as `Unsupported` at the engine boundary when
  no index is installed).
- The index DB is single-writer (one daemon); like the journal, embedded concurrent
  CLI/TUI writers are out of scope (the daemon owns it).

## Testing

- `norte-index` unit: open/build/query happy path; **reindex updates** (a second
  build with a changed tree upserts new + sweeps vanished); **non-UTF-8 name** is
  indexed, queryable by display form, and returned byte-exact; FTS5 `MATCH` ranking;
  empty query / no hits; `open_memory` for all.
- Engine integration: `index_build_as` over `MemProvider` (seeded tree, incl. a
  hostile non-UTF-8 name), `index_query` returns the seeded entries; **clean
  cancellation** (cancel mid-build → coherent index, re-build completes).
- proto golden tests for the new methods/types.

## Non-goals (sub-project 1)

Content/full-text of file bodies, embeddings/semantic NN, tags, auto-watch/inotify,
journal-hook live updates, TUI/GUI surface beyond the CLI. All deferred to
sub-project 2 or tracked debt.
