//! Frontend configuration: the shared scalar merge (`norte-config`) plus the
//! frontend-only passes — `keymap.toml` layers and `openers.toml` (#28).

use std::path::{Path, PathBuf};

use norte_config::schema::read_optional;
use norte_config::{CommonConfig, ConfigError, Layer, Layers, QuickSearch};

use crate::keymap::{KeymapFile, parse_keymap};
use crate::nav;
use crate::openers::OpenersConfig;

/// Everything a frontend needs, flat (same shape the TUI historically used).
#[derive(Debug, Clone)]
pub struct FrontendConfig {
    /// The merged scalars (preset, ui, daemon, hotlist, archive, ai, sources).
    pub common: CommonConfig,
    /// `keymap.toml` layers present, ascending precedence.
    pub keymap_layers: Vec<KeymapFile>,
    /// Quick-search mode mapped onto the navigation enum.
    pub quick_search_mode: nav::Mode,
    /// Merged declarative openers (#28): System/User only, fail-closed.
    pub openers: OpenersConfig,
}

/// Carga la capa `keymap.toml` de `dir` (ADR 0006/0007); `None` si no existe.
/// La capa de PROYECTO se marca (`mark_project`) para que `Effective` descarte
/// sus bindings `lua:` (seguridad #75). Una capa de usuario no admite la lista
/// `keymap` completa (eso es de presets): es error con archivo culpable.
///
/// `pub` porque norte-gui llama esto directamente (M4/Task 9): la GUI carga
/// keymaps sin pasar por el `load` combinado de este módulo.
///
/// # Errors
/// [`ConfigError::Toml`] si no parsea o usa `keymap` en una capa.
pub fn load_keymap_layer(
    dir: &Path,
    kind: Layer,
    sources: &mut Vec<PathBuf>,
) -> Result<Option<KeymapFile>, ConfigError> {
    let keymap = dir.join("keymap.toml");
    let Some(raw) = read_optional(&keymap)? else {
        return Ok(None);
    };
    let mut parsed = parse_keymap(&raw).map_err(|e| ConfigError::Toml {
        path: keymap.clone(),
        message: e.to_string(),
    })?;
    if kind == Layer::Project {
        parsed.mark_project();
    }
    if parsed.has_full_keymap() {
        return Err(ConfigError::Toml {
            path: keymap,
            message:
                "una capa de config no admite `keymap`: usa prepend_keymap/append_keymap (ADR 0006)"
                    .to_owned(),
        });
    }
    sources.push(keymap);
    Ok(Some(parsed))
}

/// Carga y parsea `openers.toml` de una capa (#28); `None` si el fichero no
/// existe o la capa es de PROYECTO (fail-closed — un `./.norte/openers.toml`
/// de un repo hostil no debe lanzar binarios externos).
///
/// `pub` porque norte-gui llama esto directamente (M4/Task 9): la GUI carga
/// openers sin pasar por el `load` combinado de este módulo.
///
/// # Errors
/// [`ConfigError::Toml`] con el archivo culpable si no parsea.
pub fn load_openers(
    dir: &Path,
    kind: Layer,
    sources: &mut Vec<PathBuf>,
) -> Result<Option<OpenersConfig>, ConfigError> {
    if kind == Layer::Project {
        return Ok(None);
    }
    let openers_path = dir.join("openers.toml");
    let Some(raw) = read_optional(&openers_path)? else {
        return Ok(None);
    };
    let parsed = OpenersConfig::parse(&raw).map_err(|e| ConfigError::Toml {
        path: openers_path.clone(),
        message: e.to_string(),
    })?;
    sources.push(openers_path);
    Ok(Some(parsed))
}

/// Load and merge every layer (ADR 0007): common scalars + keymap + openers.
///
/// # Errors
/// [`ConfigError`] with the culprit file; an absent layer is not an error.
pub fn load(layers: &Layers) -> Result<FrontendConfig, ConfigError> {
    let mut common = norte_config::load(layers)?;
    let mut keymap_layers = Vec::new();
    let mut openers = OpenersConfig::empty();
    for (dir, kind) in &layers.dirs {
        if let Some(parsed) = load_keymap_layer(dir, *kind, &mut common.sources)? {
            keymap_layers.push(parsed);
        }
        if let Some(parsed) = load_openers(dir, *kind, &mut common.sources)? {
            openers.extend_front(parsed);
        }
    }
    let quick_search_mode = match common.quick_search {
        QuickSearch::Filter => nav::Mode::Filter,
        QuickSearch::Jump => nav::Mode::Jump,
    };
    Ok(FrontendConfig {
        common,
        keymap_layers,
        quick_search_mode,
        openers,
    })
}

#[cfg(test)]
mod tests {
    use norte_config::{Layer, Layers};

    use super::*;

    /// #28 seguridad: un `openers.toml` en la capa de PROYECTO (`./.norte`) se
    /// IGNORA fail-closed — un repo hostil no puede inyectar un binario que se
    /// ejecute al pulsar F4. La capa de USUARIO sí se honra.
    #[test]
    fn openers_de_proyecto_se_ignoran_usuario_se_honra() {
        let usuario = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("openers.toml"),
            "[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n",
        )
        .unwrap();
        let proyecto = tempfile::tempdir().unwrap();
        std::fs::write(
            proyecto.path().join("openers.toml"),
            "[[opener]]\nmime = \"text/*\"\ncommand = [\"curl-malicioso\", \"%f\"]\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (usuario.path().to_path_buf(), Layer::User),
                (proyecto.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("carga");
        // Resuelve al opener del USUARIO, jamás al del proyecto.
        assert_eq!(
            cfg.openers
                .resolve_for("text/plain", "linux")
                .unwrap()
                .program(),
            "bat",
            "el opener de proyecto se ignora fail-closed"
        );
    }

    /// #28: entre capas, la superior (usuario) gana el empate de mimetype.
    #[test]
    fn openers_usuario_gana_sobre_sistema() {
        let sistema = tempfile::tempdir().unwrap();
        std::fs::write(
            sistema.path().join("openers.toml"),
            "[[opener]]\nmime = \"text/*\"\ncommand = [\"less\", \"%f\"]\n",
        )
        .unwrap();
        let usuario = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("openers.toml"),
            "[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (sistema.path().to_path_buf(), Layer::System),
                (usuario.path().to_path_buf(), Layer::User),
            ],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(
            cfg.openers
                .resolve_for("text/plain", "linux")
                .unwrap()
                .program(),
            "bat",
            "la capa de usuario (superior) gana"
        );
    }

    /// The combined loader wires all three passes: scalars, keymap layers,
    /// openers — and maps `quick_search` onto `nav::Mode`.
    #[test]
    fn load_combina_escalares_keymap_y_openers() {
        let usuario = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[ui]\nquick_search = \"jump\"\n[keymap]\npreset = \"vim\"\n",
        )
        .unwrap();
        std::fs::write(
            usuario.path().join("keymap.toml"),
            "[pane]\nprepend_keymap = [{ on = [\"j\"], run = \"cursor.down\" }]\n",
        )
        .unwrap();
        std::fs::write(
            usuario.path().join("openers.toml"),
            "[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(usuario.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.common.preset, "vim");
        assert_eq!(cfg.quick_search_mode, nav::Mode::Jump);
        assert_eq!(cfg.keymap_layers.len(), 1);
        assert!(cfg.openers.resolve_for("text/plain", "linux").is_some());
        assert_eq!(
            cfg.common.sources.len(),
            3,
            "norte.toml + keymap.toml + openers.toml"
        );
    }
}
