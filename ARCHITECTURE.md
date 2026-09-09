# Architecture

This is a one-page map of the repository. The full design is in the
[project specification](docs/spec/norte-spec.md), and significant decisions
are recorded as [ADRs](docs/adr/README.md).

## System overview

norte is an orthodox file manager with a headless Rust core. The core exposes a
JSON-RPC protocol and a provider-independent virtual filesystem. The TUI,
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
| `norte-vfs-rar` | Read-only RAR provider; delegates to an installed `7z`/`unrar`. | MIT OR Apache-2.0 |
| `norte-index` | SQLite FTS5 name/metadata search index (ADR 0034). Raw-bytes path authority + lossy-UTF-8 matching. Content/embeddings/tags are future work. | AGPL-3.0-only |
| `norte-compare` | Directory comparison engine (ADR 0048): pairing key, the cheap-to-expensive criterion cascade, and the streamed rows. A pure function of two `Provider`s — it knows nothing of the daemon, the scheduler or the policy gate, and mutates nothing. | AGPL-3.0-only |
| `norte-sync` | One-way synchronisation planner (ADR 0049): a transducer from `norte-compare`'s rows to the plan's steps and blockers. It touches no provider — the two capability answers it needs arrive already resolved in its options — which is what makes the whole matrix of step kinds, modes and confidences testable without a daemon. Executing the plan is `norte-core`'s job. | AGPL-3.0-only |
| `norte-config` | Layered configuration (ADR 0007, ADR 0035): the single config-dir resolver, the strict `norte.toml` schema, scalar merge across layers, profiles, persistence helpers and (feature `watch`) live reload. Reads with `std::fs` on purpose: configuration selects the providers, so it cannot go through them. | MIT OR Apache-2.0 |
| `norte-connect` | Remote connections and secrets (ADR 0015): `connections.toml` holds references only; secrets resolve env → keyring → `age` file; SSH with host-key TOFU and FTP/FTPS with a TLS policy. The resulting session is injected into the provider, which never sees a secret. | AGPL-3.0-only |
| `norte-client` | The daemon client SDK (ADR 0066): transport, framed JSON-RPC, reconnection with resynchronisation, remote task primitives and the typed `RemoteBackend`. Depends on `norte-proto` and nothing else of ours — a dependency test fails the build if the core, a provider, the index, the AI layer, the plugin host or the presentation crate ever reaches it. | MIT OR Apache-2.0 |
| `norte-ui-host` | The semantic state of a graphical frontend, with no idea who paints it (ADR 0066): the versioned bridge (`UiAction` in, ordered `UiUpdate` out), renderer-safe DTOs that carry no raw path, and the single-writer controller. Its boundary test forbids every painting toolkit and the core. | MIT OR Apache-2.0 |
| `norte-gui-tauri` | The reference graphical renderer (ADR 0066/0067): a Tauri 2 window over `norte-ui-host`, plus a plain-TypeScript webview that paints and does nothing else. The ONLY crate that may name Tauri, WRY or a JavaScript runtime. A supported frontend since 2026-09-01 (ADR 0087). It stays out of the *portable* gate on purpose — it needs WebKitGTK, GTK3 and libsoup3 from the system, and requiring those to test the core would be the wrong trade — so it has its own gate, `just gui-ci`, run by its own CI workflow on every change to it, `norte-ui-host`, `norte-client`, `norte-frontend` or `norte-proto`. | MIT OR Apache-2.0 |
| `norte-testkit` | Deterministic `MemProvider`, injectable failures, hostile fixtures, and proptest strategies. | MIT OR Apache-2.0 |
| `norte-core` | Task scheduling, transfers, sessions, policy enforcement, journaling, and the daemon. Its `sync/` module owns the half of ADR 0049 that touches the world: the **spool** (the approved plan retained on disk, keyed to the connection that produced it, single-use, with five ways to die), the executor that revalidates before every destructive step, and the journal batch that makes the result undoable. | AGPL-3.0-only |
| `norte-plugin-host` | WASM plugin manifests, capabilities, catalogue, and runtime. Hosts the first-party FTP provider guest (`examples-wasm/ftp-provider`), which replaces the former `norte-vfs-ftp` crate (ADR 0033). | AGPL-3.0-only |
| `norte-ai` | Model-provider abstraction (`AiProvider`) and implementations: Anthropic, Ollama, OpenAI-compatible. | AGPL-3.0-only |
| `norte-cli` | A command-line client and manual core test bed. | AGPL-3.0-only |
| `norte-tui` | The ratatui dual-pane terminal frontend. | AGPL-3.0-only |
| `norte-frontend` | UI-independent state and behaviour shared by every frontend. | MIT OR Apache-2.0 |
| `norte-help` | Help corpus and markdown-lite model, consumed by every frontend and the CLI (ADR 0040). | MIT OR Apache-2.0 |
| `norte-encoding` | Text encoding detection and decoding. | MIT OR Apache-2.0 |
| `norte-i18n` | Fluent localization resources shared by the frontends. | MIT OR Apache-2.0 |
| `norte-theme` | Semantic theme roles, true-colour values, terminal fallbacks, and bundled presets. | MIT OR Apache-2.0 |

Other subsystems include `norte-index` and `norte-mcp`.

Outside the workspace, `plugins/git-status/` is the first official plugin: a
columns guest built to `wasm32-wasip2` and installed under
`config_dir/plugins/` exactly the way a third party's would be (ADR 0057). Its
gate is `just plugin-git-ci`; `just build-git-wasm` produces the component.

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
