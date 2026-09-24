//! `keymap.toml` as data: the raw binding lists, the layer file, and the
//! per-context merge order (ADR 0006/0007).

use serde::Deserialize;

use super::{KeymapError, presets};

/// One binding as represented in TOML.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub(super) struct RawBinding {
    pub(super) on: Vec<String>,
    pub(super) run: String,
}

/// The three binding lists in a section: `keymap` for presets and
/// `prepend_keymap`/`append_keymap` for user layers.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub(super) struct RawSection {
    #[serde(default)]
    pub(super) keymap: Vec<RawBinding>,
    #[serde(default)]
    pub(super) prepend_keymap: Vec<RawBinding>,
    #[serde(default)]
    pub(super) append_keymap: Vec<RawBinding>,
}

impl RawSection {
    /// The name of the first of the three lists that declares anything, or
    /// `None` if the section is empty. Used by the `dialog_from` rule:
    /// "declares a `[dialog]` of its own" has to mean ANY list, not just
    /// `keymap`, or the same question could be answered twice through
    /// `append_keymap` — and the diagnostic has to name the list it found, or
    /// it sends the reader looking for a `keymap` that is not there.
    fn declared_list(&self) -> Option<&'static str> {
        if !self.keymap.is_empty() {
            Some("keymap")
        } else if !self.prepend_keymap.is_empty() {
            Some("prepend_keymap")
        } else if !self.append_keymap.is_empty() {
            Some("append_keymap")
        } else {
            None
        }
    }

    /// Whether the section declares no binding at all, in any of its three
    /// lists (see [`RawSection::declared_list`]). Only the tests ask it this
    /// way round — production wants the NAME of the offending list, for the
    /// diagnostic — so it is gated rather than left as dead code.
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.declared_list().is_none()
    }
}

/// A parsed `keymap.toml` preset or user layer.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct KeymapFile {
    /// Whether a numeric prefix multiplies the next command (`5j`). Opt-in per
    /// PRESET, and `vim` is the only bundled preset that takes the option up:
    /// vi's counts are the reason vi is imitated at all. Every other original
    /// spends its digits elsewhere and would have them stolen — `orthodox`,
    /// `cua`, Total Commander, Krusader and Norton plainly, and Far too: Far
    /// has no numeric prefix, it binds `Ctrl+1`..`Ctrl+0` to panel view modes.
    // K2b rule 3. The K2a version of this doc said "vim and far set it because
    // their originals have counts", which was wrong about Far — kept as a `//`
    // note because the `///` text above is published verbatim in
    // `docs/schema/keymap.schema.json`, where our own history is noise.
    #[serde(default)]
    pub(super) counts: bool,
    #[serde(default)]
    pub(super) global: RawSection,
    #[serde(default)]
    pub(super) pane: RawSection,
    #[serde(default)]
    pub(super) viewer: RawSection,
    /// `dialog` context (H1, issue #24): modal/overlay keys (confirmation,
    /// approval, navigation popups…) as data keymap instead of ad hoc
    /// handlers — the generated help can never fall out of sync with a
    /// rebind. Merges with `global` the same as `pane`/`viewer` (see
    /// [`Screen::Dialog`]).
    #[serde(default)]
    pub(super) dialog: RawSection,
    /// The preset whose `[dialog]` section this one adopts. norte's dialogs are
    /// norte's, not the imitated program's: a Total Commander user expects TC's
    /// panel keys, not a TC confirmation dialog that TC never had. Only legal in
    /// a PRESET (a user layer that set it would silently redefine every overlay
    /// key — the same reason `counts` is refused there), only one level deep, and
    /// only naming a bundled preset. Declaring it together with a `[dialog]`
    /// section of your own is an error rather than a precedence puzzle.
    //
    // Notes for us, deliberately NOT rustdoc: this text ships as the schema
    // `description` in `docs/schema/keymap.schema.json`, which is the only
    // documentation of `keymap.toml` a third party has (ADR 0045).
    //
    // `parse_keymap` resolves the key and then LEAVES IT SET, so the value is
    // afterwards provenance rather than input — which is what lets
    // `check_layer_keys` still refuse it on a `KeymapFile` that did not come
    // through `parse_keymap_layer`.
    //
    // `skip_serializing_if` is inert for serde here (`KeymapFile` derives only
    // `Deserialize`); it is load-bearing for schemars, where it is what keeps
    // `"default": null` out of the published property.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) dialog_from: Option<String>,
    /// `true` if this layer is the PROJECT one (`./.norte`) — potentially
    /// FOREIGN content (comes with a cloned repo) loaded WITHOUT trust. A
    /// project keymap CANNOT bind `lua:`:
    /// [`Effective::build_for`](super::Effective::build_for) discards those
    /// bindings (counted in
    /// [`Effective::discarded_lua_bindings`](super::Effective::discarded_lua_bindings))
    /// — rebinding a common key to a command from the USER's `init.lua`
    /// (with no sandbox) would be repo-directed execution with no
    /// confirmation at all. It does not come from the TOML (`serde(skip)`):
    /// `load_keymap_layer` (`config.rs`) marks it by reading the
    /// [`Layer`](norte_config::Layer) of the `dir` each layer carries (ADR
    /// 0035: the kind travels PER DIR in `Layers`, no longer inferred by
    /// position — debt #75 closed).
    #[serde(skip)]
    project: bool,
}

