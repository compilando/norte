//! Interpreter for a theme's `[effects]` section (schema v1.1, ADR 0036 +
//! its G2 amendment).
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
//!   absent one (all seven fields [`None`]), but the distinction matters for
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
//! | `flicker.strength`       | `0.05`  |
//!
//! `cursor_blink` and `fade_ms` are bare scalars directly under `[effects]`,
//! not subfields of an always-present sub-table (unlike `flicker.strength`
//! above) — there is no "table present but key absent" case for them, so no
//! default substitution applies: absent means [`None`], same as any other
//! top-level key.
//!
//! # Clamp ranges (ADR 0036 §2, extended §2 amendment for v1.1)
//!
//! | Key                     | Range          |
//! | ------------------------ | -------------- |
//! | `scanlines.opacity`      | `[0.0, 0.35]`  |
//! | `scanlines.spacing_px`   | `[2, 16]`      |
//! | `vignette.strength`      | `[0.0, 0.6]`   |
//! | `glow.strength`          | `[0.0, 1.0]`   |
//! | `bezel.radius_px`        | `[0, 32]`      |
//! | `flicker.strength`       | `[0.0, 0.15]`  |
//! | `fade_ms`                | `[0, 400]`     |
//!
//! `bezel.inset` and `cursor_blink` are bools: no clamp applies.
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
/// Schema v1.1 (G2). Deliberately tiny: an a11y guard against
/// photosensitive-trigger risk, same reasoning class as
/// `SCANLINES_OPACITY_RANGE`'s AA-contrast cap.
const FLICKER_STRENGTH_RANGE: (f32, f32) = (0.0, 0.15);
/// Schema v1.1 (G2). Milliseconds; parsed only — the GUI does not yet
/// animate fades (plan G2 decision 1).
const FADE_MS_RANGE: (u16, u16) = (0, 400);

const DEFAULT_SCANLINES_OPACITY: f32 = 0.1;
const DEFAULT_SCANLINES_SPACING: u8 = 3;
const DEFAULT_VIGNETTE_STRENGTH: f32 = 0.3;
const DEFAULT_GLOW_STRENGTH: f32 = 0.4;
const DEFAULT_BEZEL_RADIUS: u8 = 10;
const DEFAULT_BEZEL_INSET: bool = false;
/// Schema v1.1 (G2). Used only when `[effects.flicker]` is present but
/// `strength` is absent — `cursor_blink`/`fade_ms` are bare scalars with no
/// equivalent "table present, subfield absent" case (see module docs).
const DEFAULT_FLICKER_STRENGTH: f32 = 0.05;

const KNOWN_KEYS: [&str; 7] = [
    "scanlines",
    "vignette",
    "glow",
    "bezel",
    "flicker",
    "cursor_blink",
    "fade_ms",
];

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

/// Subtle CRT flicker: `[effects.flicker]` (schema v1.1, phase G2, ADR 0036
/// amendment). See the [module docs](self) for the default and clamp range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Flicker {
    /// Flicker amplitude, clamped to `[0.0, 0.15]` — deliberately tiny, an
    /// accessibility guard against photosensitive-trigger risk (same
    /// reasoning class as `scanlines.opacity`'s AA-contrast cap).
    pub strength: f32,
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
    /// `[effects.flicker]`, if present and well-shaped (schema v1.1, phase
    /// G2, ADR 0036 amendment).
    pub flicker: Option<Flicker>,
    /// `[effects] cursor_blink`, if present and a `bool` (schema v1.1, phase
    /// G2). A bare scalar, not a sub-table — a wrong TOML type degrades this
    /// key alone, same per-key contract as every other key.
    pub cursor_blink: Option<bool>,
    /// `[effects] fade_ms`, if present and well-shaped, clamped to
    /// `[0, 400]` milliseconds (schema v1.1, phase G2). The field is parsed;
    /// rendering lands with the fade work (plan G2 decision 1) — v1.1 ships
    /// the schema, not yet the animation.
    pub fade_ms: Option<u16>,
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
            flicker: table
                .get("flicker")
                .and_then(|v| decode_flicker(v, &mut warnings)),
            cursor_blink: match bool_field(table, "cursor_blink") {
                Field::Absent => None,
                Field::Valid(b) => Some(b),
                Field::Invalid => {
                    record(&mut warnings, "cursor_blink", "wrong shape");
                    None
                }
            },
            fade_ms: match num_field(table, "fade_ms") {
                Field::Absent => None,
                Field::Valid(f) => Some(clamp_u16(f, FADE_MS_RANGE)),
                Field::Invalid => {
                    record(&mut warnings, "fade_ms", "wrong shape");
                    None
                }
            },
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

