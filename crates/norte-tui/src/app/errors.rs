//! Presenting an error in the status bar (#73): each family's Fluent
//! category (I/O, config, theme, keymaps) and the sanitizing of the detail
//! appended to it. Pure machinery: doesn't touch `App`.

use super::display_name;
use norte_i18n::{t, ta};
use norte_proto::Error;

/// The error-CATEGORY vocabulary lives in [`norte_frontend::error`] (#158,
/// phase C1 review): the GUI reports the same errors and couldn't reach it
/// here, so it interpolated the English `Display` into otherwise localized
/// sentences. Re-exported under its usual name because it's this crate's
/// public API (Lua scripts compare against these keys).
pub use norte_frontend::error::{error_category, error_key};

/// Status-bar message `error: <category>` (wraps [`error_category`]).
#[must_use]
pub fn error_message(e: &Error) -> String {
    ta("msg-error", &[("error", &error_category(e))])
}

/// Cap on the diagnostic detail in the status bar (one line; a hostile TOML
/// can quote mile-long values).
pub const DETAIL_MAX_CHARS: usize = 160;

/// Diagnostic detail ready for the status bar (#73): masked like a name
/// ([`display_name`]: lossy and marked, no controls/bidi/invisibles) and
/// capped at [`DETAIL_MAX_CHARS`] (truncation marked with `…`).
#[must_use]
pub fn detail_for_bar(detail: &str) -> String {
    let (masked, _) = display_name(detail.as_bytes());
    let mut out: String = masked.chars().take(DETAIL_MAX_CHARS).collect();
    if masked.chars().nth(DETAIL_MAX_CHARS).is_some() {
        out.push('…');
    }
    out
}

/// LOCALIZED category for a LOCAL io error (#73): `ErrorKind` → Fluent key —
/// never the OS's `Display`, which the OS localizes however it pleases
/// ("Permission denied (os error 13)"; rule 1).
#[must_use]
pub fn io_error_category(e: &std::io::Error) -> String {
    let key = match e.kind() {
        std::io::ErrorKind::NotFound => "err-not-found",
        std::io::ErrorKind::PermissionDenied => "err-permission-denied",
        std::io::ErrorKind::StorageFull => "err-no-space",
        _ => "err-io",
    };
    t(key)
}

/// LOCALIZED category for a [`crate::config::ConfigError`] (#73): its own
/// path (explicit lossy + mask) and, for the TOML case, the parser's
/// diagnostic sanitized by [`detail_for_bar`] — the position ("at line N")
/// is the actionable part. The underlying io goes through
/// [`io_error_category`].
#[must_use]
pub fn config_error_category(e: &crate::config::ConfigError) -> String {
    use crate::config::ConfigError;
    match e {
        ConfigError::Io { path, source } => ta(
            "err-config-io",
            &[
                ("path", &detail_for_bar(&path.display().to_string())),
                ("error", &io_error_category(source)),
            ],
        ),
        ConfigError::Toml { path, message } => ta(
            "err-config-parse",
            &[
                ("path", &detail_for_bar(&path.display().to_string())),
                ("detail", &detail_for_bar(message)),
            ],
        ),
    }
}

/// LOCALIZED category for a [`crate::theme::ResolveError`] (#73), a mirror of
/// [`config_error_category`]. `spec` can come from a FOREIGN repo's
/// `./.norte` layer: always goes through [`detail_for_bar`].
#[must_use]
pub fn theme_error_category(e: &crate::theme::ResolveError) -> String {
    use crate::theme::ResolveError;
    match e {
        ResolveError::Io { spec, source } => ta(
            "err-config-io",
            &[
                ("path", &detail_for_bar(spec)),
                ("error", &io_error_category(source)),
            ],
        ),
        ResolveError::Parse { spec, detail } => ta(
            "err-config-parse",
            &[
                ("path", &detail_for_bar(spec)),
                ("detail", &detail_for_bar(detail)),
            ],
        ),
    }
}

/// Typed error for mounting keymaps (#73): each variant maps to a Fluent key
/// in [`keymaps_error_category`] — no hardcoded Spanish anyhow contexts on
/// the status bar. The (thiserror) `Display` only goes out via stderr at
/// startup, before the TUI comes up.
#[derive(Debug, thiserror::Error)]
pub enum KeymapsError {
    /// The requested preset (CLI or config) doesn't exist.
    #[error("unknown preset {name:?}; available: {available}")]
    UnknownPreset {
        /// What was requested.
        name: String,
        /// The ones that do exist, already joined for display.
        available: String,
    },
    /// A keymap layer doesn't validate against the commands.
    #[error("invalid keymap: {detail}")]
    Invalid {
        /// The validator's diagnostic ([`crate::keymap::KeymapError`]).
        detail: String,
    },
}

/// LOCALIZED category for a [`KeymapsError`] (#73).
#[must_use]
pub fn keymaps_error_category(e: &KeymapsError) -> String {
    match e {
        KeymapsError::UnknownPreset { name, available } => ta(
            "err-keymap-preset-unknown",
            &[("name", &detail_for_bar(name)), ("available", available)],
        ),
        KeymapsError::Invalid { detail } => {
            ta("err-keymap-invalid", &[("detail", &detail_for_bar(detail))])
        }
    }
}

