# norte foundational specification (v0.2)

> `norte` is a working name inspired by Norton Commander. ADRs take precedence
> when this document and an accepted decision differ.

norte is an orthodox file manager built around a headless Rust core. The core
provides a stable protocol and a provider-independent virtual filesystem. TUI,
GUI, and CLI frontends are clients of the same core, as are AI agents connected
through governed MCP access.

## 1. Design principles

1. **Build the headless core first.** Frontends contain no business logic. An
   operation that is unavailable through the protocol does not exist.
2. **Make I/O asynchronous and cancellable.** Long-running operations are tasks
   with progress, priority, and cancellation. The UI remains responsive.
3. **Treat bytes as the source of truth.** Filenames are not assumed to be
   UTF-8. UTF-8 is a display view, never a lossy storage representation.
4. **Make mutations reversible or explicitly irreversible.** A transactional
   journal, trash, and undo cover normal operations. Irreversible actions
   require a stronger confirmation.
5. **Treat AI as a client, not an owner.** Agents use the same protocol as
   people, pass through allow/ask/deny policy, and leave a complete audit trail.
6. **Provide safe defaults and deep configuration.** Configuration is layered
   and hot reloaded; keymaps ship with orthodox, Vim, and CUA presets.
7. **Design for testing.** The VFS is a trait, deterministic memory providers
   support fault injection, and coverage and property tests are CI gates.
8. **Support Linux, macOS, and Windows deliberately.** Platform-specific path,
   normalization, permission, and filesystem cases have dedicated tests.

Version 1 does not attempt device synchronization, a distributed database,
first-party cloud storage, or mobile clients.

## 2. System architecture

```text
 TUI       GUI       CLI       MCP agents
  |         |         |            |
  +---------+---------+------------+
       norte protocol / MCP bridge
                  |
             norte-core
    sessions, tasks, policy, journal
          /          |          \
       VFS         index         AI
    providers      SQLite     providers
```

The core can run in process for a low-latency standalone client or as a shared
daemon for multiple simultaneous clients. Both modes expose the same logical
API through an in-process channel, Unix-domain socket, or Windows named pipe.

A client initializes a session by negotiating protocol versions and
capabilities. Session state belongs to the core so another authorized client can
observe the same panes, selections, and tasks.

## 3. Cargo workspace

Each crate has one responsibility, a small public API, and its own tests.

| Crate | Responsibility |
| --- | --- |
| `norte-proto` | Versioned Serde wire types and schemas; no business logic. |
| `norte-vfs` | `Provider`, `VPath`, entries, streams, and capabilities. |
| `norte-vfs-local` | Platform-specific local filesystem provider. |
| `norte-vfs-sftp` | SSH/SFTP provider. |
| `norte-vfs-object` | Feature-gated object-storage backends, starting with S3. |
| `norte-vfs-archive` | ZIP, TAR and TAR.GZ archives as read-only virtual directories, nested (ADR 0018), plus the pure format writers the core's pack operation drives (ADR 0060). |
| `norte-vfs-rar` | Read-only RAR by delegation to an installed `unrar`/`7z` (ADR 0056). |
| `norte-compare` | Directory comparison engine: pairing key and criterion cascade (ADR 0048). |
| `norte-sync` | One-way synchronisation planner over comparison rows (ADR 0049). |
| `norte-config` | Layered configuration, profiles (ADR 0079), persistence and live reload. |
| `norte-connect` | Remote connections and secret resolution; providers never see a secret (ADR 0015). |
| `norte-index` | SQLite metadata, search, tags, and embeddings. |
| `norte-plugin-host` | WASM Component Model runtime, WIT interfaces, and permissions. |
| `norte-ai` | Model-provider abstraction and implementations. |
| `norte-mcp` | MCP bridge between agent clients and the daemon. |
| `norte-core` | Sessions, scheduler, policy, journal, and daemon. |
| `norte-client` | The daemon client SDK: transport, framed JSON-RPC, reconnection (ADR 0066). Depends on the protocol and runtime crates only. |
| `norte-frontend` | Presentation-independent state shared by official frontends. |
| `norte-help` | Help corpus and markdown-lite model for every frontend (ADR 0040). |
| `norte-encoding` | Text encoding detection and decoding. |
| `norte-theme` | Semantic theme roles, true-colour values and terminal fallbacks. |
| `norte-i18n` | Embedded Fluent catalogues, English and Spanish, with parity tests. |
| `norte-tui` | ratatui terminal frontend. |
| `norte-ui-host` | The semantic state of a graphical frontend, over the SDK and `norte-frontend`; knows no painting toolkit and not the core (ADR 0066). |
| `norte-gui-tauri` | The reference graphical renderer: a Tauri 2 webview that paints `norte-ui-host` and decides nothing (ADR 0067). |
| `norte-cli` | Headless command-line client. |
| `norte-testkit` | Memory providers, fixtures, and proptest strategies. |

