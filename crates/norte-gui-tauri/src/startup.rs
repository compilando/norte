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
use std::collections::BTreeMap;

use norte_theme::Theme;
use norte_ui_host::pickers::HostTheme;
use norte_ui_host::settings::{ConfigLayer, HostPath, HostPaths};
use norte_ui_host::{UiHost, UiHostOptions, ViewSnapshot};

/// Hasta dónde llega esta ventana.
///
/// **Completo desde la tarea 5.4** (2026-08-22), que es la revisión de
/// seguridad de las mutaciones que el gate de salida de la fase 5 exige antes
/// de que una compilación de release escriba nada. Hasta entonces fue
/// `SoloLectura`, y no por falta de código: la revisión de la tarea 3.3 había
/// encontrado que el preset ya ataba F7/F8 a crear y borrar y que
/// `Dialog{choice:"approve"}` aprobaba la operación de un agente, o sea que
/// la rebanada «de solo lectura» tenía autoridad destructiva y de policy.
///
/// Lo que sostiene el cambio, y que está probado en `norte-ui-host`:
///
/// - **Ningún operando lo nombra el renderer** (ADR 0070). `UiAction` no
///   lleva un `VPath` ni una cadena que sea una ruta; los orígenes salen de
///   las marcas del hueco enfocado y el destino del hueco con el rol
///   `Target`. Lo único que cruza es texto TECLEADO, que se valida como
///   segmento y se rehúsa si trae el carácter de sustitución.
/// - **Toda mutación pasa por una confirmación** y de ahí a una Task del
///   daemon: journal, tablero, cancelación y relistado. Los caminos a
///   `backend.delete/copy/move_/mkdir/rename_batch` son DOS y los dos exigen
///   una pantalla contestada: `ejecutar_pendiente` (los diálogos) y
///   `aprobar_revision_ia` (la revisión de un plan, que además exige el
///   `plan_hash` que devolvió el core y haber leído el plan entero).
/// - **Levantar esto también habilita `pane.ai-rename`**, que manda el
///   contenido del directorio a un modelo externo. No escribe, pero sale del
///   proceso, y por eso está en la lista de lo que solo lectura quita.
/// - **La decisión de una aprobación no tiene respuesta implícita**: solo
///   `approve` aprueba, el diálogo se abre sin reconocer —la primera tecla
///   solo dice «ya lo veo»—, enseña su TTL, se cierra al vencer, y si el
///   `policy.decide` no llega al daemon se DICE.
/// - **La superficie de la webview sigue siendo la de siempre**: cuatro
///   comandos, CSP sin `eval` ni orígenes remotos, capacidades mínimas, sin
///   filesystem ni shell, y `tests/webview_boundary.rs` lo clava.
pub const EFECTOS: norte_ui_host::commands::Efectos = norte_ui_host::commands::Efectos::Completo;

/// La ayuda de la línea de comandos. Corta a propósito: lo que esta ventana
/// sabe hacer se documenta DENTRO (F1), no en un `--help`.
pub const USAGE: &str = "\
norte-gui — el renderer gráfico de norte

USO:
    norte-gui [DIR] [OPCIONES]

ARGUMENTOS:
    DIR                  Directorio de arranque (por defecto, el actual)

OPCIONES:
    --socket <RUTA>      Socket del daemon (por defecto, el del sistema)
    --layout <NOMBRE>    Disposición de arranque (por defecto, la de la config)
    --preset <NOMBRE>    Preset de teclado (por defecto, el de la config)
    --no-splash          Sin pantalla de inicio en este arranque
    --profile <NOMBRE>   Perfil de configuración (por defecto, ninguno)
    --attach             Recoge la pantalla que la terminal acaba de entregar
                         (`app.handoff`), marcas incluidas
    -h, --help           Esta ayuda
    -V, --version        La versión
";

/// El comando que arranca el daemon, o `None` si no hay binario que lanzar.
///
/// **`norte-gui` no sabe ser daemon**, al revés que el CLI: aquel usa su
/// propio `current_exe` porque el mismo ejecutable trae el subcomando. Aquí
/// hay que encontrar a `norte`, y el orden importa:
///
/// 1. **El hermano**: `norte` en el mismo directorio que este ejecutable. Es
///    lo determinista — el par que se instaló junto — y funciona con
///    `just link`, donde los dos symlinks apuntan al mismo `target/debug`
///    (`current_exe` ya resuelve el enlace, así que el hermano es el del
///    árbol y no el de `~/.local/bin`).
/// 2. **El `PATH`**, como último recurso.
///
/// Nunca el directorio de trabajo: ahí el binario lo elige quien haya dejado
/// un fichero, y esto lanza un proceso. El hermano no añade riesgo — quien
/// pueda escribir en el directorio de este ejecutable ya controla la ventana
/// que está corriendo.
///
/// `None` deja el arranque como estaba: se intenta conectar y, si no hay
/// nadie, se dice.
async fn comando_de_daemon(socket: &std::path::Path) -> Option<Vec<std::ffi::OsString>> {
    let socket = socket.to_path_buf();
    // Sondas de FS fuera del runtime (regla 2).
    tokio::task::spawn_blocking(move || {
        let junto_a = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf));
        // El argv es el compartido: lo que arranca esta ventana se apaga
        // solo cuando su último cliente se va.
        Some(norte_client::daemon_run_argv(
            programa_del_daemon(junto_a.as_deref()),
            &socket,
        ))
    })
    .await
    .ok()
    .flatten()
}

/// Qué `norte` se va a lanzar: el HERMANO del ejecutable si está, y si no el
/// del `PATH`.
///
/// Separada de [`comando_de_daemon`] para que se pueda probar. Es la promesa
/// que sostiene el paquete —`norte-gui`, `norte` y `ntc` viajan juntos y la
/// ventana encuentra al suyo (#256)— y hasta ahora sólo se podía comprobar
/// instalando, que es cuando ya es tarde. Lo que no se puede probar aquí es
/// `current_exe`, y por eso el directorio entra como argumento.
fn programa_del_daemon(junto_a: Option<&std::path::Path>) -> std::ffi::OsString {
    junto_a
        .map(|d| d.join("norte"))
        .filter(|p| p.is_file())
        .map_or_else(|| "norte".into(), Into::into)
}

