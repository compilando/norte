# Multi-frontend architecture and Tauri GUI transition — implementation plan

> **Status:** phase 0 and phase 1 DONE on 2026-08-20 (ADR 0065 and ADR 0066);
> phases 2 onwards not started. Decision D1 AMENDED on 2026-08-20 (ADR 0065): the
> GPUI frontend was retired BEFORE construction, not after it. Read D1, the
> non-goals and phase 8 with that in mind — every "keep GPUI alive" and every
> parity comparison against it is void, and the parity target is the TUI. No
> other implementation has started.
>
> **Purpose:** preserve the complete TUI and the existing GPUI frontend while
> creating reusable client/UI boundaries and a second graphical frontend. The
> reference implementation is Tauri 2, but neither the client SDK nor the UI
> host may depend on Tauri, HTML, JavaScript or a particular renderer.
>
> **Date:** 2026-08-19.

## Goal

Build a new, distributable graphical frontend without moving business logic out
of the headless Rust core, without losing any TUI capability, and without making
Tauri the only possible future frontend.

The completed architecture must support three kinds of client:

1. a Rust frontend such as the existing ratatui TUI or a future Slint/Iced GUI,
   reusing `norte-client` and `norte-frontend` directly;
2. a renderer in another language, such as a Tauri webview, Electron or Flutter,
   using a toolkit-independent Rust `norte-ui-host` through a narrow typed bridge;
3. an independent protocol client, in any language, generated and validated
   against `docs/schema/proto.schema.json` and speaking JSON-RPC to the daemon.

The reference delivery is a Tauri 2 desktop application built as a new binary
beside `ntc`, `norte` and the GPUI `norte-gui`. It becomes the default graphical
frontend only after the parity and release gates in this plan pass.

## Non-goals

- Do not rewrite the TUI.
- ~~Do not delete, rename or freeze the GPUI frontend during the construction
  phases.~~ **Void (ADR 0065):** the GPUI frontend was removed on 2026-08-20,
  before phase 1. Parity is measured against the TUI and the feature ledger,
  not against a running GPUI.
- Do not move filesystem operations, policy, journalling, undo, task scheduling,
  provider dispatch or plugin execution into the renderer or UI host.
- Do not expose an unrestricted `rpc(method, params)` or filesystem/shell API to
  web content.
- Do not promise Windows daemon support as a consequence of changing UI toolkit.
  Authenticated named pipes are a separate transport milestone in this plan.
- Do not make the frontend remotely reachable over TCP or HTTP. A remote web UI
  needs its own authentication and threat-model decision.
- Do not compile all of `norte-frontend` to WebAssembly. It contains host
  infrastructure (`notify`, tokio and native-path integration) and is not a
  browser library.
- Do not make mobile a product target merely because Tauri or Flutter can target
  mobile platforms. The current core, filesystem authority and transport model
  are desktop-oriented.
- Do not reproduce GPUI pixels one for one. Behavioural parity, accessibility,
  safety and theme semantics are required; toolkit-specific implementation
  details are not.

## Why this plan exists

The repository already has the correct top-level boundary: a headless core, a
provider-independent VFS and a versioned JSON-RPC daemon. The current protocol
is `0.51.0`, accepts N/N-1, publishes a generated JSON Schema and pins wire types
with golden tests.

The less obvious boundary is the presentation engine:

- `norte-frontend` is about 44,751 lines of Rust, with roughly 864 inline tests.
  It owns pane state, hostile-name display, sorting, keymaps, selection, mouse
  semantics, layouts, session bodies, availability, settings, help, compare and
  sync presentation state.
- `norte-gui` is about 32,058 lines with roughly 374 inline tests. Its
  `main.rs` is about 15,866 lines and `session.rs` about 2,272 lines.
- The current GUI does not implement JSON-RPC itself. It imports
  `norte_core::backend::remote::RemoteBackend`, `TaskRef` and `TaskCanceller`.
- `RemoteBackend`, its task observers, reconnection, notification pump, feed
  routing and client transport live in the same `backend.rs` as the embedded
  engine facade. A remote-only GUI therefore depends on `norte-core`, whose
  dependency graph also contains providers and engine subsystems it should not
  conceptually need.
- The current GUI always uses the daemon. The remote client and daemon transport
  are `cfg(unix)`; replacing GPUI does not create Windows named-pipe support.
- The UI session body is intentionally opaque to the core and is owned by
  `norte-frontend::session::SessionBody`. A non-Rust frontend that interprets or
  rewrites this body independently risks losing future fields or unknown slot
  kinds.

This means a direct TypeScript or Dart rewrite is technically possible but
would duplicate the most mature and most heavily tested part of frontend
behaviour. The plan instead separates the existing Rust code into reusable
client and UI-host layers, then treats Tauri as one renderer adapter.

## Source decisions and required reading

Before implementing any task, read:

- `ARCHITECTURE.md` — repository map and dependency rules;
- `CLAUDE.md` — hard rules, gate budget and review workflow;
- `docs/adr/0011-jsonrpc-envelope-daemon.md` — transport and daemon lifecycle;
- `docs/adr/0027-gpui-feasibility.md` — why GPUI was selected and why Tauri was
  retained as fallback;
- `docs/adr/0038-protocol-json-schema-and-semver-gate.md` — external-client
  schema contract;
- `docs/adr/0055-a-daemon-may-tell-a-client-to-start-its-replacement.md` —
  reconnect and handover;
- `docs/adr/0058-a-screen-is-a-tree-the-core-keeps-and-does-not-read.md` — layout
  ownership and opaque slot kinds;
- `docs/adr/0059-the-session-is-a-document-with-one-writer.md` — session body,
  ownership and persistence;
- `crates/norte-core/src/backend.rs` — current embedded/remote facade;
- `crates/norte-core/src/daemon/client.rs` — framed JSON-RPC client;
- `crates/norte-gui/src/session.rs` — current GUI/Tokio bridge;
- `crates/norte-frontend/src/lib.rs` and `session.rs` — reusable presentation
  surface and stored UI document.

## Architectural decisions

These decisions are part of the plan. If implementation reveals that one is
wrong, stop that task and amend the architecture ADR before coding around it.

### D1 — The migration is additive

`ntc` remains a supported, complete frontend through every phase. Existing
imports keep compiling through compatibility re-exports or small adapters.
There is no flag day in which the TUI must migrate to an unfinished client SDK
or host.

**Amended 2026-08-20 (ADR 0065).** The GPUI binary is NOT the behavioural
oracle, because it no longer exists: it was retired before phase 1, so that
there is exactly one implementation of every presentation rule while the new
frontend is built. The oracle is the TUI plus `norte-frontend`'s own tests, and
the feature parity ledger below is measured against them. What stays true of D1
is the part that matters: the boundaries are additive, `ntc` never has to
migrate to an unfinished SDK, and no phase requires a flag day.

### D2 — Tauri is a renderer adapter, not an architecture layer

Only the application crate may depend on `tauri`, WRY, a JavaScript runtime or
web assets. `norte-client`, `norte-frontend` and `norte-ui-host` must compile and
run their tests without Tauri and without Node installed.

The bridge exposed to Tauri must be usable by another adapter. A future Electron
sidecar, Flutter FFI wrapper or headless integration test should be able to send
the same `UiAction` values and consume the same `UiUpdate` values.

### D3 — State ownership follows meaning

| Layer | Owns | Must not own |
| --- | --- | --- |
| `norte-core` / daemon | providers, tasks, mutations, policy, journal, undo, plugins, retained plans, stored opaque session | renderer state or layout interpretation |
| `norte-client` | transport, handshake, timeouts, cancellation-on-drop, reconnection, notification routing, remote task/feed handles | embedded engine, provider construction or visual state |
| `norte-frontend` | toolkit-independent presentation rules and models | renderer widgets or business effects |
| `norte-ui-host` | live semantic UI document, action reduction, async orchestration, generation guards, view projection | HTML/CSS, Tauri APIs or direct filesystem authority |
| renderer | DOM/widgets, focus ring, hover, scroll animation, transient geometry, text measurement and painting | `VPath` parsing, sorting rules, task authority, mutations or session persistence |

The renderer may cache immutable view data for painting, but Rust remains the
source of truth for semantic state.

### D4 — Extract a remote client SDK before expanding the GUI

Create `norte-client`, a Rust SDK that depends on `norte-proto` and transport
dependencies but not on `norte-core`, provider crates, `norte-index`,
`norte-plugin-host`, `norte-ai` or the embedded engine.

The extraction must preserve current behaviour exactly:

- initialize and actor declaration;
- N/N-1 negotiation;
- authenticated Unix socket connection;
- connect-or-spawn on first connection only;
- daemon handover spawn permission and expiry;
- reconnection and `task.list` resynchronisation;
- orphan task resolution;
- call timeouts and cancellation-on-drop;
- task progress, foreign tasks and cloneable observers;
- search, compare and sync streamed-feed routing;
- policy approvals, degraded-connection events and going-away events;
- fallback for N-1 `METHOD_NOT_FOUND` methods where the current facade has one.

Do not redesign those mechanisms during extraction. Behavioural changes get a
separate issue and commit after the move is green.

### D5 — Keep the existing `Backend` facade for embedded clients

The TUI and CLI need the single embedded/remote `Backend` surface. Keep that
surface in `norte-core` initially, but make its remote arm delegate to or
re-export `norte-client` types.

Conceptually:

```text
norte-core::Backend::Embedded(Engine)
norte-core::Backend::Remote(norte_client::RemoteBackend)
```

This prevents a wide TUI rewrite and preserves embedded mode. A later plan may
move the common facade to another crate if the dependency graph permits it; it
is not required for the new GUI.

### D6 — The host accepts semantic actions, not backend methods

The public host entry point is an action dispatcher plus subscriptions. The
renderer asks to move the cursor, open the focused item, confirm a pending copy
or change layout. It does not choose `fs.copy` parameters, parse `VPath`, invent
task ids or call plugin methods directly.

