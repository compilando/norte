# Changelog

All notable changes to norte are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and releases follow
[Semantic Versioning](https://semver.org/). The wire protocol is versioned
independently through `PROTOCOL_VERSION`.

## [Unreleased]

### Added

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