#[cfg(test)]
mod prueba_del_daemon {
    use super::programa_del_daemon;

    /// Con un `norte` al lado, se lanza ESE y con su ruta completa.
    ///
    /// Es lo que hace que el paquete funcione: en una instalación limpia el
    /// `PATH` puede no tener nada, y el hermano sí está.
    #[test]
    fn el_hermano_gana() {
        let dir = tempfile::tempdir().expect("temp");
        let hermano = dir.path().join("norte");
        std::fs::write(&hermano, b"#!/bin/sh\n").expect("se escribe");
        assert_eq!(programa_del_daemon(Some(dir.path())), hermano.as_os_str());
    }

    /// Sin hermano se cae al `PATH`, que es el caso del árbol de desarrollo.
    #[test]
    fn sin_hermano_se_cae_al_path() {
        let dir = tempfile::tempdir().expect("temp");
        assert_eq!(programa_del_daemon(Some(dir.path())), "norte");
        assert_eq!(programa_del_daemon(None), "norte");
    }

    /// Un DIRECTORIO llamado `norte` no es un daemon: se ignora.
    ///
    /// Sin el `is_file` se lanzaría un directorio como si fuera un programa y
    /// el fallo saldría como «no se pudo conectar», que no dice nada.
    #[test]
    fn un_directorio_no_es_un_daemon() {
        let dir = tempfile::tempdir().expect("temp");
        std::fs::create_dir(dir.path().join("norte")).expect("se crea");
        assert_eq!(programa_del_daemon(Some(dir.path())), "norte");
    }
}

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
    /// El daemon se arrancó y MURIÓ, con lo que él dijo.
    ///
    /// Separado de [`StartupError::Connect`] porque el consejo es el
    /// contrario: aquello invita a comprobar si hay un daemon, y esto a leer
    /// una frase que ya explica el problema. Reintentar no lo arregla.
    #[error("el daemon no pudo arrancar{}:\n{}",
        match .status { Some(c) => format!(" (salió con {c})"), None => String::new() },
        if .stderr.is_empty() { "y no dijo por qué" } else { .stderr })]
    DaemonMuerto {
        /// Código de salida, si lo hubo (`None` = lo mató una señal).
        status: Option<i32>,
        /// Lo que escribió por `stderr`. Puede venir vacío.
        stderr: String,
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
#[expect(
    clippy::struct_excessive_bools,
    reason = "cada bool es un flag INDEPENDIENTE de la línea de órdenes; un enum \
              de dos variantes por flag sería el mismo dato con más ruido"
)]
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
    /// Perfil pedido para ESTE arranque (#307, ADR 0079).
    ///
    /// Bytes intactos por lo mismo que [`Self::layout`], y con más motivo: un
    /// nombre de perfil acaba siendo un DIRECTORIO (`profiles/<nombre>/`).
    pub profile: Option<std::ffi::OsString>,
    /// Sin pantalla de arranque en ESTE arranque, diga lo que diga `[ui]
    /// splash`. Para pilotos y capturas, igual que en el terminal.
    pub no_splash: bool,
    /// Esta ventana es el otro extremo de un RELEVO (`--attach`, fase 9): además
    /// de la pantalla, reclama lo MARCADO que la terminal dejó en la sesión.
    ///
    /// Sin él un arranque es un arranque, y unas marcas de un relevo que se
    /// quedó a medias no resucitan al día siguiente.
    pub attach: bool,
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
    let crudo = norte_frontend::cli::parse(
        args,
        &["--no-splash", norte_frontend::handoff::ATTACH],
        &["--socket", "--layout", "--preset", "--profile"],
    );
    if let Some(flag) = crudo.unknown {
        return Err(StartupError::UnknownFlag(flag));
    }
    Ok(Cli {
        dir: crudo.dir.clone(),
        socket: crudo.path("--socket"),
        layout: crudo.os_text("--layout").map(std::ffi::OsString::from),
        preset: crudo.os_text("--preset").map(std::ffi::OsString::from),
        profile: crudo.os_text("--profile").map(std::ffi::OsString::from),
        no_splash: crudo.has("--no-splash"),
        attach: crudo.has(norte_frontend::handoff::ATTACH),
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
    /// Fuentes y movimiento, de `[ui]`: lo que esta ventana pinta y no es
    /// color. Va aquí y no se relee en `main` porque la configuración ya está
    /// cargada y volver a mirarla sería una segunda lectura que puede diferir.
    pub appearance: crate::catalog::Appearance,
    /// No hay `norte.toml` de usuario (spec 2026-09-10): el catálogo lo
    /// lleva y el renderer abre el asistente de primer arranque.
    pub first_run: bool,
    /// Esta ventana arranca SIN pantalla de inicio, diga lo que diga `[ui]
    /// splash` (ADR 0115): `--no-splash` o `NORTE_NO_SPLASH`.
    ///
    /// Se decide aquí y viaja en el catálogo, como `first_run`: el host no
    /// mira el entorno del proceso ni la línea de órdenes —no son suyos—, y
    /// el renderer solo necesita saber si avisar del arranque o callarse.
    pub no_splash: bool,
    /// `[ui] theme_light` / `theme_dark` ya resueltos a variables (spec
    /// 2026-09-11, V6), o `None` cuando la clave no está o su tema no carga
    /// — entonces la ventana pinta `theme` en ese esquema, y se avisa.
    pub theme_light: Option<BTreeMap<String, String>>,
    /// La variante oscura; ver [`Self::theme_light`].
    pub theme_dark: Option<BTreeMap<String, String>>,
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
fn tema_visto(
    spec: Option<&str>,
    theme: &Theme,
    variante_clara: Option<Theme>,
    variante_oscura: Option<Theme>,
) -> HostTheme {
    HostTheme {
        // En `Box`: `HostTheme` viaja dentro del futuro de arranque, y dos
        // `Theme` inline lo cruzaban el umbral de `large_futures`.
        variante_clara: variante_clara.map(Box::new),
        variante_oscura: variante_oscura.map(Box::new),
        // El RESUELTO, que es el de `theme`. `spec` es lo que se pidió, y con
        // un fichero roto los dos no coinciden.
        name: spec.unwrap_or("default").to_owned(),
        roles: crate::catalog::variables(theme).into_iter().collect(),
        effects: efectos_declarados(theme),
        // El tema ENTERO, que es lo que hace falta para colorear una entrada
        // por su extensión (puente 66): eso no se puede proyectar como
        // variables CSS porque las extensiones son un conjunto abierto.
        resuelto: theme.clone(),
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
/// **`NORTE_LANG` > `[ui] lang` > el entorno del sistema**, que es lo que
/// hace el terminal (`norte-tui/src/main.rs`). Las dos superficies
/// documentaban reglas CONTRARIAS y las dos las cumplían: aquí ganaba la
/// configuración, allí ganaba `NORTE_LANG`, así que con `NORTE_LANG=en` y
/// `lang = "es"` escritos, `ntc` salía en inglés y `norte-gui` en español.
///
/// Manda el terminal porque su regla es la que ya sigue el resto: `NORTE_LANG`
/// es específico de norte y se pone para UNA ejecución, o sea la misma clase
/// de cosa que `--layout`, que gana a `[ui] layout`. `LANG` no: ése es el
/// idioma del sistema, y una decisión escrita en la configuración es más
/// específica que él.
fn idioma(pedido: Option<&str>) -> Lang {
    let explicito = std::env::var("NORTE_LANG").ok().filter(|v| !v.is_empty());
    let lang = elegir_idioma(explicito.as_deref(), pedido, Lang::from_env());
    let _ = norte_i18n::force(lang);
    lang
}

/// La regla de precedencia, sin tocar el entorno.
///
/// Separada para poder probarla: `std::env::set_var` es `unsafe` desde la
/// edición 2024 y la regla 5 lo prohíbe, así que lo que se lee del entorno
/// entra como argumento. Es el mismo arreglo que [`programa_del_daemon`].
fn elegir_idioma(explicito: Option<&str>, config: Option<&str>, del_entorno: Lang) -> Lang {
    match (explicito, config) {
        (Some(e), _) => Lang::negotiate(Some(e)),
        (None, Some(c)) => Lang::negotiate(Some(c)),
        (None, None) => del_entorno,
    }
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
                    norte_config::Layer::Profile => ConfigLayer::Profile,
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

/// Lo que la primera foto tiene que DECIR, si hay algo.
///
/// Va en el mensaje de la foto inicial, que es el equivalente exacto del
/// `app.message` del arranque del terminal: lo pisa la primera acción del
/// lector, no antes.
///
/// El de Lua va el ÚLTIMO y por eso gana: un `lua:` que un repositorio pone
/// en su capa de proyecto se descarta —un repositorio no elige qué código
/// corre una tecla— y ése es el aviso que no puede quedar pisado. El de la
/// capa de proyecto ignorada (#260) es el otro: saltársela en silencio deja
/// al lector con una configuración que cree activa y no lo está.
fn aviso_de_arranque(
    cfg: &norte_frontend::config::FrontendConfig,
    lang: Lang,
    lua_descartadas: usize,
    perfil: Option<&std::ffi::OsStr>,
) -> Option<String> {
    let mut msg = None;
    if !cfg.common.project_warnings.is_empty() {
        for aviso in &cfg.common.project_warnings {
            tracing::warn!(motivo = %aviso, "capa de proyecto ignorada");
        }
        msg = Some(norte_i18n::ta_in(
            lang,
            "msg-project-config-skipped",
            &[("n", &cfg.common.project_warnings.len().to_string())],
        ));
    }
    // Las líneas del PERFIL que no se entienden, con el mismo reparto: el
    // conteo a la barra y el motivo al registro. Sin esto, ser estricto con
    // `[profile.start]` era una trampa — se escribe `/tmp`, la línea se tira y
    // el hueco abre donde le parece sin que nada lo diga (ADR 0098, D5).
    if !cfg.common.profile_warnings.is_empty() {
        for aviso in &cfg.common.profile_warnings {
            tracing::warn!(motivo = %aviso, "línea del perfil ignorada");
        }
        msg = Some(norte_i18n::ta_in(
            lang,
            "msg-profile-config-ignored",
            &[
                (
                    "profile",
                    &perfil
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                ),
                ("n", &cfg.common.profile_warnings.len().to_string()),
            ],
        ));
    }
    if lua_descartadas > 0 {
        msg = Some(norte_i18n::ta_in(
            lang,
            "msg-lua-keymap-project",
            &[("n", &lua_descartadas.to_string())],
        ));
    }
    msg
}

/// Los tres keymaps EFECTIVOS: preset de fábrica más las capas del usuario.
///
/// SOLO LECTURA hasta la fase 5. El preset ata F7/F8 a crear y borrar, y que
/// la tecla exista no es permiso: los comandos que escriben no entran en el
/// keymap efectivo —la tecla se responde «aquí no» en vez de quedarse muda— y
/// el host los rechaza aunque llegaran por otra vía.
///
/// CON las capas del usuario, igual que el terminal: sin ellas un
/// `keymap.toml` con rebinds se ignoraba en silencio aquí mientras el otro
/// frontend sí lo honraba (#253). `UiHostOptions::keymap` documenta que lo
/// que recibe ya viene fusionado, y fusionarlo es trabajo de quien lee disco.
///
/// El visor y los diálogos son otras PANTALLAS con el mismo preset: `esc`
/// cierra y `e` recarga con otro encoding porque lo dice el preset, no porque
/// el renderer lo decida.
///
/// Un preset que no existe se DICE: el mismo fichero rechaza a gritos un flag
/// mal escrito, y tragarse un VALOR mal escrito para arrancar con otra cosa
/// sería la incoherencia contraria.
fn keymaps(
    preset: &str,
    cfg: &norte_frontend::config::FrontendConfig,
) -> Result<
    (
        norte_frontend::keymap::Effective,
        norte_frontend::keymap::Effective,
        norte_frontend::keymap::Effective,
    ),
    StartupError,
> {
    let keymap =
        norte_ui_host::keys::keymap_de_preset_con_capas(preset, &cfg.keymap_layers, EFECTOS)
            .map_err(|_| StartupError::Desconocido {
                que: "preset",
                valor: preset.to_owned(),
            })?;
    let (visor, dialogo) = otras_pantallas(preset, &cfg.keymap_layers)?;
    Ok((keymap, visor, dialogo))
}

/// cambie esta ventana igual que el TUI (#287).
fn otras_pantallas(
    preset: &str,
    capas: &[norte_frontend::keymap::KeymapFile],
) -> Result<
    (
        norte_frontend::keymap::Effective,
        norte_frontend::keymap::Effective,
    ),
    StartupError,
> {
    let desconocido = || StartupError::Desconocido {
        que: "preset",
        valor: preset.to_owned(),
    };
    let visor = norte_ui_host::keys::keymap_visor_de_preset_con_capas(preset, capas)
        .map_err(|_| desconocido())?;
    let dialogo = norte_ui_host::keys::keymap_dialogo_de_preset_con_capas(preset, capas)
        .map_err(|_| desconocido())?;
    Ok((visor, dialogo))
}

/// Las capas de configuración con el perfil que el lector NOMBRÓ metido
/// dentro (#307, ADR 0079 D7).
///
/// Y si ese perfil no se puede usar, esto FALLA: pediste ese perfil, y
/// arrancar como otra cosa sería contestar otra pregunta. El nombre tiene que
/// estar en el LISTADO, byte a byte — mirar solo si el resolutor produjo una
/// capa no basta, porque la añade en cuanto el nombre es legal y hay
/// directorio de usuario, exista o no; entonces la carga la trata como una
/// capa ausente, que no es un error, y `--profile fantasma` arrancaba como si
/// nada. Es la misma comprobación, palabra por palabra, que hace el terminal.
fn capas_con_perfil(nombre: &std::ffi::OsStr) -> Result<norte_config::Layers, StartupError> {
    let dir = norte_config::profiles_dir_from(&|k| std::env::var_os(k)).ok_or_else(|| {
        StartupError::Config("no hay directorio de configuración donde colgar un perfil".to_owned())
    })?;
    capas_con_perfil_en(&dir, nombre)
}

/// El núcleo probable de [`capas_con_perfil`]: el directorio de perfiles entra
/// como ARGUMENTO, para que su test no dependa del `HOME` de quien lo corra.
fn capas_con_perfil_en(
    dir: &std::path::Path,
    nombre: &std::ffi::OsStr,
) -> Result<norte_config::Layers, StartupError> {
    let hay = norte_config::list_profiles(dir)
        .unwrap_or_default()
        .iter()
        .any(|n| n == nombre);
    if !hay {
        return Err(StartupError::Desconocido {
            que: "--profile",
            valor: nombre.to_string_lossy().into_owned(),
        });
    }
    Ok(norte_config::standard_layers_with_profile(Some(nombre)))
}

/// Monta el host: configuración, socket, directorio, keymap y disposición.
///
/// **Es una SECUENCIA, y por eso crece un paso por cosa que haya que montar.**
/// Cada línea es un nombre y una llamada, en el único orden en que se pueden
/// hacer: el log necesita la configuración, el keymap necesita el preset, el
/// host los necesita a todos. Partirla para bajar del umbral del lint mete el
/// orden en dos sitios y deja al lector reconstruyéndolo — y el orden es lo
/// único delicado que hay aquí. Mismo trato que `aplicar_efecto` en el host.
///
/// # Errors
/// [`StartupError`] si la configuración no carga, el directorio no vale, o no
/// hay daemon al otro lado.
#[expect(
    clippy::too_many_lines,
    reason = "secuencia de arranque: un paso por línea, y el orden es el contrato"
)]
pub async fn boot(cli: &Cli) -> Result<Boot, StartupError> {
    // Las MISMAS capas que el TUI, leídas fuera del runtime (regla 2) — y con
    // el perfil que `--profile` nombre metido YA en ellas (#307, ADR 0079).
    //
    // En la PRIMERA carga y no por el cambio en caliente, igual que en el
    // terminal: así aplica hasta `[ui] lang`, que es lo único que un cambio en
    // marcha no puede deshacer (`norte_i18n::force` corre una vez). El perfil
    // PEGAJOSO no puede hacer esto —vive en la sesión, y la sesión la tiene el
    // daemon, al que se llega con la configuración que estamos cargando— y por
    // eso llega por el otro camino.
    let capas = match &cli.profile {
        Some(nombre) => capas_con_perfil(nombre)?,
        None => norte_config::standard_layers(),
    };
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
    // Las variantes por esquema del escritorio (V6): cada una se resuelve
    // como `theme` y viaja ya como variables. Una clave puesta cuyo tema no
    // carga se queda SIN variante —no con el de fábrica—, para que la
    // ventana pinte `theme` en ese lado y el fallo no se disfrace de tema.
    //
    // Se guarda el `Theme` ENTERO además de sus variables: las variables las
    // enchufa el renderer, pero el color de una entrada por `[files.ext]`
    // (puente 66) lo resuelve el HOST, y no cabe en variables porque las
    // extensiones son un conjunto abierto. Resolviendo siempre contra `[ui]
    // theme`, un escritorio en claro pintaba el cromo con la variante clara y
    // los NOMBRES con los colores de la oscura.
    let variante = |nombre: Option<&str>| -> Option<Theme> {
        let n = nombre?;
        match norte_frontend::theme::resolve_theme(Some(n)) {
            Ok(t) => Some(t),
            Err(e) => {
                tracing::warn!(error = %e, tema = n, "la variante de tema no cargó: se ignora");
                None
            }
        }
    };
    let tema_claro = variante(cfg.common.ui_theme_light.as_deref());
    let tema_oscuro = variante(cfg.common.ui_theme_dark.as_deref());
    let theme_light: Option<BTreeMap<String, String>> =
        tema_claro.as_ref().map(crate::catalog::variables);
    let theme_dark: Option<BTreeMap<String, String>> =
        tema_oscuro.as_ref().map(crate::catalog::variables);
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
    // su proceso (decisión D10). Lo que sí hace es ARRANCARLO si no hay
    // ninguno, igual que el `--daemon` del CLI: exigir que el lector abra un
    // terminal antes de poder abrir una ventana no es una decisión de
    // arquitectura, es una tarea que se le queda al lector.
    //
    // Y se conecta con `connect_detallado` a propósito: cuando el daemon
    // arranca y MUERE —un journal que no se puede migrar es el caso real—, la
    // única frase que dice qué hacer la escribe él por `stderr`, y la
    // taxonomía del wire no tiene dónde ponerla. Sin esto, la ventana decía
    // «no se pudo conectar (retryable: true)», o sea «espera», sobre algo que
    // no iba a llegar nunca.
    let arranque = comando_de_daemon(&socket).await;
    let backend = RemoteBackend::connect_detallado(
        socket.clone(),
        arranque,
        ClientInfo {
            name: "norte-gui".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        },
    )
    .await
    .map_err(|e| match e {
        norte_client::ClientError::SpawnFailed { status, stderr } => {
            StartupError::DaemonMuerto { status, stderr }
        }
        otro => StartupError::Connect {
            socket: socket.display().to_string(),
            source: norte_client::to_taxonomy(otro),
        },
    })?;

    // El log, DESPUÉS de cargar la config porque `[log] dir` y `[log] retain`
    // salen de ella —la ventana los ignoraba— y antes que nada más, para que
    // lo que falle a partir de aquí deje rastro. Misma regla que el terminal:
    // un `--help` sale antes y no escribe nada, que es lo correcto.
    // #326: el log va TAMBIÉN a un anillo en memoria, que es lo que pinta el
    // panel de registro. El fichero sirve para investigar después; el anillo,
    // para ver lo que está pasando sin salir de la ventana.
    //
    // Ojo con lo que este anillo NO lleva: la ventana arranca su propio daemon
    // (#300), así que aquí solo están las líneas de ESTE proceso — los
    // providers, el journal y la política registran en el suyo. El panel lo
    // dice; callarlo haría que pareciera roto.
    let log_ring = logging(&cfg);

    let preset = if let Some(p) = &cli.preset {
        nombre_de(p, "--preset")?
    } else {
        cfg.common.preset.clone()
    };
    let (keymap, keymap_viewer, keymap_dialog) = keymaps(&preset, &cfg)?;
    // Un `lua:` de la capa de PROYECTO se descarta —un repositorio no elige
    // qué código corre una tecla—, y se DICE, como en el terminal: un
    // descarte silencioso es una tecla que no hace lo que su fichero dice.
    let capas_lua_descartadas = keymap
        .discarded_lua_bindings()
        .max(keymap_viewer.discarded_lua_bindings())
        .max(keymap_dialog.discarded_lua_bindings());

    // De la línea de órdenes se exige que exista; de la CONFIGURACIÓN se cae
    // a la de siempre, que es lo que el usuario tenía antes de escribir la
    // clave (la misma regla que el TUI). La diferencia es quién lo acaba de
    // teclear.
    //
    // El fichero se lee FUERA del runtime (regla 2), como en el TUI: es un
    // TOML pequeño, pero leerlo con `std::fs` dentro de un `async fn` es I/O
    // bloqueante igual.
    let (layout, layout_roto) = {
        let cli_layout = cli.layout.clone();
        let cfg_layout = cfg.common.ui_layout.clone();
        let dir = norte_config::user_config_dir();
        tokio::task::spawn_blocking(move || {
            arbol_de_arranque(cli_layout.as_deref(), cfg_layout.as_deref(), dir.as_deref())
        })
        .await
        .map_err(|e| StartupError::Config(e.to_string()))??
    };
    // Un fichero roto NO deja sin pantalla —queda el preset— pero tampoco se
    // calla: un layout que no parsea y desaparece en silencio es una
    // configuración que el lector cree puesta. Va al LOG y a la barra de
    // estado, como en el TUI: el log solo no lo lee nadie que esté mirando
    // una disposición que no pidió.
    let aviso_layout = layout_roto.map(|e| {
        tracing::warn!(error = %e, "la disposición del usuario no cargó: queda la de fábrica");
        let nombre = cli.layout.clone().unwrap_or_else(|| {
            std::ffi::OsString::from(cfg.common.ui_layout.as_deref().unwrap_or("orthodox"))
        });
        norte_i18n::ta_in(
            lang,
            "msg-layout-load-failed",
            &[("name", &layout_pintable(&nombre)), ("err", &e.to_string())],
        )
    });

    let columnas = norte_frontend::columns::ColumnsSettings::resolve(&cfg.common.ui_columns)
        .with_date_format(cfg.common.ui_chrome.date_format());
    // Un id de columna que no parsea no desaparece en silencio: `doctor` lo
    // reporta, y aquí al menos queda en el log de arranque.
    for malo in &columnas.invalid {
        tracing::warn!(columna = %malo, "id de columna inválido: se ignora");
    }
    let (paths, user_layouts) = diagnostico(&capas_vistas, &socket).await;
    let (host, snapshot) = UiHost::start(UiHostOptions {
        backend: Arc::new(backend),
        initial_dir: inicio,
        // Lo escribió un humano, así que gana a la sesión en el panel activo
        // — la misma regla que el terminal cerró en `eb237c61`.
        initial_dir_pedido: cli.dir.is_some(),
        attach: cli.attach,
        locale: match lang {
            Lang::Es => "es".to_owned(),
            Lang::En => "en".to_owned(),
        },
        keymap,
        keymap_viewer,
        keymap_dialog,
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
        theme: tema_visto(tema_resuelto.as_deref(), &theme, tema_claro, tema_oscuro),
        user_layouts,
        // Ya está APLICADO en `settings` (sus capas entraron arriba); esto es
        // para que el host lo sepa y el selector lo marque puesto (#307).
        profile: cli.profile.clone(),
        log_ring,
    })
    .await?;
    let mut snapshot = snapshot;
    // El de la disposición va PRIMERO si lo hay: los otros dos avisan de una
    // capa ignorada, y éste de que la pantalla que se está mirando no es la
    // pedida — que es lo que el lector no puede deducir solo.
    snapshot.status.message = aviso_layout
        .or_else(|| aviso_de_arranque(&cfg, lang, capas_lua_descartadas, cli.profile.as_deref()));
    // El asistente de primer arranque (spec 2026-09-10): sin `norte.toml` de
    // usuario y sin `NORTE_NO_WIZARD`, como en el terminal. Un `stat`, fuera
    // del hilo de la UI (regla 2).
    let first_run = if std::env::var_os("NORTE_NO_WIZARD").is_some() {
        false
    } else if let Some(dir) = norte_config::user_config_dir() {
        !tokio::fs::try_exists(dir.join("norte.toml"))
            .await
            .unwrap_or(true)
    } else {
        false
    };
    // Sin pantalla de inicio en ESTE arranque (ADR 0115): la bandera, o la
    // variable que usan los pilotos y las capturas. Misma pareja que el
    // terminal, porque una ventana que ignora `NORTE_NO_SPLASH` deja una
    // pantalla encima de cada captura automática.
    let no_splash = cli.no_splash || std::env::var_os("NORTE_NO_SPLASH").is_some();
    Ok(Boot {
        host,
        snapshot,
        lang,
        theme,
        appearance: crate::catalog::Appearance::de(&cfg.common),
        first_run,
        no_splash,
        theme_light,
        theme_dark,
    })
}

/// El nombre de un layout, PINTABLE: lossy marcado y hazards enmascarados.
///
/// Solo para mensajes. Los bytes no se tocan —los usa el cargador—, y esto es
/// lo mismo que hace el TUI en `App::apply_loaded_layout`: sin la máscara, un
/// `--layout $'a\x1b[31mb'` deja la secuencia CRUDA en `norte-gui.log`, y sin
/// la marca `$'\xff'` y `$'\xfe'` dan el mismo mensaje.
fn layout_pintable(name: &std::ffi::OsStr) -> String {
    let (showable, lossy) = norte_frontend::display_os_name(name);
    let showable = norte_encoding::mask_terminal_hazards(&showable);
    if lossy {
        format!("! {showable}")
    } else {
        showable
    }
}

/// La disposición con la que arranca la ventana, y el aviso si el fichero del
/// usuario estaba roto.
///
/// Dos fuentes y dos criterios: de `--layout` se exige que exista, porque lo
/// acaba de teclear un humano; de `[ui] layout` se cae a `orthodox`, que es lo
/// que se tenía antes de escribir la clave.
///
/// **La ventana es más estricta que el TUI en la primera**, y a propósito: el
/// TUI avisa por la barra y sigue, porque ya tiene una pantalla puesta cuando
/// eso ocurre; aquí no hay todavía nada que enseñar, y arrancar con una
/// disposición que no es la pedida es peor que decir que no existe. Lo que sí
/// se comparte es la REGLA de resolución (`or_preset`) y el motivo: un fichero
/// ROTO no se anuncia como «no existe», se anuncia con su error de parseo.
///
/// Dentro de cada fuente, el fichero del usuario gana al preset de fábrica
/// —[`norte_frontend::layout::config::or_preset`] es esa regla, compartida—.
/// Antes esto miraba SOLO los presets, así que un layout guardado no se podía
/// pedir por la línea de órdenes aunque el selector de esta misma ventana lo
/// ofreciera.
///
/// El nombre viaja como [`std::ffi::OsStr`] y no como `String` (#246): es un
/// nombre de FICHERO, y colapsar sus bytes manda a `layouts/\u{fffd}.toml` a
/// dos nombres inválidos distintos.
///
/// Lee del disco: va bajo `spawn_blocking`.
fn arbol_de_arranque(
    cli: Option<&std::ffi::OsStr>,
    config: Option<&str>,
    dir: Option<&std::path::Path>,
) -> Result<
    (
        norte_frontend::layout::Node,
        Option<norte_frontend::layout::LayoutError>,
    ),
    StartupError,
> {
    use norte_frontend::layout::{LayoutError, config};

    let leer = |name: &std::ffi::OsStr| {
        dir.map_or_else(
            || Err(LayoutError::NotFound(String::new())),
            |d| config::load(d, name),
        )
    };
    if let Some(name) = cli {
        return config::or_preset(name, leer(name)).map_err(|e| match e {
            // No hay fichero ni preset con ese nombre: es un valor que no
            // existe, y eso es lo que se dice.
            LayoutError::NotFound(_) | LayoutError::BadName(_) => StartupError::Desconocido {
                que: "--layout",
                valor: layout_pintable(name),
            },
            // El fichero SÍ está y no sirve. Anunciarlo como «no existe»
            // manda al lector a buscar un nombre que ya tiene bien escrito:
            // lo que necesita es el error de parseo.
            otro => StartupError::Config(format!("--layout {}: {otro}", layout_pintable(name))),
        });
    }
    let name = std::ffi::OsString::from(config.unwrap_or("orthodox"));
    match config::or_preset(&name, leer(&name)) {
        Ok(v) => Ok(v),
        // La clave nombra algo que no existe: se sigue con la de siempre, que
        // es lo que el lector tenía antes de escribirla.
        Err(e) => Ok((
            norte_frontend::layout::presets::tree("orthodox")
                .map_err(|x| StartupError::Config(x.to_string()))?,
            Some(e),
        )),
    }
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

/// El tema pedido, y el nombre del que se va a pintar.
///
/// Por `resolve_theme`, que es el resolutor COMPARTIDO: acepta el nombre de
/// un preset **o la ruta a un `.toml`** (ADR 0020). Aquí se llamaba a
/// `Theme::preset` a secas, así que un `theme = "~/.config/norte/mio.toml"`
/// tematizaba el terminal y dejaba la ventana con la paleta por defecto, sin
/// decir nada — la misma forma que tenía el bug de `--layout`.
///
/// Lee del disco cuando el spec es una ruta: va bajo `spawn_blocking`.
///
/// Devuelve el nombre del que SE VA A PINTAR y no el del que se pidió: la
/// vista del tema existe para ver por dentro el que hay, y titularla con un
/// nombre cuyos colores no son los de debajo es justo lo que esa vista viene
/// a impedir. Un tema de fichero no tiene nombre de preset, así que va con el
/// suyo propio si lo declara.
fn tema(nombre: Option<&str>) -> (Theme, Option<String>) {
    let Some(n) = nombre else {
        return (Theme::preset_default(), None);
    };
    match norte_frontend::theme::resolve_theme(Some(n)) {
        Ok(t) => {
            // Un preset se titula con el nombre pedido; uno de fichero, con
            // el que el propio fichero declare.
            let titulo = Theme::preset(n)
                .ok()
                .flatten()
                .map_or_else(|| t.name.clone(), |_| Some(n.to_owned()));
            (t, titulo)
        }
        Err(e) => {
            tracing::warn!(error = %e, "el tema pedido no cargó: queda el de fábrica");
            (Theme::preset_default(), None)
        }
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
    norte_vfs::native::vpath_from_native(&nativo)
        .map_err(|e| StartupError::Dir(format!("{}: {e}", nativo.display())))
}

/// El log va al FICHERO y solo al fichero.
///
/// El MONTAJE es el compartido (`norte_config::logging`): rotación diaria,
/// directorio 0700 y ficheros 0600, retención acotada y el cap de seguridad
/// de `suppaftp`. Aquí se montaba a mano —porque el helper vivía en el core y
/// este binario habla con el daemon por un socket—, y lo que costó fue que la
/// copia se dejó el endurecimiento: un log legible por cualquier cuenta local
/// con las rutas por las que el usuario había navegado (#255). El helper vive
/// ahora en `norte-config`, que ya era dueño de `state_dir()` y de `[log]`.
///
/// El prefijo SÍ es propio: el daemon y esta ventana pueden estar vivos a la
/// vez, y compartir fichero de rotación haría que la retención de uno podase
/// los ficheros del otro.
fn logging(cfg: &norte_frontend::config::FrontendConfig) -> Option<norte_config::logring::LogRing> {
    norte_config::logging::init_to_file_with_ring(
        norte_config::logging::LogConfig {
            dir: cfg.common.log_dir.as_deref(),
            retain: cfg.common.log_retain,
            prefix: Some("norte-gui.log"),
        },
        norte_config::logring::RING_DEFAULT,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `NORTE_LANG` > `[ui] lang` > el entorno del sistema.
    ///
    /// Las dos superficies documentaban reglas CONTRARIAS y las dos las
    /// cumplían: la ventana daba la razón a la configuración y el terminal a
    /// `NORTE_LANG`, así que con las dos puestas `ntc` salía en un idioma y
    /// `norte-gui` en otro. Manda el terminal: `NORTE_LANG` es específico de
    /// norte y se pone para UNA ejecución, o sea la misma clase de cosa que
    /// `--layout`, que gana a `[ui] layout`.
    #[test]
    fn norte_lang_gana_a_la_config_y_la_config_al_entorno() {
        assert_eq!(
            elegir_idioma(Some("en"), Some("es"), Lang::Es),
            Lang::En,
            "lo que se puso para esta ejecución manda"
        );
        assert_eq!(
            elegir_idioma(None, Some("es"), Lang::En),
            Lang::Es,
            "y una decisión escrita manda sobre el idioma del sistema"
        );
        assert_eq!(
            elegir_idioma(None, None, Lang::En),
            Lang::En,
            "sin nada, el sistema"
        );
    }

    /// `[ui] theme` acepta la RUTA a un `.toml`, no solo un preset (ADR 0020).
    ///
    /// La ventana llamaba a `Theme::preset` a secas, así que un tema propio
    /// tematizaba el terminal y dejaba la ventana con la paleta por defecto,
    /// sin decir nada.
    #[test]
    fn el_tema_puede_ser_un_fichero() {
        let dir = tempfile::tempdir().expect("tmp");
        let ruta = dir.path().join("mio.toml");
        std::fs::write(&ruta, "name = \"mío\"\n").expect("write");
        let (t, titulo) = tema(Some(&ruta.to_string_lossy()));
        assert_eq!(
            t.name.as_deref(),
            Some("mío"),
            "se cargó el tema del fichero"
        );
        assert_eq!(
            titulo.as_deref(),
            Some("mío"),
            "y se titula con el nombre que el fichero declara"
        );
    }

    /// Un preset sigue siendo un preset, y se titula con el nombre pedido.
    #[test]
    fn un_preset_sigue_yendo_por_su_nombre() {
        let (_t, titulo) = tema(Some("nord"));
        assert_eq!(titulo.as_deref(), Some("nord"));
    }

    /// Y un tema que no carga deja el de fábrica, sin nombre: la vista del
    /// tema no puede titularse con unos colores que no son los de debajo.
    #[test]
    fn un_tema_que_no_carga_deja_el_de_fabrica() {
        let (_t, titulo) = tema(Some("/no/existe/ni/de/lejos.toml"));
        assert_eq!(titulo, None);
    }

    fn escribe_layout(dir: &std::path::Path, fichero: &std::ffi::OsStr, texto: &str) {
        let layouts = dir.join(norte_frontend::layout::config::LAYOUTS_DIR);
        std::fs::create_dir_all(&layouts).expect("mkdir");
        std::fs::write(layouts.join(fichero), texto).expect("write");
    }

    /// `--layout mio` abre el fichero del USUARIO, igual que en el TUI.
    ///
    /// La ventana miraba solo los presets de fábrica, así que un layout
    /// guardado no se podía pedir por la línea de órdenes — y esta misma
    /// ventana lo ofrece en su selector, o sea que la lista y la opción
    /// decían cosas distintas sobre el mismo fichero.
    #[test]
    fn el_layout_de_la_linea_de_ordenes_puede_ser_del_usuario() {
        let dir = tempfile::tempdir().expect("tmp");
        escribe_layout(
            dir.path(),
            std::ffi::OsStr::new("mio.toml"),
            "[slot]\nid = 1\nkind = \"browser\"\n",
        );
        let (arbol, aviso) =
            arbol_de_arranque(Some(std::ffi::OsStr::new("mio")), None, Some(dir.path()))
                .expect("carga el del usuario");
        assert_eq!(
            arbol.slot_ids().len(),
            1,
            "el del fichero, de un solo hueco"
        );
        assert!(aviso.is_none());
    }

    /// Y uno del usuario que se llama como un preset GANA al preset, que es
    /// la regla del resto de la configuración.
    #[test]
    fn el_fichero_del_usuario_gana_al_preset_del_mismo_nombre() {
        let dir = tempfile::tempdir().expect("tmp");
        escribe_layout(
            dir.path(),
            std::ffi::OsStr::new("simple.toml"),
            "[slot]\nid = 1\nkind = \"browser\"\n",
        );
        let (arbol, _) =
            arbol_de_arranque(Some(std::ffi::OsStr::new("simple")), None, Some(dir.path()))
                .expect("carga");
        assert_eq!(
            arbol.slot_ids().len(),
            1,
            "el `simple` de fábrica tiene tres huecos: éste es el del usuario"
        );
    }

    /// Un nombre que no es UTF-8 es un nombre de fichero como cualquier otro
    /// (#246): se busca, no se rechaza de entrada.
    #[test]
    fn un_nombre_de_layout_que_no_es_utf8_se_busca_igual() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().expect("tmp");
        let nombre = std::ffi::OsStr::from_bytes(b"\xff\xfe");
        let mut fichero = nombre.to_os_string();
        fichero.push(".toml");
        escribe_layout(dir.path(), &fichero, "[slot]\nid = 1\nkind = \"browser\"\n");
        let (arbol, _) =
            arbol_de_arranque(Some(nombre), None, Some(dir.path())).expect("carga por bytes");
        assert_eq!(arbol.slot_ids().len(), 1);
    }

    /// De la línea de órdenes se EXIGE que exista; de la configuración se cae
    /// a `orthodox`, que es lo que se tenía antes de escribir la clave.
    #[test]
    fn un_nombre_inventado_falla_en_la_orden_y_cae_en_la_config() {
        let dir = tempfile::tempdir().expect("tmp");
        assert!(
            arbol_de_arranque(Some(std::ffi::OsStr::new("nada")), None, Some(dir.path())).is_err(),
            "lo acaba de teclear un humano: se le dice"
        );
        let (arbol, _) = arbol_de_arranque(None, Some("nada"), Some(dir.path()))
            .expect("la config no deja sin pantalla");
        assert_eq!(
            arbol,
            norte_frontend::layout::presets::tree("orthodox").expect("preset")
        );
    }

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

    /// `--profile` existe también en la ventana (#307), y con los BYTES
    /// intactos: un nombre de perfil acaba siendo un directorio.
    #[test]
    fn el_perfil_se_lee_y_conserva_sus_bytes() {
        let cli = parse(["--profile", "trabajo"]).expect("parsea");
        assert_eq!(
            cli.profile.as_deref(),
            Some(std::ffi::OsStr::new("trabajo"))
        );

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            let crudo = std::ffi::OsStr::from_bytes(b"perf\xffil");
            let cli = parse([std::ffi::OsString::from("--profile"), crudo.to_os_string()])
                .expect("parsea");
            assert_eq!(
                cli.profile.as_deref(),
                Some(crudo),
                "sin pasar por texto: dos bytes inválidos distintos abrirían el mismo directorio"
            );
        }
    }

    /// Y un perfil que no está en el listado ABORTA (ADR 0079, D7): pediste
    /// ese perfil, y arrancar como otra cosa sería contestar otra pregunta.
    #[test]
    fn un_perfil_que_no_existe_no_arranca() {
        let dir = tempfile::tempdir().expect("temp");
        // Sin `profiles/` dentro, así que el listado viene vacío.
        let e =
            capas_con_perfil_en(dir.path(), std::ffi::OsStr::new("fantasma")).expect_err("no vale");
        assert!(
            matches!(&e, StartupError::Desconocido { que, valor } if *que == "--profile" && valor == "fantasma"),
            "{e}"
        );
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

    /// La ventana ACEPTA lo que la terminal le pasa en un relevo (fase 9).
    ///
    /// El test que faltaba, y el bug que lo pidió: la terminal lanzaba
    /// `ntc-gui --attach --daemon`, este parser no conocía ninguno de los dos,
    /// y la ventana salía con código 2 — sin decir nada, porque el relevo le
    /// cierra `stderr`. Se construye con la MISMA función que usa la terminal,
    /// así que un flag nuevo en un lado sin el otro pone esto en rojo.
    #[test]
    fn la_ventana_acepta_el_argv_del_relevo() {
        let cli = parse(norte_frontend::handoff::window_args())
            .expect("la ventana tiene que aceptar lo que el relevo le pasa");
        assert!(cli.attach, "y entender que viene de un relevo");
        // Un arranque cualquiera NO es un relevo.
        assert!(!parse(Vec::<String>::new()).expect("sin flags").attach);
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