impl KeymapFile {
    /// The section of `screen`'s SPECIFIC context, mutably — the same choice
    /// [`Screen::specific`] makes for reading, so a writer cannot put a
    /// binding in a context the reader will not look in. Used to build the
    /// PROSPECTIVE layer of
    /// [`rebind_dry_run`](super::rebind_dry_run); nothing else mutates a
    /// parsed keymap.
    pub(super) fn section_mut(&mut self, screen: Screen) -> &mut RawSection {
        match screen {
            Screen::Browse => &mut self.pane,
            Screen::Viewer => &mut self.viewer,
            Screen::Dialog => &mut self.dialog,
        }
    }

    /// Does it define `keymap` (a preset's full list)? User LAYERS do not
    /// accept it — the diagnostic with the file lives in `config::load`.
    #[must_use]
    pub fn has_full_keymap(&self) -> bool {
        !self.global.keymap.is_empty()
            || !self.pane.keymap.is_empty()
            || !self.viewer.keymap.is_empty()
            || !self.dialog.keymap.is_empty()
    }

    /// Marks this layer as the PROJECT one (see the `project` field): its
    /// `lua:` bindings are discarded on merge. Called by `config::load`
    /// with `./.norte`'s `keymap.toml`.
    pub fn mark_project(&mut self) {
        self.project = true;
    }

    /// Is it the project layer? (see [`Self::mark_project`]).
    #[must_use]
    pub fn is_project(&self) -> bool {
        self.project
    }
}

/// Active screen: decides which specific context merges with `global`
/// (ADR 0006; the stack grows with the UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// The two panes (`pane` context).
    Browse,
    /// The viewer (`viewer` context, phase 7).
    Viewer,
    /// Modals/overlays (`dialog` context, H1 — issue #24): confirmation,
    /// approval, navigation popups… each overlay declares its own
    /// ALLOWLIST of which `dialog.*` commands it supports (the security
    /// semantics live in code, not here).
    Dialog,
}

impl Screen {
    /// The `keymap.toml` section that holds this screen's SPECIFIC context —
    /// the section a writer must put a binding in for this screen to see it,
    /// and the inverse of the mapping `Self::specific` (private) reads.
    ///
    /// It never answers `"global"`, and for a writer that is the point. The
    /// specific context is merged WHOLE before `global` (`merged_bindings`),
    /// so a binding here beats a preset binding in `global` just as it beats
    /// one in the same context — it is never the weaker choice — and it cannot
    /// touch the other two screens. A shortcut editor that wrote `[global]`
    /// would change the viewer and every dialog from a row that named one
    /// screen.
    ///
    /// ```
    /// use norte_frontend::keymap::Screen;
    ///
    /// assert_eq!(Screen::Browse.section(), "pane");
    /// assert_eq!(Screen::Viewer.section(), "viewer");
    /// assert_eq!(Screen::Dialog.section(), "dialog");
    /// ```
    #[must_use]
    pub fn section(self) -> &'static str {
        match self {
            Self::Browse => "pane",
            Self::Viewer => "viewer",
            Self::Dialog => "dialog",
        }
    }

    /// How to READ this screen's specific context out of a [`KeymapFile`].
    /// One function pointer instead of the same three-arm match written once
    /// per reader: [`merged_bindings`],
    /// [`preset_commands`](super::preset_commands) and [`Self::section`] all
    /// describe the same mapping, and three copies of it is how the writer and
    /// the reader would eventually disagree about where a `[viewer]` binding
    /// lives.
    pub(super) fn specific(self) -> fn(&KeymapFile) -> &RawSection {
        match self {
            Self::Browse => |f| &f.pane,
            Self::Viewer => |f| &f.viewer,
            Self::Dialog => |f| &f.dialog,
        }
    }
}

