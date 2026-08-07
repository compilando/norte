# H3h — Full EN/ES corpus, documentation gate at zero

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** every command the TUI dispatches and every screen it can open is
explained by a help page in both locales, so both allowlists in
`crates/norte-tui/tests/help_gate.rs` disappear along with their ceilings.

**Architecture:** the gate does not move — 47 commands and 8 contexts are paid
by writing pages, not by weakening assertions. Eight new topics join the
`include_str!` table in an order that keeps each tag run contiguous (the sidebar
groups consecutive runs, `norte-frontend/src/help.rs:708`), and four existing
topics grow the sections that carry the leftovers. One task before the writing
is code: the plugin provenance badge, which a wide publisher pushes out of the
GUI's 720 px panel.

**Tech stack:** `norte-help` (front matter TOML + markdown subset, `{{cmd:id}}`
and `[[topic]]` marks), `norte-i18n` Fluent catalogues, `norte-testkit` hostile
corpus, nextest.

---

## Vocabulary this plan assumes

- A command is **documented** when some topic names it in `commands = [...]`,
  in **both** locales (`check_locales` pins the sets equal).
- A context is **claimed** when exactly one topic lists it in `context = [...]`
  — `check_contexts` reports both a context with none and a context with two.
- Group headers in the sidebar come from the **first tag** of a topic, looked up
  as `help-group-{tag}`. A tag with no Fluent key paints its own id (the H3f
  BLOCKER); a new tag therefore means two new catalogue entries, not one.

## Command → topic map (all 47)

| Commands | Topic | New? |
|---|---|---|
| `cursor.up/down/page-up/page-down/top/bottom` | `panes` | grow |
| `pane.mkdir`, `dialog.overwrite/skip/rename/newer`, `pane.rename` | `copying` | grow |
| `dialog.add`, `dialog.remove` | `remote` | grow |
| `pane.quick-search`, `pane.search`, `pane.toggle-hidden` | `finding` | new |
| `pane.columns`, `dialog.toggle-enabled/move-up/move-down/sort/cycle-format` | `columns` | new |
| `pane.view`, `viewer.*` (10), `pane.open` | `viewer` | new |
| `pane.ai-rename`, `pane.semantic-search` | `ai` | new |
| `dialog.confirm/cancel/up/down/page-up/page-down`, `app.quit` | `dialogs` | new |
| `app.settings`, `app.theme` | `settings` | new |
| `dialog.approve`, `dialog.deny` | `agents` | new |
| `app.extensions` | `plugins` | new |

## Context → topic map (all 8)

| Context | Topic |
|---|---|
| `viewer` | `viewer` |
| `dialog.quit` | `dialogs` |
| `dialog.transfer-name`, `dialog.mkdir` | `copying` |
| `dialog.approval` | `agents` |
| `dialog.trust-lua` | `plugins` |
| `dialog.ai-rename`, `dialog.semantic-search` | `ai` |

## Corpus order (both locales, identical)

```
index, panes, selection, mouse, help, dialogs, settings,   # basics
copying, finding, columns, viewer, ai,                     # doing
remote, archives,                                          # remote
agents,                                                    # agents  (new group)
plugins                                                    # extensions
```

`agents` is a new tag and needs `help-group-agents` in `en.ftl` and `es.ftl`.
`plugins` reuses the existing `extensions` tag, so the built-in page opens the
same group the plugin pages land in.

---

### Task 0: the provenance badge survives a wide publisher

A publisher is capped at 280 **characters** (`MAX_HEADER_CHARS`,
`norte-help/src/parse.rs:577`). In CJK that is 560 columns, and the GUI paints
the badge as one unwrapped line in a 720 px panel: the segments the host
appends — `cut short`, `some bytes did not decode` — are pushed off the right
edge by third-party text. The flags are the part a reader acts on, so they
cannot be the part that gets pushed out.

The three frontends carry three byte-identical copies of `plugin_badge`
(`norte-tui/src/help_render.rs:306`, `norte-gui/src/help_render.rs:347`,
`norte-cli/src/help.rs:385`). Fixing it three times is how one of them drifts;
it moves to `norte-frontend::help` instead, clamped by display width.

**Files:**
- Modify: `crates/norte-frontend/src/help.rs` (add `plugin_badge`)
- Modify: `crates/norte-tui/src/help_render.rs:306-327` (delete copy, re-export)
- Modify: `crates/norte-gui/src/help_render.rs:347-368` (delete copy)
- Modify: `crates/norte-cli/src/help.rs:385-392` (delete copy)
- Test: in `crates/norte-frontend/src/help.rs` `mod tests`

