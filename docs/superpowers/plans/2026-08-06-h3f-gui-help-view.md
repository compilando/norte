# H3f — GUI help view Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the GUI the same help the TUI got in H3b–H3e — a GPUI overlay
over `norte_frontend::help::HelpState` with sidebar, body, filter, executable
rows and plugin pages — and put `app.help` into the GUI's `COMMANDS` so the
shared preset's `F1` stops being silently dropped.

**Architecture:** Same split every GUI overlay already uses (`palette_view` /
`settings_view` / `columns_view`): a **pure** module owns state and keyboard
routing, `main.rs` owns async `Backend` calls and painting. Two new files —
`help_view.rs` (resolver + view state + `on_key`) and `help_render.rs` (pure
`norte_help::Topic` → `Vec<HelpLine>`, so the layout is unit-testable without a
GPUI window) — plus wiring in `main.rs`, `keymap.rs` and `session.rs`. The
model, corpus, parser and availability table are all reused unchanged: this
phase adds a **renderer and a wiring**, not a second help system.

**Tech Stack:** Rust 2024, GPUI (`crates/norte-gui`, excluded from the
workspace — its own `Cargo.lock`, gated by `just gui-ci`), `norte-help`
(corpus + hostile parser), `norte-frontend` (`help::HelpState`,
`availability::{Facts, verdict_with_plugins, reason_key}`, `keymap::Effective`),
`norte-i18n` (Fluent), `norte-theme` (`Role`), `norte-proto` 0.34.0
(`PluginInfo.has_help`, `plugin.help`).

---

## Context an implementer needs before task 1

Read these first; every task below assumes them.

* `docs/superpowers/specs/2026-08-04-help-system-redesign-design.md`, sections
  *Surfaces → GUI* and *Plugin help*. H3f is one row of its phase table.
* `crates/norte-tui/src/help.rs` (`TuiChords`, `build`) and
  `crates/norte-tui/src/app.rs` (`HelpView`, `HelpOutcome`, `help_action`,
  `plugin_label`) — this phase is the GUI twin of those. **Do not move them to
  a shared crate**: the two frontends resolve different `Screen` sets (the GUI
  has no `Dialog` keymap) and paint with different technologies. The shared part
  is already shared (`norte_frontend::help::HelpState`).
* `crates/norte-gui/src/palette_view.rs` — the file to imitate for module
  shape, doc style, and the `on_key` → `Outcome` → `main.rs` contract.
* Rules that bite here: **rule 1** (bytes, never `to_str().unwrap()`),
  **rule 7** (no business logic in a frontend), **rule 6** (no `unwrap`/`expect`
  outside tests).

Commands (the GUI is excluded from the workspace — `just t`/`just c` do **not**
cover it):

```sh
cd crates/norte-gui && cargo nextest run help          # this phase's tests
just check-gui                                          # cheap: cargo check --locked
just gui-ci                                             # nextest + clippy -D warnings + fmt --check
just ci                                                 # once, at the end (i18n parity lives in the workspace)
```

## File structure

| File | Responsibility |
| --- | --- |
| `crates/norte-gui/src/help_view.rs` (**create**) | `GuiChords` (the `ChordResolver` of this frontend), `keys_lines` (the synthetic keyboard page), `HelpView` (state + plugin snapshot + fetch claim), `HelpOutcome`, `on_key`. No GPUI types. |
| `crates/norte-gui/src/help_render.rs` (**create**) | Pure `Topic` → `Vec<HelpLine>` with `(text, Role)` spans, the plugin badge, the action→line map, dim reasons. No GPUI types, so it is unit-testable. |
| `crates/norte-gui/src/main.rs` (**modify**) | `help: Option<help_view::HelpView>` field, `app.help` dispatch arm, key capture slot, `render_help`, async `plugin.list`/`plugin.help` handling, Enter dispatch, `ctrl+p` handoff. |
| `crates/norte-gui/src/keymap.rs` (**modify**) | `"app.help"` in `COMMANDS`. |
| `crates/norte-gui/src/session.rs` (**modify**) | `SessionCmd::PluginHelp { id }` + `SessionEvent::PluginHelpReady/Failed`. |
| `crates/norte-i18n/i18n/{en,es}.ftl` (**modify**) | `help-hint-gui`, `help-blocked-*` reuse — see task 4. |
| `CHANGELOG.md` (**modify**) | One entry under Unreleased. |

---

### Task 1: `GuiChords` — the GUI's `ChordResolver`

**Files:**
- Create: `crates/norte-gui/src/help_view.rs`
- Modify: `crates/norte-gui/src/main.rs` (add `mod help_view;` next to `mod palette_view;`, around line 107)
- Test: inline `#[cfg(test)] mod tests` in `crates/norte-gui/src/help_view.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/norte-gui/src/help_view.rs` with ONLY the test module plus the
`use`s it needs, so the first run fails to compile on the missing type:

```rust
//! GUI help overlay (H3f, `F1`): the GUI twin of the TUI's `help`/`app::HelpView`.
//!
//! Same split as `palette_view`/`settings_view`: this file is PURE — resolver,
//! view state, keyboard routing — and `main.rs` owns the async `Backend` calls
//! and the painting. The model itself (`norte_frontend::help::HelpState`) is
//! shared with the TUI; what a frontend must supply is the answer to three
//! questions `norte-help` asks — the reader's chord, a short label, and whether
//! the command can run right now — which is [`GuiChords`].

use std::collections::{BTreeMap, BTreeSet, HashMap};

use norte_frontend::availability::Facts;
use norte_frontend::keymap::Effective;
use norte_help::{Availability, ChordResolver};

use crate::keymap::{gpui_chord_label, help_id};

#[cfg(test)]
mod tests {
    use super::*;

    fn effectives() -> (Effective, Effective) {
        crate::keymap::build_effectives_preset_only("orthodox")
    }

    #[test]
    fn chord_resuelve_en_la_pantalla_del_comando() {
        let (browse, viewer) = effectives();
        let r = GuiChords::new(&browse, &viewer, norte_i18n::Lang::En);
        // A browse command wears its browse chord…
        assert_eq!(r.chord("pane.copy").as_deref(), Some("F5"));
        // …and a command nobody bound names nothing rather than inventing a key.
        assert_eq!(r.chord("no.such.command"), None);
    }

    #[test]
    fn label_cae_a_vacio_y_nunca_al_id_fluent() {
        let (browse, viewer) = effectives();
        let r = GuiChords::new(&browse, &viewer, norte_i18n::Lang::En);
        assert_eq!(r.label("app.quit"), norte_i18n::t_in(norte_i18n::Lang::En, "help-cmd-app-quit"));
        assert_eq!(r.label("no.such.command"), "", "a miss is BLANK, never the echoed id");
    }

    #[test]
    fn availability_sin_congelar_no_atenua_nada() {
        let (browse, viewer) = effectives();
        let r = GuiChords::new(&browse, &viewer, norte_i18n::Lang::En);
        assert!(r.availability("pane.copy").is_available());
    }

    #[test]
    fn un_plugin_apagado_atenua_su_fila_y_conserva_su_titulo() {
        let (browse, viewer) = effectives();
        let key = "plugin:org.norte.demo:greet";
        let mut titles = HashMap::new();
        titles.insert(key.to_owned(), "Greet the world".to_owned());
        let r = GuiChords::new(&browse, &viewer, norte_i18n::Lang::En)
            .with_plugins(BTreeSet::new(), titles);
        assert_eq!(r.label(key), "Greet the world");
        assert!(
            !r.availability(key).is_available(),
            "an empty active set is an allowlist miss: fail-closed"
        );
        let r = r.with_plugins(BTreeSet::from(["org.norte.demo".to_owned()]), HashMap::new());
        assert!(r.availability(key).is_available());
    }

    #[test]
    fn una_clave_plugin_desconocida_se_pinta_enmascarada_y_acotada() {
        let (browse, viewer) = effectives();
        let r = GuiChords::new(&browse, &viewer, norte_i18n::Lang::En);
        let hostile = format!("plugin:evil\u{202e}id:{}", "x".repeat(200));
        let painted = r.label(&hostile);
        assert!(!painted.contains('\u{202e}'), "bidi override survived: {painted:?}");
        assert!(painted.chars().count() <= 65, "no cap: {} chars", painted.chars().count());
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd crates/norte-gui && cargo nextest run help_view`
Expected: compile error — `cannot find type GuiChords in this scope` (and
`gpui_chord_label` / `help_id` unresolved if `keymap.rs` does not export them
yet; step 3 covers that).

