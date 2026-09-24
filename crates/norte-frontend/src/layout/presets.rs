//! The five factory layouts.
//!
//! They live in TOML — the SAME format as `layouts/<name>.toml` and as
//! L2's session body (ADR 0058) — and not in Rust, so what norte ships and
//! what a user saves are the same thing: a preset can be copied, two
//! numbers changed, and kept.
//!
//! `NAMES` and [`source`] are SEPARATE items and are tested against each
//! other, as in [`crate::keymap::presets`]: a preset added to one and not
//! the other breaks CI instead of silently disappearing.
//!
//! The files are not hand-written. This module's test builds the five
//! trees with [`Node`]'s constructors and compares; with
//! `NORTE_UPDATE_GOLDEN=1` it rewrites them. Nesting four levels of TOML by
//! hand is how a `sizes` ends up with one fewer entry than its `children`.

use super::{LayoutError, Node};

/// The usual one: two listings, the tasks strip and the status bar.
pub const ORTHODOX: &str = include_str!("../../presets/layout/orthodox.toml");
/// A single listing. A narrow terminal, an ssh session, a shared screen on
/// a call.
pub const SIMPLE: &str = include_str!("../../presets/layout/simple.toml");
/// Two listings with the places sidebar.
pub const KRUSADER: &str = include_str!("../../presets/layout/krusader.toml");
/// One listing with places, a docked viewer and the processes panel.
pub const EXPLORER: &str = include_str!("../../presets/layout/explorer.toml");
/// Everything on: places, two listings, viewer, attributes and processes.
pub const FULL: &str = include_str!("../../presets/layout/full.toml");

/// The five names, in the order the picker shows them.
pub const NAMES: &[&str] = &["orthodox", "simple", "krusader", "explorer", "full"];

/// A factory preset's TOML, or `None` if that name is not one.
#[must_use]
pub fn source(name: &str) -> Option<&'static str> {
    match name {
        "orthodox" => Some(ORTHODOX),
        "simple" => Some(SIMPLE),
        "krusader" => Some(KRUSADER),
        "explorer" => Some(EXPLORER),
        "full" => Some(FULL),
        _ => None,
    }
}

