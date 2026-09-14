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

/// Un tema del USUARIO: un `<config>/themes/<nombre>.toml` ya parseado.
///
/// Se carga con la config —en un contexto que ya puede tocar el disco— y
/// viaja con ella, para que los selectores y el asistente lo ofrezcan y lo
/// previsualicen sin leer un fichero dentro de una tecla (regla 2).
#[derive(Debug, Clone)]
pub struct UserTheme {
    /// El nombre por el que se elige: el del fichero sin `.toml`.
    pub name: String,
    /// El tema, ya validado.
    pub theme: Theme,
}

/// ¿Vale `s` como nombre de tema del usuario?
///
/// Un NOMBRE, no una ruta: sin separadores ni `..`, sin empezar por punto y
/// en un alfabeto que se puede escribir tal cual en `[ui] theme`. Lo que no
/// pase se trata como ruta, que es lo que era antes de que hubiera nombres.
///
/// Pública para `norte theme import`, que tiene que rehusar escribir un
/// fichero que el resolutor nunca buscaría por nombre.
///
/// ```
/// use norte_frontend::theme::is_theme_name;
/// assert!(is_theme_name("one-dark-pro"));
/// assert!(!is_theme_name("../fuera"));
/// assert!(!is_theme_name(".oculto"));
/// ```
#[must_use]
pub fn is_theme_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with('.')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// `<config_dir>/themes`.
fn directorio_de_temas(config_dir: &Path) -> std::path::PathBuf {
    config_dir.join("themes")
}

/// Los temas del usuario de `<config_dir>/themes/*.toml`, por nombre.
///
/// No falla: lo que no sirve se salta, porque una lista de la que falta un
/// fichero roto es mejor que un selector que no abre. Se salta un fichero que
/// no parsea, un nombre que no es de tema, un nombre que no es UTF-8 (tiene
/// que poder escribirse en `[ui] theme`, que es texto) y un nombre que ya es
/// de un preset: el resolutor pone los presets primero, así que ese fichero
/// nunca se leería, y listarlo ofrecería una elección que no existe.
///
/// SYNC: lee el disco. La llama la carga de la config, que ya corre donde
/// puede.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// assert!(norte_frontend::theme::load_user_themes(dir.path()).is_empty());
/// ```
#[must_use]
pub fn load_user_themes(config_dir: &Path) -> Vec<UserTheme> {
    let Ok(entradas) = std::fs::read_dir(directorio_de_temas(config_dir)) else {
        return Vec::new();
    };
    let mut temas: Vec<UserTheme> = entradas
        .filter_map(Result::ok)
        .filter_map(|entrada| {
            let path = entrada.path();
            if path.extension().is_none_or(|x| x != "toml") {
                return None;
            }
            let name = path.file_stem()?.to_str()?.to_owned();
            if !is_theme_name(&name) || matches!(Theme::preset(&name), Ok(Some(_))) {
                return None;
            }
            let raw = std::fs::read_to_string(&path).ok()?;
            let theme = Theme::from_toml(&raw).ok()?;
            Some(UserTheme { name, theme })
        })
        .collect();
    temas.sort_by(|a, b| a.name.cmp(&b.name));
    temas
}

/// Los nombres de tema que se ofrecen: los presets embebidos y, detrás, los
/// del usuario.
///
/// UNA lista para todas las superficies —los dos selectores, los dos
/// asistentes, las dos pantallas de ajustes—, que construían cada una la suya
/// desde `preset_names()`: seis sitios escribiendo «qué temas hay» es como una
/// lista diverge en silencio (ADR 0077).
///
/// ```
/// let nombres = norte_frontend::theme::theme_names(&[]);
/// assert!(nombres.iter().any(|n| n == "nord"));
/// ```
#[must_use]
pub fn theme_names(user: &[UserTheme]) -> Vec<String> {
    norte_theme::preset_names()
        .into_iter()
        .map(String::from)
        .chain(user.iter().map(|t| t.name.clone()))
        .collect()
}

/// El tema que nombra `name` SIN tocar el disco: un preset embebido o uno del
/// usuario ya cargado. Es lo que usa una vista previa en vivo.
///
/// ```
/// let t = norte_frontend::theme::theme_by_name("nord", &[]).expect("preset");
/// assert_eq!(t.name.as_deref(), Some("nord"));
/// ```
#[must_use]
pub fn theme_by_name(name: &str, user: &[UserTheme]) -> Option<Theme> {
    if let Ok(Some(theme)) = Theme::preset(name) {
        return Some(theme);
    }
    user.iter()
        .find(|t| t.name == name)
        .map(|t| t.theme.clone())
}

/// Resolves the `[ui].theme` spec: an embedded preset name, the name of a
/// theme in `<config>/themes/`, or a path to a `.toml` theme file, in that
/// order. `None` = the default preset. SYNC (startup): wrap in
/// `spawn_blocking` from async contexts.
///
/// # Errors
/// [`ResolveError`] if the path cannot be read or the TOML does not
/// validate; the caller decides to degrade to the default and warn.
pub fn resolve_theme(spec: Option<&str>) -> Result<Theme, ResolveError> {
    resolve_theme_in(spec, norte_config::user_config_dir().as_deref())
}

