# Nested archives — zip inside tar (#56) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Multi-layer archive addressing per ADR 0018 A3 — `zip+tar+file:///b.tar/!/inner.zip/!/doc.txt` resolves right-to-left (leftmost format = outermost provider, split at the LAST `!` marker), with a configurable nesting-depth limit.

**Architecture:** Proto relaxes `archive_split` (compound inner scheme allowed; split at LAST marker) and `archive_compose` (outer may itself be a well-formed archive path). The engine's `provider_for` recursion already composes layer-by-layer (Box::pin); it gains a nesting-depth gate. `ArchiveProvider` needs no structural change: its `ProviderReader` does ranged reads against the inner provider, which can itself be an `ArchiveProvider`. Perf reality documented: zip inside tar composes by ranges (cheap); anything above a `tar+gz` layer forward-decodes per read (#95.1 spool is the future fix, not this issue).

**Proto impact:** bump (0.23 → 0.24), new goldens for nested wire forms + `Error::LIMIT_NESTING` const; protocol-guardian mandatory. N-1: old daemons reject nested paths with `ArchiveAddressing`→`InvalidPath` (clean degradation — new addressing, not changed meaning).

---

### Task 1: proto — multi-layer split/compose (RED goldens first)

Files: `crates/norte-proto/src/vpath.rs`, `crates/norte-proto/src/error.rs`, golden `methods.json`/schemas, version bump.

- [ ] RED: doctest/unit for `VPath::parse("zip+tar+file:///b.tar/!/x.zip/!/f").archive_split()` → format `"zip"`, outer wire `tar+file:///b.tar/!/x.zip`, inner `[f]`. Plus proptest roundtrip: compose(compose(tar, file-path, [x.zip]), then zip over it) splits back layer-exact.
- [ ] `archive_split`: drop the compound-inner-scheme rejection (`InvalidScheme` branch); split at the **last** marker (`rposition`). A compound scheme with NO marker stays `ArchiveAddressing`.
- [ ] `archive_compose`: outer with a compound scheme is legal IFF `outer.archive_split()` is `Ok(Some(_))` (well-formed lower layer); outer segments may then contain markers, but the roundtrip guard extends: after composing, `archive_split()` of the result must return exactly (format, outer, inner) — property-tested (this subsumes the ADR-0028 tar+gz ambiguity guard).
- [ ] `Error::LIMIT_NESTING: &str = "nesting"` const beside `LIMIT_ENTRIES`/`LIMIT_DECOMPRESSED_BYTES`, golden for the value.
- [ ] Version bump + goldens (nested wire form in methods.json corpus), N-1 window shift. Run protocol-guardian on the diff BEFORE proceeding.

### Task 2: engine — nesting gate + composition (already recursive)

Files: `crates/norte-core/src/engine.rs`, `crates/norte-vfs-archive/src/index.rs` (Limits), `crates/norte-core/src/archive_config.rs`.

- [ ] `Limits.max_nesting: usize` default **3** (rustdoc: layers of archive formats; 1 = plain `zip+file`).
- [ ] `provider_for` archive branch: compute layer count by peeling `scheme_archive_format` repeatedly on the scheme; `> limits.max_nesting` → `Error::LimitExceeded { limit: Error::LIMIT_NESTING }` BEFORE composing anything.
- [ ] `[archive] max_nesting` in `archive_config.rs` (+ TUI config parity in `norte-tui/src/config.rs`, project layer ignored as the rest).
- [ ] Tests (engine_archive.rs): zip-in-tar via MemProvider (TarSmith embedding a ZipSmith build as a tar file entry) — list + read byte-exact through `zip+tar+mem://…`; nesting over the cap → LimitExceeded; eviction drag (#47) still sweeps `zip+tar+sftp://h` when `sftp://h` dies (suffix rule already matches — pin it).

### Task 3: known-limitation documentation + generation caveat

- [ ] Nested cache generation uses the (mtime,size) of the *entry inside the outer archive* — replacing the outer container with same-metadata entries can serve a stale inner index; full-read CRC (#59) and fail-loud short reads are the backstop. Document in ArchiveProvider rustdoc + ADR addendum (single paragraph added to ADR 0018, "Nesting (v2, #56)" section — do NOT rewrite the ADR).
- [ ] TUI: Enter on an archive file *inside* an archive pane must compose over the composite path (check `archive_root_for` guards; the compose relaxation makes it legal). Snapshot/unit test.
- [ ] CLI: `vpath()` accepts nested URLs (is_archive_url / REMOTE_SCHEMES path) — verify, test.

### Task 4: gate + reviewers

- [ ] `just ci` EXIT 0; reviewers: protocol-guardian (Task 1, mandatory), rust, security (nesting = resource amplification: a zip bomb inside a tgz — verify limits stack per layer), encoding-auditor (marker/name interplay with hostile names).
