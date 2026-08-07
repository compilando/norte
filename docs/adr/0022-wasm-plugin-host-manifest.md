# 0022 - WASM plugin host, manifest, and capabilities

- Status: accepted
- Date: 2026-07-15
- Decision makers: Oscar González
- Related: specification section 7; ADRs 0010 and 0020

## Context

M4 opens norte to third-party extensions. The project needs a typed runtime,
manifest, enforceable capabilities, clear separation between plugins/scripts/
built-ins, local distribution, and useful governance before M3's policy engine
is connected. The extension manager must make category and authority visible
rather than mixing every customization into one opaque list.

## Decision

### Runtime and interfaces

Use Wasmtime and the Component Model with a `norte:plugin` WIT world. Interfaces
cover `previewer`, `provider`, `command`, `columns`, and `hook`; the VFS provider
contract is projected into WIT. Wasmtime enters only with runtime work, not the
manifest/catalogue scaffold.

### Manifest

Each local plugin contains `plugin.toml` and `plugin.wasm` under
`config_dir/plugins/<id>/`:

```toml
[plugin]
id = "org.norte.syntax-preview"
name = "Syntax Preview"
publisher = "norte"
version = "0.1.0"
category = "previewer"

[contributions]
previewer = [{ mimetypes = ["text/*", "application/json"] }]

[capabilities]
fs-read = "scoped"
fs-write = "none"
exec = "none"
```

IDs use reverse-DNS form. Contributions declare concrete commands, MIME types,
schemes, or hooks. Capabilities include scoped filesystem access, an allowlist
of network hosts, and AI chat. `exec` must always be `none`; any other value is
a manifest error.

### Enforcement and categories

Start each component with an empty WASI Preview 2 context: no filesystem,
network, environment, or ambient resources. Host functions independently check
manifest capabilities and resolve scoped resource tokens. A missing declaration
means the syscall is unavailable.

Keep three visibly separate extension levels:

| Level | Source | Sandbox |
| --- | --- | --- |
| Plugins | Installed third-party WASM | WASI plus norte capabilities |
| Scripts | User-owned Lua configuration | No sandbox; user's permissions |
| Built-ins | Native commands in the binary | Not applicable |

The extension manager groups by level and primary category, always displays
capability badges, and marks unapproved plugins. Approval and enablement are
human-only operations. Remote registries and UI installation are deferred;
initial distribution is local files.

M4 enforces the WASM sandbox. M3 later layers per-operation allow/ask/deny,
journalling, and undo over host calls.

## Implementation addenda

### P2: runtime

Wasmtime 46 executes real components. `previewer::render`, `command::run`, and
host logging/scoped reads work end to end. The guest may call `read-scoped`, but
the host returns an error without touching a resource unless `fs-read=scoped`
and the token was explicitly seeded. Example guests build separately for
`wasm32-wasip2`; component tests skip only when the target is absent.

The original P2 left provider/columns/hook, per-category worlds, store memory and
CPU limits, policy integration, and the extension manager for later work.

### P3: catalogue and human governance

`PluginRegistry` discovers manifests and exposes `plugin.list` with valid
plugins and sanitized errors. Broken manifests do not hide the rest of the
catalogue, and errors reveal only a basename. The TUI treats manifest names and
publishers as untrusted display text.

Only `Actor::User` may call approval or enablement methods. State is written
atomically to `plugins-state.toml`. The TUI overlay renders server-owned state;
it does not implement plugin policy. Manual file installation remains the only
installation path. A corrupt state file currently produces a warning and an
empty catalogue, and embedded/daemon writers are not yet coordinated.

### P4: command execution

Approved and enabled command plugins can run through the core, protocol 0.14.0,
and CLI. Resolution fails closed for unknown, unapproved, disabled, or missing
plugins. Resolve under the registry lock, then compile and instantiate outside
it through `spawn_blocking`. Return coarse redacted runtime errors to clients.
An end-to-end guest test covers discovery, denial, approval, enablement, and
execution.

TUI command-palette integration, the other WIT interfaces, fine-grained policy,
journal integration, and instance caching remain separate work.

### P5: viewer previewers

The core selects the first approved and enabled previewer matching a MIME glob,
reads at most 1 MiB through the normal `fs.read` gate, and passes those bytes to
the guest. The guest never opens the filesystem itself. `plugin.preview` is
unavailable unless `fs.read` is available, runtime errors are redacted, and the
viewer labels plugin output.

Content sniffing, explicit previewer priority, pane previews, streaming, and the
provider/columns/hook interfaces remain deferred.

### 2026-07-21 hardening

- Approval is bound to a SHA-256 digest of the canonical complete manifest,
  including category and contributions as well as capabilities. Any change, or
  legacy approval without a digest, requires fresh consent.
- Reject all directories sharing a plugin ID so one cannot inherit another's
  approval.
- Canonicalize `plugin.wasm` and require it to remain inside its plugin
  directory.
- Reject guest return values above 4 MiB and component artifacts above 64 MiB
  before compilation.
- Keep the standard WASI linker because Rust `wasm32-wasip2` guests import its
  interfaces. Security comes from the empty `WasiCtx` and mediated resources,
  not from omitting interface definitions.

## Consequences

norte gains typed, sandboxed extensions with visible authority and a catalogue
that separates third-party software from user scripts and built-ins. The tested
native provider contract informs the WIT design. Wasmtime is a large dependency,
guest authors need a WASI component toolchain, and several interfaces and
policy/journal wiring remain incomplete. Guest bindings should eventually live
in a separate permissively licensed SDK; the host remains part of the AGPL core.

## Amendment 2026-08-08: a declared hook is now rejected, not accepted and ignored

"provider/columns/hook interfaces remain deferred" was written when all three
were deferred together. Two of them arrived — `provider` in ADR 0032, `columns`
in ADR 0037. `hook` did not, and the manifest kept accepting it: `Category::Hook`
and `HookContrib` parse, reach the catalog, and would appear in the plugin
manager as a plugin like any other. Nothing anywhere would ever call it. There
is no `hook` interface in the WIT, no world, and no call site in the host or the
core.

So a manifest declaring a hook installed something inert, and its author would
have found out by nothing ever happening. `Manifest::from_toml` now rejects it —
by primary category or by contribution, since it is the declaration that makes
the promise — with `ManifestError::HookNotImplemented` and a reason that says so.

`Category::Hook` stays. Spec §7.1 names operation hooks among the interfaces WIT
is meant to cover, so removing the variant would move the code away from the
specification rather than towards it; and its `digest_tag` is part of the
approval digest, which is never reordered.

What a hook may do is a policy and journal question before it is a WIT one — a
guest that runs before a mutation can veto, delay or observe it, and each of
those is a different contract. That design is not attempted here.
