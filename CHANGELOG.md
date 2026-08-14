# Changelog

All notable changes to norte are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and releases follow
[Semantic Versioning](https://semver.org/). The wire protocol is versioned
independently through `PROTOCOL_VERSION`.

## [Unreleased]

### Added

- **Synchronise two directories, one way, and be told what you cannot take
  back before you say yes:** `Ctrl+y` over the two panes plans a
  synchronisation, and inside the **terminal interface's** diff pane `s`
  chooses *update* (copy what is missing, overwrite what differs) and `m`
  chooses *mirror* (that, and delete what the source does not have). Either way
  you are shown every step it intends to take before anything moves. Mark rows
  in that pane first (`Ins`) and the plan covers only those, subtrees included.
  There is now a second diff pane, in the graphical interface (below), where
  `Ctrl+y` plans an *update* and applies it — the two keys that choose the mode
  from inside that pane are still terminal-only (#188).
  What makes this different from a scripted copy is that **the plan you
  approved is the plan that runs.** norte keeps it, and approving sends back
  nothing but a fingerprint of it, so there is no path by which a different
  intention arrives between the screen and the disk — not from a bug, not from
  an agent, not from a client that decided to be clever. The plan is retained
  for ten minutes, can only be applied once, and is discarded when it is
  applied, when it expires, when you disconnect, when the daemon restarts, or
  when too many pile up. Since the world can move inside those ten minutes,
  norte re-checks each file it is about to overwrite or delete against what it
  looked like when you approved, and reports a conflict instead of destroying
  something that changed underneath you.
  **The headline is what the destination's trash can give back, not a count of
  irreversible steps**, and that distinction is the whole reason this took the
  shape it did. A plan of nothing but copies onto a disk with no trash looks
  identical, step for step, to the same plan onto one with a trash — and one of
  them undoes completely while the other undoes nothing at all. So norte says
  which of the three cases you are in: everything comes back, or what was
  replaced is in the system trash where you can fish it out by hand (macOS and
  Windows), or it is gone. The confirmation question changes with the answer
  rather than reading the same over all three.
  It is journalled, so `undo` reverses the whole batch — copies, overwrites and
  deletions together, in the right order — and where it cannot, it says which
  files it left alone rather than reporting success over a half-undo. On Linux
  and BSD this now works at all, which it previously did not: norte implements
  the freedesktop trash itself instead of delegating, so it knows exactly where
  it put each file and can put it back. Previously an undo could restore the
  *new* file over itself and leave your original in the trash, and call that
  success.
  Every step carries the criterion that decided it and how much that criterion
  proves, so a mirror that deleted something can tell you it did so because two
  dates differed and not because anyone verified the contents. Sizes are an
  honest lower bound: a plan reads "1.2 GB, plus 340 files whose size the
  provider would not give", never a confident total built out of zeroes. It is
  a normal task — progress, and `Ctrl+K` cancels — and cancelling leaves a
  clean destination and a closed, undoable batch, never a half-written file.
  Overlapping folders are refused before a single byte moves: the same folder
  twice, one inside the other, the same folder under two spellings on a
  case-insensitive disk, and a symlink pointing at the other side.
  On the wire that is protocol **0.40.0**: `sync.plan` and `sync.apply` as
  cancellable tasks, `sync.steps` and `sync.plan_done` streaming to the
  connection that asked and to no other, `sync.report` for what happened
  (ADR 0049).
  **What this is not, on purpose.** It is **one-way**. There is no two-way
  synchronisation, because "both sides changed" needs a rule for choosing and
  there is nothing here honest enough to make that choice for you. There is
  **no resume**: a cancelled synchronisation is undoable and re-plannable, not
  continuable. There are **no conflict rules** beyond the one switch — what to
  do when the comparison could not verify its own answer. And it is the
  terminal interface only, over a daemon: no command line, no agent surface, no
  graphical one (#161, #162), and **not in the standalone TUI**, which has no
  journal — the key is shown greyed with that reason rather than hidden,
  because synchronising without an undo is not a convenience worth having
  (#167). Two limits worth saying out loud: a name that is legal at the source
  and not at the destination is reported when it fails rather than when you
  approve (#163), and the re-check before deleting a folder looks at the folder
  and not at every file inside it, so something added deep within it after you
  approved goes with it.
- **Compare two directories, and be told how much the answer is worth:**
  `Shift+F2` compares the two panes and opens a diff pane over both of them —
  one row per pair, and each row says not only *what* was concluded but *which
  criterion* concluded it and *how much that criterion proves*. This is the
  point of the feature. Two different sizes prove two different files. Two
  dates nine seconds apart suggest it and prove nothing: a restored backup, a
  `touch` and a real edit look identical at that rung. An entry inside a zip
  has no size and no date you should trust, and the honest answer there is
  "the provider cannot say" — which norte shows as an answer, with its own
  glyph, rather than as an error or as a comforting "same". So a comparison
  against an archive or an object store completes normally instead of filling
  the screen with failures that are not failures. Both columns are ASCII
  glyphs, not colour, so "same, verified" and "same, probably" stay
  distinguishable to a reader who cannot see colour.
  The comparison is cheap first and expensive only if you ask: presence, kind,
  symlink target, size, then date; a full sha256 of both sides runs **only**
  when you request it, and an agent needs the content permission to ask for it
  at all. It is a normal task, so it shows progress and `Ctrl+K` cancels it,
  and it walks with bounded memory rather than holding two whole listings — a
  directory that cannot be read becomes one row and the walk carries on
  instead of dying at leaf 40 000.
  In the pane you filter by category, jump into either side, and choose which
  side an action means — the active side is one you pick, never one norte
  infers from the row, because guessing on a destructive operation is not a
  feature. If the answer came back incomplete, it says so instead of painting
  "done" over a partial one.
  On the wire that is protocol **0.39.0**: `fs.compare` returns a task and
  `compare.rows` streams the rows, in batches, to the connection that asked and
  to no other (ADR 0048).
  **What this is not, on purpose:** it does not change anything. There is no
  synchronisation plan, no journal entry and no undo, because nothing is
  written — that is the next piece of work. Symlinks are never followed
  (norte refuses the request rather than quietly ignoring it, and compares link
  targets as bytes). And it is the terminal interface only: no command line and
  no agent surface. (The graphical one arrived after this note was written —
  see below, #158.) One known limitation worth
  saying out loud: inside tmux, `Shift+F2` does not reach norte at all — nor do
  the long-standing `Shift+F6` and `Alt+F7` — while plain function keys work
  (#159). Until that is fixed, reach it from the command palette.
- **Synchronising from the graphical interface:** `Ctrl+y` over the two panes
  plans the same one-way synchronisation the terminal does, shows every step
  with the same three glyphs — what it is, how sure the comparison was, and
  what undoing it would give you back — and asks before it writes. The question
  it asks is the shared one, so it counts the deletions and says whether they
  go to a trash you can restore from; a plan it will not let you approve gets
  an explanation instead of a shorter prompt. Pressing `Esc` after you approve
  and before the daemon answers now cancels rather than closing, and a report
  that arrives with the panel already gone is still shown — because it is the
  only record of what was written and of the undo that exists (#161).

  This lands *update* only: choosing *mirror*, the mode that deletes, still
  needs the gesture in the diff pane, and that is #188 rather than a key away.

- **The diff pane, in the graphical interface too:** `Shift+F2` — `Alt+D`
  under the `far` and `norton` presets, which never had a comparison key to
  transcribe — now compares the two panes in the GUI and opens the same diff
  pane the terminal interface has had: the same rows, the same two ASCII
  glyphs for verdict and confidence, the same five category filters with their
  counts, the same choice of which side an action means, and the same footer
  that says whether the answer is complete rather than painting "done" over a
  partial one. Both interfaces now take that last decision from one shared
  rule instead of each deciding for itself — the command line and the agent
  surface each re-derived it and each got it wrong, reporting a complete
  answer for a run that had lost batches, and this was the last surface left
  that could have made the same mistake a third time.
  Roots and filenames are shown the way the file list shows them: sanitised,
  and marked when sanitising changed them. The pane's header keeps each root
  in its own element with the `↔` in a third, so a directory named with an
  embedded `↔` cannot read as a different pair of roots and a long left root
  cannot push the right one off the screen unmarked. The terminal's header is
  still built as one string and does not have that protection (#185).
  Synchronising from this pane is the next piece (#161): `s`, `m` and `Ins` do
  nothing here yet, and neither does the mouse — selecting and navigating are
  keyboard-only in this pane for now. Closes #158.
- **A number before a key repeats it, on the presets whose originals do that:**
  typing `5` then `j` under the `vim` preset moves down five rows, `12` then a
  page key turns twelve pages. The number is visible at the status bar while
  you type it, `Esc` cancels it, and a key that misses clears it — a count can
  never end up glued to the keystroke after it. A count over a command that
  does not take one runs the command once and says so rather than swallowing
  the number: `3` then a quit key quits, and tells you the 3 was ignored.
  It is opt-in per preset, so `orthodox` and `cua` are unchanged and their
  digit keys still mean what they meant. **Two things a `vim` user will
  notice.** Digits are now counts there, so on the GUI a bare digit starts a
  count instead of opening the type-to-filter quick search. And `12gg` does
  **not** mean "go to line 12": norte repeats the command twelve times, and
  twelve "go to the top" is still the top, so norte reports the count as
  ignored instead of pretending. Absolute positioning needs a command that
  takes a line number, which does not exist yet (ADR 0044).
- **A keymap that would break pane switching, or that fights its own counts,
  now fails to load with a diagnostic** instead of doing something unexpected
  under your fingers. `Tab` is reserved for switching panes on the file
  screen — a preset or a layer that binds it to something else, or that merely
  starts a two-key sequence with it, is rejected at load, because a `Tab` that
  sits waiting for a second key has lost you pane switching just as
  completely. And with counts enabled, binding a bare `1`-`9` is rejected too,
  since the key cannot be a digit of a count and a command at the same time.
  `0` stays bindable: a count never starts with zero. Both are reported by
  `norte doctor` as well.
- **A batch of renames is one transaction, so a permutation finally works:** AI
  rename used to apply its plan one `fs.move` at a time. Ask it to number a
  season of episodes correctly and the very first move refuses, because the name
  it wants is the name the next file still has — a permutation could never
  succeed, and a permutation is the ordinary case. There was no preview of the
  whole plan either, so the first collision was discovered halfway through, and
  undoing what had already landed meant walking back a list of individual moves
  by hand.
  The core now decides the whole thing before anything moves. It reads the
  directory, works out an order in which each name is free when it is needed,
  inserts a temporary of its own where a cycle has to be opened, and reports
  every collision it found against the plan as a whole — a name two rules both
  want, a name already taken by a file nobody is moving, a source that is no
  longer there. Then it runs the plan as ONE task and records it as ONE undoable
  unit: undo is a single step, and a batch that fails partway renames back
  everything it had already done. Cancelling one does the same, so the directory
  is never left half-renamed.
  Names are compared the way the destination directory compares them, asked of
  that directory and never guessed from the operating system: two spellings of
  the same accented name are the same name where the filesystem says so, and a
  name that is not valid text is compared byte for byte and never normalised.
  Both frontends now apply the AI plan through this path.
  On the wire that is protocol **0.36.0**: `fs.rename_batch_plan` returns the
  reviewable plan, `fs.rename_batch` executes the plan a human approved,
  `fs.rename_batch_report` says what a failed batch managed to put back, and a
  filename now crosses on its own as a percent-encoded segment rather than as a
  string. New task kind `RenameBatch`, new errors `PlanStale` (the directory
  changed between the preview and the confirmation — re-plan and look again) and
  `PlanNotExecutable` (the plan had collisions, so nothing was attempted).
  `policy.undo_report` gained `batch_stuck` and `compensations_lost` for an undo
  that could not finish putting a batch back, and the approval prompt gained
  `paths_total` so a request over many paths can say how many. See ADR 0042.

- **The journal groups a batch under one id:** it gained a `batch_id` column, so
  the entries of one batch are recognised as one unit by undo instead of as a
  pile of unrelated renames — walking those back one at a time is exactly the
  thing that cannot work. The migration runs on open and leaves an existing
  journal's entries hashing precisely as they did, so the tamper-evident chain
  still verifies across it. One caveat worth knowing: a journal that has been
  migrated is read as `Broken` by a norte binary older than this release, which
  is [#127](https://github.com/compilando/norte/issues/127).

- **The graphical help answers where you ARE, from every screen:** `F1` opened
  the index no matter what was on screen. It now opens the page about the
  screen you are on — the viewer, the collision dialog, the rename prompt, the
  AI plan — through a table that a new modal cannot compile without extending,
  so nobody can add a screen without deciding which page explains it.
  Two places the key could not even reach: with a dialog open the modal ate it,
  and with the viewer open its own resolver did. Both now reach the help, and a
  help opened over a dialog takes the keyboard while it is up — the dialog
  stays painted underneath, so the question is never hidden, only unanswerable
  until you close the page about it. Nothing gets confirmed through a page
  covering the question.
  From the command palette, `F1` on a row opens that command's page — the other
  direction of the bridge that already carried your filter INTO the palette. A
  row nothing documents says so instead of dropping you in the index, where you
  could not tell whether your command was in there somewhere or simply
  undocumented.
  The help's scrollbar can be dragged now, and clicking the track jumps there.
  The sidebar's stays an indicator on purpose: its window is derived from the
  cursor, and moving the cursor OPENS the page it lands on — a scrollbar that
  changes what you are reading is not a scrollbar.

- **The graphical file listing lines its columns up again:** size and modified
  sat wherever the name happened to end, so every row put them in a different
  place while the header — painted outside the list, and therefore full width —
  lined up with nothing. The list virtualiser hands each row a definite space
  but leaves the row itself auto-width, so the name column had nothing to
  absorb; rows now take the width they were given.

- **The graphical help is readable, navigable with the mouse, and sized to your
  window:** what a screenshot showed and no test could. A paragraph did not
  wrap — it ran off the panel and was cut mid-word — and rows painted over each
  other, so a long page appeared duplicated and overlapping. Text is now laid
  out as text: one run per line that breaks at a word, with table rows kept in
  columns instead of run together.
  The overlay was a fixed 720×520 box, a stamp in the middle of a large window;
  it now takes 86% of the window, up to a width where a line of prose is still
  a line of prose. How many rows fit is derived from that size rather than
  guessed at, which is what used to let the sidebar cursor walk off the bottom
  edge and vanish while the keyboard said it had moved.
  The mouse works: click a topic to open it, click a runnable row or a link to
  activate it through the same path `Enter` uses — so a row the keyboard
  refuses is a row the mouse refuses — and the wheel scrolls the page. Both
  panels show a scroll indicator when there is more than fits. Dragging that
  indicator is not wired yet.
  Two things that were simply hard to read: links and command names took the
  colour of the quick-search highlight, which a theme guarantees against ITS
  background and not against this panel, and the sidebar drew group headers
  almost exactly like the pages under them. Secondary text is now derived from
  the prose colour, links are underlined as well as coloured, and a group looks
  like a group. The page title moved out of the scrolling body into a header of
  its own, so a long page stays labelled.
  A command that no key runs now paints its NAME and never its dispatch key:
  the dialog verbs live in a different catalogue, and the page about answering
  a dialog used to read `dialog.confirm accepts what the dialog is showing`.

- **The key that opens the GUI's help now closes it — all of them:** the close
  was spelled `f1` inside the overlay, so the two halves of one switch could
  disagree. No rebinding was needed to see it: the **vim** preset binds
  `app.help` to both `f1` and `?`, so `?` opened a page that only `f1` and
  `Esc` could close. The close is now asked of the keymap, and every chord
  bound to `app.help` answers it — including one behind `ctrl`, which the
  overlay's modifier gate used to swallow. `Esc` still closes in every
  configuration, which is what the footer promises.

- **A keymap change now reaches a help page that is already open:** the page's
  chords and its generated keyboard sheet were frozen when the overlay opened,
  so they could go on naming keys that no longer did anything. They are rebuilt
  where the effective keymaps are; the availability FACTS are deliberately not,
  since freezing those is what keeps a row from changing verdict under the
  reader's cursor. The window is narrow and worth naming: this frontend watches
  no config files, and the settings screen cannot be open at the same time as
  the help — but the write is asynchronous, so changing the preset, closing
  settings and opening the help lands the swap with the page in front of you.

- **A wide publisher no longer pushes a warning off a plugin's help page:** the
  provenance line under a plugin page title is `from an extension · published
  by X`, followed by the host's own flags — cut short, some bytes did not
  decode. The publisher comes from the plugin and is capped at 280 characters,
  which in CJK is 560 columns; the graphical frontend paints that line
  unwrapped in a fixed-width panel, so those flags — the only part of the line
  a reader acts on — were pushed off the right edge by text the plugin chose.
  The publisher now gets what a 96-cell budget has left once the host's
  segments are reserved. The clamp is on the publisher and never on the
  finished line, because truncating that would eat the flags.

- **The help now covers everything, and the gate has nothing left to forgive
  (H3h):** every command norte can dispatch and every screen it can open is
  explained by a page, in English and in Spanish. Sixteen topics per locale,
  up from eight: the viewer, finding things, what the listing shows, answering
  a dialog, settings and themes, AI rename and semantic search, what an agent
  may do here, and extensions — plus the sections the existing pages were
  missing, from the cursor keys to renaming, creating a directory and the four
  answers to a collision.
  The part that outlives the prose is the gate underneath it. Since the corpus
  existed, a command with no page was carried by an explicit allowlist that
  could only shrink, with a compile-time ceiling so it could not quietly grow.
  H3h emptied it, and the list and the ceiling are deleted: a new command or a
  new screen that arrives without a page now fails the suite with nowhere to
  write it down. That is permanent friction on adding a command, and it is the
  point.
  `F1` over any dialog opens prose about THAT dialog — the fallback that
  refused to cover a live question with the index is still there, but it is now
  a guard against someone deleting a page rather than a state the app ships in.
  Three hostile cases the audit asked for are covered from the shared corpus
  rather than from literals: a plugin id carrying a bidi override (a new
  fixture, since an id is a lookup key that crosses the wire), a command title
  that is blank after masking, and a double-width publisher.

- **`norte help` reads the same corpus from the command line (H3g):** the index,
  one page by id, `--list`, `--search`, and `keys` — the keyboard sheet
  generated from *your* effective keymap, not a written list. Embedded: no
  daemon, no network, no engine. `--json` dumps the whole thing (or one page)
  for agents and for a golden that pins the shape, with a `version` field so a
  consumer has a discriminator before it needs one.
  It behaves like a command-line tool rather than a window: exit 1 with one line
  on stderr for an unknown page (naming `--list`) or a search that matched
  nothing, following `grep`; a closed pipe is a quiet exit rather than the Rust
  panic `println!` produces, so `norte help | head` prints what you asked for
  and stops; and the arguments it echoes back in an error are masked and capped,
  since an error line is exactly where a pasted string carrying an escape ends
  up. Callouts are ASCII words where the terminal app paints glyphs — the stream
  may not be a terminal.
  `norte help <command>` is no longer clap's way to reprint a subcommand's
  `--help`; that lives where it always did, at `norte <command> --help`.
  Plugin pages are deliberately not reachable from here: they need the plugin
  registry, and `norte doctor` already reports what is wrong with one.
- **The GUI has the help, and `F1` finally does something (H3f):** the same
  corpus, the same model and the same executable rows the terminal app got in
  H3b–H3e, painted in the active theme — sidebar of topics on the left, page on
  the right, `/` to filter, `⇥` to cross between the two, `⏎` to run the row you
  are reading, `Ctrl+P` to hand your filter to the command palette. Extensions
  get a page each, fetched the moment you open their row and never before, and
  every plugin page says on its face that a plugin wrote it.
  The bug underneath it is worth naming, because nothing reported it: all three
  shipped keymaps have bound `F1` to the help since the corpus existed, and the
  GUI filtered that binding out because its command table did not list the
  command. The key was not broken, it was dropped — silently, which is the part
  that took a test to make impossible again.
  A row the app would refuse is dimmed here too, judged against the facts of the
  moment the overlay opened rather than a fresher set: a page whose verdicts
  shift while you read it disagrees with itself, and `⏎` on a dimmed row now
  says why instead of quietly doing nothing.
  What the reviews caught, since it is the part worth recording: the sidebar
  shipped a literal `help-group-keys` to every reader who pressed `F1` — the
  model always emits that group and neither catalogue names it, on purpose, so
  the painter had to drop it rather than translate it; a plugin whose name is
  blank *after* masking bought itself a nameless row in the Extensions group,
  which is the H3e fix this frontend was missing; plugin ids arrived unvalidated
  where the terminal app drops them; a blank command title sent the row back to
  wearing its raw dispatch key; the reason a dimmed row gave on `⏎` was painted
  underneath the overlay's own scrim; the search box could be entered and never
  left, so letters meant for the page silently edited it; and the body laid out
  every line of a document, not the ones on screen.
  Everything this entry once listed as deliberately not done is done; see
  below.
- **Plugins bring their own help page (H3e):** a plugin can ship a `help.md`
  next to its `plugin.toml`, and it becomes one more page in the help overlay,
  under an *Extensions* group, with the plugin's own commands as rows you can
  run from there. The page arrives on demand — 64 KiB per plugin has no
  business riding every listing — and it is fetched once per time you open the
  help, never while a frame is being painted. A row for a plugin that is not
  approved and enabled is dimmed and says so, which is the answer you came for
  if you are reading that plugin's documentation to decide whether to turn it
  on. The rows wear the name the plugin gave the command, not the internal key
  norte dispatches it with.
  The text is third-party and is treated as such throughout: the host bounds it
  at the same cap the hostile-input parser uses, decodes it, and refuses to
  serve a `help.md` that symlinks out of the plugin's own directory; the page
  is masked when it is parsed, so a bidi override in a title cannot rearrange
  what you read; a `{{cmd:}}` naming a command the plugin does not own stays
  literal text instead of becoming a row you could press; and every plugin page
  says on its face that it is a plugin page, because a page that could pass for
  norte's own prose is a page that can tell you approving is safe.
  `norte doctor` now names six ways a `help.md` can silently do nothing: over
  the size cap, undecodable bytes, announcing a page that serves nothing, a
  `+++` header that does not parse, commands the plugin does not own, and an id
  that collides with a built-in page.
  Two things it deliberately does not do. `F1` pressed over a dialog or from
  the command palette opens without the extensions group: those paths are
  synchronous, and a daemon round trip inside key handling is not worth a
  sidebar node. And the command palette still hides an inactive plugin's
  commands rather than dimming them — the palette is the fast gesture, and the
  place to explain a plugin is its own page.
- **The help stops offering what the app would refuse (H3d):** a row for a
  command that cannot run where you are — writing into an archive, renaming
  something the backend will not let you rename — is dimmed and says why,
  instead of promising a key that is about to fail. The verdict comes from the
  same table the GUI's context menu uses, so the two cannot disagree about
  whether "copy" is available; where the frontends genuinely differ they say
  so as facts rather than forking the table (a `.zip` is a thing you enter in
  the terminal and a file you open in the GUI). Capability flags now come from
  the answer norte was already fetching for the columns and throwing half
  away, so knowing this costs no extra round trip, and a degraded connection
  is finally kept as data instead of a sentence — two degraded connections no
  longer overwrite each other, and the status bar names the most recent one
  and says how many more there are, masked like every other name that arrives
  from somewhere else.
  Three things it deliberately does not claim: policy denial, because in the
  embedded app you are the human, whom the policy engine never denies;
  the difference between a plugin that was never approved and one whose
  approval expired, because the protocol does not carry it; and anything at
  all from a degraded connection — that flag means the session is unencrypted,
  not that it cannot act, and dimming on it would lie to every FTP user.
- **Help about where you actually are (H3c):** `F1` no longer always opens the
  index. From a pane it opens the page about the panes, from a collision dialog
  the copying page, from the host-key prompt the remote page — the mapping
  lives in the corpus, so moving an explanation from one page to another is an
  edit to prose. Pressed over a dialog, the help reads about *that* dialog and
  `Esc` puts you back in front of the question without answering it. While the
  page covers a dialog the dialog's own keys are inert, and its footer says so
  instead of advertising keys that do nothing. A dialog that arrives *while*
  you are reading closes the help rather than hiding behind it: an approval
  you cannot see is an approval you cannot answer — and if you leave the page
  open over one, its timeout denies the agent, which is the safe direction.
  `F1` on a command-palette row opens the page that documents that command,
  the other half of the `Ctrl+P` handoff the help already had.
  Five places have their own page so far; everywhere else `F1` still opens the
  index, and the ones still missing are listed, one per line with its reason, in
  the gate that will not let them be forgotten. Over a *dialog* with no page yet
  nothing opens at all: the index would cover a live question with prose about
  something else, so norte says so and leaves the prompt answerable.
- **Passing a location between the panes:** `Alt+i` sends this pane's location
  to the other one and leaves the focus where it is — the fastest way to line
  up a copy, because the destination is whatever the other pane holds. `Alt+u`
  is the same gesture the other way round, and `Ctrl+U` swaps the two, which is
  how you reverse the direction of a copy without navigating anything: nothing
  is re-read, and the marks, the filter, the sort and the pane's own trail all
  travel with their pane, so the focus stays on the side of the screen you were
  already looking at. Mirroring onto a host you have not visited connects and
  asks about its key exactly as walking there would, and the question belongs to
  the pane that is travelling, not to the one you are looking at. If the
  destination cannot be reached the pane stays where it was and says why. From a
  live-search results pane there is nothing to send: a list of hits is not a
  location, and norte says so rather than guessing which directory you meant.
- **A real back and forward:** `Alt+←` returns the focused pane to where it was
  and `Alt+→` undoes that. It is a trail, not a list of favourites: from one
  directory to a second and then a third, back twice reaches the first — walking
  the "places this pane has been" list instead would bounce between the two most
  recent forever. Navigating somewhere new from the middle of the trail forgets
  the branch you stepped off, as a browser does. If a step back lands on a
  directory that has since disappeared, the step is rewound and the dead
  directory leaves the trail, the popup and the forward branch at once, so the
  key cannot trap you on it; any other failure keeps it, because a host that is
  down is still a place. When the trail runs out the key says so — a key that
  goes quiet is indistinguishable from a broken one.
- **Help you can navigate, and that knows your keys (H3b):** `F1` opens a page,
  not a key dump. The topics of the corpus on the left, grouped by tag; the page
  you are on to the right; `/` to filter the list by page title, page id or the
  commands a page documents; `Tab` to move the cursor into the page; `Enter` to
  run the command a row describes — through the same dispatch its own key uses,
  with the same confirmation, the same policy gate and the same journal entry,
  because a second, quieter way to run a mutation is exactly what this project
  does not want; and `Backspace` to go back where you came from, or out of the
  help when there is nowhere left to go back to. Those are the orthodox
  preset's keys: the overlay resolves every one of them through the `dialog`
  context, so a rebind moves them and the footer it generates says where they
  went.
  Enter on a link follows it and the help stays open — reading is not leaving.
  Enter on a runnable row closes the overlay first, deliberately: the command
  acts on the panes underneath, and help left on screen would cover the
  confirmation it opens. Arrowing the list of topics PREVIEWS them instead of
  navigating, so a scan down the index does not cost one `Backspace` per row
  before the overlay can close. `Ctrl+P` hands whatever you have typed to the
  command palette rather than making you retype it — the same model at a
  different speed — with the built-in commands only, since that handoff cannot
  wait on the backend for the plugin rows.
  No key in any page is written into the text. Each one is a `{{cmd:…}}` mark
  looked up in your effective keymap as the page is drawn, so a rebind changes
  the prose (a hot reload rebuilds that resolver next to the cheatsheet, or
  every page would keep teaching the old key), and a command you have unbound
  is named in the prose instead of claiming a chord you do not have. The corpus
  gains a page about this help itself, in English and Spanish like the rest.
  The old flat cheatsheet is still there, as the last entry in the list,
  generated from the same effective keymap — and still the one place the
  `dialog.*` verbs are all visible, which overlay footers leave out for want of
  width.
  What this phase does **not** do, each with a later phase of its own: the page
  still claims every command it documents is runnable right now, so nothing is
  dimmed and no reason is given for what the app would refuse (H3d); plugins
  cannot ship help pages yet (H3e); and the GUI has no help view at all (H3f),
  although the navigation model lives in `norte-frontend` rather than in the
  TUI precisely so that view is a second painter and not a second corpus.

- **The mouse, in both frontends:** left click focuses a pane and moves its
  cursor, double click does exactly what `nav.enter` does, and the wheel
  scrolls the listing **under the pointer** rather than the focused one.
  Ctrl+click toggles one mark, shift+click marks the range from the cursor
  (additively), and a drag marks what it sweeps — retreating gives those rows
  back instead of leaving everything the pointer ever touched marked. A plain
  click still never marks: browsing a listing cannot change what the next
  command acts on.
  Dragging onto the other pane **copies; shift+drag moves**, and the modifier
  is read at RELEASE, so someone who starts a drag and changes their mind does
  not move what they meant to copy. The drop opens the same confirmation the
  copy and move keys open and goes through the same collision dialog, policy
  gate, journal entry and undo — there is no quieter second mutation path,
  in either frontend.
  The rule that lets one gesture do two jobs is the row you press: a drag from
  a **marked** row carries the marks, a drag from an **unmarked** row carries
  that one row — but only once the pointer crosses into the other pane
  (*promotion*, without which the commonest drag in any file manager would
  transfer nothing). Promotion changes what the gesture does, never what is
  selected: the pressed row is not marked, and rows the sweep marked on the
  way out are given back, so a cancelled drag leaves the selection exactly as
  it was. A sweep armed with shift is deliberately **not** promotable —
  shift means "extend the range", and reading a range that ends past the pane
  boundary as a drop would turn a marking gesture into a move of the whole
  selection. Because the gesture means one thing at home and another across
  the way, it says which before the button comes up: how many items, to which
  directory, copy or move — the GUI as a drag label, the TUI in the status
  bar, both read from the same state the release reads.
  Right click (GUI) opens a menu of operations that already have keys — open,
  view, copy, move, rename, delete, copy path — each dispatching the SAME
  command the keyboard does, with entries that cannot run right now dimmed
  and carrying their reason. Two of them became real commands rather than
  menu-only actions: `pane.rename` (shift+F6) and `pane.copy-path` (alt+y).
  The menu acts on the marks when the clicked row is marked and on that row
  alone when it is not — which means **right-clicking an unmarked row drops
  that pane's marks**, irrecoverably, Esc included. That is deliberate: every
  command prefers the marks when there are any, so leaving them would let the
  menu say "1" while the copy took eleven.
  In the terminal the mouse is captured by default, which means your emulator
  stops seeing the buttons it uses for its own text selection. `[ui] mouse =
  false` (hot-reloaded, and discoverable in the settings overlay) gives it
  back, and Shift+drag selects natively in almost every emulator. Capture is
  released on exit, on panic, and whenever norte hands the terminal to an
  external program, so nothing ever inherits a terminal in mouse mode.
  The semantics live once, in `norte-frontend::mouse`, as a pure state
  machine over pane indices: each frontend hit-tests, feeds it, and applies
  the effects, so the two cannot drift into two different file managers.

- **Help corpus (H3a, ADR 0040):** new crate `norte-help`, the foundation of
  the help-system redesign. Topics are markdown-lite files with TOML front
  matter between `+++` fences, embedded through an explicit `include_str!`
  table (the `norte-theme` preset pattern) with a test that cross-checks the
  table against the directory, so a topic file cannot be silently left out.
  The markdown accepted is a **closed vocabulary** — headings, paragraphs,
  bullets, fenced code, tables, callouts, inline strong/emph/code — plus two
  **live marks** the parser deliberately leaves unresolved: `{{cmd:id}}` and
  `[[topic]]`. A frontend resolves them at draw time through the new
  `ChordResolver` seam, so the prose shows the key the user actually has bound
  and can never claim a chord that a rebind has moved. Ships six seed topics
  in English and Spanish (index, panes, selection, copying, remote, archives),
  every factual claim in them verified against the code rather than against
  the design docs.
  A second parse mode reads plugin-supplied `help.md` as hostile input: it
  never fails and never panics, decodes through `norte-encoding` (so a
  BOM'd file keeps its header instead of losing it), bounds source bytes,
  line length, block count and total table cells, masks terminal hazards
  where the data is built, and reports `truncated`/`lossy` for the UI badge.
  A plugin's topic id is host-assigned, its `{{cmd:…}}` marks must be
  namespaced to itself, and its `[[…]]` links are inert — so plugin help
  cannot shadow a built-in topic, forge a reference to a host command, or
  link into the host corpus.
  Integrity checks (`check_corpus`, `check_commands`, `check_contexts`)
  report findings as data for both the test suite and the future `norte
  doctor`, covering locale parity, duplicate ids, dangling links,
  unknown/undocumented commands, unknown/duplicate contexts, stale allowlist
  entries, and marks typed where the parser cannot make them live. A gate in
  `norte-tui` fails the build when a command in the vocabulary appears in no
  topic, with a hand-written allowlist that phase H3h drains to zero.
  The canonical `norte-testkit` corpus grows a fixture for the live mark
  itself (`cmd_mark_bidi_payload`, hostile names 31 → 32): a bidi override
  inside a `{{cmd:…}}` payload, which the parser must carry byte-for-byte so
  the gate's byte-exact cross-check refuses to ship it.
  No frontend renders any of this yet — `norte-tui` takes the crate as a
  **dev-dependency only**, for the gate; the F1 overlay is phase H3b/H3c.

- **Semantic index (M4-IA-2, ADR 0031 A3, proto 0.33.0):** two new
  methods over the wire. `index.embed` is a cancellable Task
  (`TaskKind::Embed`) that embeds the files a previous `index.build`
  already indexed: it filters by `denied_prefixes`, an
  extension-based text heuristic and a size cap **before reading a
  single byte**, then reads bounded 32 KiB prefixes through the
  providers, skips anything whose `(sha256, model)` is unchanged, and
  batches 16 texts per provider call with a bounded, cancel-aware
  retry on rate limits. `index.search_semantic` is a direct response,
  cancellable via `rpc.cancel`: one embedding call for the query plus
  a brute-force cosine scan over the stored vectors (`k` clamped to
  100, query capped at 4 KiB, scores guaranteed finite). Vectors live
  in the existing index database as an additive `embeddings` table
  (f32 little-endian, cascading with the file row); a vector from
  another model counts as absent and is regenerated. Both endpoints
  are human-only — content prefixes and the query leave the process,
  so agent connections are denied fail-closed — and both pass the
  full AI gate (`enabled`, `local_only`, `denied_prefixes`).
  Configuration is `[ai] embed_provider`; local Ollama is the
  expected default. New CLI verbs `norte index embed` and `norte index
  semantic`, plus a semantic search flow in both frontends
  (`pane.semantic-search`): query prompt → cancellable search (TUI) →
  hostile-safe hit list (masked paths, badges, scores that a crafted
  path cannot push out of view) → Enter navigates to the file. A
  hostile or broken daemon cannot flood either frontend: the shared
  `validate_semantic_hits` belt in `norte-frontend` rejects any
  response over the wire ceiling or carrying a non-finite score,
  whole, never truncated.

- **AI rename over the wire (M4-IA, ADR 0031, proto 0.32.0):** new
  `ai.rename_plan` method — a direct response, cancellable via
  `rpc.cancel`, that returns the reviewable plan and never mutates.
  The daemon builds the configured AI provider at startup (opt-in,
  degrading — a broken `[ai]` never aborts `norte daemon run`), denies
  agents fail-closed, and caps the instruction at 4 KiB. TUI and GUI
  gain the full flow (`pane.ai-rename`): instruction prompt → reviewable
  plan modal (target dir shown, numbered pairs, hostile names masked and
  badged, scrollable window, plans over 256 entries rejected en bloc) →
  N journaled `fs.move` tasks with undo. A malformed pair from a
  hostile or broken daemon aborts the whole apply before any move is
  submitted (shared `validate_ai_plan` belt in `norte-frontend`).

- **The plugin system has an installable plugin, and a way to install one.**
  It shipped with neither: discovery reads `~/.config/norte/plugins/<id>/` and
  nothing ever put anything there, while a finished syntax-highlighting
  previewer existed only as a test fixture. `norte plugin install <dir>` brings
  one in, `just plugin-syntect` builds and installs that one, and its manifest
  is now a real file the test reads rather than a literal the test invented.
  Installing is not consenting: the plugin arrives discovered and unapproved,
  because turning "I brought this file" into "it has its capabilities" is the
  whole decision. Replacing an installed plugin requires `--force` and
  **withdraws its consent** — the approval digest covers the manifest and not
  the `.wasm`, so without that, a new binary would run under a permission a
  human granted to a different one, with the manifest identical so nothing
  noticed.

- **A plugin's WIT no longer breaks every other plugin.** The package version
  travels inside each interface name, so one shared package meant any change to
  `provider` renamed `previewer` and every previously compiled plugin stopped
  loading — verified twice and never fixed, invisible here because the example
  guests are rebuilt every time, fatal for anybody else. Three packages now, so
  the interface that is going to keep moving moves alone.

- **A plugin that declares a hook is refused instead of installed.** There is no
  hook interface, no world, and no call site: the manifest accepted one, the
  manager listed it, and nothing would ever have run it.

### Fixed

- **A name containing `↔` could spoof the pair in the terminal's comparison
  pane.** The block title joined both roots into one string —
  `left ↔ right` — and `↔` is an ordinary printable character: it is not a
  terminal hazard, so it was never masked and carried no badge. A directory
  legally named `docs ↔ ⟨file⟩/home/victim/backup` therefore read as a
  different pair of roots than the ones actually being compared. The same
  join also let a long left root push the right one out of the title with no
  `…` and no other sign that anything had been cut. Both roots are now built
  and width-budgeted separately before either reaches the title, and the
  separator is its own styled span, so an embedded `↔` stays inside its
  root's half instead of being read as the boundary between the two (#185).

- **Lowering the case of a name can re-spell it, and batch rename was not
  looking again.** When a directory does not distinguish upper from lower case,
  the batch planner compares names by normalising them and then folding the
  case. Those two steps do not commute. `J` followed by a combining caron has no
  single-character capital, so normalising leaves it as two characters; lower
  its case and the result *does* have one, `ǰ`. The planner therefore called
  those two names different, and every case-insensitive volume — Apple's APFS
  and HFS+, a case-folding Linux directory, a case-insensitive network share —
  calls them the same file. A batch aiming at one while the other sat in the
  directory saw no collision and asked for a rename onto a name that was taken.
  The comparison now normalises again after folding, so the collision is
  reported and the plan is refused before anything moves. Both spellings are in
  the canonical hostile-name corpus, which is what the fix is tested against.
  One relative of this is still open and now written down where the code makes
  the decision: a filesystem folds case with case *folding*, this folds with a
  lowercase *mapping*, and the two disagree on about twenty characters —
  a word-final Greek sigma, the micro sign against Greek mu, the long s. See
  [#129](https://github.com/compilando/norte/issues/129).

- **Batch rename asked a directory about its case rules before looking at it.**
  A provider is allowed to work out what a directory does with names lazily, on
  its first real operation — the local provider probes exactly then, because the
  question cannot be answered without touching the disk and answering it must
  not block. The planner asked before it listed, so the very first plan on a
  freshly opened directory got the operating system's *guess* instead: a volume
  that folds case planned as though it distinguished it, which is a collision
  not reported and a preview missing the one line that mattered. That is the
  normal path for an agent bridge or a one-shot command, where planning is the
  first thing that happens. It now lists first and asks afterwards.

- **Eight invisible characters were walking straight past the mask (#125).**
  `is_terminal_hazard` claimed in its own documentation to cover "the invisible
  Cf/Zl/Zp", and it was a list somebody wrote by hand. `U+2064` INVISIBLE PLUS,
  `U+2061` FUNCTION APPLICATION, `U+206E`, `U+FFF9`, `U+180E` and `U+2800`
  BRAILLE PATTERN BLANK were not in it — and `U+3164` HANGUL FILLER and
  `U+115F` never could have been, because they are category **Lo**: letters
  that paint nothing. No enumeration of Cf was ever going to catch them, and
  they are the classic invisible-smuggling code points.
  What that costs is the premise the mask exists to protect. `a<U+3164>b.txt`
  and `ab.txt` are two different names that look identical, and neither one
  raised the hostile badge — so approving the one you read approved the one you
  did not. Every frontend delegates here, so there was no defence downstream.
  The set is now decided by the Unicode property
  `Default_Ignorable_Code_Point` plus the characters that paint nothing without
  being ignorable to anyone (braille blank, the interlinear annotations, the
  line and paragraph separators). ZWJ and the variation selectors stay allowed,
  deliberately and now explicitly: they are default-ignorable too, and masking
  them would break composed emoji for the sake of a twin that differs only in
  that. Two corpus fixtures pin each half of the hole, and a test walks the
  whole code point space so a set that grows by accident is loud.

### Changed

- **The terminal binary is `ntc`.** Nobody types `norte-tui` twice a day;
  Norton Commander was `nc`. `norte tui` still launches it and the CRATE keeps
  its name — renaming that would touch five manifests and a crates.io identity
  to spare nobody any typing. The rename had one defect that could half-land:
  the CLI hands the process over BY BINARY NAME, a string that compiles whether
  or not a binary answers to it and fails at `exec` time, with the user in
  front of it. A test now reads the expected name out of the TUI's own
  manifest, so the two cannot drift apart. `cargo uninstall norte-tui` still
  takes the crate name, which is why the `justfile` says so out loud.

- **A tag now produces something you can download.** cargo-dist had been
  configured since before the first tag and had never run: its model is
  CI-driven, and CI is off. `just dist` builds the artefacts on a developer
  machine, `just dist-smoke` unpacks each archive and runs the binary inside it
  — the one packaging failure a user finds before we do — and `just
  dist-publish <tag>` uploads them, with the protocol, config and keymap JSON
  Schemas alongside, so a third party can write a client without cloning
  (#13). The five configured targets stay in `dist-workspace.toml` because
  they describe the release the project should produce; what we build today is
  **x86_64 Linux only**, and the release notes say so rather than the config
  quietly pretending otherwise. The checksums dist generates are checksums, not
  signatures, and nothing calls them that.

- **`norte` ships with `ntc`.** The CLI was marked out of the release back when
  it was an M0 test bench. It is now the non-interactive half of the product —
  `daemon`, `connect`, `mcp`, `policy`, `undo`, `index`, `ai`, `audit`,
  `doctor` — and an artefact carrying `ntc` alone leaves a user with no daemon
  and no `doctor`. dist builds one archive and one installer per package, so
  there are two of each; the README gives both.

- **The graphical interface stays source-only, and the gate says why.**
  `norte-gui` joined the workspace (one lockfile, one resolution) but is kept
  out of `default-members` and out of the release. The reason is not build
  time. GPUI enables `serde_json/preserve_order`, and cargo unifies features
  per invocation: with the GUI in the same `cargo` as the core, the core's
  `serde_json` swaps sorted maps for insertion order — and with it the protocol
  JSON Schema we publish, the CLI's `--json`, and the goldens that pin them.
  Five tests went red without a line of code changing. So the gate names its
  packages instead of saying `--workspace`, which was silently overriding
  `default-members`, and the core is now tested exactly as it is distributed.
  A GPUI binary also links against the graphics stack of the machine that built
  it, which is the honest limit of shipping one at all. Its own gate,
  `just gui-ci`, keeps auditing it against its own licence policy — excluded
  from the workspace audit, never unaudited.

- **The help overlay reads like a page now.** The sidebar is sized to its own
  titles instead of a flat 24 cells — floored at that 24 so nothing narrows, and
  capped at a third of the screen — so the list of topics stops cutting five of
  its nine rows on a normal terminal. The prose it points at gained a gutter and
  lost its 90-cell lines: the body is laid out at a 72-cell measure, which is
  what a line of text is read at. Groups are separated by a blank line, a
  heading inside a page gets more air than a paragraph break, and the synthetic
  keyboard entry no longer carries a header that repeats its own name. The
  footer says where you are in the page (`{line}/{total}`, the viewer's idiom)
  whenever the page does not fit — the runnable rows of a topic are painted
  behind all of its prose, and nothing used to say they were down there.

- **Keys are spelled the way the documentation spells them.** `F5`,
  `Shift+F8`, `Ctrl+Alt+F5`, `PgUp` — everywhere a chord is painted: the F1
  cheatsheet, the prose of every help topic, the command palette's chord
  column and the footer of every overlay. `Chord`'s `Display` stays raw and
  lower case, because logs and debug output want the literal chord; the
  conventional spelling is a single shared presentation home
  (`norte_frontend::keymap::paint_chord`) that the three painters now route
  through, and which masks terminal hazards FIRST — an untrusted project
  `./.norte/keymap.toml` can bind any codepoint, and nothing cosmetic may
  resurrect it. A key bound to a single printable character is left exactly as
  it is: `y` is not painted `Y`, because `Y` is a different binding and would
  be telling you to press Shift.

- **The help footer fills the terminal it is given.** It used to drop
  `[enter]` and `[esc]` from the printed hint unconditionally so the rest
  would fit at 80 columns, which left a 113-column footer half empty with two
  keys hidden for no reason. All five verbs are now offered in priority order
  and the width decides: a wide terminal shows them all, a narrow one keeps
  the ones you cannot guess (`[/] filtrar`, `[backspace] atrás`,
  `[tab] otro panel`) and marks the cut with `…`. Whole `[chord] label` groups
  as always — never half of one.

- **`q` no longer closes the help.** The overlay now resolves its keys through
  the `dialog` context like every other one, and `q` is `app.quit` there — a
  key that means "leave the app" cannot also mean "leave this page". `Esc`
  closes it, the key that opened it (`F1`) closes it, and `Backspace` goes back
  a page or out of the help when there is nowhere left to go back to.

### Fixed

- **A help page no longer breaks a word at a style change.** Only whitespace
  is a break opportunity: two styled fragments with nothing between them are
  one word and wrap together. A sentence closing on an inline code span used to
  render as `…dentro de un .zip` with the full stop alone on the next line, at
  any width where the boundary landed near the margin.

- **The F1 cheatsheet stops quoting its own translation keys.** The `dialog`
  keymap merges the preset's `[global]` section, so `app.quit` and its
  neighbours turn up while the dialog half of the page is drawn — and that half
  was asking the `dialog-cmd-*` catalogue for them. A missing Fluent message
  answers with its own id, so eight rows read `dialog-cmd-app-quit` at whoever
  came looking for the key. Each command is now labelled from the catalogue it
  actually lives in.

## [0.3.0-alpha.2] - 2026-08-02

### Added

- **The TUI watches the visible directories (#106):** external changes to
  the panes' local directories now refresh automatically — a native
  watcher (inotify/FSEvents/ReadDirectoryChangesW) over both visible
  `file://` dirs, with a graceful fallback to a 2-second mtime poll (with
  a one-time status notice) when the watcher cannot start or the inotify
  watch limit is hit, never a failure. Events are debounced with a true
  trailing edge plus a floor between refreshes, so a large copy into the
  watched directory coalesces instead of refreshing every 300 ms; a watch
  event never interrupts an open dialog, overlay, or quick search — it
  queues and fires when the interaction ends. The refresh takes the same
  cancellable path as Ctrl+R (marks survive, #118 ritual). Remote and
  archive panes remain manual-refresh (no inotify there); polling-mode
  limits are stated honestly in the notice (edits to existing file
  contents don't change the parent dir's mtime). GUI watching is still
  pending on #106.

- **Plugin column cells rendered in both frontends (#117 follow-up):**
  `plugin:<plugin>/<column>` ids configured in `[ui.columns]` now paint
  real cells through the shared column funnel — default width 12 (spec
  width/align/header overrides apply), headers via the shared label
  resolver, capped at 8 plugin columns per list (painted always equals
  requested; `norte doctor` reports the excess as
  `columns-plugins-over-cap` and retires `columns-no-renderer`). Values
  arrive asynchronously per listing (piggybacked on the decoration fetch
  in the TUI, the session Columns command in the GUI), are validated
  against the live catalog (approved + enabled + the column declared by
  THAT plugin), sanitized and capped on ingest, and defensively re-masked
  at render; absent stays blank. The GUI's previous behavior of
  unconditionally painting every declared plugin column at a fixed 96px
  is retired: `[ui.columns]` is now the single source of truth. Deferred
  to #120: offering declared plugin columns in the picker, and
  disambiguating duplicate bare column ids across plugins.

- **Provider attribute columns rendered in both frontends (#117):** the
  `attr:` columns the config, model and picker already accepted now paint
  real cells. The shared render funnel is `ColumnId`-typed end to end;
  attr cells format by the value's own tag refined by the catalog hint
  (sizes IEC/SI/exact, timestamps relative/ISO, POSIX modes `rwx`/`octal`
  — two new spec format words), third-party `Text`/`Bytes` values render
  masked and capped (bytes lossy-with-U+FFFD, originals untouched), blank
  strictly means absent (`?` = present but unpaintable). Panes request the
  configured attr ids on every listing and cache the provider catalog once
  per scheme (`fs.capabilities`); the picker now OFFERS advertised
  provider columns (disabled rows, localized or masked labels) and cycles
  attr formats by hint. Column headers resolve localized → masked catalog
  label → sanitized id. `norte doctor` retires `columns-no-renderer` for
  `attr:` (plugin cells still pending) and gains
  `columns-attrs-over-cap` (>16 configured) and
  `columns-attr-id-not-wire-safe` (an id that parses but is illegal on the
  wire is skipped instead of failing remote listings). The TUI/GUI refresh
  affected panes when a picker apply or hot-reload changes the requested
  attr set; painted always equals requested. Follow-up filed: #118
  (pre-existing Ctrl+R refresh ritual gap). No wire change (additive 0.30
  contract). Sorting stays on the closed name/size/mtime vocabulary —
  sorting by attr columns is future work.

- **GUI column picker — Alt+C (#108 block 7c):** the GUI gets the same
  picker the TUI ships, as an overlay panel over the shared model: toggle
  (Space/E), reorder (Shift+↑/↓), sort by the cursor's column (S), cycle
  format (F); Enter applies in-session and persists (columns + sort +
  changed formats), Esc discards. Applying clears the session header-click
  sort override for the panes the save targets — the persisted sort
  supersedes it. Opaque ids render masked and length-capped (screen readers
  included); the panel scrim occludes mouse input; all GUI config writes are
  now serialized (follow-up for atomic persist: #116). Closes the last
  block of the columns design.

- **Provider attributes produced end-to-end (#108 block 2):** the wire that
  0.30.0 shipped now carries real data. `Provider` gains defaulted
  `attrs()`/`list_with()`/`stat_with()` (`ListOptions`/`AttrRequest`);
  producers: local (`posix.mode`/`uid`/`gid`/`nlink`/`ctime_ms` on unix,
  `win.attributes` on Windows — the #52 lazy listing stays untouched unless
  an advertised id is requested), sftp (`posix.mode`/`uid`/`gid` off the
  already-parsed SFTP attrs), object (`s3.etag`; `s3.content_type` on stat),
  archive-zip (`archive.method`/`packed_size`/`crc32`, kept even for
  encrypted entries) and the testkit `MemProvider` (hostile synthetic
  values). The daemon publishes the catalog through `fs.capabilities`,
  rejects malformed or over-16 requested ids (`-32602`), forwards only
  advertised ids and enforces emit caps per entry; paginated listings keep
  the request from the opening call. The conformance suites gain an
  attributes contract (type agreement, request scoping, caps,
  unknown-id absence). CLI: `ls --attrs <id>` (repeatable; wire-exact under
  `--json`, masked column in human output). Deferred with issues:
  `sftp.owner`/`group` names (#114), `s3.storage_class` (#115).

- **Per-column presentation — `[[ui.columns.spec]]` (#108 block 7b):** a
  spec entry keyed by column `id` sets `width` (`"auto"` / `{ fixed = n }` /
  `{ min = n, weight = m }`), `align` (`left`/`right`), `format` (size:
  `exact`/`iec`/`si`; mtime: `relative`/`iso`) and a custom `header`
  (sanitized and capped at resolve), globally or per scheme (scheme wins,
  last-wins per field). Both frontends honor the resolved style at the same
  points they already read the shared layout: custom headers replace the
  Fluent label, cells format through `styled_cell`, align picks the padding
  side, and width overrides flow through the shared layout. Every
  vocabulary is closed and validated at load (a typo is a load error naming
  the path); whether a format fits its column is a resolve-time diagnostic:
  the default is applied and `norte doctor` reports it as
  `columns-bad-spec` (masked, capped) — never a silent skip.
- **Column format cycling in the picker — `f` (#108 block 7b):** inside the
  Alt+C picker, `f` rotates the format of the row under the cursor through
  its closed vocabulary (size: `iec`/`si`/`exact`; mtime: `relative`/`iso`;
  name/kind/opaque rows have no format) and the row shows the current value
  (` · iec`). The cycle starts from the RESOLVED style of the pane's scheme,
  and Enter persists only the formats that actually changed, each as a
  replace-by-id `[[ui.columns.spec]]` entry that preserves the entry's other
  fields (header/width/align) — the session sees the new format immediately,
  in lockstep with the file. One exception: a row whose format is pinned by
  a scheme-level spec is LOCKED in the picker (cycling would write a global
  entry the scheme override keeps masking, and leak into other schemes) —
  edit the scheme spec in `norte.toml` instead. Width cycling stays
  deferred (needs numeric entry UX).
- **TUI column picker — Alt+C (#108 block 7a):** a keyboard-driven overlay
  over the shared picker model: `e`/space toggles a column on or off (name
  is pinned first and immutable), Shift+↑/↓ — or vim-style `K`/`J` —
  reorders below the pinned name, Ctrl+S applies the header-click sort
  semantics to the row under the cursor, Enter applies to the session AND
  persists to `norte.toml`, Esc discards. The save target is one rule,
  stated in the title: the pane's scheme if the config already has an entry
  for it, otherwise the `[ui.columns]` default. Non-builtin ids (attr:/
  plugin:/unparseable) appear as inert-but-editable rows, masked in the
  render, and survive a save verbatim — cleaning the user's config is
  doctor's job. Also fixed here: editing `[ui.columns]` outside now
  hot-reloads into the session (dead since block 4), and a configured
  mid-list `name` is normalized to the front (the TUI budgets the first
  width as the name).
- **Size and date in the GUI listing (#108 block 6):** the pane paints the
  same default column set as the TUI — name, size (IEC), modified (relative
  time) — from the shared layout, under a header row with the ▲/▼ sort
  indicator; the plugin-column headers (G3c) join that row over their fixed
  cells. Sortable headers (Name/Size/Mtime) are clickable: a click flips or
  switches the sort and is remembered per pane for the session, surviving
  cd — persisting it is the picker's job (block 7, with the context menu).
  Absent values stay blank (never a fabricated 0).
- **Column and sort configuration (#108 block 4):** `[ui.columns]` chooses
  which built-in columns each pane paints and the sort order — globally and
  per scheme (an override REPLACES the list; sort vocabulary is closed and
  validated at load). Both frontends seed the sort at startup and re-apply
  it when a cd lands on another scheme (the GUI consumes the sort only —
  its cells arrive with block 6). A configured column id that does not
  parse, or that has no renderer yet (attr:/plugin:), never disappears
  silently: `norte doctor` names it. Per-column width/format overrides
  (`[[ui.columns.spec]]`) land with the picker block.
- **Size and date in the TUI listing (#108 block 5):** the pane paints the
  default column set — name, size (IEC), modified (relative time) — under a
  dim header line carrying the sort indicator; absent values stay blank
  (never a fabricated 0), hostile names keep their badge and never break
  the column alignment, and the first-render budget is unchanged (~0.7ms
  for 100k entries). Column choice/config and the picker are the next
  blocks of the approved design.
- **Manual refresh — Ctrl+R (#106, beta minimum):** reloads both panes
  through the same cancellable path as the post-mutation refresh — marks
  survive by identity with visible pruning, cursor is kept by index, and
  a live-search results pane is left alone. Real directory watching stays
  tracked in #106.
- **Rename and editable destination name (#105):** Shift+F6 renames in
  place (a Move to the entry's own parent — correct inside search results
  too), and F5/F6 with a single item opens an editable destination name
  prefilled with the original; multi-item batches keep the list confirm.
  Byte-exact by rule 1: an untouched prefill copies the ORIGINAL bytes
  (never the lossy form), editing works on the displayed text, and a name
  still containing U+FFFD is rejected instead of writing mojibake.
  Collisions reuse the existing dialog; a failed submit keeps the typed
  name. TUI only — the GUI still has no text input.
- **Create directory — F7 (#104, proto 0.31.0):** `fs.mkdir` as a policy-
  gated, journalled Task (`Created` with undo; clean cancellation), wired
  end to end: engine, daemon (rpc.cancel-able), both backend modes, a TUI
  F7 dialog with the same masking discipline as the pattern dialog, and
  `norte mkdir` in the CLI. Not `mkdir -p`: the parent must exist, and any
  occupant — a directory included — is a conflict. The GUI picks it up
  when it grows text input (same explicit gap as mark-by-pattern).
- **Hidden-entry toggle (#107):** Ctrl+H (and Alt+.) shows or hides unix
  dot-entries per pane, in both frontends; `[ui] show_hidden` seeds the
  startup state. Presentation only — the provider keeps listing everything,
  and while hiding, the pane says how many entries are stashed. Hiding
  prunes marks of the entries it removes (reported, never silent), and the
  live-search results pane is exempt: a hit you asked for is never
  swallowed by the filter.
- **First-class selection (#103):** mark, mark all, invert, clear, and mark or
  unmark by glob, in both frontends; marks survive a refresh (vanished entries
  are pruned, and the status bar says how many), and copy, move, and delete
  operate on the whole selection, consuming the marks on submit. The pattern
  dialog is TUI-only for now — the GUI has no text input yet.
- **Provider attributes on the wire (proto 0.30.0, ADR 0039):** protocol-specific
  metadata — POSIX mode/uid/gid, an SFTP owner string, an S3 storage class, an
  archive member's packed size — can finally reach a client *typed* rather than
  pre-rendered, so a later block can paint it as a configurable column that still
  sorts and formats correctly. Four additive fields across three surfaces —
  catalog, request ×2, entry. `FsCapabilitiesResult.attrs` (an `AttrCatalog`)
  advertises what a provider offers (`AttrInfo` = `id`, `label`, `AttrType`,
  `AttrHint` — the declared type and the suggested format/alignment are separate,
  because two `Uint`s are painted very differently as a byte count and as a
  permission word), `FsListParams.attrs`/`FsStatParams.attrs` request the ids a
  client will actually paint (nothing is delivered unrequested), and `Entry.attrs`
  carries the values as `AttrValue` (`Uint | Int | Text | Bytes | TimeMs | Bool |
  Unknown`). Ids are namespaced by construction (at least one `.`, every segment
  starting with an ASCII letter and continuing in `[a-z0-9_-]`, ≤ 64 bytes — so
  neither the argv-shaped `-x.y` nor the float-shaped `0.0` is an id) and the
  caps — 16 requested ids per call, 64 advertised descriptors, 64-byte label,
  256-byte `Text`/`Bytes`, all counted in BYTES where the schema's `maxLength`
  counts code points — travel in the published JSON Schema (ADR 0038). The
  request surfaces are `fs.list`/`fs.stat` only: `search.hits` and
  `index.query` carry no attributes at 0.30. Wire-only for now: no provider
  advertises an attribute yet and the daemon ignores requested ids, which is a
  valid answer under the contract. `norte-proto` gains `base64` (0.22, already a vetted workspace dep) so
  `AttrValue::Bytes` owns its decode. Three properties are worth stating exactly:
  - All four fields are `skip_serializing_if`-guarded, so a **0.29 peer emits and
    receives byte-identical payloads**; the window becomes N=0.30.x / N-1=0.29.x.
  - **Any malformed attribute VALUE degrades to `AttrValue::Unknown`** — an
    unrecognised tag from a protocol-N+1 daemon (ADR 0004 applied at value
    granularity), a wrong JSON type, a `null`, two known tags at once, undecodable
    base64, an over-cap `Text`/`Bytes`. It costs one cell, never the entry and
    never the page.
  - **The two receive-side fields filter and never error**, while the two
    request fields deliberately do not. `Entry.attrs` drops a malformed key
    and bounds the map at 16 (smallest ids in byte order, so the surviving set
    does not depend on the peer's key order); `FsCapabilitiesResult.attrs` is an
    `AttrCatalog` — a newtype with a private field whose only constructor drops a
    malformed or repeated id keeping the first, clamps an over-long label on a
    char boundary, and truncates at 64, preserving the provider's own meaningful
    order. Making it a type rather than a call is what covers the EMBEDDED
    TUI/CLI path, which never crosses the deserialisation boundary where a plain
    filter would sit. A request keeps a bad id verbatim on purpose: the daemon
    must be able to answer `-32602` instead of silently laundering a caller's bug.
- **Protocol JSON Schema artifact (#13, ADR 0038):** `docs/schema/proto.schema.json`
  is now generated from the same `norte-proto` serde types that speak the wire,
  behind an optional `schema` cargo feature (off by default — the shipped crate
  gains no dependency at runtime). A golden test pins it byte-for-byte and a
  source scan guards that every schema-deriving type reaches the artifact, so it
  cannot silently drift. External clients (the MCP bridge, third-party tooling)
  get a machine-readable contract for the request/response/notification payloads.
  A standalone `just semver` recipe (cargo-semver-checks over the publishable
  crates) is staged for the gate once the binary and a release baseline exist.
- **Plugin previews mark lossy decoding (#101, proto 0.29.0):**
  `PluginPreview` and `PluginPreviewStyled` gain an additive `lossy: bool`.
  When the core's host-side text decoding (§6.2, #29) had to substitute `�`
  for invalid bytes, the viewer now shows a `[lossy decode]` marker next to
  the `via <plugin>` indicator — the same honesty the raw viewer already
  gives via its encoding status. Additive over 0.28.x (`#[serde(default)]`,
  so an N-1 peer reads it as `false`); the window becomes N=0.29.x /
  N-1=0.28.x.

- **Styled plugin previews, end-to-end (G3a):** `plugin.preview_styled`
  (proto 0.27.0, ADR 0037) is now wired from a real WASM guest through the
  daemon and both frontends. `Backend::plugin_preview_styled` (embedded:
  resolve → read → `render-styled`; remote: the wire call) mirrors
  `plugin_preview`'s resolve/read steps but treats ANY runtime failure in
  the styled render (a guest trap, a guest-side error, or a cap violation —
  `RuntimeError::StyledPreviewTooLarge`) as `Ok(None)` rather than an
  error — a styled preview is a pure enrichment over the plain one, so it
  must never block the file; the caller falls back to `plugin_preview`,
  which falls back to the raw view. The daemon handler
  (`handle_plugin_preview_styled`) mirrors that same fallback contract
  server-side. A pre-0.27 daemon is never reachable (handshake rejects it);
  within the 0.27 window a daemon that hasn't wired the handler yet answers
  `MethodNotFound` (-32601), which the remote client also folds into
  `Ok(None)`. `SpanWire::role` travels **unvalidated** across
  `norte-core` (it has no dependency on `norte-theme`, which owns the
  closed `Role` set) — validation happens once, at the frontend boundary
  that actually paints: `norte_frontend::viewer::Viewer::
  with_plugin_preview_styled` resolves each `role` string through the new
  `norte_theme::Role::from_kebab` (reuses the existing serde kebab-case
  derive as the single source of truth for role names, rather than a
  hand-duplicated table), collapsing an unrecognized name to `None` — never
  a panic, never a raw string leaking into a frontend's paint path. `role`
  wins over the raw `fg` fallback when a span carries both (the user's
  theme outranks a plugin's fixed color); every span's `text` is masked
  through the same `display_name` the ANSI-derived preview already used —
  `ansi::StyledSpan` grew a `role: Option<Role>` field shared by both
  preview paths (ANSI-SGR-derived and WIT-structured), so `draw_viewer`
  (TUI) and `render_viewer` (GUI) paint them with one code path. TUI/GUI
  viewer-open flows try the styled preview first and fall back to the
  plain one. TUI: per-span role resolves through `TuiTheme::role`
  (ratatui `Style`, falls back to raw RGB, then to the theme default). GUI:
  `styled_span_color` resolves role through `Theme::style(..).fg` (with G1
  glow applied on top, same as `entry_color`) or the raw `fg` (via
  `norte_theme::Color::rgb` + the existing `theme_map::to_gpui_rgba`, no
  parallel conversion), rendering a flex-row of per-span child divs — as a
  side effect, the GUI now also paints the pre-existing ANSI-derived
  preview in color (it shares the same `StyledSpan` type and render path),
  closing a gap noted in ADR 0037's context section. Covered by a real-WASM
  e2e (`previewer-demo`'s mini-highlighter: digits → `role: "number"`,
  `TODO`/`FIXME`/`norte` → `role: "keyword"` + a fixed `fg` — both are
  deliberately *not* valid `Role` names, proving the unvalidated-wire /
  validated-at-frontend boundary end to end) through `Backend::Remote`
  against a real daemon socket, plus a TUI buffer-inspection test pinning
  role-over-fg precedence and GUI unit tests for the pure color-resolution
  function.

- **Row decorators and plugin columns, host + backend + both frontends
  (G3b, ADR 0037):** `plugin.decorate`/`plugin.column_values` (wire-only
  since 0.27.0/G3a) now have a real handler and are painted end to end.
  New manifest `Category::Decorator` plus an (initially empty, additive)
  `contributions.decorator` — the digest follows the SAME optional-section
  pattern as `[config]`: a manifest without `[[contributions.decorator]]`
  digests byte-identical to before this change, so no existing human
  approval is invalidated by the mere existence of the new category.
  `PluginRegistry::resolve_decorators` returns **every** approved+enabled
  decorator plugin (unlike `resolve_previewer`'s first-match: multiple
  decorators can badge the same page); `resolve_columns(id)` resolves the
  one `columns` plugin declaring that column id. Both are gated on the
  primary `category` (decorator/columns each get their own dedicated WIT
  world, unlike previewer/command which share `norte-plugin`). The
  entries that cross to a guest are **basenames**, never full paths
  (`plugins::paths_to_basenames`) — a decorator/columns plugin sees a
  name, not where it lives in the tree. `Backend::plugin_decorate`/
  `plugin_column_values` (embedded + remote, daemon handlers
  `handle_plugin_decorate`/`handle_plugin_column_values` gated by the same
  read-gate as `fs.list`, extended to the whole batch) are fail-closed
  **per plugin**, never per batch: a plugin that fails to instantiate,
  traps, or breaks the positional 1:1 contract (checked by
  `decorations_to_wire_checked`/`column_values_checked`) is dropped from
  the result with a log warning — the rest of the page still paints.
  TUI: after a listing lands, a background fetch (mirroring the existing
  `Fill`/`StatProbe` one-in-flight pattern) decorates the loaded page and
  installs the result on `PaneState`; `entry_item` paints a badge span
  after the hostile-name-badge slot (role resolves through the theme,
  unstyled falls back to dim). GUI: the same fetch rides the existing
  `SessionCmd`/`SessionEvent` session channel (`Decorate`/`Decorated`,
  double guard on generation *and* dir); `render_row` becomes a flex row
  (name flex-grows and truncates, badge never does) and reuses
  `styled_span_color` for role resolution — `DecorationWire` carries no
  raw `fg`, so an unrecognized role derives a dim tone from the row's own
  color instead. Every badge is masked and capped to 8 chars *after*
  masking (`norte_frontend::sanitize_decoration`, shared by both
  frontends — the same module also flattens the wire's per-plugin overlay
  to one winning decoration per path, `merge_decorations`). Two new
  columns/decorator demo guests (`examples-wasm/decorator-demo`,
  `examples-wasm/columns-demo`) back a real-WASM e2e through
  `Backend::Remote` against a real daemon socket. **Scope note:** GUI/TUI
  column *cells* are not rendered in this change — `plugin.list`'s
  `PluginInfo` does not expose `contributions.columns` today, so a
  frontend has no wire-level way to discover which column ids exist
  without a further (additive) protocol change; that's follow-up work,
  tracked separately from this change's registry/wire/decorator-UI scope.

- **Plugin config on the wire, GUI palette + extension manager, column
  cells (G3c, ADR 0037, proto 0.28.0):** closes the two deferrals G3
  accumulated. `PluginInfo` gains `columns: Vec<PluginColumnInfo>`
  (id + masked header, additive, discovery for the column UI) and two new
  methods expose P2's `[config]` on the wire, which was host-only by
  design until now: `plugin.get_config` (schema + effective value
  together, `PluginConfigKeyWire`) and `plugin.set_config` (validates
  against the SAME schema `config.toml` uses —
  `norte_plugin_host::encode_wire_value` reuses the private
  `encode_override` validator, never a parallel path — before persisting;
  `PluginRegistry::set_config` re-resolves settings in memory so the very
  next `run_command`/`get_config` sees the new value without a fresh
  `discover`). `persist_plugin_setting_typed` fixes a latent gap in P2's
  write primitive: it writes the NATIVE TOML type (`bool`/`int`/`string`)
  the schema declares instead of always a string, which a later
  `resolve_settings` re-parse requires. `plugin.set_config` is gated to
  non-agent connections (same criterion as `plugin.set_approval`: a
  plugin's settings are user data, not something an agent edits on its
  own).
  TUI: the extension manager gains a `[config]` drill-down (Enter on a
  plugin fetches its schema and opens a panel; `bool`/`enum` cycle
  immediately, `string`/`int` open inline editing with client **and**
  server-side range validation) built on a new shared
  `norte_frontend::plugin_config::PluginConfigState`; the settings
  overlay's Plugins section drops the old "edit `config.toml` by hand"
  note and shows one row per plugin with declared settings, drilling into
  the same panel.
  GUI: `app.palette`/`app.extensions` join `crate::keymap::COMMANDS` — the
  shared presets already bound `ctrl+p`/`f12` to them, but the GUI's own
  keymap supplement had claimed `ctrl+p` for `task.prev` (a layer that
  outranks the preset), silently shadowing the binding; `task.prev` moves
  to `ctrl+b` to free it. The command palette (`palette_view.rs`, an
  overlay painted like the modal, same key-capture priority, "modal
  preempts palette" preserved by construction) and the extension manager
  (`extensions_view.rs`, a full-view swap like the settings view, with the
  same `[config]` drill-down as the TUI) are new. `norte_frontend::palette`
  hoists the TUI's `Row`/`plugin_rows`/`rows_for_context`/`first_chord`
  (pure, no `COMMANDS` coupling) so the GUI doesn't re-implement plugin-row
  masking and the `[extension]`-prefix anti-spoofing discipline from
  scratch; each frontend keeps its own `build_rows` (genuinely different
  `COMMANDS`/help-id sources, not incidental duplication). Column *cells*
  (the G3b GUI deferral) now render: a new `SessionCmd::Columns` discovers
  approved+enabled `columns` plugins via `plugin.list`, fetches
  `plugin.column_values` for every declared column over the visible page,
  and `render_row` appends one fixed-width, monospace, masked cell per
  column (TUI columns remain deferred, tracked separately). `norte-i18n`
  gains help text for the four GUI-only commands
  (`mark.toggle`/`task.next`/`task.prev`/`task.dismiss`) that the palette
  now needs to describe, and messages for the config drill-down's
  save/empty feedback, in both locales. Session tests, daemon wire tests
  (validation-then-`INVALID_PARAMS`-without-persisting, agent-denied),
  registry tests, a real-WASM e2e proving a guest reads a value written
  through `set_config` on its very next run (extends
  `plugins_config_e2e.rs`), and a `Backend::Remote`-level e2e for the new
  wrapper methods.

- **GUI settings view (S4):** `app.settings` (`F11`, same shared preset
  binding as the TUI) opens a searchable, VSCode-style full-view swap over
  the same General catalog (S2) — search box, grouped list (General/
  Plugins) with descriptions, mouse AND keyboard (click/hover to select,
  click cycles a bool/enum/theme/keymap-preset row or opens inline text/int
  editing; Enter/Esc mirror the TUI overlay). Writes persist off the UI
  thread through GPUI's background executor (no new OS thread, no coupling
  to the daemon session — config I/O has nothing to do with that
  connection's lifecycle) and, on success, re-resolve what the GUI can
  apply live from the freshly reloaded config: theme + `[effects]`, fonts
  (family/size — resolved once at startup until now, but cheap enough to
  redo on every write), reduce-motion, confirm-quit, quick-search (closes a
  pre-existing gap: the GUI had always hardcoded `Filter` mode, ignoring
  this setting entirely), and the keymap preset (rebuilds both resolvers).
  Only the UI language can't apply live in this frontend (Fluent negotiates
  it once at process startup) — that row carries a static "restart
  required" badge, and any write that couldn't apply live says so in the
  save confirmation. The pure editor state machine (search/cursor/inline
  edit, `SettingsState`) and row builder (`build_rows`) that power the S3
  TUI overlay moved to `norte_frontend::settings` unchanged (they had no
  ratatui/crossterm coupling to begin with) so both frontends share the
  exact same behavior instead of duplicating it; the TUI's own modules
  re-export the same names for source compatibility.

- **TUI settings overlay (S3):** `app.settings` (`F11` in all three bundled
  presets — `F9`/`F10`/`F12` were already taken) opens a searchable overlay
  over the General settings catalog (S2): type to filter by id, name, or
  description; Enter toggles a bool, cycles an enum/theme/keymap-preset
  setting immediately, or opens inline text/int editing (Int validates its
  range before writing — an invalid value shows a status-bar error and
  changes nothing). Every write goes through the same comment-preserving
  `norte_config::persist_set` as the theme picker, and the existing
  hot-reload picks it up live; the overlay stays open across a reload and
  refreshes its rows in place instead of closing, unlike the help/palette
  overlays. The Plugins section shows a single informational row for now:
  editing plugin settings from the UI needs a protocol bump the wire
  doesn't have yet (P2's `ConfigKeySpec`/`settings_of` aren't exposed to a
  remote frontend) — until then, edit `plugins/<id>/config.toml` by hand
  and validate with `norte doctor`.

- **`[ui] confirm_quit` (S2):** a new `norte.toml` setting controls whether
  quitting asks for confirmation — `"auto"` (default, unchanged behavior)
  confirms only with pending work, `"always"` always confirms even with
  nothing pending, and `"never"` closes immediately. Wired end to end in
  both frontends: the GUI's existing quit-confirmation modal now honors the
  three modes (and shows a generic title instead of "0 task(s), 0 mark(s)"
  when `"always"` fires with nothing pending), and the TUI gains its own
  confirmation modal on `app.quit` (Ctrl+C's emergency-exit shortcuts stay
  immediate everywhere, unaffected by this setting) — `"auto"` there
  confirms only when the task board has work in flight. A generic
  comment-preserving config writer (`norte_config::persist_set`) and a
  curated, Fluent-localized settings registry
  (`norte_frontend::settings::catalog`) land alongside it as the shared
  foundation the upcoming in-app settings UI (VSCode-style, searchable) will
  build on.

- **Per-directory cursor memory (S1):** both the TUI and the GUI now
  remember where the cursor was in each directory you visit this session
  (in-memory only, capped at 64 directories, byte-exact identity — hostile
  path twins are never folded together). Navigating to the parent directory
  now selects the folder you just came from, instead of always landing on
  the first entry.

- **GUI opt-in motion (G2, ADR 0036 amendment):** the `[effects]` theme
  schema grows to v1.1 with `flicker = { strength }` (CRT flicker, clamped
  to `[0.0, 0.15]` — deliberately tiny, an accessibility guard against
  photosensitive-trigger risk) and `cursor_blink` (bool); both render in the
  GUI. `fade_ms` (clamp `[0, 400]`) is parsed but not yet animated — a
  documented, deliberate schema/render split, not an oversight. The bundled
  `retro-crt`/`retro-crt-amber` presets now declare `flicker`+
  `cursor_blink` by default. A new `[ui] reduce_motion` config key (spec
  §17 a11y; last-wins across every layer including Project) forces all
  motion off via GPUI's native `App::set_reduce_motion`, which also frees
  `with_animation`-driven cursor blink from any hand-rolled reduce-motion
  check. The frame loop stays alive ONLY while a motion effect is active
  AND the window is focused AND `reduce_motion` is off — spot-checked with
  `NORTE_GUI_DEBUG`'s render counter (a focused retro-crt window renders
  continuously; the same theme under `reduce_motion = true`, or any theme
  with no motion keys, settles after the initial listing and goes fully
  event-driven, exactly as before this feature landed). This manual check
  is not yet pinned by an automated test — tracked as follow-up debt
  (no `gpui::test` harness exists in `norte-gui` yet to drive one).

- **Declarative per-plugin configuration (P2):** a plugin manifest can now
  declare typed settings under `[config.<key>]` (`string`/`bool`/`int`/`enum`,
  with an in-range default, an optional description, and per-type caps —
  ≤32 keys, key charset `[a-z0-9-]{1,32}`, ≤280-char strings/descriptions,
  ≤16 enum values). The schema is **inside the approval digest** (it decides
  what a plugin can be configured to do, same as `category`/`contributions`):
  a manifest with no `[config]` digests byte-identical to before P2 (existing
  human approvals are never reset), and any change to the schema — including
  just a default value — moves the digest and forces re-consent. Values live
  in `config_dir/plugins/<id>/config.toml` (flat `key = value`, validated
  fail-closed at discover time: an unknown key, a wrong TOML type, an
  out-of-range int, or a non-member enum value excludes the **whole plugin**
  from the catalog as a load error naming the offending key — never the
  value, #73) and are resolved to defaults-with-overrides applied.
  `norte doctor` gained a `plugin-config` finding per resolved key
  (`{id}: {key}={value}`, masked and capped like everything else untrusted in
  its report) for every plugin that declares `[config]`. Delivery to the
  sandboxed guest is a new WIT interface, `host-config` (`get`/`all`,
  package `norte:plugin@0.5.0`), linked for every plugin the same way
  `host-log` already is; values reach the guest through it for `command`,
  `previewer`, **and** provider guests alike, wired at every instantiation
  site (the embedded CLI/backend path, the daemon's `plugin.run_command`/
  `plugin.preview` handlers, and the previewer path in both). A provider
  guest (today, only FTP) never receives `[config]` — not an oversight:
  providers aren't discovered through the plugin manifest/catalog system at
  all, they're driven by `connections.toml`, a structurally separate config
  path with no `[config]` schema to resolve; the delivery plumbing
  (`PluginProvider::set_settings`) exists and is safe to call, ready for the
  day a provider *does* originate from a plugin manifest. The WIT package
  bump is **not** backward compatible with previously-compiled `.wasm`
  artifacts, verified empirically (not just by the pre-existing shared-package
  caveat in the WIT file's own history comment): instantiating a `command-demo`
  build from before the bump against the post-bump host fails outright
  (`component imports instance norte:plugin/host-log@0.4.0, but a matching
  implementation was not found in the linker`) — every precompiled guest,
  including the embedded `ftp-provider.wasm`, had to be rebuilt
  (`just build-ftp-wasm`). The extension manager's settings display is
  **deferred** to the wire (`PROTOCOL_VERSION`) bump G3 already requires:
  settings live host-side and the manager is wire-fed, so there is nothing to
  show there yet — `norte doctor` (which runs embedded) carries the display
  burden in the meantime.

- **`norte doctor` (H2):** read-only diagnostics over config layers,
  keymaps, plugins, and connections — `[config]` (parse errors per layer,
  and a split-brain warning when `NORTE_CONFIG_DIR` shadows a legacy dir
  with its own config files), `[keymap]` (structural errors — bad TOML,
  ambiguous prefixes, bad chords — vs. an honest per-screen approximation
  that downgrades an unrecognized layer command to a warning against the
  three bundled presets' own vocabulary), `[plugins]` (broken manifests,
  a plugin approved but whose capabilities digest went stale since —
  re-approval required — and a missing `plugin.wasm`), and `[connections]`
  (parse errors, invalid endpoints, and — side-effect-free v1 — whether the
  `NORTE_SECRET_<CONN>` env var a password/access-key connection falls back
  to is present; the OS keyring and `secrets.age` are explicitly NOT probed,
  since either could prompt or touch the keychain). `--json` emits a stable
  `{ findings, summary }` shape for tooling — locale-free and secret-free by
  construction: a `Finding`'s `detail` only ever carries machine values (ids,
  paths, var names), never the underlying library error's raw `Display`
  (a `connections.toml` syntax error inside a `password = "…` line is
  reported generically, never echoing the fragment); the handful of
  narrative sentences (e.g. "re-approval required") live in the text
  renderer, keyed by finding code, and are looked up in the user's locale.
  A layer's `lua:<name>` binding with an invalid Lua identifier is reported
  as a single structural error instead of retrying a fix that can never
  converge. Exit code is non-zero only when a finding is an error (a
  warning-only report still exits clean), and the full report always prints
  regardless of the exit code.

- **Command palette (H1, `Ctrl+P`; vim preset also `:`):** a filterable
  overlay lists every command with its Fluent description and its first
  bound chord (falling back from the browse to the viewer keymap); Enter
  dispatches the highlighted row through the exact same path a keypress
  would. Rows are precomputed from the effective keymap, like the F1 help
  and the dialog footer hints below, and refreshed on every hot-reload.

- **Plugin descriptions and commands on the wire, in the extension manager,
  and in the palette (P1, `PROTOCOL_VERSION` 0.26.0):** a plugin manifest can
  now declare an optional `description` (cosmetic, capped at 280 characters,
  outside the approval digest — editing it never resets an already-approved
  plugin's consent) and its `contributions.command` entries are exposed on
  `PluginInfo` alongside it. The extension manager (F12) shows the
  description as a dimmed second line under each plugin, masked and
  ellipsized like the rest of third-party text. The command palette
  (`Ctrl+P`) now fetches the plugin catalog on open and appends one row per
  command of every *approved and enabled* plugin, masked and tagged with an
  `[extension]` prefix that no built-in row can carry (a hostile plugin
  cannot spoof a built-in command by copying its exact display text); Enter
  runs it through `plugin.run_command` and shows the (masked, capped) result
  on the status bar. The row's internal dispatch key is never painted — a
  command id from the manifest has no charset validation of its own, unlike
  the plugin id.

- **Generated dialog footer hints (#24):** the confirm/collision/agent
  approval/host-key-trust modals and the theme picker, extension manager, and
  favorites popup now show a footer built from the *effective* `dialog`
  keymap — the join of the overlay's supported commands, the keys actually
  bound (preset plus any user layer), and a short label. Rebinding a dialog
  key can no longer desync its own hint. Notable key changes that came out of
  this: the collision dialog's "keep newer" moved from `n` to `w` — on
  collision, `n` (`dialog.deny`) is simply **inert**, not a cancel; it is not
  bound to anything the collision dialog listens for, it just no longer does
  "keep newer" by accident. The extension manager's approval toggle moved
  from a hardcoded `a` to `dialog.approve` (`y` in the bundled presets — `a`
  is now `dialog.add`, used by the favorites popup), and its `q`-to-close
  fallback was removed (`Esc` closes it, like every other overlay). The
  orthodox/cua presets also lost a hardcoded `k`/`j` fallback in the theme
  picker and extension manager: `k`/`j` now only navigate overlays under the
  **vim** preset, via its own `[dialog]` bindings, not as a blanket default.
  A later pass (encoding audit H1) found and fixed a masking gap: a hostile
  `./.norte/keymap.toml` project layer could bind a bidi-override or other
  hazardous codepoint to a supported dialog command, and that raw codepoint
  would reach the generated footer and the palette's chord column unmasked —
  both render sites now mask hazards the same way the query bar already did.
  A follow-up pass also fixed hint text that could get cut mid-word on an
  80-column overlay by dropping self-evident arrow/paging keys from
  non-modal hints and sizing the theme picker and extension manager boxes to
  their footer instead of a fixed width.

- **Content-match preview in live search (#81):** with a content search
  active, the status bar shows the line number and a sanitised preview of the
  match for the hit under the cursor.

- **Show names as… (#57):** `Alt+E` cycles a per-pane reinterpretation of
  non-UTF-8 file names for display (cp437, cp866, Shift-JIS, GBK,
  windows-1252), with a chardetng suggestion as the first step. Display only:
  bytes never change, reinterpreted names keep their hostile badge, and the
  status bar shows the active mode persistently. Valid UTF-8 names are never
  reinterpreted. Quick search matches against the reinterpreted text (typing
  "П" finds the entry shown as "Папка"), and decision surfaces — confirm and
  collision dialogs, viewer title, navigation popups, the search dialog root —
  follow the pane's active reinterpretation (#98).

- **Omitted-entries badge for archives (#93, protocol 0.22.0):** listings of
  zip/tar/tar.gz containers now report how many entries the index omitted
  (hostile names, anti-bomb limits) through the new optional
  `FsListResult.skipped` field. The TUI shows a persistent status-bar badge
  ("N entries omitted") and `norte ls` prints a warning to stderr — an
  incomplete listing is never silent.

- **Configurable archive limits (#95):** the new `[archive]` section of
  `norte.toml` (`max_entries`, `max_decompressed_bytes`) lowers the anti-bomb
  limits for browsing containers. The project layer (`./.norte`) is ignored
  for this section — a foreign repository must not be able to raise safety
  limits.

- **Themes (MT milestone, ADR 0020):** the TUI now uses the shared
  `norte-theme` crate. It provides semantic roles, true-colour values with
  256- and 16-colour terminal fallbacks, styles by node type and extension, and
  bundled presets (`default`, `catppuccin-mocha`, `gruvbox-dark`, and `nord`).
  Select a preset or a custom TOML file with `[ui].theme`. The setting is hot
  reloaded and falls back to `default` on error. An `[effects]` section is
  reserved for the GPU-backed GUI. See [the theme guide](docs/theming.md).
- **Light themes and explicit backgrounds:** added `gruvbox-light` and
  `catppuccin-latte`, plus a `background` role so both light and dark themes
  control the terminal's base colour.
- **Theme picker:** press `F9` to preview bundled themes. Enter applies and
  saves the choice to the user's `norte.toml` without discarding comments or
  formatting; Esc restores the previous theme.
- **Roadmap update:** after M2, the planned order became MT (themes), M4
  (plugins), M3 (agent integration), and M5 (GUI).

### Changed

- **Streaming prefix rename on object storage (#49):** renaming an S3 prefix
  no longer materialises the whole tree in memory (peak is now proportional
  to the number of directories) and deletes in batches (`DeleteObjects`).
  The operation stays non-atomic: an object created concurrently under the
  source prefix during the rename is left unmoved; a concurrent overwrite of
  an already-copied object can be lost.

- **Honest resource errors for archives (#95, protocol 0.23.0):** a container
  that exceeds a local anti-bomb limit now fails with the new `limit_exceeded`
  error (closed vocabulary: `entries`, `decompressed-bytes`) instead of
  masquerading as `corrupt` — a legitimate huge tar.gz is not "corrupt".
  Older clients degrade to a generic error.

- **Listing sort keys allocate less (#94):** the persisted NFC sort key is
  only materialised when it differs from the raw name bytes (non-ASCII NFD
  names); ASCII, already-NFC, and non-UTF-8 names no longer allocate.

### Added

- **`norte tui` and `norte gui`:** the CLI now launches either frontend,
  handing the process over (unix `exec`: same pid, same terminal, same
  signals) and preferring the binary installed next to itself over
  whatever the `PATH` finds first. Arguments pass through verbatim.

- **`norte-gui` takes a starting directory** too, plus `--socket`,
  `--help` and `--version`. Command line beats `NORTE_DIR`/`NORTE_SOCKET`,
  which beat the current directory and the daemon's own socket — resolved
  in one place (`LoadConfig::resolve`), with no `set_var` detour. The
  binary also stops reporting version `0.0.0`.

- **`norte-tui` takes a starting directory** and real `--help`/
  `--version`. The positional argument used to be the keymap preset,
  which nobody guessed; it is now the directory to open, with the preset
  behind `--preset`. An unknown flag is named and refused instead of
  being silently ignored — `--help` used to fall into that branch and
  the binary died trying to take over a terminal.

### Fixed

- **The F1 help no longer disappears, and neither do the columns
  (bugfixing session):** seven defects that only showed up by driving
  the real app.
  - Overlays are painted over the viewer. `ui::draw` returned right
    after the viewer, so any overlay opened on top of it stayed
    invisible while still swallowing every keystroke — the run loop
    routes them before the viewer, and `f1 -> app.help` is a `[global]`
    binding, so it is live on the viewer screen too.
  - An in-flight modal wins the key over every overlay, not just the
    palette and the settings pane. The modal is painted last, above
    everything, but the key chain resolved first against the theme
    picker, columns picker, extension manager, nav popup, search dialog
    and help — so a keystroke aimed at the modal landed in a text field
    or toggled the highlighted plugin.
  - The transfer-name modal elides its paths in the middle instead of
    letting the box border cut them, which used to expel the tail of the
    destination with nothing to signal it.
  - A cursor at the top stays at the top while a paginated listing
    fills. It was re-anchored to the path under it, and the first page
    of a local listing arrives in readdir order, so a 5000-file
    directory opened showing its tail.
  - Size and Date are hydrated for every visible row in both panes
    (TUI and GUI, #123), not just the focused one — with the lazy local
    listing (#52) those columns were otherwise blank.
  - Paging and the stat probe use the real viewport height (#124)
    instead of a fixed 10 rows and a fixed radius.
  - Config hot-reload only fires for `norte.toml`/`keymap.toml`/
    `openers.toml`. The native watcher can only watch a directory and
    forwarded every event, so `index.db`/`journal.db` writes — SQLite,
    in the same directory — reloaded the config and closed the open help
    and palette. Read events (`Access`) are ignored too: the reload
    re-reads the layers, so counting an open as a change fed the cycle.
  - Text inherits the THEME's foreground. Only the background came from
    the theme, so every span without an explicit `fg` (including the
    column cells and header, painted with a bare `DIM`) kept the
    TERMINAL's foreground: with a light theme in a dark terminal they
    were painted almost in the background color. Two tests over every
    shipped preset now pin this and a 3:1 floor for text.

- **Semantic signals reach WCAG AA in every preset:** `error`, `warning`
  and `hostile-badge` — the last one marks a masked name (spec §6), a
  security surface — fell as low as 2.31:1 against their own theme
  background. Adjusted in `catppuccin-latte`, `gruvbox-light`, `nord`
  and `gruvbox-dark`, keeping each palette's hue, and pinned by a test
  over all presets. Decorative roles (borders, status bar accents) keep
  their looks and their lower floor.

- **Dialog keys are now discoverable in-app (#113):** the F1 help gains a
  "Dialogs and overlays" section built from the effective `dialog` keymap —
  the same generated-from-config invariant as the other sections. Overlay
  footers stay space-filtered (arrows and, in the columns picker, the
  reorder verbs are dropped to fit 80 columns), but every dialog verb and
  its real chord is now listed somewhere reachable without opening the
  manual; a note clarifies each dialog supports its own subset.
- **Config writes are now atomic and cross-process safe (#116):** every
  `norte.toml` persist helper (theme, settings, columns, formats, hotlist)
  used to read-modify-write the file in place — two writers (e.g. the GUI
  and the TUI on the same config) could interleave and silently drop each
  other's changes, and a concurrent reader could catch a truncated file
  whose partial parse a later write would then rewrite, losing unrelated
  user sections. Writers now take a cross-process advisory lock
  (`norte.toml.lock`, held for the whole read-modify-write; released by
  the OS even on crash) and replace the file via a synced sibling tmp +
  atomic rename, preserving existing file permissions — readers see the
  old or the new file, never a torn one. Per-plugin `config.toml` writes
  got the same treatment (#119).
- **Ctrl+R skipped the post-refresh ritual (#118):** `pane.refresh` ran from
  the command dispatcher, which cannot see the run loop's paginated fill or
  the stat-probe dedup — with a large listing still streaming in, the old
  drainer kept appending batches onto the freshly refreshed pane
  (duplicated rows until the next cd) and the focused entry could refuse to
  re-hydrate its lazified size. The refresh outcome now travels back to the
  run loop (`Cd::Refreshed`), which applies the same ritual as
  mutation-completed refreshes: the drainer is released only for panes that
  truly got a complete listing (an Esc mid-refresh keeps the other pane's
  still-valid fill), and the probe dedup is invalidated.
- **Palette and quick-search dispatch sites missed the resolver's
  post-command tail (#118 review):** a cd chosen from the command palette
  (or a quick-search Enter landing in a hit directory) could exit a pane's
  virtual search mode without cancelling the live search task, and a
  `pane.open` picked from the palette left the resolved external command
  queued until the next keypress. Both sites now reap the search run; the
  palette also launches the pending opener immediately.
- **Daemon plugin previews skipped host-side text decoding (#101):** the
  `plugin.preview`/`plugin.preview_styled` daemon handlers passed RAW bytes
  to the previewer guest, unlike the embedded backend, which decodes to text
  first (§6.2, #29) — a latent behavior gap between embedded and daemon
  mode. Both handlers now decode through the shared
  `plugins::decode_for_preview`, matching embedded and surfacing the new
  `lossy` flag.
- **`norte doctor`'s keymap check used an O(n) retry loop (#102):** unknown
  `run` names were discovered by rebuilding the effective keymap once per
  distinct typo (capped at 256), a loop that could never converge for a
  `lua:<name>` binding failing the charset and had to special-case it. It is
  replaced by a single-pass `Effective::build_diagnostics` that reports every
  finding at once — no retry, no cap, no non-convergent case.
- **`persist_set` panicked on a malformed `[section]` (S review I1):** a
  hand-edited `norte.toml` with a scalar section (`ui = 3`) or an
  array-of-tables (`[[ui]]`) made the settings-write primitive panic instead
  of returning an error — reachable in both frontends' background write
  task (GUI: could take the process down; TUI: the panic was swallowed
  silently, leaving the settings row optimistically showing "edited" even
  though nothing was written). Now a clean `io::ErrorKind::InvalidData`; the
  TUI's previously-silent panic arm now shows a status message too.
- **Settings paint order (S review M3):** a modal (e.g. an async policy
  approval) painted UNDER the TUI's command palette or settings overlay
  when both were open, even though key input already treated the modal as
  authoritative — the pixels lied about who was in control. The modal now
  paints last, on top of every overlay.
- **`nav.parent`'s cursor-memory hint could survive a failed `cd` (S review
  M2):** landing back on the child you came from only worked after a
  *successful* navigation; a failed one (permission denied, a dead session)
  left the hint set, ready to hijack an unrelated future navigation's
  cursor placement. Both frontends now clear it on failure.
- **`ui.font-size` couldn't be edited to a fractional value (S review M4):**
  the settings UI's `Int` editor only accepted whole numbers, even though
  `[ui] font_size` is a float — a hand-set `14.5` was invisible to the
  editor (typing it back always failed). It now accepts a fractional part
  and round-trips it.
- **Silent short reads from zip entries (#95):** a zip whose central directory
  promises more bytes than the deflate stream delivers now fails loudly with
  `corrupt` mid-stream instead of silently returning a partial file.

## [0.3.0-alpha.1] - 2026-07-15

The first tagged release completes the M2 milestone: remote providers and
archives. norte can manage local files, remote storage, and compressed archives.
This is an alpha release; the interface and configuration may still change,
and some daemon/socket tests are only available in CI environments.

### Added

#### M2: remote providers and archives

- SFTP provider based on `russh`, with accurate capabilities, byte-safe names,
  and containment for hostile names and symlinks.
- Object-storage provider based on `opendal`, with server-side S3 copies,
  cursor pagination for large listings, and byte-exact UTF-8 keys.
- Read-only ZIP and TAR provider that exposes archives as virtual directories
  (`zip+...!/path`), honours ZIP filename encoding, and enforces zip-bomb
  limits.
- Cross-provider copy engine with resumable `.norte-partial` files, multipart
  S3 support, and destination-side overwrite protection.
- Remote logical trash at `.norte-trash/` for providers without an operating
  system trash facility, including byte-safe origin metadata.
- JSON-RPC 2.0 daemon over Unix-domain sockets or Windows named pipes, with
  peer-credential authentication, NDJSON framing, automatic startup, and idle
  shutdown. Frontends can use embedded or daemon mode.
- Connection profiles and secret handling through `connections.toml`, system
  keyrings, and trust on first use for SSH host keys.
- End-to-end coverage of the release criterion (remote ZIP to S3 to local),
  framing and ZIP-name fuzzing, copy benchmarks, and nightly tests using
  testcontainers.

#### M1: usable terminal interface

- A ratatui dual-pane TUI, configurable keymaps, layered hot-reloaded
  configuration, an encoding-aware viewer, and explicit fallback when trash is
  unavailable.
- Fluent localization resources for English and Spanish.

#### M0: foundation

- Cargo workspace, protocol and VFS crates, byte-preserving `VPath`, cancellable
  task scheduling, local copy/move/delete with progress, and CI on three
  operating systems.

### Notes

- Filenames remain bytes throughout the stack. The canonical `norte-testkit`
  corpus covers hostile and non-UTF-8 paths.
- `norte-proto`, `norte-vfs*`, and `norte-testkit` are available under either
  Apache-2.0 or MIT. `norte-core` and the official frontends are
  AGPL-3.0-only.

### Planned

- Agent-facing MCP server, policy engine, journal, session undo, and audit
  export.
- Writes inside ZIP archives; list, restore, and purge operations for logical
  trash; and the M5 GUI.

[Unreleased]: https://github.com/compilando/norte/compare/v0.3.0-alpha.2...HEAD
[0.3.0-alpha.2]: https://github.com/compilando/norte/compare/v0.3.0-alpha.1...v0.3.0-alpha.2
[0.3.0-alpha.1]: https://github.com/compilando/norte/releases/tag/v0.3.0-alpha.1
