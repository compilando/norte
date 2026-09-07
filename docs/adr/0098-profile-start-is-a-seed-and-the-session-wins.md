# 0098 — `[profile.start]` is a seed in wire form, and the session wins

- Status: accepted
- Date: 2026-09-07
- Decision makers: Oscar González
- Related: ADR 0079 (a profile declares, it does not execute), ADR 0059
  (the session document), ADR 0077 (a decision taken once, in
  `norte-frontend`), ADR 0097 (parity is a test), spec 2026-08-26 D3

## Context

`[profile.start]` says where each slot of a profile opens. Both frontends
*write* it — `save_profile` records the directory of every live slot — and
two files promise it makes a freshly saved profile useful.

**No one read it.** The key was parsed into `CommonConfig::profile_start` and
then destructured away as `profile_start: _` in the terminal and never
mentioned in the window. Entering a profile left both panels wherever they
were, so a profile changed the colours and the keymap and nothing else. It was
found by the parity audit of 2026-09-05, as one of the configuration keys that
only one frontend honours — except this one was honoured by neither.

Because nothing read it, nothing noticed that **the two ends disagreed about
its type**. `profile_snapshot` writes `VPath::to_wire()` (`file:///home/u/src`,
`sftp://host/srv`); the loader parsed it as a `std::path::PathBuf` and its
rustdoc explained, at length, how `~` and relative paths were left unexpanded
for the caller to resolve. A whole paragraph of reasoning about a value shape
that never occurs.

## Decision

1. **The values are `VPath`s in wire form.** Not native paths. A slot of a
   profile may sit on sftp or inside a container, and a `PathBuf` cannot say
   so; the writer has always emitted wire form. This also deletes the question
   of what a `~` or a relative path resolves against — a profile is used
   across machines and across days, and "wherever you launched it from" is not
   an answer to give a workspace.

2. **The session wins, and "the first time" is enforced twice.** In the seed
   order `[profile.start]` sits between the session and the command line: what
   you were doing beats what the profile declares, and what a human just typed
   beats both. A profile is a workspace, not a bookmark that drags you back to
   the start whenever you enter it — that would make it useless precisely for
   whoever uses it daily.

   Two vetoes, and both are needed. The first is the session **as read from
   disk** — not the screen as it stands, which names every live slot and would
   veto everything. The second is the set of slots **this process has already
   seeded**: on a machine with no session document the first veto is empty
   forever, so without the second, entering and leaving a profile would yank
   the reader back on every round trip. That is this ADR's own rejected
   alternative arriving through a different door.

3. **Which slots are seeded is decided once**, in
   `norte_frontend::config::profile_start_seeds`: the slots the profile names
   that neither veto rules out. Each frontend then places them with its own
   machinery. This is the ADR 0077 shape, and it is what keeps the two from
   drifting the way the writer and the reader of this key already had.

   Sharing the decision is not enough on its own, and the review of this
   branch is the proof: both frontends called that function correctly and both
   called it in the wrong place — the terminal seeded only on a profile
   switch, with a veto set that named every live slot, so it never seeded at
   all; the window seeded from inside its session-parsing ladder, below four
   early returns, three of which are the fresh-install case this key exists
   for. Each frontend's own tests were green. **The parity case is what
   catches this class**, which is ADR 0097 restated: a shared function proves
   the decision is single, not that both surfaces reach it.

   Only slots the LAYOUT places are seeded. Not "slots the pane store knows":
   that store keeps orphans for when their layout comes back, and inserting
   revives them — seeding there would replace a remembered pane with the
   profile's start directory, which the store explicitly promises not to lose.

4. **Seeding is not a step the reader took.** It does not enter the trail:
   a "back" that leads to the previous profile's directory offers a return to
   somewhere you never came from. `norte_frontend::nav::Trail` gains a `Seed`
   variant so the two frontends can *say* this — the terminal seeds by
   building a pane from scratch and never had a trail entry, while the window
   goes through `navegar_hueco`, which records. Without a word for it, the two
   surfaces ended up with different histories.

5. **What does not parse is dropped and said.** A key that is not a slot id, a
   value that is not a `VPath`: the line is skipped rather than refusing to
   start, because the file is the reader's and a typo does not earn a refusal
   to run. But the count reaches the screen and each reason reaches the log,
   in both frontends, at startup and on every profile switch. Without that,
   the strictness of decision 1 would be a trap: someone writes `/tmp`, the
   slot opens wherever it likes, and nothing says why.

   `profile_warnings` existed and was displayed by nobody. That was the second
   half of the same defect.

## Consequences

- A profile saved on one machine and copied to another opens its slots where
  it says, which is the feature the two promises described.
- A hand-written `[profile.start] 1 = "/tmp"` is now rejected — with a visible
  message. Nothing depended on the old shape, because nothing read it.
- The value is not a path any more, so no `$HOME` of this process is baked
  into anything the daemon might read. The old rustdoc's concern is satisfied
  by not having the problem.
- `Trail::Seed` is a third variant on a widely constructed enum. It changes no
  existing behaviour: `step()` answers `None` for it, like `Record`.

## Alternatives considered

- **Accept a bare absolute path as `file://`.** Friendlier to a hand-written
  file, and it was the shape the old rustdoc implied. Rejected because it is a
  second syntax for the same field, it does not survive Windows drive letters
  without dragging path logic into `norte-config`, and percent-encoding makes
  the naive string concatenation wrong for any name with a space or a `%`. The
  visible warning covers the reader who tries it.
- **Re-apply the seed on every configuration reload.** Rejected: it would pull
  you back to the profile's starting directory every time the file is touched,
  which is decision 2 inverted.
- **Delete the key and the two promises.** The plan offered this as the other
  half of the choice. Rejected because the feature is real, the writer already
  produces the data, and the cost was one shared function.