- [ ] **Step 1: failing test in `norte-frontend/src/help.rs`**

```rust
#[test]
fn a_wide_publisher_cannot_push_the_host_flags_off_the_line() {
    // 280 CJK characters is what the parser's cap allows, and it is 560
    // columns: the flags are appended AFTER the publisher, so an unclamped
    // segment is a third-party string deciding whether a host warning is
    // visible.
    let publisher = "字".repeat(280);
    let badge = plugin_badge_parts(Some(&publisher), true, true, Lang::En)
        .expect("a plugin topic always has a badge");
    assert!(
        badge.contains(&norte_i18n::t_in(Lang::En, "help-plugin-truncated")),
        "the cut-short flag must survive a 560-column publisher: {badge}"
    );
    assert!(
        badge.contains(&norte_i18n::t_in(Lang::En, "help-plugin-lossy")),
        "the lossy flag must survive too: {badge}"
    );
    assert!(
        display_cells(&badge) <= MAX_BADGE_CELLS,
        "the badge is {} cells wide, over the {MAX_BADGE_CELLS} budget",
        display_cells(&badge)
    );
}

#[test]
fn a_publisher_that_fits_is_not_touched() {
    let badge = plugin_badge_parts(Some("ACME"), false, false, Lang::En)
        .expect("a plugin topic always has a badge");
    assert!(badge.contains("ACME"), "an ordinary publisher is painted whole: {badge}");
}
```

- [ ] **Step 2: run it, expect a compile failure naming `plugin_badge_parts`**

Run: `just t norte-frontend`
Expected: FAIL, `cannot find function 'plugin_badge_parts'`

- [ ] **Step 3: implement in `norte-frontend/src/help.rs`**

```rust
/// Display budget for the whole provenance line, in terminal cells.
///
/// It exists because the publisher is THIRD-PARTY and the flags that follow it
/// are the HOST's: without a clamp, 280 CJK characters (what
/// `norte_help::parse` allows) are 560 columns, and in the GUI's fixed-width
/// panel that pushes `cut short` and `some bytes did not decode` off the edge.
/// A plugin must not be able to decide whether a warning about itself is on
/// screen.
pub const MAX_BADGE_CELLS: usize = 96;

/// The provenance line of a plugin page: `from an extension · published by X`,
/// plus the host's flags. `None` for a built-in topic.
///
/// The publisher is clamped so the segments after it always fit. Clamping
/// happens on the PUBLISHER alone and not on the joined line: truncating the
/// result would eat the flags, which is the failure this guards against.
#[must_use]
pub fn plugin_badge_parts(
    publisher: Option<&str>,
    truncated: bool,
    lossy: bool,
    lang: Lang,
) -> Option<String> {
    let mut parts: Vec<String> = vec![norte_i18n::t_in(lang, "help-plugin-origin")];
    if let Some(p) = publisher.filter(|p| !norte_help::is_blank_id(p)) {
        let room = MAX_BADGE_CELLS.saturating_sub(
            display_cells(&parts[0])
                + if truncated { display_cells(&norte_i18n::t_in(lang, "help-plugin-truncated")) + 3 } else { 0 }
                + if lossy { display_cells(&norte_i18n::t_in(lang, "help-plugin-lossy")) + 3 } else { 0 }
                + 3,
        );
        let clamped = crate::display::middle_ellipsis(p, room);
        parts.push(norte_i18n::ta_in(lang, "help-plugin-by", &[("who", clamped.as_str())]));
    }
    if truncated {
        parts.push(norte_i18n::t_in(lang, "help-plugin-truncated"));
    }
    if lossy {
        parts.push(norte_i18n::t_in(lang, "help-plugin-lossy"));
    }
    Some(parts.join(" · "))
}
```

`display_cells` is the existing width helper of `crate::display`; if it is
private there, make it `pub(crate)` in the same step.

- [ ] **Step 4: run the test, expect PASS**

Run: `just t norte-frontend`

- [ ] **Step 5: delete the three copies, call the shared one**

Each frontend keeps its own `plugin_badge(topic, lang)` wrapper that destructures
`Origin::Plugin` and delegates:

```rust
fn plugin_badge(topic: &Topic, lang: Lang) -> Option<String> {
    let norte_help::Origin::Plugin { publisher, truncated, lossy, .. } = &topic.origin else {
        return None;
    };
    norte_frontend::help::plugin_badge_parts(publisher.as_deref(), *truncated, *lossy, lang)
}
```

The long rustdoc on the TUI copy (the `·` in-band joiner analysis) moves to the
shared function; it is the only place it is still true.

