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
   exports `on-events(list<event>) -> result<list<effect>, string>`; the
   only `effect` is `notify(string)`. The event carries `seq`, `ts-ms`,
   `op`, `actor`, the path in wire form, `path-to`, the leaf `name` in raw
   bytes, `batch`, and — for a plugin approved for `location = "read"` — a
   token for the **parent directory** of the entry, so an `after-renamed`
   hook can `stat` the result. Writing a sidecar or touching an index would
   be another `effect` variant, would go through the policy engine as
   `actor_kind = plugin`, and is a separate ADR.

4. **Off the critical path, bounded, and it trips a fuse.** The sender
   never waits: a full queue drops the newest event and counts it. The
   dispatcher drains up to 256 events, rediscovers the plugin registry once
   per drain (so an approval given a second ago counts), instantiates each
   hook plugin once per drain, and mints one location session per distinct
   parent directory. Three consecutive failures — no instance, a trap, over
   budget, or the guest's own `Err` — disable that plugin's hooks for the
   rest of the process and say so. A hook never slows a copy and never
   makes one fail.

5. **The sentence crosses as `plugin.notice`** (protocol 0.69.0): a Direct
   notification to human connections only, `{ plugin_id, kind, text }`,
   with `kind` a closed vocabulary — `notify` (the hook's text, masked and
   capped by the daemon as any guest sentence) and `hooks-disabled` (the
   daemon's own, no text). Both frontends compose the line once, in
   `norte-frontend`, id first and labelled by norte, so a plugin's sentence
   cannot pass for one of norte's. Embedded frontends run the same
   dispatcher over their own journal and drain the same channel.

6. **Its own WIT package.** As with `norte:renamer` (ADR 0095): adding it
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
- Listing a hook's events in `PluginInfo`: additive, and not needed for the
  first hook. The manifest digest already covers them.

## Consequences

- A third party can react to what norte did without being able to change
  what norte does, and a hook that misbehaves costs the human a sentence,
  never a file.
- The demo, `org.norte.rename-log`, is the pattern: one event, no
  capabilities, a pure function with host tests, and a sentence written for
  the reader.
- A journal written by a newer build with an op this binary cannot name is
  skipped per event, not per batch: hooks keep working on the ops they know.
- The fuse is per process. A plugin whose hooks were disabled comes back on
  the next start, or when it is disabled and re-enabled in the extension
  manager — which is what the notice tells the reader to do.