Representative shape (exact fields are settled by Task 2.1):

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiAction {
    MoveCursor { slot: u32, delta: i64 },
    SetCursor { slot: u32, row: RowKey },
    OpenFocused { slot: u32 },
    ChangeVisibleRange { slot: u32, start: u64, len: u32 },
    RunCommand { id: String },
    ConfirmModal { modal: ModalId, choice: ModalChoice },
    CancelTask { task: norte_proto::TaskId },
}
```

`UiAction` is presentation API, not wire protocol. Changing it bumps a bridge
schema version, not `PROTOCOL_VERSION`.

### D7 — Renderer updates are ordered, versioned and bounded

Every host instance has:

- a random `instance_id` generated in Rust;
- a monotonically increasing `sequence`;
- a `BRIDGE_VERSION` constant;
- a full `ViewSnapshot` for initial attach/recovery;
- bounded `UiPatch` or typed `UiEvent` updates afterward.

The renderer rejects a patch for a different instance, ignores an already
applied sequence, and requests a new full snapshot on a gap. It never attempts
to merge an unknown gap itself.

Large directories must not produce a JSON copy of every row on every cursor
movement. The host publishes a window around the renderer's visible range plus
stable summary state. Cursor/mark changes patch only affected rows and chrome.

### D8 — Raw paths never become renderer authority

The Rust host uses `VPath` internally and uses `norte-frontend` to produce
display strings and hostile/lossy flags. A row sent to the renderer has a
host-issued `RowKey`; operations sent back refer to the key and slot, not to a
path string reconstructed in JavaScript.

A wire-form path may be included in an explicitly named copy/export field for
display or clipboard use, but no mutation or navigation accepts it back without
normal typed parsing and current-generation validation in Rust.

This protects the invariant that filenames are bytes and prevents DOM text from
becoming an operation parameter.

### D9 — Session and layouts stay in Rust

`norte-ui-host` captures and applies the existing
`norte_frontend::session::SessionBody`. The renderer receives a resolved view of
layouts and slots, not ownership of the opaque `session.body` document.

Unknown slot kinds are preserved by the Rust layout tree. The renderer paints a
named unsupported panel where required, but it cannot erase unknown `params` on
save. Session ownership, revision conflicts, detached mode and re-acquisition
retain ADR 0059 semantics.

### D10 — All effects continue through the daemon

The reference GUI uses `norte-client::RemoteBackend` and the daemon. It does not
construct an `Engine` in the application process. Therefore:

- policy and journalling have the same guarantees as the current GUI;
- retained sync plans remain bound to the producing connection;
- plugin and agent boundaries do not change;
- GUI crashes do not own filesystem tasks;
- multiple clients continue to share one daemon safely.

An embedded GUI mode, if ever desired, is a separate product decision and must
state its journal/session/spool consequences.

### D11 — The webview is unprivileged by default

The production webview loads only packaged local assets. It has:

- no Node integration;
- no arbitrary filesystem, process, shell, HTTP or raw socket API;
- a restrictive Content Security Policy;
- no navigation to remote origins;
- external links opened only through an allowlisted, validated Rust command;
- a minimal Tauri capability file listing only required window/event commands;
- typed validation at every invoke boundary;
- no secrets, native paths or unsanitised plugin markup in logs or DOM HTML.

Help, plugin output, filenames and previews are data, never executable markup.
Markdown-lite renders through a closed element vocabulary; do not use raw
`innerHTML`.

### D12 — Linux first does not mean Linux only in the design

The first release target is Linux Wayland/X11 because that is the currently
shipped alpha target and the only platform on which the complete project gate is
regularly exercised.

The new crates must remain portable Rust. OS-specific transport is isolated
behind `norte-client::transport`. macOS uses the Unix transport after a native
build/test pass. Windows cannot claim daemon parity until an authenticated named
pipe transport has its own ADR, threat model and integration tests.

### D13 — Accessibility is a contract, not a polish phase

Rows, tabs, panes, menus, dialogs, task progress and status messages must have
semantic roles and names from the first vertical slice. Keyboard-only operation
is a release gate. The renderer must expose the current focus, active/target
roles, selection, expanded/collapsed state and progress through the platform
accessibility tree.

### D14 — No dual implementation of presentation rules

TypeScript may format CSS units, compose localized strings already provided as
parts, or choose toolkit-specific iconography. It may not independently define:

- filename sanitisation or hostile-name badges;
- sort order or comparison keys;
- keymap resolution, sacred keys or command availability;
- selection/mark/drag semantics;
- layout role resolution;
- rename/sync/compare validation;
- session caps and migration;
- error taxonomy mapping;
- plugin decoration sanitisation.

If the renderer needs a result, add a projection to `norte-frontend` or
`norte-ui-host` and test it there.

## Target dependency graph

```text
                                  norte-proto
                                      ^
                                      |
                    +-----------------+------------------+
                    |                                    |
              norte-client                         norte-vfs/core
                    ^                                    ^
                    |                                    |
              norte-ui-host                        embedded Backend
                    ^                                    ^
                    |                                    |
          Tauri / Electron sidecar                       ntc
                    ^
                    |
             web/Dart renderer

       norte-frontend is used by ntc and norte-ui-host, and depends on
       norte-proto; it never depends on either renderer.
```

Forbidden dependency edges:

```text
norte-client  -X-> norte-core / providers / frontend
norte-ui-host -X-> tauri / gpui / ratatui / web framework
renderer      -X-> daemon socket / native filesystem / session.json
norte-core    -X-> norte-frontend / renderer
```

## Proposed repository structure

Names are normative unless an ADR records a better one before Task 1.1.

```text
crates/
  norte-client/
    Cargo.toml
    src/
      lib.rs                 public SDK and re-exports
      transport.rs           sealed transport abstraction
      transport/unix.rs      authenticated Unix socket implementation
      rpc.rs                 framed JSON-RPC Client moved from core
      remote.rs              RemoteBackend public facade
      reconnect.rs           establish/resync/handover lifecycle
      task.rs                TaskRef/TaskObserver/TaskCanceller remote forms
      feeds.rs               search/compare/sync route machinery
      events.rs              connection, approvals and degradation events
    tests/
      transport.rs
      reconnect.rs
      parity.rs

  norte-ui-host/
    Cargo.toml
    src/
      lib.rs
      action.rs              UiAction and validation
      bridge.rs              version, instance, sequence, snapshot/patch
      controller.rs          owner of live semantic state
      dto.rs                 renderer-safe view types
      panes.rs               listing/navigation orchestration
      tasks.rs               task registry and progress projection
      session.rs             capture/apply/coalesced persistence
      commands.rs            command dispatch and availability
      dialogs.rs             toolkit-neutral pending dialogs
      viewer.rs              viewer projection and bounded payloads
      compare.rs
      sync.rs
      plugins.rs
      settings.rs
    tests/
      contract.rs
      hostile.rs
      lifecycle.rs
      parity.rs

  norte-gui-tauri/
    Cargo.toml
    build.rs
    tauri.conf.json
    capabilities/
      main.json
    src/
      main.rs                process/window setup only
      adapter.rs             Tauri invoke/event adapter to norte-ui-host
    web/
      package.json
      pnpm-lock.yaml         or the package manager chosen in Task 3.1
      vite.config.*
      src/
        bridge/              generated types + one transport implementation
        state/               renderer cache, sequence/gap recovery
        components/
        styles/
        tests/
```

The final application name remains undecided during parallel development.
`norte-gui-tauri` is the package/binary working name. Do not rename the existing
`norte-gui` until cutover Task 8.2.

## Public surfaces

### `norte-client`

The extraction should converge on a public surface similar to:

```rust
pub struct RemoteBackend { /* private */ }

pub struct ConnectOptions {
    pub socket: PathBuf,
    pub client_info: norte_proto::methods::ClientInfo,
    pub spawn_command: Option<Vec<OsString>>,
    pub actor: ClientActor,
}

pub enum ClientActor {
    Human,
    Agent { session: String },
}

impl RemoteBackend {
    pub async fn connect(options: ConnectOptions) -> Result<Self, ClientError>;
    pub fn take_events(&self) -> Option<ClientEvents>;
    // Typed filesystem/task/plugin/session methods, preserving current
    // RemoteBackend behaviour and names where practical.
}
```

Do not make the low-level generic `Client::call` the primary frontend API. It
may remain public or `doc(hidden)` for the MCP bridge and tests only if existing
consumers require it. Normal frontends use typed methods.

### `norte-ui-host`

Target surface:

```rust
pub struct UiHost { /* private */ }

pub struct UiHostOptions {
    pub backend: norte_client::RemoteBackend,
    pub initial_dir: norte_proto::VPath,
    pub config: ResolvedFrontendConfig,
    pub viewport: InitialViewport,
}

impl UiHost {
    pub async fn start(options: UiHostOptions) -> Result<(Self, ViewSnapshot), UiError>;
    pub async fn dispatch(&self, action: UiAction) -> Result<ActionAck, UiError>;
    pub fn subscribe(&self) -> UiSubscription;
    pub async fn snapshot(&self) -> ViewSnapshot;
    pub async fn shutdown(self) -> ShutdownReport;
}
```

The real implementation may use an actor task and channels so `dispatch` never
holds renderer locks across I/O. `UiHost` must be cheap to clone or be wrapped by
an `Arc`; the sole state writer lives in the actor.

### Bridge envelope

All renderer-facing messages use an envelope:

```rust
pub const BRIDGE_VERSION: u32 = 1;

pub struct BridgeEnvelope<T> {
    pub bridge_version: u32,
    pub instance_id: String,
    pub sequence: u64,
    pub payload: T,
}

pub enum UiUpdate {
    Snapshot(ViewSnapshot),
    Patch(ViewPatch),
    Notice(UiNotice),
}
```

Rules:

- `sequence` orders updates from one host instance only;
- `Snapshot` replaces all cached renderer state;
- `Patch` contains a `base_sequence`; applying to another base is forbidden;
- `Notice` is still ordered; it is not a second unordered event channel;
- unknown `bridge_version` causes a fatal compatibility screen, not partial
  interpretation;
- every collection and string has an explicit cap in Rust;
- serialization errors are terminal host bugs and surface without logging
  filenames or session bodies.

## Renderer-safe view model

The DTOs should carry exactly what is needed to render, and no operation
authority. Suggested minimum:

```text
ViewSnapshot
  connection
  layout
  slots: Map<SlotId, SlotView>
  focus
  roles
  status
  dialogs
  tasks
  theme
  locale

BrowserSlotView
  slot_id
  generation
  title/path_display
  hostile_path
  total_rows
  visible_range
  rows: Vec<RowView>
  cursor: RowKey?
  marks_count
  sort/columns
  loading/error/skipped

RowView
  key: RowKey
  display_name
  hostile/lossy
  kind
  selected/marked
  cells: Vec<CellView>
  decoration
  accessibility_label/description
```

`RowKey` is valid only for `(instance_id, slot_id, generation)`. On relist, the
host increments `generation`; a late click or drag referring to the previous
generation returns a benign stale-action result and does nothing.

## Async and backpressure model

The host is a single-writer state machine:

```text
renderer actions ----+
client events -------+----> bounded actor inbox ---> semantic state
watch events --------+              |
timers --------------+              +--> coalesced UiUpdate stream
```

Requirements:

- Actions that can be repeated rapidly (`MoveCursor`, visible-range changes,
  resize) are coalesced or use latest-value channels where semantics allow it.
- Destructive actions, confirmations and task terminal events are never dropped.
- Pointer movement and hover stay entirely in the renderer.
- Scroll painting stays in the renderer; only a debounced visible-range change
  crosses to Rust.
- A slow renderer cannot cause unbounded host memory. On patch-channel overflow,
  discard intermediate paint-only patches and enqueue one fresh snapshot.
- Long-running operations return task identity immediately and progress through
  the ordered update stream.
- Every async response carries the slot generation or request token that caused
  it. Stale responses are discarded in Rust, not merely hidden by the renderer.
- Host shutdown stops watchers, flushes an owned session, drops retained plans
  through the existing backend rule and reports incomplete cleanup.

## Phases and task dependency graph

```text
Phase 0: decision + baselines
        |
Phase 1: norte-client extraction
        |
Phase 2: norte-ui-host foundation
        |
Phase 3: Tauri vertical-slice spike ---- GO / NO-GO
        |
Phase 4: read-only beta
        |
Phase 5: mutations and task safety
        |
Phase 6: advanced feature parity
        |
Phase 7: packaging and platform gates
        |
