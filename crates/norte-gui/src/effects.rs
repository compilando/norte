//! Interpreter for a theme's `[effects]` section (schema v1, ADR 0036).
//!
//! `norte-theme` keeps `Theme.effects` deliberately untyped
//! (`Option<toml::Value>`, ADR 0020 D4) so a TUI-only build never links
//! GUI-effects-parsing code and an older GUI build never chokes on a newer
//! theme. This module is the GUI-only typed, clamped interpretation of that
//! opaque value: [`EffectsV1::from_theme`] turns a [`norte_theme::Theme`]
//! into an [`EffectsV1`] the renderer can paint directly, with no further
//! validation needed downstream.
//!
//! # Leniency contract (ADR 0036 §2)
//!
//! - `[effects]` absent → [`None`]: paint nothing.
//! - `[effects]` present → always [`Some`], even if every key inside it is
//!   malformed. A degraded-to-empty `[effects]` renders identically to an
//!   absent one (all four fields [`None`]), but the distinction matters for
//!   callers that only need "is there an effects section at all"
//!   ([`norte_theme::Theme::has_effects`]).
//! - Numeric values out of range **clamp** to the nearest bound — never an
//!   error, never a skip. A slider-adjacent typo (`opacity = 2.0`) is
//!   ordinary user data.
//! - A key with the wrong shape (wrong TOML type, e.g. a string where a
//!   table is expected) is invalid **for that key only**: it is dropped
//!   (`None` in the corresponding [`EffectsV1`] field) and the rest of the
//!   section is interpreted normally.
//! - Unknown keys inside `[effects]` (from a newer schema version) are
//!   ignored; their names are logged once, never their values.
//! - A subfield that is present but has the wrong TOML type invalidates the
//!   whole containing key (same per-key granularity as above). A subfield
//!   that is simply *absent* is not an error: it falls back to that
//!   subfield's documented default (below), then the default is clamped like
//!   any other value.
//!
//! # Subfield defaults (used when a known key's table omits the subfield)
//!
//! | Key                     | Default |
//! | ------------------------ | ------- |
//! | `scanlines.opacity`      | `0.1`   |
//! | `scanlines.spacing_px`   | `3`     |
//! | `vignette.strength`      | `0.3`   |
//! | `glow.strength`          | `0.4`   |
//! | `bezel.radius_px`        | `10`    |
//! | `bezel.inset`            | `false` |
//!
//! # Clamp ranges (ADR 0036 §2)
//!
//! | Key                     | Range          |
//! | ------------------------ | -------------- |
//! | `scanlines.opacity`      | `[0.0, 0.35]`  |
//! | `scanlines.spacing_px`   | `[2, 16]`      |
//! | `vignette.strength`      | `[0.0, 0.6]`   |
//! | `glow.strength`          | `[0.0, 1.0]`   |
//! | `bezel.radius_px`        | `[0, 32]`      |
//!
//! `bezel.inset` is a bool: no clamp applies.
//!
//! # Logging
//!
//! `norte-gui` has no `tracing` dependency (checked: absent from
//! `Cargo.toml`). Unlike this crate's `NORTE_GUI_DEBUG`-gated `eprintln!`
//! diagnostic convention (see `main.rs`), a malformed `[effects]` key is
//! user-actionable — it means a hand-edited or newer-schema theme file has a
//! typo or an unsupported shape — so these warnings are **not** gated behind
//! `NORTE_GUI_DEBUG`: they always go to stderr, prefixed `[norte-gui]` like
//! every other diagnostic line this crate emits.

// This module's public API is exercised only by its own tests until the G1
// plan's render-integration task (`docs/superpowers/plans/2026-07-23-g1-effects-retro-crt.md`
// Task 4) wires `EffectsV1::from_theme` into `NorteGui`'s render path. `norte-gui`
// is a binary crate, so `pub` alone does not silence `dead_code` the way it
// would in a library — every code path here is already covered by the test
// module below, so this is a scope allowance, not a correctness gap.
#![allow(dead_code)]

