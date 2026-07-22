# 0033 - FTP provider as an embedded WASM plugin (retire `norte-vfs-ftp`)

- Status: accepted
- Date: 2026-07-23
- Decision makers: Oscar González
- Related: ADR 0032 (plugin provider interface), ADR 0022 (plugin host), ADR
  0014 (native FTP provider, now retired), ADR 0015 (FTP/FTPS connection);
  issue #30

## Context

ADR 0032 designed the WIT `provider` interface and staged the migration of
`norte-vfs-ftp` to a sandboxed WASM plugin. Stages 1–3b landed: the read/write
projection, the `net` capability gated to `wasi:sockets` outbound-TCP, and a
PROOF guest (`ftp-probe`) that connected a real `suppaftp` client compiled to
`wasm32-wasip2` against an in-process `libunftp` server. What remained was the
full port: every `Provider` op, the CR/LF anti-injection defenses of the native
provider, host wiring, passing `provider_contract!`, and deleting the native
crate.

Three questions had to be settled to finish:

1. **How does a self-connecting guest learn where to connect?** The host cannot
   inject a live `suppaftp` stream into wasm — the guest must dial the socket
   itself over `wasi:sockets`. It therefore needs the endpoint and credentials.
2. **How does `ftp://` reach the guest in production once `norte-vfs-ftp` is
   gone?** The guest is a `wasm32-wasip2` artifact; that target may be absent on
   a build host, so the guest cannot be an ordinary build-time dependency of
   `norte-core`.
3. **DNS.** The guest has no DNS (`allow_ip_name_lookup(false)`, ADR 0032 stage
   3a) and its `net` allow-list is by IP. Something must resolve hostnames.

## Decision

### WIT `configure` (package `norte:plugin@0.4.0`)

Add `configure(provider-config) -> result<_, vfs-error>` to the `provider`
interface. `provider-config` carries `endpoint` (already resolved `ip:port`),
`user`, `password`, and `base` (the remote root). The host calls it exactly once
after instantiation, before using the provider. Providers that hold no
connection (the in-memory guests) implement it as a no-op. Adding an export to a
pre-release interface is a breaking change for guests, encoded as the `0.3.0 →
0.4.0` minor bump (no published guests exist).

Credentials cross into guest sandbox memory. This is inherent: the guest
performs the `login`. Over plaintext FTP the password is already exposed on the
wire; FTPS is deferred (below), so there is no cheaper option than the guest
authenticating itself.

### Host wiring: resolve, screen, grant, configure

`norte_core::ftp_plugin::connect_ftp_plugin` does, host-side:

1. **Resolve** the hostname to an IP (the guest has no DNS).
2. **Screen** the resolved IP against an anti-SSRF deny-list: link-local
   (`169.254.0.0/16`, `fe80::/10` — this covers the `169.254.169.254` cloud
   metadata endpoint), loopback (unless the destination was explicitly loopback,
   for tests and local tunnels), unspecified, and multicast/broadcast. This
   closes the gap ADR 0032 stage 3a deferred ("deny-list of link-local/metadata
   is the wiring's job"): a hostname that resolves to a metadata IP is rejected
   before any capability is granted.
3. **Grant** `net` scoped to the single resolved IP (bare-IP = all ports, which
   passive FTP's dynamic data ports require, per ADR 0032 stage 3a).
4. **Instantiate + configure** the guest with the resolved `ip:port`.

### Shipping: embedded artifact + `Component::from_binary`

`norte-core` embeds a **prebuilt `ftp-provider.wasm`** via `include_bytes!` and
loads it with `wasmtime::component::Component::from_binary` (no temp file, no
`wasm32-wasip2` toolchain required on the build host). The artifact lives at
`crates/norte-core/resources/ftp-provider.wasm` and is rebuilt with
`just build-ftp-wasm` whenever the guest or the WIT changes. The embedded path
skips the `MAX_ARTIFACT_BYTES` guard (the artifact is first-party and trusted,
unlike a third-party plugin loaded from disk).

## Consequences

- **`norte-vfs-ftp` is deleted.** `connect.rs` routes `ftp://` through
  `connect_ftp_plugin`. The native provider, its crate, and its dependency are
  gone; the shared `provider_contract!` now runs against the plugin over
  in-process `libunftp` (`crates/norte-core/tests/ftp_provider_contract.rs`).
- **A binary blob is committed.** ~1.5 MiB of `.wasm` is version-controlled.
  This is unusual but pragmatic: it decouples `norte-core`'s build from the wasm
  toolchain. The blob is reproducible via the `just` recipe; reviewers diff the
  source guest, not the blob.
- **FTPS is deferred (debt).** `aws-lc-rs` (and the other TLS backends) do not
  compile to `wasm32-wasip2`, so the guest is plaintext-only. `connect.rs`
  surfaces a `FtpPlaintext` connection warning. Host-side FTPS (terminating TLS
  in the host and handing the guest a plain byte stream) is a future option; the
  `FtpConnector` in `norte-connect` is retained, unused, for that path.
- **Per-chunk RETR (debt).** The WIT `read(segments, offset, len)` is bounded
  and cannot hold a live data connection across calls (no read-stream resource
  in the interface). Each `read` performs one REST+RETR cycle, draining to EOF —
  O(n²) transfer for a large file read in chunks. Acceptable for the contract
  (tiny files); a streaming read resource is future work.
- **Single connection.** The guest holds one control connection (single-threaded
  wasm), so the native provider's dedicated read connection for same-host FTP→FTP
  copies (issue #39 B1) is not reproduced. Same-host FTP→FTP copy is not a
  contract requirement; filed as debt.
