# Changelog

All notable changes to norte are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and releases follow
[Semantic Versioning](https://semver.org/). The wire protocol is versioned
independently through `PROTOCOL_VERSION`.

## [Unreleased]

### Added

- **Help corpus (H3a, ADR 0040):** new crate `norte-help`, the foundation of
  the help-system redesign. Topics are markdown-lite files with TOML front
  matter between `+++` fences, embedded through an explicit `include_str!`
  table (the `norte-theme` preset pattern) with a test that cross-checks the
  table against the directory, so a topic file cannot be silently left out.
  The markdown accepted is a **closed vocabulary** — headings, paragraphs,
  bullets, fenced code, tables, callouts, inline strong/emph/code — plus two
  **live marks** the parser deliberately leaves unresolved: `{{cmd:id}}` and
  `[[topic]]`. A frontend resolves them at draw time through the new
  `ChordResolver` seam, so the prose shows the key the user actually has bound
  and can never claim a chord that a rebind has moved. Ships six seed topics
  in English and Spanish (index, panes, selection, copying, remote, archives),
  every factual claim in them verified against the code rather than against
  the design docs.
  A second parse mode reads plugin-supplied `help.md` as hostile input: it
  never fails and never panics, decodes through `norte-encoding` (so a
  BOM'd file keeps its header instead of losing it), bounds source bytes,
  line length, block count and total table cells, masks terminal hazards
  where the data is built, and reports `truncated`/`lossy` for the UI badge.
  A plugin's topic id is host-assigned, its `{{cmd:…}}` marks must be
  namespaced to itself, and its `[[…]]` links are inert — so plugin help
  cannot shadow a built-in topic, forge a reference to a host command, or
  link into the host corpus.
  Integrity checks (`check_corpus`, `check_commands`, `check_contexts`)
  report findings as data for both the test suite and the future `norte
  doctor`, covering locale parity, duplicate ids, dangling links,
  unknown/undocumented commands, unknown/duplicate contexts, stale allowlist
  entries, and marks typed where the parser cannot make them live. A gate in
  `norte-tui` fails the build when a command in the vocabulary appears in no
  topic, with a hand-written allowlist that phase H3h drains to zero.
  The canonical `norte-testkit` corpus grows a fixture for the live mark
  itself (`cmd_mark_bidi_payload`, hostile names 31 → 32): a bidi override
  inside a `{{cmd:…}}` payload, which the parser must carry byte-for-byte so
  the gate's byte-exact cross-check refuses to ship it.
  No frontend renders any of this yet — `norte-tui` takes the crate as a
  **dev-dependency only**, for the gate; the F1 overlay is phase H3b/H3c.

- **Semantic index (M4-IA-2, ADR 0031 A3, proto 0.33.0):** two new
  methods over the wire. `index.embed` is a cancellable Task
  (`TaskKind::Embed`) that embeds the files a previous `index.build`
  already indexed: it filters by `denied_prefixes`, an
  extension-based text heuristic and a size cap **before reading a
  single byte**, then reads bounded 32 KiB prefixes through the
  providers, skips anything whose `(sha256, model)` is unchanged, and
  batches 16 texts per provider call with a bounded, cancel-aware
  retry on rate limits. `index.search_semantic` is a direct response,
  cancellable via `rpc.cancel`: one embedding call for the query plus
  a brute-force cosine scan over the stored vectors (`k` clamped to
  100, query capped at 4 KiB, scores guaranteed finite). Vectors live
  in the existing index database as an additive `embeddings` table
  (f32 little-endian, cascading with the file row); a vector from
  another model counts as absent and is regenerated. Both endpoints
  are human-only — content prefixes and the query leave the process,
  so agent connections are denied fail-closed — and both pass the
  full AI gate (`enabled`, `local_only`, `denied_prefixes`).
  Configuration is `[ai] embed_provider`; local Ollama is the
  expected default. New CLI verbs `norte index embed` and `norte index
  semantic`, plus a semantic search flow in both frontends
  (`pane.semantic-search`): query prompt → cancellable search (TUI) →
  hostile-safe hit list (masked paths, badges, scores that a crafted
  path cannot push out of view) → Enter navigates to the file. A
  hostile or broken daemon cannot flood either frontend: the shared
  `validate_semantic_hits` belt in `norte-frontend` rejects any
  response over the wire ceiling or carrying a non-finite score,
  whole, never truncated.

- **AI rename over the wire (M4-IA, ADR 0031, proto 0.32.0):** new
  `ai.rename_plan` method — a direct response, cancellable via
  `rpc.cancel`, that returns the reviewable plan and never mutates.
  The daemon builds the configured AI provider at startup (opt-in,
  degrading — a broken `[ai]` never aborts `norte daemon run`), denies
  agents fail-closed, and caps the instruction at 4 KiB. TUI and GUI
  gain the full flow (`pane.ai-rename`): instruction prompt → reviewable
  plan modal (target dir shown, numbered pairs, hostile names masked and
  badged, scrollable window, plans over 256 entries rejected en bloc) →
  N journaled `fs.move` tasks with undo. A malformed pair from a
  hostile or broken daemon aborts the whole apply before any move is
  submitted (shared `validate_ai_plan` belt in `norte-frontend`).

## [0.3.0-alpha.2] - 2026-08-02

### Added

- **The TUI watches the visible directories (#106):** external changes to
  the panes' local directories now refresh automatically — a native
  watcher (inotify/FSEvents/ReadDirectoryChangesW) over both visible
  `file://` dirs, with a graceful fallback to a 2-second mtime poll (with
  a one-time status notice) when the watcher cannot start or the inotify
  watch limit is hit, never a failure. Events are debounced with a true
  trailing edge plus a floor between refreshes, so a large copy into the
  watched directory coalesces instead of refreshing every 300 ms; a watch
  event never interrupts an open dialog, overlay, or quick search — it
  queues and fires when the interaction ends. The refresh takes the same
  cancellable path as Ctrl+R (marks survive, #118 ritual). Remote and
  archive panes remain manual-refresh (no inotify there); polling-mode
  limits are stated honestly in the notice (edits to existing file
  contents don't change the parent dir's mtime). GUI watching is still
  pending on #106.

- **Plugin column cells rendered in both frontends (#117 follow-up):**
  `plugin:<plugin>/<column>` ids configured in `[ui.columns]` now paint
  real cells through the shared column funnel — default width 12 (spec
  width/align/header overrides apply), headers via the shared label
  resolver, capped at 8 plugin columns per list (painted always equals
  requested; `norte doctor` reports the excess as
  `columns-plugins-over-cap` and retires `columns-no-renderer`). Values
  arrive asynchronously per listing (piggybacked on the decoration fetch
  in the TUI, the session Columns command in the GUI), are validated
  against the live catalog (approved + enabled + the column declared by
  THAT plugin), sanitized and capped on ingest, and defensively re-masked
  at render; absent stays blank. The GUI's previous behavior of
  unconditionally painting every declared plugin column at a fixed 96px
  is retired: `[ui.columns]` is now the single source of truth. Deferred
  to #120: offering declared plugin columns in the picker, and
  disambiguating duplicate bare column ids across plugins.

- **Provider attribute columns rendered in both frontends (#117):** the
  `attr:` columns the config, model and picker already accepted now paint
  real cells. The shared render funnel is `ColumnId`-typed end to end;
  attr cells format by the value's own tag refined by the catalog hint
  (sizes IEC/SI/exact, timestamps relative/ISO, POSIX modes `rwx`/`octal`
  — two new spec format words), third-party `Text`/`Bytes` values render
  masked and capped (bytes lossy-with-U+FFFD, originals untouched), blank
  strictly means absent (`?` = present but unpaintable). Panes request the
  configured attr ids on every listing and cache the provider catalog once
  per scheme (`fs.capabilities`); the picker now OFFERS advertised
  provider columns (disabled rows, localized or masked labels) and cycles
  attr formats by hint. Column headers resolve localized → masked catalog
  label → sanitized id. `norte doctor` retires `columns-no-renderer` for
  `attr:` (plugin cells still pending) and gains
  `columns-attrs-over-cap` (>16 configured) and
  `columns-attr-id-not-wire-safe` (an id that parses but is illegal on the
  wire is skipped instead of failing remote listings). The TUI/GUI refresh
  affected panes when a picker apply or hot-reload changes the requested
  attr set; painted always equals requested. Follow-up filed: #118
  (pre-existing Ctrl+R refresh ritual gap). No wire change (additive 0.30
  contract). Sorting stays on the closed name/size/mtime vocabulary —
  sorting by attr columns is future work.

- **GUI column picker — Alt+C (#108 block 7c):** the GUI gets the same
  picker the TUI ships, as an overlay panel over the shared model: toggle
  (Space/E), reorder (Shift+↑/↓), sort by the cursor's column (S), cycle
  format (F); Enter applies in-session and persists (columns + sort +
  changed formats), Esc discards. Applying clears the session header-click
  sort override for the panes the save targets — the persisted sort
  supersedes it. Opaque ids render masked and length-capped (screen readers
  included); the panel scrim occludes mouse input; all GUI config writes are
  now serialized (follow-up for atomic persist: #116). Closes the last
  block of the columns design.

- **Provider attributes produced end-to-end (#108 block 2):** the wire that
  0.30.0 shipped now carries real data. `Provider` gains defaulted
  `attrs()`/`list_with()`/`stat_with()` (`ListOptions`/`AttrRequest`);
  producers: local (`posix.mode`/`uid`/`gid`/`nlink`/`ctime_ms` on unix,
  `win.attributes` on Windows — the #52 lazy listing stays untouched unless
  an advertised id is requested), sftp (`posix.mode`/`uid`/`gid` off the
  already-parsed SFTP attrs), object (`s3.etag`; `s3.content_type` on stat),
  archive-zip (`archive.method`/`packed_size`/`crc32`, kept even for
  encrypted entries) and the testkit `MemProvider` (hostile synthetic
  values). The daemon publishes the catalog through `fs.capabilities`,
  rejects malformed or over-16 requested ids (`-32602`), forwards only
  advertised ids and enforces emit caps per entry; paginated listings keep
  the request from the opening call. The conformance suites gain an
  attributes contract (type agreement, request scoping, caps,
  unknown-id absence). CLI: `ls --attrs <id>` (repeatable; wire-exact under
  `--json`, masked column in human output). Deferred with issues:
  `sftp.owner`/`group` names (#114), `s3.storage_class` (#115).

- **Per-column presentation — `[[ui.columns.spec]]` (#108 block 7b):** a
  spec entry keyed by column `id` sets `width` (`"auto"` / `{ fixed = n }` /
  `{ min = n, weight = m }`), `align` (`left`/`right`), `format` (size:
  `exact`/`iec`/`si`; mtime: `relative`/`iso`) and a custom `header`
  (sanitized and capped at resolve), globally or per scheme (scheme wins,
  last-wins per field). Both frontends honor the resolved style at the same
  points they already read the shared layout: custom headers replace the
  Fluent label, cells format through `styled_cell`, align picks the padding
  side, and width overrides flow through the shared layout. Every
  vocabulary is closed and validated at load (a typo is a load error naming
  the path); whether a format fits its column is a resolve-time diagnostic:
  the default is applied and `norte doctor` reports it as
  `columns-bad-spec` (masked, capped) — never a silent skip.
- **Column format cycling in the picker — `f` (#108 block 7b):** inside the
  Alt+C picker, `f` rotates the format of the row under the cursor through
  its closed vocabulary (size: `iec`/`si`/`exact`; mtime: `relative`/`iso`;
  name/kind/opaque rows have no format) and the row shows the current value
  (` · iec`). The cycle starts from the RESOLVED style of the pane's scheme,
  and Enter persists only the formats that actually changed, each as a
  replace-by-id `[[ui.columns.spec]]` entry that preserves the entry's other
  fields (header/width/align) — the session sees the new format immediately,
  in lockstep with the file. One exception: a row whose format is pinned by
  a scheme-level spec is LOCKED in the picker (cycling would write a global
  entry the scheme override keeps masking, and leak into other schemes) —
  edit the scheme spec in `norte.toml` instead. Width cycling stays
  deferred (needs numeric entry UX).
- **TUI column picker — Alt+C (#108 block 7a):** a keyboard-driven overlay
  over the shared picker model: `e`/space toggles a column on or off (name
  is pinned first and immutable), Shift+↑/↓ — or vim-style `K`/`J` —
  reorders below the pinned name, Ctrl+S applies the header-click sort
  semantics to the row under the cursor, Enter applies to the session AND
  persists to `norte.toml`, Esc discards. The save target is one rule,
  stated in the title: the pane's scheme if the config already has an entry
  for it, otherwise the `[ui.columns]` default. Non-builtin ids (attr:/
  plugin:/unparseable) appear as inert-but-editable rows, masked in the
  render, and survive a save verbatim — cleaning the user's config is
  doctor's job. Also fixed here: editing `[ui.columns]` outside now
  hot-reloads into the session (dead since block 4), and a configured
  mid-list `name` is normalized to the front (the TUI budgets the first
  width as the name).
- **Size and date in the GUI listing (#108 block 6):** the pane paints the
  same default column set as the TUI — name, size (IEC), modified (relative
  time) — from the shared layout, under a header row with the ▲/▼ sort
  indicator; the plugin-column headers (G3c) join that row over their fixed
  cells. Sortable headers (Name/Size/Mtime) are clickable: a click flips or
  switches the sort and is remembered per pane for the session, surviving
  cd — persisting it is the picker's job (block 7, with the context menu).
  Absent values stay blank (never a fabricated 0).
- **Column and sort configuration (#108 block 4):** `[ui.columns]` chooses
  which built-in columns each pane paints and the sort order — globally and
  per scheme (an override REPLACES the list; sort vocabulary is closed and
  validated at load). Both frontends seed the sort at startup and re-apply
  it when a cd lands on another scheme (the GUI consumes the sort only —
  its cells arrive with block 6). A configured column id that does not
  parse, or that has no renderer yet (attr:/plugin:), never disappears
  silently: `norte doctor` names it. Per-column width/format overrides
  (`[[ui.columns.spec]]`) land with the picker block.
- **Size and date in the TUI listing (#108 block 5):** the pane paints the
  default column set — name, size (IEC), modified (relative time) — under a
  dim header line carrying the sort indicator; absent values stay blank
  (never a fabricated 0), hostile names keep their badge and never break
  the column alignment, and the first-render budget is unchanged (~0.7ms
  for 100k entries). Column choice/config and the picker are the next
  blocks of the approved design.
- **Manual refresh — Ctrl+R (#106, beta minimum):** reloads both panes
  through the same cancellable path as the post-mutation refresh — marks
  survive by identity with visible pruning, cursor is kept by index, and
  a live-search results pane is left alone. Real directory watching stays
  tracked in #106.
- **Rename and editable destination name (#105):** Shift+F6 renames in
  place (a Move to the entry's own parent — correct inside search results
  too), and F5/F6 with a single item opens an editable destination name
  prefilled with the original; multi-item batches keep the list confirm.
  Byte-exact by rule 1: an untouched prefill copies the ORIGINAL bytes
  (never the lossy form), editing works on the displayed text, and a name
  still containing U+FFFD is rejected instead of writing mojibake.
  Collisions reuse the existing dialog; a failed submit keeps the typed
  name. TUI only — the GUI still has no text input.
- **Create directory — F7 (#104, proto 0.31.0):** `fs.mkdir` as a policy-
  gated, journalled Task (`Created` with undo; clean cancellation), wired
  end to end: engine, daemon (rpc.cancel-able), both backend modes, a TUI
  F7 dialog with the same masking discipline as the pattern dialog, and
  `norte mkdir` in the CLI. Not `mkdir -p`: the parent must exist, and any
  occupant — a directory included — is a conflict. The GUI picks it up
  when it grows text input (same explicit gap as mark-by-pattern).
- **Hidden-entry toggle (#107):** Ctrl+H (and Alt+.) shows or hides unix
  dot-entries per pane, in both frontends; `[ui] show_hidden` seeds the
  startup state. Presentation only — the provider keeps listing everything,
  and while hiding, the pane says how many entries are stashed. Hiding
  prunes marks of the entries it removes (reported, never silent), and the
  live-search results pane is exempt: a hit you asked for is never
  swallowed by the filter.
- **First-class selection (#103):** mark, mark all, invert, clear, and mark or
  unmark by glob, in both frontends; marks survive a refresh (vanished entries
  are pruned, and the status bar says how many), and copy, move, and delete
  operate on the whole selection, consuming the marks on submit. The pattern
  dialog is TUI-only for now — the GUI has no text input yet.
- **Provider attributes on the wire (proto 0.30.0, ADR 0039):** protocol-specific
  metadata — POSIX mode/uid/gid, an SFTP owner string, an S3 storage class, an
  archive member's packed size — can finally reach a client *typed* rather than
  pre-rendered, so a later block can paint it as a configurable column that still
  sorts and formats correctly. Four additive fields across three surfaces —
  catalog, request ×2, entry. `FsCapabilitiesResult.attrs` (an `AttrCatalog`)
  advertises what a provider offers (`AttrInfo` = `id`, `label`, `AttrType`,
  `AttrHint` — the declared type and the suggested format/alignment are separate,
  because two `Uint`s are painted very differently as a byte count and as a
  permission word), `FsListParams.attrs`/`FsStatParams.attrs` request the ids a
  client will actually paint (nothing is delivered unrequested), and `Entry.attrs`
  carries the values as `AttrValue` (`Uint | Int | Text | Bytes | TimeMs | Bool |
  Unknown`). Ids are namespaced by construction (at least one `.`, every segment
  starting with an ASCII letter and continuing in `[a-z0-9_-]`, ≤ 64 bytes — so
  neither the argv-shaped `-x.y` nor the float-shaped `0.0` is an id) and the
  caps — 16 requested ids per call, 64 advertised descriptors, 64-byte label,
  256-byte `Text`/`Bytes`, all counted in BYTES where the schema's `maxLength`
  counts code points — travel in the published JSON Schema (ADR 0038). The
  request surfaces are `fs.list`/`fs.stat` only: `search.hits` and
  `index.query` carry no attributes at 0.30. Wire-only for now: no provider
  advertises an attribute yet and the daemon ignores requested ids, which is a
  valid answer under the contract. `norte-proto` gains `base64` (0.22, already a vetted workspace dep) so
  `AttrValue::Bytes` owns its decode. Three properties are worth stating exactly:
  - All four fields are `skip_serializing_if`-guarded, so a **0.29 peer emits and
    receives byte-identical payloads**; the window becomes N=0.30.x / N-1=0.29.x.
  - **Any malformed attribute VALUE degrades to `AttrValue::Unknown`** — an
    unrecognised tag from a protocol-N+1 daemon (ADR 0004 applied at value
    granularity), a wrong JSON type, a `null`, two known tags at once, undecodable
    base64, an over-cap `Text`/`Bytes`. It costs one cell, never the entry and
    never the page.
  - **The two receive-side fields filter and never error**, while the two
    request fields deliberately do not. `Entry.attrs` drops a malformed key
    and bounds the map at 16 (smallest ids in byte order, so the surviving set
    does not depend on the peer's key order); `FsCapabilitiesResult.attrs` is an
    `AttrCatalog` — a newtype with a private field whose only constructor drops a
    malformed or repeated id keeping the first, clamps an over-long label on a
    char boundary, and truncates at 64, preserving the provider's own meaningful
    order. Making it a type rather than a call is what covers the EMBEDDED
    TUI/CLI path, which never crosses the deserialisation boundary where a plain
    filter would sit. A request keeps a bad id verbatim on purpose: the daemon
    must be able to answer `-32602` instead of silently laundering a caller's bug.
- **Protocol JSON Schema artifact (#13, ADR 0038):** `docs/schema/proto.schema.json`
  is now generated from the same `norte-proto` serde types that speak the wire,
  behind an optional `schema` cargo feature (off by default — the shipped crate
  gains no dependency at runtime). A golden test pins it byte-for-byte and a
  source scan guards that every schema-deriving type reaches the artifact, so it
  cannot silently drift. External clients (the MCP bridge, third-party tooling)
  get a machine-readable contract for the request/response/notification payloads.
  A standalone `just semver` recipe (cargo-semver-checks over the publishable
  crates) is staged for the gate once the binary and a release baseline exist.
- **Plugin previews mark lossy decoding (#101, proto 0.29.0):**
  `PluginPreview` and `PluginPreviewStyled` gain an additive `lossy: bool`.
  When the core's host-side text decoding (§6.2, #29) had to substitute `�`
  for invalid bytes, the viewer now shows a `[lossy decode]` marker next to
  the `via <plugin>` indicator — the same honesty the raw viewer already
  gives via its encoding status. Additive over 0.28.x (`#[serde(default)]`,
  so an N-1 peer reads it as `false`); the window becomes N=0.29.x /
  N-1=0.28.x.

- **Styled plugin previews, end-to-end (G3a):** `plugin.preview_styled`
  (proto 0.27.0, ADR 0037) is now wired from a real WASM guest through the
  daemon and both frontends. `Backend::plugin_preview_styled` (embedded:
  resolve → read → `render-styled`; remote: the wire call) mirrors
  `plugin_preview`'s resolve/read steps but treats ANY runtime failure in
  the styled render (a guest trap, a guest-side error, or a cap violation —
  `RuntimeError::StyledPreviewTooLarge`) as `Ok(None)` rather than an
  error — a styled preview is a pure enrichment over the plain one, so it
  must never block the file; the caller falls back to `plugin_preview`,
  which falls back to the raw view. The daemon handler
  (`handle_plugin_preview_styled`) mirrors that same fallback contract
  server-side. A pre-0.27 daemon is never reachable (handshake rejects it);
  within the 0.27 window a daemon that hasn't wired the handler yet answers
  `MethodNotFound` (-32601), which the remote client also folds into
  `Ok(None)`. `SpanWire::role` travels **unvalidated** across
  `norte-core` (it has no dependency on `norte-theme`, which owns the
  closed `Role` set) — validation happens once, at the frontend boundary
  that actually paints: `norte_frontend::viewer::Viewer::
  with_plugin_preview_styled` resolves each `role` string through the new
  `norte_theme::Role::from_kebab` (reuses the existing serde kebab-case
  derive as the single source of truth for role names, rather than a
  hand-duplicated table), collapsing an unrecognized name to `None` — never
  a panic, never a raw string leaking into a frontend's paint path. `role`
  wins over the raw `fg` fallback when a span carries both (the user's
  theme outranks a plugin's fixed color); every span's `text` is masked
  through the same `display_name` the ANSI-derived preview already used —
  `ansi::StyledSpan` grew a `role: Option<Role>` field shared by both
  preview paths (ANSI-SGR-derived and WIT-structured), so `draw_viewer`
  (TUI) and `render_viewer` (GUI) paint them with one code path. TUI/GUI
  viewer-open flows try the styled preview first and fall back to the
  plain one. TUI: per-span role resolves through `TuiTheme::role`
  (ratatui `Style`, falls back to raw RGB, then to the theme default). GUI:
  `styled_span_color` resolves role through `Theme::style(..).fg` (with G1
  glow applied on top, same as `entry_color`) or the raw `fg` (via
  `norte_theme::Color::rgb` + the existing `theme_map::to_gpui_rgba`, no
  parallel conversion), rendering a flex-row of per-span child divs — as a
  side effect, the GUI now also paints the pre-existing ANSI-derived
  preview in color (it shares the same `StyledSpan` type and render path),
  closing a gap noted in ADR 0037's context section. Covered by a real-WASM
  e2e (`previewer-demo`'s mini-highlighter: digits → `role: "number"`,
  `TODO`/`FIXME`/`norte` → `role: "keyword"` + a fixed `fg` — both are
  deliberately *not* valid `Role` names, proving the unvalidated-wire /
  validated-at-frontend boundary end to end) through `Backend::Remote`
  against a real daemon socket, plus a TUI buffer-inspection test pinning
  role-over-fg precedence and GUI unit tests for the pure color-resolution
  function.

- **Row decorators and plugin columns, host + backend + both frontends
  (G3b, ADR 0037):** `plugin.decorate`/`plugin.column_values` (wire-only
  since 0.27.0/G3a) now have a real handler and are painted end to end.
  New manifest `Category::Decorator` plus an (initially empty, additive)
  `contributions.decorator` — the digest follows the SAME optional-section
  pattern as `[config]`: a manifest without `[[contributions.decorator]]`
  digests byte-identical to before this change, so no existing human
  approval is invalidated by the mere existence of the new category.
  `PluginRegistry::resolve_decorators` returns **every** approved+enabled
  decorator plugin (unlike `resolve_previewer`'s first-match: multiple
  decorators can badge the same page); `resolve_columns(id)` resolves the
  one `columns` plugin declaring that column id. Both are gated on the
  primary `category` (decorator/columns each get their own dedicated WIT
  world, unlike previewer/command which share `norte-plugin`). The
  entries that cross to a guest are **basenames**, never full paths
  (`plugins::paths_to_basenames`) — a decorator/columns plugin sees a
  name, not where it lives in the tree. `Backend::plugin_decorate`/
  `plugin_column_values` (embedded + remote, daemon handlers
  `handle_plugin_decorate`/`handle_plugin_column_values` gated by the same
  read-gate as `fs.list`, extended to the whole batch) are fail-closed
  **per plugin**, never per batch: a plugin that fails to instantiate,
  traps, or breaks the positional 1:1 contract (checked by
  `decorations_to_wire_checked`/`column_values_checked`) is dropped from
  the result with a log warning — the rest of the page still paints.
  TUI: after a listing lands, a background fetch (mirroring the existing
  `Fill`/`StatProbe` one-in-flight pattern) decorates the loaded page and
  installs the result on `PaneState`; `entry_item` paints a badge span
  after the hostile-name-badge slot (role resolves through the theme,
  unstyled falls back to dim). GUI: the same fetch rides the existing
  `SessionCmd`/`SessionEvent` session channel (`Decorate`/`Decorated`,
  double guard on generation *and* dir); `render_row` becomes a flex row
  (name flex-grows and truncates, badge never does) and reuses
  `styled_span_color` for role resolution — `DecorationWire` carries no
  raw `fg`, so an unrecognized role derives a dim tone from the row's own
  color instead. Every badge is masked and capped to 8 chars *after*
  masking (`norte_frontend::sanitize_decoration`, shared by both
  frontends — the same module also flattens the wire's per-plugin overlay
  to one winning decoration per path, `merge_decorations`). Two new
  columns/decorator demo guests (`examples-wasm/decorator-demo`,
  `examples-wasm/columns-demo`) back a real-WASM e2e through
  `Backend::Remote` against a real daemon socket. **Scope note:** GUI/TUI
  column *cells* are not rendered in this change — `plugin.list`'s
  `PluginInfo` does not expose `contributions.columns` today, so a
  frontend has no wire-level way to discover which column ids exist
  without a further (additive) protocol change; that's follow-up work,
  tracked separately from this change's registry/wire/decorator-UI scope.

- **Plugin config on the wire, GUI palette + extension manager, column
  cells (G3c, ADR 0037, proto 0.28.0):** closes the two deferrals G3
  accumulated. `PluginInfo` gains `columns: Vec<PluginColumnInfo>`
  (id + masked header, additive, discovery for the column UI) and two new
  methods expose P2's `[config]` on the wire, which was host-only by
  design until now: `plugin.get_config` (schema + effective value
  together, `PluginConfigKeyWire`) and `plugin.set_config` (validates
  against the SAME schema `config.toml` uses —
  `norte_plugin_host::encode_wire_value` reuses the private
  `encode_override` validator, never a parallel path — before persisting;
  `PluginRegistry::set_config` re-resolves settings in memory so the very
  next `run_command`/`get_config` sees the new value without a fresh
  `discover`). `persist_plugin_setting_typed` fixes a latent gap in P2's
  write primitive: it writes the NATIVE TOML type (`bool`/`int`/`string`)
  the schema declares instead of always a string, which a later
  `resolve_settings` re-parse requires. `plugin.set_config` is gated to
  non-agent connections (same criterion as `plugin.set_approval`: a
  plugin's settings are user data, not something an agent edits on its
  own).
  TUI: the extension manager gains a `[config]` drill-down (Enter on a
  plugin fetches its schema and opens a panel; `bool`/`enum` cycle
  immediately, `string`/`int` open inline editing with client **and**
  server-side range validation) built on a new shared
  `norte_frontend::plugin_config::PluginConfigState`; the settings
  overlay's Plugins section drops the old "edit `config.toml` by hand"
  note and shows one row per plugin with declared settings, drilling into
  the same panel.
  GUI: `app.palette`/`app.extensions` join `crate::keymap::COMMANDS` — the
  shared presets already bound `ctrl+p`/`f12` to them, but the GUI's own
  keymap supplement had claimed `ctrl+p` for `task.prev` (a layer that
  outranks the preset), silently shadowing the binding; `task.prev` moves
  to `ctrl+b` to free it. The command palette (`palette_view.rs`, an
  overlay painted like the modal, same key-capture priority, "modal
  preempts palette" preserved by construction) and the extension manager
  (`extensions_view.rs`, a full-view swap like the settings view, with the
  same `[config]` drill-down as the TUI) are new. `norte_frontend::palette`
  hoists the TUI's `Row`/`plugin_rows`/`rows_for_context`/`first_chord`
  (pure, no `COMMANDS` coupling) so the GUI doesn't re-implement plugin-row
  masking and the `[extension]`-prefix anti-spoofing discipline from
  scratch; each frontend keeps its own `build_rows` (genuinely different
  `COMMANDS`/help-id sources, not incidental duplication). Column *cells*
  (the G3b GUI deferral) now render: a new `SessionCmd::Columns` discovers
  approved+enabled `columns` plugins via `plugin.list`, fetches
  `plugin.column_values` for every declared column over the visible page,
  and `render_row` appends one fixed-width, monospace, masked cell per
  column (TUI columns remain deferred, tracked separately). `norte-i18n`
  gains help text for the four GUI-only commands
  (`mark.toggle`/`task.next`/`task.prev`/`task.dismiss`) that the palette
  now needs to describe, and messages for the config drill-down's
  save/empty feedback, in both locales. Session tests, daemon wire tests
  (validation-then-`INVALID_PARAMS`-without-persisting, agent-denied),
  registry tests, a real-WASM e2e proving a guest reads a value written
  through `set_config` on its very next run (extends
  `plugins_config_e2e.rs`), and a `Backend::Remote`-level e2e for the new
  wrapper methods.

- **GUI settings view (S4):** `app.settings` (`F11`, same shared preset
  binding as the TUI) opens a searchable, VSCode-style full-view swap over
  the same General catalog (S2) — search box, grouped list (General/
  Plugins) with descriptions, mouse AND keyboard (click/hover to select,
  click cycles a bool/enum/theme/keymap-preset row or opens inline text/int
  editing; Enter/Esc mirror the TUI overlay). Writes persist off the UI
  thread through GPUI's background executor (no new OS thread, no coupling
  to the daemon session — config I/O has nothing to do with that
  connection's lifecycle) and, on success, re-resolve what the GUI can
  apply live from the freshly reloaded config: theme + `[effects]`, fonts
  (family/size — resolved once at startup until now, but cheap enough to
  redo on every write), reduce-motion, confirm-quit, quick-search (closes a
  pre-existing gap: the GUI had always hardcoded `Filter` mode, ignoring
  this setting entirely), and the keymap preset (rebuilds both resolvers).
  Only the UI language can't apply live in this frontend (Fluent negotiates
  it once at process startup) — that row carries a static "restart
  required" badge, and any write that couldn't apply live says so in the
  save confirmation. The pure editor state machine (search/cursor/inline
  edit, `SettingsState`) and row builder (`build_rows`) that power the S3
  TUI overlay moved to `norte_frontend::settings` unchanged (they had no
  ratatui/crossterm coupling to begin with) so both frontends share the
  exact same behavior instead of duplicating it; the TUI's own modules
  re-export the same names for source compatibility.

- **TUI settings overlay (S3):** `app.settings` (`F11` in all three bundled
  presets — `F9`/`F10`/`F12` were already taken) opens a searchable overlay
  over the General settings catalog (S2): type to filter by id, name, or
  description; Enter toggles a bool, cycles an enum/theme/keymap-preset
  setting immediately, or opens inline text/int editing (Int validates its
  range before writing — an invalid value shows a status-bar error and
  changes nothing). Every write goes through the same comment-preserving
  `norte_config::persist_set` as the theme picker, and the existing
  hot-reload picks it up live; the overlay stays open across a reload and
  refreshes its rows in place instead of closing, unlike the help/palette
  overlays. The Plugins section shows a single informational row for now:
  editing plugin settings from the UI needs a protocol bump the wire
  doesn't have yet (P2's `ConfigKeySpec`/`settings_of` aren't exposed to a
  remote frontend) — until then, edit `plugins/<id>/config.toml` by hand
  and validate with `norte doctor`.

- **`[ui] confirm_quit` (S2):** a new `norte.toml` setting controls whether
  quitting asks for confirmation — `"auto"` (default, unchanged behavior)
  confirms only with pending work, `"always"` always confirms even with
  nothing pending, and `"never"` closes immediately. Wired end to end in
  both frontends: the GUI's existing quit-confirmation modal now honors the
  three modes (and shows a generic title instead of "0 task(s), 0 mark(s)"
  when `"always"` fires with nothing pending), and the TUI gains its own
  confirmation modal on `app.quit` (Ctrl+C's emergency-exit shortcuts stay
  immediate everywhere, unaffected by this setting) — `"auto"` there
  confirms only when the task board has work in flight. A generic
  comment-preserving config writer (`norte_config::persist_set`) and a
  curated, Fluent-localized settings registry
  (`norte_frontend::settings::catalog`) land alongside it as the shared
  foundation the upcoming in-app settings UI (VSCode-style, searchable) will
  build on.

- **Per-directory cursor memory (S1):** both the TUI and the GUI now
  remember where the cursor was in each directory you visit this session
  (in-memory only, capped at 64 directories, byte-exact identity — hostile
  path twins are never folded together). Navigating to the parent directory
  now selects the folder you just came from, instead of always landing on
  the first entry.

- **GUI opt-in motion (G2, ADR 0036 amendment):** the `[effects]` theme
  schema grows to v1.1 with `flicker = { strength }` (CRT flicker, clamped
  to `[0.0, 0.15]` — deliberately tiny, an accessibility guard against
  photosensitive-trigger risk) and `cursor_blink` (bool); both render in the
  GUI. `fade_ms` (clamp `[0, 400]`) is parsed but not yet animated — a
  documented, deliberate schema/render split, not an oversight. The bundled
  `retro-crt`/`retro-crt-amber` presets now declare `flicker`+
  `cursor_blink` by default. A new `[ui] reduce_motion` config key (spec
  §17 a11y; last-wins across every layer including Project) forces all
  motion off via GPUI's native `App::set_reduce_motion`, which also frees
  `with_animation`-driven cursor blink from any hand-rolled reduce-motion
  check. The frame loop stays alive ONLY while a motion effect is active
  AND the window is focused AND `reduce_motion` is off — spot-checked with
  `NORTE_GUI_DEBUG`'s render counter (a focused retro-crt window renders
  continuously; the same theme under `reduce_motion = true`, or any theme
  with no motion keys, settles after the initial listing and goes fully
  event-driven, exactly as before this feature landed). This manual check
  is not yet pinned by an automated test — tracked as follow-up debt
  (no `gpui::test` harness exists in `norte-gui` yet to drive one).

- **Declarative per-plugin configuration (P2):** a plugin manifest can now
  declare typed settings under `[config.<key>]` (`string`/`bool`/`int`/`enum`,
  with an in-range default, an optional description, and per-type caps —
  ≤32 keys, key charset `[a-z0-9-]{1,32}`, ≤280-char strings/descriptions,
  ≤16 enum values). The schema is **inside the approval digest** (it decides
  what a plugin can be configured to do, same as `category`/`contributions`):
  a manifest with no `[config]` digests byte-identical to before P2 (existing
  human approvals are never reset), and any change to the schema — including
  just a default value — moves the digest and forces re-consent. Values live
  in `config_dir/plugins/<id>/config.toml` (flat `key = value`, validated
  fail-closed at discover time: an unknown key, a wrong TOML type, an
  out-of-range int, or a non-member enum value excludes the **whole plugin**
  from the catalog as a load error naming the offending key — never the
  value, #73) and are resolved to defaults-with-overrides applied.
  `norte doctor` gained a `plugin-config` finding per resolved key
  (`{id}: {key}={value}`, masked and capped like everything else untrusted in
  its report) for every plugin that declares `[config]`. Delivery to the
  sandboxed guest is a new WIT interface, `host-config` (`get`/`all`,
  package `norte:plugin@0.5.0`), linked for every plugin the same way
  `host-log` already is; values reach the guest through it for `command`,
  `previewer`, **and** provider guests alike, wired at every instantiation
  site (the embedded CLI/backend path, the daemon's `plugin.run_command`/
  `plugin.preview` handlers, and the previewer path in both). A provider
  guest (today, only FTP) never receives `[config]` — not an oversight:
  providers aren't discovered through the plugin manifest/catalog system at
  all, they're driven by `connections.toml`, a structurally separate config
  path with no `[config]` schema to resolve; the delivery plumbing
  (`PluginProvider::set_settings`) exists and is safe to call, ready for the
  day a provider *does* originate from a plugin manifest. The WIT package
  bump is **not** backward compatible with previously-compiled `.wasm`
  artifacts, verified empirically (not just by the pre-existing shared-package
  caveat in the WIT file's own history comment): instantiating a `command-demo`
  build from before the bump against the post-bump host fails outright
  (`component imports instance norte:plugin/host-log@0.4.0, but a matching
  implementation was not found in the linker`) — every precompiled guest,
  including the embedded `ftp-provider.wasm`, had to be rebuilt
  (`just build-ftp-wasm`). The extension manager's settings display is
  **deferred** to the wire (`PROTOCOL_VERSION`) bump G3 already requires:
  settings live host-side and the manager is wire-fed, so there is nothing to
  show there yet — `norte doctor` (which runs embedded) carries the display
  burden in the meantime.

- **`norte doctor` (H2):** read-only diagnostics over config layers,
  keymaps, plugins, and connections — `[config]` (parse errors per layer,
  and a split-brain warning when `NORTE_CONFIG_DIR` shadows a legacy dir
  with its own config files), `[keymap]` (structural errors — bad TOML,
  ambiguous prefixes, bad chords — vs. an honest per-screen approximation
  that downgrades an unrecognized layer command to a warning against the
  three bundled presets' own vocabulary), `[plugins]` (broken manifests,
  a plugin approved but whose capabilities digest went stale since —
  re-approval required — and a missing `plugin.wasm`), and `[connections]`
  (parse errors, invalid endpoints, and — side-effect-free v1 — whether the
  `NORTE_SECRET_<CONN>` env var a password/access-key connection falls back
  to is present; the OS keyring and `secrets.age` are explicitly NOT probed,
  since either could prompt or touch the keychain). `--json` emits a stable
  `{ findings, summary }` shape for tooling — locale-free and secret-free by
  construction: a `Finding`'s `detail` only ever carries machine values (ids,
  paths, var names), never the underlying library error's raw `Display`
  (a `connections.toml` syntax error inside a `password = "…` line is
  reported generically, never echoing the fragment); the handful of
  narrative sentences (e.g. "re-approval required") live in the text
  renderer, keyed by finding code, and are looked up in the user's locale.
  A layer's `lua:<name>` binding with an invalid Lua identifier is reported
  as a single structural error instead of retrying a fix that can never
  converge. Exit code is non-zero only when a finding is an error (a
  warning-only report still exits clean), and the full report always prints
  regardless of the exit code.

- **Command palette (H1, `Ctrl+P`; vim preset also `:`):** a filterable
  overlay lists every command with its Fluent description and its first
  bound chord (falling back from the browse to the viewer keymap); Enter
  dispatches the highlighted row through the exact same path a keypress
  would. Rows are precomputed from the effective keymap, like the F1 help
  and the dialog footer hints below, and refreshed on every hot-reload.

- **Plugin descriptions and commands on the wire, in the extension manager,
  and in the palette (P1, `PROTOCOL_VERSION` 0.26.0):** a plugin manifest can
  now declare an optional `description` (cosmetic, capped at 280 characters,
  outside the approval digest — editing it never resets an already-approved
  plugin's consent) and its `contributions.command` entries are exposed on
  `PluginInfo` alongside it. The extension manager (F12) shows the
  description as a dimmed second line under each plugin, masked and
  ellipsized like the rest of third-party text. The command palette
  (`Ctrl+P`) now fetches the plugin catalog on open and appends one row per
  command of every *approved and enabled* plugin, masked and tagged with an
  `[extension]` prefix that no built-in row can carry (a hostile plugin
  cannot spoof a built-in command by copying its exact display text); Enter
  runs it through `plugin.run_command` and shows the (masked, capped) result
  on the status bar. The row's internal dispatch key is never painted — a
  command id from the manifest has no charset validation of its own, unlike
  the plugin id.

- **Generated dialog footer hints (#24):** the confirm/collision/agent
  approval/host-key-trust modals and the theme picker, extension manager, and
  favorites popup now show a footer built from the *effective* `dialog`
  keymap — the join of the overlay's supported commands, the keys actually
  bound (preset plus any user layer), and a short label. Rebinding a dialog
  key can no longer desync its own hint. Notable key changes that came out of
  this: the collision dialog's "keep newer" moved from `n` to `w` — on
  collision, `n` (`dialog.deny`) is simply **inert**, not a cancel; it is not
  bound to anything the collision dialog listens for, it just no longer does
  "keep newer" by accident. The extension manager's approval toggle moved
  from a hardcoded `a` to `dialog.approve` (`y` in the bundled presets — `a`
  is now `dialog.add`, used by the favorites popup), and its `q`-to-close
  fallback was removed (`Esc` closes it, like every other overlay). The
  orthodox/cua presets also lost a hardcoded `k`/`j` fallback in the theme
  picker and extension manager: `k`/`j` now only navigate overlays under the
  **vim** preset, via its own `[dialog]` bindings, not as a blanket default.
  A later pass (encoding audit H1) found and fixed a masking gap: a hostile
  `./.norte/keymap.toml` project layer could bind a bidi-override or other
  hazardous codepoint to a supported dialog command, and that raw codepoint
  would reach the generated footer and the palette's chord column unmasked —
  both render sites now mask hazards the same way the query bar already did.
  A follow-up pass also fixed hint text that could get cut mid-word on an
  80-column overlay by dropping self-evident arrow/paging keys from
  non-modal hints and sizing the theme picker and extension manager boxes to
  their footer instead of a fixed width.

- **Content-match preview in live search (#81):** with a content search
  active, the status bar shows the line number and a sanitised preview of the
  match for the hit under the cursor.

- **Show names as… (#57):** `Alt+E` cycles a per-pane reinterpretation of
  non-UTF-8 file names for display (cp437, cp866, Shift-JIS, GBK,
  windows-1252), with a chardetng suggestion as the first step. Display only:
  bytes never change, reinterpreted names keep their hostile badge, and the
  status bar shows the active mode persistently. Valid UTF-8 names are never
  reinterpreted. Quick search matches against the reinterpreted text (typing
  "П" finds the entry shown as "Папка"), and decision surfaces — confirm and
  collision dialogs, viewer title, navigation popups, the search dialog root —
  follow the pane's active reinterpretation (#98).

- **Omitted-entries badge for archives (#93, protocol 0.22.0):** listings of
  zip/tar/tar.gz containers now report how many entries the index omitted
  (hostile names, anti-bomb limits) through the new optional
  `FsListResult.skipped` field. The TUI shows a persistent status-bar badge
  ("N entries omitted") and `norte ls` prints a warning to stderr — an
  incomplete listing is never silent.

- **Configurable archive limits (#95):** the new `[archive]` section of
  `norte.toml` (`max_entries`, `max_decompressed_bytes`) lowers the anti-bomb
  limits for browsing containers. The project layer (`./.norte`) is ignored
  for this section — a foreign repository must not be able to raise safety
  limits.

- **Themes (MT milestone, ADR 0020):** the TUI now uses the shared
  `norte-theme` crate. It provides semantic roles, true-colour values with
  256- and 16-colour terminal fallbacks, styles by node type and extension, and
  bundled presets (`default`, `catppuccin-mocha`, `gruvbox-dark`, and `nord`).
  Select a preset or a custom TOML file with `[ui].theme`. The setting is hot
  reloaded and falls back to `default` on error. An `[effects]` section is
  reserved for the GPU-backed GUI. See [the theme guide](docs/theming.md).
- **Light themes and explicit backgrounds:** added `gruvbox-light` and
  `catppuccin-latte`, plus a `background` role so both light and dark themes
  control the terminal's base colour.
- **Theme picker:** press `F9` to preview bundled themes. Enter applies and
  saves the choice to the user's `norte.toml` without discarding comments or
  formatting; Esc restores the previous theme.
- **Roadmap update:** after M2, the planned order became MT (themes), M4
  (plugins), M3 (agent integration), and M5 (GUI).

### Changed

- **Streaming prefix rename on object storage (#49):** renaming an S3 prefix
  no longer materialises the whole tree in memory (peak is now proportional
  to the number of directories) and deletes in batches (`DeleteObjects`).
  The operation stays non-atomic: an object created concurrently under the
  source prefix during the rename is left unmoved; a concurrent overwrite of
  an already-copied object can be lost.

- **Honest resource errors for archives (#95, protocol 0.23.0):** a container
  that exceeds a local anti-bomb limit now fails with the new `limit_exceeded`
  error (closed vocabulary: `entries`, `decompressed-bytes`) instead of
  masquerading as `corrupt` — a legitimate huge tar.gz is not "corrupt".
  Older clients degrade to a generic error.

- **Listing sort keys allocate less (#94):** the persisted NFC sort key is
  only materialised when it differs from the raw name bytes (non-ASCII NFD
  names); ASCII, already-NFC, and non-UTF-8 names no longer allocate.

### Added

- **`norte tui` and `norte gui`:** the CLI now launches either frontend,
  handing the process over (unix `exec`: same pid, same terminal, same
  signals) and preferring the binary installed next to itself over
  whatever the `PATH` finds first. Arguments pass through verbatim.

- **`norte-gui` takes a starting directory** too, plus `--socket`,
  `--help` and `--version`. Command line beats `NORTE_DIR`/`NORTE_SOCKET`,
  which beat the current directory and the daemon's own socket — resolved
  in one place (`LoadConfig::resolve`), with no `set_var` detour. The
  binary also stops reporting version `0.0.0`.

- **`norte-tui` takes a starting directory** and real `--help`/
  `--version`. The positional argument used to be the keymap preset,
  which nobody guessed; it is now the directory to open, with the preset
  behind `--preset`. An unknown flag is named and refused instead of
  being silently ignored — `--help` used to fall into that branch and
  the binary died trying to take over a terminal.

### Fixed

- **The F1 help no longer disappears, and neither do the columns
  (bugfixing session):** seven defects that only showed up by driving
  the real app.
  - Overlays are painted over the viewer. `ui::draw` returned right
    after the viewer, so any overlay opened on top of it stayed
    invisible while still swallowing every keystroke — the run loop
    routes them before the viewer, and `f1 -> app.help` is a `[global]`
    binding, so it is live on the viewer screen too.
  - An in-flight modal wins the key over every overlay, not just the
    palette and the settings pane. The modal is painted last, above
    everything, but the key chain resolved first against the theme
    picker, columns picker, extension manager, nav popup, search dialog
    and help — so a keystroke aimed at the modal landed in a text field
    or toggled the highlighted plugin.
  - The transfer-name modal elides its paths in the middle instead of
    letting the box border cut them, which used to expel the tail of the
    destination with nothing to signal it.
  - A cursor at the top stays at the top while a paginated listing
    fills. It was re-anchored to the path under it, and the first page
    of a local listing arrives in readdir order, so a 5000-file
    directory opened showing its tail.
  - Size and Date are hydrated for every visible row in both panes
    (TUI and GUI, #123), not just the focused one — with the lazy local
    listing (#52) those columns were otherwise blank.
  - Paging and the stat probe use the real viewport height (#124)
    instead of a fixed 10 rows and a fixed radius.
  - Config hot-reload only fires for `norte.toml`/`keymap.toml`/
    `openers.toml`. The native watcher can only watch a directory and
    forwarded every event, so `index.db`/`journal.db` writes — SQLite,
    in the same directory — reloaded the config and closed the open help
    and palette. Read events (`Access`) are ignored too: the reload
    re-reads the layers, so counting an open as a change fed the cycle.
  - Text inherits the THEME's foreground. Only the background came from
    the theme, so every span without an explicit `fg` (including the
    column cells and header, painted with a bare `DIM`) kept the
    TERMINAL's foreground: with a light theme in a dark terminal they
    were painted almost in the background color. Two tests over every
    shipped preset now pin this and a 3:1 floor for text.

- **Semantic signals reach WCAG AA in every preset:** `error`, `warning`
  and `hostile-badge` — the last one marks a masked name (spec §6), a
  security surface — fell as low as 2.31:1 against their own theme
  background. Adjusted in `catppuccin-latte`, `gruvbox-light`, `nord`
  and `gruvbox-dark`, keeping each palette's hue, and pinned by a test
  over all presets. Decorative roles (borders, status bar accents) keep
  their looks and their lower floor.

- **Dialog keys are now discoverable in-app (#113):** the F1 help gains a
  "Dialogs and overlays" section built from the effective `dialog` keymap —
  the same generated-from-config invariant as the other sections. Overlay
  footers stay space-filtered (arrows and, in the columns picker, the
  reorder verbs are dropped to fit 80 columns), but every dialog verb and
  its real chord is now listed somewhere reachable without opening the
  manual; a note clarifies each dialog supports its own subset.
- **Config writes are now atomic and cross-process safe (#116):** every
  `norte.toml` persist helper (theme, settings, columns, formats, hotlist)
  used to read-modify-write the file in place — two writers (e.g. the GUI
  and the TUI on the same config) could interleave and silently drop each
  other's changes, and a concurrent reader could catch a truncated file
  whose partial parse a later write would then rewrite, losing unrelated
  user sections. Writers now take a cross-process advisory lock
  (`norte.toml.lock`, held for the whole read-modify-write; released by
  the OS even on crash) and replace the file via a synced sibling tmp +
  atomic rename, preserving existing file permissions — readers see the
  old or the new file, never a torn one. Per-plugin `config.toml` writes
  got the same treatment (#119).
- **Ctrl+R skipped the post-refresh ritual (#118):** `pane.refresh` ran from
  the command dispatcher, which cannot see the run loop's paginated fill or
  the stat-probe dedup — with a large listing still streaming in, the old
  drainer kept appending batches onto the freshly refreshed pane
  (duplicated rows until the next cd) and the focused entry could refuse to
  re-hydrate its lazified size. The refresh outcome now travels back to the
  run loop (`Cd::Refreshed`), which applies the same ritual as
  mutation-completed refreshes: the drainer is released only for panes that
  truly got a complete listing (an Esc mid-refresh keeps the other pane's
  still-valid fill), and the probe dedup is invalidated.
- **Palette and quick-search dispatch sites missed the resolver's
  post-command tail (#118 review):** a cd chosen from the command palette
  (or a quick-search Enter landing in a hit directory) could exit a pane's
  virtual search mode without cancelling the live search task, and a
  `pane.open` picked from the palette left the resolved external command
  queued until the next keypress. Both sites now reap the search run; the
  palette also launches the pending opener immediately.
- **Daemon plugin previews skipped host-side text decoding (#101):** the
  `plugin.preview`/`plugin.preview_styled` daemon handlers passed RAW bytes
  to the previewer guest, unlike the embedded backend, which decodes to text
  first (§6.2, #29) — a latent behavior gap between embedded and daemon
  mode. Both handlers now decode through the shared
  `plugins::decode_for_preview`, matching embedded and surfacing the new
  `lossy` flag.
- **`norte doctor`'s keymap check used an O(n) retry loop (#102):** unknown
  `run` names were discovered by rebuilding the effective keymap once per
  distinct typo (capped at 256), a loop that could never converge for a
  `lua:<name>` binding failing the charset and had to special-case it. It is
  replaced by a single-pass `Effective::build_diagnostics` that reports every
  finding at once — no retry, no cap, no non-convergent case.
- **`persist_set` panicked on a malformed `[section]` (S review I1):** a
  hand-edited `norte.toml` with a scalar section (`ui = 3`) or an
  array-of-tables (`[[ui]]`) made the settings-write primitive panic instead
  of returning an error — reachable in both frontends' background write
  task (GUI: could take the process down; TUI: the panic was swallowed
  silently, leaving the settings row optimistically showing "edited" even
  though nothing was written). Now a clean `io::ErrorKind::InvalidData`; the
  TUI's previously-silent panic arm now shows a status message too.
- **Settings paint order (S review M3):** a modal (e.g. an async policy
  approval) painted UNDER the TUI's command palette or settings overlay
  when both were open, even though key input already treated the modal as
  authoritative — the pixels lied about who was in control. The modal now
  paints last, on top of every overlay.
- **`nav.parent`'s cursor-memory hint could survive a failed `cd` (S review
  M2):** landing back on the child you came from only worked after a
  *successful* navigation; a failed one (permission denied, a dead session)
  left the hint set, ready to hijack an unrelated future navigation's
  cursor placement. Both frontends now clear it on failure.
- **`ui.font-size` couldn't be edited to a fractional value (S review M4):**
  the settings UI's `Int` editor only accepted whole numbers, even though
  `[ui] font_size` is a float — a hand-set `14.5` was invisible to the
  editor (typing it back always failed). It now accepts a fractional part
  and round-trips it.
- **Silent short reads from zip entries (#95):** a zip whose central directory
  promises more bytes than the deflate stream delivers now fails loudly with
  `corrupt` mid-stream instead of silently returning a partial file.

## [0.3.0-alpha.1] - 2026-07-15

The first tagged release completes the M2 milestone: remote providers and
archives. norte can manage local files, remote storage, and compressed archives.
This is an alpha release; the interface and configuration may still change,
and some daemon/socket tests are only available in CI environments.

### Added

#### M2: remote providers and archives

- SFTP provider based on `russh`, with accurate capabilities, byte-safe names,
  and containment for hostile names and symlinks.
- Object-storage provider based on `opendal`, with server-side S3 copies,
  cursor pagination for large listings, and byte-exact UTF-8 keys.
- Read-only ZIP and TAR provider that exposes archives as virtual directories
  (`zip+...!/path`), honours ZIP filename encoding, and enforces zip-bomb
  limits.
- Cross-provider copy engine with resumable `.norte-partial` files, multipart
  S3 support, and destination-side overwrite protection.
- Remote logical trash at `.norte-trash/` for providers without an operating
  system trash facility, including byte-safe origin metadata.
- JSON-RPC 2.0 daemon over Unix-domain sockets or Windows named pipes, with
  peer-credential authentication, NDJSON framing, automatic startup, and idle
  shutdown. Frontends can use embedded or daemon mode.
- Connection profiles and secret handling through `connections.toml`, system
  keyrings, and trust on first use for SSH host keys.
- End-to-end coverage of the release criterion (remote ZIP to S3 to local),
  framing and ZIP-name fuzzing, copy benchmarks, and nightly tests using
  testcontainers.

#### M1: usable terminal interface

- A ratatui dual-pane TUI, configurable keymaps, layered hot-reloaded
  configuration, an encoding-aware viewer, and explicit fallback when trash is
  unavailable.
- Fluent localization resources for English and Spanish.

#### M0: foundation

- Cargo workspace, protocol and VFS crates, byte-preserving `VPath`, cancellable
  task scheduling, local copy/move/delete with progress, and CI on three
  operating systems.

### Notes

- Filenames remain bytes throughout the stack. The canonical `norte-testkit`
  corpus covers hostile and non-UTF-8 paths.
- `norte-proto`, `norte-vfs*`, and `norte-testkit` are available under either
  Apache-2.0 or MIT. `norte-core` and the official frontends are
  AGPL-3.0-only.

### Planned

- Agent-facing MCP server, policy engine, journal, session undo, and audit
  export.
- Writes inside ZIP archives; list, restore, and purge operations for logical
  trash; and the M5 GUI.

[Unreleased]: https://github.com/compilando/norte/compare/v0.3.0-alpha.2...HEAD
[0.3.0-alpha.2]: https://github.com/compilando/norte/compare/v0.3.0-alpha.1...v0.3.0-alpha.2
[0.3.0-alpha.1]: https://github.com/compilando/norte/releases/tag/v0.3.0-alpha.1
