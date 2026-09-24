//! Frontend configuration: the shared scalar merge (`norte-config`) plus the
//! frontend-only passes — `keymap.toml` layers and `openers.toml` (#28).

use std::path::{Path, PathBuf};

use norte_config::schema::read_optional;
use norte_config::{CommonConfig, ConfigError, Layer, Layers, QuickSearch};

use crate::keymap::{KeymapFile, parse_keymap_layer};
use crate::nav;
use crate::openers::OpenersConfig;

/// Everything a frontend needs, flat (same shape the TUI historically used).
#[derive(Debug, Clone)]
pub struct FrontendConfig {
    /// The merged scalars (preset, ui, daemon, hotlist, archive, ai, sources).
    /// `common.sources` is NOT grouped by layer: `norte-config::load` fills
    /// it with every layer's `norte.toml` first, and this module's `load`
    /// then appends each layer's `keymap.toml`/`openers.toml` afterwards —
    /// treat it as a set of files that participated, not an ordered log.
    pub common: CommonConfig,
    /// `keymap.toml` layers present, ascending precedence.
    ///
    /// One entry per layer dir that HAS the file — a dir without one
    /// contributes nothing, so the position of a layer here does NOT say
    /// which layer it is. That is what [`Self::keymap_layer_kinds`] is for;
    /// see it before cutting this list at any index.
    pub keymap_layers: Vec<KeymapFile>,
    /// The [`Layer`] each entry of [`Self::keymap_layers`] came from — same
    /// length, same order, index by index.
    ///
    /// Carried rather than inferred because the two are not recoverable from
    /// each other and a wrong guess is silent: with only the system dir
    /// holding a `keymap.toml`, `keymap_layers` is a one-element list whose
    /// entry is the SYSTEM layer, and with a project layer present the last
    /// entry is `./.norte`. A shortcut editor that cut this list positionally
    /// would model its write into the wrong layer and approve a binding that
    /// never fires — see
    /// [`RebindSources::split_at`](crate::keymap::RebindSources::split_at),
    /// which is the only supported way to make that cut.
    pub keymap_layer_kinds: Vec<Layer>,
    /// The DIRECTORY each entry of [`Self::keymap_layers`] came from — same
    /// length, same order, index by index as [`Self::keymap_layer_kinds`].
    ///
    /// The three vectors are ONE table. This one exists because a shortcut
    /// write's destination and the directory it lands in have to come from
    /// the same place: `split_at` says which layer it targets and this says
    /// where it lives, so the writer no longer resolves a directory on its
    /// own. With an active profile that used to send the write to the
    /// user's file, where the profile shadowed it (#305).
    pub keymap_layer_dirs: Vec<std::path::PathBuf>,
    /// Quick-search mode mapped onto the navigation enum.
    pub quick_search_mode: nav::Mode,
    /// Merged declarative openers (#28): System/User only, fail-closed.
    pub openers: OpenersConfig,
    /// The USER's themes, `<config>/themes/*.toml`, already parsed
    /// ([`crate::theme::load_user_themes`]).
    ///
    /// From the user layer only. A theme launches nothing, but the list is
    /// what the selectors offer, and a foreign `./.norte` has no business
    /// putting names in it. They travel with the config so a selector can
    /// list and preview them without reading disk inside a keystroke.
    pub user_themes: Vec<crate::theme::UserTheme>,
}