Phase 8: cutover and GPUI retirement
```

Phases 1 and 2 are valuable even if Tauri fails the spike: they improve the
existing architecture and can feed another renderer. Phases 4–8 must not start
automatically after a failed go/no-go review.

---

## Phase 0 — Freeze decisions and establish baselines

### Task 0.1: write the architecture ADR

**Create:** `docs/adr/0064-renderers-use-a-rust-ui-host.md` (use the next free
number if 0064 is occupied when work begins).

The ADR records:

- Tauri 2 as the reference renderer and why Electron/Flutter/direct Dart or JS
  clients are not the first implementation;
- `norte-client` extraction;
- `norte-ui-host` as toolkit-independent boundary;
- semantic Rust state versus ephemeral renderer state;
- daemon-only reference GUI;
- renderer least privilege;
- Linux-first and the independent Windows transport blocker;
- coexistence and rollback strategy;
- accepted WebKitGTK variability and dependency costs;
- the alternative of a fully Rust Slint/Iced GUI.

Do not change code in this task. Link this plan and ADR 0027.

**Acceptance:** the ADR answers every D1–D14 decision or explicitly supersedes
it.

**Suggested commit:**

```text
docs(adr): define the multi-renderer UI boundary
```

### Task 0.2: record reproducible current-GUI baselines

**Create:** `docs/benchmarks/gui-baseline-<date>.md`.

Measure on the same machine and display session:

- clean and incremental GUI build time;
- release binary and packaged dependency size;
- cold start to window;
- warm start to first real directory frame;
- idle RSS after first listing;
- RSS with 10,000 and 100,000 synthetic rows;
- cursor input-to-paint p50/p95;
- continuous scroll frame time and dropped frames;
- daemon reconnect time;
- session restore time;
- accessible-tree node count for a visible 100-row pane.

Add a deterministic fixture command or testkit generator if necessary, but do
not optimise current GPUI in this task. Record exact hardware, compositor,
display protocol and build profile.

**Acceptance:** another developer can repeat every measurement from commands in
the document.

### Task 0.3: repair architecture-document drift

**Modify:**

- `crates/norte-gui/Cargo.toml` comments;
- `Makefile` GUI help text;
- `ARCHITECTURE.md` if its current GUI/workspace statement has drifted;
- `README.md` only where it states a fact that is already false.

The manifest currently calls the GUI an excluded, read-only spike even though it
is a workspace member and implements mutations. Correct comments only; no
dependency changes.

**Test:** documentation review plus `cargo metadata --no-deps` to confirm the
written workspace status.

**Suggested commit:**

```text
docs(gui): make the current frontend status honest
```

### Phase 0 exit gate

- ADR accepted.
- Baseline document reproducible.
- Current facts no longer contradict manifests.
- Initial performance budgets below are either accepted or amended in the ADR.

---

## Phase 1 — Extract `norte-client` without behaviour change

> **DONE 2026-08-20.** `crates/norte-client` exists with transport, framed
> JSON-RPC, socket vocabulary, remote task primitives and the typed
> `RemoteBackend` split into `remote/{mod,routes,paging,calls}.rs`.
> `norte-core/src/backend.rs`: 5.271 → 2.416 lines. Compatibility re-exports
> keep `norte_core::daemon::{Client, ClientError, default_socket_path,
> is_version_mismatch}` and `norte_core::backend::remote` working, so MCP, the
> CLI and the e2e tests did not change a line. `just ci-fast` green (5.077
> tests), coverage 88,12 % with the SDK inside the gate.
>
> Deviations from the task text, all deliberate:
>
> - Task 0.2 (GUI baselines) is void: ADR 0065 retired the GPUI frontend
>   before this phase, so there was nothing to measure.
> - `TransferOptions` took option 3 (SDK value type + exhaustive `From` in the
>   core). `SyncPlanEvent` and `ConnEvent` took a shorter route than the task
>   describes: the SDK owns them and `norte-core` RE-EXPORTS them, because
>   their variants are wire types and two definitions would be two places to
>   add a variant.
> - `take_foreign_tasks` needed a forwarding task in the core: a channel
>   cannot be mapped in place, and the bridge dies with the connection that
>   feeds it.

This is a move/refactor phase. Do not mix new frontend features into it.

### Task 1.1: create the crate and pin its dependency boundary

**Create:**

- `crates/norte-client/Cargo.toml`;
- `crates/norte-client/src/lib.rs`;
- `crates/norte-client/tests/dependency_boundary.rs` or an equivalent CI script;
- workspace membership/dependency entries in root `Cargo.toml`.

Initial allowed dependencies should be limited to what the remote client truly
uses: `norte-proto`, tokio, tokio-util if still required, futures, serde,
serde_json, base64, thiserror and tracing. Every additional dependency follows
hard rule 8.

Add a boundary check that fails if `cargo metadata` shows a runtime dependency
from `norte-client` to:

```text
norte-core
norte-vfs and every provider
norte-index
norte-ai
norte-plugin-host
norte-frontend
```

If `EntryStream` currently forces `norte-vfs`, replace the public remote listing
stream with an SDK-owned alias over `Stream<Item = Result<Entry, Error>>`; do not
pull the VFS/provider crate into the client merely for a type alias.

**RED test:** boundary test lists forbidden packages and initially fails while
the extraction is incomplete.

**Acceptance:** empty SDK builds and its boundary test is part of the targeted
crate suite.

### Task 1.2: move the framed JSON-RPC client

**Move/adapt:** `crates/norte-core/src/daemon/client.rs` to
`crates/norte-client/src/rpc.rs` and transport modules.

Keep compatibility re-exports from `norte_core::daemon` for existing internal
tests during this phase. Do not copy the implementation and leave two clients.

Tests moved or added must cover:

- 16 MiB framing limit and partial frames;
- request correlation and malformed response handling;
- connection close waking all pending calls;
- a call started after close fails instead of hanging;
- notification reception;
- initialize as human and as agent;
- server peer credential rejection;
- first connect-or-spawn backoff and timeout;
- request id tracking used by cancellation-on-drop;
- no path or frame content in error logs.

`transport::unix` owns `UnixStream` and peer credentials. `rpc.rs` operates on a
private async read/write transport interface so a named-pipe implementation can
be added later without copying correlation/framing logic.

**No protocol bump:** serialized bytes and method calls are unchanged.

**Suggested commit:**

```text
refactor(client): extract the framed daemon connection
```

### Task 1.3: extract remote task primitives

Move or split the remote-capable parts of:

- `TaskRef`;
- `TaskObserver`;
- `TaskCanceller::Remote`;
- `ConnEvent`;
- streamed feed handles and terminal semantics.

The embedded variants remain in `norte-core`. Prefer shared SDK types with an
adapter around embedded `TaskHandle` over an SDK enum that depends on the
engine.

One acceptable shape is:

```text
norte-client::RemoteTask
  id
  progress watch receiver
  RemoteTaskCanceller
  join

norte-core::TaskRef
  Embedded(...)
  Remote(norte_client::RemoteTask)