/// Compact diagnostic for a TOML parse error: `"line N: msg"` if the error
/// carries a span, or just the message if not (semantic errors). LOCAL copy
/// of `norte_tui::config::toml_diag` — the engine does not depend on the
/// TUI.
fn toml_diag(raw: &str, e: &toml::de::Error) -> String {
    match e.span() {
        Some(s) => {
            let line = 1 + raw[..s.start.min(raw.len())].matches('\n').count();
            format!("line {line}: {}", e.message())
        }
        None => e.message().to_owned(),
    }
}

/// The bare TOML parse, WITHOUT resolving `dialog_from`. It exists so that
/// [`parse_keymap`] can read the inherited preset without calling itself:
/// inheritance is one level, and with no recursion there is no depth to
/// bound — not even a `dialog_from` that names its own file can loop.
/// Swapping this call for [`parse_keymap`] is the change the
/// `a_dialog_from_that_points_at_itself_does_not_recurse` test exists to
/// catch.
fn parse_raw(s: &str) -> Result<KeymapFile, KeymapError> {
    toml::from_str(s).map_err(|e| KeymapError::Toml(toml_diag(s, &e)))
}

/// Parses a PRESET's `keymap.toml` and resolves its `dialog_from` (see
/// `KeymapFile::dialog_from`): the named preset's `[dialog]` section is
/// copied in, so that every later reader sees an ordinary [`KeymapFile`] and
/// need not know inheritance exists. User and project layers go through
/// [`parse_keymap_layer`] instead, which refuses the key.
///
/// ```
/// use norte_frontend::keymap::{Effective, Screen, parse_keymap};
///
/// // A preset that says nothing about overlays still gets norte's.
/// let tc = parse_keymap("dialog_from = \"orthodox\"\n\n[pane]\nkeymap = []\n").unwrap();
/// let eff = Effective::build_for(&tc, &[], &["dialog.approve"], Screen::Dialog).unwrap();
/// assert!(eff.bindings().iter().any(|(_, run)| *run == "dialog.approve"));
///
/// // Naming something that is not a bundled preset is a load error, not silence.
/// assert!(parse_keymap("dialog_from = \"totalcommander\"\n").is_err());
/// ```
///
/// # Errors
/// - [`KeymapError::Toml`] if it does not parse or has unknown keys.
/// - [`KeymapError::DialogFromAndDialog`] if it declares both.
/// - [`KeymapError::UnknownDialogFrom`] if it names a preset that does not exist.
/// - [`KeymapError::DialogFromChain`] if the named preset inherits in turn.
pub fn parse_keymap(s: &str) -> Result<KeymapFile, KeymapError> {
    let mut file = parse_raw(s)?;
    resolve_dialog_from(&mut file, presets::source)?;
    Ok(file)
}

/// Parses a `keymap.toml` that is a user or project LAYER: identical to
/// [`parse_keymap`] except that `dialog_from` is REFUSED here instead of
/// resolved. A layer is never a preset, so nothing is ever copied into one —
/// which is a stronger statement than "the copy turns out to be unreachable",
/// and it is what keeps the diagnostic honest: the load error names
/// `dialog_from`, the key the user actually wrote, instead of the `keymap`
/// list the resolver would otherwise have put there
/// (`config::load_keymap_layer` checks [`KeymapFile::has_full_keymap`] right
/// after parsing, and that reads `dialog.keymap`).
///
/// `check_layer_keys` carries the same refusal, and keeps carrying it:
/// this function is the door every real layer comes through, but a caller
/// that builds a [`KeymapFile`] some other way and passes it as a layer must
/// still be told no.
///
/// # Errors
/// - [`KeymapError::Toml`] if it does not parse or has unknown keys.
/// - [`KeymapError::WrongLayerKey`] if it declares `dialog_from`.
pub fn parse_keymap_layer(s: &str) -> Result<KeymapFile, KeymapError> {
    let file = parse_raw(s)?;
    if file.dialog_from.is_some() {
        return Err(KeymapError::WrongLayerKey {
            layer: "usuario",
            key: "dialog_from",
        });
    }
    Ok(file)
}