/// Loads the `keymap.toml` layer from `dir` (ADR 0006/0007); `None` if it
/// doesn't exist. The PROJECT layer is marked (`mark_project`) so
/// `Effective` discards its `lua:` bindings (security #75). A user layer
/// does not accept the full `keymap` list (that belongs to presets): using
/// it is an error naming the culprit file.
///
/// Kept `pub` for out-of-workspace frontends (e.g. the GUI): they can load
/// keymap layers without going through this module's combined `load`.
///
/// # Errors
/// [`ConfigError::Toml`] if it doesn't parse, or a layer uses `keymap` or
/// `dialog_from` (both are preset-only keys).
pub fn load_keymap_layer(
    dir: &Path,
    kind: Layer,
    sources: &mut Vec<PathBuf>,
) -> Result<Option<KeymapFile>, ConfigError> {
    let keymap = dir.join("keymap.toml");
    let Some(raw) = read_optional(&keymap)? else {
        return Ok(None);
    };
    // `parse_keymap_layer`, not `parse_keymap`: a layer may not inherit a
    // `[dialog]` (ADR 0045), and refusing the key BEFORE resolving it is what
    // makes the error name `dialog_from` instead of the `keymap` list the
    // resolution would have copied in — `has_full_keymap` below reads
    // `dialog.keymap` and would otherwise fire first, on a key the user never
    // wrote.
    let mut parsed = parse_keymap_layer(&raw).map_err(|e| ConfigError::Toml {
        path: keymap.clone(),
        message: e.to_string(),
    })?;
    if kind == Layer::Project {
        parsed.mark_project();
    }
    if parsed.has_full_keymap() {
        return Err(ConfigError::Toml {
            path: keymap,
            message:
                "a config layer does not accept `keymap`: use prepend_keymap/append_keymap (ADR 0006)"
                    .to_owned(),
        });
    }
    sources.push(keymap);
    Ok(Some(parsed))
}

/// Loads and parses `openers.toml` for a layer (#28); `None` if the file
/// doesn't exist or the layer is PROJECT (fail-closed — a hostile repo's
/// `./.norte/openers.toml` must not be able to launch external binaries).
///
/// Kept `pub` for out-of-workspace frontends (e.g. the GUI): they can load
/// openers without going through this module's combined `load`.
///
/// # Errors
/// [`ConfigError::Toml`] naming the culprit file if it doesn't parse.
pub fn load_openers(
    dir: &Path,
    kind: Layer,
    sources: &mut Vec<PathBuf>,
) -> Result<Option<OpenersConfig>, ConfigError> {
    if kind == Layer::Project {
        return Ok(None);
    }
    let openers_path = dir.join("openers.toml");
    let Some(raw) = read_optional(&openers_path)? else {
        return Ok(None);
    };
    let parsed = OpenersConfig::parse(&raw).map_err(|e| ConfigError::Toml {
        path: openers_path.clone(),
        message: e.to_string(),
    })?;
    sources.push(openers_path);
    Ok(Some(parsed))
}

/// Load and merge every layer (ADR 0007): common scalars + keymap + openers.
///
/// # Errors
/// [`ConfigError`] with the culprit file; an absent layer is not an error.
pub fn load(layers: &Layers) -> Result<FrontendConfig, ConfigError> {
    let mut common = norte_config::load(layers)?;
    let mut keymap_layers = Vec::new();
    let mut keymap_layer_kinds = Vec::new();
    let mut keymap_layer_dirs = Vec::new();
    let mut openers = OpenersConfig::empty();
    for (dir, kind) in &layers.dirs {
        if let Some(parsed) = load_keymap_layer(dir, *kind, &mut common.sources)? {
            keymap_layers.push(parsed);
            // In lockstep with the push above and never apart from it: the
            // two vectors are one table, and a layer whose kind was dropped
            // cannot be recovered by position (a dir with no `keymap.toml`
            // leaves no gap here). Three since #305: the directory too, for
            // the same reason.
            keymap_layer_kinds.push(*kind);
            keymap_layer_dirs.push(dir.clone());
        }
        if let Some(parsed) = load_openers(dir, *kind, &mut common.sources)? {
            openers.extend_front(parsed);
        }
    }
    let quick_search_mode = match common.quick_search {
        QuickSearch::Filter => nav::Mode::Filter,
        QuickSearch::Jump => nav::Mode::Jump,
    };
    let user_themes = layers
        .dirs
        .iter()
        .find(|(_, kind)| *kind == Layer::User)
        .map(|(dir, _)| crate::theme::load_user_themes(dir))
        .unwrap_or_default();
    Ok(FrontendConfig {
        common,
        keymap_layers,
        keymap_layer_kinds,
        keymap_layer_dirs,
        quick_search_mode,
        openers,
        user_themes,
    })
}

