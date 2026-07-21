# 0011 - JSON-RPC framing, transport, and daemon lifecycle

- Status: accepted
- Date: 2026-07-12
- Decision makers: Oscar González
- Related: specification sections 11, 17.6, and 17.7; ADR 0004; issue #31

## Context

M1 defined embedded `fs.*` and `task.*` parameters and results but no envelope,
transport, or daemon. M2 must stabilize JSON-RPC messages, stream framing, error
codes, initialization, authentication, and lifecycle behaviour.

## Options considered

- LSP-style `Content-Length` framing can carry future binary encodings but needs
  a stateful header parser. NDJSON is inspectable, easy to fuzz, and safe because
  JSON encoders escape embedded newlines, but a future binary encoding will need
  separate framing.
- One JSON-RPC code per application error duplicates the error taxonomy. One
  application code with the typed error in `data` preserves a single contract.
- Implementing Windows named-pipe peer authentication immediately requires
  carefully isolated Win32 calls and safe descriptors. Deferring it leaves
  embedded mode available while Unix-domain transport is completed securely.

## Decision

- Expose standard JSON-RPC `Request`, `Response`, and `Notification` types from
  `norte-proto::wire`. Canonical request IDs are `u64`; readers accept numbers
  or strings. Classify messages structurally by `method` and `id`.
- Use NDJSON with a 16 MiB frame limit. Oversized frames close the connection.
  Initialization currently negotiates `json`; a later binary encoding will
  negotiate its own framing.
- Use standard JSON-RPC protocol codes and `-32000` for application failures,
  carrying the complete typed norte error in `error.data`. Messages are for
  people and debugging and must never be parsed.
- Require `initialize` before all other methods. It exchanges client/server
  identity, protocol version, and encodings. During 0.x, accept the current and
  previous minor version; reject other versions with `-32001` and close.
  Repeated initialization is an invalid request.
- Add `Error::Loop` for symlink cycles and bump the protocol from 0.3.0 to
  0.4.0. Version 0.3.0 is the accepted N-1.
- On Unix, listen at `$XDG_RUNTIME_DIR/norte/daemon.sock`. The fallback is a
  verified, non-symlink, mode-0700 `/tmp/norte-<uid>` directory. Reject root and
  authenticate the peer UID before reading data. Clients also verify the
  daemon's peer credentials.
- Defer Windows named pipes until their security boundary has a dedicated
  implementation decision. Windows continues to support embedded mode.
- Run the daemon in the foreground. Shut down only after the configured idle
  interval has no clients or live tasks. Graceful shutdown stops accepting
  clients and waits; forced shutdown cancels tasks. `connect_or_spawn` supports
  first-client startup.
- Subscribe a connection to broadcasts after initialization. Task progress is
  coalesced and sent to every authenticated client.

## Resource and protocol hardening

- Bound per-connection output queues, connection count, and live task count.
  Return `-32003` for overload and disconnect repeated parse failures.
- Distinguish malformed JSON (`-32700`) from a valid JSON value with an invalid
  envelope (`-32600`). Illegal request-ID types receive an explicit error.
- Recheck the fallback directory's device/inode after binding and return an
  actionable error for a squatted directory. Bilateral peer credentials remain
  the channel-integrity guarantee because path checks alone have TOCTOU races.
- Gate shutdown, cancellation, and other cross-client actions through actor
  policy before exposing the socket to agents.

Later protocol 0.5.0 work added `task.list`, `fs.read`, and
`fs.capabilities`. Recent task outcomes live in a bounded ring and clients
deduplicate by task ID. Reconnection resolves unknown orphan tasks as failed,
does not restart an already-used daemon, and applies call timeouts. `fs.read`
uses base64 chunks no larger than 8 MiB plus an EOF marker.

## Consequences

The daemon uses a standard, inspectable protocol and keeps one typed application
error hierarchy. Unix peer authentication is small and testable without unsafe
code. Windows daemon mode and complete upgrade choreography remain deferred.
NDJSON deliberately trades future binary-framing reuse for current simplicity,
and generic JSON-RPC tools must inspect `error.data` to distinguish application
categories.
