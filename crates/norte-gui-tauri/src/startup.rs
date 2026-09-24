//! Starting up: read the configuration ONCE, resolve where the daemon is and
//! where the listing starts, and mount the host.
//!
//! Neither a new argument parser (`norte_frontend::cli`), nor its own
//! configuration read (`norte_config`), nor a second idea of where the socket
//! lives (`norte_client::default_socket_path`): a frontend that resolves
//! these things its own way is a frontend that starts up somewhere else than
//! the rest (ADR 0066, decision D14).

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

/// How far this window reaches.
///
/// **Full since task 5.4** (2026-08-22), which is the mutation security
/// review that phase 5's exit gate requires before a release build writes
/// anything. Until then it was `SoloLectura` (read-only), and not for lack of
/// code: task 3.3's review had found that the preset already bound F7/F8 to
/// create and delete, and that `Dialog{choice:"approve"}` approved an agent's
/// operation — so the "read-only" slice already had destructive and policy
/// authority.
///
/// What backs the change, and is tested in `norte-ui-host`:
///
/// - **No operand is named by the renderer** (ADR 0070). `UiAction` carries
///   no `VPath` nor a string that is a path; sources come from the focused
///   slot's marks and the destination from the slot with the `Target` role.
///   The only thing that crosses is TYPED text, which is validated as a
///   segment and refused if it carries the substitution character.
/// - **Every mutation goes through a confirmation** and from there to a
///   daemon Task: journal, dashboard, cancellation and relisting. The paths
///   to `backend.delete/copy/move_/mkdir/rename_batch` are TWO and both
///   require an answered screen: `ejecutar_pendiente` (the dialogs) and
///   `aprobar_revision_ia` (an AI plan's review, which also requires the
///   `plan_hash` the core returned and having read the whole plan).
/// - **Turning this on also enables `pane.ai-rename`**, which sends the
///   directory's contents to an external model. It does not write, but it
///   leaves the process, and that is why it is on the list of what read-only
///   takes away.
/// - **An approval's decision has no implicit answer**: only `approve`
///   approves, the dialog opens unacknowledged — the first keystroke only
///   says "I see it" — shows its TTL, closes when it expires, and if
///   `policy.decide` does not reach the daemon it is SAID.
/// - **The webview's surface stays the same as ever**: four commands, a CSP
///   with no `eval` and no remote origins, minimal capabilities, no
///   filesystem and no shell, and `tests/webview_boundary.rs` pins it.
pub const EFECTOS: norte_ui_host::commands::Efectos = norte_ui_host::commands::Efectos::Completo;

/// The command-line help. Short on purpose: what this window knows how to do
/// is documented INSIDE (F1), not in a `--help`.
// TODO(translation): this banner is user-visible text that does not go
// through Fluent (reported to the controller for phase 3); left in Spanish.
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

/// The command that starts the daemon, or `None` if there is no binary to
/// launch.
///
/// **`norte-gui` does not know how to be a daemon**, unlike the CLI: that one
/// uses its own `current_exe` because the same executable carries the
/// subcommand. Here `norte` has to be found, and the order matters:
///
/// 1. **The sibling**: `norte` in the same directory as this executable. It
///    is the deterministic one — the pair that was installed together — and
///    it works with `just link`, where both symlinks point at the same
///    `target/debug` (`current_exe` already resolves the link, so the
///    sibling is the tree's and not `~/.local/bin`'s).
/// 2. **The `PATH`**, as a last resort.
///
/// Never the working directory: there the binary is chosen by whoever left a
/// file, and this launches a process. The sibling adds no risk — whoever can
/// write to this executable's directory already controls the window that is
/// running.
///
/// `None` leaves startup as it was: it tries to connect and, if nobody
/// answers, it says so.
async fn comando_de_daemon(socket: &std::path::Path) -> Option<Vec<std::ffi::OsString>> {
    let socket = socket.to_path_buf();
    // FS probes outside the runtime (rule 2).
    tokio::task::spawn_blocking(move || {
        let next_to = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf));
        // The argv is the shared one: what starts this window shuts down
        // only once its last client leaves.
        Some(norte_client::daemon_run_argv(
            programa_del_daemon(next_to.as_deref()),
            &socket,
        ))
    })
    .await
    .ok()
    .flatten()
}

/// Which `norte` is going to be launched: the executable's SIBLING if there
/// is one, and if not, the `PATH`'s.
///
/// Separate from [`comando_de_daemon`] so it can be tested. It is the promise
/// that backs the package — `norte-gui`, `norte` and `ntc` travel together and
/// the window finds its own (#256) — and until now it could only be checked
/// by installing, which is when it is already too late. What cannot be tested
/// here is `current_exe`, and that is why the directory comes in as an
/// argument.
fn programa_del_daemon(next_to: Option<&std::path::Path>) -> std::ffi::OsString {
    next_to
        .map(|d| d.join("norte"))
        .filter(|p| p.is_file())
        .map_or_else(|| "norte".into(), Into::into)
}

#[cfg(test)]
mod prueba_del_daemon {
    use super::programa_del_daemon;

    /// With a `norte` next to it, THAT one is launched, and with its full
    /// path.
    ///
    /// It is what makes the package work: on a clean install the `PATH` may
    /// have nothing, and the sibling is there.
    #[test]
    fn el_hermano_gana() {
        let dir = tempfile::tempdir().expect("temp");
        let sibling = dir.path().join("norte");
        std::fs::write(&sibling, b"#!/bin/sh\n").expect("is written");
        assert_eq!(programa_del_daemon(Some(dir.path())), sibling.as_os_str());
    }