/// [`load`] with a profile in the mix, with D7's rule-of-three-answers
/// applied to the WHOLE LAYER and not just its `norte.toml`.
///
/// **This is the one a frontend calls**, and `norte-config`'s is the one the
/// core calls. The difference is not one of convenience: that one decides
/// about `norte.toml`, and a profile also brings `keymap.toml` and
/// `openers.toml`, which are fatal for any layer that is not project. Going
/// through the one below, a profile with a typo in a shortcut declared
/// itself healthy and blew up later — with `ProfileSource::Sticky` that is
/// exactly the outcome D7 exists to prevent, because the reader is left
/// outside the program with no way to choose another profile (#305).
///
/// The rule is not duplicated: [`norte_config::load_with`] has it, and this
/// passes it this crate's loader.
///
/// # Errors
///
/// [`norte_config::ProfileError`] depending on the name's provenance, same as
/// [`norte_config::load_with_profile`].
pub fn load_with_profile(
    layers_for: &impl Fn(Option<&std::ffi::OsStr>) -> Layers,
    name: Option<&std::ffi::OsStr>,
    source: norte_config::ProfileSource,
) -> Result<norte_config::Loaded<FrontendConfig>, norte_config::ProfileError> {
    norte_config::load_with(layers_for, name, source, &load)
}

/// What "save as profile" saves, assembled from what is SEEN (#306 in the
/// terminal, #318 in the window).
///
/// Lives here, and not a copy in each frontend, for ADR 0077's lesson: **a
/// decision duplicated between frontends silently drifts**. And this is the
/// worst place it could drift — two "save as" that produce different
/// profiles turn a profile into something that depends on where you saved
/// it from. With a single function, parity is not a test someone has to
/// remember to write: there is no second thing to compare it against.
///
/// `slot_dir` answers where each listing is; a slot that is not one (viewer,
/// processes, sites) answers `None` and does not enter `[profile.start]`,
/// which is correct: it has no directory to remember.
///
/// What is NOT saved, and why:
///
/// - `[ui]`'s scalars: what the reader changes on the fly — theme, preset —
///   is already persisted through its own path, and copying it here would
///   write the same thing twice with two possible truths;
/// - the favorites, the connections and the rest of the sections. A profile
///   is a WORKSPACE, not a copy of the entire configuration: duplicating
///   the hotlist into every profile would freeze it, and the user's keeps
///   showing through underneath.
///
/// `keymap.toml` IS copied, BYTE for BYTE and without rewriting it: it is
/// the reader's file, with their comments, and "save as" has to produce a
/// profile that behaves the same as the one you had.
#[must_use]
pub fn profile_snapshot(
    tree: &crate::layout::Node,
    slot_dir: &dyn Fn(crate::layout::SlotId) -> Option<norte_proto::VPath>,
    keymap: Option<Vec<u8>>,
) -> norte_config::ProfileSnapshot {
    let start = tree
        .slot_ids()
        .into_iter()
        .filter_map(|id| {
            let crate::layout::SlotId(n) = id;
            Some((n.to_string(), slot_dir(id)?.to_wire()))
        })
        .collect();
    norte_config::ProfileSnapshot {
        title: None,
        layout_toml: crate::layout::config::to_toml(tree).ok(),
        ui: Vec::new(),
        start,
        keymap,
    }
}

