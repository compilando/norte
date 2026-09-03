# 0093 — A provider plugin serves the scheme it declares

- Status: accepted
- Date: 2026-09-03
- Decision makers: Oscar González
- Related: ADR 0022 (manifest and capabilities), ADR 0032/0033 (the `provider`
  interface and FTP as its proof), ADR 0041 (which providers are core and which
  arrive as plugins), ADR 0018 (composed archive schemes), ADR 0088 (a declared
  capability nobody honours is a lie), hard rule 9, spec §7.1 and §17

## Context

ADR 0041 decided that new backends arrive as plugins and the four built-in
providers stay in the core. The WIT interface for a provider exists
(`norte:provider@0.1.0`), the FTP backend is a guest that implements it, the
manifest has a `[[contributions.provider]]` with a `scheme`, and the extension
manager lets a human approve and enable such a plugin.

None of that reached the connection. `ConnectionManager::establish_inner`
matched `ep.scheme` against `"sftp"`, `"ftp"` and `"s3"`, and the `ftp` arm
instantiated a `.wasm` compiled into `norte-core` with `include_bytes!`. No
code anywhere asked the catalogue "who serves this scheme". A third party who
did exactly what ADR 0041 invites — write a WebDAV provider, install it, get it
approved — shipped something inert, and would have learned it by nothing
happening. Two more closed lists sat in front of it: `norte-connect`'s endpoint
parser accepted only the three core schemes, and the CLI treated any other
`x://y` argument as a local file name.

This is the hook failure (ADR 0022 amendment of 2026-08-08) one floor up, and
the same family as `ai` (amended alongside this ADR): a surface the manifest
offers, the digest covers and the manager paints, that the host never honours.

## Decision

**A provider plugin that is approved and enabled serves the scheme its
manifest declares, and the core's schemes cannot be claimed.**

### 1. The catalogue is asked first, by scheme

`PluginRegistry::resolve_provider(scheme)` returns the first provider plugin —
category `provider`, approval current, enabled, `plugin.wasm` verified inside
its directory — whose `[[contributions.provider]]` declares `scheme`. The
connection manager calls it before its own `match`, for every scheme that is
not the core's. On a hit the guest is instantiated under the capabilities of
**its own manifest**, given its resolved `[config]` values, and `configure`d
with the connection's endpoint, user, password and base `/`. On a miss the
core's arms answer as before, and an unknown scheme is still `Unsupported`.

