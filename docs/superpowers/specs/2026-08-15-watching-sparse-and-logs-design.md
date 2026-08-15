# Watching, sparse files, and a log you can hand over: design

> Roadmap post-alpha items **7**, **8** (the half this machine can verify) and
> **9**. Three independent pieces that share one property: every one of them is
> local, touches no wire, and can be proven on the developer's Linux box. Item
> 10 is deliberately not here — it changes the protocol, and it has its own
> spec.

## Why these three, together

They are not related by subsystem. They are related by **what it costs to be
wrong about them**: nothing here is reachable by an agent, none of it can lose
data, and all of it is verifiable end to end without CI. That is the whole
reason they travel in one spec while the daemon handover travels alone.

Two of the three turned out to be much smaller than the roadmap says, and the
roadmap is now wrong on both counts. That is recorded below rather than quietly
worked around.

---

## Item 7 — the graphical frontend watches its directories (#106)

### What exists

`crates/norte-tui/src/watch.rs`, 508 lines with its own tests. A native watcher
(`notify`: inotify / FSEvents / `ReadDirectoryChangesW`) over the `file://`
directories of both panes, a debouncer that coalesces a burst of writes into one
refresh, and a documented fallback to mtime polling when the native watcher
cannot start or `watch()` fails — the inotify-limit pitfall from `CLAUDE.md`,
degraded with a notice rather than failed.

Its surface is four items and none of them mentions a terminal:

```rust
pub struct DirWatch { pub rx: tokio::sync::mpsc::Receiver<()>, /* … */ }
impl DirWatch {
    pub fn new() -> Self;
    pub fn rewatch(&mut self, targets: &[Option<PathBuf>; 2]);
    pub fn take_degraded_notice(&mut self) -> bool;
}
```

The graphical frontend is blind to external changes. The release notes describe
watching as a feature; today it is half true.

### The decision: a shared frontend crate, not the protocol

The architecturally pure answer is to watch in the core and push
`fs.changed` notifications over the wire. Hard rule 7 points that way, and it is
the only design that would ever work for a daemon on another machine or for a
watched `sftp://` pane.

**It is not the right answer today, and the reason is that it would be
speculative.** No client is ever anywhere the socket is not — that is exactly
what roadmap item 10 decided to defer. `notify` only ever watches `file://`
paths, which are on the same machine as any frontend that can open them. Paying
a protocol bump, a wire surface and a `protocol-guardian` review to move working
code behind an interface nobody can use yet is the definition of building for a
requirement that does not exist.

So: `watch.rs` moves to `crates/norte-frontend/src/watch.rs` unchanged, and both
frontends use it. If a remote daemon ever ships, this becomes the local
implementation behind whatever the wire grows, and the move is a move rather
than a rewrite.

`norte-gui` already depends on `norte-frontend`. `notify` is already a workspace
dependency and already used by `norte-config`, so no new third-party code enters
the graph — only a new edge, which hard rule 8 is satisfied by naming.

**One caveat about that crate's charter.** `norte-frontend` is documented as
presentation logic, and a filesystem watcher is not presentation. It goes there
anyway, because the alternative — a crate whose entire contents is one 508-line
file shared by two consumers — buys nothing but a `Cargo.toml`. The module doc
states what it is: frontend infrastructure that is UI-toolkit-independent, in
the crate that already means "shared by the frontends".

### What changes

| file | change |
| --- | --- |
| `crates/norte-frontend/src/watch.rs` | the module, moved verbatim with its tests |
| `crates/norte-frontend/src/lib.rs` | `pub mod watch;` |
| `crates/norte-frontend/Cargo.toml` | `notify.workspace = true` |
| `crates/norte-tui/src/lib.rs`, `main.rs` | the module declaration goes; two `use` sites re-point |
| `crates/norte-tui/Cargo.toml` | `notify` drops if nothing else in the crate uses it |
| `crates/norte-gui/src/main.rs` | owns a `DirWatch`, re-watches on `cd`, refreshes on the channel |

### How the graphical frontend consumes it

The terminal frontend selects on `dir_watch.rx.recv()` in its run loop. GPUI has
no such loop: async reaches the UI through `cx.spawn` plus `this.update` plus
`cx.notify()`, and the window already runs one long-lived spawn for the session
event pump (`main.rs:1444`).

