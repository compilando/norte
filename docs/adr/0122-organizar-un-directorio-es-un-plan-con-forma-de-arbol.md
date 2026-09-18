# 0122 — Organizing a directory is a plan shaped like a tree

- Status: accepted
- Date: 2026-09-18
- Decision makers: Oscar González
- Protocol: 0.76.0 → **0.77.0** (`ai.organize_plan`, `plugin.organize_plan`,
  `fs.organize`, `PluginCommandKind::Organizer`,
  `AiOrganizePlanResult::plan_hash`)
- Bridge: 71 → **72** (`OrganizeView`, `organize_decide`, `organize_scroll`)
- WIT: new package `norte:organizer@0.1.0`
- Related: ADR 0095 (a `renamer` plugin proposes, the core renames), ADR 0077
  (the same command means the same thing in both frontends), ADR 0089 (the
  RPC catalogue), ADR 0121 (phase 7), spec
  `docs/superpowers/specs/2026-09-15-historia-y-wow-design.md` (phase 8)

## Context and problem statement

norte could already propose a rename plan for a whole directory and let a
human approve it. What it could not do is the thing people actually ask a file
manager for when a `Downloads` folder has eight hundred files in it: **put
them into folders**.

That is not a rename with a longer name. A rename stays inside one directory,
and the whole safety argument of `ai.rename_plan` rests on that: the proposed
name is a single `Segment`, so a plan cannot walk out of the directory it was
asked about. Organizing needs the destination to carry subdirectories, which
means the operation also CREATES things, and creating things has its own
reversal, its own failure modes and its own way of going half-done.

So it is a different operation, not a wider field on an existing one.

## Decision

**1. Three methods, one plan.** `ai.organize_plan` and `plugin.organize_plan`
PROPOSE; `fs.organize` APPLIES. The first two mutate nothing — the plan is the
product. The split is the same one ADR 0095 made for renamers, and for the
same reason: what makes the operation safe is not where the names came from,
so a model's plan and a plugin's plan land in the same review and are applied
through the same door.

**2. `fs.organize` is one method and not N client calls.** It creates the
missing folders and moves the files, all under ONE `batch_id`, so it undoes as
a unit. `fs.create` plus `fs.move` from the client would leave a batch nobody
owns: cancel it halfway and you have folders with nothing in them and files
in two places, with no single thing to undo.

**3. The undo is `revert_sync_batch`, not `revert_batch`.** This is the part
that is easy to get wrong. `revert_batch` assumes every entry of the batch
shares one directory — an organize batch does not, by construction. The
existing sync-batch executor already handles "no common directory, one
provider, LIFO", and LIFO happens to be exactly right here: the folders were
created first, so they are deleted last, by which time the files have already
moved back out of them and the folders are empty. The operation gets its own
`OP_ORGANIZED` so the dispatch can tell it apart.

**4. A destination that escapes kills the WHOLE plan.** `validar_proposed_rel`
rejects an empty relative path, an absolute one, one deeper than
`ORGANIZE_MAX_DEPTH`, and any segment that is not a legal `Segment`; the core
additionally rejects a duplicated origin or a duplicated destination. One bad
move rejects the plan entire, and that is deliberate: a plan is an intention a
human approves in one go, and applying "the part that was fine" of a proposal
that contained a `..` is applying something nobody reviewed.

**5. The token travels WITH the plan.** `AiOrganizePlanResult` carries the
`plan_hash`, unlike the batch rename, where the hash comes from a second call
(`fs.rename_batch_plan`). That second call exists there because it also checks
for collisions and decides whether the batch is applicable; here there is
nothing to check beyond the plan's shape, and the core already did that in
order to propose it. A second round trip would only add a window in which the
human is looking at a plan that cannot yet be approved. It also keeps the
digest in ONE place: computing it in the client is how the two ends stop
agreeing and `fs.organize` starts answering `PlanStale` to a plan nobody
touched.

