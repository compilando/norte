# Remote Session Lifecycle (#47) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the Engine's remote-provider cache a real lifecycle: single-flight connects, drop-based cancelable dialing, failure backoff, evict-on-dead-session with lazy reconnection, canonical-authority dedup, and archive-composite drag on eviction (#62).

**Architecture:** New `norte-core/src/sessions.rs` module owning a `SessionPool` (Arc-backed) that absorbs the Engine's `providers` map and adds `connecting` (single-flight jobs) and `cooldown` (backoff negative-cache) maps. Remote providers get wrapped in a `SessionProvider` that self-evicts from the pool when an operation returns `ProviderUnavailable`; eviction sweeps aliases (same Arc) and composite archive keys (`{fmt}+{session-key}` suffix rule). Dialing runs in a spawned job with a `CancellationToken`; waiters hold an RAII guard — when the last waiter drops, the dial is cancelled (this composes with `rpc.cancel` #72 for user-facing cancel; regla 3 satisfied drop-based, no Task needed). Dedup: new defaulted trait method `RemoteConnector::canonical_authority` resolved BEFORE dialing (pure local `connections.toml` read), so an alias hit never dials.

**Tech Stack:** tokio (`watch`, `spawn`, `time`), tokio-util `CancellationToken`, existing `Error: Clone`. No new deps, no proto change.

**Key design decisions (record in ADR 0029):**
- No background health-check, no idle TTL: lifecycle is lazy — evict on `ProviderUnavailable` from an op, reconnect on next access. A file manager touches sessions on user action; background probes add traffic and secrets-surface for no UX gain.
- Backoff negative-cache ONLY for `Error::ProviderUnavailable` (incl. dial timeout). TOFU (`HostKeyUnknown`/`Mismatch`), auth and policy errors never enter cooldown — the user fixes and retries immediately (existing test `tofu_error_trust_y_reintento` stays green).
- Backoff: 1s initial, ×2, cap 30s; any successful connect clears the key's cooldown.
- Cancel semantics: dial completes even if all waiters left *after* the connect future resolved (session gets cached — handshake not wasted); if waiters leave mid-dial, the job's token cancels the connect future.
- Eviction is ptr-checked: a `SessionProvider` only evicts the map entry that IS itself (a newer session under the same key is never clobbered).

---

## File structure

- Create: `crates/norte-core/src/sessions.rs` — `SessionPool`, `PoolInner`, `ConnectJob`, `Cooldown`, `WaiterGuard`, `SessionProvider` wrapper.
- Modify: `crates/norte-core/src/engine.rs` — drop `providers` field (use `sessions: SessionPool`), rewrite remote branch of `provider_for`, move `CONNECT_TIMEOUT` out.
- Modify: `crates/norte-core/src/connect.rs` — `RemoteConnector::canonical_authority` (defaulted), `canonical_authority_of` helper (default-port-stripping), `ConnectionManager` impl.
- Modify: `crates/norte-core/src/lib.rs` — `mod sessions;` (private).
- Test: `crates/norte-core/tests/connect.rs` — new lifecycle tests.
- Create: `docs/adr/0029-remote-session-lifecycle.md`.

---

### Task 1: `SessionPool` skeleton (pure refactor, tests stay green)

**Files:** Create `crates/norte-core/src/sessions.rs`; modify `engine.rs`, `lib.rs`.

- [ ] **Step 1.1**: Create `sessions.rs` with `SessionPool` wrapping `Arc<PoolInner>`; `PoolInner { providers: RwLock<HashMap<String, Arc<dyn Provider>>>, connecting: Mutex<HashMap<String, ConnectJob>>, cooldown: Mutex<HashMap<String, Cooldown>> }`. Methods: `new()`, `register_process(provider)` (key = scheme), `lookup(key) -> Option<Arc<dyn Provider>>`, `insert_composite(key, provider) -> Arc<dyn Provider>` (entry/or_insert_with double-check, returns the winning Arc).
- [ ] **Step 1.2**: Engine: replace `providers` field with `sessions: SessionPool`; `register_provider` delegates; `provider_for` fast path uses `lookup(p.scheme())` then `lookup(&key)`; composite branch uses `insert_composite`; remote branch temporarily keeps old inline logic but inserts via a `SessionPool::insert_composite`-style method (`insert_remote_raw` placeholder to be replaced in Task 2).
- [ ] **Step 1.3**: `cargo nextest run -p norte-core` → all green (refactor only).
- [ ] **Step 1.4**: Commit `refactor(core): extrae SessionPool a sessions.rs (#47, sin cambio de conducta)`.

