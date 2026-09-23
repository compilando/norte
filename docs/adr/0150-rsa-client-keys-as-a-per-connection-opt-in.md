# 0150 — RSA client keys as a per-connection opt-in

- Status: accepted
- Date: 2026-09-23
- Decision makers: Oscar González
- Amends: ADR 0015, "SSH trust and client keys" (the RSA rejection)
- Protocol: unchanged. Bridge: unchanged.
- Related: issue #36, RUSTSEC-2023-0071, `deny.toml`

## Context and problem statement

ADR 0015 rejects every RSA client key so that norte never signs through the
`rsa` crate, whose private-key operations carry the Marvin timing side
channel (RUSTSEC-2023-0071, no upstream fix). That kept `deny.toml`'s
exception honest: the vulnerable code is linked, but never reached.

Some servers norte has to reach hand out an RSA key and nothing else. AWS
Transfer Family issues per-user keys from whoever administers the server, and
the reader cannot always get an ed25519 one registered. For them norte has no
answer today but "use another client", which moves the same RSA signature to
a different program instead of removing it.

## Options considered

1. **Keep rejecting RSA.** Nothing changes, and those servers stay out of
   reach.
2. **Lift the rejection.** Any RSA key works, and the risk ADR 0015 closed is
   open again for everyone, including people who never needed it.
3. **An explicit opt-in per connection** (`allow_rsa = true` in
   `connections.toml`). The default stays exactly as ADR 0015 left it; the
   risk exists only where someone wrote it down, and only for that entry.
4. **RSA through the SSH agent**, so that OpenSSH's agent signs and norte's
   `rsa` crate does not. That avoids Marvin in norte, but it makes the agent
   a requirement for these servers and widens `auth = "agent"`'s rules. It
   can follow later without undoing this.

## Decision

**Option 3.**

1. `ConnectionSpec.allow_rsa: bool`, off by default. It changes nothing
   except loading a client key file under `auth = "key"` on `sftp://`.
   Without it an RSA key is still `KeyUnsupported`, and the message still
   recommends ed25519.
2. **rsa-sha2 only, never SHA-1.** The hash comes from the server's
   `server-sig-algs` (RFC 8308): rsa-sha2-512, else rsa-sha2-256. A server
   that offers only `ssh-rsa` is refused with its own error, `RsaSha1Only`.
   A server that does not send the extension gets rsa-sha2-256, which every
   server from the last decade accepts.
3. **It says so every time.** Each RSA authentication logs a `warn!` naming
   the host and this ADR. With a daemon it lands in the daemon log; with the
   embedded core (`norte connect` without `--daemon`) it lands on stderr. A
   log filter above `warn` hides it, and that is why `doctor` repeats it:
   `norte doctor` reports `conn-rsa-allowed` (Warn) for as
   long as the key is set, and `conn-rsa-allowed-inert` where it can do
   nothing (another scheme or auth method), the same way #325 treats an
   inert `secret = "prompt"`.
4. **No wire change.** It does not become a `connection.degraded` reason:
   today that notification means the session is unencrypted, which an RSA
   session is not. Bringing it to the TUI and window banners would redefine
   that notification for every frontend and needs its own decision.
5. The agent keeps its ed25519-only rule (option 4 is not taken here).

## Consequences

- Servers that only accept RSA keys are reachable, for the readers who ask
  for it by name.
- On those connections norte signs through RUSTSEC-2023-0071's code path.
  The exposure is a client authenticating a handful of times per session,
  not a server answering an attacker's queries, and it is limited to the
  entries that carry the key. `deny.toml`'s exception now says that too.
- The warning lives in the log and in `doctor`, not on screen. Someone
  using only the TUI or the window sees it only if they run `doctor`.
- Two more `doctor` codes and one more `ConnectError` variant, which maps
  to `PermissionDenied` like `KeyUnsupported`.
- `connections.toml` denies unknown fields, so a norte older than this one
  refuses the WHOLE file once any entry carries `allow_rsa`, not only that
  entry. It fails closed, as `secret = "prompt"` (#325) already does; it
  costs availability on a downgrade, never security.
- If a server sends `server-sig-algs` without any RSA algorithm, norte still
  tries rsa-sha2-256 and the server's refusal surfaces as a plain
  `AuthFailed`, without naming the reason.
