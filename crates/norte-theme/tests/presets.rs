//! Every embedded preset parses, covers every role and every node type
//! with an EXPLICIT color (not only fallback), and carries a set of extensions
//! (ADR 0020 D3, phase T3).

use norte_theme::{FileKind, Role, Theme, preset_names, preset_source};

const KINDS: &[FileKind] = &[
    FileKind::Dir,
    FileKind::Symlink,
    FileKind::Executable,
    FileKind::Fifo,
    FileKind::Socket,
    FileKind::BlockDevice,
    FileKind::CharDevice,
];

#[test]
fn every_preset_parses_and_is_complete() {
    let names = preset_names();
    assert!(names.contains(&"default"));
    assert!(names.contains(&"catppuccin-mocha"));
    assert!(names.contains(&"gruvbox-dark"));
    assert!(names.contains(&"nord"));
    assert!(names.contains(&"vscode-dark"));
    assert!(names.contains(&"vscode-light"));

    for name in names {
        let src = preset_source(name).expect("preset source");
        let theme =
            Theme::from_toml(src).unwrap_or_else(|e| panic!("[{name}] does not parse: {e}"));
        assert_eq!(theme.name.as_deref(), Some(name), "[{name}] name matches");

        // Every role has an EXPLICIT foreground color (a preset colors everything, it does not
        // stay on the monochrome fallback).
        //
        // CORE, not ALL: the ten CHROME roles (spec 2026-09-11, F2) are
        // derived in the window's stylesheet from colors the theme
        // already has, so requiring them from every preset would mean eighty invented
        // values — and a monochrome `hover` is not a cautious hover, it is an
        // invisible one. The `vscode-*` presets do define them, because for them
        // the chrome is the point.
        for &role in Role::CORE {
            let st = theme.style(role);
            assert!(
                st.fg.is_some() || st.bg.is_some(),
                "[{name}] role {role:?} has no color"
            );
        }

        // Every node type has a color by kind.
        for &kind in KINDS {
            let st = theme.file_style(b"x", kind);
            assert!(st.fg.is_some(), "[{name}] kind {kind:?} has no color");
        }

        // And the usual extensions are colored (ext beats kind).
        for ext in ["main.rs", "a.zip", "foto.png", "readme.md"] {
            assert!(
                theme
                    .file_style(ext.as_bytes(), FileKind::Regular)
                    .fg
                    .is_some(),
                "[{name}] the extension of {ext} is not colored"
            );
        }
    }
}

/// G1: the retro presets declare [effects] (the GUI interprets them; the TUI
/// ignores them) and pass the same completeness as the rest.
#[test]
fn retro_presets_carry_effects() {
    for name in ["retro-crt", "retro-crt-amber"] {
        let t = norte_theme::Theme::preset(name)
            .expect("parses")
            .expect("registered preset");
        assert!(t.has_effects(), "{name} without [effects]");
    }
}

#[test]
fn preset_by_name_and_default() {
    assert_eq!(
        Theme::preset("nord").unwrap().unwrap().name.as_deref(),
        Some("nord")
    );
    // Unknown name = None (the caller will treat it as a path).
    assert!(Theme::preset("does-not-exist").unwrap().is_none());
    // The default is always there.
    assert_eq!(Theme::preset_default().name.as_deref(), Some("default"));
}

/// The SEMANTIC signals (`error`, `warning` and above all `hostile-badge`,
/// which marks a masked name — spec §6, security surface) cannot
/// fall below WCAG AA's 4.5:1 against THEIR theme's background: they are exactly
/// the ones that must be read when something goes wrong. The rest
/// of the palette (borders, accents, status bar) is left out on purpose
/// —there the theme's taste rules— and its lower floor is pinned by
/// `norte-tui`'s render.
#[test]
fn semantic_signals_reach_wcag_aa_in_every_preset() {
    fn luminance(c: norte_theme::Color) -> f64 {
        let channel = |v: u8| {
            let s = f64::from(v) / 255.0;
            if s <= 0.039_28 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b)
    }
    fn contrast(a: norte_theme::Color, b: norte_theme::Color) -> f64 {
        let (l1, l2) = (luminance(a), luminance(b));
        let (high, low) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
        (high + 0.05) / (low + 0.05)
    }

    for name in preset_names() {
        let theme = Theme::preset(name)
            .expect("the preset parses")
            .expect("the preset exists");
        // Without its own `background` the terminal's rules: there is nothing to
        // measure against (and the theme inherits the user's pairing).
        let Some(background) = theme.style(Role::Background).bg else {
            continue;
        };
        for role in [Role::Error, Role::Warning, Role::HostileBadge] {
            let Some(fg) = theme.style(role).fg else {
                continue; // no color: the monochrome fallback inherits the foreground
            };
            let r = contrast(fg, background);
            assert!(
                r >= 4.5,
                "{name}: {role:?} ({}) gives {r:.2}:1 over the background ({}) — WCAG AA asks for 4.5",
                fg.to_hex(),
                background.to_hex()
            );
        }
    }
}
