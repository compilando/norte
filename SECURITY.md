# Security policy

Security matters in norte because it handles untrusted filenames, remote
servers, archives, plugins, credentials, and agent-initiated filesystem
operations.

## Supported versions

norte is currently an alpha project. Security fixes are applied to the latest
release and the current `main` branch. Older alpha releases may not receive
backports.

## Report a vulnerability

Please do not disclose a suspected vulnerability in a public issue, discussion,
or pull request.

Use the repository's private **Report a vulnerability** form under the GitHub
Security tab. If private reporting is unavailable, contact the repository
maintainers privately through their GitHub profiles and include only enough
information to establish a secure reporting channel.

Include, when possible:

- The affected version or commit.
- The operating system and provider involved.
- A concise reproduction or proof of concept.
- The impact and required attacker access.
- Whether the report concerns data loss, credential exposure, policy bypass,
  sandbox escape, path traversal, or denial of service.

Do not include real credentials, private keys, personal paths, or sensitive
files. Use synthetic fixtures and redact logs.

## What happens next

Maintainers will acknowledge the report, reproduce it, assess severity, and
coordinate a fix and disclosure. Response times are best-effort while the
project is in alpha. Please allow time for a safe release before publishing
details.

## Security boundaries

The project explicitly considers:

- Hostile ZIP, TAR, SFTP, FTP, and object-storage names or metadata.
- Path traversal, symlink races, normalization collisions, and non-UTF-8 data.
- Archive bombs and unbounded resource consumption.
- Secrets in configuration, logs, process environments, and crash reports.
- Local socket authentication and cross-client authorization. On Windows the
  daemon listens on a named pipe whose owner, DACL and integrity label admit
  only its own user at its own integrity level; the client checks that owner
  and label before trusting the pipe, and opens it at identification level so
  a squatter cannot impersonate it (ADR 0159). A pipe name squatted by another
  user stops the daemon from starting; it cannot receive a client.
- Agent scope or policy bypass, including confused-deputy attacks.
- WASM capability bypass and unbounded plugin execution.
- Journal rewriting, truncation, rollback, and audit-anchor handling.

The MCP policy model governs cooperative agents. It is not a sandbox against
arbitrary hostile code already running as the same operating-system user. A
deployment must prevent an agent from accessing the daemon socket directly when
the agent should be restricted to the MCP bridge.

Raw user Lua scripts are also not sandboxed. Project-local scripts require
explicit trust, but trusted Lua has the user's operating-system permissions.
