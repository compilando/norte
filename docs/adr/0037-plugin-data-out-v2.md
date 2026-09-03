# 0037 - Plugin data-out v2: styled previews, decorators, columns

- Status: accepted
- Date: 2026-07-24
- Decision makers: Oscar González
- Related: ADR 0022 (WASM plugin host, manifest, capabilities, and extension
  levels); ADR 0032 (plugin provider interface, WIT projection of `Provider`);
  ADR 0020 (shared semantic themes and terminal colour fallback); ADR 0036
  (GUI effects schema v1); design spec
  `docs/superpowers/specs/2026-07-23-gui-visual-plugins-design.md` (phase G3,
  lines 161-187); plan
  `docs/superpowers/plans/2026-07-24-g3-data-out-wit-v2.md`

## Context

Plugins today speak to the host only in flat strings. `plugin.preview`
(M4-P5, proto 0.15.0) returns one `output: String` per file; `plugin.
run_command` (M4-P4) returns one `output: String` per invocation. Any richer
render decision — syntax highlighting, git-style badges, or per-column
values — would today require the PLUGIN itself to paint (ANSI escapes, raw
color codes embedded in text), which violates the governing principle phase
G locked: **a visual plugin is data the host interprets, never third-party
code that paints** (the same principle ADR 0036 built the GUI `[effects]`
schema on).

The TUI already proves the paintable end of this pattern works, but through
a side channel that bypasses norte's plugin protocol boundary entirely:
`Viewer::with_plugin_preview` parses ANSI SGR escapes out of the PLAIN
`plugin.preview` string (`norte-frontend/src/viewer.rs`, `ansi.rs:18-48`) to
build `StyledLine`/`StyledSpan{text, fg}`, which `draw_viewer` turns into
per-span ratatui `Span`s (`ui.rs:531-549`). The GUI has no equivalent and
renders the same preview flat — one `SharedString` per row (`main.rs:1643`).
Neither frontend has ANY typed channel for row decorations (a per-entry
git-status badge) or computed columns (e.g. a plugin-provided "git blame" or
"checksum" column); those two manifest categories have existed since M4 with
no WIT surface backing them at all.

Spec phase G3 asks for three data-out surfaces the HOST paints from
structured data — styled preview spans, row decorations, and real
per-plugin columns — plus GUI parity for the command palette and extension
manager. The palette/manager work (spec phases G3c) is pure frontend
scheduling with no wire or WIT surface and is out of scope for this ADR;
this record covers the three protocol/WIT decisions (G3 plan decisions 1-3).

## Decision

### 1. Wire shape: three new methods, folded into ONE 0.27.0 bump

Add three JSON-RPC methods rather than flags on the existing ones:

- **`plugin.preview_styled`** — the styled twin of `plugin.preview`. Mirrors
  its all-or-nothing shape exactly: `PluginPreviewStyledResult
  { #[serde(flatten)] preview: Option<PluginPreviewStyled> }`, where
  `PluginPreviewStyled { plugin_id, plugin_name, lines: Vec<Vec<SpanWire>> }`
  and `SpanWire { text: String, role: Option<String>, fg: Option<[u8; 3]> }`.
- **`plugin.decorate`** — batched, POSITIONAL 1:1 with `params.paths`:
  `PluginDecorateParams { paths: Vec<VPath> }` →
  `PluginDecorateResult { plugins: Vec<PluginDecorations
  { plugin_id: String, decorations: Vec<DecorationWire
  { badge: Option<String>, role: Option<String> }> }> }`.
- **`plugin.column_values`** — also positional 1:1:
  `PluginColumnValuesParams { column_id: String, paths: Vec<VPath> }` →
  `PluginColumnValuesResult { values: Vec<Option<String>> }`. Each cell is
  `Option<String>`, not `String`, for the same reason as
  `DecorationWire::badge`: a column that does not apply to a given entry
  (e.g. a "duration" column on a non-media file) needs a way to say so that
  is distinguishable from a real, legitimately empty-string value —
  `None` is "no cell here", never omitted from the positional vector.

New methods, not a flag, because the all-or-nothing preview shape stays
simple (no partial-styling state to reason about at the type level, same
argument that shaped `PluginPreviewResult` in M4-P5), and — within the
window where these methods are actually reachable (see the accurate
fallback story in Consequences below: an old, pre-0.27 daemon is never in
that window at all, `version_compatible` rejects the handshake before any
method call happens) — an unanswered call still falls back cleanly to plain
`plugin.preview` via `MethodNotFound`, with no flag-negotiation state
machine on either side. `plugin.decorate` and
`plugin.column_values` were originally scoped for a LATER bump (the G3 plan
had them as phase G3b, Task 4), but they were foreseeable at ADR time, so
they fold into this SAME 0.27.0 bump rather than forcing a second
protocol-version churn for the same milestone — the wire lands now, the host
behavior that answers it lands in Task 4.