### Task 2: Single-flight + drop-cancelable dial

**Files:** `sessions.rs`, `engine.rs`, test `tests/connect.rs`.

- [ ] **Step 2.1**: Write failing test `connect_concurrente_es_single_flight`: FakeConnector variant `GatedConnector` (holds `tokio::sync::Notify` + AtomicUsize connects; `connect()` bumps counter, waits for notify). Two concurrent `engine.stat` to same host; `notify_waiters` after both pending; both succeed; assert `connects == 1`.
- [ ] **Step 2.2**: Write failing test `abandono_de_todos_los_waiters_cancela_el_dial`: connector whose `connect` future sets an `Arc<AtomicBool>` on Drop (guard struct) and pends forever. Spawn `engine.stat` in a task, wait until dial started, abort the task; yield; assert drop-flag true and `connecting` cleaned (a following stat dials again: started-count == 2).
- [ ] **Step 2.3**: Implement `SessionPool::connect_remote(cache_key, alias, scheme, authority, connector, observer)` per design: cooldown check (stub for now — always pass), subscribe-or-spawn under `connecting` lock with per-job unique id + `WaiterGuard` (drop: waiters-=1; at 0 → `cancel()` + remove entry, atomically under the lock); spawned job: `select!{ cancelled => abandon, timeout(CONNECT_TIMEOUT, connect) => result }`; success: wrap in `SessionProvider` (Task 4 — for now insert `connected.provider` raw), insert cache_key (+alias), emit #44 warnings via observer, `remove_if(key, id)`, `tx.send`; failure: `remove_if` + send. Waiter loop: `borrow_and_update` / `changed()`, map closed channel to `Error::Internal { panic: true }`.
- [ ] **Step 2.4**: Engine remote branch delegates to `connect_remote` (observer passed as `Option<Arc<dyn ConnectionObserver>>`, cloned from the field). Delete the old inline connect + double-check block and `CONNECT_TIMEOUT` (moves to sessions.rs).
- [ ] **Step 2.5**: `cargo nextest run -p norte-core --no-fail-fast` → green incl. old `connect_colgado_expira_con_timeout` (start_paused), `observer_recibe_el_aviso_de_degradacion`.
- [ ] **Step 2.6**: Commit `feat(core): #47 single-flight y dial cancelable por drop en SessionPool`.

### Task 3: Cooldown / backoff negative-cache

**Files:** `sessions.rs`, test `tests/connect.rs`.

- [ ] **Step 3.1**: Failing test `fallo_transitorio_entra_en_cooldown_con_backoff` (`start_paused`): connector always `Err(ProviderUnavailable{retryable:true})`. stat → Err, connects==1; immediate stat → same Err, connects==1 (cached); advance 1s; stat → connects==2; immediate stat → cached (2); advance 1s (window now 2s, not expired) → still 2; advance 1s more → stat → 3.
- [ ] **Step 3.2**: Failing test `exito_limpia_el_cooldown`: connector fails once then succeeds. stat→Err (1); advance 1s; stat→Ok (2); provider dies? no — just assert a third stat is cache-hit (2) and no cooldown residue (evict manually not needed).
- [ ] **Step 3.3**: Implement `Cooldown { until: tokio::time::Instant, next: Duration, last_err: Error }`; check at top of `connect_remote` (return `last_err.clone()` if `now < until`); escalate in job failure path only for `ProviderUnavailable`; clear on success and on non-eligible errors. Constants `BACKOFF_INITIAL=1s`, `BACKOFF_MAX=30s`.
- [ ] **Step 3.4**: Run suite (incl. `tofu_error_trust_y_reintento` — must stay green because HostKeyUnknown is not eligible). Commit `feat(core): #47 backoff de reconexión (negative-cache solo ProviderUnavailable)`.

### Task 4: `SessionProvider` wrapper — evict on dead session, lazy reconnect

**Files:** `sessions.rs`, test `tests/connect.rs`.