**6. The review is a TREE, in both frontends, from one shared model.**
`norte_frontend::organize::tree_lines` turns the moves into lines with a
depth and a kind (new folder / existing folder / moved file), and both the
terminal and the window paint those lines. Which folder is NEW cannot be
decided twice: it is the difference between "this creates three folders" and
"this puts things into folders you already had", and a reader deciding on the
wrong one of those is the whole risk of the screen.

Above the tree goes the count — "creates 3 folders and moves 12 files" —
because it is what you read to decide without counting lines, and because a
box taller than the terminal is cropped from the BOTTOM.

**7. The kind is painted twice, and neither is the color alone.** The terminal
gives each line a role (`Strong` for a new folder, `Dim` for one that existed)
AND a marker glyph; the window gives it a CSS class AND a marker element. A
monochrome theme, a screen reader, and a `--no-color` terminal all survive
that; a color alone survives none of them. The marker is never concatenated
into the name, and the indentation is a style variable rather than spaces in
the text: otherwise a file called `+ facturas` disguises itself as a new
folder, and one with leading spaces looks like it lands deeper than it will.

**8. Approving requires having reached the end** (`approval_ready`), with
scroll in both surfaces — and in the window, a mouse gesture too. The review
is the only defense against a plan written from names an attacker controls,
and a window of ten lines out of a hundred covers a tenth of it. The window
gets `organize_scroll` because otherwise that same requirement made the screen
unapprovable for a reader without a keyboard.

**9. An organizer plugin is a `PluginCommandKind`, not a new list.** It rides
in `PluginInfo::commands` with `kind: "organizer"`, the way a renamer already
does, because it answers the same question a human asks of a plugin — "what do
you offer me" — and a second list would be a second place to look. The palette
labels it and dispatches it to `plugin.organize_plan`. The wire exposure is
exactly the one 0.67.0 already accepted for `renamer`: the field is omitted
when it is `command`, so an older peer only ever sees the value if the plugin
really declares one.

**10. No key in any preset**, like `pane.ai-rename`. None of the four imported
managers attests a command like this, and inventing a chord for all seven
would be inventing history. It lives in the catalogue, the menu, the palette
and the help, in both locales.

## Consequences

**Good.** The expensive half was already built and reused: the journal, the
policy gate, the task, the undo executor, the review discipline and the
plugin host's instantiation path all took an organizer without new machinery.
A plan from a plugin and a plan from a model are literally indistinguishable
downstream, which is what makes "the plugin proposes, the core executes" hold
instead of being a slogan.

**The cost.** `PluginCommandKind` grew a variant, and an N-1 client that
receives `kind: "organizer"` fails to deserialize that `PluginInfo`. That is
the same exposure 0.67.0 took knowingly, and it only materializes for a plugin
that actually declares an organizer.

**What is not done.** Nobody has run this against a real model yet: this
machine has no AI provider configured, so the AI path is exercised through the
engine's contract and its validator, and the end-to-end evidence comes from
the `by-extension` test plugin, which is a real wasm guest running through the
real host. A plan proposed by an actual model over hostile filenames is the
first thing to try when a provider is available.

## Alternatives considered

**A field on `ai.rename_plan`.** Rejected in the ADR's first paragraph: it
shares a name with the existing operation and nothing else — different
reversal, different dialog, different way to fail.

**Letting the provider create the folders.** That is rule 9 and rule 4 at
once: the plugin does not touch the filesystem, and the core has to know what
it created in order to undo it.

**Computing the `plan_hash` client-side.** It would have avoided a protocol
field and put the digest algorithm in two places. See decision 5.

**A separate `organizers` list in `PluginInfo`**, like `columns` and `panels`.
Strictly more additive on the wire. Rejected because a renamer — the closest
twin, and a producer of the same kind of reviewable plan — already rides in
`commands`, and splitting the two would have made "what does this plugin
offer" a question with two answers.