```

Do not make a wait handle clonable. Preserve the current distinction: the
owner joins; observers are cloneable; cancellation is idempotent.

Tests:

- one join owner and multiple observers;
- terminal-before-response race;
- disconnect before terminal becomes typed `ProviderUnavailable`;
- resync finds a live task;
- resync resolves an unknown orphan;
- cancelling a remote observer calls `task.cancel` once best-effort;
- dropping the last backend ends background reconnect work.

### Task 1.4: extract `RemoteBackend` and feed routing

Move `backend::remote` to the SDK in coherent units rather than one 2,000-line
file. Preserve all current method behaviour.

Split at least:

- connection establishment/resync;
- typed calls and taxonomy mapping;
- task registry;
- search feed routes;
- compare feed routes;
- sync-plan feed routes;
- approval/degradation/connection event fan-out.

Tests must pin the subtle races already described in current rustdoc:

- a feed batch may arrive before route registration;
- terminal progress may arrive before the final batch;
- route removal grace does not drop a late final batch;
- pending batches are capped;
- unrelated task terminals do not evict a search/compare/sync termination mark;
- an N-1 daemon missing an additive method degrades exactly as before;
- read calls and mutation submissions cancel on abandoned futures;
- normal RPC errors disarm the cancellation guard;
- AI calls retain their longer timeout;
- handover spawn permission is consumed and expires;
- normal daemon shutdown cannot be resurrected by reconnection.

Where remote methods currently use core-only types (`TransferOptions`, sync
events, volume mappings), choose one of these in order:

1. use the already published `norte-proto` wire type directly;
2. move a transport-neutral value type to `norte-proto` only if it is genuinely
   part of the wire, with the required protocol review and bump if shape changes;
3. define an SDK value type and map exhaustively in `norte-core`;
4. never depend back on `norte-core` to save a mapping.

Avoid protocol changes during the extraction unless a hard dependency cycle
makes one unavoidable.

### Task 1.5: adapt `norte-core::Backend` and existing consumers

**Modify:**

- `crates/norte-core/src/backend.rs`;
- `crates/norte-core/Cargo.toml`;
- `crates/norte-tui` imports only where necessary;
- `crates/norte-gui/src/session.rs` imports only where necessary;
- CLI/MCP imports discovered by `rg`.

`Backend::Embedded` continues to call `Engine`. `Backend::Remote` wraps or
aliases the SDK. Preserve public names through re-exports for one migration
cycle where that avoids unrelated changes.

Run the existing TUI, GUI, CLI and MCP tests. No user-facing behaviour or
snapshot should change.

Add a compile-only consumer test that constructs:

- an embedded backend through `norte-core`;
- a remote backend through `norte-client`;
- the compatibility remote arm through `norte-core`.

### Task 1.6: architecture and dependency audit

Run and record:

```text
cargo tree -p norte-client
cargo tree -p norte-gui
cargo metadata --no-deps
```

Acceptance:

- no forbidden dependency reaches `norte-client`;
- TUI functional tests remain unchanged;
- GPUI GUI builds and its tests remain green;
- no wire golden changed unless separately reviewed;
- no duplicated JSON-RPC client remains in `norte-core`;
- rustdoc describes the new ownership honestly;
- `ARCHITECTURE.md` lists the client SDK.

### Phase 1 review and gate

- `rust-reviewer`: mandatory, because the move is substantial and concurrent.
- `protocol-guardian`: mandatory only if a wire type, handler or serialized
  shape changed; otherwise confirm explicitly that the diff is wire-neutral.
- `security-reviewer`: review Unix peer authentication, connect-or-spawn and
  cancellation/handover movement.
- `just t norte-client`, `just t norte-core`, then existing frontend targeted
  suites during RED→GREEN.
- One `just ci-fast` after Tasks 1.1–1.4, not after each move.
- One full `just ci` at phase close.

---

## Phase 2 — Build the toolkit-independent `norte-ui-host`

> **Phase 2 DONE 2026-08-20** in the sense its exit gate asks for: tasks 2.1,
> 2.2 and 2.7 complete, and the core of 2.3–2.6 with the rest written down
> below as owed. The crate exists with `bridge.rs`
> (envelope, `BRIDGE_VERSION`, opaque `RowKey`/`ModalId`/`RequestToken`, caps),
> `dto.rs` (renderer-safe views; no raw path crosses), `action.rs` (semantic
> actions) and `controller.rs` (bounded inbox, one writer, broadcast
> subscription with lag → snapshot recovery). Pinned by a golden JSON corpus
> with 1:1 coverage in both directions, plus a dependency boundary test that
> forbids every toolkit and the core.
>
> Deviations: `HostBackend` is the minimal internal trait the plan allows —
> `list` only for now; the golden corpus lives at `tests/golden/` and no JSON
> Schema is generated yet (schemars is not a dependency of this crate).
>
> **Task 2.3, done:** the slot IS `norte_frontend::PaneState` plus
> `norte_frontend::nav::History` — cursor, marks, hidden, cursor memory and
> the listing epoch (which is the bridge's `generation`) all come from the
> shared layer. `History` and `Trail`/`TrailStep` moved OUT of `norte-tui`
> into `norte-frontend` to make that possible; the TUI re-exports them and no
> call site changed. Navigation (activate directory, parent with pending
> focus, history back/forward) leaves the request in flight with a token and
> its answer returns to the actor as another message, so a superseded listing
> is discarded in Rust with a test to prove it.
>
> **Task 2.3, still owed:** pagination/stream drain policy (the host asks for
> the whole listing today), quick search, configured columns and the attr
> catalogue, watcher refresh and degradation, and the hidden-slot lifecycle.
> Those need tasks 2.4 and 2.5 around them to mean anything.
>
> **Task 2.4, done:** the renderer sends normalized keys through a thin
> adapter (`keys.rs`) and Rust resolves counts, prefixes and commands with the
> shared `Resolver`, presets and catalogue. `commands.rs` declares what this
> host implements — which is what makes `Availability::NotHere` mean
> something — with tests that stop the list and the effects from drifting
> apart. Pending sequences and counts are projected into the status view.
>
> **Task 2.4, still owed:** which-key panel projection beyond the pending
> string, menu/palette/shortcuts views, the sacred-key rule (it needs two
> slots to mean anything, so it lands with task 2.5) and text-entry contexts
> (they need dialogs, task 2.6).
>
> **Task 2.5, done:** the host holds the configured layout tree, resolved
> with the shared engine and the shared minimums. Resizing re-resolves and
> never rewrites the tree; hidden slots ask for nothing; an unprojected kind
> travels greyed out with its name; the target role is always another VISIBLE
> browser or nothing. Session (ADR 0059) reads at start, applies what it
> understands, and flushes at shutdown under its three rules — a detached
> window does not write, a future schema is neither applied nor overwritten,
> and a conflict is reported instead of overwriting someone. Marks never
> enter the session.
>
> **Task 2.5, still owed:** the coalesced periodic `session.put` (only the
> shutdown flush exists), ownership acquired later in the session's life,
> reconnect/handover persistence, and the unsupported-kind `params`
> round-trip (the host preserves the tree it was given, but does not yet
> merge a session that carries kinds it cannot project).
>
> **Task 2.6, done:** delete is the first effect the host runs, and it goes
> in through the door every effect must use — the confirmation. The dialog
> declares which choice destroys; a choice it did not offer is not
> interpreted; modal ids are monotonic so confirming twice does not delete
> twice (`Stale{Modal}`). The task board projects progress and the TERMINAL
> state cannot be lost: it travels on the same ordered queue and is sent
> before the channel is dropped. Cancel is idempotent. Dialog bodies are
> sanitised and capped like listing rows.
>
> **Task 2.6, still owed:** foreign tasks, connection lost/restored and
> degradation notices, policy approvals, journal status, quit confirmation,
> and dialogs with a text field (they need the mutations that open them).
>
> **Task 2.7, done:** `tests/parity.rs` runs each scenario twice — against
> `PaneState` + `History` directly and against the host's actions and
> snapshots — and compares semantic state step by step, so "the host does not
> reimplement the rules" is a test and not a comment. `tests/daemon_e2e.rs`
> drives the host against a REAL daemon over a temp socket: initial listing,
> navigation with the trail, and the session written at shutdown and read
> back by the host's next life. No display, no Node.
>
> **Phase 2 exit gate:** host tests run without display and without Node ✔;
> navigation/session scenarios work against a real daemon ✔; no Tauri types
> in the graph ✔ (boundary test); the host serializes bounded windowed views
> ✔ (40 of 100.000 rows); hostile corpus and session round-trip ✔; the TUI
> is green and unchanged ✔ (the GPUI frontend no longer exists, ADR 0065).
>
> **Debt paid after closing the phase (2026-08-20, same day):**
>
> - **Pagination.** `HostBackend::list` returns a stream; the host paints the
>   first hundred entries and drains the rest in batches of five hundred
>   through the actor's own inbox, extending with `PaneState::extend`. A batch
>   from a superseded navigation is discarded by its token.
> - **Quick search.** `pane.quick-search` opens the shared `QuickSearch`, and
>   while it is open the TEXT keys are its own — typing does not run commands.
>   It does not disconnect the rest of the keyboard: modifier chords still
>   take their normal path. Its state travels to the renderer.
> - **Connection notices and foreign tasks.** Both channels are taken once at
>   start and travel through the same ordered inbox; losing the daemon is
>   painted AND said, and a task another client started shows in the board
>   marked as foreign.
>
> - **Columns.** The configured set travels as cells built by
>   `norte_frontend::columns::styled_cell` — the same function the TUI uses —
>   and the attr ids those columns need ride along with every listing, because
>   a provider only sends what it is asked for. Absence travels as absence.
> - **Dialogs with a text field.** `pane.mkdir` opens one; the renderer sends
>   the whole text after each edit (the caret is its own) and confirming
>   validates the name with the same rule as any other segment before queuing
>   anything.
>
> - **Attr catalogue.** Requested once per scheme and only when `attr:`
>   columns are configured; when it arrives the host sends a SNAPSHOT, because
>   it changes how cells that already travelled are read.
> - **Policy approvals.** An agent op under an `ask` rule opens its dialog
>   with masked paths and says when the list is TRUNCATED. Only `approve`
>   approves: any other answer denies, and so does closing the dialog —
>   leaving the agent waiting is worse than telling it no. The affirmative id
>   differs from the normal `confirm` on purpose.
>
> Still owed from phase 2: watcher refresh, the which-key panel and
> menu/palette/shortcuts views, the periodic coalesced `session.put`, and
> ownership acquired later in the session's life.
>
> **Next: phase 3** — the Tauri vertical-slice spike and its go/no-go. That is
> the first time a renderer appears at all.

Implement one vertical feature at a time. The host should be useful to a
headless test before Tauri exists.

### Task 2.1: bridge types, caps and golden contract

**Create:** `norte-ui-host` crate and the `action.rs`, `bridge.rs`, `dto.rs`
foundation.

Define and document:

- `BRIDGE_VERSION`;
- `InstanceId`, `RowKey`, `ModalId`, `RequestToken` newtypes;
- `BridgeEnvelope<T>`;
- `UiAction`, `ActionAck`, `StaleAction`;
- `ViewSnapshot`, `ViewPatch`, `UiNotice`;
- `ConnectionView`, `StatusView`, `DialogView`, `TaskView`;
- maximum sizes for strings, row batches, notices, task lists and preview bytes.

Use stable externally tagged or internally tagged serde shapes. Pin them with a
golden JSON corpus under `crates/norte-ui-host/tests/golden/`. Generate a JSON
Schema for the bridge if schemars can describe every DTO without weakening the
runtime rules. This schema belongs to the UI bridge and is not added to
`proto.schema.json`.

Tests:

- every action/patch variant has one golden fixture;
- unknown action tag is rejected;
- future bridge version is rejected;
- oversized strings/collections are rejected or truncated only at the
  documented safe display boundary;
- `sequence` gap requests a snapshot;
- stale instance/generation actions cannot mutate host state;
- serialized DTOs never include native `PathBuf`, `OsString`, raw secret fields
  or debug representations of `VPath`.

### Task 2.2: single-writer controller and subscription

Implement the actor loop with bounded channels.

The first fake backend is deterministic and returns proto entries. Do not need a
daemon for controller unit tests.

Tests:

- start produces exactly one sequence-0 snapshot;
- two dispatch clones still have one state writer;
- actions are applied in order;
- a slow subscriber causes snapshot recovery, not unbounded memory;
- subscriber drop does not stop the host;
- host drop/shutdown ends background tasks;
- an internal error produces one typed fatal notice and a usable shutdown;
- no lock is held across backend I/O.

Use an internal trait only where it enables deterministic tests. Do not create a
second public backend abstraction mirroring every SDK method.

### Task 2.3: browser pane, listing and navigation

Reuse `norte_frontend::PaneState`, layout slot ids, sorting, display and
viewport helpers.

Implement:

- initial list;
- pagination/stream drain policy;
- visible-window projection;
- cursor movement and direct row selection;
- open directory, parent, history back/forward;
- hidden toggle;
- quick search;
- configured columns and attr catalogue;
- watcher refresh and degradation;
- generation guards;
- empty/loading/error/skipped states.

Tests use `norte-testkit` hostile fixtures and a generated 100,000-entry listing:

- non-UTF-8 names survive as `VPath` and render with the canonical marker;
- CJK/emoji widths use the shared rules;
- sorting equals TUI/GPUI results for the same entries;
- a relist preserves or resolves cursor as current `PaneState` specifies;
- late list/decorate/stat results for an old generation are ignored;
- only the visible window plus overscan is serialized;
- moving one cursor does not resend every row;
- hidden slots stop watchers and list probes;
- unknown provider attrs do not become arbitrary HTML.

### Task 2.4: command/keymap/availability layer

Reuse `norte-frontend` command catalogue, resolver, presets, which-key,
availability and menu models.

The renderer sends normalized physical/logical key input only through a small
adapter. Rust resolves counts, prefixes, sacred keys and commands.

Tests:

- all keymap presets resolve identically to TUI fixtures;
- pending prefix and count updates are projected;
- sacred keys cannot be swallowed by a sequence;
- unavailable commands never dispatch an effect;
- menu, palette, shortcuts and which-key derive from one command catalogue;
- macOS `mod` mapping is adapter input, not a forked keymap;
- text-entry contexts prevent browser commands without destroying the pending
  resolver state incorrectly.

### Task 2.5: layout and session lifecycle

Implement:

- resolve configured/bundled layout;
- visible and hidden slot lifecycle;
- focus, active and target roles;
- renderer resize inputs to layout resolution;
- initial `session.get` and owner/detached state;
- apply `SessionBody`;
- capture/prune/coalesced `session.put`;
- revision conflict re-read;
- reconnect/handover persistence;
- shutdown flush;
- unsupported slot preservation.

Tests:

- all five bundled layout presets resolve at representative window sizes;
- resizing never rewrites stored layout intent;
- hidden slots suspend work;
- target role never points at a hidden slot;
- an unsupported kind round-trips `params` byte-for-byte through session save;
- future `SCHEMA_VERSION` starts from configuration without overwriting it;
- detached host does not write;
- ownership gained later adopts the correct revision per ADR 0059;
- conflict cannot silently overwrite another client;
- marks do not enter the session;
- session body caps stay those of `norte-frontend`, not renderer constants.

### Task 2.6: tasks, dialogs and safe effects

Create toolkit-neutral pending operations and dialogs. Refactor reusable types
out of GPUI `modal.rs` only when they are genuinely shared; do not make the host
depend on the GUI crate.

Implement:

- task registry and foreign tasks;
- progress projection and coalescing;
- cancellation;
- connection lost/restored notices;
- degraded connections;
- policy approvals;
- journal status required by the selected backend mode;
- modal confirm/cancel with stable ids;
- quit confirmation based on live operations/session state.

Tests:

- destructive action requires the same confirmation as keyboard/menu/drag;
- stale/double modal confirmation is a no-op;
- task terminal is never dropped under patch pressure;
- cancel is idempotent;
- reconnect resync neither duplicates nor loses tasks;
- an operation submitted before renderer disconnect remains visible after
  reattach;
- policy approval data is bounded and hostile path display is canonical;
- no dialog string is hard-coded outside Fluent resources.

### Task 2.7: headless parity harness

Create a table-driven harness that runs semantic scenarios against:

- the existing shared frontend primitives directly;
- `norte-ui-host` actions/snapshots;
- optionally the current GPUI/TUI state adapters where practical.

Initial scenarios:

```text
list -> move -> mark -> copy confirmation -> cancel
open dir -> back -> forward -> parent
quick search -> cycle -> clear
layout switch -> hide tab -> restore tab
session capture -> serialize -> apply
daemon lost -> restored -> task resync
compare start -> batches -> terminal
sync plan -> approve -> apply -> terminal
```

The harness compares semantic state, never pixel output.

### Phase 2 exit gate

- Host tests run without display and without Node.
- Initial navigation/session/task scenarios work against a real daemon test.
- No Tauri types in the dependency graph.
- Host serializes bounded windowed views, not whole-directory snapshots on each
  action.
- Hostile corpus and session round-trip tests pass.
- TUI and GPUI remain green and functionally unchanged.

---

## Phase 3 — Tauri vertical-slice spike and go/no-go

This phase proves the complete path before committing to feature parity.

### Task 3.1: scaffold the application reproducibly

Create `crates/norte-gui-tauri` with:

- Tauri 2 versions pinned in Cargo.lock;
- Vite + TypeScript;
- one chosen UI framework or plain TypeScript, recorded in the ADR;
- a committed package-manager lockfile;
- formatter, linter, typecheck and unit-test scripts;
- no SSR or runtime development server in production;
- packaged local assets only;
- build commands integrated into `justfile` without entering default Rust CI
  until the spike gate is stable.

Choose React, Svelte, Solid or another DOM framework based on team familiarity
and measurable table/list ergonomics. The choice must not leak into the host
contract. Record dependency benefit, size, maintenance and alternatives.

### Task 3.2: implement the minimal Tauri adapter

The Rust application:

- parses CLI/env startup exactly once;
- resolves socket and initial `VPath` through existing Rust helpers;
- creates `RemoteBackend` and `UiHost`;
- exposes `initial_snapshot`, `dispatch` and `request_snapshot` commands;
- emits ordered updates to the main window;
- handles window close through `UiHost::shutdown`;
- initializes file logging without leaking data;
- displays connection/startup failure inside the window.

No feature-specific invoke command is added when `UiAction` already expresses
it.

Tests with a mock window/event sink verify command validation and event order
without opening a display.

### Task 3.3: secure the webview boundary

Create a minimal capability file and CSP.

Explicitly test/inspect:

- navigation to `https://example.invalid` is refused;
- `window.open` cannot create an unrestricted webview;
- renderer cannot read arbitrary files;
- renderer cannot spawn processes or invoke shell;
- external link action rejects non-allowlisted schemes;
- all invoke payloads go through typed deserialization and caps;
- plugin/help strings render as text nodes;
- production assets contain no remote scripts, eval or development websocket;
- debug tooling is disabled or gated in release.

