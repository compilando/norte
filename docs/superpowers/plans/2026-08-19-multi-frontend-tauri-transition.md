# Multi-frontend architecture and Tauri GUI transition — implementation plan

> **Status:** proposed; no implementation has started.
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
- Do not delete, rename or freeze the GPUI frontend during the construction
  phases.
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

The GPUI binary remains available as the graphical behavioural oracle until the
new GUI has passed parity and shipped for at least one alpha release.

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
| warm start to first real listing | no more than 20% slower than GPUI baseline, and target <= 500 ms |
| cold start | target <= 3 s on baseline machine |
| cursor input-to-paint p95 | <= 50 ms |
| continuous local scroll | p95 frame <= 20 ms for visible-row work |
| 100,000-entry directory | no full-directory JSON resend on cursor/mark change |
| idle RSS | recorded and explicitly accepted; unexplained growth across 30 min is a failure |
| patch payload | routine cursor patch <= 16 KiB; row-window patch explicitly capped |
| reconnect | no task/session semantic regression against GPUI |

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

- manual batch rename plan and collision display;
- AI plan request, journal precondition and limits;
- review/edit/approve path;
- typed stale/collision/error handling;
- execution task and report;
- cancellation before effect and during allowed phases;
- hostile names remain correlated by opaque row/path identity.

The renderer never constructs rename pairs from displayed names.

### Task 5.3: task board, approvals, journal and undo

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

Mandatory reviews:

- `security-reviewer` for webview-to-host authority, policy and journal paths;
- `encoding-auditor` for every path/filename projection;
- `rust-reviewer` for async ownership and stale guards;
- `protocol-guardian` only if wire/handlers changed.

Resolve every BLOCKER and MAJOR before enabling mutations in release builds.

### Phase 5 exit gate

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

- filesystem search streaming batches;
- index query/build/embed tasks;
- semantic-search limits and validation;
- results navigation without path reconstruction;
- cancellation and terminal/batch ordering;
- stale result protection.

### Task 6.2: directory compare

- compare configuration and start;
- streamed rows, confidence and reason cells;
- orphan size/stat hydration;
- cancellation and failed-before-snapshot semantics;
- side/path navigation;
- current compare snapshot/session rules.

Reuse `norte-frontend::compare`; do not reproduce row-pairing or display verdict
logic in TypeScript.

### Task 6.3: sync planning and apply

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

Do not scaffold Tauri first. Start with **Task 0.1**, validate that the current
ADRs and protocol version have not moved, refresh the code/line metrics, and
then execute the `norte-client` extraction. That order is what makes the work
produce reusable architecture instead of a second GUI-specific bridge.
