# 0044 - A count repeats the dispatch, and two keys are not for sale

- Status: accepted
- Date: 2026-08-09
- Decision makers: Oscar González
- Related: specification section 12 (keymap, sacred keys); ADR 0006 (keymap
  resolution semantics — **extended**, see decision 8), ADR 0043 (keymap
  availability and the `mod+` alias — **extended**, see decisions 2 and 9).
  Design: `docs/superpowers/specs/2026-08-09-keymap-catalogue-and-presets-design.md`;
  plan: `docs/superpowers/plans/2026-08-09-k2a-counts-and-sacred-keys.md`.

## Context

K2b will ship faithful Total Commander, Krusader, Norton Commander and Far
presets. Two of the four originals have things norte's keymap engine cannot
express, and a preset written against an engine that is still moving is a
preset that gets rewritten twice.

**A numeric prefix.** vim and Far both multiply the next command by a typed
number: `5j` moves down five rows. Nothing in the engine accumulates digits,
and nothing in either frontend repeats anything. Without it, `vim.toml` is a
keymap that looks like vim and does not behave like it — which is the trap ADR
0043 exists to avoid, arriving through a different door.

**`Tab`.** Specification §12 reserves it for pane switching. ADR 0043 recorded
that the rule was **not implemented**, on the grounds that it had no violator
until the first imported preset existed and "a prohibition with nothing to
prohibit is how a rule ends up untested". K2b is that preset — four of them —
so the rule is due.

Two debts from K1 are settled here for the same reason: they are both in code
K2b's presets are the first real load for. `build_effectives_with` — the GUI's
*error-recovery* path — still `expect`ed, and ADR 0043 decision 4 had given it a
live way to fail (an unavailable binding now takes part in the prefix-free
check). And `means_command`, which runs on every key event, rendered every
binding to a `String` and re-parsed it: 115-134 allocations per keystroke today,
roughly triple that once four more presets land.

## Options considered

**A. The count reaches the command.** `dispatch(cmd, count)`, and each command
decides what a count means to it. Most faithful to what a count *is*. Rejected:
~80 commands change signature, and a command that forgets to read its count is
a key that silently does one thing when the user asked for five. That is
exactly the silence K1 spent five commits removing — a keymap whose failure mode
is "nothing visible happened". The compiler cannot help, because ignoring a
parameter is legal.

**B. The count repeats the dispatch, and the frontend is what repeats.**
Chosen. No command's signature changes, no command can forget, and the set of
commands for which repeating is *meaningful* is already declared in one place
(ADR 0043 decision 3 put `counts: bool` in the catalogue, unread, for exactly
this).

**C. A count is a resolver-internal multiplier applied to a movement delta.**
The narrowest version: only cursor moves take counts, and the resolver hands the
frontend a delta instead of a command. Rejected: it privileges one command
family in a type that is supposed to be command-neutral, and it cannot express
`Ignored` — the count would simply not exist for anything else, which is
silence again.

For the two load rules: **resolve the conflict silently by precedence** (the
count wins over the digit binding, or vice versa) was rejected on the same
ground ADR 0006 rejected ambiguous prefixes — a keymap that quietly picks for
you is a keymap you cannot predict. **Warn at load and continue** was rejected
because nothing reads warnings; `norte doctor` would report it and the user
would never run `norte doctor`.

## Decision

### 1. The count rides with the command; the frontend repeats the dispatch

`Resolution::Run` is a struct variant `{ command: String, count: Count }`, and
`Count` is `None | Repeat(u32) | Ignored(u32)`. The resolver accumulates; the
frontend loops.

This is option B, and its cost is real: the repeat lives in each frontend, so
there are two loops to keep honest instead of one. That is paid for by what it
buys — a command cannot be wrong about counts, because a command never sees
one.

### 2. The catalogue decides who takes a count, and a count over a command that takes none is `Ignored`, never swallowed

`CommandDef.counts` (ADR 0043 decision 3, recorded unread) is the authority.
Ten entries carry `counts: true`: `cursor.{up,down,page-up,page-down}`,
`nav.{back,forward}` and `viewer.{up,down,page-up,page-down}` — clamped,
in-memory, relative movers, all of them. A test pins that set **by name**, so
that adding an eleventh is a decision made in review rather than discovered by
a user who typed a number: `counts: true` is a licence to run something 9 999
times from one keystroke.

