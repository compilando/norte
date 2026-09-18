# 0125 — Menus are read in sections

- Status: accepted
- Date: 2026-09-18
- Decision makers: Oscar González
- Bridge: 73 → **74** (`MenuItemView.section`, `MenuItemView.role`; `chord`
  empty instead of `—`)
- Related: ADR 0077 (one rule, both frontends), the ten-menu regrouping of
  2026-09-10

## Context and problem statement

Each menu was a flat list of command ids. Operate had seventeen entries in a
row. Copy and Delete permanently looked the same. Pack, Unpack and Test
archive were not visibly a group. Every entry without a key showed `—`, which
reads as "disabled".

A menu is for finding things, and a flat list of seventeen has to be read in
full.

## Decision

**1. A menu is made of sections.** `Menu.sections: &[Section]`, where each
section has an optional title (`menu-section-*`) and its ids. A title goes
only where the group does not explain itself (Archives, Split files,
Integrity, History, Places…). The others are a plain rule. The first section
draws no rule, because the menu's top border already does that.

**2. The cursor does not see sections.** `MenuState` still walks a flat list
(`Menu::items()`, `Menu::item(i)`). A rule is not a stop, and every index that
crosses the bridge (`menu_point_row`, `menu_activate_row`) still names a
command. `Menu::section_at(i)` tells the painters where a section starts.

**3. An entry says what kind it is.** `menu::role(id)`: `destructive` for
delete and delete-permanently, painted in the error colour except under the
cursor, and `ai` for AI rename and semantic search, which carry `✦`. It is a
presentation list in the shared crate, not a catalogue field, and both
painters read it.

**4. No key, no mark.** An entry without a binding leaves the chord column
empty.

**5. The terminal gives way before it cuts.** A menu taller than the screen
drops its untitled rules first, then its titles, and never a command. Rules
join the border (`├───┤`), as in mc.

**6. The window gets the same thing over the bridge.** Each `MenuItemView`
carries `section` (`null` = same section, `""` = untitled rule, text = title)
and `role`. It goes on the entry, not as an element of its own, so the
renderer's row indices still count commands. The renderer paints a
`role="separator"` list item and styles destructive and AI entries in CSS.

## Consequences

- Regrouping a menu is a data change in `menu.rs`. The tests check that
  every command exists and is live, that none appears twice, that no section
  is empty, and that every title has a label in both languages.
- Operate is now copy/move/rename/organise · mkdir/chmod · delete · archives ·
  split files · integrity.

## Left for later

- **A letter per entry** (press it to run the entry while the menu is open).
  It needs a letter-assignment rule that is stable across languages, and key
  handling in both frontends.
- **Plugin contributions** (`menu = "operate/archives"` in the manifest). The
  section model is ready for them. The manifest, the WIT package and the
  policy review of which menus a plugin may enter are not.
- **Context greying in the terminal.** The window greys what it cannot run.
  The terminal runs everything, and "cannot run *here*" (Combine with nothing
  split) is not modelled yet.