Caps enforced on the wire (server enforces before sending; a client
re-validates and falls back fail-closed to the plain/undecorated path on any
violation, never trusting a daemon blindly):

| Limit | Value |
| --- | --- |
| Lines per styled preview | ≤ 10,000 |
| Spans per line | ≤ 64 |
| Span text length | ≤ 4 KiB |
| Total styled-preview payload | ≤ 4 MiB (reuses the existing plugin runtime return cap, spec:211) |
| Badge length, POST-masking | ≤ 8 chars |
| `decorations` / `values` | POSITIONAL 1:1 with the input `paths` — never keyed, never reordered, never sparse |

`role` names are validated HOST-SIDE against `norte_theme::Role`'s closed
set (`crates/norte-theme/src/role.rs`). An unknown role string degrades to
`None` (unstyled) plus a warning — never a hard error, matching ADR 0020's
established lenient-theme-data precedent (a newer plugin, or one from a
different norte fork, must not break rendering). This validation applies
wherever `role: Option<String>` crosses the wire (`SpanWire`,
`DecorationWire`): the HOST validates when it PRODUCES the field from a WIT
guest's output (decision 2), and, defensively, a REMOTE client re-validates
on receipt from a daemon it does not fully trust — an unrecognized role
string is `None` on both ends, never forwarded as a literal string for a
frontend to interpret unchecked.

### 2. WIT 0.6.0

`norte:plugin@0.5.0` bumps to `0.6.0`
(`crates/norte-plugin-host/wit/norte-plugin.wit`):

- The `previewer` interface's `render` gains a sibling REQUIRED export,
  `render-styled: func(input: preview-input) -> result<styled-text, string>`,
  where `record span { text: string, role: option<string>, fg:
  option<tuple<u8, u8, u8>> }` and `styled-text = list<list<span>>` (a list
  of lines, each a list of spans — the WIT mirror of `Vec<Vec<SpanWire>>`).
  It is REQUIRED, not optional, because a WIT world's exports are
  all-or-nothing per interface: a guest cannot conditionally implement half
  of `previewer`. A plain-only guest satisfies it trivially by implementing
  `render-styled` as a single-span-per-line wrapper around its own existing
  `render`.
- Two new worlds, `norte-decorator` and `norte-columns`, mirroring
  `norte-provider`'s established pattern (its own world, its own interface,
  resolved by manifest `category` rather than folded into the shared
  `norte-plugin` world): `interface decorator { decorate: func(entries:
  list<list<u8>>) -> list<decoration> }` with `record decoration { badge:
  option<string>, role: option<string> }`, called batched per visible page
  and positional 1:1 with the input entries; `interface columns {
  column-values: func(id: string, entries: list<list<u8>>) -> list<option<string>>
  }`.

**Consequence carried forward from the P2 lesson**, quoted verbatim from the
WIT header (`norte-plugin.wit:36-44`, confirmed empirically for the
0.4.0→0.5.0 bump):

> Es decir, el guest viejo pide `host-log@0.4.0` (el nombre versionado que
> llevaba grabado al compilar) y el linker del host 0.5.0 solo ofrece
> `host-log@0.5.0` — rompe por el LADO DE IMPORT, no solo el de export como
> advertía el análisis original. Confirma la deuda del paquete compartido
> (ADR 0032): CUALQUIER bump de este `package` invalida TODOS los artefactos
> `.wasm` ya compilados, aditivo o no en su contenido textual.

In English: any bump of this shared WIT `package` invalidates ALL
precompiled guest `.wasm` artifacts, additive or not in textual content,
because the package version travels inside every VERSIONED INTERFACE NAME
(`norte:plugin/previewer@0.5.0` → `@0.6.0`) and a guest compiled against the
old name fails to instantiate against a linker that only offers the new
one. This is not a theoretical risk deferred to "when third-party plugins
exist" — it is immediate and mechanical inside this repo too. T2 of the plan
must rebuild EVERY in-repo guest (`previewer-demo`, `command-demo`) and
re-run `just build-ftp-wasm` for the FTP provider plugin (ADR 0033) as part
of the SAME change that lands `@0.6.0`, or their prebuilt artifacts stop
instantiating the moment the host starts linking against the new package
version.

### 3. Host paint

Not itself a wire or WIT decision, but locked alongside 1-2 because it
constrains what the wire shape must be able to carry: `role` resolves
through each frontend's OWN theme seam (TUI `theme.role`, GUI
`ChromeColors`/entry seam — ADR 0020's pair-honest fg+bg idiom, never a
frontend painting a raw hex it invented). A raw `fg: [u8; 3]` is a validated
RGB fallback for roleless spans (e.g. a highlighter with its own fixed
palette); when a span carries both, `role` wins — the user's theme has
precedence over a plugin's fixed color choice. GUI-side glow (ADR 0036 §4,
`lerp(fg, white, strength * 0.25)`) applies on top of a role-resolved color
when the active theme sets `[effects].glow`. EVERY span's `text` is masked
through the SAME per-span masking `Viewer::with_plugin_preview` already
applies on the ANSI-derived path — extended to cover this structured twin,
never reimplemented in parallel. Badges paint as an extra `Span` in the TUI
list item (in-band, the existing hostile-name-badge convention) and as a
role-colored child element in the GUI row.