Run a `security-reviewer` before the first real mutation is ever wired, even
though the spike is read-only.

### Task 3.4: render the vertical slice

Required UI:

- one application window;
- current layout with two browser panes;
- virtualized rows and configured columns;
- focus/active/target indication;
- cursor and marks;
- keyboard navigation and quick search;
- mouse click, double-click, wheel and basic range mark;
- connection status and one task-progress surface;
- one confirmation dialog exercised with a non-destructive fake action;
- semantic accessibility roles and labels;
- theme colors and Fluent locale from Rust-projected state.

Do not implement compare/sync/settings/plugins here.

### Task 3.5: spike test matrix

Automated:

- renderer unit tests with a fake bridge;
- bridge sequence/gap/reconnect tests;
- DOM accessibility scan;
- keyboard scenario tests;
- component tests for empty/loading/error/hostile rows;
- Rust adapter tests;
- real-daemon smoke test under a virtual/real display;
- production build and package smoke test.

Manual on Linux Wayland and X11:

- IME/text input;
- clipboard;
- fractional scaling and 100/150/200% scale;
- light/dark themes;
- screen reader traversal with Orca/AT-SPI;
- keyboard-only complete navigation;
- drag selection and context menu;
- daemon handover while window remains open;
- compositor/window restore behaviour.

### Task 3.6: performance measurement and go/no-go review

Measure with the Phase 0 method. Initial budgets, amendable only before the
review:

| Metric | Gate |
| --- | --- |
| warm start to first real listing | ~~no more than 20% slower than GPUI baseline~~, target <= 500 ms |
| cold start | target <= 3 s on baseline machine |
| cursor input-to-paint p95 | <= 50 ms **and <= idle frame + one frame** |
| continuous local scroll | p95 frame **<= the platform's idle frame** (was: <= 20 ms) |
| 100,000-entry directory | no full-directory JSON resend on cursor/mark change |
| idle RSS | recorded and explicitly accepted; unexplained growth across 30 min is a failure |
| patch payload | routine cursor patch <= 16 KiB; row-window patch explicitly capped |
| reconnect | ~~no semantic regression against GPUI~~ no task/session semantic regression against the terminal frontend |

**Amended 2026-08-20, after the measurement** (condition 1 of the go/no-go,
`docs/spike-tauri-2026-08-20.md`):

- **The two GPUI comparisons are struck.** That frontend was retired before
  this one existed (ADR 0065), so there is no baseline to be 20% of. Parity is
  measured against the terminal frontend and against `norte-frontend`'s tests.
- **The frame budgets are relative to the platform, not to 60 Hz.** Measured
  on the reference machine, an idle WebKitGTK page gets a frame every 32–33 ms
  — about 30 Hz, on displays running at 100 and 144 Hz. A fixed 20 ms budget
  is unreachable there by construction, and a renderer that meets the idle
  frame exactly is adding nothing. So the question the gate asks is "how much
  does the renderer add to the platform's own cadence", and the answer has to
  be: nothing for local scroll, at most one frame for a round trip.
- **Every run records the idle frame first.** A latency number without the
  floor beside it cannot distinguish a slow renderer from a slow compositor;
  the measurement pass emits `idle-frame` for exactly this reason.

Go if:

- security boundary is narrow and review has no unresolved BLOCKER/MAJOR;
- keyboard, hostile names, virtualization and accessibility work;
- package runs on clean supported Linux installations;
- performance meets or has an accepted, evidenced exception;
- no new presentation rule has been implemented independently in TypeScript;
- team accepts WebKitGTK behaviour and build/distribution dependencies.

No-go or pause if:

- correct interaction requires raw daemon/filesystem access in the renderer;
- bridge traffic cannot meet list/cursor budgets without moving semantic state to
  TypeScript;
- accessibility tree cannot represent the dense pane interaction;
- WebKitGTK platform variance breaks required input/rendering on supported
  distributions;
- packaging is no more reliable than the GPUI status quo.

On no-go, keep `norte-client` and `norte-ui-host`, archive the spike and run a
small Slint/Iced renderer evaluation against the same host. Do not undo the
reusable extractions.

---

## Phase 4 — Read-only graphical beta

Implement in vertical slices. Each task includes host model, renderer,
accessibility, tests and parity evidence; do not build all host models first and
all screens later.

### Task 4.1: complete browser panes and layouts

- N-pane layout tree, tabs and collapse-on-size resolution;
- layout picker and factory/user presets;
- processes, places and metadata slots;
- active/target role chrome;
- resize, grow/shrink and tab commands;
- hidden-slot suspension;
- per-slot visible-range and watcher lifetime;
- session restore across layouts.

Parity checklist: every bundled preset, one-browser destination prompt,
unsupported kind box, screen-size non-persistence and slot-id preservation.

### Task 4.2: columns, decorations and dense-list behaviour

- column picker and scheme-specific configuration;
- attr catalog labels and fallback sanitisation;
- plugin column values for visible page only;
- plugin decorations and badges;
- sort indicators and click-to-sort where command model allows it;
- very large directory pagination;
- skipped/omitted entry warning.

Performance test ensures plugin values and attrs are requested only for visible
rows/overscan and hidden slots do no work.

### Task 4.3: viewer and preview

- bounded text viewer with encoding detection;
- binary/hex or existing fallback behaviour;
- image preview with explicit decode/size caps;
- plugin preview and styled preview;
- search/navigation within viewer if present in current feature set;
- external opener/terminal commands through Rust only;
- accessibility description and keyboard contexts.

Never pass arbitrary file paths to `<img src="file://...">`. Decode through
the host or a narrowly scoped safe asset protocol with a separate security
review.

### Task 4.4: help, palette, shortcuts and which-key

- canonical help corpus and markdown-lite renderer;
- contextual help and availability reasons;
- command palette;
- shortcuts/reference sheet;
- which-key/prefix/count surface;
- plugin help with publisher/truncation/lossy badges;
- localized keyboard labels.

DOM tests confirm no raw HTML injection and semantic heading/list/dialog
structure.

### Task 4.5: settings, themes and extensions read views

- settings registry and effective values;
- theme semantic roles and effects supported by the new renderer;
- plugin list, approval/enabled state display;
- plugin config schema read view;
- connection/volume pickers;
- diagnostic/log location view without revealing secret values.

Theme `effects` are renderer-specific interpreters. Unsupported effects degrade
visibly and safely; they do not change shared theme parsing.

### Phase 4 exit gate

- A user can browse, inspect, search, configure layouts and restore a session
  without GPUI.
- Read-only feature matrix is complete.
- No mutation button is active unless Phase 5 supplies the safe effect path.
- Accessibility and keyboard suites cover every new surface.
- Package is labelled beta/experimental and installs beside GPUI.

---

## Phase 5 — Mutations, tasks and safety parity

Every mutation task requires a failing test first, daemon integration coverage,
typed errors, confirmation parity, progress/cancellation and journal/undo
evidence. Renderer tests alone are insufficient.

### Task 5.1: mkdir, copy, move and delete

> **DONE 2026-08-21** (`main b4b4e607`, bridge **23**, ADR **0070**). All four
> operations go through confirmation, a daemon Task, the journal, the board,
> cancellation and a refresh of the directories they changed. The renderer
> names neither operand: `pane.copy` carries nothing, and Rust derives the
> sources from the focused slot's marks and the destination from the slot
> holding `RoleId::Target` — which now follows the shared rule of ADR 0058 D7
> instead of a local one, so with several candidates and none designated the
> transfer asks you to pick.
>
> Drag-and-drop has nothing to attach to yet: the renderer has no drag
> handlers. When it grows them, the design already forces the right answer —
> there is no action shape that carries a path, so a drop can only dispatch the
> same pending transfer.
>
> Three reviews ran before the commit (`security-reviewer`, `encoding-auditor`,
> `rust-reviewer`): one BLOCKER and eight MAJOR applied, four findings deferred
> as issues #268–#271. Effects stay `SoloLectura` until task 5.4, and
> `webview_boundary.rs` now pins that.


For each operation:

- derive selected paths in Rust from current slot state;
- compute availability through shared rules;
- open the canonical confirmation/policy surface;
- submit through `RemoteBackend`;
- register observer before races can lose terminal progress;
- show progress and current path safely;
- support cancellation;
- show typed terminal result;
- expose undo/irreversible status where current product does;
- refresh affected panes through watchers/generation guards.

Drag-and-drop dispatches the same pending transfer action as keyboard/menu. A
drop never becomes a separate silent mutation path.

Tests include destination collision, read-only provider, trash versus permanent
delete, cancellation and daemon loss.

### Task 5.2: batch rename and AI rename plan

> **DONE 2026-08-22** (bridge **24**, ADR 0070 extended). Single rename
> (`shift+F6`) and the AI plan both land. The rules that matter: an untouched
> name field sends the ORIGINAL BYTES and a touched one carrying U+FFFD is
> refused; a plan is validated WHOLE before it is shown and the core's verdict
> arrives in a second trip; approving sends the `plan_hash` the core returned.
> A request carries its own directory, so a plan landing after the reader
> navigated opens over the directory it was planned for — and one that arrives
> after they closed the review does not reopen it.
>
> Two things this task did NOT build, deliberately: a MANUAL batch rename (the
> TUI has no such surface either — the batch path is reached through the AI
> plan) and `fs.rename_batch_report` beyond the `HostBackend` method. The
> report is the only signal that a batch left the directory half-done, and
> showing it belongs with the task board in 5.3 — tracked as **#272**, which is
> BLOCKING for 5.4.
>
> Three reviews ran before committing: **two BLOCKERs and thirteen MAJORs**,
> all applied. The two that generalise beyond this task: a surface that opens
> BY ITSELF cannot be answered by the next keystroke (the first key only
> acknowledges, `Enter` stopped approving, and there are buttons), and the
> paint order has to match the key order — the review took the keyboard while
> seven full-screen panels painted over it. Further debt: #273, #274, #275.


- manual batch rename plan and collision display;
- AI plan request, journal precondition and limits;
- review/edit/approve path;
- typed stale/collision/error handling;
- execution task and report;
- cancellation before effect and during allowed phases;
- hostile names remain correlated by opaque row/path identity.

The renderer never constructs rename pairs from displayed names.

### Task 5.3: task board, approvals, journal and undo

