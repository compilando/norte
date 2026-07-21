# 0014 - FTP provider, MLSD, and cleartext risk

- Status: accepted
- Date: 2026-07-12
- Decision makers: Oscar González
- Related: ADRs 0005, 0012, and 0013

## Context

FTP has separate control and data connections, stateful control commands,
ambiguous legacy `LIST` output, optional UTF-8, an ASCII default that corrupts
binary data, and no confidentiality without FTPS. The provider needs the same
connection injection and ordinary-CI coverage as SFTP.

## Decision

- Use maintained async `suppaftp` 10 with Tokio. It provides MLSD/MLST,
  REST/APPE resume, and construction from an established stream.
- Put the implementation in permissively licensed `norte-vfs-ftp` with unsafe
  code forbidden. Wrap one authenticated `AsyncFtpStream` in an async mutex;
  an FTP control connection is inherently stateful and serialized.
- Force binary `TYPE I` during construction. Never allow the protocol's ASCII
  default to translate line endings.
- Detect MLSD through `FEAT`. Use machine-readable MLST/MLSD when supported and
  fall back to parent-directory `LIST` parsing for compatibility. The fallback
  is suitable for ordinary names but remains less robust and is tracked by #40.
- Enable `OPTS UTF8 ON` when advertised. Reject outgoing non-UTF-8 names and
  incoming names already decoded with U+FFFD. Raw-byte support shares issue #37
  with SFTP.
- Advertise `APPEND`, `CASE_PRESERVING`, and the conservative POSIX assumption
  `CASE_SENSITIVE`. Do not advertise symlinks, random write, atomic rename,
  trash before ADR 0019, server copy, or stable node IDs.
- Resume with `SIZE` plus REST/APPE and implement ranged reads with REST before
  RETR.
- Build every path from validated segments beneath the configured base. Reject
  `/`, `.`, `..`, U+FFFD, NUL, CR, and LF. CR/LF rejection is essential because
  names are inserted into a line protocol and could otherwise inject commands.
- Keep production authentication in the connection layer. FTPS with explicit
  AUTH TLS is the recommended/default path. Plain FTP requires explicit opt-in
  and a visible warning; opportunistic mode may fall back with a warning.
- Test normal conformance with an in-process `libunftp` server on an ephemeral
  loopback port. Run a different real FTP server through testcontainers in
  nightly CI to exercise the LIST fallback and resume.

## Consequences

FTP reuses the provider contract and in-process server approach, and MLSD gives
reliable metadata on capable servers. A single control connection serializes
operations and a read owns it for the stream lifetime. A future connection pool
must handle same-server FTP copies and cancellation that leaves control state
unsynchronized (#39).

Non-UTF-8 names remain unsupported, legacy LIST parsing is heuristic, and plain
FTP exposes credentials and content. FTP is therefore the first candidate for
later migration to a WASM provider. TLS dependencies stay feature-scoped, and
the configured base is validated before future configuration can supply it.