/// The resolution of [`KeymapFile::dialog_from`], with the preset catalogue
/// INJECTED. [`parse_keymap`] passes it [`presets::source`]; the tests pass
/// inline sources, which is the only way to exercise the "chain" and
/// "self-reference" cases while no bundled preset inherits yet.
fn resolve_dialog_from(
    file: &mut KeymapFile,
    lookup: impl Fn(&str) -> Option<&'static str>,
) -> Result<(), KeymapError> {
    let Some(name) = file.dialog_from.clone() else {
        return Ok(());
    };
    if let Some(list) = file.dialog.declared_list() {
        return Err(KeymapError::DialogFromAndDialog { name, list });
    }
    let src = lookup(&name).ok_or_else(|| KeymapError::UnknownDialogFrom {
        name: name.clone(),
        // Built from `NAMES` so the list cannot drift when K2b's four land.
        known: presets::NAMES.join(", "),
    })?;
    // `parse_raw`, NOT `parse_keymap`: one level and no more. Chaining would
    // make the effective `[dialog]` depend on a hop nobody sees when reading
    // the file, which is exactly the opacity ADR 0045 rejects. It is also
    // what makes the loop impossible rather than merely bounded — a
    // `dialog_from` naming its own file stops at the chain check below
    // instead of recursing.
    let parent = parse_raw(src)?;
    if let Some(then) = parent.dialog_from {
        return Err(KeymapError::DialogFromChain { name, then });
    }
    file.dialog = parent.dialog;
    Ok(())
}

/// Where a merged binding comes from: the shared preset or a user/project
/// layer. Since K1 the per-binding verdict no longer depends on it (a name
/// absent from `known_commands` is looked up in the shared catalogue, the same
/// way for both) — it survives because `merge_ctx` needs it to discard the
/// `lua:` bindings of a project layer, and because a future rule that DOES
/// depend on provenance would otherwise have to re-derive it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Origin {
    Preset,
    Layer,
}

/// Which section of a `keymap.toml` a merged binding was read from: the
/// screen's own context, or `[global]` — merged into EVERY screen
/// (`merged_bindings`), so a screen alone cannot tell the two apart once the
/// merge is done. This is the provenance a shortcut editor needs to mark a
/// row non-editable (K3c #141): [`Screen::section`] never answers `"global"`,
/// so a row whose binding came from here cannot be unbound or rebound through
/// a section that names one screen — a write there would change all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Section {
    /// The screen's own context (`[pane]`, `[viewer]`, `[dialog]`).
    Specific,
    /// `[global]`.
    Global,
}

/// Merges a context (ADR 0006/0007): higher-layer prepends first (they
/// win), then the preset, then the appends (higher ones first). A PROJECT
/// layer's `lua:` bindings are DISCARDED here, counted in `discarded_lua`
/// (security: see [`KeymapFile::mark_project`] — a foreign repo's keymap
/// cannot direct the execution of Lua commands). Each binding is tagged
/// with its [`Origin`] (preset vs. layer) and its [`Section`] (screen-
/// specific vs. `[global]`) — the CALLER fixes `section` once per call,
/// because `get` already decides which list is being read.
fn merge_ctx<'a>(
    preset: &'a KeymapFile,
    layers: &'a [KeymapFile],
    get: fn(&KeymapFile) -> &RawSection,
    section: Section,
    discarded_lua: &mut usize,
) -> Vec<(&'a RawBinding, Origin, Section)> {
    let mut out = Vec::new();
    let mut push = |layer_project: bool,
                    b: &'a RawBinding,
                    out: &mut Vec<(&'a RawBinding, Origin, Section)>| {
        if layer_project && b.run.starts_with("lua:") {
            *discarded_lua += 1;
        } else {
            out.push((b, Origin::Layer, section));
        }
    };
    for l in layers.iter().rev() {
        for b in &get(l).prepend_keymap {
            push(l.project, b, &mut out);
        }
    }
    out.extend(
        get(preset)
            .keymap
            .iter()
            .map(|b| (b, Origin::Preset, section)),
    );
    for l in layers.iter().rev() {
        for b in &get(l).append_keymap {
            push(l.project, b, &mut out);
        }
    }
    out
}