> **DONE 2026-08-22** (ADR 0070 extended again; no bridge bump — everything
> here fitted the shapes that already cross). What it added: `task.cancel`
> from the window (the board's cursor decides which task when the process
> panel has the focus, the most recent live one otherwise), the batch report
> (**#272**, which was blocking 5.4) and its twin for undo, approval TTL with
> a dialog that closes itself and dedupe against the `policy.pending` resync,
> and the three persistent banners the status bar was painting from nobody:
> plaintext session, daemon going away, journal-refused mutation.
>
> Three things worth carrying forward:
>
> - **`ConnEvent` gained `GoingAway { reconnect }`.** A handover and a shutdown
>   look identical the moment the connection drops; the daemon's warning
>   beforehand is the only thing that tells them apart, and the SDK was keeping
>   it to itself (it used it to decide whether to respawn). The TUI got the
>   message too.
> - **A task that is already terminal when first observed is never announced as
>   foreign** — by SDK design, there is nothing to subscribe to. It is why the
>   cross-client e2e test has to slow the provider down: against a memory
>   provider a copy finishes before the observer hears about it, and a test
>   that passed by accident here would have proved nothing.
> - **What was NOT built: the gesture that LAUNCHES `policy.undo_session`** —
>   tracked as **#276**. The report is built and shown; the trigger needs a
>   surface where an agent session is a nameable, selectable thing, and that
>   belongs with task 6.4. A prompt asking someone to type a session id would
>   be a governance surface whose operand is typed by hand, which is what this
>   whole phase avoids. The TUI does not have it either, so this is not a
>   parity regression: it is a capability that today lives only in the CLI.
> - **No throughput figure is painted.** `current` (which item) travels and the
>   percentage now falls back to entry counts, but there is no bytes-per-second
>   or ETA — the TUI does not show one either, and inventing one for the window
>   alone would be a second answer to a question the two frontends should
>   answer identically.

- all local and foreign tasks;
- progress rate/current item;
- cancel actions;
- policy approval queue and TTL semantics;
- connection degraded and daemon going-away notices;
- journal guarantee display;
- undo session/report workflow;
- quit with live tasks/session flush.

Test multiple connected frontends: TUI + GPUI + Tauri against one daemon. A task
started by one appears correctly to the others according to existing actor
visibility rules.

### Task 5.4: mutation security review

> **DONE 2026-08-22.** `EFECTOS` is `Completo`: the window writes. What
> supports that is written in the constant's own rustdoc
> (`norte-gui-tauri/src/startup.rs`) so it is read where it is changed, and
> `tests/webview_boundary.rs` still pins it — now in the other direction, so
> going back to `SoloLectura` is also a decision rather than a merge.
>
> **The named reviewer agents were NOT dispatched** — this session was not
> allowed to run agents. The audit was done by hand against the same four
> lenses, and it found four things, all fixed with a failing test first:
>
> - **A persistent journal notice that could not turn itself off.** A daemon
>   that refuses a mutation for lack of a journal may recover, and nothing
>   announces it (the TUI has a signal only because its journal is embedded).
>   An accepted mutation IS the proof, so it clears the notice — an indicator
>   that cannot say "it's fine now" lies about the only thing it describes for
>   the whole session.
> - **An approval that never reached the daemon was silent.** `policy.decide`
>   is fire-and-forget; if the daemon died between the question and the yes,
>   the window treated the operation as authorized while it stayed denied by
>   silence. Approving now says so when it does not land. Denying stays
>   silent on purpose: if THAT does not arrive, the outcome is the one asked
>   for.
> - **A reconnect wiped a batch report from the board.** The SDK re-announces
>   tasks, the registration re-projects the row from a progress snapshot that
>   knows nothing about the report, and the only signal that a directory was
>   left half renamed vanished exactly when the connection recovered.
> - **The corpus never reached the report bodies.** Now it does: a stuck path
>   with a bidi override is masked and flagged like every other path line.
>
> Two checklist items are answered by design rather than by code, and it is
> worth saying why: *sender/window identity* (one window, no navigation, CSP
> `default-src 'none'`, and the binary's own commands validate their types —
> Tauri capabilities only gate plugin commands) and *production devtools*
> (the `devtools` feature is not enabled anywhere in the manifest).
>
> **The reviewer agents ran afterwards** (same range), and they were right to:
> four BLOCKERs and nine MAJORs, applied in two passes (`763ecae4`,
> `e9e871d4`). What the hand audit had missed shares one shape — a promise
> written in one place and broken in another:
>
> - The board crosses the bridge capped at 256 rows and the cursor is an
>   index; the cap lived in one place and the cursor counted over the whole
>   map, so past 256 tasks the highlighted row and the cancelled task were two
>   different tasks. The DTO's own rustdoc states the invariant verbatim.
> - A batch born terminal never asked for its report — `registrar_task`
>   documents that exact race and handles it for the relisting, not for the
>   report.
> - The acknowledge rule was keyboard-only, and the pointer is the primary
>   input of this surface: dialogs paint in the same place with the same first
>   button, so a click already in flight over "Confirm" landed on the
>   "Approve" of an approval that had just arrived. The `EFECTOS` rustdoc
>   cited that guard as one of the four things holding the switch up.
> - The approval's hostile flag was computed by masking text the DAEMON had
>   already passed through its lossy pass, so it never fired for the most
>   dangerous class — while a zero-width space, which that pass does not
>   touch, did.
> - Task ids restart at 1 in every daemon, so after a handover a new task
>   inherited the old one's state. Rows now carry a connection epoch; the root
>   fix belongs in the scheduler (**#278**).
>
> The lesson for the next phase that wants to skip them: **a hand audit finds
> what you already know to look for.** Three of the four blockers are
> invariants this repository had already written down somewhere else.

Mandatory reviews:

- `security-reviewer` for webview-to-host authority, policy and journal paths;
- `encoding-auditor` for every path/filename projection;
- `rust-reviewer` for async ownership and stale guards;
- `protocol-guardian` only if wire/handlers changed.

Resolve every BLOCKER and MAJOR before enabling mutations in release builds.

### Phase 5 exit gate

> **MET 2026-08-22**, with one thing named rather than hidden: the reviewer
> agents of task 5.4 were not dispatched (the session forbade agents), so the
> audit is a hand one. Everything else below has tests behind it, and the
> cross-client behaviour is covered against a real daemon in
> `norte-ui-host/tests/daemon_e2e.rs`.

- All everyday mutations have behavioural parity.
- No renderer-supplied path string authorizes an effect.
- Confirmation is identical across keyboard, menu and drag.
- Cancellation and terminal progress cannot be lost on reconnect or renderer
  reattach.
- Journal/undo guarantees remain daemon-owned and demonstrable by integration
  tests.

---

## Phase 6 — Advanced feature parity

### Task 6.1: search and semantic search

> **DONE 2026-08-22 for the search half** (bridge **27**). Filesystem search
> with streaming batches, cancellation and stale protection was already built
> in phase 4; what this task added is the SEMANTIC side: `pane.semantic-search`
> — which the shared catalogue has bound since K2 and the host answered
> `NotHere` — now opens a query prompt and asks the index.
>
> The decisions worth carrying:
>
> - **Semantic results extend the search view rather than opening a second
>   one.** A row gains `score` and the view gains `semantic`. Two lists of
>   results drift apart, and the one you are looking at stops being the one
>   you navigate — the same argument the process panel already made.
> - **`None` is not `false`.** The index answers with paths and similarity, not
>   kinds, so a semantic hit carries `kind: None` and activating it opens the
>   containing directory with the cursor on it. Claiming "file" because it
>   usually is would be inventing the answer.
> - **`NotFound` here is not "no results"** — it is "that root has no rows in
>   the index", and reading it as an empty search leaves the reader believing
>   nothing resembles what they asked. It says what to run instead.
> - **It is in `MUTAN`.** It writes nothing, but the query LEAVES the process
>   towards the AI provider, exactly like the directory listing behind
>   `pane.ai-rename`. That list is now "writes **or** leaves the process",
>   because read-only removes both for the same reason.
> - **Cancellation is an abort.** There is no Task to cancel — it is a direct
>   call — so relaunching or closing the view aborts the future, which is what
>   makes the SDK send `rpc.cancel` and stops an embed plus an index sweep at
>   the other end.
>
> **Not built: triggering `index.build` / `index.embed`.** The shared catalogue
> has no command for either and the TUI does not offer them; they are CLI
> verbs. Their tasks DO appear on the board already, since they are
> `TaskKind::Index`/`Embed`. Adding a command would be a shared-surface
> decision, not a GUI one.

- filesystem search streaming batches;
- index query/build/embed tasks;
- semantic-search limits and validation;
- results navigation without path reconstruction;
- cancellation and terminal/batch ordering;
- stale result protection.

### Task 6.2: directory compare

> **DONE 2026-08-22** (bridge **28**). `pane.compare-dirs` — bound in the
> shared catalogue since spec 1 of the comparison work — opens a diff panel
> over the two panes: streamed rows, per-category filters with their counts,
> selection, side switching, navigation and cancellation.
>
> **The reuse the task asked for is total**: the model IS
> `norte_frontend::compare::CompareView`, the same one the TUI paints, and the
> host projects it. Nothing here re-pairs rows or decides a verdict, and the
> two things that would have been tempting to reimplement are exactly the two
> that already bit other surfaces:
>
> - **`status_line`, not a `bool`.** A first pass had `viva: bool` and would
>   have thrown away the distinction that IS the answer: a comparison that
>   lost batches is INCOMPLETE, and one whose channel closed without an
>   observed outcome is UNKNOWN. The CLI and the MCP tool each reported
>   "complete" for a lossy run before that enum existed.
> - **`navigation_target`, not a local rule.** A row's `Enter` goes to the
>   directory of the ACTIVE side — the row itself when it is a directory, its
>   parent when it is a file — and `None` when that side is empty does NOT
>   fall back to the other one. And the pane it navigates is the one belonging
>   to that side, not the focused one, or a reader looking at the right side
>   loses their left directory to go and see the right one.
>
> **The row window is the other decision.** The engine emits one row per
> paired name over the whole tree and nothing caps it — a cap would turn "are
> these two trees the same?" into half an answer — so what crosses the bridge
> is a window (`first_visible` + `total`), like a listing's. Rows are named by
> their `id`, never by position: a filter hides rows and would renumber them.

- compare configuration and start;
- streamed rows, confidence and reason cells;
- orphan size/stat hydration;
- cancellation and failed-before-snapshot semantics;
- side/path navigation;
- current compare snapshot/session rules.

Reuse `norte-frontend::compare`; do not reproduce row-pairing or display verdict
logic in TypeScript.

### Task 6.3: sync planning and apply

> **Split in two on purpose**, because the plan calls this the highest-risk
> surface: **phase A is the PLAN** (read-only: ask, stream, review) and phase B
> is APPLYING it (confirmation, task, report, trash). The reviewers run in
> between, not at the end.
>
> **Phase A DONE 2026-08-22** (bridge **29**). `pane.sync-dirs` asks for a plan
> from the active pane onto the target one and opens a panel with its steps,
> what blocks it, and whether it can be approved. Nothing here writes.
>
> The model is `norte_frontend::sync::SyncView`, the same one the TUI drives,
> and it earned its keep twice on the way in:
>
> - **The panel opens when the Task id is KNOWN, not before.** The shared model
>   uses that id to discard batches belonging to another plan; built with a
>   filler id it discarded its OWN, and the panel sat at zero steps and closed
>   saying "this plan cannot be approved". The state machine was right and the
>   host was wrong.
> - **`can_approve` is not "the plan closed".** It also requires that what
>   arrived accounts for what the daemon counted, class by class, and that no
>   step contradicts its own shape. Two of the test fixtures had to be
>   corrected to satisfy it — a `Copy` whose reversal was `RestoreTrash`, and
>   counts that summed differently — which is exactly the check working.
>
> Approving is deliberately absent: this pass only reads, and the panel's hint
> line says so rather than offering a key that does nothing.
>
> **Reviewed before phase B, which is the whole point of the split.**
> `security-reviewer` and `rust-reviewer` agreed on one BLOCKER and most of the
> majors; all are applied. What they found, and what generalises:
>
> - **A model's guard is only as alive as the field that feeds it.** The plan
>   Task's outcome never reached `SyncView::run`, so the clause that refuses to
>   approve a CANCELLED or FAILED plan — which the shared model documents as its
>   reason for existing — was dead here, and a cancelled plan crossed the bridge
>   saying it could be approved. A failed one left the panel reading
>   "planning…" forever.
> - **`take()` before the filter throws away the live request.** A stale Task id
>   discarded a newer pending plan, so no panel opened at all while two walks
>   kept running on the daemon. Filter first; and only one plan at a time.
> - **A screen that offers a key it does not have trains the reader to press
>   it** — on the screen where the next phase puts the writing.
> - **`sync_roots` exists so "which tree gets overwritten" has ONE answer.** The
>   host had rederived it from the roles, which already disagreed with the shared
>   rule when a diff panel is open. It now goes through the shared function,
>   encodings included.
> - **`summary_lines`, `blockers_total` and a blocker's own path are not
>   decoration**: without them an approvable plan with three irreversible steps,
>   an unreadable branch and 340 unmeasured files reads as "5 steps".
> - And a gate the commit had not run: the rustdoc link `Self::sync_apply`
>   pointed at a method the trait deliberately does not have, so `just docs`
>   was RED on main. `just t` and `gui-ci` are not the whole gate.
>
> **Phase B DONE 2026-08-22** (same bridge, 29): approve → the second question
> → apply → the report. `a` asks; the second question only exists when the plan
> deletes trees or leaves something without a way back, and only `y` answers it
> — asking every time is what teaches people to answer without reading. What
> goes out is the hash the CORE returned, through `SyncView::submit`, which is
> the one door: it checks `can_approve` and latches the in-flight apply in the
> same gesture, and this window reads events between keystrokes, so the window
> where a second `a` slips in is reachable here in a way it is not in the TUI.
>
> While the daemon is writing, `Escape` asks to cancel and does NOT close:
> closing loses the report — and with it the counts, the failures and the undo
> handle — over a destination that was rewritten halfway. When the apply task
> ends, the report is asked for and `on_apply_ended` reads the pair (outcome,
> report), so "cancelled after applying N" says both halves.
>
> One thing changed in the shared layer: `on_apply_ended` now takes the
> language. It was localising the error category with the process-global one,
> which for a window with a per-instance language is the wrong sentence.
>
> **Phase B reviewed too** (bridge **29 → 30**; phase B had added required
> fields without a bump, which is the whole reason the bridge is single-level).
> Both reviewers landed on the same theme: the panel asserted things it did not
> know.
>
> - **"It failed" is not "it did not write."** A failed `sync.apply` released
>   the latch and offered `a` again — but the daemon answering *no* and the
>   connection dropping *after* the request are different facts. In the second
>   case the request may have arrived, so re-offering apply offers to write the
>   same plan twice over the same destination. `Fondo::SyncNoAplicado` now
>   carries whether we KNOW nothing was written; the ambiguous case says so and
>   does not re-offer.
> - **The one who knows the id has to be the one who cancels.** When the shared
>   model refuses an apply — the reader asked to stop in the window where it had
>   no id yet — nobody else knows that Task id. The host cancels it.
> - **The writing panel was the only screen in norte with no way out.** The
>   second `Escape` now closes, saying the destination may be halfway. And
>   `Escape` on an already-finished apply no longer rewrites its outcome to
>   cancelled.
> - **An anchor is said, not inferred from a `data-` attribute nobody reads.**
>   `anchor_label` crosses the bridge: staying quiet about an `either` on a
>   panel where an unqualified path means "on the source" asserts the source.
> - **A fake canceller that counts nothing passes a cancellation test with the
>   panel frozen.** The fake's `sync_apply` now records who was asked to stop,
>   and the test asserts the apply's id — not the plan's — was the one asked.

- sync plan configuration;
- streamed steps and blockers;
- retained plan identity/hash;
- review and apply confirmation;
- task transition from plan to apply without orphaning either observer;
- report and cleanup;
- daemon handover/stale plan behaviour;
- cancellation and irreversible/opaque trash cases.

This is the highest-risk GUI surface. Require the same end-to-end fixtures used
by TUI/GPUI and a dedicated security review.

### Task 6.4: plugin governance, commands and config writes

> **First half DONE 2026-08-22** (bridge **31 → 32**). The extension manager
> governs: approve/revoke, enable/disable, a typed `[config]` editor over the
> shared `plugin_config` model, and the commands a plugin contributes, run from
> the palette with their output shown. Three rules carry it: approving ASKS and
> the question enumerates the capabilities one per line, each masked on its own
> and carrying its own flag; none of it exists in read-only mode; and after a
> change the CATALOGUE is fetched again rather than flipping a local boolean.
>
> **Two reviews, three BLOCKERs, thirteen MAJORs — all applied.** What
> generalises beyond this task:
>
> - **A flag computed from one string cannot describe three.** One `hostile`
>   for the plugin name, the command title and the output was derived from the
>   output — so a hostile manifest with ASCII output painted unbadged, and
>   since a newline is a C0 control, every honest multi-line run painted
>   badged. A flag that is true for everything honest and false for the one
>   hostile case is worse than no flag.
> - **"It failed" is not "it did not happen" — again, and this time in the
>   pessimistic direction.** A grant that timed out on OUR deadline left the
>   row saying "not approved" over capabilities the daemon had granted.
> - **A full-screen panel that only claims one key is not modal.** The output
>   panel painted over an open confirmation while the confirmation kept the
>   keyboard — and the moment was chosen by the PLUGIN, which decides when its
>   command answers. Paint order (DOM order, no `z-index` anywhere) and input
>   order have to agree.
> - **A consent question that truncates its list is not consent.** Showing
>   sixteen of forty capabilities while the yes grants forty is the whole hole.
>   Above the cap it now refuses to ask rather than asking about a part.
> - **A modal holds the keyboard, not the mailbox.** A catalogue landing
>   between the question and the yes could change what the yes granted, so the
>   answer re-compares against what the manifest declares now.
> - **Two halves of one row, and only the operand was moving.** Cycling a
>   `bool` wrote `false` and kept painting `true`; the next Enter wrote `true`
>   back. The daemon oscillated and the screen never moved.
>
> Debt filed: **#280** (the TUI approves without asking and trusts its own
> optimism), **#281** (the manifest caps a command's id and title but not how
> many commands there are), **#282** (a grant binds to the id, not to the
> capabilities that were read — needs `expected_digest` on the wire).
>
> Still open in this task: **#276**, the gesture that launches
> `policy.undo_session`, which needs a surface where an agent session is a
> nameable, selectable thing.

