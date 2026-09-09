# 0100 — A hook observes what the journal recorded, and may only speak

- Status: accepted
- Date: 2026-09-08
- Decision makers: Oscar González
- Related: ADR 0022 (manifest and sandbox), ADR 0023 (the journal), ADR
  0041 decision 4 (one WIT package per concern), ADR 0057 (the location
  token), ADR 0077 (a decision taken once, in the core), ADR 0088 (a
  declared capability nobody honours), ADR 0094 (one served version per WIT
  package), ADR 0095 (the renamer, whose shape this follows), spec §7.1
  ("operation hooks")

## Context

Spec §7.1 names operation hooks among the interfaces WIT covers, and the
manifest has carried a `hook` category and a `[[contributions.hook]]`
section since ADR 0022. Nothing executed them, so since ADR 0088 a manifest
declaring one was rejected at parse: an inert plugin the extension manager
paints as a normal one is a promise nobody keeps.

The open question was what a hook hooks into, and what it may do when a
mutation is about to happen. Two answers, and they are different products:

1. **A hook that can veto** (`before-*` returning deny) is a policy
   decision. The policy engine is the one place that says whether a
   mutation happens; it is what the human approves scopes against, and its
   verdicts are what the audit chain attests. Third-party wasm returning
   "no" would be a second gate, with reasons outside the policy vocabulary
   and a failure mode (a trap, a timeout) that has to resolve as either
   allow or deny — both wrong for some plugin.
2. **A hook that observes** (`after-*`) is a subscriber to the journal. It
   changes nothing, so its failure changes nothing, and it sees exactly what
   the journal saw: every mutation from every frontend, the CLI, an agent
   session, a batch, an undo, in one place.

## Decision

1. **Hooks are journal subscribers; v1 is after-only.** The source of
   events is the journal's commit path: `Journal::record_entry` offers every
   committed row to a `HookSender` after the insert is durable. No handler
   and no frontend has a say in whether a hook fires (ADR 0077). Agent
   mutations fire hooks like human ones; a hook learns the actor's *kind*
   and never the agent's session id.

2. **The vocabulary is the journal's, in the past tense.** `on` is one of
   `after-created`, `after-removed`, `after-trashed`, `after-renamed`,
   `after-mode-changed`, closed and validated at manifest parse
   (`HookUnknownEvent`); a `category = "hook"` that listens to nothing is
   rejected too (`HookWithoutEvents`). There is no `after-batch`: events
   carry `batch`, and the guest receives a **list** of events per call, so a
   rename batch reaches it as a group when it fits in a drain.

3. **A hook may only speak.** `norte:hook@0.1.0`, world `norte-hook`,
   exports `on-events(list<event>, dropped: u64) -> result<list<effect>,
   string>`; the only `effect` is `notify(string)`. The event carries
   `seq`, `ts-ms`, `op`, `actor`, the path in wire form **without
   userinfo**, `path-to`, the leaf `name` in raw bytes, `batch`, and — for a
   plugin approved for `location = "read"` — a token for the **parent
   directory** of the entry, so an `after-renamed` hook can `stat` the
   result. `dropped` is how many events the queue lost since the previous
   call: a hook that counts says "at least". Writing a sidecar or touching
   an index would be another `effect` variant, would go through the policy
   engine as `actor_kind = plugin`, and is a separate ADR.

4. **Off the critical path, bounded, and it trips a fuse.** The sender
   never waits: a full queue drops the newest event and counts it. The
   dispatcher drains up to 256 events in `seq` order, rediscovers the plugin
   registry once per drain (so an approval given a second ago counts), keeps
   one live instance per hook plugin while its `.wasm` is unchanged, and
   mints one location session per distinct parent directory. Three
   consecutive failures — no instance, a trap, over budget, or the guest's
   own `Err` — disable that plugin's hooks and say so; disabling the plugin
   in the extension manager re-arms the fuse, so the notice's remedy is
   true. Notices are one per plugin per drain and four in a burst then one
   per second, so a chatty hook cannot bury the status bar — or the notice
   about itself. A hook never slows a copy and never makes one fail.

5. **What a hook is not shown.** Entries under a protected root (the
   daemon's own state) are withheld entirely, not just their contents;
   entries recorded before the drain that first saw the plugin consented are
   not delivered to it (they may predate the approval); the location token
   is never minted for `$HOME` or the filesystem root — a hook looks at the
   directory of a mutation, not at the disk — and never climbs to a root
   marker. **A hook may not declare `net`** (`HookWithNet`): it receives the
   path of every mutation, and with a socket that is an exfiltration channel
   the approval badge does not describe. A hook's events show in the
   approval as `hook:<event>` badges, so a plugin with no other capability
   is never approved over an empty list.

6. **The sentence crosses as `plugin.notice`** (protocol 0.69.0): a Direct
   notification to human connections only, `{ plugin_id, kind, text }`,
   with `kind` a closed vocabulary — `notify` (the hook's text, masked and
   capped by the daemon as any guest sentence) and `hooks-disabled` (the
   daemon's own, no text). Both frontends compose the line once, in
   `norte-frontend`, id first and labelled by norte, so a plugin's sentence
   cannot pass for one of norte's. Embedded frontends run the same
   dispatcher over their own journal and drain the same channel.

7. **Its own WIT package.** As with `norte:renamer` (ADR 0095): adding it
   does not move `norte:plugin`, and bumping it later cannot invalidate a
   previewer.

## What this deliberately leaves out

- `before-*` hooks of any kind. If a use case appears that observation
  cannot serve — "refuse to delete anything under `.git`" — it is a policy
  rule the human writes, and the right feature is policy rules with a
  plugin-provided *predicate*: still a policy decision, still attested.
- Hooks on reads, listings or navigation: not mutations, not journaled, and
  a hook on "the user looked at X" is a tracker.
- Hooks in the TUI's Lua (ADR 0026): Lua is the user's `.bashrc`, not a
  third-party plugin. Whether Lua should also subscribe to journal events is
  a parity question for the window, not this decision.
- A `doctor` finding for a tripped fuse: the fuse is a fact about a running
  process, and `doctor` reads the disk. The notice is where the human is.
- A cumulative CPU budget across drains: each call has the runtime's epoch
  deadline, and a hook that spends it every drain holds one blocking thread
  for as long as mutations arrive. Self-limiting for the daemon — the queue
  drops and counts — but a way to starve other hooks; a per-plugin budget is
  a later change.

## Consequences

- A third party can react to what norte did without being able to change
  what norte does, and a hook that misbehaves costs the human a sentence,
  never a file.
- The demo, `org.norte.rename-log`, is the pattern: one event, no
  capabilities, a pure function with host tests, and a sentence written for
  the reader.
- A journal written by a newer build with an op this binary cannot name is
  skipped per event, not per batch: hooks keep working on the ops they know.
- The fuse is per process and re-armed by disabling the plugin: a plugin
  whose hooks were switched off comes back on the next start, or when it is
  disabled and re-enabled in the extension manager — which is what the
  notice tells the reader to do, and the dispatcher makes true by dropping
  the fuse of any plugin that stops being consented.
- `[[contributions.hook]]` on a plugin of another category is rejected
  (`HookOnOtherCategory`): only `hook` plugins are dispatched, and a
  contribution that never fires is the inert plugin this ADR replaces.
