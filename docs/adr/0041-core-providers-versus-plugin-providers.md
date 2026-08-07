# 0041 - Which providers live in the core, and which arrive as plugins

- Status: accepted
- Date: 2026-08-08
- Decision makers: Oscar González
- Related: spec §5 (VFS and `Capabilities`), §7.1 (WASM plugins), §15 (M4 exit
  criterion), hard rules 3 (cancellation), 8 (new dependencies) and 9 (no
  direct plugin filesystem access); ADR 0022 (plugin host and manifest),
  ADR 0032 (the `provider` WIT interface), ADR 0033 (FTP as a plugin
  provider), ADR 0039 (provider attributes on the wire). Plan:
  `docs/superpowers/specs/2026-08-07-plugins-and-delivery-plan.md`.

## Context

norte ships four providers compiled into the core — local, SFTP, object
storage (S3) and archives — and one, FTP, that runs as a WASM plugin through
the `provider` WIT interface (ADR 0033). The plugin path therefore works: a
real provider carries real traffic through it today.

That raises a question the project had not answered. Should the four built-in
providers also become plugins? The attraction is not technical. A plugin has
its own release cadence, so another team could maintain S3 without touching
this repository or waiting for our release, and new backends — WebDAV, Google
Drive, MTP, whatever somebody needs — could arrive without our involvement.
M4's exit criterion says exactly that: "a third party can ship a plugin
without changing the core."

Two things had to be established before deciding, and both were checked against
the tree rather than assumed.

### What the `provider` WIT interface can express

It has capabilities (read-only, case sensitivity, case preservation), `stat`,
paginated `list-dir`, ranged `read`, `configure` with an endpoint and
credentials, a transactional `writer` resource, `make-dir`, `remove` and
`rename`. That is a complete simple filesystem.

It does not have: server-side copy, trash, provider attributes (ADR 0039),
resumable writes, or cancellation. The WIT's own comments already say the
resume gap out loud — "declararlo mentiría" — and the attribute gap is
recorded in ADR 0039, which notes that a `columns` plugin cannot read an SFTP
mode bit because it has no access to the provider.

### What the sandbox can link

`aws-lc-rs`, the cryptography backend under both `rustls` (S3, via opendal and
reqwest) and `russh` (SFTP), does not compile to wasm. This is not a new
finding: the WIT header already records it as the reason FTPS is deferred.

## Decision

### 1. The four built-in providers stay in the core

Local, SFTP, object storage and archives remain Rust crates implementing the
`Provider` trait.

Moving S3 out would cost, concretely:

- **TLS.** A WASM guest cannot link `aws-lc-rs`, so the plugin would speak
  plaintext HTTP. That alone is disqualifying.
- **Server-side copy.** `copy_native` and the `SERVER_COPY` capability let S3
  copy an object inside a bucket without moving bytes. Through the WIT, copying
  a 5 GiB object becomes a download and a re-upload.
- **Attributes.** `s3.etag` and `s3.content_type` reach the columns because the
  provider produces them (ADR 0039). The WIT has no attribute channel, so those
  columns would go blank.
- **Trash, resume and cancellation.** Absent from the interface; hard rule 3
  requires cancellation of every long-running operation.

The same reasoning covers SFTP (also `aws-lc-rs`) and archives (composition
over an inner provider, which the WIT cannot express). Local is the trivial
case: sandboxing the filesystem provider behind a sandbox whose purpose is to
keep guests off the filesystem is circular.

### 2. The plugin path is how NEW providers arrive

WebDAV, cloud drives, device protocols, anything a user needs and we have not
built: these are the plugin path's constituency. They tend to be simpler than
S3, and what the WIT already offers — list, read, write, rename, remove, with
a host-mediated network capability — is what they need.

FTP stays as it is and keeps its role: it is the interface's proof and its
regression test.

### 3. The gaps close on demand, not in advance

The missing pieces — server-side copy, trash, attributes, resume,
cancellation — are not scheduled. Each is added when a plugin somebody actually
wants requires it, and the plugin is the specification for the shape it takes.

Building all five in advance would guess at five interfaces with no consumer to
correct them, and every guess is a WIT bump. Which brings us to the reason this
ADR has a fourth decision.

### 4. Third-party plugins are not viable until the WIT package splits

The package version travels inside every interface name
(`norte:plugin/previewer@0.5.0` → `@0.6.0`), so **any** bump — additive or
not — makes every previously compiled `.wasm` fail to instantiate, on the
import side. The WIT header records this having been verified empirically
twice.

Inside this repository it is invisible: the example guests are recompiled from
the current WIT every time. For a third party it is fatal — their plugin stops
loading on our next release, whatever we changed.

So M4's exit criterion is currently satisfied only within a single release, and
decision 3 makes that worse by design: closing gaps on demand means the WIT
will keep moving. **Splitting `provider` into its own package (ADR 0032's
recorded debt) is therefore a precondition for inviting anyone outside this
repository to write a plugin, and it comes before the gap-closing work rather
than after it.**

ADR 0032 recorded the blocker as `wit-parser` 0.239 not supporting nested
`wit/deps/` packages. The tree is now on 0.251; whether that lifts the blocker
is the first thing the split's session tests.

### 5. Independent maintenance does not require a plugin

If the goal for a specific provider is that another team maintains it, the
cheaper route is the one the codebase already allows: `norte-vfs-object` is an
independent crate, and the project's rule is that a provider must not know
about other providers. Such a crate can move to another repository and be
consumed as a dependency — no sandbox, no lost capabilities, no WIT.

A plugin buys isolation from a provider we do not trust. It does not buy
independent maintenance; a crate boundary already does that.

## Consequences

The four built-in providers keep every capability they have, and users keep
TLS, server-side copy, resumable transfers and the S3 columns.

New backends can arrive without touching the core, within the limits of what
the WIT expresses today — which is enough for a simple remote filesystem and
not enough for a sophisticated one. A plugin author meets those limits as
missing features rather than as errors, so the manifest's declared
capabilities and the honesty rules of phase 10a still apply: a plugin provider
that cannot resume must not claim it can.

The gap list becomes a backlog driven by demand rather than a roadmap. The
risk is that the first serious third-party provider needs three of the five at
once; the mitigation is that it will say so, which is more information than we
have now.

Until the package split lands, plugin authors outside this repository should be
told plainly that their artefact is tied to one norte version. Publishing a
plugin API while that is true would be promising something we cannot keep.

## Alternatives considered

**Move everything to plugins.** Rejected on TLS alone, before the capability
losses are counted. It would also make the local provider — the one the sandbox
exists to protect — run inside that sandbox.

**Close every WIT gap first, then decide.** Rejected: five interfaces designed
without a consumer, each a breaking bump, and no way to tell whether any of
them is the shape a real plugin needs.

**Keep the status quo and say nothing.** Rejected because the status quo has an
unstated rule — "providers are core, except FTP, for historical reasons" — and
an unstated rule cannot be argued with when the next provider shows up.
