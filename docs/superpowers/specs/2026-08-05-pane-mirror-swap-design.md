# Passing a location between panes — mirror, pull, swap, and going back

- Date: 2026-08-05
- Status: approved
- Related: ADR 0006 (keymap), spec §17 (required product capabilities), the
  2026-07-18 navigation design (history, hotlist, cursor memory)

## Problem

norte has two panes and no way to say "the other one should be here". Preparing
a copy means navigating to the same place twice, or navigating one pane and
then walking the other there by hand. Every orthodox file manager solves this
with two or three keys — Krusader binds `Ctrl+←`/`Ctrl+→` and `Ctrl+U`,
Midnight Commander binds `Alt+i` and `Ctrl+u` — and norte binds none: today the
vocabulary has `pane.switch` (move the focus), `pane.history` and
`pane.hotlist`, and nothing that moves a *location* across.

norte has no tabs; it has exactly two panes (M1). This design is about those
two.

## Vocabulary

Three commands, named relative to the FOCUS rather than to the screen:

| command | effect | default chord |
| --- | --- | --- |
| `pane.mirror` | the unfocused pane goes where this one is | `alt+i` |
| `pane.pull` | this pane goes where the other one is | `alt+u` |
| `pane.swap` | the two panes exchange locations | `ctrl+u` (`alt+s` in the `vim` preset) |
| `nav.back` | this pane returns to where it was before | `alt+left` |
| `nav.forward` | undoes a `nav.back` | `alt+right` |

Focus-relative and not screen-absolute (`send-left`/`send-right`, the Krusader
shape) because a focus-relative command reads the same wherever the focus is,
and that is what lets the help, the palette and the GUI describe it in one
sentence. A screen-absolute pair would need the reader to know which side has
the focus before it could say what the key does.

`ctrl+u` is free in the `orthodox` and `cua` presets. The `vim` preset already
binds it to `cursor.page-up`, which is the vim idiom and outranks a borrowed
one, so `pane.swap` takes `alt+s` there.

Nothing new goes into `norte.toml`. All three are ordinary commands: they enter
`COMMANDS`, so they are rebindable from any keymap layer, they appear in the
command palette, in the help, and in `norte doctor`'s keymap check, and a user
who wants Krusader's exact keys writes three lines in their own layer.

## Semantics

### Mirror and pull are navigation

Both reuse the existing `cd`: the same path `nav.enter` takes, with its history
entry, its per-directory cursor memory (arrive somewhere you have been and the
cursor is where you left it), its watcher registration and its TOFU prompt when
the destination is an unknown host.

Only the LOCATION travels. Marks, quick filter, sort and cursor are not copied:
the destination pane behaves exactly as if the user had navigated there
themselves. Copying marks across would make two panes that look alike and hold
different truths, which is how someone deletes on the wrong side.

One structural change is required, and it is the only one: `cd` today always
acts on `app.focused_mut()`. It needs to take the target pane. Every existing
caller passes the focused pane and behaves as before.

### Swap is not navigation

`pane.swap` exchanges the two panes and touches no disk: no listing is
re-fetched, nothing can fail, and marks, filters, sort and cursor all survive
because the whole pane moves rather than being rebuilt.

The focus stays on the same PHYSICAL SIDE, as in Krusader: someone looking at
the left half keeps looking at the left half, which now holds what the right
half held. Moving the focus with the content would make the command a no-op
from the user's point of view.

Swap pushes no history entry on either pane. Nothing navigated: the panes are
where they were, on the other side. `pane.swap` twice is exactly the identity,
and a history that recorded it would fill each pane's `pane.history` popup with
places the reader never went — the popup lists the directories a pane has been
in, and after two swaps it would list its own current directory twice.

The histories must MOVE with the panes, and this is not free: the history is
not part of `PaneState`, it is `App::history: [History; 2]`, one more array
indexed by pane. Swap has to exchange it explicitly. Leave it behind and each
pane shows the trail of the content that used to be on that side — a popup full
of places the reader never went, offering to take them "back" somewhere they
have never been.

### Going back needs a trail, and the one that exists is not one

`nav.back` cannot be built on what is there. `App::history` is a `VecDeque` of
visited directories with `push`/`remove`/`entries` and no cursor — an MRU list
for the `pane.history` popup, which is a different thing from a trail. Walking
it as if it were a trail gives the classic oscillation: from A go to B, back
lands on A, back again lands on B, and the reader is stuck between two
directories with no way further back.

So `History` grows a real trail alongside the MRU it already keeps, with
browser semantics:

- a `cd` the user asked for pushes the previous directory onto the BACK stack
  and CLEARS the forward stack;
- `nav.back` pops the back stack, pushes the current directory onto the forward
  stack, and navigates;
- `nav.forward` is the mirror image, and exists because without it `nav.back`
  is a trapdoor: one keystroke too many and the only way home is to navigate by
  hand. Five extra lines for the half that makes the other half safe.

Both are ordinary navigation: same `cd`, same cursor memory, same TOFU. What
they must NOT do is feed themselves — a `cd` issued BY `nav.back` must not push
onto the back stack, or back becomes a loop between two directories, which is
the same trap in a new shape. The trail therefore distinguishes navigation the
user initiated from navigation the trail itself replayed.