/// Each layer admits ONLY its own lists (phase-4 review): a preset defines
/// `keymap`; a user/project layer defines `prepend_keymap`/`append_keymap`.
/// Silently dropping the wrong list would be the "weird behavior" the ADR
/// forbids. Returns the first offending layer/key (there is at most one kind
/// of mistake worth reporting per source). Shared by
/// [`Effective::build_for`](super::Effective::build_for) (fails on it) and
/// [`Effective::build_diagnostics`](super::Effective::build_diagnostics)
/// (reports it and keeps walking — the bindings still merge from the CORRECT
/// lists via `merge_ctx`).
pub(super) fn check_layer_keys(
    preset: &KeymapFile,
    layers: &[KeymapFile],
) -> Result<(), KeymapError> {
    for section in [&preset.global, &preset.pane, &preset.viewer, &preset.dialog] {
        if !(section.prepend_keymap.is_empty() && section.append_keymap.is_empty()) {
            return Err(KeymapError::WrongLayerKey {
                layer: "preset",
                key: "prepend_keymap/append_keymap",
            });
        }
    }
    for layer in layers {
        // The count POLICY is the preset's. A layer that could turn counts on
        // would silently change what EVERY digit key means — the "weird
        // behaviour" ADR 0006 forbids, so it is a load error like any other
        // wrong key.
        if layer.counts {
            return Err(KeymapError::WrongLayerKey {
                layer: "usuario",
                key: "counts",
            });
        }
        // Same argument as `counts`, and one notch sharper: `dialog_from` in a
        // layer would replace the WHOLE overlay context — every confirmation,
        // approval and overwrite key at once — from one line that names no
        // key.
        //
        // The layer that came through `parse_keymap_layer` (every real one,
        // via `config::load_keymap_layer`) has already been refused there, so
        // this is the second lock: a caller that built its `KeymapFile` with
        // `parse_keymap` — a test, or a frontend embedding a layer literal —
        // is still told no, and told it here rather than through the copied
        // `keymap` list. Both locks are cheap and neither subsumes the other.
        if layer.dialog_from.is_some() {
            return Err(KeymapError::WrongLayerKey {
                layer: "usuario",
                key: "dialog_from",
            });
        }
        for section in [&layer.global, &layer.pane, &layer.viewer, &layer.dialog] {
            if !section.keymap.is_empty() {
                return Err(KeymapError::WrongLayerKey {
                    layer: "usuario",
                    key: "keymap",
                });
            }
        }
    }
    Ok(())
}

/// The merged, ordered binding list for `screen` (screen-specific context
/// before `global`, ADR 0006), with project-layer `lua:` bindings discarded
/// and counted. Shared by
/// [`Effective::build_for`](super::Effective::build_for) and
/// [`Effective::build_diagnostics`](super::Effective::build_diagnostics) so the
/// merge order is defined once.
pub(super) fn merged_bindings<'a>(
    preset: &'a KeymapFile,
    layers: &'a [KeymapFile],
    screen: Screen,
    discarded_lua_bindings: &mut usize,
) -> Vec<(&'a RawBinding, Origin, Section)> {
    merge_ctx(
        preset,
        layers,
        screen.specific(),
        Section::Specific,
        discarded_lua_bindings,
    )
    .into_iter()
    .chain(merge_ctx(
        preset,
        layers,
        |f| &f.global,
        Section::Global,
        discarded_lua_bindings,
    ))
    .collect()
}

#[cfg(test)]
mod dialog_from_tests {
    use super::{
        KeymapError, KeymapFile, check_layer_keys, parse_keymap, parse_keymap_layer,
        resolve_dialog_from,
    };

    /// An inline imported preset: what tasks 2 and 3 will bring, without
    /// the file. Nothing here depends on `total-commander.toml` existing.
    const IMPORTED: &str = r#"
dialog_from = "orthodox"

[pane]
keymap = [{ on = ["f5"], run = "pane.copy" }]
"#;