- [ ] **Step 4.1**: Failing test `sesion_muerta_se_evicta_y_reconecta`: test provider `FlipProvider` (delegates to MemProvider, `poisoned: AtomicBool` → all ops return `ProviderUnavailable{retryable:true}` when set); connector returns a fresh FlipProvider per connect and keeps handles. stat ok (1 connect); poison; stat → Err(PU); stat again → dials again (2), Ok.
- [ ] **Step 4.2**: Implement `SessionProvider { inner: Arc<dyn Provider>, key: String, pool: Weak<PoolInner> }` implementing ALL `Provider` methods by delegation (scheme, capabilities, stat, list, list_skipped, read, node_id, read_link, trash, gc_partials, restore_trashed, symlink, write, open_resumable, partial_digest, mkdir, remove, rename, copy_native) with `observe(result)` on every `Result` (and inside `copy_native`'s `Option<Result>`): on `Err(ProviderUnavailable{..})` → `pool.evict_session(&self.key, self-ptr)`. Document (rustdoc): errors inside returned streams/sinks do NOT evict (v1).
- [ ] **Step 4.3**: `PoolInner::evict_session(key, wrapper_ptr)`: under `providers` write lock — ptr-check current entry; collect all keys with same Arc ptr (aliases); remove them; sweep composite keys `ck.ends_with("+{k}")` for each removed key (#62); `tracing::warn!` with count.
- [ ] **Step 4.4**: Wire: job success path wraps provider before inserting. Run suite → green. Commit `feat(core): #47 evicción de sesión muerta y reconexión perezosa (SessionProvider)`.

### Task 5: Canonical dedup

**Files:** `connect.rs`, `engine.rs`, tests (unit in connect.rs + integration in tests/connect.rs).

- [ ] **Step 5.1**: Failing integration test `dedup_canonica_no_abre_segunda_sesion`: FakeConnector override `canonical_authority` → `Some("oscar@h".into())` for authority `"h"` or `"oscar@h"`. stat `sftp://h/x` (dial 1); stat `sftp://oscar@h/x` → connects==1. Second scenario (reverse order) same assert.
- [ ] **Step 5.2**: Add to `RemoteConnector`: `async fn canonical_authority(&self, _scheme: &str, _authority: &str) -> Option<String> { None }`.
- [ ] **Step 5.3**: `connect.rs`: helper `canonical_authority_of(ep) -> String` = `authority_of` but omitting the port when it equals the scheme default (reuses `effective_port` logic); unit tests: `sftp://h:22`→`h`, `sftp://oscar@h:2222`→`oscar@h:2222`, `s3://b`→`b`. `ConnectionManager::canonical_authority`: `load_connections` + `resolve_spec(scheme://authority)` + `canonical_authority_of(resolved endpoint)`; any error → `None` (connect will surface it). Unit test with the existing `CONNS` fixture via a pure helper `canonical_from_file(&ConnectionsFile, url)` so no config dir needed: `sftp://work.example:2222` → `oscar@work.example:2222` (inherits user).
- [ ] **Step 5.4**: Engine `provider_for` remote branch: resolve canonical before dialing; on canonical cache hit → `sessions.alias(requested_key, &arc)` + return; else dial under canonical key with `alias = Some(requested_key)` when different. Run suite. Commit `feat(core): #47 dedup canonica de sesiones via canonical_authority`.

### Task 6: #62 drag test (composite archive eviction)

**Files:** test `tests/connect.rs` (impl already in Task 4 sweep).

- [ ] **Step 6.1**: Failing-first test `evictar_sesion_arrastra_archive_compuestos`: connector returns FlipProvider over MemProvider containing `/a.zip` built with `norte_testkit::ZipSmith` (member `uno.txt` on dial 1, member `dos.txt` on dial 2). `engine.list(zip+sftp://h/a.zip!/)` → see `uno.txt` (composite cached). Poison session; `engine.stat(sftp://h/x)` → PU (evicts). `engine.list(zip+sftp://h/a.zip!/)` again → sees `dos.txt` (composite was dragged; a stale composite would still serve `uno.txt` from the old session/index). Assert connects==2.
- [ ] **Step 6.2**: Green (sweep exists). If red, fix sweep. Commit `test(core): #47/#62 evicción arrastra providers archive compuestos`.

### Task 7: ADR + docs + gate + reviewers

- [ ] **Step 7.1**: Write `docs/adr/0029-remote-session-lifecycle.md` (English, MADR): context (#47, fase 6e debt), decisions listed above, consequences (no health-check; streams don't evict; #62 rule).
- [ ] **Step 7.2**: Rustdoc pass on `sessions.rs` (module docs = lifecycle contract) + update stale comments in `engine.rs`/`connect.rs` that referenced "#47 pendiente".
- [ ] **Step 7.3**: `just ci` → EXIT 0.
- [ ] **Step 7.4**: Commit, then reviewers: rust-reviewer + security-reviewer (session lifecycle = auth surface). Apply findings, re-run `just ci`, commit fixes.
- [ ] **Step 7.5**: Comment/close #47 with design summary; note in #62 that eviction drag is done.
