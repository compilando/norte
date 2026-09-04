# Operation hooks: design proposal

**Status:** proposal, not approved. Nothing here is built. Written so the
decision can be taken with the code in view; the plugin plan (A2) rejects a
hook manifest today precisely because these questions had no answer.

**What exists:** `Category::Hook` and `HookContrib { on }` in the manifest,
rejected at parse with `HookNotImplemented`. Spec §7.1 names "operation
hooks" among the interfaces WIT covers. The journal records every mutation
with `actor_kind ∈ {user, agent, plugin}`, `op ∈ {created, removed, trashed,
renamed}`, `path`, `path_to`, and a reversal. Notifications already cross
the wire with a reason (#322/#327) and both frontends show them.

## The question the plan asked

"What does an operation hook hook into, and what may it do when a mutation
is about to happen?" Two answers, and they are different products:

1. **A hook that can veto** (`before-*` returning deny) is a policy
   decision. The policy engine is the one place that says whether a
   mutation happens, it is what the human approves scopes against, and its
   verdicts are what the audit chain attests. Letting third-party wasm
   return "no" adds a second gate whose reasons are not in the policy
   vocabulary and whose failure (a trap, a timeout) has to be resolved as
   either allow or deny — both wrong for some plugin.
2. **A hook that observes** (`after-*`) is a subscriber to the journal. It
   changes nothing, so its failure changes nothing, and it sees exactly what
   the journal saw: every mutation from every frontend, the CLI, an agent
   session or a plugin command, in one place.

## Decision proposed: hooks are journal subscribers, v1 is after-only

- **Source of events: the journal's commit path.** A hook never hangs off a
  handler. After an entry is committed, the core enqueues an event for each
  hook plugin whose manifest declares that `on`. Parity by construction
  (ADR 0077): no frontend has a say in whether a hook fires.
- **Event vocabulary = journal ops.** `after-created`, `after-removed`,
  `after-trashed`, `after-renamed`, plus `after-batch` (one event per
  `batch_id` when its last entry lands, with the count). Closed vocabulary,
  validated at manifest parse: an unknown `on` is a manifest error, like an
  unknown capability.
- **What the hook receives:** `op`, `actor-kind`, `path` and `path-to` as
  bytes (rule 1), `ts-ms`, `seq`. NOT `reversal_ref` (a trash path is the
  host's business) and NOT the actor id of an agent session. With
  `location = "read"` it also gets a location token for the parent
  directory of `path`, minted like a column's, so an "after-renamed" hook
  can `stat` the result.
- **What the hook may return:** `result<list<effect>, string>`, where v1's
  only effect is `notify(text)`: a sentence the host masks and caps
  (`guest_reason`, #332) and sends through the existing notification
  channel, attributed to the plugin. A hook cannot mutate. A hook that
  wants to write a sidecar or update an index is a later `effect` variant
  that goes through the policy engine as `actor_kind = plugin`, and that is
  a separate ADR.
- **Off the critical path, bounded, fail-closed.** Events go to a bounded
  queue per plugin (say 256); a full queue drops the oldest and counts it.
  Each call runs under the runtime's epoch deadline. Three consecutive
  failures (trap, timeout, over-budget) disable the plugin's hooks until
  re-enabled, and `norte doctor` reports `plugin-hook-disabled` with the
  count. A hook never slows a copy and never makes one fail.
- **Its own WIT package, `norte:hook@0.1.0`**, world `norte-hook`, importing
  `host-log`, `host-config`, `norte:location/location@0.2.0`, exporting
  `hook::on(event) -> result<list<effect>, string>`. Same reasoning as
  `norte:renamer` (ADR 0095): adding it does not move `norte:plugin`.
- **No wire change in v1.** `PluginInfo` already lists contributions; the
  effect rides the notification that exists. The extension manager shows
  the events a hook listens to, as it shows a renamer's titles.

## What this deliberately leaves out

- `before-*` hooks of any kind. If a use case appears that observation
  cannot serve ("refuse to delete anything under `.git`"), that is a policy
  rule the human writes, and the right feature is policy rules with a
  plugin-provided *predicate* — still a policy decision, still attested.
- Hooks on reads, listings, or navigation: not mutations, not journaled,
  and a hook on "the user looked at X" is a tracker.
- Hooks running in the TUI's Lua (ADR 0026): Lua is the user's `.bashrc`,
  not a third-party plugin, and it already has a statusbar hook. Whether
  Lua should also subscribe to journal events is a parity question for the
  window, not this design.

## Cost

Roughly C3-sized: WIT package and host instance, a journal subscriber with
its queue in the core, manifest vocabulary, `doctor` finding, a demo
(`plugins/rename-log`: appends nothing, notifies "renamed 3 files" after a
batch — or, if the effect grows, writes a `.norte-renames.log` under
policy). One ADR. No proto bump unless the notification needs a
`plugin_id` field, which it may.

## For Oscar to decide

1. After-only in v1, with veto hooks explicitly refused as a plugin feature.
2. Agent mutations fire hooks like user ones (yes, if the source is the
   journal), and hooks do not learn which agent session.
3. Whether `notify` is enough for a first demo, or whether the "write a
   sidecar under policy" effect is what makes hooks worth building at all —
   in which case the ADR is bigger and touches the policy engine's actor
   model before any WIT.