The watcher plugs in the same way, and this is the part that matters: **the
event triggers the refresh path that already exists**, `refresh_dir(pane, dir)`,
not a new one. That function re-lists the directory a pane is already in without
`begin_loading`, so marks survive and are pruned by `refill` — which is exactly
the behaviour the terminal side gets, and exactly what a refresh caused by
somebody else's write has to do. It also feeds the existing `relist_dirs`
coalescing (#84), so a burst that survives the debouncer still cannot send one
list per event.

`rewatch` is called with both panes' directories whenever either changes,
mirroring the terminal's per-loop-iteration call. The degraded notice goes to
the status bar the first time, once, like its counterpart.

### Scope, stated so it is not mistaken for more

- **Local, non-virtual panes only.** An `sftp://`, `s3://` or archive directory
  has no inotify; its refresh stays manual. Unchanged from the terminal half.
- **Degraded mode polls the directory's mtime.** Creating, deleting and renaming
  inside it is visible; writing into an existing file is not, because that does
  not change the parent's mtime. The status line says so.
- **The degradation is a session latch**, one-way. A transient inotify exhaustion
  leaves polling on until restart. Simplicity over hysteresis, as decided for v1.

### Testing

The moved tests come with the module and must pass unchanged in their new
crate — that is the assertion that the move is a move. The graphical side gets a
test that a channel event reaches `refresh_dir` for both panes, in
`norte-gui`'s own suite (`just gui-ci`, since that crate is outside the
workspace).

**Closes #106.**

---

## Item 8 — sparse files

### The roadmap is stale here, and by a lot

It lists five things: explicit symlink policy, cycle detection, sparse files,
Windows reparse points, bounded retry for locked files.