    /// Without a sibling it falls back to the `PATH`, which is the
    /// development tree's case.
    #[test]
    fn sin_hermano_se_cae_al_path() {
        let dir = tempfile::tempdir().expect("temp");
        assert_eq!(programa_del_daemon(Some(dir.path())), "norte");
        assert_eq!(programa_del_daemon(None), "norte");
    }

    /// A DIRECTORY named `norte` is not a daemon: it is ignored.
    ///
    /// Without the `is_file` check, a directory would be launched as if it
    /// were a program, and the failure would come out as "could not
    /// connect", which says nothing.
    #[test]
    fn un_directorio_no_es_un_daemon() {
        let dir = tempfile::tempdir().expect("temp");
        std::fs::create_dir(dir.path().join("norte")).expect("is created");
        assert_eq!(programa_del_daemon(Some(dir.path())), "norte");
    }
}

/// What can keep it from starting.
#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    /// A flag that does not exist.
    #[error("unknown flag: {0}")]
    UnknownFlag(String),
    /// The startup directory does not work.
    #[error("startup directory: {0}")]
    Dir(String),
    /// The configuration does not load.
    #[error("configuration: {0}")]
    Config(String),
    /// There is no daemon on the other side.
    #[error("could not connect to the daemon at {socket}: {source}")]
    Connect {
        /// Where it looked.
        socket: String,
        /// What the daemon (or the socket) said.
        #[source]
        source: norte_proto::Error,
    },
    /// The daemon started and DIED, with whatever it said.
    ///
    /// Separate from [`StartupError::Connect`] because the advice is the
    /// opposite: that one invites checking whether there is a daemon, and
    /// this one invites reading a sentence that already explains the
    /// problem. Retrying does not fix it.
    #[error("the daemon could not start{}:\n{}",
        match .status { Some(c) => format!(" (exited with {c})"), None => String::new() },
        if .stderr.is_empty() { "and did not say why" } else { .stderr })]
    DaemonMuerto {
        /// Exit code, if there was one (`None` = a signal killed it).
        status: Option<i32>,
        /// What it wrote to `stderr`. May come empty.
        stderr: String,
    },
    /// A command-line value that does not exist.
    #[error("{que}: \"{valor}\" does not exist")]
    Desconocido {
        /// Which option.
        que: &'static str,
        /// What was asked for.
        valor: String,
    },
    /// The host did not start.
    #[error("the host did not start: {0}")]
    Host(#[from] norte_ui_host::controller::UiError),
}

/// The arguments, already resolved.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is an INDEPENDENT command-line flag; a two-variant enum \
              per flag would be the same data with more noise"
)]
pub struct Cli {
    /// Startup directory, raw (rule 1: it has no reason to be UTF-8).
    pub dir: Option<PathBuf>,
    /// The daemon's socket.
    pub socket: Option<PathBuf>,
    /// Layout requested for THIS run, with the BYTES intact.
    ///
    /// A layout name ends up being a file name (`layouts/<name>.toml`), and
    /// two different invalid byte sequences collapse to the SAME `\u{FFFD}`
    /// with a lossy conversion: they would open the same file (#246, ADR
    /// 0061). The TUI already reads it this way.
    pub layout: Option<std::ffi::OsString>,
    /// Keyboard preset requested for THIS run.
    pub preset: Option<std::ffi::OsString>,
    /// Profile requested for THIS run (#307, ADR 0079).
    ///
    /// Bytes intact for the same reason as [`Self::layout`], and with more
    /// reason: a profile name ends up being a DIRECTORY
    /// (`profiles/<name>/`).
    pub profile: Option<std::ffi::OsString>,
    /// No startup screen on THIS run, whatever `[ui] splash` says. For pilots
    /// and screenshots, same as in the terminal.
    pub no_splash: bool,
    /// This window is the other end of a HANDOFF (`--attach`, phase 9): in
    /// addition to the screen, it claims the MARKS the terminal left in the
    /// session.
    ///
    /// Without it a startup is a startup, and marks from a handoff that was
    /// left halfway do not come back to life the next day.
    pub attach: bool,
    /// Help was requested.
    pub help: bool,
    /// The version was requested.
    pub version: bool,
}

/// Parses with the SAME parser as the TUI.
///
/// # Errors
/// [`StartupError::UnknownFlag`] if a flag that does not exist shows up: a
/// misspelled flag that gets ignored is an option the user believes they set.
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

/// Everything the process needs in order to paint.
pub struct Boot {
    /// The host, already with its first listing requested.
    pub host: UiHost,
    /// The first frame: sequence 0.
    pub snapshot: ViewSnapshot,
    /// The negotiated language.
    pub lang: Lang,
    /// The resolved theme.
    pub theme: Theme,
    /// Fonts and motion, from `[ui]`: what this window paints that is not
    /// color. It goes here and is not re-read in `main` because the
    /// configuration is already loaded and looking at it again would be a
    /// second read that can differ.
    pub appearance: crate::catalog::Appearance,
    /// There is no user `norte.toml` (spec 2026-09-10): the catalogue carries
    /// this and the renderer opens the first-run wizard.
    pub first_run: bool,
    /// This window starts WITHOUT a splash screen, whatever `[ui] splash`
    /// says (ADR 0115): `--no-splash` or `NORTE_NO_SPLASH`.
    ///
    /// Decided here and carried in the catalogue, like `first_run`: the host
    /// does not look at the process's environment nor the command line —
    /// they are not its — and the renderer only needs to know whether to
    /// announce the startup or stay quiet.
    pub no_splash: bool,
    /// `[ui] theme_light` / `theme_dark` already resolved to variables (spec
    /// 2026-09-11, V6), or `None` when the key is absent or its theme does
    /// not load — then the window paints `theme` on that side, and warns.
    pub theme_light: Option<BTreeMap<String, String>>,
    /// The dark variant; see [`Self::theme_light`].
    pub theme_dark: Option<BTreeMap<String, String>>,
}