- approve/revoke and enable/disable;
- command execution and bounded output;
- typed plugin config editors for string/bool/int/enum;
- config validation and daemon round trip;
- unknown future config kind becomes read-only;
- preview/decoration/columns consent behaviour;
- help and publisher metadata.

Renderer never loads plugin-provided JavaScript, CSS or HTML.

### Task 6.5: shell integration and desktop affordances

- clipboard copy/paste through semantic host commands;
- open in external application;
- open terminal with pane cwd;
- drag-out/drag-in only after a platform/security design;
- file dialogs where a one-browser operation must ask for destination;
- desktop notifications only for explicitly selected events;
- correct macOS/Windows modifier naming when those platforms enter the gate.

Each native affordance gets a narrowly scoped capability, not general shell or
filesystem access.

### Phase 6 feature matrix

Before exit, compare current TUI and GPUI commands against the new host command
catalogue. For every command classify:

```text
supported
intentionally not applicable to GUI
deferred with issue
blocked by platform transport
```

No command may disappear merely because nobody remembered it. Update help and
availability from the same classification.

---

## Phase 7 — Distribution and platform support

### Task 7.1: Linux packaging matrix

Produce at least the formats selected by the release ADR, initially from:

- `.deb` for Debian/Ubuntu baseline;
- RPM for Fedora-family baseline;
- AppImage or Flatpak after measuring WebKitGTK/runtime implications.

Requirements:

- build on the oldest supported glibc/WebKitGTK baseline;
- package or depend on the correct WebKitGTK runtime explicitly;
- include icons, desktop entry, MIME declarations only where behaviour exists;
- preserve daemon/CLI discovery when GUI apps lack shell dotfile `PATH`;
- bundle or locate a compatible `norte` daemon deterministically;
- never start an arbitrary `norte` found earlier on an attacker-controlled path;
- sign/checksum release artifacts according to project release policy;
- install/uninstall without removing user config, state or journal;
- clean-machine smoke tests for Wayland and X11.

Decide whether GUI and CLI/daemon ship as one bundle or coordinated packages.
Version mismatch behaviour must be explicit and tested against the N/N-1 window.

### Task 7.2: macOS validation

On real macOS hardware/runners:

- build SDK/client/host/application;
- validate Unix peer authentication available on that platform;
- test NFD filenames and hostile corpus;
- test Keychain/config integration inherited through daemon;
- test menus, shortcuts, clipboard, opener and terminal cwd;
- test VoiceOver accessibility;
- sign and notarize app bundle/DMG;
- test daemon discovery outside shell environment;
- record any unsupported provider/platform module honestly.

Do not mark macOS supported based on cross-compilation alone.

### Task 7.3: Windows named-pipe prerequisite

Create a separate ADR before code. It must decide:

- named-pipe name/location;
- same-user server and client authentication;
- ACL creation and verification;
- server spoofing resistance;
- framing reuse;
- connect-or-spawn and handover;
- cleanup/stale pipe behaviour;
- test strategy on Windows CI;
- whether peer identity semantics exactly match Unix actors.

Then implement `norte-client::transport::windows` and the daemon listener behind
the shared transport interface. Run protocol, reconnect, task and security
tests on real Windows CI.

Only after this passes may the Tauri GUI claim daemon-mode Windows parity.
Embedded mode is not an acceptable silent fallback.

### Task 7.4: release automation

Integrate the GUI into release planning without unifying features into CLI/TUI
builds unexpectedly. Preserve `precise-builds` rationale from
`dist-workspace.toml`.

Automate:

- Rust and web dependency locks;
- reproducible production web build;
- application build per target;
- package smoke tests;
- signing/notarization inputs;
- SBOM/license review for Rust and JS graphs;
- update metadata if auto-update is explicitly selected;
- checksums and provenance;
- version alignment across `norte`, `ntc` and GUI.

No automatic update mechanism is added without deciding daemon handover, task
drain and signature verification end to end.

---

## Phase 8 — Cutover and retirement

### Task 8.1: side-by-side alpha

Ship the new binary under its temporary name. Document:

- how to launch each GUI;
- shared config/theme/keymap/session behaviour;
- that only one session writer exists and additional windows detach;
- how to collect diagnostics without path content;
- known platform/WebView differences;
- rollback to GPUI.

Collect structured local performance/error diagnostics only if the project has
an explicit opt-in policy; do not add network telemetry as part of this plan.

Run at least one full release cycle with both GUIs available.

### Task 8.2: default switch

Switch `norte gui` only when all are true:

- Phase 4–6 feature matrix has no unexplained gaps;
- mutation/security reviews are clean;
- Linux package install/upgrade/uninstall tests pass;
- session created by TUI, GPUI and Tauri round-trips without loss;
- daemon handover works with TUI and new GUI connected together;
- accessibility/keyboard release checklist passes;
- performance budgets pass on supported baseline;
- crash/restart and rollback procedures have been exercised;
- user-facing docs and screenshots describe the new default.

Possible naming sequence:

```text
existing GPUI: norte-gui-gpui
new Tauri:     norte-gui
launcher:      norte gui
```

Perform rename/package changes in one focused release commit, with compatibility
for scripts where practical.

### Task 8.3: GPUI deprecation and removal

Do not remove in the same release as the default switch. After at least one
successful alpha release:

- confirm no unique command/surface remains;
- archive its baseline and ADR outcome;
- remove GPUI Git dependencies and platform glue;
- move any remaining reusable state/tests into `norte-frontend` or host first;
- update cargo-deny/workspace/dist configuration;
- remove old packages and launcher branches;
- record removal and rollback boundary in changelog.

The removal is its own PR and full gate, not cleanup hidden inside another
feature.

## Test strategy

### Test pyramid

| Level | Runs without display | Purpose |
| --- | --- | --- |
| `norte-client` unit/integration | yes | transport, reconnect, tasks, feeds, typed calls |
| `norte-ui-host` state/contract | yes | semantic actions, sessions, generations, bounded updates |
| renderer unit/component | yes | DOM/widgets, focus, patch reducer, accessibility semantics |
| adapter integration | mostly | Tauri invoke/event validation and lifecycle |
| real-daemon GUI smoke | no | complete process/socket/window path |
| manual OS/accessibility | no | compositor, IME, screen reader, packaging reality |

### Mandatory cross-frontend scenarios

Run with TUI and new GUI concurrently against one daemon:

1. both list the same hostile directory;
2. TUI starts copy; GUI sees task and progress;
3. GUI starts compare; TUI remains responsive;
4. daemon handover restores both;
5. session owner closes; detached client later acquires ownership;
6. one frontend changes layout/session; the other does not overwrite it;
7. plugin approval and policy visibility follow current actor rules;
8. sync plan cannot be redeemed by another connection;
9. normal daemon stop is not resurrected by either client;
10. N-1 daemon/client combinations degrade according to protocol tests.

### Encoding/path suite

Every renderer release gate runs the canonical `norte-testkit` hostile corpus:

