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
use norte_frontend::layout_picker::UserLayout;
use norte_i18n::Lang;
use norte_proto::VPath;
use norte_proto::methods::ClientInfo;
use norte_theme::Theme;
use norte_ui_host::pickers::HostTheme;
use norte_ui_host::settings::{ConfigLayer, HostPath, HostPaths};
use norte_ui_host::{UiHost, UiHostOptions, ViewSnapshot};

/// Hasta dónde llega esta ventana HOY.
///
/// Solo lectura, y es una decisión escrita: la revisión de seguridad de la
/// tarea 3.3 encontró que el preset ya ataba F7/F8 a crear y borrar, y que
/// `Dialog{choice:"approve"}` aprobaba la operación de un agente — o sea que
/// la rebanada «de solo lectura» tenía autoridad destructiva y de policy. Se
/// levanta en la fase 5, junto con el camino seguro que esa fase define.
pub const EFECTOS: norte_ui_host::commands::Efectos = norte_ui_host::commands::Efectos::SoloLectura;

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
    /// Un valor de la línea de órdenes que no existe.
    #[error("{que}: «{valor}» no existe")]
    Desconocido {
        /// Qué opción.
        que: &'static str,
        /// Lo que se pidió.
        valor: String,
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
    /// Disposición pedida para ESTE arranque, con los BYTES intactos.
    ///
    /// Un nombre de disposición acaba siendo un nombre de fichero
    /// (`layouts/<nombre>.toml`), y dos bytes inválidos distintos colapsan al
    /// MISMO `\u{FFFD}` con una conversión lossy: abrirían el mismo fichero
    /// (#246, ADR 0061). El TUI ya lo lee así.
    pub layout: Option<std::ffi::OsString>,
    /// Preset de teclado pedido para ESTE arranque.
    pub preset: Option<std::ffi::OsString>,
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
        layout: crudo.os_text("--layout").map(std::ffi::OsString::from),
        preset: crudo.os_text("--preset").map(std::ffi::OsString::from),
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

/// Las disposiciones que el usuario tiene guardadas, YA leídas.
///
/// Leídas aquí y no por nombre porque el selector pinta la FORMA de cada una:
/// leerlas al mover el cursor sería I/O en el bucle de eventos. Una que no
/// parsea se conserva CON su motivo — el selector la enseña sin vista previa
/// y explica por qué, que es más útil que una fila que no está.
///
/// Va por `spawn_blocking`, como sus dos vecinas. «Es el arranque y es un
/// directorio pequeño» no es el criterio: `read_dir` sobre una capa de
/// configuración en un montaje caído bloquea el hilo de trabajo del runtime
/// igual de bien, y aquí ni siquiera hay ventana donde decirlo (regla 2).
fn disposiciones_del_usuario(capas: &norte_config::Layers) -> Vec<UserLayout> {
    let Some((dir, _)) = capas
        .dirs
        .iter()
        .rev()
        .find(|(_, k)| *k == norte_config::Layer::User)
    else {
        return Vec::new();
    };
    norte_frontend::layout::config::list(dir)
        .into_iter()
        .map(|name| UserLayout {
            tree: norte_frontend::layout::config::load(dir, &name).map_err(|e| e.to_string()),
            name,
        })
        .collect()
}

/// El tema, para poder verlo por dentro desde la ventana.
///
/// Los roles salen de la MISMA correspondencia explícita que alimenta las
/// variables CSS (`catalog::variables`), no de un volcado aparte: lo que la
/// vista enseña es literalmente lo que pinta. Los efectos se nombran uno a
/// uno como NO soportados, porque este renderer es una webview y no
/// interpreta ninguno — y un tema retro que se ve idéntico se lee como roto.
fn tema_visto(spec: Option<&str>, theme: &Theme) -> HostTheme {
    HostTheme {
        // El RESUELTO, que es el de `theme`. `spec` es lo que se pidió, y con
        // un fichero roto los dos no coinciden.
        name: spec.unwrap_or("default").to_owned(),
        roles: crate::catalog::variables(theme).into_iter().collect(),
        effects: efectos_declarados(theme),
    }
}

/// Los nombres de los efectos que el tema declara.
///
/// El bloque `[effects]` es libre a propósito (ADR 0036): cada renderer lo
/// interpreta. Aquí solo se enumeran sus claves de primer nivel, que es lo
/// que hace falta para decir cuáles no se pintan.
fn efectos_declarados(theme: &Theme) -> Vec<String> {
    // `Theme::effects` es un `toml::Value` y este crate no depende de `toml`
    // (ni tiene por qué: no parsea configuración). Se pregunta por la forma
    // a través del tipo que ya tiene delante.
    theme.effect_names().unwrap_or_default()
}

/// El idioma de la ventana, y fijado para todo el proceso.
///
/// La configuración manda sobre el entorno: `[ui] lang` es una decisión que
/// el usuario escribió, y `LANG` es lo que había puesto.
fn idioma(pedido: Option<&str>) -> Lang {
    let lang = match pedido {
        Some(l) => Lang::negotiate(Some(l)),
        None => Lang::from_env(),
    };
    let _ = norte_i18n::force(lang);
    lang
}

/// Las dos lecturas de disco del arranque que no son la configuración.
///
/// Juntas y fuera del runtime (regla 2): `rutas` hace un `metadata` por sitio
/// y `disposiciones_del_usuario` un `read_dir` más un `read_to_string` por
/// disposición. Las dos sobre las MISMAS capas que `config::load`, que ya iba
/// por `spawn_blocking` por este mismo motivo, y las dos pueden tocar un
/// montaje caído.
async fn diagnostico(
    capas: &norte_config::Layers,
    socket: &std::path::Path,
) -> (HostPaths, Vec<UserLayout>) {
    let capas = capas.clone();
    let socket = socket.to_path_buf();
    match tokio::task::spawn_blocking(move || {
        (rutas(&capas, &socket), disposiciones_del_usuario(&capas))
    })
    .await
    {
        Ok(par) => par,
        // Un panic aquí es un bug NUESTRO, no un directorio que falta.
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// Dónde vive cada cosa, para la vista de diagnóstico de los ajustes.
///
/// Se construye con las capas que el arranque ACABA de leer y con el socket
/// al que acaba de conectar: preguntarlo otra vez podría contestar otra cosa
/// (un `NORTE_CONFIG_DIR` que cambie, un `--socket` que se ignore) y la
/// ventana diría que su configuración sale de un sitio distinto de donde
/// salió de verdad.
fn rutas(capas: &norte_config::Layers, socket: &std::path::Path) -> HostPaths {
    // Con su `missing` YA resuelto: el host proyecta esta lista dentro del
    // bucle del único escritor, y un `exists()` allí es `std::fs::metadata`
    // sobre —entre otras— una capa de configuración que puede estar en un
    // montaje caído. Esta función corre en `spawn_blocking` (regla 2).
    let sitio = |p: PathBuf| HostPath {
        missing: !p.exists(),
        path: p,
    };
    HostPaths {
        config_layers: capas
            .dirs
            .iter()
            .map(|(dir, kind)| {
                let capa = match kind {
                    norte_config::Layer::System => ConfigLayer::System,
                    norte_config::Layer::User => ConfigLayer::User,
                    norte_config::Layer::Project => ConfigLayer::Project,
                };
                (capa, sitio(dir.clone()))
            })
            .collect(),
        state_dir: norte_config::dirs::state_dir().map(sitio),
        // El MISMO sitio al que escribe `logging()`, que es lo único que hace
        // útil enseñarlo.
        logs_dir: norte_config::dirs::state_dir()
            .map(|d| d.join("logs"))
            .map(sitio),
        socket: Some(sitio(socket.to_path_buf())),
    }
}

/// Monta el host: configuración, socket, directorio, keymap y disposición.
///
/// # Errors
/// [`StartupError`] si la configuración no carga, el directorio no vale, o no
/// hay daemon al otro lado.
pub async fn boot(cli: &Cli) -> Result<Boot, StartupError> {
    // Las MISMAS capas que el TUI, leídas fuera del runtime (regla 2).
    let capas = norte_config::standard_layers();
    // Se guardan para la vista de «dónde vive cada cosa»: el host no descubre
    // ficheros, así que la lista de capas se la damos ya resuelta y es
    // exactamente la que se acaba de LEER, no una que se vuelva a calcular.
    let capas_vistas = capas.clone();
    let cfg = match tokio::task::spawn_blocking(move || norte_frontend::config::load(&capas)).await
    {
        Ok(res) => res.map_err(|e| StartupError::Config(e.to_string()))?,
        // Un panic dentro de `load` es un bug NUESTRO: no se entierra como
        // un error de configuración con una ruta inventada (regla 6).
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    };

    let lang = idioma(cfg.common.ui_lang.as_deref());

    let (theme, tema_resuelto) = tema(cfg.common.ui_theme.as_deref());
    // Fuera del runtime (regla 2): `metadata` sobre un NFS caído bloquea el
    // hilo de trabajo hasta que expire el montaje, y encima antes de que
    // exista ventana donde decirlo. La lectura de la configuración de arriba
    // ya iba por `spawn_blocking`; esta se quedó a ocho líneas.
    let dir_pedido = cli.dir.clone();
    let inicio = match tokio::task::spawn_blocking(move || start_dir(dir_pedido)).await {
        Ok(res) => res?,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    };

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

    let preset = if let Some(p) = &cli.preset {
        nombre_de(p, "--preset")?
    } else {
        cfg.common.preset.clone()
    };
    // SOLO LECTURA hasta la fase 5. El preset ata F7/F8 a crear y borrar, y
    // que la tecla exista no es permiso: el gate de salida de la fase 4 dice
    // que ninguna mutación está viva hasta que la fase 5 traiga su camino
    // seguro. Aquí eso significa que esos comandos no entran en el keymap
    // efectivo —la tecla se responde «aquí no» en vez de quedarse muda— y que
    // el host los rechaza aunque llegaran por otra vía.
    // Un preset que no existe se DICE. El mismo fichero rechaza a gritos un
    // flag mal escrito; tragarse un VALOR mal escrito y arrancar con otra
    // cosa es la incoherencia contraria.
    let keymap = norte_ui_host::keys::keymap_de_preset_con(&preset, EFECTOS).map_err(|_| {
        StartupError::Desconocido {
            que: "preset",
            valor: preset.clone(),
        }
    })?;
    // El visor es otra PANTALLA, con el mismo preset: `esc` cierra y `e`
    // recarga con otro encoding porque eso es lo que dice el preset, no
    // porque el renderer lo decida.
    let keymap_viewer = norte_ui_host::keys::keymap_visor_de_preset(&preset).map_err(|_| {
        StartupError::Desconocido {
            que: "preset",
            valor: preset.clone(),
        }
    })?;

    // De la línea de órdenes se exige que exista; de la CONFIGURACIÓN se cae
    // a la de siempre, que es lo que el usuario tenía antes de escribir la
    // clave (la misma regla que el TUI). La diferencia es quién lo acaba de
    // teclear.
    let layout = if let Some(l) = &cli.layout {
        let nombre = nombre_de(l, "--layout")?;
        norte_frontend::layout::presets::tree(&nombre).map_err(|_| StartupError::Desconocido {
            que: "--layout",
            valor: nombre,
        })?
    } else {
        let nombre = cfg
            .common
            .ui_layout
            .clone()
            .unwrap_or_else(|| "orthodox".to_owned());
        norte_frontend::layout::presets::tree(&nombre)
            .or_else(|_| norte_frontend::layout::presets::tree("orthodox"))
            .map_err(|e| StartupError::Config(e.to_string()))?
    };

    let columnas = norte_frontend::columns::ColumnsSettings::resolve(&cfg.common.ui_columns);
    // Un id de columna que no parsea no desaparece en silencio: `doctor` lo
    // reporta, y aquí al menos queda en el log de arranque.
    for malo in &columnas.invalid {
        tracing::warn!(columna = %malo, "id de columna inválido: se ignora");
    }
    let (paths, user_layouts) = diagnostico(&capas_vistas, &socket).await;
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
        // Los ajustes ENTEROS: las columnas se configuran por esquema, y
        // resolverlas aquí para `file` dejaba muerta esa mitad de la
        // configuración en cuanto un panel navegaba a un `sftp://`.
        columns: columnas,
        effects: EFECTOS,
        settings: cfg.clone(),
        paths,
        theme: tema_visto(tema_resuelto.as_deref(), &theme),
        user_layouts,
    })
    .await?;
    Ok(Boot {
        host,
        snapshot,
        lang,
        theme,
    })
}

/// Un valor de la línea de órdenes que TIENE que ser texto para poder
/// compararlo con una lista de nombres conocidos.
///
/// Los bytes se conservan hasta aquí (`OsString`) y la conversión falla en
/// vez de colapsar: dos nombres inválidos distintos no pueden acabar siendo
/// el mismo (#246).
fn nombre_de(v: &std::ffi::OsStr, que: &'static str) -> Result<String, StartupError> {
    v.to_str()
        .map(str::to_owned)
        .ok_or_else(|| StartupError::Desconocido {
            que,
            valor: v.to_string_lossy().into_owned(),
        })
}

fn tema(nombre: Option<&str>) -> (Theme, Option<String>) {
    match nombre {
        Some(n) => match Theme::preset(n).ok().flatten() {
            Some(t) => (t, Some(n.to_owned())),
            // El pedido no cargó. Se devuelve el que SE VA A PINTAR y el
            // nombre del que se va a pintar, no el del que se pidió: la vista
            // del tema existe para ver por dentro el que hay, y titularla con
            // un nombre cuyos colores no son los de debajo es justo lo que
            // esa vista viene a impedir.
            None => (Theme::preset_default(), None),
        },
        None => (Theme::preset_default(), None),
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
        assert_eq!(cli.layout.as_deref(), Some(std::ffi::OsStr::new("simple")));
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

    /// Dos nombres de disposición con bytes DISTINTOS no pueden acabar
    /// siendo el mismo: con una conversión lossy los dos colapsaban a
    /// `caf\u{FFFD}` y abrían el mismo fichero (#246).
    #[cfg(unix)]
    #[test]
    fn dos_nombres_invalidos_distintos_siguen_siendo_distintos() {
        use std::os::unix::ffi::OsStringExt as _;
        let uno = std::ffi::OsString::from_vec(b"caf\xff".to_vec());
        let otro = std::ffi::OsString::from_vec(b"caf\xfe".to_vec());
        let a = parse([std::ffi::OsString::from("--layout"), uno]).expect("parsea");
        let b = parse([std::ffi::OsString::from("--layout"), otro]).expect("parsea");
        assert_ne!(a.layout, b.layout, "los bytes se conservan");
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