/// The layouts the user has saved, ALREADY read.
///
/// Read here and not by name because the picker paints each one's SHAPE:
/// reading them when the cursor moves would be I/O on the event loop. One
/// that does not parse is kept WITH its reason — the picker shows it without
/// a preview and explains why, which is more useful than a row that is not
/// there.
///
/// Goes through `spawn_blocking`, like its two neighbors. "It is startup and
/// it is a small directory" is not the criterion: a `read_dir` over a
/// configuration layer on a downed mount blocks the runtime's worker thread
/// just as well, and here there is not even a window to say so in (rule 2).
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

/// The theme, so it can be seen from the inside from the window.
///
/// The roles come from the SAME explicit mapping that feeds the CSS
/// variables (`catalog::variables`), not from a separate dump: what the view
/// shows is literally what gets painted. The effects are named one by one as
/// NOT supported, because this renderer is a webview and interprets none of
/// them — and a retro theme that looks identical reads as broken.
fn tema_visto(
    spec: Option<&str>,
    theme: &Theme,
    variante_clara: Option<Theme>,
    variante_oscura: Option<Theme>,
) -> HostTheme {
    HostTheme {
        // In a `Box`: `HostTheme` travels inside the startup future, and two
        // inline `Theme`s crossed `large_futures`'s threshold.
        variante_clara: variante_clara.map(Box::new),
        variante_oscura: variante_oscura.map(Box::new),
        // The RESOLVED one, which is `theme`'s. `spec` is what was
        // requested, and with a broken file the two do not match.
        name: spec.unwrap_or("default").to_owned(),
        roles: crate::catalog::variables(theme).into_iter().collect(),
        effects: efectos_declarados(theme),
        // The WHOLE theme, which is what is needed to color an entry by its
        // extension (bridge 66): that cannot be projected as CSS variables
        // because extensions are an open set.
        resuelto: theme.clone(),
    }
}

/// The names of the effects the theme declares.
///
/// The `[effects]` block is free-form on purpose (ADR 0036): each renderer
/// interprets it. Here only its top-level keys are enumerated, which is what
/// is needed to say which ones are not painted.
fn efectos_declarados(theme: &Theme) -> Vec<String> {
    // `Theme::effects` is a `toml::Value` and this crate does not depend on
    // `toml` (nor does it need to: it does not parse configuration). It asks
    // for the shape through the type it already has in hand.
    theme.effect_names().unwrap_or_default()
}

/// The window's language, fixed for the whole process.
///
/// **`NORTE_LANG` > `[ui] lang` > the system's environment**, which is what
/// the terminal does (`norte-tui/src/main.rs`). The two surfaces used to
/// document OPPOSITE rules and both honored them: here the configuration
/// won, there `NORTE_LANG` won, so with `NORTE_LANG=en` and `lang = "es"`
/// both set, `ntc` came out in English and `norte-gui` in Spanish.
///
/// The terminal rules because its rule is the one the rest already follows:
/// `NORTE_LANG` is norte-specific and is set for ONE run, i.e. the same kind
/// of thing as `--layout`, which beats `[ui] layout`. `LANG` does not: that
/// is the system's language, and a decision written in the configuration is
/// more specific than it.
fn idioma(pedido: Option<&str>) -> Lang {
    let explicito = std::env::var("NORTE_LANG").ok().filter(|v| !v.is_empty());
    let lang = elegir_idioma(explicito.as_deref(), pedido, Lang::from_env());
    let _ = norte_i18n::force(lang);
    // Keys are named in the window's language (see the TUI).
    let _ = norte_frontend::keymap::set_chord_lang(lang);
    lang
}

/// The precedence rule, without touching the environment.
///
/// Separate so it can be tested: `std::env::set_var` is `unsafe` since the
/// 2024 edition and rule 5 forbids it, so what is read from the environment
/// comes in as an argument. Same fix as [`programa_del_daemon`].
fn elegir_idioma(explicito: Option<&str>, config: Option<&str>, del_entorno: Lang) -> Lang {
    match (explicito, config) {
        (Some(e), _) => Lang::negotiate(Some(e)),
        (None, Some(c)) => Lang::negotiate(Some(c)),
        (None, None) => del_entorno,
    }
}

/// The two startup disk reads that are not the configuration.
///
/// Together and outside the runtime (rule 2): `rutas` does one `metadata`
/// per location and `disposiciones_del_usuario` a `read_dir` plus a
/// `read_to_string` per layout. Both over the SAME layers as `config::load`,
/// which already went through `spawn_blocking` for this same reason, and
/// both can touch a downed mount.
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
        // A panic here is a bug of OURS, not a missing directory.
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// Where each thing lives, for the settings' diagnostic view.
///
/// Built with the layers startup JUST read and the socket it just connected
/// to: asking again could answer something else (a `NORTE_CONFIG_DIR` that
/// changes, a `--socket` that gets ignored) and the window would say its
/// configuration comes from a place other than where it really came from.
fn rutas(capas: &norte_config::Layers, socket: &std::path::Path) -> HostPaths {
    // With its `missing` ALREADY resolved: the host projects this list
    // inside the single writer's loop, and an `exists()` there is
    // `std::fs::metadata` over — among others — a configuration layer that
    // may be on a downed mount. This function runs in `spawn_blocking`
    // (rule 2).
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
        // The SAME place `logging()` writes to, which is the only thing that
        // makes showing it useful.
        logs_dir: norte_config::dirs::state_dir()
            .map(|d| d.join("logs"))
            .map(sitio),
        socket: Some(sitio(socket.to_path_buf())),
    }
}