- [ ] **Step 3: Implement `GuiChords`**

First, check what `crates/norte-gui/src/keymap.rs` already exports. It has
`gpui_chord` (line ~307) and the palette derives its own `help-cmd-*` id in
`palette_view::help_id`. Reuse rather than duplicate:

* If `keymap.rs` has no public `help_id`, **move** `palette_view::help_id` into
  `keymap.rs` as `pub fn help_id(cmd: &str) -> String` and have
  `palette_view.rs` call `crate::keymap::help_id` (delete its private copy).
  One derivation of the id per frontend, as in the TUI.
* For the painted chord, reuse whatever `palette_view::build_rows` uses today
  (`norte_frontend::palette::first_chord`, which already masks). Name the import
  in `help_view.rs` accordingly and drop `gpui_chord_label` from the `use` list
  if it does not exist — the test asserts `"F5"`, which is what the palette
  paints.

Then append to `help_view.rs`, above the test module:

```rust
/// The facts of a context with nothing in the way: what [`GuiChords`] answers
/// against until the overlay opens and freezes the real ones.
///
/// Everything permissive on purpose — a derived `Default` would be all-false,
/// which is the OPPOSITE (`enterable: false` alone dims `nav.enter` on every
/// page painted through a resolver nobody froze yet).
const NO_IMPEDIMENT: Facts = Facts {
    enterable: true,
    viewable: true,
    rename_single: true,
    source_read_only: false,
    dest_read_only: false,
    degraded: false,
};

/// Cap, in CHARS, on a `plugin:` key painted as its own label — the GUI twin of
/// the TUI's `plugin_label`. A third party picks that string.
const PLUGIN_KEY_CAP: usize = 64;

/// What a plugin-contributed dispatch key starts with. The LOOSE question
/// ("does this claim to be a plugin's?"), unlike
/// `norte_frontend::availability::plugin_of_command`, which asks the strict one.
const PLUGIN_KEY_PREFIX: &str = "plugin:";

/// The GUI's answer to the three questions `norte-help` asks a frontend: the
/// reader's effective chord, a short label, and whether the command can run now.
///
/// Rebuild it wherever the effectives are rebuilt (startup and theme/keymap
/// reload): a rebind that does not reach this resolver is a help page teaching
/// the OLD key.
#[derive(Debug, Clone)]
pub struct GuiChords {
    /// Command → painted chord, filled browse then viewer, first writer wins
    /// (a command bound in both keeps its browse chord — same precedence a
    /// browse-first or-chain would have had).
    chords: HashMap<String, String>,
    /// The language `label` answers in.
    lang: norte_i18n::Lang,
    /// The context `availability` answers against, FROZEN when the overlay
    /// opened: a row that changed verdict halfway down the page because the
    /// reader scrolled would make the page disagree with itself.
    facts: Facts,
    /// Plugin ids that are approved AND enabled. Empty dims every plugin row:
    /// an allowlist miss is fail-closed, unlike [`Facts`], which is a list of
    /// known impediments and therefore fail-open.
    active_plugins: BTreeSet<String>,
    /// Dispatch key → the title the MANIFEST gives that command, from the same
    /// `plugin.list` snapshot as `active_plugins` (never from the `help.md`:
    /// the manager, the palette and the page must call a command one thing).
    plugin_labels: HashMap<String, String>,
}

impl GuiChords {
    /// Builds from the GUI's two effective keymaps and the label language.
    #[must_use]
    pub fn new(browse: &Effective, viewer: &Effective, lang: norte_i18n::Lang) -> Self {
        let mut chords: HashMap<String, String> = HashMap::new();
        for eff in [browse, viewer] {
            for (seq, cmd) in eff.bindings() {
                chords
                    .entry(cmd.to_owned())
                    .or_insert_with(|| norte_frontend::keymap::paint_chord(&seq));
            }
        }
        Self {
            chords,
            lang,
            facts: NO_IMPEDIMENT,
            active_plugins: BTreeSet::new(),
            plugin_labels: HashMap::new(),
        }
    }

    /// The same resolver answering against `facts`. A NEW value: the open
    /// overlay was laid out through the old one, and nothing already painted
    /// may change underneath it.
    #[must_use]
    pub fn with_facts(&self, facts: Facts) -> Self {
        Self {
            facts,
            ..self.clone()
        }
    }

    /// The same resolver carrying one `plugin.list` photograph: which plugins
    /// are active and what the manifest calls each of their commands. Both
    /// halves at once — a fresh active set beside stale titles would dim a row
    /// correctly and name it wrong.
    #[must_use]
    pub fn with_plugins(&self, active: BTreeSet<String>, titles: HashMap<String, String>) -> Self {
        Self {
            active_plugins: active,
            plugin_labels: titles,
            ..self.clone()
        }
    }
}

impl ChordResolver for GuiChords {
    fn chord(&self, command: &str) -> Option<String> {
        self.chords.get(command).cloned()
    }

    /// The catalogue's short label, or an EMPTY string on a miss.
    ///
    /// Blank is the contract: `norte_i18n::t_in` answers a missing message with
    /// the id itself, so returning it unconditionally would paint `help-cmd-…`
    /// at the reader and stop `norte_help::render_command`'s fallback chain from
    /// ever naming the command. The miss is detected by testing for that echo,
    /// which IS the failure mode and therefore cannot drift.
    fn label(&self, command: &str) -> String {
        if norte_frontend::availability::plugin_of_command(command).is_some()
            && let Some(title) = self.plugin_labels.get(command)
        {
            return title.clone();
        }
        let id = crate::keymap::help_id(command);
        let text = norte_i18n::t_in(self.lang, &id);
        if text != id {
            return text;
        }
        if command.starts_with(PLUGIN_KEY_PREFIX) {
            // Third-party text. Blank would send `label_or_id` back to the raw
            // id, and that fallback paints it RAW.
            return plugin_key_label(command);
        }
        String::new()
    }

    fn availability(&self, command: &str) -> Availability {
        norte_frontend::availability::verdict_with_plugins(
            command,
            &self.facts,
            &self.active_plugins,
        )
    }
}

/// Masks and caps a string a plugin chose — the GUI twin of the TUI's
/// `plugin_label`. `mask_terminal_hazards` does not cap length, and a kilometric
/// name is its own denial of service against a one-line row.
#[must_use]
pub fn plugin_key_label(raw: &str) -> String {
    let mut chars = raw.chars();
    let head: String = chars.by_ref().take(PLUGIN_KEY_CAP).collect();
    let overflowed = chars.next().is_some();
    let mut out = norte_encoding::mask_terminal_hazards(&head);
    if overflowed {
        out.push('…');
    }
    out
}
```

Add `mod help_view;` to `main.rs` beside `mod palette_view;`.

- [ ] **Step 4: Run the tests**

Run: `cd crates/norte-gui && cargo nextest run help_view`
Expected: 5 passed.

- [ ] **Step 5: Lint and commit**