Frontends depend on the protocol and presentation crates, plus the core in
embedded mode. Providers do not depend on each other. AI providers receive
content, not filesystem paths. Workspace configuration and cargo-deny enforce
these boundaries.

## 4. Tasks and asynchronous I/O

Tokio is the shared multi-threaded runtime. `norte-vfs-local` moves synchronous
filesystem calls to `spawn_blocking`; a future feature may use io_uring if
benchmarks justify a second path.

Any operation expected to take more than roughly 10 ms is represented as a
task with:

- a stable ID and kind;
- pending, running, paused, completed, failed, or cancelled state;
- byte and item progress plus the current item;
- low, normal, high, or user-interactive priority;
- cooperative cancellation;
- an optional parent for task trees.

The scheduler enforces priority and per-provider concurrency, and serializes
conflicting writes by destination. Providers check cancellation in inner loops.
Bounded channels provide backpressure, and progress notifications are coalesced
to no more than 30 updates per second per task.

Every provider must prove that cancelling a large copy leaves either a clean
destination or a clearly marked `.norte-partial` file.

## 5. Virtual filesystem

The `Provider` trait supplies stat, list, ranged read, write, mkdir, remove,
rename, server-side copy, trash, symlink, and watch operations where supported.
Exact signatures evolve through ADRs and the versioned protocol.

`VPath` combines a scheme, optional authority, and raw-byte segments. Its wire
form uses lossless percent encoding. Display is explicitly lossy and marks
undecodable bytes. See ADR 0001.

Capabilities describe behaviour such as watching, atomic rename, server-side
copy, trash, symlinks, POSIX permissions, xattrs, NTFS alternate streams, case
sensitivity, read-only access, random writes, and append. Composite core
operations select strategies from these capabilities and frontends use them to
disable unavailable actions.

The core copy engine provides streaming, optional verification, metadata
preservation, collision policies, retry with backoff, and resumable transfers.
Cross-provider moves are copy, verify, then delete.

Archives use compound schemes and a `!` boundary between the container and
internal path. Reading inside ZIP, TAR and TAR.GZ is read-only and nests (ADR
0018). Writing an archive is not writing into one: pack, test, split and combine
are core operations on the container, journalled like any mutation (ADR 0060,
ADR 0078).

Local providers use the native trash facility. Remote providers may opt into a
logical `.norte-trash/`; otherwise a frontend must obtain explicit confirmation
before requesting permanent deletion.

## 6. Names and text encodings

Filename representation and file-content decoding are separate concerns.

### 6.1 Filenames

