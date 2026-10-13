# 0182 - What a release promises to keep working

- Status: accepted
- Date: 2026-10-13
- Decision makers: Oscar González
- Related: ADR 0004 (wire conventions), ADR 0011 (`initialize`), ADR 0062
  (session schema version), ADR 0094 (WIT version read from the binary),
  spec §11 and §15.

## Context and problem statement

norte is heading for a beta. A beta promises that what a user has keeps
working on the next version. Three surfaces carry that promise, and in
alpha.12 each was in a different state:

- **The wire** had a rule: N/N-1 on the minor while `0.x`, any older minor
  of the same major from `1.0` (`version_compatible`). It is at `0.86.0`:
  86 minors, each one breaking a client two versions old.
- **The plugin WIT** had none. The host served one exact version per
  package, and any bump listed every compiled plugin as broken. M4's exit
  criterion ("a third party can ship a plugin without changing the core")
  does not survive a bump.
- **The files on disk** had no test. `norte.toml`, `keymap.toml` and
  `connections.toml` carry no version and reject unknown keys. The index
  has no schema version. The journal, the UI session and the sync spool
  are versioned.

## Decision

**One direction everywhere: newer reads older.** A newer daemon serves an
older client, a newer host links an older plugin, and a newer binary opens
an older binary's files. The reverse (downgrade, a newer file read by an
older binary) is not promised.

### Wire

The mechanism stays. **The beta is `PROTOCOL_VERSION = 1.0.0`.** From then
on an additive change is a minor bump and a breaking one is a major, so a
beta client keeps working with every 1.x daemon.

### WIT

The host links a guest built against an older **compatible** version of
each package. That is the component model's semver rule, which wasmtime
already applies to imports and exports: the key is the major from `1.0`,
the minor in `0.y`, and the exact version in `0.0.z` or a pre-release. On
top of it, the guest may not be newer than the host
(`norte_plugin_host::wit_mismatch`).

A compatible bump may only **add**: a host function, a new interface, a
new optional kind. A changed signature, a removed function or a new
function the GUEST must export is a breaking bump. wasmtime would accept
the name and fail at instantiation.

Every host function links for every guest, whatever version it names. So
a function added in a compatible bump checks its capability INSIDE, as
`location` does. It never relies on "only new guests import it".

At the beta every `norte:*` package goes to `1.0.0`.

### Disk

- `tests/disk-compat/<version>/` holds one corpus per release, written by
  that release's binary and loaded by every later one. A red test there is a
  user's file that stopped loading. The fix is in the code, never the corpus.
- A renamed or removed config key keeps a serde `alias`, or is accepted and
  ignored with a warning, until the next major. The same goes for a keymap
  command name.
- A rebuildable store (the index) needs a schema version and must rebuild on
  a mismatch instead of failing at query time.

## Consequences

- Each release adds a corpus directory. `tests/disk-compat/README.md` says
  how to freeze one.
- The plugin gate is now looser than "exact". The first compatible WIT bump
  must prove that a guest compiled before it still instantiates, with a
  test. Until then the promise rests on wasmtime's documented behaviour
  (`component::Linker`, "Names and Semver") and on the rule for what a
  compatible bump may add.
- **Not done here:** the index schema version, the keymap command alias,
  and the `1.0.0` bumps themselves. The bumps belong to the beta release.
- 86 wire minors in alpha say the protocol has not settled. Going to `1.0.0`
  is a bet that the beta's surface is the right one. A major later is
  allowed, but it costs every client at once.

## Answered by the decision makers (2026-10-13)

1. `1.0.0` goes with the **beta**, not the final 1.0.
2. Downgrade is **out**. A newer `norte.toml` key aborts an older binary,
   and that is the price of `deny_unknown_fields` catching typos.