/// What the first frame has to SAY, if anything.
///
/// Goes in the initial frame's message, which is the exact equivalent of the
/// terminal startup's `app.message`: the reader's first action overwrites
/// it, not before.
///
/// Lua's goes LAST and that is why it wins: a `lua:` a repository puts in its
/// project layer is discarded — a repository does not choose what code a key
/// runs — and that is the warning that cannot be left overwritten. The
/// ignored project layer's (#260) is the other one: skipping it silently
/// leaves the reader with a configuration they believe is active and is not.
fn aviso_de_arranque(
    cfg: &norte_frontend::config::FrontendConfig,
    lang: Lang,
    lua_descartadas: usize,
    perfil: Option<&std::ffi::OsStr>,
) -> Option<String> {
    let mut msg = None;
    if !cfg.common.project_warnings.is_empty() {
        for warning in &cfg.common.project_warnings {
            tracing::warn!(reason = %warning, "project layer ignored");
        }
        msg = Some(norte_i18n::ta_in(
            lang,
            "msg-project-config-skipped",
            &[("n", &cfg.common.project_warnings.len().to_string())],
        ));
    }
    // The PROFILE lines that are not understood, with the same split: the
    // count to the bar and the reason to the log. Without this, being strict
    // with `[profile.start]` was a trap — `/tmp` gets written, the line is
    // dropped and the slot opens wherever it likes without anything saying
    // so (ADR 0098, D5).
    if !cfg.common.profile_warnings.is_empty() {
        for warning in &cfg.common.profile_warnings {
            tracing::warn!(reason = %warning, "profile line ignored");
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

/// The three EFFECTIVE keymaps: factory preset plus the user's layers.
///
/// READ-ONLY until phase 5. The preset binds F7/F8 to create and delete, and
/// the key existing is not permission: commands that write do not enter the
/// effective keymap — the key answers "not here" instead of staying mute —
/// and the host rejects them even if they arrived some other way.
///
/// WITH the user's layers, same as the terminal: without them a
/// `keymap.toml` with rebinds was silently ignored here while the other
/// frontend did honor it (#253). `UiHostOptions::keymap` documents that what
/// it receives already comes merged, and merging it is the job of whoever
/// reads disk.
///
/// The viewer and the dialogs are other SCREENS with the same preset: `esc`
/// closes and `e` reloads with another encoding because the preset says so,
/// not because the renderer decides it.
///
/// A preset that does not exist is SAID: the same file loudly rejects a
/// misspelled flag, and swallowing a misspelled VALUE to start with something
/// else would be the opposite inconsistency.
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

/// Change this window the same as the TUI (#287).
// TODO(translation): review — fragment, looks like it is missing its subject.
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

/// The configuration layers with the profile the reader NAMED already tucked
/// inside (#307, ADR 0079 D7).
///
/// And if that profile cannot be used, this FAILS: you asked for that
/// profile, and starting as something else would be answering a different
/// question. The name has to be in the LISTING, byte for byte — looking only
/// at whether the resolver produced a layer is not enough, because it adds
/// one as soon as the name is legal and there is a user directory, whether it
/// exists or not; the load then treats it as an absent layer, which is not an
/// error, and `--profile ghost` used to start as if nothing happened. It is
/// the same check, word for word, that the terminal makes.
fn capas_con_perfil(nombre: &std::ffi::OsStr) -> Result<norte_config::Layers, StartupError> {
    let dir = norte_config::profiles_dir_from(&|k| std::env::var_os(k)).ok_or_else(|| {
        StartupError::Config("no configuration directory to hang a profile off of".to_owned())
    })?;
    capas_con_perfil_en(&dir, nombre)
}

/// The probable core of [`capas_con_perfil`]: the profiles directory comes in
/// as an ARGUMENT, so its test does not depend on the `HOME` of whoever runs
/// it.
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

/// Mounts the host: configuration, socket, directory, keymap and layout.
///
/// **It is a SEQUENCE, and that is why it grows one step per thing that has
/// to be mounted.** Each line is a name and a call, in the only order they
/// can be done in: the log needs the configuration, the keymap needs the
/// preset, the host needs all of them. Splitting it to get under the lint's
/// threshold puts the order in two places and leaves the reader
/// reconstructing it — and the order is the only delicate thing here. Same
/// treatment as `aplicar_efecto` in the host.
///
/// # Errors
/// [`StartupError`] if the configuration does not load, the directory is not
/// valid, or there is no daemon on the other side.
#[expect(
    clippy::too_many_lines,
    reason = "startup sequence: one step per line, and the order is the contract"
)]
pub async fn boot(cli: &Cli) -> Result<Boot, StartupError> {
    // The SAME layers as the TUI, read outside the runtime (rule 2) — and
    // with the profile `--profile` names ALREADY tucked into them (#307, ADR
    // 0079).
    //
    // On the FIRST load and not through hot reload, same as the terminal:
    // this way it applies up to `[ui] lang`, which is the only thing a
    // running change cannot undo (`norte_i18n::force` runs once). The STICKY
    // profile cannot do this — it lives in the session, and the session
    // belongs to the daemon, which is reached with the configuration we are
    // loading right now — and that is why it arrives by the other path.
    let capas = match &cli.profile {
        Some(nombre) => capas_con_perfil(nombre)?,
        None => norte_config::standard_layers(),
    };
    // Saved for the "where each thing lives" view: the host does not
    // discover files, so the layer list is handed to it already resolved and
    // is exactly the one that was just READ, not one recomputed later.
    let capas_vistas = capas.clone();
    let cfg = match tokio::task::spawn_blocking(move || norte_frontend::config::load(&capas)).await
    {
        Ok(res) => res.map_err(|e| StartupError::Config(e.to_string()))?,
        // A panic inside `load` is a bug of OURS: it is not buried as a
        // configuration error with a made-up path (rule 6).
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    };

    let lang = idioma(cfg.common.ui_lang.as_deref());

    let (theme, tema_resuelto) = tema(cfg.common.ui_theme.as_deref());
    // The desktop-scheme variants (V6): each one resolves like `theme` and
    // already travels as variables. A key that is set whose theme does not
    // load ends up WITHOUT a variant — not with the factory one — so the
    // window paints `theme` on that side and the failure does not disguise
    // itself as a theme.
    //
    // The WHOLE `Theme` is saved in addition to its variables: the renderer
    // plugs in the variables, but an entry's color by `[files.ext]` (bridge
    // 66) is resolved by the HOST, and it does not fit in variables because
    // extensions are an open set. Always resolving against `[ui] theme`, a
    // desktop in light mode painted the chrome with the light variant and
    // the NAMES with the dark one's colors.
    let variante = |nombre: Option<&str>| -> Option<Theme> {
        let n = nombre?;
        match norte_frontend::theme::resolve_theme(Some(n)) {
            Ok(t) => Some(t),
            Err(e) => {
                tracing::warn!(error = %e, theme = n, "theme variant did not load: ignored");
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
    // Outside the runtime (rule 2): `metadata` over a downed NFS mount blocks
    // the worker thread until the mount times out, and before there is even
    // a window to say so in. The configuration read above already went
    // through `spawn_blocking`; this one was left at eight lines.
    let dir_pedido = cli.dir.clone();
    let inicio = match tokio::task::spawn_blocking(move || start_dir(dir_pedido)).await {
        Ok(res) => res?,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    };

    let socket = cli
        .socket
        .clone()
        .or_else(|| cfg.common.daemon.socket.clone())
        .unwrap_or_else(|| norte_client::default_socket_path(None));

    // Daemon and ONLY daemon: the reference GUI does not build an `Engine` in
    // its own process (decision D10). What it does do is START ONE if there
    // is none, same as the CLI's `--daemon`: requiring the reader to open a
    // terminal before they can open a window is not an architecture
    // decision, it is a chore left for the reader.
    //
    // And it connects with `connect_detallado` on purpose: when the daemon
    // starts and DIES — a journal that cannot migrate is the real case — the
    // only sentence that says what to do is the one it writes to `stderr`,
    // and the wire's taxonomy has nowhere to put it. Without this, the
    // window said "could not connect (retryable: true)", i.e. "wait", about
    // something that was never going to arrive.
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

    // The log, AFTER loading the config because `[log] dir` and `[log]
    // retain` come from it — the window used to ignore them — and before
    // anything else, so that whatever fails from here on leaves a trace.
    // Same rule as the terminal: a `--help` exits before this and writes
    // nothing, which is correct.
    // #326: the log ALSO goes to an in-memory ring, which is what the log
    // panel paints. The file is for investigating afterwards; the ring is
    // for seeing what is happening without leaving the window.
    //
    // Watch what this ring does NOT carry: the window starts its own daemon
    // (#300), so only THIS process's lines are here — the providers, the
    // journal and the policy log to their own. The panel says so; keeping
    // quiet about it would make it look broken.
    let log_ring = logging(&cfg);

    let preset = if let Some(p) = &cli.preset {
        nombre_de(p, "--preset")?
    } else {
        cfg.common.preset.clone()
    };
    let (keymap, keymap_viewer, keymap_dialog) = keymaps(&preset, &cfg)?;
    // A `lua:` from the PROJECT layer is discarded — a repository does not
    // choose what code a key runs — and it is SAID, like in the terminal: a
    // silent discard is a key that does not do what its file says.
    let capas_lua_descartadas = keymap
        .discarded_lua_bindings()
        .max(keymap_viewer.discarded_lua_bindings())
        .max(keymap_dialog.discarded_lua_bindings());

    // From the command line it is required to exist; from the CONFIGURATION
    // it falls back to the usual one, which is what the user had before
    // writing the key (same rule as the TUI). The difference is who just
    // typed it.
    //
    // The file is read OUTSIDE the runtime (rule 2), as in the TUI: it is a
    // small TOML, but reading it with `std::fs` inside an `async fn` is
    // blocking I/O all the same.
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
    // A broken file does NOT leave the screen empty — the preset stays — but
    // it does not stay quiet either: a layout that fails to parse and
    // disappears silently is a configuration the reader believes is set. It
    // goes to the LOG and the status bar, as in the TUI: the log alone is
    // read by nobody who is looking at a layout they did not ask for.
    let aviso_layout = layout_roto.map(|e| {
        tracing::warn!(error = %e, "user layout did not load: the factory one stays");
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
    // A column id that fails to parse does not disappear silently: `doctor`
    // reports it, and here it at least stays in the startup log.
    for malo in &columnas.invalid {
        tracing::warn!(column = %malo, "invalid column id: ignored");
    }
    let (paths, user_layouts) = diagnostico(&capas_vistas, &socket).await;
    let (host, snapshot) = UiHost::start(UiHostOptions {
        backend: Arc::new(backend),
        initial_dir: inicio,
        // A human typed it, so it beats the session in the active pane — the
        // same rule the terminal settled in `eb237c61`.
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
        // The renderer corrects the size as soon as it knows its own; this
        // is what gets used meanwhile.
        viewport: (120, 40),
        // The WHOLE settings: columns are configured per scheme, and
        // resolving them here for `file` left that half of the
        // configuration dead as soon as a pane navigated to an `sftp://`.
        columns: columnas,
        effects: EFECTOS,
        settings: cfg.clone(),
        paths,
        theme: tema_visto(tema_resuelto.as_deref(), &theme, tema_claro, tema_oscuro),
        user_layouts,
        // Already APPLIED in `settings` (its layers went in above); this is
        // so the host knows and the picker marks it as set (#307).
        profile: cli.profile.clone(),
        log_ring,
    })
    .await?;
    let mut snapshot = snapshot;
    // The layout's goes FIRST if there is one: the other two warn about an
    // ignored layer, and this one warns that the screen being looked at is
    // not the one requested — which is what the reader cannot work out on
    // their own.
    snapshot.status.message = aviso_layout
        .or_else(|| aviso_de_arranque(&cfg, lang, capas_lua_descartadas, cli.profile.as_deref()));
    // The first-run wizard (spec 2026-09-10): without a user `norte.toml` and
    // without `NORTE_NO_WIZARD`, as in the terminal. A `stat`, outside the UI
    // thread (rule 2).
    let first_run = if std::env::var_os("NORTE_NO_WIZARD").is_some() {
        false
    } else if let Some(dir) = norte_config::user_config_dir() {
        !tokio::fs::try_exists(dir.join("norte.toml"))
            .await
            .unwrap_or(true)
    } else {
        false
    };
    // No splash screen on THIS run (ADR 0115): the flag, or the variable
    // pilots and screenshots use. Same pair as the terminal, because a
    // window that ignores `NORTE_NO_SPLASH` leaves a screen on top of every
    // automated screenshot.
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

/// A layout's name, SHOWABLE: lossy marked and hazards masked.
///
/// For messages only. The bytes are not touched — the loader uses them —
/// and this is the same thing the TUI does in `App::apply_loaded_layout`:
/// without the mask, a `--layout $'a\x1b[31mb'` leaves the RAW sequence in
/// `norte-gui.log`, and without the marker `$'\xff'` and `$'\xfe'` give the
/// same message.
fn layout_pintable(name: &std::ffi::OsStr) -> String {
    let (showable, lossy) = norte_frontend::display_os_name(name);
    let showable = norte_encoding::mask_terminal_hazards(&showable);
    if lossy {
        format!("! {showable}")
    } else {
        showable
    }
}

/// The layout the window starts with, and the warning if the user's file was
/// broken.
///
/// Two sources and two criteria: `--layout` is required to exist, because a
/// human just typed it; `[ui] layout` falls back to `orthodox`, which is what
/// was there before the key was written.
///
/// **The window is stricter than the TUI on the first one**, on purpose: the
/// TUI warns through the bar and continues, because it already has a screen
/// up when that happens; here there is not yet anything to show, and
/// starting with a layout that is not the one requested is worse than saying
/// it does not exist. What IS shared is the resolution RULE (`or_preset`) and
/// the reasoning: a BROKEN file is not announced as "does not exist", it is
/// announced with its parse error.
///
/// Within each source, the user's file beats the factory preset —
/// [`norte_frontend::layout::config::or_preset`] is that rule, shared. This
/// used to look ONLY at the presets, so a saved layout could not be
/// requested from the command line even though this same window's picker
/// offered it.
///
/// The name travels as [`std::ffi::OsStr`] and not as `String` (#246): it is
/// a FILE name, and collapsing its bytes sends two different invalid names to
/// `layouts/\u{fffd}.toml`.
///
/// Reads from disk: goes under `spawn_blocking`.
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
            // There is no file and no preset with that name: it is a value
            // that does not exist, and that is what gets said.
            LayoutError::NotFound(_) | LayoutError::BadName(_) => StartupError::Desconocido {
                que: "--layout",
                valor: layout_pintable(name),
            },
            // The file IS there and does not work. Announcing it as "does
            // not exist" sends the reader looking for a name they already
            // typed correctly: what they need is the parse error.
            otro => StartupError::Config(format!("--layout {}: {otro}", layout_pintable(name))),
        });
    }
    let name = std::ffi::OsString::from(config.unwrap_or("orthodox"));
    match config::or_preset(&name, leer(&name)) {
        Ok(v) => Ok(v),
        // The key names something that does not exist: it falls back to the
        // usual one, which is what the reader had before writing it.
        Err(e) => Ok((
            norte_frontend::layout::presets::tree("orthodox")
                .map_err(|x| StartupError::Config(x.to_string()))?,
            Some(e),
        )),
    }
}

/// A command-line value that HAS to be text so it can be compared against a
/// list of known names.
///
/// The bytes are kept intact up to here (`OsString`) and the conversion
/// fails instead of collapsing: two different invalid names cannot end up
/// being the same one (#246).
fn nombre_de(v: &std::ffi::OsStr, que: &'static str) -> Result<String, StartupError> {
    v.to_str()
        .map(str::to_owned)
        .ok_or_else(|| StartupError::Desconocido {
            que,
            valor: v.to_string_lossy().into_owned(),
        })
}

/// The requested theme, and the name of the one that is going to be painted.
///
/// Through `resolve_theme`, the SHARED resolver: it accepts a preset's name
/// **or the path to a `.toml`** (ADR 0020). This used to call plain
/// `Theme::preset`, so a `theme = "~/.config/norte/mine.toml"` themed the
/// terminal and left the window with the default palette, without saying
/// anything — the same shape the `--layout` bug had.
///
/// Reads from disk when the spec is a path: goes under `spawn_blocking`.
///
/// Returns the name of the one that IS GOING TO BE PAINTED and not the one
/// that was requested: the theme view exists to see the one that is actually
/// there from the inside, and titling it with a name whose colors are not
/// the ones underneath is exactly what that view exists to prevent. A file
/// theme has no preset name, so it goes with its own if it declares one.
fn tema(nombre: Option<&str>) -> (Theme, Option<String>) {
    let Some(n) = nombre else {
        return (Theme::preset_default(), None);
    };
    match norte_frontend::theme::resolve_theme(Some(n)) {
        Ok(t) => {
            // A preset is titled with the requested name; a file one, with
            // whatever the file itself declares.
            let titulo = Theme::preset(n)
                .ok()
                .flatten()
                .map_or_else(|| t.name.clone(), |_| Some(n.to_owned()));
            (t, titulo)
        }
        Err(e) => {
            tracing::warn!(error = %e, "requested theme did not load: the factory one stays");
            (Theme::preset_default(), None)
        }
    }
}

/// The startup directory, as a `VPath`.
///
/// Validated HERE and not inside the window: a startup error with the screen
/// already mounted is a gray box that says nothing.
fn start_dir(dir: Option<PathBuf>) -> Result<VPath, StartupError> {
    let nativo = match dir {
        Some(d) => {
            let meta = std::fs::metadata(&d)
                .map_err(|e| StartupError::Dir(format!("{}: {e}", d.display())))?;
            if !meta.is_dir() {
                return Err(StartupError::Dir(format!(
                    "{} is not a directory",
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

/// The log goes to the FILE and only to the file.
///
/// The SETUP is the shared one (`norte_config::logging`): daily rotation,
/// 0700 directory and 0600 files, bounded retention and `suppaftp`'s safety
/// cap. This used to be set up by hand — because the helper lived in the
/// core and this binary talks to the daemon over a socket — and what it cost
/// was that the copy was left without the hardening: a log readable by any
/// local account, with the paths the user had navigated (#255). The helper
/// now lives in `norte-config`, which already owned `state_dir()` and
/// `[log]`.
///
/// The prefix IS its own: the daemon and this window can both be alive at
/// the same time, and sharing a rotation file would make one's retention
/// prune the other's files.
fn logging(cfg: &norte_frontend::config::FrontendConfig) -> Option<norte_config::logring::LogRing> {
    norte_config::logging::init_to_file_with_ring(
        norte_config::logging::LogConfig {
            dir: cfg.common.log.dir.as_deref(),
            retain: cfg.common.log.retain,
            prefix: Some("norte-gui.log"),
            format: cfg.common.log.format,
        },
        norte_config::logring::RING_DEFAULT,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `NORTE_LANG` > `[ui] lang` > the system's environment.
    ///
    /// The two surfaces used to document OPPOSITE rules and both honored
    /// them: the window sided with the configuration and the terminal with
    /// `NORTE_LANG`, so with both set `ntc` came out in one language and
    /// `norte-gui` in another. The terminal rules: `NORTE_LANG` is
    /// norte-specific and is set for ONE run, i.e. the same kind of thing as
    /// `--layout`, which beats `[ui] layout`.
    #[test]
    fn norte_lang_gana_a_la_config_y_la_config_al_entorno() {
        assert_eq!(
            elegir_idioma(Some("en"), Some("es"), Lang::Es),
            Lang::En,
            "what was set for this run rules"
        );
        assert_eq!(
            elegir_idioma(None, Some("es"), Lang::En),
            Lang::Es,
            "and a decision written down rules over the system's language"
        );
        assert_eq!(
            elegir_idioma(None, None, Lang::En),
            Lang::En,
            "with nothing, the system"
        );
    }

    /// `[ui] theme` accepts the PATH to a `.toml`, not just a preset (ADR
    /// 0020).
    ///
    /// The window used to call plain `Theme::preset`, so a custom theme
    /// themed the terminal and left the window with the default palette,
    /// without saying anything.
    #[test]
    fn el_tema_puede_ser_un_fichero() {
        let dir = tempfile::tempdir().expect("tmp");
        let ruta = dir.path().join("mio.toml");
        std::fs::write(&ruta, "name = \"mío\"\n").expect("write");
        let (t, titulo) = tema(Some(&ruta.to_string_lossy()));
        assert_eq!(
            t.name.as_deref(),
            Some("mío"),
            "the file's theme was loaded"
        );
        assert_eq!(
            titulo.as_deref(),
            Some("mío"),
            "and it is titled with the name the file declares"
        );
    }

    /// A preset is still a preset, and is titled with the requested name.
    #[test]
    fn un_preset_sigue_yendo_por_su_nombre() {
        let (_t, titulo) = tema(Some("nord"));
        assert_eq!(titulo.as_deref(), Some("nord"));
    }

    /// And a theme that fails to load leaves the factory one, with no name:
    /// the theme view cannot be titled with colors that are not the ones
    /// underneath.
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

    /// `--layout mio` opens the USER's file, same as in the TUI.
    ///
    /// The window used to look only at the factory presets, so a saved
    /// layout could not be requested from the command line — and this same
    /// window offers it in its picker, i.e. the list and the option said
    /// different things about the same file.
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
                .expect("loads the user's");
        assert_eq!(arbol.slot_ids().len(), 1, "the file's, with a single slot");
        assert!(aviso.is_none());
    }

    /// And a user one named like a preset BEATS the preset, which is the
    /// rule for the rest of the configuration.
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
                .expect("loads");
        assert_eq!(
            arbol.slot_ids().len(),
            1,
            "the factory `simple` has three slots: this is the user's"
        );
    }

    /// A name that is not UTF-8 is a file name like any other (#246): it is
    /// searched for, not rejected outright.
    #[test]
    fn un_nombre_de_layout_que_no_es_utf8_se_busca_igual() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().expect("tmp");
        let nombre = std::ffi::OsStr::from_bytes(b"\xff\xfe");
        let mut fichero = nombre.to_os_string();
        fichero.push(".toml");
        escribe_layout(dir.path(), &fichero, "[slot]\nid = 1\nkind = \"browser\"\n");
        let (arbol, _) =
            arbol_de_arranque(Some(nombre), None, Some(dir.path())).expect("loads by bytes");
        assert_eq!(arbol.slot_ids().len(), 1);
    }

    /// From the command line it is REQUIRED to exist; from the configuration
    /// it falls back to `orthodox`, which is what was there before the key
    /// was written.
    #[test]
    fn un_nombre_inventado_falla_en_la_orden_y_cae_en_la_config() {
        let dir = tempfile::tempdir().expect("tmp");
        assert!(
            arbol_de_arranque(Some(std::ffi::OsStr::new("nada")), None, Some(dir.path())).is_err(),
            "a human just typed it: they are told"
        );
        let (arbol, _) = arbol_de_arranque(None, Some("nada"), Some(dir.path()))
            .expect("the config does not leave the screen empty");
        assert_eq!(
            arbol,
            norte_frontend::layout::presets::tree("orthodox").expect("preset")
        );
    }

    #[test]
    fn los_flags_se_leen_como_en_el_tui() {
        let cli = parse(["--socket", "/tmp/x.sock", "--layout", "simple"]).expect("parses");
        assert_eq!(
            cli.socket.as_deref(),
            Some(std::path::Path::new("/tmp/x.sock"))
        );
        assert_eq!(cli.layout.as_deref(), Some(std::ffi::OsStr::new("simple")));
        assert!(!cli.help);
    }

    /// `--profile` also exists on the window (#307), and with the BYTES
    /// intact: a profile name ends up being a directory.
    #[test]
    fn el_perfil_se_lee_y_conserva_sus_bytes() {
        let cli = parse(["--profile", "trabajo"]).expect("parses");
        assert_eq!(
            cli.profile.as_deref(),
            Some(std::ffi::OsStr::new("trabajo"))
        );

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            let crudo = std::ffi::OsStr::from_bytes(b"perf\xffil");
            let cli = parse([std::ffi::OsString::from("--profile"), crudo.to_os_string()])
                .expect("parses");
            assert_eq!(
                cli.profile.as_deref(),
                Some(crudo),
                "without going through text: two different invalid byte \
                 sequences would open the same directory"
            );
        }
    }

    /// And a profile that is not in the listing ABORTS (ADR 0079, D7): you
    /// asked for that profile, and starting as something else would be
    /// answering a different question.
    #[test]
    fn un_perfil_que_no_existe_no_arranca() {
        let dir = tempfile::tempdir().expect("temp");
        // No `profiles/` inside, so the listing comes back empty.
        let e = capas_con_perfil_en(dir.path(), std::ffi::OsStr::new("fantasma"))
            .expect_err("not valid");
        assert!(
            matches!(&e, StartupError::Desconocido { que, valor } if *que == "--profile" && valor == "fantasma"),
            "{e}"
        );
    }

    /// A misspelled flag is NOT ignored: it is said. Ignoring it would be
    /// starting up without the option the user believes they set.
    #[test]
    fn un_flag_desconocido_no_se_traga() {
        let e = parse(["--socketo", "/tmp/x"]).expect_err("not valid");
        assert!(
            matches!(&e, StartupError::UnknownFlag(f) if f == "--socketo"),
            "{e}"
        );
    }

    /// The window ACCEPTS what the terminal hands it in a handoff (phase 9).
    ///
    /// The test that was missing, and the bug that asked for it: the
    /// terminal launched `ntc-gui --attach --daemon`, this parser knew
    /// neither of the two, and the window exited with code 2 — without
    /// saying anything, because the handoff closes its `stderr`. Built with
    /// the SAME function the terminal uses, so a new flag on one side
    /// without the other turns this red.
    #[test]
    fn la_ventana_acepta_el_argv_del_relevo() {
        let cli = parse(norte_frontend::handoff::window_args())
            .expect("the window has to accept what the handoff hands it");
        assert!(cli.attach, "and understand that it comes from a handoff");
        // Any regular startup is NOT a handoff.
        assert!(!parse(Vec::<String>::new()).expect("no flags").attach);
    }

    /// Two layout names with DIFFERENT bytes cannot end up being the same
    /// one: with a lossy conversion the two used to collapse to
    /// `caf\u{FFFD}` and open the same file (#246).
    #[cfg(unix)]
    #[test]
    fn dos_nombres_invalidos_distintos_siguen_siendo_distintos() {
        use std::os::unix::ffi::OsStringExt as _;
        let uno = std::ffi::OsString::from_vec(b"caf\xff".to_vec());
        let otro = std::ffi::OsString::from_vec(b"caf\xfe".to_vec());
        let a = parse([std::ffi::OsString::from("--layout"), uno]).expect("parses");
        let b = parse([std::ffi::OsString::from("--layout"), otro]).expect("parses");
        assert_ne!(a.layout, b.layout, "the bytes are kept");
    }

    #[test]
    fn la_ayuda_y_la_version_se_reconocen() {
        assert!(parse(["--help"]).expect("parses").help);
        assert!(parse(["-V"]).expect("parses").version);
    }

    /// A directory that does not exist is reported BEFORE opening a window.
    #[test]
    fn un_directorio_que_no_existe_se_dice_pronto() {
        let e = start_dir(Some(PathBuf::from("/no/existe/ni/de/lejos"))).expect_err("fails");
        assert!(matches!(e, StartupError::Dir(_)), "{e}");
    }
}