```sh
cd crates/norte-gui && cargo clippy --all-targets -- -D warnings && cargo fmt
git add crates/norte-gui/src/help_view.rs crates/norte-gui/src/main.rs crates/norte-gui/src/keymap.rs crates/norte-gui/src/palette_view.rs
git commit -m "feat(gui): the help corpus resolves its marks against the GUI's own keymap (H3f)"
```

---

### Task 2: the synthetic keyboard page

**Files:**
- Modify: `crates/norte-gui/src/help_view.rs`
- Test: same file's test module

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn keys_lines_cubre_ambas_pantallas_y_traduce_cada_binding() {
        let (browse, viewer) = effectives();
        let lines = keys_lines(&browse, &viewer);
        let joined = lines.join("\n");
        assert!(joined.contains(&norte_i18n::t("help-section-browse")));
        assert!(joined.contains(&norte_i18n::t("help-section-viewer")));
        // A real binding, spelled the way the documentation spells it.
        assert!(joined.contains("F5"), "no browse chord: {joined}");
        // Nothing paints a bare Fluent id at the reader.
        assert!(!joined.contains("help-cmd-"), "untranslated id in the sheet: {joined}");
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd crates/norte-gui && cargo nextest run help_view::tests::keys_lines`
Expected: FAIL — `cannot find function keys_lines in this scope`.

- [ ] **Step 3: Implement**

```rust
/// Width in CHARS of the chord column of the cheatsheet. The GUI paints the
/// sheet as two columns of elements (`main::render_help`), so this is only the
/// fallback used when the lines are consumed as text (tests, copy).
const CHORD_COLUMN: usize = 14;

/// The body of the synthetic keyboard page: every binding of both screens with
/// its catalogue description, in real precedence order (what the key DOES, not
/// what the preset says).
///
/// Generated, never a maintained list — rebinding changes the sheet. The GUI has
/// no `Dialog` screen, so unlike the TUI's `build` there is no third section.
#[must_use]
pub fn keys_lines(browse: &Effective, viewer: &Effective) -> Vec<String> {
    let mut out = Vec::new();
    for (title, eff) in [
        (norte_i18n::t("help-section-browse"), browse),
        (norte_i18n::t("help-section-viewer"), viewer),
    ] {
        out.push(String::new());
        out.push(format!("── {title} ──"));
        for (seq, cmd) in eff.bindings() {
            let seq = norte_frontend::keymap::paint_chord(&seq);
            let pad = " ".repeat(CHORD_COLUMN.saturating_sub(seq.chars().count()));
            out.push(format!("  {seq}{pad} {}", norte_i18n::t(&crate::keymap::help_id(cmd))));
        }
    }
    out
}
```

- [ ] **Step 4: Run the tests**

Run: `cd crates/norte-gui && cargo nextest run help_view`
Expected: 6 passed.

- [ ] **Step 5: Commit**

```sh
cd crates/norte-gui && cargo fmt
git add crates/norte-gui/src/help_view.rs
git commit -m "feat(gui): the keyboard page is generated from the effective keymap (H3f)"
```

---

### Task 3: `HelpView` state, plugin snapshot and fetch claim

**Files:**
- Modify: `crates/norte-gui/src/help_view.rs`
- Test: same file's test module

- [ ] **Step 1: Write the failing test**

```rust
    fn info(id: &str, has_help: bool, approved: bool, enabled: bool) -> norte_proto::methods::PluginInfo {
        norte_proto::methods::PluginInfo {
            id: id.into(),
            name: format!("Name of {id}"),
            publisher: "ACME".into(),
            version: "0.1.0".into(),
            category: "command".into(),
            capabilities: Vec::new(),
            approved,
            enabled,
            description: None,
            commands: vec![norte_proto::methods::PluginCommandInfo {
                id: "greet".into(),
                title: "Greet".into(),
            }],
            columns: Vec::new(),
            has_help,
        }
    }

    #[test]
    fn set_plugins_solo_da_fila_a_quien_trae_pagina() {
        let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
        view.set_plugins(&[info("acme.ftp", true, true, true), info("acme.mute", false, true, true)]);
        let ids: Vec<&str> = view
            .state
            .rows()
            .iter()
            .filter_map(|r| match r {
                norte_frontend::help::SidebarRow::Topic { id, .. } => Some(id.as_str()),
                norte_frontend::help::SidebarRow::Group { .. } => None,
            })
            .collect();
        assert!(ids.contains(&"acme.ftp"), "a plugin with help has a row: {ids:?}");
        assert!(!ids.contains(&"acme.mute"), "a node that opens nothing is a dead end");
    }

    #[test]
    fn claim_plugin_fetch_pregunta_una_sola_vez() {
        let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
        view.set_plugins(&[info("acme.ftp", true, true, true)]);
        view.state.open(&norte_help::TopicId::new("acme.ftp"));
        assert_eq!(view.claim_plugin_fetch().as_deref(), Some("acme.ftp"));
        assert_eq!(view.claim_plugin_fetch(), None, "a page that never arrives is not re-asked");
    }

    #[test]
    fn install_enmascara_la_pagina_y_conserva_el_editor_del_snapshot() {
        let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
        view.set_plugins(&[info("acme.ftp", true, true, true)]);
        view.state.open(&norte_help::TopicId::new("acme.ftp"));
        let _ = view.claim_plugin_fetch();
        view.install_plugin_page(
            "acme.ftp",
            &norte_proto::methods::PluginHelpResult {
                markdown: "---\nid = \"acme.ftp\"\ntitle = \"T\u{202e}itle\"\n---\n\nbody".into(),
                truncated: false,
                lossy: false,
            },
        );
        let topic = view.state.current_topic().expect("the page is installed");
        assert!(!topic.title.contains('\u{202e}'), "bidi override survived: {:?}", topic.title);
        assert!(
            matches!(&topic.origin, norte_help::Origin::Plugin { publisher, .. } if publisher.as_deref() == Some("ACME")),
            "the publisher comes from the SNAPSHOT, never from the page: {:?}",
            topic.origin
        );
    }

    #[test]
    fn el_resolver_congelado_conoce_los_plugins_activos_y_sus_titulos() {
        let (browse, viewer) = effectives();
        let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
        view.set_plugins(&[info("acme.ftp", true, true, false)]); // approved, DISABLED
        let r = view.freeze(&GuiChords::new(&browse, &viewer, norte_i18n::Lang::En), NO_IMPEDIMENT);
        let key = "plugin:acme.ftp:greet";
        assert_eq!(r.label(key), "Greet");
        assert!(!r.availability(key).is_available(), "a disabled plugin dims its own rows");
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd crates/norte-gui && cargo nextest run help_view`
Expected: FAIL — `cannot find type HelpView in this scope`.

- [ ] **Step 3: Implement**

```rust
/// The open help overlay: the shared model plus what only this frontend knows.
#[derive(Debug)]
pub struct HelpView {
    /// Sidebar, body scroll, filter, history and focus — shared with the TUI.
    pub state: norte_frontend::help::HelpState,
    /// Body of the synthetic keyboard page ([`keys_lines`]).
    pub keys_lines: Vec<String>,
    /// Plugin ids already ASKED FOR in this overlay.
    ///
    /// `HelpState::plugin_needs_fetch` is a POLLING question: it keeps
    /// answering `Some(id)` until the page is installed, so a caller that
    /// visits it per frame would re-issue the request forever against a daemon
    /// that cannot answer. Claiming BEFORE the request is the point — the case
    /// worth guarding is the answer that never comes. Scope is the open
    /// overlay: closing and reopening is the reader's retry.
    asked: BTreeSet<String>,
    /// Publisher of each plugin of the snapshot, keyed by id, already masked
    /// and capped. `parse_untrusted` takes it as the attribution of the page,
    /// and the page is parsed long after the snapshot that knew it.
    publishers: BTreeMap<String, String>,
    /// Active plugin ids of the snapshot (approved AND enabled).
    active: BTreeSet<String>,
    /// `plugin:{id}:{command}` → the manifest's title, from the same snapshot.
    titles: HashMap<String, String>,
}

impl HelpView {
    /// Opens on the index topic of `lang`.
    #[must_use]
    pub fn new(lang: norte_i18n::Lang, keys_lines: Vec<String>) -> Self {
        Self {
            state: norte_frontend::help::HelpState::new(lang, norte_i18n::t("help-topic-keys")),
            keys_lines,
            asked: BTreeSet::new(),
            publishers: BTreeMap::new(),
            active: BTreeSet::new(),
            titles: HashMap::new(),
        }
    }

    /// Installs one `plugin.list` photograph: sidebar nodes, publishers, the
    /// active set and the command titles. All of it at once, because they are
    /// one instant (see [`GuiChords::with_plugins`]).
    ///
    /// Every third-party string is masked and capped HERE, at the single point
    /// where a `PluginListResult` enters this view: the painter downstream
    /// receives text that is already safe to paint.
    pub fn set_plugins(&mut self, plugins: &[norte_proto::methods::PluginInfo]) {
        let mut nodes = Vec::new();
        self.publishers.clear();
        self.active.clear();
        self.titles.clear();
        for p in plugins {
            let active = p.approved && p.enabled;
            if active {
                self.active.insert(p.id.clone());
            }
            self.publishers
                .insert(p.id.clone(), plugin_key_label(&p.publisher));
            for c in &p.commands {
                self.titles.insert(
                    format!("plugin:{}:{}", p.id, c.id),
                    plugin_key_label(&c.title),
                );
            }
            nodes.push(norte_frontend::help::PluginNode {
                id: p.id.clone(),
                title: plugin_key_label(&p.name),
                has_help: p.has_help,
                active,
            });
        }
        self.state.set_plugins(nodes);
    }

    /// The plugin id whose page must be fetched NOW, claimed so the next call
    /// does not ask again.
    pub fn claim_plugin_fetch(&mut self) -> Option<String> {
        let id = self.state.plugin_needs_fetch()?.to_owned();
        self.asked.insert(id.clone()).then_some(id)
    }

    /// Parses a `plugin.help` answer in hostile mode and installs it.
    ///
    /// `fold_flags` is not optional: the text arrives already short and already
    /// decoded, so this parse comes out clean and the badge — the whole
    /// user-facing mitigation for a hostile `help.md` — would go dark.
    pub fn install_plugin_page(&mut self, id: &str, res: &norte_proto::methods::PluginHelpResult) {
        let publisher = self.publishers.get(id).cloned();
        let parsed = norte_help::parse_untrusted(res.markdown.as_bytes(), id, publisher)
            .fold_flags(res.truncated, res.lossy);
        self.state.install_plugin_topic(parsed.topic);
    }

    /// The resolver this overlay paints through: `base` carrying the frozen
    /// facts and this view's plugin snapshot.
    #[must_use]
    pub fn freeze(&self, base: &GuiChords, facts: Facts) -> GuiChords {
        base.with_facts(facts)
            .with_plugins(self.active.clone(), self.titles.clone())
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cd crates/norte-gui && cargo nextest run help_view`
Expected: 10 passed.

- [ ] **Step 5: Commit**

```sh
cd crates/norte-gui && cargo clippy --all-targets -- -D warnings && cargo fmt
git add crates/norte-gui/src/help_view.rs
git commit -m "feat(gui): the help overlay carries one plugin snapshot and asks for a page once (H3f)"
```

---

### Task 4: keyboard routing (`on_key` → `HelpOutcome`)

**Files:**
- Modify: `crates/norte-gui/src/help_view.rs`
- Test: same file's test module

Key vocabulary, hardcoded GPUI key names — the convention `settings_view::on_key`
and `palette_view::on_key` already established for GUI overlays (the GUI has no
`dialog` keymap to resolve through, unlike the TUI):

| Key | Meaning |
| --- | --- |
| `escape`, `f1` | close |
| `up` / `down` | move the cursor of the focused half |
| `pageup` / `pagedown` | page the focused half |
| `tab` | swap focus sidebar ↔ body |
| `enter` | open the selected topic / run the focused row / follow the focused link |
| `backspace` | while filtering, erase; otherwise history back |
| `/` | start filtering |
| any typed char while filtering | append to the filter |

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn escape_cierra_y_barra_abre_el_filtro() {
        let (browse, viewer) = effectives();
        let chords = GuiChords::new(&browse, &viewer, norte_i18n::Lang::En);
        let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
        assert!(matches!(on_key(&mut view, "escape", None, &chords), HelpOutcome::Close));
        assert!(matches!(on_key(&mut view, "/", Some("/"), &chords), HelpOutcome::None));
        assert!(view.state.filtering());
        on_key(&mut view, "c", Some("c"), &chords);
        assert_eq!(view.state.filter_raw(), "c");
        // Backspace erases WHILE filtering instead of walking history back.
        on_key(&mut view, "backspace", None, &chords);
        assert_eq!(view.state.filter_raw(), "");
    }

    #[test]
    fn enter_sobre_una_fila_ejecutable_despacha_el_mismo_id_que_la_paleta() {
        let (browse, viewer) = effectives();
        let chords = GuiChords::new(&browse, &viewer, norte_i18n::Lang::En);
        let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
        view.state.open(&norte_help::TopicId::new("copying"));
        on_key(&mut view, "tab", None, &chords); // focus the body
        let out = on_key(&mut view, "enter", None, &chords);
        match out {
            HelpOutcome::Run(cmd) => assert!(cmd.contains('.'), "a dispatch id: {cmd}"),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn enter_sobre_una_fila_atenuada_no_despacha_nada() {
        let (browse, viewer) = effectives();
        // Everything read-only: the copy/move/delete rows of `copying` are dimmed.
        let facts = Facts {
            source_read_only: true,
            dest_read_only: true,
            ..NO_IMPEDIMENT
        };
        let chords = GuiChords::new(&browse, &viewer, norte_i18n::Lang::En).with_facts(facts);
        let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
        view.state.open(&norte_help::TopicId::new("copying"));
        on_key(&mut view, "tab", None, &chords);
        match on_key(&mut view, "enter", None, &chords) {
            HelpOutcome::Blocked(_) => {}
            other => panic!("a dimmed row must not dispatch: {other:?}"),
        }
    }

    #[test]
    fn tab_alterna_el_foco_y_backspace_vuelve_atras() {
        let (browse, viewer) = effectives();
        let chords = GuiChords::new(&browse, &viewer, norte_i18n::Lang::En);
        let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
        let start = view.state.current().clone();
        view.state.open(&norte_help::TopicId::new("copying"));
        on_key(&mut view, "backspace", None, &chords);
        assert_eq!(view.state.current(), &start, "history back");
        assert_eq!(view.state.focus(), norte_frontend::help::Focus::Topics);
        on_key(&mut view, "tab", None, &chords);
        assert_eq!(view.state.focus(), norte_frontend::help::Focus::Body);
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd crates/norte-gui && cargo nextest run help_view`
Expected: FAIL — `cannot find function on_key` / `cannot find type HelpOutcome`.

- [ ] **Step 3: Implement**

```rust
/// What the caller (`main.rs`) must do after a key.
#[derive(Debug)]
pub enum HelpOutcome {
    /// Nothing beyond a repaint.
    None,
    /// Close the overlay.
    Close,
    /// Enter over a runnable row: dispatch this key — a built-in command id or
    /// a `plugin:{id}:{command}`, the SAME vocabulary the palette dispatches,
    /// through the same path (no second route, no bypass of policy or
    /// approval).
    Run(String),
    /// Enter over a DIMMED row: say why, run nothing.
    Blocked(norte_help::Reason),
    /// `ctrl+p`: hand the current filter to the command palette.
    Palette(String),
}

/// Keyboard routing. `chords` is the FROZEN resolver the page was painted
/// through ([`HelpView::freeze`]): Enter must be answered by the same verdict
/// the reader can see, never by a fresher one.
#[must_use]
pub fn on_key(
    view: &mut HelpView,
    key: &str,
    key_char: Option<&str>,
    chords: &GuiChords,
) -> HelpOutcome {
    use norte_frontend::help::{Action, Focus};

    match key {
        "escape" | "f1" => return HelpOutcome::Close,
        "tab" => view.state.toggle_focus(),
        "up" => view.state.up(),
        "down" => view.state.down(),
        "pageup" => view.state.page_up(PAGE),
        "pagedown" => view.state.page_down(PAGE),
        "backspace" => {
            if view.state.filtering() {
                view.state.backspace();
            } else if !view.state.back() {
                return HelpOutcome::None;
            }
        }
        "enter" => {
            if view.state.focus() == Focus::Topics {
                view.state.open_selected();
                return HelpOutcome::None;
            }
            return match view.state.action().cloned() {
                Some(Action::Open(id)) => {
                    view.state.open(&id);
                    HelpOutcome::None
                }
                Some(Action::Run(cmd)) => match chords.availability(&cmd) {
                    Availability::Available => HelpOutcome::Run(cmd),
                    Availability::Unavailable { reason } => HelpOutcome::Blocked(reason),
                },
                None => HelpOutcome::None,
            };
        }
        _ => {
            if let Some(c) = crate::keys::typed_char(key, key_char) {
                if !view.state.filtering() && c == '/' {
                    view.state.start_filter();
                } else if view.state.filtering() {
                    view.state.push_char(c);
                }
            }
        }
    }
    HelpOutcome::None
}

/// Rows a page key moves. A constant rather than the painted height: this
/// module never sees a window (`main.rs` clamps the scroll against the real
/// body length after every key).
const PAGE: usize = 10;
```

If `norte_frontend::help::HelpState` exposes no `open_selected`/`action`
accessor with these exact names, read `crates/norte-frontend/src/help.rs` and
use the real ones (`selected_topic`, `action`, `action_cursor`, `open_selected`
are all listed there) — do not add methods to the shared crate for this phase.

- [ ] **Step 4: Run the tests**

Run: `cd crates/norte-gui && cargo nextest run help_view`
Expected: 14 passed.

- [ ] **Step 5: Commit**

```sh
cd crates/norte-gui && cargo clippy --all-targets -- -D warnings && cargo fmt
git add crates/norte-gui/src/help_view.rs
git commit -m "feat(gui): the help overlay answers keys, and a dimmed row runs nothing (H3f)"
```

---

### Task 5: pure layout — `Topic` → `Vec<HelpLine>`

**Files:**
- Create: `crates/norte-gui/src/help_render.rs`
- Modify: `crates/norte-gui/src/main.rs` (`mod help_render;`)
- Test: inline test module

Why a pure module rather than building GPUI elements straight from `Block`:
this is the only way the layout gets tested (a GPUI window cannot run in
`nextest`), and it is the same split `columns_view`/`context_menu` already use.
GPUI wraps text itself, so — unlike the TUI's `help_render` — this module does
NOT wrap: it emits semantic lines and `main.rs` paints each one.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use norte_help::{Availability, ChordResolver, Lang};

    struct Fixed;
    impl ChordResolver for Fixed {
        fn chord(&self, c: &str) -> Option<String> {
            (c == "pane.copy").then(|| "F5".to_owned())
        }
        fn label(&self, c: &str) -> String {
            c.to_owned()
        }
        fn availability(&self, c: &str) -> Availability {
            if c == "pane.delete" {
                Availability::Unavailable { reason: norte_help::Reason::ReadOnlyBackend }
            } else {
                Availability::Available
            }
        }
    }

    #[test]
    fn una_pagina_del_corpus_se_convierte_en_lineas_con_rol() {
        let topic = norte_help::topic(Lang::En, "copying").expect("corpus topic");
        let lines = render_topic(topic, Lang::En, &Fixed);
        assert!(!lines.is_empty());
        assert!(
            lines.iter().any(|l| l.spans.iter().any(|s| s.text.contains("F5"))),
            "a {{cmd:}} mark became the reader's chord"
        );
    }

    #[test]
    fn una_fila_no_disponible_lleva_su_razon_y_no_es_accionable() {
        let topic = norte_help::topic(Lang::En, "copying").expect("corpus topic");
        let lines = render_topic(topic, Lang::En, &Fixed);
        let dimmed: Vec<&HelpLine> = lines.iter().filter(|l| l.dim).collect();
        assert!(!dimmed.is_empty(), "the resolver dims pane.delete");
        for l in dimmed {
            assert!(
                l.spans.iter().any(|s| !s.text.trim().is_empty()),
                "a dimmed row still says something"
            );
        }
    }

    #[test]
    fn el_mapa_de_acciones_apunta_a_lineas_existentes() {
        let topic = norte_help::topic(Lang::En, "copying").expect("corpus topic");
        let lines = render_topic(topic, Lang::En, &Fixed);
        let actions: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter_map(|(i, l)| l.action.map(|_| i))
            .collect();
        assert!(!actions.is_empty(), "the page has runnable rows");
    }

    #[test]
    fn toda_pagina_de_plugin_lleva_insignia_aunque_no_declare_nada() {
        let parsed = norte_help::parse_untrusted(b"body", "acme.ftp", None);
        let lines = render_topic(&parsed.topic, Lang::En, &Fixed);
        assert!(
            lines.iter().take(3).any(|l| l.badge),
            "the provenance line is what tells third-party prose from ours: it is unconditional"
        );
    }

    #[test]
    fn el_texto_hostil_de_un_plugin_llega_ya_enmascarado() {
        let parsed = norte_help::parse_untrusted(
            "a \u{202e}reversed\u{202e} paragraph\n".as_bytes(),
            "acme.ftp",
            None,
        );
        let lines = render_topic(&parsed.topic, Lang::En, &Fixed);
        for l in &lines {
            for s in &l.spans {
                assert!(!s.text.contains('\u{202e}'), "bidi override painted: {:?}", s.text);
            }
        }
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd crates/norte-gui && cargo nextest run help_render`
Expected: FAIL — `cannot find function render_topic` / `cannot find type HelpLine`.

- [ ] **Step 3: Implement**

Write the module above the tests. Shape (fill in every `Block` arm of
`norte_help::Block` — `Heading`, `Paragraph`, `Bullets`, `Code`, `Table`,
`Callout` — plus the command rows and the `see_also` links, exactly as
`crates/norte-tui/src/help_render.rs::render_topic` does; read it and mirror the
ORDER it emits, so the two frontends show the same page in the same sequence):

```rust
//! Laying a help topic out for the GUI: [`norte_help::Topic`] → semantic lines.
//!
//! Pure and GPUI-free on purpose: `main.rs` turns each [`HelpLine`] into
//! elements, and everything decided here (order, roles, the plugin badge, which
//! line an action landed on) is therefore unit-testable. Unlike the TUI's
//! renderer this one does NOT wrap — GPUI wraps text itself.
//!
//! # This module masks NOTHING
//!
//! A claim about its INPUTS. A built-in topic is trusted text gated by
//! `norte-help`'s corpus sweep; a plugin topic was already masked and bounded by
//! `norte_help::parse_untrusted`; a chord was masked by the resolver
//! (`crate::help_view::GuiChords`, through `norte_frontend::keymap::paint_chord`).
//! A caller feeding this an ungated corpus must mask before calling in.

use norte_help::{Availability, Block, Callout, ChordResolver, Lang, Span, Topic};
use norte_theme::Role;

/// One painted fragment: text plus the theme role that colours it.
#[derive(Debug, Clone)]
pub struct HelpSpan {
    /// Text to paint, already safe (see the module doc).
    pub text: String,
    /// Theme role.
    pub role: Role,
    /// Monospace (inline code, code fences).
    pub mono: bool,
}

/// One line of the body.
#[derive(Debug, Clone, Default)]
pub struct HelpLine {
    /// Fragments, in paint order.
    pub spans: Vec<HelpSpan>,
    /// Indentation depth, in steps (bullets, code, table rows).
    pub indent: u8,
    /// Index into `HelpState::actions` when this line IS an action row.
    pub action: Option<usize>,
    /// Dimmed: the row's command cannot run now.
    pub dim: bool,
    /// The provenance/badge line of a plugin page.
    pub badge: bool,
}

/// Lays `topic` out. `lang` resolves link titles and the badge's Fluent keys;
/// `r` resolves `{{cmd:…}}` marks, row labels and availability.
#[must_use]
pub fn render_topic(topic: &Topic, lang: Lang, r: &(impl ChordResolver + ?Sized)) -> Vec<HelpLine> {
    // 1. title line (Role::StatusBar)
    // 2. plugin badge, UNCONDITIONAL for Origin::Plugin — `help-plugin-origin`,
    //    `help-plugin-by`, `help-plugin-truncated`, `help-plugin-lossy`
    // 3. blocks, in corpus order
    // 4. command rows (`norte_help::rows_of`), each with `action: Some(i)` and
    //    `dim` from its `Availability`; the reason via
    //    `norte_frontend::availability::reason_key` → `norte_i18n::t_in`
    // 5. see_also links, continuing the SAME action index sequence
    todo!("write it out; the tests above pin the observable parts")
}
```

The `todo!()` is a scaffold marker for the implementer, **not** something that
ships: the task is not done until it is gone and the five tests pass. Two
details that are easy to get wrong and are pinned by the tests:

* the action indices must continue across commands → links in ONE sequence,
  because `HelpState::actions()` is one flat list in that order;
* the badge is emitted for every `Origin::Plugin`, even when the plugin
  declared no publisher and no flags — a plugin must not be able to make the
  one line that marks its prose as third-party disappear by declaring nothing.

- [ ] **Step 4: Run the tests**

Run: `cd crates/norte-gui && cargo nextest run help_render`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```sh
cd crates/norte-gui && cargo clippy --all-targets -- -D warnings && cargo fmt
git add crates/norte-gui/src/help_render.rs crates/norte-gui/src/main.rs
git commit -m "feat(gui): lay a help topic out as semantic lines the painter can test (H3f)"
```

---

### Task 6: `app.help` joins `COMMANDS`, and F1 opens the overlay

**Files:**
- Modify: `crates/norte-gui/src/keymap.rs` (`COMMANDS`, around line 16)
- Modify: `crates/norte-gui/src/main.rs` (`NorteGui` field ~line 354, `dispatch` arm ~line 2025, key capture in `on_key`)
- Test: `crates/norte-gui/src/keymap.rs` test module (reachability pin) and `palette_view.rs`'s existing i18n pin

- [ ] **Step 1: Write the failing test**

In `crates/norte-gui/src/keymap.rs`'s test module:

```rust
    #[test]
    fn app_help_esta_en_commands_y_el_preset_compartido_lo_alcanza() {
        assert!(COMMANDS.contains(&"app.help"), "F1 was being dropped silently");
        let (browse, _) = build_effectives_preset_only("orthodox");
        assert!(
            browse.bindings().iter().any(|(_, cmd)| cmd == "app.help"),
            "the shared preset binds f1 → app.help; COMMANDS is what lets it through"
        );
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd crates/norte-gui && cargo nextest run keymap::tests::app_help`
Expected: FAIL — `assertion failed: COMMANDS.contains(&"app.help")`.

- [ ] **Step 3: Implement**

`keymap.rs`, in `COMMANDS`, right after `"app.settings"`:

```rust
    // H3f: the shared presets have bound `f1` → `app.help` since H3a, but the
    // GUI's `COMMANDS` did not list it, so `Effective::build_for_subset`
    // dropped the binding and F1 did nothing — silently. No supplement needed:
    // the chord comes from the shared catalogue, like `app.palette`.
    "app.help",
```

`main.rs`:

1. field, beside `palette` / `extensions`:

```rust
    /// The help overlay (H3f, `F1`): `Some` while open. Same key-capture slot
    /// and z-order as the palette — a modal still wins.
    help: Option<help_view::HelpView>,
    /// The resolver the open page was painted through, frozen when it opened
    /// ([`help_view::HelpView::freeze`]).
    help_chords: Option<help_view::GuiChords>,
```

Initialise both to `None` in the two constructors (~lines 1027 and 1113).

2. dispatch arm, beside `"app.palette"`:

```rust
            "app.help" => self.open_help(cx),
```

3. the opener and the key handler:

```rust
    /// Opens the help on the index, freezing the resolver the page is painted
    /// through, and asks for the plugin catalogue (the sidebar's Extensions
    /// group). A catalogue that never arrives leaves the group empty, which is
    /// the honest answer to "I could not find out".
    fn open_help(&mut self, cx: &mut Context<Self>) {
        let (browse, viewer) = self.effectives();
        let mut view = help_view::HelpView::new(
            self.lang,
            help_view::keys_lines(&browse, &viewer),
        );
        view.set_plugins(&self.plugins);
        let base = help_view::GuiChords::new(&browse, &viewer, self.lang);
        self.help_chords = Some(view.freeze(&base, self.help_facts()));
        self.help = Some(view);
        self.send(session::SessionCmd::PluginsList);
        cx.notify();
    }

    /// Routes a key to the open help overlay.
    fn on_help_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) {
        let Some(chords) = self.help_chords.clone() else {
            return;
        };
        let Some(view) = &mut self.help else {
            return;
        };
        match help_view::on_key(view, &ks.key, ks.key_char.as_deref(), &chords) {
            help_view::HelpOutcome::None => {}
            help_view::HelpOutcome::Close => {
                self.help = None;
                self.help_chords = None;
            }
            help_view::HelpOutcome::Run(key) => { /* task 7 */ }
            help_view::HelpOutcome::Blocked(reason) => { /* task 7 */ }
            help_view::HelpOutcome::Palette(filter) => { /* task 7 */ }
        }
        cx.notify();
    }
```

`self.effectives()`, `self.lang`, `self.plugins`, `self.help_facts()` and
`self.send(...)` are placeholders for whatever the surrounding code already
calls these things — read `open_palette` (~line 2408) and `open_extensions`
(~line 2614) and use the same accessors. If there is no `help_facts()`, build
`Facts` with `context_menu::facts_for(...)` from the active pane's cursor
exactly as the context menu does, and add it as a small private method next to
it.

4. key capture: in `on_key`, add a `self.help.is_some()` branch with the same
   priority as `palette` (after the modal guard, before the dual-pane arm), and
   make a modal ARRIVING close the help, exactly as it closes the palette
   (search for `modal_preempts_palette`).

- [ ] **Step 4: Run the tests**

Run: `cd crates/norte-gui && cargo nextest run` and `just check-gui`
Expected: all green; `todo_comando_gui_tiene_ayuda_traducida_en_ambos_locales`
still passes (`help-cmd-app-help` already exists in both locales) and
`todo_comando_gui_es_alcanzable_desde_el_preset_default` passes without a
supplement.

- [ ] **Step 5: Commit**

```sh
cd crates/norte-gui && cargo clippy --all-targets -- -D warnings && cargo fmt
git add crates/norte-gui/src/keymap.rs crates/norte-gui/src/main.rs
git commit -m "feat(gui): F1 opens the help instead of being dropped (H3f)"
```

---

### Task 7: Enter dispatch, blocked rows, and the palette handoff

**Files:**
- Modify: `crates/norte-gui/src/main.rs`
- Modify: `crates/norte-i18n/i18n/en.ftl`, `crates/norte-i18n/i18n/es.ftl`
- Test: `crates/norte-gui/src/help_view.rs` (handoff), workspace i18n parity suite

- [ ] **Step 1: Write the failing test**

In `help_view.rs`:

```rust
    #[test]
    fn ctrl_p_entrega_el_filtro_a_la_paleta() {
        let (browse, viewer) = effectives();
        let chords = GuiChords::new(&browse, &viewer, norte_i18n::Lang::En);
        let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
        on_key(&mut view, "/", Some("/"), &chords);
        for c in "copy".chars() {
            on_key(&mut view, &c.to_string(), Some(&c.to_string()), &chords);
        }
        match handoff(&view) {
            HelpOutcome::Palette(q) => assert_eq!(q, "copy"),
            other => panic!("expected Palette, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd crates/norte-gui && cargo nextest run help_view::tests::ctrl_p`
Expected: FAIL — `cannot find function handoff`.

- [ ] **Step 3: Implement**

In `help_view.rs`:

```rust
/// `ctrl+p` from the help: the palette opens carrying the filter the reader
/// already typed. Two views of one model at two densities — crossing between
/// them must not cost a re-type.
///
/// The RAW filter, not the display one: the palette applies its own masking
/// when it paints, and a masked query would no longer match what the reader
/// meant. `main.rs` calls this before `on_key` (the modifier gate there strips
/// `ctrl` before the pure router ever sees the key).
#[must_use]
pub fn handoff(view: &HelpView) -> HelpOutcome {
    HelpOutcome::Palette(view.state.filter_raw().to_owned())
}
```

In `main.rs`, complete the three arms of `on_help_key`:

```rust
            help_view::HelpOutcome::Run(key) => {
                self.help = None;
                self.help_chords = None;
                if let Some((id, command)) = parse_plugin_palette_key(&key) {
                    self.run_plugin_command(id, command, cx);
                } else {
                    self.dispatch(&key, cx);
                }
            }
            help_view::HelpOutcome::Blocked(reason) => {
                let key = norte_frontend::availability::reason_key(reason);
                self.set_status(norte_i18n::t(key), false);
            }
            help_view::HelpOutcome::Palette(filter) => {
                self.help = None;
                self.help_chords = None;
                self.open_palette();
                if let Some(p) = &mut self.palette {
                    for c in filter.chars() {
                        p.push_char(c);
                    }
                }
            }
```

Use the REAL names for the plugin dispatch and the status setter — read the
`PaletteOutcome::Run` arm (~line 2434) and copy exactly what it does, including
its plugin-key branch. If the GUI has no general status line, paint the blocked
reason where the palette's hint line goes rather than inventing a new surface.

Intercept `ctrl+p` while the help is open, in the same place the GUI gates
modifiers before calling the pure router:

```rust
        if self.help.is_some() && ks.modifiers.control && ks.key == "p" {
            let out = self.help.as_ref().map(help_view::handoff);
            // …same arms as above
        }
```

Fluent: add the footer hint the overlay paints (both locales, same message ids
— the workspace i18n parity test fails otherwise):

```ftl
# en.ftl, next to `palette-hint`
help-hint-gui = ⇥ pane · ⏎ run · / filter · ⌫ back · Ctrl+P palette · Esc close
```

```ftl
# es.ftl
help-hint-gui = ⇥ panel · ⏎ ejecutar · / filtrar · ⌫ atrás · Ctrl+P paleta · Esc cerrar
```

- [ ] **Step 4: Run the tests**

Run: `cd crates/norte-gui && cargo nextest run` then `just t norte-i18n`
Expected: both green (the i18n crate's parity suite covers the new key).

- [ ] **Step 5: Commit**

```sh
cd crates/norte-gui && cargo clippy --all-targets -- -D warnings && cargo fmt
git add crates/norte-gui/src crates/norte-i18n/i18n
git commit -m "feat(gui): running from the help goes through the palette's dispatch (H3f)"
```

---

### Task 8: plugin pages on demand (`plugin.help`)

**Files:**
- Modify: `crates/norte-gui/src/session.rs` (`SessionCmd` ~line 152, `SessionEvent` ~line 365, the command loop ~line 567)
- Modify: `crates/norte-gui/src/main.rs` (`apply_event`, and the poll after every help key)
- Test: `crates/norte-gui/src/help_view.rs` already pins the claim/install halves (task 3); this task adds the transport

- [ ] **Step 1: Write the failing test**

`session.rs` is a thin transport, so the test that earns its keep is the one
that pins the SHAPE of the round trip. In `help_view.rs`'s test module:

```rust
    #[test]
    fn una_pagina_que_no_llega_deja_la_pagina_vacia_y_no_se_repregunta() {
        let mut view = HelpView::new(norte_i18n::Lang::En, Vec::new());
        view.set_plugins(&[info("acme.ftp", true, true, true)]);
        view.state.open(&norte_help::TopicId::new("acme.ftp"));
        assert_eq!(view.claim_plugin_fetch().as_deref(), Some("acme.ftp"));
        // The daemon never answers (N-1 without the handler, or a failure).
        assert_eq!(view.claim_plugin_fetch(), None);
        assert!(
            view.state.current_topic().is_none(),
            "an empty page with the plugin's name beats an error toast over the help"
        );
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd crates/norte-gui && cargo nextest run help_view::tests::una_pagina_que_no_llega`
Expected: PASS already if task 3 landed — in that case treat this step as the
regression pin and move to step 3 (the transport is what is missing).

- [ ] **Step 3: Implement the transport**

`session.rs`, in `SessionCmd`:

```rust
    /// The `help.md` of ONE plugin, ON DEMAND (H3f, `plugin.help`, 0.34.0):
    /// 64 KiB per plugin must not ride every `plugin.list`.
    PluginHelp {
        /// Plugin id whose page the help overlay just opened.
        id: String,
    },
```

in `SessionEvent`:

```rust
    /// Answer to [`SessionCmd::PluginHelp`]. The markdown is NOT masked — it
    /// carries verbatim whatever the plugin wrote — and is parsed with
    /// `norte_help::parse_untrusted`, which masks while building the model. It
    /// is never painted or logged raw.
    PluginHelpReady {
        /// Plugin the page belongs to.
        id: String,
        /// The bounded result off the wire.
        result: norte_proto::methods::PluginHelpResult,
    },
```

in the command loop, beside `SessionCmd::PluginGetConfig`:

```rust
                    SessionCmd::PluginHelp { id } => {
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            // A failure is SILENT: an empty page with the
                            // plugin's name beats an error toast over the help,
                            // and a daemon N-1 without the handler lands here
                            // too. Closing and reopening the help is the retry.
                            if let Ok(result) = backend.plugin_help(&id).await {
                                let _ = tx.send(SessionEvent::PluginHelpReady { id, result });
                            }
                        });
                    }
```

`main.rs`, in `apply_event`:

```rust
            SessionEvent::PluginHelpReady { id, result } => {
                if let Some(view) = &mut self.help {
                    view.install_plugin_page(&id, &result);
                    cx.notify();
                }
            }
```

and, after every help key (end of `on_help_key`) plus after
`SessionEvent::PluginsListed` while the help is open, drain the claim:

```rust
        if let Some(id) = self.help.as_mut().and_then(help_view::HelpView::claim_plugin_fetch) {
            self.send(session::SessionCmd::PluginHelp { id });
        }
```

`PluginsListed` must also refresh the snapshot and RE-FREEZE the resolver while
the overlay is open (`view.set_plugins(&plugins)` then rebuild
`self.help_chords`), for the same reason the TUI re-freezes: a stale snapshot
dims rows for a catalogue that no longer applies.

- [ ] **Step 4: Verify**

Run: `cd crates/norte-gui && cargo nextest run && cargo clippy --all-targets -- -D warnings`
Expected: green.

- [ ] **Step 5: Commit**

```sh
cd crates/norte-gui && cargo fmt
git add crates/norte-gui/src
git commit -m "feat(gui): a plugin's help page is fetched once, when the reader opens it (H3f)"
```

---

### Task 9: paint it (GPUI) — themed and effects-aware

**Files:**
- Modify: `crates/norte-gui/src/main.rs` (`render_help`, called from the same place `render_palette` is)
- Test: `crates/norte-gui/src/help_render.rs` (the layout is already covered); this task adds the theme mapping pin

- [ ] **Step 1: Write the failing test**

In `main.rs`'s test module (or `theme_map.rs`'s, wherever the GUI already pins
role→colour mapping — grep for `to_gpui_rgba` in tests):

```rust
    #[test]
    fn los_roles_de_la_ayuda_resuelven_en_los_temas_instalados() {
        for name in norte_theme::builtin_names() {
            let theme = norte_theme::load_builtin(name).expect("builtin theme");
            for role in [
                norte_theme::Role::Regular,
                norte_theme::Role::StatusBar,
                norte_theme::Role::Selection,
                norte_theme::Role::Error,
            ] {
                // Resolving must not panic and must not fall through to a
                // transparent colour: a help page painted invisible is a page
                // the reader cannot read.
                let c = theme_map::to_gpui_rgba(theme.style(role).fg.unwrap_or_default());
                assert!(c.a > 0.0, "{name}/{role:?} resolved transparent");
            }
        }
    }
```

Adjust the API names to whatever `norte-theme` really exports (grep
`builtin_names`/`load_builtin` first; if the GUI resolves through
`ChromeColors::resolve`, pin that instead — the point is that the four roles the
help paints with resolve in every shipped theme).

- [ ] **Step 2: Run it to verify it fails or passes**

Run: `cd crates/norte-gui && cargo nextest run los_roles_de_la_ayuda`
Expected: FAIL if a role is missing a fallback; otherwise it stands as the pin.

- [ ] **Step 3: Implement `render_help`**

Mirror `render_palette` (~line 4405) and `render_extensions` (~line 4585) for
chrome, ids, ARIA roles and fonts. Structure:

```rust
    /// Paints the help overlay (H3f): sidebar (topic groups + Extensions),
    /// body (the [`help_render::HelpLine`]s of the open page), filter header and
    /// hint footer. Same visual language as the palette — this is the same
    /// model at a lower density, not a second design.
    ///
    /// Nothing here masks: every string arrives already safe (see
    /// `help_render`'s module doc).
    fn render_help(
        &self,
        view: &help_view::HelpView,
        chrome: &ChromeColors,
    ) -> impl IntoElement {
```

Requirements the reviewers will look for:

* the sidebar paints `SidebarRow::Group { tag }` through
  `norte_i18n::t(&format!("help-group-{tag}"))` and `SidebarRow::Topic { title }`
  verbatim (the title is already the corpus' own, masked at parse for plugins);
* the body paints the keyboard page from `view.keys_lines` when
  `view.state.current().as_str() == norte_frontend::help::KEYS_ID`, and an EMPTY
  body — never the cheatsheet — for a plugin page still in flight (the whole
  sheet under an extension's name would read as that extension's documentation);
* a `HelpLine` with `dim` uses the dimmed colour and its reason text, and is not
  selectable-looking;
* the focused action row is highlighted with `chrome.sel_bg`, and the focused
  half is the one whose border is `chrome.border_focus`;
* the footer paints `norte_i18n::t("help-hint-gui")`;
* effects: nothing to do beyond painting inside the same container the other
  overlays use — `[effects]` (ADR 0036) is applied by the window-level layer in
  `effects.rs`, so an overlay inherits it. Verify by eye with
  `NORTE_THEME=retro-crt` (or whatever the crate's env/config knob is — grep
  `EffectsV1::from_theme`'s caller) and confirm the help is scanlined like the
  panes.

- [ ] **Step 4: Verify by running the app**

```sh
cd crates/norte-gui && cargo run --locked
```

Press `F1`: the index opens. `/` filters, `⇥` swaps halves, `⏎` on a command row
runs it and closes, `Esc` closes. With a plugin installed and approved, its row
appears under Extensions and opens its page with the provenance badge.

- [ ] **Step 5: Commit**

```sh
cd crates/norte-gui && cargo clippy --all-targets -- -D warnings && cargo fmt
git add crates/norte-gui/src/main.rs
git commit -m "feat(gui): paint the help overlay in the active theme (H3f)"
```

---

### Task 10: close the phase

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `docs/superpowers/specs/2026-08-04-help-system-redesign-design.md` (mark H3f done, if the doc tracks phase state)

- [ ] **Step 1: CHANGELOG entry**

Under `## [Unreleased]`, `### Added`:

```markdown
- GUI: `F1` opens the help — the same corpus, model and executable rows as the
  TUI, painted in the active theme, with a page per extension fetched on demand
  (`plugin.help`). `app.help` joined the GUI's command table, so the shared
  preset's `F1` is no longer dropped silently (H3f).
```

Note under `### Known limitations` anything left undone, in the honest register
the H3e entry used (for instance: the GUI has no contextual `F1` — every open
lands on the index — because the GUI has no `dialog` context to map from).

- [ ] **Step 2: Full gate**

```sh
just gui-ci
just ci
```

Expected: both EXIT=0. `just ci` matters even though the GUI is excluded: task 7
touches `crates/norte-i18n`, which is in the workspace and has a parity suite.

- [ ] **Step 3: Reviewers**

The design assigns H3f the `rust` reviewer. Given what this phase touches, also
run `encoding-auditor` — it carries third-party strings (plugin names,
publishers, command titles, `help.md` bodies) into a painter, which is exactly
the seam H3e's audit found bugs in twice.

- [ ] **Step 4: Commit**

```sh
git add CHANGELOG.md docs/superpowers/specs/2026-08-04-help-system-redesign-design.md
git commit -m "docs(gui): record the GUI help view (H3f)"
```

---

## Self-review notes

* **Spec coverage.** Design's GUI section asks for: same `norte-help` model
  (tasks 3, 5), GPUI view with sidebar/body/filter (tasks 5, 9), same key
  vocabulary (task 4), `app.help` in the GUI's `COMMANDS` (task 6), the active
  theme and `[effects]` (task 9). Plugin help (task 8) is H3e's wire consumed
  here. The design's H3f test bullet — "GUI view test with themed and
  effects-enabled render" — is task 9 step 1 plus the layout suite in task 5.
* **Deliberately out of scope**, to be stated in the CHANGELOG rather than
  silently skipped: contextual `F1` (H3c is TUI-only; the GUI has no `dialog`
  context table), the palette's reverse `F1`-on-a-row affordance, and
  `app.help` in `VIEWER_COMMANDS` (the GUI viewer captures all keys through its
  own resolver — adding it there is a separate decision about viewer key
  routing, not about the help).
* **Known fixture risk.** H3e burned three tasks on the assumption that `id` is
  optional in `help.md` front matter — it is REQUIRED. Task 3's
  `install_enmascara_la_pagina…` test therefore writes a complete header. If a
  parse comes back with a fallback title, check the header before blaming the
  masker.