The four `dialog.*` movers are the shape that would take a count and declare
`counts: false` anyway. No overlay dispatcher honours one: every overlay
resolves against the dialog screen and then resets the resolver on
`Resolution::Counting` — the same decision that gives overlays no multi-key
sequences either — so a count typed over a dialog is destroyed at the digit and
can never reach the command. Declaring `true` there would be the catalogue
claiming a capability nothing implements, which is exactly the drift the shared
catalogue exists to end. It is debt, recorded as such, and the flag flips the
day an overlay learns to repeat.

`3q` does not quit three times and it does not quit silently either: it quits
once and says *"app.quit does not take a count (3 ignored)"*. The sentence lives
in `norte-frontend` next to `unavailable_message`, so the two frontends cannot
word it differently, and it is set **before** the dispatch so that a command
with something of its own to say overwrites it rather than being overwritten.

A `lua:` command is not in the catalogue and can never be (the Lua registry is
populated at run time, ADR 0043 decision 2). A count over one is therefore
`Ignored` — honestly, because norte genuinely cannot know what repeating it
would mean. The Lua branch runs once and never enters the loop.

### 3. Repeating a dispatch is not the same thing as a count, and K2b must not pretend it is

Found while writing the tests, and recorded because it is the one place this
design is weaker than the originals it imitates.

vim's `12gg` means **go to line 12**. Repeating the dispatch twelve times cannot
produce it: twelve "go to the top" is still the top. The catalogue is therefore
*right* to declare `cursor.top` as `counts: false`, and `12gg` in norte is
honestly `Ignored(12)` — the cursor goes to the top and the user is told the
twelve did nothing.

A real `12gg` needs a **new command that takes a line number**
(`cursor.goto-line`), not a flag flip. **K2b must not try to buy it with
`counts = true`**: flipping the flag would turn an honest "the twelve was
ignored" into twelve real dispatches that land in the same place, which is the
same wrong answer with the diagnostic removed.

The general shape: repeating buys anything *relative* (move by one, N times)
and buys nothing *absolute* (go to position N). Every `counts: true` entry today
is relative.

### 4. Zero never opens a count, but it does accumulate

A count never starts with zero, so `0` stays a bindable key — which is what
vim's "first column" and mc's mask keys rely on. Once a count is open, `0`
accumulates normally: `1` then `0` is ten.

### 5. Four digits is the ceiling, and the fifth is dropped

`MAX_COUNT_DIGITS = 4`, `MAX_COUNT = 9_999`, the second derived from the first
so they cannot disagree. 9 999 repetitions of a cursor move on a listing of any
realistic size lands on the last row, so the ceiling costs nothing a user wants.

The fifth digit is **dropped**, not wrapped and not modulo'd: `99999` becomes
`9999`, never `9999` × 10 + 9 as a `u32` that silently became a number nobody
typed. A count the user cannot predict is worse than a count they cannot type.

### 6. Counts are opt-in per preset, and forbidden in a user layer

`counts = true` is a top-level key of a preset file. `vim.toml` sets it;
`orthodox` and `cua` do not, and turning it on there would steal their digit
keys. (`far.toml` sets it in K2b, when the file exists.)

A **user layer setting it is a load error** (`WrongLayerKey`). The count policy
belongs to the preset: a layer flipping it on would silently change what every
digit key in the preset means, and the user who wrote the layer would be the
last to know. A user who wants counts changes preset.

### 7. Two load-time rules, in the spirit of prefix-freedom

Both fail the **load**, like ADR 0006's prefix-free rule and for the same
reason: the conflict surfaces when the file is written, not when a finger slips.

**A bare digit 1-9 may not open a binding while counts are on.** `0` is exempt
by decision 4 — the two never compete for it. Only the FIRST chord of a
sequence is inspected, which is exactly what the accumulator does: it never
opens a count with a sequence in flight, so a digit anywhere but the front is an
ordinary key. Both rules share one `digit_of`; two answers to "is this a digit"
is precisely how the load rule and the resolver would drift apart.

**`Tab` is reserved for `pane.switch`, on the Browse screen only.** Browse only
because every bundled preset legitimately binds `tab` to `dialog.pane` inside
`[dialog]`, and that is not pane switching — banning the key outright would
break dialogs norte already ships.