The MRU stays exactly as it is: `pane.history` keeps listing where the pane has
been, most recent first, with no duplicates. The two answer different
questions — "where have I been?" versus "where was I just now?" — and neither
can be derived from the other.

A `nav.back` with an empty trail does nothing and says so in the status bar,
for the reason the help overlay's `Backspace` closes rather than doing nothing:
a key that silently does nothing is indistinguishable from a broken one.

Both stacks live INSIDE `History`, not beside it. That keeps `App` at one
array per pane for all of this, which is exactly the point the next section
makes.

### The trap: state indexed by pane outside `app.panes`

`app.panes` is not the only place that knows a pane index. So do:

- `App::history: [History; 2]`, the per-pane navigation trail behind
  `pane.history`;
- the in-flight `Fill` — the drain of a paginated listing — which carries the
  pane it is filling;
- `decorate_fetch[2]`, the plugin column fetches;
- `last_probed`, the `(pane, path)` dedup of the on-focus `stat` probe;
- the directory watcher (`watch.rs`), which tracks one directory per pane.

A bare `mem::swap` of `app.panes` leaves one pane's listing draining into the
other and both histories attached to the wrong side. So the swap is: exchange
`panes`, exchange `history`, exchange `decorate_fetch`, flip the pane index of
the live `Fill` if there is one, clear `last_probed` (it is only a cache), and
re-point the watcher.

That is six lines, and omitting any one of them is a bug a green test suite
does not see — the listing keeps arriving, just into the wrong half of the
screen. Each of the six gets its own test.

The list is also a warning about the shape of `App`: every one of these is a
`[T; 2]` that has to be kept in step with `panes` by hand, and swap is the
first operation that reorders them. If a seventh appears, it will be found the
same way this one was — by someone reading for it. A follow-up worth
considering, and out of scope here, is moving these into the pane itself so
there is one thing to exchange rather than six.

## Edges

**Remote destinations.** Mirroring onto an unvisited SSH host connects, and may
raise the TOFU modal or a policy approval, exactly as navigating there would.
This is the case the feature exists for — preparing a local→remote copy without
walking the tree twice — so it is not restricted.

**Archives.** Mirroring inside a `.zip` mirrors the path inside the archive.
The composed scheme is just a location.

**Failure.** If the `cd` fails — gone, refused, connection dead — the other
pane STAYS WHERE IT WAS and the error goes to the status bar. A convenience
gesture may not leave a pane blank.

**Virtual panes.** From a live-search results pane, `mirror` and `pull` are
refused with a message: a list of hits is not a location, and the directory the
search ran under is not on screen, so silently sending the other pane there
would be a jump the reader cannot predict. `swap` DOES work: exchanging two
panes needs neither to be addressable, and moving a result list to the other
side is a legitimate thing to want.

**Both panes already in the same place.** `mirror` and `pull` are silent
no-ops. `swap` still exchanges, because two panes on one directory still differ
in marks, filter, cursor and sort.

## What it drags through the house

Three new commands cost more than their code, and all of it is enforced:

- `help-cmd-pane-mirror`, `-pull`, `-swap`, `help-cmd-nav-back` and
  `-forward` in both locales, or the i18n parity suite fails;
- bindings in the three presets;
- a paragraph in the help corpus, or the documentation gate fails the build —
  its allowlist has a ceiling that can only shrink. The three pane commands
  belong in *Two panes, one destination*, which is the page about exactly this
  relationship; `nav.back`/`nav.forward` belong with `nav.enter`/`nav.parent`
  in *Moving around*, whichever topic currently documents those.

## Testing

- Mirror, pull and swap on the happy path, including that mirror leaves the
  focus where it was and swap leaves it on the same side.
- Back and forward walk a real trail: A→B→C, back twice reaches A, forward
  twice reaches C. The oscillation the MRU would have produced (A→B→A→B) is
  what this test exists to refuse.
- A `cd` the user initiates after going back CLEARS the forward stack.
- A `cd` issued by `nav.back` does not push onto the back stack — the property
  that keeps back from looping between two directories.
- `nav.back` on an empty trail is a no-op that says so.
- The MRU behind `pane.history` is unchanged by any of it: same entries, same
  order, still no duplicates.
- A `cd` that fails leaves the other pane untouched, and says so.
- Mirror and pull are refused from a virtual search pane; swap is not.
- **Swap with a `Fill` in flight** — the test that catches the pane-index bug:
  the batches must keep arriving into the pane that asked for them.
- Swap exchanges `history`: after swapping, each pane's `pane.history` popup
  lists where the CONTENT in front of the reader has been, not where that side
  of the screen has been.
- Swap exchanges `decorate_fetch` and re-points the watcher.
- Mirror onto an unknown host raises the TOFU modal rather than silently
  failing or silently trusting.
- Both panes on one directory: mirror is a no-op, swap still exchanges marks.

## Out of scope

- **Opening the directory under the cursor in the other pane** (Krusader folds
  this into the same key). Deliberately not built: it makes one key mean two
  things depending on what the cursor happens to be on.
- Persisting the trail across sessions. The MRU is not persisted either, and a
  back stack that survives a restart would offer to take the reader back to a
  place they left in another session, possibly on a host that is no longer
  reachable.
- Copying marks, filter or cursor with the location.
- Tabs. norte has two panes; if tabs ever arrive, these commands work per
  visible pane and need no change.