/// [`resolve_theme`] against an explicit config directory, for tests and for
/// callers that already know it.
///
/// The order is fixed and load-bearing: an embedded preset first, so a stale
/// `themes/nord.toml` cannot change what `nord` means; then
/// `<config_dir>/themes/<name>.toml` when the spec is a plain name; then the
/// spec as a path.
///
/// # Errors
/// As [`resolve_theme`].
pub fn resolve_theme_in(
    spec: Option<&str>,
    config_dir: Option<&Path>,
) -> Result<Theme, ResolveError> {
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
    if let Some(dir) = config_dir
        && is_theme_name(spec)
    {
        let candidato = directorio_de_temas(dir).join(format!("{spec}.toml"));
        match std::fs::read_to_string(&candidato) {
            Ok(raw) => return Theme::from_toml(&raw).map_err(parse),
            // No hay tema con ese nombre: puede ser una ruta relativa.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(ResolveError::Io {
                    spec: spec.to_owned(),
                    source: e,
                });
            }
        }
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

    fn con_tema(dir: &Path, nombre: &str, toml: &str) {
        let temas = dir.join("themes");
        std::fs::create_dir_all(&temas).unwrap();
        std::fs::write(temas.join(format!("{nombre}.toml")), toml).unwrap();
    }

    /// Un nombre que no es preset se busca en `<config>/themes/<nombre>.toml`
    /// ANTES de tratarse como ruta.
    #[test]
    fn un_nombre_de_usuario_resuelve_contra_el_directorio_de_temas() {
        let dir = tempfile::tempdir().unwrap();
        con_tema(dir.path(), "mio", "name = \"mio\"\n");
        let t = resolve_theme_in(Some("mio"), Some(dir.path())).expect("resuelve");
        assert_eq!(t.name.as_deref(), Some("mio"));
    }

    /// Y un preset EMBEBIDO no se puede tapar con un fichero.
    #[test]
    fn un_fichero_no_puede_tapar_un_preset() {
        let dir = tempfile::tempdir().unwrap();
        con_tema(dir.path(), "nord", "name = \"impostor\"\n");
        let t = resolve_theme_in(Some("nord"), Some(dir.path())).expect("resuelve");
        assert_eq!(t.name.as_deref(), Some("nord"), "gana el preset embebido");
        assert!(
            load_user_themes(dir.path()).is_empty(),
            "y no se lista: sería una elección que no existe"
        );
    }

    /// La lista: presets primero, los del usuario detrás y por nombre, sin
    /// los rotos, los ocultos ni los que no son un nombre.
    #[test]
    fn los_temas_del_usuario_se_listan_detras_de_los_presets() {
        let dir = tempfile::tempdir().unwrap();
        con_tema(dir.path(), "zeta", "name = \"zeta\"\n");
        con_tema(dir.path(), "mio", "name = \"mio\"\n");
        con_tema(dir.path(), "roto", "esto no es toml {{{");
        con_tema(dir.path(), ".oculto", "name = \"oculto\"\n");
        std::fs::write(dir.path().join("themes/nota.txt"), "no es un tema").unwrap();
        let usuario = load_user_themes(dir.path());
        let nombres: Vec<&str> = usuario.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(nombres, ["mio", "zeta"]);

        let todos = theme_names(&usuario);
        assert!(todos.iter().any(|n| n == "vscode-dark"));
        assert_eq!(&todos[todos.len() - 2..], ["mio", "zeta"]);
    }

    /// Sin directorio de temas no hay nada, y no es un error.
    #[test]
    fn sin_directorio_de_temas_la_lista_es_la_de_presets() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_user_themes(dir.path()).is_empty());
        assert_eq!(theme_names(&[]).len(), norte_theme::preset_names().len());
    }

    /// La vista previa no toca el disco: preset o tema ya cargado, o nada.
    #[test]
    fn theme_by_name_encuentra_presets_y_temas_cargados() {
        let usuario = vec![UserTheme {
            name: "mio".to_owned(),
            theme: Theme::preset_default(),
        }];
        assert!(theme_by_name("mio", &usuario).is_some());
        assert_eq!(
            theme_by_name("nord", &usuario).and_then(|t| t.name),
            Some("nord".to_owned())
        );
        assert!(theme_by_name("otro", &usuario).is_none());
    }

    /// Un spec con separador no es un nombre: no se busca en `themes/`, se
    /// trata como ruta.
    #[test]
    fn un_spec_con_separador_no_se_busca_como_tema_de_usuario() {
        let dir = tempfile::tempdir().unwrap();
        con_tema(dir.path(), "mio", "name = \"mio\"\n");
        assert!(matches!(
            resolve_theme_in(Some("themes/../mio"), Some(dir.path())),
            Err(ResolveError::Io { .. })
        ));
    }
}
