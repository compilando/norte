# Changelog

All notable changes to norte are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and releases follow
[Semantic Versioning](https://semver.org/). The wire protocol is versioned
independently through `PROTOCOL_VERSION`.

## [Unreleased]

### Added

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

### Fixed

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

[Unreleased]: https://github.com/compilando/norte/compare/v0.3.0-alpha.1...HEAD
[0.3.0-alpha.1]: https://github.com/compilando/norte/releases/tag/v0.3.0-alpha.1