| Environment | Reality | norte policy |
| --- | --- | --- |
| Linux and Unix | Arbitrary bytes except `/` and NUL. | Preserve bytes; mark lossy display and offer explicit name repair. |
| Windows | UTF-16 may contain unpaired surrogates. | Preserve with `OsString`/WTF-8 and use the `\\?\` prefix for long paths. |
| macOS | HFS+/APFS commonly expose NFD names. | Preserve original bytes; compare and search using NFC. |
| Case-insensitive filesystems | Case may be preserved but not distinguished. | Detect destination behaviour and evaluate collisions there. |
| ZIP | Names may be UTF-8, CP437, or a legacy local encoding. | Honour the UTF-8 flag and otherwise retain honest byte-level behaviour with manual override. |

Unicode comparison may be configured as NFC, NFD, or none, with NFC as the
default. Comparison never renames an item on disk.

### 6.2 File content

- Detect a BOM first, then inspect a bounded sample with `chardetng` and binary
  heuristics. Always show the selected encoding and allow manual override.
- Decode and transcode incrementally with `encoding_rs`; large files are not
  loaded in full. A conversion reports unrepresentable characters before the
  user chooses whether to abort or replace them.
- Detect LF, CRLF, CR, and mixed endings and expose conversion as an explicit
  operation.
- Route every text preview through the detector and use a hex view as the
  universal fallback.
- Maintain fixtures for UTF-8, UTF-16, Latin-1, Windows-1252, Shift-JIS,
  GB18030, KOI8-R, invalid bytes, NFD, surrogates, emoji, RTL names, and
  platform-hostile trailing characters.

## 7. Extensions

### 7.1 WASM plugins

Plugins use Wasmtime and the Component Model. WIT interfaces cover previewers,
providers, commands, columns, and operation hooks. A `plugin.toml` manifest
declares scoped filesystem, network-host, AI, and execution capabilities. The
host mediates every capability through WASI and norte policy; undeclared access
is unavailable, not merely discouraged.

Local `.wasm` installation is the initial distribution model. Registry and
signature design are deferred until real demand.

### 7.2 Lua scripts

Embedded Lua provides user automation, programmable keybindings, and custom
status behaviour. Scripts are user configuration, not third-party sandboxed
software. Raw Lua `io` and `os` functions have the user's operating-system
permissions; `norte.*` filesystem calls pass through the core, journal, and
policy engine. Project-local scripts require explicit hash-based trust.

### 7.3 External programs

`openers.toml` maps MIME types and operating systems to user-chosen external
commands. Openers are declarative configuration, not plugins.

## 8. Keybindings

A binding maps a context and sequence to a named command. Contexts are
hierarchical and resolve from most specific to most general. Sequences are
deterministic and prefix-free within an effective context stack; ADR 0006
defines exact merge and resolution behaviour.

Protocol-style command names provide parity across keyboard input, the command
palette, Lua, and agent clients. Bundled presets are `orthodox`, `vim`, and
`cua`; `orthodox` is the default. User TOML layers prepend and append bindings
and are hot reloaded. The command palette lists every command and its current
binding.

Keyboard capture should support logical and physical modes so non-US layouts
can choose predictable behaviour.

## 9. AI providers

`AiProvider` exposes model metadata, streaming chat, optional embeddings, and
capabilities such as tools, vision, and structured output. Planned integrations
include Anthropic, OpenAI, Google Gemini/Vertex, local Ollama/llama.cpp, and a
generic OpenAI-compatible endpoint.

Credentials live in the operating-system keyring, environment variables, or
cloud-native identity; never in plain configuration. Configuration selects a
model per task so bulk work can use inexpensive local models and focused work
can use a larger remote model.

AI is opt-in. The UI shows exactly what will leave the machine, the core
enforces denied paths before content reaches a provider, and local-only mode
disables remote providers. AI-assisted rename and organization always produce a
reviewable plan that the existing mutation engine executes.

## 10. Agent access

`norte-mcp` is an unprivileged stdio bridge to the daemon. It exposes only
operations already available in the norte wire protocol. Agents therefore use
norte instead of bypassing it with direct filesystem access.

An agent requests a time-limited scope containing allowed paths and operations.
The human grants it through a client or a pre-approved policy. The core rejects
access outside the scope.

Policy rules evaluate the path, size, extension, provider, actor, and other
context to return `allow`, `ask`, or `deny`. `ask` suspends the operation and
pushes a preview to an interactive frontend.

Every mutation records the human, agent, or plugin actor, before/after data,
and reversal information in SQLite. Users can undo an operation or an entire
agent session. Agent deletes use recoverable trash or staging by default.

The journal supports verified export to CSV and JSONL. Its hash chain detects
corruption; HMAC anchors provide tamper evidence within the documented threat
model.

## 11. Protocol

The base protocol is JSON-RPC 2.0 over an in-process channel, Unix-domain
socket, or named pipe. NDJSON framing is bounded. An `initialize` handshake
negotiates client information, protocol version, and capabilities; the core
supports N and N-1 where the protocol version permits it.

Method families cover sessions, filesystems, tasks, configuration, plugins,
AI, policy, and indexing. Server notifications report filesystem changes, task
progress, approval requests, configuration reloads, and session updates.

Large listings use cursor pagination and incremental rendering. A frontend can
paint the first page without waiting for a complete directory.

Serde types in `norte-proto` are the source of truth. Generated JSON Schemas and
golden fixtures are versioned release artifacts.

## 12. Testing

- Unit-test every decision point. Maintain at least 85% line coverage in core,
  VFS, and protocol crates with cargo-llvm-cov as a CI gate.
- Property-test `VPath` round trips, Unicode comparison, keymap resolution, and
  collision planning.
- Use deterministic memory providers with injected latency, short writes,
  disconnections, and errors to test transfers, cancellation, resume, journal,
  and undo without disk access.
- Run integration tests on real temporary filesystems across Linux, macOS, and
  Windows. Run SFTP and S3 service tests with containers in nightly CI.
- Version protocol request/response fixtures. Any wire change requires an
  intentional protocol decision and version review.
- Fuzz untrusted parsers: archive names, encoding detection, TOML
  configuration, and JSON-RPC framing.
- Test WASM capability denial with a reference guest.
- Snapshot important TUI screens and benchmark startup, large listings, and
  transfer throughput.

Target budgets are a sub-50 ms TUI cold start and first render of a 100,000-entry
listing within 200 ms.

## 13. Configuration

Layers apply from lowest to highest precedence: compiled defaults, system, user,
project-local `.norte/`, and CLI flags. Project-local configuration is opt-in
where it can execute code.

A profile is one more layer directory under the user's configuration,
`profiles/<name>/`, that declares and never executes (ADR 0079). TOML files
separate general configuration, keymaps, themes, openers, AI, policy, and
connections. Connection files contain references, never secrets.
Published JSON Schemas support validation and editor completion. Hot reload
retains the last complete valid configuration on error.

`norte doctor` diagnoses configuration, keymap conflicts, plugin permissions,
and provider connectivity.

## 14. Security

- Keep secrets in the system keyring or another explicitly configured resolver;
  use `zeroize` where appropriate.
- Mediate WASM through declared capabilities. Plugins never receive general
  process execution.
- Enforce agent scopes and policy in the core, with per-session rate limits.
- Use cargo-deny and security advisories in CI, a committed lockfile, signed
  releases, and a release SBOM.
- Maintain `SECURITY.md` with threats including malicious plugins,
  prompt-injected agents, hostile remote servers, path traversal, and archive
  bombs.

## 15. Milestones

| Milestone | Scope | Exit criterion |
| --- | --- | --- |
| **M0: foundation** | Workspace, protocol and VFS traits, memory/local providers, scheduler, and three-OS CI. | Local copy, move, and delete with tested progress and cancellation. |
| **M1: usable TUI** | Dual panes, keymaps, layered config, encoding-aware viewer, and trash. | Maintainers can use it daily in place of mc/Yazi. |
| **M2: remotes and archives** | SFTP, ZIP/TAR, cross-provider resume, and object storage. | A remote/archive/S3/local transfer completes without hidden data loss. |
| **MT: themes** | Shared semantic themes, terminal fallback, presets, and hot reload. | Rich TUI themes use the same model prepared for the GUI. |
| **M4: plugins and AI** | WASM previewers/commands, Lua, model providers, AI rename, and semantic search. | A third party can ship a plugin without changing the core. |
| **M3: governed agents** | MCP bridge, scopes, policy, journal, undo, and audit export. | An agent manages a real directory under `ask` policy with full-session undo. |
| **M5: GUI** | GPUI spike measured (ADR 0027) and retired (ADR 0065); a Rust UI host with a Tauri renderer over it (ADR 0066/0067). | GUI and TUI use the same daemon session simultaneously. |

ADR 0020 reordered the work after M2 to themes, plugins, governed agents, then
GUI. The scope and exit criteria did not change.

## 16. Product decisions

1. `norte` remains the working name pending a final branding decision.
2. Protocol, VFS, testkit, and plugin SDK crates use dual MIT/Apache-2.0.
   The core and official frontends use AGPL-3.0-only.
3. GPUI was selected after the measured spike in ADR 0027 and retired in ADR
   0065. The graphical frontend is a Rust UI host (`norte-ui-host`) painted by
   a Tauri 2 webview that decides nothing (ADR 0066, ADR 0067).
4. SQLite with FTS5 is the first index and storage engine. Tantivy remains a
   possible upgrade if a corpus above one million files demonstrates a need.
5. RAR remains read-only through optional delegation to an installed `unrar` or
   `7z` executable; non-free code does not enter the dependency graph. **Built**
   (`norte-vfs-rar`, ADR 0056): the delegate gets a path, an entry name and a
   pipe, never the filesystem, and only over a local `file://` container.