The rule is about the **first chord of the sequence**, not the whole sequence.
The first implementation rejected only a bare `tab` bound elsewhere, and left
`["tab", "j"]` legal in a preset that binds no bare `tab`. That is not a
loophole worth keeping: **pressing Tab and having it sit pending is exactly as
much a loss of pane switching as rebinding it**, and the user who presses Tab
expecting the other pane gets a keymap waiting for a second key instead. The one
legal shape is the reserved chord, alone, bound to its reserved command. No
bundled preset was affected by the widening.

The reserved table holds a `KeyCode`, not the text that spells it. A `&str`
would have to go through `parse_chord`, and a fallible path over a hardcoded
constant either gets `unwrap`ed (rule 6) or blames the user's file for a typo in
ours. The spelling in the diagnostic comes back out of `Display`, so the round
trip is closed by construction.

Both rules run in `build_for_impl` **and** in `build_diagnostics`, so `norte
doctor` is not the one path that stays quiet.

### 8. Resolution is still timing-free

A count terminates on the first non-digit. There is no timeout anywhere in the
count path — not for "have you finished typing the number", not for "was that
digit meant as a count". That is what ADR 0006 bought by making the map
prefix-free at load, and this feature does not spend it.

`Esc` clears the count as well as the pending sequence, and so does any key that
misses. A count left glued to the next keystroke is the worst failure this
feature can have, so both terminal paths clear it explicitly rather than relying
on the next `Run` to consume it.

### 9. `build_effectives_with` returns a `Result`, and `build_effectives_preset_only` still does not

The first is the GUI's error-recovery path — what runs when a *user's* keymap
layer is broken. ADR 0043 decision 4 gave it a live panic route, and ADR 0043's
own consequences section said it "must become a `Result` before" K2. It is one:
the error goes to the existing `NorteGui::keymap_error` banner and the GUI falls
back to the bare preset, which is the recovery the surrounding code already
performs for other load failures.

`build_effectives_preset_only` deliberately keeps its `expect`. Its only inputs
are compiled-in TOML constants — an unknown preset name resolves to `orthodox`
*before* parsing — so the panic is unreachable by any user input; the invariant
is stated in a `# Panics` section, and a test walks every `KNOWN_PRESETS` entry
to keep it unreachable. Making it fallible would force `main.rs` to invent a
last-resort keymap for a case that cannot happen, which is a policy decision
with no failure to justify it. Recorded so the asymmetry reads as a choice
rather than an oversight.

### 10. The hot path stops allocating

`means_command` asked "is this keypress the shortcut for that menu item?" by
rendering every binding to a `String` and re-parsing each one — 115-134
allocations per key event, roughly triple once K2b lands four presets. It is now
`Effective::single_chord_runs(chord, command)`, one pass comparing `Chord`s,
zero allocations. Behaviour is pinned unchanged first: a multi-key sequence
still never matches a single press, and an unavailable binding is still not a
match.

## Consequences

**Positive.**

`vim.toml` behaves like vim for the count-taking half of its movement keys, and
K2b's `far.toml` can be faithful on the same mechanism. No command signature
changed, so the ~80 commands are exactly as they were. The sacred-key rule that
ADR 0043 deferred now exists and is tested, ahead of the four presets that make
it necessary — and the widened form means a preset cannot lose Tab by accident
either. Both of K1's deferred debts are closed, one of them on a hot path.

**Negative, and accepted.**

*The repeat loop lives in the frontends, three times.* How many times to run is
**not** repeated: `Count::times()` is the single policy, and the TUI's key arm
and the GUI's two sites call it. Three private copies of the same `match` is how
three sites come to disagree, and the first review of this work found them
already differing in their stopping conditions.

Where they legitimately differ is *when to stop*, because they guard different
screens, and each guard is now complete rather than a hand-listed pair:

- The TUI wraps the whole outcome tail (`cd_landed_pane` → `apply_cd` →
  `reap_search_run` → `pending_open`), because a `dispatch` without its outcome
  leaves tasks alive and panes unrefreshed. It stops on `app.quit`, on
  `nav_stalled` (below), and on **any change** to `keyboard_owner(app)` — a
  fingerprint of the eleven surfaces the run loop routes keys by. Comparing
  rather than testing is required: `5` then `viewer.down` starts with the
  viewer already open, and a guard that stopped on "a viewer is open" would kill
  that count on its first turn.
