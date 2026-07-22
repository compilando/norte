# Architecture

This is a one-page map of the repository. The full design is in the
[project specification](docs/spec/norte-spec.md), and significant decisions
are recorded as [ADRs](docs/adr/README.md).

## System overview

norte is an orthodox file manager with a headless Rust core. The core exposes a
JSON-RPC protocol and a provider-independent virtual filesystem. The TUI, GUI,
CLI, and agent integrations are clients of that core.

Frontends contain no business logic. If an operation is not available through
the protocol, a frontend cannot provide it.

## Workspace crates

| Crate | Responsibility | License |
| --- | --- | --- |
| `norte-proto` | Serializable wire types. Any change is a protocol change and requires updated golden tests, a version bump, and an additional review. | MIT OR Apache-2.0 |
| `norte-vfs` | The central `Provider` contract, `VPath`, streams, capabilities, and the shared provider conformance suite. | MIT OR Apache-2.0 |
| `norte-vfs-local` | Platform-specific local-filesystem provider. This is the only crate where `unsafe` may be used, and every use requires a `// SAFETY:` explanation and a test. | MIT OR Apache-2.0 |
| `norte-vfs-sftp` | SFTP provider, including hostile-server containment and byte-safe names. | MIT OR Apache-2.0 |
| `norte-vfs-object` | Object-storage provider, with S3 as the first supported backend. | MIT OR Apache-2.0 |
| `norte-vfs-archive` | Read-only ZIP and TAR provider. | MIT OR Apache-2.0 |
| `norte-testkit` | Deterministic `MemProvider`, injectable failures, hostile fixtures, and proptest strategies. | MIT OR Apache-2.0 |
| `norte-core` | Task scheduling, transfers, sessions, policy enforcement, journaling, and the daemon. | AGPL-3.0-only |
| `norte-plugin-host` | WASM plugin manifests, capabilities, catalogue, and runtime. | AGPL-3.0-only |
| `norte-ai` | Model-provider abstraction (`AiProvider`) and implementations: Anthropic, Ollama, OpenAI-compatible. | AGPL-3.0-only |
| `norte-cli` | A command-line client and manual core test bed. | AGPL-3.0-only |
| `norte-tui` | The ratatui dual-pane terminal frontend. | AGPL-3.0-only |
| `norte-gui` | The GPUI graphical frontend. | AGPL-3.0-only |
| `norte-frontend` | UI-independent state and behaviour shared by the TUI and GUI. | MIT OR Apache-2.0 |
| `norte-encoding` | Text encoding detection and decoding. | MIT OR Apache-2.0 |
| `norte-i18n` | Fluent localization resources shared by the frontends. | MIT OR Apache-2.0 |
| `norte-theme` | Semantic theme roles, true-colour values, terminal fallbacks, and bundled presets. | MIT OR Apache-2.0 |

Other subsystems include `norte-index` and `norte-mcp`.

## Dependency rules

The main dependency direction is:

```text
proto  <-  vfs  <-  { providers, testkit, core }  <-  clients
```

- Frontends use `norte-proto` and shared presentation crates. Embedded mode may
  also use `norte-core`.
- VFS providers do not depend on or call one another.
- `norte-testkit` is a development dependency, never a runtime dependency.
- Agent and plugin operations always pass through the core and policy engine.

These boundaries are enforced through workspace configuration, `cargo-deny`,
tests, and review.

## Non-negotiable invariants

1. Filenames are bytes. `VPath` preserves them; UTF-8 conversion is only for
   display and must make lossy conversion visible.
2. Blocking I/O never runs directly in an async context. Local filesystem work
   uses `spawn_blocking` or helpers from `norte-vfs-local`.
3. Every long-running operation is a task whose inner loop checks its
   `CancellationToken`.
4. Cancellation leaves either a clean destination or a marked
   `.norte-partial` file, never an unmarked partial result.
5. Errors follow the protocol taxonomy. Frontends render error categories
   instead of parsing messages.
6. Every mutation is journalled and carries enough information to undo it, or
   is explicitly marked irreversible with a reason.

## Repository guide

- Development commands: `justfile`
- Architecture decisions: `docs/adr/`
- Protocol schemas: `docs/schema/`
- Hostile test corpus: `crates/norte-testkit/fixtures/`
- Claude Code project configuration: `.claude/` and `CLAUDE.md`
