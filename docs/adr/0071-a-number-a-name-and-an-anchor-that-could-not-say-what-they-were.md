# 0071 - A number, a name and an anchor that could not say what they were

- Status: accepted
- Date: 2026-08-23
- Decision makers: Oscar González
- Related: ADR 0004 (unknown fields are ignored), ADR 0005 (collision policy),
  ADR 0063 (delivering a result through progress), ADR 0066 (renderers use a
  Rust UI host), issues #251, #265, #282.

## Context and problem statement

Three findings from three different reviews turned out to be the same shape of
bug, and all three needed the wire to change. Bundling them into one bump
(0.52.0 → **0.53.0**) is the point of this record: each one alone is a field,
and each one alone would have cost its own version window.

The shape: **a value crossed the wire having already lost the thing that made
it trustworthy, and the receiver had no way to get it back.**

### #251 — a count that could not say it was a lower bound

`fs.dir_size` walks a tree and counts. Subtrees it cannot read went into a
local counter, produced a `tracing::info!`, and stopped there. A tree half of
which returned `EACCES` therefore reported `Completed` with a confident total.

The method exists to answer *"does this fit on the destination?"*. A number
that is silently too small is the dangerous direction of wrong: it says yes to
a copy that will run out of space halfway.

`fs.compare` already solved the same problem for rows — every row carries a
`confidence` — and counting had no equivalent.

### #265 — a name that had already been converted

`PluginLoadError.dir` is a `String`, and the daemon filled it with
`to_string_lossy()`. A plugin directory called `caf\xff` — a legal name on
Linux, and fixture `lossy_collapse_ff` in the canonical corpus — arrived at
the frontend already converted.

`norte_frontend::display_name` cannot recover that. It raises `lossy` only
when `str::from_utf8` fails and `masked` only for `is_terminal_hazard`, and
`U+FFFD` is neither: it is Specials, not a control and not
`Default_Ignorable`. So the row painted a name that differed from the name on
disk, and declared itself faithful.

The window already did what a client can do from its side (it flags a row whose
text *contains* a replacement character), and that is a heuristic with a false
positive on a directory genuinely called `caf<U+FFFD>`.

### #282 — an approval anchored to what the daemon had, not to what a human read

`plugin.set_approval` carried `{id, approved}`. The daemon anchors the
capability digest **it** holds at the moment it writes, not the one the human
was looking at when they said yes.

Today that window is closed by *accident* in the daemon — it discovers the
catalogue once at startup, so nothing can change between the `plugin.list` a
human saw and the `set_approval` that confirms. The embedded `Backend` does
NOT have that property: it re-discovers on every call.

The client already compared the capability LIST it had shown. That covers what
is painted. It does not cover what is granted: `category` and `contributions`
— *when* and *how* the plugin fires — are inside the anchor and outside the
list.

## Decision

One bump, three optional fields, each omitted when it has nothing to say.

**`TaskProgress.unreadable: u64`** — how many subtrees or entries the task
could not read. Zero for nearly every task, and `skip_serializing_if` keeps it
off the wire in that case: a `task.progress` travels many times a second per
task. `fs.dir_size` publishes it on every update, so a client paints
*"at least X"* instead of *"X"*.

**`PluginLoadError.dir_bytes: Option<Vec<u8>>`** — the basename's bytes, base64
on the wire like `Volume::label`. Still the basename and never the absolute
path: that would leak the user's home to an agent calling `plugin.list`. The
`dir` string stays, as the fallback for a 0.52 peer and for anyone who only
wants something to paint. With the bytes, the receiver does its own conversion
and *marks* it — which is the standing rule: what gets masked gets said.

**`PluginSetApprovalParams.expected_digest: Option<String>`** and
**`PluginInfo.manifest_digest: Option<String>`** — the anchor travels out with
the catalogue and back with the yes. The daemon refuses if it no longer
matches. It applies **only when approving**: revoking grants nothing, and
refusing a revocation over a stale anchor would keep alive exactly the
permission someone is trying to remove.

## Consequences

The version window shifts to N=0.53.x / N-1=0.52.x, and it shifts for the same
reason in all three cases even though every field is additive: **what a 0.52
peer loses is a check, not correctness.** A count still counts, a broken plugin
directory still has a name, and an approval still lands — what cannot happen
against an old peer is the warning, the mark and the refusal.

**For `expected_digest` that is true by accident, and it is worth writing down.**
A 0.53 client sends the field, a 0.52 daemon ignores it (ADR 0004) and grants
without checking, and *nothing tells the client that its guarantee did not
apply*. What makes that safe today is a property of the old implementation and
not of the protocol: a 0.52 daemon discovers the plugin catalogue once at
startup, so there is no window between the `plugin.list` a human saw and the
`set_approval` that confirms. The embedded `Backend` never had that property,
which is exactly why the check lives there too.

The client cannot currently detect the difference — `RemoteBackend` discards
the `InitializeResult`, so the peer's `protocol_version` is not retained
anywhere. Retaining it, and refusing (or visibly not-verifying) an approval
below 0.53, is the follow-up this record deliberately does not do: it is a
change to the SDK's connection state, not to the wire, and bundling it here
would have made a three-field bump into a fourth change.

`unreadable` is `Option<u64>` and not `u64` for the same family of reason,
made explicit: `None` means *the emitter does not count this* and `Some(0)`
means *it counts and there were none*. A bare `u64` would have made a 0.52
daemon indistinguishable from a clean count, and the fabricated answer is the
dangerous one. It is the rule `Volume::total_bytes` already states for its own
case — never a zero standing in for unknown.

`dir_bytes` is emitted on Unix only. Off Unix there are no raw bytes to send
without going through UTF-16, and sending the lossy form would be *worse than
sending nothing*: the receiver takes `dir_bytes` as raw, so already-converted
bytes come back `lossy = false, masked = false` — an altered name declaring
itself faithful — and they also switch off the string heuristic that is the
only thing marking a Windows lone surrogate today. `None` there keeps the
0.52 behaviour, which is the correct floor. WTF-8 is the right answer and
arrives with the rest of Windows support.

The embedded `Backend` gains the digest check too, and there it is not
belt-and-braces: it re-discovers the catalogue on every call, so it is the path
where the TOCTOU window is genuinely open.

What this does not do: it does not make `fs.dir_size` retry what it could not
read, and it does not tell a reader *which* subtrees were skipped. A count and
a list are different answers; the count is the one the method promises, and
saying it is a floor is what it was missing.