- The GUI's dual pane stops on `overlay_in_front() || help.is_some()`; its
  viewer loop stops when the viewer closes.

Every early `continue` of the outer event loop that the TUI arm used to contain
— the `lua:` branch and the `Command::parse` guard — sits **above** the loop,
because a `continue` that skips the loop counter turns `5j` into an infinite
loop.

*A repeated navigation stops the moment a step does not land.* `nav.back` and
`nav.forward` are the only `counts: true` commands that reach the network, and
they interact badly with the trail: a `Failed` or `Cancelled` step is put BACK
on the trail (correctly — the pane never moved), so the next turn of the count
would take the same step and issue the identical listing. One keystroke would
become up to 9 999 sequential remote calls on a slow or dead host, and `Esc`
during a listing *is* `Cd::Cancelled`, so the key pressed to stop it would feed
the next retry and the only way out would be killing norte. `nav_stalled` is
asked of those two commands only, deliberately: `Cd::Cancelled` is also the
default outcome of every command that is not a `cd`, so a blanket break on it
would stop `5j` after one row.

*A user-visible behaviour change in the GUI.* With `vim` and counts on, a bare
digit on the dual pane resolves as `Counting` instead of `Reset`, and the
type-to-filter quick search only opens on `Reset`. So under that preset, typing
`5` starts a count rather than a filter. This is intended — it is what a count
*is* — but it is a change a user will notice, and it is the second instance of
the class ADR 0043 flagged (a key that would have opened quick search doing
something else instead). Users of `orthodox` and `cua` are unaffected, because
the flag is per preset.

*A count is bounded by 9 999 dispatches, not by anything cheaper.* Nothing
coalesces `9999j` into one move: it is 9 999 calls, each clamping the cursor.
That is acceptable only because every `counts: true` command is a pure in-memory
mover — and it stays acceptable only as long as that is true. **Adding
`counts: true` to a command that submits a task, does I/O or allocates per call
turns a four-keystroke sequence into 9 999 of those.** `nav.back`/`nav.forward`
are the two exceptions and are held by three bounds together, not one:
`nav::HISTORY_MAX` caps the trail at 30 steps, `nav_stalled` stops the repeat
on the first step that does not land, and the by-name test makes the next
addition argue its case. Any future `counts: true` entry needs the same
argument made explicitly.

*`Ignored` is honest but not always visible.* In the GUI's viewer the flash is
not painted while the viewer is open (the debt ADR 0043 already recorded), so a
count ignored there is correct in state and silent on screen until K3 gives the
viewer a status line.

*Decision 3 is a gap, not a solved problem.* `12gg` is `Ignored`, and a user
arriving from vim will read that as norte not supporting counts properly. It
supports repetition; it does not support absolute positioning, and no flag in
this ADR can give it one.

## Alternatives considered

**Passing the count into `dispatch`.** Rejected on decision 1: ~80 signatures,
and a command that forgets its count fails silently, which is the failure class
K1 removed.

**Resolving the digit-versus-binding conflict by precedence instead of failing
the load.** Rejected on decision 7: it is ADR 0006's ambiguous-prefix argument
with the tokens changed. Whichever side wins, the user cannot predict which key
does what by reading their file.

**Banning `Tab` on every screen.** Rejected: every bundled preset binds it to
`dialog.pane` inside `[dialog]`, so the blanket rule would fail the load of
keymaps norte already ships, to prohibit something that was never pane
switching.

**Rejecting only a bare `tab` binding, leaving `["tab", …]` sequences legal.**
This was the first implementation. Rejected on decision 7: a pending Tab loses
pane switching as completely as a rebound one.

**Sacred keys beyond `Tab`.** The specification names one. `SACRED_BROWSE` is a
table so a second entry costs one line, but inventing entries the specification
does not name would be making policy in an implementation.

**Wrapping or saturating the count at `u32::MAX` instead of four digits.**
Rejected on decision 5: both produce a number the user did not type. Dropping
the extra digit is the only behaviour that is explainable in one sentence.