#[cfg(test)]
mod error_message_tests {
    use super::{
        DETAIL_MAX_CHARS, KeymapsError, config_error_category, detail_for_bar, error_message,
        io_error_category, keymaps_error_category, theme_error_category,
    };
    use crate::config;
    use norte_proto::{ConflictKind, Error};

    /// Every category renders its OWN localized message — never the proto's
    /// hardcoded English `Display` (#20, spec §17.7).
    #[test]
    fn cada_categoria_tiene_mensaje_propio_no_display() {
        let _ = norte_i18n::force(norte_i18n::Lang::Es);
        let nf = error_message(&Error::NotFound);
        assert!(nf.contains("no encontrado"), "localized ES: {nf}");
        assert!(
            !nf.contains("not found"),
            "NOT the proto's English Display: {nf}"
        );
        // Conflict's variants are distinguished from each other.
        let exists = error_message(&Error::Conflict {
            conflict: ConflictKind::Exists,
        });
        let case = error_message(&Error::Conflict {
            conflict: ConflictKind::CaseCollision,
        });
        assert_ne!(exists, case, "each ConflictKind renders differently");
        // PolicyDenied never leaks the concrete rule (closed vocabulary).
        let pd = error_message(&Error::PolicyDenied {
            rule: "scope-expired".into(),
        });
        assert!(
            !pd.contains("scope-expired"),
            "the concrete rule is NOT shown: {pd}"
        );
    }

    /// An unknown future category falls back to `err-unknown`, never empty.
    #[test]
    fn categoria_desconocida_cae_a_unknown() {
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let u = error_message(&Error::Unknown);
        assert!(u.contains("unknown error"), "{u}");
    }

    /// `error_category` (the basis of ALL status-bar renders) never leaks a
    /// `HostKeyUnknown`'s host — a hostile host with a bidi override would
    /// spoof the status bar — nor a `PolicyDenied`'s `rule`.
    #[test]
    fn categoria_no_filtra_host_hostil_ni_rule() {
        use super::error_category;
        let hk = error_category(&Error::HostKeyUnknown {
            host: "evil\u{202E}host".into(),
            port: Some(22),
            algo: "ssh-ed25519".into(),
            fingerprint: "SHA256:AAAA".into(),
        });
        assert!(!hk.contains("evil"), "the host is NOT shown: {hk:?}");
        assert!(
            !hk.contains('\u{202E}'),
            "no bidi in the status bar: {hk:?}"
        );
        let pd = error_category(&Error::PolicyDenied {
            rule: "scope-expired".into(),
        });
        assert!(
            !pd.contains("scope-expired"),
            "the rule is NOT leaked: {pd}"
        );
    }

    /// The STABLE key (`error_key`, the one Lua scripts see) and the
    /// localized text (`error_category`) are the SAME map: zero
    /// duplication, zero drift between what a script compares and what the
    /// status bar paints.
    #[test]
    fn error_key_es_la_clave_estable_de_la_categoria() {
        use super::{error_category, error_key};
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        assert_eq!(error_key(&Error::NotFound), "err-not-found");
        assert_eq!(error_key(&Error::Cancelled), "err-cancelled");
        assert_eq!(error_key(&Error::Unknown), "err-unknown");
        // 0.36.0: the rename batch's two arms lived on a `_ =>
        // "err-unknown"`, so deleting them COMPILES and the suite would
        // still be green — with two errors whose rustdoc calls them
        // actionable falling into "unknown error", which is the opposite.
        // These asserts are the only thing that prevents it.
        assert_eq!(error_key(&Error::PlanStale), "err-plan-stale");
        assert_eq!(
            error_key(&Error::PlanNotExecutable),
            "err-plan-not-executable"
        );
        // The category is exactly t(key).
        assert_eq!(
            error_category(&Error::NotFound),
            norte_i18n::t("err-not-found")
        );
    }

