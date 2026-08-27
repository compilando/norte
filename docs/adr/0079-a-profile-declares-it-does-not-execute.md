# 0079 - A profile declares, it does not execute

- Status: accepted
- Date: 2026-08-26
- Decision makers: Oscar González
- Related: ADR 0007 (configuration layers), ADR 0035 (the config crate and its
  precedence), ADR 0026 (project layer trust), ADR 0058 (a screen is a tree the
  core keeps and does not read), ADR 0059 (the UI session), spec
  `docs/superpowers/specs/2026-08-26-config-profiles-design.md`, issues #304,
  #305.

## Context and problem statement

norte had three configuration layers — system, user, project — and exactly one
screen. Every ingredient of a workspace already existed and none of them could
be tied together: five factory arrangements plus `layouts/<name>.toml`, seven
keymap presets, a theme, columns, favourites, and panel state held by the
daemon. Picking the `norton` arrangement changed not a single key, and the
layout picker carried a field whose only job was to say so out loud —
`Row::shares_keymap_name`, documented as "two distinct settings that share a
name, and without the line the coincidence is a trap rather than a
convenience". That field was an apology for a missing feature.

ADR 0058 had left the hole and named it: `layouts: Map<ProfileId, Layout>`,
annotated "one entry in v1". The map shipped, with exactly one key,
`"default"`.

This record covers what a profile is, where it sits among the layers, what it
may and may not decide, and the two things the implementation got wrong in its
first pass — because both were the *same* mistake in two different files, and
that is the part worth remembering.

## Decision

**A profile is a configuration-layer directory** under the user's own config
directory, `profiles/<name>/`, with the shape of any other layer:
`norte.toml`, `keymap.toml`, `openers.toml`, `layouts/`. It is a layer because
`Layers.dirs` is already `Vec<(PathBuf, Layer)>`, so this is one more entry in
that vector rather than a new mechanism, and a profile can carry its own keymap
and its own arrangements without polluting the user's.

**Precedence is `system → user → PROFILE → project`.** Picking a profile
overrides the reader's own `norte.toml` — that is what picking it is for — and
a trusted project layer still wins over it, so ADR 0026 and #260 keep their
meaning exactly.

