# 0159 — The Windows daemon listens on an owner-only named pipe, and its unsafe lives in `norte-winpipe`

- Status: accepted
- Date: 2026-09-26
- Decision makers: Oscar González
- Related: ADR 0011 (daemon over UDS), ADR 0066 (daemon-only GUI, client
  SDK), ADR 0157 (Windows builder milestones), spec §17.6

## Context

On unix the daemon listens on a socket inside a 0700 directory, and both
ends check the peer's uid with `SO_PEERCRED`. ADR 0157 requires the same
boundary on Windows before the GUI ships there.

A named pipe gives neither for free. Its default DACL lets Everyone read, so
any user could receive the approval broadcasts. Tokio exposes no peer
identity. Setting a security descriptor and reading a token are Win32 calls
that need `unsafe`, and both `norte-client` and `norte-core` are
`forbid(unsafe_code)`.

## Decision

1. **A new crate, `norte-winpipe`, holds the only `unsafe`** (deny + allow
   per item + `SAFETY`, as in `norte-vfs-local`). It is empty off Windows and
   exposes `create_server`, `client_user`, `server_is_ours` and
   `current_user`. The SDK depends on it only on Windows; its boundary test
   still forbids the core and the providers. Hard rule 5 gains this second
   exception.
2. **The pipe's security descriptor is explicit**:
   `O:<user>D:P(A;;GA;;;<user>)S:(ML;;NWNR;;;<own integrity>)`. The owner is
   the user, the protected DACL has one entry, remote clients are refused,
   and the label is the daemon's own integrity. So an elevated daemon cannot
   be driven from medium integrity, and a low-integrity process cannot even
   read.
3. **Both ends check the other.**
   - The daemon admits a client whose token user equals its own. The DACL is
     the gate; this is the second check, and a PID resolved late can only
     fail it.
   - The client reads the pipe OBJECT, not a PID: the owner must be itself,
     which another user cannot set, and the label at least medium, which a
     sandboxed process of the same user cannot give. Otherwise
     `ForeignDaemon`.
   - Every client open uses `SECURITY_IDENTIFICATION`, the presence probe
     included, so whoever holds the name learns who connects but cannot act
     as them.
4. **Addresses stay path-shaped.** The default is
   `%LOCALAPPDATA%\norte\daemon.pipe`. `norte_client::pipe_name` maps an
   address to `\\.\pipe\norte-<FNV-1a 64>`, or passes a single plain
   `\\.\pipe\<name>` through. Anything Win32 would normalise elsewhere (`..`,
   a second separator) is hashed instead.
5. **The first instance claims the name** (`FILE_FLAG_FIRST_PIPE_INSTANCE`).
   The listener always holds the next instance, so a client never finds the
   name missing between accepts. Service accounts (SYSTEM, LOCAL SERVICE,
   NETWORK SERVICE) are refused as root is on unix.

## Consequences

- `norte-client`, `norte-core`, `norte-cli` and `norte-tui` compile for
  Windows. `norte --daemon ls` spawns a daemon over the pipe, lists and stops
  on the ADR 0157 VM.
- A pipe name squatted by another user stops the daemon from starting. It
  reports `AlreadyRunning`, and clients refuse the squatter. That is a denial
  of service, not an impersonation. A random per-user suffix would remove it;
  not done.
- An elevated daemon is served only to elevated clients of the same user. A
  medium client finds no daemon it may open and cannot start a second one.
- The TUI's persistent subshell stays unix-only (ADR 0084); the terminal
  pane's key translation no longer depends on it.
- The unix path is unchanged except for the log line of a refused peer.
