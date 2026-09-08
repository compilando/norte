//! The frontend's answer to the three questions `norte-help` asks (H3b): the
//! reader's own chord for a command, a short label, and whether it can run in
//! the context the page was opened in.
//!
//! It lives here and not in a frontend because BOTH graphical surfaces ask
//! them, and the answers are presentation rules — which screen's keymap a
//! command is resolved in, when a label falls back to the dispatch key, when
//! third-party text is masked. Two copies are two help pages that teach
//! different keys for the same command without anyone noticing (ADR 0066,
//! decision D14). The TUI re-exports this type as `Chords`.
//!
//! Every chord that leaves here goes through
//! [`crate::keymap::paint_chord`]. `Chord`'s `Display` is raw and
//! lower case ON PURPOSE (logs and debug output want the real chord), and a
//! project `./.norte/keymap.toml` carries no trust, so masking — and then the
//! conventional spelling — is done once, here, rather than at each call site.

use std::collections::HashMap;

use norte_help::{Availability, ChordResolver};

use crate::availability::Facts;
use crate::help_badge::plugin_label;
use crate::keymap::{Effective, Screen, paint_chord};
use crate::whichkey::label_id;

/// What a plugin-contributed command's dispatch key starts with — the same
/// prefix [`crate::availability::plugin_of_command`] strips, and that
/// function stays the authority on whether a key is WELL FORMED. This constant
/// answers the looser question [`Chords::label`] needs: does this key claim
/// to be a plugin's, and therefore carry text nobody in this process wrote?
const PLUGIN_KEY_PREFIX: &str = "plugin:";

/// The facts of a context with nothing in the way: what [`Chords`] answers
/// against until the overlay opens and freezes the real ones.
///
/// Everything permissive, so the table vetoes nothing. Not `Default`, because
/// "all false" is what a derive would give and that is the OPPOSITE of
/// permissive here — `enterable: false` alone would dim `nav.enter` on every
/// page painted through a resolver nobody had frozen yet.
const NO_IMPEDIMENT: Facts = Facts {
    enterable: true,
    viewable: true,
    rename_single: true,
    source_read_only: false,
    dest_read_only: false,
    degraded: false,
    journalled: true,
};