/// Which slots `[profile.start]` seeds when entering a profile.
///
/// **The SESSION wins.** `[profile.start]` says where a slot opens "the
/// first time": as soon as that slot has saved state, what rules is where
/// you left it, because a profile is a workspace and not a bookmark that
/// sends you back to the start every time you enter it.
///
/// Two vetoes, and both are needed:
///
/// - `known` are the slots the SAVED session knows something about. It has
///   to be what was read from disk, not the current screen: the latter
///   names every live slot, so asking the profile about it would never seed
///   anything.
/// - `seeded` are the ones this process already seeded. Without them, a
///   reader with no saved session — a fresh install — would go back to the
///   profile's startup directory every time they enter and leave it,
///   because as far as it is concerned the session never knows anything at
///   all.
///
/// Returned in the map's order — by slot id — so that seeding is
/// deterministic: two slots seeded in a different order end up with the
/// same content but with focus in different places.
///
/// Lives in this crate because both frontends answer the same question, and
/// that is exactly the kind of decision that drifts when written twice (ADR
/// 0077). Does no I/O: it decides, and the caller lists.
///
/// ```
/// use std::collections::{BTreeMap, BTreeSet};
/// use norte_proto::VPath;
/// use norte_frontend::config::profile_start_seeds;
///
/// let mut start = BTreeMap::new();
/// start.insert(1, VPath::parse("file:///src").unwrap());
/// start.insert(2, VPath::parse("file:///tmp").unwrap());
/// let none = BTreeSet::new();
///
/// // With no session and nothing seeded yet, both go.
/// assert_eq!(profile_start_seeds(&start, &none, &none).len(), 2);
///
/// // Slot 1 is known to the session: that one is theirs to rule.
/// let known = BTreeSet::from([1]);
/// let seeds = profile_start_seeds(&start, &known, &none);
/// assert_eq!(seeds.len(), 1);
/// assert_eq!(seeds[0].0, 2);
///
/// // And what is already seeded is not seeded again: entering and leaving
/// // the profile does not pull you away from where you were.
/// let seeded = BTreeSet::from([2]);
/// assert!(profile_start_seeds(&start, &known, &seeded).is_empty());
/// ```
#[must_use]
pub fn profile_start_seeds(
    start: &std::collections::BTreeMap<u32, norte_proto::VPath>,
    known: &std::collections::BTreeSet<u32>,
    seeded: &std::collections::BTreeSet<u32>,
) -> Vec<(u32, norte_proto::VPath)> {
    start
        .iter()
        .filter(|(id, _)| !known.contains(id) && !seeded.contains(id))
        .map(|(id, v)| (*id, v.clone()))
        .collect()
}

/// The slots `[profile.start]` names that this LAYOUT does not place.
///
/// They have nowhere to open, so they are dropped — and that has to be
/// said. It is the same kind of silence the whole key used to have before
/// ADR 0098: something is written in the profile's file and nothing
/// happens, with nothing explaining why. It happens by hand-editing or by
/// changing the profile's layout without re-saving it; `save_profile`
/// always writes ids its own layout places.
///
/// Returns the ids IN ORDER, so the message is the same on both surfaces.
///
/// ```
/// use std::collections::{BTreeMap, BTreeSet};
/// use norte_proto::VPath;
/// use norte_frontend::config::profile_start_huerfanos;
///
/// let mut start = BTreeMap::new();
/// start.insert(1, VPath::parse("file:///src").unwrap());
/// start.insert(9, VPath::parse("file:///tmp").unwrap());
/// let placed = BTreeSet::from([1, 2]);
/// assert_eq!(profile_start_huerfanos(&start, &placed), vec![9]);
/// ```
#[must_use]
pub fn profile_start_huerfanos(
    start: &std::collections::BTreeMap<u32, norte_proto::VPath>,
    placed: &std::collections::BTreeSet<u32>,
) -> Vec<u32> {
    start
        .keys()
        .filter(|id| !placed.contains(id))
        .copied()
        .collect()
}