    /// A [`super::RawSection`]'s `[dialog]` as comparable pairs: a LENGTH
    /// comparison would accept a partial copy, or the wrong preset's
    /// section if it measured the same.
    fn pairs(s: &super::RawSection) -> Vec<(Vec<String>, String)> {
        s.keymap
            .iter()
            .map(|b| (b.on.clone(), b.run.clone()))
            .collect()
    }

    /// What the key buys: the imported preset does NOT copy `[dialog]`'s 25
    /// lines and still comes out of parsing with the context set — the
    /// same one, binding by binding.
    #[test]
    fn inheriting_populates_the_dialog_context() {
        let kf = parse_keymap(IMPORTED).expect("the imported preset parses");
        let orthodox = parse_keymap(super::presets::ORTHODOX).expect("orthodox parses");
        assert!(!kf.dialog.keymap.is_empty(), "[dialog] came out empty");
        assert_eq!(
            pairs(&kf.dialog),
            pairs(&orthodox.dialog),
            "the inherited [dialog] is not orthodox's"
        );
        let inherited: Vec<&str> = kf.dialog.keymap.iter().map(|b| b.run.as_str()).collect();
        assert!(inherited.contains(&"dialog.approve"), "{inherited:?}");
        // The field STAYS set after resolving: it is what `check_layer_keys`
        // looks at to deny it to a layer.
        assert_eq!(kf.dialog_from.as_deref(), Some("orthodox"));
        // And its own is not touched.
        assert_eq!(kf.pane.keymap.len(), 1);
    }

    /// Two answers to the same question. Counts ANY of the three lists, not
    /// just `keymap`.
    #[test]
    fn inheriting_and_declaring_dialog_at_once_is_an_error() {
        for list in ["keymap", "prepend_keymap", "append_keymap"] {
            let src = format!(
                "dialog_from = \"orthodox\"\n\n[dialog]\n{list} = [{{ on = [\"y\"], run = \"dialog.deny\" }}]\n"
            );
            let e = parse_keymap(&src)
                .err()
                .unwrap_or_else(|| panic!("{list}: was accepted"));
            assert!(
                matches!(
                    &e,
                    KeymapError::DialogFromAndDialog { name, list: found }
                        if name == "orthodox" && *found == list
                ),
                "{list}: {e:?}"
            );
            // The message names the list it found, not `keymap` by default.
            assert!(e.to_string().contains(list), "{list}: {e}");
        }
    }

    /// A name that does not exist is a typo, and the message says so with
    /// the key and the value.
    #[test]
    fn inheriting_from_a_nonexistent_preset_is_an_error() {
        let e = parse_keymap("dialog_from = \"totalcommander\"\n")
            .expect_err("a nonexistent preset was accepted");
        assert!(
            matches!(&e, KeymapError::UnknownDialogFrom { name, .. } if name == "totalcommander"),
            "{e:?}"
        );
        let msg = e.to_string();
        assert!(msg.contains("dialog_from"), "{msg}");
        assert!(msg.contains("totalcommander"), "{msg}");
        // And it says which ones ARE valid, taken from `NAMES` so it does
        // not fall behind once K2b registers the four imported ones.
        for name in super::presets::NAMES {
            assert!(msg.contains(name), "{msg} does not offer {name}");
        }
    }

    /// One level only: if the named preset inherits in turn, it stops. The
    /// catalogue is injected because no bundled preset inherits yet — and
    /// the day one does (task 2), this rule is already in place.
    #[test]
    fn a_chain_of_inheritance_is_an_error() {
        let mut kf: KeymapFile =
            toml::from_str("dialog_from = \"intermedio\"\n").expect("the child parses");
        let e = resolve_dialog_from(&mut kf, |n| {
            (n == "intermedio").then_some("dialog_from = \"orthodox\"\n")
        })
        .expect_err("a chain was accepted");
        assert!(
            matches!(
                &e,
                KeymapError::DialogFromChain { name, then } if name == "intermedio" && then == "orthodox"
            ),
            "{e:?}"
        );
        assert!(kf.dialog.is_empty(), "a broken chain cannot leave a trace");
    }