- invalid UTF-8;
- control characters and bidi hazards;
- newline/tab/escape sequences;
- normalization twins;
- very long names;
- archive/provider-specific byte names;
- Windows reserved/trailing names when Windows enters support.

Assertions cover visible label, accessibility label, clipboard/export result,
row identity and actual operation target. A display string must never be parsed
back into a path.

### Accessibility suite

Automated where possible:

- roles/names/states for every interactive component;
- focus order and focus restoration after dialog close;
- keyboard-only command scenarios;
- large text/zoom and high contrast;
- reduced motion;
- color-independent selection/status;
- live-region throttling for task progress;
- virtualized rows expose a coherent visible set, total count and current item.

Manual before each supported-platform release:

- Orca on Linux;
- VoiceOver on macOS;
- Narrator on Windows after named-pipe/platform support;
- IME and non-US keyboard layouts;
- mouse unavailable scenario.

### Performance and soak suite

- 100,000-entry synthetic directory;
- remote directory with paged high latency;
- ten live tasks with rapid progress;
- search/compare/sync batch bursts;
- 20 layout tabs verifying hidden suspension;
- repeated daemon loss/restore for 30 minutes;
- cursor/scroll/mark soak checking stable RSS;
- session save every quiet interval checking no UI stall;
- renderer detach/reattach under active tasks.

## Gate budget

Follow `CLAUDE.md`: use targeted `just t <crate>` in RED→GREEN, not the full
gate as debugger.

Recommended full-gate cadence:

- Phase 0: documentation checks only unless code/test fixture changes.
- Phase 1: one `ci-fast` mid-phase, one full `ci` at close.
- Phase 2: one `ci-fast` after every roughly three coupled tasks, one full `ci`
  at close.
- Phase 3 onward: renderer lint/typecheck/component suite on every renderer
  task; Rust targeted suites as needed; one combined GUI smoke gate per vertical
  slice; one full project `ci` per phase/branch close.

Add `just` recipes rather than invoking feature-incompatible bare Cargo worlds:

```text
just client-ci
just ui-host-ci
just gui-tauri-check
just gui-tauri-test
just gui-tauri-build
just gui-tauri-smoke
```

The exact recipes must reuse workspace feature sets and avoid causing CLI/TUI
builds to inherit renderer dependency features.

## Review matrix

| Change surface | Required review |
| --- | --- |
| client extraction, async host | `rust-reviewer` |
| proto type/JSON-RPC handler/wire shape | `protocol-guardian` |
| socket/named pipe, spawn, webview capabilities, mutations | `security-reviewer` |
| paths, names, viewer, clipboard, drag/drop | `encoding-auditor` |
| contract/e2e/performance harness | `test-engineer` where available |

Reviewers read diffs and do not run the expensive full gate. Apply BLOCKER and
MAJOR findings before committing; record deferred MINORs with issues/reasons.

## Performance budgets

Phase 0 replaces guesses with baselines. Until then these are provisional hard
targets for the new GUI on the same machine:

- warm first listing: <= 500 ms and no more than 20% slower than GPUI;
- cold window: <= 3 s;
- input-to-paint p95: <= 50 ms;
- scroll-frame p95: <= 20 ms for local visible rows;
- routine cursor update payload: <= 16 KiB;
- no full-directory payload caused by cursor, hover, focus or mark toggle;
- idle memory reaches a plateau; 30-minute browse soak grows by <= 10% after
  caches warm unless a documented bounded cache explains it;
- hidden slot produces zero watcher, pagination or plugin-column work;
- session persistence never blocks input for a frame budget;
- task terminal and destructive confirmation events are lossless under load.

Do not optimise by moving shared semantics into TypeScript. If a budget fails,
first change projection granularity, patch coalescing, serialization or host
data structures.

## Security checklist

- [ ] Packaged local renderer assets only.
- [ ] Restrictive CSP; no `eval`, remote script or unsafe inline execution.
- [ ] Minimal Tauri capabilities by explicit window.
- [ ] No generic shell/process/filesystem/network invoke.
- [ ] No direct daemon socket from renderer.
- [ ] Typed and capped action deserialization.
- [ ] Sender/window identity validated by adapter.
- [ ] External URL scheme/host allowlist.
- [ ] Plugin/help/filename text never injected as raw HTML.
- [ ] Row keys/generations validated before path lookup.
- [ ] Mutations derive `VPath`s from current Rust state.
- [ ] Secrets never enter snapshots, DOM, renderer logs or crash reports.
- [ ] Preview/image payloads bounded before decode and before serialization.
- [ ] Production devtools policy decided.
- [ ] Rust and JS dependency/license audit in release gate.
- [ ] Signing/notarization and update authenticity decided before auto-update.
- [ ] Named-pipe authentication complete before Windows daemon claim.

## Feature parity ledger

Maintain this table in the plan or a linked generated document as work lands.
Each row requires host, renderer, tests and accessibility—not merely a visible
button.

| Surface | Host | Renderer | Tests | A11y | Status |
| --- | --- | --- | --- | --- | --- |
| connection/reconnect/handover | [ ] | [ ] | [ ] | [ ] | pending |
| directory list/navigation/history | [ ] | [ ] | [ ] | [ ] | pending |
| sort/columns/attrs | [ ] | [ ] | [ ] | [ ] | pending |
| selection/marks/mouse/drag | [ ] | [ ] | [ ] | [ ] | pending |
| quick search/fs search | [ ] | [ ] | [ ] | [ ] | pending |
| layouts/tabs/roles | [ ] | [ ] | [ ] | [ ] | pending |
| places/metadata/processes | [ ] | [ ] | [ ] | [ ] | pending |
| viewer/image/plugin preview | [ ] | [ ] | [ ] | [ ] | pending |
| help/palette/which-key/shortcuts | [ ] | [ ] | [ ] | [ ] | pending |
| settings/theme/effects/i18n | [ ] | [ ] | [ ] | [ ] | pending |
| plugins list/governance/config | [ ] | [ ] | [ ] | [ ] | pending |
| mkdir/copy/move/delete | [ ] | [ ] | [ ] | [ ] | pending |
| batch rename/AI rename | [ ] | [ ] | [ ] | [ ] | pending |
| tasks/cancel/approvals/journal/undo | [ ] | [ ] | [ ] | [ ] | pending |
| index/semantic search | [ ] | [ ] | [ ] | [ ] | pending |
| compare | [ ] | [ ] | [ ] | [ ] | pending |
| sync plan/apply/report | [ ] | [ ] | [ ] | [ ] | pending |
| session ownership/persistence | [ ] | [ ] | [ ] | [ ] | pending |
| clipboard/openers/terminal | [ ] | [ ] | [ ] | [ ] | pending |
| packaging/install/upgrade | n/a | [ ] | [ ] | n/a | pending |

## Risks and mitigations

| Risk | Consequence | Mitigation / stop condition |
| --- | --- | --- |
| UI behaviour is reimplemented in TypeScript | TUI and GUI become different file managers | D14, host parity tests, code review rejects duplicated rules |
| `norte-client` extraction accidentally changes races | lost tasks/batches or daemon resurrection | move-first commits, existing race tests, security/Rust review |
| host-to-renderer JSON is too chatty | input lag and memory pressure | windowed rows, bounded patches, coalescing, snapshot-on-overflow |
| webview compromise reaches files | arbitrary user-data mutation | no raw APIs, semantic actions, capabilities/CSP, Rust state authority |
| WebKitGTK differs across distros | rendering/input/package failures | clean-machine matrix, oldest baseline, go/no-go spike |
| Tauri app links all of core again | build/size remain poor and boundaries blur | `norte-client` forbidden-dependency test |
| session body is ported to JS | unknown state lost on save | host owns `SessionBody`; round-trip fixtures |
| virtualization breaks accessibility | screen reader cannot navigate panes | visible semantic rows, total/current metadata, manual AT gate |
| Windows is advertised prematurely | GUI starts without daemon guarantees | named-pipe task is explicit prerequisite; no silent embedded fallback |
| two GUI implementations double maintenance | feature changes land in only one | time-box coexistence, shared host, parity ledger, eventual retirement gate |
| JS dependency graph grows unchecked | supply-chain/build burden | minimal framework/deps, lockfile, license/audit, hard-rule-8 justification |
| native opener/clipboard shortcuts bypass policy | unsafe effects | narrow Rust commands; effects still derive from semantic host state |
| renderer crash loses UI ownership/session | user returns to stale screen | daemon session, host reattach snapshot, shutdown/recovery tests |

## Rollback strategy

At every phase:

- TUI remains installable and complete.
- GPUI remains launchable until Phase 8.3.
- New crates are additive and can remain even if the Tauri application is
  removed.
- The launcher default does not change before Phase 8.2.
- Session schema and daemon protocol are not forked for Tauri.
- Packages use distinct binary names during alpha, so uninstalling the new GUI
  cannot remove the old one.

If a release of the new GUI is bad, repoint `norte gui` to GPUI and publish a
package update. No data migration is required because both use the same daemon,
configuration and Rust-owned session schema.

## Effort estimate and sequencing

Very rough estimate for one experienced engineer, after baselines:

| Work | Engineer-weeks |
| --- | ---: |
| Phase 0 decisions/baseline | 1 |
| Phase 1 client extraction | 2–4 |
| Phase 2 UI host foundation | 3–5 |
| Phase 3 Tauri spike | 2–3 |
| Phase 4 read-only beta | 3–5 |
| Phase 5 mutations | 3–5 |
| Phase 6 advanced parity | 4–7 |
| Phase 7 packaging/platforms | 2–5 excluding Windows transport |
| Phase 8 cutover | 1–2 plus one release observation period |

Total to Linux feature parity is likely **16–30 engineer-weeks**, not a small
GUI rewrite. Windows named-pipe work is additional and should be estimated from
its ADR.

Safe parallel work is limited:

- after bridge goldens exist, renderer components can proceed against a fake
  bridge while Rust host vertical slices land;
- packaging automation can begin after the Phase 3 production build is stable;
- accessibility fixtures can accompany renderer components;
- `norte-client` extraction and `norte-core::Backend` adaptation are tightly
  coupled and should not be implemented concurrently in one worktree;
- compare/sync and mutation work share task/dialog state and should land
  sequentially unless isolated worktrees and explicit interfaces exist.

## Definition of done

The architecture transition is complete only when:

- `norte-client` is a provider/core-independent SDK with preserved reconnect,
  task and feed semantics;
- `norte-ui-host` is toolkit-independent, headless-tested and the semantic owner
  for the new GUI;
- TUI retains every existing feature and gate;
- new GUI reaches the complete feature parity ledger or documents an explicitly
  accepted non-applicable item;
- hostile filenames are never round-tripped through display text;
- every mutation still goes through daemon, policy and journal;
- session/layout state round-trips between TUI, GPUI and new GUI;
- accessibility and keyboard gates pass on supported platforms;
- performance budgets and soak tests pass;
- supported-platform packages install, upgrade, run and uninstall on clean
  systems;
- release/security/license reviews are complete;
- launcher cutover has a tested rollback;
- GPUI is retired only in a later focused release after the new default has
  proven stable.

## First action when this plan is resumed

> **Superseded 2026-08-20.** Phases 0, 1 and 2 are done (ADR 0065, ADR 0066);
> `norte-client` and `norte-ui-host` exist, with their dependency boundaries
> enforced by tests. The instruction below is kept because its REASON still
> holds for whoever resumes at phase 3: check first, scaffold second.

**Resuming at phase 3:** do not scaffold Tauri first either. Read ADR 0066 and
this plan's decisions D2, D7, D8 and D11; confirm the host's public surface
has not moved (`cargo doc -p norte-ui-host`); and only then start Task 3.1.
The spike exists to answer a question with measurements — Task 3.6 — not to
produce a window as fast as possible. A vertical slice that looks good and
starts in three seconds is a NO-GO, and finding that out is the point.

*(Original instruction, now done: start with Task 0.1, validate that the
current ADRs and protocol version have not moved, refresh the code/line
metrics, and then execute the `norte-client` extraction. That order is what
makes the work produce reusable architecture instead of a second GUI-specific
bridge.)*
