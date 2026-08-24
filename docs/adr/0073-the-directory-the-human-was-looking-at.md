# 0073 - The directory the human was looking at

- Status: accepted
- Date: 2026-08-24
- Decision makers: Oscar González
- Related: ADR 0072 (the approved directory is the root a leaf hangs from),
  ADR 0071 (an anchor that could not say what it was), ADR 0054 (confined
  writes), ADR 0039 (provider attributes), issues #295, #219, #218, #164.

## Context and problem statement

ADR 0072 confines a leaf transfer: `ops::copy_task` opens its destination
*directory* as a `ConfinedRoot`, so the directory is resolved once instead of
three times plus a retry each, and a substitution **after** that point
redirects nothing.

It closes the race. It does not close what was already there.

A symlink planted at `dest/sub` **before the core ever looks** is, from inside
the core, indistinguishable from a legitimate `~/copias -> /mnt/disco/copias`.
Both resolve elsewhere. The first version of that change compared the opened
root's identity against an `lstat` of the destination path and rejected the
mismatch — which rejects *every* link, and therefore rejects copying to `/tmp`,
`/var` and `/etc` on macOS and to `/bin` and `/lib` on a usrmerge Linux. The
security review caught it before it shipped. `engine_leaf_confined.rs` carries
a test named `un_enlace_ya_plantado_en_el_destino_no_lo_puede_distinguir_el_core`
so nobody reads the file and believes the case is covered.

So the question is not "how does the core tell the two links apart". It cannot,
and no amount of stat-ing will change that. The question is **who can**.

## Decision

The one thing that separates them is the identity that was observed *when the
human approved*: the listing they were looking at. At that moment `dest/sub`
was a directory with a particular inode. That observation lives outside the
core, so it has to travel with the request.

`fs.list` returns a **`dir_anchor`**: the opaque identity of the directory it
just listed. `fs.copy` and `fs.move` accept a **`dest_anchor`** and refuse to
write when the destination directory is no longer that node. Protocol 0.54.0,
both fields optional and omitted when empty.

### The anchor is opaque, and that is not decoration

What identifies a node is `(volume, index)` — device and inode. Putting those
on the wire would tell every client which two paths are the same file and what
inode numbers exist, and clients include agents with a policy scope and
plugins. So what travels is `sha256(process secret || volume || index)`
truncated to 128 bits, rendered as lowercase hex.

Equality survives, which is all the check needs. Forging does not: without the
secret, an anchor for a node you have not been shown cannot be computed. The
secret is drawn once per process from the system CSPRNG, and a failure there is
fatal rather than degraded — a predictable secret would turn the check into
theatre.

An anchor does not survive a daemon restart. A client that reconnects has lost
its listing anyway and asks for it again, so the window that matters — look,
approve, write — falls inside one session.

### Where the comparison happens, and why it differs per path

| path | compared against | window |
| --- | --- | --- |
| leaf (`copy_task`, `move_by_copy`) | `ConfinedRoot::root_id()` — the descriptor already open | none |
| tree (`copy_tree`) | `node_id(to.parent())` by path, before the destination is created | tiny, between asking and creating |

The leaf case is the one #295 is about and it is exact: the root that gets
verified **is** the root that gets written through, so there is nothing to
substitute in between. A tree creates its own destination, so at check time
there is no descriptor to ask; the check is by path and says so in its rustdoc.
It still catches the link that was already planted, which is the case this
exists for.

Unlike ADR 0072's identity check, the anchor comparison **also runs over a
symlinked destination**. That is the whole point: the anchor does not care by
what name the directory was reached, only *which node* it is. A legitimate
`~/copias -> /mnt/disco/copias` anchors to the node behind the link, and
listing it again yields the same anchor.

### The client retains it, not the frontend

`RemoteBackend` remembers the anchor of every directory it lists, capped at 64
(open panes and their recent history — human scale), and `transfer` sends the
one for `to.parent()` automatically. Every frontend gains the check without a
line of code, and a `norte cp` against a hand-typed path sends nothing and
behaves as 0.53 did.

A listing that comes back **without** an anchor erases the remembered one. A
destination that stopped being able to identify nodes, or a reconnection
against a 0.53 daemon, must not leave a stale anchor behind that would make the
next copy refuse itself for no reason.

## Considered alternatives

**Send the raw `NodeId`, as #295 proposed.** Simplest, and what the issue asked
for. Rejected because it leaks inode identity to every client for no gain: the
core only ever compares for equality.

**Reuse the attribute channel (ADR 0039), delivering the anchor as an attr on
`fs.stat`.** Attributes attach to entries, and the directory being listed is
not an entry in its own listing, so the client would need an extra round trip
per navigation to learn what it just listed.

**Refuse any symlinked destination.** Already tried, in the first version of
ADR 0072. It breaks copying to half of macOS.

**Make the check mandatory.** A destination that cannot identify nodes (an
object bucket, an SFTP without extensions) can never produce an anchor, so a
mandatory check would forbid copying there entirely.

## Consequences

- **#295 closes, and with it the remainder of #219** for any client that
  lists its destination through the SDK.
- **What is still not closed** is the substitution of an *intermediate*
  component of the destination path. The anchor names the final directory; the
  components above it are resolved by path when the root is opened. That is the
  same residue ADR 0072 records.
- **The embedded backend does not anchor yet.** A frontend running the core
  in-process calls `Engine::copy_with_as` directly and keeps no listing cache,
  so it sends nothing and behaves as before. `copy_anchored` is public
  precisely so that gap can be closed without another protocol change.
- **An agent does not anchor either** (`norte-mcp`). The anchor says what a
  *human* was looking at when they approved; there is no human listing behind a
  tool call. What bounds an agent is its policy scope, which is a different
  mechanism and still applies.
- **A client cannot tell whether the check ran.** Against a 0.53 daemon there
  is no anchor to return, and the write proceeds unchecked — the same
  degradation ADR 0071 records for `expected_digest`, and the reason the SDK
  keeps the peer's protocol version (#294).
- **A stale anchor refuses a legitimate copy** if the destination directory was
  genuinely replaced (deleted and recreated) since the pane last listed it. The
  fix is the one the user would perform anyway: refresh the pane. The refusal
  is `Conflict { EscapesRoot }`, which frontends already render.
- **Cost**: one `node_id` per listing (already paid by the same provider call
  that answers `stat`), and one `root_id` per leaf transfer, on a descriptor
  that is already open.