const SCANLINES_OPACITY_RANGE: (f32, f32) = (0.0, 0.35);
const SCANLINES_SPACING_RANGE: (u8, u8) = (2, 16);
const VIGNETTE_STRENGTH_RANGE: (f32, f32) = (0.0, 0.6);
const GLOW_STRENGTH_RANGE: (f32, f32) = (0.0, 1.0);
const BEZEL_RADIUS_RANGE: (u8, u8) = (0, 32);

const DEFAULT_SCANLINES_OPACITY: f32 = 0.1;
const DEFAULT_SCANLINES_SPACING: u8 = 3;
const DEFAULT_VIGNETTE_STRENGTH: f32 = 0.3;
const DEFAULT_GLOW_STRENGTH: f32 = 0.4;
const DEFAULT_BEZEL_RADIUS: u8 = 10;
const DEFAULT_BEZEL_INSET: bool = false;

const KNOWN_KEYS: [&str; 4] = ["scanlines", "vignette", "glow", "bezel"];

/// Scanline overlay: `[effects.scanlines]`. See the [module docs](self) for
/// defaults and clamp ranges.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scanlines {
    /// Overlay opacity, clamped to `[0.0, 0.35]` (accessibility guard: the
    /// worst case must still keep AA text contrast, ADR 0036 §2).
    pub opacity: f32,
    /// Vertical spacing between scanlines in pixels, clamped to `[2, 16]`.
    pub spacing_px: u8,
}

/// Edge-darkening overlay: `[effects.vignette]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vignette {
    /// Overlay strength, clamped to `[0.0, 0.6]`.
    pub strength: f32,
}

/// Foreground-brightening amount: `[effects.glow]`. Interpreted as
/// `lerp(fg, white, strength * 0.25)` (ADR 0036 §4) — approximate glow via
/// brightened colors, not shader bloom.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glow {
    /// Brightening strength, clamped to `[0.0, 1.0]`.
    pub strength: f32,
}

/// Root container bezel: `[effects.bezel]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bezel {
    /// Corner radius in pixels, clamped to `[0, 32]`.
    pub radius_px: u8,
    /// Whether the bezel renders an inset shadow.
    pub inset: bool,
}

/// The typed, clamped interpretation of a theme's `[effects]` section
/// (schema v1, ADR 0036). Every field is independently optional: a theme may
/// declare any subset of scanlines/vignette/glow/bezel. See the
/// [module docs](self) for the full leniency contract.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct EffectsV1 {
    /// `[effects.scanlines]`, if present and well-shaped.
    pub scanlines: Option<Scanlines>,
    /// `[effects.vignette]`, if present and well-shaped.
    pub vignette: Option<Vignette>,
    /// `[effects.glow]`, if present and well-shaped.
    pub glow: Option<Glow>,
    /// `[effects.bezel]`, if present and well-shaped.
    pub bezel: Option<Bezel>,
}

impl EffectsV1 {
    /// Interprets `theme.effects` per the schema-v1 leniency contract
    /// (module docs, ADR 0036).
    ///
    /// Returns [`None`] when the theme has no `[effects]` section at all.
    /// Returns `Some` in every other case, even when every key inside the
    /// section is malformed — a fully-degraded section renders as "no
    /// effects" but is still recorded as "present" for
    /// [`norte_theme::Theme::has_effects`] callers.
    #[must_use]
    pub fn from_theme(theme: &norte_theme::Theme) -> Option<Self> {
        let value = theme.effects.as_ref()?;

        let Some(table) = value.as_table() else {
            warn("[effects] section is not a table: treating as empty");
            return Some(Self::default());
        };

        let mut unknown: Vec<&str> = table
            .keys()
            .map(String::as_str)
            .filter(|k| !KNOWN_KEYS.contains(k))
            .collect();
        if !unknown.is_empty() {
            unknown.sort_unstable();
            warn(&format!(
                "[effects] unknown key(s) skipped: {}",
                unknown.join(", ")
            ));
        }

        Some(Self {
            scanlines: table.get("scanlines").and_then(decode_scanlines),
            vignette: table.get("vignette").and_then(decode_vignette),
            glow: table.get("glow").and_then(decode_glow),
            bezel: table.get("bezel").and_then(decode_bezel),
        })
    }
}