**A profile's live screen state is `SessionBody.layouts`, keyed by profile
name**, with slot ids allocated per profile so a flat `slots` map holds several
profiles' panels with no schema surgery. The active profile is `SessionBody
.active`: state, not configuration — what the reader was doing, not what they
decided — so the daemon holds it and their `norte.toml` stays a file they
wrote. This costs no protocol bump: the session body is opaque to the core and
its version lives in `norte-frontend`. It does cost a bump of **both** schema
numbers, which is the subject of a consequence below.

### What a profile may decide, in the positive

A profile MAY set the whole of `[ui]`, `[keymap] preset` with its own
`keymap.toml`, `[[hotlist]]`, its own `openers.toml`, its own `layouts/`, and a
new `[profile]` section carrying a display title and a per-slot starting
directory.

A profile MAY NOT set `[daemon]`, `[log]`, `[ai]`, `[archive]`, anything
touching the policy engine, or **an `init.lua`**. What it declares and cannot
have is warned about by file and key; it is never ignored in silence.

Connections are in neither list because they are not configuration: the daemon
owns them (ADR 0074).

### The line, and why it is where it is

**A profile declares, it does not execute.**

Everything granted above is a file the reader can open and understand — a
theme, a keymap, a set of openers, an arrangement, a list of favourites.
`openers.toml` stays granted although it names external programs: it is a
declaration readable in full, its directory is confined to the reader's own
config tree, and a profile that could not choose how files open would not be a
workspace. `init.lua` is the other kind of thing entirely — the whole host API
with the whole Lua standard library behind it, `os.execute` included, in a host
that is deliberately unsandboxed (ADR 0026).

The reason the line matters *here* and not for the other three layers is one
sentence: **a profile is chosen from a list while the program is running.** A
configuration layer is edited; a profile is picked. A picker that grants in
silence is a permission escalator.

### A broken profile has three answers, not one

`parse_layer` is explicit that user and system layers are fatal — "those ARE
yours, and starting while ignoring them silently would be worse than not
starting". A profile is the reader's own file, so a blunt "always degrade"
would contradict a rule written on purpose. But a profile is also the only
layer chosen from a picker mid-session, and aborting a running norte over a
typo in a directory the reader merely browsed to is worse still.

- `--profile <name>` that does not load → **fatal**, naming the file. The
  reader asked for that profile; starting as something else answers a different
  question.
- The sticky profile from the session → **start with no profile layer, loudly**.
  Nobody asked for it this run, and aborting would trap the reader outside the
  program with no way to pick another.
- Switching to it at runtime → **refused**, current profile untouched. A
  half-applied profile is not a state this design admits.

A failure in a layer that is not the profile's is fatal for all three: the
degrade path loads without the profile first, so a broken user `norte.toml`
surfaces as itself rather than being blamed on the profile.

### A profile name is checked before it is joined

A name reaches the filesystem through `profiles_dir.join(name)`, and
`Path::join` with an absolute path — or a Windows drive prefix — replaces the
base entirely, while `..` climbs out. A name is refused unless it can be a
single directory entry, and it must then appear **byte-for-byte in the listing**
of `profiles/` before it is used, because letting the filesystem resolve it
opens `work` when `WORK` was asked for (#245). The repository already had that
check for layout filenames; the canonical copy moves down into `norte-config`
and the layout loader defers to it.

## Consequences

**The negation is the lesson, not the feature.** The project layer's carve-out
was written as `*kind != Layer::Project`. Adding a fourth `Layer` variant
therefore granted it, silently, everything the user layer could do — and it was
granted by an `enum` variant rather than by a line anyone wrote. The same
negation existed a second time, in the Lua loader (`layer == Layer::Project`),
where it granted arbitrary code execution, and both reviewers found it in the
same place. Both are now exhaustive `match`es over `Layer`, so a fifth variant
does not compile until somebody decides which side it falls on. **Any predicate
over `Layer` in this codebase should be written in the positive.**

**Two schema numbers move together.** `norte_frontend::session::SCHEMA_VERSION`
decides whether a body can be read; `norte_core::ui_session::disk::SCHEMA_VERSION`
decides whether the file can be overwritten. They live in different crates
because the core cannot depend on the frontend, and nothing ties them at
compile time — bumping only one leaves the core refusing its own file at every
start, forever, behind a `warn!`. Both go to 2. The cost is a real downgrade
cliff: a session written by this build is from-the-future to the previous
release, which refuses to read it *and* refuses to overwrite it. That is the
correct behaviour and it is bought before P3 delivers the benefit.

**Profile state is capped at four, from a measurement.** Four profiles of eight
slots with full history in both directions serialize to 295 567 bytes against
`SESSION_BODY_MAX`'s 1 048 576. Over the cap, the least-recently-activated
profile's state is dropped whole; its configuration directory is untouched, so
the profile still exists and starts from `[profile.start]`. The active profile
is never swept. Measuring this surfaced #304: `ORPHAN_CAP` alone can already
exceed the envelope, which predates profiles entirely.

**Cross-profile slot ids are an allocator invariant, not a schema one.** A body
arriving from disk can share ids between arrangements — `from_value` validates
each tree separately and the body is opaque to the core — so pruning must
delete against what survives rather than against the tree it just removed.
Otherwise dropping a stale profile takes the *active* one's panels with it.

**#305 is closed by P3, and both halves confirmed the shape of the problem.**
D7 turned out to be a rule about a *layer*, not about `norte.toml`: a profile
also carries `keymap.toml` and `openers.toml`, both fatal outside the project
layer, so answering the three-way question over the scalars alone declared a
profile with a typo'd shortcut healthy and let it abort the program afterwards.
The rule is now generic over what loading a layer means, so it still lives in
one place. And the shortcut editor's write target now comes from the same cut
that decides it, rather than from a separately resolved user config directory —
which, after D10 moved the target, would have written where the active profile
shadows it.

**The three sources of D7 do not share one path**, and P3 is where that
surfaced. An explicit `--profile` is known before anything connects, so it
enters the first configuration load and applies everything, `ui.lang` included;
the sticky profile arrives with the session and can only switch hot. The
design's single "what could not be applied" list belongs to the sticky path
alone, and it has exactly one entry.

## Alternatives considered

**One file per profile** (`profiles/<name>.toml`, symmetric with
`layouts/<name>.toml`) can only *name* a theme or keymap that already exists in
the user's directory. Too thin for a workspace.

**A section per profile** (`[profiles.work]` inside `norte.toml`) makes one
file grow without bound, teaches the `toml_edit` settings writer nested paths
it does not need, and makes "copy this profile" stop being a copy of anything.

**A profile as the highest layer**, above project, was rejected because it
would change what a trusted repository can do — a question ADR 0026 already
answered, and one this feature has no business reopening.

**Sharing slot ids across profiles** and keying state by `(profile, slot)` was
rejected as a schema migration for something the allocator can guarantee for
free.
