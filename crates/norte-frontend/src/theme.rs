//! Frontend-shared theme resolution (ADR 0020): a `[ui].theme` spec is a
//! bundled preset NAME or a PATH to a theme TOML. Each frontend then bridges
//! the resolved [`Theme`] to its renderer (ratatui in the TUI, GPUI in the
//! GUI).

use std::path::Path;

use norte_theme::Theme;

/// Error tipado de [`resolve_theme`] (#73): el caller mapea cada variante a
/// una clave Fluent para la barra — jamás el `Display` del OS (localizado
/// por el SO) ni el diagnóstico crudo del parser ni el `spec` (que puede
/// venir de la capa `./.norte` de un repo AJENO) sin sanear. El `Display`
/// thiserror es solo para logs/stderr.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// La ruta del spec no se pudo leer.
    #[error("tema {spec:?}: {source}")]
    Io {
        /// El spec `[ui].theme` tal cual (ruta).
        spec: String,
        /// La causa.
        source: std::io::Error,
    },
    /// El TOML del tema (o el preset embebido) no valida.
    #[error("tema {spec:?}: {detail}")]
    Parse {
        /// El spec `[ui].theme` tal cual (nombre o ruta).
        spec: String,
        /// Diagnóstico de `norte-theme`.
        detail: String,
    },
}

/// Resolves the `[ui].theme` spec: an embedded preset name or a path to a
/// `.toml` theme file. `None` = the default preset. SYNC (startup):
/// wrap in `spawn_blocking` from async contexts.
///
/// # Errors
/// [`ResolveError`] if the path cannot be read or the TOML does not
/// validate; the caller decides to degrade to the default and warn.
pub fn resolve_theme(spec: Option<&str>) -> Result<Theme, ResolveError> {
    let Some(spec) = spec else {
        return Ok(Theme::preset_default());
    };
    let parse = |e: norte_theme::ThemeError| ResolveError::Parse {
        spec: spec.to_owned(),
        detail: e.to_string(),
    };
    if let Some(theme) = Theme::preset(spec).map_err(parse)? {
        return Ok(theme);
    }
    let path = Path::new(spec);
    let raw = std::fs::read_to_string(path).map_err(|e| ResolveError::Io {
        spec: spec.to_owned(),
        source: e,
    })?;
    Theme::from_toml(&raw).map_err(parse)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_es_el_preset_default() {
        let t = resolve_theme(None).expect("default");
        assert_eq!(t.name.as_deref(), Theme::preset_default().name.as_deref());
    }

    #[test]
    fn nombre_de_preset_resuelve() {
        let t = resolve_theme(Some("nord")).expect("preset embebido");
        assert_eq!(t.name.as_deref(), Some("nord"));
    }

    #[test]
    fn ruta_a_fichero_resuelve_y_rota_es_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("mio.toml");
        std::fs::write(&p, "name = \"mio\"\n").unwrap();
        let t = resolve_theme(Some(p.to_str().unwrap())).expect("fichero");
        assert_eq!(t.name.as_deref(), Some("mio"));
        let missing = dir.path().join("no-existe.toml");
        assert!(matches!(
            resolve_theme(Some(missing.to_str().unwrap())),
            Err(ResolveError::Io { .. })
        ));
    }
}