/// The outcome of reading one subfield out of a sub-table: absent (use the
/// default), present and well-typed, or present with the wrong TOML type
/// (invalidates the whole containing key).
enum Field<T> {
    Absent,
    Valid(T),
    Invalid,
}

fn num_field(table: &toml::Table, key: &str) -> Field<f64> {
    match table.get(key) {
        None => Field::Absent,
        Some(v) => match v.as_float().or_else(|| v.as_integer().map(|i| i as f64)) {
            Some(f) => Field::Valid(f),
            None => Field::Invalid,
        },
    }
}

fn bool_field(table: &toml::Table, key: &str) -> Field<bool> {
    match table.get(key) {
        None => Field::Absent,
        Some(v) => match v.as_bool() {
            Some(b) => Field::Valid(b),
            None => Field::Invalid,
        },
    }
}

fn clamp_f32(v: f64, range: (f32, f32)) -> f32 {
    #[allow(clippy::cast_possible_truncation)]
    let v = v as f32;
    v.clamp(range.0, range.1)
}

fn clamp_u8(v: f64, range: (u8, u8)) -> u8 {
    let clamped = v.clamp(f64::from(range.0), f64::from(range.1)).round();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let clamped = clamped as u8;
    clamped
}

fn decode_scanlines(value: &toml::Value) -> Option<Scanlines> {
    let Some(t) = value.as_table() else {
        warn("[effects] key skipped: wrong shape: scanlines");
        return None;
    };
    let opacity = match num_field(t, "opacity") {
        Field::Absent => f64::from(DEFAULT_SCANLINES_OPACITY),
        Field::Valid(f) => f,
        Field::Invalid => {
            warn("[effects] key skipped: wrong shape: scanlines.opacity");
            return None;
        }
    };
    let spacing_px = match num_field(t, "spacing_px") {
        Field::Absent => f64::from(DEFAULT_SCANLINES_SPACING),
        Field::Valid(f) => f,
        Field::Invalid => {
            warn("[effects] key skipped: wrong shape: scanlines.spacing_px");
            return None;
        }
    };
    Some(Scanlines {
        opacity: clamp_f32(opacity, SCANLINES_OPACITY_RANGE),
        spacing_px: clamp_u8(spacing_px, SCANLINES_SPACING_RANGE),
    })
}

fn decode_vignette(value: &toml::Value) -> Option<Vignette> {
    let Some(t) = value.as_table() else {
        warn("[effects] key skipped: wrong shape: vignette");
        return None;
    };
    let strength = match num_field(t, "strength") {
        Field::Absent => f64::from(DEFAULT_VIGNETTE_STRENGTH),
        Field::Valid(f) => f,
        Field::Invalid => {
            warn("[effects] key skipped: wrong shape: vignette.strength");
            return None;
        }
    };
    Some(Vignette {
        strength: clamp_f32(strength, VIGNETTE_STRENGTH_RANGE),
    })
}

fn decode_glow(value: &toml::Value) -> Option<Glow> {
    let Some(t) = value.as_table() else {
        warn("[effects] key skipped: wrong shape: glow");
        return None;
    };
    let strength = match num_field(t, "strength") {
        Field::Absent => f64::from(DEFAULT_GLOW_STRENGTH),
        Field::Valid(f) => f,
        Field::Invalid => {
            warn("[effects] key skipped: wrong shape: glow.strength");
            return None;
        }
    };
    Some(Glow {
        strength: clamp_f32(strength, GLOW_STRENGTH_RANGE),
    })
}

