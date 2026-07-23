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
    ///
    /// Alongside the parsed value, returns the list of warnings produced
    /// while interpreting the section: one entry per degraded key, each
    /// naming the key and a short reason (never the malformed value itself
    /// — see the [module docs](self)). Empty when `[effects]` is absent or
    /// every present key was well-formed. Callers that only need the parsed
    /// value can ignore the second element; `norte-gui`'s startup path
    /// forwards each entry to the localized startup banner (`main.rs`) so a
    /// theme problem is visible even when stderr is not (e.g. launched from
    /// a desktop entry).
    #[must_use]
    pub fn from_theme(theme: &norte_theme::Theme) -> (Option<Self>, Vec<String>) {
        let mut warnings = Vec::new();

        let Some(value) = theme.effects.as_ref() else {
            return (None, warnings);
        };

        let Some(table) = value.as_table() else {
            record(&mut warnings, "effects", "not a table");
            return (Some(Self::default()), warnings);
        };

        let mut unknown: Vec<&str> = table
            .keys()
            .map(String::as_str)
            .filter(|k| !KNOWN_KEYS.contains(k))
            .collect();
        unknown.sort_unstable();
        for key in unknown {
            record(&mut warnings, key, "unknown key");
        }

        let effects = Self {
            scanlines: table
                .get("scanlines")
                .and_then(|v| decode_scanlines(v, &mut warnings)),
            vignette: table
                .get("vignette")
                .and_then(|v| decode_vignette(v, &mut warnings)),
            glow: table
                .get("glow")
                .and_then(|v| decode_glow(v, &mut warnings)),
            bezel: table
                .get("bezel")
                .and_then(|v| decode_bezel(v, &mut warnings)),
        };

        (Some(effects), warnings)
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
        Some(v) => match v
            .as_float()
            .or_else(|| v.as_integer().map(|i| i as f64))
            .filter(|f| f.is_finite())
        {
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

fn decode_scanlines(value: &toml::Value, warnings: &mut Vec<String>) -> Option<Scanlines> {
    let Some(t) = value.as_table() else {
        record(warnings, "scanlines", "wrong shape");
        return None;
    };
    let opacity = match num_field(t, "opacity") {
        Field::Absent => f64::from(DEFAULT_SCANLINES_OPACITY),
        Field::Valid(f) => f,
        Field::Invalid => {
            record(warnings, "scanlines.opacity", "wrong shape");
            return None;
        }
    };
    let spacing_px = match num_field(t, "spacing_px") {
        Field::Absent => f64::from(DEFAULT_SCANLINES_SPACING),
        Field::Valid(f) => f,
        Field::Invalid => {
            record(warnings, "scanlines.spacing_px", "wrong shape");
            return None;
        }
    };
    Some(Scanlines {
        opacity: clamp_f32(opacity, SCANLINES_OPACITY_RANGE),
        spacing_px: clamp_u8(spacing_px, SCANLINES_SPACING_RANGE),
    })
}

fn decode_vignette(value: &toml::Value, warnings: &mut Vec<String>) -> Option<Vignette> {
    let Some(t) = value.as_table() else {
        record(warnings, "vignette", "wrong shape");
        return None;
    };
    let strength = match num_field(t, "strength") {
        Field::Absent => f64::from(DEFAULT_VIGNETTE_STRENGTH),
        Field::Valid(f) => f,
        Field::Invalid => {
            record(warnings, "vignette.strength", "wrong shape");
            return None;
        }
    };
    Some(Vignette {
        strength: clamp_f32(strength, VIGNETTE_STRENGTH_RANGE),
    })
}

fn decode_glow(value: &toml::Value, warnings: &mut Vec<String>) -> Option<Glow> {
    let Some(t) = value.as_table() else {
        record(warnings, "glow", "wrong shape");
        return None;
    };
    let strength = match num_field(t, "strength") {
        Field::Absent => f64::from(DEFAULT_GLOW_STRENGTH),
        Field::Valid(f) => f,
        Field::Invalid => {
            record(warnings, "glow.strength", "wrong shape");
            return None;
        }
    };
    Some(Glow {
        strength: clamp_f32(strength, GLOW_STRENGTH_RANGE),
    })
}

fn decode_bezel(value: &toml::Value, warnings: &mut Vec<String>) -> Option<Bezel> {
    let Some(t) = value.as_table() else {
        record(warnings, "bezel", "wrong shape");
        return None;
    };
    let radius_px = match num_field(t, "radius_px") {
        Field::Absent => f64::from(DEFAULT_BEZEL_RADIUS),
        Field::Valid(f) => f,
        Field::Invalid => {
            record(warnings, "bezel.radius_px", "wrong shape");
            return None;
        }
    };
    let inset = match bool_field(t, "inset") {
        Field::Absent => DEFAULT_BEZEL_INSET,
        Field::Valid(b) => b,
        Field::Invalid => {
            record(warnings, "bezel.inset", "wrong shape");
            return None;
        }
    };
    Some(Bezel {
        radius_px: clamp_u8(radius_px, BEZEL_RADIUS_RANGE),
        inset,
    })
}

/// A malformed `[effects]` key is user-actionable (typo or unsupported shape
/// in a hand-edited or newer-schema theme). Two channels carry it: an
/// unconditional `eprintln!` (unlike this crate's `NORTE_GUI_DEBUG`-gated
/// diagnostics — see the [module docs](self) for why `norte-gui` uses
/// `eprintln!` here instead of `tracing`) for a terminal-launched norte, and
/// an entry appended to `warnings` for [`EffectsV1::from_theme`]'s caller to
/// route through the startup banner (`main.rs`) — the only channel visible
/// when norte is launched from a desktop entry with no attached terminal.
/// `reason` is always a short fixed phrase, never the malformed TOML value.
fn record(warnings: &mut Vec<String>, key: &str, reason: &str) {
    eprintln!("[norte-gui] [effects] key skipped: {reason}: {key}");
    warnings.push(format!("{key}: {reason}"));
}

#[cfg(test)]
mod tests {
    use super::{
        BEZEL_RADIUS_RANGE, EffectsV1, GLOW_STRENGTH_RANGE, SCANLINES_OPACITY_RANGE,
        SCANLINES_SPACING_RANGE, VIGNETTE_STRENGTH_RANGE, clamp_f32, clamp_u8,
    };

    #[test]
    fn tema_sin_effects_es_none() {
        let t = norte_theme::Theme::preset_default();
        let (e, warnings) = EffectsV1::from_theme(&t);
        assert!(e.is_none());
        assert!(warnings.is_empty(), "sin [effects], sin warnings");
    }

    #[test]
    fn retro_crt_parsea_con_valores_del_preset() {
        let t = norte_theme::Theme::preset("retro-crt").unwrap().unwrap();
        let (e, warnings) = EffectsV1::from_theme(&t);
        let e = e.expect("retro trae effects");
        let s = e.scanlines.expect("scanlines");
        assert!((s.opacity - 0.12).abs() < f32::EPSILON);
        assert_eq!(s.spacing_px, 3);
        assert!(e.vignette.is_some() && e.glow.is_some() && e.bezel.is_some());
        assert!(warnings.is_empty(), "preset bien formado, sin warnings");
    }

    /// Clamps: fuera de rango SATURA, jamás error (ADR 0036) — y saturar NO
    /// es un warning (distinto de un tipo/forma inválidos).
    #[test]
    fn valores_fuera_de_rango_se_clampan() {
        let t = norte_theme::Theme::from_toml(
            "[effects]\nscanlines = { opacity = 9.0, spacing_px = 1 }\nvignette = { strength = -3.0 }\n",
        )
        .unwrap();
        let (e, warnings) = EffectsV1::from_theme(&t);
        let e = e.unwrap();
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
        assert!(warnings.is_empty(), "clamp silencioso, no es warning");
    }

    /// Clave desconocida o tipo malo: WARN + skip de ESA clave, el resto vive.
    #[test]
    fn clave_rota_degrada_por_clave_no_todo() {
        let t = norte_theme::Theme::from_toml(
            "[effects]\nscanlines = \"muchas\"\nvignette = { strength = 0.2 }\nfuturo = { x = 1 }\n",
        )
        .unwrap();
        let (e, warnings) = EffectsV1::from_theme(&t);
        let e = e.expect("vignette sobrevive");
        assert!(e.scanlines.is_none(), "scanlines mal tipado: skip");
        assert!(e.vignette.is_some(), "vignette válido: vive");
        assert!(
            warnings.iter().any(|w| w.starts_with("scanlines")),
            "el warning nombra la clave scanlines: {warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.starts_with("futuro")),
            "el warning nombra la clave desconocida futuro: {warnings:?}"
        );
        assert!(
            warnings.iter().all(|w| !w.contains("muchas")),
            "el warning NUNCA lleva el valor malformado: {warnings:?}"
        );
    }

    /// [effects] presente pero TODO degradado: Some(default) — modo effects
    /// "vacío", pinta nada (decisión documentada en from_theme).
    #[test]
    fn seccion_presente_todo_degradado_es_some_vacio() {
        let t = norte_theme::Theme::from_toml("[effects]\nscanlines = 3\n").unwrap();
        let (e, warnings) = EffectsV1::from_theme(&t);
        let e = e.expect("Some aunque vacío");
        assert_eq!(e, EffectsV1::default());
        assert!(!warnings.is_empty(), "el degrade se reporta igual");
    }

    /// BLOCKER de review: `nan`/`inf` en TOML no deben colar por los clamps
    /// (`f64::clamp` propaga NaN, `NaN as u8` satura a 0 — por debajo del
    /// piso `[2, 16]` de `spacing_px`, lo que colgaría `paint_scanlines` en
    /// `main.rs`: `while y < bottom { ... y += px(0) }` nunca termina). Un
    /// valor no finito debe tomar la rama `Invalid` de `num_field`, igual
    /// que un tipo incorrecto: la clave se descarta entera, sin panic.
    #[test]
    fn nan_no_rompe_el_clamp() {
        let t = norte_theme::Theme::from_toml(
            "[effects]\nscanlines = { opacity = nan, spacing_px = nan }\nvignette = { strength = inf }\n",
        )
        .unwrap();
        let (e, warnings) = EffectsV1::from_theme(&t);
        let e = e.expect("Some aunque degradado: [effects] SÍ estaba presente");
        assert!(e.scanlines.is_none(), "opacity/spacing_px NaN: no cuela");
        assert!(e.vignette.is_none(), "strength inf: no cuela");
        assert!(
            warnings.iter().any(|w| w.starts_with("scanlines.opacity")),
            "warning nombra scanlines.opacity: {warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.starts_with("vignette.strength")),
            "warning nombra vignette.strength: {warnings:?}"
        );
    }

    /// Los helpers de clamp jamás emiten fuera de rango para entradas
    /// finitas, por extremas que sean (defensa directa, sin pasar por TOML).
    #[test]
    fn clamp_helpers_finito_extremo_nunca_sale_de_rango() {
        for v in [f64::MAX, f64::MIN, -0.0, 0.0] {
            let o = clamp_f32(v, SCANLINES_OPACITY_RANGE);
            assert!(
                (SCANLINES_OPACITY_RANGE.0..=SCANLINES_OPACITY_RANGE.1).contains(&o),
                "opacity fuera de rango para v={v}: {o}"
            );
            let vg = clamp_f32(v, VIGNETTE_STRENGTH_RANGE);
            assert!(
                (VIGNETTE_STRENGTH_RANGE.0..=VIGNETTE_STRENGTH_RANGE.1).contains(&vg),
                "vignette fuera de rango para v={v}: {vg}"
            );
            let gl = clamp_f32(v, GLOW_STRENGTH_RANGE);
            assert!(
                (GLOW_STRENGTH_RANGE.0..=GLOW_STRENGTH_RANGE.1).contains(&gl),
                "glow fuera de rango para v={v}: {gl}"
            );
            let sp = clamp_u8(v, SCANLINES_SPACING_RANGE);
            assert!(
                (SCANLINES_SPACING_RANGE.0..=SCANLINES_SPACING_RANGE.1).contains(&sp),
                "spacing fuera de rango para v={v}: {sp}"
            );
            let br = clamp_u8(v, BEZEL_RADIUS_RANGE);
            assert!(
                (BEZEL_RADIUS_RANGE.0..=BEZEL_RADIUS_RANGE.1).contains(&br),
                "bezel radius fuera de rango para v={v}: {br}"
            );
        }
    }
}