/// A factory preset's tree, parsed and validated.
///
/// # Errors
///
/// [`LayoutError::NotFound`] if the name is not a factory one, and whatever
/// parsing or [`super::validate`] return — which in practice never
/// happens, because this module's tests parse all five on every CI run.
///
/// ```
/// use norte_frontend::layout::presets;
///
/// let tree = presets::tree("simple").expect("factory one");
/// // A listing, the tasks strip and the status bar.
/// assert_eq!(tree.slot_ids().len(), 3);
/// assert!(presets::tree("no-exists").is_err());
/// ```
pub fn tree(name: &str) -> Result<Node, LayoutError> {
    let text = source(name).ok_or_else(|| LayoutError::NotFound(name.to_owned()))?;
    let tree: Node = toml::from_str(text).map_err(|e| LayoutError::Parse(e.to_string()))?;
    super::validate(&tree)?;
    Ok(tree)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::config::to_toml;
    use crate::layout::{Bindings, Dir, Edge, Follow, KindId, KindRegistry, RoleId, Size, SlotId};

    /// The well-known slots. The first four are the ones the TUI has
    /// already named since L1a, so switching preset does not renumber the
    /// panels underneath.
    const LEFT: SlotId = SlotId(1);
    const RIGHT: SlotId = SlotId(2);
    const TASKS: SlotId = SlotId(3);
    const STATUS: SlotId = SlotId(4);
    const PLACES: SlotId = SlotId(5);
    const PREVIEW: SlotId = SlotId(6);
    const PROCESSES: SlotId = SlotId(7);
    const METADATA: SlotId = SlotId(8);

    fn browser(id: SlotId) -> Node {
        Node::slot(id, KindId::browser())
    }

    /// A panel that LOOKS at the active listing: the docked viewer and the
    /// attribute sheet. Without the binding they are empty boxes.
    fn siguiendo(id: SlotId, kind: &str) -> Node {
        Node::slot_bound(
            id,
            KindId::new(kind),
            Bindings {
                follows: Some(Follow::Role(RoleId::Active)),
            },
        )
    }

    /// The body with what is below and the status bar. All five end the
    /// same way: weighted body, strip or panel, and a status row.
    fn with_chrome(body: Node, below: Node, below_height: Size) -> Node {
        Node::Split {
            dir: Dir::Vertical,
            children: vec![body, below, Node::slot(STATUS, KindId::new("status"))],
            sizes: vec![Size::Weight(1), below_height, Size::Fixed(1)],
        }
    }

    fn tasks() -> Node {
        Node::slot(TASKS, KindId::new("tasks"))
    }

    fn processes() -> Node {
        Node::slot(PROCESSES, KindId::new("processes"))
    }

    fn esperado(name: &str) -> Node {
        match name {
            "orthodox" => with_chrome(
                Node::split(Dir::Horizontal, vec![browser(LEFT), browser(RIGHT)]),
                tasks(),
                Size::Auto,
            ),
            "simple" => with_chrome(browser(LEFT), tasks(), Size::Auto),
            // The sidebar goes next to the LISTINGS, not next to the
            // chrome: that is exactly what `dock` produces, and the last
            // test pins it.
            "krusader" => with_chrome(
                Node::Split {
                    dir: Dir::Horizontal,
                    children: vec![
                        Node::slot(PLACES, KindId::new("places")),
                        browser(LEFT),
                        browser(RIGHT),
                    ],
                    sizes: vec![Size::Fixed(16), Size::Weight(1), Size::Weight(1)],
                },
                tasks(),
                Size::Auto,
            ),
            "explorer" => with_chrome(
                Node::Split {
                    dir: Dir::Horizontal,
                    children: vec![
                        Node::slot(PLACES, KindId::new("places")),
                        browser(LEFT),
                        siguiendo(PREVIEW, "viewer"),
                    ],
                    sizes: vec![Size::Fixed(16), Size::Weight(1), Size::Weight(1)],
                },
                processes(),
                Size::Fixed(8),
            ),
            "full" => with_chrome(
                Node::Split {
                    dir: Dir::Horizontal,
                    children: vec![
                        Node::slot(PLACES, KindId::new("places")),
                        browser(LEFT),
                        browser(RIGHT),
                        Node::split(
                            Dir::Vertical,
                            vec![
                                siguiendo(PREVIEW, "viewer"),
                                siguiendo(METADATA, "metadata"),
                            ],
                        ),
                    ],
                    sizes: vec![
                        Size::Fixed(16),
                        Size::Weight(1),
                        Size::Weight(1),
                        Size::Fixed(30),
                    ],
                },
                processes(),
                Size::Fixed(8),
            ),
            other => panic!("unknown preset: {other}"),
        }
    }

    fn path(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("presets/layout")
            .join(format!("{name}.toml"))
    }

    /// The shipped file IS the tree above. With `NORTE_UPDATE_GOLDEN` it is
    /// rewritten; without it, it is compared.
    #[test]
    fn the_five_files_are_the_five_trees() {
        for name in NAMES {
            let want = to_toml(&esperado(name)).expect("serializes");
            if std::env::var_os("NORTE_UPDATE_GOLDEN").is_some() {
                std::fs::write(path(name), &want).expect("writes");
            }
            let have = std::fs::read_to_string(path(name))
                .expect("the preset — regenerate it with NORTE_UPDATE_GOLDEN=1");
            assert_eq!(
                have, want,
                "{name}: regenerate it with NORTE_UPDATE_GOLDEN=1"
            );
        }
    }

    /// What really matters: what `tree` returns is what was expected.
    #[test]
    fn all_five_parse_into_what_they_claim_to_be() {
        for name in NAMES {
            assert_eq!(tree(name).expect(name), esperado(name), "{name}");
        }
    }

    /// A kind this binary does not declare is painted as a box with its
    /// name. In a FACTORY preset that would be a broken built-in preset.
    #[test]
    fn no_preset_names_a_kind_that_does_not_exist() {
        let reg = KindRegistry::builtin();
        for name in NAMES {
            let tree = tree(name).expect(name);
            for id in tree.slot_ids() {
                let kind = tree.kind_of(id).expect("kind");
                assert!(
                    reg.get(kind).is_some(),
                    "{name}: kind {} does not exist",
                    kind.as_str()
                );
            }
        }
    }

    /// Repeated ids: `validate` already rejects them, so this checks that
    /// none of the five ever produces the error.
    #[test]
    fn no_preset_repeats_a_slot() {
        for name in NAMES {
            assert!(
                tree(name).expect(name).duplicate_slot_ids().is_empty(),
                "{name}"
            );
        }
    }

    /// EVERY preset, on EVERY reasonable screen, leaves a usable listing.
    ///
    /// `full` shipped with a 40x10 screen with no listing at all — the
    /// fixed ones charge first, 16 for the sidebar plus 30 for the right
    /// column out of 40 columns left both browsers at zero — and the
    /// snapshot that approved it was the only gate there was: it painted
    /// whatever it painted, so it blessed the emptiness (#244 M4). What was
    /// missing was the PROPERTY, and this is it. The fix (#229, setting the
    /// chrome aside) lives in `resolve`; this test is what says whether it
    /// is still doing its job.
    #[test]
    fn no_preset_leaves_a_screen_without_a_usable_listing() {
        use crate::layout::{Rect, resolve};

        // The floor #229's rescue promises: `resolve::CONTENT`, the
        // ceiling each kind's minimum is clamped to. On roomy screens the
        // listing's OWN minimum is also required, which is what is seen
        // when nothing has to be squeezed.
        const USABLE: (u16, u16) = (12, 4);

        let reg = KindRegistry::builtin();
        let (mw, mh) = reg.min_of(&crate::layout::KindId::browser());
        for name in NAMES {
            let tree = tree(name).expect(name);
            for (w, h) in [(40_u16, 10_u16), (60, 15), (80, 24), (120, 40)] {
                // At 120 columns there is nothing to squeeze and the
                // listing's OWN minimum is required; below that the
                // rescue's floor rules, which is what #229 promises —
                // `full` at 80 leaves 17 columns per listing (16 for the
                // sidebar + 30 for the attribute sheet are fixed) and that
                // is tight, not broken.
                let (pw, ph) = if w >= 120 { (mw, mh) } else { USABLE };
                let res = resolve(Rect::new(0, 0, w, h), &tree, &reg);
                let best = res
                    .placements
                    .iter()
                    .filter(|(id, _)| {
                        tree.kind_of(*id)
                            .is_some_and(|k| *k == crate::layout::KindId::browser())
                    })
                    .map(|(_, r)| (r.width, r.height))
                    .max();
                let Some((bw, bh)) = best else {
                    panic!("{name} at {w}x{h}: no listing placed");
                };
                assert!(
                    bw >= pw && bh >= ph,
                    "{name} at {w}x{h}: the best listing measures {bw}x{bh}, below {pw}x{ph}"
                );
            }
        }
    }

    /// `NAMES` and `source` are two items and can fall out of sync. Not
    /// here.
    #[test]
    fn the_catalog_and_the_search_say_the_same_thing() {
        for name in NAMES {
            assert!(source(name).is_some(), "{name} in NAMES and not in source");
        }
        assert!(source("no-existe").is_none());
        assert!(matches!(tree("no-existe"), Err(LayoutError::NotFound(_))));
    }

    /// `krusader` is `orthodox` with the sidebar docked. If this breaks,
    /// either the preset stopped being reachable from the keyboard, or
    /// `dock` changed its mind about where a sidebar goes; both need
    /// looking at.
    #[test]
    fn krusader_is_orthodox_with_sidebar_on() {
        let docked = tree("orthodox").expect("orthodox").dock(
            LEFT,
            Edge::Left,
            Size::Fixed(16),
            &Node::slot(PLACES, KindId::new("places")),
        );
        assert_eq!(docked, tree("krusader").expect("krusader"));
    }
}