6. norte collects no telemetry. Diagnostic reports are generated locally and
   shared only when a user chooses to do so.

## 17. Required product capabilities

- **Live and indexed search:** stream cancellable name/content searches across
  VFS providers, with encoding-aware text matching, and expose results as an
  operable virtual pane.
- **Directory comparison and synchronization:** compare panes by metadata or
  hash and produce an approved one-way or two-way operation plan. **Built**
  one-way (`norte-compare`, `norte-sync`, ADR 0048/0049); two-way remains.
- **Batch rename:** a transactional executor for a batch of renames inside one
  directory — whole-plan collision preview, cycle-safe ordering so that a
  permutation succeeds, and one undoable unit. Separately, the rule sets that
  produce the pairs it runs: counters, slices, regular expressions, case
  changes, and character cleanup. AI rename produces pairs for the same
  executor.
- **Volumes and mounts:** enumerate platform volumes, show free space, support
  removable media and safe ejection, and expose drive switching as commands.
- **First-class selection:** preserve selections by entry identity across sorts
  and refreshes; support pattern add/remove, inversion, saved selections, and
  independent view filters.
- **Daemon lifecycle:** start on demand, shut down after configurable idle time,
  upgrade gracefully, authenticate local peers, and never run as root. Loopback
  TCP requires a token.
