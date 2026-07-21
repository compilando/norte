# 0010 - Extension boundaries between core, plugins, and configuration

- Status: accepted
- Date: 2026-07-12
- Decision makers: Oscar González
- Related: specification section 7 and M2 planning

## Context

M2 added SFTP, object storage, archives, and FTP while M4 was expected to add a
WASM plugin host. The project needs a repeatable rule for deciding which
integrations are native providers, sandboxed plugins, Lua scripts, or declarative
external commands.

The M2 transfer path requires native streaming, cancellation, resume, hostile
input handling, and secret access. The WASM host did not yet exist, plugins
never receive process execution, and mature native implementations are needed
before a useful WIT provider interface can be designed.

## Options considered

- Make all remote and archive providers plugins immediately. This isolates
  dependencies but blocks M2 on M4, adds serialization to every transfer chunk,
  and moves keyring/archive-bomb boundaries into an immature extension API.
- Keep milestone-critical and security-sensitive data paths native, reserve
  long-tail providers and presentation extensions for WASM, and describe
  external executables in configuration.
- Keep every provider in-tree permanently. This is simple but contradicts the
  promised third-party provider ecosystem and grows dependencies without bound.

## Decision

Use the following order of classification:

1. **Core crate or subsystem:** code belongs in-tree when it is on the copy
   engine's streaming/cancellation/resume path, handles secrets or a documented
   threat, is required by a milestone, or is needed to establish the plugin
   contract. M2 therefore includes native SFTP, object, archive, and FTP
   providers.
2. **WASM plugin:** long-tail providers, MIME previewers, columns, hooks, and
   commands use the M4 host. Plugins do not execute external processes or manage
   secrets outside host-mediated facilities.
3. **Declarative configuration:** `openers.toml` integrates external programs
   such as `bat`, `delta`, `unrar`, and `7z`, with runtime detection and a clear
   unavailable-program message.

`bat` is an opener, not a plugin. In-pane syntax highlighting is a WASM
previewer. The built-in viewer keeps encoding-aware text handling rather than
delegating all text to an external program that may assume UTF-8.

FTP remains native for M2 but is the first candidate for migration to a WIT
provider. That migration is the practical test that a third party can implement
a provider without modifying the core.

## Consequences

M2 remains independent of M4, and the future WIT interface can be projected
from six tested native providers. External-program security policy remains in a
single declarative mechanism. The costs are the native Russh, OpenDAL, and FTP
dependency trees plus intentional later migration work for FTP. M4 issues track
openers, a WASM syntax previewer, and FTP migration.