- [ ] **Step 6: whole suite plus the GUI's**

Run: `just t norte-tui && just t norte-cli && just t norte-frontend && just gui-ci`

- [ ] **Step 7: commit**

```bash
git add crates/norte-frontend/src/help.rs crates/norte-tui/src/help_render.rs \
        crates/norte-gui/src/help_render.rs crates/norte-cli/src/help.rs
git commit -m "fix(help): a wide publisher can no longer push the host flags off the badge"
```

---

### Tasks 1–6: the corpus

Each task adds its pages to `topics/en/`, `topics/es/` and the two
`include_str!` tables in the declared corpus order, then **deletes the entries
it paid for from `PENDIENTES` / `CONTEXTOS_PENDIENTES` and lowers both `const _`
ceilings in the same diff**. A task is not done while an entry it covered is
still on a list: `check_commands` reports it as `Stale` and the suite goes red,
which is the mechanism working.

Every page follows the house voice of the existing eight: second person, the
key never written literally (always `{{cmd:id}}`), a `> ⚠` callout only where
something can lose data or surprise, `[[links]]` to the neighbours, and a
`see_also` that is reciprocated by at least one of the pages it names.

Per task, the loop is the same five steps:

- [ ] **Step A:** write `topics/en/<id>.md` and `topics/es/<id>.md`.
- [ ] **Step B:** add both to `EN`/`ES` in `crates/norte-help/src/corpus.rs` at
      the position the corpus order table gives.
- [ ] **Step C:** delete the covered ids from `PENDIENTES` /
      `CONTEXTOS_PENDIENTES` and lower the ceilings (`<= N`) to the new lengths.
- [ ] **Step D:** run `just t norte-help && just t norte-tui`; expect green. A
      `Stale` issue means an entry was left behind; an `UnknownCommand` means a
      typo in `commands`.
- [ ] **Step E:** commit, one topic group per commit.

#### Task 1: `viewer` (11 commands, 1 context)

**Front matter, EN (ES identical but for `title`):**

```toml
+++
id = "viewer"
title = "Reading a file without leaving"
tags = ["doing"]
see_also = ["panes", "archives", "finding"]
commands = [
    "pane.view",
    "viewer.close",
    "viewer.up",
    "viewer.down",
    "viewer.page-up",
    "viewer.page-down",
    "viewer.top",
    "viewer.bottom",
    "viewer.encoding",
    "viewer.encoding-auto",
    "viewer.hex",
    "pane.open",
]
context = ["viewer"]
+++
```

Prose must cover: the viewer opens on the entry under the cursor and reads a
bounded head of it, so a 40 GB file opens as fast as a small one; movement;
`{{cmd:viewer.hex}}` for anything that is not text; encoding is DETECTED and
`{{cmd:viewer.encoding}}` cycles the candidates while `{{cmd:viewer.encoding-auto}}`
gives the decision back to the detector — with the warning that decoding is a
guess, the bytes on disk never change, and a wrong guess is cosmetic; a page
inside an archive is read the same way; and `{{cmd:pane.open}}` hands the file
to an external program from `openers.toml`, which is the one command on the
page that leaves norte's sandbox. Note that the help is unreachable while the
viewer is open (the viewer captures every key), which is why this page exists
where the reader can reach it: from the index, before opening one.

Removes from `PENDIENTES`: `pane.open`, the ten `viewer.*`. New ceiling: 36.
Removes from `CONTEXTOS_PENDIENTES`: `viewer`. New ceiling: 7.

#### Task 2: `finding` + `columns` (8 commands)

**`finding`:**

```toml
+++
id = "finding"
title = "Finding things in a listing"
tags = ["doing"]
see_also = ["selection", "columns", "ai"]
commands = ["pane.quick-search", "pane.search", "pane.toggle-hidden"]
+++
```

Covers: `{{cmd:pane.quick-search}}` filters/jumps inside what the pane already
holds — no I/O, no daemon; `{{cmd:pane.search}}` is the real search by name or
content, and it is a task like a copy is (it reports progress, it can be
cancelled, it keeps going while you navigate); `{{cmd:pane.toggle-hidden}}` and
the fact that hidden is a display decision, never a filter on what a command
acts on. A `> ⚠` for the honest bit: a search over a remote backend pays a round
trip per directory, so a deep tree over SFTP is slow in a way a local one is not.

**`columns`:**

```toml
+++
id = "columns"
title = "What the listing shows"
tags = ["doing"]
see_also = ["finding", "panes"]
commands = [
    "pane.columns",
    "dialog.toggle-enabled",
    "dialog.move-up",
    "dialog.move-down",
    "dialog.sort",
    "dialog.cycle-format",
]
+++
```

