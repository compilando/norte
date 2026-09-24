//! Per-entry plugin decorations (row badges, G3b, ADR 0037 decision 2):
//! sanitizing + role validation shared by the TUI and the GUI — the same
//! criterion as sanitizing a preview's styled spans
//! (`crate::viewer::Viewer::with_plugin_preview_styled`): THIRD-PARTY text,
//! never trusted without going through [`crate::display_name`], and a
//! `role` that arrives UNVALIDATED over the wire (`norte-core` is headless,
//! it does not know `norte_theme::Role` — see the rustdoc of
//! `norte_core::Backend::plugin_decorate`) is validated HERE, where the
//! frontend finally knows the theme.

use std::collections::HashMap;

use norte_proto::VPath;
use norte_proto::methods::{DecorationSlot, DecorationWire, PluginDecorations};

/// Cap on a badge AFTER masking (ADR 0037 decision table 1), in CHARACTERS
/// (not bytes: consistent with the rest of the display caps — `description`,
/// `title`…). The server ALREADY applies an equivalent cap before sending;
/// this is defense in depth — a remote frontend does not blindly trust a
/// third-party daemon.
pub const BADGE_MAX_CHARS: usize = 8;

/// A SANITIZED decoration ready to paint: `badge` already masked and
/// truncated to [`BADGE_MAX_CHARS`] characters (or `None` — no badge for that
/// entry, either absent or empty after masking/truncating, both cases
/// identical for rendering); `role` already validated against
/// `norte_theme::Role` (`None` = no role, or a name the frontend does not
/// recognize).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Decoration {
    /// Short badge, already masked and capped, or `None` = no badge.
    pub badge: Option<String>,
    /// The badge is painted DIFFERENT from what it is.
    ///
    /// A plugin writes it and it is painted right next to a file name, which
    /// is the place where a difference between what is seen and what there
    /// is matters most. The mark used to be computed and thrown away, as in
    /// six other surfaces of the graphical window.
    pub badge_hostile: bool,
    /// Already-validated semantic role, or `None` = no recognized role.
    pub role: Option<norte_theme::Role>,
    /// The row's ICON (ADR 0105): what the first `icon`-slot decorator
    /// returned, already masked and capped. Painted to the LEFT of the name,
    /// in a fixed-width column; the badge above, to the right. The two
    /// coexist: they come from different plugins.
    pub icon: Option<String>,
    /// The icon is painted different from what it is. Same reason as
    /// `badge_hostile`.
    pub icon_hostile: bool,
}

impl Decoration {
    /// Nothing to paint: neither icon nor badge.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.badge.is_none() && self.icon.is_none()
    }
}

/// Sanitizes ONE raw wire [`DecorationWire`]: masks `badge`
/// ([`crate::display_name`], the same sanitizing as any plugin text) and
/// truncates it to [`BADGE_MAX_CHARS`] characters AFTER masking; validates
/// `role` against `norte_theme::Role::from_kebab_requestable` (an unknown
/// name collapses to `None`, never a panic nor a free string another layer
/// must reinterpret — the same criterion ADR 0037 applies to
/// `SpanWire::role`).
///
/// The vocabulary is the REQUESTABLE one and not all of `Role` (spec
/// 2026-09-11, F2): a plugin describes content, so it names what a piece
/// MEANS and not the window's chrome or its state. A role from that set
/// degrades the same as an unknown name, which is what ADR 0037 already
/// promised.
#[must_use]
pub fn sanitize_decoration(w: &DecorationWire) -> Decoration {
    let mut badge_hostile = false;
    let badge = w.badge.as_deref().and_then(|b| {
        let (masked, hostile) = crate::display_name(b.as_bytes());
        let truncated: String = masked.chars().take(BADGE_MAX_CHARS).collect();
        // The mark stays only if a badge remains: a badge that masks ENTIRELY
        // to empty is not painted, and saying what is painted differs from
        // reality when nothing is painted is noise.
        let has_one = !truncated.is_empty();
        badge_hostile = hostile && has_one;
        has_one.then_some(truncated)
    });
    let role = w
        .role
        .as_deref()
        .and_then(norte_theme::Role::from_kebab_requestable);
    Decoration {
        badge,
        badge_hostile,
        role,
        icon: None,
        icon_hostile: false,
    }
}