    /// 0.40.0: the three overlap relations paint DIFFERENTLY, and none falls
    /// into "unknown error".
    ///
    /// Lives on the same `_ => "err-unknown"` as the two above, so deleting
    /// the arm compiles and leaves the reader with "unknown error" facing
    /// the one refusal in this family that's fixed by moving elsewhere —
    /// which is exactly what the variant exists to not be. And the three
    /// keys have to be three: `Same`'s actionable phrase ("choose another
    /// directory") isn't the other two's ("leave the tree containing the
    /// other one").
    #[test]
    fn cada_relacion_de_solape_tiene_su_propia_frase() {
        use super::{error_category, error_key};
        use norte_proto::RootOverlap;
        let mut seen = std::collections::BTreeSet::new();
        for relation in [
            RootOverlap::Same,
            RootOverlap::SourceInsideDest,
            RootOverlap::DestInsideSource,
        ] {
            let key = error_key(&Error::OverlappingRoots { relation });
            assert!(
                key.starts_with("err-overlapping-roots"),
                "{relation:?} → {key}"
            );
            assert!(seen.insert(key), "two relations share {key}");
            for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
                let text = norte_i18n::t_in(lang, key);
                assert_ne!(text, key, "{key} untranslated in {lang:?}");
                assert_ne!(
                    text,
                    norte_i18n::t_in(lang, "err-unknown"),
                    "{key} says the same as \"unknown error\""
                );
                assert_ne!(
                    text,
                    norte_i18n::t_in(lang, "err-internal"),
                    "{key} says the same as \"internal error\""
                );
            }
        }
        // And a relation from a newer protocol falls into the family's
        // GENERIC key, not into `err-unknown`: it's still an overlap.
        assert_eq!(
            error_key(&Error::OverlappingRoots {
                relation: RootOverlap::Unknown
            }),
            "err-overlapping-roots"
        );
        assert_eq!(
            error_category(&Error::OverlappingRoots {
                relation: RootOverlap::Same
            }),
            norte_i18n::t("err-overlapping-roots-same")
        );
    }

    /// #73: a LOCAL io error goes by Fluent category — never the OS's
    /// `Display` ("Permission denied (os error 13)", which the OS localizes
    /// however it pleases — rule 1).
    #[test]
    fn categoria_io_local_no_filtra_el_display_del_os() {
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let e = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let s = io_error_category(&e);
        assert!(s.contains("permission denied"), "{s}");
        assert!(!s.contains("os error"), "no OS string: {s}");
        let full = std::io::Error::from(std::io::ErrorKind::StorageFull);
        let s = io_error_category(&full);
        assert!(s.contains("no space"), "kind with its own key: {s}");
    }

    /// #73: `ConfigError` renders a localized category + path; the parser's
    /// diagnostic is kept (the position is the actionable part) but goes
    /// through `display_name` — never raw bidi/controls in the status bar —
    /// and capped.
    #[test]
    fn categoria_config_no_filtra_el_diagnostico_del_parser() {
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let e = config::ConfigError::Toml {
            path: "/etc/norte/config.toml".into(),
            message: "unknown field `colr\u{202E}` at line 3".into(),
        };
        let s = config_error_category(&e);
        assert!(s.contains("config.toml"), "the path IS shown: {s}");
        assert!(
            s.contains("line 3"),
            "the position is the actionable part: {s}"
        );
        assert!(!s.contains('\u{202E}'), "no bidi in the status bar: {s}");
        let e = config::ConfigError::Io {
            path: "/etc/norte/config.toml".into(),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        let s = config_error_category(&e);
        assert!(s.contains("permission denied"), "io by category: {s}");
        assert!(!s.contains("os error"), "no OS string: {s}");
    }

    /// #73 (encoding-auditor's HIGH-1): the THEME pipeline had the same bug —
    /// a hostile spec (can come from a FOREIGN repo's `./.norte`) and the
    /// OS's Display, raw on the status bar via `apply_theme`.
    #[test]
    fn categoria_tema_no_filtra_spec_hostil_ni_os() {
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let e = crate::theme::ResolveError::Io {
            spec: "themes/\u{202E}x.toml".into(),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        };
        let s = theme_error_category(&e);
        assert!(!s.contains("os error"), "no OS string: {s}");
        assert!(!s.contains('\u{202E}'), "no bidi in the status bar: {s}");
        assert!(s.contains("not found"), "io by category: {s}");
        let e = crate::theme::ResolveError::Parse {
            spec: "nord".into(),
            detail: "unknown role `panel\u{202E}`".into(),
        };
        let s = theme_error_category(&e);
        assert!(s.contains("nord"), "the sanitized spec IS shown: {s}");
        assert!(!s.contains('\u{202E}'), "detail masked: {s}");
    }

    /// #73: a mile-long diagnostic (a hostile TOML can quote arbitrary
    /// values) comes out TRUNCATED — the status bar is one line.
    #[test]
    fn el_detalle_del_parser_tiene_tope() {
        let s = detail_for_bar(&"x".repeat(1000));
        assert!(s.chars().count() <= DETAIL_MAX_CHARS + 1, "{}", s.len());
        assert!(s.ends_with('…'), "truncation marked: {s}");
    }

    /// #73: the keymaps error is localized via Fluent (hardcoded Spanish
    /// anyhow contexts violated the i18n convention).
    #[test]
    fn error_de_keymap_se_localiza_con_el_nombre_del_preset() {
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let s = keymaps_error_category(&KeymapsError::UnknownPreset {
            name: "vintage".into(),
            available: "cua, orthodox".into(),
        });
        assert!(
            s.contains("vintage"),
            "the requested name is actionable: {s}"
        );
        assert!(s.contains("cua, orthodox"), "and the available ones: {s}");
        assert!(
            !s.contains("unknown preset {name"),
            "no raw thiserror Display leaking through: {s}"
        );
        let s = keymaps_error_category(&KeymapsError::Invalid {
            detail: "conflict on F5".into(),
        });
        assert!(s.contains("invalid keymap"), "localized: {s}");
        assert!(s.contains("F5"), "the diagnostic detail is kept: {s}");
    }
}