Covers the column picker as the one overlay whose verbs are worth naming:
toggle a column, reorder it, choose what the listing is sorted by, cycle how a
size or a date is formatted. Say that the choice is per pane and persisted, and
that a column a backend cannot answer (an owner on S3) shows empty rather than
guessing.

Removes 8 entries. New ceiling: 28.

#### Task 3: `dialogs` + `settings` (9 commands, 1 context)

**`dialogs`:**

```toml
+++
id = "dialogs"
title = "Answering a dialog"
tags = ["basics"]
see_also = ["copying", "help", "agents"]
commands = [
    "dialog.confirm",
    "dialog.cancel",
    "dialog.up",
    "dialog.down",
    "dialog.page-up",
    "dialog.page-down",
    "app.quit",
]
context = ["dialog.quit"]
+++
```

Covers the shared grammar of every overlay: the same six verbs everywhere, and
each dialog supports the subset that means something in it — the footer of the
dialog is generated from that subset, so what it shows is what it accepts.
Then the rule the reader must not learn the hard way: **no dialog that can
destroy data has a default answer**, so `⏎` is never a way to get past one
without reading it. `{{cmd:app.quit}}` is the example this page carries all the
way, because quitting is the one confirmation that mutates nothing: it asks
because a running task would be cancelled, and it names them.

**`settings`:**

```toml
+++
id = "settings"
title = "Settings and themes"
tags = ["basics"]
see_also = ["plugins", "help", "mouse"]
commands = ["app.settings", "app.theme"]
+++
```

Covers: settings edit `norte.toml`, which is a file you can also edit by hand
and which is re-read when saved; `{{cmd:app.theme}}` picks a theme, previews it
live and only persists on confirm; a setting whose effect needs a restart says
so on its row. Cross-references `[[mouse]]` for `mouse = false`, which is the
setting people look for first.

Removes 7 commands + `dialog.quit`. New ceilings: 21 and 6.

#### Task 4: `agents` + `plugins` (3 commands, 2 contexts, 1 new group)

**`agents`:**

```toml
+++
id = "agents"
title = "When an agent asks"
tags = ["agents"]
see_also = ["dialogs", "plugins", "copying"]
commands = ["dialog.approve", "dialog.deny"]
context = ["dialog.approval"]
+++
```

This is the page a reader reaches with a modal in front of them, so it is
written for that moment: what the request says (who is asking, which paths,
which operation), that `{{cmd:dialog.approve}}` and `{{cmd:dialog.deny}}` are
the only two answers and that closing the modal is a **deny**, that a scope is
granted for a subtree and expires, and that everything an agent did is in the
journal and can be undone by you even after its scope expired. A `> ⚠` on the
one thing that matters: the paths in the request are shown one per line and
masked, because a path is where a name that reads like another name would be
used to get an approval it did not deserve.

**`plugins`:**

```toml
+++
id = "plugins"
title = "Extensions"
tags = ["extensions"]
see_also = ["settings", "remote", "agents"]
commands = ["app.extensions"]
context = ["dialog.trust-lua"]
+++
```

Covers: `{{cmd:app.extensions}}` lists what is installed, what is approved and
what is enabled — three different things, and nothing runs until you approve
it; a plugin runs as WebAssembly with only the capabilities its manifest asks
for, which is why the FTP backend on `[[remote]]` is a plugin and not a special
case; a plugin's own page appears in this same group and says on its face that
a plugin wrote it. Then the `init.lua` question: a project directory can carry
one, it is not run until you say so, the decision is remembered per file
contents, and changing the file asks again.

`i18n/en.ftl` and `i18n/es.ftl` gain `help-group-agents = Agents & policy` /
`= Agentes y política`, next to the other `help-group-*` entries.

Removes 3 commands + 2 contexts. New ceilings: 18 and 4.

#### Task 5: `ai` (2 commands, 2 contexts)

```toml
+++
id = "ai"
title = "AI rename and semantic search"
tags = ["doing"]
see_also = ["finding", "agents", "copying"]
commands = ["pane.ai-rename", "pane.semantic-search"]
context = ["dialog.ai-rename", "dialog.semantic-search"]
+++
```

Two features, one page, because both are two-step flows and the second step is
where the safety is: `{{cmd:pane.ai-rename}}` produces a **reviewable plan** —
nothing is renamed until you accept it, every line is editable, and rejecting
it costs nothing; `{{cmd:pane.semantic-search}}` searches the local index by
meaning rather than by name, over what has been indexed and only that. A `> ⚠`
that both send content to a model, so what leaves the machine is content, and
the setting that turns them off is on `[[settings]]`.