/// A frontend's answer to the three questions `norte-help` asks (H3b): the
/// user's effective chord, a short label, and whether the command can run now.
///
/// **Must be rebuilt wherever the effective keymap is** — startup and the
/// hot-reload arm of a config change — and from the same effectives, for the
/// same reason: a rebind that does not reach this resolver is a help page that
/// teaches the OLD key. It holds no borrows precisely so the rebuild can be a
/// whole new value swapped in.
///
/// ```
/// use norte_frontend::help_chords::Chords;
/// use norte_frontend::keymap::{Effective, Screen, parse_keymap};
/// use norte_help::{ChordResolver, CommandText, render_command};
///
/// let src = r#"
/// [pane]
/// keymap = [{ on = ["f5"], run = "pane.copy" }]
///
/// [viewer]
/// keymap = [{ on = ["f3"], run = "viewer.close" }]
/// "#;
/// let preset = parse_keymap(src).unwrap();
/// let known = ["pane.copy", "viewer.close"];
/// let eff = |s| Effective::build_for(&preset, &[], &known, s).unwrap();
/// let r = Chords::new(
///     &eff(Screen::Browse),
///     &eff(Screen::Viewer),
///     &eff(Screen::Dialog),
///     norte_i18n::Lang::En,
/// );
///
/// // A `{{cmd:pane.copy}}` mark in the corpus becomes the reader's own key,
/// // spelled the way the documentation spells it (`paint_chord`).
/// assert_eq!(
///     render_command("pane.copy", &r),
///     CommandText::Chord("F5".to_owned()),
/// );
/// // A command of ANOTHER screen resolves in the screen it lives in.
/// assert_eq!(r.chord("viewer.close").as_deref(), Some("F3"));
/// // A command with no key names itself rather than inventing one.
/// assert_eq!(r.chord("no.such.command"), None);
/// ```
#[derive(Debug)]
// El campo `chords` repite el nombre del tipo, y es el nombre correcto de las
// dos cosas: el tipo ES el resolver de acordes y el campo ES su mapa. Cualquier
// otro nombre («map», «por_comando») describiría peor lo que hay dentro.
#[expect(
    clippy::struct_field_names,
    reason = "el tipo ES el resolver de acordes y el campo ES su mapa"
)]
pub struct Chords {
    /// Command to its painted chord, filled browse → viewer → dialog.
    ///
    /// Precomputed rather than resolved per row (M1): `Effective::bindings`
    /// materialises the WHOLE keymap — every chord formatted into a fresh
    /// `String` — and asking it once per screen per rendered mark, per frame,
    /// is the cost this map pays once. Same lineage as `DialogHints`, which
    /// precomputes its finished strings for the same reason.
    ///
    /// The FILL ORDER is the rule [`ChordResolver::chord`] states: resolve a
    /// command in the screen THAT COMMAND lives in. A `viewer.*` command is
    /// not in the browse keymap and a `dialog.*` verb is in neither, so a map
    /// built from one screen would report "no key bound" for most of the
    /// vocabulary. First writer wins, so a command bound in several screens
    /// keeps its browse chord — the same precedence a browse-first or-chain
    /// would have had.
    chords: HashMap<String, String>,
    /// The language `label` answers in. Also the language whose catalogue
    /// decides whether there IS an answer; see [`ChordResolver::label`].
    lang: norte_i18n::Lang,
    /// The context [`ChordResolver::availability`] answers against, FROZEN
    /// when the overlay opened ([`Self::with_facts`], called by
    /// `App::freeze_help_facts` in the TUI, `Controller` in the GUI host).
    ///
    /// Frozen and not read live, which is the same decision the GUI's context
    /// menu makes and for the same reason: the reader walks a page whose rows
    /// were dimmed under one set of facts, and a row that changed verdict
    /// halfway down — because the cursor moved, or a frame was laid out at a
    /// different width — would make the page disagree with itself.
    ///
    /// What the freeze buys is "no verdict changes because the READER moved",
    /// and only that. It is NOT stale for the whole lifetime of the overlay,
    /// because two of these facts do go out of date on their own: the two
    /// read-only ones cannot change without a `cd`, which needs a key the help
    /// is eating, but `enterable` and `viewable` describe the entry under the
    /// cursor, and a copy or a delete finishing while the help is open re-lists
    /// both panes underneath it (the `tick` arm of the run loop has no overlay
    /// guard, unlike the `dir_watch` one). So the refresh funnel re-freezes —
    /// `main::after_panes_refresh`, which all three refresh triggers go
    /// through. Without it a row said "does not apply to this selection" about
    /// a selection that no longer existed.
    ///
    /// Before the first freeze it is [`Facts`] with nothing impeded, so the
    /// resolver dims NOTHING. That is the table's own fail-open default
    /// ([`crate::availability::verdict`]) applied one level up: a
    /// resolver built by a hot reload and not yet frozen must not start
    /// claiming commands are broken.
    facts: Facts,
    /// Plugin ids that are approved AND enabled, snapshotted when the overlay
    /// opened (H3e). Empty means "nothing active" and dims every plugin row,
    /// which is the right answer both when there are no plugins and when the
    /// snapshot could not be taken: offering a row `plugin.run_command` would
    /// refuse is the worse mistake.
    ///
    /// Note the asymmetry with [`Self::facts`], which defaults to fail-OPEN.
    /// It is not an inconsistency: the facts table is a list of known
    /// IMPEDIMENTS, so "I have not looked" means "no impediment known", while
    /// a plugin set is an ALLOWLIST and "I have not looked" means "I cannot
    /// vouch for any of them". Same principle in both — do not claim what has
    /// not been established.
    active_plugins: std::collections::BTreeSet<String>,
    /// Dispatch key (`plugin:{plugin_id}:{command_id}`) to the title the
    /// MANIFEST gives that command, snapshotted with [`Self::active_plugins`]
    /// from the same `plugin.list` (H3e).
    ///
    /// Without it a plugin's own page named its commands by their raw dispatch
    /// key — `plugin:org.norte.demo:greet` where the manifest says «Greet the
    /// world» — in the prose AND in the row below it. The chain is
    /// `norte_help::render_command` → no chord → `label_or_id` →
    /// [`ChordResolver::label`], and a `plugin:` key has no `help-cmd-*` entry
    /// BY CONSTRUCTION, so the fallback to the id was guaranteed on the one
    /// page where every row is a plugin command.
    ///
    /// The titles come from the SNAPSHOT and never from the `help.md`. A plugin
    /// authors both, so only one of them can be the answer, and it has to be
    /// the one the extension manager shows the human who approves the plugin —
    /// otherwise a page could call `greet` one thing while the manager, the
    /// palette and the approval prompt call it another.
    ///
    /// Values are already masked and capped (`plugin_label`): the
    /// resolver hands strings straight to a painter, so anything it carries has
    /// to be paintable already.
    plugin_labels: HashMap<String, String>,
}

