//! Arrancar: leer la configuración UNA vez, resolver dónde está el daemon y
//! dónde empieza el listado, y montar el host.
//!
//! Ni un parser de argumentos nuevo (`norte_frontend::cli`), ni una lectura de
//! configuración propia (`norte_config`), ni una segunda idea de dónde vive el
//! socket (`norte_client::default_socket_path`): un frontend que resuelve
//! estas cosas a su manera es un frontend que arranca en otro sitio que el
//! resto (ADR 0066, decisión D14).

use std::path::PathBuf;
use std::sync::Arc;

use norte_client::RemoteBackend;
use norte_i18n::Lang;
use norte_proto::VPath;
use norte_proto::methods::ClientInfo;
use norte_theme::Theme;
use norte_ui_host::{UiHost, UiHostOptions, ViewSnapshot};

/// La ayuda. Corta a propósito: el spike no tiene superficie que documentar.
pub const USAGE: &str = "\
norte-gui — el renderer gráfico de norte (spike de la fase 3)

USO:
    norte-gui [DIR] [OPCIONES]

ARGUMENTOS:
    DIR                  Directorio de arranque (por defecto, el actual)

OPCIONES:
    --socket <RUTA>      Socket del daemon (por defecto, el del sistema)
    --layout <NOMBRE>    Disposición de arranque (por defecto, la de la config)
    --preset <NOMBRE>    Preset de teclado (por defecto, el de la config)
    -h, --help           Esta ayuda
    -V, --version        La versión
";

/// Lo que puede impedir arrancar.
#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    /// Un flag que no existe.
    #[error("flag desconocido: {0}")]
    UnknownFlag(String),
    /// El directorio de arranque no sirve.
    #[error("directorio de arranque: {0}")]
    Dir(String),
    /// La configuración no carga.
    #[error("configuración: {0}")]
    Config(String),
    /// No hay daemon al otro lado.
    #[error("no se pudo conectar con el daemon en {socket}: {source}")]
    Connect {
        /// Dónde se buscó.
        socket: String,
        /// Qué dijo el daemon (o el socket).
        #[source]
        source: norte_proto::Error,
    },
    /// El host no arrancó.
    #[error("el host no arrancó: {0}")]
    Host(#[from] norte_ui_host::controller::UiError),
}

/// Los argumentos ya resueltos.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cli {
    /// Directorio de arranque, crudo (regla 1: no tiene por qué ser UTF-8).
    pub dir: Option<PathBuf>,
    /// Socket del daemon.
    pub socket: Option<PathBuf>,
    /// Disposición pedida para ESTE arranque.
    pub layout: Option<String>,
    /// Preset de teclado pedido para ESTE arranque.
    pub preset: Option<String>,
    /// Se pidió la ayuda.
    pub help: bool,
    /// Se pidió la versión.
    pub version: bool,
}

/// Parsea con el MISMO parser que el TUI.
///
/// # Errors
/// [`StartupError::UnknownFlag`] si aparece un flag que no existe: un flag
/// mal escrito que se ignora es una opción que el usuario cree haber puesto.
pub fn parse<I, T>(args: I) -> Result<Cli, StartupError>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString>,
{
    let crudo = norte_frontend::cli::parse(args, &[], &["--socket", "--layout", "--preset"]);
    if let Some(flag) = crudo.unknown {
        return Err(StartupError::UnknownFlag(flag));
    }
    Ok(Cli {
        dir: crudo.dir.clone(),
        socket: crudo.path("--socket"),
        // El spike solo acepta disposiciones de FÁBRICA, así que el nombre es
        // texto por contrato. En cuanto se carguen ficheros de `layouts/`
        // tendrá que ser `os_text` (bytes): dos nombres inválidos distintos
        // no pueden abrir el mismo fichero (#246).
        layout: crudo.text("--layout"),
        preset: crudo.text("--preset"),
        help: crudo.help,
        version: crudo.version,
    })
}

/// Todo lo que el proceso necesita para pintar.
pub struct Boot {
    /// El host, ya con su primer listado pedido.
    pub host: UiHost,
    /// La primera foto: la secuencia 0.
    pub snapshot: ViewSnapshot,
    /// El idioma negociado.
    pub lang: Lang,
    /// El tema resuelto.
    pub theme: Theme,
}