- **Stable errors:** map platform/provider failures to documented typed protocol
  errors. A task panic fails that task and produces a local report rather than
  terminating the daemon.
- **Local observability:** structured tracing by task and session, rotating local
  logs, and inspectable task traces; nothing leaves the machine.
- **Filesystem edge cases:** explicit symlink policy, cycle detection, sparse
  files, Windows reparse points, and bounded retry for locked files.
- **Shell integration:** cd-on-quit wrappers, file-picker mode, and opening a
  terminal in the active pane.
- **Git awareness:** ship status as an official columns plugin, not a Git client
  in the core. **Built** (`plugins/git-status/`, ADR 0057): the core learns
  nothing about git — the plugin is handed a confined handle to the directory
  and reads the index itself.
- **Localization and accessibility:** Fluent resources for English and Spanish,
  textual cues that do not rely only on colour, high-contrast themes, and
  AccessKit in the GUI.
- **Packaging:** reproducible cargo-dist builds, signed artifacts, common package
  managers, and update notification without unattended installation.

## 18. Engineering contract

- Record architectural changes as MADR documents under `docs/adr/`; do not
  silently rewrite this specification.
- Use short-lived branches, focused pull requests, squash merges, and keep
  `main` releasable.
- Follow Conventional Commits and release-plz.
- A change is done when code, appropriate tests, public API documentation,
  release notes, and all local CI checks are complete.
- Require an additional owner review for protocol and policy changes.
- Pin the Rust toolchain, test stable minus two, forbid unsafe code outside the
  local provider, and treat Clippy warnings as errors.
- Use typed library errors, reserve anyhow for binaries, and avoid unchecked
  unwrap/expect outside documented invariants.
- Run cargo-semver-checks, cargo-deny, documentation builds, coverage, and the
  three-OS test matrix in CI.
- Keep hostile fixtures in `norte-testkit` and add regressions to that corpus
  before their fixes.
- Publish generated protocol and configuration schemas with releases.

## Appendix A: working-name candidates

Names should be pronounceable in English and Spanish, short enough for a CLI,
available across package ecosystems, and suggest navigation or organization.

| Name | Rationale | Risk |
| --- | --- | --- |
| **norte** | A direct Norton Commander reference; also means direction or bearing. | Common word and moderate searchability. |
| **rumbo** | Means heading or course and directly suggests navigation. | Existing travel brands. |
| **estiba** | The careful arrangement of cargo, close to the file-organization domain. | Less obvious pronunciation for English speakers. |
| **veta** | Short and distinctive; suggests following a vein of data. | Abstract meaning. |
| **derrota** | Classical nautical Spanish for a plotted route. | Common modern meaning is defeat. |
| **faro** | A guide or signal. | Strong collision with Grafana Faro. |

The working recommendation remains **norte**, with **estiba** as the more
distinctive fallback.