/// Compile anchor: a fourth `Screen` must not silently make `chord` answer
/// `None` for every command that lives in it. A new variant fails to compile
/// HERE, next to the fill that has to grow with it. (Same idiom as
/// `CONTEXTOS` in `tests/help_gate.rs`.)
const _: fn(Screen) = |screen| match screen {
    Screen::Browse | Screen::Viewer | Screen::Dialog => (),
};

impl Chords {
    /// Takes the three effective keymaps — one per [`Screen`] — and the
    /// language `label` answers in.
    ///
    /// BORROWS them, like `DialogHints::build` and
    /// `norte_tui::palette::build_rows` and for the same reason: `main.rs`
    /// moves those effectives into the shared `Resolver`, so everything
    /// precomputed from them has to be built alongside the other two, before
    /// the move, without forcing a clone. Nothing is retained — the chords
    /// are copied out here (see `Chords`' chord map).
    #[must_use]
    pub fn new(
        browse: &Effective,
        viewer: &Effective,
        dialog: &Effective,
        lang: norte_i18n::Lang,
    ) -> Self {
        Self::over(&[browse, viewer, dialog], lang)
    }

    /// The same, over the screens a frontend ACTUALLY has.
    ///
    /// The TUI has the three; the graphical host has two — its dialogs are
    /// answered by the renderer's own buttons, so there is no `dialog` keymap
    /// to resolve a `dialog.*` verb in and pretending otherwise would put a
    /// key on a page that nothing would press. A command with no screen here
    /// simply has no chord, which is what [`ChordResolver::chord`] means by
    /// `None`.
    ///
    /// Order is PRECEDENCE: first writer wins, so a command bound in several
    /// screens keeps the earliest one's chord.
    #[must_use]
    pub fn over(effectives: &[&Effective], lang: norte_i18n::Lang) -> Self {
        let mut chords: HashMap<String, String> = HashMap::new();
        for eff in effectives {
            for (seq, cmd) in eff.bindings() {
                // `bindings()` is already in precedence order and `or_insert`
                // keeps the first writer, so within a screen this picks the
                // binding that actually FIRES, and across screens it picks
                // the earlier screen.
                chords
                    .entry(cmd.to_owned())
                    .or_insert_with(|| paint_chord(&seq));
            }
        }
        Self {
            chords,
            lang,
            facts: NO_IMPEDIMENT,
            active_plugins: std::collections::BTreeSet::new(),
            plugin_labels: HashMap::new(),
        }
    }

    /// The language this resolver answers in — the same one its labels and
    /// its verdicts were built with.
    ///
    /// Asked of the resolver rather than threaded alongside it, because they
    /// are the same language by construction, and two sources is where a page
    /// ends up with its title in one and its prose in the other.
    #[must_use]
    pub fn lang(&self) -> norte_i18n::Lang {
        self.lang
    }

    /// The same resolver answering [`ChordResolver::availability`] against
    /// `facts`.
    ///
    /// Returns a NEW value instead of mutating: the open overlay holds an
    /// `Arc` of the resolver it was laid out with, and the freeze happens by
    /// swapping a fresh one in (`App::freeze_help_facts` in the TUI, `Controller` in the GUI host) exactly
    /// as the hot reload swaps a rebuilt one. Nothing that a page has already
    /// been painted through can change underneath it.
    ///
    /// The chord map is cloned, once per help open. That is the whole cost, and
    /// the alternative — resolving facts per rendered row — is what the frozen
    /// snapshot exists to avoid.
    #[must_use]
    pub fn with_facts(&self, facts: Facts) -> Self {
        Self {
            chords: self.chords.clone(),
            lang: self.lang,
            facts,
            active_plugins: self.active_plugins.clone(),
            plugin_labels: self.plugin_labels.clone(),
        }
    }

