//! `[archive]` limits for processes without the TUI loader (#95), now via
//! the shared layered loader (ADR 0035): System+User layers apply, the
//! Project layer never does (a foreign repo must not raise security limits).

use norte_vfs_archive::Limits;

/// Compiled-default [`Limits`] with the given overrides applied. `None` = no
/// override present for that field, keep the compiled default. Single home
/// of the override+saturation rule (rust review item 3, C1): it previously
/// existed twice, hand-duplicated between this function and the TUI's
/// `make_backend` — the TUI and the daemon path must never diverge on
/// anti-bomb limits.
///
/// Saturates `max_entries` UPWARD (only reachable on 32-bit with a value
/// greater than `u32::MAX`): a limit is never silently shrunk by wraparound.
#[must_use]
pub fn limits_from_overrides(
    max_entries: Option<u64>,
    max_decompressed_bytes: Option<u64>,
    max_nesting: Option<usize>,
) -> Option<Limits> {
    if max_entries.is_none() && max_decompressed_bytes.is_none() && max_nesting.is_none() {
        return None;
    }
    let mut limits = Limits::default();
    if let Some(n) = max_entries {
        limits.max_entries = usize::try_from(n).unwrap_or_else(|_| {
            tracing::warn!(
                n,
                "[archive] max_entries saturates to usize::MAX on this platform"
            );
            usize::MAX
        });
    }
    if let Some(b) = max_decompressed_bytes {
        limits.max_decompressed_bytes = b;
    }
    if let Some(n) = max_nesting {
        limits.max_nesting = n;
    }
    Some(limits)
}

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
    Ok(limits_from_overrides(
        cfg.archive.max_entries,
        cfg.archive.max_decompressed_bytes,
        cfg.archive.max_nesting,
    ))
}

/// The pinned RAR delegate (`[archive] rar_delegate`) from the given layers,
/// or `None` to probe `PATH` (roadmap item 11).
///
/// Same layer rule as the limits, and sharper here: the key names an
/// EXECUTABLE, so honouring it from a repository's `.norte.toml` would be
/// arbitrary code execution on `cd`. `norte-config::load` already drops it
/// from the Project layer; this loader never even reads that layer.
///
/// # Errors
/// Those of [`load_archive_limits_from`] — a config that exists but does not
/// parse aborts startup rather than silently falling back to `PATH`.
pub fn load_rar_delegate_from(
    layers: &norte_config::Layers,
) -> std::io::Result<Option<std::path::PathBuf>> {
    let cfg = norte_config::load(layers)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(cfg.archive.rar_delegate.map(std::path::PathBuf::from))
}

/// Layered load from the standard layers. SYNC (startup): wrap in
/// `spawn_blocking` from async contexts.
///
/// C1 review (item 2): uses [`norte_config::standard_layers_no_project`]
/// rather than [`norte_config::standard_layers`] — `norte-config::load`
/// never honors `[archive]` from the Project layer anyway (a foreign repo
/// must not raise the anti-bomb limits), so parsing `./.norte/norte.toml`
/// here would give it a startup-abort lever over `norte daemon run` and no
/// other effect.
///
/// # Errors
/// Those of [`load_archive_limits_from`].
pub fn load_archive_limits() -> std::io::Result<Option<Limits>> {
    load_archive_limits_from(&norte_config::standard_layers_no_project())
}

/// [`load_rar_delegate_from`] over the standard layers, Project excluded.
///
/// # Errors
/// Those of [`load_rar_delegate_from`].
pub fn load_rar_delegate() -> std::io::Result<Option<std::path::PathBuf>> {
    load_rar_delegate_from(&norte_config::standard_layers_no_project())
}