fn decode_bezel(value: &toml::Value) -> Option<Bezel> {
    let Some(t) = value.as_table() else {
        warn("[effects] key skipped: wrong shape: bezel");
        return None;
    };
    let radius_px = match num_field(t, "radius_px") {
        Field::Absent => f64::from(DEFAULT_BEZEL_RADIUS),
        Field::Valid(f) => f,
        Field::Invalid => {
            warn("[effects] key skipped: wrong shape: bezel.radius_px");
            return None;
        }
    };
    let inset = match bool_field(t, "inset") {
        Field::Absent => DEFAULT_BEZEL_INSET,
        Field::Valid(b) => b,
        Field::Invalid => {
            warn("[effects] key skipped: wrong shape: bezel.inset");
            return None;
        }
    };
    Some(Bezel {
        radius_px: clamp_u8(radius_px, BEZEL_RADIUS_RANGE),
        inset,
    })
}

/// A malformed `[effects]` key is user-actionable (typo or unsupported shape
/// in a hand-edited or newer-schema theme), so — unlike this crate's
/// `NORTE_GUI_DEBUG`-gated diagnostics — this always goes to stderr. See the
/// [module docs](self) for why `norte-gui` uses `eprintln!` here instead of
/// `tracing`.
fn warn(msg: &str) {
    eprintln!("[norte-gui] {msg}");
}

#[cfg(test)]
mod tests {
    use super::EffectsV1;

    #[test]
    fn tema_sin_effects_es_none() {
        let t = norte_theme::Theme::preset_default();
        assert!(EffectsV1::from_theme(&t).is_none());
    }

    #[test]
    fn retro_crt_parsea_con_valores_del_preset() {
        let t = norte_theme::Theme::preset("retro-crt").unwrap().unwrap();
        let e = EffectsV1::from_theme(&t).expect("retro trae effects");
        let s = e.scanlines.expect("scanlines");
        assert!((s.opacity - 0.12).abs() < f32::EPSILON);
        assert_eq!(s.spacing_px, 3);
        assert!(e.vignette.is_some() && e.glow.is_some() && e.bezel.is_some());
    }

    /// Clamps: fuera de rango SATURA, jamás error (ADR 0036).
    #[test]
    fn valores_fuera_de_rango_se_clampan() {
        let t = norte_theme::Theme::from_toml(
            "[effects]\nscanlines = { opacity = 9.0, spacing_px = 1 }\nvignette = { strength = -3.0 }\n",
        )
        .unwrap();
        let e = EffectsV1::from_theme(&t).unwrap();
        let s = e.scanlines.unwrap();
        assert!(
            (s.opacity - 0.35).abs() < f32::EPSILON,
            "opacity clampa a 0.35"
        );
        assert_eq!(s.spacing_px, 2, "spacing clampa a 2");
        assert!(
            e.vignette.unwrap().strength.abs() < f32::EPSILON,
            "strength clampa a 0"
        );
    }

    /// Clave desconocida o tipo malo: WARN + skip de ESA clave, el resto vive.
    #[test]
    fn clave_rota_degrada_por_clave_no_todo() {
        let t = norte_theme::Theme::from_toml(
            "[effects]\nscanlines = \"muchas\"\nvignette = { strength = 0.2 }\nfuturo = { x = 1 }\n",
        )
        .unwrap();
        let e = EffectsV1::from_theme(&t).expect("vignette sobrevive");
        assert!(e.scanlines.is_none(), "scanlines mal tipado: skip");
        assert!(e.vignette.is_some(), "vignette válido: vive");
    }

    /// [effects] presente pero TODO degradado: Some(default) — modo effects
    /// "vacío", pinta nada (decisión documentada en from_theme).
    #[test]
    fn seccion_presente_todo_degradado_es_some_vacio() {
        let t = norte_theme::Theme::from_toml("[effects]\nscanlines = 3\n").unwrap();
        let e = EffectsV1::from_theme(&t).expect("Some aunque vacío");
        assert_eq!(e, EffectsV1::default());
    }
}
