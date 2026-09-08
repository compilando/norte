# 0101 — A hook may write a sidecar, through the policy engine, as a plugin actor

- Status: proposed
- Date: 2026-09-08
- Decision makers: Oscar González
- Related: ADR 0100 (hooks observe and may only speak), ADR 0022 (manifest
  and sandbox), ADR 0023 (the journal), ADR 0024 (policy for agents), ADR
  0057 (the location token), ADR 0081 (permissions are a mutation with a way
  back), ADR 0088 (a declared capability nobody honours is a lie), ADR 0009
  (trash versus remove), spec §7.1, §10

## Context and problem statement

ADR 0100 gave hooks one effect, `notify(text)`. That makes the category
honest — a hook cannot change a file — and it makes it a demo: the only
thing a hook can do with "these three files were renamed" is say so. The
uses that justify hooks at all write something next to what changed: a
`.norte-renames.log` in the directory, a refreshed `.index` or checksum
manifest, a marker that a sync should pick up. ADR 0100 deferred that
deliberately: "writing a sidecar would be another `effect` variant, would go
through the policy engine as `actor_kind = plugin`, and is a separate ADR".

What exists today, and shapes the answer:

- The policy gate already knows a plugin actor. `ScopedPolicy::evaluate`
  treats `Actor::Plugin { id }` exactly like an agent session: every path
  must be inside a scope granted to that id in the `ScopeRegistry`, and
  inside the scope `policy.toml` rules decide, with `actor = "plugin"`
  available to the rule author. Nothing grants a plugin a scope, so the
  branch is reachable by nobody.
- The journal already records `actor_kind = plugin` with the plugin id, and
  `revertible_for(actor)` already partitions undo by actor.
- The manifest already parses `fs-write = "scoped"` and it is "reserved; no
  host door uses it yet" — a declared capability nobody honours, which ADR
  0088 calls a lie and which the parser therefore should be rejecting.
- The core has no primitive that writes bytes into a new file from memory:
  `fs.create` makes an empty file, and content only arrives by copy. The
  journal's reversal for a created node is `Delete`; there is no reversal
  for "the previous content of a file", so any overwrite is irreversible
  unless the previous content goes somewhere first.
- Every hook effect is rate-limited and fused (ADR 0100 decision 4); the
  guest never sees a path it can name, only a location token for the parent
  directory of the event (ADR 0057).

The questions: where may a plugin write, who approves it, how does an
overwrite stay undoable, whose undo does it belong to, and what a policy
denial does to the fuse.

## Decision drivers

- Rule 4: every mutation goes through the journal with a way back or an
  explicit `Irreversible`.
- Rule 9: plugins never touch the filesystem directly; the core and the
  policy engine mediate.
- ADR 0088: what the human approves must be exactly what is granted.
- A hook fires after every mutation; anything that asks the human per write
  is unusable, and anything that lets a plugin write anywhere is a second
  agent without a session.

## Considered options

### Option A — a manifest-declared sidecar name, written by the core under the event's parent, as a plugin actor through the gate

The manifest declares what the hook may write:

```toml
[capabilities]
fs-write = { sidecar = [".norte-renames.log", ".norte-index.json"] }
```

A list of **exact file names** (no globs, no separators, no `.`/`..`), each
shown at approval as a badge `fs-write:.norte-renames.log`. The effect is

```wit
variant effect {
    notify(string),
    write-sidecar(sidecar),
}
record sidecar {
    name: list<u8>,        // one of the manifest's names, raw bytes
    content: list<u8>,     // at most 64 KiB
    if-exists: on-exists,  // refuse | replace
}
enum on-exists { refuse, replace }
```

The dispatcher, for each effect: checks the name is in the manifest list;
resolves the target as `parent(event.path) / name`; grants the plugin id a
**transient scope** in the `ScopeRegistry` over that parent with ops
`{create, delete(trash)}` and a TTL of one drain; and calls the engine's
new `write_file_as(path, bytes, Actor::Plugin { id })`, which goes through
`PolicyGate::evaluate` like any agent op, so `policy.toml` rules with
`actor = "plugin"` apply and `ask` rules are honoured (the request shows in
`policy.pending` attributed to the plugin). The scope is dropped after the
drain whatever happened.

`write_file_as` is one new core op: create the file with content when it
does not exist (journal `created`, reversal `delete`); when it exists and
`if-exists = replace`, trash the old node to the logical trash first, then
create (two entries, one `batch_id`: reversal `restore_trash` + `delete`,
so undo restores the previous content); when it exists and `refuse`, the
effect fails with a reason. Nothing is ever overwritten in place. Never
under a protected root, never when the parent is `$HOME` or `/` (ADR 0100
decision 5), never outside `file://` in v1.

`PolicyOp::Create` widens its meaning from "an empty file" to "a file, with
or without content": the permission is the same one — bringing a name into
existence — and a rule that allowed `create` allowed the name, not its
emptiness.