/// Schema v1.1 (G2): `fade_ms`'s range (`[0, 400]`) does not fit `u8`, so it
/// gets its own clamp helper — same rounding/truncation shape as
/// [`clamp_u8`].
fn clamp_u16(v: f64, range: (u16, u16)) -> u16 {
    let clamped = v.clamp(f64::from(range.0), f64::from(range.1)).round();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let clamped = clamped as u16;
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

/// Schema v1.1 (G2, ADR 0036 amendment). Same shape as [`decode_vignette`]:
/// one clamped `f32` subfield with a default.
fn decode_flicker(value: &toml::Value, warnings: &mut Vec<String>) -> Option<Flicker> {
    let Some(t) = value.as_table() else {
        record(warnings, "flicker", "wrong shape");
        return None;
    };
    let strength = match num_field(t, "strength") {
        Field::Absent => f64::from(DEFAULT_FLICKER_STRENGTH),
        Field::Valid(f) => f,
        Field::Invalid => {
            record(warnings, "flicker.strength", "wrong shape");
            return None;
        }
    };
    Some(Flicker {
        strength: clamp_f32(strength, FLICKER_STRENGTH_RANGE),
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

    // --- schema v1.1 (G2 motion, ADR 0036 amendment) -----------------------

    /// `flicker.strength` parsea y clampa a `[0.0, 0.15]`, mismo patrón que
    /// `vignette.strength`.
    #[test]
    fn flicker_parsea_y_clampa() {
        let t =
            norte_theme::Theme::from_toml("[effects]\nflicker = { strength = 0.08 }\n").unwrap();
        let (e, warnings) = EffectsV1::from_theme(&t);
        let e = e.unwrap();
        let f = e.flicker.expect("flicker presente");
        assert!((f.strength - 0.08).abs() < f32::EPSILON);
        assert!(warnings.is_empty());

        let t = norte_theme::Theme::from_toml("[effects]\nflicker = { strength = 9.0 }\n").unwrap();
        let (e, warnings) = EffectsV1::from_theme(&t);
        let e = e.unwrap();
        assert!(
            (e.flicker.unwrap().strength - 0.15).abs() < f32::EPSILON,
            "clampa al techo 0.15"
        );
        assert!(warnings.is_empty(), "clamp silencioso, no es warning");
    }

    /// `flicker` presente sin `strength`: usa el default documentado (0.05),
    /// clampado igual que cualquier otro valor (mismo patrón que
    /// `scanlines.opacity`).
    #[test]
    fn flicker_sin_strength_usa_default() {
        let t = norte_theme::Theme::from_toml("[effects]\nflicker = {}\n").unwrap();
        let (e, warnings) = EffectsV1::from_theme(&t);
        let e = e.unwrap();
        assert!((e.flicker.unwrap().strength - 0.05).abs() < f32::EPSILON);
        assert!(warnings.is_empty());
    }

    /// `cursor_blink` es un bool suelto en `[effects]` (no una subtabla):
    /// parsea directo.
    #[test]
    fn cursor_blink_bool_parsea() {
        let t = norte_theme::Theme::from_toml("[effects]\ncursor_blink = true\n").unwrap();
        let (e, warnings) = EffectsV1::from_theme(&t);
        assert_eq!(e.unwrap().cursor_blink, Some(true));
        assert!(warnings.is_empty());

        let t = norte_theme::Theme::from_toml("[effects]\ncursor_blink = false\n").unwrap();
        let (e, _) = EffectsV1::from_theme(&t);
        assert_eq!(e.unwrap().cursor_blink, Some(false));
    }

    /// Tipo incorrecto (string en vez de bool): degrada SOLO esa clave, con
    /// warning — mismo contrato per-key que el resto del módulo.
    #[test]
    fn cursor_blink_tipo_incorrecto_degrada() {
        let t = norte_theme::Theme::from_toml("[effects]\ncursor_blink = \"yes\"\n").unwrap();
        let (e, warnings) = EffectsV1::from_theme(&t);
        let e = e.unwrap();
        assert!(e.cursor_blink.is_none());
        assert!(
            warnings.iter().any(|w| w.starts_with("cursor_blink")),
            "{warnings:?}"
        );
    }

    /// `fade_ms` parsea y clampa a `[0, 400]`. Solo el campo — el renderer
    /// aún no anima el fade (plan G2 decisión 1).
    #[test]
    fn fade_ms_parsea_y_clampa() {
        let t = norte_theme::Theme::from_toml("[effects]\nfade_ms = 200\n").unwrap();
        let (e, warnings) = EffectsV1::from_theme(&t);
        assert_eq!(e.unwrap().fade_ms, Some(200));
        assert!(warnings.is_empty());

        let t = norte_theme::Theme::from_toml("[effects]\nfade_ms = 9999\n").unwrap();
        let (e, warnings) = EffectsV1::from_theme(&t);
        assert_eq!(e.unwrap().fade_ms, Some(400), "clampa al techo 400");
        assert!(warnings.is_empty());

        let t = norte_theme::Theme::from_toml("[effects]\nfade_ms = -50\n").unwrap();
        let (e, warnings) = EffectsV1::from_theme(&t);
        assert_eq!(e.unwrap().fade_ms, Some(0), "clampa al piso 0");
        assert!(warnings.is_empty());
    }

    /// Tipo incorrecto en `fade_ms`: degrada SOLO esa clave, con warning.
    #[test]
    fn fade_ms_tipo_incorrecto_degrada() {
        let t = norte_theme::Theme::from_toml("[effects]\nfade_ms = \"pronto\"\n").unwrap();
        let (e, warnings) = EffectsV1::from_theme(&t);
        let e = e.unwrap();
        assert!(e.fade_ms.is_none());
        assert!(
            warnings.iter().any(|w| w.starts_with("fade_ms")),
            "{warnings:?}"
        );
    }

    /// Extiende el test BLOCKER existente: `nan`/`inf` en las claves v1.1
    /// tampoco cuelan por el clamp (mismo bug de fondo que `scanlines`).
    #[test]
    fn nan_no_rompe_el_clamp_de_las_claves_v1_1() {
        let t = norte_theme::Theme::from_toml(
            "[effects]\nflicker = { strength = nan }\nfade_ms = nan\n",
        )
        .unwrap();
        let (e, warnings) = EffectsV1::from_theme(&t);
        let e = e.expect("Some aunque degradado");
        assert!(e.flicker.is_none(), "strength NaN: no cuela");
        assert!(e.fade_ms.is_none(), "fade_ms NaN: no cuela");
        assert!(
            warnings.iter().any(|w| w.starts_with("flicker.strength")),
            "{warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.starts_with("fade_ms")),
            "{warnings:?}"
        );
    }

    /// Las 3 claves nuevas conviven con las 4 de v1 sin pisarse, y una clave
    /// realmente desconocida sigue avisando igual (KNOWN_KEYS 4→7 no rompe
    /// el warning de "unknown key").
    #[test]
    fn claves_v1_1_conviven_y_desconocida_sigue_avisando() {
        let t = norte_theme::Theme::from_toml(
            "[effects]\nscanlines = { opacity = 0.1 }\nflicker = { strength = 0.05 }\n\
             cursor_blink = true\nfade_ms = 120\nfuturo = 1\n",
        )
        .unwrap();
        let (e, warnings) = EffectsV1::from_theme(&t);
        let e = e.unwrap();
        assert!(e.scanlines.is_some());
        assert!(e.flicker.is_some());
        assert_eq!(e.cursor_blink, Some(true));
        assert_eq!(e.fade_ms, Some(120));
        assert!(warnings.iter().any(|w| w.starts_with("futuro")));
        assert_eq!(
            warnings.len(),
            1,
            "solo 'futuro' es realmente desconocida: {warnings:?}"
        );
    }

    /// El preset retro-crt (editado en G2 decisión 5) trae flicker+blink.
    #[test]
    fn retro_crt_preset_trae_flicker_y_cursor_blink() {
        let t = norte_theme::Theme::preset("retro-crt").unwrap().unwrap();
        let (e, warnings) = EffectsV1::from_theme(&t);
        let e = e.expect("retro trae effects");
        assert!((e.flicker.expect("flicker").strength - 0.05).abs() < f32::EPSILON);
        assert_eq!(e.cursor_blink, Some(true));
        assert!(warnings.is_empty(), "preset bien formado, sin warnings");
    }
}