    /// `resolve_dialog_from`'s `parse_raw` is LOAD-BEARING, and this is the
    /// only thing that proves it: with `parse_keymap` in its place — the
    /// obvious "simplification", especially the day someone wants two
    /// levels — a preset naming itself would recurse until it overflowed
    /// the stack BEFORE reaching the chain check, and overflowing the stack
    /// is an abort, not a load error. The chain test does NOT cover this:
    /// with recursion it would still pass green.
    #[test]
    fn a_dialog_from_that_points_at_itself_does_not_recurse() {
        let mut kf: KeymapFile =
            toml::from_str("dialog_from = \"bucle\"\n").expect("the file parses");
        let e = resolve_dialog_from(&mut kf, |n| {
            (n == "bucle").then_some("dialog_from = \"bucle\"\n")
        })
        .expect_err("a self-loan has to stop");
        assert!(
            matches!(
                &e,
                KeymapError::DialogFromChain { name, then } if name == "bucle" && then == "bucle"
            ),
            "{e:?}"
        );
    }

    /// The door every real layer comes through (`config::load_keymap_layer`)
    /// REJECTS it before resolving anything: a layer is never a preset, so
    /// it is never copied a `[dialog]` that would then have to be declared
    /// unreachable.
    #[test]
    fn parse_keymap_layer_rejects_dialog_from_without_resolving_it() {
        let e = parse_keymap_layer("dialog_from = \"orthodox\"\n")
            .expect_err("a layer cannot inherit [dialog]");
        assert!(
            matches!(
                e,
                KeymapError::WrongLayerKey {
                    layer: "usuario",
                    key: "dialog_from"
                }
            ),
            "{e:?}"
        );
        // And the normal case still goes through the same door.
        let layer = parse_keymap_layer(
            "[pane]\nprepend_keymap = [{ on = [\"j\"], run = \"cursor.down\" }]\n",
        )
        .expect("a normal layer parses");
        assert!(layer.dialog.is_empty());
    }

    /// Second lock: a `KeymapFile` built with `parse_keymap` and passed as a
    /// layer — a test, or a frontend with a literal layer — is also
    /// rejected, and by the name of the key that was written.
    #[test]
    fn dialog_from_in_a_layer_is_wrong_layer_key() {
        let preset = parse_keymap("[pane]\nkeymap = [{ on = [\"f5\"], run = \"pane.copy\" }]\n")
            .expect("preset");
        let layer = parse_keymap("dialog_from = \"orthodox\"\n").expect("the layer parses");
        let e = check_layer_keys(&preset, std::slice::from_ref(&layer))
            .expect_err("dialog_from in a layer was accepted");
        assert!(
            matches!(
                e,
                KeymapError::WrongLayerKey {
                    layer: "usuario",
                    key: "dialog_from"
                }
            ),
            "{e:?}"
        );
    }

    /// And the PROJECT layer — foreign content — does not smuggle the
    /// inherited bindings through the back door either: `merge_ctx` only
    /// reads a layer's `prepend_keymap`/`append_keymap`, so the copy stays
    /// structurally unreachable even though the diagnostic keeps walking.
    #[test]
    fn a_project_layer_does_not_smuggle_the_inherited_dialog() {
        let preset = parse_keymap("[pane]\nkeymap = [{ on = [\"f5\"], run = \"pane.copy\" }]\n")
            .expect("preset");
        let mut layer = parse_keymap("dialog_from = \"orthodox\"\n").expect("the layer parses");
        layer.mark_project();
        assert!(!layer.dialog.keymap.is_empty(), "the copy did happen");
        let mut discarded = 0;
        let merged = super::merged_bindings(
            &preset,
            std::slice::from_ref(&layer),
            super::Screen::Dialog,
            &mut discarded,
        );
        assert!(
            merged.is_empty(),
            "the layer contributed dialog bindings: {merged:?}"
        );
    }

    /// `orthodox` does not use the key and does not change because of this.
    #[test]
    fn orthodox_inherits_from_nobody() {
        let kf = parse_keymap(super::presets::ORTHODOX).expect("orthodox parses");
        assert!(kf.dialog_from.is_none());
        assert!(!kf.dialog.keymap.is_empty());
        check_layer_keys(&kf, &[]).expect("orthodox is still a valid preset");
    }
}
