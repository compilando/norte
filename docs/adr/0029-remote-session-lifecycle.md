# Remote session lifecycle: lazy eviction, single-flight, canonical dedup

- Status: accepted
- Date: 2026-07-22
- Issues: #47, #62

## Context and problem statement

The Engine's remote-provider cache (phase 6e, ADR 0015) was deliberately
minimal: a dead session (server restart, network loss) stayed cached forever
returning `ProviderUnavailable`; two concurrent requests to an uncached host
opened two authenticated sessions; `sftp://host` and `sftp://user@host`
resolving to the same identity opened two sessions; and the connect happened
before any cancelable Task existed, bounded only by a fixed 30 s timeout.
Evicting a session also has to drag the composed archive providers
(`{fmt}+scheme://authority`) that cache the session's `Arc` (#62).

## Decision

A `SessionPool` in `norte-core/src/sessions.rs` owns the provider cache and
the whole lifecycle:

1. **Lazy lifecycle — no background health-checks, no idle TTL.** Every
   cached remote session is wrapped in a `SessionProvider`; when any provider
   call returns `ProviderUnavailable`, the wrapper evicts its own cache entry
   (pointer-checked so a newer session under the same key is never clobbered)
   and the error propagates unchanged. The next access reconnects. A file
   manager touches sessions on user action; background probing adds traffic
   and keeps credentials hot for no UX gain. Errors surfaced *inside* already
   returned streams/sinks do not evict (v1); the next direct call does.
2. **Single-flight.** One dial per cache key: concurrent requests subscribe
   to the in-flight job (`tokio::watch`). The dial runs in a spawned job
   bounded by `CONNECT_TIMEOUT`.
3. **Drop-based cancelable connect.** Waiters hold an RAII guard; when the
   last waiter drops mid-dial, the job's `CancellationToken` cancels the
   connect future and the key is cleaned atomically. This composes with
   `rpc.cancel` (#72): dropping the daemon dispatch drops the waiter. No
   Task wrapping needed (regla 3 satisfied by bounded + cancelable).
4. **Backoff negative-cache.** A dial failing with `ProviderUnavailable`
   (including timeout) enters a cooldown (1 s initial, ×2, capped at 30 s);
   retries inside the window get the cached error without dialing. User-
   actionable errors (TOFU `HostKeyUnknown`/`HostKeyMismatch`, auth, invalid
   path) are never cached — fix-and-retry stays instant. Success clears the
   key's cooldown.
5. **Canonical dedup.** `RemoteConnector::canonical_authority` (defaulted to
   `None`) resolves the effective identity locally (connections.toml user
   inheritance, default-port stripping) *before* dialing. The session is
   cached under the canonical key plus the requested alias; two spellings of
   one identity share one session.
6. **Composite drag (#62).** Evicting session keys also removes every cache
   key with suffix `+{session-key}` — the composed archive providers over
   that session. They recompose lazily over the fresh session.

## Consequences

- A dead session costs exactly one failed user operation before healing.
- No wire change: everything is engine-internal; `Connected` is unchanged.
- The dial job may outlive an abandoned waiter and still cache a completed
  session (handshake not wasted; no side effects beyond a live session).
- The suffix rule for composite drag relies on composite schemes being
  `{fmt}+{scheme}` from the proto whitelist; authorities cannot contain
  `://`, so false positives are impossible.
- Sessions still never expire while healthy; if idle-server resource use
  ever matters, an idle TTL can be added inside `SessionPool` without
  touching callers.
