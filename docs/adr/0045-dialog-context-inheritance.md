# 0045 - norte's dialogs stay norte's: `dialog_from`, one level, presets only

- Status: accepted
- Date: 2026-08-09
- Decision makers: Oscar González
- Related: specification section 12 (keymap); ADR 0006 (keymap resolution and
  layer semantics — **extended**: `keymap.toml` gains a key), ADR 0043 (keymap
  availability), ADR 0044 (counts and sacred keys). Design:
  `docs/superpowers/specs/2026-08-09-keymap-catalogue-and-presets-design.md`;
  plan: `docs/superpowers/plans/2026-08-09-k2b-four-presets.md`.

## Context and problem statement

K2b ships four imported presets — Total Commander, Krusader, Norton Commander
and Far. Each one is a faithful transcription of a *file manager's panel keys*:
what F5 does, what `Alt+F7` does, which key marks a file.

None of the four originals has norte's overlays. There is no Total Commander
"approve this agent operation" modal, no Far "a file exists at the destination:
overwrite / skip / rename / newer" resolution dialog with a keymap the user
could recognise. The `[dialog]` context is a norte concept, invented for issue
#24, and it is where confirmation, approval, overwrite resolution and the
navigation popups live — 25 bindings in `orthodox.toml` today.

So every imported preset needs a `[dialog]` section, and the honest content of
that section is *norte's*, identical for all of them. The question is how each
preset gets it.

This matters more than it sounds. A preset that ships with an empty `[dialog]`
is not a preset with fewer keys: it is a preset where every modal is a room
with no door — `Esc` does not cancel, `y` does not approve, and the user's only
recourse is to kill the process. And the alternative that suggests itself
first, copying the block, means the answer to "what cancels a dialog?" is
stored in seven files that nothing forces to agree.

## Options considered

**A. Copy the `[dialog]` block into every preset.** Zero engine change, and
each file is self-contained: what you read is what you get.

Rejected. Seven files (three today, seven after K2b) each holding the same 25
lines is 175 lines whose only invariant is that they are identical, with
nothing checking it. And the drift is not hypothetical: the three sections that
exist today already disagree — `orthodox` has 25 bindings, `cua` 25, `vim` 28
(vim adds `j`/`k`-style movers over the same 22 distinct commands). That is why
`orthodox`, not `vim`, is the source the four imports name: it is the plain one,
with no preset's house style layered on norte's overlays. The rest of the
failure is a *scheduled* edit: `dialog.*` has grown three times already (the
approval modal, the
overwrite resolution, the navigation popups), and each growth would become a
seven-file change where forgetting one file produces a preset in which one
overlay key silently does nothing. That is precisely the "a key that does
nothing and says nothing" failure K1 spent its whole budget removing.

**B. A general `inherit = "orthodox"` that adopts every section.** The obvious
generalisation, and it would also let a preset be written as a small delta.

Rejected, and this is the important rejection. A Total Commander preset that
inherited orthodox's `[pane]` would bind `j`/`k`, `Ctrl+P`, `/` and every other
key orthodox happens to define — keys Total Commander never had. The user
pressed a key, it did something, and no `Unavailable` message can correct it
because nothing went wrong: the binding is real. That is the fidelity failure
the whole imported-preset feature exists to prevent, arriving through the
inheritance door. **Fidelity is a claim about what is NOT bound**, and general
inheritance cannot make that claim.

**C. `dialog_from = "<preset>"`: one key, one section, one level.** Chosen.

## Decision

`KeymapFile` gains an optional `dialog_from: Option<String>`, resolved by
`parse_keymap` immediately after the TOML parses: the named preset's `[dialog]`
section is copied in, and every later reader sees an ordinary `KeymapFile` that
need not know inheritance exists.

Four rules, each a load error rather than a silent choice:

1. **One section only.** `dialog_from` moves `[dialog]` and nothing else.
   Option B's failure is a design constraint, not an omission to be fixed later.
2. **Presets only, and refused before it is resolved.** Parsing splits in two:
   `parse_keymap` is the PRESET door and resolves the key; `parse_keymap_layer`
   is the layer door — the one `config::load_keymap_layer` uses for every real
   user and project layer — and it *refuses* the key without copying anything.
   The order matters more than it looks: `load_keymap_layer` rejects a layer
   that declares a full `keymap` list, and it decides that by reading
   `dialog.keymap`. Resolve first and a user who wrote `dialog_from` is told
   they used `keymap`, a key they never touched. `check_layer_keys` keeps the
   same refusal as a second lock for a `KeymapFile` built some other way — a
   test, a frontend with an embedded layer literal.
3. **One level, no chains.** The named preset may not itself declare
   `dialog_from`. Resolution therefore calls a private `parse_raw` rather than
   recursing into `parse_keymap`: there is no depth to bound, and a
   self-referential `dialog_from` cannot hang the loader because there is no
   loop. That property lives in one call and would survive no review on its
   own, so a test asserts the self-reference case directly.
4. **Not both.** A preset declaring `dialog_from` *and* a non-empty `[dialog]`
   (in any of the three lists, not just `keymap`) is an error: two answers to
   one question, and picking one silently would leave a user with an overlay
   context they did not write. The message names the list it found, since
   `append_keymap` is a real trigger and "you also have a `[dialog]`" would
   send the reader hunting for a `keymap` that is not there.

An unknown name is a load error naming the key, the value, and the names that
would have worked (built from `presets::NAMES`, so it cannot go stale) — a typo
in a preset name must not degrade to "no dialog keys".

## Consequences

**Positive.** A new `dialog.*` command is a one-file edit again. The four
imported presets state their `[dialog]` provenance in one readable line instead
of 25 copied ones, and a reader of `total-commander.toml` can see at a glance
that the overlay keys are not a transcription claim. `is_empty()` over all three
binding lists makes "declares a `[dialog]` of its own" mean what it says, so the
both-keys rule cannot be evaded through `append_keymap`.

**Negative.** `keymap.toml` grows a key, so the published JSON Schema
(`docs/schema/keymap.schema.json`) is republished and third-party editors see a
new optional property. A preset file is no longer fully self-contained: reading
`total-commander.toml` no longer tells you which keys work in a modal without a
second file open — accepted, because the alternative was seven files that
disagree. And the one-level rule will eventually be argued with; the argument to
make then is option B's, and it is recorded above.

**Neutral, worth stating.** A *project* layer is untrusted content that arrives
with a cloned repository, so "is this refusable?" is not the only question —
"is it reachable?" is the other. With rule 2 a project layer is never resolved,
so there is nothing to reach. Even if it were, the copy would land in the
layer's `dialog.keymap`, and `merge_ctx` reads only
`prepend_keymap`/`append_keymap` from a layer: those bindings are structurally
unreachable whatever `build_diagnostics` — which reports and keeps walking —
decides to do. Both halves have a test.
