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

/// ¿Este spec es un preset EMBEBIDO, o sea que resolverlo no toca el disco?
///
/// La usan los dos frontends para partir el camino: un preset se resuelve en
/// el sitio —es aritmética sobre colores, y mandarlo a otro hilo añadiría un
/// frame de retraso a algo que el lector ve cambiar bajo el cursor— y una RUTA
/// se lee, así que va por `spawn_blocking` (regla 2). Sin esta pregunta, las
/// dos superficies elegían «solo presets» y un `[ui] theme` que nombra un
/// fichero se caía en silencio al cambiar de perfil.
///
/// `None` cuenta como preset: es el de fábrica, y tampoco toca el disco.
///
/// ```
/// use norte_frontend::theme::is_preset;
/// assert!(is_preset(None));
/// assert!(is_preset(Some("nord")));
/// assert!(!is_preset(Some("/home/u/.config/norte/mio.toml")));
/// ```
#[must_use]
pub fn is_preset(spec: Option<&str>) -> bool {
    match spec {
        None => true,
        Some(s) => matches!(Theme::preset(s), Ok(Some(_))),
    }
}

/// `EntryKind` del protocolo → `FileKind` del tema.
///
/// El protocolo no distingue aún ejecutable/fifo/socket/dispositivo —el
/// `Entry` no lleva modo— así que todo lo que no es directorio ni enlace cae a
/// `Regular`; el color por EXTENSIÓN sigue aplicando encima. Como consecuencia,
/// las claves `executable`, `fifo`, `socket`, `block-device` y `char-device`
/// que los presets traen en `[files.kind]` están DORMIDAS: ningún frontend
/// puede seleccionarlas todavía.
///
/// Vive aquí y no en cada frontend porque los dos la necesitan y son la misma
/// decisión (ADR 0077): escrita dos veces, diverge en silencio — y el día que
/// `Entry` lleve modo, un frontend lo aprovecharía y el otro no.
///
/// ```
/// use norte_frontend::theme::file_kind_of;
/// use norte_proto::EntryKind;
/// use norte_theme::FileKind;
/// assert_eq!(file_kind_of(EntryKind::Dir), FileKind::Dir);
/// assert_eq!(file_kind_of(EntryKind::Other), FileKind::Regular);
/// ```
#[must_use]
pub fn file_kind_of(kind: norte_proto::EntryKind) -> norte_theme::FileKind {
    use norte_proto::EntryKind;
    use norte_theme::FileKind;
    match kind {
        EntryKind::Dir => FileKind::Dir,
        EntryKind::Symlink => FileKind::Symlink,
        EntryKind::File | EntryKind::Other => FileKind::Regular,
    }
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