    /// The same resolver, carrying the plugin snapshot (H3e): which plugins are
    /// active, and what the manifest calls each of their commands.
    ///
    /// BOTH halves in one builder because they are one photograph — the same
    /// `plugin.list`, the same instant — and a resolver holding a fresh active
    /// set beside stale titles would dim a row correctly while naming it wrong.
    ///
    /// A NEW value, for the reason [`Self::with_facts`] gives, and the two
    /// compose in either order: each carries the other's fields over, so the
    /// re-freeze the refresh funnel performs while the overlay is open
    /// (`main::after_panes_refresh`) cannot drop the snapshot and dim every
    /// plugin row halfway down a page — nor take their names away.
    #[must_use]
    pub fn with_plugins(
        &self,
        active: std::collections::BTreeSet<String>,
        titles: HashMap<String, String>,
    ) -> Self {
        Self {
            chords: self.chords.clone(),
            lang: self.lang,
            facts: self.facts,
            active_plugins: active,
            plugin_labels: titles,
        }
    }
}

impl ChordResolver for Chords {
    /// The command's chord in the screen it belongs to, masked — see
    /// `Chords`' chord map for why the map is filled the way it is, and
    /// `paint_chord` for why nothing raw leaves here.
    fn chord(&self, command: &str) -> Option<String> {
        self.chords.get(command).cloned()
    }

    /// The catalogue's short label, or an EMPTY string when it has no entry.
    ///
    /// A `plugin:{id}:{command}` key is answered from the SNAPSHOT instead
    /// (H3e): the app's Fluent catalogue cannot possibly hold an entry for a
    /// command a third party declared, so the generic path below returns blank
    /// and `norte_help::label_or_id` falls back to the raw dispatch key —
    /// which is how a plugin's own page came to name its commands
    /// `plugin:org.norte.demo:greet` while the manifest, the palette and the
    /// extension manager all said «Greet the world».
    ///
    /// Only a key [`crate::availability::plugin_of_command`] recognises
    /// is answered from here, and that guard is STRUCTURAL rather than a
    /// promise about the caller: this map arrives through a public builder, and
    /// a built-in command's label must not be overridable by data that came off
    /// the wire even in principle. `App::freeze_help_plugins` cannot produce
    /// such a key — it puts the prefix on itself — so the guard costs one
    /// comparison and closes the shape of the mistake rather than the instance.
    /// Sharing the predicate with the availability arm also means "what counts
    /// as a plugin key" has one definition in this frontend, not two.
    ///
    /// A key the snapshot does not name still returns blank, so the fallback to
    /// the id survives untouched: a row wearing its dispatch key is poor, and a
    /// row wearing nothing at all is worse.
    ///
    /// Blank on a miss is the contract, not an accident: `norte_i18n::t_in`
    /// answers a missing message with the id itself, so returning it
    /// unconditionally would paint `help-cmd-…` at the reader and stop
    /// `norte_help::render_command`'s fallback chain from ever naming the
    /// command.
    ///
    /// The miss is detected by testing for that echo rather than for a proxy
    /// (a set of known ids, say): the echo IS the failure mode, so this
    /// cannot drift out of agreement with it. A message id can never be its
    /// own translation, so the comparison has no false positive.
    fn label(&self, command: &str) -> String {
        if crate::availability::plugin_of_command(command).is_some()
            && let Some(title) = self.plugin_labels.get(command)
        {
            return title.clone();
        }
        let id = label_id(command);
        let text = norte_i18n::t_in(self.lang, &id);
        if text != id {
            return text;
        }
        if command.starts_with(PLUGIN_KEY_PREFIX) {
            // A `plugin:` key the snapshot does not name. Blank would be the
            // generic answer, and `norte_help::label_or_id` would then fall
            // back to the command id — the behaviour we want, except that a
            // `plugin:` id is THIRD-PARTY TEXT and that fallback paints it
            // RAW. Handing back the masked, capped spelling keeps the visible
            // behaviour (the row wears its key) and takes the hazard out of it.
            //
            // Today only `norte_help::parse_untrusted` builds these topics and
            // its `is_own_command` already refuses a key carrying a control or
            // a bidi override — but `Topic` is a plain struct with public
            // fields and `HelpState::install_plugin_topic` does not re-check,
            // so the renderer would be trusting a filter three crates away to
            // stay in place. It costs one comparison not to.
            //
            // The PERMISSIVE prefix on purpose, unlike the strict
            // `plugin_of_command` above: that one asks "may this be answered
            // from the snapshot", an identity question, while this asks "is
            // this third-party text", and anything merely CLAIMING to be a
            // plugin key must be handled as though it were.
            return plugin_label(command);
        }
        String::new()
    }