Fail-closed is inherited, not added: an installed plugin that is not approved,
or approved but off, resolves to nothing, and the scheme does not exist. The
answer is `Unsupported` — the same as a scheme nobody serves — and not
`PermissionDenied`, because the manager reads a denial with a session-typed
secret as "the server rejected the password" and re-prompts (#325); the reason
goes to the log, which the panel can show. The end-to-end test installs
`provider-mem` the way a third party's plugin is installed and checks both
halves.

**What runs is what was approved.** The approval anchor covers the binary
(#241), and the resolver returns the digest the catalogue computed. Before
instantiating, the manager reads `plugin.wasm`, hashes it and compares; a
mismatch is refused. The window between discovery and load is a check, not a
hope.

**What approving grants is visible.** A provider's claimed scheme is listed
among its capabilities as `provider:<scheme>`, next to `net` and the rest, so
the approval question says "this will answer `webdav://`". The field is the
open-vocabulary `capabilities` list of `PluginInfo`, which frontends already
mask and paint as text; no protocol change.

### 2. Reserved schemes

`file`, `sftp`, `ftp` and `s3` are the core's. A manifest claiming any of
them, an archive format (`zip`, `tar`, …) or any scheme containing `+` (the
composition operator of ADR 0018) is rejected at parse time with
`ManifestError::ReservedScheme`, and the registry never answers for a core
scheme even if an entry were planted past the parser. Two doors, because the
consequence of one failing is a plugin sitting in front of a backend that has
trash, resume, server-side copy and TLS, and the human approving it would not
see the difference.

`ftp` is reserved **although its backend is a guest**. A plugin claiming it
would receive, through `configure`, the password of every stored `ftp://`
connection, and would skip the plaintext warning the core arm emits. The
embedded guest stays where it is; the day it is distributed as a plugin,
`ftp` leaves the reserved list in that same commit.

### 3. Network: the host resolves, the guest connects — to one port

A guest has no DNS. If the plugin's manifest declares `net`, the host resolves
the connection's host through the same anti-SSRF filter the FTP guest already
uses (loopback only when asked for literally, no link-local, no metadata
range, IPv4-mapped IPv6 canonicalised) and adds **`ip:port`** to the
allow-list the human approved. The port is the URL's, or the contribution's
`default-port` — which enters the approval digest — and with neither the
connection is refused: the host does not know the default port of a scheme it
does not serve, and granting the whole host to third-party code because the
port was omitted is what `webdav://localhost` would otherwise mean (every
loopback service on the machine). The embedded FTP guest keeps its bare-IP
grant because passive mode negotiates data ports; a plugin that needs the
same will say so in a manifest field that does not exist yet, and the human
will see it.

The `net` badge is what they approved; the endpoint is their own connection.
A manifest without `net` gets no network, whatever the endpoint says — a
provider in memory is the example, and it is also what the test uses.

Key- and access-key authentication have no shape in the WIT (`configure`
carries user and password); a connection configured that way is refused with
`Unsupported` rather than sending an empty password to a guest.

### 4. The lists that were closed open to the same alphabet

`norte-connect::parse_endpoint` accepts any scheme `norte_proto::Scheme`
accepts, instead of `sftp|ftp|s3`; the `s3` bucket rules still apply to `s3`.
The CLI routes an argument as a URL when its scheme is the core's, an archive
composition, or one declared by an **installed** provider plugin — consented or
not, because routing grants nothing and connecting stays fail-closed. The
typo the closed list used to catch (`sfpt://`) moves to `norte doctor`, the
one place that has the connections file and the catalogue side by side: a
connection whose scheme nobody serves is a warning there. A plugin scheme has
no default port the core knows, so only an explicit port matches an explicit
port when a URL is matched against `connections.toml`.

### 5. `norte plugin uninstall` withdraws consent; `norte plugin list` shows the two facts

The CLI could install and never remove. `uninstall <id>` validates the id as a
plugin id before it becomes a path, removes the directory, and writes the
state entry **off** rather than deleting it — `persist_state` merges on write,
so a deleted key would survive in the file and a plugin installed later under
the same id would inherit consent given to another binary. `install --force`
already did this for the same reason. `list` prints what the manager shows:
id, category, approved, enabled, the capabilities approving grants; broken
plugins are counted, and `norte doctor` explains them. A running daemon keeps
its in-memory registry until it rediscovers: connecting by scheme rediscovers
on every dial, running a command fails for want of a binary, and `list`
through that daemon may still show the row until then.

## Consequences

- ADR 0041's second decision is now true. A third party can write a provider,
  install it from a local directory, and have `scheme://host` open through it
  after a human approves it — without touching the core.
- The catalogue is read on every connection attempt. A connection is
  established once and cached by the engine, so this is per session, not per
  operation, and it is what the embedded `Backend` already does per plugin
  call.
- The WIT still moves. Decision 3 of ADR 0041 stands: the first WebDAV or
  Drive plugin that needs what `configure` cannot carry is the specification
  for the next `norte:provider` bump, and every compiled guest recompiles.
- Nothing about a provider plugin reaches the wire. `PluginInfo` does not
  list schemes; the frontends navigate by `VPath` and the daemon decides. If a
  frontend ever needs to know which schemes are servable before typing one,
  that is a `PluginInfo` field and a protocol bump.

## Alternatives considered

- **Reject `Category::Provider` at parse time, like hooks.** Honest and one
  line, and it closes the door ADR 0041 opened. The interface exists, its proof
  runs in the gate, and the missing part was a lookup — rejecting would have
  been declaring the whole path unbuilt to avoid building the lookup.
- **Promote the embedded FTP guest to an installed plugin now.** It would test
  the distribution path with the real guest, at the cost of FTP not working on
  a clean install until someone installs it. The distribution path is tested
  with `provider-mem` instead.
- **Leave `ftp` claimable so a plugin could replace the embedded guest.** The
  first draft did. Review showed what that hands over: every stored FTP
  password, to third-party code the human approved without seeing a scheme.
  Reserved until the promotion happens on purpose.
- **Grant the bare IP, as the FTP guest gets.** Passive FTP needs it; a WebDAV
  plugin does not, and `webdav://localhost` would have handed a guest every
  service on loopback. `ip:port`, with the port declared where the human can
  see it.
- **Let the manifest's `net.hosts` alone decide the endpoint.** A provider's
  destination is the user's connection, not a list the author wrote; a static
  list would make every WebDAV plugin declare every server in advance. The
  host resolving and narrowing is what the FTP guest already relied on, and it
  keeps the approval badge honest.
