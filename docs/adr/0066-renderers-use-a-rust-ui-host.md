# 0066 - Renderers use a Rust UI host, and a client SDK below it

- Status: accepted
- Date: 2026-08-20
- Decision makers: Oscar González
- Related: ADR 0027 (GPUI feasibility, superseded in practice by ADR 0065),
  ADR 0065 (a frontend is retired before its replacement exists), the plan
  `docs/superpowers/plans/2026-08-19-multi-frontend-tauri-transition.md`
  (decisions D1–D14), `docs/architecture-review-2026-08-20.md`.

## Context and problem statement

norte needs a graphical frontend again. The one it had was written directly
against `norte-core` and GPUI, and that produced the two costs ADR 0065
records: a second copy of presentation rules, and a frontend that had to be
excluded from the gate.

Building the next one raises a question the previous one never answered: **what
exactly is a frontend allowed to depend on?** The current shape is

```text
frontend → norte-core → engine, providers, index, plugin host, journal
```

even for a frontend that only ever talks to a daemon over a socket. A remote
GUI pulls in the entire filesystem authority just to send JSON-RPC, and the
only reason is that `RemoteBackend`, the framed client, the reconnection
lifecycle and the feed routing all live inside `norte-core/src/backend.rs`
beside the embedded engine facade.

The second question is where a renderer that is NOT Rust — a webview, a Flutter
shell — is allowed to keep state. Every answer that lets the renderer own
semantic state ends with presentation rules reimplemented in JavaScript, which
is exactly what `norte-frontend` exists to prevent.

## Options considered

### Option A — Write the new GUI directly against `norte-core`, as before

- **Advantage:** nothing to extract; start rendering immediately.
- **Drawback:** repeats the coupling that made the previous frontend
  expensive, and keeps a socket client welded to an engine it never uses.
- **Drawback:** a non-Rust renderer would have no boundary at all, so its state
  would grow into a second presentation engine.

### Option B — A client SDK plus a toolkit-independent UI host

Two new crates:

- `norte-client`: the framed JSON-RPC client, transport, reconnection, remote
  task primitives and `RemoteBackend`, depending only on `norte-proto` and
  runtime crates.
- `norte-ui-host`: the semantic state owner, on top of `norte-client` and
  `norte-frontend`, exposing `UiAction` in and versioned, sequenced
  `UiUpdate` out.

- **Advantage:** a renderer — Tauri today, something else tomorrow — is an
  adapter over a typed bridge, not an architecture layer.
- **Advantage:** the existing embedded path (`ntc` with the engine in-process)
  is untouched: `norte-core::Backend::Remote` becomes a thin wrapper over the
  SDK.
- **Advantage:** phases 1 and 2 are worth doing even if the Tauri spike fails.
- **Drawback:** two new crates and a substantial move before a single pixel is
  drawn.
- **Drawback:** some core-only value types (`TransferOptions`, sync plan
  events, volumes) need an SDK form and an exhaustive mapping.

### Option C — A fully Rust GUI (Slint, Iced) against `norte-frontend`

- **Advantage:** no bridge, no serialisation, no web toolchain.
- **Drawback:** it does not answer the non-Rust renderer question at all, so
  the boundary would still have to be invented later.
- **Drawback:** the toolkit choice becomes load-bearing again, which is the
  mistake ADR 0065 is paying off.

## Decision

**Option B**, with the plan's decisions D1–D14 accepted as written, and D1
amended by ADR 0065 (the GPUI frontend is already gone; the boundaries are
additive, the frontends are not).

The decisions this ADR freezes, in one line each:

- **Tauri 2 is the reference renderer** and nothing but the application crate
  may depend on it. Electron, Flutter or a plain protocol client remain
  possible because the bridge is typed, versioned and toolkit-neutral.
- **`norte-client` is extracted before the GUI grows**, and a dependency test
  fails the build if `norte-core`, any provider, `norte-vfs`, `norte-index`,
  `norte-ai`, `norte-plugin-host` or `norte-frontend` ever reaches it.
- **`norte-ui-host` owns semantic state**; the renderer owns only ephemeral
  view state (scroll position, focus ring, animation).
- **Updates are ordered, versioned and bounded**: one sequence per host
  instance, snapshots replace, patches declare their base, unknown bridge
  versions fail loudly instead of interpreting half a message.
- **Raw paths never become renderer authority.** The renderer addresses rows by
  opaque ids; `VPath` bytes stay in Rust.
- **Sessions and layouts stay in Rust.** The session body is opaque to the core
  and owned by `norte-frontend`; no renderer rewrites it.
- **All effects continue through the daemon.** The webview gets no filesystem,
  no shell and no `rpc(method, params)` escape hatch.
- **The reference GUI is daemon-only**, and Windows named pipes remain a
  separate transport milestone, not a side effect of changing toolkit.
- **Accessibility is a contract**, checked in its own suite, not a polish
  phase.
- **No dual implementation of presentation rules**: anything both frontends
  need lives in `norte-frontend`, and the answer to "the GUI needs this
  slightly different" is to change the shared rule, not to fork it.

## Consequences

### Positive

- A remote frontend stops depending on the engine, the providers and the
  plugin host to send a JSON-RPC request.
- `norte-core/src/backend.rs` loses ~2.400 lines of remote client and stops
  being the place where two unrelated things live.
- A second renderer becomes an adapter, and a headless parity harness can
  drive the same `UiAction`/`UiUpdate` values the renderer does.

### Negative

- Two crates and a serialisation boundary that did not exist before, with the
  latency and versioning discipline that implies.
- A handful of value types need an SDK form and an exhaustive mapping in
  `norte-core`; a variant added on one side and forgotten on the other is a
  compile error by design, which is the cost being bought.
- The bridge is one more public surface to keep compatible, with its own
  version and its own golden tests.