## Consequences

- **The fallback contract is mandatory, not best-effort — and it has TWO
  distinct triggers that must not be conflated.** A 0.27 client can never
  even reach a pre-0.27 (0.26 or older) daemon with these methods:
  `version_compatible` (`crates/norte-proto/src/methods.rs`) accepts only N
  or N-1 on the SERVER side and rejects a client whose minor is NEWER than
  the server's — `initialize` fails with `VERSION_MISMATCH` (-32001) before
  a single RPC call is attempted, so a pre-0.27 daemon is out of the picture
  entirely, not a `MethodNotFound` case. `MethodNotFound` (the existing
  "unknown method" taxonomy, ADR 0004; no new error variant needed) is the
  real trigger inside the SAME 0.27 window: a 0.27 daemon that has not yet
  wired the handler for one of these methods (this ADR's bump is wire-only;
  T3/T4 land the daemon-side implementation), or a client that inspects
  `InitializeResult::protocol_version` and chooses not to call a method it
  isn't confident the daemon answers yet. Both triggers land the client in
  the same place — plain `plugin.preview` / no decorations / no columns —
  so the OBSERVABLE fallback behavior described elsewhere in this ADR is
  correct; only the mechanism differs, and getting the mechanism right
  matters for anyone debugging why a call failed. An old (pre-0.27) client
  talking to a NEW daemon simply never calls the new methods and observes no
  behavior change, independent of both triggers above.
- **The WIT rebuild story is load-bearing for THIS repo right now**, not
  just a documented future risk: T2 must rebuild `previewer-demo`,
  `command-demo`, and `ftp-provider.wasm` against `norte:plugin@0.6.0` in the
  SAME change that bumps the package, or the in-repo demo guests are
  permanently broken until someone notices.
- The protocol version window shifts to N/N-1 = 0.27.x/0.26.x. The recurring
  N-1-literal trap — hardcoded old-version strings in
  `crates/norte-core/tests/daemon.rs` and `crates/norte-proto/tests/types.rs`
  asserting acceptance/rejection at the version boundary — is fixed
  proactively as part of this bump rather than left for a future author to
  rediscover the hard way.
- `plugin.decorate` and `plugin.column_values` land with WIRE support in
  0.27.0 but no HOST implementation yet — registry resolution by manifest
  category, the listing-pipeline batching that calls `decorate` after a page
  renders, and TUI/GUI rendering are the G3 plan's Task 4. Landing the wire
  shape now and the behavior later is intentional: it avoids a second
  protocol bump for the same conceptual surface, at the cost of a brief
  window where the methods exist on the wire with no daemon handler
  answering them yet (tracked by the plan, not a new issue).
- This ADR formalizes decisions 1-3 of the G3 plan (wire shape, WIT bump,
  host-paint contract). Decisions 4-5 of that plan — GUI palette/extension
  manager parity and the sub-phasing schedule (G3a/G3b/G3c) — are
  implementation scheduling with no protocol or WIT surface, and are
  deliberately not recorded here.

## Amendment log

- **2026-09-03 (demo D3, `org.norte.markdown`): an exact mimetype beats a
  glob.** `resolve_previewer` chose the first consented previewer in
  catalogue order whose declaration matched, so with `org.norte.markdown`
  (`text/markdown`) and `org.norte.syntect` (`text/*`) both installed, who
  painted a `.md` depended on the alphabet of the ids. Now an exact
  declaration wins over a wildcard wherever it sits; among equals, catalogue
  order still decides. The host also learned `text/markdown` for `.md` and
  `.markdown` — until then they were `text/plain`, and no previewer could
  claim Markdown without claiming all text.

G3's Task 4 (decorator/column HOST implementation) and Task 5 (GUI
palette/manager) closed out the plan without needing a wire or WIT change
beyond what this ADR locked.

## Addendum (2026-08-16, ADR 0057): `column-values` changed shape

`columns::column-values` now takes a `location: option<location-ref>` —
the opaque token of the directory being listed plus the prefix the user is
looking at — and the package moved to `norte:plugin@0.8.0`.

The basename-only contract this ADR chose is **not** reversed: a guest still
never receives a path. What ADR 0057 adds is a handle, gated by its own
capability, so a column can say something about the file instead of only about
its name. Everything else here — the positional 1:1 contract, `none` meaning
"does not apply", the host-side caps on returned bytes — is unchanged.