/// Monta el host: configuración, socket, directorio, keymap y disposición.
///
/// # Errors
/// [`StartupError`] si la configuración no carga, el directorio no vale, o no
/// hay daemon al otro lado.
pub async fn boot(cli: &Cli) -> Result<Boot, StartupError> {
    // Las MISMAS capas que el TUI, leídas fuera del runtime (regla 2).
    let capas = norte_config::standard_layers();
    let cfg = match tokio::task::spawn_blocking(move || norte_frontend::config::load(&capas)).await
    {
        Ok(res) => res.map_err(|e| StartupError::Config(e.to_string()))?,
        // Un panic dentro de `load` es un bug NUESTRO: no se entierra como
        // un error de configuración con una ruta inventada (regla 6).
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    };

    let lang = match cfg.common.ui_lang.as_deref() {
        Some(l) => Lang::negotiate(Some(l)),
        None => Lang::from_env(),
    };
    let _ = norte_i18n::force(lang);

    let theme = tema(cfg.common.ui_theme.as_deref());
    let inicio = start_dir(cli.dir.clone())?;

    let socket = cli
        .socket
        .clone()
        .or_else(|| cfg.common.daemon_socket.clone())
        .unwrap_or_else(|| norte_client::default_socket_path(None));

    // Daemon y SOLO daemon: la GUI de referencia no construye un `Engine` en
    // su proceso (decisión D10). Si no hay daemon, se dice; no se levanta un
    // motor por detrás con otras garantías de journal y de sesión.
    let backend = RemoteBackend::connect(
        socket.clone(),
        None,
        ClientInfo {
            name: "norte-gui".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        },
    )
    .await
    .map_err(|source| StartupError::Connect {
        socket: socket.display().to_string(),
        source,
    })?;

    let preset = cli
        .preset
        .clone()
        .unwrap_or_else(|| cfg.common.preset.clone());
    let keymap = norte_ui_host::keys::keymap_de_preset(&preset)
        .or_else(|_| norte_ui_host::keys::keymap_de_preset("orthodox"))
        .map_err(|e| StartupError::Config(e.to_string()))?;
    // El visor es otra PANTALLA, con el mismo preset: `esc` cierra y `e`
    // recarga con otro encoding porque eso es lo que dice el preset, no
    // porque el renderer lo decida.
    let keymap_viewer = norte_ui_host::keys::keymap_visor_de_preset(&preset)
        .or_else(|_| norte_ui_host::keys::keymap_visor_de_preset("orthodox"))
        .map_err(|e| StartupError::Config(e.to_string()))?;

    let nombre_layout = cli
        .layout
        .clone()
        .or_else(|| cfg.common.ui_layout.clone())
        .unwrap_or_else(|| "orthodox".to_owned());
    // Una disposición que no carga NO deja la ventana sin pantalla: se cae a
    // la de siempre, que es lo que el usuario tenía antes de escribir la
    // clave (la misma regla que el TUI).
    let layout = norte_frontend::layout::presets::tree(&nombre_layout)
        .or_else(|_| norte_frontend::layout::presets::tree("orthodox"))
        .map_err(|e| StartupError::Config(e.to_string()))?;

    let columnas = norte_frontend::columns::ColumnsSettings::resolve(&cfg.common.ui_columns);
    let (host, snapshot) = UiHost::start(UiHostOptions {
        backend: Arc::new(backend),
        initial_dir: inicio,
        locale: match lang {
            Lang::Es => "es".to_owned(),
            Lang::En => "en".to_owned(),
        },
        keymap,
        keymap_viewer,
        layout,
        // El renderer corrige el tamaño en cuanto sepa el suyo; esto es lo
        // que se reparte mientras tanto.
        viewport: (120, 40),
        columns: columnas
            .layout_items_for("file")
            .into_iter()
            .map(|(id, _)| id)
            .collect(),
    })
    .await?;
    Ok(Boot {
        host,
        snapshot,
        lang,
        theme,
    })
}

fn tema(nombre: Option<&str>) -> Theme {
    match nombre {
        Some(n) => Theme::preset(n)
            .ok()
            .flatten()
            .unwrap_or_else(Theme::preset_default),
        None => Theme::preset_default(),
    }
}

/// El directorio de arranque, como `VPath`.
///
/// Se valida AQUÍ y no dentro de la ventana: un error de arranque con la
/// pantalla ya montada es un cuadro gris que no dice nada.
fn start_dir(dir: Option<PathBuf>) -> Result<VPath, StartupError> {
    let nativo = match dir {
        Some(d) => {
            let meta = std::fs::metadata(&d)
                .map_err(|e| StartupError::Dir(format!("{}: {e}", d.display())))?;
            if !meta.is_dir() {
                return Err(StartupError::Dir(format!(
                    "{} no es un directorio",
                    d.display()
                )));
            }
            std::path::absolute(&d).unwrap_or(d)
        }
        None => std::env::current_dir().map_err(|e| StartupError::Dir(e.to_string()))?,
    };
    norte_vfs_local::vpath_from_native(&nativo)
        .map_err(|e| StartupError::Dir(format!("{}: {e}", nativo.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn los_flags_se_leen_como_en_el_tui() {
        let cli = parse(["--socket", "/tmp/x.sock", "--layout", "simple"]).expect("parsea");
        assert_eq!(
            cli.socket.as_deref(),
            Some(std::path::Path::new("/tmp/x.sock"))
        );
        assert_eq!(cli.layout.as_deref(), Some("simple"));
        assert!(!cli.help);
    }

    /// Un flag mal escrito NO se ignora: se dice. Ignorarlo es arrancar sin
    /// la opción que el usuario cree haber puesto.
    #[test]
    fn un_flag_desconocido_no_se_traga() {
        let e = parse(["--socketo", "/tmp/x"]).expect_err("no vale");
        assert!(
            matches!(&e, StartupError::UnknownFlag(f) if f == "--socketo"),
            "{e}"
        );
    }

    #[test]
    fn la_ayuda_y_la_version_se_reconocen() {
        assert!(parse(["--help"]).expect("parsea").help);
        assert!(parse(["-V"]).expect("parsea").version);
    }

    /// Un directorio que no existe se dice ANTES de abrir ventana.
    #[test]
    fn un_directorio_que_no_existe_se_dice_pronto() {
        let e = start_dir(Some(PathBuf::from("/no/existe/ni/de/lejos"))).expect_err("falla");
        assert!(matches!(e, StartupError::Dir(_)), "{e}");
    }
}
