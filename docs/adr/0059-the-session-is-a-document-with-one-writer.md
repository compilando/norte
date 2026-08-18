# 0059 - The session is a document with one writer

- Status: accepted
- Date: 2026-08-18
- Decision makers: Oscar González
- Related: ADR 0058 (a screen is a tree the core keeps and does not read), ADR
  0055 (daemon handover), ADR 0004/0005 (wire tolerance), the design
  (`docs/superpowers/specs/2026-08-18-layout-presets-and-ui-session-design.md`,
  phase B), protocol 0.48.0, hard rules 2, 6 and 7.

## Context and problem statement

ADR 0058 made the screen a tree and phase A made five of them loadable by name.
What none of that does is survive a restart. Every reader who closes norte —
or whose daemon is replaced under them (ADR 0055) — comes back to the layout
their configuration names, at the directory their shell was in, with an empty
history and the cursor on the first row.

Keeping that state raises four questions, and they have to be answered
together:

1. **Where does it live?** In the client that produced it, or in the core?
2. **What shape does it cross the wire in?** The screen is made of `Node`,
   `SortSpec` and `ColumnId`, which live in `norte-frontend` — a crate that
   depends on `norte-proto` and not the other way round.
3. **Who may write it?** Two windows over one state is two writers.
4. **What stops it growing?** History is unbounded, and slots outlive the
   layouts that named them.

## Decision

**The core stores the session; it does not read it.** `session.get` and
`session.put` (protocol 0.48.0) carry a document of three fields: `version`,
`revision` and an opaque `body`. The core versions it, refuses it over a
ceiling, hands it back and writes it to disk. What is inside the body belongs
to `norte-frontend`, whose `SessionBody` is its v1 schema.

The body is opaque for the reason ADR 0058 already gave one process over:
mirroring `Node`, `SortSpec` and `ColumnId` into `norte-proto` would duplicate
four types across a dependency edge and turn every new UI field into a wire
change with its own bump and its own golden. Adding a field to the body raises
`norte_frontend::session::SCHEMA_VERSION`, not `PROTOCOL_VERSION`.

**Concurrency is one number.** `revision` rises on every accepted `put`; a
`put` that carries a stale one is refused with `Conflict { stale_revision }`
and the client re-reads. It is not there for simultaneous editors — there are
none — but for the client that reconnects after a handover holding state from
before.

**Ownership is a property of the channel, not of the document.** The first
human connection to call `session.get` keeps it; later ones get the same copy,
are told `owner: false`, and run detached: same screen, then their own way,
never writing. Opening a second window gives the reader what they expected and
there are never two writers over one state. `owner` rides in the GET result and
not inside the session, because who may write is about the connection, not
about the screen. Agents are refused outright: an agent session has no screen
to keep.

**Between processes the same question is a `flock`** on a `session.json.lock`
sibling — never on the file itself, which is replaced by rename. It is a
try-lock: whoever loses runs detached, because waiting would hang a start
behind a core that is alive and does not intend to let go. A frontend without a
daemon takes the same lock over the same file, so an embedded window and a
daemon do not overwrite each other either.

**The caps are the client's, and they live in the type.** History is 64
entries per slot and direction, orphan slots are 128, and an orphan untouched
for thirty days is swept. A slot some layout mentions is never swept — what is
on screen is not recycled. The core enforces exactly one thing, because it is
the only thing it can enforce honestly about a document it does not read: a
serialised body over 1 MiB is refused whole, with
`LimitExceeded { session-body }`, and the stored session stays exactly as it
was. Truncating a document whose schema you do not know is worse than refusing
it.

**There is no notification.** The core does not tell anybody the session
changed, because the only client that may write it is the one that changed it,
and every other client is detached by definition. A `session.changed` would
have no correct recipient.

**On disk it is the wire document**, at `<state_dir>/session.json`, written to
a temporary file and renamed with the `sync_all` before the rename. No envelope:
the only thing anyone needs to know about the file is which body version it
carries, and that already travels inside. A file from a newer version is
neither read nor overwritten — losing the session a newer binary wrote does not
come back, and respecting it costs one start from configuration. The file is
0600 from the `open` and the directory 0700, because a session is the list of
paths its reader walks. A corrupt file is a diagnosis and a start from
configuration, never a blank screen — and the diagnosis is category and
position, never the content, for the same reason.

**It is written on quiet, on handover, and on the last goodbye.** One writer
task coalesces on a one-second tick — the cursor moves on every arrow key and
this is a file, not a database — the last connection to leave nudges it early,
and the shutdown path flushes before the socket is withdrawn, because
withdrawing the path is what lets a successor start.

## Consequences

- A reader gets their screen back: arrangement, directories, cursors, history,
  sort and hidden flag, across a daemon that was replaced under them.
- A second window is honest about being a copy, and cannot cost the first one
  its state.
- The protocol carries UI state without knowing any of it. The wire freezes
  that the body travels intact, not what is inside it, so the layout line can
  keep moving without a bump.
- A 0.47 client does not know `session.*`: it starts without the screen it left
  and never writes one. It does not break — it silently loses exactly what this
  phase exists to keep, which is why the compatibility window moved.
- The core cannot help a client that writes nonsense into the body. It can only
  refuse the size, and it does.
- Two numbers to keep straight instead of one: `PROTOCOL_VERSION` for the
  envelope and `SCHEMA_VERSION` for the body. That is the price of the opacity,
  and it is paid once per field added rather than once per release.

## Alternatives considered

- **Typed session in `norte-proto`.** Rejected: it inverts the dependency
  (`Node` and friends live in `norte-frontend`) or duplicates four types across
  it, and it makes every UI field a wire change. ADR 0058 already decided this
  same question inside one process.
- **The client writes its own file.** Rejected: a handover leaves nobody to
  write it, three frontends would produce three formats, and the "one writer"
  question would still have to be answered — with no shared place to answer it.
- **A family of per-field methods** (`session.set_cursor`, …). Rejected:
  fifteen methods, fifteen goldens and a merge engine nobody asked for, to
  avoid sending a kilobyte the client already has in hand.
- **Blocking lock instead of try-lock.** Rejected: it hangs a start behind a
  live core. Detached is a worse screen than owning it and a much better one
  than no screen at all.
- **Truncating an oversized body.** Rejected: the core does not know the schema,
  so it cannot know what it just deleted. Refusing whole leaves the client to
  drop what it knows it can afford — history first.
- **A `session.changed` notification.** Rejected: no correct recipient, since
  every reader other than the writer is detached on purpose.
