# 0013 - SFTP provider and hostile-server containment

- Status: accepted
- Date: 2026-07-12
- Decision makers: Oscar González
- Related: specification sections 3, 4, 12, 14, and 15; ADRs 0005 and 0012

## Context

SFTP is norte's first remote provider. The implementation must choose an async
library, separate provider logic from authentication, contain hostile names and
symlinks, and run its conformance suite without requiring Docker for every pull
request. A library limitation also matters: SFTP carries bytes but
`russh-sftp` exposes path values as UTF-8 `String`.

## Decision

- Use pure-Rust async `russh` and `russh-sftp`. A blocking libssh2 binding was
  rejected because it would require per-operation blocking bridges and provide
  weaker cancellation.
- Put the provider in permissively licensed `norte-vfs-sftp` with unsafe code
  forbidden. Its public boundary exposes only `Provider`, never Russh types.
- Construct `SftpProvider` from an already-established shared `SftpSession` and
  remote base. Authentication, keyrings, and host-key verification belong to
  the connection layer from ADR 0015.
- Run the full provider contract against an in-process `russh-sftp` server over
  `tokio::io::duplex`, backed by a temporary directory. Keep an OpenSSH
  testcontainers check in nightly CI under an opt-in feature.
- Treat the dependency as UTF-8-only and fail loudly in both directions.
  Reject non-UTF-8 outgoing segments. Because `russh-sftp` has already decoded
  incoming invalid bytes with U+FFFD, reject any such name or link target rather
  than returning corrupted bytes. Raw-name support is tracked by #37.
- Advertise `SYMLINKS`, `APPEND`, `RANDOM_WRITE`, `CASE_PRESERVING`, and the
  conservative POSIX assumption `CASE_SENSITIVE`. Do not advertise atomic
  rename, trash before ADR 0019, server copy, or a stable node identity.

## Hostile-server rules

- Build every requested remote path from validated `VPath` segments under the
  configured base. Never reuse an absolute path echoed by the server.
- Reject list entries containing `/`, literal `.` or `..`, or replacement
  characters. Do not continue a listing after structural corruption.
- Use `lstat` for metadata and reads. Expose symlinks as symlinks, return raw
  link-target data, and reject direct file reads through a symlink. With no
  stable node ID, directory-link following remains unsupported, preventing
  traversal outside the base.
- Create ordinary staging files with exclusive-create so a pre-planted symlink
  cannot be followed. Resumable staging must reopen an existing file, so lstat
  and reject a symlink before opening. The remaining check/open race is
  documented and tested against real OpenSSH.
- Enforce the four-stream limit in the scheduler, not inside a multiplexed
  `SftpSession`.

## Consequences

Remote conformance and hostile-name tests run in normal CI without Docker, and
the provider remains independent from connection secrets. The main limitation
is that remote non-UTF-8 names are inoperable until the dependency exposes raw
protocol bytes. macOS normalization and remote case behaviour remain engine
concerns; late server conflicts are safe even when the conservative capability
guess is imperfect.

Russh and its cryptography stack are large. RSA client keys are rejected by the
connection layer because of RUSTSEC-2023-0071; production authentication uses
Ed25519 and verified host keys. A server's rename and append semantics remain
provider-dependent and are not advertised as atomic.