**The first two are done.** `SymlinkPolicy` is `Skip`/`Preserve`/`Follow` on the
wire and honoured by the copy engine, and `ops::plan_for` routes a `Follow` walk
through `walk_following`, which expands directory symlinks carrying the chain of
ancestor `NodeId`s and refuses a cycle (§17.9, issue #19). Verified by reading
`ops.rs:2122`; nothing to build.

**The last two are Windows**, and this machine cannot run a single line of
either. Writing unverifiable platform code is how #25 and #33 came to be: two
open issues nobody can close. They are filed as issues blocked on CI rather than
written blind.

That leaves sparse files, which ext4 supports and this machine can prove.

### The design: the destination decides

A hole is not in the byte stream. `Provider::read` yields `Bytes`, and a region
that was never written reads back as zeros — indistinguishable, by construction,
from zeros somebody wrote on purpose. Teaching the stream about holes means a
new shape for `ByteStream` and a wire concept for every provider that has none.

So the source is left alone and **`norte-vfs-local`'s sink decides**: a chunk
that is entirely zeros is not written, the file position is advanced past it, and
`commit` fixes the final length with `set_len` so a hole at the tail exists
rather than being lost. Everything else about the sink — the `.norte-partial`
staging, the publish, `partial_digest` — is untouched.

Three consequences worth stating:

- **It works regardless of the source.** Copying a fully-allocated file full of
  zeros produces a sparse destination. That is a feature, not a side effect: the
  bytes read back identical and the disk keeps the difference.
- **It does not save read I/O.** The source is still read in full. Saving that
  needs `SEEK_HOLE`/`SEEK_DATA` on the read side, which is the interface change
  above. Named here so nobody reads this as "sparse copying is done".
- **A filesystem without holes stays correct.** The seek-past-zeros writes
  nothing; the `set_len` allocates. Same bytes, no saving.

The zero scan costs memory bandwidth per chunk, against disk I/O saved per hole.
On a VM image — the case the roadmap names — that is not a close call.

### Testing

Create a file with a 64 MiB hole (`set_len` past a small write), copy it, then
assert two things: the destination's bytes are identical to the source's, and its
`st_blocks` is a small fraction of its `st_len`. The first assertion is the one
that matters; the second is the only proof the optimisation happened at all.

Plus a hostile case: a file whose *content* is legitimately zeros in the middle,
copied and compared byte for byte. It must be identical, and it is allowed to be
sparse.

### What gets filed instead of built

Two issues, both labelled as blocked on CI returning:

- Windows reparse points: junctions and mount points are not symlinks and today
  are neither followed nor refused deliberately.
- Bounded retry for a locked file: `ERROR_SHARING_VIOLATION` is what makes
  Windows usable at all, and is untestable here.

---

## Item 9 — a log the user can hand over

### What exists

`crates/norte-core/src/logging.rs`, 83 lines. An `EnvFilter` defaulting to
`INFO`, honouring `RUST_LOG`, with one non-negotiable trailing directive that
caps `suppaftp` at `info` — because that crate logs `PASS <password>` at TRACE
and hard rule 10 says a secret never reaches a sink. The subscriber writes to
stderr.

`#[instrument]` is already on the effectful core functions, so the events exist.
There is nowhere for them to go.

**And for two of the three frontends there is not even a subscriber.** Only
`norte-cli` calls `logging::init` (`main.rs:627`). The terminal frontend
deliberately installs none, and says why at `main.rs:2218`: a `fmt` layer on
stderr would fight the alternate screen, so a `tracing::warn!` from inside the
terminal frontend is discarded silently, today, everywhere. The graphical
frontend installs none either. The two frontends a user actually runs produce no
diagnostics at all.

### The design

A second layer on the same registry: `tracing-appender`'s rolling file
appender, daily rotation, bounded retention by file count, writing to
`<state_dir>/logs/`. New dependency, justified per hard rule 8: it is a tokio
project, it is the appender `tracing-subscriber` is designed around, and the
alternative is hand-rolling rotation and retention — file naming, pruning, and
the writer's non-blocking guard — which is more code than the feature.

**`logging` gains a second entry point, and this is what unblocks the
frontends:**

| function | layers | who calls it |
| --- | --- | --- |
| `init()` | stderr + file | `norte-cli`, the daemon — unchanged for them except that the file appears |
| `init_to_file()` | file only | the terminal and graphical frontends |

Both build the identical `EnvFilter`, cap included. The frontends get
diagnostics for the first time without a single byte reaching a screen they are
drawing on, and the comment at `norte-tui/src/main.rs:2218` — which currently
explains why a `warn!` there is thrown away — stops being true and gets deleted
rather than left to mislead.

**The security cap comes along for free, and that is the point of putting it on
the registry rather than the layer.** `EnvFilter` filters events before any
layer sees them, so `suppaftp=info` covers the file exactly as it covers stderr.
The existing test that proves the cap is extended to assert it on the file layer
too — because "it should follow from the architecture" is not evidence.

**`state_dir` becomes shared.** It exists today as a private function in
`norte-tui/src/main.rs:7634`, resolving `$XDG_STATE_HOME/norte`,
`~/.local/state/norte`, or `%LOCALAPPDATA%\norte\state`. It moves to
`norte-config::dirs` next to `config_dir`, with the injectable-environment test
seam that module already uses for `user_config_dir_on` — which is what lets the
Windows branch be pinned by a suite that only ever runs on Linux.

`None` (a bare environment with no `HOME`) keeps degrading: no file layer, stderr
only, one warning. Logging that fails to start must never stop the program.

**Configuration** in `norte.toml`, minimal:

| key | meaning |
| --- | --- |
| `[log] dir` | override the directory; default `<state_dir>/logs` |
| `[log] retain` | how many rotated files to keep |

No level key: `RUST_LOG` already does that, and a second mechanism for one
setting is how they drift apart.

**`norte doctor` gains a `logs` section**: where the file is, how much it
occupies, and whether the directory is writable. That is the line that turns
this from "logs exist" into "somebody who is not us can file a bug report" —
`doctor` is already where a user is told to look, and its findings already carry
stable machine-readable codes.

### Redaction, and what this does not promise

Paths in core spans already go through `engine::span_path`, which redacts. The
`suppaftp` cap is enforced. Beyond those two, **this design does not audit every
existing log line** — it gives them a destination. A line that leaks today leaks
into a file tomorrow instead of a terminal, which is worse in exactly one way:
it persists. That is stated here so the security review of this work knows what
its job is, and it is the reason `norte doctor` reports the log's *location*
rather than offering to upload it anywhere.

A bundle command that packs logs, versions and redacted config for a bug report
was considered and deliberately deferred: it is the useful thing, and it needs
its own pass over what "redacted" means with a security reviewer present.

### Testing

- The appender writes a file under the resolved directory, and the events land
  in it.
- Retention prunes to `retain`.
- The `suppaftp` cap holds on the file layer (the existing collector test,
  extended).
- `state_dir` resolves correctly for each platform branch through the injected
  environment, including the empty-variable cases that `user_config_dir_on`
  already treats as absent.
- `norte doctor` reports the `logs` section, and reports it as a warning rather
  than an error when the directory cannot be written — a machine with no state
  directory is degraded, not broken.
- A `tracing::warn!` raised from inside the terminal frontend lands in the file
  and **nothing lands on stderr**. Both halves are the assertion: the first is
  the feature, the second is why the frontend refused a subscriber until now.

---

## What this spec does not do

- **The wire.** Nothing here changes `norte-proto`. No version bump, no
  `protocol-guardian`.
- **Watching over the protocol.** Reasoned above; revisit if a remote daemon
  ever ships.
- **Sparse on the read side.** Needs `ByteStream` to carry holes.
- **Windows reparse points and locked-file retry.** Issues, blocked on CI.
- **A bug-report bundle.** Deferred with its reason.
- **An audit of existing log lines.** Named as the security review's job here.

## Order

7, then 8, then 9. They are independent, so the order is by risk: the move is
mechanical and proves itself by its own tests passing in a new crate; sparse
files touch the write path that every copy in the product goes through, and want
the freshest attention; logging is additive and touches nothing that already
works.
