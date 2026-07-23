//! `[archive]` limits for processes without the TUI loader (#95), now via
//! the shared layered loader (ADR 0035): System+User layers apply, the
//! Project layer never does (a foreign repo must not raise security limits).

use norte_vfs_archive::Limits;

/// Merged `[archive]` overrides from the given layers. `None` = no
/// overrides anywhere (use compiled defaults).
///
/// # Errors
/// Any layer that exists but does not parse strictly — fail-loud: an
/// operator who LOWERED limits for agents must not stay at 64 GiB over a
/// silent typo.
pub fn load_archive_limits_from(layers: &norte_config::Layers) -> std::io::Result<Option<Limits>> {
    let cfg = norte_config::load(layers)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if cfg.archive_max_entries.is_none()
        && cfg.archive_max_decompressed_bytes.is_none()
        && cfg.archive_max_nesting.is_none()
    {
        return Ok(None);
    }
    let mut limits = Limits::default();
    if let Some(n) = cfg.archive_max_entries {
        // Saturate UPWARD (only possible on 32-bit with a value > u32::MAX):
        // never shrink a limit by wrap, and not silently.
        limits.max_entries = usize::try_from(n).unwrap_or_else(|_| {
            tracing::warn!(
                n,
                "[archive] max_entries saturates to usize::MAX on this platform"
            );
            usize::MAX
        });
    }
    if let Some(b) = cfg.archive_max_decompressed_bytes {
        limits.max_decompressed_bytes = b;
    }
    if let Some(n) = cfg.archive_max_nesting {
        limits.max_nesting = n;
    }
    Ok(Some(limits))
}

/// Layered load from the standard layers. SYNC (startup): wrap in
/// `spawn_blocking` from async contexts.
///
/// # Errors
/// Those of [`load_archive_limits_from`].
pub fn load_archive_limits() -> std::io::Result<Option<Limits>> {
    load_archive_limits_from(&norte_config::standard_layers())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sin_overrides_en_ninguna_capa_es_none() {
        let dir = tempfile::tempdir().unwrap();
        // Sección conocida (`deny_unknown_fields` rechaza secciones
        // desconocidas también, no solo campos) sin nada de `[archive]`.
        std::fs::write(dir.path().join("norte.toml"), "[ui]\nlang = \"en\"\n").unwrap();
        let layers = norte_config::Layers {
            dirs: vec![(dir.path().to_path_buf(), norte_config::Layer::User)],
        };
        assert!(load_archive_limits_from(&layers).expect("carga").is_none());

        let vacio = norte_config::Layers { dirs: vec![] };
        assert!(
            load_archive_limits_from(&vacio)
                .expect("sin capas")
                .is_none()
        );
    }

    #[test]
    fn overrides_se_aplican_sobre_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "[archive]\nmax_entries = 100\n",
        )
        .unwrap();
        let layers = norte_config::Layers {
            dirs: vec![(dir.path().to_path_buf(), norte_config::Layer::User)],
        };
        let l = load_archive_limits_from(&layers)
            .expect("parsea")
            .expect("hay overrides");
        assert_eq!(l.max_entries, 100);
        assert_eq!(
            l.max_decompressed_bytes,
            Limits::default().max_decompressed_bytes,
            "el campo ausente conserva el default"
        );

        let dir2 = tempfile::tempdir().unwrap();
        std::fs::write(
            dir2.path().join("norte.toml"),
            "[archive]\nmax_decompressed_bytes = 1024\n",
        )
        .unwrap();
        let layers2 = norte_config::Layers {
            dirs: vec![(dir2.path().to_path_buf(), norte_config::Layer::User)],
        };
        let l = load_archive_limits_from(&layers2)
            .expect("parsea")
            .expect("hay overrides");
        assert_eq!(l.max_decompressed_bytes, 1024);
    }

    /// C1 exit criterion: a system-layer `[archive]` binds the daemon path
    /// exactly like it binds the TUI (spec gap 3).
    #[test]
    fn capa_sistema_tambien_aplica() {
        let sistema = tempfile::tempdir().unwrap();
        std::fs::write(
            sistema.path().join("norte.toml"),
            "[archive]\nmax_entries = 7\n",
        )
        .unwrap();
        let layers = norte_config::Layers {
            dirs: vec![(sistema.path().to_path_buf(), norte_config::Layer::System)],
        };
        let l = load_archive_limits_from(&layers)
            .expect("carga")
            .expect("overrides");
        assert_eq!(l.max_entries, 7);
    }

    /// Uniform strictness (ADR 0035 decision 4): a `[ui]` typo now fails
    /// the daemon load too — no more silent divergence from the TUI.
    #[test]
    fn typo_en_otra_seccion_es_error_tambien_para_el_daemon() {
        let user = tempfile::tempdir().unwrap();
        std::fs::write(user.path().join("norte.toml"), "[ui]\ntheem = \"nord\"\n").unwrap();
        let layers = norte_config::Layers {
            dirs: vec![(user.path().to_path_buf(), norte_config::Layer::User)],
        };
        assert!(load_archive_limits_from(&layers).is_err());
    }

    /// Boundary pin: broken TOML syntax in a layer is a hard error, not a
    /// silent `None` (fail-loud carries over from the pre-migration parser).
    #[test]
    fn toml_roto_es_error_fail_loud() {
        let user = tempfile::tempdir().unwrap();
        std::fs::write(user.path().join("norte.toml"), "[archive\n").unwrap();
        let layers = norte_config::Layers {
            dirs: vec![(user.path().to_path_buf(), norte_config::Layer::User)],
        };
        assert!(load_archive_limits_from(&layers).is_err());
    }

    /// Boundary pin: a field with the wrong TOML type is a hard error, not a
    /// silently-ignored override.
    #[test]
    fn tipo_malo_es_error_fail_loud() {
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("norte.toml"),
            "[archive]\nmax_entries = \"muchas\"\n",
        )
        .unwrap();
        let layers = norte_config::Layers {
            dirs: vec![(user.path().to_path_buf(), norte_config::Layer::User)],
        };
        assert!(load_archive_limits_from(&layers).is_err());
    }

    /// Boundary pin: an empty `[archive]` section (present, no fields) is
    /// still `Ok(None)` — presence of the section alone is not an override.
    #[test]
    fn seccion_archive_vacia_es_none() {
        let user = tempfile::tempdir().unwrap();
        std::fs::write(user.path().join("norte.toml"), "[archive]\n").unwrap();
        let layers = norte_config::Layers {
            dirs: vec![(user.path().to_path_buf(), norte_config::Layer::User)],
        };
        assert!(load_archive_limits_from(&layers).expect("carga").is_none());
    }
}