/// Reads every profile from `<dir>/profiles/`, with its title and its reason
/// if it does not load.
///
/// **Blocks**: lists a directory and opens one file per profile. Whoever
/// calls it from an event loop goes through `spawn_blocking` (rule 2), and
/// #244 is why.
///
/// A profile that does not parse does NOT disappear: it comes back with its
/// `problem` set, so the selector shows it broken instead of hiding a
/// directory the reader created.
///
/// Lives in this module and not in [`crate::profile_picker`] because it
/// opens files, and that one is pure by contract. And it lives in this
/// CRATE and not in a frontend because both need it equally: the window and
/// the terminal show the same list, and two readers of the same directory
/// end up disagreeing on what counts as a broken profile.
#[must_use]
pub fn read_profiles(dir: &Path) -> Vec<crate::profile_picker::UserProfile> {
    let root = dir.join("profiles");
    norte_config::list_profiles(&root)
        .unwrap_or_default()
        .into_iter()
        .map(|name| {
            let toml = root.join(&name).join("norte.toml");
            let (title, problem) = match std::fs::read_to_string(&toml) {
                // A profile with no `norte.toml` is legitimate: it can bring
                // only its `layouts/` or its `keymap.toml`.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, None),
                Err(e) => (None, Some(e.kind().to_string())),
                Ok(raw) => match toml::from_str::<norte_config::NorteToml>(&raw) {
                    Ok(p) => (p.profile.title, None),
                    // The diagnostic does NOT quote the file's content: the
                    // message bar has a cap and a config can carry paths
                    // (#73).
                    Err(e) => (None, Some(e.message().to_owned())),
                },
            };
            crate::profile_picker::UserProfile {
                name,
                title,
                problem,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use norte_config::{Layer, Layers};

    use super::*;

    /// **Only what IS a listing enters `[profile.start]`.**
    ///
    /// A viewer, processes, or sites slot has no directory to remember, and
    /// lumping it in with the panel next to it would write a profile that,
    /// on opening, sends a viewer to a directory.
    #[test]
    fn start_only_carries_the_slots_that_are_a_listing() {
        use crate::layout::SlotId;
        use crate::layout::{Dir, KindId, Node};
        let tree = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        // Slot 2 does not answer: it is not a listing.
        let snap = profile_snapshot(
            &tree,
            &|SlotId(n)| (n == 1).then(|| norte_proto::VPath::parse("mem:///uno").unwrap()),
            None,
        );
        assert_eq!(snap.start, [("1".to_owned(), "mem:///uno".to_owned())]);
        assert!(snap.layout_toml.is_some(), "the layout does go whole");
        assert!(snap.ui.is_empty(), "[ui]'s scalars are not copied");
        assert!(snap.keymap.is_none());
    }

    /// `keymap.toml` travels BYTE for BYTE: it is the reader's file, with
    /// their comments, and rewriting it would change theirs.
    #[test]
    fn the_keymap_is_copied_as_is() {
        use crate::layout::SlotId;
        use crate::layout::{KindId, Node};
        let tree = Node::slot(SlotId(1), KindId::browser());
        let raw = b"# mio\n[pane]\nkeymap = []\n\xff".to_vec();
        let snap = profile_snapshot(&tree, &|SlotId(_)| None, Some(raw.clone()));
        assert_eq!(snap.keymap.as_deref(), Some(raw.as_slice()));
    }

    /// A tree with a user layer and a `work` profile whose content is given.
    fn tree_with_profile(
        files: &[(&str, &str)],
    ) -> (
        impl Fn(Option<&std::ffi::OsStr>) -> Layers + use<>,
        tempfile::TempDir,
    ) {
        let user = tempfile::tempdir().expect("tempdir");
        let dir = user.path().join("profiles").join("work");
        std::fs::create_dir_all(&dir).expect("mkdir");
        for (name, content) in files {
            std::fs::write(dir.join(name), content).expect("write");
        }
        let root = user.path().to_path_buf();
        let f = move |n: Option<&std::ffi::OsStr>| Layers {
            dirs: match n {
                None => vec![(root.clone(), Layer::User)],
                Some(n) => vec![
                    (root.clone(), Layer::User),
                    (root.join("profiles").join(n), Layer::Profile),
                ],
            },
        };
        (f, user)
    }

    /// D7 is a rule about the LAYER, not about `norte.toml`.
    ///
    /// A broken `keymap.toml` in the STICKY profile has to degrade the same
    /// way: `load_keymap_layer` is fatal for any layer that is not project,
    /// so going only through `norte.toml`'s loader a profile with a typo in
    /// a shortcut declared itself healthy and blew up later — leaving the
    /// reader outside the program with no way to choose another, which is
    /// exactly what D7 exists to prevent (#305).
    #[test]
    fn a_broken_keymap_in_the_sticky_profile_degrades() {
        // `keymap` (the WHOLE list) is a preset-only key: in a layer it is
        // an error naming the culprit file.
        let (layers_for, _g) = tree_with_profile(&[
            ("norte.toml", "[ui]\ntheme = \"nord\"\n"),
            (
                "keymap.toml",
                "[pane]\nkeymap = [{ on = [\"f5\"], run = \"pane.copy\" }]\n",
            ),
        ]);
        let work = std::ffi::OsStr::new("work");

        let r = load_with_profile(&layers_for, Some(work), norte_config::ProfileSource::Sticky)
            .expect("starts the same");
        assert_eq!(r.active, None, "no profile layer");
        assert!(r.degraded.is_some(), "and not silently");

        assert!(
            load_with_profile(
                &layers_for,
                Some(work),
                norte_config::ProfileSource::Explicit
            )
            .is_err(),
            "with --profile it is fatal: the reader named that profile"
        );
    }

    /// The same with `openers.toml`, which is the other half of the layer and
    /// is also fatal outside of project.
    #[test]
    fn a_broken_openers_in_the_sticky_profile_degrades() {
        let (layers_for, _g) = tree_with_profile(&[("openers.toml", "[[opener]]\nmime = 3\n")]);
        let r = load_with_profile(
            &layers_for,
            Some(std::ffi::OsStr::new("work")),
            norte_config::ProfileSource::Sticky,
        )
        .expect("starts the same");
        assert_eq!(r.active, None);
        assert!(r.degraded.is_some());
    }

    /// The happy path brings the PROFILE's keymap, and its kind travels so
    /// `split_at` can cut by it (D10).
    #[test]
    fn a_healthy_profile_contributes_its_keymap_layer() {
        let (layers_for, _g) = tree_with_profile(&[(
            "keymap.toml",
            "[pane]\nprepend_keymap = [{ on = [\"f5\"], run = \"pane.move\" }]\n",
        )]);
        let r = load_with_profile(
            &layers_for,
            Some(std::ffi::OsStr::new("work")),
            norte_config::ProfileSource::Explicit,
        )
        .expect("loads");
        assert_eq!(r.active.as_deref(), Some(std::ffi::OsStr::new("work")));
        assert_eq!(r.config.keymap_layer_kinds, vec![Layer::Profile]);
    }

    /// #28 security: an `openers.toml` in the PROJECT layer (`./.norte`) is
    /// IGNORED fail-closed — a hostile repo cannot inject a binary that runs
    /// when F4 is pressed. The USER layer IS honored.
    #[test]
    fn project_openers_are_ignored_user_ones_are_honored() {
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("openers.toml"),
            "[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n",
        )
        .unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("openers.toml"),
            "[[opener]]\nmime = \"text/*\"\ncommand = [\"curl-malicioso\", \"%f\"]\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (user.path().to_path_buf(), Layer::User),
                (project.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("loads");
        // Resolves to the USER's opener, never the project's.
        assert_eq!(
            cfg.openers
                .resolve_for("text/plain", "linux")
                .unwrap()
                .program(),
            "bat",
            "the project opener is ignored fail-closed"
        );
    }

    /// K3c c1, from the other side: what `norte_config::persist_keymap_bind`
    /// writes LOADS — through the real loader — and the binding it wrote
    /// RESOLVES. This is the pin for the whole point of that writer: it lives
    /// in `norte-config`, which is below the keymap grammar and cannot call
    /// `parse_keymap_layer`/`check_layer_keys`, so a section name or a list
    /// key that drifted there would only show up as a user's entire keymap
    /// silently reverting on the next reload.
    #[test]
    fn a_persisted_binding_loads_and_resolves() {
        use crate::keymap::{Effective, Screen, parse_chord, parse_keymap};

        let dir = tempfile::tempdir().unwrap();
        for section in ["global", "pane", "viewer", "dialog"] {
            norte_config::persist_keymap_bind(
                dir.path(),
                section,
                norte_config::KeymapList::Prepend,
                &["ctrl+g".to_owned()],
                "cursor.top",
            )
            .expect("persist");
        }
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("what the persister wrote LOADS");
        assert_eq!(cfg.keymap_layers.len(), 1);
        // A minimal preset: the pin is about the LAYER, not about a specific
        // preset, so `ctrl+g` cannot collide with whatever the preset of the
        // day binds.
        let preset =
            parse_keymap("[pane]\nkeymap = [{ on = [\"j\"], run = \"cursor.down\" }]\n").unwrap();
        // `build_for` is the one that runs `check_layer_keys`: had the
        // writer produced `keymap` (or `counts`, or `dialog_from`) this
        // would be `Err` and the user's config would have reverted whole.
        let eff = Effective::build_for(
            &preset,
            &cfg.keymap_layers,
            // The frontend's set: without it a catalogue command resolves
            // `NotHere` (this screen does not serve it) and
            // `single_chord_runs` would say `false` for a reason that is not
            // the one being tested.
            &["cursor.top", "cursor.down"],
            Screen::Browse,
        )
        .expect("the written layer is a legal layer");
        assert!(
            eff.single_chord_runs(parse_chord("ctrl+g").unwrap(), "cursor.top"),
            "the persisted binding resolves"
        );
    }

    /// K3c c1, the reason `persist_keymap_bind` carries a `KeymapList`: on a
    /// key the PRESET already binds in the SAME context, only a
    /// `prepend_keymap` wins. An `append_keymap` parses, loads, validates —
    /// and never fires, because the merge order is prepends → preset →
    /// appends and the FIRST one wins. Written as a test and not as a
    /// comment because it is exactly the mistake a shortcut editor makes
    /// silently: "saved", and the key keeps doing what it did before.
    #[test]
    fn only_a_prepend_overrides_the_preset_in_its_own_context() {
        use crate::keymap::{Effective, Screen, parse_chord, parse_keymap};

        let preset =
            parse_keymap("[pane]\nkeymap = [{ on = [\"f5\"], run = \"pane.copy\" }]\n").unwrap();
        let effective = |list| {
            let dir = tempfile::tempdir().unwrap();
            norte_config::persist_keymap_bind(
                dir.path(),
                "pane",
                list,
                &["f5".to_owned()],
                "pane.move",
            )
            .expect("persist");
            let layers = Layers {
                dirs: vec![(dir.path().to_path_buf(), Layer::User)],
            };
            let cfg = load(&layers).expect("loads");
            Effective::build_for(
                &preset,
                &cfg.keymap_layers,
                &["pane.copy", "pane.move"],
                Screen::Browse,
            )
            .expect("legal layer")
        };
        let f5 = parse_chord("f5").unwrap();
        assert!(
            effective(norte_config::KeymapList::Prepend).single_chord_runs(f5, "pane.move"),
            "a prepend overrides the preset: it is what a rebind needs"
        );
        assert!(
            effective(norte_config::KeymapList::Append).single_chord_runs(f5, "pane.copy"),
            "an append does NOT override the preset — the binding is written and does nothing"
        );
    }

    /// `dialog_from` is a PRESET key (ADR 0045). A layer that uses it has to
    /// find out by its own name: if the layer were parsed with
    /// `parse_keymap`, inheritance would resolve BEFORE the
    /// `has_full_keymap` check, which looks at `dialog.keymap` — and the
    /// user would get an error about `keymap`, a key they did not write.
    /// This test is the only safety net at the loader's level;
    /// `check_layer_keys` is tested separately and does not see this.
    #[test]
    fn a_layer_with_dialog_from_fails_naming_dialog_from() {
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("keymap.toml"),
            "dialog_from = \"orthodox\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(user.path().to_path_buf(), Layer::User)],
        };
        let e = load(&layers).expect_err("a layer cannot inherit [dialog]");
        let msg = e.to_string();
        assert!(msg.contains("dialog_from"), "{msg}");
        assert!(
            !msg.contains("prepend_keymap"),
            "the diagnostic names the wrong key: {msg}"
        );
    }

    /// K3c c2: `keymap_layers` carries one entry per dir that HAS the file, so
    /// its INDICES say nothing about which layer is which — here the user dir
    /// has no `keymap.toml` and the list is `[system, project]`, with the
    /// system layer sitting at index 0 where a positional guess would look for
    /// the user's. `keymap_layer_kinds` is the answer, parallel index by
    /// index; without it a shortcut editor cutting this list would model its
    /// write into the system layer (see `RebindSources::split_at`).
    #[test]
    fn keymap_layer_kinds_runs_parallel_to_the_layers_present() {
        let system = tempfile::tempdir().unwrap();
        std::fs::write(
            system.path().join("keymap.toml"),
            "[pane]\nprepend_keymap = [{ on = [\"j\"], run = \"cursor.down\" }]\n",
        )
        .unwrap();
        // The user has no file yet: a fresh install's first rebind.
        let user = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("keymap.toml"),
            "[pane]\nprepend_keymap = [{ on = [\"k\"], run = \"cursor.up\" }]\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (system.path().to_path_buf(), Layer::System),
                (user.path().to_path_buf(), Layer::User),
                (project.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("loads");
        assert_eq!(cfg.keymap_layers.len(), 2, "the user contributes no file");
        assert_eq!(
            cfg.keymap_layer_kinds,
            vec![Layer::System, Layer::Project],
            "the kind travels with the layer, not with the index"
        );
        assert!(
            cfg.keymap_layers[1].is_project(),
            "and the project one is still marked"
        );
    }

    /// #28: between layers, the higher one (user) wins the mimetype tie.
    #[test]
    fn user_openers_win_over_system() {
        let system = tempfile::tempdir().unwrap();
        std::fs::write(
            system.path().join("openers.toml"),
            "[[opener]]\nmime = \"text/*\"\ncommand = [\"less\", \"%f\"]\n",
        )
        .unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("openers.toml"),
            "[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (system.path().to_path_buf(), Layer::System),
                (user.path().to_path_buf(), Layer::User),
            ],
        };
        let cfg = load(&layers).expect("loads");
        assert_eq!(
            cfg.openers
                .resolve_for("text/plain", "linux")
                .unwrap()
                .program(),
            "bat",
            "the user layer (higher) wins"
        );
    }

    /// The combined loader wires all three passes: scalars, keymap layers,
    /// openers — and maps `quick_search` onto `nav::Mode`.
    #[test]
    fn load_combines_scalars_keymap_and_openers() {
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("norte.toml"),
            "[ui]\nquick_search = \"jump\"\n[keymap]\npreset = \"vim\"\n",
        )
        .unwrap();
        std::fs::write(
            user.path().join("keymap.toml"),
            "[pane]\nprepend_keymap = [{ on = [\"j\"], run = \"cursor.down\" }]\n",
        )
        .unwrap();
        std::fs::write(
            user.path().join("openers.toml"),
            "[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(user.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("loads");
        assert_eq!(cfg.common.preset, "vim");
        assert_eq!(cfg.quick_search_mode, nav::Mode::Jump);
        assert_eq!(cfg.keymap_layers.len(), 1);
        assert!(cfg.openers.resolve_for("text/plain", "linux").is_some());
        assert_eq!(
            cfg.common.sources.len(),
            3,
            "norte.toml + keymap.toml + openers.toml"
        );
    }
}
