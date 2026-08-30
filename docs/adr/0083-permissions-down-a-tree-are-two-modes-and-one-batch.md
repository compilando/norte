# 0083 - Permissions down a tree are two modes and one batch

- Status: accepted
- Date: 2026-08-30
- Decision makers: Oscar González
- Related: ADR 0004 (wire compatibility), ADR 0009 (long operations are Tasks),
  ADR 0081 (permissions are a mutation), issues #315, #121.

## Context and problem statement

ADR 0081 gave norte `fs.set_mode` and deliberately left one hole: it changes
EXACTLY the paths it is given. A folder changes its own mode and not that of
what it contains. Krusader offers "apply to subfolders" from its properties
dialog, so the hole is real — and it was left open because four things had to
be decided first, none of them about the syscall.

## Decision

**Protocol 0.62.0**: `FsSetModeParams` gains `recursive` and `dir_mode`.

### A directory does not get the file mode

`chmod -R 644` over a tree makes it unusable: without the execute bit you
cannot even enter a directory. The three known ways out were two modes, the
`X` of `chmod`, and one mode for everything with the foot-gun documented.

`dir_mode` is optional and, when absent, everything gets the same mode — which
is what `chmod -R` does, and what breaks trees. The escape hatch is explicit
rather than clever: `X` is POSIX's right answer and needs SYMBOLIC modes, which
this protocol does not have — the mode travels as a number, on purpose (the
text is the business of whoever paints it).

The terminal takes it in the field it already had, with `chmod`'s own grammar:
`755`, `-R 755`, `-R 644,755`. No new key inside a field where every key is
text, and no invented vocabulary: `-R` is what the person who knows what they
want already types. A dir mode WITHOUT `-R` is an error rather than a value
that gets ignored — asking for something that will not happen deserves an
answer.

### The question the human answers carries the SCOPE

`PolicyOp::SetMode` and `ApprovalDetail` gain `recursive` and `dir_mode` in the
same bump, and this is not decoration. A recursive change over one root arrives
with `paths_total = 1`: the approval said "set-mode over 1 path" while what was
being approved was every descendant, and the folder mode did not appear at all.
That is the exact hole `ApprovalDetail::mode` closed in 0.61 — "a human who
approves without seeing the scope is not consenting to what they think" — one
size larger, and reopening it silently would have made 0.61 pointless.

Both frontends paint it, and they say it in the loudest form the surface has:
*and EVERYTHING inside it*.

### The cap is on nodes VISITED, and what is left over is said

The existing cap (4096) is on the paths REQUESTED; a tree is millions. Counting
first to refuse would walk it twice, and truncating silently leaves half a
selection changed — which is exactly what #311 and #314 refuse everywhere else.

So the walk stops at `SET_MODE_RECURSIVE_MAX` and what it did not reach is
reported — **in a field of its own**, `TaskProgress::unvisited`, and not folded
into `unreadable`. They both mean "this did not happen" and they are not the
same thing: an unreadable node is a permission or a file that moved, things the
reader fixes, and this is norte saying the tree is bigger than it walks at once.
Mixing them made a huge tree report "40 000 could not be changed (a symlink, or
not yours)", which is not what happened, and `unreadable` has carried its own
contract since 0.53 — a counter that meant two things depending on the task
could not be read by anybody.

The walk happens BEFORE anything is touched, so `entries_total` is the real
number from the first progress snapshot rather than a figure that climbs while
the reader watches. A node whose `stat` fails is counted as unvisited and
skipped rather than `chmod`'d blind: without the stat there is no way to know it
is a symlink, and `chmod(2)` follows those.

Top-down order, and it matters: taking the execute bit off a directory before
walking into it would strip the rest of the tree from itself. With `dir_mode`
at `755` that does not arise; with one mode for everything it does, and that is
the foot-gun the documentation names. Since the whole tree is enumerated before
the first `chmod`, the tree in THIS batch is finished either way.

A symlink inside the tree is not followed and not touched, the same as outside
one: `chmod(2)` would follow it, so the mode saved as the reversal would be the
link's — and the target may be outside the scope somebody approved.

### One action is one batch

A hundred thousand journal entries that nobody can join back together read as a
hundred thousand actions where the human did one. So the entries of a recursive
`set_mode` share a `batch_id`, the way a batch rename's do.

**Undoing that batch is NOT "all or nothing"**, and that is the difference from
a rename batch. Modes are independent — none depends on another having gone
back first — so a tree of a hundred thousand files where one cannot be touched
goes back except that one, which is what the reader wants. A rename batch
cannot afford that: half a permutation undone is the state that machinery
exists to prevent.

### The same version carries `ai.rename_plan`'s `names` (#121)

Not thrift: it is the same kind of change. `AiRenamePlanParams` gains `names`,
the basenames the plan is asked about, empty meaning the whole directory. With
first-class selection, marking five files and asking for a plan sent the
directory's thousand names to the provider — more than the human pointed at,
and the AI gate exists precisely to bound what leaves the machine.

Names and not paths: a plan is about one directory, and a path here would open
the door to asking for a plan about something in another. A name that is not in
the listing is ignored rather than refusing the whole plan — between marking
and asking, a file can be gone, and punishing the reader for that race fixes
nothing.

## Consequences

- A client one version behind loses only SCOPE: it changes permissions on the
  exact paths (0.61's behaviour, which is what it already expects) and asks for
  plans about the whole directory. No check stops happening.
- A recursive `set_mode` over a very large tree stops at the cap, and the task
  reports how many nodes it never reached. It is not an error: the ones it did
  reach are changed.
- The window does not offer recursion yet (#315 covers the terminal); its
  dialog sends `recursive: false` explicitly, so the day the checkbox appears
  nobody has to hunt for where the decision was made.

## Alternatives considered

- **Symbolic modes (`a+rX`).** The right POSIX answer, and it needs a symbolic
  grammar on the wire, in the core and in two dialogs. That is its own change,
  larger than this one, and it can be added later without taking `dir_mode`
  away: `X` is a rule for DERIVING two modes, and this is the pair it derives.
- **Refusing above a cap by counting first.** Two walks over a tree that can
  take minutes, to answer a question the reader would rather have answered
  with work done.
- **Declaring a recursive `set_mode` irreversible.** Cheap and honest, and
  wrong for the case that matters: a permission change over a tree is exactly
  what one wants to undo.