    /// Whether the command can run in the context the overlay was opened in,
    /// answered by the ONE shared table
    /// ([[`crate::availability::verdict`]]) so a dimmed help row and a
    /// greyed-out GUI menu entry can never disagree.
    ///
    /// One reason of the vocabulary is deliberately NOT computed here, and the
    /// absence is the honest answer rather than a gap:
    /// [`norte_help::Reason::PolicyDenied`] is unreachable. The embedded TUI's
    /// actor is `journal::Actor::User`, which `ScopedPolicy::evaluate` allows
    /// unconditionally, and the embedded engine is handed `AllowAll` anyway.
    /// Policy denial is meaningful for an AGENT going through the daemon; in
    /// this app the human is the one who APPROVES a denial, never its subject.
    /// Computing it would dim a row for a rule that does not apply to the
    /// reader.
    ///
    /// [`norte_help::Reason::PluginInactive`] DOES have a surface since H3e: a
    /// plugin's own page lists that plugin's `plugin:{id}:{command}` rows, and
    /// a switched-off plugin must show them dimmed rather than promise a
    /// dispatch `plugin.run_command` would refuse. Which is why this routes
    /// through `verdict_with_plugins` and never through plain `verdict` — the
    /// latter answers a `plugin:` key through its fail-OPEN wildcard, lighting
    /// every such row unconditionally.
    ///
    /// (The palette makes the OPPOSITE call and both are right: it filters an
    /// inactive plugin's commands out entirely, because a list of what you can
    /// run has no business showing what you cannot, while a page ABOUT one
    /// plugin has every business saying that this is its command and it is
    /// switched off.)
    fn availability(&self, command: &str) -> Availability {
        crate::availability::verdict_with_plugins(command, &self.facts, &self.active_plugins)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Screen, parse_keymap};
    use norte_i18n::Lang;

    fn resolver() -> Chords {
        let src = r#"
[pane]
keymap = [{ on = ["f5"], run = "pane.copy" }]
"#;
        let preset = parse_keymap(src).expect("fixture parses");
        let known = ["pane.copy"];
        let eff = |s| Effective::build_for(&preset, &[], &known, s).expect("fixture builds");
        Chords::new(
            &eff(Screen::Browse),
            &eff(Screen::Viewer),
            &eff(Screen::Dialog),
            Lang::En,
        )
    }

    /// A miss answers BLANK and never the lookup key. `t_in` echoes the id of
    /// a message it does not have, so returning it would paint `help-cmd-…` at
    /// the reader and stop `norte_help`'s own fallback chain from ever naming
    /// the command.
    #[test]
    fn una_clave_sin_entrada_en_el_catalogo_contesta_en_blanco() {
        assert_eq!(resolver().label("no.such.command"), "");
    }

    /// A `plugin:` key the snapshot does not name still wears its id — but
    /// masked, because that id is THIRD-PARTY text on its way to a row.
    #[test]
    fn una_clave_de_plugin_hostil_no_se_pinta_cruda() {
        let label = resolver().label("plugin:demo\u{202e}evil:run");
        assert!(
            !label.contains('\u{202e}'),
            "una marca de dirección llegó cruda: {label:?}"
        );
        assert!(label.starts_with("plugin:demo"), "{label:?}");
    }

    /// Before anything is frozen the resolver dims NOTHING: the facts table is
    /// a list of known impediments, and "I have not looked" is not one.
    #[test]
    fn sin_hechos_congelados_no_se_atenua_nada() {
        assert_eq!(
            resolver().availability("pane.copy"),
            Availability::Available
        );
    }

    /// A plugin command with no snapshot is NOT offered: a plugin set is an
    /// allowlist, so "I have not looked" means "I cannot vouch for any of
    /// them" — the opposite default from the facts table above, on purpose.
    #[test]
    fn sin_foto_de_plugins_ninguna_fila_de_plugin_se_ofrece() {
        assert!(
            !resolver()
                .availability("plugin:org.norte.demo:greet")
                .is_available()
        );
    }
}