Removes 2 commands + 2 contexts. New ceilings: 16 and 2.

#### Task 6: the grown pages (16 commands, 2 contexts) — the lists go to zero

- `panes`: a *Moving around* section for the six `cursor.*`, added to
  `commands`. Say that the cursor is per pane and that the pane remembers it
  when you leave and come back.
- `copying`: a *Renaming* paragraph naming `{{cmd:pane.rename}}` and the
  editable name a transfer offers (claiming `dialog.transfer-name`), a
  *Making a directory* paragraph for `{{cmd:pane.mkdir}}` (claiming
  `dialog.mkdir`), and the four collision answers added to `commands` so the
  table that already explains them also names them as commands. Title becomes
  "Copying, moving, renaming and deleting" (`id` unchanged).
- `remote`: `dialog.add` / `dialog.remove` added to `commands`, in the
  paragraph about the hotlist popup — naming an entry and dropping one.
- `index`: the *Where to go next* list gains the eight new pages, in corpus
  order.

Both allowlists are now empty. **Delete the two `const` arrays, the two
`const _` ceiling assertions, and the module prose that describes them**, then
change the two call sites to pass `&[]` — which is what the other assertion in
that file already does, and the reason it can say so without a caveat.

- [ ] Final step of this task: `just t norte-tui` and read the gate test's own
      output: `check_commands(&vocabulario(), &[])` empty, `check_contexts`
      empty.

---

### Task 7: the hostile fixtures the audit asked for

Three cases the corpus does not have, all on the plugin-help seam, added with
the `fixture` skill so they land in the canonical `norte-testkit` corpus with a
`why` and the pin count updated (currently 22 → 25).

**Files:**
- Modify: `crates/norte-testkit/src/corpus.rs` (or wherever the corpus lives)
- Test: `crates/norte-help/src/parse.rs` `mod tests`, plus the frontends' help
  render tests

- [ ] **Step 1: `plugin_id_path_traversal`** — a plugin id of `../../etc`.
      Asserts: it never reaches a `TopicId`, and the sidebar row for it is
      dropped rather than painted (the H3f guard, now with a corpus fixture
      behind it instead of a literal in one test).
- [ ] **Step 2: `command_title_blank_after_mask`** — a command title that is
      `"\u{3164}"` (HANGUL FILLER): not whitespace, blank on screen. Asserts:
      the row falls back to the command's own catalogue label, never to the raw
      dispatch key.
- [ ] **Step 3: `publisher_fullwidth_overflow`** — 280 fullwidth characters.
      Asserts what Task 0 introduced, from the corpus rather than from a
      `repeat` in the test: the badge stays within `MAX_BADGE_CELLS` and both
      host flags survive.
- [ ] **Step 4:** `just t norte-testkit && just t norte-help && just t norte-tui && just gui-ci`
- [ ] **Step 5:** commit.

---

### Task 8: close-out

- [ ] **Step 1:** `docs/superpowers/specs/2026-08-04-help-system-redesign-design.md`
      — mark H3h done in the phase table, and delete the sentence that says the
      allowlist carries the remainder "until phase H3h".
- [ ] **Step 2:** CHANGELOG entry under `### Added`, in the voice of the H3f/H3g
      entries: the help now covers every command and every screen; the gate's
      allowlists are gone, so a new command without a page fails the build with
      nothing to add it to.
- [ ] **Step 3:** `just ci`. Expect EXIT=0. Coverage is at 85.13% against an
      85% gate and this change is mostly markdown, which is not instrumented —
      if it moves at all it moves by the Task 0 code.
- [ ] **Step 4:** commit.

---

## Self-review

**Spec coverage.** All 47 commands and all 8 contexts appear exactly once in the
two maps above; the count per task (11+8+9+3+2+16 = 49 command slots, of which
`pane.view` and `pane.rename` were already documented elsewhere → 47 newly
covered) closes the list. Hostile-corpus coverage is Task 7, which the design
names as H3h's own criterion.

**Placeholders.** Front matter is given literally for every new page; the prose
is specified by what it must cover, which is the part that cannot be
pre-written without writing it. No step says "add error handling" or "similar to
Task N".

**Type consistency.** `plugin_badge_parts(publisher, truncated, lossy, lang)`
and `MAX_BADGE_CELLS` are used with the same signature in Task 0 and Task 7.
`middle_ellipsis(s, max)` matches `norte-frontend/src/display.rs:182`.