/// The same as [`sanitize_decoration`] for an ICON decorator (ADR 0105): the
/// text goes into the icon slot, with the same masking and the same cap. The
/// role does not apply: an icon is painted with the entry's color, not the
/// state's theme color.
#[must_use]
pub fn sanitize_icon(w: &DecorationWire) -> Decoration {
    let s = sanitize_decoration(w);
    Decoration {
        badge: None,
        badge_hostile: false,
        role: None,
        icon: s.badge,
        icon_hostile: s.badge_hostile,
    }
}

/// Flattens the wire's decorator OVERLAY (`PluginDecorateResult::plugins`,
/// one element per consented `decorator` plugin) into ONE [`Decoration`] per
/// path, indexed by [`VPath`], with TWO slots (ADR 0105): the icon, to the
/// left of the name, and the badge, to the right. Each slot is filled by the
/// FIRST plugin for that slot (in the order `plugins` arrives — the
/// catalogue's, see `PluginRegistry::resolve_decorators`) whose text is
/// neither `None` NOR masks to nothing: a badge left empty after masking is
/// not a badge, and does not block the next one. An icon does not cover a
/// badge nor the other way around; two icons do cover each other, and order
/// wins. A path left with nothing in either slot does not enter the map.
///
/// `paths` and `pd.decorations` are walked POSITIONALLY (`zip`, stops at the
/// shorter one): defense in depth in case a remote daemon did not honor the
/// wire's 1:1 contract (already validated server-side by
/// `decorations_to_wire_checked`, but a client does not blindly trust it).
#[must_use]
pub fn merge_decorations(
    paths: &[VPath],
    plugins: &[PluginDecorations],
) -> HashMap<VPath, Decoration> {
    let mut out: HashMap<VPath, Decoration> = HashMap::new();
    for pd in plugins {
        for (path, wire) in paths.iter().zip(pd.decorations.iter()) {
            if wire.badge.is_none() {
                continue;
            }
            // One slot per plugin and the FIRST one for each slot wins (ADR
            // 0105): an icon does not cover a badge nor the other way
            // around, because they are two different spots on the row; two
            // icons do cover each other, and the catalogue's order wins.
            let d = out.entry(path.clone()).or_default();
            match pd.slot {
                DecorationSlot::Icon if d.icon.is_none() => {
                    let s = sanitize_icon(wire);
                    d.icon = s.icon;
                    d.icon_hostile = s.icon_hostile;
                }
                DecorationSlot::Badge if d.badge.is_none() => {
                    let s = sanitize_decoration(wire);
                    d.badge = s.badge;
                    d.badge_hostile = s.badge_hostile;
                    d.role = s.role;
                }
                DecorationSlot::Icon | DecorationSlot::Badge => {}
            }
        }
    }
    // An entry that masked entirely to nothing is not a decoration.
    out.retain(|_, d| !d.is_empty());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_decoration_truncates_after_masking_not_before() {
        // 10 control characters (masked to 10 '�'), then truncated to 8.
        let w = DecorationWire {
            badge: Some("\n".repeat(10)),
            role: None,
        };
        let d = sanitize_decoration(&w);
        assert_eq!(
            d.badge.as_deref().map(str::chars).map(Iterator::count),
            Some(8)
        );
    }

    #[test]
    fn sanitize_decoration_empty_after_masking_is_none() {
        let w = DecorationWire {
            badge: Some(String::new()),
            role: None,
        };
        assert_eq!(sanitize_decoration(&w).badge, None);
    }

    #[test]
    fn sanitize_decoration_unknown_role_collapses_to_none() {
        let w = DecorationWire {
            badge: None,
            role: Some("not-a-real-role".to_string()),
        };
        assert_eq!(sanitize_decoration(&w).role, None);
    }

    #[test]
    fn sanitize_decoration_valid_role_is_recognized() {
        let w = DecorationWire {
            badge: None,
            role: Some("warning".to_string()),
        };
        assert_eq!(
            sanitize_decoration(&w).role,
            norte_theme::Role::from_kebab("warning")
        );
    }

    fn vp(s: &str) -> VPath {
        VPath::parse(s).unwrap()
    }

    #[test]
    fn merge_decorations_first_plugin_with_a_badge_wins() {
        let paths = vec![vp("mem:///a.rs"), vp("mem:///b.rs")];
        let plugins = vec![
            PluginDecorations {
                plugin_id: "p1".into(),
                slot: DecorationSlot::Badge,
                decorations: vec![
                    DecorationWire {
                        badge: None,
                        role: None,
                    },
                    DecorationWire {
                        badge: Some("M".into()),
                        role: Some("warning".into()),
                    },
                ],
            },
            PluginDecorations {
                plugin_id: "p2".into(),
                slot: DecorationSlot::Badge,
                decorations: vec![
                    DecorationWire {
                        badge: Some("X".into()),
                        role: None,
                    },
                    DecorationWire {
                        badge: Some("Y".into()),
                        role: None,
                    },
                ],
            },
        ];
        let merged = merge_decorations(&paths, &plugins);
        // a.rs: p1 has no badge, p2 has "X" -> p2 wins.
        assert_eq!(
            merged.get(&vp("mem:///a.rs")).unwrap().badge.as_deref(),
            Some("X")
        );
        // b.rs: p1's "M" is already there first -> p1 wins, p2 does not override it.
        assert_eq!(
            merged.get(&vp("mem:///b.rs")).unwrap().badge.as_deref(),
            Some("M")
        );
    }

    #[test]
    fn merge_decorations_no_badge_in_any_plugin_does_not_enter_the_map() {
        let paths = vec![vp("mem:///a.rs")];
        let plugins = vec![PluginDecorations {
            plugin_id: "p1".into(),
            slot: DecorationSlot::Badge,
            decorations: vec![DecorationWire {
                badge: None,
                role: None,
            }],
        }];
        let merged = merge_decorations(&paths, &plugins);
        assert!(merged.is_empty());
    }

    /// ADR 0105: an icon and a badge are two SLOTS on the row and do not
    /// cover each other; two icons do, and the first one wins. An icon with
    /// a role loses it: it is painted with the entry's color.
    #[test]
    fn merge_decorations_icon_and_badge_coexist_and_two_icons_do_not() {
        let paths = vec![vp("mem:///a.rs")];
        let deco = |b: &str, r: Option<&str>| DecorationWire {
            badge: Some(b.into()),
            role: r.map(str::to_owned),
        };
        let plugins = vec![
            PluginDecorations {
                plugin_id: "icons".into(),
                slot: DecorationSlot::Icon,
                decorations: vec![deco("🦀", Some("warning"))],
            },
            PluginDecorations {
                plugin_id: "git".into(),
                slot: DecorationSlot::Badge,
                decorations: vec![deco("M", Some("warning"))],
            },
            PluginDecorations {
                plugin_id: "other-icons".into(),
                slot: DecorationSlot::Icon,
                decorations: vec![deco("X", None)],
            },
        ];
        let merged = merge_decorations(&paths, &plugins);
        let d = &merged[&paths[0]];
        assert_eq!(d.icon.as_deref(), Some("🦀"), "the first icon");
        assert_eq!(d.badge.as_deref(), Some("M"), "and the badge, separately");
        assert_eq!(d.role, norte_theme::Role::from_kebab("warning"));
        assert!(!d.icon_hostile);
    }

    /// A badge left with NOTHING — `Some("")`, which sanitizing turns into
    /// `None` — is not a badge: it does not block the next plugin's. It used
    /// to be that the first plugin with `Some(..)` claimed the path even if
    /// its text ended up empty, and the map kept a decoration with nothing to
    /// paint.
    #[test]
    fn merge_decorations_a_badge_masked_to_nothing_does_not_block_the_next_one() {
        let paths = vec![vp("mem:///a.rs")];
        let deco = |b: &str| DecorationWire {
            badge: Some(b.into()),
            role: None,
        };
        let plugins = vec![
            PluginDecorations {
                plugin_id: "p1".into(),
                slot: DecorationSlot::Badge,
                decorations: vec![deco("")],
            },
            PluginDecorations {
                plugin_id: "p2".into(),
                slot: DecorationSlot::Badge,
                decorations: vec![deco("M")],
            },
        ];
        let merged = merge_decorations(&paths, &plugins);
        assert_eq!(merged[&paths[0]].badge.as_deref(), Some("M"));
        // And with only the empty one, the path does not enter the map.
        let alone = vec![PluginDecorations {
            plugin_id: "p1".into(),
            slot: DecorationSlot::Badge,
            decorations: vec![deco("")],
        }];
        assert!(merge_decorations(&paths, &alone).is_empty());
    }
}