Undo: plugin rows belong to the plugin actor. The human's `undo_session`
does not touch them and does not see them; `norte audit` does. A later
`plugin.undo <id>` can revert a plugin's rows as a unit if a case appears.

Fuse: a policy `Deny` or a pending `Ask` is a verdict, not a failure of the
guest, and does not count towards the fuse. It is reported once per plugin
per drain as a new `plugin.notice` kind, `effect-denied`, rate-limited like
`notify`. A malformed effect — a name not in the manifest, content over the
cap, a bad `on-exists` — is the guest's fault and counts.

- Good: reuses the gate, the scope registry, the journal, the trash, the
  approval digest and the notice channel; nothing new is invented, every
  write is attested with the plugin's id, and the human's policy has the
  last word without a prompt per write.
- Good: what the human approves is a list of file names, which is what a
  sidecar is; the badge says the truth.
- Bad: `replace` costs a trash entry per write, so a hook that rewrites an
  index on every mutation fills the logical trash; the trash sweeper's
  existing budget bounds it, but a chatty hook makes that budget visible.
- Bad: names only, no subdirectory, so a hook cannot keep its files in a
  `.norte/` folder; that is a later widening (`sidecar-dir`) with its own
  question about creating the directory.

### Option B — `fs-write = "scoped"` as a general write door with human-granted scopes, like an agent

The plugin asks for a scope the way an agent does (`policy.request_scope`
attributed to the plugin id), the human grants roots and ops in the
approval dialog, and the hook may create, move, delete and write anything
inside. `fs-write = "scoped"` stops being reserved and means "may write
inside the scopes you grant it".

- Good: one model for agents and plugins; powerful.
- Bad: a scope is a *directory tree*; a hook fires for mutations anywhere,
  so the useful scope is "wherever anything changes", which is the whole
  home directory. Approving that is approving an agent without a session
  or a conversation. It is exactly the second gate ADR 0100 refused.
- Bad: a general write has no reversible shape; the core would need to
  journal arbitrary overwrites, which is a content-versioning journal, not
  this one.

### Option C — no writing; hooks stay observers, and a sidecar is a Lua script or an external program

Keep ADR 0100 as the final word. Users who want a rename log write it with
the TUI's Lua statusbar hook, or an opener.

- Good: nothing to secure.
- Bad: Lua is TUI-only (the window has none, ADR 0097), so the log exists
  in one frontend; and it reintroduces the problem hooks were built to
  remove — a frontend deciding what happens after a mutation instead of
  the core (ADR 0077).

## Decision

Option A, when accepted. Concretely:

1. `norte:hook@0.2.0` adds `write-sidecar` to `effect` with the record
   above. Guests built against 0.1.0 list as mismatched until rebuilt
   (ADR 0094), which is the rule for every package bump.
2. `[capabilities] fs-write = { sidecar = [names] }` replaces the reserved
   `fs-write = "scoped"`, which is rejected at parse from now on (ADR 0088).
   Names are validated as single segments, non-empty, not `.`/`..`, at most
   16 per manifest, and enter the approval digest and the badge list.
3. The core gains `Engine::write_file_as(path, bytes, actor)` with the
   create / trash-then-create / refuse semantics above and a 64 KiB cap,
   journaled as `created` (+ `trashed` in one batch when replacing).
4. The dispatcher grants a transient scope per effect and routes every
   write through `PolicyGate::evaluate` as `Actor::Plugin { id }`. Policy
   verdicts are reported as `plugin.notice` kind `effect-denied`
   (protocol 0.70.0, additive) and do not trip the fuse.
5. Plugin rows are outside the human's undo and inside the audit.
6. A demo, `plugins/rename-log` grows `after-renamed → .norte-renames.log`
   with `replace`, appending the batch to the previous content it reads
   under the location token — the read and the write both under consent.

## Consequences

- Positive: a hook can do the one useful thing a hook does, and the human
  approves a list of file names, sees every write attributed to the plugin
  in the audit, and can deny it all with one `policy.toml` rule.
- Positive: no new trust primitive. The plugin actor already existed in the
  gate and the journal; this is the first thing that reaches it.
- Negative: two journal entries per replaced sidecar, and a trash entry per
  rewrite; a hook that rewrites on every mutation is visible in `du` of the
  trash. The rate limit on effects is what keeps it bounded.
- Negative: sidecars are flat files in the mutated directory. Anyone who
  wants a `.norte/` folder waits for the widening, which has to decide who
  creates the directory and how that is undone.
- Negative: a second WIT package bump within a day of the first. Acceptable
  because 0.1.0 shipped to nobody; the rule stays that a bump costs every
  guest a rebuild.

## Open before acceptance

- Whether `effect-denied` should reach the human at all, or only the daemon
  log: it is the user's own policy speaking, and a notice per denied write
  could read as the plugin nagging.
- Whether the `ask` action should be honoured for plugins at all, or mapped
  to `deny` with a one-time notice: a hook fires unattended, and a pending
  approval that nobody is looking at expires by TTL either way.