/// Aplica a `engine` los límites anti-bomba y el programa que lee RAR de
/// `[archive]` ([`load_archive_limits`], [`load_rar_delegate`]).
///
/// Fail-loud: un `norte.toml` roto aborta el arranque, mismo criterio que
/// `policy.toml`. Lo usan el daemon ([`crate::daemon::componer()`]) y los
/// comandos de la CLI que montan un engine propio; antes vivía en la CLI y el
/// daemon no podía llamarlo desde el core.
///
/// # Errors
/// Los de leer o validar `norte.toml`.
pub async fn aplicar(engine: &crate::Engine) -> std::io::Result<()> {
    let limites = crate::blocking::spawn_blocking(load_archive_limits)
        .await
        .map_err(std::io::Error::other)??;
    if let Some(limites) = limites {
        engine.set_archive_limits(limites);
    }
    // Ítem 11 del roadmap: qué programa lee los RAR. `None` = sondear PATH.
    let delegado = crate::blocking::spawn_blocking(load_rar_delegate)
        .await
        .map_err(std::io::Error::other)??;
    engine.set_rar_delegate(delegado);
    Ok(())
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

    /// C1 review, item 2: a broken/hostile `./.norte/norte.toml` (Project
    /// layer) must not be able to abort `norte daemon run`. This is pinned
    /// two ways, and one of them changed with #260. A broken PROJECT layer is
    /// now SKIPPED rather than fatal —the same reasoning one layer up: a
    /// `.norte.toml` someone else wrote must not break the file manager of
    /// whoever `cd`s into that repository— and the skip is REPORTED in
    /// `project_warnings`, never silent. The other half is unchanged and is
    /// the real fix for the daemon: `load_archive_limits()` does not ask for
    /// that entry at all, because it uses `standard_layers_no_project()`,
    /// whose exclusion of `Layer::Project` is pinned at the `norte-config`
    /// level by `standard_layers_no_project_excluye_proyecto`.
    #[test]
    fn norte_toml_de_proyecto_roto_no_aborta_el_daemon() {
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("norte.toml"),
            "[archive]\nmax_entries = 5\n",
        )
        .unwrap();
        let proyecto = tempfile::tempdir().unwrap();
        // Broken TOML — the kind of thing a hostile or merely careless
        // project checkout could ship.
        std::fs::write(proyecto.path().join("norte.toml"), "[archive\n").unwrap();

        // Explicit layers INCLUDING the broken project entry. Since #260 a
        // broken PROJECT layer is skipped rather than fatal — a `.norte.toml`
        // someone else wrote must not break the file manager of whoever `cd`s
        // into that repository — so this no longer errors. It is not
        // swallowed either: the layer is reported, and here the limits come
        // out as the USER's, which is what "the project layer contributed
        // nothing" means.
        let with_project = norte_config::Layers {
            dirs: vec![
                (user.path().to_path_buf(), norte_config::Layer::User),
                (proyecto.path().to_path_buf(), norte_config::Layer::Project),
            ],
        };
        let limits = load_archive_limits_from(&with_project)
            .expect("a broken PROJECT layer is skipped, not fatal");
        assert_eq!(
            limits.expect("hay límites").max_entries,
            5,
            "the user's layer still applies"
        );
        let cfg = norte_config::load(&with_project).expect("carga");
        assert_eq!(
            cfg.project_warnings.len(),
            1,
            "and the skipped layer is REPORTED, never silent"
        );

        // What the daemon actually does (`load_archive_limits()`, via
        // `standard_layers_no_project()`): the same broken project dir is
        // never consulted, so the same on-disk state loads cleanly.
        let without_project = norte_config::Layers {
            dirs: vec![(user.path().to_path_buf(), norte_config::Layer::User)],
        };
        let limits = load_archive_limits_from(&without_project)
            .expect("no project layer in play: loads cleanly")
            .expect("override present");
        assert_eq!(limits.max_entries, 5);
    }

    /// Unit coverage for the deduped helper (rust review item 3): overrides
    /// are respected per-field, an absent field keeps the compiled default,
    /// and `max_entries` saturates upward rather than wrapping.
    #[test]
    fn limits_from_overrides_respeta_overrides_y_defaults() {
        assert!(limits_from_overrides(None, None, None).is_none());

        let l = limits_from_overrides(Some(100), None, None).expect("override presente");
        assert_eq!(l.max_entries, 100);
        assert_eq!(
            l.max_decompressed_bytes,
            Limits::default().max_decompressed_bytes,
            "campo ausente conserva el default"
        );
        assert_eq!(
            l.max_nesting,
            Limits::default().max_nesting,
            "campo ausente conserva el default"
        );

        let l = limits_from_overrides(None, Some(1024), Some(3)).expect("overrides presentes");
        assert_eq!(l.max_decompressed_bytes, 1024);
        assert_eq!(l.max_nesting, 3);
        assert_eq!(
            l.max_entries,
            Limits::default().max_entries,
            "campo ausente conserva el default"
        );
    }

    /// Saturation branch (32-bit only in practice, but the conversion path
    /// is portable): a `max_entries` beyond `usize::MAX` never wraps to a
    /// small number — it saturates to `usize::MAX`, loud (a `tracing::warn`)
    /// rather than a silently-shrunk anti-bomb limit.
    #[test]
    fn limits_from_overrides_max_entries_satura_no_envuelve() {
        // On 64-bit `usize::try_from(u64)` never fails, so this pins the
        // in-range identity path instead — the saturation branch itself is
        // only reachable on 32-bit, documented in the rustdoc.
        let l = limits_from_overrides(Some(u64::from(u32::MAX)), None, None)
            .expect("override presente");
        assert_eq!(l.max_entries, u32::MAX as usize);
    }
}
